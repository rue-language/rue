//! Idiomatic error types for the diagnostic system

use crate::{Diagnostic, SourceId};
use rue_lexer::Span;
use thiserror::Error;

/// Main error type for diagnostic operations
#[derive(Error, Debug)]
pub enum DiagnosticError {
    #[error("Source file not found: {path}")]
    SourceNotFound { path: String },

    #[error("Invalid span: {span:?} in source {source_id}")]
    InvalidSpan { span: Span, source_id: SourceId },

    #[error("Formatter error: {message}")]
    FormatterError { message: String },

    #[error("Recovery failed: maximum attempts exceeded")]
    RecoveryFailed,

    #[error("Invalid diagnostic code: {code}")]
    InvalidCode { code: String },

    #[error("I/O error: {0}")]
    Io(std::io::Error),
}

impl DiagnosticError {
    /// Convert this error into a diagnostic for error reporting
    pub fn into_diagnostic(self, source_id: SourceId) -> Diagnostic {
        use crate::DiagnosticBuilder;

        match self {
            DiagnosticError::SourceNotFound { path } => {
                DiagnosticBuilder::error(format!("Source file not found: {path}"), source_id)
                    .with_help("Check that the file path is correct and the file exists")
                    .build()
            }
            DiagnosticError::InvalidSpan { span, .. } => {
                DiagnosticBuilder::error("Invalid source span", source_id)
                    .with_primary_span(span)
                    .with_help("This span is outside the bounds of the source file")
                    .build()
            }
            DiagnosticError::FormatterError { message } => {
                DiagnosticBuilder::error(format!("Formatter error: {message}"), source_id).build()
            }
            DiagnosticError::RecoveryFailed => DiagnosticBuilder::error(
                "Error recovery failed",
                source_id,
            )
            .with_help(
                "Too many parse errors encountered. Please fix the most obvious errors first",
            )
            .build(),
            DiagnosticError::InvalidCode { code } => {
                DiagnosticBuilder::error(format!("Invalid diagnostic code: {code}"), source_id)
                    .build()
            }
            DiagnosticError::Io(io_error) => {
                DiagnosticBuilder::error(format!("I/O error: {io_error}"), source_id).build()
            }
        }
    }
}

impl From<std::io::Error> for DiagnosticError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Result type for diagnostic operations
pub type DiagnosticResult<T> = Result<T, DiagnosticError>;

/// Specialized error for source manager operations
#[derive(Error, Debug, Clone)]
pub enum SourceError {
    #[error("Source file '{id}' not found")]
    NotFound { id: SourceId },

    #[error("Duplicate source file: {id}")]
    Duplicate { id: SourceId },

    #[error("Invalid line number {line} for source {id} (max: {max_line})")]
    InvalidLine {
        id: SourceId,
        line: usize,
        max_line: usize,
    },

    #[error("Invalid offset {offset} for source {id} (length: {length})")]
    InvalidOffset {
        id: SourceId,
        offset: usize,
        length: usize,
    },
}

/// Result type for source manager operations
pub type SourceResult<T> = Result<T, SourceError>;

/// Error type for diagnostic formatting
#[derive(Error, Debug, Clone)]
pub enum FormatError {
    #[error("Unknown format style: {style}")]
    UnknownStyle { style: String },

    #[error("Missing source content for diagnostic")]
    MissingSource,

    #[error("Color support not available")]
    NoColorSupport,

    #[error("Template error: {message}")]
    Template { message: String },
}

/// Result type for formatting operations
pub type FormatResult<T> = Result<T, FormatError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostic_error_conversion() {
        let source_id = SourceId::new("test.rue");
        let error = DiagnosticError::SourceNotFound {
            path: "nonexistent.rue".to_string(),
        };

        let diagnostic = error.into_diagnostic(source_id);
        assert!(diagnostic.message.contains("Source file not found"));
        assert!(diagnostic.help.is_some());
    }

    #[test]
    fn test_source_error_formatting() {
        let error = SourceError::NotFound {
            id: SourceId::new("test.rue"),
        };

        assert_eq!(error.to_string(), "Source file 'test.rue' not found");
    }

    #[test]
    fn test_format_error() {
        let error = FormatError::UnknownStyle {
            style: "rainbow".to_string(),
        };

        assert_eq!(error.to_string(), "Unknown format style: rainbow");
    }
}
