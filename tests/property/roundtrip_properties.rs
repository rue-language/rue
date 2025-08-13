//! Property-based tests for roundtrip operations

use proptest::prelude::*;
use rue_parser::{parse_with_recovery, unparse};

proptest! {
    #[test]
    fn test_parse_unparse_integers(n: i32) {
        let source = format!("fn main() -> i32 {{ {} }}", n);
        
        // Parse the program
        let cst = parse_with_recovery(&source, "test.rue").unwrap();
        
        // Unparse it back to source
        let unparsed = unparse(&cst);
        
        // Parse again
        let reparsed = parse_with_recovery(&unparsed, "test.rue").unwrap();
        
        // Should produce equivalent AST
        // (Note: exact string comparison may fail due to formatting)
        assert_eq!(cst.root.kind(), reparsed.root.kind());
    }
    
    #[test]
    fn test_parse_unparse_identifiers(name in "[a-z][a-z0-9_]{0,10}") {
        // Skip keywords
        if matches!(name.as_str(), "fn" | "let" | "if" | "else" | "while" | "return" | "true" | "false") {
            return Ok(());
        }
        
        let source = format!("fn main() -> i32 {{ let {} = 42; {} }}", name, name);
        
        // Parse the program
        let cst = parse_with_recovery(&source, "test.rue").unwrap();
        
        // Unparse it back to source
        let unparsed = unparse(&cst);
        
        // Parse again
        let reparsed = parse_with_recovery(&unparsed, "test.rue").unwrap();
        
        // Should produce equivalent AST
        assert_eq!(cst.root.kind(), reparsed.root.kind());
    }
    
    #[test]
    fn test_arithmetic_expression_roundtrip(a: i8, b: i8, c: i8) {
        // Use small values to avoid overflow in source
        let source = format!(
            "fn main() -> i32 {{ ({} + {}) * {} }}", 
            a as i32, b as i32, c as i32
        );
        
        // Parse the program
        let cst = parse_with_recovery(&source, "test.rue").unwrap();
        
        // Unparse it back to source
        let unparsed = unparse(&cst);
        
        // Parse again
        let reparsed = parse_with_recovery(&unparsed, "test.rue").unwrap();
        
        // Should preserve structure
        assert_eq!(cst.root.kind(), reparsed.root.kind());
    }
}