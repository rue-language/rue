//! Core parser structure and utilities

use crate::error::{ParseError, ParserContext};
use rue_ast::*;
use rue_diagnostic::{DiagnosticCollector, SourceId, SuggestionEngine};
use rue_lexer::{Span, TokenKind};

pub type ParseResult<T> = Result<T, ParseError>;

/// Main parser structure
pub struct Parser {
    pub(crate) tokens: Vec<TokenNode>,
    pub(crate) current: usize,
    pub(crate) source_id: SourceId,
    pub(crate) diagnostics: DiagnosticCollector,
    pub(crate) suggestions: SuggestionEngine,
    pub(crate) context_stack: Vec<ParserContext>,
}

impl Parser {
    /// Create a new parser
    pub fn new(tokens: Vec<TokenNode>, source_id: SourceId) -> Self {
        let mut suggestions = SuggestionEngine::new();

        // Add built-in identifiers for suggestions
        suggestions.add_scope(
            "builtins",
            vec![
                "fn".to_string(),
                "let".to_string(),
                "if".to_string(),
                "else".to_string(),
                "while".to_string(),
                "return".to_string(),
                "true".to_string(),
                "false".to_string(),
                "i32".to_string(),
                "i64".to_string(),
                "bool".to_string(),
            ],
        );

        Self {
            tokens,
            current: 0,
            source_id,
            diagnostics: DiagnosticCollector::new(100),
            suggestions,
            context_stack: Vec::new(),
        }
    }

    /// Parse the token stream into a CST
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

    /// Push a parsing context
    pub(crate) fn push_context(&mut self, context: ParserContext) {
        self.context_stack.push(context);
    }

    /// Pop a parsing context
    pub(crate) fn pop_context(&mut self) {
        self.context_stack.pop();
    }

    /// Get the current parsing context
    pub(crate) fn current_context(&self) -> Option<ParserContext> {
        self.context_stack.last().copied()
    }

    /// Get a context description for error messages
    pub(crate) fn context_description(&self) -> Option<&'static str> {
        self.current_context().map(|c| c.description())
    }

    // Token manipulation methods

    pub(crate) fn peek(&self) -> &TokenNode {
        self.tokens.get(self.current).unwrap_or(&TokenNode {
            kind: TokenKind::Eof,
            span: self.last_span(),
            trivia: Trivia {
                leading: vec![],
                trailing: vec![],
            },
        })
    }

    pub(crate) fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    pub(crate) fn advance(&mut self) -> TokenNode {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.tokens[self.current - 1].clone()
    }

    pub(crate) fn is_at_end(&self) -> bool {
        self.peek_kind() == &TokenKind::Eof
    }

    pub(crate) fn check_kind(&self, kind: &TokenKind) -> bool {
        if self.is_at_end() {
            false
        } else {
            std::mem::discriminant(self.peek_kind()) == std::mem::discriminant(kind)
        }
    }

    pub(crate) fn match_kinds(&mut self, kinds: &[TokenKind]) -> bool {
        for kind in kinds {
            if self.check_kind(kind) {
                self.advance();
                return true;
            }
        }
        false
    }

    pub(crate) fn expect_kind(&mut self, kind: &TokenKind) -> ParseResult<TokenNode> {
        if self.check_kind(kind) {
            Ok(self.advance())
        } else {
            Err(ParseError::unexpected_token(
                self.peek_kind(),
                &[kind.clone()],
                self.peek().span,
                self.source_id.clone(),
                self.context_description(),
            ))
        }
    }

    pub(crate) fn expect_ident(&mut self) -> ParseResult<TokenNode> {
        match self.peek_kind() {
            TokenKind::Ident(_) => Ok(self.advance()),
            _ => {
                let context = self.context_description().unwrap_or("here");
                Err(ParseError::expected_identifier(
                    self.peek_kind(),
                    self.peek().span,
                    self.source_id.clone(),
                    context,
                ))
            }
        }
    }

    pub(crate) fn expect_integer(&mut self) -> ParseResult<TokenNode> {
        match self.peek_kind() {
            TokenKind::Integer(_) => Ok(self.advance()),
            _ => Err(ParseError::unexpected_token(
                self.peek_kind(),
                &[TokenKind::Integer(0)],
                self.peek().span,
                self.source_id.clone(),
                Some("expected integer literal"),
            )),
        }
    }

    pub(crate) fn last_span(&self) -> Span {
        if self.current > 0 {
            self.tokens[self.current - 1].span
        } else if !self.tokens.is_empty() {
            self.tokens[0].span
        } else {
            Span::new(0, 0)
        }
    }

    // Trivia handling

    pub(crate) fn consume_trivia(&mut self) -> Vec<TokenNode> {
        let mut trivia = Vec::new();
        while let TokenKind::Comment(_) = self.peek_kind() {
            trivia.push(self.advance());
        }
        trivia
    }

    pub(crate) fn skip_comments(&mut self) {
        while let TokenKind::Comment(_) = self.peek_kind() {
            self.advance();
        }
    }

    // Parse top-level item
    fn parse_item(&mut self) -> ParseResult<CstNode> {
        match self.peek_kind() {
            TokenKind::Fn => {
                self.push_context(ParserContext::FunctionDefinition);
                let result = self.parse_function();
                self.pop_context();
                result.map(|f| CstNode::Function(Box::new(f)))
            }
            TokenKind::Struct => {
                self.push_context(ParserContext::StructDefinition);
                let result = self.parse_struct_definition();
                self.pop_context();
                result.map(|s| CstNode::StructDefinition(Box::new(s)))
            }
            _ => {
                self.push_context(ParserContext::Statement);
                let result = self.parse_statement();
                self.pop_context();
                result.map(|s| CstNode::Statement(Box::new(s)))
            }
        }
    }

    // These methods are implemented in other modules via impl blocks:
    // - parse_function() in items.rs
    // - parse_struct_definition() in items.rs
    // - parse_statement() in statements.rs
    // - parse_expression() in expressions.rs
    // - parse_type() in types.rs
    // - parse_block() in statements.rs
}
