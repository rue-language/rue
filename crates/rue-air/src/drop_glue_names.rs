//! Canonical derivation of synthesized drop-glue symbol names.
//!
//! Drop glue for a struct, enum, or array is a synthesized function whose
//! external symbol name encodes the type it drops (for example
//! `__rue_drop_Container` or `__rue_drop_array_String_3`). The compiler's glue
//! *synthesis* and both backends' *lowering* must spell that name identically,
//! or a drop call cannot find the glue it needs at link time.
//!
//! This module is the single authority for those spellings (RUE-796). Every
//! consumer derives names here instead of re-deriving the `__rue_drop_*` format
//! string, so the external ABI cannot drift between phases. The names are a
//! function of the type alone — they do not depend on the target architecture.
//!
//! Two type vocabularies reach the grammar: the live type pool during semantic
//! analysis and code generation, and the durable type instances `rue-compiler`
//! names rooted callables from. [`DropGlueTypeShape`] is what they have in
//! common, so the recursion and every fragment separator are spelled once here
//! while each vocabulary contributes only its own leaves.

use crate::{ArrayTypeId, EnumId, FrozenTypeInternPool, StructId, Type, TypeKind};

/// One node of the drop-glue fragment grammar.
///
/// A leaf spells itself — scalars, nominals, and the compile-time-only kinds
/// that never reach real glue. The composite arms carry the structure the
/// fragment encodes, and nothing else: a vocabulary decides what its types
/// *are*, never how the fragment reads.
pub enum DropGlueTypeShape<T> {
    Leaf(String),
    Array { element: T, len: u64 },
    PtrConst(T),
    PtrMut(T),
}

/// A type vocabulary that can be spelled as a drop-glue fragment.
///
/// `None` marks a type with no drop-glue spelling in that vocabulary: the
/// durable instances carry kinds (slices, generic parameters) that never name
/// glue, and a caller that reaches one falls back to its own naming.
pub trait DropGlueTypeShapeSource: Sized {
    fn drop_glue_shape(&self) -> Option<DropGlueTypeShape<Self>>;
}

/// The structural name fragment for a type, used to build drop-glue symbols.
///
/// Nominal types use their live nominal symbol, so two same-named types
/// declared in different modules receive distinct glue and distinct
/// array-element fragments (RUE-571). Arrays and pointers recurse into their
/// element/pointee.
pub fn drop_glue_type_fragment<T: DropGlueTypeShapeSource>(ty: &T) -> Option<String> {
    Some(match ty.drop_glue_shape()? {
        DropGlueTypeShape::Leaf(name) => name,
        DropGlueTypeShape::Array { element, len } => {
            format!("array_{}_{len}", drop_glue_type_fragment(&element)?)
        }
        DropGlueTypeShape::PtrConst(pointee) => {
            format!("ptr_const_{}", drop_glue_type_fragment(&pointee)?)
        }
        DropGlueTypeShape::PtrMut(pointee) => {
            format!("ptr_mut_{}", drop_glue_type_fragment(&pointee)?)
        }
    })
}

/// The drop-glue symbol built from an already-spelled type fragment.
pub fn drop_glue_symbol(type_fragment: &str) -> String {
    format!("__rue_drop_{type_fragment}")
}

/// One live pool type, the vocabulary sema and codegen spell glue from.
#[derive(Clone, Copy)]
struct PoolType<'a> {
    ty: Type,
    type_pool: &'a FrozenTypeInternPool,
}

impl DropGlueTypeShapeSource for PoolType<'_> {
    fn drop_glue_shape(&self) -> Option<DropGlueTypeShape<Self>> {
        let leaf = |name: &str| Some(DropGlueTypeShape::Leaf(name.to_owned()));
        let nested = |ty: Type| PoolType {
            ty,
            type_pool: self.type_pool,
        };
        match self.ty.kind() {
            TypeKind::I8 => leaf("i8"),
            TypeKind::I16 => leaf("i16"),
            TypeKind::I32 => leaf("i32"),
            TypeKind::I64 => leaf("i64"),
            TypeKind::U8 => leaf("u8"),
            TypeKind::U16 => leaf("u16"),
            TypeKind::U32 => leaf("u32"),
            TypeKind::U64 => leaf("u64"),
            TypeKind::Bool => leaf("bool"),
            TypeKind::Unit => leaf("unit"),
            TypeKind::Never => leaf("never"),
            TypeKind::Error => leaf("error"),
            // ComptimeType only exists at compile time, no runtime representation.
            TypeKind::ComptimeType => leaf("comptime_type"),
            TypeKind::ComptimeFloat => leaf("comptime_float"),
            TypeKind::F32 => leaf("f32"),
            TypeKind::F64 => leaf("f64"),
            TypeKind::Enum(enum_id) => Some(DropGlueTypeShape::Leaf(
                self.type_pool.enum_symbol_name(enum_id),
            )),
            // Struct types include builtin types like String.
            TypeKind::Struct(struct_id) => Some(DropGlueTypeShape::Leaf(
                self.type_pool.struct_symbol_name(struct_id),
            )),
            // Module types should never reach drop glue (compile-time only).
            TypeKind::Module(_) => leaf("module"),
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.type_pool.array_def(array_id);
                Some(DropGlueTypeShape::Array {
                    element: nested(element_type),
                    len: length,
                })
            }
            TypeKind::PtrConst(ptr_id) => Some(DropGlueTypeShape::PtrConst(nested(
                self.type_pool.ptr_const_def(ptr_id),
            ))),
            TypeKind::PtrMut(ptr_id) => Some(DropGlueTypeShape::PtrMut(nested(
                self.type_pool.ptr_mut_def(ptr_id),
            ))),
        }
    }
}

/// Drop-glue symbol for a struct type, e.g. `__rue_drop_Container`.
pub fn struct_drop_glue_name(struct_id: StructId, type_pool: &FrozenTypeInternPool) -> String {
    drop_glue_symbol(&type_pool.struct_symbol_name(struct_id))
}

/// Drop-glue symbol for an enum type, e.g. `__rue_drop_Choice`.
pub fn enum_drop_glue_name(enum_id: EnumId, type_pool: &FrozenTypeInternPool) -> String {
    drop_glue_symbol(&type_pool.enum_symbol_name(enum_id))
}

/// Drop-glue symbol for an array type.
///
/// The name encodes the element type and length, e.g. `__rue_drop_array_String_3`.
pub fn array_drop_glue_name(array_id: ArrayTypeId, type_pool: &FrozenTypeInternPool) -> String {
    drop_glue_symbol(&type_name(Type::new_array(array_id), type_pool))
}

/// The structural name fragment for one live pool type.
///
/// The pool vocabulary spells every kind it has, so this never fails; the
/// grammar itself is [`drop_glue_type_fragment`].
pub fn type_name(ty: Type, type_pool: &FrozenTypeInternPool) -> String {
    drop_glue_type_fragment(&PoolType { ty, type_pool })
        .expect("every live pool type has a drop-glue fragment")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lasso::ThreadedRodeo;

    use super::*;
    use crate::{EnumDef, StructDef, StructField, TypeInternPool};

    fn register_struct(
        type_pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        file_id: rue_span::FileId,
        fields: Vec<StructField>,
    ) -> StructId {
        let symbol = interner.get_or_intern(name);
        type_pool
            .register_struct(
                symbol,
                StructDef {
                    name: name.into(),
                    fields,
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                    is_builtin: false,
                    is_pub: false,
                    file_id,
                },
            )
            .0
    }

    fn register_enum(
        type_pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        file_id: rue_span::FileId,
    ) -> EnumId {
        let symbol = interner.get_or_intern(name);
        type_pool
            .register_enum(
                symbol,
                EnumDef {
                    name: name.into(),
                    variants: Arc::from(["Only".into()]),
                    variant_payloads: vec![vec![]],
                    is_pub: false,
                    is_non_exhaustive: false,
                    file_id,
                },
            )
            .0
    }

    #[test]
    fn primitive_type_names_are_stable() {
        let type_pool = TypeInternPool::new().freeze();
        assert_eq!(type_name(Type::I32, &type_pool), "i32");
        assert_eq!(type_name(Type::U64, &type_pool), "u64");
        assert_eq!(type_name(Type::BOOL, &type_pool), "bool");
    }

    #[test]
    fn struct_and_enum_glue_names() {
        let type_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let struct_id = register_struct(
            &type_pool,
            &interner,
            "Container",
            rue_span::FileId::DEFAULT,
            vec![],
        );
        let enum_id = register_enum(&type_pool, &interner, "Choice", rue_span::FileId::DEFAULT);
        let type_pool = type_pool.freeze();

        // Named nominals are unconditionally file-qualified (ADR-0066,
        // RUE-1089); the standalone test pool has no logical path, so the file
        // component falls back to the numeric file index.
        assert_eq!(
            struct_drop_glue_name(struct_id, &type_pool),
            "__rue_drop_Container$0"
        );
        assert_eq!(
            enum_drop_glue_name(enum_id, &type_pool),
            "__rue_drop_Choice$0"
        );
    }

    #[test]
    fn array_and_nested_array_glue_names() {
        let type_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let struct_id = register_struct(
            &type_pool,
            &interner,
            "String",
            rue_span::FileId::DEFAULT,
            vec![],
        );
        // [String; 3]
        let inner = type_pool.intern_array_from_type(Type::new_struct(struct_id), 3);
        // [[String; 3]; 2]
        let outer = type_pool.intern_array_from_type(Type::new_array(inner), 2);
        let type_pool = type_pool.freeze();

        // `String` here is a plain user struct (not the builtin), so it is
        // file-qualified in the element-name fragment (ADR-0066, RUE-1089).
        assert_eq!(
            array_drop_glue_name(inner, &type_pool),
            "__rue_drop_array_String$0_3"
        );
        assert_eq!(
            array_drop_glue_name(outer, &type_pool),
            "__rue_drop_array_array_String$0_3_2"
        );
    }

    #[test]
    fn pointer_element_names_recurse() {
        let type_pool = TypeInternPool::new();
        let const_ptr = type_pool.intern_ptr_const_from_type(Type::I32);
        let array_of_ptr = type_pool.intern_array_from_type(Type::new_ptr_const(const_ptr), 4);
        let type_pool = type_pool.freeze();
        assert_eq!(
            array_drop_glue_name(array_of_ptr, &type_pool),
            "__rue_drop_array_ptr_const_i32_4"
        );
    }

    #[test]
    fn same_named_types_in_different_files_get_distinct_glue() {
        // Collision resistance: two `Choice` enums declared in different files
        // must not share drop glue.
        let type_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let left = register_enum(&type_pool, &interner, "Choice", rue_span::FileId::new(1));
        let right = register_enum(&type_pool, &interner, "Choice", rue_span::FileId::new(2));
        let left_array = type_pool.intern_array_from_type(Type::new_enum(left), 2);
        let right_array = type_pool.intern_array_from_type(Type::new_enum(right), 2);
        let type_pool = type_pool.freeze();

        let left_name = array_drop_glue_name(left_array, &type_pool);
        let right_name = array_drop_glue_name(right_array, &type_pool);
        assert_eq!(left_name, "__rue_drop_array_Choice$1_2");
        assert_eq!(right_name, "__rue_drop_array_Choice$2_2");
        assert_ne!(left_name, right_name);
    }
}
