use rue_ast::*;
use rue_lexer::{Span, TokenKind, format_error_with_context};

pub mod ast_builder;
pub mod ast_builder_tests;
mod diagnostic_impl;
pub mod diagnostics;
pub mod error_recovery;
pub mod simple_snapshot;
pub mod snapshot;

pub use ast_builder::lower_cst_to_ast;
pub use diagnostics::{parse_with_diagnostics, parse_with_recovery};
pub use error_recovery::{ErrorRecoveryExt, RecoveringParser};

pub struct Parser {
    pub(crate) tokens: Vec<TokenNode>,
    pub(crate) current: usize,
}

pub type ParseResult<T> = Result<T, ParseError>;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("parse error: {message}")]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl ParseError {
    pub fn format_with_source(&self, source: &str) -> String {
        format_error_with_context(source, self.span, &self.message, "parse error")
    }
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
            TokenKind::Struct => Ok(CstNode::StructDefinition(Box::new(
                self.parse_struct_definition()?,
            ))),
            _ => {
                let stmt = self.parse_statement()?;
                Ok(CstNode::Statement(Box::new(stmt)))
            }
        }
    }

    pub(crate) fn parse_function(&mut self) -> ParseResult<FunctionNode> {
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

    pub(crate) fn parse_struct_definition(&mut self) -> ParseResult<StructDefinitionNode> {
        let leading_trivia = self.consume_trivia();
        let struct_token = self.expect_kind(&TokenKind::Struct)?;
        let name = self.expect_ident()?;
        let open_brace = self.expect_kind(&TokenKind::LeftBrace)?;

        let mut fields = Vec::new();
        self.skip_comments();

        // Parse struct fields
        let mut field_names = std::collections::HashSet::new();
        while !self.check_kind(&TokenKind::RightBrace) && !self.is_at_end() {
            let field = self.parse_struct_field_def()?;

            // Check for duplicate field names
            if let TokenKind::Ident(field_name) = &field.name.kind
                && !field_names.insert(field_name.clone())
            {
                return Err(ParseError {
                    message: format!("Duplicate field name '{field_name}' in struct definition"),
                    span: field.name.span,
                });
            }

            fields.push(field);
            self.skip_comments();

            // Optional comma
            if self.check_kind(&TokenKind::Comma) {
                self.advance();
                self.skip_comments();
            } else if !self.check_kind(&TokenKind::RightBrace) {
                return Err(ParseError {
                    message: "Expected ',' or '}' after struct field".to_string(),
                    span: self.peek().span,
                });
            }
        }

        let close_brace = self.expect_kind(&TokenKind::RightBrace)?;

        Ok(StructDefinitionNode {
            struct_token,
            name,
            open_brace,
            fields,
            close_brace,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_struct_field_def(&mut self) -> ParseResult<StructFieldDefNode> {
        let leading_trivia = self.consume_trivia();
        let name = self.expect_ident()?;
        let type_annotation = self.parse_type_annotation()?;

        Ok(StructFieldDefNode {
            name,
            type_annotation,
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

        // Skip comments before close paren
        self.skip_comments();
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
            // Skip any comments before checking what's next
            self.skip_comments();

            // Check again after skipping comments
            if self.check_kind(&TokenKind::RightBrace) {
                break;
            }

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

        // Skip comments before expecting closing brace
        self.skip_comments();
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
            TokenKind::Return => true,
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

    pub(crate) fn parse_statement(&mut self) -> ParseResult<StatementNode> {
        match self.peek().kind {
            TokenKind::Let => Ok(StatementNode::Let(Box::new(self.parse_let_statement()?))),
            TokenKind::Return => Ok(StatementNode::Return(self.parse_return_statement()?)),
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

    fn parse_return_statement(&mut self) -> ParseResult<ReturnStatementNode> {
        let leading_trivia = self.consume_trivia();
        let return_token = self.expect_kind(&TokenKind::Return)?;

        // Check if there's an expression after return
        let expression = if self.check_kind(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expression()?)
        };

        let semicolon = self.expect_kind(&TokenKind::Semicolon)?;

        Ok(ReturnStatementNode {
            return_token,
            expression,
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
        let mut expr = self.parse_unary()?;

        while self.check_kind(&TokenKind::Star)
            || self.check_kind(&TokenKind::Slash)
            || self.check_kind(&TokenKind::Percent)
        {
            let leading_trivia = self.consume_trivia();
            let operator = self.advance();
            let right = self.parse_unary()?;
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

    fn parse_unary(&mut self) -> ParseResult<ExpressionNode> {
        if self.check_kind(&TokenKind::Minus) {
            let leading_trivia = self.consume_trivia();
            let operator = self.advance();
            let operand = self.parse_unary()?;
            Ok(ExpressionNode::Unary(UnaryExprNode {
                operator,
                operand: Box::new(operand),
                trivia: Trivia {
                    leading: leading_trivia,
                    trailing: self.consume_trivia(),
                },
            }))
        } else {
            self.parse_call()
        }
    }

    fn parse_call(&mut self) -> ParseResult<ExpressionNode> {
        let mut expr = self.parse_primary()?;

        // Handle postfix operators: function calls, field access, array access
        loop {
            match &self.peek().kind {
                TokenKind::LeftParen => {
                    // Function call
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

                    // Skip comments before close paren
                    self.skip_comments();
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
                TokenKind::Dot => {
                    // Field access
                    let leading_trivia = self.consume_trivia();
                    let dot = self.advance();

                    // Field can be an identifier (named field) or integer (tuple field)
                    let field = match &self.peek().kind {
                        TokenKind::Ident(_) => FieldKindNode::Named(self.advance()),
                        TokenKind::Integer(_) => FieldKindNode::Positional(self.advance()),
                        _ => {
                            return Err(ParseError {
                                message: "Expected field name or index after '.'".to_string(),
                                span: self.peek().span,
                            });
                        }
                    };

                    expr = ExpressionNode::FieldAccess(FieldAccessNode {
                        base: Box::new(expr),
                        dot,
                        field,
                        trivia: Trivia {
                            leading: leading_trivia,
                            trailing: self.consume_trivia(),
                        },
                    });
                }
                TokenKind::LeftBracket => {
                    // Array access
                    let leading_trivia = self.consume_trivia();
                    let open_bracket = self.advance();

                    let index = Box::new(self.parse_expression()?);

                    self.skip_comments();
                    let close_bracket = self.expect_kind(&TokenKind::RightBracket)?;

                    expr = ExpressionNode::ArrayAccess(ArrayAccessNode {
                        base: Box::new(expr),
                        open_bracket,
                        index,
                        close_bracket,
                        trivia: Trivia {
                            leading: leading_trivia,
                            trailing: self.consume_trivia(),
                        },
                    });
                }
                _ => break, // No more postfix operators
            }
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> ParseResult<ExpressionNode> {
        match &self.peek().kind {
            TokenKind::Integer(_) => Ok(ExpressionNode::Literal(self.advance())),
            TokenKind::True | TokenKind::False => Ok(ExpressionNode::Literal(self.advance())),
            TokenKind::Ident(_) => {
                // Could be identifier or struct literal
                if self.peek_ahead(1).kind == TokenKind::LeftBrace {
                    // Need to distinguish between struct literal and if/while condition
                    // Look ahead to see if this looks like a struct literal pattern
                    if self.looks_like_struct_literal() {
                        // Struct literal: Identifier { field: value, ... }
                        Ok(ExpressionNode::StructLiteral(self.parse_struct_literal()?))
                    } else {
                        // Regular identifier (e.g., in "if condition {")
                        Ok(ExpressionNode::Identifier(self.advance()))
                    }
                } else {
                    // Regular identifier
                    Ok(ExpressionNode::Identifier(self.advance()))
                }
            }
            TokenKind::If => Ok(ExpressionNode::If(Box::new(self.parse_if_statement()?))),
            TokenKind::While => Ok(ExpressionNode::While(Box::new(
                self.parse_while_statement()?,
            ))),
            TokenKind::LeftParen => {
                // Could be unit literal, tuple literal, or parenthesized expression
                self.parse_parenthesized_expression_or_tuple()
            }
            TokenKind::LeftBracket => {
                // Array literal: [...]
                Ok(ExpressionNode::ArrayLiteral(self.parse_array_literal()?))
            }
            _ => Err(ParseError {
                message: format!("Unexpected token: {:?}", self.peek().kind),
                span: self.peek().span,
            }),
        }
    }

    fn parse_struct_literal(&mut self) -> ParseResult<StructLiteralNode> {
        let leading_trivia = self.consume_trivia();
        let name = self.expect_ident()?;
        let open_brace = self.expect_kind(&TokenKind::LeftBrace)?;

        let mut fields = Vec::new();
        self.skip_comments();

        // Parse struct field initializers
        let mut field_names = std::collections::HashSet::new();
        while !self.check_kind(&TokenKind::RightBrace) && !self.is_at_end() {
            let field = self.parse_struct_field_init()?;

            // Check for duplicate field names
            if let TokenKind::Ident(field_name) = &field.name.kind
                && !field_names.insert(field_name.clone())
            {
                return Err(ParseError {
                    message: format!("Duplicate field name '{field_name}' in struct literal"),
                    span: field.name.span,
                });
            }

            fields.push(field);
            self.skip_comments();

            // Optional comma
            if self.check_kind(&TokenKind::Comma) {
                self.advance();
                self.skip_comments();
            } else if !self.check_kind(&TokenKind::RightBrace) {
                return Err(ParseError {
                    message: "Expected ',' or '}' after struct field initializer".to_string(),
                    span: self.peek().span,
                });
            }
        }

        let close_brace = self.expect_kind(&TokenKind::RightBrace)?;

        Ok(StructLiteralNode {
            name,
            open_brace,
            fields,
            close_brace,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    fn parse_struct_field_init(&mut self) -> ParseResult<StructFieldInitNode> {
        let leading_trivia = self.consume_trivia();
        let name = self.expect_ident()?;
        let colon = self.expect_kind(&TokenKind::Colon)?;
        let value = self.parse_expression()?;

        Ok(StructFieldInitNode {
            name,
            colon,
            value,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    /// Look ahead to determine if `Identifier {` pattern is likely a struct literal
    /// rather than part of an if/while statement.
    ///
    /// A struct literal should have the pattern: Identifier { field: value, ... }
    /// We look for an identifier followed by a colon after the opening brace.
    fn looks_like_struct_literal(&mut self) -> bool {
        // We're at position: Identifier {
        // Look ahead to see what's after the opening brace
        let mut pos = 2; // Skip identifier and opening brace

        // Skip any comments
        while self.current + pos < self.tokens.len() {
            match &self.tokens[self.current + pos].kind {
                TokenKind::Comment(_) => {
                    pos += 1;
                    continue;
                }
                _ => break,
            }
        }

        if self.current + pos >= self.tokens.len() {
            return false;
        }

        // Check if we have an identifier followed by a colon (struct field pattern)
        // or closing brace (empty struct literal)
        match &self.tokens[self.current + pos].kind {
            TokenKind::Ident(_) => {
                // Look for colon after identifier
                pos += 1;
                while self.current + pos < self.tokens.len() {
                    match &self.tokens[self.current + pos].kind {
                        TokenKind::Comment(_) => {
                            pos += 1;
                            continue;
                        }
                        TokenKind::Colon => return true, // field: value pattern
                        _ => return false,               // not a struct literal
                    }
                }
                false
            }
            TokenKind::RightBrace => true, // Empty struct literal: Struct {}
            _ => false,                    // Something else, not a struct literal
        }
    }

    fn parse_parenthesized_expression_or_tuple(&mut self) -> ParseResult<ExpressionNode> {
        let leading_trivia = self.consume_trivia();
        let open_paren = self.advance(); // consume '('
        self.skip_comments();

        // Check for unit literal: ()
        if self.check_kind(&TokenKind::RightParen) {
            let close_paren = self.advance();
            // Create a synthetic Unit token
            let unit_token = TokenNode {
                kind: TokenKind::Unit,
                span: rue_lexer::Span {
                    start: open_paren.span.start,
                    end: close_paren.span.end,
                },
            };
            return Ok(ExpressionNode::Literal(unit_token));
        }

        // Parse first expression
        let first_expr = self.parse_expression()?;
        self.skip_comments();

        // Check if this is a tuple (has comma) or parenthesized expression
        if self.check_kind(&TokenKind::Comma) {
            // This is a tuple literal
            let mut elements = vec![first_expr];
            self.advance(); // consume comma
            self.skip_comments();

            // If we see ), this is a single-element tuple (expr,)
            if self.check_kind(&TokenKind::RightParen) {
                let close_paren = self.advance();
                return Ok(ExpressionNode::TupleLiteral(TupleLiteralNode {
                    open_paren,
                    elements,
                    close_paren,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }));
            }

            // Parse remaining elements
            while !self.check_kind(&TokenKind::RightParen) && !self.is_at_end() {
                elements.push(self.parse_expression()?);
                self.skip_comments();

                if self.check_kind(&TokenKind::Comma) {
                    self.advance();
                    self.skip_comments();
                } else {
                    break;
                }
            }

            let close_paren = self.expect_kind(&TokenKind::RightParen)?;
            Ok(ExpressionNode::TupleLiteral(TupleLiteralNode {
                open_paren,
                elements,
                close_paren,
                trivia: Trivia {
                    leading: leading_trivia,
                    trailing: self.consume_trivia(),
                },
            }))
        } else {
            // This is a parenthesized expression
            self.expect_kind(&TokenKind::RightParen)?;
            Ok(first_expr)
        }
    }

    fn parse_array_literal(&mut self) -> ParseResult<ArrayLiteralNode> {
        let leading_trivia = self.consume_trivia();
        let open_bracket = self.expect_kind(&TokenKind::LeftBracket)?;

        let mut elements = Vec::new();
        self.skip_comments();

        // Parse array elements
        while !self.check_kind(&TokenKind::RightBracket) && !self.is_at_end() {
            elements.push(self.parse_expression()?);
            self.skip_comments();

            // Optional comma
            if self.check_kind(&TokenKind::Comma) {
                self.advance();
                self.skip_comments();
            } else if !self.check_kind(&TokenKind::RightBracket) {
                return Err(ParseError {
                    message: "Expected ',' or ']' after array element".to_string(),
                    span: self.peek().span,
                });
            }
        }

        let close_bracket = self.expect_kind(&TokenKind::RightBracket)?;

        Ok(ArrayLiteralNode {
            open_bracket,
            elements,
            close_bracket,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    // Helper methods
    pub(crate) fn peek(&self) -> &TokenNode {
        self.tokens.get(self.current).unwrap_or(&TokenNode {
            kind: TokenKind::Eof,
            span: rue_lexer::Span { start: 0, end: 0 },
        })
    }

    fn peek_ahead(&self, n: usize) -> &TokenNode {
        self.tokens.get(self.current + n).unwrap_or(&TokenNode {
            kind: TokenKind::Eof,
            span: rue_lexer::Span { start: 0, end: 0 },
        })
    }

    pub(crate) fn advance(&mut self) -> TokenNode {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.tokens.get(self.current - 1).unwrap().clone()
    }

    pub(crate) fn check_kind(&self, kind: &TokenKind) -> bool {
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

    fn expect_integer(&mut self) -> ParseResult<TokenNode> {
        match &self.peek().kind {
            TokenKind::Integer(_) => Ok(self.advance()),
            _ => Err(ParseError {
                message: format!("Expected integer literal, found {:?}", self.peek().kind),
                span: self.peek().span,
            }),
        }
    }

    pub(crate) fn is_at_end(&self) -> bool {
        self.current >= self.tokens.len() || self.peek().kind == TokenKind::Eof
    }

    /// Get the current position in the token stream
    pub(crate) fn current_position(&self) -> usize {
        self.current
    }

    pub(crate) fn consume_trivia(&mut self) -> Vec<TokenNode> {
        let mut trivia = Vec::new();

        // Consume any comment tokens
        while self.check_kind(&TokenKind::Comment(String::new())) {
            trivia.push(self.advance());
        }

        trivia
    }

    fn skip_comments(&mut self) {
        while self.check_kind(&TokenKind::Comment(String::new())) {
            self.advance();
        }
    }

    fn parse_type(&mut self) -> ParseResult<TypeNode> {
        match &self.peek().kind {
            TokenKind::I32 => Ok(TypeNode::I32(self.advance())),
            TokenKind::I64 => Ok(TypeNode::I64(self.advance())),
            TokenKind::Bool => Ok(TypeNode::Bool(self.advance())),
            TokenKind::LeftParen => {
                // Tuple type or unit type: (T1, T2, ...) or ()
                self.parse_tuple_type()
            }
            TokenKind::LeftBracket => {
                // Array type: [T; size]
                self.parse_array_type()
            }
            TokenKind::Ident(_) => {
                // Struct type: StructName
                self.parse_struct_type()
            }
            _ => Err(ParseError {
                message: format!("Expected type, found {:?}", self.peek().kind),
                span: self.peek().span,
            }),
        }
    }

    fn parse_struct_type(&mut self) -> ParseResult<TypeNode> {
        let leading_trivia = self.consume_trivia();
        let name = self.expect_ident()?;

        Ok(TypeNode::Struct(StructTypeNode {
            name,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
    }

    fn parse_tuple_type(&mut self) -> ParseResult<TypeNode> {
        let leading_trivia = self.consume_trivia();
        let open_paren = self.expect_kind(&TokenKind::LeftParen)?;

        self.skip_comments();

        // Check for unit type: ()
        if self.check_kind(&TokenKind::RightParen) {
            let _close_paren = self.advance();
            return Ok(TypeNode::Unit);
        }

        let mut types = Vec::new();

        // Parse first type
        types.push(self.parse_type()?);
        self.skip_comments();

        // Handle single-element tuple: (T,)
        if self.check_kind(&TokenKind::Comma) {
            self.advance(); // consume comma
            self.skip_comments();

            // Optional trailing comma - if we see ), this is a single-element tuple
            if self.check_kind(&TokenKind::RightParen) {
                let close_paren = self.advance();
                return Ok(TypeNode::Tuple(TupleTypeNode {
                    open_paren,
                    types,
                    close_paren,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }));
            }

            // Parse remaining types
            while !self.check_kind(&TokenKind::RightParen) && !self.is_at_end() {
                types.push(self.parse_type()?);
                self.skip_comments();

                if self.check_kind(&TokenKind::Comma) {
                    self.advance(); // consume comma
                    self.skip_comments();
                } else {
                    break;
                }
            }
        }

        let close_paren = self.expect_kind(&TokenKind::RightParen)?;

        Ok(TypeNode::Tuple(TupleTypeNode {
            open_paren,
            types,
            close_paren,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
    }

    fn parse_array_type(&mut self) -> ParseResult<TypeNode> {
        let leading_trivia = self.consume_trivia();
        let open_bracket = self.expect_kind(&TokenKind::LeftBracket)?;

        self.skip_comments();
        let element_type = Box::new(self.parse_type()?);

        self.skip_comments();
        let semicolon = self.expect_kind(&TokenKind::Semicolon)?;

        self.skip_comments();
        let size = self.expect_integer()?;

        // Validate array size is non-negative
        if let TokenKind::Integer(size_value) = &size.kind
            && *size_value < 0
        {
            return Err(ParseError {
                message: format!("Array size must be non-negative, found {size_value}"),
                span: size.span,
            });
        }

        self.skip_comments();
        let close_bracket = self.expect_kind(&TokenKind::RightBracket)?;

        Ok(TypeNode::Array(ArrayTypeNode {
            open_bracket,
            element_type,
            semicolon,
            size,
            close_bracket,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
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
mod snapshot_tests;

#[cfg(test)]
mod diagnostic_snapshot_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;

    fn lex_and_parse(source: &str) -> ParseResult<CstRoot> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().map_err(|e| ParseError {
            message: format!("Lexical error: {}", e.message),
            span: Span {
                start: e.position,
                end: e.position + 1,
            },
        })?;
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
    fn test_negative_number() {
        let result = lex_and_parse("-42;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::Literal(token) => match &token.kind {
                        TokenKind::Integer(value) => assert_eq!(*value, -42),
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
    fn test_negative_number_in_let() {
        let result = lex_and_parse("let x = -5;");
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
                            TokenKind::Integer(value) => assert_eq!(*value, -5),
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

    #[test]
    fn test_unit_literal() {
        let result = lex_and_parse("let x: () = ();");
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
                        TypeNode::Unit => {} // Success
                        _ => panic!("Expected unit type"),
                    }

                    // Check value
                    match &let_stmt.value {
                        ExpressionNode::Literal(token) => match &token.kind {
                            TokenKind::Unit => {} // Success
                            _ => panic!("Expected unit literal for value"),
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
    fn test_unit_literal_in_expression() {
        let result = lex_and_parse("();");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::Literal(token) => match &token.kind {
                        TokenKind::Unit => {} // Success
                        _ => panic!("Expected unit literal"),
                    },
                    _ => panic!("Expected literal expression"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    // Edge case tests for parser

    #[test]
    fn test_deeply_nested_expressions() {
        // Test deeply nested binary expressions
        let result = lex_and_parse("1 + 2 * 3 - 4 / 5 % 6 + 7 * 8 - 9;");
        assert!(result.is_ok());

        // First test simpler nested parentheses
        let result = lex_and_parse("((42));");
        assert!(result.is_ok());

        let result = lex_and_parse("((((42))));");
        assert!(result.is_ok());

        // Let's test progressively deeper nesting to see where it breaks
        for depth in 5..=10 {
            let opens = "(".repeat(depth);
            let closes = ")".repeat(depth);
            let test_str = format!("{opens}42{closes};");
            let result = lex_and_parse(&test_str);
            assert!(result.is_ok(), "Failed to parse {depth} levels of nesting");
        }

        // Test deeply nested function calls
        // First test simple function calls
        let result = lex_and_parse("f();");
        assert!(result.is_ok());

        let result = lex_and_parse("f(g());");
        assert!(result.is_ok());

        let result = lex_and_parse("f(g(h()));");
        assert!(result.is_ok());

        // Now test the deeply nested version
        let result = lex_and_parse("f(g(h(i(j(k(l(m(n(o(p(q())))))))))));");
        assert!(result.is_ok());

        // Test deeply nested if expressions
        let nested_if = r#"
if true {
    if false {
        if true {
            if false {
                if true {
                    42
                } else {
                    0
                }
            } else {
                1
            }
        } else {
            2
        }
    } else {
        3
    }
} else {
    4
};"#;
        let result = lex_and_parse(nested_if);
        assert!(result.is_ok());

        // Test complex mixed nesting
        let complex = r#"
fn complex(x: i32) -> i32 {
    if x > 0 {
        factorial(x - 1) * x + compute(x / 2, x % 2) - adjust((x + 1) * (x - 1))
    } else {
        if x < -10 {
            -factorial(-x) + compute(-x / 2, -x % 2) * adjust((-x + 1) * (-x - 1))
        } else {
            0
        }
    }
}"#;
        let result = lex_and_parse(complex);
        assert!(result.is_ok());
    }

    #[test]
    fn test_maximum_identifier_length() {
        // Test very long identifier (255 chars)
        let long_ident = "a".repeat(255);
        let code = format!("let {long_ident} = 42;");
        let result = lex_and_parse(&code);
        assert!(result.is_ok());

        // Test even longer identifier (1000 chars)
        let very_long_ident = "very_long_identifier_name_that_goes_on_and_on_and_on_".repeat(20);
        let code = format!("let {very_long_ident} = 42;");
        let result = lex_and_parse(&code);
        assert!(result.is_ok());

        // Test identifier with underscores and numbers
        let result = lex_and_parse("let _private_var_123_with_numbers = 42;");
        assert!(result.is_ok());

        // Test single character identifiers
        let result = lex_and_parse("let a = 1; let b = 2; let c = a + b;");
        assert!(result.is_ok());

        // Test identifiers that are similar to keywords but not
        let result = lex_and_parse("let fnx = 1; let iff = 2; let whiles = 3; let lett = 4;");
        assert!(result.is_ok());

        // Test identifiers with keyword prefixes/suffixes
        let result = lex_and_parse("let function = 1; let my_fn = 2; let if_condition = 3;");
        assert!(result.is_ok());
    }

    #[test]
    fn test_comment_preservation() {
        // Test that comments are preserved in the CST
        let result = lex_and_parse("// This is a comment\nlet x = 42;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        // The comment should be in the root's leading trivia
        assert!(!cst.trivia.leading.is_empty());
        assert_eq!(cst.trivia.leading.len(), 1);
        match &cst.trivia.leading[0].kind {
            TokenKind::Comment(text) => {
                assert_eq!(text, " This is a comment");
            }
            _ => panic!("Expected a comment token"),
        }

        // Test multi-line comment preservation
        let result2 = lex_and_parse("/* This is a\nmulti-line comment */\nlet x = 42;");
        assert!(result2.is_ok());
        let cst2 = result2.unwrap();
        assert_eq!(cst2.items.len(), 1);

        // The multi-line comment should be in the root's leading trivia
        assert!(!cst2.trivia.leading.is_empty());
        assert_eq!(cst2.trivia.leading.len(), 1);
        match &cst2.trivia.leading[0].kind {
            TokenKind::Comment(text) => {
                assert_eq!(text, " This is a\nmulti-line comment ");
            }
            _ => panic!("Expected a comment token"),
        }
    }

    #[test]
    fn test_unicode_in_comments() {
        // Test single-line comments with ASCII
        let result = lex_and_parse("// Hello World\n42;");
        assert!(result.is_ok());

        // Test multi-line comments with ASCII
        let result = lex_and_parse("/* This is a comment */ 42;");
        assert!(result.is_ok());

        // Test comments with extended ASCII that should work
        let result = lex_and_parse("// Comment with cafe, naive, resume\n42;");
        assert!(result.is_ok());

        // Test Japanese characters
        let result = lex_and_parse("// こんにちは世界\n42;");
        assert!(result.is_ok());

        // Test Chinese characters
        let result = lex_and_parse("// 你好世界\n42;");
        assert!(result.is_ok());

        // Test Arabic characters
        let result = lex_and_parse("// مرحبا بالعالم\n42;");
        assert!(result.is_ok());

        // Test emojis
        let result = lex_and_parse("// 🚀🎉🎊🎈🎁🎂\n42;");
        assert!(result.is_ok());

        // Test box drawing characters
        let result = lex_and_parse("// ┌─────────┐\n42;");
        assert!(result.is_ok());

        // Test accented characters
        let result = lex_and_parse("// café, naïve, résumé\n42;");
        assert!(result.is_ok());

        // Test unicode in multi-line comments
        let result = lex_and_parse("/* 日本語のコメント */\n42;");
        assert!(result.is_ok());

        // Test mixed unicode and code
        let result = lex_and_parse("let x = 10; // 设置x的值\nx + 5;");
        assert!(result.is_ok());
    }

    #[test]
    fn test_empty_files_and_single_tokens() {
        // Test completely empty file
        let result = lex_and_parse("");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 0);

        // Test file with only whitespace
        let result = lex_and_parse("   \n\t\n   ");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 0);

        // Test file with only comments
        let result = lex_and_parse("// Just a comment");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 0);

        let result = lex_and_parse("/* Just a multi-line comment */");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 0);

        // Test single expression (no semicolon - should fail as it's not in a function)
        let result = lex_and_parse("42");
        assert!(result.is_err());

        // Test single expression with semicolon
        let result = lex_and_parse("42;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        // Test single identifier (should fail without semicolon)
        let result = lex_and_parse("x");
        assert!(result.is_err());

        // Test single identifier with semicolon
        let result = lex_and_parse("x;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        // Test minimal function
        let result = lex_and_parse("fn f() { }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        // Test function with single expression body
        let result = lex_and_parse("fn f() { 42 }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        // Test single let statement
        let result = lex_and_parse("let x = 1;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        // Test file with mixed comments and whitespace but no code
        let result = lex_and_parse(
            r#"
// This file has comments
/* And multi-line comments */
    // And indented comments
        /* But no actual code */
        
        
"#,
        );
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 0);
    }

    // ===== AGGREGATE TYPE TESTS =====

    #[test]
    fn test_struct_definition_simple() {
        let result = lex_and_parse("struct Point { x: i32, y: i32 }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::StructDefinition(struct_def) => {
                assert_eq!(struct_def.fields.len(), 2);
                if let TokenKind::Ident(name) = &struct_def.name.kind {
                    assert_eq!(name, "Point");
                }
                if let TokenKind::Ident(field_name) = &struct_def.fields[0].name.kind {
                    assert_eq!(field_name, "x");
                }
                if let TokenKind::Ident(field_name) = &struct_def.fields[1].name.kind {
                    assert_eq!(field_name, "y");
                }
            }
            _ => panic!("Expected struct definition"),
        }
    }

    #[test]
    fn test_struct_definition_with_trailing_comma() {
        let result = lex_and_parse("struct Point { x: i32, y: i32, }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);
    }

    #[test]
    fn test_struct_definition_duplicate_fields() {
        let result = lex_and_parse("struct Point { x: i32, x: i64 }");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Duplicate field name 'x'"));
    }

    #[test]
    fn test_struct_literal_simple() {
        let result = lex_and_parse("Point { x: 10, y: 20 };");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::StructLiteral(struct_lit) => {
                        if let TokenKind::Ident(name) = &struct_lit.name.kind {
                            assert_eq!(name, "Point");
                        }
                        assert_eq!(struct_lit.fields.len(), 2);
                    }
                    _ => panic!("Expected struct literal"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_struct_literal_duplicate_fields() {
        let result = lex_and_parse("Point { x: 10, x: 20 };");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Duplicate field name 'x'"));
    }

    #[test]
    fn test_tuple_type_empty() {
        let result = lex_and_parse("fn test() -> () { }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                if let Some(return_type) = &func.return_type {
                    match &return_type.ty {
                        TypeNode::Unit => {} // Expected
                        _ => panic!("Expected unit type"),
                    }
                }
            }
            _ => panic!("Expected function"),
        }
    }

    #[test]
    fn test_tuple_type_single_element() {
        let result = lex_and_parse("fn test() -> (i32,) { }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                if let Some(return_type) = &func.return_type {
                    match &return_type.ty {
                        TypeNode::Tuple(tuple_type) => {
                            assert_eq!(tuple_type.types.len(), 1);
                        }
                        _ => panic!("Expected tuple type"),
                    }
                }
            }
            _ => panic!("Expected function"),
        }
    }

    #[test]
    fn test_tuple_type_multiple_elements() {
        let result = lex_and_parse("fn test() -> (i32, bool, i64) { }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                if let Some(return_type) = &func.return_type {
                    match &return_type.ty {
                        TypeNode::Tuple(tuple_type) => {
                            assert_eq!(tuple_type.types.len(), 3);
                        }
                        _ => panic!("Expected tuple type"),
                    }
                }
            }
            _ => panic!("Expected function"),
        }
    }

    #[test]
    fn test_array_type() {
        let result = lex_and_parse("fn test() -> [i32; 5] { }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                if let Some(return_type) = &func.return_type {
                    match &return_type.ty {
                        TypeNode::Array(array_type) => {
                            if let TokenKind::Integer(size) = &array_type.size.kind {
                                assert_eq!(*size, 5);
                            }
                        }
                        _ => panic!("Expected array type"),
                    }
                }
            }
            _ => panic!("Expected function"),
        }
    }

    #[test]
    fn test_array_type_zero_size() {
        let result = lex_and_parse("fn test() -> [i32; 0] { }");
        assert!(result.is_ok()); // Zero-size arrays are allowed
    }

    #[test]
    fn test_array_type_negative_size() {
        let result = lex_and_parse("fn test() -> [i32; -1] { }");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Array size must be non-negative"));
    }

    #[test]
    fn test_struct_type() {
        let result = lex_and_parse("fn test() -> Point { }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Function(func) => {
                if let Some(return_type) = &func.return_type {
                    match &return_type.ty {
                        TypeNode::Struct(struct_type) => {
                            if let TokenKind::Ident(name) = &struct_type.name.kind {
                                assert_eq!(name, "Point");
                            }
                        }
                        _ => panic!("Expected struct type"),
                    }
                }
            }
            _ => panic!("Expected function"),
        }
    }

    #[test]
    fn test_tuple_literal_empty() {
        let result = lex_and_parse("();");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::Literal(token) => {
                        assert_eq!(token.kind, TokenKind::Unit);
                    }
                    _ => panic!("Expected unit literal"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_tuple_literal_single_element() {
        let result = lex_and_parse("(42,);");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::TupleLiteral(tuple_lit) => {
                        assert_eq!(tuple_lit.elements.len(), 1);
                    }
                    _ => panic!("Expected tuple literal"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_tuple_literal_multiple_elements() {
        let result = lex_and_parse("(1, true, 42);");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::TupleLiteral(tuple_lit) => {
                        assert_eq!(tuple_lit.elements.len(), 3);
                    }
                    _ => panic!("Expected tuple literal"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_array_literal_empty() {
        let result = lex_and_parse("[];");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::ArrayLiteral(array_lit) => {
                        assert_eq!(array_lit.elements.len(), 0);
                    }
                    _ => panic!("Expected array literal"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_array_literal_with_elements() {
        let result = lex_and_parse("[1, 2, 3];");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::ArrayLiteral(array_lit) => {
                        assert_eq!(array_lit.elements.len(), 3);
                    }
                    _ => panic!("Expected array literal"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_field_access_named() {
        let result = lex_and_parse("point.x;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::FieldAccess(field_access) => match &field_access.field {
                        FieldKindNode::Named(token) => {
                            if let TokenKind::Ident(name) = &token.kind {
                                assert_eq!(name, "x");
                            }
                        }
                        _ => panic!("Expected named field"),
                    },
                    _ => panic!("Expected field access"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_field_access_positional() {
        let result = lex_and_parse("tuple.0;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::FieldAccess(field_access) => match &field_access.field {
                        FieldKindNode::Positional(token) => {
                            if let TokenKind::Integer(index) = &token.kind {
                                assert_eq!(*index, 0);
                            }
                        }
                        _ => panic!("Expected positional field"),
                    },
                    _ => panic!("Expected field access"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_array_access() {
        let result = lex_and_parse("arr[0];");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::ArrayAccess(array_access) => {
                        match array_access.index.as_ref() {
                            ExpressionNode::Literal(token) => {
                                if let TokenKind::Integer(index) = &token.kind {
                                    assert_eq!(*index, 0);
                                }
                            }
                            _ => panic!("Expected integer literal"),
                        }
                    }
                    _ => panic!("Expected array access"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_chained_access() {
        let result = lex_and_parse("data.points[0].x;");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        // Should parse as ((data.points)[0]).x
        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::FieldAccess(_) => {} // Expected outermost field access
                    _ => panic!("Expected field access"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_parenthesized_expression_vs_tuple() {
        // (42) should be a parenthesized expression, not a tuple
        let result = lex_and_parse("(42);");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::Statement(stmt) => match &**stmt {
                StatementNode::Expression(expr_stmt) => match &expr_stmt.expression {
                    ExpressionNode::Literal(_) => {} // Should be unwrapped literal
                    _ => panic!("Expected literal (parentheses should be unwrapped)"),
                },
                _ => panic!("Expected expression statement"),
            },
            _ => panic!("Expected statement"),
        }
    }

    #[test]
    fn test_nested_aggregates() {
        let result = lex_and_parse("struct Nested { points: [Point; 3], metadata: (i32, bool) }");
        assert!(result.is_ok());
        let cst = result.unwrap();
        assert_eq!(cst.items.len(), 1);

        match &cst.items[0] {
            CstNode::StructDefinition(struct_def) => {
                assert_eq!(struct_def.fields.len(), 2);
                // Verify the field types contain the expected nested structures
                match &struct_def.fields[0].type_annotation.ty {
                    TypeNode::Array(_) => {} // First field should be array type
                    _ => panic!("Expected array type for first field"),
                }
                match &struct_def.fields[1].type_annotation.ty {
                    TypeNode::Tuple(_) => {} // Second field should be tuple type
                    _ => panic!("Expected tuple type for second field"),
                }
            }
            _ => panic!("Expected struct definition"),
        }
    }
}
