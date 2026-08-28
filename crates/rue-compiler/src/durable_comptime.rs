//! Durable compile-time values and the compiler-side authority adapter for
//! the canonical AIR comptime engine.
//!
//! The value cases in this module are the durable semantic representation;
//! AIR owns instruction evaluation while this module supplies host policy.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ahash::AHashMap;
use lasso::Key;
use rue_air::{
    ComptimeFile, ComptimeIdentity, ComptimeMatchPattern, ComptimeName, ComptimeSemanticRejection,
    ComptimeTargetIntrinsic, ComptimeType, ComptimeUnaryOperation, ComptimeValue,
};
#[cfg(test)]
use rue_query::CancellationToken;
use rue_query::QueryAbort;

use crate::ModuleId;
use crate::body_query::ForeignComptimeCallLookup;
use crate::declaration_candidate::{DeclarationCandidateKey, DeclarationImportFailure};
use crate::durable_semantics::{DurableConstValue, DurableType};
use crate::semantic_query_nucleus::SemanticNucleusFailure;

type DurableAnonymousNominal = crate::durable_semantics::DurableAnonymousNominal;
type SemanticDeclarationDependency = crate::semantic_query_nucleus::SemanticDeclarationDependency;
type DeferredOwnershipGate = crate::semantic_query_nucleus::DeferredOwnershipGate;
type DeferredOwnershipApplication = crate::semantic_query_nucleus::DeferredOwnershipApplication;

/// A revision-independent diagnostic location owned by the declaration that
/// produced a durable comptime fact.  Durable failures must carry this
/// semantic location rather than borrowing a caller span (or an instruction
/// reference) from the revision that happened to observe them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeDiagnosticSite {
    producer: DeclarationCandidateKey,
    start: u32,
    end: u32,
}

impl DurableComptimeDiagnosticSite {
    pub(crate) fn new(producer: DeclarationCandidateKey, start: u32, end: u32) -> Self {
        Self {
            producer,
            start,
            end,
        }
    }
}

/// The durable boundary distinguishes query control from a committed
/// semantic failure.  In particular, cancellation and missing inputs remain
/// aborts and are never converted into diagnostics or memoized failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableComptimeFailure {
    Abort(QueryAbort),
    Failure(Box<SemanticNucleusFailure>),
}

/// The AIR error channel has one failure type for both tagged branches.  This
/// opaque payload keeps query aborts and semantic failures distinct; the
/// canonical constructors below always pair each payload with its matching
/// AIR outer tag.  AIR's public error enum still permits arbitrary payloads in
/// principle, so this guarantee is enforced by this durable funnel rather
/// than claimed as a property of the AIR type itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeHostFailure(DurableComptimeHostFailureKind);

#[derive(Debug, Clone, PartialEq, Eq)]
enum DurableComptimeHostFailureKind {
    Semantic(Box<SemanticNucleusFailure>),
    QueryFailure(rue_query::QueryFailure),
    QueryAbort(QueryAbort),
}

impl DurableComptimeHostFailure {
    fn semantic(failure: Box<SemanticNucleusFailure>) -> Self {
        Self(DurableComptimeHostFailureKind::Semantic(failure))
    }

    fn query_abort(abort: QueryAbort) -> Self {
        Self(DurableComptimeHostFailureKind::QueryAbort(abort))
    }

    fn query_failure(failure: rue_query::QueryFailure) -> Self {
        Self(DurableComptimeHostFailureKind::QueryFailure(failure))
    }

    fn into_host_error(self) -> rue_air::ComptimeHostError<Self> {
        match &self.0 {
            DurableComptimeHostFailureKind::Semantic(_) => {
                rue_air::ComptimeHostError::HostFailure(self)
            }
            DurableComptimeHostFailureKind::QueryFailure(_) => {
                rue_air::ComptimeHostError::HostFailure(self)
            }
            DurableComptimeHostFailureKind::QueryAbort(_) => {
                rue_air::ComptimeHostError::Abort(self)
            }
        }
    }

    /// Split the host's terminal channel at a declaration query root. AIR
    /// keeps semantic failures and retained query failures in the same
    /// `HostFailure` variant, while the query family must publish those two
    /// outcomes through different outer channels.
    pub(crate) fn into_root_host_failure(
        self,
    ) -> Result<Box<SemanticNucleusFailure>, rue_query::QueryFailure> {
        match self.0 {
            DurableComptimeHostFailureKind::Semantic(failure) => Ok(failure),
            DurableComptimeHostFailureKind::QueryFailure(failure) => Err(failure),
            DurableComptimeHostFailureKind::QueryAbort(_) => {
                unreachable!("query aborts use the AIR Abort outcome")
            }
        }
    }

    pub(crate) fn into_root_abort(self) -> QueryAbort {
        match self.0 {
            DurableComptimeHostFailureKind::QueryAbort(abort) => abort,
            DurableComptimeHostFailureKind::Semantic(_)
            | DurableComptimeHostFailureKind::QueryFailure(_) => {
                unreachable!("semantic and retained query failures use HostFailure")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn semantic_failure(&self) -> Option<&SemanticNucleusFailure> {
        match &self.0 {
            DurableComptimeHostFailureKind::Semantic(failure) => Some(failure),
            DurableComptimeHostFailureKind::QueryFailure(_)
            | DurableComptimeHostFailureKind::QueryAbort(_) => None,
        }
    }
}

impl DurableComptimeFailure {
    pub(crate) fn failure(failure: SemanticNucleusFailure) -> Self {
        Self::Failure(Box::new(failure))
    }

    pub(crate) fn abort(abort: QueryAbort) -> Self {
        Self::Abort(abort)
    }

    pub(crate) fn resolution(message: impl Into<Arc<str>>) -> Self {
        Self::failure(SemanticNucleusFailure::Resolution(message.into()))
    }

    pub(crate) fn comptime_failure(reason: impl Into<String>) -> Self {
        Self::failure(SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::ComptimeEvaluationFailed {
                reason: reason.into(),
            },
        ))
    }

    /// The exact durable terminal used when a comptime match reaches no
    /// selected arm. This remains a resolution failure, matching the
    /// established declaration-time behavior.
    /// evaluator's existing `comptime match has no selected arm` policy.
    pub(crate) fn comptime_match_no_selected_arm() -> Self {
        Self::resolution("comptime match has no selected arm")
    }

    /// Map the AIR-owned semantic rejection vocabulary to the exact durable
    /// declaration-time diagnostics. Ordinary AIR hosts intentionally keep
    /// these same reasons runtime-dependent.
    pub(crate) fn comptime_rejection(
        rejection: ComptimeSemanticRejection<EvaluatedSemanticConst>,
    ) -> Self {
        match rejection {
            ComptimeSemanticRejection::ConditionNotBoolean(value) => match value {
                EvaluatedSemanticConst::Module(_) => {
                    Self::resolution("module used where a value is required")
                }
                EvaluatedSemanticConst::TargetEnum(_) => Self::resolution(
                    "target descriptor used where a durable const value is required",
                ),
                EvaluatedSemanticConst::Value(_) => {
                    Self::resolution("comptime condition is not boolean")
                }
            },
            ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                operation: _operation,
                lhs,
                rhs,
            } => {
                let values = [Some(lhs.clone()), rhs.clone()];
                let target_count = values
                    .iter()
                    .flatten()
                    .filter(|value| matches!(value, EvaluatedSemanticConst::TargetEnum(_)))
                    .count();
                if rhs.is_some() && target_count == 2 {
                    return Self::resolution(
                        "target descriptors support only equality comparisons",
                    );
                }
                if rhs.is_some() && target_count == 1 {
                    return Self::resolution(
                        "target descriptor comparison requires matching enum variants",
                    );
                }
                for value in [Some(lhs), rhs.clone()].into_iter().flatten() {
                    match value {
                        EvaluatedSemanticConst::Module(_) => {
                            return Self::resolution("module used where a value is required");
                        }
                        EvaluatedSemanticConst::TargetEnum(_) => {
                            return Self::resolution(
                                "target descriptor used where a durable const value is required",
                            );
                        }
                        EvaluatedSemanticConst::Value(_) => {}
                    }
                }
                let bool_count = values
                    .iter()
                    .flatten()
                    .filter(|value| {
                        matches!(
                            value,
                            EvaluatedSemanticConst::Value(value)
                                if matches!(value.value, DurableConstValue::Bool(_))
                        )
                    })
                    .count();
                if rhs.is_some() && bool_count == 2 {
                    return Self::resolution("boolean values support only equality comparisons");
                }
                Self::resolution("comptime arithmetic operand is not an integer")
            }
            ComptimeSemanticRejection::UnaryOperandNotInteger(value) => match value {
                EvaluatedSemanticConst::Module(_) => {
                    Self::resolution("module used where a value is required")
                }
                EvaluatedSemanticConst::TargetEnum(_) => Self::resolution(
                    "target descriptor used where a durable const value is required",
                ),
                EvaluatedSemanticConst::Value(_) => {
                    Self::resolution("comptime arithmetic operand is not an integer")
                }
            },
            ComptimeSemanticRejection::UnaryTypeNotInteger { operation, value } => match value {
                EvaluatedSemanticConst::Module(_) => {
                    Self::resolution("module used where a value is required")
                }
                EvaluatedSemanticConst::TargetEnum(_) => Self::resolution(
                    "target descriptor used where a durable const value is required",
                ),
                EvaluatedSemanticConst::Value(_) => match operation {
                    ComptimeUnaryOperation::Neg => {
                        Self::resolution("comptime negation operand is not an integer")
                    }
                    ComptimeUnaryOperation::BitNot => {
                        Self::resolution("comptime bitwise NOT operand is not an integer")
                    }
                },
            },
            ComptimeSemanticRejection::Assignment => {
                Self::resolution("assignment is not supported in declaration-time comptime")
            }
            ComptimeSemanticRejection::AggregateExpression => Self::failure(
                SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::ConstExprNotSupported {
                    expr_kind: "aggregate expression".to_owned(),
                }),
            ),
            ComptimeSemanticRejection::EmptyBlock => {
                Self::resolution("comptime block has no result instruction")
            }
            ComptimeSemanticRejection::UnsupportedIntrinsic(name) => Self::failure(
                SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::ConstExprNotSupported {
                    expr_kind: format!("intrinsic `@{name}`"),
                }),
            ),
            ComptimeSemanticRejection::UnsupportedExpression => {
                Self::resolution("expression is not supported in declaration-time comptime")
            }
        }
    }

    pub(crate) fn maximum_depth(name: &str, maximum: usize) -> Self {
        Self::comptime_failure(format!(
            "specialization of '{name}' exceeded the maximum nesting depth ({maximum}); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?"
        ))
    }

    pub(crate) fn integer_literal_overflow(type_name: &str, value: i128) -> Self {
        Self::comptime_failure(format!(
            "integer overflow evaluating constant at type {type_name}: value {value} is out of range for type {type_name}; {value} does not fit in {type_name} (this operation would panic at runtime)"
        ))
    }

    pub(crate) fn arithmetic_overflow(type_name: &str, operation: &str, detail: &str) -> Self {
        Self::comptime_failure(format!(
            "integer overflow evaluating {operation} at type {type_name}: {detail} (this operation would panic at runtime)"
        ))
    }

    /// Construct a diagnostic anchored to the owning declaration.  The site
    /// is explicit so nested/foreign evaluation cannot accidentally label the
    /// failure with the ambient caller's span.
    pub(crate) fn comptime_failure_at(
        site: &DurableComptimeDiagnosticSite,
        reason: impl Into<String>,
    ) -> Self {
        Self::diagnostic_at_site(site, reason.into())
    }

    fn diagnostic_at_site(site: &DurableComptimeDiagnosticSite, reason: String) -> Self {
        Self::failure(SemanticNucleusFailure::DiagnosticAtProducerRange {
            kind: rue_error::ErrorKind::ComptimeEvaluationFailed { reason },
            producer: site.producer.clone(),
            start: site.start,
            end: site.end,
        })
    }

    /// Adapt a supported AIR arithmetic trap using the owning declaration and
    /// the trap's own span.  Unsupported operations remain unsupported rather
    /// than acquiring a new diagnostic spelling.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn trap_at(
        producer: DeclarationCandidateKey,
        trap: rue_air::ComptimeTrap,
    ) -> Option<Self> {
        let site = DurableComptimeDiagnosticSite::new(producer, trap.span.start, trap.span.end);
        let reason = arithmetic_trap_reason(trap.operation)?;
        Some(Self::diagnostic_at_site(&site, reason.to_owned()))
    }

    /// Adapt a durable terminal directly for the AIR host.  Provider
    /// errors use `provider_error_as_host` instead, so this seam never creates
    /// a durable failure only to convert it back into a provider error.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn into_host_error(self) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
        match self {
            Self::Failure(failure) => {
                DurableComptimeHostFailure::semantic(failure).into_host_error()
            }
            Self::Abort(abort) => DurableComptimeHostFailure::query_abort(abort).into_host_error(),
        }
    }

    /// Build the AIR error directly from a provider result.  This is the
    /// AIR host funnel; the query-root adapter below only unwraps its matching
    /// outer channel and never normalizes crossed tags.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn provider_error_as_host(
        error: rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    ) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
        match error {
            rue_air::SemanticProviderError::Abort(abort) => {
                DurableComptimeHostFailure::query_abort(abort).into_host_error()
            }
            rue_air::SemanticProviderError::Failure(failure) => {
                DurableComptimeHostFailure::semantic(Box::new(failure)).into_host_error()
            }
        }
    }
}

fn arithmetic_trap_reason(operation: &str) -> Option<&'static str> {
    match operation {
        "division by zero" => Some("division by zero (this operation would panic at runtime)"),
        "remainder by zero" => Some("remainder by zero (this operation would panic at runtime)"),
        _ => None,
    }
}

/// The ownership-site policy is issued with an admitted call rather than
/// inferred while finishing it. Expression calls may attribute still-deferred
/// gates to their parent call; structured type calls deliberately preserve
/// missing applications for an enclosing expression call to fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableComptimeApplicationPolicy {
    Preserve,
    ApplyAtParentCall {
        application: DeferredOwnershipApplication,
    },
}

impl DurableComptimeApplicationPolicy {
    pub(crate) fn preserve() -> Self {
        Self::Preserve
    }

    pub(crate) fn apply_at_parent_call(
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
        call_ordinal: u32,
    ) -> Self {
        Self::ApplyAtParentCall {
            application: DeferredOwnershipApplication {
                declaration,
                call_ordinal,
            },
        }
    }

    fn application(&self) -> Option<DeferredOwnershipApplication> {
        match self {
            Self::Preserve => None,
            Self::ApplyAtParentCall { application } => Some(application.clone()),
        }
    }
}

/// Effects observed while reducing one durable comptime root.
///
/// The collections deliberately use the same canonical keys and ordering as
/// the semantic nucleus. Anonymous nominals replace an earlier observation at
/// the same identity, while dependencies and ownership gates are set-unioned;
/// this is the existing publication behavior, expressed as one operation
/// boundary for the AIR host.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DurableComptimeEffects {
    anonymous_nominals: BTreeMap<crate::AnonymousNominalKey, DurableAnonymousNominal>,
    dependencies: BTreeSet<SemanticDeclarationDependency>,
    deferred_ownership: BTreeSet<DeferredOwnershipGate>,
}

impl DurableComptimeEffects {
    pub(crate) fn observe_anonymous_nominal(&mut self, nominal: DurableAnonymousNominal) {
        self.anonymous_nominals
            .insert(nominal.identity.clone(), nominal);
    }

    pub(crate) fn observe_dependency(&mut self, dependency: SemanticDeclarationDependency) {
        self.dependencies.insert(dependency);
    }

    pub(crate) fn observe_deferred_ownership(&mut self, gate: DeferredOwnershipGate) {
        self.deferred_ownership.insert(gate);
    }

    pub(crate) fn merge_child(
        &mut self,
        child: DurableComptimeEffects,
        policy: &DurableComptimeApplicationPolicy,
    ) {
        merge_effects_into(
            &mut self.anonymous_nominals,
            &mut self.dependencies,
            &mut self.deferred_ownership,
            child.anonymous_nominals.into_values(),
            child.dependencies,
            child.deferred_ownership,
            policy.application(),
        );
    }

    pub(crate) fn merge_projection(
        &mut self,
        anonymous_nominals: &[DurableAnonymousNominal],
        dependencies: &[SemanticDeclarationDependency],
        deferred_ownership: &[DeferredOwnershipGate],
        policy: &DurableComptimeApplicationPolicy,
    ) {
        merge_effects_into(
            &mut self.anonymous_nominals,
            &mut self.dependencies,
            &mut self.deferred_ownership,
            anonymous_nominals.iter().cloned(),
            dependencies.iter().cloned(),
            deferred_ownership.iter().cloned(),
            policy.application(),
        );
    }

    #[allow(dead_code)] // publication adapters consume the canonical projection directly
    pub(crate) fn anonymous_nominals(&self) -> impl Iterator<Item = &DurableAnonymousNominal> {
        self.anonymous_nominals.values()
    }

    #[allow(dead_code)] // publication adapters consume the canonical projection directly
    pub(crate) fn dependencies(&self) -> impl Iterator<Item = &SemanticDeclarationDependency> {
        self.dependencies.iter()
    }

    #[allow(dead_code)] // publication adapters consume the canonical projection directly
    pub(crate) fn deferred_ownership(&self) -> impl Iterator<Item = &DeferredOwnershipGate> {
        self.deferred_ownership.iter()
    }

    #[allow(dead_code)] // root publication owns the empty-result fast path
    pub(crate) fn is_empty(&self) -> bool {
        self.anonymous_nominals.is_empty()
            && self.dependencies.is_empty()
            && self.deferred_ownership.is_empty()
    }

    pub(crate) fn apply_to(
        self,
        anonymous_nominals: &mut BTreeMap<crate::AnonymousNominalKey, DurableAnonymousNominal>,
        dependencies: &mut BTreeSet<SemanticDeclarationDependency>,
        deferred_ownership: &mut BTreeSet<DeferredOwnershipGate>,
        policy: &DurableComptimeApplicationPolicy,
    ) {
        merge_effects_into(
            anonymous_nominals,
            dependencies,
            deferred_ownership,
            self.anonymous_nominals.into_values(),
            self.dependencies,
            self.deferred_ownership,
            policy.application(),
        );
    }
}

/// The one canonical publication kernel for durable comptime effects.
/// Every root-local merge, borrowed projection merge, and provider publication
/// uses this operation so nominal replacement, set union, and deferred
/// application filling cannot drift between entry points.
fn merge_effects_into(
    anonymous_nominals: &mut BTreeMap<crate::AnonymousNominalKey, DurableAnonymousNominal>,
    dependencies: &mut BTreeSet<SemanticDeclarationDependency>,
    deferred_ownership: &mut BTreeSet<DeferredOwnershipGate>,
    observed_nominals: impl IntoIterator<Item = DurableAnonymousNominal>,
    observed_dependencies: impl IntoIterator<Item = SemanticDeclarationDependency>,
    observed_deferred: impl IntoIterator<Item = DeferredOwnershipGate>,
    application: Option<DeferredOwnershipApplication>,
) {
    for nominal in observed_nominals {
        anonymous_nominals.insert(nominal.identity.clone(), nominal);
    }
    dependencies.extend(observed_dependencies);
    deferred_ownership.extend(observed_deferred.into_iter().map(|mut gate| {
        if gate.application.is_none() {
            gate.application = application.clone();
        }
        gate
    }));
}

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

/// Engine-shaped semantic input for one anonymous nominal.
///
/// The AIR engine decodes RIR into this descriptor. Keeping the descriptor
/// independent of RIR lets the durable
/// AIR host reuse the exact identity, shape, mode, capture, and effect policy
/// without acquiring a second instruction dispatcher.
#[derive(Debug, Clone)]
pub(crate) struct DurableAnonymousNominalDescriptor {
    /// The producer canonicalizes this key before crossing the boundary. The
    /// kernel validates and preserves it verbatim.
    pub(crate) identity: crate::AnonymousNominalKey,
    pub(crate) shape: DurableAnonymousNominalDescriptorShape,
    pub(crate) type_captures: Arc<[(Arc<str>, DurableType)]>,
    pub(crate) value_captures: Arc<[(Arc<str>, DurableConstValue)]>,
}

#[derive(Debug, Clone)]
pub(crate) enum DurableAnonymousNominalDescriptorShape {
    Struct {
        fields: Arc<[rue_air::ComptimeField<Arc<str>, DurableType>]>,
        methods: Arc<[rue_air::ComptimeMethodDescriptor<Arc<str>, DurableType>]>,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[DurableType]>)]>,
    },
}

/// Construct and publish one anonymous nominal through the durable session's
/// effect authority.  The returned type is the same nominal identity whose
/// complete shape and captures are observed by the session.
pub(crate) fn project_durable_anonymous_nominal(
    session: &mut DurableComptimeSession,
    descriptor: DurableAnonymousNominalDescriptor,
) -> Result<DurableType, DurableComptimeFailure> {
    let expected_kind = match &descriptor.shape {
        DurableAnonymousNominalDescriptorShape::Struct { .. } => {
            rue_air::AnonymousNominalKind::Struct
        }
        DurableAnonymousNominalDescriptorShape::Enum { .. } => rue_air::AnonymousNominalKind::Enum,
    };
    if descriptor.identity.kind != expected_kind {
        return Err(DurableComptimeFailure::resolution(format!(
            "anonymous nominal identity kind {:?} does not match {:?} descriptor shape",
            descriptor.identity.kind, expected_kind
        )));
    }
    let type_captures = canonicalize_captures(descriptor.type_captures, "type")?;
    let value_captures = canonicalize_captures(descriptor.value_captures, "value")?;
    let shape = match descriptor.shape {
        DurableAnonymousNominalDescriptorShape::Struct { fields, methods } => {
            let method_type = |ty: rue_air::ComptimeMethodType<DurableType>| match ty {
                rue_air::ComptimeMethodType::SelfType => {
                    Ok(crate::durable_semantics::DurableAnonymousMethodType::SelfType)
                }
                rue_air::ComptimeMethodType::Concrete(ty) => {
                    Ok(crate::durable_semantics::DurableAnonymousMethodType::Concrete(ty))
                }
                rue_air::ComptimeMethodType::Unsupported(shape) => {
                    Err(DurableComptimeFailure::resolution(format!(
                        "unsupported anonymous method type: {shape}"
                    )))
                }
            };
            let methods = methods
                .iter()
                .map(|method| {
                    let parameters = method
                        .parameters
                        .iter()
                        .map(|parameter| {
                            Ok((
                                method_type(parameter.ty.clone())?,
                                durable_parameter_mode(parameter.mode),
                                parameter.is_comptime,
                            ))
                        })
                        .collect::<Result<Vec<_>, DurableComptimeFailure>>()?;
                    Ok(crate::durable_semantics::DurableAnonymousMethodSignature {
                        name: method.name.clone(),
                        has_self: method.has_self,
                        self_mode: durable_parameter_mode(method.self_mode),
                        returns_borrow: method.returns_borrow,
                        returns_inout: method.returns_inout,
                        parameters: parameters.into(),
                        result: method_type(method.result.clone())?,
                        has_body: true,
                    })
                })
                .collect::<Result<Vec<_>, DurableComptimeFailure>>()?;
            crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                fields: fields
                    .iter()
                    .map(|field| (field.name.clone(), field.ty.clone()))
                    .collect(),
                methods: methods.into(),
            }
        }
        DurableAnonymousNominalDescriptorShape::Enum { variants } => {
            crate::durable_semantics::DurableAnonymousNominalShape::Enum { variants }
        }
    };
    session.observe_anonymous_nominal(DurableAnonymousNominal::new(
        descriptor.identity.clone(),
        shape,
        type_captures,
        value_captures,
    ));
    Ok(DurableType::AnonymousNominal(descriptor.identity))
}

fn canonicalize_captures<T: Clone>(
    captures: Arc<[(Arc<str>, T)]>,
    kind: &str,
) -> Result<Arc<[(Arc<str>, T)]>, DurableComptimeFailure> {
    let mut captures = captures.iter().cloned().collect::<Vec<_>>();
    captures.sort_by(|left, right| left.0.cmp(&right.0));
    for pair in captures.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(DurableComptimeFailure::resolution(format!(
                "duplicate {kind} capture `{}` in anonymous nominal",
                pair[0].0
            )));
        }
    }
    Ok(captures.into())
}

fn durable_parameter_mode(
    mode: rue_rir::RirParamMode,
) -> crate::durable_semantics::DurableParameterMode {
    match mode {
        rue_rir::RirParamMode::Normal => crate::durable_semantics::DurableParameterMode::Value,
        rue_rir::RirParamMode::Borrow => crate::durable_semantics::DurableParameterMode::Borrow,
        rue_rir::RirParamMode::Inout => crate::durable_semantics::DurableParameterMode::Inout,
    }
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

    fn observe_anonymous_nominal(&mut self, nominal: DurableAnonymousNominal) {
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
    pub(crate) fn lifecycle_mut(&mut self) -> &mut DurableComptimeCallLifecycle {
        &mut self.lifecycle
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

/// Convert the canonical type-instance representation into the durable type
/// domain used by call binding. This is kept beside the binding policy so
/// diagnostics and substitution never acquire a second local conversion.
pub(crate) fn durable_type_from_instance_key(
    value: &crate::TypeInstanceKey,
) -> Option<DurableType> {
    use crate::TypeInstanceKey as T;
    use crate::durable_semantics::DurableType as D;
    Some(match value {
        T::I8 => D::I8,
        T::I16 => D::I16,
        T::I32 => D::I32,
        T::I64 => D::I64,
        T::U8 => D::U8,
        T::U16 => D::U16,
        T::U32 => D::U32,
        T::U64 => D::U64,
        T::Bool => D::Bool,
        T::Unit => D::Unit,
        T::Never => D::Never,
        T::ComptimeType => D::ComptimeType,
        T::BuiltinNominal { kind, name } => D::BuiltinNominal {
            name: name.clone(),
            kind: match kind {
                crate::AnonymousNominalKind::Struct => rue_air::SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
            },
        },
        T::Nominal(crate::NominalInstanceKey::Builtin { kind, name }) => D::BuiltinNominal {
            name: name.clone(),
            kind: match kind {
                crate::AnonymousNominalKind::Struct => rue_air::SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
            },
        },
        T::Nominal(crate::NominalInstanceKey::Named(key)) => D::Nominal(key.clone()),
        T::Nominal(crate::NominalInstanceKey::Anonymous(key)) => {
            D::AnonymousNominal((**key).clone())
        }
        T::Array { element, len } => D::Array {
            element: Arc::new(durable_type_from_instance_key(element)?),
            len: *len,
        },
        T::Slice { element, name } => D::Slice {
            element: Arc::new(durable_type_from_instance_key(element)?),
            name: name.clone(),
        },
        T::PtrConst(value) => D::PtrConst(Arc::new(durable_type_from_instance_key(value)?)),
        T::PtrMut(value) => D::PtrMut(Arc::new(durable_type_from_instance_key(value)?)),
        T::Module(value) => D::Module(value.clone()),
        T::GenericParameter(index) => D::GenericParameter(*index),
    })
}

fn durable_type_diagnostic_name_kernel(ty: &DurableType) -> String {
    use crate::durable_semantics::DurableType as T;

    fn function_name(function: &crate::FunctionInstanceKey) -> Option<&str> {
        match function {
            crate::FunctionInstanceKey::Definition(key) => Some(key.name()),
            crate::FunctionInstanceKey::Specialization { base, .. } => function_name(base),
            crate::FunctionInstanceKey::AnonymousMember { .. }
            | crate::FunctionInstanceKey::DropGlue(_) => None,
        }
    }

    match ty {
        T::I8 => "i8".to_owned(),
        T::I16 => "i16".to_owned(),
        T::I32 => "i32".to_owned(),
        T::I64 => "i64".to_owned(),
        T::U8 => "u8".to_owned(),
        T::U16 => "u16".to_owned(),
        T::U32 => "u32".to_owned(),
        T::U64 => "u64".to_owned(),
        T::Bool => "bool".to_owned(),
        T::Unit => "()".to_owned(),
        T::Never => "!".to_owned(),
        T::ComptimeType => "type".to_owned(),
        T::BuiltinNominal { name, .. } => name.to_string(),
        T::Nominal(key) => key.name().to_owned(),
        T::AnonymousNominal(key) => match &key.producer {
            crate::StableProducerId::Definition(key) => key.name().to_owned(),
            crate::StableProducerId::Function(function) => {
                let name = function_name(function).unwrap_or("anonymous");
                let applied = key.producer_arguments();
                let mut arguments = applied
                    .map(|applied| {
                        applied
                            .types
                            .iter()
                            .filter_map(durable_type_from_instance_key)
                            .map(|ty| durable_type_diagnostic_name(&ty))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                arguments.extend(
                    applied
                        .into_iter()
                        .flat_map(|applied| applied.values.iter())
                        .map(|value| match value {
                            crate::CanonicalArgumentValue::Integer(value) => value.to_string(),
                            crate::CanonicalArgumentValue::Bool(value) => value.to_string(),
                            crate::CanonicalArgumentValue::Type(value) => {
                                durable_type_from_instance_key(value.as_ref()).map_or_else(
                                    || "type".to_owned(),
                                    |ty| durable_type_diagnostic_name(&ty),
                                )
                            }
                            crate::CanonicalArgumentValue::Function(_) => "function".to_owned(),
                            crate::CanonicalArgumentValue::Unit => "()".to_owned(),
                            crate::CanonicalArgumentValue::String(value) => format!("\"{value}\""),
                        }),
                );
                if arguments.is_empty() {
                    name.to_owned()
                } else {
                    format!("{name}({})", arguments.join(", "))
                }
            }
        },
        T::Array { element, len } => {
            format!("[{}; {len}]", durable_type_diagnostic_name(element))
        }
        T::Slice { name, .. } => name.to_string(),
        T::PtrConst(pointee) => {
            format!("ptr const {}", durable_type_diagnostic_name(pointee))
        }
        T::PtrMut(pointee) => format!("ptr mut {}", durable_type_diagnostic_name(pointee)),
        T::Module(module) => module.to_string(),
        T::GenericParameter(index) => format!("T{index}"),
    }
}

pub(crate) fn durable_type_diagnostic_name(ty: &DurableType) -> String {
    DurableComptimeScalarPolicy::type_name(ty)
}

/// Render an anonymous producer with the declaration-relative comptime
/// parameter schema. `CanonicalArguments` intentionally stores type and value
/// streams separately for identity; the callable schema is the only authority
/// that can safely interleave those streams for presentation.
pub(crate) fn durable_type_diagnostic_name_with_parameters(
    ty: &DurableType,
    parameters: &[crate::durable_semantics::DurableSemanticParameter],
) -> String {
    let DurableType::AnonymousNominal(identity) = ty else {
        return durable_type_diagnostic_name(ty);
    };
    let crate::StableProducerId::Function(function) = &identity.producer else {
        return durable_type_diagnostic_name(ty);
    };
    let definition = match function.as_ref() {
        crate::FunctionInstanceKey::Definition(definition) => Some(definition),
        crate::FunctionInstanceKey::Specialization { base, .. } => {
            fn base_definition(
                function: &crate::FunctionInstanceKey,
            ) -> Option<&crate::StableDefinitionKey> {
                match function {
                    crate::FunctionInstanceKey::Definition(definition) => Some(definition),
                    crate::FunctionInstanceKey::Specialization { base, .. } => {
                        base_definition(base)
                    }
                    crate::FunctionInstanceKey::AnonymousMember { .. }
                    | crate::FunctionInstanceKey::DropGlue(_) => None,
                }
            }
            base_definition(base)
        }
        crate::FunctionInstanceKey::AnonymousMember { .. }
        | crate::FunctionInstanceKey::DropGlue(_) => None,
    };
    let Some(definition) = definition else {
        return durable_type_diagnostic_name(ty);
    };
    let parameters = parameters
        .iter()
        .map(|parameter| rue_air::CanonicalDisplayParameter {
            is_comptime: parameter.is_comptime,
            is_type: matches!(
                parameter.ty,
                crate::durable_semantics::DurableType::ComptimeType
            ),
        });
    rue_air::format_canonical_application(
        definition.name(),
        parameters,
        identity.producer_arguments(),
        |argument| {
            durable_type_from_instance_key(argument)
                .map(|argument| durable_type_diagnostic_name(&argument))
        },
    )
    .unwrap_or_else(|| durable_type_diagnostic_name(ty))
}

pub(crate) fn inferred_durable_const_type_name(value: &DurableConstValue) -> &'static str {
    match value {
        DurableConstValue::Integer(value) if i32::try_from(*value).is_ok() => "i32",
        DurableConstValue::Integer(value) if i64::try_from(*value).is_ok() => "i64",
        DurableConstValue::Integer(_) => "u64",
        DurableConstValue::Bool(_) => "bool",
        DurableConstValue::Unit => "()",
        DurableConstValue::String(_) => "str",
        DurableConstValue::Type(_) | DurableConstValue::Function(_) => "type",
    }
}

pub(crate) fn substitute_durable_generics(
    ty: &DurableType,
    type_arguments: &[DurableType],
) -> DurableType {
    use crate::durable_semantics::DurableType as T;
    match ty {
        T::GenericParameter(index) => type_arguments
            .get(*index as usize)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        T::Array { element, len } => T::Array {
            element: Arc::new(substitute_durable_generics(element, type_arguments)),
            len: *len,
        },
        T::Slice { element, name } => T::Slice {
            element: Arc::new(substitute_durable_generics(element, type_arguments)),
            name: name.clone(),
        },
        T::PtrConst(pointee) => T::PtrConst(Arc::new(substitute_durable_generics(
            pointee,
            type_arguments,
        ))),
        T::PtrMut(pointee) => T::PtrMut(Arc::new(substitute_durable_generics(
            pointee,
            type_arguments,
        ))),
        _ => ty.clone(),
    }
}

pub(crate) fn durable_const_fits_type(value: &DurableConstValue, ty: &DurableType) -> bool {
    use crate::durable_semantics::{DurableConstValue as V, DurableType as T};
    match (ty, value) {
        (_, V::Integer(value)) => {
            durable_int_width(ty).is_some_and(|integer| integer.fits_i128(*value))
        }
        (T::Bool, V::Bool(_)) | (T::Unit, V::Unit) => true,
        (T::ComptimeType, V::Type(_)) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableComptimeValueFitFailure {
    CallableAlias,
    IntegerOutOfRange { value: i128, type_name: String },
    TypeMismatch { expected: String, found: String },
}

/// Return the canonical durable value-fit classification, if a reduced value
/// cannot satisfy its declared comptime parameter type. Both expression
/// binding and structured type-constructor reduction consume this policy so
/// callable aliases, integer ranges, and mismatch diagnostics cannot drift.
pub(crate) fn durable_value_fit_failure(
    value: &DurableConstValue,
    expected: &DurableType,
) -> Option<DurableComptimeValueFitFailure> {
    if durable_const_fits_type(value, expected) {
        return None;
    }
    if matches!(value, DurableConstValue::Function(_)) {
        return Some(DurableComptimeValueFitFailure::CallableAlias);
    }
    if let DurableConstValue::Integer(value) = value
        && durable_int_width(expected).is_some()
    {
        return Some(DurableComptimeValueFitFailure::IntegerOutOfRange {
            value: *value,
            type_name: durable_type_diagnostic_name(expected),
        });
    }
    Some(DurableComptimeValueFitFailure::TypeMismatch {
        expected: durable_type_diagnostic_name(expected),
        found: inferred_durable_const_type_name(value).to_owned(),
    })
}

/// Map the shared value-fit classification to the exact semantic channel used
/// by structured durable calls.  Consumers may add presentation-specific
/// wrappers, but they must not reimplement this mapping.
pub(crate) fn durable_structured_value_fit_failure(
    value: &DurableConstValue,
    expected: &DurableType,
) -> Option<SemanticNucleusFailure> {
    durable_value_fit_failure(value, expected).map(|failure| match failure {
        DurableComptimeValueFitFailure::CallableAlias => SemanticNucleusFailure::Resolution(
            Arc::from("a callable alias cannot be passed as a comptime value argument"),
        ),
        DurableComptimeValueFitFailure::IntegerOutOfRange { value, type_name } => {
            SemanticNucleusFailure::Resolution(Arc::from(format!(
                "value {value} is outside the range of type {type_name}"
            )))
        }
        DurableComptimeValueFitFailure::TypeMismatch { expected, found } => {
            SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::TypeMismatch {
                expected,
                found,
            })
        }
    })
}

pub(crate) fn durable_int_width(
    ty: &DurableType,
) -> Option<rue_air::integer_semantics::IntegerType> {
    use rue_air::integer_semantics::IntegerType;
    let (bits, signed) = match ty {
        DurableType::I8 => (8, true),
        DurableType::I16 => (16, true),
        DurableType::I32 => (32, true),
        DurableType::I64 => (64, true),
        DurableType::U8 => (8, false),
        DurableType::U16 => (16, false),
        DurableType::U32 => (32, false),
        DurableType::U64 => (64, false),
        _ => return None,
    };
    IntegerType::new(bits, signed)
}

/// Stateless scalar policy shared by declaration-time evaluation and the
/// AIR durable host.  It owns no query or RIR state; all inputs are
/// already-reduced durable values and types.
pub(crate) struct DurableComptimeScalarPolicy;

impl DurableComptimeScalarPolicy {
    pub(crate) fn type_name(ty: &DurableType) -> String {
        durable_type_diagnostic_name_kernel(ty)
    }

    #[allow(dead_code)] // activated by the canonical durable AIR host
    pub(crate) fn type_is_unsigned(ty: &DurableType) -> bool {
        Self::type_integer_semantics(ty).is_some_and(|integer| integer.is_unsigned())
    }

    pub(crate) fn type_integer_semantics(
        ty: &DurableType,
    ) -> Option<rue_air::integer_semantics::IntegerType> {
        durable_int_width(ty)
    }

    pub(crate) fn integer_operation_type(
        expected: Option<&DurableType>,
        left: Option<&DurableType>,
        right: Option<&DurableType>,
    ) -> Result<DurableType, DurableComptimeFailure> {
        let fallback = expected
            .filter(|ty| durable_int_width(ty).is_some())
            .cloned()
            .unwrap_or(DurableType::I32);
        match (left, right) {
            (Some(left), Some(right)) if left != right => Err(DurableComptimeFailure::failure(
                SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::TypeMismatch {
                    expected: durable_type_diagnostic_name(left),
                    found: durable_type_diagnostic_name(right),
                }),
            )),
            (Some(ty), _) | (_, Some(ty)) => Ok(ty.clone()),
            (None, None) => Ok(fallback),
        }
    }

    pub(crate) fn unary_integer_type(
        expected: Option<&DurableType>,
        operand: Option<&DurableType>,
    ) -> Result<DurableType, DurableComptimeFailure> {
        Self::integer_operation_type(expected, operand, None)
    }

    pub(crate) fn require_integer_fits(
        ty: &DurableType,
        value: i128,
    ) -> Result<(), DurableComptimeFailure> {
        let integer = DurableConstValue::Integer(value);
        if durable_const_fits_type(&integer, ty) {
            return Ok(());
        }
        Err(DurableComptimeFailure::integer_literal_overflow(
            &durable_type_diagnostic_name(ty),
            value,
        ))
    }

    pub(crate) fn checked_integer_result(
        ty: &DurableType,
        result: rue_air::integer_semantics::CheckedIntegerResult,
        operation: &str,
    ) -> Result<i128, DurableComptimeFailure> {
        let Some(value) = result.checked() else {
            let type_name = durable_type_diagnostic_name(ty);
            let detail = result.raw().map_or_else(
                || format!("the result does not fit in {type_name}"),
                |value| {
                    format!(
                        "value {value} is out of range for type {type_name}; {value} does not fit in {type_name}"
                    )
                },
            );
            return Err(DurableComptimeFailure::arithmetic_overflow(
                &type_name, operation, &detail,
            ));
        };
        Ok(value)
    }
}

/// Integer-bound policy consumed after AIR has classified the intrinsic. It
/// owns durable diagnostics and integer semantics but no spelling table or
/// instruction/RIR authority.
pub(crate) struct DurableComptimeTypeIntrinsicPolicy;

impl DurableComptimeTypeIntrinsicPolicy {
    pub(crate) fn integer_bound(
        bound: rue_air::ComptimeIntegerBound,
        ty: &DurableType,
    ) -> Result<i128, DurableComptimeFailure> {
        let Some(integer) = DurableComptimeScalarPolicy::type_integer_semantics(ty) else {
            return Err(DurableComptimeFailure::failure(
                SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::IntrinsicTypeMismatch(
                    Box::new(rue_error::IntrinsicTypeMismatchError {
                        name: bound.as_str().to_owned(),
                        expected: "an integer type".to_owned(),
                        found: ty.kind().display_name().to_owned(),
                    }),
                )),
            ));
        };
        Ok(match bound {
            rue_air::ComptimeIntegerBound::Max => integer.max_i128(),
            rue_air::ComptimeIntegerBound::Min => integer.min_i128(),
        })
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
            type_arguments: &self.type_arguments,
            value_arguments: &self.value_arguments,
        }
    }
}

/// One-shot borrowed query facts retained by a pending prepared call.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct DurableComptimeBoundCallQuery<'a> {
    type_arguments: &'a [(Arc<str>, DurableType)],
    value_arguments: &'a [(Arc<str>, DurableConstValue)],
}

impl<'a> DurableComptimeBoundCallQuery<'a> {
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
    fn query_view(&self) -> DurableComptimeBoundCallQuery<'_> {
        self.bound.query_view()
    }
}

/// A non-replayable result of exactly one foreign probe. Raw lookup variants
/// never escape this package and cannot be retried without reconstructing the
/// consumed admission, edge, and bound call.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct DurableComptimeProbedCall {
    pending: DurableComptimePendingCall,
    lookup: ForeignComptimeCallLookup,
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

/// The exact durable projection of one named value lookup.  The dependency is
/// deliberately direct: resolving a const, callable, or nominal observes the
/// resolved declaration only, while any transitive const effects remain owned
/// by the const query that produced its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeNamedValueProjection {
    value: EvaluatedSemanticConst,
    dependency: SemanticDeclarationDependency,
    anonymous_nominals: Arc<[DurableAnonymousNominal]>,
}

/// The only declaration kinds considered by durable named-value lookup, in
/// the same order as the established semantic lookup policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeNamedValueKind {
    Const,
    Function,
    Struct,
    Enum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeNamedValueOrder {
    Unqualified,
    ModuleMember,
}

const DURABLE_COMPTIME_UNQUALIFIED_VALUE_KINDS: [DurableComptimeNamedValueKind; 4] = [
    DurableComptimeNamedValueKind::Const,
    DurableComptimeNamedValueKind::Function,
    DurableComptimeNamedValueKind::Struct,
    DurableComptimeNamedValueKind::Enum,
];
const DURABLE_COMPTIME_MODULE_MEMBER_KINDS: [DurableComptimeNamedValueKind; 4] = [
    DurableComptimeNamedValueKind::Const,
    DurableComptimeNamedValueKind::Struct,
    DurableComptimeNamedValueKind::Enum,
    DurableComptimeNamedValueKind::Function,
];

/// Run the canonical named-value candidate order.  The probe is semantic
/// candidate/identity work only; it cannot evaluate an instruction or demand
/// a child query.  Errors stop the order immediately, and the first value
/// stops it without probing later declaration kinds.
pub(crate) fn resolve_named_value_in_order<T, E>(
    probe: impl FnMut(DurableComptimeNamedValueKind) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
    resolve_named_value_with_order(DurableComptimeNamedValueOrder::Unqualified, probe)
}

pub(crate) fn resolve_module_member_in_order<T, E>(
    mut probe: impl FnMut(DurableComptimeNamedValueKind) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
    resolve_named_value_with_order(DurableComptimeNamedValueOrder::ModuleMember, &mut probe)
}

fn resolve_named_value_with_order<T, E>(
    order: DurableComptimeNamedValueOrder,
    mut probe: impl FnMut(DurableComptimeNamedValueKind) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
    let kinds = match order {
        DurableComptimeNamedValueOrder::Unqualified => &DURABLE_COMPTIME_UNQUALIFIED_VALUE_KINDS,
        DurableComptimeNamedValueOrder::ModuleMember => &DURABLE_COMPTIME_MODULE_MEMBER_KINDS,
    };
    for kind in kinds {
        if let Some(value) = probe(*kind)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

impl DurableComptimeNamedValueProjection {
    pub(crate) fn new(
        value: EvaluatedSemanticConst,
        dependency: SemanticDeclarationDependency,
    ) -> Self {
        Self {
            value,
            dependency,
            anonymous_nominals: Arc::from([]),
        }
    }

    pub(crate) fn with_anonymous_nominals(
        mut self,
        anonymous_nominals: Arc<[DurableAnonymousNominal]>,
    ) -> Self {
        self.anonymous_nominals = anonymous_nominals;
        self
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        EvaluatedSemanticConst,
        SemanticDeclarationDependency,
        Arc<[DurableAnonymousNominal]>,
    ) {
        (self.value, self.dependency, self.anonymous_nominals)
    }
}

/// Canonical semantic services needed by durable comptime entry points.
///
/// Implementations live beside the query authorities. This facade is an
/// operation boundary, not an evaluator: neither trait accepts an instruction
/// reference, instruction data, or callback capable of evaluating a child.
pub(crate) trait DurableComptimeSemanticAuthority {
    fn check_canceled(&self) -> Result<(), QueryAbort>;

    /// Resolve one declaration-owned type syntax through the canonical AIR
    /// structured resolver. The program key selects one registered owning
    /// arena, symbol table, and module; callers cannot mix dense syntax refs
    /// with another program. This operation never walks expression
    /// instructions or evaluates a child.
    fn resolve_type_syntax(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Result<
        DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    >;

    /// Resolve syntax against the exact active type/value substitution view.
    /// The program key remains the sole arena and symbol authority; callers
    /// cannot pair a syntax reference with an independently supplied arena.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    fn resolve_type_syntax_with_substitutions(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: &[(Arc<str>, DurableType)],
        value_substitutions: &[(Arc<str>, DurableConstValue)],
    ) -> Result<
        DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    >;

    fn begin_structured_type(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: Vec<(Arc<str>, DurableType)>,
        value_substitutions: Vec<(Arc<str>, DurableConstValue)>,
    ) -> Result<
        DurableStructuredTypePoll,
        DurableStructuredTypeBeginError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn resume_structured_type(
        &mut self,
        job: DurableStructuredTypeJob,
        reduced: rue_air::SemanticProviderResult<
            Option<rue_air::SemanticComptimeCallResult<DurableType, DurableConstValue>>,
            QueryAbort,
            SemanticNucleusFailure,
        >,
    ) -> Result<
        DurableStructuredTypePoll,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    >;

    fn begin_comptime_call_admission(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    /// Begin admission from the exact stable callable head supplied by a
    /// structured-type continuation.  Unlike the spelling-based operation,
    /// this operation must not rediscover a declaration through module/name
    /// lookup: the implementation reconstructs the canonical syntax candidate
    /// for the stable key with discriminator zero, then verifies the projected
    /// identity key. A stable key does not carry duplicate-discriminator
    /// information, so this operation must not claim to preserve duplicate
    /// candidates; it never infers identity from a spelling.
    #[allow(dead_code)] // consumed by the canonical structured-frame adapter
    fn begin_comptime_call_admission_for_key(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        head: &crate::StableDefinitionKey,
    ) -> Result<
        DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn finish_comptime_call_admission(
        &self,
        start: DurableComptimeCallableAdmissionStart,
        argument_modes: &[crate::durable_semantics::DurableParameterMode],
    ) -> Result<
        DurableComptimeCallableAdmission,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    /// Resolve a named const, module binding, callable, or nominal in the
    /// canonical order used by durable identifier evaluation.
    fn resolve_named_value(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<DurableComptimeNamedValueProjection>,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn resolve_module_member(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        member: &str,
    ) -> Result<
        DurableComptimeNamedValueProjection,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn resolve_import(
        &self,
        site: &DurableImportSite,
    ) -> Result<DurableImportResolution, QueryAbort>;

    /// Resolve an import occurrence through the exact registered program
    /// authority. The semantic site already carries the owning program and
    /// source-order occurrence; implementations must not consult an ambient
    /// occurrence map.
    fn resolve_keyed_import(
        &self,
        site: &rue_air::ComptimeSite<crate::body_query::DurableComptimeProgramKey>,
        specifier: &str,
    ) -> Result<DurableImportResolution, DurableComptimeKeyedImportError>;

    /// Resolve a target intrinsic from semantic name/arity facts.  The
    /// authority owns the configured target and the diagnostic policy; no RIR
    /// instruction or argument callback crosses this boundary.
    fn resolve_target_intrinsic(
        &self,
        intrinsic: ComptimeTargetIntrinsic,
        argument_count: usize,
    ) -> Result<TargetEnumValue, rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>>;

    /// Resolve a target descriptor member through the canonical target
    /// authority, preserving the durable evaluator's exact value shape.
    fn resolve_target_enum_variant(
        &self,
        type_name: &str,
        variant: &str,
    ) -> Result<TargetEnumValue, rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>>;
}

#[allow(dead_code)] // activated by the canonical durable AIR host
pub(crate) trait DurableComptimeForeignCallAuthority {
    fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort>;
}

pub(crate) struct DurableComptimeServices<'a, A: ?Sized> {
    authority: &'a mut A,
}

impl<'a, A: ?Sized> DurableComptimeServices<'a, A> {
    pub(crate) fn new(authority: &'a mut A) -> Self {
        Self { authority }
    }
}

impl<A: DurableComptimeSemanticAuthority + ?Sized> DurableComptimeServices<'_, A> {
    pub(crate) fn resolve_type_syntax(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Result<
        DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    > {
        self.authority.resolve_type_syntax(program, syntax)
    }

    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn resolve_type_syntax_with_substitutions(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: &[(Arc<str>, DurableType)],
        value_substitutions: &[(Arc<str>, DurableConstValue)],
    ) -> Result<
        DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    > {
        self.authority.resolve_type_syntax_with_substitutions(
            program,
            syntax,
            type_substitutions,
            value_substitutions,
        )
    }

    #[allow(dead_code)] // consumed by the canonical structured-frame adapter
    pub(crate) fn begin_comptime_call_admission_for_key(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        head: &crate::StableDefinitionKey,
    ) -> Result<
        DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .begin_comptime_call_admission_for_key(accessing_source, head)
    }

    pub(crate) fn finish_comptime_call_admission(
        &self,
        start: DurableComptimeCallableAdmissionStart,
        argument_modes: &[crate::durable_semantics::DurableParameterMode],
    ) -> Result<
        DurableComptimeCallableAdmission,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .finish_comptime_call_admission(start, argument_modes)
    }

    /// Finish admission for a structured-type call.  Structured syntax has
    /// already supplied the ordered arguments, so every argument is a value;
    /// the begin phase remains separate so its dependency can be observed
    /// before signature/arity policy is allowed to fail.
    #[allow(dead_code)] // consumed by the canonical structured-frame adapter
    pub(crate) fn finish_structured_comptime_call_admission(
        &self,
        start: DurableComptimeCallableAdmissionStart,
        argument_count: usize,
    ) -> Result<
        DurableComptimeCallableAdmission,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        let argument_modes = (0..argument_count)
            .map(|_| crate::durable_semantics::DurableParameterMode::Value)
            .collect::<Vec<_>>();
        self.finish_comptime_call_admission(start, &argument_modes)
    }

    /// Resolve an import against the registered program selected by `program`.
    /// This is the only evaluator-facing import path; occurrence metadata and
    /// declaration identity are paired atomically before querying imports.
    pub(crate) fn resolve_keyed_import(
        &self,
        site: &rue_air::ComptimeSite<crate::body_query::DurableComptimeProgramKey>,
        specifier: &str,
    ) -> Result<DurableImportResolution, DurableComptimeKeyedImportError> {
        if site.kind() != rue_air::ComptimeSiteKind::Import {
            return Err(DurableComptimeKeyedImportError::WrongSiteKind);
        }
        self.authority.resolve_keyed_import(site, specifier)
    }
}

#[allow(dead_code)] // activated by the canonical durable AIR host
impl<A: DurableComptimeForeignCallAuthority + ?Sized> DurableComptimeServices<'_, A> {
    /// Probe only an already-published foreign fact or admit its owned body
    /// frame. The authority owns dependency observation and cancellation; this
    /// method never demands a child comptime query.
    #[allow(dead_code)] // activated by the canonical durable AIR host
    pub(crate) fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort> {
        self.authority
            .probe_comptime_call(producer, type_arguments, value_arguments)
    }

    /// Consume the pending package and perform exactly one raw foreign probe.
    /// The query slices are borrowed from the opaque bound call; lookup and
    /// lifecycle state cannot be reconstructed or retried by the caller.
    #[allow(dead_code)]
    pub(crate) fn probe_prepared_call(
        &self,
        pending: DurableComptimePendingCall,
    ) -> Result<DurableComptimeProbedCall, QueryAbort> {
        let query = pending.query_view();
        let lookup = self.authority.probe_comptime_call(
            &pending.producer,
            query.type_arguments(),
            query.value_arguments(),
        )?;
        Ok(DurableComptimeProbedCall { pending, lookup })
    }

    /// Probe one structured-type continuation exactly once. The caller keeps
    /// the AIR job for resume; this package owns only its copied keyed request
    /// until the session consumes the lookup, so it cannot be cross-paired.
    #[allow(dead_code)]
    pub(crate) fn probe_structured_type_call(
        &self,
        pending: DurableStructuredTypeValidatedCall,
    ) -> Result<DurableStructuredTypeProbedCall, QueryAbort> {
        let request = &pending.pending.request;
        let lookup = self.authority.probe_comptime_call(
            &request.head_key,
            &request.type_arguments,
            &request.value_arguments,
        )?;
        Ok(DurableStructuredTypeProbedCall { pending, lookup })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluatedSemanticConst {
    Value(Arc<TypedSemanticConst>),
    Module(ModuleId),
    TargetEnum(TargetEnumValue),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetEnumValue {
    pub(crate) type_name: &'static str,
    pub(crate) variant: &'static str,
}

/// The semantic state an array-repeat count can have before global lookup.
/// `Unbound` is the only state that may proceed to the provider; a shadowed
/// value or type never falls through to a same-named global constant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // runtime-dependent bindings arrive with the durable AIR host
pub(crate) enum DurableComptimeArrayLengthBinding {
    LocalValue(EvaluatedSemanticConst),
    /// A type substitution shadows the name but has no value representation.
    Shadowed,
    RuntimeDependent,
    Unbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // consumed by the canonical durable AIR array-length hook
pub(crate) enum DurableComptimeArrayLengthDecision {
    Concrete(u64),
    Shadowed,
    RuntimeDependent,
    ResolveGlobal,
}

/// Diagnostic-free semantic failures from named array-length conversion. Each
/// caller owns the wording/channel adapter appropriate to its query surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeArrayLengthError {
    Module,
    TargetEnum,
    NonInteger,
    Negative(i128),
    TooLarge(i128),
}

/// Convert the AIR-owned lexical fact without dropping any value variant.
/// This is intentionally exhaustive so the AIR host cannot accidentally
/// reinterpret a shadow as an unbound global lookup.
#[allow(dead_code)] // consumed by the canonical durable AIR host
pub(crate) fn durable_array_length_binding_from_air(
    binding: rue_air::ComptimeArrayLengthBinding<EvaluatedSemanticConst>,
) -> DurableComptimeArrayLengthBinding {
    match binding {
        rue_air::ComptimeArrayLengthBinding::LocalValue(value) => {
            DurableComptimeArrayLengthBinding::LocalValue(value)
        }
        rue_air::ComptimeArrayLengthBinding::Shadowed => {
            DurableComptimeArrayLengthBinding::Shadowed
        }
        rue_air::ComptimeArrayLengthBinding::RuntimeDependent => {
            DurableComptimeArrayLengthBinding::RuntimeDependent
        }
        rue_air::ComptimeArrayLengthBinding::Unbound => DurableComptimeArrayLengthBinding::Unbound,
    }
}

/// Apply the canonical named array-length policy to a lexical semantic fact.
/// Concrete conversion and its semantic failures are shared by the durable
/// host paths; global lookup remains the caller's
/// responsibility so dependency observation happens exactly at the existing
/// provider point.
pub(crate) fn classify_durable_named_array_length(
    _name: &str,
    binding: DurableComptimeArrayLengthBinding,
) -> Result<DurableComptimeArrayLengthDecision, DurableComptimeArrayLengthError> {
    match binding {
        DurableComptimeArrayLengthBinding::RuntimeDependent => {
            Ok(DurableComptimeArrayLengthDecision::RuntimeDependent)
        }
        DurableComptimeArrayLengthBinding::Shadowed => {
            Ok(DurableComptimeArrayLengthDecision::Shadowed)
        }
        DurableComptimeArrayLengthBinding::Unbound => {
            Ok(DurableComptimeArrayLengthDecision::ResolveGlobal)
        }
        DurableComptimeArrayLengthBinding::LocalValue(value) => Ok(
            DurableComptimeArrayLengthDecision::Concrete(durable_named_array_length_value(&value)?),
        ),
    }
}

pub(crate) fn durable_named_array_length_failure(
    name: &str,
    error: DurableComptimeArrayLengthError,
) -> SemanticNucleusFailure {
    use DurableComptimeArrayLengthError as E;
    match error {
        E::Module => {
            SemanticNucleusFailure::Resolution(Arc::from("module used where a value is required"))
        }
        E::TargetEnum => SemanticNucleusFailure::Resolution(Arc::from(
            "target descriptor used where a durable const value is required",
        )),
        E::NonInteger | E::Negative(_) | E::TooLarge(_) => {
            SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::InvalidArrayLength {
                reason: if matches!(error, E::NonInteger) {
                    format!("array length expression '{name}' is not an integer")
                } else {
                    format!("array length expression '{name}' is negative or too large")
                },
            })
        }
    }
}

pub(crate) fn durable_named_array_length_value(
    value: &EvaluatedSemanticConst,
) -> Result<u64, DurableComptimeArrayLengthError> {
    let EvaluatedSemanticConst::Value(value) = value else {
        return Err(match value {
            EvaluatedSemanticConst::Module(_) => DurableComptimeArrayLengthError::Module,
            EvaluatedSemanticConst::TargetEnum(_) => DurableComptimeArrayLengthError::TargetEnum,
            EvaluatedSemanticConst::Value(_) => unreachable!(),
        });
    };
    durable_named_array_length_const(&value.value)
}

pub(crate) fn durable_named_array_length_const(
    value: &DurableConstValue,
) -> Result<u64, DurableComptimeArrayLengthError> {
    let DurableConstValue::Integer(value) = value else {
        return Err(DurableComptimeArrayLengthError::NonInteger);
    };
    durable_named_array_length_integer(*value)
}

pub(crate) fn durable_named_array_length_integer(
    value: i128,
) -> Result<u64, DurableComptimeArrayLengthError> {
    u64::try_from(value).map_err(|_| {
        if value < 0 {
            DurableComptimeArrayLengthError::Negative(value)
        } else {
            DurableComptimeArrayLengthError::TooLarge(value)
        }
    })
}

/// Match semantics shared by the durable evaluator and the AIR host.
/// Durable values deliberately remain narrower than the language's runtime
/// enum algebra: only an exact, unqualified, binding-free target descriptor
/// path is decidable here.
pub(crate) fn durable_match_pattern_matches<N: AsRef<str>>(
    pattern: &ComptimeMatchPattern<N>,
    value: &EvaluatedSemanticConst,
) -> bool {
    match pattern {
        ComptimeMatchPattern::Wildcard => true,
        ComptimeMatchPattern::Integer(pattern) => matches!(
            value,
            EvaluatedSemanticConst::Value(value)
                if matches!(value.value, DurableConstValue::Integer(actual) if actual == *pattern)
        ),
        ComptimeMatchPattern::Bool(pattern) => matches!(
            value,
            EvaluatedSemanticConst::Value(value)
                if matches!(value.value, DurableConstValue::Bool(actual) if actual == *pattern)
        ),
        ComptimeMatchPattern::Path {
            module_qualified: false,
            ctor_qualified: false,
            type_name,
            variant,
            binding_count: 0,
        } => matches!(
            value,
            EvaluatedSemanticConst::TargetEnum(target)
                if type_name.as_ref() == target.type_name && variant.as_ref() == target.variant
        ),
        ComptimeMatchPattern::Path { .. } => false,
    }
}

/// The canonical pure target-descriptor kernel used by durable semantic
/// authorities.  It receives only decomposed target facts, so tests and
/// future query adapters can cover data models not currently exposed by a
/// concrete compiler target without copying the mapping policy.
pub(crate) fn resolve_target_intrinsic_facts(
    intrinsic: ComptimeTargetIntrinsic,
    argument_count: usize,
    arch: rue_target::Arch,
    os: rue_target::Os,
    data_model: rue_target::DataModel,
) -> Result<TargetEnumValue, SemanticNucleusFailure> {
    if argument_count != 0 {
        return Err(SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::IntrinsicWrongArgCount {
                name: intrinsic.as_str().to_owned(),
                expected: 0,
                found: argument_count,
            },
        ));
    }
    let (type_name, variant) = match intrinsic {
        ComptimeTargetIntrinsic::Arch => (
            "Arch",
            match arch {
                rue_target::Arch::X86_64 => "X86_64",
                rue_target::Arch::Aarch64 => "Aarch64",
            },
        ),
        ComptimeTargetIntrinsic::Os => (
            "Os",
            match os {
                rue_target::Os::Linux => "Linux",
                rue_target::Os::Macos => "Macos",
            },
        ),
        ComptimeTargetIntrinsic::DataModel => (
            "DataModel",
            match data_model {
                rue_target::DataModel::Ilp32 => "Ilp32",
                rue_target::DataModel::Lp64 => "Lp64",
                rue_target::DataModel::Llp64 => "Llp64",
            },
        ),
    };
    Ok(TargetEnumValue { type_name, variant })
}

pub(crate) fn resolve_target_enum_variant_facts(
    type_name: &str,
    variant: &str,
) -> Result<TargetEnumValue, SemanticNucleusFailure> {
    const VARIANTS: &[(&str, &[&str])] = &[
        ("Arch", &["X86_64", "Aarch64"]),
        ("Os", &["Linux", "Macos"]),
        ("DataModel", &["Ilp32", "Lp64", "Llp64"]),
    ];
    let Some((canonical_type, variants)) = VARIANTS
        .iter()
        .find(|(candidate, _)| *candidate == type_name)
    else {
        return Err(SemanticNucleusFailure::Resolution(Arc::from(
            "unknown target descriptor enum",
        )));
    };
    let Some(canonical_variant) = variants
        .iter()
        .copied()
        .find(|candidate| *candidate == variant)
    else {
        return Err(SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::UnknownVariant {
                enum_name: (*canonical_type).to_owned(),
                variant_name: variant.to_owned(),
            },
        ));
    };
    Ok(TargetEnumValue {
        type_name: canonical_type,
        variant: canonical_variant,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedSemanticConst {
    pub(crate) value: DurableConstValue,
    /// `None` is reserved for an unconstrained integer literal. Every named
    /// value, local derived from one, and completed operation carries its
    /// canonical semantic type; consumers must never reconstruct it from the
    /// value's magnitude.
    pub(crate) ty: Option<DurableType>,
}

impl TypedSemanticConst {
    pub(crate) fn typed(value: DurableConstValue, ty: DurableType) -> Arc<Self> {
        Arc::new(Self {
            value,
            ty: Some(ty),
        })
    }

    pub(crate) fn integer_literal(value: i128) -> Arc<Self> {
        Arc::new(Self {
            value: DurableConstValue::Integer(value),
            ty: None,
        })
    }
}

/// AIR's type marker for a durable value.
///
/// Keeping the wrapper local avoids implementing an AIR trait for the generic
/// semantic-import type alias, which would violate Rust's orphan rules. The
/// conversion is lossless and intentionally carries no behavior of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeType(pub(crate) DurableType);

/// Compiler-owned name domain for AIR frames. `Arc<str>` itself is foreign to
/// both crates, so the wrapper is the lossless orphan-rule boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DurableComptimeName(pub(crate) Arc<str>);

impl DurableComptimeName {
    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<Arc<str>> for DurableComptimeName {
    fn from(value: Arc<str>) -> Self {
        Self(value)
    }
}

impl From<&str> for DurableComptimeName {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl AsRef<str> for DurableComptimeName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl ComptimeName for DurableComptimeName {}

/// The AIR file domain is keyed by the complete owning program identity.
/// A raw span file id or ambient module is insufficient when foreign programs
/// reuse dense file/instruction ids, so frames receive this only after their
/// program has been validated by the session registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DurableComptimeFile(crate::body_query::DurableComptimeProgramKey);

impl DurableComptimeFile {
    fn new(program: crate::body_query::DurableComptimeProgramKey) -> Self {
        Self(program)
    }

    #[allow(dead_code)] // consumed by the canonical durable query-root host
    pub(crate) fn program(&self) -> &crate::body_query::DurableComptimeProgramKey {
        &self.0
    }
}

impl ComptimeFile for DurableComptimeFile {}

/// Lossless compiler-owned AIR identity domain. The wrapped producer retains
/// definition and specialized-function identity; the newtype exists only to
/// satisfy the cross-crate trait boundary without violating orphan rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DurableComptimeIdentity(pub(crate) crate::StableProducerId);

impl From<crate::StableProducerId> for DurableComptimeIdentity {
    fn from(value: crate::StableProducerId) -> Self {
        Self(value)
    }
}

impl AsRef<crate::StableProducerId> for DurableComptimeIdentity {
    fn as_ref(&self) -> &crate::StableProducerId {
        &self.0
    }
}

impl ComptimeIdentity for DurableComptimeIdentity {}

/// Anonymous nominal identities are issued by AIR from the active producer
/// and structural anchor.  This wrapper keeps the canonical compiler key
/// opaque while satisfying AIR's identity marker at the host boundary.
#[allow(dead_code)] // consumed by the canonical durable AIR host
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DurableComptimeAnonymousIdentity(crate::AnonymousNominalKey);

impl ComptimeIdentity for DurableComptimeAnonymousIdentity {}

impl DurableComptimeAnonymousIdentity {
    fn new(key: crate::AnonymousNominalKey) -> Self {
        Self(key)
    }

    fn key(&self) -> &crate::AnonymousNominalKey {
        &self.0
    }
}

/// The narrow authority required by the production-shaped durable AIR host.
/// Query roots implement this beside their existing semantic and foreign
/// authorities; the host never owns a second registry or provider.
#[allow(dead_code)] // consumed by the canonical durable AIR host
pub(crate) trait DurableComptimeHostAuthority:
    DurableComptimeSemanticAuthority + DurableComptimeForeignCallAuthority
{
    fn durable_session(&self) -> &DurableComptimeSession;
    fn durable_session_mut(&mut self) -> &mut DurableComptimeSession;

    /// Test-only injection point for exercising the named array-length
    /// conversion boundary with a value the source language cannot spell.
    /// Production authorities leave this disabled.
    fn test_array_length_override(&self) -> Option<i128> {
        None
    }
}

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

/// Production host composition boundary.  Its implementation is introduced
/// The canonical AIR engine uses this host for both declaration-time query
/// roots and nested admitted frames.
#[allow(dead_code)] // consumed by the canonical durable AIR host
pub(crate) struct DurableComptimeHost<'a, A: DurableComptimeHostAuthority + ?Sized> {
    authority: &'a mut A,
}

impl<'a, A: DurableComptimeHostAuthority + ?Sized> DurableComptimeHost<'a, A> {
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn new(authority: &'a mut A) -> Self {
        Self { authority }
    }

    #[allow(dead_code)]
    fn program_rir(
        &self,
        program: &crate::body_query::DurableComptimeProgramKey,
    ) -> &rue_rir::ValidatedRir {
        &self
            .authority
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
            .authority
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
        self.authority
            .durable_session()
            .file_for_program(program)
            .unwrap_or_else(|_| panic!("unregistered durable program at {span:?}"))
    }

    fn diagnostic_site(
        &self,
        site: &rue_air::ComptimeDiagnosticSite<crate::body_query::DurableComptimeProgramKey>,
    ) -> DurableComptimeDiagnosticSite {
        self.authority
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
            .authority
            .durable_session_mut()
            .reserve_bound_expression_call();
        let start = self
            .authority
            .begin_comptime_call_admission(accessing_source, module, name.as_str())
            .map_err(durable_provider_error)?;
        self.authority
            .durable_session_mut()
            .observe_dependency(start.dependency.clone());
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
            .authority
            .finish_comptime_call_admission(start, &modes)
            .map_err(durable_provider_error)?;
        self.authority
            .durable_session_mut()
            .admit_bound_expression_call(reservation, admission)
            .map_err(|error| {
                durable_host_error(DurableComptimeFailure::resolution(format!(
                    "durable call lifecycle: {error:?}"
                )))
            })
    }
}

fn durable_host_failure(error: DurableComptimeFailure) -> DurableComptimeHostFailure {
    match error {
        DurableComptimeFailure::Failure(failure) => DurableComptimeHostFailure::semantic(failure),
        DurableComptimeFailure::Abort(abort) => DurableComptimeHostFailure::query_abort(abort),
    }
}

fn durable_diagnostic_failure(
    _site: &DurableComptimeDiagnosticSite,
    kind: rue_error::ErrorKind,
) -> DurableComptimeHostFailure {
    DurableComptimeHostFailure::semantic(Box::new(SemanticNucleusFailure::Diagnostic(kind)))
}

fn durable_host_error(
    error: DurableComptimeFailure,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    error.into_host_error()
}

fn durable_provider_error(
    error: rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    match error {
        rue_air::SemanticProviderError::Abort(error) => {
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure::query_abort(error))
        }
        rue_air::SemanticProviderError::Failure(error) => rue_air::ComptimeHostError::HostFailure(
            DurableComptimeHostFailure::semantic(Box::new(error)),
        ),
    }
}

fn durable_foreign_call_error(
    error: DurableComptimeForeignCallError,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    match error {
        DurableComptimeForeignCallError::ReadyFailure(failure) => {
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure::semantic(Box::new(
                failure,
            )))
        }
        DurableComptimeForeignCallError::StructuredFrame(
            DurableComptimeStructuredFrameAdmissionError::ValueFit(failure),
        ) => rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure::semantic(failure)),
        DurableComptimeForeignCallError::ReadyQueryFailure(failure) => {
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure::query_failure(
                failure,
            ))
        }
        DurableComptimeForeignCallError::AdmissionFailure(failure) => {
            durable_projection_failure_error(failure)
        }
        DurableComptimeForeignCallError::FrameAdmission(failure) => durable_host_error(
            DurableComptimeFailure::resolution(durable_frame_admission_failure_reason(failure)),
        ),
        DurableComptimeForeignCallError::StructuredFrame(failure) => durable_host_error(
            DurableComptimeFailure::resolution(durable_structured_frame_failure_reason(failure)),
        ),
        DurableComptimeForeignCallError::UnexpectedReadyProjection => {
            durable_host_error(DurableComptimeFailure::resolution(
                "durable comptime call returned an unexpected projection",
            ))
        }
        DurableComptimeForeignCallError::Lifecycle(failure) => durable_host_error(
            DurableComptimeFailure::resolution(durable_lifecycle_failure_reason(failure)),
        ),
    }
}

/// Maps candidate-artifact failures into the durable semantic channel.  Query
/// failures remain an outer query channel; callers must preserve that split.
pub(crate) fn durable_candidate_rir_semantic_failure(
    failure: &crate::revisioned_query_database::DeclarationBodyPlanFailure,
) -> SemanticNucleusFailure {
    use crate::revisioned_query_database::DeclarationBodyPlanFailure as Artifact;

    match failure {
        Artifact::Build(kind) => SemanticNucleusFailure::Diagnostic(kind.clone()),
        Artifact::CandidateRirRejected(errors) => {
            SemanticNucleusFailure::Syntax(Arc::from(errors.to_string()))
        }
        failure => SemanticNucleusFailure::Resolution(Arc::from(format!(
            "candidate RIR artifact failed: {failure:?}"
        ))),
    }
}

/// Maps candidate materialization failures into the durable semantic channel,
/// retaining query cancellation as an outer abort.
pub(crate) fn durable_materialization_semantic_failure(
    failure: crate::canonical_lower::BodyPlanMaterializationFailure,
) -> Result<SemanticNucleusFailure, QueryAbort> {
    use crate::canonical_lower::BodyPlanMaterializationFailure as Materialization;

    match failure {
        Materialization::Query(abort) => Err(abort),
        Materialization::Build(error) => Ok(SemanticNucleusFailure::Diagnostic(
            crate::canonical_lower::rir_build_error_kind(
                "declaration-time candidate materialization",
                &error,
            ),
        )),
        Materialization::Invalid(detail) => Ok(SemanticNucleusFailure::Resolution(detail)),
    }
}

fn durable_projection_failure_error(
    failure: crate::body_query::ComptimeProgramProjectionFailure,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    use crate::body_query::ComptimeProgramProjectionFailure as Projection;
    use crate::canonical_lower::BodyPlanMaterializationFailure as Materialization;
    use SemanticNucleusFailure as Failure;

    let failure = match failure {
        Projection::Materialization(Materialization::Query(abort)) => {
            return rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure::query_abort(
                abort,
            ));
        }
        Projection::Materialization(failure) => {
            match durable_materialization_semantic_failure(failure) {
                Ok(failure) => failure,
                Err(abort) => {
                    return rue_air::ComptimeHostError::Abort(
                        DurableComptimeHostFailure::query_abort(abort),
                    );
                }
            }
        }
        Projection::Artifact(failure) => durable_candidate_rir_semantic_failure(&failure),
        Projection::ArtifactQueryFailure(failure) => {
            return rue_air::ComptimeHostError::HostFailure(
                DurableComptimeHostFailure::query_failure(failure),
            );
        }
        Projection::NotFunction { .. } => Failure::Resolution(Arc::from(
            "comptime candidate artifact has a non-function root",
        )),
        Projection::NotConst { .. } => Failure::Resolution(Arc::from(
            "constant candidate artifact has a non-constant root",
        )),
        Projection::InvalidProducer(producer) => Failure::Resolution(Arc::from(format!(
            "{:?}",
            Projection::InvalidProducer(producer)
        ))),
        Projection::IdentityMismatch => {
            Failure::Resolution(Arc::from(format!("{:?}", Projection::IdentityMismatch)))
        }
    };
    DurableComptimeHostFailure::semantic(Box::new(failure)).into_host_error()
}

fn durable_frame_admission_failure_reason(
    failure: DurableComptimeForeignFrameAdmissionError,
) -> &'static str {
    match failure {
        DurableComptimeForeignFrameAdmissionError::NotCallable => {
            "durable comptime call root is not callable"
        }
        DurableComptimeForeignFrameAdmissionError::TicketMismatch => {
            "durable comptime call ticket does not match its program"
        }
        DurableComptimeForeignFrameAdmissionError::RegistryMismatch => {
            "durable comptime call program is not registered"
        }
    }
}

fn durable_structured_frame_failure_reason(
    failure: DurableComptimeStructuredFrameAdmissionError,
) -> &'static str {
    match failure {
        DurableComptimeStructuredFrameAdmissionError::InvalidContract => {
            "durable structured comptime call has an invalid contract"
        }
        DurableComptimeStructuredFrameAdmissionError::ValueFit(_) => {
            "durable structured comptime call argument does not fit"
        }
        DurableComptimeStructuredFrameAdmissionError::ResultNotType => {
            "durable structured comptime call result is not a type"
        }
    }
}

fn durable_lifecycle_failure_reason(failure: DurableComptimeLifecycleError) -> &'static str {
    match failure {
        DurableComptimeLifecycleError::TicketMismatch => "durable call lifecycle ticket mismatch",
        DurableComptimeLifecycleError::BindingMismatch => "durable call lifecycle binding mismatch",
        DurableComptimeLifecycleError::InvalidProgramAuthority => {
            "durable call lifecycle invalid program authority"
        }
        DurableComptimeLifecycleError::NotEntered => "durable call lifecycle call was not entered",
        DurableComptimeLifecycleError::OutOfOrder => {
            "durable call lifecycle call finished out of order"
        }
        DurableComptimeLifecycleError::TicketReused => "durable call lifecycle ticket was reused",
        DurableComptimeLifecycleError::InvalidContext => "durable call lifecycle invalid context",
        DurableComptimeLifecycleError::ReadyProjectionRequired => {
            "durable call lifecycle requires a ready projection"
        }
    }
}

/// Normalized type-syntax terminal shared by the durable comptime and
/// signature-query adapters. Nested comptime-call arguments are reduced to
/// their terminal so each context can apply its own diagnostic policy once.
#[derive(Debug)]
pub(crate) enum DurableTypeSyntaxClassification {
    Abort(QueryAbort),
    Failure(SemanticNucleusFailure),
    Semantic(rue_air::SemanticTypeSyntaxFailure<crate::StableDefinitionKey, Arc<str>>),
}

pub(crate) fn classify_durable_type_syntax_failure(
    error: rue_air::SemanticTypeSyntaxError<
        QueryAbort,
        SemanticNucleusFailure,
        crate::StableDefinitionKey,
        Arc<str>,
    >,
) -> DurableTypeSyntaxClassification {
    use rue_air::SemanticResolutionError as E;

    match error {
        E::ProviderAbort(abort) => DurableTypeSyntaxClassification::Abort(abort),
        E::ProviderFailure(failure) => DurableTypeSyntaxClassification::Failure(failure),
        E::Semantic(failure) => DurableTypeSyntaxClassification::Semantic(failure),
        E::ComptimeCallTypeArgument { error, .. } => classify_durable_type_syntax_failure(*error),
    }
}

/// Durable comptime's type-syntax adapter preserves its historical
/// resolution-shaped debug diagnostic. Signature queries intentionally use a
/// separate detailed adapter in `revisioned_query_database`.
pub(crate) fn durable_comptime_type_syntax_failure(
    error: rue_air::SemanticTypeSyntaxError<
        QueryAbort,
        SemanticNucleusFailure,
        crate::StableDefinitionKey,
        Arc<str>,
    >,
) -> DurableComptimeFailure {
    match classify_durable_type_syntax_failure(error) {
        DurableTypeSyntaxClassification::Abort(abort) => DurableComptimeFailure::abort(abort),
        DurableTypeSyntaxClassification::Failure(failure) => {
            DurableComptimeFailure::failure(failure)
        }
        DurableTypeSyntaxClassification::Semantic(failure) => {
            DurableComptimeFailure::resolution(format!("Semantic({failure:?})"))
        }
    }
}

fn durable_type_syntax_error(
    error: rue_air::SemanticTypeSyntaxError<
        QueryAbort,
        SemanticNucleusFailure,
        crate::StableDefinitionKey,
        Arc<str>,
    >,
) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
    durable_comptime_type_syntax_failure(error).into_host_error()
}

fn durable_host_error_outcome<T>(
    error: rue_air::ComptimeHostError<DurableComptimeHostFailure>,
) -> rue_air::ComptimeOutcome<T, DurableComptimeHostFailure> {
    match error {
        rue_air::ComptimeHostError::HostFailure(error) => {
            rue_air::ComptimeOutcome::HostFailure(error)
        }
        rue_air::ComptimeHostError::Abort(error) => rue_air::ComptimeOutcome::Abort(error),
    }
}

/// AIR supplies compact operator tokens; durable diagnostics use the
/// operation names from established declaration-time semantics. Unary negation is
/// passed as its own token by the canonical engine so it cannot be confused
/// with subtraction.
fn durable_arithmetic_operation_name(operation: &str) -> &str {
    match operation {
        "+" => "addition",
        "-" => "subtraction",
        "*" => "multiplication",
        "/" => "division",
        "%" => "remainder",
        "<<" => "left shift",
        ">>" => "right shift",
        "negation" => "negation",
        other => other,
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> rue_air::ComptimeHost
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

    fn check_canceled(&self) -> rue_air::ComptimeHostResult<(), Self::Failure> {
        self.authority.check_canceled().map_err(|abort| {
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure::query_abort(abort))
        })
    }

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
            .authority
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
        self.authority
            .durable_session_mut()
            .observe_dependency(dependency);
        for nominal in anonymous_nominals.iter().cloned() {
            self.authority
                .durable_session_mut()
                .observe_anonymous_nominal(nominal);
        }
        Ok(rue_air::ComptimeNamedValueResolution::Known(value))
    }

    fn match_pattern(
        &self,
        pattern: &rue_air::ComptimeMatchPattern<Self::Name>,
        value: &Self::Value,
    ) -> Option<bool> {
        Some(durable_match_pattern_matches(pattern, value))
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

    fn evaluate_binary_rhs_after_rejection(&self) -> bool {
        true
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
        Err(rue_air::ComptimeHostError::HostFailure(
            durable_diagnostic_failure(
                &self.diagnostic_site(site),
                rue_error::ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_owned(),
                },
            ),
        ))
    }

    fn depth_exceeded(
        &self,
        name: &Self::Name,
        depth: usize,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        durable_host_failure(DurableComptimeFailure::maximum_depth(name.as_str(), depth))
    }

    fn literal_out_of_range(
        &self,
        value: u64,
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

    fn float_not_implemented(
        &self,
        site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        durable_diagnostic_failure(
            &self.diagnostic_site(site),
            rue_error::ErrorKind::FloatNotYetImplemented,
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
                let projection = match self.authority.resolve_named_value(
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
                self.authority
                    .durable_session_mut()
                    .observe_dependency(dependency);
                let value = self
                    .authority
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
            .authority
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
            .authority
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
            self.authority.durable_session_mut(),
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
            self.authority.durable_session_mut(),
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
        let source = self.diagnostic_site(site);
        self.authority
            .durable_session_mut()
            .observe_deferred_ownership(DeferredOwnershipGate {
                kind: crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireDroppable,
                ty: ty.0,
                source: Arc::new(crate::semantic_query_nucleus::DeferredOwnershipGateSource {
                    declaration: source.producer,
                    start: source.start,
                    end: source.end,
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
        let source = self.diagnostic_site(site);
        self.authority
            .durable_session_mut()
            .observe_deferred_ownership(DeferredOwnershipGate {
            kind:
                crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireTriviallyDroppable,
            ty: ty.0,
            source: Arc::new(crate::semantic_query_nucleus::DeferredOwnershipGateSource {
                declaration: source.producer,
                start: source.start,
                end: source.end,
            }),
            application: None,
        });
        Ok(())
    }

    fn type_name(&self, ty: &Self::Type) -> String {
        DurableComptimeScalarPolicy::type_name(ty.as_ref())
    }

    fn type_is_unsigned(&self, ty: &Self::Type) -> bool {
        DurableComptimeScalarPolicy::type_is_unsigned(ty.as_ref())
    }

    fn reject_unsigned_negation(
        &self,
        _ty: &Self::Type,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Option<Self::Failure> {
        // Declaration-time evaluation reports the checked integer overflow
        // from `finish_arith`, rather than AIR's ordinary CannotNegate policy.
        None
    }

    fn type_integer_semantics(
        &self,
        ty: &Self::Type,
    ) -> Option<rue_air::integer_semantics::IntegerType> {
        DurableComptimeScalarPolicy::type_integer_semantics(ty.as_ref())
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

    fn finish_arith(
        &self,
        result: rue_air::integer_semantics::CheckedIntegerResult,
        ty: Option<Self::Type>,
        op: &str,
        _site: &rue_air::ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> rue_air::ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        let ty = ty.unwrap_or(DurableComptimeType(DurableType::I32));
        let value = DurableComptimeScalarPolicy::checked_integer_result(
            ty.as_ref(),
            result,
            durable_arithmetic_operation_name(op),
        )
        .map_err(durable_host_error)?;
        Ok(Some(EvaluatedSemanticConst::integer_typed(value, Some(ty))))
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

    fn resolve_named_type_value(
        &mut self,
        _program: &Self::ProgramKey,
        _name: Self::Name,
        _span: rue_span::Span,
    ) -> rue_air::ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        // Durable TypeConst names are resolved by the canonical keyed
        // structured-type continuation below. Returning `None` here avoids a
        // speculative named-value query/dependency before that resolver has
        // established the exact type-syntax authority.
        Ok(None)
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
            .authority
            .resolve_target_enum_variant(segments[0].as_str(), segments[1].as_str())
        {
            Ok(value) => Ok(Some(EvaluatedSemanticConst::TargetEnum(value))),
            Err(error) => Err(durable_provider_error(error)),
        }
    }

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
        let Some(parameter) = binding.admission.parameters.get(index).cloned() else {
            return Err(durable_host_error(DurableComptimeFailure::resolution(
                "durable comptime call argument index is out of bounds",
            )));
        };
        let Some(header) = binding.admission.shell_parameters.get(index).cloned() else {
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
            .authority
            .durable_session_mut()
            .prepare_bound_expression_call(admission.payload, bound)
            .map_err(|error| {
                durable_host_error(DurableComptimeFailure::resolution(format!(
                    "durable call lifecycle: {error:?}"
                )))
            })?;
        if let Some(failure) = self.authority.durable_session().active_comptime_call_cycle(
            &pending.producer,
            &pending.bound.admission.configuration,
            &pending.bound.type_arguments,
            &pending.bound.value_arguments,
        ) {
            return Err(durable_host_error(DurableComptimeFailure::failure(failure)));
        }
        let probed = DurableComptimeServices::new(&mut *self.authority)
            .probe_prepared_call(pending)
            .map_err(|abort| {
                rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure::query_abort(abort))
            })?;
        let prepared = self
            .authority
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
            .authority
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
        self.authority
            .durable_session_mut()
            .enter_call(ticket)
            .map_err(|error| {
                durable_host_error(DurableComptimeFailure::resolution(format!(
                    "durable call lifecycle: {error:?}"
                )))
            })
    }

    fn label_ctor_instantiation_site(
        error: Self::Failure,
        _call_span: rue_span::Span,
    ) -> Self::Failure {
        error
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
        self.authority
            .resolve_type_syntax_with_substitutions(
                program,
                syntax,
                &type_substitutions,
                &value_substitutions,
            )
            .ok()
            .map(DurableComptimeType)
    }

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
        match self.authority.begin_structured_type(
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
            .authority
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
        if let Some(failure) = self.authority.durable_session().active_comptime_call_cycle(
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
            .authority
            .begin_comptime_call_admission_for_key(&program.declaration, &head)
        {
            Ok(start) => start,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        self.authority
            .durable_session_mut()
            .observe_dependency(start.dependency.clone());
        let admission = match DurableComptimeServices::new(&mut *self.authority)
            .finish_structured_comptime_call_admission(start, argument_count)
        {
            Ok(admission) => admission,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        let validated = match self
            .authority
            .durable_session()
            .validate_structured_type_call(pending, admission)
        {
            Ok(validated) => validated,
            Err(error) => {
                return durable_host_error_outcome(durable_foreign_call_error(error));
            }
        };
        let probed = match DurableComptimeServices::new(&mut *self.authority)
            .probe_structured_type_call(validated)
        {
            Ok(probed) => probed,
            Err(abort) => {
                return rue_air::ComptimeOutcome::Abort(DurableComptimeHostFailure::query_abort(
                    abort,
                ));
            }
        };
        let prepared = match self
            .authority
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
            .authority
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
                let resolution = DurableComptimeServices::new(&mut *self.authority)
                    .resolve_keyed_import(site, specifier.as_str());
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
                .authority
                .resolve_target_intrinsic(intrinsic, argument_count)
            {
                Ok(value) => {
                    rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::TargetEnum(value))
                }
                Err(error) => durable_host_error_outcome(durable_provider_error(error)),
            },
        }
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
            .authority
            .resolve_target_enum_variant(type_name.as_str(), variant.as_str())
        {
            Ok(value) => rue_air::ComptimeOutcome::Known(EvaluatedSemanticConst::TargetEnum(value)),
            Err(error) => durable_host_error_outcome(durable_provider_error(error)),
        }
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
        let EvaluatedSemanticConst::Module(module) = base else {
            return rue_air::ComptimeOutcome::HostFailure(durable_host_failure(
                DurableComptimeFailure::resolution("member access on a non-module const value"),
            ));
        };
        let projection = self.authority.resolve_module_member(
            &site.program().declaration,
            &module,
            field.as_str(),
        );
        let projection = match projection {
            Ok(projection) => projection,
            Err(error) => return durable_host_error_outcome(durable_provider_error(error)),
        };
        let (value, dependency, anonymous_nominals) = projection.into_parts();
        self.authority
            .durable_session_mut()
            .observe_dependency(dependency);
        for nominal in anonymous_nominals.iter().cloned() {
            self.authority
                .durable_session_mut()
                .observe_anonymous_nominal(nominal);
        }
        rue_air::ComptimeOutcome::Known(value)
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

impl From<DurableType> for DurableComptimeType {
    fn from(value: DurableType) -> Self {
        Self(value)
    }
}

impl From<DurableComptimeType> for DurableType {
    fn from(value: DurableComptimeType) -> Self {
        value.0
    }
}

impl AsRef<DurableType> for DurableComptimeType {
    fn as_ref(&self) -> &DurableType {
        &self.0
    }
}

impl ComptimeType for DurableComptimeType {}

/// The canonical structured-type job owned by one durable program core. The
/// job retains the cloned type arena and symbol authority internally; callers
/// can only resume the exact continuation returned by the AIR resolver.
#[allow(dead_code)]
pub(crate) type DurableStructuredTypeJob = rue_air::ComptimeStructuredTypeJob<
    DurableStructuredTypeProgramCapability,
    ModuleId,
    crate::StableDefinitionKey,
    Arc<str>,
    crate::StableDefinitionKey,
    DurableType,
    DurableConstValue,
    lasso::Spur,
    Arc<[Arc<str>]>,
>;

#[allow(dead_code)]
pub(crate) type DurableStructuredTypePoll = rue_air::ComptimeStructuredTypePoll<
    DurableStructuredTypeProgramCapability,
    ModuleId,
    crate::StableDefinitionKey,
    Arc<str>,
    crate::StableDefinitionKey,
    DurableType,
    DurableConstValue,
    lasso::Spur,
    Arc<[Arc<str>]>,
>;

/// The immutable request contract copied from a canonical AIR job. The job
/// itself remains owned by AIR for resume and never enters the probe package.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DurableStructuredTypeProgramCapability {
    key: crate::body_query::DurableComptimeProgramKey,
    owner: u64,
}

impl DurableStructuredTypeProgramCapability {
    fn new(key: crate::body_query::DurableComptimeProgramKey, owner: u64) -> Self {
        Self { key, owner }
    }

    pub(crate) fn key(&self) -> &crate::body_query::DurableComptimeProgramKey {
        &self.key
    }
}

#[allow(dead_code)]
struct DurableStructuredTypeRequest {
    program: crate::body_query::DurableComptimeProgramKey,
    head_key: crate::StableDefinitionKey,
    parameters: Arc<[rue_air::SemanticTypeConstructorParameter<Arc<str>>]>,
    returns_type: bool,
    type_arguments: Vec<(Arc<str>, DurableType)>,
    value_arguments: Vec<(Arc<str>, DurableConstValue)>,
    call_span: rue_span::Span,
}

/// A structured-type call after its canonical AIR job has supplied the
/// immutable request facts. It does not own the AIR job, so the continuation
/// cannot be replayed or cross-paired by this coordinator.
#[allow(dead_code)]
pub(crate) struct DurableStructuredTypePendingCall {
    request: DurableStructuredTypeRequest,
    edge: DurableComptimeCallEdge,
}

/// A pending structured call after the exact callable contract has been
/// checked. The typed bindings are produced here, before a foreign query is
/// probed, so the eventual frame cannot be paired with a different request.
#[allow(dead_code)]
pub(crate) struct DurableStructuredTypeValidatedCall {
    pending: DurableStructuredTypePendingCall,
    admission: DurableComptimeCallableAdmission,
    type_bindings: AHashMap<DurableComptimeName, DurableComptimeType>,
    value_bindings: AHashMap<DurableComptimeName, EvaluatedSemanticConst>,
    expected_result: DurableComptimeType,
}

/// The result of one and only one structured foreign probe. It is consuming
/// and non-cloneable by construction. The canonical AIR job remains owned by
/// the engine; the pending request snapshots only its keyed facts and the
/// original call span supplied separately by the engine.
#[allow(dead_code)]
pub(crate) struct DurableStructuredTypeProbedCall {
    pending: DurableStructuredTypeValidatedCall,
    lookup: ForeignComptimeCallLookup,
}

#[allow(dead_code)]
pub(crate) enum DurableStructuredTypeCall {
    Ready {
        result: crate::semantic_query_nucleus::ComptimeCallResultProjection,
    },
    Enter {
        program: crate::body_query::OwnedForeignComptimeProgram,
        frame: Box<DurableComptimeForeignFrame>,
        ticket: Box<DurableComptimeCallTicket>,
    },
    NotReady,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum DurableStructuredTypeBeginError<E, F> {
    UnregisteredProgram,
    InvalidProgramAuthority,
    Resolution(rue_air::SemanticTypeSyntaxError<E, F, crate::StableDefinitionKey, Arc<str>>),
}

/// Begin the canonical AIR structured resolver against an owning program.
/// This is deliberately generic over the existing semantic provider so the
/// adapter adds no second type-syntax traversal or query authority.
#[allow(dead_code)]
pub(crate) fn begin_durable_structured_type<Q>(
    session: &DurableComptimeSession,
    key: &crate::body_query::DurableComptimeProgramKey,
    root: rue_rir::RirTypeSyntaxRef,
    type_substitutions: Vec<(Arc<str>, DurableType)>,
    value_substitutions: Vec<(Arc<str>, DurableConstValue)>,
    provider: &mut Q,
) -> Result<DurableStructuredTypePoll, DurableStructuredTypeBeginError<Q::Abort, Q::Failure>>
where
    Q: rue_air::SemanticTypeSyntaxProvider<
            ModuleId,
            ModuleId,
            crate::StableDefinitionKey,
            crate::StableDefinitionKey,
            Arc<str>,
            DurableType,
            DurableConstValue,
        >,
{
    let Some(capability) = session.structured_program_capability(key) else {
        return Err(DurableStructuredTypeBeginError::UnregisteredProgram);
    };
    let Some(authority) = session.programs.structured_type_authority_with_program(
        key,
        capability,
        key.declaration.module().clone(),
        root,
    ) else {
        return Err(DurableStructuredTypeBeginError::InvalidProgramAuthority);
    };
    DurableStructuredTypeJob::begin::<ModuleId, Q>(
        provider,
        authority,
        type_substitutions,
        value_substitutions,
    )
    .map_err(DurableStructuredTypeBeginError::Resolution)
}

/// Resume one consuming canonical structured continuation. The reduced call
/// result is supplied by the same engine that owns the enclosing expression.
#[allow(dead_code)]
pub(crate) fn resume_durable_structured_type<Q>(
    job: DurableStructuredTypeJob,
    provider: &mut Q,
    reduced: rue_air::SemanticProviderResult<
        Option<rue_air::SemanticComptimeCallResult<DurableType, DurableConstValue>>,
        Q::Abort,
        Q::Failure,
    >,
) -> Result<
    DurableStructuredTypePoll,
    rue_air::SemanticTypeSyntaxError<Q::Abort, Q::Failure, crate::StableDefinitionKey, Arc<str>>,
>
where
    Q: rue_air::SemanticTypeSyntaxProvider<
            ModuleId,
            ModuleId,
            crate::StableDefinitionKey,
            crate::StableDefinitionKey,
            Arc<str>,
            DurableType,
            DurableConstValue,
        >,
{
    job.resume::<ModuleId, Q>(provider, reduced)
}

impl ComptimeValue for EvaluatedSemanticConst {
    type Type = DurableComptimeType;

    fn integer(value: i128) -> Self {
        Self::Value(TypedSemanticConst::integer_literal(value))
    }

    fn boolean(value: bool) -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Bool(value),
            DurableType::Bool,
        ))
    }

    fn unit() -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Unit,
            DurableType::Unit,
        ))
    }

    fn type_value(value: Self::Type) -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Type(value.0),
            DurableType::ComptimeType,
        ))
    }

    fn as_integer(&self) -> Option<i128> {
        let Self::Value(value) = self else {
            return None;
        };
        match value.value {
            DurableConstValue::Integer(value) => Some(value),
            DurableConstValue::Bool(_)
            | DurableConstValue::Type(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_boolean(&self) -> Option<bool> {
        let Self::Value(value) = self else {
            return None;
        };
        match value.value {
            DurableConstValue::Bool(value) => Some(value),
            DurableConstValue::Integer(_)
            | DurableConstValue::Type(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_type(&self) -> Option<Self::Type> {
        let Self::Value(value) = self else {
            return None;
        };
        match &value.value {
            DurableConstValue::Type(value) => Some(DurableComptimeType(value.clone())),
            DurableConstValue::Integer(_)
            | DurableConstValue::Bool(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn eligible_for_comptime_capture(&self) -> bool {
        matches!(self, Self::Value(_))
    }

    fn as_integer_type(&self) -> Option<Self::Type> {
        let Self::Value(value) = self else {
            return None;
        };
        if !matches!(value.value, DurableConstValue::Integer(_)) {
            return None;
        }
        value.ty.clone().map(DurableComptimeType)
    }

    fn integer_typed(value: i128, ty: Option<Self::Type>) -> Self {
        match ty {
            Some(ty) => Self::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(value),
                ty.0,
            )),
            None => Self::integer(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durable_semantics::{DurableParameterMode, DurableSemanticParameter};

    fn value(value: DurableConstValue, ty: Option<DurableType>) -> EvaluatedSemanticConst {
        EvaluatedSemanticConst::Value(Arc::new(TypedSemanticConst { value, ty }))
    }

    fn typed(value: DurableConstValue, ty: DurableType) -> TypedSemanticConst {
        TypedSemanticConst {
            value,
            ty: Some(ty),
        }
    }

    #[test]
    fn structured_value_fit_mapping_preserves_each_exact_failure_channel() {
        let alias_key = crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("structured-fit.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "alias",
            None,
        );
        assert_eq!(
            durable_structured_value_fit_failure(
                &DurableConstValue::Function(alias_key),
                &DurableType::I32,
            ),
            Some(SemanticNucleusFailure::Resolution(Arc::from(
                "a callable alias cannot be passed as a comptime value argument",
            )))
        );
        assert_eq!(
            durable_structured_value_fit_failure(
                &DurableConstValue::Integer(i128::MAX),
                &DurableType::I32,
            ),
            Some(SemanticNucleusFailure::Resolution(Arc::from(
                "value 170141183460469231731687303715884105727 is outside the range of type i32",
            )))
        );
        assert_eq!(
            durable_structured_value_fit_failure(&DurableConstValue::Bool(true), &DurableType::I32,),
            Some(SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::TypeMismatch {
                    expected: "i32".into(),
                    found: "bool".into(),
                },
            ))
        );
    }

    fn parameter(name: &str, ty: DurableType) -> DurableSemanticParameter {
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
            parameter("T", DurableType::ComptimeType),
            parameter("value", DurableType::GenericParameter(0)),
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
        let admitted = DurableComptimeAdmittedCall::new(
            DurableComptimeCallToken::new(0, 0),
            binding_admission(),
        );
        DurableComptimeBinding::new(&admitted)
    }

    #[test]
    fn scalar_constructors_preserve_the_existing_durable_forms() {
        assert_eq!(
            EvaluatedSemanticConst::integer(7),
            value(DurableConstValue::Integer(7), None)
        );
        assert_eq!(
            EvaluatedSemanticConst::boolean(true),
            value(DurableConstValue::Bool(true), Some(DurableType::Bool))
        );
        assert_eq!(
            EvaluatedSemanticConst::unit(),
            value(DurableConstValue::Unit, Some(DurableType::Unit))
        );
    }

    #[test]
    fn named_array_length_policy_preserves_lexical_and_conversion_channels() {
        let integer = EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
            DurableConstValue::Integer(4),
            DurableType::I32,
        ));
        assert_eq!(
            classify_durable_named_array_length(
                "N",
                DurableComptimeArrayLengthBinding::LocalValue(integer),
            )
            .unwrap(),
            DurableComptimeArrayLengthDecision::Concrete(4)
        );
        assert_eq!(
            classify_durable_named_array_length("N", DurableComptimeArrayLengthBinding::Unbound,)
                .unwrap(),
            DurableComptimeArrayLengthDecision::ResolveGlobal
        );
        assert_eq!(
            classify_durable_named_array_length(
                "N",
                DurableComptimeArrayLengthBinding::RuntimeDependent,
            )
            .unwrap(),
            DurableComptimeArrayLengthDecision::RuntimeDependent
        );

        let non_integer = EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
            DurableConstValue::Bool(true),
            DurableType::Bool,
        ));
        let error = classify_durable_named_array_length(
            "N",
            DurableComptimeArrayLengthBinding::LocalValue(non_integer),
        )
        .unwrap_err();
        assert!(matches!(error, DurableComptimeArrayLengthError::NonInteger));
        assert_eq!(
            classify_durable_named_array_length("N", DurableComptimeArrayLengthBinding::Shadowed,)
                .unwrap(),
            DurableComptimeArrayLengthDecision::Shadowed
        );
        assert!(matches!(
            classify_durable_named_array_length(
                "N",
                DurableComptimeArrayLengthBinding::LocalValue(EvaluatedSemanticConst::Module(
                    ModuleId::from_validated_canonical("root")
                ),),
            )
            .unwrap_err(),
            DurableComptimeArrayLengthError::Module
        ));
        assert!(matches!(
            classify_durable_named_array_length(
                "N",
                DurableComptimeArrayLengthBinding::LocalValue(EvaluatedSemanticConst::TargetEnum(
                    TargetEnumValue {
                        type_name: "Target",
                        variant: "x86_64",
                    }
                ),),
            )
            .unwrap_err(),
            DurableComptimeArrayLengthError::TargetEnum
        ));

        for (value, expected) in [(-1, "negative"), (i128::from(u64::MAX) + 1, "too_large")] {
            let error = durable_named_array_length_value(&EvaluatedSemanticConst::Value(
                TypedSemanticConst::typed(DurableConstValue::Integer(value), DurableType::I64),
            ))
            .unwrap_err();
            match (expected, error) {
                ("negative", DurableComptimeArrayLengthError::Negative(actual)) => {
                    assert_eq!(actual, value)
                }
                ("too_large", DurableComptimeArrayLengthError::TooLarge(actual)) => {
                    assert_eq!(actual, value)
                }
                _ => panic!("unexpected array-length error"),
            }
        }
    }

    #[test]
    fn air_array_length_binding_conversion_is_exhaustive_and_lossless() {
        let value = EvaluatedSemanticConst::integer(7);
        let cases = [
            (
                rue_air::ComptimeArrayLengthBinding::LocalValue(value.clone()),
                DurableComptimeArrayLengthBinding::LocalValue(value),
            ),
            (
                rue_air::ComptimeArrayLengthBinding::Shadowed,
                DurableComptimeArrayLengthBinding::Shadowed,
            ),
            (
                rue_air::ComptimeArrayLengthBinding::RuntimeDependent,
                DurableComptimeArrayLengthBinding::RuntimeDependent,
            ),
            (
                rue_air::ComptimeArrayLengthBinding::Unbound,
                DurableComptimeArrayLengthBinding::Unbound,
            ),
        ];
        for (air, expected) in cases {
            assert_eq!(durable_array_length_binding_from_air(air), expected);
        }
    }

    #[test]
    fn integer_metadata_is_optional_and_lossless() {
        let plain = EvaluatedSemanticConst::integer(9);
        assert_eq!(plain.as_integer(), Some(9));
        assert_eq!(plain.as_integer_type(), None);

        let typed =
            EvaluatedSemanticConst::integer_typed(9, Some(DurableComptimeType(DurableType::I16)));
        assert_eq!(typed.as_integer(), Some(9));
        assert_eq!(
            typed.as_integer_type(),
            Some(DurableComptimeType(DurableType::I16))
        );
        assert_eq!(
            typed,
            value(DurableConstValue::Integer(9), Some(DurableType::I16))
        );
    }

    #[test]
    fn type_values_round_trip_without_reinterpreting_other_variants() {
        let ty = DurableComptimeType(DurableType::Array {
            element: Arc::new(DurableType::U32),
            len: 3,
        });
        let type_value = EvaluatedSemanticConst::type_value(ty.clone());
        assert_eq!(type_value.as_type(), Some(ty.clone()));
        assert_eq!(
            type_value,
            value(
                DurableConstValue::Type(ty.0),
                Some(DurableType::ComptimeType)
            )
        );
        assert_eq!(type_value.as_integer(), None);
        assert_eq!(type_value.as_boolean(), None);
        assert_eq!(type_value.as_integer_type(), None);
    }

    #[test]
    fn module_and_target_enum_values_are_not_scalar_values() {
        let module = EvaluatedSemanticConst::Module(ModuleId::from_logical_path("m").unwrap());
        assert_eq!(module.as_integer(), None);
        assert_eq!(module.as_boolean(), None);
        assert_eq!(module.as_type(), None);
        assert_eq!(module.as_integer_type(), None);

        let target = EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name: "Os",
            variant: "Macos",
        });
        assert_eq!(target.as_integer(), None);
        assert_eq!(target.as_boolean(), None);
        assert_eq!(target.as_type(), None);
        assert_eq!(target.as_integer_type(), None);
    }

    #[test]
    fn clone_and_conversions_preserve_representation() {
        let ty = DurableType::PtrMut(Arc::new(DurableType::I64));
        let wrapped = DurableComptimeType(ty.clone());
        let unwrapped: DurableType = wrapped.clone().into();
        assert_eq!(unwrapped, ty.clone());
        assert_eq!(DurableComptimeType::from(ty.clone()).as_ref(), &ty);

        let original =
            EvaluatedSemanticConst::integer_typed(-12, Some(DurableComptimeType(ty.clone())));
        assert_eq!(original.clone(), original);
        assert_eq!(original.as_integer_type(), Some(DurableComptimeType(ty)));
    }

    #[test]
    fn durable_match_kernel_preserves_scalar_and_target_pattern_policy() {
        let integer = value(DurableConstValue::Integer(-7), Some(DurableType::I16));
        let boolean = value(DurableConstValue::Bool(true), Some(DurableType::Bool));
        let target = EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name: "Os",
            variant: "Macos",
        });
        let path = |module_qualified, ctor_qualified, type_name, variant, binding_count| {
            ComptimeMatchPattern::Path {
                module_qualified,
                ctor_qualified,
                type_name: Arc::from(type_name),
                variant: Arc::from(variant),
                binding_count,
            }
        };

        assert!(durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Wildcard,
            &EvaluatedSemanticConst::Module(ModuleId::from_logical_path("m").unwrap()),
        ));
        assert!(durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Integer(-7),
            &integer,
        ));
        assert!(!durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Integer(7),
            &integer,
        ));
        assert!(durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Bool(true),
            &boolean,
        ));
        assert!(!durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Bool(false),
            &integer,
        ));
        assert!(!durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Integer(-7),
            &boolean,
        ));
        assert!(durable_match_pattern_matches(
            &path(false, false, "Os", "Macos", 0),
            &target,
        ));
        for pattern in [
            path(false, false, "Os", "Linux", 0),
            path(false, false, "Arch", "Macos", 0),
            path(true, false, "Os", "Macos", 0),
            path(false, true, "Os", "Macos", 0),
            path(false, false, "Os", "Macos", 1),
        ] {
            assert!(!durable_match_pattern_matches(&pattern, &target));
        }
    }

    #[test]
    fn incremental_binding_preserves_type_then_value_order_and_substitution() {
        let mut binding = binding();
        bind_durable_comptime_argument(
            &mut binding,
            "T",
            &parameter("T", DurableType::ComptimeType),
            typed(
                DurableConstValue::Type(DurableType::I16),
                DurableType::ComptimeType,
            ),
            false,
        )
        .unwrap();
        bind_durable_comptime_argument(
            &mut binding,
            "value",
            &parameter("value", DurableType::GenericParameter(0)),
            typed(DurableConstValue::Integer(12), DurableType::I16),
            false,
        )
        .unwrap();
        let bound = binding.finish();
        assert_eq!(bound.expected_result, DurableType::I16);
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
            &parameter("value", DurableType::I16),
            typed(DurableConstValue::Bool(true), DurableType::Bool),
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
            &parameter("value", DurableType::I8),
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
            &parameter("T", DurableType::ComptimeType),
            typed(DurableConstValue::Unit, DurableType::Unit),
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
            &parameter("T", DurableType::ComptimeType),
            typed(DurableConstValue::Unit, DurableType::Unit),
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
    fn named_value_kernel_is_ordered_and_short_circuits() {
        let mut all_missing = Vec::new();
        assert_eq!(
            resolve_named_value_in_order(|kind| {
                all_missing.push(kind);
                Ok::<Option<()>, ()>(None)
            })
            .unwrap(),
            None
        );
        assert_eq!(
            all_missing,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Function,
                DurableComptimeNamedValueKind::Struct,
                DurableComptimeNamedValueKind::Enum,
            ]
        );

        let mut early_success = Vec::new();
        assert_eq!(
            resolve_named_value_in_order(|kind| {
                early_success.push(kind);
                Ok::<Option<&str>, ()>(
                    (kind == DurableComptimeNamedValueKind::Struct).then_some("struct"),
                )
            })
            .unwrap(),
            Some("struct")
        );
        assert_eq!(
            early_success,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Function,
                DurableComptimeNamedValueKind::Struct,
            ]
        );

        let mut const_failure = Vec::new();
        assert_eq!(
            resolve_named_value_in_order(|kind| {
                const_failure.push(kind);
                if kind == DurableComptimeNamedValueKind::Const {
                    Err::<Option<()>, _>("const failure")
                } else {
                    Ok(None)
                }
            }),
            Err("const failure")
        );
        assert_eq!(const_failure, vec![DurableComptimeNamedValueKind::Const]);

        let mut middle_failure = Vec::new();
        assert_eq!(
            resolve_named_value_in_order(|kind| {
                middle_failure.push(kind);
                if kind == DurableComptimeNamedValueKind::Struct {
                    Err::<Option<()>, _>("struct failure")
                } else {
                    Ok(None)
                }
            }),
            Err("struct failure")
        );
        assert_eq!(
            middle_failure,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Function,
                DurableComptimeNamedValueKind::Struct,
            ]
        );

        let mut module_missing = Vec::new();
        assert_eq!(
            resolve_module_member_in_order(|kind| {
                module_missing.push(kind);
                Ok::<Option<()>, ()>(None)
            })
            .unwrap(),
            None
        );
        assert_eq!(
            module_missing,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Struct,
                DurableComptimeNamedValueKind::Enum,
                DurableComptimeNamedValueKind::Function,
            ]
        );

        let mut module_success = Vec::new();
        assert_eq!(
            resolve_module_member_in_order(|kind| {
                module_success.push(kind);
                Ok::<Option<&str>, ()>(
                    (kind == DurableComptimeNamedValueKind::Enum).then_some("enum"),
                )
            })
            .unwrap(),
            Some("enum")
        );
        assert_eq!(
            module_success,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Struct,
                DurableComptimeNamedValueKind::Enum,
            ]
        );

        let mut module_failure = Vec::new();
        assert_eq!(
            resolve_module_member_in_order(|kind| {
                module_failure.push(kind);
                if kind == DurableComptimeNamedValueKind::Struct {
                    Err::<Option<()>, _>("module struct failure")
                } else {
                    Ok(None)
                }
            }),
            Err("module struct failure")
        );
        assert_eq!(
            module_failure,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Struct,
            ]
        );
    }

    #[test]
    fn target_kernel_covers_all_facts_and_error_channels() {
        for arch in [rue_target::Arch::X86_64, rue_target::Arch::Aarch64] {
            for os in [rue_target::Os::Linux, rue_target::Os::Macos] {
                for data_model in [
                    rue_target::DataModel::Ilp32,
                    rue_target::DataModel::Lp64,
                    rue_target::DataModel::Llp64,
                ] {
                    assert_eq!(
                        resolve_target_intrinsic_facts(
                            ComptimeTargetIntrinsic::Arch,
                            0,
                            arch,
                            os,
                            data_model,
                        )
                        .unwrap()
                        .variant,
                        match arch {
                            rue_target::Arch::X86_64 => "X86_64",
                            rue_target::Arch::Aarch64 => "Aarch64",
                        }
                    );
                    assert_eq!(
                        resolve_target_intrinsic_facts(
                            ComptimeTargetIntrinsic::Os,
                            0,
                            arch,
                            os,
                            data_model,
                        )
                        .unwrap()
                        .variant,
                        match os {
                            rue_target::Os::Linux => "Linux",
                            rue_target::Os::Macos => "Macos",
                        }
                    );
                    assert_eq!(
                        resolve_target_intrinsic_facts(
                            ComptimeTargetIntrinsic::DataModel,
                            0,
                            arch,
                            os,
                            data_model,
                        )
                        .unwrap()
                        .variant,
                        match data_model {
                            rue_target::DataModel::Ilp32 => "Ilp32",
                            rue_target::DataModel::Lp64 => "Lp64",
                            rue_target::DataModel::Llp64 => "Llp64",
                        }
                    );
                }
            }
        }
        for (type_name, variants) in [
            ("Arch", ["X86_64", "Aarch64"].as_slice()),
            ("Os", ["Linux", "Macos"].as_slice()),
            ("DataModel", ["Ilp32", "Lp64", "Llp64"].as_slice()),
        ] {
            for variant in variants {
                assert_eq!(
                    resolve_target_enum_variant_facts(type_name, variant).unwrap(),
                    TargetEnumValue { type_name, variant }
                );
            }
        }
        assert!(matches!(
            resolve_target_intrinsic_facts(
                ComptimeTargetIntrinsic::Os,
                1,
                rue_target::Arch::X86_64,
                rue_target::Os::Linux,
                rue_target::DataModel::Lp64,
            ),
            Err(SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::IntrinsicWrongArgCount { found: 1, .. }
            ))
        ));
        assert!(matches!(
            resolve_target_enum_variant_facts("Target", "X86_64"),
            Err(SemanticNucleusFailure::Resolution(message))
                if message.as_ref() == "unknown target descriptor enum"
        ));
        assert!(matches!(
            resolve_target_enum_variant_facts("Arch", "Linux"),
            Err(SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::UnknownVariant {
                enum_name,
                variant_name,
            })) if enum_name == "Arch" && variant_name == "Linux"
        ));
    }
}

#[cfg(test)]
mod effect_lifecycle_tests {
    use super::*;

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
            for nominal in effects.anonymous_nominals.into_values() {
                self.observe_anonymous_nominal(nominal);
            }
            for dependency in effects.dependencies {
                self.observe_dependency(dependency);
            }
            for gate in effects.deferred_ownership {
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
        let mut direct = observed_effects();
        lifecycle.observe_anonymous_nominal(
            direct
                .anonymous_nominals
                .pop_first()
                .expect("direct nominal observation")
                .1,
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

#[cfg(test)]
mod structured_type_adapter_tests {
    use super::*;
    use crate::durable_semantics::{DurableParameterMode, DurableSemanticParameter};
    use std::cell::{Cell, RefCell};
    use std::convert::Infallible;

    fn assert_frame_domains<
        V: rue_air::ComptimeValue<Type = T>,
        T: rue_air::ComptimeType,
        N: rue_air::ComptimeName,
        F: rue_air::ComptimeFile,
        P: Clone,
        I: rue_air::ComptimeIdentity,
    >(
        _frame: Option<rue_air::ComptimeFrame<V, T, N, F, P, I>>,
    ) {
    }

    struct Provider {
        scope: ModuleId,
    }

    impl rue_air::SemanticModulePathProvider<ModuleId, ModuleId, crate::StableDefinitionKey>
        for Provider
    {
        type Abort = Infallible;
        type Failure = Infallible;

        fn root_module_binding(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticModuleBinding<ModuleId, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_binding(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticModuleBinding<ModuleId, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_display_name(&self, module: &ModuleId) -> Arc<str> {
            Arc::from(module.as_str())
        }

        fn accessing_domain(&self, scope: &ModuleId) -> rue_air::SemanticVisibilityDomain {
            assert_eq!(
                scope, &self.scope,
                "the registry key supplies the exact root scope"
            );
            rue_air::SemanticVisibilityDomain::from_file_path(Some(scope.as_str()))
        }
    }

    #[rustfmt::skip]
    impl rue_air::SemanticTypeSyntaxProvider<ModuleId, ModuleId, crate::StableDefinitionKey, crate::StableDefinitionKey, Arc<str>, DurableType, DurableConstValue> for Provider {
        fn substituted_type(
            &mut self,
            scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<Option<DurableType>, Self::Abort, Self::Failure>
        {
            assert_eq!(scope, &self.scope);
            Ok(None)
        }

        fn primitive_type(
            &mut self,
            name: &str,
        ) -> rue_air::SemanticProviderResult<Option<DurableType>, Self::Abort, Self::Failure>
        {
            Ok(match name {
                "i32" => Some(DurableType::I32),
                "i64" => Some(DurableType::I64),
                _ => None,
            })
        }

        fn builtin_type(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<Option<DurableType>, Self::Abort, Self::Failure>
        {
            Ok(None)
        }

        fn root_struct_type(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn root_enum_type(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn root_type_alias(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_struct_type(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_enum_type(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn module_type_alias(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticTypeFact<DurableType, crate::StableDefinitionKey>>,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn resolve_array_length(
            &mut self,
            _scope: &ModuleId,
            _length: rue_air::SemanticValueSyntax<'_>,
        ) -> rue_air::SemanticProviderResult<Option<u64>, Self::Abort, Self::Failure> {
            unreachable!("fixture has no array syntax")
        }

        fn array_length_from_value(
            &mut self,
            _scope: &ModuleId,
            _value: &DurableConstValue,
        ) -> rue_air::SemanticProviderResult<Option<u64>, Self::Abort, Self::Failure> {
            unreachable!("fixture has no array syntax")
        }

        fn array_type(
            &mut self,
            _element: DurableType,
            _length: Option<u64>,
        ) -> rue_air::SemanticProviderResult<DurableType, Self::Abort, Self::Failure> {
            unreachable!("fixture has no array syntax")
        }

        fn ptr_const_type(
            &mut self,
            _pointee: DurableType,
        ) -> rue_air::SemanticProviderResult<DurableType, Self::Abort, Self::Failure> {
            unreachable!("fixture has no pointer syntax")
        }

        fn ptr_mut_type(
            &mut self,
            _pointee: DurableType,
        ) -> rue_air::SemanticProviderResult<DurableType, Self::Abort, Self::Failure> {
            unreachable!("fixture has no pointer syntax")
        }

        fn slice_type(
            &mut self,
            _scope: &ModuleId,
            _syntax: &str,
            _element: DurableType,
        ) -> rue_air::SemanticProviderResult<DurableType, Self::Abort, Self::Failure> {
            unreachable!("fixture has no slice syntax")
        }

        fn builtin_type_call(
            &mut self,
            _scope: &ModuleId,
            _name: &str,
            _arguments: &[rue_air::SemanticValueSyntax<'_>],
        ) -> rue_air::SemanticProviderResult<Option<DurableType>, Self::Abort, Self::Failure>
        {
            Ok(None)
        }

        fn root_constructor(
            &mut self,
            scope: &ModuleId,
            name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<
                rue_air::SemanticTypeConstructorHead<
                    crate::StableDefinitionKey,
                    Arc<str>,
                    crate::StableDefinitionKey,
                >,
            >,
            Self::Abort,
            Self::Failure,
        > {
            assert_eq!(scope, &self.scope);
            if name != "Wrap" {
                return Ok(None);
            }
            let interleaved = scope.as_str().contains("structured-ordered-frame");
            let constructor = crate::StableDefinitionKey::from_stable_parts(
                scope.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::Function,
                name,
                None,
            );
            Ok(Some(rue_air::SemanticTypeConstructorHead {
                key: constructor.clone(),
                site: constructor,
                parameters: if interleaved {
                    Arc::from([
                        rue_air::SemanticTypeConstructorParameter {
                            name: Arc::from("T0"),
                            is_comptime: true,
                            is_type: true,
                        },
                        rue_air::SemanticTypeConstructorParameter {
                            name: Arc::from("x0"),
                            is_comptime: true,
                            is_type: false,
                        },
                        rue_air::SemanticTypeConstructorParameter {
                            name: Arc::from("T1"),
                            is_comptime: true,
                            is_type: true,
                        },
                        rue_air::SemanticTypeConstructorParameter {
                            name: Arc::from("x1"),
                            is_comptime: true,
                            is_type: false,
                        },
                    ])
                } else {
                    Arc::from([rue_air::SemanticTypeConstructorParameter {
                        name: Arc::from("T"),
                        is_comptime: true,
                        is_type: true,
                    }])
                },
                returns_type: true,
                is_public: true,
                defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                    scope.as_str(),
                )),
                defining_file: Arc::from(scope.as_str()),
            }))
        }

        fn module_constructor(
            &mut self,
            _module: &ModuleId,
            _name: &str,
        ) -> rue_air::SemanticProviderResult<
            Option<
                rue_air::SemanticTypeConstructorHead<
                    crate::StableDefinitionKey,
                    Arc<str>,
                    crate::StableDefinitionKey,
                >,
            >,
            Self::Abort,
            Self::Failure,
        > {
            Ok(None)
        }

        fn resolve_value_argument(
            &mut self,
            scope: &ModuleId,
            _constructor: &str,
            _head: &rue_air::SemanticTypeConstructorHead<
                crate::StableDefinitionKey,
                Arc<str>,
                crate::StableDefinitionKey,
            >,
            _parameter_index: usize,
            _type_arguments: &[(Arc<str>, DurableType)],
            _value_arguments: &[(Arc<str>, DurableConstValue)],
            _syntax: rue_air::SemanticValueSyntax<'_>,
        ) -> rue_air::SemanticProviderResult<DurableConstValue, Self::Abort, Self::Failure>
        {
            if scope.as_str().contains("structured-ordered-frame") {
                return match _syntax {
                    rue_air::SemanticValueSyntax::Integer(value) => {
                        Ok(DurableConstValue::Integer(value))
                    }
                    rue_air::SemanticValueSyntax::Name(_) => Ok(DurableConstValue::Integer(0)),
                };
            }
            unreachable!("fixture constructor has no value argument")
        }

        fn reduce_comptime_call(
            &mut self,
            _head: &rue_air::SemanticTypeConstructorHead<
                crate::StableDefinitionKey,
                Arc<str>,
                crate::StableDefinitionKey,
            >,
            _type_arguments: &[(Arc<str>, DurableType)],
            _value_arguments: &[(Arc<str>, DurableConstValue)],
        ) -> rue_air::SemanticProviderResult<
            Option<rue_air::SemanticComptimeCallResult<DurableType, DurableConstValue>>,
            Self::Abort,
            Self::Failure,
        > {
            unreachable!("the durable host supplies the reduced call result on resume")
        }
    }

    pub(super) fn const_program(
        path: &str,
        argument: &str,
    ) -> Arc<crate::body_query::OwnedComptimeProgramCore> {
        let snapshot = crate::SourceSnapshot::single(
            path,
            format!("const target: Wrap({argument}) = @import(\"{path}\");"),
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
        let artifacts =
            crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || {
                Ok(())
            })
            .unwrap();
        let key = crate::body_query::DurableComptimeProgramKey {
            declaration: crate::StableDefinitionKey::from_stable_parts(
                candidate.module.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::ValueConst,
                "target",
                None,
            ),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: rue_target::Target::X86_64Linux,
                preview_features: crate::StablePreviewFeatures::new(
                    &crate::PreviewFeatures::default(),
                ),
            },
        };
        crate::body_query::OwnedComptimeProgramCore::from_const_body_plan(
            crate::body_query::DurableComptimeProgramPlan { key, candidate },
            &artifacts,
            || Ok(()),
        )
        .unwrap()
    }

    fn const_program_without_imports(
        path: &str,
        argument: &str,
    ) -> Arc<crate::body_query::OwnedComptimeProgramCore> {
        let snapshot = crate::SourceSnapshot::single(
            path,
            format!("const target: Wrap({argument}) = @import(\"{path}\");"),
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
        let artifacts =
            crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || {
                Ok(())
            })
            .unwrap();
        let key = crate::body_query::DurableComptimeProgramKey {
            declaration: crate::StableDefinitionKey::from_stable_parts(
                candidate.module.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::ValueConst,
                "target",
                None,
            ),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: rue_target::Target::X86_64Linux,
                preview_features: crate::StablePreviewFeatures::new(
                    &crate::PreviewFeatures::default(),
                ),
            },
        };
        crate::body_query::OwnedComptimeProgramCore::from_const_body_plan_without_imports(
            crate::body_query::DurableComptimeProgramPlan { key, candidate },
            &artifacts,
            || Ok(()),
        )
        .unwrap()
    }

    pub(super) fn callable_program(path: &str) -> Arc<crate::body_query::OwnedComptimeProgramCore> {
        callable_program_named(path, "target")
    }

    fn callable_program_named(
        path: &str,
        name: &str,
    ) -> Arc<crate::body_query::OwnedComptimeProgramCore> {
        let source = if path.contains("structured-ordered-frame") {
            format!(
                "fn {name}(comptime T0: type, comptime x0: i32, comptime T1: type, comptime x1: i64) -> type {{ [T0; x0] }}"
            )
        } else {
            format!("fn {name}() -> i32 {{ 1 }}")
        };
        let snapshot = crate::SourceSnapshot::single(path, source).unwrap();
        let module = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
            .unwrap()
            .modules()[0]
            .clone();
        let candidate = module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|candidate| candidate.name.as_ref() == name)
            .unwrap()
            .clone();
        let artifacts =
            crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || {
                Ok(())
            })
            .unwrap();
        let producer = crate::StableDefinitionKey::from_stable_parts(
            candidate.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            name,
            None,
        );
        let program = crate::body_query::OwnedForeignComptimeProgram::from_body_plan(
            crate::body_query::DurableComptimeProgramPlan {
                key: crate::body_query::DurableComptimeProgramKey {
                    declaration: producer,
                    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                        target: rue_target::Target::X86_64Linux,
                        preview_features: crate::StablePreviewFeatures::new(
                            &crate::PreviewFeatures::default(),
                        ),
                    },
                },
                candidate,
            },
            &artifacts,
            crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            },
            || Ok(()),
        )
        .unwrap();
        program.core
    }

    pub(super) fn session() -> DurableComptimeSession {
        let module = ModuleId::from_logical_path("structured-parent.rue").unwrap();
        let producer = crate::StableDefinitionKey::from_stable_parts(
            module,
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "parent",
            None,
        );
        let declaration =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&producer)
                .unwrap();
        DurableComptimeSession::new(producer, declaration).unwrap()
    }

    fn fresh_session() -> DurableComptimeSession {
        session()
    }

    fn bound_call(
        admitted: &DurableComptimeAdmittedCall,
        value: Option<i128>,
    ) -> DurableComptimeBoundCall {
        let mut binding = DurableComptimeBinding::new(admitted);
        if let Some(value) = value {
            bind_durable_comptime_argument(
                &mut binding,
                "T",
                &crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from("T"),
                    ty: DurableType::ComptimeType,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                },
                TypedSemanticConst {
                    value: DurableConstValue::Type(DurableType::I32),
                    ty: Some(DurableType::ComptimeType),
                },
                false,
            )
            .unwrap();
            bind_durable_comptime_argument(
                &mut binding,
                "x",
                &crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from("x"),
                    ty: DurableType::I32,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                },
                TypedSemanticConst {
                    value: DurableConstValue::Integer(value),
                    ty: Some(DurableType::I32),
                },
                false,
            )
            .unwrap();
        }
        binding.finish()
    }

    fn test_admitted(
        admission: DurableComptimeCallableAdmission,
        ordinal: u32,
    ) -> DurableComptimeAdmittedCall {
        DurableComptimeAdmittedCall::new(DurableComptimeCallToken::new(0, ordinal), admission)
    }

    fn prepare_call(
        session: &mut DurableComptimeSession,
        ordinal: u32,
        admission: DurableComptimeCallableAdmission,
        value: Option<i128>,
    ) -> DurableComptimePendingCall {
        let admitted = admitted_call(session, ordinal, admission);
        let bound = bound_call(&admitted, value);
        session
            .prepare_bound_expression_call(admitted, bound)
            .unwrap()
    }

    fn admitted_call(
        session: &mut DurableComptimeSession,
        ordinal: u32,
        admission: DurableComptimeCallableAdmission,
    ) -> DurableComptimeAdmittedCall {
        while session.next_call < ordinal {
            let _ = session.reserve_bound_expression_call();
        }
        let reservation = session.reserve_bound_expression_call();
        session
            .admit_bound_expression_call(reservation, admission)
            .unwrap()
    }

    struct PreparedProbeAuthority {
        calls: Cell<usize>,
        expected: RefCell<
            Vec<(
                Vec<(Arc<str>, DurableType)>,
                Vec<(Arc<str>, DurableConstValue)>,
            )>,
        >,
        lookups: RefCell<Vec<ForeignComptimeCallLookup>>,
        abort: Cell<bool>,
    }

    impl DurableComptimeForeignCallAuthority for PreparedProbeAuthority {
        fn probe_comptime_call(
            &self,
            _producer: &crate::StableDefinitionKey,
            type_arguments: &[(Arc<str>, DurableType)],
            value_arguments: &[(Arc<str>, DurableConstValue)],
        ) -> Result<ForeignComptimeCallLookup, QueryAbort> {
            self.calls.set(self.calls.get() + 1);
            if self.abort.get() {
                return Err(QueryAbort::Canceled);
            }
            let (expected_types, expected_values) = self.expected.borrow_mut().remove(0);
            assert_eq!(type_arguments, expected_types.as_slice());
            assert_eq!(value_arguments, expected_values.as_slice());
            Ok(self.lookups.borrow_mut().remove(0))
        }
    }

    fn prepared_admission(
        core: &crate::body_query::OwnedComptimeProgramCore,
    ) -> DurableComptimeCallableAdmission {
        DurableComptimeCallableAdmission {
            candidate: core.plan.candidate.clone(),
            identity: crate::semantic_query_nucleus::DeclarationIdentityProjection {
                key: core.plan.key.declaration.clone(),
                is_public: true,
            },
            configuration: core.plan.key.configuration.clone(),
            parameters: Arc::from([
                crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from("T"),
                    ty: DurableType::ComptimeType,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                },
                crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from("x"),
                    ty: DurableType::I32,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                },
            ]),
            result: DurableType::I32,
            shell_parameters: Arc::from([
                crate::declaration_candidate::DeclarationParameterHeader {
                    name: Arc::from("T"),
                    mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                    is_comptime: true,
                    is_type_parameter: true,
                },
                crate::declaration_candidate::DeclarationParameterHeader {
                    name: Arc::from("x"),
                    mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                    is_comptime: true,
                    is_type_parameter: false,
                },
            ]),
        }
    }

    fn structured_admission(
        pending: &DurableStructuredTypePendingCall,
    ) -> DurableComptimeCallableAdmission {
        let request = &pending.request;
        let parameters: Arc<[_]> = request
            .parameters
            .iter()
            .map(|parameter| DurableSemanticParameter {
                name: parameter.name.clone(),
                ty: if parameter.is_type {
                    DurableType::ComptimeType
                } else {
                    DurableType::I32
                },
                mode: DurableParameterMode::Value,
                is_comptime: parameter.is_comptime,
            })
            .collect::<Vec<_>>()
            .into();
        let shell_parameters: Arc<[_]> = request
            .parameters
            .iter()
            .map(
                |parameter| crate::declaration_candidate::DeclarationParameterHeader {
                    name: parameter.name.clone(),
                    mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                    is_comptime: parameter.is_comptime,
                    is_type_parameter: parameter.is_type,
                },
            )
            .collect::<Vec<_>>()
            .into();
        DurableComptimeCallableAdmission {
            candidate: crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &request.head_key,
            )
            .unwrap(),
            identity: crate::semantic_query_nucleus::DeclarationIdentityProjection {
                key: request.head_key.clone(),
                is_public: true,
            },
            configuration: request.program.configuration.clone(),
            parameters,
            result: DurableType::ComptimeType,
            shell_parameters,
        }
    }

    fn ordered_admission(
        core: &crate::body_query::OwnedComptimeProgramCore,
    ) -> DurableComptimeCallableAdmission {
        let mut admission = prepared_admission(core);
        admission.parameters = Arc::from([
            crate::durable_semantics::DurableSemanticParameter {
                name: Arc::from("T0"),
                ty: DurableType::ComptimeType,
                mode: crate::durable_semantics::DurableParameterMode::Value,
                is_comptime: true,
            },
            crate::durable_semantics::DurableSemanticParameter {
                name: Arc::from("T1"),
                ty: DurableType::ComptimeType,
                mode: crate::durable_semantics::DurableParameterMode::Value,
                is_comptime: true,
            },
            crate::durable_semantics::DurableSemanticParameter {
                name: Arc::from("x0"),
                ty: DurableType::I32,
                mode: crate::durable_semantics::DurableParameterMode::Value,
                is_comptime: true,
            },
            crate::durable_semantics::DurableSemanticParameter {
                name: Arc::from("x1"),
                ty: DurableType::I64,
                mode: crate::durable_semantics::DurableParameterMode::Value,
                is_comptime: true,
            },
        ]);
        admission.shell_parameters = Arc::from([
            crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("T0"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: true,
            },
            crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("T1"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: true,
            },
            crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("x0"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: false,
            },
            crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("x1"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: false,
            },
        ]);
        admission
    }

    fn ordered_bound_call(
        admitted: &DurableComptimeAdmittedCall,
        reverse_types: bool,
        reverse_values: bool,
    ) -> DurableComptimeBoundCall {
        let mut binding = DurableComptimeBinding::new(admitted);
        let types = [("T0", DurableType::I32), ("T1", DurableType::I64)];
        let values = [
            ("x0", DurableConstValue::Integer(10), DurableType::I32),
            ("x1", DurableConstValue::Integer(20), DurableType::I64),
        ];
        let type_order: Vec<_> = if reverse_types {
            types.into_iter().rev().collect()
        } else {
            types.into_iter().collect()
        };
        for (name, ty) in type_order {
            bind_durable_comptime_argument(
                &mut binding,
                name,
                &crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from(name),
                    ty: DurableType::ComptimeType,
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                },
                TypedSemanticConst {
                    value: DurableConstValue::Type(ty),
                    ty: Some(DurableType::ComptimeType),
                },
                false,
            )
            .unwrap();
        }
        let value_order: Vec<_> = if reverse_values {
            values.into_iter().rev().collect()
        } else {
            values.into_iter().collect()
        };
        for (name, value, ty) in value_order {
            bind_durable_comptime_argument(
                &mut binding,
                name,
                &crate::durable_semantics::DurableSemanticParameter {
                    name: Arc::from(name),
                    ty: ty.clone(),
                    mode: crate::durable_semantics::DurableParameterMode::Value,
                    is_comptime: true,
                },
                TypedSemanticConst {
                    value,
                    ty: Some(ty),
                },
                false,
            )
            .unwrap();
        }
        binding.finish()
    }

    fn prepared_authority(
        expected_types: Vec<(Arc<str>, DurableType)>,
        expected_values: Vec<(Arc<str>, DurableConstValue)>,
        lookup: ForeignComptimeCallLookup,
    ) -> PreparedProbeAuthority {
        PreparedProbeAuthority {
            calls: Cell::new(0),
            expected: RefCell::new(vec![(expected_types, expected_values)]),
            lookups: RefCell::new(vec![lookup]),
            abort: Cell::new(false),
        }
    }

    fn prepared_ready_projection(
        ordinal: u32,
    ) -> crate::semantic_query_nucleus::ComptimeCallProjection {
        crate::semantic_query_nucleus::ComptimeCallProjection {
            result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                DurableConstValue::Integer(ordinal.into()),
            ),
            anonymous_nominals: Arc::from([]),
            dependencies: Arc::from([]),
            deferred_ownership: Arc::from([prepared_gate(ordinal)]),
        }
    }

    fn prepared_gate(ordinal: u32) -> DeferredOwnershipGate {
        DeferredOwnershipGate {
            kind: crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireDroppable,
            ty: DurableType::I32,
            source: Arc::new(crate::semantic_query_nucleus::DeferredOwnershipGateSource {
                declaration:
                    crate::revisioned_query_database::declaration_candidate_for_stable_key(
                        &prepared_definition("child"),
                    )
                    .unwrap(),
                start: ordinal,
                end: ordinal + 1,
            }),
            application: None,
        }
    }

    fn prepared_definition(name: &str) -> crate::StableDefinitionKey {
        crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("effects.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from(name),
            None,
        )
    }

    #[test]
    fn prepared_call_probe_is_one_shot_and_preserves_ready_ordinal_and_type() {
        let core = callable_program("prepared-ready.rue");
        let types = vec![(Arc::from("T"), DurableType::I32)];
        let values = vec![(Arc::from("x"), DurableConstValue::Integer(7))];
        let mut session = session();
        let admission = prepared_admission(&core);
        let pending = prepare_call(&mut session, 17, admission.clone(), Some(7));
        let mut authority = prepared_authority(
            types,
            values,
            ForeignComptimeCallLookup::Ready(prepared_ready_projection(17)),
        );
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(pending)
            .unwrap();
        let prepared = session
            .consume_probed_call(probed, rue_span::Span::new(3, 4))
            .unwrap();
        assert!(matches!(
            prepared,
            DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(17)
                ),
                expected_result: DurableType::I32,
            }
        ));
        assert_eq!(authority.calls.get(), 1);
        let effects = session.drain_root_effects().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
                .collect::<Vec<_>>(),
            vec![17]
        );
    }

    #[test]
    fn prepared_call_admission_keeps_frame_and_ticket_from_one_bound_payload() {
        let core = callable_program("prepared-enter.rue");
        let mut session = session();
        let admission = ordered_admission(&core);
        let admitted_call = admitted_call(&mut session, 21, admission.clone());
        let bound = ordered_bound_call(&admitted_call, false, false);
        let pending = session
            .prepare_bound_expression_call(admitted_call, bound)
            .unwrap();
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([
                    (Arc::from("T0"), DurableType::I32),
                    (Arc::from("T1"), DurableType::I64),
                ]),
                value_arguments: Arc::from([
                    (Arc::from("x0"), DurableConstValue::Integer(10)),
                    (Arc::from("x1"), DurableConstValue::Integer(20)),
                ]),
            },
        };
        let mut authority = prepared_authority(
            vec![
                (Arc::from("T0"), DurableType::I32),
                (Arc::from("T1"), DurableType::I64),
            ],
            vec![
                (Arc::from("x0"), DurableConstValue::Integer(10)),
                (Arc::from("x1"), DurableConstValue::Integer(20)),
            ],
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(pending)
            .unwrap();
        let prepared = session
            .consume_probed_call(probed, rue_span::Span::new(11, 15))
            .unwrap();
        let DurableComptimePreparedCall::Enter { frame, mut ticket } = prepared else {
            panic!("admitted prepared call must produce an AIR frame");
        };
        assert_eq!(frame.program, core.plan.key);
        assert_eq!(frame.span, rue_span::Span::new(11, 15));
        assert_eq!(
            frame.expected_result,
            Some(DurableComptimeType(DurableType::I32))
        );
        assert_eq!(
            ticket.canonical_function_producer(&core.plan.key).unwrap(),
            canonical_specialized_function_producer(
                &core.plan.key.declaration,
                &[
                    (Arc::from("T0"), DurableType::I32),
                    (Arc::from("T1"), DurableType::I64),
                ],
                &[
                    (Arc::from("x0"), DurableConstValue::Integer(10)),
                    (Arc::from("x1"), DurableConstValue::Integer(20)),
                ],
            )
            .unwrap()
        );
        let issued = ticket.canonical_function_producer(&core.plan.key).unwrap();
        let type_reversed = canonical_specialized_function_producer(
            &core.plan.key.declaration,
            &[
                (Arc::from("T1"), DurableType::I64),
                (Arc::from("T0"), DurableType::I32),
            ],
            &[
                (Arc::from("x0"), DurableConstValue::Integer(10)),
                (Arc::from("x1"), DurableConstValue::Integer(20)),
            ],
        )
        .unwrap();
        let value_reversed = canonical_specialized_function_producer(
            &core.plan.key.declaration,
            &[
                (Arc::from("T0"), DurableType::I32),
                (Arc::from("T1"), DurableType::I64),
            ],
            &[
                (Arc::from("x1"), DurableConstValue::Integer(20)),
                (Arc::from("x0"), DurableConstValue::Integer(10)),
            ],
        )
        .unwrap();
        assert_ne!(
            issued, type_reversed,
            "type stream order affects ticket identity"
        );
        assert_ne!(
            issued, value_reversed,
            "value stream order affects ticket identity"
        );
        assert_eq!(
            frame.type_bindings,
            AHashMap::from([
                (
                    DurableComptimeName::from("T0"),
                    DurableComptimeType(DurableType::I32),
                ),
                (
                    DurableComptimeName::from("T1"),
                    DurableComptimeType(DurableType::I64),
                ),
            ])
        );
        assert_eq!(
            frame.value_bindings,
            AHashMap::from([
                (
                    DurableComptimeName::from("x0"),
                    EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        DurableConstValue::Integer(10),
                        DurableType::I32,
                    )),
                ),
                (
                    DurableComptimeName::from("x1"),
                    EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        DurableConstValue::Integer(20),
                        DurableType::I64,
                    )),
                ),
            ])
        );
        assert!(session.lifecycle.active.is_empty());
        session.lifecycle.enter(&ticket).unwrap();
        session
            .lifecycle
            .finish(&mut ticket, &rue_air::ComptimeOutcome::<(), ()>::Known(()))
            .unwrap();
        assert!(session.drain_root_effects().unwrap().is_empty());
        assert_eq!(authority.calls.get(), 1);
    }

    #[test]
    fn prepared_call_rejects_cross_paired_admitted_authority_before_registration() {
        let admitted_core = callable_program("prepared-admitted.rue");
        let pending_core = callable_program("prepared-pending.rue");
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: admitted_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([(Arc::from("x"), DurableConstValue::Integer(9))]),
            },
        };
        let mut session = session();
        let admission = prepared_admission(&pending_core);
        let pending = prepare_call(&mut session, 22, admission.clone(), Some(9));
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(9))],
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(pending)
            .unwrap();
        assert!(matches!(
            session.consume_probed_call(probed, rue_span::Span::new(22, 24)),
            Err(DurableComptimeForeignCallError::FrameAdmission(
                DurableComptimeForeignFrameAdmissionError::RegistryMismatch
            ))
        ));
        assert!(
            session
                .registered_program(&admitted_core.plan.key)
                .is_none()
        );
        assert!(session.lifecycle.active.is_empty());

        // Even identical semantic admissions receive distinct call tokens;
        // crossing sibling bound payloads is rejected before an edge exists.
        let first = admitted_call(&mut session, 27, admission.clone());
        let second = admitted_call(&mut session, 28, admission);
        let second_bound = bound_call(&second, Some(2));
        assert!(matches!(
            session.prepare_bound_expression_call(first, second_bound),
            Err(DurableComptimeLifecycleError::BindingMismatch)
        ));
        assert!(session.lifecycle.active.is_empty());
        assert!(session.drain_root_effects().unwrap().is_empty());
        assert_eq!(authority.calls.get(), 1);
    }

    #[test]
    fn prepared_call_rejects_crossed_admission_contract_before_edge_issuance() {
        let core = callable_program("prepared-contract.rue");
        let admission = prepared_admission(&core);
        let mut session = session();

        let mut wrong_result = admission.clone();
        wrong_result.result = DurableType::I64;
        assert!(matches!(
            {
                let admitted = admitted_call(&mut session, 23, admission.clone());
                let wrong = admitted_call(&mut session, 25, wrong_result.clone());
                session.prepare_bound_expression_call(admitted, bound_call(&wrong, Some(1)))
            },
            Err(DurableComptimeLifecycleError::BindingMismatch)
        ));
        assert!(session.lifecycle.active.is_empty());

        let mut wrong_configuration = admission.clone();
        wrong_configuration.configuration.target = rue_target::Target::Aarch64Linux;
        assert!(matches!(
            {
                let admitted = admitted_call(&mut session, 24, admission);
                let wrong = admitted_call(&mut session, 26, wrong_configuration);
                session.prepare_bound_expression_call(admitted, bound_call(&wrong, Some(1)))
            },
            Err(DurableComptimeLifecycleError::BindingMismatch)
        ));
        assert!(session.lifecycle.active.is_empty());
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn prepared_call_preserves_two_ordered_type_and_value_streams() {
        let core = callable_program("prepared-ordered-streams.rue");
        let admission = ordered_admission(&core);
        let mut session = session();
        let first = admitted_call(&mut session, 0, admission.clone());
        let second = admitted_call(&mut session, 1, admission);
        let first_bound = ordered_bound_call(&first, false, false);
        let swapped_bound = ordered_bound_call(&second, true, true);
        let first_view = first_bound.query_view();
        let swapped_view = swapped_bound.query_view();
        assert_ne!(
            first_view.type_arguments(),
            swapped_view.type_arguments(),
            "type argument order is part of the query"
        );
        assert_ne!(
            first_view.value_arguments(),
            swapped_view.value_arguments(),
            "value argument order is part of the query"
        );
        assert!(matches!(
            session.prepare_bound_expression_call(first, swapped_bound),
            Err(DurableComptimeLifecycleError::BindingMismatch)
        ));
        assert!(session.lifecycle.active.is_empty());
    }

    #[test]
    fn prepared_call_siblings_keep_original_ordinals_and_recover_after_abort() {
        let core = callable_program("prepared-siblings.rue");
        let admission = prepared_admission(&core);
        let mut session = session();
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(1))],
            ForeignComptimeCallLookup::NotReady,
        );
        authority.abort.set(true);
        let aborted = prepare_call(&mut session, 31, admission.clone(), Some(1));
        assert!(matches!(
            DurableComptimeServices::new(&mut authority).probe_prepared_call(aborted),
            Err(QueryAbort::Canceled)
        ));
        authority.abort.set(false);
        authority.expected.borrow_mut().clear();
        authority.expected.borrow_mut().push((
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(2))],
        ));
        authority.lookups.borrow_mut().clear();
        authority
            .lookups
            .borrow_mut()
            .push(ForeignComptimeCallLookup::Ready(prepared_ready_projection(
                32,
            )));
        let sibling = prepare_call(&mut session, 32, admission.clone(), Some(2));
        let sibling = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(sibling)
            .unwrap();
        assert!(matches!(
            session.consume_probed_call(sibling, rue_span::Span::new(32, 33)),
            Ok(DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(32)
                ),
                ..
            })
        ));
        assert_eq!(authority.calls.get(), 2);
        let effects = session.drain_root_effects().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
                .collect::<Vec<_>>(),
            vec![32]
        );

        authority.expected.borrow_mut().extend([
            (
                vec![(Arc::from("T"), DurableType::I32)],
                vec![(Arc::from("x"), DurableConstValue::Integer(3))],
            ),
            (
                vec![(Arc::from("T"), DurableType::I32)],
                vec![(Arc::from("x"), DurableConstValue::Integer(4))],
            ),
        ]);
        authority.lookups.borrow_mut().extend([
            ForeignComptimeCallLookup::Ready(prepared_ready_projection(33)),
            ForeignComptimeCallLookup::Ready(prepared_ready_projection(34)),
        ]);
        let first = prepare_call(&mut session, 33, admission.clone(), Some(3));
        let second = prepare_call(&mut session, 34, admission.clone(), Some(4));
        let services = DurableComptimeServices::new(&mut authority);
        let first = services.probe_prepared_call(first).unwrap();
        let second = services.probe_prepared_call(second).unwrap();
        drop(services);
        assert!(matches!(
            session.consume_probed_call(second, rue_span::Span::new(34, 35)),
            Ok(DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(34)
                ),
                ..
            })
        ));
        assert!(matches!(
            session.consume_probed_call(first, rue_span::Span::new(33, 34)),
            Ok(DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(33)
                ),
                ..
            })
        ));
        let effects = session.drain_root_effects().unwrap();
        assert_eq!(
            effects
                .deferred_ownership()
                .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
                .collect::<Vec<_>>(),
            vec![33, 34]
        );
    }

    #[test]
    fn prepared_call_not_ready_then_successful_sibling_uses_one_session() {
        let core = callable_program("prepared-not-ready-sibling.rue");
        let admission = prepared_admission(&core);
        let mut session = session();
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(1))],
            ForeignComptimeCallLookup::NotReady,
        );
        let not_ready = prepare_call(&mut session, 40, admission.clone(), Some(1));
        let not_ready = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(not_ready)
            .unwrap();
        assert!(matches!(
            session.consume_probed_call(not_ready, rue_span::Span::new(40, 41)),
            Ok(DurableComptimePreparedCall::NotReady)
        ));

        authority.expected.borrow_mut().push((
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(2))],
        ));
        authority
            .lookups
            .borrow_mut()
            .push(ForeignComptimeCallLookup::Ready(prepared_ready_projection(
                41,
            )));
        let sibling = prepare_call(&mut session, 41, admission, Some(2));
        let sibling = DurableComptimeServices::new(&mut authority)
            .probe_prepared_call(sibling)
            .unwrap();
        assert!(matches!(
            session.consume_probed_call(sibling, rue_span::Span::new(41, 42)),
            Ok(DurableComptimePreparedCall::Ready {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    DurableConstValue::Integer(41)
                ),
                ..
            })
        ));
        assert_eq!(authority.calls.get(), 2);
        assert_eq!(
            session
                .drain_root_effects()
                .unwrap()
                .deferred_ownership()
                .map(|gate| gate.application.as_ref().unwrap().call_ordinal)
                .collect::<Vec<_>>(),
            vec![41]
        );
    }

    #[test]
    fn prepared_call_terminals_are_consumed_without_retry_or_effects() {
        let terminals = vec![
            ForeignComptimeCallLookup::NotReady,
            ForeignComptimeCallLookup::ReadyFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(Arc::from("ready")),
            ),
            ForeignComptimeCallLookup::ReadyQueryFailure(rue_query::QueryFailure::new(
                "query", "ready",
            )),
            ForeignComptimeCallLookup::AdmissionFailure(
                crate::body_query::ComptimeProgramProjectionFailure::IdentityMismatch,
            ),
            ForeignComptimeCallLookup::UnexpectedReadyProjection,
        ];
        for lookup in terminals {
            let core = callable_program("prepared-terminal.rue");
            let mut session = session();
            let admission = prepared_admission(&core);
            let pending = prepare_call(&mut session, 29, admission.clone(), Some(1));
            let mut authority = prepared_authority(
                vec![(Arc::from("T"), DurableType::I32)],
                vec![(Arc::from("x"), DurableConstValue::Integer(1))],
                lookup,
            );
            let probed = DurableComptimeServices::new(&mut authority)
                .probe_prepared_call(pending)
                .unwrap();
            assert!(matches!(
                session.consume_probed_call(probed, rue_span::Span::new(1, 2)),
                Ok(DurableComptimePreparedCall::NotReady) | Err(_)
            ));
            assert_eq!(authority.calls.get(), 1);
            assert!(session.drain_root_effects().unwrap().is_empty());
        }

        let core = callable_program("prepared-abort.rue");
        let mut session = session();
        let admission = prepared_admission(&core);
        let pending = prepare_call(&mut session, 30, admission.clone(), Some(1));
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            vec![(Arc::from("x"), DurableConstValue::Integer(1))],
            ForeignComptimeCallLookup::NotReady,
        );
        authority.abort.set(true);
        assert!(matches!(
            DurableComptimeServices::new(&mut authority).probe_prepared_call(pending),
            Err(QueryAbort::Canceled)
        ));
        assert_eq!(authority.calls.get(), 1);
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn durable_registry_owns_structured_jobs_across_colliding_programs() {
        let first = const_program("first.rue", "i32");
        let second = const_program("second.rue", "i64");
        let mut session = session();
        session.register_program(&first).unwrap();
        session.register_program(&second).unwrap();
        assert_eq!(
            session.register_program(&first),
            Err(rue_air::ComptimeProgramRegistrationError::AlreadyRegistered)
        );

        let (_, Some(first_root), _) = first.const_root().unwrap() else {
            panic!("first const retains its declared type root");
        };
        let (_, Some(second_root), _) = second.const_root().unwrap() else {
            panic!("second const retains its declared type root");
        };
        assert_eq!(first_root, second_root, "fixture uses colliding dense refs");

        let first_registered = session.registered_program(&first.plan.key).unwrap();
        let second_registered = session.registered_program(&second.plan.key).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first_registered.rir, &first.rir));
        assert!(std::sync::Arc::ptr_eq(&second_registered.rir, &second.rir));
        assert_ne!(first_registered.symbols, second_registered.symbols);
        assert_eq!(first_registered.imports.imports.len(), 1);
        assert_eq!(second_registered.imports.imports.len(), 1);
        assert_eq!(
            first_registered.imports.imports[0].specifier,
            Arc::<str>::from("first.rue")
        );
        assert_eq!(
            second_registered.imports.imports[0].specifier,
            Arc::<str>::from("second.rue")
        );
        let mut wrong_configuration = first.plan.key.configuration.clone();
        wrong_configuration.target = rue_target::Target::Aarch64Linux;
        let wrong_key = rue_air::ComptimeProgramKey {
            declaration: first.plan.key.declaration.clone(),
            configuration: wrong_configuration,
        };
        assert!(session.registered_program(&wrong_key).is_none());

        let mut first_provider = Provider {
            scope: first.plan.candidate.module.clone(),
        };
        let first_poll = begin_durable_structured_type(
            &session,
            &first.plan.key,
            first_root,
            Vec::new(),
            Vec::new(),
            &mut first_provider,
        )
        .unwrap();
        let DurableStructuredTypePoll::Suspended(first_job) = first_poll else {
            panic!("Wrap(i32) suspends for the durable call result");
        };
        assert_eq!(first_job.program().key(), &first.plan.key);
        assert_eq!(
            first_job.type_arguments(),
            &[(Arc::from("T"), DurableType::I32)]
        );
        let first_ready = resume_durable_structured_type(
            *first_job,
            &mut first_provider,
            Ok(Some(rue_air::SemanticComptimeCallResult::Type(
                DurableType::I64,
            ))),
        )
        .unwrap();
        assert!(matches!(
            first_ready,
            DurableStructuredTypePoll::Ready(DurableType::I64)
        ));

        let mut second_provider = Provider {
            scope: second.plan.candidate.module.clone(),
        };
        let second_poll = begin_durable_structured_type(
            &session,
            &second.plan.key,
            second_root,
            Vec::new(),
            Vec::new(),
            &mut second_provider,
        )
        .unwrap();
        let DurableStructuredTypePoll::Suspended(second_job) = second_poll else {
            panic!("colliding program independently suspends");
        };
        assert_eq!(second_job.program().key(), &second.plan.key);
        assert_ne!(second_job.program().key(), &first.plan.key);
        assert_eq!(
            second_job.type_arguments(),
            &[(Arc::from("T"), DurableType::I64)],
            "the second key selects the second arena despite its colliding root ref"
        );

        let mut missing_key = first.plan.key.clone();
        missing_key.declaration = crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("missing.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
            "target",
            None,
        );
        assert!(matches!(
            begin_durable_structured_type(
                &session,
                &missing_key,
                first_root,
                Vec::new(),
                Vec::new(),
                &mut first_provider,
            ),
            Err(DurableStructuredTypeBeginError::UnregisteredProgram)
        ));
        assert!(matches!(
            begin_durable_structured_type(
                &session,
                &first.plan.key,
                rue_rir::RirTypeSyntaxRef::from_u32(u32::MAX),
                Vec::new(),
                Vec::new(),
                &mut first_provider
            ),
            Err(DurableStructuredTypeBeginError::InvalidProgramAuthority)
        ));
    }

    #[test]
    fn structured_type_call_coordinator_consumes_job_and_probe_once() {
        let core = const_program("structured-call-coordinator.rue", "i32");
        let (_, Some(root), _) = core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&core).unwrap();
        let mut provider = Provider {
            scope: core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(9, 10))
            .unwrap();
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Ready(prepared_ready_projection(73)),
        );
        let admission = structured_admission(&pending);
        let validated = match session.validate_structured_type_call(pending, admission) {
            Ok(value) => value,
            Err(error) => panic!("structured fixture contract: {error:?}"),
        };
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let DurableStructuredTypeCall::Ready { result } =
            session.consume_structured_type_call(probed).unwrap()
        else {
            panic!("ready structured call must retain its job");
        };
        assert_eq!(job.program().key(), &core.plan.key);
        assert_eq!(job.type_arguments(), &[(Arc::from("T"), DurableType::I32)]);
        assert!(matches!(
            result,
            crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                DurableConstValue::Integer(73)
            )
        ));
        assert_eq!(authority.calls.get(), 1);
        assert!(session.lifecycle.active.is_empty());
        assert!(!session.drain_root_effects().unwrap().is_empty());

        // A second continuation proves NotReady is a terminal handoff that
        // retains the canonical job without retrying the probe or publishing.
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("second fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(11, 12))
            .unwrap();
        authority
            .expected
            .borrow_mut()
            .push((vec![(Arc::from("T"), DurableType::I32)], Vec::new()));
        authority
            .lookups
            .borrow_mut()
            .push(ForeignComptimeCallLookup::NotReady);
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let DurableStructuredTypeCall::NotReady =
            session.consume_structured_type_call(probed).unwrap()
        else {
            panic!("not-ready structured call must retain its job");
        };
        assert_eq!(authority.calls.get(), 2);
        assert!(session.lifecycle.active.is_empty());
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn structured_type_call_preserves_failure_and_abort_without_publication() {
        let core = const_program("structured-call-terminals.rue", "i32");
        let (_, Some(root), _) = core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&core).unwrap();
        let mut provider = Provider {
            scope: core.plan.candidate.module.clone(),
        };
        let make_pending = |session: &mut DurableComptimeSession, provider: &mut Provider| {
            let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
                session,
                &core.plan.key,
                root,
                Vec::new(),
                Vec::new(),
                provider,
            )
            .unwrap() else {
                panic!("fixture structured call must suspend");
            };
            let pending = session
                .prepare_structured_type_call(&job, rue_span::Span::new(13, 14))
                .unwrap();
            let admission = structured_admission(&pending);
            session
                .validate_structured_type_call(pending, admission)
                .unwrap()
        };

        let mut failure_authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::ReadyFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(Arc::from(
                    "structured failure",
                )),
            ),
        );
        let probed = DurableComptimeServices::new(&mut failure_authority)
            .probe_structured_type_call(make_pending(&mut session, &mut provider))
            .unwrap();
        assert!(matches!(
            session.consume_structured_type_call(probed),
            Err(DurableComptimeForeignCallError::ReadyFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(_)
            ))
        ));
        assert_eq!(failure_authority.calls.get(), 1);
        assert!(session.lifecycle.active.is_empty());
        assert!(session.drain_root_effects().unwrap().is_empty());

        let mut abort_authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::NotReady,
        );
        abort_authority.abort.set(true);
        assert!(matches!(
            DurableComptimeServices::new(&mut abort_authority)
                .probe_structured_type_call(make_pending(&mut session, &mut provider)),
            Err(QueryAbort::Canceled)
        ));
        assert_eq!(abort_authority.calls.get(), 1);
        assert!(session.lifecycle.active.is_empty());
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn structured_frame_validation_rejects_before_probe_or_registry_mutation() {
        let core = const_program("structured-frame-validation.rue", "i32");
        let (_, Some(root), _) = core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&core).unwrap();
        let mut provider = Provider {
            scope: core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(suspension) = begin_durable_structured_type(
            &session,
            &core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&suspension, rue_span::Span::new(90, 91))
            .unwrap();
        let mut invalid = structured_admission(&pending);
        invalid.result = DurableType::I32;
        let serial_before = session.lifecycle.next_serial;
        let programs_before = session.programs.len();
        assert!(matches!(
            session.validate_structured_type_call(pending, invalid),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::ResultNotType
            ))
        ));
        assert_eq!(session.programs.len(), programs_before);
        assert_eq!(session.lifecycle.active.len(), 0);
        assert!(session.lifecycle.effects.is_empty());
        assert_eq!(session.lifecycle.next_serial, serial_before);
    }

    #[test]
    fn structured_non_callable_is_rejected_before_ticket_or_registration() {
        let root_core = const_program("structured-non-callable.rue", "i32");
        let callable_core = callable_program_named("structured-non-callable.rue", "Wrap");
        let (_, Some(root), _) = root_core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&root_core).unwrap();
        let mut provider = Provider {
            scope: root_core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(suspension) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&suspension, rue_span::Span::new(92, 93))
            .unwrap();
        let mut non_callable_core = (*root_core).clone();
        non_callable_core.plan.key = callable_core.plan.key.clone();
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: Arc::new(non_callable_core),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([]),
            },
        };
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let serial_before = session.lifecycle.next_serial;
        let programs_before = session.programs.len();
        assert!(matches!(
            session.consume_structured_type_call(probed),
            Err(DurableComptimeForeignCallError::FrameAdmission(
                DurableComptimeForeignFrameAdmissionError::NotCallable
            ))
        ));
        assert_eq!(authority.calls.get(), 1);
        assert_eq!(session.lifecycle.next_serial, serial_before);
        assert!(session.lifecycle.active.is_empty());
        assert!(session.lifecycle.effects.is_empty());
        assert_eq!(session.programs.len(), programs_before);
        assert!(
            session
                .registered_program(&callable_core.plan.key)
                .is_none()
        );
    }

    #[test]
    fn structured_frame_validation_rejects_name_order_kind_and_value_fit() {
        let root_core = const_program("structured-frame-contract.rue", "i32");
        let callable_core = callable_program_named("structured-frame-contract.rue", "Wrap");
        let (_, Some(root), _) = root_core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&root_core).unwrap();
        let mut provider = Provider {
            scope: root_core.plan.candidate.module.clone(),
        };
        let pending = |session: &mut DurableComptimeSession, provider: &mut Provider| {
            let DurableStructuredTypePoll::Suspended(suspension) = begin_durable_structured_type(
                &*session,
                &root_core.plan.key,
                root,
                Vec::new(),
                Vec::new(),
                provider,
            )
            .unwrap() else {
                panic!("fixture structured call must suspend");
            };
            session
                .prepare_structured_type_call(&suspension, rue_span::Span::new(94, 95))
                .unwrap()
        };

        let name_pending = pending(&mut session, &mut provider);
        let mut name_admission = structured_admission(&name_pending);
        let mut name_parameter = name_admission.parameters[0].clone();
        name_parameter.name = Arc::from("Wrong");
        name_admission.parameters = Arc::from([name_parameter]);
        assert!(matches!(
            session.validate_structured_type_call(name_pending, name_admission),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::InvalidContract
            ))
        ));

        let kind_pending = pending(&mut session, &mut provider);
        let mut kind_admission = structured_admission(&kind_pending);
        let mut kind_shell = kind_admission.shell_parameters[0].clone();
        kind_shell.is_type_parameter = false;
        kind_admission.shell_parameters = Arc::from([kind_shell]);
        assert!(matches!(
            session.validate_structured_type_call(kind_pending, kind_admission),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::InvalidContract
            ))
        ));

        let mut order_pending = pending(&mut session, &mut provider);
        order_pending.request.parameters = Arc::from([
            rue_air::SemanticTypeConstructorParameter {
                name: Arc::from("T0"),
                is_comptime: true,
                is_type: true,
            },
            rue_air::SemanticTypeConstructorParameter {
                name: Arc::from("T1"),
                is_comptime: true,
                is_type: true,
            },
        ]);
        order_pending.request.type_arguments = vec![
            (Arc::from("T0"), DurableType::I32),
            (Arc::from("T1"), DurableType::I64),
        ];
        let mut order_admission = ordered_admission(&callable_core);
        order_admission.result = DurableType::ComptimeType;
        order_admission.parameters = Arc::from([
            order_admission.parameters[1].clone(),
            order_admission.parameters[0].clone(),
        ]);
        order_admission.shell_parameters = Arc::from([
            order_admission.shell_parameters[1].clone(),
            order_admission.shell_parameters[0].clone(),
        ]);
        assert!(matches!(
            session.validate_structured_type_call(order_pending, order_admission),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::InvalidContract
            ))
        ));

        let mut fit_pending = pending(&mut session, &mut provider);
        fit_pending.request.parameters = Arc::from([rue_air::SemanticTypeConstructorParameter {
            name: Arc::from("x"),
            is_comptime: true,
            is_type: false,
        }]);
        fit_pending.request.type_arguments.clear();
        fit_pending.request.value_arguments =
            vec![(Arc::from("x"), DurableConstValue::Integer(i128::MAX))];
        let mut fit_admission = structured_admission(&fit_pending);
        fit_admission.result = DurableType::ComptimeType;
        fit_admission.parameters = Arc::from([DurableSemanticParameter {
            name: Arc::from("x"),
            ty: DurableType::I32,
            mode: DurableParameterMode::Value,
            is_comptime: true,
        }]);
        fit_admission.shell_parameters =
            Arc::from([crate::declaration_candidate::DeclarationParameterHeader {
                name: Arc::from("x"),
                mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                is_comptime: true,
                is_type_parameter: false,
            }]);
        assert!(matches!(
            session.validate_structured_type_call(fit_pending, fit_admission),
            Err(DurableComptimeForeignCallError::StructuredFrame(
                DurableComptimeStructuredFrameAdmissionError::ValueFit(_)
            ))
        ));
    }

    #[test]
    fn structured_frame_admission_preserves_ordered_typed_bindings_and_lifecycle() {
        let root_core = const_program("structured-ordered-frame.rue", "i32, 10, i64, 20");
        let callable_core = callable_program_named("structured-ordered-frame.rue", "Wrap");
        let (_, Some(root), _) = root_core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&root_core).unwrap();
        let mut provider = Provider {
            scope: root_core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(suspension) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&suspension, rue_span::Span::new(1000, 1001))
            .unwrap();
        let mut admission = ordered_admission(&callable_core);
        admission.result = DurableType::ComptimeType;
        admission.parameters = Arc::from([
            admission.parameters[0].clone(),
            admission.parameters[2].clone(),
            admission.parameters[1].clone(),
            admission.parameters[3].clone(),
        ]);
        admission.shell_parameters = Arc::from([
            admission.shell_parameters[0].clone(),
            admission.shell_parameters[2].clone(),
            admission.shell_parameters[1].clone(),
            admission.shell_parameters[3].clone(),
        ]);
        assert_eq!(
            admission.candidate,
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &pending.request.head_key
            )
            .unwrap()
        );
        assert_eq!(admission.identity.key, pending.request.head_key);
        assert_eq!(
            admission.configuration,
            pending.request.program.configuration
        );
        assert_eq!(admission.parameters.len(), 4);
        assert_eq!(admission.shell_parameters.len(), 4);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: callable_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([
                    (Arc::from("T0"), DurableType::I32),
                    (Arc::from("T1"), DurableType::I64),
                ]),
                value_arguments: Arc::from([
                    (Arc::from("x0"), DurableConstValue::Integer(10)),
                    (Arc::from("x1"), DurableConstValue::Integer(20)),
                ]),
            },
        };
        let mut authority = prepared_authority(
            vec![
                (Arc::from("T0"), DurableType::I32),
                (Arc::from("T1"), DurableType::I64),
            ],
            vec![
                (Arc::from("x0"), DurableConstValue::Integer(10)),
                (Arc::from("x1"), DurableConstValue::Integer(20)),
            ],
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let DurableStructuredTypeCall::Enter {
            frame, mut ticket, ..
        } = session.consume_structured_type_call(probed).unwrap()
        else {
            panic!("ordered structured call must admit a frame");
        };
        assert_eq!(authority.calls.get(), 1);
        assert_eq!(frame.span, rue_span::Span::new(1000, 1001));
        assert_ne!(frame.span, frame.function_span);
        assert_eq!(frame.call_identity, None);
        assert_eq!(
            frame.type_bindings.get(&DurableComptimeName::from("T0")),
            Some(&DurableComptimeType::from(DurableType::I32))
        );
        assert_eq!(
            frame.type_bindings.get(&DurableComptimeName::from("T1")),
            Some(&DurableComptimeType::from(DurableType::I64))
        );
        assert_eq!(
            frame.value_bindings.get(&DurableComptimeName::from("x0")),
            Some(&EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(10),
                DurableType::I32,
            )))
        );
        assert_eq!(
            frame.value_bindings.get(&DurableComptimeName::from("x1")),
            Some(&EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(20),
                DurableType::I64,
            )))
        );
        assert_eq!(
            frame.expected_result,
            Some(DurableComptimeType::from(DurableType::ComptimeType))
        );
        assert!(session.lifecycle.active.is_empty());
        session.enter_call(&ticket).unwrap();
        assert_eq!(session.lifecycle.active.len(), 1);
        session
            .finish_call(&mut ticket, &rue_air::ComptimeOutcome::<(), ()>::Known(()))
            .unwrap();
        assert!(session.lifecycle.active.is_empty());
        assert!(session.drain_root_effects().unwrap().is_empty());
    }

    #[test]
    fn structured_type_call_admission_returns_dormant_ticket_after_validation() {
        let root_core = const_program("structured-call-enter.rue", "i32");
        let callable_core = callable_program_named("structured-call-enter.rue", "Wrap");
        let (_, Some(root), _) = root_core.const_root().unwrap() else {
            panic!("fixture retains its declared type root");
        };
        let mut session = session();
        session.register_program(&root_core).unwrap();
        let mut provider = Provider {
            scope: root_core.plan.candidate.module.clone(),
        };
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(15, 16))
            .unwrap();
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: callable_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([]),
            },
        };
        let mut authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Admitted(admitted),
        );
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let probed = DurableComptimeServices::new(&mut authority)
            .probe_structured_type_call(validated)
            .unwrap();
        let DurableStructuredTypeCall::Enter {
            program,
            frame,
            mut ticket,
        } = session.consume_structured_type_call(probed).unwrap()
        else {
            panic!("admitted structured call must issue a dormant ticket");
        };
        assert_eq!(program.plan.key, callable_core.plan.key);
        assert_eq!(frame.span, rue_span::Span::new(15, 16));
        assert_eq!(frame.call_identity, None);
        assert_eq!(
            frame.expected_result,
            Some(DurableComptimeType::from(DurableType::ComptimeType))
        );
        assert_eq!(
            frame.context.as_ref().unwrap().program(),
            &callable_core.plan.key
        );
        assert_eq!(
            frame.type_bindings.get(&DurableComptimeName::from("T")),
            Some(&DurableComptimeType::from(DurableType::I32))
        );
        assert!(frame.value_bindings.is_empty());
        assert!(
            session
                .registered_program(&callable_core.plan.key)
                .is_some()
        );
        assert!(session.lifecycle.active.is_empty());
        assert_eq!(
            ticket
                .canonical_function_producer(&callable_core.plan.key)
                .unwrap(),
            canonical_specialized_function_producer(
                &callable_core.plan.key.declaration,
                &[(Arc::from("T"), DurableType::I32)],
                &[],
            )
            .unwrap()
        );
        assert_eq!(
            session.finish_call(&mut ticket, &rue_air::ComptimeOutcome::<(), ()>::Known(())),
            Err(DurableComptimeLifecycleError::NotEntered)
        );
        assert!(session.lifecycle.active.is_empty());
        assert!(session.drain_root_effects().unwrap().is_empty());

        // A returned program with the wrong head contract is rejected only
        // after the dormant ticket is issued, and cannot enter the registry.
        let wrong_core = callable_program_named("structured-call-enter.rue", "Other");
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("second fixture structured call must suspend");
        };
        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(17, 18))
            .unwrap();
        let wrong_program = crate::body_query::OwnedForeignComptimeProgram {
            core: wrong_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([]),
            },
        };
        let mut wrong_authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Admitted(wrong_program),
        );
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let probed = DurableComptimeServices::new(&mut wrong_authority)
            .probe_structured_type_call(validated)
            .unwrap();
        assert!(matches!(
            session.consume_structured_type_call(probed),
            Err(DurableComptimeForeignCallError::FrameAdmission(
                DurableComptimeForeignFrameAdmissionError::RegistryMismatch
            ))
        ));
        assert!(session.registered_program(&wrong_core.plan.key).is_none());
        assert!(session.lifecycle.active.is_empty());

        // An edge issued by another session fails before any registry
        // mutation, even when its probe carries an otherwise valid admitted
        // child.  This exercises the full ticket/authority handoff rather
        // than only the Ready projection path.
        let DurableStructuredTypePoll::Suspended(job) = begin_durable_structured_type(
            &session,
            &root_core.plan.key,
            root,
            Vec::new(),
            Vec::new(),
            &mut provider,
        )
        .unwrap() else {
            panic!("third fixture structured call must suspend");
        };

        // A capability issued by this session cannot be prepared by another
        // session, even when that session has registered the same stable key.
        // Reject before the foreign session issues a serial/edge or changes
        // its lifecycle/effect state.
        let mut other_session = fresh_session();
        other_session.register_program(&root_core).unwrap();
        let serial_before = other_session.lifecycle.next_serial;
        let active_before = other_session.lifecycle.active.clone();
        assert!(matches!(
            other_session.prepare_structured_type_call(&job, rue_span::Span::new(19, 20)),
            Err(DurableComptimeLifecycleError::InvalidProgramAuthority)
        ));
        assert_eq!(other_session.lifecycle.next_serial, serial_before);
        assert_eq!(other_session.lifecycle.active, active_before);
        assert!(other_session.lifecycle.effects.is_empty());
        assert!(
            other_session
                .registered_program(&root_core.plan.key)
                .is_some()
        );

        let pending = session
            .prepare_structured_type_call(&job, rue_span::Span::new(19, 20))
            .unwrap();
        let admission = structured_admission(&pending);
        let validated = session
            .validate_structured_type_call(pending, admission)
            .unwrap();
        let mut foreign_session = fresh_session();
        let foreign_program = crate::body_query::OwnedForeignComptimeProgram {
            core: callable_core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
                value_arguments: Arc::from([]),
            },
        };
        let mut foreign_authority = prepared_authority(
            vec![(Arc::from("T"), DurableType::I32)],
            Vec::new(),
            ForeignComptimeCallLookup::Admitted(foreign_program),
        );
        let probed = DurableComptimeServices::new(&mut foreign_authority)
            .probe_structured_type_call(validated)
            .unwrap();
        assert!(matches!(
            foreign_session.consume_structured_type_call(probed),
            Err(DurableComptimeForeignCallError::Lifecycle(
                DurableComptimeLifecycleError::TicketMismatch
            ))
        ));
        assert!(
            foreign_session
                .registered_program(&root_core.plan.key)
                .is_none()
        );
        assert!(foreign_session.lifecycle.active.is_empty());
    }

    #[test]
    fn keyed_import_sites_preserve_program_local_occurrences_and_reject_mismatches() {
        let first = const_program("first-import.rue", "i32");
        let second = const_program("second-import.rue", "i64");
        let mut session = session();
        session.register_program(&first).unwrap();
        session.register_program(&second).unwrap();

        let first_registered = session.registered_program(&first.plan.key).unwrap();
        let second_registered = session.registered_program(&second.plan.key).unwrap();
        let first_occurrence = first_registered.imports.imports[0].inst;
        let second_occurrence = second_registered.imports.imports[0].inst;
        assert_eq!(first_occurrence, second_occurrence);

        let first_site = session
            .import_site_for_instruction(&first.plan.key, first_occurrence, "first-import.rue")
            .unwrap();
        assert_eq!(first_site.occurrence(), 0);
        assert_eq!(first_site.kind(), rue_air::ComptimeSiteKind::Import);
        assert_eq!(first_site.program(), &first.plan.key);

        let second_site = session
            .import_site_for_instruction(&second.plan.key, second_occurrence, "second-import.rue")
            .unwrap();
        assert_eq!(second_site.occurrence(), 0);
        assert_eq!(second_site.kind(), rue_air::ComptimeSiteKind::Import);
        assert_eq!(second_site.program(), &second.plan.key);
        assert_ne!(first_site.program(), second_site.program());

        assert!(matches!(
            session.import_site_for_instruction(
                &first.plan.key,
                first_occurrence,
                "second-import.rue",
            ),
            Err(DurableComptimeKeyedImportError::SpecifierMismatch)
        ));
        assert!(matches!(
            session.import_site_for_instruction(
                &first.plan.key,
                rue_rir::InstRef::from_raw(u32::MAX),
                "first-import.rue",
            ),
            Err(DurableComptimeKeyedImportError::UnknownInstruction)
        ));
        let mut wrong_key = first.plan.key.clone();
        wrong_key.configuration.target = rue_target::Target::Aarch64Linux;
        assert!(matches!(
            session.import_site_for_instruction(&wrong_key, first_occurrence, "first-import.rue",),
            Err(DurableComptimeKeyedImportError::UnknownProgram)
        ));

        // A caller cannot pair the first key with the second program: the
        // second instruction is interpreted only against the first registry
        // entry and therefore fails before any import query/effect exists.
        assert!(matches!(
            session.import_site_for_instruction(
                &first.plan.key,
                second_occurrence,
                "second-import.rue",
            ),
            Err(DurableComptimeKeyedImportError::SpecifierMismatch)
                | Err(DurableComptimeKeyedImportError::UnknownInstruction)
        ));
    }

    enum ImportServiceMode {
        Missing,
        Failure(crate::declaration_candidate::DeclarationImportFailure),
        Abort,
    }

    #[derive(Clone, Copy)]
    enum CallFinishMode {
        Success,
        Failure,
        Abort,
    }

    struct ImportServiceAuthority {
        calls: Cell<usize>,
        mode: ImportServiceMode,
        keyed_heads: RefCell<Vec<crate::StableDefinitionKey>>,
        finish_modes: RefCell<Vec<Vec<crate::durable_semantics::DurableParameterMode>>>,
        finish_mode: CallFinishMode,
    }

    impl DurableComptimeSemanticAuthority for ImportServiceAuthority {
        fn check_canceled(&self) -> Result<(), QueryAbort> {
            panic!("not part of keyed import service test")
        }

        fn resolve_type_syntax(
            &mut self,
            _program: &crate::body_query::DurableComptimeProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
        ) -> Result<
            DurableType,
            rue_air::SemanticTypeSyntaxError<
                QueryAbort,
                SemanticNucleusFailure,
                crate::StableDefinitionKey,
                Arc<str>,
            >,
        > {
            panic!("not part of keyed import service test")
        }

        fn resolve_type_syntax_with_substitutions(
            &mut self,
            _program: &crate::body_query::DurableComptimeProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
            _type_substitutions: &[(Arc<str>, DurableType)],
            _value_substitutions: &[(Arc<str>, DurableConstValue)],
        ) -> Result<
            DurableType,
            rue_air::SemanticTypeSyntaxError<
                QueryAbort,
                SemanticNucleusFailure,
                crate::StableDefinitionKey,
                Arc<str>,
            >,
        > {
            panic!("not part of keyed import service test")
        }

        fn begin_structured_type(
            &mut self,
            _program: &crate::body_query::DurableComptimeProgramKey,
            _syntax: rue_rir::RirTypeSyntaxRef,
            _type_substitutions: Vec<(Arc<str>, DurableType)>,
            _value_substitutions: Vec<(Arc<str>, DurableConstValue)>,
        ) -> Result<
            DurableStructuredTypePoll,
            DurableStructuredTypeBeginError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn resume_structured_type(
            &mut self,
            _job: DurableStructuredTypeJob,
            _reduced: rue_air::SemanticProviderResult<
                Option<rue_air::SemanticComptimeCallResult<DurableType, DurableConstValue>>,
                QueryAbort,
                SemanticNucleusFailure,
            >,
        ) -> Result<
            DurableStructuredTypePoll,
            rue_air::SemanticTypeSyntaxError<
                QueryAbort,
                SemanticNucleusFailure,
                crate::StableDefinitionKey,
                Arc<str>,
            >,
        > {
            panic!("not part of keyed import service test")
        }

        fn begin_comptime_call_admission(
            &self,
            _accessing_source: &crate::StableDefinitionKey,
            _module: &ModuleId,
            _name: &str,
        ) -> Result<
            DurableComptimeCallableAdmissionStart,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn begin_comptime_call_admission_for_key(
            &self,
            accessing_source: &crate::StableDefinitionKey,
            head: &crate::StableDefinitionKey,
        ) -> Result<
            DurableComptimeCallableAdmissionStart,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            self.keyed_heads.borrow_mut().push(head.clone());
            let candidate =
                crate::revisioned_query_database::declaration_candidate_for_stable_key(head)
                    .expect("test keyed head has a canonical candidate");
            let identity = crate::semantic_query_nucleus::DeclarationIdentityProjection {
                key: head.clone(),
                is_public: true,
            };
            Ok(DurableComptimeCallableAdmissionStart {
                candidate,
                identity,
                configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                    target: rue_target::Target::X86_64Linux,
                    preview_features: crate::StablePreviewFeatures::new(
                        &crate::PreviewFeatures::default(),
                    ),
                },
                name: Arc::from(head.name()),
                dependency: SemanticDeclarationDependency {
                    source: accessing_source.clone(),
                    kind: rue_air::DeclarationTypeDependencyKind::Body,
                    target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        head.clone(),
                    ),
                },
            })
        }

        fn finish_comptime_call_admission(
            &self,
            start: DurableComptimeCallableAdmissionStart,
            argument_modes: &[crate::durable_semantics::DurableParameterMode],
        ) -> Result<
            DurableComptimeCallableAdmission,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            self.finish_modes.borrow_mut().push(argument_modes.to_vec());
            match self.finish_mode {
                CallFinishMode::Success => {
                    let parameters: Arc<[crate::durable_semantics::DurableSemanticParameter]> =
                        Arc::from([
                            crate::durable_semantics::DurableSemanticParameter {
                                name: Arc::from("first"),
                                ty: DurableType::I32,
                                mode: crate::durable_semantics::DurableParameterMode::Value,
                                is_comptime: true,
                            },
                            crate::durable_semantics::DurableSemanticParameter {
                                name: Arc::from("second"),
                                ty: DurableType::I64,
                                mode: crate::durable_semantics::DurableParameterMode::Value,
                                is_comptime: true,
                            },
                        ]);
                    let shell_parameters: Arc<
                        [crate::declaration_candidate::DeclarationParameterHeader],
                    > = Arc::from([
                        crate::declaration_candidate::DeclarationParameterHeader {
                            name: Arc::from("first"),
                            mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                            is_comptime: true,
                            is_type_parameter: false,
                        },
                        crate::declaration_candidate::DeclarationParameterHeader {
                            name: Arc::from("second"),
                            mode: crate::declaration_candidate::DeclarationParameterMode::Value,
                            is_comptime: true,
                            is_type_parameter: false,
                        },
                    ]);
                    Ok(DurableComptimeCallableAdmission {
                        candidate: start.candidate,
                        identity: start.identity,
                        configuration: start.configuration,
                        parameters,
                        result: DurableType::ComptimeType,
                        shell_parameters,
                    })
                }
                CallFinishMode::Failure => Err(rue_air::SemanticProviderError::Failure(
                    SemanticNucleusFailure::Resolution(Arc::from("structured finish failed")),
                )),
                CallFinishMode::Abort => {
                    Err(rue_air::SemanticProviderError::Abort(QueryAbort::Canceled))
                }
            }
        }

        fn resolve_named_value(
            &self,
            _accessing_source: &crate::StableDefinitionKey,
            _module: &ModuleId,
            _name: &str,
        ) -> Result<
            Option<DurableComptimeNamedValueProjection>,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn resolve_module_member(
            &self,
            _accessing_source: &crate::StableDefinitionKey,
            _module: &ModuleId,
            _member: &str,
        ) -> Result<
            DurableComptimeNamedValueProjection,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn resolve_import(
            &self,
            _site: &DurableImportSite,
        ) -> Result<DurableImportResolution, QueryAbort> {
            panic!("keyed import service must not use the unkeyed import operation")
        }

        fn resolve_keyed_import(
            &self,
            _site: &rue_air::ComptimeSite<crate::body_query::DurableComptimeProgramKey>,
            _specifier: &str,
        ) -> Result<DurableImportResolution, DurableComptimeKeyedImportError> {
            self.calls.set(self.calls.get() + 1);
            match &self.mode {
                ImportServiceMode::Missing => Ok(DurableImportResolution::Missing),
                ImportServiceMode::Failure(failure) => {
                    Ok(DurableImportResolution::Failure(failure.clone()))
                }
                ImportServiceMode::Abort => Err(DurableComptimeKeyedImportError::ProviderAbort(
                    QueryAbort::Canceled,
                )),
            }
        }

        fn resolve_target_intrinsic(
            &self,
            _intrinsic: ComptimeTargetIntrinsic,
            _argument_count: usize,
        ) -> Result<
            TargetEnumValue,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }

        fn resolve_target_enum_variant(
            &self,
            _type_name: &str,
            _variant: &str,
        ) -> Result<
            TargetEnumValue,
            rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
        > {
            panic!("not part of keyed import service test")
        }
    }

    #[test]
    fn keyed_callable_admission_uses_exact_head_without_spelling_lookup() {
        let first = super::structured_type_adapter_tests::callable_program("structured-head-a.rue");
        let second =
            super::structured_type_adapter_tests::callable_program("structured-head-b.rue");
        let first_head = first.plan.key.declaration.clone();
        let second_head = second.plan.key.declaration.clone();
        assert_ne!(first_head, second_head);
        let accessing_source = crate::StableDefinitionKey::from_stable_parts(
            first_head.module().clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "caller",
            None,
        );
        let mut authority = ImportServiceAuthority {
            calls: Cell::new(0),
            mode: ImportServiceMode::Missing,
            keyed_heads: RefCell::new(Vec::new()),
            finish_modes: RefCell::new(Vec::new()),
            finish_mode: CallFinishMode::Success,
        };
        let services = DurableComptimeServices::new(&mut authority);
        let first_admission = services
            .begin_comptime_call_admission_for_key(&accessing_source, &first_head)
            .unwrap();
        let second_admission = services
            .begin_comptime_call_admission_for_key(&accessing_source, &second_head)
            .unwrap();

        assert_eq!(first_admission.identity.key, first_head);
        assert_eq!(second_admission.identity.key, second_head);
        assert_eq!(
            first_admission.candidate.module,
            first_head.module().clone()
        );
        assert_eq!(
            second_admission.candidate.module,
            second_head.module().clone()
        );
        assert_eq!(
            authority.keyed_heads.borrow().as_slice(),
            &[first_head, second_head]
        );
        // This test authority intentionally has no module/name candidate path;
        // reaching both starts proves the structured seam carries the exact
        // canonical head through the services facade.
    }

    #[test]
    fn structured_callable_finish_uses_all_value_modes_and_preserves_begin_effects() {
        let program = callable_program("structured-finish.rue");
        let head = program.plan.key.declaration.clone();
        let source = crate::StableDefinitionKey::from_stable_parts(
            head.module().clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "structured-caller",
            None,
        );
        let dependency = |start: &DurableComptimeCallableAdmissionStart| start.dependency.clone();
        let mut authority = ImportServiceAuthority {
            calls: Cell::new(0),
            mode: ImportServiceMode::Missing,
            keyed_heads: RefCell::new(Vec::new()),
            finish_modes: RefCell::new(Vec::new()),
            finish_mode: CallFinishMode::Success,
        };
        let mut session = session();
        let initial_serial = session.lifecycle.next_serial;
        let initial_active = session.lifecycle.active.clone();
        let initial_programs = session.programs.len();
        let start = {
            let services = DurableComptimeServices::new(&mut authority);
            services
                .begin_comptime_call_admission_for_key(&source, &head)
                .unwrap()
        };
        session.observe_dependency(dependency(&start));
        let admission = {
            let services = DurableComptimeServices::new(&mut authority);
            services
                .finish_structured_comptime_call_admission(start, 2)
                .unwrap()
        };
        assert_eq!(admission.identity.key, head);
        assert_eq!(admission.result, DurableType::ComptimeType);
        assert_eq!(admission.parameters[0].name.as_ref(), "first");
        assert_eq!(admission.parameters[0].ty, DurableType::I32);
        assert_eq!(admission.parameters[1].name.as_ref(), "second");
        assert_eq!(admission.parameters[1].ty, DurableType::I64);
        assert_eq!(
            admission
                .shell_parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect::<Vec<_>>(),
            vec![Arc::from("first"), Arc::from("second")]
        );
        assert_eq!(
            authority.finish_modes.borrow().as_slice(),
            &[[
                crate::durable_semantics::DurableParameterMode::Value,
                crate::durable_semantics::DurableParameterMode::Value,
            ]]
        );
        assert_eq!(
            session.drain_root_effects().unwrap().dependencies().count(),
            1
        );
        assert!(session.drain_root_effects().unwrap().is_empty());
        assert_eq!(session.lifecycle.next_serial, initial_serial);
        assert_eq!(session.lifecycle.active, initial_active);
        assert_eq!(session.programs.len(), initial_programs);

        for finish_mode in [CallFinishMode::Failure, CallFinishMode::Abort] {
            let mut authority = ImportServiceAuthority {
                calls: Cell::new(0),
                mode: ImportServiceMode::Missing,
                keyed_heads: RefCell::new(Vec::new()),
                finish_modes: RefCell::new(Vec::new()),
                finish_mode,
            };
            let start = {
                let services = DurableComptimeServices::new(&mut authority);
                services
                    .begin_comptime_call_admission_for_key(&source, &head)
                    .unwrap()
            };
            let mut session = fresh_session();
            let initial_serial = session.lifecycle.next_serial;
            let initial_active = session.lifecycle.active.clone();
            let initial_programs = session.programs.len();
            session.observe_dependency(start.dependency.clone());
            let result = {
                let services = DurableComptimeServices::new(&mut authority);
                services.finish_structured_comptime_call_admission(start, 2)
            };
            match finish_mode {
                CallFinishMode::Failure => assert!(matches!(
                    result,
                    Err(rue_air::SemanticProviderError::Failure(
                        SemanticNucleusFailure::Resolution(_)
                    ))
                )),
                CallFinishMode::Abort => assert!(matches!(
                    result,
                    Err(rue_air::SemanticProviderError::Abort(QueryAbort::Canceled))
                )),
                CallFinishMode::Success => unreachable!(),
            }
            assert_eq!(
                session.drain_root_effects().unwrap().dependencies().count(),
                1
            );
            assert!(session.drain_root_effects().unwrap().is_empty());
            assert_eq!(session.lifecycle.next_serial, initial_serial);
            assert_eq!(session.lifecycle.active, initial_active);
            assert_eq!(session.programs.len(), initial_programs);
        }
    }

    #[test]
    fn keyed_import_service_preserves_terminals_and_skips_structural_rejections() {
        let first = const_program("service-import.rue", "i32");
        let mut session = session();
        session.register_program(&first).unwrap();
        let instruction = session
            .registered_program(&first.plan.key)
            .unwrap()
            .imports
            .imports[0]
            .inst;
        let site = session
            .import_site_for_instruction(&first.plan.key, instruction, "service-import.rue")
            .unwrap();
        let import_key = crate::declaration_candidate::DeclarationImportSiteKey {
            declaration: first.plan.candidate.clone(),
            occurrence: 0,
            specifier: Arc::from("service-import.rue"),
        };

        for (mode, expected) in [
            (ImportServiceMode::Missing, DurableImportResolution::Missing),
            (
                ImportServiceMode::Failure(
                    crate::declaration_candidate::DeclarationImportFailure::ResolutionUnavailable(
                        import_key.clone(),
                    ),
                ),
                DurableImportResolution::Failure(
                    crate::declaration_candidate::DeclarationImportFailure::ResolutionUnavailable(
                        import_key.clone(),
                    ),
                ),
            ),
        ] {
            let calls = Cell::new(0);
            let mut authority = ImportServiceAuthority {
                calls,
                mode,
                keyed_heads: RefCell::new(Vec::new()),
                finish_modes: RefCell::new(Vec::new()),
                finish_mode: CallFinishMode::Success,
            };
            let services = DurableComptimeServices::new(&mut authority);
            assert_eq!(
                services
                    .resolve_keyed_import(&site, "service-import.rue")
                    .unwrap(),
                expected
            );
            assert_eq!(authority.calls.get(), 1);
        }

        let calls = Cell::new(0);
        let mut authority = ImportServiceAuthority {
            calls,
            mode: ImportServiceMode::Abort,
            keyed_heads: RefCell::new(Vec::new()),
            finish_modes: RefCell::new(Vec::new()),
            finish_mode: CallFinishMode::Success,
        };
        let services = DurableComptimeServices::new(&mut authority);
        assert!(matches!(
            services.resolve_keyed_import(&site, "service-import.rue"),
            Err(DurableComptimeKeyedImportError::ProviderAbort(
                QueryAbort::Canceled
            ))
        ));
        assert_eq!(authority.calls.get(), 1);

        let wrong_kind = rue_air::ComptimeSite::from_occurrence(
            first.plan.key.clone(),
            rue_air::ComptimeSiteKind::Intrinsic,
            site.occurrence(),
            site.span(),
        );
        let mut authority = ImportServiceAuthority {
            calls: Cell::new(0),
            mode: ImportServiceMode::Missing,
            keyed_heads: RefCell::new(Vec::new()),
            finish_modes: RefCell::new(Vec::new()),
            finish_mode: CallFinishMode::Success,
        };
        let services = DurableComptimeServices::new(&mut authority);
        assert!(matches!(
            services.resolve_keyed_import(&wrong_kind, "service-import.rue"),
            Err(DurableComptimeKeyedImportError::WrongSiteKind)
        ));
        assert_eq!(authority.calls.get(), 0);
    }

    #[test]
    fn registered_const_core_receives_finalized_imports_without_authority_replacement() {
        let core = const_program_without_imports("finalize.rue", "i32");
        let key = core.plan.key.clone();
        let mut session = session();
        session.register_program(&core).unwrap();
        assert!(
            session
                .registered_program(&key)
                .unwrap()
                .imports
                .imports
                .is_empty()
        );

        let finalized =
            crate::body_query::OwnedComptimeProgramCore::finalize_imports(core, || Ok(())).unwrap();
        session.finalize_registered_imports(&finalized).unwrap();
        let registered = session.registered_program(&key).unwrap();
        assert_eq!(registered.imports.imports.len(), 1);
        assert_eq!(
            registered.imports.imports[0].specifier,
            Arc::<str>::from("finalize.rue")
        );

        let mismatched = const_program("finalize.rue", "i64");
        assert_eq!(
            session.finalize_registered_imports(&mismatched),
            Err(DurableComptimeProgramFinalizationError::AuthorityMismatch)
        );
        assert_eq!(
            session.registered_program(&key).unwrap().imports.imports[0].specifier,
            Arc::<str>::from("finalize.rue")
        );
    }

    #[test]
    fn foreign_frame_admission_is_keyed_atomic_and_keeps_ticket_unentered() {
        let core = callable_program("foreign-frame.rue");
        let seed = crate::body_query::ForeignComptimeCallSeed {
            type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
            value_arguments: Arc::from([(
                Arc::from("x"),
                crate::durable_semantics::DurableConstValue::Integer(9),
            )]),
        };
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: core.clone(),
            seed: seed.clone(),
        };
        let mut session = session();
        let edge = session.prepare_expression_edge(7).unwrap();
        let ticket = session
            .lifecycle
            .ticket_from_admitted_edge(edge, &admitted)
            .unwrap();
        let admission = prepared_admission(&core);
        let bound_admitted = test_admitted(admission, 7);
        let (frame, _ticket) = session
            .admit_foreign_frame(
                admitted,
                Box::new(ticket),
                rue_span::Span::new(17, 23),
                bound_call(&bound_admitted, Some(9)),
            )
            .unwrap();
        assert_eq!(frame.program, core.plan.key);
        assert_eq!(frame.body, core.callable().unwrap().body);
        assert_eq!(frame.name.as_ref().unwrap().as_str(), "target");
        assert_eq!(
            frame.context.as_ref().map(DurableComptimeFile::program),
            Some(&core.plan.key)
        );
        assert_eq!(frame.span, rue_span::Span::new(17, 23));
        assert_eq!(
            frame.function_span,
            core.rir.get(core.callable().unwrap().root).span
        );
        assert!(frame.call_identity.is_none());
        assert_eq!(
            frame.type_bindings.get(&DurableComptimeName::from("T")),
            Some(&DurableComptimeType(DurableType::I32))
        );
        assert_eq!(
            frame.value_bindings.get(&DurableComptimeName::from("x")),
            Some(&EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(9),
                DurableType::I32,
            )))
        );
        assert_eq!(
            frame.expected_result,
            Some(DurableComptimeType(DurableType::I32))
        );
        assert!(session.lifecycle.active.is_empty());
        assert!(session.registered_program(&core.plan.key).is_some());

        // Binding validation happens before a cold program is inserted.
        let invalid_core = callable_program("invalid-bindings.rue");
        let invalid_admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: invalid_core.clone(),
            seed: seed.clone(),
        };
        let invalid_edge = session.prepare_expression_edge(8).unwrap();
        let invalid_ticket = session
            .lifecycle
            .ticket_from_admitted_edge(invalid_edge, &invalid_admitted)
            .unwrap();
        let invalid_admission = prepared_admission(&invalid_core);
        let invalid_bound_admitted = test_admitted(invalid_admission, 8);
        assert!(matches!(
            session.admit_foreign_frame(
                invalid_admitted,
                Box::new(invalid_ticket),
                rue_span::Span::new(24, 27),
                bound_call(&invalid_bound_admitted, Some(10)),
            ),
            Err(DurableComptimeForeignFrameAdmissionError::TicketMismatch)
        ));
        assert!(session.registered_program(&invalid_core.plan.key).is_none());

        // A separately materialized equivalent core is valid, but it cannot
        // replace the first authority in the keyed registry.
        let equivalent = callable_program("foreign-frame.rue");
        let repeat_seed = crate::body_query::ForeignComptimeCallSeed {
            type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
            value_arguments: Arc::from([(
                Arc::from("x"),
                crate::durable_semantics::DurableConstValue::Integer(10),
            )]),
        };
        let equivalent_admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: equivalent,
            seed: repeat_seed,
        };
        let edge = session.prepare_expression_edge(8).unwrap();
        let ticket = session
            .lifecycle
            .ticket_from_admitted_edge(edge, &equivalent_admitted)
            .unwrap();
        let equivalent_admission = prepared_admission(&equivalent_admitted.core);
        let equivalent_bound_admitted = test_admitted(equivalent_admission, 8);
        let (second_frame, _) = session
            .admit_foreign_frame(
                equivalent_admitted,
                Box::new(ticket),
                rue_span::Span::new(24, 27),
                bound_call(&equivalent_bound_admitted, Some(10)),
            )
            .unwrap();
        assert_eq!(second_frame.program, core.plan.key);
        assert_eq!(second_frame.body, core.callable().unwrap().body);
        assert_eq!(
            second_frame
                .context
                .as_ref()
                .map(DurableComptimeFile::program),
            Some(&core.plan.key)
        );
        assert_eq!(
            second_frame.function_span,
            core.rir.get(core.callable().unwrap().root).span
        );
        assert_eq!(second_frame.span, rue_span::Span::new(24, 27));
        assert!(second_frame.call_identity.is_none());
        assert_eq!(
            second_frame
                .value_bindings
                .get(&DurableComptimeName::from("x")),
            Some(&EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(10),
                DurableType::I32,
            )))
        );
        assert!(session.lifecycle.active.is_empty());
        let registered = session.registered_program(&core.plan.key).unwrap();
        assert!(std::sync::Arc::ptr_eq(&registered.rir, &core.rir));
    }

    #[test]
    fn foreign_frame_admission_rejects_non_callable_without_registration() {
        let core = const_program("foreign-const.rue", "i32");
        let ticket_core = callable_program("foreign-const.rue");
        let ticket_admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: ticket_core,
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            },
        };
        let admitted = crate::body_query::OwnedForeignComptimeProgram {
            core: core.clone(),
            seed: crate::body_query::ForeignComptimeCallSeed {
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            },
        };
        let mut session = session();
        let edge = session.prepare_expression_edge(0).unwrap();
        let ticket = session
            .lifecycle
            .ticket_from_admitted_edge(edge, &ticket_admitted)
            .unwrap();
        let ticket_admission = prepared_admission(&ticket_admitted.core);
        let ticket_bound_admitted = test_admitted(ticket_admission, 0);
        assert!(matches!(
            session.admit_foreign_frame(
                admitted,
                Box::new(ticket),
                rue_span::Span::new(0, 1),
                bound_call(&ticket_bound_admitted, None),
            ),
            Err(DurableComptimeForeignFrameAdmissionError::NotCallable)
        ));
        assert!(session.registered_program(&core.plan.key).is_none());
        assert!(session.lifecycle.active.is_empty());
    }

    #[test]
    fn const_root_admission_returns_keyed_ticket_free_frames_atomically() {
        assert_frame_domains::<
            EvaluatedSemanticConst,
            DurableComptimeType,
            DurableComptimeName,
            DurableComptimeFile,
            crate::body_query::DurableComptimeProgramKey,
            DurableComptimeIdentity,
        >(None);
        let first = const_program("frame-first.rue", "i32");
        let second = const_program("frame-second.rue", "i64");
        let callable = callable_program("frame-callable.rue");
        let mut session = session();
        assert_eq!(
            session.file_for_program(&callable.plan.key),
            Err(DurableComptimeDiagnosticSiteError::UnknownProgram)
        );

        let specialized_producer = crate::StableProducerId::Function(rue_air::Node::new(
            crate::FunctionInstanceKey::Specialization {
                base: rue_air::Node::new(crate::FunctionInstanceKey::Definition(
                    first.plan.key.declaration.clone(),
                )),
                arguments: Default::default(),
            },
        ));
        let durable_identity = DurableComptimeIdentity::from(specialized_producer.clone());
        assert_eq!(durable_identity.as_ref(), &specialized_producer);

        let first_frame = session.admit_const_root(first.clone(), None).unwrap();
        assert_eq!(first_frame.program, first.plan.key);
        assert_eq!(first_frame.body, first.const_root().unwrap().0);
        assert_eq!(
            first_frame
                .context
                .as_ref()
                .map(DurableComptimeFile::program),
            Some(&first.plan.key)
        );
        assert_eq!(first_frame.span, first.rir.get(first_frame.body).span);
        assert_eq!(
            first_frame.function_span,
            first.rir.get(first.const_root().unwrap().2).span
        );
        assert!(first_frame.name.is_none());
        assert!(first_frame.call_identity.is_none());
        assert!(first_frame.type_bindings.is_empty());
        assert!(first_frame.value_bindings.is_empty());
        assert!(first_frame.name_bindings.is_empty());
        assert!(first_frame.expected_result.is_none());

        let second_frame = session
            .admit_const_root(second.clone(), Some(DurableComptimeType(DurableType::I64)))
            .unwrap();
        assert_eq!(second_frame.program, second.plan.key);
        assert_eq!(second_frame.body, second.const_root().unwrap().0);
        assert_eq!(
            second_frame.expected_result,
            Some(DurableComptimeType(DurableType::I64))
        );
        assert_eq!(
            first_frame.body, second_frame.body,
            "dense refs intentionally collide"
        );
        assert_ne!(first_frame.program, second_frame.program);
        assert_eq!(
            first_frame
                .context
                .as_ref()
                .map(DurableComptimeFile::program),
            Some(&first.plan.key)
        );
        assert_eq!(
            second_frame
                .context
                .as_ref()
                .map(DurableComptimeFile::program),
            Some(&second.plan.key)
        );
        assert_ne!(first_frame.context, second_frame.context);
        assert_eq!(session.programs.len(), 2);
        assert_ne!(
            session.programs.get(&first_frame.program).unwrap().symbols,
            session.programs.get(&second_frame.program).unwrap().symbols,
            "colliding refs retain distinct owning symbol authorities"
        );

        assert!(matches!(
            session.admit_const_root(first, None),
            Err(DurableComptimeConstRootAdmissionError::DuplicateProgram)
        ));
        assert!(matches!(
            session.admit_const_root(callable, None),
            Err(DurableComptimeConstRootAdmissionError::NotConstRoot)
        ));
        assert_eq!(session.programs.len(), 2, "rejected admissions are atomic");
    }
}

#[cfg(test)]
mod terminal_adapter_tests {
    use super::*;
    use rue_air::ComptimeIntegerOperation;

    fn site(name: &str, start: u32, end: u32) -> DurableComptimeDiagnosticSite {
        DurableComptimeDiagnosticSite::new(
            DeclarationCandidateKey {
                module: ModuleId::from_validated_canonical("terminal-tests"),
                category:
                    crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
                name: Arc::from(name),
                owner: None,
                duplicate_discriminator: 0,
            },
            start,
            end,
        )
    }

    #[test]
    fn durable_failure_preserves_query_control_and_domain_channels() {
        let aborts = [
            QueryAbort::Canceled,
            QueryAbort::Cycle(Arc::from([])),
            QueryAbort::ForeignRuntime,
            QueryAbort::UnpublishedRevision(rue_query::Revision::new(7, 8)),
            QueryAbort::MissingInput(rue_query::InputIdentity::new("terminal", "missing")),
        ];
        for abort in aborts {
            assert_eq!(
                DurableComptimeFailure::abort(abort.clone()),
                DurableComptimeFailure::Abort(abort),
            );
        }

        let failure = DurableComptimeFailure::resolution(Arc::<str>::from("not ready"));
        assert!(matches!(
            failure,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message) if message.as_ref() == "not ready")
        ));

        assert!(matches!(
            DurableComptimeFailure::provider_error_as_host(rue_air::SemanticProviderError::Abort(
                QueryAbort::Canceled
            )),
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryAbort(QueryAbort::Canceled)
            ))
        ));
        assert!(matches!(
            DurableComptimeFailure::provider_error_as_host(
                rue_air::SemanticProviderError::Failure(SemanticNucleusFailure::Resolution(
                    Arc::from("host failure")
                ))
            ),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            ))
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message) if message.as_ref() == "host failure")
        ));

        assert!(matches!(
            DurableComptimeFailure::provider_error_as_host(rue_air::SemanticProviderError::Abort(
                QueryAbort::Canceled
            )),
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryAbort(QueryAbort::Canceled)
            ))
        ));
        assert!(matches!(
            DurableComptimeFailure::provider_error_as_host(
                rue_air::SemanticProviderError::Failure(SemanticNucleusFailure::Resolution(
                    Arc::from("provider failure")
                ))
            ),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            ))
                if matches!(value.as_ref(), SemanticNucleusFailure::Resolution(actual) if actual.as_ref() == "provider failure")
        ));
        assert!(matches!(
            DurableComptimeFailure::abort(QueryAbort::Canceled).into_host_error(),
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryAbort(QueryAbort::Canceled)
            ))
        ));
    }

    #[test]
    fn named_diagnostic_constructors_preserve_exact_legacy_text() {
        let cases = [
            (
                DurableComptimeFailure::maximum_depth(
                    "count",
                    rue_air::specialize::MAX_COMPTIME_CALL_DEPTH,
                ),
                format!(
                    "specialization of 'count' exceeded the maximum nesting depth ({}); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?",
                    rue_air::specialize::MAX_COMPTIME_CALL_DEPTH,
                ),
            ),
            (
                DurableComptimeFailure::integer_literal_overflow("i8", 128),
                "integer overflow evaluating constant at type i8: value 128 is out of range for type i8; 128 does not fit in i8 (this operation would panic at runtime)".to_owned(),
            ),
            (
                DurableComptimeFailure::arithmetic_overflow(
                    "i8",
                    "addition",
                    "value 128 is out of range for type i8; 128 does not fit in i8",
                ),
                "integer overflow evaluating addition at type i8: value 128 is out of range for type i8; 128 does not fit in i8 (this operation would panic at runtime)".to_owned(),
            ),
        ];
        for (failure, expected) in cases {
            let DurableComptimeFailure::Failure(value) = failure else {
                panic!("durable diagnostic must remain a domain failure");
            };
            let SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::ComptimeEvaluationFailed { reason: actual },
            ) = *value
            else {
                panic!("durable diagnostic has the wrong error channel");
            };
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn unmatched_comptime_match_uses_the_canonical_legacy_failure() {
        assert!(matches!(
            DurableComptimeFailure::comptime_match_no_selected_arm(),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message)
                    if message.as_ref() == "comptime match has no selected arm")
        ));
    }

    #[test]
    fn projection_failures_preserve_typed_host_channels_and_legacy_text() {
        use crate::body_query::ComptimeProgramProjectionFailure as Projection;
        use crate::canonical_lower::BodyPlanMaterializationFailure as Materialization;
        use crate::revisioned_query_database::DeclarationBodyPlanFailure as Artifact;

        let producer = site("projection", 1, 2).producer;
        assert!(matches!(
            durable_projection_failure_error(Projection::Materialization(Materialization::Query(
                QueryAbort::Canceled
            ))),
            rue_air::ComptimeHostError::Abort(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryAbort(QueryAbort::Canceled)
            ))
        ));

        let build = rue_rir::RirPayloadBuildError::ResourceLimitExceeded { family: "test" };
        assert!(matches!(
            durable_projection_failure_error(Projection::Materialization(
                Materialization::Build(build)
            )),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::CompilerResourceLimit(_)
            ))
        ));
        assert!(matches!(
            durable_projection_failure_error(Projection::Materialization(
                Materialization::Invalid(Arc::from("invalid materialization"))
            )),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                if detail.as_ref() == "invalid materialization")
        ));

        let artifact_build = rue_error::ErrorKind::InvalidInteger;
        assert!(matches!(
            durable_projection_failure_error(Projection::Artifact(Artifact::Build(
                artifact_build.clone()
            ))),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::InvalidInteger
            ))
        ));
        let mut errors = crate::CompileErrors::new();
        errors.push(crate::CompileError::without_span(
            rue_error::ErrorKind::InvalidInteger,
        ));
        assert!(matches!(
            durable_projection_failure_error(Projection::Artifact(
                Artifact::CandidateRirRejected(errors)
            )),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Syntax(_))
        ));
        let unavailable = Artifact::CandidateUnavailable(producer.clone());
        assert!(matches!(
            durable_projection_failure_error(Projection::Artifact(unavailable.clone())),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                if detail.as_ref() == format!("candidate RIR artifact failed: {unavailable:?}"))
        ));

        let query_failure = rue_query::QueryFailure::new("test-query", "payload");
        assert!(matches!(
            durable_projection_failure_error(Projection::ArtifactQueryFailure(
                query_failure.clone()
            )),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::QueryFailure(actual)
            )) if actual == query_failure
        ));

        for (failure, expected) in [
            (
                Projection::NotFunction {
                    root: rue_rir::InstRef::from_raw(0),
                },
                "comptime candidate artifact has a non-function root",
            ),
            (
                Projection::NotConst {
                    root: rue_rir::InstRef::from_raw(0),
                },
                "constant candidate artifact has a non-constant root",
            ),
        ] {
            assert!(matches!(
                durable_projection_failure_error(failure),
                rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                    DurableComptimeHostFailureKind::Semantic(value)
                )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                    if detail.as_ref() == expected)
            ));
        }

        let invalid_stable_key = crate::StableDefinitionKey::from_stable_parts(
            producer.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
            producer.name.clone(),
            None,
        );
        let invalid_producer = Projection::InvalidProducer(invalid_stable_key);
        let invalid_debug = format!("{invalid_producer:?}");
        assert!(matches!(
            durable_projection_failure_error(invalid_producer),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                if detail.as_ref() == invalid_debug)
        ));
        let identity_mismatch = Projection::IdentityMismatch;
        let identity_debug = format!("{identity_mismatch:?}");
        assert!(matches!(
            durable_projection_failure_error(identity_mismatch),
            rue_air::ComptimeHostError::HostFailure(DurableComptimeHostFailure(
                DurableComptimeHostFailureKind::Semantic(value)
            )) if matches!(*value, SemanticNucleusFailure::Resolution(ref detail)
                if detail.as_ref() == identity_debug)
        ));
    }

    #[test]
    fn semantic_rejection_kernel_preserves_each_legacy_channel_and_text() {
        let unit = EvaluatedSemanticConst::unit();
        let cases = [
            (
                ComptimeSemanticRejection::ConditionNotBoolean(unit.clone()),
                "comptime condition is not boolean",
            ),
            (
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation: ComptimeIntegerOperation::Add,
                    lhs: unit.clone(),
                    rhs: Some(unit.clone()),
                },
                "comptime arithmetic operand is not an integer",
            ),
            (
                ComptimeSemanticRejection::UnaryOperandNotInteger(unit.clone()),
                "comptime arithmetic operand is not an integer",
            ),
            (
                ComptimeSemanticRejection::UnaryTypeNotInteger {
                    operation: ComptimeUnaryOperation::BitNot,
                    value: unit,
                },
                "comptime bitwise NOT operand is not an integer",
            ),
        ];
        for (rejection, expected) in cases {
            let DurableComptimeFailure::Failure(value) =
                DurableComptimeFailure::comptime_rejection(rejection)
            else {
                panic!("semantic rejection must remain a durable failure");
            };
            let SemanticNucleusFailure::Resolution(reason) = *value else {
                panic!("semantic rejection changed its failure channel");
            };
            assert_eq!(reason.as_ref(), expected);
        }
        for rejection in [
            ComptimeSemanticRejection::EmptyBlock,
            ComptimeSemanticRejection::UnsupportedExpression,
        ] {
            assert!(matches!(
                DurableComptimeFailure::comptime_rejection(rejection),
                DurableComptimeFailure::Failure(value)
                    if matches!(*value, SemanticNucleusFailure::Resolution(_))
            ));
        }
        let module = EvaluatedSemanticConst::Module(ModuleId::from_validated_canonical("m"));
        let target = EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name: "Arch",
            variant: "X86_64",
        });
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::ConditionNotBoolean(module)
            ),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message)
                    if message.as_ref() == "module used where a value is required")
        ));
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation: ComptimeIntegerOperation::Add,
                    lhs: target,
                    rhs: None,
                }
            ),
                    DurableComptimeFailure::Failure(value)
                        if matches!(*value, SemanticNucleusFailure::Resolution(ref message)
                    if message.as_ref() == "target descriptor used where a durable const value is required")
        ));
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::AggregateExpression
            ),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ConstExprNotSupported { ref expr_kind }
                ) if expr_kind == "aggregate expression")
        ));
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::UnsupportedIntrinsic("size_of".to_owned())
            ),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ConstExprNotSupported { ref expr_kind }
                ) if expr_kind == "intrinsic `@size_of`")
        ));
        assert!(matches!(
            DurableComptimeFailure::comptime_rejection(ComptimeSemanticRejection::Assignment),
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message)
                    if message.as_ref()
                        == "assignment is not supported in declaration-time comptime")
        ));
    }

    #[test]
    fn semantic_rejection_kernel_preserves_noncommutative_operand_decisions() {
        let module = || EvaluatedSemanticConst::Module(ModuleId::from_validated_canonical("m"));
        let target = || {
            EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
                type_name: "Arch",
                variant: "X86_64",
            })
        };
        let boolean = || {
            EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Bool(true),
                DurableType::Bool,
            ))
        };
        let integer = || {
            EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(1),
                DurableType::I32,
            ))
        };
        let reason = |lhs, rhs| {
            let DurableComptimeFailure::Failure(value) = DurableComptimeFailure::comptime_rejection(
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation: ComptimeIntegerOperation::Lt,
                    lhs,
                    rhs,
                },
            ) else {
                panic!("expected durable failure");
            };
            let SemanticNucleusFailure::Resolution(reason) = *value else {
                panic!("expected resolution failure");
            };
            reason.to_string()
        };
        assert_eq!(
            reason(target(), Some(module())),
            "target descriptor comparison requires matching enum variants"
        );
        assert_eq!(
            reason(module(), Some(target())),
            "target descriptor comparison requires matching enum variants"
        );
        assert_eq!(
            reason(target(), Some(target())),
            "target descriptors support only equality comparisons"
        );
        assert_eq!(
            reason(boolean(), Some(boolean())),
            "boolean values support only equality comparisons"
        );
        assert_eq!(
            reason(boolean(), Some(integer())),
            "comptime arithmetic operand is not an integer"
        );
        assert_eq!(
            reason(integer(), Some(boolean())),
            "comptime arithmetic operand is not an integer"
        );
        assert_eq!(
            reason(target(), None),
            "target descriptor used where a durable const value is required"
        );
        assert_eq!(
            reason(module(), None),
            "module used where a value is required"
        );
    }

    #[test]
    fn stable_trap_site_uses_trap_range_and_distinguishes_colliding_owners() {
        let root = site("root", 10, 20);
        let nested = site("nested", 100, 120);
        let trap = |operation| rue_air::ComptimeTrap {
            operation,
            span: rue_span::Span::new(999, 1000),
        };
        let root_failure =
            DurableComptimeFailure::trap_at(root.producer.clone(), trap("division by zero"))
                .expect("division trap is supported");
        let nested_failure =
            DurableComptimeFailure::trap_at(nested.producer.clone(), trap("division by zero"))
                .expect("division trap is supported");
        assert!(matches!(
            root_failure,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::DiagnosticAtProducerRange {
                    ref producer, start: 999, end: 1000, ..
                } if producer == &root.producer)
        ));
        assert!(matches!(
            nested_failure,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::DiagnosticAtProducerRange {
                    ref producer, start: 999, end: 1000, ..
                } if producer == &nested.producer)
        ));
        let remainder =
            DurableComptimeFailure::trap_at(nested.producer.clone(), trap("remainder by zero"))
                .expect("remainder trap is supported");
        assert!(matches!(
            remainder,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::DiagnosticAtProducerRange {
                    ref producer,
                    kind: rue_error::ErrorKind::ComptimeEvaluationFailed { ref reason },
                    start: 999, end: 1000, ..
                } if producer == &nested.producer && reason == "remainder by zero (this operation would panic at runtime)"
            )
        ));
        assert!(DurableComptimeFailure::trap_at(nested.producer.clone(), trap("other")).is_none());
        assert_eq!((root.start, root.end), (10, 20));
        assert_eq!((nested.start, nested.end), (100, 120));
    }

    #[test]
    fn stable_site_constructor_keeps_explicit_non_trap_diagnostics() {
        let nested = site("nested", 100, 120);
        let failure = DurableComptimeFailure::comptime_failure_at(&nested, "foreign failure");
        assert!(matches!(
            failure,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::DiagnosticAtProducerRange {
                    ref producer, start: 100, end: 120, ..
                } if producer == &nested.producer)
        ));
    }

    #[test]
    fn scalar_policy_preserves_integer_precedence_and_fallbacks() {
        use crate::durable_semantics::DurableType as T;

        assert_eq!(DurableComptimeScalarPolicy::type_name(&T::U16), "u16");
        for ty in [T::U8, T::U16, T::U32, T::U64] {
            assert!(DurableComptimeScalarPolicy::type_is_unsigned(&ty));
        }
        for ty in [T::I8, T::I16, T::I32, T::I64, T::Bool] {
            assert!(!DurableComptimeScalarPolicy::type_is_unsigned(&ty));
        }
        assert_eq!(
            DurableComptimeScalarPolicy::type_integer_semantics(&T::I32)
                .expect("i32 has integer semantics")
                .bits(),
            32
        );

        // A reduced operand type wins over the frame expected type, matching
        // the established integer_type(left, right) precedence.
        assert_eq!(
            DurableComptimeScalarPolicy::integer_operation_type(Some(&T::U16), Some(&T::I8), None,)
                .unwrap(),
            T::I8
        );
        assert_eq!(
            DurableComptimeScalarPolicy::unary_integer_type(Some(&T::U16), Some(&T::I8)).unwrap(),
            T::I8
        );
        assert_eq!(
            DurableComptimeScalarPolicy::integer_operation_type(Some(&T::U8), None, None).unwrap(),
            T::U8
        );
        assert_eq!(
            DurableComptimeScalarPolicy::unary_integer_type(None, None).unwrap(),
            T::I32
        );
        assert!(matches!(
            DurableComptimeScalarPolicy::integer_operation_type(
                None,
                Some(&T::I8),
                Some(&T::U8),
            ),
            Err(DurableComptimeFailure::Failure(value))
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::TypeMismatch { .. }
                ))
        ));
    }

    #[test]
    fn scalar_policy_preserves_fit_and_arithmetic_diagnostics() {
        use crate::durable_semantics::DurableType as T;

        DurableComptimeScalarPolicy::require_integer_fits(&T::U8, 255).unwrap();
        assert!(matches!(
            DurableComptimeScalarPolicy::require_integer_fits(&T::U8, 256),
            Err(DurableComptimeFailure::Failure(value))
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ComptimeEvaluationFailed { ref reason }
                ) if reason.contains("does not fit in u8"))
        ));
        let integer = rue_air::integer_semantics::IntegerType::new(8, true).unwrap();
        assert_eq!(
            DurableComptimeScalarPolicy::checked_integer_result(
                &T::I8,
                integer.checked_add_report_i128(1, 2),
                "addition",
            )
            .unwrap(),
            3
        );
        assert!(matches!(
            DurableComptimeScalarPolicy::checked_integer_result(
                &T::I8,
                integer.checked_add_report_i128(127, 1),
                "addition",
            ),
            Err(DurableComptimeFailure::Failure(value))
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ComptimeEvaluationFailed { ref reason }
                ) if reason.contains("integer overflow evaluating addition"))
        ));
        assert!(matches!(
            DurableComptimeScalarPolicy::checked_integer_result(
                &T::I8,
                integer.checked_neg_literal_report_i128(129),
                "negation",
            ),
            Err(DurableComptimeFailure::Failure(value))
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ComptimeEvaluationFailed { ref reason }
                ) if reason.contains("integer overflow evaluating negation"))
        ));
    }

    #[test]
    fn type_intrinsic_policy_preserves_all_bounds_gates_and_mismatch() {
        use crate::durable_semantics::DurableType as T;
        assert_eq!(
            rue_air::ComptimeTypeIntrinsic::from_name("require_droppable"),
            Some(rue_air::ComptimeTypeIntrinsic::RequireDroppable)
        );
        assert_eq!(
            rue_air::ComptimeTypeIntrinsic::from_name("require_trivially_droppable"),
            Some(rue_air::ComptimeTypeIntrinsic::RequireTriviallyDroppable)
        );
        assert_eq!(rue_air::ComptimeTypeIntrinsic::from_name("size_of"), None);

        for (ty, min, max) in [
            (T::I8, -128, 127),
            (T::I16, -32_768, 32_767),
            (T::I32, i32::MIN as i128, i32::MAX as i128),
            (T::I64, i64::MIN as i128, i64::MAX as i128),
            (T::U8, 0, 255),
            (T::U16, 0, 65_535),
            (T::U32, 0, u32::MAX as i128),
            (T::U64, 0, u64::MAX as i128),
        ] {
            assert_eq!(
                DurableComptimeTypeIntrinsicPolicy::integer_bound(
                    rue_air::ComptimeIntegerBound::Min,
                    &ty,
                )
                .unwrap(),
                min
            );
            assert_eq!(
                DurableComptimeTypeIntrinsicPolicy::integer_bound(
                    rue_air::ComptimeIntegerBound::Max,
                    &ty,
                )
                .unwrap(),
                max
            );
        }

        let Err(DurableComptimeFailure::Failure(failure)) =
            DurableComptimeTypeIntrinsicPolicy::integer_bound(
                rue_air::ComptimeIntegerBound::Min,
                &T::Bool,
            )
        else {
            panic!("non-integer bound must be a semantic failure");
        };
        assert!(matches!(
            *failure,
            SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::IntrinsicTypeMismatch(ref mismatch)
            ) if mismatch.name == "int_min"
                && mismatch.expected == "an integer type"
                && mismatch.found == "bool"
        ));
    }

    #[test]
    fn diagnostic_sites_are_keyed_and_reject_unknown_programs() {
        let first =
            super::structured_type_adapter_tests::const_program("diagnostic-first.rue", "i32");
        let second =
            super::structured_type_adapter_tests::const_program("diagnostic-second.rue", "i64");
        let mut session = super::structured_type_adapter_tests::session();
        session.register_program(&first).unwrap();
        session.register_program(&second).unwrap();

        let span = rue_span::Span::with_file(rue_span::FileId::DEFAULT, 11, 19);
        let first_site = session.diagnostic_site(&first.plan.key, span).unwrap();
        let second_site = session.diagnostic_site(&second.plan.key, span).unwrap();
        assert_eq!((first_site.start, first_site.end), (11, 19));
        assert_eq!((second_site.start, second_site.end), (11, 19));
        assert_ne!(first_site.producer, second_site.producer);

        let unknown =
            super::structured_type_adapter_tests::callable_program("diagnostic-unknown.rue");
        assert_eq!(
            session.diagnostic_site(&unknown.plan.key, span),
            Err(DurableComptimeDiagnosticSiteError::UnknownProgram)
        );
    }
}
