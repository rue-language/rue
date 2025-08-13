//! Parser diagnostic snapshot tests
//!
//! These tests use the new diagnostic system to provide rich error messages

#[cfg(test)]
mod tests {
    use crate::diagnostics::parse_with_diagnostics;
    use rue_diagnostic::{DiagnosticFormatter, SourceManager};
    use rue_snapshot::{Snapshot, SnapshotConfig, normalize::CompositeNormalizer};

    fn assert_diagnostic_snapshot(test_name: &str, source: &str) {
        let result = parse_with_diagnostics(source, "test.rue");

        let output = match result {
            Ok(cst) => format!("Success: {cst:#?}"),
            Err(diagnostics) => {
                let formatter = DiagnosticFormatter::plain(); // Plain for consistent snapshots
                let mut source_manager = SourceManager::new();
                source_manager.add_source("test.rue", source);

                let mut output = String::new();
                for diagnostic in &diagnostics {
                    if !output.is_empty() {
                        output.push_str("\n---\n");
                    }
                    let formatted = formatter
                        .format_diagnostic(diagnostic, &source_manager)
                        .unwrap_or_else(|e| format!("Format error: {e}"));
                    output.push_str(&formatted);
                }
                output
            }
        };

        // Use the new snapshot system with normalization for consistent output
        let config = SnapshotConfig::default().with_normalizer(CompositeNormalizer::standard());

        Snapshot::with_config(test_name, config)
            .assert(&output)
            .expect("Snapshot assertion failed");
    }

    #[test]
    fn test_missing_semicolon() {
        assert_diagnostic_snapshot("diagnostic_missing_semicolon", "let x = 42");
    }

    #[test]
    fn test_unclosed_brace() {
        assert_diagnostic_snapshot("diagnostic_unclosed_brace", "fn main() { 42");
    }

    #[test]
    fn test_unexpected_token() {
        assert_diagnostic_snapshot("diagnostic_unexpected_token", "fn main() { let x = ; }");
    }

    #[test]
    fn test_duplicate_field() {
        assert_diagnostic_snapshot(
            "diagnostic_duplicate_field",
            "struct Point { x: i32, x: i32 }",
        );
    }

    #[test]
    fn test_missing_type_annotation() {
        assert_diagnostic_snapshot(
            "diagnostic_missing_type_annotation",
            "fn add(x, y) { x + y }",
        );
    }

    #[test]
    fn test_invalid_array_size() {
        assert_diagnostic_snapshot("diagnostic_invalid_array_size", "let arr: [i32; -5];");
    }
}
