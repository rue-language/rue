//! Stable per-body query values and independently stamped projections.

use std::sync::Arc;

use rue_query::QueryKey;

/// Exact owned syntax requested by the registered body-input evaluator. The
/// stable owner is the only identity carried with the syntax; parser and RIR
/// handles are deliberately confined to the evaluator-local lowering helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedBodyInput {
    pub(crate) owner: crate::StableDefinitionKey,
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
    Lowering(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyInputValue {
    Available(OwnedBodyInput),
    Incomplete(BodyInputIncomplete),
}

pub(crate) fn body_input_equal(left: &BodyInputValue, right: &BodyInputValue) -> bool {
    left == right
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
    Definition(crate::StableDefinitionKey),
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

impl WellKnownOptionResolution {
    pub(crate) fn is_empty(&self) -> bool {
        self.option_by_payload.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyReferences(pub(crate) Arc<[BodyReference]>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyProducedAnonymousNominals(
    pub(crate) Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum BodyLookupKey {
    Name {
        module: crate::ModuleId,
        namespace: BodyLookupNamespace,
        name: Arc<str>,
    },
    Member {
        module: crate::ModuleId,
        owner_name: Arc<str>,
        member_name: Arc<str>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum BodyLookupNamespace {
    ModuleItem,
    Destructor,
}

/// The `body-produced-anonymous` family's terminal value.
///
/// A producer either publishes the anonymous nominals it owns (`Produced`) or
/// its comptime evaluation committed an internal-error (E9000-class) failure —
/// notably an anonymous-anchor TRANSPORT invariant violation (RUE-1089). Such a
/// committed internal error is a corrupt-input fact about an existing raw
/// fragment terminal, NOT a "not yet available" condition, so it must never be
/// downgraded to a retryable `Canceled` abort: doing so let a consuming body
/// silently fall back to recomputing the identity from RIR, masking the defect.
/// It is carried here so every consumer fails closed on it. Genuine
/// unavailability still surfaces as a `Canceled` abort, never as this value.
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
        lookup_observations: Arc<[BodyLookupKey]>,
    },
    DeterministicFailure {
        errors: crate::CompileErrors,
        references: BodyReferences,
        lookup_observations: Arc<[BodyLookupKey]>,
    },
}

impl BodyTransaction {
    pub(crate) fn references(&self) -> &BodyReferences {
        match self {
            Self::Success { references, .. } | Self::DeterministicFailure { references, .. } => {
                references
            }
        }
    }

    pub(crate) fn lookup_observations(&self) -> &[BodyLookupKey] {
        match self {
            Self::Success {
                lookup_observations,
                ..
            }
            | Self::DeterministicFailure {
                lookup_observations,
                ..
            } => lookup_observations,
        }
    }

    pub(crate) fn install_lookup_observations(&mut self, observations: Arc<[BodyLookupKey]>) {
        match self {
            Self::Success {
                lookup_observations,
                ..
            }
            | Self::DeterministicFailure {
                lookup_observations,
                ..
            } => *lookup_observations = observations,
        }
    }
}

pub(crate) fn transaction_equal(left: &BodyTransaction, right: &BodyTransaction) -> bool {
    match (left, right) {
        (
            BodyTransaction::Success {
                body: left_body,
                references: left_references,
                produced_anonymous_nominals: left_produced,
                ..
            },
            BodyTransaction::Success {
                body: right_body,
                references: right_references,
                produced_anonymous_nominals: right_produced,
                ..
            },
        ) => {
            left_body == right_body
                && left_references == right_references
                && left_produced == right_produced
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
        ) => {
            left_errors.to_string() == right_errors.to_string()
                && left_references == right_references
        }
        _ => false,
    }
}
