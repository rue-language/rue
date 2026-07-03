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
//! - `timeout_ms`: wall-clock limit for running the program (default 10s)
//! - `known_bug = "RUE-NN"`: expected failure (xfail). The case runs; if it
//!   fails, it is reported as ignored with the bug reference. If it PASSES,
//!   the suite fails loudly so the marker gets removed when the bug is fixed.
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
//! failure class, regardless of what the case expected (it still counts as
//! "failure" for `known_bug` purposes).
//!
//! # Timeouts
//!
//! The compiled program is run under a per-case wall-clock timeout (default
//! [`rue_test_runner::DEFAULT_TIMEOUT_MS`], overridable per case with
//! `timeout_ms`). If it runs long — e.g. an infinite loop in generated code —
//! its whole process group is killed and the case is reported as a distinct
//! TIMEOUT failure (see [`rue_test_runner::TIMEOUT_PREFIX`]), so one bad
//! program can never hang the suite. `compile_only = true` still skips the run
//! entirely for sources that are meant only to compile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use libtest2_mimic::{Harness, RunContext, RunError, Trial};
use rue_test_runner::{DEFAULT_TIMEOUT_MS, find_dir, find_rue_binary, run_with_timeout};
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

/// Expected outcome of compiling and running one `examples/*.rue` program.
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
    /// File stem: the basename without the `.rue` extension.
    name: &'static str,
    /// Expected process exit code.
    exit_code: i32,
    /// Exact expected stdout.
    stdout: &'static str,
}

const EXAMPLE_EXPECTATIONS: &[ExampleExpectation] = &[
    ExampleExpectation {
        name: "arrays",
        exit_code: 157,
        stdout: "157\n64\n12\n60\n",
    },
    ExampleExpectation {
        name: "binary_search",
        exit_code: 4,
        stdout: "4\n",
    },
    ExampleExpectation {
        name: "collatz",
        exit_code: 97,
        stdout: "27\n111\n",
    },
    ExampleExpectation {
        name: "dbg",
        exit_code: 0,
        stdout: "42\n-17\ntrue\nfalse\n70\ntrue\ntrue\n120\n0\n1\n2\n3\n4\n",
    },
    ExampleExpectation {
        name: "fibonacci",
        exit_code: 55,
        stdout: "0\n1\n1\n2\n3\n5\n8\n13\n21\n34\n55\n89\n144\n233\n377\n610\n987\n1597\n2584\n4181\n",
    },
    ExampleExpectation {
        name: "fizzbuzz",
        exit_code: 0,
        stdout: "1\n2\n1\n4\n2\n1\n7\n8\n1\n2\n11\n1\n13\n14\n3\n16\n17\n1\n19\n2\n1\n22\n23\n1\n2\n26\n1\n28\n29\n3\n",
    },
    ExampleExpectation {
        name: "gcd",
        exit_code: 21,
        stdout: "6\n1\n36\n",
    },
    ExampleExpectation {
        name: "generics",
        exit_code: 72,
        stdout: "42\n20\n10\n100\n8\n17\n",
    },
    ExampleExpectation {
        name: "hello",
        exit_code: 42,
        stdout: "",
    },
    ExampleExpectation {
        name: "match",
        exit_code: 5,
        stdout: "5\n",
    },
    ExampleExpectation {
        name: "power",
        exit_code: 9,
        stdout: "1\n2\n1024\n243\n2401\n1024\n",
    },
    ExampleExpectation {
        name: "primes",
        exit_code: 25,
        stdout: "2\n3\n5\n7\n11\n13\n17\n19\n23\n29\n31\n37\n41\n43\n47\n",
    },
    ExampleExpectation {
        name: "quicksort",
        exit_code: 11,
        stdout: "0\n64\n34\n25\n12\n22\n11\n90\n42\n15\n77\n1\n11\n12\n15\n22\n25\n34\n42\n64\n77\n90\n",
    },
    ExampleExpectation {
        name: "sqrt",
        exit_code: 12,
        stdout: "0\n1\n2\n2\n3\n3\n4\n10\n31\n",
    },
    ExampleExpectation {
        name: "structs",
        exit_code: 50,
        stdout: "25\n50\n30\ntrue\n",
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    /// Human-readable explanation of what this case pins and why. Not used by
    /// the harness; it exists so case files can document intent inline.
    #[allow(dead_code)]
    #[serde(default)]
    description: Option<String>,
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

type TestResult = Result<(), String>;

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

/// Compile and run one case, returning the program's outcome.
///
/// When `opt_level` is `Some("-O2")` etc., that flag is appended to the
/// compiler args so the same case can be driven across optimization levels
/// (see [`run_case_differential`]); when `None`, args are used verbatim.
fn run_case(
    case: &Case,
    rue_binary: &Path,
    real_std: &Path,
    opt_level: Option<&str>,
) -> Result<RunOutcome, String> {
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
    let mut args: Vec<String> = match &case.args {
        Some(args) => args.clone(),
        None => {
            let first = case.files.first().ok_or("case has no files")?.path.clone();
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

    // Debug-spew / leaked-diagnostics guard runs regardless of compile outcome.
    for expected in &case.compile_stderr_contains {
        if !compile_stderr.contains(expected) {
            return Err(format!(
                "compiler stderr missing expected substring: {}\n--- actual stderr ---\n{}",
                expected, compile_stderr
            ));
        }
    }

    for forbidden in &case.compile_stderr_not_contains {
        if compile_stderr.contains(forbidden) {
            return Err(format!(
                "compiler stderr contained forbidden substring: {}\n--- actual stderr ---\n{}",
                forbidden, compile_stderr
            ));
        }
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
        return Ok(RunOutcome::default());
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
        return Ok(RunOutcome::default());
    }

    // Run the produced binary from the temp dir.
    let program = dir.join(&output_name);
    if !program.exists() {
        return Err(format!(
            "compiler reported success but did not produce '{}'",
            output_name
        ));
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
fn run_case_differential(case: &Case, rue_binary: &Path, real_std: &Path) -> TestResult {
    // Levels to compare. -O2/-O3 alias -O1 today; the net catches a future
    // divergence.
    const OPT_LEVELS: &[&str] = &["-O0", "-O1", "-O2", "-O3"];

    // Guard against misuse: the runner drives the opt level, so these would be
    // ambiguous or meaningless.
    if case.compile_fail || case.compile_only {
        return Err(
            "differential_opt case must be a compile-and-run case (not compile_fail/compile_only)"
                .to_string(),
        );
    }
    if let Some(args) = &case.args {
        if args.iter().any(|a| a.starts_with("-O")) {
            return Err(
                "differential_opt case must not set its own -O flag in args (the runner drives it)"
                    .to_string(),
            );
        }
    }

    let mut baseline: Option<(&str, RunOutcome)> = None;
    for level in OPT_LEVELS {
        let outcome = run_case(case, rue_binary, real_std, Some(level))
            .map_err(|e| format!("at {}: {}", level, e))?;
        match &baseline {
            None => baseline = Some((level, outcome)),
            Some((base_level, base)) => {
                if &outcome != base {
                    return Err(format!(
                        "opt-level divergence: {} produced (exit={:?}, stdout={:?}) but {} produced (exit={:?}, stdout={:?})",
                        base_level,
                        base.exit_code,
                        base.stdout,
                        level,
                        outcome.exit_code,
                        outcome.stdout
                    ));
                }
            }
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
        run_case_differential(case, rue_binary, real_std)
    } else {
        run_case(case, rue_binary, real_std, None).map(|_| ())
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

/// Compile and run one `examples/*.rue` program end to end (RUE-48).
///
/// The example is written to a temp dir and compiled with the real driver
/// (`rue <file> -o prog`), exactly as a user would. A compiler panic is an ICE;
/// a compile failure fails the case. The produced binary is then run under the
/// standard wall-clock timeout, and — crucially — must exit *normally*: a
/// program killed by a signal (SIGSEGV/SIGABRT show up as a `None` exit code on
/// unix) fails as a crash. If the example is in [`EXAMPLE_EXPECTATIONS`], its
/// exact exit code and stdout are asserted; otherwise passing this "no crash"
/// bar is enough (self-maintaining smoke coverage for newly added examples).
fn run_example(
    name: &str,
    source: &str,
    expectation: Option<&ExampleExpectation>,
    rue_binary: &Path,
) -> TestResult {
    let temp_dir = tempfile::tempdir().map_err(|e| format!("failed to create temp dir: {}", e))?;
    let dir = temp_dir.path();
    let src_name = format!("{}.rue", name);
    std::fs::write(dir.join(&src_name), source)
        .map_err(|e| format!("failed to write {}: {}", src_name, e))?;

    let mut cmd = Command::new(rue_binary);
    cmd.args([src_name.as_str(), "-o", "prog"]).current_dir(dir);
    let compile_output = cmd
        .output()
        .map_err(|e| format!("failed to invoke rue compiler: {}", e))?;
    let compile_stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
    let compile_stdout = String::from_utf8_lossy(&compile_output.stdout).to_string();

    if let Some(ice) = ice_message(&compile_output.status, &compile_stderr) {
        return Err(ice);
    }
    if !compile_output.status.success() {
        return Err(format!(
            "example failed to compile (exit: {:?}):\n--- compiler stdout ---\n{}\n--- compiler stderr ---\n{}",
            compile_output.status.code(),
            compile_stdout,
            compile_stderr
        ));
    }

    let program = dir.join("prog");
    if !program.exists() {
        return Err("compiler reported success but produced no 'prog' binary".to_string());
    }

    let mut run_cmd = Command::new(&program);
    run_cmd.current_dir(dir);
    let run_output = run_with_timeout(run_cmd, Duration::from_millis(DEFAULT_TIMEOUT_MS), None)?;
    let run_stdout = String::from_utf8_lossy(&run_output.stdout).to_string();
    let run_stderr = String::from_utf8_lossy(&run_output.stderr).to_string();

    // "Runs without crashing": a normal exit yields Some(code); death by
    // signal (SIGSEGV/SIGABRT) yields None on unix.
    let actual_exit = match run_output.status.code() {
        Some(code) => code,
        None => {
            return Err(format!(
                "example crashed (killed by signal, status {:?})\n--- program stdout ---\n{}\n--- program stderr ---\n{}",
                run_output.status, run_stdout, run_stderr
            ));
        }
    };

    if let Some(exp) = expectation {
        if actual_exit != exp.exit_code {
            return Err(format!(
                "example exit code mismatch:\n  expected: {}\n  actual: {}\n--- program stdout ---\n{}\n--- program stderr ---\n{}",
                exp.exit_code, actual_exit, run_stdout, run_stderr
            ));
        }
        if run_stdout != exp.stdout {
            return Err(format!(
                "example stdout mismatch:\n--- expected ---\n{}\n--- actual ---\n{}",
                exp.stdout, run_stdout
            ));
        }
    }

    Ok(())
}

/// Discover `examples/*.rue` and build one smoke-test trial per file (RUE-48).
///
/// This is self-maintaining: it enumerates the directory at run time, so a new
/// example is picked up automatically. If the directory can't be found or holds
/// no `.rue` files, a single loud failing trial is emitted rather than silently
/// running zero example tests (which would let example rot slip through CI).
fn example_trials(rue_binary: &Path) -> Vec<Trial> {
    let examples_dir = find_dir("RUE_EXAMPLES_DIR", EXAMPLES_DIR_PATHS, "examples");

    let mut example_files: Vec<PathBuf> = match std::fs::read_dir(&examples_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "rue"))
            .collect(),
        Err(e) => {
            let msg = format!(
                "cannot read examples directory '{}': {} (set RUE_EXAMPLES_DIR)",
                examples_dir.display(),
                e
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
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("failed to read {}: {}", path.display(), e);
                trials.push(Trial::test(
                    format!("cli.examples::{}", name),
                    move |_ctx| Err(RunError::fail(msg.clone())),
                ));
                continue;
            }
        };
        let rue_binary = rue_binary.to_path_buf();
        let test_name = format!("cli.examples::{}", name);
        trials.push(Trial::test(test_name, move |_ctx| {
            let expectation = EXAMPLE_EXPECTATIONS.iter().find(|e| e.name == name);
            run_example(&name, &source, expectation, &rue_binary).map_err(RunError::fail)
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

    // RUE-48: compile+run every examples/*.rue through the real driver, so a
    // regression that breaks a shipped example (or an example referencing a
    // removed flag) can't slip past CI unnoticed.
    tests.extend(example_trials(&rue_binary));

    Harness::with_env().discover(tests).main();
}
