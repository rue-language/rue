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

/// Stable AIR identity of an admitted durable comptime program. Candidate
/// syntax is retained separately as lookup provenance and is never used as an
/// alternate owner key.
pub(crate) type DurableComptimeProgramKey = rue_air::ComptimeProgramKey<
    crate::StableDefinitionKey,
    crate::semantic_query_nucleus::SemanticQueryConfiguration,
>;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct DurableComptimeProgramMetadata {
    pub(crate) imports: Arc<[DurableComptimeImportOccurrence]>,
    pub(crate) root: OwnedComptimeProgramRoot,
}

pub(crate) type DurableComptimeProgram =
    rue_air::ComptimeProgram<Arc<str>, DurableComptimeProgramMetadata>;

pub(crate) type DurableComptimeProgramRegistry = rue_air::ComptimeProgramRegistry<
    crate::StableDefinitionKey,
    crate::semantic_query_nucleus::SemanticQueryConfiguration,
    Arc<str>,
    DurableComptimeProgramMetadata,
>;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeProgramPlan {
    pub(crate) key: DurableComptimeProgramKey,
    /// Compiler-private syntax provenance used to fetch the canonical body
    /// plan. It is not part of the stable program identity.
    pub(crate) candidate: crate::declaration_candidate::DeclarationCandidateKey,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignComptimeCallable {
    pub(crate) body: rue_rir::InstRef,
    pub(crate) context: crate::ModuleId,
    pub(crate) root: rue_rir::InstRef,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OwnedComptimeProgramRoot {
    Callable(ForeignComptimeCallable),
    Const {
        init: rue_rir::InstRef,
        declared_type: Option<rue_rir::RirTypeSyntaxRef>,
        root: rue_rir::InstRef,
    },
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeImportOccurrence {
    /// Owner-local instruction in the admitted RIR. The future host must use
    /// this only with the `rir` retained by the same program payload.
    pub(crate) inst: rue_rir::InstRef,
    pub(crate) occurrence: u32,
    pub(crate) specifier: Arc<str>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum ComptimeProgramProjectionFailure {
    Materialization(crate::canonical_lower::BodyPlanMaterializationFailure),
    Artifact(crate::revisioned_query_database::DeclarationBodyPlanFailure),
    ArtifactQueryFailure(rue_query::QueryFailure),
    NotFunction { root: rue_rir::InstRef },
    NotConst { root: rue_rir::InstRef },
    InvalidProducer(crate::StableDefinitionKey),
    IdentityMismatch,
}

impl From<crate::canonical_lower::BodyPlanMaterializationFailure>
    for ComptimeProgramProjectionFailure
{
    fn from(error: crate::canonical_lower::BodyPlanMaterializationFailure) -> Self {
        Self::Materialization(error)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignComptimeCallSeed {
    pub(crate) type_arguments: Arc<[(Arc<str>, crate::durable_semantics::DurableType)]>,
    pub(crate) value_arguments: Arc<[(Arc<str>, crate::durable_semantics::DurableConstValue)]>,
}

/// One owning durable program core shared by const roots and admitted foreign
/// callables. Dense instruction references are meaningful only through this
/// exact core, so colliding references from different programs cannot alias.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct OwnedComptimeProgramCore {
    pub(crate) plan: DurableComptimeProgramPlan,
    program: DurableComptimeProgram,
}

/// Owned compiler/query-side admission payload for a durable comptime call.
/// The program core is shared with const-root payloads; the seed is
/// call-specific and preserves ordered argument substitutions.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct OwnedForeignComptimeProgram {
    pub(crate) core: Arc<OwnedComptimeProgramCore>,
    pub(crate) seed: ForeignComptimeCallSeed,
}

impl Deref for OwnedForeignComptimeProgram {
    type Target = OwnedComptimeProgramCore;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl Deref for OwnedComptimeProgramCore {
    type Target = DurableComptimeProgram;

    fn deref(&self) -> &Self::Target {
        &self.program
    }
}

impl OwnedComptimeProgramCore {
    pub(crate) fn root(&self) -> &OwnedComptimeProgramRoot {
        &self.program.imports.root
    }

    pub(crate) fn callable(&self) -> Option<&ForeignComptimeCallable> {
        match &self.program.imports.root {
            OwnedComptimeProgramRoot::Callable(callable) => Some(callable),
            OwnedComptimeProgramRoot::Const { .. } => None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn const_root(
        &self,
    ) -> Option<(
        rue_rir::InstRef,
        Option<rue_rir::RirTypeSyntaxRef>,
        rue_rir::InstRef,
    )> {
        match self.program.imports.root {
            OwnedComptimeProgramRoot::Const {
                init,
                declared_type,
                root,
            } => Some((init, declared_type, root)),
            OwnedComptimeProgramRoot::Callable(_) => None,
        }
    }

    pub(crate) fn register_into(
        &self,
        registry: &mut DurableComptimeProgramRegistry,
    ) -> Result<(), rue_air::ComptimeProgramRegistrationError> {
        registry.register(self.plan.key.clone(), self.program.clone())
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum ForeignComptimeCallLookup {
    Ready(crate::semantic_query_nucleus::ComptimeCallProjection),
    ReadyFailure(crate::semantic_query_nucleus::SemanticNucleusFailure),
    ReadyQueryFailure(rue_query::QueryFailure),
    Admitted(OwnedForeignComptimeProgram),
    AdmissionFailure(ComptimeProgramProjectionFailure),
    NotReady,
    UnexpectedReadyProjection,
}

impl OwnedForeignComptimeProgram {
    #[allow(dead_code)]
    pub(crate) fn from_body_plan(
        plan: DurableComptimeProgramPlan,
        artifacts: &crate::canonical_lower::DeclarationBodyPlanArtifacts,
        seed: ForeignComptimeCallSeed,
        mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<Self, ComptimeProgramProjectionFailure> {
        let core = OwnedComptimeProgramCore::from_body_plan(
            plan,
            artifacts,
            OwnedComptimeRootExpectation::Callable,
            &mut checkpoint,
        )?;
        Ok(Self { core, seed })
    }
}

impl OwnedComptimeProgramCore {
    pub(crate) fn from_callable_body_plan_without_imports(
        plan: DurableComptimeProgramPlan,
        artifacts: &crate::canonical_lower::DeclarationBodyPlanArtifacts,
        mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<Arc<Self>, ComptimeProgramProjectionFailure> {
        Self::from_body_plan_without_imports(
            plan,
            artifacts,
            OwnedComptimeRootExpectation::Callable,
            &mut checkpoint,
        )
    }
}

#[derive(Debug, Clone, Copy)]
enum OwnedComptimeRootExpectation {
    Callable,
    Const,
}

impl OwnedComptimeProgramCore {
    fn from_body_plan(
        plan: DurableComptimeProgramPlan,
        artifacts: &crate::canonical_lower::DeclarationBodyPlanArtifacts,
        expectation: OwnedComptimeRootExpectation,
        checkpoint: &mut impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<Arc<Self>, ComptimeProgramProjectionFailure> {
        let core = Self::from_body_plan_without_imports(plan, artifacts, expectation, checkpoint)?;
        Self::finalize_imports(core, || checkpoint())
    }

    fn from_body_plan_without_imports(
        plan: DurableComptimeProgramPlan,
        artifacts: &crate::canonical_lower::DeclarationBodyPlanArtifacts,
        expectation: OwnedComptimeRootExpectation,
        checkpoint: &mut impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<Arc<Self>, ComptimeProgramProjectionFailure> {
        if artifacts.candidate != plan.candidate {
            return Err(ComptimeProgramProjectionFailure::IdentityMismatch);
        }
        let (rir, spellings, root) = artifacts
            .plan
            .materialize_semantic_candidate_rir(checkpoint)
            .map_err(ComptimeProgramProjectionFailure::Materialization)?;
        let Some(expected_candidate) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &plan.key.declaration,
            )
        else {
            return Err(ComptimeProgramProjectionFailure::IdentityMismatch);
        };
        if expected_candidate != plan.candidate {
            return Err(ComptimeProgramProjectionFailure::IdentityMismatch);
        }
        let program_root = match (expectation, &rir.get(root).data) {
            (OwnedComptimeRootExpectation::Callable, rue_rir::InstData::FnDecl { body, .. }) => {
                OwnedComptimeProgramRoot::Callable(ForeignComptimeCallable {
                    body: *body,
                    context: plan.candidate.module.clone(),
                    root,
                })
            }
            (OwnedComptimeRootExpectation::Callable, _) => {
                return Err(ComptimeProgramProjectionFailure::NotFunction { root });
            }
            (
                OwnedComptimeRootExpectation::Const,
                rue_rir::InstData::ConstDecl { ty, init, .. },
            ) => OwnedComptimeProgramRoot::Const {
                init: *init,
                declared_type: *ty,
                root,
            },
            (OwnedComptimeRootExpectation::Const, _) => {
                return Err(ComptimeProgramProjectionFailure::NotConst { root });
            }
        };
        Ok(Arc::new(Self {
            plan,
            program: DurableComptimeProgram {
                rir: Arc::new(rir),
                symbols: spellings.into_iter().map(Arc::from).collect(),
                imports: DurableComptimeProgramMetadata {
                    imports: Vec::new().into(),
                    root: program_root,
                },
            },
        }))
    }

    pub(crate) fn finalize_imports(
        mut core: Arc<Self>,
        mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<Arc<Self>, ComptimeProgramProjectionFailure> {
        let imports = crate::revisioned_query_database::semantic_candidate_import_occurrences(
            &core.rir,
            &core.symbols.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            &mut checkpoint,
        )
        .map_err(|error| {
            ComptimeProgramProjectionFailure::Materialization(
                crate::canonical_lower::BodyPlanMaterializationFailure::Query(error),
            )
        })?
        .into_iter()
        .map(
            |(inst, (occurrence, specifier))| DurableComptimeImportOccurrence {
                inst,
                occurrence,
                specifier,
            },
        )
        .collect::<Vec<_>>();
        let program = Arc::get_mut(&mut core).expect("unshared durable core before imports");
        program.program.imports.imports = imports.into();
        Ok(core)
    }

    /// Materialize a const declaration into the shared owning program core.
    /// This validates identity and owns the init/type syntax, but deliberately
    /// does not evaluate either expression.
    #[allow(dead_code)]
    pub(crate) fn from_const_body_plan(
        plan: DurableComptimeProgramPlan,
        artifacts: &crate::canonical_lower::DeclarationBodyPlanArtifacts,
        mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<Arc<Self>, ComptimeProgramProjectionFailure> {
        Self::from_body_plan(
            plan,
            artifacts,
            OwnedComptimeRootExpectation::Const,
            &mut checkpoint,
        )
    }

    pub(crate) fn from_const_body_plan_without_imports(
        plan: DurableComptimeProgramPlan,
        artifacts: &crate::canonical_lower::DeclarationBodyPlanArtifacts,
        mut checkpoint: impl FnMut() -> Result<(), rue_query::QueryAbort>,
    ) -> Result<Arc<Self>, ComptimeProgramProjectionFailure> {
        Self::from_body_plan_without_imports(
            plan,
            artifacts,
            OwnedComptimeRootExpectation::Const,
            &mut checkpoint,
        )
    }
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
    use super::{
        BodyQueryKey, DurableComptimeProgramKey, DurableComptimeProgramPlan,
        ForeignComptimeCallSeed, OwnedForeignComptimeProgram, merge_ordered_unique,
    };
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

    #[test]
    fn foreign_program_projection_owns_rir_imports_and_preserves_seed_order() {
        let snapshot = crate::SourceSnapshot::single(
            "<foreign-program>",
            "fn target() -> i32 { @import(\"dep\"); 1 } fn sibling() -> i32 { 2 }",
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
        let sibling_candidate = module
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
        let sibling_artifacts = crate::canonical_lower::lower_parsed_declaration_body_plan(
            &module,
            &sibling_candidate,
            || Ok(()),
        )
        .unwrap();
        let producer = crate::StableDefinitionKey::from_stable_parts(
            candidate.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            candidate.name.clone(),
            None,
        );
        let plan = DurableComptimeProgramPlan {
            key: DurableComptimeProgramKey {
                declaration: producer,
                configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                    target: rue_target::Target::X86_64Linux,
                    preview_features: crate::StablePreviewFeatures::new(
                        &crate::PreviewFeatures::default(),
                    ),
                },
            },
            candidate,
        };
        let seed = ForeignComptimeCallSeed {
            type_arguments: Arc::from([
                (Arc::from("z"), crate::durable_semantics::DurableType::I32),
                (Arc::from("a"), crate::durable_semantics::DurableType::I64),
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
        let program =
            OwnedForeignComptimeProgram::from_body_plan(plan, &artifacts, seed.clone(), || Ok(()))
                .unwrap();
        assert!(matches!(
            OwnedForeignComptimeProgram::from_body_plan(
                program.plan.clone(),
                &sibling_artifacts,
                seed.clone(),
                || Ok(()),
            ),
            Err(super::ComptimeProgramProjectionFailure::IdentityMismatch)
        ));
        let mut wrong_candidate = program.plan.candidate.clone();
        wrong_candidate.name = Arc::from("sibling");
        let wrong_plan = DurableComptimeProgramPlan {
            key: program.plan.key.clone(),
            candidate: wrong_candidate,
        };
        assert!(matches!(
            OwnedForeignComptimeProgram::from_body_plan(
                wrong_plan,
                &artifacts,
                seed.clone(),
                || Ok(())
            ),
            Err(super::ComptimeProgramProjectionFailure::IdentityMismatch)
        ));
        drop(artifacts);

        assert_eq!(program.seed, seed, "argument order is request order");
        assert_eq!(
            program.callable().expect("callable root").context,
            program.plan.candidate.module
        );
        assert_eq!(
            program.callable().expect("callable root").root,
            program
                .rir
                .iter()
                .find_map(|(inst, instruction)| {
                    matches!(&instruction.data, rue_rir::InstData::FnDecl { .. }).then_some(inst)
                })
                .unwrap()
        );
        let rue_rir::InstData::FnDecl { body, .. } = &program
            .rir
            .get(program.callable().expect("callable root").root)
            .data
        else {
            panic!("the admitted callable root must remain a function declaration");
        };
        assert_eq!(program.callable().expect("callable root").body, *body);
        assert_eq!(program.imports.imports.len(), 1);
        let import = &program.imports.imports[0];
        assert_eq!(import.specifier.as_ref(), "dep");
        assert!(matches!(
            &program.rir.get(import.inst).data,
            rue_rir::InstData::Intrinsic { .. }
        ));

        let const_snapshot =
            crate::SourceSnapshot::single("<const-program>", "const target: i32 = 1;").unwrap();
        let const_module = crate::parsed_modules::parse_source_snapshot_modules(&const_snapshot)
            .unwrap()
            .modules()[0]
            .clone();
        let const_candidate = const_module
            .definitions()
            .declaration_keys_in_source_order()
            .find(|candidate| candidate.name.as_ref() == "target")
            .unwrap()
            .clone();
        let const_artifacts = crate::canonical_lower::lower_parsed_declaration_body_plan(
            &const_module,
            &const_candidate,
            || Ok(()),
        )
        .unwrap();
        let const_plan = DurableComptimeProgramPlan {
            key: DurableComptimeProgramKey {
                declaration: crate::StableDefinitionKey::from_stable_parts(
                    const_candidate.module.clone(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::ValueConst,
                    "target",
                    None,
                ),
                configuration: program.plan.key.configuration.clone(),
            },
            candidate: const_candidate,
        };
        let const_program = crate::body_query::OwnedComptimeProgramCore::from_const_body_plan(
            const_plan,
            &const_artifacts,
            || Ok(()),
        )
        .unwrap();
        assert!(const_program.callable().is_none());
        assert!(const_program.const_root().is_some());
        assert!(!const_program.symbols.is_empty());
        assert!(!const_program.rir.type_syntax().symbols().is_empty());
        assert!(
            !Arc::ptr_eq(&program.core, &const_program),
            "different stable producers must not share program identity"
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
