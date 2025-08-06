//! Unified error type for the Rue compiler
//!
//! This module provides a unified error type that aggregates all the different
//! error types from various compilation phases.

use crate::pipeline::CompileError;
use rue_diagnostic::{Diagnostic, DiagnosticBuilder, SourceId};
use rue_lexer::Span;
use rue_semantic::SemanticError;

/// Unified diagnostic-based error for the Rue compiler
/// All compilation errors are now handled as diagnostics
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RueError {
    /// Single diagnostic error
    #[error("{0}")]
    Diagnostic(Diagnostic),

    /// Multiple diagnostics (e.g., from a phase that can recover and continue)
    #[error("compilation failed with {} errors", .0.len())]
    MultipleErrors(Vec<Diagnostic>),

    /// I/O errors (not diagnostic-related)
    #[error("I/O error: {0}")]
    Io(String),
}

impl RueError {
    /// Create a RueError from a single diagnostic
    pub fn from_diagnostic(diagnostic: Diagnostic) -> Self {
        RueError::Diagnostic(diagnostic)
    }

    /// Create a RueError from multiple diagnostics
    pub fn from_diagnostics(diagnostics: Vec<Diagnostic>) -> Self {
        match diagnostics.len() {
            0 => panic!("Cannot create RueError from empty diagnostics"),
            1 => RueError::Diagnostic(diagnostics.into_iter().next().unwrap()),
            _ => RueError::MultipleErrors(diagnostics),
        }
    }

    /// Get all diagnostics from this error
    pub fn diagnostics(&self) -> Vec<&Diagnostic> {
        match self {
            RueError::Diagnostic(d) => vec![d],
            RueError::MultipleErrors(diagnostics) => diagnostics.iter().collect(),
            RueError::Io(_) => vec![], // I/O errors don't have diagnostics
        }
    }

    /// Get the primary diagnostic (first one if multiple)
    pub fn primary_diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            RueError::Diagnostic(d) => Some(d),
            RueError::MultipleErrors(diagnostics) => diagnostics.first(),
            RueError::Io(_) => None,
        }
    }

    /// Get the span of the primary error if available
    pub fn span(&self) -> Option<Span> {
        self.primary_diagnostic()?.primary_span()
    }

    /// Check if this error represents compilation failure (vs I/O failure)
    pub fn is_compilation_error(&self) -> bool {
        matches!(self, RueError::Diagnostic(_) | RueError::MultipleErrors(_))
    }
}

impl From<std::io::Error> for RueError {
    fn from(error: std::io::Error) -> Self {
        RueError::Io(error.to_string())
    }
}

impl From<Diagnostic> for RueError {
    fn from(diagnostic: Diagnostic) -> Self {
        RueError::from_diagnostic(diagnostic)
    }
}

impl From<Vec<Diagnostic>> for RueError {
    fn from(diagnostics: Vec<Diagnostic>) -> Self {
        RueError::from_diagnostics(diagnostics)
    }
}

impl From<SemanticError> for RueError {
    fn from(error: SemanticError) -> Self {
        let diagnostic = DiagnosticBuilder::error(error.message, SourceId::stdin())
            .with_primary_label(error.span, "error occurred here")
            .build();

        RueError::from_diagnostic(diagnostic)
    }
}

/// Temporary conversion from CompileError until codegen/lowering are migrated
impl From<CompileError> for RueError {
    fn from(error: CompileError) -> Self {
        let diagnostic = DiagnosticBuilder::error(error.to_string(), SourceId::stdin())
            .with_help(
                "This error will provide better diagnostics once codegen/lowering are migrated",
            )
            .build();

        RueError::from_diagnostic(diagnostic)
    }
}
