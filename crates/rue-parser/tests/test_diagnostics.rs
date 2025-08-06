use rue_diagnostic::{DiagnosticFormatter, SourceManager};
use rue_parser::parse_with_diagnostics;

#[test]
fn test_parser_error_diagnostic() {
    let source = r#"fn main() -> i32 {
    let x = 42;
    let y = ;  // Error: missing expression
    x + y
}"#;

    match parse_with_diagnostics(source, "test.rue") {
        Ok(_) => panic!("Expected parse error"),
        Err(diagnostics) => {
            assert!(!diagnostics.is_empty());

            // Format the diagnostic for display
            let formatter = DiagnosticFormatter::terminal();
            let mut source_manager = SourceManager::new();
            source_manager.add_source("test.rue", source);

            for diagnostic in &diagnostics {
                let output = formatter
                    .format_diagnostic(diagnostic, &source_manager)
                    .unwrap();
                println!("{output}");

                // Check that it's an error
                assert!(diagnostic.is_error());
                // Check that it has a helpful message
                assert!(!diagnostic.message.is_empty());
            }
        }
    }
}

#[test]
fn test_unclosed_brace_diagnostic() {
    let source = "fn main() -> i32 { 42";

    match parse_with_diagnostics(source, "test.rue") {
        Ok(_) => panic!("Expected parse error"),
        Err(diagnostics) => {
            assert!(!diagnostics.is_empty());

            let formatter = DiagnosticFormatter::plain();
            let mut source_manager = SourceManager::new();
            source_manager.add_source("test.rue", source);

            for diagnostic in &diagnostics {
                let output = formatter
                    .format_diagnostic(diagnostic, &source_manager)
                    .unwrap();
                println!("{output}");

                assert!(diagnostic.is_error());
            }
        }
    }
}

#[test]
fn test_successful_parse_no_diagnostics() {
    let source = "fn main() -> i32 { 42 }";

    match parse_with_diagnostics(source, "test.rue") {
        Ok(_cst) => {
            // Success - no diagnostics
        }
        Err(diagnostics) => {
            panic!("Unexpected parse error: {diagnostics:?}");
        }
    }
}
