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
use std::io::IsTerminal;

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

/// Color choice for diagnostic output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Automatically detect whether to use colors based on terminal capabilities.
    #[default]
    Auto,
    /// Always use colors.
    Always,
    /// Never use colors.
    Never,
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
    ///
    /// By default, uses automatic color detection based on whether stderr is a terminal.
    pub fn new(source_info: &'a SourceInfo<'a>) -> Self {
        Self::with_color_choice(source_info, ColorChoice::Auto)
    }

    /// Create a new diagnostic formatter with explicit color choice.
    pub fn with_color_choice(source_info: &'a SourceInfo<'a>, color_choice: ColorChoice) -> Self {
        let use_color = match color_choice {
            ColorChoice::Auto => std::io::stderr().is_terminal(),
            ColorChoice::Always => true,
            ColorChoice::Never => false,
        };
        let renderer = if use_color {
            Renderer::styled()
        } else {
            Renderer::plain()
        };
        Self {
            source_info,
            renderer,
        }
    }

    /// Format a compilation error into a string.
    ///
    /// The error is formatted with its error code, e.g.:
    /// `error[E0206]: type mismatch: expected i32, found bool`
    pub fn format_error(&self, error: &CompileError) -> String {
        // Format with error code: error[E0XXX]: message
        let message_with_code = format!("[{}]: {}", error.kind.code(), error.kind);
        self.format_diagnostic_impl(
            Level::Error,
            &message_with_code,
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

// ============================================================================
// JSON Diagnostic Output
// ============================================================================

use crate::{Applicability, Suggestion};

/// A diagnostic formatted for JSON output.
///
/// This structure is designed to be compatible with common editor protocols
/// (LSP, cargo's JSON format) while containing all information needed for
/// rich diagnostic display.
#[derive(Debug)]
pub struct JsonDiagnostic {
    /// Error or warning code (e.g., "E0206").
    pub code: String,
    /// The diagnostic message.
    pub message: String,
    /// Severity level: "error" or "warning".
    pub severity: &'static str,
    /// Primary and secondary spans with labels.
    pub spans: Vec<JsonSpan>,
    /// Suggested fixes that can be applied.
    pub suggestions: Vec<JsonSuggestion>,
    /// Additional notes providing context.
    pub notes: Vec<String>,
    /// Additional help messages.
    pub helps: Vec<String>,
}

/// A span in JSON format with file location and labels.
#[derive(Debug)]
pub struct JsonSpan {
    /// Source file path.
    pub file: String,
    /// Start byte offset (0-indexed).
    pub start: u32,
    /// End byte offset (exclusive).
    pub end: u32,
    /// Line number (1-indexed).
    pub line: u32,
    /// Column number (1-indexed).
    pub column: u32,
    /// Label text for this span.
    pub label: Option<String>,
    /// Whether this is the primary span.
    pub primary: bool,
}

/// A suggested fix in JSON format.
#[derive(Debug)]
pub struct JsonSuggestion {
    /// Human-readable description.
    pub message: String,
    /// File containing the span.
    pub file: String,
    /// Start byte offset.
    pub start: u32,
    /// End byte offset.
    pub end: u32,
    /// Replacement text.
    pub replacement: String,
    /// Applicability level.
    pub applicability: String,
}

impl JsonDiagnostic {
    /// Serialize this diagnostic to a JSON string.
    pub fn to_json(&self) -> String {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "code".to_string(),
            serde_json::Value::String(self.code.clone()),
        );
        obj.insert(
            "message".to_string(),
            serde_json::Value::String(self.message.clone()),
        );
        obj.insert(
            "severity".to_string(),
            serde_json::Value::String(self.severity.to_string()),
        );

        // Spans
        let spans: Vec<serde_json::Value> = self
            .spans
            .iter()
            .map(|s| {
                let mut span = serde_json::Map::new();
                span.insert(
                    "file".to_string(),
                    serde_json::Value::String(s.file.clone()),
                );
                span.insert(
                    "start".to_string(),
                    serde_json::Value::Number(s.start.into()),
                );
                span.insert("end".to_string(), serde_json::Value::Number(s.end.into()));
                span.insert("line".to_string(), serde_json::Value::Number(s.line.into()));
                span.insert(
                    "column".to_string(),
                    serde_json::Value::Number(s.column.into()),
                );
                if let Some(label) = &s.label {
                    span.insert(
                        "label".to_string(),
                        serde_json::Value::String(label.clone()),
                    );
                } else {
                    span.insert("label".to_string(), serde_json::Value::Null);
                }
                span.insert("primary".to_string(), serde_json::Value::Bool(s.primary));
                serde_json::Value::Object(span)
            })
            .collect();
        obj.insert("spans".to_string(), serde_json::Value::Array(spans));

        // Suggestions
        let suggestions: Vec<serde_json::Value> = self
            .suggestions
            .iter()
            .map(|s| {
                let mut sugg = serde_json::Map::new();
                sugg.insert(
                    "message".to_string(),
                    serde_json::Value::String(s.message.clone()),
                );
                sugg.insert(
                    "file".to_string(),
                    serde_json::Value::String(s.file.clone()),
                );
                sugg.insert(
                    "start".to_string(),
                    serde_json::Value::Number(s.start.into()),
                );
                sugg.insert("end".to_string(), serde_json::Value::Number(s.end.into()));
                sugg.insert(
                    "replacement".to_string(),
                    serde_json::Value::String(s.replacement.clone()),
                );
                sugg.insert(
                    "applicability".to_string(),
                    serde_json::Value::String(s.applicability.clone()),
                );
                serde_json::Value::Object(sugg)
            })
            .collect();
        obj.insert(
            "suggestions".to_string(),
            serde_json::Value::Array(suggestions),
        );

        // Notes and helps
        let notes: Vec<serde_json::Value> = self
            .notes
            .iter()
            .map(|n| serde_json::Value::String(n.clone()))
            .collect();
        obj.insert("notes".to_string(), serde_json::Value::Array(notes));

        let helps: Vec<serde_json::Value> = self
            .helps
            .iter()
            .map(|h| serde_json::Value::String(h.clone()))
            .collect();
        obj.insert("helps".to_string(), serde_json::Value::Array(helps));

        serde_json::to_string(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Formats diagnostics as JSON for machine consumption.
///
/// Use this formatter when outputting to tools like editors, CI systems,
/// or any context requiring machine-readable output.
pub struct JsonDiagnosticFormatter<'a> {
    source_info: &'a SourceInfo<'a>,
}

impl<'a> JsonDiagnosticFormatter<'a> {
    /// Create a new JSON diagnostic formatter.
    pub fn new(source_info: &'a SourceInfo<'a>) -> Self {
        Self { source_info }
    }

    /// Calculate line and column for a byte offset.
    fn offset_to_line_col(&self, offset: u32) -> (u32, u32) {
        let offset = offset as usize;
        let source = self.source_info.source;
        let mut line = 1u32;
        let mut col = 1u32;
        for (i, ch) in source.char_indices() {
            if i >= offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// Format a compile error as JSON.
    pub fn format_error(&self, error: &CompileError) -> JsonDiagnostic {
        let diag = error.diagnostic();
        let (line, col) = error
            .span()
            .map(|s| self.offset_to_line_col(s.start))
            .unwrap_or((1, 1));

        let primary_span = error.span().map(|span| JsonSpan {
            file: self.source_info.path.to_string(),
            start: span.start,
            end: span.end,
            line,
            column: col,
            label: None,
            primary: true,
        });

        let secondary_spans: Vec<JsonSpan> = diag
            .labels
            .iter()
            .map(|label| {
                let (line, col) = self.offset_to_line_col(label.span.start);
                JsonSpan {
                    file: self.source_info.path.to_string(),
                    start: label.span.start,
                    end: label.span.end,
                    line,
                    column: col,
                    label: Some(label.message.clone()),
                    primary: false,
                }
            })
            .collect();

        let mut spans: Vec<JsonSpan> = primary_span.into_iter().collect();
        spans.extend(secondary_spans);

        let suggestions: Vec<JsonSuggestion> = diag
            .suggestions
            .iter()
            .map(|s| JsonSuggestion {
                message: s.message.clone(),
                file: self.source_info.path.to_string(),
                start: s.span.start,
                end: s.span.end,
                replacement: s.replacement.clone(),
                applicability: s.applicability.to_string(),
            })
            .collect();

        JsonDiagnostic {
            code: format!("{}", error.kind.code()),
            message: format!("{}", error.kind),
            severity: "error",
            spans,
            suggestions,
            notes: diag.notes.iter().map(|n| n.0.clone()).collect(),
            helps: diag.helps.iter().map(|h| h.0.clone()).collect(),
        }
    }

    /// Format a compile warning as JSON.
    pub fn format_warning(&self, warning: &CompileWarning) -> JsonDiagnostic {
        let diag = warning.diagnostic();
        let (line, col) = warning
            .span()
            .map(|s| self.offset_to_line_col(s.start))
            .unwrap_or((1, 1));

        let primary_span = warning.span().map(|span| JsonSpan {
            file: self.source_info.path.to_string(),
            start: span.start,
            end: span.end,
            line,
            column: col,
            label: None,
            primary: true,
        });

        let secondary_spans: Vec<JsonSpan> = diag
            .labels
            .iter()
            .map(|label| {
                let (line, col) = self.offset_to_line_col(label.span.start);
                JsonSpan {
                    file: self.source_info.path.to_string(),
                    start: label.span.start,
                    end: label.span.end,
                    line,
                    column: col,
                    label: Some(label.message.clone()),
                    primary: false,
                }
            })
            .collect();

        let mut spans: Vec<JsonSpan> = primary_span.into_iter().collect();
        spans.extend(secondary_spans);

        let suggestions: Vec<JsonSuggestion> = diag
            .suggestions
            .iter()
            .map(|s| JsonSuggestion {
                message: s.message.clone(),
                file: self.source_info.path.to_string(),
                start: s.span.start,
                end: s.span.end,
                replacement: s.replacement.clone(),
                applicability: s.applicability.to_string(),
            })
            .collect();

        JsonDiagnostic {
            code: String::new(), // Warnings don't have codes yet
            message: format!("{}", warning.kind),
            severity: "warning",
            spans,
            suggestions,
            notes: diag.notes.iter().map(|n| n.0.clone()).collect(),
            helps: diag.helps.iter().map(|h| h.0.clone()).collect(),
        }
    }

    /// Format multiple errors as a JSON array string.
    pub fn format_errors(&self, errors: &CompileErrors) -> String {
        let diagnostics: Vec<String> = errors
            .iter()
            .map(|e| self.format_error(e).to_json())
            .collect();
        format!("[{}]", diagnostics.join(","))
    }

    /// Format multiple warnings as a JSON array string.
    pub fn format_warnings(&self, warnings: &[CompileWarning]) -> String {
        let diagnostics: Vec<String> = warnings
            .iter()
            .map(|w| self.format_warning(w).to_json())
            .collect();
        format!("[{}]", diagnostics.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorKind, Suggestion, WarningKind};

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
        // Should include error code
        assert!(output.contains("[E0206]"));
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
        // Should include error code
        assert!(output.contains("[E0200]"));
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

    #[test]
    fn test_color_choice_never() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let output = formatter.format_error(&error);
        // Output should not contain ANSI escape codes
        assert!(!output.contains("\x1b["));
        assert!(output.contains("type mismatch"));
    }

    #[test]
    fn test_color_choice_always() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Always);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let output = formatter.format_error(&error);
        // Output should contain ANSI escape codes
        assert!(output.contains("\x1b["));
        assert!(output.contains("type mismatch"));
    }

    // ========================================================================
    // JSON Formatting Tests
    // ========================================================================

    #[test]
    fn test_json_format_error() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let json_diag = formatter.format_error(&error);
        assert_eq!(json_diag.severity, "error");
        assert_eq!(json_diag.code, "E0206");
        assert!(json_diag.message.contains("type mismatch"));
        assert_eq!(json_diag.spans.len(), 1);
        assert_eq!(json_diag.spans[0].file, "test.rue");
        assert_eq!(json_diag.spans[0].start, 23);
        assert_eq!(json_diag.spans[0].end, 27);
        assert!(json_diag.spans[0].primary);
    }

    #[test]
    fn test_json_format_error_line_col() {
        let source = "fn main() -> i32 {\n    1 + true\n}";
        //                            ^--- line 2, col 9 (0-indexed: 23)
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(27, 31), // "true" on line 2
        );

        let json_diag = formatter.format_error(&error);
        assert_eq!(json_diag.spans[0].line, 2);
        assert!(json_diag.spans[0].column > 1); // Column should be > 1 (indented)
    }

    #[test]
    fn test_json_format_error_with_suggestion() {
        let source = "fn main() -> i32 { x = 1; 0 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::AssignToImmutable("x".to_string()),
            Span::new(19, 20),
        )
        .with_suggestion(Suggestion::machine_applicable(
            "add mut",
            Span::new(4, 5),
            "mut x",
        ));

        let json_diag = formatter.format_error(&error);
        assert_eq!(json_diag.suggestions.len(), 1);
        assert_eq!(json_diag.suggestions[0].message, "add mut");
        assert_eq!(json_diag.suggestions[0].replacement, "mut x");
        assert_eq!(json_diag.suggestions[0].applicability, "MachineApplicable");
    }

    #[test]
    fn test_json_format_warning() {
        let source = "fn main() -> i32 { let x = 42; 0 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let warning = CompileWarning::new(
            WarningKind::UnusedVariable("x".to_string()),
            Span::new(23, 24),
        );

        let json_diag = formatter.format_warning(&warning);
        assert_eq!(json_diag.severity, "warning");
        assert!(json_diag.message.contains("unused variable"));
    }

    #[test]
    fn test_json_to_string() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let json_diag = formatter.format_error(&error);
        let json_str = json_diag.to_json();

        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["severity"], "error");
        assert_eq!(parsed["code"], "E0206");
        assert!(parsed["spans"].is_array());
        assert_eq!(parsed["spans"][0]["primary"], true);
    }

    #[test]
    fn test_json_format_errors_array() {
        let source = "fn main() -> i32 {\n    1 + true\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(27, 31),
        ));

        let json_str = formatter.format_errors(&errors);
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }
}
