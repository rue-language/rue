//! Shared type utilities for code generation backends.
//!
//! This module provides common functions for calculating type sizes and
//! field offsets, shared between x86_64 and aarch64 backends.
//!
//! Struct, enum, array, and pointer definitions are resolved through the
//! canonical `FrozenTypeInternPool` (ADR-0024).

use lasso::ThreadedRodeo;
use rue_air::{ArrayTypeId, EnumId, FrozenTypeInternPool, StructId, TypeKind};
use rue_cfg::{Cfg, CfgInstData, CfgValue, Type, ValidatedCfg};
use std::collections::HashMap;

use crate::vreg::VReg;

/// Extract the ArrayTypeId from a Type::Array.
/// Returns None if the type is not an array type.
#[inline]
pub fn extract_array_type_id(ty: Type) -> Option<ArrayTypeId> {
    match ty.kind() {
        TypeKind::Array(id) => Some(id),
        _ => None,
    }
}

/// Get the array type definition for an array type ID.
///
/// Returns `(element_type, length)`.
pub fn array_type_def(type_pool: &FrozenTypeInternPool, array_type_id: ArrayTypeId) -> (Type, u64) {
    type_pool.array_def(array_type_id)
}

/// Get the array type definition from a Type.
///
/// Returns `Some((element_type, length))` if the type is an array type, `None` otherwise.
#[inline]
pub fn array_type_def_from_type(type_pool: &FrozenTypeInternPool, ty: Type) -> Option<(Type, u64)> {
    extract_array_type_id(ty).map(|id| array_type_def(type_pool, id))
}

/// Get the length of an array from its Type.
/// Returns 0 if the type is not an array.
#[inline]
pub fn array_length_from_type(type_pool: &FrozenTypeInternPool, ty: Type) -> u64 {
    array_type_def_from_type(type_pool, ty)
        .map(|(_element_type, length)| length)
        .unwrap_or(0)
}

/// Calculate the slot count for a single element of an array from its Type.
#[inline]
pub fn array_element_slot_count_from_type(type_pool: &FrozenTypeInternPool, ty: Type) -> u32 {
    if let Some((element_type, _length)) = array_type_def_from_type(type_pool, ty) {
        type_slot_count(type_pool, element_type)
    } else {
        1
    }
}

/// Calculate the total number of slots needed to store a type.
///
/// For scalars, this is 1. For arrays, it's `length * slot_count(element_type)`.
/// For structs, this is the sum of slot counts for all fields.
/// For nested types, this recursively calculates.
/// Zero-sized types (unit, never, empty structs, zero-length arrays) return 0.
pub fn type_slot_count(type_pool: &FrozenTypeInternPool, ty: Type) -> u32 {
    type_pool.abi_slot_count(ty)
}

/// Whether `ty` needs a complete aggregate slot representation rather than a
/// single primary vreg. Discriminant-only enums remain scalar values.
pub fn is_multislot_aggregate(type_pool: &FrozenTypeInternPool, ty: Type) -> bool {
    matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_))
        || (ty.is_enum() && type_slot_count(type_pool, ty) > 1)
}

/// Return the `(Some, None)` discriminant values for an `Option`-shaped enum
/// type, i.e. the variant indices of its `Some` and `None` variants.
///
/// Used to lower the fallible intrinsics (`@read_line`, `@parse_*`), whose
/// runtime writes the caller's `Some`/`None` discriminant into the tagged-union
/// result (RUE-6, ADR-0038). Sema has already validated the result is
/// `Option`-shaped, so both variants are present; a missing one is a compiler
/// bug and panics (a correctness guard, RUE-45).
pub fn option_variant_discriminants(type_pool: &FrozenTypeInternPool, ty: Type) -> (u64, u64) {
    let enum_id = ty
        .as_enum()
        .expect("fallible intrinsic result must be an Option enum");
    let enum_def = type_pool.enum_def(enum_id);
    let some_disc = enum_def
        .find_variant("Some")
        .expect("Option result enum must have a `Some` variant") as u64;
    let none_disc = enum_def
        .find_variant("None")
        .expect("Option result enum must have a `None` variant") as u64;
    (some_disc, none_disc)
}

/// Whether comparing a slot holding `ty` needs a full 64-bit compare.
///
/// Narrow scalars (i8..i32, bool) live in 32-bit-comparable slots; 64-bit
/// integers and raw pointers must be compared at full width so two pointers or
/// i64s that differ only in their high 32 bits are not judged equal. Raw
/// pointers compare by ADDRESS (identity), which a 64-bit compare of the
/// pointer value gives directly.
pub fn slot_needs_wide_compare(ty: Type) -> bool {
    matches!(
        ty.kind(),
        TypeKind::I64 | TypeKind::U64 | TypeKind::PtrConst(_) | TypeKind::PtrMut(_)
    )
}

/// Flatten an aggregate type into the list of leaf types, one entry per storage
/// slot, in slot order. The length equals [`type_slot_count`].
///
/// This drives structural equality (RUE-285): a struct/array/payload-enum `==`
/// compares every slot pairwise, and each slot's leaf type selects the compare
/// width via [`slot_needs_wide_compare`].
///
/// Layout mirrors [`type_slot_count`]:
/// - **struct**: fields in declaration order, each recursively flattened
/// - **array**: the element leaf types repeated `length` times
/// - **payload enum**: slot 0 is the discriminant (a narrow tag), followed by
///   the payload area sized to the widest variant. For each payload slot we
///   pick a representative leaf type that is wide if *any* variant places a
///   wide leaf there, so a mixed `i64`/`i32` payload slot is compared at 64
///   bits (narrow values are zero-extended in their slot, so a wide compare of
///   a narrow value is still correct). Padding slots beyond every variant's
///   payload are zero and compare as narrow.
/// - **unit / never**: zero slots (no entries)
pub fn aggregate_leaf_types(type_pool: &FrozenTypeInternPool, ty: Type) -> Vec<Type> {
    let mut out = Vec::new();
    push_leaf_types(type_pool, ty, &mut out);
    out
}

fn push_leaf_types(type_pool: &FrozenTypeInternPool, ty: Type, out: &mut Vec<Type>) {
    match ty.kind() {
        TypeKind::Unit | TypeKind::Never => {}
        TypeKind::Struct(struct_id) => {
            let struct_def = type_pool.struct_def(struct_id);
            for field in &struct_def.fields {
                push_leaf_types(type_pool, field.ty, out);
            }
        }
        TypeKind::Array(array_id) => {
            let (element_type, length) = type_pool.array_def(array_id);
            for _ in 0..length {
                push_leaf_types(type_pool, element_type, out);
            }
        }
        TypeKind::Enum(enum_id) => {
            // Slot 0: the discriminant. Variant indices are small, and both the
            // tag and any padding are materialized as clean (zero-extended)
            // values, so a narrow compare of the tag is correct.
            out.push(Type::I32);
            // Payload area sized to the widest variant. For each slot position,
            // prefer a wide leaf type if any variant carries one there.
            let enum_def = type_pool.enum_def(enum_id);
            let mut per_slot: Vec<Type> = Vec::new();
            for v in 0..enum_def.variant_count() {
                let mut variant_leaves: Vec<Type> = Vec::new();
                for &pty in enum_def.variant_payload(v) {
                    push_leaf_types(type_pool, pty, &mut variant_leaves);
                }
                for (i, leaf) in variant_leaves.into_iter().enumerate() {
                    if i >= per_slot.len() {
                        per_slot.push(leaf);
                    } else if slot_needs_wide_compare(leaf) && !slot_needs_wide_compare(per_slot[i])
                    {
                        per_slot[i] = leaf;
                    }
                }
            }
            out.extend(per_slot);
        }
        _ => out.push(ty),
    }
}

/// Slot offset of payload field `field_index` within an enum's payload area.
///
/// The discriminant occupies slot 0, so payload fields begin at slot 1
/// (RUE-221). This is the *internal value-decomposition* offset (ADR-0052
/// representation 2), which the layout authority reports directly and keeps
/// independent of the compact physical layout so the slot-based stack/register
/// model is unchanged when compact byte offsets diverge from slot offsets
/// (RUE-975). Under the slot model it equals the physical payload byte offset
/// divided by the slot width.
pub fn enum_payload_slot_offset(
    type_pool: &FrozenTypeInternPool,
    enum_id: EnumId,
    variant_index: u32,
    field_index: u32,
) -> u32 {
    type_pool.enum_payload_slot_offset(enum_id, variant_index, field_index)
}

/// Calculate the slot count for a single element of an array type.
pub fn array_element_slot_count(
    type_pool: &FrozenTypeInternPool,
    array_type_id: ArrayTypeId,
) -> u32 {
    let (element_type, _length) = type_pool.array_def(array_type_id);
    type_slot_count(type_pool, element_type)
}

/// Calculate the size in bytes of a type from the canonical layout authority.
///
/// This is the physical byte size (used, e.g., for pointer arithmetic), as
/// opposed to [`type_slot_count`]'s internal value decomposition.
pub fn type_size_bytes(type_pool: &FrozenTypeInternPool, ty: Type) -> u64 {
    type_pool.layout(ty).size
}

/// Total slot count of a struct: the sum of every field's slot count. Equal to
/// `type_slot_count(Struct(struct_id))`, but reachable directly from a
/// `StructId` (used to anchor ascending place addressing at a struct root —
/// ADR-0040 / RUE-311).
pub fn struct_slot_count(type_pool: &FrozenTypeInternPool, struct_id: StructId) -> u32 {
    let struct_def = type_pool.struct_def(struct_id);
    let mut total = 0u32;
    for field in &struct_def.fields {
        total += type_slot_count(type_pool, field.ty);
    }
    total
}

/// Calculate the slot offset for a field within a struct.
///
/// This is the *internal value-decomposition* offset (ADR-0052 representation
/// 2), reported directly by the layout authority and kept independent of the
/// compact physical layout so code generation's slot-based stack/register model
/// is unchanged when compact byte offsets diverge from slot offsets (RUE-975).
/// Under the slot model it equals the physical field byte offset (shared with
/// `@offset_of`) divided by the slot width.
pub fn struct_field_slot_offset(
    type_pool: &FrozenTypeInternPool,
    struct_id: StructId,
    field_index: u32,
) -> u32 {
    type_pool.struct_field_slot_offset(struct_id, field_index)
}

/// Whether `ty`'s compact physical layout (ADR-0052 `aggregate_layout`) is
/// byte-for-byte identical to the flattened eight-byte slot layout.
///
/// True exactly when every leaf scalar occupies a full eight bytes, so the type
/// has no narrow fields, no interior or tail padding, and no narrowed enum tag.
/// For such a type the compact size, alignment, stride, and field/element byte
/// offsets all equal the slot-model values, so the slot-based memory
/// marshalling code generation already emits is physically correct under the
/// compact layout. When false, correct compact access requires narrow
/// (one/two/four-byte) loads and stores at compact byte offsets, which is staged
/// for a follow-up (see [`compact_physical_access_unsupported`]).
///
/// Delegates to the canonical layout authority `rue_air::is_slot_identical_layout`
/// so the compact call-ABI classifier's transitional indirect rule (RUE-976) and
/// this narrow-access refusal cannot disagree about which types the compact
/// layout leaves unchanged.
pub(crate) fn is_slot_identical_layout(type_pool: &FrozenTypeInternPool, ty: Type) -> bool {
    rue_air::is_slot_identical_layout(type_pool, ty)
}

/// Physical width (1/2/4 bytes) and extension of a narrow scalar accessed
/// through a pointer under the compact layout (ADR-0052, RUE-989).
///
/// A load extends the narrow physical value into its slot-shaped virtual
/// register — zero-extend for unsigned integers and `bool` (whose 0/1 validity
/// is preserved, ruling 1), sign-extend for signed integers — and a store
/// truncates the slot value to the narrow width. This is the explicit
/// pack/unpack at the slot-value ↔ physical-memory boundary of ADR-0052's
/// three-representation split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NarrowScalar {
    /// Physical byte width: 1, 2, or 4.
    pub width: u8,
    /// Whether a load sign-extends (`true`) or zero-extends (`false`).
    pub signed: bool,
}

/// The narrow-access description for a scalar accessed through a pointer, or
/// `None` when the value occupies a full eight-byte slot (`i64`/`u64`/pointers).
/// When `None`, the eight-byte slot-shaped load/store is byte-for-byte correct.
///
/// Only scalar leaves narrow: aggregates crossing the pointer boundary as whole
/// values remain refused by [`compact_physical_access_unsupported`].
pub(crate) fn narrow_scalar_access(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
) -> Option<NarrowScalar> {
    let (width, signed) = match ty.kind() {
        TypeKind::I8 => (1, true),
        TypeKind::U8 | TypeKind::Bool => (1, false),
        TypeKind::I16 => (2, true),
        TypeKind::U16 => (2, false),
        TypeKind::I32 => (4, true),
        TypeKind::U32 => (4, false),
        // i64/u64/pointers occupy a full slot; aggregates never take this path.
        _ => return None,
    };
    assert_eq!(
        u64::from(width),
        type_pool.layout(ty).size,
        "narrow scalar width must equal the compact physical size"
    );
    Some(NarrowScalar { width, signed })
}

/// Physical width (1/2/4/8) and load extension of a scalar leaf, independent of
/// the compact gate. Full eight-byte leaves (`i64`/`u64`/pointers) report width
/// 8; a load of them needs no narrowing. Returns `None` for aggregate,
/// zero-sized, or compile-time-only types, which have no scalar leaf.
fn scalar_physical_access(ty: Type) -> Option<(u8, bool)> {
    match ty.kind() {
        TypeKind::I8 => Some((1, true)),
        TypeKind::U8 | TypeKind::Bool => Some((1, false)),
        TypeKind::I16 => Some((2, true)),
        TypeKind::U16 => Some((2, false)),
        TypeKind::I32 => Some((4, true)),
        TypeKind::U32 => Some((4, false)),
        TypeKind::I64 | TypeKind::U64 | TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => {
            Some((8, false))
        }
        _ => None,
    }
}

/// One internal value-decomposition slot of a compact aggregate mapped onto its
/// physical byte position (ADR-0052 phase 5.6/5.7, RUE-1000/RUE-1004).
///
/// For an enum the discriminant slot maps to the narrow tag at offset 0 and every
/// payload slot to a payload-union field position; for a struct each slot maps to
/// a flattened scalar field at its compact byte offset. Each is accessed narrow
/// ([`Some`]) or full-slot ([`None`], an eight-byte leaf). Both backends consume
/// this to marshal a whole compact aggregate value across a typed pointer: stores
/// truncate each slot to its physical width, loads extend back into the
/// slot-shaped vreg. (The `Enum` in the name is historical; the map now serves
/// every aggregate with a variant-independent compact image.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalEnumSlot {
    /// Byte offset of this slot within the compact aggregate.
    pub byte_offset: i32,
    /// Narrow (1/2/4-byte) access, or `None` for a full eight-byte slot.
    pub access: Option<NarrowScalar>,
}

/// The internal-slot → physical-byte map for whole-enum memory marshalling under
/// the compact layout (ADR-0052 phase 5.6), or `None` when the enum's compact
/// memory access is *not* variant-independent and must stay refused.
///
/// A whole-enum `@ptr_write`/`@ptr_read` sees an opaque enum value (the runtime
/// variant is unknown at the access), so a single slot→byte map must be correct
/// for *every* variant. That holds only when the payload is a union of scalar
/// fields whose positions agree across variants: at each payload position every
/// variant that carries a field there places it at the same byte offset with the
/// same physical width (and the widest field's signedness), the positions do not
/// overlap, and they fit within the enum. Enums that fail any of these
/// conditions (non-scalar payloads, variant-dependent field offsets/widths, or
/// overlapping union slots) have no variant-independent memory image and are
/// kept loudly refused by [`compact_physical_access_unsupported`] rather than
/// miscompiled.
///
/// The byte offsets come straight from the canonical layout authority
/// (`layout()`'s [`rue_air::layout::LayoutKind::Enum`] tag/payload placement), so
/// the marshalling and `@size_of`/`@offset_of` agree by construction.
pub(crate) fn enum_physical_slot_map(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
) -> Option<Vec<PhysicalEnumSlot>> {
    use rue_air::layout::LayoutKind;

    let enum_id = ty.as_enum()?;
    let layout = type_pool.layout(ty);
    let (tag_size, payload_offset, variant_offsets) = match &layout.kind {
        LayoutKind::Enum {
            tag,
            payload_offset,
            variants,
        } => (tag.size, *payload_offset, variants),
        _ => return None,
    };

    // Slot 0: the smallest-sufficient unsigned tag at offset 0 (u8/u16/u32),
    // zero-extended on load.
    let tag_width = u8::try_from(tag_size).ok()?;
    if !matches!(tag_width, 1 | 2 | 4) {
        return None;
    }
    let mut slots = vec![PhysicalEnumSlot {
        byte_offset: 0,
        access: Some(NarrowScalar {
            width: tag_width,
            signed: false,
        }),
    }];

    // Payload union positions, one per flattened scalar LEAF position across all
    // variants. Each variant's payload is flattened to its scalar leaves at their
    // compact byte offsets (a struct or array payload field flattens recursively
    // via [`push_physical_slots`], mirroring the internal value decomposition of
    // [`aggregate_leaf_types`]); the leaves are then merged by position index so a
    // variant-dependent enum (e.g. `Option(Point)` or `Json`) whose per-variant
    // aggregate payloads still overlay a single, agreeing scalar image gets one
    // marshalling map (RUE-1014). Positions that disagree on byte offset, overlap,
    // or carry an imageless leaf (an imageless nested enum, or an array without a
    // compact image) yield `None` and keep the enum loudly refused.
    #[derive(Clone, Copy)]
    struct Position {
        offset: i32,
        width: u8,
        signed: bool,
    }
    let def = type_pool.enum_def(enum_id);
    let mut positions: Vec<Position> = Vec::new();
    for variant in 0..def.variant_count() {
        let payload = def.variant_payload(variant);
        // Flatten this variant's whole payload into its scalar leaves at their
        // compact byte offsets (enum-base-relative), in internal-slot order. A
        // struct/array payload field recurses; an imageless leaf bails to `None`.
        let mut leaves: Vec<PhysicalEnumSlot> = Vec::new();
        for (field_index, &field_ty) in payload.iter().enumerate() {
            let field_offset =
                i32::try_from(*variant_offsets.get(variant)?.get(field_index)?).ok()?;
            push_physical_slots(type_pool, field_ty, field_offset, &mut leaves)?;
        }
        // Merge the flattened leaves into the union positions by leaf index.
        for (leaf_index, leaf) in leaves.iter().enumerate() {
            let (width, signed) = match leaf.access {
                None => (8u8, false),
                Some(access) => (access.width, access.signed),
            };
            let offset = leaf.byte_offset;
            if let Some(pos) = positions.get_mut(leaf_index) {
                if pos.offset != offset {
                    // Variant-dependent leaf offset: not a single memory image.
                    return None;
                }
                match width.cmp(&pos.width) {
                    std::cmp::Ordering::Greater => {
                        pos.width = width;
                        pos.signed = signed;
                    }
                    // Equal widest widths must agree on signedness, or the load
                    // extension of the union slot would be ambiguous.
                    std::cmp::Ordering::Equal if pos.signed != signed => return None,
                    _ => {}
                }
            } else {
                positions.push(Position {
                    offset,
                    width,
                    signed,
                });
            }
        }
    }

    // Reject overlapping union slots and anything spilling past the enum.
    let size = i32::try_from(layout.size).ok()?;
    let mut prev_end = i32::try_from(payload_offset).ok()?;
    for pos in &positions {
        if pos.offset < prev_end || pos.offset + i32::from(pos.width) > size {
            return None;
        }
        prev_end = pos.offset + i32::from(pos.width);
        slots.push(PhysicalEnumSlot {
            byte_offset: pos.offset,
            access: if pos.width == 8 {
                None
            } else {
                Some(NarrowScalar {
                    width: pos.width,
                    signed: pos.signed,
                })
            },
        });
    }

    // The map must cover exactly the enum's internal value-decomposition slots.
    if slots.len() != type_pool.abi_slot_count(ty) as usize {
        return None;
    }
    Some(slots)
}

/// The internal-slot → physical-byte map for whole-aggregate memory marshalling
/// at a call boundary under the compact layout (ADR-0052 phase 5.7, RUE-1004),
/// or `None` when the aggregate has no variant-independent compact memory image
/// and must stay refused.
///
/// This is the single authority the call-boundary marshalling consults so the
/// caller-side compact image write, the callee-side unmarshalling, and the
/// sret/return image agree by construction. It flattens the aggregate to its
/// internal value-decomposition slots in the same order
/// [`aggregate_leaf_types`]/[`require_aggregate_slots`] use, giving each slot its
/// compact byte offset from the canonical layout authority:
///
/// - **struct**: each field is placed at its layout byte offset and recursively
///   flattened; a scalar field is one slot, a nested struct/enum contributes its
///   own sub-map shifted by the field offset;
/// - **enum**: delegated to [`enum_physical_slot_map`] (the variant-independent
///   tag+payload image);
/// - **array**: `None` — a fixed array's compact stride differs from the slot
///   stride and array call marshalling is not implemented (kept refused);
/// - **scalar / unit**: a single slot (or none), for the recursive cases.
///
/// Returns `None` (stay refused) for any aggregate containing a construct without
/// a variant-independent image (an array, or an enum [`enum_physical_slot_map`]
/// rejects), and asserts the flattened slot count equals the type's
/// [`type_slot_count`].
pub(crate) fn aggregate_physical_slot_map(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
) -> Option<Vec<PhysicalEnumSlot>> {
    let mut slots = Vec::new();
    push_physical_slots(type_pool, ty, 0, &mut slots)?;
    if slots.len() != type_pool.abi_slot_count(ty) as usize {
        return None;
    }
    Some(slots)
}

/// Append the compact physical slots of `ty` (each offset by `base_offset`) to
/// `out`, in internal value-decomposition slot order. Returns `None` when any
/// leaf lacks a variant-independent compact image (see
/// [`aggregate_physical_slot_map`]).
fn push_physical_slots(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    base_offset: i32,
    out: &mut Vec<PhysicalEnumSlot>,
) -> Option<()> {
    use rue_air::layout::LayoutKind;
    match ty.kind() {
        TypeKind::Unit | TypeKind::Never => Some(()),
        TypeKind::Struct(struct_id) => {
            let layout = type_pool.layout(ty);
            let LayoutKind::Struct { field_offsets, .. } = &layout.kind else {
                return None;
            };
            let def = type_pool.struct_def(struct_id);
            for (index, field) in def.fields.iter().enumerate() {
                let field_offset = i32::try_from(*field_offsets.get(index)?).ok()?;
                push_physical_slots(type_pool, field.ty, base_offset + field_offset, out)?;
            }
            Some(())
        }
        TypeKind::Enum(_) => {
            for slot in enum_physical_slot_map(type_pool, ty)? {
                out.push(PhysicalEnumSlot {
                    byte_offset: base_offset + slot.byte_offset,
                    access: slot.access,
                });
            }
            Some(())
        }
        // A fixed array flattens to its `count` elements laid out at the compact
        // element stride, each recursively flattened (RUE-1014). The internal
        // slot order matches [`aggregate_leaf_types`] (element leaves repeated per
        // element). An element without a compact image bails to `None`.
        TypeKind::Array(array_id) => {
            let (element, count) = type_pool.array_def(array_id);
            let stride = i32::try_from(type_pool.layout(element).stride).ok()?;
            for k in 0..count {
                let element_base =
                    base_offset.checked_add(i32::try_from(k).ok()?.checked_mul(stride)?)?;
                push_physical_slots(type_pool, element, element_base, out)?;
            }
            Some(())
        }
        _ => {
            let (width, signed) = scalar_physical_access(ty)?;
            out.push(PhysicalEnumSlot {
                byte_offset: base_offset,
                access: (width != 8).then_some(NarrowScalar { width, signed }),
            });
            Some(())
        }
    }
}

/// The internal-slot → physical-byte image used to marshal a whole aggregate
/// pointee through a typed pointer. An enum uses its variant-independent image
/// (RUE-1000); a **struct** uses its compact image (RUE-987) only when it is NOT
/// slot-identical — a slot-identical struct keeps the byte-identical full-slot
/// marshalling path. Scalars (handled by narrow access) and arrays (no
/// variant-independent compact image) return `None`.
pub(crate) fn pointer_image_slot_map(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
) -> Option<Vec<PhysicalEnumSlot>> {
    match ty.kind() {
        TypeKind::Enum(_) => enum_physical_slot_map(type_pool, ty),
        // A compact (non-slot-identical) struct or array marshals its whole value
        // through the pointer via its internal-slot → physical-byte image (RUE-987
        // struct, RUE-1014 array). A slot-identical aggregate keeps the
        // byte-identical full-slot path; one without a compact image (an imageless
        // enum leaf) returns `None` and stays refused.
        TypeKind::Struct(_) | TypeKind::Array(_) if !is_slot_identical_layout(type_pool, ty) => {
            aggregate_physical_slot_map(type_pool, ty)
        }
        _ => None,
    }
}

/// One segment of a heterogeneous aggregate's compact memory image, mapping the
/// aggregate's internal value-decomposition slots onto physical bytes with a
/// runtime tag dispatch where the payload layout is variant-dependent (ADR-0052
/// phase 5.12, RUE-1037).
///
/// A [`Leaf`](ImageSeg::Leaf) is a single value slot at a fixed byte position
/// (exactly a [`PhysicalEnumSlot`], as in the variant-independent maps). A
/// [`Switch`](ImageSeg::Switch) is a nested enum whose per-variant payload
/// layouts disagree: the discriminant is read from `tag_slot`/`tag_offset` and
/// selects one arm's segments. This is the general form the flatten-and-merge
/// image (`enum_physical_slot_map`) collapses to when every variant agrees; when
/// they disagree the merge returns `None` and this per-variant form is used
/// instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSeg {
    /// A scalar leaf: value slot `slot` lives at `phys.byte_offset` accessed at
    /// `phys.access` width. Present for every non-enum leaf and for a nested
    /// enum whose payload is variant-independent (it flattens to leaves).
    Leaf { slot: u32, phys: PhysicalEnumSlot },
    /// A tag-dispatched enum region whose per-variant payload layouts disagree.
    Switch(SwitchRegion),
}

/// A tag-dispatched enum region within a compact memory image (RUE-1037). The
/// discriminant at `tag_slot` (byte `tag_offset`, accessed `tag_access`) selects
/// one arm; the region occupies value slots `tag_slot .. tag_slot + span` (the
/// enum's internal slot count), of which `tag_slot` is the tag and the rest are
/// the payload union. Slots the active variant does not use are zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchRegion {
    pub tag_slot: u32,
    pub tag_offset: i32,
    pub tag_access: NarrowScalar,
    /// Total value slots the enum occupies (tag + widest payload union).
    pub span: u32,
    pub arms: Vec<SwitchArm>,
}

/// One variant arm of a [`SwitchRegion`]: the runtime `discriminant` (the
/// variant index) and the image segments for that variant's payload, referencing
/// the payload's value slots (`tag_slot + 1 ..`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchArm {
    pub discriminant: u64,
    pub segs: Vec<ImageSeg>,
}

/// A whole aggregate's compact memory image expressed as tag-dispatched segments
/// (RUE-1037). Used for a heterogeneous enum — or any aggregate containing one —
/// whose variants' payload layouts disagree, so no single variant-independent map
/// (`aggregate_physical_slot_map`) exists. The value decomposition is still
/// variant-independent (`total_slots` slots); only the physical marshalling
/// dispatches on the runtime tag at each `Switch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchImage {
    /// The aggregate's internal value-decomposition slot count (`abi_slot_count`).
    pub total_slots: u32,
    /// The compact image byte size, used for full-extent zeroing on a store so
    /// every byte outside the active variant's leaves is deterministically zero
    /// (ADR-0052 ruling 5 / RUE-1007).
    pub image_size: u32,
    pub segs: Vec<ImageSeg>,
}

/// Build the tag-dispatched compact memory image for `ty` (RUE-1037), or `None`
/// when `ty` needs no dispatch — a slot-identical aggregate, or one whose compact
/// image is already variant-independent (`aggregate_physical_slot_map` is
/// `Some`), both of which use their existing single-map / full-slot paths
/// unchanged.
///
/// Returns `Some` only when the aggregate genuinely requires a per-variant tag
/// dispatch, i.e. it contains at least one heterogeneous enum whose per-variant
/// payload layouts disagree. Every scalar/pointer/struct/array/enum leaf marshals
/// (each variant's own layout is a valid compact image), so an enum — or an
/// aggregate containing one — is no longer refused for heterogeneity.
pub(crate) fn aggregate_dispatch_image(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
) -> Option<DispatchImage> {
    // A variant-independent image (or a slot-identical aggregate) needs no
    // dispatch; those keep their existing single-map / full-slot paths.
    if aggregate_physical_slot_map(type_pool, ty).is_some() {
        return None;
    }
    let mut segs = Vec::new();
    let mut cursor = 0u32;
    push_image_segments(type_pool, ty, 0, &mut cursor, &mut segs)?;
    if cursor != type_pool.abi_slot_count(ty) {
        return None;
    }
    // Only a genuine tag dispatch belongs here; a switch-free flattening is a
    // full-slot / slot-identical case handled elsewhere and must not divert.
    if !segs.iter().any(seg_has_switch) {
        return None;
    }
    let image_size = u32::try_from(type_pool.layout(ty).size).ok()?;
    Some(DispatchImage {
        total_slots: cursor,
        image_size,
        segs,
    })
}

fn seg_has_switch(seg: &ImageSeg) -> bool {
    match seg {
        ImageSeg::Leaf { .. } => false,
        ImageSeg::Switch(_) => true,
    }
}

/// Append the compact image segments of `ty` (byte-offset by `base_offset`) to
/// `out`, advancing `slot_cursor` over the value slots consumed, in internal
/// value-decomposition slot order. A variant-independent enum flattens to leaves
/// (via [`enum_physical_slot_map`]); a heterogeneous enum becomes a
/// [`SwitchRegion`]. Returns `None` for a genuinely imageless leaf (an unusable
/// tag width — no real enum has one).
fn push_image_segments(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    base_offset: i32,
    slot_cursor: &mut u32,
    out: &mut Vec<ImageSeg>,
) -> Option<()> {
    use rue_air::layout::LayoutKind;
    match ty.kind() {
        TypeKind::Unit | TypeKind::Never => Some(()),
        TypeKind::Struct(struct_id) => {
            let layout = type_pool.layout(ty);
            let LayoutKind::Struct { field_offsets, .. } = &layout.kind else {
                return None;
            };
            let def = type_pool.struct_def(struct_id);
            for (index, field) in def.fields.iter().enumerate() {
                let field_offset = i32::try_from(*field_offsets.get(index)?).ok()?;
                push_image_segments(
                    type_pool,
                    field.ty,
                    base_offset + field_offset,
                    slot_cursor,
                    out,
                )?;
            }
            Some(())
        }
        TypeKind::Array(array_id) => {
            let (element, count) = type_pool.array_def(array_id);
            let stride = i32::try_from(type_pool.layout(element).stride).ok()?;
            for k in 0..count {
                let element_base =
                    base_offset.checked_add(i32::try_from(k).ok()?.checked_mul(stride)?)?;
                push_image_segments(type_pool, element, element_base, slot_cursor, out)?;
            }
            Some(())
        }
        TypeKind::Enum(enum_id) => {
            // A variant-independent enum keeps its flat leaves (no dispatch), so a
            // homogeneous nested enum in a heterogeneous aggregate stays cheap.
            if let Some(map) = enum_physical_slot_map(type_pool, ty) {
                for slot in map {
                    out.push(ImageSeg::Leaf {
                        slot: *slot_cursor,
                        phys: PhysicalEnumSlot {
                            byte_offset: base_offset + slot.byte_offset,
                            access: slot.access,
                        },
                    });
                    *slot_cursor += 1;
                }
                return Some(());
            }

            // Heterogeneous: dispatch on the tag. The tag is slot 0 of the enum at
            // its base byte offset (RUE-1000); every variant's payload begins at
            // the next value slot (matching the value decomposition, which places
            // payload fields at slots 1.. for every variant).
            let layout = type_pool.layout(ty);
            let LayoutKind::Enum {
                tag,
                variants: variant_offsets,
                ..
            } = &layout.kind
            else {
                return None;
            };
            let tag_width = u8::try_from(tag.size).ok()?;
            if !matches!(tag_width, 1 | 2 | 4) {
                return None;
            }
            let span = type_pool.abi_slot_count(ty);
            let tag_slot = *slot_cursor;
            let def = type_pool.enum_def(enum_id);
            let mut arms = Vec::with_capacity(def.variant_count() as usize);
            for variant in 0..def.variant_count() {
                let payload = def.variant_payload(variant);
                let mut arm_cursor = tag_slot + 1;
                let mut arm_segs = Vec::new();
                for (field_index, &field_ty) in payload.iter().enumerate() {
                    let field_offset =
                        i32::try_from(*variant_offsets.get(variant as usize)?.get(field_index)?)
                            .ok()?;
                    push_image_segments(
                        type_pool,
                        field_ty,
                        base_offset + field_offset,
                        &mut arm_cursor,
                        &mut arm_segs,
                    )?;
                }
                // A variant's payload must fit within its own value-slot region.
                if arm_cursor > tag_slot + span {
                    return None;
                }
                arms.push(SwitchArm {
                    discriminant: variant as u64,
                    segs: arm_segs,
                });
            }
            out.push(ImageSeg::Switch(SwitchRegion {
                tag_slot,
                tag_offset: base_offset,
                tag_access: NarrowScalar {
                    width: tag_width,
                    signed: false,
                },
                span,
                arms,
            }));
            *slot_cursor = tag_slot + span;
            Some(())
        }
        _ => {
            let (width, signed) = scalar_physical_access(ty)?;
            out.push(ImageSeg::Leaf {
                slot: *slot_cursor,
                phys: PhysicalEnumSlot {
                    byte_offset: base_offset,
                    access: (width != 8).then_some(NarrowScalar { width, signed }),
                },
            });
            *slot_cursor += 1;
            Some(())
        }
    }
}

/// The pointee of a pointer type, or the type itself when it is not a pointer.
fn pointee_or_self(type_pool: &FrozenTypeInternPool, ty: Type) -> Type {
    match ty.kind() {
        TypeKind::PtrConst(id) => type_pool.ptr_const_def(id),
        TypeKind::PtrMut(id) => type_pool.ptr_mut_def(id),
        _ => ty,
    }
}

/// If the compact physical layout is active and this CFG would require physical
/// memory access that code generation does not yet implement, return the first
/// offending type so the backend can fail loudly rather than emit slot-shaped
/// access against a compact layout (ADR-0052, RUE-974 refusal narrowed by
/// RUE-989).
///
/// RUE-989 implements narrow (one/two/four-byte) **scalar** access through typed
/// pointers with the correct zero/sign extension, so a narrow scalar
/// `@ptr_read`/`@ptr_write`/`@alloc`/`@free`/`@realloc` (including a scalar
/// struct field reached with `@field_ptr` or a scalar heap element reached with
/// `@ptr_offset`) now compiles and executes. A non-slot-identical **struct**
/// value may also exist in the slot-based frame, because its field access uses
/// static slot offsets and its scalar fields are reached with narrow access.
///
/// The scan still refuses what remains unimplemented, always consulting the
/// canonical [`is_slot_identical_layout`] authority rather than a parallel local
/// judgment:
///
/// - a non-slot-identical **array** value (dynamic array indexing strides by the
///   compact element size while the slot-based frame stores elements at the slot
///   stride). A non-slot-identical **struct** or **enum** value may exist in the
///   frame — a struct uses static slot-offset field access and a payload enum
///   keeps its slot-based value decomposition (RUE-975) — so only the boundary
///   checks below constrain them;
/// - a **pointer to a struct/array** that is non-slot-identical, or to an **enum
///   without a variant-independent memory image** (whole-aggregate marshalling
///   through a pointer is unimplemented for those, and frame-versus-heap
///   provenance is ambiguous). A compact enum *with* such an image (RUE-1000) is
///   allowed through pointers, including `@alloc`;
/// - a **by-reference non-slot-identical aggregate** parameter;
/// - a non-slot-identical aggregate that **crosses a call boundary** as an
///   argument or as the **return value** (RUE-976's transitional indirect rule;
///   enums included — call marshalling is the next phase).
///
/// Purely compile-time layout queries (`@size_of`/`@align_of`/`@offset_of`) fold
/// to constants before code generation. Slot-identical aggregates (all
/// eight-byte leaves, e.g. `StrBuf`) marshal correctly, as does the
/// byte-addressed raw-byte family (`@byte_read`/`@byte_write` over an
/// `@alloc`'d block).
///
/// Returning `None` never weakens correctness: every remaining physical access
/// is either byte-for-byte identical to the slot model, a narrow scalar access
/// (RUE-989), or a compact enum marshalled through its variant-independent memory
/// image (RUE-1000).
pub(crate) fn compact_physical_access_unsupported(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
) -> Option<Type> {
    // A non-slot-identical aggregate (struct/array/enum) whose whole-value
    // marshalling across a CALL boundary is still unimplemented (RUE-976's
    // transitional indirect rule; call marshalling is the next phase). Narrow
    // SCALAR access through typed pointers IS implemented, so scalars never
    // trigger a refusal here.
    let unimplemented_aggregate = |ty: Type| -> bool {
        matches!(
            ty.kind(),
            TypeKind::Struct(_) | TypeKind::Array(_) | TypeKind::Enum(_)
        ) && !is_slot_identical_layout(type_pool, ty)
    };

    // A non-slot-identical aggregate crossing a CALL boundary (return via sret,
    // by-value argument, or by-reference parameter) that has NO variant-
    // independent compact memory image, so the call marshalling cannot build or
    // consume one (RUE-1004). Aggregates WITH an image are marshalled through a
    // caller-owned compact buffer and are allowed; arrays and enums without an
    // image stay refused.
    let call_boundary_unsupported = |ty: Type| -> bool {
        unimplemented_aggregate(ty)
            && aggregate_physical_slot_map(type_pool, ty).is_none()
            && aggregate_dispatch_image(type_pool, ty).is_none()
    };

    // A non-slot-identical aggregate whose whole-value marshalling THROUGH A
    // TYPED POINTER (memory) is unimplemented. RUE-1000 implements compact enum
    // memory for enums with a variant-independent slot→byte image (see
    // [`enum_physical_slot_map`]), so those enums are no longer refused here;
    // structs and arrays, and enums without such an image, still are.
    let unimplemented_through_pointer = |ty: Type| -> bool {
        match ty.kind() {
            TypeKind::Enum(_) => {
                enum_physical_slot_map(type_pool, ty).is_none()
                    && aggregate_dispatch_image(type_pool, ty).is_none()
            }
            // A compact (non-slot-identical) struct or array WITH a variant-
            // independent memory image marshals its whole value through the pointer
            // via its internal-slot → physical-byte map (RUE-987 struct, RUE-1014
            // array), exactly like a compact enum (RUE-1000). A slot-identical
            // aggregate keeps the byte-identical full-slot path; one WITHOUT an
            // image (it contains an imageless enum) stays refused.
            TypeKind::Struct(_) | TypeKind::Array(_) => {
                !is_slot_identical_layout(type_pool, ty)
                    && aggregate_physical_slot_map(type_pool, ty).is_none()
                    && aggregate_dispatch_image(type_pool, ty).is_none()
            }
            _ => false,
        }
    };

    // A returned aggregate is written to the caller's sret buffer as its compact
    // memory image (RUE-1004); refused only when it has no variant-independent
    // image (an array, or an enum without one).
    if call_boundary_unsupported(cfg.return_type()) {
        return Some(cfg.return_type());
    }

    for raw in 0..cfg.value_count() {
        let value = CfgValue::from_raw(raw as u32);
        let inst = cfg.get_inst(value);

        // A non-slot-identical ARRAY, STRUCT, or ENUM value may exist in the
        // frame. Array `[]` indexing strides by the *slot* stride
        // (`abi_slot_count(element) * SLOT_BYTES`, RUE-1014) because the frame
        // stores every array element slot-shaped (RUE-975), so element addressing
        // is physically correct; a struct uses static slot-offset field access and
        // narrow scalar field pointers; an enum keeps its slot-based value
        // decomposition (discriminant slot + payload slots) untouched by the
        // compact layout. The boundary checks below still refuse an aggregate
        // crossing a pointer, call, return, or by-ref boundary when it has no
        // variant-independent compact memory image.

        // A pointer to a non-slot-identical aggregate: whole-aggregate
        // marshalling through a pointer is unimplemented (except a compact enum
        // with a variant-independent memory image, RUE-1000) and the pointer's
        // frame-versus-heap provenance is otherwise ambiguous.
        if matches!(inst.ty.kind(), TypeKind::PtrConst(_) | TypeKind::PtrMut(_)) {
            let pointee = pointee_or_self(type_pool, inst.ty);
            if unimplemented_through_pointer(pointee) {
                return Some(pointee);
            }
        }

        match &inst.data {
            // A by-reference aggregate parameter is a pointer into the caller's
            // slot-based frame; whole-aggregate compact access is unimplemented.
            // A by-reference narrow scalar reads/writes its eight-byte slot
            // through the pointer and is correct, so scalars are allowed.
            // A by-reference (`inout`/`borrow`) aggregate parameter is a pointer
            // into the caller's slot-based frame storage (RUE-975 keeps a struct
            // frame value slot-shaped, and a pointer to a compact heap aggregate
            // cannot exist under the gate — see the pointer check above), so its
            // whole-value access through the pointer is the existing slot-shaped
            // by-ref path and is physically correct. Allowed for aggregates with a
            // variant-independent compact image (RUE-1004); arrays and imageless
            // enums stay refused.
            CfgInstData::Param { index } => {
                if cfg.is_param_by_ref(*index) && call_boundary_unsupported(inst.ty) {
                    return Some(inst.ty);
                }
            }
            // An aggregate crossing a call boundary. A by-reference (`inout` /
            // `borrow`) argument uses the same slot-shaped by-ref transport as a
            // by-ref parameter (RUE-1004). A by-value non-slot-identical aggregate
            // crosses indirectly by the classifier (RUE-976): the caller writes
            // its compact image to a caller-owned buffer and passes one pointer,
            // and the callee prologue homes that pointer and unmarshals the image
            // (RUE-1005, per-source-parameter ABI descriptors now plumbed). Both
            // are allowed exactly when the aggregate has a variant-independent
            // compact image; arrays and imageless enums stay refused.
            CfgInstData::Call { .. } => {
                for arg in cfg.get_call_args(&inst.data) {
                    let arg_ty = cfg.get_inst(arg.value).ty;
                    if call_boundary_unsupported(arg_ty) {
                        return Some(arg_ty);
                    }
                }
            }
            // Typed pointer reads/writes and typed allocation address memory by
            // the pointee's physical layout. A narrow scalar pointee is lowered
            // correctly (RUE-989); a compact enum pointee with a
            // variant-independent memory image is marshalled slot by slot
            // (RUE-1000); a struct/array (or intractable enum) pointee is
            // refused. The byte-addressed raw-byte family is not matched here —
            // it is correct under any layout.
            CfgInstData::Intrinsic { name, .. } => {
                let op = interner.resolve(name);
                if matches!(op, "ptr_read" | "ptr_write" | "alloc" | "free" | "realloc") {
                    let mut relevant = vec![inst.ty];
                    for &arg in cfg.get_intrinsic_args(&inst.data) {
                        relevant.push(cfg.get_inst(arg).ty);
                    }
                    for ty in relevant {
                        let pointee = pointee_or_self(type_pool, ty);
                        if unimplemented_through_pointer(pointee) {
                            return Some(pointee);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// An `@raw`/`@raw_mut`/`@field_ptr` pointer into a
/// **frame** aggregate that would then be accessed as a whole non-slot-identical
/// aggregate, or strided across a non-slot-identical array, is unsupported and
/// must be refused loudly (RUE-1035 M2).
///
/// `@raw`-family intrinsics take the address of a place, whose base is always a
/// frame local or a parameter (never a heap block — a `Place` base is only
/// `Local`/`Param`). Frames stay slot-shaped (RUE-975): a struct/array/enum sits
/// one eight-byte slot per leaf. But `@ptr_read`/`@ptr_write` address a whole
/// aggregate by its packed *compact image*, and `@ptr_offset` strides by the
/// compact element size. For a non-slot-identical aggregate the two disagree, so
/// the raw pointer silently mixes representations. Exactly two shapes break:
///
/// - the pointer's **pointee is a non-slot-identical aggregate** (`@raw_mut(v)`
///   of a whole struct/array): a whole-value round trip through it scrambles
///   fields (M2a); and
/// - the addressed place **indexes into a non-slot-identical array**
///   (`@raw(arr[i])` of a frame array element): the resulting element pointer
///   strides by the compact size while the frame array strides by the slot size
///   (M2b).
///
/// Everything else stays allowed and correct: a **narrow scalar field** pointer
/// (`@field_ptr(p.b)` → `ptr i32`) addresses the field's slot and hits its low
/// bytes (RUE-989); a **bare scalar local** (`@raw_mut(n)` on `i32`) is not an
/// aggregate; a **slot-identical** array/struct strides and round-trips
/// identically to the slot model. Heap provenance is untouched: `@alloc` never
/// flows through `@raw`, so every heap `@ptr_read`/`@ptr_write` round trip keeps
/// working. Returns the offending frame aggregate type, or `None`.
fn frame_raw_aggregate_pointer_unsupported(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
) -> Option<Type> {
    let non_slot_identical_aggregate = |ty: Type| -> bool {
        matches!(
            ty.kind(),
            TypeKind::Struct(_) | TypeKind::Array(_) | TypeKind::Enum(_)
        ) && !is_slot_identical_layout(type_pool, ty)
    };

    for raw in 0..cfg.value_count() {
        let value = CfgValue::from_raw(raw as u32);
        let inst = cfg.get_inst(value);
        let CfgInstData::Intrinsic { name, .. } = &inst.data else {
            continue;
        };
        if !matches!(interner.resolve(name), "raw" | "raw_mut" | "field_ptr") {
            continue;
        }

        // (a) The pointer addresses a whole non-slot-identical aggregate: reading
        // or writing it through the pointer marshals the compact image against
        // slot-shaped frame storage (M2a). A narrow scalar field pointer has a
        // scalar pointee and is not caught here (RUE-989 handles it).
        let pointee = pointee_or_self(type_pool, inst.ty);
        if non_slot_identical_aggregate(pointee) {
            return Some(pointee);
        }

        // (b) The addressed place indexes into a non-slot-identical frame array:
        // the element pointer would stride by the compact element size while the
        // frame array strides by the slot size (M2b). A field projection into a
        // struct is not an array stride and stays allowed.
        let Some(&operand) = cfg.get_intrinsic_args(&inst.data).first() else {
            continue;
        };
        if let CfgInstData::PlaceRead { place } = &cfg.get_inst(operand).data {
            for projection in cfg.get_place_projections(place) {
                if let rue_cfg::Projection::Index { array_type, .. } = projection
                    && non_slot_identical_aggregate(*array_type)
                {
                    return Some(*array_type);
                }
            }
        }
    }

    None
}

/// Refuse code generation with a clear diagnostic when the function marshals a
/// compact aggregate that has no single physical memory image (see
/// [`compact_physical_access_unsupported`]). Both backends call this at the top
/// of lowering so an unsupported construct fails loudly rather than being
/// silently miscompiled.
pub(crate) fn ensure_compact_layout_codegen_supported(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
) -> rue_error::CompileResult<()> {
    // A raw pointer into a non-slot-identical frame aggregate mixes the frame's
    // slot layout with compact-image pointer semantics (RUE-1035 M2). Name the
    // construct — not the preview — and point at the working heap alternative.
    if let Some(ty) = frame_raw_aggregate_pointer_unsupported(cfg, type_pool, interner) {
        let name = rue_air::drop_glue_names::type_name(ty, type_pool);
        return Err(rue_error::CompileError::without_span(
            rue_error::ErrorKind::InternalCodegenError(format!(
                "taking a raw pointer with `@raw`/`@raw_mut`/`@field_ptr` into the \
                 frame-resident aggregate `{name}` (in function `{}`) is not supported: the \
                 stack frame stores the aggregate slot-shaped (one eight-byte slot per leaf), \
                 while `@ptr_read`/`@ptr_write`/`@ptr_offset` address memory by its packed \
                 compact image, so the pointer would stride across mismatched field and element \
                 layouts. Allocate the aggregate on the heap with `@alloc`, whose storage is the \
                 compact image, to round-trip it through a raw pointer.",
                cfg.fn_name()
            )),
        ));
    }

    if let Some(ty) = compact_physical_access_unsupported(cfg, type_pool, interner) {
        let name = rue_air::drop_glue_names::type_name(ty, type_pool);
        return Err(rue_error::CompileError::without_span(
            rue_error::ErrorKind::InternalCodegenError(format!(
                "the aggregate `{name}` (in function `{}`) has no compact memory image, so it \
                 cannot be marshalled through a pointer or across a call boundary. Narrow scalar \
                 access through typed pointers (RUE-989), compact enum memory (RUE-1000), \
                 call-boundary marshalling of a compact aggregate through its memory image — sret \
                 returns and `inout`/`borrow` parameters (RUE-1004), by-value arguments (RUE-1005) \
                 — whole compact-struct marshalling through typed pointers (RUE-987), \
                 variant-dependent enum images plus compact arrays through pointers and across \
                 calls (RUE-1014), and heterogeneous enums whose per-variant payload layouts \
                 disagree via per-variant tag dispatch (RUE-1037) all marshal, but this aggregate \
                 reduces to neither a variant-independent compact image nor a tag-dispatched one, \
                 so it is refused.",
                cfg.fn_name()
            )),
        ));
    }
    Ok(())
}

/// Recursively collect all scalar vregs from an array value.
///
/// For nested arrays, this flattens them to a list of scalar vregs.
/// This is used during code generation to handle array arguments that need
/// to be passed in registers or stored to memory slot by slot.
///
/// # Arguments
/// * `cfg` - The control flow graph containing the instructions
/// * `struct_slot_vregs` - Cache mapping CFG values to their slot vregs
/// * `value` - The CFG value to collect vregs from
/// * `get_vreg` - Closure to get/allocate a vreg for a given CFG value
pub fn collect_array_scalar_vregs(
    cfg: &ValidatedCfg,
    struct_slot_vregs: &HashMap<CfgValue, Vec<VReg>>,
    value: CfgValue,
    get_vreg: &mut impl FnMut(CfgValue) -> VReg,
) -> Vec<VReg> {
    let inst = cfg.get_inst(value);
    match &inst.data {
        CfgInstData::ArrayInit { .. } => {
            let elements = cfg.get_array_elements(&inst.data);
            let mut result = Vec::new();
            // Collect elements in logical order (element 0 first), matching
            // The slot cache is uniform ascending order and
            // the ascending physical layout is produced at store time (ADR-0040
            // / RUE-311).
            for elem in elements.iter() {
                let elem_inst = cfg.get_inst(*elem);
                if elem_inst.ty.is_array() {
                    // Recursively collect from nested array
                    result.extend(collect_array_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *elem,
                        get_vreg,
                    ));
                } else if elem_inst.ty.is_struct() {
                    // Recursively collect from struct element (includes builtin String)
                    result.extend(collect_struct_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *elem,
                        get_vreg,
                    ));
                } else {
                    // Scalar element - get its vreg
                    result.push(get_vreg(*elem));
                }
            }
            result
        }
        _ => {
            // For non-ArrayInit sources, try struct_slot_vregs cache
            if let Some(vregs) = struct_slot_vregs.get(&value).cloned() {
                vregs
            } else {
                vec![get_vreg(value)]
            }
        }
    }
}

// The drop-glue array symbol name comes from the single authority in
// `rue_air::drop_glue_names`, shared with compiler glue synthesis and the other
// backend, so the external ABI cannot drift (RUE-796). Re-exported for the
// existing `crate::types::array_drop_glue_name` call sites.
pub use rue_air::drop_glue_names::array_drop_glue_name;

/// Recursively collect all scalar vregs from a struct value.
///
/// This flattens any array fields to their scalar elements.
/// This is used during code generation to handle struct arguments that need
/// to be passed in registers or stored to memory slot by slot.
///
/// # Arguments
/// * `cfg` - The control flow graph containing the instructions
/// * `struct_slot_vregs` - Cache mapping CFG values to their slot vregs
/// * `value` - The CFG value to collect vregs from
/// * `get_vreg` - Closure to get/allocate a vreg for a given CFG value
pub fn collect_struct_scalar_vregs(
    cfg: &ValidatedCfg,
    struct_slot_vregs: &HashMap<CfgValue, Vec<VReg>>,
    value: CfgValue,
    get_vreg: &mut impl FnMut(CfgValue) -> VReg,
) -> Vec<VReg> {
    let inst = cfg.get_inst(value);
    match &inst.data {
        CfgInstData::StructInit { .. } => {
            let fields = cfg.get_struct_fields(&inst.data);
            let mut result = Vec::new();
            for field in fields {
                let field_inst = cfg.get_inst(*field);
                if field_inst.ty.is_array() {
                    // Recursively collect from array field
                    result.extend(collect_array_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *field,
                        get_vreg,
                    ));
                } else if field_inst.ty.is_struct() {
                    // Recursively collect from nested struct field (includes builtin String)
                    result.extend(collect_struct_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *field,
                        get_vreg,
                    ));
                } else {
                    // Scalar field - get its vreg
                    result.push(get_vreg(*field));
                }
            }
            result
        }
        _ => {
            // For non-StructInit sources, try struct_slot_vregs cache
            if let Some(vregs) = struct_slot_vregs.get(&value).cloned() {
                vregs
            } else {
                vec![get_vreg(value)]
            }
        }
    }
}

#[cfg(test)]
mod layout_authority_tests {
    use rue_air::Sema;
    use rue_air::layout::LayoutKind;
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    use super::{struct_field_slot_offset, type_size_bytes, type_slot_count};

    /// The compact layout authority and code generation's size helper agree, and
    /// the physical field offsets and the internal slot decomposition are each
    /// self-consistent (ascending, in bounds) across every type in a real
    /// compiled program. The physical byte offsets and the slot indices are
    /// deliberately distinct representations (ADR-0052), so they are checked for
    /// their own invariants rather than for equality.
    #[test]
    fn layout_agrees_with_codegen_helpers_over_a_compiled_fixture() {
        let source = r#"
            struct Point { x: i32, y: i64 }
            struct Outer { tag: bool, points: [Point; 3] }
            fn main() -> i32 {
                let o = Outer {
                    tag: true,
                    points: [
                        Point { x: 1, y: 2 },
                        Point { x: 3, y: 4 },
                        Point { x: 5, y: 6 },
                    ],
                };
                o.points[0].x
            }
        "#;

        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().expect("fixture should lex");
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().expect("fixture should parse");
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let output = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .analyze_all_for_test()
            .expect("fixture should analyze");
        let pool = &output.type_pool;

        for ty in pool.all_types() {
            let layout = pool.layout(ty);
            // Code generation's size helper reads the same authority.
            assert_eq!(type_size_bytes(pool, ty), layout.size);
            assert_eq!(layout.stride, layout.size);

            // The physical field byte offsets (compact) and the internal slot
            // offsets (value decomposition) are each ascending and in bounds,
            // but are distinct representations and need not coincide.
            if let (Some(struct_id), LayoutKind::Struct { field_offsets, .. }) =
                (ty.as_struct(), &layout.kind)
            {
                let mut prev_offset = 0u64;
                let mut prev_slot = 0u32;
                for (index, &offset) in field_offsets.iter().enumerate() {
                    assert!(offset <= layout.size, "field offset within {ty:?}");
                    assert!(offset >= prev_offset, "field offsets ascend in {ty:?}");
                    prev_offset = offset;
                    let slot_offset = struct_field_slot_offset(pool, struct_id, index as u32);
                    assert!(slot_offset >= prev_slot, "slot offsets ascend in {ty:?}");
                    prev_slot = slot_offset;
                }
                assert!(type_slot_count(pool, ty) >= field_offsets.len() as u32);
            }
        }
    }
}
