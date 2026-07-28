//! RUE-1086 — two-dimensional scaling and warm/fresh correctness harness.
//!
//! This harness gates on *structural work counters*, never wall time. It varies
//! two axes of the semantic workload independently — the number of reached
//! function bodies and the number of unrelated (never-called) declarations — and
//! reads the per-body declaration-context counters plumbed onto
//! [`BodyAnalysisWork`] (`per_body_declaration_context`) plus the ordinary
//! binding/manifest/body counters that already exist on
//! [`CanonicalSemanticWork`].
//!
//! Wall-time, allocation-count, and peak-memory experiments must remain
//! separate from these deterministic counter assertions.
//!
//! # What it proves
//!
//! 1. Deterministic synthetic corpus varying reached-body count and
//!    unrelated-declaration count independently.
//! 2. Scaling rows (counter-based): fixed bodies / growing declarations, and
//!    fixed declarations / growing bodies.
//! 3. Two-revision invalidation rows asserted via counters and one shared
//!    warm-vs-fresh parity oracle used by *every* edit row.
//! 4. Specialization rows (breadth compiles, depth fails E1200), referencing the
//!    canonical boundary unit tests rather than duplicating them.
//! 5. A warm-vs-fresh parity oracle after every edit row.
//!
//! # The two-envelope expected-failure discipline
//!
//! Most long-lived expected-failure rows use [`Row::envelope`], which asserts
//! either a repaired target envelope or a documented known-bad witness. RUE-1090
//! is intentionally different: it is a post-identity-cut decision gate, so it
//! compares observed baseline and grown per-body counters directly.
//!
//! * **(a) the repaired target envelope** — the measured value at or below the
//!   flat/linear/incremental target (within a tight tolerance). This is a hard
//!   PASS. A repair *better* than the target still passes; the envelope never
//!   panics on an improvement.
//! * **(b) the documented unrepaired witness** — the measured value inside a
//!   *tight* band around the structurally predicted known-bad shape (e.g. per
//!   body prepare/install growing 1:1 with the declaration universe). This is a
//!   tracked expected-failure naming its issue.
//!
//! Anything **worse** than the witness band (e.g. duplicate body analyses, or a
//! warm recompute worse than a full fresh one), or **structurally between** the
//! two envelopes (a partial repair that is neither flat nor the documented
//! witness), FAILS the test so a human reconciles the row with the new reality.
//! The witnesses are predicted from the corpus knobs, not copied from a prior
//! run, so an unrelated regression that inflates the counters still fails loudly.
//!
//! Tracking issues, each named at its row:
//!
//! * **RUE-1089** — identity rows: per-body identity/lookup installation work
//!   should be invariant to unrelated-declaration count.
//! * **RUE-1091** — conditional shared-base / narrow-epoch repair: activated by
//!   RUE-1090 if per-body install/project/endpoint work grows with unrelated
//!   declarations; its edit-invalidation rows remain expected-failure evidence.
//! * **RUE-1090** — measurement gate: fixed bodies / growing declarations must
//!   leave observed per-body project/install/endpoint work flat.
//!
//! # Stage-source counter provenance
//!
//! The per-body declaration-context counters are accrued INSIDE
//! `analyze_body_query`, at each stage's own source, as the stage actually
//! performs the work (see `canonical_semantic.rs`). The coordinator no longer
//! charges them from the input slice lengths it happens to hold, and a body that
//! fails before install is not charged for installation it never performed. A
//! shortcut added inside any stage (a shared base that predeclares fewer shells,
//! a projection that reuses cached exports) therefore drops the corresponding
//! counter, and this harness observes it.
//!
//! # Recorded prediction
//!
//! Two distinct historical Caldera figures must not be conflated (ADR-0066 §3,
//! RUE-1086):
//!
//! * **~62% of cold wall time at Caldera scale** — the frozen ADR-0066 recorded
//!   prediction, restored verbatim: the per-body *install/project/endpoint*
//!   term. This is the `O(bodies × declarations)` share this harness gates on.
//! * **~85% total per-body setup share** — a *separate* number: the total
//!   per-body setup share (install/project/endpoint **plus** prepare and
//!   config), measured via the 200-sample stack profile. It is not the 62%
//!   prediction and is stated only as the total-setup context around it.
//!
//! Caldera measurement provenance (RUE-1083): base cutover commit 586f50c,
//! measured at commit aca4acb, release build (`--target-platforms
//! //platforms:release`), linux x64, `RUE_STD_PATH` set, 200 gdb stack samples
//! at 0.25s intervals plus per-stage `--time-passes` spans over
//! `examples/caldera/main.rue`. The ~45 ms *pre-link* target is an eventual
//! reference-host goal, not a current gate.
//!
//! ## Recorded structural baseline (stage-sourced counters)
//!
//! Single-file corpus, no std import, so the declaration universe is exactly
//! `reached_bodies + unrelated_decls + 1` (`main`) and `cold_bodies` is exactly
//! `reached_bodies + 1`:
//!
//! With `U = reached_bodies + unrelated_decls + 1`, per-body prepare, project,
//! and install each equal `U`, per-body endpoints equal `2·U + 1` (one stable
//! definition endpoint and one body-owner endpoint per declaration plus one
//! module endpoint), and total prepare work is `cold_bodies · U`:
//!
//! ```text
//!   bodies × decls | cold_bodies per_body_shells per_body_project per_body_semantics per_body_endpoints shells_total
//!     100 ×   100  |        101            201              201                201                403          20301
//!    1000 ×   100  |       1001           1101             1101               1101               2203        1102101
//! ```
//!
//! per-body prepare/project/install grow 1:1 with the universe and endpoints
//! grow 2:1. These are retained as historical predictions in the RUE-1090 audit
//! output, but the gate itself compares the observed baseline and grown values:
//! the post-identity-cut constant may change while remaining flat.

use crate::unstable::{
    DiscoverySourceAssembler, ImportDemandMode, begin_import_input_request,
    import_demand_frontier_for_roots, import_observation_ledger, publish_import_observation_batch,
};
use crate::*;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

// ---------------------------------------------------------------------------
// Deterministic synthetic corpus generator
// ---------------------------------------------------------------------------

/// A knob set for the synthetic corpus. The two structural axes —
/// `reached_bodies` and `unrelated_decls` — vary independently.
#[derive(Debug, Clone, Copy)]
struct Corpus {
    /// Free functions `b{i}` each called exactly once from `main`. Together with
    /// `main` these are the reached bodies the semantic pipeline analyzes.
    reached_bodies: usize,
    /// Free functions `d{j}` that nothing calls. They enlarge the declaration
    /// universe without adding reached bodies, isolating the per-body
    /// declaration-context term.
    unrelated_decls: usize,
}

impl Corpus {
    fn new(reached_bodies: usize, unrelated_decls: usize) -> Self {
        Self {
            reached_bodies,
            unrelated_decls,
        }
    }

    /// Render the corpus to deterministic Rue source. Byte-for-byte identical for
    /// identical knobs, so counters are reproducible.
    fn source(&self) -> String {
        let mut src = String::with_capacity((self.reached_bodies + self.unrelated_decls) * 24);
        for i in 0..self.reached_bodies {
            src.push_str(&format!("fn b{i}() -> i32 {{ {i} }}\n"));
        }
        for j in 0..self.unrelated_decls {
            src.push_str(&format!("fn d{j}() -> i32 {{ {j} }}\n"));
        }
        src.push_str("fn main() -> i32 {\n    let mut acc = 0;\n");
        for i in 0..self.reached_bodies {
            src.push_str(&format!("    acc = acc + b{i}();\n"));
        }
        src.push_str("    acc\n}\n");
        src
    }

    fn snapshot(&self) -> SourceSnapshot {
        SourceSnapshot::single("main.rue", self.source()).expect("synthetic corpus parses")
    }
}

fn unrelated_module_snapshot(unrelated_modules: usize) -> SourceSnapshot {
    unrelated_module_snapshot_with_main("fn main() -> i32 { 0 }", unrelated_modules)
}

fn unrelated_module_snapshot_with_main(
    main_source: &str,
    unrelated_modules: usize,
) -> SourceSnapshot {
    unrelated_module_snapshot_with_main_and_suffix(main_source, unrelated_modules, "")
}

fn unrelated_module_snapshot_with_main_and_suffix(
    main_source: &str,
    unrelated_modules: usize,
    suffix: &str,
) -> SourceSnapshot {
    let mut physical = HashMap::new();
    let mut logical = HashMap::new();
    let mut contents = Vec::new();
    physical.insert(FileId::DEFAULT, "main.rue".to_owned());
    logical.insert(FileId::DEFAULT, "main.rue".to_owned());
    contents.push((FileId::DEFAULT, Arc::new(main_source.to_owned())));
    for index in 0..unrelated_modules {
        let file = FileId::new(index as u32 + 1);
        let path = format!("unrelated{index}.rue");
        physical.insert(file, path.clone());
        logical.insert(file, path.clone());
        contents.push((
            file,
            Arc::new(format!(
                "fn unrelated{index}() -> i32 {{ {index} }}{suffix}"
            )),
        ));
    }
    let metadata = SourceMetadata::new(FileId::DEFAULT, physical, logical)
        .expect("unrelated module metadata is valid");
    SourceSnapshot::new(metadata, contents).expect("unrelated module snapshot is valid")
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// Structural counters read from one cold `canonical_semantic` compile. Every
/// field is a work counter; not one is a clock or an allocation total.
#[derive(Debug, Clone, Copy)]
struct Measure {
    /// Cold reached-body analyses that paid the declaration-context cost.
    cold_bodies: usize,
    /// Declaration prefixes actually built for the request (RUE-1135). The
    /// prepare/project/install/endpoint stages are charged here, once per
    /// declaration base rather than once per body, so this is the counter that
    /// says how many times the O(declarations) prefix really ran.
    declaration_prefixes: usize,
    /// Σ_body declaration-shell predeclarations (the per-body "prepare" term),
    /// sourced from the prepare stage's own output.
    shells_total: usize,
    /// Σ_body durable declaration records the project stage actually joined (the
    /// per-body "project" term), sourced from the projector's returned work.
    projections_total: usize,
    /// Σ_body declaration semantics installed (the "install" term), sourced from
    /// the install stage's recorded `durable_payloads_installed`, charged only
    /// when the install actually ran.
    semantics_total: usize,
    /// Σ_body stable-identity/body-owner/module endpoints the endpoint stage
    /// actually installed (the per-body "endpoint" term).
    endpoints_total: usize,
    /// AIR instructions produced — genuine per-body body work, independent of the
    /// unrelated-declaration universe.
    air_instructions: usize,
    body_analyses_computed: usize,
    body_analyses_reused: usize,
    body_analyses_invalidated: usize,
    cfg_builds: usize,
    cfg_imports: usize,
    declarations_inspected: usize,
    modules_registered: usize,
    rir_indexes_constructed: usize,
    rir_instructions_visited: usize,
    durable_source_records_inspected: usize,
    durable_source_records_copied: usize,
}

impl Measure {
    /// Compile `corpus` cold in a fresh session and read its counters.
    fn cold(corpus: &Corpus) -> Self {
        let mut session = CompilerSession::new();
        session.update(&corpus.snapshot()).into_result().unwrap();
        let output = session
            .canonical_semantic(&CompileOptions::default())
            .expect("synthetic corpus compiles");
        Self::from_work(&output.work())
    }

    fn cold_snapshot(snapshot: SourceSnapshot) -> Self {
        let mut session = CompilerSession::new();
        session.update(&snapshot).into_result().unwrap();
        let output = session
            .canonical_semantic(&CompileOptions::default())
            .expect("synthetic snapshot compiles");
        Self::from_work(&output.work())
    }

    fn from_work(work: &CanonicalSemanticWork) -> Self {
        let body = &work.body_analysis;
        let ctx = &body.per_body_declaration_context;
        Self {
            // Cold reached bodies. Since RUE-1135 the per-body epoch is derived
            // from one request-scoped base, so the count of bodies that paid for
            // an epoch is the derivation count; `cold_body_preparations` counts
            // the prefixes, which is now a per-request quantity.
            cold_bodies: ctx.body_epochs_derived,
            declaration_prefixes: ctx.cold_body_preparations,
            shells_total: ctx.shells_prepared,
            projections_total: ctx.projections_performed,
            semantics_total: ctx.semantics_installed,
            endpoints_total: ctx.endpoints_installed,
            air_instructions: body.air_instructions_produced,
            body_analyses_computed: body.body_analyses_computed,
            body_analyses_reused: body.body_analyses_reused,
            body_analyses_invalidated: body.body_analyses_invalidated,
            cfg_builds: work.cfg.cfg_builds_attempted,
            cfg_imports: work.cfg.cfg_import_attempts,
            declarations_inspected: ctx.declarations_inspected,
            modules_registered: ctx.modules_registered,
            rir_indexes_constructed: ctx.rir_indexes_constructed,
            rir_instructions_visited: ctx.rir_instructions_visited,
            durable_source_records_inspected: ctx.durable_source_records_inspected,
            durable_source_records_copied: ctx.durable_source_records_copied,
        }
    }

    /// Per-body declaration-shell "prepare" work. Flat iff the per-body pipeline
    /// stops re-preparing the whole universe for every body.
    fn per_body_shells(&self) -> usize {
        self.shells_total / self.cold_bodies.max(1)
    }

    /// Per-body declaration "project" work. Flat under the same repair.
    fn per_body_projections(&self) -> usize {
        self.projections_total / self.cold_bodies.max(1)
    }

    /// Per-body declaration "install" work. Flat under the same repair.
    fn per_body_semantics(&self) -> usize {
        self.semantics_total / self.cold_bodies.max(1)
    }

    /// Per-body stable-identity/body-owner/module "endpoint" work. Flat under
    /// the same repair.
    fn per_body_endpoints(&self) -> usize {
        self.endpoints_total / self.cold_bodies.max(1)
    }
}

fn assert_exact_zero_slope(label: &str, baseline: usize, grown: usize) {
    assert_eq!(
        baseline, grown,
        "{label}: unrelated-universe growth changed a fixed-reach work counter"
    );
}

// ---------------------------------------------------------------------------
// Corpus-derived structural prediction (never a measured run)
// ---------------------------------------------------------------------------

/// Closed-form per-body declaration-context work predicted *only* from the
/// corpus knobs, for the current (unrepaired) whole-universe-per-body pipeline.
///
/// Every field is an exact formula in `(bodies, decls)` — never read back from a
/// measured compile — so a uniform regression that inflates a counter equally at
/// the baseline size and the grown size (e.g. an extra full-universe traversal
/// added to every body at every size) lands *outside* both the flat target and
/// this witness band and fails the row loudly, instead of scaling the measured
/// baseline in lockstep and staying green. The formulas are:
///
/// * declaration universe `U = bodies + decls + 1` (reached bodies + unrelated
///   declarations + `main`);
/// * `cold_bodies = bodies + 1` (each reached body plus `main` analyzed once);
/// * per-body prepare/project/install each traverse the whole universe: `U`;
/// * per-body endpoints install one stable-definition endpoint and one
///   body-owner endpoint per declaration plus one module endpoint: `2·U + 1`
///   (single-module corpus);
/// * total prepare work `shells_total = cold_bodies · U = (bodies+1)·(bodies+decls+1)`.
#[derive(Debug, Clone, Copy)]
struct CorpusWitness {
    cold_bodies: usize,
    per_body_shells: usize,
    per_body_projections: usize,
    per_body_semantics: usize,
    per_body_endpoints: usize,
    shells_total: usize,
}

impl CorpusWitness {
    fn predict(bodies: usize, decls: usize) -> Self {
        let universe = bodies + decls + 1;
        let cold_bodies = bodies + 1;
        Self {
            cold_bodies,
            per_body_shells: universe,
            per_body_projections: universe,
            per_body_semantics: universe,
            per_body_endpoints: 2 * universe + 1,
            shells_total: cold_bodies * universe,
        }
    }
}

// ---------------------------------------------------------------------------
// Two-envelope expected-failure discipline (single consistent mechanism)
// ---------------------------------------------------------------------------

/// Outcome of one scaling/identity row.
#[derive(Debug, Clone)]
enum Row {
    /// The repaired target envelope holds today — a hard pass.
    Met { label: String },
    /// The target does not hold yet; the documented known-bad witness holds
    /// within a tight band and the row is a tracked expected-failure.
    Tracked { label: String, issue: &'static str },
    /// A measurement gate fired and requires the named follow-up action.
    Activation { label: String },
}

impl Row {
    /// Lower-is-better two-envelope check for one tracked row.
    ///
    /// Asserts the measured value is EITHER within the repaired target envelope
    /// (`measured <= target + target_tol` — a hard PASS, including any repair
    /// strictly better than the target) OR inside the tight documented witness
    /// band (`[witness - witness_tol, witness + witness_tol]` — a tracked XFAIL
    /// naming `issue`). Anything worse than the witness band, or structurally
    /// between the two envelopes, panics: the row must be reconciled with the
    /// new reality before it can be edited.
    fn envelope(
        label: impl Into<String>,
        measured: usize,
        target: usize,
        target_tol: usize,
        witness: usize,
        witness_tol: usize,
        issue: &'static str,
    ) -> Row {
        let label = label.into();
        if measured <= target.saturating_add(target_tol) {
            return Row::Met { label };
        }
        let witness_lo = witness.saturating_sub(witness_tol);
        let witness_hi = witness.saturating_add(witness_tol);
        assert!(
            (witness_lo..=witness_hi).contains(&measured),
            "{label}: measured {measured} is neither within the repaired target \
             envelope (<= {target}+{target_tol}) nor the documented {issue} \
             witness band [{witness_lo}, {witness_hi}]. It is worse than the \
             witness or a partial/unrecognized shape — reconcile this row with \
             the current pipeline before editing it."
        );
        Row::Tracked { label, issue }
    }

    /// Hard linear invariant: `measured` must be within `tolerance` of the linear
    /// `expected` target. There is no known-bad witness — a non-linear result is
    /// a real regression, so this panics rather than tracking.
    fn linear_hard(
        label: impl Into<String>,
        measured: usize,
        expected: usize,
        tolerance: usize,
    ) -> Row {
        let label = label.into();
        let low = expected.saturating_sub(tolerance);
        let high = expected.saturating_add(tolerance);
        assert!(
            (low..=high).contains(&measured),
            "{label}: measured {measured} is not within the linear target \
             [{low}, {high}] — a non-linear body-analysis count is a regression"
        );
        Row::Met { label }
    }

    fn describe(&self) -> String {
        match self {
            Row::Met { label } => format!("  PASS  {label}"),
            Row::Tracked { label, issue } => format!("  XFAIL {label}  (tracked {issue})"),
            Row::Activation { label } => format!("  ACTIVATE  {label}"),
        }
    }
}

/// Collects row outcomes and prints one report block. The harness stays green
/// with expected-failures marked; a `Tracked` row is not a test failure.
struct Report {
    issue: &'static str,
    title: String,
    rows: Vec<Row>,
}

impl Report {
    fn new(title: impl Into<String>) -> Self {
        Self {
            issue: "RUE-1086",
            title: title.into(),
            rows: Vec::new(),
        }
    }

    fn rue_1090_gate(title: impl Into<String>) -> Self {
        Self {
            issue: "RUE-1090",
            title: title.into(),
            rows: Vec::new(),
        }
    }

    fn push(&mut self, row: Row) {
        self.rows.push(row);
    }

    fn emit(&self) {
        eprintln!("\n== {} {} ==", self.issue, self.title);
        for row in &self.rows {
            eprintln!("{}", row.describe());
        }
    }
}

// ---------------------------------------------------------------------------
// Full-parity warm/fresh oracle (single shared helper)
// ---------------------------------------------------------------------------

/// Render a full-fidelity diagnostic string for a failed compile: every error in
/// natural order, each with its kind, span, labels, notes, helps, and
/// suggestions (the `Debug` of `CompileError` is the diagnostic presentation
/// state). Comparing this warm-vs-fresh catches divergence in success/failure,
/// diagnostic ordering, spans, and labels.
fn render_diagnostics(errors: &CompileErrors) -> String {
    errors
        .iter()
        .map(|error| format!("{error:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_successful_output_body_presence(
    label: &str,
    output: &CanonicalSemanticOutput,
    states: &BTreeMap<String, Option<crate::BodyTransaction>>,
) {
    for function in output.functions() {
        let prefix = format!("{:?}:", function.semantic_identity);
        let matching = states
            .iter()
            .filter(|(identity, _)| identity.starts_with(&prefix))
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            1,
            "{label}: successful output body identity is absent or ambiguous: {:?}",
            function.semantic_identity
        );
        assert!(
            matching[0].1.is_some(),
            "{label}: successful output body has no observable retained transaction: {:?}",
            function.semantic_identity
        );
    }
}

fn body_query_identity(instance: &crate::FunctionInstanceKey, options: &CompileOptions) -> String {
    format!(
        "{:?}:{:?}",
        instance,
        crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: options.target,
            preview_features: crate::StablePreviewFeatures::new(&options.preview_features),
        }
    )
}

fn reachable_successful_body_identities(
    label: &str,
    output: &CanonicalSemanticOutput,
    states: &BTreeMap<String, Option<crate::BodyTransaction>>,
    options: &CompileOptions,
) -> BTreeSet<String> {
    let mut pending = output
        .functions()
        .iter()
        .map(|function| body_query_identity(&function.semantic_identity, options))
        .collect::<Vec<_>>();
    let mut reachable = BTreeSet::new();
    while let Some(identity) = pending.pop() {
        if !reachable.insert(identity.clone()) {
            continue;
        }
        let Some(Some(transaction)) = states.get(&identity) else {
            panic!("{label}: reachable successful body identity has no transaction: {identity}");
        };
        for reference in transaction.references().0.iter() {
            if let crate::body_query::BodyReference::Callable(instance) = reference {
                pending.push(body_query_identity(instance, options));
            }
        }
    }
    reachable
}

fn assert_reachable_body_key_set_parity(
    label: &str,
    warm_bodies: &BTreeMap<String, Option<crate::BodyTransaction>>,
    fresh_bodies: &BTreeMap<String, Option<crate::BodyTransaction>>,
) {
    assert_eq!(
        warm_bodies.keys().collect::<Vec<_>>(),
        fresh_bodies.keys().collect::<Vec<_>>(),
        "{label}: successful warm/fresh body-key sets differ"
    );
}

/// The one shared warm-vs-fresh oracle every edit row uses. Built on the exact
/// semantic-parity machinery (`CanonicalSemanticOutput::unstable_parity_snapshot`
/// — the same owned projection the in-tree cold-vs-reused differential compares),
/// it asserts full parity between a warm (incremental) rev2 compile and a fresh
/// rev2 compile:
///
/// * success vs failure agreement;
/// * on success, the full parity snapshot: functions, strings, type pool, bound
///   definitions, anonymous-nominal associations, every dependency surface,
///   full warnings (kind/spans/labels/order/presentation), and durable status;
/// * on failure, the full ordered diagnostic presentation.
fn assert_warm_fresh_parity(
    label: &str,
    warm_session: &mut CompilerSession,
    fresh_session: &mut CompilerSession,
    source: &SourceSnapshot,
    options: &CompileOptions,
    warm: &Result<Arc<CanonicalSemanticOutput>, CompileErrors>,
    fresh: &Result<Arc<CanonicalSemanticOutput>, CompileErrors>,
) {
    // The body-transaction family is the canonical production cache. Its
    // test-only snapshot includes stale keys as well as current terminals, so
    // successful parity must first narrow it to the identities reachable from
    // each output and its transaction edges.
    let warm_bodies = warm_session.retained_body_identity_states_for_test(options);
    let fresh_bodies = fresh_session.retained_body_identity_states_for_test(options);
    // A failed rooted semantic query may stop before an unchanged helper is
    // requested. Such a helper can remain observable from the warm cache while
    // having no fresh-side retained key; that is cache reachability, not a
    match (warm, fresh) {
        (Ok(warm), Ok(fresh)) => {
            // Successful results must expose the same complete reachable
            // identity universe. This includes output functions and every
            // callable body demanded by their transaction edges, while
            // ignoring retained but unreachable stale keys.
            let warm_reachable =
                reachable_successful_body_identities("warm", warm, &warm_bodies, options);
            let fresh_reachable =
                reachable_successful_body_identities("fresh", fresh, &fresh_bodies, options);
            let warm_reachable_states = warm_reachable
                .iter()
                .map(|identity| {
                    (
                        identity.clone(),
                        warm_bodies.get(identity).cloned().unwrap_or(None),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let fresh_reachable_states = fresh_reachable
                .iter()
                .map(|identity| {
                    (
                        identity.clone(),
                        fresh_bodies.get(identity).cloned().unwrap_or(None),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            assert_reachable_body_key_set_parity(
                label,
                &warm_reachable_states,
                &fresh_reachable_states,
            );
            for identity in warm_reachable.iter() {
                let warm_transaction = warm_bodies.get(identity).and_then(Option::as_ref);
                let fresh_transaction = fresh_bodies.get(identity).and_then(Option::as_ref);
                assert_eq!(
                    warm_transaction.is_some(),
                    fresh_transaction.is_some(),
                    "{label}: warm/fresh body identity presence diverged for {identity}\n"
                );
                if let (Some(warm_transaction), Some(fresh_transaction)) =
                    (warm_transaction, fresh_transaction)
                {
                    assert!(
                        crate::transaction_equal(warm_transaction, fresh_transaction),
                        "{label}: exact BodyTransaction diverged for {identity}\n warm={warm_transaction:?}\n fresh={fresh_transaction:?}"
                    );
                }
            }
            assert_eq!(
                warm.unstable_parity_snapshot(),
                fresh.unstable_parity_snapshot(),
                "{label}: warm/fresh semantic parity snapshot diverged"
            );

            // Keep the public output check as a consistency assertion, not as
            // the identity enumeration source. Every emitted function must
            // also have exactly one observable retained body transaction.
            assert_successful_output_body_presence(label, warm, &warm_bodies);
            assert_successful_output_body_presence(label, fresh, &fresh_bodies);

            assert_eq!(
                format!("{:?}", warm.functions()),
                format!("{:?}", fresh.functions()),
                "{label}: full AIR/CFG public artifacts diverged"
            );
            assert_eq!(
                warm.durable_ordinary_body_payloads(),
                fresh.durable_ordinary_body_payloads(),
                "{label}: durable ordinary bodies diverged"
            );
            assert_eq!(
                warm.durable_specialized_body_payloads(),
                fresh.durable_specialized_body_payloads(),
                "{label}: durable specialized bodies diverged"
            );
            assert_eq!(
                format!("{:?}", warm.durable_cfgs()),
                format!("{:?}", fresh.durable_cfgs()),
                "{label}: durable CFG artifacts diverged"
            );
            assert_eq!(
                format!("{:?}", warm.warnings()),
                format!("{:?}", fresh.warnings()),
                "{label}: ordered semantic warnings diverged"
            );
            assert_eq!(
                format!("{:?}", warm_session.latest_diagnostics()),
                format!("{:?}", fresh_session.latest_diagnostics()),
                "{label}: ordered diagnostic snapshots diverged"
            );

            let warm_executable = warm_session.oracle_executable(source, options);
            let fresh_executable = fresh_session.oracle_executable(source, options);
            match (warm_executable, fresh_executable) {
                (Ok(warm), Ok(fresh)) => {
                    assert_eq!(warm.elf, fresh.elf, "{label}: executable bytes diverged");
                    assert_eq!(
                        format!("{:?}", warm.warnings),
                        format!("{:?}", fresh.warnings),
                        "{label}: executable warnings diverged"
                    );
                }
                (Err(warm), Err(fresh)) => assert_eq!(
                    render_diagnostics(&warm),
                    render_diagnostics(&fresh),
                    "{label}: executable failure diagnostics diverged"
                ),
                (Ok(_), Err(fresh)) => panic!(
                    "{label}: warm executable succeeded but fresh failed:\n{}",
                    render_diagnostics(&fresh)
                ),
                (Err(warm), Ok(_)) => panic!(
                    "{label}: warm executable failed but fresh succeeded:\n{}",
                    render_diagnostics(&warm)
                ),
            }
        }
        (Err(warm), Err(fresh)) => {
            assert_eq!(
                render_diagnostics(warm),
                render_diagnostics(fresh),
                "{label}: warm/fresh failure diagnostics diverged"
            );
            assert_eq!(
                format!("{:?}", warm_session.latest_diagnostics()),
                format!("{:?}", fresh_session.latest_diagnostics()),
                "{label}: ordered failure diagnostic snapshots diverged"
            );
            // A failed rooted semantic query may stop before unchanged helper
            // bodies are requested. Compare deterministic failures demanded
            // by both sides, preserving early-stop behavior for helpers that
            // exist only in one retained family.
            let warm_failures = warm_bodies
                .iter()
                .filter(|(_, transaction)| {
                    matches!(
                        transaction,
                        Some(crate::BodyTransaction::DeterministicFailure { .. })
                    )
                })
                .map(|(identity, transaction)| {
                    (identity.clone(), transaction.as_ref().unwrap().clone())
                })
                .collect::<BTreeMap<_, _>>();
            let fresh_failures = fresh_bodies
                .iter()
                .filter(|(_, transaction)| {
                    matches!(
                        transaction,
                        Some(crate::BodyTransaction::DeterministicFailure { .. })
                    )
                })
                .map(|(identity, transaction)| {
                    (identity.clone(), transaction.as_ref().unwrap().clone())
                })
                .collect::<BTreeMap<_, _>>();
            for identity in warm_failures
                .keys()
                .filter(|identity| fresh_failures.contains_key(*identity))
            {
                assert!(
                    crate::transaction_equal(&warm_failures[identity], &fresh_failures[identity]),
                    "{label}: exact failed BodyTransaction diverged for {identity}"
                );
            }
        }
        (Ok(_), Err(fresh)) => panic!(
            "{label}: warm compiled but fresh failed:\n{}",
            render_diagnostics(fresh)
        ),
        (Err(warm), Ok(_)) => panic!(
            "{label}: warm failed but fresh compiled:\n{}",
            render_diagnostics(warm)
        ),
    }
}

// ---------------------------------------------------------------------------
// CI vs bench sizing (matrix mode only)
// ---------------------------------------------------------------------------

/// The larger 10k-per-axis corpus runs only when `RUE_SCALING_LARGE=1`. The
/// dedicated matrix target runs the bounded 100/1k subset; the huge sizes stay
/// behind this explicit flag.
#[cfg(scaling_matrix)]
fn large_sizes_enabled() -> bool {
    std::env::var_os("RUE_SCALING_LARGE").is_some_and(|v| v == "1")
}

/// The reached-body / declaration size ladder for the matrix target.
#[cfg(scaling_matrix)]
fn matrix_size_ladder() -> Vec<usize> {
    if large_sizes_enabled() {
        vec![100, 1_000, 10_000]
    } else {
        vec![100, 1_000]
    }
}

// ---------------------------------------------------------------------------
// Scaling-row logic (shared by the small unit smoke and the heavy matrix)
// ---------------------------------------------------------------------------

/// The RUE-1090 decision produced by an observed baseline/grown counter pair.
///
/// The frozen decision rule is about slope, not an absolute post-identity-cut
/// count: a new implementation may legitimately have a different constant
/// amount of per-body work. Any non-flat per-body count as unrelated declarations
/// grow activates RUE-1091 for investigation; only an exact flat ratio cancels
/// it. These are deterministic integer counters, so the comparison has zero
/// tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rue1090GateVerdict {
    Flat,
    ActivateRue1091,
}

impl Rue1090GateVerdict {
    fn combine(self, other: Self) -> Self {
        if matches!(self, Self::ActivateRue1091) || matches!(other, Self::ActivateRue1091) {
            Self::ActivateRue1091
        } else {
            Self::Flat
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::Flat => "CANCEL RUE-1091 (all gated counters flat)",
            Self::ActivateRue1091 => "ACTIVATE RUE-1091 (non-flat per-body count observed)",
        }
    }
}

/// Compare exact per-body ratios without normalizing them through integer
/// division. This matters even though the current corpus keeps the number of
/// cold bodies fixed: a future pipeline change must not hide a non-flat ratio
/// merely because two truncated integer quotients happen to agree.
fn rue_1090_gate_verdict(
    baseline_total: usize,
    baseline_bodies: usize,
    grown_total: usize,
    grown_bodies: usize,
) -> Rue1090GateVerdict {
    assert!(
        baseline_bodies > 0 && grown_bodies > 0,
        "per-body denominator is zero"
    );
    let baseline_scaled = baseline_total as u128 * grown_bodies as u128;
    let grown_scaled = grown_total as u128 * baseline_bodies as u128;
    if grown_scaled == baseline_scaled {
        Rue1090GateVerdict::Flat
    } else {
        Rue1090GateVerdict::ActivateRue1091
    }
}

/// Render one self-contained audit row. Historical formulas remain in the
/// output for comparison only; they do not participate in the RUE-1090 verdict.
fn rue_1090_audit_line(
    counter: &str,
    bodies: usize,
    baseline_decls: usize,
    grown_decls: usize,
    baseline_total: usize,
    baseline_bodies: usize,
    grown_total: usize,
    grown_bodies: usize,
    historical_baseline: usize,
    historical_grown: usize,
) -> (Rue1090GateVerdict, String) {
    let verdict = rue_1090_gate_verdict(baseline_total, baseline_bodies, grown_total, grown_bodies);
    let slope_numerator = grown_total as i128 * baseline_bodies as i128
        - baseline_total as i128 * grown_bodies as i128;
    let decision = match verdict {
        Rue1090GateVerdict::Flat => "FLAT",
        Rue1090GateVerdict::ActivateRue1091 => "ACTIVATE RUE-1091",
    };
    (
        verdict,
        format!(
            "RUE-1090 {decision}: per-body {counter} @ {bodies} bodies; \
             baseline={baseline_total}/{baseline_bodies} ({baseline_decls} decls), \
             grown={grown_total}/{grown_bodies} ({grown_decls} decls), \
             exact_ratio_slope_numerator={slope_numerator:+}; \
             historical_prediction={historical_baseline}->{historical_grown} (informational)",
        ),
    )
}

/// Keep a hard ceiling at the historical known-bad witness while RUE-1090
/// determines the *direction* of the follow-up. This is intentionally
/// independent of the exact-flat verdict: a result below the old witness may
/// still be non-flat and activate RUE-1091, while a second whole-universe pass
/// per body must fail rather than being recorded as ordinary activation.
///
/// The `+2` slack is the existing historical witness band used by the former
/// fixed-bodies envelope. It is a tripwire only, not a new target threshold.
const RUE_1090_HISTORICAL_WITNESS_TOLERANCE: usize = 2;

fn rue_1090_historical_witness_ceiling_line(
    counter: &str,
    measured_total: usize,
    measured_bodies: usize,
    historical_per_body_witness: usize,
) -> String {
    assert!(measured_bodies > 0, "per-body denominator is zero");
    let ceiling_per_body = historical_per_body_witness
        .checked_add(RUE_1090_HISTORICAL_WITNESS_TOLERANCE)
        .expect("historical witness ceiling overflow");
    let ceiling_total = ceiling_per_body
        .checked_mul(measured_bodies)
        .expect("historical witness total overflow");
    assert!(
        measured_total <= ceiling_total,
        "RUE-1090 historical witness ceiling tripped for {counter}: \
         measured={measured_total}/{measured_bodies}, \
         ceiling={ceiling_per_body} per body ({ceiling_total} total; \
         historical witness {historical_per_body_witness}+{RUE_1090_HISTORICAL_WITNESS_TOLERANCE})"
    );
    format!(
        "RUE-1090 historical tripwire: per-body {counter}={measured_total}/{measured_bodies} \
         <= {ceiling_per_body} (historical witness {historical_per_body_witness} \
         + {RUE_1090_HISTORICAL_WITNESS_TOLERANCE})",
    )
}

/// RUE-1121's post-repair acceptance row for a declaration-context counter.
///
/// The RUE-1090 decision remains its exact-ratio verdict below. This row is the
/// complementary acceptance assertion: with the same reached-body topology,
/// growing unrelated declarations must leave this observable counter exactly
/// flat. Until RUE-1091 repairs the shared declaration context, the current
/// whole-universe witness records a controlled expected failure. Once repaired,
/// the same row becomes a hard target pass without duplicating the test.
///
/// `witness` is the documented known-bad TOTAL, which differs by counter
/// because the counters are no longer charged the same way. A counter still
/// paid per body witnesses at `universe · cold_bodies`; the RUE-1132 projection
/// is charged once per request and witnesses at `universe`. Passing the total
/// rather than a per-body value keeps that difference explicit at each call
/// site instead of hiding it in a uniform multiply here.
fn rue_1121_exact_flat_context_row(
    counter: &str,
    baseline_total: usize,
    baseline_bodies: usize,
    grown_total: usize,
    grown_bodies: usize,
    witness: usize,
) -> Row {
    assert_eq!(
        grown_bodies, baseline_bodies,
        "RUE-1121 fixed-body corpus changed its body-preparation topology"
    );
    Row::envelope(
        format!(
            "RUE-1121 exact-flat {counter}: warm-independent cold {} -> {} \
             across {} unchanged body preparations (target {}, witness {})",
            baseline_total, grown_total, grown_bodies, baseline_total, witness,
        ),
        grown_total,
        baseline_total,
        0,
        witness,
        0,
        "RUE-1091",
    )
}

/// Axis: unrelated declarations grow while reached bodies stay fixed.
///
/// This is the RUE-1090 activation measurement. It compares the observed
/// per-body projection, installation, and endpoint counters at each size with
/// the observed baseline. The historical whole-universe formulas are printed as
/// context only; they are deliberately not an acceptance envelope.
fn run_fixed_bodies_growing_declarations(bodies: usize, ladder: &[usize]) {
    let baseline_decls = ladder[0];
    let historical_baseline = CorpusWitness::predict(bodies, baseline_decls);
    let mut report = Report::rue_1090_gate(format!(
        "gate: fixed {bodies} bodies, growing unrelated declarations"
    ));

    // A body's real AIR work is fixed by the reached bodies alone; retain that
    // independent hard invariant alongside the declaration-context gate.
    let baseline = Measure::cold(&Corpus::new(bodies, baseline_decls));
    let baseline_air = baseline.air_instructions;
    report.push(Row::Met {
        label: format!(
            "RUE-1090 raw baseline @ {bodies} bodies, {baseline_decls} decls: \
             per_body_project={}/{} per_body_install={}/{} per_body_endpoints={}/{}",
            baseline.projections_total,
            baseline.cold_bodies,
            baseline.semantics_total,
            baseline.cold_bodies,
            baseline.endpoints_total,
            baseline.cold_bodies,
        ),
    });
    report.push(Row::linear_hard(
        format!(
            "RUE-1121 baseline cold body preparations @ {bodies} bodies, \
             {baseline_decls} decls: {} (corpus target {})",
            baseline.cold_bodies, historical_baseline.cold_bodies,
        ),
        baseline.cold_bodies,
        historical_baseline.cold_bodies,
        0,
    ));

    let mut overall = Rue1090GateVerdict::Flat;

    for &decls in ladder.iter().skip(1) {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        let historical_grown = CorpusWitness::predict(bodies, decls);
        for (counter, baseline_value, grown_value) in [
            (
                "body analyses computed",
                baseline.body_analyses_computed,
                grown.body_analyses_computed,
            ),
            (
                "body analyses reused",
                baseline.body_analyses_reused,
                grown.body_analyses_reused,
            ),
            (
                "body analyses invalidated",
                baseline.body_analyses_invalidated,
                grown.body_analyses_invalidated,
            ),
            ("CFG builds", baseline.cfg_builds, grown.cfg_builds),
            ("CFG imports", baseline.cfg_imports, grown.cfg_imports),
            (
                "AIR instructions",
                baseline.air_instructions,
                grown.air_instructions,
            ),
        ] {
            assert_exact_zero_slope(counter, baseline_value, grown_value);
        }
        for (counter, baseline_value, grown_value) in [
            (
                "declarations inspected",
                baseline.declarations_inspected,
                grown.declarations_inspected,
            ),
            (
                "modules registered",
                baseline.modules_registered,
                grown.modules_registered,
            ),
            (
                "RIR indexes constructed",
                baseline.rir_indexes_constructed,
                grown.rir_indexes_constructed,
            ),
            (
                "RIR instructions visited",
                baseline.rir_instructions_visited,
                grown.rir_instructions_visited,
            ),
            (
                "durable source records inspected",
                baseline.durable_source_records_inspected,
                grown.durable_source_records_inspected,
            ),
            (
                "durable source records copied",
                baseline.durable_source_records_copied,
                grown.durable_source_records_copied,
            ),
        ] {
            report.push(Row::Met {
                label: format!(
                    "production work {counter}: baseline={baseline_value}, grown={grown_value}"
                ),
            });
        }
        // The fixed-body source topology is itself an exact acceptance fact:
        // unrelated declarations add no reached bodies, so cold preparation
        // count cannot hide a context regression by changing its denominator.
        report.push(Row::linear_hard(
            format!(
                "RUE-1121 exact-flat cold body preparations @ {bodies} bodies, \
                 +{} decls: {} (corpus target {})",
                decls - baseline_decls,
                grown.cold_bodies,
                historical_grown.cold_bodies,
            ),
            grown.cold_bodies,
            historical_grown.cold_bodies,
            0,
        ));
        // RUE-1135: one declaration base serves the whole request, so the
        // O(declarations) prefix runs exactly once no matter how many bodies
        // the request reaches. A body that rebuilt its own epoch would push
        // this straight back to `cold_bodies`.
        report.push(Row::linear_hard(
            format!(
                "RUE-1135 declaration prefixes @ {bodies} bodies, +{} decls: {} \
                 across {} cold bodies",
                decls - baseline_decls,
                grown.declaration_prefixes,
                grown.cold_bodies,
            ),
            grown.declaration_prefixes,
            1,
            0,
        ));
        // RUE-1121 acceptance rows consume only stage-sourced counters. The
        // RUE-1090 verdict below deliberately remains limited to its original
        // project/install/endpoint exact-ratio decision.
        // Each witness is the known-bad TOTAL for that counter's charging shape.
        // Every one of these stages is now charged once per request — the
        // RUE-1132 projection is computed once and shared, and since RUE-1135 the
        // whole declaration epoch is built once and each body derives from it —
        // so each witness is a single `universe`, never `universe · cold_bodies`.
        // These rows therefore remain tracked RUE-1091 expected-failures — one
        // whole-universe prefix is still not exactly flat in unrelated
        // declarations — but their witnesses record the repaired shape, so a
        // regression back to per-body construction lands at
        // `universe · cold_bodies`, blows past the band, and panics instead of
        // being absorbed.
        for (counter, baseline_total, grown_total, witness) in [
            (
                "shells prepared",
                baseline.shells_total,
                grown.shells_total,
                historical_grown.per_body_shells,
            ),
            (
                "projections",
                baseline.projections_total,
                grown.projections_total,
                historical_grown.per_body_projections,
            ),
            (
                "semantics installed",
                baseline.semantics_total,
                grown.semantics_total,
                historical_grown.per_body_semantics,
            ),
            (
                "endpoints",
                baseline.endpoints_total,
                grown.endpoints_total,
                historical_grown.per_body_endpoints,
            ),
        ] {
            report.push(rue_1121_exact_flat_context_row(
                counter,
                baseline_total,
                baseline.cold_bodies,
                grown_total,
                grown.cold_bodies,
                witness,
            ));
        }
        // A worsening beyond the historical unrepaired witness is a regression,
        // not an ordinary RUE-1090 activation. Keep prepare/shell work here as
        // well: it is outside the three-counter verdict but still catches a
        // duplicated whole-universe declaration traversal.
        for (counter, measured_total, historical_per_body_witness) in [
            (
                "prepare (shells)",
                grown.shells_total,
                historical_grown.per_body_shells,
            ),
            (
                "projection",
                grown.projections_total,
                historical_grown.per_body_projections,
            ),
            (
                "install",
                grown.semantics_total,
                historical_grown.per_body_semantics,
            ),
            (
                "endpoints",
                grown.endpoints_total,
                historical_grown.per_body_endpoints,
            ),
        ] {
            report.push(Row::Met {
                label: rue_1090_historical_witness_ceiling_line(
                    counter,
                    measured_total,
                    grown.cold_bodies,
                    historical_per_body_witness,
                ),
            });
        }
        for (
            counter,
            baseline_value,
            grown_value,
            historical_baseline_value,
            historical_grown_value,
        ) in [
            (
                "projection",
                baseline.projections_total,
                grown.projections_total,
                historical_baseline.per_body_projections,
                historical_grown.per_body_projections,
            ),
            (
                "install",
                baseline.semantics_total,
                grown.semantics_total,
                historical_baseline.per_body_semantics,
                historical_grown.per_body_semantics,
            ),
            (
                "endpoints",
                baseline.endpoints_total,
                grown.endpoints_total,
                historical_baseline.per_body_endpoints,
                historical_grown.per_body_endpoints,
            ),
        ] {
            let (verdict, line) = rue_1090_audit_line(
                counter,
                bodies,
                baseline_decls,
                decls,
                baseline_value,
                baseline.cold_bodies,
                grown_value,
                grown.cold_bodies,
                historical_baseline_value,
                historical_grown_value,
            );
            overall = overall.combine(verdict);
            report.push(match verdict {
                Rue1090GateVerdict::Flat => Row::Met { label: line },
                Rue1090GateVerdict::ActivateRue1091 => Row::Activation { label: line },
            });
        }

        // Hard invariant that holds today: AIR (real per-body body work) is
        // invariant to unrelated declarations. Same reached bodies => same AIR.
        assert_eq!(
            grown.air_instructions, baseline_air,
            "unrelated declarations must not change real per-body AIR work"
        );
    }

    let final_label = format!("RUE-1090 VERDICT: {}", overall.summary());
    report.push(match overall {
        Rue1090GateVerdict::Flat => Row::Met { label: final_label },
        Rue1090GateVerdict::ActivateRue1091 => Row::Activation { label: final_label },
    });

    report.emit();
}

/// Frozen per-reached-body AIR instruction constants for the synthetic corpus.
///
/// Real per-body body work is not analytically derivable from the corpus knobs
/// — it is the codegen of `fn b{i}() -> i32 { i }` and `main`'s accumulation
/// loop — so the exact closed form is captured once as an explicitly frozen
/// external constant. Measured 2026-07-22 on this corpus, total AIR is exactly
/// `AIR_PER_REACHED_BODY · cold_bodies + AIR_BODY_WORK_CONSTANT` at every size
/// (verified at 21, 41 cold bodies and the 2-body floor, and invariant to
/// unrelated declarations). Freezing this, rather than scaling a same-process
/// measured baseline by `factor`, means a uniform per-body body-work regression
/// (an extra traversal inflating AIR at *every* size) lands outside the tight
/// band and fails instead of scaling the target in lockstep.
const AIR_PER_REACHED_BODY: usize = 6;
/// `main`'s accumulation-loop tail contributes a single size-independent
/// instruction on top of the per-body term.
const AIR_BODY_WORK_CONSTANT: usize = 1;
/// Tight fixed slack around the exact frozen AIR closed form.
const AIR_BODY_WORK_TOLERANCE: usize = 2;

/// Axis: reached bodies grow while unrelated declarations stay fixed.
///
/// Hard invariant (holds today): the NUMBER of body analyses is linear in
/// reached bodies — each reached body is analyzed exactly once (`cold_bodies =
/// bodies + 1`, corpus-derived).
///
/// Every per-body declaration-context counter (prepare/project/install/endpoint)
/// is asserted hard against its corpus-derived value (`U = bodies + decls + 1`
/// for prepare/project/install, `2·U + 1` for endpoints): on this axis those
/// values grow with `bodies`, and asserting the exact closed form catches a
/// uniform regression that a scaled measured baseline would hide.
///
/// Tracked (RUE-1090): TOTAL declaration-context work should be linear in
/// reached bodies, but today it is quadratic (each body re-installs the whole
/// universe), so `shells_total` grows as `(bodies+1)·(bodies+decls+1)`. Both the
/// linear target and the quadratic witness are corpus-derived, never a measured
/// run. Strong invariant (Finding 5): total AIR body work stays linear in
/// reached bodies, checked against the frozen per-body AIR constant.
fn run_fixed_declarations_growing_bodies(decls: usize, ladder: &[usize]) {
    let base_bodies = ladder[0];
    let baseline = Measure::cold(&Corpus::new(base_bodies, decls));
    let base_predicted = CorpusWitness::predict(base_bodies, decls);
    let mut report = Report::new(format!(
        "scaling: fixed {decls} declarations, growing bodies"
    ));

    eprintln!(
        "\n== RUE-1086 BASELINE (RUE-1090 gate): per-body declaration-context counters ==\n  \
         {:>6} bodies × {:>4} decls | cold_bodies={:>5} per_body_shells={:>5} \
         per_body_project={:>5} per_body_semantics={:>5} per_body_endpoints={:>5} \
         shells_total={:>9}",
        base_bodies,
        decls,
        baseline.cold_bodies,
        baseline.per_body_shells(),
        baseline.per_body_projections(),
        baseline.per_body_semantics(),
        baseline.per_body_endpoints(),
        baseline.shells_total,
    );

    for &bodies in ladder.iter().skip(1) {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        let factor = bodies / base_bodies;
        // Corpus-derived predictions at this size — nothing read from `grown`.
        let predicted = CorpusWitness::predict(bodies, decls);

        eprintln!(
            "  {:>6} bodies × {:>4} decls | cold_bodies={:>5} per_body_shells={:>5} \
             per_body_project={:>5} per_body_semantics={:>5} per_body_endpoints={:>5} \
             shells_total={:>9} air={:>7}",
            bodies,
            decls,
            grown.cold_bodies,
            grown.per_body_shells(),
            grown.per_body_projections(),
            grown.per_body_semantics(),
            grown.per_body_endpoints(),
            grown.shells_total,
            grown.air_instructions,
        );

        // Hard: analysis COUNT is linear in reached bodies, corpus-derived as
        // `bodies + 1` (each reached body plus `main`), NOT `baseline * factor`.
        report.push(Row::linear_hard(
            format!(
                "body-analysis count @ {decls} decls: {bodies} bodies cold={} (corpus target {})",
                grown.cold_bodies, predicted.cold_bodies
            ),
            grown.cold_bodies,
            predicted.cold_bodies,
            2,
        ));
        assert!(
            grown.cold_bodies >= bodies,
            "every reached body must be analyzed once: {} < {bodies}",
            grown.cold_bodies
        );

        // RUE-1135 changed the SHAPE of the prepare/install/endpoint counters
        // exactly as RUE-1132 changed the projection: the whole declaration
        // epoch is body-invariant, so it is built once per request and every
        // body derives its epoch from that base. Each of these is therefore a
        // TOTAL row against one universe rather than a per-body row — strictly
        // stronger, because a regression to per-body construction lands at
        // `U · cold_bodies` and fails here immediately instead of being hidden
        // by the integer-division quotient at large body counts.
        report.push(Row::linear_hard(
            format!(
                "RUE-1135 shared prepare (shells) @ {decls} decls: {bodies} bodies \
                 total={} (corpus U={}, charged once per request)",
                grown.shells_total, predicted.per_body_shells
            ),
            grown.shells_total,
            predicted.per_body_shells,
            1,
        ));
        // The declaration base itself: one per request, whatever the body count.
        report.push(Row::linear_hard(
            format!(
                "RUE-1135 declaration prefixes @ {decls} decls: {bodies} bodies \
                 prefixes={} across {} cold bodies",
                grown.declaration_prefixes, grown.cold_bodies
            ),
            grown.declaration_prefixes,
            1,
            0,
        ));
        // RUE-1132 changed the SHAPE of this counter, so it is no longer a
        // per-body linear row. The declaration projection is body-invariant and
        // is now computed once per request and shared, so total projection work
        // must stay at one universe REGARDLESS of how many bodies are reached.
        // Asserting the total (not the per-body quotient) is strictly stronger
        // than the old linear row: a regression to per-body projection lands at
        // `U · cold_bodies` and fails here immediately, and the row cannot be
        // satisfied by an integer-division artifact at large body counts.
        report.push(Row::linear_hard(
            format!(
                "RUE-1132 shared projection @ {decls} decls: {bodies} bodies \
                 total={} (corpus U={}, charged once per request)",
                grown.projections_total, predicted.per_body_projections
            ),
            grown.projections_total,
            predicted.per_body_projections,
            1,
        ));
        report.push(Row::linear_hard(
            format!(
                "RUE-1135 shared install (semantics) @ {decls} decls: {bodies} bodies \
                 total={} (corpus U={}, charged once per request)",
                grown.semantics_total, predicted.per_body_semantics
            ),
            grown.semantics_total,
            predicted.per_body_semantics,
            1,
        ));
        report.push(Row::linear_hard(
            format!(
                "RUE-1135 shared endpoints @ {decls} decls: {bodies} bodies \
                 total={} (corpus 2U+1={}, charged once per request)",
                grown.endpoints_total, predicted.per_body_endpoints
            ),
            grown.endpoints_total,
            predicted.per_body_endpoints,
            1,
        ));

        // Hard (Finding 5): real per-body body work (AIR) is linear in reached
        // bodies. Checked against the frozen per-body AIR constant, so total AIR
        // must be `cold_bodies · AIR_PER_REACHED_BODY` within a tight band — a
        // corpus/frozen bound, not a scaled measured baseline.
        let air_target = grown.cold_bodies * AIR_PER_REACHED_BODY + AIR_BODY_WORK_CONSTANT;
        report.push(Row::linear_hard(
            format!(
                "total AIR body work @ {decls} decls: {bodies} bodies air={} (frozen {air_target})",
                grown.air_instructions,
            ),
            grown.air_instructions,
            air_target,
            AIR_BODY_WORK_TOLERANCE,
        ));

        // Tracked (RUE-1090): total declaration-context work should be linear in
        // reached bodies but is quadratic today. The linear target scales the
        // *corpus-predicted* base total (not a measured run) by `factor`; the
        // witness is the exact quadratic closed form `(bodies+1)·(bodies+decls+1)`.
        let expected_linear_total = base_predicted.shells_total * factor;
        let witness_total = predicted.shells_total;
        report.push(Row::envelope(
            format!(
                "total declaration-context work @ {decls} decls: {bodies} bodies shells={} (linear target ~{expected_linear_total}, witness ~{witness_total})",
                grown.shells_total
            ),
            grown.shells_total,
            expected_linear_total,
            expected_linear_total / 10,
            witness_total,
            bodies + decls + 1,
            "RUE-1090",
        ));
    }

    report.emit();
}

/// Identity rows (RUE-1089). The identity/lookup installation a body performs
/// over the declaration universe should be invariant to declarations the body
/// never references. Measured as per-body install work at a FIXED reached-body
/// count across a growing declaration universe.
fn run_identity_invariant(bodies: usize, decl_ladder: &[usize]) {
    let mut report = Report::new(format!(
        "identity: per-body lookup invariant to unrelated declarations ({bodies} bodies)"
    ));

    // Flat target = corpus-predicted per-body install work with zero unrelated
    // declarations. The witness at each size is the corpus-predicted per-body
    // install with the declarations present. Both are closed forms in the corpus
    // knobs, never a measured run, so a uniform per-body regression fails.
    let target = CorpusWitness::predict(bodies, 0);
    for &decls in decl_ladder {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        let witness = CorpusWitness::predict(bodies, decls);
        report.push(Row::envelope(
            format!(
                "per-body identity install @ {bodies} bodies: +{decls} unrelated decls => {} (target {}, witness {})",
                grown.per_body_semantics(),
                target.per_body_semantics,
                witness.per_body_semantics
            ),
            grown.per_body_semantics(),
            target.per_body_semantics,
            2,
            witness.per_body_semantics,
            2,
            "RUE-1089",
        ));
    }

    report.emit();
}

// ---------------------------------------------------------------------------
// Small always-on unit smoke (default `rue-compiler-test`)
// ---------------------------------------------------------------------------

#[test]
fn scaling_smoke_fixed_bodies_growing_declarations() {
    // Genuinely small corpus (20 bodies, 20 -> 40 decls) so the default unit
    // target stays fast. Same observed-slope gate the matrix runs at 100/1k.
    run_fixed_bodies_growing_declarations(20, &[20, 40]);
}

#[test]
fn scaling_smoke_fixed_reach_growing_unrelated_modules_keeps_body_work_flat() {
    let baseline = Measure::cold_snapshot(unrelated_module_snapshot(0));
    for modules in [1, 2, 4] {
        let grown = Measure::cold_snapshot(unrelated_module_snapshot(modules));
        for (counter, baseline_value, grown_value) in [
            (
                "module-universe body analyses computed",
                baseline.body_analyses_computed,
                grown.body_analyses_computed,
            ),
            (
                "module-universe body analyses reused",
                baseline.body_analyses_reused,
                grown.body_analyses_reused,
            ),
            (
                "module-universe body analyses invalidated",
                baseline.body_analyses_invalidated,
                grown.body_analyses_invalidated,
            ),
            (
                "module-universe CFG builds",
                baseline.cfg_builds,
                grown.cfg_builds,
            ),
            (
                "module-universe CFG imports",
                baseline.cfg_imports,
                grown.cfg_imports,
            ),
            (
                "module-universe AIR instructions",
                baseline.air_instructions,
                grown.air_instructions,
            ),
        ] {
            assert_exact_zero_slope(counter, baseline_value, grown_value);
        }
        assert_eq!(
            grown.modules_registered,
            baseline.modules_registered + modules,
            "module registration work must account for every supplied module"
        );
        assert!(
            grown.rir_instructions_visited > baseline.rir_instructions_visited,
            "the RIR universe must expose added unrelated module instructions"
        );
    }
}

#[test]
fn rue_1090_gate_accepts_a_flat_changed_constant() {
    // The frozen rule is flatness, not equality with the historical 201-count
    // baseline. A producer-nominal cut may change the constant work amount.
    let (verdict, line) =
        rue_1090_audit_line("install", 100, 100, 1_000, 250, 1, 250, 1, 201, 1_101);

    assert_eq!(verdict, Rue1090GateVerdict::Flat);
    assert!(line.contains("baseline=250/1 (100 decls)"));
    assert!(line.contains("grown=250/1 (1000 decls)"));
    assert!(line.contains("exact_ratio_slope_numerator=+0"));
    assert!(line.contains("RUE-1090 FLAT"));
    assert!(line.contains("historical_prediction=201->1101 (informational)"));
}

#[test]
fn rue_1090_historical_witness_ceiling_allows_known_bad_work() {
    let line = rue_1090_historical_witness_ceiling_line("prepare (shells)", 1_101, 1, 1_101);

    assert!(line.contains("prepare (shells)=1101/1"));
    assert!(line.contains("<= 1103 (historical witness 1101 + 2)"));
}

#[test]
#[should_panic(expected = "RUE-1090 historical witness ceiling tripped")]
fn rue_1090_historical_witness_ceiling_rejects_a_second_full_traversal() {
    // Two whole-universe traversals exceed the inherited `witness + 2` band.
    let _ = rue_1090_historical_witness_ceiling_line("install", 2_202, 1, 1_101);
}

#[test]
fn rue_1090_gate_activates_on_a_positive_per_body_slope() {
    let (verdict, line) =
        rue_1090_audit_line("endpoints", 100, 100, 1_000, 250, 1, 251, 1, 403, 2_203);

    assert_eq!(verdict, Rue1090GateVerdict::ActivateRue1091);
    assert!(line.contains("exact_ratio_slope_numerator=+1"));
    assert!(line.contains("RUE-1090 ACTIVATE RUE-1091"));

    // This is also activating even though integer division would show 250 for
    // both ratios (`500 / 2` and `751 / 3`).
    assert_eq!(
        rue_1090_gate_verdict(500, 2, 751, 3),
        Rue1090GateVerdict::ActivateRue1091
    );
}

#[test]
fn rue_1090_gate_does_not_cancel_on_a_negative_per_body_slope() {
    let (verdict, line) =
        rue_1090_audit_line("projection", 100, 100, 1_000, 251, 1, 250, 1, 201, 1_101);

    assert_eq!(verdict, Rue1090GateVerdict::ActivateRue1091);
    assert!(line.contains("exact_ratio_slope_numerator=-1"));
    assert!(line.contains("RUE-1090 ACTIVATE RUE-1091"));
}

#[test]
fn scaling_smoke_fixed_declarations_growing_bodies() {
    // 20 -> 40 reached bodies at a fixed 20 declarations.
    run_fixed_declarations_growing_bodies(20, &[20, 40]);
}

#[test]
fn identity_per_body_lookup_invariant_to_unrelated_declarations() {
    run_identity_invariant(20, &[20, 40, 80]);
}

// ---------------------------------------------------------------------------
// Heavy structural matrix (dedicated `scaling-matrix-test` target only)
// ---------------------------------------------------------------------------

#[cfg(scaling_matrix)]
#[test]
fn scaling_matrix_fixed_bodies_growing_declarations() {
    run_fixed_bodies_growing_declarations(100, &matrix_size_ladder());
}

#[cfg(scaling_matrix)]
#[test]
fn scaling_matrix_fixed_declarations_growing_bodies() {
    run_fixed_declarations_growing_bodies(100, &matrix_size_ladder());
}

#[cfg(scaling_matrix)]
#[test]
fn scaling_matrix_identity_invariant() {
    run_identity_invariant(50, &[50, 200, 400]);
}

// ---------------------------------------------------------------------------
// Two-revision edit scenarios (share the warm/fresh parity oracle)
// ---------------------------------------------------------------------------

/// A two-revision edit scenario. `warm` walks rev1 -> rev2 in one session; the
/// oracle recompiles rev2 in a `fresh` session, asserts full parity, and returns
/// the warm rev2 work counters for the caller's invalidation assertions.
struct EditScenario {
    label: &'static str,
    rev1: Corpus,
    rev2: Corpus,
}

impl EditScenario {
    /// Run rev1 then rev2 warm, run rev2 fresh, assert the shared warm-vs-fresh
    /// parity oracle, and return the warm rev2 counters (plus a success flag for
    /// callers that key on it).
    fn run(&self, body_names: &[String]) -> (Option<Measure>, bool, BTreeSet<String>) {
        let options = CompileOptions::default();

        let mut warm = CompilerSession::new();
        warm.update(&self.rev1.snapshot()).into_result().unwrap();
        warm.canonical_semantic(&options).ok(); // rev1 is not oracled; only rev2.
        let rev1_origins = warm.retained_body_transaction_origins_for_test(body_names);
        warm.update(&self.rev2.snapshot()).into_result().unwrap();
        let warm_rev2 = warm.canonical_semantic(&options);
        let rev2_origins = warm.retained_body_transaction_origins_for_test(body_names);

        let mut fresh = CompilerSession::new();
        fresh.update(&self.rev2.snapshot()).into_result().unwrap();
        let fresh_rev2 = fresh.canonical_semantic(&options);

        let rev2_source = self.rev2.snapshot();
        assert_warm_fresh_parity(
            self.label,
            &mut warm,
            &mut fresh,
            &rev2_source,
            &options,
            &warm_rev2,
            &fresh_rev2,
        );

        let succeeded = warm_rev2.is_ok();
        let measure = warm_rev2
            .as_ref()
            .ok()
            .map(|output| Measure::from_work(&output.work()));
        (
            measure,
            succeeded,
            changed_body_origins(&rev1_origins, &rev2_origins),
        )
    }
}

fn changed_body_origins(
    before: &BTreeMap<String, u64>,
    after: &BTreeMap<String, u64>,
) -> BTreeSet<String> {
    before
        .keys()
        .chain(after.keys())
        .filter(|name| before.get(*name) != after.get(*name))
        .cloned()
        .collect()
}

fn corpus_body_names(reached_bodies: usize) -> Vec<String> {
    (0..reached_bodies)
        .map(|index| format!("b{index}"))
        .chain(std::iter::once("main".to_owned()))
        .collect()
}

/// Run one small source edit through the same production session path used by
/// the scaling rows. The exact oracle is deliberately shared by all of these
/// cases so a new edit shape cannot accidentally fall back to comparing only a
/// root semantic projection.
struct SourceEditOracle {
    changed: BTreeSet<String>,
    before_states: BTreeMap<String, Option<crate::BodyTransaction>>,
    warm_states: BTreeMap<String, Option<crate::BodyTransaction>>,
    fresh_states: BTreeMap<String, Option<crate::BodyTransaction>>,
}

fn run_source_edit(
    label: &str,
    rev1_source: &str,
    rev2_source: &str,
    body_names: &[&str],
) -> SourceEditOracle {
    let options = CompileOptions::default();
    let rev1 = SourceSnapshot::single("main.rue", rev1_source).unwrap();
    let rev2 = SourceSnapshot::single("main.rue", rev2_source).unwrap();
    let body_names = body_names
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();

    let mut warm = CompilerSession::new();
    warm.update(&rev1).into_result().unwrap();
    warm.canonical_semantic(&options).ok();
    let before = warm.retained_body_transaction_origins_for_test(&body_names);
    let before_states = warm.retained_body_identity_states_for_test(&options);
    warm.update(&rev2).into_result().unwrap();
    let warm_result = warm.canonical_semantic(&options);
    let after = warm.retained_body_transaction_origins_for_test(&body_names);
    let warm_states = warm.retained_body_identity_states_for_test(&options);

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2).into_result().unwrap();
    let fresh_result = fresh.canonical_semantic(&options);
    let fresh_states = fresh.retained_body_identity_states_for_test(&options);
    assert_warm_fresh_parity(
        label,
        &mut warm,
        &mut fresh,
        &rev2,
        &options,
        &warm_result,
        &fresh_result,
    );
    SourceEditOracle {
        changed: changed_body_origins(&before, &after),
        before_states,
        warm_states,
        fresh_states,
    }
}

fn named_state<'a>(
    states: &'a BTreeMap<String, Option<crate::BodyTransaction>>,
    name: &str,
) -> Option<&'a Option<crate::BodyTransaction>> {
    let name = format!("name: \"{name}\"");
    states
        .iter()
        .find(|(identity, _)| identity.contains(&name))
        .map(|(_, state)| state)
}

fn import_source(
    value: i32,
    epoch: u64,
) -> (SourceSnapshot, ImportDiscoveryContext, AcceptedReadManifest) {
    let context = ImportDiscoveryContext::new(epoch, "/p", None, "scaling-import").unwrap();
    let root = Arc::new("const a = @import(\"a.rue\"); fn main() -> i32 { a.value() }".to_owned());
    let imported = Arc::new(format!("pub fn value() -> i32 {{ {value} }}"));
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/p/main.rue",
        "/p/main.rue",
        PhysicalFileIdentity::new(1086, 1),
        FileMetadataFingerprint::new(root.len() as u64, epoch, epoch),
        root,
    )
    .unwrap();
    assembler
        .add_explicit(
            "/p/a.rue",
            "/p/a.rue",
            PhysicalFileIdentity::new(1086, 2),
            FileMetadataFingerprint::new(imported.len() as u64, epoch, epoch),
            imported,
        )
        .unwrap();
    (
        assembler.snapshot().unwrap(),
        context,
        assembler.accepted_read_manifest(),
    )
}

fn close_import_source(
    session: &mut CompilerSession,
    source: &SourceSnapshot,
    context: ImportDiscoveryContext,
    accepted_reads: AcceptedReadManifest,
) {
    let mut revision =
        begin_import_input_request(session, source, context.clone(), accepted_reads.clone())
            .unwrap();
    loop {
        let ledger = import_observation_ledger(session, revision).unwrap();
        let plan = session
            .stage_import_discovery(
                source,
                context.clone(),
                accepted_reads.shared_slice(),
                ledger.clone(),
            )
            .unwrap();
        let frontier = import_demand_frontier_for_roots(
            session,
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .unwrap();
        if frontier.requests().is_empty() {
            session.close_import_discovery(ledger).unwrap();
            return;
        }
        let observations = frontier
            .requests()
            .iter()
            .map(|request| {
                let read = accepted_reads
                    .iter()
                    .find(|read| read.requested_path() == request.requested_path())
                    .expect("the import fixture accepts every demanded read");
                let module = source
                    .files()
                    .find(|file| source.module_id(file.file_id) == Some(read.module()))
                    .expect("the accepted import belongs to the fixture snapshot");
                ImportObservation::accepted(
                    request.clone(),
                    AcceptedImportSource::new(
                        request.requested_path(),
                        read.canonical_path(),
                        read.metadata_identity(),
                        read.metadata_fingerprint(),
                        Arc::new(module.source.to_owned()),
                    )
                    .unwrap(),
                )
                .unwrap()
            })
            .collect();
        revision = publish_import_observation_batch(
            session,
            &frontier,
            source,
            accepted_reads.clone(),
            observations,
        )
        .unwrap();
    }
}

fn rue_1121_exact_recompute_set_row(
    label: impl Into<String>,
    measured: &BTreeSet<String>,
    target: BTreeSet<String>,
    witness: BTreeSet<String>,
) -> Row {
    let label = label.into();
    if measured == &target {
        return Row::Met {
            label: format!("{label}: exact recompute set {measured:?}"),
        };
    }
    assert_eq!(
        measured, &witness,
        "{label}: recomputed body identities are neither the exact repaired target \
         {target:?} nor the documented whole-universe witness {witness:?}"
    );
    Row::Tracked {
        label: format!("{label}: known-bad recompute set {measured:?}"),
        issue: "RUE-1091",
    }
}

/// The one flip point for RUE-1121 edit acceptance. `fresh` is measured in its
/// own session by the caller, never inferred from the warm result. The exact
/// fresh body count proves the corpus topology, and `target < fresh` proves a
/// repaired warm result is genuinely incremental rather than merely bounded.
///
/// Until RUE-1091 lands, the whole-universe witness is a controlled XFAIL. The
/// same row automatically becomes the hard, exact target once the repair makes
/// `warm == target`; no mechanism-specific counter or duplicate test is needed.
fn rue_1121_edit_recompute_row(
    label: impl Into<String>,
    warm: Measure,
    fresh: Measure,
    exact_recomputed_bodies: usize,
    fresh_body_count: usize,
) -> Row {
    let label = label.into();
    assert_eq!(
        fresh.cold_bodies, fresh_body_count,
        "{label}: fresh revision must prove the expected body topology"
    );
    assert!(
        exact_recomputed_bodies < fresh.cold_bodies,
        "{label}: the repaired exact target must be strictly cheaper than fresh"
    );
    Row::envelope(
        format!(
            "{label}: warm body transactions={} computed={} reused={} invalidated={} \
             (exact target {}, independently measured fresh {})",
            warm.cold_bodies,
            warm.body_analyses_computed,
            warm.body_analyses_reused,
            warm.body_analyses_invalidated,
            exact_recomputed_bodies,
            fresh.cold_bodies,
        ),
        warm.cold_bodies,
        exact_recomputed_bodies,
        0,
        fresh.cold_bodies,
        0,
        "RUE-1091",
    )
}

#[test]
fn invalidation_unrelated_declaration_added_keeps_bodies_green() {
    // Add one unrelated declaration. Every previously-green body terminal must
    // stay green: warm rev2 succeeds and equals a fresh rev2 compile (the shared
    // parity oracle inside `run` asserts that).
    let scenario = EditScenario {
        label: "unrelated declaration added",
        rev1: Corpus::new(40, 5),
        rev2: Corpus::new(40, 6),
    };
    assert_ne!(
        scenario.rev1.source(),
        scenario.rev2.source(),
        "the unrelated-declaration edit must change source"
    );
    assert_eq!(
        scenario.rev2.source().matches("d5").count(),
        1,
        "the added declaration must occur exactly once and have no body consumer"
    );
    let body_names = corpus_body_names(40);
    let (warm, succeeded, recomputed) = scenario.run(&body_names);
    assert!(succeeded, "adding an unrelated declaration must compile");
    let warm = warm.unwrap();

    // No reached body references the added declaration. The exact repaired
    // target is therefore zero transactions, strictly less than the separately
    // measured fresh 41-body compile.
    let fresh = Measure::cold(&scenario.rev2);
    let mut report = Report::new("invalidation: unrelated declaration added");
    report.push(rue_1121_exact_recompute_set_row(
        "unrelated declaration invalidates the exact body set",
        &recomputed,
        BTreeSet::new(),
        body_names.into_iter().collect(),
    ));
    report.push(rue_1121_edit_recompute_row(
        "unrelated declaration invalidates zero previously-green bodies",
        warm,
        fresh,
        0,
        41,
    ));
    report.emit();
}

#[test]
fn invalidation_single_body_edit_declaration_work_does_not_rerun() {
    // Edit exactly one reached body's *text*, leaving every declaration signature
    // untouched. Because no declaration changed, declaration-level work does not
    // invalidate the other bodies: they survive via durable body import and only
    // the edited body's cone recomputes.
    let rev1_src = Corpus::new(40, 5).source();
    let rev2_src = rev1_src.replacen("fn b0() -> i32 { 0 }", "fn b0() -> i32 { 123 }", 1);
    assert_ne!(rev1_src, rev2_src, "the edit must change source");
    assert!(
        rev1_src.contains("fn b0() -> i32 { 0 }")
            && rev2_src.contains("fn b0() -> i32 { 123 }")
            && rev1_src.contains("fn b1() -> i32 { 1 }")
            && rev2_src.contains("fn b1() -> i32 { 1 }"),
        "the body-text edit must change b0 alone while retaining an unaffected body"
    );

    let options = CompileOptions::default();
    let rev1_snap = SourceSnapshot::single("main.rue", rev1_src).unwrap();
    let rev2_snap = SourceSnapshot::single("main.rue", rev2_src).unwrap();

    let mut warm = CompilerSession::new();
    warm.update(&rev1_snap).into_result().unwrap();
    warm.canonical_semantic(&options).unwrap();
    let body_names = corpus_body_names(40);
    let rev1_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    warm.update(&rev2_snap).into_result().unwrap();
    let warm_rev2 = warm.canonical_semantic(&options);
    let rev2_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    let recomputed = changed_body_origins(&rev1_origins, &rev2_origins);
    let warm_measure = warm_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("single-body-edit warm rev2 compiles");

    // Fresh rev2 compile for the shared parity oracle and the cold-cost floor.
    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_rev2 = fresh.canonical_semantic(&options);
    let fresh_measure = fresh_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("single-body-edit fresh rev2 compiles");
    assert_warm_fresh_parity(
        "single body edit",
        &mut warm,
        &mut fresh,
        &rev2_snap,
        &options,
        &warm_rev2,
        &fresh_rev2,
    );

    // A body-text-only edit is already narrow today: it has exactly one body
    // transaction, and the independent fresh measurement proves that this is
    // strictly cheaper than a full revision-2 compile.
    let mut report = Report::new("invalidation: one body-text edit");
    report.push(rue_1121_exact_recompute_set_row(
        "body-text-only edit recomputes the exact body set",
        &recomputed,
        BTreeSet::from(["b0".to_owned()]),
        BTreeSet::from(["b0".to_owned()]),
    ));
    report.push(rue_1121_edit_recompute_row(
        "body-text-only edit recomputes exactly b0",
        warm_measure,
        fresh_measure,
        1,
        41,
    ));
    report.emit();
}

#[test]
fn invalidation_declaration_value_edit_recomputes_exact_consumers() {
    // `selected` has exactly two direct body consumers. `control` is unrelated;
    // `main` reaches both consumers but does not mention `selected`, so the
    // source proves the exact direct-consumer recompute set is {left, right}.
    let rev1_src = "\
const selected: i32 = 1;
fn left() -> i32 { selected }
fn right() -> i32 { selected }
fn control() -> i32 { 99 }
fn main() -> i32 { left() + right() + control() }
";
    let rev2_src = rev1_src.replacen("const selected: i32 = 1;", "const selected: i32 = 2;", 1);
    assert_ne!(
        rev1_src, rev2_src,
        "the declaration-value edit must change source"
    );
    assert_eq!(
        rev2_src.matches("selected").count(),
        3,
        "the edited declaration must have exactly two body consumers"
    );
    assert!(
        rev2_src.contains("fn control() -> i32 { 99 }")
            && rev2_src.contains("fn main() -> i32 { left() + right() + control() }"),
        "the non-consumer control bodies must stay source-identical"
    );

    let options = CompileOptions::default();
    let rev1_snap = SourceSnapshot::single("main.rue", rev1_src).unwrap();
    let rev2_snap = SourceSnapshot::single("main.rue", rev2_src).unwrap();

    let mut warm = CompilerSession::new();
    warm.update(&rev1_snap).into_result().unwrap();
    warm.canonical_semantic(&options).unwrap();
    let body_names = ["left", "right", "control", "main"].map(str::to_owned);
    let rev1_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    warm.update(&rev2_snap).into_result().unwrap();
    let warm_rev2 = warm.canonical_semantic(&options);
    let rev2_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    let recomputed = changed_body_origins(&rev1_origins, &rev2_origins);
    let warm_measure = warm_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("declaration-value-edit warm rev2 compiles");

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_rev2 = fresh.canonical_semantic(&options);
    let fresh_measure = fresh_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("declaration-value-edit fresh rev2 compiles");

    assert_warm_fresh_parity(
        "declaration value edit",
        &mut warm,
        &mut fresh,
        &rev2_snap,
        &options,
        &warm_rev2,
        &fresh_rev2,
    );

    let mut report = Report::new("invalidation: declaration value edit");
    report.push(rue_1121_exact_recompute_set_row(
        "declaration value edit recomputes the exact body set",
        &recomputed,
        BTreeSet::from(["left".to_owned(), "right".to_owned()]),
        body_names.into_iter().collect(),
    ));
    report.push(rue_1121_edit_recompute_row(
        "declaration value edit recomputes exactly its consumers {left, right}",
        warm_measure,
        fresh_measure,
        2,
        4,
    ));
    report.emit();
}

#[test]
fn invalidation_negative_lookup_becoming_positive_invalidates_consumers() {
    // rev1: `main` calls `extra()`, which does not exist — a negative lookup that
    // fails the compile. rev2: define `extra`, turning the lookup positive. The
    // consumer (`main`) must invalidate and the program must go green, matching a
    // fresh rev2 compile exactly. A `control` body that references neither must
    // NOT be dragged into the recompute (Finding 5).
    let control = "fn control() -> i32 { 99 }\n";
    let rev1_src = format!("{control}fn main() -> i32 {{ extra() + control() }}\n");
    let rev2_src =
        format!("fn extra() -> i32 {{ 7 }}\n{control}fn main() -> i32 {{ extra() + control() }}\n");
    assert_ne!(
        rev1_src, rev2_src,
        "the negative-to-positive edit must change source"
    );
    assert_eq!(
        rev2_src.matches("extra").count(),
        2,
        "the newly-positive declaration must have exactly one consumer"
    );

    let options = CompileOptions::default();
    let rev1_snap = SourceSnapshot::single("main.rue", rev1_src).unwrap();
    let rev2_snap = SourceSnapshot::single("main.rue", rev2_src).unwrap();

    let mut warm = CompilerSession::new();
    warm.update(&rev1_snap).into_result().unwrap();
    let rev1_result = warm.canonical_semantic(&options);
    assert!(
        rev1_result.is_err(),
        "calling an undefined function must fail the negative-lookup revision"
    );
    let body_names = ["extra", "control", "main"].map(str::to_owned);
    let rev1_origins = warm.retained_body_transaction_origins_for_test(&body_names);

    warm.update(&rev2_snap).into_result().unwrap();
    let warm_rev2 = warm.canonical_semantic(&options);
    let rev2_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    let recomputed = changed_body_origins(&rev1_origins, &rev2_origins);
    let warm_measure = warm_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("defining the previously-missing function must make the consumer compile");

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_rev2 = fresh.canonical_semantic(&options);
    let fresh_measure = fresh_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("fresh rev2 compiles");

    // Shared full-parity oracle: warm negative->positive result equals fresh.
    assert_warm_fresh_parity(
        "negative->positive",
        &mut warm,
        &mut fresh,
        &rev2_snap,
        &options,
        &warm_rev2,
        &fresh_rev2,
    );

    // The exact repaired set is the new declaration plus `main`, its only
    // consumer. `control` is independently reached yet unrelated, so it must
    // remain green. The separately measured fresh body count proves the target
    // is strictly cheaper than a full revision-2 compile.
    //
    // This is an explicit diagnostic for the current production discrepancy,
    // not a weakened equality gate: provenance includes newly retained bodies
    // (`control` and `extra`) and the retained-key lifecycle counter includes
    // only `main`, the one body that existed before the failed revision. The
    // exact-set row below remains the RUE-1091 gate for the repaired behavior.
    assert_eq!(
        rev1_origins.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from(["main".to_owned()])
    );
    assert_eq!(
        rev2_origins.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from(["control".to_owned(), "extra".to_owned(), "main".to_owned()])
    );
    assert_eq!(
        recomputed,
        BTreeSet::from(["control".to_owned(), "extra".to_owned(), "main".to_owned()])
    );
    assert_eq!(warm_measure.body_analyses_computed, 3);
    assert_eq!(warm_measure.body_analyses_reused, 0);
    assert_eq!(warm_measure.body_analyses_invalidated, 1);
    let mut report = Report::new("invalidation: negative->positive lookup (with control body)");
    report.push(Row::Met {
        label: format!(
            "diagnostic provenance rev1={rev1_origins:?} rev2={rev2_origins:?} \
             changed={recomputed:?}; lifecycle computed={} reused={} invalidated={}",
            warm_measure.body_analyses_computed,
            warm_measure.body_analyses_reused,
            warm_measure.body_analyses_invalidated,
        ),
    });
    report.push(rue_1121_exact_recompute_set_row(
        "negative-to-positive lookup recomputes the exact body set",
        &recomputed,
        BTreeSet::from(["extra".to_owned(), "main".to_owned()]),
        body_names.into_iter().collect(),
    ));
    report.push(rue_1121_edit_recompute_row(
        "negative->positive recomputes exactly {extra, main}",
        warm_measure,
        fresh_measure,
        2,
        3,
    ));
    report.emit();
}

#[test]
fn correctness_oracle_noop_edit_preserves_every_body_terminal() {
    let source = "fn isolated() -> i32 { 1 }\nfn main() -> i32 { isolated() }\n";
    let changed = run_source_edit("no-op edit", source, source, &["isolated", "main"]);
    assert!(
        changed.changed.is_empty(),
        "a no-op must retain every body terminal"
    );
}

#[test]
fn counter_lifecycle_covers_cold_unrelated_edit_changed_body_and_unrelated_deletion() {
    let options = CompileOptions::default();
    let mut unrelated_session = CompilerSession::new();
    let cold_source = unrelated_module_snapshot(1);
    unrelated_session
        .update(&cold_source)
        .into_result()
        .unwrap();
    let cold = unrelated_session
        .canonical_semantic(&options)
        .expect("cold lifecycle fixture compiles");
    let cold_body = cold.work().body_analysis;
    assert_eq!(cold_body.body_analyses_reused, 0);
    assert_eq!(cold_body.body_analyses_invalidated, 0);
    assert!(
        cold_body.body_analyses_computed > 0,
        "cold compilation must enter at least one body analysis"
    );

    unrelated_session
        .update(&unrelated_module_snapshot(2))
        .into_result()
        .unwrap();
    let unrelated_output = unrelated_session
        .canonical_semantic(&options)
        .expect("unrelated-edit lifecycle fixture compiles");
    let unrelated_body = unrelated_output.work().body_analysis;
    assert_eq!(unrelated_body.body_analyses_computed, 0);
    assert_eq!(unrelated_body.body_analyses_invalidated, 0);
    assert_eq!(
        unrelated_body.body_analyses_reused, cold_body.body_analyses_computed,
        "an unrelated edit must reuse every unaffected body analysis"
    );
    assert_eq!(
        unrelated_body.per_body_declaration_context,
        rue_air::PerBodyDeclarationContextWork::default(),
        "reused bodies must not add declaration/module/RIR work"
    );

    let changed_initial = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }\n",
    )
    .unwrap();
    let changed = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 2 }\nfn main() -> i32 { helper() }\n",
    )
    .unwrap();
    let mut changed_session = CompilerSession::new();
    changed_session
        .update(&changed_initial)
        .into_result()
        .unwrap();
    let changed_cold = changed_session
        .canonical_semantic(&options)
        .expect("changed-body cold fixture compiles");
    let changed_cold_body = changed_cold.work().body_analysis;
    changed_session.update(&changed).into_result().unwrap();
    let changed_output = changed_session
        .canonical_semantic(&options)
        .expect("changed-body lifecycle fixture compiles");
    let changed_body = changed_output.work().body_analysis;
    assert!(changed_body.body_analyses_computed > 0);
    assert!(changed_body.body_analyses_reused > 0);
    assert_eq!(
        changed_body.body_analyses_invalidated, changed_body.body_analyses_computed,
        "every recomputed changed-body transaction had a retained predecessor"
    );
    assert_eq!(
        changed_body.body_analyses_computed + changed_body.body_analyses_reused,
        changed_cold_body.body_analyses_computed,
        "changed-body lifecycle must partition cold bodies into recomputed and reused"
    );

    // Force a genuinely new semantic request. An exact-cycle snapshot would
    // reuse the retained top-level semantic result and expose its historical
    // work fields; those fields describe the original request, not this one.
    // The main body/key stays byte-identical; only the unrelated universe is
    // perturbed while one unrelated module is removed.
    let deleted_output = unrelated_session
        .update(&unrelated_module_snapshot_with_main_and_suffix(
            "fn main() -> i32 { 0 }",
            1,
            "\n",
        ))
        .into_result()
        .and_then(|_| unrelated_session.canonical_semantic(&options))
        .expect("deletion lifecycle fixture compiles");
    let deleted_body = deleted_output.work().body_analysis;
    assert_eq!(deleted_body.body_analyses_computed, 0);
    assert_eq!(deleted_body.body_analyses_reused, 1);
    assert_eq!(
        deleted_body.body_analyses_invalidated, 0,
        "deleting an unreachable module must not charge main-body invalidation"
    );
    assert_eq!(
        deleted_body.per_body_declaration_context,
        rue_air::PerBodyDeclarationContextWork::default(),
        "unrelated deletion must not enter the body coordinator"
    );
}

#[test]
#[should_panic(expected = "successful output body identity is absent or ambiguous")]
fn correctness_oracle_rejects_missing_successful_body_transaction() {
    let options = CompileOptions::default();
    let source = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }\n").unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let output = session.canonical_semantic(&options).unwrap();
    let mut states = session.retained_body_identity_states_for_test(&options);
    let main_identity = states
        .keys()
        .find(|identity| identity.contains("main"))
        .cloned()
        .expect("main body identity is retained");
    states.remove(&main_identity);
    assert_successful_output_body_presence("missing retained body regression", &output, &states);
}

#[test]
#[should_panic(expected = "successful warm/fresh body-key sets differ")]
fn correctness_oracle_rejects_asymmetric_successful_body_key_sets() {
    let options = CompileOptions::default();
    let source = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }\n").unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.canonical_semantic(&options).unwrap();
    let transaction = session
        .retained_body_identity_states_for_test(&options)
        .into_values()
        .find_map(|transaction| transaction)
        .expect("synthetic successful transaction is retained");
    let warm = BTreeMap::from([
        ("reachable".to_owned(), Some(transaction.clone())),
        ("warm-only".to_owned(), Some(transaction.clone())),
    ]);
    let fresh = BTreeMap::from([("reachable".to_owned(), Some(transaction))]);
    assert_reachable_body_key_set_parity("asymmetric successful body keys", &warm, &fresh);
}

#[test]
#[should_panic(expected = "reachable successful body identity has no transaction")]
fn correctness_oracle_rejects_missing_reachable_transaction_on_both_sides() {
    let options = CompileOptions::default();
    let source = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }\n",
    )
    .unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let output = session.canonical_semantic(&options).unwrap();
    let states = session.retained_body_identity_states_for_test(&options);
    let (root_identity, root_transaction) = states
        .into_iter()
        .find_map(|(identity, transaction)| {
            identity
                .contains("name: \"main\"")
                .then(|| transaction.map(|transaction| (identity, transaction)))
                .flatten()
        })
        .expect("main root transaction is retained");
    assert!(
        root_transaction
            .references()
            .0
            .iter()
            .any(|reference| matches!(reference, crate::body_query::BodyReference::Callable(_))),
        "root transaction must demand a callable body"
    );
    let mut omitted = BTreeMap::new();
    omitted.insert(root_identity, Some(root_transaction));
    reachable_successful_body_identities("both-missing", &output, &omitted, &options);
}

#[test]
fn correctness_oracle_signature_edit_follows_transitive_fanout() {
    let rev1 = "fn leaf() -> i32 { 1 }\nfn middle() -> i32 { leaf() }\nfn control() -> i32 { 9 }\nfn main() -> i32 { middle() }\n";
    let rev2 = "fn leaf() -> i64 { 1 }\nfn middle() -> i64 { leaf() }\nfn control() -> i32 { 9 }\nfn main() -> i64 { middle() }\n";
    let changed = run_source_edit(
        "signature and transitive fanout edit",
        rev1,
        rev2,
        &["leaf", "middle", "control", "main"],
    );
    assert_eq!(
        changed.changed,
        BTreeSet::from(["leaf".to_owned(), "middle".to_owned(), "main".to_owned()]),
        "signature changes must reach direct and transitive consumers, not control"
    );
}

#[test]
fn correctness_oracle_diagnostic_introduce_and_repair() {
    let good = "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }\n";
    let bad = "fn helper() -> i32 { 1 }\nfn main() -> i32 { missing() }\n";
    let repaired = run_source_edit("diagnostic introduce", good, bad, &["main"]);
    assert!(
        repaired.changed.contains("main"),
        "introducing a missing lookup must replace the consumer body terminal"
    );
    assert!(
        named_state(&repaired.before_states, "main")
            .and_then(Option::as_ref)
            .is_some(),
        "diagnostic introduction must start with a successful main transaction"
    );
    assert_eq!(
        named_state(&repaired.warm_states, "main").map(|state| matches!(
            state,
            Some(crate::BodyTransaction::DeterministicFailure { .. })
        )),
        named_state(&repaired.fresh_states, "main").map(|state| matches!(
            state,
            Some(crate::BodyTransaction::DeterministicFailure { .. })
        )),
        "failed main terminal availability must match warm and fresh"
    );

    let repaired = run_source_edit("diagnostic repair", bad, good, &["helper", "main"]);
    assert!(
        repaired.changed.contains("main"),
        "repairing a missing lookup must publish a fresh successful consumer terminal"
    );
    assert!(named_state(&repaired.warm_states, "helper").is_some());
    assert!(
        named_state(&repaired.warm_states, "main")
            .and_then(Option::as_ref)
            .is_some()
    );
}

#[test]
fn correctness_oracle_rename_delete_and_visibility_edits() {
    let renamed = run_source_edit(
        "declaration rename",
        "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }\n",
        "fn renamed() -> i32 { 1 }\nfn main() -> i32 { renamed() }\n",
        &["renamed", "main"],
    );
    assert!(renamed.changed.contains("main"));
    assert!(
        named_state(&renamed.before_states, "helper")
            .and_then(Option::as_ref)
            .is_some()
    );
    assert!(
        named_state(&renamed.warm_states, "renamed")
            .and_then(Option::as_ref)
            .is_some()
    );
    assert!(
        named_state(&renamed.warm_states, "helper")
            .and_then(Option::as_ref)
            .is_none()
    );

    let deleted = run_source_edit(
        "unused declaration deletion",
        "fn unused() -> i32 { 1 }\nfn main() -> i32 { unused() }\n",
        "fn main() -> i32 { 0 }\n",
        &["unused", "main"],
    );
    assert!(
        named_state(&deleted.before_states, "unused")
            .and_then(Option::as_ref)
            .is_some()
    );
    assert!(
        named_state(&deleted.warm_states, "unused")
            .and_then(Option::as_ref)
            .is_none()
    );
    assert!(
        named_state(&deleted.fresh_states, "unused")
            .and_then(Option::as_ref)
            .is_none()
    );

    let unreachable = run_source_edit(
        "reached body becomes unreachable",
        "fn hidden() -> i32 { 1 }\nfn main() -> i32 { hidden() }\n",
        "fn hidden() -> i32 { 1 }\nfn main() -> i32 { 0 }\n",
        &["hidden", "main"],
    );
    assert!(
        named_state(&unreachable.before_states, "hidden")
            .and_then(Option::as_ref)
            .is_some()
    );
    assert!(
        named_state(&unreachable.warm_states, "hidden")
            .and_then(Option::as_ref)
            .is_some()
    );
    assert!(
        named_state(&unreachable.fresh_states, "hidden")
            .and_then(Option::as_ref)
            .is_none()
    );

    let visibility = run_source_edit(
        "visibility edit",
        "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }\n",
        "pub fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }\n",
        &["helper", "main"],
    );
    for name in ["helper", "main"] {
        assert!(
            named_state(&visibility.warm_states, name)
                .and_then(Option::as_ref)
                .is_some()
        );
        assert!(
            named_state(&visibility.fresh_states, name)
                .and_then(Option::as_ref)
                .is_some()
        );
    }
}

#[test]
fn correctness_oracle_import_edit_compares_imported_body_and_linked_bytes() {
    let options = CompileOptions::default();
    let (rev1, context1, reads1) = import_source(1, 1);
    let (rev2, context2, reads2) = import_source(2, 2);

    let mut warm = CompilerSession::new();
    warm.update(&rev1).into_result().unwrap();
    close_import_source(&mut warm, &rev1, context1, reads1);
    warm.semantic(&options).unwrap();
    let before =
        warm.retained_body_transaction_origins_for_test(&["value".to_owned(), "main".to_owned()]);
    warm.update(&rev2).into_result().unwrap();
    close_import_source(&mut warm, &rev2, context2, reads2);
    let warm_result = warm.canonical_semantic(&options);
    let after =
        warm.retained_body_transaction_origins_for_test(&["value".to_owned(), "main".to_owned()]);

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2).into_result().unwrap();
    let (_, fresh_context, fresh_reads) = import_source(2, 2);
    close_import_source(&mut fresh, &rev2, fresh_context, fresh_reads);
    let fresh_result = fresh.canonical_semantic(&options);

    assert_warm_fresh_parity(
        "imported body edit",
        &mut warm,
        &mut fresh,
        &rev2,
        &options,
        &warm_result,
        &fresh_result,
    );
    assert!(
        changed_body_origins(&before, &after).contains("main"),
        "changing an imported value must refresh its root consumer"
    );
}

// ---------------------------------------------------------------------------
// Specialization rows
// ---------------------------------------------------------------------------

#[test]
fn specialization_breadth_compiles_depth_fails_e1200() {
    // Breadth is not depth (RUE-1083). Many shallow specializations compile; an
    // unbounded instantiation chain fails E1200. The precise depth boundary is
    // owned by the canonical boundary unit tests — this row references them
    // rather than duplicating the boundary proof:
    //   * session.rs `many_shallow_specializations_compile`
    //   * session.rs `cross_body_specialization_chain_still_overflows`
    //   * rue-air sema::tests `test_single_error_no_cascade_deep_chain`
    let options = CompileOptions::default();

    // Breadth: a wide fan of shallow (depth-1) specializations must compile and
    // EACH distinct argument must produce its own specialized body.
    let breadth = 64;
    let mut wide = String::from("fn tag(comptime n: i32) -> i32 { n }\n");
    wide.push_str("fn main() -> i32 {\n    let mut total = 0;\n");
    for k in 0..breadth {
        wide.push_str(&format!("    total = total + tag({k});\n"));
    }
    wide.push_str("    total\n}\n");
    let mut wide_session = CompilerSession::new();
    wide_session
        .update(&SourceSnapshot::single("main.rue", wide).unwrap())
        .into_result()
        .unwrap();
    let wide_out = wide_session
        .canonical_semantic(&options)
        .expect("many shallow specializations must compile");
    // Finding 5: assert ALL 64 distinct specializations were analyzed, not merely
    // that at least one was.
    assert_eq!(
        wide_out.work().body_analysis.specialized_bodies_succeeded,
        breadth,
        "each of the {breadth} distinct comptime arguments must produce its own \
         specialized body"
    );

    // Depth: an unbounded cross-body instantiation chain must fail E1200. We
    // assert the diagnostic code, keeping the exact boundary in the referenced
    // boundary tests.
    let deep = "fn deepen(comptime n: i32) -> i32 { deepen(n + 1) }\n\
                fn main() -> i32 { deepen(0) }";
    let mut deep_session = CompilerSession::new();
    deep_session
        .update(&SourceSnapshot::single("main.rue", deep).unwrap())
        .into_result()
        .unwrap();
    let deep_err = deep_session
        .canonical_semantic(&options)
        .expect_err("an unbounded specialization chain must fail deterministically");
    assert!(
        deep_err.iter().any(
            |error| matches!(&error.kind, ErrorKind::ComptimeEvaluationFailed { reason }
                if reason.contains("maximum nesting depth"))
        ),
        "deep specialization chain must fail with E1200 (maximum nesting depth), got {:?}",
        deep_err.iter().map(|e| e.kind.code()).collect::<Vec<_>>()
    );

    eprintln!(
        "\n== RUE-1086 specialization ==\n  PASS  breadth {breadth} shallow specializations compile (all {breadth} analyzed)\
         \n  PASS  unbounded depth chain fails E1200"
    );
}
