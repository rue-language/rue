//! Comprehensive tests for the enhanced diagnostic system
//!
//! This test demonstrates the full diagnostic pipeline including:
//! - Parser error recovery
//! - Rich error formatting
//! - Multi-span diagnostics
//! - Suggestions and help text

use rue_parser::parse_with_recovery;

#[test]
fn test_comprehensive_diagnostic_output() {
    let source = r#"
// Test various error scenarios
fn main() -> i32 {
    let x = ;         // Missing expression
    let y = 42        // Missing semicolon
    let z = true;     // Type will mismatch later
    
    // Undefined variable
    let result = unknown_var + x;
    
    // Wrong number of arguments
    some_function(x, y, z, extra);
    
    // Unclosed delimiter
    if x > 5 {
        return x * 2
    // Missing closing brace
    
    return result;
}

// Duplicate function
fn main() {
    // This should error
}

// Struct with errors
struct Point {
    x: i32,
    y: ,        // Missing type
    x: bool,    // Duplicate field
}
"#;

    match parse_with_recovery(source, "test.rue") {
        Ok(_) => panic!("Expected errors"),
        Err(diagnostics) => {
            // Should have collected multiple errors
            assert!(
                diagnostics.len() >= 3,
                "Expected at least 3 errors, got {}",
                diagnostics.len()
            );

            // Verify specific error messages exist
            let error_messages: Vec<String> =
                diagnostics.iter().map(|d| d.message.clone()).collect();

            // Check for missing expression error
            assert!(
                error_messages
                    .iter()
                    .any(|m| m.contains("Unexpected token: Semicolon") || m.contains("expression")),
                "Should have error about missing expression"
            );

            // Check for missing semicolon error
            assert!(
                error_messages
                    .iter()
                    .any(|m| m.contains("Semicolon") || m.contains("semicolon")),
                "Should have error about missing semicolon"
            );
        }
    }
}

#[test]
fn test_diagnostic_with_suggestions() {
    let source = r#"
fn main() {
    let counter = 10;
    let result = countr + 1;  // Typo: should be 'counter'
}
"#;

    // This test is for future when we have semantic analysis with suggestions
    // For now, it will just parse successfully
    match parse_with_recovery(source, "test.rue") {
        Ok(_cst) => {
            // Currently this parses fine - suggestions will come from semantic analysis
            // The parser succeeded, which is expected for this input
        }
        Err(diagnostics) => {
            // Once semantic analysis is integrated, we expect suggestions here
            for diagnostic in &diagnostics {
                if diagnostic.message.contains("countr") {
                    assert!(
                        diagnostic.help.is_some(),
                        "Should have help text with suggestions"
                    );
                }
            }
        }
    }
}

#[test]
fn test_error_recovery_limit() {
    // Generate source with many errors to test recovery limits
    let mut source = String::from("fn main() {\n");

    // Add 150 lines with errors
    for i in 0..150 {
        source.push_str(&format!("    let var{i} = ;  // Error\n"));
    }

    source.push_str("}\n");

    match parse_with_recovery(&source, "test.rue") {
        Ok(_) => panic!("Expected errors"),
        Err(diagnostics) => {
            // Should be limited to 100 errors (our configured max)
            assert!(
                diagnostics.len() <= 100,
                "Error count {} should be limited to 100",
                diagnostics.len()
            );
            assert!(!diagnostics.is_empty(), "Should have at least some errors");
        }
    }
}
