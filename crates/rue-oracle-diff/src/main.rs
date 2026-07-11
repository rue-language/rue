//! # rue-oracle-diff — the differential harness (RUE-50)
//!
//! Runs the concrete [`rue-cli-tests`] corpus through the [`rue_oracle`]
//! reference interpreter and checks the oracle agrees with each case's expected
//! exit code / stdout. Those expectations are what the *compiled binary*
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
//!   (concrete `source` + expected exit/stdout; golden-IR-only, `compile_fail`,
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
//! Every mode exits non-zero if an eligible program is rejected by the front end
//! or if the oracle disagrees with the expected behavior. Generated fuzz mode
//! also exits non-zero if the oracle reports `Unsupported`, because its
//! generator promises to stay within the modeled subset.

mod fuzz;
mod generator;
mod trap;

use rue_error::{PreviewFeature, PreviewFeatures};
use rue_oracle::{RunSourceError, TrapKind, run_source_with_preview_features};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

#[derive(Deserialize)]
struct TestFile {
    #[serde(default, rename = "case")]
    cases: Vec<Case>,
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

enum CaseOutcome {
    Agree,
    /// The case has an executable shape, but the oracle does not model it yet.
    Unmodeled,
    /// The case does not describe a single concrete execution this harness can drive.
    Ineligible,
    /// An eligible case was rejected by the shared compiler front end.
    FrontendFailure(String),
    /// The oracle ran but disagreed with the expected behavior.
    Disagreement(String),
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
    unmodeled: u32,
    ineligible: u32,
    frontend_failures: Vec<String>,
    disagreements: Vec<String>,
    /// corpus discovery/read/parse failures — the harness did not check all inputs
    harness_failures: Vec<String>,
}

impl Report {
    /// Record exactly one terminal classification for one successfully loaded case.
    fn record(&mut self, outcome: CaseOutcome) {
        match outcome {
            CaseOutcome::Agree => self.agree += 1,
            CaseOutcome::Unmodeled => self.unmodeled += 1,
            CaseOutcome::Ineligible => self.ineligible += 1,
            CaseOutcome::FrontendFailure(failure) => self.frontend_failures.push(failure),
            CaseOutcome::Disagreement(disagreement) => self.disagreements.push(disagreement),
        }
    }

    fn modeled_eligible_count(&self) -> u32 {
        self.agree + self.frontend_failures.len() as u32 + self.disagreements.len() as u32
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
/// oracle and check it agrees with the case's expected exit code / stdout.
fn corpus_mode(raw_args: Vec<String>) -> ExitCode {
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
    let (toml_files, discovery_failures) = discover_toml(&dirs);
    report.harness_failures.extend(discovery_failures);
    if toml_files.is_empty() && report.harness_failures.is_empty() {
        report.harness_failures.push(format!(
            "no .toml case files found under {dirs:?} (run from the repo root)"
        ));
    }

    for path in &toml_files {
        let file = match load_cli_test_file(path) {
            Ok(file) => file,
            Err(failure) => {
                report.harness_failures.push(failure);
                continue;
            }
        };
        for case in &file.cases {
            report.record(check_case(path, case));
        }
    }
    report.harness_failures.sort();

    finish_report(&report, "rue-cli-tests")
}

/// Print the tallied report and turn it into a process exit code: success when
/// the oracle agreed with the compiler on every modeled eligible case, failure
/// on any harness/front-end failure or disagreement. Shared by [`corpus_mode`]
/// and [`spec_mode`]; `corpus` names which corpus was audited, for the header.
fn finish_report(report: &Report, corpus: &str) -> ExitCode {
    let total = report.agree
        + report.unmodeled
        + report.ineligible
        + report.frontend_failures.len() as u32
        + report.disagreements.len() as u32;
    println!("\n=== rue-oracle-diff: {corpus} corpus classification audit ({total} cases) ===");
    println!("  agree (modeled eligible): {}", report.agree);
    println!("  unmodeled eligible:       {}", report.unmodeled);
    println!("  ineligible:               {}", report.ineligible);
    println!("  HARNESS FAILURES: {}", report.harness_failures.len());
    for failure in &report.harness_failures {
        println!("\n  ✗ {failure}");
    }
    println!("  FRONTEND FAILURES: {}", report.frontend_failures.len());
    for failure in &report.frontend_failures {
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
        && report.disagreements.is_empty()
    {
        println!("\noracle agrees with the compiler on every modeled eligible case.");
        ExitCode::SUCCESS
    } else {
        println!(
            "\n{} harness failure(s), {} frontend failure(s), {} disagreement(s) — \
             corpus inputs must load and modeled eligible cases must compile before the \
             oracle can check them.",
            report.harness_failures.len(),
            report.frontend_failures.len(),
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
    let dirs: Vec<PathBuf> = if !raw_args.is_empty() {
        raw_args.into_iter().map(PathBuf::from).collect()
    } else if let Some(cases) = std::env::var_os("RUE_ORACLE_DIFF_SPEC_CASES") {
        // Set by the `//crates/rue-oracle-diff:oracle-diff-spec-test` sh_test to
        // the rue-spec `cases` filegroup's absolute location.
        vec![PathBuf::from(cases)]
    } else {
        vec![PathBuf::from("crates/rue-spec/cases")]
    };

    if !dirs.iter().any(|d| d.exists()) {
        eprintln!(
            "no rue-spec cases dir found under {:?} (run from the repo root)",
            dirs
        );
        return ExitCode::FAILURE;
    }

    let mut report = Report::default();
    for dir in &dirs {
        // `load_test_files` expands `params` templates and validates preview
        // feature names exactly as the spec runner does; it needs only the
        // cases dir (no spec markdown).
        for (ident, file) in rue_test_runner::load_test_files(dir) {
            for case in &file.case {
                report.record(check_spec_case(&ident, case));
            }
        }
    }

    finish_report(&report, "rue-spec")
}

/// Run one expanded rue-spec case through the oracle and return one outcome.
///
/// Ineligible (never a disagreement) when the case is not
/// a single concrete program the oracle can execute and compare: `compile_fail`
/// (no runtime output), `compile_only` (never executed), golden-IR-only (an IR
/// dump, not a run), or a shape with no oracle model (stdin, multi-file
/// `aux_files`). Preview-gated cases run with their declared preview feature.
/// Cases the oracle simply can't model yet return [`RunSourceError::Unsupported`]
/// and count as unmodeled. [`RunSourceError::Compile`] for a case that
/// survived these shape filters is a front-end failure and fails the harness.
fn check_spec_case(ident: &str, case: &rue_test_runner::Case) -> CaseOutcome {
    let is_known_gap = KNOWN_ORACLE_GAPS
        .iter()
        .any(|(i, n, _)| *i == ident && *n == case.name);
    check_spec_case_with_known_gap(ident, case, is_known_gap)
}

fn check_spec_case_with_known_gap(
    ident: &str,
    case: &rue_test_runner::Case,
    is_known_gap: bool,
) -> CaseOutcome {
    let golden_only = case.has_golden_ir_assertions() && !case.has_execution_assertions();
    // A `target`-pinned case's expected exit is target-specific (e.g. it matches
    // on `@target_arch()`), and a case restricted by `only_on` to other hosts is
    // built for a target the oracle isn't evaluating for. The oracle interprets a
    // single source with no `--target` notion, so neither is a program it can be
    // asked to reproduce — classify both as ineligible, matching the spec
    // runner's `only_on` exclusion.
    let target_specific =
        case.target.is_some() || rue_test_runner::should_skip_for_platform(&case.only_on).is_some();
    if case.skip
        || case.compile_fail
        || case.compile_only
        || case.stdin.is_some()
        || !case.aux_files.is_empty()
        || golden_only
        || target_specific
    {
        return CaseOutcome::Ineligible;
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
        return CaseOutcome::Ineligible;
    };

    let preview_features = spec_preview_features(case);
    let expected_trap = trap::trap_expectation(case.runtime_error.iter().map(String::as_str));

    match run_source_with_preview_features(&case.source, &preview_features) {
        // Unsupported means the interpreter cannot model a valid program.
        // Compile means this eligible corpus case was rejected by
        // the shared front end and must fail rather than disappear as unmodeled.
        Err(error) => {
            let context = format!("{ident} :: {}", case.name);
            record_oracle_error(&context, error)
        }
        Ok(outcome) => {
            let trap_comparison = compare_trap_expectation(expected_trap, outcome.panic);
            if let TrapComparison::UndeclaredActual(actual) = trap_comparison {
                return CaseOutcome::Disagreement(format!(
                    "{ident} :: {}\n      trap contract: oracle got {actual:?}, but the case \
                     declares no runtime trap cause",
                    case.name
                ));
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
            let trap_ok = trap_comparison == TrapComparison::Match;
            let agrees = exit_ok && stdout_ok && trap_ok;

            if is_known_gap {
                // xfail semantics (mirroring rue-cli-tests `known_bug`): the case
                // is expected to diverge because of a tracked oracle bug. If it
                // now AGREES, the gap is fixed — fail loudly so the entry is
                // removed and the case becomes a live regression check again.
                return if agrees {
                    CaseOutcome::Disagreement(format!(
                        "{ident} :: {} — KNOWN_ORACLE_GAPS entry now AGREES; the tracked \
                         oracle gap is fixed, so delete its entry in \
                         crates/rue-oracle-diff/src/main.rs",
                        case.name
                    ))
                } else {
                    // Preserve the existing policy: a known wrong-but-Ok oracle
                    // result is excluded from the audit as ineligible, rather
                    // than being conflated with Unsupported/unmodeled coverage.
                    CaseOutcome::Ineligible
                };
            }

            // A missing trap model cannot erase a concrete exit/stdout
            // disagreement or bypass the known-gap registry above. Only
            // classify it as coverage when every independently modeled
            // observation agrees.
            if exit_ok && stdout_ok && trap_comparison == TrapComparison::UnmodeledExpectation {
                return CaseOutcome::Unmodeled;
            }

            if agrees {
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
    if case.skip || rue_test_runner::should_skip_for_platform(&case.only_on).is_some() {
        return CaseOutcome::Ineligible;
    }

    let known_bug_applies = case.known_bug.is_some()
        && (case.known_bug_on.is_empty()
            || case
                .known_bug_on
                .iter()
                .any(|platform| platform == rue_test_runner::get_host_target()));

    // Shapes the harness cannot drive through the single-source oracle.
    if case.compile_fail
        || case.compile_only
        || known_bug_applies
        || case.stdin.is_some()
        // The in-process oracle compiles one source string; it cannot reproduce
        // CLI-only environment-dependent loading such as RUE_STD_PATH. Treat
        // any declared environment as a shape limitation before compilation.
        || !case.env.is_empty()
        // `source_path` names a repository input that the oracle-diff Buck
        // target does not own; do not silently substitute an inline file.
        || case.source_path.is_some()
        || case.files.len() != 1
    {
        return CaseOutcome::Ineligible;
    }

    let Some(preview_features) = corpus_preview_features(case) else {
        return CaseOutcome::Ineligible;
    };
    let source = &case.files[0].source;
    let expected_trap =
        trap::trap_expectation(case.runtime_error_contains.iter().map(String::as_str));

    // A program that expects a runtime panic exits 101 by convention.
    let expected_exit = case
        .exit_code
        .unwrap_or(if case.runtime_error_contains.is_empty() {
            0
        } else {
            101
        });

    match run_source_with_preview_features(source, &preview_features) {
        Err(error) => {
            let context = format!("{} :: {}", rel(path), case.name);
            record_oracle_error(&context, error)
        }
        Ok(outcome) => {
            let trap_comparison = compare_trap_expectation(expected_trap, outcome.panic);
            if let TrapComparison::UndeclaredActual(actual) = trap_comparison {
                return CaseOutcome::Disagreement(format!(
                    "{} :: {}\n      trap contract: oracle got {actual:?}, but the case \
                     declares no runtime trap cause",
                    rel(path),
                    case.name
                ));
            }
            let exit_ok = outcome.exit_code == expected_exit;
            let stdout_ok = case.stdout.as_ref().is_none_or(|s| &outcome.stdout == s);
            let missing_stdout = case
                .stdout_contains
                .iter()
                .find(|expected| !outcome.stdout.contains(expected.as_str()));
            let trap_ok = trap_comparison == TrapComparison::Match;
            if exit_ok
                && stdout_ok
                && missing_stdout.is_none()
                && trap_comparison == TrapComparison::UnmodeledExpectation
            {
                return CaseOutcome::Unmodeled;
            }
            if exit_ok && stdout_ok && missing_stdout.is_none() && trap_ok {
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
                if let TrapComparison::Mismatch { expected, actual } = trap_comparison {
                    msg += &format!("\n      trap: expected {expected:?}, oracle got {actual:?}");
                }
                CaseOutcome::Disagreement(msg)
            }
        }
    }
}

/// Classify an oracle error without conflating a compiler rejection with an
/// interpreter coverage gap. Corpus/spec callers prefilter intentional
/// `compile_fail` cases, so any [`RunSourceError::Compile`] reaching this helper
/// is an unexpected front-end failure in an eligible program.
fn record_oracle_error(context: &str, error: RunSourceError) -> CaseOutcome {
    match error {
        RunSourceError::Compile(errors) => {
            CaseOutcome::FrontendFailure(format!("{context}\n      {errors:#?}"))
        }
        RunSourceError::Unsupported(_) => CaseOutcome::Unmodeled,
    }
}

fn empty_preview_features() -> PreviewFeatures {
    PreviewFeatures::new()
}

fn parse_preview_feature(name: &str) -> Option<PreviewFeature> {
    PreviewFeature::from_str(name).ok()
}

fn corpus_preview_features(case: &Case) -> Option<PreviewFeatures> {
    let Some(args) = &case.args else {
        return Some(empty_preview_features());
    };
    let only_source = case.files.first()?;
    let mut features = empty_preview_features();
    let mut saw_source = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--preview" => {
                let feature = args
                    .get(i + 1)
                    .and_then(|name| parse_preview_feature(name))?;
                features.insert(feature);
                i += 2;
            }
            "-o" | "--output" => {
                args.get(i + 1)?;
                i += 2;
            }
            arg if !arg.starts_with('-') => {
                if arg != only_source.path {
                    return None;
                }
                saw_source = true;
                i += 1;
            }
            _ => return None,
        }
    }

    saw_source.then_some(features)
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
        let before = files.len();
        match collect_toml(dir, &mut files) {
            Ok(()) if files.len() == before => failures.push(format!(
                "no .toml case files found under requested root {}",
                dir.display()
            )),
            Ok(()) => {}
            Err(mut errors) => failures.append(&mut errors),
        }
    }
    files.sort();
    files.dedup();
    failures.sort();
    (files, failures)
}

/// Recursively collect TOML files, reporting rather than hiding filesystem errors.
fn collect_toml(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Vec<String>> {
    let mut failures = Vec::new();
    let mut visited_dirs = BTreeSet::new();
    collect_toml_inner(dir, out, &mut failures, &mut visited_dirs);
    if failures.is_empty() {
        Ok(())
    } else {
        failures.sort();
        Err(failures)
    }
}

fn collect_toml_inner(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    failures: &mut Vec<String>,
    visited_dirs: &mut BTreeSet<PathBuf>,
) {
    let canonical_dir = match std::fs::canonicalize(dir) {
        Ok(path) => path,
        Err(error) => {
            failures.push(format!(
                "could not resolve cases directory {}: {error}",
                dir.display()
            ));
            return;
        }
    };
    if !visited_dirs.insert(canonical_dir) {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            failures.push(format!(
                "could not read cases directory {}: {error}",
                dir.display()
            ));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(format!(
                    "could not read a directory entry under {}: {error}",
                    dir.display()
                ));
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                failures.push(format!(
                    "could not determine file type for {}: {error}",
                    path.display()
                ));
                continue;
            }
        };

        if file_type.is_dir() {
            collect_toml_inner(&path, out, failures, visited_dirs);
        } else if file_type.is_file() {
            collect_toml_file(&path, out);
        } else if file_type.is_symlink() {
            // Buck inputs and user-provided case roots may contain symlinks.
            // Follow them explicitly, but surface a broken link as discovery
            // failure instead of silently treating it as an absent case.
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => {
                    collect_toml_inner(&path, out, failures, visited_dirs);
                }
                Ok(metadata) if metadata.is_file() => collect_toml_file(&path, out),
                Ok(_) => {}
                Err(error) => failures.push(format!(
                    "could not determine symlink target type for {}: {error}",
                    path.display()
                )),
            }
        }
    }
}

fn collect_toml_file(path: &Path, out: &mut Vec<PathBuf>) {
    if path
        .extension()
        .is_some_and(|extension| extension == "toml")
    {
        out.push(path.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_oracle::Unsupported;

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
        assert_eq!(report.unmodeled, 0);
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
        assert!(matches!(
            check_case(Path::new("platform.toml"), &case),
            CaseOutcome::Ineligible
        ));
    }

    #[test]
    fn known_bug_on_scopes_the_expected_failure() {
        let mut case = corpus_case("fn main() -> i32 { 0 }", false);
        case.known_bug = Some("RUE-test".to_string());
        assert!(matches!(
            check_case(Path::new("known-bug.toml"), &case),
            CaseOutcome::Ineligible
        ));

        case.known_bug_on = vec![rue_test_runner::get_host_target().to_string()];
        assert!(matches!(
            check_case(Path::new("known-bug.toml"), &case),
            CaseOutcome::Ineligible
        ));

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
        assert!(matches!(
            check_case(Path::new("source-path.toml"), &case),
            CaseOutcome::Ineligible
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
            CaseOutcome::Unmodeled
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
    fn undeclared_cli_trap_is_a_hard_contract_failure() {
        let mut case = corpus_case("fn main() -> i32 { let x: i32 = 2147483647; x + 1 }", false);
        case.exit_code = Some(101);

        let outcome = check_case(Path::new("undeclared-trap.toml"), &case);
        let CaseOutcome::Disagreement(message) = &outcome else {
            panic!("undeclared typed trap did not produce a disagreement");
        };
        assert!(message.contains("trap contract"));
        assert!(message.contains("ArithmeticOverflow"));

        // A modeled agreement elsewhere in the corpus must not make a missing
        // declaration aggregate-allowed like an ordinary coverage gap.
        let mut report = Report::default();
        report.record(CaseOutcome::Agree);
        report.record(outcome);
        assert_eq!(report.unmodeled, 0);
        assert_eq!(report.disagreements.len(), 1);
        assert_eq!(finish_report(&report, "test"), ExitCode::FAILURE);
    }

    #[test]
    fn undeclared_spec_trap_is_hard_even_for_a_known_gap() {
        let case = rue_test_runner::Case {
            name: "undeclared spec trap".to_string(),
            source: "fn main() -> i32 { let x: i32 = 2147483647; x + 1 }".to_string(),
            exit_code: Some(101),
            ..Default::default()
        };

        for outcome in [
            check_spec_case("test:1", &case),
            check_spec_case_with_known_gap("test:1", &case, true),
        ] {
            let CaseOutcome::Disagreement(message) = outcome else {
                panic!("undeclared typed trap was hidden by spec classification");
            };
            assert!(message.contains("trap contract"));
            assert!(message.contains("ArithmeticOverflow"));
            assert!(!message.contains("KNOWN_ORACLE_GAPS entry now AGREES"));
        }
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

        let unsupported_source = "fn main() -> u32 { let value: u32 = @random_u32(); value }";
        let unsupported_cli = corpus_case(unsupported_source, false);
        assert!(matches!(
            check_case(Path::new("unsupported.toml"), &unsupported_cli),
            CaseOutcome::Unmodeled
        ));

        let unsupported_spec = rue_test_runner::Case {
            name: "unsupported semantics".to_string(),
            source: unsupported_source.to_string(),
            exit_code: Some(0),
            ..Default::default()
        };
        assert!(matches!(
            check_spec_case("test:1", &unsupported_spec),
            CaseOutcome::Unmodeled
        ));
    }

    #[test]
    fn unknown_runtime_expectations_are_counted_as_unmodeled() {
        let mut case = corpus_case("fn main() -> i32 { let x: i32 = 2147483647; x + 1 }", false);
        case.exit_code = None;
        case.runtime_error_contains = vec!["panic: integer overflow".to_string()];

        let mut report = Report::default();
        report.record(check_case(Path::new("trap.toml"), &case));

        assert_eq!(report.unmodeled, 1);
        assert!(report.disagreements.is_empty());
        assert_eq!(report.modeled_eligible_count(), 0);

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
    fn report_records_each_terminal_case_outcome_once() {
        let mut report = Report::default();
        report.record(CaseOutcome::Agree);
        report.record(CaseOutcome::Unmodeled);
        report.record(CaseOutcome::Ineligible);
        report.record(CaseOutcome::FrontendFailure("frontend".to_string()));
        report.record(CaseOutcome::Disagreement("disagreement".to_string()));

        assert_eq!(report.agree, 1);
        assert_eq!(report.unmodeled, 1);
        assert_eq!(report.ineligible, 1);
        assert_eq!(report.frontend_failures, vec!["frontend".to_string()]);
        assert_eq!(report.disagreements, vec!["disagreement".to_string()]);
    }

    #[test]
    fn an_all_unmodeled_corpus_fails_closed() {
        let mut report = Report::default();

        report.record(record_oracle_error(
            "unsupported probe",
            RunSourceError::Unsupported(Unsupported("known coverage gap".to_string())),
        ));

        assert_eq!(report.unmodeled, 1);
        assert_eq!(report.modeled_eligible_count(), 0);
        assert!(report.frontend_failures.is_empty());
        assert_eq!(finish_report(&report, "test"), ExitCode::FAILURE);
    }

    #[test]
    fn agreement_plus_unmodeled_coverage_is_successful() {
        let mut report = Report::default();
        report.record(CaseOutcome::Agree);
        report.record(CaseOutcome::Unmodeled);

        assert_eq!(report.modeled_eligible_count(), 1);
        assert_eq!(finish_report(&report, "test"), ExitCode::SUCCESS);
    }

    #[test]
    fn an_all_ineligible_corpus_fails_closed() {
        let case = corpus_case("this is intentionally not Rue", true);
        let mut report = Report::default();

        report.record(check_case(Path::new("compile_fail.toml"), &case));

        assert_eq!(report.ineligible, 1);
        assert_eq!(report.unmodeled, 0);
        assert_eq!(report.modeled_eligible_count(), 0);
        assert!(report.frontend_failures.is_empty());
        assert_eq!(finish_report(&report, "test"), ExitCode::FAILURE);
    }

    #[test]
    fn environment_dependent_case_is_prefiltered() {
        let mut case = corpus_case("this would fail if the oracle compiled it", false);
        case.env
            .insert("RUE_STD_PATH".to_string(), "${REAL_STD}".to_string());
        let mut report = Report::default();

        report.record(check_case(Path::new("environment.toml"), &case));

        assert_eq!(report.ineligible, 1);
        assert_eq!(report.unmodeled, 0);
        assert!(report.frontend_failures.is_empty());
    }
}
