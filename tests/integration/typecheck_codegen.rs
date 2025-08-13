//! Integration tests for type checker → code generator pipeline

use rue_parser::parse_with_recovery;
use rue_semantic::analyze_cst;
use rue_lowering::lower_hir_to_mir;
use rue_codegen::compile_mir;

#[test]
fn test_typecheck_to_codegen_simple() {
    let source = r#"
        fn main() -> i32 {
            42
        }
    "#;
    
    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Lower to MIR
    let mir = lower_hir_to_mir(&hir.hir).expect("Lowering failed");
    
    // Generate code
    let code = compile_mir(&mir).expect("Code generation failed");
    
    // Verify we got executable code
    assert!(!code.is_empty());
}

#[test]
fn test_type_information_preserved_through_codegen() {
    let source = r#"
        fn add(x: i64, y: i64) -> i64 {
            x + y
        }
        
        fn main() -> i32 {
            let result = add(100, 200);
            to_i32(result)
        }
    "#;
    
    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check - ensures i64 types are tracked
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Lower to MIR - should preserve type information
    let mir = lower_hir_to_mir(&hir.hir).expect("Lowering failed");
    
    // Generate code - should use correct instructions for i64
    let code = compile_mir(&mir).expect("Code generation failed");
    
    // Code should be generated (actual execution test would be in e2e)
    assert!(!code.is_empty());
}

#[test]
fn test_struct_through_pipeline() {
    let source = r#"
        struct Vec2 { x: i32, y: i32 }
        
        fn main() -> i32 {
            let v = Vec2 { x: 10, y: 20 };
            v.x + v.y
        }
    "#;
    
    // Parse struct syntax
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check struct definition and usage
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Lower struct operations
    let mir = lower_hir_to_mir(&hir.hir).expect("Lowering failed");
    
    // Generate code for struct access
    let code = compile_mir(&mir).expect("Code generation failed");
    
    assert!(!code.is_empty());
}

#[test]
fn test_control_flow_through_pipeline() {
    let source = r#"
        fn main() -> i32 {
            let x = 10;
            if x > 5 {
                x * 2
            } else {
                x / 2
            }
        }
    "#;
    
    // Parse control flow
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check branches
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Lower to basic blocks
    let mir = lower_hir_to_mir(&hir.hir).expect("Lowering failed");
    
    // Generate branch instructions
    let code = compile_mir(&mir).expect("Code generation failed");
    
    assert!(!code.is_empty());
}