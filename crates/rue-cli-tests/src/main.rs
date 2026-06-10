//! End-to-end CLI integration tests for the Rue compiler (RUE-12).
//!
//! Unlike the spec suite (which exercises the compiler through the test
//! harness with pre-injected files), these tests exercise the compiler
//! **the way a user does**: real files on disk in a temp directory, the
//! actual `rue` binary invoked with relative paths and a controlled
//! environment, stdin piped to the compiled program, and stdout/exit codes
//! asserted.
//!
//! # Why this exists
//!
//! The spec suite gave a false sense of health: `@import` resolution from
//! disk and large by-value aggregates were both broken in the shipped driver
//! while 100% of spec tests passed. See Linear issues RUE-12/13/14.
//!
//! # Case format
//!
//! Cases live in `crates/rue-cli-tests/cases/*.toml`:
//!
//! ```toml
//! [section]
//! id = "cli.basics"
//! name = "Basic CLI behavior"
//!
//! [[case]]
//! name = "hello_string"
//! files = [{ path = "main.rue", source = """
//! fn main() -> i32 {
//!     @dbg("Hello!");
//!     0
//! }
//! """ }]
//! stdout = "Hello!\n"
//! exit_code = 0
//! ```
//!
//! Optional fields:
//! - `args`: explicit compiler args (default: `[<first file>, "-o", "prog"]`)
//! - `output`: name of produced executable (default `"prog"`)
//! - `env`: extra env vars for the compiler; the value `"${REAL_STD}"`
//!   expands to the absolute path of the repo's `std/` directory
//! - `stdin`: piped to the compiled program when it runs
//! - `compile_fail` + `error_contains`: expect compilation failure
//! - `compile_only`: don't run the produced binary
//! - `compile_stdout_contains`: assert on compiler stdout (e.g. `--emit`)
//! - `stdout` / `stdout_contains`: assert on the program's stdout
//! - `runtime_error_contains`: assert on the program's stderr
//! - `exit_code`: expected program exit code (default 0)
//! - `known_bug = "RUE-NN"`: expected failure (xfail). The case runs; if it
//!   fails, it is reported as ignored with the bug reference. If it PASSES,
//!   the suite fails loudly so the marker gets removed when the bug is fixed.
//!
//! # ICE detection
//!
//! Any compiler invocation that dies by signal or whose stderr contains a
//! Rust panic is reported as an INTERNAL COMPILER ERROR — a distinct, loud
//! failure class, regardless of what the case expected (it still counts as
//! "failure" for `known_bug` purposes).
//!
//! # Limitations
//!
//! There is no per-case timeout; programs under test must terminate. Use
//! `compile_only = true` for sources with infinite loops.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use libtest2_mimic::{Harness, RunContext, RunError, Trial};
use rue_test_runner::{find_dir, find_rue_binary};
use serde::Deserialize;

/// Possible paths for the cases directory.
const CASES_DIR_PATHS: &[&str] = &[
    "crates/rue-cli-tests/cases",
    "cases",
    "../rue-cli-tests/cases",
];

/// Possible paths for the repo's std library (for `${REAL_STD}` expansion).
const STD_DIR_PATHS: &[&str] = &["std", "../std", "../../std"];

#[derive(Debug, Deserialize)]
struct TestFile {
    section: Section,
    #[serde(default, rename = "case")]
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Section {
    id: String,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SourceFile {
    path: String,
    source: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Case {
    name: String,
    /// Files written to the temp directory before invoking the compiler.
    files: Vec<SourceFile>,
    /// Compiler arguments, relative to the temp dir (default: first file + `-o prog`).
    #[serde(default)]
    args: Option<Vec<String>>,
    /// Name of the executable the compiler is expected to produce.
    #[serde(default)]
    output: Option<String>,
    /// Extra environment variables for the compiler invocation.
    #[serde(default)]
    env: HashMap<String, String>,
    /// Piped to the compiled program's stdin.
    #[serde(default)]
    stdin: Option<String>,
    /// Expect compilation to fail.
    #[serde(default)]
    compile_fail: bool,
    /// Substrings expected in the compiler's stderr when compilation fails.
    #[serde(default)]
    error_contains: Vec<String>,
    /// Compile but don't run the produced binary.
    #[serde(default)]
    compile_only: bool,
    /// Substrings expected in the compiler's stdout (e.g. `--emit` output).
    #[serde(default)]
    compile_stdout_contains: Vec<String>,
    /// Exact expected program stdout.
    #[serde(default)]
    stdout: Option<String>,
    /// Substrings expected in the program's stdout.
    #[serde(default)]
    stdout_contains: Vec<String>,
    /// Substrings expected in the program's stderr (runtime panics).
    #[serde(default)]
    runtime_error_contains: Vec<String>,
    /// Expected program exit code (default 0).
    #[serde(default)]
    exit_code: Option<i32>,
    /// Expected failure: reference to the Linear issue tracking the bug.
    #[serde(default)]
    known_bug: Option<String>,
    /// Skip this case entirely.
    #[serde(default)]
    skip: bool,
}

type TestResult = Result<(), String>;

/// Check a finished process for signs of a compiler panic / ICE.
fn ice_message(status: &std::process::ExitStatus, stderr: &str) -> Option<String> {
    if stderr.contains("panicked at") || stderr.contains("internal compiler error") {
        return Some(format!(
            "INTERNAL COMPILER ERROR: compiler panicked\n--- compiler stderr ---\n{}",
            stderr
        ));
    }
    // Death by signal (e.g. SIGABRT after a Rust abort) has no exit code on unix.
    if status.code().is_none() {
        return Some(format!(
            "INTERNAL COMPILER ERROR: compiler killed by signal ({:?})\n--- compiler stderr ---\n{}",
            status, stderr
        ));
    }
    None
}

/// Expand `${REAL_STD}` in env values to the absolute path of the repo's std/.
fn expand_env_value(value: &str, real_std: &Path) -> String {
    value.replace("${REAL_STD}", &real_std.to_string_lossy())
}

fn run_case(case: &Case, rue_binary: &Path, real_std: &Path) -> TestResult {
    let temp_dir = tempfile::tempdir().map_err(|e| format!("failed to create temp dir: {}", e))?;
    let dir = temp_dir.path();

    // Write the case's files to disk, creating subdirectories as needed.
    for file in &case.files {
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create dir for {}: {}", file.path, e))?;
        }
        std::fs::write(&path, &file.source)
            .map_err(|e| format!("failed to write {}: {}", file.path, e))?;
    }

    let output_name = case.output.clone().unwrap_or_else(|| "prog".to_string());

    // Default invocation mirrors what a user types: `rue main.rue -o prog`,
    // with RELATIVE paths and the temp dir as the working directory.
    let args: Vec<String> = match &case.args {
        Some(args) => args.clone(),
        None => {
            let first = case.files.first().ok_or("case has no files")?.path.clone();
            vec![first, "-o".to_string(), output_name.clone()]
        }
    };

    let mut cmd = Command::new(rue_binary);
    cmd.args(&args).current_dir(dir);
    for (key, value) in &case.env {
        cmd.env(key, expand_env_value(value, real_std));
    }
    let compile_output = cmd
        .output()
        .map_err(|e| format!("failed to invoke rue compiler: {}", e))?;

    let compile_stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
    let compile_stdout = String::from_utf8_lossy(&compile_output.stdout).to_string();

    // ICE detection comes first: a compiler panic is never acceptable output,
    // even for compile_fail cases.
    if let Some(ice) = ice_message(&compile_output.status, &compile_stderr) {
        return Err(ice);
    }

    let compile_succeeded = compile_output.status.success();

    if case.compile_fail {
        if compile_succeeded {
            return Err("expected compilation to fail, but it succeeded".to_string());
        }
        for expected in &case.error_contains {
            if !compile_stderr.contains(expected) {
                return Err(format!(
                    "compiler error mismatch:\n  expected stderr to contain: {}\n--- actual stderr ---\n{}",
                    expected, compile_stderr
                ));
            }
        }
        return Ok(());
    }

    if !compile_succeeded {
        return Err(format!(
            "compilation failed (exit: {:?}):\n--- compiler stdout ---\n{}\n--- compiler stderr ---\n{}",
            compile_output.status.code(),
            compile_stdout,
            compile_stderr
        ));
    }

    for expected in &case.compile_stdout_contains {
        if !compile_stdout.contains(expected) {
            return Err(format!(
                "compiler stdout mismatch:\n  expected to contain: {}\n--- actual stdout ---\n{}",
                expected, compile_stdout
            ));
        }
    }

    if case.compile_only {
        return Ok(());
    }

    // Run the produced binary from the temp dir.
    let program = dir.join(&output_name);
    if !program.exists() {
        return Err(format!(
            "compiler reported success but did not produce '{}'",
            output_name
        ));
    }

    let mut run_cmd = Command::new(&program);
    run_cmd
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = run_cmd
        .spawn()
        .map_err(|e| format!("failed to run compiled program: {}", e))?;

    if let Some(input) = &case.stdin {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        // The program may exit without reading all input; a broken pipe here
        // is not a test failure.
        let _ = stdin.write_all(input.as_bytes());
    } else {
        drop(child.stdin.take());
    }

    let run_output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for program: {}", e))?;

    let run_stdout = String::from_utf8_lossy(&run_output.stdout).to_string();
    let run_stderr = String::from_utf8_lossy(&run_output.stderr).to_string();

    let expected_exit = case.exit_code.unwrap_or(0);
    let actual_exit = run_output.status.code();
    if actual_exit != Some(expected_exit) {
        return Err(format!(
            "program exit code mismatch:\n  expected: {}\n  actual: {:?}\n--- program stdout ---\n{}\n--- program stderr ---\n{}",
            expected_exit, actual_exit, run_stdout, run_stderr
        ));
    }

    if let Some(expected) = &case.stdout {
        if &run_stdout != expected {
            return Err(format!(
                "program stdout mismatch:\n--- expected ---\n{}\n--- actual ---\n{}",
                expected, run_stdout
            ));
        }
    }

    for expected in &case.stdout_contains {
        if !run_stdout.contains(expected) {
            return Err(format!(
                "program stdout mismatch:\n  expected to contain: {}\n--- actual stdout ---\n{}",
                expected, run_stdout
            ));
        }
    }

    for expected in &case.runtime_error_contains {
        if !run_stderr.contains(expected) {
            return Err(format!(
                "program stderr mismatch:\n  expected to contain: {}\n--- actual stderr ---\n{}",
                expected, run_stderr
            ));
        }
    }

    Ok(())
}

/// Wrapper handling skip and known_bug (xfail) semantics.
fn run_case_wrapper(
    case: &Case,
    rue_binary: &Path,
    real_std: &Path,
    ctx: RunContext<'_>,
) -> Result<(), RunError> {
    if case.skip {
        return ctx.ignore_for("marked as skip");
    }

    let result = run_case(case, rue_binary, real_std);

    match (&case.known_bug, result) {
        // Normal case.
        (None, Ok(())) => Ok(()),
        (None, Err(e)) => Err(RunError::fail(e)),
        // Expected failure: report as ignored with the bug reference.
        (Some(bug), Err(e)) => {
            let first_line = e.lines().next().unwrap_or("");
            ctx.ignore_for(format!(
                "known bug {} (expected failure): {}",
                bug, first_line
            ))
        }
        // Unexpected pass: the bug may be fixed — demand marker removal.
        (Some(bug), Ok(())) => Err(RunError::fail(format!(
            "test PASSED but is marked known_bug = \"{}\". If the bug is fixed, \
             remove the known_bug marker so this becomes a regression test.",
            bug
        ))),
    }
}

fn load_cases(cases_dir: &Path) -> Vec<(String, TestFile)> {
    let mut toml_files = Vec::new();
    rue_test_runner::collect_toml_files(cases_dir, &mut toml_files);
    toml_files.sort();

    let mut out = Vec::new();
    for path in toml_files {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: failed to read {}: {}", path.display(), e);
                std::process::exit(1);
            }
        };
        match toml::from_str::<TestFile>(&content) {
            Ok(tf) => out.push((path.display().to_string(), tf)),
            Err(e) => {
                eprintln!("error: failed to parse {}: {}", path.display(), e);
                std::process::exit(1);
            }
        }
    }
    out
}

fn main() {
    // The compiler is invoked with the test's temp dir as cwd, so the binary
    // path must be absolute (find_rue_binary may return a relative path).
    let rue_binary = find_rue_binary();
    let rue_binary = rue_binary.canonicalize().unwrap_or_else(|e| {
        eprintln!(
            "error: cannot resolve rue binary '{}': {}",
            rue_binary.display(),
            e
        );
        std::process::exit(1);
    });
    let cases_dir = find_dir("RUE_CLI_CASES", CASES_DIR_PATHS, "cases");
    let real_std = find_dir("RUE_STD_DIR", STD_DIR_PATHS, "std");
    let real_std = real_std.canonicalize().unwrap_or(real_std);

    let files = load_cases(&cases_dir);

    let total: usize = files.iter().map(|(_, f)| f.cases.len()).sum();
    let mut tests: Vec<Trial> = Vec::with_capacity(total);

    for (_, file) in files {
        let section_id = file.section.id.clone();
        for case in file.cases {
            let test_name = format!("{}::{}", section_id, case.name);
            let rue_binary = rue_binary.clone();
            let real_std = real_std.clone();
            tests.push(Trial::test(test_name, move |ctx| {
                run_case_wrapper(&case, &rue_binary, &real_std, ctx)
            }));
        }
    }

    if tests.is_empty() {
        eprintln!(
            "warning: no CLI test cases found in {}",
            cases_dir.display()
        );
    }

    Harness::with_env().discover(tests).main();
}
