//! Target-independent planning for Rue calls.
//!
//! This module decides the logical ABI shape of a call.  It deliberately does
//! not know about physical registers or target instructions; the two backend
//! lowerers consume the normalized slot vector and only choose how to marshal
//! those slots for their ABI.

use rue_air::{
    ArgConvention, ArgLocation, FrozenTypeInternPool, LoweredReturn, NativeCallAbi,
    PointerLocation, RegisterPiece, ReturnClass, lower_native_signature,
};
// `ScalarAbiExtension` is referenced by fully-qualified path in the struct so it
// stays visible to readers of the field; no direct import is needed.
use rue_cfg::{Cfg, CfgArgMode, CfgCallArg, Type};
use rue_runtime_abi::{ReservedExportClass, ReservedExportId};
use rue_target::{CRegisterClass, CallingConvention, ConventionSpec};

use crate::native_abi::{NativeArg, NativeArgMarshal, NativeImage, native_arg};

use crate::types;
use crate::vreg::VReg;

use crate::frame_layout::checked_aligned_cell_region_bytes;

/// The convention a call target's callee follows.
///
/// A Rue-compiled function uses the native convention, which places every
/// argument where the compilation target's own C row places it (ADR-0084). A
/// compiler-built C memory routine crosses that C row directly; which row it is
/// comes from the compilation target, so the caller supplies it.
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

    /// The register bank this class travels in.
    pub const fn bank(self) -> CRegisterClass {
        match self {
            Self::Gp => CRegisterClass::Gp,
            Self::Fp(_) => CRegisterClass::Fp,
        }
    }
}

/// Where one native ABI slot travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiSlotLocation {
    /// Argument register `index` of the general-purpose bank.
    GpReg(usize),
    /// Argument register `index` of the floating-point bank.
    FpReg(usize),
    /// In the outgoing argument area: `size` bytes at `offset`, whose
    /// alignment the placement already satisfied — the same description a
    /// stacked C argument carries (`rue_air::ArgLocation::Stack`).
    Stack { offset: u32, size: u32, align: u32 },
}

impl AbiSlotLocation {
    /// The `index`-th eight-byte slot of the outgoing argument area.
    ///
    /// A native ABI slot carries a canonically 64-bit-extended value
    /// (ADR-0084), so it occupies one whole eightbyte at eight-byte alignment
    /// under every supported row's stacked-argument packing — Apple's
    /// natural-size amendment included, since eight bytes *is* the slot's
    /// natural size. That is what lets a caller holding only a slot index name
    /// the place the convention's own argument area gives it.
    pub const fn stack_slot(index: usize) -> Self {
        Self::Stack {
            offset: (index * rue_air::SLOT_BYTES as usize) as u32,
            size: rue_air::SLOT_BYTES as u32,
            align: rue_air::SLOT_BYTES as u32,
        }
    }
}

/// Assign each result slot a register of its own bank, in order, or `None` once
/// that bank's roster is spent. A spent roster still counts the slot, so the
/// banks stay independent of each other.
fn claim_bank_registers(
    classes: impl IntoIterator<Item = AbiSlotClass>,
    banks: AbiRegisterBanks,
) -> impl Iterator<Item = Option<AbiSlotLocation>> {
    let mut gp = 0usize;
    let mut fp = 0usize;
    classes.into_iter().map(move |class| match class {
        AbiSlotClass::Gp => {
            let index = gp;
            gp += 1;
            (index < banks.gp).then_some(AbiSlotLocation::GpReg(index))
        }
        AbiSlotClass::Fp(_) => {
            let index = fp;
            fp += 1;
            (index < banks.fp).then_some(AbiSlotLocation::FpReg(index))
        }
    })
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
    // A result has no argument area to overflow into, so the bank assignment
    // is the whole answer and a slot that does not fit is a classification
    // error rather than a stacked slot.
    claim_bank_registers(classes.clone(), banks)
        .zip(classes)
        .map(|(location, class)| match (location, class) {
            (Some(AbiSlotLocation::GpReg(index)), _) => ReturnSlotReg::Gp(index),
            (Some(AbiSlotLocation::FpReg(index)), AbiSlotClass::Fp(width)) => {
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
        /// The argument's own type, which the convention classifies.
        ty: Type,
        slot_count: u32,
        is_multislot_aggregate: bool,
        slot_types: Vec<Type>,
    },
    ByRef {
        mode: ByRefMode,
        address: crate::value_plan::ByRefAddressPlan,
    },
}

impl CallArgInput {
    /// The source-level mode the convention classifies this argument under.
    fn arg_convention(&self) -> ArgConvention {
        match self {
            Self::Value { .. } => ArgConvention::ByValue,
            Self::ByRef { .. } => ArgConvention::ByReference,
        }
    }
}

/// Shared policy inputs copied from a CFG before backend materialization.
#[derive(Debug, Clone)]
pub struct CallInputs {
    pub args: Vec<CallArgInput>,
    /// The native description of each argument, in source order: the facts the
    /// convention classifies and the marshaling that reaches its placement.
    pub natives: Vec<NativeArg>,
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
        let args: Vec<CallArgInput> = args
            .iter()
            .zip(by_ref_plans)
            .map(|(arg, by_ref_plan)| {
                let arg_ty = cfg.get_inst(arg.value).ty;
                let aggregate = types::is_multislot_aggregate(type_pool, arg_ty);
                normalize_call_arg(
                    arg.mode,
                    arg.value,
                    arg_ty,
                    types::type_slot_count(type_pool, arg_ty),
                    aggregate,
                    if aggregate {
                        types::aggregate_leaf_types(type_pool, arg_ty)
                    } else {
                        vec![arg_ty]
                    },
                    by_ref_plan.clone(),
                )
                .unwrap_or_else(|message| panic!("{message}"))
            })
            .collect();
        let natives = args
            .iter()
            .map(|arg| match arg {
                CallArgInput::Value { ty, .. } => {
                    native_arg(type_pool, *ty, ArgConvention::ByValue)
                }
                CallArgInput::ByRef { .. } => {
                    native_arg(type_pool, Type::I64, ArgConvention::ByReference)
                }
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
            natives,
            return_plan,
            compact_return_image,
            compact_return_dispatch,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn normalize_call_arg(
    mode: CfgArgMode,
    value: rue_cfg::CfgValue,
    ty: Type,
    slot_count: u32,
    is_multislot_aggregate: bool,
    slot_types: Vec<Type>,
    by_ref_plan: Option<crate::value_plan::ByRefAddressPlan>,
) -> Result<CallArgInput, &'static str> {
    match (mode, by_ref_plan) {
        (CfgArgMode::Normal, None) => Ok(CallArgInput::Value {
            value,
            ty,
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

    /// The native convention on this backend's compilation target, paired with
    /// the description it places by: the target's own C description with the
    /// native amendments (`ConventionSpec::native`).
    fn native_convention(&self) -> ConventionSpec;
    fn materialize_scalar(&mut self, value: rue_cfg::CfgValue) -> VReg;
    fn materialize_aggregate(&mut self, value: rue_cfg::CfgValue) -> Vec<VReg>;
    fn materialize_by_ref(&mut self, plan: &crate::value_plan::ByRefAddressPlan) -> VReg;
    fn materialize_sret_pointer(&mut self, storage_bytes: u32) -> VReg;
    /// Write the aggregate `value`'s leaves into a scratch stack image and read
    /// its eightbytes back out, one vreg per eightbyte in ascending memory
    /// order. This is the marshaling that packs `{u8, u8, u8, u8}` into one
    /// register; the scratch buffer is released before the call sequence
    /// begins.
    fn materialize_image_eightbytes(
        &mut self,
        value: rue_cfg::CfgValue,
        image: &NativeImage,
    ) -> Vec<VReg>;
    /// Reserve a caller-owned buffer, write the aggregate `value`'s compact
    /// image into it, and return a pointer to it. The buffer stays live across
    /// the call, because the callee reads the aggregate through the pointer
    /// (AAPCS64 section 6.8.2 C.12).
    fn materialize_indirect_image(&mut self, value: rue_cfg::CfgValue, image: &NativeImage)
    -> VReg;
    /// Move the 64 bits of a marshaled eightbyte into a floating-point vreg.
    ///
    /// An eightbyte read out of a compact image is an integer-shaped lane; when
    /// the classification puts it in the floating-point bank the whole lane
    /// crosses, so the move is a bit pattern rather than a numeric conversion.
    fn materialize_eightbyte_as_float(&mut self, bits: VReg) -> VReg;
}

/// The already-decided return a native signature is placed against.
///
/// Phase 1 of ADR-0084 switches arguments only, so return classification stays
/// [`ReturnPlan`]'s and this projects it onto the one fact argument placement
/// depends on: whether a hidden indirect-result pointer takes an ordinary
/// argument register ahead of every user argument.
fn lowered_return(pairing: ConventionSpec, plan: ReturnPlan) -> LoweredReturn {
    let spec = pairing.spec();
    match plan {
        ReturnPlan::ZeroSized => LoweredReturn::Void,
        ReturnPlan::Scalar => LoweredReturn::Registers {
            class: CRegisterClass::Gp,
            count: 1,
            extension: rue_air::ScalarAbiExtension::None,
        },
        ReturnPlan::Registers { slot_count } => LoweredReturn::Registers {
            class: CRegisterClass::Gp,
            count: slot_count,
            extension: rue_air::ScalarAbiExtension::None,
        },
        ReturnPlan::Sret {
            slot_count,
            storage_bytes: _,
        } => LoweredReturn::Sret {
            register: spec.sret_register,
            echoed: spec.sret_pointer_echoed_in_result_register,
            size: slot_count.saturating_mul(rue_air::SLOT_BYTES as u32),
            align: rue_air::SLOT_BYTES as u32,
        },
    }
}

/// Where one register piece of a value travels.
const fn register_piece_location(piece: RegisterPiece) -> AbiSlotLocation {
    match piece.class {
        CRegisterClass::Gp => AbiSlotLocation::GpReg(piece.index as usize),
        CRegisterClass::Fp => AbiSlotLocation::FpReg(piece.index as usize),
    }
}

/// The stack placements one value's eightbytes occupy inside `location`.
///
/// A stacked scalar occupies exactly the footprint the convention's packing
/// gave it — one byte for an `i8` under Apple's amendment. A stacked aggregate
/// crosses through its eightbyte image, so each eightbyte claims its own whole
/// slot at an ascending offset inside the argument's placement.
fn stacked_pieces(location: ArgLocation, scalar: bool, count: usize) -> Vec<AbiSlotLocation> {
    let ArgLocation::Stack {
        offset,
        size,
        align,
    } = location
    else {
        panic!("stacked pieces are only taken from a stacked placement");
    };
    if scalar {
        assert_eq!(count, 1, "a stacked scalar occupies one placement");
        return vec![AbiSlotLocation::Stack {
            offset,
            size,
            align,
        }];
    }
    (0..count)
        .map(|piece| AbiSlotLocation::Stack {
            offset: offset + u32::try_from(piece * 8).expect("stack offset must fit u32"),
            size: rue_air::SLOT_BYTES as u32,
            align: rue_air::SLOT_BYTES as u32,
        })
        .collect()
}

/// Materialize one argument's values and give each the position the lowered
/// signature named for it.
fn place_argument<M: CallMaterializer>(
    arg: &CallArgInput,
    native: &NativeArg,
    placements: &[ArgLocation],
    materializer: &mut M,
    caller_indirect_bytes: &mut u32,
) -> (
    UserArgMode,
    Vec<VReg>,
    Vec<AbiSlotClass>,
    Vec<AbiSlotLocation>,
) {
    let location = placements[0];
    if let CallArgInput::ByRef { mode, address } = arg {
        // A reference is an ABI pointer even when the pointee has no storage
        // slots, so this precedes zero-sized omission.
        let pointer = materializer.materialize_by_ref(address);
        return (
            (*mode).into(),
            vec![pointer],
            vec![AbiSlotClass::Gp],
            vec![scalar_location(location)],
        );
    }
    let CallArgInput::Value {
        value,
        slot_count,
        is_multislot_aggregate,
        ..
    } = arg
    else {
        unreachable!("a by-reference argument was handled above");
    };

    if let NativeArg::PerLeaf { count } = native {
        // The cleanup convention hands each already-materialized leaf to its
        // own register-width placement, in ascending order.
        let values = materializer.materialize_aggregate(*value);
        assert_eq!(
            values.len(),
            *count as usize,
            "cleanup-convention materialization must produce every leaf"
        );
        return (
            UserArgMode::Value,
            values,
            vec![AbiSlotClass::Gp; *count as usize],
            placements.iter().copied().map(scalar_location).collect(),
        );
    }

    if let ArgLocation::Indirect { pointer, .. } = location {
        let NativeArg::Aggregate { image } = native else {
            panic!("only an aggregate crosses by reference under a composite rule");
        };
        *caller_indirect_bytes = caller_indirect_bytes
            .checked_add(image.storage_bytes)
            .expect("indirect argument area must fit u32");
        let address = materializer.materialize_indirect_image(*value, image);
        let position = match pointer {
            PointerLocation::Register { index } => AbiSlotLocation::GpReg(index as usize),
            PointerLocation::Stack { offset } => AbiSlotLocation::Stack {
                offset,
                size: rue_air::SLOT_BYTES as u32,
                align: rue_air::SLOT_BYTES as u32,
            },
        };
        return (
            UserArgMode::Value,
            vec![address],
            vec![AbiSlotClass::Gp],
            vec![position],
        );
    }

    if matches!(location, ArgLocation::Omitted) {
        return (UserArgMode::Value, Vec::new(), Vec::new(), Vec::new());
    }

    // The eightbytes the placement wants: the value's own leaf vregs when every
    // leaf starts its own eightbyte, and the marshaled image otherwise.
    let marshal = native.marshal(location);
    let mut values = match (&marshal, native) {
        (NativeArgMarshal::Image { .. }, NativeArg::Aggregate { image }) => {
            materializer.materialize_image_eightbytes(*value, image)
        }
        _ if *is_multislot_aggregate => {
            let slots = materializer.materialize_aggregate(*value);
            assert_eq!(
                slots.len(),
                *slot_count as usize,
                "aggregate materialization must produce every logical ABI slot"
            );
            slots
        }
        _ => vec![materializer.materialize_scalar(*value)],
    };
    assert_eq!(
        values.len(),
        marshal.eightbyte_count(),
        "one value per eightbyte the classification placed"
    );

    let (classes, locations) = match location {
        ArgLocation::Registers { pieces } => {
            assert_eq!(
                pieces.len() as usize,
                values.len(),
                "one value per register the classification named"
            );
            let classes = pieces
                .as_slice()
                .iter()
                .enumerate()
                .map(|(index, piece)| marshal.class(index, piece.class))
                .collect::<Vec<_>>();
            // A marshaled eightbyte comes out of memory integer-shaped; a piece
            // in the floating-point bank takes its bits as a whole lane.
            if matches!(marshal, NativeArgMarshal::Image { .. }) {
                for (value, class) in values.iter_mut().zip(&classes) {
                    if matches!(class, AbiSlotClass::Fp(_)) {
                        *value = materializer.materialize_eightbyte_as_float(*value);
                    }
                }
            }
            (
                classes,
                pieces
                    .as_slice()
                    .iter()
                    .copied()
                    .map(register_piece_location)
                    .collect::<Vec<_>>(),
            )
        }
        ArgLocation::Stack { .. } => (
            (0..values.len())
                .map(|index| marshal.class(index, marshal.stacked_bank(index)))
                .collect::<Vec<_>>(),
            stacked_pieces(location, native.packs_as_scalar(), values.len()),
        ),
        ArgLocation::Omitted | ArgLocation::Indirect { .. } => {
            unreachable!("both were handled above")
        }
    };
    (UserArgMode::Value, values, classes, locations)
}

/// The position of a value that occupies exactly one register or one stacked
/// placement.
fn scalar_location(location: ArgLocation) -> AbiSlotLocation {
    match location {
        ArgLocation::Registers { pieces } => {
            assert_eq!(pieces.len(), 1, "a scalar occupies one register");
            register_piece_location(pieces.as_slice()[0])
        }
        ArgLocation::Stack {
            offset,
            size,
            align,
        } => AbiSlotLocation::Stack {
            offset,
            size,
            align,
        },
        ArgLocation::Omitted | ArgLocation::Indirect { .. } => {
            panic!("a pointer-sized argument is never omitted and never indirect")
        }
    }
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
        natives: &[NativeArg],
        materializer: &mut M,
    ) -> Self {
        Self::from_inputs_with_result(
            target,
            return_plan,
            None,
            None,
            args,
            natives,
            materializer,
            None,
        )
    }

    /// Place and marshal every argument of one native (or compiler-built C)
    /// call.
    ///
    /// Placement is [`lower_native_signature`]'s answer against the facts each
    /// argument projects, under the convention the target follows: the native
    /// row for a Rue callee, the compilation target's own C row for a
    /// compiler-built memory routine. Marshaling is this function's: a value
    /// whose leaves are its eightbytes hands its own vregs over, one whose
    /// leaves pack together goes through its compact image, and one the
    /// convention passes by reference goes through a caller-owned copy.
    #[allow(clippy::too_many_arguments)]
    pub fn from_inputs_with_result<M: CallMaterializer>(
        target: CallTarget,
        return_plan: ReturnPlan,
        compact_return_image: Option<Vec<crate::types::PhysicalEnumSlot>>,
        compact_return_dispatch: Option<crate::types::DispatchImage>,
        args: &[CallArgInput],
        natives: &[NativeArg],
        materializer: &mut M,
        result: Option<VReg>,
    ) -> Self {
        assert_eq!(
            args.len(),
            natives.len(),
            "every call argument carries its native description"
        );
        let callee_convention = callee_convention(&target, materializer.target_c_convention());
        let pairing = match callee_convention {
            CallingConvention::Rue => materializer.native_convention(),
            row => ConventionSpec::c(row),
        };
        // Every argument contributes one placement, except the cleanup
        // convention's already-flattened leaves, which contribute one each; the
        // spans map an argument back to the run of placements it owns.
        let mut parameters = Vec::with_capacity(args.len());
        let mut spans = Vec::with_capacity(args.len());
        for (arg, native) in args.iter().zip(natives) {
            let start = parameters.len();
            let convention = arg.arg_convention();
            parameters.extend(native.facts().into_iter().map(|facts| (facts, convention)));
            spans.push(start..parameters.len());
        }
        let signature =
            lower_native_signature(pairing, &parameters, lowered_return(pairing, return_plan));

        let mut hidden_sret = None;
        let mut abi_slots = Vec::new();
        let mut abi_classes = Vec::new();
        let mut abi_locations = Vec::new();

        if let ReturnPlan::Sret {
            slot_count,
            storage_bytes,
        } = return_plan
        {
            assert!(
                signature.sret_in_argument_register(),
                "the native convention passes its indirect-result pointer as the \
                 hidden first ordinary argument"
            );
            let pointer = materializer.materialize_sret_pointer(storage_bytes);
            hidden_sret = Some(HiddenSretPlan {
                pointer,
                abi_slot: 0,
                slot_count,
                storage_bytes,
            });
            abi_slots.push(pointer);
            abi_classes.push(AbiSlotClass::Gp);
            abi_locations.push(AbiSlotLocation::GpReg(0));
        }

        let mut user_args = Vec::with_capacity(args.len());
        let mut caller_indirect_bytes = 0u32;
        for ((arg, native), span) in args.iter().zip(natives).zip(spans) {
            let placements = signature.arguments()[span]
                .iter()
                .map(|argument| argument.location)
                .collect::<Vec<_>>();
            let (mode, slots, classes, locations) = place_argument(
                arg,
                native,
                &placements,
                materializer,
                &mut caller_indirect_bytes,
            );
            abi_slots.extend(slots.iter().copied());
            abi_classes.extend(classes);
            abi_locations.extend(locations);
            user_args.push(UserArgPlan { mode, slots });
        }

        assert_eq!(
            abi_slots.len(),
            abi_classes.len(),
            "call ABI slots and classes must have equal cardinality"
        );
        assert_eq!(
            abi_slots.len(),
            abi_locations.len(),
            "call ABI slots and locations must have equal cardinality"
        );
        let stack_slot_count = abi_locations
            .iter()
            .filter(|location| matches!(location, AbiSlotLocation::Stack { .. }))
            .count();

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
            stack_bytes: signature.stack_bytes(),
            caller_indirect_bytes,
        }
    }

    /// Build the same normalized shape for a cleanup call — a destructor or a
    /// drop glue body — whose slots have already been materialized by the
    /// canonical aggregate leaves.
    ///
    /// A cleanup callee receives those leaves directly rather than a
    /// reconstructed value ([`NativeArg::PerLeaf`]), so each leaf is placed as
    /// one register-width argument by the same lowering every other call goes
    /// through. The callee's own parameter plan reads the same arm, so the two
    /// ends cannot disagree.
    pub fn from_slot_values(
        target: CallTarget,
        slots: &[VReg],
        native_convention: ConventionSpec,
        arg_register_banks: impl Into<AbiRegisterBanks>,
        c_convention: CallingConvention,
    ) -> Self {
        let _ = arg_register_banks.into();
        let count = u32::try_from(slots.len()).expect("cleanup leaf count must fit u32");
        let native = NativeArg::PerLeaf { count };
        let parameters = native
            .facts()
            .into_iter()
            .map(|facts| (facts, ArgConvention::ByValue))
            .collect::<Vec<_>>();
        let signature = lower_native_signature(native_convention, &parameters, LoweredReturn::Void);
        let abi_classes = vec![AbiSlotClass::Gp; slots.len()];
        let abi_locations = signature
            .arguments()
            .iter()
            .map(|argument| scalar_location(argument.location))
            .collect::<Vec<_>>();
        let stack_slot_count = abi_locations
            .iter()
            .filter(|location| matches!(location, AbiSlotLocation::Stack { .. }))
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
            stack_bytes: signature.stack_bytes(),
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
    use rue_air::{StructDef, StructField, TypeInternPool};

    struct TestMaterializer;

    impl CallMaterializer for TestMaterializer {
        fn native_convention(&self) -> ConventionSpec {
            ConventionSpec::native(rue_target::Target::X86_64Linux)
        }

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

        fn materialize_image_eightbytes(
            &mut self,
            _value: rue_cfg::CfgValue,
            image: &NativeImage,
        ) -> Vec<VReg> {
            (0..image.eightbytes()).map(|i| VReg::new(50 + i)).collect()
        }

        fn materialize_indirect_image(
            &mut self,
            _value: rue_cfg::CfgValue,
            _image: &NativeImage,
        ) -> VReg {
            VReg::new(60)
        }

        fn materialize_eightbyte_as_float(&mut self, bits: VReg) -> VReg {
            VReg::new(200 + bits.index())
        }
    }

    /// A pool holding one probe struct of the given field types.
    fn pool_with_struct(fields: &[Type]) -> (FrozenTypeInternPool, Type) {
        let interner = lasso::ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let (id, _) = pool.register_struct(
            interner.get_or_intern("Probe"),
            StructDef {
                name: "Probe".into(),
                fields: fields
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| StructField {
                        name: format!("f{index}"),
                        ty: *ty,
                    })
                    .collect(),
                is_copy: true,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        );
        let ty = Type::new_struct(id);
        (pool.freeze(), ty)
    }

    fn scalar_arg(value: u32, ty: Type) -> CallArgInput {
        CallArgInput::Value {
            value: rue_cfg::CfgValue::from_raw(value),
            ty,
            slot_count: 1,
            is_multislot_aggregate: false,
            slot_types: vec![ty],
        }
    }

    fn natives_of(pool: &FrozenTypeInternPool, args: &[CallArgInput]) -> Vec<NativeArg> {
        args.iter()
            .map(|arg| match arg {
                CallArgInput::Value { ty, .. } => native_arg(pool, *ty, ArgConvention::ByValue),
                CallArgInput::ByRef { .. } => {
                    native_arg(pool, Type::I64, ArgConvention::ByReference)
                }
            })
            .collect()
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
            Type::I32,
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
            Type::I32,
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
                Type::I32,
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
                Type::I32,
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
            ConventionSpec::native(rue_target::Target::X86_64Linux),
            6,
            CallingConvention::X86_64SysV,
        );

        assert_eq!(plan.abi_slots, slots);
        assert_eq!(plan.stack_slot_count, 3);
        assert_eq!(plan.stack_bytes, 32);
        assert_eq!(plan.return_plan, ReturnPlan::ZeroSized);
    }

    #[test]
    fn every_row_places_a_cleanup_leaf_in_a_whole_ascending_eightbyte() {
        // The cleanup convention (destructors and drop glue) passes an
        // aggregate's already-flattened leaves one register-width value per
        // leaf, each carrying a canonically 64-bit-extended value, so a leaf the
        // roster cannot hold claims one whole eightbyte from its convention's
        // argument area — under Apple's natural-size packing as much as under
        // the eight-byte slot rows, because eight bytes is a register-width
        // value's natural size.
        let slots: Vec<_> = (0..10).map(VReg::new).collect();
        for target in rue_target::Target::all() {
            let plan = CallPlan::from_slot_values(
                CallTarget::rue("drop"),
                &slots,
                ConventionSpec::native(*target),
                6,
                target.c_calling_convention(),
            );
            let registers =
                usize::try_from(ConventionSpec::native(*target).spec().gp_argument_registers)
                    .expect("a roster size fits usize");
            let stacked = &plan.abi_locations[registers..];
            assert_eq!(
                stacked,
                (0..stacked.len())
                    .map(AbiSlotLocation::stack_slot)
                    .collect::<Vec<_>>(),
                "{target:?}: cleanup leaves stack in whole ascending eightbytes"
            );
            assert_eq!(
                &plan.abi_locations[..registers],
                (0..registers)
                    .map(AbiSlotLocation::GpReg)
                    .collect::<Vec<_>>(),
                "{target:?}: cleanup leaves take the roster in order"
            );
        }
    }

    #[test]
    fn zero_sized_normal_arguments_do_not_consume_abi_locations() {
        let (pool, _) = pool_with_struct(&[Type::I64]);
        let mut args = Vec::new();
        for value in 1..=9 {
            if value == 4 {
                // Semantic lowering retains the unit type even though the
                // value has no physical ABI slot.
                args.push(CallArgInput::Value {
                    value: rue_cfg::CfgValue::from_raw(100),
                    ty: Type::UNIT,
                    slot_count: 0,
                    is_multislot_aggregate: false,
                    slot_types: vec![Type::UNIT],
                });
            }
            args.push(scalar_arg(value, Type::I32));
        }
        let natives = natives_of(&pool, &args);

        let mut materializer = TestMaterializer;
        let plan = CallPlan::from_inputs(
            CallTarget::rue("callee"),
            ReturnPlan::ZeroSized,
            &args,
            &natives,
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
                AbiSlotLocation::stack_slot(0),
                AbiSlotLocation::stack_slot(1),
                AbiSlotLocation::stack_slot(2),
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
    fn a_packed_aggregate_argument_crosses_through_its_image_and_sret_stays_first() {
        // `{i32, i32}` is one eightbyte: its leaves do not start their own
        // eightbytes, so the caller marshals them through the compact image and
        // hands the placement a single register (ADR-0084).
        let (pool, packed) = pool_with_struct(&[Type::I32, Type::I32]);
        let args = [
            CallArgInput::Value {
                value: rue_cfg::CfgValue::from_raw(1),
                ty: packed,
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
                ty: Type::UNIT,
                slot_count: 0,
                is_multislot_aggregate: false,
                slot_types: Vec::new(),
            },
        ];
        let natives = natives_of(&pool, &args);
        let mut materializer = TestMaterializer;
        let rue = CallPlan::from_inputs(
            CallTarget::rue("callee"),
            ReturnPlan::Sret {
                slot_count: 3,
                storage_bytes: 32,
            },
            &args,
            &natives,
            &mut materializer,
        );

        // The hidden sret pointer takes the first argument register, the packed
        // struct the second, and the borrow pointer the third.
        assert_eq!(
            rue.abi_slots,
            vec![VReg::new(40), VReg::new(50), VReg::new(30)]
        );
        assert_eq!(
            rue.abi_locations,
            vec![
                AbiSlotLocation::GpReg(0),
                AbiSlotLocation::GpReg(1),
                AbiSlotLocation::GpReg(2),
            ]
        );
        assert_eq!(rue.hidden_sret.unwrap().abi_slot, 0);
        assert_eq!(rue.stack_slot_count, 0);
        assert_eq!(rue.stack_bytes, 0);
        assert_eq!(rue.user_args[1].mode, UserArgMode::Borrow);
        assert!(rue.user_args[2].slots.is_empty());
    }

    #[test]
    fn an_eightbyte_aligned_aggregate_hands_its_own_leaves_to_the_registers() {
        // `{i64, i64}` has one leaf per eightbyte, in ascending memory order,
        // so the leaf vregs are the eightbytes and nothing is marshaled.
        let (pool, wide) = pool_with_struct(&[Type::I64, Type::I64]);
        let args = [CallArgInput::Value {
            value: rue_cfg::CfgValue::from_raw(1),
            ty: wide,
            slot_count: 2,
            is_multislot_aggregate: true,
            slot_types: vec![Type::I64, Type::I64],
        }];
        let natives = natives_of(&pool, &args);
        let mut materializer = TestMaterializer;
        let plan = CallPlan::from_inputs(
            CallTarget::rue("callee"),
            ReturnPlan::ZeroSized,
            &args,
            &natives,
            &mut materializer,
        );
        // Ascending memory order: leaf 0 in the first register, leaf 1 in the
        // second. No reversal (ADR-0084).
        assert_eq!(plan.abi_slots, vec![VReg::new(10), VReg::new(11)]);
        assert_eq!(
            plan.abi_locations,
            vec![AbiSlotLocation::GpReg(0), AbiSlotLocation::GpReg(1)]
        );
    }

    #[test]
    fn generated_rue_symbols_do_not_trigger_runtime_abi_by_prefix() {
        let (pool, wide) = pool_with_struct(&[Type::I64, Type::I64]);
        let args = [CallArgInput::Value {
            value: rue_cfg::CfgValue::from_raw(1),
            ty: wide,
            slot_count: 2,
            is_multislot_aggregate: true,
            slot_types: vec![Type::I64, Type::I64],
        }];
        let natives = natives_of(&pool, &args);

        let mut materializer = TestMaterializer;
        let generated_rue = CallPlan::from_inputs(
            CallTarget::rue("__rue_drop_generated"),
            ReturnPlan::ZeroSized,
            &args,
            &natives,
            &mut materializer,
        );
        assert_eq!(generated_rue.callee_convention, CallingConvention::Rue);
        assert_eq!(generated_rue.abi_slots, vec![VReg::new(10), VReg::new(11)]);
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
