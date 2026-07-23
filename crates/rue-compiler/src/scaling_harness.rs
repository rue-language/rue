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
//! Wall-time, allocation-count, and peak-memory measurement live in a *separate*
//! opt-in binary, `//crates/rue-scaling-bench`, so they never mix with these
//! counter assertions. See that crate's module docs for the reproducible runner
//! command lines and the recorded-baseline provenance.
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
//! # Reference host and recorded prediction
//!
//! The recorded structural baselines below were captured on the CI/dev host; the
//! `//crates/rue-scaling-bench` runner prints `nproc`, total memory, and the
//! commit hash at run time so any wall-time/allocation/memory baseline is
//! attributable to a concrete host and revision.
//!
//! Two distinct Caldera figures must not be conflated (ADR-0066 §3, RUE-1086
//! provenance in `docs/benchmarks/rue-1086-caldera-baseline.json`):
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
//! `examples/caldera/main.rue`. The full machine-readable record — including
//! this host's configuration and the per-sample raw evidence — is checked in at
//! `docs/benchmarks/rue-1086-caldera-baseline.json`. The ~45 ms *pre-link*
//! target is an eventual reference-host goal, not a current gate.
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

use crate::*;
use std::{
    collections::{BTreeMap, BTreeSet},
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

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// Structural counters read from one cold `canonical_semantic` compile. Every
/// field is a work counter; not one is a clock or an allocation total.
#[derive(Debug, Clone, Copy)]
struct Measure {
    /// Cold reached-body analyses that paid the declaration-context cost.
    cold_bodies: usize,
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

    fn from_work(work: &CanonicalSemanticWork) -> Self {
        let body = &work.body_analysis;
        let ctx = &body.per_body_declaration_context;
        Self {
            cold_bodies: ctx.cold_body_preparations,
            shells_total: ctx.shells_prepared,
            projections_total: ctx.projections_performed,
            semantics_total: ctx.semantics_installed,
            endpoints_total: ctx.endpoints_installed,
            air_instructions: body.air_instructions_produced,
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
    warm: &Result<Arc<CanonicalSemanticOutput>, CompileErrors>,
    fresh: &Result<Arc<CanonicalSemanticOutput>, CompileErrors>,
) {
    match (warm, fresh) {
        (Ok(warm), Ok(fresh)) => {
            assert_eq!(
                warm.unstable_parity_snapshot(),
                fresh.unstable_parity_snapshot(),
                "{label}: warm/fresh semantic parity snapshot diverged"
            );
        }
        (Err(warm), Err(fresh)) => {
            assert_eq!(
                render_diagnostics(warm),
                render_diagnostics(fresh),
                "{label}: warm/fresh failure diagnostics diverged"
            );
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

/// RUE-1121's post-repair acceptance row for a counter charged once per body.
///
/// The RUE-1090 decision remains its exact-ratio verdict below. This row is the
/// complementary acceptance assertion: with the same reached-body topology,
/// growing unrelated declarations must leave this observable counter exactly
/// flat. Until RUE-1091 repairs the shared declaration context, the current
/// whole-universe witness records a controlled expected failure. Once repaired,
/// the same row becomes a hard target pass without duplicating the test.
fn rue_1121_exact_flat_context_row(
    counter: &str,
    baseline_total: usize,
    baseline_bodies: usize,
    grown_total: usize,
    grown_bodies: usize,
    historical_grown_per_body_witness: usize,
) -> Row {
    assert_eq!(
        grown_bodies, baseline_bodies,
        "RUE-1121 fixed-body corpus changed its body-preparation topology"
    );
    let witness = historical_grown_per_body_witness
        .checked_mul(grown_bodies)
        .expect("RUE-1121 context witness overflow");
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
        // RUE-1121 acceptance rows consume only stage-sourced counters. The
        // RUE-1090 verdict below deliberately remains limited to its original
        // project/install/endpoint exact-ratio decision.
        for (counter, baseline_total, grown_total, historical_grown_per_body_witness) in [
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
                historical_grown_per_body_witness,
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

        // Hard: each per-body declaration-context counter equals its exact
        // corpus-derived closed form. These grow with `bodies` on this axis, so
        // asserting the closed form (not a scaled measured baseline) catches a
        // uniform per-body regression.
        report.push(Row::linear_hard(
            format!(
                "per-body prepare (shells) @ {decls} decls: {bodies} bodies={} (corpus U={})",
                grown.per_body_shells(),
                predicted.per_body_shells
            ),
            grown.per_body_shells(),
            predicted.per_body_shells,
            1,
        ));
        report.push(Row::linear_hard(
            format!(
                "per-body project @ {decls} decls: {bodies} bodies={} (corpus U={})",
                grown.per_body_projections(),
                predicted.per_body_projections
            ),
            grown.per_body_projections(),
            predicted.per_body_projections,
            1,
        ));
        report.push(Row::linear_hard(
            format!(
                "per-body install (semantics) @ {decls} decls: {bodies} bodies={} (corpus U={})",
                grown.per_body_semantics(),
                predicted.per_body_semantics
            ),
            grown.per_body_semantics(),
            predicted.per_body_semantics,
            1,
        ));
        report.push(Row::linear_hard(
            format!(
                "per-body endpoints @ {decls} decls: {bodies} bodies={} (corpus 2U+1={})",
                grown.per_body_endpoints(),
                predicted.per_body_endpoints
            ),
            grown.per_body_endpoints(),
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

        assert_warm_fresh_parity(self.label, &warm_rev2, &fresh_rev2);

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
            "{label}: warm body transactions={} (exact target {}, independently measured fresh {})",
            warm.cold_bodies, exact_recomputed_bodies, fresh.cold_bodies,
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
    assert_warm_fresh_parity("single body edit", &warm_rev2, &fresh_rev2);

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

    assert_warm_fresh_parity("declaration value edit", &warm_rev2, &fresh_rev2);

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
    assert_warm_fresh_parity("negative->positive", &warm_rev2, &fresh_rev2);

    // The exact repaired set is the new declaration plus `main`, its only
    // consumer. `control` is independently reached yet unrelated, so it must
    // remain green. The separately measured fresh body count proves the target
    // is strictly cheaper than a full revision-2 compile.
    let mut report = Report::new("invalidation: negative->positive lookup (with control body)");
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
