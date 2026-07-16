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
                let struct_def = self
                    .type_pool
                    .try_struct_def(struct_id)
                    .expect("linearity queries require a complete struct definition");
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
    ///
    /// Every reachable nominal definition must be complete. During the
    /// declaration fixpoint the struct flags are the values being converged;
    /// outside that fixpoint callers require finalized declaration ownership.
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
                let enum_def = self
                    .type_pool
                    .try_enum_def(enum_id)
                    .expect("linearity queries require a complete enum definition");
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
    /// Every reachable nominal definition and destructor assignment must be
    /// finalized before this query is authoritative.
    pub(crate) fn type_has_drop_glue(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = self
                    .type_pool
                    .try_struct_def(struct_id)
                    .expect("drop-glue queries require a complete struct definition");
                struct_def.destructor.is_some()
                    || struct_def
                        .fields
                        .iter()
                        .any(|f| self.type_has_drop_glue(f.ty))
            }
            TypeKind::Enum(enum_id) => {
                let enum_def = self
                    .type_pool
                    .try_enum_def(enum_id)
                    .expect("drop-glue queries require a complete enum definition");
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

    /// Whether declaration finalization can still change an ownership query
    /// for `ty`. Pointers deliberately stop the walk because they do not own
    /// their pointee; arrays carry their element by value.
    pub(crate) fn type_ownership_depends_on_nominal(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(_) | TypeKind::Enum(_) => true,
            TypeKind::Array(array_id) => {
                let (element_type, _) = self.type_pool.array_def(array_id);
                self.type_ownership_depends_on_nominal(element_type)
            }
            _ => false,
        }
    }

    /// Return only declaration-time ownership facts that cannot be invalidated
    /// by later payload completion or infectious-linearity propagation.
    pub(crate) fn known_linear_during_binding(&self, ty: Type) -> Option<bool> {
        match ty.kind() {
            TypeKind::Struct(id) => self
                .type_pool
                .struct_metadata(id)
                .and_then(|metadata| metadata.is_linear.then_some(true)),
            TypeKind::Enum(id) => {
                let def = self.type_pool.try_enum_def(id)?;
                let mut deferred = false;
                for payload in def.variant_payloads.iter().flatten() {
                    match self.known_linear_during_binding(*payload) {
                        Some(true) => return Some(true),
                        Some(false) => {}
                        None => deferred = true,
                    }
                }
                (!deferred).then_some(false)
            }
            TypeKind::Array(id) => self.known_linear_during_binding(self.type_pool.array_def(id).0),
            _ => Some(false),
        }
    }

    /// Return only declaration-time drop-glue facts that cannot be invalidated
    /// by later payload completion or destructor collection.
    pub(crate) fn known_drop_glue_during_binding(&self, ty: Type) -> Option<bool> {
        match ty.kind() {
            TypeKind::Struct(id) => self
                .type_pool
                .struct_metadata(id)
                .and_then(|metadata| metadata.destructor.is_some().then_some(true)),
            TypeKind::Enum(id) => {
                let def = self.type_pool.try_enum_def(id)?;
                let mut deferred = false;
                for payload in def.variant_payloads.iter().flatten() {
                    match self.known_drop_glue_during_binding(*payload) {
                        Some(true) => return Some(true),
                        Some(false) => {}
                        None => deferred = true,
                    }
                }
                (!deferred).then_some(false)
            }
            TypeKind::Array(id) => {
                self.known_drop_glue_during_binding(self.type_pool.array_def(id).0)
            }
            _ => Some(false),
        }
    }
}
