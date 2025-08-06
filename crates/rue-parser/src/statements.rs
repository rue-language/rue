//! Parsing of statements and blocks

use crate::error::{ParseError, ParserContext};
use crate::parser::{ParseResult, Parser};
use rue_ast::*;
use rue_lexer::{Span, TokenKind};

impl Parser {
    /// Parse a block of statements
    pub(crate) fn parse_block(&mut self) -> ParseResult<BlockNode> {
        let leading_trivia = self.consume_trivia();
        let open_brace = self.expect_kind(&TokenKind::LeftBrace)?;
        let open_span = open_brace.span;

        self.push_context(ParserContext::Block);
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

    /// Check if the current token starts a statement
    fn is_statement_start(&self) -> bool {
        match self.peek_kind() {
            TokenKind::Let => true,
            TokenKind::Return => true,
            TokenKind::If => true,
            TokenKind::While => true,
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

    /// Parse a statement
    pub(crate) fn parse_statement(&mut self) -> ParseResult<StatementNode> {
        self.skip_comments();
        
        match self.peek_kind() {
            TokenKind::Let => Ok(StatementNode::Let(self.parse_let_statement()?)),
            TokenKind::Return => Ok(StatementNode::Return(self.parse_return_statement()?)),
            TokenKind::If => Ok(StatementNode::If(self.parse_if_statement()?)),
            TokenKind::While => Ok(StatementNode::While(self.parse_while_statement()?)),
            TokenKind::Ident(_) if self.is_assignment() => {
                Ok(StatementNode::Assign(self.parse_assign_statement()?))
            }
            _ => {
                // Parse as expression statement
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

    /// Check if the current position is an assignment
    fn is_assignment(&self) -> bool {
        if self.current + 1 < self.tokens.len() {
            matches!(self.tokens[self.current + 1].kind, TokenKind::Assign)
        } else {
            false
        }
    }

    /// Parse a let statement
    fn parse_let_statement(&mut self) -> ParseResult<LetStatementNode> {
        let leading_trivia = self.consume_trivia();
        let let_token = self.expect_kind(&TokenKind::Let)?;
        let pattern = self.expect_ident()?;
        
        // Add variable to local scope for suggestions
        if let TokenKind::Ident(var_name) = &pattern.kind {
            self.suggestions.add_identifier("local", var_name.clone());
        }

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
            pattern,
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

    /// Parse an assignment statement
    fn parse_assign_statement(&mut self) -> ParseResult<AssignStatementNode> {
        let leading_trivia = self.consume_trivia();
        let target = self.parse_expression()?;
        let equals = self.expect_kind(&TokenKind::Assign)?;
        let value = self.parse_expression()?;
        let semicolon = self.expect_kind(&TokenKind::Semicolon)?;

        Ok(AssignStatementNode {
            target,
            equals,
            value,
            semicolon,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    /// Parse a return statement
    fn parse_return_statement(&mut self) -> ParseResult<ReturnStatementNode> {
        let leading_trivia = self.consume_trivia();
        let return_token = self.expect_kind(&TokenKind::Return)?;

        // Check if there's a value to return
        let value = if self.check_kind(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expression()?)
        };

        let semicolon = self.expect_kind(&TokenKind::Semicolon)?;

        Ok(ReturnStatementNode {
            return_token,
            value,
            semicolon,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    /// Parse an if statement
    fn parse_if_statement(&mut self) -> ParseResult<IfStatementNode> {
        let leading_trivia = self.consume_trivia();
        let if_token = self.expect_kind(&TokenKind::If)?;
        
        self.push_context(ParserContext::IfCondition);
        let condition = self.parse_expression()?;
        self.pop_context();
        
        let then_branch = self.parse_block()?;
        let else_clause = if self.check_kind(&TokenKind::Else) {
            Some(self.parse_else_clause()?)
        } else {
            None
        };

        Ok(IfStatementNode {
            if_token,
            condition,
            then_branch,
            else_clause,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    /// Parse an else clause
    fn parse_else_clause(&mut self) -> ParseResult<ElseClauseNode> {
        let leading_trivia = self.consume_trivia();
        let else_token = self.expect_kind(&TokenKind::Else)?;
        let body = if self.check_kind(&TokenKind::If) {
            ElseBody::If(Box::new(self.parse_if_statement()?))
        } else {
            ElseBody::Block(self.parse_block()?)
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

    /// Parse a while statement
    fn parse_while_statement(&mut self) -> ParseResult<WhileStatementNode> {
        let leading_trivia = self.consume_trivia();
        let while_token = self.expect_kind(&TokenKind::While)?;
        
        self.push_context(ParserContext::WhileCondition);
        let condition = self.parse_expression()?;
        self.pop_context();
        
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
}