//! Built-in type injection for semantic analysis.
//!
//! This module handles injection of built-in types like `StrBuf` (the
//! growable-string type, ADR-0043; formerly `String`) as synthetic structs,
//! and built-in enums like Arch and Os as synthetic enums.
//! Built-in types are registered before user code is processed,
//! enabling collision detection and proper type resolution.

use rue_builtins::{BUILTIN_ENUMS, BUILTIN_TYPES, BuiltinFieldType, BuiltinTypeDef};

use super::{DeclarationPhase, Sema};
use crate::types::{EnumDef, StructDef, StructField, StructId, Type, TypeKind};

impl<'a> Sema<'a> {
    /// Phase 0: Inject built-in types as synthetic structs and enums.
    ///
    /// This creates `StructDef` entries for built-in types like `StrBuf` and
    /// `EnumDef` entries for built-in enums like `Arch` and `Os` before
    /// processing user code. The built-in types are registered in the `structs`
    /// and `enums` HashMaps so they can be looked up by name, and their IDs are
    /// stored in dedicated fields for fast access.
    ///
    /// Built-in types are marked with `is_builtin: true` and have their fields,
    /// destructor, and copy status derived from the `rue-builtins` registry.
    pub(crate) fn inject_builtin_types(&mut self) {
        // Inject built-in struct types (StrBuf, etc.)
        for builtin in BUILTIN_TYPES {
            // Convert builtin field types to our Type enum
            let fields: Vec<StructField> = builtin
                .fields
                .iter()
                .map(|f| StructField {
                    name: f.name.to_string(),
                    ty: match f.ty {
                        BuiltinFieldType::U64 => Type::U64,
                        BuiltinFieldType::U8 => Type::U8,
                        BuiltinFieldType::Bool => Type::BOOL,
                    },
                })
                .collect();

            // Create the synthetic struct definition
            // Built-in types are always public and have no source file
            let struct_def = StructDef {
                name: builtin.name.to_string(),
                fields,
                is_copy: builtin.is_copy,
                is_linear: false, // Built-in types are not linear
                destructor: builtin.drop_fn.map(|s| s.to_string()),
                is_builtin: true,
                is_pub: true,                      // Built-in types are always public
                file_id: rue_span::FileId::new(0), // Synthetic, no source file
            };

            // Register in type pool and get pool-based StructId
            let name_spur = self.interner.get_or_intern(builtin.name);
            let file_id = struct_def.file_id;
            let (struct_id, _) = self.type_pool.register_struct(name_spur, struct_def);

            // Register in struct lookup with pool-based StructId
            self.builtin_structs.insert(name_spur, struct_id);
            self.structs_by_file_name
                .insert((file_id, name_spur), struct_id);

            // Store special IDs for quick access
            if builtin.name == "StrBuf" {
                self.builtin_string_id = Some(struct_id);
            }

            // Note: Associated functions and methods are not registered here.
            // They are handled by looking up methods in the builtin registry
            // when analyzing method calls on builtin types.
        }

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

    /// Check if a type is the builtin String type.
    ///
    /// Uses the stored `builtin_string_id` for fast comparison.
    pub(crate) fn is_builtin_string(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => Some(struct_id) == self.builtin_string_id,
            _ => false,
        }
    }

    /// Get the builtin type definition for a struct if it's a builtin type.
    ///
    /// Returns `Some(&BuiltinTypeDef)` if the struct is a builtin type,
    /// `None` otherwise.
    pub(crate) fn get_builtin_type_def(
        &self,
        struct_id: StructId,
    ) -> Option<&'static BuiltinTypeDef> {
        let struct_def = self.type_pool.struct_def(struct_id);
        if struct_def.is_builtin {
            rue_builtins::get_builtin_type(&struct_def.name)
        } else {
            None
        }
    }

    /// Get the String struct type.
    ///
    /// Returns the Type::Struct for the builtin String type.
    /// Panics if called before builtin types are injected.
    pub(crate) fn builtin_string_type(&self) -> Type {
        Type::new_struct(
            self.builtin_string_id
                .expect("builtin types not injected yet"),
        )
    }

    /// Check if a method name is a builtin mutation method.
    ///
    /// Mutation methods need special handling because they require storage location
    /// to be captured before the receiver is analyzed.
    pub(crate) fn is_builtin_mutation_method(&self, method_name: &str) -> bool {
        use rue_builtins::ReceiverMode;

        // Check all builtin types for methods with ByMutRef receiver
        for builtin in BUILTIN_TYPES {
            if let Some(method) = builtin.find_method(method_name) {
                if method.receiver_mode == ReceiverMode::ByMutRef {
                    return true;
                }
            }
        }
        false
    }

    /// Get the AIR output type for a builtin struct.
    ///
    /// Builtin types like String are now represented as Type::Struct with is_builtin=true.
    pub(crate) fn builtin_air_type(&self, struct_id: StructId) -> Type {
        Type::new_struct(struct_id)
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
