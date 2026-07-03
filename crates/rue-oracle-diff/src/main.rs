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
//! Two modes:
//!
//! - **corpus** (default): `rue-oracle-diff [cases-dir ...]` runs the concrete
//!   rue-cli-tests corpus through the oracle. Case-directory resolution, in
//!   order: explicit argv paths; else the `RUE_ORACLE_DIFF_CASES` env var (a
//!   single dir — how the `buck2 test` sh_test feeds the rue-cli-tests `cases`
//!   filegroup); else the default `crates/rue-cli-tests/cases`.
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
    // Subcommand dispatch: `rue-oracle-diff fuzz [...]` runs the differential
    // *fuzzer* (generate valid programs, cross-check oracle vs compiled binary);
    // with no subcommand it runs the corpus differential (below).
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("fuzz") {
        return fuzz::run(&raw[1..]);
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

    let total = report.agree
        + report.skip_unsupported
        + report.skip_nonrunnable
        + report.disagreements.len() as u32;
    println!("\n=== rue-oracle-diff: differential agreement over {total} cases ===");
    println!("  agree:            {}", report.agree);
    println!("  skip (unmodeled): {}", report.skip_unsupported);
    println!("  skip (non-runnable shape): {}", report.skip_nonrunnable);
    println!("  DISAGREEMENTS:    {}", report.disagreements.len());
    for d in &report.disagreements {
        println!("\n  ✗ {d}");
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
