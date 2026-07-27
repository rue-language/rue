//! Stable per-body query values and independently stamped projections.

use std::sync::Arc;

use rue_query::QueryKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodySourceLocator {
    pub(crate) file_id: rue_span::FileId,
    pub(crate) physical_path: Arc<str>,
    pub(crate) source_length: u32,
    pub(crate) declaration_start: u32,
    pub(crate) declaration_end: u32,
    pub(crate) body_start: u32,
    pub(crate) body_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BodyRelativeRange {
    pub(crate) start: u32,
    pub(crate) end: u32,
}

pub(crate) fn body_source_locator_equal(
    left: &Option<BodySourceLocator>,
    right: &Option<BodySourceLocator>,
) -> bool {
    left == right
}

pub(crate) fn body_source_basis_equal(
    left: &Option<BodySourceLocator>,
    right: &Option<BodySourceLocator>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            left.file_id == right.file_id && left.physical_path == right.physical_path
        }
        (None, None) => true,
        _ => false,
    }
}

/// Exact owned syntax requested by the registered body-input evaluator. The
/// stable owner and its request-local source locator are the only identities
/// carried with the syntax; parser and RIR handles remain confined to the
/// evaluator-local lowering helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedBodyInput {
    pub(crate) owner: crate::StableDefinitionKey,
    pub(crate) source: BodySourceLocator,
    pub(crate) signature: crate::declaration_candidate::RawDeclarationSignatureSyntax,
    pub(crate) body: crate::declaration_candidate::RawDeclarationBodySyntax,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyInputIncomplete {
    UnsupportedInstance,
    UnsupportedKind(crate::StableDefinitionKind),
    Generic,
    Extern,
    MissingPrerequisite(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyInputValue {
    Available(OwnedBodyInput),
    Incomplete(BodyInputIncomplete),
}

pub(crate) fn body_input_equal(left: &BodyInputValue, right: &BodyInputValue) -> bool {
    match (left, right) {
        (BodyInputValue::Available(left), BodyInputValue::Available(right)) => {
            left.owner == right.owner
                && left.source.file_id == right.source.file_id
                && left.source.physical_path == right.source.physical_path
                && left.signature == right.signature
                && left.body == right.body
        }
        (BodyInputValue::Incomplete(left), BodyInputValue::Incomplete(right)) => left == right,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BodyQueryKey {
    pub(crate) instance: crate::FunctionInstanceKey,
    pub(crate) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
}

impl QueryKey for BodyQueryKey {
    fn stable_identity(&self) -> String {
        format!("{:?}:{:?}", self.instance, self.configuration)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalBody {
    Ordinary {
        owner: crate::StableDefinitionKey,
        body: rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
    },
    Anonymous {
        identity: crate::FunctionInstanceKey,
        body_anchor: BodyRelativeRange,
        body: rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
    },
    Specialization {
        identity:
            rue_air::SemanticSpecializationIdentity<crate::StableDefinitionKey, crate::ModuleId>,
        body: rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
        dependencies: Arc<[crate::StableDefinitionKey]>,
        dependency_boundary_complete: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum BodyReference {
    Callable(crate::FunctionInstanceKey),
    #[allow(dead_code)]
    Definition(crate::StableDefinitionKey),
    #[allow(dead_code)]
    Type(crate::TypeInstanceKey),
}

/// The per-body resolution of the well-known `Option` demands: the resolved
/// enum for each demanded payload, plus every anonymous nominal to materialize
/// narrowly. Empty only when a body contains no fallible intrinsic. A body with
/// demands reaches analysis only after every exact trusted specialization has
/// resolved successfully.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WellKnownOptionResolution {
    pub(crate) option_by_payload: Arc<
        [(
            crate::durable_semantics::DurableType,
            crate::durable_semantics::DurableType,
        )],
    >,
    pub(crate) anonymous_nominals: Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyReferences(pub(crate) Arc<[BodyReference]>);

/// Descriptor-only record of the exact lookup terminals consulted while
/// analyzing one body. Pin ownership is deliberately absent: the registered
/// evaluator hands pins directly into the session publication lease before its
/// request-scoped lease can end. Keeping only identities here lets retained
/// `BodyTransaction` memo terminals remain ordinary semantic values instead of
/// silently co-owning lookup retention.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BodyLookupObservations {
    pub(crate) terminals: Arc<[(crate::revisioned_query_database::LookupObservationKey, u64)]>,
}

/// Query-local control outcomes that are not semantic body terminals.
///
/// These values travel through the registered query result itself so the
/// request boundary cannot race a revision/key side table when distinguishing
/// an ordinary cancellation from a domain-specific deferral.
#[derive(Debug, Clone)]
pub(crate) enum BodyTransactionControl {
    DeferredAnonymousProducers(Arc<[crate::FunctionInstanceKey]>),
    ProducerFailed(Box<crate::semantic_query_nucleus::SemanticNucleusFailure>),
    WellKnownOptionResolution(WellKnownOptionResolutionFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WellKnownOptionResolutionFailure {
    Incomplete {
        payload: crate::well_known_option::FalliblePayload,
        prerequisite: Option<crate::StableDefinitionKey>,
        detail: Arc<str>,
    },
    Semantic {
        payload: crate::well_known_option::FalliblePayload,
        failure: Box<crate::semantic_query_nucleus::SemanticNucleusFailure>,
    },
    WrongProjection {
        payload: crate::well_known_option::FalliblePayload,
        detail: Arc<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyProducedAnonymousNominals(
    pub(crate) Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
);

/// Exact anonymous facts supplied to a provider-backed body by its registered
/// prerequisites. They are not produced by the body, but final import-only
/// composition must materialize them alongside declaration-level facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyConsultedAnonymousNominals(
    pub(crate) Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
);

/// The `body-produced-anonymous` family's terminal value.
///
/// A producer either publishes the anonymous nominals it owns (`Produced`) or
/// its comptime evaluation commits a deterministic semantic failure. The latter
/// includes ordinary source diagnostics as well as internal anchor-transport
/// invariant failures (RUE-1089). Both are stable facts about the producer and
/// must remain typed query values; downgrading either to retryable `Canceled`
/// would turn a source error into an uncanceled request abort or let a consumer
/// silently rescue a corrupt identity. Genuine unavailability still surfaces
/// as a query abort, never as this value.
#[derive(Debug, Clone)]
pub(crate) enum ProducedAnonymous {
    Produced(BodyProducedAnonymousNominals),
    ProducerFailed(Box<crate::semantic_query_nucleus::SemanticNucleusFailure>),
}

pub(crate) fn produced_anonymous_equal(
    left: &ProducedAnonymous,
    right: &ProducedAnonymous,
) -> bool {
    match (left, right) {
        (ProducedAnonymous::Produced(left), ProducedAnonymous::Produced(right)) => left == right,
        (ProducedAnonymous::ProducerFailed(left), ProducedAnonymous::ProducerFailed(right)) => {
            left == right
        }
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub(crate) enum BodyTransaction {
    Success {
        body: Box<CanonicalBody>,
        references: BodyReferences,
        produced_anonymous_nominals: BodyProducedAnonymousNominals,
        consulted_anonymous_nominals: BodyConsultedAnonymousNominals,
        lookup_observations: BodyLookupObservations,
    },
    DeterministicFailure {
        errors: crate::CompileErrors,
        diagnostic_basis: Option<BodySourceLocator>,
        references: BodyReferences,
        lookup_observations: BodyLookupObservations,
    },
    Control(BodyTransactionControl),
}

#[derive(Debug, Clone)]
pub(crate) struct BodyAnalysisBundle {
    pub(crate) transaction: BodyTransaction,
    pub(crate) produced_anonymous: Option<ProducedAnonymous>,
    pub(crate) source_locator: Option<BodySourceLocator>,
}

pub(crate) fn analysis_bundle_equal(left: &BodyAnalysisBundle, right: &BodyAnalysisBundle) -> bool {
    let transaction_equal = match (&left.transaction, &right.transaction) {
        (
            BodyTransaction::DeterministicFailure {
                errors: left_errors,
                references: left_references,
                ..
            },
            BodyTransaction::DeterministicFailure {
                errors: right_errors,
                references: right_references,
                ..
            },
        ) => left_errors == right_errors && left_references == right_references,
        (left, right) => transaction_equal(left, right),
    };
    transaction_equal
        && left.source_locator == right.source_locator
        && match (&left.produced_anonymous, &right.produced_anonymous) {
            (Some(left), Some(right)) => produced_anonymous_equal(left, right),
            (None, None) => true,
            _ => false,
        }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BodyClosureQueryKey {
    pub(crate) modules: Arc<[crate::ModuleId]>,
    pub(crate) roots: Arc<[crate::FunctionInstanceKey]>,
    pub(crate) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
}

impl rue_query::QueryKey for BodyClosureQueryKey {
    fn stable_identity(&self) -> String {
        format!(
            "modules={:?};roots={:?};target={:?};preview={:?}",
            self.modules,
            self.roots,
            self.configuration.target,
            self.configuration.preview_features,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BodyClosurePublicationKey {
    pub(crate) closure: BodyClosureQueryKey,
    pub(crate) epoch: u64,
}

impl rue_query::QueryKey for BodyClosurePublicationKey {
    fn stable_identity(&self) -> String {
        format!("{};epoch={}", self.closure.stable_identity(), self.epoch)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BodyClosureBody {
    pub(crate) key: BodyQueryKey,
    pub(crate) bundle: Arc<rue_query::QueryTerminal<BodyAnalysisBundle>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyClosureFatal {
    DeclarationFailed {
        declaration: Option<crate::declaration_candidate::DeclarationCandidateKey>,
        failure: Box<crate::semantic_query_nucleus::SemanticNucleusFailure>,
    },
    BodyAvailability {
        instance: crate::FunctionInstanceKey,
        detail: Arc<str>,
    },
    ProducerFailed {
        instance: crate::FunctionInstanceKey,
        failure: Box<crate::semantic_query_nucleus::SemanticNucleusFailure>,
    },
    WellKnownOptionResolution {
        instance: crate::FunctionInstanceKey,
        failure: crate::revisioned_query_database::WellKnownOptionResolutionFailure,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct BodyClosureOutput {
    pub(crate) bodies: Arc<[BodyClosureBody]>,
    pub(crate) scheduling_errors: Arc<[(crate::FunctionInstanceKey, crate::CompileErrors)]>,
    pub(crate) fatal: Option<BodyClosureFatal>,
    pub(crate) parked_toolchain: Option<crate::ParkedToolchainModules>,
}

pub(crate) fn body_closure_output_equal(
    left: &BodyClosureOutput,
    right: &BodyClosureOutput,
) -> bool {
    left.bodies.len() == right.bodies.len()
        && left
            .bodies
            .iter()
            .zip(right.bodies.iter())
            .all(|(left, right)| {
                left.key == right.key
                    && match (left.bundle.outcome(), right.bundle.outcome()) {
                        (
                            rue_query::QueryOutcome::Success(left),
                            rue_query::QueryOutcome::Success(right),
                        ) => analysis_bundle_equal(left, right),
                        (
                            rue_query::QueryOutcome::Failure(left),
                            rue_query::QueryOutcome::Failure(right),
                        ) => left == right,
                        _ => false,
                    }
            })
        && left.scheduling_errors == right.scheduling_errors
        && left.fatal == right.fatal
        && left.parked_toolchain == right.parked_toolchain
}

impl BodyTransaction {
    pub(crate) fn references(&self) -> &BodyReferences {
        match self {
            Self::Success { references, .. } | Self::DeterministicFailure { references, .. } => {
                references
            }
            Self::Control(_) => {
                unreachable!("control outcomes are unwrapped at the request boundary")
            }
        }
    }

    pub(crate) fn lookup_observations(&self) -> Option<&BodyLookupObservations> {
        match self {
            Self::Success {
                lookup_observations,
                ..
            }
            | Self::DeterministicFailure {
                lookup_observations,
                ..
            } => Some(lookup_observations),
            Self::Control(_) => None,
        }
    }

    pub(crate) fn attach_provider_observations(
        mut self,
        lookup_observations: BodyLookupObservations,
        selected_references: impl IntoIterator<Item = BodyReference>,
    ) -> Self {
        match &mut self {
            Self::Success {
                references,
                lookup_observations: stored,
                ..
            }
            | Self::DeterministicFailure {
                references,
                lookup_observations: stored,
                ..
            } => {
                let mut merged = references
                    .0
                    .iter()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>();
                merged.extend(selected_references);
                references.0 = merged.into_iter().collect::<Vec<_>>().into();
                *stored = lookup_observations;
            }
            Self::Control(_) => {}
        }
        self
    }
}

pub(crate) fn transaction_equal(left: &BodyTransaction, right: &BodyTransaction) -> bool {
    match (left, right) {
        (
            BodyTransaction::Success {
                body: left_body,
                references: left_references,
                produced_anonymous_nominals: left_produced,
                consulted_anonymous_nominals: left_consulted,
                ..
            },
            BodyTransaction::Success {
                body: right_body,
                references: right_references,
                produced_anonymous_nominals: right_produced,
                consulted_anonymous_nominals: right_consulted,
                ..
            },
        ) => {
            left_body == right_body
                && left_references == right_references
                && left_produced == right_produced
                && left_consulted == right_consulted
        }
        (
            BodyTransaction::DeterministicFailure {
                errors: left_errors,
                references: left_references,
                ..
            },
            BodyTransaction::DeterministicFailure {
                errors: right_errors,
                references: right_references,
                ..
            },
        ) => left_errors == right_errors && left_references == right_references,
        (
            BodyTransaction::Control(BodyTransactionControl::DeferredAnonymousProducers(left)),
            BodyTransaction::Control(BodyTransactionControl::DeferredAnonymousProducers(right)),
        ) => left == right,
        (
            BodyTransaction::Control(BodyTransactionControl::ProducerFailed(left)),
            BodyTransaction::Control(BodyTransactionControl::ProducerFailed(right)),
        ) => left == right,
        (
            BodyTransaction::Control(BodyTransactionControl::WellKnownOptionResolution(left)),
            BodyTransaction::Control(BodyTransactionControl::WellKnownOptionResolution(right)),
        ) => left == right,
        _ => false,
    }
}
