//! # Differential fuzzer (`rue-oracle-diff fuzz`) — RUE-247
//!
//! Generate random **valid** Rue programs (see [`crate::gen`]) and run each one
//! through *both* engines:
//!
//! 1. the [`rue_oracle`] reference interpreter (`run_source`), and
//! 2. the real compiler + the produced native binary.
//!
//! Then compare the observable behavior — process exit code and `@dbg` stdout.
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

use crate::generator;
use rue_oracle::{RunSourceError, run_source};
use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Outcome of compiling and running a generated program natively.
enum Compiled {
    /// The compiler rejected a program the oracle's frontend accepted — an ICE
    /// or backend gap (carries the compiler's stderr, truncated).
    CompileFail(String),
    /// The compiler did not terminate within the per-phase timeout.
    CompileTimeout,
    /// The binary ran to completion: process exit code + captured stdout +
    /// captured stderr. `stderr` carries the runtime's trap message (e.g.
    /// `"error: integer overflow\n"`) so that when both engines exit 101 we can
    /// compare *which* trap fired, not just that one did (RUE-339).
    Ran {
        exit: i32,
        stdout: String,
        /// More stdout existed beyond [`STDOUT_CAP`]; `stdout` is only a prefix.
        stdout_truncated: bool,
        stderr: String,
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
    Unsupported(String),
}

impl GeneratorContractFailure {
    fn from_run_source(error: RunSourceError) -> Self {
        match error {
            RunSourceError::Compile(errors) => Self::Compile(format!("{errors:#?}")),
            RunSourceError::Unsupported(unsupported) => Self::Unsupported(unsupported.0),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Compile(_) => "compile",
            Self::Unsupported(_) => "unsupported",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Compile(detail) | Self::Unsupported(detail) => detail,
        }
    }
}

struct GeneratorContractFinding {
    seed: u64,
    source: String,
    failure: GeneratorContractFailure,
}

impl GeneratorContractFinding {
    fn render(&self) -> String {
        format!(
            "\n\u{2717} GENERATOR CONTRACT FAILURE (seed {seed})\n  {kind}: {detail}\n  \
             --- source (regenerate with `fuzz --start {seed} --seeds 1`) ---\n{source}",
            seed = self.seed,
            kind = self.failure.kind(),
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            start: 0,
            seeds: 200,
            timeout: Duration::from_secs(10),
            crash_dir: PathBuf::from("crates/rue-fuzz/crashes"),
            verbose: false,
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

        let oracle = match run_source(&source) {
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
                    report_repro_write_error(&cfg.crash_dir, seed, &error);
                }
                generator_contract_failures.push(finding);
                continue;
            }
            Ok(o) => o,
        };

        let compiled = match compile_and_run(&rue, workdir.path(), &source, cfg.timeout) {
            Ok(c) => c,
            Err(e) => {
                // An infrastructure failure (couldn't invoke the tools) is fatal
                // — better to stop loudly than silently pass.
                eprintln!("seed {seed}: harness error: {e}");
                return ExitCode::FAILURE;
            }
        };

        match classify(&oracle, &compiled) {
            Verdict::Agree => {
                agree += 1;
                if cfg.verbose {
                    println!("  seed {seed}: agree (exit {})", oracle.exit_code);
                }
            }
            Verdict::Disagree(reason) => {
                let d = Disagreement {
                    seed,
                    timeout_secs: cfg.timeout.as_secs(),
                    source: source.clone(),
                    oracle_exit: oracle.exit_code,
                    oracle_stdout: oracle.stdout.clone(),
                    oracle_panic: oracle.panic.clone(),
                    compiled: describe(&compiled),
                    reason,
                };
                eprintln!("{}", d.render());
                if let Err(error) = save_repro(&cfg.crash_dir, &d) {
                    report_repro_write_error(&cfg.crash_dir, seed, &error);
                }
                disagreements.push(d);
            }
        }
    }

    let total = agree + generator_contract_failures.len() as u32 + disagreements.len() as u32;
    println!("\n=== summary over {total} generated programs ===");
    println!("  agree:            {agree}");
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
enum Verdict {
    Agree,
    Disagree(String),
}

/// A category of runtime trap, shared vocabulary between the oracle's panic
/// reason ([`rue_oracle::Outcome::panic`]) and the compiled runtime's stderr
/// message (`rue-runtime/src/error.rs`). Every defined Rue trap exits 101, so
/// the exit code alone cannot tell two traps apart; this is what lets
/// [`classify`] catch a miscompile that traps for the *wrong reason* (RUE-339).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrapCategory {
    DivideByZero,
    Overflow,
    IntCastOverflow,
    Bounds,
}

/// Classify the oracle's panic reason (the exact strings raised in
/// `rue-oracle/src/lib.rs`) into a [`TrapCategory`]. Reasons we don't map (e.g.
/// `"reached unreachable"`, which has no distinct runtime message class) return
/// `None`, so the caller falls back to plain exit-code agreement rather than
/// inventing a disagreement.
fn oracle_trap_category(reason: &str) -> Option<TrapCategory> {
    match reason {
        // The runtime routes both `x / 0` and `x % 0` to the single
        // `error: division by zero` handler, so both oracle reasons map here.
        "divide by zero" | "remainder by zero" => Some(TrapCategory::DivideByZero),
        "arithmetic overflow" => Some(TrapCategory::Overflow),
        "integer cast overflow" => Some(TrapCategory::IntCastOverflow),
        "index out of bounds" => Some(TrapCategory::Bounds),
        _ => None,
    }
}

/// Classify the compiled runtime's stderr message into a [`TrapCategory`] by its
/// distinguishing class substring (the messages are defined in
/// `rue-runtime/src/error.rs`). We match on the class rather than the exact
/// bytes so wording tweaks don't cause false-disagrees, and check the more
/// specific `integer cast overflow` before `integer overflow`. An unrecognized
/// message (a `@panic`/`@assert`/UTF-8/unreachable trap, or future wording)
/// returns `None`, falling back to exit-code agreement.
fn runtime_trap_category(stderr: &str) -> Option<TrapCategory> {
    if stderr.contains("integer cast overflow") {
        Some(TrapCategory::IntCastOverflow)
    } else if stderr.contains("integer overflow") {
        Some(TrapCategory::Overflow)
    } else if stderr.contains("division by zero") {
        Some(TrapCategory::DivideByZero)
    } else if stderr.contains("index out of bounds") {
        Some(TrapCategory::Bounds)
    } else {
        None
    }
}

fn classify(oracle: &rue_oracle::Outcome, compiled: &Compiled) -> Verdict {
    match compiled {
        Compiled::Ran {
            exit,
            stdout,
            stdout_truncated,
            stderr,
        } => {
            // A retained prefix can equal the oracle's complete output while
            // hiding additional compiled output. Fail before comparing that
            // prefix so truncation can never manufacture agreement.
            if *stdout_truncated {
                return Verdict::Disagree(format!(
                    "compiled stdout exceeded the {STDOUT_CAP}-byte capture limit; \
                     retained prefix cannot prove agreement"
                ));
            }
            let exit_ok = oracle.exit_code == *exit;
            let stdout_ok = &oracle.stdout == stdout;
            if !exit_ok || !stdout_ok {
                let mut r = String::new();
                if !exit_ok {
                    r += &format!("exit: oracle {} vs compiled {exit}; ", oracle.exit_code);
                }
                if !stdout_ok {
                    r += &format!(
                        "stdout: oracle {:?} vs compiled {:?}",
                        oracle.stdout, stdout
                    );
                }
                return Verdict::Disagree(r);
            }
            // Exit code and stdout agree. When both engines ended in a runtime
            // trap (exit 101), also compare *which* trap fired: since every Rue
            // trap exits 101, a miscompile that traps for the wrong reason at
            // the same point would otherwise be a false-AGREE (RUE-339). We
            // compare category classes, not exact message bytes, to avoid
            // false-disagrees; if either side's cause can't be classified (e.g.
            // a `@panic`/unreachable trap, or an unmapped oracle reason) we fall
            // back to the exit-code agreement above — a documented residual
            // blind spot, never a manufactured disagreement.
            if *exit == 101
                && let Some(want) = oracle.panic.as_deref().and_then(oracle_trap_category)
                && let Some(got) = runtime_trap_category(stderr)
                && want != got
            {
                return Verdict::Disagree(format!(
                    "panic category: oracle {want:?} vs compiled {got:?} \
                     (both exit 101; compiled stderr {:?})",
                    first_line(stderr)
                ));
            }
            Verdict::Agree
        }
        Compiled::CompileFail(stderr) => Verdict::Disagree(format!(
            "compiler rejected a program the oracle accepted (possible ICE/backend gap): {}",
            first_line(stderr)
        )),
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

fn compile_and_run(
    rue: &Path,
    dir: &Path,
    source: &str,
    timeout: Duration,
) -> std::io::Result<Compiled> {
    let src_path = dir.join("prog.rue");
    std::fs::write(&src_path, source)?;
    let bin_path = dir.join("prog");
    // Best-effort remove of a stale binary so a compile failure can't run last
    // iteration's executable.
    let _ = std::fs::remove_file(&bin_path);

    let mut compile_cmd = Command::new(rue);
    compile_cmd
        .arg("prog.rue")
        .arg("-o")
        .arg("prog")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    match run_process_with_timeout(compile_cmd, timeout)? {
        ProcessOutcome::TimedOut => return Ok(Compiled::CompileTimeout),
        ProcessOutcome::Exited { status, stderr, .. } if !status.success() => {
            return Ok(Compiled::CompileFail(stderr));
        }
        ProcessOutcome::Exited { .. } => {}
    }

    // Run the produced binary directly with a manual timeout so we can read the
    // child's own exit code / terminating signal unambiguously. stderr is
    // captured (not `Stdio::null`) so a trap's message is available to
    // `classify` for panic-category comparison (RUE-339).
    let mut cmd = Command::new(&bin_path);
    cmd.current_dir(dir);
    run_with_timeout(cmd, timeout)
}

/// Maximum stdout bytes retained from a generated binary. Output past this
/// bound is still drained, and the explicit truncation bit makes the run a
/// disagreement rather than comparing an incomplete prefix as if it were exact.
const STDOUT_CAP: usize = 256 * 1024;

/// Maximum stderr bytes retained from a run. Trap messages are short; the cap
/// only bounds how much we *keep* — the pipe is still drained to EOF (see
/// [`read_capped`]) so a chatty process can never deadlock the wait loop.
const STDERR_CAP: usize = 8192;

/// Result of one child process whose stdout/stderr were drained while it ran.
enum ProcessOutcome {
    Exited {
        status: ExitStatus,
        stdout: String,
        stdout_truncated: bool,
        stderr: String,
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
        stdout_pipe.map_or_else(CappedRead::default, |out| read_capped(out, STDOUT_CAP))
    });
    let stderr_reader = std::thread::spawn(move || {
        stderr_pipe.map_or_else(CappedRead::default, |err| read_capped(err, STDERR_CAP))
    });

    let start = Instant::now();
    let status: std::io::Result<Option<ExitStatus>> = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(Some(status)),
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
    // Lossy decode so garbage bytes from a miscompile surface as a stdout
    // mismatch rather than silently becoming empty (as `read_to_string` would).
    let stdout = String::from_utf8_lossy(&stdout_capture.bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_capture.bytes).into_owned();

    match status? {
        Some(status) => Ok(ProcessOutcome::Exited {
            status,
            stdout,
            stdout_truncated: stdout_capture.truncated,
            stderr,
        }),
        None => Ok(ProcessOutcome::TimedOut),
    }
}

fn terminate_and_reap(child: &mut Child) {
    // `kill` can race with a natural exit; `wait` still reaps either way. The
    // harness executes direct compiler/program children, not shell process
    // trees, so terminating this one process closes every captured write end.
    let _ = child.kill();
    let _ = child.wait();
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
        } => match status.code() {
            Some(code) => Compiled::Ran {
                exit: code,
                stdout,
                stdout_truncated,
                stderr,
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
    /// Per-phase compiler/native execution budget needed to replay timeouts.
    timeout_secs: u64,
    source: String,
    oracle_exit: i32,
    oracle_stdout: String,
    oracle_panic: Option<String>,
    compiled: String,
    reason: String,
}

impl Disagreement {
    fn render(&self) -> String {
        format!(
            "\n\u{2717} DISAGREEMENT (seed {seed})\n  {reason}\n  oracle:   exit={exit} panic={panic:?} stdout={stdout:?}\n  compiled: {compiled}\n  --- source (regenerate with `fuzz --start {seed} --seeds 1 --timeout {timeout_secs}`) ---\n{source}",
            seed = self.seed,
            timeout_secs = self.timeout_secs,
            reason = self.reason,
            exit = self.oracle_exit,
            panic = self.oracle_panic,
            stdout = self.oracle_stdout,
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
            ..
        } => {
            let label = if *stdout_truncated {
                "stdout-prefix"
            } else {
                "stdout"
            };
            format!("ran exit={exit} {label}={stdout:?} stdout-truncated={stdout_truncated}")
        }
        Compiled::CompileFail(e) => format!("compile-fail: {}", first_line(e)),
        Compiled::CompileTimeout => "compile-timeout".to_string(),
        Compiled::Crash(sig) => format!("crash signal={sig}"),
        Compiled::Timeout => "timeout".to_string(),
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

fn repro_path(dir: &Path, seed: u64) -> PathBuf {
    dir.join(format!("oracle-diff-seed-{seed}.rue"))
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
    push_repro_comment(&mut contents, "reason", &d.reason);
    push_repro_comment(
        &mut contents,
        "oracle",
        &format!(
            "exit={} panic={:?} stdout={:?}",
            d.oracle_exit, d.oracle_panic, d.oracle_stdout
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

fn write_repro(dir: &Path, seed: u64, contents: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(repro_path(dir, seed), contents)
}

fn report_repro_write_error(dir: &Path, seed: u64, error: &std::io::Error) {
    eprintln!(
        "seed {seed}: failed to save repro at {}: {error}",
        repro_path(dir, seed).display()
    );
}

/// Persist a repro so a CI failure uploads a concrete, self-contained program.
fn save_repro(dir: &Path, d: &Disagreement) -> std::io::Result<()> {
    write_repro(dir, d.seed, &disagreement_repro_contents(d))
}

fn save_generator_contract_repro(
    dir: &Path,
    finding: &GeneratorContractFinding,
) -> std::io::Result<()> {
    write_repro(
        dir,
        finding.seed,
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
            panic: None,
        }
    }

    /// An oracle outcome that ended in a runtime trap (exit 101) with `reason`.
    fn trap(reason: &str) -> Outcome {
        Outcome {
            exit_code: 101,
            stdout: String::new(),
            panic: Some(reason.to_string()),
        }
    }

    /// A `Compiled::Ran` with no captured stderr (for the non-trap tests).
    fn ran(exit: i32, stdout: &str) -> Compiled {
        Compiled::Ran {
            exit,
            stdout: stdout.to_string(),
            stdout_truncated: false,
            stderr: String::new(),
        }
    }

    /// A `Compiled::Ran` that trapped at exit 101 with the given stderr message.
    fn ran_trap(stderr: &str) -> Compiled {
        Compiled::Ran {
            exit: 101,
            stdout: String::new(),
            stdout_truncated: false,
            stderr: stderr.to_string(),
        }
    }

    fn is_disagree(v: Verdict) -> bool {
        matches!(v, Verdict::Disagree(_))
    }

    #[test]
    fn agrees_when_exit_and_stdout_match() {
        let v = classify(&oc(42, "7\n"), &ran(42, "7\n"));
        assert!(matches!(v, Verdict::Agree));
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
    }

    #[test]
    fn detects_wrong_trap_at_exit_101() {
        // RUE-339: the false-AGREE this fix closes. Both engines exit 101 with
        // identical (empty) stdout, but the oracle expected an arithmetic
        // overflow while the compiled binary trapped on a bounds check — a
        // miscompile that the old exit-code-only comparator scored as Agree.
        let v = classify(
            &trap("arithmetic overflow"),
            &ran_trap("error: index out of bounds\n"),
        );
        assert!(is_disagree(v));
    }

    #[test]
    fn agrees_when_trap_categories_match() {
        // Same cause on both sides -> genuine agreement, not a false-disagree.
        let v = classify(
            &trap("divide by zero"),
            &ran_trap("error: division by zero\n"),
        );
        assert!(matches!(v, Verdict::Agree));
        // The oracle's `remainder by zero` maps to the runtime's single
        // `division by zero` handler — must still agree (category, not bytes).
        let v = classify(
            &trap("remainder by zero"),
            &ran_trap("error: division by zero\n"),
        );
        assert!(matches!(v, Verdict::Agree));
        // `integer cast overflow` must not be confused with `integer overflow`.
        let v = classify(
            &trap("integer cast overflow"),
            &ran_trap("error: integer cast overflow\n"),
        );
        assert!(matches!(v, Verdict::Agree));
        let v = classify(
            &trap("integer cast overflow"),
            &ran_trap("error: integer overflow\n"),
        );
        assert!(is_disagree(v));
    }

    #[test]
    fn falls_back_when_cause_unclassifiable() {
        // An unmapped runtime message (e.g. a `@panic`/unreachable trap) must
        // NOT manufacture a disagreement — we fall back to exit-code agreement.
        let v = classify(&trap("arithmetic overflow"), &ran_trap("panic: boom\n"));
        assert!(matches!(v, Verdict::Agree));
        // Likewise an oracle reason we don't map (e.g. reached unreachable).
        let v = classify(
            &trap("reached unreachable"),
            &ran_trap("error: integer overflow\n"),
        );
        assert!(matches!(v, Verdict::Agree));
    }

    #[test]
    fn compile_fail_crash_and_both_timeouts_are_disagreements() {
        // Oracle produced a clean outcome, so any non-Ran compiled result is a
        // divergence worth reporting.
        assert!(is_disagree(classify(
            &oc(0, ""),
            &Compiled::CompileFail("boom".into())
        )));
        assert!(is_disagree(classify(&oc(0, ""), &Compiled::CompileTimeout)));
        assert!(is_disagree(classify(&oc(0, ""), &Compiled::Crash(11))));
        assert!(is_disagree(classify(&oc(0, ""), &Compiled::Timeout)));
    }

    #[test]
    fn generated_oracle_errors_remain_typed_contract_failures() {
        // A frontend rejection is a generator compile-contract failure, not an
        // oracle Unsupported skip and not a native `Compiled::CompileFail`.
        let compile_error = run_source("fn main(").expect_err("invalid Rue must not compile");
        let compile_failure = GeneratorContractFailure::from_run_source(compile_error);
        assert!(matches!(
            compile_failure,
            GeneratorContractFailure::Compile(detail) if !detail.is_empty()
        ));

        // A compiled-but-unmodeled program is the other distinct contract
        // failure class. Keep the variant typed so summaries/repros cannot
        // collapse it back into the old generic skip path.
        let unsupported = RunSourceError::Unsupported(rue_oracle::Unsupported(
            "intrinsic @random_u32".to_string(),
        ));
        assert_eq!(
            GeneratorContractFailure::from_run_source(unsupported),
            GeneratorContractFailure::Unsupported("intrinsic @random_u32".to_string())
        );
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
            timeout_secs: 3,
            source: "fn main() -> i32 { 23 }\n".to_string(),
            oracle_exit: 23,
            oracle_stdout: String::new(),
            oracle_panic: None,
            compiled: "compile-fail: first\rsecond".to_string(),
            reason: "wrong exit\nconst injected = 1;".to_string(),
        };

        assert!(disagreement.render().contains("--timeout 3"));
        let contents = disagreement_repro_contents(&disagreement);
        assert!(contents.contains("// reason: wrong exit\\nconst injected = 1;\n"));
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
            Duration::from_millis(200),
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
            Duration::from_secs(30),
        )
        .expect("run fake compiler");

        match result {
            Compiled::CompileFail(stderr) => {
                assert!(!stderr.is_empty(), "some compiler stderr is retained");
                assert_eq!(
                    stderr.len(),
                    STDERR_CAP,
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
        // Disagree). Emit ~200KB (below STDOUT_CAP), capture every byte, and
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
                assert!(matches!(classify(&oc(0, stdout), &result), Verdict::Agree));
            }
            other => panic!("expected Ran with full stdout, got {}", describe(other)),
        }
    }

    #[test]
    fn stdout_above_cap_is_drained_capped_and_cannot_agree_on_its_prefix() {
        let n = STDOUT_CAP + 100_000;
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
                assert_eq!(stdout.len(), STDOUT_CAP, "retained stdout must be capped");
                assert!(*stdout_truncated, "overflow must remain explicit");

                // Even an oracle outcome equal to the retained prefix cannot
                // agree: additional compiled output was observed and drained.
                let verdict = classify(&oc(0, stdout), &result);
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
        // STDOUT_CAP and kill/reap/join still returns promptly.
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
        // only STDERR_CAP bytes, the pipe is drained to EOF, so a program spewing
        // >64KB to stderr still terminates as `Ran` (RUE-338).
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("yes e | head -c 200000 1>&2");
        let result = run_with_timeout(cmd, Duration::from_secs(30)).expect("spawn");
        match result {
            Compiled::Ran { exit, stderr, .. } => {
                assert_eq!(exit, 0);
                assert!(!stderr.is_empty(), "some stderr should be retained");
                assert!(
                    stderr.len() <= STDERR_CAP,
                    "stderr retained ({}) must not exceed the cap {STDERR_CAP}",
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
        let kept = read_capped(std::io::Cursor::new(data), STDERR_CAP);
        assert_eq!(kept.bytes.len(), STDERR_CAP);
        assert!(kept.bytes.iter().all(|&b| b == b'z'));
        assert!(kept.truncated);
        // Fewer bytes than the cap: keep them all.
        let kept = read_capped(std::io::Cursor::new(vec![b'q'; 10]), STDERR_CAP);
        assert_eq!(kept.bytes.len(), 10);
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
