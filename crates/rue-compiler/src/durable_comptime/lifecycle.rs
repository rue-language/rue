//! Admission, lifecycle, finalization, and one-shot token ownership.
//!
//! A reservation may create exactly one admission token, an admitted edge may
//! create exactly one ticket, and only the matching active lifecycle may enter
//! or finish that ticket. Non-known AIR outcomes always clean up and never
//! publish child effects.

use super::diagnostics::*;
use super::effects::*;
use super::host::*;
use super::projection::*;
use super::structured::*;
use super::*;

/// The exact identity carried by an admitted foreign call.
///
/// The fields are private deliberately: a caller cannot construct a query
/// whose configuration, ordered arguments, producer, and program disagree.
/// The only production constructor derives all of them from the owned
/// admission payload.
#[allow(dead_code)] // carried by the canonical root-integrated AIR host
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeCallContext {
    query: crate::semantic_query_nucleus::ComptimeCallQueryKey,
    parent_producer: crate::StableDefinitionKey,
    parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    child_producer: crate::StableDefinitionKey,
    program: crate::body_query::DurableComptimeProgramKey,
    application_policy: DurableComptimeApplicationPolicy,
}

/// Failure while turning the ordered durable call arguments into a stable
/// specialization identity.  This kernel is semantic-only: it owns no RIR,
/// evaluator, query, or lifecycle authority.
#[allow(dead_code)] // consumed by the root-integrated durable AIR host
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeProducerIssuanceError {
    ProgramMismatch,
    InvalidTypeArgument,
    InvalidValueArgument,
}

pub(crate) fn canonical_specialized_function_producer(
    base: &crate::StableDefinitionKey,
    type_arguments: &[(Arc<str>, DurableType)],
    value_arguments: &[(Arc<str>, DurableConstValue)],
) -> Result<crate::StableProducerId, DurableComptimeProducerIssuanceError> {
    let function = canonical_specialized_function_instance(base, type_arguments, value_arguments)?;
    Ok(crate::StableProducerId::Function(rue_air::Node::new(
        function,
    )))
}

pub(crate) fn canonical_specialized_function_instance(
    base: &crate::StableDefinitionKey,
    type_arguments: &[(Arc<str>, DurableType)],
    value_arguments: &[(Arc<str>, DurableConstValue)],
) -> Result<crate::FunctionInstanceKey, DurableComptimeProducerIssuanceError> {
    let types = type_arguments
        .iter()
        .map(|(_, value)| crate::semantic_identity::type_instance_from_semantic(value))
        .collect::<Option<Vec<_>>>()
        .ok_or(DurableComptimeProducerIssuanceError::InvalidTypeArgument)?
        .into();
    let values = value_arguments
        .iter()
        .map(|(_, value)| crate::semantic_identity::argument_value_from_semantic(value))
        .collect::<Option<Vec<_>>>()
        .ok_or(DurableComptimeProducerIssuanceError::InvalidValueArgument)?
        .into();
    Ok(
        crate::semantic_identity::function_instance_from_canonical_arguments(
            base.clone(),
            types,
            values,
        ),
    )
}

impl DurableComptimeCallContext {
    #[allow(dead_code)] // consumed by the root-integrated durable AIR host
    fn canonical_function_producer(
        &self,
        program: &crate::body_query::DurableComptimeProgramKey,
    ) -> Result<crate::StableProducerId, DurableComptimeProducerIssuanceError> {
        if &self.program != program
            || self.child_producer != program.declaration
            || self.query.declaration.declaration.module != *program.declaration.module()
            || self.query.declaration.configuration != program.configuration
        {
            return Err(DurableComptimeProducerIssuanceError::ProgramMismatch);
        }
        canonical_specialized_function_producer(
            &self.child_producer,
            &self.query.type_arguments,
            &self.query.value_arguments,
        )
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn from_admitted_expression(
        admitted: &crate::body_query::OwnedForeignComptimeProgram,
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
        call_ordinal: u32,
    ) -> Result<Self, DurableComptimeLifecycleError> {
        let policy = DurableComptimeApplicationPolicy::apply_at_parent_call(
            parent_declaration.clone(),
            call_ordinal,
        );
        Self::from_admitted_with_policy(admitted, parent_producer, parent_declaration, policy)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn from_admitted_structured(
        admitted: &crate::body_query::OwnedForeignComptimeProgram,
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<Self, DurableComptimeLifecycleError> {
        Self::from_admitted_with_policy(
            admitted,
            parent_producer,
            parent_declaration,
            DurableComptimeApplicationPolicy::preserve(),
        )
    }

    fn from_admitted_with_policy(
        admitted: &crate::body_query::OwnedForeignComptimeProgram,
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
        application_policy: DurableComptimeApplicationPolicy,
    ) -> Result<Self, DurableComptimeLifecycleError> {
        let child_producer = admitted.plan.key.declaration.clone();
        let Some(child_declaration) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&child_producer)
        else {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        };
        if child_declaration != admitted.plan.candidate
            || admitted
                .callable()
                .is_none_or(|callable| callable.context != child_declaration.module)
        {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        let Some(expected_parent) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &parent_producer,
            )
        else {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        };
        if expected_parent != parent_declaration {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        let configuration = admitted.plan.key.configuration.clone();
        let query = crate::semantic_query_nucleus::ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: child_declaration,
                configuration: configuration.clone(),
            },
            type_arguments: admitted.seed.type_arguments.clone(),
            value_arguments: admitted.seed.value_arguments.clone(),
        };
        Ok(Self {
            query,
            parent_producer,
            parent_declaration,
            child_producer: child_producer.clone(),
            program: crate::body_query::DurableComptimeProgramKey {
                declaration: child_producer,
                configuration,
            },
            application_policy,
        })
    }

    #[cfg(test)]
    fn for_test(
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
        child_producer: crate::StableDefinitionKey,
        call_ordinal: u32,
    ) -> Self {
        let policy = DurableComptimeApplicationPolicy::apply_at_parent_call(
            parent_declaration.clone(),
            call_ordinal,
        );
        Self::for_test_with_policy(parent_producer, parent_declaration, child_producer, policy)
    }

    #[cfg(test)]
    fn for_test_structured(
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
        child_producer: crate::StableDefinitionKey,
    ) -> Self {
        Self::for_test_with_policy(
            parent_producer,
            parent_declaration,
            child_producer,
            DurableComptimeApplicationPolicy::preserve(),
        )
    }

    #[cfg(test)]
    fn for_test_with_policy(
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
        child_producer: crate::StableDefinitionKey,
        application_policy: DurableComptimeApplicationPolicy,
    ) -> Self {
        let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: rue_target::Target::X86_64Linux,
            preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
        };
        let child_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&child_producer)
                .unwrap();
        Self {
            query: crate::semantic_query_nucleus::ComptimeCallQueryKey {
                declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration: child_declaration,
                    configuration: configuration.clone(),
                },
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            },
            parent_producer,
            parent_declaration,
            child_producer: child_producer.clone(),
            program: crate::body_query::DurableComptimeProgramKey {
                declaration: child_producer,
                configuration,
            },
            application_policy,
        }
    }
}

/// Non-clone edge capability issued after parent validation and before lookup.
///
/// An edge is the single capability for either side of a ready/admitted
/// lookup.  A ready projection consumes it directly; an admitted program
/// converts it into an entered-call ticket.  Its fields remain private so a
/// host cannot reconstruct policy from an unordered binding map or use an
/// edge for another call.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct DurableComptimeCallEdge {
    owner: u64,
    serial: u64,
    expected_parent: Option<(u64, u64)>,
    parent_producer: crate::StableDefinitionKey,
    parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    application_policy: DurableComptimeApplicationPolicy,
    consumed: bool,
}

impl DurableComptimeCallEdge {
    /// The exact declaration source captured when this edge was issued.
    ///
    /// Future hosts use this opaque identity for lookup visibility and
    /// dependency attribution; they do not reconstruct it from call
    /// bindings or ambient provider state.
    #[allow(dead_code)] // consumed by the root-integrated durable host
    pub(crate) fn accessing_source(&self) -> &crate::StableDefinitionKey {
        &self.parent_producer
    }
}

/// Non-clone lifecycle capability issued only after an edge is admitted.
/// Its fields remain private so a host cannot reconstruct a ticket from an
/// unordered binding map or use a ticket for another call.
#[allow(dead_code)] // opaque capability consumed by the root-integrated AIR host
#[derive(Debug)]
pub(crate) struct DurableComptimeCallTicket {
    owner: u64,
    serial: u64,
    context: DurableComptimeCallContext,
    expected_parent: Option<(u64, u64)>,
    consumed: bool,
}

impl DurableComptimeCallTicket {
    #[allow(dead_code)] // consumed by the root-integrated durable AIR host
    pub(crate) fn canonical_function_producer(
        &self,
        program: &crate::body_query::DurableComptimeProgramKey,
    ) -> Result<crate::StableProducerId, DurableComptimeProducerIssuanceError> {
        self.context.canonical_function_producer(program)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableTicketState {
    Entered,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeLifecycleError {
    TicketMismatch,
    BindingMismatch,
    InvalidProgramAuthority,
    NotEntered,
    OutOfOrder,
    TicketReused,
    InvalidContext,
    ReadyProjectionRequired,
}

#[allow(dead_code)]
static NEXT_DURABLE_LIFECYCLE_ID: AtomicU64 = AtomicU64::new(1);

/// The unchanged result and effects published by one completed durable root.
///
/// Effects are deliberately attached to the exact AIR outcome rather than
/// represented by a compiler-local outcome enum. Only `Known` outcomes carry
/// observations; every other terminal has an empty effects value.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct DurableComptimeCompletion<V, F> {
    outcome: rue_air::ComptimeOutcome<V, F>,
    effects: DurableComptimeEffects,
}

#[allow(dead_code)]
impl<V, F> DurableComptimeCompletion<V, F> {
    pub(crate) fn into_parts(self) -> (rue_air::ComptimeOutcome<V, F>, DurableComptimeEffects) {
        (self.outcome, self.effects)
    }

    #[cfg(test)]
    pub(crate) fn outcome(&self) -> &rue_air::ComptimeOutcome<V, F> {
        &self.outcome
    }

    #[cfg(test)]
    pub(crate) fn effects(&self) -> &DurableComptimeEffects {
        &self.effects
    }

    #[cfg(test)]
    fn deferred_ownership(&self) -> impl Iterator<Item = &DeferredOwnershipGate> {
        self.effects.deferred_ownership()
    }

    #[cfg(test)]
    fn anonymous_nominals(&self) -> impl Iterator<Item = &DurableAnonymousNominal> {
        self.effects.anonymous_nominals()
    }

    #[cfg(test)]
    fn dependencies(&self) -> impl Iterator<Item = &SemanticDeclarationDependency> {
        self.effects.dependencies()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }
}

/// Per-root durable comptime session.
///
/// The AIR frame remains the owner of expression locals, producer identity,
/// and expected-result context. This session owns compiler-side call lifecycle
/// state, the root-local call ordinal allocator, and the one AIR program
/// registry shared by every root and foreign frame in the evaluation.
#[derive(Debug)]
pub(crate) struct DurableComptimeSession {
    lifecycle: DurableComptimeCallLifecycle,
    next_call: u32,
    programs: crate::body_query::DurableComptimeProgramRegistry,
}

/// The result of consuming one pre-lookup foreign-call edge. A ready fact is
/// merged into the lifecycle-owned scope, while an admitted body is returned
/// with the exact ticket that the canonical AIR engine must later enter. The
/// edge cannot be used for both alternatives.
#[allow(dead_code)] // consumed by the root-integrated durable AIR host
#[derive(Debug)]
pub(crate) enum DurableComptimeForeignCall {
    Ready(crate::semantic_query_nucleus::ComptimeCallResultProjection),
    Enter {
        program: crate::body_query::OwnedForeignComptimeProgram,
        ticket: Box<DurableComptimeCallTicket>,
    },
    NotReady,
}

#[allow(dead_code)] // preserves exact lookup/lifecycle errors for the host
#[derive(Debug)]
pub(crate) enum DurableComptimeForeignCallError {
    ReadyFailure(crate::semantic_query_nucleus::SemanticNucleusFailure),
    ReadyQueryFailure(rue_query::QueryFailure),
    AdmissionFailure(crate::body_query::ComptimeProgramProjectionFailure),
    FrameAdmission(DurableComptimeForeignFrameAdmissionError),
    StructuredFrame(DurableComptimeStructuredFrameAdmissionError),
    UnexpectedReadyProjection,
    Lifecycle(DurableComptimeLifecycleError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableComptimeStructuredFrameAdmissionError {
    InvalidContract,
    ValueFit(Box<SemanticNucleusFailure>),
    ResultNotType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum DurableComptimeConstRootAdmissionError {
    NotConstRoot,
    DuplicateProgram,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeProgramFinalizationError {
    MissingProgram,
    AuthorityMismatch,
}

/// Failure to turn a registered program identity and source range into a
/// durable diagnostic site. Unknown keys never fall back to the session's
/// parent provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // consumed by the canonical durable AIR host
pub(crate) enum DurableComptimeDiagnosticSiteError {
    UnknownProgram,
    UnknownDeclaration,
}

/// Failure before a foreign AIR frame is handed to the engine.  Admission is
/// intentionally separate from lifecycle activation: the engine still owns
/// the depth check, `enter`, and cleanup after it receives this frame.
#[allow(dead_code)] // consumed by the canonical durable AIR host
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeForeignFrameAdmissionError {
    NotCallable,
    TicketMismatch,
    RegistryMismatch,
}

/// A completed foreign call after one-shot probing. The ready result retains
/// the bound call's substituted result type so a host cannot reconstruct
/// typed metadata from the raw query projection.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum DurableComptimePreparedCall {
    Ready {
        result: crate::semantic_query_nucleus::ComptimeCallResultProjection,
        expected_result: DurableType,
    },
    Enter {
        frame: Box<DurableComptimeForeignFrame>,
        ticket: Box<DurableComptimeCallTicket>,
    },
    NotReady,
}

impl DurableComptimeSession {
    pub(crate) fn new(
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<Self, DurableComptimeLifecycleError> {
        Ok(Self {
            lifecycle: DurableComptimeCallLifecycle::new(parent_producer, parent_declaration)?,
            next_call: 0,
            programs: crate::body_query::DurableComptimeProgramRegistry::new(),
        })
    }

    /// Return the semantic cycle produced when a fully-keyed call repeats an
    /// active admitted frame.  The key includes declaration, configuration,
    /// and ordered type/value arguments; changing any of those keeps the call
    /// a real recursive specialization and lets AIR enforce its depth limit.
    pub(crate) fn active_comptime_call_cycle(
        &self,
        producer: &crate::StableDefinitionKey,
        configuration: &crate::semantic_query_nucleus::SemanticQueryConfiguration,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Option<SemanticNucleusFailure> {
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(producer)?;
        let query = crate::semantic_query_nucleus::ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: configuration.clone(),
            },
            type_arguments: type_arguments.to_vec().into(),
            value_arguments: value_arguments.to_vec().into(),
        };
        let first = self.lifecycle.active.iter().position(|key| {
            self.lifecycle
                .contexts
                .get(key)
                .is_some_and(|context| context.query == query)
        })?;
        let mut names = self.lifecycle.active[first..]
            .iter()
            .filter_map(|key| self.lifecycle.contexts.get(key))
            .map(|context| context.query.declaration.declaration.name.clone())
            .collect::<Vec<Arc<str>>>();
        names.push(query.declaration.declaration.name.clone());
        Some(SemanticNucleusFailure::Cycle(names.into()))
    }

    pub(super) fn active_pending_call_cycle(
        &self,
        pending: &DurableComptimePendingCall,
    ) -> Option<SemanticNucleusFailure> {
        let query = pending.query_view();
        self.active_comptime_call_cycle(
            pending.producer(),
            query.configuration(),
            query.type_arguments(),
            query.value_arguments(),
        )
    }

    #[allow(dead_code)]
    pub(crate) fn register_program(
        &mut self,
        core: &crate::body_query::OwnedComptimeProgramCore,
    ) -> Result<(), rue_air::ComptimeProgramRegistrationError> {
        core.register_into(&mut self.programs)
    }

    fn structured_program_capability(
        &self,
        key: &crate::body_query::DurableComptimeProgramKey,
    ) -> Option<DurableStructuredTypeProgramCapability> {
        self.programs
            .get(key)
            .map(|_| DurableStructuredTypeProgramCapability::new(key.clone(), self.lifecycle.owner))
    }

    pub(crate) fn structured_type_authority(
        &self,
        key: &crate::body_query::DurableComptimeProgramKey,
        root: rue_rir::RirTypeSyntaxRef,
    ) -> Option<DurableStructuredTypeAuthority> {
        let capability = self.structured_program_capability(key)?;
        self.programs.structured_type_authority_with_program(
            key,
            capability,
            key.declaration.module().clone(),
            root,
        )
    }

    fn structured_program_is_current(
        &self,
        capability: &DurableStructuredTypeProgramCapability,
    ) -> bool {
        capability.owner == self.lifecycle.owner && self.programs.get(capability.key()).is_some()
    }

    /// Snapshot one suspended AIR structured job and issue the preserve-policy
    /// edge for its exact foreign call request. AIR retains ownership of the
    /// job for resume; this package contains only copied request facts.
    #[allow(dead_code)]
    pub(crate) fn prepare_structured_type_call(
        &mut self,
        job: &DurableStructuredTypeJob,
        call_span: rue_span::Span,
    ) -> Result<DurableStructuredTypePendingCall, DurableComptimeLifecycleError> {
        let request = job.request_view();
        if !self.structured_program_is_current(request.program()) {
            return Err(DurableComptimeLifecycleError::InvalidProgramAuthority);
        }
        let request = DurableStructuredTypeRequest {
            program: request.program().key().clone(),
            head_key: request.head().key.clone(),
            parameters: request.head().parameters.clone(),
            returns_type: request.head().returns_type,
            type_arguments: request.type_arguments().to_vec(),
            value_arguments: request.value_arguments().to_vec(),
            call_span,
        };
        let edge = self.lifecycle.prepare_structured_edge()?;
        Ok(DurableStructuredTypePendingCall { request, edge })
    }

    /// Validate the keyed constructor contract before probing its foreign
    /// result.  This is the structured-call equivalent of ordinary argument
    /// binding: all typed frame metadata is derived from the admitted
    /// signature and the ordered AIR request, never from caller-supplied maps.
    #[allow(dead_code)]
    pub(crate) fn validate_structured_type_call(
        &self,
        pending: DurableStructuredTypePendingCall,
        admission: DurableComptimeCallableAdmission,
    ) -> Result<DurableStructuredTypeValidatedCall, DurableComptimeForeignCallError> {
        let request = &pending.request;
        let expected_candidate =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &request.head_key,
            );
        if expected_candidate.as_ref() != Some(&admission.candidate)
            || admission.identity.key != request.head_key
            || admission.configuration != request.program.configuration
            || !request.returns_type
            || admission.result != DurableType::ComptimeType
            || request.parameters.len() != admission.parameters.len()
            || request.parameters.len() != admission.shell_parameters.len()
        {
            return Err(DurableComptimeForeignCallError::StructuredFrame(
                if admission.result != DurableType::ComptimeType || !request.returns_type {
                    DurableComptimeStructuredFrameAdmissionError::ResultNotType
                } else {
                    DurableComptimeStructuredFrameAdmissionError::InvalidContract
                },
            ));
        }

        let type_arguments = request
            .type_arguments
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>();
        let mut type_bindings = AHashMap::new();
        let mut value_bindings = AHashMap::new();
        let mut type_index = 0;
        let mut value_index = 0;
        for ((head, parameter), shell) in request
            .parameters
            .iter()
            .zip(admission.parameters.iter())
            .zip(admission.shell_parameters.iter())
        {
            if head.name != parameter.name
                || head.name != shell.name
                || head.is_type != shell.is_type_parameter
                || head.is_comptime != parameter.is_comptime
                || parameter.mode != crate::durable_semantics::DurableParameterMode::Value
                || shell.mode != crate::declaration_candidate::DeclarationParameterMode::Value
                || shell.is_comptime != parameter.is_comptime
            {
                return Err(DurableComptimeForeignCallError::StructuredFrame(
                    DurableComptimeStructuredFrameAdmissionError::InvalidContract,
                ));
            }
            if head.is_type {
                let Some((name, ty)) = request.type_arguments.get(type_index) else {
                    return Err(DurableComptimeForeignCallError::StructuredFrame(
                        DurableComptimeStructuredFrameAdmissionError::InvalidContract,
                    ));
                };
                if name != &head.name {
                    return Err(DurableComptimeForeignCallError::StructuredFrame(
                        DurableComptimeStructuredFrameAdmissionError::InvalidContract,
                    ));
                }
                type_bindings.insert(
                    DurableComptimeName::from(name.clone()),
                    DurableComptimeType(ty.clone()),
                );
                type_index += 1;
            } else {
                let Some((name, value)) = request.value_arguments.get(value_index) else {
                    return Err(DurableComptimeForeignCallError::StructuredFrame(
                        DurableComptimeStructuredFrameAdmissionError::InvalidContract,
                    ));
                };
                if name != &head.name {
                    return Err(DurableComptimeForeignCallError::StructuredFrame(
                        DurableComptimeStructuredFrameAdmissionError::InvalidContract,
                    ));
                }
                let expected = substitute_durable_generics(&parameter.ty, &type_arguments);
                if let Some(failure) = durable_structured_value_fit_failure(value, &expected) {
                    return Err(DurableComptimeForeignCallError::StructuredFrame(
                        DurableComptimeStructuredFrameAdmissionError::ValueFit(Box::new(failure)),
                    ));
                }
                value_bindings.insert(
                    DurableComptimeName::from(name.clone()),
                    EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        value.clone(),
                        expected,
                    )),
                );
                value_index += 1;
            }
        }
        if type_index != request.type_arguments.len()
            || value_index != request.value_arguments.len()
        {
            return Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::InvalidContract,
            ));
        }
        Ok(DurableStructuredTypeValidatedCall {
            pending,
            admission,
            type_bindings,
            value_bindings,
            expected_result: DurableComptimeType::from(substitute_durable_generics(
                &DurableType::ComptimeType,
                &type_arguments,
            )),
        })
    }

    /// Consume exactly one structured probe. An admitted lookup is registered
    /// only after its complete declaration/configuration key has been checked;
    /// a repeated equivalent registration keeps the existing first authority.
    #[allow(dead_code)]
    pub(crate) fn consume_structured_type_call(
        &mut self,
        probed: DurableStructuredTypeProbedCall,
    ) -> Result<DurableStructuredTypeCall, DurableComptimeForeignCallError> {
        let DurableStructuredTypeProbedCall { pending, lookup } = probed;
        let DurableStructuredTypeValidatedCall {
            pending,
            admission,
            type_bindings,
            value_bindings,
            expected_result,
        } = pending;
        let DurableStructuredTypePendingCall { request, edge } = pending;
        match lookup {
            ForeignComptimeCallLookup::Admitted(program) => {
                // Validate every immutable child fact before consuming the
                // lifecycle edge.  In particular, a const-root payload must
                // not obtain a ticket or enter the registry before the
                // structured callable check below.
                let is_callable = matches!(
                    program.root(),
                    crate::body_query::OwnedComptimeProgramRoot::Callable(_)
                );
                let child_contract_matches = admission.identity.key == request.head_key
                    && admission.configuration == request.program.configuration
                    && admission.result == DurableType::ComptimeType
                    && program.plan.key.configuration == request.program.configuration
                    && program.plan.key.declaration == request.head_key
                    && program.seed.type_arguments.as_ref() == request.type_arguments.as_slice()
                    && program.seed.value_arguments.as_ref() == request.value_arguments.as_slice();
                let registry_matches = self
                    .programs
                    .get(&program.plan.key)
                    .map(|existing| same_registered_program(existing, &program))
                    .unwrap_or(true);
                if !is_callable {
                    return Err(DurableComptimeForeignCallError::FrameAdmission(
                        DurableComptimeForeignFrameAdmissionError::NotCallable,
                    ));
                }
                if !child_contract_matches || !registry_matches {
                    return Err(DurableComptimeForeignCallError::FrameAdmission(
                        DurableComptimeForeignFrameAdmissionError::RegistryMismatch,
                    ));
                }
                let DurableComptimeForeignCall::Enter { program, ticket } = self
                    .consume_foreign_lookup(edge, ForeignComptimeCallLookup::Admitted(program))?
                else {
                    return Err(DurableComptimeForeignCallError::FrameAdmission(
                        DurableComptimeForeignFrameAdmissionError::RegistryMismatch,
                    ));
                };
                if let Some(existing) = self.programs.get(&program.plan.key) {
                    if !same_registered_program(existing, &program) {
                        return Err(DurableComptimeForeignCallError::FrameAdmission(
                            DurableComptimeForeignFrameAdmissionError::RegistryMismatch,
                        ));
                    }
                } else {
                    program
                        .core
                        .register_into(&mut self.programs)
                        .map_err(|_| {
                            DurableComptimeForeignCallError::FrameAdmission(
                                DurableComptimeForeignFrameAdmissionError::RegistryMismatch,
                            )
                        })?;
                }
                let registered = self
                    .programs
                    .get(&program.plan.key)
                    .expect("admitted structured program is registered");
                let callable = match &registered.imports.root {
                    crate::body_query::OwnedComptimeProgramRoot::Callable(callable) => callable,
                    crate::body_query::OwnedComptimeProgramRoot::Const { .. } => unreachable!(
                        "structured callable was prevalidated before registry admission"
                    ),
                };
                let frame = rue_air::ComptimeFrame {
                    program: program.plan.key.clone(),
                    body: callable.body,
                    name: Some(DurableComptimeName::from(
                        program.plan.key.declaration.name(),
                    )),
                    context: Some(self.registered_file(&program.plan.key)),
                    span: request.call_span,
                    function_span: registered.rir.get(callable.root).span,
                    type_bindings,
                    value_bindings,
                    name_bindings: AHashMap::new(),
                    call_identity: None,
                    expected_result: Some(expected_result),
                };
                Ok(DurableStructuredTypeCall::Enter {
                    program,
                    frame: Box::new(frame),
                    ticket,
                })
            }
            ForeignComptimeCallLookup::Ready(projection) => {
                match self
                    .consume_foreign_lookup(edge, ForeignComptimeCallLookup::Ready(projection))?
                {
                    DurableComptimeForeignCall::Ready(result) => {
                        Ok(DurableStructuredTypeCall::Ready { result })
                    }
                    DurableComptimeForeignCall::Enter { .. }
                    | DurableComptimeForeignCall::NotReady => {
                        Err(DurableComptimeForeignCallError::UnexpectedReadyProjection)
                    }
                }
            }
            ForeignComptimeCallLookup::NotReady => Ok(DurableStructuredTypeCall::NotReady),
            ForeignComptimeCallLookup::ReadyFailure(failure) => {
                Err(DurableComptimeForeignCallError::ReadyFailure(failure))
            }
            ForeignComptimeCallLookup::ReadyQueryFailure(failure) => {
                Err(DurableComptimeForeignCallError::ReadyQueryFailure(failure))
            }
            ForeignComptimeCallLookup::AdmissionFailure(failure) => {
                Err(DurableComptimeForeignCallError::AdmissionFailure(failure))
            }
            ForeignComptimeCallLookup::UnexpectedReadyProjection => {
                Err(DurableComptimeForeignCallError::UnexpectedReadyProjection)
            }
        }
    }

    /// Finalize import metadata on the exact program already registered for a
    /// root.  The RIR, symbols, and root authority must be the same immutable
    /// payload that was admitted; only the discovered import index is updated.
    pub(crate) fn finalize_registered_imports(
        &mut self,
        core: &crate::body_query::OwnedComptimeProgramCore,
    ) -> Result<(), DurableComptimeProgramFinalizationError> {
        let key = &core.plan.key;
        let Some(registered) = self.programs.get(key) else {
            return Err(DurableComptimeProgramFinalizationError::MissingProgram);
        };
        if !same_registered_program_authority(registered, core) {
            return Err(DurableComptimeProgramFinalizationError::AuthorityMismatch);
        }
        let Some(metadata) = self.programs.metadata_mut(key) else {
            return Err(DurableComptimeProgramFinalizationError::MissingProgram);
        };
        metadata.imports = core.imports.imports.clone();
        Ok(())
    }

    /// Atomically register and frame one declaration root.  A callable core
    /// is rejected before touching the keyed registry; a duplicate key is
    /// rejected by the registry before a frame escapes.  The caller supplies
    /// the already-canonical expected result, so this boundary never resolves
    /// declared type syntax independently.
    #[allow(dead_code)]
    pub(crate) fn admit_const_root(
        &mut self,
        core: Arc<crate::body_query::OwnedComptimeProgramCore>,
        expected_result: Option<DurableComptimeType>,
    ) -> Result<DurableComptimeConstFrame, DurableComptimeConstRootAdmissionError> {
        let Some((init, _, root)) = core.const_root() else {
            return Err(DurableComptimeConstRootAdmissionError::NotConstRoot);
        };
        core.register_into(&mut self.programs)
            .map_err(|_| DurableComptimeConstRootAdmissionError::DuplicateProgram)?;
        let context = self.registered_file(&core.plan.key);
        Ok(rue_air::ComptimeFrame {
            program: core.plan.key.clone(),
            body: init,
            name: None,
            context: Some(context),
            span: core.rir.get(init).span,
            function_span: core.rir.get(root).span,
            type_bindings: AHashMap::new(),
            value_bindings: AHashMap::new(),
            name_bindings: AHashMap::new(),
            call_identity: None,
            expected_result,
        })
    }

    /// Reserve one call ordinal and seal it to this session. Failed admission
    /// or binding still consumes the reservation, matching the established
    /// admission timing.
    #[allow(dead_code)]
    pub(crate) fn reserve_bound_expression_call(&mut self) -> DurableComptimeCallReservation {
        let ordinal = self.next_call;
        self.next_call += 1;
        DurableComptimeCallReservation {
            token: DurableComptimeCallToken::new(self.lifecycle.owner, ordinal),
        }
    }

    /// Pair an already-admitted callable with one reservation before its
    /// arguments are evaluated. Both token identity and ordinal are owned by
    /// this session; callers cannot mint a token or reuse a consumed wrapper.
    #[allow(dead_code)]
    pub(crate) fn admit_bound_expression_call(
        &mut self,
        reservation: DurableComptimeCallReservation,
        admission: DurableComptimeCallableAdmission,
    ) -> Result<DurableComptimeAdmittedCall, DurableComptimeLifecycleError> {
        if reservation.token.identity.session != self.lifecycle.owner
            || reservation.token.identity.ordinal >= self.next_call
        {
            return Err(DurableComptimeLifecycleError::TicketMismatch);
        }
        Ok(DurableComptimeAdmittedCall::new(
            reservation.token,
            admission,
        ))
    }

    /// Consume one admitted binding and issue the exact lifecycle edge that
    /// will own its probe and eventual completion. The producer comes from
    /// the admission identity; callers cannot provide an independent query
    /// key, edge, or unordered argument map.
    #[allow(dead_code)]
    pub(crate) fn prepare_bound_expression_call(
        &mut self,
        admitted: DurableComptimeAdmittedCall,
        bound: DurableComptimeBoundCall,
    ) -> Result<DurableComptimePendingCall, DurableComptimeLifecycleError> {
        let admission_stamp = DurableComptimeAdmissionStamp::from_admission(&admitted.admission);
        if !admitted.token.handle().same(&bound.token) || admission_stamp != bound.admission {
            return Err(DurableComptimeLifecycleError::BindingMismatch);
        }
        let producer = admitted.admission.identity.key.clone();
        let program = crate::body_query::DurableComptimeProgramKey {
            declaration: producer.clone(),
            configuration: admitted.admission.configuration.clone(),
        };
        let edge = self.prepare_expression_edge(bound.token.ordinal())?;
        Ok(DurableComptimePendingCall {
            edge,
            producer,
            program,
            token: admitted.token.handle(),
            bound,
        })
    }

    /// Read one already-registered program by its complete stable key. Dense
    /// instruction references remain meaningful only through the returned
    /// owning program, so callers cannot accidentally pair a reference with a
    /// colliding program.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn registered_program(
        &self,
        key: &crate::body_query::DurableComptimeProgramKey,
    ) -> Option<&crate::body_query::DurableComptimeProgram> {
        self.programs.get(key)
    }

    /// Issue the AIR file capability only for a program retained by this
    /// session's keyed registry. Unknown keys cannot acquire a file domain.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn file_for_program(
        &self,
        key: &crate::body_query::DurableComptimeProgramKey,
    ) -> Result<DurableComptimeFile, DurableComptimeDiagnosticSiteError> {
        if self.programs.get(key).is_none() {
            return Err(DurableComptimeDiagnosticSiteError::UnknownProgram);
        }
        Ok(self.registered_file(key))
    }

    fn registered_file(
        &self,
        key: &crate::body_query::DurableComptimeProgramKey,
    ) -> DurableComptimeFile {
        assert!(
            self.programs.get(key).is_some(),
            "registered file capability requires a retained program"
        );
        DurableComptimeFile::new(key.clone())
    }

    /// Build the canonical AIR import site from one registered program.  The
    /// raw instruction is accepted only at this compiler-side adapter;
    /// lookup, occurrence, span, and program identity are paired here from
    /// the same registry entry before a semantic site escapes.
    #[cfg(test)]
    pub(crate) fn import_site_for_instruction(
        &self,
        key: &crate::body_query::DurableComptimeProgramKey,
        instruction: rue_rir::InstRef,
        specifier: &str,
    ) -> Result<
        rue_air::ComptimeSite<crate::body_query::DurableComptimeProgramKey>,
        DurableComptimeKeyedImportError,
    > {
        let Some(program) = self.programs.get(key) else {
            return Err(DurableComptimeKeyedImportError::UnknownProgram);
        };
        let Some(occurrence) = program
            .imports
            .imports
            .iter()
            .find(|occurrence| occurrence.inst == instruction)
        else {
            return Err(DurableComptimeKeyedImportError::UnknownInstruction);
        };
        if occurrence.specifier.as_ref() != specifier {
            return Err(DurableComptimeKeyedImportError::SpecifierMismatch);
        }
        let span = program.rir.get(instruction).span;
        Ok(rue_air::ComptimeSite::from_import_occurrence(
            key.clone(),
            occurrence.occurrence,
            span,
        ))
    }

    /// Resolve an engine-created diagnostic range against the exact
    /// registered program key, without observing effects or query state.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn diagnostic_site(
        &self,
        key: &crate::body_query::DurableComptimeProgramKey,
        span: rue_span::Span,
    ) -> Result<DurableComptimeDiagnosticSite, DurableComptimeDiagnosticSiteError> {
        if self.programs.get(key).is_none() {
            return Err(DurableComptimeDiagnosticSiteError::UnknownProgram);
        }
        let producer = crate::revisioned_query_database::declaration_candidate_for_stable_key(
            &key.declaration,
        )
        .ok_or(DurableComptimeDiagnosticSiteError::UnknownDeclaration)?;
        Ok(DurableComptimeDiagnosticSite::new(
            producer, span.start, span.end,
        ))
    }

    /// Atomically admit an already-prepared foreign callable into the keyed
    /// AIR program registry and construct its frame.  The ticket is only a
    /// capability here: this method validates its exact call identity but
    /// never enters a lifecycle scope.  The canonical AIR engine performs
    /// depth admission and calls `enter` after producer issuance.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn admit_foreign_frame(
        &mut self,
        admitted: crate::body_query::OwnedForeignComptimeProgram,
        ticket: Box<DurableComptimeCallTicket>,
        call_span: rue_span::Span,
        bound: DurableComptimeBoundCall,
    ) -> Result<
        (DurableComptimeForeignFrame, Box<DurableComptimeCallTicket>),
        DurableComptimeForeignFrameAdmissionError,
    > {
        let Some(callable) = admitted.callable() else {
            return Err(DurableComptimeForeignFrameAdmissionError::NotCallable);
        };
        if bound.admission.candidate != admitted.plan.candidate
            || bound.admission.identity.key != admitted.plan.key.declaration
            || bound.admission.configuration != admitted.plan.key.configuration
        {
            return Err(DurableComptimeForeignFrameAdmissionError::TicketMismatch);
        }
        let key = &admitted.plan.key;
        let context = &ticket.context;
        let ticket_matches = ticket.owner == self.lifecycle.owner
            && !ticket.consumed
            && ticket.serial < self.lifecycle.next_serial
            && self.lifecycle.active.last().copied() == ticket.expected_parent
            && !self
                .lifecycle
                .states
                .contains_key(&(ticket.owner, ticket.serial))
            && context.program == *key
            && context.child_producer == key.declaration
            && context.query.declaration.configuration == key.configuration
            && context.query.declaration.declaration == admitted.plan.candidate
            && context.query.type_arguments == admitted.seed.type_arguments
            && context.query.value_arguments == admitted.seed.value_arguments
            && callable.context == admitted.plan.candidate.module;
        if !ticket_matches {
            return Err(DurableComptimeForeignFrameAdmissionError::TicketMismatch);
        }

        if bound.type_arguments.as_slice() != admitted.seed.type_arguments.as_ref()
            || bound.value_arguments.as_slice() != admitted.seed.value_arguments.as_ref()
            || bound.typed_value_arguments.len() != admitted.seed.value_arguments.len()
            || !bound
                .typed_value_arguments
                .iter()
                .zip(admitted.seed.value_arguments.iter())
                .all(|((bound_name, bound_value), (seed_name, seed))| {
                    bound_name == seed_name
                        && matches!(
                            bound_value,
                            EvaluatedSemanticConst::Value(value)
                                if value.value == *seed && value.ty.is_some()
                        )
                })
        {
            return Err(DurableComptimeForeignFrameAdmissionError::TicketMismatch);
        }

        // Registry keys are first-wins.  A repeated admission is valid only
        // when it carries the same immutable symbol/import/root authority; a
        // colliding authority can never replace the first registration.
        if let Some(existing) = self.programs.get(key) {
            if !same_registered_program(existing, &admitted) {
                return Err(DurableComptimeForeignFrameAdmissionError::RegistryMismatch);
            }
        } else if admitted.core.register_into(&mut self.programs).is_err() {
            return Err(DurableComptimeForeignFrameAdmissionError::RegistryMismatch);
        }
        let Some(registered) = self.programs.get(key) else {
            return Err(DurableComptimeForeignFrameAdmissionError::RegistryMismatch);
        };
        let context = self.registered_file(key);
        let crate::body_query::OwnedComptimeProgramRoot::Callable(callable) =
            &registered.imports.root
        else {
            return Err(DurableComptimeForeignFrameAdmissionError::NotCallable);
        };

        let mut type_bindings = AHashMap::new();
        for (name, ty) in bound.type_arguments.iter() {
            type_bindings.insert(DurableComptimeName::from(name.clone()), ty.clone().into());
        }
        let mut value_bindings = AHashMap::new();
        for (name, value) in bound.typed_value_arguments.iter() {
            value_bindings.insert(DurableComptimeName::from(name.clone()), value.clone());
        }
        Ok((
            rue_air::ComptimeFrame {
                program: key.clone(),
                body: callable.body,
                name: Some(DurableComptimeName::from(key.declaration.name())),
                context: Some(context),
                span: call_span,
                function_span: registered.rir.get(callable.root).span,
                type_bindings,
                value_bindings,
                name_bindings: AHashMap::new(),
                call_identity: None,
                expected_result: Some(bound.expected_result.into()),
            },
            ticket,
        ))
    }

    pub(super) fn observe_anonymous_nominal(&mut self, nominal: DurableAnonymousNominal) {
        self.lifecycle.observe_anonymous_nominal(nominal);
    }

    /// Issue the expression edge for an already-known call projection. The
    /// lifecycle owns the edge policy and retains its root scope until the
    /// evaluator has fully unwound.
    pub(crate) fn prepare_expression_edge(
        &mut self,
        call_ordinal: u32,
    ) -> Result<DurableComptimeCallEdge, DurableComptimeLifecycleError> {
        self.lifecycle.prepare_expression_edge(call_ordinal)
    }

    #[cfg(test)]
    pub(crate) fn finish_ready_expression_edge(
        &mut self,
        edge: DurableComptimeCallEdge,
        projection: crate::semantic_query_nucleus::ComptimeCallProjection,
    ) -> Result<
        crate::semantic_query_nucleus::ComptimeCallResultProjection,
        DurableComptimeLifecycleError,
    > {
        self.consume_foreign_lookup(edge, ForeignComptimeCallLookup::Ready(projection))
            .map(|result| match result {
                DurableComptimeForeignCall::Ready(result) => result,
                DurableComptimeForeignCall::Enter { .. } | DurableComptimeForeignCall::NotReady => {
                    unreachable!("finish_ready_expression_edge supplies a ready projection")
                }
            })
            .map_err(|error| match error {
                DurableComptimeForeignCallError::Lifecycle(error) => error,
                DurableComptimeForeignCallError::ReadyFailure(_)
                | DurableComptimeForeignCallError::ReadyQueryFailure(_)
                | DurableComptimeForeignCallError::AdmissionFailure(_)
                | DurableComptimeForeignCallError::FrameAdmission(_)
                | DurableComptimeForeignCallError::StructuredFrame(_)
                | DurableComptimeForeignCallError::UnexpectedReadyProjection => {
                    unreachable!("finish_ready_expression_edge supplies a ready projection")
                }
            })
    }

    /// Consume the one lookup result associated with a pre-lookup edge. This
    /// is the compiler-side adapter for the RUE-1795 seam: it never evaluates
    /// a child or demands a terminal. The canonical AIR engine converts the
    /// returned admitted program and exact ticket into its normal call path.
    pub(crate) fn consume_foreign_lookup(
        &mut self,
        edge: DurableComptimeCallEdge,
        lookup: ForeignComptimeCallLookup,
    ) -> Result<DurableComptimeForeignCall, DurableComptimeForeignCallError> {
        let mut edge = edge;
        match lookup {
            ForeignComptimeCallLookup::Ready(projection) => {
                let result = self
                    .lifecycle
                    .merge_ready_projection_owned(&mut edge, projection)
                    .map_err(DurableComptimeForeignCallError::Lifecycle)?;
                Ok(DurableComptimeForeignCall::Ready(result))
            }
            ForeignComptimeCallLookup::Admitted(program) => {
                let ticket = self
                    .lifecycle
                    .ticket_from_admitted_edge(edge, &program)
                    .map_err(DurableComptimeForeignCallError::Lifecycle)?;
                Ok(DurableComptimeForeignCall::Enter {
                    program,
                    ticket: Box::new(ticket),
                })
            }
            ForeignComptimeCallLookup::NotReady => Ok(DurableComptimeForeignCall::NotReady),
            ForeignComptimeCallLookup::ReadyFailure(failure) => {
                Err(DurableComptimeForeignCallError::ReadyFailure(failure))
            }
            ForeignComptimeCallLookup::ReadyQueryFailure(failure) => {
                Err(DurableComptimeForeignCallError::ReadyQueryFailure(failure))
            }
            ForeignComptimeCallLookup::AdmissionFailure(failure) => {
                Err(DurableComptimeForeignCallError::AdmissionFailure(failure))
            }
            ForeignComptimeCallLookup::UnexpectedReadyProjection => {
                Err(DurableComptimeForeignCallError::UnexpectedReadyProjection)
            }
        }
    }

    /// Consume a one-shot probed call. Ready projections publish through the
    /// exact edge once; admitted programs are framed immediately with the
    /// same bound payload; all other terminals discard the package without
    /// retrying, entering, or publishing effects.
    #[allow(dead_code)]
    pub(crate) fn consume_probed_call(
        &mut self,
        probed: DurableComptimeProbedCall,
        call_span: rue_span::Span,
    ) -> Result<DurableComptimePreparedCall, DurableComptimeForeignCallError> {
        let DurableComptimeProbedCall { pending, lookup } = probed;
        let DurableComptimePendingCall {
            edge,
            producer,
            program: pending_program,
            token,
            bound,
        } = pending;
        if !token.same(&bound.token) {
            return Err(DurableComptimeForeignCallError::Lifecycle(
                DurableComptimeLifecycleError::BindingMismatch,
            ));
        }
        match lookup {
            ForeignComptimeCallLookup::Ready(projection) => {
                let expected_result = bound.expected_result.clone();
                let mut edge = edge;
                let result = self
                    .lifecycle
                    .merge_ready_projection_owned(&mut edge, projection)
                    .map_err(DurableComptimeForeignCallError::Lifecycle)?;
                Ok(DurableComptimePreparedCall::Ready {
                    result,
                    expected_result,
                })
            }
            ForeignComptimeCallLookup::Admitted(program) => {
                if program.plan.key != pending_program || program.plan.key.declaration != producer {
                    return Err(DurableComptimeForeignCallError::FrameAdmission(
                        DurableComptimeForeignFrameAdmissionError::RegistryMismatch,
                    ));
                }
                let ticket = self
                    .lifecycle
                    .ticket_from_admitted_edge(edge, &program)
                    .map_err(DurableComptimeForeignCallError::Lifecycle)?;
                let (frame, ticket) = self
                    .admit_foreign_frame(program, Box::new(ticket), call_span, bound)
                    .map_err(DurableComptimeForeignCallError::FrameAdmission)?;
                Ok(DurableComptimePreparedCall::Enter {
                    frame: Box::new(frame),
                    ticket,
                })
            }
            ForeignComptimeCallLookup::NotReady => Ok(DurableComptimePreparedCall::NotReady),
            ForeignComptimeCallLookup::ReadyFailure(failure) => {
                Err(DurableComptimeForeignCallError::ReadyFailure(failure))
            }
            ForeignComptimeCallLookup::ReadyQueryFailure(failure) => {
                Err(DurableComptimeForeignCallError::ReadyQueryFailure(failure))
            }
            ForeignComptimeCallLookup::AdmissionFailure(failure) => {
                Err(DurableComptimeForeignCallError::AdmissionFailure(failure))
            }
            ForeignComptimeCallLookup::UnexpectedReadyProjection => {
                Err(DurableComptimeForeignCallError::UnexpectedReadyProjection)
            }
        }
    }

    /// Drain observations only after the evaluator has fully unwound. The
    /// lifecycle validates that no entered frame remains before mutating its
    /// root effects, so a premature drain is recoverable and non-destructive.
    pub(crate) fn drain_root_effects(
        &mut self,
    ) -> Result<DurableComptimeEffects, DurableComptimeLifecycleError> {
        self.lifecycle.take_root_effects()
    }

    /// Record a semantic dependency in the current lifecycle scope.  Service
    /// callers use this funnel between keyed admission's begin and finish
    /// phases so a later admission failure cannot erase the begin observation.
    #[allow(dead_code)] // consumed by the canonical structured-frame adapter
    pub(crate) fn observe_dependency(&mut self, dependency: SemanticDeclarationDependency) {
        self.lifecycle.observe_dependency(dependency);
    }

    pub(crate) fn observe_deferred_ownership(&mut self, gate: DeferredOwnershipGate) {
        self.lifecycle.observe_deferred_ownership(gate);
    }

    /// Enter one lifecycle ticket through the session-owned funnel.  Keeping
    /// this operation here prevents hosts from reaching around the
    /// session and mutating the lifecycle with a mismatched ticket.
    #[allow(dead_code)] // activated by the canonical durable call lifecycle
    pub(crate) fn enter_call(
        &mut self,
        ticket: &DurableComptimeCallTicket,
    ) -> Result<(), DurableComptimeLifecycleError> {
        self.lifecycle.enter(ticket)
    }

    /// Finish one entered call through the session-owned funnel.  Lifecycle
    /// validation, cleanup, and Known-only effect publication remain a single
    /// operation owned by the session.
    #[allow(dead_code)] // activated by the canonical durable call lifecycle
    pub(crate) fn finish_call<V, F>(
        &mut self,
        ticket: &mut DurableComptimeCallTicket,
        outcome: &rue_air::ComptimeOutcome<V, F>,
    ) -> Result<(), DurableComptimeLifecycleError> {
        self.lifecycle.finish(ticket, outcome)
    }

    #[cfg(test)]
    fn lifecycle_mut(&mut self) -> &mut DurableComptimeCallLifecycle {
        &mut self.lifecycle
    }

    #[cfg(test)]
    pub(super) fn next_call_ordinal_for_test(&self) -> u32 {
        self.next_call
    }

    #[cfg(test)]
    pub(super) fn active_call_count_for_test(&self) -> usize {
        self.lifecycle.active.len()
    }

    #[cfg(test)]
    pub(super) fn active_calls_for_test(&self) -> Vec<(u64, u64)> {
        self.lifecycle.active.clone()
    }

    #[cfg(test)]
    pub(super) fn next_lifecycle_serial_for_test(&self) -> u64 {
        self.lifecycle.next_serial
    }

    #[cfg(test)]
    pub(super) fn root_effects_are_empty_for_test(&self) -> bool {
        self.lifecycle.effects.is_empty()
    }

    #[cfg(test)]
    pub(super) fn program_count_for_test(&self) -> usize {
        self.programs.len()
    }

    #[cfg(test)]
    pub(super) fn ticket_from_admitted_edge_for_test(
        &mut self,
        edge: DurableComptimeCallEdge,
        admitted: &crate::body_query::OwnedForeignComptimeProgram,
    ) -> Result<DurableComptimeCallTicket, DurableComptimeLifecycleError> {
        self.lifecycle.ticket_from_admitted_edge(edge, admitted)
    }
}

/// Compare the immutable metadata retained by the keyed registry, rather than
/// allocation identity. Body-plan materialization can produce a fresh
/// equivalent `Arc`; the first registered RIR remains authoritative for the
/// returned frame and a different root/symbol/import authority is rejected.
fn same_registered_program(
    existing: &crate::body_query::DurableComptimeProgram,
    admitted: &crate::body_query::OwnedForeignComptimeProgram,
) -> bool {
    existing.symbols == admitted.symbols
        && existing.imports.imports == admitted.imports.imports
        && &existing.imports.root == admitted.root()
}

fn same_registered_program_authority(
    existing: &crate::body_query::DurableComptimeProgram,
    core: &crate::body_query::OwnedComptimeProgramCore,
) -> bool {
    std::sync::Arc::ptr_eq(&existing.rir, &core.rir)
        && std::sync::Arc::ptr_eq(&existing.symbols, &core.symbols)
        && existing.imports.root == *core.root()
}

/// Root-local call/effect authority for a durable comptime host.
///
/// `finish` consumes an entered ticket and its lifecycle-owned scope. Cleanup
/// happens for every AIR terminal, while effects publish only for a known
/// result, without copying AIR's outcome algebra into the compiler.
#[allow(dead_code)] // AIR owns the active root lifecycle
#[derive(Debug)]
pub(crate) struct DurableComptimeCallLifecycle {
    owner: u64,
    next_serial: u64,
    parent_producer: crate::StableDefinitionKey,
    parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    active: Vec<(u64, u64)>,
    states: BTreeMap<(u64, u64), DurableTicketState>,
    contexts: BTreeMap<(u64, u64), DurableComptimeCallContext>,
    scopes: BTreeMap<(u64, u64), DurableComptimeEffects>,
    effects: DurableComptimeEffects,
}

#[allow(dead_code)]
impl DurableComptimeCallLifecycle {
    pub(crate) fn new(
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<Self, DurableComptimeLifecycleError> {
        let Some(expected_parent) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &parent_producer,
            )
        else {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        };
        if expected_parent != parent_declaration {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        Ok(Self {
            owner: NEXT_DURABLE_LIFECYCLE_ID.fetch_add(1, Ordering::Relaxed),
            next_serial: 0,
            parent_producer,
            parent_declaration,
            active: Vec::new(),
            states: BTreeMap::new(),
            contexts: BTreeMap::new(),
            scopes: BTreeMap::new(),
            effects: DurableComptimeEffects::default(),
        })
    }

    #[cfg(test)]
    pub(crate) fn prepare(
        &mut self,
        context: DurableComptimeCallContext,
    ) -> Result<DurableComptimeCallTicket, DurableComptimeLifecycleError> {
        let expected_parent = self.active.last().copied();
        let (expected_producer, expected_declaration) = self.current_parent_identity();
        if context.parent_producer != expected_producer
            || context.parent_declaration != expected_declaration
        {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        let serial = self.next_serial;
        self.next_serial = self.next_serial.saturating_add(1);
        Ok(DurableComptimeCallTicket {
            owner: self.owner,
            serial,
            context,
            expected_parent,
            consumed: false,
        })
    }

    /// Issue one validated edge for either a ready projection or an admitted
    /// foreign program.  No lifecycle scope is created until an admitted edge
    /// is entered.
    pub(crate) fn prepare_expression_edge(
        &mut self,
        call_ordinal: u32,
    ) -> Result<DurableComptimeCallEdge, DurableComptimeLifecycleError> {
        let (_, parent_declaration) = self.current_parent_identity();
        self.prepare_edge_with_policy(DurableComptimeApplicationPolicy::apply_at_parent_call(
            parent_declaration,
            call_ordinal,
        ))
    }

    pub(crate) fn prepare_structured_edge(
        &mut self,
    ) -> Result<DurableComptimeCallEdge, DurableComptimeLifecycleError> {
        self.prepare_edge_with_policy(DurableComptimeApplicationPolicy::preserve())
    }

    fn prepare_edge_with_policy(
        &mut self,
        application_policy: DurableComptimeApplicationPolicy,
    ) -> Result<DurableComptimeCallEdge, DurableComptimeLifecycleError> {
        let expected_parent = self.active.last().copied();
        let (parent_producer, parent_declaration) = self.current_parent_identity();
        let serial = self.next_serial;
        self.next_serial = self.next_serial.saturating_add(1);
        Ok(DurableComptimeCallEdge {
            owner: self.owner,
            serial,
            expected_parent,
            parent_producer,
            parent_declaration,
            application_policy,
            consumed: false,
        })
    }

    fn current_parent_identity(
        &self,
    ) -> (
        crate::StableDefinitionKey,
        crate::declaration_candidate::DeclarationCandidateKey,
    ) {
        self.active
            .last()
            .and_then(|key| self.contexts.get(key))
            .map(|context| {
                (
                    context.child_producer.clone(),
                    context.query.declaration.declaration.clone(),
                )
            })
            .unwrap_or_else(|| {
                (
                    self.parent_producer.clone(),
                    self.parent_declaration.clone(),
                )
            })
    }

    fn current_effects_mut(&mut self) -> &mut DurableComptimeEffects {
        if let Some(key) = self.active.last().copied() {
            self.scopes
                .get_mut(&key)
                .expect("active call must retain its effect scope")
        } else {
            &mut self.effects
        }
    }

    fn take_root_effects(
        &mut self,
    ) -> Result<DurableComptimeEffects, DurableComptimeLifecycleError> {
        if !self.active.is_empty() {
            return Err(DurableComptimeLifecycleError::OutOfOrder);
        }
        Ok(std::mem::take(&mut self.effects))
    }

    pub(crate) fn observe_dependency(&mut self, dependency: SemanticDeclarationDependency) {
        self.current_effects_mut().observe_dependency(dependency);
    }

    pub(crate) fn observe_anonymous_nominal(&mut self, nominal: DurableAnonymousNominal) {
        self.current_effects_mut()
            .observe_anonymous_nominal(nominal);
    }

    pub(crate) fn observe_deferred_ownership(&mut self, gate: DeferredOwnershipGate) {
        self.current_effects_mut().observe_deferred_ownership(gate);
    }

    /// Consume an edge on the admitted-program branch and derive the exact
    /// query context from the owned program. Admission deliberately does not
    /// activate a scope; `enter` remains the sole activation point.
    pub(crate) fn ticket_from_admitted_edge(
        &mut self,
        edge: DurableComptimeCallEdge,
        admitted: &crate::body_query::OwnedForeignComptimeProgram,
    ) -> Result<DurableComptimeCallTicket, DurableComptimeLifecycleError> {
        if edge.consumed {
            return Err(DurableComptimeLifecycleError::TicketReused);
        }
        if edge.owner != self.owner || edge.serial >= self.next_serial {
            return Err(DurableComptimeLifecycleError::TicketMismatch);
        }
        if self.active.last().copied() != edge.expected_parent {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        let context = DurableComptimeCallContext::from_admitted_with_policy(
            admitted,
            edge.parent_producer.clone(),
            edge.parent_declaration.clone(),
            edge.application_policy.clone(),
        )?;
        Ok(DurableComptimeCallTicket {
            owner: edge.owner,
            serial: edge.serial,
            context,
            expected_parent: edge.expected_parent,
            consumed: false,
        })
    }

    pub(crate) fn enter(
        &mut self,
        ticket: &DurableComptimeCallTicket,
    ) -> Result<(), DurableComptimeLifecycleError> {
        let key = (ticket.owner, ticket.serial);
        if ticket.owner != self.owner {
            return Err(DurableComptimeLifecycleError::TicketMismatch);
        }
        if ticket.consumed {
            return Err(DurableComptimeLifecycleError::TicketReused);
        }
        if ticket.serial >= self.next_serial {
            return Err(DurableComptimeLifecycleError::TicketMismatch);
        }
        match self.states.get(&key).copied() {
            None => {
                if self.active.last().copied() != ticket.expected_parent {
                    return Err(DurableComptimeLifecycleError::InvalidContext);
                }
                self.states.insert(key, DurableTicketState::Entered);
                self.contexts.insert(key, ticket.context.clone());
                self.scopes.insert(key, DurableComptimeEffects::default());
                self.active.push(key);
                Ok(())
            }
            Some(DurableTicketState::Entered) => Err(DurableComptimeLifecycleError::TicketReused),
        }
    }

    /// Merge a ready foreign-call projection without manufacturing a ticket.
    ///
    /// A ready projection is already a Known result, so it has no entered
    /// child scope to finish. It still crosses the same explicit edge policy
    /// as an entered call: first retain the projection's observations with
    /// `Preserve`, then apply the edge policy as it enters the active parent
    /// scope (or the root when there is no active call).
    pub(crate) fn merge_ready_projection(
        &mut self,
        edge: &mut DurableComptimeCallEdge,
        projection: &crate::semantic_query_nucleus::ComptimeCallProjection,
    ) -> Result<(), DurableComptimeLifecycleError> {
        self.validate_ready_edge(edge)?;
        let mut ready = DurableComptimeEffects::default();
        let preserve = DurableComptimeApplicationPolicy::preserve();
        ready.merge_projection(
            &projection.anonymous_nominals,
            &projection.dependencies,
            &projection.deferred_ownership,
            &preserve,
        );
        edge.consumed = true;
        self.current_effects_mut()
            .merge_child(ready, &edge.application_policy);
        Ok(())
    }

    pub(crate) fn merge_ready_projection_owned(
        &mut self,
        edge: &mut DurableComptimeCallEdge,
        projection: crate::semantic_query_nucleus::ComptimeCallProjection,
    ) -> Result<
        crate::semantic_query_nucleus::ComptimeCallResultProjection,
        DurableComptimeLifecycleError,
    > {
        self.merge_ready_projection(edge, &projection)?;
        Ok(projection.result)
    }

    /// Consume a foreign-call lookup only when it contains a ready Known
    /// projection. Admission failures, misses, and query failures cannot
    /// publish effects through this path.
    pub(crate) fn merge_ready_lookup(
        &mut self,
        edge: &mut DurableComptimeCallEdge,
        lookup: ForeignComptimeCallLookup,
    ) -> Result<(), DurableComptimeLifecycleError> {
        let ForeignComptimeCallLookup::Ready(projection) = lookup else {
            return Err(DurableComptimeLifecycleError::ReadyProjectionRequired);
        };
        self.merge_ready_projection(edge, &projection)
    }

    fn validate_ready_edge(
        &self,
        edge: &DurableComptimeCallEdge,
    ) -> Result<(), DurableComptimeLifecycleError> {
        if edge.owner != self.owner || edge.serial >= self.next_serial {
            return Err(DurableComptimeLifecycleError::TicketMismatch);
        }
        if edge.consumed {
            return Err(DurableComptimeLifecycleError::TicketReused);
        }
        if self.active.last().copied() != edge.expected_parent {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        }
        if self.states.contains_key(&(edge.owner, edge.serial)) {
            return Err(DurableComptimeLifecycleError::TicketReused);
        }
        Ok(())
    }

    pub(crate) fn validate_finish(
        &self,
        ticket: &DurableComptimeCallTicket,
    ) -> Result<(), DurableComptimeLifecycleError> {
        let key = (ticket.owner, ticket.serial);
        if ticket.owner != self.owner {
            return Err(DurableComptimeLifecycleError::TicketMismatch);
        }
        if ticket.consumed {
            return Err(DurableComptimeLifecycleError::TicketReused);
        }
        match self.states.get(&key).copied() {
            Some(DurableTicketState::Entered) => {}
            None => return Err(DurableComptimeLifecycleError::NotEntered),
        }
        if self.active.last().copied() != Some(key) {
            return Err(DurableComptimeLifecycleError::OutOfOrder);
        }
        Ok(())
    }

    pub(crate) fn finish<V, F>(
        &mut self,
        ticket: &mut DurableComptimeCallTicket,
        outcome: &rue_air::ComptimeOutcome<V, F>,
    ) -> Result<(), DurableComptimeLifecycleError> {
        self.validate_finish(ticket)?;
        let key = (ticket.owner, ticket.serial);
        ticket.consumed = true;
        self.active.pop();
        self.states.remove(&key);
        let context = self
            .contexts
            .remove(&key)
            .expect("entered ticket must retain its context");
        let scope = self
            .scopes
            .remove(&key)
            .expect("entered ticket must retain its effect scope");
        if matches!(outcome, rue_air::ComptimeOutcome::Known(_)) {
            // First retain all direct observations alongside effects from
            // completed nested calls. The current call's policy is applied
            // only when this complete scope crosses into its parent/root.
            if let Some(parent) = self.active.last().copied() {
                self.scopes
                    .get_mut(&parent)
                    .expect("active parent must retain its effect scope")
                    .merge_child(scope, &context.application_policy);
            } else {
                self.effects.merge_child(scope, &context.application_policy);
            }
        }
        Ok(())
    }

    pub(crate) fn complete_root<V, F>(
        self,
        outcome: rue_air::ComptimeOutcome<V, F>,
    ) -> Result<
        DurableComptimeCompletion<V, F>,
        (
            Self,
            rue_air::ComptimeOutcome<V, F>,
            DurableComptimeLifecycleError,
        ),
    > {
        if !self.active.is_empty() {
            return Err((self, outcome, DurableComptimeLifecycleError::OutOfOrder));
        }
        let effects = if matches!(outcome, rue_air::ComptimeOutcome::Known(_)) {
            self.effects
        } else {
            DurableComptimeEffects::default()
        };
        Ok(DurableComptimeCompletion { outcome, effects })
    }
}

/// The exact semantic site consumed by the durable import authority.
///
/// This intentionally carries the declaration identity, semantic occurrence,
/// and decoded specifier only. It cannot name or evaluate an RIR instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableImportSite {
    pub(crate) declaration: DeclarationCandidateKey,
    pub(crate) occurrence: u32,
    pub(crate) specifier: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableImportResolution {
    Resolved(ModuleId),
    Missing,
    Failure(DeclarationImportFailure),
}

/// Failure while pairing an engine import instruction with the occurrence
/// owned by its exact registered program.  The provider/query abort variant
/// remains separate so the evaluator cannot turn cancellation into a
/// declaration-time diagnostic.
#[derive(Debug)]
pub(crate) enum DurableComptimeKeyedImportError {
    UnknownProgram,
    UnknownInstruction,
    WrongSiteKind,
    SpecifierMismatch,
    UnknownDeclaration,
    ProviderAbort(QueryAbort),
}

/// The identity and dependency facts established before signature admission.
///
/// Callers observe `dependency` immediately after this phase succeeds, before
/// any signature, shell, arity, or mode work can fail.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DurableComptimeCallableAdmissionStart {
    pub(crate) candidate: DeclarationCandidateKey,
    pub(crate) identity: crate::semantic_query_nucleus::DeclarationIdentityProjection,
    pub(crate) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    pub(crate) name: Arc<str>,
    pub(crate) dependency: SemanticDeclarationDependency,
}

/// The immutable, ordered facts admitted for one durable comptime callable.
///
/// The projection contains both the keyed signature and the declaration-shell
/// headers because argument binding must preserve their canonical order and
/// names. It deliberately carries no RIR handles or evaluation callback; the
/// caller remains responsible for evaluating argument expressions and fitting
/// their resulting values to these descriptors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeCallableAdmission {
    pub(crate) candidate: DeclarationCandidateKey,
    pub(crate) identity: crate::semantic_query_nucleus::DeclarationIdentityProjection,
    pub(crate) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    pub(crate) parameters: Arc<[crate::durable_semantics::DurableSemanticParameter]>,
    pub(crate) result: DurableType,
    pub(crate) shell_parameters: Arc<[crate::declaration_candidate::DeclarationParameterHeader]>,
}

/// A session-issued call capability.  The capability is deliberately
/// non-Clone; only its private identity handle may be retained by the bound
/// payload while the admitted wrapper remains owned by the caller.
#[derive(Debug)]
struct DurableComptimeCallToken {
    identity: Arc<DurableComptimeCallTokenIdentity>,
}

#[derive(Debug, PartialEq, Eq)]
struct DurableComptimeCallTokenIdentity {
    session: u64,
    ordinal: u32,
}

#[derive(Debug)]
struct DurableComptimeCallTokenHandle(Arc<DurableComptimeCallTokenIdentity>);

/// A one-shot ordinal reservation issued by a durable session. It is consumed
/// to create the admission wrapper and cannot be copied into another call.
#[derive(Debug)]
pub(crate) struct DurableComptimeCallReservation {
    token: DurableComptimeCallToken,
}

#[cfg(test)]
impl DurableComptimeCallReservation {
    fn ordinal(&self) -> u32 {
        self.token.identity.ordinal
    }
}

impl DurableComptimeCallToken {
    fn new(session: u64, ordinal: u32) -> Self {
        Self {
            identity: Arc::new(DurableComptimeCallTokenIdentity { session, ordinal }),
        }
    }

    fn handle(&self) -> DurableComptimeCallTokenHandle {
        DurableComptimeCallTokenHandle(Arc::clone(&self.identity))
    }
}

impl DurableComptimeCallTokenHandle {
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    fn ordinal(&self) -> u32 {
        self.0.ordinal
    }
}

impl PartialEq for DurableComptimeCallTokenHandle {
    fn eq(&self, other: &Self) -> bool {
        self.same(other)
    }
}

impl Eq for DurableComptimeCallTokenHandle {}

/// Admission paired with the session-issued token before argument evaluation.
/// It is consumed only after the resulting bound payload is complete.
#[derive(Debug)]
pub(crate) struct DurableComptimeAdmittedCall {
    token: DurableComptimeCallToken,
    admission: DurableComptimeCallableAdmission,
}

impl DurableComptimeAdmittedCall {
    fn new(token: DurableComptimeCallToken, admission: DurableComptimeCallableAdmission) -> Self {
        Self { token, admission }
    }

    pub(crate) fn parameters(&self) -> &[crate::durable_semantics::DurableSemanticParameter] {
        &self.admission.parameters
    }

    pub(crate) fn shell_parameters(
        &self,
    ) -> &[crate::declaration_candidate::DeclarationParameterHeader] {
        &self.admission.shell_parameters
    }
}

/// Opaque call-specific admission contract.  It retains every semantic fact
/// that was admitted before argument evaluation, so a bound payload cannot be
/// paired with a different candidate, configuration, signature, shell, or
/// result contract.
#[derive(Debug, PartialEq, Eq)]
struct DurableComptimeAdmissionStamp {
    candidate: DeclarationCandidateKey,
    identity: crate::semantic_query_nucleus::DeclarationIdentityProjection,
    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    parameters: Arc<[crate::durable_semantics::DurableSemanticParameter]>,
    result: DurableType,
    shell_parameters: Arc<[crate::declaration_candidate::DeclarationParameterHeader]>,
}

impl DurableComptimeAdmissionStamp {
    fn from_admission(admission: &DurableComptimeCallableAdmission) -> Self {
        Self {
            candidate: admission.candidate.clone(),
            identity: admission.identity.clone(),
            configuration: admission.configuration.clone(),
            parameters: admission.parameters.clone(),
            result: admission.result.clone(),
            shell_parameters: admission.shell_parameters.clone(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DurableComptimeBinding {
    token: DurableComptimeCallTokenHandle,
    admission: DurableComptimeAdmissionStamp,
    type_arguments: Vec<(Arc<str>, DurableType)>,
    value_arguments: Vec<(Arc<str>, DurableConstValue)>,
    typed_value_arguments: Vec<(Arc<str>, EvaluatedSemanticConst)>,
}

impl DurableComptimeBinding {
    pub(crate) fn new(admitted: &DurableComptimeAdmittedCall) -> Self {
        Self {
            token: admitted.token.handle(),
            admission: DurableComptimeAdmissionStamp::from_admission(&admitted.admission),
            type_arguments: Vec::new(),
            value_arguments: Vec::new(),
            typed_value_arguments: Vec::new(),
        }
    }

    pub(super) fn parameter(
        &self,
        index: usize,
    ) -> Option<&crate::durable_semantics::DurableSemanticParameter> {
        self.admission.parameters.get(index)
    }

    pub(super) fn shell_parameter(
        &self,
        index: usize,
    ) -> Option<&crate::declaration_candidate::DeclarationParameterHeader> {
        self.admission.shell_parameters.get(index)
    }

    /// Finish binding only after every argument has passed the canonical
    /// parameter fit policy.  The resulting payload owns the substituted
    /// frame metadata; callers cannot reconstruct it from raw query values.
    pub(crate) fn finish(self) -> DurableComptimeBoundCall {
        let expected_result = substitute_durable_generics(
            &self.admission.result,
            &self
                .type_arguments
                .iter()
                .map(|(_, ty)| ty.clone())
                .collect::<Vec<_>>(),
        );
        DurableComptimeBoundCall {
            token: self.token,
            admission: self.admission,
            type_arguments: self.type_arguments,
            value_arguments: self.value_arguments,
            typed_value_arguments: self.typed_value_arguments,
            expected_result,
        }
    }
}

/// Opaque ordered call facts produced by the durable binding kernel.  The
/// typed values and substituted result are private so a host cannot
/// manufacture arbitrary frame metadata beside the binding policy.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DurableComptimeBoundCall {
    token: DurableComptimeCallTokenHandle,
    admission: DurableComptimeAdmissionStamp,
    type_arguments: Vec<(Arc<str>, DurableType)>,
    value_arguments: Vec<(Arc<str>, DurableConstValue)>,
    typed_value_arguments: Vec<(Arc<str>, EvaluatedSemanticConst)>,
    expected_result: DurableType,
}

impl DurableComptimeBoundCall {
    /// Borrow the canonical ordered query facts without consuming the bound
    /// call. The view is intentionally private and cannot be paired with an
    /// independently supplied producer or lifecycle edge.
    pub(crate) fn query_view(&self) -> DurableComptimeBoundCallQuery<'_> {
        DurableComptimeBoundCallQuery {
            configuration: &self.admission.configuration,
            type_arguments: &self.type_arguments,
            value_arguments: &self.value_arguments,
        }
    }

    #[cfg(test)]
    pub(super) fn expected_result(&self) -> &DurableType {
        &self.expected_result
    }
}

/// One-shot borrowed query facts retained by a pending prepared call.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct DurableComptimeBoundCallQuery<'a> {
    configuration: &'a crate::semantic_query_nucleus::SemanticQueryConfiguration,
    type_arguments: &'a [(Arc<str>, DurableType)],
    value_arguments: &'a [(Arc<str>, DurableConstValue)],
}

impl<'a> DurableComptimeBoundCallQuery<'a> {
    pub(super) fn configuration(
        &self,
    ) -> &crate::semantic_query_nucleus::SemanticQueryConfiguration {
        self.configuration
    }

    pub(crate) fn type_arguments(&self) -> &[(Arc<str>, DurableType)] {
        self.type_arguments
    }

    #[allow(dead_code)]
    pub(crate) fn value_arguments(&self) -> &[(Arc<str>, DurableConstValue)] {
        self.value_arguments
    }
}

/// A non-replayable call after admission and before the foreign probe. The
/// edge, producer, complete program key, and bound call are consumed together
/// so callers cannot cross-pair an ordinal with another query, configuration,
/// or binding payload.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct DurableComptimePendingCall {
    edge: DurableComptimeCallEdge,
    producer: crate::StableDefinitionKey,
    program: crate::body_query::DurableComptimeProgramKey,
    token: DurableComptimeCallTokenHandle,
    bound: DurableComptimeBoundCall,
}

impl DurableComptimePendingCall {
    pub(super) fn producer(&self) -> &crate::StableDefinitionKey {
        &self.producer
    }

    pub(super) fn query_view(&self) -> DurableComptimeBoundCallQuery<'_> {
        self.bound.query_view()
    }
}

/// A non-replayable result of exactly one foreign probe. Raw lookup variants
/// never escape this package and cannot be retried without reconstructing the
/// consumed admission, edge, and bound call.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct DurableComptimeProbedCall {
    pub(super) pending: DurableComptimePendingCall,
    pub(super) lookup: ForeignComptimeCallLookup,
}

/// Match one already-evaluated durable argument immediately. The binding is
/// mutated in source order, so a later value parameter sees all preceding
/// concrete type substitutions and no earlier argument is replayed.
pub(crate) fn bind_durable_comptime_argument(
    binding: &mut DurableComptimeBinding,
    parameter_name: &str,
    parameter: &crate::durable_semantics::DurableSemanticParameter,
    argument: TypedSemanticConst,
    direct_unit_literal: bool,
) -> Result<(), DurableComptimeFailure> {
    let TypedSemanticConst { value, ty } = argument;
    if parameter.ty == DurableType::ComptimeType {
        let value = match value {
            DurableConstValue::Type(ty) => ty,
            DurableConstValue::Unit if direct_unit_literal => DurableType::Unit,
            _ => {
                return Err(DurableComptimeFailure::comptime_failure(format!(
                    "argument for comptime parameter `{parameter_name}` must be a type"
                )));
            }
        };
        binding
            .type_arguments
            .push((Arc::from(parameter_name), value));
        return Ok(());
    }

    let expected = substitute_durable_generics(
        &parameter.ty,
        &binding
            .type_arguments
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>(),
    );
    if let Some(found) = ty
        && found != expected
    {
        return Err(DurableComptimeFailure::failure(
            SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::TypeMismatch {
                expected: durable_type_diagnostic_name(&expected),
                found: durable_type_diagnostic_name(&found),
            }),
        ));
    }
    if let Some(failure) = durable_value_fit_failure(&value, &expected) {
        return Err(match failure {
            DurableComptimeValueFitFailure::CallableAlias => {
                DurableComptimeFailure::comptime_failure(
                    "a callable alias cannot be passed as a comptime value argument",
                )
            }
            DurableComptimeValueFitFailure::IntegerOutOfRange { value, type_name } => {
                DurableComptimeFailure::comptime_failure(format!(
                    "value {value} is outside the range of type {type_name}"
                ))
            }
            DurableComptimeValueFitFailure::TypeMismatch { expected, found } => {
                DurableComptimeFailure::failure(SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::TypeMismatch { expected, found },
                ))
            }
        });
    }
    let parameter_name: Arc<str> = Arc::from(parameter_name);
    binding
        .value_arguments
        .push((parameter_name.clone(), value.clone()));
    binding.typed_value_arguments.push((
        parameter_name,
        EvaluatedSemanticConst::Value(TypedSemanticConst::typed(value, expected)),
    ));
    Ok(())
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    pub(crate) fn admitted_call_fixture(
        admission: DurableComptimeCallableAdmission,
        ordinal: u32,
    ) -> DurableComptimeAdmittedCall {
        DurableComptimeAdmittedCall::new(
            DurableComptimeCallToken::new(u64::MAX, ordinal),
            admission,
        )
    }
}

#[cfg(test)]
mod effect_lifecycle_tests {
    use super::test_support::admitted_call_fixture;
    use super::*;
    use crate::durable_semantics::{DurableParameterMode, DurableSemanticParameter};

    fn definition(name: &str) -> crate::StableDefinitionKey {
        crate::StableDefinitionKey::from_stable_parts(
            crate::ModuleId::from_logical_path("effects.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from(name),
            None,
        )
    }

    fn context(ordinal: u32) -> DurableComptimeCallContext {
        let parent_producer = definition("parent");
        context_with_parent(parent_producer, ordinal)
    }

    fn context_with_parent(
        parent_producer: crate::StableDefinitionKey,
        ordinal: u32,
    ) -> DurableComptimeCallContext {
        context_with_parent_and_child(parent_producer, definition("child"), ordinal)
    }

    fn context_with_parent_and_child(
        parent_producer: crate::StableDefinitionKey,
        child_producer: crate::StableDefinitionKey,
        ordinal: u32,
    ) -> DurableComptimeCallContext {
        let parent_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &parent_producer,
            )
            .unwrap();
        DurableComptimeCallContext::for_test(
            parent_producer,
            parent_declaration,
            child_producer,
            ordinal,
        )
    }

    fn binding_parameter(name: &str, ty: DurableType) -> DurableSemanticParameter {
        DurableSemanticParameter {
            name: Arc::from(name),
            ty,
            mode: DurableParameterMode::Value,
            is_comptime: true,
        }
    }

    fn binding_admission() -> DurableComptimeCallableAdmission {
        let module = ModuleId::from_logical_path("binding-test.rue").unwrap();
        let key = crate::StableDefinitionKey::from_stable_parts(
            module,
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "binding",
            None,
        );
        let parameters: Arc<[DurableSemanticParameter]> = Arc::from([
            binding_parameter("T", DurableType::ComptimeType),
            binding_parameter("value", DurableType::GenericParameter(0)),
        ]);
        let shell_parameters: Arc<[crate::declaration_candidate::DeclarationParameterHeader]> =
            Arc::from([
                crate::declaration_candidate::DeclarationParameterHeader {
                    name: Arc::from("T"),
                    mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                    is_comptime: true,
                    is_type_parameter: true,
                },
                crate::declaration_candidate::DeclarationParameterHeader {
                    name: Arc::from("value"),
                    mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                    is_comptime: true,
                    is_type_parameter: false,
                },
            ]);
        DurableComptimeCallableAdmission {
            candidate: crate::revisioned_query_database::declaration_candidate_for_stable_key(&key)
                .unwrap(),
            identity: crate::semantic_query_nucleus::DeclarationIdentityProjection {
                key,
                is_public: true,
            },
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: rue_target::Target::X86_64Linux,
                preview_features: crate::StablePreviewFeatures::new(
                    &crate::PreviewFeatures::default(),
                ),
            },
            parameters,
            result: DurableType::GenericParameter(0),
            shell_parameters,
        }
    }

    fn binding() -> DurableComptimeBinding {
        let admitted = admitted_call_fixture(binding_admission(), 0);
        DurableComptimeBinding::new(&admitted)
    }

    fn typed_binding_value(value: DurableConstValue, ty: DurableType) -> TypedSemanticConst {
        TypedSemanticConst {
            value,
            ty: Some(ty),
        }
    }

    #[test]
    fn incremental_binding_preserves_type_then_value_order_and_substitution() {
        let mut binding = binding();
        bind_durable_comptime_argument(
            &mut binding,
            "T",
            &binding_parameter("T", DurableType::ComptimeType),
            typed_binding_value(
                DurableConstValue::Type(DurableType::I16),
                DurableType::ComptimeType,
            ),
            false,
        )
        .unwrap();
        bind_durable_comptime_argument(
            &mut binding,
            "value",
            &binding_parameter("value", DurableType::GenericParameter(0)),
            typed_binding_value(DurableConstValue::Integer(12), DurableType::I16),
            false,
        )
        .unwrap();
        let bound = binding.finish();
        assert_eq!(bound.expected_result(), &DurableType::I16);
        let query = bound.query_view();
        assert_eq!(
            query.type_arguments(),
            &[(Arc::from("T"), DurableType::I16)]
        );
        assert_eq!(
            query.value_arguments(),
            &[(Arc::from("value"), DurableConstValue::Integer(12))]
        );
    }

    #[test]
    fn incremental_binding_preserves_early_type_and_range_failures() {
        let mut mismatch = binding();
        let failure = bind_durable_comptime_argument(
            &mut mismatch,
            "value",
            &binding_parameter("value", DurableType::I16),
            typed_binding_value(DurableConstValue::Bool(true), DurableType::Bool),
            false,
        )
        .unwrap_err();
        match failure {
            DurableComptimeFailure::Failure(failure) => assert!(matches!(
                failure.as_ref(),
                SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::TypeMismatch { .. })
            )),
            other => panic!("unexpected binding failure: {other:?}"),
        }

        let mut range = binding();
        let failure = bind_durable_comptime_argument(
            &mut range,
            "value",
            &binding_parameter("value", DurableType::I8),
            TypedSemanticConst {
                value: DurableConstValue::Integer(300),
                ty: None,
            },
            false,
        )
        .unwrap_err();
        match failure {
            DurableComptimeFailure::Failure(failure) => match failure.as_ref() {
                SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ComptimeEvaluationFailed { reason },
                ) => assert_eq!(reason, "value 300 is outside the range of type i8"),
                other => panic!("unexpected range failure: {other:?}"),
            },
            other => panic!("unexpected binding failure: {other:?}"),
        }
    }

    #[test]
    fn incremental_binding_requires_direct_unit_for_type_arguments() {
        let mut direct = binding();
        bind_durable_comptime_argument(
            &mut direct,
            "T",
            &binding_parameter("T", DurableType::ComptimeType),
            typed_binding_value(DurableConstValue::Unit, DurableType::Unit),
            true,
        )
        .unwrap();
        let bound = direct.finish();
        assert_eq!(
            bound.query_view().type_arguments(),
            &[(Arc::from("T"), DurableType::Unit)]
        );

        let mut computed = binding();
        let failure = bind_durable_comptime_argument(
            &mut computed,
            "T",
            &binding_parameter("T", DurableType::ComptimeType),
            typed_binding_value(DurableConstValue::Unit, DurableType::Unit),
            false,
        )
        .unwrap_err();
        match failure {
            DurableComptimeFailure::Failure(failure) => match failure.as_ref() {
                SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ComptimeEvaluationFailed { reason },
                ) => assert_eq!(reason, "argument for comptime parameter `T` must be a type"),
                other => panic!("unexpected type failure: {other:?}"),
            },
            other => panic!("unexpected binding failure: {other:?}"),
        }
    }

    #[test]
    fn diagnostic_sites_are_keyed_and_reject_unknown_programs() {
        let first = super::super::structured::structured_type_adapter_tests::const_program(
            "diagnostic-first.rue",
            "i32",
        );
        let second = super::super::structured::structured_type_adapter_tests::const_program(
            "diagnostic-second.rue",
            "i64",
        );
        let mut session = super::super::structured::structured_type_adapter_tests::session();
        session.register_program(&first).unwrap();
        session.register_program(&second).unwrap();

        let span = rue_span::Span::with_file(rue_span::FileId::DEFAULT, 11, 19);
        let first_site = session.diagnostic_site(&first.plan.key, span).unwrap();
        let second_site = session.diagnostic_site(&second.plan.key, span).unwrap();
        assert_eq!(first_site.range_for_test(), (11, 19));
        assert_eq!(second_site.range_for_test(), (11, 19));
        assert_ne!(
            first_site.producer_for_test(),
            second_site.producer_for_test()
        );

        let unknown = super::super::structured::structured_type_adapter_tests::callable_program(
            "diagnostic-unknown.rue",
        );
        assert_eq!(
            session.diagnostic_site(&unknown.plan.key, span),
            Err(DurableComptimeDiagnosticSiteError::UnknownProgram)
        );
    }

    #[test]
    fn durable_session_isolates_root_ordinals_and_owns_lifecycle() {
        let parent = definition("parent");
        let parent_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let mut session =
            DurableComptimeSession::new(parent.clone(), parent_declaration.clone()).unwrap();
        assert_eq!(session.reserve_bound_expression_call().ordinal(), 0);
        assert_eq!(session.reserve_bound_expression_call().ordinal(), 1);

        let mut ticket = session.lifecycle_mut().prepare(context(0)).unwrap();
        session.lifecycle_mut().enter(&ticket).unwrap();
        session
            .lifecycle_mut()
            .finish(&mut ticket, &rue_air::ComptimeOutcome::<(), ()>::Known(()))
            .unwrap();

        let mut sibling = DurableComptimeSession::new(parent, parent_declaration).unwrap();
        assert_eq!(sibling.reserve_bound_expression_call().ordinal(), 0);
    }

    #[test]
    fn durable_session_routes_ready_projection_through_expression_edge() {
        let parent = definition("parent");
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let mut session = DurableComptimeSession::new(parent, declaration).unwrap();
        let edge = session.prepare_expression_edge(9).unwrap();
        session
            .finish_ready_expression_edge(edge, ready_projection(9))
            .unwrap();
        let effects = session.drain_root_effects().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .next()
                .unwrap()
                .application
                .as_ref()
                .unwrap()
                .call_ordinal,
            9
        );
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn anonymous_nominal_projection_preserves_struct_shape_modes_captures_and_effects() {
        let parent = definition("parent");
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let mut session = DurableComptimeSession::new(parent.clone(), declaration).unwrap();
        let identity_anchor = rue_rir::RirStructuralAnchor::new(Arc::from([
            rue_rir::RirStructuralPathSegment::AnonymousType(3),
            rue_rir::RirStructuralPathSegment::Method(7),
        ]));
        let identity = crate::AnonymousNominalKey {
            kind: rue_air::AnonymousNominalKind::Struct,
            producer: crate::StableProducerId::Definition(parent.clone()),
            anchor: identity_anchor.clone(),
        };
        let ty = project_durable_anonymous_nominal(
            &mut session,
            DurableAnonymousNominalDescriptor {
                identity: identity.clone(),
                shape: DurableAnonymousNominalDescriptorShape::Struct {
                    fields: Arc::from([rue_air::ComptimeField {
                        name: Arc::from("value"),
                        ty: DurableType::I32,
                    }]),
                    methods: Arc::from([rue_air::ComptimeMethodDescriptor {
                        name: Arc::from("borrow_value"),
                        has_self: true,
                        self_mode: rue_rir::RirParamMode::Inout,
                        returns_borrow: true,
                        returns_inout: false,
                        parameters: vec![rue_air::ComptimeMethodParameter {
                            ty: rue_air::ComptimeMethodType::Concrete(DurableType::I32),
                            mode: rue_rir::RirParamMode::Borrow,
                            is_comptime: true,
                            is_comptime_type: false,
                        }],
                        parameter_names: vec![Arc::from("value")],
                        result: rue_air::ComptimeMethodType::SelfType,
                        declaration_span: rue_span::Span::new(0, 0),
                    }]),
                },
                type_captures: Arc::from([(Arc::from("T"), DurableType::U64)]),
                value_captures: Arc::from([(Arc::from("n"), DurableConstValue::Integer(9))]),
            },
        )
        .unwrap();
        let DurableType::AnonymousNominal(identity) = ty else {
            panic!("anonymous projection must return its nominal identity");
        };
        assert_eq!(identity.anchor, identity_anchor);
        assert_eq!(identity.kind, rue_air::AnonymousNominalKind::Struct);
        let effects = session.drain_root_effects().unwrap();
        let nominal = effects
            .anonymous_nominals()
            .next()
            .expect("projection publishes exactly one nominal effect");
        assert_eq!(nominal.identity, identity);
        assert_eq!(
            nominal.type_captures.as_ref(),
            &[(Arc::from("T"), DurableType::U64)]
        );
        assert_eq!(
            nominal.value_captures.as_ref(),
            &[(Arc::from("n"), DurableConstValue::Integer(9))]
        );
        let crate::durable_semantics::DurableAnonymousNominalShape::Struct { methods, .. } =
            &nominal.shape
        else {
            panic!("projection changed the struct shape");
        };
        let crate::durable_semantics::DurableAnonymousNominalShape::Struct { fields, .. } =
            &nominal.shape
        else {
            panic!("projection changed the struct shape");
        };
        assert_eq!(fields[0].0.as_ref(), "value");
        assert_eq!(fields[0].1, DurableType::I32);
        assert_eq!(methods[0].name.as_ref(), "borrow_value");
        assert!(methods[0].has_self);
        assert_eq!(
            methods[0].self_mode,
            crate::durable_semantics::DurableParameterMode::Inout
        );
        assert_eq!(
            methods[0].parameters[0].1,
            crate::durable_semantics::DurableParameterMode::Borrow
        );
        assert!(methods[0].parameters[0].2);
        assert!(methods[0].returns_borrow);
        assert!(!methods[0].returns_inout);
        assert_eq!(
            methods[0].result,
            crate::durable_semantics::DurableAnonymousMethodType::SelfType
        );
        assert!(methods[0].has_body);
    }

    #[test]
    fn anonymous_nominal_projection_preserves_enum_shape_and_identity_kind() {
        let parent = definition("parent");
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let mut session = DurableComptimeSession::new(parent.clone(), declaration).unwrap();
        let anchor = rue_rir::RirStructuralAnchor::new(Arc::from([
            rue_rir::RirStructuralPathSegment::AnonymousType(11),
        ]));
        let ty = project_durable_anonymous_nominal(
            &mut session,
            DurableAnonymousNominalDescriptor {
                identity: crate::AnonymousNominalKey {
                    kind: rue_air::AnonymousNominalKind::Enum,
                    producer: crate::StableProducerId::Definition(parent),
                    anchor,
                },
                shape: DurableAnonymousNominalDescriptorShape::Enum {
                    variants: Arc::from([
                        (Arc::from("None"), Arc::from([])),
                        (Arc::from("Some"), Arc::from([DurableType::I32])),
                    ]),
                },
                type_captures: Arc::from([]),
                value_captures: Arc::from([]),
            },
        )
        .unwrap();
        let DurableType::AnonymousNominal(identity) = ty else {
            panic!("anonymous projection must return its nominal identity");
        };
        assert_eq!(identity.kind, rue_air::AnonymousNominalKind::Enum);
        let effects = session.drain_root_effects().unwrap();
        let nominal = effects
            .anonymous_nominals()
            .next()
            .expect("projection publishes the enum effect");
        let crate::durable_semantics::DurableAnonymousNominalShape::Enum { variants } =
            &nominal.shape
        else {
            panic!("projection changed the enum shape");
        };
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].0.as_ref(), "None");
        assert!(variants[0].1.is_empty());
        assert_eq!(variants[1].1.as_ref(), &[DurableType::I32]);
    }

    #[test]
    fn anonymous_nominal_projection_canonicalizes_permuted_captures() {
        let parent = definition("parent");
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let identity = crate::AnonymousNominalKey {
            kind: rue_air::AnonymousNominalKind::Enum,
            producer: crate::StableProducerId::Definition(parent.clone()),
            anchor: rue_rir::RirStructuralAnchor::new(Arc::from([
                rue_rir::RirStructuralPathSegment::AnonymousType(12),
            ])),
        };
        let project = |session: &mut DurableComptimeSession,
                       type_captures: Arc<[(Arc<str>, DurableType)]>,
                       value_captures: Arc<[(Arc<str>, DurableConstValue)]>| {
            project_durable_anonymous_nominal(
                session,
                DurableAnonymousNominalDescriptor {
                    identity: identity.clone(),
                    shape: DurableAnonymousNominalDescriptorShape::Enum {
                        variants: Arc::from([(Arc::from("None"), Arc::from([]))]),
                    },
                    type_captures,
                    value_captures,
                },
            )
            .unwrap()
        };
        let mut first = DurableComptimeSession::new(parent.clone(), declaration.clone()).unwrap();
        let first_ty = project(
            &mut first,
            Arc::from([
                (Arc::from("Z"), DurableType::U64),
                (Arc::from("A"), DurableType::I32),
            ]),
            Arc::from([
                (Arc::from("z"), DurableConstValue::Integer(2)),
                (Arc::from("a"), DurableConstValue::Integer(1)),
            ]),
        );
        let first_effects = first.drain_root_effects().unwrap();
        let mut second = DurableComptimeSession::new(parent, declaration).unwrap();
        let second_ty = project(
            &mut second,
            Arc::from([
                (Arc::from("A"), DurableType::I32),
                (Arc::from("Z"), DurableType::U64),
            ]),
            Arc::from([
                (Arc::from("a"), DurableConstValue::Integer(1)),
                (Arc::from("z"), DurableConstValue::Integer(2)),
            ]),
        );
        let second_effects = second.drain_root_effects().unwrap();
        assert_eq!(first_ty, second_ty);
        assert_eq!(first_effects, second_effects);
        let nominal = first_effects.anonymous_nominals().next().unwrap();
        assert_eq!(
            nominal.type_captures.as_ref(),
            &[
                (Arc::from("A"), DurableType::I32),
                (Arc::from("Z"), DurableType::U64),
            ]
        );
        assert_eq!(
            nominal.value_captures.as_ref(),
            &[
                (Arc::from("a"), DurableConstValue::Integer(1)),
                (Arc::from("z"), DurableConstValue::Integer(2)),
            ]
        );
    }

    #[test]
    fn anonymous_nominal_projection_rejects_mismatched_identity_without_effect() {
        let parent = definition("parent");
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let mut session = DurableComptimeSession::new(parent.clone(), declaration).unwrap();
        let result = project_durable_anonymous_nominal(
            &mut session,
            DurableAnonymousNominalDescriptor {
                identity: crate::AnonymousNominalKey {
                    kind: rue_air::AnonymousNominalKind::Enum,
                    producer: crate::StableProducerId::Definition(parent),
                    anchor: rue_rir::RirStructuralAnchor::new(Arc::from([
                        rue_rir::RirStructuralPathSegment::AnonymousType(13),
                    ])),
                },
                shape: DurableAnonymousNominalDescriptorShape::Struct {
                    fields: Arc::from([]),
                    methods: Arc::from([]),
                },
                type_captures: Arc::from([]),
                value_captures: Arc::from([]),
            },
        );
        assert!(result.is_err());
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn anonymous_nominal_projection_preserves_canonical_function_producer_identity() {
        let parent = definition("parent");
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let base = crate::FunctionInstanceKey::Definition(parent.clone());
        let raw_identity = crate::AnonymousNominalKey {
            kind: rue_air::AnonymousNominalKind::Enum,
            producer: crate::StableProducerId::Function(rue_air::Node::new(
                crate::FunctionInstanceKey::Specialization {
                    base: rue_air::Node::new(base),
                    arguments: Default::default(),
                },
            )),
            anchor: rue_rir::RirStructuralAnchor::new(Arc::from([
                rue_rir::RirStructuralPathSegment::AnonymousType(14),
            ])),
        };
        let canonical_identity = raw_identity.with_canonical_producer().into_owned();
        assert_ne!(raw_identity, canonical_identity);
        let mut session = DurableComptimeSession::new(parent, declaration).unwrap();
        let ty = project_durable_anonymous_nominal(
            &mut session,
            DurableAnonymousNominalDescriptor {
                identity: canonical_identity.clone(),
                shape: DurableAnonymousNominalDescriptorShape::Enum {
                    variants: Arc::from([]),
                },
                type_captures: Arc::from([]),
                value_captures: Arc::from([]),
            },
        )
        .unwrap();
        assert_eq!(ty, DurableType::AnonymousNominal(canonical_identity));
    }

    #[test]
    fn anonymous_nominal_projection_uses_active_lifecycle_scope() {
        let parent = definition("parent");
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let make_descriptor = |kind| DurableAnonymousNominalDescriptor {
            identity: crate::AnonymousNominalKey {
                kind,
                producer: crate::StableProducerId::Definition(parent.clone()),
                anchor: rue_rir::RirStructuralAnchor::new(Arc::from([
                    rue_rir::RirStructuralPathSegment::AnonymousType(15),
                ])),
            },
            shape: DurableAnonymousNominalDescriptorShape::Enum {
                variants: Arc::from([]),
            },
            type_captures: Arc::from([]),
            value_captures: Arc::from([]),
        };

        let mut known = DurableComptimeSession::new(parent.clone(), declaration.clone()).unwrap();
        let mut known_ticket = known.lifecycle_mut().prepare(context(0)).unwrap();
        known.lifecycle_mut().enter(&known_ticket).unwrap();
        project_durable_anonymous_nominal(
            &mut known,
            make_descriptor(rue_air::AnonymousNominalKind::Enum),
        )
        .unwrap();
        known
            .lifecycle_mut()
            .finish(
                &mut known_ticket,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
            )
            .unwrap();
        assert_eq!(
            known
                .drain_root_effects()
                .unwrap()
                .anonymous_nominals()
                .count(),
            1
        );

        let mut dropped = DurableComptimeSession::new(parent.clone(), declaration).unwrap();
        let mut dropped_ticket = dropped.lifecycle_mut().prepare(context(1)).unwrap();
        dropped.lifecycle_mut().enter(&dropped_ticket).unwrap();
        project_durable_anonymous_nominal(
            &mut dropped,
            make_descriptor(rue_air::AnonymousNominalKind::Enum),
        )
        .unwrap();
        dropped
            .lifecycle_mut()
            .finish(
                &mut dropped_ticket,
                &rue_air::ComptimeOutcome::<(), ()>::NotReady,
            )
            .unwrap();
        assert!(dropped.drain_root_effects().unwrap().is_empty());
    }

    fn structured_context() -> DurableComptimeCallContext {
        structured_context_with_parent(definition("parent"))
    }

    fn structured_context_with_parent(
        parent_producer: crate::StableDefinitionKey,
    ) -> DurableComptimeCallContext {
        let parent_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &parent_producer,
            )
            .unwrap();
        DurableComptimeCallContext::for_test_structured(
            parent_producer,
            parent_declaration,
            definition("child"),
        )
    }

    fn lifecycle() -> DurableComptimeCallLifecycle {
        DurableComptimeCallLifecycle::new(
            definition("parent"),
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "parent",
            ))
            .unwrap(),
        )
        .unwrap()
    }

    fn gate(ordinal: u32) -> DeferredOwnershipGate {
        DeferredOwnershipGate {
            kind: crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireDroppable,
            ty: DurableType::I32,
            source: Arc::new(crate::semantic_query_nucleus::DeferredOwnershipGateSource {
                declaration:
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &definition("child"),
                    )
                    .unwrap(),
                start: ordinal,
                end: ordinal + 1,
            }),
            application: None,
        }
    }

    fn child_effects(ordinal: u32) -> DurableComptimeEffects {
        let mut effects = DurableComptimeEffects::default();
        effects.observe_deferred_ownership(gate(ordinal));
        effects
    }

    fn ready_projection(ordinal: u32) -> crate::semantic_query_nucleus::ComptimeCallProjection {
        crate::semantic_query_nucleus::ComptimeCallProjection {
            result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                DurableConstValue::Integer(ordinal.into()),
            ),
            anonymous_nominals: Arc::from([]),
            dependencies: Arc::from([]),
            deferred_ownership: Arc::from([gate(ordinal)]),
        }
    }

    fn child_effects_with_application(
        ordinal: u32,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
        call_ordinal: u32,
    ) -> DurableComptimeEffects {
        let mut effects = DurableComptimeEffects::default();
        let mut gate = gate(ordinal);
        gate.application = Some(
            crate::semantic_query_nucleus::DeferredOwnershipApplication {
                declaration,
                call_ordinal,
            },
        );
        effects.observe_deferred_ownership(gate);
        effects
    }

    trait LifecycleTestEffects {
        fn finish_with_effects<V, F>(
            &mut self,
            ticket: &mut DurableComptimeCallTicket,
            outcome: &rue_air::ComptimeOutcome<V, F>,
            effects: DurableComptimeEffects,
        ) -> Result<(), DurableComptimeLifecycleError>;

        fn complete_known(
            self,
        ) -> Result<
            DurableComptimeCompletion<(), ()>,
            (
                Self,
                rue_air::ComptimeOutcome<(), ()>,
                DurableComptimeLifecycleError,
            ),
        >
        where
            Self: Sized;

        fn root_effects_for_test(&self) -> &DurableComptimeEffects;
    }

    impl LifecycleTestEffects for DurableComptimeCallLifecycle {
        fn finish_with_effects<V, F>(
            &mut self,
            ticket: &mut DurableComptimeCallTicket,
            outcome: &rue_air::ComptimeOutcome<V, F>,
            effects: DurableComptimeEffects,
        ) -> Result<(), DurableComptimeLifecycleError> {
            for nominal in effects.anonymous_nominals().cloned() {
                self.observe_anonymous_nominal(nominal);
            }
            for dependency in effects.dependencies().cloned() {
                self.observe_dependency(dependency);
            }
            for gate in effects.deferred_ownership().cloned() {
                self.observe_deferred_ownership(gate);
            }
            self.finish(ticket, outcome)
        }

        fn complete_known(
            self,
        ) -> Result<
            DurableComptimeCompletion<(), ()>,
            (
                Self,
                rue_air::ComptimeOutcome<(), ()>,
                DurableComptimeLifecycleError,
            ),
        > {
            self.complete_root(rue_air::ComptimeOutcome::Known(()))
        }

        fn root_effects_for_test(&self) -> &DurableComptimeEffects {
            &self.effects
        }
    }

    fn observed_effects() -> DurableComptimeEffects {
        let mut effects = DurableComptimeEffects::default();
        let identity = crate::AnonymousNominalKey {
            kind: rue_air::AnonymousNominalKind::Struct,
            producer: crate::StableProducerId::Definition(definition("parent")),
            anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
        };
        effects.observe_anonymous_nominal(DurableAnonymousNominal::new(
            identity,
            crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                fields: Arc::from([]),
                methods: Arc::from([]),
            },
            Arc::from([]),
            Arc::from([]),
        ));
        effects.observe_dependency(SemanticDeclarationDependency {
            source: definition("parent"),
            kind: rue_air::DeclarationTypeDependencyKind::Body,
            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                definition("child"),
            ),
        });
        effects
    }

    #[test]
    fn effects_merge_canonicalizes_nominal_collisions_and_observations() {
        assert!(DurableComptimeEffects::default().is_empty());
        let identity = crate::AnonymousNominalKey {
            kind: rue_air::AnonymousNominalKind::Struct,
            producer: crate::StableProducerId::Definition(definition("parent")),
            anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
        };
        let first = DurableAnonymousNominal::new(
            identity.clone(),
            crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                fields: Arc::from([]),
                methods: Arc::from([]),
            },
            Arc::from([]),
            Arc::from([]),
        );
        let second = first.with_shape(
            crate::durable_semantics::DurableAnonymousNominalShape::Enum {
                variants: Arc::from([]),
            },
        );
        let mut effects = DurableComptimeEffects::default();
        effects.observe_anonymous_nominal(first);
        effects.observe_anonymous_nominal(second.clone());
        effects.observe_dependency(SemanticDeclarationDependency {
            source: definition("parent"),
            kind: rue_air::DeclarationTypeDependencyKind::Body,
            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                definition("child"),
            ),
        });
        effects.observe_dependency(SemanticDeclarationDependency {
            source: definition("parent"),
            kind: rue_air::DeclarationTypeDependencyKind::Body,
            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                definition("child"),
            ),
        });
        effects.observe_deferred_ownership(gate(1));
        assert_eq!(effects.anonymous_nominals().count(), 1);
        assert_eq!(effects.dependencies().count(), 1);
        assert_eq!(effects.anonymous_nominals().next(), Some(&second));
        assert_eq!(effects.deferred_ownership().count(), 1);
    }

    #[test]
    fn root_completion_preserves_known_outcome_and_direct_observations() {
        let mut lifecycle = lifecycle();
        let direct = observed_effects();
        lifecycle.observe_anonymous_nominal(
            direct
                .anonymous_nominals()
                .next()
                .cloned()
                .expect("direct nominal observation"),
        );
        lifecycle.observe_dependency(SemanticDeclarationDependency {
            source: definition("parent"),
            kind: rue_air::DeclarationTypeDependencyKind::Body,
            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                definition("child"),
            ),
        });
        lifecycle.observe_deferred_ownership(gate(77));
        let completion = lifecycle
            .complete_root(rue_air::ComptimeOutcome::<u32, ()>::Known(17))
            .unwrap();
        assert!(matches!(
            completion.outcome(),
            rue_air::ComptimeOutcome::Known(17)
        ));
        assert_eq!(completion.effects().anonymous_nominals().count(), 1);
        assert_eq!(completion.effects().dependencies().count(), 1);
        assert_eq!(completion.effects().deferred_ownership().count(), 1);
        let (outcome, effects) = completion.into_parts();
        assert!(matches!(outcome, rue_air::ComptimeOutcome::Known(17)));
        assert_eq!(effects.deferred_ownership().count(), 1);
    }

    #[test]
    fn root_completion_preserves_every_non_known_terminal_without_effects() {
        fn assert_empty(
            outcome: rue_air::ComptimeOutcome<(), &'static str>,
            expected: fn(&rue_air::ComptimeOutcome<(), &'static str>) -> bool,
        ) {
            let mut lifecycle = lifecycle();
            lifecycle.observe_dependency(SemanticDeclarationDependency {
                source: definition("parent"),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target:
                    crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        definition("child"),
                    ),
            });
            let completion = lifecycle.complete_root(outcome).unwrap();
            assert!(expected(completion.outcome()));
            assert!(completion.effects().is_empty());
        }

        assert_empty(rue_air::ComptimeOutcome::RuntimeDependent, |outcome| {
            matches!(outcome, rue_air::ComptimeOutcome::RuntimeDependent)
        });
        assert_empty(rue_air::ComptimeOutcome::NotReady, |outcome| {
            matches!(outcome, rue_air::ComptimeOutcome::NotReady)
        });
        assert_empty(rue_air::ComptimeOutcome::UnsupportedContext, |outcome| {
            matches!(outcome, rue_air::ComptimeOutcome::UnsupportedContext)
        });
        assert_empty(
            rue_air::ComptimeOutcome::Trap(rue_air::ComptimeTrap {
                operation: "root",
                span: rue_span::Span::new(0, 0),
            }),
            |outcome| {
                matches!(
                    outcome,
                    rue_air::ComptimeOutcome::Trap(rue_air::ComptimeTrap {
                        operation: "root",
                        ..
                    })
                )
            },
        );
        assert_empty(rue_air::ComptimeOutcome::HostFailure("host"), |outcome| {
            matches!(outcome, rue_air::ComptimeOutcome::HostFailure("host"))
        });
        assert_empty(rue_air::ComptimeOutcome::Abort("abort"), |outcome| {
            matches!(outcome, rue_air::ComptimeOutcome::Abort("abort"))
        });
    }

    #[test]
    fn root_failure_discards_direct_and_ready_observations() {
        let mut lifecycle = lifecycle();
        lifecycle.observe_dependency(SemanticDeclarationDependency {
            source: definition("parent"),
            kind: rue_air::DeclarationTypeDependencyKind::Body,
            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                definition("child"),
            ),
        });
        let mut edge = lifecycle.prepare_expression_edge(88).unwrap();
        lifecycle
            .merge_ready_projection(&mut edge, &ready_projection(88))
            .unwrap();
        let completion = lifecycle
            .complete_root(rue_air::ComptimeOutcome::<(), ()>::RuntimeDependent)
            .unwrap();
        assert!(completion.effects().is_empty());
        assert!(matches!(
            completion.outcome(),
            rue_air::ComptimeOutcome::RuntimeDependent
        ));
    }

    #[test]
    fn admitted_context_derives_the_exact_program_and_ordered_arguments() {
        let snapshot = crate::SourceSnapshot::single(
            "<durable-context>",
            "fn target() -> i32 { 1 } fn sibling() -> i32 { 2 }",
        )
        .unwrap();
        let module = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
            .unwrap()
            .modules()[0]
            .clone();
        let candidate = module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|candidate| candidate.name.as_ref() == "target")
            .unwrap()
            .clone();
        let sibling = module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|candidate| candidate.name.as_ref() == "sibling")
            .unwrap()
            .clone();
        let artifacts =
            crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || {
                Ok(())
            })
            .unwrap();
        let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: rue_target::Target::X86_64Linux,
            preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
        };
        let producer = crate::StableDefinitionKey::from_stable_parts(
            candidate.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            candidate.name.clone(),
            None,
        );
        let seed = crate::body_query::ForeignComptimeCallSeed {
            type_arguments: Arc::from([
                (Arc::from("z"), DurableType::I32),
                (Arc::from("a"), DurableType::I64),
            ]),
            value_arguments: Arc::from([
                (
                    Arc::from("z"),
                    crate::durable_semantics::DurableConstValue::Integer(9),
                ),
                (
                    Arc::from("a"),
                    crate::durable_semantics::DurableConstValue::Integer(1),
                ),
            ]),
        };
        let admitted = crate::body_query::OwnedForeignComptimeProgram::from_body_plan(
            crate::body_query::DurableComptimeProgramPlan {
                key: crate::body_query::DurableComptimeProgramKey {
                    declaration: producer.clone(),
                    configuration: configuration.clone(),
                },
                candidate: candidate.clone(),
            },
            &artifacts,
            seed.clone(),
            || Ok(()),
        )
        .unwrap();
        let parent = definition("parent");
        let parent_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&parent)
                .unwrap();
        let context = DurableComptimeCallContext::from_admitted_expression(
            &admitted,
            parent.clone(),
            parent_declaration.clone(),
            42,
        )
        .unwrap();
        assert_eq!(context.program, admitted.plan.key);
        assert_eq!(context.child_producer, producer);
        assert_eq!(
            context.query.declaration.declaration,
            admitted.plan.candidate
        );
        assert_eq!(
            context.query.declaration.configuration,
            admitted.plan.key.configuration
        );
        assert_eq!(context.query.type_arguments, seed.type_arguments);
        assert_eq!(context.query.value_arguments, seed.value_arguments);
        assert_eq!(
            context.application_policy,
            DurableComptimeApplicationPolicy::ApplyAtParentCall {
                application: crate::semantic_query_nucleus::DeferredOwnershipApplication {
                    declaration: context.parent_declaration.clone(),
                    call_ordinal: 42,
                },
            }
        );
        let structured = DurableComptimeCallContext::from_admitted_structured(
            &admitted,
            parent,
            parent_declaration,
        )
        .unwrap();
        assert_eq!(
            structured.application_policy,
            DurableComptimeApplicationPolicy::Preserve
        );

        // The production session adapter consumes one pre-lookup edge and
        // returns the admitted owned program with the exact ticket; admission
        // alone does not activate a lifecycle scope.
        let mut session = DurableComptimeSession::new(
            definition("parent"),
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "parent",
            ))
            .unwrap(),
        )
        .unwrap();
        let edge = session.prepare_expression_edge(42).unwrap();
        let consumed = session
            .consume_foreign_lookup(edge, ForeignComptimeCallLookup::Admitted(admitted.clone()))
            .unwrap();
        let DurableComptimeForeignCall::Enter {
            program,
            mut ticket,
        } = consumed
        else {
            panic!("an admitted lookup must return an entered-frame plan");
        };
        assert_eq!(program.plan, admitted.plan);
        assert!(session.lifecycle.active.is_empty());

        // Producer issuance is a ticket capability, not a context helper.
        // This uses the exact ticket returned by lifecycle admission before
        // activation, so an unordered AIR binding map cannot participate.
        let issued = ticket
            .canonical_function_producer(&program.plan.key)
            .unwrap();
        assert_eq!(
            issued,
            canonical_specialized_function_producer(
                &producer,
                &seed.type_arguments,
                &seed.value_arguments,
            )
            .unwrap()
        );
        let renamed = canonical_specialized_function_producer(
            &producer,
            &seed
                .type_arguments
                .iter()
                .map(|(_, value)| (Arc::from("renamed"), value.clone()))
                .collect::<Vec<_>>(),
            &seed
                .value_arguments
                .iter()
                .map(|(_, value)| (Arc::from("renamed"), value.clone()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(issued, renamed, "argument names are not identity inputs");
        let type_reordered = canonical_specialized_function_producer(
            &producer,
            &seed
                .type_arguments
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>(),
            &seed.value_arguments,
        )
        .unwrap();
        assert_ne!(issued, type_reordered, "type stream order affects identity");
        let value_reordered = canonical_specialized_function_producer(
            &producer,
            &seed.type_arguments,
            &seed
                .value_arguments
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_ne!(
            issued, value_reordered,
            "value stream order affects identity"
        );
        let empty = canonical_specialized_function_producer(&producer, &[], &[]).unwrap();
        assert!(matches!(
            empty,
            crate::StableProducerId::Function(ref function)
                if matches!(function.as_ref(), crate::FunctionInstanceKey::Specialization { arguments, .. } if arguments.types.is_empty() && arguments.values.is_empty())
        ));
        let wrong_program = crate::body_query::DurableComptimeProgramKey {
            declaration: crate::StableDefinitionKey::from_stable_parts(
                sibling.module.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::Function,
                sibling.name.clone(),
                None,
            ),
            configuration: configuration.clone(),
        };
        assert_eq!(
            ticket.canonical_function_producer(&wrong_program),
            Err(DurableComptimeProducerIssuanceError::ProgramMismatch)
        );
        let wrong_configuration = crate::body_query::DurableComptimeProgramKey {
            declaration: program.plan.key.declaration.clone(),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: rue_target::Target::Aarch64Linux,
                preview_features: configuration.preview_features.clone(),
            },
        };
        assert_eq!(
            ticket.canonical_function_producer(&wrong_configuration),
            Err(DurableComptimeProducerIssuanceError::ProgramMismatch)
        );
        session.enter_call(&ticket).unwrap();
        session
            .finish_call(&mut ticket, &rue_air::ComptimeOutcome::<(), ()>::Known(()))
            .unwrap();

        let mut ready_session = DurableComptimeSession::new(
            definition("parent"),
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "parent",
            ))
            .unwrap(),
        )
        .unwrap();
        let ready_edge = ready_session.prepare_expression_edge(42).unwrap();
        assert!(matches!(
            ready_session
                .consume_foreign_lookup(
                    ready_edge,
                    ForeignComptimeCallLookup::Ready(ready_projection(42)),
                )
                .unwrap(),
            DurableComptimeForeignCall::Ready(
                crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(42),
                )
            )
        ));
        assert!(!ready_session.drain_root_effects().unwrap().is_empty());

        let mut miss_session = DurableComptimeSession::new(
            definition("parent"),
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "parent",
            ))
            .unwrap(),
        )
        .unwrap();
        let miss_edge = miss_session.prepare_expression_edge(43).unwrap();
        assert!(matches!(
            miss_session.consume_foreign_lookup(miss_edge, ForeignComptimeCallLookup::NotReady),
            Ok(DurableComptimeForeignCall::NotReady)
        ));
        assert!(miss_session.drain_root_effects().unwrap().is_empty());

        let failures = [
            ForeignComptimeCallLookup::ReadyFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(Arc::from("shell")),
            ),
            ForeignComptimeCallLookup::ReadyQueryFailure(rue_query::QueryFailure::new(
                "query", "failure",
            )),
            ForeignComptimeCallLookup::AdmissionFailure(
                crate::body_query::ComptimeProgramProjectionFailure::IdentityMismatch,
            ),
            ForeignComptimeCallLookup::UnexpectedReadyProjection,
        ];
        for lookup in failures {
            let mut failure_session = DurableComptimeSession::new(
                definition("parent"),
                crate::revisioned_query_database::declaration_candidate_for_stable_key(
                    &definition("parent"),
                )
                .unwrap(),
            )
            .unwrap();
            let failure_edge = failure_session.prepare_expression_edge(44).unwrap();
            let error = failure_session
                .consume_foreign_lookup(failure_edge, lookup)
                .expect_err("non-ready lookup must preserve its exact error channel");
            match error {
                DurableComptimeForeignCallError::ReadyFailure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(message),
                ) => assert_eq!(message.as_ref(), "shell"),
                DurableComptimeForeignCallError::ReadyQueryFailure(failure) => {
                    assert_eq!(failure.code.as_ref(), "query")
                }
                DurableComptimeForeignCallError::AdmissionFailure(
                    crate::body_query::ComptimeProgramProjectionFailure::IdentityMismatch,
                )
                | DurableComptimeForeignCallError::UnexpectedReadyProjection => {}
                other => panic!("wrong foreign lookup error channel: {other:?}"),
            }
            assert!(failure_session.drain_root_effects().unwrap().is_empty());
        }

        // The same pre-lookup edge can choose the admitted branch and derive
        // the child query only from the owned program payload.
        let mut lifecycle = lifecycle();
        let edge = lifecycle.prepare_expression_edge(42).unwrap();
        assert_eq!(edge.accessing_source(), &definition("parent"));
        let mut ticket = lifecycle
            .ticket_from_admitted_edge(edge, &admitted)
            .unwrap();
        assert!(lifecycle.active.is_empty());
        lifecycle.enter(&ticket).unwrap();
        assert_eq!(lifecycle.active.len(), 1);
        lifecycle
            .finish_with_effects(
                &mut ticket,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        lifecycle.complete_known().unwrap();

        let mut inconsistent = admitted.clone();
        Arc::make_mut(&mut inconsistent.core).plan.candidate = sibling;
        assert!(matches!(
            DurableComptimeCallContext::from_admitted_expression(
                &inconsistent,
                definition("parent"),
                crate::revisioned_query_database::declaration_candidate_for_stable_key(
                    &definition("parent")
                )
                .unwrap(),
                42,
            ),
            Err(DurableComptimeLifecycleError::InvalidContext)
        ));
    }

    #[test]
    fn entered_calls_merge_once_in_lifo_order_and_fill_deferred_application() {
        let mut lifecycle = lifecycle();
        let outer_context = context(3);
        let inner_context = context_with_parent(definition("child"), 4);
        let mut outer = lifecycle.prepare(outer_context).unwrap();
        lifecycle.enter(&outer).unwrap();
        let mut inner = lifecycle.prepare(inner_context).unwrap();
        lifecycle.enter(&inner).unwrap();
        let inner_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        assert_eq!(
            lifecycle.finish_with_effects(&mut inner, &inner_outcome, child_effects(4)),
            Ok(())
        );
        let outer_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        assert_eq!(
            lifecycle.finish_with_effects(&mut outer, &outer_outcome, child_effects(3)),
            Ok(())
        );
        let effects = lifecycle.complete_known().unwrap();
        let applications = effects
            .deferred_ownership()
            .map(|gate| {
                let application = gate.application.as_ref().unwrap();
                (application.declaration.clone(), application.call_ordinal)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            applications,
            vec![
                (
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &definition("parent"),
                    )
                    .unwrap(),
                    3,
                ),
                (
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &definition("child"),
                    )
                    .unwrap(),
                    4,
                ),
            ]
        );
    }

    #[test]
    fn structured_calls_preserve_missing_application_directly() {
        let mut lifecycle = lifecycle();
        let mut ticket = lifecycle.prepare(structured_context()).unwrap();
        lifecycle.enter(&ticket).unwrap();
        lifecycle
            .finish_with_effects(
                &mut ticket,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                child_effects(5),
            )
            .unwrap();
        let effects = lifecycle.complete_known().unwrap();
        assert!(
            effects
                .deferred_ownership()
                .next()
                .unwrap()
                .application
                .is_none()
        );
    }

    #[test]
    fn ready_projection_merges_at_root_with_its_edge_policy_without_a_ticket() {
        let mut expression = lifecycle();
        let mut expression_edge = expression.prepare_expression_edge(10).unwrap();
        expression
            .merge_ready_lookup(
                &mut expression_edge,
                ForeignComptimeCallLookup::Ready(ready_projection(10)),
            )
            .unwrap();
        let expression_effects = expression.complete_known().unwrap();
        assert_eq!(
            expression_effects
                .deferred_ownership()
                .next()
                .unwrap()
                .application
                .as_ref()
                .unwrap()
                .call_ordinal,
            10
        );

        let mut structured = lifecycle();
        let mut structured_edge = structured.prepare_structured_edge().unwrap();
        structured
            .merge_ready_projection(&mut structured_edge, &ready_projection(11))
            .unwrap();
        assert!(
            structured
                .complete_known()
                .unwrap()
                .deferred_ownership()
                .next()
                .unwrap()
                .application
                .is_none()
        );
    }

    #[test]
    fn ready_projection_uses_the_active_edge_policy_in_both_nested_directions() {
        let mut expression_outer = lifecycle();
        let mut outer = expression_outer.prepare(context(20)).unwrap();
        expression_outer.enter(&outer).unwrap();
        let mut inner_edge = expression_outer.prepare_structured_edge().unwrap();
        assert_eq!(inner_edge.accessing_source(), &definition("child"));
        expression_outer
            .merge_ready_projection(&mut inner_edge, &ready_projection(21))
            .unwrap();
        expression_outer
            .finish_with_effects(
                &mut outer,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        let effects = expression_outer.complete_known().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .next()
                .unwrap()
                .application
                .as_ref()
                .unwrap()
                .call_ordinal,
            20
        );

        let mut structured_outer = lifecycle();
        let mut outer = structured_outer.prepare(structured_context()).unwrap();
        structured_outer.enter(&outer).unwrap();
        let mut inner_edge = structured_outer.prepare_expression_edge(22).unwrap();
        assert_eq!(inner_edge.accessing_source(), &definition("child"));
        structured_outer
            .merge_ready_projection(&mut inner_edge, &ready_projection(22))
            .unwrap();
        structured_outer
            .finish_with_effects(
                &mut outer,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        let effects = structured_outer.complete_known().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .next()
                .unwrap()
                .application
                .as_ref()
                .unwrap()
                .call_ordinal,
            22
        );
    }

    #[test]
    fn ready_projection_is_dropped_when_the_active_outer_call_is_not_known() {
        let mut lifecycle = lifecycle();
        let mut outer = lifecycle.prepare(context(30)).unwrap();
        lifecycle.enter(&outer).unwrap();
        let mut inner_edge = lifecycle.prepare_structured_edge().unwrap();
        lifecycle
            .merge_ready_projection(&mut inner_edge, &ready_projection(31))
            .unwrap();
        lifecycle
            .finish_with_effects(
                &mut outer,
                &rue_air::ComptimeOutcome::<(), ()>::RuntimeDependent,
                DurableComptimeEffects::default(),
            )
            .unwrap();
        assert!(lifecycle.complete_known().unwrap().is_empty());
    }

    #[test]
    fn premature_root_drain_preserves_nested_ready_effects_until_parent_finishes() {
        let mut lifecycle = lifecycle();
        let mut outer = lifecycle.prepare(context(32)).unwrap();
        lifecycle.enter(&outer).unwrap();
        let mut inner = lifecycle.prepare_structured_edge().unwrap();
        lifecycle
            .merge_ready_projection(&mut inner, &ready_projection(33))
            .unwrap();

        assert_eq!(
            lifecycle.take_root_effects(),
            Err(DurableComptimeLifecycleError::OutOfOrder)
        );
        lifecycle
            .finish_with_effects(
                &mut outer,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        let effects = lifecycle.complete_known().unwrap();
        assert_eq!(effects.deferred_ownership().count(), 1);
        assert_eq!(
            effects
                .deferred_ownership()
                .next()
                .unwrap()
                .application
                .as_ref()
                .unwrap()
                .call_ordinal,
            32
        );
    }

    #[test]
    fn repeated_ready_projection_edges_preserve_distinct_expression_ordinals() {
        let mut lifecycle = lifecycle();
        for ordinal in [40, 41] {
            let mut edge = lifecycle.prepare_expression_edge(ordinal).unwrap();
            lifecycle
                .merge_ready_projection(&mut edge, &ready_projection(ordinal))
                .unwrap();
        }
        let applications = lifecycle
            .complete_known()
            .unwrap()
            .deferred_ownership()
            .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
            .collect::<Vec<_>>();
        assert_eq!(applications, vec![40, 41]);
    }

    #[test]
    fn non_ready_lookup_cannot_publish_or_consume_a_ready_edge() {
        let mut lifecycle = lifecycle();
        let mut edge = lifecycle.prepare_expression_edge(50).unwrap();
        assert_eq!(
            lifecycle.merge_ready_lookup(&mut edge, ForeignComptimeCallLookup::NotReady),
            Err(DurableComptimeLifecycleError::ReadyProjectionRequired)
        );
        lifecycle
            .merge_ready_lookup(
                &mut edge,
                ForeignComptimeCallLookup::Ready(ready_projection(50)),
            )
            .unwrap();
        assert_eq!(
            lifecycle.merge_ready_projection(&mut edge, &ready_projection(51)),
            Err(DurableComptimeLifecycleError::TicketReused)
        );
        assert_eq!(
            lifecycle
                .complete_known()
                .unwrap()
                .deferred_ownership()
                .count(),
            1
        );
    }

    #[test]
    fn wrong_lifecycle_ready_merge_preserves_the_edge_for_its_owner() {
        let mut owner = lifecycle();
        let mut other = lifecycle();
        let mut edge = owner.prepare_expression_edge(51).unwrap();
        let projection = ready_projection(51);

        assert_eq!(
            other.merge_ready_projection(&mut edge, &projection),
            Err(DurableComptimeLifecycleError::TicketMismatch)
        );
        assert!(other.complete_known().unwrap().is_empty());

        owner
            .merge_ready_projection(&mut edge, &projection)
            .unwrap();
        assert_eq!(
            owner.complete_known().unwrap().deferred_ownership().count(),
            1
        );
    }

    #[test]
    fn mixed_expression_and_structured_scopes_apply_only_at_expression_sites() {
        let parent_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "parent",
            ))
            .unwrap();
        let child_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "child",
            ))
            .unwrap();

        // Expression outer + structured inner: the outer expression supplies
        // the only application site to both direct and nested gates.
        let mut expression_lifecycle = lifecycle();
        let mut outer = expression_lifecycle.prepare(context(3)).unwrap();
        expression_lifecycle.enter(&outer).unwrap();
        let mut inner = expression_lifecycle
            .prepare(structured_context_with_parent(definition("child")))
            .unwrap();
        expression_lifecycle.enter(&inner).unwrap();
        expression_lifecycle
            .finish_with_effects(
                &mut inner,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                child_effects(4),
            )
            .unwrap();
        expression_lifecycle
            .finish_with_effects(
                &mut outer,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                child_effects(3),
            )
            .unwrap();
        let effects = expression_lifecycle.complete_known().unwrap();
        let applications = effects
            .deferred_ownership()
            .map(|gate| gate.application.clone().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(applications.len(), 2);
        assert!(applications.iter().all(|application| {
            application.declaration == parent_declaration && application.call_ordinal == 3
        }));

        // Structured outer + expression inner: the inner expression owns its
        // application, while the outer structured call preserves its direct
        // gate as unresolved.
        let mut structured_lifecycle = lifecycle();
        let mut outer = structured_lifecycle.prepare(structured_context()).unwrap();
        structured_lifecycle.enter(&outer).unwrap();
        let mut inner = structured_lifecycle
            .prepare(context_with_parent(definition("child"), 4))
            .unwrap();
        structured_lifecycle.enter(&inner).unwrap();
        structured_lifecycle
            .finish_with_effects(
                &mut inner,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                child_effects(4),
            )
            .unwrap();
        structured_lifecycle
            .finish_with_effects(
                &mut outer,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                child_effects(3),
            )
            .unwrap();
        let effects = structured_lifecycle.complete_known().unwrap();
        let mut applications = effects
            .deferred_ownership()
            .map(|gate| gate.application.clone());
        assert!(applications.next().unwrap().is_none());
        assert_eq!(
            applications.next().unwrap(),
            Some(
                crate::semantic_query_nucleus::DeferredOwnershipApplication {
                    declaration: child_declaration,
                    call_ordinal: 4,
                }
            )
        );

        // Structured nesting never manufactures an application.
        let mut structured_nested_lifecycle = lifecycle();
        let mut outer = structured_nested_lifecycle
            .prepare(structured_context())
            .unwrap();
        structured_nested_lifecycle.enter(&outer).unwrap();
        let mut inner = structured_nested_lifecycle
            .prepare(structured_context_with_parent(definition("child")))
            .unwrap();
        structured_nested_lifecycle.enter(&inner).unwrap();
        structured_nested_lifecycle
            .finish_with_effects(
                &mut inner,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                child_effects(4),
            )
            .unwrap();
        structured_nested_lifecycle
            .finish_with_effects(
                &mut outer,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                child_effects(3),
            )
            .unwrap();
        assert!(
            structured_nested_lifecycle
                .complete_known()
                .unwrap()
                .deferred_ownership()
                .all(|gate| gate.application.is_none())
        );
    }

    #[test]
    fn non_known_outer_outcome_drops_nested_accumulated_effects() {
        let mut lifecycle = lifecycle();
        let mut outer = lifecycle.prepare(context(3)).unwrap();
        lifecycle.enter(&outer).unwrap();
        let mut inner = lifecycle
            .prepare(structured_context_with_parent(definition("child")))
            .unwrap();
        lifecycle.enter(&inner).unwrap();
        lifecycle
            .finish_with_effects(
                &mut inner,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                child_effects(4),
            )
            .unwrap();
        lifecycle
            .finish_with_effects(
                &mut outer,
                &rue_air::ComptimeOutcome::<(), ()>::RuntimeDependent,
                child_effects(3),
            )
            .unwrap();
        assert!(lifecycle.complete_known().unwrap().is_empty());
    }

    #[test]
    fn every_non_known_outer_terminal_drops_nested_known_effects() {
        fn assert_dropped(outcome: rue_air::ComptimeOutcome<(), ()>) {
            let mut lifecycle = lifecycle();
            let mut outer = lifecycle.prepare(context(3)).unwrap();
            lifecycle.enter(&outer).unwrap();
            let mut inner = lifecycle
                .prepare(structured_context_with_parent(definition("child")))
                .unwrap();
            lifecycle.enter(&inner).unwrap();
            lifecycle
                .finish_with_effects(
                    &mut inner,
                    &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                    child_effects(4),
                )
                .unwrap();
            lifecycle
                .finish_with_effects(&mut outer, &outcome, child_effects(3))
                .unwrap();
            assert!(lifecycle.complete_known().unwrap().is_empty());
        }

        assert_dropped(rue_air::ComptimeOutcome::RuntimeDependent);
        assert_dropped(rue_air::ComptimeOutcome::NotReady);
        assert_dropped(rue_air::ComptimeOutcome::UnsupportedContext);
        assert_dropped(rue_air::ComptimeOutcome::Trap(rue_air::ComptimeTrap {
            operation: "test",
            span: rue_span::Span::new(0, 0),
        }));
        assert_dropped(rue_air::ComptimeOutcome::HostFailure(()));
        assert_dropped(rue_air::ComptimeOutcome::Abort(()));
    }

    #[test]
    fn expression_sibling_occurrences_keep_distinct_application_ordinals() {
        let mut lifecycle = lifecycle();
        for ordinal in [10, 11] {
            let mut ticket = lifecycle.prepare(context(ordinal)).unwrap();
            lifecycle.enter(&ticket).unwrap();
            lifecycle
                .finish_with_effects(
                    &mut ticket,
                    &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                    child_effects(ordinal),
                )
                .unwrap();
        }
        let applications = lifecycle
            .complete_known()
            .unwrap()
            .deferred_ownership()
            .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
            .collect::<Vec<_>>();
        assert_eq!(applications, vec![10, 11]);
    }

    #[test]
    fn nested_nominal_and_dependency_observations_merge_once() {
        let mut lifecycle = lifecycle();
        let mut outer = lifecycle.prepare(context(3)).unwrap();
        lifecycle.enter(&outer).unwrap();
        let mut inner = lifecycle
            .prepare(context_with_parent(definition("child"), 4))
            .unwrap();
        lifecycle.enter(&inner).unwrap();
        lifecycle
            .finish_with_effects(
                &mut inner,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                observed_effects(),
            )
            .unwrap();
        lifecycle
            .finish_with_effects(
                &mut outer,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                observed_effects(),
            )
            .unwrap();
        let effects = lifecycle.complete_known().unwrap();
        assert_eq!(effects.anonymous_nominals().count(), 1);
        assert_eq!(effects.dependencies().count(), 1);
    }

    #[test]
    fn preexisting_applications_survive_the_full_policy_matrix() {
        fn finish_pair(
            outer_context: DurableComptimeCallContext,
            inner_context: DurableComptimeCallContext,
        ) -> Vec<Option<crate::semantic_query_nucleus::DeferredOwnershipApplication>> {
            let mut lifecycle = lifecycle();
            let mut outer = lifecycle.prepare(outer_context).unwrap();
            lifecycle.enter(&outer).unwrap();
            let mut inner = lifecycle.prepare(inner_context).unwrap();
            lifecycle.enter(&inner).unwrap();
            let application_declaration =
                crate::revisioned_query_database::declaration_candidate_for_stable_key(
                    &definition("child"),
                )
                .unwrap();
            lifecycle
                .finish_with_effects(
                    &mut inner,
                    &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                    child_effects_with_application(4, application_declaration.clone(), 99),
                )
                .unwrap();
            lifecycle
                .finish_with_effects(
                    &mut outer,
                    &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                    child_effects_with_application(3, application_declaration, 99),
                )
                .unwrap();
            lifecycle
                .complete_known()
                .unwrap()
                .deferred_ownership()
                .map(|gate| gate.application.clone())
                .collect()
        }

        let expression = context(3);
        let expression_inner = context_with_parent(definition("child"), 4);
        let structured = structured_context();
        let structured_inner = structured_context_with_parent(definition("child"));
        let expected = Some(
            crate::semantic_query_nucleus::DeferredOwnershipApplication {
                declaration:
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &definition("child"),
                    )
                    .unwrap(),
                call_ordinal: 99,
            },
        );
        for applications in [
            finish_pair(expression.clone(), expression_inner.clone()),
            finish_pair(expression, structured_inner.clone()),
            finish_pair(structured.clone(), expression_inner),
            finish_pair(structured, structured_inner),
        ] {
            assert_eq!(applications, vec![expected.clone(), expected.clone()]);
        }
    }

    #[test]
    fn mismatched_order_rejection_does_not_publish_child_effects() {
        let mut lifecycle = lifecycle();
        let outer_context = context(1);
        let inner_context = context_with_parent(definition("child"), 2);
        let mut outer = lifecycle.prepare(outer_context).unwrap();
        lifecycle.enter(&outer).unwrap();
        lifecycle.observe_deferred_ownership(gate(1));
        let mut inner = lifecycle.prepare(inner_context).unwrap();
        lifecycle.enter(&inner).unwrap();
        let outer_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        let inner_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        let Err(error) = lifecycle.finish(&mut outer, &outer_outcome) else {
            panic!("out-of-order finish should return its inputs");
        };
        assert_eq!(error, DurableComptimeLifecycleError::OutOfOrder);
        assert_eq!(
            lifecycle
                .root_effects_for_test()
                .deferred_ownership()
                .count(),
            0
        );
        lifecycle
            .finish_with_effects(&mut inner, &inner_outcome, child_effects(2))
            .unwrap();
        lifecycle.finish(&mut outer, &outer_outcome).unwrap();

        let parent_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "parent",
            ))
            .unwrap();
        let child_declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "child",
            ))
            .unwrap();
        let effects = lifecycle.complete_known().unwrap();
        let applications = effects
            .deferred_ownership()
            .map(|gate| {
                let application = gate.application.as_ref().unwrap();
                (application.declaration.clone(), application.call_ordinal)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            applications,
            vec![(parent_declaration, 1), (child_declaration, 2)]
        );
    }

    #[test]
    fn prepared_ticket_can_be_dropped_and_non_known_outcomes_do_not_publish() {
        let mut lifecycle = lifecycle();
        let prepared_context = context(0);
        let prepared = lifecycle.prepare(prepared_context).unwrap();
        drop(prepared);
        let prepared_context = context(0);
        let mut prepared = lifecycle.prepare(prepared_context).unwrap();
        let prepared_outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        let Err(error) = lifecycle.finish(&mut prepared, &prepared_outcome) else {
            panic!("prepared finish should be rejected");
        };
        assert_eq!(error, DurableComptimeLifecycleError::NotEntered);

        let ticket_context = context(1);
        let mut ticket = lifecycle.prepare(ticket_context).unwrap();
        lifecycle.enter(&ticket).unwrap();
        assert_eq!(
            lifecycle.enter(&ticket),
            Err(DurableComptimeLifecycleError::TicketReused)
        );
        let abort_outcome = rue_air::ComptimeOutcome::<(), ()>::Abort(());
        lifecycle
            .finish_with_effects(&mut ticket, &abort_outcome, child_effects(1))
            .unwrap();
        assert_eq!(
            lifecycle
                .root_effects_for_test()
                .deferred_ownership()
                .count(),
            0
        );
    }

    #[test]
    fn rejected_finish_and_cross_owner_attempts_preserve_recovery() {
        let mut lifecycle = lifecycle();
        let mut other = DurableComptimeCallLifecycle::new(
            definition("other"),
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&definition(
                "other",
            ))
            .unwrap(),
        )
        .unwrap();
        let mut ticket = lifecycle.prepare(context(8)).unwrap();
        lifecycle.enter(&ticket).unwrap();
        let outcome = rue_air::ComptimeOutcome::<(), ()>::Known(());
        let Err(error) = other.finish(&mut ticket, &outcome) else {
            panic!("cross-owner finish should be rejected");
        };
        assert_eq!(error, DurableComptimeLifecycleError::TicketMismatch);
        let active_outcome = rue_air::ComptimeOutcome::<(), &str>::Abort("active-root");
        let Err((returned_lifecycle, returned_outcome, error)) =
            lifecycle.complete_root(active_outcome)
        else {
            panic!("active lifecycle must not finish as a root");
        };
        lifecycle = returned_lifecycle;
        assert_eq!(error, DurableComptimeLifecycleError::OutOfOrder);
        assert!(matches!(
            returned_outcome,
            rue_air::ComptimeOutcome::Abort("active-root")
        ));
        lifecycle
            .finish_with_effects(&mut ticket, &outcome, child_effects(8))
            .unwrap();
        assert_eq!(
            lifecycle
                .complete_known()
                .unwrap()
                .deferred_ownership()
                .count(),
            1
        );
    }

    #[test]
    fn prepared_parent_slot_prevents_reordered_entry() {
        let mut lifecycle = lifecycle();
        let mut first = lifecycle
            .prepare(context_with_parent_and_child(
                definition("parent"),
                definition("child_a"),
                30,
            ))
            .unwrap();
        let mut second = lifecycle
            .prepare(context_with_parent_and_child(
                definition("parent"),
                definition("child_b"),
                31,
            ))
            .unwrap();
        lifecycle.enter(&second).unwrap();
        assert_eq!(
            lifecycle.enter(&first),
            Err(DurableComptimeLifecycleError::InvalidContext)
        );
        lifecycle
            .finish_with_effects(
                &mut second,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        lifecycle.enter(&first).unwrap();
        lifecycle
            .finish_with_effects(
                &mut first,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        assert!(lifecycle.complete_known().unwrap().is_empty());
    }

    #[test]
    fn all_non_known_terminals_cleanup_without_publishing_effects() {
        fn assert_not_published(outcome: rue_air::ComptimeOutcome<(), ()>) {
            let mut lifecycle = lifecycle();
            let mut ticket = lifecycle.prepare(context(20)).unwrap();
            lifecycle.enter(&ticket).unwrap();
            lifecycle
                .finish_with_effects(&mut ticket, &outcome, child_effects(20))
                .unwrap();
            assert!(lifecycle.complete_known().unwrap().is_empty());
        }

        assert_not_published(rue_air::ComptimeOutcome::RuntimeDependent);
        assert_not_published(rue_air::ComptimeOutcome::NotReady);
        assert_not_published(rue_air::ComptimeOutcome::UnsupportedContext);
        assert_not_published(rue_air::ComptimeOutcome::Trap(rue_air::ComptimeTrap {
            operation: "test",
            span: rue_span::Span::new(0, 0),
        }));
        assert_not_published(rue_air::ComptimeOutcome::HostFailure(()));
        assert_not_published(rue_air::ComptimeOutcome::Abort(()));
    }

    #[test]
    fn preexisting_deferred_application_is_not_rewritten() {
        let mut lifecycle = lifecycle();
        let mut ticket = lifecycle.prepare(context(21)).unwrap();
        lifecycle.enter(&ticket).unwrap();
        let mut effects = DurableComptimeEffects::default();
        let mut gate = gate(21);
        gate.application = Some(
            crate::semantic_query_nucleus::DeferredOwnershipApplication {
                declaration:
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &definition("child"),
                    )
                    .unwrap(),
                call_ordinal: 99,
            },
        );
        effects.observe_deferred_ownership(gate);
        lifecycle
            .finish_with_effects(
                &mut ticket,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                effects,
            )
            .unwrap();
        let effects = lifecycle.complete_known().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .next()
                .unwrap()
                .application
                .as_ref()
                .unwrap()
                .call_ordinal,
            99
        );
    }

    #[test]
    fn invalid_root_identity_is_rejected_before_ticket_issuance() {
        let wrong = crate::revisioned_query_database::declaration_candidate_for_stable_key(
            &definition("child"),
        )
        .unwrap();
        assert!(matches!(
            DurableComptimeCallLifecycle::new(definition("parent"), wrong),
            Err(DurableComptimeLifecycleError::InvalidContext)
        ));
    }

    #[test]
    fn active_root_cannot_publish_effects_until_children_are_finished() {
        let mut lifecycle = lifecycle();
        let context = context(0);
        let mut ticket = lifecycle.prepare(context).unwrap();
        lifecycle.enter(&ticket).unwrap();
        let active_outcome = rue_air::ComptimeOutcome::<(), &str>::HostFailure("active-root");
        let Err((returned_lifecycle, returned_outcome, error)) =
            lifecycle.complete_root(active_outcome)
        else {
            panic!("active lifecycle must not finish as a root");
        };
        lifecycle = returned_lifecycle;
        assert_eq!(error, DurableComptimeLifecycleError::OutOfOrder);
        assert!(matches!(
            returned_outcome,
            rue_air::ComptimeOutcome::HostFailure("active-root")
        ));
        lifecycle
            .finish_with_effects(
                &mut ticket,
                &rue_air::ComptimeOutcome::<(), ()>::Known(()),
                DurableComptimeEffects::default(),
            )
            .unwrap();
        assert!(lifecycle.complete_known().unwrap().is_empty());
    }
}
