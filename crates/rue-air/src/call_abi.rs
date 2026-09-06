//! Per-type call-ABI facts: the projections one placement walk reads.
//!
//! Where a value crosses a call boundary — native or foreign — is answered once
//! by [`lower_c_signature`](crate::lower_c_signature) and
//! [`lower_native_signature`](crate::lower_native_signature) against the
//! convention description in `rue-target` (ADR-0064, ADR-0084). This module is
//! the other half: the *facts* those functions classify. It projects a live
//! type onto [`CAbiTypeFacts`] ([`c_abi_type_facts`], [`aggregate_leaves`]),
//! names the width-and-signedness class the extension table is keyed by
//! ([`CAbiScalarKind`]), and keeps the native value-decomposition width the CFG
//! parameter contract and the oracle's call contract track
//! ([`NativeCallAbi::arg_slot_width`]) — a layout measure, not a placement.
//!
//! Which convention governs a boundary is named by exactly one value type,
//! [`rue_target::CallingConvention`], whose rows are the native Rue convention
//! and the concrete platform psABIs.
//!
//! ## Two planes, one policy kernel
//!
//! Two walkers consume this module because their lifetimes differ: the live
//! classifiers here walk the request-scoped [`FrozenTypeInternPool`], while the
//! stable query plane (`compiler.call-abi` in `rue-compiler`) walks its own
//! revision-stable type keys and canonical layout values and must not hold a
//! live pool. Both project per-type facts and then run the same placement walk
//! — [`CAbiTypeFacts`] plus [`CAbiScalarKind`] against the convention
//! description [`rue_target::ConventionSpec`] carries — so the classification
//! policy itself has exactly one production home.

use crate::lowered_signature::CAbiTypeFacts;
use crate::{FrozenTypeInternPool, Type, TypeKind};
use rue_target::{CRegisterClass, CallingConvention};

/// How an argument is presented at the source level, before ABI classification.
///
/// The classifier only needs to distinguish a by-value argument from a
/// by-reference one; callers map their own argument-mode enum (`CfgArgMode`)
/// onto this so the classifier does not depend on the CFG crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgConvention {
    /// A normal by-value argument; its physical layout crosses directly.
    ByValue,
    /// An `inout` or `borrow` argument, represented by one caller pointer.
    ByReference,
}

/// The native convention's *physical slot* measure.
///
/// Where a native value crosses is [`lower_native_signature`](crate::lower_native_signature)'s
/// answer against the same [`CAbiTypeFacts`] a C crossing presents (ADR-0084);
/// what survives here is the value-decomposition width the CFG parameter
/// contract and the oracle's call contract track, which is a layout measure
/// rather than a placement.
#[derive(Debug, Clone, Copy)]
pub struct NativeCallAbi<'a> {
    type_pool: &'a FrozenTypeInternPool,
}

impl<'a> NativeCallAbi<'a> {
    /// Build the native slot measure over `type_pool`.
    pub fn new(type_pool: &'a FrozenTypeInternPool) -> Self {
        Self { type_pool }
    }

    /// The convention this classifier implements.
    pub const fn abi(&self) -> CallingConvention {
        CallingConvention::Rue
    }

    /// Physical parameter-slot width of one argument: the value-decomposition
    /// count the CFG parameter layout and the oracle's call contract track.
    ///
    /// A by-reference argument is one pointer slot; a by-value argument is its
    /// flattened slot count. This is representation 2 in ADR-0052 terms and is
    /// deliberately independent of the compact transitional classification: the
    /// callee's parameter slots stay slot-shaped even for a compact aggregate,
    /// which is precisely why code generation refuses the not-yet-implemented
    /// indirect marshaling rather than silently disagreeing about slot counts.
    pub fn arg_slot_width(&self, ty: Type, convention: ArgConvention) -> u32 {
        match convention {
            ArgConvention::ByReference => 1,
            ArgConvention::ByValue => self.type_pool.abi_slot_count(ty),
        }
    }
}

/// Whether `ty` needs a complete aggregate slot representation rather than a
/// single primary vreg, given its flattened slot count `slot_count`.
///
/// Structs and arrays always do; a discriminant-only enum stays a scalar, and
/// an enum with a payload becomes an aggregate exactly when its slot count says
/// so (oversized enums route through the same slot-count policy per RUE-946).
/// This is the single authority behind both halves of a call: the call planner
/// and code generation's slots-versus-primary materialization, which cannot
/// disagree about which types are aggregates without a call passing a value in
/// a shape the other side never expects.
pub fn is_multislot_aggregate(ty: Type, slot_count: u32) -> bool {
    matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_))
        || (ty.is_enum() && slot_count > 1)
}

/// Whether the compact physical layout of `ty` is byte-for-byte identical to
/// the flattened eight-byte slot layout, so slot-shaped marshaling is exactly
/// correct for it (ADR-0052).
///
/// True for eight-byte leaves (`i64`/`u64`/pointers, the recovery scalar) and
/// zero-sized / compile-time-only types, and for aggregates built entirely from
/// slot-identical leaves. Narrow scalars (one/two/four bytes) and enums (narrow
/// tag) are not slot-identical. This is the single authority code generation's
/// narrow-access refusal (RUE-974) consults, so no two sites disagree about
/// which types the compact layout leaves unchanged.
pub fn is_slot_identical_layout<P: crate::FfiTypePool + ?Sized>(type_pool: &P, ty: Type) -> bool {
    match ty.kind() {
        // Eight-byte leaves and the recovery scalar: identical in both models.
        TypeKind::I64
        | TypeKind::U64
        | TypeKind::PtrConst(_)
        | TypeKind::PtrMut(_)
        | TypeKind::Error => true,
        // Zero-sized and compile-time-only types have identical (zero) extent.
        TypeKind::Unit
        | TypeKind::Never
        | TypeKind::ComptimeType
        | TypeKind::ComptimeFloat
        | TypeKind::Module(_) => true,
        // Phase 4 deliberately has no float ABI lowering yet.
        TypeKind::F32 | TypeKind::F64 => false,
        // Narrow scalars: one/two/four bytes under the compact layout.
        TypeKind::I8
        | TypeKind::U8
        | TypeKind::Bool
        | TypeKind::I16
        | TypeKind::U16
        | TypeKind::I32
        | TypeKind::U32 => false,
        TypeKind::Struct(id) => type_pool
            .ffi_struct_field_types(id)
            .into_iter()
            .all(|field_ty| is_slot_identical_layout(type_pool, field_ty)),
        TypeKind::Array(id) => {
            let element = type_pool.ffi_array_element(id);
            is_slot_identical_layout(type_pool, element)
        }
        // Enums narrow their tag (u8/u16/u32 vs an eight-byte slot).
        TypeKind::Enum(_) => false,
    }
}

// ============================================================================
// The guaranteed target-C classifier (ADR-0064 P2, RUE-1056)
// ============================================================================

/// Width-and-signedness class of a target-C-passable scalar: the one fact the
/// extension policy needs. Each plane projects its own type representation
/// onto this class, so the sign/zero/`_Bool` extension table itself
/// ([`Self::extension`]) has exactly one home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CAbiScalarKind {
    /// 8-bit signed integer.
    I8,
    /// 16-bit signed integer.
    I16,
    /// 32-bit signed integer.
    I32,
    /// 8-bit unsigned integer.
    U8,
    /// 16-bit unsigned integer.
    U16,
    /// 32-bit unsigned integer.
    U32,
    /// The 1-byte C `_Bool` whose byte is 0/1 by contract.
    Bool,
    /// A 32-bit binary floating-point value.
    F32,
    /// A 64-bit binary floating-point value.
    F64,
    /// A value that already fills its 64-bit register: `i64`/`u64`, pointers,
    /// and the recovery scalar.
    RegisterWidth,
}

impl CAbiScalarKind {
    /// The live plane's projection of a type onto its width-and-signedness
    /// class, or `None` when the type is not a scalar (an aggregate, or a
    /// compile-time-only type). The stable query plane makes the same
    /// projection from its own type keys.
    ///
    /// The float classes are part of the projection because the native
    /// convention places floats by these same rules (ADR-0084); the C boundary
    /// rejects them earlier, in `c_passable_by_value`.
    pub fn for_live_type(ty: Type) -> Option<Self> {
        Some(match ty.kind() {
            TypeKind::I8 => Self::I8,
            TypeKind::I16 => Self::I16,
            TypeKind::I32 => Self::I32,
            TypeKind::U8 => Self::U8,
            TypeKind::U16 => Self::U16,
            TypeKind::U32 => Self::U32,
            TypeKind::Bool => Self::Bool,
            TypeKind::F32 => Self::F32,
            TypeKind::F64 => Self::F64,
            TypeKind::I64
            | TypeKind::U64
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_)
            | TypeKind::Error => Self::RegisterWidth,
            _ => return None,
        })
    }

    /// The register bank this scalar travels in: the floating-point file for a
    /// float, the general-purpose one for every integer, `bool`, and pointer.
    pub const fn register_class(self) -> CRegisterClass {
        match self {
            Self::F32 | Self::F64 => CRegisterClass::Fp,
            Self::I8
            | Self::I16
            | Self::I32
            | Self::U8
            | Self::U16
            | Self::U32
            | Self::Bool
            | Self::RegisterWidth => CRegisterClass::Gp,
        }
    }

    /// The scalar's own width in bytes: the footprint a stacked copy takes
    /// under a row that packs the outgoing argument area at natural size
    /// ([`rue_target::StackedArgumentPacking::NaturalSize`]).
    pub const fn natural_bytes(self) -> u32 {
        match self {
            Self::I8 | Self::U8 | Self::Bool => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 | Self::RegisterWidth => 8,
        }
    }

    /// The canonical 64-bit extension for this scalar at a target-C boundary.
    /// Both psABIs agree on the operation; signed narrows sign-extend from
    /// their declared width, unsigned narrows zero-extend, `_Bool`
    /// zero-extends from its low byte, and register-width values need nothing.
    pub const fn extension(self) -> ScalarAbiExtension {
        match self {
            Self::I8 => ScalarAbiExtension::Signed { from_bits: 8 },
            Self::I16 => ScalarAbiExtension::Signed { from_bits: 16 },
            Self::I32 => ScalarAbiExtension::Signed { from_bits: 32 },
            Self::U8 | Self::Bool => ScalarAbiExtension::Unsigned { from_bits: 8 },
            Self::U16 => ScalarAbiExtension::Unsigned { from_bits: 16 },
            Self::U32 => ScalarAbiExtension::Unsigned { from_bits: 32 },
            // A float fills its register: nothing is extended into or out of it.
            Self::F32 | Self::F64 | Self::RegisterWidth => ScalarAbiExtension::None,
        }
    }
}

/// How a narrow scalar is extended to fill its 64-bit integer register at a
/// target-C boundary (ADR-0064 P2). "Narrow" means any value smaller than the
/// register: the sub-64-bit integers and `bool`. The extension is the same
/// operation whether it is applied by the caller before an argument crosses or
/// by the caller after a return crosses; [`c_abi_type_facts`] documents which
/// side each row leaves owing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarAbiExtension {
    /// The value already fills its 64-bit register (`i64`/`u64`/pointer): no
    /// extension instruction is emitted.
    None,
    /// Sign-extend the low `from_bits` to 64 (a signed narrow integer:
    /// `i8`/`i16`/`i32`).
    Signed {
        /// The declared width of the value, in bits (8, 16, or 32).
        from_bits: u32,
    },
    /// Zero-extend the low `from_bits` to 64 (an unsigned narrow integer
    /// `u8`/`u16`/`u32`, or `bool` as the 1-byte `_Bool` whose byte is 0/1 by
    /// contract, `from_bits == 8`).
    Unsigned {
        /// The declared width of the value, in bits (8, 16, or 32).
        from_bits: u32,
    },
}

impl ScalarAbiExtension {
    /// Whether this extension emits no instruction (the value is already
    /// register-width canonical).
    pub const fn is_noop(self) -> bool {
        matches!(self, Self::None)
    }

    /// The scalar's natural C width in bytes: the declared width a narrow value
    /// is extended *from*, or the full 8-byte register for a register-width
    /// value. This is the size a stacked argument occupies under a psABI that
    /// packs the outgoing argument area at natural size
    /// ([`rue_target::StackedArgumentPacking::NaturalSize`]).
    pub const fn natural_bytes(self) -> u32 {
        match self {
            Self::None => 8,
            Self::Signed { from_bits } | Self::Unsigned { from_bits } => from_bits / 8,
        }
    }
}

/// The narrow-integer extension every C row asks for, and who owes it.
///
/// Every supported scalar (`c_passable_by_value`: the full integer set, `bool`,
/// pointers) occupies exactly one general-purpose register, and Rue's internal
/// invariant keeps a narrow value canonically 64-bit-extended in its vreg
/// (signed sign-extended, unsigned and `bool` zero-extended). That is a
/// *stronger* guarantee than any row asks of an argument, so **argument passing
/// needs no boundary instruction** on any of them — including Apple's row,
/// which makes the caller extend an argument narrower than 32 bits.
///
/// The one direction that does need an instruction is the **return**: SysV
/// AMD64 leaves the bits above a narrow result unspecified, and AAPCS64 defines
/// only bits 0..31, so the caller re-extends the returned scalar to Rue's
/// canonical form. That operation is the [`ScalarAbiExtension`] the lowered
/// signature carries ([`LoweredReturn::Registers`](crate::LoweredReturn)), and
/// applying it at the boundary is what preserves the program-wide scalar
/// invariant across a C call.
///
/// C `_Bool` is one byte whose only valid values are 0 and 1. Passing Rue's
/// `bool` (a 0/1 word) satisfies that directly; a `_Bool` return is
/// zero-extended from its low byte, materializing exactly 0/1
/// ([`ScalarAbiExtension::Unsigned`] `{ from_bits: 8 }`).
///
/// The live plane's projection of `ty` onto the target-C classification facts
/// [`lower_c_signature`](crate::lower_c_signature) consumes.
///
/// This is the C-boundary twin of [`NativeCallAbi::facts`]: it walks the
/// request-scoped [`FrozenTypeInternPool`], while the stable query plane makes
/// the same projection from its revision-stable type keys and canonical layout
/// values. Both then classify through the one kernel, so a call, a return, and
/// an export cannot disagree about a placement.
///
/// `ty` must already have passed
/// [`c_passable_by_value`](crate::c_passable_by_value); an unsupported type
/// panics rather than being guessed at.
pub fn c_abi_type_facts(type_pool: &FrozenTypeInternPool, ty: Type) -> CAbiTypeFacts {
    if matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_)) {
        let layout = type_pool.layout(ty);
        return CAbiTypeFacts::Aggregate {
            size: layout.size,
            align: layout.alignment,
            leaves: aggregate_leaves(type_pool, ty, layout.size),
        };
    }
    if type_pool.abi_slot_count(ty) == 0 {
        return CAbiTypeFacts::ZeroSized;
    }
    let kind = CAbiScalarKind::for_live_type(ty).unwrap_or_else(|| {
        panic!(
            "target-C classification called on unsupported type {:?}; \
             c_passable_by_value gates the boundary before lowering",
            ty.kind()
        )
    });
    CAbiTypeFacts::Scalar {
        kind,
        class: kind.register_class(),
    }
}

/// The live plane's projection of an aggregate's scalar leaves.
///
/// Classification asks which bank each eightbyte belongs to and whether the
/// whole aggregate is a homogeneous floating-point aggregate, and both answers
/// come from the leaves: a scalar field's byte offset, its width, and whether
/// it is an integer, an `f32` or an `f64`. The walk reads the same canonical
/// layout every other physical consumer reads — struct field offsets, array
/// element stride, the enum tag and per-variant payload offsets — so the leaves
/// describe the type's actual memory image.
///
/// An aggregate larger than [`crate::MAX_LEAF_CLASSIFIED_BYTES`] is reported all
/// integer without a walk: no supported row's answer for one that large depends
/// on its leaves, so this bounds the cost of a large array.
pub fn aggregate_leaves(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    size: u64,
) -> crate::AggregateLeaves {
    if size > crate::MAX_LEAF_CLASSIFIED_BYTES {
        return crate::AggregateLeaves::all_integer(size);
    }
    let mut leaves = Vec::new();
    push_leaves(type_pool, ty, 0, &mut leaves);
    crate::AggregateLeaves::from_leaves(size, leaves)
}

/// The classification kind of one scalar leaf: the floats are told apart from
/// each other because AAPCS64's homogeneous rule is keyed by member width, and
/// everything else — integers, `bool`, pointers — is one kind.
fn leaf_kind(ty: Type) -> crate::CAbiLeafKind {
    match ty.kind() {
        TypeKind::F32 => crate::CAbiLeafKind::F32,
        TypeKind::F64 => crate::CAbiLeafKind::F64,
        _ => crate::CAbiLeafKind::Integer,
    }
}

/// Append every scalar leaf of `ty`, placed at `base` bytes, to `out`.
///
/// An enum contributes its tag as an integer leaf and then the *union* of every
/// variant's payload leaves, each at that variant's own offsets: a union's
/// eightbyte is classified by every leaf that can occupy it, which is the same
/// merge SysV AMD64 section 3.2.3 applies to a C union.
fn push_leaves(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    base: u64,
    out: &mut Vec<crate::CAbiLeaf>,
) {
    match ty.kind() {
        TypeKind::Unit | TypeKind::Never => {}
        TypeKind::Struct(struct_id) => {
            let layout = type_pool.layout(ty);
            let crate::LayoutKind::Struct { field_offsets, .. } = &layout.kind else {
                return;
            };
            let struct_def = type_pool.struct_def(struct_id);
            for (field, offset) in struct_def.fields.iter().zip(field_offsets) {
                push_leaves(type_pool, field.ty, base.saturating_add(*offset), out);
            }
        }
        TypeKind::Array(array_id) => {
            let (element, count) = type_pool.array_def(array_id);
            let stride = type_pool.layout(element).stride;
            for index in 0..count {
                push_leaves(
                    type_pool,
                    element,
                    base.saturating_add(index.saturating_mul(stride)),
                    out,
                );
            }
        }
        TypeKind::Enum(enum_id) => {
            let layout = type_pool.layout(ty);
            let crate::LayoutKind::Enum { tag, variants, .. } = &layout.kind else {
                return;
            };
            out.push(crate::CAbiLeaf::integer(base, tag.size));
            let enum_def = type_pool.enum_def(enum_id);
            for (variant, offsets) in variants.iter().enumerate() {
                for (payload, offset) in enum_def.variant_payload(variant).iter().zip(offsets) {
                    push_leaves(type_pool, *payload, base.saturating_add(*offset), out);
                }
            }
        }
        _ => out.push(crate::CAbiLeaf {
            offset: base,
            width: type_pool.layout(ty).size,
            kind: leaf_kind(ty),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_target::StackedArgumentPacking;

    // Behavioral coverage that exercises the classifier against a real type
    // pool lives with the backend ABI/oracle suites (which own program
    // fixtures); these unit checks pin the pure classification algebra, and the
    // leaf projection against a small pool of its own.

    fn pool_with_struct(fields: &[(&str, Type)]) -> (crate::FrozenTypeInternPool, Type) {
        let interner = lasso::ThreadedRodeo::new();
        let pool = crate::TypeInternPool::new();
        let (id, _) = pool.register_struct(
            interner.get_or_intern("Probe"),
            crate::StructDef {
                name: "Probe".into(),
                fields: fields
                    .iter()
                    .map(|(name, ty)| crate::StructField {
                        name: (*name).to_string(),
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
        let pool = pool.freeze();
        (pool, ty)
    }

    #[test]
    fn the_live_plane_projects_each_leaf_at_its_own_offset_and_kind() {
        let (pool, mixed) = pool_with_struct(&[("a", Type::F64), ("b", Type::I64)]);
        let facts = c_abi_type_facts(&pool, mixed);
        let CAbiTypeFacts::Aggregate { size, leaves, .. } = facts else {
            panic!("a struct projects aggregate facts, got {facts:?}");
        };
        assert_eq!(
            leaves.eightbyte_class(0),
            crate::EightbyteClass::Sse,
            "the `f64` field's eightbyte carries no integer leaf"
        );
        assert_eq!(
            leaves.eightbyte_class(size.div_ceil(8) as u32 - 1),
            crate::EightbyteClass::Integer
        );
        assert_eq!(leaves.homogeneous_floats(), None);
        assert!(!leaves.has_unaligned_leaf());

        // Two floats of one width are a homogeneous floating-point aggregate,
        // whatever the layout in force spaces them at.
        let (pool, floats) = pool_with_struct(&[("a", Type::F64), ("b", Type::F64)]);
        let facts = c_abi_type_facts(&pool, floats);
        let CAbiTypeFacts::Aggregate { leaves, .. } = facts else {
            panic!("a struct projects aggregate facts, got {facts:?}");
        };
        assert_eq!(leaves.homogeneous_floats(), Some((8, 2)));
        assert_eq!(leaves.eightbyte_class(0), crate::EightbyteClass::Sse);
        assert_eq!(leaves.eightbyte_class(1), crate::EightbyteClass::Sse);
    }

    #[test]
    fn an_aggregate_past_the_leaf_bound_is_projected_all_integer() {
        // Nothing any row does with an aggregate this large depends on its
        // leaves, so the projection stops rather than walking it.
        let pool = crate::TypeInternPool::new();
        let array = pool.intern_array_from_type(Type::F64, 64);
        let pool = pool.freeze();
        let ty = Type::new_array(array);
        assert!(pool.layout(ty).size > crate::MAX_LEAF_CLASSIFIED_BYTES);
        let facts = c_abi_type_facts(&pool, ty);
        let CAbiTypeFacts::Aggregate { size, leaves, .. } = facts else {
            panic!("an array projects aggregate facts, got {facts:?}");
        };
        assert_eq!(leaves, crate::AggregateLeaves::all_integer(size));
    }

    /// The extension a scalar crossing `convention` needs in each direction,
    /// read out of the one lowering every C crossing consumes.
    fn scalar_extensions(
        convention: CallingConvention,
        ty: Type,
    ) -> (ScalarAbiExtension, ScalarAbiExtension) {
        let pool = crate::TypeInternPool::new().freeze();
        let facts = c_abi_type_facts(&pool, ty);
        let lowered =
            crate::lower_c_signature(convention, &[(facts, ArgConvention::ByValue)], facts);
        let crate::LoweredReturn::Registers { extension, .. } = lowered.ret() else {
            panic!("a C scalar comes back in a result register");
        };
        (lowered.arguments()[0].extension, extension)
    }

    #[test]
    fn target_c_scalar_return_extension_table() {
        let ret = |ty| scalar_extensions(CallingConvention::X86_64SysV, ty).1;
        assert_eq!(ret(Type::I8), ScalarAbiExtension::Signed { from_bits: 8 });
        assert_eq!(ret(Type::U8), ScalarAbiExtension::Unsigned { from_bits: 8 });
        assert_eq!(ret(Type::I16), ScalarAbiExtension::Signed { from_bits: 16 });
        assert_eq!(
            ret(Type::U16),
            ScalarAbiExtension::Unsigned { from_bits: 16 }
        );
        assert_eq!(ret(Type::I32), ScalarAbiExtension::Signed { from_bits: 32 });
        assert_eq!(
            ret(Type::U32),
            ScalarAbiExtension::Unsigned { from_bits: 32 }
        );
        // The 1-byte `_Bool` 0/1 contract: zero-extend from its byte.
        assert_eq!(
            ret(Type::BOOL),
            ScalarAbiExtension::Unsigned { from_bits: 8 }
        );
        // Register-width scalars need no extension.
        assert!(ret(Type::I64).is_noop());
        assert!(ret(Type::U64).is_noop());
    }

    #[test]
    fn every_row_agrees_on_the_scalar_extension_operation() {
        // The narrow-integer extension is the same operation on every C row and
        // in both directions; the rows differ only in documented "who extends"
        // and sret-echo details, which the convention description carries.
        for ty in [
            Type::I8,
            Type::U8,
            Type::I16,
            Type::U16,
            Type::I32,
            Type::U32,
            Type::I64,
            Type::U64,
            Type::BOOL,
        ] {
            let sysv = scalar_extensions(CallingConvention::X86_64SysV, ty);
            assert_eq!(
                sysv.0, sysv.1,
                "arg and return extension are the same operation for {ty:?}"
            );
            for convention in [
                CallingConvention::Aarch64Aapcs,
                CallingConvention::Aarch64AapcsDarwin,
            ] {
                assert_eq!(
                    scalar_extensions(convention, ty),
                    sysv,
                    "{convention} must agree with SysV on the extension for {ty:?}"
                );
            }
        }
    }

    #[test]
    fn each_row_carries_its_own_register_budget_and_sret_rule() {
        let sysv = CallingConvention::X86_64SysV.c_spec();
        let aapcs = CallingConvention::Aarch64Aapcs.c_spec();
        assert_eq!(sysv.gp_argument_registers, 6);
        assert_eq!(aapcs.gp_argument_registers, 8);
        // SysV passes the sret pointer as the hidden first argument in `rdi`
        // and echoes it in `rax`; AAPCS64 uses the dedicated `x8`, unechoed.
        assert!(sysv.sret_pointer_in_argument_register());
        assert!(sysv.sret_pointer_echoed_in_result_register);
        assert!(!aapcs.sret_pointer_in_argument_register());
        assert!(!aapcs.sret_pointer_echoed_in_result_register);
        // 16-byte call alignment on both.
        assert_eq!(sysv.call_stack_alignment, 16);
        assert_eq!(aapcs.call_stack_alignment, 16);
    }

    #[test]
    fn the_row_follows_the_target_c_alias_not_the_architecture() {
        use rue_target::Target;
        assert_eq!(
            CallingConvention::c_for_target(Target::X86_64Linux),
            CallingConvention::X86_64SysV
        );
        assert_eq!(
            CallingConvention::c_for_target(Target::Aarch64Linux),
            CallingConvention::Aarch64Aapcs
        );
        assert_eq!(
            CallingConvention::c_for_target(Target::Aarch64Macos),
            CallingConvention::Aarch64AapcsDarwin
        );
    }

    #[test]
    fn the_two_aapcs_rows_agree_except_on_stacked_argument_packing() {
        let aapcs = CallingConvention::Aarch64Aapcs;
        let darwin = CallingConvention::Aarch64AapcsDarwin;
        assert_eq!(
            aapcs.c_spec().gp_argument_registers,
            darwin.c_spec().gp_argument_registers
        );
        assert_eq!(
            aapcs.c_spec().sret_pointer_in_argument_register(),
            darwin.c_spec().sret_pointer_in_argument_register()
        );
        assert_eq!(
            aapcs.c_spec().sret_pointer_echoed_in_result_register,
            darwin.c_spec().sret_pointer_echoed_in_result_register
        );
        assert_eq!(
            aapcs.c_spec().call_stack_alignment,
            darwin.c_spec().call_stack_alignment
        );
        // Apple's amendment: a stacked argument occupies its natural size at
        // its natural alignment rather than a whole 8-byte slot.
        assert_eq!(
            aapcs.stacked_argument_packing(),
            StackedArgumentPacking::EightByteSlots
        );
        assert_eq!(
            darwin.stacked_argument_packing(),
            StackedArgumentPacking::NaturalSize
        );
        assert_eq!(
            CallingConvention::X86_64SysV.stacked_argument_packing(),
            StackedArgumentPacking::EightByteSlots,
            "x86-64 is unaffected by the Apple amendment"
        );
    }

    #[test]
    fn c_scalar_kind_extension_table_is_the_shared_authority() {
        use CAbiScalarKind as K;
        assert_eq!(
            K::I8.extension(),
            ScalarAbiExtension::Signed { from_bits: 8 }
        );
        assert_eq!(
            K::I16.extension(),
            ScalarAbiExtension::Signed { from_bits: 16 }
        );
        assert_eq!(
            K::I32.extension(),
            ScalarAbiExtension::Signed { from_bits: 32 }
        );
        assert_eq!(
            K::U8.extension(),
            ScalarAbiExtension::Unsigned { from_bits: 8 }
        );
        assert_eq!(
            K::U16.extension(),
            ScalarAbiExtension::Unsigned { from_bits: 16 }
        );
        assert_eq!(
            K::U32.extension(),
            ScalarAbiExtension::Unsigned { from_bits: 32 }
        );
        // The 1-byte `_Bool` 0/1 contract zero-extends from its byte.
        assert_eq!(
            K::Bool.extension(),
            ScalarAbiExtension::Unsigned { from_bits: 8 }
        );
        assert!(K::RegisterWidth.extension().is_noop());
    }
}
