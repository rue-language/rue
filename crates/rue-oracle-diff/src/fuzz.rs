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
//! The compiler binary is located via `RUE_BINARY` (what `scripts/rue` /
//! `test.sh` set) or the `bin/rue` symlink, mirroring `rue-test-runner`.

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
    /// The binary ran to completion: process exit code + captured stdout.
    Ran { exit: i32, stdout: String },
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
/// does: `RUE_BINARY` override, else the `bin/rue` symlink.
fn find_rue_binary() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("RUE_BINARY") {
        return Ok(PathBuf::from(p));
    }
    let sym = Path::new("bin/rue");
    if sym.exists() {
        // Canonicalize to an absolute path: each generated program is compiled
        // with `current_dir` set to the temp workdir, so a relative `bin/rue`
        // would no longer resolve. (The harness normally runs with an absolute
        // RUE_BINARY; this keeps the bare `bin/rue` fallback working too.)
        return Ok(sym.canonicalize().unwrap_or_else(|_| sym.to_path_buf()));
    }
    Err(
        "cannot locate the rue compiler: set RUE_BINARY to an explicit path, \
         or run `scripts/rue build` to refresh the bin/rue symlink"
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

fn classify(oracle: &rue_oracle::Outcome, compiled: &Compiled) -> Verdict {
    match compiled {
        Compiled::Ran { exit, stdout } => {
            let exit_ok = oracle.exit_code == *exit;
            let stdout_ok = &oracle.stdout == stdout;
            if exit_ok && stdout_ok {
                Verdict::Agree
            } else {
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
                Verdict::Disagree(r)
            }
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
    // child's own exit code / terminating signal unambiguously.
    let mut child = Command::new(&bin_path)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let start = Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let mut stdout = String::new();
                if let Some(mut out) = child.stdout.take() {
                    out.read_to_string(&mut stdout).ok();
                }
                return Ok(match status.code() {
                    Some(code) => Compiled::Ran { exit: code, stdout },
                    None => Compiled::Crash(status.signal().unwrap_or(0)),
                });
            }
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(Compiled::Timeout);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
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
        Compiled::Ran { exit, stdout } => format!("ran exit={exit} stdout={stdout:?}"),
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

    fn is_disagree(v: Verdict) -> bool {
        matches!(v, Verdict::Disagree(_))
    }

    #[test]
    fn agrees_when_exit_and_stdout_match() {
        let v = classify(
            &oc(42, "7\n"),
            &Compiled::Ran {
                exit: 42,
                stdout: "7\n".to_string(),
            },
        );
        assert!(matches!(v, Verdict::Agree));
    }

    #[test]
    fn detects_exit_mismatch() {
        // The planted-bug shape: same stdout, wrong exit code.
        let v = classify(
            &oc(42, ""),
            &Compiled::Ran {
                exit: 43,
                stdout: String::new(),
            },
        );
        assert!(is_disagree(v));
    }

    #[test]
    fn detects_stdout_mismatch() {
        let v = classify(
            &oc(0, "7\n"),
            &Compiled::Ran {
                exit: 0,
                stdout: "8\n".to_string(),
            },
        );
        assert!(is_disagree(v));
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
    fn args_parse_flags() {
        let cfg = parse_args(&["--start=5".into(), "--seeds".into(), "12".into()]).expect("parse");
        assert_eq!(cfg.start, 5);
        assert_eq!(cfg.seeds, 12);
    }
}
