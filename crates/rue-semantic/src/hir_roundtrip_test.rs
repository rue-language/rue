//! HIR round-trip tests
//!
//! These tests verify that the same source code produces identical HIR when processed
//! multiple times, ensuring HIR generation is deterministic and consistent.

use super::*;

/// Custom comparison for HIR programs
fn hir_programs_equal(a: &hir::HirProgram, b: &hir::HirProgram) -> bool {
    if a.functions.len() != b.functions.len() {
        return false;
    }

    for (func_a, func_b) in a.functions.iter().zip(b.functions.iter()) {
        if func_a.name != func_b.name {
            return false;
        }
        if func_a.params != func_b.params {
            return false;
        }
        if func_a.return_type != func_b.return_type {
            return false;
        }

        // Compare function bodies using debug representation for determinism
        let body_a_debug = format!("{:?}", func_a.body);
        let body_b_debug = format!("{:?}", func_b.body);
        if body_a_debug != body_b_debug {
            return false;
        }
    }

    true
}

/// Parse and analyze source code, returning HIR
fn parse_and_get_hir(source: &str) -> Result<hir::HirProgram, SemanticError> {
    let ast = rue_parser::parse_with_recovery(source, "test.rue").map_err(|diagnostics| {
        // Convert first diagnostic to SemanticError for compatibility
        if let Some(first) = diagnostics.first() {
            SemanticError {
                message: first.message.clone(),
                span: first.primary_span().unwrap_or(rue_lexer::Span::new(0, 0)),
            }
        } else {
            SemanticError {
                message: "Parse error".to_string(),
                span: rue_lexer::Span::new(0, 0),
            }
        }
    })?;
    let analysis = analyze_cst(&ast)?;
    Ok(analysis.hir)
}

/// Test that HIR generation is deterministic for simple functions
#[test]
fn test_hir_roundtrip_simple_function() {
    let source = r#"
fn main() -> i32 {
    42
}
"#;

    // Generate HIR multiple times
    let hir1 = parse_and_get_hir(source).expect("First HIR generation failed");
    let hir2 = parse_and_get_hir(source).expect("Second HIR generation failed");
    let hir3 = parse_and_get_hir(source).expect("Third HIR generation failed");

    // All HIR should be identical
    assert!(
        hir_programs_equal(&hir1, &hir2),
        "First and second HIR differ"
    );
    assert!(
        hir_programs_equal(&hir2, &hir3),
        "Second and third HIR differ"
    );
    assert!(
        hir_programs_equal(&hir1, &hir3),
        "First and third HIR differ"
    );

    // Verify basic structure
    assert_eq!(hir1.functions.len(), 1);
    assert_eq!(hir1.functions[0].name, "main");
}

/// Test HIR roundtrip for functions with parameters
#[test]
fn test_hir_roundtrip_with_parameters() {
    let source = r#"
fn add(x: i32, y: i32) -> i32 {
    x + y
}
"#;

    let hir1 = parse_and_get_hir(source).expect("First HIR generation failed");
    let hir2 = parse_and_get_hir(source).expect("Second HIR generation failed");

    assert!(
        hir_programs_equal(&hir1, &hir2),
        "HIR roundtrip failed for function with parameters"
    );

    // Verify parameter structure
    assert_eq!(hir1.functions[0].params.len(), 2);
    assert_eq!(hir1.functions[0].params[0].0, "x");
    assert_eq!(hir1.functions[0].params[1].0, "y");
}

/// Test HIR roundtrip for recursive functions
#[test]
fn test_hir_roundtrip_recursive_function() {
    let source = r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}
"#;

    let hir1 = parse_and_get_hir(source).expect("First HIR generation failed");
    let hir2 = parse_and_get_hir(source).expect("Second HIR generation failed");

    assert!(
        hir_programs_equal(&hir1, &hir2),
        "HIR roundtrip failed for recursive function"
    );

    // Verify function structure
    assert_eq!(hir1.functions[0].name, "factorial");
    assert_eq!(hir1.functions[0].params.len(), 1);
}

/// Test HIR roundtrip for functions with let bindings
#[test]
fn test_hir_roundtrip_with_let_bindings() {
    let source = r#"
fn fibonacci(n: i32) -> i32 {
    if n <= 1 {
        n
    } else {
        let left: i32 = fibonacci(n - 1);
        let right: i32 = fibonacci(n - 2);
        left + right
    }
}
"#;

    let hir1 = parse_and_get_hir(source).expect("First HIR generation failed");
    let hir2 = parse_and_get_hir(source).expect("Second HIR generation failed");

    assert!(
        hir_programs_equal(&hir1, &hir2),
        "HIR roundtrip failed for function with let bindings"
    );

    // Verify function structure
    assert_eq!(hir1.functions[0].name, "fibonacci");
}

/// Test HIR roundtrip for functions with while loops  
#[test]
fn test_hir_roundtrip_with_while_loops() {
    let source = r#"
fn countdown(n: i32) -> i32 {
    let counter: i32 = n;
    while counter > 0 {
        counter = counter - 1;
    };
    counter
}
"#;

    let hir1 = parse_and_get_hir(source).expect("First HIR generation failed");
    let hir2 = parse_and_get_hir(source).expect("Second HIR generation failed");

    assert!(
        hir_programs_equal(&hir1, &hir2),
        "HIR roundtrip failed for function with while loops"
    );
}

/// Test HIR roundtrip for complex expressions
#[test]
fn test_hir_roundtrip_complex_expressions() {
    let source = r#"
fn complex_math(a: i32, b: i32, c: i32) -> i32 {
    let result: i32 = (a + b) * c - (a - b) / (c + 1);
    if result > 0 {
        result * 2
    } else {
        -result
    }
}
"#;

    let hir1 = parse_and_get_hir(source).expect("First HIR generation failed");
    let hir2 = parse_and_get_hir(source).expect("Second HIR generation failed");

    assert!(
        hir_programs_equal(&hir1, &hir2),
        "HIR roundtrip failed for complex expressions"
    );
}

/// Test HIR roundtrip for multiple functions
#[test]
fn test_hir_roundtrip_multiple_functions() {
    let source = r#"
fn add(x: i32, y: i32) -> i32 {
    x + y
}

fn multiply(x: i32, y: i32) -> i32 {
    x * y
}

fn main() -> i32 {
    add(multiply(2, 3), 4)
}
"#;

    let hir1 = parse_and_get_hir(source).expect("First HIR generation failed");
    let hir2 = parse_and_get_hir(source).expect("Second HIR generation failed");

    assert!(
        hir_programs_equal(&hir1, &hir2),
        "HIR roundtrip failed for multiple functions"
    );

    // Verify all functions are present
    assert_eq!(hir1.functions.len(), 3);
    let func_names: Vec<&str> = hir1.functions.iter().map(|f| f.name.as_str()).collect();
    assert!(func_names.contains(&"add"));
    assert!(func_names.contains(&"multiply"));
    assert!(func_names.contains(&"main"));
}

/// Test HIR roundtrip with different literal types
#[test]
fn test_hir_roundtrip_literal_types() {
    let source = r#"
fn test_literals() -> bool {
    let int_val: i32 = 42;
    let bool_val: bool = true;
    let unit_val: () = ();
    bool_val
}
"#;

    let hir1 = parse_and_get_hir(source).expect("First HIR generation failed");
    let hir2 = parse_and_get_hir(source).expect("Second HIR generation failed");

    assert!(
        hir_programs_equal(&hir1, &hir2),
        "HIR roundtrip failed for different literal types"
    );
}

/// Test HIR determinism across many iterations
#[test]
fn test_hir_determinism_stress() {
    let source = r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn fibonacci(n: i32) -> i32 {
    if n <= 1 {
        n
    } else {
        fibonacci(n - 1) + fibonacci(n - 2)
    }
}

fn main() -> i32 {
    factorial(5) + fibonacci(8)
}
"#;

    // Generate HIR 10 times and verify all are identical
    let mut hir_instances = Vec::new();
    for i in 0..10 {
        let hir = parse_and_get_hir(source)
            .unwrap_or_else(|e| panic!("HIR generation {} failed: {}", i, e.message));
        hir_instances.push(hir);
    }

    // Compare each instance with the first one
    for (i, hir) in hir_instances.iter().enumerate().skip(1) {
        assert!(
            hir_programs_equal(&hir_instances[0], hir),
            "HIR instance {i} differs from the first instance"
        );
    }
}

/// Test that HIR structure is consistent - basic determinism test
#[test]
fn test_hir_basic_determinism() {
    let source = r#"
fn main() -> i32 {
    42
}
"#;

    // Parse the same source multiple times
    let hir1 = parse_and_get_hir(source).expect("First HIR generation failed");
    let hir2 = parse_and_get_hir(source).expect("Second HIR generation failed");
    let hir3 = parse_and_get_hir(source).expect("Third HIR generation failed");

    // All should produce identical HIR
    assert!(
        hir_programs_equal(&hir1, &hir2),
        "HIR generation is not deterministic (first vs second)"
    );
    assert!(
        hir_programs_equal(&hir1, &hir3),
        "HIR generation is not deterministic (first vs third)"
    );
    assert!(
        hir_programs_equal(&hir2, &hir3),
        "HIR generation is not deterministic (second vs third)"
    );
}
