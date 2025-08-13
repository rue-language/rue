//! Property-based tests for type system soundness

use proptest::prelude::*;
use rue_test_utils::{assert_compiles, assert_compile_error};

proptest! {
    #[test]
    fn test_integer_literal_type_inference(n: i32) {
        let source = format!(r#"
            fn main() -> i32 {{
                let x = {};
                x
            }}
        "#, n);
        
        // Should always compile - integer literals default to i32
        assert_compiles(&source).unwrap();
    }
    
    #[test]
    fn test_type_annotation_enforced(n: i32) {
        // Trying to assign i32 to bool should fail
        let source = format!(r#"
            fn main() -> i32 {{
                let x: bool = {};
                0
            }}
        "#, n);
        
        assert_compile_error(&source, "type").unwrap();
    }
    
    #[test]
    fn test_binary_operator_type_checking(a: i8, b: i8, op in "[+\\-*/]") {
        let source = format!(r#"
            fn main() -> i32 {{
                let a: i32 = {};
                let b: i32 = {};
                a {} b
            }}
        "#, a as i32, b as i32, op);
        
        // Arithmetic operators on same types should compile
        assert_compiles(&source).unwrap();
        
        // Mixed types should fail
        let mixed_source = format!(r#"
            fn main() -> i32 {{
                let a: i32 = {};
                let b: i64 = {};
                a {} b
            }}
        "#, a as i32, b as i32, op);
        
        assert_compile_error(&mixed_source, "type").unwrap();
    }
    
    #[test]
    fn test_comparison_returns_bool(a: i8, b: i8, op in prop::sample::select(vec!["<", ">", "<=", ">=", "==", "!="])) {
        let source = format!(r#"
            fn main() -> i32 {{
                let a = {};
                let b = {};
                let result: bool = a {} b;
                if result {{ 1 }} else {{ 0 }}
            }}
        "#, a as i32, b as i32, op);
        
        // Comparison operators should return bool
        assert_compiles(&source).unwrap();
    }
    
    #[test]
    fn test_if_condition_must_be_bool(n: i32) {
        // Using bool condition should work
        let bool_source = format!(r#"
            fn main() -> i32 {{
                if {} > 0 {{
                    1
                }} else {{
                    0
                }}
            }}
        "#, n);
        
        assert_compiles(&bool_source).unwrap();
        
        // Using non-bool should fail
        let int_source = format!(r#"
            fn main() -> i32 {{
                if {} {{
                    1
                }} else {{
                    0
                }}
            }}
        "#, n);
        
        assert_compile_error(&int_source, "bool").unwrap();
    }
}