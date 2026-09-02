//! Provider-owned semantic body projection and input resolution.
//!
//! This module owns provider-side type/reference conversion, semantic type
//! syntax services, signature resolution, and canonical body-input assembly.
//! It does not own body transaction publication or query-runtime state.

use super::super::*;

fn body_type_instance(
    ty: &rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>,
) -> crate::TypeInstanceKey {
    use rue_air::SemanticImportType as T;
    match ty {
        T::I8 => crate::TypeInstanceKey::I8,
        T::I16 => crate::TypeInstanceKey::I16,
        T::I32 => crate::TypeInstanceKey::I32,
        T::I64 => crate::TypeInstanceKey::I64,
        T::U8 => crate::TypeInstanceKey::U8,
        T::U16 => crate::TypeInstanceKey::U16,
        T::U32 => crate::TypeInstanceKey::U32,
        T::U64 => crate::TypeInstanceKey::U64,
        T::Bool => crate::TypeInstanceKey::Bool,
        T::Unit => crate::TypeInstanceKey::Unit,
        T::Never => crate::TypeInstanceKey::Never,
        T::ComptimeType => crate::TypeInstanceKey::ComptimeType,
        T::BuiltinNominal { name, kind } => crate::TypeInstanceKey::BuiltinNominal {
            kind: match kind {
                rue_air::SemanticImportNominalKind::Struct => rue_air::AnonymousNominalKind::Struct,
                rue_air::SemanticImportNominalKind::Enum => rue_air::AnonymousNominalKind::Enum,
            },
            name: name.clone(),
        },
        T::Nominal(definition) => {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(definition.clone()))
        }
        T::AnonymousNominal(identity) => crate::TypeInstanceKey::Nominal(
            crate::NominalInstanceKey::Anonymous(Node::new(identity.clone())),
        ),
        T::Array { element, len } => crate::TypeInstanceKey::Array {
            element: Node::new(body_type_instance(element)),
            len: *len,
        },
        T::Slice { element, name } => crate::TypeInstanceKey::Slice {
            element: Node::new(body_type_instance(element)),
            name: name.clone(),
        },
        T::PtrConst(element) => {
            crate::TypeInstanceKey::PtrConst(Node::new(body_type_instance(element)))
        }
        T::PtrMut(element) => {
            crate::TypeInstanceKey::PtrMut(Node::new(body_type_instance(element)))
        }
        T::Module(module) => crate::TypeInstanceKey::Module(module.clone()),
        T::GenericParameter(index) => crate::TypeInstanceKey::GenericParameter(*index),
    }
}

fn collect_body_type_reference(
    ty: &rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>,
    references: &mut BTreeSet<crate::body_query::BodyReference>,
) {
    references.insert(crate::body_query::BodyReference::Type(body_type_instance(
        ty,
    )));
    use rue_air::SemanticImportType as T;
    match ty {
        T::Array { element, .. }
        | T::Slice { element, .. }
        | T::PtrConst(element)
        | T::PtrMut(element) => collect_body_type_reference(element, references),
        _ => {}
    }
}

/// Publish the exact identity-bearing dependencies already observed in a
/// provider-produced semantic body. This traverses the canonical output only;
/// it never repeats lookup or semantic queries after analysis.
pub(in crate::revisioned_query_database) fn collect_published_body_references(
    body: &rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
    references: &mut BTreeSet<crate::body_query::BodyReference>,
) {
    use rue_air::SemanticBodyInstDependency as D;
    let collect_drop_obligation =
        |value: rue_air::SemanticBodyRef,
         references: &mut BTreeSet<crate::body_query::BodyReference>| {
            if let Some(value) = body.instructions.get(value as usize) {
                references.insert(crate::body_query::BodyReference::DropGlue(
                    body_type_instance(&value.ty),
                ));
            }
        };
    collect_body_type_reference(&body.return_type, references);
    for instruction in body.instructions.iter() {
        collect_body_type_reference(&instruction.ty, references);
        use rue_air::SemanticBodyInstData as I;
        match &instruction.data {
            // These are precisely the ownership sites from which CFG cleanup
            // elaboration can emit an implicit destroy: a live local, an
            // overwritten local/parameter/place, or a discarded statement
            // result. Publishing their value types here keeps DropGlue rooted
            // in the reached body that owns the obligation without duplicating
            // CFG's path-sensitive drop elaboration.
            I::Alloc { init: value, .. }
            | I::Store { value, .. }
            | I::ParamStore { value, .. }
            | I::PlaceWrite { value, .. }
            | I::Drop { value } => collect_drop_obligation(*value, references),
            I::Block { statements, .. } => {
                for &statement in statements.iter() {
                    collect_drop_obligation(statement, references);
                }
            }
            _ => {}
        }
        instruction
            .data
            .visit_dependencies(&mut |dependency| match dependency {
                D::Definition(definition) => {
                    references.insert(crate::body_query::BodyReference::Definition(
                        definition.clone(),
                    ));
                }
                D::Nominal(nominal) => {
                    references.insert(crate::body_query::BodyReference::Type(
                        crate::TypeInstanceKey::Nominal(nominal.clone()),
                    ));
                }
                D::Function(function) => {
                    references.insert(crate::body_query::BodyReference::Callable(function.clone()));
                }
                D::Type(ty) => collect_body_type_reference(ty, references),
                D::Instruction(_) | D::Place(_) | D::String(_) => {}
            });
    }
    for place in body.places.iter() {
        collect_body_type_reference(&place.base_type, references);
        for projection in place.projections.iter() {
            match projection {
                rue_air::SemanticBodyProjection::Field { struct_key, .. } => {
                    references.insert(crate::body_query::BodyReference::Type(
                        crate::TypeInstanceKey::Nominal(struct_key.clone()),
                    ));
                }
                rue_air::SemanticBodyProjection::Index { array_type, .. } => {
                    collect_body_type_reference(array_type, references);
                }
            }
        }
    }
    for (_, ty) in body.param_drops.iter() {
        collect_body_type_reference(ty, references);
        references.insert(crate::body_query::BodyReference::DropGlue(
            body_type_instance(ty),
        ));
    }
}

pub(crate) fn semantic_candidate_import_occurrences(
    rir: &rue_rir::ValidatedRir,
    symbols: &[&str],
    mut checkpoint: impl FnMut() -> Result<(), QueryAbort>,
) -> Result<BTreeMap<rue_rir::InstRef, (u32, Arc<str>)>, QueryAbort> {
    let mut sites = Vec::new();
    for (instruction_ref, instruction) in rir.iter() {
        if instruction_ref.as_u32() % 64 == 0 {
            checkpoint()?;
        }
        let rue_rir::InstData::Intrinsic { name, args } = &instruction.data else {
            continue;
        };
        if symbols[name.into_usize()] != "import" {
            continue;
        }
        let arguments = rir.intrinsic_args(args);
        if arguments.len() != 1 {
            continue;
        }
        let argument = arguments
            .get(0)
            .expect("validated intrinsic argument index");
        let rue_rir::InstData::StringConst { content, .. } = &rir.get(argument).data else {
            continue;
        };
        sites.push((
            instruction.span.start,
            instruction.span.end,
            instruction_ref,
            Arc::<str>::from(symbols[content.into_usize()]),
        ));
    }
    sites.sort_by_key(|(start, end, instruction, _)| (*start, *end, *instruction));
    Ok(sites
        .into_iter()
        .enumerate()
        .map(|(occurrence, (_, _, instruction, specifier))| {
            (
                instruction,
                (
                    u32::try_from(occurrence)
                        .expect("validated RIR instruction count is bounded by u32"),
                    specifier,
                ),
            )
        })
        .collect())
}

impl SemanticNucleusTypeProvider<'_> {
    pub(in crate::revisioned_query_database) fn with_dependency_source<R>(
        &mut self,
        source: &crate::StableDefinitionKey,
        operation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        with_restored_state(
            self,
            |provider| std::mem::replace(&mut provider.dependency_source, source.clone()),
            operation,
            |provider, previous| provider.dependency_source = previous,
        )
    }

    pub(in crate::revisioned_query_database) fn merge_comptime_effects(
        &mut self,
        effects: crate::durable_comptime::DurableComptimeEffects,
        policy: &crate::durable_comptime::DurableComptimeApplicationPolicy,
    ) {
        effects.apply_to(
            &mut self.anonymous_nominals,
            &mut self.dependencies,
            &mut self.deferred_ownership,
            policy,
        );
    }

    pub(in crate::revisioned_query_database) fn merge_anonymous_projections(
        &mut self,
        nominals: &[crate::durable_semantics::DurableAnonymousNominal],
    ) -> Result<
        (),
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        for nominal in nominals {
            crate::durable_semantics::merge_anonymous_nominal(
                &mut self.anonymous_nominals,
                nominal,
            )
            .map_err(|identity| {
                Self::provider_failure_value(format!(
                    "conflicting durable anonymous facts for {identity:?}"
                ))
            })?;
        }
        Ok(())
    }

    fn anonymous_projection(
        &self,
        identity: &crate::AnonymousNominalKey,
    ) -> Option<crate::durable_semantics::DurableAnonymousNominal> {
        let identity = identity.with_canonical_producer();
        self.anonymous_nominals.get(identity.as_ref()).cloned()
    }

    fn ffi_shape_failure(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        path: &mut Vec<String>,
    ) -> Result<
        Option<(
            rue_air::FfiRejectReason,
            Vec<String>,
            crate::durable_semantics::DurableType,
        )>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::DurableType as T;
        use rue_air::FfiRejectReason as R;
        match ty {
            T::I8
            | T::I16
            | T::I32
            | T::I64
            | T::U8
            | T::U16
            | T::U32
            | T::U64
            | T::Bool
            | T::PtrConst(_)
            | T::PtrMut(_) => Ok(None),
            T::Array { element, .. } => self.ffi_shape_failure(element, path),
            T::Nominal(key) if key.kind() == crate::StableDefinitionKind::Enum => {
                Ok(Some((R::Enum, path.clone(), ty.clone())))
            }
            T::Nominal(key) if key.kind() == crate::StableDefinitionKind::Struct => {
                let Some(candidate) =
                    self.candidate(key.module(), key.name(), DefinitionKind::Struct)?
                else {
                    return Self::provider_failure(format!(
                        "FFI struct `{}` is unavailable",
                        key.name()
                    ));
                };
                let signature = self.signature(candidate)?;
                let crate::semantic_query_nucleus::DeclarationSignatureProjection::Struct {
                    fields,
                    is_linear,
                    is_repr_c,
                    ..
                } = signature
                else {
                    return Self::provider_failure("FFI nominal has the wrong signature kind");
                };
                if !is_repr_c {
                    return Ok(Some((R::NonReprCAggregate, path.clone(), ty.clone())));
                }
                if fields.is_empty() {
                    return Ok(Some((R::EmptyStruct, path.clone(), ty.clone())));
                }
                if is_linear {
                    return Ok(Some((R::Linear, path.clone(), ty.clone())));
                }
                if self
                    .candidate(key.module(), key.name(), DefinitionKind::Destructor)?
                    .is_some()
                {
                    return Ok(Some((R::HasDestructor, path.clone(), ty.clone())));
                }
                for (name, field) in fields.iter() {
                    path.push(name.to_string());
                    if let Some(failure) = self.ffi_shape_failure(field, path)? {
                        return Ok(Some(failure));
                    }
                    path.pop();
                }
                Ok(None)
            }
            T::AnonymousNominal(_)
            | T::Slice { .. }
            | T::Unit
            | T::Never
            | T::ComptimeType
            | T::BuiltinNominal { .. }
            | T::Module(_)
            | T::GenericParameter(_) => Ok(Some((R::UnsupportedType, path.clone(), ty.clone()))),
            T::Nominal(_) => Ok(Some((R::UnsupportedType, path.clone(), ty.clone()))),
        }
    }

    fn repr_c_failure_for_fields(
        &mut self,
        fields: &[(Arc<str>, crate::durable_semantics::DurableType)],
        is_linear: bool,
        has_destructor: bool,
    ) -> Result<
        Option<(
            rue_air::FfiRejectReason,
            Vec<String>,
            crate::durable_semantics::DurableType,
        )>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use rue_air::FfiRejectReason as R;
        if fields.is_empty() {
            return Ok(Some((
                R::EmptyStruct,
                Vec::new(),
                crate::durable_semantics::DurableType::Unit,
            )));
        }
        if is_linear {
            return Ok(Some((
                R::Linear,
                Vec::new(),
                crate::durable_semantics::DurableType::Unit,
            )));
        }
        if has_destructor {
            return Ok(Some((
                R::HasDestructor,
                Vec::new(),
                crate::durable_semantics::DurableType::Unit,
            )));
        }
        let mut path = Vec::new();
        for (name, ty) in fields {
            path.push(name.to_string());
            if let Some(failure) = self.ffi_shape_failure(ty, &mut path)? {
                return Ok(Some(failure));
            }
            path.pop();
        }
        Ok(None)
    }

    fn provider_failure_value(
        message: impl Into<Arc<str>>,
    ) -> rue_air::SemanticProviderError<
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        rue_air::SemanticProviderError::Failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(message.into()),
        )
    }

    fn provider_failure<T>(
        message: impl Into<Arc<str>>,
    ) -> Result<
        T,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        Err(Self::provider_failure_value(message))
    }

    fn provider_domain_failure<T>(
        failure: crate::semantic_query_nucleus::SemanticNucleusFailure,
    ) -> Result<
        T,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        Err(rue_air::SemanticProviderError::Failure(failure))
    }

    pub(in crate::revisioned_query_database) fn type_carries_linear(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        match self.type_carries_linear_inner(ty, &mut OwnershipWalk::new())? {
            LinearOwnershipFact::DoesNotCarry => Ok(false),
            LinearOwnershipFact::Carries => Ok(true),
            LinearOwnershipFact::Deferred => Ok(false),
        }
    }

    /// The memoizing entry point every recursive call goes through.
    ///
    /// A nominal key is the only thing worth storing — reaching one costs a
    /// signature resolution — so anything else goes straight to the walk. The
    /// subtree's taint is measured on its own rather than inherited, then
    /// folded back into the caller's, so one recursive branch cannot suppress
    /// memoization of an unrelated sibling.
    fn type_carries_linear_inner(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        LinearOwnershipFact,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let crate::durable_semantics::DurableType::Nominal(key) = ty else {
            return self.type_carries_linear_walk(ty, walk);
        };
        if let Some(fact) = self
            .ownership_properties
            .get(key)
            .and_then(|properties| properties.carries_linear)
        {
            return Ok(fact);
        }
        let outer = std::mem::replace(&mut walk.tainted, false);
        let result = self.type_carries_linear_walk(ty, walk);
        let tainted = walk.tainted;
        walk.tainted = outer || tainted;
        if let Ok(fact) = &result
            && !tainted
        {
            self.ownership_properties
                .entry(key.clone())
                .or_default()
                .carries_linear = Some(*fact);
        }
        result
    }

    fn type_carries_linear_walk(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        LinearOwnershipFact,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;

        match ty {
            T::Array { len: 0, .. } => Ok(LinearOwnershipFact::DoesNotCarry),
            T::Array { element, .. } => self.type_carries_linear_inner(element, walk),
            T::Nominal(key) => {
                if !walk.visiting.insert(key.clone()) {
                    // Provisional: this key is already on the stack, so the
                    // answer belongs to that stack rather than to the type.
                    walk.taint();
                    return Ok(LinearOwnershipFact::DoesNotCarry);
                }
                let kind = match key.kind() {
                    crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                    crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                    _ => {
                        walk.visiting.remove(key);
                        return Self::provider_failure(format!(
                            "non-nominal definition `{}` used as a nominal type",
                            key.name()
                        ));
                    }
                };
                let candidate =
                    self.candidate(key.module(), key.name(), kind)?
                        .ok_or_else(|| {
                            Self::provider_failure_value(format!(
                                "nominal definition `{}` is unavailable",
                                key.name()
                            ))
                        })?;
                let signature_query = crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                    self.declaration_query(candidate.clone()),
                );
                let resolved = match self.resolved_signature(candidate) {
                    Ok(signature) => signature,
                    Err(rue_air::SemanticProviderError::Failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::SignatureReentry {
                            signature,
                            ..
                        },
                    )) if signature == *key => {
                        walk.visiting.remove(key);
                        // Not resolvable yet; a later request may answer
                        // differently, so nothing here is a stable property.
                        walk.taint();
                        return Ok(LinearOwnershipFact::Deferred);
                    }
                    Err(rue_air::SemanticProviderError::Abort(QueryAbort::Cycle(nodes)))
                        if nodes.iter().any(|node| {
                            node.family() == "compiler.semantic-nucleus"
                                && node.key() == signature_query.stable_identity()
                        }) =>
                    {
                        walk.visiting.remove(key);
                        // Not resolvable yet; a later request may answer
                        // differently, so nothing here is a stable property.
                        walk.taint();
                        return Ok(LinearOwnershipFact::Deferred);
                    }
                    Err(error) => {
                        walk.visiting.remove(key);
                        return Err(error);
                    }
                };
                self.merge_anonymous_projections(&resolved.anonymous_nominals)?;
                let signature = resolved.signature;
                let carries = match signature {
                    P::Struct {
                        fields, is_linear, ..
                    } => {
                        let mut carries = if is_linear {
                            LinearOwnershipFact::Carries
                        } else {
                            LinearOwnershipFact::DoesNotCarry
                        };
                        for (_, field) in fields.iter() {
                            carries = carries.combine(self.type_carries_linear_inner(field, walk)?);
                        }
                        carries
                    }
                    P::Enum { variants, .. } => {
                        let mut carries = LinearOwnershipFact::DoesNotCarry;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                carries =
                                    carries.combine(self.type_carries_linear_inner(field, walk)?);
                            }
                        }
                        carries
                    }
                    _ => {
                        walk.visiting.remove(key);
                        return Self::provider_failure(format!(
                            "nominal definition `{}` has a non-nominal signature",
                            key.name()
                        ));
                    }
                };
                walk.visiting.remove(key);
                Ok(carries)
            }
            T::AnonymousNominal(key) => {
                let Some(nominal) = self.anonymous_projection(key) else {
                    return Self::provider_failure(
                        "anonymous nominal is unavailable while checking linearity",
                    );
                };
                match nominal.shape {
                    S::Struct { fields, .. } => {
                        let mut carries = LinearOwnershipFact::DoesNotCarry;
                        for (_, field) in fields.iter() {
                            carries = carries.combine(self.type_carries_linear_inner(field, walk)?);
                        }
                        Ok(carries)
                    }
                    S::Enum { variants, .. } => {
                        let mut carries = LinearOwnershipFact::DoesNotCarry;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                carries =
                                    carries.combine(self.type_carries_linear_inner(field, walk)?);
                            }
                        }
                        Ok(carries)
                    }
                }
            }
            T::Slice { .. } | T::PtrConst(_) | T::PtrMut(_) => {
                Ok(LinearOwnershipFact::DoesNotCarry)
            }
            T::I8
            | T::I16
            | T::I32
            | T::I64
            | T::U8
            | T::U16
            | T::U32
            | T::U64
            | T::Bool
            | T::Unit
            | T::Never
            | T::ComptimeType
            | T::BuiltinNominal { .. }
            | T::Module(_)
            | T::GenericParameter(_) => Ok(LinearOwnershipFact::DoesNotCarry),
        }
    }

    pub(in crate::revisioned_query_database) fn type_has_drop_glue(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        self.type_has_drop_glue_inner(ty, &mut OwnershipWalk::new())
    }

    /// See [`Self::type_carries_linear_inner`] for why the memo is keyed on
    /// nominal types and why a tainted answer is not stored.
    fn type_has_drop_glue_inner(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let crate::durable_semantics::DurableType::Nominal(key) = ty else {
            return self.type_has_drop_glue_walk(ty, walk);
        };
        if let Some(has_glue) = self
            .ownership_properties
            .get(key)
            .and_then(|properties| properties.has_drop_glue)
        {
            return Ok(has_glue);
        }
        let outer = std::mem::replace(&mut walk.tainted, false);
        let result = self.type_has_drop_glue_walk(ty, walk);
        let tainted = walk.tainted;
        walk.tainted = outer || tainted;
        if let Ok(has_glue) = &result
            && !tainted
        {
            self.ownership_properties
                .entry(key.clone())
                .or_default()
                .has_drop_glue = Some(*has_glue);
        }
        result
    }

    fn type_has_drop_glue_walk(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;
        match ty {
            T::Array { len: 0, .. } => Ok(false),
            T::Array { element, .. } => self.type_has_drop_glue_inner(element, walk),
            T::Nominal(key) => {
                if !walk.visiting.insert(key.clone()) {
                    // Provisional; see `type_carries_linear_walk`.
                    walk.taint();
                    return Ok(false);
                }
                if key.kind() == crate::StableDefinitionKind::Struct {
                    let destructors = self
                        .context
                        .query_registered(
                            self.names,
                            LookupNameKey {
                                module: key.module().clone(),
                                namespace: DefinitionNamespace::Destructor,
                                name: Arc::from(key.name()),
                            },
                        )
                        .map_err(rue_air::SemanticProviderError::Abort)?;
                    let rue_query::QueryOutcome::Success(LookupNameValue(destructors)) =
                        destructors.outcome()
                    else {
                        unreachable!("LookupName publishes typed values")
                    };
                    if destructors.as_ref().is_ok_and(|facts| !facts.is_empty()) {
                        walk.visiting.remove(key);
                        return Ok(true);
                    }
                }
                let kind = match key.kind() {
                    crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                    crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                    _ => {
                        walk.visiting.remove(key);
                        return Ok(false);
                    }
                };
                let candidate = self
                    .candidate(key.module(), key.name(), kind)?
                    .ok_or_else(|| Self::provider_failure_value("nominal type is unavailable"))?;
                let signature = self.resolved_signature(candidate)?.signature;
                let has_glue = match signature {
                    P::Struct { fields, .. } => {
                        let mut has_glue = false;
                        for (_, field) in fields.iter() {
                            has_glue |= self.type_has_drop_glue_inner(field, walk)?;
                        }
                        has_glue
                    }
                    P::Enum { variants, .. } => {
                        let mut has_glue = false;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                has_glue |= self.type_has_drop_glue_inner(field, walk)?;
                            }
                        }
                        has_glue
                    }
                    _ => false,
                };
                walk.visiting.remove(key);
                Ok(has_glue)
            }
            T::AnonymousNominal(key) => {
                let nominal = self.anonymous_projection(key).ok_or_else(|| {
                    Self::provider_failure_value(
                        "anonymous nominal is unavailable while checking drop glue",
                    )
                })?;
                match nominal.shape {
                    S::Struct { fields, .. } => {
                        for (_, field) in fields.iter() {
                            if self.type_has_drop_glue_inner(field, walk)? {
                                return Ok(true);
                            }
                        }
                    }
                    S::Enum { variants, .. } => {
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                if self.type_has_drop_glue_inner(field, walk)? {
                                    return Ok(true);
                                }
                            }
                        }
                    }
                }
                Ok(false)
            }
            T::GenericParameter { .. } => Self::provider_failure(
                "generic parameter remained unresolved while checking drop glue",
            ),
            _ => Ok(false),
        }
    }

    fn type_is_copy(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        self.type_is_copy_inner(ty, &mut OwnershipWalk::new())
    }

    /// See [`Self::type_carries_linear_inner`] for why the memo is keyed on
    /// nominal types and why a tainted answer is not stored.
    fn type_is_copy_inner(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let crate::durable_semantics::DurableType::Nominal(key) = ty else {
            return self.type_is_copy_walk(ty, walk);
        };
        if let Some(is_copy) = self
            .ownership_properties
            .get(key)
            .and_then(|properties| properties.is_copy)
        {
            return Ok(is_copy);
        }
        let outer = std::mem::replace(&mut walk.tainted, false);
        let result = self.type_is_copy_walk(ty, walk);
        let tainted = walk.tainted;
        walk.tainted = outer || tainted;
        if let Ok(is_copy) = &result
            && !tainted
        {
            self.ownership_properties
                .entry(key.clone())
                .or_default()
                .is_copy = Some(*is_copy);
        }
        result
    }

    fn type_is_copy_walk(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;

        match ty {
            T::I8
            | T::I16
            | T::I32
            | T::I64
            | T::U8
            | T::U16
            | T::U32
            | T::U64
            | T::Bool
            | T::Unit
            | T::Never
            | T::ComptimeType
            | T::PtrConst(_)
            | T::PtrMut(_)
            | T::Module(_)
            | T::Slice { .. }
            | T::BuiltinNominal { .. } => Ok(true),
            T::GenericParameter(_) => {
                Self::provider_failure("unsubstituted generic parameter reached Copy validation")
            }
            T::Array { element, .. } => self.type_is_copy_inner(element, walk),
            T::Nominal(key) => {
                if !walk.visiting.insert(key.clone()) {
                    // Provisional; see `type_carries_linear_walk`.
                    walk.taint();
                    return Ok(true);
                }
                let kind = match key.kind() {
                    crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                    crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                    _ => {
                        walk.visiting.remove(key);
                        return Self::provider_failure(format!(
                            "non-nominal definition `{}` used as a nominal type",
                            key.name()
                        ));
                    }
                };
                let candidate =
                    self.candidate(key.module(), key.name(), kind)?
                        .ok_or_else(|| {
                            Self::provider_failure_value(format!(
                                "nominal definition `{}` is unavailable",
                                key.name()
                            ))
                        })?;
                let resolved = self.resolved_signature(candidate)?;
                self.merge_anonymous_projections(&resolved.anonymous_nominals)?;
                let is_copy = match resolved.signature {
                    P::Struct { is_copy, .. } => is_copy,
                    P::Enum { variants, .. } => {
                        let mut is_copy = true;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                is_copy &= self.type_is_copy_inner(field, walk)?;
                            }
                        }
                        is_copy
                    }
                    _ => {
                        walk.visiting.remove(key);
                        return Self::provider_failure(format!(
                            "nominal definition `{}` has a non-nominal signature",
                            key.name()
                        ));
                    }
                };
                walk.visiting.remove(key);
                Ok(is_copy)
            }
            T::AnonymousNominal(key) => {
                let nominal = self.anonymous_projection(key).ok_or_else(|| {
                    Self::provider_failure_value(
                        "anonymous nominal is unavailable while checking Copy",
                    )
                })?;
                match nominal.shape {
                    S::Struct { fields, .. } => {
                        for (_, field) in fields.iter() {
                            if !self.type_is_copy_inner(field, walk)? {
                                return Ok(false);
                            }
                        }
                    }
                    S::Enum { variants, .. } => {
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                if !self.type_is_copy_inner(field, walk)? {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
                Ok(true)
            }
        }
    }

    fn candidate(
        &self,
        module: &ModuleId,
        name: &str,
        kind: DefinitionKind,
    ) -> Result<
        Option<crate::declaration_candidate::DeclarationCandidateKey>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        self.candidate_from(&self.dependency_source, module, name, kind)
    }

    pub(in crate::revisioned_query_database) fn candidate_from(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
        kind: DefinitionKind,
    ) -> Result<
        Option<crate::declaration_candidate::DeclarationCandidateKey>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let terminal = self
            .context
            .query_registered(
                self.names,
                LookupNameKey {
                    module: module.clone(),
                    namespace: if kind == DefinitionKind::Destructor {
                        DefinitionNamespace::Destructor
                    } else {
                        DefinitionNamespace::ModuleItem
                    },
                    name: Arc::from(name),
                },
            )
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(LookupNameValue(result)) = terminal.outcome() else {
            unreachable!("LookupName publishes typed values")
        };
        let entries = result
            .as_ref()
            .map_err(|failure| Self::provider_failure_value(format!("{failure:?}")))?;
        let mut matching = entries.iter().filter(|entry| entry.kind == kind);
        let Some(entry) = matching.next() else {
            return Ok(None);
        };
        if matching.next().is_some() {
            return Self::provider_failure(format!(
                "ambiguous declaration `{name}` in module {module}"
            ));
        }
        let defining = rue_air::SemanticVisibilityDomain::from_file_path(Some(module.as_str()));
        let accessing = rue_air::SemanticVisibilityDomain::from_file_path(Some(
            accessing_source.module().as_str(),
        ));
        let is_public = entry.visibility == Some(rue_parser::ast::Visibility::Public);
        if !defining.is_visible_from(&accessing, is_public) {
            return Self::provider_domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::PrivateMemberAccess {
                        item_kind: format!("{kind:?}").to_lowercase(),
                        name: name.to_owned(),
                    },
                ),
            );
        }
        let categories: &[crate::declaration_candidate::DeclarationCandidateCategory] = match kind {
            DefinitionKind::Function => &[
                crate::declaration_candidate::DeclarationCandidateCategory::Function,
                crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction,
            ],
            DefinitionKind::Struct => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::Struct]
            }
            DefinitionKind::Enum => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::Enum]
            }
            DefinitionKind::Const => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate]
            }
            DefinitionKind::Destructor => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::Destructor]
            }
        };
        for category in categories {
            let key = crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category: *category,
                name: entry.name.clone(),
                owner: (*category
                    == crate::declaration_candidate::DeclarationCandidateCategory::Destructor)
                    .then(|| crate::declaration_candidate::DeclarationCandidateOwner {
                        category:
                            crate::declaration_candidate::DeclarationCandidateCategory::Struct,
                        name: entry.name.clone(),
                    }),
                duplicate_discriminator: 0,
            };
            let shell = self
                .context
                .query_registered(self.shells, DeclarationShellQueryKey(key.clone()))
                .map_err(rue_air::SemanticProviderError::Abort)?;
            let rue_query::QueryOutcome::Success(shell) = shell.outcome() else {
                unreachable!("DeclarationShell publishes typed values")
            };
            if matches!(shell, DeclarationShellQueryValue::Available(_)) {
                return Ok(Some(key));
            }
        }
        Self::provider_failure(format!(
            "name index and declaration-shell index disagree for `{name}`"
        ))
    }

    fn query(
        &self,
        key: crate::semantic_query_nucleus::SemanticNucleusKey,
    ) -> Result<
        crate::semantic_query_nucleus::SemanticNucleusValue,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let terminal = self
            .context
            .query_registered(self.family, key)
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("SemanticNucleus publishes typed values")
        };
        Ok(value.clone())
    }

    fn declaration_query(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: self.configuration.clone(),
        }
    }

    pub(in crate::revisioned_query_database) fn identity(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::DeclarationIdentityProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::{SemanticNucleusKey as K, SemanticNucleusValue as V};
        match self.query(K::Identity(self.declaration_query(declaration)))? {
            V::Identity(identity) => Ok(identity),
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("identity query returned the wrong projection"),
        }
    }

    pub(in crate::revisioned_query_database) fn const_resolution(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::ConstResolutionProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::{SemanticNucleusKey as K, SemanticNucleusValue as V};
        match self.query(K::ConstResolution(self.declaration_query(declaration)))? {
            V::ConstResolution(value) => Ok(value),
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("const query returned the wrong projection"),
        }
    }

    pub(in crate::revisioned_query_database) fn signature(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::DeclarationSignatureProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        Ok(self.resolved_signature(declaration)?.signature)
    }

    fn resolved_signature(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::ResolvedDeclarationSignature,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::{SemanticNucleusKey as K, SemanticNucleusValue as V};
        match self.query(K::Signature(self.declaration_query(declaration)))? {
            V::Signature(value) => Ok(value),
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("signature query returned the wrong projection"),
        }
    }

    pub(in crate::revisioned_query_database) fn validate_nominal_well_formedness(
        &mut self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        (),
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;

        fn collect_type(
            ty: &T,
            anonymous: &BTreeMap<
                crate::AnonymousNominalKey,
                crate::durable_semantics::DurableAnonymousNominal,
            >,
            neighbors: &mut BTreeSet<StableDefinitionKey>,
        ) {
            let mut pending = vec![ty];
            let mut seen_anonymous = BTreeSet::new();
            while let Some(ty) = pending.pop() {
                match ty {
                    T::Nominal(key) => {
                        neighbors.insert(key.clone());
                    }
                    // Arrays are inline containment edges even at length zero.
                    T::Array { element, .. } => pending.push(element),
                    T::AnonymousNominal(key) if seen_anonymous.insert(key.clone()) => {
                        if let Some(nominal) = anonymous.get(key) {
                            match &nominal.shape {
                                S::Struct { fields, .. } => {
                                    pending.extend(fields.iter().map(|(_, ty)| ty));
                                }
                                S::Enum { variants, .. } => {
                                    pending.extend(
                                        variants.iter().flat_map(|(_, payload)| payload.iter()),
                                    );
                                }
                            }
                        }
                    }
                    // Pointers and slices are indirection and therefore break
                    // the by-value containment graph.
                    T::PtrConst(_) | T::PtrMut(_) | T::Slice { .. } => {}
                    _ => {}
                }
            }
        }

        let root = self.identity(declaration.clone())?.key;
        if declaration.category
            == crate::declaration_candidate::DeclarationCandidateCategory::Struct
            && matches!(
                self.signature(declaration.clone())?,
                P::Struct { is_copy: true, .. }
            )
            && self
                .candidate(
                    &declaration.module,
                    &declaration.name,
                    DefinitionKind::Destructor,
                )?
                .is_some()
        {
            return Self::provider_domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::CopyStructWithDestructor {
                        type_name: declaration.name.to_string(),
                    },
                ),
            );
        }
        let mut colors = BTreeMap::<StableDefinitionKey, u8>::new();
        let mut path = vec![root.clone()];
        let mut frames = Vec::<(StableDefinitionKey, Vec<StableDefinitionKey>, usize)>::new();

        let load = |provider: &mut Self,
                    key: &StableDefinitionKey|
         -> Result<
            Vec<StableDefinitionKey>,
            rue_air::SemanticProviderError<
                QueryAbort,
                crate::semantic_query_nucleus::SemanticNucleusFailure,
            >,
        > {
            let kind = match key.kind() {
                crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                _ => return Ok(Vec::new()),
            };
            let Some(candidate) = provider.candidate(key.module(), key.name(), kind)? else {
                return Self::provider_failure(format!(
                    "nominal definition `{}` is unavailable",
                    key.name()
                ));
            };
            let resolved = provider.resolved_signature(candidate)?;
            let mut anonymous = BTreeMap::new();
            for nominal in resolved.anonymous_nominals.iter() {
                crate::durable_semantics::merge_anonymous_nominal(&mut anonymous, nominal)
                    .map_err(|identity| {
                        Self::provider_failure_value(format!(
                            "conflicting durable anonymous facts for {identity:?}"
                        ))
                    })?;
            }
            let mut neighbors = BTreeSet::new();
            match &resolved.signature {
                P::Struct { fields, .. } => {
                    for (_, ty) in fields.iter() {
                        collect_type(ty, &anonymous, &mut neighbors);
                    }
                }
                P::Enum { variants, .. } => {
                    for (_, payload) in variants.iter() {
                        for ty in payload.iter() {
                            collect_type(ty, &anonymous, &mut neighbors);
                        }
                    }
                }
                _ => {
                    return Self::provider_failure(format!(
                        "nominal definition `{}` has a non-nominal signature",
                        key.name()
                    ));
                }
            }
            Ok(neighbors.into_iter().collect())
        };

        colors.insert(root.clone(), 1);
        frames.push((root.clone(), load(self, &root)?, 0));
        while let Some((key, neighbors, next)) = frames.last_mut() {
            if *next == neighbors.len() {
                colors.insert(key.clone(), 2);
                frames.pop();
                path.pop();
                continue;
            }
            let child = neighbors[*next].clone();
            *next += 1;
            match colors.get(&child).copied() {
                Some(1) => {
                    let start = path.iter().position(|key| key == &child).unwrap_or(0);
                    let cycle = path[start..]
                        .iter()
                        .chain(std::iter::once(&child))
                        .map(|key| key.name())
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    return Self::provider_domain_failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            rue_error::ErrorKind::RecursiveTypeInfiniteSize {
                                name: child.name().to_owned(),
                                cycle,
                            },
                        ),
                    );
                }
                Some(2) => {}
                _ => {
                    colors.insert(child.clone(), 1);
                    path.push(child.clone());
                    frames.push((child.clone(), load(self, &child)?, 0));
                }
            }
        }
        Ok(())
    }

    fn constructor_fact(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<
            rue_air::SemanticTypeConstructorHead<
                StableDefinitionKey,
                Arc<str>,
                StableDefinitionKey,
            >,
        >,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::DeclarationSignatureProjection;
        let Some(candidate) = self.candidate(module, name, DefinitionKind::Function)? else {
            return Ok(None);
        };
        let identity = self.identity(candidate.clone())?;
        let signature = self.signature(candidate.clone())?;
        let DeclarationSignatureProjection::Callable {
            parameters, result, ..
        } = signature
        else {
            return Ok(None);
        };
        let shell = self
            .context
            .query_registered(self.shells, DeclarationShellQueryKey(candidate))
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(shell)) =
            shell.outcome()
        else {
            return Self::provider_failure("constructor shell became unavailable");
        };
        if shell.parameters.len() != parameters.len() {
            return Self::provider_failure("constructor parameter projections disagree");
        }
        let parameters = shell
            .parameters
            .iter()
            .zip(parameters.iter())
            .map(
                |(header, parameter)| rue_air::SemanticTypeConstructorParameter {
                    name: header.name.clone(),
                    is_comptime: parameter.is_comptime,
                    is_type: parameter.is_comptime
                        && parameter.ty == crate::durable_semantics::DurableType::ComptimeType,
                },
            )
            .collect::<Vec<_>>();
        self.dependencies.insert(
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.dependency_source.clone(),
                kind: self.dependency_kind,
                target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::TypeCallHead(
                    identity.key.clone(),
                ),
            },
        );
        Ok(Some(rue_air::SemanticTypeConstructorHead {
            key: identity.key.clone(),
            site: identity.key,
            parameters: parameters.into(),
            returns_type: result == crate::durable_semantics::DurableType::ComptimeType,
            is_public: identity.is_public,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }

    fn module_binding_fact(
        &self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<rue_air::SemanticModuleBinding<ModuleId, StableDefinitionKey>>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(candidate) = self.candidate(module, name, DefinitionKind::Const)? else {
            return Ok(None);
        };
        let resolution = self.const_resolution(candidate)?;
        let crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding { key, target } =
            resolution
        else {
            return Ok(None);
        };
        let shell = self.identity_key_visibility(&key)?;
        Ok(Some(rue_air::SemanticModuleBinding {
            target,
            site: key,
            is_public: shell,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }

    fn identity_key_visibility(
        &self,
        key: &StableDefinitionKey,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let category = match key.kind() {
            crate::StableDefinitionKind::Function => {
                crate::declaration_candidate::DeclarationCandidateCategory::Function
            }
            crate::StableDefinitionKind::Struct => {
                crate::declaration_candidate::DeclarationCandidateCategory::Struct
            }
            crate::StableDefinitionKind::Enum => {
                crate::declaration_candidate::DeclarationCandidateCategory::Enum
            }
            crate::StableDefinitionKind::ValueConst
            | crate::StableDefinitionKind::ModuleBinding => {
                crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate
            }
            _ => return Ok(false),
        };
        let candidate = crate::declaration_candidate::DeclarationCandidateKey {
            module: key.module().clone(),
            category,
            name: Arc::from(key.name()),
            owner: None,
            duplicate_discriminator: 0,
        };
        let terminal = self
            .context
            .query_registered(self.shells, DeclarationShellQueryKey(candidate))
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("DeclarationShell publishes typed values")
        };
        match value {
            DeclarationShellQueryValue::Available(shell) => Ok(shell.is_public),
            DeclarationShellQueryValue::Failure(failure) => {
                Self::provider_failure(format!("{failure:?}"))
            }
        }
    }

    fn named_fact(
        &self,
        module: &ModuleId,
        name: &str,
        kind: DefinitionKind,
    ) -> Result<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(candidate) = self.candidate(module, name, kind)? else {
            return Ok(None);
        };
        let identity = self.identity(candidate)?;
        Ok(Some(rue_air::SemanticTypeFact {
            value: crate::durable_semantics::DurableType::Nominal(identity.key.clone()),
            site: identity.key,
            is_public: identity.is_public,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }

    fn alias_fact(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(candidate) = self.candidate(module, name, DefinitionKind::Const)? else {
            return Ok(None);
        };
        let resolution = self.const_resolution(candidate)?;
        let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
            key,
            value,
            anonymous_nominals,
            dependencies,
            ..
        } = resolution
        else {
            return Ok(None);
        };
        let crate::durable_semantics::DurableConstValue::Type(value) = *value else {
            return Ok(None);
        };
        self.merge_anonymous_projections(&anonymous_nominals)?;
        self.dependencies.extend(dependencies.iter().cloned());
        let is_public = self.identity_key_visibility(&key)?;
        Ok(Some(rue_air::SemanticTypeFact {
            value,
            site: key,
            is_public,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }
}

impl rue_air::SemanticModulePathProvider<ModuleId, ModuleId, StableDefinitionKey>
    for SemanticNucleusTypeProvider<'_>
{
    type Abort = QueryAbort;
    type Failure = crate::semantic_query_nucleus::SemanticNucleusFailure;

    fn root_module_binding(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> Result<
        Option<rue_air::SemanticModuleBinding<ModuleId, StableDefinitionKey>>,
        rue_air::SemanticProviderError<Self::Abort, Self::Failure>,
    > {
        self.module_binding_fact(scope, name)
    }

    fn module_binding(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<rue_air::SemanticModuleBinding<ModuleId, StableDefinitionKey>>,
        rue_air::SemanticProviderError<Self::Abort, Self::Failure>,
    > {
        self.module_binding_fact(module, name)
    }

    fn module_display_name(&self, module: &ModuleId) -> Arc<str> {
        Arc::from(module.as_str())
    }

    fn accessing_domain(&self, scope: &ModuleId) -> rue_air::SemanticVisibilityDomain {
        rue_air::SemanticVisibilityDomain::from_file_path(Some(scope.as_str()))
    }
}

#[rustfmt::skip]
impl rue_air::SemanticTypeSyntaxProvider<ModuleId, ModuleId, StableDefinitionKey, StableDefinitionKey, Arc<str>, crate::durable_semantics::DurableType, crate::durable_semantics::DurableConstValue> for SemanticNucleusTypeProvider<'_> {
    fn with_comptime_substitutions<R>(
        &mut self,
        type_substitutions: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_substitutions: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
        operation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let new_types = type_substitutions.iter().cloned().collect();
        let new_values = value_substitutions.iter().cloned().collect();
        with_restored_state(
            self,
            |provider| {
                (
                    std::mem::replace(&mut provider.substitutions, new_types),
                    std::mem::replace(&mut provider.value_substitutions, new_values),
                )
            },
            operation,
            |provider, (previous_types, previous_values)| {
                provider.substitutions = previous_types;
                provider.value_substitutions = previous_values;
            },
        )
    }

    fn observe_selected_named_type(
        &mut self,
        _name: &str,
        kind: rue_air::SemanticTypeFactKind,
        fact: &rue_air::SemanticTypeFact<
            crate::durable_semantics::DurableType,
            StableDefinitionKey,
        >,
    ) -> rue_air::SemanticProviderResult<
        (),
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        if matches!(
            kind,
            rue_air::SemanticTypeFactKind::Struct
                | rue_air::SemanticTypeFactKind::Enum
                | rue_air::SemanticTypeFactKind::Constant
        ) {
            self.dependencies.insert(
                crate::semantic_query_nucleus::SemanticDeclarationDependency {
                    source: self.dependency_source.clone(),
                    kind: self.dependency_kind,
                    target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                        fact.site.clone(),
                    ),
                },
            );
        }
        Ok(())
    }

    fn observe_materialized_type(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        (),
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        fn collect(
            ty: &crate::durable_semantics::DurableType,
            output: &mut Vec<StableDefinitionKey>,
        ) {
            match ty {
                crate::durable_semantics::DurableType::Nominal(key) => output.push(key.clone()),
                crate::durable_semantics::DurableType::Array { element, .. }
                | crate::durable_semantics::DurableType::Slice { element, .. }
                | crate::durable_semantics::DurableType::PtrConst(element)
                | crate::durable_semantics::DurableType::PtrMut(element) => {
                    collect(element, output)
                }
                _ => {}
            }
        }
        let mut targets = Vec::new();
        collect(ty, &mut targets);
        self.dependencies.extend(targets.into_iter().map(|target| {
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.dependency_source.clone(),
                kind: self.dependency_kind,
                target:
                    crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                        target,
                    ),
            }
        }));
        Ok(())
    }

    fn substituted_type(
        &mut self,
        _scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(self.substitutions.get(name).cloned())
    }

    fn primitive_type(
        &mut self,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        use crate::durable_semantics::DurableType as T;
        Ok(Some(match name {
            "i8" => T::I8,
            "i16" => T::I16,
            "i32" => T::I32,
            "i64" => T::I64,
            "isize" => T::I64,
            "u8" => T::U8,
            "u16" => T::U16,
            "u32" => T::U32,
            "u64" => T::U64,
            "usize" => T::U64,
            "bool" => T::Bool,
            "()" => T::Unit,
            "!" => T::Never,
            "type" => T::ComptimeType,
            _ => return Ok(None),
        }))
    }

    fn builtin_type(
        &mut self,
        _scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(
            (name == "str").then(|| crate::durable_semantics::DurableType::BuiltinNominal {
                name: Arc::from("str"),
                kind: rue_air::SemanticImportNominalKind::Struct,
            }),
        )
    }

    fn root_struct_type(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(scope, name, DefinitionKind::Struct)
    }
    fn root_enum_type(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(scope, name, DefinitionKind::Enum)
    }
    fn root_type_alias(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.alias_fact(scope, name)
    }
    fn module_struct_type(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(module, name, DefinitionKind::Struct)
    }
    fn module_enum_type(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(module, name, DefinitionKind::Enum)
    }
    fn module_type_alias(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.alias_fact(module, name)
    }

    fn resolve_array_length(
        &mut self,
        scope: &ModuleId,
        length: rue_air::SemanticValueSyntax<'_>,
    ) -> rue_air::SemanticProviderResult<
        Option<u64>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        match length {
            rue_air::SemanticValueSyntax::Integer(value) => u64::try_from(value)
                .map(Some)
                .map_err(|_| {
                    rue_air::SemanticProviderError::Failure(durable_literal_array_length_failure(
                        value,
                    ))
                }),
            rue_air::SemanticValueSyntax::Name(name) => {
                if let Some(value) = self.value_substitutions.get(name) {
                    return crate::durable_comptime::durable_named_array_length_const(value)
                        .map(Some)
                        .map_err(|error| {
                            rue_air::SemanticProviderError::Failure(
                                durable_provider_named_array_length_failure(name, error),
                            )
                        });
                }
                if let Some(ty) = self.deferred_value_parameters.get(name) {
                    if matches!(
                        ty,
                        crate::durable_semantics::DurableType::I8
                            | crate::durable_semantics::DurableType::I16
                            | crate::durable_semantics::DurableType::I32
                            | crate::durable_semantics::DurableType::I64
                            | crate::durable_semantics::DurableType::U8
                            | crate::durable_semantics::DurableType::U16
                            | crate::durable_semantics::DurableType::U32
                            | crate::durable_semantics::DurableType::U64
                    ) {
                        return Ok(None);
                    }
                    return Self::provider_domain_failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            rue_error::ErrorKind::InvalidArrayLength {
                                reason: format!(
                                    "array length expression '{name}' has non-integer type {}",
                                    durable_type_diagnostic_name(ty),
                                ),
                            },
                        ),
                    );
                }
                let Some(candidate) = self.candidate(scope, name, DefinitionKind::Const)? else {
                    return Self::provider_failure(format!("unknown array length `{name}`"));
                };
                let resolution = self.const_resolution(candidate)?;
                let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                    value,
                    ..
                } = resolution
                else {
                    return Self::provider_failure(format!(
                        "array length `{name}` is not an integer"
                    ));
                };
                crate::durable_comptime::durable_named_array_length_const(&value)
                    .map(Some)
                    .map_err(|error| {
                        rue_air::SemanticProviderError::Failure(
                            durable_provider_named_array_length_failure(name, error),
                        )
                    })
            }
        }
    }

    fn array_length_from_value(
        &mut self,
        _scope: &ModuleId,
        value: &crate::durable_semantics::DurableConstValue,
    ) -> rue_air::SemanticProviderResult<
        Option<u64>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        let crate::durable_semantics::DurableConstValue::Integer(value) = value else {
            return Self::provider_failure("array length is not an integer");
        };
        let value = *value;
        u64::try_from(value).map(Some).map_err(|_| {
            rue_air::SemanticProviderError::Failure(durable_literal_array_length_failure(value))
        })
    }

    fn array_type(
        &mut self,
        element: crate::durable_semantics::DurableType,
        length: Option<u64>,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(match length {
            Some(len) => crate::durable_semantics::DurableType::Array {
                element: Arc::new(element),
                len,
            },
            None => crate::durable_semantics::DurableType::ComptimeType,
        })
    }
    fn ptr_const_type(
        &mut self,
        pointee: crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(crate::durable_semantics::DurableType::PtrConst(Arc::new(
            pointee,
        )))
    }
    fn ptr_mut_type(
        &mut self,
        pointee: crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(crate::durable_semantics::DurableType::PtrMut(Arc::new(
            pointee,
        )))
    }
    fn slice_type(
        &mut self,
        _scope: &ModuleId,
        syntax: &str,
        element: crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(crate::durable_semantics::DurableType::Slice {
            element: Arc::new(element),
            name: Arc::from(syntax),
        })
    }
    fn builtin_type_call(
        &mut self,
        _scope: &ModuleId,
        name: &str,
        arguments: &[rue_air::SemanticValueSyntax<'_>],
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        if name != "Str" {
            return Ok(None);
        }
        let [capacity] = arguments else {
            return Self::provider_failure("Str expects one capacity argument");
        };
        let capacity = match capacity {
            rue_air::SemanticValueSyntax::Integer(capacity) => u64::try_from(*capacity)
                .map_err(|_| Self::provider_failure_value("Str capacity must be an integer"))?,
            rue_air::SemanticValueSyntax::Name(capacity) => capacity
                .parse::<u64>()
                .map_err(|_| Self::provider_failure_value("Str capacity must be an integer"))?,
        };
        self.dependencies.insert(
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.dependency_source.clone(),
                kind: self.dependency_kind,
                target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::BuiltinTypeCallHead(
                    rue_air::BuiltinTypeCallHead::FixedCapacityString,
                ),
            },
        );
        Ok(Some(
            crate::durable_semantics::DurableType::BuiltinNominal {
                name: Arc::from(format!("Str({capacity})")),
                kind: rue_air::SemanticImportNominalKind::Struct,
            },
        ))
    }
    fn root_constructor(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeConstructorHead<
                StableDefinitionKey,
                Arc<str>,
                StableDefinitionKey,
            >,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.constructor_fact(scope, name)
    }
    fn module_constructor(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeConstructorHead<
                StableDefinitionKey,
                Arc<str>,
                StableDefinitionKey,
            >,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.constructor_fact(module, name)
    }
    fn resolve_value_argument(
        &mut self,
        scope: &ModuleId,
        _constructor: &str,
        head: &rue_air::SemanticTypeConstructorHead<
            StableDefinitionKey,
            Arc<str>,
            StableDefinitionKey,
        >,
        parameter_index: usize,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
        syntax: rue_air::SemanticValueSyntax<'_>,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableConstValue,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        use crate::durable_semantics::DurableConstValue as V;
        let syntax = match syntax {
            rue_air::SemanticValueSyntax::Integer(value) => return Ok(V::Integer(value)),
            rue_air::SemanticValueSyntax::Name(syntax) => syntax,
        };
        if syntax == "true" || syntax == "false" {
            return Ok(V::Bool(syntax == "true"));
        }
        if let Some((_, value)) = value_arguments
            .iter()
            .find(|(name, _)| name.as_ref() == syntax)
        {
            return Ok(value.clone());
        }
        if let Some((_, ty)) = type_arguments
            .iter()
            .find(|(name, _)| name.as_ref() == syntax)
        {
            return Ok(V::Type(ty.clone()));
        }
        if let Some(ty) = self.deferred_value_parameters.get(syntax) {
            return match ty {
                crate::durable_semantics::DurableType::I8
                | crate::durable_semantics::DurableType::I16
                | crate::durable_semantics::DurableType::I32
                | crate::durable_semantics::DurableType::I64
                | crate::durable_semantics::DurableType::U8
                | crate::durable_semantics::DurableType::U16
                | crate::durable_semantics::DurableType::U32
                | crate::durable_semantics::DurableType::U64 => Ok(V::Integer(0)),
                crate::durable_semantics::DurableType::Bool => Ok(V::Bool(false)),
                crate::durable_semantics::DurableType::Unit => Ok(V::Unit),
                _ => Self::provider_failure(format!(
                    "comptime parameter `{syntax}` has unsupported declared type {}",
                    durable_type_diagnostic_name(ty),
                )),
            };
        }
        if let Some(value) = self.value_substitutions.get(syntax) {
            return Ok(value.clone());
        }
        if let Some(ty) = self.substitutions.get(syntax) {
            return Ok(V::Type(ty.clone()));
        }
        if let Some(candidate) = self.candidate(scope, syntax, DefinitionKind::Const)? {
            if let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                value, ..
            } = self.const_resolution(candidate)?
            {
                return Ok(*value);
            }
        }
        let parameter = head
            .parameters
            .get(parameter_index)
            .map(|parameter| parameter.name.as_ref())
            .unwrap_or("?");
        Self::provider_failure(format!(
            "argument for comptime parameter `{parameter}` must be a compile-time known value"
        ))
    }
    fn reduce_comptime_call(
        &mut self,
        head: &rue_air::SemanticTypeConstructorHead<
            StableDefinitionKey,
            Arc<str>,
            StableDefinitionKey,
        >,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticComptimeCallResult<
                crate::durable_semantics::DurableType,
                crate::durable_semantics::DurableConstValue,
            >,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        use crate::semantic_query_nucleus::{
            ComptimeCallQueryKey, ComptimeCallResultProjection as P, DeclarationSemanticQueryKey,
            SemanticNucleusKey as K, SemanticNucleusValue as V,
        };
        let declaration = crate::declaration_candidate::DeclarationCandidateKey {
            module: head.key.module().clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name: Arc::from(head.key.name()),
            owner: None,
            duplicate_discriminator: 0,
        };
        let signature = self.signature(declaration.clone())?;
        let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
            parameters,
            ..
        } = signature
        else {
            return Self::provider_failure("type constructor has a non-callable signature");
        };
        let concrete_type_arguments = type_arguments
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>();
        for (name, value) in value_arguments {
            let Some((_, parameter)) = head
                .parameters
                .iter()
                .zip(parameters.iter())
                .find(|(header, _)| &header.name == name)
            else {
                return Self::provider_failure("comptime value argument has no parameter");
            };
            let expected = substitute_durable_generics(&parameter.ty, &concrete_type_arguments);
            if let Some(failure) =
                crate::durable_comptime::durable_structured_value_fit_failure(value, &expected)
            {
                return Self::provider_domain_failure(failure);
            }
        }
        let query = K::ComptimeCall(ComptimeCallQueryKey {
            declaration: DeclarationSemanticQueryKey {
                declaration,
                configuration: self.configuration.clone(),
            },
            type_arguments: type_arguments.to_vec().into(),
            value_arguments: value_arguments.to_vec().into(),
        });
        let _depth = SemanticComptimeCallDepthGuard::enter(head.key.name()).map_err(
            |error| match error {
                EvaluateSemanticConstError::Failure(failure) => {
                    rue_air::SemanticProviderError::Failure(*failure)
                }
                EvaluateSemanticConstError::Abort(abort) => {
                    rue_air::SemanticProviderError::Abort(abort)
                }
            },
        )?;
        let queried = self.query(query)?;
        match queried {
            V::ComptimeCall(value) => {
                let mut effects = crate::durable_comptime::DurableComptimeEffects::default();
                effects.merge_projection(
                    &value.anonymous_nominals,
                    &value.dependencies,
                    &value.deferred_ownership,
                    &crate::durable_comptime::DurableComptimeApplicationPolicy::preserve(),
                );
                self.merge_comptime_effects(
                    effects,
                    &crate::durable_comptime::DurableComptimeApplicationPolicy::preserve(),
                );
                match value.result {
                    P::Type(value) => Ok(Some(rue_air::SemanticComptimeCallResult::Type(value))),
                    P::Value(value) => Ok(Some(rue_air::SemanticComptimeCallResult::Value(value))),
                }
            }
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("comptime query returned the wrong projection"),
        }
    }
}

pub(in crate::revisioned_query_database) enum ResolveSemanticSignatureError {
    Abort(QueryAbort),
    Failure(Box<crate::semantic_query_nucleus::SemanticNucleusFailure>),
}

impl ResolveSemanticSignatureError {
    fn failure(failure: crate::semantic_query_nucleus::SemanticNucleusFailure) -> Self {
        Self::Failure(Box::new(failure))
    }
}

/// Resolve one retained type root of a declaration signature.
fn resolve_signature_type_root(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    module: &ModuleId,
    syntax: &rue_rir::RirTypeSyntaxArena<Arc<str>>,
    root: rue_rir::RirTypeSyntaxRef,
) -> Result<crate::durable_semantics::DurableType, ResolveSemanticSignatureError> {
    provider.dependency_kind = rue_air::DeclarationTypeDependencyKind::Signature;
    rue_air::resolve_structured_semantic_type_syntax(provider, module, syntax, root)
        .map_err(semantic_type_query_failure)
}

/// Whether a durable nominal key names an `interface` shell (spec 6.7),
/// read from the declaration shell so the answer needs neither the
/// interface's nor the asking declaration's resolved signature.
fn is_interface_shell(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    key: &StableDefinitionKey,
) -> Result<bool, ResolveSemanticSignatureError> {
    if key.kind() != crate::StableDefinitionKind::Struct {
        return Ok(false);
    }
    let shell = provider
        .context
        .query_registered(
            provider.shells,
            DeclarationShellQueryKey(crate::declaration_candidate::DeclarationCandidateKey {
                module: key.module().clone(),
                category: crate::declaration_candidate::DeclarationCandidateCategory::Struct,
                name: Arc::from(key.name()),
                owner: None,
                duplicate_discriminator: 0,
            }),
        )
        .map_err(ResolveSemanticSignatureError::Abort)?;
    let rue_query::QueryOutcome::Success(shell) = shell.outcome() else {
        unreachable!("DeclarationShell publishes typed values")
    };
    Ok(matches!(
        shell,
        DeclarationShellQueryValue::Available(shell) if shell.is_interface
    ))
}

/// The nucleus failure the type resolver reports for a name it cannot
/// resolve, when `error` is that failure.
fn unknown_type_name(error: &ResolveSemanticSignatureError) -> Option<String> {
    let ResolveSemanticSignatureError::Failure(failure) = error else {
        return None;
    };
    match failure.as_ref() {
        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::UnknownType(name),
        ) => Some(name.clone()),
        _ => None,
    }
}

/// Resolve a type root that must name an interface (a bound, a header
/// assertion, a refinement, a freestanding assertion; spec 6.7:18). An
/// unresolvable name is E0300 and any other type is E0306, both anchored
/// by `anchor`.
fn resolve_interface_reference(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    module: &ModuleId,
    syntax: &rue_rir::RirTypeSyntaxArena<Arc<str>>,
    root: rue_rir::RirTypeSyntaxRef,
    anchor: &dyn Fn(rue_error::ErrorKind) -> ResolveSemanticSignatureError,
) -> Result<StableDefinitionKey, ResolveSemanticSignatureError> {
    let rendered = || syntax.render_type(root).unwrap_or_default();
    let resolved = match resolve_signature_type_root(provider, module, syntax, root) {
        Ok(resolved) => resolved,
        Err(error) => {
            return Err(match unknown_type_name(&error) {
                Some(name) => anchor(rue_error::ErrorKind::InterfaceNotFound { name }),
                None => error,
            });
        }
    };
    match resolved {
        crate::durable_semantics::DurableType::Nominal(key)
            if is_interface_shell(provider, &key)? =>
        {
            Ok(key)
        }
        _ => Err(anchor(rue_error::ErrorKind::BoundIsNotAnInterface {
            name: rendered(),
        })),
    }
}

/// Classify a comptime parameter that is not declared `: type` (spec
/// 6.7:14): its interface bound when it names one or more interfaces, or
/// empty when it is an ordinary comptime value parameter. A composed bound
/// (`A + B`) is always a bound, so each name in it must be an interface; a
/// single name is a bound only when it resolves to an interface, except that
/// a struct name — never a comptime value type — is E0306 under the preview.
fn classify_interface_bounds(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    module: &ModuleId,
    syntax: &rue_rir::RirTypeSyntaxArena<Arc<str>>,
    parameter: &crate::semantic_query_nucleus::ParsedSemanticParameter,
    ordinal: u32,
) -> Result<Arc<[StableDefinitionKey]>, ResolveSemanticSignatureError> {
    let at_parameter = |kind| {
        ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticAtParameter {
                kind,
                ordinal,
            },
        )
    };
    let composed = !parameter.bounds.is_empty();
    let preview = provider
        .configuration
        .preview_features
        .contains(rue_error::PreviewFeature::Interfaces);
    let mut keys = Vec::with_capacity(1 + parameter.bounds.len());
    for root in std::iter::once(parameter.ty).chain(parameter.bounds.iter().copied()) {
        if composed || !keys.is_empty() {
            keys.push(resolve_interface_reference(
                provider,
                module,
                syntax,
                root,
                &at_parameter,
            )?);
            continue;
        }
        // A single name: only an interface makes it a bound.
        let Ok(crate::durable_semantics::DurableType::Nominal(key)) =
            resolve_signature_type_root(provider, module, syntax, root)
        else {
            return Ok(Arc::from([]));
        };
        if is_interface_shell(provider, &key)? {
            keys.push(key);
        } else if preview && key.kind() == crate::StableDefinitionKind::Struct {
            return Err(at_parameter(rue_error::ErrorKind::BoundIsNotAnInterface {
                name: syntax.render_type(root).unwrap_or_default(),
            }));
        } else {
            return Ok(Arc::from([]));
        }
    }
    if !preview {
        return Err(at_parameter(rue_error::ErrorKind::PreviewFeatureRequired {
            feature: rue_error::PreviewFeature::Interfaces,
            what: "an interface bound on a comptime parameter".to_owned(),
        }));
    }
    Ok(keys.into())
}

/// The diagnostic for an interface where a type is required (spec 6.7:18),
/// as the nucleus reports it for a signature or field position: the kind
/// and the help text, for the anchor to compose.
fn reject_interface_type(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    ty: &crate::durable_semantics::DurableType,
    anchor: impl Fn(rue_error::ErrorKind, String) -> ResolveSemanticSignatureError,
) -> Result<(), ResolveSemanticSignatureError> {
    use crate::durable_semantics::DurableType as T;
    let mut pending = vec![ty];
    while let Some(ty) = pending.pop() {
        match ty {
            T::Nominal(key) => {
                if is_interface_shell(provider, key)? {
                    let name = key.name();
                    return Err(anchor(
                        rue_error::ErrorKind::TypeMismatch {
                            expected: "a type".to_owned(),
                            found: format!("interface `{name}`"),
                        },
                        format!(
                            "an interface is not a type; bound a comptime type parameter with it instead: `comptime T: {name}`"
                        ),
                    ));
                }
            }
            T::Array { element, .. }
            | T::Slice { element, .. }
            | T::PtrConst(element)
            | T::PtrMut(element) => pending.push(element),
            _ => {}
        }
    }
    Ok(())
}

/// Resolve the parsed interface facts of a struct-shaped declaration (spec
/// 6.7): the preview gate (6.7:3), each asserted or refined interface
/// (6.7:9, 6.7:7), the associated type declarations or type-valued
/// requirements, and the requirement names.
fn resolve_conformance_facts(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    module: &ModuleId,
    syntax: &rue_rir::RirTypeSyntaxArena<Arc<str>>,
    parsed: &crate::semantic_query_nucleus::ParsedConformanceFacts,
) -> Result<crate::durable_semantics::DurableConformanceFacts, ResolveSemanticSignatureError> {
    if !parsed.uses_interfaces() {
        return Ok(crate::durable_semantics::DurableConformanceFacts::default());
    }
    let producer = declaration_candidate_for_stable_key(&provider.dependency_source)
        .expect("struct signature has a declaration candidate");
    let at_producer_range = |kind, start, end| {
        ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticAtProducerRange {
                kind,
                producer: producer.clone(),
                start,
                end,
            },
        )
    };
    let at_declaration = |kind| {
        ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(kind),
        )
    };
    if !provider
        .configuration
        .preview_features
        .contains(rue_error::PreviewFeature::Interfaces)
    {
        let kind = rue_error::ErrorKind::PreviewFeatureRequired {
            feature: rue_error::PreviewFeature::Interfaces,
            what: if parsed.is_interface {
                "an `interface` declaration".to_owned()
            } else if !parsed.conformances.is_empty() {
                "a conformance assertion".to_owned()
            } else {
                "an associated type declaration".to_owned()
            },
        };
        // A header assertion is gated at its `is` list; an interface or a
        // lone associated type declaration at the whole declaration.
        return Err(
            match (parsed.conformances.first(), parsed.conformances.last()) {
                (Some(first), Some(last)) if !parsed.is_interface => {
                    at_producer_range(kind, first.start, last.end)
                }
                _ => at_declaration(kind),
            },
        );
    }
    let mut conformances = Vec::with_capacity(parsed.conformances.len());
    for conformance in parsed.conformances.iter() {
        let interface = resolve_interface_reference(
            provider,
            module,
            syntax,
            conformance.interface,
            &|kind| at_producer_range(kind, conformance.start, conformance.end),
        )?;
        conformances.push(rue_air::DurableConformance {
            interface,
            start: conformance.start,
            end: conformance.end,
        });
    }
    let mut assoc_types = Vec::with_capacity(parsed.assoc_types.len());
    for (name, root) in parsed.assoc_types.iter() {
        let name: Arc<str> = Arc::from(
            syntax
                .symbol(*name)
                .expect("signature symbols are validated when projected")
                .as_ref(),
        );
        let ty = resolve_signature_type_root(provider, module, syntax, *root)?;
        reject_interface_type(provider, &ty, |kind, help| {
            ResolveSemanticSignatureError::failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticWithHelp {
                    kind,
                    help: Arc::from(help),
                },
            )
        })?;
        assoc_types.push((name, ty));
    }
    let requirements = parsed
        .requirements
        .iter()
        .map(|name| {
            Arc::from(
                syntax
                    .symbol(*name)
                    .expect("signature symbols are validated when projected")
                    .as_ref(),
            )
        })
        .collect::<Vec<Arc<str>>>();
    Ok(crate::durable_semantics::DurableConformanceFacts {
        is_interface: parsed.is_interface,
        conformances: conformances.into(),
        assoc_types: assoc_types.into(),
        requirements: requirements.into(),
    })
}

/// Resolve the freestanding conformance assertions of one module (spec
/// 6.7:9): each `Type is I + J;` item's subject and interfaces, under the
/// interfaces preview gate (spec 6.7:3). Every diagnostic is anchored at the
/// assertion's own source range.
pub(in crate::revisioned_query_database) fn resolve_module_conformances(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    module: &ModuleId,
    parsed: &crate::parsed_modules::ParsedModule,
) -> Result<Vec<crate::durable_semantics::DurableConformanceAssertion>, ResolveSemanticSignatureError>
{
    let at_range = |kind, span: rue_span::Span| {
        ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticAtModuleRange {
                kind,
                module: module.clone(),
                start: span.start,
                end: span.end,
            },
        )
    };
    let preview = provider
        .configuration
        .preview_features
        .contains(rue_error::PreviewFeature::Interfaces);
    let mut assertions = Vec::new();
    for item in &parsed.ast().items {
        let rue_parser::ast::Item::Conformance(assertion) = item else {
            continue;
        };
        if !preview {
            return Err(at_range(
                rue_error::ErrorKind::PreviewFeatureRequired {
                    feature: rue_error::PreviewFeature::Interfaces,
                    what: "a conformance assertion".to_owned(),
                },
                assertion.span,
            ));
        }
        let mut builder = rue_rir::RirTypeSyntaxBuilder::default();
        let resolve_symbol = |symbol| Arc::from(parsed.resolve_raw_symbol(symbol));
        let subject_root = builder
            .push_parser_type(&assertion.subject, resolve_symbol)
            .map_err(|error| {
                ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Syntax(Arc::from(
                        format!(
                            "conformance assertion syntax exceeds the supported size: {error:?}"
                        ),
                    )),
                )
            })?;
        let interface_roots = assertion
            .interfaces
            .iter()
            .map(|interface| {
                builder
                    .push_parser_type(interface, resolve_symbol)
                    .map(|root| (root, interface.span()))
                    .map_err(|error| {
                        ResolveSemanticSignatureError::failure(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Syntax(
                                Arc::from(format!(
                                    "conformance assertion syntax exceeds the supported size: {error:?}"
                                )),
                            ),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let syntax = builder.finish();
        let subject = resolve_signature_type_root(provider, module, &syntax, subject_root)
            .map_err(|error| match error {
                ResolveSemanticSignatureError::Failure(failure) => match *failure {
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(kind) => {
                        at_range(kind, assertion.subject.span())
                    }
                    other => ResolveSemanticSignatureError::failure(other),
                },
                abort => abort,
            })?;
        reject_interface_type(provider, &subject, |kind, _| {
            at_range(kind, assertion.subject.span())
        })?;
        let mut interfaces = Vec::with_capacity(interface_roots.len());
        for (root, span) in interface_roots {
            interfaces.push(resolve_interface_reference(
                provider,
                module,
                &syntax,
                root,
                &|kind| at_range(kind, span),
            )?);
        }
        assertions.push(crate::durable_semantics::DurableConformanceAssertion {
            subject,
            interfaces: interfaces.into(),
            module: module.clone(),
            start: assertion.span.start,
            end: assertion.span.end,
        });
    }
    Ok(assertions)
}

pub(in crate::revisioned_query_database) fn semantic_type_query_failure(
    failure: rue_air::SemanticTypeSyntaxError<
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
        StableDefinitionKey,
        Arc<str>,
    >,
) -> ResolveSemanticSignatureError {
    use rue_air::SemanticTypeSyntaxFailure as F;
    use rue_error::ErrorKind;

    match crate::durable_comptime::classify_durable_type_syntax_failure(failure) {
        crate::durable_comptime::DurableTypeSyntaxClassification::Abort(abort) => {
            ResolveSemanticSignatureError::Abort(abort)
        }
        crate::durable_comptime::DurableTypeSyntaxClassification::Failure(failure) => {
            ResolveSemanticSignatureError::failure(failure)
        }
        crate::durable_comptime::DurableTypeSyntaxClassification::Semantic(failure) => {
            match failure {
                F::UnknownType { syntax } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::UnknownType(syntax.to_string()),
                    ),
                ),
                F::UnknownModuleMember { module, member, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            ErrorKind::UnknownModuleMember {
                                module_name: module.to_string(),
                                member_name: member.to_string(),
                            },
                        ),
                    )
                }
                F::ValueWhereTypeExpected { parameter, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                            Arc::from(format!(
                                "argument for comptime parameter `{parameter}` must be a type"
                            )),
                        ),
                    )
                }
                F::UnknownConstructor {
                    constructor,
                    expectation: rue_air::SemanticComptimeCallExpectation::Type,
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::UnknownType(format!("{constructor}(...)")),
                    ),
                ),
                F::UnknownConstructor {
                    constructor,
                    expectation: rue_air::SemanticComptimeCallExpectation::Value,
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "`{constructor}` is not a function; a compile-time value call requires a value-returning comptime function"
                            ),
                        },
                    ),
                ),
                F::InvalidConstructorArity {
                    constructor,
                    expected,
                    found,
                    expectation: rue_air::SemanticComptimeCallExpectation::Type,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                        format!(
                            "type constructor `{constructor}` expects {expected} comptime type argument(s), but {found} provided"
                        ),
                    )),
                ),
                F::InvalidConstructorArity {
                    constructor,
                    expected,
                    found,
                    expectation: rue_air::SemanticComptimeCallExpectation::Value,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "value-returning comptime function `{constructor}` expects {expected} comptime {}, but {found} {} provided",
                                if expected == 1 {
                                    "argument"
                                } else {
                                    "arguments"
                                },
                                if found == 1 { "was" } else { "were" },
                            ),
                        },
                    ),
                ),
                F::NotTypeConstructor { constructor, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                            Arc::from(format!("function `{constructor}` is not a type")),
                        ),
                    )
                }
                F::TypeWhereValueExpected { constructor, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "`{constructor}` returns `type` and cannot be used where a compile-time value is required"
                                ),
                            },
                        ),
                    )
                }
                F::RuntimeConstructorParameter {
                    constructor,
                    expectation: rue_air::SemanticComptimeCallExpectation::Type,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                        format!(
                            "type constructor `{constructor}` cannot have runtime parameters; all parameters must be `comptime`"
                        ),
                    )),
                ),
                F::RuntimeConstructorParameter {
                    constructor,
                    expectation: rue_air::SemanticComptimeCallExpectation::Value,
                    expected,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: if expected == 0 {
                                format!(
                                    "call `{constructor}(...)` is not a compile-time value because its callee must declare at least one `comptime` parameter"
                                )
                            } else {
                                format!(
                                    "call `{constructor}(...)` is not a compile-time value because all parameters must be `comptime`"
                                )
                            },
                        },
                    ),
                ),
                F::ConstructorDidNotReduce { constructor, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "the type constructor `{constructor}` did not reduce to a concrete type at compile time"
                                ),
                            },
                        ),
                    )
                }
                F::PrivateItem {
                    kind,
                    name,
                    defining_file,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::PrivateUnqualifiedAccess(Box::new(
                            rue_error::PrivateUnqualifiedAccessData {
                                item_kind: kind.diagnostic_name().to_owned(),
                                name: name.to_string(),
                                defining_file: defining_file.to_string(),
                            },
                        )),
                    ),
                ),
                F::AmbiguousItem { name, .. } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!("type resolution is ambiguous for `{name}`"),
                        },
                    ),
                ),
                F::Path(path) => match path {
                    rue_air::SemanticModulePathFailure::Empty => {
                        ResolveSemanticSignatureError::failure(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "type path is empty".to_owned(),
                                },
                            ),
                        )
                    }
                    rue_air::SemanticModulePathFailure::UnknownRoot { name } => {
                        ResolveSemanticSignatureError::failure(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                                ErrorKind::UnknownType(name.to_string()),
                            ),
                        )
                    }
                    rue_air::SemanticModulePathFailure::UnknownMember {
                        module, member, ..
                    } => ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            ErrorKind::UnknownModuleMember {
                                module_name: module.to_string(),
                                member_name: member.to_string(),
                            },
                        ),
                    ),
                    rue_air::SemanticModulePathFailure::PrivateMember { member, .. } => {
                        ResolveSemanticSignatureError::failure(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: format!(
                                        "private module member `{member}` cannot be used in a type path"
                                    ),
                                },
                            ),
                        )
                    }
                },
            }
        }
    }
}

pub(in crate::revisioned_query_database) fn resolve_parsed_semantic_signature(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    module: &ModuleId,
    parsed: &crate::semantic_query_nucleus::ParsedSemanticSignature,
) -> Result<
    crate::semantic_query_nucleus::DeclarationSignatureProjection,
    ResolveSemanticSignatureError,
> {
    use crate::durable_semantics::{DurableParameterMode as M, DurableSemanticParameter};
    use crate::semantic_query_nucleus::{
        DeclarationSignatureProjection as Output, ParsedSemanticSignature as Input,
    };

    fn contains_slice(ty: &crate::durable_semantics::DurableType) -> bool {
        use crate::durable_semantics::DurableType as T;
        match ty {
            T::Slice { .. } => true,
            T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
                contains_slice(element)
            }
            _ => false,
        }
    }

    let diagnostic = |kind| {
        ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(kind),
        )
    };

    let resolve = |provider: &mut SemanticNucleusTypeProvider<'_>,
                   syntax: &rue_rir::RirTypeSyntaxArena<Arc<str>>,
                   root: rue_rir::RirTypeSyntaxRef,
                   kind: rue_air::DeclarationTypeDependencyKind| {
        provider.dependency_kind = kind;
        rue_air::resolve_structured_semantic_type_syntax(provider, module, syntax, root)
            .map_err(semantic_type_query_failure)
    };
    match parsed {
        Input::Callable {
            syntax,
            parameters,
            result,
            has_self,
            self_mode,
            is_unchecked,
            is_extern,
            is_c_export,
            is_accessor,
            accessor_result_mode,
            accessor_body,
            accessor_cycle,
            owner_placeholders,
            ..
        } => {
            if *is_accessor {
                // 6.6:3-6.6:7 over the exact canonical declaration. Which forms are
                // illegal, in which order, and how each diagnostic reads are
                // `rue_air::declaration_validation`'s, shared with the RIR
                // producers (RUE-1232); this seam owns only the lowering of
                // the parsed signature projection into that vocabulary.
                use rue_air::declaration_validation as rules;
                use rue_air::declaration_validation::{
                    AccessorParameterForm, AccessorReceiverForm,
                };
                let receiver = if provider.dependency_source.owner().is_none() {
                    AccessorReceiverForm::FreeFunction
                } else if !*has_self {
                    AccessorReceiverForm::AssociatedFunction
                } else {
                    match self_mode {
                        crate::declaration_candidate::DeclarationParameterMode::Borrow => {
                            AccessorReceiverForm::BorrowSelf
                        }
                        crate::declaration_candidate::DeclarationParameterMode::Inout => {
                            AccessorReceiverForm::InoutSelf
                        }
                        crate::declaration_candidate::DeclarationParameterMode::Value => {
                            AccessorReceiverForm::ValueSelf
                        }
                    }
                };
                if let Some(violation) = rules::accessor_signature_for_mode(
                    receiver,
                    *accessor_result_mode
                        == crate::declaration_candidate::DeclarationParameterMode::Inout,
                    parameters.iter().map(|parameter| {
                        if parameter.is_comptime {
                            return AccessorParameterForm::Comptime;
                        }
                        match parameter.mode {
                            crate::declaration_candidate::DeclarationParameterMode::Value => {
                                AccessorParameterForm::ByValue
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Borrow => {
                                AccessorParameterForm::Borrow
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Inout => {
                                AccessorParameterForm::Inout
                            }
                        }
                    }),
                ) {
                    use rue_air::declaration_validation::AccessorSignatureViolation as Violation;
                    return Err(match violation {
                        Violation::Receiver {
                            kind,
                            note: Some(note),
                        } => ResolveSemanticSignatureError::failure(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticWithNote {
                                kind,
                                note: Arc::from(note),
                            },
                        ),
                        Violation::Receiver { kind, note: None }
                        | Violation::Parameter { kind, .. } => diagnostic(kind),
                    });
                }
                // 6.6:6 and 6.6:7 over the accessor's own retained body. These
                // are legality rules on the declaration, so they hold with no
                // call site anywhere in the program (RUE-1212); see
                // `AccessorBodyVerdict` for the single link they leave to the
                // demanded path.
                if let Some(kind) = rules::accessor_body_error(accessor_body) {
                    return Err(diagnostic(kind));
                }
                // 6.6:14 over the owner's retained `self`-call edges: an
                // accessor cycle has no finite expansion, so it too is a
                // legality rule on the declaration (RUE-1282). Exotic edges
                // through a non-`self` receiver stay with the demanded-path
                // checks.
                if let Some(method) = accessor_cycle {
                    return Err(ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticWithNote {
                            kind: rules::accessor_recursion_error(method),
                            note: Arc::from(rules::ACCESSOR_RECURSION_NOTE),
                        },
                    ));
                }
            }
            // The names an interface requirement binds for its owner (`Self`
            // and the interface's type-valued associated constants, spec
            // 6.7:5) are generic placeholders ahead of the callable's own
            // comptime type parameters: the requirement's signature is then
            // deferred exactly like a generic signature, and conformance
            // verification substitutes the conforming type through the
            // retained syntax (spec 6.7:10).
            let mut generic_index = 0_u32;
            for placeholder in owner_placeholders.iter() {
                provider.substitutions.insert(
                    Arc::from(parsed.symbol(*placeholder)),
                    crate::durable_semantics::DurableType::GenericParameter(generic_index),
                );
                generic_index += 1;
            }
            // A comptime parameter is a type parameter when it is declared
            // `: type` or when it carries an interface bound (spec 6.7:14);
            // a bound is classified before the other parameters resolve so
            // `x: T` can already name it.
            let mut parameter_bounds = Vec::with_capacity(parameters.len());
            for (ordinal, parameter) in parameters.iter().enumerate() {
                let mut bounds: Arc<[StableDefinitionKey]> = Arc::from([]);
                if parameter.is_comptime {
                    let is_type = parsed.is_type_parameter_syntax(parameter.ty);
                    if !is_type {
                        bounds = classify_interface_bounds(
                            provider,
                            module,
                            syntax,
                            parameter,
                            ordinal as u32,
                        )?;
                    }
                    if is_type || !bounds.is_empty() {
                        provider.substitutions.insert(
                            Arc::from(parsed.symbol(parameter.name)),
                            crate::durable_semantics::DurableType::GenericParameter(generic_index),
                        );
                        generic_index += 1;
                    }
                }
                parameter_bounds.push(bounds);
            }
            let parameters = parameters
                .iter()
                .zip(parameter_bounds)
                .map(|(parameter, bounds)| {
                    let ty = if bounds.is_empty() {
                        resolve(
                            provider,
                            syntax,
                            parameter.ty,
                            rue_air::DeclarationTypeDependencyKind::Signature,
                        )?
                    } else {
                        // A bounded parameter is a type parameter (spec
                        // 6.7:16); only the call site reads its bound.
                        crate::durable_semantics::DurableType::ComptimeType
                    };
                    if parameter.is_comptime
                        && bounds.is_empty()
                        && !parsed.is_type_parameter_syntax(parameter.ty)
                    {
                        provider
                            .deferred_value_parameters
                            .insert(Arc::from(parsed.symbol(parameter.name)), ty.clone());
                    }
                    if !parameter.is_comptime {
                        reject_interface_type(provider, &ty, |kind, help| {
                            ResolveSemanticSignatureError::failure(
                                crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticWithHelp {
                                    kind,
                                    help: Arc::from(help),
                                },
                            )
                        })?;
                    }
                    Ok(DurableSemanticParameter {
                        name: Arc::from(parsed.symbol(parameter.name)),
                        ty,
                        mode: match parameter.mode {
                            crate::declaration_candidate::DeclarationParameterMode::Value => {
                                M::Value
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Borrow => {
                                M::Borrow
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Inout => {
                                M::Inout
                            }
                        },
                        is_comptime: parameter.is_comptime,
                        bounds,
                    })
                })
                .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?;
            let result = resolve(
                provider,
                syntax,
                *result,
                rue_air::DeclarationTypeDependencyKind::Signature,
            )?;
            if contains_slice(&result) {
                return Err(diagnostic(rue_error::ErrorKind::SliceReturnNotAllowed));
            }
            reject_interface_type(provider, &result, |kind, help| {
                ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticWithHelp {
                        kind,
                        help: Arc::from(help),
                    },
                )
            })?;
            if (*is_extern || *is_c_export)
                && !provider
                    .configuration
                    .preview_features
                    .contains(rue_error::PreviewFeature::CFfi)
            {
                return Err(diagnostic(rue_error::ErrorKind::PreviewFeatureRequired {
                    feature: rue_error::PreviewFeature::CFfi,
                    what: if *is_extern {
                        "an `extern \"C\"` foreign declaration".to_owned()
                    } else {
                        "a `pub extern \"C\" fn` export".to_owned()
                    },
                }));
            }
            if *is_extern || *is_c_export {
                let check =
                    |provider: &mut SemanticNucleusTypeProvider<'_>,
                     ty: &crate::durable_semantics::DurableType| {
                        use crate::durable_semantics::DurableType as T;
                        if matches!(ty, T::Array { .. }) {
                            return Err(diagnostic(rue_error::ErrorKind::ExternArrayByValue {
                                ty: durable_type_diagnostic_name(ty),
                            }));
                        }
                        if let T::Nominal(key) = ty
                            && key.kind() == crate::StableDefinitionKind::Struct
                        {
                            let failure = provider.ffi_shape_failure(ty, &mut Vec::new()).map_err(
                                |error| match error {
                                    rue_air::SemanticProviderError::Abort(abort) => {
                                        ResolveSemanticSignatureError::Abort(abort)
                                    }
                                    rue_air::SemanticProviderError::Failure(failure) => {
                                        ResolveSemanticSignatureError::failure(failure)
                                    }
                                },
                            )?;
                            if failure.as_ref().is_some_and(|(reason, _, _)| {
                                *reason == rue_air::FfiRejectReason::NonReprCAggregate
                            }) {
                                return Err(diagnostic(
                                    rue_error::ErrorKind::ExternAggregateNotReprC {
                                        ty: durable_type_diagnostic_name(ty),
                                    },
                                ));
                            }
                            if failure.is_some() {
                                return Err(diagnostic(
                                    rue_error::ErrorKind::ExternSignatureTypeUnsupported {
                                        ty: durable_type_diagnostic_name(ty),
                                    },
                                ));
                            }
                            return Ok(());
                        }
                        if !matches!(
                            ty,
                            T::I8
                                | T::I16
                                | T::I32
                                | T::I64
                                | T::U8
                                | T::U16
                                | T::U32
                                | T::U64
                                | T::Bool
                                | T::PtrConst(_)
                                | T::PtrMut(_)
                        ) {
                            return Err(diagnostic(
                                rue_error::ErrorKind::ExternSignatureTypeUnsupported {
                                    ty: durable_type_diagnostic_name(ty),
                                },
                            ));
                        }
                        Ok(())
                    };
                for parameter in &parameters {
                    check(provider, &parameter.ty)?;
                }
                if result != crate::durable_semantics::DurableType::Unit {
                    check(provider, &result)?;
                }
            }
            if *is_c_export {
                let name = provider.dependency_source.name().to_owned();
                let reject = |reason| {
                    diagnostic(rue_error::ErrorKind::ExportSignatureUnsupported {
                        name: name.clone(),
                        reason,
                    })
                };
                if name == "main" {
                    return Err(reject("an export named `main` collides with the program entry point; give it a different C name".to_owned()));
                }
                if parameters.iter().any(|parameter| parameter.is_comptime) {
                    return Err(reject("a generic function has no single C symbol; export a concrete (non-`comptime`) function".to_owned()));
                }
                if let Some((index, _)) = parameters
                    .iter()
                    .enumerate()
                    .find(|(_, parameter)| parameter.mode != M::Value)
                {
                    return Err(reject(format!(
                        "parameter {} uses a by-reference (`borrow`/`inout`) mode, which does not cross a C boundary; pass a raw pointer instead",
                        index + 1
                    )));
                }
                if let Some(parameter) = parameters.iter().find(|parameter| {
                    matches!(
                        parameter.ty,
                        crate::durable_semantics::DurableType::Nominal(_)
                            | crate::durable_semantics::DurableType::Array { .. }
                    )
                }) {
                    return Err(reject(format!(
                        "aggregate parameter `{}` is not supported by the P4 export thunk (register repacking across the export boundary is future work); pass a pointer instead",
                        durable_type_diagnostic_name(&parameter.ty)
                    )));
                }
                if matches!(
                    result,
                    crate::durable_semantics::DurableType::Nominal(_)
                        | crate::durable_semantics::DurableType::Array { .. }
                ) {
                    return Err(reject(format!(
                        "aggregate return `{}` is not supported by the P4 export thunk",
                        durable_type_diagnostic_name(&result)
                    )));
                }
                if parameters.len() > 6 {
                    return Err(reject(format!(
                        "{} scalar parameters exceed the 6-register argument budget the P4 export thunk supports; reduce the parameter count",
                        parameters.len()
                    )));
                }
            }
            Ok(Output::Callable {
                parameters: parameters.into(),
                result,
                has_self: *has_self,
                self_mode: match self_mode {
                    crate::declaration_candidate::DeclarationParameterMode::Value => M::Value,
                    crate::declaration_candidate::DeclarationParameterMode::Borrow => M::Borrow,
                    crate::declaration_candidate::DeclarationParameterMode::Inout => M::Inout,
                },
                is_accessor: *is_accessor,
                accessor_result_mode: match accessor_result_mode {
                    crate::declaration_candidate::DeclarationParameterMode::Value => M::Value,
                    crate::declaration_candidate::DeclarationParameterMode::Borrow => M::Borrow,
                    crate::declaration_candidate::DeclarationParameterMode::Inout => M::Inout,
                },
                is_unchecked: *is_unchecked,
                is_extern: *is_extern,
                is_c_export: *is_c_export,
            })
        }
        Input::Struct {
            syntax,
            fields,
            is_copy,
            is_linear,
            is_repr_c,
            conformance,
            ..
        } => {
            let conformance = resolve_conformance_facts(provider, module, syntax, conformance)?;
            if let Some(kind) = rue_air::declaration_validation::linear_copy_struct(
                provider.dependency_source.name(),
                *is_linear,
                *is_copy,
            ) {
                return Err(diagnostic(kind));
            }
            if let Some(kind) = rue_air::declaration_validation::duplicate_field(
                provider.dependency_source.name(),
                fields.iter().map(|field| parsed.symbol(field.name)),
            ) {
                return Err(diagnostic(kind));
            }
            let fields = fields
                .iter()
                .map(|field| {
                    let name: Arc<str> = Arc::from(parsed.symbol(field.name));
                    Ok((
                        name,
                        resolve(
                            provider,
                            syntax,
                            field.ty,
                            rue_air::DeclarationTypeDependencyKind::Field,
                        )?,
                    ))
                })
                .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?;
            if fields.iter().any(|(_, ty)| contains_slice(ty)) {
                return Err(diagnostic(rue_error::ErrorKind::SliceInAggregateField));
            }
            if fields
                .iter()
                .any(|(_, ty)| *ty == crate::durable_semantics::DurableType::ComptimeType)
            {
                return Err(ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                        "type values cannot exist at runtime",
                    )),
                ));
            }
            for (_, ty) in &fields {
                reject_interface_type(provider, ty, |kind, help| {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticWithHelp {
                            kind,
                            help: Arc::from(help),
                        },
                    )
                })?;
            }
            if *is_copy {
                for (field_name, field_ty) in &fields {
                    if !provider
                        .type_is_copy(field_ty)
                        .map_err(|error| match error {
                            rue_air::SemanticProviderError::Abort(abort) => {
                                ResolveSemanticSignatureError::Abort(abort)
                            }
                            rue_air::SemanticProviderError::Failure(failure) => {
                                ResolveSemanticSignatureError::failure(failure)
                            }
                        })?
                    {
                        return Err(diagnostic(rue_error::ErrorKind::CopyStructNonCopyField(
                            Box::new(rue_error::CopyStructNonCopyFieldError {
                                struct_name: provider.dependency_source.name().to_owned(),
                                field_name: field_name.to_string(),
                                field_type: durable_type_diagnostic_name(field_ty),
                            }),
                        )));
                    }
                }
            }
            if *is_repr_c {
                if !provider
                    .configuration
                    .preview_features
                    .contains(rue_error::PreviewFeature::CFfi)
                {
                    return Err(diagnostic(rue_error::ErrorKind::PreviewFeatureRequired {
                        feature: rue_error::PreviewFeature::CFfi,
                        what: "the `@repr(c)` representation marker".to_owned(),
                    }));
                }
                let has_destructor = provider
                    .candidate(
                        module,
                        provider.dependency_source.name(),
                        DefinitionKind::Destructor,
                    )
                    .map_err(|error| match error {
                        rue_air::SemanticProviderError::Abort(abort) => {
                            ResolveSemanticSignatureError::Abort(abort)
                        }
                        rue_air::SemanticProviderError::Failure(failure) => {
                            ResolveSemanticSignatureError::failure(failure)
                        }
                    })?
                    .is_some();
                if let Some((reason, path, failing)) = provider
                    .repr_c_failure_for_fields(&fields, *is_linear, has_destructor)
                    .map_err(|error| match error {
                        rue_air::SemanticProviderError::Abort(abort) => {
                            ResolveSemanticSignatureError::Abort(abort)
                        }
                        rue_air::SemanticProviderError::Failure(failure) => {
                            ResolveSemanticSignatureError::failure(failure)
                        }
                    })?
                {
                    let field_path = path.join(".");
                    let reason = if field_path.is_empty() {
                        reason.describe().to_owned()
                    } else {
                        format!(
                            "field `{field_path}` of type `{}` — {}",
                            durable_type_diagnostic_name(&failing),
                            reason.describe()
                        )
                    };
                    return Err(diagnostic(rue_error::ErrorKind::ReprCStructIneligible(
                        Box::new(rue_error::ReprCIneligibleError {
                            struct_name: provider.dependency_source.name().to_owned(),
                            field_path,
                            failing_type: durable_type_diagnostic_name(&failing),
                            reason,
                        }),
                    )));
                }
            }
            Ok(Output::Struct {
                fields: fields.into(),
                is_copy: *is_copy,
                is_linear: *is_linear,
                is_repr_c: *is_repr_c,
                conformance,
            })
        }
        Input::Enum {
            syntax,
            variants,
            payloads,
            is_non_exhaustive,
            is_public,
            non_exhaustive_range,
            ..
        } => {
            if *is_non_exhaustive && !*is_public {
                let kind = rue_error::ErrorKind::ParseError(
                    "@non_exhaustive can only be applied to public enums".to_string(),
                );
                return Err(match non_exhaustive_range {
                    Some((start, end)) => ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticAtProducerRange {
                            kind,
                            producer: declaration_candidate_for_stable_key(
                                &provider.dependency_source,
                            )
                            .expect("enum signature has a declaration candidate"),
                            start: *start,
                            end: *end,
                        },
                    ),
                    None => diagnostic(kind),
                });
            }
            if *is_non_exhaustive
                && !provider
                    .configuration
                    .preview_features
                    .contains(rue_error::PreviewFeature::NonExhaustiveEnums)
            {
                let kind = rue_error::ErrorKind::PreviewFeatureRequired {
                    feature: rue_error::PreviewFeature::NonExhaustiveEnums,
                    what: "@non_exhaustive enums".to_owned(),
                };
                return Err(match non_exhaustive_range {
                    Some((start, end)) => ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticAtProducerRange {
                            kind,
                            producer: declaration_candidate_for_stable_key(
                                &provider.dependency_source,
                            )
                            .expect("enum signature has a declaration candidate"),
                            start: *start,
                            end: *end,
                        },
                    ),
                    None => diagnostic(kind),
                });
            }
            if let Some(kind) = rue_air::declaration_validation::duplicate_variant(
                provider.dependency_source.name(),
                variants.iter().map(|variant| parsed.symbol(variant.name)),
            ) {
                return Err(diagnostic(kind));
            }
            let variants: Vec<(Arc<str>, Arc<[crate::durable_semantics::DurableType]>)> = variants
                .iter()
                .map(|variant| {
                    let payload = payloads
                        .get(variant.payload_start as usize..variant.payload_end as usize)
                        .expect("signature payload ranges are validated when projected");
                    Ok((
                        Arc::from(parsed.symbol(variant.name)),
                        payload
                            .iter()
                            .map(|root| {
                                resolve(
                                    provider,
                                    syntax,
                                    *root,
                                    rue_air::DeclarationTypeDependencyKind::Payload,
                                )
                            })
                            .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?
                            .into(),
                    ))
                })
                .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?;
            if variants
                .iter()
                .flat_map(|(_, payload)| payload.iter())
                .any(contains_slice)
            {
                return Err(diagnostic(rue_error::ErrorKind::SliceInAggregateField));
            }
            Ok(Output::Enum {
                variants: variants.into(),
                is_non_exhaustive: *is_non_exhaustive,
            })
        }
        Input::Destructor => Ok(Output::Destructor),
    }
}
#[derive(Clone)]
pub(in crate::revisioned_query_database) struct BodyInputResolver {
    pub(in crate::revisioned_query_database) stable_declaration_classifications: QueryFamily<
        StableDeclarationClassificationQueryKey,
        StableDeclarationClassificationQueryValue,
    >,
    pub(crate) declaration_shells:
        QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    pub(in crate::revisioned_query_database) declaration_body_plan_artifacts:
        QueryFamily<DeclarationBodyPlanQueryKey, DeclarationBodyPlanArtifactsValue>,
    pub(in crate::revisioned_query_database) body_source_bases:
        QueryFamily<crate::body_query::BodyQueryKey, Option<crate::body_query::BodySourceLocator>>,
}

impl BodyInputResolver {
    fn select(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
    ) -> Result<
        Result<
            (
                StableDefinitionKey,
                crate::declaration_candidate::DeclarationCandidateKey,
            ),
            crate::body_query::BodyInputIncomplete,
        >,
        QueryAbort,
    > {
        use crate::body_query::BodyInputIncomplete as Incomplete;

        let Some(definition) = body_source_definition_key(&key.instance).cloned() else {
            return Ok(Err(Incomplete::UnsupportedInstance));
        };
        let classification = match context.query_registered(
            &self.stable_declaration_classifications,
            StableDeclarationClassificationQueryKey(definition.clone()),
        ) {
            Ok(value) => value,
            Err(QueryAbort::MissingInput(_)) => {
                return Ok(Err(Incomplete::MissingPrerequisite(Arc::from(
                    "stable declaration classification",
                ))));
            }
            Err(abort) => return Err(abort),
        };
        let candidate = match classification.outcome() {
            rue_query::QueryOutcome::Success(
                StableDeclarationClassificationQueryValue::Selected(candidate),
            ) => candidate.clone(),
            _ => {
                return Ok(Err(Incomplete::MissingPrerequisite(Arc::from(
                    "stable declaration candidate",
                ))));
            }
        };
        Ok(Ok((definition, candidate)))
    }

    fn resolve_selected_artifact(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
        definition: StableDefinitionKey,
        candidate: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<crate::body_query::BodyInputValue, QueryAbort> {
        use crate::body_query::{BodyInputIncomplete as Incomplete, BodyInputValue};

        let artifacts = match context.query_registered(
            &self.declaration_body_plan_artifacts,
            DeclarationBodyPlanQueryKey(candidate),
        ) {
            Ok(value) => value,
            Err(QueryAbort::MissingInput(_)) => {
                return Ok(BodyInputValue::Incomplete(Incomplete::MissingPrerequisite(
                    Arc::from("declaration body plan"),
                )));
            }
            Err(abort) => return Err(abort),
        };
        let rue_query::QueryOutcome::Success(artifacts) = artifacts.outcome() else {
            unreachable!("DeclarationBodyPlanArtifacts publishes typed values")
        };
        let artifacts = match artifacts {
            DeclarationBodyPlanArtifactsValue::Available(artifacts) => artifacts,
            DeclarationBodyPlanArtifactsValue::Failure(failure) => {
                return Ok(BodyInputValue::Incomplete(Incomplete::BodyPlanFailure(
                    failure.clone(),
                )));
            }
        };
        let locator = context.query_registered(&self.body_source_bases, key.clone())?;
        let rue_query::QueryOutcome::Success(Some(locator)) = locator.outcome() else {
            return Ok(BodyInputValue::Incomplete(Incomplete::MissingPrerequisite(
                Arc::from("body source basis"),
            )));
        };
        Ok(BodyInputValue::Available(
            crate::body_query::OwnedBodyInput {
                owner: definition,
                source: locator.clone(),
                artifacts: artifacts.clone(),
            },
        ))
    }

    pub(in crate::revisioned_query_database) fn resolve_producer_artifact(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
    ) -> Result<crate::body_query::BodyInputValue, QueryAbort> {
        let (definition, candidate) = match self.select(context, key)? {
            Ok(selected) => selected,
            Err(incomplete) => {
                return Ok(crate::body_query::BodyInputValue::Incomplete(incomplete));
            }
        };
        self.resolve_selected_artifact(context, key, definition, candidate)
    }

    pub(in crate::revisioned_query_database) fn resolve(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
    ) -> Result<crate::body_query::BodyInputValue, QueryAbort> {
        use crate::body_query::{BodyInputIncomplete as Incomplete, BodyInputValue};

        let (definition, candidate) = match self.select(context, key)? {
            Ok(selected) => selected,
            Err(incomplete) => return Ok(BodyInputValue::Incomplete(incomplete)),
        };
        if !definition.kind().owns_body() {
            return Ok(BodyInputValue::Incomplete(Incomplete::UnsupportedKind(
                definition.kind(),
            )));
        }
        let shell = match context.query_registered(
            &self.declaration_shells,
            DeclarationShellQueryKey(candidate.clone()),
        ) {
            Ok(value) => value,
            Err(QueryAbort::MissingInput(_)) => {
                return Ok(BodyInputValue::Incomplete(Incomplete::MissingPrerequisite(
                    Arc::from("declaration shell"),
                )));
            }
            Err(abort) => return Err(abort),
        };
        let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(shell)) =
            shell.outcome()
        else {
            return Ok(BodyInputValue::Incomplete(Incomplete::MissingPrerequisite(
                Arc::from("declaration shell"),
            )));
        };
        if shell.is_extern
            || candidate.category
                == crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction
        {
            return Ok(BodyInputValue::Incomplete(Incomplete::Extern));
        }
        if shell.is_generic && matches!(key.instance, crate::FunctionInstanceKey::Definition(_)) {
            use crate::declaration_candidate::DeclarationCandidateCategory as Category;

            let named_runtime_value_body = matches!(
                candidate.category,
                Category::Method | Category::AssociatedFunction
            ) && shell
                .parameters
                .iter()
                .all(|parameter| !parameter.is_comptime || !parameter.is_type_parameter);
            if !named_runtime_value_body {
                return Ok(BodyInputValue::Incomplete(Incomplete::Generic));
            }
        }
        self.resolve_selected_artifact(context, key, definition, candidate)
    }
}
