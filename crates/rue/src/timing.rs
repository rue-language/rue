//! Timing infrastructure for `--time-passes`.
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
//!    timing as a human-readable table.
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
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
}
