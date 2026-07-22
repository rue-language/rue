//! RUE-1086 scaling measurement runner (separate, opt-in binary).
//!
//! This binary produces the *measurement* deliverables RUE-1086 requires — warm
//! edit latency, cold latency, allocation counts, and peak memory — for the same
//! two-dimensional synthetic corpus the counter harness
//! (`rue-compiler/src/scaling_harness.rs`) gates on. It is deliberately a
//! separate binary so its clocks, its counting global allocator, and its memory
//! probes NEVER run inside the counter-assertion or timing suites.
//!
//! The counting global allocator is only compiled in under
//! `--cfg=rue_benchmark_allocations`, mirroring the `rue`/`rue-benchmark` split,
//! and is built as a distinct target (`rue-scaling-bench-allocations`) so the
//! default timing/memory target has an unmodified allocator.
//!
//! # Reproducible commands
//!
//! Build release first: `scripts/rue-bin --target-platforms //platforms:release`
//! (or `./buck2 build //crates/rue-scaling-bench:rue-scaling-bench`).
//!
//! ```text
//! # Cold pre-link latency, 5 samples:
//! rue-scaling-bench --mode timing --bodies 1000 --decls 100 --iterations 5
//! # Warm single-body-edit latency (two-revision scenario), 20 samples:
//! rue-scaling-bench --mode timing --warm --bodies 1000 --decls 100 --iterations 20
//! # Peak resident memory (VmHWM / rusage) for a cold compile:
//! rue-scaling-bench --mode memory --bodies 1000 --decls 100
//! # Allocation counts (distinct binary; counting global allocator):
//! rue-scaling-bench-allocations --mode alloc --bodies 1000 --decls 100
//! ```
//!
//! Every run prints a provenance header (nproc, total memory, commit hash) so a
//! recorded baseline is attributable to a concrete reference host and revision.
//! The ~45 ms pre-link target from RUE-1083's Caldera prediction is an eventual
//! reference-host goal, not a gate this runner enforces.

use std::time::{Duration, Instant};

use rue_compiler::{CompileOptions, CompilerSession, SourceSnapshot};

#[cfg(rue_benchmark_allocations)]
mod allocation;

// ---------------------------------------------------------------------------
// Deterministic synthetic corpus (mirror of scaling_harness.rs `Corpus`)
// ---------------------------------------------------------------------------
//
// Kept byte-for-byte identical to the counter harness's generator so a timing /
// allocation / memory number is attributable to the same structural workload the
// counters describe. The generator is tiny and test-only there
// (`#[cfg(test)]`), so it is intentionally duplicated here rather than exposed as
// production API.

fn corpus_source(reached_bodies: usize, unrelated_decls: usize) -> String {
    let mut src = String::with_capacity((reached_bodies + unrelated_decls) * 24);
    for i in 0..reached_bodies {
        src.push_str(&format!("fn b{i}() -> i32 {{ {i} }}\n"));
    }
    for j in 0..unrelated_decls {
        src.push_str(&format!("fn d{j}() -> i32 {{ {j} }}\n"));
    }
    src.push_str("fn main() -> i32 {\n    let mut acc = 0;\n");
    for i in 0..reached_bodies {
        src.push_str(&format!("    acc = acc + b{i}();\n"));
    }
    src.push_str("    acc\n}\n");
    src
}

fn snapshot(source: String) -> SourceSnapshot {
    SourceSnapshot::single("main.rue", source).expect("synthetic corpus parses")
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

fn nproc() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0)
}

fn total_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Peak resident set size in bytes (VmHWM on Linux, rusage on macOS). Reuses the
/// approach in `crates/rue/src/main.rs`.
fn peak_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn commit_hash() -> String {
    if let Ok(hash) = std::env::var("RUE_SCALING_COMMIT") {
        if !hash.is_empty() {
            return hash;
        }
    }
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn print_provenance() {
    let mem = total_memory_bytes()
        .map(|b| format!("{:.1} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0)))
        .unwrap_or_else(|| "unknown".to_owned());
    eprintln!(
        "== RUE-1086 scaling-bench provenance ==\n  \
         host: nproc={} memory={} os={} arch={}\n  \
         commit: {}",
        nproc(),
        mem,
        std::env::consts::OS,
        std::env::consts::ARCH,
        commit_hash(),
    );
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Mode {
    Timing,
    Memory,
    Alloc,
}

struct Config {
    mode: Mode,
    bodies: usize,
    decls: usize,
    iterations: usize,
    warm: bool,
}

fn parse_config() -> Config {
    let mut mode = Mode::Timing;
    let mut bodies = 1_000usize;
    let mut decls = 100usize;
    let mut iterations = 5usize;
    let mut warm = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => {
                mode = match args.next().as_deref() {
                    Some("timing") => Mode::Timing,
                    Some("memory") => Mode::Memory,
                    Some("alloc") => Mode::Alloc,
                    other => {
                        eprintln!("unknown --mode {other:?} (timing|memory|alloc)");
                        std::process::exit(2);
                    }
                };
            }
            "--bodies" => bodies = parse_usize(args.next(), "--bodies"),
            "--decls" => decls = parse_usize(args.next(), "--decls"),
            "--iterations" => iterations = parse_usize(args.next(), "--iterations").max(1),
            "--warm" => warm = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument {other:?}; try --help");
                std::process::exit(2);
            }
        }
    }
    Config {
        mode,
        bodies,
        decls,
        iterations,
        warm,
    }
}

fn parse_usize(value: Option<String>, flag: &str) -> usize {
    match value.as_deref().map(str::parse::<usize>) {
        Some(Ok(n)) => n,
        _ => {
            eprintln!("{flag} requires a non-negative integer");
            std::process::exit(2);
        }
    }
}

fn print_help() {
    eprintln!(
        "RUE-1086 scaling measurement runner\n\n\
         USAGE: rue-scaling-bench [--mode timing|memory|alloc] [--bodies N] \
         [--decls N] [--iterations N] [--warm]\n\n\
         --mode timing   wall time of a compile (default)\n\
         --mode memory   peak resident memory (VmHWM/rusage) of a compile\n\
         --mode alloc    allocation count/bytes (needs the -allocations binary)\n\
         --warm          measure warm single-body-edit latency (two revisions)\n\
         --bodies N      reached bodies (default 1000)\n\
         --decls N       unrelated declarations (default 100)\n\
         --iterations N  samples for timing mode (default 5)"
    );
}

// ---------------------------------------------------------------------------
// Compile drivers (public API only; no internal counters here)
// ---------------------------------------------------------------------------

/// One cold compile in a fresh session, driven through the public artifact
/// query. Returns the time spent in `semantic()`.
fn cold_compile(bodies: usize, decls: usize) -> Duration {
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session
        .update(&snapshot(corpus_source(bodies, decls)))
        .into_result()
        .expect("corpus parses");
    let start = Instant::now();
    session.semantic(&options).expect("corpus compiles");
    start.elapsed()
}

/// Warm single-body-edit latency: prime rev1, then time only the rev2 recompile
/// after editing exactly one reached body's text. This is the incremental edit
/// latency RUE-1086 asks for, distinct from the cold number.
fn warm_edit_compile(bodies: usize, decls: usize) -> Duration {
    let options = CompileOptions::default();
    let rev1 = corpus_source(bodies, decls);
    let rev2 = rev1.replacen("fn b0() -> i32 { 0 }", "fn b0() -> i32 { 123 }", 1);
    assert_ne!(rev1, rev2, "the warm edit must change source");

    let mut session = CompilerSession::new();
    session
        .update(&snapshot(rev1))
        .into_result()
        .expect("rev1 parses");
    session.semantic(&options).expect("rev1 compiles");
    session
        .update(&snapshot(rev2))
        .into_result()
        .expect("rev2 parses");
    let start = Instant::now();
    session.semantic(&options).expect("rev2 compiles");
    start.elapsed()
}

fn run_timing(config: &Config) {
    let kind = if config.warm { "warm edit" } else { "cold" };
    eprintln!(
        "\n== timing mode ({kind}) : {} bodies x {} decls, {} iterations ==",
        config.bodies, config.decls, config.iterations
    );
    // One untimed warmup keeps allocator/OS first-touch noise out of the samples.
    if config.warm {
        warm_edit_compile(config.bodies, config.decls);
    } else {
        cold_compile(config.bodies, config.decls);
    }
    let mut samples = Vec::with_capacity(config.iterations);
    for _ in 0..config.iterations {
        let elapsed = if config.warm {
            warm_edit_compile(config.bodies, config.decls)
        } else {
            cold_compile(config.bodies, config.decls)
        };
        samples.push(elapsed);
    }
    samples.sort();
    let min = samples.first().copied().unwrap_or_default();
    let max = samples.last().copied().unwrap_or_default();
    let median = samples[samples.len() / 2];
    let sum: Duration = samples.iter().sum();
    let mean = sum / samples.len() as u32;
    eprintln!("  min={min:?} median={median:?} mean={mean:?} max={max:?}");
}

fn run_memory(config: &Config) {
    let kind = if config.warm { "warm edit" } else { "cold" };
    eprintln!(
        "\n== memory mode ({kind}) : {} bodies x {} decls ==",
        config.bodies, config.decls
    );
    if config.warm {
        warm_edit_compile(config.bodies, config.decls);
    } else {
        cold_compile(config.bodies, config.decls);
    }
    match peak_memory_bytes() {
        Some(bytes) => eprintln!(
            "  peak_resident={:.1} MiB ({bytes} bytes)",
            bytes as f64 / (1024.0 * 1024.0)
        ),
        None => eprintln!("  peak_resident=unavailable on this platform"),
    }
}

fn run_alloc(config: &Config) {
    #[cfg(rue_benchmark_allocations)]
    {
        let kind = if config.warm { "warm edit" } else { "cold" };
        eprintln!(
            "\n== alloc mode ({kind}) : {} bodies x {} decls ==",
            config.bodies, config.decls
        );
        allocation::begin();
        if config.warm {
            warm_edit_compile(config.bodies, config.decls);
        } else {
            cold_compile(config.bodies, config.decls);
        }
        allocation::finish();
        let metrics = allocation::snapshot();
        eprintln!(
            "  allocations={} requested_bytes={} ({:.1} MiB)",
            metrics.allocations,
            metrics.allocated_bytes,
            metrics.allocated_bytes as f64 / (1024.0 * 1024.0)
        );
    }
    #[cfg(not(rue_benchmark_allocations))]
    {
        let _ = config;
        eprintln!(
            "\nalloc mode requires the counting global allocator; build and run \
             //crates/rue-scaling-bench:rue-scaling-bench-allocations instead."
        );
        std::process::exit(2);
    }
}

fn main() {
    let config = parse_config();
    print_provenance();
    match config.mode {
        Mode::Timing => run_timing(&config),
        Mode::Memory => run_memory(&config),
        Mode::Alloc => run_alloc(&config),
    }
}
