//! Target-independent planning for Rue calls.
//!
//! This module decides the logical ABI shape of a call.  It deliberately does
//! not know about physical registers or target instructions; the two backend
//! lowerers consume the normalized slot vector and only choose how to marshal
//! those slots for their ABI.

use rue_air::{
    ArgConvention, ArgLocation, FrozenTypeInternPool, LoweredReturn, PointerLocation,
    RegisterPiece, lower_native_signature,
};
// `ScalarAbiExtension` is referenced by fully-qualified path in the struct so it
// stays visible to readers of the field; no direct import is needed.
use rue_cfg::{Cfg, CfgArgMode, CfgCallArg, Type};
use rue_runtime_abi::{ReservedExportClass, ReservedExportId};
use rue_target::{CRegisterClass, CallingConvention, ConventionSpec, SretRegisterKind};

use crate::abi_slot_class::AbiSlotClass;
use crate::native_abi::{
    NativeArg, NativeArgMarshal, NativeImage, native_arg, native_by_value_arg,
};

use crate::types;
use crate::vreg::VReg;

use crate::frame_layout::checked_aligned_cell_region_bytes;

/// The description a call target's callee is placed by.
///
/// This is the only branch left in call planning, and it chooses a *description*
/// rather than an algorithm: one lowering (`lower_native_signature`) walks
/// whichever `ConventionSpec` this returns. A Rue-compiled function is placed by
/// `ConventionSpec::native` — the compilation target's own C description with
/// the native amendments (ADR-0084) — and a compiler-built C memory routine by
/// that target's plain C row, which the caller supplies because the row comes
/// from the whole target rather than from the architecture.
const fn callee_pairing(
    target: &CallTarget,
    native: ConventionSpec,
    c_convention: CallingConvention,
) -> ConventionSpec {
    match target {
        CallTarget::Rue(_) => native,
        CallTarget::MemoryBuiltin(_) => ConventionSpec::c(c_convention),
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

/// Where one eightbyte of a REGISTER-RETURNED value travels.
///
/// The bank and roster index are the lowered return's own
/// ([`rue_air::lower_native_return`]): a result's eightbytes are classified by
/// exactly the rule the target's C row applies to an argument's, read against
/// the wider native return bank (ADR-0084). A `struct P { field: f64 }`
/// therefore comes back in the first floating-point result register — the same
/// bank it would cross in as an argument — and a `{i64, f64}` comes back with
/// one eightbyte in each bank.
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

/// How a register-returned value's eightbytes reach the result registers.
///
/// The two shapes are the argument path's, read in the other direction
/// ([`NativeArgMarshal`]): when every leaf starts its own eightbyte and travels
/// in the bank the classification named, the leaf vregs *are* the eightbytes
/// and nothing is marshaled; otherwise the value's leaves are written into a
/// scratch image at their compact byte offsets and the eightbytes are read out
/// of it, which is what packs `{u8, u8, u8, u8}` into one result register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnRegisters {
    /// One result register per eightbyte, in ascending memory order.
    pub regs: Vec<ReturnSlotReg>,
    /// The compact image the value marshals through, or `None` when the
    /// value's own leaf vregs are its eightbytes.
    pub image: Option<NativeImage>,
}

impl ReturnRegisters {
    /// How many eightbytes cross.
    pub fn eightbytes(&self) -> usize {
        self.regs.len()
    }
}

/// The result registers a register-returned value of type `ty` occupies under
/// `pairing`, and the image it marshals through when its leaves are not its
/// eightbytes.
///
/// Both ends of a native return consult this, so a callee's stores and its
/// caller's reads cannot disagree about a bank or a byte offset.
pub fn return_registers(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    pairing: ConventionSpec,
) -> ReturnRegisters {
    let native = native_by_value_arg(type_pool, ty);
    let lowered = rue_air::lower_native_return(pairing, native.facts());
    let LoweredReturn::Registers { pieces, .. } = lowered else {
        panic!("only a register return names result registers");
    };
    let marshal = native.marshal_in_registers(pieces);
    assert_eq!(
        marshal.eightbyte_count(),
        pieces.len() as usize,
        "one eightbyte per result register the classification named"
    );
    let regs = pieces
        .as_slice()
        .iter()
        .enumerate()
        .map(|(index, piece)| match marshal.class(index, piece.class) {
            AbiSlotClass::Gp => ReturnSlotReg::Gp(piece.index as usize),
            AbiSlotClass::Fp(width) => ReturnSlotReg::Fp {
                index: piece.index as usize,
                width,
            },
        })
        .collect();
    let image = match (&marshal, native) {
        (NativeArgMarshal::Image { .. }, NativeArg::Aggregate { image }) => Some(image),
        _ => None,
    };
    ReturnRegisters { regs, image }
}

/// The hidden caller-provided return storage, when the return uses sret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HiddenSretPlan {
    /// The vreg containing the address of the caller-owned storage.
    pub pointer: VReg,
    /// Where the hidden pointer travels: an ordinary argument register — and
    /// then it is also the first entry of the plan's ABI slots — or the row's
    /// dedicated indirect-result register, which the backend writes itself.
    pub register: SretRegisterKind,
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
    /// A one-slot scalar is returned in the primary result register of its own
    /// bank.
    Scalar,
    /// A complete aggregate is returned in result registers, one per eightbyte
    /// ([`return_registers`]); `slot_count` is the value's own leaf count,
    /// which the two ends marshal to and from those eightbytes.
    Registers { slot_count: u32 },
    /// A complete aggregate is written to caller-provided storage whose address
    /// travels in the target row's own indirect-result register.
    Sret {
        slot_count: u32,
        storage_bytes: u32,
        /// Where the hidden pointer travels.
        register: SretRegisterKind,
        /// Whether the callee leaves the pointer in the primary result
        /// register (SysV AMD64's `rax` echo).
        echoed: bool,
    },
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

    /// Whether the hidden indirect-result pointer takes an ordinary argument
    /// register, shifting every user argument one general-purpose register
    /// right (SysV AMD64), rather than the row's dedicated one (AAPCS64 `x8`).
    pub const fn sret_in_argument_register(self) -> bool {
        matches!(
            self,
            Self::Sret {
                register: SretRegisterKind::ArgumentRegister,
                ..
            }
        )
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
    /// For a [`ReturnPlan::Registers`] return, the result register each
    /// eightbyte arrives in and the image it is read apart through (see
    /// [`return_registers`]). `None` for every other return plan, which has no
    /// register pieces to read back.
    pub return_registers: Option<ReturnRegisters>,
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
    /// For a register return, the result register each eightbyte arrives in and
    /// the image the value is read apart through. `None` for every other return.
    pub return_registers: Option<ReturnRegisters>,
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
        pairing: ConventionSpec,
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
        let return_plan = return_plan(type_pool, return_ty, pairing);
        // A register return names one result register per eightbyte, and the
        // image the value is read apart through when its leaves are not those
        // eightbytes. Both ends of the call read this one answer.
        let return_registers = matches!(return_plan, ReturnPlan::Registers { .. })
            .then(|| return_registers(type_pool, return_ty, pairing));
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
            return_registers,
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

/// The already-classified return an argument placement is computed against.
///
/// Only one fact about a result reaches argument placement: whether a hidden
/// indirect-result pointer takes an ordinary argument register ahead of every
/// user argument, which is the target row's own sret rule (ADR-0084). The
/// registers a result occupies never move an argument, so this projection is
/// placement-equivalent to the return's own lowering.
fn lowered_return(plan: ReturnPlan) -> LoweredReturn {
    match plan {
        ReturnPlan::ZeroSized => LoweredReturn::Void,
        ReturnPlan::Scalar | ReturnPlan::Registers { .. } => LoweredReturn::Registers {
            pieces: rue_air::RegisterPieces::one(CRegisterClass::Gp, 0),
            extension: rue_air::ScalarAbiExtension::None,
        },
        ReturnPlan::Sret {
            slot_count,
            storage_bytes: _,
            register,
            echoed,
        } => LoweredReturn::Sret {
            register,
            echoed,
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
    location: ArgLocation,
    materializer: &mut M,
    caller_indirect_bytes: &mut u32,
) -> (
    UserArgMode,
    Vec<VReg>,
    Vec<AbiSlotClass>,
    Vec<AbiSlotLocation>,
) {
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

/// One already-materialized leaf of a cleanup callee's flattened parameter
/// list: a register-width general-purpose scalar.
pub(crate) const fn register_width_leaf() -> NativeArg {
    NativeArg::Scalar {
        kind: rue_air::CAbiScalarKind::RegisterWidth,
        class: AbiSlotClass::Gp,
    }
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
        let pairing = callee_pairing(
            &target,
            materializer.native_convention(),
            materializer.target_c_convention(),
        );
        let callee_convention = pairing.convention();
        // One argument, one placement: the convention places every value as a
        // whole, so the lowered signature's arguments line up with these
        // one-for-one.
        let parameters = args
            .iter()
            .zip(natives)
            .map(|(arg, native)| (native.facts(), arg.arg_convention()))
            .collect::<Vec<_>>();
        let signature = lower_native_signature(pairing, &parameters, lowered_return(return_plan));

        let mut hidden_sret = None;
        let mut abi_slots = Vec::new();
        let mut abi_classes = Vec::new();
        let mut abi_locations = Vec::new();

        if let ReturnPlan::Sret {
            slot_count,
            storage_bytes,
            register,
            ..
        } = return_plan
        {
            assert_eq!(
                signature.sret_in_argument_register(),
                matches!(register, SretRegisterKind::ArgumentRegister),
                "the return's indirect-result rule and the placement it was \
                 computed against must be one rule"
            );
            let pointer = materializer.materialize_sret_pointer(storage_bytes);
            hidden_sret = Some(HiddenSretPlan {
                pointer,
                register,
                slot_count,
                storage_bytes,
            });
            // Under SysV AMD64 the pointer is the hidden first ordinary
            // argument and takes its register from the argument roster; under
            // AAPCS64 it takes the dedicated `x8`, which is outside the roster
            // and which the backend writes itself.
            if signature.sret_in_argument_register() {
                abi_slots.push(pointer);
                abi_classes.push(AbiSlotClass::Gp);
                abi_locations.push(AbiSlotLocation::GpReg(0));
            }
        }

        let mut user_args = Vec::with_capacity(args.len());
        let mut caller_indirect_bytes = 0u32;
        for ((arg, native), argument) in args.iter().zip(natives).zip(signature.arguments()) {
            let (mode, slots, classes, locations) = place_argument(
                arg,
                native,
                argument.location,
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
            return_registers: None,
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
    /// A cleanup callee's parameters *are* those leaves: the synthesized body
    /// addresses an owner's decomposition one slot at a time, so its signature
    /// is one register-width scalar per leaf. That signature goes through the
    /// same lowering every other call uses, and the callee's own parameter plan
    /// reads the same per-slot descriptors, so the two ends cannot disagree.
    pub fn from_slot_values(
        target: CallTarget,
        slots: &[VReg],
        native_convention: ConventionSpec,
        c_convention: CallingConvention,
    ) -> Self {
        let parameters = vec![(register_width_leaf().facts(), ArgConvention::ByValue); slots.len()];
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
            callee_convention: callee_pairing(&target, native_convention, c_convention)
                .convention(),
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
            return_registers: None,
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

/// The one shared return policy: where a native result of `ty` comes back
/// under `pairing`.
///
/// The decision is [`rue_air::lower_native_return`]'s against the same
/// [`CAbiTypeFacts`](rue_air::CAbiTypeFacts) an argument of that type presents,
/// so a value crosses in the same bank in both directions by construction
/// (ADR-0084). This function only projects that answer onto the codegen
/// [`ReturnPlan`], adding the leaf count both ends reconstruct and the
/// caller-storage byte size sret needs.
pub fn return_plan(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    pairing: ConventionSpec,
) -> ReturnPlan {
    let native = native_by_value_arg(type_pool, ty);
    let slot_count = type_pool.abi_slot_count(ty);
    match rue_air::lower_native_return(pairing, native.facts()) {
        LoweredReturn::Void => ReturnPlan::ZeroSized,
        LoweredReturn::Registers { .. } => {
            if matches!(native, NativeArg::Aggregate { .. }) {
                ReturnPlan::Registers { slot_count }
            } else {
                ReturnPlan::Scalar
            }
        }
        LoweredReturn::Sret {
            register, echoed, ..
        } => ReturnPlan::Sret {
            slot_count,
            storage_bytes: checked_aligned_cell_region_bytes(u64::from(slot_count))
                .expect("sret storage must pass frame-budget preflight"),
            register,
            echoed,
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
            CallingConvention::X86_64SysV,
        );

        assert_eq!(plan.abi_slots, slots);
        assert_eq!(plan.stack_slot_count, 3);
        assert_eq!(plan.stack_bytes, 32);
        assert_eq!(plan.return_plan, ReturnPlan::ZeroSized);
    }

    #[test]
    fn every_row_places_a_cleanup_leaf_in_a_whole_ascending_eightbyte() {
        // A cleanup callee (a destructor, a drop glue body) takes an
        // aggregate's leaves as one register-width parameter per
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
                storage_bytes: 32,
                register: SretRegisterKind::ArgumentRegister,
                echoed: true,
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
                register: SretRegisterKind::ArgumentRegister,
                echoed: true,
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
        assert_eq!(
            rue.hidden_sret.unwrap().register,
            SretRegisterKind::ArgumentRegister
        );
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
    fn a_result_is_classified_by_the_same_rule_and_bank_in_both_directions() {
        // A `{f64}` comes back in the floating-point bank it would cross in as
        // an argument, which is why the RUE-2010 disagreement cannot recur;
        // a `{i64, f64}` splits across the two banks; and `{i32, i32}` packs
        // into one result register through its image.
        let x86_64 = ConventionSpec::native(rue_target::Target::X86_64Linux);
        let (pool, one_float) = pool_with_struct(&[Type::F64]);
        let registers = return_registers(&pool, one_float, x86_64);
        assert!(registers.image.is_none(), "one leaf, one eightbyte");
        assert_eq!(
            registers.regs,
            vec![ReturnSlotReg::Fp {
                index: 0,
                width: crate::value_plan::FloatWidth::F64,
            }]
        );

        let (pool, split) = pool_with_struct(&[Type::I64, Type::F64]);
        let registers = return_registers(&pool, split, x86_64);
        assert!(registers.image.is_none());
        assert_eq!(
            registers.regs,
            vec![
                ReturnSlotReg::Gp(0),
                ReturnSlotReg::Fp {
                    index: 0,
                    width: crate::value_plan::FloatWidth::F64,
                },
            ]
        );

        let (pool, packed) = pool_with_struct(&[Type::I32, Type::I32]);
        assert_eq!(
            return_plan(&pool, packed, x86_64),
            ReturnPlan::Registers { slot_count: 2 },
            "two narrow leaves share one eightbyte and come back in one register"
        );
        let registers = return_registers(&pool, packed, x86_64);
        assert!(
            registers.image.is_some(),
            "leaves that pack together marshal through the compact image"
        );
        assert_eq!(registers.regs, vec![ReturnSlotReg::Gp(0)]);
    }

    #[test]
    fn a_result_past_the_bank_takes_its_rows_own_indirect_register() {
        // Nine eightbytes exceed both banks. The pointer travels where the C
        // row puts it: SysV's hidden first argument with the `rax` echo, and
        // AAPCS64's dedicated `x8` with none (ADR-0084).
        let (pool, wide) = pool_with_struct(&[Type::I64; 9]);
        assert_eq!(
            return_plan(
                &pool,
                wide,
                ConventionSpec::native(rue_target::Target::X86_64Linux)
            ),
            ReturnPlan::Sret {
                slot_count: 9,
                storage_bytes: 80,
                register: SretRegisterKind::ArgumentRegister,
                echoed: true,
            }
        );
        assert_eq!(
            return_plan(
                &pool,
                wide,
                ConventionSpec::native(rue_target::Target::Aarch64Linux)
            ),
            ReturnPlan::Sret {
                slot_count: 9,
                storage_bytes: 80,
                register: SretRegisterKind::DedicatedRegister,
                echoed: false,
            }
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
            callee_pairing(
                &target,
                ConventionSpec::native(rue_target::Target::Aarch64Macos),
                CallingConvention::Aarch64AapcsDarwin
            )
            .convention(),
            CallingConvention::Aarch64AapcsDarwin
        );
        assert_eq!(
            callee_pairing(
                &CallTarget::rue("f"),
                ConventionSpec::native(rue_target::Target::Aarch64Macos),
                CallingConvention::Aarch64AapcsDarwin
            )
            .convention(),
            CallingConvention::Rue
        );
        assert!(matches!(
            target,
            CallTarget::MemoryBuiltin(id) if id.export_id() == ReservedExportId::Memcpy
        ));
    }
}
