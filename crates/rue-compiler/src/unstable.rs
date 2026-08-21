//! Explicitly unstable compiler instrumentation and test-support views.
//!
//! Nothing in this module is covered by the supported facade's compatibility
//! policy. These owned snapshots and opaque session products cannot be
//! installed into a session or used as query keys.

use std::fmt::Write as _;
use std::sync::Arc;

pub use crate::diagnostic::{
    ColorChoice, DiagnosticFormatter, JsonDiagnostic, JsonDiagnosticFormatter, JsonSpan,
    JsonSuggestion, MultiFileFormatter, MultiFileJsonFormatter, SourceInfo,
};
pub use crate::import_discovery::{
    AcceptedImportSource, DiscoverySourceAssembler, ImportDemandFrontier, ImportDemandMode,
    ImportDemandRoots, ImportDiscoveryPlan, ImportDiscoveryRequest, ImportDiscoveryWave,
    ImportInputRevision, ImportObservation, ImportObservationLedger, ImportObservationStatus,
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

/// The demand roots for one ordinary discovery round continuing `previous`: the
/// occurrences the plan gained in this round's stage, plus the occurrences
/// `previous` demanded host answers for. Rooting a round in this union emits the
/// same requests, in the same canonical order, as rooting it in the whole plan —
/// every occurrence outside it is already conclusive for the generation. The
/// host supplies no membership of its own: both halves come from the
/// compiler-owned plan and frontier values.
pub fn plan_round_roots(
    plan: &crate::ImportDiscoveryPlan,
    previous: &ImportDemandFrontier,
) -> ImportDemandRoots {
    plan.round_roots(previous)
}

/// Open a discovery wave over one round's starting frontier (ADR-0075).
///
/// The wave is the unit of publication: it resolves the transitive import
/// closure reachable from that frontier hop by hop, emitting each hop's host
/// operations in the exact order the round it replaces would have emitted them,
/// and is published once by [`publish_import_wave`].
pub fn begin_import_wave(
    session: &mut crate::CompilerSession,
    revision: ImportInputRevision,
    plan: &crate::ImportDiscoveryPlan,
    frontier: &ImportDemandFrontier,
) -> crate::CompileResult<ImportDiscoveryWave> {
    session.begin_import_wave(revision, plan, frontier)
}

/// Record one wave hop's answers and derive the next hop's operations. The host
/// supplies only results for the exact batch [`ImportDiscoveryWave::requests`]
/// named, in that order.
pub fn extend_import_wave(
    session: &mut crate::CompilerSession,
    wave: &mut ImportDiscoveryWave,
    observations: Vec<crate::ImportObservation>,
) -> crate::CompileResult<()> {
    session.extend_import_wave(wave, observations)
}

/// Publish one whole wave as one successor immutable revision, returning that
/// revision and the batch frontier the next round continues from.
pub fn publish_import_wave(
    session: &mut crate::CompilerSession,
    wave: ImportDiscoveryWave,
    snapshot: &crate::SourceSnapshot,
    accepted_reads: crate::AcceptedReadManifest,
) -> crate::CompileResult<(ImportInputRevision, ImportDemandFrontier)> {
    session.publish_import_wave(wave, snapshot, accepted_reads)
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

/// Cumulative red/green validation certificate misses (ADR-0073). Fresh-build
/// discovery preserves certificates across append-only frontier rounds, so a
/// deep import chain keeps this linear in module count; growth toward
/// rounds-times-graph is a structural regression, gated where discovery-shape
/// tests already pin per-round linearity.
pub fn validation_certificate_misses(session: &crate::CompilerSession) -> u64 {
    session.validation_certificate_misses()
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

/// Module-identity resolutions performed against published source snapshots
/// ("which file is this module bound to?"), asked once per module by
/// the parse projection and once more per module it rebinds.
///
/// This counter and [`snapshot_module_resolution_visits`] exist as a pair. The
/// question count is linear in the program by construction; the *visit* count
/// is what distinguishes an index from a scan, and only the ratio between them
/// is a scaling property. A scan answered each question by walking the whole
/// snapshot, so the visits grew as modules squared while nothing this compiler
/// dispatched — parses, plan groups, frontier roots, certificate misses —
/// changed at all.
pub fn snapshot_module_resolutions(session: &crate::CompilerSession) -> u64 {
    session.snapshot_module_resolutions()
}

/// Snapshot positions examined answering [`snapshot_module_resolutions`]: one
/// probe per snapshot segment, plus the one record the answer reads. Segment
/// count is bounded, so this stays linear in the program; growth toward
/// resolutions-times-modules is the scan regression.
pub fn snapshot_module_resolution_visits(session: &crate::CompilerSession) -> u64 {
    session.snapshot_module_resolution_visits()
}

/// Physical-identity lookups performed against accepted-read manifests
/// ("which manifest entry names this physical file?"), asked once per
/// accepted import observation while authorizing a discovery batch. Paired with
/// [`accepted_read_identity_visits`] exactly as the module pair above.
pub fn accepted_read_identity_lookups(session: &crate::CompilerSession) -> u64 {
    session.accepted_read_identity_lookups()
}

/// Accepted-read manifest entries examined answering
/// [`accepted_read_identity_lookups`]. A manifest materializes its
/// inverse physical-identity index once and shares it with every clone, so the
/// whole build pays one pass per manifest value plus one entry per lookup;
/// answering by scan instead cost one pass per lookup.
pub fn accepted_read_identity_visits(session: &crate::CompilerSession) -> u64 {
    session.accepted_read_identity_visits()
}

/// Attempt-handoff lifecycles offered to a task's observation scope ("has this
/// scope already recorded this lifecycle?"), asked once per handoff a task
/// observes or inherits from a structured child. Paired with
/// [`handoff_observation_visits`] exactly as the two pairs above.
pub fn handoff_observations(session: &crate::CompilerSession) -> u64 {
    session.handoff_observations()
}

/// Observation-scope positions examined answering [`handoff_observations`]. An
/// already committed lifecycle carries no obligation and is answered without
/// being recorded, so a scope holds only what is still live and the ratio
/// between these two counters sits at one; a scope that begins accumulating
/// live lifecycles turns each observation into a walk over the ones before it,
/// and that ratio is the only thing which moves.
pub fn handoff_observation_visits(session: &crate::CompilerSession) -> u64 {
    session.handoff_observation_visits()
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
    /// Materialization requests routed through a provider adapter which shares
    /// canonical immutable payload storage.
    pub shared_payload_materializations: u64,
    /// Materialization requests routed through a provider adapter which
    /// rebuilds an owned durable payload.
    pub owned_payload_materializations: u64,
    /// Constant facts materialized into a body-local overlay.
    pub const_materializations: u64,
    /// Named nominal facts materialized into a body-local overlay.
    pub nominal_materializations: u64,
    /// Free-function facts materialized into a body-local overlay.
    pub function_materializations: u64,
    /// Method facts materialized into a body-local overlay.
    pub method_materializations: u64,
    /// Named nominal materialization requests satisfied by the body
    /// transaction's exact durable-payload cache.
    pub nominal_materialization_reuses: u64,
    /// Free-function materialization requests satisfied by the body
    /// transaction's exact durable-payload cache.
    pub function_materialization_reuses: u64,
    /// Anonymous-nominal fact observations.
    pub anonymous_facts: u64,
    /// Producer-body fact observations.
    pub producer_facts: u64,
    /// Trusted-toolchain fact observations.
    pub toolchain_facts: u64,
    /// Top-level imported-type nominal registration requests.
    pub import_nominal_registration_requests: u64,
    /// Durable type nodes visited by imported-type nominal registration.
    pub import_nominal_type_visits: u64,
    /// Named nominal nodes probed against the body-local registration cache.
    pub import_named_nominal_probes: u64,
    /// Named nominal probes satisfied by complete body-local closures.
    pub import_named_nominal_complete_hits: u64,
    /// Recursive named nominal probes stopped by an in-progress cycle marker.
    pub import_named_nominal_cycle_hits: u64,
    /// Named nominal closures installed completely in body-local identity domains.
    pub import_named_nominals_registered: u64,
    /// Container-element and nominal-field edges traversed while installing closures.
    pub import_nominal_type_edges_traversed: u64,
    /// Anonymous nominal identities installed through imported durable types.
    pub import_anonymous_nominals_registered: u64,
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

/// Storage-tier witnesses for the committed import discovery's three additive
/// artifacts (RUE-1112): `[graph_records, plan_groups, resolution_modules]`,
/// each `(lineage_root_segment_address, exact_delta_len)`. The address is a
/// stable ancestry witness even when size-tiered storage compacts.
pub fn committed_successor_sharing(
    session: &crate::CompilerSession,
) -> Option<[(usize, usize); 3]> {
    session.committed_successor_sharing()
}

pub use crate::session::{
    ClosedDiscoveryContinuation, RootedCfgOutput, RootedCfgUnit, RootedParkOutcome,
    TrustedSuccessorDelta,
};

/// Run the production body-closure root without constructing a presentation
/// artifact.
pub fn rooted_or_toolchain_park(
    session: &mut crate::CompilerSession,
    options: &crate::CompileOptions,
) -> RootedParkOutcome {
    session.rooted_or_toolchain_park(options)
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
    warnings: Vec<crate::CompileWarning>,
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
/// oracle. Stable callers use `compile_snapshot`.
pub fn oracle_executable(
    session: &mut crate::CompilerSession,
    snapshot: &crate::SourceSnapshot,
    options: &crate::CompileOptions,
) -> crate::MultiErrorResult<crate::CompileOutput> {
    session.oracle_executable(snapshot, options)
}

/// Produce an executable inside a compile span owned by the filesystem
/// driver. Stable callers use `compile_snapshot`, which owns its tracing
/// root.
pub fn executable_in_compile_scope(
    session: &mut crate::CompilerSession,
    options: &crate::CompileOptions,
) -> crate::MultiErrorResult<crate::CompileOutput> {
    session.executable_in_compile_scope(options)
}

/// Cloneable cancellation authority for one retained-host compile cycle.
///
/// This deliberately hides query-runtime identities and keys. Canceling a
/// request prevents its unpublished backend root and linked bytes from being
/// selected for publication; already retained terminals remain reusable.
#[derive(Clone, Default)]
pub struct CompilationCancellation {
    token: rue_query::CancellationToken,
}

impl CompilationCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_canceled(&self) -> bool {
        self.token.is_canceled()
    }
}

/// Host-facing outcome of a cancellable retained compilation.
pub enum CancellableCompileOutcome {
    Completed(Box<crate::CompileOutput>),
    Errors(crate::CompileErrors),
    Canceled,
}

/// Compile the current closed discovery revision with cooperative
/// cancellation, inside a tracing span owned by the filesystem driver.
pub fn cancellable_executable_in_compile_scope(
    session: &mut crate::CompilerSession,
    options: &crate::CompileOptions,
    cancellation: CompilationCancellation,
) -> CancellableCompileOutcome {
    let snapshot = match session.committed_snapshot_for_executable() {
        Ok(snapshot) => snapshot,
        Err(errors) => return CancellableCompileOutcome::Errors(errors),
    };
    match crate::queries::compile_with_session_with_cancellation(
        session,
        &snapshot,
        options,
        cancellation.token,
    ) {
        Ok(output) => CancellableCompileOutcome::Completed(Box::new(output)),
        Err(crate::session::PipelineRequestControl::Compile(errors)) => {
            CancellableCompileOutcome::Errors(errors)
        }
        Err(crate::session::PipelineRequestControl::Abort(rue_query::QueryAbort::Canceled)) => {
            CancellableCompileOutcome::Canceled
        }
        Err(crate::session::PipelineRequestControl::Abort(abort)) => {
            CancellableCompileOutcome::Errors(crate::CompileErrors::from(
                crate::CompileError::without_span(crate::ErrorKind::InternalError(format!(
                    "cancellable compile query aborted: {abort:?}"
                ))),
            ))
        }
        Err(crate::session::PipelineRequestControl::Parked(park)) => {
            CancellableCompileOutcome::Errors(crate::session::unresolved_toolchain_park_errors(
                &park,
            ))
        }
    }
}

/// Opaque continuation at ADR-0068's codegen-ready endpoint.
///
/// It owns the unpublished backend-root protection acquired with the rooted
/// CodegenUnit collection. Callers can only advance it through
/// [`objects_ready`]; compiler query keys and mutable IR stay private.
pub struct CodegenReady {
    rooted: crate::session::RootedCodegenReadyOutput,
    snapshot: crate::SourceSnapshot,
    options: crate::CompileOptions,
    owner: std::sync::Arc<()>,
    generation: usize,
}

/// Opaque continuation at ADR-0068's objects-ready endpoint.
pub struct ObjectsReady {
    rooted: crate::session::RootedCodegenOutput,
    snapshot: crate::SourceSnapshot,
    options: crate::CompileOptions,
    owner: std::sync::Arc<()>,
    generation: usize,
}

/// Aggregate lifecycle work for one canonical per-function query collection.
///
/// This is owned measurement data only. It contains no query keys, artifacts,
/// or handles that can be installed back into a compiler session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EndpointQueryWork {
    pub computed: usize,
    pub reused: usize,
    pub invalidated: usize,
    pub joined: usize,
    pub canceled: usize,
}

impl From<crate::session::BackendQueryWork> for EndpointQueryWork {
    fn from(work: crate::session::BackendQueryWork) -> Self {
        Self {
            computed: work.computed,
            reused: work.reused,
            invalidated: 0,
            joined: work.joined,
            canceled: work.canceled,
        }
    }
}

impl EndpointQueryWork {
    fn from_semantic_work(work: crate::CanonicalSemanticWork) -> Self {
        Self {
            computed: work.body_analysis.body_analyses_computed,
            reused: work.body_analysis.body_analyses_reused,
            invalidated: work.body_analysis.body_analyses_invalidated,
            joined: 0,
            canceled: 0,
        }
    }
}

/// Structural work available at the retained compilation endpoints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EndpointWork {
    pub semantic: EndpointQueryWork,
    pub cfg: EndpointQueryWork,
    pub codegen: EndpointQueryWork,
    pub object_projection: EndpointQueryWork,
}

impl CodegenReady {
    pub fn unstable_work(&self) -> EndpointWork {
        EndpointWork {
            semantic: EndpointQueryWork::from_semantic_work(self.rooted.work),
            cfg: self.rooted.cfg_work.into(),
            codegen: self.rooted.codegen_work.into(),
            object_projection: EndpointQueryWork::default(),
        }
    }
}

impl ObjectsReady {
    pub fn unstable_work(&self) -> EndpointWork {
        EndpointWork {
            semantic: EndpointQueryWork::from_semantic_work(self.rooted.work),
            cfg: self.rooted.cfg_work.into(),
            codegen: self.rooted.codegen_work.into(),
            object_projection: self.rooted.object_projection_work.into(),
        }
    }
}

fn validate_endpoint_capability(
    session: &crate::CompilerSession,
    owner: &std::sync::Arc<()>,
    generation: usize,
) -> crate::MultiErrorResult<()> {
    if !std::sync::Arc::ptr_eq(owner, &session.endpoint_capability_owner()) {
        return Err(crate::CompileErrors::from(
            crate::CompileError::without_span(crate::ErrorKind::InvalidCompilerInput(
                "retained endpoint capability belongs to another compiler session".into(),
            )),
        ));
    }
    if generation != session.endpoint_capability_generation() {
        return Err(crate::CompileErrors::from(
            crate::CompileError::without_span(crate::ErrorKind::InvalidCompilerInput(
                "retained endpoint capability is stale after a newer source revision".into(),
            )),
        ));
    }
    Ok(())
}

/// Drive a closed retained session through rooted CodegenUnit collection only.
/// Object projection and linking do not run before this function returns.
pub fn codegen_ready(
    session: &mut crate::CompilerSession,
    options: &crate::CompileOptions,
) -> crate::MultiErrorResult<CodegenReady> {
    let snapshot = session.committed_snapshot_for_executable()?;
    let rooted =
        session.rooted_codegen_ready(options, rue_codegen::BackendArtifactRequest::default())?;
    Ok(CodegenReady {
        rooted,
        snapshot,
        options: options.clone(),
        owner: session.endpoint_capability_owner(),
        generation: session.endpoint_capability_generation(),
    })
}

/// Consume a compiler-issued codegen-ready continuation and collect its exact
/// retained object projections.
pub fn objects_ready(
    session: &mut crate::CompilerSession,
    ready: CodegenReady,
) -> crate::MultiErrorResult<ObjectsReady> {
    validate_endpoint_capability(session, &ready.owner, ready.generation)?;
    let rooted = session.rooted_objects_ready(ready.rooted)?;
    Ok(ObjectsReady {
        rooted,
        snapshot: ready.snapshot,
        options: ready.options,
        owner: ready.owner,
        generation: ready.generation,
    })
}

/// Consume an objects-ready continuation and perform the canonical fresh link.
pub fn runnable_ready(
    session: &mut crate::CompilerSession,
    ready: ObjectsReady,
) -> crate::MultiErrorResult<crate::CompileOutput> {
    validate_endpoint_capability(session, &ready.owner, ready.generation)?;
    crate::queries::compile_rooted_with_session(
        session,
        &ready.snapshot,
        &ready.options,
        ready.rooted,
    )
}

impl PresentationOutput {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn warnings(&self) -> &[crate::CompileWarning] {
        &self.warnings
    }
}

/// Query the canonical reached-body/CFG artifact used by codegen.
pub fn rooted_cfg(
    session: &mut crate::CompilerSession,
    options: &crate::CompileOptions,
) -> Result<RootedCfgOutput, crate::CompileErrors> {
    session.rooted_cfg(options)
}

pub fn rir_payload_storage_stats(view: &crate::RirView) -> rue_rir::RirPayloadStorageStats {
    view.rir().payload_storage_stats()
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
        let mut warnings = Vec::new();
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
                    let rooted = self.rooted_codegen(request.options, backend_request)?;
                    warnings = rooted.warnings;
                    for collected in rooted.units {
                        let unit = collected.unit;
                        match stage {
                            PresentationStage::Lowering => {
                                write!(
                                    &mut text,
                                    "{}",
                                    unit.artifacts
                                        .lowering
                                        .as_ref()
                                        .expect("lowering projection was requested")
                                )
                                .expect("write to String");
                            }
                            PresentationStage::Mir => {
                                writeln!(&mut text, "function {}:", unit.defined_symbol)
                                    .expect("write to String");
                                writeln!(
                                    &mut text,
                                    "{}",
                                    unit.artifacts
                                        .mir
                                        .as_deref()
                                        .expect("MIR projection was requested")
                                )
                                .expect("write to String");
                            }
                            PresentationStage::Liveness => {
                                writeln!(&mut text, "function {}:", unit.defined_symbol)
                                    .expect("write to String");
                                writeln!(
                                    &mut text,
                                    "{}",
                                    unit.artifacts
                                        .liveness
                                        .as_deref()
                                        .expect("liveness projection was requested")
                                )
                                .expect("write to String");
                            }
                            PresentationStage::RegAlloc => {
                                writeln!(&mut text, "function {}:", unit.defined_symbol)
                                    .expect("write to String");
                                write!(
                                    &mut text,
                                    "{}",
                                    unit.artifacts
                                        .regalloc
                                        .as_deref()
                                        .expect("regalloc projection was requested")
                                )
                                .expect("write to String");
                            }
                            PresentationStage::Asm => {
                                writeln!(&mut text, ".globl {}", unit.defined_symbol)
                                    .expect("write to String");
                                writeln!(&mut text, "{}:", unit.defined_symbol)
                                    .expect("write to String");
                                write!(
                                    &mut text,
                                    "{}",
                                    unit.artifacts
                                        .asm
                                        .as_deref()
                                        .expect("assembly projection was requested")
                                )
                                .expect("write to String");
                            }
                            _ => unreachable!("backend request has a backend presentation stage"),
                        }
                    }
                } else {
                    let rooted = self.rooted_cfg(request.options)?;
                    warnings = rooted.warnings;
                    for function in rooted.cfgs {
                        let record = &function.record;
                        match stage {
                            PresentationStage::Air => {
                                writeln!(&mut text, "function {}:", record.source_name)
                                    .expect("write to String");
                                writeln!(
                                    &mut text,
                                    "{}",
                                    record.air.display_with_interner(&record.interner)
                                )
                                .expect("write to String");
                            }
                            PresentationStage::Cfg => {
                                writeln!(
                                    &mut text,
                                    "{}",
                                    record.cfg.display_with_interner(&record.interner)
                                )
                                .expect("write to String");
                            }
                            PresentationStage::StackFrame => {
                                writeln!(
                                    &mut text,
                                    "{}",
                                    rue_codegen::generate_stack_frame_info(
                                        &record.cfg,
                                        &record.codegen.defined_symbol,
                                        &record.type_pool,
                                        &record.interner,
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
        Ok(PresentationOutput { text, warnings })
    }
}

#[cfg(test)]
mod codegen_unit_tests {
    use super::*;

    fn trusted_accessor_snapshot(root: &str, module: &str) -> crate::SourceSnapshot {
        use std::collections::{HashMap, HashSet};

        let root_file = crate::FileId::new(1);
        let module_file = crate::FileId::new(2);
        let metadata = crate::SourceMetadata::new_with_trusted_standard_library(
            root_file,
            HashMap::from([
                (root_file, "/project/main.rue".to_owned()),
                (module_file, "/project/std/bridge.rue".to_owned()),
            ]),
            HashMap::from([
                (root_file, "main.rue".to_owned()),
                (module_file, "\0rue-std/bridge.rue".to_owned()),
            ]),
            HashSet::from([module_file]),
        )
        .expect("trusted accessor fixture metadata is valid");
        crate::SourceSnapshot::new(
            metadata,
            vec![
                (root_file, Arc::new(root.to_owned())),
                (module_file, Arc::new(module.to_owned())),
            ],
        )
        .expect("trusted accessor fixture snapshot is valid")
    }

    fn borrow_accessor_options() -> crate::CompileOptions {
        let mut options = crate::CompileOptions::default();
        options
            .preview_features
            .insert(rue_error::PreviewFeature::BorrowAccessors);
        options
    }

    #[test]
    fn codegen_presentation_is_available_before_and_after_normal_codegen() {
        let snapshot = crate::SourceSnapshot::single("main.rue", "fn main() -> i32 { 7 }").unwrap();
        let edited = crate::SourceSnapshot::single(
            "main.rue",
            "fn main() -> i32 { let value = 7; value + value }",
        )
        .unwrap();
        let options = crate::CompileOptions::default();
        for stage in [
            PresentationStage::Lowering,
            PresentationStage::Mir,
            PresentationStage::Liveness,
            PresentationStage::RegAlloc,
            PresentationStage::Asm,
        ] {
            let order = snapshot
                .files()
                .map(|file| file.file_id)
                .collect::<Vec<_>>();
            let mut session = crate::CompilerSession::new();
            crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
            let before = session
                .unstable_present(PresentationRequest {
                    stage,
                    options: &options,
                    file_order: &order,
                })
                .unwrap();
            let semantic = session.rooted_cfg(&options).unwrap();
            session
                .codegen_units(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap();
            let after = session
                .unstable_present(PresentationRequest {
                    stage,
                    options: &options,
                    file_order: &order,
                })
                .unwrap();
            assert_eq!(before.text, after.text, "{stage:?}");

            session
                .update_for_presentation(&edited)
                .into_result()
                .unwrap();
            let edited_order = edited.files().map(|file| file.file_id).collect::<Vec<_>>();
            let after_edit = session
                .unstable_present(PresentationRequest {
                    stage,
                    options: &options,
                    file_order: &edited_order,
                })
                .unwrap();
            assert_ne!(after.text, after_edit.text, "{stage:?} edit was stale");
        }
    }

    #[test]
    fn codegen_unit_failure_preserves_the_backend_diagnostic() {
        let snapshot = crate::SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
        let options = crate::CompileOptions::default();
        let mut session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
        let semantic = session.rooted_cfg(&options).unwrap();
        let errors = crate::codegen_query::with_test_codegen_failure_injection(|| {
            session
                .codegen_units(
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
    fn frontend_emit_stages_stop_at_rooted_cfg_and_do_not_leak_backend_failures() {
        let snapshot = crate::SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
        let options = crate::CompileOptions::default();
        let order = [rue_span::FileId::DEFAULT];
        let mut frontend = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut frontend, &snapshot).unwrap();

        crate::codegen_query::with_test_codegen_failure_injection(|| {
            for stage in [
                PresentationStage::Air,
                PresentationStage::Cfg,
                PresentationStage::StackFrame,
            ] {
                frontend
                    .unstable_present(PresentationRequest {
                        stage,
                        options: &options,
                        file_order: &order,
                    })
                    .unwrap_or_else(|errors| {
                        panic!("frontend stage {stage:?} leaked a backend failure: {errors:?}")
                    });
            }
        });
        assert!(frontend.codegen_executions().is_empty());
        assert_eq!(frontend.rooted_cfg_executions().len(), 1);

        let mut backend = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut backend, &snapshot).unwrap();
        let errors = crate::codegen_query::with_test_codegen_failure_injection(|| {
            backend
                .unstable_present(PresentationRequest {
                    stage: PresentationStage::Mir,
                    options: &options,
                    file_order: &order,
                })
                .unwrap_err()
        });
        assert!(matches!(
            errors.first().map(|error| &error.kind),
            Some(crate::ErrorKind::InternalCodegenError(_))
        ));
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
        let semantic = session.rooted_cfg(&options).unwrap();
        let before = session
            .codegen_units(
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
            .find(|product| product.unit.defined_symbol.as_ref() == "main")
            .unwrap()
            .unit
            .text_atom()
            .unwrap();
        crate::publish_test_snapshot(&mut session, &second).unwrap();
        let semantic = session.rooted_cfg(&options).unwrap();
        let after = session
            .codegen_units(
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
                .find(|product| product.unit.defined_symbol.as_ref() == "main")
                .unwrap()
                .unit
                .text_atom()
                .unwrap()
        );
    }

    #[test]
    fn accessor_yield_link_verdict_follows_a_sibling_method_s_borrow_qualifier() {
        // 6.6:7 admits `yield self.link()` only when `link` is itself an
        // accessor. The declaration-time producer decides that from the
        // owner's other *parsed* declarations, so editing `link`'s result
        // qualifier has to invalidate the verdict recorded for `xr` — in both
        // directions (RUE-1232).
        let source = |link: &str| {
            crate::SourceSnapshot::single(
                "main.rue",
                format!(
                    "struct P {{ x: i64, fn link(borrow self) -> {link} fn xr(borrow self) -> borrow i64 {{ yield self.link(); }} }} fn main() -> i32 {{ let p = P {{ x: 0 }}; @intCast(p.xr()) }}"
                ),
            )
            .unwrap()
        };
        let plain = "i64 { self.x }";
        let accessor = "borrow i64 { yield self.x; }";
        let mut options = crate::CompileOptions::default();
        options
            .preview_features
            .insert(rue_error::PreviewFeature::BorrowAccessors);

        let mut session = crate::CompilerSession::new();
        session.update(&source(plain)).into_result().unwrap();
        let errors = session.rooted_cfg(&options).unwrap_err();
        assert!(
            errors.iter().any(|error| matches!(
                &error.kind,
                rue_error::ErrorKind::AccessorYieldNotReceiverRooted { .. }
            )),
            "{errors:?}"
        );

        session.update(&source(accessor)).into_result().unwrap();
        session
            .rooted_cfg(&options)
            .expect("an accessor link is a legal projection");

        session.update(&source(plain)).into_result().unwrap();
        let errors = session.rooted_cfg(&options).unwrap_err();
        assert!(
            errors.iter().any(|error| matches!(
                &error.kind,
                rue_error::ErrorKind::AccessorYieldNotReceiverRooted { .. }
            )),
            "{errors:?}"
        );
    }

    #[test]
    fn accessor_edit_recomputes_caller_without_publishing_accessor_abi() {
        let source = |value| {
            crate::SourceSnapshot::single(
                "main.rue",
                format!(
                    "struct P {{ x: i64, fn value(borrow self) -> borrow i64 {{ if self.x == {value} {{ let bad = 1 / 0; if bad == 0 {{ }} }} yield self.x; }} }} fn helper() -> i64 {{ 1 }} fn main() -> i32 {{ let p = P {{ x: 7 }}; if p.value() + helper() == 8 {{ 0 }} else {{ 1 }} }}"
                ),
            )
            .unwrap()
        };
        let mut options = crate::CompileOptions::default();
        options
            .preview_features
            .insert(rue_error::PreviewFeature::BorrowAccessors);
        let mut session = crate::CompilerSession::new();

        session.update(&source(7)).into_result().unwrap();
        let semantic = session.rooted_cfg(&options).unwrap();
        let cold = session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        assert_eq!(cold.len(), 2, "accessors have no out-of-line ABI unit");
        assert!(
            cold.iter()
                .all(|unit| !unit.unit.defined_symbol.contains(".value"))
        );
        let cold_rooted = session.rooted_cfg(&options).unwrap();
        let cold_helper_key = cold_rooted
            .cfgs
            .iter()
            .find(|unit| crate::cfg_query::accessor_source_name(&unit.function) == "helper")
            .unwrap()
            .optimized_cfg_key
            .clone();

        session.update(&source(8)).into_result().unwrap();
        let warm_rooted = session.rooted_cfg(&options).unwrap();
        let warm_helper_key = &warm_rooted
            .cfgs
            .iter()
            .find(|unit| crate::cfg_query::accessor_source_name(&unit.function) == "helper")
            .unwrap()
            .optimized_cfg_key;
        assert_eq!(
            cold_helper_key, *warm_helper_key,
            "{cold_helper_key:#?}\n{warm_helper_key:#?}"
        );
        let execution = |name: &str| {
            session
                .rooted_cfg_executions()
                .iter()
                .find_map(|(identity, execution)| {
                    matches!(identity, crate::FunctionInstanceKey::Definition(definition) if definition.name() == name)
                        .then_some(*execution)
                })
                .unwrap()
        };
        assert_eq!(execution("main"), rue_query::RequestExecution::Computed);
        assert_eq!(
            execution("helper"),
            rue_query::RequestExecution::Reused,
            "{:#?}",
            session.rooted_cfg_executions()
        );
        assert!(session.rooted_cfg_executions().iter().all(|(identity, _)| {
            !matches!(identity, crate::FunctionInstanceKey::Definition(definition) if definition.name() == "value")
        }));
        let semantic = session.rooted_cfg(&options).unwrap();
        let warm = session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let mut fresh = crate::CompilerSession::new();
        fresh.update(&source(8)).into_result().unwrap();
        let semantic = fresh.rooted_cfg(&options).unwrap();
        let fresh = fresh
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let warm = warm
            .iter()
            .map(|product| {
                (
                    &product.unit.defined_symbol,
                    product.unit.text_atom().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let fresh = fresh
            .iter()
            .map(|product| {
                (
                    &product.unit.defined_symbol,
                    product.unit.text_atom().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(warm, fresh);
    }

    #[test]
    fn trusted_place_bridge_is_confined_to_the_exact_receiver_rooted_form() {
        let root = r#"
const bridge = @import("std/bridge.rue");

fn main() -> i32 {
    let value: i64 = 7;
    let pointer: ptr const i64 = checked { @raw(value) };
    let P = bridge.Buf(i64);
    let p = P { buf: pointer, value };
    @intCast(p.get_ref(pointer))
}
"#;
        let module = |body: &str| {
            format!(
                r#"
pub fn Buf(comptime T: type) -> type {{
    struct {{
        buf: ptr const T,
        value: T,

        fn get_ref(borrow self, other: ptr const T) -> borrow T {{
            {body}
        }}
    }}
}}
"#
            )
        };
        let options = borrow_accessor_options();
        let compile = |body: &str| {
            let snapshot = trusted_accessor_snapshot(root, &module(body));
            let mut session = crate::CompilerSession::new();
            crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
            session.rooted_cfg(&options)
        };

        compile("yield checked { @place(@ptr_offset(self.buf, 0)) };")
            .expect("the exact trusted receiver-rooted bridge is accepted");

        let errors = compile("yield checked { @place(@ptr_offset(other, 0)) };").unwrap_err();
        assert!(
            errors.iter().any(|error| matches!(
                &error.kind,
                rue_error::ErrorKind::AccessorYieldNotReceiverRooted { .. }
            )),
            "{errors:?}"
        );

        for body in [
            "yield @place(@ptr_offset(self.buf, 0));",
            "let ignored = checked { @place(@ptr_offset(self.buf, 0)) }; yield self.value;",
        ] {
            let errors = compile(body).unwrap_err();
            assert!(
                errors.iter().any(|error| matches!(
                    &error.kind,
                    rue_error::ErrorKind::UnknownIntrinsic(name) if name == "place"
                ) || matches!(
                    &error.kind,
                    rue_error::ErrorKind::AccessorYieldNotReceiverRooted { .. }
                )),
                "{errors:?}"
            );
        }

        let errors = compile("yield checked { @place() };").unwrap_err();
        assert!(
            errors.iter().any(|error| matches!(
                &error.kind,
                rue_error::ErrorKind::IntrinsicWrongArgCount { name, expected: 1, found: 0 }
                    if name == "place"
            )),
            "{errors:?}"
        );

        let errors = compile("yield checked { @place(@ptr_offset(self.value, 0)) };").unwrap_err();
        assert!(
            errors.iter().any(|error| matches!(
                &error.kind,
                rue_error::ErrorKind::IntrinsicTypeMismatch(mismatch)
                    if mismatch.name == "ptr_offset"
            )),
            "{errors:?}"
        );
    }

    #[test]
    fn trusted_anonymous_accessor_edit_recomputes_only_its_caller_without_an_abi_unit() {
        let root = r#"
const bridge = @import("std/bridge.rue");

fn helper() -> i32 { 1 }

fn main() -> i32 {
    let value: i64 = 7;
    let pointer: ptr const i64 = checked { @raw(value) };
    let B = bridge.Buf(i64);
    let b = B { buf: pointer };
    @intCast(b.get_ref(0)) + helper()
}
"#;
        let module = |guard: u64| {
            format!(
                r#"
pub fn Buf(comptime T: type) -> type {{
    struct {{
        buf: ptr const T,

        fn get_ref(borrow self, i: u64) -> borrow T {{
            if i == {guard} {{ @panic("index out of bounds"); }}
            yield checked {{ @place(@ptr_offset(self.buf, i)) }};
        }}
    }}
}}
"#
            )
        };
        let options = borrow_accessor_options();
        let source = |guard| trusted_accessor_snapshot(root, &module(guard));
        let mut session = crate::CompilerSession::new();

        let cold_source = source(11);
        crate::publish_test_snapshot(&mut session, &cold_source).unwrap();
        let cold_semantic = session.rooted_cfg(&options).unwrap();
        let cold = session
            .codegen_units(
                &cold_semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        assert_eq!(
            cold.len(),
            2,
            "trusted accessors have no out-of-line ABI unit"
        );
        assert!(
            cold.iter()
                .all(|unit| !unit.unit.defined_symbol.contains("get_ref"))
        );
        let cold_helper_key = cold_semantic
            .cfgs
            .iter()
            .find(|unit| crate::cfg_query::accessor_source_name(&unit.function) == "helper")
            .unwrap()
            .optimized_cfg_key
            .clone();

        let warm_source = source(12);
        crate::publish_test_snapshot(&mut session, &warm_source).unwrap();
        let warm_semantic = session.rooted_cfg(&options).unwrap();
        let warm_helper_key = &warm_semantic
            .cfgs
            .iter()
            .find(|unit| crate::cfg_query::accessor_source_name(&unit.function) == "helper")
            .unwrap()
            .optimized_cfg_key;
        assert_eq!(cold_helper_key, *warm_helper_key);
        let execution = |name: &str| {
            session
                .rooted_cfg_executions()
                .iter()
                .find_map(|(identity, execution)| {
                    matches!(identity, crate::FunctionInstanceKey::Definition(definition) if definition.name() == name)
                        .then_some(*execution)
                })
                .unwrap()
        };
        assert_eq!(execution("main"), rue_query::RequestExecution::Computed);
        assert_eq!(execution("helper"), rue_query::RequestExecution::Reused);

        let warm = session
            .codegen_units(
                &warm_semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let mut fresh = crate::CompilerSession::new();
        let fresh_source = source(12);
        crate::publish_test_snapshot(&mut fresh, &fresh_source).unwrap();
        let fresh_semantic = fresh.rooted_cfg(&options).unwrap();
        let fresh = fresh
            .codegen_units(
                &fresh_semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let warm = warm
            .iter()
            .map(|product| {
                (
                    product.unit.defined_symbol.clone(),
                    product.unit.text_atom().unwrap().to_vec(),
                )
            })
            .collect::<Vec<_>>();
        let fresh = fresh
            .iter()
            .map(|product| {
                (
                    product.unit.defined_symbol.clone(),
                    product.unit.text_atom().unwrap().to_vec(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(warm, fresh);
    }

    #[test]
    fn accessor_raw_cfg_dependency_key_is_shared_across_distinct_callers() {
        let snapshot = crate::SourceSnapshot::single(
            "main.rue",
            "struct P { x: i64, fn value(borrow self) -> borrow i64 { yield self.x; } } \
             struct A { n: i64 } struct B { n: i64 } \
             fn caller_a(borrow p: P) -> i64 { let a = A { n: 1 }; p.value() + a.n } \
             fn caller_b(borrow p: P) -> i64 { let b = B { n: 2 }; p.value() + b.n } \
             fn main() -> i32 { let p = P { x: 3 }; if caller_a(borrow p) + caller_b(borrow p) == 9 { 0 } else { 1 } }",
        )
        .unwrap();
        let mut options = crate::CompileOptions::default();
        options
            .preview_features
            .insert(rue_error::PreviewFeature::BorrowAccessors);
        let mut session = crate::CompilerSession::new();
        session.update(&snapshot).into_result().unwrap();
        let rooted = session.rooted_cfg(&options).unwrap();
        let dependency = |name: &str| {
            let unit = rooted
                .cfgs
                .iter()
                .find(|unit| crate::cfg_query::accessor_source_name(&unit.function) == name)
                .unwrap();
            assert_eq!(unit.optimized_cfg_key.accessor_dependencies.len(), 1);
            unit.optimized_cfg_key.accessor_dependencies[0].clone()
        };

        assert_eq!(dependency("caller_a"), dependency("caller_b"));
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
            let semantic = session.rooted_cfg(&options).unwrap();
            let first = session
                .codegen_units(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap();
            let second = session
                .codegen_units(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap();
            assert_eq!(
                first
                    .iter()
                    .map(|product| (product.unit.text_atom().unwrap(), &product.unit.relocations))
                    .collect::<Vec<_>>(),
                second
                    .iter()
                    .map(|product| (product.unit.text_atom().unwrap(), &product.unit.relocations))
                    .collect::<Vec<_>>()
            );
            let relocs = &first
                .iter()
                .find(|product| product.unit.defined_symbol.as_ref() == "main")
                .unwrap()
                .unit
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
            let semantic = session.rooted_cfg(&options).unwrap();
            let units = session
                .codegen_units(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest {
                        asm: true,
                        ..Default::default()
                    },
                )
                .unwrap();
            let unit_identity = units
                .iter()
                .map(|unit| {
                    (
                        unit.unit.defined_symbol.clone(),
                        unit.unit.content_fingerprint,
                        unit.unit.sections.clone(),
                        unit.unit.relocations.clone(),
                    )
                })
                .collect::<Vec<_>>();
            let objects = units
                .iter()
                .map(|unit| {
                    crate::backend::project_backend_object(&unit.unit, options.target).unwrap()
                })
                .collect::<Vec<_>>();
            (unit_identity, objects)
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
        let semantic = session.rooted_cfg(&options).unwrap();
        let consume_name = semantic
            .functions()
            .iter()
            .find(|function| function.definition_source_name() == Some("consume"))
            .unwrap()
            .record
            .codegen
            .defined_symbol
            .to_string();
        let before = session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let consume_before = before
            .iter()
            .find(|product| product.unit.defined_symbol.as_ref() == consume_name)
            .map(|product| {
                (
                    product.unit.text_atom().unwrap().to_vec(),
                    product.unit.relocations.clone(),
                )
            })
            .unwrap();
        crate::publish_test_snapshot(&mut session, &second).unwrap();
        let semantic = session.rooted_cfg(&options).unwrap();
        let after = session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let consume_after = after
            .iter()
            .find(|product| product.unit.defined_symbol.as_ref() == consume_name)
            .map(|product| {
                (
                    product.unit.text_atom().unwrap().to_vec(),
                    product.unit.relocations.clone(),
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

/// Deterministic retained-terminal validation work contained in [`MetricsSnapshot`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryValidationMetrics {
    pub traversals: u64,
    pub successful_traversals: u64,
    pub dirty_traversals: u64,
    pub aborted_traversals: u64,
    pub input_observations: u64,
    pub dependency_observations: u64,
    pub registry_probes: u64,
    pub registry_index_lookups: u64,
    pub registry_misses: u64,
    pub node_visits: u64,
    pub active_cycle_prunes: u64,
    pub memo_hits: u64,
    pub memo_misses: u64,
    pub certificate_misses: u64,
    pub proof_reacquisition_misses: u64,
    pub endorsement_probes: u64,
    pub endorsement_hits: u64,
    pub terminal_lease_observations: u64,
    pub duplicate_terminal_lease_observations: u64,
    pub demands: u64,
    pub demand_reuses: u64,
    pub demand_computes: u64,
    pub demand_joins: u64,
    pub demand_aborts: u64,
    pub superseded: u64,
    pub certificates_published: u64,
}

impl QueryValidationMetrics {
    #[must_use]
    pub fn saturating_sub(self, earlier: Self) -> Self {
        Self::from(self.into_runtime().saturating_sub(earlier.into_runtime()))
    }

    pub fn saturating_add_assign(&mut self, other: Self) {
        let mut aggregate = self.into_runtime();
        aggregate.saturating_add_assign(other.into_runtime());
        *self = Self::from(aggregate);
    }

    fn into_runtime(self) -> rue_query::ValidationWork {
        rue_query::ValidationWork {
            traversals: self.traversals,
            successful_traversals: self.successful_traversals,
            dirty_traversals: self.dirty_traversals,
            aborted_traversals: self.aborted_traversals,
            input_observations: self.input_observations,
            dependency_observations: self.dependency_observations,
            registry_probes: self.registry_probes,
            registry_index_lookups: self.registry_index_lookups,
            registry_misses: self.registry_misses,
            node_visits: self.node_visits,
            active_cycle_prunes: self.active_cycle_prunes,
            memo_hits: self.memo_hits,
            memo_misses: self.memo_misses,
            certificate_misses: self.certificate_misses,
            proof_reacquisition_misses: self.proof_reacquisition_misses,
            endorsement_probes: self.endorsement_probes,
            endorsement_hits: self.endorsement_hits,
            terminal_lease_observations: self.terminal_lease_observations,
            duplicate_terminal_lease_observations: self.duplicate_terminal_lease_observations,
            demands: self.demands,
            demand_reuses: self.demand_reuses,
            demand_computes: self.demand_computes,
            demand_joins: self.demand_joins,
            demand_aborts: self.demand_aborts,
            superseded: self.superseded,
            certificates_published: self.certificates_published,
        }
    }
}

impl From<rue_query::ValidationWork> for QueryValidationMetrics {
    fn from(work: rue_query::ValidationWork) -> Self {
        Self {
            traversals: work.traversals,
            successful_traversals: work.successful_traversals,
            dirty_traversals: work.dirty_traversals,
            aborted_traversals: work.aborted_traversals,
            input_observations: work.input_observations,
            dependency_observations: work.dependency_observations,
            registry_probes: work.registry_probes,
            registry_index_lookups: work.registry_index_lookups,
            registry_misses: work.registry_misses,
            node_visits: work.node_visits,
            active_cycle_prunes: work.active_cycle_prunes,
            memo_hits: work.memo_hits,
            memo_misses: work.memo_misses,
            certificate_misses: work.certificate_misses,
            proof_reacquisition_misses: work.proof_reacquisition_misses,
            endorsement_probes: work.endorsement_probes,
            endorsement_hits: work.endorsement_hits,
            terminal_lease_observations: work.terminal_lease_observations,
            duplicate_terminal_lease_observations: work.duplicate_terminal_lease_observations,
            demands: work.demands,
            demand_reuses: work.demand_reuses,
            demand_computes: work.demand_computes,
            demand_joins: work.demand_joins,
            demand_aborts: work.demand_aborts,
            superseded: work.superseded,
            certificates_published: work.certificates_published,
        }
    }
}

#[cfg(test)]
mod query_validation_metrics_tests {
    use super::{QueryDisplayIdentityMetrics, QueryRuntimeMetrics, QueryValidationMetrics};

    #[test]
    fn validation_work_classification_survives_projection_and_deltas() {
        let before = rue_query::ValidationWork {
            memo_misses: 3,
            certificate_misses: 2,
            proof_reacquisition_misses: 1,
            terminal_lease_observations: 7,
            duplicate_terminal_lease_observations: 2,
            ..Default::default()
        };
        let after = rue_query::ValidationWork {
            memo_misses: 8,
            certificate_misses: 5,
            proof_reacquisition_misses: 3,
            terminal_lease_observations: 11,
            duplicate_terminal_lease_observations: 5,
            ..Default::default()
        };

        assert_eq!(QueryValidationMetrics::from(after).into_runtime(), after);
        let delta = QueryValidationMetrics::from(after)
            .saturating_sub(QueryValidationMetrics::from(before));
        assert_eq!(delta.memo_misses, 5);
        assert_eq!(delta.certificate_misses, 3);
        assert_eq!(delta.proof_reacquisition_misses, 2);
        assert_eq!(delta.terminal_lease_observations, 4);
        assert_eq!(delta.duplicate_terminal_lease_observations, 3);
    }

    #[test]
    fn runtime_work_arithmetic_covers_every_published_counter() {
        let unit = QueryRuntimeMetrics {
            claims: 1,
            reuses: 1,
            joins: 1,
            declined_joins: 1,
            body_completions: 1,
            red_publications: 1,
            green_publications: 1,
            cancellations: 1,
            cycles: 1,
            validation: QueryValidationMetrics {
                traversals: 1,
                ..Default::default()
            },
            display_identities: QueryDisplayIdentityMetrics {
                memo_node_materializations: 1,
                ..Default::default()
            },
            retention_enforcements: 1,
            retention_scan_entries: 1,
            query_worker_active_ns: 1,
            ready_items: 1,
            ready_wait_ns: 1,
            max_ready_wait_ns: 1,
            longest_query_dependency_chain: 1,
            peak_query_workers: 1,
            donated_permits: 1,
        };
        let mut accumulated = unit;
        accumulated.saturating_add_assign(unit);
        let mut expected_delta = unit;
        // High-water marks compose by maximum rather than addition. Repeating
        // the same maximum therefore contributes no newly observed height to
        // a cumulative-snapshot delta.
        expected_delta.max_ready_wait_ns = 0;
        expected_delta.longest_query_dependency_chain = 0;
        expected_delta.peak_query_workers = 0;
        assert_eq!(accumulated.saturating_sub(unit), expected_delta);
    }
}

/// Query-runtime lifecycle, validation, and retention work contained in
/// [`MetricsSnapshot`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryRuntimeMetrics {
    pub claims: u64,
    pub reuses: u64,
    pub joins: u64,
    pub declined_joins: u64,
    pub body_completions: u64,
    pub red_publications: u64,
    pub green_publications: u64,
    pub cancellations: u64,
    pub cycles: u64,
    pub validation: QueryValidationMetrics,
    /// Presentation-only query identity materialization.
    pub display_identities: QueryDisplayIdentityMetrics,
    /// Family-local retention passes run.
    pub retention_enforcements: u64,
    /// Retention-queue entries examined by those passes.
    pub retention_scan_entries: u64,
    /// Aggregated critical-path and worker-scheduling evidence.
    pub query_worker_active_ns: u64,
    pub ready_items: u64,
    pub ready_wait_ns: u64,
    pub max_ready_wait_ns: u64,
    pub longest_query_dependency_chain: u64,
    pub peak_query_workers: u64,
    pub donated_permits: u64,
}

impl QueryRuntimeMetrics {
    /// Saturating delta between two cumulative runtime snapshots.
    pub fn saturating_sub(self, earlier: Self) -> Self {
        Self {
            claims: self.claims.saturating_sub(earlier.claims),
            reuses: self.reuses.saturating_sub(earlier.reuses),
            joins: self.joins.saturating_sub(earlier.joins),
            declined_joins: self.declined_joins.saturating_sub(earlier.declined_joins),
            body_completions: self
                .body_completions
                .saturating_sub(earlier.body_completions),
            red_publications: self
                .red_publications
                .saturating_sub(earlier.red_publications),
            green_publications: self
                .green_publications
                .saturating_sub(earlier.green_publications),
            cancellations: self.cancellations.saturating_sub(earlier.cancellations),
            cycles: self.cycles.saturating_sub(earlier.cycles),
            validation: self.validation.saturating_sub(earlier.validation),
            display_identities: self
                .display_identities
                .saturating_sub(earlier.display_identities),
            retention_enforcements: self
                .retention_enforcements
                .saturating_sub(earlier.retention_enforcements),
            retention_scan_entries: self
                .retention_scan_entries
                .saturating_sub(earlier.retention_scan_entries),
            query_worker_active_ns: self
                .query_worker_active_ns
                .saturating_sub(earlier.query_worker_active_ns),
            ready_items: self.ready_items.saturating_sub(earlier.ready_items),
            ready_wait_ns: self.ready_wait_ns.saturating_sub(earlier.ready_wait_ns),
            max_ready_wait_ns: self
                .max_ready_wait_ns
                .saturating_sub(earlier.max_ready_wait_ns),
            longest_query_dependency_chain: self
                .longest_query_dependency_chain
                .saturating_sub(earlier.longest_query_dependency_chain),
            peak_query_workers: self
                .peak_query_workers
                .saturating_sub(earlier.peak_query_workers),
            donated_permits: self.donated_permits.saturating_sub(earlier.donated_permits),
        }
    }

    /// Saturating accumulation for collection-level diagnostics.
    pub fn saturating_add_assign(&mut self, other: Self) {
        self.claims = self.claims.saturating_add(other.claims);
        self.reuses = self.reuses.saturating_add(other.reuses);
        self.joins = self.joins.saturating_add(other.joins);
        self.declined_joins = self.declined_joins.saturating_add(other.declined_joins);
        self.body_completions = self.body_completions.saturating_add(other.body_completions);
        self.red_publications = self.red_publications.saturating_add(other.red_publications);
        self.green_publications = self
            .green_publications
            .saturating_add(other.green_publications);
        self.cancellations = self.cancellations.saturating_add(other.cancellations);
        self.cycles = self.cycles.saturating_add(other.cycles);
        self.validation.saturating_add_assign(other.validation);
        self.display_identities
            .saturating_add_assign(other.display_identities);
        self.retention_enforcements = self
            .retention_enforcements
            .saturating_add(other.retention_enforcements);
        self.retention_scan_entries = self
            .retention_scan_entries
            .saturating_add(other.retention_scan_entries);
        self.query_worker_active_ns = self
            .query_worker_active_ns
            .saturating_add(other.query_worker_active_ns);
        self.ready_items = self.ready_items.saturating_add(other.ready_items);
        self.ready_wait_ns = self.ready_wait_ns.saturating_add(other.ready_wait_ns);
        self.max_ready_wait_ns = self.max_ready_wait_ns.max(other.max_ready_wait_ns);
        self.longest_query_dependency_chain = self
            .longest_query_dependency_chain
            .max(other.longest_query_dependency_chain);
        self.peak_query_workers = self.peak_query_workers.max(other.peak_query_workers);
        self.donated_permits = self.donated_permits.saturating_add(other.donated_permits);
    }
}

impl From<rue_query::RuntimeMetrics> for QueryRuntimeMetrics {
    fn from(runtime: rue_query::RuntimeMetrics) -> Self {
        Self {
            claims: runtime.claims,
            reuses: runtime.reuses,
            joins: runtime.joins,
            declined_joins: runtime.declined_joins,
            body_completions: runtime.body_completions,
            red_publications: runtime.red_publications,
            green_publications: runtime.green_publications,
            cancellations: runtime.cancellations,
            cycles: runtime.cycles,
            validation: runtime.validation.into(),
            display_identities: runtime.display_identities.into(),
            retention_enforcements: runtime.retention_enforcements,
            retention_scan_entries: runtime.retention_scan_entries,
            query_worker_active_ns: runtime.query_worker_active_ns,
            ready_items: runtime.ready_items,
            ready_wait_ns: runtime.ready_wait_ns,
            max_ready_wait_ns: runtime.max_ready_wait_ns,
            longest_query_dependency_chain: runtime.longest_query_dependency_chain,
            peak_query_workers: runtime.peak_query_workers,
            donated_permits: runtime.donated_permits,
        }
    }
}

/// Database-owned semantic-reachability scheduling work accumulated by the
/// session, including acquisition rounds that park before final publication.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticReachabilityMetrics {
    pub frontier_scans: u64,
    pub frontier_scan_keys: u64,
    pub frontier_batches: u64,
    pub frontier_keys: u64,
    pub frontier_width_one: u64,
    pub frontier_width_two_to_three: u64,
    pub frontier_width_four_to_seven: u64,
    pub frontier_width_eight_or_more: u64,
    pub transactions_prefetched: u64,
    pub transactions_serial: u64,
}

/// Display-only query identity materialization contained in [`MetricsSnapshot`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryDisplayIdentityMetrics {
    /// New memo-node identity count.
    pub memo_node_materializations: u64,
    /// New memo-node formatted key bytes.
    pub memo_node_bytes: u64,
    /// Structured batch identities materialized to render wait cycles.
    pub structured_wait_materializations: u64,
    /// Structured batch key bytes formatted to render wait cycles.
    pub structured_wait_bytes: u64,
    /// Abort-fallback identity count.
    pub abort_fallback_materializations: u64,
    /// Abort-fallback formatted key bytes.
    pub abort_fallback_bytes: u64,
}

impl QueryDisplayIdentityMetrics {
    /// Saturating addition for aggregation across independent requests.
    pub fn saturating_add_assign(&mut self, other: Self) {
        self.memo_node_materializations = self
            .memo_node_materializations
            .saturating_add(other.memo_node_materializations);
        self.memo_node_bytes = self.memo_node_bytes.saturating_add(other.memo_node_bytes);
        self.structured_wait_materializations = self
            .structured_wait_materializations
            .saturating_add(other.structured_wait_materializations);
        self.structured_wait_bytes = self
            .structured_wait_bytes
            .saturating_add(other.structured_wait_bytes);
        self.abort_fallback_materializations = self
            .abort_fallback_materializations
            .saturating_add(other.abort_fallback_materializations);
        self.abort_fallback_bytes = self
            .abort_fallback_bytes
            .saturating_add(other.abort_fallback_bytes);
    }

    /// Saturating delta between two cumulative runtime snapshots.
    pub fn saturating_sub(self, earlier: Self) -> Self {
        Self {
            memo_node_materializations: self
                .memo_node_materializations
                .saturating_sub(earlier.memo_node_materializations),
            memo_node_bytes: self.memo_node_bytes.saturating_sub(earlier.memo_node_bytes),
            structured_wait_materializations: self
                .structured_wait_materializations
                .saturating_sub(earlier.structured_wait_materializations),
            structured_wait_bytes: self
                .structured_wait_bytes
                .saturating_sub(earlier.structured_wait_bytes),
            abort_fallback_materializations: self
                .abort_fallback_materializations
                .saturating_sub(earlier.abort_fallback_materializations),
            abort_fallback_bytes: self
                .abort_fallback_bytes
                .saturating_sub(earlier.abort_fallback_bytes),
        }
    }
}

impl From<rue_query::DisplayIdentityMetrics> for QueryDisplayIdentityMetrics {
    fn from(metrics: rue_query::DisplayIdentityMetrics) -> Self {
        Self {
            memo_node_materializations: metrics.memo_node_materializations,
            memo_node_bytes: metrics.memo_node_bytes,
            structured_wait_materializations: metrics.structured_wait_materializations,
            structured_wait_bytes: metrics.structured_wait_bytes,
            abort_fallback_materializations: metrics.abort_fallback_materializations,
            abort_fallback_bytes: metrics.abort_fallback_bytes,
        }
    }
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
    pub retained_query_records: usize,
    pub retained_bytes: usize,
    pub peak_retained_bytes: usize,
    pub retained_byte_budget: usize,
    pub dependency_pins: usize,
    pub peak_dependency_pins: usize,
    pub dependency_pin_budget: usize,
    pub aggregate_retention_probes: usize,
    pub retained_byte_probe_quantum: usize,
    pub dependency_pin_probe_quantum: usize,
    pub retained_byte_probe_overshoot_bound: usize,
    pub dependency_pin_probe_overshoot_bound: usize,
    pub active_task_leases: usize,
    pub peak_task_leases: usize,
    pub active_retained_pins: usize,
    pub peak_retained_pins: usize,
    pub retained_revisions: usize,
    pub retained_module_input_views: usize,
    pub retained_module_source_stamps: usize,
    pub retained_import_input_views: usize,
    pub retained_import_context_stamps: usize,
    pub retained_import_topology_stamps: usize,
    pub retained_import_provenance_stamps: usize,
    pub retained_import_observation_stamps: usize,
    pub retained_byte_pressure_events: usize,
    pub dependency_pin_pressure_events: usize,
    pub retained_byte_overflow_events: usize,
    pub dependency_pin_overflow_events: usize,
    pub peak_retained_byte_overage: usize,
    pub peak_dependency_pin_overage: usize,
    pub query_evictions: usize,
    pub retained_byte_evictions: usize,
    pub dependency_pin_evictions: usize,
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
    pub functions_considered: usize,
    pub materialization_index_builds: usize,
    pub materialization_declarations_scanned: usize,
    pub materialization_anonymous_nominals_scanned: usize,
    pub materialization_type_nodes_scanned: usize,
    pub materialization_fact_selections: usize,
    pub materialization_fact_closures_allocated: usize,
    pub materialization_fact_closures_reused: usize,
    pub materialization_declarations_selected: usize,
    pub materialization_anonymous_nominals_selected: usize,
    pub materialization_callables_selected: usize,
    pub materialization_nominal_metadata_selected: usize,
    pub materialization_modules_selected: usize,
    pub materialization_builtin_nominals_selected: usize,
    pub materialization_required_types_selected: usize,
    pub prerequisite_stable_types_scanned: usize,
    pub prerequisite_layout_requests: usize,
    pub prerequisite_drop_glue_requests: usize,
    pub retained_interner_charge_scans: usize,
    pub retained_interner_entries_scanned: usize,
    pub retained_interner_utf8_bytes_scanned: usize,
    pub local_epochs: usize,
    pub local_air_instructions: usize,
    pub local_air_payload_bytes: usize,
    pub local_type_entries: usize,
    pub local_aggregate_type_aliases: usize,
    pub local_materialized_type_handles: usize,
    pub local_interner_entries: usize,
    pub local_interner_utf8_bytes: usize,
    pub local_strings: usize,
    pub local_atoms: usize,
    pub cfg_builds_attempted: usize,
    pub cfg_builds_succeeded: usize,
    pub cfg_builds_failed: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticBodyMetrics {
    pub analyses_computed: usize,
    pub analyses_reused: usize,
    pub analyses_invalidated: usize,
    pub reachability_frontier_scans: usize,
    pub reachability_frontier_scan_keys: usize,
    pub reachability_frontier_batches: usize,
    pub reachability_frontier_keys: usize,
    pub reachability_frontier_width_one: usize,
    pub reachability_frontier_width_two_to_three: usize,
    pub reachability_frontier_width_four_to_seven: usize,
    pub reachability_frontier_width_eight_or_more: usize,
    pub reachability_transactions_prefetched: usize,
    pub reachability_transactions_serial: usize,
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
                reachability_frontier_scans: work.body_analysis.reachability_frontier_scans,
                reachability_frontier_scan_keys: work.body_analysis.reachability_frontier_scan_keys,
                reachability_frontier_batches: work.body_analysis.reachability_frontier_batches,
                reachability_frontier_keys: work.body_analysis.reachability_frontier_keys,
                reachability_frontier_width_one: work.body_analysis.reachability_frontier_width_one,
                reachability_frontier_width_two_to_three: work
                    .body_analysis
                    .reachability_frontier_width_two_to_three,
                reachability_frontier_width_four_to_seven: work
                    .body_analysis
                    .reachability_frontier_width_four_to_seven,
                reachability_frontier_width_eight_or_more: work
                    .body_analysis
                    .reachability_frontier_width_eight_or_more,
                reachability_transactions_prefetched: work
                    .body_analysis
                    .reachability_transactions_prefetched,
                reachability_transactions_serial: work
                    .body_analysis
                    .reachability_transactions_serial,
            },
            cfg: SemanticCfgMetrics {
                functions_considered: work.cfg.functions_considered,
                materialization_index_builds: work.cfg.materialization_index_builds,
                materialization_declarations_scanned: work.cfg.materialization_declarations_scanned,
                materialization_anonymous_nominals_scanned: work
                    .cfg
                    .materialization_anonymous_nominals_scanned,
                materialization_type_nodes_scanned: work.cfg.materialization_type_nodes_scanned,
                materialization_fact_selections: work.cfg.materialization_fact_selections,
                materialization_fact_closures_allocated: work
                    .cfg
                    .materialization_fact_closures_allocated,
                materialization_fact_closures_reused: work.cfg.materialization_fact_closures_reused,
                materialization_declarations_selected: work
                    .cfg
                    .materialization_declarations_selected,
                materialization_anonymous_nominals_selected: work
                    .cfg
                    .materialization_anonymous_nominals_selected,
                materialization_callables_selected: work.cfg.materialization_callables_selected,
                materialization_nominal_metadata_selected: work
                    .cfg
                    .materialization_nominal_metadata_selected,
                materialization_modules_selected: work.cfg.materialization_modules_selected,
                materialization_builtin_nominals_selected: work
                    .cfg
                    .materialization_builtin_nominals_selected,
                materialization_required_types_selected: work
                    .cfg
                    .materialization_required_types_selected,
                prerequisite_stable_types_scanned: work.cfg.prerequisite_stable_types_scanned,
                prerequisite_layout_requests: work.cfg.prerequisite_layout_requests,
                prerequisite_drop_glue_requests: work.cfg.prerequisite_drop_glue_requests,
                retained_interner_charge_scans: work.cfg.retained_interner_charge_scans,
                retained_interner_entries_scanned: work.cfg.retained_interner_entries_scanned,
                retained_interner_utf8_bytes_scanned: work.cfg.retained_interner_utf8_bytes_scanned,
                local_epochs: work.cfg.local_epochs,
                local_air_instructions: work.cfg.local_air_instructions,
                local_air_payload_bytes: work.cfg.local_air_payload_bytes,
                local_type_entries: work.cfg.local_type_entries,
                local_aggregate_type_aliases: work.cfg.local_aggregate_type_aliases,
                local_materialized_type_handles: work.cfg.local_materialized_type_handles,
                local_interner_entries: work.cfg.local_interner_entries,
                local_interner_utf8_bytes: work.cfg.local_interner_utf8_bytes,
                local_strings: work.cfg.local_strings,
                local_atoms: work.cfg.local_atoms,
                cfg_builds_attempted: work.cfg.cfg_builds_attempted,
                cfg_builds_succeeded: work.cfg.cfg_builds_succeeded,
                cfg_builds_failed: work.cfg.cfg_builds_failed,
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
    pub query_runtime: QueryRuntimeMetrics,
    pub semantic_reachability: SemanticReachabilityMetrics,
    pub provider_observations: ProviderObservationMetrics,
    pub publication: PublicationMetrics,
}

/// Deterministic publication-seam health for one compiler process (RUE-1576).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublicationMetrics {
    /// Times the declaration publication could not retain its projection cone
    /// and validation fell back to per-node demand cascades. Expected zero.
    pub cone_retention_failures: u64,
}

impl OneShotMetrics {
    pub(crate) fn new(
        stats: crate::SourceStats,
        work: crate::PipelineWork,
        query_runtime: QueryRuntimeMetrics,
        semantic_reachability: SemanticReachabilityMetrics,
        provider_observations: ProviderObservationMetrics,
        publication: PublicationMetrics,
    ) -> Self {
        Self {
            files: stats.files,
            bytes: stats.bytes,
            lines: stats.lines,
            tokens: stats.tokens,
            parsed: ParseMetrics::from_work(work.parsed),
            lowered: LowerMetrics::from_work(work.lowered),
            semantic: SemanticMetrics::from_work(work.semantic),
            query_runtime,
            semantic_reachability,
            provider_observations,
            publication,
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
    pub fn imports(&self) -> QueryMetrics {
        self.inner.imports.into()
    }
    pub fn import_diagnostics(&self) -> QueryMetrics {
        self.inner.import_diagnostics.into()
    }
    pub fn merge(&self) -> QueryMetrics {
        self.inner.merge.into()
    }
    pub fn rir(&self) -> QueryMetrics {
        self.inner.rir.into()
    }
    pub fn downstream_invalidations(&self) -> usize {
        self.inner.downstream_invalidations
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
            retained_query_records: self.inner.retention.retained_query_records,
            retained_bytes: self.inner.retention.retained_bytes,
            peak_retained_bytes: self.inner.retention.peak_retained_bytes,
            retained_byte_budget: self.inner.retention.retained_byte_budget,
            dependency_pins: self.inner.retention.dependency_pins,
            peak_dependency_pins: self.inner.retention.peak_dependency_pins,
            dependency_pin_budget: self.inner.retention.dependency_pin_budget,
            aggregate_retention_probes: self.inner.retention.aggregate_retention_probes,
            retained_byte_probe_quantum: self.inner.retention.retained_byte_probe_quantum,
            dependency_pin_probe_quantum: self.inner.retention.dependency_pin_probe_quantum,
            retained_byte_probe_overshoot_bound: self
                .inner
                .retention
                .retained_byte_probe_overshoot_bound,
            dependency_pin_probe_overshoot_bound: self
                .inner
                .retention
                .dependency_pin_probe_overshoot_bound,
            active_task_leases: self.inner.retention.active_task_leases,
            peak_task_leases: self.inner.retention.peak_task_leases,
            active_retained_pins: self.inner.retention.active_retained_pins,
            peak_retained_pins: self.inner.retention.peak_retained_pins,
            retained_revisions: self.inner.retention.retained_revisions,
            retained_module_input_views: self.inner.retention.retained_module_input_views,
            retained_module_source_stamps: self.inner.retention.retained_module_source_stamps,
            retained_import_input_views: self.inner.retention.retained_import_input_views,
            retained_import_context_stamps: self.inner.retention.retained_import_context_stamps,
            retained_import_topology_stamps: self.inner.retention.retained_import_topology_stamps,
            retained_import_provenance_stamps: self
                .inner
                .retention
                .retained_import_provenance_stamps,
            retained_import_observation_stamps: self
                .inner
                .retention
                .retained_import_observation_stamps,
            retained_byte_pressure_events: self.inner.retention.retained_byte_pressure_events,
            dependency_pin_pressure_events: self.inner.retention.dependency_pin_pressure_events,
            retained_byte_overflow_events: self.inner.retention.retained_byte_overflow_events,
            dependency_pin_overflow_events: self.inner.retention.dependency_pin_overflow_events,
            peak_retained_byte_overage: self.inner.retention.peak_retained_byte_overage,
            peak_dependency_pin_overage: self.inner.retention.peak_dependency_pin_overage,
            query_evictions: self.inner.retention.query_evictions,
            retained_byte_evictions: self.inner.retention.retained_byte_evictions,
            dependency_pin_evictions: self.inner.retention.dependency_pin_evictions,
            diagnostic_entries: self.inner.retention.diagnostic_entries,
            diagnostic_source_attempts: self.inner.retention.diagnostic_source_attempts,
            diagnostic_source_bytes: self.inner.retention.diagnostic_source_bytes,
        }
    }
    pub fn query_runtime(&self) -> QueryRuntimeMetrics {
        self.inner.runtime.query
    }
    pub fn semantic_reachability(&self) -> SemanticReachabilityMetrics {
        self.inner.runtime.semantic_reachability
    }
}

/// Deliberate fault selection for the differential incremental oracle.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DifferentialOracleFault {
    Semantic,
    Diagnostic,
    Import,
}
