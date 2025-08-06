//! Expression parsing with precedence climbing

use crate::error::{ParseError, ParserContext};
use crate::parser::{ParseResult, Parser};
use rue_ast::*;
use rue_lexer::{Span, TokenKind};
use std::collections::{HashMap, HashSet};

impl Parser {
    /// Parse an expression
    pub(crate) fn parse_expression(&mut self) -> ParseResult<ExpressionNode> {
        self.push_context(ParserContext::Expression);
        let result = self.parse_comparison();
        self.pop_context();
        result
    }

    /// Parse comparison operators (<, <=, >, >=, ==, !=)
    fn parse_comparison(&mut self) -> ParseResult<ExpressionNode> {
        let mut expr = self.parse_addition()?;

        while self.is_comparison_op() {
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

    /// Check if current token is a comparison operator
    fn is_comparison_op(&self) -> bool {
        matches!(
            self.peek_kind(),
            TokenKind::Less
                | TokenKind::LessEqual
                | TokenKind::Greater
                | TokenKind::GreaterEqual
                | TokenKind::Equal
                | TokenKind::NotEqual
        )
    }

    /// Parse addition and subtraction
    fn parse_addition(&mut self) -> ParseResult<ExpressionNode> {
        let mut expr = self.parse_multiplication()?;

        while matches!(self.peek_kind(), TokenKind::Plus | TokenKind::Minus) {
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

    /// Parse multiplication, division, and modulo
    fn parse_multiplication(&mut self) -> ParseResult<ExpressionNode> {
        let mut expr = self.parse_unary()?;

        while matches!(
            self.peek_kind(),
            TokenKind::Star | TokenKind::Slash | TokenKind::Percent
        ) {
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

    /// Parse unary expressions (e.g., -expr)
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
            self.parse_postfix()
        }
    }

    /// Parse postfix expressions (calls, field access, array indexing)
    fn parse_postfix(&mut self) -> ParseResult<ExpressionNode> {
        let mut expr = self.parse_primary()?;

        // Handle postfix operators
        loop {
            match self.peek_kind() {
                TokenKind::LeftParen => {
                    // Function call
                    expr = self.parse_call_expression(expr)?;
                }
                TokenKind::Dot => {
                    // Field access or tuple indexing
                    expr = self.parse_field_access(expr)?;
                }
                TokenKind::LeftBracket => {
                    // Array indexing
                    expr = self.parse_index_expression(expr)?;
                }
                _ => break,
            }
        }

        Ok(expr)
    }

    /// Parse a function call expression
    fn parse_call_expression(&mut self, callee: ExpressionNode) -> ParseResult<ExpressionNode> {
        let leading_trivia = self.consume_trivia();
        let open_paren = self.advance(); // consume '('
        let open_span = open_paren.span;
        
        let mut arguments = Vec::new();

        while !self.check_kind(&TokenKind::RightParen) && !self.is_at_end() {
            arguments.push(self.parse_expression()?);

            if !self.check_kind(&TokenKind::RightParen) {
                self.expect_kind(&TokenKind::Comma)?;
            }
        }

        // Check for unclosed parenthesis
        if self.is_at_end() && !self.check_kind(&TokenKind::RightParen) {
            return Err(ParseError::unclosed_delimiter(
                "(",
                open_span,
                self.peek().span,
                self.source_id.clone(),
            ));
        }

        let close_paren = self.expect_kind(&TokenKind::RightParen)?;

        Ok(ExpressionNode::Call(CallExprNode {
            callee: Box::new(callee),
            open_paren,
            arguments,
            close_paren,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
    }

    /// Parse field access or tuple indexing
    fn parse_field_access(&mut self, expr: ExpressionNode) -> ParseResult<ExpressionNode> {
        let leading_trivia = self.consume_trivia();
        let dot = self.advance(); // consume '.'

        match self.peek_kind() {
            TokenKind::Ident(_) => {
                let field = self.advance();
                Ok(ExpressionNode::FieldAccess(FieldAccessNode {
                    object: Box::new(expr),
                    dot,
                    field,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }))
            }
            TokenKind::Integer(index) if *index >= 0 => {
                let index_token = self.advance();
                Ok(ExpressionNode::TupleIndex(TupleIndexNode {
                    tuple: Box::new(expr),
                    dot,
                    index: index_token,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }))
            }
            _ => Err(ParseError::unexpected_token(
                self.peek_kind(),
                &[TokenKind::Ident(String::new()), TokenKind::Integer(0)],
                self.peek().span,
                self.source_id.clone(),
                Some("after '.' in field access"),
            )),
        }
    }

    /// Parse array indexing
    fn parse_index_expression(&mut self, expr: ExpressionNode) -> ParseResult<ExpressionNode> {
        let leading_trivia = self.consume_trivia();
        let open_bracket = self.advance(); // consume '['
        let open_span = open_bracket.span;
        
        let index = self.parse_expression()?;

        // Check for unclosed bracket
        if self.is_at_end() && !self.check_kind(&TokenKind::RightBracket) {
            return Err(ParseError::unclosed_delimiter(
                "[",
                open_span,
                self.peek().span,
                self.source_id.clone(),
            ));
        }

        let close_bracket = self.expect_kind(&TokenKind::RightBracket)?;

        Ok(ExpressionNode::Index(IndexExprNode {
            object: Box::new(expr),
            open_bracket,
            index: Box::new(index),
            close_bracket,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
    }

    /// Parse primary expressions
    fn parse_primary(&mut self) -> ParseResult<ExpressionNode> {
        let leading_trivia = self.consume_trivia();

        match self.peek_kind() {
            TokenKind::Integer(value) => {
                let token = self.advance();
                Ok(ExpressionNode::Integer(IntegerLiteralNode {
                    token,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }))
            }
            TokenKind::True | TokenKind::False => {
                let token = self.advance();
                Ok(ExpressionNode::Bool(BoolLiteralNode {
                    token,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }))
            }
            TokenKind::Unit => {
                let token = self.advance();
                Ok(ExpressionNode::Unit(UnitLiteralNode {
                    token,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }))
            }
            TokenKind::Ident(name) => {
                // Check if this is a struct literal
                if self.is_struct_literal() {
                    self.parse_struct_literal()
                } else {
                    let token = self.advance();
                    Ok(ExpressionNode::Identifier(IdentifierNode {
                        token,
                        trivia: Trivia {
                            leading: leading_trivia,
                            trailing: self.consume_trivia(),
                        },
                    }))
                }
            }
            TokenKind::LeftParen => {
                self.parse_parenthesized_expression_or_tuple()
            }
            TokenKind::LeftBracket => {
                self.push_context(ParserContext::ArrayLiteral);
                let result = self.parse_array_literal();
                self.pop_context();
                result
            }
            TokenKind::If => {
                // Parse if expression
                let if_expr = self.parse_if_statement()?;
                Ok(ExpressionNode::If(Box::new(if_expr)))
            }
            _ => {
                if self.is_at_end() {
                    Err(ParseError::unexpected_eof(
                        "expression",
                        self.peek().span,
                        self.source_id.clone(),
                    ))
                } else {
                    Err(ParseError::unexpected_token(
                        self.peek_kind(),
                        &[], // No specific expected tokens
                        self.peek().span,
                        self.source_id.clone(),
                        Some("in expression"),
                    ))
                }
            }
        }
    }

    /// Check if this looks like a struct literal
    fn is_struct_literal(&self) -> bool {
        if self.current + 1 < self.tokens.len() {
            matches!(self.tokens[self.current + 1].kind, TokenKind::LeftBrace)
        } else {
            false
        }
    }

    /// Parse a struct literal
    fn parse_struct_literal(&mut self) -> ParseResult<ExpressionNode> {
        self.push_context(ParserContext::StructLiteral);
        let leading_trivia = self.consume_trivia();
        let name = self.expect_ident()?;
        let open_brace = self.expect_kind(&TokenKind::LeftBrace)?;
        let open_span = open_brace.span;

        let mut fields = Vec::new();
        let mut field_names = HashSet::new();
        let mut field_spans = HashMap::new();

        self.skip_comments();

        while !self.check_kind(&TokenKind::RightBrace) && !self.is_at_end() {
            let field = self.parse_struct_field_init()?;

            // Check for duplicate field names
            if let TokenKind::Ident(field_name) = &field.name.kind {
                if let Some(&prev_span) = field_spans.get(field_name) {
                    return Err(ParseError::duplicate_field(
                        field_name,
                        field.name.span,
                        prev_span,
                        self.source_id.clone(),
                        false,
                    ));
                }
                field_names.insert(field_name.clone());
                field_spans.insert(field_name.clone(), field.name.span);
            }

            fields.push(field);
            self.skip_comments();

            if self.check_kind(&TokenKind::Comma) {
                self.advance();
                self.skip_comments();
            } else if !self.check_kind(&TokenKind::RightBrace) {
                return Err(ParseError::unexpected_token(
                    self.peek_kind(),
                    &[TokenKind::Comma, TokenKind::RightBrace],
                    self.peek().span,
                    self.source_id.clone(),
                    Some("in struct literal"),
                ));
            }
        }

        // Check for unclosed brace
        if self.is_at_end() && !self.check_kind(&TokenKind::RightBrace) {
            return Err(ParseError::unclosed_delimiter(
                "{",
                open_span,
                self.peek().span,
                self.source_id.clone(),
            ));
        }

        let close_brace = self.expect_kind(&TokenKind::RightBrace)?;
        self.pop_context();

        Ok(ExpressionNode::StructLiteral(StructLiteralNode {
            name,
            open_brace,
            fields,
            close_brace,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
    }

    /// Parse a struct field initializer
    fn parse_struct_field_init(&mut self) -> ParseResult<StructFieldInitNode> {
        let leading_trivia = self.consume_trivia();
        let name = self.expect_ident()?;

        // Check for shorthand syntax (field without explicit value)
        let value = if self.check_kind(&TokenKind::Colon) {
            let colon = self.advance();
            let value = self.parse_expression()?;
            Some(FieldInitValue { colon, value })
        } else {
            None
        };

        Ok(StructFieldInitNode {
            name,
            value,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    /// Parse parenthesized expression or tuple
    fn parse_parenthesized_expression_or_tuple(&mut self) -> ParseResult<ExpressionNode> {
        self.push_context(ParserContext::TupleLiteral);
        let leading_trivia = self.consume_trivia();
        let open_paren = self.expect_kind(&TokenKind::LeftParen)?;
        let open_span = open_paren.span;

        // Empty tuple
        if self.check_kind(&TokenKind::RightParen) {
            let close_paren = self.advance();
            self.pop_context();
            return Ok(ExpressionNode::Tuple(TupleLiteralNode {
                open_paren,
                elements: vec![],
                close_paren,
                trivia: Trivia {
                    leading: leading_trivia,
                    trailing: self.consume_trivia(),
                },
            }));
        }

        let first_expr = self.parse_expression()?;

        // Single expression in parentheses
        if self.check_kind(&TokenKind::RightParen) {
            let close_paren = self.advance();
            self.pop_context();
            return Ok(ExpressionNode::Parenthesized(ParenthesizedExprNode {
                open_paren,
                expr: Box::new(first_expr),
                close_paren,
                trivia: Trivia {
                    leading: leading_trivia,
                    trailing: self.consume_trivia(),
                },
            }));
        }

        // Tuple with multiple elements
        let mut elements = vec![first_expr];
        self.expect_kind(&TokenKind::Comma)?;

        while !self.check_kind(&TokenKind::RightParen) && !self.is_at_end() {
            elements.push(self.parse_expression()?);

            if !self.check_kind(&TokenKind::RightParen) {
                self.expect_kind(&TokenKind::Comma)?;
            } else if self.check_kind(&TokenKind::Comma) {
                // Allow trailing comma
                self.advance();
            }
        }

        // Check for unclosed parenthesis
        if self.is_at_end() && !self.check_kind(&TokenKind::RightParen) {
            return Err(ParseError::unclosed_delimiter(
                "(",
                open_span,
                self.peek().span,
                self.source_id.clone(),
            ));
        }

        let close_paren = self.expect_kind(&TokenKind::RightParen)?;
        self.pop_context();

        Ok(ExpressionNode::Tuple(TupleLiteralNode {
            open_paren,
            elements,
            close_paren,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
    }

    /// Parse an array literal
    fn parse_array_literal(&mut self) -> ParseResult<ExpressionNode> {
        let leading_trivia = self.consume_trivia();
        let open_bracket = self.expect_kind(&TokenKind::LeftBracket)?;
        let open_span = open_bracket.span;

        let mut elements = Vec::new();
        self.skip_comments();

        while !self.check_kind(&TokenKind::RightBracket) && !self.is_at_end() {
            elements.push(self.parse_expression()?);
            self.skip_comments();

            if self.check_kind(&TokenKind::Comma) {
                self.advance();
                self.skip_comments();
            } else if !self.check_kind(&TokenKind::RightBracket) {
                return Err(ParseError::unexpected_token(
                    self.peek_kind(),
                    &[TokenKind::Comma, TokenKind::RightBracket],
                    self.peek().span,
                    self.source_id.clone(),
                    Some("in array literal"),
                ));
            }
        }

        // Check for unclosed bracket
        if self.is_at_end() && !self.check_kind(&TokenKind::RightBracket) {
            return Err(ParseError::unclosed_delimiter(
                "[",
                open_span,
                self.peek().span,
                self.source_id.clone(),
            ));
        }

        let close_bracket = self.expect_kind(&TokenKind::RightBracket)?;

        Ok(ExpressionNode::ArrayLiteral(ArrayLiteralNode {
            open_bracket,
            elements,
            close_bracket,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
    }
}