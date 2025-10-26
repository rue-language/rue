//! Parser snapshot tests using insta
//!
//! This demonstrates the migration from rue-snapshot to insta for AST validation.
//! Insta provides better ergonomics, inline snapshots, and powerful redactions.

mod insta_utils;

use rue_parser::parse_with_diagnostics;

/// Helper to configure insta settings for all tests in this module
fn insta_settings() -> insta::Settings {
    insta_utils::configure_insta("tests/snapshots")
}

#[test]
fn test_simple_function_ast() {
    let source = r#"
fn main() -> i32 {
    42
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();

    insta_settings().bind(|| {
        insta::assert_debug_snapshot!("insta_parser_simple_function", ast);
    });
}

#[test]
fn test_binary_expression_ast() {
    let source = r#"
fn main() -> i32 {
    let x = 10;
    let y = 20;
    x + y * 2
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();

    insta_settings().bind(|| {
        insta::assert_debug_snapshot!("insta_parser_binary_expression", ast);
    });
}

#[test]
fn test_if_expression_ast() {
    let source = r#"
fn main() -> i32 {
    let x = 10;
    if x > 5 {
        100
    } else {
        200
    }
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();

    insta_settings().bind(|| {
        insta::assert_debug_snapshot!("insta_parser_if_expression", ast);
    });
}

#[test]
fn test_while_loop_ast() {
    let source = r#"
fn main() -> i32 {
    let count = 5;
    while count > 0 {
        count = count - 1;
    };
    count
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();

    insta_settings().bind(|| {
        insta::assert_debug_snapshot!("insta_parser_while_loop", ast);
    });
}

#[test]
fn test_function_call_ast() {
    let source = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(10, 20)
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();

    insta_settings().bind(|| {
        insta::assert_debug_snapshot!("insta_parser_function_call", ast);
    });
}

#[test]
fn test_type_annotations_ast() {
    let source = r#"
fn main() -> i32 {
    let x: i32 = 42;
    let y: i64 = 100;
    let z: bool = true;
    x
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();

    insta_settings().bind(|| {
        insta::assert_debug_snapshot!("insta_parser_type_annotations", ast);
    });
}

#[test]
fn test_parser_error_recovery() {
    // Test that parser can recover from errors
    let source = r#"
fn main() -> i32 {
    let x = ;  // Missing expression
    42
}
"#;

    let result = parse_with_diagnostics(source, "test.rue");

    insta_settings().bind(|| {
        insta::assert_debug_snapshot!("insta_parser_error_recovery", result);
    });
}

#[test]
fn test_complex_nested_expressions() {
    let source = r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    let result = factorial(5);
    result
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();

    insta_settings().bind(|| {
        insta::assert_debug_snapshot!("insta_parser_complex_nested", ast);
    });
}

// Demonstration of inline snapshots
#[test]
fn test_inline_snapshot_example() {
    let source = "fn main() -> i32 { 42 }";
    let ast = parse_with_diagnostics(source, "test.rue").unwrap();

    // The snapshot value will be stored directly in this file after the @ sign
    // Run with INSTA_UPDATE=always to populate it initially
    insta_settings().bind(|| {
        insta::assert_snapshot!(format!("{:?}", ast), @"");
    });
}

// Demonstration of redactions (similar to normalizers)
#[test]
fn test_with_redactions() {
    let source = r#"
fn main() -> i32 {
    42
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();
    let output = format!("{:#?}", ast);

    let mut settings = insta_settings();

    // Redact things that might change between runs
    // This is equivalent to the CompositeNormalizer in rue-snapshot
    settings.add_redaction(r"\b0x[0-9a-fA-F]+\b", "[ADDRESS]");
    settings.add_redaction(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}", "[TIMESTAMP]");

    settings.bind(|| {
        insta::assert_snapshot!("insta_parser_with_redactions", output);
    });
}
