//! Diagnostic formatting for compiler errors and warnings.
//!
//! This module provides utilities for formatting compiler diagnostics into
//! human-readable output using annotate-snippets for source annotations.
//!
//! # Example
//!
//! ```ignore
//! use rue_compiler::{DiagnosticFormatter, SourceInfo};
//!
//! let source_info = SourceInfo::new(&source, "example.rue");
//! let formatter = DiagnosticFormatter::new(&source_info);
//!
//! // Format an error
//! let error_output = formatter.format_error(&error);
//! eprintln!("{}", error_output);
//!
//! // Format warnings
//! let warning_output = formatter.format_warnings(&warnings);
//! eprintln!("{}", warning_output);
//! ```

use std::collections::HashMap;

use annotate_snippets::{Level, Renderer, Snippet};

use crate::{CompileError, CompileErrors, CompileWarning, Diagnostic, Span};

/// Source code information for diagnostic rendering.
///
/// Contains the source text and file path needed for rendering annotated
/// error and warning messages.
#[derive(Debug, Clone)]
pub struct SourceInfo<'a> {
    /// The source code text.
    pub source: &'a str,
    /// The path to the source file (for display in diagnostics).
    pub path: &'a str,
}

impl<'a> SourceInfo<'a> {
    /// Create a new SourceInfo with the given source and file path.
    pub fn new(source: &'a str, path: &'a str) -> Self {
        Self { source, path }
    }
}

/// Formatter for compiler diagnostics.
///
/// Provides methods to format compilation errors and warnings into
/// human-readable strings with annotated source snippets.
pub struct DiagnosticFormatter<'a> {
    source_info: &'a SourceInfo<'a>,
    renderer: Renderer,
}

impl<'a> DiagnosticFormatter<'a> {
    /// Create a new diagnostic formatter for the given source info.
    pub fn new(source_info: &'a SourceInfo<'a>) -> Self {
        Self {
            source_info,
            renderer: Renderer::plain(),
        }
    }

    /// Format a compilation error into a string.
    pub fn format_error(&self, error: &CompileError) -> String {
        self.format_diagnostic_impl(
            Level::Error,
            &error.to_string(),
            error.span(),
            error.diagnostic(),
        )
    }

    /// Format multiple compilation errors into a string.
    ///
    /// Each error is formatted on its own line(s). A summary line is added at
    /// the end if there are multiple errors showing the total count.
    pub fn format_errors(&self, errors: &CompileErrors) -> String {
        if errors.is_empty() {
            return String::new();
        }

        let mut output = String::new();
        for error in errors.iter() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&self.format_error(error));
        }

        // Add summary line if multiple errors
        if errors.len() > 1 {
            output.push_str(&format!(
                "\nerror: aborting due to {} previous errors\n",
                errors.len()
            ));
        }

        output
    }

    /// Format all warnings, adding line numbers when multiple variables share the same name.
    ///
    /// This improves error messages by disambiguating when there are multiple unused
    /// variables with the same name (e.g., shadowed variables in different scopes).
    pub fn format_warnings(&self, warnings: &[CompileWarning]) -> String {
        if warnings.is_empty() {
            return String::new();
        }

        // Count occurrences of each unused variable name
        let mut var_name_counts: HashMap<&str, usize> = HashMap::new();
        for warning in warnings {
            if let Some(name) = warning.kind.unused_variable_name() {
                *var_name_counts.entry(name).or_insert(0) += 1;
            }
        }

        // Format each warning, adding line number if there are duplicates
        let mut output = String::new();
        for warning in warnings {
            let needs_line_number = warning
                .kind
                .unused_variable_name()
                .is_some_and(|name| var_name_counts.get(name).copied().unwrap_or(0) > 1);

            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&self.format_warning_internal(warning, needs_line_number));
        }
        output
    }

    /// Format a single warning into a string.
    pub fn format_warning(&self, warning: &CompileWarning) -> String {
        self.format_warning_internal(warning, false)
    }

    fn format_warning_internal(
        &self,
        warning: &CompileWarning,
        include_line_number: bool,
    ) -> String {
        // Get the message, optionally with line number for disambiguation
        let message = if include_line_number {
            if let Some(span) = warning.span() {
                let line = span.line_number(self.source_info.source);
                warning.kind.format_with_line(Some(line))
            } else {
                warning.to_string()
            }
        } else {
            warning.to_string()
        };

        self.format_diagnostic_impl(
            Level::Warning,
            &message,
            warning.span(),
            warning.diagnostic(),
        )
    }

    /// Internal implementation for formatting diagnostics.
    fn format_diagnostic_impl(
        &self,
        level: Level,
        message: &str,
        span: Option<Span>,
        diagnostic: &Diagnostic,
    ) -> String {
        // For diagnostics without a span, just format the message with any footers
        let Some(span) = span else {
            let mut report = level.title(message);
            // Add notes and helps as footers
            for note in &diagnostic.notes {
                report = report.footer(Level::Note.title(note.0.as_str()));
            }
            for help in &diagnostic.helps {
                report = report.footer(Level::Help.title(help.0.as_str()));
            }
            return format!("{}", self.renderer.render(report));
        };

        // Build snippet with primary annotation
        let mut snippet = Snippet::source(self.source_info.source)
            .origin(self.source_info.path)
            .fold(true)
            .annotation(level.span(span.start as usize..span.end as usize));

        // Add secondary labels as Info annotations
        for label in &diagnostic.labels {
            snippet = snippet.annotation(
                Level::Info
                    .span(label.span.start as usize..label.span.end as usize)
                    .label(&label.message),
            );
        }

        let mut report = level.title(message).snippet(snippet);

        // Add notes and helps as footers
        for note in &diagnostic.notes {
            report = report.footer(Level::Note.title(note.0.as_str()));
        }
        for help in &diagnostic.helps {
            report = report.footer(Level::Help.title(help.0.as_str()));
        }

        format!("{}", self.renderer.render(report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorKind, WarningKind};

    #[test]
    fn test_format_error_with_span() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let output = formatter.format_error(&error);
        assert!(output.contains("type mismatch"));
        assert!(output.contains("expected i32"));
        assert!(output.contains("found bool"));
        assert!(output.contains("test.rue"));
    }

    #[test]
    fn test_format_error_without_span() {
        let source = "fn foo() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::without_span(ErrorKind::NoMainFunction);

        let output = formatter.format_error(&error);
        assert!(output.contains("no main function"));
    }

    #[test]
    fn test_format_warning() {
        let source = "fn main() -> i32 { let x = 42; 0 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let warning = CompileWarning::new(
            WarningKind::UnusedVariable("x".to_string()),
            Span::new(23, 24),
        );

        let output = formatter.format_warning(&warning);
        assert!(output.contains("unused variable"));
        assert!(output.contains("'x'"));
    }

    #[test]
    fn test_format_warnings_with_duplicates() {
        let source = "fn main() -> i32 {\n    let x = 1;\n    let x = 2;\n    0\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let warnings = vec![
            CompileWarning::new(
                WarningKind::UnusedVariable("x".to_string()),
                Span::new(23, 24),
            ),
            CompileWarning::new(
                WarningKind::UnusedVariable("x".to_string()),
                Span::new(36, 37),
            ),
        ];

        let output = formatter.format_warnings(&warnings);
        // Should include line numbers for disambiguation
        assert!(output.contains("line"));
    }

    #[test]
    fn test_format_warnings_empty() {
        let source = "fn main() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let output = formatter.format_warnings(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn test_format_error_with_help() {
        let source = "fn main() -> i32 { x = 1; 0 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::AssignToImmutable("x".to_string()),
            Span::new(19, 20),
        )
        .with_help("consider making `x` mutable: `let mut x`");

        let output = formatter.format_error(&error);
        assert!(output.contains("help"));
        assert!(output.contains("mutable"));
    }

    #[test]
    fn test_format_error_with_label() {
        let source = "fn main() -> i32 { if true { 1 } else { false } }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(40, 45),
        )
        .with_label("then branch is here", Span::new(29, 30));

        let output = formatter.format_error(&error);
        assert!(output.contains("then branch"));
    }

    #[test]
    fn test_format_errors_empty() {
        let source = "fn main() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let errors = CompileErrors::new();
        let output = formatter.format_errors(&errors);
        assert!(output.is_empty());
    }

    #[test]
    fn test_format_errors_single() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        ));

        let output = formatter.format_errors(&errors);
        assert!(output.contains("type mismatch"));
        // Single error should not have summary line
        assert!(!output.contains("aborting"));
    }

    #[test]
    fn test_format_errors_multiple() {
        let source = "fn main() -> i32 {\n    let x = 1 + true;\n    let y = false - 1;\n    0\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(32, 36),
        ));
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "bool".to_string(),
                found: "i32".to_string(),
            },
            Span::new(58, 59),
        ));

        let output = formatter.format_errors(&errors);
        // Should contain both errors
        assert!(output.contains("expected i32, found bool"));
        assert!(output.contains("expected bool, found i32"));
        // Should have summary line
        assert!(output.contains("aborting due to 2 previous errors"));
    }
}
