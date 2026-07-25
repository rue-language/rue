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
//! # Honest measurement scopes
//!
//! * **Timing** reports two separately labeled intervals per run: `semantic`
//!   (time in `CompilerSession::semantic`) and `pre_link` (semantic PLUS the
//!   backend tail through CFG lowering, code generation, and object-file
//!   creation — everything before linking, via
//!   `rue_compiler::unstable::pre_link_object_bytes`). The ~45 ms Caldera target
//!   is a *pre-link* number, so `pre_link` is its instrument; `semantic` is
//!   never labeled pre-link.
//! * **Allocation** in `--warm` mode snapshots the counting allocator's totals
//!   immediately before the rev2 source update and reports the DELTA as the
//!   warm-edit allocations over the same update-through-object-generation scope
//!   as the timed `pre_link` interval, alongside a separately labeled whole-run
//!   total that still includes rev1 priming.
//! * **Memory** peak: a cold compile's peak is measured in a *child process* so
//!   the parent's warmup does not pollute the process high-water mark; a warm
//!   edit cannot be isolated from priming by a process HWM, so it is labeled
//!   `session_peak_including_priming`. The VmHWM/rusage path is Linux-specific;
//!   elsewhere the runner prints an explicit `unavailable on this platform`
//!   marker.
//!
//! # Reproducible commands
//!
//! Build/run the release target directly — `scripts/rue-bin` builds the *CLI*,
//! not this benchmark, so it is NOT the release entry point here. Use Buck2's
//! release platform against these targets:
//!
//! ```text
//! # Cold semantic + pre-link latency, 5 samples:
//! ./buck2 run --target-platforms //platforms:release \
//!     //crates/rue-scaling-bench:rue-scaling-bench -- \
//!     --mode timing --bodies 1000 --decls 100 --iterations 5
//! # Warm single-body-edit latency (two-revision scenario), 20 samples, JSON:
//! ./buck2 run --target-platforms //platforms:release \
//!     //crates/rue-scaling-bench:rue-scaling-bench -- \
//!     --mode timing --warm --bodies 1000 --decls 100 --iterations 20 --json
//! # Peak resident memory (VmHWM / rusage) for a cold compile (child process):
//! ./buck2 run --target-platforms //platforms:release \
//!     //crates/rue-scaling-bench:rue-scaling-bench -- \
//!     --mode memory --bodies 1000 --decls 100
//! # Allocation counts (distinct binary; counting global allocator):
//! ./buck2 run --target-platforms //platforms:release \
//!     //crates/rue-scaling-bench:rue-scaling-bench-allocations -- \
//!     --mode alloc --warm --bodies 1000 --decls 100
//! ```
//!
//! Every run prints a provenance header (nproc, total memory, commit hash) so a
//! recorded baseline is attributable to a concrete reference host and revision.
//! The ~45 ms pre-link target from RUE-1083's Caldera prediction is an eventual
//! reference-host goal, not a gate this runner enforces.

use std::time::{Duration, Instant};

use rue_compiler::unstable::{CloneProbeMetrics, CloneProbeMode};
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

/// Peak resident set size in bytes (VmHWM on Linux). Returns `None` off Linux so
/// callers print an explicit `unavailable on this platform` marker.
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
    /// RUE-1133 clone-from-template probe: one point on the declaration-size
    /// curve measured in both arms, plus a parity run.
    Probe,
}

struct Config {
    mode: Mode,
    bodies: usize,
    decls: usize,
    iterations: usize,
    warm: bool,
    json: bool,
    /// RUE-1133 probe arm applied to `timing`, `memory`, and `alloc` runs.
    /// `--mode probe` drives both arms itself and ignores this.
    clone_probe: Option<CloneProbeMode>,
}

fn parse_config() -> Config {
    let mut mode = Mode::Timing;
    let mut bodies = 1_000usize;
    let mut decls = 100usize;
    let mut iterations = 5usize;
    let mut warm = false;
    let mut json = false;
    let mut clone_probe = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => {
                mode = match args.next().as_deref() {
                    Some("timing") => Mode::Timing,
                    Some("memory") => Mode::Memory,
                    Some("alloc") => Mode::Alloc,
                    Some("probe") => Mode::Probe,
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
            "--json" => json = true,
            "--clone-probe" => {
                clone_probe = match args.next().as_deref() {
                    Some("clone") => Some(CloneProbeMode::CloneOnly),
                    Some("differential") => Some(CloneProbeMode::Differential),
                    Some("off") => None,
                    other => {
                        eprintln!("unknown --clone-probe {other:?} (off|clone|differential)");
                        std::process::exit(2);
                    }
                };
            }
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
        json,
        clone_probe,
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
         USAGE: rue-scaling-bench [--mode timing|memory|alloc|probe] [--bodies N] \
         [--decls N] [--iterations N] [--warm] [--json]\n\n\
         --mode timing   wall time of a compile (default); reports `semantic` \
         and `pre_link` intervals\n\
         --mode memory   peak resident memory (VmHWM/rusage); cold via child \
         process, warm labeled session_peak_including_priming\n\
         --mode alloc    allocation count/bytes (needs the -allocations binary); \
         warm reports the edit DELTA plus a whole-run total\n\
         --mode probe    RUE-1133 clone-from-template probe: rebuild arm vs \
         clone arm at one curve point, plus a parity run\n\
         --clone-probe M run timing/memory/alloc under the probe \
         (off|clone|differential)\n\
         --warm          measure warm single-body-edit latency (two revisions)\n\
         --bodies N      reached bodies (default 1000)\n\
         --decls N       unrelated declarations (default 100)\n\
         --iterations N  samples for timing mode (default 5)\n\
         --json          emit per-sample JSON to stdout (timing mode)"
    );
}

// ---------------------------------------------------------------------------
// Compile drivers (public API only)
// ---------------------------------------------------------------------------

/// The two separately labeled timing intervals a single compile produces.
#[derive(Debug, Clone, Copy)]
struct Intervals {
    /// Time spent in `CompilerSession::semantic`.
    semantic: Duration,
    /// Semantic PLUS the backend tail through object generation (pre-link).
    pre_link: Duration,
}

/// One cold compile in a fresh session. The compiler pipeline starts at the
/// source snapshot, so `pre_link` runs from immediately before
/// `session.update` (parse and invalidation are compiler work) through object
/// generation, stopping before linking. `semantic` remains the useful
/// semantic-only sub-interval. Snapshot construction is corpus setup and stays
/// outside both intervals.
fn cold_intervals(bodies: usize, decls: usize, clone_probe: Option<CloneProbeMode>) -> Intervals {
    let options = CompileOptions::default();
    let source = snapshot(corpus_source(bodies, decls));
    let mut session = CompilerSession::new();
    rue_compiler::unstable::enable_clone_from_template_probe(&mut session, clone_probe);
    let start = Instant::now();
    session
        .update(&source)
        .into_result()
        .expect("corpus parses");
    let semantic_start = Instant::now();
    session.semantic(&options).expect("corpus compiles");
    let semantic = semantic_start.elapsed();
    rue_compiler::unstable::pre_link_object_bytes(&mut session, &options)
        .expect("pre-link backend succeeds");
    let pre_link = start.elapsed();
    Intervals { semantic, pre_link }
}

/// Warm single-body-edit latency: prime rev1 fully through pre-link, edit exactly
/// one reached body's text, then measure the rev2 recompile. The edit's compiler
/// work starts at the source update — parse and invalidation are caused by the
/// edit — so `pre_link` runs from immediately before `session.update(rev2)`
/// through object generation. `semantic` remains the semantic-only
/// sub-interval; rev2 snapshot construction stays outside both.
fn warm_intervals(bodies: usize, decls: usize, clone_probe: Option<CloneProbeMode>) -> Intervals {
    let options = CompileOptions::default();
    let rev1 = corpus_source(bodies, decls);
    let rev2 = rev1.replacen("fn b0() -> i32 { 0 }", "fn b0() -> i32 { 123 }", 1);
    assert_ne!(rev1, rev2, "the warm edit must change source");

    let mut session = CompilerSession::new();
    rue_compiler::unstable::enable_clone_from_template_probe(&mut session, clone_probe);
    session
        .update(&snapshot(rev1))
        .into_result()
        .expect("rev1 parses");
    // Prime rev1 through the same pre-link tail so warmed backend caches are not
    // charged to the timed rev2 edit.
    session.semantic(&options).expect("rev1 compiles");
    rue_compiler::unstable::pre_link_object_bytes(&mut session, &options)
        .expect("rev1 pre-link succeeds");
    let rev2_snapshot = snapshot(rev2);
    let start = Instant::now();
    session
        .update(&rev2_snapshot)
        .into_result()
        .expect("rev2 parses");
    let semantic_start = Instant::now();
    session.semantic(&options).expect("rev2 compiles");
    let semantic = semantic_start.elapsed();
    rue_compiler::unstable::pre_link_object_bytes(&mut session, &options)
        .expect("rev2 pre-link succeeds");
    let pre_link = start.elapsed();
    Intervals { semantic, pre_link }
}

fn measure_intervals(config: &Config) -> Intervals {
    if config.warm {
        warm_intervals(config.bodies, config.decls, config.clone_probe)
    } else {
        cold_intervals(config.bodies, config.decls, config.clone_probe)
    }
}

// ---------------------------------------------------------------------------
// Timing mode
// ---------------------------------------------------------------------------

struct Stats {
    min: Duration,
    median: Duration,
    mean: Duration,
    max: Duration,
}

fn summarize(mut samples: Vec<Duration>) -> Stats {
    samples.sort();
    let min = samples.first().copied().unwrap_or_default();
    let max = samples.last().copied().unwrap_or_default();
    let median = samples[samples.len() / 2];
    let sum: Duration = samples.iter().sum();
    let mean = sum / samples.len() as u32;
    Stats {
        min,
        median,
        mean,
        max,
    }
}

fn run_timing(config: &Config) {
    let kind = if config.warm { "warm edit" } else { "cold" };
    eprintln!(
        "\n== timing mode ({kind}) : {} bodies x {} decls, {} iterations ==",
        config.bodies, config.decls, config.iterations
    );
    // One untimed warmup keeps allocator/OS first-touch noise out of the samples.
    measure_intervals(config);

    let mut semantic_samples = Vec::with_capacity(config.iterations);
    let mut pre_link_samples = Vec::with_capacity(config.iterations);
    for _ in 0..config.iterations {
        let intervals = measure_intervals(config);
        semantic_samples.push(intervals.semantic);
        pre_link_samples.push(intervals.pre_link);
    }

    let semantic = summarize(semantic_samples.clone());
    let pre_link = summarize(pre_link_samples.clone());
    eprintln!(
        "  semantic : min={:?} median={:?} mean={:?} max={:?}",
        semantic.min, semantic.median, semantic.mean, semantic.max
    );
    eprintln!(
        "  pre_link : min={:?} median={:?} mean={:?} max={:?}",
        pre_link.min, pre_link.median, pre_link.mean, pre_link.max
    );

    if config.json {
        emit_timing_json(config, kind, &semantic_samples, &pre_link_samples);
    }
}

/// Emit machine-readable per-sample JSON (not aggregates only) to stdout so a
/// checked-in raw artifact can capture every sample, per ADR-0066's raw-evidence
/// requirement. Provenance stays on stderr.
fn emit_timing_json(
    config: &Config,
    kind: &str,
    semantic_samples: &[Duration],
    pre_link_samples: &[Duration],
) {
    let ns = |samples: &[Duration]| {
        samples
            .iter()
            .map(|d| d.as_nanos().to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let summary = |samples: &[Duration]| {
        let s = summarize(samples.to_vec());
        format!(
            "\"min_ns\":{},\"median_ns\":{},\"mean_ns\":{},\"max_ns\":{}",
            s.min.as_nanos(),
            s.median.as_nanos(),
            s.mean.as_nanos(),
            s.max.as_nanos()
        )
    };
    println!(
        "{{\"kind\":\"timing\",\"mode\":\"{}\",\"bodies\":{},\"decls\":{},\"iterations\":{},\
         \"commit\":\"{}\",\"nproc\":{},\
         \"semantic\":{{\"samples_ns\":[{}],{}}},\
         \"pre_link\":{{\"samples_ns\":[{}],{}}}}}",
        if kind == "warm edit" { "warm" } else { "cold" },
        config.bodies,
        config.decls,
        config.iterations,
        commit_hash(),
        nproc(),
        ns(semantic_samples),
        summary(semantic_samples),
        ns(pre_link_samples),
        summary(pre_link_samples),
    );
}

// ---------------------------------------------------------------------------
// Clone-from-template probe mode (RUE-1133 / RUE-1091 ordered probe #1)
// ---------------------------------------------------------------------------
//
// One point on the declaration-size curve, measured in both arms:
//
//   rebuild arm — production today: every reached body derives the whole
//                 declaration epoch before analyzing its one body.
//   clone arm   — the probe: the epoch is derived once for the revision and
//                 deep-copied per body.
//
// The difference is the derivation cost the rewire can delete. What is LEFT in
// the clone arm is the structural floor: the cost of moving a whole declaration
// epoch per body, which no scheme that still materializes per-body declaration
// state can get below. A parity run then establishes that the clone arm
// publishes the same program, so the timing comparison is about one variable.

/// One cold semantic-only compile, returning the `semantic` interval and the
/// probe meter afterwards.
fn probe_semantic(
    bodies: usize,
    decls: usize,
    clone_probe: Option<CloneProbeMode>,
) -> (Duration, CloneProbeMetrics) {
    let options = CompileOptions::default();
    let source = snapshot(corpus_source(bodies, decls));
    let mut session = CompilerSession::new();
    rue_compiler::unstable::enable_clone_from_template_probe(&mut session, clone_probe);
    session
        .update(&source)
        .into_result()
        .expect("corpus parses");
    let started = Instant::now();
    session.semantic(&options).expect("corpus compiles");
    let semantic = started.elapsed();
    (
        semantic,
        rue_compiler::unstable::clone_from_template_probe_metrics(&session),
    )
}

fn run_probe(config: &Config) {
    eprintln!(
        "\n== clone-from-template probe (RUE-1133) : {} bodies x {} decls, {} iterations ==",
        config.bodies, config.decls, config.iterations
    );
    // One untimed warmup per arm keeps allocator/OS first-touch noise out of the
    // samples, exactly as timing mode does.
    probe_semantic(config.bodies, config.decls, None);
    probe_semantic(config.bodies, config.decls, Some(CloneProbeMode::CloneOnly));

    let mut rebuild_samples = Vec::with_capacity(config.iterations);
    let mut clone_samples = Vec::with_capacity(config.iterations);
    let mut metrics = CloneProbeMetrics::default();
    for _ in 0..config.iterations {
        rebuild_samples.push(probe_semantic(config.bodies, config.decls, None).0);
        let (elapsed, sample) =
            probe_semantic(config.bodies, config.decls, Some(CloneProbeMode::CloneOnly));
        clone_samples.push(elapsed);
        metrics = sample;
    }
    let rebuild = summarize(rebuild_samples.clone());
    let clone = summarize(clone_samples.clone());

    // The parity run is separate and never timed: it analyzes every body twice
    // and compares the published transactions.
    let (_, parity) = probe_semantic(
        config.bodies,
        config.decls,
        Some(CloneProbeMode::Differential),
    );

    eprintln!(
        "  rebuild arm semantic : min={:?} median={:?} mean={:?} max={:?}",
        rebuild.min, rebuild.median, rebuild.mean, rebuild.max
    );
    eprintln!(
        "  clone arm   semantic : min={:?} median={:?} mean={:?} max={:?}",
        clone.min, clone.median, clone.mean, clone.max
    );
    let speedup = rebuild.median.as_secs_f64() / clone.median.as_secs_f64().max(f64::MIN_POSITIVE);
    eprintln!("  median clone-arm speedup : {speedup:.2}x");
    eprintln!(
        "  probe: bodies={} templates_built={} templates_cloned={} template_input_misses={}",
        metrics.bodies_analyzed,
        metrics.templates_built,
        metrics.templates_cloned,
        metrics.template_input_misses
    );
    eprintln!(
        "  probe timing (in-session): template_build={:?} clone={:?} analyze={:?}",
        Duration::from_nanos(metrics.template_build_nanos),
        Duration::from_nanos(metrics.clone_nanos),
        Duration::from_nanos(metrics.analyze_nanos)
    );
    let units = metrics.units;
    eprintln!(
        "  units copied: declaration_namespace={} type_pool={} parameters={} modules={} endpoints={} total={}",
        units.declaration_namespace_entries_copied,
        units.type_pool_entries_copied,
        units.parameter_entries_copied,
        units.module_registry_entries_copied,
        units.endpoint_entries_copied,
        units.total_units_copied()
    );
    if metrics.templates_cloned > 0 {
        eprintln!(
            "  units per cloned body: {}",
            units.total_units_copied() / metrics.templates_cloned
        );
    }
    eprintln!(
        "  parity: comparisons={} mismatches={}",
        parity.parity_comparisons, parity.parity_mismatches
    );
    assert_eq!(
        parity.parity_mismatches, 0,
        "a clone-arm body published a different transaction than the rebuild arm; \
         the probe's timing result would be meaningless"
    );

    if config.json {
        emit_probe_json(config, &rebuild_samples, &clone_samples, &metrics, &parity);
    }
}

/// Machine-readable per-sample probe JSON, matching the raw-evidence shape the
/// timing mode already emits.
fn emit_probe_json(
    config: &Config,
    rebuild_samples: &[Duration],
    clone_samples: &[Duration],
    metrics: &CloneProbeMetrics,
    parity: &CloneProbeMetrics,
) {
    let ns = |samples: &[Duration]| {
        samples
            .iter()
            .map(|d| d.as_nanos().to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let summary = |samples: &[Duration]| {
        let s = summarize(samples.to_vec());
        format!(
            "\"min_ns\":{},\"median_ns\":{},\"mean_ns\":{},\"max_ns\":{}",
            s.min.as_nanos(),
            s.median.as_nanos(),
            s.mean.as_nanos(),
            s.max.as_nanos()
        )
    };
    let units = metrics.units;
    println!(
        "{{\"kind\":\"clone_from_template_probe\",\"issue\":\"RUE-1133\",\
         \"bodies\":{},\"decls\":{},\"iterations\":{},\"commit\":\"{}\",\"nproc\":{},\
         \"rebuild_semantic\":{{\"samples_ns\":[{}],{}}},\
         \"clone_semantic\":{{\"samples_ns\":[{}],{}}},\
         \"probe\":{{\"bodies_analyzed\":{},\"templates_built\":{},\"templates_cloned\":{},\
         \"template_input_misses\":{},\"template_build_nanos\":{},\"clone_nanos\":{},\
         \"analyze_nanos\":{}}},\
         \"units\":{{\"declaration_namespace\":{},\"type_pool\":{},\"parameters\":{},\
         \"modules\":{},\"endpoints\":{},\"total\":{}}},\
         \"parity\":{{\"comparisons\":{},\"mismatches\":{}}}}}",
        config.bodies,
        config.decls,
        config.iterations,
        commit_hash(),
        nproc(),
        ns(rebuild_samples),
        summary(rebuild_samples),
        ns(clone_samples),
        summary(clone_samples),
        metrics.bodies_analyzed,
        metrics.templates_built,
        metrics.templates_cloned,
        metrics.template_input_misses,
        metrics.template_build_nanos,
        metrics.clone_nanos,
        metrics.analyze_nanos,
        units.declaration_namespace_entries_copied,
        units.type_pool_entries_copied,
        units.parameter_entries_copied,
        units.module_registry_entries_copied,
        units.endpoint_entries_copied,
        units.total_units_copied(),
        parity.parity_comparisons,
        parity.parity_mismatches,
    );
}

// ---------------------------------------------------------------------------
// Memory mode
// ---------------------------------------------------------------------------

/// Hidden child-process entry: run exactly one cold pre-link compile with no
/// warmup, then print this process's isolated VmHWM to stdout. The parent spawns
/// it so its own warmup does not inflate the reported cold peak.
const INTERNAL_COLD_PEAK: &str = "--internal-cold-peak";

fn run_internal_cold_peak(bodies: usize, decls: usize, clone_probe: Option<CloneProbeMode>) -> ! {
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    rue_compiler::unstable::enable_clone_from_template_probe(&mut session, clone_probe);
    session
        .update(&snapshot(corpus_source(bodies, decls)))
        .into_result()
        .expect("corpus parses");
    session.semantic(&options).expect("corpus compiles");
    rue_compiler::unstable::pre_link_object_bytes(&mut session, &options)
        .expect("pre-link backend succeeds");
    match peak_memory_bytes() {
        Some(bytes) => println!("COLD_PEAK_BYTES={bytes}"),
        None => println!("COLD_PEAK_BYTES=NA"),
    }
    std::process::exit(0);
}

fn run_memory(config: &Config) {
    let kind = if config.warm { "warm edit" } else { "cold" };
    eprintln!(
        "\n== memory mode ({kind}) : {} bodies x {} decls ==",
        config.bodies, config.decls
    );

    if config.warm {
        // A process HWM cannot separate the warm-edit peak from the rev1 priming
        // that necessarily precedes it, so this is labeled honestly as the whole
        // session's peak including priming.
        warm_intervals(config.bodies, config.decls, config.clone_probe);
        match peak_memory_bytes() {
            Some(bytes) => eprintln!(
                "  session_peak_including_priming={:.1} MiB ({bytes} bytes)",
                bytes as f64 / (1024.0 * 1024.0)
            ),
            None => eprintln!("  session_peak_including_priming=unavailable on this platform"),
        }
        return;
    }

    // Cold peak: measured in a child process with no warmup so this process's
    // own warmup/allocator state cannot pollute the high-water mark.
    match cold_peak_via_child(config.bodies, config.decls, config.clone_probe) {
        Some(Some(bytes)) => eprintln!(
            "  cold_peak_resident (isolated child process)={:.1} MiB ({bytes} bytes)",
            bytes as f64 / (1024.0 * 1024.0)
        ),
        Some(None) => eprintln!("  cold_peak_resident=unavailable on this platform"),
        None => {
            // Child spawn/exec failed: fall back to an in-process measurement,
            // clearly labeled as not isolated.
            cold_intervals(config.bodies, config.decls, config.clone_probe);
            match peak_memory_bytes() {
                Some(bytes) => eprintln!(
                    "  cold_peak_resident (in-process fallback, not isolated)={:.1} MiB ({bytes} bytes)",
                    bytes as f64 / (1024.0 * 1024.0)
                ),
                None => eprintln!("  cold_peak_resident=unavailable on this platform"),
            }
        }
    }
}

/// Spawn this executable as a child that performs one isolated cold compile.
/// Returns `None` if the child could not be run at all, `Some(None)` if the
/// child reports the platform is unsupported, and `Some(Some(bytes))` on success.
fn cold_peak_via_child(
    bodies: usize,
    decls: usize,
    clone_probe: Option<CloneProbeMode>,
) -> Option<Option<u64>> {
    let exe = std::env::current_exe().ok()?;
    let mut command = std::process::Command::new(exe);
    command
        .arg(INTERNAL_COLD_PEAK)
        .arg("--bodies")
        .arg(bodies.to_string())
        .arg("--decls")
        .arg(decls.to_string());
    if let Some(mode) = clone_probe {
        command.arg("--clone-probe").arg(match mode {
            CloneProbeMode::CloneOnly => "clone",
            CloneProbeMode::Differential => "differential",
        });
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("COLD_PEAK_BYTES=") {
            if rest == "NA" {
                return Some(None);
            }
            return Some(rest.parse::<u64>().ok());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Allocation mode
// ---------------------------------------------------------------------------

fn run_alloc(config: &Config) {
    #[cfg(rue_benchmark_allocations)]
    {
        let kind = if config.warm { "warm edit" } else { "cold" };
        eprintln!(
            "\n== alloc mode ({kind}) : {} bodies x {} decls ==",
            config.bodies, config.decls
        );

        if config.warm {
            let options = CompileOptions::default();
            let rev1 = corpus_source(config.bodies, config.decls);
            let rev2 = rev1.replacen("fn b0() -> i32 { 0 }", "fn b0() -> i32 { 123 }", 1);
            assert_ne!(rev1, rev2, "the warm edit must change source");

            let mut session = CompilerSession::new();
            rue_compiler::unstable::enable_clone_from_template_probe(
                &mut session,
                config.clone_probe,
            );
            session
                .update(&snapshot(rev1))
                .into_result()
                .expect("rev1 parses");

            // Count from here so the whole-run total includes rev1 priming.
            allocation::begin();
            session.semantic(&options).expect("rev1 compiles");
            rue_compiler::unstable::pre_link_object_bytes(&mut session, &options)
                .expect("rev1 pre-link succeeds");
            // Snapshot immediately before the rev2 source update; the DELTA to
            // the post-edit snapshot is the warm-edit allocation cost over the
            // same request scope as the timed pre_link interval (update through
            // object generation), excluding all priming. Snapshot construction
            // stays outside the delta as corpus setup.
            let rev2_snapshot = snapshot(rev2);
            let before = allocation::snapshot();
            session
                .update(&rev2_snapshot)
                .into_result()
                .expect("rev2 parses");
            session.semantic(&options).expect("rev2 compiles");
            rue_compiler::unstable::pre_link_object_bytes(&mut session, &options)
                .expect("rev2 pre-link succeeds");
            let after = allocation::snapshot();
            allocation::finish();

            let edit_allocations = after.allocations.saturating_sub(before.allocations);
            let edit_bytes = after.allocated_bytes.saturating_sub(before.allocated_bytes);
            eprintln!(
                "  warm_edit_allocations={} warm_edit_bytes={} ({:.1} MiB) [delta over the rev2 update-through-object-generation scope]",
                edit_allocations,
                edit_bytes,
                edit_bytes as f64 / (1024.0 * 1024.0)
            );
            eprintln!(
                "  whole_run_allocations={} whole_run_bytes={} ({:.1} MiB) [includes rev1 priming]",
                after.allocations,
                after.allocated_bytes,
                after.allocated_bytes as f64 / (1024.0 * 1024.0)
            );
        } else {
            allocation::begin();
            let intervals_start = allocation::snapshot();
            cold_compile_for_alloc(config.bodies, config.decls, config.clone_probe);
            let after = allocation::snapshot();
            allocation::finish();
            let allocations = after
                .allocations
                .saturating_sub(intervals_start.allocations);
            let bytes = after
                .allocated_bytes
                .saturating_sub(intervals_start.allocated_bytes);
            eprintln!(
                "  cold_allocations={} cold_bytes={} ({:.1} MiB)",
                allocations,
                bytes,
                bytes as f64 / (1024.0 * 1024.0)
            );
        }
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

#[cfg(rue_benchmark_allocations)]
fn cold_compile_for_alloc(bodies: usize, decls: usize, clone_probe: Option<CloneProbeMode>) {
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    rue_compiler::unstable::enable_clone_from_template_probe(&mut session, clone_probe);
    session
        .update(&snapshot(corpus_source(bodies, decls)))
        .into_result()
        .expect("corpus parses");
    session.semantic(&options).expect("corpus compiles");
    rue_compiler::unstable::pre_link_object_bytes(&mut session, &options)
        .expect("pre-link backend succeeds");
}

fn main() {
    // Hidden child-process entry for isolated cold-peak memory measurement.
    let raw: Vec<String> = std::env::args().collect();
    if raw.iter().any(|arg| arg == INTERNAL_COLD_PEAK) {
        let mut bodies = 1_000usize;
        let mut decls = 100usize;
        let mut clone_probe = None;
        let mut iter = raw.iter().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--bodies" => bodies = iter.next().and_then(|v| v.parse().ok()).unwrap_or(bodies),
                "--decls" => decls = iter.next().and_then(|v| v.parse().ok()).unwrap_or(decls),
                "--clone-probe" => {
                    clone_probe = match iter.next().map(String::as_str) {
                        Some("clone") => Some(CloneProbeMode::CloneOnly),
                        Some("differential") => Some(CloneProbeMode::Differential),
                        _ => None,
                    };
                }
                _ => {}
            }
        }
        run_internal_cold_peak(bodies, decls, clone_probe);
    }

    let config = parse_config();
    print_provenance();
    match config.mode {
        Mode::Timing => run_timing(&config),
        Mode::Memory => run_memory(&config),
        Mode::Alloc => run_alloc(&config),
        Mode::Probe => run_probe(&config),
    }
}
