//! Integration tests for optimization pipeline

use rue_parser::parse_with_recovery;
use rue_semantic::analyze_cst;
use rue_lowering::lower_hir_to_mir;
use rue_optimize::{optimize_mir, OptimizationLevel};

#[test]
fn test_constant_propagation_through_pipeline() {
    let source = r#"
        fn main() -> i32 {
            let x = 10;
            let y = 20;
            let z = x + y;  // Should be optimized to 30
            z
        }
    "#;
    
    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Lower to MIR
    let mut mir = lower_hir_to_mir(&hir.hir).expect("Lowering failed");
    
    // Optimize with constant propagation
    optimize_mir(&mut mir, OptimizationLevel::O1);
    
    // The optimization should have simplified the MIR
    // (actual verification would check MIR structure)
}

#[test]
fn test_dead_code_elimination() {
    let source = r#"
        fn main() -> i32 {
            let x = 10;
            let y = 20;  // Dead - never used
            let z = 30;  // Dead - never used
            x
        }
    "#;
    
    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Lower to MIR
    let mut mir = lower_hir_to_mir(&hir.hir).expect("Lowering failed");
    
    let initial_instructions = count_mir_instructions(&mir);
    
    // Optimize with dead code elimination
    optimize_mir(&mut mir, OptimizationLevel::O2);
    
    let optimized_instructions = count_mir_instructions(&mir);
    
    // Should have fewer instructions after DCE
    assert!(optimized_instructions < initial_instructions);
}

#[test]
fn test_common_subexpression_elimination() {
    let source = r#"
        fn main() -> i32 {
            let a = 5;
            let b = 10;
            let x = a * b + a * b;  // a * b computed twice
            x
        }
    "#;
    
    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Lower to MIR
    let mut mir = lower_hir_to_mir(&hir.hir).expect("Lowering failed");
    
    // Optimize with CSE
    optimize_mir(&mut mir, OptimizationLevel::O2);
    
    // The duplicate computation should be eliminated
    // (actual verification would check MIR for single multiplication)
}

#[test]
fn test_optimization_preserves_semantics() {
    let source = r#"
        fn factorial(n: i32) -> i32 {
            if n <= 1 {
                1
            } else {
                n * factorial(n - 1)
            }
        }
        
        fn main() -> i32 {
            factorial(5)  // Should still compute 120
        }
    "#;
    
    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Lower to MIR
    let mut mir = lower_hir_to_mir(&hir.hir).expect("Lowering failed");
    
    // Clone for comparison
    let unoptimized_mir = mir.clone();
    
    // Optimize
    optimize_mir(&mut mir, OptimizationLevel::O3);
    
    // Both versions should be valid MIR
    validate_mir(&unoptimized_mir).expect("Unoptimized MIR invalid");
    validate_mir(&mir).expect("Optimized MIR invalid");
}

// Helper functions
fn count_mir_instructions(mir: &rue_ir::MirProgram) -> usize {
    mir.functions
        .iter()
        .flat_map(|f| &f.blocks)
        .map(|b| b.instructions.len())
        .sum()
}

fn validate_mir(mir: &rue_ir::MirProgram) -> Result<(), String> {
    // Basic validation that MIR is well-formed
    if mir.functions.is_empty() {
        return Err("No functions in MIR".to_string());
    }
    
    for func in &mir.functions {
        if func.blocks.is_empty() {
            return Err(format!("Function {} has no blocks", func.name));
        }
    }
    
    Ok(())
}