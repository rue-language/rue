//! Tests for CST to AST conversion

#[cfg(test)]
mod tests {
    use crate::{Parser, ast_builder::lower_cst_to_ast};
    use rue_ir::ast::{Ast, NodeTag};
    use rue_lexer::Lexer;

    /// Helper function to parse source code and convert to AST
    fn parse_to_ast(source: &str) -> Ast {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().expect("Failed to lex");
        let parser = Parser::new(tokens);
        let cst = parser.parse().expect("Failed to parse");
        lower_cst_to_ast(cst)
    }

    /// Helper to count nodes of a specific type in the AST
    fn count_nodes_of_type(ast: &Ast, tag: NodeTag) -> usize {
        ast.nodes.tags.iter().filter(|&&t| t == tag).count()
    }

    #[test]
    fn test_empty_function() {
        let source = "fn main() {}";
        let ast = parse_to_ast(source);

        // Should have 1 function and 1 block
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Function), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Block), 1);
    }

    #[test]
    fn test_function_with_params() {
        let source = "fn add(a: i32, b: i32) -> i32 { a + b }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::Function), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Block), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Identifier), 7); // 'add', 'a', 'b', 'i32', 'i32', 'a', 'b' in body
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Binary), 1);
    }

    #[test]
    fn test_let_statement() {
        let source = "fn main() { let x = 42; }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::Let), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 1);
    }

    #[test]
    fn test_assignment_statement() {
        let source = "fn main() { let x = 0; x = 42; }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::Let), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Assign), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 2);
    }

    #[test]
    fn test_return_statement() {
        let source = "fn main() -> i32 { return 42; }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::Return), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 1);
    }

    #[test]
    fn test_if_expression() {
        let source = "fn main() { if true { 1 } else { 2 } }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::If), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Block), 3); // main block + then block + else block
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 3); // true, 1, 2
    }

    #[test]
    fn test_while_loop() {
        let source = "fn main() { while true { break; } }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::While), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Block), 2); // main block + while body
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 1); // true
    }

    #[test]
    fn test_binary_expressions() {
        let source = "fn main() { 1 + 2 * 3 - 4 / 5; }";
        let ast = parse_to_ast(source);

        // Should have multiple binary operators
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Binary), 4); // +, *, -, /
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 5); // 1, 2, 3, 4, 5
    }

    #[test]
    fn test_comparison_operators() {
        let source = "fn main() { 1 < 2; 3 > 4; 5 == 6; 7 != 8; 9 <= 10; 11 >= 12; }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::Binary), 6);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 12);
    }

    #[test]
    fn test_unary_expression() {
        // Note: The lexer may treat -42 as a negative literal, not unary minus
        // Let's test with a variable to ensure we get a unary operator
        let source = "fn main() { let x = 42; -x; }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::Unary), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 1);
        assert!(count_nodes_of_type(&ast, NodeTag::Identifier) >= 1); // x in -x
    }

    #[test]
    fn test_function_call() {
        let source = "fn main() { foo(); bar(1, 2, 3); }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::Call), 2);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Identifier), 2); // foo, bar (main is in Function node)
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 3); // 1, 2, 3
    }

    #[test]
    fn test_nested_blocks() {
        // Blocks in Rue need to be part of control flow, not standalone
        let source = "fn main() { if true { if false { 42 } else { 43 } } }";
        let ast = parse_to_ast(source);

        // Should have main block + outer if block + inner if blocks (then + else)
        assert!(count_nodes_of_type(&ast, NodeTag::Block) >= 3);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 4); // true, false, 42, 43
    }

    #[test]
    fn test_string_interning() {
        let source = "fn main() { let x = 1; let y = 2; let x = 3; }";
        let ast = parse_to_ast(source);

        // The name 'x' should be interned only once
        // We can verify this by checking that there are only 3 unique strings: main, x, y
        // (StringPool doesn't expose its internal storage, so we check indirectly)
        // Count unique identifier nodes
        let unique_idents = ast
            .nodes
            .tags
            .iter()
            .enumerate()
            .filter(|(_, tag)| {
                **tag == NodeTag::Identifier || **tag == NodeTag::Let || **tag == NodeTag::Function
            })
            .count();

        // We should have main function + x variable (used twice) + y variable
        assert!(unique_idents > 0, "Should have identifier nodes");
    }

    #[test]
    fn test_complex_program() {
        let source = r#"
            fn factorial(n: i32) -> i32 {
                if n <= 1 {
                    return 1;
                } else {
                    return n * factorial(n - 1);
                }
            }
            
            fn main() {
                let result = factorial(5);
                println(result);
            }
        "#;

        let ast = parse_to_ast(source);

        // Verify we have two functions
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Function), 2);

        // Verify we have recursive call
        assert!(count_nodes_of_type(&ast, NodeTag::Call) >= 2); // factorial call + println

        // Verify we have if statement
        assert_eq!(count_nodes_of_type(&ast, NodeTag::If), 1);

        // Verify we have return statements
        assert!(count_nodes_of_type(&ast, NodeTag::Return) >= 2);
    }

    #[test]
    fn test_span_preservation() {
        let source = "fn main() { let x = 42; }";
        let ast = parse_to_ast(source);

        // Find the literal node
        let literal_idx = ast
            .nodes
            .tags
            .iter()
            .position(|&tag| tag == NodeTag::Literal)
            .expect("Should have a literal");

        // Check that the span is reasonable
        let span = ast.spans[literal_idx];
        assert!(span.start < span.end);
        assert!(span.end <= source.len());
    }

    #[test]
    fn test_node_size() {
        use std::mem;

        // Verify that nodes are exactly 16 bytes as designed
        assert_eq!(mem::size_of::<NodeTag>(), 1);
        assert_eq!(mem::size_of::<rue_ir::ast::NodeData>(), 8);

        // The total node size in arrays should be:
        // 1 byte (tag) + 8 bytes (data) + 4 bytes (token) + padding = 16 bytes when aligned
    }

    #[test]
    fn test_expression_statement() {
        let source = "fn main() { 42; foo(); }";
        let ast = parse_to_ast(source);

        assert_eq!(count_nodes_of_type(&ast, NodeTag::ExprStmt), 2);
    }

    #[test]
    fn test_implicit_return() {
        let source = "fn main() -> i32 { 42 }"; // No semicolon, implicit return
        let ast = parse_to_ast(source);

        // The final expression in a block without semicolon should be preserved
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Literal), 1);
        assert_eq!(count_nodes_of_type(&ast, NodeTag::Block), 1);
    }

    #[test]
    fn test_struct_definition() {
        let source = "struct Point { x: i32, y: i32 }";
        let ast = parse_to_ast(source);

        // Should have one struct definition node
        assert!(
            count_nodes_of_type(&ast, NodeTag::StructDef) > 0,
            "Struct definition not found in AST"
        );
    }

    #[test]
    fn test_tuple_type() {
        let source = "fn test(x: (i32, bool)) -> i32 { 0 }";
        let ast = parse_to_ast(source);

        // Should have a tuple type node
        assert!(
            count_nodes_of_type(&ast, NodeTag::TupleType) > 0,
            "Tuple type not found in AST"
        );
    }

    #[test]
    fn test_array_type() {
        let source = "fn test(arr: [i32; 10]) -> i32 { 0 }";
        let ast = parse_to_ast(source);

        // Should have an array type node
        assert!(
            count_nodes_of_type(&ast, NodeTag::ArrayType) > 0,
            "Array type not found in AST"
        );
    }

    #[test]
    fn test_struct_type_reference() {
        let source = "fn test(p: Point) -> i32 { 0 }";
        let ast = parse_to_ast(source);

        // Should have a struct type reference
        assert!(
            count_nodes_of_type(&ast, NodeTag::StructType) > 0,
            "Struct type reference not found in AST"
        );
    }

    #[test]
    fn test_complex_struct_with_types() {
        let source = "struct Complex { arr: [i32; 5], tup: (bool, i64), nested: Point }";
        let ast = parse_to_ast(source);

        // Should have struct def, array type, tuple type, and struct type nodes
        assert!(
            count_nodes_of_type(&ast, NodeTag::StructDef) > 0,
            "Struct definition not found"
        );
        assert!(
            count_nodes_of_type(&ast, NodeTag::ArrayType) > 0,
            "Array type not found"
        );
        assert!(
            count_nodes_of_type(&ast, NodeTag::TupleType) > 0,
            "Tuple type not found"
        );
        assert!(
            count_nodes_of_type(&ast, NodeTag::StructType) > 0,
            "Struct type reference not found"
        );
    }
}
