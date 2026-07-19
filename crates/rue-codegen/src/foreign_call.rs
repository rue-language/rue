//! Target-independent planning for `extern "C"` foreign calls (ADR-0064 P3).
//!
//! P2 crossed only scalars and pointers, which occupy one integer register each
//! and reuse the native call plan with a boundary return extension. P3 adds
//! **C-classifiable aggregates**, whose target-C crossing disagrees with the
//! native slot model — the native planner decomposes an aggregate one slot per
//! leaf and *reverses* the slots (the register-packing audit's key finding),
//! whereas C packs fields into eightbytes in ascending memory order. So a foreign
//! call that touches an aggregate is planned here, entirely separate from the
//! native `CallPlan`, and lowered through each aggregate's **physical memory
//! image** (which, for a `@repr(c)` struct of scalar/pointer fields, is exactly
//! the C object layout under the compact-layout default).
//!
//! This module only *classifies*: it names, per argument and for the return,
//! which target-C class ([`rue_air::AggregateArgClass`] /
//! [`rue_air::AggregateReturnClass`]) governs the crossing and carries the
//! aggregate's compact image map. The two backends' `emit_foreign_call` own the
//! physical register/stack/sret sequence — the only genuinely per-architecture
//! part — consuming this shared authority so neither makes a local ABI decision.

use rue_air::layout::PaddingRange;
use rue_air::{
    AggregateArgClass, AggregateReturnClass, FrozenTypeInternPool, ScalarAbiExtension,
    TargetCAbiFlavor, TargetCCallAbi, Type,
};
use rue_cfg::{Cfg, CfgCallArg, CfgValue};

use crate::types::{self, PhysicalEnumSlot};

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
            size: layout.size as u32,
            align: layout.alignment as u32,
            storage_bytes: align_up(layout.size as u32, 16),
        }
    }

    /// Number of eightbytes (8-byte integer registers / stack slots) the image
    /// spans: `ceil(size / 8)`.
    pub fn eightbytes(&self) -> u32 {
        self.size.div_ceil(8)
    }
}

/// How one foreign-call argument crosses the target-C boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignArg {
    /// A scalar or pointer, already canonically extended in its vreg by Rue's
    /// internal invariant — occupies one integer register (or spills to the
    /// stack once the integer registers are exhausted).
    Scalar { value: CfgValue },
    /// A ≤16-byte aggregate packed into one or two integer registers in C field
    /// order ([`AggregateArgClass::IntegerRegisters`]).
    AggregateRegisters {
        value: CfgValue,
        image: AggregateImage,
    },
    /// A >16-byte aggregate passed by value in the outgoing stack argument area
    /// (SysV MEMORY class, [`AggregateArgClass::ByValueStack`]).
    AggregateByvalStack {
        value: CfgValue,
        image: AggregateImage,
    },
    /// A >16-byte aggregate passed as a pointer to a caller-owned copy (AAPCS64,
    /// [`AggregateArgClass::ByReferenceCopy`]).
    AggregateByRefCopy {
        value: CfgValue,
        image: AggregateImage,
    },
}

/// How a foreign call's return value crosses the target-C boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignReturn {
    /// Unit / never / empty: no materialized value.
    ZeroSized,
    /// A scalar returned in the primary result register; `ext` re-extends it to
    /// Rue's canonical 64-bit form (a C callee leaves the high bits unspecified).
    Scalar { ext: ScalarAbiExtension },
    /// A ≤16-byte aggregate returned in one or two result registers (`rax:rdx` /
    /// `x0:x1`) in C field order; the backend bridges the registers through the
    /// image buffer to reconstruct the native slots.
    AggregateRegisters { image: AggregateImage },
    /// A >16-byte aggregate returned indirectly through caller storage (sret);
    /// the callee writes the C image, the backend reads the native slots back.
    AggregateSret { image: AggregateImage },
}

/// A classified foreign call: the C symbol, every argument and the return, plus
/// the psABI flavor whose classifier produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignCallInputs {
    /// The resolved (unmangled) C symbol the undefined reference targets.
    pub symbol: String,
    pub flavor: TargetCAbiFlavor,
    pub args: Vec<ForeignArg>,
    pub ret: ForeignReturn,
}

impl ForeignCallInputs {
    /// The C symbol this call targets.
    pub fn symbol_ref(&self) -> &str {
        &self.symbol
    }
}

impl ForeignCallInputs {
    /// Whether a foreign call to `return_ty` with `args` needs the aggregate
    /// path at all: true when the return or any argument is an aggregate
    /// (struct/array) type. A scalars-only foreign call keeps P2's native-plan
    /// route with a boundary return extension.
    pub(crate) fn call_touches_aggregate(cfg: &Cfg, return_ty: Type, args: &[CfgCallArg]) -> bool {
        is_aggregate(return_ty)
            || args
                .iter()
                .any(|arg| is_aggregate(cfg.get_inst(arg.value).ty))
    }

    /// Classify a foreign call through the shared [`TargetCCallAbi`] authority.
    pub(crate) fn from_cfg(
        symbol: String,
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        return_ty: Type,
        args: &[CfgCallArg],
        flavor: TargetCAbiFlavor,
    ) -> Self {
        let abi = TargetCCallAbi::new(flavor);
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
                if !is_aggregate(ty) {
                    return ForeignArg::Scalar { value };
                }
                let image = AggregateImage::for_type(type_pool, ty);
                match abi.classify_aggregate_arg(image.size as u64, image.align as u64) {
                    AggregateArgClass::IntegerRegisters { .. } => {
                        ForeignArg::AggregateRegisters { value, image }
                    }
                    AggregateArgClass::ByValueStack { .. } => {
                        ForeignArg::AggregateByvalStack { value, image }
                    }
                    AggregateArgClass::ByReferenceCopy { .. } => {
                        ForeignArg::AggregateByRefCopy { value, image }
                    }
                }
            })
            .collect();

        let ret = if !is_aggregate(return_ty) {
            match type_pool.abi_slot_count(return_ty) {
                0 => ForeignReturn::ZeroSized,
                _ => ForeignReturn::Scalar {
                    ext: abi.scalar_return_extension(return_ty),
                },
            }
        } else {
            let image = AggregateImage::for_type(type_pool, return_ty);
            match abi.classify_aggregate_return(image.size as u64, image.align as u64) {
                AggregateReturnClass::IntegerRegisters { .. } => {
                    ForeignReturn::AggregateRegisters { image }
                }
                AggregateReturnClass::Indirect { .. } => ForeignReturn::AggregateSret { image },
            }
        };

        Self {
            symbol,
            flavor,
            args: planned_args,
            ret,
        }
    }
}

/// Whether `ty` is an aggregate (struct or fixed array) — the types whose
/// target-C crossing needs the image-based foreign path.
fn is_aggregate(ty: Type) -> bool {
    matches!(
        ty.kind(),
        rue_air::TypeKind::Struct(_) | rue_air::TypeKind::Array(_)
    )
}

const fn align_up(value: u32, alignment: u32) -> u32 {
    value.div_ceil(alignment) * alignment
}
