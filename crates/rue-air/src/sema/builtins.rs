//! Built-in type injection for semantic analysis.
//!
//! This module handles injection of compiler-provided enums like Arch and Os.

use std::sync::Arc;

use rue_builtins::BUILTIN_ENUMS;

use super::{DeclarationPhase, Sema};
use crate::types::{EnumDef, Type, TypeKind};

#[derive(Clone, Copy)]
enum OwnershipProperty {
    Linearity,
    Drop,
}

impl<'a> Sema<'a, super::MutableDeclarations> {
    /// Phase 0: Inject compiler-provided enums.
    pub(crate) fn inject_builtin_types(&mut self) {
        // Inject built-in enum types (Arch, Os)
        for builtin_enum in BUILTIN_ENUMS {
            let variants: Arc<[Arc<str>]> = builtin_enum
                .variants
                .iter()
                .map(|v| Arc::from(*v))
                .collect();

            // Create the synthetic enum definition
            let enum_def = EnumDef {
                name: Arc::from(builtin_enum.name),
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
            } else if builtin_enum.name == "DataModel" {
                self.builtin_data_model_id = Some(enum_id);
            }
        }
    }
}

impl<'a, D: DeclarationPhase> Sema<'a, D> {
    // ========================================================================
    // Builtin type helper methods
    // ========================================================================

    /// Return only declaration-time ownership facts that cannot be invalidated
    /// by later payload completion or infectious-linearity propagation.
    pub(crate) fn known_linear_during_binding(&self, ty: Type) -> Option<bool> {
        self.known_ownership_during_binding(ty, OwnershipProperty::Linearity)
    }

    /// Return only declaration-time drop-glue facts that cannot be invalidated
    /// by later payload completion or destructor collection.
    pub(crate) fn known_drop_glue_during_binding(&self, ty: Type) -> Option<bool> {
        self.known_ownership_during_binding(ty, OwnershipProperty::Drop)
    }

    fn known_ownership_during_binding(
        &self,
        root: Type,
        property: OwnershipProperty,
    ) -> Option<bool> {
        let mut results = std::collections::HashMap::<Type, Option<bool>>::new();
        let mut visiting = std::collections::HashSet::new();
        let mut stack = vec![(root, false)];
        while let Some((ty, expanded)) = stack.pop() {
            if results.contains_key(&ty) {
                continue;
            }
            if expanded {
                visiting.remove(&ty);
                let value = match ty.kind() {
                    TypeKind::Struct(id) => {
                        self.type_pool
                            .struct_metadata(id)
                            .and_then(|metadata| match property {
                                OwnershipProperty::Linearity => metadata.is_linear.then_some(true),
                                OwnershipProperty::Drop => {
                                    metadata.destructor.is_some().then_some(true)
                                }
                            })
                    }
                    TypeKind::Enum(id) => self.type_pool.try_enum_def(id).and_then(|def| {
                        let mut unknown = false;
                        for &child in def.variant_payloads.iter().flatten() {
                            match results.get(&child).copied().flatten() {
                                Some(true) => return Some(true),
                                Some(false) => {}
                                None => unknown = true,
                            }
                        }
                        (!unknown).then_some(false)
                    }),
                    TypeKind::Array(id) => {
                        let (element, len) = self.type_pool.array_def(id);
                        if len == 0 {
                            Some(false)
                        } else {
                            results.get(&element).copied().flatten()
                        }
                    }
                    _ => Some(false),
                };
                results.insert(ty, value);
                continue;
            }

            if !visiting.insert(ty) {
                continue;
            }
            stack.push((ty, true));
            match ty.kind() {
                TypeKind::Enum(id) => {
                    if let Some(def) = self.type_pool.try_enum_def(id) {
                        for &child in def.variant_payloads.iter().flatten().rev() {
                            if !visiting.contains(&child) {
                                stack.push((child, false));
                            }
                        }
                    }
                }
                TypeKind::Array(id) => {
                    let (child, len) = self.type_pool.array_def(id);
                    if len != 0 && !visiting.contains(&child) {
                        stack.push((child, false));
                    }
                }
                _ => {}
            }
        }
        results.get(&root).copied().flatten()
    }
}
