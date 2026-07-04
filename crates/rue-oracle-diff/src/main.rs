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
//!   suite does, then filter to the runnable subset the oracle can model
//!   (concrete `source` + expected exit/stdout; golden-IR-only, preview-gated,
//!   `compile_fail`, multi-file and stdin cases are skipped, not disagreements).
//!   Dir resolution mirrors corpus mode: argv; else `RUE_ORACLE_DIFF_SPEC_CASES`
//!   (how the `buck2 test` sh_test feeds the rue-spec `cases` filegroup); else
//!   `crates/rue-spec/cases`.
//! - **fuzz** (`rue-oracle-diff fuzz [...]`): the differential *fuzzer* of
//!   RUE-247 — generate random valid programs and cross-check the oracle
//!   against the real compiler + native binary. See [`fuzz`]. A `dump <seed>`
//!   subcommand prints a generated program for inspection.
//!
//! Every mode exits non-zero if any disagreement is found.

mod fuzz;
mod generator;

use rue_oracle::run_source;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Deserialize)]
struct TestFile {
    #[serde(default, rename = "case")]
    cases: Vec<Case>,
}

/// A permissive subset of the rue-cli-tests case schema — only the fields the
/// oracle can act on. Unknown fields (spec references, substring matchers, …)
/// are ignored.
#[derive(Deserialize)]
struct Case {
    name: String,
    #[serde(default)]
    files: Vec<SourceFile>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    stdin: Option<String>,
    #[serde(default)]
    compile_fail: bool,
    #[serde(default)]
    compile_only: bool,
    #[serde(default)]
    stdout: Option<String>,
    #[serde(default)]
    runtime_error_contains: Vec<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    known_bug: Option<String>,
    #[serde(default)]
    skip: bool,
}

#[derive(Deserialize)]
struct SourceFile {
    #[allow(dead_code)]
    path: String,
    source: String,
}

#[derive(Default)]
struct Report {
    agree: u32,
    /// oracle could not model this program (outside its coverage) — not a bug
    skip_unsupported: u32,
    /// case shape the harness cannot drive (multi-file, stdin, custom args, …)
    skip_nonrunnable: u32,
    /// oracle ran but disagreed with the expected behavior — a bug
    disagreements: Vec<String>,
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
    let mut toml_files = Vec::new();
    for dir in &dirs {
        collect_toml(dir, &mut toml_files);
    }
    if toml_files.is_empty() {
        eprintln!(
            "no .toml case files found under {:?} (run from the repo root)",
            dirs
        );
        return ExitCode::FAILURE;
    }

    for path in &toml_files {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skip {}: {e}", path.display());
                continue;
            }
        };
        let file: TestFile = match toml::from_str(&text) {
            Ok(f) => f,
            // A case file the harness's subset schema can't parse is not a
            // failure of the oracle — skip it (spec-only fields, etc.).
            Err(_) => continue,
        };
        for case in &file.cases {
            check_case(path, case, &mut report);
        }
    }

    finish_report(&report, "rue-cli-tests")
}

/// Print the tallied report and turn it into a process exit code: success when
/// the oracle agreed with the compiler on every runnable case, failure on any
/// disagreement. Shared by [`corpus_mode`] and [`spec_mode`]; `corpus` names
/// which corpus was differenced, for the header.
fn finish_report(report: &Report, corpus: &str) -> ExitCode {
    let total = report.agree
        + report.skip_unsupported
        + report.skip_nonrunnable
        + report.disagreements.len() as u32;
    println!("\n=== rue-oracle-diff: differential agreement over {total} {corpus} cases ===");
    println!("  agree:            {}", report.agree);
    println!("  skip (unmodeled): {}", report.skip_unsupported);
    println!("  skip (non-runnable shape): {}", report.skip_nonrunnable);
    println!("  DISAGREEMENTS:    {}", report.disagreements.len());
    for d in &report.disagreements {
        println!("\n  ✗ {d}");
    }

    // Zero cases checked means the corpus wiring is broken (empty dir, stale
    // env-var path, TOMLs with no parseable cases) — a no-op differential run
    // must not report success (the RUE-341 coverage-overstatement hazard).
    if total == 0 {
        println!("\nno runnable {corpus} cases were checked — corpus dir empty or misconfigured.");
        return ExitCode::FAILURE;
    }

    if report.disagreements.is_empty() {
        println!("\noracle agrees with the compiler on every runnable case.");
        ExitCode::SUCCESS
    } else {
        println!(
            "\n{} disagreement(s) — each is a bug in the oracle or (more likely) codegen.",
            report.disagreements.len()
        );
        ExitCode::FAILURE
    }
}

/// The spec-corpus differential (RUE-204): expand each rue-spec templated case
/// and run the runnable subset through the oracle, checking three-way agreement
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
                check_spec_case(&ident, case, &mut report);
            }
        }
    }

    finish_report(&report, "rue-spec")
}

/// Run one expanded rue-spec case through the oracle, tallying the outcome.
///
/// Skipped (as a non-runnable shape, never a disagreement) when the case is not
/// a single concrete program the oracle can execute and compare: `compile_fail`
/// (no runtime output), `preview`-gated (the oracle models only stable
/// semantics), `compile_only` (never executed), golden-IR-only (an IR dump, not
/// a run), or a shape with no oracle model (stdin, multi-file `aux_files`).
/// Cases the oracle simply can't model yet return [`Unsupported`] and count as
/// unmodeled skips.
fn check_spec_case(ident: &str, case: &rue_test_runner::Case, report: &mut Report) {
    let golden_only = case.has_golden_ir_assertions() && !case.has_execution_assertions();
    // A `target`-pinned case's expected exit is target-specific (e.g. it matches
    // on `@target_arch()`), and a case restricted by `only_on` to other hosts is
    // built for a target the oracle isn't evaluating for. The oracle interprets a
    // single source with no `--target` notion, so neither is a program it can be
    // asked to reproduce — skip both, exactly as the spec runner skips `only_on`.
    let target_specific =
        case.target.is_some() || rue_test_runner::should_skip_for_platform(&case.only_on).is_some();
    if case.skip
        || case.compile_fail
        || case.preview.is_some()
        || case.compile_only
        || case.stdin.is_some()
        || !case.aux_files.is_empty()
        || golden_only
        || target_specific
    {
        report.skip_nonrunnable += 1;
        return;
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
        report.skip_nonrunnable += 1;
        return;
    };

    let is_known_gap = KNOWN_ORACLE_GAPS
        .iter()
        .any(|(i, n, _)| *i == ident && *n == case.name);

    match run_source(&case.source) {
        // Oracle bailed (unmodeled construct). For a tracked gap this is the
        // "skip Unsupported cleanly" outcome; either way it's a skip, not a bug.
        Err(_unsupported) => report.skip_unsupported += 1,
        Ok(outcome) => {
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
            let agrees = exit_ok && stdout_ok;

            if is_known_gap {
                // xfail semantics (mirroring rue-cli-tests `known_bug`): the case
                // is expected to diverge because of a tracked oracle bug. If it
                // now AGREES, the gap is fixed — fail loudly so the entry is
                // removed and the case becomes a live regression check again.
                if agrees {
                    report.disagreements.push(format!(
                        "{ident} :: {} — KNOWN_ORACLE_GAPS entry now AGREES; the tracked \
                         oracle gap is fixed, so delete its entry in \
                         crates/rue-oracle-diff/src/main.rs",
                        case.name
                    ));
                } else {
                    report.skip_nonrunnable += 1;
                }
                return;
            }

            if agrees {
                report.agree += 1;
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
                report.disagreements.push(msg);
            }
        }
    }
}

/// Spec-corpus cases the oracle is known to model incorrectly (returning a
/// wrong-but-`Ok` result rather than [`Unsupported`]), each paired with the
/// tracking issue. These are **not** miscompiles — the compiler is correct; the
/// oracle is. They are carried as xfails (see [`check_spec_case`]): each is
/// asserted to *still* diverge, so when its tracked bug is fixed the harness
/// fails and points at the entry to delete. Keep this list tiny — it exists
/// only to keep CI green over a documented, tracked oracle limitation, never to
/// paper over a real codegen disagreement.
///
/// Entry: `(section-identifier, case-name, tracking-issue)`.
const KNOWN_ORACLE_GAPS: &[(&str, &str, &str)] = &[
    // (empty) — RUE-285 fixed: the oracle now models structural ==/!= over
    // structs, arrays, and payload enums, so the former aggregate-equality gaps
    // run as normal three-way-agreement checks again.
];

fn check_case(path: &Path, case: &Case, report: &mut Report) {
    // Shapes the harness cannot drive through the single-source oracle.
    if case.skip
        || case.compile_fail
        || case.compile_only
        || case.known_bug.is_some()
        || case.args.is_some()
        || case.stdin.is_some()
        || case.files.len() != 1
    {
        report.skip_nonrunnable += 1;
        return;
    }
    let source = &case.files[0].source;

    // A program that expects a runtime panic exits 101 by convention.
    let expected_exit = case
        .exit_code
        .unwrap_or(if case.runtime_error_contains.is_empty() {
            0
        } else {
            101
        });

    match run_source(source) {
        Err(_unsupported) => report.skip_unsupported += 1,
        Ok(outcome) => {
            let exit_ok = outcome.exit_code == expected_exit;
            let stdout_ok = case.stdout.as_ref().is_none_or(|s| &outcome.stdout == s);
            if exit_ok && stdout_ok {
                report.agree += 1;
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
                report.disagreements.push(msg);
            }
        }
    }
}

fn rel(path: &Path) -> String {
    path.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn collect_toml(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_toml(&p, out);
        } else if p.extension().is_some_and(|e| e == "toml") {
            out.push(p);
        }
    }
}
