//! Type parsing

use crate::error::{ParseError, ParserContext};
use crate::parser::{ParseResult, Parser};
use rue_ast::*;
use rue_lexer::{Span, TokenKind};

impl Parser {
    /// Parse a type
    pub(crate) fn parse_type(&mut self) -> ParseResult<TypeNode> {
        self.push_context(ParserContext::Type);
        let result = self.parse_type_impl();
        self.pop_context();
        result
    }

    fn parse_type_impl(&mut self) -> ParseResult<TypeNode> {
        let leading_trivia = self.consume_trivia();

        match self.peek_kind() {
            TokenKind::I32 => {
                let token = self.advance();
                Ok(TypeNode::I32(I32TypeNode {
                    token,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }))
            }
            TokenKind::I64 => {
                let token = self.advance();
                Ok(TypeNode::I64(I64TypeNode {
                    token,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }))
            }
            TokenKind::Bool => {
                let token = self.advance();
                Ok(TypeNode::Bool(BoolTypeNode {
                    token,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }))
            }
            TokenKind::Unit => {
                let token = self.advance();
                Ok(TypeNode::Unit(UnitTypeNode {
                    token,
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                }))
            }
            TokenKind::Ident(_) => {
                // Could be a struct type or start of generic type
                self.parse_struct_type()
            }
            TokenKind::LeftParen => {
                // Tuple type
                self.parse_tuple_type()
            }
            TokenKind::LeftBracket => {
                // Array type
                self.parse_array_type()
            }
            _ => Err(ParseError::unexpected_token(
                self.peek_kind(),
                &[
                    TokenKind::I32,
                    TokenKind::I64,
                    TokenKind::Bool,
                    TokenKind::Unit,
                    TokenKind::Ident(String::new()),
                    TokenKind::LeftParen,
                    TokenKind::LeftBracket,
                ],
                self.peek().span,
                self.source_id.clone(),
                Some("in type"),
            )),
        }
    }

    /// Parse a struct type (just an identifier for now)
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

    /// Parse a tuple type
    fn parse_tuple_type(&mut self) -> ParseResult<TypeNode> {
        let leading_trivia = self.consume_trivia();
        let open_paren = self.expect_kind(&TokenKind::LeftParen)?;
        let open_span = open_paren.span;

        // Empty tuple type ()
        if self.check_kind(&TokenKind::RightParen) {
            let close_paren = self.advance();
            return Ok(TypeNode::Unit(UnitTypeNode {
                token: TokenNode {
                    kind: TokenKind::Unit,
                    span: Span::new(open_span.start, close_paren.span.end),
                    trivia: Trivia {
                        leading: leading_trivia,
                        trailing: self.consume_trivia(),
                    },
                },
                trivia: Trivia {
                    leading: vec![],
                    trailing: vec![],
                },
            }));
        }

        let mut element_types = vec![self.parse_type()?];

        // Check if this is a single type in parentheses or a tuple
        if !self.check_kind(&TokenKind::Comma) {
            // Single type in parentheses - just return it unwrapped
            let close_paren = self.expect_kind(&TokenKind::RightParen)?;
            return Ok(element_types.into_iter().next().expect("element_types should have exactly one element"));
        }

        // It's a tuple - parse remaining elements
        self.expect_kind(&TokenKind::Comma)?;

        while !self.check_kind(&TokenKind::RightParen) && !self.is_at_end() {
            element_types.push(self.parse_type()?);

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

        Ok(TypeNode::Tuple(TupleTypeNode {
            open_paren,
            element_types,
            close_paren,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
    }

    /// Parse an array type [T; N]
    fn parse_array_type(&mut self) -> ParseResult<TypeNode> {
        let leading_trivia = self.consume_trivia();
        let open_bracket = self.expect_kind(&TokenKind::LeftBracket)?;
        let open_span = open_bracket.span;

        let element_type = Box::new(self.parse_type()?);

        // Expect semicolon separator
        self.expect_kind(&TokenKind::Semicolon)?;

        // Parse array size
        let size = self.expect_integer()?;

        // Validate array size
        if let TokenKind::Integer(size_value) = size.kind {
            if size_value < 0 {
                return Err(ParseError::invalid_array_size(
                    size_value,
                    size.span,
                    self.source_id.clone(),
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

        Ok(TypeNode::Array(ArrayTypeNode {
            open_bracket,
            element_type,
            semicolon: TokenNode {
                kind: TokenKind::Semicolon,
                span: Span::new(0, 0), // Will be fixed when we track semicolon properly
                trivia: Trivia {
                    leading: vec![],
                    trailing: vec![],
                },
            },
            size,
            close_bracket,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        }))
    }
}
