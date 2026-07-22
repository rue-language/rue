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
//! Per-body work is O(declarations) today (tracked by RUE-1090/RUE-1091): the
//! demand body pipeline re-prepares, re-projects, and re-installs the whole
//! declaration universe for every reached body. Rows that assert *flat* per-body
//! work therefore cannot pass yet. Each such row is recorded through a single
//! two-envelope mechanism ([`Row::envelope`]) that asserts EITHER:
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
//! * **RUE-1091** — per-body shared-base / narrow-epoch repair: fixed bodies,
//!   growing declarations should leave per-body install/project work unchanged,
//!   and a purely-unrelated declaration edit should invalidate no body.
//! * **RUE-1090** — measurement gate: total declaration-context work should be
//!   linear in reached bodies (fixed declarations), not quadratic.
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
//! attributable to a concrete host and revision. The frozen Caldera prediction
//! (RUE-1083): base commit 586f50c cutover measured at commit aca4acb (release
//! build, linux x64, 4-core dev container), ~85% of cold wall in per-body
//! prepare/project/install via a 200-sample stack profile plus per-stage
//! `--time-passes` spans; commands
//! `scripts/rue-bin --target-platforms //platforms:release` and
//! `RUE_STD_PATH=$PWD/std <rue> --time-passes examples/caldera/main.rue`. The
//! ~45 ms pre-link target is an eventual reference-host goal, not a current gate.
//!
//! ## Recorded structural baseline (stage-sourced counters)
//!
//! Single-file corpus, no std import, so the declaration universe is exactly
//! `reached_bodies + unrelated_decls + 1` (`main`) and `cold_bodies` is exactly
//! `reached_bodies + 1`:
//!
//! ```text
//!   bodies × decls | cold_bodies per_body_shells per_body_semantics shells_total
//!     100 ×   100  |        101            201                201          20301
//!    1000 ×   100  |       1001           1101               1101        1102101
//! ```
//!
//! per-body prepare/install grow 1:1 with the universe (witness), and total
//! declaration-context work is `(bodies+1)·(bodies+decls+1)` — quadratic, the
//! RUE-1090 witness the gate reads.

use crate::*;
use std::sync::Arc;

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
    /// Σ_body declaration semantics installed (the "install" term), sourced from
    /// the install stage, charged only when the install actually ran.
    semantics_total: usize,
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
        Self {
            cold_bodies: body.per_body_declaration_context.cold_body_preparations,
            shells_total: body.per_body_declaration_context.shells_prepared,
            semantics_total: body.per_body_declaration_context.semantics_installed,
            air_instructions: body.air_instructions_produced,
        }
    }

    /// Per-body declaration-shell "prepare" work. Flat iff the per-body pipeline
    /// stops re-preparing the whole universe for every body.
    fn per_body_shells(&self) -> usize {
        self.shells_total / self.cold_bodies.max(1)
    }

    /// Per-body declaration "install" work. Flat under the same repair.
    fn per_body_semantics(&self) -> usize {
        self.semantics_total / self.cold_bodies.max(1)
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
        }
    }
}

/// Collects row outcomes and prints one report block. The harness stays green
/// with expected-failures marked; a `Tracked` row is not a test failure.
struct Report {
    title: String,
    rows: Vec<Row>,
}

impl Report {
    fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            rows: Vec::new(),
        }
    }

    fn push(&mut self, row: Row) {
        self.rows.push(row);
    }

    fn emit(&self) {
        eprintln!("\n== RUE-1086 {} ==", self.title);
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

/// Axis: unrelated declarations grow while reached bodies stay fixed. The
/// repaired target is that per-body install/project/prepare work is UNCHANGED —
/// a body does not care how many declarations it never touches. The documented
/// witness (RUE-1091) is per-body work growing 1:1 with the declaration
/// universe: at `decls`, per-body work is `baseline_per_body + (decls -
/// baseline_decls)`.
fn run_fixed_bodies_growing_declarations(bodies: usize, ladder: &[usize]) {
    let baseline_decls = ladder[0];
    let baseline = Measure::cold(&Corpus::new(bodies, baseline_decls));
    let mut report = Report::new(format!(
        "scaling: fixed {bodies} bodies, growing declarations"
    ));

    for &decls in ladder.iter().skip(1) {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        let witness_delta = decls - baseline_decls;

        report.push(Row::envelope(
            format!(
                "per-body prepare (shells) @ {bodies} bodies: {decls} decls={} vs {baseline_decls} decls={}",
                grown.per_body_shells(),
                baseline.per_body_shells()
            ),
            grown.per_body_shells(),
            baseline.per_body_shells(),
            2,
            baseline.per_body_shells() + witness_delta,
            2,
            "RUE-1091",
        ));
        report.push(Row::envelope(
            format!(
                "per-body install (semantics) @ {bodies} bodies: {decls} decls={} vs {baseline_decls} decls={}",
                grown.per_body_semantics(),
                baseline.per_body_semantics()
            ),
            grown.per_body_semantics(),
            baseline.per_body_semantics(),
            2,
            baseline.per_body_semantics() + witness_delta,
            2,
            "RUE-1091",
        ));

        // Hard invariant that holds today: AIR (real per-body body work) is
        // invariant to unrelated declarations. Same reached bodies => same AIR.
        assert_eq!(
            grown.air_instructions, baseline.air_instructions,
            "unrelated declarations must not change real per-body AIR work"
        );
    }

    report.emit();
}

/// Axis: reached bodies grow while unrelated declarations stay fixed.
///
/// Hard invariant (holds today): the NUMBER of body analyses is linear in
/// reached bodies — each reached body is analyzed exactly once (`cold_bodies`).
///
/// Tracked (RUE-1090): TOTAL declaration-context work should also be linear in
/// reached bodies, but today it is quadratic (each body re-installs the whole
/// universe), so `shells_total` grows as `(bodies+1)·(bodies+decls+1)`. Strong
/// invariant (Finding 5): total AIR body work stays linear in reached bodies.
fn run_fixed_declarations_growing_bodies(decls: usize, ladder: &[usize]) {
    let base_bodies = ladder[0];
    let baseline = Measure::cold(&Corpus::new(base_bodies, decls));
    let mut report = Report::new(format!(
        "scaling: fixed {decls} declarations, growing bodies"
    ));

    eprintln!(
        "\n== RUE-1086 BASELINE (RUE-1090 gate): per-body declaration-context counters ==\n  \
         {:>6} bodies × {:>4} decls | cold_bodies={:>5} per_body_shells={:>5} \
         per_body_semantics={:>5} shells_total={:>9}",
        base_bodies,
        decls,
        baseline.cold_bodies,
        baseline.per_body_shells(),
        baseline.per_body_semantics(),
        baseline.shells_total,
    );

    for &bodies in ladder.iter().skip(1) {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        let factor = bodies / base_bodies;

        eprintln!(
            "  {:>6} bodies × {:>4} decls | cold_bodies={:>5} per_body_shells={:>5} \
             per_body_semantics={:>5} shells_total={:>9} air={:>7}",
            bodies,
            decls,
            grown.cold_bodies,
            grown.per_body_shells(),
            grown.per_body_semantics(),
            grown.shells_total,
            grown.air_instructions,
        );

        // Hard: analysis COUNT is linear in reached bodies (+1 for `main`).
        report.push(Row::linear_hard(
            format!(
                "body-analysis count @ {decls} decls: {bodies} bodies cold={} (~{factor}x of {})",
                grown.cold_bodies, baseline.cold_bodies
            ),
            grown.cold_bodies,
            baseline.cold_bodies * factor,
            factor + 2,
        ));
        assert!(
            grown.cold_bodies >= bodies,
            "every reached body must be analyzed once: {} < {bodies}",
            grown.cold_bodies
        );

        // Hard (Finding 5): real per-body body work (AIR) is linear in reached
        // bodies within a small tolerance. This is the genuine body work the
        // per-body pipeline must do; only the declaration-context term is
        // quadratic, so AIR must not be.
        report.push(Row::linear_hard(
            format!(
                "total AIR body work @ {decls} decls: {bodies} bodies air={} (linear ~{})",
                grown.air_instructions,
                baseline.air_instructions * factor
            ),
            grown.air_instructions,
            baseline.air_instructions * factor,
            baseline.air_instructions * factor / 20 + factor + 2,
        ));

        // Tracked (RUE-1090): total declaration-context work should be linear
        // (factor x baseline) but is quadratic today. The witness is predicted
        // structurally from the knobs: (bodies+1) reached analyses each
        // traversing a (bodies+decls+1) universe.
        let expected_linear_total = baseline.shells_total * factor;
        let witness_total = (bodies + 1) * (bodies + decls + 1);
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

    let baseline = Measure::cold(&Corpus::new(bodies, 0));
    for &decls in decl_ladder {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        report.push(Row::envelope(
            format!(
                "per-body identity install @ {bodies} bodies: +{decls} unrelated decls => {} vs {}",
                grown.per_body_semantics(),
                baseline.per_body_semantics()
            ),
            grown.per_body_semantics(),
            baseline.per_body_semantics(),
            2,
            baseline.per_body_semantics() + decls,
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
    // target stays fast. Same envelope logic the matrix runs at 100/1k.
    run_fixed_bodies_growing_declarations(20, &[20, 40]);
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
    fn run(&self) -> (Option<Measure>, bool) {
        let options = CompileOptions::default();

        let mut warm = CompilerSession::new();
        warm.update(&self.rev1.snapshot()).into_result().unwrap();
        warm.canonical_semantic(&options).ok(); // rev1 is not oracled; only rev2.
        warm.update(&self.rev2.snapshot()).into_result().unwrap();
        let warm_rev2 = warm.canonical_semantic(&options);

        let mut fresh = CompilerSession::new();
        fresh.update(&self.rev2.snapshot()).into_result().unwrap();
        let fresh_rev2 = fresh.canonical_semantic(&options);

        assert_warm_fresh_parity(self.label, &warm_rev2, &fresh_rev2);

        let succeeded = warm_rev2.is_ok();
        let measure = warm_rev2
            .as_ref()
            .ok()
            .map(|output| Measure::from_work(&output.work()));
        (measure, succeeded)
    }
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
    let (warm, succeeded) = scenario.run();
    assert!(succeeded, "adding an unrelated declaration must compile");
    let warm = warm.unwrap();

    // TRACKED (RUE-1091): no reached body's meaning changed, so the repaired
    // target is zero warm recompute — a purely-unrelated declaration should
    // invalidate no body. The documented witness is that adding one declaration
    // mutates the shared declaration epoch and busts every body's durable-import
    // key, so the warm session recomputes the whole universe (warm == fresh).
    let fresh = Measure::cold(&scenario.rev2);
    let mut report = Report::new("invalidation: unrelated declaration added");
    report.push(Row::envelope(
        format!(
            "unrelated declaration invalidates no body: warm cold_bodies={} (target 0, witness fresh {})",
            warm.cold_bodies, fresh.cold_bodies
        ),
        warm.cold_bodies,
        0,
        0,
        fresh.cold_bodies,
        1,
        "RUE-1091",
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

    let options = CompileOptions::default();
    let rev1_snap = SourceSnapshot::single("main.rue", rev1_src).unwrap();
    let rev2_snap = SourceSnapshot::single("main.rue", rev2_src).unwrap();

    let mut warm = CompilerSession::new();
    warm.update(&rev1_snap).into_result().unwrap();
    warm.canonical_semantic(&options).unwrap();
    warm.update(&rev2_snap).into_result().unwrap();
    let warm_rev2 = warm.canonical_semantic(&options);
    let warm_measure = warm_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("single-body-edit warm rev2 compiles");
    let warm_binding = warm_rev2.as_ref().unwrap().work().binding;

    // Fresh rev2 compile for the shared parity oracle and the cold-cost floor.
    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_rev2 = fresh.canonical_semantic(&options);
    let fresh_measure = fresh_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("single-body-edit fresh rev2 compiles");
    let fresh_binding = fresh_rev2.as_ref().unwrap().work().binding;

    assert_warm_fresh_parity("single body edit", &warm_rev2, &fresh_rev2);

    eprintln!(
        "\n== RUE-1086 invalidation: single body edit (declaration work does not rerun) ==\n  \
         warm cold_bodies={} vs fresh cold_bodies={} (only the edited cone recomputes)\n  \
         declaration resolution invocations: warm={} vs fresh={} (warm reuses the base)",
        warm_measure.cold_bodies,
        fresh_measure.cold_bodies,
        warm_binding.declaration_resolution_invocations,
        fresh_binding.declaration_resolution_invocations,
    );
    // HARD: a body-text edit reuses the 39 untouched bodies and main, recomputing
    // only the edited cone, far below a full fresh compile.
    assert!(
        warm_measure.cold_bodies < fresh_measure.cold_bodies,
        "editing one body must reuse the untouched bodies: warm {} vs fresh {}",
        warm_measure.cold_bodies,
        fresh_measure.cold_bodies
    );
    assert!(
        warm_measure.cold_bodies <= 2,
        "a single body edit must recompute only its cone, not {} bodies",
        warm_measure.cold_bodies
    );
    // HARD (Finding 5): the declaration base stayed green in revision 2. A
    // body-text edit changes no declaration signature, so every declaration
    // record must be served from the durable base by exact reuse — none
    // re-resolved, none fell back to a rebuild. If b0's *signature* had changed
    // instead, `durable_records_reused` would drop below `durable_records_compared`
    // and `fallbacks` would rise, so this is a real green assertion, not a
    // vacuous one. (The session rebuilds the declaration base per request via the
    // durable path, so this is identical warm and fresh; the incremental win
    // shows up in the body work asserted above, cold_bodies warm 1 vs fresh 41.)
    let warm_reuse = warm_rev2.as_ref().unwrap().work().declaration_reuse;
    assert_eq!(
        warm_binding.declaration_resolution_invocations, 0,
        "a body-text edit must not re-resolve any declaration: {warm_binding:?}"
    );
    assert_eq!(
        warm_reuse.fallbacks, 0,
        "a body-text edit must not fall back to rebuilding the declaration base: {warm_reuse:?}"
    );
    assert!(
        warm_reuse.durable_records_compared > 0
            && warm_reuse.durable_records_reused == warm_reuse.durable_records_compared,
        "every declaration record must be reused from the durable base: {warm_reuse:?}"
    );
}

#[test]
fn invalidation_referenced_declaration_removed_recomputes_affected_cone() {
    // Remove a referenced declaration together with its single reference: drop
    // `b0` from `main`'s body and delete `fn b0`. The affected cone is `main`
    // (whose body changed); the 39 surviving reached bodies are unchanged and the
    // program stays green.
    let rev1_src = Corpus::new(40, 5).source();
    let rev2_src = rev1_src.replacen("fn b0() -> i32 { 0 }\n", "", 1).replacen(
        "    acc = acc + b0();\n",
        "",
        1,
    );
    assert!(
        !rev2_src.contains("fn b0()") && !rev2_src.contains("b0()"),
        "b0 and its reference must both be gone"
    );

    let options = CompileOptions::default();
    let rev1_snap = SourceSnapshot::single("main.rue", rev1_src).unwrap();
    let rev2_snap = SourceSnapshot::single("main.rue", rev2_src).unwrap();

    let mut warm = CompilerSession::new();
    warm.update(&rev1_snap).into_result().unwrap();
    warm.canonical_semantic(&options).unwrap();
    warm.update(&rev2_snap).into_result().unwrap();
    let warm_rev2 = warm.canonical_semantic(&options);
    let warm_measure = warm_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("referenced-removal warm rev2 compiles");

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_rev2 = fresh.canonical_semantic(&options);
    let fresh_measure = fresh_rev2
        .as_ref()
        .map(|output| Measure::from_work(&output.work()))
        .expect("referenced-removal fresh rev2 compiles");

    assert_warm_fresh_parity("referenced declaration removed", &warm_rev2, &fresh_rev2);

    // TRACKED (RUE-1091): the affected cone is just `main`; the 39 survivors
    // should be reused (repaired target <= 2 for `main` plus a slack body). The
    // documented witness is that removing a declaration mutates the shared
    // declaration epoch, busting every survivor's durable-import key (warm ==
    // fresh, whole-universe recompute).
    let mut report = Report::new("invalidation: referenced declaration removed");
    report.push(Row::envelope(
        format!(
            "only the affected cone recomputes: warm cold_bodies={} (target <=2, witness fresh {})",
            warm_measure.cold_bodies, fresh_measure.cold_bodies
        ),
        warm_measure.cold_bodies,
        2,
        0,
        fresh_measure.cold_bodies,
        1,
        "RUE-1091",
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

    warm.update(&rev2_snap).into_result().unwrap();
    let warm_rev2 = warm.canonical_semantic(&options);
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

    // Finding 5: only the exact consumers recompute. The affected cone is the
    // newly-defined `extra` plus its consumer `main` (repaired target <= 2). The
    // documented witness (RUE-1091) is that a rev1 whole-compile failure drops
    // every body terminal, so warm rev2 recomputes the whole universe including
    // the unaffected `control` (warm == fresh). `warm > fresh` — recomputing
    // more than a cold compile — fails.
    let mut report = Report::new("invalidation: negative->positive lookup (with control body)");
    report.push(Row::envelope(
        format!(
            "only exact consumers recompute: warm cold_bodies={} (target <=2 [extra,main], witness fresh {})",
            warm_measure.cold_bodies, fresh_measure.cold_bodies
        ),
        warm_measure.cold_bodies,
        2,
        0,
        fresh_measure.cold_bodies,
        1,
        "RUE-1091",
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
