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
//! - `source_path`: repo-root-relative source path to compile instead of
//!   inline `files`, for cases that should pin a checked-in example/program
//! - `output`: name of produced executable (default `"prog"`)
//! - `env`: extra env vars for the compiler; the value `"${REAL_STD}"`
//!   expands to the absolute path of the repo's `std/` directory
//! - `stdin`: piped to the compiled program when it runs
//! - `compile_fail` + `error_contains`: expect compilation failure
//! - `compile_only`: don't run the produced binary
//! - `compile_stdout_contains`: substrings that must appear in compiler stdout (e.g. `--emit`)
//! - `compile_stdout_not_contains`: substrings that must not appear in compiler stdout
//! - `stdout` / `stdout_contains`: assert on the program's stdout
//! - `runtime_error_contains`: assert on the program's stderr
//! - `exit_code`: expected program exit code (default 0)
//! - `timeout_ms`: wall-clock limit for running the program (default 10s)
//! - `known_bug = "RUE-NN"`: expected failure (xfail). An ordinary assertion
//!   failure is ignored with the bug reference. A fatal subprocess failure or
//!   unexpected pass fails loudly.
//! - `only_on = ["x86-64-linux", ...]`: run the case only on these hosts
//!   (ignored elsewhere). For behavior that depends on the host platform,
//!   e.g. whether `--target X` is a cross-compile or a native compile.
//! - `differential_opt = true`: compile+run the case once per optimization
//!   level (`-O0`/`-O1`/`-O2`/`-O3`) and assert identical exit code AND stdout
//!   across all levels, catching optimizer miscompiles (RUE-236). The runner
//!   drives `-O`, so the case must not set its own `-O` in `args` and must not
//!   be `compile_fail`/`compile_only`. Give it exact `stdout`/`exit_code`.
//!
//! # ICE detection
//!
//! Any compiler invocation that dies by signal or whose stderr contains a
//! Rust panic is reported as an INTERNAL COMPILER ERROR — a distinct, loud
//! failure class that a `known_bug` marker cannot turn into an expected pass.
//!
//! # Timeouts
//!
//! Both the COMPILE step and the compiled program run under a per-case
//! wall-clock timeout (default [`rue_test_runner::DEFAULT_TIMEOUT_MS`],
//! overridable per case with `timeout_ms`). If either runs long — an infinite
//! loop in generated code, or a compile-time hang in comptime evaluation —
//! its whole process group is killed and the case is reported as a distinct
//! TIMEOUT failure (see [`rue_test_runner::TIMEOUT_PREFIX`]), so one bad
//! case can never hang the suite. `compile_only = true` still skips the run
//! entirely for sources that are meant only to compile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use libtest2_mimic::{Harness, RunContext, RunError, Trial};
use rue_test_runner::{
    DEFAULT_TIMEOUT_MS, ExpectedFailureOutcome, KNOWN_TARGETS, TestFailure, TestResult,
    classify_expected_failure, compiler_command, find_dir, find_rue_binary, ice_message,
    run_with_timeout, validate_nonempty_case_corpus,
};
use serde::Deserialize;

/// Possible paths for the cases directory.
const CASES_DIR_PATHS: &[&str] = &[
    "crates/rue-cli-tests/cases",
    "cases",
    "../rue-cli-tests/cases",
];

/// Possible paths for the repo's std library (for `${REAL_STD}` expansion).
const STD_DIR_PATHS: &[&str] = &["std", "../std", "../../std"];

/// Possible paths for the repo's `examples/` directory (RUE-48 smoke tests).
const EXAMPLES_DIR_PATHS: &[&str] = &["examples", "../../examples", "../examples"];

/// Expected outcome of compiling and running one `examples/**/*.rue` program.
///
/// Every file under `examples/` is compiled and run by the suite (see
/// [`example_trials`]); this table pins the *deterministic* ones to an exact
/// exit code and stdout so a regression that silently changes their output is
/// caught. An example NOT listed here is still smoke-tested — it must compile,
/// produce a binary, run, and exit *normally* (never die by SIGSEGV/SIGABRT) —
/// so dropping a new file into `examples/` is covered automatically without
/// editing this harness. Adding an entry here upgrades that smoke check into a
/// pinned exact-output regression test.
///
/// Exit codes are the low byte of `main()`'s return value (a process exit code
/// is 0-255), e.g. `power.rue` returns 5^6 = 15625, which exits as 15625 % 256
/// = 9.
struct ExampleExpectation {
    /// Path relative to `examples/`, using `/` separators.
    path: &'static str,
    /// Expected process exit code.
    exit_code: i32,
    /// Exact expected stdout.
    stdout: &'static str,
    /// Optional stdin to pipe to the example.
    stdin: Option<&'static str>,
}

const EXAMPLE_EXPECTATIONS: &[ExampleExpectation] = &[
    // The 2026-07-11 ambitious-dogfood programs: each is a self-checking
    // program that returns 42 exactly when every internal @assert passes.
    ExampleExpectation {
        path: "sudoku/main.rue",
        exit_code: 42,
        stdout: "19\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "bignum/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "calculator/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "tinydb/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "maze/main.rue",
        exit_code: 42,
        stdout: "125\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "dijkstra/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "hashmap/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "linear_pool.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "arrays.rue",
        exit_code: 157,
        stdout: "157\n64\n12\n60\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "binary_search.rue",
        exit_code: 4,
        stdout: "4\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "collatz.rue",
        exit_code: 97,
        stdout: "27\n111\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "dbg.rue",
        exit_code: 0,
        stdout: "42\n-17\ntrue\nfalse\n70\ntrue\ntrue\n120\n0\n1\n2\n3\n4\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "fibonacci.rue",
        exit_code: 55,
        stdout: "0\n1\n1\n2\n3\n5\n8\n13\n21\n34\n55\n89\n144\n233\n377\n610\n987\n1597\n2584\n4181\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "first/option_try.rue",
        exit_code: 0,
        stdout: "triple: 12\ntriple: none\n",
        stdin: None,
    },
    // `?` propagation for Result (RUE-591, ADR-0038): Ok unwraps, Err
    // short-circuits. add_one_over(10,2)=Ok(6); add_one_over(10,0)=Err(1).
    ExampleExpectation {
        path: "first/result_try.rue",
        exit_code: 0,
        stdout: "ok: 6\nerr: 1\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "first/stats.rue",
        exit_code: 3,
        stdout: "count: 3\nsum: 13\nmax: 7\n",
        stdin: Some("7\n5\n1\n"),
    },
    ExampleExpectation {
        path: "fizzbuzz.rue",
        exit_code: 0,
        stdout: "1\n2\n1\n4\n2\n1\n7\n8\n1\n2\n11\n1\n13\n14\n3\n16\n17\n1\n19\n2\n1\n22\n23\n1\n2\n26\n1\n28\n29\n3\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "gcd.rue",
        exit_code: 21,
        stdout: "6\n1\n36\n",
        stdin: None,
    },
    // Generic fixed-capacity Stack(T, CAP) (RUE-586 dogfood): a user-defined
    // comptime generic instantiated at two types (i32 and bool) in one program —
    // specialization stress. Pops 7, true; sizes 1, 2; flag true -> 7+1+2 = 10.
    ExampleExpectation {
        path: "generic_stack.rue",
        exit_code: 10,
        stdout: "7\ntrue\n1\n2\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "generics.rue",
        exit_code: 72,
        stdout: "42\n20\n10\n100\n8\n17\n",
        stdin: None,
    },
    // Binary min-heap over ArrayBuf (RUE-586 dogfood): heapsort pops a scramble
    // in ascending order 1..9.
    ExampleExpectation {
        path: "heap.rue",
        exit_code: 9,
        stdout: "1\n2\n3\n4\n5\n6\n7\n8\n9\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "hello.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    // Conway's Game of Life (RUE-586 dogfood): a glider's population is 5 and
    // holds across all eight generations we print, so this pins both the
    // per-generation output and the final population (exit code).
    ExampleExpectation {
        path: "life.rue",
        exit_code: 5,
        stdout: "5\n5\n5\n5\n5\n5\n5\n5\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "match.rue",
        exit_code: 5,
        stdout: "5\n",
        stdin: None,
    },
    // Integer matrix library (RUE-586 dogfood): exercises a @copy struct with a
    // const-sized 2D array field + methods (the RUE-587 pattern). a * identity
    // == a, so trace(a*id) == 1+5+9 == 15; identity trace == 3.
    ExampleExpectation {
        path: "matrix.rue",
        exit_code: 15,
        stdout: "15\n3\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "power.rue",
        exit_code: 9,
        stdout: "1\n2\n1024\n243\n2401\n1024\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "primes.rue",
        exit_code: 25,
        stdout: "2\n3\n5\n7\n11\n13\n17\n19\n23\n29\n31\n37\n41\n43\n47\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "quicksort.rue",
        exit_code: 11,
        stdout: "0\n64\n34\n25\n12\n22\n11\n90\n42\n15\n77\n1\n11\n12\n15\n22\n25\n34\n42\n64\n77\n90\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "second/calculator.rue",
        exit_code: 0,
        stdout: "result: 11\n",
        stdin: Some("2 + 3 * (4 - 1)\n"),
    },
    // Tagged-union geometry (RUE-586 dogfood): enum payloads (single + multi-
    // field), match with tuple bindings, and an array of enum values. Areas
    // 12, 12, 25, 3 sum to 52.
    ExampleExpectation {
        path: "shapes.rue",
        exit_code: 52,
        stdout: "12\n12\n25\n3\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "sqrt.rue",
        exit_code: 12,
        stdout: "0\n1\n2\n2\n3\n3\n4\n10\n31\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "std/arraybuf_demo.rue",
        exit_code: 119,
        stdout: "3\n20\n99\n30\n2\n-1\n119\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "structs.rue",
        exit_code: 50,
        stdout: "25\n50\n30\ntrue\n",
        stdin: None,
    },
    // The onboarding smoke test. README, CONTRIBUTING, and the tutorial's
    // installation page point new users at `scripts/rue exec examples/welcome.rue`
    // to verify their setup, so it MUST print recognizable output and exit 0 —
    // otherwise `set -e`, `&&` chains, and CI steps read a successful run as a
    // failure (RUE-517). Guarded further by test_welcome_example_is_zero_exit.
    ExampleExpectation {
        path: "welcome.rue",
        exit_code: 0,
        stdout: "1\n2\n3\n42\n",
        stdin: None,
    },
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestFile {
    section: Section,
    #[serde(default, rename = "case")]
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    id: String,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    path: String,
    source: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    /// Human-readable explanation of what this case pins and why. Not used by
    /// the harness; it exists so case files can document intent inline.
    #[allow(dead_code)]
    #[serde(default)]
    description: Option<String>,
    /// Files written to the temp directory before invoking the compiler.
    #[serde(default)]
    files: Vec<SourceFile>,
    /// Repo-root-relative source file to compile directly instead of copying
    /// inline source into the temp directory. Use this when a CLI case should
    /// pin a checked-in example/program rather than duplicating its source.
    #[serde(default)]
    source_path: Option<String>,
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
    /// Substrings that must NOT appear in the compiler's stdout.
    #[serde(default)]
    compile_stdout_not_contains: Vec<String>,
    /// Substrings that MUST appear in the compiler's stderr, regardless of
    /// whether compilation succeeds or fails. Use for warnings that must
    /// survive a successful compile (e.g. under `--emit`).
    #[serde(default)]
    compile_stderr_contains: Vec<String>,
    /// Substrings that must NOT appear in the compiler's stderr, regardless of
    /// whether compilation succeeds or fails. Use to guard against debug spew
    /// or leaked internal diagnostics (e.g. raw `DEBUG:` eprintln lines).
    #[serde(default)]
    compile_stderr_not_contains: Vec<String>,
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
    /// Wall-clock timeout in milliseconds for RUNNING the compiled program.
    /// Defaults to [`rue_test_runner::DEFAULT_TIMEOUT_MS`]. On timeout the
    /// program's process group is killed and the case fails as a TIMEOUT.
    #[serde(default)]
    timeout_ms: Option<u64>,
    /// Expected failure: reference to the Linear issue tracking the bug.
    #[serde(default)]
    known_bug: Option<String>,
    /// Platforms the known_bug applies to (e.g. ["x86-64-linux"]). Empty
    /// means all platforms. On other platforms the case runs as a normal
    /// test. Useful for ABI bugs that manifest differently per target.
    #[serde(default)]
    known_bug_on: Vec<String>,
    /// Platforms this case runs on (e.g. ["x86-64-linux"]); elsewhere it is
    /// reported as ignored. Empty means all platforms. Use when the expected
    /// behavior itself depends on the host (e.g. `--target X` is a
    /// cross-compile on some hosts and a native compile on others).
    #[serde(default)]
    only_on: Vec<String>,
    /// Skip this case entirely.
    #[serde(default)]
    skip: bool,
    /// Opt-level differential test (RUE-236): compile+run this case once per
    /// optimization level (`-O0`, `-O1`, `-O2`, `-O3`) and assert IDENTICAL
    /// exit code AND stdout across all levels. A divergence fails the case,
    /// naming the level that differs. This catches optimizer passes that break
    /// semantics — the analogue, at the *program's* opt level, of the
    /// release-mode CI job (RUE-45) that catches `cfg(debug_assertions)`
    /// divergence in the *compiler*. `-O2`/`-O3` alias `-O1` today, so results
    /// match now; the net is set so a future divergence is caught.
    ///
    /// Marked cases must be plain compile-and-run cases: `compile_fail`,
    /// `compile_only`, and an explicit `-O` in `args` are rejected (the runner
    /// drives the opt level itself). Give the case exact `stdout` and
    /// `exit_code` so each level is also checked against the known-good result,
    /// not merely against the other levels.
    #[serde(default)]
    differential_opt: bool,
}

/// What running one case produced: the compiled program's exit code and
/// stdout. For cases that don't run a program (`compile_fail`/`compile_only`),
/// `ran` is false and the other fields are empty. Used by the opt-level
/// differential runner to compare results across `-O` levels.
#[derive(Debug, Default, PartialEq, Eq)]
struct RunOutcome {
    ran: bool,
    exit_code: Option<i32>,
    stdout: String,
}

/// Expand `${REAL_STD}` in env values to the absolute path of the repo's std/.
fn expand_env_value(value: &str, real_std: &Path) -> String {
    value.replace("${REAL_STD}", &real_std.to_string_lossy())
}

fn apply_case_environment(
    command: &mut Command,
    environment: &HashMap<String, String>,
    real_std: &Path,
) {
    for (key, value) in environment {
        command.env(key, expand_env_value(value, real_std));
    }
}

fn case_compiler_command(
    binary: &Path,
    args: &[String],
    directory: &Path,
    environment: &HashMap<String, String>,
    real_std: &Path,
) -> Command {
    let mut command = compiler_command(binary);
    command.args(args).current_dir(directory);
    apply_case_environment(&mut command, environment, real_std);
    command
}

fn find_repo_root(cases_dir: &Path, real_std: &Path) -> PathBuf {
    if let Ok(path) = std::env::var("RUE_REPO_DIR") {
        return PathBuf::from(path);
    }

    let mut candidates = Vec::new();
    if let Ok(path) = cases_dir.canonicalize() {
        candidates.push(path);
    }
    if let Ok(path) = std::env::current_dir() {
        candidates.push(path);
    }
    if let Some(parent) = real_std.parent() {
        candidates.push(parent.to_path_buf());
    }

    for candidate in candidates {
        for ancestor in candidate.ancestors() {
            if ancestor.join("crates/rue-cli-tests/cases").is_dir()
                && ancestor.join("std/_std.rue").is_file()
            {
                return ancestor.to_path_buf();
            }
        }
    }

    PathBuf::from(".")
}

fn resolve_source_path(path: &str, real_std: &Path, repo_root: &Path) -> PathBuf {
    let source_path = Path::new(path);
    if source_path.is_absolute() {
        return source_path.to_path_buf();
    }

    if let Some(rest) = path.strip_prefix("examples/") {
        return find_dir("RUE_EXAMPLES_DIR", EXAMPLES_DIR_PATHS, "examples").join(rest);
    }
    if let Some(rest) = path.strip_prefix("std/") {
        return real_std.join(rest);
    }
    repo_root.join(source_path)
}

/// Compile and run one case, returning the program's outcome.
///
/// When `opt_level` is `Some("-O2")` etc., that flag is appended to the
/// compiler args so the same case can be driven across optimization levels
/// (see [`run_case_differential`]); when `None`, args are used verbatim.
fn run_case(
    case: &Case,
    rue_binary: &Path,
    real_std: &Path,
    repo_root: &Path,
    opt_level: Option<&str>,
) -> TestResult<RunOutcome> {
    let temp_dir = tempfile::tempdir()
        .map_err(|e| TestFailure::fatal(format!("failed to create temp dir: {}", e)))?;
    let dir = temp_dir.path();
    let source_path = case
        .source_path
        .as_ref()
        .map(|path| resolve_source_path(path, real_std, repo_root));

    // Write the case's files to disk, creating subdirectories as needed.
    for file in &case.files {
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                TestFailure::fatal(format!("failed to create dir for {}: {}", file.path, e))
            })?;
        }
        std::fs::write(&path, &file.source)
            .map_err(|e| TestFailure::fatal(format!("failed to write {}: {}", file.path, e)))?;
    }

    let output_name = case.output.clone().unwrap_or_else(|| "prog".to_string());

    // Default invocation mirrors what a user types: `rue main.rue -o prog`,
    // with RELATIVE paths and the temp dir as the working directory.
    let mut args: Vec<String> = match &case.args {
        Some(args) => args.clone(),
        None => {
            let first = match &source_path {
                Some(path) => path.display().to_string(),
                None => case
                    .files
                    .first()
                    .ok_or_else(|| TestFailure::assertion("case has no files"))?
                    .path
                    .clone(),
            };
            vec![first, "-o".to_string(), output_name.clone()]
        }
    };
    // For an opt-level differential run, append the level flag. Flag position
    // is irrelevant to the CLI, so this composes with either default or
    // explicit args (differential cases are validated to not carry their own
    // `-O`, so there's no conflict).
    if let Some(level) = opt_level {
        args.push(level.to_string());
    }

    let cmd = case_compiler_command(rue_binary, &args, dir, &case.env, real_std);
    // The COMPILE step runs under the same per-case timeout as execution: a
    // compile-time hang (comptime/parser loop) must fail this one case as a
    // TIMEOUT, not wedge the suite. Mirrors the spec runner's compile step.
    let compile_timeout = Duration::from_millis(case.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let compile_output = run_with_timeout(cmd, compile_timeout, None)?;

    let compile_stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
    let compile_stdout = String::from_utf8_lossy(&compile_output.stdout).to_string();

    // ICE detection comes first: a compiler panic is never acceptable output,
    // even for compile_fail cases.
    if let Some(ice) = ice_message(&compile_output.status, &compile_stderr) {
        return Err(ice);
    }

    // Debug-spew / leaked-diagnostics guard runs regardless of compile outcome.
    for expected in &case.compile_stderr_contains {
        if !compile_stderr.contains(expected) {
            return Err(TestFailure::assertion(format!(
                "compiler stderr missing expected substring: {}\n--- actual stderr ---\n{}",
                expected, compile_stderr
            )));
        }
    }

    for forbidden in &case.compile_stderr_not_contains {
        if compile_stderr.contains(forbidden) {
            return Err(TestFailure::assertion(format!(
                "compiler stderr contained forbidden substring: {}\n--- actual stderr ---\n{}",
                forbidden, compile_stderr
            )));
        }
    }

    for expected in &case.compile_stdout_contains {
        if !compile_stdout.contains(expected) {
            return Err(TestFailure::assertion(format!(
                "compiler stdout mismatch:\n  expected to contain: {}\n--- actual stdout ---\n{}",
                expected, compile_stdout
            )));
        }
    }

    for forbidden in &case.compile_stdout_not_contains {
        if compile_stdout.contains(forbidden) {
            return Err(TestFailure::assertion(format!(
                "compiler stdout contained forbidden substring: {}\n--- actual stdout ---\n{}",
                forbidden, compile_stdout
            )));
        }
    }

    let compile_succeeded = compile_output.status.success();

    if case.compile_fail {
        if compile_succeeded {
            return Err(TestFailure::assertion(
                "expected compilation to fail, but it succeeded",
            ));
        }
        for expected in &case.error_contains {
            if !compile_stderr.contains(expected) {
                return Err(TestFailure::assertion(format!(
                    "compiler error mismatch:\n  expected stderr to contain: {}\n--- actual stderr ---\n{}",
                    expected, compile_stderr
                )));
            }
        }
        return Ok(RunOutcome::default());
    }

    if !compile_succeeded {
        return Err(TestFailure::assertion(format!(
            "compilation failed (exit: {:?}):\n--- compiler stdout ---\n{}\n--- compiler stderr ---\n{}",
            compile_output.status.code(),
            compile_stdout,
            compile_stderr
        )));
    }

    if case.compile_only {
        return Ok(RunOutcome::default());
    }

    // Run the produced binary from the temp dir.
    let program = dir.join(&output_name);
    if !program.exists() {
        return Err(TestFailure::assertion(format!(
            "compiler reported success but did not produce '{}'",
            output_name
        )));
    }

    // Run the produced binary under a per-case wall-clock timeout. An infinite
    // loop in generated code must fail this one case (as a distinct TIMEOUT
    // class), not hang the whole suite. `run_with_timeout` puts the program in
    // its own process group and kills the group on timeout.
    let mut run_cmd = Command::new(&program);
    run_cmd.current_dir(dir);
    let timeout = Duration::from_millis(case.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let run_output = run_with_timeout(run_cmd, timeout, case.stdin.as_deref())?;

    let run_stdout = String::from_utf8_lossy(&run_output.stdout).to_string();
    let run_stderr = String::from_utf8_lossy(&run_output.stderr).to_string();

    if run_output.status.code().is_none() {
        return Err(TestFailure::fatal(format!(
            "TEST PROGRAM CRASH: process killed by signal ({:?})\n--- program stderr ---\n{}",
            run_output.status, run_stderr
        )));
    }

    let expected_exit = case.exit_code.unwrap_or(0);
    let actual_exit = run_output.status.code();
    if actual_exit != Some(expected_exit) {
        return Err(TestFailure::assertion(format!(
            "program exit code mismatch:\n  expected: {}\n  actual: {:?}\n--- program stdout ---\n{}\n--- program stderr ---\n{}",
            expected_exit, actual_exit, run_stdout, run_stderr
        )));
    }

    if let Some(expected) = &case.stdout {
        if &run_stdout != expected {
            return Err(TestFailure::assertion(format!(
                "program stdout mismatch:\n--- expected ---\n{}\n--- actual ---\n{}",
                expected, run_stdout
            )));
        }
    }

    for expected in &case.stdout_contains {
        if !run_stdout.contains(expected) {
            return Err(TestFailure::assertion(format!(
                "program stdout mismatch:\n  expected to contain: {}\n--- actual stdout ---\n{}",
                expected, run_stdout
            )));
        }
    }

    for expected in &case.runtime_error_contains {
        if !run_stderr.contains(expected) {
            return Err(TestFailure::assertion(format!(
                "program stderr mismatch:\n  expected to contain: {}\n--- actual stderr ---\n{}",
                expected, run_stderr
            )));
        }
    }

    Ok(RunOutcome {
        ran: true,
        exit_code: actual_exit,
        stdout: run_stdout,
    })
}

/// Opt-level differential runner (RUE-236): compile+run a marked case at every
/// optimization level and assert identical exit code AND stdout across all of
/// them, so an optimizer pass that miscompiles is caught.
///
/// Each level runs through the full [`run_case`] machinery, so its declared
/// `exit_code`/`stdout` are checked at *every* level (a correctness anchor),
/// and then the levels' outcomes are cross-checked against the first level so a
/// divergence that still matches no declared value can't slip through. On a
/// mismatch the error names the diverging level.
fn run_case_differential(
    case: &Case,
    rue_binary: &Path,
    real_std: &Path,
    repo_root: &Path,
) -> TestResult {
    // Levels to compare. -O2/-O3 alias -O1 today; the net catches a future
    // divergence.
    const OPT_LEVELS: &[&str] = &["-O0", "-O1", "-O2", "-O3"];

    // Guard against misuse: the runner drives the opt level, so these would be
    // ambiguous or meaningless.
    if case.compile_fail || case.compile_only {
        return Err(TestFailure::assertion(
            "differential_opt case must be a compile-and-run case (not compile_fail/compile_only)",
        ));
    }
    if let Some(args) = &case.args {
        if args.iter().any(|a| a.starts_with("-O")) {
            return Err(TestFailure::assertion(
                "differential_opt case must not set its own -O flag in args (the runner drives it)",
            ));
        }
    }

    let mut baseline: Option<(&str, RunOutcome)> = None;
    for level in OPT_LEVELS {
        let outcome = run_case(case, rue_binary, real_std, repo_root, Some(level))
            .map_err(|error| error.with_context(format!("at {level}")))?;
        match &baseline {
            None => baseline = Some((level, outcome)),
            Some((base_level, base)) => {
                if &outcome != base {
                    return Err(TestFailure::assertion(format!(
                        "opt-level divergence: {} produced (exit={:?}, stdout={:?}) but {} produced (exit={:?}, stdout={:?})",
                        base_level,
                        base.exit_code,
                        base.stdout,
                        level,
                        outcome.exit_code,
                        outcome.stdout
                    )));
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum KnownBugDisposition {
    Ignore(String),
    Fail(String),
}

fn known_bug_disposition(bug: &str, result: TestResult) -> KnownBugDisposition {
    match classify_expected_failure(result) {
        ExpectedFailureOutcome::ExpectedFailure(error) => {
            let first_line = error.lines().next().unwrap_or("");
            KnownBugDisposition::Ignore(format!(
                "known bug {} (expected failure): {}",
                bug, first_line
            ))
        }
        ExpectedFailureOutcome::FatalFailure(error) => KnownBugDisposition::Fail(error.to_string()),
        ExpectedFailureOutcome::UnexpectedPass => KnownBugDisposition::Fail(format!(
            "test PASSED but is marked known_bug = \"{}\". If the bug is fixed, \
             remove the known_bug marker so this becomes a regression test.",
            bug
        )),
    }
}

/// Wrapper handling skip and known_bug (xfail) semantics.
fn run_case_wrapper(
    case: &Case,
    rue_binary: &Path,
    real_std: &Path,
    repo_root: &Path,
    ctx: RunContext<'_>,
) -> Result<(), RunError> {
    if case.skip {
        return ctx.ignore_for("marked as skip");
    }

    // Host-dependent cases: only run on the listed platforms.
    if !case.only_on.is_empty()
        && !case
            .only_on
            .iter()
            .any(|p| p == rue_test_runner::get_host_target())
    {
        return ctx.ignore_for(format!(
            "not applicable on this host (only_on = {:?})",
            case.only_on
        ));
    }

    let result = if case.differential_opt {
        run_case_differential(case, rue_binary, real_std, repo_root)
    } else {
        run_case(case, rue_binary, real_std, repo_root, None).map(|_| ())
    };

    // A known_bug scoped to other platforms doesn't apply here: the case
    // runs as a normal test on this host.
    let bug_applies_here = case.known_bug.is_some()
        && (case.known_bug_on.is_empty()
            || case
                .known_bug_on
                .iter()
                .any(|p| p == rue_test_runner::get_host_target()));
    let known_bug = if bug_applies_here {
        &case.known_bug
    } else {
        &None
    };

    match (known_bug, result) {
        // Normal case.
        (None, Ok(())) => Ok(()),
        (None, Err(error)) => Err(RunError::fail(error.to_string())),
        (Some(bug), result) => match known_bug_disposition(bug, result) {
            KnownBugDisposition::Ignore(reason) => ctx.ignore_for(reason),
            KnownBugDisposition::Fail(reason) => Err(RunError::fail(reason)),
        },
    }
}

/// A `differential_opt` case's cross-level check is only meaningful if each opt
/// level is also pinned to a known-good result, so it must declare both an
/// explicit `stdout` and `exit_code`. Returns true when the case is
/// `differential_opt` but missing either — a load-time error (RUE-132).
fn differential_opt_missing_pin(case: &Case) -> bool {
    case.differential_opt && (case.stdout.is_none() || case.exit_code.is_none())
}

/// A `compile_fail` case that pins nothing about *why* compilation fails passes
/// on ANY rejection — a diagnostic for an unrelated reason, or (before ICE
/// detection) even a compiler crash. Require it to assert on the compiler's
/// output via `error_contains` or `compile_stderr_contains`, so it verifies the
/// specific error it is testing (RUE-132). Returns true when the case is
/// `compile_fail` but carries neither assertion — a load-time error.
fn compile_fail_missing_assertion(case: &Case) -> bool {
    case.compile_fail && case.error_contains.is_empty() && case.compile_stderr_contains.is_empty()
}

fn compile_fail_has_exit_code(case: &Case) -> bool {
    case.compile_fail && case.exit_code.is_some()
}

fn unknown_only_on_targets(case: &Case) -> Vec<&str> {
    case.only_on
        .iter()
        .map(String::as_str)
        .filter(|platform| !KNOWN_TARGETS.contains(platform))
        .collect()
}

fn load_cases(cases_dir: &Path) -> Vec<(String, TestFile)> {
    let toml_files = rue_test_runner::discover_files(cases_dir, "toml").unwrap_or_else(|error| {
        eprintln!(
            "error: failed to discover CLI test files under {}: {error}",
            cases_dir.display()
        );
        std::process::exit(1);
    });

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
            Ok(tf) => {
                // A differential_opt case's cross-level check is trivially green
                // today (-O2/-O3 alias -O1), so it only catches a *consistently*
                // wrong program if each level is also pinned to a known-good
                // result. Require both an explicit `stdout` and `exit_code` at
                // load time so the doc comment's promise is enforced, not merely
                // documented (RUE-132).
                for case in &tf.cases {
                    let unknown_platforms = unknown_only_on_targets(case);
                    if !unknown_platforms.is_empty() {
                        eprintln!(
                            "error: {}: case '{}' has unknown only_on platform(s): {} (known: {})",
                            path.display(),
                            case.name,
                            unknown_platforms.join(", "),
                            KNOWN_TARGETS.join(", ")
                        );
                        std::process::exit(1);
                    }
                    if compile_fail_has_exit_code(case) {
                        eprintln!(
                            concat!(
                                "error: {}: compile_fail case '{}' also declares `exit_code`, ",
                                "but no program runs for a compile failure. Remove `exit_code`."
                            ),
                            path.display(),
                            case.name
                        );
                        std::process::exit(1);
                    }
                    if differential_opt_missing_pin(case) {
                        eprintln!(
                            "error: {}: differential_opt case '{}' must declare both an explicit \
                             `stdout` and `exit_code` (each opt level is checked against them, so \
                             the cross-level compare can't pass a consistently-wrong program)",
                            path.display(),
                            case.name
                        );
                        std::process::exit(1);
                    }
                    // A compile_fail case with no error assertion passes on any
                    // rejection, verifying nothing about why it failed (RUE-132).
                    if compile_fail_missing_assertion(case) {
                        eprintln!(
                            "error: {}: compile_fail case '{}' declares neither `error_contains` \
                             nor `compile_stderr_contains`, so it would pass on ANY rejection. \
                             Add an assertion pinning the specific error.",
                            path.display(),
                            case.name
                        );
                        std::process::exit(1);
                    }
                }
                out.push((path.display().to_string(), tf));
            }
            Err(e) => {
                eprintln!("error: failed to parse {}: {}", path.display(), e);
                std::process::exit(1);
            }
        }
    }
    out
}

/// Compile and run one `examples/**/*.rue` program end to end (RUE-48).
///
/// The real example file is compiled with the real driver (`rue <file> -o
/// prog`). The produced binary lives in a temp dir, but the compiler sees the
/// source at its repository path, so relative imports are exercised exactly as
/// they are written. A compiler panic is an ICE; a compile failure fails the
/// case. The produced binary is then run under the standard wall-clock timeout,
/// and — crucially — must exit *normally*: a program killed by a signal
/// (SIGSEGV/SIGABRT show up as a `None` exit code on unix) fails as a crash. If
/// the example is in [`EXAMPLE_EXPECTATIONS`], its exact exit code and stdout
/// are asserted; otherwise passing this "no crash" bar is enough
/// (self-maintaining smoke coverage for newly added examples).
fn run_example(
    path: &Path,
    expectation: Option<&ExampleExpectation>,
    rue_binary: &Path,
    real_std: &Path,
) -> TestResult {
    let temp_dir = tempfile::tempdir()
        .map_err(|e| TestFailure::fatal(format!("failed to create temp dir: {}", e)))?;
    let dir = temp_dir.path();

    let mut cmd = compiler_command(rue_binary);
    cmd.arg(path).args(["-o", "prog"]).current_dir(dir);
    cmd.env("RUE_STD_PATH", real_std);
    // Compile under the default timeout too (see run_case): an example that
    // hangs the compiler fails as one TIMEOUT, not a wedged suite.
    let compile_output = run_with_timeout(cmd, Duration::from_millis(DEFAULT_TIMEOUT_MS), None)?;
    let compile_stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
    let compile_stdout = String::from_utf8_lossy(&compile_output.stdout).to_string();

    if let Some(ice) = ice_message(&compile_output.status, &compile_stderr) {
        return Err(ice);
    }
    if !compile_output.status.success() {
        return Err(TestFailure::assertion(format!(
            "example failed to compile (exit: {:?}):\n--- compiler stdout ---\n{}\n--- compiler stderr ---\n{}",
            compile_output.status.code(),
            compile_stdout,
            compile_stderr
        )));
    }

    let program = dir.join("prog");
    if !program.exists() {
        return Err(TestFailure::assertion(
            "compiler reported success but produced no 'prog' binary",
        ));
    }

    let mut run_cmd = Command::new(&program);
    run_cmd.current_dir(dir);
    let run_output = run_with_timeout(
        run_cmd,
        Duration::from_millis(DEFAULT_TIMEOUT_MS),
        expectation.and_then(|exp| exp.stdin),
    )?;
    let run_stdout = String::from_utf8_lossy(&run_output.stdout).to_string();
    let run_stderr = String::from_utf8_lossy(&run_output.stderr).to_string();

    // "Runs without crashing": a normal exit yields Some(code); death by
    // signal (SIGSEGV/SIGABRT) yields None on unix.
    let actual_exit = match run_output.status.code() {
        Some(code) => code,
        None => {
            return Err(TestFailure::fatal(format!(
                "example crashed (killed by signal, status {:?})\n--- program stdout ---\n{}\n--- program stderr ---\n{}",
                run_output.status, run_stdout, run_stderr
            )));
        }
    };

    if let Some(exp) = expectation {
        if actual_exit != exp.exit_code {
            return Err(TestFailure::assertion(format!(
                "example exit code mismatch:\n  expected: {}\n  actual: {}\n--- program stdout ---\n{}\n--- program stderr ---\n{}",
                exp.exit_code, actual_exit, run_stdout, run_stderr
            )));
        }
        if run_stdout != exp.stdout {
            return Err(TestFailure::assertion(format!(
                "example stdout mismatch:\n--- expected ---\n{}\n--- actual ---\n{}",
                exp.stdout, run_stdout
            )));
        }
    }

    Ok(())
}

fn collect_example_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    // A directory that directly contains a `main.rue` is ONE root-module
    // example (RUE-424): its other `.rue` files are modules reached through
    // `@import` from that root, not standalone programs — compiling them
    // alone would fail on the missing `main`. Emit the root and stop
    // recursing into the program's subtree.
    let root = dir.join("main.rue");
    if root.is_file() {
        out.push(root);
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read examples directory '{}': {}", dir.display(), e))?
    {
        let entry = entry.map_err(|e| format!("cannot read examples entry: {}", e))?;
        let path = entry.path();
        if path.is_dir() {
            collect_example_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rue") {
            out.push(path);
        }
    }
    Ok(())
}

fn example_relative_path(examples_dir: &Path, path: &Path) -> String {
    path.strip_prefix(examples_dir)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn example_test_name(relative_path: &str) -> String {
    let name = relative_path
        .strip_suffix(".rue")
        .unwrap_or(relative_path)
        .replace('/', "::");
    format!("cli.examples::{name}")
}

/// Discover `examples/**/*.rue` and build one smoke-test trial per file (RUE-48).
///
/// This is self-maintaining: it enumerates the directory at run time, so a new
/// example is picked up automatically. If the directory can't be found or holds
/// no `.rue` files, a single loud failing trial is emitted rather than silently
/// running zero example tests (which would let example rot slip through CI).
fn example_trials(rue_binary: &Path, real_std: &Path) -> Vec<Trial> {
    let examples_dir = find_dir("RUE_EXAMPLES_DIR", EXAMPLES_DIR_PATHS, "examples");

    let mut example_files = Vec::new();
    match collect_example_files(&examples_dir, &mut example_files) {
        Ok(()) => {}
        Err(e) => {
            let msg = format!(
                "{} (set RUE_EXAMPLES_DIR to override '{}')",
                e,
                examples_dir.display(),
            );
            return vec![Trial::test("cli.examples::_discovery", move |_ctx| {
                Err(RunError::fail(msg.clone()))
            })];
        }
    };
    example_files.sort();

    if example_files.is_empty() {
        let msg = format!(
            "no *.rue examples found in '{}' — RUE-48 smoke coverage is empty",
            examples_dir.display()
        );
        return vec![Trial::test("cli.examples::_discovery", move |_ctx| {
            Err(RunError::fail(msg.clone()))
        })];
    }

    let mut trials = Vec::with_capacity(example_files.len());
    for path in example_files {
        let relative_path = example_relative_path(&examples_dir, &path);
        let rue_binary = rue_binary.to_path_buf();
        let real_std = real_std.to_path_buf();
        let test_name = example_test_name(&relative_path);
        trials.push(Trial::test(test_name, move |_ctx| {
            let expectation = EXAMPLE_EXPECTATIONS
                .iter()
                .find(|e| e.path == relative_path);
            run_example(&path, expectation, &rue_binary, &real_std).map_err(RunError::fail)
        }));
    }
    trials
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
    let repo_root = find_repo_root(&cases_dir, &real_std);

    let files = load_cases(&cases_dir);

    let total: usize = files.iter().map(|(_, f)| f.cases.len()).sum();
    if let Err(error) = validate_nonempty_case_corpus(&cases_dir, total, "CLI") {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
    let mut tests: Vec<Trial> = Vec::with_capacity(total);

    for (_, file) in files {
        let section_id = file.section.id.clone();
        for case in file.cases {
            let test_name = format!("{}::{}", section_id, case.name);
            let rue_binary = rue_binary.clone();
            let real_std = real_std.clone();
            let repo_root = repo_root.clone();
            tests.push(Trial::test(test_name, move |ctx| {
                run_case_wrapper(&case, &rue_binary, &real_std, &repo_root, ctx)
            }));
        }
    }

    // RUE-48: compile+run every examples/**/*.rue through the real driver, so a
    // regression that breaks a shipped example (or an example referencing a
    // removed flag) can't slip past CI unnoticed.
    tests.extend(example_trials(&rue_binary, &real_std));

    Harness::with_env().discover(tests).main();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_compiler(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary fake compiler directory");
        let binary = directory.path().join("rue");
        std::fs::write(&binary, script).expect("write fake compiler");
        let mut permissions = std::fs::metadata(&binary)
            .expect("fake compiler metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).expect("make fake compiler executable");
        (directory, binary)
    }

    #[test]
    fn test_welcome_example_is_zero_exit() {
        // RUE-517: the onboarding docs (README, CONTRIBUTING, and the tutorial's
        // installation page) use `scripts/rue exec examples/welcome.rue` as the
        // "did my setup work?" health check. Because `exec` propagates the
        // program's exit code, welcome MUST exit 0 and print recognizable output
        // — otherwise a successful setup looks like a failure under `set -e`,
        // `&&` chains, or CI. Pin the invariant so the canonical onboarding
        // example can't silently regress to a nonzero exit.
        let welcome = EXAMPLE_EXPECTATIONS
            .iter()
            .find(|e| e.path == "welcome.rue")
            .expect("welcome.rue must be a pinned example expectation");
        assert_eq!(
            welcome.exit_code, 0,
            "the onboarding example must exit 0 (RUE-517)"
        );
        assert!(
            !welcome.stdout.is_empty(),
            "the onboarding example must print recognizable output (RUE-517)"
        );
    }

    #[test]
    fn differential_opt_requires_stdout_and_exit_code() {
        // Missing both -> rejected.
        let mut case = Case {
            name: "c".to_string(),
            differential_opt: true,
            ..Default::default()
        };
        assert!(differential_opt_missing_pin(&case));

        // Only stdout -> still rejected (exit_code missing).
        case.stdout = Some("59\n".to_string());
        assert!(differential_opt_missing_pin(&case));

        // Only exit_code -> still rejected (stdout missing).
        case.stdout = None;
        case.exit_code = Some(59);
        assert!(differential_opt_missing_pin(&case));

        // Both present -> accepted.
        case.stdout = Some("59\n".to_string());
        assert!(!differential_opt_missing_pin(&case));
    }

    #[test]
    fn non_differential_case_not_required_to_pin() {
        // A plain case missing stdout/exit_code is fine — the requirement only
        // applies to differential_opt cases.
        let case = Case {
            name: "c".to_string(),
            differential_opt: false,
            ..Default::default()
        };
        assert!(!differential_opt_missing_pin(&case));
    }

    #[test]
    fn compile_fail_case_must_pin_an_error() {
        // Bare compile_fail -> rejected.
        let mut case = Case {
            name: "c".to_string(),
            compile_fail: true,
            ..Default::default()
        };
        assert!(compile_fail_missing_assertion(&case));

        // error_contains satisfies the guard.
        case.error_contains = vec!["[E0206]".to_string()];
        assert!(!compile_fail_missing_assertion(&case));

        // compile_stderr_contains also satisfies it (pins stderr content).
        case.error_contains = vec![];
        case.compile_stderr_contains = vec!["bad.rue:".to_string()];
        assert!(!compile_fail_missing_assertion(&case));
    }

    #[test]
    fn non_compile_fail_case_not_required_to_pin_error() {
        // A compile-and-run case needs no compile-error assertion.
        let case = Case {
            name: "c".to_string(),
            compile_fail: false,
            ..Default::default()
        };
        assert!(!compile_fail_missing_assertion(&case));
    }

    #[test]
    fn unknown_only_on_target_is_rejected() {
        let case = Case {
            name: "platform_typo".to_string(),
            only_on: vec!["x86_64-linux".to_string()],
            ..Default::default()
        };

        assert_eq!(unknown_only_on_targets(&case), vec!["x86_64-linux"]);
    }

    #[test]
    fn known_only_on_targets_are_accepted() {
        let case = Case {
            name: "known_platforms".to_string(),
            only_on: KNOWN_TARGETS
                .iter()
                .map(|target| (*target).to_string())
                .collect(),
            ..Default::default()
        };

        assert!(unknown_only_on_targets(&case).is_empty());
    }

    #[test]
    fn compile_fail_case_rejects_runtime_exit_code() {
        let case = Case {
            name: "ignored_exit".to_string(),
            compile_fail: true,
            exit_code: Some(1),
            ..Default::default()
        };

        assert!(compile_fail_has_exit_code(&case));
    }

    #[test]
    fn case_compiler_command_applies_explicit_environment_after_sanitizing() {
        let real_std = Path::new("/repo/std");
        let environment = HashMap::from([
            ("RUE_STD_PATH".to_string(), "${REAL_STD}".to_string()),
            ("RUST_LOG".to_string(), "trace".to_string()),
        ]);
        let command = case_compiler_command(
            Path::new("rue"),
            &["main.rue".to_string()],
            Path::new("/case"),
            &environment,
            real_std,
        );
        let environments: HashMap<_, _> = command.get_envs().collect();

        assert_eq!(
            environments.get(std::ffi::OsStr::new("RUE_STD_PATH")),
            Some(&Some(std::ffi::OsStr::new("/repo/std")))
        );
        assert_eq!(
            environments.get(std::ffi::OsStr::new("RUST_LOG")),
            Some(&Some(std::ffi::OsStr::new("trace")))
        );
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["main.rue"]);
        assert_eq!(command.get_current_dir(), Some(Path::new("/case")));
    }

    #[cfg(unix)]
    #[test]
    fn known_bug_cannot_absorb_fake_compiler_panic() {
        let (_directory, binary) =
            fake_compiler("#!/bin/sh\nprintf 'panicked at fake CLI compiler' >&2\nexit 101\n");
        let case = Case {
            name: "known_bug_panic".to_string(),
            files: vec![SourceFile {
                path: "main.rue".to_string(),
                source: "fn main() -> i32 { 0 }".to_string(),
            }],
            known_bug: Some("RUE-PROBE".to_string()),
            differential_opt: true,
            stdout: Some(String::new()),
            exit_code: Some(0),
            ..Default::default()
        };

        let result = run_case_differential(&case, &binary, Path::new("std"), Path::new("."));
        let error = result.expect_err("compiler panic must fail an xfail");
        assert!(error.is_fatal());
        assert!(error.contains("at -O0"));
        assert!(matches!(
            known_bug_disposition("RUE-PROBE", Err(error)),
            KnownBugDisposition::Fail(message) if message.contains("INTERNAL COMPILER ERROR")
        ));
    }

    #[test]
    fn known_bug_disposition_ignores_only_ordinary_failures_and_rejects_xpass() {
        assert!(matches!(
            known_bug_disposition(
                "RUE-PROBE",
                Err(TestFailure::assertion("wrong output\nmore detail"))
            ),
            KnownBugDisposition::Ignore(message)
                if message == "known bug RUE-PROBE (expected failure): wrong output"
        ));
        assert!(matches!(
            known_bug_disposition("RUE-PROBE", Ok(())),
            KnownBugDisposition::Fail(message) if message.contains("test PASSED")
        ));
    }
}
