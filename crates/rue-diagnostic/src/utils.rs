//! Utility functions for common diagnostic patterns

use crate::{Diagnostic, DiagnosticBuilder, DiagnosticCode, Label, SourceId};
use rue_lexer::Span;

/// Helper trait for fluent diagnostic creation
pub trait DiagnosticExt {
    /// Create an error diagnostic with a primary span
    fn error_at(message: impl Into<String>, span: Span, source_id: SourceId) -> Diagnostic {
        DiagnosticBuilder::error(message, source_id)
            .with_primary_span(span)
            .build()
    }

    /// Create a warning diagnostic with a primary span
    fn warning_at(message: impl Into<String>, span: Span, source_id: SourceId) -> Diagnostic {
        DiagnosticBuilder::warning(message, source_id)
            .with_primary_span(span)
            .build()
    }

    /// Create an error with code and help text
    fn error_with_help(
        message: impl Into<String>,
        span: Span,
        source_id: SourceId,
        code: DiagnosticCode,
        help: impl Into<String>,
    ) -> Diagnostic {
        DiagnosticBuilder::error(message, source_id)
            .with_code(code)
            .with_primary_span(span)
            .with_help(help)
            .build()
    }

    /// Create a type mismatch diagnostic
    fn type_mismatch(
        expected: impl std::fmt::Display,
        actual: impl std::fmt::Display,
        span: Span,
        source_id: SourceId,
    ) -> Diagnostic {
        DiagnosticBuilder::error(
            format!("Type mismatch: expected '{expected}', found '{actual}'"),
            source_id,
        )
        .with_code(crate::E3001)
        .with_primary_span(span)
        .with_label(Label::primary_with_message(
            span,
            format!("has type '{actual}'"),
        ))
        .with_help("Consider adding a cast or changing the value")
        .build()
    }

    /// Create an undefined identifier diagnostic
    fn undefined_identifier(
        name: impl std::fmt::Display,
        span: Span,
        source_id: SourceId,
    ) -> Diagnostic {
        DiagnosticBuilder::error(format!("Undefined identifier: {name}"), source_id)
            .with_code(crate::E2001)
            .with_primary_span(span)
            .with_label(Label::primary_with_message(span, "not found in this scope"))
            .with_help(format!("Did you mean to declare '{name}' first?"))
            .build()
    }
}

impl DiagnosticExt for Diagnostic {}

/// Common text processing utilities for error messages
pub mod text {

    /// Extract a quoted string from an error message
    pub fn extract_quoted_string(message: &str) -> Option<&str> {
        let start = message.find('\'')?;
        let end = message[start + 1..].find('\'')?;
        Some(&message[start + 1..start + 1 + end])
    }

    /// Extract two types from a type mismatch message
    pub fn extract_type_mismatch(message: &str) -> Option<(String, String)> {
        if let Some(idx) = message.find("expected") {
            let rest = &message[idx..];
            if let Some(found_idx) = rest.find("found") {
                let expected = rest[8..found_idx].trim().trim_end_matches(',').to_string();
                let actual = rest[found_idx + 5..]
                    .trim()
                    .trim_end_matches('.')
                    .to_string();
                return Some((expected, actual));
            }
        }
        None
    }

    /// Extract numbers from error messages
    pub fn extract_numbers(message: &str) -> Vec<usize> {
        message
            .split_whitespace()
            .filter_map(|word| {
                word.trim_end_matches(',')
                    .trim_end_matches('.')
                    .parse::<usize>()
                    .ok()
            })
            .collect()
    }

    /// Convert snake_case to Title Case for user-friendly display
    pub fn snake_to_title_case(s: &str) -> String {
        s.split('_')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Create a suggestions based on edit distance
    pub fn suggest_similar(input: &str, candidates: &[&str], max_distance: usize) -> Vec<String> {
        candidates
            .iter()
            .filter_map(|&candidate| {
                let distance = edit_distance(input, candidate);
                if distance <= max_distance && distance > 0 {
                    Some((distance, candidate.to_string()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .into_iter()
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, suggestion)| vec![suggestion])
            .unwrap_or_default()
    }

    /// Simple Levenshtein distance implementation
    #[allow(clippy::needless_range_loop)]
    fn edit_distance(a: &str, b: &str) -> usize {
        let a_len = a.len();
        let b_len = b.len();

        if a_len == 0 {
            return b_len;
        }
        if b_len == 0 {
            return a_len;
        }

        let mut matrix = vec![vec![0; b_len + 1]; a_len + 1];

        // Initialize first row and column
        for i in 0..=a_len {
            matrix[i][0] = i;
        }
        for j in 0..=b_len {
            matrix[0][j] = j;
        }

        // Fill the matrix
        for (i, a_char) in a.chars().enumerate() {
            for (j, b_char) in b.chars().enumerate() {
                let cost = if a_char == b_char { 0 } else { 1 };
                matrix[i + 1][j + 1] = [
                    matrix[i][j + 1] + 1, // deletion
                    matrix[i + 1][j] + 1, // insertion
                    matrix[i][j] + cost,  // substitution
                ]
                .into_iter()
                .min()
                .unwrap();
            }
        }

        matrix[a_len][b_len]
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_extract_quoted_string() {
            let msg = "Undefined variable 'foo' in scope";
            assert_eq!(extract_quoted_string(msg), Some("foo"));
        }

        #[test]
        fn test_extract_type_mismatch() {
            let msg = "Type mismatch: expected i32, found bool";
            assert_eq!(
                extract_type_mismatch(msg),
                Some(("i32".to_string(), "bool".to_string()))
            );
        }

        #[test]
        fn test_snake_to_title_case() {
            assert_eq!(
                snake_to_title_case("snake_case_string"),
                "Snake Case String"
            );
        }

        #[test]
        fn test_edit_distance() {
            assert_eq!(edit_distance("kitten", "sitting"), 3);
            assert_eq!(edit_distance("", "abc"), 3);
            assert_eq!(edit_distance("abc", ""), 3);
            assert_eq!(edit_distance("same", "same"), 0);
        }

        #[test]
        fn test_suggest_similar() {
            let candidates = &["hello", "world", "help", "held"];
            let suggestions = suggest_similar("helo", candidates, 3);
            assert!(!suggestions.is_empty());
            // "hello" should definitely match "helo" (edit distance 1)
            assert!(suggestions.contains(&"hello".to_string()));
        }
    }
}

/// Span utilities
pub mod spans {
    use rue_lexer::Span;

    /// Check if a span is valid for a given source length
    pub fn is_valid_span(span: Span, source_len: usize) -> bool {
        span.start <= source_len && span.end <= source_len && span.start <= span.end
    }

    /// Create a zero-width span at a position
    pub fn point_span(pos: usize) -> Span {
        Span::new(pos, pos)
    }

    /// Extend a span to include another span
    pub fn extend_span(base: Span, other: Span) -> Span {
        Span::new(base.start.min(other.start), base.end.max(other.end))
    }

    /// Create a span that covers multiple spans
    pub fn encompassing_span(spans: &[Span]) -> Option<Span> {
        if spans.is_empty() {
            return None;
        }

        spans.iter().copied().reduce(extend_span)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_is_valid_span() {
            assert!(is_valid_span(Span::new(0, 5), 10));
            assert!(is_valid_span(Span::new(5, 10), 10));
            assert!(!is_valid_span(Span::new(0, 15), 10));
            assert!(!is_valid_span(Span::new(5, 3), 10));
        }

        #[test]
        fn test_extend_span() {
            let span1 = Span::new(2, 5);
            let span2 = Span::new(7, 10);
            let extended = extend_span(span1, span2);
            assert_eq!(extended, Span::new(2, 10));
        }

        #[test]
        fn test_encompassing_span() {
            let spans = vec![Span::new(2, 5), Span::new(8, 12), Span::new(1, 3)];
            let encompassing = encompassing_span(&spans);
            assert_eq!(encompassing, Some(Span::new(1, 12)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostic_ext_error_at() {
        let source_id = SourceId::new("test.rue");
        let span = Span::new(10, 15);
        let diagnostic = Diagnostic::error_at("Test error", span, source_id);

        assert_eq!(diagnostic.message, "Test error");
        assert_eq!(diagnostic.primary_span(), Some(span));
    }

    #[test]
    fn test_type_mismatch_diagnostic() {
        let source_id = SourceId::new("test.rue");
        let span = Span::new(10, 15);
        let diagnostic = Diagnostic::type_mismatch("i32", "bool", span, source_id);

        assert!(diagnostic.message.contains("Type mismatch"));
        assert!(diagnostic.message.contains("i32"));
        assert!(diagnostic.message.contains("bool"));
        assert_eq!(diagnostic.code, Some(crate::E3001));
    }
}
