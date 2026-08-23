//! Stable per-body query values and independently stamped projections.

use std::{
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
    sync::{Arc, OnceLock},
};

use rue_query::QueryKey;

use crate::retained_charge::RetainedCharge;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyDiagnosticOffset {
    Declaration(u32),
    Body(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyDiagnosticCoordinate {
    Relative {
        start: BodyDiagnosticOffset,
        end: BodyDiagnosticOffset,
    },
    Preserved {
        file_id: rue_span::FileId,
        start: u32,
        end: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyDiagnosticBasis {
    pub(crate) coordinates: Arc<[BodyDiagnosticCoordinate]>,
}

pub(crate) fn relative_body_diagnostics(
    errors: crate::CompileErrors,
    source: &BodySourceLocator,
) -> (crate::CompileErrors, BodyDiagnosticBasis) {
    let mut coordinates = Vec::new();
    let errors = errors.map_spans(|span| {
        let coordinate = if span.file_id == source.file_id
            && span.start >= source.declaration_start
            && span.end <= source.body_end
        {
            let offset = |position| {
                if position >= source.body_start {
                    BodyDiagnosticOffset::Body(position - source.body_start)
                } else {
                    BodyDiagnosticOffset::Declaration(position - source.declaration_start)
                }
            };
            BodyDiagnosticCoordinate::Relative {
                start: offset(span.start),
                end: offset(span.end),
            }
        } else {
            BodyDiagnosticCoordinate::Preserved {
                file_id: span.file_id,
                start: span.start,
                end: span.end,
            }
        };
        coordinates.push(coordinate);

        // The typed coordinate stream owns every location. Erasing the payload
        // prevents stale absolute positions from entering semantic equality and
        // makes projection independent of any otherwise-valid FileId value.
        let mut erased = span;
        erased.file_id = rue_span::FileId::DEFAULT;
        erased.start = 0;
        erased.end = 0;
        erased
    });
    (
        errors,
        BodyDiagnosticBasis {
            coordinates: coordinates.into(),
        },
    )
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

/// Exact canonical candidate plan requested by the body evaluator.
///
/// The current source locator owns absolute presentation state. The artifact
/// owns both normalized structure and its candidate-relative diagnostic basis,
/// so sibling/prefix relocation can refresh the locator without invalidating a
/// retained body transaction, while internal coordinate changes dirty the
/// artifact and transaction together.
#[derive(Debug, Clone)]
pub(crate) struct OwnedBodyInput {
    pub(crate) owner: crate::StableDefinitionKey,
    pub(crate) source: BodySourceLocator,
    pub(crate) artifacts: Arc<crate::canonical_lower::DeclarationBodyPlanArtifacts>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyInputIncomplete {
    UnsupportedInstance,
    UnsupportedKind(crate::StableDefinitionKind),
    Generic,
    Extern,
    BodyPlanFailure(crate::revisioned_query_database::DeclarationBodyPlanFailure),
    MissingPrerequisite(Arc<str>),
}

#[derive(Debug, Clone)]
pub(crate) enum BodyInputValue {
    Available(OwnedBodyInput),
    Incomplete(BodyInputIncomplete),
}

#[cfg(test)]
pub(crate) fn body_input_equal(left: &BodyInputValue, right: &BodyInputValue) -> bool {
    match (left, right) {
        (BodyInputValue::Available(left), BodyInputValue::Available(right)) => {
            left.owner == right.owner
                && left.artifacts.plan.structurally_eq(&right.artifacts.plan)
                && left.source.physical_path == right.source.physical_path
        }
        (BodyInputValue::Incomplete(left), BodyInputValue::Incomplete(right)) => left == right,
        _ => false,
    }
}

pub(crate) struct BodyQueryKeyData {
    pub(crate) instance: crate::FunctionInstanceKey,
    pub(crate) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    display_identity: OnceLock<Arc<str>>,
}

/// One immutable body identity shared by its independently stamped query
/// projections.
///
/// Body analysis deliberately carries the same key through several families.
/// Keeping the payload behind one `Arc` makes those clones constant-size and
/// lets every memo node share the diagnostic identity formatted on the first
/// family miss.
#[derive(Clone)]
pub(crate) struct BodyQueryKey(Arc<BodyQueryKeyData>);

impl BodyQueryKey {
    pub(crate) fn new(
        instance: crate::FunctionInstanceKey,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    ) -> Self {
        Self(Arc::new(BodyQueryKeyData {
            instance,
            configuration,
            display_identity: OnceLock::new(),
        }))
    }

    fn format_identity(&self) -> String {
        format!("{:?}:{:?}", self.instance, self.configuration)
    }
}

impl Deref for BodyQueryKey {
    type Target = BodyQueryKeyData;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Debug for BodyQueryKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BodyQueryKey")
            .field("instance", &self.instance)
            .field("configuration", &self.configuration)
            .finish()
    }
}

impl PartialEq for BodyQueryKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.instance == other.instance && self.configuration == other.configuration)
    }
}

impl Eq for BodyQueryKey {}

impl Hash for BodyQueryKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.instance.hash(state);
        self.configuration.hash(state);
    }
}

impl QueryKey for BodyQueryKey {
    fn stable_identity(&self) -> String {
        self.format_identity()
    }

    fn shared_stable_identity(&self) -> Arc<str> {
        self.display_identity
            .get_or_init(|| self.format_identity().into())
            .clone()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.instance.hash(hasher);
        self.configuration.hash(hasher);
    }
}

/// Request-independent semantic body shared by every stamped projection and
/// downstream CFG input. This type deliberately is not `Clone`: query
/// boundaries share its immutable allocation through `Arc` instead of copying
/// instructions, places, and strings.
#[derive(Debug, PartialEq, Eq)]
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
    /// Exact type whose value is destroyed by this body.  This is distinct
    /// from an ordinary type mention: only this edge can root drop glue.
    DropGlue(crate::TypeInstanceKey),
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

/// Canonical sorted, duplicate-free body-reference summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyReferences(pub(crate) Arc<[BodyReference]>);

fn merge_ordered_unique<T: Clone + Ord>(
    existing: Arc<[T]>,
    selected: std::collections::BTreeSet<T>,
) -> Arc<[T]> {
    if selected.is_empty() {
        return existing;
    }
    if existing.is_empty() {
        return selected.into_iter().collect::<Vec<_>>().into();
    }

    let mut merged = Vec::with_capacity(existing.len() + selected.len());
    let mut existing = existing.iter().peekable();
    let mut selected = selected.into_iter().peekable();
    loop {
        match (existing.peek(), selected.peek()) {
            (Some(left), Some(right)) => match (*left).cmp(right) {
                std::cmp::Ordering::Less => {
                    merged.push(existing.next().expect("peeked existing value").clone());
                }
                std::cmp::Ordering::Equal => {
                    existing.next();
                    merged.push(selected.next().expect("peeked selected value"));
                }
                std::cmp::Ordering::Greater => {
                    merged.push(selected.next().expect("peeked selected value"));
                }
            },
            (Some(_), None) => {
                merged.extend(existing.cloned());
                break;
            }
            (None, Some(_)) => {
                merged.extend(selected);
                break;
            }
            (None, None) => break,
        }
    }
    merged.into()
}

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
        body: Arc<CanonicalBody>,
        references: BodyReferences,
        produced_anonymous_nominals: BodyProducedAnonymousNominals,
        consulted_anonymous_nominals: BodyConsultedAnonymousNominals,
        lookup_observations: BodyLookupObservations,
    },
    DeterministicFailure {
        errors: crate::CompileErrors,
        diagnostic_basis: Option<BodyDiagnosticBasis>,
        references: BodyReferences,
        lookup_observations: BodyLookupObservations,
    },
    Control(BodyTransactionControl),
}

#[derive(Debug, Clone)]
pub(crate) struct BodyAnalysisBundle {
    // This aggregate is semantic-only so its enclosing BodyClosure can stay
    // green across relocation. Presentation consumers request the exact
    // BodySourceLocator projection for their current revision.
    pub(crate) transaction: BodyTransaction,
    pub(crate) produced_anonymous: Option<ProducedAnonymous>,
}

pub(crate) fn analysis_bundle_equal(left: &BodyAnalysisBundle, right: &BodyAnalysisBundle) -> bool {
    transaction_equal(&left.transaction, &right.transaction)
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

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.modules.hash(hasher);
        self.roots.hash(hasher);
        self.configuration.hash(hasher);
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

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        rue_query::QueryKey::stable_hash(&self.closure, hasher);
        self.epoch.hash(hasher);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BodyClosureBody {
    pub(crate) key: BodyQueryKey,
    pub(crate) bundle: Arc<rue_query::QueryTerminal<BodyAnalysisBundle>>,
}

#[derive(Debug, Clone)]
pub(crate) struct BodyReachabilityOutput {
    pub(crate) reached: Arc<[crate::FunctionInstanceKey]>,
    pub(crate) demanded_drop_glue: Arc<[crate::TypeInstanceKey]>,
    pub(crate) demanded_drop_glue_plans:
        Arc<[(crate::TypeInstanceKey, crate::type_queries::DropGlueFacts)]>,
    pub(crate) scheduling_errors: Arc<[(crate::FunctionInstanceKey, crate::CompileErrors)]>,
    pub(crate) fatal: Option<BodyClosureFatal>,
    pub(crate) parked_toolchain: Option<crate::ParkedToolchainModules>,
}

pub(crate) fn body_reachability_output_equal(
    left: &BodyReachabilityOutput,
    right: &BodyReachabilityOutput,
) -> bool {
    left.reached == right.reached
        && left.demanded_drop_glue == right.demanded_drop_glue
        && left.demanded_drop_glue_plans == right.demanded_drop_glue_plans
        && left.scheduling_errors == right.scheduling_errors
        && left.fatal == right.fatal
        && left.parked_toolchain == right.parked_toolchain
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
    TypeQuery {
        ty: Option<crate::TypeInstanceKey>,
        detail: Arc<str>,
    },
    AnonymousDigestCollision {
        digest: u128,
        first: crate::AnonymousNominalKey,
        second: crate::AnonymousNominalKey,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct BodyClosureOutput {
    pub(crate) reached: Arc<[crate::FunctionInstanceKey]>,
    pub(crate) demanded_drop_glue: Arc<[crate::TypeInstanceKey]>,
    pub(crate) demanded_drop_glue_plans:
        Arc<[(crate::TypeInstanceKey, crate::type_queries::DropGlueFacts)]>,
    pub(crate) bodies: Arc<[BodyClosureBody]>,
    pub(crate) scheduling_errors: Arc<[(crate::FunctionInstanceKey, crate::CompileErrors)]>,
    pub(crate) fatal: Option<BodyClosureFatal>,
    pub(crate) parked_toolchain: Option<crate::ParkedToolchainModules>,
}

pub(crate) fn body_closure_output_equal(
    left: &BodyClosureOutput,
    right: &BodyClosureOutput,
) -> bool {
    left.reached == right.reached
        && left.demanded_drop_glue == right.demanded_drop_glue
        && left.demanded_drop_glue_plans == right.demanded_drop_glue_plans
        && left.bodies.len() == right.bodies.len()
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

impl RetainedCharge for BodySourceLocator {
    fn retained_charge(&self) -> u64 {
        self.physical_path.retained_charge()
    }
}

impl RetainedCharge for BodyDiagnosticCoordinate {
    fn retained_charge(&self) -> u64 {
        0
    }
}

impl RetainedCharge for BodyDiagnosticBasis {
    fn retained_charge(&self) -> u64 {
        self.coordinates.retained_charge()
    }
}

impl RetainedCharge for OwnedBodyInput {
    fn retained_charge(&self) -> u64 {
        self.owner
            .retained_charge()
            .saturating_add(self.source.retained_charge())
            .saturating_add(self.artifacts.plan.retained_charge())
    }
}

impl RetainedCharge for BodyInputIncomplete {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::BodyPlanFailure(failure) => failure.retained_charge(),
            Self::MissingPrerequisite(detail) => detail.retained_charge(),
            Self::UnsupportedInstance | Self::UnsupportedKind(_) | Self::Generic | Self::Extern => {
                0
            }
        }
    }
}

impl RetainedCharge for BodyInputValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(input) => input.retained_charge(),
            Self::Incomplete(incomplete) => incomplete.retained_charge(),
        }
    }
}

impl RetainedCharge for BodyQueryKey {
    fn retained_charge(&self) -> u64 {
        self.instance.retained_charge()
    }
}

impl RetainedCharge for CanonicalBody {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Ordinary { owner, body } => owner
                .retained_charge()
                .saturating_add(body.retained_charge()),
            Self::Anonymous { identity, body, .. } => identity
                .retained_charge()
                .saturating_add(body.retained_charge()),
            Self::Specialization {
                identity,
                body,
                dependencies,
                ..
            } => identity
                .retained_charge()
                .saturating_add(body.retained_charge())
                .saturating_add(dependencies.retained_charge()),
        }
    }
}

impl RetainedCharge for BodyReference {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Callable(value) => value.retained_charge(),
            Self::Definition(value) => value.retained_charge(),
            Self::Type(value) | Self::DropGlue(value) => value.retained_charge(),
        }
    }
}

impl RetainedCharge for WellKnownOptionResolution {
    fn retained_charge(&self) -> u64 {
        self.option_by_payload
            .retained_charge()
            .saturating_add(self.anonymous_nominals.retained_charge())
    }
}

impl RetainedCharge for BodyReferences {
    fn retained_charge(&self) -> u64 {
        self.0.retained_charge()
    }
}

impl RetainedCharge for BodyLookupObservations {
    fn retained_charge(&self) -> u64 {
        self.terminals.retained_charge()
    }
}

impl RetainedCharge for BodyTransactionControl {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::DeferredAnonymousProducers(values) => values.retained_charge(),
            Self::ProducerFailed(failure) => failure.retained_charge(),
            Self::WellKnownOptionResolution(failure) => failure.retained_charge(),
        }
    }
}

impl RetainedCharge for WellKnownOptionResolutionFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Incomplete {
                prerequisite,
                detail,
                ..
            } => prerequisite
                .retained_charge()
                .saturating_add(detail.retained_charge()),
            Self::Semantic { failure, .. } => failure.retained_charge(),
            Self::WrongProjection { detail, .. } => detail.retained_charge(),
        }
    }
}

impl RetainedCharge for BodyProducedAnonymousNominals {
    fn retained_charge(&self) -> u64 {
        self.0.retained_charge()
    }
}

impl RetainedCharge for BodyConsultedAnonymousNominals {
    fn retained_charge(&self) -> u64 {
        self.0.retained_charge()
    }
}

impl RetainedCharge for ProducedAnonymous {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Produced(value) => value.retained_charge(),
            Self::ProducerFailed(failure) => failure.retained_charge(),
        }
    }
}

impl RetainedCharge for BodyTransaction {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Success {
                body,
                references,
                produced_anonymous_nominals,
                consulted_anonymous_nominals,
                lookup_observations,
            } => body
                .retained_charge()
                .saturating_add(references.retained_charge())
                .saturating_add(produced_anonymous_nominals.retained_charge())
                .saturating_add(consulted_anonymous_nominals.retained_charge())
                .saturating_add(lookup_observations.retained_charge()),
            Self::DeterministicFailure {
                errors,
                diagnostic_basis,
                references,
                lookup_observations,
            } => errors
                .retained_charge()
                .saturating_add(diagnostic_basis.retained_charge())
                .saturating_add(references.retained_charge())
                .saturating_add(lookup_observations.retained_charge()),
            Self::Control(control) => control.retained_charge(),
        }
    }
}

impl RetainedCharge for BodyAnalysisBundle {
    fn retained_charge(&self) -> u64 {
        self.transaction
            .retained_charge()
            .saturating_add(self.produced_anonymous.retained_charge())
    }
}

impl RetainedCharge for BodyClosureBody {
    fn retained_charge(&self) -> u64 {
        self.key
            .retained_charge()
            .saturating_add(self.bundle.retained_charge())
    }
}

impl RetainedCharge for BodyClosureFatal {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::DeclarationFailed {
                declaration,
                failure,
            } => declaration
                .retained_charge()
                .saturating_add(failure.retained_charge()),
            Self::BodyAvailability { instance, detail } => instance
                .retained_charge()
                .saturating_add(detail.retained_charge()),
            Self::TypeQuery {
                ty: Some(instance),
                detail,
            } => instance
                .retained_charge()
                .saturating_add(detail.retained_charge()),
            Self::TypeQuery { ty: None, detail } => detail.retained_charge(),
            Self::ProducerFailed { instance, failure } => instance
                .retained_charge()
                .saturating_add(failure.retained_charge()),
            Self::WellKnownOptionResolution { instance, failure } => instance
                .retained_charge()
                .saturating_add(failure.retained_charge()),
            Self::AnonymousDigestCollision { first, second, .. } => first
                .retained_charge()
                .saturating_add(second.retained_charge()),
        }
    }
}

impl RetainedCharge for BodyReachabilityOutput {
    fn retained_charge(&self) -> u64 {
        self.reached
            .retained_charge()
            .saturating_add(self.demanded_drop_glue.retained_charge())
            .saturating_add(self.demanded_drop_glue_plans.retained_charge())
            .saturating_add(self.scheduling_errors.retained_charge())
            .saturating_add(self.fatal.retained_charge())
            .saturating_add(self.parked_toolchain.retained_charge())
    }
}

impl RetainedCharge for BodyClosureOutput {
    fn retained_charge(&self) -> u64 {
        self.reached
            .retained_charge()
            .saturating_add(self.demanded_drop_glue.retained_charge())
            .saturating_add(self.demanded_drop_glue_plans.retained_charge())
            .saturating_add(self.bodies.retained_charge())
            .saturating_add(self.scheduling_errors.retained_charge())
            .saturating_add(self.fatal.retained_charge())
            .saturating_add(self.parked_toolchain.retained_charge())
    }
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
        selected_references: std::collections::BTreeSet<BodyReference>,
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
                debug_assert!(
                    references.0.windows(2).all(|pair| pair[0] < pair[1]),
                    "body-reference summaries must be canonical before publication"
                );
                if !selected_references.is_empty() {
                    let existing = std::mem::replace(&mut references.0, Arc::from([]));
                    references.0 = merge_ordered_unique(existing, selected_references);
                }
                *stored = lookup_observations;
            }
            Self::Control(_) => {}
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{BodyQueryKey, merge_ordered_unique};
    use rue_air::Node;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use rue_query::QueryKey;

    #[test]
    fn cloned_body_keys_share_one_lazy_display_identity() {
        let key = BodyQueryKey::new(
            crate::FunctionInstanceKey::DropGlue(Node::new(crate::TypeInstanceKey::I64)),
            crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: rue_target::Target::X86_64Linux,
                preview_features: crate::StablePreviewFeatures::new(
                    &crate::PreviewFeatures::default(),
                ),
            },
        );
        let cloned = key.clone();
        assert!(key.display_identity.get().is_none());

        let first = key.shared_stable_identity();
        let second = cloned.shared_stable_identity();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.as_ref(), key.stable_identity());
    }

    #[test]
    fn ordered_unique_merge_handles_empty_overlap_and_interleaving() {
        let empty = merge_ordered_unique(Arc::from([]), BTreeSet::<u8>::new());
        assert_eq!(&*empty, &[] as &[u8]);

        let selected = BTreeSet::from([1, 3, 5]);
        assert_eq!(&*merge_ordered_unique(Arc::from([]), selected), &[1, 3, 5]);

        let selected = BTreeSet::from([2, 3, 6]);
        assert_eq!(
            &*merge_ordered_unique(Arc::from([1, 3, 4, 7]), selected),
            &[1, 2, 3, 4, 6, 7]
        );

        let selected = BTreeSet::new();
        assert_eq!(
            &*merge_ordered_unique(Arc::from([1, 2, 3]), selected),
            &[1, 2, 3]
        );
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
                diagnostic_basis: left_basis,
                references: left_references,
                ..
            },
            BodyTransaction::DeterministicFailure {
                errors: right_errors,
                diagnostic_basis: right_basis,
                references: right_references,
                ..
            },
        ) => {
            left_errors == right_errors
                && left_basis == right_basis
                && left_references == right_references
        }
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
