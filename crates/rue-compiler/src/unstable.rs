//! Explicitly unstable compiler instrumentation and test-support views.
//!
//! Nothing in this module is covered by the supported facade's compatibility
//! policy. These owned snapshots and opaque session products cannot be
//! installed into a session or used as query keys.

use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::Arc;

use crate::canonical_semantic::CanonicalSemanticFailurePhase as SemanticFailurePhase;

pub use crate::diagnostic::{
    ColorChoice, DiagnosticFormatter, JsonDiagnostic, JsonDiagnosticFormatter, JsonSpan,
    JsonSuggestion, MultiFileFormatter, MultiFileJsonFormatter, SourceInfo,
};
pub use crate::import_discovery::{
    DiscoverySourceAssembler, ImportDemandFrontier, ImportDemandMode, ImportDemandRoots,
    ImportInputRevision,
};

/// Begin a fresh external-input observation generation.
///
/// Prior observations can flow only through
/// [`publish_import_observation_batch`]; no caller ledger can seed freshness.
pub fn begin_import_input_request(
    session: &mut crate::CompilerSession,
    snapshot: &crate::SourceSnapshot,
    context: crate::ImportDiscoveryContext,
    accepted_reads: crate::AcceptedReadManifest,
) -> crate::CompileResult<ImportInputRevision> {
    session.begin_import_input_request(snapshot, context, accepted_reads)
}

pub fn import_demand_frontier_for_roots(
    session: &mut crate::CompilerSession,
    revision: ImportInputRevision,
    plan: &crate::ImportDiscoveryPlan,
    mode: ImportDemandMode,
    roots: &ImportDemandRoots,
) -> crate::CompileResult<ImportDemandFrontier> {
    session.import_demand_frontier_for_roots(revision, plan, mode, roots)
}

/// Stages the exact input revision currently owned by the compiler.
///
/// The host supplies only the opaque revision returned by
/// [`begin_import_input_request`] or [`publish_import_observation_batch`].
pub fn stage_import_input_request(
    session: &mut crate::CompilerSession,
    revision: ImportInputRevision,
) -> Result<crate::ImportDiscoveryPlan, crate::CompileErrors> {
    session.stage_import_input_request(revision)
}

/// The demand roots for a trusted-toolchain successor plan's delta occurrences
/// alone (RUE-1112). Derived directly from the plan's delta segment, so the host
/// roots its re-close frontier without materializing or filtering the merged
/// predecessor plan.
pub fn plan_delta_roots(plan: &crate::ImportDiscoveryPlan) -> ImportDemandRoots {
    plan.delta_roots()
}

pub fn publish_import_observation_batch(
    session: &mut crate::CompilerSession,
    frontier: &ImportDemandFrontier,
    snapshot: &crate::SourceSnapshot,
    accepted_reads: crate::AcceptedReadManifest,
    observations: Vec<crate::ImportObservation>,
) -> crate::CompileResult<ImportInputRevision> {
    session.publish_import_observation_batch(frontier, snapshot, accepted_reads, observations)
}

pub fn import_observation_ledger(
    session: &crate::CompilerSession,
    revision: ImportInputRevision,
) -> crate::CompileResult<crate::ImportObservationLedger> {
    session.import_observation_ledger(revision)
}

/// Closes the exact compiler-published input revision after its rooted
/// frontier is exhausted.
pub fn close_import_input_request(
    session: &mut crate::CompilerSession,
    revision: ImportInputRevision,
) -> Result<Arc<crate::ImportDiscoveryView>, crate::CompileErrors> {
    session.close_import_input_request(revision)
}

/// Stage a strictly-additive trusted-toolchain successor (RUE-1112). The staged
/// snapshot, context, provenance, and carried ledger are the CURRENT
/// compiler-published view's own state and the module delta is derived from the
/// opaque `delta` capability — the host supplies nothing but the capability, so
/// no replacement state can be substituted. Predecessor occurrences are never
/// re-staged.
pub fn stage_import_discovery_successor(
    session: &mut crate::CompilerSession,
    delta: &TrustedSuccessorDelta,
) -> Result<crate::ImportDiscoveryPlan, crate::CompileErrors> {
    session.stage_import_discovery_successor(delta)
}

/// Close a strictly-additive trusted-toolchain successor (RUE-1112). The closing
/// ledger is the CURRENT compiler-published view's own carried ledger and the
/// module delta is derived from the opaque `delta` capability — the host
/// supplies nothing but the capability. Predecessor occurrences are never
/// re-projected or re-reduced.
pub fn close_import_discovery_successor(
    session: &mut crate::CompilerSession,
    delta: &TrustedSuccessorDelta,
) -> Result<Arc<crate::ImportDiscoveryView>, crate::CompileErrors> {
    session
        .close_import_discovery_successor(delta)
        .map(|artifact| Arc::new(crate::ImportDiscoveryView::new(artifact)))
}

/// Cumulative import occurrences the demand frontier has rooted (RUE-1112).
///
/// One `ResolveImport` projection is dispatched per rooted occurrence. The delta
/// across a trusted-toolchain re-close counts only occurrences owned by the newly
/// appended leaves and modules newly discovered from them; a predecessor
/// occurrence is rooted once at the initial close and never re-rooted during
/// acquisition. Host source-loading reads this to prove the re-close is O(new
/// leaves), independent of the predecessor import topology.
pub fn import_frontier_roots_requested(session: &crate::CompilerSession) -> u64 {
    session.import_frontier_roots_requested()
}

/// Cumulative import-plan request groups constructed during staging (RUE-1112).
///
/// A full plan build constructs one group per program import occurrence; a
/// trusted-toolchain successor stage reuses the committed predecessor plan's
/// groups and constructs only the newly appended occurrences'. The delta across a
/// re-close is O(new leaves), independent of the predecessor topology.
pub fn import_plan_groups_constructed(session: &crate::CompilerSession) -> u64 {
    session.import_plan_groups_constructed()
}

/// Cumulative close-time `ResolveImport` projections dispatched (RUE-1112).
///
/// A full close projects one per program occurrence; a trusted-toolchain
/// successor close projects only the newly appended occurrences'. The delta
/// across a re-close is O(new leaves).
pub fn exact_import_groups_dispatched(session: &crate::CompilerSession) -> u64 {
    session.exact_import_groups_dispatched()
}

/// Cumulative canonical import records reduced and validated during close
/// (RUE-1112).
///
/// A full close reduces/validates one per program occurrence; a trusted-toolchain
/// successor close carries the predecessor's closed graph and reduces/validates
/// only the newly appended occurrences'. The delta across a re-close is O(new
/// leaves).
pub fn import_close_records_reduced(session: &crate::CompilerSession) -> u64 {
    session.import_close_records_reduced()
}

/// Cumulative leaves published through the complete input-publication path
/// (fresh generations only). Scales with the program (RUE-1112).
pub fn import_view_full_leaves_published(session: &crate::CompilerSession) -> u64 {
    session.import_view_full_leaves_published()
}

/// Cumulative delta leaves published through the sparse successor overlay path
/// (RUE-1112). Each same-generation successor publishes only its own additions
/// plus at most one re-stamped aggregate topology leaf; predecessor leaves are
/// structurally inherited and never republished, so the acquisition delta is
/// O(new leaves), independent of the predecessor topology.
pub fn import_view_overlay_leaves_published(session: &crate::CompilerSession) -> u64 {
    session.import_view_overlay_leaves_published()
}

/// Cumulative predecessor ledger observations deep-cloned into successor view
/// ledgers (RUE-1112). The successor view still carries a complete ledger value,
/// so this cost remains predecessor-scale and is surfaced rather than hidden.
pub fn import_view_ledger_entries_cloned(session: &crate::CompilerSession) -> u64 {
    session.import_view_ledger_entries_cloned()
}

/// Predecessor source entries element-compared by the overlay publication's
/// fallback diff (RUE-1112). The structural-authority path — a successor
/// snapshot whose segments share the parent view by `Arc` identity — never
/// compares a predecessor entry, so acquisition keeps this at zero.
pub fn import_view_source_entries_compared(session: &crate::CompilerSession) -> u64 {
    session.import_view_source_entries_compared()
}

/// Predecessor accepted-read entries element-compared by the overlay
/// publication's fallback provenance diff (RUE-1112) — a host-rebuilt manifest
/// that cannot prove itself by segment identity. The structural-authority path
/// never increments this.
pub fn import_view_read_entries_compared(session: &crate::CompilerSession) -> u64 {
    session.import_view_read_entries_compared()
}

/// Source entries materialized into whole-program parse projections
/// (RUE-1112): the presentation order, demanded module set, and merged program
/// a FULL parse build enumerates. A trusted-toolchain successor stage extends
/// the retained predecessor artifact and never increments this.
pub fn parse_sources_materialized(session: &crate::CompilerSession) -> u64 {
    session.parse_sources_materialized()
}

/// Source entries embedded in parse query keys (RUE-1112): an ordinary key
/// carries every file's exact content identity; a successor key carries only
/// the published lineage identity plus its appended segment.
pub fn parse_key_entries_compared(session: &crate::CompilerSession) -> u64 {
    session.parse_key_entries_compared()
}

/// Module parse queries dispatched by the parse projection (RUE-1112). A full
/// build dispatches one per module; a successor stage dispatches only the
/// appended modules'.
pub fn parse_modules_dispatched(session: &crate::CompilerSession) -> u64 {
    session.parse_modules_dispatched()
}

/// Entries examined by parse invalidation classification (RUE-1112). A full
/// classification examines every current module; a successor classifies only
/// its appended delta.
pub fn parse_invalidation_entries_compared(session: &crate::CompilerSession) -> u64 {
    session.parse_invalidation_entries_compared()
}

/// An owned snapshot of the provider-op observation counters (RUE-1091,
/// ADR-0066 §4): how many facts of each §4 family the exact body-fact provider
/// observed. Owned plain data, not a query-engine record. These are live
/// production counters from the registered provider-native body evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProviderObservationMetrics {
    /// Name-lookup observations (unqualified, qualified, and language-item).
    pub name_lookups: u64,
    /// Import/module-binding observations.
    pub import_lookups: u64,
    /// Method-candidate observations.
    pub method_candidates: u64,
    /// Operator-candidate observations.
    pub operator_candidates: u64,
    /// Declaration identity/signature/const/well-formedness observations.
    pub declaration_facts: u64,
    /// Exact declaration-identity provider reads.
    pub identity_facts: u64,
    /// Exact callable/nominal signature provider reads.
    pub signature_facts: u64,
    /// Exact nominal well-formedness/type provider reads.
    pub type_facts: u64,
    /// Exact constant and comptime-reduction provider reads.
    pub const_facts: u64,
    /// Durable facts materialized into a body-local overlay.
    pub materializations: u64,
    /// Anonymous-nominal fact observations.
    pub anonymous_facts: u64,
    /// Producer-body fact observations.
    pub producer_facts: u64,
    /// Trusted-toolchain fact observations.
    pub toolchain_facts: u64,
}

/// A snapshot of the provider-op observation counters. See
/// [`ProviderObservationMetrics`].
pub fn provider_observation_metrics(
    session: &crate::CompilerSession,
) -> ProviderObservationMetrics {
    session.provider_observation_metrics()
}

/// A snapshot of the lookup-family pressure metrics (RUE-1091, ADR-0066 §4): the
/// session-held `PublishedRootLookupLease`'s retained working set and its
/// grow-with-pressure, eviction, and rederivation-after-eviction accounting.
///
/// The lease-scoped fields are live production measurements. The
/// `retained_family_*` fields report the lookup families' current runtime
/// retention and are informational.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LookupPressureMetrics {
    /// Distinct published roots currently held by the session lease.
    pub published_roots: u64,
    /// Total lookup-terminal pins the lease currently holds across all roots.
    pub leased_terminals: u64,
    /// Distinct logical lookup keys currently retained under the lease.
    pub retained_logical_keys: u64,
    /// Lookup-family logical memo nodes currently retained by the runtime
    /// (name plus, under test, import). Informational; nonzero on production.
    pub retained_family_nodes: u64,
    /// Lookup-family terminals currently retained by the runtime (name plus,
    /// under test, import). Informational; nonzero on production.
    pub retained_family_terminals: u64,
    /// Grow-with-pressure gauge: how far the lookup families' currently retained
    /// terminals exceed their configured historical floor. This is a live gauge
    /// (retained-terminals-above-floor), not a cumulative count of the runtime's
    /// `retention_growth` events: the runtime grows a family past its floor only
    /// when every eviction candidate is a protected root — a waiter, an explicit
    /// pin, a request-scoped observation lease, or a retained revision — so any
    /// excess is a set held above the floor by protection of some kind rather
    /// than an eviction of a name merely because a large program consults more
    /// than the floor.
    pub protected_growth: u64,
    /// Lookup terminals evicted while a superseded root batch-released — the
    /// runtime eviction delta captured across the prior root's release.
    pub evictions: u64,
    /// Lookup keys re-observed with a changed node incarnation: a key whose
    /// retained terminal is gone (evicted, or otherwise a fresh node) so the
    /// re-observation sees a new incarnation. Under retention pressure this is
    /// eviction-forced rederivation — the acceptance falsifier (invisible to
    /// correctness: the recomputed value equals the evicted one) — but a changed
    /// incarnation from a legitimate source-driven recompute counts here too.
    pub rederivations_after_eviction: u64,
}

/// A snapshot of the live lookup-family pressure metrics. See
/// [`LookupPressureMetrics`].
pub fn lookup_pressure_metrics(session: &crate::CompilerSession) -> LookupPressureMetrics {
    session.lookup_pressure_metrics()
}

/// Cumulative dependency-graph invalidation events across the retained
/// frontend query families (RUE-1112). A strictly-additive successor adoption
/// keeps the predecessor's immutable source leaf live and contributes zero
/// here; only a genuine replacement invalidates retained dependents.
pub fn frontend_query_invalidations(session: &crate::CompilerSession) -> u64 {
    session.frontend_query_invalidations()
}

/// Structural-sharing witnesses for the committed import discovery's three
/// additively shared artifacts (RUE-1112): `[graph_records, plan_groups,
/// resolution_modules]`, each `(predecessor_segment_address, delta_len)`. A
/// trusted-toolchain successor shares each predecessor segment `Arc` by
/// reference, so every address is identical to the predecessor close's — proving
/// no predecessor entry was copied when the successor artifacts were built.
pub fn committed_successor_sharing(
    session: &crate::CompilerSession,
) -> Option<[(usize, usize); 3]> {
    session.committed_successor_sharing()
}

pub use crate::session::{ClosedDiscoveryContinuation, SemanticParkOutcome, TrustedSuccessorDelta};

/// Run rooted, park-aware semantic analysis on the current committed revision,
/// surfacing an unsatisfied trusted-toolchain park distinctly (RUE-1112).
///
/// This is the host source-loading driver's retry entry: on
/// [`SemanticParkOutcome::Parked`] the park atomically attaches its exact
/// missing-demand set to the outstanding closed continuation, so a subsequent
/// [`closed_discovery_continuation`] mints an authorizing token; the driver
/// acquires exactly those modules, publishes a successor, re-closes, and calls
/// this again.
pub fn semantic_or_toolchain_park(
    session: &mut crate::CompilerSession,
    options: &crate::CompileOptions,
) -> SemanticParkOutcome {
    session.semantic_or_toolchain_park(options)
}

/// Mint the single-use trusted-toolchain continuation for the current successful
/// import-discovery close, if one is outstanding (RUE-1112).
pub fn closed_discovery_continuation(
    session: &crate::CompilerSession,
) -> Option<ClosedDiscoveryContinuation> {
    session.closed_discovery_continuation()
}

/// Publish one strictly-additive trusted-toolchain successor on the
/// continuation's closed revision, verified entirely from records (RUE-1112).
///
/// Returns an opaque [`TrustedSuccessorDelta`] capability derived from the
/// compiler-verified appended module set. The host carries it to the successor
/// stage and close, which derive the module delta from it; the host cannot
/// inspect or edit the module identities.
pub fn publish_trusted_toolchain_successor(
    session: &mut crate::CompilerSession,
    token: ClosedDiscoveryContinuation,
    issued_frontier: &ImportDemandFrontier,
    successor: &crate::SourceSnapshot,
    accepted_reads: crate::AcceptedReadManifest,
) -> Result<TrustedSuccessorDelta, crate::CompileErrors> {
    session.publish_trusted_toolchain_successor(token, issued_frontier, successor, accepted_reads)
}

/// Unstable human-readable compiler stages. These are projections of the
/// canonical session artifacts, never alternate phase entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationStage {
    Tokens,
    Ast,
    Rir,
    Air,
    Cfg,
    Lowering,
    Mir,
    Liveness,
    RegAlloc,
    Asm,
    StackFrame,
}

/// One explicitly unstable textual presentation request.
#[derive(Debug, Clone)]
pub struct PresentationRequest<'a> {
    pub stage: PresentationStage,
    pub options: &'a crate::CompileOptions,
    pub file_order: &'a [crate::FileId],
}

/// Owned unstable presentation text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationOutput {
    text: String,
}

/// Publish a source snapshot with caller-selected diagnostic presentation
/// order. This is a tooling adapter over the canonical parse query, not a
/// second parser or supported session operation.
pub fn update_for_presentation(
    session: &mut crate::CompilerSession,
    snapshot: &crate::SourceSnapshot,
) -> crate::CompilerSessionUpdate {
    session.update_for_presentation(snapshot)
}

/// Force the canonical merge query for in-tree query-metrics tooling without
/// exposing its raw owner.
pub fn query_merge(session: &mut crate::CompilerSession) -> Result<(), crate::CompileErrors> {
    session.merge().map(drop)
}

/// Execute the definition-binding query for benchmark instrumentation without
/// exposing its compiler-owned records.
pub fn prepare_stable_definitions(
    session: &mut crate::CompilerSession,
    options: &crate::CompileOptions,
) -> Result<(), crate::CompileErrors> {
    session.stable_definitions(options).map(drop)
}

/// Return the latest attempted discovery revision for in-tree source-loading
/// diagnostics. Stable consumers query committed graph and diagnostic views.
pub fn discovery_attempt(
    session: &crate::CompilerSession,
) -> Option<Arc<crate::ImportDiscoveryView>> {
    session
        .discovery_attempt_artifact()
        .map(|artifact| Arc::new(crate::ImportDiscoveryView::new(artifact.clone())))
}

/// Return the last closed-valid discovery revision for diagnostics tooling.
pub fn last_good_discovery(
    session: &crate::CompilerSession,
) -> Option<Arc<crate::ImportDiscoveryView>> {
    session
        .last_good_discovery_artifact()
        .map(|artifact| Arc::new(crate::ImportDiscoveryView::new(artifact.clone())))
}

/// Return the closed-valid discovery revision selected by the session.
pub fn committed_import_discovery(
    session: &crate::CompilerSession,
) -> Option<Arc<crate::ImportDiscoveryView>> {
    session
        .committed_import_discovery_artifact()
        .map(|artifact| Arc::new(crate::ImportDiscoveryView::new(artifact.clone())))
}

/// Debug rendering of the import-graph query input retained by a discovery view.
pub fn import_discovery_graph_input_debug(view: &crate::ImportDiscoveryView) -> Option<String> {
    view.inner
        .graph()
        .map(|graph| format!("{:?}", graph.input()))
}

/// Debug rendering of accepted reads retained by a discovery view.
pub fn import_discovery_accepted_reads_debug(view: &crate::ImportDiscoveryView) -> String {
    format!("{:?}", view.inner.accepted_read_manifest())
}

/// Debug rendering of the observation ledger retained by a discovery view.
pub fn import_discovery_observation_ledger_debug(view: &crate::ImportDiscoveryView) -> String {
    format!("{:?}", view.inner.ledger())
}

/// Run the fresh backend tail used by the cold-versus-reused differential
/// oracle. Stable callers use `CompilerSession::executable`.
pub fn oracle_executable(
    session: &mut crate::CompilerSession,
    snapshot: &crate::SourceSnapshot,
    options: &crate::CompileOptions,
) -> crate::MultiErrorResult<crate::CompileOutput> {
    session.oracle_executable(snapshot, options)
}

/// Produce an executable inside a compile span owned by the filesystem
/// driver. Stable callers use `CompilerSession::executable`, which owns its
/// tracing root.
pub fn executable_in_compile_scope(
    session: &mut crate::CompilerSession,
    options: &crate::CompileOptions,
) -> crate::MultiErrorResult<crate::CompileOutput> {
    session.executable_in_compile_scope(options)
}

/// Drive this session through the pre-link boundary (RIR → semantic → CFG →
/// codegen → object generation) without linking, returning the total generated
/// object-byte count. Used by the RUE-1086 scaling-bench runner to time a
/// genuinely pre-link interval; the ~45 ms Caldera target is a pre-link number.
pub fn pre_link_object_bytes(
    session: &mut crate::CompilerSession,
    options: &crate::CompileOptions,
) -> crate::MultiErrorResult<usize> {
    crate::queries::pre_link_object_bytes_with_session(session, options)
}

impl PresentationOutput {
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// Return semantic instrumentation without exposing the semantic owner.
pub fn semantic_metrics(view: &crate::SemanticView) -> SemanticMetrics {
    view.unstable_metrics()
}

pub fn semantic_input_debug(view: &crate::SemanticView) -> String {
    view.owner().unstable_input_debug()
}

/// Exact, owned semantic-state projection used by in-tree cold-vs-reused
/// differential tooling. Its contents and formatting are deliberately
/// unstable and are not an artifact that can be fed back into the compiler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticParitySnapshot {
    details: String,
}

impl SemanticParitySnapshot {
    pub(crate) fn new(details: String) -> Self {
        Self { details }
    }
}

pub fn semantic_parity_snapshot(view: &crate::SemanticView) -> SemanticParitySnapshot {
    view.owner().unstable_parity_snapshot()
}

/// Raw owner-crate state for the in-tree CFG differential model. This is an
/// explicitly unstable consuming bridge; ordinary compiler clients use views.
pub struct OracleSemanticState {
    pub interner: Arc<lasso::ThreadedRodeo>,
    pub functions: Vec<UnstableSemanticFunction>,
    pub type_pool: rue_air::FrozenTypeInternPool,
    pub strings: Vec<String>,
    pub rir_payload_storage_stats: rue_rir::RirPayloadStorageStats,
}

/// Raw function state for in-tree differential and storage-profile tooling.
pub struct UnstableSemanticFunction {
    pub analyzed: Arc<rue_air::AnalyzedFunction>,
    /// The declaration's source name when this callable is an ordinary
    /// definition. `analyzed.name` is the internal symbol, which an ordinary
    /// definition qualifies by module (RUE-1125), so a consumer that speaks in
    /// source terms reads this instead.
    pub source_name: Option<String>,
    pub cfg: rue_cfg::ValidatedCfg,
}

pub fn into_oracle_semantic_state(
    semantic: Arc<crate::SemanticView>,
) -> Result<OracleSemanticState, &'static str> {
    let semantic = Arc::try_unwrap(semantic).map_err(|_| "semantic view is still shared")?;
    let (semantic_owner, rir_owner) = semantic.into_owners();
    drop(rir_owner);
    let semantic_owner =
        Arc::try_unwrap(semantic_owner).map_err(|_| "semantic owner is still shared")?;
    let (rir_owner, functions, type_pool, strings, _) = semantic_owner.into_parts_with_rir();
    let rir_owner = Arc::try_unwrap(rir_owner).map_err(|_| "RIR owner is still shared")?;
    let rir_payload_storage_stats = rir_owner.rir().payload_storage_stats();
    let (_, symbols) = rir_owner.into_parts();
    Ok(OracleSemanticState {
        interner: symbols.into_interner(),
        functions: functions
            .into_iter()
            .map(|function| UnstableSemanticFunction {
                source_name: function.definition_source_name().map(str::to_owned),
                analyzed: function.analyzed,
                cfg: function.cfg,
            })
            .collect(),
        type_pool,
        strings,
        rir_payload_storage_stats,
    })
}

/// Inject one typed stale-query fault for in-tree differential testing.
pub fn inject_stale_query_for_oracle(
    session: &mut crate::CompilerSession,
    fault: DifferentialOracleFault,
) -> bool {
    session.inject_stale_query_for_oracle(fault)
}

impl crate::CompilerSession {
    /// Format one compiler stage from this session's canonical artifacts.
    pub fn unstable_present(
        &mut self,
        request: PresentationRequest<'_>,
    ) -> Result<PresentationOutput, crate::CompileErrors> {
        let invalid_input = |message: String| {
            crate::CompileErrors::from(crate::CompileError::without_span(
                crate::ErrorKind::InvalidCompilerInput(message),
            ))
        };
        let program = self.published_owner().cloned().ok_or_else(|| {
            invalid_input("presentation requires a published source revision".into())
        })?;
        let mut seen = std::collections::HashSet::with_capacity(request.file_order.len());
        for file_id in request.file_order {
            if !seen.insert(*file_id) {
                return Err(invalid_input(format!(
                    "presentation order contains duplicate file id {file_id:?}"
                )));
            }
            if !program
                .modules()
                .iter()
                .any(|module| module.file_id() == *file_id)
            {
                return Err(invalid_input(format!(
                    "presentation order contains unknown file id {file_id:?}"
                )));
            }
        }
        if !matches!(
            request.stage,
            PresentationStage::Tokens | PresentationStage::Ast
        ) && seen.len() != program.modules().len()
        {
            return Err(invalid_input(format!(
                "presentation order must contain every published file exactly once (expected {}, got {})",
                program.modules().len(),
                seen.len()
            )));
        }

        let mut text = String::new();
        match request.stage {
            PresentationStage::Tokens => {
                for file_id in request.file_order {
                    let module = program
                        .modules()
                        .iter()
                        .find(|module| module.file_id() == *file_id)
                        .ok_or_else(|| {
                            invalid_input(format!(
                                "presentation order contains unknown file id {file_id:?}"
                            ))
                        })?;
                    for token in module.tokens() {
                        writeln!(&mut text, "{token}").expect("write to String");
                    }
                }
            }
            PresentationStage::Ast => {
                for file_id in request.file_order {
                    let module = program
                        .modules()
                        .iter()
                        .find(|module| module.file_id() == *file_id)
                        .ok_or_else(|| {
                            invalid_input(format!(
                                "presentation order contains unknown file id {file_id:?}"
                            ))
                        })?;
                    write!(&mut text, "{}", module.ast()).expect("write to String");
                }
            }
            PresentationStage::Rir => {
                let rir = self.canonical_rir()?;
                let order = rir.presentation_order(request.file_order.iter().copied());
                write!(
                    &mut text,
                    "{}",
                    rue_rir::RirPrinter::with_presentation_order(
                        rir.rir(),
                        rir.semantic_symbols().interner(),
                        order.instructions,
                        order.extra,
                    )
                )
                .expect("write to String");
            }
            stage => {
                let semantic = self.canonical_semantic(request.options)?;
                let rir = semantic.rir_owner().clone();
                let interner = rir.semantic_symbols().interner();
                let backend_request = match stage {
                    PresentationStage::Lowering => Some(rue_codegen::BackendArtifactRequest {
                        lowering: true,
                        ..Default::default()
                    }),
                    PresentationStage::Mir => Some(rue_codegen::BackendArtifactRequest {
                        mir: true,
                        ..Default::default()
                    }),
                    PresentationStage::Liveness => Some(rue_codegen::BackendArtifactRequest {
                        liveness: true,
                        ..Default::default()
                    }),
                    PresentationStage::RegAlloc => Some(rue_codegen::BackendArtifactRequest {
                        regalloc: true,
                        ..Default::default()
                    }),
                    PresentationStage::Asm => Some(rue_codegen::BackendArtifactRequest {
                        asm: true,
                        ..Default::default()
                    }),
                    _ => None,
                };
                if let Some(backend_request) = backend_request {
                    let products =
                        self.codegen_products(&semantic, request.options, backend_request)?;
                    for product in products {
                        match stage {
                            PresentationStage::Lowering => {
                                write!(
                                    &mut text,
                                    "{}",
                                    product
                                        .artifacts
                                        .lowering
                                        .expect("lowering projection was requested")
                                )
                                .expect("write to String");
                            }
                            PresentationStage::Mir => {
                                writeln!(&mut text, "function {}:", product.machine_name)
                                    .expect("write to String");
                                writeln!(
                                    &mut text,
                                    "{}",
                                    product.artifacts.mir.expect("MIR projection was requested")
                                )
                                .expect("write to String");
                            }
                            PresentationStage::Liveness => {
                                writeln!(&mut text, "function {}:", product.machine_name)
                                    .expect("write to String");
                                writeln!(
                                    &mut text,
                                    "{}",
                                    product
                                        .artifacts
                                        .liveness
                                        .expect("liveness projection was requested")
                                )
                                .expect("write to String");
                            }
                            PresentationStage::RegAlloc => {
                                writeln!(&mut text, "function {}:", product.machine_name)
                                    .expect("write to String");
                                write!(
                                    &mut text,
                                    "{}",
                                    product
                                        .artifacts
                                        .regalloc
                                        .expect("regalloc projection was requested")
                                )
                                .expect("write to String");
                            }
                            PresentationStage::Asm => {
                                writeln!(&mut text, ".globl {}", product.machine_name)
                                    .expect("write to String");
                                writeln!(&mut text, "{}:", product.machine_name)
                                    .expect("write to String");
                                write!(
                                    &mut text,
                                    "{}",
                                    product
                                        .artifacts
                                        .asm
                                        .expect("assembly projection was requested")
                                )
                                .expect("write to String");
                            }
                            _ => unreachable!("backend request has a backend presentation stage"),
                        }
                    }
                } else {
                    for function in semantic.functions() {
                        match stage {
                            PresentationStage::Air => {
                                writeln!(&mut text, "function {}:", function.analyzed.name)
                                    .expect("write to String");
                                writeln!(
                                    &mut text,
                                    "{}",
                                    function.analyzed.air.display_with_interner(interner)
                                )
                                .expect("write to String");
                            }
                            PresentationStage::Cfg => {
                                writeln!(
                                    &mut text,
                                    "{}",
                                    function.cfg.display_with_interner(interner)
                                )
                                .expect("write to String");
                            }
                            PresentationStage::StackFrame => {
                                writeln!(
                                    &mut text,
                                    "{}",
                                    rue_codegen::generate_stack_frame_info(
                                        &function.cfg,
                                        &function.machine_name,
                                        semantic.type_pool(),
                                        interner,
                                        request.options.target,
                                    )?
                                )
                                .expect("write to String");
                            }
                            PresentationStage::Tokens
                            | PresentationStage::Ast
                            | PresentationStage::Rir
                            | PresentationStage::Lowering
                            | PresentationStage::Mir
                            | PresentationStage::Liveness
                            | PresentationStage::RegAlloc
                            | PresentationStage::Asm => unreachable!(),
                        }
                    }
                }
            }
        }
        Ok(PresentationOutput { text })
    }
}

#[cfg(test)]
mod codegen_unit_tests {
    use super::*;

    #[test]
    fn codegen_presentation_is_available_before_and_after_normal_codegen() {
        let snapshot = crate::SourceSnapshot::single("main.rue", "fn main() -> i32 { 7 }").unwrap();
        let options = crate::CompileOptions::default();
        let order = snapshot
            .files()
            .map(|file| file.file_id)
            .collect::<Vec<_>>();
        let mut first_emit = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut first_emit, &snapshot).unwrap();
        let emitted_first = first_emit
            .unstable_present(PresentationRequest {
                stage: PresentationStage::Asm,
                options: &options,
                file_order: &order,
            })
            .unwrap();
        let semantic = first_emit.canonical_semantic(&options).unwrap();
        first_emit
            .codegen_products(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let emitted_after = first_emit
            .unstable_present(PresentationRequest {
                stage: PresentationStage::Asm,
                options: &options,
                file_order: &order,
            })
            .unwrap();
        assert_eq!(emitted_first.text, emitted_after.text);

        let mut first_link = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut first_link, &snapshot).unwrap();
        let semantic = first_link.canonical_semantic(&options).unwrap();
        first_link
            .codegen_products(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        assert_eq!(
            emitted_first.text,
            first_link
                .unstable_present(PresentationRequest {
                    stage: PresentationStage::Asm,
                    options: &options,
                    file_order: &order
                })
                .unwrap()
                .text
        );
    }

    #[test]
    fn codegen_unit_failure_preserves_the_backend_diagnostic() {
        let snapshot = crate::SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
        let options = crate::CompileOptions::default();
        let mut session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();

        let errors = crate::codegen_query::with_test_codegen_failure_injection(|| {
            session
                .codegen_products(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap_err()
        });

        assert!(
            matches!(
                errors.first().map(|error| &error.kind),
                Some(crate::ErrorKind::InternalCodegenError(message))
                    if message
                        == "production machine-symbol resolver left source/glue call `injected_unresolved_codegen_symbol` unresolved"
            ),
            "codegen failure must retain its original diagnostic: {errors:?}"
        );
    }

    #[test]
    fn codegen_units_skip_unreached_bodies_and_keep_non_inlined_caller_bytes() {
        let first = crate::SourceSnapshot::single(
            "main.rue",
            "fn callee() -> i32 { 1 } fn dead() -> i32 { 99 } fn main() -> i32 { callee() }",
        )
        .unwrap();
        let second = crate::SourceSnapshot::single(
            "main.rue",
            "fn callee() -> i32 { 2 } fn dead() -> i32 { 99 } fn main() -> i32 { callee() }",
        )
        .unwrap();
        let options = crate::CompileOptions::default();
        let mut session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut session, &first).unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let before = session
            .codegen_products(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        assert_eq!(
            before.len(),
            2,
            "only reached callee and main publish units"
        );
        let caller = before
            .iter()
            .find(|product| product.machine_name == "main")
            .unwrap()
            .machine_code
            .code
            .clone();
        crate::publish_test_snapshot(&mut session, &second).unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let after = session
            .codegen_products(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let executions = session.codegen_executions();
        assert_eq!(
            executions.len(),
            2,
            "only main and reached callee publish units"
        );
        assert!(executions.iter().any(|(identity, execution)| matches!(identity, crate::FunctionInstanceKey::Definition(definition) if definition.name() == "callee") && *execution == rue_query::RequestExecution::Computed), "{executions:?}");
        assert!(executions.iter().any(|(identity, execution)| matches!(identity, crate::FunctionInstanceKey::Definition(definition) if definition.name() == "main") && *execution == rue_query::RequestExecution::Reused), "{executions:?}");
        assert_eq!(
            caller,
            after
                .iter()
                .find(|product| product.machine_name == "main")
                .unwrap()
                .machine_code
                .code
        );
    }

    #[test]
    fn codegen_units_cover_x86_and_aarch64_relocations_deterministically() {
        let snapshot = crate::SourceSnapshot::single(
            "main.rue",
            "fn callee() -> i32 { 1 } fn main() -> i32 { callee() }",
        )
        .unwrap();
        for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
            let options = crate::CompileOptions {
                target,
                ..crate::CompileOptions::default()
            };
            let mut session = crate::CompilerSession::new();
            crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
            let semantic = session.canonical_semantic(&options).unwrap();
            let first = session
                .codegen_products(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap();
            let second = session
                .codegen_products(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap();
            assert_eq!(
                first
                    .iter()
                    .map(|product| (
                        &product.machine_code.code,
                        &product.machine_code.relocations
                    ))
                    .collect::<Vec<_>>(),
                second
                    .iter()
                    .map(|product| (
                        &product.machine_code.code,
                        &product.machine_code.relocations
                    ))
                    .collect::<Vec<_>>()
            );
            let relocs = &first
                .iter()
                .find(|product| product.machine_name == "main")
                .unwrap()
                .machine_code
                .relocations;
            assert!(relocs.iter().any(|relocation| matches!(
                (target.arch(), relocation.kind),
                (
                    rue_target::Arch::X86_64,
                    rue_codegen::RelocationKind::X86Plt32
                ) | (
                    rue_target::Arch::Aarch64,
                    rue_codegen::RelocationKind::Aarch64Call26
                )
            )));
        }
    }

    #[test]
    fn codegen_units_are_identical_with_one_or_four_query_workers() {
        let snapshot = crate::SourceSnapshot::single(
            "main.rue",
            "fn f() -> i32 { 1 } fn main() -> i32 { f() }",
        )
        .unwrap();
        let options = crate::CompileOptions::default();
        let run = |workers| {
            let mut session = crate::CompilerSession::with_query_concurrency(workers);
            crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
            let semantic = session.canonical_semantic(&options).unwrap();
            let products = session
                .codegen_products(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest {
                        asm: true,
                        ..Default::default()
                    },
                )
                .unwrap();
            let units = products
                .iter()
                .map(|product| format!("{:?}", product))
                .collect::<Vec<_>>();
            let objects = crate::backend::generate_pre_link_objects_from_products(
                semantic.functions(),
                products,
                &options,
                &[],
            )
            .unwrap();
            (units, objects)
        };
        assert_eq!(run(1), run(4));
    }

    #[test]
    fn layout_successor_rebuilds_codegen_units() {
        let first = crate::SourceSnapshot::single("main.rue", "struct Inner { b: i32, a: i32 } struct Outer { inner: Inner } fn consume(value: Outer) -> i32 { value.inner.a } fn main() -> i32 { consume(Outer { inner: Inner { b: 1, a: 1 } }) }").unwrap();
        let second = crate::SourceSnapshot::single("main.rue", "struct Inner { b: i64, a: i32 } struct Outer { inner: Inner } fn consume(value: Outer) -> i32 { value.inner.a } fn main() -> i32 { consume(Outer { inner: Inner { b: 1, a: 1 } }) }").unwrap();
        let options = crate::CompileOptions::default();
        let mut session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut session, &first).unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let consume_name = semantic
            .functions()
            .iter()
            .find(|function| function.definition_source_name() == Some("consume"))
            .unwrap()
            .machine_name
            .clone();
        let before = session
            .codegen_products(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let consume_before = before
            .iter()
            .find(|product| product.machine_name == consume_name)
            .map(|product| {
                (
                    product.machine_code.code.clone(),
                    product.machine_code.relocations.clone(),
                )
            })
            .unwrap();
        crate::publish_test_snapshot(&mut session, &second).unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let after = session
            .codegen_products(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let consume_after = after
            .iter()
            .find(|product| product.machine_name == consume_name)
            .map(|product| {
                (
                    product.machine_code.code.clone(),
                    product.machine_code.relocations.clone(),
                )
            })
            .unwrap();
        assert_ne!(consume_before, consume_after);
        assert!(
            session
                .codegen_executions()
                .iter()
                .any(|(identity, execution)| matches!(identity, crate::FunctionInstanceKey::Definition(definition) if definition.name() == "consume") && *execution == rue_query::RequestExecution::Computed)
        );
    }
}

/// Query lifecycle counters contained in [`MetricsSnapshot`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryMetrics {
    pub calls: usize,
    pub executions: usize,
    pub reuses: usize,
}

impl From<crate::session::FrontendQueryWork> for QueryMetrics {
    fn from(work: crate::session::FrontendQueryWork) -> Self {
        Self {
            calls: work.calls,
            executions: work.executions,
            reuses: work.reuses,
        }
    }
}

/// Retention gauges contained in [`MetricsSnapshot`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionMetrics {
    pub diagnostic_entries: usize,
    pub diagnostic_source_attempts: usize,
    pub diagnostic_source_bytes: usize,
}

/// Merge counters used by the in-tree benchmark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeMetrics {
    pub definition_shards_indexed: usize,
    pub definition_shards_reused: usize,
    pub definition_shards_rebuilt: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseMetrics {
    pub lexer_invocations: usize,
    pub parser_invocations: usize,
    pub lexed_bytes: usize,
    pub tokens: usize,
    pub modules_considered: usize,
    pub modules_reused: usize,
    pub modules_rebound: usize,
    pub modules_reparsed: usize,
    pub source_text_clones: usize,
    pub source_bytes_rehashed: usize,
}

impl ParseMetrics {
    pub(crate) fn from_work(work: crate::ParsedModulesWork) -> Self {
        Self {
            lexer_invocations: work.syntax.lexer_invocations,
            parser_invocations: work.syntax.parser_invocations,
            lexed_bytes: work.syntax.lexed_bytes,
            tokens: work.syntax.tokens,
            modules_considered: work.modules_considered,
            modules_reused: work.modules_reused,
            modules_rebound: work.modules_rebound,
            modules_reparsed: work.modules_reparsed,
            source_text_clones: work.source_text_clones,
            source_bytes_rehashed: work.source_bytes_rehashed,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LowerMetrics {
    pub parser_invocations: usize,
    pub ast_payload_clones: usize,
}

impl LowerMetrics {
    pub(crate) fn from_work(work: crate::CanonicalRirWork) -> Self {
        Self {
            parser_invocations: work.parser_invocations,
            ast_payload_clones: work.ast_payload_clones,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticBindingMetrics {
    pub bind_invocations: usize,
    pub declarations_inspected: usize,
    pub modules_registered: usize,
    pub rir_indexes_constructed: usize,
    pub rir_instructions_visited: usize,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticManifestMetrics {
    pub build_invocations: usize,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticCfgMetrics {
    pub cfg_builds_attempted: usize,
    pub cfg_builds_succeeded: usize,
    pub cfg_builds_failed: usize,
    pub cfg_import_attempts: usize,
    pub cfg_import_successes: usize,
    pub cfg_import_failures: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticBodyMetrics {
    pub analyses_computed: usize,
    pub analyses_reused: usize,
    pub analyses_invalidated: usize,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticMetrics {
    pub binding: SemanticBindingMetrics,
    pub manifest: SemanticManifestMetrics,
    pub body: SemanticBodyMetrics,
    pub cfg: SemanticCfgMetrics,
}

impl SemanticMetrics {
    pub(crate) fn from_work(work: crate::CanonicalSemanticWork) -> Self {
        Self {
            binding: SemanticBindingMetrics {
                bind_invocations: work.binding.bind_invocations,
                declarations_inspected: work.binding.indexed_declaration_records_visited,
                modules_registered: work.binding.modules_registered,
                rir_indexes_constructed: work.declaration_index.build_invocations,
                rir_instructions_visited: work.declaration_index.rir_instructions_visited,
            },
            manifest: SemanticManifestMetrics {
                build_invocations: work.manifest.build_invocations,
            },
            body: SemanticBodyMetrics {
                analyses_computed: work.body_analysis.body_analyses_computed,
                analyses_reused: work.body_analysis.body_analyses_reused,
                analyses_invalidated: work.body_analysis.body_analyses_invalidated,
            },
            cfg: SemanticCfgMetrics {
                cfg_builds_attempted: work.cfg.cfg_builds_attempted,
                cfg_builds_succeeded: work.cfg.cfg_builds_succeeded,
                cfg_builds_failed: work.cfg.cfg_builds_failed,
                cfg_import_attempts: work.cfg.cfg_import_attempts,
                cfg_import_successes: work.cfg.cfg_import_successes,
                cfg_import_failures: work.cfg.cfg_import_failures,
            },
        }
    }
}

/// Metrics captured by the one-shot compilation adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OneShotMetrics {
    pub files: usize,
    pub bytes: usize,
    pub lines: usize,
    pub tokens: usize,
    pub parsed: ParseMetrics,
    pub lowered: LowerMetrics,
    pub semantic: SemanticMetrics,
}

impl OneShotMetrics {
    pub(crate) fn new(stats: crate::SourceStats, work: crate::PipelineWork) -> Self {
        Self {
            files: stats.files,
            bytes: stats.bytes,
            lines: stats.lines,
            tokens: stats.tokens,
            parsed: ParseMetrics::from_work(work.parsed),
            lowered: LowerMetrics::from_work(work.lowered),
            semantic: SemanticMetrics::from_work(work.semantic),
        }
    }
}

/// Owned compiler metrics snapshot with query records and inputs projected out.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    inner: crate::session::CompilerSessionWork,
}

impl MetricsSnapshot {
    pub(crate) fn new(inner: crate::session::CompilerSessionWork) -> Self {
        Self { inner }
    }
    pub fn updates(&self) -> usize {
        self.inner.updates
    }
    pub fn merge(&self) -> QueryMetrics {
        self.inner.merge.into()
    }
    pub fn rir(&self) -> QueryMetrics {
        self.inner.rir.into()
    }
    pub fn semantic(&self) -> QueryMetrics {
        self.inner.semantic.into()
    }
    pub fn definitions(&self) -> QueryMetrics {
        self.inner.definitions.into()
    }
    pub fn downstream_invalidations(&self) -> usize {
        self.inner.downstream_invalidations
    }
    pub fn semantic_entries_invalidated(&self) -> usize {
        self.inner.semantic_entries_invalidated
    }
    pub fn definition_entries_invalidated(&self) -> usize {
        self.inner.definition_entries_invalidated
    }
    pub fn declaration_reuse_plans(&self) -> usize {
        self.inner.declaration_reuse_plans
    }
    pub fn durable_records_compared(&self) -> usize {
        self.inner.durable_records_compared
    }
    pub fn durable_records_reused(&self) -> usize {
        self.inner.durable_records_reused
    }
    pub fn ordinary_declaration_resolutions_skipped(&self) -> usize {
        self.inner.ordinary_declaration_resolutions_skipped
    }
    pub fn durable_installs(&self) -> usize {
        self.inner.durable_installs
    }
    pub fn declaration_reuse_fallbacks(&self) -> usize {
        self.inner.declaration_reuse_fallbacks
    }
    pub fn semantic_record_count(&self) -> usize {
        self.inner.semantic_records.len()
    }
    pub fn definition_record_count(&self) -> usize {
        self.inner.definition_records.len()
    }
    pub fn merge_metrics(&self) -> MergeMetrics {
        MergeMetrics {
            definition_shards_indexed: self.inner.last_merge.definition_shards_indexed,
            definition_shards_reused: self.inner.last_merge.definition_shards_reused,
            definition_shards_rebuilt: self.inner.last_merge.definition_shards_rebuilt,
        }
    }
    pub fn parse_metrics(&self) -> ParseMetrics {
        ParseMetrics::from_work(self.inner.last_parse)
    }
    pub fn lower_metrics(&self) -> LowerMetrics {
        LowerMetrics {
            parser_invocations: self.inner.last_rir.parser_invocations,
            ast_payload_clones: self.inner.last_rir.ast_payload_clones,
        }
    }
    pub fn retention(&self) -> RetentionMetrics {
        RetentionMetrics {
            diagnostic_entries: self.inner.retention.diagnostic_entries,
            diagnostic_source_attempts: self.inner.retention.diagnostic_source_attempts,
            diagnostic_source_bytes: self.inner.retention.diagnostic_source_bytes,
        }
    }
    pub fn semantic_work_json(&self, from: usize) -> Value {
        semantic_work_json(&self.inner, from)
    }
    pub fn definition_work_json(&self, from: usize) -> Value {
        definition_work_json(&self.inner, from)
    }
}

fn semantic_work_json(work: &crate::session::CompilerSessionWork, from: usize) -> Value {
    let records = &work.semantic_records[from..];
    json!({
        "schema_version": 1,
        "failed_requests": records.iter().filter(|record| record.failure.is_some()).count(),
        "failure_phases": {
            "declaration": records.iter().filter(|record| matches!(record.failure.map(|failure| failure.phase), Some(SemanticFailurePhase::Declaration))).count(),
            "body_analysis": records.iter().filter(|record| matches!(record.failure.map(|failure| failure.phase), Some(SemanticFailurePhase::BodyAnalysis))).count(),
            "cfg_construction": records.iter().filter(|record| matches!(record.failure.map(|failure| failure.phase), Some(SemanticFailurePhase::CfgConstruction))).count(),
        },
        "bind_invocations": records.iter().map(|record| record.work.binding.bind_invocations).sum::<usize>(),
        "declaration_resolution_invocations": records.iter().map(|record| record.work.binding.declaration_resolution_invocations).sum::<usize>(),
        "declaration_resolution_failures": records.iter().map(|record| record.work.binding.declaration_resolution_failures).sum::<usize>(),
        "body_readiness_finalization_invocations": records.iter().map(|record| record.work.binding.body_readiness_finalization_invocations).sum::<usize>(),
        "declarations_inspected": records.iter().map(|record| record.work.binding.indexed_declaration_records_visited).sum::<usize>(),
        "modules_registered": records.iter().map(|record| record.work.binding.modules_registered).sum::<usize>(),
        "rir_indexes_constructed": records.iter().map(|record| record.work.declaration_index.build_invocations).sum::<usize>(),
        "rir_instructions_visited": records.iter().map(|record| record.work.declaration_index.rir_instructions_visited).sum::<usize>(),
        "body_free_function_lookups": records.iter().map(|record| record.work.body_analysis.free_function_record_lookups).sum::<usize>(),
        "body_analyses_computed": records.iter().map(|record| record.work.body_analysis.body_analyses_computed).sum::<usize>(),
        "body_analyses_reused": records.iter().map(|record| record.work.body_analysis.body_analyses_reused).sum::<usize>(),
        "body_analyses_invalidated": records.iter().map(|record| record.work.body_analysis.body_analyses_invalidated).sum::<usize>(),
        "bodies_attempted": records.iter().map(|record| record.work.body_analysis.bodies_attempted).sum::<usize>(),
        "bodies_succeeded": records.iter().map(|record| record.work.body_analysis.bodies_succeeded).sum::<usize>(),
        "bodies_failed": records.iter().map(|record| record.work.body_analysis.bodies_failed).sum::<usize>(),
        "air_instructions_produced": records.iter().map(|record| record.work.body_analysis.air_instructions_produced).sum::<usize>(),
        "body_dependency_air_instructions_observed": records.iter().map(|record| record.work.body_analysis.body_dependency_air_instructions_observed).sum::<usize>(),
        "local_strings_produced": records.iter().map(|record| record.work.body_analysis.local_strings_produced).sum::<usize>(),
        "string_ids_remapped": records.iter().map(|record| record.work.body_analysis.string_ids_remapped).sum::<usize>(),
        "specialization_air_instructions_scanned": records.iter().map(|record| record.work.body_analysis.specialization_air_instructions_scanned).sum::<usize>(),
        "generic_calls_observed": records.iter().map(|record| record.work.body_analysis.generic_calls_observed).sum::<usize>(),
        "specialization_requests_unique": records.iter().map(|record| record.work.body_analysis.specialization_requests_unique).sum::<usize>(),
        "specialization_requests_duplicate": records.iter().map(|record| record.work.body_analysis.specialization_requests_duplicate).sum::<usize>(),
        "specialization_rewrites": records.iter().map(|record| record.work.body_analysis.specialization_rewrites).sum::<usize>(),
        "specialization_rounds": records.iter().map(|record| record.work.body_analysis.specialization_rounds).sum::<usize>(),
        "specialization_driver_failures": records.iter().map(|record| record.work.body_analysis.specialization_driver_failures).sum::<usize>(),
        "specialized_bodies_attempted": records.iter().map(|record| record.work.body_analysis.specialized_bodies_attempted).sum::<usize>(),
        "specialized_bodies_succeeded": records.iter().map(|record| record.work.body_analysis.specialized_bodies_succeeded).sum::<usize>(),
        "specialized_bodies_failed": records.iter().map(|record| record.work.body_analysis.specialized_bodies_failed).sum::<usize>(),
        "durable_bodies": {
            "candidate_fallbacks": records.iter().map(|record| record.work.durable_bodies.candidate_fallbacks).sum::<usize>(),
            "export_attempts": records.iter().map(|record| record.work.durable_bodies.export_attempts).sum::<usize>(),
            "export_successes": records.iter().map(|record| record.work.durable_bodies.export_successes).sum::<usize>(),
            "export_rejections": records.iter().map(|record| record.work.durable_bodies.export_rejections).sum::<usize>(),
            "instructions_exported": records.iter().map(|record| record.work.durable_bodies.instructions_exported).sum::<usize>(),
            "places_exported": records.iter().map(|record| record.work.durable_bodies.places_exported).sum::<usize>(),
            "strings_exported": records.iter().map(|record| record.work.durable_bodies.strings_exported).sum::<usize>(),
            "import_attempts": records.iter().map(|record| record.work.durable_bodies.import_attempts).sum::<usize>(),
            "import_successes": records.iter().map(|record| record.work.durable_bodies.import_successes).sum::<usize>(),
            "import_failures": records.iter().map(|record| record.work.durable_bodies.import_failures).sum::<usize>(),
            "installed_instructions": records.iter().map(|record| record.work.durable_bodies.installed_instructions).sum::<usize>(),
            "installed_places": records.iter().map(|record| record.work.durable_bodies.installed_places).sum::<usize>(),
            "installed_strings": records.iter().map(|record| record.work.durable_bodies.installed_strings).sum::<usize>(),
            "atomic_discards": records.iter().map(|record| record.work.durable_bodies.atomic_discards).sum::<usize>(),
            "reused_bodies": records.iter().map(|record| record.work.durable_bodies.reused_bodies).sum::<usize>(),
            "skipped_body_analyses": records.iter().map(|record| record.work.durable_bodies.skipped_body_analyses).sum::<usize>(),
        },
        "cfg": {
            "drop_glue_functions_synthesized": records.iter().map(|record| record.work.cfg.drop_glue_functions_synthesized).sum::<usize>(),
            "functions_considered": records.iter().map(|record| record.work.cfg.functions_considered).sum::<usize>(),
            "comptime_functions_filtered": records.iter().map(|record| record.work.cfg.comptime_functions_filtered).sum::<usize>(),
            "builds_attempted": records.iter().map(|record| record.work.cfg.cfg_builds_attempted).sum::<usize>(),
            "builds_succeeded": records.iter().map(|record| record.work.cfg.cfg_builds_succeeded).sum::<usize>(),
            "builds_failed": records.iter().map(|record| record.work.cfg.cfg_builds_failed).sum::<usize>(),
            "import_attempts": records.iter().map(|record| record.work.cfg.cfg_import_attempts).sum::<usize>(),
            "import_successes": records.iter().map(|record| record.work.cfg.cfg_import_successes).sum::<usize>(),
            "import_failures": records.iter().map(|record| record.work.cfg.cfg_import_failures).sum::<usize>(),
            "air_instructions_consumed": records.iter().map(|record| record.work.cfg.air_instructions_consumed).sum::<usize>(),
            "optimization_attempts": records.iter().map(|record| record.work.cfg.optimization_attempts).sum::<usize>(),
            "optimization_completions": records.iter().map(|record| record.work.cfg.optimization_completions).sum::<usize>(),
            "optimized_level_attempts": records.iter().map(|record| record.work.cfg.optimized_level_attempts).sum::<usize>(),
            "warnings_emitted": records.iter().map(|record| record.work.cfg.cfg_warnings_emitted).sum::<usize>(),
            "implicit_destructor_targets_emitted": records.iter().map(|record| record.work.cfg.implicit_destructor_targets_emitted).sum::<usize>(),
            "reuse_candidates": records.iter().map(|record| record.work.cfg.cfg_reuse_candidates).sum::<usize>(),
            "reuses": records.iter().map(|record| record.work.cfg.cfg_reuses).sum::<usize>(),
            "fallbacks": records.iter().map(|record| record.work.cfg.cfg_fallbacks).sum::<usize>(),
            "warnings_reused": records.iter().map(|record| record.work.cfg.cfg_warnings_reused).sum::<usize>(),
            "implicit_destructor_targets_reused": records.iter().map(|record| record.work.cfg.implicit_destructor_targets_reused).sum::<usize>(),
            "export_attempts": records.iter().map(|record| record.work.cfg.cfg_export_attempts).sum::<usize>(),
            "export_successes": records.iter().map(|record| record.work.cfg.cfg_export_successes).sum::<usize>(),
            "export_rejections": records.iter().map(|record| record.work.cfg.cfg_export_rejections).sum::<usize>(),
        },
        "ordinary_free_function_dependency_events": records.iter().map(|record| record.work.body_analysis.ordinary_free_function_dependency_events).sum::<usize>(),
        "specialized_origin_records": records.iter().map(|record| record.work.body_analysis.specialized_origin_records).sum::<usize>(),
        "specialized_free_function_dependency_events": records.iter().map(|record| record.work.body_analysis.specialized_free_function_dependency_events).sum::<usize>(),
        "named_method_dependency_events": records.iter().map(|record| record.work.body_analysis.named_method_dependency_events).sum::<usize>(),
        "named_destructor_dependency_events": records.iter().map(|record| record.work.body_analysis.named_destructor_dependency_events).sum::<usize>(),
        "declaration_type_dependency_events": records.iter().map(|record| record.work.body_analysis.declaration_type_dependency_events).sum::<usize>(),
        "declaration_type_call_head_dependency_events": records.iter().map(|record| record.work.body_analysis.declaration_type_call_head_dependency_events).sum::<usize>(),
        "named_const_dependency_events": records.iter().map(|record| record.work.body_analysis.named_const_dependency_events).sum::<usize>(),
        "manifest_build_invocations": records.iter().map(|record| record.work.manifest.build_invocations).sum::<usize>(),
        "body_owner_tokens": {
            "provisional_slots": records.iter().map(|record| record.work.body_owner_tokens.provisional_slots).sum::<usize>(),
            "authoritative_slots": records.iter().map(|record| record.work.body_owner_tokens.authoritative_slots).sum::<usize>(),
            "slots_validated": records.iter().map(|record| record.work.body_owner_tokens.slots_validated).sum::<usize>(),
            "tokens_installed": records.iter().map(|record| record.work.body_owner_tokens.tokens_installed).sum::<usize>(),
            "validation_failures": records.iter().map(|record| record.work.body_owner_tokens.validation_failures).sum::<usize>(),
        },
        "declaration_reuse": {
            "plan_executions": records.iter().map(|record| record.work.declaration_reuse.plan_executions).sum::<usize>(),
            "durable_records_compared": records.iter().map(|record| record.work.declaration_reuse.durable_records_compared).sum::<usize>(),
            "durable_records_reused": records.iter().map(|record| record.work.declaration_reuse.durable_records_reused).sum::<usize>(),
            "ordinary_declaration_resolutions_skipped": records.iter().map(|record| record.work.declaration_reuse.ordinary_declaration_resolutions_skipped).sum::<usize>(),
            "install_invocations": records.iter().map(|record| record.work.declaration_reuse.install_invocations).sum::<usize>(),
            "fallbacks": records.iter().map(|record| record.work.declaration_reuse.fallbacks).sum::<usize>(),
            "declaration_prefixes_built": records.iter().map(|record| record.work.declaration_reuse.declaration_prefixes_built).sum::<usize>(),
            "declaration_indexes_built": records.iter().map(|record| record.work.declaration_reuse.declaration_indexes_built).sum::<usize>(),
            "declaration_prefix_population_runs": records.iter().map(|record| record.work.declaration_reuse.declaration_prefix_population_runs).sum::<usize>(),
            "durable_cache_population_exports": records.iter().map(|record| record.work.declaration_reuse.durable_cache_population_exports).sum::<usize>(),
            "declaration_prefix_fallbacks": records.iter().map(|record| record.work.declaration_reuse.declaration_prefix_fallbacks).sum::<usize>(),
        },
        "declaration_prefixes_built": records.iter().map(|record| record.work.declaration_reuse.declaration_prefixes_built).sum::<usize>(),
        "declaration_indexes_built": records.iter().map(|record| record.work.declaration_reuse.declaration_indexes_built).sum::<usize>(),
        "declaration_prefix_population_runs": records.iter().map(|record| record.work.declaration_reuse.declaration_prefix_population_runs).sum::<usize>(),
        "durable_cache_population_exports": records.iter().map(|record| record.work.declaration_reuse.durable_cache_population_exports).sum::<usize>(),
        "declaration_prefix_fallbacks": records.iter().map(|record| record.work.declaration_reuse.declaration_prefix_fallbacks).sum::<usize>(),
    })
}

fn definition_work_json(work: &crate::session::CompilerSessionWork, from: usize) -> Value {
    let records = &work.definition_records[from..];
    json!({
        "bind_invocations": records.iter().map(|record| record.binding.bind_invocations).sum::<usize>(),
        "manifest_build_invocations": records.iter().map(|record| record.manifest.build_invocations).sum::<usize>(),
        "manifest_bindings_visited": records.iter().map(|record| record.issuance.manifest_bindings_visited).sum::<usize>(),
        "ids_issued": records.iter().map(|record| record.issuance.ids_issued).sum::<usize>(),
    })
}

/// Deliberate fault selection for the differential incremental oracle.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DifferentialOracleFault {
    Semantic,
    Diagnostic,
    Import,
}
