use rue_parser::parse_with_recovery;
use rue_semantic::analyze_cst;

#[test]
fn test_while_loop_semantic_analysis() {
    let source = r#"
fn main() -> i32 {
    let count: i32 = 5;
    let sum: i32 = 0;
    while count > 0 {
        sum = sum + count;
        count = count - 1;
    };
    sum
}
"#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR - this should succeed with our new implementation
    let hir = analyze_cst(&cst).expect("Semantic analysis should succeed");

    // Verify we have the expected structure
    assert!(
        !hir.hir.function_arena.is_empty(),
        "Should have main function"
    );

    // The HIR should have multiple blocks for the while loop
    assert!(
        hir.hir.block_arena.len() >= 4,
        "Should have at least 4 blocks (entry, header, body, exit)"
    );

    println!("✓ While loop semantic analysis passed!");
}
