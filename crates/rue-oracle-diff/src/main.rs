//! # rue-oracle-diff — the differential harness (RUE-50)
//!
//! Runs the concrete [`rue-cli-tests`] corpus through the [`rue_oracle`]
//! reference interpreter and checks the oracle agrees with each case's expected
//! exit code / stdout / stderr. Those expectations are what the *compiled binary*
//! already produces (the CLI suite enforces that), so a disagreement here means
//! the oracle and the compiler disagree — which, since the oracle is an
//! independent implementation of the semantics operating *before* codegen,
//! localizes a miscompile. This is the automated bug-catcher of RUE-50: it
//! turns "we check the outputs we thought to write down" into "we check that
//! the semantics and codegen agree on every program in the corpus."
//!
//! Modes:
//!
//! - **corpus** (default): `rue-oracle-diff [cases-dir ...]` runs the concrete
//!   rue-cli-tests corpus through the oracle. Case-directory resolution, in
//!   order: explicit argv paths; else the `RUE_ORACLE_DIFF_CASES` env var (a
//!   single dir — how the `buck2 test` sh_test feeds the rue-cli-tests `cases`
//!   filegroup); else the default `crates/rue-cli-tests/cases`.
//! - **spec** (`rue-oracle-diff spec [cases-dir ...]`): the same differential
//!   over the **rue-spec** corpus (RUE-204). The spec schema is richer than
//!   rue-cli-tests — templated `params`, `spec=[...]` refs, golden-IR and
//!   preview assertions — so we lean on [`rue_test_runner`] (the spec runner's
//!   own crate) to parse and template-expand every case exactly as the spec
//!   suite does, then filter to the eligible subset the oracle can model
//!   (concrete `source` + expected exit/stdout/stderr; golden-IR-only, `compile_fail`,
//!   multi-file and stdin cases are ineligible, not disagreements). Preview-gated
//!   cases run with their declared preview feature.
//!   Dir resolution mirrors corpus mode: argv; else `RUE_ORACLE_DIFF_SPEC_CASES`
//!   (how the `buck2 test` sh_test feeds the rue-spec `cases` filegroup); else
//!   `crates/rue-spec/cases`.
//! - **fuzz** (`rue-oracle-diff fuzz [...]`): the differential *fuzzer* of
//!   RUE-247 — generate random valid programs and cross-check the oracle
//!   against the real compiler + native binary. See [`fuzz`]. A `dump <seed>`
//!   subcommand prints a generated program for inspection.
//!
//! Every mode exits non-zero if an eligible program is rejected by the front
//! end, interpretation hits a resource limit or compiler/oracle contract
//! violation, or the oracle disagrees with expected behavior. Only typed
//! [`rue_oracle::ModelGapKind`] values can count as unmodeled corpus coverage;
//! each corpus mode also requires an exact registry entry.
//! Generated fuzz mode has the stronger contract that every `Unsupported` is a
//! failure because its generator promises to stay within the modeled subset.

mod fuzz;
mod generator;
mod model_gaps;
mod trap;

use rue_compiler::unstable::{
    AcceptedImportSource, DiscoverySourceAssembler, ImportDemandMode, ImportObservation,
    begin_import_input_request, close_import_input_request, import_demand_frontier_for_roots,
    import_observation_ledger, publish_import_observation_batch, stage_import_input_request,
};
use rue_compiler::{
    CompilerSession, FileMetadataFingerprint, ImportDiscoveryContext, PhysicalFileIdentity,
};
use rue_error::{PreviewFeature, PreviewFeatures};
use rue_oracle::{
    ModelGapKind, Outcome, RunSourceError, TrapKind, run_session_with_preview_features,
    run_source_with_preview_features,
};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Deserialize)]
struct TestFile {
    section: Section,
    #[serde(default, rename = "case")]
    cases: Vec<Case>,
}

/// The stable identity-bearing subset of a rue-cli-tests section. The
/// differential schema remains permissive about every field it does not use.
#[derive(Deserialize)]
struct Section {
    id: String,
}

/// A permissive subset of the rue-cli-tests case schema — only the fields the
/// oracle can act on. Unknown fields (spec references, compiler diagnostics,
/// …) are ignored.
#[derive(Deserialize)]
struct Case {
    name: String,
    #[serde(default)]
    files: Vec<SourceFile>,
    #[serde(default)]
    source_path: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    stdin: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    compile_fail: bool,
    #[serde(default)]
    compile_only: bool,
    #[serde(default)]
    watch: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    stdout: Option<String>,
    #[serde(default)]
    stdout_contains: Vec<String>,
    #[serde(default)]
    runtime_error_contains: Vec<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    known_bug: Option<String>,
    #[serde(default)]
    known_bug_on: Vec<String>,
    #[serde(default)]
    only_on: Vec<String>,
    #[serde(default)]
    skip: bool,
}

#[derive(Deserialize)]
struct SourceFile {
    #[allow(dead_code)]
    path: String,
    source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CliInvocationReason {
    MissingSourceArgument,
    SourceArgumentMismatch,
    MissingOptionValue,
    UnknownPreviewFeature,
    UnsupportedFlag,
}

impl fmt::Display for CliInvocationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingSourceArgument => "missing source argument",
            Self::SourceArgumentMismatch => "source argument mismatch",
            Self::MissingOptionValue => "missing option value",
            Self::UnknownPreviewFeature => "unknown preview feature",
            Self::UnsupportedFlag => "unsupported flag",
        })
    }
}

/// Why a loaded corpus case cannot be judged by the runtime oracle.
///
/// Declaration order is the stable, user-visible report order. Keep this
/// closed: a new exclusion path must name its reason instead of disappearing
/// into an aggregate catch-all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum IneligibleReason {
    ExplicitSkip,
    HostFiltered,
    PreviewExpectedFailure,
    ExpectedCompileFailure,
    CompileOnly,
    ApplicableKnownBug,
    WatchOrchestration,
    StandardInput,
    CompilerEnvironment,
    ExternalSourcePath,
    NoInlineSource,
    MultipleSourceFiles,
    CliInvocation(CliInvocationReason),
    AuxiliaryFiles,
    GoldenIrOnly,
    TargetPinned,
    MissingExecutionExpectation,
    KnownOracleGap,
}

impl fmt::Display for IneligibleReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExplicitSkip => f.write_str("explicit skip marker"),
            Self::HostFiltered => f.write_str("host platform filter"),
            Self::PreviewExpectedFailure => f.write_str("preview expected failure"),
            Self::ExpectedCompileFailure => f.write_str("expected compile failure"),
            Self::CompileOnly => f.write_str("compile-only case"),
            Self::ApplicableKnownBug => f.write_str("applicable known bug"),
            Self::WatchOrchestration => f.write_str("watch orchestration"),
            Self::StandardInput => f.write_str("standard input required"),
            Self::CompilerEnvironment => f.write_str("compiler environment"),
            Self::ExternalSourcePath => f.write_str("external source path"),
            Self::NoInlineSource => f.write_str("no inline source"),
            Self::MultipleSourceFiles => f.write_str("multiple source files"),
            Self::CliInvocation(reason) => write!(f, "CLI invocation ({reason})"),
            Self::AuxiliaryFiles => f.write_str("auxiliary source files"),
            Self::GoldenIrOnly => f.write_str("golden IR only"),
            Self::TargetPinned => f.write_str("target-pinned execution"),
            Self::MissingExecutionExpectation => f.write_str("missing execution expectation"),
            Self::KnownOracleGap => f.write_str("known oracle gap"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CaseOutcome {
    Agree,
    /// The case has an executable shape, but the oracle does not model it yet.
    Unmodeled(UnmodeledReason),
    /// The case does not describe a single concrete execution this harness can drive.
    Ineligible(IneligibleReason),
    /// An eligible case was rejected by the shared compiler front end.
    FrontendFailure(String),
    /// Interpretation hit a resource limit or compiler/oracle contract breach.
    OracleFailure(String),
    /// The oracle ran but disagreed with the expected behavior.
    Disagreement(String),
}

/// Why an otherwise executable case could not be fully judged.
///
/// Keep this closed: oracle model gaps are registry-ready typed causes, while
/// harness observation gaps remain distinct and cannot be mistaken for oracle
/// implementation debt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum UnmodeledReason {
    ModelGap(ModelGapKind),
    TrapExpectation,
}

impl fmt::Display for UnmodeledReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModelGap(kind) => write!(f, "oracle model gap ({kind:?})"),
            Self::TrapExpectation => f.write_str("runtime trap expectation"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrapComparison {
    Match,
    UnmodeledExpectation,
    UndeclaredActual(TrapKind),
    Mismatch {
        expected: TrapKind,
        actual: Option<TrapKind>,
    },
}

fn compare_trap_expectation(
    expected: trap::TrapExpectation,
    actual: Option<TrapKind>,
) -> TrapComparison {
    match expected {
        trap::TrapExpectation::Undeclared => {
            actual.map_or(TrapComparison::Match, TrapComparison::UndeclaredActual)
        }
        trap::TrapExpectation::Unmodeled => TrapComparison::UnmodeledExpectation,
        trap::TrapExpectation::Modeled(expected) if actual == Some(expected) => {
            TrapComparison::Match
        }
        trap::TrapExpectation::Modeled(expected) => TrapComparison::Mismatch { expected, actual },
    }
}

#[derive(Default)]
struct Report {
    agree: u32,
    unmodeled_reasons: BTreeMap<UnmodeledReason, u32>,
    ineligible_reasons: BTreeMap<IneligibleReason, u32>,
    frontend_failures: Vec<String>,
    oracle_failures: Vec<String>,
    disagreements: Vec<String>,
    /// corpus discovery/read/parse failures — the harness did not check all inputs
    harness_failures: Vec<String>,
}

impl Report {
    /// Record exactly one terminal classification for one successfully loaded case.
    fn record(&mut self, outcome: CaseOutcome) {
        match outcome {
            CaseOutcome::Agree => self.agree += 1,
            CaseOutcome::Unmodeled(reason) => {
                *self.unmodeled_reasons.entry(reason).or_default() += 1;
            }
            CaseOutcome::Ineligible(reason) => {
                *self.ineligible_reasons.entry(reason).or_default() += 1;
            }
            CaseOutcome::FrontendFailure(failure) => self.frontend_failures.push(failure),
            CaseOutcome::OracleFailure(failure) => self.oracle_failures.push(failure),
            CaseOutcome::Disagreement(disagreement) => self.disagreements.push(disagreement),
        }
    }

    fn modeled_eligible_count(&self) -> u32 {
        self.agree
            + self.frontend_failures.len() as u32
            + self.oracle_failures.len() as u32
            + self.disagreements.len() as u32
    }

    fn ineligible_count(&self) -> u32 {
        self.ineligible_reasons.values().sum()
    }

    fn unmodeled_count(&self) -> u32 {
        self.unmodeled_reasons.values().sum()
    }

    fn classified_case_count(&self) -> u32 {
        self.agree
            + self.unmodeled_count()
            + self.ineligible_count()
            + self.frontend_failures.len() as u32
            + self.oracle_failures.len() as u32
            + self.disagreements.len() as u32
    }

    fn ineligible_breakdown_lines(&self) -> Vec<String> {
        self.ineligible_reasons
            .iter()
            .map(|(reason, count)| format!("    {reason}: {count}"))
            .collect()
    }

    fn unmodeled_breakdown_lines(&self) -> Vec<String> {
        self.unmodeled_reasons
            .iter()
            .map(|(reason, count)| format!("    {reason}: {count}"))
            .collect()
    }
}

fn main() -> ExitCode {
    // The oracle interpreter (`rue_oracle::run_source` -> `eval`) recurses per
    // expression, so a deeply-nested but valid corpus program (e.g. the depth-60
    // `deep_nesting` case) can exhaust the default main-thread stack — which is
    // smaller on macOS than on Linux and overflows there. Run the whole harness
    // on a dedicated thread with a large stack so deep-but-valid programs are
    // interpreted without overflowing (RUE-227/236 re-land; deep_nesting re-enabled).
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(run)
        .expect("spawn oracle-diff worker thread")
        .join()
        .expect("oracle-diff worker thread panicked")
}

fn run() -> ExitCode {
    // Subcommand dispatch: `rue-oracle-diff fuzz [...]` runs the differential
    // *fuzzer* (generate valid programs, cross-check oracle vs compiled binary);
    // with no subcommand it runs the corpus differential (below).
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("fuzz") {
        return fuzz::run(&raw[1..]);
    }
    // `spec [cases-dir ...]` runs the differential over the rue-spec corpus
    // (templated cases expanded via rue-test-runner) instead of rue-cli-tests.
    if raw.first().map(String::as_str) == Some("spec") {
        return spec_mode(raw[1..].to_vec());
    }
    // `dump <seed>...` prints the generated program(s) — a debugging aid for
    // inspecting what a seed produces (and reducing a repro by hand).
    if raw.first().map(String::as_str) == Some("dump") {
        for s in &raw[1..] {
            match s.parse::<u64>() {
                Ok(seed) => {
                    println!("// ===== seed {seed} =====");
                    print!("{}", generator::generate(seed));
                }
                Err(_) => eprintln!("dump: not a seed: {s}"),
            }
        }
        return ExitCode::SUCCESS;
    }

    corpus_mode(raw)
}

/// The original corpus differential: run each rue-cli-tests case through the
/// oracle and check it agrees with the case's expected exit code / stdout /
/// stderr.
fn corpus_mode(raw_args: Vec<String>) -> ExitCode {
    let inventory_scope = if raw_args.is_empty() {
        model_gaps::InventoryScope::Authoritative
    } else {
        model_gaps::InventoryScope::Partial
    };
    let dirs: Vec<PathBuf> = {
        let args: Vec<String> = raw_args;
        if !args.is_empty() {
            args.into_iter().map(PathBuf::from).collect()
        } else if let Some(cases) = std::env::var_os("RUE_ORACLE_DIFF_CASES") {
            // Set by the `//crates/rue-oracle-diff:oracle-diff-test` sh_test to
            // the rue-cli-tests `cases` filegroup's absolute location.
            vec![PathBuf::from(cases)]
        } else {
            vec![PathBuf::from("crates/rue-cli-tests/cases")]
        }
    };

    let mut report = Report::default();
    let mut gap_audit = model_gaps::cli::audit(inventory_scope);
    let (toml_files, discovery_failures) = discover_toml(&dirs);
    let mut inventory_complete = discovery_failures.is_empty();
    report.harness_failures.extend(discovery_failures);
    if toml_files.is_empty() && report.harness_failures.is_empty() {
        inventory_complete = false;
        report.harness_failures.push(format!(
            "no .toml case files found under {dirs:?} (run from the repo root)"
        ));
    }

    for path in &toml_files {
        let file = match load_cli_test_file(path) {
            Ok(file) => file,
            Err(failure) => {
                inventory_complete = false;
                report.harness_failures.push(failure);
                continue;
            }
        };
        for case in &file.cases {
            let outcome = check_case(path, case);
            gap_audit.observe(
                model_gaps::cli::CaseId::new(&file.section.id, &case.name),
                &case.only_on,
                model_gap_observation(case, &outcome),
            );
            report.record(outcome);
        }
    }
    report
        .harness_failures
        .extend(gap_audit.finish(inventory_complete));
    report.harness_failures.sort();

    finish_report(&report, "rue-cli-tests")
}

/// Translate the closed corpus classification into equally closed registry
/// policy. In particular, trap observation gaps are not
/// [`ModelGapKind`] values and cannot be accepted by the typed registry.
fn model_gap_observation(case: &Case, outcome: &CaseOutcome) -> model_gaps::Observation {
    // Eligibility wrappers deliberately prevent inactive cases from making
    // claims on this host. For every active executable case, however, audit
    // its declared trap contract independently of whether interpretation
    // produced an Outcome or stopped first at a typed model gap. Otherwise an
    // Unsupported result could hide unrelated harness observation debt.
    if !matches!(outcome, CaseOutcome::Ineligible(_))
        && trap::cli_trap_expectation(case.runtime_error_contains.iter().map(String::as_str))
            == trap::TrapExpectation::Unmodeled
    {
        return model_gaps::Observation::Unregistrable("runtime trap expectation");
    }

    match outcome {
        CaseOutcome::Agree => model_gaps::Observation::Agreement,
        CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(kind)) => {
            model_gaps::Observation::ModelGap(*kind)
        }
        CaseOutcome::Unmodeled(UnmodeledReason::TrapExpectation) => {
            model_gaps::Observation::Unregistrable("runtime trap expectation")
        }
        CaseOutcome::Ineligible(IneligibleReason::HostFiltered) => {
            model_gaps::Observation::InactiveHost
        }
        CaseOutcome::Ineligible(_) => model_gaps::Observation::OtherIneligible,
        CaseOutcome::FrontendFailure(_)
        | CaseOutcome::OracleFailure(_)
        | CaseOutcome::Disagreement(_) => model_gaps::Observation::HardFailure,
    }
}

/// Print the tallied report and turn it into a process exit code: success when
/// the oracle agreed with the compiler on every modeled eligible case, failure
/// on any harness/front-end/oracle failure or disagreement. Shared by
/// [`corpus_mode`] and [`spec_mode`]; `corpus` names which corpus was audited,
/// for the header.
fn finish_report(report: &Report, corpus: &str) -> ExitCode {
    let total = report.classified_case_count();
    println!("\n=== rue-oracle-diff: {corpus} corpus classification audit ({total} cases) ===");
    println!("  agree (modeled eligible): {}", report.agree);
    println!("  unmodeled eligible:       {}", report.unmodeled_count());
    for line in report.unmodeled_breakdown_lines() {
        println!("{line}");
    }
    println!("  ineligible:               {}", report.ineligible_count());
    for line in report.ineligible_breakdown_lines() {
        println!("{line}");
    }
    println!("  HARNESS FAILURES: {}", report.harness_failures.len());
    for failure in &report.harness_failures {
        println!("\n  ✗ {failure}");
    }
    println!("  FRONTEND FAILURES: {}", report.frontend_failures.len());
    for failure in &report.frontend_failures {
        println!("\n  ✗ {failure}");
    }
    println!("  ORACLE FAILURES:   {}", report.oracle_failures.len());
    for failure in &report.oracle_failures {
        println!("\n  ✗ {failure}");
    }
    println!("  DISAGREEMENTS:    {}", report.disagreements.len());
    for d in &report.disagreements {
        println!("\n  ✗ {d}");
    }

    // Ineligible and unmodeled cases do not judge compiler semantics. A corpus
    // containing only those outcomes must not turn this differential into a
    // vacuous green check (the RUE-341 coverage-overstatement hazard).
    if report.modeled_eligible_count() == 0 {
        println!(
            "\nno modeled eligible {corpus} cases were judged — corpus empty, \
             misconfigured, or entirely outside oracle coverage."
        );
        return ExitCode::FAILURE;
    }

    if report.harness_failures.is_empty()
        && report.frontend_failures.is_empty()
        && report.oracle_failures.is_empty()
        && report.disagreements.is_empty()
    {
        println!("\noracle agrees with the compiler on every modeled eligible case.");
        ExitCode::SUCCESS
    } else {
        println!(
            "\n{} harness failure(s), {} frontend failure(s), {} oracle failure(s), {} \
             disagreement(s) — \
             corpus inputs must load and modeled eligible cases must compile before the \
             oracle can check them.",
            report.harness_failures.len(),
            report.frontend_failures.len(),
            report.oracle_failures.len(),
            report.disagreements.len()
        );
        ExitCode::FAILURE
    }
}

/// The spec-corpus differential (RUE-204): expand each rue-spec templated case
/// and run the eligible subset through the oracle, checking three-way agreement
/// (oracle == expected == compiler).
///
/// Parsing and `params` template-expansion are delegated to
/// [`rue_test_runner::load_test_files`] — the exact code the spec suite uses —
/// so template semantics can never drift from the real runner. We then filter
/// to the shapes the single-source oracle can model (see [`check_spec_case`]).
fn spec_mode(raw_args: Vec<String>) -> ExitCode {
    let inventory_scope = if raw_args.is_empty() {
        model_gaps::InventoryScope::Authoritative
    } else {
        model_gaps::InventoryScope::Partial
    };
    let dirs: Vec<PathBuf> = if !raw_args.is_empty() {
        raw_args.into_iter().map(PathBuf::from).collect()
    } else if let Some(cases) = std::env::var_os("RUE_ORACLE_DIFF_SPEC_CASES") {
        // Set by the `//crates/rue-oracle-diff:oracle-diff-spec-test` sh_test to
        // the rue-spec `cases` filegroup's absolute location.
        vec![PathBuf::from(cases)]
    } else {
        vec![PathBuf::from("crates/rue-spec/cases")]
    };

    let mut report = Report::default();
    let mut gap_audit = model_gaps::spec::audit(inventory_scope);
    let mut inventory_complete = true;
    for dir in &dirs {
        if !dir.exists() {
            inventory_complete = false;
            report.harness_failures.push(format!(
                "no rue-spec cases dir found under {} (run from the repo root)",
                dir.display()
            ));
            continue;
        }

        // `load_test_files` expands `params` templates and validates preview
        // feature names exactly as the spec runner does; it needs only the
        // cases dir (no spec markdown).
        let files = match rue_test_runner::load_test_files(dir) {
            Ok(files) => files,
            Err(error) => {
                inventory_complete = false;
                report.harness_failures.push(error);
                continue;
            }
        };
        if files.is_empty() {
            inventory_complete = false;
            report.harness_failures.push(format!(
                "no rue-spec case files found under {}",
                dir.display()
            ));
        }
        for (ident, file) in files {
            for case in &file.case {
                let outcome = check_spec_case(&ident, case);
                gap_audit.observe(
                    model_gaps::spec::CaseId::new(&file.section.id, &case.name),
                    &case.only_on,
                    spec_model_gap_observation(case, &outcome),
                );
                report.record(outcome);
            }
        }
    }
    report
        .harness_failures
        .extend(gap_audit.finish(inventory_complete));
    report.harness_failures.sort();

    finish_report(&report, "rue-spec")
}

/// Translate a spec outcome into exact model-gap registry policy while
/// independently auditing observations the interpreter cannot currently make.
fn spec_model_gap_observation(
    case: &rue_test_runner::Case,
    outcome: &CaseOutcome,
) -> model_gaps::Observation {
    if !matches!(outcome, CaseOutcome::Ineligible(_)) {
        if trap::trap_expectation(case.runtime_error.iter().map(String::as_str))
            == trap::TrapExpectation::Unmodeled
        {
            return model_gaps::Observation::Unregistrable("runtime trap expectation");
        }
    }

    match outcome {
        CaseOutcome::Agree => model_gaps::Observation::Agreement,
        CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(kind)) => {
            model_gaps::Observation::ModelGap(*kind)
        }
        CaseOutcome::Unmodeled(UnmodeledReason::TrapExpectation) => {
            model_gaps::Observation::Unregistrable("runtime trap expectation")
        }
        CaseOutcome::Ineligible(IneligibleReason::HostFiltered) => {
            model_gaps::Observation::InactiveHost
        }
        CaseOutcome::Ineligible(_) => model_gaps::Observation::OtherIneligible,
        CaseOutcome::FrontendFailure(_)
        | CaseOutcome::OracleFailure(_)
        | CaseOutcome::Disagreement(_) => model_gaps::Observation::HardFailure,
    }
}

/// Run one expanded rue-spec case through the oracle and return one outcome.
///
/// Ineligible (never a disagreement) when the case is not
/// a single concrete program the oracle can execute and compare: `compile_fail`
/// (no runtime output), `compile_only` (never executed), golden-IR-only (an IR
/// dump, not a run), or a shape with no oracle model (stdin, multi-file
/// `aux_files`). Preview-gated cases run with their declared preview feature;
/// preview cases the real runner explicitly allows to fail remain ineligible.
/// A typed oracle model gap counts as unmodeled; resource exhaustion or a
/// compiler/oracle contract violation fails the harness. [`RunSourceError::Compile`]
/// for a case that survived these shape filters is likewise a hard front-end
/// failure.
fn check_spec_case(ident: &str, case: &rue_test_runner::Case) -> CaseOutcome {
    let is_known_gap = KNOWN_ORACLE_GAPS
        .iter()
        .any(|(i, n, _)| *i == ident && *n == case.name);
    check_spec_case_with_known_gap(ident, case, is_known_gap)
}

fn run_source_with_real_std(
    source: &str,
    preview_features: &PreviewFeatures,
) -> Result<Result<Outcome, RunSourceError>, String> {
    let std_root = std::env::var_os("RUE_ORACLE_DIFF_STD")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("std"));
    let std_root = std_root
        .canonicalize()
        .map_err(|error| format!("cannot locate real std at {}: {error}", std_root.display()))?;
    let context = ImportDiscoveryContext::new(
        1,
        "/oracle",
        Some(std_root.to_string_lossy().as_ref()),
        "oracle-real-std",
    )
    .map_err(|error| error.to_string())?;
    let root_source = Arc::new(source.to_owned());
    let root_len = root_source.len() as u64;
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/oracle/test.rue",
        "/oracle/test.rue",
        PhysicalFileIdentity::new(1, 1),
        FileMetadataFingerprint::new(root_len, 0, 0),
        root_source,
    )
    .map_err(|error| error.to_string())?;
    let mut session = CompilerSession::new();
    let mut identities = BTreeMap::<PathBuf, PhysicalFileIdentity>::new();
    let initial = assembler.snapshot().map_err(|error| error.to_string())?;
    let mut revision = begin_import_input_request(
        &mut session,
        &initial,
        context.clone(),
        assembler.accepted_read_manifest(),
    )
    .map_err(|error| error.to_string())?;

    loop {
        let ledger =
            import_observation_ledger(&session, revision).map_err(|error| error.to_string())?;
        let plan = stage_import_input_request(&mut session, revision)
            .map_err(|errors| format!("{errors:#?}"))?;
        let frontier = import_demand_frontier_for_roots(
            &mut session,
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .map_err(|error| error.to_string())?;
        if frontier.requests().is_empty() {
            close_import_input_request(&mut session, revision)
                .map_err(|errors| format!("{errors:#?}"))?;
            return Ok(run_session_with_preview_features(session, preview_features));
        }
        let mut observations = Vec::new();
        for request in frontier.requests().iter().cloned() {
            let requested = request.requested_path().to_owned();
            let path = Path::new(&requested);
            let observation = match path.canonicalize() {
                Ok(canonical) if canonical.is_file() => {
                    let source = fs::read_to_string(&canonical).map_err(|error| {
                        format!(
                            "cannot read imported source {}: {error}",
                            canonical.display()
                        )
                    })?;
                    let next = (identities.len() + 2) as u64;
                    let identity = *identities
                        .entry(canonical.clone())
                        .or_insert_with(|| PhysicalFileIdentity::new(1, next));
                    let fingerprint = FileMetadataFingerprint::new(source.len() as u64, 0, 0);
                    let accepted = AcceptedImportSource::new(
                        requested,
                        canonical.to_string_lossy().into_owned(),
                        identity,
                        fingerprint,
                        Arc::new(source),
                    )
                    .map_err(|error| error.to_string())?;
                    ImportObservation::accepted(request, accepted)
                        .map_err(|error| error.to_string())?
                }
                _ => ImportObservation::absent(request),
            };
            observations.push(observation);
        }
        let mut assembly_ledger = ledger;
        for observation in observations.iter().cloned() {
            assembly_ledger
                .record(observation)
                .map_err(|error| error.to_string())?;
        }
        assembler
            .add_plan_reads(&plan, &assembly_ledger)
            .map_err(|error| error.to_string())?;
        let successor = assembler.snapshot().map_err(|error| error.to_string())?;
        revision = publish_import_observation_batch(
            &mut session,
            &frontier,
            &successor,
            assembler.accepted_read_manifest(),
            observations,
        )
        .map_err(|error| error.to_string())?;
    }
}

fn check_spec_case_with_known_gap(
    ident: &str,
    case: &rue_test_runner::Case,
    is_known_gap: bool,
) -> CaseOutcome {
    // Mirror the real rue-spec wrapper before classifying case shapes. Preview
    // cases without `preview_should_pass` use xfail semantics there, so they do
    // not constitute required compiler/oracle agreement here either.
    if case.skip {
        return CaseOutcome::Ineligible(IneligibleReason::ExplicitSkip);
    }
    if rue_test_runner::should_skip_for_platform(&case.only_on).is_some() {
        return CaseOutcome::Ineligible(IneligibleReason::HostFiltered);
    }
    if case.preview.is_some() && !case.preview_should_pass {
        return CaseOutcome::Ineligible(IneligibleReason::PreviewExpectedFailure);
    }
    if case.compile_fail {
        return CaseOutcome::Ineligible(IneligibleReason::ExpectedCompileFailure);
    }
    if case.compile_only {
        return CaseOutcome::Ineligible(IneligibleReason::CompileOnly);
    }
    if case.stdin.is_some() {
        return CaseOutcome::Ineligible(IneligibleReason::StandardInput);
    }
    if !case.aux_files.is_empty() {
        return CaseOutcome::Ineligible(IneligibleReason::AuxiliaryFiles);
    }
    if case.has_golden_ir_assertions() && !case.has_execution_assertions() {
        return CaseOutcome::Ineligible(IneligibleReason::GoldenIrOnly);
    }
    // A target-pinned case's expected exit is target-specific (for example,
    // it may branch on `@target_arch()`). The in-process oracle has no target
    // context, so it cannot reproduce that execution contract.
    if case.target.is_some() {
        return CaseOutcome::Ineligible(IneligibleReason::TargetPinned);
    }

    // The exit code the compiled binary is expected to produce. A runtime-error
    // case exits with its runtime exit code (101 by convention); otherwise the
    // case's explicit `exit_code`. A case with neither (e.g. a warnings-only
    // check) describes no concrete run to diff against.
    let expected_exit = if case.runtime_error.is_some() {
        case.runtime_exit_code
            .unwrap_or(rue_test_runner::RUNTIME_ERROR_EXIT_CODE)
    } else if let Some(code) = case.exit_code {
        code
    } else {
        return CaseOutcome::Ineligible(IneligibleReason::MissingExecutionExpectation);
    };

    let preview_features = spec_preview_features(case);
    let expected_trap = case.runtime_error.as_deref().map_or_else(
        || {
            if expected_exit == rue_test_runner::RUNTIME_ERROR_EXIT_CODE {
                case.stderr_contains
                    .as_deref()
                    .and_then(trap::spec_stderr_trap_kind)
                    .map_or(
                        trap::TrapExpectation::Undeclared,
                        trap::TrapExpectation::Modeled,
                    )
            } else {
                trap::TrapExpectation::Undeclared
            }
        },
        |runtime_error| trap::trap_expectation([runtime_error]),
    );

    let oracle_result = if case.real_std {
        match run_source_with_real_std(&case.source, &preview_features) {
            Ok(result) => result,
            Err(error) => {
                return CaseOutcome::FrontendFailure(format!(
                    "{ident} :: {}\n      {error}",
                    case.name
                ));
            }
        }
    } else {
        run_source_with_preview_features(&case.source, &preview_features)
    };
    match oracle_result {
        // Only Unsupported values carrying a registrable ModelGapKind count as
        // coverage. Resource/contract failures and eligible compile rejections
        // fail rather than disappearing as unmodeled.
        Err(error) => {
            let context = format!("{ident} :: {}", case.name);
            record_oracle_error(&context, error)
        }
        Ok(outcome) => {
            let trap_comparison = compare_trap_expectation(expected_trap, outcome.panic);
            // See the CLI path: a case whose *only* observable contract is the
            // runtime-error exit code (101) — no `runtime_error`, no
            // `stderr_contains` — is asserting termination by trap, so an oracle
            // trap at the same exit satisfies that contract rather than diverging
            // from it. A case that *does* declare stderr (even in the narrow
            // spec-vocabulary sense) keeps the strict undeclared-trap contract so
            // its declaration is still verified.
            let trap_declared_by_exit = expected_exit == rue_test_runner::RUNTIME_ERROR_EXIT_CODE
                && case.runtime_error.is_none()
                && case.stderr_contains.is_none();
            if let TrapComparison::UndeclaredActual(actual) = trap_comparison {
                if !trap_declared_by_exit {
                    return CaseOutcome::Disagreement(format!(
                        "{ident} :: {}\n      trap contract: oracle got {actual:?}, but the case \
                         declares no runtime trap cause",
                        case.name
                    ));
                }
            }
            let exit_ok = outcome.exit_code == expected_exit;
            // Compare stdout the way the spec runner itself does: byte-exact
            // modulo the single `"""` block-boundary newline. NOT
            // normalize_golden — that trims per-line trailing whitespace and
            // boundary blank lines, which would record a real
            // whitespace-shaped oracle-vs-compiler divergence as agreement
            // (RUE-132's byte-exactness rule applies here too).
            let stdout_ok = case.expected_stdout.as_ref().is_none_or(|s| {
                rue_test_runner::strip_block_boundary_newlines(&outcome.stdout)
                    == rue_test_runner::strip_block_boundary_newlines(s)
            });
            // Match the real spec runner's two stderr paths: `runtime_error`
            // checks its own substring and returns early there; otherwise the
            // generic `stderr_contains` assertion participates.
            let expected_stderr = case
                .runtime_error
                .as_ref()
                .or(case.stderr_contains.as_ref());
            let stderr_ok =
                expected_stderr.is_none_or(|expected| outcome.stderr.contains(expected));
            let trap_ok = trap_comparison == TrapComparison::Match
                || matches!(trap_comparison, TrapComparison::UndeclaredActual(_))
                    && trap_declared_by_exit;
            let modeled_observations_agree = exit_ok && stdout_ok && stderr_ok && trap_ok;

            if is_known_gap {
                // xfail semantics (mirroring rue-cli-tests `known_bug`): the case
                // is expected to diverge because of a tracked wrong-but-Ok
                // oracle result. If every observation the oracle models now
                // agrees, fail loudly so the stale exemption is removed and
                // any remaining coverage gap is classified normally.
                return if modeled_observations_agree {
                    CaseOutcome::Disagreement(format!(
                        "{ident} :: {} — KNOWN_ORACLE_GAPS entry is stale: every observation \
                         the oracle models now agrees; the tracked wrong-result gap is fixed, \
                         so delete its entry in crates/rue-oracle-diff/src/main.rs and let any \
                         remaining coverage gap be classified normally",
                        case.name
                    ))
                } else {
                    // Preserve the existing policy: a known wrong-but-Ok oracle
                    // result is excluded from the audit as ineligible, rather
                    // than being conflated with Unsupported/unmodeled coverage.
                    CaseOutcome::Ineligible(IneligibleReason::KnownOracleGap)
                };
            }

            // A missing trap model cannot erase a concrete exit/stdout/stderr
            // disagreement or bypass the known-gap registry above. Only
            // classify it as coverage when every independently modeled
            // observation agrees.
            if exit_ok
                && stdout_ok
                && stderr_ok
                && trap_comparison == TrapComparison::UnmodeledExpectation
            {
                return CaseOutcome::Unmodeled(UnmodeledReason::TrapExpectation);
            }

            if modeled_observations_agree {
                CaseOutcome::Agree
            } else {
                let mut msg = format!("{ident} :: {}", case.name);
                if !exit_ok {
                    msg += &format!(
                        "\n      exit: expected {expected_exit}, oracle got {}",
                        outcome.exit_code
                    );
                }
                if !stdout_ok {
                    msg += &format!(
                        "\n      stdout: expected {:?}, oracle got {:?}",
                        case.expected_stdout.as_deref().unwrap_or(""),
                        outcome.stdout
                    );
                }
                if !stderr_ok {
                    msg += &format!(
                        "\n      stderr: missing required substring {:?} in {:?}",
                        expected_stderr.map(String::as_str).unwrap_or(""),
                        outcome.stderr
                    );
                }
                if let TrapComparison::Mismatch { expected, actual } = trap_comparison {
                    msg += &format!("\n      trap: expected {expected:?}, oracle got {actual:?}");
                }
                CaseOutcome::Disagreement(msg)
            }
        }
    }
}

fn spec_preview_features(case: &rue_test_runner::Case) -> PreviewFeatures {
    let mut features = empty_preview_features();
    if let Some(feature_name) = &case.preview {
        let feature = PreviewFeature::from_str(feature_name)
            .expect("rue-test-runner validates preview feature names while loading cases");
        features.insert(feature);
    }
    features
}

/// Spec-corpus cases the oracle is known to model incorrectly (returning a
/// wrong-but-`Ok` result rather than [`RunSourceError::Unsupported`]), each
/// paired with the tracking issue. These are **not** miscompiles — the compiler
/// is correct; the oracle is. They are carried as xfails (see
/// [`check_spec_case`]): each is asserted to *still* diverge, so when its
/// tracked bug is fixed the harness fails and points at the entry to delete.
/// Keep this list tiny — it exists only to keep CI green over a documented,
/// tracked oracle limitation, never to paper over a real codegen disagreement.
///
/// Entry: `(section-identifier, case-name, tracking-issue)`.
const KNOWN_ORACLE_GAPS: &[(&str, &str, &str)] = &[
    // (empty) — RUE-285 fixed: the oracle now models structural ==/!= over
    // structs, arrays, and payload enums, so the former aggregate-equality gaps
    // run as normal three-way-agreement checks again.
];

fn check_case(path: &Path, case: &Case) -> CaseOutcome {
    // Mirror the real CLI runner's wrapper order: explicit and host-filtered
    // cases never reach invocation parsing or execution.
    if case.skip {
        return CaseOutcome::Ineligible(IneligibleReason::ExplicitSkip);
    }
    if rue_test_runner::should_skip_for_platform(&case.only_on).is_some() {
        return CaseOutcome::Ineligible(IneligibleReason::HostFiltered);
    }

    let known_bug_applies = case.known_bug.is_some()
        && (case.known_bug_on.is_empty()
            || case
                .known_bug_on
                .iter()
                .any(|platform| platform == rue_test_runner::get_host_target()));

    if case.compile_fail {
        return CaseOutcome::Ineligible(IneligibleReason::ExpectedCompileFailure);
    }
    if case.compile_only {
        return CaseOutcome::Ineligible(IneligibleReason::CompileOnly);
    }
    if known_bug_applies {
        return CaseOutcome::Ineligible(IneligibleReason::ApplicableKnownBug);
    }
    // A watch case is an imperative sequence of filesystem edits and driver
    // publications, not one concrete source execution the in-process oracle
    // can compare. The CLI harness owns this orchestration contract.
    if case.watch.is_some() {
        return CaseOutcome::Ineligible(IneligibleReason::WatchOrchestration);
    }
    if case.stdin.is_some() {
        return CaseOutcome::Ineligible(IneligibleReason::StandardInput);
    }
    // The canonical real-std opt-in is reproducible through the compiler-owned
    // import-discovery path below. Other CLI process environments remain
    // outside this in-process harness.
    let real_std = case.env.len() == 1
        && case.env.get("RUE_STD_PATH").map(String::as_str) == Some("${REAL_STD}");
    if !case.env.is_empty() && !real_std {
        return CaseOutcome::Ineligible(IneligibleReason::CompilerEnvironment);
    }
    // A stack-overflow trap (deep/unbounded recursion) is structurally
    // un-modelable: reproducing it means actually exhausting the machine stack,
    // which the in-process interpreter cannot do — its own recursion budget
    // aborts first with a ResourceLimit. So a case expecting this trap is an
    // oracle gap, not a case the harness can judge (RUE-645).
    if case
        .runtime_error_contains
        .iter()
        .any(|fragment| fragment == "stack overflow")
    {
        return CaseOutcome::Ineligible(IneligibleReason::KnownOracleGap);
    }
    // `source_path` names a repository input that the oracle-diff Buck target
    // does not own; do not silently substitute an inline file.
    if case.source_path.is_some() {
        return CaseOutcome::Ineligible(IneligibleReason::ExternalSourcePath);
    }
    if case.files.is_empty() {
        return CaseOutcome::Ineligible(IneligibleReason::NoInlineSource);
    }
    if case.files.len() > 1 {
        return CaseOutcome::Ineligible(IneligibleReason::MultipleSourceFiles);
    }

    let preview_features = match corpus_preview_features(case) {
        Ok(features) => features,
        Err(reason) => {
            return CaseOutcome::Ineligible(IneligibleReason::CliInvocation(reason));
        }
    };
    let source = &case.files[0].source;
    let expected_trap =
        trap::cli_trap_expectation(case.runtime_error_contains.iter().map(String::as_str));

    // A program that expects a runtime panic exits 101 by convention.
    let expected_exit = case
        .exit_code
        .unwrap_or(if case.runtime_error_contains.is_empty() {
            0
        } else {
            101
        });

    let oracle_result = if real_std {
        match run_source_with_real_std(source, &preview_features) {
            Ok(result) => result,
            Err(error) => {
                return CaseOutcome::FrontendFailure(format!(
                    "{} :: {}\n      {error}",
                    rel(path),
                    case.name
                ));
            }
        }
    } else {
        run_source_with_preview_features(source, &preview_features)
    };
    match oracle_result {
        Err(error) => {
            let context = format!("{} :: {}", rel(path), case.name);
            record_oracle_error(&context, error)
        }
        Ok(outcome) => {
            let trap_comparison = compare_trap_expectation(expected_trap, outcome.panic);
            // A case whose only observable contract is the runtime-error exit
            // code (101) — no `runtime_error_contains` trap declaration — is
            // nonetheless asserting termination *by trap*. An oracle trap whose
            // exit matches is therefore consistent with that contract, not a
            // divergence, so fall through to the exit/stdout/stderr comparison
            // instead of reporting an undeclared-trap disagreement. A trap where
            // the case pins a non-101 exit is still caught by the exit check
            // below. (Verified by hand that the compiled binary and the oracle
            // emit the same trap for the affected slice-bounds and
            // allocation-overflow corpus cases.)
            let trap_declared_by_exit = expected_exit == rue_test_runner::RUNTIME_ERROR_EXIT_CODE
                && case.runtime_error_contains.is_empty();
            if let TrapComparison::UndeclaredActual(actual) = trap_comparison {
                if !trap_declared_by_exit {
                    return CaseOutcome::Disagreement(format!(
                        "{} :: {}\n      trap contract: oracle got {actual:?}, but the case \
                         declares no runtime trap cause",
                        rel(path),
                        case.name
                    ));
                }
            }
            let exit_ok = outcome.exit_code == expected_exit;
            let stdout_ok = case.stdout.as_ref().is_none_or(|s| &outcome.stdout == s);
            let missing_stdout = case
                .stdout_contains
                .iter()
                .find(|expected| !outcome.stdout.contains(expected.as_str()));
            let missing_stderr = case
                .runtime_error_contains
                .iter()
                .find(|expected| !outcome.stderr.contains(expected.as_str()));
            // A `runtime_error_contains` fragment is a *substring* contract, and
            // the same bare phrase can be spelled either by a runtime trap
            // (`error: index out of bounds`) or by a user panic
            // (`@panic("index out of bounds")`): the fragment alone does not pin
            // the cause. So a `UserPanic` whose stderr satisfies every declared
            // fragment at the runtime-error exit code discharges the contract —
            // the std container bridges deliberately spell their bounds check as
            // `@panic("index out of bounds")` pending a source-accessible
            // bounds-trap primitive, and the native binary reports the very same
            // `UserPanic`, so accepting it matches the compiler rather than
            // masking a divergence. Genuine runtime-trap cases still match by
            // kind, and any exit/stderr divergence still fails above.
            let user_panic_substring_contract = matches!(
                trap_comparison,
                TrapComparison::Mismatch {
                    actual: Some(TrapKind::UserPanic),
                    ..
                }
            ) && expected_exit
                == rue_test_runner::RUNTIME_ERROR_EXIT_CODE
                && !case.runtime_error_contains.is_empty()
                && missing_stderr.is_none();
            let trap_ok = trap_comparison == TrapComparison::Match
                || matches!(trap_comparison, TrapComparison::UndeclaredActual(_))
                    && trap_declared_by_exit
                || user_panic_substring_contract;
            if exit_ok
                && stdout_ok
                && missing_stdout.is_none()
                && missing_stderr.is_none()
                && trap_comparison == TrapComparison::UnmodeledExpectation
            {
                return CaseOutcome::Unmodeled(UnmodeledReason::TrapExpectation);
            }
            if exit_ok
                && stdout_ok
                && missing_stdout.is_none()
                && missing_stderr.is_none()
                && trap_ok
            {
                CaseOutcome::Agree
            } else {
                let mut msg = format!("{} :: {}", rel(path), case.name);
                if !exit_ok {
                    msg += &format!(
                        "\n      exit: expected {expected_exit}, oracle got {}",
                        outcome.exit_code
                    );
                }
                if !stdout_ok {
                    msg += &format!(
                        "\n      stdout: expected {:?}, oracle got {:?}",
                        case.stdout.as_deref().unwrap_or(""),
                        outcome.stdout
                    );
                }
                if let Some(expected) = missing_stdout {
                    msg += &format!(
                        "\n      stdout: missing required substring {expected:?} in {:?}",
                        outcome.stdout
                    );
                }
                if let Some(expected) = missing_stderr {
                    msg += &format!(
                        "\n      stderr: missing required substring {expected:?} in {:?}",
                        outcome.stderr
                    );
                }
                if let TrapComparison::Mismatch { expected, actual } = trap_comparison {
                    msg += &format!("\n      trap: expected {expected:?}, oracle got {actual:?}");
                }
                CaseOutcome::Disagreement(msg)
            }
        }
    }
}

/// Classify an oracle error without conflating a compiler rejection, a
/// registrable interpreter coverage gap, and a hard oracle failure. Corpus/spec
/// callers prefilter intentional `compile_fail` cases, so any
/// [`RunSourceError::Compile`] reaching this helper is an unexpected front-end
/// failure in an eligible program.
fn record_oracle_error(context: &str, error: RunSourceError) -> CaseOutcome {
    match error {
        RunSourceError::Compile(errors) => {
            CaseOutcome::FrontendFailure(format!("{context}\n      {errors:#?}"))
        }
        RunSourceError::Unsupported(unsupported) => {
            if let Some(kind) = unsupported.model_gap() {
                CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(kind))
            } else {
                CaseOutcome::OracleFailure(format!(
                    "{context}\n      {:?}: {}",
                    unsupported.kind(),
                    unsupported.detail()
                ))
            }
        }
    }
}

fn empty_preview_features() -> PreviewFeatures {
    PreviewFeatures::new()
}

fn corpus_preview_features(case: &Case) -> Result<PreviewFeatures, CliInvocationReason> {
    let Some(args) = &case.args else {
        return Ok(empty_preview_features());
    };
    let only_source = case
        .files
        .first()
        .ok_or(CliInvocationReason::MissingSourceArgument)?;
    let mut features = empty_preview_features();
    let mut saw_source = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--preview" => {
                let name = args
                    .get(i + 1)
                    .ok_or(CliInvocationReason::MissingOptionValue)?;
                let feature = PreviewFeature::from_str(name)
                    .map_err(|_| CliInvocationReason::UnknownPreviewFeature)?;
                features.insert(feature);
                i += 2;
            }
            "-o" | "--output" => {
                args.get(i + 1)
                    .ok_or(CliInvocationReason::MissingOptionValue)?;
                i += 2;
            }
            arg if !arg.starts_with('-') => {
                if arg != only_source.path {
                    return Err(CliInvocationReason::SourceArgumentMismatch);
                }
                saw_source = true;
                i += 1;
            }
            _ => return Err(CliInvocationReason::UnsupportedFlag),
        }
    }

    if saw_source {
        Ok(features)
    } else {
        Err(CliInvocationReason::MissingSourceArgument)
    }
}

fn rel(path: &Path) -> String {
    path.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn load_cli_test_file(path: &Path) -> Result<TestFile, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let file: TestFile = toml::from_str(&text)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;

    let mut platform_failures = Vec::new();
    for case in &file.cases {
        let mut unknown: Vec<&str> = case
            .only_on
            .iter()
            .map(String::as_str)
            .filter(|platform| !rue_test_runner::KNOWN_TARGETS.contains(platform))
            .collect();
        unknown.sort_unstable();
        unknown.dedup();
        if !unknown.is_empty() {
            platform_failures.push(format!(
                "case {:?} has unknown only_on platform(s): {} (known: {})",
                case.name,
                unknown.join(", "),
                rue_test_runner::KNOWN_TARGETS.join(", ")
            ));
        }
    }
    platform_failures.sort();
    if platform_failures.is_empty() {
        Ok(file)
    } else {
        Err(format!(
            "could not validate {}:\n  {}",
            path.display(),
            platform_failures.join("\n  ")
        ))
    }
}

/// Discover TOML files under every requested root while retaining all failures.
/// Paths and diagnostics are sorted so reports do not depend on directory order.
fn discover_toml(dirs: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut files = Vec::new();
    let mut failures = Vec::new();
    for dir in dirs {
        match rue_test_runner::discover_files(dir, "toml") {
            Ok(discovered) if discovered.is_empty() => failures.push(format!(
                "no .toml case files found under requested root {}",
                dir.display()
            )),
            Ok(mut discovered) => files.append(&mut discovered),
            Err(error) => failures.push(format!(
                "could not discover case files under {}: {error}",
                dir.display()
            )),
        }
    }
    files.sort();
    files.dedup();
    failures.sort();
    (files, failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus_case(source: &str, compile_fail: bool) -> Case {
        Case {
            name: "classification probe".to_string(),
            files: vec![SourceFile {
                path: "probe.rue".to_string(),
                source: source.to_string(),
            }],
            source_path: None,
            args: None,
            stdin: None,
            env: HashMap::new(),
            compile_fail,
            compile_only: false,
            watch: None,
            stdout: None,
            stdout_contains: Vec::new(),
            runtime_error_contains: Vec::new(),
            exit_code: Some(0),
            known_bug: None,
            known_bug_on: Vec::new(),
            only_on: Vec::new(),
            skip: false,
        }
    }

    fn assert_cli_ineligible(case: &Case, expected: IneligibleReason) {
        assert_eq!(
            check_case(Path::new("classification.toml"), case),
            CaseOutcome::Ineligible(expected)
        );
    }

    fn assert_spec_ineligible(case: &rue_test_runner::Case, expected: IneligibleReason) {
        assert_eq!(
            check_spec_case("test:1", case),
            CaseOutcome::Ineligible(expected)
        );
    }

    fn other_known_target() -> String {
        rue_test_runner::KNOWN_TARGETS
            .iter()
            .copied()
            .find(|target| *target != rue_test_runner::get_host_target())
            .expect("the test runner supports more than one target")
            .to_string()
    }

    fn write_compile_fail_case(path: &Path, name: &str) {
        std::fs::write(
            path,
            format!(
                r#"
[section]
id = "cli.fixture"

[[case]]
name = "{name}"
compile_fail = true
files = [{{ path = "probe.rue", source = "not Rue" }}]
"#
            ),
        )
        .expect("write CLI case fixture");
    }

    #[test]
    fn discovery_keeps_valid_roots_and_reports_missing_roots_in_sorted_order() {
        let temp = tempfile::tempdir().expect("create temporary cases root");
        let valid = temp.path().join("valid");
        let nested = valid.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested cases root");
        write_compile_fail_case(&valid.join("z.toml"), "z case");
        write_compile_fail_case(&nested.join("a.toml"), "a case");
        let z_missing = temp.path().join("z-missing");
        let a_missing = temp.path().join("a-missing");

        // Deliberately repeat and overlap roots: each TOML case must still be
        // loaded once, while diagnostics remain sorted independently of input
        // order.
        let roots = vec![
            valid.clone(),
            nested,
            valid.clone(),
            z_missing.clone(),
            a_missing.clone(),
        ];
        let (files, failures) = discover_toml(&roots);

        assert_eq!(files.len(), 2);
        assert!(files.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(failures.len(), 2);
        assert!(failures[0].contains(&a_missing.display().to_string()));
        assert!(failures[1].contains(&z_missing.display().to_string()));

        assert_eq!(
            corpus_mode(
                roots
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect()
            ),
            ExitCode::FAILURE,
            "a valid root must not hide another requested root's discovery failure"
        );
    }

    #[test]
    fn malformed_cli_toml_is_a_fatal_harness_failure() {
        let temp = tempfile::tempdir().expect("create temporary cases root");
        write_compile_fail_case(&temp.path().join("valid.toml"), "valid case");
        let malformed = temp.path().join("malformed.toml");
        std::fs::write(&malformed, "[[case]\nname = 1").expect("write malformed CLI case fixture");

        let failure = match load_cli_test_file(&malformed) {
            Err(failure) => failure,
            Ok(_) => panic!("malformed TOML unexpectedly loaded"),
        };
        assert!(failure.contains("could not parse"));
        assert_eq!(
            corpus_mode(vec![temp.path().to_string_lossy().into_owned()]),
            ExitCode::FAILURE,
            "a valid case must not hide a malformed collected case file"
        );
    }

    #[test]
    fn unknown_only_on_is_a_fatal_load_error() {
        let temp = tempfile::tempdir().expect("create temporary cases root");
        let invalid = temp.path().join("invalid-platform.toml");
        std::fs::write(
            &invalid,
            r#"
[section]
id = "cli.invalid_platform"

[[case]]
name = "misspelled platform"
only_on = ["x86_64-linux"]
files = [{ path = "probe.rue", source = "fn main() -> i32 { 0 }" }]
"#,
        )
        .expect("write invalid-platform fixture");

        let failure = match load_cli_test_file(&invalid) {
            Err(failure) => failure,
            Ok(_) => panic!("unknown only_on platform unexpectedly loaded"),
        };
        assert!(failure.contains(&invalid.display().to_string()));
        assert!(failure.contains("misspelled platform"));
        assert!(failure.contains("x86_64-linux"));
        for target in rue_test_runner::KNOWN_TARGETS {
            assert!(failure.contains(target));
        }
        assert_eq!(
            corpus_mode(vec![temp.path().to_string_lossy().into_owned()]),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn cli_section_id_is_required_for_stable_registry_identity() {
        let temp = tempfile::tempdir().expect("create temporary cases root");
        let missing_section = temp.path().join("missing-section.toml");
        std::fs::write(
            &missing_section,
            r#"
[[case]]
name = "identity-less"
compile_fail = true
files = [{ path = "probe.rue", source = "not Rue" }]
"#,
        )
        .expect("write missing-section fixture");

        let failure = load_cli_test_file(&missing_section)
            .err()
            .expect("a CLI corpus file without section.id must not load");
        assert!(failure.contains("could not parse"));
        assert!(failure.contains("missing field `section`"));
    }

    #[test]
    fn unreadable_or_empty_cli_inputs_fail_through_the_report() {
        let temp = tempfile::tempdir().expect("create temporary cases root");
        let missing_file = temp.path().join("vanished.toml");
        let failure = match load_cli_test_file(&missing_file) {
            Err(failure) => failure,
            Ok(_) => panic!("missing case file unexpectedly loaded"),
        };
        assert!(failure.contains("could not read"));

        assert_eq!(
            corpus_mode(vec![temp.path().to_string_lossy().into_owned()]),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn eligible_compile_rejection_is_a_frontend_failure() {
        let case = corpus_case("fn main() -> i32 { missing_name }", false);
        let mut report = Report::default();

        report.record(check_case(Path::new("classification.toml"), &case));

        assert_eq!(report.frontend_failures.len(), 1);
        assert!(report.frontend_failures[0].contains("classification.toml"));
        assert_eq!(report.unmodeled_count(), 0);
        assert_eq!(finish_report(&report, "test"), ExitCode::FAILURE);
    }

    #[test]
    fn only_on_matches_cli_runner_host_semantics() {
        let mut case = corpus_case("fn main() -> i32 { 0 }", false);
        case.only_on = vec![rue_test_runner::get_host_target().to_string()];
        assert!(matches!(
            check_case(Path::new("platform.toml"), &case),
            CaseOutcome::Agree
        ));

        case.only_on = vec![other_known_target()];
        assert_eq!(
            check_case(Path::new("platform.toml"), &case),
            CaseOutcome::Ineligible(IneligibleReason::HostFiltered)
        );
    }

    #[test]
    fn known_bug_on_scopes_the_expected_failure() {
        let mut case = corpus_case("fn main() -> i32 { 0 }", false);
        case.known_bug = Some("RUE-test".to_string());
        assert_eq!(
            check_case(Path::new("known-bug.toml"), &case),
            CaseOutcome::Ineligible(IneligibleReason::ApplicableKnownBug)
        );

        case.known_bug_on = vec![rue_test_runner::get_host_target().to_string()];
        assert_eq!(
            check_case(Path::new("known-bug.toml"), &case),
            CaseOutcome::Ineligible(IneligibleReason::ApplicableKnownBug)
        );

        case.known_bug_on = vec![other_known_target()];
        assert!(matches!(
            check_case(Path::new("known-bug.toml"), &case),
            CaseOutcome::Agree
        ));

        // Keep parity with rue-cli-tests: only `only_on` is validated. An
        // unknown known_bug_on value does not match this host, so the case
        // runs normally and can fail loudly instead of being silently xfailed.
        case.known_bug_on = vec!["not-a-real-target".to_string()];
        assert!(matches!(
            check_case(Path::new("known-bug.toml"), &case),
            CaseOutcome::Agree
        ));
    }

    #[test]
    fn source_path_is_explicitly_ineligible() {
        let mut case = corpus_case("fn main() -> i32 { 0 }", false);
        // Keep the inline source too: this proves the oracle does not silently
        // substitute it for the external source selected by the real runner.
        case.source_path = Some("examples/welcome.rue".to_string());
        assert_eq!(
            check_case(Path::new("source-path.toml"), &case),
            CaseOutcome::Ineligible(IneligibleReason::ExternalSourcePath)
        );
    }

    #[test]
    fn cli_invocation_classifier_rejects_removed_slice_preview() {
        let mut case = corpus_case("fn main() -> i32 { 0 }", false);
        assert_eq!(
            corpus_preview_features(&case),
            Ok(empty_preview_features()),
            "absent args use the real runner's synthesized default invocation"
        );

        case.args = Some(
            [
                "-o",
                "-unusual-output-name",
                "probe.rue",
                "--preview",
                "slices",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        );
        assert_eq!(
            corpus_preview_features(&case),
            Err(CliInvocationReason::UnknownPreviewFeature)
        );
    }

    #[test]
    fn cli_invocation_classifier_reports_the_first_exact_exclusion() {
        let mut case = corpus_case("fn main() -> i32 { 0 }", false);
        let probes = [
            (vec![], CliInvocationReason::MissingSourceArgument),
            (
                vec!["wrong.rue"],
                CliInvocationReason::SourceArgumentMismatch,
            ),
            (
                vec!["probe.rue", "--preview"],
                CliInvocationReason::MissingOptionValue,
            ),
            (
                vec!["probe.rue", "-o"],
                CliInvocationReason::MissingOptionValue,
            ),
            (
                vec!["--preview", "future_feature", "probe.rue"],
                CliInvocationReason::UnknownPreviewFeature,
            ),
            (
                vec!["-O2", "probe.rue"],
                CliInvocationReason::UnsupportedFlag,
            ),
            (
                vec!["-O2", "wrong.rue"],
                CliInvocationReason::UnsupportedFlag,
            ),
            (
                vec!["wrong.rue", "-O2"],
                CliInvocationReason::SourceArgumentMismatch,
            ),
            (
                vec!["--preview", "future_feature", "-O2"],
                CliInvocationReason::UnknownPreviewFeature,
            ),
        ];

        for (args, expected) in probes {
            case.args = Some(args.iter().map(|arg| (*arg).to_string()).collect());
            assert_eq!(corpus_preview_features(&case), Err(expected), "{args:?}");
        }

        case.files.clear();
        case.args = Some(Vec::new());
        assert_eq!(
            corpus_preview_features(&case),
            Err(CliInvocationReason::MissingSourceArgument)
        );
    }

    #[test]
    fn typed_cli_invocation_exclusions_remain_ineligible() {
        let mut case = corpus_case("fn main() -> i32 { 0 }", false);
        case.args = Some(vec!["-O2".to_string(), "probe.rue".to_string()]);
        assert_eq!(
            check_case(Path::new("optimization.toml"), &case),
            CaseOutcome::Ineligible(IneligibleReason::CliInvocation(
                CliInvocationReason::UnsupportedFlag
            ))
        );
    }

    #[test]
    fn cli_ineligibility_reasons_have_runner_ordered_precedence() {
        let mut case = corpus_case("fn main() -> i32 { 0 }", false);
        case.skip = true;
        case.only_on = vec![other_known_target()];
        case.compile_fail = true;
        case.compile_only = true;
        case.known_bug = Some("RUE-test".to_string());
        case.stdin = Some(String::new());
        case.env
            .insert("RUE_STD_PATH".to_string(), "std".to_string());
        case.source_path = Some("external.rue".to_string());
        case.args = Some(vec!["-O2".to_string(), "probe.rue".to_string()]);

        assert_cli_ineligible(&case, IneligibleReason::ExplicitSkip);
        case.skip = false;
        assert_cli_ineligible(&case, IneligibleReason::HostFiltered);
        case.only_on.clear();
        assert_cli_ineligible(&case, IneligibleReason::ExpectedCompileFailure);
        case.compile_fail = false;
        assert_cli_ineligible(&case, IneligibleReason::CompileOnly);
        case.compile_only = false;
        assert_cli_ineligible(&case, IneligibleReason::ApplicableKnownBug);
        case.known_bug = None;
        case.watch = Some(serde::de::IgnoredAny);
        assert_cli_ineligible(&case, IneligibleReason::WatchOrchestration);
        case.watch = None;
        assert_cli_ineligible(&case, IneligibleReason::StandardInput);
        case.stdin = None;
        assert_cli_ineligible(&case, IneligibleReason::CompilerEnvironment);
        case.env.clear();
        case.files.clear();
        assert_cli_ineligible(&case, IneligibleReason::ExternalSourcePath);
        case.source_path = None;
        assert_cli_ineligible(&case, IneligibleReason::NoInlineSource);
        case.files = vec![
            SourceFile {
                path: "probe.rue".to_string(),
                source: "fn main() -> i32 { 0 }".to_string(),
            },
            SourceFile {
                path: "other.rue".to_string(),
                source: "fn other() -> i32 { 0 }".to_string(),
            },
        ];
        assert_cli_ineligible(&case, IneligibleReason::MultipleSourceFiles);
        case.files.pop();
        assert_cli_ineligible(
            &case,
            IneligibleReason::CliInvocation(CliInvocationReason::UnsupportedFlag),
        );
    }

    #[test]
    fn spec_ineligibility_reasons_have_runner_ordered_precedence() {
        let mut case = rue_test_runner::Case {
            name: "spec precedence".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            skip: true,
            only_on: vec![other_known_target()],
            preview: Some("test_infra".to_string()),
            compile_fail: true,
            compile_only: true,
            stdin: Some(String::new()),
            expected_ast: Some("golden".to_string()),
            target: Some(rue_test_runner::get_host_target().to_string()),
            ..Default::default()
        };
        case.aux_files
            .insert("aux.rue".to_string(), "fn aux() {}".to_string());

        assert_spec_ineligible(&case, IneligibleReason::ExplicitSkip);
        case.skip = false;
        assert_spec_ineligible(&case, IneligibleReason::HostFiltered);
        case.only_on.clear();
        assert_spec_ineligible(&case, IneligibleReason::PreviewExpectedFailure);
        case.preview_should_pass = true;
        assert_spec_ineligible(&case, IneligibleReason::ExpectedCompileFailure);
        case.compile_fail = false;
        assert_spec_ineligible(&case, IneligibleReason::CompileOnly);
        case.compile_only = false;
        assert_spec_ineligible(&case, IneligibleReason::StandardInput);
        case.stdin = None;
        assert_spec_ineligible(&case, IneligibleReason::AuxiliaryFiles);
        case.aux_files.clear();
        assert_spec_ineligible(&case, IneligibleReason::GoldenIrOnly);
        case.expected_ast = None;
        assert_spec_ineligible(&case, IneligibleReason::TargetPinned);
        case.target = None;
        assert_spec_ineligible(&case, IneligibleReason::MissingExecutionExpectation);
    }

    #[test]
    fn known_oracle_gap_reason_is_live_and_stale_checked() {
        let mut case = rue_test_runner::Case {
            name: "known gap probe".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            exit_code: Some(1),
            ..Default::default()
        };

        assert_eq!(
            check_spec_case_with_known_gap("test:1", &case, true),
            CaseOutcome::Ineligible(IneligibleReason::KnownOracleGap)
        );
        assert!(matches!(
            check_spec_case_with_known_gap("test:1", &case, false),
            CaseOutcome::Disagreement(_)
        ));

        case.exit_code = Some(0);
        let CaseOutcome::Disagreement(message) =
            check_spec_case_with_known_gap("test:1", &case, true)
        else {
            panic!("a stale known-gap entry did not fail");
        };
        assert!(message.contains("KNOWN_ORACLE_GAPS entry is stale"));

        case.source =
            "fn probe() -> u32 { let value: u32 = @random_u32(); value } fn main() { probe(); }"
                .to_string();
        assert_eq!(
            check_spec_case_with_known_gap("test:1", &case, true),
            CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(ModelGapKind::ExternalDependency(
                rue_oracle::ExternalDependencyKind::RandomU32
            ))),
            "a known-gap entry must not absorb a genuine oracle coverage gap"
        );

        case.source = "fn main() -> i32 { missing_name }".to_string();
        assert!(matches!(
            check_spec_case_with_known_gap("test:1", &case, true),
            CaseOutcome::FrontendFailure(_)
        ));
    }

    #[test]
    fn stdout_contains_is_enforced_alongside_exact_stdout() {
        let mut case = corpus_case("fn main() -> i32 { @dbg(42); 0 }", false);
        case.stdout_contains = vec!["42".to_string()];
        assert!(matches!(
            check_case(Path::new("stdout.toml"), &case),
            CaseOutcome::Agree
        ));

        case.stdout = Some("42\n".to_string());
        case.stdout_contains = vec!["42".to_string(), "\n".to_string()];
        assert!(matches!(
            check_case(Path::new("stdout.toml"), &case),
            CaseOutcome::Agree
        ));

        case.stdout_contains.push("missing".to_string());
        let CaseOutcome::Disagreement(message) = check_case(Path::new("stdout.toml"), &case) else {
            panic!("missing stdout substring did not produce a disagreement");
        };
        assert!(message.contains("missing required substring \"missing\""));
        assert!(message.contains("42\\n"));
    }

    #[test]
    fn cli_runtime_trap_expectations_distinguish_same_exit_code_causes() {
        let mut case = corpus_case("fn main() -> i32 { let z = 0; 10 / z }", false);
        case.exit_code = None;
        case.runtime_error_contains = vec!["division by zero".to_string()];
        assert!(matches!(
            check_case(Path::new("trap.toml"), &case),
            CaseOutcome::Agree
        ));

        case.runtime_error_contains = vec!["integer overflow".to_string()];
        let CaseOutcome::Disagreement(message) = check_case(Path::new("trap.toml"), &case) else {
            panic!("wrong trap category did not produce a disagreement");
        };
        assert!(message.contains("ArithmeticOverflow"));
        assert!(message.contains("DivisionByZero"));

        case = corpus_case("fn main() -> i32 { 101 }", false);
        case.exit_code = None;
        case.runtime_error_contains = vec!["division by zero".to_string()];
        let CaseOutcome::Disagreement(message) = check_case(Path::new("trap.toml"), &case) else {
            panic!("normal return 101 was accepted as an expected trap");
        };
        assert!(message.contains("oracle got None"));
    }

    #[test]
    fn cli_abort_intrinsics_check_exact_stderr_observations() {
        for (source, expected, expected_kind) in [
            (
                "fn main() -> i32 { @panic(); 0 }",
                "panic",
                TrapKind::UserPanic,
            ),
            (
                r#"fn main() -> i32 { @panic("boom"); 0 }"#,
                "panic: boom",
                TrapKind::UserPanic,
            ),
            (
                "fn main() -> i32 { @assert(false); 0 }",
                "assertion failed",
                TrapKind::AssertionFailure,
            ),
            (
                r#"fn main() -> i32 { @assert(false, "boom"); 0 }"#,
                "panic: boom",
                TrapKind::UserPanic,
            ),
        ] {
            let mut case = corpus_case(source, false);
            case.exit_code = Some(101);
            case.runtime_error_contains = vec![expected.to_string()];
            assert_eq!(
                check_case(Path::new("panic.toml"), &case),
                CaseOutcome::Agree,
                "{expected_kind:?} with {expected:?}"
            );
        }

        let mut wrong_message = corpus_case(r#"fn main() -> i32 { @panic("actual"); 0 }"#, false);
        wrong_message.exit_code = Some(101);
        // Both strings map to UserPanic. Exact stderr comparison must still
        // reject the wrong user-visible message.
        wrong_message.runtime_error_contains = vec!["panic: expected".to_string()];
        let CaseOutcome::Disagreement(message) =
            check_case(Path::new("panic.toml"), &wrong_message)
        else {
            panic!("same-category panic message mismatch was accepted");
        };
        assert!(message.contains("missing required substring"));
        assert!(message.contains("panic: actual"));
    }

    #[test]
    fn spec_runtime_trap_expectations_use_the_same_typed_comparison() {
        let mut case = rue_test_runner::Case {
            name: "spec trap probe".to_string(),
            source: "fn main() -> i32 { let z = 0; 10 % z }".to_string(),
            runtime_error: Some("division by zero".to_string()),
            ..Default::default()
        };
        assert!(matches!(
            check_spec_case("test:1", &case),
            CaseOutcome::Agree
        ));

        case.runtime_error = Some("integer overflow".to_string());
        let CaseOutcome::Disagreement(message) = check_spec_case("test:1", &case) else {
            panic!("wrong spec trap category did not produce a disagreement");
        };
        assert!(message.contains("ArithmeticOverflow"));
        assert!(message.contains("DivisionByZero"));

        case.source = "fn main() -> i32 { 101 }".to_string();
        case.runtime_error = Some("division by zero".to_string());
        let CaseOutcome::Disagreement(message) = check_spec_case("test:1", &case) else {
            panic!("normal return 101 was accepted as a spec trap");
        };
        assert!(message.contains("oracle got None"));

        case.source = "fn main() -> i32 { let x: i32 = 2147483647; x + 1 }".to_string();
        case.runtime_error = Some("panic: integer overflow".to_string());
        assert!(matches!(
            check_spec_case("test:1", &case),
            CaseOutcome::Disagreement(_)
        ));

        case.expected_stdout = Some("required output\n".to_string());
        assert!(matches!(
            check_spec_case("test:1", &case),
            CaseOutcome::Disagreement(_)
        ));

        case.source = "fn main() -> i32 { 0 }".to_string();
        case.expected_stdout = None;
        assert!(matches!(
            check_spec_case("test:1", &case),
            CaseOutcome::Disagreement(_)
        ));
    }

    #[test]
    fn spec_abort_stderr_has_a_narrow_separate_trap_vocabulary() {
        let mut case = rue_test_runner::Case {
            name: "spec panic probe".to_string(),
            source: r#"fn main() -> i32 { @panic("boom"); 0 }"#.to_string(),
            exit_code: Some(101),
            stderr_contains: Some("panic: boom".to_string()),
            ..Default::default()
        };
        assert_eq!(check_spec_case("test:1", &case), CaseOutcome::Agree);

        case.source = "fn main() -> i32 { @assert(false); 0 }".to_string();
        case.stderr_contains = Some("assertion failed".to_string());
        assert_eq!(check_spec_case("test:1", &case), CaseOutcome::Agree);

        case.source = r#"fn main() -> i32 { @assert(false, "actual"); 0 }"#.to_string();
        case.stderr_contains = Some("panic: expected".to_string());
        assert!(matches!(
            check_spec_case("test:1", &case),
            CaseOutcome::Disagreement(_)
        ));

        case.source = "fn main() -> i32 { let x: i32 = 2147483647; x + 1 }".to_string();
        case.stderr_contains = Some("error: integer overflow".to_string());
        let CaseOutcome::Disagreement(message) = check_spec_case("test:1", &case) else {
            panic!("generic stderr text declared an unrelated arithmetic trap");
        };
        assert!(message.contains("trap contract"));
    }

    #[test]
    fn spec_stderr_expectations_are_enforced_as_modeled_observations() {
        let mut case = rue_test_runner::Case {
            name: "stderr coverage probe".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            exit_code: Some(0),
            stderr_contains: Some("required notice".to_string()),
            ..Default::default()
        };
        let CaseOutcome::Disagreement(message) = check_spec_case("test:1", &case) else {
            panic!("missing modeled stderr did not disagree");
        };
        assert!(message.contains("missing required substring"));
        assert!(message.contains("required notice"));

        case.exit_code = Some(1);
        assert!(matches!(
            check_spec_case("test:1", &case),
            CaseOutcome::Disagreement(_)
        ));

        case.exit_code = Some(0);
        case.expected_stdout = Some("required output\n".to_string());
        assert!(matches!(
            check_spec_case("test:1", &case),
            CaseOutcome::Disagreement(_)
        ));

        case.expected_stdout = None;
        case.source = "fn main() -> i32 { missing_name }".to_string();
        assert!(matches!(
            check_spec_case("test:1", &case),
            CaseOutcome::FrontendFailure(_)
        ));
    }

    #[test]
    fn trap_and_known_gap_contracts_precede_generic_stderr_coverage() {
        let mut case = rue_test_runner::Case {
            name: "stderr ordering probe".to_string(),
            source: "fn main() -> i32 { let x: i32 = 2147483647; x + 1 }".to_string(),
            exit_code: Some(101),
            stderr_contains: Some("arbitrary runtime stderr".to_string()),
            ..Default::default()
        };
        let CaseOutcome::Disagreement(message) = check_spec_case("test:1", &case) else {
            panic!("generic stderr coverage hid an undeclared typed trap");
        };
        assert!(message.contains("trap contract"));

        case.runtime_error = Some("division by zero".to_string());
        let CaseOutcome::Disagreement(message) = check_spec_case("test:1", &case) else {
            panic!("generic stderr coverage hid a typed trap mismatch");
        };
        assert!(message.contains("ArithmeticOverflow"));

        case.runtime_error = Some("integer overflow".to_string());
        assert_eq!(check_spec_case("test:1", &case), CaseOutcome::Agree);

        case.source = "fn main() -> i32 { 0 }".to_string();
        case.runtime_error = None;
        case.exit_code = Some(1);
        assert_eq!(
            check_spec_case_with_known_gap("test:1", &case, true),
            CaseOutcome::Ineligible(IneligibleReason::KnownOracleGap)
        );

        case.exit_code = Some(0);
        assert_eq!(
            check_spec_case_with_known_gap("test:1", &case, true),
            CaseOutcome::Ineligible(IneligibleReason::KnownOracleGap),
            "the remaining stderr mismatch keeps the tracked wrong-result entry live"
        );
        assert!(matches!(
            check_spec_case_with_known_gap("test:1", &case, false),
            CaseOutcome::Disagreement(_)
        ));
    }

    #[test]
    fn undeclared_cli_trap_at_the_runtime_exit_satisfies_the_exit_only_contract() {
        // A case whose only observable contract is `exit_code = 101` (no
        // `runtime_error_contains` trap declaration) is asserting termination by
        // trap. An oracle trap whose exit matches therefore satisfies that
        // contract — it is agreement, not an undeclared-trap divergence. This is
        // what lets the raw-pointer heap model cover corpus cases (slice bounds
        // traps, allocation-size-overflow traps) that pin only exit 101; the
        // compiled binary and the oracle were verified by hand to emit the same
        // trap. A case that also declares a trap cause keeps the strict contract
        // (see `spec_abort_stderr_has_a_narrow_separate_trap_vocabulary`).
        let mut case = corpus_case("fn main() -> i32 { let x: i32 = 2147483647; x + 1 }", false);
        case.exit_code = Some(101);
        assert_eq!(
            check_case(Path::new("undeclared-trap.toml"), &case),
            CaseOutcome::Agree
        );

        // A matching oracle trap at a *non*-101 pinned exit is still a
        // divergence: the exit contract is violated.
        let mut mismatched =
            corpus_case("fn main() -> i32 { let x: i32 = 2147483647; x + 1 }", false);
        mismatched.exit_code = Some(7);
        let CaseOutcome::Disagreement(message) =
            check_case(Path::new("undeclared-trap.toml"), &mismatched)
        else {
            panic!("an oracle trap at a non-101 pinned exit must diverge");
        };
        assert!(message.contains("trap contract"));
    }

    #[test]
    fn undeclared_spec_trap_at_the_runtime_exit_satisfies_the_exit_only_contract() {
        // Spec mirror of the CLI contract above.
        let case = rue_test_runner::Case {
            name: "undeclared spec trap".to_string(),
            source: "fn main() -> i32 { let x: i32 = 2147483647; x + 1 }".to_string(),
            exit_code: Some(101),
            ..Default::default()
        };
        assert_eq!(check_spec_case("test:1", &case), CaseOutcome::Agree);

        // Declaring any stderr cause keeps the strict undeclared-trap contract,
        // so a mismatched declaration still diverges.
        let declared = rue_test_runner::Case {
            name: "declared spec trap".to_string(),
            source: "fn main() -> i32 { let x: i32 = 2147483647; x + 1 }".to_string(),
            exit_code: Some(101),
            stderr_contains: Some("some unrelated stderr".to_string()),
            ..Default::default()
        };
        let CaseOutcome::Disagreement(message) = check_spec_case("test:1", &declared) else {
            panic!("a declared-but-unmodeled spec stderr must still diverge on the trap");
        };
        assert!(message.contains("trap contract"));
    }

    #[test]
    fn only_modeled_traps_require_a_runtime_cause_declaration() {
        let mut normal_cli = corpus_case("fn main() -> i32 { 101 }", false);
        normal_cli.exit_code = Some(101);
        assert!(matches!(
            check_case(Path::new("normal-101.toml"), &normal_cli),
            CaseOutcome::Agree
        ));

        let normal_spec = rue_test_runner::Case {
            name: "normal 101".to_string(),
            source: "fn main() -> i32 { 101 }".to_string(),
            exit_code: Some(101),
            ..Default::default()
        };
        assert!(matches!(
            check_spec_case("test:1", &normal_spec),
            CaseOutcome::Agree
        ));

        let unsupported_source =
            "fn probe() -> u32 { let value: u32 = @random_u32(); value } fn main() { probe(); }";
        let unsupported_cli = corpus_case(unsupported_source, false);
        assert!(matches!(
            check_case(Path::new("unsupported.toml"), &unsupported_cli),
            CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(ModelGapKind::ExternalDependency(
                rue_oracle::ExternalDependencyKind::RandomU32
            )))
        ));

        let unsupported_spec = rue_test_runner::Case {
            name: "unsupported semantics".to_string(),
            source: unsupported_source.to_string(),
            exit_code: Some(0),
            ..Default::default()
        };
        assert!(matches!(
            check_spec_case("test:1", &unsupported_spec),
            CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(ModelGapKind::ExternalDependency(
                rue_oracle::ExternalDependencyKind::RandomU32
            )))
        ));
    }

    #[test]
    fn unknown_runtime_expectations_are_counted_as_unmodeled() {
        let mut case = corpus_case("fn main() -> i32 { let x: i32 = 2147483647; x + 1 }", false);
        case.exit_code = None;
        // This fragment pins the exact emitted stderr but is intentionally not
        // part of the typed trap vocabulary, so category coverage alone remains
        // explicit.
        case.runtime_error_contains = vec!["error: integer overflow".to_string()];

        let mut report = Report::default();
        report.record(check_case(Path::new("trap.toml"), &case));

        assert_eq!(report.unmodeled_count(), 1);
        assert!(report.disagreements.is_empty());
        assert_eq!(report.modeled_eligible_count(), 0);

        case.runtime_error_contains = vec!["custom trap wording".to_string()];
        assert!(matches!(
            check_case(Path::new("trap.toml"), &case),
            CaseOutcome::Disagreement(_)
        ));
        case.runtime_error_contains = vec!["error: integer overflow".to_string()];

        // The same ordering applies to substring output contracts.
        case.stdout_contains = vec!["required output".to_string()];
        assert!(matches!(
            check_case(Path::new("trap.toml"), &case),
            CaseOutcome::Disagreement(_)
        ));

        // The unknown category does not hide a separately provable exit
        // disagreement.
        case.files[0].source = "fn main() -> i32 { 0 }".to_string();
        case.stdout_contains.clear();
        assert!(matches!(
            check_case(Path::new("trap.toml"), &case),
            CaseOutcome::Disagreement(_)
        ));
    }

    #[test]
    fn report_records_each_outcome_and_formats_reasons_deterministically() {
        let mut report = Report::default();
        report.record(CaseOutcome::Agree);
        report.record(CaseOutcome::Unmodeled(UnmodeledReason::TrapExpectation));
        report.record(CaseOutcome::Ineligible(IneligibleReason::CliInvocation(
            CliInvocationReason::UnsupportedFlag,
        )));
        report.record(CaseOutcome::Ineligible(
            IneligibleReason::ExpectedCompileFailure,
        ));
        report.record(CaseOutcome::Ineligible(IneligibleReason::HostFiltered));
        report.record(CaseOutcome::Ineligible(
            IneligibleReason::ExpectedCompileFailure,
        ));
        report.record(CaseOutcome::FrontendFailure("frontend".to_string()));
        report.record(CaseOutcome::OracleFailure("oracle".to_string()));
        report.record(CaseOutcome::Disagreement("disagreement".to_string()));

        assert_eq!(report.agree, 1);
        assert_eq!(report.unmodeled_count(), 1);
        assert_eq!(
            report.unmodeled_breakdown_lines(),
            vec!["    runtime trap expectation: 1"]
        );
        assert_eq!(report.ineligible_count(), 4);
        assert_eq!(report.classified_case_count(), 9);
        assert_eq!(
            report.ineligible_breakdown_lines(),
            vec![
                "    host platform filter: 1",
                "    expected compile failure: 2",
                "    CLI invocation (unsupported flag): 1",
            ]
        );
        assert_eq!(report.frontend_failures, vec!["frontend".to_string()]);
        assert_eq!(report.oracle_failures, vec!["oracle".to_string()]);
        assert_eq!(report.disagreements, vec!["disagreement".to_string()]);
    }

    #[test]
    fn exact_model_gap_kind_survives_case_classification() {
        let error = rue_oracle::run_source(
            "fn probe() -> u32 { let value: u32 = @random_u32(); value } fn main() { probe(); }",
        )
        .expect_err("randomness must remain outside the deterministic oracle");

        assert_eq!(
            record_oracle_error("typed gap probe", error),
            CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(ModelGapKind::ExternalDependency(
                rue_oracle::ExternalDependencyKind::RandomU32
            )))
        );
    }

    #[test]
    fn modeled_stderr_mismatches_are_not_unmodeled_coverage() {
        let mut trap_case =
            corpus_case("fn main() -> i32 { let x: i32 = 2147483647; x + 1 }", false);
        trap_case.exit_code = None;
        trap_case.runtime_error_contains = vec!["custom trap wording".to_string()];
        let trap_outcome = check_case(Path::new("trap.toml"), &trap_case);

        let stderr_case = rue_test_runner::Case {
            name: "stderr coverage probe".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            exit_code: Some(0),
            stderr_contains: Some("required notice".to_string()),
            ..Default::default()
        };
        let stderr_outcome = check_spec_case("test:1", &stderr_case);

        assert!(matches!(trap_outcome, CaseOutcome::Disagreement(_)));
        assert!(matches!(stderr_outcome, CaseOutcome::Disagreement(_)));

        let mut report = Report::default();
        report.record(trap_outcome);
        report.record(stderr_outcome);
        assert_eq!(report.unmodeled_count(), 0);
        assert_eq!(report.disagreements.len(), 2);
    }

    #[test]
    fn non_oracle_unmodeled_reasons_are_unregistrable() {
        let case = corpus_case("fn main() -> i32 { 0 }", false);
        assert_eq!(
            model_gap_observation(
                &case,
                &CaseOutcome::Unmodeled(UnmodeledReason::TrapExpectation)
            ),
            model_gaps::Observation::Unregistrable("runtime trap expectation")
        );
    }

    #[test]
    fn active_unknown_trap_contract_cannot_hide_behind_a_typed_model_gap() {
        let mut case = corpus_case(
            "fn probe() -> u32 { let value: u32 = @random_u32(); value } fn main() { probe(); }",
            false,
        );
        case.runtime_error_contains = vec!["custom trap wording".to_string()];
        let outcome = check_case(Path::new("trap-plus-gap.toml"), &case);
        assert!(matches!(
            outcome,
            CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(_))
        ));
        assert_eq!(
            model_gap_observation(&case, &outcome),
            model_gaps::Observation::Unregistrable("runtime trap expectation")
        );

        // The same declaration on a case that is inactive on this host makes
        // no runtime claim here and must remain a scope-checked inactive case.
        case.only_on = vec![other_known_target()];
        let outcome = check_case(Path::new("inactive-trap-plus-gap.toml"), &case);
        assert_eq!(
            model_gap_observation(&case, &outcome),
            model_gaps::Observation::InactiveHost
        );
    }

    #[test]
    fn spec_registry_preserves_typed_debt_without_hiding_trap_contracts() {
        let gap = ModelGapKind::ExternalDependency(rue_oracle::ExternalDependencyKind::RandomU32);
        let clean = rue_test_runner::Case::default();
        assert_eq!(
            spec_model_gap_observation(
                &clean,
                &CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(gap)),
            ),
            model_gaps::Observation::ModelGap(gap)
        );

        let unknown_trap = rue_test_runner::Case {
            runtime_error: Some("custom trap wording".to_string()),
            ..Default::default()
        };
        assert_eq!(
            spec_model_gap_observation(
                &unknown_trap,
                &CaseOutcome::Unmodeled(UnmodeledReason::ModelGap(gap)),
            ),
            model_gaps::Observation::Unregistrable("runtime trap expectation")
        );
        assert_eq!(
            spec_model_gap_observation(
                &unknown_trap,
                &CaseOutcome::Ineligible(IneligibleReason::HostFiltered),
            ),
            model_gaps::Observation::InactiveHost
        );
    }

    #[test]
    fn exact_spec_stderr_remains_a_modeled_registry_observation() {
        let case = rue_test_runner::Case {
            name: "spec panic registry probe".to_string(),
            source: r#"fn main() -> i32 { @panic("boom"); 0 }"#.to_string(),
            exit_code: Some(101),
            stderr_contains: Some("panic: boom".to_string()),
            ..Default::default()
        };
        let outcome = check_spec_case("test:1", &case);
        assert_eq!(outcome, CaseOutcome::Agree);
        assert_eq!(
            spec_model_gap_observation(&case, &outcome),
            model_gaps::Observation::Agreement
        );
    }

    #[test]
    fn an_all_unmodeled_corpus_fails_closed() {
        let mut report = Report::default();
        let error = rue_oracle::run_source(
            "fn main() -> i32 { let n: u32 = @random_u32(); if n == 0 { 0 } else { 1 } }",
        )
        .expect_err("randomness must remain outside the deterministic oracle");

        report.record(record_oracle_error("unsupported probe", error));

        assert_eq!(report.unmodeled_count(), 1);
        assert_eq!(report.modeled_eligible_count(), 0);
        assert!(report.frontend_failures.is_empty());
        assert_eq!(finish_report(&report, "test"), ExitCode::FAILURE);
    }

    #[test]
    fn agreement_plus_unmodeled_coverage_is_successful() {
        let mut report = Report::default();
        report.record(CaseOutcome::Agree);
        report.record(CaseOutcome::Unmodeled(UnmodeledReason::TrapExpectation));

        assert_eq!(report.modeled_eligible_count(), 1);
        assert_eq!(finish_report(&report, "test"), ExitCode::SUCCESS);
    }

    #[test]
    fn resource_failure_cannot_hide_in_a_mixed_corpus() {
        let error = rue_oracle::run_source(
            "fn recurse(n: i32) -> i32 { recurse(n) }
            fn main() -> i32 { recurse(0) }",
        )
        .expect_err("unbounded recursion must hit the oracle depth limit");
        let outcome = record_oracle_error("resource probe", error);
        let CaseOutcome::OracleFailure(message) = &outcome else {
            panic!("resource exhaustion must be a hard oracle failure: {outcome:?}");
        };
        assert!(message.contains("ResourceLimit(RecursionDepth)"));

        let mut report = Report::default();
        report.record(CaseOutcome::Agree);
        report.record(outcome);

        assert_eq!(report.agree, 1);
        assert_eq!(report.unmodeled_count(), 0);
        assert_eq!(report.oracle_failures.len(), 1);
        assert_eq!(finish_report(&report, "test"), ExitCode::FAILURE);
    }

    #[test]
    fn an_all_ineligible_corpus_fails_closed() {
        let case = corpus_case("this is intentionally not Rue", true);
        let mut report = Report::default();

        report.record(check_case(Path::new("compile_fail.toml"), &case));

        assert_eq!(report.ineligible_count(), 1);
        assert_eq!(
            report.ineligible_breakdown_lines(),
            vec!["    expected compile failure: 1"]
        );
        assert_eq!(report.unmodeled_count(), 0);
        assert_eq!(report.modeled_eligible_count(), 0);
        assert!(report.frontend_failures.is_empty());
        assert_eq!(finish_report(&report, "test"), ExitCode::FAILURE);
    }

    #[test]
    fn noncanonical_environment_dependent_case_is_prefiltered() {
        let mut case = corpus_case("this would fail if the oracle compiled it", false);
        case.env
            .insert("RUE_STD_PATH".to_string(), "some/custom/std".to_string());
        let mut report = Report::default();

        report.record(check_case(Path::new("environment.toml"), &case));

        assert_eq!(report.ineligible_count(), 1);
        assert_eq!(
            report.ineligible_breakdown_lines(),
            vec!["    compiler environment: 1"]
        );
        assert_eq!(report.unmodeled_count(), 0);
        assert!(report.frontend_failures.is_empty());
    }
}
