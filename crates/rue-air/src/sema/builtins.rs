//! Built-in type injection for semantic analysis.
//!
//! This module handles injection of compiler-provided enums like Arch and Os.

use rue_builtins::BUILTIN_ENUMS;

use super::{DeclarationPhase, Sema};
use crate::types::{EnumDef, Type, TypeKind};

impl<'a> Sema<'a> {
    /// Phase 0: Inject compiler-provided enums.
    pub(crate) fn inject_builtin_types(&mut self) {
        // Inject built-in enum types (Arch, Os)
        for builtin_enum in BUILTIN_ENUMS {
            let variants: Vec<String> = builtin_enum
                .variants
                .iter()
                .map(|v| v.to_string())
                .collect();

            // Create the synthetic enum definition
            let enum_def = EnumDef {
                name: builtin_enum.name.to_string(),
                variants,
                variant_payloads: Vec::new(),
                is_pub: true,                      // Built-in enums are always public
                file_id: rue_span::FileId::new(0), // Synthetic, no source file
            };

            // Register in type pool and get pool-based EnumId
            let name_spur = self.interner.get_or_intern(builtin_enum.name);
            let file_id = enum_def.file_id;
            let (enum_id, _) = self.type_pool.register_enum(name_spur, enum_def);

            // Register in enum lookup
            self.builtin_enums.insert(name_spur, enum_id);
            self.enums_by_file_name
                .insert((file_id, name_spur), enum_id);

            // Store special IDs for quick access
            if builtin_enum.name == "Arch" {
                self.builtin_arch_id = Some(enum_id);
            } else if builtin_enum.name == "Os" {
                self.builtin_os_id = Some(enum_id);
            }
        }
    }
}

impl<'a, D: DeclarationPhase> Sema<'a, D> {
    // ========================================================================
    // Builtin type helper methods
    // ========================================================================

    /// Check whether a type is the canonical trusted standard-library StrBuf.
    pub(crate) fn is_strbuf(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => self.type_pool.is_strbuf(struct_id),
            _ => false,
        }
    }

    /// Get the canonical StrBuf type visible to this compilation.
    ///
    /// The type is absent when the trusted standard-library module is not in
    /// the root module's transitive import graph.
    pub(crate) fn strbuf_type(&self) -> Option<Type> {
        self.type_pool.lang_item_type(crate::LangItem::StrBuf)
    }

    /// Check if a type is a linear type.
    /// Only struct types can be linear - primitives and other types are not linear.
    pub(crate) fn is_type_linear(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def.is_linear
            }
            // Only struct types can be linear
            _ => false,
        }
    }

    /// Check if a type carries a linear value when stored by value: a linear
    /// struct itself, or an array whose element type carries one.
    ///
    /// Used by infectious linearity (RUE-40): a struct with such a field must
    /// itself be linear, and destructuring a linear struct must not implicitly
    /// drop such a field. Pointers don't own their pointee and don't carry.
    pub(crate) fn type_carries_linear(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(_) => self.is_type_linear(ty),
            TypeKind::Array(array_id) => {
                let (element_type, _length) = self.type_pool.array_def(array_id);
                self.type_carries_linear(element_type)
            }
            // An enum carries a linear value when any variant payload does
            // (RUE-221, multiplicity join: a linear-payload variant makes the
            // whole enum linear/must-consume). The discriminant selects the
            // active variant at runtime, so a conservative static check
            // requires consumption if *any* variant could carry one.
            TypeKind::Enum(enum_id) => {
                let enum_def = self.type_pool.enum_def(enum_id);
                enum_def
                    .variant_payloads
                    .iter()
                    .flatten()
                    .any(|&payload_ty| self.type_carries_linear(payload_ty))
            }
            _ => false,
        }
    }

    /// Whether a type has drop glue — a destructor that runs at scope exit,
    /// either directly or through a by-value field/payload/element. Mirrors the
    /// traversal `rue_cfg`'s `type_needs_drop` performs (the codegen authority):
    /// a struct has glue if it has a destructor or any field does; an enum if any
    /// variant payload does; an array if its element type does; pointers and
    /// scalars have none (a pointer does not own its pointee).
    ///
    /// Used to gate by-copy element reads (`ArrayBuf(T)::get`/`get_or`, RUE-651):
    /// copying a drop-glue element by `@ptr_read` would alias its owned resources
    /// and double-free at scope exit, so those reads are rejected for such `T`.
    pub(crate) fn type_has_drop_glue(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def.destructor.is_some()
                    || struct_def
                        .fields
                        .iter()
                        .any(|f| self.type_has_drop_glue(f.ty))
            }
            TypeKind::Enum(enum_id) => {
                let enum_def = self.type_pool.enum_def(enum_id);
                enum_def
                    .variant_payloads
                    .iter()
                    .flatten()
                    .any(|&payload_ty| self.type_has_drop_glue(payload_ty))
            }
            TypeKind::Array(array_id) => {
                let (element_type, _length) = self.type_pool.array_def(array_id);
                self.type_has_drop_glue(element_type)
            }
            _ => false,
        }
    }
}
