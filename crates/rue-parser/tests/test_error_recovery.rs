use rue_parser::parse_with_recovery;

#[test]
fn test_multiple_errors_in_single_pass() {
    let source = r#"
fn main() -> i32 {
    let x = ;     // Error 1: missing expression
    let y = 42    // Error 2: missing semicolon
    let z = true; // Should still parse this
    return x + y + z  // Error 3: missing semicolon
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
        }
    }
}

#[test]
fn test_recovery_with_unclosed_delimiters() {
    let source = r#"
fn main() {
    if x > 5 {
        let y = 10;
        // Missing closing brace for if
    
    let z = 20;  // Should still try to parse this
}
"#;

    match parse_with_recovery(source, "test.rue") {
        Ok(_) => panic!("Expected errors"),
        Err(diagnostics) => {
            assert!(!diagnostics.is_empty());

            // Should have error about missing brace
            let has_brace_error = diagnostics.iter().any(|d| {
                d.message.contains("brace")
                    || d.message.contains("RightBrace")
                    || d.message.contains("}")
            });
            assert!(has_brace_error, "Expected error about missing brace");
        }
    }
}

#[test]
fn test_recovery_limit() {
    // Create a source with many errors to test the recovery limit
    let mut source = String::from("fn main() {\n");
    for i in 0..200 {
        source.push_str(&format!("    let x{i} = ;\n")); // 200 errors
    }
    source.push('}');

    match parse_with_recovery(&source, "test.rue") {
        Ok(_) => panic!("Expected errors"),
        Err(diagnostics) => {
            // Should stop collecting after a reasonable limit (100)
            assert!(
                diagnostics.len() <= 100,
                "Should limit errors to 100, got {}",
                diagnostics.len()
            );
            assert!(!diagnostics.is_empty(), "Should have at least some errors");
        }
    }
}

#[test]
fn test_recovery_preserves_error_quality() {
    let source = r#"
fn main() {
    let x: i32 = ;     // Should have helpful error about missing expression
    let y = true;
    return x + y;      // Should have type mismatch error eventually
}
"#;

    match parse_with_recovery(source, "test.rue") {
        Ok(_) => panic!("Expected errors"),
        Err(diagnostics) => {
            assert!(!diagnostics.is_empty());

            // Check that errors have meaningful messages
            for diagnostic in &diagnostics {
                assert!(!diagnostic.message.is_empty());
                // Should have at least primary span
                assert!(diagnostic.primary_span().is_some());
            }
        }
    }
}
