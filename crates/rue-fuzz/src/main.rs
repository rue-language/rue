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
//! # List available targets
//! ./buck2 run //crates/rue-fuzz:rue-fuzz -- --list
//! ```

pub mod codegen_generators;
mod corpus;
pub mod generators;
pub mod harness;
mod mutate;
mod targets;

use harness::{CrashReporter, RunOutcome, run_forked};
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
            "runs: {}, crashes: {} (panics: {}, signals: {}, unique: {}), exec/s: {:.1}, elapsed: {:.1}s",
            self.runs,
            self.crashes,
            self.panics,
            self.signals,
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
}

impl Default for FuzzConfig {
    fn default() -> Self {
        Self {
            max_time: None,
            max_runs: None,
            mutate: false,
            crash_dir: None,
            print_interval: 1000,
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
    let mut crashes: u64 = 0;

    // Reproducer writing + dedup lives here; each distinct crash signature is
    // saved once, so a single flooding bug (e.g. the RUE-42 stack overflow)
    // can't bury the crashes dir under thousands of identical files.
    let mut reporter = CrashReporter::new(config.crash_dir.clone());

    let mut rng = mutate::SimpleRng::new(42);

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
        let outcome = run_forked(|| target.fuzz(&input));

        if outcome.is_crash() {
            crashes += 1;
            match &outcome {
                RunOutcome::Panic(_) => panics += 1,
                RunOutcome::Signal(_) => signals += 1,
                RunOutcome::Ok => unreachable!(),
            }
            reporter.report(target.name(), &input, &outcome);
        }

        runs += 1;

        if runs > 0 && runs % config.print_interval == 0 {
            let stats = FuzzStats {
                runs,
                crashes,
                panics,
                signals,
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
        unique_crashes: reporter.unique_crashes,
        elapsed: start.elapsed(),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage(&args[0]);
        std::process::exit(1);
    }

    let mut target_name: Option<String> = None;
    let mut corpus_dir: Option<PathBuf> = None;
    let mut config = FuzzConfig::default();
    let mut init_corpus = false;
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
        } else if arg == "--mutate" {
            config.mutate = true;
        } else if arg.starts_with("--max-time=") {
            let secs: u64 = arg["--max-time=".len()..].parse().unwrap_or(0);
            config.max_time = Some(Duration::from_secs(secs));
        } else if arg.starts_with("--max-runs=") {
            let runs: u64 = arg["--max-runs=".len()..].parse().unwrap_or(0);
            config.max_runs = Some(runs);
        } else if arg.starts_with("--crash-dir=") {
            config.crash_dir = Some(PathBuf::from(&arg["--crash-dir=".len()..]));
        } else if arg.starts_with("--print-interval=") {
            config.print_interval = arg["--print-interval=".len()..].parse().unwrap_or(1000);
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
            Ok(count) => {
                eprintln!(
                    "Created seed corpus with {} files in {}",
                    count,
                    output_dir.display()
                );
            }
            Err(e) => {
                eprintln!("Error creating corpus: {}", e);
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
                    "Found {} crash(es): {} panic(s), {} signal(s); {} unique reproducer(s) saved.",
                    stats.crashes, stats.panics, stats.signals, stats.unique_crashes
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
        "  {} --list                       List available targets",
        program
    );
    eprintln!();
    eprintln!("Targets:");
    eprintln!("  lexer       Fuzz the lexer (tokenization)");
    eprintln!("  parser      Fuzz the parser (AST construction)");
    eprintln!("  sema        Fuzz semantic analysis (type checking, inference)");
    eprintln!("  compiler    Fuzz the full frontend (through sema)");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --mutate              Enable input mutation");
    eprintln!("  --max-time=<secs>     Maximum time to run");
    eprintln!("  --max-runs=<n>        Maximum number of runs");
    eprintln!("  --crash-dir=<dir>     Directory to save crashes");
    eprintln!("  --print-interval=<n>  Print progress every N runs");
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
