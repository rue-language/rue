//! Rue Fuzzer - Fuzz testing for the Rue compiler
//!
//! # Usage
//!
//! ```bash
//! # Run the lexer fuzzer with a corpus directory
//! ./buck2 run //crates/rue-fuzz:rue-fuzz -- lexer corpus/
//!
//! # Run with mutations for a specific duration
//! ./buck2 run //crates/rue-fuzz:rue-fuzz -- --mutate --max-time=60 parser corpus/
//!
//! # Generate a seed corpus from test files
//! ./buck2 run //crates/rue-fuzz:rue-fuzz -- --init-corpus output_dir/
//!
//! # Sanitize a restored nightly corpus and merge fresh seeds
//! ./buck2 run //crates/rue-fuzz:rue-fuzz -- --prepare-corpus=restored \
//!     --fresh-corpus=spec-seeds --output-corpus=nightly-corpus/lexer
//!
//! # List available targets
//! ./buck2 run //crates/rue-fuzz:rue-fuzz -- --list
//! ```

pub mod codegen_generators;
mod corpus;
pub mod generators;
pub mod harness;
mod mutate;
mod targets;

use harness::{CrashReporter, DEFAULT_PER_INPUT_TIMEOUT, RunOutcome, run_forked_with_timeout};
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A fuzz target that can be run with arbitrary input.
pub trait FuzzTarget: Send + Sync {
    /// The name of this fuzz target.
    fn name(&self) -> &'static str;

    /// Run the fuzz target with the given input.
    fn fuzz(&self, input: &[u8]);
}

/// Statistics from a fuzzing run.
///
/// `crashes` counts *every* crashing input (panics and signal deaths alike),
/// broken down into `panics` and `signals`. `unique_crashes` is the number of
/// distinct crash signatures actually saved to disk (see [`CrashReporter`]).
#[derive(Debug, Default)]
pub struct FuzzStats {
    pub runs: u64,
    pub crashes: u64,
    pub panics: u64,
    pub signals: u64,
    pub timeouts: u64,
    pub unique_crashes: u64,
    pub elapsed: Duration,
}

impl FuzzStats {
    pub fn exec_per_sec(&self) -> f64 {
        if self.elapsed.as_secs_f64() > 0.0 {
            self.runs as f64 / self.elapsed.as_secs_f64()
        } else {
            0.0
        }
    }
}

impl std::fmt::Display for FuzzStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "runs: {}, crashes: {} (panics: {}, signals: {}, timeouts: {}, unique: {}), exec/s: {:.1}, elapsed: {:.1}s",
            self.runs,
            self.crashes,
            self.panics,
            self.signals,
            self.timeouts,
            self.unique_crashes,
            self.exec_per_sec(),
            self.elapsed.as_secs_f64()
        )
    }
}

/// Configuration for the fuzzer.
#[derive(Debug, Clone)]
pub struct FuzzConfig {
    pub max_time: Option<Duration>,
    pub max_runs: Option<u64>,
    pub mutate: bool,
    pub crash_dir: Option<PathBuf>,
    pub print_interval: u64,
    /// Wall-clock budget per input; a child still running after this is
    /// SIGKILLed and counted as a Timeout crash.
    pub per_input_timeout: Duration,
    /// Mutation RNG seed. `None` = derive from the clock (each run explores a
    /// fresh sequence); the chosen seed is printed so any run can be replayed
    /// with `--seed=`.
    pub seed: Option<u64>,
    /// Optional per-target directory where successful mutated inputs are
    /// retained for the next nightly run. Crash inputs are intentionally not
    /// written here; [`CrashReporter`] owns crash artifacts and redaction.
    pub evolve_corpus: Option<PathBuf>,
}

impl Default for FuzzConfig {
    fn default() -> Self {
        Self {
            max_time: None,
            max_runs: None,
            mutate: false,
            crash_dir: None,
            print_interval: 1000,
            per_input_timeout: DEFAULT_PER_INPUT_TIMEOUT,
            seed: None,
            evolve_corpus: None,
        }
    }
}

/// Run a fuzz target with the given corpus and configuration.
pub fn run_fuzzer<T: FuzzTarget + ?Sized>(
    target: &T,
    corpus_dir: &Path,
    config: &FuzzConfig,
) -> anyhow::Result<FuzzStats> {
    let corpus = corpus::load_corpus(corpus_dir)?;
    if corpus.is_empty() {
        anyhow::bail!("corpus is empty: {}", corpus_dir.display());
    }

    eprintln!(
        "Fuzzing {} with {} corpus entries",
        target.name(),
        corpus.len()
    );

    let start = Instant::now();
    let mut runs: u64 = 0;
    let mut panics: u64 = 0;
    let mut signals: u64 = 0;
    let mut timeouts: u64 = 0;
    let mut crashes: u64 = 0;

    // Reproducer writing + dedup lives here; each distinct crash signature is
    // saved once, so a single flooding bug (e.g. the RUE-42 stack overflow)
    // can't bury the crashes dir under thousands of identical files.
    let mut reporter = CrashReporter::new(config.crash_dir.clone());
    let mut evolved = config
        .evolve_corpus
        .as_deref()
        .map(corpus::EvolvedCorpus::open)
        .transpose()?;

    // A fixed default seed would make every nightly run explore a byte-identical
    // input sequence (a regression re-run, not fuzzing). Default to a per-run
    // seed and print it so any run is still reproducible.
    let seed = config.seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42)
    });
    eprintln!(
        "[{}] seed: {seed} (replay with --seed={seed})",
        target.name()
    );
    let mut rng = mutate::SimpleRng::new(seed);

    loop {
        let elapsed = start.elapsed();
        if let Some(max_time) = config.max_time {
            if elapsed >= max_time {
                break;
            }
        }
        if let Some(max_runs) = config.max_runs {
            if runs >= max_runs {
                break;
            }
        }

        let input_idx = rng.next_u64() as usize % corpus.len();
        let mut input = corpus[input_idx].clone();

        if config.mutate {
            mutate::mutate(&mut input, &mut rng);
        }

        // Run each input in a forked child so that *aborts* (stack overflow,
        // OOM, SIGABRT/SIGSEGV/SIGFPE) are detected via the child's wait-status,
        // not just Rust panics. This is the core RUE-43 fix.
        let outcome = run_forked_with_timeout(|| target.fuzz(&input), config.per_input_timeout);

        if outcome.is_crash() {
            crashes += 1;
            match &outcome {
                RunOutcome::Panic(_) => panics += 1,
                RunOutcome::Signal(_) => signals += 1,
                RunOutcome::Timeout(_) => timeouts += 1,
                RunOutcome::Ok => unreachable!(),
            }
            reporter.report(target.name(), &input, &outcome);
        } else if config.mutate {
            if let Some(evolved) = evolved.as_mut() {
                if let Err(error) = evolved.record(&input) {
                    // Persistence is an optimization for the next campaign;
                    // it must not hide the target result or turn a crash into
                    // a corpus entry.
                    eprintln!("Warning: failed to save evolved corpus input: {error}");
                }
            }
        }

        runs += 1;

        if runs > 0 && runs % config.print_interval == 0 {
            let stats = FuzzStats {
                runs,
                crashes,
                panics,
                signals,
                timeouts,
                unique_crashes: reporter.unique_crashes,
                elapsed,
            };
            eprintln!("[{}] {}", target.name(), stats);
        }
    }

    Ok(FuzzStats {
        runs,
        crashes,
        panics,
        signals,
        timeouts,
        unique_crashes: reporter.unique_crashes,
        elapsed: start.elapsed(),
    })
}

/// Parse the value of a numeric `--flag=<n>` option (RUE-568). Returns an error
/// MESSAGE (not a coerced default) when the value is malformed, so the caller
/// can reject it with a usage diagnostic. Silently coercing a typo to
/// zero/default turned a mistyped budget into a green zero-work campaign, a
/// `--print-interval=0` into a divide-by-zero abort, and a bad `--seed` into an
/// unrelated fresh seed that broke replay. When `require_positive` is set, zero
/// is also rejected (budgets and the print interval must make progress / never
/// divide by zero); overflow past `u64::MAX` is a parse error like any other
/// non-integer.
fn parse_numeric_option(flag: &str, value: &str, require_positive: bool) -> Result<u64, String> {
    match value.parse::<u64>() {
        Ok(n) if !require_positive || n > 0 => Ok(n),
        Ok(_) => Err(format!("{flag} must be a positive integer (got '{value}')")),
        Err(_) => Err(format!("{flag} expects an integer (got '{value}')")),
    }
}

/// [`parse_numeric_option`] wrapper for the argument loop: on a malformed value
/// it prints the error and usage, then exits 1 (RUE-568).
fn numeric_or_exit(prog: &str, flag: &str, value: &str, require_positive: bool) -> u64 {
    match parse_numeric_option(flag, value, require_positive) {
        Ok(n) => n,
        Err(msg) => {
            eprintln!("Error: {msg}");
            print_usage(prog);
            std::process::exit(1);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let prog = args[0].clone();

    if args.len() < 2 {
        print_usage(&args[0]);
        std::process::exit(1);
    }

    let mut target_name: Option<String> = None;
    let mut corpus_dir: Option<PathBuf> = None;
    let mut config = FuzzConfig::default();
    let mut init_corpus = false;
    let mut prepare_corpus: Option<PathBuf> = None;
    let mut fresh_corpus: Option<PathBuf> = None;
    let mut output_corpus: Option<PathBuf> = None;
    let mut input_corpus: Option<PathBuf> = None;
    let mut publish_corpus: Option<PathBuf> = None;
    let mut cache_corpus: Option<PathBuf> = None;
    let mut list_targets = false;

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];

        if arg == "--help" || arg == "-h" {
            print_usage(&args[0]);
            return;
        } else if arg == "--list" {
            list_targets = true;
        } else if arg == "--init-corpus" {
            init_corpus = true;
            i += 1;
            if i < args.len() {
                corpus_dir = Some(PathBuf::from(&args[i]));
            }
        } else if let Some(val) = arg.strip_prefix("--prepare-corpus=") {
            if val.is_empty() {
                eprintln!("Error: --prepare-corpus requires a directory");
                print_usage(&prog);
                std::process::exit(1);
            }
            prepare_corpus = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--fresh-corpus=") {
            if val.is_empty() {
                eprintln!("Error: --fresh-corpus requires a directory");
                print_usage(&prog);
                std::process::exit(1);
            }
            fresh_corpus = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--output-corpus=") {
            if val.is_empty() {
                eprintln!("Error: --output-corpus requires a directory");
                print_usage(&prog);
                std::process::exit(1);
            }
            output_corpus = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--input-corpus=") {
            if val.is_empty() {
                eprintln!("Error: --input-corpus requires a directory");
                print_usage(&prog);
                std::process::exit(1);
            }
            input_corpus = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--publish-corpus=") {
            if val.is_empty() {
                eprintln!("Error: --publish-corpus requires a directory");
                print_usage(&prog);
                std::process::exit(1);
            }
            publish_corpus = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--cache-corpus=") {
            if val.is_empty() {
                eprintln!("Error: --cache-corpus requires a directory");
                print_usage(&prog);
                std::process::exit(1);
            }
            cache_corpus = Some(PathBuf::from(val));
        } else if arg == "--mutate" {
            config.mutate = true;
        } else if let Some(val) = arg.strip_prefix("--max-time=") {
            let secs = numeric_or_exit(&prog, "--max-time", val, true);
            config.max_time = Some(Duration::from_secs(secs));
        } else if let Some(val) = arg.strip_prefix("--max-runs=") {
            config.max_runs = Some(numeric_or_exit(&prog, "--max-runs", val, true));
        } else if arg.starts_with("--crash-dir=") {
            config.crash_dir = Some(PathBuf::from(&arg["--crash-dir=".len()..]));
        } else if let Some(val) = arg.strip_prefix("--print-interval=") {
            // Must be positive: the reporting loop does `runs % print_interval`.
            config.print_interval = numeric_or_exit(&prog, "--print-interval", val, true);
        } else if let Some(val) = arg.strip_prefix("--per-input-timeout=") {
            let secs = numeric_or_exit(&prog, "--per-input-timeout", val, true);
            config.per_input_timeout = Duration::from_secs(secs);
        } else if let Some(val) = arg.strip_prefix("--seed=") {
            // A malformed seed must be an error, never a silently-chosen fresh
            // seed (which would break the requested replay). Zero is a valid
            // seed, so this does not require positive.
            config.seed = Some(numeric_or_exit(&prog, "--seed", val, false));
        } else if let Some(val) = arg.strip_prefix("--evolve-corpus=") {
            if val.is_empty() {
                eprintln!("Error: --evolve-corpus requires a directory");
                print_usage(&prog);
                std::process::exit(1);
            }
            config.evolve_corpus = Some(PathBuf::from(val));
        } else if !arg.starts_with('-') {
            if target_name.is_none() {
                target_name = Some(arg.clone());
            } else if corpus_dir.is_none() {
                corpus_dir = Some(PathBuf::from(arg));
            }
        } else {
            eprintln!("Unknown argument: {}", arg);
            std::process::exit(1);
        }

        i += 1;
    }

    if list_targets {
        eprintln!("Available fuzz targets:");
        for target in targets::all_targets() {
            eprintln!("  {}", target.name());
        }
        return;
    }

    if init_corpus {
        let output_dir = corpus_dir.unwrap_or_else(|| {
            eprintln!("Error: --init-corpus requires an output directory");
            std::process::exit(1);
        });

        let spec_dir = find_spec_cases_dir();

        match corpus::create_seed_corpus(&spec_dir, &output_dir) {
            Ok(summary) => {
                eprintln!(
                    "Created seed corpus in {}: {}",
                    output_dir.display(),
                    summary
                );
            }
            Err(e) => {
                eprintln!("Error creating corpus: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    if let Some(restored_dir) = prepare_corpus {
        let fresh_dir = fresh_corpus.unwrap_or_else(|| {
            eprintln!("Error: --prepare-corpus requires --fresh-corpus=<directory>");
            print_usage(&prog);
            std::process::exit(1);
        });
        let output_dir = output_corpus.unwrap_or_else(|| {
            eprintln!("Error: --prepare-corpus requires --output-corpus=<directory>");
            print_usage(&prog);
            std::process::exit(1);
        });
        let input_dir = input_corpus.unwrap_or_else(|| {
            eprintln!("Error: --prepare-corpus requires --input-corpus=<directory>");
            print_usage(&prog);
            std::process::exit(1);
        });
        match corpus::sanitize_corpus(&restored_dir, &fresh_dir, &input_dir, &output_dir) {
            Ok(summary) => {
                eprintln!(
                    "Prepared corpus in {}: {} fresh seed(s), {} restored input(s), {} retained, {} ignored, {} bytes",
                    output_dir.display(),
                    summary.fresh_seeds,
                    summary.restored_inputs,
                    summary.retained_inputs,
                    summary.ignored_inputs,
                    summary.bytes,
                );
            }
            Err(error) => {
                eprintln!("Error preparing corpus: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if let Some(source_dir) = publish_corpus {
        let cache_dir = cache_corpus.unwrap_or_else(|| {
            eprintln!("Error: --publish-corpus requires --cache-corpus=<directory>");
            print_usage(&prog);
            std::process::exit(1);
        });
        match corpus::publish_corpus(&source_dir, &cache_dir) {
            Ok(count) => eprintln!(
                "Published {count} clean corpus file(s) to {}",
                cache_dir.display()
            ),
            Err(error) => {
                eprintln!("Error publishing corpus: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    let target_name = target_name.unwrap_or_else(|| {
        eprintln!("Error: no fuzz target specified");
        print_usage(&args[0]);
        std::process::exit(1);
    });

    let corpus_dir = corpus_dir.unwrap_or_else(|| {
        eprintln!("Error: no corpus directory specified");
        print_usage(&args[0]);
        std::process::exit(1);
    });

    let target = targets::get_target(&target_name).unwrap_or_else(|| {
        eprintln!("Unknown fuzz target: {}", target_name);
        eprintln!("Use --list to see available targets");
        std::process::exit(1);
    });

    if config.crash_dir.is_none() {
        config.crash_dir = Some(corpus_dir.parent().unwrap_or(&corpus_dir).join("crashes"));
    }

    match run_fuzzer(target.as_ref(), &corpus_dir, &config) {
        Ok(stats) => {
            eprintln!("\nFuzzing complete: {}", stats);
            if stats.crashes > 0 {
                eprintln!(
                    "Found {} crash(es): {} panic(s), {} signal(s), {} timeout(s); {} unique reproducer(s) saved.",
                    stats.crashes,
                    stats.panics,
                    stats.signals,
                    stats.timeouts,
                    stats.unique_crashes
                );
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn print_usage(program: &str) {
    eprintln!("Rue Fuzzer - Fuzz testing for the Rue compiler");
    eprintln!();
    eprintln!("Usage:");
    eprintln!(
        "  {} <target> <corpus_dir>        Run a fuzz target",
        program
    );
    eprintln!(
        "  {} --init-corpus <output_dir>   Create seed corpus",
        program
    );
    eprintln!(
        "  {} --prepare-corpus=<dir> --fresh-corpus=<dir> --input-corpus=<dir> --output-corpus=<dir>",
        program
    );
    eprintln!("  {} --publish-corpus=<dir> --cache-corpus=<dir>", program);
    eprintln!(
        "  {} --list                       List available targets",
        program
    );
    eprintln!();
    // Drive this from all_targets() so help output and target dispatch share
    // one authoritative inventory.
    eprintln!("Targets:");
    for target in targets::all_targets() {
        eprintln!("  {}", target.name());
    }
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --mutate                   Enable input mutation");
    eprintln!("  --max-time=<secs>          Maximum time to run");
    eprintln!("  --max-runs=<n>             Maximum number of runs");
    eprintln!("  --crash-dir=<dir>          Directory to save crashes");
    eprintln!("  --print-interval=<n>       Print progress every N runs");
    eprintln!("  --per-input-timeout=<secs> Kill+report inputs running longer (default 5)");
    eprintln!("  --seed=<n>                 Mutation RNG seed (default: per-run, printed)");
    eprintln!("  --evolve-corpus=<dir>      Save bounded successful mutations for reuse");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  {} --init-corpus corpus/", program);
    eprintln!("  {} lexer corpus/", program);
    eprintln!("  {} --mutate --max-time=300 parser corpus/", program);
}

fn find_spec_cases_dir() -> PathBuf {
    let candidates = [
        PathBuf::from("crates/rue-spec/cases"),
        PathBuf::from("../rue-spec/cases"),
        PathBuf::from("../../crates/rue-spec/cases"),
    ];

    for candidate in &candidates {
        if candidate.exists() && candidate.is_dir() {
            return candidate.clone();
        }
    }

    PathBuf::from("crates/rue-spec/cases")
}

#[cfg(test)]
mod tests {
    use super::parse_numeric_option;
    use super::{FuzzConfig, FuzzTarget, run_fuzzer};
    use std::time::Duration;

    struct SuccessfulTarget;

    impl FuzzTarget for SuccessfulTarget {
        fn name(&self) -> &'static str {
            "successful"
        }

        fn fuzz(&self, _input: &[u8]) {}
    }

    struct PanickingTarget;

    impl FuzzTarget for PanickingTarget {
        fn name(&self) -> &'static str {
            "panicking"
        }

        fn fuzz(&self, _input: &[u8]) {
            panic!("test crash");
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rue-fuzz-runner-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn valid_positive_values_parse() {
        assert_eq!(parse_numeric_option("--max-runs", "42", true), Ok(42));
        assert_eq!(parse_numeric_option("--max-time", "1", true), Ok(1));
    }

    #[test]
    fn zero_is_a_valid_seed_but_not_a_valid_budget() {
        // Seeds allow zero (require_positive = false); budgets/intervals do not.
        assert_eq!(parse_numeric_option("--seed", "0", false), Ok(0));
        assert!(parse_numeric_option("--print-interval", "0", true).is_err());
        assert!(parse_numeric_option("--max-runs", "0", true).is_err());
    }

    #[test]
    fn wrong_type_is_rejected_not_coerced() {
        // The original bug: `typo` became 0/default instead of an error.
        for flag in ["--max-time", "--max-runs", "--print-interval", "--seed"] {
            let require_positive = flag != "--seed";
            let err = parse_numeric_option(flag, "typo", require_positive)
                .expect_err("malformed value must error");
            assert!(err.contains(flag), "{err}");
            assert!(err.contains("typo"), "{err}");
        }
    }

    #[test]
    fn negative_and_overflow_are_rejected() {
        // `-1` is not a u64; a value past u64::MAX overflows the parse.
        assert!(parse_numeric_option("--max-runs", "-1", true).is_err());
        assert!(parse_numeric_option("--max-time", "99999999999999999999999", true).is_err());
    }

    #[test]
    fn positive_flag_message_distinguishes_zero_from_non_integer() {
        let zero = parse_numeric_option("--max-runs", "0", true).unwrap_err();
        assert!(zero.contains("positive"), "{zero}");
        let bad = parse_numeric_option("--max-runs", "x", true).unwrap_err();
        assert!(bad.contains("integer"), "{bad}");
    }

    #[test]
    fn successful_mutations_are_retained_but_crashes_are_not() {
        let root = scratch("evolution");
        let input = root.join("inputs");
        let evolved = root.join("evolved");
        std::fs::create_dir_all(&input).unwrap();
        std::fs::write(input.join("seed"), b"fn main() -> i32 { 0 }").unwrap();

        let config = FuzzConfig {
            max_runs: Some(1),
            mutate: true,
            seed: Some(7),
            evolve_corpus: Some(evolved.clone()),
            per_input_timeout: Duration::from_secs(1),
            ..FuzzConfig::default()
        };
        let successful = run_fuzzer(&SuccessfulTarget, &input, &config).unwrap();
        assert_eq!(successful.crashes, 0);
        assert!(std::fs::read_dir(&evolved).unwrap().next().is_some());

        let crash_evolved = root.join("crash-evolved");
        let crash_config = FuzzConfig {
            evolve_corpus: Some(crash_evolved.clone()),
            ..config
        };
        let crashed = run_fuzzer(&PanickingTarget, &input, &crash_config).unwrap();
        assert_eq!(crashed.crashes, 1);
        assert!(
            std::fs::read_dir(&crash_evolved).unwrap().next().is_none(),
            "crash inputs must remain in crash reporting only"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
