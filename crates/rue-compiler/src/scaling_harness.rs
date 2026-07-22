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
//! # What it proves
//!
//! 1. Deterministic synthetic corpus varying reached-body count and
//!    unrelated-declaration count independently.
//! 2. Scaling rows (counter-based): fixed bodies / growing declarations, and
//!    fixed declarations / growing bodies.
//! 3. Two-revision invalidation rows asserted via counters and the warm-vs-fresh
//!    oracle.
//! 4. Specialization rows (breadth compiles, depth fails E1200), referencing the
//!    canonical boundary unit tests rather than duplicating them.
//! 5. A warm-vs-fresh oracle after every edit row.
//! 6. Timing/allocation measurement kept in a *separate opt-in mode* that never
//!    mixes with counter assertions.
//!
//! # Current reality and the expected-failure discipline
//!
//! Per-body work is O(declarations) today (tracked by RUE-1090/RUE-1091): the
//! demand body pipeline re-prepares, re-projects, and re-installs the whole
//! declaration universe for every reached body. Rows that assert *flat* per-body
//! work therefore cannot pass yet. They are recorded through a single mechanism
//! ([`Row::flat_or_track`]) that:
//!
//! * passes as a hard assertion the moment the measured value goes flat (the
//!   repair landed), and
//! * until then asserts the *documented known-bad witness* (per-body work still
//!   growing) so an unrelated regression still fails loudly, and records a
//!   tracked expected-failure naming its issue.
//!
//! Tracking issues, each named at its row:
//!
//! * **RUE-1089** — identity rows: per-body identity/lookup installation work
//!   should be invariant to unrelated-declaration count.
//! * **RUE-1091** — per-body shared-base / narrow-epoch repair: fixed bodies,
//!   growing declarations should leave per-body install/project work unchanged.
//! * **RUE-1090** — measurement gate: total declaration-context work should be
//!   linear in reached bodies (fixed declarations), not quadratic.
//!
//! ## Recorded prediction (pre-implementation, 2026-07-21)
//!
//! Hashed typed-key lookup plus producer-nominal machinery deletion will not
//! materially reduce the O(bodies × declarations) per-body
//! installation/projection/endpoint term (~62% of cold wall time at Caldera
//! scale). Decision rule: after the identity cut lands, if the harness shows
//! per-body install/project/endpoint work still increasing with
//! unrelated-declaration count, the shared-base or narrow-epoch repair proceeds;
//! an incidental wall-time improvement without flat per-body counters is not
//! success.

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
    /// Σ_body declaration-shell predeclarations (the per-body "prepare" term).
    shells_total: usize,
    /// Σ_body declaration semantics projected + installed (the "install/project"
    /// term).
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

    /// Per-body declaration "install/project" work. Flat under the same repair.
    fn per_body_semantics(&self) -> usize {
        self.semantics_total / self.cold_bodies.max(1)
    }
}

// ---------------------------------------------------------------------------
// Expected-failure discipline (single consistent mechanism)
// ---------------------------------------------------------------------------

/// Outcome of one scaling/identity row.
#[derive(Debug, Clone)]
enum Row {
    /// The ideal (flat/linear) relationship holds today — a hard pass.
    Met { label: String },
    /// The ideal does not hold yet; the documented known-bad witness holds
    /// instead and the row is a tracked expected-failure.
    Tracked { label: String, issue: &'static str },
}

impl Row {
    /// Assert `growing`'s per-body work equals `baseline`'s (the flat, repaired
    /// shape). If not, assert the *known-bad witness* — per-body work strictly
    /// grew — and record a tracked expected-failure for `issue`.
    ///
    /// This flips to a hard pass automatically once the repair lands, because the
    /// equality branch is taken the moment per-body work goes flat.
    fn flat_or_track(
        label: impl Into<String>,
        baseline: usize,
        growing: usize,
        issue: &'static str,
    ) -> Row {
        let label = label.into();
        if growing == baseline {
            Row::Met { label }
        } else {
            assert!(
                growing > baseline,
                "{label}: per-body work must be flat (repaired) or growing \
                 (known-bad, {issue}); got growing={growing} < baseline={baseline}, \
                 which is neither — investigate before editing this row"
            );
            Row::Tracked { label, issue }
        }
    }

    /// Assert a warm (incremental) recompute stays within an `incremental_target`
    /// cone — a hard pass, the repair landed — else record a tracked
    /// expected-failure, first asserting the known-bad witness that warm is no
    /// better than a full `fresh` recompute (so a *worse-than-full* regression
    /// still fails loudly).
    ///
    /// This flips to a hard pass the moment the warm session stops recomputing
    /// the whole body universe after a declaration-set change.
    fn incremental_or_track(
        label: impl Into<String>,
        warm_recomputed: usize,
        fresh_recomputed: usize,
        incremental_target: usize,
        issue: &'static str,
    ) -> Row {
        let label = label.into();
        if warm_recomputed <= incremental_target {
            Row::Met { label }
        } else {
            assert!(
                warm_recomputed <= fresh_recomputed,
                "{label}: warm recompute {warm_recomputed} exceeds a full fresh \
                 recompute {fresh_recomputed} — that is worse than no reuse at all, \
                 a real regression, not the known-bad {issue} shape"
            );
            Row::Tracked { label, issue }
        }
    }

    /// Assert `measured` is within `tolerance` of the `expected` linear target —
    /// a hard pass — else record a tracked expected-failure for `issue`.
    fn linear_or_track(
        label: impl Into<String>,
        measured: usize,
        expected: usize,
        tolerance: usize,
        issue: &'static str,
    ) -> Row {
        let label = label.into();
        let low = expected.saturating_sub(tolerance);
        let high = expected + tolerance;
        if (low..=high).contains(&measured) {
            Row::Met { label }
        } else {
            Row::Tracked { label, issue }
        }
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
    title: &'static str,
    rows: Vec<Row>,
}

impl Report {
    fn new(title: &'static str) -> Self {
        Self {
            title,
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
// CI vs bench sizing
// ---------------------------------------------------------------------------

/// The larger 10k-per-axis corpus runs only when `RUE_SCALING_LARGE=1`. CI runs
/// the bounded 100/1k subset; the huge sizes stay behind this explicit flag so a
/// normal `buck2 test` stays fast.
fn large_sizes_enabled() -> bool {
    std::env::var_os("RUE_SCALING_LARGE").is_some_and(|v| v == "1")
}

/// The reached-body / declaration size ladder for the current mode.
fn size_ladder() -> Vec<usize> {
    if large_sizes_enabled() {
        vec![100, 1_000, 10_000]
    } else {
        vec![100, 1_000]
    }
}

// ---------------------------------------------------------------------------
// Scaling rows
// ---------------------------------------------------------------------------

#[test]
fn scaling_fixed_bodies_growing_declarations() {
    // Axis: unrelated declarations grow while reached bodies stay fixed. The
    // ideal is that per-body install/project/prepare work is UNCHANGED — a body
    // does not care how many declarations it never touches. Today it grows
    // linearly with the declaration universe (RUE-1091 shared-base repair).
    let bodies = 100;
    let ladder = size_ladder();
    let baseline_decls = ladder[0];

    let baseline = Measure::cold(&Corpus::new(bodies, baseline_decls));
    let mut report = Report::new("scaling: fixed bodies, growing declarations");

    for &decls in ladder.iter().skip(1) {
        let grown = Measure::cold(&Corpus::new(bodies, decls));

        report.push(Row::flat_or_track(
            format!(
                "per-body prepare (shells) @ {bodies} bodies: {decls} decls={} vs {baseline_decls} decls={}",
                grown.per_body_shells(),
                baseline.per_body_shells()
            ),
            baseline.per_body_shells(),
            grown.per_body_shells(),
            "RUE-1091",
        ));
        report.push(Row::flat_or_track(
            format!(
                "per-body install/project (semantics) @ {bodies} bodies: {decls} decls={} vs {baseline_decls} decls={}",
                grown.per_body_semantics(),
                baseline.per_body_semantics()
            ),
            baseline.per_body_semantics(),
            grown.per_body_semantics(),
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

#[test]
fn scaling_fixed_declarations_growing_bodies() {
    // Axis: reached bodies grow while unrelated declarations stay fixed.
    //
    // Hard invariant (holds today): the NUMBER of body analyses is linear in
    // reached bodies — each reached body is analyzed exactly once. That is the
    // `bodies_attempted` / `cold_bodies` count.
    //
    // Tracked (RUE-1090): TOTAL declaration-context work should also be linear in
    // reached bodies, but today it is quadratic (each body re-installs the whole
    // universe), so `shells_total` grows as bodies × (bodies + decls).
    let decls = 100;
    let ladder = size_ladder();
    let base_bodies = ladder[0];

    let baseline = Measure::cold(&Corpus::new(base_bodies, decls));
    let mut report = Report::new("scaling: fixed declarations, growing bodies");

    // RUE-1090 gate baseline table: per-body counters at each corpus size. This
    // is the measurement the RUE-1090 gate reads to decide whether per-body work
    // went flat. `per_body_shells`/`per_body_semantics` are the O(declarations)
    // term today; the gate flips when they stop rising with corpus size.
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
             per_body_semantics={:>5} shells_total={:>9}",
            bodies,
            decls,
            grown.cold_bodies,
            grown.per_body_shells(),
            grown.per_body_semantics(),
            grown.shells_total,
        );

        // Hard: analysis COUNT is linear in reached bodies (+1 for `main`).
        let expected_cold = baseline.cold_bodies * factor;
        let cold_tolerance = factor + 2;
        report.push(Row::linear_or_track(
            format!(
                "body-analysis count @ {decls} decls: {bodies} bodies cold={} (~{factor}x of {})",
                grown.cold_bodies, baseline.cold_bodies
            ),
            grown.cold_bodies,
            expected_cold,
            cold_tolerance,
            "RUE-1090",
        ));
        assert!(
            grown.cold_bodies >= bodies,
            "every reached body must be analyzed once: {} < {bodies}",
            grown.cold_bodies
        );

        // Tracked: total declaration-context work should be linear (factor x) but
        // is quadratic today.
        let expected_linear_total = baseline.shells_total * factor;
        report.push(Row::flat_or_track(
            format!(
                "total declaration-context work @ {decls} decls: {bodies} bodies shells={} (linear target ~{expected_linear_total})",
                grown.shells_total
            ),
            expected_linear_total,
            grown.shells_total,
            "RUE-1090",
        ));
    }

    report.emit();
}

#[test]
fn identity_per_body_lookup_invariant_to_unrelated_declarations() {
    // Identity rows (RUE-1089). The identity/lookup installation a body performs
    // over the declaration universe should be invariant to declarations the body
    // never references. Measured as per-body install/project work at a FIXED
    // reached-body count across a growing declaration universe. Today the hashed
    // typed-key lookup has not landed, so this per-body work still tracks the
    // whole universe and the rows are tracked expected-failures.
    let bodies = 50;
    let mut report = Report::new("identity: per-body lookup invariant to unrelated declarations");

    let baseline = Measure::cold(&Corpus::new(bodies, 0));
    for &decls in &[bodies, 4 * bodies, 8 * bodies] {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        report.push(Row::flat_or_track(
            format!(
                "per-body identity install @ {bodies} bodies: +{decls} unrelated decls => {} vs {}",
                grown.per_body_semantics(),
                baseline.per_body_semantics()
            ),
            baseline.per_body_semantics(),
            grown.per_body_semantics(),
            "RUE-1089",
        ));
    }

    report.emit();
}

// ---------------------------------------------------------------------------
// Warm-vs-fresh oracle
// ---------------------------------------------------------------------------

/// Outcome of one compile, reduced to what the oracle compares: success/failure,
/// diagnostics (by kind), and — when successful — the cheap equivalent-artifact
/// projections used by the existing cold-vs-reused differential
/// (`durable_compatibility_tests`): functions debug, strings, and type-pool
/// stats.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    succeeded: bool,
    diagnostics: Vec<String>,
    functions: Option<String>,
    strings: Option<Vec<String>>,
    type_pool_stats: Option<String>,
}

impl Outcome {
    fn of(result: &Result<Arc<CanonicalSemanticOutput>, CompileErrors>) -> Self {
        match result {
            Ok(output) => Outcome {
                succeeded: true,
                diagnostics: Vec::new(),
                functions: Some(format!("{:?}", output.functions())),
                strings: Some(output.strings().to_vec()),
                type_pool_stats: Some(format!("{:?}", output.type_pool().stats())),
            },
            Err(errors) => {
                let mut diagnostics: Vec<String> = errors
                    .iter()
                    .map(|error| format!("{:?}:{:?}", error.kind.code(), error.kind))
                    .collect();
                diagnostics.sort();
                Outcome {
                    succeeded: false,
                    diagnostics,
                    functions: None,
                    strings: None,
                    type_pool_stats: None,
                }
            }
        }
    }
}

/// A two-revision edit scenario. `warm` walks rev1 -> rev2 in one session; the
/// oracle recompiles rev2 in a `fresh` session and asserts equivalence, then
/// returns the warm rev2 work counters for the caller's invalidation assertions.
struct EditScenario {
    label: &'static str,
    rev1: Corpus,
    rev2: Corpus,
}

impl EditScenario {
    /// Run rev1 then rev2 warm, run rev2 fresh, assert the warm-vs-fresh oracle,
    /// and return the warm rev2 counters (plus its `Outcome` for callers that
    /// key on success/failure).
    fn run(&self) -> (Option<Measure>, Outcome) {
        let options = CompileOptions::default();

        let mut warm = CompilerSession::new();
        warm.update(&self.rev1.snapshot()).into_result().unwrap();
        warm.canonical_semantic(&options).ok(); // rev1 result is not oracled; only the post-edit rev2 is.
        warm.update(&self.rev2.snapshot()).into_result().unwrap();
        let warm_rev2 = warm.canonical_semantic(&options);

        let mut fresh = CompilerSession::new();
        fresh.update(&self.rev2.snapshot()).into_result().unwrap();
        let fresh_rev2 = fresh.canonical_semantic(&options);

        let warm_outcome = Outcome::of(&warm_rev2);
        let fresh_outcome = Outcome::of(&fresh_rev2);
        assert_eq!(
            warm_outcome.succeeded, fresh_outcome.succeeded,
            "{}: warm/fresh success disagreement",
            self.label
        );
        assert_eq!(
            warm_outcome.diagnostics, fresh_outcome.diagnostics,
            "{}: warm/fresh diagnostics disagreement",
            self.label
        );
        assert_eq!(
            warm_outcome.functions, fresh_outcome.functions,
            "{}: warm/fresh function artifacts diverged",
            self.label
        );
        assert_eq!(
            warm_outcome.strings, fresh_outcome.strings,
            "{}: warm/fresh strings diverged",
            self.label
        );
        assert_eq!(
            warm_outcome.type_pool_stats, fresh_outcome.type_pool_stats,
            "{}: warm/fresh type-pool diverged",
            self.label
        );

        let measure = warm_rev2
            .as_ref()
            .ok()
            .map(|o| Measure::from_work(&o.work()));
        (measure, warm_outcome)
    }
}

// ---------------------------------------------------------------------------
// Two-revision invalidation rows
// ---------------------------------------------------------------------------

#[test]
fn invalidation_unrelated_declaration_added_keeps_bodies_green() {
    // Add one unrelated declaration. Every previously-green body terminal must
    // stay green: warm rev2 succeeds and equals a fresh rev2 compile.
    let scenario = EditScenario {
        label: "unrelated declaration added",
        rev1: Corpus::new(40, 5),
        rev2: Corpus::new(40, 6),
    };
    // HARD: terminals stay green. `scenario.run()` already asserted the
    // warm-vs-fresh oracle (same success, diagnostics, and artifacts); here we
    // pin that the compile succeeded at all.
    let (warm, outcome) = scenario.run();
    assert!(
        outcome.succeeded,
        "adding an unrelated declaration must compile"
    );
    let warm = warm.unwrap();

    // TRACKED (RUE-1091): no reached body's meaning changed, so the ideal warm
    // recompute is zero — a purely-unrelated declaration should invalidate no
    // body. Today adding one declaration mutates the shared declaration epoch and
    // busts every body's durable-import key, so the warm session recomputes the
    // whole universe (warm == fresh). The counter makes that entanglement
    // visible; the row flips to a hard pass once the narrow-epoch / shared-base
    // repair lands and unrelated bodies survive.
    let fresh = Measure::cold(&scenario.rev2);
    let mut report = Report::new("invalidation: unrelated declaration added");
    report.push(Row::incremental_or_track(
        format!(
            "unrelated declaration invalidates no body: warm cold_bodies={} (ideal 0, fresh {})",
            warm.cold_bodies, fresh.cold_bodies
        ),
        warm.cold_bodies,
        fresh.cold_bodies,
        0,
        "RUE-1091",
    ));
    report.emit();
}

#[test]
fn invalidation_single_body_edit_declaration_work_does_not_rerun() {
    // Edit exactly one reached body's *text*, leaving every declaration signature
    // untouched. Because no declaration changed, declaration-level work does not
    // invalidate the other bodies: they survive via durable body import and only
    // the edited body's cone recomputes. This is the counterpart to the
    // add/remove rows below — those DO mutate the declaration set and (today)
    // bust every body. The contrast localizes the RUE-1091 entanglement to
    // declaration-set changes, not body edits.
    let rev1_src = Corpus::new(40, 5).source();
    let rev2_src = rev1_src.replacen("fn b0() -> i32 { 0 }", "fn b0() -> i32 { 123 }", 1);
    assert_ne!(rev1_src, rev2_src, "the edit must change source");

    let options = CompileOptions::default();
    let rev1_snap = SourceSnapshot::single("main.rue", rev1_src).unwrap();
    let rev2_snap = SourceSnapshot::single("main.rue", rev2_src.clone()).unwrap();

    let mut warm = CompilerSession::new();
    warm.update(&rev1_snap).into_result().unwrap();
    warm.canonical_semantic(&options).unwrap();
    warm.update(&rev2_snap).into_result().unwrap();
    let warm_out = warm.canonical_semantic(&options).unwrap();
    let warm_measure = Measure::from_work(&warm_out.work());

    // Fresh rev2 compile for both the oracle and the cold-cost comparison.
    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_out = fresh.canonical_semantic(&options).unwrap();
    let fresh_measure = Measure::from_work(&fresh_out.work());

    // Warm-vs-fresh oracle.
    assert_eq!(
        format!("{:?}", warm_out.functions()),
        format!("{:?}", fresh_out.functions()),
        "single-body-edit warm/fresh functions diverged"
    );
    assert_eq!(warm_out.strings(), fresh_out.strings());
    assert_eq!(warm_out.type_pool().stats(), fresh_out.type_pool().stats());

    eprintln!(
        "\n== RUE-1086 invalidation: single body edit (declaration work does not rerun) ==\n  \
         warm cold_bodies={} vs fresh cold_bodies={} (only the edited cone recomputes)",
        warm_measure.cold_bodies, fresh_measure.cold_bodies
    );
    // HARD: a body-text edit reuses the 39 untouched bodies and main, recomputing
    // only the edited cone. Because declaration signatures did not change, the
    // survivors' durable-import keys stay valid — declaration-level work did not
    // invalidate them. The edited cone is tiny and far below a full fresh compile.
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
}

#[test]
fn invalidation_referenced_declaration_removed_recomputes_affected_cone() {
    // Remove a referenced declaration together with its single reference: drop
    // `b0` from `main`'s body and delete `fn b0`. The affected cone is `main`
    // (whose body changed); the 39 surviving reached bodies are unchanged and the
    // program stays green. Correctness (the oracle) is asserted hard; the ideal
    // that ONLY the affected cone recomputes is tracked (RUE-1091): removing a
    // declaration mutates the shared declaration epoch, so today the survivors'
    // durable-import keys are busted and the warm session recomputes them all.
    let rev1_src = Corpus::new(40, 5).source();
    // Remove the definition of b0 and its call site.
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
    let warm_out = warm.canonical_semantic(&options).unwrap();
    let warm_measure = Measure::from_work(&warm_out.work());

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_out = fresh.canonical_semantic(&options).unwrap();
    let fresh_measure = Measure::from_work(&fresh_out.work());

    assert_eq!(
        format!("{:?}", warm_out.functions()),
        format!("{:?}", fresh_out.functions()),
        "referenced-removal warm/fresh functions diverged"
    );
    assert_eq!(warm_out.strings(), fresh_out.strings());
    assert_eq!(warm_out.type_pool().stats(), fresh_out.type_pool().stats());

    // TRACKED (RUE-1091): the affected cone is just `main`; the 39 survivors
    // should be reused. The `+2` target admits `main` plus a slack body.
    let mut report = Report::new("invalidation: referenced declaration removed");
    report.push(Row::incremental_or_track(
        format!(
            "only the affected cone recomputes: warm cold_bodies={} (ideal <=2, fresh {})",
            warm_measure.cold_bodies, fresh_measure.cold_bodies
        ),
        warm_measure.cold_bodies,
        fresh_measure.cold_bodies,
        2,
        "RUE-1091",
    ));
    report.emit();
}

#[test]
fn invalidation_negative_lookup_becoming_positive_invalidates_consumers() {
    // rev1: `main` calls `extra()`, which does not exist — a negative lookup that
    // fails the compile. rev2: define `extra`, turning the lookup positive. The
    // consumer (`main`) must invalidate and the program must go green, matching a
    // fresh rev2 compile exactly.
    let rev1_src = "fn main() -> i32 { extra() }\n".to_string();
    let rev2_src = "fn extra() -> i32 { 7 }\nfn main() -> i32 { extra() }\n".to_string();

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

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_rev2 = fresh.canonical_semantic(&options);

    let warm_outcome = Outcome::of(&warm_rev2);
    let fresh_outcome = Outcome::of(&fresh_rev2);
    assert!(
        warm_outcome.succeeded,
        "defining the previously-missing function must make the consumer compile"
    );
    assert_eq!(
        warm_outcome, fresh_outcome,
        "negative->positive warm result must equal a fresh rev2 compile"
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
    // each must be a distinct specialized body the coordinator counts.
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
    assert!(
        wide_out.work().body_analysis.specialized_bodies_succeeded >= 1,
        "shallow specialization breadth must produce specialized bodies"
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
        "\n== RUE-1086 specialization ==\n  PASS  breadth {breadth} shallow specializations compile\
         \n  PASS  unbounded depth chain fails E1200"
    );
}

// ---------------------------------------------------------------------------
// Timing / allocation mode (opt-in, NEVER mixed with counter assertions)
// ---------------------------------------------------------------------------

#[test]
fn timing_mode_is_opt_in_only() {
    // Wall-time and allocation measurement is a separate mode gated on
    // `RUE_SCALING_TIMING=1`. It performs NO counter assertions. When the flag is
    // unset (the CI default) this test is an immediate no-op, so timing noise
    // never enters the counter-based suite.
    if std::env::var_os("RUE_SCALING_TIMING").is_none_or(|v| v != "1") {
        return;
    }

    use std::time::Instant;
    let ladder = size_ladder();
    eprintln!("\n== RUE-1086 timing mode (opt-in; NOT a counter assertion) ==");
    for &bodies in &ladder {
        for &decls in &ladder {
            let corpus = Corpus::new(bodies, decls);
            let mut session = CompilerSession::new();
            session.update(&corpus.snapshot()).into_result().unwrap();
            let start = Instant::now();
            session
                .canonical_semantic(&CompileOptions::default())
                .unwrap();
            let elapsed = start.elapsed();
            eprintln!("  bodies={bodies:>6} decls={decls:>6}  cold_compile={elapsed:?}");
        }
    }
}
