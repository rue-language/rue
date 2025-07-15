use rue_ast::*;
use rue_lexer::{Span, TokenKind};

pub struct Parser {
    tokens: Vec<TokenNode>,
    current: usize,
}

pub type ParseResult<T> = Result<T, ParseError>;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl Parser {
    pub fn new(tokens: Vec<TokenNode>) -> Self {
        Self { tokens, current: 0 }
    }

    pub fn parse(mut self) -> ParseResult<CstRoot> {
        let mut items = Vec::new();
        let leading_trivia = self.consume_trivia();

        while !self.is_at_end() {
            items.push(self.parse_item()?);
        }

        Ok(CstRoot {
            items,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: vec![],
            },
        })
    }

    fn parse_item(&mut self) -> ParseResult<CstNode> {
        match self.peek().kind {
            TokenKind::Fn => Ok(CstNode::Function(Box::new(self.parse_function()?))),
            _ => {
                let stmt = self.parse_statement()?;
                Ok(CstNode::Statement(Box::new(stmt)))
            }
        }
    }

    fn parse_function(&mut self) -> ParseResult<FunctionNode> {
        let leading_trivia = self.consume_trivia();
        let fn_token = self.expect_kind(&TokenKind::Fn)?;
        let name = self.expect_ident()?;
        let param_list = self.parse_param_list()?;

        // Parse optional return type
        let return_type = if self.check_kind(&TokenKind::Arrow) {
            Some(self.parse_return_type()?)
        } else {
            None
        };

        let body = self.parse_block()?;

        Ok(FunctionNode {
            fn_token,
            name,
            param_list,
            return_type,
            body,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_return_type(&mut self) -> ParseResult<ReturnTypeNode> {
        let leading_trivia = self.consume_trivia();
        let arrow = self.expect_kind(&TokenKind::Arrow)?;
        let ty = self.parse_type()?;

        Ok(ReturnTypeNode {
            arrow,
            ty,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_param_list(&mut self) -> ParseResult<ParamListNode> {
        let leading_trivia = self.consume_trivia();
        let open_paren = self.expect_kind(&TokenKind::LeftParen)?;

        let mut params = Vec::new();
        if !self.check_kind(&TokenKind::RightParen) {
            params.push(self.parse_parameter()?);

            // Handle multiple parameters with commas
            while self.check_kind(&TokenKind::Comma) {
                self.advance(); // consume comma
                params.push(self.parse_parameter()?);
            }
        }

        let close_paren = self.expect_kind(&TokenKind::RightParen)?;

        Ok(ParamListNode {
            open_paren,
            params,
            close_paren,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_parameter(&mut self) -> ParseResult<ParameterNode> {
        let leading_trivia = self.consume_trivia();
        let name = self.expect_ident()?;

        // Parse optional type annotation
        let type_annotation = if self.check_kind(&TokenKind::Colon) {
            Some(self.parse_type_annotation()?)
        } else {
            None
        };

        Ok(ParameterNode {
            name,
            type_annotation,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_block(&mut self) -> ParseResult<BlockNode> {
        let leading_trivia = self.consume_trivia();
        let open_brace = self.expect_kind(&TokenKind::LeftBrace)?;

        let mut statements = Vec::new();
        let mut final_expr = None;

        while !self.check_kind(&TokenKind::RightBrace) && !self.is_at_end() {
            // Try to parse as statement first
            if self.is_statement_start() {
                statements.push(self.parse_statement()?);
            } else {
                // Parse as potential final expression
                let expr = self.parse_expression()?;

                // If followed by semicolon, it's an expression statement
                if self.check_kind(&TokenKind::Semicolon) {
                    let semicolon = self.advance();
                    statements.push(StatementNode::Expression(ExpressionStatementNode {
                        expression: expr,
                        semicolon,
                        trivia: Trivia {
                            leading: vec![],
                            trailing: self.consume_trivia(),
                        },
                    }));
                } else {
                    // No semicolon - this is the final expression
                    final_expr = Some(expr);
                    break;
                }
            }
        }

        let close_brace = self.expect_kind(&TokenKind::RightBrace)?;

        Ok(BlockNode {
            open_brace,
            statements,
            final_expr,
            close_brace,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn is_statement_start(&self) -> bool {
        match self.peek().kind {
            TokenKind::Let => true,
            TokenKind::Ident(_) => {
                // Check if this is an assignment statement (identifier = expression)
                if self.current + 1 < self.tokens.len() {
                    matches!(self.tokens[self.current + 1].kind, TokenKind::Assign)
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn parse_statement(&mut self) -> ParseResult<StatementNode> {
        match self.peek().kind {
            TokenKind::Let => Ok(StatementNode::Let(self.parse_let_statement()?)),
            TokenKind::Ident(_) => {
                // Look ahead to see if this is an assignment (identifier = expression)
                if self.current + 1 < self.tokens.len() {
                    match &self.tokens[self.current + 1].kind {
                        TokenKind::Assign => {
                            Ok(StatementNode::Assign(self.parse_assign_statement()?))
                        }
                        _ => {
                            // This is an expression statement - parse expression + semicolon
                            let expr = self.parse_expression()?;
                            let semicolon = self.expect_kind(&TokenKind::Semicolon)?;
                            Ok(StatementNode::Expression(ExpressionStatementNode {
                                expression: expr,
                                semicolon,
                                trivia: Trivia {
                                    leading: vec![],
                                    trailing: self.consume_trivia(),
                                },
                            }))
                        }
                    }
                } else {
                    Err(ParseError {
                        message: "Unexpected end of input".to_string(),
                        span: self.peek().span,
                    })
                }
            }
            _ => {
                // Expression statement
                let expr = self.parse_expression()?;
                let semicolon = self.expect_kind(&TokenKind::Semicolon)?;
                Ok(StatementNode::Expression(ExpressionStatementNode {
                    expression: expr,
                    semicolon,
                    trivia: Trivia {
                        leading: vec![],
                        trailing: self.consume_trivia(),
                    },
                }))
            }
        }
    }

    fn parse_let_statement(&mut self) -> ParseResult<LetStatementNode> {
        let leading_trivia = self.consume_trivia();
        let let_token = self.expect_kind(&TokenKind::Let)?;
        let name = self.expect_ident()?;

        // Parse optional type annotation
        let type_annotation = if self.check_kind(&TokenKind::Colon) {
            Some(self.parse_type_annotation()?)
        } else {
            None
        };

        let equals = self.expect_kind(&TokenKind::Assign)?;
        let value = self.parse_expression()?;
        let semicolon = self.expect_kind(&TokenKind::Semicolon)?;

        Ok(LetStatementNode {
            let_token,
            name,
            type_annotation,
            equals,
            value,
            semicolon,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_assign_statement(&mut self) -> ParseResult<AssignStatementNode> {
        let leading_trivia = self.consume_trivia();
        let name = self.expect_ident()?;
        let equals = self.expect_kind(&TokenKind::Assign)?;
        let value = self.parse_expression()?;
        let semicolon = self.expect_kind(&TokenKind::Semicolon)?;

        Ok(AssignStatementNode {
            name,
            equals,
            value,
            semicolon,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_if_statement(&mut self) -> ParseResult<IfStatementNode> {
        let leading_trivia = self.consume_trivia();
        let if_token = self.expect_kind(&TokenKind::If)?;
        let condition = self.parse_expression()?;
        let then_block = self.parse_block()?;

        let else_clause = if self.check_kind(&TokenKind::Else) {
            Some(self.parse_else_clause()?)
        } else {
            None
        };

        Ok(IfStatementNode {
            if_token,
            condition,
            then_block,
            else_clause,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_else_clause(&mut self) -> ParseResult<ElseClauseNode> {
        let leading_trivia = self.consume_trivia();
        let else_token = self.expect_kind(&TokenKind::Else)?;

        let body = if self.check_kind(&TokenKind::If) {
            ElseBodyNode::If(Box::new(self.parse_if_statement()?))
        } else {
            ElseBodyNode::Block(Box::new(self.parse_block()?))
        };

        Ok(ElseClauseNode {
            else_token,
            body,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_while_statement(&mut self) -> ParseResult<WhileStatementNode> {
        let leading_trivia = self.consume_trivia();
        let while_token = self.expect_kind(&TokenKind::While)?;
        let condition = self.parse_expression()?;
        let body = self.parse_block()?;

        Ok(WhileStatementNode {
            while_token,
            condition,
            body,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_expression(&mut self) -> ParseResult<ExpressionNode> {
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> ParseResult<ExpressionNode> {
        let mut expr = self.parse_addition()?;

        while self.check_kind(&TokenKind::LessEqual)
            || self.check_kind(&TokenKind::Less)
            || self.check_kind(&TokenKind::Greater)
            || self.check_kind(&TokenKind::GreaterEqual)
            || self.check_kind(&TokenKind::Equal)
            || self.check_kind(&TokenKind::NotEqual)
        {
            let leading_trivia = self.consume_trivia();
            let operator = self.advance();
            let right = self.parse_addition()?;
            expr = ExpressionNode::Binary(BinaryExprNode {
                left: Box::new(expr),
                operator,
                right: Box::new(right),
                trivia: Trivia {
                    leading: leading_trivia,
                    trailing: self.consume_trivia(),
                },
            });
        }

        Ok(expr)
    }

    fn parse_addition(&mut self) -> ParseResult<ExpressionNode> {
        let mut expr = self.parse_multiplication()?;

        while self.check_kind(&TokenKind::Plus) || self.check_kind(&TokenKind::Minus) {
            let leading_trivia = self.consume_trivia();
            let operator = self.advance();
            let right = self.parse_multiplication()?;
            expr = ExpressionNode::Binary(BinaryExprNode {
                left: Box::new(expr),
                operator,
                right: Box::new(right),
                trivia: Trivia {
                    leading: leading_trivia,
                    trailing: self.consume_trivia(),
                },
            });
        }

        Ok(expr)
    }

    fn parse_multiplication(&mut self) -> ParseResult<ExpressionNode> {
        let mut expr = self.parse_call()?;

        while self.check_kind(&TokenKind::Star)
            || self.check_kind(&TokenKind::Slash)
            || self.check_kind(&TokenKind::Percent)
        {
            let leading_trivia = self.consume_trivia();
            let operator = self.advance();
            let right = self.parse_call()?;
            expr = ExpressionNode::Binary(BinaryExprNode {
                left: Box::new(expr),
                operator,
                right: Box::new(right),
                trivia: Trivia {
                    leading: leading_trivia,
                    trailing: self.consume_trivia(),
                },
            });
        }

        Ok(expr)
    }

    fn parse_call(&mut self) -> ParseResult<ExpressionNode> {
        let mut expr = self.parse_primary()?;

        while self.check_kind(&TokenKind::LeftParen) {
            let leading_trivia = self.consume_trivia();
            let open_paren = self.advance();

            let mut args = Vec::new();
            if !self.check_kind(&TokenKind::RightParen) {
                args.push(self.parse_expression()?);

                // Handle multiple arguments with commas
                while self.check_kind(&TokenKind::Comma) {
                    self.advance(); // consume comma
                    args.push(self.parse_expression()?);
                }
            }

            let close_paren = self.expect_kind(&TokenKind::RightParen)?;

            expr = ExpressionNode::Call(CallExprNode {
                function: Box::new(expr),
                open_paren,
                args,
                close_paren,
                trivia: Trivia {
                    leading: leading_trivia,
                    trailing: self.consume_trivia(),
                },
            });
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> ParseResult<ExpressionNode> {
        match &self.peek().kind {
            TokenKind::Integer(_) => Ok(ExpressionNode::Literal(self.advance())),
            TokenKind::True | TokenKind::False => Ok(ExpressionNode::Literal(self.advance())),
            TokenKind::Ident(_) => Ok(ExpressionNode::Identifier(self.advance())),
            TokenKind::If => Ok(ExpressionNode::If(Box::new(self.parse_if_statement()?))),
            TokenKind::While => Ok(ExpressionNode::While(Box::new(
                self.parse_while_statement()?,
            ))),
            TokenKind::LeftParen => {
                self.advance(); // consume '('
                let expr = self.parse_expression()?;
                self.expect_kind(&TokenKind::RightParen)?;
                Ok(expr)
            }
            _ => Err(ParseError {
                message: format!("Unexpected token: {:?}", self.peek().kind),
                span: self.peek().span,
            }),
        }
    }

    // Helper methods
    fn peek(&self) -> &TokenNode {
        self.tokens.get(self.current).unwrap_or(&TokenNode {
            kind: TokenKind::Eof,
            span: rue_lexer::Span { start: 0, end: 0 },
        })
    }

    fn advance(&mut self) -> TokenNode {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.tokens.get(self.current - 1).unwrap().clone()
    }

    fn check_kind(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind)
    }

    fn expect_kind(&mut self, kind: &TokenKind) -> ParseResult<TokenNode> {
        if self.check_kind(kind) {
            Ok(self.advance())
        } else {
            Err(ParseError {
                message: format!("Expected {:?}, found {:?}", kind, self.peek().kind),
                span: self.peek().span,
            })
        }
    }

    fn expect_ident(&mut self) -> ParseResult<TokenNode> {
        match &self.peek().kind {
            TokenKind::Ident(_) => Ok(self.advance()),
            _ => Err(ParseError {
                message: format!("Expected identifier, found {:?}", self.peek().kind),
                span: self.peek().span,
            }),
        }
    }

    fn is_at_end(&self) -> bool {
        self.current >= self.tokens.len() || self.peek().kind == TokenKind::Eof
    }

    fn consume_trivia(&mut self) -> Vec<TokenNode> {
        // Note: lexer already skips whitespace, so no trivia to consume for now
        // TODO: Handle comments when lexer supports them
        Vec::new()
    }

    fn parse_type(&mut self) -> ParseResult<TypeNode> {
        match &self.peek().kind {
            TokenKind::I32 => Ok(TypeNode::I32(self.advance())),
            TokenKind::I64 => Ok(TypeNode::I64(self.advance())),
            TokenKind::Bool => Ok(TypeNode::Bool(self.advance())),
            TokenKind::LeftParen => {
                // Unit type: ()
                self.advance(); // consume '('
                self.expect_kind(&TokenKind::RightParen)?;
                Ok(TypeNode::Unit)
            }
            _ => Err(ParseError {
                message: format!("Expected type, found {:?}", self.peek().kind),
                span: self.peek().span,
            }),
        }
    }

    fn parse_type_annotation(&mut self) -> ParseResult<TypeAnnotationNode> {
        let leading_trivia = self.consume_trivia();
        let colon = self.expect_kind(&TokenKind::Colon)?;
        let ty = self.parse_type()?;

        Ok(TypeAnnotationNode {
            colon,
            ty,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }
}

pub fn parse(tokens: Vec<TokenNode>) -> ParseResult<CstRoot> {
    Parser::new(tokens).parse()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;

    fn lex_and_parse(source: &str) -> ParseResult<CstRoot> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        parse(tokens)
    }

    #[test]
    fn test_simple_number() {
        let result = lex_and_parse("42;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::Literal(token) => match &token.kind {
                        TokenKind::Integer(value) => assert_eq!(*value, 42),
                        _ => panic!("Expected integer token"),
                    },
                    _ => panic!("Expected literal expression"),
                },
                _ => panic!("Expected expression statement with literal"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_simple_identifier() {
        let result = lex_and_parse("foo;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::Identifier(token) => match &token.kind {
                        TokenKind::Ident(name) => assert_eq!(name, "foo"),
                        _ => panic!("Expected identifier token"),
                    },
                    _ => panic!("Expected identifier expression"),
                },
                _ => panic!("Expected expression statement with identifier"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_binary_expression() {
        let result = lex_and_parse("2 + 3;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::Binary(binary) => {
                        // Check left operand
                        match &*binary.left {
                            ExpressionNode::Literal(token) => match &token.kind {
                                TokenKind::Integer(value) => assert_eq!(*value, 2),
                                _ => panic!("Expected integer token for left operand"),
                            },
                            _ => panic!("Expected literal for left operand"),
                        }

                        // Check operator
                        assert_eq!(binary.operator.kind, TokenKind::Plus);

                        // Check right operand
                        match &*binary.right {
                            ExpressionNode::Literal(token) => match &token.kind {
                                TokenKind::Integer(value) => assert_eq!(*value, 3),
                                _ => panic!("Expected integer token for right operand"),
                            },
                            _ => panic!("Expected literal for right operand"),
                        }
                    }
                    _ => panic!("Expected binary expression"),
                },
                _ => panic!("Expected expression statement with binary expression"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_function_call() {
        let result = lex_and_parse("factorial(5);");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::Call(call) => {
                        // Check function name
                        match &*call.function {
                            ExpressionNode::Identifier(token) => match &token.kind {
                                TokenKind::Ident(name) => assert_eq!(name, "factorial"),
                                _ => panic!("Expected identifier token for function name"),
                            },
                            _ => panic!("Expected identifier for function name"),
                        }

                        // Check arguments
                        assert_eq!(call.args.len(), 1);
                        match &call.args[0] {
                            ExpressionNode::Literal(token) => match &token.kind {
                                TokenKind::Integer(value) => assert_eq!(*value, 5),
                                _ => panic!("Expected integer token for argument"),
                            },
                            _ => panic!("Expected literal for argument"),
                        }
                    }
                    _ => panic!("Expected function call"),
                },
                _ => panic!("Expected expression statement with function call"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_let_statement() {
        let result = lex_and_parse("let x = 42;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Let(let_stmt) => {
                    // Check variable name
                    match &let_stmt.name.kind {
                        TokenKind::Ident(name) => assert_eq!(name, "x"),
                        _ => panic!("Expected identifier token for variable name"),
                    }

                    // Check value
                    match &let_stmt.value {
                        ExpressionNode::Literal(token) => match &token.kind {
                            TokenKind::Integer(value) => assert_eq!(*value, 42),
                            _ => panic!("Expected integer token for value"),
                        },
                        _ => panic!("Expected literal for value"),
                    }
                }
                _ => panic!("Expected let statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_simple_function() {
        let result = lex_and_parse("fn test(x: i32) -> i32 { x }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                // Check function name
                match &func.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "test"),
                    _ => panic!("Expected identifier token for function name"),
                }

                // Check parameter
                assert_eq!(func.param_list.params.len(), 1);
                let param = &func.param_list.params[0];
                match &param.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "x"),
                    _ => panic!("Expected identifier for parameter"),
                }
                assert!(param.type_annotation.is_some());
                let type_ann = param.type_annotation.as_ref().unwrap();
                match &type_ann.ty {
                    TypeNode::I32(_) => {} // Success
                    _ => panic!("Expected i32 type for parameter"),
                }

                // Check return type
                assert!(func.return_type.is_some());
                let return_type = func.return_type.as_ref().unwrap();
                match &return_type.ty {
                    TypeNode::I32(_) => {} // Success
                    _ => panic!("Expected i32 return type"),
                }

                // Check body has a final expression
                assert!(func.body.final_expr.is_some());
            }
            _ => panic!("Expected function"),
        }
    }

    #[test]
    fn test_factorial_example() {
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

        let result = lex_and_parse(source);
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 2); // factorial function + main function

        // Check factorial function
        match &cst.items[0] {
            CstNode::Function(func) => {
                match &func.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "factorial"),
                    _ => panic!("Expected identifier token for factorial function name"),
                }

                // Check that the body contains a final expression (the if expression)
                assert!(func.body.final_expr.is_some());
                match &func.body.final_expr {
                    Some(ExpressionNode::If(_)) => {} // Success
                    _ => panic!("Expected if expression in factorial function"),
                }
            }
            _ => panic!("Expected factorial function"),
        }

        // Check main function
        match &cst.items[1] {
            CstNode::Function(func) => {
                match &func.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "main"),
                    _ => panic!("Expected identifier token for main function name"),
                }

                // Check that the body contains a function call as final expression
                assert!(func.body.final_expr.is_some());
                match &func.body.final_expr {
                    Some(ExpressionNode::Call(_)) => {} // Success
                    _ => panic!("Expected function call as final expression in main function"),
                }
            }
            _ => panic!("Expected main function"),
        }
    }

    #[test]
    fn test_while_statement() {
        let result = lex_and_parse("while x <= 10 { x };");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::While(while_stmt) => {
                        // Check condition is a binary expression
                        match &while_stmt.condition {
                            ExpressionNode::Binary(binary) => {
                                // Check left operand
                                match &*binary.left {
                                    ExpressionNode::Identifier(token) => match &token.kind {
                                        TokenKind::Ident(name) => assert_eq!(name, "x"),
                                        _ => panic!("Expected identifier token for left operand"),
                                    },
                                    _ => panic!("Expected identifier for left operand"),
                                }

                                // Check operator
                                assert_eq!(binary.operator.kind, TokenKind::LessEqual);

                                // Check right operand
                                match &*binary.right {
                                    ExpressionNode::Literal(token) => match &token.kind {
                                        TokenKind::Integer(value) => assert_eq!(*value, 10),
                                        _ => panic!("Expected integer token for right operand"),
                                    },
                                    _ => panic!("Expected literal for right operand"),
                                }
                            }
                            _ => panic!("Expected binary expression for condition"),
                        }

                        // Check body has final expression
                        assert!(while_stmt.body.final_expr.is_some());
                        match &while_stmt.body.final_expr {
                            Some(ExpressionNode::Identifier(token)) => match &token.kind {
                                TokenKind::Ident(name) => assert_eq!(name, "x"),
                                _ => panic!("Expected identifier token in body"),
                            },
                            _ => panic!("Expected identifier as final expression in body"),
                        }
                    }
                    _ => panic!("Expected while expression"),
                },
                _ => panic!("Expected expression statement with while expression"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_assign_statement() {
        let result = lex_and_parse("x = 42;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Assign(assign_stmt) => {
                    // Check variable name
                    match &assign_stmt.name.kind {
                        TokenKind::Ident(name) => assert_eq!(name, "x"),
                        _ => panic!("Expected identifier token for variable name"),
                    }

                    // Check value
                    match &assign_stmt.value {
                        ExpressionNode::Literal(token) => match &token.kind {
                            TokenKind::Integer(value) => assert_eq!(*value, 42),
                            _ => panic!("Expected integer token for value"),
                        },
                        _ => panic!("Expected literal for value"),
                    }
                }
                _ => panic!("Expected assign statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_typed_let_statement() {
        let result = lex_and_parse("let x: i32 = 42;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Let(let_stmt) => {
                    // Check variable name
                    match &let_stmt.name.kind {
                        TokenKind::Ident(name) => assert_eq!(name, "x"),
                        _ => panic!("Expected identifier token for variable name"),
                    }

                    // Check type annotation
                    assert!(let_stmt.type_annotation.is_some());
                    let type_ann = let_stmt.type_annotation.as_ref().unwrap();
                    match &type_ann.ty {
                        TypeNode::I32(_) => {} // Success
                        _ => panic!("Expected i32 type"),
                    }

                    // Check value
                    match &let_stmt.value {
                        ExpressionNode::Literal(token) => match &token.kind {
                            TokenKind::Integer(value) => assert_eq!(*value, 42),
                            _ => panic!("Expected integer token for value"),
                        },
                        _ => panic!("Expected literal for value"),
                    }
                }
                _ => panic!("Expected let statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_bool_type_let_statement() {
        let result = lex_and_parse("let flag: bool = true;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Let(let_stmt) => {
                    // Check variable name
                    match &let_stmt.name.kind {
                        TokenKind::Ident(name) => assert_eq!(name, "flag"),
                        _ => panic!("Expected identifier token for variable name"),
                    }

                    // Check type annotation
                    assert!(let_stmt.type_annotation.is_some());
                    let type_ann = let_stmt.type_annotation.as_ref().unwrap();
                    match &type_ann.ty {
                        TypeNode::Bool(_) => {} // Success
                        _ => panic!("Expected bool type"),
                    }

                    // Check value
                    match &let_stmt.value {
                        ExpressionNode::Literal(token) => match &token.kind {
                            TokenKind::True => {} // Success
                            _ => panic!("Expected true token for value"),
                        },
                        _ => panic!("Expected literal for value"),
                    }
                }
                _ => panic!("Expected let statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_untyped_let_statement() {
        // Parser should still accept untyped let statements
        let result = lex_and_parse("let x = 42;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Let(let_stmt) => {
                    // Check variable name
                    match &let_stmt.name.kind {
                        TokenKind::Ident(name) => assert_eq!(name, "x"),
                        _ => panic!("Expected identifier token for variable name"),
                    }

                    // Check type annotation is None
                    assert!(let_stmt.type_annotation.is_none());

                    // Check value
                    match &let_stmt.value {
                        ExpressionNode::Literal(token) => match &token.kind {
                            TokenKind::Integer(value) => assert_eq!(*value, 42),
                            _ => panic!("Expected integer token for value"),
                        },
                        _ => panic!("Expected literal for value"),
                    }
                }
                _ => panic!("Expected let statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_typed_function() {
        let result = lex_and_parse("fn add(a: i32, b: i32) -> i32 { a + b }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                // Check function name
                match &func.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "add"),
                    _ => panic!("Expected identifier token for function name"),
                }

                // Check parameters
                assert_eq!(func.param_list.params.len(), 2);

                // Check first parameter
                let param0 = &func.param_list.params[0];
                match &param0.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "a"),
                    _ => panic!("Expected identifier for first parameter"),
                }
                assert!(param0.type_annotation.is_some());
                let type_ann0 = param0.type_annotation.as_ref().unwrap();
                match &type_ann0.ty {
                    TypeNode::I32(_) => {} // Success
                    _ => panic!("Expected i32 type for first parameter"),
                }

                // Check second parameter
                let param1 = &func.param_list.params[1];
                match &param1.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "b"),
                    _ => panic!("Expected identifier for second parameter"),
                }
                assert!(param1.type_annotation.is_some());
                let type_ann1 = param1.type_annotation.as_ref().unwrap();
                match &type_ann1.ty {
                    TypeNode::I32(_) => {} // Success
                    _ => panic!("Expected i32 type for second parameter"),
                }

                // Check return type
                assert!(func.return_type.is_some());
                let return_type = func.return_type.as_ref().unwrap();
                match &return_type.ty {
                    TypeNode::I32(_) => {} // Success
                    _ => panic!("Expected i32 return type"),
                }

                // Check body has a final expression
                assert!(func.body.final_expr.is_some());
            }
            _ => panic!("Expected function"),
        }
    }

    #[test]
    fn test_unit_return_function() {
        let result = lex_and_parse("fn print(x: i32) { x; }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                // Check function name
                match &func.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "print"),
                    _ => panic!("Expected identifier token for function name"),
                }

                // Check parameter
                assert_eq!(func.param_list.params.len(), 1);
                let param = &func.param_list.params[0];
                match &param.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "x"),
                    _ => panic!("Expected identifier for parameter"),
                }
                assert!(param.type_annotation.is_some());
                let type_ann = param.type_annotation.as_ref().unwrap();
                match &type_ann.ty {
                    TypeNode::I32(_) => {} // Success
                    _ => panic!("Expected i32 type for parameter"),
                }

                // Check no return type (defaults to unit)
                assert!(func.return_type.is_none());

                // Check body has no final expression (statements only)
                assert!(func.body.final_expr.is_none());
                assert_eq!(func.body.statements.len(), 1);
            }
            _ => panic!("Expected function"),
        }
    }

    #[test]
    fn test_explicit_unit_return() {
        let result = lex_and_parse("fn noop() -> () { }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                // Check function name
                match &func.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "noop"),
                    _ => panic!("Expected identifier token for function name"),
                }

                // Check no parameters
                assert_eq!(func.param_list.params.len(), 0);

                // Check explicit unit return type
                assert!(func.return_type.is_some());
                let return_type = func.return_type.as_ref().unwrap();
                match &return_type.ty {
                    TypeNode::Unit => {} // Success
                    _ => panic!("Expected unit return type"),
                }

                // Check empty body
                assert!(func.body.final_expr.is_none());
                assert_eq!(func.body.statements.len(), 0);
            }
            _ => panic!("Expected function"),
        }
    }

    #[test]
    fn test_untyped_parameters() {
        let result = lex_and_parse("fn factorial(n) { n }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                // Check function name
                match &func.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "factorial"),
                    _ => panic!("Expected identifier token for function name"),
                }

                // Check parameter without type annotation
                assert_eq!(func.param_list.params.len(), 1);
                let param = &func.param_list.params[0];
                match &param.name.kind {
                    TokenKind::Ident(name) => assert_eq!(name, "n"),
                    _ => panic!("Expected identifier for parameter"),
                }
                assert!(param.type_annotation.is_none()); // No type annotation

                // Check no return type
                assert!(func.return_type.is_none());

                // Check body has a final expression
                assert!(func.body.final_expr.is_some());
            }
            _ => panic!("Expected function"),
        }
    }
}
