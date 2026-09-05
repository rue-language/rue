//! Lowering for `extern "C"` foreign calls (ADR-0064 P3).
//!
//! Every `extern "C"` call is planned here, whatever its argument and result
//! shapes, so one crossing has one plan: the target-C convention places the
//! call's values and each by-value aggregate is lowered through its **physical
//! memory image** (which, for a `@repr(c)` struct of scalar/pointer fields, is
//! exactly the C object layout under the compact-layout default).
//!
//! ## Where the classification lives
//!
//! This module does not classify or place anything. It projects each argument
//! and the return onto [`rue_air::CAbiTypeFacts`], asks
//! [`rue_air::lower_c_signature`] where every value goes, and then drives the
//! shared register/stack/sret event sequence against that answer. The same
//! function answers the export thunk (`crate::export_thunk`), which is what
//! makes an import and an export of one signature agree by construction rather
//! than by review. The two backends implement only the physical register,
//! stack, instruction, and image-operation leaves, so neither can grow a second
//! ABI sequence.

use rue_air::layout::PaddingRange;
use rue_air::{
    ArgConvention, ArgLocation, CAbiScalarKind, CAbiTypeFacts, FrozenTypeInternPool, LoweredReturn,
    LoweredSignature, PointerLocation, ScalarAbiExtension, Type, lower_c_signature,
};
use rue_cfg::{Cfg, CfgCallArg, CfgValue};
use rue_target::{CRegisterClass, CallingConvention};

use crate::frame_layout::{
    FrameBudgetExceeded, checked_aligned_region_bytes, checked_call_area_from_stack_bytes,
};
use crate::types::{self, PhysicalEnumSlot};
use crate::vreg::VReg;

/// The compact physical memory image of an aggregate crossing the C boundary —
/// its C object layout under the compact-layout default. Every by-value
/// aggregate crossing (register-packed, byval-stack, by-reference, and every
/// aggregate return) is marshaled through this image, so C field order is
/// honored by construction and the native reversed-slot packing is never used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateImage {
    /// Internal-slot → physical-byte map (the compact/C image). Writing an
    /// aggregate's native slots through this map lays out the C image; reading it
    /// back reconstructs the native slots.
    pub map: Vec<PhysicalEnumSlot>,
    /// Padding byte ranges zeroed before the field stores so the image buffer is
    /// deterministically initialized (ADR-0052 ruling 5).
    pub padding: Vec<PaddingRange>,
    /// Number of internal value-decomposition slots (native slot count).
    pub slot_count: u32,
    /// The aggregate's byte size (`@size_of`).
    pub size: u32,
    /// The aggregate's alignment (`@align_of`).
    pub align: u32,
    /// Backing-buffer size: `size` rounded up to the 16-byte call-stack
    /// alignment, so whole-eightbyte loads/stores never run past the buffer.
    pub storage_bytes: u32,
    /// The scalar leaves of the image, which is what classifies its eightbytes
    /// and answers the homogeneous-float rule.
    pub leaves: rue_air::AggregateLeaves,
}

impl AggregateImage {
    fn for_type(type_pool: &FrozenTypeInternPool, ty: Type) -> Self {
        let map = types::aggregate_physical_slot_map(type_pool, ty).expect(
            "an FFI-eligible @repr(c) aggregate (no enums, variant-independent) must have a \
             single compact memory image; c_passable_by_value gated this before lowering",
        );
        let layout = type_pool.layout(ty);
        AggregateImage {
            map,
            padding: type_pool.compact_image_padding_ranges(ty),
            slot_count: types::type_slot_count(type_pool, ty),
            size: u32::try_from(layout.size).expect("foreign aggregate size must fit u32"),
            align: u32::try_from(layout.alignment)
                .expect("foreign aggregate alignment must fit u32"),
            storage_bytes: checked_aligned_region_bytes(layout.size)
                .expect("foreign aggregate storage must pass frame-budget preflight"),
            leaves: rue_air::aggregate_leaves(type_pool, ty, layout.size),
        }
    }

    /// Number of eightbytes (8-byte integer registers / stack slots) the image
    /// spans: `ceil(size / 8)`.
    pub fn eightbytes(&self) -> u32 {
        self.size.div_ceil(8)
    }

    /// The classification facts this image presents at the C boundary.
    pub fn facts(&self) -> CAbiTypeFacts {
        CAbiTypeFacts::Aggregate {
            size: u64::from(self.size),
            align: u64::from(self.align),
            leaves: self.leaves,
        }
    }
}

/// One value crossing into a foreign call. *Where* it crosses is the lowered
/// signature's answer, not this enum's: the two variants distinguish only how
/// the backend materializes the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignArg {
    /// A scalar or pointer, already canonically extended in its vreg by Rue's
    /// internal invariant, so it needs no boundary instruction on the way out.
    Scalar {
        /// The CFG value carrying it.
        value: CfgValue,
        /// Its width-and-signedness class, which fixes the footprint a stacked
        /// copy takes under Apple's natural-size packing.
        kind: CAbiScalarKind,
    },
    /// A C-classifiable aggregate, marshaled through its compact memory image.
    Aggregate {
        /// The CFG value carrying it.
        value: CfgValue,
        /// Its compact memory image.
        image: AggregateImage,
    },
}

impl ForeignArg {
    /// The classification facts this argument presents at the C boundary.
    pub fn facts(&self) -> CAbiTypeFacts {
        match self {
            Self::Scalar { kind, .. } => CAbiTypeFacts::Scalar {
                kind: *kind,
                class: kind.register_class(),
            },
            Self::Aggregate { image, .. } => image.facts(),
        }
    }

    /// The CFG value this argument reads.
    pub fn value(&self) -> CfgValue {
        match self {
            Self::Scalar { value, .. } | Self::Aggregate { value, .. } => *value,
        }
    }

    /// Whether a stacked copy of this argument is packed as a scalar (Apple's
    /// natural-size amendment) rather than as whole eightbytes.
    fn packs_as_scalar(&self) -> bool {
        matches!(self, Self::Scalar { .. })
    }
}

/// How a foreign call's return value crosses. *Where* it crosses — result
/// registers or caller storage — is the lowered signature's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignReturn {
    /// Unit / never / empty: no materialized value.
    ZeroSized,
    /// A scalar; the lowered signature names the re-extension that restores
    /// Rue's canonical 64-bit form (a C callee leaves the high bits
    /// unspecified).
    Scalar,
    /// An aggregate, reconstructed through its compact memory image.
    Aggregate {
        /// Its compact memory image.
        image: AggregateImage,
    },
}

impl ForeignReturn {
    /// The classification facts this return presents at the C boundary.
    fn facts(&self, scalar: CAbiTypeFacts) -> CAbiTypeFacts {
        match self {
            Self::ZeroSized => CAbiTypeFacts::ZeroSized,
            Self::Scalar => scalar,
            Self::Aggregate { image } => image.facts(),
        }
    }
}

/// A classified foreign call: the C symbol, every argument and the return, and
/// the lowered signature that places them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignCallInputs {
    /// The resolved (unmangled) C symbol the undefined reference targets.
    symbol: String,
    args: Vec<ForeignArg>,
    ret: ForeignReturn,
    signature: LoweredSignature,
}

impl ForeignCallInputs {
    /// Classify and place a foreign call to `symbol` under `convention`.
    ///
    /// `return_facts` describes the return type; for a scalar return it carries
    /// the scalar kind whose extension the caller applies on the way back.
    pub fn new(
        symbol: String,
        convention: CallingConvention,
        args: Vec<ForeignArg>,
        ret: ForeignReturn,
        return_facts: CAbiTypeFacts,
    ) -> Self {
        let parameters = args
            .iter()
            .map(|arg| (arg.facts(), ArgConvention::ByValue))
            .collect::<Vec<_>>();
        let signature = lower_c_signature(convention, &parameters, ret.facts(return_facts));
        Self {
            symbol,
            args,
            ret,
            signature,
        }
    }

    /// The C symbol this call targets.
    pub fn symbol_ref(&self) -> &str {
        &self.symbol
    }

    /// The convention the compilation target's `"C"` alias resolves to. Always
    /// a C row; the native Rue convention never reaches this path.
    pub fn convention(&self) -> CallingConvention {
        self.signature.convention()
    }

    /// The lowered signature that places every argument and the return.
    pub fn signature(&self) -> &LoweredSignature {
        &self.signature
    }
}

/// One store into the outgoing argument area, in ascending-offset order.
///
/// `bytes` is the number of low bytes of `value` the store must commit: 1, 2, 4,
/// or 8. Under the 8-byte-slot packing it is always 8. Under Apple's
/// natural-size packing a stacked *scalar* names its own C width, so an `i8`
/// writes one byte at the next free byte and an `i16` starts at the next even
/// offset.
///
/// `offset` is always a multiple of `bytes`, which is what makes every store
/// encodable in AArch64's scaled `imm12` addressing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForeignStackStore {
    pub(crate) value: VReg,
    pub(crate) offset: u32,
    pub(crate) bytes: u32,
}

/// The complete outgoing argument area: what to store where, and how many bytes
/// the caller reserves (already rounded to the psABI's call alignment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignStackArea {
    pub(crate) stores: Vec<ForeignStackStore>,
    pub(crate) bytes: u32,
}

/// A foreign call ready to lower: the classified values plus the signature that
/// places them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignCallPlan {
    inputs: ForeignCallInputs,
}

impl ForeignCallPlan {
    pub(crate) fn new(inputs: ForeignCallInputs) -> Self {
        Self { inputs }
    }

    fn signature(&self) -> &LoweredSignature {
        self.inputs.signature()
    }

    /// Reserved size of the outgoing argument area, call-aligned.
    fn stack_area_bytes(&self) -> u32 {
        self.signature().stack_bytes()
    }

    /// Total storage reserved for by-reference argument copies, each rounded to
    /// the frame's 16-byte allocation granule.
    fn byref_bytes(&self) -> u32 {
        self.signature()
            .indirect_copy_sizes()
            .map(|(size, _)| {
                checked_aligned_region_bytes(u64::from(size))
                    .expect("foreign argument storage must pass frame-budget preflight")
            })
            .fold(0u32, |total, bytes| {
                total
                    .checked_add(bytes)
                    .expect("foreign argument storage must fit u32")
            })
    }

    /// The stores one stacked argument makes: its eightbyte (or narrower)
    /// pieces at ascending offsets from the argument's own packed offset.
    fn stack_stores(&self, arg_index: usize, values: &[VReg]) -> Vec<ForeignStackStore> {
        let ArgLocation::Stack { offset, size, .. } =
            self.signature().arguments()[arg_index].location
        else {
            panic!("stack stores are only emitted for a stacked argument");
        };
        let bytes = if self.inputs.args[arg_index].packs_as_scalar() {
            size
        } else {
            8
        };
        values
            .iter()
            .enumerate()
            .map(|(piece, value)| ForeignStackStore {
                value: *value,
                offset: offset
                    + u32::try_from(piece * 8).expect("foreign stack offset must fit u32"),
                bytes,
            })
            .collect()
    }
}

#[cfg(test)]
impl ForeignCallPlan {
    /// The outgoing argument area this plan produces, taking one vreg per
    /// stacked piece in placement order. Backend encoding tests drive
    /// [`ForeignCallLoweringBackend::foreign_emit_stack_args`] with it instead
    /// of standing up a whole lowering fixture.
    pub(crate) fn stack_area_for_test(&self, values: &[VReg]) -> ForeignStackArea {
        let mut values = values.iter().copied();
        let mut stores = Vec::new();
        for (arg_index, argument) in self.signature().arguments().iter().enumerate() {
            let ArgLocation::Stack { size, .. } = argument.location else {
                continue;
            };
            let pieces = if self.inputs.args[arg_index].packs_as_scalar() {
                1
            } else {
                (size / 8) as usize
            };
            let taken = (&mut values).take(pieces).collect::<Vec<_>>();
            assert_eq!(taken.len(), pieces, "one vreg per stacked piece");
            stores.extend(self.stack_stores(arg_index, &taken));
        }
        assert!(
            values.next().is_none(),
            "every vreg must name a stacked piece"
        );
        ForeignStackArea {
            stores,
            bytes: self.stack_area_bytes(),
        }
    }
}

/// One value crossing in a register: which bank, which roster index within it,
/// and the vreg holding the value.
///
/// The roster index is the lowered signature's, not the position of this entry:
/// a value's register is decided by the classification, and a backend maps the
/// pair to a physical register rather than counting the values it has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForeignRegisterArg {
    /// The register bank.
    pub(crate) class: CRegisterClass,
    /// Roster index within that bank's argument registers.
    pub(crate) index: u32,
    /// The vreg holding the value.
    pub(crate) value: VReg,
}

/// Target-specific leaves for the shared foreign-call lowering driver.
///
/// The driver below owns the call's event order and all target-independent
/// decisions. Implementations only select concrete registers/instructions and
/// perform the target's stack and image operations.
pub(crate) trait ForeignCallLoweringBackend {
    /// The convention this backend's compilation target resolves `"C"` to.
    fn target_c_convention(&self) -> CallingConvention;
    fn foreign_int_arg_register_count(&self) -> usize;
    fn foreign_reserve_sret(&mut self, image: &AggregateImage) -> VReg;
    fn foreign_get_vreg(&mut self, value: CfgValue) -> VReg;
    fn foreign_image_arg_eightbytes(
        &mut self,
        value: CfgValue,
        image: &AggregateImage,
    ) -> Vec<VReg>;
    fn foreign_byref_copy(&mut self, value: CfgValue, image: &AggregateImage) -> VReg;
    /// Reserve the outgoing argument area and commit every store in it. The
    /// area's size and each store's offset and width are the convention's
    /// decision, made once by the lowered signature; the backend chooses only
    /// the instructions.
    fn foreign_emit_stack_args(&mut self, stack: &ForeignStackArea);
    /// Move every register-passed value into the register the classification
    /// named for it.
    fn foreign_emit_register_args(&mut self, register_args: &[ForeignRegisterArg]);
    fn foreign_assign_sret(&mut self, sret_ptr: VReg);
    fn foreign_issue_call(&mut self, symbol: &str);
    /// Release the outgoing argument area reserved by
    /// [`foreign_emit_stack_args`](Self::foreign_emit_stack_args).
    fn foreign_cleanup_stack(&mut self, stack: &ForeignStackArea);
    fn foreign_cleanup_byref(&mut self, byref_bytes: u32);
    fn foreign_zero_result(&mut self, primary: VReg);
    fn foreign_scalar_result(&mut self, primary: VReg, ext: ScalarAbiExtension);
    fn foreign_register_result(&mut self, primary: VReg, image: &AggregateImage) -> Vec<VReg>;
    fn foreign_sret_result(
        &mut self,
        primary: VReg,
        image: &AggregateImage,
        sret_ptr: VReg,
    ) -> Vec<VReg>;
    fn foreign_move_primary(&mut self, primary: VReg, slot: VReg);
}

/// Lower one foreign call through the single shared event sequence. Concrete
/// backends provide only instruction-selection leaves via
/// [`ForeignCallLoweringBackend`].
pub(crate) fn lower_foreign_call<B: ForeignCallLoweringBackend>(
    backend: &mut B,
    inputs: ForeignCallInputs,
    primary: VReg,
) -> crate::value_plan::MaterializedValue {
    assert_eq!(
        inputs.convention(),
        backend.target_c_convention(),
        "foreign call convention disagrees with the selected backend"
    );
    assert_eq!(
        usize::try_from(inputs.signature().spec().gp_argument_registers)
            .expect("target-C integer argument budget must fit usize"),
        backend.foreign_int_arg_register_count(),
        "backend argument-register roster disagrees with target-C ABI"
    );
    let plan = ForeignCallPlan::new(inputs);
    let sret_in_argument_register = plan.signature().sret_in_argument_register();
    let stack_area_bytes = plan.stack_area_bytes();
    let byref_bytes = plan.byref_bytes();
    let return_uses_sret = plan.signature().ret().uses_sret();
    let inputs = &plan.inputs;

    // Establish indirect-result storage first; it remains live through the
    // call and is released only after its native slots have been reconstructed.
    let sret_ptr = match (&inputs.ret, return_uses_sret) {
        (ForeignReturn::Aggregate { image }, true) => Some(backend.foreign_reserve_sret(image)),
        _ => None,
    };

    let mut register_args: Vec<ForeignRegisterArg> = Vec::new();
    let mut stack = ForeignStackArea {
        stores: Vec::new(),
        bytes: stack_area_bytes,
    };
    if sret_in_argument_register {
        // The hidden indirect-result pointer is the hidden first argument, so
        // it takes the first general-purpose argument register and the
        // classification already shifted every user argument past it.
        register_args.push(ForeignRegisterArg {
            class: CRegisterClass::Gp,
            index: 0,
            value: sret_ptr.expect("SysV sret placement requires storage"),
        });
    }
    for (arg_index, (arg, argument)) in inputs
        .args
        .iter()
        .zip(plan.signature().arguments())
        .enumerate()
    {
        let values = match (arg, argument.location) {
            (_, ArgLocation::Omitted) => continue,
            (ForeignArg::Scalar { value, .. }, _) => vec![backend.foreign_get_vreg(*value)],
            (ForeignArg::Aggregate { value, image }, ArgLocation::Indirect { .. }) => {
                vec![backend.foreign_byref_copy(*value, image)]
            }
            (ForeignArg::Aggregate { value, image }, _) => {
                backend.foreign_image_arg_eightbytes(*value, image)
            }
        };
        match argument.location {
            ArgLocation::Registers { pieces } => {
                assert_eq!(
                    pieces.len() as usize,
                    values.len(),
                    "one value per register the classification named"
                );
                register_args.extend(pieces.as_slice().iter().zip(&values).map(
                    |(piece, value)| ForeignRegisterArg {
                        class: piece.class,
                        index: piece.index,
                        value: *value,
                    },
                ));
            }
            ArgLocation::Indirect {
                pointer: PointerLocation::Register { index },
                ..
            } => register_args.push(ForeignRegisterArg {
                class: CRegisterClass::Gp,
                index,
                value: values[0],
            }),
            ArgLocation::Stack { .. } => stack.stores.extend(plan.stack_stores(arg_index, &values)),
            ArgLocation::Indirect {
                pointer: PointerLocation::Stack { offset },
                ..
            } => stack.stores.push(ForeignStackStore {
                value: values[0],
                offset,
                bytes: 8,
            }),
            ArgLocation::Omitted => unreachable!("an omitted argument was skipped above"),
        }
    }

    // The adapters own concrete stack layout and register moves, but the
    // driver fixes the shared boundary order: outgoing stack, integer args,
    // hidden sret assignment, call, then stack/byref cleanup.
    backend.foreign_emit_stack_args(&stack);
    backend.foreign_emit_register_args(&register_args);
    if let Some(sret_ptr) = sret_ptr {
        if !sret_in_argument_register {
            backend.foreign_assign_sret(sret_ptr);
        }
    }
    backend.foreign_issue_call(inputs.symbol_ref());
    backend.foreign_cleanup_stack(&stack);
    backend.foreign_cleanup_byref(byref_bytes);

    let slots = match (&inputs.ret, plan.signature().ret()) {
        (ForeignReturn::ZeroSized, _) | (_, LoweredReturn::Void) => {
            backend.foreign_zero_result(primary);
            Vec::new()
        }
        (ForeignReturn::Scalar, LoweredReturn::Registers { extension, .. }) => {
            backend.foreign_scalar_result(primary, extension);
            Vec::new()
        }
        (ForeignReturn::Aggregate { image }, LoweredReturn::Registers { .. }) => {
            backend.foreign_register_result(primary, image)
        }
        (ForeignReturn::Aggregate { image }, LoweredReturn::Sret { .. }) => backend
            .foreign_sret_result(
                primary,
                image,
                sret_ptr.expect("sret return requires storage"),
            ),
        (ForeignReturn::Scalar, LoweredReturn::Sret { .. }) => {
            unreachable!("a scalar return never crosses through caller storage")
        }
    };
    if let Some(&slot) = slots.first() {
        backend.foreign_move_primary(primary, slot);
    }
    crate::value_plan::MaterializedValue { primary, slots }
}

impl ForeignCallInputs {
    /// Compute the simultaneous transient area of a foreign call from the same
    /// lowered signature the lowerers consume. This accounts for hidden sret
    /// storage, byval stack cells, caller-owned by-reference copies, and
    /// argument-register exhaustion that spills pointers and eightbytes to the
    /// outgoing stack area.
    pub(crate) fn checked_call_area_bytes(
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        return_ty: Type,
        args: &[CfgCallArg],
        convention: CallingConvention,
    ) -> Result<u32, FrameBudgetExceeded> {
        let parameters = args
            .iter()
            .map(|arg| {
                (
                    rue_air::c_abi_type_facts(type_pool, cfg.get_inst(arg.value).ty),
                    ArgConvention::ByValue,
                )
            })
            .collect::<Vec<_>>();
        let signature = lower_c_signature(
            convention,
            &parameters,
            rue_air::c_abi_type_facts(type_pool, return_ty),
        );

        let sret_storage_bytes = match signature.ret() {
            LoweredReturn::Sret { size, .. } => {
                u64::from(checked_aligned_region_bytes(u64::from(size))?)
            }
            _ => 0,
        };
        let mut indirect_bytes = 0_u64;
        for (arg, argument) in args.iter().zip(signature.arguments()) {
            let ty = cfg.get_inst(arg.value).ty;
            if !is_aggregate(ty) {
                continue;
            }
            let layout = type_pool.layout(ty);
            match argument.location {
                ArgLocation::Indirect { size, .. } => {
                    indirect_bytes = indirect_bytes
                        .checked_add(u64::from(checked_aligned_region_bytes(u64::from(size))?))
                        .ok_or(FrameBudgetExceeded)?;
                }
                // A register-packed or byval-stacked aggregate is marshaled
                // through a temporary image buffer. It is short-lived, but can
                // overlap the hidden sret area and any earlier caller-owned
                // copies.
                ArgLocation::Registers { .. } | ArgLocation::Stack { .. } => {
                    let scratch = checked_aligned_region_bytes(layout.size)?;
                    checked_call_area_from_stack_bytes(
                        0,
                        sret_storage_bytes,
                        indirect_bytes
                            .checked_add(u64::from(scratch))
                            .ok_or(FrameBudgetExceeded)?,
                    )?;
                }
                ArgLocation::Omitted => {}
            }
        }

        if is_aggregate(return_ty) && !signature.ret().uses_sret() {
            // Return-image scratch is allocated after outgoing stack and
            // by-reference areas are released, so it is a separate peak from
            // the call-boundary reservation.
            let scratch = checked_aligned_region_bytes(type_pool.layout(return_ty).size)?;
            checked_call_area_from_stack_bytes(0, 0, u64::from(scratch))?;
        }
        checked_call_area_from_stack_bytes(
            u64::from(signature.stack_bytes()),
            sret_storage_bytes,
            indirect_bytes,
        )
    }

    /// Classify a foreign call through the shared target-C authority.
    pub(crate) fn from_cfg(
        symbol: String,
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        return_ty: Type,
        args: &[CfgCallArg],
        convention: CallingConvention,
    ) -> Self {
        let planned_args = args
            .iter()
            .map(|arg| {
                let value = arg.value;
                let ty = cfg.get_inst(value).ty;
                assert!(
                    matches!(arg.mode, rue_cfg::CfgArgMode::Normal),
                    "an `extern \"C\"` argument is always by value; inout/borrow do not cross a \
                     C boundary"
                );
                if is_aggregate(ty) {
                    ForeignArg::Aggregate {
                        value,
                        image: AggregateImage::for_type(type_pool, ty),
                    }
                } else {
                    ForeignArg::Scalar {
                        value,
                        kind: live_scalar_kind(ty),
                    }
                }
            })
            .collect();

        let return_facts = rue_air::c_abi_type_facts(type_pool, return_ty);
        let ret = if is_aggregate(return_ty) {
            ForeignReturn::Aggregate {
                image: AggregateImage::for_type(type_pool, return_ty),
            }
        } else if matches!(return_facts, CAbiTypeFacts::ZeroSized) {
            ForeignReturn::ZeroSized
        } else {
            ForeignReturn::Scalar
        };

        Self::new(symbol, convention, planned_args, ret, return_facts)
    }
}

/// The live plane's projection of a target-C-passable scalar onto its
/// width-and-signedness class.
fn live_scalar_kind(ty: Type) -> CAbiScalarKind {
    CAbiScalarKind::for_live_type(ty).unwrap_or_else(|| {
        panic!(
            "target-C classification called on unsupported type {:?}; \
             c_passable_by_value gates the boundary before lowering",
            ty.kind()
        )
    })
}

/// Whether `ty` is an aggregate (struct or fixed array) — the types whose
/// target-C crossing needs the image-based foreign path.
fn is_aggregate(ty: Type) -> bool {
    matches!(
        ty.kind(),
        rue_air::TypeKind::Struct(_) | rue_air::TypeKind::Array(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_cfg::CfgValue;

    fn image(size: u32) -> AggregateImage {
        image_aligned(size, 8)
    }

    fn image_aligned(size: u32, align: u32) -> AggregateImage {
        AggregateImage {
            map: Vec::new(),
            padding: Vec::new(),
            slot_count: 0,
            size,
            align,
            storage_bytes: size.div_ceil(16) * 16,
            leaves: rue_air::AggregateLeaves::all_integer(u64::from(size)),
        }
    }

    fn scalar(index: u32) -> ForeignArg {
        narrow_scalar(index, CAbiScalarKind::RegisterWidth)
    }

    fn narrow_scalar(index: u32, kind: CAbiScalarKind) -> ForeignArg {
        ForeignArg::Scalar {
            value: CfgValue::from_raw(index),
            kind,
        }
    }

    fn aggregate(index: u32, image: AggregateImage) -> ForeignArg {
        ForeignArg::Aggregate {
            value: CfgValue::from_raw(index),
            image,
        }
    }

    fn word_return() -> CAbiTypeFacts {
        CAbiTypeFacts::ZeroSized
    }

    fn plan(
        convention: CallingConvention,
        args: Vec<ForeignArg>,
        ret: ForeignReturn,
    ) -> ForeignCallPlan {
        let return_facts = match &ret {
            ForeignReturn::Aggregate { image } => image.facts(),
            _ => word_return(),
        };
        ForeignCallPlan::new(ForeignCallInputs::new(
            "f".into(),
            convention,
            args,
            ret,
            return_facts,
        ))
    }

    fn stack_offsets(plan: &ForeignCallPlan) -> Vec<u32> {
        plan.signature()
            .arguments()
            .iter()
            .filter_map(|argument| match argument.location {
                ArgLocation::Stack { offset, .. } => Some(offset),
                ArgLocation::Indirect {
                    pointer: PointerLocation::Stack { offset },
                    ..
                } => Some(offset),
                _ => None,
            })
            .collect()
    }

    fn is_register(plan: &ForeignCallPlan, index: usize) -> bool {
        matches!(
            plan.signature().arguments()[index].location,
            ArgLocation::Registers { .. }
                | ArgLocation::Indirect {
                    pointer: PointerLocation::Register { .. },
                    ..
                }
        )
    }

    #[test]
    fn sysv_hidden_sret_and_aggregate_placement_are_shared_decisions() {
        let plan = plan(
            CallingConvention::X86_64SysV,
            vec![
                scalar(0),
                aggregate(1, image(16)),
                scalar(2),
                scalar(3),
                scalar(4),
            ],
            ForeignReturn::Aggregate { image: image(24) },
        );

        assert!(plan.signature().sret_in_argument_register());
        assert_eq!(plan.signature().arguments().len(), 5);
        for index in 0..4 {
            assert!(
                is_register(&plan, index),
                "argument {index} takes registers"
            );
        }
        assert_eq!(stack_offsets(&plan), vec![0]);
    }

    #[test]
    fn aapcs_byref_storage_does_not_consume_a_register_per_byte() {
        let plan = plan(
            CallingConvention::Aarch64Aapcs,
            vec![aggregate(0, image(24))],
            ForeignReturn::ZeroSized,
        );

        assert!(!plan.signature().sret_in_argument_register());
        assert_eq!(plan.byref_bytes(), 32);
        assert!(is_register(&plan, 0));
    }

    /// The byval tail of a call whose arguments overflow the eight AAPCS64
    /// integer registers. Every argument after the eighth is stacked, so the two
    /// AAPCS rows can be compared on identical inputs.
    fn narrow_tail_args() -> Vec<ForeignArg> {
        let mut args = vec![aggregate(0, image(8))];
        // Seven register-width scalars fill x1..x7 behind the aggregate in x0.
        args.extend((1..8).map(scalar));
        // The stacked tail: i8, i16, i32, i64.
        args.push(narrow_scalar(8, CAbiScalarKind::I8));
        args.push(narrow_scalar(9, CAbiScalarKind::I16));
        args.push(narrow_scalar(10, CAbiScalarKind::I32));
        args.push(narrow_scalar(11, CAbiScalarKind::RegisterWidth));
        args
    }

    fn narrow_tail_plan(convention: CallingConvention) -> ForeignCallPlan {
        plan(convention, narrow_tail_args(), ForeignReturn::ZeroSized)
    }

    #[test]
    fn a_narrow_scalar_tail_takes_whole_slots_under_aapcs_and_packs_under_darwin() {
        let aapcs = narrow_tail_plan(CallingConvention::Aarch64Aapcs);
        // AAPCS64 gives every stacked argument its own 8-byte slot.
        assert_eq!(stack_offsets(&aapcs), vec![0, 8, 16, 24]);
        assert_eq!(aapcs.stack_area_bytes(), 32);

        let darwin = narrow_tail_plan(CallingConvention::Aarch64AapcsDarwin);
        // Apple's amendment packs each argument at its natural size and
        // alignment: i8 at 0, i16 at 2 (aligned), i32 at 4, i64 at 8.
        assert_eq!(stack_offsets(&darwin), vec![0, 2, 4, 8]);
        // 16 packed bytes, already call-aligned.
        assert_eq!(darwin.stack_area_bytes(), 16);
    }

    #[test]
    fn only_the_darwin_row_narrows_a_stacked_scalar_store() {
        let plan = narrow_tail_plan(CallingConvention::Aarch64Aapcs);
        let stores = plan.stack_stores(8, &[VReg::new(1)]);
        assert_eq!(stores[0].bytes, 8, "AAPCS64 writes the whole 8-byte slot");

        let plan = narrow_tail_plan(CallingConvention::Aarch64AapcsDarwin);
        assert_eq!(plan.stack_stores(8, &[VReg::new(1)])[0].bytes, 1);
        assert_eq!(plan.stack_stores(9, &[VReg::new(1)])[0].bytes, 2);
        assert_eq!(plan.stack_stores(10, &[VReg::new(1)])[0].bytes, 4);
        assert_eq!(plan.stack_stores(11, &[VReg::new(1)])[0].bytes, 8);
    }

    #[test]
    fn x86_64_is_unaffected_by_the_apple_amendment() {
        // The same narrow tail on SysV: six integer registers, then whole
        // 8-byte slots for every stacked argument regardless of its C width.
        let mut args = vec![aggregate(0, image(8))];
        args.extend((1..6).map(scalar));
        args.push(narrow_scalar(6, CAbiScalarKind::I8));
        args.push(narrow_scalar(7, CAbiScalarKind::I16));
        args.push(narrow_scalar(8, CAbiScalarKind::I32));
        let plan = plan(
            CallingConvention::X86_64SysV,
            args,
            ForeignReturn::ZeroSized,
        );
        assert_eq!(stack_offsets(&plan), vec![0, 8, 16]);
        assert_eq!(plan.stack_area_bytes(), 32);
        for arg_index in 6..9 {
            assert_eq!(plan.stack_stores(arg_index, &[VReg::new(1)])[0].bytes, 8);
        }
    }

    #[test]
    fn a_stacked_composite_keeps_whole_eightbytes_under_every_row() {
        // A stacked byte followed by a 12-byte, 4-aligned composite: the byte
        // packs to one byte under Apple's rule, and the composite still takes
        // whole eightbytes at 8-byte alignment, so it starts at 8 and the
        // following scalar at 24. Every store offset stays a multiple of its
        // width, which is what keeps the AArch64 encodings in range.
        let args = (0..8)
            .map(scalar)
            .chain([
                narrow_scalar(8, CAbiScalarKind::I8),
                aggregate(9, image_aligned(12, 4)),
                narrow_scalar(10, CAbiScalarKind::I32),
            ])
            .collect();
        let plan = plan(
            CallingConvention::Aarch64AapcsDarwin,
            args,
            ForeignReturn::ZeroSized,
        );
        assert_eq!(stack_offsets(&plan), vec![0, 8, 24]);
        assert_eq!(plan.stack_area_bytes(), 32);
        let stores = plan.stack_stores(9, &[VReg::new(1), VReg::new(2)]);
        assert_eq!(stores.len(), 2);
        assert_eq!((stores[0].offset, stores[0].bytes), (8, 8));
        assert_eq!((stores[1].offset, stores[1].bytes), (16, 8));
    }

    #[test]
    fn every_stacked_store_offset_is_a_multiple_of_its_width() {
        // AArch64 addresses a stacked store through the scaled `imm12` form, so
        // an unaligned offset would be unencodable. The packing rule keeps the
        // invariant for both AAPCS rows.
        for convention in [
            CallingConvention::Aarch64Aapcs,
            CallingConvention::Aarch64AapcsDarwin,
        ] {
            let plan = narrow_tail_plan(convention);
            for (arg_index, argument) in plan.signature().arguments().iter().enumerate() {
                if !matches!(argument.location, ArgLocation::Stack { .. }) {
                    continue;
                }
                for store in plan.stack_stores(arg_index, &[VReg::new(1)]) {
                    assert_eq!(
                        store.offset % store.bytes,
                        0,
                        "{convention}: store at {} is not {}-aligned",
                        store.offset,
                        store.bytes
                    );
                }
            }
        }
    }

    #[derive(Debug)]
    struct TraceBackend {
        convention: CallingConvention,
        register_count: usize,
        next_vreg: u32,
        events: Vec<String>,
    }

    impl TraceBackend {
        fn new(convention: CallingConvention, register_count: usize) -> Self {
            Self {
                convention,
                register_count,
                next_vreg: 100,
                events: Vec::new(),
            }
        }

        fn vreg(&mut self) -> VReg {
            let vreg = VReg::new(self.next_vreg);
            self.next_vreg += 1;
            vreg
        }

        fn record(&mut self, event: impl Into<String>) {
            self.events.push(event.into());
        }
    }

    impl ForeignCallLoweringBackend for TraceBackend {
        fn target_c_convention(&self) -> CallingConvention {
            self.convention
        }

        fn foreign_int_arg_register_count(&self) -> usize {
            self.register_count
        }

        fn foreign_reserve_sret(&mut self, _image: &AggregateImage) -> VReg {
            self.record("reserve_sret");
            self.vreg()
        }

        fn foreign_get_vreg(&mut self, value: CfgValue) -> VReg {
            self.record(format!("get:{value:?}"));
            self.vreg()
        }

        fn foreign_image_arg_eightbytes(
            &mut self,
            _value: CfgValue,
            image: &AggregateImage,
        ) -> Vec<VReg> {
            self.record(format!("image:{}", image.eightbytes()));
            (0..image.eightbytes()).map(|_| self.vreg()).collect()
        }

        fn foreign_byref_copy(&mut self, _value: CfgValue, image: &AggregateImage) -> VReg {
            self.record(format!("byref:{}", image.storage_bytes));
            self.vreg()
        }

        fn foreign_emit_stack_args(&mut self, stack: &ForeignStackArea) {
            let stores = stack
                .stores
                .iter()
                .map(|store| format!("{}@{}+{}", store.value, store.offset, store.bytes))
                .collect::<Vec<_>>();
            self.record(format!("stack[{}]:{}", stack.bytes, stores.join(",")));
        }

        fn foreign_emit_register_args(&mut self, register_args: &[ForeignRegisterArg]) {
            let moves = register_args
                .iter()
                .map(|arg| format!("{:?}{}={}", arg.class, arg.index, arg.value))
                .collect::<Vec<_>>();
            self.record(format!("registers:{}", moves.join(",")));
        }

        fn foreign_assign_sret(&mut self, _sret_ptr: VReg) {
            self.record("assign_sret");
        }

        fn foreign_issue_call(&mut self, symbol: &str) {
            self.record(format!("call:{symbol}"));
        }

        fn foreign_cleanup_stack(&mut self, stack: &ForeignStackArea) {
            self.record(format!("cleanup_stack:{}", stack.bytes));
        }

        fn foreign_cleanup_byref(&mut self, byref_bytes: u32) {
            self.record(format!("cleanup_byref:{byref_bytes}"));
        }

        fn foreign_zero_result(&mut self, _primary: VReg) {
            self.record("zero_result");
        }

        fn foreign_scalar_result(&mut self, _primary: VReg, _ext: ScalarAbiExtension) {
            self.record("scalar_result");
        }

        fn foreign_register_result(&mut self, _primary: VReg, image: &AggregateImage) -> Vec<VReg> {
            self.record(format!("register_result:{}", image.eightbytes()));
            vec![self.vreg()]
        }

        fn foreign_sret_result(
            &mut self,
            _primary: VReg,
            image: &AggregateImage,
            _sret_ptr: VReg,
        ) -> Vec<VReg> {
            self.record(format!("sret_result:{}", image.eightbytes()));
            vec![self.vreg()]
        }

        fn foreign_move_primary(&mut self, _primary: VReg, _slot: VReg) {
            self.record("move_primary");
        }
    }

    fn inputs(
        symbol: &str,
        convention: CallingConvention,
        args: Vec<ForeignArg>,
        ret: ForeignReturn,
    ) -> ForeignCallInputs {
        let return_facts = match &ret {
            ForeignReturn::Aggregate { image } => image.facts(),
            _ => CAbiTypeFacts::ZeroSized,
        };
        ForeignCallInputs::new(symbol.into(), convention, args, ret, return_facts)
    }

    #[test]
    fn shared_driver_preserves_sysv_and_aapcs_event_order() {
        let sysv_inputs = inputs(
            "sysv_fn",
            CallingConvention::X86_64SysV,
            vec![
                scalar(0),
                aggregate(1, image(16)),
                scalar(2),
                scalar(3),
                scalar(4),
            ],
            ForeignReturn::Aggregate { image: image(24) },
        );
        let mut sysv = TraceBackend::new(CallingConvention::X86_64SysV, 6);
        let sysv_result = lower_foreign_call(&mut sysv, sysv_inputs, VReg::new(0));
        assert_eq!(sysv_result.slots.len(), 1);
        assert_eq!(
            sysv.events,
            vec![
                "reserve_sret",
                "get:CfgValue(0)",
                "image:2",
                "get:CfgValue(2)",
                "get:CfgValue(3)",
                "get:CfgValue(4)",
                "stack[16]:v106@0+8",
                "registers:Gp0=v100,Gp1=v101,Gp2=v102,Gp3=v103,Gp4=v104,Gp5=v105",
                "call:sysv_fn",
                "cleanup_stack:16",
                "cleanup_byref:0",
                "sret_result:3",
                "move_primary",
            ]
        );

        let aapcs_inputs = inputs(
            "aapcs_fn",
            CallingConvention::Aarch64Aapcs,
            vec![
                scalar(0),
                aggregate(1, image(24)),
                scalar(2),
                scalar(3),
                scalar(4),
                scalar(5),
                scalar(6),
                scalar(7),
                scalar(8),
                scalar(9),
            ],
            ForeignReturn::Aggregate { image: image(24) },
        );
        let mut aapcs = TraceBackend::new(CallingConvention::Aarch64Aapcs, 8);
        let aapcs_result = lower_foreign_call(&mut aapcs, aapcs_inputs, VReg::new(0));
        assert_eq!(aapcs_result.slots.len(), 1);
        assert_eq!(
            aapcs.events,
            vec![
                "reserve_sret",
                "get:CfgValue(0)",
                "byref:32",
                "get:CfgValue(2)",
                "get:CfgValue(3)",
                "get:CfgValue(4)",
                "get:CfgValue(5)",
                "get:CfgValue(6)",
                "get:CfgValue(7)",
                "get:CfgValue(8)",
                "get:CfgValue(9)",
                "stack[16]:v109@0+8,v110@8+8",
                "registers:Gp0=v101,Gp1=v102,Gp2=v103,Gp3=v104,Gp4=v105,Gp5=v106,Gp6=v107,Gp7=v108",
                "assign_sret",
                "call:aapcs_fn",
                "cleanup_stack:16",
                "cleanup_byref:32",
                "sret_result:3",
                "move_primary",
            ]
        );

        let register_return_inputs = inputs(
            "aapcs_register_fn",
            CallingConvention::Aarch64Aapcs,
            Vec::new(),
            ForeignReturn::Aggregate { image: image(16) },
        );
        let mut register_return = TraceBackend::new(CallingConvention::Aarch64Aapcs, 8);
        let register_result =
            lower_foreign_call(&mut register_return, register_return_inputs, VReg::new(0));
        assert_eq!(register_result.slots.len(), 1);
        assert_eq!(
            register_return.events,
            vec![
                "stack[0]:",
                "registers:",
                "call:aapcs_register_fn",
                "cleanup_stack:0",
                "cleanup_byref:0",
                "register_result:2",
                "move_primary",
            ]
        );
    }

    #[test]
    fn the_darwin_row_reaches_the_backend_as_packed_stores() {
        let mut darwin = TraceBackend::new(CallingConvention::Aarch64AapcsDarwin, 8);
        lower_foreign_call(
            &mut darwin,
            inputs(
                "narrow_tail",
                CallingConvention::Aarch64AapcsDarwin,
                narrow_tail_args(),
                ForeignReturn::ZeroSized,
            ),
            VReg::new(0),
        );
        assert!(
            darwin
                .events
                .contains(&"stack[16]:v108@0+1,v109@2+2,v110@4+4,v111@8+8".to_string()),
            "packed Darwin stores must reach the backend leaf: {:?}",
            darwin.events
        );

        let mut linux = TraceBackend::new(CallingConvention::Aarch64Aapcs, 8);
        lower_foreign_call(
            &mut linux,
            inputs(
                "narrow_tail",
                CallingConvention::Aarch64Aapcs,
                narrow_tail_args(),
                ForeignReturn::ZeroSized,
            ),
            VReg::new(0),
        );
        assert!(
            linux
                .events
                .contains(&"stack[32]:v108@0+8,v109@8+8,v110@16+8,v111@24+8".to_string()),
            "AAPCS64 keeps whole 8-byte slots: {:?}",
            linux.events
        );
    }
}
