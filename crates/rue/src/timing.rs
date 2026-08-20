//! Timing infrastructure for `--time-passes` and `--benchmark-json`.
//!
//! This module provides a tracing layer that collects timing information from
//! compiler passes. It uses the tracing span lifecycle to measure how long each
//! pass takes, then formats a summary report.
//!
//! # Architecture
//!
//! The timing system is built on tracing's layer architecture:
//!
//! 1. **Instrumentation**: Compiler passes are wrapped in tracing spans
//!    like `info_span!("lexer")`. These are zero-cost when no subscriber collects them.
//!
//! 2. **Collection** (this module): `TimingLayer` implements `tracing_subscriber::Layer`
//!    to hook into span enter/exit events. Each worker appends to a thread-local
//!    buffer; bounded worker completion and benchmark finalization publish those
//!    buffers.
//!
//! 3. **Reporting**: After compilation, `TimingData::report()` formats the collected
//!    timing as a human-readable table. For machine-readable output, use
//!    `TimingData::to_json()`.
//!
//! # Two timing models, kept distinct
//!
//! This module publishes two kinds of phase measurement, and they must never be
//! mixed in one visualization (ADR-0067).
//!
//! **Inclusive spans** are the `passes` table. Spans nest and overlap, a child's
//! duration is contained in its parent's, and backend subphases can run
//! concurrently across query workers. Summing them double-counts, which is why
//! the root total is a union of active intervals rather than a sum.
//!
//! **Wall-clock phase accounting** is [`BenchmarkTiming::phase_accounting`]. A
//! small set of explicitly instrumented, mutually exclusive phases partitions
//! compiler-root wall time so that, in exact integer nanoseconds:
//!
//! ```text
//! sum(phase_ns) + mixed_parallel_ns + unattributed_ns == compiler_root_ns
//! ```
//!
//! Only this model may be stacked.
//!
//! ## Declaring a phase
//!
//! Membership is declared at the instrumentation site with a `phase = "..."`
//! span field naming a `rue_perf_schema::Phase`, never inferred from span names.
//! Compiler spans nest deeply, so a name-based mapping would read a nested span
//! as a second concurrent phase and charge the interval to `mixed_parallel`.
//! Unlike `driver_phase`, the marker is not inherited: a marker on a descendant
//! is a phase *transition*, and an unmarked child simply continues its parent's
//! phase.
//!
//! Two rules follow for anyone adding a marker:
//!
//! * **Mark work and compiler-owned query consumers.** Validation, dispatch,
//!   and terminal collection belong to the phase whose artifact is requested.
//!   A broad consumer boundary may enclose same-phase query work, but must end
//!   before a consumer for another phase begins.
//! * **Distinct published phases must not nest.** If two different markers can
//!   be active at once, redraw the boundary rather than accepting the mixed band.
//!
//! Because attribution follows the demand context, work reached lazily is
//! charged to the phase that demanded it — on-demand parsing inside semantic
//! analysis counts as semantic analysis. Total parse cost remains available in
//! the inclusive `passes` table, which is exactly what the two-model split is
//! for.
//!
//! `mixed_parallel` and `unattributed` are published bands, not artifacts.
//! Growth in the first means the boundaries no longer describe the compiler's
//! parallel structure; growth in the second means compiler time is moving
//! somewhere the instrumentation does not describe.
//!
//! # Example
//!
//! ```ignore
//! use crate::timing::{TimingLayer, TimingData};
//!
//! let timing_data = TimingData::new();
//! let timing_layer = TimingLayer::new(timing_data.clone());
//!
//! // Install as a tracing subscriber layer
//! let subscriber = Registry::default().with(timing_layer);
//! tracing::subscriber::set_global_default(subscriber).unwrap();
//!
//! // ... run compilation ...
//!
//! // Print the timing report
//! eprintln!("{}", timing_data.report());
//!
//! // Or get JSON for benchmarking
//! println!("{}", timing_data.to_json());
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
#[cfg(test)]
use std::thread::ThreadId;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(rue_release_build)]
const COMPILER_BUILD_PROFILE: &str = "release_thin_lto";
#[cfg(not(rue_release_build))]
const COMPILER_BUILD_PROFILE: &str = "debug";

use rue_perf_schema::{CompilerWork, DurationDistribution, Phase, PhaseAccounting};
use serde::Serialize;

use tracing::span::{Attributes, Id};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Accumulated timing data for all compiler passes.
///
/// Hot-path observations are accumulated in thread-local buffers. The shared
/// state is touched only when a worker completes or a report explicitly
/// publishes the calling thread, so parallel query execution never serializes
/// on this value.
#[derive(Clone)]
pub struct TimingData {
    id: u64,
    inner: Arc<Mutex<TimingDataInner>>,
}

static NEXT_TIMING_DATA_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static LOCAL_TIMING: RefCell<HashMap<u64, LocalTiming>> = RefCell::new(HashMap::new());
}

/// The actual timing data storage.
struct TimingDataInner {
    /// Accumulated measurements per pass name.
    /// Key is the span name (e.g., "lexer", "parser").
    passes: HashMap<String, PassAggregate>,
    /// Exact structural counters published by wide timing events.
    counters: HashMap<String, u64>,

    /// Accumulated measurements per driver-phase name.
    ///
    /// Driver phases are host work outside the compiler's timing root, such as
    /// writing the linked executable. They are measured with the same span
    /// machinery but kept out of `passes` and out of `root_duration`, so
    /// `total_ms` keeps meaning "compiler work" and `compile` stays the sole
    /// timing root (RUE-786).
    driver_phases: HashMap<String, DriverPhaseAggregate>,

    /// Timestamped root and phase transitions, merged once per worker. Sorting
    /// this bounded event stream at finalization reconstructs the one global
    /// wall-clock partition without coordinating its producers.
    accounting_events: Vec<AccountingEvent>,

    /// Test-only fabricated root time. Runtime root time always comes from the
    /// timestamped event stream above.
    #[cfg(test)]
    synthetic_unattributed: Duration,

    /// Direct parent-child span names captured by the real timing layer.
    ///
    /// This remains test-only so parentage regressions can be checked without
    /// expanding the public benchmark JSON schema.
    #[cfg(test)]
    parent_edges: Vec<(String, String)>,

    /// Threads whose real `timing_flush` lifecycle marker published a local
    /// buffer. Kept out of production state and the benchmark schema.
    #[cfg(test)]
    flush_threads: Vec<ThreadId>,
}

/// Measurements aggregated across every span with the same name.
#[derive(Debug, Clone, Copy)]
struct PassAggregate {
    duration: Duration,
    max_duration: Duration,
    invocations: u64,
    root_invocations: u64,
    leaf_invocations: u64,
    duration_log2_buckets: [u64; 64],
}

impl Default for PassAggregate {
    fn default() -> Self {
        Self {
            duration: Duration::ZERO,
            max_duration: Duration::ZERO,
            invocations: 0,
            root_invocations: 0,
            leaf_invocations: 0,
            duration_log2_buckets: [0; 64],
        }
    }
}

/// Measurements aggregated across every driver-phase span with the same name.
#[derive(Debug, Clone, Copy, Default)]
struct DriverPhaseAggregate {
    duration: Duration,
    invocations: u64,
}

#[derive(Debug, Clone, Copy)]
struct AccountingEvent {
    at: Instant,
    transition: AccountingTransition,
}

#[derive(Debug, Clone, Copy)]
enum AccountingTransition {
    RootEnter,
    RootExit,
    PhaseEnter(Phase),
    PhaseExit(Phase),
}

/// One producer's unsynchronized accumulation. Query batch workers publish
/// their buffers at bounded worker completion, with thread teardown as a
/// fallback; the compiler thread publishes its buffer when a report is
/// requested.
struct LocalTiming {
    target: Weak<Mutex<TimingDataInner>>,
    passes: HashMap<String, PassAggregate>,
    counters: HashMap<String, u64>,
    driver_phases: HashMap<String, DriverPhaseAggregate>,
    accounting_events: Vec<AccountingEvent>,
    #[cfg(test)]
    synthetic_unattributed: Duration,
    #[cfg(test)]
    parent_edges: Vec<(String, String)>,
}

impl LocalTiming {
    fn new(target: Weak<Mutex<TimingDataInner>>) -> Self {
        Self {
            target,
            passes: HashMap::new(),
            counters: HashMap::new(),
            driver_phases: HashMap::new(),
            accounting_events: Vec::new(),
            #[cfg(test)]
            synthetic_unattributed: Duration::ZERO,
            #[cfg(test)]
            parent_edges: Vec::new(),
        }
    }
}

impl Drop for LocalTiming {
    fn drop(&mut self) {
        let Some(target) = self.target.upgrade() else {
            return;
        };
        let mut inner = target.lock().unwrap_or_else(PoisonError::into_inner);
        for (name, local) in self.passes.drain() {
            merge_pass(&mut inner.passes, name, local);
        }
        for (name, local) in self.counters.drain() {
            let aggregate = inner.counters.entry(name).or_default();
            *aggregate = aggregate.saturating_add(local);
        }
        for (name, local) in self.driver_phases.drain() {
            merge_driver_phase(&mut inner.driver_phases, name, local);
        }
        inner.accounting_events.append(&mut self.accounting_events);
        #[cfg(test)]
        {
            inner.synthetic_unattributed += self.synthetic_unattributed;
            inner.parent_edges.append(&mut self.parent_edges);
        }
    }
}

fn merge_pass(target: &mut HashMap<String, PassAggregate>, name: String, local: PassAggregate) {
    let aggregate = target.entry(name).or_default();
    aggregate.duration += local.duration;
    aggregate.max_duration = aggregate.max_duration.max(local.max_duration);
    aggregate.invocations += local.invocations;
    aggregate.root_invocations += local.root_invocations;
    aggregate.leaf_invocations += local.leaf_invocations;
    for (target, local) in aggregate
        .duration_log2_buckets
        .iter_mut()
        .zip(local.duration_log2_buckets)
    {
        *target = target.saturating_add(local);
    }
}

fn merge_driver_phase(
    target: &mut HashMap<String, DriverPhaseAggregate>,
    name: String,
    local: DriverPhaseAggregate,
) {
    let aggregate = target.entry(name).or_default();
    aggregate.duration += local.duration;
    aggregate.invocations += local.invocations;
}

/// JSON output structure for benchmark timing data.
///
/// This structure is designed for machine-readable output that can be
/// consumed by the benchmark runner and visualization tools. It includes
/// metadata for historical analysis and comparison across runs.
#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkTiming {
    /// Version of this machine-readable timing contract.
    pub schema_version: u32,
    /// Pass durations are inclusive and may overlap their parents.
    pub timing_model: &'static str,
    /// The additive wall-clock partition of compiler-root time.
    ///
    /// This is the only model that may be stacked. Its integer nanoseconds
    /// satisfy `sum(phase_ns) + mixed_parallel_ns + unattributed_ns ==
    /// compiler_root_ns` exactly. The `passes` table below is the *other*
    /// model — inclusive spans that nest and overlap — and the two must never
    /// be mixed in one visualization.
    pub phase_accounting: PhaseAccounting,
    /// Metadata about this benchmark run.
    pub metadata: BenchmarkMetadata,
    /// Individual pass timings in milliseconds.
    pub passes: Vec<PassTiming>,
    /// Driver-side phases measured outside the compiler's timing root.
    ///
    /// These break down `process - total_ms`, never `total_ms` itself, so they
    /// must not be added to the pass table. The field is omitted entirely when
    /// the run measured no driver phase.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub driver_phases: Vec<DriverPhaseTiming>,
    /// Total compilation time in milliseconds.
    pub total_ms: f64,
    /// Source and program-shape metrics for throughput calculations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_metrics: Option<SourceMetrics>,
    /// Deterministic compiler work independent of host timing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler_work: Option<CompilerWork>,
    /// Peak memory usage in bytes (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_memory_bytes: Option<u64>,
}

/// Source and program-shape metrics for throughput calculations.
#[derive(Debug, Clone, Serialize)]
pub struct SourceMetrics {
    /// Number of source files compiled.
    pub files: usize,
    /// Number of modules consumed by parsing.
    pub modules: usize,
    /// Total bytes across source files.
    pub bytes: usize,
    /// Total lines across source files.
    pub lines: usize,
    /// Total tokens produced by the lexer invocations consumed by parsing.
    pub tokens: usize,
    /// Number of source and synthesized functions considered for CFG construction.
    pub functions: usize,
}

/// Metadata about a benchmark run for historical analysis.
#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkMetadata {
    /// ISO 8601 timestamp of when the benchmark was run.
    pub timestamp: String,
    /// Compiler version.
    pub version: String,
    /// Target platform (e.g., "x86_64-linux", "aarch64-macos").
    pub target: String,
    /// Build profile of the Rust compiler binary being measured.
    pub compiler_build_profile: &'static str,
}

/// Timing data for a single compiler pass.
#[derive(Debug, Clone, Serialize)]
pub struct PassTiming {
    /// Name of the pass (e.g., "lexer", "parser").
    pub name: String,
    /// Time spent in this pass in milliseconds.
    pub duration_ms: f64,
    /// Percentage of total compilation time.
    pub percent: f64,
    /// Number of spans aggregated into this row.
    pub invocations: u64,
    /// Number of invocations without a parent span.
    pub root_invocations: u64,
    /// Number of invocations without a child span.
    pub leaf_invocations: u64,
}

/// Timing for one driver-side phase outside the compiler's timing root.
#[derive(Debug, Clone, Serialize)]
pub struct DriverPhaseTiming {
    /// Name of the driver phase (e.g., "output_write").
    pub name: String,
    /// Time spent in this phase in milliseconds.
    pub duration_ms: f64,
    /// Number of spans aggregated into this row.
    pub invocations: u64,
}

impl TimingData {
    /// Create a new empty timing data collector.
    pub fn new() -> Self {
        Self {
            id: NEXT_TIMING_DATA_ID.fetch_add(1, Ordering::Relaxed),
            inner: Arc::new(Mutex::new(TimingDataInner {
                passes: HashMap::new(),
                counters: HashMap::new(),
                driver_phases: HashMap::new(),
                accounting_events: Vec::new(),
                #[cfg(test)]
                synthetic_unattributed: Duration::ZERO,
                #[cfg(test)]
                parent_edges: Vec::new(),
                #[cfg(test)]
                flush_threads: Vec::new(),
            })),
        }
    }

    fn with_local(&self, observe: impl FnOnce(&mut LocalTiming)) {
        LOCAL_TIMING.with(|locals| {
            let mut locals = locals.borrow_mut();
            let local = locals
                .entry(self.id)
                .or_insert_with(|| LocalTiming::new(Arc::downgrade(&self.inner)));
            observe(local);
        });
    }

    fn flush_local(&self) {
        let local = LOCAL_TIMING.with(|locals| locals.borrow_mut().remove(&self.id));
        drop(local);
    }

    #[cfg(test)]
    fn record_flush_marker(&self) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush_threads
            .push(std::thread::current().id());
    }

    #[cfg(test)]
    fn flush_threads(&self) -> Vec<ThreadId> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush_threads
            .clone()
    }

    /// Inspect already-published state without flushing the calling thread.
    #[cfg(test)]
    fn published_pass(&self, name: &str) -> Option<PassAggregate> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .passes
            .get(name)
            .copied()
    }

    /// Record a standalone leaf pass.
    ///
    /// This helper is used by deterministic unit tests. Runtime spans use
    /// [`Self::record_span`] so their nesting is retained.
    #[cfg(test)]
    fn record(&self, pass: &str, duration: Duration) {
        self.record_test_span(pass, duration, true, true);
    }

    /// Record a duration and its position in the span tree.
    fn record_span(&self, pass: &str, duration: Duration, is_root: bool, is_leaf: bool) {
        self.with_local(|local| {
            let entry = local.passes.entry(pass.to_string()).or_default();
            entry.duration += duration;
            entry.max_duration = entry.max_duration.max(duration);
            entry.invocations += 1;
            entry.root_invocations += u64::from(is_root);
            entry.leaf_invocations += u64::from(is_leaf);
            entry.duration_log2_buckets[duration_bucket(duration)] += 1;
        });
    }

    fn record_counter(&self, name: &str, value: u64) {
        self.with_local(|local| {
            let aggregate = local.counters.entry(name.to_string()).or_default();
            *aggregate = aggregate.saturating_add(value);
        });
    }

    pub fn counter_total(&self, name: &str) -> u64 {
        self.flush_local();
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .counters
            .get(name)
            .copied()
            .unwrap_or(0)
    }

    /// Return the bounded per-invocation distribution for one named span.
    ///
    /// The histogram is accumulated with the existing thread-local timing
    /// buffer, so enabling this evidence adds no shared write to a body query.
    pub fn pass_duration_distribution(&self, pass: &str) -> DurationDistribution {
        self.flush_local();
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(aggregate) = inner.passes.get(pass) else {
            return DurationDistribution::default();
        };
        DurationDistribution {
            count: aggregate.invocations,
            total_ns: duration_ns(aggregate.duration),
            max_ns: duration_ns(aggregate.max_duration),
            log2_buckets: aggregate.duration_log2_buckets.to_vec(),
        }
    }

    /// Return the inclusive total for one named span in integer nanoseconds.
    pub fn pass_total_ns(&self, pass: &str) -> u64 {
        self.flush_local();
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .passes
            .get(pass)
            .map_or(0, |aggregate| duration_ns(aggregate.duration))
    }

    /// Record one driver-phase span outside the compiler's timing root.
    fn record_driver_phase(&self, phase: &str, duration: Duration) {
        self.with_local(|local| {
            let entry = local.driver_phases.entry(phase.to_string()).or_default();
            entry.duration += duration;
            entry.invocations += 1;
        });
    }

    /// Record a synthetic span and its non-overlapping root contribution.
    #[cfg(test)]
    fn record_test_span(&self, pass: &str, duration: Duration, is_root: bool, is_leaf: bool) {
        self.record_span(pass, duration, is_root, is_leaf);
        if is_root {
            // Synthetic root time has no phase active, so it is unattributed.
            // Charging it keeps the phase-sum invariant true for tests that
            // fabricate root spans instead of driving the span lifecycle.
            self.with_local(|local| local.synthetic_unattributed += duration);
        }
    }

    /// Record one real direct parent-child relationship for regression tests.
    #[cfg(test)]
    fn record_parent_edge(&self, parent: &str, child: &str) {
        self.with_local(|local| {
            local
                .parent_edges
                .push((parent.to_owned(), child.to_owned()));
        });
    }

    /// Snapshot parent-child relationships captured by the real timing layer.
    #[cfg(test)]
    pub(crate) fn parent_edges(&self) -> Vec<(String, String)> {
        self.flush_local();
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .parent_edges
            .clone()
    }

    /// Enter a published phase.
    ///
    fn enter_phase(&self, phase: Phase) {
        self.record_accounting(
            Instant::now(),
            [Some(AccountingTransition::PhaseEnter(phase)), None],
        );
    }

    /// Exit a published phase.
    fn exit_phase(&self, phase: Phase) {
        self.record_accounting(
            Instant::now(),
            [Some(AccountingTransition::PhaseExit(phase)), None],
        );
    }

    /// Snapshot the additive wall-clock partition of compiler-root time.
    ///
    /// Production readers reach this through the benchmark JSON, which snapshots
    /// the partition and the root total together.
    #[cfg(test)]
    pub(crate) fn phase_accounting(&self) -> PhaseAccounting {
        self.flush_local();
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        phase_accounting_locked(&inner, Instant::now())
    }

    /// Enter a root span and update both timing views with one timestamp.
    fn enter_root_span(&self, timing: &mut SpanTiming, now: Instant) {
        timing.enter_at(now);
        self.record_accounting(
            now,
            [
                Some(AccountingTransition::RootEnter),
                timing.phase.map(AccountingTransition::PhaseEnter),
            ],
        );
    }

    /// Exit a root span and update both timing views with one timestamp.
    fn exit_root_span(&self, timing: &mut SpanTiming, now: Instant) {
        self.record_accounting(
            now,
            [
                timing.phase.map(AccountingTransition::PhaseExit),
                Some(AccountingTransition::RootExit),
            ],
        );
        timing.exit_at(now);
    }

    fn record_accounting(&self, at: Instant, transitions: [Option<AccountingTransition>; 2]) {
        self.with_local(|local| {
            local.accounting_events.extend(
                transitions
                    .into_iter()
                    .flatten()
                    .map(|transition| AccountingEvent { at, transition }),
            );
        });
    }

    /// Begin one synthetic root-span active interval.
    #[cfg(test)]
    fn root_enter_at(&self, now: Instant) {
        self.record_accounting(now, [Some(AccountingTransition::RootEnter), None]);
    }

    /// End one synthetic root-span active interval.
    #[cfg(test)]
    fn root_exit_at(&self, now: Instant) {
        self.record_accounting(now, [Some(AccountingTransition::RootExit), None]);
    }

    /// Generate the timing report.
    ///
    /// Returns a formatted string showing each pass's timing and percentage
    /// of total compilation time.
    pub fn report(&self) -> String {
        self.flush_local();
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);

        if inner.passes.is_empty() {
            return String::from("No timing data collected (no instrumented passes ran).\n");
        }

        let total_ms =
            phase_accounting_locked(&inner, Instant::now()).compiler_root_ns as f64 / 1_000_000.0;
        let pass_order = ordered_aggregates(&inner.passes);

        let mut output = String::new();
        output.push_str("=== Compilation Timing (inclusive spans) ===\n\n");

        // Find the longest pass name for alignment
        let max_name_len = pass_order.iter().map(|s| s.len()).max().unwrap_or(0);

        for pass in &pass_order {
            if let Some(measurement) = inner.passes.get(pass) {
                let ms = measurement.duration.as_secs_f64() * 1000.0;
                let pct = if total_ms > 0.0 {
                    (ms / total_ms) * 100.0
                } else {
                    0.0
                };

                // Format: "  Lexer:              0.2ms (  1%)"
                // Capitalize first letter for display
                let display_name = capitalize(pass);
                output.push_str(&format!(
                    "  {:<width$} {:>8.1}ms ({:>3.0}%)\n",
                    format!("{}:", display_name),
                    ms,
                    pct,
                    width = max_name_len + 1
                ));
            }
        }

        output.push_str(&format!("  {:-<width$}\n", "", width = max_name_len + 20));
        output.push_str(&format!(
            "  {:<width$} {:>8.1}ms (100%)\n",
            "Total (root spans):",
            total_ms,
            width = max_name_len + 1
        ));
        output.push_str("\n  Pass rows are inclusive; nested rows overlap their parents.\n");

        if !inner.driver_phases.is_empty() {
            output.push_str("\n  Driver phases (outside the compiler total):\n");
            for phase in ordered_driver_aggregates(&inner.driver_phases) {
                if let Some(measurement) = inner.driver_phases.get(&phase) {
                    output.push_str(&format!(
                        "  {:<width$} {:>8.1}ms\n",
                        format!("{}:", capitalize(&phase)),
                        measurement.duration.as_secs_f64() * 1000.0,
                        width = max_name_len + 1
                    ));
                }
            }
        }

        output
    }

    /// Generate structured timing data with optional source metrics and memory usage.
    ///
    /// # Arguments
    /// * `target` - The target platform string (e.g., "x86_64-linux")
    /// * `version` - The compiler version string
    /// * `source_metrics` - Optional source and program-shape metrics
    /// * `compiler_work` - Optional deterministic compiler-work counters
    /// * `peak_memory_bytes` - Optional peak memory usage in bytes
    pub fn to_benchmark_timing_with_metrics(
        &self,
        target: &str,
        version: &str,
        source_metrics: Option<SourceMetrics>,
        compiler_work: Option<CompilerWork>,
        peak_memory_bytes: Option<u64>,
    ) -> BenchmarkTiming {
        self.flush_local();
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);

        // Take the partition first so `total_ms` and the bands describe the same
        // instant; `phase_accounting_locked` charges the in-flight interval.
        let phase_accounting = phase_accounting_locked(&inner, Instant::now());
        let total_ms = phase_accounting.compiler_root_ns as f64 / 1_000_000.0;

        let passes = ordered_aggregates(&inner.passes)
            .into_iter()
            .filter_map(|pass| {
                inner.passes.get(&pass).map(|measurement| {
                    // Derived from integer nanoseconds, like every other
                    // millisecond value here, so the root row and `total_ms`
                    // agree bit for bit rather than to within rounding.
                    let duration_ms = duration_ns(measurement.duration) as f64 / 1_000_000.0;
                    let percent = if total_ms > 0.0 {
                        (duration_ms / total_ms) * 100.0
                    } else {
                        0.0
                    };
                    PassTiming {
                        name: pass,
                        duration_ms,
                        percent,
                        invocations: measurement.invocations,
                        root_invocations: measurement.root_invocations,
                        leaf_invocations: measurement.leaf_invocations,
                    }
                })
            })
            .collect();

        let driver_phases = ordered_driver_aggregates(&inner.driver_phases)
            .into_iter()
            .filter_map(|phase| {
                inner
                    .driver_phases
                    .get(&phase)
                    .map(|measurement| DriverPhaseTiming {
                        name: phase,
                        duration_ms: duration_ns(measurement.duration) as f64 / 1_000_000.0,
                        invocations: measurement.invocations,
                    })
            })
            .collect();

        let metadata = BenchmarkMetadata {
            timestamp: iso8601_now(),
            version: version.to_string(),
            target: target.to_string(),
            compiler_build_profile: COMPILER_BUILD_PROFILE,
        };

        BenchmarkTiming {
            schema_version: 16,
            timing_model: "inclusive_spans",
            phase_accounting,
            metadata,
            passes,
            driver_phases,
            total_ms,
            source_metrics,
            compiler_work,
            peak_memory_bytes,
        }
    }

    /// Generate JSON output with additional source metrics.
    ///
    /// # Arguments
    /// * `target` - The target platform string
    /// * `version` - The compiler version string
    /// * `source_metrics` - Source and program-shape metrics
    /// * `compiler_work` - Deterministic compiler-work counters
    /// * `peak_memory_bytes` - Optional peak memory usage
    pub fn to_json_with_metrics(
        &self,
        target: &str,
        version: &str,
        source_metrics: Option<SourceMetrics>,
        compiler_work: Option<CompilerWork>,
        peak_memory_bytes: Option<u64>,
    ) -> String {
        let timing = self.to_benchmark_timing_with_metrics(
            target,
            version,
            source_metrics,
            compiler_work,
            peak_memory_bytes,
        );
        serde_json::to_string(&timing).unwrap_or_else(|_| "{}".to_string())
    }
}

impl Default for TimingData {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct AccountingTimeline {
    active_roots: u64,
    phase_counts: [u64; Phase::ALL.len()],
    phase_durations: [Duration; Phase::ALL.len()],
    mixed_parallel: Duration,
    unattributed: Duration,
    cursor: Option<Instant>,
}

/// Charge the interval since the last timestamp to the bucket the preceding
/// merged state implied, then advance the cursor.
///
/// Every interval of compiler-root wall time is partitioned into exactly one
/// bucket:
///
/// * exactly one phase active -> that phase;
/// * more than one distinct phase active -> `mixed_parallel`;
/// * root active with no phase active -> `unattributed`;
/// * root inactive -> excluded from compiler-root totals entirely.
///
/// Because this partitions a single timeline rather than summing independent
/// measurements, the phase-sum invariant holds exactly.
fn charge_interval(timeline: &mut AccountingTimeline, now: Instant) {
    let Some(previous) = timeline.cursor.replace(now) else {
        // First transition: there is no earlier interval to attribute.
        return;
    };
    if timeline.active_roots == 0 {
        return;
    }
    let elapsed = now.saturating_duration_since(previous);
    let mut active_phase = None;
    let mut distinct = 0usize;
    for (index, count) in timeline.phase_counts.iter().enumerate() {
        if *count > 0 {
            distinct += 1;
            active_phase = Some(index);
        }
    }
    match (distinct, active_phase) {
        (0, _) => timeline.unattributed += elapsed,
        (1, Some(index)) => timeline.phase_durations[index] += elapsed,
        _ => timeline.mixed_parallel += elapsed,
    }
}

/// Independently reconstruct the union of active compiler-root intervals.
///
/// This deliberately does not call the phase-band reducer. The published root
/// total and the sum of the bands therefore cross-check two separate passes
/// over the observation stream instead of sharing one duration accumulator.
fn compiler_root_union(events: &[AccountingEvent], now: Instant) -> Duration {
    let mut active_roots = 0u64;
    let mut cursor = None;
    let mut duration = Duration::ZERO;
    let mut index = 0;
    while index < events.len() {
        let at = events[index].at;
        if let Some(previous) = cursor.replace(at)
            && active_roots > 0
        {
            duration += at.saturating_duration_since(previous);
        }
        let mut enters = 0u64;
        let mut exits = 0u64;
        while index < events.len() && events[index].at == at {
            match events[index].transition {
                AccountingTransition::RootEnter => enters += 1,
                AccountingTransition::RootExit => exits += 1,
                AccountingTransition::PhaseEnter(_) | AccountingTransition::PhaseExit(_) => {}
            }
            index += 1;
        }
        active_roots = active_roots.saturating_add(enters).saturating_sub(exits);
    }
    if active_roots > 0
        && let Some(previous) = cursor
    {
        duration += now.saturating_duration_since(previous);
    }
    duration
}

/// Partition root-gated intervals into published phase bands.
///
/// Root enter/exit observations gate which intervals belong to the compiler,
/// but this reducer does not compute or return the compiler-root total.
fn phase_bands(events: &[AccountingEvent], now: Instant) -> AccountingTimeline {
    let mut timeline = AccountingTimeline::default();
    let mut index = 0;
    while index < events.len() {
        let at = events[index].at;
        charge_interval(&mut timeline, at);
        let mut root_enters = 0u64;
        let mut root_exits = 0u64;
        let mut phase_enters = [0u64; Phase::ALL.len()];
        let mut phase_exits = [0u64; Phase::ALL.len()];
        while index < events.len() && events[index].at == at {
            match events[index].transition {
                AccountingTransition::RootEnter => root_enters += 1,
                AccountingTransition::RootExit => root_exits += 1,
                AccountingTransition::PhaseEnter(phase) => phase_enters[phase.index()] += 1,
                AccountingTransition::PhaseExit(phase) => phase_exits[phase.index()] += 1,
            }
            index += 1;
        }
        timeline.active_roots = timeline
            .active_roots
            .saturating_add(root_enters)
            .saturating_sub(root_exits);
        for phase in Phase::ALL {
            timeline.phase_counts[phase.index()] = timeline.phase_counts[phase.index()]
                .saturating_add(phase_enters[phase.index()])
                .saturating_sub(phase_exits[phase.index()]);
        }
    }
    let final_at = events.last().map_or(now, |event| event.at.max(now));
    charge_interval(&mut timeline, final_at);
    timeline
}

/// Read the phase partition and the root total as of one instant.
///
/// Charging before reading is what lets an in-flight compilation report bands
/// that still sum to its root total. Reading without charging would report a
/// root total including the current interval while the bands excluded it.
fn phase_accounting_locked(inner: &TimingDataInner, now: Instant) -> PhaseAccounting {
    let mut events = inner.accounting_events.clone();
    events.sort_by_key(|event| event.at);
    let compiler_root_ns = duration_ns(compiler_root_union(&events, now));
    let timeline = phase_bands(&events, now);
    #[cfg(test)]
    let mut timeline = timeline;
    #[cfg(test)]
    {
        timeline.unattributed += inner.synthetic_unattributed;
    }
    let phase_ns = Phase::ALL
        .into_iter()
        .map(|phase| (phase, duration_ns(timeline.phase_durations[phase.index()])))
        .collect();
    let mixed_parallel_ns = duration_ns(timeline.mixed_parallel);
    let unattributed_ns = duration_ns(timeline.unattributed);
    #[cfg(test)]
    let compiler_root_ns =
        compiler_root_ns.saturating_add(duration_ns(inner.synthetic_unattributed));
    PhaseAccounting {
        phase_ns,
        mixed_parallel_ns,
        unattributed_ns,
        compiler_root_ns,
    }
}

/// A duration as integer nanoseconds.
///
/// Raw records are integers so that content addressing never depends on
/// floating-point formatting. `u64` nanoseconds spans roughly 584 years, so the
/// saturation arm is unreachable for a compilation.
fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn duration_bucket(duration: Duration) -> usize {
    let nanoseconds = duration_ns(duration);
    if nanoseconds == 0 {
        0
    } else {
        (u64::BITS - 1 - nanoseconds.leading_zeros()) as usize
    }
}

fn ordered_aggregates(aggregates: &HashMap<String, PassAggregate>) -> Vec<String> {
    let mut names = aggregates.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
}

fn ordered_driver_aggregates(aggregates: &HashMap<String, DriverPhaseAggregate>) -> Vec<String> {
    let mut names = aggregates.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
}

/// Capitalize the first letter of a string.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Generate an ISO 8601 timestamp for the current time.
///
/// Format: "2025-12-27T21:30:00Z"
fn iso8601_now() -> String {
    let now = SystemTime::now();
    let duration = now.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = duration.as_secs();

    // Convert to date/time components (simplified, assumes UTC)
    // This is a basic implementation without external dependencies
    const SECS_PER_MIN: u64 = 60;
    const SECS_PER_HOUR: u64 = 3600;
    const SECS_PER_DAY: u64 = 86400;

    let days = secs / SECS_PER_DAY;
    let remaining = secs % SECS_PER_DAY;
    let hours = remaining / SECS_PER_HOUR;
    let remaining = remaining % SECS_PER_HOUR;
    let minutes = remaining / SECS_PER_MIN;
    let seconds = remaining % SECS_PER_MIN;

    // Calculate year, month, day from days since epoch (1970-01-01)
    let (year, month, day) = days_to_ymd(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Simplified algorithm for date calculation
    let mut remaining_days = days as i64;
    let mut year: i64 = 1970;

    // Find the year
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    // Find the month and day
    let leap = is_leap_year(year);
    let days_in_months: [i64; 12] = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1;
    for &days_in_month in &days_in_months {
        if remaining_days < days_in_month {
            break;
        }
        remaining_days -= days_in_month;
        month += 1;
    }

    let day = remaining_days + 1; // Days are 1-indexed

    (year as u64, month, day as u64)
}

/// Check if a year is a leap year.
fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// A tracing layer that collects timing information from spans.
///
/// This layer hooks into the span lifecycle to measure how long each span is
/// active (entered). Observations stay thread-local on the hot path and are
/// merged into `TimingData` only at bounded worker completion or report
/// finalization, with thread teardown as a fallback.
pub struct TimingLayer {
    data: TimingData,
}

impl TimingLayer {
    /// Create a new timing layer that stores data in the given `TimingData`.
    pub fn new(data: TimingData) -> Self {
        Self { data }
    }
}

/// Per-span storage for timing state.
///
/// This is stored in the span's extensions and tracks when the span was entered.
struct SpanTiming {
    /// Number of active entries. A span may be re-entered or entered from
    /// multiple threads, so one optional timestamp is insufficient.
    active_enters: u64,
    /// Start of the current interval in which this span has at least one entry.
    active_since: Option<Instant>,
    /// Union of intervals in which this span was active.
    accumulated: Duration,
    /// Whether this span has at least one direct child.
    has_children: bool,
    /// Whether the span was created without a parent.
    ///
    /// Store this at creation because a child may outlive its parent; looking
    /// the parent up again during a later enter/close can then be ambiguous.
    is_root: bool,
    /// Whether this span measures driver work outside the compiler root.
    ///
    /// Set by a `driver_phase = true` span field, and inherited by children so
    /// nested driver work cannot be mistaken for a compiler pass.
    is_driver_phase: bool,
    /// The published phase this span declares, if any.
    ///
    /// Deliberately NOT inherited from the parent, unlike `is_driver_phase`. A
    /// phase marker on a nested span is a phase *transition*; inheriting it
    /// would make every child re-enter its ancestor's phase and inflate the
    /// reference count without a matching boundary.
    phase: Option<Phase>,
}

/// Reads the `driver_phase` and `phase` markers off a span's creation-time
/// fields.
///
/// Phase membership is declared at the instrumentation site rather than inferred
/// from span names. The compiler's spans nest deeply — `parser_grammar_execution`
/// under `parser` under `parse_file`, the MIR subphases under the backend — so a
/// name-based mapping would read nested spans as concurrent phases and charge
/// them to `mixed_parallel`. Declaring membership keeps the published phases
/// genuinely top-level.
#[derive(Default)]
struct SpanMarkerVisitor {
    is_driver_phase: bool,
    phase: Option<Phase>,
}

#[derive(Default)]
struct TimingFlushVisitor(bool);

#[derive(Default)]
struct TimingDurationVisitor(Option<u64>);

#[derive(Default)]
struct CfgConstructionBreakdownVisitor {
    input_preparation_ns: Option<u64>,
    semantic_materialization_ns: Option<u64>,
    domain_prerequisites_ns: Option<u64>,
    domain_projection_ns: Option<u64>,
    prerequisite_collection_ns: Option<u64>,
    prerequisite_queries_ns: Option<u64>,
    cfg_builder_ns: Option<u64>,
    cfg_publication_ns: Option<u64>,
}

#[derive(Default)]
struct SemanticProviderBreakdownVisitor {
    host_setup_ns: Option<u64>,
    expression_engine_ns: Option<u64>,
    specialization_selection_ns: Option<u64>,
    body_export_ns: Option<u64>,
    result_projection_ns: Option<u64>,
    setup_ns: Option<u64>,
    inference_precompute_ns: Option<u64>,
    inference_precompute_structural_ns: Option<u64>,
    inference_precompute_eval_provider_ns: Option<u64>,
    constraint_generation_ns: Option<u64>,
    unification_resolution_ns: Option<u64>,
    air_emission_validation_ns: Option<u64>,
    counters: HashMap<&'static str, u64>,
}

#[derive(Default)]
struct SemanticBodyLoweringBreakdownVisitor {
    attributed_total_ns: Option<u64>,
    assembly_snapshot_ns: Option<u64>,
    lex_parse_ns: Option<u64>,
    rir_lower_ns: Option<u64>,
    span_remap_validation_ns: Option<u64>,
    body_rir_index_ns: Option<u64>,
    counters: HashMap<&'static str, u64>,
}

impl tracing::field::Visit for TimingFlushVisitor {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "timing_flush" {
            self.0 = value;
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}

impl tracing::field::Visit for TimingDurationVisitor {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "duration_ns" {
            self.0 = Some(value);
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}

impl tracing::field::Visit for CfgConstructionBreakdownVisitor {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "input_preparation_ns" => self.input_preparation_ns = Some(value),
            "semantic_materialization_ns" => self.semantic_materialization_ns = Some(value),
            "domain_prerequisites_ns" => self.domain_prerequisites_ns = Some(value),
            "domain_projection_ns" => self.domain_projection_ns = Some(value),
            "prerequisite_collection_ns" => self.prerequisite_collection_ns = Some(value),
            "prerequisite_queries_ns" => self.prerequisite_queries_ns = Some(value),
            "cfg_builder_ns" => self.cfg_builder_ns = Some(value),
            "cfg_publication_ns" => self.cfg_publication_ns = Some(value),
            _ => {}
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}

impl tracing::field::Visit for SemanticProviderBreakdownVisitor {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "host_setup_ns" => self.host_setup_ns = Some(value),
            "expression_engine_ns" => self.expression_engine_ns = Some(value),
            "specialization_selection_ns" => self.specialization_selection_ns = Some(value),
            "body_export_ns" => self.body_export_ns = Some(value),
            "result_projection_ns" => self.result_projection_ns = Some(value),
            "setup_ns" => self.setup_ns = Some(value),
            "inference_precompute_ns" => self.inference_precompute_ns = Some(value),
            "inference_precompute_structural_ns" => {
                self.inference_precompute_structural_ns = Some(value)
            }
            "inference_precompute_eval_provider_ns" => {
                self.inference_precompute_eval_provider_ns = Some(value)
            }
            "constraint_generation_ns" => self.constraint_generation_ns = Some(value),
            "unification_resolution_ns" => self.unification_resolution_ns = Some(value),
            "air_emission_validation_ns" => self.air_emission_validation_ns = Some(value),
            name if name.starts_with("precompute_") => {
                self.counters.insert(field.name(), value);
            }
            _ => {}
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}

impl tracing::field::Visit for SemanticBodyLoweringBreakdownVisitor {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "attributed_total_ns" => self.attributed_total_ns = Some(value),
            "assembly_snapshot_ns" => self.assembly_snapshot_ns = Some(value),
            "lex_parse_ns" => self.lex_parse_ns = Some(value),
            "rir_lower_ns" => self.rir_lower_ns = Some(value),
            "span_remap_validation_ns" => self.span_remap_validation_ns = Some(value),
            "body_rir_index_ns" => self.body_rir_index_ns = Some(value),
            name => {
                self.counters.insert(name, value);
            }
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}

impl tracing::field::Visit for SpanMarkerVisitor {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "driver_phase" {
            self.is_driver_phase = value;
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "phase" {
            // An unrecognized name yields `None`, so the span simply marks no
            // phase and its time falls to `unattributed`. That is visible in the
            // published bands rather than silently misattributed.
            self.phase = Phase::from_wire_name(value);
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}

impl SpanTiming {
    fn enter_at(&mut self, now: Instant) {
        if self.active_enters == 0 {
            self.active_since = Some(now);
        }
        self.active_enters += 1;
    }

    fn exit_at(&mut self, now: Instant) {
        if self.active_enters == 0 {
            return;
        }
        self.active_enters -= 1;
        if self.active_enters == 0 {
            if let Some(started) = self.active_since.take() {
                self.accumulated += now.saturating_duration_since(started);
            }
        }
    }

    fn duration(&self) -> Duration {
        self.accumulated
            + self
                .active_since
                .map_or(Duration::ZERO, |started| started.elapsed())
    }
}

impl<S> Layer<S> for TimingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().fields().field("timing_flush").is_some() {
            let mut visitor = TimingFlushVisitor::default();
            event.record(&mut visitor);
            if visitor.0 {
                self.data.flush_local();
                #[cfg(test)]
                self.data.record_flush_marker();
            }
            return;
        }
        if event.metadata().target() == "rue::timing"
            && event.metadata().name() == "cfg_construction_breakdown"
        {
            let mut visitor = CfgConstructionBreakdownVisitor::default();
            event.record(&mut visitor);
            let (
                Some(input_preparation_ns),
                Some(semantic_materialization_ns),
                Some(domain_prerequisites_ns),
                Some(domain_projection_ns),
                Some(prerequisite_collection_ns),
                Some(prerequisite_queries_ns),
                Some(cfg_builder_ns),
                Some(cfg_publication_ns),
            ) = (
                visitor.input_preparation_ns,
                visitor.semantic_materialization_ns,
                visitor.domain_prerequisites_ns,
                visitor.domain_projection_ns,
                visitor.prerequisite_collection_ns,
                visitor.prerequisite_queries_ns,
                visitor.cfg_builder_ns,
                visitor.cfg_publication_ns,
            )
            else {
                return;
            };
            for (name, duration_ns) in [
                ("cfg_input_preparation", input_preparation_ns),
                ("semantic_materialization", semantic_materialization_ns),
                ("cfg_domain_prerequisites", domain_prerequisites_ns),
                ("cfg_domain_projection", domain_projection_ns),
                ("cfg_prerequisite_collection", prerequisite_collection_ns),
                ("cfg_prerequisite_queries", prerequisite_queries_ns),
                ("cfg_builder", cfg_builder_ns),
                ("cfg_publication", cfg_publication_ns),
            ] {
                self.data
                    .record_span(name, Duration::from_nanos(duration_ns), false, true);
            }
            return;
        }
        if event.metadata().target() == "rue::timing"
            && event.metadata().name() == "semantic_body_lowering_breakdown"
        {
            let mut visitor = SemanticBodyLoweringBreakdownVisitor::default();
            event.record(&mut visitor);
            let (
                Some(attributed_total_ns),
                Some(assembly_snapshot_ns),
                Some(lex_parse_ns),
                Some(rir_lower_ns),
                Some(span_remap_validation_ns),
                Some(body_rir_index_ns),
            ) = (
                visitor.attributed_total_ns,
                visitor.assembly_snapshot_ns,
                visitor.lex_parse_ns,
                visitor.rir_lower_ns,
                visitor.span_remap_validation_ns,
                visitor.body_rir_index_ns,
            )
            else {
                return;
            };
            if assembly_snapshot_ns
                .saturating_add(lex_parse_ns)
                .saturating_add(rir_lower_ns)
                .saturating_add(span_remap_validation_ns)
                .saturating_add(body_rir_index_ns)
                != attributed_total_ns
            {
                return;
            }
            for (name, duration_ns) in [
                ("semantic_body_input_attributed_total", attributed_total_ns),
                (
                    "semantic_body_input_assembly_snapshot",
                    assembly_snapshot_ns,
                ),
                ("semantic_body_input_lex_parse", lex_parse_ns),
                ("semantic_body_input_rir_lower", rir_lower_ns),
                (
                    "semantic_body_input_span_remap_validation",
                    span_remap_validation_ns,
                ),
                ("semantic_body_input_rir_index", body_rir_index_ns),
            ] {
                self.data
                    .record_span(name, Duration::from_nanos(duration_ns), false, true);
            }
            self.data.record_counter("body_lowerings", 1);
            for (name, value) in visitor.counters {
                self.data.record_counter(name, value);
            }
            return;
        }
        if event.metadata().target() == "rue::timing"
            && event.metadata().name() == "semantic_provider_breakdown"
        {
            let mut visitor = SemanticProviderBreakdownVisitor::default();
            event.record(&mut visitor);
            let (
                Some(host_setup_ns),
                Some(expression_engine_ns),
                Some(specialization_selection_ns),
                Some(body_export_ns),
                Some(result_projection_ns),
                Some(setup_ns),
                Some(inference_precompute_ns),
                Some(inference_precompute_structural_ns),
                Some(inference_precompute_eval_provider_ns),
                Some(constraint_generation_ns),
                Some(unification_resolution_ns),
                Some(air_emission_validation_ns),
            ) = (
                visitor.host_setup_ns,
                visitor.expression_engine_ns,
                visitor.specialization_selection_ns,
                visitor.body_export_ns,
                visitor.result_projection_ns,
                visitor.setup_ns,
                visitor.inference_precompute_ns,
                visitor.inference_precompute_structural_ns,
                visitor.inference_precompute_eval_provider_ns,
                visitor.constraint_generation_ns,
                visitor.unification_resolution_ns,
                visitor.air_emission_validation_ns,
            )
            else {
                return;
            };
            if inference_precompute_structural_ns
                .saturating_add(inference_precompute_eval_provider_ns)
                != inference_precompute_ns
            {
                return;
            }
            for (name, duration_ns) in [
                ("semantic_provider_host_setup", host_setup_ns),
                ("semantic_provider_expression_engine", expression_engine_ns),
                (
                    "semantic_provider_specialization_selection",
                    specialization_selection_ns,
                ),
                ("semantic_provider_body_export", body_export_ns),
                ("semantic_provider_result_projection", result_projection_ns),
                ("semantic_expression_setup", setup_ns),
                ("semantic_inference_precompute", inference_precompute_ns),
                (
                    "semantic_inference_precompute_structural",
                    inference_precompute_structural_ns,
                ),
                (
                    "semantic_inference_precompute_eval_provider",
                    inference_precompute_eval_provider_ns,
                ),
                ("semantic_constraint_generation", constraint_generation_ns),
                ("semantic_unification_resolution", unification_resolution_ns),
                (
                    "semantic_air_emission_validation",
                    air_emission_validation_ns,
                ),
            ] {
                self.data
                    .record_span(name, Duration::from_nanos(duration_ns), false, true);
            }
            self.data.record_counter("precompute_bodies", 1);
            for (name, value) in visitor.counters {
                self.data.record_counter(name, value);
            }
            return;
        }
        if event.metadata().target() == "rue::timing"
            && event.metadata().fields().field("duration_ns").is_some()
        {
            let mut visitor = TimingDurationVisitor::default();
            event.record(&mut visitor);
            if let Some(duration_ns) = visitor.0 {
                self.data.record_span(
                    event.metadata().name(),
                    Duration::from_nanos(duration_ns),
                    false,
                    true,
                );
            }
        }
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // Initialize timing state for this span
        if let Some(span) = ctx.span(id) {
            let parent_id = span.parent().map(|parent| parent.id().clone());
            #[cfg(test)]
            let parent_edge = span
                .parent()
                .map(|parent| (parent.name().to_owned(), span.name().to_owned()));
            let is_root = parent_id.is_none();
            let mut visitor = SpanMarkerVisitor::default();
            attrs.record(&mut visitor);
            let parent_is_driver_phase = parent_id
                .as_ref()
                .and_then(|parent_id| ctx.span(parent_id))
                .is_some_and(|parent| {
                    parent
                        .extensions()
                        .get::<SpanTiming>()
                        .is_some_and(|timing| timing.is_driver_phase)
                });
            let mut extensions = span.extensions_mut();
            extensions.insert(SpanTiming {
                active_enters: 0,
                active_since: None,
                accumulated: Duration::ZERO,
                has_children: false,
                is_root,
                is_driver_phase: visitor.is_driver_phase || parent_is_driver_phase,
                phase: visitor.phase,
            });
            drop(extensions);

            if let Some(parent_id) = parent_id {
                if let Some(parent) = ctx.span(&parent_id) {
                    let mut parent_extensions = parent.extensions_mut();
                    if let Some(parent_timing) = parent_extensions.get_mut::<SpanTiming>() {
                        parent_timing.has_children = true;
                    }
                }
            }

            #[cfg(test)]
            if let Some((parent, child)) = parent_edge {
                self.data.record_parent_edge(&parent, &child);
            }
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(timing) = extensions.get_mut::<SpanTiming>() {
                let now = Instant::now();
                if timing.is_root && !timing.is_driver_phase {
                    self.data.enter_root_span(timing, now);
                } else {
                    // Sample after acquiring the per-span extension lock so
                    // concurrent callbacks cannot apply stale timestamps.
                    timing.enter_at(now);
                    // A driver phase is outside the compiler root by
                    // construction, so it can never also be a published phase.
                    if let Some(phase) = timing.phase.filter(|_| !timing.is_driver_phase) {
                        self.data.enter_phase(phase);
                    }
                }
            }
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(timing) = extensions.get_mut::<SpanTiming>() {
                let now = Instant::now();
                if timing.is_root && !timing.is_driver_phase {
                    self.data.exit_root_span(timing, now);
                } else {
                    // Leave the phase before closing the span's inclusive view.
                    if let Some(phase) = timing.phase.filter(|_| !timing.is_driver_phase) {
                        self.data.exit_phase(phase);
                    }
                    // Sample after acquiring the per-span extension lock so
                    // concurrent callbacks cannot apply stale timestamps.
                    timing.exit_at(now);
                }
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        // When the span is fully closed, record its total time
        if let Some(span) = ctx.span(&id) {
            let measurement = {
                let extensions = span.extensions();
                extensions.get::<SpanTiming>().map(|timing| {
                    (
                        span.name().to_string(),
                        timing.duration(),
                        timing.is_root,
                        !timing.has_children,
                        timing.is_driver_phase,
                    )
                })
            };

            // Copy the measurement while the span exists, then append it to
            // this worker's local aggregate.
            if let Some((name, duration, is_root, is_leaf, is_driver_phase)) = measurement {
                if is_driver_phase {
                    self.data.record_driver_phase(&name, duration);
                } else {
                    self.data.record_span(&name, duration, is_root, is_leaf);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rue_compiler::unstable::update_for_presentation;
    use rue_compiler::{CompileOptions, CompilerSession, SourceSnapshot};
    use rue_query::{CancellationToken, QueryAbort, QueryKey, QueryOutput, QueryRuntime, Revision};
    use std::sync::Barrier;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct TimingBatchKey(u64);

    impl QueryKey for TimingBatchKey {
        fn stable_identity(&self) -> String {
            self.0.to_string()
        }

        fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
            std::hash::Hash::hash(&self.0, hasher);
        }
    }

    fn publish_test_revision(runtime: &QueryRuntime) {
        runtime.publish_revision(Revision::new(1, 1), []).unwrap();
    }

    #[test]
    fn registered_batch_lifecycle_publishes_real_worker_local_timing() {
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        let caller = std::thread::current().id();

        tracing::subscriber::with_default(subscriber, || {
            let _compile = tracing::info_span!("compile").entered();
            // This span closes before the batch. It can reach shared state on
            // this long-lived caller only through the inline worker's real
            // completion marker; this test deliberately does not call a report
            // method or `flush_local` before inspecting it.
            {
                let _before = tracing::info_span!("inline_before_registered_batch").entered();
            }

            let runtime = QueryRuntime::new(4);
            publish_test_revision(&runtime);
            let rendezvous = Arc::new(Barrier::new(4));
            let rendezvous_for_child = rendezvous.clone();
            let child = runtime
                .family_with_evaluator::<TimingBatchKey, u64, _>(
                    "timing-batch-child",
                    8,
                    move |_, _, key| {
                        let _work = tracing::info_span!("registered_batch_child_timing").entered();
                        rendezvous_for_child.wait();
                        Ok(QueryOutput::success(key.0))
                    },
                )
                .unwrap();
            let child_for_root = child.clone();
            let root = runtime
                .family_with_evaluator::<TimingBatchKey, (), _>(
                    "timing-batch-root",
                    8,
                    move |context, _, _| {
                        context
                            .query_registered_batch(&child_for_root, (0..4).map(TimingBatchKey))?;
                        Ok(QueryOutput::success(()))
                    },
                )
                .unwrap();
            let attempt = runtime.request_registered(
                &root,
                Revision::new(1, 1),
                TimingBatchKey(99),
                CancellationToken::new(),
            );
            assert!(attempt.terminal().is_some());

            let success_flushes = data.flush_threads();
            assert_eq!(success_flushes.len(), 4, "{success_flushes:?}");
            assert!(success_flushes.contains(&caller));
            assert!(success_flushes.iter().any(|thread| *thread != caller));
            assert!(
                data.published_pass("inline_before_registered_batch")
                    .is_some(),
                "the inline completion marker must publish before finalization"
            );
            assert_eq!(
                data.published_pass("registered_batch_child_timing")
                    .unwrap()
                    .invocations,
                4
            );

            // Cooperative cancellation still returns through every worker's
            // bounded completion boundary.
            let cancellation = CancellationToken::new();
            let cancellation_for_child = cancellation.clone();
            let canceled_child = runtime
                .family_with_evaluator::<TimingBatchKey, u64, _>(
                    "timing-batch-canceled-child",
                    8,
                    move |context, _, key| {
                        if key.0 == 0 {
                            cancellation_for_child.cancel();
                        }
                        context.check_canceled()?;
                        Ok(QueryOutput::success(key.0))
                    },
                )
                .unwrap();
            let canceled_child_for_root = canceled_child.clone();
            let canceled_root = runtime
                .family_with_evaluator::<TimingBatchKey, (), _>(
                    "timing-batch-canceled-root",
                    8,
                    move |context, _, _| {
                        context.query_registered_batch(
                            &canceled_child_for_root,
                            (0..4).map(TimingBatchKey),
                        )?;
                        Ok(QueryOutput::success(()))
                    },
                )
                .unwrap();
            let canceled = runtime.request_registered(
                &canceled_root,
                Revision::new(1, 1),
                TimingBatchKey(100),
                cancellation,
            );
            assert_eq!(canceled.abort(), Some(&QueryAbort::Canceled));
            assert_eq!(data.flush_threads().len(), success_flushes.len() + 4);
        });
    }

    #[test]
    fn cli_orchestration_modules_do_not_restore_the_retired_peer_orchestrator() {
        let retired_type = ["Compilation", "Unit"].concat();
        for (name, source) in [
            ("main.rs", include_str!("main.rs")),
            ("compile.rs", include_str!("compile.rs")),
            ("emit.rs", include_str!("emit.rs")),
            ("source_loader.rs", include_str!("source_loader.rs")),
        ] {
            assert!(
                !source.contains(&retired_type),
                "{name} must query CompilerSession directly"
            );
        }
    }

    #[test]
    fn real_compiler_phase_spans_preserve_leaf_boundaries() {
        let snapshot = SourceSnapshot::single("main.rue", "fn main() -> i32 { 42 }").unwrap();

        let direct_data = TimingData::new();
        let direct_subscriber =
            tracing_subscriber::registry().with(TimingLayer::new(direct_data.clone()));
        tracing::subscriber::with_default(direct_subscriber, || {
            let mut session = CompilerSession::new();
            update_for_presentation(&mut session, &snapshot)
                .into_result()
                .unwrap();
        });

        let direct_edges = direct_data.parent_edges();
        assert!(
            direct_edges.contains(&("parse_file".to_owned(), "lexer".to_owned())),
            "direct parse edges: {direct_edges:?}"
        );
        assert!(
            direct_edges.contains(&("parse_file".to_owned(), "parser".to_owned())),
            "direct parse edges: {direct_edges:?}"
        );
        for expected in [
            ("parser", "parser_nesting_scan"),
            ("parser", "parser_state_setup"),
            ("parser", "parser_grammar_execution"),
            ("parser", "parser_directive_validation"),
        ] {
            assert!(
                direct_edges.contains(&(expected.0.to_owned(), expected.1.to_owned())),
                "missing {expected:?} in direct parse edges: {direct_edges:?}"
            );
        }

        let direct_timing =
            direct_data.to_benchmark_timing_with_metrics("test", "test", None, None, None);
        let direct_parse = direct_timing
            .passes
            .iter()
            .find(|pass| pass.name == "parse_file")
            .unwrap();
        assert_eq!(direct_parse.invocations, 1);
        // The parse query's own phases wrap it (RUE-786), so `parse_query_key`
        // and its siblings own the root here and `parse_file` nests beneath
        // `parse_program`.
        assert_eq!(direct_parse.root_invocations, 0);
        assert!(
            direct_edges.contains(&("parse_program".to_owned(), "parse_file".to_owned())),
            "direct parse edges: {direct_edges:?}"
        );

        let session_data = TimingData::new();
        let session_subscriber =
            tracing_subscriber::registry().with(TimingLayer::new(session_data.clone()));
        tracing::subscriber::with_default(session_subscriber, || {
            let mut session = CompilerSession::new();
            session.update(&snapshot).into_result().unwrap();
            {
                let _span = tracing::info_span!("semantic_astgen").entered();
                session.rir().unwrap();
            }
            rue_compiler::unstable::rooted_cfg(&mut session, &CompileOptions::default()).unwrap();
        });

        let session_edges = session_data.parent_edges();
        for expected in [
            ("parse_file", "lexer"),
            ("parse_file", "parser"),
            // The definition snapshot is built by the canonical merge, which
            // RUE-786 gave its own span beneath the astgen aggregate.
            ("semantic_astgen", "canonical_merge"),
            ("canonical_merge", "definition_snapshot_modules"),
        ] {
            assert!(
                session_edges.contains(&(expected.0.to_owned(), expected.1.to_owned())),
                "missing {expected:?} in compiler-session edges: {session_edges:?}"
            );
        }

        let session_timing =
            session_data.to_benchmark_timing_with_metrics("test", "test", None, None, None);
        let session_parse_file = session_timing
            .passes
            .iter()
            .find(|pass| pass.name == "parse_file")
            .unwrap();
        let occurrence_index = session_timing
            .passes
            .iter()
            .find(|pass| pass.name == "declaration_occurrence_index")
            .unwrap();
        let declaration_graph = session_timing
            .passes
            .iter()
            .find(|pass| pass.name == "declaration_graph_collection")
            .unwrap();
        let body_closure = session_timing
            .passes
            .iter()
            .find(|pass| pass.name == "body_closure_collection")
            .unwrap();
        let body_graph_projection = session_timing
            .passes
            .iter()
            .find(|pass| pass.name == "body_graph_projection")
            .unwrap();
        let optimized_cfg = session_timing
            .passes
            .iter()
            .find(|pass| pass.name == "optimized_cfg_collection")
            .unwrap();
        // The session parses the source module once. Declaration signatures
        // project their exact canonical AST nodes, and named body analysis
        // consumes the canonical candidate plan; neither owns a parser
        // invocation.
        assert_eq!(session_parse_file.invocations, 1);
        assert_eq!(session_parse_file.root_invocations, 0);
        assert!(session_edges.contains(&("parse_program".to_owned(), "parse_file".to_owned())));
        assert!(!session_edges.iter().any(|(parent, child)| {
            parent == "declaration_signature_parsing" || child == "declaration_signature_parsing"
        }));
        // And the path that replaced it is present. Asserting only the absence
        // above lets a producer that projects nothing at all pass (RUE-1515).
        assert!(session_edges.contains(&(
            "declaration_nucleus".to_owned(),
            "declaration_signature_projection".to_owned()
        )));
        assert!(
            !session_edges.contains(&("body_input_lowering".to_owned(), "parse_file".to_owned()))
        );
        // Semantic presentation is a projection of the same rooted query
        // graph used by normal compilation. The old whole-program declaration
        // index and `sema` coordinator must therefore remain absent.
        assert_eq!(declaration_graph.invocations, 1);
        assert_eq!(declaration_graph.root_invocations, 1);
        assert_eq!(occurrence_index.invocations, 1);
        assert_eq!(occurrence_index.root_invocations, 0);
        assert_eq!(occurrence_index.leaf_invocations, 1);
        assert_eq!(body_closure.invocations, 1);
        assert_eq!(body_closure.root_invocations, 1);
        assert_eq!(body_graph_projection.invocations, 1);
        assert_eq!(body_graph_projection.root_invocations, 1);
        assert_eq!(optimized_cfg.invocations, 1);
        assert_eq!(optimized_cfg.root_invocations, 1);
        assert!(session_edges.contains(&(
            "declaration_graph_collection".to_owned(),
            "declaration_occurrence_index".to_owned()
        )));
        assert!(session_edges.contains(&(
            "body_closure_collection".to_owned(),
            "body_analysis".to_owned()
        )));
        assert!(
            session_edges.contains(&("body_analysis".to_owned(), "body_input_lowering".to_owned()))
        );
        assert!(session_edges.contains(&(
            "body_analysis".to_owned(),
            "semantic_provider_analysis".to_owned()
        )));
        assert!(session_edges.contains(&(
            "optimized_cfg_collection".to_owned(),
            "cfg_construction".to_owned()
        )));
        assert!(
            !session_timing
                .passes
                .iter()
                .any(|pass| pass.name == "rir_declaration_index" || pass.name == "sema")
        );

        let compile_data = TimingData::new();
        let compile_subscriber =
            tracing_subscriber::registry().with(TimingLayer::new(compile_data.clone()));
        tracing::subscriber::with_default(compile_subscriber, || {
            rue_compiler::compile_snapshot(&snapshot, &CompileOptions::default()).unwrap();
        });

        let compile_edges = compile_data.parent_edges();
        assert!(
            compile_edges.contains(&("compile".to_owned(), "compile_pipeline".to_owned())),
            "missing compile -> compile_pipeline in batch edges: {compile_edges:?}"
        );
        for edge in [
            ("compile_pipeline", "declaration_graph_collection"),
            (
                "declaration_graph_collection",
                "declaration_occurrence_index",
            ),
            ("declaration_graph_collection", "declaration_nucleus"),
            ("compile_pipeline", "body_closure_collection"),
            ("compile_pipeline", "body_graph_projection"),
            ("compile_pipeline", "optimized_cfg_collection"),
            ("optimized_cfg_collection", "cfg_construction"),
            ("optimized_cfg_collection", "cfg_optimization"),
            // Query-native codegen units own their backend subphases. They
            // remain beneath the compiler-owned collection boundary without
            // reviving a peer whole-program codegen coordinator.
            ("compile_pipeline", "codegen_collection"),
            ("codegen_collection", "codegen_unit"),
            ("codegen_unit", "mir_lowering"),
            ("codegen_unit", "register_allocation"),
            ("codegen_unit", "machine_emission"),
            // Object serialization runs after collecting the query units, so
            // it remains a sibling of those backend leaves.
            ("compile_pipeline", "object_serialization"),
            ("compile_pipeline", "linker"),
            ("linker", "link_parse_objects"),
            ("linker", "link_layout"),
            ("linker", "link_emit"),
        ] {
            assert!(
                compile_edges.contains(&(edge.0.to_owned(), edge.1.to_owned())),
                "missing {} -> {} in batch edges: {compile_edges:?}",
                edge.0,
                edge.1
            );
        }
        assert!(
            !compile_edges
                .iter()
                .any(|(parent, _)| parent == "semantic_astgen" || parent == "sema"),
            "normal compilation must not enter whole-program semantic spans: {compile_edges:?}"
        );

        let compile_timing =
            compile_data.to_benchmark_timing_with_metrics("test", "test", None, None, None);
        let compile = compile_timing
            .passes
            .iter()
            .find(|pass| pass.name == "compile")
            .unwrap();
        assert_eq!(compile.invocations, 1);
        assert_eq!(compile.root_invocations, 1);
        assert_eq!(compile.leaf_invocations, 0);
        let parser = compile_timing
            .passes
            .iter()
            .find(|pass| pass.name == "parser")
            .unwrap();
        assert_eq!(parser.root_invocations, 0);
        assert_eq!(parser.leaf_invocations, 0);
        for phase in [
            "parser_nesting_scan",
            "parser_state_setup",
            "parser_grammar_execution",
            "parser_directive_validation",
        ] {
            let timing = compile_timing
                .passes
                .iter()
                .find(|pass| pass.name == phase)
                .unwrap();
            assert_eq!(timing.root_invocations, 0, "{phase}");
        }
        let occurrence_index = compile_timing
            .passes
            .iter()
            .find(|pass| pass.name == "declaration_occurrence_index")
            .unwrap();
        assert_eq!(occurrence_index.invocations, 1);
        assert_eq!(occurrence_index.root_invocations, 0);
        assert_eq!(occurrence_index.leaf_invocations, 1);
        let declaration = compile_timing
            .passes
            .iter()
            .find(|pass| pass.name == "declaration_nucleus")
            .unwrap();
        // The rooted plan requests the declaration once while selecting the
        // entry point and once through the reached-body closure. The query
        // database reuses the value; both requests remain visible beneath the
        // pipeline aggregate.
        //
        // Only the second is a leaf. The first evaluates the signature and
        // opens `declaration_signature_projection` inside itself (RUE-1515);
        // the second reuses that value and nests nothing, so it stays a leaf.
        // A regression that stopped projecting would put this back at 2.
        assert_eq!(declaration.invocations, 2);
        assert_eq!(declaration.root_invocations, 0);
        assert_eq!(declaration.leaf_invocations, 1);
        for phase in [
            Phase::SemanticAnalysis,
            Phase::CfgAndOptimization,
            Phase::Backend,
        ] {
            assert!(
                compile_timing
                    .phase_accounting
                    .phase_ns
                    .get(&phase)
                    .copied()
                    .unwrap_or(0)
                    > 0,
                "query-native compilation must attribute {} work: {:?}",
                phase.wire_name(),
                compile_timing.phase_accounting
            );
        }
    }

    #[test]
    fn body_query_work_spans_report_query_native_phase_boundaries() {
        let snapshot = SourceSnapshot::single(
            "main.rue",
            "fn helper() -> i32 { 7 }\nfn main() -> i32 { helper() }",
        )
        .unwrap();

        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let mut session = CompilerSession::new();
            session.update(&snapshot).into_result().unwrap();
            rue_compiler::unstable::rooted_cfg(&mut session, &CompileOptions::default()).unwrap();
        });

        // Registered query evaluation is not represented as a presentation
        // coordinator span. The semantic phase has one work span for the
        // prerequisite fan-out and one for body analysis; the retired session
        // worklist and body-local epoch pipeline must not reappear.
        let timing = data.to_benchmark_timing_with_metrics("test", "test", None, None, None);
        for retired in [
            "body_queries",
            "body_transaction",
            "body_schedule",
            "body_record",
            "body_toolchain_demands",
            "body_derive_epoch",
            "body_prepare_declarations",
            "body_project_declarations",
            "body_install_declarations",
            "body_analyze",
            "body_export",
        ] {
            assert!(
                !timing.passes.iter().any(|pass| pass.name == retired),
                "retired body-local stage {retired} reappeared: {:?}",
                timing.passes
            );
        }
        let prerequisites = timing
            .passes
            .iter()
            .find(|pass| pass.name == "body_query_prerequisites")
            .unwrap_or_else(|| panic!("missing prerequisites pass: {:?}", timing.passes));
        assert!(
            prerequisites.invocations >= 2,
            "helper and main each run query prerequisites: {prerequisites:?}"
        );
        assert_eq!(
            prerequisites.root_invocations, 0,
            "rooted semantic presentation owns registered-query prerequisites"
        );
        assert_eq!(
            prerequisites.leaf_invocations, prerequisites.invocations,
            "registered-query prerequisites are presentation leaves"
        );
        assert!(data.parent_edges().contains(&(
            "body_closure_collection".to_owned(),
            "body_query_prerequisites".to_owned()
        )));
        let parse_file = timing
            .passes
            .iter()
            .find(|pass| pass.name == "parse_file")
            .unwrap();
        assert_eq!(
            parse_file.root_invocations, 0,
            "every reparse is owned by the phase which demanded it: {parse_file:?}"
        );
    }

    #[test]
    fn parser_failure_subphases_preserve_parentage() {
        let capture = |source: &str| {
            let snapshot = SourceSnapshot::single("main.rue", source).unwrap();
            let data = TimingData::new();
            let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
            tracing::subscriber::with_default(subscriber, || {
                let mut session = CompilerSession::new();
                update_for_presentation(&mut session, &snapshot)
                    .into_result()
                    .unwrap_err();
            });
            data.parent_edges()
        };

        let parse_error_edges = capture("fn main( {");
        assert!(
            parse_error_edges
                .contains(&("parser".to_owned(), "parser_grammar_execution".to_owned())),
            "parse-error edges: {parse_error_edges:?}"
        );
        assert!(
            !parse_error_edges
                .iter()
                .any(|(_, child)| child == "parser_directive_validation"),
            "directive validation must not run after grammar failure: {parse_error_edges:?}"
        );

        let validation_edges = capture("@important fn main() -> i32 { 0 }");
        assert!(
            validation_edges.contains(&(
                "parser".to_owned(),
                "parser_directive_validation".to_owned()
            )),
            "validation-error edges: {validation_edges:?}"
        );
        assert!(
            validation_edges
                .contains(&("parser".to_owned(), "parser_grammar_execution".to_owned()))
        );
    }

    #[test]
    fn driver_phase_spans_stay_out_of_the_compiler_total() {
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        tracing::subscriber::with_default(subscriber, || {
            {
                let _compile = tracing::info_span!("compile").entered();
                let _leaf = tracing::info_span!("sema").entered();
                std::thread::sleep(Duration::from_millis(5));
            }
            let _write = tracing::info_span!("output_write", driver_phase = true).entered();
            // A driver phase's own children are driver work too, so they must
            // not reappear as compiler passes either.
            let _nested = tracing::info_span!("output_fsync").entered();
            std::thread::sleep(Duration::from_millis(5));
        });

        let timing = data.to_benchmark_timing_with_metrics("test", "test", None, None, None);
        let pass_names: Vec<_> = timing
            .passes
            .iter()
            .map(|pass| pass.name.as_str())
            .collect();
        assert_eq!(pass_names, ["compile", "sema"], "{pass_names:?}");
        let phase_names: Vec<_> = timing
            .driver_phases
            .iter()
            .map(|phase| phase.name.as_str())
            .collect();
        assert_eq!(phase_names, ["output_fsync", "output_write"]);
        assert!(
            timing
                .driver_phases
                .iter()
                .all(|phase| phase.invocations == 1)
        );

        // `compile` remains the sole root and still owns the whole total, so a
        // driver phase can never inflate or dilute the compiler's percentages.
        let compile = timing
            .passes
            .iter()
            .find(|pass| pass.name == "compile")
            .unwrap();
        assert_eq!(compile.root_invocations, 1);
        assert!((compile.duration_ms - timing.total_ms).abs() < f64::EPSILON);
        let write = timing
            .driver_phases
            .iter()
            .find(|phase| phase.name == "output_write")
            .unwrap();
        assert!(write.duration_ms > 0.0);

        let json = data.to_json_with_metrics("test", "test", None, None, None);
        assert!(json.contains("\"driver_phases\""), "{json}");
        assert!(data.report().contains("Driver phases"));
    }

    #[test]
    fn a_run_without_driver_phases_omits_the_field_entirely() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(1));
        let json = data.to_json_with_metrics("test", "test", None, None, None);
        assert!(!json.contains("driver_phases"), "{json}");
        assert!(!data.report().contains("Driver phases"));
    }

    #[test]
    fn test_timing_data_empty_report() {
        let data = TimingData::new();
        let report = data.report();
        assert!(report.contains("No timing data collected"));
    }

    #[test]
    fn test_timing_data_record_and_report() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));
        data.record("parser", Duration::from_millis(200));

        let report = data.report();
        assert!(report.contains("Compilation Timing"));
        assert!(report.contains("Lexer"));
        assert!(report.contains("Parser"));
        assert!(report.contains("Total"));
    }

    #[test]
    fn test_timing_data_order_is_deterministic() {
        let data = TimingData::new();
        data.record("aaa", Duration::from_millis(100));
        data.record("zzz", Duration::from_millis(100));
        data.record("mmm", Duration::from_millis(100));

        let report = data.report();
        let aaa_pos = report.find("Aaa").unwrap();
        let zzz_pos = report.find("Zzz").unwrap();
        let mmm_pos = report.find("Mmm").unwrap();

        // Pass presentation is stable regardless of worker completion order.
        assert!(aaa_pos < mmm_pos);
        assert!(mmm_pos < zzz_pos);
    }

    #[test]
    fn test_timing_data_accumulates() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));
        data.record("lexer", Duration::from_millis(50));

        let report = data.report();
        // Should show ~150ms for lexer
        assert!(report.contains("150"));
    }

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("lexer"), "Lexer");
        assert_eq!(capitalize("PARSER"), "PARSER");
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("a"), "A");
    }

    #[test]
    fn test_to_benchmark_timing_empty() {
        let data = TimingData::new();
        let timing =
            data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None, None);
        assert!(timing.passes.is_empty());
        assert_eq!(timing.total_ms, 0.0);
    }

    #[test]
    fn test_to_benchmark_timing_with_data() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));
        data.record("parser", Duration::from_millis(200));

        let timing =
            data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None, None);
        assert_eq!(timing.passes.len(), 2);
        assert_eq!(timing.passes[0].name, "lexer");
        assert_eq!(timing.passes[1].name, "parser");
        // Total should be ~300ms
        assert!((timing.total_ms - 300.0).abs() < 1.0);
    }

    #[test]
    fn test_to_benchmark_timing_percentages() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));
        data.record("parser", Duration::from_millis(300));

        let timing =
            data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None, None);
        // lexer should be 25%, parser should be 75%
        assert!((timing.passes[0].percent - 25.0).abs() < 0.1);
        assert!((timing.passes[1].percent - 75.0).abs() < 0.1);
    }

    #[test]
    fn nested_spans_do_not_inflate_total() {
        let data = TimingData::new();
        data.record_test_span("compile", Duration::from_millis(10), true, false);
        data.record_test_span("parse", Duration::from_millis(6), false, false);
        data.record_test_span("lexer", Duration::from_millis(4), false, true);
        data.record_test_span("parser", Duration::from_millis(2), false, true);
        data.record_test_span("codegen", Duration::from_millis(4), false, true);

        let timing =
            data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None, None);
        assert!((timing.total_ms - 10.0).abs() < f64::EPSILON);

        let compile = timing
            .passes
            .iter()
            .find(|pass| pass.name == "compile")
            .unwrap();
        assert!((compile.percent - 100.0).abs() < f64::EPSILON);
        assert_eq!(compile.invocations, 1);
        assert_eq!(compile.root_invocations, 1);
        assert_eq!(compile.leaf_invocations, 0);

        let leaf_ms: f64 = timing
            .passes
            .iter()
            .filter(|pass| pass.leaf_invocations == pass.invocations)
            .map(|pass| pass.duration_ms)
            .sum();
        assert!((leaf_ms - timing.total_ms).abs() < f64::EPSILON);

        let inclusive_sum: f64 = timing.passes.iter().map(|pass| pass.duration_ms).sum();
        assert!(inclusive_sum > timing.total_ms);
    }

    #[test]
    fn overlapping_roots_contribute_their_wall_time_union() {
        let data = TimingData::new();
        let start = Instant::now();
        data.root_enter_at(start);
        data.root_enter_at(start + Duration::from_millis(10));
        data.root_exit_at(start + Duration::from_millis(20));
        data.root_exit_at(start + Duration::from_millis(30));

        assert_eq!(data.phase_accounting().compiler_root_ns, 30_000_000);
    }

    #[test]
    fn out_of_order_local_merges_reconstruct_the_root_union() {
        let data = TimingData::new();
        let start = Instant::now();
        let mut first = SpanTiming {
            active_enters: 0,
            active_since: None,
            accumulated: Duration::ZERO,
            has_children: false,
            is_root: true,
            is_driver_phase: false,
            phase: None,
        };
        let mut delayed = SpanTiming {
            active_enters: 0,
            active_since: None,
            accumulated: Duration::ZERO,
            has_children: false,
            is_root: true,
            is_driver_phase: false,
            phase: None,
        };

        // Model a worker buffer merged after a later callback. Finalization
        // orders observations by their captured timestamps, not merge order.
        data.enter_root_span(&mut first, start);
        data.exit_root_span(&mut first, start + Duration::from_millis(20));
        data.enter_root_span(&mut delayed, start + Duration::from_millis(10));
        data.exit_root_span(&mut delayed, start + Duration::from_millis(30));

        assert_eq!(first.duration(), Duration::from_millis(20));
        assert_eq!(delayed.duration(), Duration::from_millis(20));
        assert_eq!(data.phase_accounting().compiler_root_ns, 30_000_000);
    }

    #[test]
    fn reentrant_span_contributes_its_wall_time_union() {
        let start = Instant::now();
        let mut timing = SpanTiming {
            active_enters: 0,
            active_since: None,
            accumulated: Duration::ZERO,
            has_children: false,
            is_root: false,
            is_driver_phase: false,
            phase: None,
        };
        timing.enter_at(start);
        timing.enter_at(start + Duration::from_millis(10));
        timing.exit_at(start + Duration::from_millis(20));
        timing.exit_at(start + Duration::from_millis(30));

        assert_eq!(timing.duration(), Duration::from_millis(30));
    }

    #[test]
    fn test_to_benchmark_timing_metadata() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));

        let timing =
            data.to_benchmark_timing_with_metrics("aarch64-macos", "0.2.0", None, None, None);
        assert_eq!(timing.metadata.target, "aarch64-macos");
        assert_eq!(timing.metadata.version, "0.2.0");
        // Timestamp should be an ISO 8601 format
        assert!(timing.metadata.timestamp.contains('T'));
        assert!(timing.metadata.timestamp.ends_with('Z'));
    }

    #[test]
    fn test_to_json_structure() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));

        let json = data.to_json_with_metrics("x86_64-linux", "0.1.0", None, None, None);
        assert!(json.contains("\"schema_version\":16"));
        assert!(json.contains(&format!(
            "\"compiler_build_profile\":\"{COMPILER_BUILD_PROFILE}\""
        )));
        assert!(json.contains("\"timing_model\":\"inclusive_spans\""));
        assert!(json.contains("\"passes\""));
        assert!(json.contains("\"name\""));
        assert!(json.contains("\"lexer\""));
        assert!(json.contains("\"duration_ms\""));
        assert!(json.contains("\"percent\""));
        assert!(json.contains("\"invocations\""));
        assert!(json.contains("\"root_invocations\""));
        assert!(json.contains("\"leaf_invocations\""));
        assert!(json.contains("\"total_ms\""));
        // Should also contain metadata
        assert!(json.contains("\"metadata\""));
        assert!(json.contains("\"timestamp\""));
        assert!(json.contains("\"version\""));
        assert!(json.contains("\"target\""));
    }

    #[test]
    fn test_to_json_empty() {
        let data = TimingData::new();
        let json = data.to_json_with_metrics("x86_64-linux", "0.1.0", None, None, None);
        // Should produce valid JSON even with empty data
        assert!(json.contains("\"passes\":[]"));
        assert!(json.contains("\"total_ms\":0"));
    }

    #[test]
    fn test_benchmark_timing_order_is_deterministic() {
        let data = TimingData::new();
        data.record("aaa", Duration::from_millis(100));
        data.record("zzz", Duration::from_millis(100));
        data.record("mmm", Duration::from_millis(100));

        let timing =
            data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None, None);
        assert_eq!(timing.passes[0].name, "aaa");
        assert_eq!(timing.passes[1].name, "mmm");
        assert_eq!(timing.passes[2].name, "zzz");
    }

    #[test]
    fn test_iso8601_now() {
        let timestamp = iso8601_now();
        // Should be in ISO 8601 format: YYYY-MM-DDTHH:MM:SSZ
        assert!(timestamp.contains('T'));
        assert!(timestamp.ends_with('Z'));
        assert_eq!(timestamp.len(), 20); // "2025-12-27T21:30:00Z"
    }

    #[test]
    fn test_days_to_ymd_epoch() {
        // Day 0 should be 1970-01-01
        let (year, month, day) = days_to_ymd(0);
        assert_eq!(year, 1970);
        assert_eq!(month, 1);
        assert_eq!(day, 1);
    }

    #[test]
    fn test_days_to_ymd_known_date() {
        // Test a known date: 2000-01-01 is 10957 days since epoch
        // (calculated as: 30 years, with 7 leap years: 1972,76,80,84,88,92,96)
        // 30 * 365 + 7 = 10957
        let (year, month, day) = days_to_ymd(10957);
        assert_eq!(year, 2000);
        assert_eq!(month, 1);
        assert_eq!(day, 1);
    }

    #[test]
    fn test_is_leap_year() {
        assert!(!is_leap_year(1900)); // divisible by 100 but not 400
        assert!(is_leap_year(2000)); // divisible by 400
        assert!(is_leap_year(2024)); // divisible by 4, not by 100
        assert!(!is_leap_year(2025)); // not divisible by 4
    }
}

/// Measurement-boundary tests for the ADR-0067 wall-clock phase partition.
///
/// These drive the real tracing span lifecycle rather than poking the
/// aggregate, because the properties under test are about *transitions* — which
/// bucket an interval lands in depends on the state at the moment a span is
/// entered or exited.
#[cfg(test)]
mod phase_accounting_tests {
    use std::sync::Barrier;
    use std::thread;
    use std::time::Duration;

    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    /// Long enough that a scheduler hiccup cannot make an interval read as zero,
    /// short enough to keep the suite fast.
    const TICK: Duration = Duration::from_millis(20);

    fn collect(body: impl FnOnce()) -> PhaseAccounting {
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        tracing::subscriber::with_default(subscriber, body);
        data.phase_accounting()
    }

    fn event(data: &TimingData, at: Instant, transition: AccountingTransition) {
        data.record_accounting(at, [Some(transition), None]);
    }

    fn phase_ns(accounting: &PhaseAccounting, phase: Phase) -> u64 {
        accounting.phase_ns.get(&phase).copied().unwrap_or(0)
    }

    /// The property the whole design rests on: the bands partition root time
    /// exactly, in integer nanoseconds, with no slack to absorb a mistake.
    fn assert_invariant(accounting: &PhaseAccounting) {
        assert!(
            accounting.holds(),
            "bands sum to {} ns but compiler root is {} ns: {accounting:?}",
            accounting.attributed_ns(),
            accounting.compiler_root_ns,
        );
        assert!(
            accounting.missing_phases().is_empty(),
            "every published phase must be present, including zero: {:?}",
            accounting.missing_phases()
        );
    }

    fn accounting_from_observations(
        root_events: &[AccountingEvent],
        band_events: &[AccountingEvent],
        now: Instant,
    ) -> PhaseAccounting {
        let timeline = phase_bands(band_events, now);
        PhaseAccounting {
            phase_ns: Phase::ALL
                .into_iter()
                .map(|phase| (phase, duration_ns(timeline.phase_durations[phase.index()])))
                .collect(),
            mixed_parallel_ns: duration_ns(timeline.mixed_parallel),
            unattributed_ns: duration_ns(timeline.unattributed),
            compiler_root_ns: duration_ns(compiler_root_union(root_events, now)),
        }
    }

    #[test]
    fn independent_root_union_exposes_a_corrupted_band_observation() {
        let start = Instant::now();
        let events = vec![
            AccountingEvent {
                at: start,
                transition: AccountingTransition::RootEnter,
            },
            AccountingEvent {
                at: start + Duration::from_millis(2),
                transition: AccountingTransition::PhaseEnter(Phase::Backend),
            },
            AccountingEvent {
                at: start + Duration::from_millis(8),
                transition: AccountingTransition::PhaseExit(Phase::Backend),
            },
            AccountingEvent {
                at: start + Duration::from_millis(10),
                transition: AccountingTransition::RootExit,
            },
        ];
        let now = start + Duration::from_millis(10);
        assert_invariant(&accounting_from_observations(&events, &events, now));

        // Simulate publication losing the root-enter observation on the band
        // side only. A total accumulated inside that same reducer would still
        // agree with its empty bands; the independent root union retains the
        // true 10 ms interval and exposes the mismatch.
        let corrupted_bands = events[1..].to_vec();
        let corrupted = accounting_from_observations(&events, &corrupted_bands, now);
        assert_eq!(corrupted.compiler_root_ns, 10_000_000);
        assert_eq!(corrupted.attributed_ns(), 0);
        assert!(
            !corrupted.holds(),
            "corruption must be observable: {corrupted:?}"
        );
    }

    #[test]
    fn worker_local_pass_accumulation_merges_once_and_exactly() {
        let data = TimingData::new();
        thread::scope(|scope| {
            for duration in [11, 13, 17] {
                let data = data.clone();
                scope.spawn(move || {
                    data.record_span("worker_pass", Duration::from_millis(duration), false, true);
                    // Registered workers invoke this same bounded publication
                    // operation through the generic timing-flush event.
                    data.flush_local();
                });
            }
        });
        data.flush_local();
        let inner = data.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let pass = inner.passes.get("worker_pass").unwrap();
        assert_eq!(pass.duration, Duration::from_millis(41));
        assert_eq!(pass.max_duration, Duration::from_millis(17));
        assert_eq!(pass.invocations, 3);
        assert_eq!(pass.leaf_invocations, 3);
        drop(inner);

        let distribution = data.pass_duration_distribution("worker_pass");
        assert_eq!(distribution.count, 3);
        assert_eq!(distribution.total_ns, 41_000_000);
        assert_eq!(distribution.max_ns, 17_000_000);
        assert_eq!(
            distribution.log2_buckets[duration_bucket(Duration::from_millis(11))],
            2
        );
        assert_eq!(
            distribution.log2_buckets[duration_bucket(Duration::from_millis(17))],
            1
        );
        assert!(distribution.validate());
    }

    #[test]
    fn direct_duration_event_populates_the_bounded_distribution() {
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(
                name: "direct_duration",
                target: "rue::timing",
                tracing::Level::INFO,
                duration_ns = 64_u64,
            );
            tracing::event!(
                name: "direct_duration",
                target: "rue::timing",
                tracing::Level::INFO,
                duration_ns = 32_u64,
            );
        });

        let distribution = data.pass_duration_distribution("direct_duration");
        assert_eq!(distribution.count, 2);
        assert_eq!(distribution.total_ns, 96);
        assert_eq!(distribution.max_ns, 64);
        assert_eq!(distribution.log2_buckets[5], 1);
        assert_eq!(distribution.log2_buckets[6], 1);
    }

    #[test]
    fn cfg_breakdown_event_populates_each_bounded_distribution() {
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(
                name: "cfg_construction_breakdown",
                target: "rue::timing",
                tracing::Level::INFO,
                input_preparation_ns = 2_u64,
                semantic_materialization_ns = 4_u64,
                domain_prerequisites_ns = 8_u64,
                domain_projection_ns = 16_u64,
                prerequisite_collection_ns = 32_u64,
                prerequisite_queries_ns = 64_u64,
                cfg_builder_ns = 128_u64,
                cfg_publication_ns = 256_u64,
            );
        });

        for (name, expected_ns) in [
            ("cfg_input_preparation", 2),
            ("semantic_materialization", 4),
            ("cfg_domain_prerequisites", 8),
            ("cfg_domain_projection", 16),
            ("cfg_prerequisite_collection", 32),
            ("cfg_prerequisite_queries", 64),
            ("cfg_builder", 128),
            ("cfg_publication", 256),
        ] {
            let distribution = data.pass_duration_distribution(name);
            assert_eq!(distribution.count, 1, "{name}");
            assert_eq!(distribution.total_ns, expected_ns, "{name}");
            assert_eq!(distribution.max_ns, expected_ns, "{name}");
        }
    }

    #[test]
    fn semantic_provider_breakdown_event_populates_each_bounded_distribution() {
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(
                name: "semantic_provider_breakdown",
                target: "rue::timing",
                tracing::Level::INFO,
                host_setup_ns = 2_u64,
                expression_engine_ns = 4_u64,
                specialization_selection_ns = 8_u64,
                body_export_ns = 16_u64,
                result_projection_ns = 32_u64,
                setup_ns = 64_u64,
                inference_precompute_ns = 128_u64,
                inference_precompute_structural_ns = 80_u64,
                inference_precompute_eval_provider_ns = 48_u64,
                precompute_alias_nodes_visited = 7_u64,
                precompute_inline_scan_bodies = 1_u64,
                constraint_generation_ns = 256_u64,
                unification_resolution_ns = 512_u64,
                air_emission_validation_ns = 1024_u64,
            );
        });

        for (name, expected_ns) in [
            ("semantic_provider_host_setup", 2),
            ("semantic_provider_expression_engine", 4),
            ("semantic_provider_specialization_selection", 8),
            ("semantic_provider_body_export", 16),
            ("semantic_provider_result_projection", 32),
            ("semantic_expression_setup", 64),
            ("semantic_inference_precompute", 128),
            ("semantic_inference_precompute_structural", 80),
            ("semantic_inference_precompute_eval_provider", 48),
            ("semantic_constraint_generation", 256),
            ("semantic_unification_resolution", 512),
            ("semantic_air_emission_validation", 1024),
        ] {
            let distribution = data.pass_duration_distribution(name);
            assert_eq!(distribution.count, 1, "{name}");
            assert_eq!(distribution.total_ns, expected_ns, "{name}");
            assert_eq!(distribution.max_ns, expected_ns, "{name}");
        }
        assert_eq!(data.counter_total("precompute_bodies"), 1);
        assert_eq!(data.counter_total("precompute_alias_nodes_visited"), 7);
        assert_eq!(data.counter_total("precompute_inline_scan_bodies"), 1);
    }

    #[test]
    fn semantic_body_lowering_event_partitions_time_and_aggregates_work() {
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(
                name: "semantic_body_lowering_breakdown",
                target: "rue::timing",
                tracing::Level::INFO,
                attributed_total_ns = 31_u64,
                assembly_snapshot_ns = 1_u64,
                lex_parse_ns = 2_u64,
                rir_lower_ns = 4_u64,
                span_remap_validation_ns = 8_u64,
                body_rir_index_ns = 16_u64,
                source_bytes = 100_u64,
                index_builds = 1_u64,
            );
        });
        for (name, expected) in [
            ("semantic_body_input_attributed_total", 31),
            ("semantic_body_input_assembly_snapshot", 1),
            ("semantic_body_input_lex_parse", 2),
            ("semantic_body_input_rir_lower", 4),
            ("semantic_body_input_span_remap_validation", 8),
            ("semantic_body_input_rir_index", 16),
        ] {
            assert_eq!(data.pass_duration_distribution(name).total_ns, expected);
        }
        assert_eq!(data.counter_total("body_lowerings"), 1);
        assert_eq!(data.counter_total("source_bytes"), 100);
        assert_eq!(data.counter_total("index_builds"), 1);
    }

    #[test]
    fn parallel_worker_events_merge_into_exact_phase_bands() {
        let data = TimingData::new();
        let start = Instant::now();
        event(&data, start, AccountingTransition::RootEnter);
        thread::scope(|scope| {
            for (phase, enter_ms, exit_ms) in
                [(Phase::SemanticAnalysis, 10, 30), (Phase::Backend, 20, 40)]
            {
                let data = data.clone();
                scope.spawn(move || {
                    event(
                        &data,
                        start + Duration::from_millis(enter_ms),
                        AccountingTransition::PhaseEnter(phase),
                    );
                    event(
                        &data,
                        start + Duration::from_millis(exit_ms),
                        AccountingTransition::PhaseExit(phase),
                    );
                    // This synthetic worker has no query-runtime lifecycle
                    // marker. Publish at its explicit completion boundary
                    // instead of depending on platform TLS teardown timing.
                    data.flush_local();
                });
            }
        });
        event(
            &data,
            start + Duration::from_millis(50),
            AccountingTransition::RootExit,
        );

        let accounting = data.phase_accounting();
        assert_invariant(&accounting);
        assert_eq!(phase_ns(&accounting, Phase::SemanticAnalysis), 10_000_000);
        assert_eq!(phase_ns(&accounting, Phase::Backend), 10_000_000);
        assert_eq!(accounting.mixed_parallel_ns, 10_000_000);
        assert_eq!(accounting.unattributed_ns, 20_000_000);
        assert_eq!(accounting.compiler_root_ns, 50_000_000);
    }

    #[test]
    fn work_stolen_between_workers_keeps_one_phase_interval() {
        let data = TimingData::new();
        let start = Instant::now();
        event(&data, start, AccountingTransition::RootEnter);
        thread::scope(|scope| {
            let entering = data.clone();
            scope.spawn(move || {
                event(
                    &entering,
                    start + Duration::from_millis(5),
                    AccountingTransition::PhaseEnter(Phase::Backend),
                );
                entering.flush_local();
            });
            let exiting = data.clone();
            scope.spawn(move || {
                event(
                    &exiting,
                    start + Duration::from_millis(25),
                    AccountingTransition::PhaseExit(Phase::Backend),
                );
                exiting.flush_local();
            });
        });
        event(
            &data,
            start + Duration::from_millis(30),
            AccountingTransition::RootExit,
        );

        let accounting = data.phase_accounting();
        assert_invariant(&accounting);
        assert_eq!(phase_ns(&accounting, Phase::Backend), 20_000_000);
        assert_eq!(accounting.unattributed_ns, 10_000_000);
    }

    #[test]
    fn canceled_phase_scope_merges_its_completed_prefix_exactly() {
        let data = TimingData::new();
        let start = Instant::now();
        for (millis, transition) in [
            (0, AccountingTransition::RootEnter),
            (5, AccountingTransition::PhaseEnter(Phase::SemanticAnalysis)),
            (12, AccountingTransition::PhaseExit(Phase::SemanticAnalysis)),
            (20, AccountingTransition::RootExit),
        ] {
            event(&data, start + Duration::from_millis(millis), transition);
        }

        let accounting = data.phase_accounting();
        assert_invariant(&accounting);
        assert_eq!(phase_ns(&accounting, Phase::SemanticAnalysis), 7_000_000);
        assert_eq!(accounting.unattributed_ns, 13_000_000);
        assert_eq!(accounting.compiler_root_ns, 20_000_000);
    }

    #[test]
    fn a_single_phase_owns_its_interval_exactly() {
        let accounting = collect(|| {
            let _root = tracing::info_span!("compile").entered();
            let _phase = tracing::info_span!("sema", phase = "semantic_analysis").entered();
            thread::sleep(TICK);
        });

        assert_invariant(&accounting);
        let semantic = phase_ns(&accounting, Phase::SemanticAnalysis);
        assert!(semantic > 0, "the sole active phase must be charged");
        // Everything under the root belongs to that one phase, so the two
        // structural buckets stay empty.
        assert_eq!(accounting.mixed_parallel_ns, 0);
        assert!(
            semantic * 2 > accounting.compiler_root_ns,
            "the phase should dominate root time: {accounting:?}"
        );
    }

    #[test]
    fn time_under_the_root_with_no_phase_is_unattributed() {
        let accounting = collect(|| {
            let _root = tracing::info_span!("compile").entered();
            thread::sleep(TICK);
        });

        assert_invariant(&accounting);
        assert!(accounting.unattributed_ns > 0);
        assert_eq!(accounting.mixed_parallel_ns, 0);
        for phase in Phase::ALL {
            assert_eq!(phase_ns(&accounting, phase), 0, "{}", phase.wire_name());
        }
    }

    #[test]
    fn time_outside_the_root_is_excluded_entirely() {
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        tracing::subscriber::with_default(subscriber, || {
            {
                let _root = tracing::info_span!("compile").entered();
                thread::sleep(TICK);
            }
            let before_driver_overhead = data.phase_accounting();
            // The root has closed. This interval is real time the process
            // spends, but it is driver overhead rather than compiler work, so it
            // must not enter the phase stack at all.
            thread::sleep(TICK * 2);
            let after_driver_overhead = data.phase_accounting();
            assert_eq!(
                before_driver_overhead.compiler_root_ns, after_driver_overhead.compiler_root_ns,
                "driver time leaked into the root total: {after_driver_overhead:?}"
            );
        });

        let accounting = data.phase_accounting();
        assert_invariant(&accounting);
    }

    #[test]
    fn two_distinct_concurrent_phases_are_mixed_parallel() {
        let accounting = collect(|| {
            let _root = tracing::info_span!("compile").entered();
            let _first = tracing::info_span!("sema", phase = "semantic_analysis").entered();
            let _second = tracing::info_span!("codegen", phase = "backend").entered();
            thread::sleep(TICK);
        });

        assert_invariant(&accounting);
        assert!(
            accounting.mixed_parallel_ns > 0,
            "overlapping distinct phases must be mixed, never split: {accounting:?}"
        );
        // The first phase owns only the gap between the two `entered()` calls,
        // which is two transitions apart and therefore tiny. The overlapping
        // interval itself goes to neither: fractional attribution across
        // concurrent phases is deliberately not offered.
        let semantic = phase_ns(&accounting, Phase::SemanticAnalysis);
        assert_eq!(phase_ns(&accounting, Phase::Backend), 0);
        assert!(
            accounting.mixed_parallel_ns > semantic * 4,
            "the shared interval must dominate the entry gap: {accounting:?}"
        );
    }

    #[test]
    fn nesting_two_phases_is_reported_as_mixed_rather_than_innermost_wins() {
        // Documents the rule that forces phase markers onto genuinely
        // non-overlapping boundaries: a nested marker is not "more specific",
        // it is two phases active at once.
        let accounting = collect(|| {
            let _root = tracing::info_span!("compile").entered();
            let _outer = tracing::info_span!("outer", phase = "semantic_analysis").entered();
            thread::sleep(TICK);
            let _inner = tracing::info_span!("inner", phase = "cfg_and_optimization").entered();
            thread::sleep(TICK);
        });

        assert_invariant(&accounting);
        assert!(
            phase_ns(&accounting, Phase::SemanticAnalysis) > 0,
            "{accounting:?}"
        );
        assert!(accounting.mixed_parallel_ns > 0, "{accounting:?}");
        assert_eq!(phase_ns(&accounting, Phase::CfgAndOptimization), 0);
    }

    #[test]
    fn concurrent_spans_of_the_same_phase_are_that_phase_not_mixed() {
        // This is the Rayon shape: RUE-786 has workers re-enter one `codegen`
        // span by value, so the same phase is entered many times at once. The
        // reference count must absorb that rather than reporting mixed.
        let accounting = collect(|| {
            let _root = tracing::info_span!("compile").entered();
            let first = tracing::info_span!("codegen", phase = "backend");
            let _a = first.clone().entered();
            let _b = first.clone().entered();
            let _c = first.entered();
            thread::sleep(TICK);
        });

        assert_invariant(&accounting);
        assert!(phase_ns(&accounting, Phase::Backend) > 0, "{accounting:?}");
        assert_eq!(
            accounting.mixed_parallel_ns, 0,
            "same-phase concurrency is not mixed: {accounting:?}"
        );
    }

    #[test]
    fn a_phase_stays_active_until_its_last_concurrent_span_exits() {
        let accounting = collect(|| {
            let _root = tracing::info_span!("compile").entered();
            let span = tracing::info_span!("codegen", phase = "backend");
            let outer = span.clone().entered();
            {
                let _inner = span.entered();
                thread::sleep(TICK);
            }
            // One span exited, but the count is still positive, so the phase
            // owns this interval too.
            thread::sleep(TICK);
            drop(outer);
            thread::sleep(TICK);
        });

        assert_invariant(&accounting);
        let backend = phase_ns(&accounting, Phase::Backend);
        assert!(
            backend >= duration_ns(TICK),
            "the phase must cover both concurrent intervals: {accounting:?}"
        );
        assert!(
            accounting.unattributed_ns > 0,
            "the interval after the last exit is unattributed: {accounting:?}"
        );
    }

    #[test]
    fn phases_entered_on_rayon_workers_are_accounted_across_threads() {
        // Real threads and independent local buffers. The final partition is of
        // wall time globally, so overlapping workers in one phase still yield
        // that phase.
        //
        // Each worker publishes through the same bounded `timing_flush`
        // completion marker a registered query worker emits — and, exactly as
        // the query runtime does for its batch workers, installs the
        // propagated dispatcher first so the marker reaches THIS subscriber.
        // An earlier draft skipped the propagation: `with_default` is
        // thread-scoped, so the worker's marker dispatched to the thread's
        // (unset) global default and publication silently fell back to TLS
        // teardown — which `thread::scope` does not order before its join, so
        // the reader raced the workers' destructors and sometimes saw every
        // phase band empty (RUE-1283).
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        let dispatch = tracing::Dispatch::new(subscriber);
        tracing::dispatcher::with_default(&dispatch, || {
            let _root = tracing::info_span!("compile").entered();
            let span = tracing::info_span!("codegen", phase = "backend");
            thread::scope(|scope| {
                for _ in 0..4 {
                    let worker_span = span.clone();
                    let worker_dispatch = dispatch.clone();
                    scope.spawn(move || {
                        tracing::dispatcher::with_default(&worker_dispatch, || {
                            {
                                let _entered = worker_span.entered();
                                thread::sleep(TICK);
                            }
                            tracing::trace!(timing_flush = true, "test worker complete");
                        });
                    });
                }
            });
        });
        let accounting = data.phase_accounting();

        assert_eq!(
            data.flush_threads().len(),
            4,
            "every worker's completion marker must reach this layer and flush"
        );
        assert_invariant(&accounting);
        assert!(phase_ns(&accounting, Phase::Backend) > 0, "{accounting:?}");
        assert_eq!(
            accounting.mixed_parallel_ns, 0,
            "four workers in one phase is not mixed: {accounting:?}"
        );
    }

    #[test]
    fn worker_flush_marker_publishes_phase_events_before_thread_teardown() {
        // The deterministic form of the RUE-1283 race: the `timing_flush`
        // marker must merge the worker's local buffer synchronously, so the
        // phase bands are complete the moment the worker has emitted it —
        // thread teardown is only a fallback, and `thread::scope` gives it no
        // ordering against the scope's join. The barriers hold the worker
        // alive past its flush, so a read that sees the phase can only have
        // been served by the marker, never by teardown.
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        let dispatch = tracing::Dispatch::new(subscriber);
        let flushed = Barrier::new(2);
        let release = Barrier::new(2);
        tracing::dispatcher::with_default(&dispatch, || {
            let _root = tracing::info_span!("compile").entered();
            let span = tracing::info_span!("codegen", phase = "backend");
            thread::scope(|scope| {
                let worker_span = span.clone();
                let worker_dispatch = dispatch.clone();
                let (flushed, release) = (&flushed, &release);
                scope.spawn(move || {
                    tracing::dispatcher::with_default(&worker_dispatch, || {
                        {
                            let _entered = worker_span.entered();
                            thread::sleep(TICK);
                        }
                        tracing::trace!(timing_flush = true, "worker flushed");
                    });
                    flushed.wait();
                    // Stay alive until the reader is done: TLS teardown must
                    // not be able to publish on this test's behalf.
                    release.wait();
                });
                flushed.wait();
                let accounting = data.phase_accounting();
                assert!(
                    phase_ns(&accounting, Phase::Backend) > 0,
                    "the flush marker alone must have published the worker's \
                     phase interval: {accounting:?}"
                );
                release.wait();
            });
        });
    }

    #[test]
    fn the_invariant_holds_across_many_interleaved_transitions() {
        let accounting = collect(|| {
            let _root = tracing::info_span!("compile").entered();
            for round in 0..12 {
                let phase = Phase::ALL[round % Phase::ALL.len()];
                let _span = tracing::info_span!("work", phase = phase.wire_name()).entered();
                thread::sleep(Duration::from_millis(2));
            }
        });

        assert_invariant(&accounting);
    }

    #[test]
    fn a_phase_marker_is_not_inherited_by_child_spans() {
        // Inheriting would make every descendant re-enter its ancestor's phase,
        // inflating the reference count without a matching boundary. An
        // unmarked child simply continues its parent's phase.
        let accounting = collect(|| {
            let _root = tracing::info_span!("compile").entered();
            let _parent = tracing::info_span!("sema", phase = "semantic_analysis").entered();
            let _child = tracing::info_span!("body_analysis").entered();
            thread::sleep(TICK);
        });

        assert_invariant(&accounting);
        assert!(phase_ns(&accounting, Phase::SemanticAnalysis) > 0);
        assert_eq!(
            accounting.mixed_parallel_ns, 0,
            "an unmarked child must not register as a second phase: {accounting:?}"
        );
    }

    #[test]
    fn an_unrecognized_phase_name_marks_no_phase() {
        let accounting = collect(|| {
            let _root = tracing::info_span!("compile").entered();
            let _span = tracing::info_span!("work", phase = "not_a_published_phase").entered();
            thread::sleep(TICK);
        });

        assert_invariant(&accounting);
        // Visible as unattributed rather than silently misfiled under some
        // nearby phase.
        assert!(accounting.unattributed_ns > 0);
        for phase in Phase::ALL {
            assert_eq!(phase_ns(&accounting, phase), 0, "{}", phase.wire_name());
        }
    }

    #[test]
    fn a_driver_phase_is_never_a_published_compiler_phase() {
        // Driver work lives outside the compiler root by construction, so even
        // an explicit phase marker on it must not enter the stack.
        let accounting = collect(|| {
            {
                let _root = tracing::info_span!("compile").entered();
                thread::sleep(Duration::from_millis(2));
            }
            let _driver =
                tracing::info_span!("output_write", driver_phase = true, phase = "linking")
                    .entered();
            thread::sleep(TICK);
        });

        assert_invariant(&accounting);
        assert_eq!(phase_ns(&accounting, Phase::Linking), 0, "{accounting:?}");
    }

    #[test]
    fn an_unbalanced_phase_exit_does_not_panic_or_corrupt_the_partition() {
        let data = TimingData::new();
        data.exit_phase(Phase::Backend);
        data.exit_phase(Phase::Backend);
        data.enter_phase(Phase::Backend);
        data.exit_phase(Phase::Backend);
        let accounting = data.phase_accounting();
        assert_invariant(&accounting);
    }

    #[test]
    fn the_benchmark_json_reports_both_timing_models_without_mixing_them() {
        let data = TimingData::new();
        let subscriber = tracing_subscriber::registry().with(TimingLayer::new(data.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let _root = tracing::info_span!("compile").entered();
            let _phase = tracing::info_span!("sema", phase = "semantic_analysis").entered();
            let _child = tracing::info_span!("body_analysis").entered();
            thread::sleep(TICK);
        });

        let timing =
            data.to_benchmark_timing_with_metrics("probe-target", "probe", None, None, None);
        assert_eq!(timing.schema_version, 16);
        assert_eq!(
            timing.metadata.compiler_build_profile,
            COMPILER_BUILD_PROFILE
        );
        assert_eq!(timing.timing_model, "inclusive_spans");
        assert_invariant(&timing.phase_accounting);

        // The inclusive table still holds the nested child, which the additive
        // partition deliberately does not surface as its own band.
        assert!(
            timing
                .passes
                .iter()
                .any(|pass| pass.name == "body_analysis")
        );

        // total_ms and the partition describe the same instant.
        let root_ms = timing.phase_accounting.compiler_root_ns as f64 / 1_000_000.0;
        assert!(
            (timing.total_ms - root_ms).abs() < f64::EPSILON,
            "total_ms {} disagrees with compiler_root_ns {root_ms}",
            timing.total_ms
        );
    }
}
