//! Integration tests for parser → type checker pipeline

use rue_parser::parse_with_recovery;
use rue_semantic::analyze_cst;

#[test]
fn test_parse_and_typecheck_valid_program() {
    let source = r#"
        fn add(x: i32, y: i32) -> i32 {
            x + y
        }
        
        fn main() -> i32 {
            add(10, 20)
        }
    "#;
    
    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Verify we have the expected functions
    assert_eq!(hir.hir.function_arena.len(), 2);
}

#[test]
fn test_parse_success_typecheck_failure() {
    let source = r#"
        fn main() -> i32 {
            let x: i32 = 42;
            let y: i64 = x;  // Type error
            0
        }
    "#;
    
    // Parse should succeed
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check should fail
    let result = analyze_cst(&cst);
    assert!(result.is_err());
    
    let err = result.unwrap_err();
    assert!(err.to_string().contains("Type mismatch") || 
            err.to_string().contains("type"));
}

#[test]
fn test_parse_with_recovery_then_typecheck() {
    let source = r#"
        fn main() -> i32 {
            let x = 42;
            let y = x + ;  // Parse error - missing operand
            x
        }
    "#;
    
    // Parse with recovery should produce a CST even with errors
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type checking should handle the recovered AST
    // It may succeed or fail depending on recovery strategy
    let _ = analyze_cst(&cst);
}

#[test]
fn test_complex_type_flow() {
    let source = r#"
        struct Point { x: i32, y: i32 }
        
        fn distance_squared(p: Point) -> i32 {
            p.x * p.x + p.y * p.y
        }
        
        fn main() -> i32 {
            let origin = Point { x: 0, y: 0 };
            let p = Point { x: 3, y: 4 };
            distance_squared(p)
        }
    "#;
    
    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check with struct support
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Verify struct was processed
    assert!(hir.hir.struct_arena.len() > 0);
}

#[test]
fn test_recursive_function_type_checking() {
    let source = r#"
        fn factorial(n: i32) -> i32 {
            if n <= 1 {
                1
            } else {
                n * factorial(n - 1)
            }
        }
        
        fn main() -> i32 {
            factorial(5)
        }
    "#;
    
    // Parse
    let cst = parse_with_recovery(source, "test.rue").expect("Parse failed");
    
    // Type check - should handle recursive calls
    let hir = analyze_cst(&cst).expect("Type checking failed");
    
    // Verify functions were analyzed
    assert_eq!(hir.hir.function_arena.len(), 2);
}