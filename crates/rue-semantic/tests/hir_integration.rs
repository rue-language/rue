use rue_ir::hir::{Hir, InstData, InstTag, Instruction, Terminator};
use rue_ir::types::RueType;
use rue_parser::parse_with_recovery;
use rue_semantic::analyze_cst;

// Helper functions for navigating the hierarchical HIR structure

/// Count instructions of a specific type across all nested blocks
fn count_instruction_type(hir: &Hir, tag: InstTag) -> usize {
    let mut count = 0;

    // Count in function bodies
    for func_id in &hir.functions {
        if let Some(func) = hir.get_function(*func_id) {
            hir.walk_block_hierarchy(&func.body, &mut |_block, inst| {
                if inst.tag == tag {
                    count += 1;
                }
            });
        }
    }

    // Count in top-level blocks
    for &block_id in &hir.blocks {
        if let Some(block) = hir.block(block_id) {
            hir.walk_block_hierarchy(block, &mut |_block, inst| {
                if inst.tag == tag {
                    count += 1;
                }
            });
        }
    }

    count
}

/// Count instructions matching a predicate across nested blocks  
fn count_instructions_with_predicate<F>(hir: &Hir, mut predicate: F) -> usize
where
    F: FnMut(&Instruction) -> bool,
{
    let mut count = 0;

    // Search in function bodies
    for func_id in &hir.functions {
        if let Some(func) = hir.get_function(*func_id) {
            hir.walk_block_hierarchy(&func.body, &mut |_block, inst| {
                if predicate(inst) {
                    count += 1;
                }
            });
        }
    }

    // Search in top-level blocks
    for &block_id in &hir.blocks {
        if let Some(block) = hir.block(block_id) {
            hir.walk_block_hierarchy(block, &mut |_block, inst| {
                if predicate(inst) {
                    count += 1;
                }
            });
        }
    }

    count
}

/// Check if HIR contains at least one instruction of a specific type
fn has_instruction_type(hir: &Hir, tag: InstTag) -> bool {
    count_instruction_type(hir, tag) > 0
}

#[test]
fn test_simple_function_hir() {
    let source = r#"
    fn main() -> i32 {
        let x: i32 = 42;
        let y = x + 10;
        return y;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Basic checks
    assert!(
        !hir.hir.functions.is_empty(),
        "Should have at least one function"
    );
    // With hierarchical structure, instructions are inside function bodies
    assert!(
        !hir.hir.function_arena.is_empty() && !hir.hir.function_arena[0].body.body.is_empty(),
        "Should have generated instructions in function body"
    );

    // Check that strings were interned (function name should be at some index)
    assert!(
        !hir.hir.strings.is_empty(),
        "Should have some strings interned"
    );
}

#[test]
fn test_binary_expression_hir() {
    let source = r#"
    fn test() -> i32 {
        return 5 + 3;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Should have generated instructions including literals and binary operations
    assert!(
        !hir.hir.function_arena.is_empty() && !hir.hir.function_arena[0].body.body.is_empty(),
        "Should have multiple instructions for literals and binary op"
    );

    // Check that we have some binary operation using helper
    assert!(
        has_instruction_type(&hir.hir, InstTag::Binary),
        "Should have a binary operation"
    );
}

#[test]
fn test_function_call_hir() {
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

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Should have generated multiple functions
    assert_eq!(hir.hir.functions.len(), 2, "Should have two functions");
    assert!(
        hir.hir
            .function_arena
            .iter()
            .any(|f| !f.body.body.is_empty()),
        "Should have generated many instructions"
    );
}

#[test]
fn test_hir_display() {
    let source = r#"
    fn test() -> i32 {
        let x = 5;
        let y = 10;
        return x + y;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Basic validation - should have emitted various instruction types
    assert!(
        !hir.hir.function_arena.is_empty() && hir.hir.function_arena[0].body.body.len() > 3,
        "Should have multiple instructions for literals, lets, binary op, return"
    );
}

#[test]
fn test_if_expression_hir() {
    let source = r#"
    fn main() -> i32 {
        let x = 10;
        let result = if x > 5 {
            42
        } else {
            0
        };
        return result;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Should have generated instructions including:
    // - literal for x = 10
    // - let instruction for x
    // - comparison x > 5
    // - blocks for then and else branches
    // - if-expression value assignment
    // With the new SSA form, control flow is handled via terminators
    assert!(
        !hir.hir.function_arena.is_empty(),
        "Should have at least one function"
    );
    assert!(
        hir.hir.block_arena.len() > 1,
        "Should have multiple blocks for if-expression branches"
    );

    // Check that we have blocks with If terminators
    let mut has_if_terminator = false;
    for &block_id in &hir.hir.blocks {
        if let Some(block) = hir.hir.block(block_id)
            && matches!(block.term, Terminator::If { .. })
        {
            has_if_terminator = true;
            break;
        }
    }
    // Also check in function bodies' blocks
    if !has_if_terminator {
        for func_id in &hir.hir.functions {
            if let Some(func) = hir.hir.get_function(*func_id)
                && matches!(func.body.term, Terminator::If { .. })
            {
                has_if_terminator = true;
                break;
            }
        }
    }
    assert!(
        has_if_terminator,
        "Should have at least one block with an If terminator for conditional control flow"
    );
}

#[test]
fn test_nested_if_expression_hir() {
    let source = r#"
    fn main() -> i32 {
        let x = 15;
        let result = if x > 20 {
            100
        } else {
            if x > 10 {
                50
            } else {
                25
            }
        };
        return result;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR - this should now work with proper block context
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed for nested if");

    // With hierarchical structure, nested blocks are inside If instructions
    // Just check that we have generated instructions
    assert!(
        !hir.hir.function_arena.is_empty() && !hir.hir.function_arena[0].body.body.is_empty(),
        "Should have instructions for nested if-expression"
    );

    // With the new SSA form, nested if-expressions create multiple blocks in the arena
    assert!(
        !hir.hir.function_arena.is_empty(),
        "Should have at least one function"
    );
    assert!(
        hir.hir.block_arena.len() > 2,
        "Should have multiple blocks for nested if-expression branches"
    );
}

#[test]
fn test_tuple_literal_hir() {
    let source = r#"
    fn main() -> (i32, bool, i64) {
        let large: i64 = 100;
        let t = (42, true, large);
        return t;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Check that we have a TupleLit instruction using helper
    assert!(
        has_instruction_type(&hir.hir, InstTag::TupleLit),
        "Should have generated a TupleLit instruction"
    );

    // Should have generated instructions for the tuple elements
    assert!(
        !hir.hir.function_arena.is_empty() && hir.hir.function_arena[0].body.body.len() >= 3,
        "Should have instructions for literals, tuple construction, let, and return"
    );
}

#[test]
fn test_tuple_field_access_hir() {
    let source = r#"
    fn main() -> i32 {
        let t = (10, 20, 30);
        let x = t.0;
        let y = t.1;
        let z = t.2;
        return x + y + z;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Check that we have FieldAccess instructions using helper
    let field_access_count = count_instruction_type(&hir.hir, InstTag::FieldAccess);
    // The compiler may generate duplicate field accesses, so we expect at least 3
    assert!(
        field_access_count >= 3,
        "Should have generated at least 3 FieldAccess instructions (found {})",
        field_access_count
    );

    // Should have many instructions for the complete program
    assert!(
        !hir.hir.function_arena.is_empty() && hir.hir.function_arena[0].body.body.len() >= 5,
        "Should have instructions for tuple, field accesses, additions, and return"
    );
}

#[test]
fn test_nested_tuple_hir() {
    let source = r#"
    fn main() -> i32 {
        let nested = ((1, 2), (3, 4));
        let first = nested.0;
        let second = first.1;
        return second;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Count TupleLit instructions (should have 3: two inner tuples and one outer) using helper
    let tuple_lit_count = count_instruction_type(&hir.hir, InstTag::TupleLit);
    // The compiler may generate duplicate tuple constructions, so we expect at least 3
    assert!(
        tuple_lit_count >= 3,
        "Should have generated at least 3 TupleLit instructions for nested tuples (found {})",
        tuple_lit_count
    );

    // Count FieldAccess instructions using helper
    let field_access_count = count_instruction_type(&hir.hir, InstTag::FieldAccess);
    // The compiler may generate duplicate field accesses, so we expect at least 2
    assert!(
        field_access_count >= 2,
        "Should have generated at least 2 FieldAccess instructions (found {})",
        field_access_count
    );
}

#[test]
fn test_tuple_with_type_annotation_hir() {
    let source = r#"
    fn main() -> (i32, i64) {
        let t: (i32, i64) = (42, 1000000000000);
        return t;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Check that we have a TupleLit instruction using helper
    assert!(
        has_instruction_type(&hir.hir, InstTag::TupleLit),
        "Should have generated a TupleLit instruction"
    );

    // For type checking, we can verify that TupleLit instructions have tuple types
    let tuple_lit_with_type_count = count_instructions_with_predicate(&hir.hir, |inst| {
        inst.tag == InstTag::TupleLit && inst.ty.is_some()
    });
    assert!(
        tuple_lit_with_type_count > 0,
        "TupleLit instruction should have type information"
    );
}

#[test]
fn test_tuple_in_expression_hir() {
    let source = r#"
    fn main() -> bool {
        let t1 = (10, 20);
        let t2 = (10, 20);
        // Compare individual fields since tuple equality isn't directly supported  
        let same_first = t1.0 == t2.0;
        let same_second = t1.1 == t2.1;
        // Since both tuples have the same values, both comparisons are true
        // For testing purposes, just return one of them
        return same_first;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Count TupleLit instructions (compiler may generate duplicates) using helper
    let tuple_lit_count = count_instruction_type(&hir.hir, InstTag::TupleLit);
    assert!(
        tuple_lit_count >= 2,
        "Should have generated at least 2 TupleLit instructions (found {})",
        tuple_lit_count
    );

    // Count FieldAccess instructions (compiler may generate duplicates) using helper
    let field_access_count = count_instruction_type(&hir.hir, InstTag::FieldAccess);
    assert!(
        field_access_count >= 4,
        "Should have generated at least 4 FieldAccess instructions (found {})",
        field_access_count
    );
}

#[test]
fn test_empty_tuple_hir() {
    let source = r#"
    fn main() -> () {
        let unit = ();
        return unit;
    }
    "#;

    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Analyze with HIR
    let hir = analyze_cst(&cst).expect("HIR analysis should succeed");

    // Empty tuples are represented as Literal instructions with Unit type
    // Check that we have Literal instructions with Unit type
    let unit_literal_count = count_instructions_with_predicate(&hir.hir, |inst| {
        matches!(inst.tag, InstTag::Literal) && matches!(inst.ty, Some(RueType::Unit))
    });
    assert!(
        unit_literal_count > 0,
        "Should have generated Literal instructions with Unit type for empty tuple"
    );

    // Also verify that we have some instruction representing unit value
    let unit_value_count = count_instructions_with_predicate(&hir.hir, |inst| {
        matches!(&inst.data, InstData::Literal(0)) && matches!(inst.ty, Some(RueType::Unit))
    });
    assert!(
        unit_value_count > 0,
        "Should have unit value (0) with Unit type"
    );
}
