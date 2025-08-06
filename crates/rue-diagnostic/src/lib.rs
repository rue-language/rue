//! Rich diagnostic system for the Rue compiler
//!
//! This crate provides a comprehensive diagnostic system with:
//! - Multiple severity levels (error, warning, info, hint)
//! - Multi-span error highlighting
//! - Structured error codes
//! - "Did you mean?" suggestions
//! - LSP integration support

use rue_lexer::Span;
use std::collections::HashMap;
use std::fmt;

pub mod builder;
pub mod collector;
pub mod convert;
pub mod errors;
pub mod formatter;
pub mod recovery;
pub mod suggestions;
pub mod types;
pub mod utils;

pub use builder::DiagnosticBuilder;
pub use collector::DiagnosticCollector;
pub use convert::{
    CompilerPhase, DiagnosticAccumulator, DiagnosticContext, DiagnosticResultExt, IntoDiagnostic,
    IntoDignosticResult,
};
pub use errors::{
    DiagnosticError, DiagnosticResult as ErrorResult, FormatError, FormatResult, SourceError,
    SourceResult,
};
pub use formatter::DiagnosticFormatter;
pub use recovery::{ErrorRecovery, RecoveryResult};
pub use suggestions::SuggestionEngine;
pub use types::{ColumnNumber, DiagnosticMessage, ErrorCode, FilePath, LineNumber, SourcePosition};
pub use utils::{DiagnosticExt, spans, text};

/// Diagnostic severity levels matching LSP and compiler conventions
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Compilation errors that prevent successful compilation
    Error = 1,
    /// Warnings about potential issues
    Warning = 2,
    /// Informational messages
    Information = 3,
    /// Hints and suggestions
    Hint = 4,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Information => write!(f, "info"),
            Severity::Hint => write!(f, "hint"),
        }
    }
}

/// Structured error codes for tooling and documentation
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiagnosticCode {
    /// Namespace (e.g., "E" for errors, "W" for warnings)
    pub namespace: &'static str,
    /// Numeric code (e.g., 0001, 0002)
    pub number: u32,
}

impl DiagnosticCode {
    pub const fn new(namespace: &'static str, number: u32) -> Self {
        Self { namespace, number }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{:04}", self.namespace, self.number)
    }
}

/// Multi-span label for highlighting related code locations
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    /// Source span to highlight
    pub span: Span,
    /// Label message (optional)
    pub message: Option<String>,
    /// Label style for visual distinction
    pub style: LabelStyle,
}

impl Label {
    pub fn primary(span: Span) -> Self {
        Self {
            span,
            message: None,
            style: LabelStyle::Primary,
        }
    }

    pub fn primary_with_message(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: Some(message.into()),
            style: LabelStyle::Primary,
        }
    }

    pub fn secondary(span: Span) -> Self {
        Self {
            span,
            message: None,
            style: LabelStyle::Secondary,
        }
    }

    pub fn secondary_with_message(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: Some(message.into()),
            style: LabelStyle::Secondary,
        }
    }

    pub fn note(span: Span) -> Self {
        Self {
            span,
            message: None,
            style: LabelStyle::Note,
        }
    }

    pub fn note_with_message(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: Some(message.into()),
            style: LabelStyle::Note,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    /// Primary error location (red underline with ^)
    Primary,
    /// Secondary related location (blue underline with -)
    Secondary,
    /// Informational note (green underline with ~)
    Note,
}

/// Main diagnostic structure
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    /// Diagnostic severity
    pub severity: Severity,
    /// Structured error code
    pub code: Option<DiagnosticCode>,
    /// Primary error message
    pub message: String,
    /// Multiple labeled spans
    pub labels: Vec<Label>,
    /// Additional help text or suggestions
    pub help: Option<String>,
    /// Related diagnostics (for complex errors)
    pub related: Vec<Box<Diagnostic>>,
    /// Source file identifier
    pub source_id: SourceId,
}

impl Diagnostic {
    /// Create a new error diagnostic
    pub fn error(message: impl Into<String>, source_id: SourceId) -> DiagnosticBuilder {
        DiagnosticBuilder::error(message, source_id)
    }

    /// Create a new warning diagnostic
    pub fn warning(message: impl Into<String>, source_id: SourceId) -> DiagnosticBuilder {
        DiagnosticBuilder::warning(message, source_id)
    }

    /// Create a new information diagnostic
    pub fn info(message: impl Into<String>, source_id: SourceId) -> DiagnosticBuilder {
        DiagnosticBuilder::info(message, source_id)
    }

    /// Create a new hint diagnostic
    pub fn hint(message: impl Into<String>, source_id: SourceId) -> DiagnosticBuilder {
        DiagnosticBuilder::hint(message, source_id)
    }

    /// Get the primary span if available
    pub fn primary_span(&self) -> Option<Span> {
        self.labels
            .iter()
            .find(|l| l.style == LabelStyle::Primary)
            .map(|l| l.span)
    }

    /// Check if this is an error
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Source file identification for multi-file diagnostics
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId(pub String);

impl SourceId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn stdin() -> Self {
        Self("<stdin>".to_string())
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.severity, self.message)
    }
}

/// Source manager for tracking multiple files
#[derive(Debug, Default)]
pub struct SourceManager {
    sources: HashMap<SourceId, SourceFile>,
}

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub id: SourceId,
    pub name: String,
    pub content: String,
    pub line_starts: Vec<usize>,
}

impl SourceManager {
    /// Create a new empty source manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a source file and return its ID
    pub fn add_source(&mut self, name: impl Into<String>, content: impl Into<String>) -> SourceId {
        let name = name.into();
        let content = content.into();
        let id = SourceId::new(name.clone());
        let line_starts = compute_line_starts(&content);
        let source = SourceFile {
            id: id.clone(),
            name,
            content,
            line_starts,
        };
        self.sources.insert(id.clone(), source);
        id
    }

    /// Get a source file by ID
    pub fn get_source(&self, id: &SourceId) -> Option<&SourceFile> {
        self.sources.get(id)
    }

    /// Convert a byte offset to line and column
    pub fn offset_to_line_col(&self, id: &SourceId, offset: usize) -> Option<(usize, usize)> {
        let source = self.get_source(id)?;
        Some(span_to_line_col(&source.line_starts, offset))
    }

    /// Get a line of source code
    pub fn get_line(&self, id: &SourceId, line_idx: usize) -> Option<&str> {
        let source = self.get_source(id)?;
        get_line_content(&source.content, &source.line_starts, line_idx)
    }
}

/// Compute starting byte positions of each line in the source
pub(crate) fn compute_line_starts(content: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(content.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

/// Convert a byte offset to line and column indices
pub fn span_to_line_col(line_starts: &[usize], offset: usize) -> (usize, usize) {
    let line = line_starts
        .binary_search(&offset)
        .unwrap_or_else(|i| i.saturating_sub(1));
    let col = offset.saturating_sub(line_starts.get(line).copied().unwrap_or(0));
    (line, col)
}

/// Get the content of a specific line
pub fn get_line_content<'a>(
    content: &'a str,
    line_starts: &[usize],
    line_idx: usize,
) -> Option<&'a str> {
    let start = *line_starts.get(line_idx)?;
    let end = line_starts
        .get(line_idx + 1)
        .copied()
        .unwrap_or(content.len());
    Some(content[start..end].trim_end_matches('\n'))
}

// Common error codes
pub const E1001: DiagnosticCode = DiagnosticCode::new("E", 1001); // Unexpected token
pub const E1002: DiagnosticCode = DiagnosticCode::new("E", 1002); // Missing semicolon
pub const E1003: DiagnosticCode = DiagnosticCode::new("E", 1003); // Unclosed delimiter
pub const E2001: DiagnosticCode = DiagnosticCode::new("E", 2001); // Undefined identifier
pub const E2002: DiagnosticCode = DiagnosticCode::new("E", 2002); // Duplicate definition
pub const E3001: DiagnosticCode = DiagnosticCode::new("E", 3001); // Type mismatch
pub const E3002: DiagnosticCode = DiagnosticCode::new("E", 3002); // Arity mismatch
pub const E3003: DiagnosticCode = DiagnosticCode::new("E", 3003); // Invalid cast

pub const W1001: DiagnosticCode = DiagnosticCode::new("W", 1001); // Unused variable
pub const W1002: DiagnosticCode = DiagnosticCode::new("W", 1002); // Dead code
pub const W1003: DiagnosticCode = DiagnosticCode::new("W", 1003); // Unreachable code
