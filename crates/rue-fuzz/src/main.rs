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

mod corpus;
pub mod generators;
mod mutate;
mod targets;

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A fuzz target that can be run with arbitrary input.
pub trait FuzzTarget: Send + Sync {
    /// The name of this fuzz target.
    fn name(&self) -> &'static str;

    /// Run the fuzz target with the given input.
    fn fuzz(&self, input: &[u8]);
}

/// Statistics from a fuzzing run.
#[derive(Debug, Default)]
pub struct FuzzStats {
    pub runs: u64,
    pub crashes: u64,
    pub panics: u64,
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
            "runs: {}, crashes: {}, panics: {}, exec/s: {:.1}, elapsed: {:.1}s",
            self.runs,
            self.crashes,
            self.panics,
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
    let runs = Arc::new(AtomicU64::new(0));
    let panics = Arc::new(AtomicU64::new(0));
    let crashes = Arc::new(AtomicU64::new(0));

    let mut rng = mutate::SimpleRng::new(42);

    loop {
        let elapsed = start.elapsed();
        if let Some(max_time) = config.max_time {
            if elapsed >= max_time {
                break;
            }
        }
        let current_runs = runs.load(Ordering::Relaxed);
        if let Some(max_runs) = config.max_runs {
            if current_runs >= max_runs {
                break;
            }
        }

        let input_idx = rng.next_u64() as usize % corpus.len();
        let mut input = corpus[input_idx].clone();

        if config.mutate {
            mutate::mutate(&mut input, &mut rng);
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            target.fuzz(&input);
        }));

        if result.is_err() {
            panics.fetch_add(1, Ordering::Relaxed);

            if let Some(ref crash_dir) = config.crash_dir {
                if let Err(e) = save_crash(crash_dir, &input, current_runs) {
                    eprintln!("Warning: failed to save crash: {}", e);
                }
            }
        }

        runs.fetch_add(1, Ordering::Relaxed);

        if current_runs > 0 && current_runs % config.print_interval == 0 {
            let stats = FuzzStats {
                runs: current_runs,
                crashes: crashes.load(Ordering::Relaxed),
                panics: panics.load(Ordering::Relaxed),
                elapsed,
            };
            eprintln!("[{}] {}", target.name(), stats);
        }
    }

    Ok(FuzzStats {
        runs: runs.load(Ordering::Relaxed),
        crashes: crashes.load(Ordering::Relaxed),
        panics: panics.load(Ordering::Relaxed),
        elapsed: start.elapsed(),
    })
}

fn save_crash(crash_dir: &Path, input: &[u8], run_id: u64) -> std::io::Result<()> {
    use std::io::Write;

    std::fs::create_dir_all(crash_dir)?;

    let hash = {
        let mut h: u64 = 0;
        for &b in input {
            h = h.wrapping_mul(31).wrapping_add(b as u64);
        }
        h
    };

    let filename = format!("crash-{:016x}-{}.txt", hash, run_id);
    let path = crash_dir.join(filename);

    let mut file = std::fs::File::create(&path)?;
    file.write_all(input)?;

    eprintln!("Saved crash to: {}", path.display());
    Ok(())
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
            if stats.panics > 0 {
                eprintln!("Found {} panic(s)!", stats.panics);
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
