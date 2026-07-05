//! # Differential fuzzer (`rue-oracle-diff fuzz`) — RUE-247
//!
//! Generate random **valid** Rue programs (see [`crate::gen`]) and run each one
//! through *both* engines:
//!
//! 1. the [`rue_oracle`] reference interpreter (`run_source`), and
//! 2. the real compiler + the produced native binary.
//!
//! Then compare the observable behavior — process exit code and `@dbg` stdout.
//! A disagreement is an **automatically-discovered miscompile** with a concrete,
//! deterministic repro: the seed regenerates the exact program. This is the
//! RUE-50 payoff wired to RUE-205's harness — "Fable runs a hunt and files bugs"
//! becomes "CI files the bugs."
//!
//! Determinism: programs are a pure function of their `u64` seed, so the fuzzer
//! is fully reproducible (`fuzz --start S --seeds 1` re-runs exactly seed `S`).
//!
//! The compiler binary is located via `RUE_BINARY`, which `scripts/rue`,
//! `test.sh`, and Buck test targets set from `scripts/rue-bin`.

use crate::generator;
use rue_oracle::run_source;
use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// Outcome of compiling and running a generated program natively.
enum Compiled {
    /// The compiler rejected a program the oracle's frontend accepted — an ICE
    /// or backend gap (carries the compiler's stderr, truncated).
    CompileFail(String),
    /// The binary ran to completion: process exit code + captured stdout +
    /// captured stderr. `stderr` carries the runtime's trap message (e.g.
    /// `"error: integer overflow\n"`) so that when both engines exit 101 we can
    /// compare *which* trap fired, not just that one did (RUE-339).
    Ran {
        exit: i32,
        stdout: String,
        stderr: String,
    },
    /// The binary was killed by a signal (e.g. SIGSEGV) — a hard miscompile.
    Crash(i32),
    /// The binary did not terminate within the per-program timeout.
    Timeout,
}

struct Config {
    start: u64,
    seeds: u64,
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

    let workdir = std::env::temp_dir().join(format!("rue-oracle-fuzz-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&workdir) {
        eprintln!("cannot create work dir {}: {e}", workdir.display());
        return ExitCode::FAILURE;
    }

    println!(
        "=== rue-oracle-diff fuzz: seeds {}..{} (compiler: {}) ===",
        cfg.start,
        cfg.start + cfg.seeds,
        rue.display()
    );

    let mut agree = 0u32;
    let mut skip_unsupported = 0u32;
    let mut disagreements: Vec<Disagreement> = Vec::new();

    for seed in cfg.start..cfg.start + cfg.seeds {
        let source = generator::generate(seed);

        let oracle = match run_source(&source) {
            // Outside the modeled subset (or a front-end compile error): a clean
            // skip, exactly as intended — never a false disagreement.
            Err(unsupported) => {
                skip_unsupported += 1;
                if cfg.verbose {
                    println!("  seed {seed}: skip ({})", unsupported.0);
                }
                continue;
            }
            Ok(o) => o,
        };

        let compiled = match compile_and_run(&rue, &workdir, &source, cfg.timeout) {
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
                    source: source.clone(),
                    oracle_exit: oracle.exit_code,
                    oracle_stdout: oracle.stdout.clone(),
                    oracle_panic: oracle.panic.clone(),
                    compiled: describe(&compiled),
                    reason,
                };
                eprintln!("{}", d.render());
                let _ = save_repro(&cfg.crash_dir, &d);
                disagreements.push(d);
            }
        }
    }

    let total = agree + skip_unsupported + disagreements.len() as u32;
    println!("\n=== summary over {total} generated programs ===");
    println!("  agree:            {agree}");
    println!("  skip (unmodeled): {skip_unsupported}");
    println!("  DISAGREEMENTS:    {}", disagreements.len());

    if disagreements.is_empty() {
        println!("\noracle and compiler agree on every generated program.");
        ExitCode::SUCCESS
    } else {
        println!(
            "\n{} disagreement(s) — each is an automatically-found miscompile. \
             Repros saved to {}.",
            disagreements.len(),
            cfg.crash_dir.display()
        );
        ExitCode::FAILURE
    }
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
            stderr,
        } => {
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

    let compile = Command::new(rue)
        .arg("prog.rue")
        .arg("-o")
        .arg("prog")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr).into_owned();
        return Ok(Compiled::CompileFail(stderr));
    }

    // Run the produced binary directly with a manual timeout so we can read the
    // child's own exit code / terminating signal unambiguously. stderr is
    // captured (not `Stdio::null`) so a trap's message is available to
    // `classify` for panic-category comparison (RUE-339).
    let mut cmd = Command::new(&bin_path);
    cmd.current_dir(dir);
    run_with_timeout(cmd, timeout)
}

/// Maximum stderr bytes retained from a run. Trap messages are short; the cap
/// only bounds how much we *keep* — the pipe is still drained to EOF (see
/// [`read_capped`]) so a chatty program can never deadlock the wait loop.
const STDERR_CAP: usize = 8192;

/// Spawn `cmd` with piped stdout/stderr, drain both pipes **concurrently** via
/// reader threads, and wait with a manual timeout.
///
/// Draining concurrently is essential (RUE-338): if the pipes were only read
/// after the child exits, a program that writes more than the OS pipe capacity
/// (~64KB on Linux) would block on `write()` forever, `try_wait` would never
/// report an exit, and the timeout would manufacture a false `Compiled::Timeout`
/// (→ a fabricated `Verdict::Disagree`). The reader threads start immediately
/// after `spawn` so the child always has a consumer. On timeout we kill first,
/// then join the readers — killing closes the child's write ends, the pipes hit
/// EOF, and the readers finish, so no thread leaks.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<Compiled> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // stdout is captured in full (the oracle compares complete output, so it
    // cannot be truncated); stderr is drained to EOF but only `STDERR_CAP` bytes
    // are retained. Both readers run on their own thread so neither pipe can
    // fill and block the child.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout_pipe {
            out.read_to_end(&mut buf).ok();
        }
        buf
    });
    let stderr_reader =
        std::thread::spawn(move || stderr_pipe.map(|err| read_capped(err, STDERR_CAP)));

    let start = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Join so the reader threads don't leak; the killed child's
                    // closed fds give them EOF, so these return promptly.
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Ok(Compiled::Timeout);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };

    // The child has exited; its write ends are closed, so the readers hit EOF
    // and finish. Join to collect the fully-drained output.
    let stdout_bytes = stdout_reader.join().unwrap_or_default();
    let stderr_bytes = stderr_reader.join().unwrap_or_default().unwrap_or_default();
    // Lossy decode so garbage bytes from a miscompile surface as a stdout
    // mismatch rather than silently becoming empty (as `read_to_string` would).
    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

    Ok(match status.code() {
        Some(code) => Compiled::Ran {
            exit: code,
            stdout,
            stderr,
        },
        None => Compiled::Crash(status.signal().unwrap_or(0)),
    })
}

/// Read `r` to EOF, retaining at most `cap` bytes. Reading all the way to EOF
/// (rather than stopping after `cap`, as `Read::take` would) is what prevents a
/// drain-deadlock: a program that writes more than `cap` to this pipe must still
/// have every byte consumed or it blocks on a full pipe forever (RUE-338). The
/// cap bounds only the memory we keep, not how much we drain.
fn read_capped<R: Read>(mut r: R, cap: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if kept.len() < cap {
                    let take = (cap - kept.len()).min(n);
                    kept.extend_from_slice(&chunk[..take]);
                }
                // Bytes beyond `cap` are read and discarded — draining, not
                // storing — so the pipe never fills.
            }
            Err(_) => break,
        }
    }
    kept
}

struct Disagreement {
    seed: u64,
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
            "\n\u{2717} DISAGREEMENT (seed {seed})\n  {reason}\n  oracle:   exit={exit} panic={panic:?} stdout={stdout:?}\n  compiled: {compiled}\n  --- source (regenerate with `fuzz --start {seed} --seeds 1`) ---\n{source}",
            seed = self.seed,
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
        Compiled::Ran { exit, stdout, .. } => format!("ran exit={exit} stdout={stdout:?}"),
        Compiled::CompileFail(e) => format!("compile-fail: {}", first_line(e)),
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

/// Persist a repro so a CI failure uploads a concrete, self-contained program.
fn save_repro(dir: &Path, d: &Disagreement) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("oracle-diff-seed-{}.rue", d.seed));
    let contents = format!(
        "// rue-oracle-diff differential miscompile (seed {seed})\n\
         // reason: {reason}\n\
         // oracle:   exit={exit} panic={panic:?} stdout={stdout:?}\n\
         // compiled: {compiled}\n\
         // regenerate: rue-oracle-diff fuzz --start {seed} --seeds 1\n\n{source}",
        seed = d.seed,
        reason = d.reason,
        exit = d.oracle_exit,
        panic = d.oracle_panic,
        stdout = d.oracle_stdout,
        compiled = d.compiled,
        source = d.source,
    );
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_oracle::Outcome;

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
            stderr: String::new(),
        }
    }

    /// A `Compiled::Ran` that trapped at exit 101 with the given stderr message.
    fn ran_trap(stderr: &str) -> Compiled {
        Compiled::Ran {
            exit: 101,
            stdout: String::new(),
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
    fn compile_fail_and_crash_and_timeout_are_disagreements() {
        // Oracle produced a clean outcome, so any non-Ran compiled result is a
        // divergence worth reporting.
        assert!(is_disagree(classify(
            &oc(0, ""),
            &Compiled::CompileFail("boom".into())
        )));
        assert!(is_disagree(classify(&oc(0, ""), &Compiled::Crash(11))));
        assert!(is_disagree(classify(&oc(0, ""), &Compiled::Timeout)));
    }

    #[test]
    fn large_stdout_does_not_deadlock() {
        // RUE-338: a program that writes far more than the OS pipe capacity
        // (~64KB) must be drained concurrently while it runs. Before the fix the
        // pipe filled, the writer blocked on `write()`, `try_wait` never saw an
        // exit, and the 10s timeout fabricated a `Compiled::Timeout` (→ a false
        // Disagree). Emit ~200KB and assert we capture every byte as `Ran`.
        let n = 200_000usize;
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!("yes y | head -c {n}"));
        let result = run_with_timeout(cmd, Duration::from_secs(30)).expect("spawn");
        match result {
            Compiled::Ran { exit, stdout, .. } => {
                assert_eq!(exit, 0, "pipeline should exit cleanly");
                assert_eq!(
                    stdout.len(),
                    n,
                    "full stdout must be captured, not truncated"
                );
            }
            other => panic!("expected Ran with full stdout, got {}", describe(&other)),
        }
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
        assert_eq!(kept.len(), STDERR_CAP);
        assert!(kept.iter().all(|&b| b == b'z'));
        // Fewer bytes than the cap: keep them all.
        let kept = read_capped(std::io::Cursor::new(vec![b'q'; 10]), STDERR_CAP);
        assert_eq!(kept.len(), 10);
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
        let cfg = parse_args(&["--start=5".into(), "--seeds".into(), "12".into()]).expect("parse");
        assert_eq!(cfg.start, 5);
        assert_eq!(cfg.seeds, 12);
    }
}
