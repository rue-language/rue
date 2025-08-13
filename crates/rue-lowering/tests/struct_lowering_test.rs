// Integration test for struct lowering from HIR to MIR

use rue_ir::mir::MirValue;
use rue_ir::types::RueType;
use rue_lowering::lower_hir_to_mir;
use rue_parser::parse_with_recovery;
use rue_semantic::{analyze_cst, scope_to_type_context};

#[test]
fn test_struct_literal_lowering_to_mir() {
    let source = r#"
        struct Point {
            x: i32,
            y: i32,
        }

        fn main() -> i32 {
            let p = Point { x: 10, y: 20 };
            p.x + p.y
        }
    "#;

    // Parse the source code
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Run semantic analysis with HIR generation
    let analysis_result = analyze_cst(&cst).expect("Failed to analyze CST");

    // Create type context from scope
    let type_ctx = scope_to_type_context(&analysis_result.scope);

    // Lower HIR to MIR
    let mir = lower_hir_to_mir(&analysis_result.hir, type_ctx);

    // Verify MIR was generated
    assert!(
        !mir.functions.is_empty(),
        "Should have at least one function"
    );

    // Check that we have a ConstructAggregate instruction for the struct literal
    let main_func = mir.functions.iter().find(|f| f.name == "main");
    assert!(main_func.is_some(), "Should have main function in MIR");

    let main = main_func.unwrap();
    assert!(!main.blocks.is_empty(), "Main function should have blocks");

    // Look for ConstructAggregate in the statements
    let mut found_struct_construct = false;
    let mut found_field_access = false;

    for block in &main.blocks {
        for stmt in &block.statements {
            if let rue_ir::mir::MirStatement::Assign { value, .. } = stmt {
                match value {
                    MirValue::ConstructAggregate { ty, fields } => {
                        if matches!(ty, RueType::Struct(_)) {
                            found_struct_construct = true;
                            assert_eq!(fields.len(), 2, "Point struct should have 2 fields");
                        }
                    }
                    MirValue::GetField { .. } => {
                        found_field_access = true;
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(
        found_struct_construct,
        "Should have found struct construction in MIR"
    );
    assert!(found_field_access, "Should have found field access in MIR");
}

#[test]
fn test_empty_struct_lowering() {
    let source = r#"
        struct Empty {
        }

        fn main() -> i32 {
            let e = Empty {};
            42
        }
    "#;

    // Parse the source code
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Run semantic analysis with HIR generation
    let analysis_result = analyze_cst(&cst).expect("Failed to analyze CST");

    // Create type context from scope
    let type_ctx = scope_to_type_context(&analysis_result.scope);

    // Lower HIR to MIR - should handle empty structs
    let mir = lower_hir_to_mir(&analysis_result.hir, type_ctx);

    // Verify MIR was generated
    assert!(
        !mir.functions.is_empty(),
        "Should have at least one function"
    );
}
