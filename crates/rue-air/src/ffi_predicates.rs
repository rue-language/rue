//! The three FFI-safety predicates (ADR-0064 Amendment 1, RUE-1063).
//!
//! FFI-safety is deliberately split into three queryable compiler judgments,
//! sitting beside the [`call_abi`](crate::call_abi) classifier they feed, so
//! pointer-based interop can ship before by-value classification exists:
//!
//! 1. [`has_c_layout`] — T's size, alignment, field order, field offsets, and
//!    tail padding equal the target C aggregate's. `@repr(c)` establishes it for
//!    a struct (a guarantee today; a real constraint once RUE-881 can reorder
//!    unmarked types).
//! 2. [`c_ffi_safe`] — the structural hard-error policy: linear,
//!    destructor-bearing, and ownership-bearing fields are forbidden across the
//!    boundary. Pointee layouts are *not* required, so opaque pointers pass.
//! 3. [`c_passable_by_value`] — the by-value classifier seam. In P1.5 it
//!    delegates to the P1 register-only restriction (`i64`/`u64`/pointers); it is
//!    the seam P2/P3 fill with eightbyte classification and register packing.
//!
//! Every failure carries the failing [`FfiPredicate`], the offending field
//! *path*, and a structured [`FfiRejectReason`] rather than a bare `bool` — the
//! RUE-504 machine-readable exposure direction, so "why is this type not
//! FFI-safe" is answerable with the failing predicate and the offending field.
//!
//! The predicates are generic over [`FfiTypePool`], implemented by both the
//! mutable [`TypeInternPool`](crate::intern_pool::TypeInternPool) (used during
//! semantic analysis, when the marker check and extern-signature enforcement
//! run) and the [`FrozenTypeInternPool`](crate::FrozenTypeInternPool) (used by
//! the classifier and codegen), so there is one algebra rather than two.

use crate::{ArrayTypeId, StructId, Type, TypeKind};

/// Which of the three FFI predicates a failure belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiPredicate {
    /// [`has_c_layout`]: the type does not have a guaranteed C object layout.
    HasCLayout,
    /// [`c_ffi_safe`]: the type carries an ownership obligation C cannot honor.
    CFfiSafe,
    /// [`c_passable_by_value`]: the type is not (yet) passable in registers.
    CPassableByValue,
}

impl FfiPredicate {
    /// The predicate name, for machine-readable exposure (RUE-504).
    pub const fn name(self) -> &'static str {
        match self {
            FfiPredicate::HasCLayout => "has_c_layout",
            FfiPredicate::CFfiSafe => "c_ffi_safe",
            FfiPredicate::CPassableByValue => "c_passable_by_value",
        }
    }
}

/// The specific reject-don't-guess reason a predicate failed (ADR-0064
/// Amendment 1). Each maps to a stable human phrase via [`Self::describe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiRejectReason {
    /// An empty / zero-sized struct (C has no zero-sized object).
    EmptyStruct,
    /// An aggregate nested by value without its own `@repr(c)` marker — not
    /// silently promoted to C layout.
    NonReprCAggregate,
    /// A Rue enum (tagged sum type) — not FFI-safe in v0.
    Enum,
    /// A destructor-bearing type — an ownership obligation C cannot honor.
    HasDestructor,
    /// A linear type — a must-consume obligation C cannot honor.
    Linear,
    /// A type with no C representation at all (unit, never, module, comptime,
    /// slice-shaped, …).
    UnsupportedType,
    /// A type that is not in the by-value set the current phase (P2) implements
    /// — the [`c_passable_by_value`] seam P3 widens to aggregates.
    NotRegisterPassable,
}

impl FfiRejectReason {
    /// A stable human phrase naming this reject-list entry.
    pub const fn describe(self) -> &'static str {
        match self {
            FfiRejectReason::EmptyStruct => {
                "empty / zero-sized structs have no C object representation in v0"
            }
            FfiRejectReason::NonReprCAggregate => {
                "a nested aggregate must itself be marked `@repr(c)` — C layout is \
                 never silently promoted"
            }
            FfiRejectReason::Enum => "a Rue enum is not FFI-safe in v0",
            FfiRejectReason::HasDestructor => {
                "a destructor-bearing type cannot cross the C boundary (no ownership \
                 obligation crosses)"
            }
            FfiRejectReason::Linear => {
                "a linear type cannot cross the C boundary (no must-consume obligation \
                 crosses)"
            }
            FfiRejectReason::UnsupportedType => "this type has no C representation in v0",
            FfiRejectReason::NotRegisterPassable => {
                "this type is not yet passable by value across a C boundary (only \
                 integer and pointer scalars are; aggregates await a later FFI phase)"
            }
        }
    }
}

/// A structured predicate failure: the failing predicate, the field path to the
/// offending component, the offending type, and the reject reason.
///
/// This is the "diagnostic struct, not just a bool" the RUE-504 exposure
/// direction asks for. `field_path` is empty when the queried type itself is the
/// problem; otherwise it names the chain of fields from the queried aggregate
/// down to the offending field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiPredicateFailure {
    /// Which predicate failed.
    pub predicate: FfiPredicate,
    /// Field chain from the queried aggregate to the offending field.
    pub field_path: Vec<String>,
    /// The offending type.
    pub failing_type: Type,
    /// The reject-list reason.
    pub reason: FfiRejectReason,
}

impl FfiPredicateFailure {
    fn new(
        predicate: FfiPredicate,
        field_path: &[String],
        failing_type: Type,
        reason: FfiRejectReason,
    ) -> Self {
        Self {
            predicate,
            field_path: field_path.to_vec(),
            failing_type,
            reason,
        }
    }

    /// The dotted field path (empty string when the queried type itself failed).
    pub fn field_path_display(&self) -> String {
        self.field_path.join(".")
    }
}

/// The minimal pool interface the predicates need, implemented by both the
/// mutable and frozen type-intern pools so the algebra is written once.
pub trait FfiTypePool {
    /// Whether the struct carries the `@repr(c)` guarantee marker.
    fn ffi_struct_is_repr_c(&self, id: StructId) -> bool;
    /// Whether the struct is a `linear` type.
    fn ffi_struct_is_linear(&self, id: StructId) -> bool;
    /// Whether the struct has a user-defined destructor.
    fn ffi_struct_has_destructor(&self, id: StructId) -> bool;
    /// The struct's fields as `(name, type)` in declaration order.
    fn ffi_struct_fields(&self, id: StructId) -> Vec<(String, Type)>;
    /// The struct's field types in declaration order, without cloning names.
    /// Shared representation predicates use this narrower projection.
    fn ffi_struct_field_types(&self, id: StructId) -> Vec<Type> {
        self.ffi_struct_fields(id)
            .into_iter()
            .map(|(_, ty)| ty)
            .collect()
    }
    /// The element type of a fixed-size array.
    fn ffi_array_element(&self, id: ArrayTypeId) -> Type;
}

/// Whether a scalar type kind is a C-compatible scalar (integer widths, `bool`,
/// or a raw pointer). The `std.c` transparent aliases resolve to these.
fn is_c_scalar(kind: TypeKind) -> bool {
    matches!(
        kind,
        TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_)
    )
}

/// Predicate 1: does `ty` have a guaranteed C object layout?
///
/// True for C scalars, raw pointers, fixed arrays of eligible elements, and
/// nested `@repr(c)` structs (recursively). A pure boolean; use
/// [`check_c_layout`] when the failing field path is wanted.
pub fn has_c_layout<P: FfiTypePool>(pool: &P, ty: Type) -> bool {
    check_c_layout(pool, ty, &mut Vec::new()).is_ok()
}

/// The diagnostic-producing form of [`has_c_layout`]: `Ok(())` when `ty` has a
/// guaranteed C layout, otherwise the failing field path and reason.
pub fn check_c_layout<P: FfiTypePool>(
    pool: &P,
    ty: Type,
    path: &mut Vec<String>,
) -> Result<(), FfiPredicateFailure> {
    let kind = ty.kind();
    if is_c_scalar(kind) {
        return Ok(());
    }
    match kind {
        // Arrays stride by the C element size; their element must have C layout.
        TypeKind::Array(id) => check_c_layout(pool, pool.ffi_array_element(id), path),
        TypeKind::Struct(id) => {
            if !pool.ffi_struct_is_repr_c(id) {
                return Err(FfiPredicateFailure::new(
                    FfiPredicate::HasCLayout,
                    path,
                    ty,
                    FfiRejectReason::NonReprCAggregate,
                ));
            }
            let fields = pool.ffi_struct_fields(id);
            if fields.is_empty() {
                return Err(FfiPredicateFailure::new(
                    FfiPredicate::HasCLayout,
                    path,
                    ty,
                    FfiRejectReason::EmptyStruct,
                ));
            }
            for (name, field_ty) in fields {
                path.push(name);
                check_c_layout(pool, field_ty, path)?;
                path.pop();
            }
            Ok(())
        }
        TypeKind::Enum(_) => Err(FfiPredicateFailure::new(
            FfiPredicate::HasCLayout,
            path,
            ty,
            FfiRejectReason::Enum,
        )),
        _ => Err(FfiPredicateFailure::new(
            FfiPredicate::HasCLayout,
            path,
            ty,
            FfiRejectReason::UnsupportedType,
        )),
    }
}

/// Predicate 2: is `ty` structurally safe to cross the C boundary?
///
/// The hard-error policy: no linear, destructor-bearing, or ownership-bearing
/// fields (recursively). Pointee layouts are *not* required — an opaque pointer
/// to an incomplete foreign type is safe, so this does not recurse through
/// pointers. Enums are rejected as not-FFI-safe in v0.
pub fn c_ffi_safe<P: FfiTypePool>(pool: &P, ty: Type) -> Result<(), FfiPredicateFailure> {
    check_c_ffi_safe(pool, ty, &mut Vec::new())
}

fn check_c_ffi_safe<P: FfiTypePool>(
    pool: &P,
    ty: Type,
    path: &mut Vec<String>,
) -> Result<(), FfiPredicateFailure> {
    let kind = ty.kind();
    // Scalars and pointers are always safe; pointees are opaque and not read.
    if is_c_scalar(kind) {
        return Ok(());
    }
    match kind {
        TypeKind::Array(id) => check_c_ffi_safe(pool, pool.ffi_array_element(id), path),
        TypeKind::Struct(id) => {
            if pool.ffi_struct_is_linear(id) {
                return Err(FfiPredicateFailure::new(
                    FfiPredicate::CFfiSafe,
                    path,
                    ty,
                    FfiRejectReason::Linear,
                ));
            }
            if pool.ffi_struct_has_destructor(id) {
                return Err(FfiPredicateFailure::new(
                    FfiPredicate::CFfiSafe,
                    path,
                    ty,
                    FfiRejectReason::HasDestructor,
                ));
            }
            for (name, field_ty) in pool.ffi_struct_fields(id) {
                path.push(name);
                check_c_ffi_safe(pool, field_ty, path)?;
                path.pop();
            }
            Ok(())
        }
        TypeKind::Enum(_) => Err(FfiPredicateFailure::new(
            FfiPredicate::CFfiSafe,
            path,
            ty,
            FfiRejectReason::Enum,
        )),
        _ => Err(FfiPredicateFailure::new(
            FfiPredicate::CFfiSafe,
            path,
            ty,
            FfiRejectReason::UnsupportedType,
        )),
    }
}

/// Predicate 3: can `ty` cross a C boundary *by value* in the current phase?
///
/// This is the classifier seam, kept in lock-step with the
/// [`TargetCCallAbi`](crate::call_abi::TargetCCallAbi) classifier that lowers
/// each crossing. P2 (RUE-1056) widened it from the P1 register-only set to the
/// **full integer scalar set** — every signed and unsigned integer width, `bool`
/// as the 1-byte `_Bool`, and raw pointers, the scalars the target-C classifier
/// assigns to a single integer register with a defined narrow-integer extension
/// ([`ScalarAbiExtension`](crate::call_abi::ScalarAbiExtension)).
///
/// P3 (RUE-1057) widens it again to **C-classifiable aggregates**: a struct that
/// is marked `@repr(c)` and satisfies [`has_c_layout`] and [`c_ffi_safe`] (i.e.
/// is [`repr_c_marker_eligible`]) is now passable by value. The target-C
/// classifier assigns it eightbyte-wise — ≤16-byte structs pack into one or two
/// integer registers in C field order; larger structs go to memory
/// ([`AggregateArgClass`](crate::call_abi::AggregateArgClass) /
/// [`AggregateReturnClass`](crate::call_abi::AggregateReturnClass)). In the
/// integer-only core every eightbyte classifies INTEGER; a field type that would
/// classify SSE cannot exist until RUE-714 adds floats (P5).
///
/// Fixed arrays stay rejected here (`NotRegisterPassable`) — C decays an array
/// parameter to a pointer, so a by-value array is not a boundary type; it is
/// eligible only as a *struct field*. As a direct `extern` parameter/return it
/// is diagnosed earlier as `ExternArrayByValue`. Enums are not FFI-safe in v0.
pub fn c_passable_by_value<P: FfiTypePool>(pool: &P, ty: Type) -> Result<(), FfiPredicateFailure> {
    match ty.kind() {
        TypeKind::I8
        | TypeKind::U8
        | TypeKind::I16
        | TypeKind::U16
        | TypeKind::I32
        | TypeKind::U32
        | TypeKind::I64
        | TypeKind::U64
        | TypeKind::Bool
        | TypeKind::PtrConst(_)
        | TypeKind::PtrMut(_) => Ok(()),
        // A C-classifiable aggregate is passable by value iff it is a marked and
        // eligible `@repr(c)` struct (P3). `repr_c_marker_eligible` runs the
        // has_c_layout + c_ffi_safe checks and reports the failing predicate,
        // field path, and reason for a struct that is marked but not eligible.
        TypeKind::Struct(_) => repr_c_marker_eligible(pool, ty),
        _ => Err(FfiPredicateFailure::new(
            FfiPredicate::CPassableByValue,
            &[],
            ty,
            FfiRejectReason::NotRegisterPassable,
        )),
    }
}

/// Whether a `@repr(c)` struct is FFI-eligible under the reject-don't-guess list
/// (ADR-0064 Amendment 1), enforced *on the marker itself*.
///
/// A marked struct must have a guaranteed C layout ([`check_c_layout`]: no empty
/// body, no enum / non-`@repr(c)` aggregate / unsupported field) *and* be
/// structurally safe ([`c_ffi_safe`]: no linear / destructor-bearing field). The
/// C-layout check runs first so the more specific "must be `@repr(c)`" / "empty
/// struct" / "enum" reasons win over the structural ones. `struct_ty` is the
/// marked struct's own type, so a failure at the top level reports it directly.
pub fn repr_c_marker_eligible<P: FfiTypePool>(
    pool: &P,
    struct_ty: Type,
) -> Result<(), FfiPredicateFailure> {
    check_c_layout(pool, struct_ty, &mut Vec::new())?;
    c_ffi_safe(pool, struct_ty)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnumId, PtrConstTypeId, PtrMutTypeId};
    use ahash::AHashMap;

    /// A tiny hand-built pool so the predicate algebra can be exercised without
    /// the full type-intern-pool completion protocol. Struct/array IDs are just
    /// map keys; pointer/enum handles are inert (predicates never dereference a
    /// pointer's pointee or read an enum body).
    #[derive(Default)]
    struct MockPool {
        structs: AHashMap<u32, MockStruct>,
        arrays: AHashMap<u32, Type>,
    }

    #[derive(Default, Clone)]
    struct MockStruct {
        repr_c: bool,
        linear: bool,
        has_destructor: bool,
        fields: Vec<(String, Type)>,
    }

    impl MockPool {
        fn add_struct(&mut self, id: u32, s: MockStruct) -> Type {
            self.structs.insert(id, s);
            Type::new_struct(StructId::from_pool_index(id))
        }
        fn add_array(&mut self, id: u32, element: Type) -> Type {
            self.arrays.insert(id, element);
            Type::new_array(ArrayTypeId::from_pool_index(id))
        }
    }

    impl FfiTypePool for MockPool {
        fn ffi_struct_is_repr_c(&self, id: StructId) -> bool {
            self.structs[&id.0].repr_c
        }
        fn ffi_struct_is_linear(&self, id: StructId) -> bool {
            self.structs[&id.0].linear
        }
        fn ffi_struct_has_destructor(&self, id: StructId) -> bool {
            self.structs[&id.0].has_destructor
        }
        fn ffi_struct_fields(&self, id: StructId) -> Vec<(String, Type)> {
            self.structs[&id.0].fields.clone()
        }
        fn ffi_array_element(&self, id: ArrayTypeId) -> Type {
            self.arrays[&id.0]
        }
    }

    fn field(name: &str, ty: Type) -> (String, Type) {
        (name.to_string(), ty)
    }

    fn ptr() -> Type {
        Type::new_ptr_const(PtrConstTypeId::from_pool_index(0))
    }

    // --- The supported subset is FFI-safe and C-laid-out ------------------

    #[test]
    fn scalars_and_pointers_have_c_layout_and_are_safe_and_passable() {
        let pool = MockPool::default();
        for ty in [Type::I64, Type::U64, Type::I32, Type::U8, Type::BOOL, ptr()] {
            assert!(has_c_layout(&pool, ty), "{ty:?} should have C layout");
            assert!(c_ffi_safe(&pool, ty).is_ok(), "{ty:?} should be FFI-safe");
        }
        // P2 (RUE-1056) widens by-value passing to the full integer scalar set
        // plus `bool` and pointers.
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
            ptr(),
        ] {
            assert!(
                c_passable_by_value(&pool, ty).is_ok(),
                "{ty:?} should be passable by value in P2"
            );
        }
        // P3 (RUE-1057): an eligible `@repr(c)` aggregate is now passable by
        // value; the classifier assigns it eightbyte-wise.
        let mut agg_pool = MockPool::default();
        let agg = agg_pool.add_struct(
            9,
            MockStruct {
                repr_c: true,
                fields: vec![field("x", Type::I64)],
                ..Default::default()
            },
        );
        assert!(
            c_passable_by_value(&agg_pool, agg).is_ok(),
            "an eligible @repr(c) struct is passable by value in P3"
        );
        // A non-`@repr(c)` struct is still not passable — it fails the marker
        // gate (has_c_layout), reported through the eligibility check.
        let mut bad_pool = MockPool::default();
        let unmarked = bad_pool.add_struct(
            9,
            MockStruct {
                repr_c: false,
                fields: vec![field("x", Type::I64)],
                ..Default::default()
            },
        );
        let fail = c_passable_by_value(&bad_pool, unmarked).unwrap_err();
        assert_eq!(fail.predicate, FfiPredicate::HasCLayout);
        assert_eq!(fail.reason, FfiRejectReason::NonReprCAggregate);
        // A fixed array is never passable directly by value (array decay).
        let mut arr_pool = MockPool::default();
        let arr = arr_pool.add_array(0, Type::I64);
        let fail = c_passable_by_value(&arr_pool, arr).unwrap_err();
        assert_eq!(fail.predicate, FfiPredicate::CPassableByValue);
        assert_eq!(fail.reason, FfiRejectReason::NotRegisterPassable);
    }

    #[test]
    fn repr_c_struct_of_eligible_fields_is_marker_eligible() {
        let mut pool = MockPool::default();
        let inner = pool.add_struct(
            1,
            MockStruct {
                repr_c: true,
                fields: vec![field("x", Type::I64)],
                ..Default::default()
            },
        );
        let arr = pool.add_array(0, Type::U8);
        let outer = pool.add_struct(
            2,
            MockStruct {
                repr_c: true,
                fields: vec![
                    field("a", Type::I64),
                    field("p", ptr()),
                    field("buf", arr),
                    field("inner", inner),
                ],
                ..Default::default()
            },
        );
        assert!(has_c_layout(&pool, outer));
        assert!(c_ffi_safe(&pool, outer).is_ok());
        assert!(repr_c_marker_eligible(&pool, outer).is_ok());
    }

    // --- Each reject-list entry fails with the right predicate + path ------

    #[test]
    fn empty_repr_c_struct_fails_has_c_layout() {
        let mut pool = MockPool::default();
        let empty = pool.add_struct(
            1,
            MockStruct {
                repr_c: true,
                ..Default::default()
            },
        );
        let fail = repr_c_marker_eligible(&pool, empty).unwrap_err();
        assert_eq!(fail.predicate, FfiPredicate::HasCLayout);
        assert_eq!(fail.reason, FfiRejectReason::EmptyStruct);
        assert!(fail.field_path.is_empty());
    }

    #[test]
    fn enum_field_fails_with_field_path() {
        let mut pool = MockPool::default();
        let enum_ty = Type::new_enum(EnumId::from_pool_index(0));
        let outer = pool.add_struct(
            1,
            MockStruct {
                repr_c: true,
                fields: vec![field("tag", enum_ty)],
                ..Default::default()
            },
        );
        let fail = repr_c_marker_eligible(&pool, outer).unwrap_err();
        assert_eq!(fail.predicate, FfiPredicate::HasCLayout);
        assert_eq!(fail.reason, FfiRejectReason::Enum);
        assert_eq!(fail.field_path, vec!["tag".to_string()]);
        assert_eq!(fail.failing_type, enum_ty);
    }

    #[test]
    fn non_repr_c_nested_aggregate_fails_with_field_path() {
        let mut pool = MockPool::default();
        let inner = pool.add_struct(
            1,
            MockStruct {
                repr_c: false,
                fields: vec![field("x", Type::I64)],
                ..Default::default()
            },
        );
        let outer = pool.add_struct(
            2,
            MockStruct {
                repr_c: true,
                fields: vec![field("inner", inner)],
                ..Default::default()
            },
        );
        let fail = repr_c_marker_eligible(&pool, outer).unwrap_err();
        assert_eq!(fail.predicate, FfiPredicate::HasCLayout);
        assert_eq!(fail.reason, FfiRejectReason::NonReprCAggregate);
        assert_eq!(fail.field_path, vec!["inner".to_string()]);
    }

    #[test]
    fn deep_field_path_is_reported() {
        let mut pool = MockPool::default();
        let enum_ty = Type::new_enum(EnumId::from_pool_index(0));
        let mid = pool.add_struct(
            1,
            MockStruct {
                repr_c: true,
                fields: vec![field("tag", enum_ty)],
                ..Default::default()
            },
        );
        let outer = pool.add_struct(
            2,
            MockStruct {
                repr_c: true,
                fields: vec![field("mid", mid)],
                ..Default::default()
            },
        );
        let fail = repr_c_marker_eligible(&pool, outer).unwrap_err();
        assert_eq!(fail.field_path, vec!["mid".to_string(), "tag".to_string()]);
        assert_eq!(fail.field_path_display(), "mid.tag");
    }

    #[test]
    fn linear_struct_fails_c_ffi_safe() {
        let mut pool = MockPool::default();
        let lin = pool.add_struct(
            1,
            MockStruct {
                repr_c: true,
                linear: true,
                fields: vec![field("x", Type::I64)],
                ..Default::default()
            },
        );
        // has_c_layout is satisfied; c_ffi_safe is the failing predicate.
        assert!(has_c_layout(&pool, lin));
        let fail = repr_c_marker_eligible(&pool, lin).unwrap_err();
        assert_eq!(fail.predicate, FfiPredicate::CFfiSafe);
        assert_eq!(fail.reason, FfiRejectReason::Linear);
    }

    #[test]
    fn destructor_bearing_field_fails_c_ffi_safe_with_path() {
        let mut pool = MockPool::default();
        let res = pool.add_struct(
            1,
            MockStruct {
                repr_c: true,
                has_destructor: true,
                fields: vec![field("x", Type::I64)],
                ..Default::default()
            },
        );
        let outer = pool.add_struct(
            2,
            MockStruct {
                repr_c: true,
                fields: vec![field("res", res)],
                ..Default::default()
            },
        );
        let fail = repr_c_marker_eligible(&pool, outer).unwrap_err();
        assert_eq!(fail.predicate, FfiPredicate::CFfiSafe);
        assert_eq!(fail.reason, FfiRejectReason::HasDestructor);
        assert_eq!(fail.field_path, vec!["res".to_string()]);
    }

    #[test]
    fn opaque_pointer_field_does_not_recurse_into_pointee() {
        // A pointer to a non-`@repr(c)`, destructor-bearing struct is still
        // FFI-safe and C-laid-out: pointee layouts are not required.
        let mut pool = MockPool::default();
        let opaque = pool.add_struct(
            1,
            MockStruct {
                repr_c: false,
                has_destructor: true,
                fields: vec![field("x", Type::I64)],
                ..Default::default()
            },
        );
        // A dangling pointer handle whose pointee we never read.
        let _ = opaque;
        let ptr_mut = Type::new_ptr_mut(PtrMutTypeId::from_pool_index(3));
        let outer = pool.add_struct(
            2,
            MockStruct {
                repr_c: true,
                fields: vec![field("handle", ptr_mut)],
                ..Default::default()
            },
        );
        assert!(has_c_layout(&pool, outer));
        assert!(c_ffi_safe(&pool, outer).is_ok());
        assert!(repr_c_marker_eligible(&pool, outer).is_ok());
    }

    #[test]
    fn array_of_eligible_elements_has_c_layout_but_is_not_passable_by_value() {
        let mut pool = MockPool::default();
        let arr = pool.add_array(0, Type::I64);
        assert!(has_c_layout(&pool, arr));
        assert!(c_ffi_safe(&pool, arr).is_ok());
        // Arrays are eligible as fields, never passable directly by value.
        assert!(c_passable_by_value(&pool, arr).is_err());
    }
}
