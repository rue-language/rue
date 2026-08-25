//! Durable compile-time values shared by the legacy evaluator and the AIR
//! adapter boundary.
//!
//! The value cases in this module are deliberately the existing durable
//! evaluator representation.  The AIR implementations below are only an
//! adapter over that representation; they do not define a second value
//! algebra or perform evaluation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rue_air::{ComptimeType, ComptimeValue};
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
    QueryAbort(QueryAbort),
}

impl DurableComptimeHostFailure {
    fn semantic(failure: Box<SemanticNucleusFailure>) -> Self {
        Self(DurableComptimeHostFailureKind::Semantic(failure))
    }

    fn query_abort(abort: QueryAbort) -> Self {
        Self(DurableComptimeHostFailureKind::QueryAbort(abort))
    }

    fn into_host_error(self) -> rue_air::ComptimeHostError<Self> {
        match &self.0 {
            DurableComptimeHostFailureKind::Semantic(_) => {
                rue_air::ComptimeHostError::HostFailure(self)
            }
            DurableComptimeHostFailureKind::QueryAbort(_) => {
                rue_air::ComptimeHostError::Abort(self)
            }
        }
    }

    fn into_legacy_failure(self) -> DurableComptimeFailure {
        match self.0 {
            DurableComptimeHostFailureKind::Semantic(failure) => {
                DurableComptimeFailure::Failure(failure)
            }
            DurableComptimeHostFailureKind::QueryAbort(_) => {
                unreachable!("canonical host failure carried a query abort")
            }
        }
    }

    fn into_legacy_abort(self) -> DurableComptimeFailure {
        match self.0 {
            DurableComptimeHostFailureKind::QueryAbort(abort) => {
                DurableComptimeFailure::Abort(abort)
            }
            DurableComptimeHostFailureKind::Semantic(_) => {
                unreachable!("canonical host abort carried a semantic failure")
            }
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

    pub(crate) fn division_by_zero() -> Self {
        Self::arithmetic_trap_failure("division by zero")
    }

    pub(crate) fn remainder_by_zero() -> Self {
        Self::arithmetic_trap_failure("remainder by zero")
    }

    fn arithmetic_trap_failure(operation: &str) -> Self {
        Self::comptime_failure(
            arithmetic_trap_reason(operation).expect("unsupported durable arithmetic trap"),
        )
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
    #[allow(dead_code)] // consumed when the durable AIR host is wired
    pub(crate) fn trap_at(
        producer: DeclarationCandidateKey,
        trap: rue_air::ComptimeTrap,
    ) -> Option<Self> {
        let site = DurableComptimeDiagnosticSite::new(producer, trap.span.start, trap.span.end);
        let reason = arithmetic_trap_reason(trap.operation)?;
        Some(Self::diagnostic_at_site(&site, reason.to_owned()))
    }

    /// Adapt a durable terminal directly for the future AIR host.  Provider
    /// errors use `provider_error_as_host` instead, so this seam never creates
    /// a durable failure only to convert it back into a provider error.
    #[allow(dead_code)] // consumed when the durable AIR host is wired
    pub(crate) fn into_host_error(self) -> rue_air::ComptimeHostError<DurableComptimeHostFailure> {
        match self {
            Self::Failure(failure) => {
                DurableComptimeHostFailure::semantic(failure).into_host_error()
            }
            Self::Abort(abort) => DurableComptimeHostFailure::query_abort(abort).into_host_error(),
        }
    }

    /// Build the AIR error directly from a provider result.  This is the
    /// future host funnel; the legacy adapter below only unwraps its matching
    /// outer channel and never normalizes crossed tags.
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

    pub(crate) fn provider_error(
        error: rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    ) -> Self {
        match Self::provider_error_as_host(error) {
            rue_air::ComptimeHostError::Abort(payload) => payload.into_legacy_abort(),
            rue_air::ComptimeHostError::HostFailure(payload) => payload.into_legacy_failure(),
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
/// boundary for the future AIR host.
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

    #[allow(dead_code)] // used by the root-integrated AIR host in the next slice
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
#[allow(dead_code)] // carried by the root-integrated AIR host in the next slice
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeCallContext {
    query: crate::semantic_query_nucleus::ComptimeCallQueryKey,
    parent_producer: crate::StableDefinitionKey,
    parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    child_producer: crate::StableDefinitionKey,
    program: crate::body_query::ForeignComptimeProgramKey,
    application_policy: DurableComptimeApplicationPolicy,
}

impl DurableComptimeCallContext {
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
        let child_producer = admitted.plan.key.producer.clone();
        let Some(child_declaration) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(&child_producer)
        else {
            return Err(DurableComptimeLifecycleError::InvalidContext);
        };
        if child_declaration != admitted.plan.candidate
            || admitted.callable.context != child_declaration.module
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
            program: crate::body_query::ForeignComptimeProgramKey {
                producer: child_producer,
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
            program: crate::body_query::ForeignComptimeProgramKey {
                producer: child_producer,
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

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableTicketState {
    Entered,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeLifecycleError {
    TicketMismatch,
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
/// and expected-result context.  This session owns only compiler-side call
/// lifecycle state and the root-local call ordinal allocator that the future
/// durable host will use when it issues lifecycle edges.
#[derive(Debug)]
pub(crate) struct DurableComptimeSession {
    lifecycle: DurableComptimeCallLifecycle,
    next_call: u32,
}

impl DurableComptimeSession {
    pub(crate) fn new(
        parent_producer: crate::StableDefinitionKey,
        parent_declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<Self, DurableComptimeLifecycleError> {
        Ok(Self {
            lifecycle: DurableComptimeCallLifecycle::new(parent_producer, parent_declaration)?,
            next_call: 0,
        })
    }

    pub(crate) fn next_call_ordinal(&mut self) -> u32 {
        let ordinal = self.next_call;
        self.next_call += 1;
        ordinal
    }

    #[allow(dead_code)] // activated when the durable AIR host enters call edges
    pub(crate) fn lifecycle_mut(&mut self) -> &mut DurableComptimeCallLifecycle {
        &mut self.lifecycle
    }
}

/// Root-local call/effect authority for a durable comptime host.
///
/// `finish` consumes an entered ticket and its lifecycle-owned scope. Cleanup
/// happens for every AIR terminal, while effects publish only for a known
/// result, without copying AIR's outcome algebra into the compiler.
#[allow(dead_code)] // AIR owns the active root lifecycle until compiler cutover
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

/// The identity and dependency facts established before signature admission.
///
/// Callers observe `dependency` immediately after this phase succeeds, before
/// any signature, shell, arity, or mode work can fail.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DurableComptimeCallableAdmissionStart {
    pub(crate) candidate: DeclarationCandidateKey,
    pub(crate) identity: crate::semantic_query_nucleus::DeclarationIdentityProjection,
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
    pub(crate) parameters: Arc<[crate::durable_semantics::DurableSemanticParameter]>,
    pub(crate) result: DurableType,
    pub(crate) shell_parameters: Arc<[crate::declaration_candidate::DeclarationParameterHeader]>,
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

pub(crate) fn durable_type_diagnostic_name(ty: &DurableType) -> String {
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

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DurableComptimeBinding {
    type_arguments: Vec<(Arc<str>, DurableType)>,
    value_arguments: Vec<(Arc<str>, DurableConstValue)>,
}

impl DurableComptimeBinding {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<(Arc<str>, DurableType)>,
        Vec<(Arc<str>, DurableConstValue)>,
    ) {
        (self.type_arguments, self.value_arguments)
    }
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
    binding
        .value_arguments
        .push((Arc::from(parameter_name), value));
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
}

/// The only declaration kinds considered by durable named-value lookup, in
/// the same order as the legacy evaluator's semantic lookup policy.
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
        Self { value, dependency }
    }

    pub(crate) fn into_parts(self) -> (EvaluatedSemanticConst, SemanticDeclarationDependency) {
        (self.value, self.dependency)
    }
}

/// Canonical semantic services needed by durable comptime entry points.
///
/// Implementations live beside the query authorities. This facade is an
/// operation boundary, not an evaluator: neither trait accepts an instruction
/// reference, instruction data, or callback capable of evaluating a child.
pub(crate) trait DurableComptimeSemanticAuthority {
    fn check_canceled(&self) -> Result<(), QueryAbort>;

    fn begin_comptime_call_admission(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
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

    /// Resolve a target intrinsic from semantic name/arity facts.  The
    /// authority owns the configured target and the diagnostic policy; no RIR
    /// instruction or argument callback crosses this boundary.
    fn resolve_target_intrinsic(
        &self,
        name: &str,
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

pub(crate) trait DurableComptimeForeignCallAuthority {
    fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort>;
}

pub(crate) struct DurableComptimeServices<'a, A: ?Sized> {
    authority: &'a A,
}

impl<'a, A: ?Sized> DurableComptimeServices<'a, A> {
    pub(crate) fn new(authority: &'a A) -> Self {
        Self { authority }
    }
}

impl<A: DurableComptimeSemanticAuthority + ?Sized> DurableComptimeServices<'_, A> {
    pub(crate) fn check_canceled(&self) -> Result<(), QueryAbort> {
        self.authority.check_canceled()
    }

    pub(crate) fn begin_comptime_call_admission(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .begin_comptime_call_admission(accessing_source, module, name)
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

    pub(crate) fn resolve_named_value(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<DurableComptimeNamedValueProjection>,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .resolve_named_value(accessing_source, module, name)
    }

    pub(crate) fn resolve_module_member(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        member: &str,
    ) -> Result<
        DurableComptimeNamedValueProjection,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .resolve_module_member(accessing_source, module, member)
    }

    pub(crate) fn resolve_import(
        &self,
        site: &DurableImportSite,
    ) -> Result<DurableImportResolution, QueryAbort> {
        self.authority.resolve_import(site)
    }

    pub(crate) fn resolve_target_intrinsic(
        &self,
        name: &str,
        argument_count: usize,
    ) -> Result<TargetEnumValue, rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>>
    {
        self.authority
            .resolve_target_intrinsic(name, argument_count)
    }

    pub(crate) fn resolve_target_enum_variant(
        &self,
        type_name: &str,
        variant: &str,
    ) -> Result<TargetEnumValue, rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>>
    {
        self.authority
            .resolve_target_enum_variant(type_name, variant)
    }
}

impl<A: DurableComptimeForeignCallAuthority + ?Sized> DurableComptimeServices<'_, A> {
    /// Probe only an already-published foreign fact or admit its owned body
    /// frame. The authority owns dependency observation and cancellation; this
    /// method never demands a child comptime query.
    pub(crate) fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort> {
        self.authority
            .probe_comptime_call(producer, type_arguments, value_arguments)
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

/// The canonical pure target-descriptor kernel used by durable semantic
/// authorities.  It receives only decomposed target facts, so tests and
/// future query adapters can cover data models not currently exposed by a
/// concrete compiler target without copying the mapping policy.
pub(crate) fn resolve_target_intrinsic_facts(
    name: &str,
    argument_count: usize,
    arch: rue_target::Arch,
    os: rue_target::Os,
    data_model: rue_target::DataModel,
) -> Result<TargetEnumValue, SemanticNucleusFailure> {
    if argument_count != 0 {
        return Err(SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::IntrinsicWrongArgCount {
                name: name.to_owned(),
                expected: 0,
                found: argument_count,
            },
        ));
    }
    let (type_name, variant) = match name {
        "target_arch" => (
            "Arch",
            match arch {
                rue_target::Arch::X86_64 => "X86_64",
                rue_target::Arch::Aarch64 => "Aarch64",
            },
        ),
        "target_os" => (
            "Os",
            match os {
                rue_target::Os::Linux => "Linux",
                rue_target::Os::Macos => "Macos",
            },
        ),
        "target_data_model" => (
            "DataModel",
            match data_model {
                rue_target::DataModel::Ilp32 => "Ilp32",
                rue_target::DataModel::Lp64 => "Lp64",
                rue_target::DataModel::Llp64 => "Llp64",
            },
        ),
        _ => {
            return Err(SemanticNucleusFailure::Resolution(Arc::from(
                "unknown target descriptor intrinsic",
            )));
        }
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

    fn parameter(name: &str, ty: DurableType) -> DurableSemanticParameter {
        DurableSemanticParameter {
            name: Arc::from(name),
            ty,
            mode: DurableParameterMode::Value,
            is_comptime: true,
        }
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
    fn incremental_binding_preserves_type_then_value_order_and_substitution() {
        let mut binding = DurableComptimeBinding::default();
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
        let (types, values) = binding.into_parts();
        assert_eq!(types, vec![(Arc::from("T"), DurableType::I16)]);
        assert_eq!(
            values,
            vec![(Arc::from("value"), DurableConstValue::Integer(12))]
        );
    }

    #[test]
    fn incremental_binding_preserves_early_type_and_range_failures() {
        let mut mismatch = DurableComptimeBinding::default();
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

        let mut range = DurableComptimeBinding::default();
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
        let mut direct = DurableComptimeBinding::default();
        bind_durable_comptime_argument(
            &mut direct,
            "T",
            &parameter("T", DurableType::ComptimeType),
            typed(DurableConstValue::Unit, DurableType::Unit),
            true,
        )
        .unwrap();
        assert_eq!(
            direct.into_parts().0,
            vec![(Arc::from("T"), DurableType::Unit)]
        );

        let mut computed = DurableComptimeBinding::default();
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
                        resolve_target_intrinsic_facts("target_arch", 0, arch, os, data_model,)
                            .unwrap()
                            .variant,
                        match arch {
                            rue_target::Arch::X86_64 => "X86_64",
                            rue_target::Arch::Aarch64 => "Aarch64",
                        }
                    );
                    assert_eq!(
                        resolve_target_intrinsic_facts("target_os", 0, arch, os, data_model,)
                            .unwrap()
                            .variant,
                        match os {
                            rue_target::Os::Linux => "Linux",
                            rue_target::Os::Macos => "Macos",
                        }
                    );
                    assert_eq!(
                        resolve_target_intrinsic_facts(
                            "target_data_model",
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
                "target_os",
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
            resolve_target_intrinsic_facts(
                "other",
                0,
                rue_target::Arch::X86_64,
                rue_target::Os::Linux,
                rue_target::DataModel::Lp64,
            ),
            Err(SemanticNucleusFailure::Resolution(message))
                if message.as_ref() == "unknown target descriptor intrinsic"
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
        assert_eq!(session.next_call_ordinal(), 0);
        assert_eq!(session.next_call_ordinal(), 1);

        let mut ticket = session.lifecycle_mut().prepare(context(0)).unwrap();
        session.lifecycle_mut().enter(&ticket).unwrap();
        session
            .lifecycle_mut()
            .finish(&mut ticket, &rue_air::ComptimeOutcome::<(), ()>::Known(()))
            .unwrap();

        let mut sibling = DurableComptimeSession::new(parent, parent_declaration).unwrap();
        assert_eq!(sibling.next_call_ordinal(), 0);
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
            crate::body_query::ForeignComptimeProgramPlan {
                key: crate::body_query::ForeignComptimeProgramKey {
                    producer: producer.clone(),
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
        inconsistent.plan.candidate = sibling;
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
mod terminal_adapter_tests {
    use super::*;

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

        let provider =
            DurableComptimeFailure::provider_error(rue_air::SemanticProviderError::Failure(
                SemanticNucleusFailure::Resolution(Arc::from("provider failure")),
            ));
        assert!(matches!(
            provider,
            DurableComptimeFailure::Failure(value)
                if matches!(*value, SemanticNucleusFailure::Resolution(ref message) if message.as_ref() == "provider failure")
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
            (
                DurableComptimeFailure::division_by_zero(),
                "division by zero (this operation would panic at runtime)".to_owned(),
            ),
            (
                DurableComptimeFailure::remainder_by_zero(),
                "remainder by zero (this operation would panic at runtime)".to_owned(),
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
}
