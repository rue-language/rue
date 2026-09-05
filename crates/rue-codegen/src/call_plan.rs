//! Target-independent planning for Rue calls.
//!
//! This module decides the logical ABI shape of a call.  It deliberately does
//! not know about physical registers or target instructions; the two backend
//! lowerers consume the normalized slot vector and only choose how to marshal
//! those slots for their ABI.

use rue_air::{ArgClass, ArgConvention, FrozenTypeInternPool, NativeCallAbi, ReturnClass};
// `ScalarAbiExtension` is referenced by fully-qualified path in the struct so it
// stays visible to readers of the field; no direct import is needed.
use rue_cfg::{Cfg, CfgArgMode, CfgCallArg, Type};
use rue_runtime_abi::{ReservedExportClass, ReservedExportId};
use rue_target::CallingConvention;

use crate::types;
use crate::vreg::VReg;

use crate::frame_layout::checked_aligned_cell_region_bytes;

/// The convention a call target's callee follows.
///
/// A Rue-compiled function uses the native convention, whose aggregate
/// arguments are in reversed ABI slot order so the callee reconstructs the
/// ascending logical frame layout. A compiler-built C memory routine crosses
/// the platform C boundary, in natural C slot order; which C row that is comes
/// from the compilation target, so the caller supplies it.
const fn callee_convention(
    target: &CallTarget,
    c_convention: CallingConvention,
) -> CallingConvention {
    match target {
        CallTarget::Rue(_) => CallingConvention::Rue,
        CallTarget::MemoryBuiltin(_) => c_convention,
    }
}

/// Typed logical identity of a call target.
///
/// Runtime helpers use the separately validated `RuntimeCallPlan` path. Rue
/// functions (including generated drop glue) and compiler-built C memory
/// routines remain separate classes instead of being inferred from a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallTarget {
    Rue(String),
    MemoryBuiltin(MemoryBuiltinId),
}

/// Checked identity for one compiler-built C memory routine.
///
/// This wrapper prevents other reserved runtime exports (entry points, shims,
/// or ABI marker data) from being represented as callable memory builtins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBuiltinId(ReservedExportId);

impl MemoryBuiltinId {
    pub fn new(id: ReservedExportId) -> Self {
        assert_eq!(
            id.export().class,
            ReservedExportClass::CompilerBuiltMemory,
            "memory builtin call targets must name compiler-built memory routines"
        );
        Self(id)
    }

    pub const fn export_id(self) -> ReservedExportId {
        self.0
    }

    pub const fn symbol(self) -> &'static str {
        self.0.symbol()
    }
}

impl CallTarget {
    pub fn rue(symbol: impl Into<String>) -> Self {
        Self::Rue(symbol.into())
    }

    pub fn memory_builtin(id: ReservedExportId) -> Self {
        Self::MemoryBuiltin(MemoryBuiltinId::new(id))
    }

    /// Resolve the external symbol at the MIR symbol-table boundary.
    pub fn symbol(&self) -> &str {
        match self {
            Self::Rue(symbol) => symbol,
            Self::MemoryBuiltin(id) => id.symbol(),
        }
    }
}

/// The logical mode of one user argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserArgMode {
    /// A normal argument is passed by value.
    Value,
    /// An `inout` argument is represented by one preserved address slot.
    Inout,
    /// A `borrow` argument is represented by one preserved address slot.
    Borrow,
}

/// The only modes that can produce a by-reference call input. Keeping this
/// separate from `UserArgMode` makes a raw CFG value impossible to pair with
/// an `inout` or `borrow` input in `CallArgInput::Value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByRefMode {
    Inout,
    Borrow,
}

impl From<ByRefMode> for UserArgMode {
    fn from(mode: ByRefMode) -> Self {
        match mode {
            ByRefMode::Inout => Self::Inout,
            ByRefMode::Borrow => Self::Borrow,
        }
    }
}

/// One classified user argument.  Its vregs are already materialized by the
/// shared aggregate/by-ref leaves, but no physical ABI assignment has happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserArgPlan {
    pub mode: UserArgMode,
    pub slots: Vec<VReg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiRegisterBanks {
    pub gp: usize,
    pub fp: usize,
}

impl From<usize> for AbiRegisterBanks {
    fn from(gp: usize) -> Self {
        Self { gp, fp: 0 }
    }
}

impl From<u32> for AbiRegisterBanks {
    fn from(gp: u32) -> Self {
        Self {
            gp: gp as usize,
            fp: 0,
        }
    }
}

#[cfg(test)]
impl From<i32> for AbiRegisterBanks {
    fn from(gp: i32) -> Self {
        Self {
            gp: gp as usize,
            fp: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiSlotClass {
    Gp,
    Fp(crate::value_plan::FloatWidth),
}

impl AbiSlotClass {
    /// The class of an ABI slot carrying `leaf`, the one rule every direction
    /// of the native convention uses: a float leaf goes to the FP bank, at its
    /// own width; everything else to the GP bank.
    pub fn for_leaf(leaf: Type) -> Self {
        match crate::value_plan::float_width(leaf) {
            Some(width) => Self::Fp(width),
            None => Self::Gp,
        }
    }
}

impl From<rue_air::NativeArgClass> for AbiSlotClass {
    fn from(value: rue_air::NativeArgClass) -> Self {
        match value {
            rue_air::NativeArgClass::Gp => Self::Gp,
            rue_air::NativeArgClass::Fp32 => Self::Fp(crate::value_plan::FloatWidth::F32),
            rue_air::NativeArgClass::Fp64 => Self::Fp(crate::value_plan::FloatWidth::F64),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiSlotLocation {
    GpReg(usize),
    FpReg(usize),
    Stack(usize),
}

pub fn assign_abi_slots(
    classes: impl IntoIterator<Item = AbiSlotClass>,
    banks: AbiRegisterBanks,
) -> Vec<AbiSlotLocation> {
    let mut gp = 0usize;
    let mut fp = 0usize;
    let mut stack = 0usize;
    classes
        .into_iter()
        .map(|class| match class {
            AbiSlotClass::Gp if gp < banks.gp => {
                let index = gp;
                gp += 1;
                AbiSlotLocation::GpReg(index)
            }
            AbiSlotClass::Fp(_) if fp < banks.fp => {
                let index = fp;
                fp += 1;
                AbiSlotLocation::FpReg(index)
            }
            AbiSlotClass::Gp => {
                gp += 1;
                let index = stack;
                stack += 1;
                AbiSlotLocation::Stack(index)
            }
            AbiSlotClass::Fp(_) => {
                fp += 1;
                let index = stack;
                stack += 1;
                AbiSlotLocation::Stack(index)
            }
        })
        .collect()
}

/// Where one logical slot of a REGISTER-RETURNED aggregate travels.
///
/// A return slot's bank follows its LEAF type, exactly as an argument slot's
/// class does ([`AbiSlotClass`]): `struct P { field: f64 }` hands its one slot
/// to the first floating-point return register, not to a general-purpose one.
/// Keeping one rule for both directions is what stops the same type from
/// crossing in an XMM/V register as an argument and a GP register as a return,
/// which is how a general-purpose vreg came to hold an FP-classed slot.
///
/// Only a ONE-SLOT aggregate can reach this today: a wider aggregate holding a
/// float is not slot-identical under the compact layout, so
/// `NativeAbiTypeFacts::classify_return` sends it through sret instead. The
/// assignment below is written for the general case anyway, so the rule does
/// not have to be rediscovered when that changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnSlotReg {
    /// Return register `index` of the general-purpose bank.
    Gp(usize),
    /// Return register `index` of the floating-point bank, moved at `width`.
    Fp {
        index: usize,
        width: crate::value_plan::FloatWidth,
    },
}

/// The ABI slot classes of `ty`'s logical slots, in logical slot order: one
/// class per aggregate leaf, floating-point leaves in the FP bank.
pub fn aggregate_slot_classes(type_pool: &FrozenTypeInternPool, ty: Type) -> Vec<AbiSlotClass> {
    crate::types::aggregate_leaf_types(type_pool, ty)
        .into_iter()
        .map(AbiSlotClass::for_leaf)
        .collect()
}

/// Assign every logical slot of a register-returned aggregate of type `ty` to a
/// return register, counting each bank independently.
///
/// `banks` is the target's return-register file. A slot that does not fit a
/// bank is a classification error rather than a stack argument: the return
/// classifier only answers `Registers` when every slot fits.
pub fn return_slot_regs(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    banks: AbiRegisterBanks,
) -> Vec<ReturnSlotReg> {
    let classes = aggregate_slot_classes(type_pool, ty);
    assign_abi_slots(classes.iter().copied(), banks)
        .into_iter()
        .zip(classes)
        .map(|(location, class)| match (location, class) {
            (AbiSlotLocation::GpReg(index), _) => ReturnSlotReg::Gp(index),
            (AbiSlotLocation::FpReg(index), AbiSlotClass::Fp(width)) => {
                ReturnSlotReg::Fp { index, width }
            }
            _ => panic!("a register-returned aggregate slot must fit a return register bank"),
        })
        .collect()
}

/// The hidden caller-provided return storage, when the return uses sret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HiddenSretPlan {
    /// The vreg containing the address of the caller-owned storage.
    pub pointer: VReg,
    /// The logical ABI position of the hidden pointer.
    pub abi_slot: usize,
    /// Number of logical return slots written by the callee.
    pub slot_count: u32,
    /// Caller storage size, rounded up to the call-stack alignment.
    pub storage_bytes: u32,
}

/// How a call result is reconstructed after the target call instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnPlan {
    /// Unit/never/empty aggregates have no materialized slots.
    ZeroSized,
    /// A one-slot scalar is returned in the target's primary return register.
    Scalar,
    /// A complete aggregate is returned one logical slot per return register.
    Registers { slot_count: u32 },
    /// A complete aggregate is written to caller-provided storage.
    Sret { slot_count: u32, storage_bytes: u32 },
}

impl ReturnPlan {
    /// Number of logical return slots represented by this plan.
    pub const fn slot_count(self) -> u32 {
        match self {
            Self::ZeroSized => 0,
            Self::Scalar => 1,
            Self::Registers { slot_count } | Self::Sret { slot_count, .. } => slot_count,
        }
    }

    pub const fn uses_sret(self) -> bool {
        matches!(self, Self::Sret { .. })
    }
}

/// A normalized call ABI plan consumed by both target adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPlan {
    pub target: CallTarget,
    pub callee_convention: CallingConvention,
    pub hidden_sret: Option<HiddenSretPlan>,
    pub user_args: Vec<UserArgPlan>,
    /// Complete logical ABI slots, including the hidden sret pointer when
    /// present.  This is the only slot vector the adapters may marshal.
    pub abi_slots: Vec<VReg>,
    pub abi_classes: Vec<AbiSlotClass>,
    pub abi_locations: Vec<AbiSlotLocation>,
    pub return_plan: ReturnPlan,
    /// For a [`ReturnPlan::Registers`] return, the return register each logical
    /// slot arrives in (see [`return_slot_regs`]). Empty for every other return
    /// plan, which has no register slots to read back.
    pub return_slot_regs: Vec<ReturnSlotReg>,
    /// For a compact aggregate returned via sret under `aggregate_layout`
    /// (ADR-0052 phase 5.7, RUE-1004), the internal-slot → physical-byte image
    /// the callee writes and the caller reads back through the sret buffer.
    /// `None` for a slot-identical sret return (read back one slot per eight
    /// bytes, unchanged) and for every non-sret return.
    pub compact_return_image: Option<Vec<crate::types::PhysicalEnumSlot>>,
    /// For a HETEROGENEOUS compact aggregate returned via sret (no single
    /// variant-independent map, RUE-1037), the tag-dispatched image the callee
    /// writes and the caller reads back. Set only when [`Self::compact_return_image`]
    /// is `None` because the return has no single map; `None` otherwise.
    pub compact_return_dispatch: Option<crate::types::DispatchImage>,
    /// Result vreg reserved by the shared dispatcher before argument leaves
    /// materialize, preserving the canonical event allocation order.
    pub result: Option<VReg>,
    /// Float width of the result's LOGICAL SLOT 0 — the width of the move that
    /// loads the primary result vreg. `None` when that slot is general-purpose.
    /// See `value_plan::primary_slot_float_width`.
    pub result_float_width: Option<crate::value_plan::FloatWidth>,
    pub stack_slot_count: usize,
    pub stack_bytes: u32,
    /// Total bytes of caller-owned compact-image buffers reserved for by-value
    /// indirect aggregate arguments (RUE-1005). Allocated below the sret buffer
    /// and above the outgoing stack arguments; freed together right after the
    /// call, before the sret read-back. Zero when no argument crosses
    /// indirectly.
    pub caller_indirect_bytes: u32,
}

/// Input metadata for one CFG call argument. This is copied out of the CFG
/// before the mutable materialization adapter is borrowed. A by-reference
/// input deliberately has no CFG value handle: its addressability has already
/// been decided by the shared value planner.
#[derive(Debug, Clone)]
pub enum CallArgInput {
    Value {
        value: rue_cfg::CfgValue,
        slot_count: u32,
        is_multislot_aggregate: bool,
        slot_types: Vec<Type>,
    },
    /// A by-value non-slot-identical compact aggregate the classifier forces
    /// indirect under `aggregate_layout` (RUE-976/RUE-1005): the caller writes
    /// its compact memory `image` into a caller-owned buffer of `storage_bytes`
    /// and passes one pointer to the callee, which unmarshals it in its
    /// prologue. Kept separate from `Value` so the single pointer ABI slot and
    /// the buffer accounting cannot be confused with an N-slot direct value.
    IndirectValue {
        value: rue_cfg::CfgValue,
        image: Vec<crate::types::PhysicalEnumSlot>,
        /// Byte ranges of the compact image that hold padding, zeroed before the
        /// field stores so the caller-owned buffer is deterministically
        /// initialized (ADR-0052 ruling 5).
        padding: Vec<rue_air::layout::PaddingRange>,
        storage_bytes: u32,
    },
    /// A by-value HETEROGENEOUS compact aggregate argument the classifier forces
    /// indirect (RUE-1037): the caller writes its tag-dispatched compact `image`
    /// into a caller-owned buffer and passes one pointer, exactly like
    /// [`Self::IndirectValue`] but with a per-variant tag dispatch instead of a
    /// single variant-independent map.
    IndirectValueDispatch {
        value: rue_cfg::CfgValue,
        image: crate::types::DispatchImage,
        storage_bytes: u32,
    },
    ByRef {
        mode: ByRefMode,
        address: crate::value_plan::ByRefAddressPlan,
    },
}

/// Shared policy inputs copied from a CFG before backend materialization.
#[derive(Debug, Clone)]
pub struct CallInputs {
    pub args: Vec<CallArgInput>,
    pub return_plan: ReturnPlan,
    /// The compact sret image for the return type, when it is a non-slot-identical
    /// aggregate returned via sret under `aggregate_layout` (RUE-1004). See
    /// [`CallPlan::compact_return_image`].
    pub compact_return_image: Option<Vec<crate::types::PhysicalEnumSlot>>,
    /// The tag-dispatched sret image for a heterogeneous compact aggregate return
    /// (RUE-1037). See [`CallPlan::compact_return_dispatch`].
    pub compact_return_dispatch: Option<crate::types::DispatchImage>,
}

impl CallInputs {
    pub(crate) fn from_cfg(
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        return_ty: Type,
        args: &[CfgCallArg],
        by_ref_plans: &[Option<crate::value_plan::ByRefAddressPlan>],
        ret_reg_budget: u32,
    ) -> Self {
        assert_eq!(
            args.len(),
            by_ref_plans.len(),
            "call argument classification must cover every CFG argument"
        );
        let args = args
            .iter()
            .zip(by_ref_plans)
            .map(|(arg, by_ref_plan)| {
                let arg_ty = cfg.get_inst(arg.value).ty;
                // A by-value aggregate the classifier forces indirect under the
                // compact layout crosses through a caller-owned compact image
                // buffer (RUE-1005), not as N direct decomposition slots. The
                // refusal scan has already guaranteed such an aggregate has a
                // variant-independent image before lowering reaches here.
                if matches!(arg.mode, CfgArgMode::Normal)
                    && matches!(
                        NativeCallAbi::for_arguments(type_pool)
                            .classify_arg(arg_ty, ArgConvention::ByValue),
                        ArgClass::Indirect
                    )
                {
                    let storage_bytes = crate::frame_layout::checked_aligned_region_bytes(
                        types::type_size_bytes(type_pool, arg_ty),
                    )
                    .expect("indirect argument storage must pass frame-budget preflight");
                    // A heterogeneous compact aggregate has no single map; it
                    // marshals its buffer with a per-variant tag dispatch (RUE-1037).
                    if let Some(image) = types::aggregate_physical_slot_map(type_pool, arg_ty) {
                        let padding = type_pool.compact_image_padding_ranges(arg_ty);
                        return CallArgInput::IndirectValue {
                            value: arg.value,
                            image,
                            padding,
                            storage_bytes,
                        };
                    }
                    let image = types::aggregate_dispatch_image(type_pool, arg_ty).expect(
                        "a by-value indirect compact aggregate argument must have a \
                         variant-independent or tag-dispatched memory image (guaranteed by \
                         the refusal scan)",
                    );
                    return CallArgInput::IndirectValueDispatch {
                        value: arg.value,
                        image,
                        storage_bytes,
                    };
                }
                normalize_call_arg(
                    arg.mode,
                    arg.value,
                    types::type_slot_count(type_pool, arg_ty),
                    types::is_multislot_aggregate(type_pool, arg_ty),
                    if types::is_multislot_aggregate(type_pool, arg_ty) {
                        types::aggregate_leaf_types(type_pool, arg_ty)
                    } else {
                        vec![arg_ty]
                    },
                    by_ref_plan.clone(),
                )
                .unwrap_or_else(|message| panic!("{message}"))
            })
            .collect();
        let return_plan = return_plan(type_pool, return_ty, ret_reg_budget);
        // A compact aggregate returned via sret carries its physical image so both
        // the callee write and the caller read-back marshal the same bytes.
        let compact_return_image = if return_plan.uses_sret() {
            types::aggregate_physical_slot_map(type_pool, return_ty)
        } else {
            None
        };
        // A heterogeneous compact aggregate return (no single map) marshals its
        // sret image with a per-variant tag dispatch (RUE-1037).
        let compact_return_dispatch = if return_plan.uses_sret() && compact_return_image.is_none() {
            types::aggregate_dispatch_image(type_pool, return_ty)
        } else {
            None
        };
        Self {
            args,
            return_plan,
            compact_return_image,
            compact_return_dispatch,
        }
    }
}

fn normalize_call_arg(
    mode: CfgArgMode,
    value: rue_cfg::CfgValue,
    slot_count: u32,
    is_multislot_aggregate: bool,
    slot_types: Vec<Type>,
    by_ref_plan: Option<crate::value_plan::ByRefAddressPlan>,
) -> Result<CallArgInput, &'static str> {
    match (mode, by_ref_plan) {
        (CfgArgMode::Normal, None) => Ok(CallArgInput::Value {
            value,
            slot_count,
            is_multislot_aggregate,
            slot_types,
        }),
        (CfgArgMode::Inout, Some(address)) => Ok(CallArgInput::ByRef {
            mode: ByRefMode::Inout,
            address,
        }),
        (CfgArgMode::Borrow, Some(address)) => Ok(CallArgInput::ByRef {
            mode: ByRefMode::Borrow,
            address,
        }),
        (CfgArgMode::Normal, Some(_)) => {
            Err("normal call argument cannot have a by-reference address plan")
        }
        (CfgArgMode::Inout | CfgArgMode::Borrow, None) => {
            Err("by-reference call argument requires an address plan")
        }
    }
}

/// Existing backend leaves used to materialize a plan's logical values.
/// Keeping these as one adapter prevents competing mutable callbacks from
/// creating a second aggregate or by-ref discovery path.
pub trait CallMaterializer {
    /// The convention this backend's compilation target resolves `"C"` to.
    /// Names the classifier that governs every C boundary the backend lowers —
    /// a foreign call, a compiler-built memory routine, or a runtime helper. It
    /// comes from the target rather than the architecture, so AArch64 Linux and
    /// AArch64 macOS answer differently.
    fn target_c_convention(&self) -> CallingConvention;
    fn materialize_scalar(&mut self, value: rue_cfg::CfgValue) -> VReg;
    fn materialize_aggregate(&mut self, value: rue_cfg::CfgValue) -> Vec<VReg>;
    fn materialize_by_ref(&mut self, plan: &crate::value_plan::ByRefAddressPlan) -> VReg;
    fn materialize_sret_pointer(&mut self, storage_bytes: u32) -> VReg;
    /// Reserve a `storage_bytes` caller-owned buffer, write the aggregate
    /// `value`'s compact `image` into it, and return a pointer to it (RUE-1005).
    /// The single pointer becomes the argument's ABI slot; the callee prologue
    /// homes it and unmarshals the image into its frame parameter slots.
    fn materialize_indirect_value_arg(
        &mut self,
        value: rue_cfg::CfgValue,
        image: &[crate::types::PhysicalEnumSlot],
        padding: &[rue_air::layout::PaddingRange],
        storage_bytes: u32,
    ) -> VReg;
    /// Tag-dispatched counterpart of [`Self::materialize_indirect_value_arg`] for
    /// a heterogeneous compact aggregate argument (RUE-1037): reserve the buffer,
    /// write the tag-dispatched compact `image`, and return the pointer.
    fn materialize_indirect_value_arg_dispatch(
        &mut self,
        value: rue_cfg::CfgValue,
        image: &crate::types::DispatchImage,
        storage_bytes: u32,
    ) -> VReg;
}

impl CallPlan {
    /// Build a complete plan from copied CFG argument metadata.
    ///
    /// The callbacks are materialization leaves only: aggregate discovery is
    /// still required to return exactly the type's canonical slot count, and
    /// by-reference arguments always return one address vreg, including for a
    /// zero-sized pointee.
    pub fn from_inputs<M: CallMaterializer>(
        target: CallTarget,
        return_plan: ReturnPlan,
        args: &[CallArgInput],
        arg_register_banks: impl Into<AbiRegisterBanks>,
        materializer: &mut M,
    ) -> Self {
        Self::from_inputs_with_result(
            target,
            return_plan,
            None,
            None,
            args,
            arg_register_banks.into(),
            materializer,
            None,
        )
    }

    pub fn from_inputs_with_result<M: CallMaterializer>(
        target: CallTarget,
        return_plan: ReturnPlan,
        compact_return_image: Option<Vec<crate::types::PhysicalEnumSlot>>,
        compact_return_dispatch: Option<crate::types::DispatchImage>,
        args: &[CallArgInput],
        arg_register_banks: impl Into<AbiRegisterBanks>,
        materializer: &mut M,
        result: Option<VReg>,
    ) -> Self {
        let callee_convention = callee_convention(&target, materializer.target_c_convention());
        let mut hidden_sret = None;
        let mut abi_slots = Vec::new();
        let mut abi_classes = Vec::new();

        if let ReturnPlan::Sret {
            slot_count,
            storage_bytes,
        } = return_plan
        {
            let pointer = materializer.materialize_sret_pointer(storage_bytes);
            hidden_sret = Some(HiddenSretPlan {
                pointer,
                abi_slot: 0,
                slot_count,
                storage_bytes,
            });
            abi_slots.push(pointer);
            abi_classes.push(AbiSlotClass::Gp);
        }

        let mut user_args = Vec::with_capacity(args.len());
        let mut caller_indirect_bytes = 0u32;
        for arg in args {
            let (mode, slots, classes) = match arg {
                CallArgInput::ByRef { mode, address } => (
                    (*mode).into(),
                    // A reference is an ABI pointer even when the pointee has
                    // no storage slots. This branch precedes ZST omission.
                    vec![materializer.materialize_by_ref(address)],
                    vec![AbiSlotClass::Gp],
                ),
                CallArgInput::IndirectValue {
                    value,
                    image,
                    padding,
                    storage_bytes,
                } => {
                    // The caller writes the aggregate's compact image into its
                    // own buffer and passes one pointer (RUE-1005). The buffers
                    // are reserved below the sret storage and freed together
                    // after the call.
                    caller_indirect_bytes = caller_indirect_bytes
                        .checked_add(*storage_bytes)
                        .expect("indirect argument area must fit u32");
                    let pointer = materializer.materialize_indirect_value_arg(
                        *value,
                        image,
                        padding,
                        *storage_bytes,
                    );
                    (UserArgMode::Value, vec![pointer], vec![AbiSlotClass::Gp])
                }
                CallArgInput::IndirectValueDispatch {
                    value,
                    image,
                    storage_bytes,
                } => {
                    // As IndirectValue, but the buffer is written with a per-variant
                    // tag dispatch (RUE-1037).
                    caller_indirect_bytes = caller_indirect_bytes
                        .checked_add(*storage_bytes)
                        .expect("indirect argument area must fit u32");
                    let pointer = materializer.materialize_indirect_value_arg_dispatch(
                        *value,
                        image,
                        *storage_bytes,
                    );
                    (UserArgMode::Value, vec![pointer], vec![AbiSlotClass::Gp])
                }
                CallArgInput::Value {
                    value,
                    slot_count,
                    is_multislot_aggregate,
                    slot_types,
                } => {
                    let slots = if *slot_count == 0 {
                        Vec::new()
                    } else if *is_multislot_aggregate {
                        let slots = materializer.materialize_aggregate(*value);
                        assert_eq!(
                            slots.len(),
                            *slot_count as usize,
                            "aggregate materialization must produce every logical ABI slot"
                        );
                        if callee_convention.is_rue() {
                            slots.into_iter().rev().collect()
                        } else {
                            slots
                        }
                    } else {
                        vec![materializer.materialize_scalar(*value)]
                    };
                    // A zero-sized normal argument has no ABI slot. Its
                    // semantic type can still appear in `slot_types`, but it
                    // must not consume a register-bank index or a stack slot.
                    let mut classes = if slots.is_empty() {
                        Vec::new()
                    } else {
                        slot_types
                            .iter()
                            .copied()
                            .map(AbiSlotClass::for_leaf)
                            .collect::<Vec<_>>()
                    };
                    if *is_multislot_aggregate && callee_convention.is_rue() {
                        classes.reverse();
                    }
                    assert_eq!(
                        slots.len(),
                        classes.len(),
                        "value argument ABI slots and classes must have equal cardinality"
                    );
                    (UserArgMode::Value, slots, classes)
                }
            };

            abi_slots.extend(slots.iter().copied());
            abi_classes.extend(classes);
            user_args.push(UserArgPlan { mode, slots });
        }

        assert_eq!(
            abi_slots.len(),
            abi_classes.len(),
            "call ABI slots and classes must have equal cardinality"
        );
        let abi_locations =
            assign_abi_slots(abi_classes.iter().copied(), arg_register_banks.into());
        assert_eq!(
            abi_slots.len(),
            abi_locations.len(),
            "call ABI slots and locations must have equal cardinality"
        );
        let stack_slot_count = abi_locations
            .iter()
            .filter(|location| matches!(location, AbiSlotLocation::Stack(_)))
            .count();
        let stack_bytes = checked_aligned_cell_region_bytes(
            u64::try_from(stack_slot_count).expect("call stack slot count must fit u64"),
        )
        .expect("call stack area must pass frame-budget preflight");

        Self {
            target,
            callee_convention,
            hidden_sret,
            user_args,
            abi_slots,
            abi_classes,
            abi_locations,
            return_plan,
            // Set by the caller (the value-plan Call arm), which holds the
            // return type and the target's return-register banks.
            return_slot_regs: Vec::new(),
            compact_return_image,
            compact_return_dispatch,
            result,
            result_float_width: None,
            stack_slot_count,
            stack_bytes,
            caller_indirect_bytes,
        }
    }

    /// Build the same normalized shape for drop/glue calls whose slots have
    /// already been materialized by the canonical aggregate leaves.
    pub fn from_slot_values(
        target: CallTarget,
        slots: &[VReg],
        arg_register_banks: impl Into<AbiRegisterBanks>,
        c_convention: CallingConvention,
    ) -> Self {
        let abi_classes = vec![AbiSlotClass::Gp; slots.len()];
        let abi_locations =
            assign_abi_slots(abi_classes.iter().copied(), arg_register_banks.into());
        let stack_slot_count = abi_locations
            .iter()
            .filter(|location| matches!(location, AbiSlotLocation::Stack(_)))
            .count();
        Self {
            callee_convention: callee_convention(&target, c_convention),
            target,
            hidden_sret: None,
            user_args: vec![UserArgPlan {
                mode: UserArgMode::Value,
                slots: slots.to_vec(),
            }],
            abi_slots: slots.to_vec(),
            abi_classes,
            abi_locations,
            return_plan: ReturnPlan::ZeroSized,
            return_slot_regs: Vec::new(),
            compact_return_image: None,
            compact_return_dispatch: None,
            result: None,
            result_float_width: None,
            stack_slot_count,
            stack_bytes: checked_aligned_cell_region_bytes(
                u64::try_from(stack_slot_count).expect("call stack slot count must fit u64"),
            )
            .expect("call stack area must pass frame-budget preflight"),
            caller_indirect_bytes: 0,
        }
    }
}

/// The one shared return policy.  Return classification (scalar / registers /
/// sret) is delegated to the canonical call-ABI classifier
/// [`NativeCallAbi::classify_return`]; this function only maps its result onto
/// the codegen [`ReturnPlan`], adding the caller-storage byte size that sret
/// needs. Keeping the classification in the shared authority is what makes both
/// backends and the oracle agree by construction.
pub fn return_plan(type_pool: &FrozenTypeInternPool, ty: Type, ret_reg_budget: u32) -> ReturnPlan {
    match NativeCallAbi::new(type_pool, ret_reg_budget).classify_return(ty) {
        ReturnClass::ZeroSized => ReturnPlan::ZeroSized,
        ReturnClass::Scalar => ReturnPlan::Scalar,
        ReturnClass::Registers { slot_count } => ReturnPlan::Registers { slot_count },
        ReturnClass::Indirect { slot_count } => ReturnPlan::Sret {
            slot_count,
            storage_bytes: checked_aligned_cell_region_bytes(u64::from(slot_count))
                .expect("sret storage must pass frame-budget preflight"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestMaterializer;

    impl CallMaterializer for TestMaterializer {
        fn target_c_convention(&self) -> CallingConvention {
            CallingConvention::X86_64SysV
        }

        fn materialize_scalar(&mut self, value: rue_cfg::CfgValue) -> VReg {
            VReg::new(100 + value.as_u32())
        }

        fn materialize_aggregate(&mut self, _value: rue_cfg::CfgValue) -> Vec<VReg> {
            vec![VReg::new(10), VReg::new(11)]
        }

        fn materialize_by_ref(&mut self, _plan: &crate::value_plan::ByRefAddressPlan) -> VReg {
            VReg::new(30)
        }

        fn materialize_sret_pointer(&mut self, _storage_bytes: u32) -> VReg {
            VReg::new(40)
        }

        fn materialize_indirect_value_arg(
            &mut self,
            _value: rue_cfg::CfgValue,
            _image: &[crate::types::PhysicalEnumSlot],
            _padding: &[rue_air::layout::PaddingRange],
            _storage_bytes: u32,
        ) -> VReg {
            VReg::new(50)
        }

        fn materialize_indirect_value_arg_dispatch(
            &mut self,
            _value: rue_cfg::CfgValue,
            _image: &crate::types::DispatchImage,
            _storage_bytes: u32,
        ) -> VReg {
            VReg::new(51)
        }
    }

    #[test]
    fn call_arg_input_structurally_separates_values_and_by_ref_modes() {
        let address = crate::value_plan::ByRefAddressPlan::FrameSlot {
            slot: 2,
            low_shift: 0,
        };
        let value = normalize_call_arg(
            CfgArgMode::Normal,
            rue_cfg::CfgValue::from_raw(1),
            1,
            false,
            vec![Type::I32],
            None,
        )
        .expect("normal values should normalize without an address plan");
        assert!(matches!(value, CallArgInput::Value { .. }));

        let by_ref = normalize_call_arg(
            CfgArgMode::Borrow,
            rue_cfg::CfgValue::from_raw(2),
            1,
            false,
            vec![Type::I32],
            Some(address.clone()),
        )
        .expect("borrow arguments should normalize with an address plan");
        assert!(matches!(
            by_ref,
            CallArgInput::ByRef {
                mode: ByRefMode::Borrow,
                ..
            }
        ));

        assert!(
            normalize_call_arg(
                CfgArgMode::Normal,
                rue_cfg::CfgValue::from_raw(3),
                1,
                false,
                vec![Type::I32],
                Some(address.clone()),
            )
            .is_err()
        );
        assert!(
            normalize_call_arg(
                CfgArgMode::Inout,
                rue_cfg::CfgValue::from_raw(4),
                1,
                false,
                vec![Type::I32],
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn slot_call_plan_counts_aligned_stack_slots() {
        let slots: Vec<_> = (0..9).map(VReg::new).collect();
        let plan = CallPlan::from_slot_values(
            CallTarget::rue("drop"),
            &slots,
            6,
            CallingConvention::X86_64SysV,
        );

        assert_eq!(plan.abi_slots, slots);
        assert_eq!(plan.stack_slot_count, 3);
        assert_eq!(plan.stack_bytes, 32);
        assert_eq!(plan.return_plan, ReturnPlan::ZeroSized);
    }

    #[test]
    fn abi_register_banks_fill_independently_and_share_stack_order() {
        let classes = [
            AbiSlotClass::Fp(crate::value_plan::FloatWidth::F32),
            AbiSlotClass::Gp,
            AbiSlotClass::Gp,
            AbiSlotClass::Fp(crate::value_plan::FloatWidth::F64),
            AbiSlotClass::Gp,
            AbiSlotClass::Fp(crate::value_plan::FloatWidth::F32),
        ];
        assert_eq!(
            assign_abi_slots(classes, AbiRegisterBanks { gp: 2, fp: 2 }),
            vec![
                AbiSlotLocation::FpReg(0),
                AbiSlotLocation::GpReg(0),
                AbiSlotLocation::GpReg(1),
                AbiSlotLocation::FpReg(1),
                AbiSlotLocation::Stack(0),
                AbiSlotLocation::Stack(1),
            ]
        );
    }

    #[test]
    fn zero_sized_normal_arguments_are_omitted_but_by_ref_is_preserved() {
        let slots = vec![VReg::new(7)];
        let plan = CallPlan {
            target: CallTarget::rue("test"),
            callee_convention: CallingConvention::Rue,
            hidden_sret: None,
            user_args: vec![
                UserArgPlan {
                    mode: UserArgMode::Value,
                    slots: vec![],
                },
                UserArgPlan {
                    mode: UserArgMode::Borrow,
                    slots: slots.clone(),
                },
            ],
            abi_slots: slots.clone(),
            abi_classes: vec![AbiSlotClass::Gp],
            abi_locations: vec![AbiSlotLocation::GpReg(0)],
            return_plan: ReturnPlan::ZeroSized,
            return_slot_regs: Vec::new(),
            compact_return_image: None,
            compact_return_dispatch: None,
            result: None,
            result_float_width: None,
            stack_slot_count: 0,
            stack_bytes: 0,
            caller_indirect_bytes: 0,
        };

        assert!(plan.user_args[0].slots.is_empty());
        assert_eq!(plan.user_args[1].slots, slots);
        assert_eq!(plan.abi_slots.len(), 1);
    }

    #[test]
    fn zero_sized_normal_arguments_do_not_consume_abi_locations() {
        let mut args = Vec::new();
        for value in 1..=9 {
            if value == 4 {
                // Semantic lowering retains the unit type even though the
                // value has no physical ABI slot.
                args.push(CallArgInput::Value {
                    value: rue_cfg::CfgValue::from_raw(100),
                    slot_count: 0,
                    is_multislot_aggregate: false,
                    slot_types: vec![Type::UNIT],
                });
            }
            args.push(CallArgInput::Value {
                value: rue_cfg::CfgValue::from_raw(value),
                slot_count: 1,
                is_multislot_aggregate: false,
                slot_types: vec![Type::I32],
            });
        }

        let mut materializer = TestMaterializer;
        let plan = CallPlan::from_inputs(
            CallTarget::rue("callee"),
            ReturnPlan::ZeroSized,
            &args,
            AbiRegisterBanks { gp: 6, fp: 8 },
            &mut materializer,
        );

        assert_eq!(plan.user_args[3].slots, Vec::<VReg>::new());
        assert_eq!(plan.abi_slots.len(), 9);
        assert_eq!(plan.abi_classes.len(), 9);
        assert_eq!(plan.abi_locations.len(), 9);
        assert_eq!(
            plan.abi_locations,
            vec![
                AbiSlotLocation::GpReg(0),
                AbiSlotLocation::GpReg(1),
                AbiSlotLocation::GpReg(2),
                AbiSlotLocation::GpReg(3),
                AbiSlotLocation::GpReg(4),
                AbiSlotLocation::GpReg(5),
                AbiSlotLocation::Stack(0),
                AbiSlotLocation::Stack(1),
                AbiSlotLocation::Stack(2),
            ]
        );
        assert_eq!(plan.stack_slot_count, 3);
        assert_eq!(plan.stack_bytes, 32);
    }

    #[test]
    fn return_plan_preserves_sret_storage_alignment() {
        // This checks the normalized shape independently of a target register
        // enum; type-pool-backed sret selection is exercised by backend ABI
        // tests through `return_plan`.
        assert_eq!(checked_aligned_cell_region_bytes(3), Ok(32));
        assert_eq!(
            ReturnPlan::Sret {
                slot_count: 3,
                storage_bytes: 32
            }
            .slot_count(),
            3
        );
    }

    #[test]
    fn cfg_plan_orders_aggregate_slots_by_callee_abi_and_keeps_hidden_sret_first() {
        let args = [
            CallArgInput::Value {
                value: rue_cfg::CfgValue::from_raw(1),
                slot_count: 2,
                is_multislot_aggregate: true,
                slot_types: vec![Type::I32, Type::I32],
            },
            CallArgInput::ByRef {
                mode: ByRefMode::Borrow,
                address: crate::value_plan::ByRefAddressPlan::FrameSlot {
                    slot: 2,
                    low_shift: 0,
                },
            },
            CallArgInput::Value {
                value: rue_cfg::CfgValue::from_raw(3),
                slot_count: 0,
                is_multislot_aggregate: false,
                slot_types: Vec::new(),
            },
        ];
        let mut materializer = TestMaterializer;
        let rue = CallPlan::from_inputs(
            CallTarget::rue("callee"),
            ReturnPlan::Sret {
                slot_count: 3,
                storage_bytes: 32,
            },
            &args,
            3,
            &mut materializer,
        );

        assert_eq!(
            rue.abi_slots,
            vec![VReg::new(40), VReg::new(11), VReg::new(10), VReg::new(30)]
        );
        assert_eq!(rue.hidden_sret.unwrap().abi_slot, 0);
        assert_eq!(rue.stack_slot_count, 1);
        assert_eq!(rue.stack_bytes, 16);
        assert_eq!(rue.user_args[1].mode, UserArgMode::Borrow);
        assert!(rue.user_args[2].slots.is_empty());
    }

    #[test]
    fn generated_rue_symbols_do_not_trigger_runtime_abi_by_prefix() {
        let args = [CallArgInput::Value {
            value: rue_cfg::CfgValue::from_raw(1),
            slot_count: 2,
            is_multislot_aggregate: true,
            slot_types: vec![Type::I32, Type::I32],
        }];

        let mut materializer = TestMaterializer;
        let generated_rue = CallPlan::from_inputs(
            CallTarget::rue("__rue_drop_generated"),
            ReturnPlan::ZeroSized,
            &args,
            8,
            &mut materializer,
        );
        assert_eq!(generated_rue.callee_convention, CallingConvention::Rue);
        assert_eq!(generated_rue.abi_slots, vec![VReg::new(11), VReg::new(10)]);
    }

    #[test]
    fn memory_builtins_are_distinct_from_helpers_and_rue_symbols() {
        let target = CallTarget::memory_builtin(ReservedExportId::Memcpy);
        assert_eq!(target.symbol(), "memcpy");
        // A memory builtin crosses the caller's `"C"` boundary, so it takes
        // whichever row the compilation target resolves the alias to.
        assert_eq!(
            callee_convention(&target, CallingConvention::Aarch64AapcsDarwin),
            CallingConvention::Aarch64AapcsDarwin
        );
        assert_eq!(
            callee_convention(&CallTarget::rue("f"), CallingConvention::Aarch64AapcsDarwin),
            CallingConvention::Rue
        );
        assert!(matches!(
            target,
            CallTarget::MemoryBuiltin(id) if id.export_id() == ReservedExportId::Memcpy
        ));
    }
}
