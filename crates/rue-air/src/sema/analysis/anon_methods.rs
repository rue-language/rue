//! Anonymous-struct method registration and comptime method-signature extraction.
//!
//! These helpers extend the canonical semantic-analysis implementation for
//! anonymous nominal types.

use super::*;

impl<'a, D: crate::sema::DeclarationPhase> crate::sema::Sema<'a, D> {
    /// Install query-resolved anonymous method signatures while using RIR only
    /// for request-local parameter names and body instruction locators.
    pub(crate) fn register_projected_anon_struct_methods(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        methods: &rue_rir::RirAnonStructMethodsRange,
        signatures: &[super::super::AnonMethodSig],
    ) -> Option<()> {
        fn materialize(ty: &super::super::AnonMethodType, self_type: Type) -> Option<Type> {
            match ty {
                super::super::AnonMethodType::SelfType => Some(self_type),
                super::super::AnonMethodType::Concrete(ty) => Some(*ty),
                // Query projections currently publish composites as concrete
                // canonical types. These variants remain valid for the cold
                // AIR evaluator but must not appear at this install seam.
                super::super::AnonMethodType::Array { .. }
                | super::super::AnonMethodType::PtrConst(_)
                | super::super::AnonMethodType::PtrMut(_)
                | super::super::AnonMethodType::Syntax(_) => None,
            }
        }

        let refs = self.rir.anon_struct_methods(methods);
        if refs.len() != signatures.len() {
            return None;
        }
        let mut seen = std::collections::HashSet::new();
        let mut staged = Vec::with_capacity(signatures.len());
        for (method_ref, signature) in refs.iter().zip(signatures) {
            let instruction = self.rir.get(*method_ref);
            let InstData::FnDecl {
                name,
                params,
                body,
                has_self,
                self_mode,
                self_is_mut,
                returns_borrow,
                ..
            } = &instruction.data
            else {
                return None;
            };
            let parameters = self.rir.params(params);
            if *name != signature.name
                || *has_self != signature.has_self
                || *self_mode != signature.self_mode
                || parameters.len() != signature.param_types.len()
                || !parameters
                    .iter()
                    .zip(signature.param_modes.iter())
                    .all(|(parameter, mode)| parameter.mode == *mode)
                || !parameters
                    .iter()
                    .zip(signature.param_comptime.iter())
                    .all(|(parameter, comptime)| parameter.is_comptime == *comptime)
                || !seen.insert(*name)
                || self.has_method((struct_id, *name))
                // Accessors are not supported on anonymous structs (ADR-0062
                // phase 1).
                || *returns_borrow
            {
                return None;
            }
            let types = signature
                .param_types
                .iter()
                .map(|ty| materialize(ty, struct_type))
                .collect::<Option<Vec<_>>>()?;
            let range = self.param_arena.alloc_method(
                parameters.iter().map(|parameter| parameter.name),
                types,
                signature.param_modes.clone(),
                signature.param_comptime.clone(),
            );
            staged.push((
                (struct_id, *name),
                MethodInfo {
                    struct_type,
                    has_self: *has_self,
                    self_mode: *self_mode,
                    self_is_mut: *self_is_mut,
                    params: range,
                    return_type: materialize(&signature.return_type, struct_type)?,
                    body: *body,
                    span: instruction.span,
                    returns_borrow: false,
                },
            ));
        }
        for ((owner, name), info) in staged {
            let has_self = info.has_self;
            self.anonymous_methods.insert((owner, name), info);
            self.index_anonymous_callable_method(owner, name, has_self);
        }
        Some(())
    }

    /// Resolve a type symbol, with special handling for Self.
    ///
    /// If the type symbol is "Self", it resolves to the provided self_type.
    /// Otherwise, it delegates to the standard resolve_type method.
    pub(crate) fn resolve_type_with_self(
        &mut self,
        type_sym: Spur,
        self_type: Type,
        span: Span,
    ) -> CompileResult<Type> {
        let type_str = self.interner.resolve(&type_sym);
        if type_str == "Self" {
            Ok(self_type)
        } else {
            self.resolve_type(type_sym, span)
        }
    }

    // ========================================================================
    // Pointer intrinsics (require unchecked context)
    // ========================================================================
}
