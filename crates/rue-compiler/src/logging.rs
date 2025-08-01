//! Logging infrastructure for the Rue compiler
//!
//! This module provides structured logging using the `tracing` crate.
//!
//! ## Log Levels
//! - ERROR: Compilation failures, invalid IR, verification failures
//! - WARN: Deprecations, non-optimal code patterns, recoverable issues
//! - INFO: Major phase transitions (parsing → HIR → MIR → codegen)
//! - DEBUG: Detailed compiler state (variable lookups, block transitions)
//! - TRACE: Instruction-level details, register allocation steps
//!
//! ## Targets
//! Log filtering can be controlled by target:
//! - `rue::lexer` - Token generation
//! - `rue::parser` - AST construction  
//! - `rue::semantic` - Type checking, HIR building
//! - `rue::mir` - MIR generation and verification
//! - `rue::optimize` - Optimization passes (const prop, DCE, CSE)
//! - `rue::codegen` - Instruction generation, register allocation
//! - `rue::elf` - Binary generation
//!
//! ## Usage
//! Set the RUST_LOG environment variable to control logging:
//! - `RUST_LOG=rue=debug` - Enable debug logging for all rue modules
//! - `RUST_LOG=rue::mir=trace` - Enable trace logging for MIR
//! - `RUST_LOG=rue::mir::vars=trace` - Very detailed variable tracking

use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

/// Output format for logs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable format with colors (default for terminals)
    Pretty,
    /// JSON format for machine processing
    Json,
    /// Compact single-line format
    Compact,
    /// Hierarchical tree format (best for debugging compiler phases)
    Tree,
}

/// Logging configuration
pub struct LogConfig {
    /// Output format
    pub format: LogFormat,
    /// Custom filter string (overrides default)
    pub filter: Option<String>,
    /// Whether to include source code locations
    pub with_source_location: bool,
    /// Whether to include thread IDs
    pub with_thread_ids: bool,
    /// Whether to include timestamps
    pub with_timestamps: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            format: if atty::is(atty::Stream::Stderr) {
                LogFormat::Pretty
            } else {
                LogFormat::Compact
            },
            filter: None,
            with_source_location: false,
            with_thread_ids: false,
            with_timestamps: true,
        }
    }
}

/// Initialize the global tracing subscriber
///
/// This should be called once at the start of the program.
/// Subsequent calls will be ignored.
pub fn init_tracing(config: LogConfig) -> Result<(), Box<dyn std::error::Error>> {
    // Create the env filter
    let filter = config.filter.as_deref().unwrap_or("rue=info");
    let env_filter = EnvFilter::try_new(filter)?;

    match config.format {
        LogFormat::Pretty => {
            let fmt_layer = tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(config.with_thread_ids)
                .with_file(config.with_source_location)
                .with_line_number(config.with_source_location)
                .with_ansi(atty::is(atty::Stream::Stderr))
                .pretty()
                .with_filter(env_filter);

            tracing_subscriber::registry().with(fmt_layer).try_init()?;
        }
        LogFormat::Json => {
            let fmt_layer = tracing_subscriber::fmt::layer()
                .json()
                .with_current_span(true)
                .with_span_list(true)
                .with_target(true)
                .with_file(config.with_source_location)
                .with_line_number(config.with_source_location)
                .with_thread_ids(config.with_thread_ids)
                .with_filter(env_filter);

            tracing_subscriber::registry().with(fmt_layer).try_init()?;
        }
        LogFormat::Compact => {
            let fmt_layer = tracing_subscriber::fmt::layer()
                .compact()
                .with_target(true)
                .with_thread_ids(config.with_thread_ids)
                .with_file(config.with_source_location)
                .with_line_number(config.with_source_location)
                .with_ansi(atty::is(atty::Stream::Stderr))
                .with_filter(env_filter);

            tracing_subscriber::registry().with(fmt_layer).try_init()?;
        }
        LogFormat::Tree => {
            // Use tracing-tree for hierarchical output
            let tree_layer = tracing_tree::HierarchicalLayer::new(2)
                .with_targets(true)
                .with_bracketed_fields(true)
                .with_thread_ids(config.with_thread_ids)
                .with_verbose_exit(false)
                .with_verbose_entry(false)
                .with_indent_amount(2)
                .with_ansi(atty::is(atty::Stream::Stderr))
                .with_filter(env_filter);

            tracing_subscriber::registry().with(tree_layer).try_init()?;
        }
    }

    Ok(())
}

/// Initialize tracing with default configuration
///
/// This is a convenience function that uses sensible defaults.
pub fn init_default() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing(LogConfig::default())
}

/// Parse verbosity level into a RUST_LOG filter string
///
/// - 0 (default): "rue=warn"
/// - 1 (-v): "rue=info"  
/// - 2 (-vv): "rue=debug"
/// - 3+ (-vvv): "rue=trace"
pub fn verbosity_to_filter(verbosity: u8) -> &'static str {
    match verbosity {
        0 => "rue=warn",
        1 => "rue=info",
        2 => "rue=debug",
        _ => "rue=trace",
    }
}
