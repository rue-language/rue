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
//!    to hook into span enter/exit events and accumulate timing data.
//!
//! 3. **Reporting**: After compilation, `TimingData::report()` formats the collected
//!    timing as a human-readable table. For machine-readable output, use
//!    `TimingData::to_json()`.
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use tracing::Subscriber;
use tracing::span::{Attributes, Id};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Accumulated timing data for all compiler passes.
///
/// This is wrapped in `Arc<Mutex<_>>` so it can be shared between the layer
/// (which collects data) and the caller (which reads the report).
#[derive(Clone)]
pub struct TimingData {
    inner: Arc<Mutex<TimingDataInner>>,
}

/// The actual timing data storage.
struct TimingDataInner {
    /// Accumulated measurements per pass name.
    /// Key is the span name (e.g., "lexer", "parser").
    passes: HashMap<String, PassAggregate>,

    /// Order in which passes were first seen, for deterministic output.
    pass_order: Vec<String>,

    /// Union of intervals in which at least one root span was active.
    ///
    /// Child spans overlap their parents, so summing every pass duration
    /// double-counts work. The active-time union is the timing boundary used
    /// for the benchmark total, even if independent roots overlap.
    root_duration: Duration,
    /// Number of root spans currently entered, including re-entrant entries.
    active_roots: u64,
    /// Start of the current interval in which at least one root is active.
    root_active_since: Option<Instant>,
    /// Most recent timestamp applied to a root-span transition.
    ///
    /// Runtime timestamps are sampled while holding `inner`, so they are
    /// already ordered. Retaining the last timestamp also makes the collector
    /// robust to a stale timestamp and gives the ordering invariant a
    /// deterministic regression test.
    last_root_transition: Option<Instant>,

    /// Direct parent-child span names captured by the real timing layer.
    ///
    /// This remains test-only so parentage regressions can be checked without
    /// expanding the public benchmark JSON schema.
    #[cfg(test)]
    parent_edges: Vec<(String, String)>,
}

/// Measurements aggregated across every span with the same name.
#[derive(Debug, Clone, Copy, Default)]
struct PassAggregate {
    duration: Duration,
    invocations: u64,
    root_invocations: u64,
    leaf_invocations: u64,
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
    /// Metadata about this benchmark run.
    pub metadata: BenchmarkMetadata,
    /// Individual pass timings in milliseconds.
    pub passes: Vec<PassTiming>,
    /// Total compilation time in milliseconds.
    pub total_ms: f64,
    /// Source code metrics (lines, bytes, tokens).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_metrics: Option<SourceMetrics>,
    /// Peak memory usage in bytes (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_memory_bytes: Option<u64>,
}

/// Source code metrics for throughput calculations.
#[derive(Debug, Clone, Serialize)]
pub struct SourceMetrics {
    /// Number of source files compiled.
    pub files: usize,
    /// Total bytes across source files.
    pub bytes: usize,
    /// Total lines across source files.
    pub lines: usize,
    /// Total tokens produced by the lexer invocations consumed by parsing.
    pub tokens: usize,
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

impl TimingData {
    /// Create a new empty timing data collector.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TimingDataInner {
                passes: HashMap::new(),
                pass_order: Vec::new(),
                root_duration: Duration::ZERO,
                active_roots: 0,
                root_active_since: None,
                last_root_transition: None,
                #[cfg(test)]
                parent_edges: Vec::new(),
            })),
        }
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
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = inner.passes.entry(pass.to_string()).or_default();
        entry.duration += duration;
        entry.invocations += 1;
        entry.root_invocations += u64::from(is_root);
        entry.leaf_invocations += u64::from(is_leaf);

        // Track order of first occurrence
        if !inner.pass_order.contains(&pass.to_string()) {
            inner.pass_order.push(pass.to_string());
        }
    }

    /// Record a synthetic span and its non-overlapping root contribution.
    #[cfg(test)]
    fn record_test_span(&self, pass: &str, duration: Duration, is_root: bool, is_leaf: bool) {
        self.record_span(pass, duration, is_root, is_leaf);
        if is_root {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            inner.root_duration += duration;
        }
    }

    /// Record one real direct parent-child relationship for regression tests.
    #[cfg(test)]
    fn record_parent_edge(&self, parent: &str, child: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner
            .parent_edges
            .push((parent.to_owned(), child.to_owned()));
    }

    /// Snapshot parent-child relationships captured by the real timing layer.
    #[cfg(test)]
    pub(crate) fn parent_edges(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .parent_edges
            .clone()
    }

    /// Enter a root span and update both timing views as one serialized event.
    ///
    /// The span's extension lock is held by the caller. Acquiring the global
    /// timing lock before sampling the clock prevents callbacks for different
    /// root spans from applying timestamps out of order.
    fn enter_root_span(&self, timing: &mut SpanTiming) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        Self::enter_root_span_inner(&mut inner, timing, now);
    }

    /// Exit a root span and update both timing views as one serialized event.
    fn exit_root_span(&self, timing: &mut SpanTiming) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        Self::exit_root_span_inner(&mut inner, timing, now);
    }

    fn enter_root_span_inner(inner: &mut TimingDataInner, timing: &mut SpanTiming, now: Instant) {
        let now = ordered_root_timestamp(inner, now);
        timing.enter_at(now);
        Self::root_enter_inner(inner, now);
    }

    fn exit_root_span_inner(inner: &mut TimingDataInner, timing: &mut SpanTiming, now: Instant) {
        let now = ordered_root_timestamp(inner, now);
        timing.exit_at(now);
        Self::root_exit_inner(inner, now);
    }

    fn root_enter_inner(inner: &mut TimingDataInner, now: Instant) {
        if inner.active_roots == 0 {
            inner.root_active_since = Some(now);
        }
        inner.active_roots += 1;
    }

    fn root_exit_inner(inner: &mut TimingDataInner, now: Instant) {
        if inner.active_roots == 0 {
            return;
        }
        inner.active_roots -= 1;
        if inner.active_roots == 0 {
            if let Some(started) = inner.root_active_since.take() {
                inner.root_duration += now.saturating_duration_since(started);
            }
        }
    }

    /// Begin one synthetic root-span active interval.
    #[cfg(test)]
    fn root_enter_at(&self, now: Instant) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let now = ordered_root_timestamp(&mut inner, now);
        Self::root_enter_inner(&mut inner, now);
    }

    /// End one synthetic root-span active interval.
    #[cfg(test)]
    fn root_exit_at(&self, now: Instant) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let now = ordered_root_timestamp(&mut inner, now);
        Self::root_exit_inner(&mut inner, now);
    }

    /// Generate the timing report.
    ///
    /// Returns a formatted string showing each pass's timing and percentage
    /// of total compilation time.
    pub fn report(&self) -> String {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);

        if inner.passes.is_empty() {
            return String::from("No timing data collected (no instrumented passes ran).\n");
        }

        let total_ms = current_root_duration(&inner).as_secs_f64() * 1000.0;

        let mut output = String::new();
        output.push_str("=== Compilation Timing (inclusive spans) ===\n\n");

        // Find the longest pass name for alignment
        let max_name_len = inner.pass_order.iter().map(|s| s.len()).max().unwrap_or(0);

        for pass in &inner.pass_order {
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

        output
    }

    /// Generate structured timing data with optional source metrics and memory usage.
    ///
    /// # Arguments
    /// * `target` - The target platform string (e.g., "x86_64-linux")
    /// * `version` - The compiler version string
    /// * `source_metrics` - Optional source code metrics (bytes, lines, tokens)
    /// * `peak_memory_bytes` - Optional peak memory usage in bytes
    pub fn to_benchmark_timing_with_metrics(
        &self,
        target: &str,
        version: &str,
        source_metrics: Option<SourceMetrics>,
        peak_memory_bytes: Option<u64>,
    ) -> BenchmarkTiming {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);

        let total_ms = current_root_duration(&inner).as_secs_f64() * 1000.0;

        let passes = inner
            .pass_order
            .iter()
            .filter_map(|pass| {
                inner.passes.get(pass).map(|measurement| {
                    let duration_ms = measurement.duration.as_secs_f64() * 1000.0;
                    let percent = if total_ms > 0.0 {
                        (duration_ms / total_ms) * 100.0
                    } else {
                        0.0
                    };
                    PassTiming {
                        name: pass.clone(),
                        duration_ms,
                        percent,
                        invocations: measurement.invocations,
                        root_invocations: measurement.root_invocations,
                        leaf_invocations: measurement.leaf_invocations,
                    }
                })
            })
            .collect();

        let metadata = BenchmarkMetadata {
            timestamp: iso8601_now(),
            version: version.to_string(),
            target: target.to_string(),
        };

        BenchmarkTiming {
            schema_version: 2,
            timing_model: "inclusive_spans",
            metadata,
            passes,
            total_ms,
            source_metrics,
            peak_memory_bytes,
        }
    }

    /// Generate JSON output with additional source metrics.
    ///
    /// # Arguments
    /// * `target` - The target platform string
    /// * `version` - The compiler version string
    /// * `source_metrics` - Source code metrics (bytes, lines, tokens)
    /// * `peak_memory_bytes` - Optional peak memory usage
    pub fn to_json_with_metrics(
        &self,
        target: &str,
        version: &str,
        source_metrics: Option<SourceMetrics>,
        peak_memory_bytes: Option<u64>,
    ) -> String {
        let timing = self.to_benchmark_timing_with_metrics(
            target,
            version,
            source_metrics,
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

/// Clamp a root transition to the serialized root timeline.
fn ordered_root_timestamp(inner: &mut TimingDataInner, now: Instant) -> Instant {
    let now = inner
        .last_root_transition
        .map_or(now, |previous| previous.max(now));
    inner.last_root_transition = Some(now);
    now
}

/// Snapshot the union of intervals in which at least one root span was active.
fn current_root_duration(inner: &TimingDataInner) -> Duration {
    let active = inner
        .root_active_since
        .map_or(Duration::ZERO, |started| started.elapsed());
    inner.root_duration + active
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
/// This layer hooks into the span lifecycle to measure how long each span
/// is active (entered). It stores timing data in a shared `TimingData`
/// that can be queried after compilation completes.
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
    fn on_new_span(&self, _attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // Initialize timing state for this span
        if let Some(span) = ctx.span(id) {
            let parent_id = span.parent().map(|parent| parent.id().clone());
            #[cfg(test)]
            let parent_edge = span
                .parent()
                .map(|parent| (parent.name().to_owned(), span.name().to_owned()));
            let is_root = parent_id.is_none();
            let mut extensions = span.extensions_mut();
            extensions.insert(SpanTiming {
                active_enters: 0,
                active_since: None,
                accumulated: Duration::ZERO,
                has_children: false,
                is_root,
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
                if timing.is_root {
                    self.data.enter_root_span(timing);
                } else {
                    // Sample after acquiring the per-span extension lock so
                    // concurrent callbacks cannot apply stale timestamps.
                    timing.enter_at(Instant::now());
                }
            }
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(timing) = extensions.get_mut::<SpanTiming>() {
                if timing.is_root {
                    self.data.exit_root_span(timing);
                } else {
                    // Sample after acquiring the per-span extension lock so
                    // concurrent callbacks cannot apply stale timestamps.
                    timing.exit_at(Instant::now());
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
                    )
                })
            };

            // Do not hold the span's extension lock while acquiring the
            // aggregate-data lock. Root enter/exit callbacks deliberately use
            // the opposite pair together (extension, then aggregate data).
            if let Some((name, duration, is_root, is_leaf)) = measurement {
                self.data.record_span(&name, duration, is_root, is_leaf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rue_compiler::unstable::update_for_presentation;
    use rue_compiler::{CompileOptions, CompilerSession, SourceSnapshot};
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

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
            direct_data.to_benchmark_timing_with_metrics("test", "test", None, None);
        let direct_parse = direct_timing
            .passes
            .iter()
            .find(|pass| pass.name == "parse_file")
            .unwrap();
        assert_eq!(direct_parse.invocations, 1);
        assert_eq!(direct_parse.root_invocations, 1);

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
            session.semantic(&CompileOptions::default()).unwrap();
        });

        let session_edges = session_data.parent_edges();
        for expected in [
            ("parse_file", "lexer"),
            ("parse_file", "parser"),
            ("semantic_astgen", "definition_snapshot_modules"),
        ] {
            assert!(
                session_edges.contains(&(expected.0.to_owned(), expected.1.to_owned())),
                "missing {expected:?} in compiler-session edges: {session_edges:?}"
            );
        }

        let session_timing =
            session_data.to_benchmark_timing_with_metrics("test", "test", None, None);
        let session_parse_file = session_timing
            .passes
            .iter()
            .find(|pass| pass.name == "parse_file")
            .unwrap();
        let rir_declaration_index = session_timing
            .passes
            .iter()
            .find(|pass| pass.name == "rir_declaration_index")
            .unwrap();
        let sema = session_timing
            .passes
            .iter()
            .find(|pass| pass.name == "sema")
            .unwrap();
        // The session parses the source module once, then lazily reparses the
        // demanded body-free declaration terminal inside the semantic query.
        assert_eq!(session_parse_file.invocations, 2);
        assert_eq!(session_parse_file.root_invocations, 2);
        assert_eq!(rir_declaration_index.invocations, 1);
        assert_eq!(rir_declaration_index.root_invocations, 1);
        assert_eq!(rir_declaration_index.leaf_invocations, 1);
        assert_eq!(sema.invocations, 1);
        assert_eq!(sema.root_invocations, 1);
        assert_eq!(sema.leaf_invocations, 1);
        assert!(
            !session_edges.contains(&("sema".to_owned(), "rir_declaration_index".to_owned())),
            "the index and sema analysis must remain sibling leaves: {session_edges:?}"
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
        for child in ["rir_declaration_index", "sema"] {
            assert!(
                compile_edges.contains(&("compile_pipeline".to_owned(), child.to_owned())),
                "missing compile_pipeline -> {child} in batch edges: {compile_edges:?}"
            );
        }
        assert!(
            !compile_edges.contains(&("sema".to_owned(), "rir_declaration_index".to_owned())),
            "the batch index and sema spans must remain siblings: {compile_edges:?}"
        );

        let compile_timing =
            compile_data.to_benchmark_timing_with_metrics("test", "test", None, None);
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
        for phase in ["rir_declaration_index", "sema"] {
            let timing = compile_timing
                .passes
                .iter()
                .find(|pass| pass.name == phase)
                .unwrap();
            assert_eq!(timing.invocations, 1, "{phase}");
            assert_eq!(timing.root_invocations, 0, "{phase}");
            assert_eq!(timing.leaf_invocations, 1, "{phase}");
        }
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
    fn test_timing_data_order_preserved() {
        let data = TimingData::new();
        data.record("aaa", Duration::from_millis(100));
        data.record("zzz", Duration::from_millis(100));
        data.record("mmm", Duration::from_millis(100));

        let report = data.report();
        let aaa_pos = report.find("Aaa").unwrap();
        let zzz_pos = report.find("Zzz").unwrap();
        let mmm_pos = report.find("Mmm").unwrap();

        // Order should be: aaa, zzz, mmm (insertion order)
        assert!(aaa_pos < zzz_pos);
        assert!(zzz_pos < mmm_pos);
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
        let timing = data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None);
        assert!(timing.passes.is_empty());
        assert_eq!(timing.total_ms, 0.0);
    }

    #[test]
    fn test_to_benchmark_timing_with_data() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));
        data.record("parser", Duration::from_millis(200));

        let timing = data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None);
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

        let timing = data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None);
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

        let timing = data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None);
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

        let inner = data.inner.lock().unwrap();
        assert_eq!(current_root_duration(&inner), Duration::from_millis(30));
    }

    #[test]
    fn stale_root_callback_timestamp_cannot_inflate_either_timing_view() {
        let data = TimingData::new();
        let start = Instant::now();
        let mut first = SpanTiming {
            active_enters: 0,
            active_since: None,
            accumulated: Duration::ZERO,
            has_children: false,
            is_root: true,
        };
        let mut delayed = SpanTiming {
            active_enters: 0,
            active_since: None,
            accumulated: Duration::ZERO,
            has_children: false,
            is_root: true,
        };

        // Model a callback that captured t=10ms, stalled, and was applied
        // after another root exited at t=20ms. Runtime callbacks sample their
        // timestamp only after acquiring this lock; clamping here makes the
        // invariant explicit and protects both the span and root-union views.
        let mut inner = data.inner.lock().unwrap();
        TimingData::enter_root_span_inner(&mut inner, &mut first, start);
        TimingData::exit_root_span_inner(&mut inner, &mut first, start + Duration::from_millis(20));
        TimingData::enter_root_span_inner(
            &mut inner,
            &mut delayed,
            start + Duration::from_millis(10),
        );
        TimingData::exit_root_span_inner(
            &mut inner,
            &mut delayed,
            start + Duration::from_millis(30),
        );

        assert_eq!(first.duration(), Duration::from_millis(20));
        assert_eq!(delayed.duration(), Duration::from_millis(10));
        assert_eq!(current_root_duration(&inner), Duration::from_millis(30));
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

        let timing = data.to_benchmark_timing_with_metrics("aarch64-macos", "0.2.0", None, None);
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

        let json = data.to_json_with_metrics("x86_64-linux", "0.1.0", None, None);
        assert!(json.contains("\"schema_version\":2"));
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
        let json = data.to_json_with_metrics("x86_64-linux", "0.1.0", None, None);
        // Should produce valid JSON even with empty data
        assert!(json.contains("\"passes\":[]"));
        assert!(json.contains("\"total_ms\":0"));
    }

    #[test]
    fn test_benchmark_timing_order_preserved() {
        let data = TimingData::new();
        data.record("aaa", Duration::from_millis(100));
        data.record("zzz", Duration::from_millis(100));
        data.record("mmm", Duration::from_millis(100));

        let timing = data.to_benchmark_timing_with_metrics("x86_64-linux", "0.1.0", None, None);
        assert_eq!(timing.passes[0].name, "aaa");
        assert_eq!(timing.passes[1].name, "zzz");
        assert_eq!(timing.passes[2].name, "mmm");
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
