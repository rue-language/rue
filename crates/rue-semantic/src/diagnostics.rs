//! Rich diagnostics for semantic analysis
//!
//! This module provides enhanced error reporting for type checking and semantic analysis,
//! integrating with the rue-diagnostic infrastructure for LSP-ready error messages.

use crate::SemanticError;
use rue_diagnostic::{
    Diagnostic, DiagnosticBuilder, E2001, E2002, E3001, E3002, Label, SourceId, W1003,
};
use rue_ir::types::RueType;
use rue_lexer::Span;

/// Enhanced semantic error with diagnostic support
pub struct SemanticDiagnostic {
    pub diagnostic: Diagnostic,
    pub severity: SemanticSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticSeverity {
    Error,
    Warning,
    Info,
}

// Type alias for error categorizer functions
type ErrorCategorizer = (fn(&str) -> bool, fn(&SemanticError, SourceId) -> Diagnostic);

/// Convert a SemanticError to a rich diagnostic using pattern matching
pub fn semantic_error_to_diagnostic(error: &SemanticError, source_id: SourceId) -> Diagnostic {
    // Use functional approach to categorize errors
    let error_categorizers: &[ErrorCategorizer] = &[
        (
            |msg| msg.contains("Type mismatch"),
            create_type_mismatch_diagnostic,
        ),
        (
            |msg| msg.contains("Undefined variable"),
            create_undefined_variable_diagnostic,
        ),
        (
            |msg| msg.contains("Duplicate"),
            create_duplicate_definition_diagnostic,
        ),
        (
            |msg| msg.contains("Cannot infer type"),
            create_type_inference_diagnostic,
        ),
        (
            |msg| msg.contains("Wrong number of arguments"),
            create_argument_count_diagnostic,
        ),
    ];

    error_categorizers
        .iter()
        .find(|(predicate, _)| predicate(&error.message))
        .map(|(_, creator)| creator(error, source_id.clone()))
        .unwrap_or_else(|| {
            // Generic semantic error
            DiagnosticBuilder::error(&error.message, source_id)
                .with_primary_span(error.span)
                .build()
        })
}

/// Create a diagnostic for type mismatch errors
fn create_type_mismatch_diagnostic(error: &SemanticError, source_id: SourceId) -> Diagnostic {
    // Try to extract expected and actual types from the message
    let (expected, actual) = extract_types_from_message(&error.message);

    let mut builder = DiagnosticBuilder::error(error.message.clone(), source_id)
        .with_code(E3001)
        .with_primary_span(error.span);

    if let (Some(expected), Some(actual)) = (expected, actual) {
        builder = builder
            .with_help(format!(
                "Expected type '{expected}' but found '{actual}'. Consider casting or changing the value."
            ))
            .with_label(Label::primary_with_message(
                error.span,
                format!("has type '{actual}'"),
            ));
    }

    builder.build()
}

/// Create a diagnostic for undefined variable errors
fn create_undefined_variable_diagnostic(error: &SemanticError, source_id: SourceId) -> Diagnostic {
    // Extract variable name from error message
    let var_name = extract_variable_name(&error.message);

    let mut builder = DiagnosticBuilder::error(error.message.clone(), source_id)
        .with_code(E2001)
        .with_primary_span(error.span)
        .with_label(Label::primary_with_message(
            error.span,
            "not found in this scope",
        ));

    if let Some(name) = var_name {
        // TODO: Add suggestions for similar variable names using edit distance
        builder = builder.with_help(format!(
            "Did you mean to declare '{name}' before using it? Use 'let {name} = ...' to declare a variable."
        ));
    }

    builder.build()
}

/// Create a diagnostic for duplicate definition errors
fn create_duplicate_definition_diagnostic(
    error: &SemanticError,
    source_id: SourceId,
) -> Diagnostic {
    DiagnosticBuilder::error(error.message.clone(), source_id)
        .with_code(E2002)
        .with_primary_span(error.span)
        .with_label(Label::primary_with_message(
            error.span,
            "duplicate definition here",
        ))
        .with_help(
            "Each variable, function, or type must have a unique name in its scope.".to_string(),
        )
        .build()
}

/// Create a diagnostic for type inference errors
fn create_type_inference_diagnostic(error: &SemanticError, source_id: SourceId) -> Diagnostic {
    DiagnosticBuilder::error(error.message.clone(), source_id)
        .with_code(E3001)
        .with_primary_span(error.span)
        .with_label(Label::primary_with_message(
            error.span,
            "cannot infer type here",
        ))
        .with_help(
            "Consider adding explicit type annotations to help the compiler infer types."
                .to_string(),
        )
        .build()
}

/// Create a diagnostic for argument count errors
fn create_argument_count_diagnostic(error: &SemanticError, source_id: SourceId) -> Diagnostic {
    // Extract expected and actual counts from message
    let (expected, actual) = extract_argument_counts(&error.message);

    let mut builder = DiagnosticBuilder::error(error.message.clone(), source_id)
        .with_code(E3002)
        .with_primary_span(error.span);

    if let (Some(expected), Some(actual)) = (expected, actual) {
        let diff = (expected as i32 - actual as i32).abs();
        let verb = if actual < expected {
            "missing"
        } else {
            "extra"
        };

        builder = builder
            .with_label(Label::primary_with_message(
                error.span,
                format!(
                    "{} {} argument{}",
                    diff,
                    verb,
                    if diff == 1 { "" } else { "s" }
                ),
            ))
            .with_help(format!(
                "This function expects {} argument{}, but {} {} provided.",
                expected,
                if expected == 1 { "" } else { "s" },
                actual,
                if actual == 1 { "was" } else { "were" }
            ));
    }

    builder.build()
}

/// Helper to extract types from a type mismatch message
fn extract_types_from_message(message: &str) -> (Option<String>, Option<String>) {
    // Look for patterns like "expected X, found Y" or "Type mismatch: X vs Y"
    if let Some(idx) = message.find("expected") {
        let rest = &message[idx..];
        if let Some(found_idx) = rest.find("found") {
            let expected = rest[8..found_idx].trim().trim_end_matches(',');
            let actual = rest[found_idx + 5..].trim().trim_end_matches('.');
            return (Some(expected.to_string()), Some(actual.to_string()));
        }
    }
    (None, None)
}

/// Helper to extract variable name from undefined variable message
fn extract_variable_name(message: &str) -> Option<String> {
    // Look for patterns like "Undefined variable 'x'" or "Variable x not found"
    if let Some(start) = message.find('\'') {
        if let Some(end) = message[start + 1..].find('\'') {
            return Some(message[start + 1..start + 1 + end].to_string());
        }
    } else if let Some(rest) = message.strip_prefix("Undefined variable ") {
        let name = rest.trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

/// Helper to extract argument counts from error message using iterators
fn extract_argument_counts(message: &str) -> (Option<usize>, Option<usize>) {
    let words: Vec<&str> = message.split_whitespace().collect();

    let expected = words
        .windows(2)
        .find(|pair| pair[0] == "expected")
        .and_then(|pair| pair[1].trim_end_matches(',').parse().ok());

    let actual = words
        .windows(2)
        .find(|pair| matches!(pair[0], "got" | "found" | "provided"))
        .and_then(|pair| pair[1].trim_end_matches(',').parse().ok());

    (expected, actual)
}

/// Create a type mismatch diagnostic with source locations for both types
pub fn type_mismatch_with_locations(
    expected_type: &RueType,
    expected_span: Span,
    actual_type: &RueType,
    actual_span: Span,
    source_id: SourceId,
) -> Diagnostic {
    DiagnosticBuilder::error(
        format!("Type mismatch: expected '{expected_type}', found '{actual_type}'"),
        source_id,
    )
    .with_code(E3001)
    .with_primary_span(actual_span)
    .with_label(Label::primary_with_message(
        actual_span,
        format!("has type '{actual_type}'"),
    ))
    .with_label(Label::secondary_with_message(
        expected_span,
        format!("expected type '{expected_type}' (required here)"),
    ))
    .with_help(
        "The types must match exactly. Consider adding a cast or changing the value.".to_string(),
    )
    .build()
}

/// Create a warning for unreachable code
pub fn unreachable_code_warning(span: Span, source_id: SourceId) -> Diagnostic {
    DiagnosticBuilder::warning("Unreachable code detected", source_id)
        .with_code(W1003)
        .with_primary_span(span)
        .with_label(Label::primary_with_message(
            span,
            "this code will never be executed",
        ))
        .with_help("Remove this code or check the control flow logic above.".to_string())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_types_from_message() {
        let msg = "Type mismatch: expected i32, found bool";
        let (expected, actual) = extract_types_from_message(msg);
        assert_eq!(expected, Some("i32".to_string()));
        assert_eq!(actual, Some("bool".to_string()));
    }

    #[test]
    fn test_extract_variable_name() {
        let msg = "Undefined variable 'foo'";
        assert_eq!(extract_variable_name(msg), Some("foo".to_string()));

        let msg2 = "Undefined variable bar";
        assert_eq!(extract_variable_name(msg2), Some("bar".to_string()));
    }

    #[test]
    fn test_extract_argument_counts() {
        let msg = "Wrong number of arguments: expected 2, got 3";
        let (expected, actual) = extract_argument_counts(msg);
        assert_eq!(expected, Some(2));
        assert_eq!(actual, Some(3));
    }
}
