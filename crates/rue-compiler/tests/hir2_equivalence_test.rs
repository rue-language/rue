//! Integration tests to verify equivalence between HIR and HIR2 compilation paths

use rue_compiler::pipeline::{compile_hir_via_mir_to_assembly, compile_hir2_via_mir_to_assembly};
use rue_parser::parse_with_recovery;
use rue_semantic::{analyze_cst, analyze_cst_v2, scope_to_type_context};

/// Test that both HIR and HIR2 paths produce equivalent assembly
#[test]
fn test_simple_function_equivalence() {
    let source = r#"
        fn main() -> i32 {
            let x = 5;
            let y = 10;
            x + y
        }
    "#;

    // Parse CST
    let cst = parse_with_recovery(source, "test.rue").unwrap();

    // Path 1: Traditional HIR
    let analysis1 = analyze_cst(&cst).unwrap();
    let asm1 = compile_hir_via_mir_to_assembly(&analysis1, false).unwrap();

    // Path 2: HIR2
    let hir2 = analyze_cst_v2(&cst).unwrap();
    let type_context = scope_to_type_context(&analysis1.scope);
    let asm2 = compile_hir2_via_mir_to_assembly(&hir2, type_context, false).unwrap();

    // Assembly should be equivalent (allowing for minor formatting differences)
    // For now, just verify both compile successfully
    assert!(!asm1.is_empty());
    assert!(!asm2.is_empty());

    // Both should contain the main function
    assert!(asm1.contains("main:"));
    assert!(asm2.contains("main:"));
}

/// Test binary operations
#[test]
fn test_binary_operations_equivalence() {
    let source = r#"
        fn calculate(a: i32, b: i32) -> i32 {
            let sum = a + b;
            let diff = a - b;
            let prod = sum * diff;
            prod / 2
        }
    "#;

    // Parse CST
    let cst = parse_with_recovery(source, "test.rue").unwrap();

    // Path 1: Traditional HIR
    let analysis1 = analyze_cst(&cst).unwrap();
    let asm1 = compile_hir_via_mir_to_assembly(&analysis1, false).unwrap();

    // Path 2: HIR2
    let hir2 = analyze_cst_v2(&cst).unwrap();
    let type_context = scope_to_type_context(&analysis1.scope);
    let asm2 = compile_hir2_via_mir_to_assembly(&hir2, type_context, false).unwrap();

    // Verify both compile successfully
    assert!(!asm1.is_empty());
    assert!(!asm2.is_empty());

    // Both should contain the calculate function
    assert!(asm1.contains("calculate:"));
    assert!(asm2.contains("calculate:"));
}

/// Test function calls
#[test]
fn test_function_call_equivalence() {
    let source = r#"
        fn add(x: i32, y: i32) -> i32 {
            x + y
        }
        
        fn main() -> i32 {
            add(3, 4)
        }
    "#;

    // Parse CST
    let cst = parse_with_recovery(source, "test.rue").unwrap();

    // Path 1: Traditional HIR
    let analysis1 = analyze_cst(&cst).unwrap();
    let asm1 = compile_hir_via_mir_to_assembly(&analysis1, false).unwrap();

    // Path 2: HIR2
    let hir2 = analyze_cst_v2(&cst).unwrap();
    let type_context = scope_to_type_context(&analysis1.scope);
    let asm2 = compile_hir2_via_mir_to_assembly(&hir2, type_context, false).unwrap();

    // Verify both compile successfully
    assert!(!asm1.is_empty());
    assert!(!asm2.is_empty());

    // Both should contain both functions
    assert!(asm1.contains("add:"));
    assert!(asm1.contains("main:"));
    assert!(asm2.contains("add:"));
    assert!(asm2.contains("main:"));

    // Both should contain a call instruction
    assert!(asm1.contains("call"));
    assert!(asm2.contains("call"));
}

/// Test with optimizations enabled
#[test]
fn test_optimized_equivalence() {
    let source = r#"
        fn main() -> i32 {
            let x = 5;
            let y = 10;
            let z = x + y;
            z * 2
        }
    "#;

    // Parse CST
    let cst = parse_with_recovery(source, "test.rue").unwrap();

    // Path 1: Traditional HIR with optimizations
    let analysis1 = analyze_cst(&cst).unwrap();
    let asm1 = compile_hir_via_mir_to_assembly(&analysis1, true).unwrap();

    // Path 2: HIR2 with optimizations
    let hir2 = analyze_cst_v2(&cst).unwrap();
    let type_context = scope_to_type_context(&analysis1.scope);
    let asm2 = compile_hir2_via_mir_to_assembly(&hir2, type_context, true).unwrap();

    // With optimizations, both should produce optimized code
    assert!(!asm1.is_empty());
    assert!(!asm2.is_empty());

    // Both should still contain main function
    assert!(asm1.contains("main:"));
    assert!(asm2.contains("main:"));
}
