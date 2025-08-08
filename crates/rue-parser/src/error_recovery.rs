//! Error recovery support for the parser
//!
//! This module provides mechanisms to recover from parse errors and continue
//! parsing to find multiple errors in a single pass.

use crate::{ParseError, Parser};
use rue_ast::*;
use rue_diagnostic::recovery::{RecoveryState, SyncPoint};
use rue_diagnostic::{Diagnostic, DiagnosticCollector};
use rue_lexer::{Span, TokenKind};
use std::collections::HashSet;

/// Extension trait for Parser to add error recovery capabilities
pub trait ErrorRecoveryExt {
    /// Try to recover from an error by skipping to a synchronization point
    fn recover_to_sync_point(&mut self, sync_points: &[SyncPoint]) -> bool;

    /// Skip tokens until we find one that matches the predicate
    fn skip_until<F>(&mut self, predicate: F) -> bool
    where
        F: Fn(&TokenKind) -> bool;

    /// Try to recover from an error in a statement context
    fn recover_statement(&mut self) -> Option<StatementNode>;

    /// Try to recover from an error in an expression context  
    fn recover_expression(&mut self) -> Option<ExpressionNode>;

    /// Check if we should continue parsing after an error
    fn should_continue_after_error(&self) -> bool;
}

impl ErrorRecoveryExt for Parser {
    fn recover_to_sync_point(&mut self, sync_points: &[SyncPoint]) -> bool {
        // Skip tokens until we find a synchronization point
        while !self.is_at_end() {
            let token_kind = &self.peek().kind;

            // Check if current token matches any sync point
            for sync_point in sync_points {
                if matches_sync_point(token_kind, sync_point) {
                    return true;
                }
            }

            // Skip the current token and continue
            self.advance();
        }

        false
    }

    fn skip_until<F>(&mut self, predicate: F) -> bool
    where
        F: Fn(&TokenKind) -> bool,
    {
        while !self.is_at_end() {
            if predicate(&self.peek().kind) {
                return true;
            }
            self.advance();
        }
        false
    }

    fn recover_statement(&mut self) -> Option<StatementNode> {
        // Common recovery strategy for statements:
        // Skip to next semicolon, closing brace, or statement keyword

        let sync_points = [
            SyncPoint::Semicolon,
            SyncPoint::RightBrace,
            SyncPoint::StatementKeyword,
        ];

        if self.recover_to_sync_point(&sync_points) {
            // If we found a semicolon, consume it
            if self.check_kind(&TokenKind::Semicolon) {
                self.advance();
            }

            // Create an error statement node (placeholder)
            Some(create_error_statement())
        } else {
            None
        }
    }

    fn recover_expression(&mut self) -> Option<ExpressionNode> {
        // Common recovery strategy for expressions:
        // Skip to next comma, semicolon, or closing delimiter

        let sync_points = [SyncPoint::Semicolon, SyncPoint::AnyClosingDelimiter];

        if self.recover_to_sync_point(&sync_points) {
            // Create an error expression node (placeholder)
            Some(create_error_expression())
        } else {
            None
        }
    }

    fn should_continue_after_error(&self) -> bool {
        // Continue parsing unless we're at the end or have hit too many errors
        !self.is_at_end()
    }
}

/// Check if a token matches a synchronization point
fn matches_sync_point(token_kind: &TokenKind, sync_point: &SyncPoint) -> bool {
    match sync_point {
        SyncPoint::Semicolon => matches!(token_kind, TokenKind::Semicolon),
        SyncPoint::RightBrace => matches!(token_kind, TokenKind::RightBrace),
        SyncPoint::RightParen => matches!(token_kind, TokenKind::RightParen),
        SyncPoint::RightBracket => matches!(token_kind, TokenKind::RightBracket),
        SyncPoint::Eof => matches!(token_kind, TokenKind::Eof),
        SyncPoint::StatementKeyword => matches!(
            token_kind,
            TokenKind::Let
                | TokenKind::Return
                | TokenKind::If
                | TokenKind::While
                | TokenKind::Fn
                | TokenKind::Struct
        ),
        SyncPoint::AnyClosingDelimiter => matches!(
            token_kind,
            TokenKind::RightBrace | TokenKind::RightParen | TokenKind::RightBracket
        ),
    }
}

/// Create a placeholder error statement node
fn create_error_statement() -> StatementNode {
    // Create a minimal error statement that won't cause issues downstream
    StatementNode::Expression(ExpressionStatementNode {
        expression: create_error_expression(),
        semicolon: TokenNode {
            kind: TokenKind::Semicolon,
            span: Span::new(0, 0),
        },
        trivia: Trivia {
            leading: vec![],
            trailing: vec![],
        },
    })
}

/// Create a placeholder error expression node
fn create_error_expression() -> ExpressionNode {
    // Create a minimal error expression (using unit literal as placeholder)
    ExpressionNode::Literal(TokenNode {
        kind: TokenKind::Unit,
        span: Span::new(0, 0),
    })
}

/// Parser wrapper that collects multiple errors
pub struct RecoveringParser {
    parser: Parser,
    diagnostics: DiagnosticCollector,
    recovery_state: RecoveryState,
    /// Track spans we've already reported errors for to avoid duplicates
    error_spans: HashSet<Span>,
    /// Track the last token position we recovered from
    last_recovery_position: Option<usize>,
}

impl RecoveringParser {
    /// Create a new recovering parser
    pub fn new(tokens: Vec<TokenNode>) -> Self {
        Self {
            parser: Parser::new(tokens),
            diagnostics: DiagnosticCollector::new(100), // Max 100 errors
            recovery_state: RecoveryState::new(50),     // Max 50 recovery attempts
            error_spans: HashSet::new(),
            last_recovery_position: None,
        }
    }

    /// Parse with error recovery, collecting multiple errors
    pub fn parse_with_recovery(mut self) -> (Option<CstRoot>, Vec<ParseError>) {
        let mut items = Vec::new();
        let leading_trivia = self.parser.consume_trivia();

        while !self.parser.is_at_end() {
            // Try to parse an item
            match self.try_parse_item() {
                Ok(item) => {
                    items.push(item);
                    self.recovery_state.exit_recovery();
                }
                Err(error) => {
                    // Check if we've already reported an error at this location
                    if !self.error_spans.contains(&error.span) {
                        // Convert ParseError to Diagnostic
                        let diagnostic = Diagnostic::error(
                            error.message.clone(),
                            rue_diagnostic::SourceId::stdin(),
                        )
                        .with_primary_span(error.span)
                        .build();
                        self.diagnostics.push(diagnostic);
                        self.error_spans.insert(error.span);
                    }

                    // Check if we should continue
                    if !self.recovery_state.should_continue() {
                        break;
                    }

                    // Enter recovery mode
                    self.recovery_state.enter_recovery();

                    // Try to recover
                    if !self.recover_from_item_error() {
                        break;
                    }
                }
            }
        }

        let cst = if items.is_empty() && self.diagnostics.has_errors() {
            None
        } else {
            Some(CstRoot {
                items,
                trivia: Trivia {
                    leading: leading_trivia,
                    trailing: vec![],
                },
            })
        };

        // Convert diagnostics to ParseErrors for compatibility
        let errors: Vec<ParseError> = self
            .diagnostics
            .into_diagnostics()
            .into_iter()
            .map(|d| {
                let span = d.primary_span().unwrap_or(Span::new(0, 0));
                ParseError {
                    message: d.message,
                    span,
                }
            })
            .collect();

        (cst, errors)
    }

    /// Try to parse an item
    fn try_parse_item(&mut self) -> Result<CstNode, ParseError> {
        match self.parser.peek().kind {
            TokenKind::Fn => Ok(CstNode::Function(Box::new(self.parser.parse_function()?))),
            TokenKind::Struct => Ok(CstNode::StructDefinition(Box::new(
                self.parser.parse_struct_definition()?,
            ))),
            _ => {
                let stmt = self.parser.parse_statement()?;
                Ok(CstNode::Statement(Box::new(stmt)))
            }
        }
    }

    /// Try to recover from an error while parsing an item
    fn recover_from_item_error(&mut self) -> bool {
        // Get current position to check if we're making progress
        let current_pos = self.parser.current_position();

        // If we're at the same position as last recovery, force advance
        if let Some(last_pos) = self.last_recovery_position
            && current_pos == last_pos
        {
            // We're stuck at the same position, force advance to prevent infinite loop
            if !self.parser.is_at_end() {
                self.parser.advance();
            }
        }

        self.last_recovery_position = Some(current_pos);

        // Try to skip to the next item boundary
        let sync_points = [
            SyncPoint::StatementKeyword,
            SyncPoint::RightBrace,
            SyncPoint::Eof,
            SyncPoint::Semicolon,
        ];

        // Skip tokens until we find a good recovery point
        let mut skipped = 0;
        while !self.parser.is_at_end() && skipped < 50 {
            // Limit how many tokens we skip
            let token_kind = &self.parser.peek().kind;

            // Check if we found a sync point
            for sync_point in &sync_points {
                if matches_sync_point(token_kind, sync_point) {
                    // If it's a semicolon, consume it to move past the error
                    if matches!(token_kind, TokenKind::Semicolon) {
                        self.parser.advance();
                    }
                    return true;
                }
            }

            self.parser.advance();
            skipped += 1;
        }

        // If we've reached the end or skipped too many tokens, stop recovery
        !self.parser.is_at_end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;

    fn tokenize(source: &str) -> Vec<TokenNode> {
        let mut lexer = Lexer::new(source);
        lexer.tokenize().unwrap()
    }

    #[test]
    fn test_error_recovery_multiple_errors() {
        let source = r#"
            fn main() {
                let x = ;  // Error 1: missing expression
                let y = 42
                // Error 2: missing semicolon (should recover)
                let z = 10;
                return x + y + z
                // Error 3: missing semicolon
            }
        "#;

        let tokens = tokenize(source);
        let parser = RecoveringParser::new(tokens);
        let (cst, errors) = parser.parse_with_recovery();

        // Should have collected multiple errors
        assert!(errors.len() >= 2);

        // Should still produce a partial CST
        assert!(cst.is_some());
    }

    #[test]
    fn test_recovery_sync_points() {
        // Use a source that will tokenize but has parser errors
        let source = r#"
            fn foo() {
                let x = ;    // Missing expression (parser error)
                let y = 5;   // Should recover here
            }
        "#;

        let tokens = tokenize(source);
        let parser = RecoveringParser::new(tokens);
        let (_cst, errors) = parser.parse_with_recovery();

        // Should have found errors
        assert!(!errors.is_empty());
        // Should have recovered and continued parsing
        assert!(
            errors.len() < 10,
            "Should not have excessive errors from failed recovery"
        );
    }

    #[test]
    fn test_lexer_error_handling() {
        // Test that lexer errors are handled properly
        let source = r#"
            fn foo() {
                let x = @#$  // Invalid characters - lexer error
                let y = 5;
            }
        "#;

        // The lexer will fail on invalid characters
        let mut lexer = Lexer::new(source);
        let result = lexer.tokenize();

        // Verify that lexer errors are produced for invalid characters
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Unexpected character"));
    }
}
