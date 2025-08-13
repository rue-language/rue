//! Parser snapshot tests using the new rue-snapshot infrastructure
//!
//! This demonstrates how to use the enhanced snapshot testing framework
//! for AST validation.

use anyhow::Result;
use rue_parser::parse_with_diagnostics;
use rue_snapshot::{Snapshot, SnapshotConfig, normalize::CompositeNormalizer};

#[test]
fn test_simple_function_ast() -> Result<()> {
    let source = r#"
fn main() -> i32 {
    42
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();
    let ast_debug = format!("{:#?}", ast);

    // Use the new snapshot testing with normalization
    let config = SnapshotConfig::default().with_normalizer(CompositeNormalizer::standard());

    Snapshot::with_config("integration_parser_simple_function", config).assert(&ast_debug)?;

    Ok(())
}

#[test]
fn test_binary_expression_ast() -> Result<()> {
    let source = r#"
fn main() -> i32 {
    let x = 10;
    let y = 20;
    x + y * 2
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();
    let ast_debug = format!("{:#?}", ast);

    Snapshot::with_config(
        "integration_parser_binary_expression",
        SnapshotConfig::default(),
    )
    .assert(&ast_debug)?;

    Ok(())
}

#[test]
fn test_if_expression_ast() -> Result<()> {
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
    let ast_debug = format!("{:#?}", ast);

    Snapshot::with_config(
        "integration_parser_if_expression",
        SnapshotConfig::default(),
    )
    .assert(&ast_debug)?;

    Ok(())
}

#[test]
fn test_while_loop_ast() -> Result<()> {
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
    let ast_debug = format!("{:#?}", ast);

    Snapshot::with_config("integration_parser_while_loop", SnapshotConfig::default())
        .assert(&ast_debug)?;

    Ok(())
}

#[test]
fn test_function_call_ast() -> Result<()> {
    let source = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(10, 20)
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();
    let ast_debug = format!("{:#?}", ast);

    Snapshot::with_config(
        "integration_parser_function_call",
        SnapshotConfig::default(),
    )
    .assert(&ast_debug)?;

    Ok(())
}

#[test]
fn test_type_annotations_ast() -> Result<()> {
    let source = r#"
fn main() -> i32 {
    let x: i32 = 42;
    let y: i64 = 100;
    let z: bool = true;
    x
}
"#;

    let ast = parse_with_diagnostics(source, "test.rue").unwrap();
    let ast_debug = format!("{:#?}", ast);

    Snapshot::with_config(
        "integration_parser_type_annotations",
        SnapshotConfig::default(),
    )
    .assert(&ast_debug)?;

    Ok(())
}

#[test]
fn test_parser_error_recovery() -> Result<()> {
    // Test that parser can recover from errors
    let source = r#"
fn main() -> i32 {
    let x = ;  // Missing expression
    42
}
"#;

    let result = parse_with_diagnostics(source, "test.rue");
    let error_debug = format!("{:#?}", result);

    Snapshot::with_config(
        "integration_parser_error_recovery",
        SnapshotConfig::default(),
    )
    .assert(&error_debug)?;

    Ok(())
}

#[test]
fn test_complex_nested_expressions() -> Result<()> {
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
    let ast_debug = format!("{:#?}", ast);

    Snapshot::with_config(
        "integration_parser_complex_nested",
        SnapshotConfig::default(),
    )
    .assert(&ast_debug)?;

    Ok(())
}
