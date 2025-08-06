//! Parser snapshot tests
//!
//! These tests parse code and compare the resulting AST against snapshots.
//! They replace verbose manual AST checking with simple snapshot assertions.

#[cfg(test)]
mod tests {
    use crate::simple_snapshot::SnapshotTest;
    use crate::{CstRoot, ParseResult, parse};
    use rue_lexer::Lexer;

    fn lex_and_parse(source: &str) -> ParseResult<CstRoot> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().map_err(|e| crate::ParseError {
            message: format!("Lexical error: {}", e.message),
            span: rue_lexer::Span {
                start: e.position,
                end: e.position + 1,
            },
        })?;
        parse(tokens)
    }

    fn assert_parse_snapshot(test_name: &str, source: &str) {
        let result = lex_and_parse(source);
        let output = match result {
            Ok(cst) => format!("{cst:#?}"),
            Err(e) => format!("Error: {e}"),
        };

        SnapshotTest::new(test_name).assert_snapshot(&output);
    }

    #[test]
    fn test_simple_number() {
        assert_parse_snapshot("parser_simple_number", "42;");
    }

    #[test]
    fn test_negative_number() {
        assert_parse_snapshot("parser_negative_number", "-42;");
    }

    #[test]
    fn test_negative_number_in_let() {
        assert_parse_snapshot("parser_negative_number_in_let", "let x = -5;");
    }

    #[test]
    fn test_simple_identifier() {
        assert_parse_snapshot("parser_simple_identifier", "foo;");
    }

    #[test]
    fn test_binary_expression() {
        assert_parse_snapshot("parser_binary_expression", "2 + 3;");
    }

    #[test]
    fn test_function_call() {
        assert_parse_snapshot("parser_function_call", "factorial(5);");
    }

    #[test]
    fn test_function_declaration() {
        assert_parse_snapshot(
            "parser_function_declaration",
            "fn add(a: i64, b: i64) -> i64 { a + b }",
        );
    }

    #[test]
    fn test_if_expression() {
        assert_parse_snapshot("parser_if_expression", "if x > 0 { 1 } else { -1 }");
    }

    #[test]
    fn test_while_loop() {
        assert_parse_snapshot("parser_while_loop", "while x < 10 { x = x + 1; }");
    }

    #[test]
    fn test_multiple_statements() {
        assert_parse_snapshot(
            "parser_multiple_statements",
            "let x = 5; let y = 10; x + y;",
        );
    }

    #[test]
    fn test_nested_blocks() {
        assert_parse_snapshot(
            "parser_nested_blocks",
            "{ let x = 1; { let y = 2; x + y } }",
        );
    }

    #[test]
    fn test_complex_expression() {
        assert_parse_snapshot("parser_complex_expression", "(2 + 3) * 4 - 5 / (6 + 7);");
    }

    #[test]
    fn test_assignment() {
        assert_parse_snapshot("parser_assignment", "x = 42;");
    }

    #[test]
    fn test_compound_assignment() {
        assert_parse_snapshot("parser_compound_assignment", "x += 5;");
    }

    #[test]
    fn test_return_statement() {
        assert_parse_snapshot("parser_return_statement", "return 42;");
    }

    #[test]
    fn test_cast_expression() {
        assert_parse_snapshot("parser_cast_expression", "x as i32");
    }
}
