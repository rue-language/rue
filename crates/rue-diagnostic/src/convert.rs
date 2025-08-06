//! Trait for converting errors to diagnostics
//!
//! This module provides the core trait for unifying all error types
//! in the Rue compiler into the diagnostic system.

use crate::{Diagnostic, SourceId};
use rue_lexer::Span;

/// Trait for converting any error type to a diagnostic
pub trait IntoDiagnostic {
    /// Convert this error into a diagnostic
    fn into_diagnostic(self, source_id: SourceId) -> Diagnostic;

    /// Convert this error into a diagnostic with additional context
    fn into_diagnostic_with_context(
        self,
        source_id: SourceId,
        _context: DiagnosticContext,
    ) -> Diagnostic
    where
        Self: Sized,
    {
        // Default implementation just ignores context
        self.into_diagnostic(source_id)
    }
}

/// Additional context for enriching diagnostics
#[derive(Debug, Clone)]
pub struct DiagnosticContext {
    pub phase: CompilerPhase,
    pub file_path: Option<String>,
    pub related_spans: Vec<(Span, String)>,
}

#[derive(Debug, Clone, Copy)]
pub enum CompilerPhase {
    Lexing,
    Parsing,
    SemanticAnalysis,
    TypeChecking,
    Lowering,
    Optimization,
    CodeGeneration,
}

impl std::fmt::Display for CompilerPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompilerPhase::Lexing => write!(f, "lexical analysis"),
            CompilerPhase::Parsing => write!(f, "parsing"),
            CompilerPhase::SemanticAnalysis => write!(f, "semantic analysis"),
            CompilerPhase::TypeChecking => write!(f, "type checking"),
            CompilerPhase::Lowering => write!(f, "lowering"),
            CompilerPhase::Optimization => write!(f, "optimization"),
            CompilerPhase::CodeGeneration => write!(f, "code generation"),
        }
    }
}

/// Extension trait for Result types
pub trait DiagnosticResultExt<T> {
    /// Convert an error result into a diagnostic result
    fn with_diagnostic(self, source_id: SourceId) -> Result<T, Box<Diagnostic>>;

    /// Map an error to a diagnostic with custom logic
    fn map_diagnostic<F>(self, f: F) -> Result<T, Box<Diagnostic>>
    where
        F: FnOnce() -> Diagnostic;
}

impl<T, E> DiagnosticResultExt<T> for Result<T, E>
where
    E: IntoDiagnostic,
{
    fn with_diagnostic(self, source_id: SourceId) -> Result<T, Box<Diagnostic>> {
        self.map_err(|e| Box::new(e.into_diagnostic(source_id)))
    }

    fn map_diagnostic<F>(self, f: F) -> Result<T, Box<Diagnostic>>
    where
        F: FnOnce() -> Diagnostic,
    {
        self.map_err(|_| Box::new(f()))
    }
}

/// Result type that can accumulate diagnostics while still producing a value
#[derive(Debug)]
pub struct DiagnosticAccumulator<T> {
    pub value: Option<T>,
    pub diagnostics: Vec<Diagnostic>,
}

impl<T> DiagnosticAccumulator<T> {
    /// Create a successful result with no diagnostics
    pub fn ok(value: T) -> Self {
        Self {
            value: Some(value),
            diagnostics: Vec::new(),
        }
    }

    /// Create a result with a value and diagnostics (partial success)
    pub fn with_diagnostics(value: T, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            value: Some(value),
            diagnostics,
        }
    }

    /// Create an error result with diagnostics
    pub fn err(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            value: None,
            diagnostics,
        }
    }

    /// Check if this result has errors
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }

    /// Convert to a standard Result, failing if there are errors
    pub fn into_result(self) -> Result<T, Vec<Diagnostic>> {
        match (self.has_errors(), self.value) {
            (true, _) | (false, None) => Err(self.diagnostics),
            (false, Some(value)) => Ok(value),
        }
    }

    /// Map the value if present
    pub fn map<U, F>(self, f: F) -> DiagnosticAccumulator<U>
    where
        F: FnOnce(T) -> U,
    {
        DiagnosticAccumulator {
            value: self.value.map(f),
            diagnostics: self.diagnostics,
        }
    }

    /// Chain another diagnostic-producing operation
    pub fn and_then<U, F>(self, f: F) -> DiagnosticAccumulator<U>
    where
        F: FnOnce(T) -> DiagnosticAccumulator<U>,
    {
        match self.value {
            Some(value) => {
                let mut result = f(value);
                // Combine diagnostics by extending the new result with existing ones
                result.diagnostics.splice(0..0, self.diagnostics);
                result
            }
            None => DiagnosticAccumulator {
                value: None,
                diagnostics: self.diagnostics,
            },
        }
    }
}

// Note: Generic implementation for std::error::Error would require specialization
// which is not stable yet. Instead, we'll implement for specific error types.

// Removed io::Error implementation due to Clone constraint issues

/// Helper trait for converting Results to DiagnosticAccumulator
pub trait IntoDignosticResult<T> {
    fn into_diagnostic_accumulator(self) -> DiagnosticAccumulator<T>;
    fn into_diagnostic_accumulator_with_context(
        self,
        context: DiagnosticContext,
    ) -> DiagnosticAccumulator<T>;
}

impl<T, E> IntoDignosticResult<T> for Result<T, E>
where
    E: std::fmt::Display,
{
    fn into_diagnostic_accumulator(self) -> DiagnosticAccumulator<T> {
        match self {
            Ok(value) => DiagnosticAccumulator::ok(value),
            Err(e) => DiagnosticAccumulator::err(vec![Diagnostic {
                severity: crate::Severity::Error,
                code: None,
                message: e.to_string(),
                labels: vec![],
                help: None,
                related: vec![],
                source_id: crate::SourceId::stdin(),
            }]),
        }
    }

    fn into_diagnostic_accumulator_with_context(
        self,
        _context: DiagnosticContext,
    ) -> DiagnosticAccumulator<T> {
        // For now, just delegate to the simpler implementation
        // TODO: Use context to enrich the diagnostic
        self.into_diagnostic_accumulator()
    }
}
