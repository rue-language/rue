//! Structural scaling and warm/fresh correctness harness for query-native body analysis.
//!
//! The scaling rows consume production provider counters: name lookups, selected
//! identity facts, signature/type/const facts, and durable materializations. They
//! vary reached bodies and unrelated declarations independently, asserting that
//! unrelated declarations add no provider work and that reached-body growth is
//! linear. Wall-time and memory experiments remain separate from these
//! deterministic structural assertions.
//!
//! The edit rows use the same production query path and a full warm-vs-fresh
//! parity oracle after every revision change.

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

/// Structural counters read from one cold `rooted_cfg` compile. Every
/// field is a work counter; not one is a clock or an allocation total.
#[derive(Debug, Clone, Copy)]
struct Measure {
    /// Cold reached-body analyses computed by the body-query family.
    cold_bodies: usize,
    /// Request-wide local-semantic materialization indexes built for rooted CFG collection.
    materialization_indexes: usize,
    /// Name/import lookup requests made by production body providers.
    provider_lookups: usize,
    /// Selected declaration identity facts consumed by production bodies.
    provider_identity_facts: usize,
    /// Selected signature, type, and const facts consumed by production bodies.
    provider_semantic_facts: usize,
    /// Durable declarations materialized for provider requests.
    provider_materializations: usize,
    /// AIR instructions produced — genuine per-body body work, independent of the
    /// unrelated-declaration universe.
    air_instructions: usize,
    body_analyses_computed: usize,
    body_analyses_reused: usize,
    body_analyses_invalidated: usize,
    cfg_builds: usize,
    cfg_imports: usize,
    modules_projected: usize,
    rir_instructions_appended: usize,
}

impl Measure {
    /// Compile `corpus` cold in a fresh session and read its counters.
    fn cold(corpus: &Corpus) -> Self {
        let mut session = CompilerSession::new();
        session.update(&corpus.snapshot()).into_result().unwrap();
        let output = session
            .rooted_cfg(&CompileOptions::default())
            .expect("synthetic corpus compiles");
        Self::from_work_and_provider(
            &output.work(),
            crate::unstable::provider_observation_metrics(&session),
            canonical_rir_work(&mut session),
        )
    }

    fn cold_snapshot(snapshot: SourceSnapshot) -> Self {
        let mut session = CompilerSession::new();
        session.update(&snapshot).into_result().unwrap();
        let output = session
            .rooted_cfg(&CompileOptions::default())
            .expect("synthetic snapshot compiles");
        Self::from_work_and_provider(
            &output.work(),
            crate::unstable::provider_observation_metrics(&session),
            canonical_rir_work(&mut session),
        )
    }

    fn from_work_and_provider(
        work: &CanonicalSemanticWork,
        provider: crate::unstable::ProviderObservationMetrics,
        rir: crate::CanonicalRirWork,
    ) -> Self {
        let body = &work.body_analysis;
        Self {
            // The registered body-transaction family is the production
            // computation boundary. Its lifecycle counters are therefore the
            // exact cold/reused/invalidated topology; the retired epoch-overlay
            // preparation counters are intentionally not consulted.
            cold_bodies: body.body_analyses_computed,
            materialization_indexes: work.cfg.materialization_index_builds,
            provider_lookups: provider.name_lookups as usize,
            provider_identity_facts: provider.identity_facts as usize,
            provider_semantic_facts: (provider.signature_facts
                + provider.type_facts
                + provider.const_facts) as usize,
            provider_materializations: provider.materializations as usize,
            air_instructions: work.cfg.air_instructions_consumed,
            body_analyses_computed: body.body_analyses_computed,
            body_analyses_reused: body.body_analyses_reused,
            body_analyses_invalidated: body.body_analyses_invalidated,
            cfg_builds: work.cfg.cfg_builds_attempted,
            cfg_imports: work.cfg.cfg_import_attempts,
            modules_projected: rir.modules_projected,
            rir_instructions_appended: rir.instructions_appended,
        }
    }
}

fn canonical_rir_work(session: &mut CompilerSession) -> crate::CanonicalRirWork {
    session
        .canonical_rir()
        .expect("successful rooted CFG has canonical RIR")
        .work()
}

fn assert_exact_zero_slope(label: &str, baseline: usize, grown: usize) {
    assert_eq!(
        baseline, grown,
        "{label}: unrelated-universe growth changed a fixed-reach work counter"
    );
}

// ---------------------------------------------------------------------------
// Scaling-report discipline
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
    output: &RootedCfgOutput,
    states: &BTreeMap<String, Option<crate::BodyTransaction>>,
) {
    for function in output.functions() {
        let prefix = format!("{:?}:", function.function);
        let matching = states
            .iter()
            .filter(|(identity, _)| identity.starts_with(&prefix))
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            1,
            "{label}: successful output body identity is absent or ambiguous: {:?}",
            function.function
        );
        assert!(
            matching[0].1.is_some(),
            "{label}: successful output body has no observable retained transaction: {:?}",
            function.function
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
    output: &RootedCfgOutput,
    states: &BTreeMap<String, Option<crate::BodyTransaction>>,
    options: &CompileOptions,
) -> BTreeSet<String> {
    let mut pending = output
        .functions()
        .iter()
        .map(|function| body_query_identity(&function.function, options))
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
/// canonical rooted CFG values, it asserts full parity between a warm
/// (incremental) rev2 compile and a fresh rev2 compile:
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
    warm: &Result<RootedCfgOutput, CompileErrors>,
    fresh: &Result<RootedCfgOutput, CompileErrors>,
) {
    // The body-transaction family is the canonical production cache. Its
    // test-only snapshot includes stale keys as well as current terminals, so
    // successful parity must first narrow it to the identities reachable from
    // each output and its transaction edges.
    let warm_bodies = warm_session.retained_body_identity_states_for_test(options);
    let fresh_bodies = fresh_session.retained_body_identity_states_for_test(options);
    // A failed rooted body/CFG query may stop before an unchanged helper is
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
            crate::test_support::assert_rooted_cfg_value_parity(label, warm, fresh);

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
                format!("{:?}", warm.warnings()),
                format!("{:?}", fresh.warnings()),
                "{label}: ordered semantic warnings diverged"
            );
            assert_eq!(
                format!("{:?}", warm_session.latest_diagnostics_for_test()),
                format!("{:?}", fresh_session.latest_diagnostics_for_test()),
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
                format!("{:?}", warm_session.latest_diagnostics_for_test()),
                format!("{:?}", fresh_session.latest_diagnostics_for_test()),
                "{label}: ordered failure diagnostic snapshots diverged"
            );
            // A failed rooted body/CFG query may stop before unchanged helper
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
/// premerge canary runs the bounded 100/1k subset; the huge sizes stay behind
/// this explicit flag, which `scaling-matrix-stress-test` sets.
fn large_sizes_enabled() -> bool {
    std::env::var_os("RUE_SCALING_LARGE").is_some_and(|v| v == "1")
}

/// The reached-body / declaration size ladder for the matrix rows.
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

fn run_fixed_bodies_growing_declarations(bodies: usize, ladder: &[usize]) {
    let baseline_decls = ladder[0];
    let baseline = Measure::cold(&Corpus::new(bodies, baseline_decls));
    let mut report = Report::new(format!(
        "provider scaling: fixed {bodies} reached bodies, growing unrelated declarations"
    ));
    for &decls in ladder.iter().skip(1) {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        for (label, before, after) in [
            (
                "lookup operations",
                baseline.provider_lookups,
                grown.provider_lookups,
            ),
            (
                "identity operations",
                baseline.provider_identity_facts,
                grown.provider_identity_facts,
            ),
            (
                "signature/type/const operations",
                baseline.provider_semantic_facts,
                grown.provider_semantic_facts,
            ),
            (
                "materialization operations",
                baseline.provider_materializations,
                grown.provider_materializations,
            ),
            (
                "AIR instructions",
                baseline.air_instructions,
                grown.air_instructions,
            ),
        ] {
            assert_exact_zero_slope(label, before, after);
            report.push(Row::Met {
                label: format!(
                    "{label} flat across +{} unrelated declarations: {before} == {after}",
                    decls - baseline_decls
                ),
            });
        }
        report.push(Row::linear_hard(
            format!("one rooted materialization index at {decls} declarations"),
            grown.materialization_indexes,
            1,
            0,
        ));
    }
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

/// Axis: reached bodies grow while unrelated declarations stay fixed. Every
/// row reads work performed by the production provider/query path.
fn run_fixed_declarations_growing_bodies(decls: usize, ladder: &[usize]) {
    let base_bodies = ladder[0];
    let baseline = Measure::cold(&Corpus::new(base_bodies, decls));
    let mut report = Report::new(format!(
        "scaling: fixed {decls} declarations, growing bodies"
    ));

    for &bodies in ladder.iter().skip(1) {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        let predicted_cold_bodies = bodies + 1;

        eprintln!(
            "  {bodies:>6} bodies × {decls:>4} decls | cold_bodies={:>5} \
             lookups={:>7} identities={:>7} semantic_facts={:>7} \
             materializations={:>7} air={:>7}",
            grown.cold_bodies,
            grown.provider_lookups,
            grown.provider_identity_facts,
            grown.provider_semantic_facts,
            grown.provider_materializations,
            grown.air_instructions,
        );

        // Hard: analysis COUNT is linear in reached bodies, corpus-derived as
        // `bodies + 1` (each reached body plus `main`), NOT `baseline * factor`.
        report.push(Row::linear_hard(
            format!(
                "body-analysis count @ {decls} decls: {bodies} bodies cold={} (corpus target {})",
                grown.cold_bodies, predicted_cold_bodies
            ),
            grown.cold_bodies,
            predicted_cold_bodies,
            2,
        ));
        assert!(
            grown.cold_bodies >= bodies,
            "every reached body must be analyzed once: {} < {bodies}",
            grown.cold_bodies
        );

        // These are live provider operations, not retired-counter zeros. With a
        // fixed declaration universe each family must scale linearly with the
        // number of reached body transactions.
        for (label, baseline_value, measured) in [
            (
                "provider lookups",
                baseline.provider_lookups,
                grown.provider_lookups,
            ),
            (
                "provider identities",
                baseline.provider_identity_facts,
                grown.provider_identity_facts,
            ),
            (
                "provider signatures/types/consts",
                baseline.provider_semantic_facts,
                grown.provider_semantic_facts,
            ),
            (
                "provider materializations",
                baseline.provider_materializations,
                grown.provider_materializations,
            ),
        ] {
            let expected = baseline_value * grown.cold_bodies / baseline.cold_bodies.max(1);
            report.push(Row::linear_hard(
                format!(
                    "{label} @ {decls} decls: {bodies} bodies measured={measured} expected={expected}"
                ),
                measured,
                expected,
                grown.cold_bodies,
            ));
        }
        // Rooted CFG collection builds its request-wide materialization index once.
        report.push(Row::linear_hard(
            format!(
                "rooted materialization indexes @ {decls} decls: {bodies} bodies \
                 indexes={} across {} cold bodies",
                grown.materialization_indexes, grown.cold_bodies
            ),
            grown.materialization_indexes,
            1,
            0,
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
    }

    report.emit();
}

/// Identity/provider fact rows must remain invariant to declarations the bodies never reference.
fn run_identity_invariant(bodies: usize, decl_ladder: &[usize]) {
    let baseline = Measure::cold(&Corpus::new(bodies, decl_ladder[0]));
    let mut report = Report::new(format!(
        "identity: exact provider work is flat in unrelated declarations ({bodies} bodies)"
    ));
    for &decls in decl_ladder.iter().skip(1) {
        let grown = Measure::cold(&Corpus::new(bodies, decls));
        for (label, before, after) in [
            (
                "identity facts",
                baseline.provider_identity_facts,
                grown.provider_identity_facts,
            ),
            (
                "materializations",
                baseline.provider_materializations,
                grown.provider_materializations,
            ),
        ] {
            assert_exact_zero_slope(label, before, after);
            report.push(Row::Met {
                label: format!("{label} flat at {decls} declarations: {before} == {after}"),
            });
        }
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
            grown.modules_projected,
            baseline.modules_projected + modules,
            "RIR projection must account for every supplied module"
        );
        assert!(
            grown.rir_instructions_appended > baseline.rir_instructions_appended,
            "the RIR universe must expose added unrelated module instructions"
        );
    }
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
// Heavy structural matrix (`scaling-matrix-test` premerge canary)
// ---------------------------------------------------------------------------
//
// These rows are minutes of work at 100/1k, so they stay out of the default
// `rue-compiler-test` run. They are excluded with libtest's own mechanism —
// `#[ignore]`, selected back by `--ignored` — rather than by compiling the
// whole crate a second time under `--cfg=scaling_matrix`. That second compile
// made `scaling-matrix-test` a strict superset of `rue-compiler-test`: 816
// tests against 813, re-running the shared 813 in the same lane to gain three
// (RUE-1262).
//
// Selection is by name: `//crates/rue-compiler:scaling-matrix-test` and its
// stress sibling wrap this binary and pass `--ignored scaling_matrix`. The
// `scaling_matrix_` prefix is therefore load-bearing — an ignored test named
// into that prefix joins the premerge canary, and one named out of it is not
// run by any target.

#[test]
#[ignore = "heavy 100/1k structural matrix; run by the scaling-matrix-test target"]
fn scaling_matrix_fixed_bodies_growing_declarations() {
    run_fixed_bodies_growing_declarations(100, &matrix_size_ladder());
}

#[test]
#[ignore = "heavy 100/1k structural matrix; run by the scaling-matrix-test target"]
fn scaling_matrix_fixed_declarations_growing_bodies() {
    run_fixed_declarations_growing_bodies(100, &matrix_size_ladder());
}

#[test]
#[ignore = "heavy 100/1k structural matrix; run by the scaling-matrix-test target"]
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
        warm.rooted_cfg(&options).ok(); // rev1 is not oracled; only rev2.
        let rev1_origins = warm.retained_body_transaction_origins_for_test(body_names);
        warm.update(&self.rev2.snapshot()).into_result().unwrap();
        let warm_rev2 = warm.rooted_cfg(&options);
        let rev2_origins = warm.retained_body_transaction_origins_for_test(body_names);

        let mut fresh = CompilerSession::new();
        fresh.update(&self.rev2.snapshot()).into_result().unwrap();
        let fresh_rev2 = fresh.rooted_cfg(&options);

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
        let measure = warm_rev2.as_ref().ok().map(|output| {
            Measure::from_work_and_provider(
                &output.work(),
                crate::unstable::provider_observation_metrics(&warm),
                canonical_rir_work(&mut warm),
            )
        });
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
    warm.rooted_cfg(&options).ok();
    let before = warm.retained_body_transaction_origins_for_test(&body_names);
    let before_states = warm.retained_body_identity_states_for_test(&options);
    warm.update(&rev2).into_result().unwrap();
    let warm_result = warm.rooted_cfg(&options);
    let after = warm.retained_body_transaction_origins_for_test(&body_names);
    let warm_states = warm.retained_body_identity_states_for_test(&options);

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2).into_result().unwrap();
    let fresh_result = fresh.rooted_cfg(&options);
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

fn rooted_demand_locality_source(
    leaf_value: i32,
    unrelated_value: i32,
    observation: u64,
) -> (SourceSnapshot, ImportDiscoveryContext, AcceptedReadManifest) {
    // The observation regime remains fixed across revisions. Only the imported
    // source leaf changes, matching a long-lived host whose read policy is
    // stable while it services successive import-input requests.
    let context = ImportDiscoveryContext::new(1, "/p", None, "scaling-import").unwrap();
    let root = Arc::new(
        "const a = @import(\"a.rue\");\n\
         fn main() -> i32 { a.value() }\n"
            .to_owned(),
    );
    let imported = Arc::new(format!(
        "pub fn value() -> i32 {{ {leaf_value} }}\n\
         fn unrelated() -> i32 {{ {unrelated_value} }}\n"
    ));
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/p/main.rue",
        "/p/main.rue",
        PhysicalFileIdentity::new(1200, 1),
        FileMetadataFingerprint::new(root.len() as u64, 1, 1),
        root,
    )
    .unwrap();
    assembler
        .add_explicit(
            "/p/a.rue",
            "/p/a.rue",
            PhysicalFileIdentity::new(1200, 2),
            FileMetadataFingerprint::new(imported.len() as u64, observation, observation),
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
    warm.rooted_cfg(&options).unwrap();
    let body_names = corpus_body_names(40);
    let rev1_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    warm.update(&rev2_snap).into_result().unwrap();
    let warm_rev2 = warm.rooted_cfg(&options);
    let rev2_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    let recomputed = changed_body_origins(&rev1_origins, &rev2_origins);
    let warm_measure = warm_rev2
        .as_ref()
        .map(|output| {
            Measure::from_work_and_provider(
                &output.work(),
                crate::unstable::provider_observation_metrics(&warm),
                canonical_rir_work(&mut warm),
            )
        })
        .expect("single-body-edit warm rev2 compiles");

    // Fresh rev2 compile for the shared parity oracle and the cold-cost floor.
    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_rev2 = fresh.rooted_cfg(&options);
    let fresh_measure = fresh_rev2
        .as_ref()
        .map(|output| {
            Measure::from_work_and_provider(
                &output.work(),
                crate::unstable::provider_observation_metrics(&fresh),
                canonical_rir_work(&mut fresh),
            )
        })
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
    warm.rooted_cfg(&options).unwrap();
    let body_names = ["left", "right", "control", "main"].map(str::to_owned);
    let rev1_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    warm.update(&rev2_snap).into_result().unwrap();
    let warm_rev2 = warm.rooted_cfg(&options);
    let rev2_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    let recomputed = changed_body_origins(&rev1_origins, &rev2_origins);
    let warm_measure = warm_rev2
        .as_ref()
        .map(|output| {
            Measure::from_work_and_provider(
                &output.work(),
                crate::unstable::provider_observation_metrics(&warm),
                canonical_rir_work(&mut warm),
            )
        })
        .expect("declaration-value-edit warm rev2 compiles");

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_rev2 = fresh.rooted_cfg(&options);
    let fresh_measure = fresh_rev2
        .as_ref()
        .map(|output| {
            Measure::from_work_and_provider(
                &output.work(),
                crate::unstable::provider_observation_metrics(&fresh),
                canonical_rir_work(&mut fresh),
            )
        })
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
    // fresh rev2 compile exactly. The independent `control` body is retained
    // from the failed revision and must not be dragged into the recompute.
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
    let rev1_result = warm.rooted_cfg(&options);
    assert!(
        rev1_result.is_err(),
        "calling an undefined function must fail the negative-lookup revision"
    );
    let body_names = ["extra", "control", "main"].map(str::to_owned);
    let rev1_origins = warm.retained_body_transaction_origins_for_test(&body_names);

    warm.update(&rev2_snap).into_result().unwrap();
    let warm_rev2 = warm.rooted_cfg(&options);
    let rev2_origins = warm.retained_body_transaction_origins_for_test(&body_names);
    let recomputed = changed_body_origins(&rev1_origins, &rev2_origins);
    let warm_measure = warm_rev2
        .as_ref()
        .map(|output| {
            Measure::from_work_and_provider(
                &output.work(),
                crate::unstable::provider_observation_metrics(&warm),
                canonical_rir_work(&mut warm),
            )
        })
        .expect("defining the previously-missing function must make the consumer compile");

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2_snap).into_result().unwrap();
    let fresh_rev2 = fresh.rooted_cfg(&options);
    let fresh_measure = fresh_rev2
        .as_ref()
        .map(|output| {
            Measure::from_work_and_provider(
                &output.work(),
                crate::unstable::provider_observation_metrics(&fresh),
                canonical_rir_work(&mut fresh),
            )
        })
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
    assert_eq!(
        rev1_origins.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from(["control".to_owned(), "main".to_owned()])
    );
    assert_eq!(
        rev2_origins.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from(["control".to_owned(), "extra".to_owned(), "main".to_owned()])
    );
    assert_eq!(
        recomputed,
        BTreeSet::from(["extra".to_owned(), "main".to_owned()])
    );
    assert_eq!(warm_measure.body_analyses_computed, 2);
    assert_eq!(warm_measure.body_analyses_reused, 1);
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
        .rooted_cfg(&options)
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
        .rooted_cfg(&options)
        .expect("unrelated-edit lifecycle fixture compiles");
    let unrelated_body = unrelated_output.work().body_analysis;
    assert_eq!(unrelated_body.body_analyses_computed, 0);
    assert_eq!(unrelated_body.body_analyses_invalidated, 0);
    assert_eq!(
        unrelated_body.body_analyses_reused, cold_body.body_analyses_computed,
        "an unrelated edit must reuse every unaffected body analysis"
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
        .rooted_cfg(&options)
        .expect("changed-body cold fixture compiles");
    let changed_cold_body = changed_cold.work().body_analysis;
    changed_session.update(&changed).into_result().unwrap();
    let changed_output = changed_session
        .rooted_cfg(&options)
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
        .and_then(|_| unrelated_session.rooted_cfg(&options))
        .expect("deletion lifecycle fixture compiles");
    let deleted_body = deleted_output.work().body_analysis;
    assert_eq!(deleted_body.body_analyses_computed, 0);
    assert_eq!(deleted_body.body_analyses_reused, 1);
    assert_eq!(
        deleted_body.body_analyses_invalidated, 0,
        "deleting an unreachable module must not charge main-body invalidation"
    );
}

#[test]
#[should_panic(expected = "successful output body identity is absent or ambiguous")]
fn correctness_oracle_rejects_missing_successful_body_transaction() {
    let options = CompileOptions::default();
    let source = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }\n").unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let output = session.rooted_cfg(&options).unwrap();
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
    session.rooted_cfg(&options).unwrap();
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
    let output = session.rooted_cfg(&options).unwrap();
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
    warm.rooted_cfg(&options).unwrap();
    let before =
        warm.retained_body_transaction_origins_for_test(&["value".to_owned(), "main".to_owned()]);
    warm.update(&rev2).into_result().unwrap();
    close_import_source(&mut warm, &rev2, context2, reads2);
    let warm_result = warm.rooted_cfg(&options);
    let after =
        warm.retained_body_transaction_origins_for_test(&["value".to_owned(), "main".to_owned()]);

    let mut fresh = CompilerSession::new();
    fresh.update(&rev2).into_result().unwrap();
    let (_, fresh_context, fresh_reads) = import_source(2, 2);
    close_import_source(&mut fresh, &rev2, fresh_context, fresh_reads);
    let fresh_result = fresh.rooted_cfg(&options);

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

/// Phase 12's rooted-host locality gate. Both revisions enter through the
/// production import-input protocol, and both edits are driven all the way
/// through rooted CFG/CodegenUnit collection and the fresh linker. This is not
/// a driver-shaped compatibility harness: `CompilerSession` remains the sole
/// owner of the canonical query graph.
#[cfg(unix)]
#[test]
fn query_native_rooted_demand_warm_edit_locality_through_fresh_link() {
    #[derive(Debug)]
    struct Locality {
        bodies_computed: usize,
        bodies_reused: usize,
        cfgs_computed: usize,
        codegen_computed: usize,
        codegen_reused: usize,
    }

    let options = CompileOptions::default();
    let run_edit = |leaf_value, unrelated_value| {
        let (baseline, baseline_context, baseline_reads) = rooted_demand_locality_source(1, 10, 1);
        let (edited, edited_context, edited_reads) =
            rooted_demand_locality_source(leaf_value, unrelated_value, 2);
        let mut warm = CompilerSession::with_query_concurrency(4);
        close_import_source(&mut warm, &baseline, baseline_context, baseline_reads);
        crate::queries::compile_with_session(&mut warm, &baseline, &options)
            .expect("rooted-demand baseline fresh-links");

        close_import_source(&mut warm, &edited, edited_context, edited_reads);
        let warm_output = crate::queries::compile_with_session(&mut warm, &edited, &options)
            .expect("rooted-demand warm edit fresh-links");
        let codegen_computed = warm
            .codegen_executions()
            .iter()
            .filter(|(_, execution)| *execution == rue_query::RequestExecution::Computed)
            .count();
        let codegen_reused = warm
            .codegen_executions()
            .iter()
            .filter(|(_, execution)| {
                matches!(
                    execution,
                    rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined
                )
            })
            .count();

        let mut fresh = CompilerSession::new();
        let (_, fresh_context, fresh_reads) =
            rooted_demand_locality_source(leaf_value, unrelated_value, 2);
        close_import_source(&mut fresh, &edited, fresh_context, fresh_reads);
        let fresh_output = crate::queries::compile_with_session(&mut fresh, &edited, &options)
            .expect("fresh rooted-demand edit fresh-links");
        assert_eq!(warm_output.elf, fresh_output.elf);
        assert_eq!(warm_output.warnings, fresh_output.warnings);

        Locality {
            bodies_computed: warm_output
                .work
                .semantic
                .body_analysis
                .body_analyses_computed,
            bodies_reused: warm_output.work.semantic.body_analysis.body_analyses_reused,
            cfgs_computed: warm_output.work.semantic.cfg.cfg_builds_attempted,
            codegen_computed,
            codegen_reused,
        }
    };

    let reachable = run_edit(2, 10);
    assert_eq!(reachable.bodies_computed, 1, "{reachable:?}");
    assert_eq!(reachable.bodies_reused, 1, "{reachable:?}");
    assert_eq!(reachable.cfgs_computed, 1, "{reachable:?}");
    assert_eq!(reachable.codegen_computed, 1, "{reachable:?}");
    assert_eq!(reachable.codegen_reused, 1, "{reachable:?}");

    let unrelated = run_edit(1, 11);
    assert_eq!(unrelated.bodies_computed, 0, "{unrelated:?}");
    assert_eq!(unrelated.bodies_reused, 2, "{unrelated:?}");
    assert_eq!(unrelated.cfgs_computed, 0, "{unrelated:?}");
    assert_eq!(unrelated.codegen_computed, 0, "{unrelated:?}");
    assert_eq!(unrelated.codegen_reused, 2, "{unrelated:?}");
}

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
        .rooted_cfg(&options)
        .expect("many shallow specializations must compile");
    // Finding 5: assert ALL 64 distinct specializations were analyzed, not merely
    // that at least one was.
    assert_eq!(
        wide_out
            .functions()
            .iter()
            .filter(|function| matches!(
                &function.function,
                crate::FunctionInstanceKey::Specialization { .. }
            ))
            .count(),
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
        .rooted_cfg(&options)
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
