//! Built-in type injection for semantic analysis.
//!
//! This module handles injection of built-in types like String as synthetic
//! structs. Built-in types are registered before user code is processed,
//! enabling collision detection and proper type resolution.

use rue_builtins::{BUILTIN_TYPES, BuiltinFieldType, BuiltinTypeDef};

use super::Sema;
use crate::types::{StructDef, StructField, StructId, Type, TypeKind};

impl<'a> Sema<'a> {
    /// Phase 0: Inject built-in types as synthetic structs.
    ///
    /// This creates `StructDef` entries for built-in types like `String` before
    /// processing user code. The built-in types are registered in the `structs`
    /// HashMap so they can be looked up by name, and their StructIds are stored
    /// in dedicated fields (e.g., `builtin_string_id`) for fast access.
    ///
    /// Built-in types are marked with `is_builtin: true` and have their fields,
    /// destructor, and copy status derived from the `rue-builtins` registry.
    pub(crate) fn inject_builtin_types(&mut self) {
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
                        BuiltinFieldType::Bool => Type::Bool,
                    },
                })
                .collect();

            // Create the synthetic struct definition
            // Built-in types are always public and have no source file
            let struct_def = StructDef {
                name: builtin.name.to_string(),
                fields,
                is_copy: builtin.is_copy,
                is_handle: false, // Built-in types don't use @handle yet
                is_linear: false, // Built-in types are not linear
                destructor: builtin.drop_fn.map(|s| s.to_string()),
                is_builtin: true,
                is_pub: true,                      // Built-in types are always public
                file_id: rue_span::FileId::new(0), // Synthetic, no source file
            };

            // Register in type pool and get pool-based StructId
            let name_spur = self.interner.get_or_intern(builtin.name);
            let (struct_id, _) = self.type_pool.register_struct(name_spur, struct_def);

            // Register in struct lookup with pool-based StructId
            self.structs.insert(name_spur, struct_id);

            // Store special IDs for quick access
            if builtin.name == "String" {
                self.builtin_string_id = Some(struct_id);
            }

            // Note: Associated functions and methods are not registered here.
            // They are handled by looking up methods in the builtin registry
            // when analyzing method calls on builtin types.
        }
    }

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
        Type::Struct(
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
        Type::Struct(struct_id)
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
}
