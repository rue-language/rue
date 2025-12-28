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
//! 1. **Instrumentation** (Phase 4): Compiler passes are wrapped in tracing spans
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
use std::sync::{Arc, Mutex};
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
    /// Accumulated duration per pass name.
    /// Key is the span name (e.g., "lexer", "parser").
    passes: HashMap<String, Duration>,

    /// Order in which passes were first seen, for deterministic output.
    pass_order: Vec<String>,
}

/// JSON output structure for benchmark timing data.
///
/// This structure is designed for machine-readable output that can be
/// consumed by the benchmark runner and visualization tools. It includes
/// metadata for historical analysis and comparison across runs.
#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkTiming {
    /// Metadata about this benchmark run.
    pub metadata: BenchmarkMetadata,
    /// Individual pass timings in milliseconds.
    pub passes: Vec<PassTiming>,
    /// Total compilation time in milliseconds.
    pub total_ms: f64,
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
}

impl TimingData {
    /// Create a new empty timing data collector.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TimingDataInner {
                passes: HashMap::new(),
                pass_order: Vec::new(),
            })),
        }
    }

    /// Record a duration for the given pass.
    fn record(&self, pass: &str, duration: Duration) {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner
            .passes
            .entry(pass.to_string())
            .or_insert(Duration::ZERO);
        *entry += duration;

        // Track order of first occurrence
        if !inner.pass_order.contains(&pass.to_string()) {
            inner.pass_order.push(pass.to_string());
        }
    }

    /// Generate the timing report.
    ///
    /// Returns a formatted string showing each pass's timing and percentage
    /// of total compilation time.
    pub fn report(&self) -> String {
        let inner = self.inner.lock().unwrap();

        if inner.passes.is_empty() {
            return String::from("No timing data collected (no instrumented passes ran).\n");
        }

        let total: Duration = inner.passes.values().sum();
        let total_ms = total.as_secs_f64() * 1000.0;

        let mut output = String::new();
        output.push_str("=== Compilation Timing ===\n\n");

        // Find the longest pass name for alignment
        let max_name_len = inner.pass_order.iter().map(|s| s.len()).max().unwrap_or(0);

        for pass in &inner.pass_order {
            if let Some(&duration) = inner.passes.get(pass) {
                let ms = duration.as_secs_f64() * 1000.0;
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
            "Total:",
            total_ms,
            width = max_name_len + 1
        ));

        output
    }

    /// Generate structured timing data for JSON serialization.
    ///
    /// Returns a `BenchmarkTiming` struct that can be serialized to JSON
    /// for machine-readable output. This is used by `--benchmark-json` for
    /// the performance dashboard.
    ///
    /// # Arguments
    /// * `target` - The target platform string (e.g., "x86_64-linux")
    /// * `version` - The compiler version string
    pub fn to_benchmark_timing(&self, target: &str, version: &str) -> BenchmarkTiming {
        let inner = self.inner.lock().unwrap();

        let total: Duration = inner.passes.values().sum();
        let total_ms = total.as_secs_f64() * 1000.0;

        let passes = inner
            .pass_order
            .iter()
            .filter_map(|pass| {
                inner.passes.get(pass).map(|&duration| {
                    let duration_ms = duration.as_secs_f64() * 1000.0;
                    let percent = if total_ms > 0.0 {
                        (duration_ms / total_ms) * 100.0
                    } else {
                        0.0
                    };
                    PassTiming {
                        name: pass.clone(),
                        duration_ms,
                        percent,
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
            metadata,
            passes,
            total_ms,
        }
    }

    /// Generate JSON output for benchmark timing.
    ///
    /// Returns a JSON string suitable for machine parsing. The format includes
    /// metadata for historical analysis:
    /// ```json
    /// {
    ///   "metadata": {
    ///     "timestamp": "2025-12-27T21:30:00Z",
    ///     "version": "0.1.0",
    ///     "target": "aarch64-macos"
    ///   },
    ///   "passes": [
    ///     {"name": "lexer", "duration_ms": 0.5, "percent": 10.0},
    ///     {"name": "parser", "duration_ms": 2.3, "percent": 46.0},
    ///     ...
    ///   ],
    ///   "total_ms": 5.0
    /// }
    /// ```
    ///
    /// # Arguments
    /// * `target` - The target platform string (e.g., "x86_64-linux")
    /// * `version` - The compiler version string
    pub fn to_json(&self, target: &str, version: &str) -> String {
        let timing = self.to_benchmark_timing(target, version);
        serde_json::to_string(&timing).unwrap_or_else(|_| "{}".to_string())
    }

    /// Generate pretty-printed JSON output for benchmark timing.
    ///
    /// Same as `to_json()` but with indentation for human readability.
    pub fn to_json_pretty(&self, target: &str, version: &str) -> String {
        let timing = self.to_benchmark_timing(target, version);
        serde_json::to_string_pretty(&timing).unwrap_or_else(|_| "{}".to_string())
    }
}

impl Default for TimingData {
    fn default() -> Self {
        Self::new()
    }
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
    /// When the span was most recently entered.
    /// None if the span is not currently entered.
    entered_at: Option<Instant>,
    /// Total time accumulated across all enter/exit cycles.
    accumulated: Duration,
}

impl<S> Layer<S> for TimingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, _attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // Initialize timing state for this span
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            extensions.insert(SpanTiming {
                entered_at: None,
                accumulated: Duration::ZERO,
            });
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        // Record when the span was entered
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(timing) = extensions.get_mut::<SpanTiming>() {
                timing.entered_at = Some(Instant::now());
            }
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        // Calculate duration and accumulate
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(timing) = extensions.get_mut::<SpanTiming>() {
                if let Some(entered_at) = timing.entered_at.take() {
                    timing.accumulated += entered_at.elapsed();
                }
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        // When the span is fully closed, record its total time
        if let Some(span) = ctx.span(&id) {
            let extensions = span.extensions();
            if let Some(timing) = extensions.get::<SpanTiming>() {
                let name = span.name();
                self.data.record(name, timing.accumulated);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let timing = data.to_benchmark_timing("x86_64-linux", "0.1.0");
        assert!(timing.passes.is_empty());
        assert_eq!(timing.total_ms, 0.0);
    }

    #[test]
    fn test_to_benchmark_timing_with_data() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));
        data.record("parser", Duration::from_millis(200));

        let timing = data.to_benchmark_timing("x86_64-linux", "0.1.0");
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

        let timing = data.to_benchmark_timing("x86_64-linux", "0.1.0");
        // lexer should be 25%, parser should be 75%
        assert!((timing.passes[0].percent - 25.0).abs() < 0.1);
        assert!((timing.passes[1].percent - 75.0).abs() < 0.1);
    }

    #[test]
    fn test_to_benchmark_timing_metadata() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));

        let timing = data.to_benchmark_timing("aarch64-macos", "0.2.0");
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

        let json = data.to_json("x86_64-linux", "0.1.0");
        assert!(json.contains("\"passes\""));
        assert!(json.contains("\"name\""));
        assert!(json.contains("\"lexer\""));
        assert!(json.contains("\"duration_ms\""));
        assert!(json.contains("\"percent\""));
        assert!(json.contains("\"total_ms\""));
        // Should also contain metadata
        assert!(json.contains("\"metadata\""));
        assert!(json.contains("\"timestamp\""));
        assert!(json.contains("\"version\""));
        assert!(json.contains("\"target\""));
    }

    #[test]
    fn test_to_json_pretty() {
        let data = TimingData::new();
        data.record("lexer", Duration::from_millis(100));

        let json = data.to_json_pretty("x86_64-linux", "0.1.0");
        // Pretty JSON should have newlines
        assert!(json.contains('\n'));
        // And should still contain the data
        assert!(json.contains("lexer"));
    }

    #[test]
    fn test_to_json_empty() {
        let data = TimingData::new();
        let json = data.to_json("x86_64-linux", "0.1.0");
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

        let timing = data.to_benchmark_timing("x86_64-linux", "0.1.0");
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
