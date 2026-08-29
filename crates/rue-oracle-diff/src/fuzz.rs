//! # Differential fuzzer (`rue-oracle-diff fuzz`) — RUE-247
//!
//! Generate random **valid** Rue programs (see [`crate::gen`]) and run each one
//! through *both* engines:
//!
//! 1. the [`rue_oracle`] reference interpreter at both canonical CFG boundaries,
//! 2. the real compiler + produced native binaries at O0, O1, O2, and O3.
//!
//! Then compare the observable behavior — process exit code, stdout, stderr,
//! and typed trap cause.
//! A disagreement is an **automatically-discovered miscompile**. A generated
//! compile failure or oracle `Unsupported` result is a generator-contract
//! failure. Both are findings with concrete, deterministic repros: the seed
//! regenerates the exact program. This is the RUE-50 payoff wired to RUE-205's
//! harness — "Fable runs a hunt and files bugs" becomes "CI files the bugs."
//!
//! Determinism: programs are a pure function of their `u64` seed, so the fuzzer
//! is fully reproducible (`fuzz --start S --seeds 1` re-runs exactly seed `S`).
//! `--timeout` is a wall-clock budget applied independently to each compiler
//! invocation and each generated binary, so either phase can fail boundedly.
//!
//! The compiler binary is located via `RUE_BINARY`, which `scripts/rue`,
//! `test.sh`, and Buck test targets set from `scripts/rue-bin`.

use crate::{generator, trap::native_runtime_trap_kind};
use rue_oracle::{
    MAX_STDERR_BYTES, MAX_STDOUT_BYTES, RunSourceError, TrapKind, Unsupported, UnsupportedKind,
    run_source_cfg_differential,
};
use std::fmt;
use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Compiler optimization lanes covered by the native differential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptimizationLevel {
    O0,
    O1,
    O2,
    O3,
}

impl OptimizationLevel {
    pub(crate) const OPTIMIZED: [Self; 3] = [Self::O1, Self::O2, Self::O3];

    fn flag(self) -> &'static str {
        match self {
            Self::O0 => "-O0",
            Self::O1 => "-O1",
            Self::O2 => "-O2",
            Self::O3 => "-O3",
        }
    }
}

impl fmt::Display for OptimizationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.flag().trim_start_matches('-'))
    }
}

pub(crate) struct CompileOptions<'a> {
    pub(crate) optimization: OptimizationLevel,
    pub(crate) previews: &'a [String],
    pub(crate) std_path: Option<&'a Path>,
    pub(crate) compile_timeout: Duration,
    pub(crate) runtime_timeout: Duration,
}

/// Outcome of compiling and running a generated program natively.
#[derive(Debug)]
pub(crate) enum Compiled {
    /// The compiler rejected a program the oracle's frontend accepted — a
    /// front/backend gap distinct from an ICE (carries truncated stderr).
    CompileRejected { exit: i32, stderr: String },
    /// The compiler terminated by signal rather than rejecting the source.
    CompileCrash { signal: i32, stderr: String },
    /// The compiler reported an internal panic/ICE with an ordinary exit code.
    CompileIce(String),
    /// The compiler did not terminate within the per-phase timeout.
    CompileTimeout,
    /// The binary ran to completion: process exit code + captured stdout +
    /// captured stderr. `stderr` carries the runtime's trap message (e.g.
    /// `"error: integer overflow\n"`) so that when both engines exit 101 we can
    /// compare *which* trap fired, not just that one did (RUE-339).
    Ran {
        exit: i32,
        /// Exact captured bytes. Diagnostics may render this lossily, but all
        /// differential comparisons use the raw trace.
        stdout: Vec<u8>,
        /// More stdout existed beyond [`MAX_STDOUT_BYTES`]; `stdout` is only a prefix.
        stdout_truncated: bool,
        stderr: String,
        /// More stderr existed beyond [`MAX_STDERR_BYTES`]; `stderr` is only a prefix.
        stderr_truncated: bool,
    },
    /// The binary was killed by a signal (e.g. SIGSEGV) — a hard miscompile.
    Crash(i32),
    /// The binary did not terminate within the per-program timeout.
    Timeout,
}

/// A generated program violated the generator's contract before native
/// compilation could be compared with the oracle. Generated programs promise
/// both to compile and to stay inside the oracle's modeled subset, so neither
/// failure is an ordinary differential-harness skip.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GeneratorContractFailure {
    /// The shared Rue frontend rejected generated source.
    Compile(String),
    /// The source compiled, but evaluation reached an unmodeled construct.
    Unsupported(Unsupported),
    /// The generated position-twin invariant itself failed in the oracle.
    Invariant(String),
}

impl GeneratorContractFailure {
    fn from_run_source(error: RunSourceError) -> Self {
        match error {
            RunSourceError::Compile(errors) => Self::Compile(format!("{errors:#?}")),
            RunSourceError::Unsupported(unsupported) => Self::Unsupported(unsupported),
            RunSourceError::CfgTransformationDisagreement {
                pre_optimization,
                post_optimization,
            } => Self::Invariant(format!(
                "pre/post CFG execution disagreement: pre={pre_optimization:?}, post={post_optimization:?}"
            )),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Compile(_) => "compile",
            Self::Unsupported(_) => "unsupported",
            Self::Invariant(_) => "invariant",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Compile(detail) => detail,
            Self::Unsupported(unsupported) => unsupported.detail(),
            Self::Invariant(detail) => detail,
        }
    }

    fn unsupported_kind(&self) -> Option<UnsupportedKind> {
        match self {
            Self::Compile(_) => None,
            Self::Unsupported(unsupported) => Some(unsupported.kind()),
            Self::Invariant(_) => None,
        }
    }
}

/// Generated programs contain exactly one assertion site: the position-twin
/// check, before any random snippets.  A failed assertion is therefore a
/// generator invariant failure, not a result to compare against native code.
fn generated_invariant_failure(oracle: &rue_oracle::Outcome) -> Option<GeneratorContractFailure> {
    (oracle.panic == Some(TrapKind::AssertionFailure))
        .then(|| GeneratorContractFailure::Invariant("position twin mismatch".to_string()))
}

struct GeneratorContractFinding {
    seed: u64,
    source: String,
    failure: GeneratorContractFailure,
}

impl GeneratorContractFinding {
    fn render(&self) -> String {
        let typed_kind = self
            .failure
            .unsupported_kind()
            .map(|kind| format!(" ({kind:?})"))
            .unwrap_or_default();
        format!(
            "\n\u{2717} GENERATOR CONTRACT FAILURE (seed {seed})\n  {kind}{typed_kind}: {detail}\n  \
             --- source (regenerate with `fuzz --start {seed} --seeds 1`) ---\n{source}",
            seed = self.seed,
            kind = self.failure.kind(),
            typed_kind = typed_kind,
            detail = self.failure.detail(),
            source = self.source,
        )
    }
}

struct Config {
    start: u64,
    seeds: u64,
    /// Wall-clock budget applied independently to compilation and execution.
    timeout: Duration,
    crash_dir: PathBuf,
    verbose: bool,
    /// Test-harness-only observation mutation proving a selected optimized
    /// lane detects a wrong result. It never changes compiler production code.
    planted_miscompile: Option<OptimizationLevel>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            start: 0,
            seeds: 200,
            timeout: Duration::from_secs(10),
            crash_dir: PathBuf::from("crates/rue-fuzz/crashes"),
            verbose: false,
            planted_miscompile: None,
        }
    }
}

fn parse_args(args: &[String]) -> Result<Config, String> {
    let mut cfg = Config::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        // Support both `--flag value` and `--flag=value`.
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (a.as_str(), None),
        };
        let mut value = || -> Result<String, String> {
            if let Some(v) = inline.clone() {
                Ok(v)
            } else {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("missing value for {key}"))
            }
        };
        match key {
            "--start" => cfg.start = value()?.parse().map_err(|_| "bad --start")?,
            "--seeds" => cfg.seeds = value()?.parse().map_err(|_| "bad --seeds")?,
            "--timeout" => {
                cfg.timeout = Duration::from_secs(value()?.parse().map_err(|_| "bad --timeout")?)
            }
            "--crash-dir" => cfg.crash_dir = PathBuf::from(value()?),
            "--verbose" | "-v" => cfg.verbose = true,
            "--test-plant-miscompile" => {
                if std::env::var_os("RUE_ORACLE_DIFF_TESTING").is_none() {
                    return Err(
                        "--test-plant-miscompile requires RUE_ORACLE_DIFF_TESTING".to_string()
                    );
                }
                cfg.planted_miscompile = Some(match value()?.as_str() {
                    "O1" | "1" => OptimizationLevel::O1,
                    "O2" | "2" => OptimizationLevel::O2,
                    "O3" | "3" => OptimizationLevel::O3,
                    _ => return Err("bad --test-plant-miscompile (expected O1, O2, or O3)".into()),
                });
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if cfg.seeds == 0 {
        return Err("--seeds must be greater than zero".to_string());
    }
    cfg.start
        .checked_add(cfg.seeds)
        .ok_or_else(|| "--start + --seeds exceeds the u64 seed range".to_string())?;

    Ok(cfg)
}

/// Locate the `rue` compiler binary the same way the rest of the test harness
/// does: via an explicit `RUE_BINARY` override.
fn find_rue_binary() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("RUE_BINARY") {
        return Ok(PathBuf::from(p));
    }
    Err(
        "cannot locate the rue compiler: set RUE_BINARY to an explicit path, \
         for example `RUE_BINARY=$(scripts/rue-bin)`"
            .to_string(),
    )
}

pub fn run(args: &[String]) -> ExitCode {
    let cfg = match parse_args(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rue-oracle-diff fuzz: {e}");
            eprintln!(
                "usage: rue-oracle-diff fuzz [--start N] [--seeds N] [--timeout SECS] \
                 [--crash-dir DIR] [--verbose]"
            );
            eprintln!(
                "       --timeout is applied separately to each compiler invocation and \
                 generated binary"
            );
            return ExitCode::FAILURE;
        }
    };

    let rue = match find_rue_binary() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rue-oracle-diff fuzz: {e}");
            return ExitCode::FAILURE;
        }
    };

    // A unique RAII directory prevents concurrent/PID-reused runs from sharing
    // stale binaries and removes all generated sources/binaries on every
    // ordinary return path (success, finding, or infrastructure failure).
    let workdir = match create_workdir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("cannot create temporary work dir: {e}");
            return ExitCode::FAILURE;
        }
    };

    let seed_end = cfg
        .start
        .checked_add(cfg.seeds)
        .expect("parse_args validates the seed range");

    println!(
        "=== rue-oracle-diff fuzz: seeds {}..{} (compiler: {}, timeout: {}s per phase) ===",
        cfg.start,
        seed_end,
        rue.display(),
        cfg.timeout.as_secs()
    );

    let mut agree = 0u32;
    let mut generator_contract_failures: Vec<GeneratorContractFinding> = Vec::new();
    let mut disagreements: Vec<Disagreement> = Vec::new();

    for seed in cfg.start..seed_end {
        let source = generator::generate(seed);

        let oracle = match run_source_cfg_differential(&source) {
            Err(error) => {
                // Unlike corpus mode, generated mode has a strong input
                // contract: every program must compile and remain within the
                // oracle's supported subset. Record either typed failure as a
                // finding, save its exact deterministic source, and continue so
                // one bad seed cannot hide the rest of the batch.
                let finding = GeneratorContractFinding {
                    seed,
                    source: source.clone(),
                    failure: GeneratorContractFailure::from_run_source(error),
                };
                eprintln!("{}", finding.render());
                if let Err(error) = save_generator_contract_repro(&cfg.crash_dir, &finding) {
                    report_repro_write_error(&cfg.crash_dir, seed, None, &error);
                }
                generator_contract_failures.push(finding);
                continue;
            }
            Ok(o) => o,
        };

        if let Some(failure) = generated_invariant_failure(&oracle) {
            let finding = GeneratorContractFinding {
                seed,
                source: source.clone(),
                failure,
            };
            eprintln!("{}", finding.render());
            if let Err(error) = save_generator_contract_repro(&cfg.crash_dir, &finding) {
                report_repro_write_error(&cfg.crash_dir, seed, None, &error);
            }
            generator_contract_failures.push(finding);
            continue;
        }

        for level in [
            OptimizationLevel::O0,
            OptimizationLevel::O1,
            OptimizationLevel::O2,
            OptimizationLevel::O3,
        ] {
            let mut compiled = match compile_and_run(
                &rue,
                workdir.path(),
                &source,
                CompileOptions {
                    optimization: level,
                    previews: &[],
                    std_path: None,
                    compile_timeout: cfg.timeout,
                    runtime_timeout: cfg.timeout,
                },
            ) {
                Ok(c) => c,
                Err(e) => {
                    // An infrastructure failure (couldn't invoke the tools) is fatal
                    // — better to stop loudly than silently pass.
                    eprintln!("seed {seed} [{level}]: harness error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            plant_test_miscompile(&mut compiled, level, cfg.planted_miscompile);

            match classify(&oracle, &compiled) {
                Verdict::Agree => {
                    agree += 1;
                    if cfg.verbose {
                        println!("  seed {seed} [{level}]: agree (exit {})", oracle.exit_code);
                    }
                }
                Verdict::Disagree(reason) => {
                    let d = Disagreement {
                        seed,
                        optimization: level,
                        timeout_secs: cfg.timeout.as_secs(),
                        source: source.clone(),
                        oracle_exit: oracle.exit_code,
                        oracle_stdout: oracle.stdout_bytes.clone(),
                        oracle_stderr: oracle.stderr.clone(),
                        oracle_panic: oracle.panic,
                        compiled: describe(&compiled),
                        reason,
                    };
                    eprintln!("{}", d.render());
                    if let Err(error) = save_repro(&cfg.crash_dir, &d) {
                        report_repro_write_error(&cfg.crash_dir, seed, Some(level), &error);
                    }
                    disagreements.push(d);
                }
            }
        }
    }

    let total_lanes = agree + disagreements.len() as u32;
    println!(
        "\n=== summary over {} generated programs / {total_lanes} native lanes ===",
        cfg.seeds
    );
    println!("  agreeing lanes:   {agree}");
    println!(
        "  GENERATOR CONTRACT FAILURES: {}",
        generator_contract_failures.len()
    );
    println!("  DISAGREEMENTS:    {}", disagreements.len());

    if generated_batch_passes(generator_contract_failures.len(), disagreements.len()) {
        println!("\noracle and compiler agree on every generated program.");
        ExitCode::SUCCESS
    } else {
        println!(
            "\n{} generator contract failure(s), {} disagreement(s). \
             Repros targeted at {}; any write failures are reported above.",
            generator_contract_failures.len(),
            disagreements.len(),
            cfg.crash_dir.display()
        );
        ExitCode::FAILURE
    }
}

fn create_workdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("rue-oracle-fuzz-")
        .tempdir()
}

fn generated_batch_passes(contract_failures: usize, disagreements: usize) -> bool {
    contract_failures == 0 && disagreements == 0
}

/// The comparison verdict.
pub(crate) enum Verdict {
    Agree,
    Disagree(String),
}

pub(crate) fn classify(oracle: &rue_oracle::Outcome, compiled: &Compiled) -> Verdict {
    match compiled {
        Compiled::Ran {
            exit,
            stdout,
            stdout_truncated,
            stderr,
            stderr_truncated,
        } => {
            // A retained prefix can equal the oracle's complete output while
            // hiding additional compiled output. Fail before comparing that
            // prefix so truncation can never manufacture agreement.
            if *stdout_truncated {
                return Verdict::Disagree(format!(
                    "compiled stdout exceeded the {MAX_STDOUT_BYTES}-byte capture limit; \
                     retained prefix cannot prove agreement"
                ));
            }
            if *stderr_truncated {
                return Verdict::Disagree(format!(
                    "compiled stderr exceeded the {MAX_STDERR_BYTES}-byte capture limit; \
                     retained prefix cannot prove agreement"
                ));
            }
            let exit_ok = oracle.exit_code == *exit;
            let stdout_ok = oracle.stdout_bytes == *stdout;
            let stderr_ok = &oracle.stderr == stderr;
            if !exit_ok || !stdout_ok || !stderr_ok {
                let mut r = String::new();
                if !exit_ok {
                    r += &format!("exit: oracle {} vs compiled {exit}; ", oracle.exit_code);
                }
                if !stdout_ok {
                    r += &format!(
                        "stdout: oracle {:?} vs compiled {:?}",
                        display_bytes(&oracle.stdout_bytes),
                        display_bytes(stdout)
                    );
                }
                if !stderr_ok {
                    r += &format!(
                        "stderr: oracle {:?} vs compiled {:?}",
                        oracle.stderr, stderr
                    );
                }
                return Verdict::Disagree(r);
            }
            // Exit code and stdout agree. Exit 101 alone is not proof of the
            // same semantics: every Rue trap shares it, and a normal `return
            // 101` is legal. Compare the oracle's typed cause with the native
            // runtime message, failing closed when either trapped cause cannot
            // be classified.
            if *exit == 101 {
                let native_trap = native_runtime_trap_kind(stderr);
                match (oracle.panic, native_trap) {
                    (Some(want), Some(got)) if want == got => {}
                    (Some(want), Some(got)) => {
                        return Verdict::Disagree(format!(
                            "trap category: oracle {want:?} vs compiled {got:?} \
                             (both exit 101; compiled stderr {:?})",
                            first_line(stderr)
                        ));
                    }
                    (Some(want), None) => {
                        return Verdict::Disagree(format!(
                            "oracle trapped with {want:?}, but compiled exit 101 had no \
                             recognized trap category (stderr {:?})",
                            first_line(stderr)
                        ));
                    }
                    (None, Some(got)) => {
                        return Verdict::Disagree(format!(
                            "oracle returned 101 normally, but compiled code trapped with {got:?}"
                        ));
                    }
                    (None, None) if !stderr.is_empty() => {
                        return Verdict::Disagree(format!(
                            "oracle returned 101 normally, but compiled exit 101 wrote \
                             unclassified stderr {:?}",
                            first_line(stderr)
                        ));
                    }
                    (None, None) => {}
                }
            }
            Verdict::Agree
        }
        Compiled::CompileRejected { exit, stderr } => Verdict::Disagree(format!(
            "compiler rejected a program the oracle accepted (exit {exit}): {}",
            first_line(stderr)
        )),
        Compiled::CompileCrash { signal, stderr } => Verdict::Disagree(format!(
            "compiler crashed with signal {signal}: {}",
            first_line(stderr)
        )),
        Compiled::CompileIce(detail) => {
            Verdict::Disagree(format!("internal compiler error: {}", first_line(detail)))
        }
        Compiled::CompileTimeout => Verdict::Disagree(
            "compiler did not terminate within the per-phase timeout (oracle ran cleanly)"
                .to_string(),
        ),
        Compiled::Crash(sig) => Verdict::Disagree(format!(
            "compiled binary killed by signal {sig} (oracle ran cleanly)"
        )),
        Compiled::Timeout => {
            Verdict::Disagree("compiled binary did not terminate (oracle ran cleanly)".to_string())
        }
    }
}

pub(crate) fn compile_and_run(
    rue: &Path,
    dir: &Path,
    source: &str,
    options: CompileOptions<'_>,
) -> std::io::Result<Compiled> {
    let src_path = dir.join("prog.rue");
    std::fs::write(&src_path, source)?;
    let bin_path = dir.join("prog");
    // Remove a stale binary so a compile failure cannot run the previous
    // iteration's executable. Only absence is benign: a permission or file
    // type error means the harness cannot establish artifact freshness.
    match std::fs::remove_file(&bin_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut compile_cmd = Command::new(rue);
    compile_cmd
        .arg(options.optimization.flag())
        .arg("prog.rue")
        .arg("-o")
        .arg("prog")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    compile_cmd.env_remove("RUE_STD_PATH");
    if let Some(std_path) = options.std_path {
        compile_cmd.env("RUE_STD_PATH", std_path);
    }
    for preview in options.previews {
        compile_cmd.arg("--preview").arg(preview);
    }
    match run_process_with_timeout(compile_cmd, options.compile_timeout)? {
        ProcessOutcome::TimedOut => return Ok(Compiled::CompileTimeout),
        ProcessOutcome::Exited { status, stderr, .. } => {
            if let Some(signal) = status.signal() {
                return Ok(Compiled::CompileCrash { signal, stderr });
            }
            if let Some(failure) = rue_test_runner::ice_message(&status, &stderr) {
                return Ok(Compiled::CompileIce(failure.to_string()));
            }
            if !status.success() {
                return Ok(Compiled::CompileRejected {
                    exit: status.code().unwrap_or(-1),
                    stderr,
                });
            }
        }
    }

    let metadata = std::fs::metadata(&bin_path).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "compiler succeeded without producing {}: {error}",
                bin_path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(std::io::Error::other(format!(
            "compiler output {} is not a regular file",
            bin_path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("compiler output {} is not executable", bin_path.display()),
            ));
        }
    }

    // Run the produced binary directly with a manual timeout so we can read the
    // child's own exit code / terminating signal unambiguously. stderr is
    // captured (not `Stdio::null`) so a trap's message is available to
    // `classify` for panic-category comparison (RUE-339).
    let mut cmd = Command::new(&bin_path);
    cmd.current_dir(dir);
    run_with_timeout(cmd, options.runtime_timeout)
}

/// Mutate only the harness's captured observation, and only when the guarded
/// negative-test flag selected this exact optimized lane. Wrapping makes the
/// planted wrong exit deterministic for every native result, including 255.
fn plant_test_miscompile(
    compiled: &mut Compiled,
    level: OptimizationLevel,
    selected: Option<OptimizationLevel>,
) {
    if selected != Some(level) {
        return;
    }
    if let Compiled::Ran { exit, .. } = compiled {
        *exit = (*exit + 1).rem_euclid(256);
    }
}

/// Result of one child process whose stdout/stderr were drained while it ran.
enum ProcessOutcome {
    Exited {
        status: ExitStatus,
        stdout: Vec<u8>,
        stdout_truncated: bool,
        stderr: String,
        stderr_truncated: bool,
    },
    TimedOut,
}

/// Spawn a configured command, drain any piped stdout/stderr **concurrently**
/// via reader threads, and wait with a manual timeout.
///
/// Draining concurrently is essential (RUE-338): if the pipes were only read
/// after the child exits, a program that writes more than the OS pipe capacity
/// (~64KB on Linux) would block on `write()` forever, `try_wait` would never
/// report an exit. This applies to the Rue compiler as well as to generated
/// binaries: compiler diagnostics can also exceed a pipe's capacity.
///
/// The reader threads start immediately after `spawn` so the child always has
/// a consumer. On timeout or a wait error, kill/reap happens before joining the
/// readers; the closed write ends give them EOF, so no child or thread leaks.
fn run_process_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> std::io::Result<ProcessOutcome> {
    rue_test_runner::configure_process_group(&mut cmd);
    let child = cmd.spawn()?;
    wait_for_process(child, timeout)
}

fn wait_for_process(mut child: Child, timeout: Duration) -> std::io::Result<ProcessOutcome> {
    // When piped, stdout and stderr are drained to EOF while retaining fixed
    // prefixes; compiler stdout is configured as null. Both readers run on
    // their own thread so neither pipe can fill and block the child.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        stdout_pipe.map_or_else(CappedRead::default, |out| {
            read_capped(out, MAX_STDOUT_BYTES)
        })
    });
    let stderr_reader = std::thread::spawn(move || {
        stderr_pipe.map_or_else(CappedRead::default, |err| {
            read_capped(err, MAX_STDERR_BYTES)
        })
    });

    let start = Instant::now();
    let status: std::io::Result<Option<ExitStatus>> = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // The direct child is authoritative for success even if it
                // crossed the deadline concurrently with this poll. Tear down
                // any descendants it left in its private process group before
                // joining readers, because an inherited pipe fd would
                // otherwise keep those joins blocked forever.
                terminate_and_reap(&mut child);
                break Ok(Some(status));
            }
            Ok(None) if start.elapsed() >= timeout => {
                terminate_and_reap(&mut child);
                break Ok(None);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                terminate_and_reap(&mut child);
                break Err(error);
            }
        }
    };

    // Whether the child exited, timed out, or hit a wait error, it has been
    // reaped before these joins. Its write ends are closed, so both readers hit
    // EOF and finish promptly.
    let stdout_capture = stdout_reader.join().unwrap_or_default();
    let stderr_capture = stderr_reader.join().unwrap_or_default();
    // Keep stdout raw through ProcessOutcome and Compiled. Lossy conversion is
    // reserved for diagnostics, so invalid bytes cannot collapse into a false
    // differential agreement.
    let stdout = stdout_capture.bytes;
    let stderr = String::from_utf8_lossy(&stderr_capture.bytes).into_owned();

    match status? {
        Some(status) => Ok(ProcessOutcome::Exited {
            status,
            stdout,
            stdout_truncated: stdout_capture.truncated,
            stderr,
            stderr_truncated: stderr_capture.truncated,
        }),
        None => Ok(ProcessOutcome::TimedOut),
    }
}

fn terminate_and_reap(child: &mut Child) {
    // The canonical helper signals the child's private process group before
    // reaping the direct child. This closes pipe fds inherited by compiler
    // linkers or native grandchildren as well as handling a natural-exit race.
    rue_test_runner::kill_process_group(child);
}

/// Run one generated native binary using the shared bounded subprocess path.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<Compiled> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(match run_process_with_timeout(cmd, timeout)? {
        ProcessOutcome::TimedOut => Compiled::Timeout,
        ProcessOutcome::Exited {
            status,
            stdout,
            stdout_truncated,
            stderr,
            stderr_truncated,
        } => match status.code() {
            Some(code) => Compiled::Ran {
                exit: code,
                stdout,
                stdout_truncated,
                stderr,
                stderr_truncated,
            },
            None => Compiled::Crash(status.signal().unwrap_or(0)),
        },
    })
}

/// Read `r` to EOF, retaining at most `cap` bytes. Reading all the way to EOF
/// (rather than stopping after `cap`, as `Read::take` would) is what prevents a
/// drain-deadlock: a program that writes more than `cap` to this pipe must still
/// have every byte consumed or it blocks on a full pipe forever (RUE-338). The
/// cap bounds only the memory we keep, not how much we drain; `truncated` records
/// whether any bytes were discarded after the retained prefix.
#[derive(Default)]
struct CappedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_capped<R: Read>(mut r: R, cap: usize) -> CappedRead {
    let mut capture = CappedRead::default();
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if capture.bytes.len() < cap {
                    let take = (cap - capture.bytes.len()).min(n);
                    capture.bytes.extend_from_slice(&chunk[..take]);
                    capture.truncated |= take < n;
                } else {
                    capture.truncated = true;
                }
                // Bytes beyond `cap` are read and discarded — draining, not
                // storing — so the pipe never fills.
            }
            Err(_) => break,
        }
    }
    capture
}

struct Disagreement {
    seed: u64,
    optimization: OptimizationLevel,
    /// Per-phase compiler/native execution budget needed to replay timeouts.
    timeout_secs: u64,
    source: String,
    oracle_exit: i32,
    /// Exact oracle stdout bytes; persisted diagnostics render escapes so
    /// invalid sequences remain distinguishable.
    oracle_stdout: Vec<u8>,
    oracle_stderr: String,
    oracle_panic: Option<TrapKind>,
    compiled: String,
    reason: String,
}

impl Disagreement {
    fn render(&self) -> String {
        format!(
            "\n\u{2717} DISAGREEMENT (seed {seed}, {optimization})\n  {reason}\n  oracle:   exit={exit} panic={panic:?} stdout={stdout:?} stderr={stderr:?}\n  compiled: {compiled}\n  --- source (regenerate with `fuzz --start {seed} --seeds 1 --timeout {timeout_secs}`) ---\n{source}",
            seed = self.seed,
            optimization = self.optimization,
            timeout_secs = self.timeout_secs,
            reason = self.reason,
            exit = self.oracle_exit,
            panic = self.oracle_panic,
            stdout = display_bytes(&self.oracle_stdout),
            stderr = self.oracle_stderr,
            compiled = self.compiled,
            source = self.source,
        )
    }
}

fn describe(c: &Compiled) -> String {
    match c {
        Compiled::Ran {
            exit,
            stdout,
            stdout_truncated,
            stderr,
            stderr_truncated,
            ..
        } => {
            let label = if *stdout_truncated {
                "stdout-prefix"
            } else {
                "stdout"
            };
            format!(
                "ran exit={exit} {label}={displayed_stdout:?} stdout-truncated={stdout_truncated} \
                 stderr={stderr:?} stderr-truncated={stderr_truncated}",
                displayed_stdout = display_bytes(stdout),
            )
        }
        Compiled::CompileRejected { exit, stderr } => {
            format!("compile-rejected exit={exit}: {}", first_line(stderr))
        }
        Compiled::CompileCrash { signal, stderr } => {
            format!("compiler-crash signal={signal}: {}", first_line(stderr))
        }
        Compiled::CompileIce(detail) => format!("compiler-ice: {}", first_line(detail)),
        Compiled::CompileTimeout => "compile-timeout".to_string(),
        Compiled::Crash(sig) => format!("crash signal={sig}"),
        Compiled::Timeout => "timeout".to_string(),
    }
}

/// Render captured bytes for diagnostics and persisted repro metadata without
/// losing identity. Printable ASCII stays readable; controls and every
/// non-ASCII byte use deterministic escapes, so `0xff` and `0xfe` cannot
/// collapse through lossy UTF-8 decoding.
fn display_bytes(bytes: &[u8]) -> String {
    let mut rendered = String::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'\\' => rendered.push_str("\\\\"),
            b'"' => rendered.push_str("\\\""),
            b'\n' => rendered.push_str("\\n"),
            b'\r' => rendered.push_str("\\r"),
            b'\t' => rendered.push_str("\\t"),
            0x20..=0x7e => rendered.push(byte as char),
            _ => rendered.push_str(&format!("\\x{byte:02x}")),
        }
    }
    rendered
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

fn repro_path(dir: &Path, seed: u64, optimization: Option<OptimizationLevel>) -> PathBuf {
    match optimization {
        Some(level) => dir.join(format!("oracle-diff-seed-{seed}-{level}.rue")),
        None => dir.join(format!("oracle-diff-seed-{seed}.rue")),
    }
}

/// Escape dynamic diagnostic text onto one physical `//` line. Diagnostics may
/// contain newlines, carriage returns, or other controls; writing them verbatim
/// could end the comment and turn metadata into active Rue source, making a
/// saved repro non-reproducible (or even syntactically invalid).
fn comment_safe(text: &str) -> String {
    text.chars().flat_map(char::escape_debug).collect()
}

fn push_repro_comment(contents: &mut String, label: &str, value: &str) {
    contents.push_str("// ");
    contents.push_str(label);
    contents.push_str(": ");
    contents.push_str(&comment_safe(value));
    contents.push('\n');
}

fn disagreement_repro_contents(d: &Disagreement) -> String {
    let mut contents = format!(
        "// rue-oracle-diff differential miscompile (seed {})\n",
        d.seed
    );
    push_repro_comment(&mut contents, "optimization", &d.optimization.to_string());
    push_repro_comment(&mut contents, "reason", &d.reason);
    push_repro_comment(
        &mut contents,
        "oracle",
        &format!(
            "exit={} panic={:?} stdout={:?} stderr={:?}",
            d.oracle_exit,
            d.oracle_panic,
            display_bytes(&d.oracle_stdout),
            d.oracle_stderr
        ),
    );
    push_repro_comment(&mut contents, "compiled", &d.compiled);
    push_repro_comment(
        &mut contents,
        "regenerate",
        &format!(
            "rue-oracle-diff fuzz --start {} --seeds 1 --timeout {}",
            d.seed, d.timeout_secs
        ),
    );
    contents.push('\n');
    contents.push_str(&d.source);
    contents
}

fn generator_contract_repro_contents(finding: &GeneratorContractFinding) -> String {
    let mut contents = format!(
        "// rue-oracle-diff generator contract failure (seed {})\n",
        finding.seed
    );
    push_repro_comment(&mut contents, "failure kind", finding.failure.kind());
    if let Some(kind) = finding.failure.unsupported_kind() {
        push_repro_comment(&mut contents, "oracle cause", &format!("{kind:?}"));
    }
    push_repro_comment(&mut contents, "detail", finding.failure.detail());
    push_repro_comment(
        &mut contents,
        "regenerate",
        &format!("rue-oracle-diff fuzz --start {} --seeds 1", finding.seed),
    );
    contents.push('\n');
    contents.push_str(&finding.source);
    contents
}

fn write_repro(
    dir: &Path,
    seed: u64,
    optimization: Option<OptimizationLevel>,
    contents: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(repro_path(dir, seed, optimization), contents)
}

fn report_repro_write_error(
    dir: &Path,
    seed: u64,
    optimization: Option<OptimizationLevel>,
    error: &std::io::Error,
) {
    eprintln!(
        "seed {seed}: failed to save repro at {}: {error}",
        repro_path(dir, seed, optimization).display()
    );
}

/// Persist a repro so a CI failure uploads a concrete, self-contained program.
fn save_repro(dir: &Path, d: &Disagreement) -> std::io::Result<()> {
    write_repro(
        dir,
        d.seed,
        Some(d.optimization),
        &disagreement_repro_contents(d),
    )
}

fn save_generator_contract_repro(
    dir: &Path,
    finding: &GeneratorContractFinding,
) -> std::io::Result<()> {
    write_repro(
        dir,
        finding.seed,
        None,
        &generator_contract_repro_contents(finding),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_oracle::Outcome;
    use std::os::unix::fs::PermissionsExt;

    fn make_executable(path: &Path) {
        let mut permissions = std::fs::metadata(path)
            .expect("executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make file executable");
    }

    fn fake_compiler(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("fake-rue");
        std::fs::write(&path, script).expect("write fake compiler");
        make_executable(&path);
        path
    }

    fn oc(exit: i32, stdout: &str) -> Outcome {
        Outcome {
            exit_code: exit,
            stdout: stdout.to_string(),
            stdout_bytes: stdout.as_bytes().to_vec(),
            stderr: String::new(),
            panic: None,
        }
    }

    fn oc_bytes(exit: i32, stdout: &[u8]) -> Outcome {
        Outcome {
            exit_code: exit,
            stdout: String::from_utf8_lossy(stdout).into_owned(),
            stdout_bytes: stdout.to_vec(),
            stderr: String::new(),
            panic: None,
        }
    }

    /// An oracle outcome that ended in a runtime trap (exit 101).
    fn trap(kind: TrapKind) -> Outcome {
        let stderr = match kind {
            TrapKind::ArithmeticOverflow => "error: integer overflow\n",
            TrapKind::DivisionByZero => "error: division by zero\n",
            TrapKind::IntegerCastOverflow => "error: integer cast overflow\n",
            TrapKind::IndexOutOfBounds => "error: index out of bounds\n",
            TrapKind::InvalidUtf8 => "error: invalid UTF-8\n",
            TrapKind::UserPanic => "panic: user message\n",
            TrapKind::AssertionFailure => "assertion failed\n",
            TrapKind::Unreachable => "",
        };
        Outcome {
            exit_code: 101,
            stdout: String::new(),
            stdout_bytes: Vec::new(),
            stderr: stderr.to_string(),
            panic: Some(kind),
        }
    }

    /// A `Compiled::Ran` with no captured stderr (for the non-trap tests).
    fn ran(exit: i32, stdout: &str) -> Compiled {
        Compiled::Ran {
            exit,
            stdout: stdout.as_bytes().to_vec(),
            stdout_truncated: false,
            stderr: String::new(),
            stderr_truncated: false,
        }
    }

    /// A `Compiled::Ran` that trapped at exit 101 with the given stderr message.
    fn ran_trap(stderr: &str) -> Compiled {
        Compiled::Ran {
            exit: 101,
            stdout: Vec::new(),
            stdout_truncated: false,
            stderr: stderr.to_string(),
            stderr_truncated: false,
        }
    }

    fn is_disagree(v: Verdict) -> bool {
        matches!(v, Verdict::Disagree(_))
    }

    #[test]
    fn planted_miscompile_mutates_only_the_selected_optimized_lane() {
        let mut o0 = ran(42, "");
        let mut o1 = ran(42, "");
        let mut o2 = ran(42, "");
        let mut o3 = ran(42, "");
        for (compiled, level) in [
            (&mut o0, OptimizationLevel::O0),
            (&mut o1, OptimizationLevel::O1),
            (&mut o2, OptimizationLevel::O2),
            (&mut o3, OptimizationLevel::O3),
        ] {
            plant_test_miscompile(compiled, level, Some(OptimizationLevel::O2));
        }
        let oracle = oc(42, "");
        assert!(matches!(classify(&oracle, &o0), Verdict::Agree));
        assert!(matches!(classify(&oracle, &o1), Verdict::Agree));
        assert!(is_disagree(classify(&oracle, &o2)));
        assert!(matches!(classify(&oracle, &o3), Verdict::Agree));
    }

    #[test]
    fn native_compile_preserves_optimization_preview_and_real_std_settings() {
        let workdir = create_workdir().expect("temporary workdir");
        let std_path = workdir.path().join("std-fixture");
        std::fs::create_dir(&std_path).expect("create std fixture");
        let compiler = fake_compiler(
            workdir.path(),
            "#!/bin/sh\n\
             [ \"$1\" = -O3 ] || exit 21\n\
             [ \"$5\" = --preview ] || exit 22\n\
             [ \"$6\" = test_infra ] || exit 23\n\
             [ -d \"$RUE_STD_PATH\" ] || exit 24\n\
             printf '#!/bin/sh\\nexit 0\\n' > prog\n\
             chmod +x prog\n",
        );
        let previews = vec!["test_infra".to_string()];
        let result = compile_and_run(
            &compiler,
            workdir.path(),
            "fn main() -> i32 { 0 }",
            CompileOptions {
                optimization: OptimizationLevel::O3,
                previews: &previews,
                std_path: Some(&std_path),
                compile_timeout: Duration::from_secs(5),
                runtime_timeout: Duration::from_secs(5),
            },
        )
        .expect("run fake compiler");
        assert!(
            matches!(result, Compiled::Ran { exit: 0, .. }),
            "{}",
            describe(&result)
        );
    }

    #[test]
    fn agrees_when_exit_and_stdout_match() {
        let v = classify(&oc(42, "7\n"), &ran(42, "7\n"));
        assert!(matches!(v, Verdict::Agree));
    }

    #[test]
    fn unexpected_stderr_on_normal_exit_fails_closed() {
        let compiled = Compiled::Ran {
            exit: 42,
            stdout: b"7\n".to_vec(),
            stdout_truncated: false,
            stderr: "unmodeled native diagnostic\n".to_string(),
            stderr_truncated: false,
        };
        assert!(is_disagree(classify(&oc(42, "7\n"), &compiled)));
    }

    #[test]
    fn detects_exit_mismatch() {
        // The planted-bug shape: same stdout, wrong exit code.
        let v = classify(&oc(42, ""), &ran(43, ""));
        assert!(is_disagree(v));
    }

    #[test]
    fn detects_stdout_mismatch() {
        let v = classify(&oc(0, "7\n"), &ran(0, "8\n"));
        assert!(is_disagree(v));

        // Trap-category agreement cannot hide an earlier stdout mismatch.
        let compiled = Compiled::Ran {
            exit: 101,
            stdout: b"unexpected\n".to_vec(),
            stdout_truncated: false,
            stderr: "error: integer overflow\n".to_string(),
            stderr_truncated: false,
        };
        assert!(is_disagree(classify(
            &trap(TrapKind::ArithmeticOverflow),
            &compiled
        )));
    }

    #[test]
    fn invalid_native_stdout_bytes_cannot_agree_or_hide_diagnostics() {
        let oracle = oc_bytes(0, &[0xff]);
        let same = Compiled::Ran {
            exit: 0,
            stdout: vec![0xff],
            stdout_truncated: false,
            stderr: String::new(),
            stderr_truncated: false,
        };
        assert!(matches!(classify(&oracle, &same), Verdict::Agree));

        let compiled = Compiled::Ran {
            exit: 0,
            stdout: vec![0xfe],
            stdout_truncated: false,
            stderr: String::new(),
            stderr_truncated: false,
        };
        let Verdict::Disagree(reason) = classify(&oracle, &compiled) else {
            panic!("distinct invalid stdout bytes must disagree")
        };
        assert!(reason.contains("stdout:"), "diagnostic: {reason}");
        assert!(reason.contains("\\xff"), "diagnostic: {reason}");
        assert!(reason.contains("\\xfe"), "diagnostic: {reason}");

        let disagreement = Disagreement {
            seed: 1,
            optimization: OptimizationLevel::O1,
            timeout_secs: 1,
            source: "fn main() -> i32 { 0 }\n".to_string(),
            oracle_exit: oracle.exit_code,
            oracle_stdout: oracle.stdout_bytes,
            oracle_stderr: oracle.stderr,
            oracle_panic: oracle.panic,
            compiled: describe(&compiled),
            reason,
        };
        let rendered = disagreement.render();
        assert!(rendered.contains("\\xff"), "rendered: {rendered}");
        assert!(rendered.contains("\\xfe"), "rendered: {rendered}");
        let persisted = disagreement_repro_contents(&disagreement);
        assert!(persisted.contains("\\xff"), "persisted: {persisted}");
        assert!(persisted.contains("\\xfe"), "persisted: {persisted}");
    }

    #[test]
    fn detects_wrong_trap_at_exit_101() {
        // RUE-339: the false-AGREE this fix closes. Both engines exit 101 with
        // identical (empty) stdout, but the oracle expected an arithmetic
        // overflow while the compiled binary trapped on a bounds check — a
        // miscompile that the old exit-code-only comparator scored as Agree.
        let v = classify(
            &trap(TrapKind::ArithmeticOverflow),
            &ran_trap("error: index out of bounds\n"),
        );
        assert!(is_disagree(v));
    }

    #[test]
    fn agrees_when_trap_categories_match() {
        // Same cause on both sides -> genuine agreement, not a false-disagree.
        let v = classify(
            &trap(TrapKind::DivisionByZero),
            &ran_trap("error: division by zero\n"),
        );
        assert!(matches!(v, Verdict::Agree));
        let v = classify(
            &trap(TrapKind::InvalidUtf8),
            &ran_trap("error: invalid UTF-8\n"),
        );
        assert!(matches!(v, Verdict::Agree));
        // `integer cast overflow` must not be confused with `integer overflow`.
        let v = classify(
            &trap(TrapKind::IntegerCastOverflow),
            &ran_trap("error: integer cast overflow\n"),
        );
        assert!(matches!(v, Verdict::Agree));
        let v = classify(
            &trap(TrapKind::IntegerCastOverflow),
            &ran_trap("error: integer overflow\n"),
        );
        assert!(is_disagree(v));
        let v = classify(
            &trap(TrapKind::UserPanic),
            &ran_trap("panic: user message\n"),
        );
        assert!(matches!(v, Verdict::Agree));
        let v = classify(
            &trap(TrapKind::AssertionFailure),
            &ran_trap("assertion failed\n"),
        );
        assert!(matches!(v, Verdict::Agree));
    }

    #[test]
    fn same_trap_category_cannot_hide_a_stderr_message_mismatch() {
        let v = classify(
            &trap(TrapKind::UserPanic),
            &ran_trap("panic: different message\n"),
        );
        assert!(matches!(
            v,
            Verdict::Disagree(reason) if reason.contains("stderr:")
        ));
    }

    #[test]
    fn truncated_stderr_prefix_cannot_manufacture_agreement() {
        let oracle = Outcome {
            exit_code: 0,
            stdout: String::new(),
            stdout_bytes: Vec::new(),
            stderr: "retained prefix".to_string(),
            panic: None,
        };
        let compiled = Compiled::Ran {
            exit: 0,
            stdout: Vec::new(),
            stdout_truncated: false,
            stderr: oracle.stderr.clone(),
            stderr_truncated: true,
        };
        assert!(matches!(
            classify(&oracle, &compiled),
            Verdict::Disagree(reason) if reason.contains("stderr exceeded")
        ));
    }

    #[test]
    fn wrong_or_one_sided_trap_causes_fail_closed() {
        let v = classify(
            &trap(TrapKind::ArithmeticOverflow),
            &ran_trap("panic: boom\n"),
        );
        assert!(is_disagree(v));
        let v = classify(
            &trap(TrapKind::Unreachable),
            &ran_trap("error: integer overflow\n"),
        );
        assert!(is_disagree(v));
        let v = classify(&oc(101, ""), &ran_trap("error: index out of bounds\n"));
        assert!(is_disagree(v));
        let v = classify(&oc(101, ""), &ran_trap("panic: boom\n"));
        assert!(is_disagree(v));
        let v = classify(&trap(TrapKind::ArithmeticOverflow), &ran(101, ""));
        assert!(is_disagree(v));

        // A normal return of 101 with no trap evidence on either side remains
        // an ordinary agreeing program.
        let v = classify(&oc(101, ""), &ran(101, ""));
        assert!(matches!(v, Verdict::Agree));
    }

    #[test]
    fn compile_fail_crash_and_both_timeouts_are_disagreements() {
        // Oracle produced a clean outcome, so any non-Ran compiled result is a
        // divergence worth reporting.
        assert!(is_disagree(classify(
            &oc(0, ""),
            &Compiled::CompileRejected {
                exit: 1,
                stderr: "boom".into(),
            }
        )));
        assert!(is_disagree(classify(
            &oc(0, ""),
            &Compiled::CompileCrash {
                signal: 6,
                stderr: String::new(),
            }
        )));
        assert!(is_disagree(classify(
            &oc(0, ""),
            &Compiled::CompileIce("compiler panicked".into())
        )));
        assert!(is_disagree(classify(&oc(0, ""), &Compiled::CompileTimeout)));
        assert!(is_disagree(classify(&oc(0, ""), &Compiled::Crash(11))));
        assert!(is_disagree(classify(&oc(0, ""), &Compiled::Timeout)));

        // A native crash or timeout is still a disagreement when the oracle
        // expected a trap; neither is evidence for the same termination cause.
        assert!(is_disagree(classify(
            &trap(TrapKind::ArithmeticOverflow),
            &Compiled::Crash(4)
        )));
        assert!(is_disagree(classify(
            &trap(TrapKind::ArithmeticOverflow),
            &Compiled::Timeout
        )));
    }

    #[test]
    fn generated_oracle_errors_remain_typed_contract_failures() {
        // A frontend rejection is a generator compile-contract failure, not an
        // oracle Unsupported skip and not a native `Compiled::CompileRejected`.
        let compile_error =
            rue_oracle::run_source("fn main(").expect_err("invalid Rue must not compile");
        let compile_failure = GeneratorContractFailure::from_run_source(compile_error);
        assert!(matches!(
            compile_failure,
            GeneratorContractFailure::Compile(detail) if !detail.is_empty()
        ));

        // A compiled-but-unmodeled program is the other distinct contract
        // failure class. Keep the variant typed so summaries/repros cannot
        // collapse it back into the old generic skip path.
        let unsupported = rue_oracle::run_source(
            "fn main() -> i32 { let n: u32 = @random_u32(); if n == 0 { 0 } else { 1 } }",
        )
        .expect_err("randomness must remain outside the deterministic oracle");
        let failure = GeneratorContractFailure::from_run_source(unsupported);
        let GeneratorContractFailure::Unsupported(unsupported) = &failure else {
            panic!("compiled source must produce the typed Unsupported variant");
        };
        assert_eq!(
            unsupported.kind(),
            rue_oracle::UnsupportedKind::ExternalDependency(
                rue_oracle::ExternalDependencyKind::RandomU32
            )
        );
        assert_eq!(unsupported.detail(), "intrinsic @random_u32");

        let finding = GeneratorContractFinding {
            seed: 9,
            source: "fn main() -> i32 { 9 }\n".to_string(),
            failure,
        };
        assert!(
            finding
                .render()
                .contains("unsupported (ExternalDependency(RandomU32))")
        );
        assert!(
            generator_contract_repro_contents(&finding)
                .contains("// oracle cause: ExternalDependency(RandomU32)\n")
        );
    }

    #[test]
    fn generated_position_assertion_is_a_contract_failure() {
        let oracle = trap(TrapKind::AssertionFailure);
        let failure = generated_invariant_failure(&oracle).expect("assertion must be classified");
        assert!(matches!(
            &failure,
            GeneratorContractFailure::Invariant(detail) if detail == "position twin mismatch"
        ));

        let finding = GeneratorContractFinding {
            seed: 41,
            source: "fn main() -> i32 { @assert(false); 0 }\n".to_string(),
            failure,
        };
        assert!(
            finding
                .render()
                .contains("invariant: position twin mismatch")
        );
        let repro = generator_contract_repro_contents(&finding);
        assert!(repro.contains("// failure kind: invariant\n"));
        assert!(repro.contains("// detail: position twin mismatch\n"));
        assert!(repro.ends_with("\n\nfn main() -> i32 { @assert(false); 0 }\n"));

        // A native-only assertion failure is not a generated invariant: it is
        // still an ordinary differential disagreement against a clean oracle.
        assert!(is_disagree(classify(
            &oc(0, ""),
            &ran_trap("assertion failed\n")
        )));
    }

    #[test]
    fn generator_contract_repro_is_deterministic_and_comment_safe() {
        let finding = GeneratorContractFinding {
            seed: 17,
            source: "fn main() -> i32 { 17 }\n".to_string(),
            failure: GeneratorContractFailure::Compile(
                "first diagnostic\nfn injected() -> i32 { 0 }\rsecond".to_string(),
            ),
        };

        let contents = generator_contract_repro_contents(&finding);
        assert_eq!(contents, generator_contract_repro_contents(&finding));
        assert!(
            contents
                .contains("// detail: first diagnostic\\nfn injected() -> i32 { 0 }\\rsecond\n")
        );
        assert!(
            !contents.contains("\nfn injected"),
            "diagnostic text must never escape its metadata comment"
        );
        assert!(
            contents.ends_with("\n\nfn main() -> i32 { 17 }\n"),
            "the exact generated source follows the comment-only metadata"
        );
    }

    #[test]
    fn disagreement_repro_also_escapes_multiline_metadata() {
        let disagreement = Disagreement {
            seed: 23,
            optimization: OptimizationLevel::O2,
            timeout_secs: 3,
            source: "fn main() -> i32 { 23 }\n".to_string(),
            oracle_exit: 101,
            oracle_stdout: Vec::new(),
            oracle_stderr: "error: integer cast overflow\n".to_string(),
            oracle_panic: Some(TrapKind::IntegerCastOverflow),
            compiled: "compile-fail: first\rsecond".to_string(),
            reason: "wrong exit\nconst injected = 1;".to_string(),
        };

        let rendered = disagreement.render();
        assert!(rendered.contains("--timeout 3"));
        assert!(rendered.contains("panic=Some(IntegerCastOverflow)"));
        let contents = disagreement_repro_contents(&disagreement);
        assert!(contents.contains("// reason: wrong exit\\nconst injected = 1;\n"));
        assert!(contents.contains("panic=Some(IntegerCastOverflow)"));
        assert!(contents.contains("// compiled: compile-fail: first\\rsecond\n"));
        assert!(
            contents
                .contains("// regenerate: rue-oracle-diff fuzz --start 23 --seeds 1 --timeout 3\n")
        );
        assert!(!contents.contains("\nconst injected"));
        assert!(contents.ends_with("\n\nfn main() -> i32 { 23 }\n"));
    }

    #[test]
    fn compiler_timeout_is_bounded_distinct_and_never_runs_a_stale_binary() {
        let workdir = create_workdir().expect("temporary workdir");
        // If compile_and_run forgot to remove the previous iteration's output,
        // a timed-out compiler could accidentally run this stale executable.
        let stale_binary = workdir.path().join("prog");
        std::fs::write(&stale_binary, "#!/bin/sh\nexit 0\n").expect("write stale binary");
        make_executable(&stale_binary);
        let compiler = fake_compiler(workdir.path(), "#!/bin/sh\nexec sleep 30\n");

        let start = Instant::now();
        let result = compile_and_run(
            &compiler,
            workdir.path(),
            "fn main() -> i32 { 0 }",
            CompileOptions {
                optimization: OptimizationLevel::O2,
                previews: &[],
                std_path: None,
                compile_timeout: Duration::from_millis(200),
                runtime_timeout: Duration::from_secs(5),
            },
        )
        .expect("spawn fake compiler");

        assert!(
            start.elapsed() < Duration::from_secs(5),
            "compiler kill/reap/join must return promptly"
        );
        assert!(
            matches!(result, Compiled::CompileTimeout),
            "compiler hangs must be distinct from runtime hangs: {}",
            describe(&result)
        );
        assert_eq!(describe(&result), "compile-timeout");
        assert!(
            !stale_binary.exists(),
            "a timed-out compile must not leave the old binary runnable"
        );
    }

    #[test]
    fn compiler_timeout_kills_descendants_holding_captured_pipes() {
        let workdir = create_workdir().expect("temporary workdir");
        let compiler = fake_compiler(
            workdir.path(),
            "#!/bin/sh\n(sh -c 'trap \"\" TERM; sleep 30') &\nsleep 30\n",
        );

        let start = Instant::now();
        let result = compile_and_run(
            &compiler,
            workdir.path(),
            "fn main() -> i32 { 0 }",
            CompileOptions {
                optimization: OptimizationLevel::O1,
                previews: &[],
                std_path: None,
                compile_timeout: Duration::from_millis(200),
                runtime_timeout: Duration::from_secs(5),
            },
        )
        .expect("spawn descendant compiler fixture");

        assert!(matches!(result, Compiled::CompileTimeout));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "process-group kill must close descendant-held pipes before reader joins"
        );
    }

    #[test]
    fn successful_leader_exit_cleans_descendants_without_becoming_a_timeout() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("(trap '' TERM; sleep 30) & exit 7");
        let started = Instant::now();
        let result = run_with_timeout(command, Duration::from_secs(5))
            .expect("run successful leader with a pipe-holding descendant");

        assert!(matches!(result, Compiled::Ran { exit: 7, .. }));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a successful direct child remains authoritative while its process group is cleaned"
        );
    }

    #[test]
    fn stale_artifact_errors_and_missing_or_non_executable_outputs_fail_closed() {
        let workdir = create_workdir().expect("temporary workdir");
        std::fs::create_dir(workdir.path().join("prog")).expect("create stale directory");
        let compiler = fake_compiler(workdir.path(), "#!/bin/sh\nexit 0\n");
        let options = || CompileOptions {
            optimization: OptimizationLevel::O1,
            previews: &[],
            std_path: None,
            compile_timeout: Duration::from_secs(5),
            runtime_timeout: Duration::from_secs(5),
        };
        let error = compile_and_run(
            &compiler,
            workdir.path(),
            "fn main() -> i32 { 0 }",
            options(),
        )
        .expect_err("a stale directory cannot be ignored as a missing artifact");
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);

        std::fs::remove_dir(workdir.path().join("prog")).expect("remove stale directory");
        let error = compile_and_run(
            &compiler,
            workdir.path(),
            "fn main() -> i32 { 0 }",
            options(),
        )
        .expect_err("compiler success without output must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

        let compiler = fake_compiler(
            workdir.path(),
            "#!/bin/sh\nprintf 'not executable\\n' > prog\n",
        );
        let error = compile_and_run(
            &compiler,
            workdir.path(),
            "fn main() -> i32 { 0 }",
            options(),
        )
        .expect_err("non-executable compiler output must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn compiler_crashes_and_ices_are_not_compile_rejections() {
        let workdir = create_workdir().expect("temporary workdir");
        let options = || CompileOptions {
            optimization: OptimizationLevel::O1,
            previews: &[],
            std_path: None,
            compile_timeout: Duration::from_secs(5),
            runtime_timeout: Duration::from_secs(5),
        };
        let crashing = fake_compiler(workdir.path(), "#!/bin/sh\nkill -TERM $$\n");
        let crash = compile_and_run(
            &crashing,
            workdir.path(),
            "fn main() -> i32 { 0 }",
            options(),
        )
        .expect("run crashing compiler");
        assert!(matches!(crash, Compiled::CompileCrash { signal: 15, .. }));

        let icing = fake_compiler(
            workdir.path(),
            "#!/bin/sh\necho 'internal compiler error: planted' 1>&2\nexit 0\n",
        );
        let ice = compile_and_run(&icing, workdir.path(), "fn main() -> i32 { 0 }", options())
            .expect("run ICE compiler");
        assert!(matches!(ice, Compiled::CompileIce(_)));
    }

    #[test]
    fn compiler_stderr_is_drained_while_retention_stays_bounded() {
        let workdir = create_workdir().expect("temporary workdir");
        let compiler = fake_compiler(
            workdir.path(),
            "#!/bin/sh\nyes e | head -c 200000 1>&2\nexit 9\n",
        );

        let result = compile_and_run(
            &compiler,
            workdir.path(),
            "fn main() -> i32 { 0 }",
            CompileOptions {
                optimization: OptimizationLevel::O3,
                previews: &[],
                std_path: None,
                compile_timeout: Duration::from_secs(30),
                runtime_timeout: Duration::from_secs(5),
            },
        )
        .expect("run fake compiler");

        match result {
            Compiled::CompileRejected { exit: 9, stderr } => {
                assert!(!stderr.is_empty(), "some compiler stderr is retained");
                assert_eq!(
                    stderr.len(),
                    MAX_STDERR_BYTES,
                    "large diagnostics must be fully drained while retention stops at the cap"
                );
            }
            other => panic!(
                "large compiler diagnostics must be a compile failure, not a timeout: {}",
                describe(&other)
            ),
        }
    }

    #[test]
    fn temporary_workdir_is_removed_on_drop() {
        let path = {
            let workdir = create_workdir().expect("temporary workdir");
            let path = workdir.path().to_path_buf();
            std::fs::write(path.join("marker"), "cleanup probe").expect("write marker");
            assert!(path.exists());
            path
        };

        assert!(
            !path.exists(),
            "dropping the harness workdir must remove generated artifacts"
        );
    }

    #[test]
    fn stdout_below_cap_is_drained_and_compared_exactly() {
        // RUE-338: a program that writes far more than the OS pipe capacity
        // (~64KB) must be drained concurrently while it runs. Before the fix the
        // pipe filled, the writer blocked on `write()`, `try_wait` never saw an
        // exit, and the 10s timeout fabricated a `Compiled::Timeout` (→ a false
        // Disagree). Emit ~200KB (below MAX_STDOUT_BYTES), capture every byte, and
        // prove the complete output still participates in exact agreement.
        let n = 200_000usize;
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!("yes y | head -c {n}"));
        let result = run_with_timeout(cmd, Duration::from_secs(30)).expect("spawn");
        match &result {
            Compiled::Ran {
                exit,
                stdout,
                stdout_truncated,
                ..
            } => {
                assert_eq!(*exit, 0, "pipeline should exit cleanly");
                assert!(!*stdout_truncated, "below-cap stdout must stay exact");
                assert_eq!(
                    stdout.len(),
                    n,
                    "full stdout must be captured, not truncated"
                );
                assert!(matches!(
                    classify(&oc_bytes(0, stdout), &result),
                    Verdict::Agree
                ));
            }
            other => panic!("expected Ran with full stdout, got {}", describe(other)),
        }
    }

    #[test]
    fn stdout_above_cap_is_drained_capped_and_cannot_agree_on_its_prefix() {
        let n = MAX_STDOUT_BYTES + 100_000;
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!("yes y | head -c {n}"));
        let result = run_with_timeout(cmd, Duration::from_secs(30)).expect("spawn");

        match &result {
            Compiled::Ran {
                exit,
                stdout,
                stdout_truncated,
                ..
            } => {
                assert_eq!(*exit, 0);
                assert_eq!(
                    stdout.len(),
                    MAX_STDOUT_BYTES,
                    "retained stdout must be capped"
                );
                assert!(*stdout_truncated, "overflow must remain explicit");

                // Even an oracle outcome equal to the retained prefix cannot
                // agree: additional compiled output was observed and drained.
                let verdict = classify(&oc_bytes(0, stdout), &result);
                assert!(
                    matches!(verdict, Verdict::Disagree(reason) if reason.contains("capture limit")),
                    "stdout overflow must disagree before prefix comparison"
                );
            }
            other => panic!("expected capped Ran output, got {}", describe(other)),
        }
    }

    #[test]
    fn infinite_stdout_is_drained_with_bounded_retention_until_timeout() {
        // Run the writer directly so killing the timed child closes the only
        // pipe write end. This is the adversarial always-on-smoke case: a
        // miscompiled binary can print forever, but retained memory stays at
        // MAX_STDOUT_BYTES and kill/reap/join still returns promptly.
        let mut cmd = Command::new("yes");
        cmd.arg("y");
        let start = Instant::now();
        let result = run_with_timeout(cmd, Duration::from_millis(200)).expect("spawn");

        assert!(
            start.elapsed() < Duration::from_secs(5),
            "an infinite writer must remain bounded by the process timeout"
        );
        assert!(
            matches!(result, Compiled::Timeout),
            "infinite stdout must time out without deadlock or unbounded retention: {}",
            describe(&result)
        );
    }

    #[test]
    fn large_stderr_does_not_deadlock() {
        // The stderr cap must not reintroduce the deadlock: even though we retain
        // only MAX_STDERR_BYTES bytes, the pipe is drained to EOF, so a program spewing
        // >64KB to stderr still terminates as `Ran` (RUE-338).
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("yes e | head -c 200000 1>&2");
        let result = run_with_timeout(cmd, Duration::from_secs(30)).expect("spawn");
        match result {
            Compiled::Ran {
                exit,
                stderr,
                stderr_truncated,
                ..
            } => {
                assert_eq!(exit, 0);
                assert!(!stderr.is_empty(), "some stderr should be retained");
                assert!(stderr_truncated, "discarded stderr must remain explicit");
                assert!(
                    stderr.len() <= MAX_STDERR_BYTES,
                    "stderr retained ({}) must not exceed the cap {MAX_STDERR_BYTES}",
                    stderr.len()
                );
            }
            other => panic!("expected Ran, got {}", describe(&other)),
        }
    }

    #[test]
    fn read_capped_drains_to_eof_but_keeps_only_cap() {
        // The drain-vs-keep contract in isolation: given more bytes than the cap,
        // we consume all of them (Cursor reaches EOF) but retain exactly `cap`.
        let data = vec![b'z'; 100_000];
        let kept = read_capped(std::io::Cursor::new(data), MAX_STDERR_BYTES);
        assert_eq!(kept.bytes.len(), MAX_STDERR_BYTES);
        assert!(kept.bytes.iter().all(|&b| b == b'z'));
        assert!(kept.truncated);
        // Fewer bytes than the cap: keep them all.
        let kept = read_capped(std::io::Cursor::new(vec![b'q'; 10]), MAX_STDERR_BYTES);
        assert_eq!(kept.bytes.len(), 10);
        assert!(!kept.truncated);
        // Exactly the cap is still complete, not truncated.
        let kept = read_capped(
            std::io::Cursor::new(vec![b'x'; MAX_STDERR_BYTES]),
            MAX_STDERR_BYTES,
        );
        assert_eq!(kept.bytes.len(), MAX_STDERR_BYTES);
        assert!(!kept.truncated);
    }

    #[test]
    fn timeout_still_reported_for_a_hung_child() {
        // A genuine non-terminating program must still yield `Timeout` (and the
        // reader threads must be joined, not leaked). We exec `sleep` directly
        // (not via `sh -c`): the harness always runs a single-process compiled
        // binary, so killing the child closes every pipe write-end, the readers
        // hit EOF, and the post-kill join returns at once. `sleep` writes
        // nothing, so this is the honest timeout path, distinct from the false
        // one the large-output tests guard against.
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let start = Instant::now();
        let result = run_with_timeout(cmd, Duration::from_millis(200)).expect("spawn");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "kill-then-join must return promptly, not block on the child's full runtime"
        );
        assert!(
            matches!(result, Compiled::Timeout),
            "a truly hung child must report Timeout, got {}",
            describe(&result)
        );
    }

    #[test]
    fn args_parse_flags() {
        let cfg = parse_args(&[
            "--start=5".into(),
            "--seeds".into(),
            "12".into(),
            "--timeout=3".into(),
        ])
        .expect("parse");
        assert_eq!(cfg.start, 5);
        assert_eq!(cfg.seeds, 12);
        assert_eq!(cfg.timeout, Duration::from_secs(3));
    }

    #[test]
    fn args_reject_vacuous_or_wrapped_seed_ranges() {
        let zero = parse_args(&["--seeds=0".into()])
            .err()
            .expect("zero seeds must fail closed");
        assert!(zero.contains("greater than zero"));

        let wrapped = parse_args(&[format!("--start={}", u64::MAX), "--seeds=1".into()])
            .err()
            .expect("a seed range must not wrap around u64::MAX");
        assert!(wrapped.contains("exceeds the u64 seed range"));
    }

    #[test]
    fn generated_batch_fails_for_either_finding_class() {
        assert!(generated_batch_passes(0, 0));
        assert!(!generated_batch_passes(1, 0));
        assert!(!generated_batch_passes(0, 1));
        assert!(!generated_batch_passes(1, 1));
    }
}
