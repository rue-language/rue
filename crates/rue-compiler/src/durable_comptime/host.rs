//! Thin AIR host adapter over compiler-owned services.
//!
//! AIR remains the only instruction evaluator. This module translates AIR
//! host operations into lifecycle, projection, semantic-service, and
//! diagnostic operations owned by their respective modules.

use super::diagnostics::*;
use super::lifecycle::*;
use super::projection::*;
use super::services::*;
use super::structured::*;
use super::*;
use rue_air::ComptimeValueAlgebra;

#[cfg(test)]
thread_local! {
    static ENUM_VARIANT_CHILD_TRIPWIRE: std::cell::RefCell<Option<CancellationToken>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_enum_variant_child_tripwire(token: Option<CancellationToken>) {
    ENUM_VARIANT_CHILD_TRIPWIRE.with(|tripwire| *tripwire.borrow_mut() = token);
}

#[cfg(test)]
fn arm_enum_variant_child_tripwire() {
    ENUM_VARIANT_CHILD_TRIPWIRE.with(|tripwire| {
        if let Some(token) = tripwire.borrow().as_ref() {
            token.cancel();
        }
    });
}

/// Production host composition boundary. The canonical AIR engine uses this
/// adapter for both declaration-time query roots and nested admitted frames;
/// the adapter holds only the named service facade.
#[allow(dead_code)] // consumed by the canonical durable AIR host
pub(crate) struct DurableComptimeHost<'a, A: DurableComptimeHostAuthority + ?Sized> {
    services: DurableComptimeServices<'a, A>,
}

impl<'a, A: DurableComptimeHostAuthority + ?Sized> DurableComptimeHost<'a, A> {
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn new(authority: &'a mut A) -> Self {
        Self {
            services: DurableComptimeServices::new(authority),
        }
    }

    /// Validate a durable structural value against its complete declared
    /// shape before any consumer projects or expands its children.  Aggregate
    /// values are bounded and Copy-only at this one authority boundary; all
    /// recursive callers use this same check.
    fn validate_durable_value(
        &self,
        value: &DurableConstValue,
        expected: &DurableType,
        depth: usize,
        nodes: &mut usize,
    ) -> rue_air::ComptimeHostResult<bool, DurableComptimeHostFailure> {
        if depth > rue_air::MAX_COMPTIME_VALUE_DEPTH {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                "structural comptime value exceeds resource limits",
            )));
        }
        *nodes = nodes.saturating_add(1);
        if *nodes > rue_air::MAX_COMPTIME_VALUE_NODES {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                "structural comptime value exceeds resource limits",
            )));
        }
        let DurableConstValue::Aggregate(aggregate) = value else {
            return Ok(durable_const_fits_type(value, expected));
        };
        // Run the shared shape walk before resolving field metadata or
        // allocating type vectors.  Imported values are untrusted query
        // payloads, so a later recursive consumer must never be the first
        // place that discovers an oversized tree.
        if !rue_air::semantic_import_const_value_within_limits(&DurableConstValue::Aggregate(
            aggregate.clone(),
        )) {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                "structural comptime value exceeds resource limits",
            )));
        }
        if aggregate.ty != *expected
            || !self
                .services
                .type_is_copy(expected)
                .map_err(durable_provider_error)?
        {
            return Ok(false);
        }
        let check_children = |children: &[DurableConstValue],
                              types: Vec<DurableType>,
                              this: &Self,
                              nodes: &mut usize| {
            if children.len() != types.len() {
                return Ok(false);
            }
            for (child, ty) in children.iter().zip(types.iter()) {
                if !this.validate_durable_value(child, ty, depth + 1, nodes)? {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        match (&aggregate.kind, expected) {
            (
                rue_air::SemanticImportAggregateKind::Array(values),
                DurableType::Array { element, len },
            ) => {
                if *len as usize != values.len() {
                    return Ok(false);
                }
                check_children(
                    values,
                    vec![element.as_ref().clone(); values.len()],
                    self,
                    nodes,
                )
            }
            (rue_air::SemanticImportAggregateKind::Struct(values), DurableType::Nominal(key))
                if key.kind() == crate::StableDefinitionKind::Struct =>
            {
                let count = self
                    .services
                    .resolve_struct_field_count(expected)
                    .map_err(durable_provider_error)?;
                if count != values.len() {
                    return Ok(false);
                }
                let mut types = Vec::with_capacity(count);
                for index in 0..count {
                    let Some(ty) = self
                        .services
                        .resolve_struct_field_type(expected, index as u32)
                        .map_err(durable_provider_error)?
                    else {
                        return Ok(false);
                    };
                    types.push(ty);
                }
                check_children(values, types, self, nodes)
            }
            (
                rue_air::SemanticImportAggregateKind::Enum { variant, payload },
                DurableType::Nominal(key),
            ) if key.kind() == crate::StableDefinitionKind::Enum => {
                let types = self
                    .services
                    .resolve_enum_variant_payload_types(expected, *variant)
                    .map_err(durable_provider_error)?;
                check_children(payload, types.to_vec(), self, nodes)
            }
            (
                rue_air::SemanticImportAggregateKind::Struct(values),
                DurableType::BuiltinNominal {
                    kind: rue_air::SemanticImportNominalKind::Struct,
                    ..
                },
            ) => {
                let count = self
                    .services
                    .resolve_struct_field_count(expected)
                    .map_err(durable_provider_error)?;
                if count != values.len() {
                    return Ok(false);
                }
                let mut types = Vec::with_capacity(count);
                for index in 0..count {
                    let Some(ty) = self
                        .services
                        .resolve_struct_field_type(expected, index as u32)
                        .map_err(durable_provider_error)?
                    else {
                        return Ok(false);
                    };
                    types.push(ty);
                }
                check_children(values, types, self, nodes)
            }
            (
                rue_air::SemanticImportAggregateKind::Enum { variant, payload },
                DurableType::BuiltinNominal {
                    kind: rue_air::SemanticImportNominalKind::Enum,
                    ..
                },
            ) => {
                let types = self
                    .services
                    .resolve_enum_variant_payload_types(expected, *variant)
                    .map_err(durable_provider_error)?;
                check_children(payload, types.to_vec(), self, nodes)
            }
            _ => Ok(false),
        }
    }

    /// Preserve the declared type carried by a reduced value while allowing
    /// the two source literals whose type is selected by context.  The raw
    /// durable representation intentionally erases this information, so
    /// checking the wrapper must happen before `into_durable_value`.
    fn durable_child_type_mismatch(
        value: &EvaluatedSemanticConst,
        expected: &DurableType,
    ) -> Option<DurableType> {
        let EvaluatedSemanticConst::Value(value) = value else {
            return None;
        };
        match value.ty.as_ref() {
            None => None,
            Some(actual) if actual == expected => None,
            Some(DurableType::ComptimeFloat)
                if matches!(expected, DurableType::F32 | DurableType::F64) =>
            {
                None
            }
            Some(actual) => Some(actual.clone()),
        }
    }

    fn durable_child_type_error(
        found: DurableType,
        expected: &DurableType,
    ) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
        durable_host_error(DurableComptimeFailure::failure(
            SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::TypeMismatch {
                expected: durable_type_diagnostic_name(expected),
                found: durable_type_diagnostic_name(&found),
            }),
        ))
    }

    #[allow(dead_code)]
    fn program_rir(
        &self,
        program: &crate::body_query::DurableComptimeProgramKey,
    ) -> &rue_rir::ValidatedRir {
        &self
            .services
            .durable_session()
            .registered_program(program)
            .expect("durable AIR frame must reference a registered program")
            .rir
    }

    #[allow(dead_code)]
    fn name_from_symbol(
        &self,
        program: &crate::body_query::DurableComptimeProgramKey,
        symbol: rue_rir::SymbolHandle,
    ) -> DurableComptimeName {
        let registered = self
            .services
            .durable_session()
            .registered_program(program)
            .expect("durable AIR frame must reference a registered program");
        DurableComptimeName::from(
            registered
                .symbols
                .get(symbol.issuing_interner_ordinal())
                .expect("validated symbol handle")
                .clone(),
        )
    }

    #[allow(dead_code)]
    fn file_for_program_span(
        &self,
        program: &crate::body_query::DurableComptimeProgramKey,
        span: &rue_span::Span,
    ) -> DurableComptimeFile {
        self.services
            .durable_session()
            .file_for_program(program)
            .unwrap_or_else(|_| panic!("unregistered durable program at {span:?}"))
    }

    fn diagnostic_site(
        &self,
        site: &rue_air::ComptimeDiagnosticSite<crate::body_query::DurableComptimeProgramKey>,
    ) -> DurableComptimeDiagnosticSite {
        self.services
            .durable_session()
            .diagnostic_site(site.program(), site.span())
            .expect("durable AIR diagnostic must reference a registered declaration program")
    }

    fn admit_call_for_module(
        &mut self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &DurableComptimeName,
        argument_modes: &[rue_air::ComptimeArgMode],
    ) -> rue_air::ComptimeHostResult<DurableComptimeAdmittedCall, DurableComptimeHostFailure> {
        let reservation = self
            .services
            .durable_session_mut()
            .reserve_bound_expression_call();
        let start = self
            .services
            .begin_comptime_call_admission(accessing_source, module, name.as_str())
            .map_err(durable_provider_error)?;
        self.services
            .durable_session_mut()
            .observe_dependency(start.dependency.clone());
        if let Some(alias) = start.alias_dependency.clone() {
            self.services
                .durable_session_mut()
                .observe_dependency(alias);
        }
        let modes = argument_modes
            .iter()
            .map(|(mode, _)| match mode {
                rue_rir::RirArgMode::Normal => {
                    crate::durable_semantics::DurableParameterMode::Value
                }
                rue_rir::RirArgMode::Borrow => {
                    crate::durable_semantics::DurableParameterMode::Borrow
                }
                rue_rir::RirArgMode::Inout => crate::durable_semantics::DurableParameterMode::Inout,
            })
            .collect::<Vec<_>>();
        let admission = self
            .services
            .finish_comptime_call_admission(start, &modes)
            .map_err(durable_provider_error)?;
        self.services
            .durable_session_mut()
            .admit_bound_expression_call(reservation, admission)
            .map_err(|error| {
                durable_host_error(DurableComptimeFailure::resolution(format!(
                    "durable call lifecycle: {error:?}"
                )))
            })
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeDomain
    for DurableComptimeHost<'_, A>
{
    type Type = DurableComptimeType;
    type Value = EvaluatedSemanticConst;
    type Name = DurableComptimeName;
    type File = DurableComptimeFile;
    type CanonicalIdentity = DurableComptimeIdentity;
    type AnonymousIdentity = DurableComptimeAnonymousIdentity;
    type ProgramKey = crate::body_query::DurableComptimeProgramKey;
    type Failure = DurableComptimeHostFailure;
    type CallAdmission = DurableComptimeAdmittedCall;
    type CallBinding = DurableComptimeBinding;
    type BoundCall = DurableComptimeBoundCall;
    type CompletionTicket = Box<DurableComptimeCallTicket>;
    type StructuredTypeSuspension = DurableStructuredTypeJob;
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeInterrupts
    for DurableComptimeHost<'_, A>
{
    fn check_canceled(&self) -> rue_air::ComptimeHostResult<(), Self::Failure> {
        self.services.check_canceled().map_err(|abort| {
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure::query_abort(abort))
        })
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeProgramFacts
    for DurableComptimeHost<'_, A>
{
    fn program_rir(&self, program: &Self::ProgramKey) -> &rue_rir::Rir {
        self.program_rir(program)
    }

    fn name_from_symbol(
        &self,
        program: &Self::ProgramKey,
        symbol: rue_rir::SymbolHandle,
    ) -> Self::Name {
        self.name_from_symbol(program, symbol)
    }

    fn display_name(&self, name: &Self::Name) -> String {
        name.as_str().to_owned()
    }

    fn file_for_program_span(
        &self,
        program: &Self::ProgramKey,
        span: &rue_span::Span,
    ) -> Self::File {
        self.file_for_program_span(program, span)
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeTypeAlgebra
    for DurableComptimeHost<'_, A>
{
    fn unsupported_anon_method_type_param(
        &self,
        method_name: &str,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        durable_host_failure(DurableComptimeFailure::comptime_failure_at(
            &self.diagnostic_site(site),
            format!(
                "method '{method_name}' declares its own `comptime` type parameter, which is not yet supported (a method cannot be monomorphized over its own type parameter); move the type parameter to the enclosing type constructor instead"
            ),
        ))
    }

    fn non_function_anon_method(
        &self,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        durable_host_failure(DurableComptimeFailure::resolution(
            "anonymous type carries a non-function method instruction",
        ))
    }

    fn resolve_named_array_length(
        &mut self,
        name: &Self::Name,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
        _values: Option<&AHashMap<Self::Name, Self::Value>>,
        binding: rue_air::ComptimeArrayLengthBinding<Self::Value>,
    ) -> rue_air::ComptimeOutcome<u64, Self::Failure> {
        let decision = classify_durable_named_array_length(
            name.as_str(),
            durable_array_length_binding_from_air(binding),
        );
        let decision = match decision {
            Ok(decision) => decision,
            Err(error) => {
                return rue_air::ComptimeOutcome::HostFailure(durable_diagnostic_failure(
                    &self.diagnostic_site(site),
                    match durable_named_array_length_failure(name.as_str(), error) {
                        SemanticNucleusFailure::Diagnostic(kind) => kind,
                        failure => {
                            return rue_air::ComptimeOutcome::HostFailure(
                                DurableComptimeHostFailure::semantic(Box::new(failure)),
                            );
                        }
                    },
                ));
            }
        };
        match decision {
            DurableComptimeArrayLengthDecision::Concrete(value) => {
                rue_air::ComptimeOutcome::Known(value)
            }
            DurableComptimeArrayLengthDecision::RuntimeDependent => {
                rue_air::ComptimeOutcome::RuntimeDependent
            }
            DurableComptimeArrayLengthDecision::Shadowed => {
                let failure = durable_named_array_length_failure(
                    name.as_str(),
                    DurableComptimeArrayLengthError::NonInteger,
                );
                rue_air::ComptimeOutcome::HostFailure(DurableComptimeHostFailure::semantic(
                    Box::new(failure),
                ))
            }
            DurableComptimeArrayLengthDecision::ResolveGlobal => {
                let program = site.program();
                let projection = match self.services.resolve_named_value(
                    &program.declaration,
                    program.declaration.module(),
                    name.as_str(),
                ) {
                    Ok(Some(projection)) => projection,
                    Ok(None) => {
                        return rue_air::ComptimeOutcome::HostFailure(
                            DurableComptimeHostFailure::semantic(Box::new(
                                SemanticNucleusFailure::Resolution(Arc::from(format!(
                                    "undefined constant `{}`",
                                    name.as_str()
                                ))),
                            )),
                        );
                    }
                    Err(error) => {
                        return match durable_provider_error(error) {
                            rue_air::ComptimeHostError::HostFailure(error) => {
                                rue_air::ComptimeOutcome::HostFailure(error)
                            }
                            rue_air::ComptimeHostError::Abort(error) => {
                                rue_air::ComptimeOutcome::Abort(error)
                            }
                        };
                    }
                };
                let (value, dependency, _anonymous_nominals) = projection.into_parts();
                self.services
                    .durable_session_mut()
                    .observe_dependency(dependency);
                let value = self
                    .services
                    .test_array_length_override()
                    .map_or(value, EvaluatedSemanticConst::integer);
                match durable_named_array_length_value(&value) {
                    Ok(value) => rue_air::ComptimeOutcome::Known(value),
                    Err(error) => {
                        rue_air::ComptimeOutcome::HostFailure(DurableComptimeHostFailure::semantic(
                            Box::new(durable_named_array_length_failure(name.as_str(), error)),
                        ))
                    }
                }
            }
        }
    }

    fn rir_type_named_symbol(
        &self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Option<Self::Name> {
        let registered = self
            .services
            .durable_session()
            .registered_program(program)?;
        let rue_rir::RirTypeSyntaxNode::Named(symbol) =
            registered.rir.type_syntax().node(syntax)?
        else {
            return None;
        };
        let symbol = registered.rir.type_syntax().symbol(*symbol)?;
        registered
            .symbols
            .get(symbol.into_usize())
            .cloned()
            .map(DurableComptimeName::from)
    }

    fn render_rir_type(
        &self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> String {
        let registered = self
            .services
            .durable_session()
            .registered_program(program)
            .expect("durable AIR type syntax must reference a registered program");
        registered
            .rir
            .type_syntax()
            .render_type_with(syntax, |symbol| {
                registered.symbols[symbol.into_usize()].as_ref()
            })
            .expect("validated durable type syntax")
    }

    fn get_or_create_array_type(&mut self, element: Self::Type, length: u64) -> Self::Type {
        DurableComptimeType(DurableType::Array {
            element: Arc::new(element.0),
            len: length,
        })
    }

    fn find_or_create_anon_struct(
        &mut self,
        identity: Self::AnonymousIdentity,
        fields: &[rue_air::ComptimeField<Self::Name, Self::Type>],
        sigs: &[rue_air::ComptimeMethodDescriptor<Self::Name, Self::Type>],
        type_subst: &AHashMap<Self::Name, Self::Type>,
        value_subst: &AHashMap<Self::Name, Self::Value>,
    ) -> rue_air::ComptimeHostResult<(Self::Type, bool), Self::Failure> {
        let fields = fields
            .iter()
            .map(|field| rue_air::ComptimeField {
                name: field.name.0.clone(),
                ty: field.ty.0.clone(),
            })
            .collect::<Vec<_>>();
        let methods = sigs
            .iter()
            .map(|method| rue_air::ComptimeMethodDescriptor {
                name: method.name.0.clone(),
                has_self: method.has_self,
                self_mode: method.self_mode,
                returns_borrow: method.returns_borrow,
                returns_inout: method.returns_inout,
                parameters: method
                    .parameters
                    .iter()
                    .map(|parameter| rue_air::ComptimeMethodParameter {
                        ty: match &parameter.ty {
                            rue_air::ComptimeMethodType::SelfType => {
                                rue_air::ComptimeMethodType::SelfType
                            }
                            rue_air::ComptimeMethodType::Concrete(ty) => {
                                rue_air::ComptimeMethodType::Concrete(ty.0.clone())
                            }
                            rue_air::ComptimeMethodType::Unsupported(shape) => {
                                rue_air::ComptimeMethodType::Unsupported(shape.clone())
                            }
                        },
                        mode: parameter.mode,
                        is_comptime: parameter.is_comptime,
                        is_comptime_type: parameter.is_comptime_type,
                    })
                    .collect(),
                parameter_names: method
                    .parameter_names
                    .iter()
                    .map(|name| name.0.clone())
                    .collect(),
                result: match &method.result {
                    rue_air::ComptimeMethodType::SelfType => rue_air::ComptimeMethodType::SelfType,
                    rue_air::ComptimeMethodType::Concrete(ty) => {
                        rue_air::ComptimeMethodType::Concrete(ty.0.clone())
                    }
                    rue_air::ComptimeMethodType::Unsupported(shape) => {
                        rue_air::ComptimeMethodType::Unsupported(shape.clone())
                    }
                },
                declaration_span: method.declaration_span,
            })
            .collect::<Vec<_>>();
        let type_captures = type_subst
            .iter()
            .map(|(name, ty)| (name.0.clone(), ty.0.clone()))
            .collect::<Vec<_>>();
        let mut value_captures = Vec::with_capacity(value_subst.len());
        for (name, value) in value_subst {
            let EvaluatedSemanticConst::Value(value) = value else {
                // Module and target locals are lexical context, not captured
                // durable value parameters. Non-const values are excluded
                // these non-const values from anonymous nominal identity.
                continue;
            };
            value_captures.push((name.0.clone(), value.value.clone()));
        }
        let ty = project_durable_anonymous_nominal(
            self.services.durable_session_mut(),
            DurableAnonymousNominalDescriptor {
                identity: identity.key().clone(),
                shape: DurableAnonymousNominalDescriptorShape::Struct {
                    fields: fields.into(),
                    methods: methods.into(),
                },
                type_captures: type_captures.into(),
                value_captures: value_captures.into(),
            },
        )
        .map_err(durable_host_error)?;
        Ok((ty.into(), true))
    }

    fn find_or_create_anon_enum(
        &mut self,
        identity: Self::AnonymousIdentity,
        names: &[String],
        payloads: &[Vec<Self::Type>],
        type_subst: &AHashMap<Self::Name, Self::Type>,
        value_subst: &AHashMap<Self::Name, Self::Value>,
    ) -> rue_air::ComptimeHostResult<Self::Type, Self::Failure> {
        let variants = names
            .iter()
            .zip(payloads)
            .map(|(name, payload)| {
                (
                    Arc::from(name.as_str()),
                    payload
                        .iter()
                        .map(|ty| ty.0.clone())
                        .collect::<Vec<_>>()
                        .into(),
                )
            })
            .collect::<Vec<_>>();
        let type_captures = type_subst
            .iter()
            .map(|(name, ty)| (name.0.clone(), ty.0.clone()))
            .collect::<Vec<_>>();
        let mut value_captures = Vec::with_capacity(value_subst.len());
        for (name, value) in value_subst {
            let EvaluatedSemanticConst::Value(value) = value else {
                // Keep module/target locals out of nominal captures just as
                // the durable evaluator does; they are lexical context, not
                // value-parameter identity.
                continue;
            };
            value_captures.push((name.0.clone(), value.value.clone()));
        }
        project_durable_anonymous_nominal(
            self.services.durable_session_mut(),
            DurableAnonymousNominalDescriptor {
                identity: identity.key().clone(),
                shape: DurableAnonymousNominalDescriptorShape::Enum {
                    variants: variants.into(),
                },
                type_captures: type_captures.into(),
                value_captures: value_captures.into(),
            },
        )
        .map(DurableComptimeType)
        .map_err(durable_host_error)
    }

    fn check_require_droppable(
        &mut self,
        ty: Self::Type,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<(), Self::Failure> {
        let (declaration, start, end) = self.diagnostic_site(site).into_parts();
        self.services
            .durable_session_mut()
            .observe_deferred_ownership(DeferredOwnershipGate {
                kind: crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireDroppable,
                ty: ty.0,
                source: Arc::new(crate::semantic_query_nucleus::DeferredOwnershipGateSource {
                    declaration,
                    start,
                    end,
                }),
                application: None,
            });
        Ok(())
    }

    fn check_trivially_droppable(
        &mut self,
        ty: Self::Type,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<(), Self::Failure> {
        let (declaration, start, end) = self.diagnostic_site(site).into_parts();
        self.services
            .durable_session_mut()
            .observe_deferred_ownership(DeferredOwnershipGate {
            kind:
                crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireTriviallyDroppable,
            ty: ty.0,
            source: Arc::new(crate::semantic_query_nucleus::DeferredOwnershipGateSource {
                declaration,
                start,
                end,
            }),
            application: None,
        });
        Ok(())
    }

    fn type_name(&self, ty: &Self::Type) -> String {
        DurableComptimeScalarPolicy::type_name(ty.as_ref())
    }
    fn type_is_enum(&self, ty: &Self::Type) -> bool {
        matches!(ty.as_ref(), DurableType::Nominal(key) if key.kind() == crate::StableDefinitionKind::Enum)
    }

    fn type_is_unsigned(&self, ty: &Self::Type) -> bool {
        DurableComptimeScalarPolicy::type_is_unsigned(ty.as_ref())
    }

    fn type_integer_semantics(
        &self,
        ty: &Self::Type,
    ) -> Option<rue_air::integer_semantics::IntegerType> {
        DurableComptimeScalarPolicy::type_integer_semantics(ty.as_ref())
    }

    fn type_float_width(&self, ty: &Self::Type) -> Option<rue_air::ComptimeFloatWidth> {
        match ty.as_ref() {
            DurableType::F32 => Some(rue_air::ComptimeFloatWidth::F32),
            DurableType::F64 => Some(rue_air::ComptimeFloatWidth::F64),
            _ => None,
        }
    }

    fn float_type(&self, width: rue_air::ComptimeFloatWidth) -> Option<Self::Type> {
        Some(DurableComptimeType(match width {
            rue_air::ComptimeFloatWidth::F32 => DurableType::F32,
            rue_air::ComptimeFloatWidth::F64 => DurableType::F64,
        }))
    }

    fn resolve_comptime_type_intrinsic(
        &mut self,
        intrinsic: rue_air::ComptimeTypeIntrinsic,
        ty: Self::Type,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        match intrinsic {
            rue_air::ComptimeTypeIntrinsic::RequireDroppable => {
                self.check_require_droppable(ty, site)?;
                Ok(Some(EvaluatedSemanticConst::unit()))
            }
            rue_air::ComptimeTypeIntrinsic::RequireTriviallyDroppable => {
                self.check_trivially_droppable(ty, site)?;
                Ok(Some(EvaluatedSemanticConst::unit()))
            }
            rue_air::ComptimeTypeIntrinsic::IntegerBound(bound) => {
                let value = DurableComptimeTypeIntrinsicPolicy::integer_bound(bound, ty.as_ref())
                    .map_err(durable_host_error)?;
                Ok(Some(EvaluatedSemanticConst::integer_typed(value, Some(ty))))
            }
        }
    }

    fn const_expr_type(
        &self,
        _program: &Self::ProgramKey,
        _env: &rue_air::ComptimeEnv<
            '_,
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::CanonicalIdentity,
        >,
        _inst_ref: rue_rir::InstRef,
    ) -> Option<Self::Type> {
        None
    }

    fn integer_operation_type(
        &self,
        resolved_type: Option<&Self::Type>,
        lhs: &Self::Value,
        rhs: &Self::Value,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        let lhs_type = lhs.as_integer_type();
        let rhs_type = rhs.as_integer_type();
        let ty = DurableComptimeScalarPolicy::integer_operation_type(
            resolved_type.map(AsRef::as_ref),
            lhs_type.as_ref().map(AsRef::as_ref),
            rhs_type.as_ref().map(AsRef::as_ref),
        )
        .map_err(durable_host_error)?;
        if let Some(value) = lhs.as_integer() {
            DurableComptimeScalarPolicy::require_integer_fits(&ty, value)
                .map_err(durable_host_error)?;
        }
        if let Some(value) = rhs.as_integer() {
            DurableComptimeScalarPolicy::require_integer_fits(&ty, value)
                .map_err(durable_host_error)?;
        }
        Ok(Some(DurableComptimeType(ty)))
    }

    fn unary_integer_type(
        &self,
        resolved_type: Option<&Self::Type>,
        operand: &Self::Value,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        let operand = operand.as_integer_type();
        DurableComptimeScalarPolicy::unary_integer_type(
            resolved_type.map(AsRef::as_ref),
            operand.as_ref().map(AsRef::as_ref),
        )
        .map(|ty| Some(DurableComptimeType(ty)))
        .map_err(durable_host_error)
    }

    fn resolve_named_type_value(
        &mut self,
        program: &Self::ProgramKey,
        name: Self::Name,
        span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        let builtin = match name.as_str() {
            "i8" => Some(DurableType::I8),
            "i16" => Some(DurableType::I16),
            "i32" => Some(DurableType::I32),
            "i64" => Some(DurableType::I64),
            "u8" => Some(DurableType::U8),
            "u16" => Some(DurableType::U16),
            "u32" => Some(DurableType::U32),
            "u64" => Some(DurableType::U64),
            "bool" => Some(DurableType::Bool),
            "unit" => Some(DurableType::Unit),
            "never" => Some(DurableType::Never),
            "type" => Some(DurableType::ComptimeType),
            "f32" => Some(DurableType::F32),
            "f64" => Some(DurableType::F64),
            "comptime_float" => Some(DurableType::ComptimeFloat),
            "str" => Some(DurableType::BuiltinNominal {
                name: Arc::from("str"),
                kind: rue_air::SemanticImportNominalKind::Struct,
            }),
            _ => None,
        };
        if let Some(ty) = builtin {
            return Ok(Some(DurableComptimeType(ty)));
        }
        let file = self.file_for_program_span(program, &span);
        let resolution = self.resolve_comptime_named_value(file, name, span)?;
        let rue_air::ComptimeNamedValueResolution::Known(value) = resolution else {
            return Ok(None);
        };
        Ok(value.as_type())
    }

    fn resolve_comptime_type_path(
        &mut self,
        _file: Self::File,
        segments: &[Self::Name],
        _span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        // The only qualified enum values supported by declaration-time
        // evaluation are the target descriptors. Their module/type spelling
        // is already semantic data from AIR; no RIR inspection or ambient
        // module inference is needed here.
        if segments.len() != 2 || !matches!(segments[0].as_str(), "Arch" | "Os" | "DataModel") {
            return Ok(None);
        }
        match self
            .services
            .resolve_target_enum_variant(segments[0].as_str(), segments[1].as_str())
        {
            Ok(value) => Ok(Some(EvaluatedSemanticConst::TargetEnum(value))),
            Err(error) => Err(durable_provider_error(error)),
        }
    }

    fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
        _span: rue_span::Span,
    ) -> Option<Self::Type> {
        let mut type_substitutions = types
            .iter()
            .map(|(name, ty)| (name.0.clone(), ty.0.clone()))
            .collect::<Vec<_>>();
        type_substitutions.sort_by(|left, right| left.0.cmp(&right.0));
        let mut value_substitutions = Vec::with_capacity(values.len());
        for (name, value) in values {
            let EvaluatedSemanticConst::Value(value) = value else {
                return None;
            };
            value_substitutions.push((name.0.clone(), value.value.clone()));
        }
        value_substitutions.sort_by(|left, right| left.0.cmp(&right.0));
        self.services
            .resolve_type_syntax_with_substitutions(
                program,
                syntax,
                &type_substitutions,
                &value_substitutions,
            )
            .ok()
            .map(DurableComptimeType)
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeValueAlgebra
    for DurableComptimeHost<'_, A>
{
    fn project_comptime_enum_payload(
        &mut self,
        value: &Self::Value,
        variant: u32,
        payload: Vec<Self::Value>,
    ) -> rue_air::ComptimeHostResult<Vec<Self::Value>, Self::Failure> {
        let EvaluatedSemanticConst::Value(value) = value else {
            return Ok(payload);
        };
        let DurableConstValue::Aggregate(aggregate) = &value.value else {
            return Ok(payload);
        };
        let types = self
            .services
            .resolve_enum_variant_payload_types(&aggregate.ty, variant)
            .map_err(durable_provider_error)?;
        if payload.len() != types.len() {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                "enum payload arity does not match its declaration",
            )));
        }
        let mut projected = Vec::with_capacity(payload.len());
        for (value, ty) in payload.into_iter().zip(types.iter()) {
            let Some(value) = into_durable_value(value) else {
                return Err(durable_host_error(DurableComptimeFailure::resolution(
                    "enum payload contains a non-value projection",
                )));
            };
            projected.push(evaluated_from_durable_value_with_type(value, ty.clone()));
        }
        Ok(projected)
    }

    fn resolve_comptime_struct(
        &mut self,
        ty: Self::Type,
        fields: Vec<(Self::Name, Self::Value)>,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        let field_count = match self.services.resolve_struct_field_count(ty.as_ref()) {
            Ok(field_count) => field_count,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        let is_copy = match self.services.type_is_copy(ty.as_ref()) {
            Ok(is_copy) => is_copy,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        if !is_copy {
            return durable_host_error_outcome(durable_host_error(
                DurableComptimeFailure::comptime_failure(
                    "structural comptime values require a Copy type",
                ),
            ));
        }
        let mut ordered = Vec::with_capacity(fields.len());
        for (name, value) in fields {
            let index = match self
                .services
                .resolve_struct_field_index(ty.as_ref(), name.as_str())
            {
                Ok(Some(index)) => index,
                Ok(None) => return rue_air::ComptimeOutcome::RuntimeDependent,
                Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
            };
            if ordered
                .iter()
                .any(|(seen, _): &(u32, Self::Value)| *seen == index)
            {
                return rue_air::ComptimeOutcome::RuntimeDependent;
            }
            let field_type = match self.services.resolve_struct_field_type(ty.as_ref(), index) {
                Ok(Some(field_type)) => field_type,
                Ok(None) => return rue_air::ComptimeOutcome::RuntimeDependent,
                Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
            };
            if let Some(found) = Self::durable_child_type_mismatch(&value, &field_type) {
                return durable_host_error_outcome(Self::durable_child_type_error(
                    found,
                    &field_type,
                ));
            }
            let Some(durable_value) = into_durable_value(value.clone()) else {
                return rue_air::ComptimeOutcome::RuntimeDependent;
            };
            let mut nodes = 0;
            match self.validate_durable_value(&durable_value, &field_type, 0, &mut nodes) {
                Ok(true) => {}
                Ok(false) => return rue_air::ComptimeOutcome::RuntimeDependent,
                Err(error) => return durable_host_error_outcome(error),
            }
            ordered.push((index, value));
        }
        if ordered.len() != field_count {
            return rue_air::ComptimeOutcome::RuntimeDependent;
        }
        ordered.sort_by_key(|(index, _)| *index);
        EvaluatedSemanticConst::aggregate_struct(
            ty,
            ordered.into_iter().map(|(_, value)| value).collect(),
        )
        .map_or_else(
            || {
                durable_host_error_outcome(durable_host_error(DurableComptimeFailure::resolution(
                    "structural comptime value exceeds resource limits",
                )))
            },
            rue_air::ComptimeOutcome::Known,
        )
    }

    fn resolve_comptime_array(
        &mut self,
        ty: Self::Type,
        elements: Vec<Self::Value>,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        let DurableType::Array { element, len } = ty.as_ref() else {
            return rue_air::ComptimeOutcome::RuntimeDependent;
        };
        match self.services.type_is_copy(ty.as_ref()) {
            Ok(true) => {}
            Ok(false) => {
                return durable_host_error_outcome(durable_host_error(
                    DurableComptimeFailure::comptime_failure(
                        "structural comptime values require a Copy type",
                    ),
                ));
            }
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        }
        if elements.len() as u64 != *len {
            return rue_air::ComptimeOutcome::RuntimeDependent;
        }
        for value in &elements {
            if let Some(found) = Self::durable_child_type_mismatch(value, element.as_ref()) {
                return durable_host_error_outcome(Self::durable_child_type_error(
                    found,
                    element.as_ref(),
                ));
            }
            let Some(value) = into_durable_value(value.clone()) else {
                return rue_air::ComptimeOutcome::RuntimeDependent;
            };
            let mut nodes = 0;
            match self.validate_durable_value(&value, element.as_ref(), 0, &mut nodes) {
                Ok(true) => {}
                Ok(false) => return rue_air::ComptimeOutcome::RuntimeDependent,
                Err(error) => return durable_host_error_outcome(error),
            }
        }
        EvaluatedSemanticConst::aggregate_array(ty, elements).map_or_else(
            || {
                durable_host_error_outcome(durable_host_error(DurableComptimeFailure::resolution(
                    "structural comptime value exceeds resource limits",
                )))
            },
            rue_air::ComptimeOutcome::Known,
        )
    }

    fn resolve_comptime_array_repeat(
        &mut self,
        ty: Self::Type,
        element: Self::Value,
        count: u64,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        let Some(durable_element) = into_durable_value(element.clone()) else {
            return rue_air::ComptimeOutcome::RuntimeDependent;
        };
        let Some((element_nodes, element_depth)) = durable_value_shape(&durable_element, 1) else {
            return durable_host_error_outcome(durable_host_error(
                DurableComptimeFailure::resolution(
                    "structural comptime value exceeds resource limits",
                ),
            ));
        };
        let Some(repeat_count) = usize::try_from(count).ok() else {
            return durable_host_error_outcome(durable_host_error(
                DurableComptimeFailure::resolution(
                    "structural comptime value exceeds resource limits",
                ),
            ));
        };
        let Some(total_nodes) = 1usize.checked_add(element_nodes.saturating_mul(repeat_count))
        else {
            return durable_host_error_outcome(durable_host_error(
                DurableComptimeFailure::resolution(
                    "structural comptime value exceeds resource limits",
                ),
            ));
        };
        if total_nodes > rue_air::MAX_COMPTIME_VALUE_NODES
            || element_depth > rue_air::MAX_COMPTIME_VALUE_DEPTH
        {
            return durable_host_error_outcome(durable_host_error(
                DurableComptimeFailure::resolution(
                    "structural comptime value exceeds resource limits",
                ),
            ));
        }
        self.resolve_comptime_array(ty, vec![element; repeat_count], site)
    }

    fn resolve_comptime_named_value(
        &mut self,
        file: Self::File,
        name: Self::Name,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<
        rue_air::ComptimeNamedValueResolution<Self::Value>,
        Self::Failure,
    > {
        let program = file.program().clone();
        let projection = self
            .services
            .resolve_named_value(
                &program.declaration,
                program.declaration.module(),
                name.as_str(),
            )
            .map_err(durable_provider_error)?;
        let Some(projection) = projection else {
            return Err(rue_air::ComptimeHostError::HostFailure(
                DurableComptimeHostFailure::semantic(Box::new(SemanticNucleusFailure::Resolution(
                    Arc::from(format!("undefined constant `{}`", name.as_str())),
                ))),
            ));
        };
        let (value, dependency, anonymous_nominals) = projection.into_parts();
        self.services
            .durable_session_mut()
            .observe_dependency(dependency);
        for nominal in anonymous_nominals.iter().cloned() {
            self.services
                .durable_session_mut()
                .observe_anonymous_nominal(nominal);
        }
        Ok(rue_air::ComptimeNamedValueResolution::Known(value))
    }

    fn match_path_pattern(
        &mut self,
        pattern: &rue_air::ComptimeMatchPattern<Self::Name>,
        value: &Self::Value,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<Option<bool>, Self::Failure> {
        if durable_target_path_pattern_matches(pattern, value) {
            return Ok(Some(true));
        }
        let rue_air::ComptimeMatchPattern::Path {
            module_qualified: false,
            type_name,
            variant,
            ..
        } = pattern
        else {
            return Ok(Some(false));
        };
        let EvaluatedSemanticConst::Value(value) = value else {
            return Ok(Some(false));
        };
        let rue_air::SemanticImportConstValue::Aggregate(aggregate) = &value.value else {
            return Ok(Some(false));
        };
        let rue_air::SemanticImportAggregateKind::Enum { variant: index, .. } = &aggregate.kind
        else {
            return Ok(Some(false));
        };
        // Resolve the pattern head through the same durable type authority as
        // calls and constructors. Comparing display names would reject aliases
        // and could conflate distinct nominals with the same spelling.
        let Some(pattern_ty) = <Self as rue_air::ComptimeTypeAlgebra>::resolve_named_type_value(
            self,
            site.program(),
            type_name.clone(),
            site.span(),
        )?
        else {
            return Ok(Some(false));
        };
        if pattern_ty.0 != aggregate.ty {
            return Ok(Some(false));
        }
        let expected = self
            .services
            .resolve_enum_variant_index(&aggregate.ty, variant.as_str())
            .map_err(durable_provider_error)?;
        Ok(Some(expected.is_some_and(|expected| expected == *index)))
    }

    fn match_no_selected_arm(
        &self,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        match durable_host_error(DurableComptimeFailure::comptime_match_no_selected_arm()) {
            rue_air::ComptimeHostError::HostFailure(error) => {
                rue_air::ComptimeOutcome::HostFailure(error)
            }
            rue_air::ComptimeHostError::Abort(error) => rue_air::ComptimeOutcome::Abort(error),
        }
    }

    fn evaluate_binary_rhs_after_rejection(&self) -> bool {
        true
    }

    fn compare_comptime_values(
        &mut self,
        lhs: &Self::Value,
        rhs: &Self::Value,
        equal: bool,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        if let (EvaluatedSemanticConst::TargetEnum(lhs), EvaluatedSemanticConst::TargetEnum(rhs)) =
            (lhs, rhs)
        {
            return rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::boolean(if equal {
                lhs == rhs
            } else {
                lhs != rhs
            }));
        }
        durable_host_error_outcome(durable_host_error(
            DurableComptimeFailure::comptime_rejection(
                rue_air::ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation: rue_air::ComptimeIntegerOperation::Add,
                    lhs: lhs.clone(),
                    rhs: Some(rhs.clone()),
                },
            ),
        ))
    }

    fn finish_arith(
        &self,
        result: rue_air::integer_semantics::CheckedIntegerResult,
        ty: Option<Self::Type>,
        op: &str,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        let ty = ty.unwrap_or(DurableComptimeType(DurableType::I32));
        let value = DurableComptimeScalarPolicy::checked_integer_result(ty.as_ref(), result, op)
            .map_err(durable_host_error)?;
        Ok(Some(EvaluatedSemanticConst::integer_typed(value, Some(ty))))
    }

    fn resolve_string_const(
        &mut self,
        content: Self::Name,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::Value(Arc::new(
            TypedSemanticConst {
                value: DurableConstValue::String(content.0),
                ty: None,
            },
        )))
    }

    fn string_value_text(&self, value: &Self::Value) -> Option<String> {
        let EvaluatedSemanticConst::Value(value) = value else {
            return None;
        };
        match &value.value {
            DurableConstValue::String(content) => Some(content.to_string()),
            _ => None,
        }
    }

    fn resolve_float_const(
        &mut self,
        content: Self::Name,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        let Some(canonical) = rue_air::canonical_decimal_literal(content.0.as_ref()) else {
            return rue_air::ComptimeOutcome::RuntimeDependent;
        };
        rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::Value(Arc::new(
            TypedSemanticConst {
                value: DurableConstValue::Float(Arc::from(canonical)),
                ty: Some(DurableType::ComptimeFloat),
            },
        )))
    }

    fn float_value_text(&self, value: &Self::Value) -> Option<String> {
        let EvaluatedSemanticConst::Value(typed) = value else {
            return None;
        };
        match &typed.value {
            DurableConstValue::Float(text) => Some(text.to_string()),
            _ => None,
        }
    }

    fn float_value_from_text(
        &mut self,
        text: &str,
        ty: Option<Self::Type>,
    ) -> rue_air::ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        Ok(Some(EvaluatedSemanticConst::Value(Arc::new(
            TypedSemanticConst {
                value: DurableConstValue::Float(Arc::from(text)),
                ty: Some(ty.map_or(DurableType::ComptimeFloat, |ty| ty.0)),
            },
        ))))
    }

    fn resolve_comptime_expression_intrinsic(
        &mut self,
        request: rue_air::ComptimeExpressionIntrinsicRequest<Self::Name>,
        site: &rue_air::ComptimeSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        match request {
            rue_air::ComptimeExpressionIntrinsicRequest::Import {
                argument_count: 1,
                sole_string_literal: Some(specifier),
            } => {
                let resolution = self.services.resolve_keyed_import(site, specifier.as_str());
                let resolution = match resolution {
                    Ok(resolution) => resolution,
                    Err(DurableComptimeKeyedImportError::ProviderAbort(abort)) => {
                        return rue_air::ComptimeOutcome::Abort(
                            DurableComptimeHostFailure::query_abort(abort),
                        );
                    }
                    Err(_) => {
                        return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                            DurableComptimeFailure::resolution(
                                "exact const import is absent from its candidate RIR occurrence index",
                            ),
                        ));
                    }
                };
                match resolution {
                    DurableImportResolution::Resolved(module) => {
                        rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::Module(module))
                    }
                    DurableImportResolution::Missing => rue_air::ComptimeOutcome::HostFailure(
                        durable_host_failure(DurableComptimeFailure::resolution(format!(
                            "cannot find module `{}`",
                            specifier.as_str()
                        ))),
                    ),
                    DurableImportResolution::Failure(
                        DeclarationImportFailure::ResolutionUnavailable(key),
                    ) => rue_air::ComptimeOutcome::Abort(DurableComptimeHostFailure::query_abort(
                        QueryAbort::MissingInput(rue_query::InputIdentity::new(
                            "declaration-import-resolution",
                            format!(
                                "{}:{}:{}",
                                key.declaration.stable_identity(),
                                key.occurrence,
                                key.specifier
                            ),
                        )),
                    )),
                    DurableImportResolution::Failure(failure) => {
                        rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                            DurableComptimeFailure::resolution(format!("{failure:?}")),
                        ))
                    }
                }
            }
            rue_air::ComptimeExpressionIntrinsicRequest::Import { .. } => {
                rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                    DurableComptimeFailure::resolution(
                        "exact const import is absent from its candidate RIR occurrence index",
                    ),
                ))
            }
            rue_air::ComptimeExpressionIntrinsicRequest::Target {
                intrinsic,
                argument_count,
            } => match self
                .services
                .resolve_target_intrinsic(intrinsic, argument_count)
            {
                Ok(value) => {
                    rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::TargetEnum(value))
                }
                Err(error) => durable_host_error_outcome(durable_provider_error(error)),
            },
        }
    }

    fn resolve_comptime_enum_variant(
        &mut self,
        module: Option<Self::Value>,
        type_name: Self::Name,
        variant: Self::Name,
        _site: &rue_air::ComptimeSite<Self::ProgramKey>,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        if module.is_some() {
            return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                DurableComptimeFailure::comptime_rejection(
                    rue_air::ComptimeSemanticRejection::UnsupportedExpression,
                ),
            ));
        }
        if !matches!(type_name.as_str(), "Arch" | "Os" | "DataModel") {
            return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                DurableComptimeFailure::resolution(
                    "path expression is not supported in declaration-time comptime",
                ),
            ));
        }
        match self
            .services
            .resolve_target_enum_variant(type_name.as_str(), variant.as_str())
        {
            Ok(value) => rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::TargetEnum(value)),
            Err(error) => durable_host_error_outcome(durable_provider_error(error)),
        }
    }

    fn resolve_comptime_enum_variant_with_payload(
        &mut self,
        enum_type: DurableComptimeType,
        variant: Self::Name,
        payload: Vec<Self::Value>,
        _site: &rue_air::ComptimeSite<Self::ProgramKey>,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        let index = match self
            .services
            .resolve_enum_variant_index(&enum_type.0, variant.as_str())
        {
            Ok(Some(index)) => index,
            Ok(None) => return rue_air::ComptimeOutcome::RuntimeDependent,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        match self.services.type_is_copy(&enum_type.0) {
            Ok(true) => {}
            Ok(false) => {
                return durable_host_error_outcome(durable_host_error(
                    DurableComptimeFailure::comptime_failure(
                        "structural comptime values require a Copy type",
                    ),
                ));
            }
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        }
        let payload_types = match self
            .services
            .resolve_enum_variant_payload_types(&enum_type.0, index)
        {
            Ok(payload_types) => payload_types,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        if payload.len() != payload_types.len() {
            return rue_air::ComptimeOutcome::RuntimeDependent;
        }
        let mut nodes = 0;
        for (value, ty) in payload.iter().zip(payload_types.iter()) {
            if let Some(found) = Self::durable_child_type_mismatch(value, ty) {
                return durable_host_error_outcome(Self::durable_child_type_error(found, ty));
            }
            let Some(value) = into_durable_value(value.clone()) else {
                return rue_air::ComptimeOutcome::RuntimeDependent;
            };
            match self.validate_durable_value(&value, ty, 0, &mut nodes) {
                Ok(true) => {}
                Ok(false) => return rue_air::ComptimeOutcome::RuntimeDependent,
                Err(error) => return durable_host_error_outcome(error),
            }
        }
        EvaluatedSemanticConst::aggregate_enum(enum_type, index, payload).map_or(
            rue_air::ComptimeOutcome::RuntimeDependent,
            rue_air::ComptimeOutcome::Known,
        )
    }

    fn admit_comptime_enum_variant(
        &mut self,
        _type_name: Self::Name,
        _variant: Self::Name,
        has_module: bool,
        _site: &rue_air::ComptimeSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<bool, Self::Failure> {
        // A qualified enum path is not a durable target descriptor. Reject
        // it before AIR evaluates the optional module child, preserving the
        // established pre-child policy and avoiding an ambient module lookup.
        if has_module {
            #[cfg(test)]
            arm_enum_variant_child_tripwire();
            return Err(durable_host_error(
                DurableComptimeFailure::comptime_rejection(
                    ComptimeSemanticRejection::UnsupportedExpression,
                ),
            ));
        }
        Ok(true)
    }

    fn admit_comptime_member(
        &mut self,
        _field: Self::Name,
        _site: &rue_air::ComptimeSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<bool, Self::Failure> {
        Ok(true)
    }

    fn resolve_comptime_member(
        &mut self,
        base: Self::Value,
        field: Self::Name,
        site: &rue_air::ComptimeSite<Self::ProgramKey>,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        if let EvaluatedSemanticConst::Value(value) = &base {
            if let DurableConstValue::Type(enum_type) = &value.value {
                match self.services.type_is_copy(enum_type) {
                    Ok(true) => {}
                    Ok(false) => {
                        return durable_host_error_outcome(durable_host_error(
                            DurableComptimeFailure::comptime_failure(
                                "structural comptime values require a Copy type",
                            ),
                        ));
                    }
                    Err(error) => {
                        return durable_host_error_outcome(durable_provider_error(error));
                    }
                }
                let index = match self
                    .services
                    .resolve_enum_variant_index(enum_type, field.as_str())
                {
                    Ok(Some(index)) => index,
                    Ok(None) => return rue_air::ComptimeOutcome::RuntimeDependent,
                    Err(error) => {
                        return durable_host_error_outcome(durable_provider_error(error));
                    }
                };
                let payload_types = match self
                    .services
                    .resolve_enum_variant_payload_types(enum_type, index)
                {
                    Ok(payload_types) => payload_types,
                    Err(error) => {
                        return durable_host_error_outcome(durable_provider_error(error));
                    }
                };
                // A payload-bearing variant needs an explicit constructor
                // call.  A member path cannot silently manufacture its
                // payload, since that would publish an invalid enum value.
                if !payload_types.is_empty() {
                    return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                        DurableComptimeFailure::failure(SemanticNucleusFailure::Diagnostic(
                            rue_error::ErrorKind::WrongArgumentCount {
                                expected: payload_types.len(),
                                found: 0,
                            },
                        )),
                    ));
                }
                return EvaluatedSemanticConst::aggregate_enum(
                    DurableComptimeType(enum_type.clone()),
                    index,
                    Vec::new(),
                )
                .map_or(
                    rue_air::ComptimeOutcome::RuntimeDependent,
                    rue_air::ComptimeOutcome::Known,
                );
            }
            if let DurableConstValue::Aggregate(aggregate) = &value.value {
                if let rue_air::SemanticImportAggregateKind::Struct(fields) = &aggregate.kind {
                    let Some(index) = (match self
                        .services
                        .resolve_struct_field_index(&aggregate.ty, field.as_str())
                    {
                        Ok(index) => index,
                        Err(error) => {
                            return durable_host_error_outcome(durable_provider_error(error));
                        }
                    }) else {
                        return rue_air::ComptimeOutcome::RuntimeDependent;
                    };
                    let Some(field_value) = fields.get(index as usize).cloned() else {
                        return rue_air::ComptimeOutcome::RuntimeDependent;
                    };
                    let field_type = match self
                        .services
                        .resolve_struct_field_type(&aggregate.ty, index)
                    {
                        Ok(Some(field_type)) => field_type,
                        Ok(None) => return rue_air::ComptimeOutcome::RuntimeDependent,
                        Err(error) => {
                            return durable_host_error_outcome(durable_provider_error(error));
                        }
                    };
                    return rue_air::ComptimeOutcome::Known(
                        evaluated_from_durable_value_with_type(field_value, field_type),
                    );
                }
            }
        }
        let EvaluatedSemanticConst::Module(module) = base else {
            return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                DurableComptimeFailure::resolution("member access on a non-module const value"),
            ));
        };
        let projection = self.services.resolve_module_member(
            &site.program().declaration,
            &module,
            field.as_str(),
        );
        let projection = match projection {
            Ok(projection) => projection,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        let (value, dependency, anonymous_nominals) = projection.into_parts();
        self.services
            .durable_session_mut()
            .observe_dependency(dependency);
        for nominal in anonymous_nominals.iter().cloned() {
            self.services
                .durable_session_mut()
                .observe_anonymous_nominal(nominal);
        }
        rue_air::ComptimeOutcome::Known(value)
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeCallProtocol
    for DurableComptimeHost<'_, A>
{
    fn resolve_module_comptime_callable(
        &mut self,
        _file_id: Self::File,
        _segments: &[Self::Name],
        _method: Self::Name,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<Option<Self::Name>, Self::Failure> {
        Ok(None)
    }

    fn comptime_method_receiver_policy(&self) -> rue_air::ComptimeMethodReceiverPolicy {
        rue_air::ComptimeMethodReceiverPolicy::EvaluateReceiver
    }

    fn admit_evaluated_comptime_method(
        &mut self,
        receiver: Self::Value,
        method: Self::Name,
        argument_count: usize,
        argument_modes: &[rue_air::ComptimeArgMode],
        env: &mut rue_air::ComptimeEnv<
            '_,
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::CanonicalIdentity,
        >,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeOutcome<
        Option<rue_air::ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
        Self::Failure,
    > {
        if argument_count != argument_modes.len() {
            return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                DurableComptimeFailure::resolution(
                    "durable comptime call argument metadata is inconsistent",
                ),
            ));
        }
        let EvaluatedSemanticConst::Module(module) = receiver else {
            return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                DurableComptimeFailure::resolution(
                    "method call in declaration-time comptime requires a module receiver",
                ),
            ));
        };
        let Some(file) = env.defining_file.as_ref() else {
            return rue_air::ComptimeOutcome::RuntimeDependent;
        };
        match self.admit_call_for_module(
            &file.program().declaration,
            &module,
            &method,
            argument_modes,
        ) {
            Ok(admitted) => rue_air::ComptimeOutcome::Known(Some(rue_air::ComptimeCallAdmission {
                name: method,
                payload: admitted,
            })),
            Err(error) => durable_host_error_outcome(error),
        }
    }

    fn admit_comptime_call(
        &mut self,
        name: Self::Name,
        argument_count: usize,
        argument_modes: &[rue_air::ComptimeArgMode],
        env: &mut rue_air::ComptimeEnv<
            '_,
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::CanonicalIdentity,
        >,
        _name_is_resolved_key: bool,
    ) -> rue_air::ComptimeHostResult<
        Option<rue_air::ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
        Self::Failure,
    > {
        if argument_count != argument_modes.len() {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                "durable comptime call argument metadata is inconsistent",
            )));
        }
        let Some(file) = env.defining_file.as_ref() else {
            return Ok(None);
        };
        let program = file.program().clone();
        let admitted = self.admit_call_for_module(
            &program.declaration,
            program.declaration.module(),
            &name,
            argument_modes,
        )?;
        Ok(Some(rue_air::ComptimeCallAdmission {
            name,
            payload: admitted,
        }))
    }

    fn begin_comptime_call_binding(
        &self,
        admission: &rue_air::ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        argument_count: usize,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<Self::CallBinding, Self::Failure> {
        if admission.payload.parameters().len() != argument_count
            || admission.payload.shell_parameters().len() != argument_count
        {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                "durable comptime call binding arity mismatch",
            )));
        }
        Ok(DurableComptimeBinding::new(&admission.payload))
    }

    fn bind_comptime_call_argument(
        &self,
        binding: &mut Self::CallBinding,
        argument: rue_air::ComptimeCallArgument<Self::Value>,
        index: usize,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<bool, Self::Failure> {
        let Some(parameter) = binding.parameter(index).cloned() else {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                "durable comptime call argument index is out of bounds",
            )));
        };
        let Some(header) = binding.shell_parameter(index).cloned() else {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                "durable comptime call shell argument index is out of bounds",
            )));
        };
        let EvaluatedSemanticConst::Value(value) = argument.value() else {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                match argument.value() {
                    EvaluatedSemanticConst::Module(_) => "module used where a value is required",
                    EvaluatedSemanticConst::TargetEnum(_) => {
                        "target descriptor used where a durable const value is required"
                    }
                    EvaluatedSemanticConst::Value(_) => unreachable!(),
                },
            )));
        };
        if matches!(value.value, DurableConstValue::Aggregate(_)) {
            let expected = substitute_durable_generics(
                &parameter.ty,
                &binding
                    .type_arguments()
                    .iter()
                    .map(|(_, ty)| ty.clone())
                    .collect::<Vec<_>>(),
            );
            if !self
                .services
                .type_is_copy(&expected)
                .map_err(durable_provider_error)?
            {
                return Err(durable_host_error(
                    DurableComptimeFailure::comptime_failure(
                        "structural comptime values require a Copy type",
                    ),
                ));
            }
        }
        bind_durable_comptime_argument(
            binding,
            &header.name,
            &parameter,
            Arc::unwrap_or_clone(value.clone()),
            argument.is_direct_unit_literal(),
        )
        .map_err(durable_host_error)?;
        Ok(true)
    }

    fn comptime_call_argument_type(
        &self,
        binding: &Self::CallBinding,
        index: usize,
    ) -> Option<Self::Type> {
        binding
            .parameter(index)
            .map(|parameter| DurableComptimeType(parameter.ty.clone()))
    }

    fn finish_comptime_call_binding(
        &mut self,
        binding: Self::CallBinding,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<Option<Self::BoundCall>, Self::Failure> {
        Ok(Some(binding.finish()))
    }

    fn prepare_comptime_call(
        &mut self,
        admission: rue_air::ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        bound: Self::BoundCall,
        span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<
        Option<
            rue_air::ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
                Self::CompletionTicket,
            >,
        >,
        Self::Failure,
    > {
        let pending = self
            .services
            .durable_session_mut()
            .prepare_bound_expression_call(admission.payload, bound)
            .map_err(|error| {
                durable_host_error(DurableComptimeFailure::resolution(format!(
                    "durable call lifecycle: {error:?}"
                )))
            })?;
        if let Some(failure) = self
            .services
            .durable_session()
            .active_pending_call_cycle(&pending)
        {
            return Err(durable_host_error(DurableComptimeFailure::failure(failure)));
        }
        let probed = self
            .services
            .probe_prepared_call(pending)
            .map_err(|abort| {
                rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure::query_abort(abort))
            })?;
        let prepared = self
            .services
            .durable_session_mut()
            .consume_probed_call(probed, span)
            .map_err(durable_foreign_call_error)?;
        Ok(Some(match prepared {
            DurableComptimePreparedCall::Ready {
                result,
                expected_result,
            } => {
                let value = match result {
                    crate::semantic_query_nucleus::ComptimeCallResultProjection::Type(value) => {
                        DurableConstValue::Type(value)
                    }
                    crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(value) => {
                        value
                    }
                };
                rue_air::ComptimeCallPreparation::Memoized(rue_air::ComptimeOutcome::Known(
                    EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        value,
                        expected_result,
                    )),
                ))
            }
            DurableComptimePreparedCall::Enter { frame, ticket } => {
                rue_air::ComptimeCallPreparation::Enter {
                    frame: *frame,
                    ticket,
                }
            }
            DurableComptimePreparedCall::NotReady => {
                rue_air::ComptimeCallPreparation::Memoized(rue_air::ComptimeOutcome::NotReady)
            }
        }))
    }

    fn finish_comptime_call(
        &mut self,
        frame: &rue_air::ComptimeFrame<
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::ProgramKey,
            Self::CanonicalIdentity,
        >,
        mut ticket: Self::CompletionTicket,
        result: rue_air::ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        let result = match (result, frame.expected_result.as_ref()) {
            (
                rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::Value(value)),
                Some(expected),
            ) => rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::Value(
                TypedSemanticConst::typed(value.value.clone(), expected.0.clone()),
            )),
            (result, _) => result,
        };
        match self
            .services
            .durable_session_mut()
            .finish_call(&mut ticket, &result)
        {
            Ok(()) => result,
            Err(error) => rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                DurableComptimeFailure::resolution(format!("durable call lifecycle: {error:?}")),
            )),
        }
    }

    fn enter_comptime_call(
        &mut self,
        _frame: &rue_air::ComptimeFrame<
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::ProgramKey,
            Self::CanonicalIdentity,
        >,
        ticket: &Self::CompletionTicket,
    ) -> rue_air::ComptimeHostResult<(), Self::Failure> {
        self.services
            .durable_session_mut()
            .enter_call(ticket)
            .map_err(|error| {
                durable_host_error(DurableComptimeFailure::resolution(format!(
                    "durable call lifecycle: {error:?}"
                )))
            })
    }

    fn canonical_function_producer(
        &self,
        program: &Self::ProgramKey,
        ticket: &Self::CompletionTicket,
        _name: Self::Name,
        _types: &AHashMap<Self::Name, Self::Type>,
        _values: &AHashMap<Self::Name, Self::Value>,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<Self::CanonicalIdentity, Self::Failure> {
        ticket
            .canonical_function_producer(program)
            .map(DurableComptimeIdentity)
            .map_err(|error| {
                durable_host_error(DurableComptimeFailure::resolution(format!(
                    "failed to issue canonical comptime producer: {error:?}"
                )))
            })
    }

    fn issue_anonymous_identity(
        &self,
        _program: &Self::ProgramKey,
        kind: rue_air::ComptimeAnonymousKind,
        producer: &Self::CanonicalIdentity,
        anchor: &rue_rir::RirStructuralAnchor,
    ) -> Self::AnonymousIdentity {
        DurableComptimeAnonymousIdentity::new(
            crate::AnonymousNominalKey {
                kind: match kind {
                    rue_air::ComptimeAnonymousKind::Struct => rue_air::AnonymousNominalKind::Struct,
                    rue_air::ComptimeAnonymousKind::Enum => rue_air::AnonymousNominalKind::Enum,
                },
                producer: producer.0.clone(),
                anchor: anchor.clone(),
            }
            .with_canonical_producer()
            .into_owned(),
        )
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeStructuredTypes
    for DurableComptimeHost<'_, A>
{
    fn begin_comptime_type_syntax(
        &mut self,
        program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<Self::Name, Self::Type>,
        values: &AHashMap<Self::Name, Self::Value>,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeOutcome<
        rue_air::ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
        Self::Failure,
    > {
        let mut type_substitutions = types
            .iter()
            .map(|(name, ty)| (name.0.clone(), ty.0.clone()))
            .collect::<Vec<_>>();
        type_substitutions.sort_by(|left, right| left.0.cmp(&right.0));
        let mut value_substitutions = Vec::with_capacity(values.len());
        for (name, value) in values {
            let EvaluatedSemanticConst::Value(value) = value else {
                return rue_air::ComptimeOutcome::RuntimeDependent;
            };
            value_substitutions.push((name.0.clone(), value.value.clone()));
        }
        value_substitutions.sort_by(|left, right| left.0.cmp(&right.0));
        match self.services.begin_structured_type(
            program,
            syntax,
            type_substitutions,
            value_substitutions,
        ) {
            Ok(DurableStructuredTypePoll::Ready(ty)) => rue_air::ComptimeOutcome::Known(
                rue_air::ComptimeStructuredTypeResolution::Ready(DurableComptimeType(ty)),
            ),
            Ok(DurableStructuredTypePoll::Suspended(job)) => rue_air::ComptimeOutcome::Known(
                rue_air::ComptimeStructuredTypeResolution::Suspended(*job),
            ),
            Err(DurableStructuredTypeBeginError::Resolution(error)) => {
                durable_host_error_outcome(durable_type_syntax_error(error))
            }
            Err(DurableStructuredTypeBeginError::UnregisteredProgram) => {
                durable_host_error_outcome(durable_host_error(DurableComptimeFailure::resolution(
                    "durable comptime type syntax references an unregistered program",
                )))
            }
            Err(DurableStructuredTypeBeginError::InvalidProgramAuthority) => {
                durable_host_error_outcome(durable_host_error(DurableComptimeFailure::resolution(
                    "durable comptime type syntax has invalid program authority",
                )))
            }
        }
    }

    fn prepare_structured_type_call(
        &mut self,
        suspension: &Self::StructuredTypeSuspension,
        span: rue_span::Span,
    ) -> rue_air::ComptimeOutcome<
        Option<
            rue_air::ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
                Self::CompletionTicket,
            >,
        >,
        Self::Failure,
    > {
        let request = suspension.request_view();
        let program = request.program().key().clone();
        let head = request.head().key.clone();
        let argument_count = request.type_arguments().len() + request.value_arguments().len();
        let pending = match self
            .services
            .durable_session_mut()
            .prepare_structured_type_call(suspension, span)
        {
            Ok(pending) => pending,
            Err(error) => {
                return durable_host_error_outcome(durable_host_error(
                    DurableComptimeFailure::resolution(format!(
                        "durable structured call lifecycle: {error:?}"
                    )),
                ));
            }
        };
        if let Some(failure) = self.services.durable_session().active_comptime_call_cycle(
            &head,
            &program.configuration,
            request.type_arguments(),
            request.value_arguments(),
        ) {
            return durable_host_error_outcome(durable_host_error(
                DurableComptimeFailure::failure(failure),
            ));
        }
        let start = match self
            .services
            .begin_comptime_call_admission_for_key(&program.declaration, &head)
        {
            Ok(start) => start,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        self.services
            .durable_session_mut()
            .observe_dependency(start.dependency.clone());
        if let Some(alias) = start.alias_dependency.clone() {
            self.services
                .durable_session_mut()
                .observe_dependency(alias);
        }
        let admission = match self
            .services
            .finish_structured_comptime_call_admission(start, argument_count)
        {
            Ok(admission) => admission,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        let validated = match self
            .services
            .durable_session()
            .validate_structured_type_call(pending, admission)
        {
            Ok(validated) => validated,
            Err(error) => {
                return durable_host_error_outcome(durable_foreign_call_error(error));
            }
        };
        let probed = match self.services.probe_structured_type_call(validated) {
            Ok(probed) => probed,
            Err(abort) => {
                return rue_air::ComptimeOutcome::Abort(DurableComptimeHostFailure::query_abort(
                    abort,
                ));
            }
        };
        let prepared = match self
            .services
            .durable_session_mut()
            .consume_structured_type_call(probed)
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return durable_host_error_outcome(durable_foreign_call_error(error));
            }
        };
        rue_air::ComptimeOutcome::Known(Some(match prepared {
            DurableStructuredTypeCall::Ready { result } => {
                let value = match result {
                    crate::semantic_query_nucleus::ComptimeCallResultProjection::Type(value) => {
                        DurableConstValue::Type(value)
                    }
                    crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(value) => {
                        value
                    }
                };
                rue_air::ComptimeCallPreparation::Memoized(rue_air::ComptimeOutcome::Known(
                    EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        value,
                        DurableType::ComptimeType,
                    )),
                ))
            }
            DurableStructuredTypeCall::Enter {
                program: _,
                frame,
                ticket,
            } => rue_air::ComptimeCallPreparation::Enter {
                frame: *frame,
                ticket,
            },
            DurableStructuredTypeCall::NotReady => {
                rue_air::ComptimeCallPreparation::Memoized(rue_air::ComptimeOutcome::NotReady)
            }
        }))
    }

    fn resume_structured_type_call(
        &mut self,
        suspension: Self::StructuredTypeSuspension,
        result: rue_air::ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> rue_air::ComptimeOutcome<
        rue_air::ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
        Self::Failure,
    > {
        let value = match result {
            rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::Value(value)) => {
                Arc::unwrap_or_clone(value)
            }
            rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::Module(_)) => {
                return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                    DurableComptimeFailure::resolution("module used where a value is required"),
                ));
            }
            rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::TargetEnum(_)) => {
                return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                    DurableComptimeFailure::resolution(
                        "target descriptor used where a durable const value is required",
                    ),
                ));
            }
            rue_air::ComptimeOutcome::RuntimeDependent => {
                return rue_air::ComptimeOutcome::RuntimeDependent;
            }
            rue_air::ComptimeOutcome::NotReady => return rue_air::ComptimeOutcome::NotReady,
            rue_air::ComptimeOutcome::UnsupportedContext => {
                return rue_air::ComptimeOutcome::UnsupportedContext;
            }
            rue_air::ComptimeOutcome::Trap(trap) => {
                return rue_air::ComptimeOutcome::Trap(trap);
            }
            rue_air::ComptimeOutcome::HostFailure(error) => {
                return rue_air::ComptimeOutcome::HostFailure(error);
            }
            rue_air::ComptimeOutcome::Abort(error) => {
                return rue_air::ComptimeOutcome::Abort(error);
            }
        };
        let reduced = Some(match value.value {
            DurableConstValue::Type(ty) => rue_air::SemanticComptimeCallResult::Type(ty),
            value => rue_air::SemanticComptimeCallResult::Value(value),
        });
        match self
            .services
            .resume_structured_type(suspension, Ok(reduced))
        {
            Ok(DurableStructuredTypePoll::Ready(ty)) => rue_air::ComptimeOutcome::Known(
                rue_air::ComptimeStructuredTypeResolution::Ready(DurableComptimeType(ty)),
            ),
            Ok(DurableStructuredTypePoll::Suspended(job)) => rue_air::ComptimeOutcome::Known(
                rue_air::ComptimeStructuredTypeResolution::Suspended(*job),
            ),
            Err(error) => durable_host_error_outcome(durable_type_syntax_error(error)),
        }
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeRejections
    for DurableComptimeHost<'_, A>
{
    fn reject_comptime_expression(
        &self,
        rejection: rue_air::ComptimeSemanticRejection<Self::Value>,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        match durable_host_error(DurableComptimeFailure::comptime_rejection(rejection)) {
            rue_air::ComptimeHostError::HostFailure(error) => {
                rue_air::ComptimeOutcome::HostFailure(error)
            }
            rue_air::ComptimeHostError::Abort(error) => rue_air::ComptimeOutcome::Abort(error),
        }
    }

    fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<(), Self::Failure> {
        if site
            .program()
            .configuration
            .preview_features
            .contains(feature)
        {
            return Ok(());
        }
        // The same `help:` line body analysis and the request-level closure
        // gate attach, assembled by the one authority in rue-error. This site
        // used to drop it, so the identical diagnostic told a user how to
        // enable the feature only when it happened to come from elsewhere.
        Err(rue_air::ComptimeHostError::HostFailure(
            durable_diagnostic_failure_with_help(
                &self.diagnostic_site(site),
                rue_error::ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_owned(),
                },
                feature.enable_help(),
            ),
        ))
    }

    fn depth_exceeded(
        &self,
        name: &Self::Name,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        durable_host_failure(DurableComptimeFailure::maximum_depth(name.as_str()))
    }

    fn literal_out_of_range(
        &self,
        value: i128,
        ty: &Self::Type,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        durable_diagnostic_failure(
            &self.diagnostic_site(site),
            rue_error::ErrorKind::LiteralOutOfRange {
                value,
                ty: DurableComptimeScalarPolicy::type_name(ty.as_ref()),
            },
        )
    }

    fn cannot_negate(
        &self,
        ty: &Self::Type,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        durable_diagnostic_failure(
            &self.diagnostic_site(site),
            rue_error::ErrorKind::CannotNegate(DurableComptimeScalarPolicy::type_name(ty.as_ref())),
        )
    }

    fn label_ctor_instantiation_site(
        error: Self::Failure,
        _call_span: rue_span::Span,
    ) -> Self::Failure {
        error
    }

    fn finish_checked(
        &mut self,
        value: Self::Value,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        rue_air::ComptimeOutcome::Known(value)
    }

    fn reject_non_type_array_repeat(
        &mut self,
        _value: Self::Value,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeOutcome<Self::Value, Self::Failure> {
        rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
            DurableComptimeFailure::comptime_rejection(
                rue_air::ComptimeSemanticRejection::AggregateExpression,
            ),
        ))
    }

    fn allow_checked_comptime(&self) -> bool {
        true
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeHost
    for DurableComptimeHost<'_, A>
{
}

/// The compiler's ticket-free declaration-root frame. `StableProducerId`
/// preserves specialized function producers even though a declaration root
/// leaves `call_identity` empty; the program key independently prevents dense
/// instruction references from being interpreted against another arena.
#[allow(dead_code)]
pub(crate) type DurableComptimeConstFrame = rue_air::ComptimeFrame<
    EvaluatedSemanticConst,
    DurableComptimeType,
    DurableComptimeName,
    DurableComptimeFile,
    crate::body_query::DurableComptimeProgramKey,
    DurableComptimeIdentity,
>;

/// The keyed frame handed to AIR for an admitted foreign callable.  It uses
/// the same compiler-owned value/type/name/file/identity domains as a const
/// root; only the call fields differ.
#[allow(dead_code)]
pub(crate) type DurableComptimeForeignFrame = DurableComptimeConstFrame;
