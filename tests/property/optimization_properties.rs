//! Property-based tests for optimization correctness

use proptest::prelude::*;
use rue_test_utils::assert_runs_with_exit_code;

proptest! {
    #[test]
    fn test_constant_folding_preserves_result(a: i8, b: i8) {
        // Avoid overflow in exit codes (0-255)
        let a = (a as i32).abs() % 50;
        let b = (b as i32).abs() % 50;
        let expected = (a + b) % 256;
        
        // Program with constants that can be folded
        let source = format!(
            "fn main() -> i32 {{ {} + {} }}", 
            a, b
        );
        
        // Should produce the same result with or without optimization
        assert_runs_with_exit_code(&source, expected).unwrap();
    }
    
    #[test]
    fn test_dead_code_elimination_preserves_result(
        used_value: i8,
        dead_value1: i8,
        dead_value2: i8
    ) {
        let used = (used_value as i32).abs() % 100;
        
        let source = format!(r#"
            fn main() -> i32 {{
                let used = {};
                let dead1 = {};  // Never used
                let dead2 = {};  // Never used
                used
            }}
        "#, used, dead_value1, dead_value2);
        
        // Should return only the used value
        assert_runs_with_exit_code(&source, used).unwrap();
    }
    
    #[test]
    fn test_arithmetic_optimization_preserves_result(x: i8, y: i8) {
        let x = (x as i32).abs() % 20;
        let y = (y as i32).abs() % 20;
        
        // Various equivalent forms that might be optimized differently
        let sources = vec![
            // Original
            format!("fn main() -> i32 {{ {} * {} + {} * {} }}", x, y, x, y),
            // Factored
            format!("fn main() -> i32 {{ 2 * ({} * {}) }}", x, y),
            // Expanded
            format!("fn main() -> i32 {{ {} * {} + {} * {} }}", x, y, x, y),
        ];
        
        // All forms should produce the same result
        let expected = (2 * x * y) % 256;
        
        for source in sources {
            assert_runs_with_exit_code(&source, expected).unwrap();
        }
    }
    
    #[test]
    fn test_control_flow_optimization_preserves_result(condition: bool, a: i8, b: i8) {
        let a = (a as i32).abs() % 100;
        let b = (b as i32).abs() % 100;
        
        let source = format!(r#"
            fn main() -> i32 {{
                if {} {{
                    {}
                }} else {{
                    {}
                }}
            }}
        "#, condition, a, b);
        
        let expected = if condition { a } else { b };
        assert_runs_with_exit_code(&source, expected).unwrap();
    }
}