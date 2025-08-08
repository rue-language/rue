use rue_parser::parse_with_recovery;
use rue_semantic::analyze_cst_v2;

#[test]
fn test_simple_function_hir2() {
    let source = r#"
    fn main() -> i32 {
        let x: i32 = 42;
        let y = x + 10;
        return y;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR2
    let hir2 = analyze_cst_v2(&cst).expect("HIR2 analysis should succeed");

    // Basic checks
    assert!(
        hir2.instructions.len() > 1,
        "Should have generated instructions"
    );
    assert!(
        !hir2.functions.is_empty(),
        "Should have at least one function"
    );

    // Check that strings were interned (function name should be at some index)
    assert!(
        !hir2.strings.is_empty(),
        "Should have some strings interned"
    );
}

#[test]
fn test_binary_expression_hir2() {
    let source = r#"
    fn test() -> i32 {
        return 5 + 3;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR2
    let hir2 = analyze_cst_v2(&cst).expect("HIR2 analysis should succeed");

    // Should have generated instructions including literals and binary operations
    assert!(
        hir2.instructions.len() > 3,
        "Should have multiple instructions for literals and binary op"
    );

    // Check that we have some binary operation in the extra data
    assert!(
        !hir2.extra_data.is_empty(),
        "Should have extra data for operators"
    );
}

#[test]
fn test_function_call_hir2() {
    let source = r#"
    fn add(a: i32, b: i32) -> i32 {
        return a + b;
    }
    
    fn main() -> i32 {
        return add(5, 10);
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR2
    let hir2 = analyze_cst_v2(&cst).expect("HIR2 analysis should succeed");

    // Should have generated multiple functions
    assert_eq!(hir2.functions.len(), 2, "Should have two functions");
    assert!(
        hir2.instructions.len() > 5,
        "Should have generated many instructions"
    );
}

#[test]
fn test_hir2_display() {
    let source = r#"
    fn test() -> i32 {
        let x = 5;
        let y = 10;
        return x + y;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR2
    let hir2 = analyze_cst_v2(&cst).expect("HIR2 analysis should succeed");

    // Print HIR2 for manual inspection
    println!("HIR2 output:");
    println!("{}", hir2);

    // Basic validation - should have emitted various instruction types
    assert!(
        hir2.instructions.len() > 8,
        "Should have multiple instructions for literals, lets, binary op, return"
    );
}
