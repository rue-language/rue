//! Stable per-body query values and independently stamped projections.

use std::sync::Arc;

use rue_query::QueryKey;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyReferences(pub(crate) Arc<[BodyReference]>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyProducedAnonymousNominals(
    pub(crate) Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
);

#[derive(Debug, Clone)]
pub(crate) enum BodyTransaction {
    Success {
        body: Box<CanonicalBody>,
        references: BodyReferences,
        produced_anonymous_nominals: BodyProducedAnonymousNominals,
    },
    DeterministicFailure {
        errors: crate::CompileErrors,
        references: BodyReferences,
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
}

pub(crate) fn transaction_equal(left: &BodyTransaction, right: &BodyTransaction) -> bool {
    match (left, right) {
        (
            BodyTransaction::Success {
                body: left_body,
                references: left_references,
                produced_anonymous_nominals: left_produced,
            },
            BodyTransaction::Success {
                body: right_body,
                references: right_references,
                produced_anonymous_nominals: right_produced,
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
            },
            BodyTransaction::DeterministicFailure {
                errors: right_errors,
                references: right_references,
            },
        ) => {
            left_errors.to_string() == right_errors.to_string()
                && left_references == right_references
        }
        _ => false,
    }
}
