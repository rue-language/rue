//! Parsing of top-level items (functions, structs)

use crate::error::{ParseError, ParserContext};
use crate::parser::{ParseResult, Parser};
use rue_ast::*;
use rue_lexer::{Span, TokenKind};
use std::collections::HashSet;

impl Parser {
    /// Parse a function definition
    pub(crate) fn parse_function(&mut self) -> ParseResult<FunctionNode> {
        let leading_trivia = self.consume_trivia();
        let fn_token = self.expect_kind(&TokenKind::Fn)?;
        let name = self.expect_ident()?;
        
        // Add function name to suggestions
        if let TokenKind::Ident(fn_name) = &name.kind {
            self.suggestions.add_identifier("functions", fn_name.clone());
        }
        
        self.push_context(ParserContext::FunctionParameters);
        let param_list = self.parse_param_list()?;
        self.pop_context();

        // Parse optional return type
        let return_type = if self.check_kind(&TokenKind::Arrow) {
            Some(self.parse_return_type()?)
        } else {
            None
        };

        self.push_context(ParserContext::FunctionBody);
        let body = self.parse_block()?;
        self.pop_context();

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

    /// Parse a struct definition
    pub(crate) fn parse_struct_definition(&mut self) -> ParseResult<StructDefinitionNode> {
        let leading_trivia = self.consume_trivia();
        let struct_token = self.expect_kind(&TokenKind::Struct)?;
        let name = self.expect_ident()?;
        
        // Add struct name to suggestions
        if let TokenKind::Ident(struct_name) = &name.kind {
            self.suggestions.add_identifier("types", struct_name.clone());
        }
        
        let open_brace = self.expect_kind(&TokenKind::LeftBrace)?;

        self.push_context(ParserContext::StructFields);
        let mut fields = Vec::new();
        self.skip_comments();

        // Parse struct fields
        let mut field_names = HashSet::new();
        let mut field_spans = std::collections::HashMap::new();
        
        while !self.check_kind(&TokenKind::RightBrace) && !self.is_at_end() {
            let field = self.parse_struct_field_def()?;

            // Check for duplicate field names
            if let TokenKind::Ident(field_name) = &field.name.kind {
                if let Some(&prev_span) = field_spans.get(field_name) {
                    return Err(ParseError::duplicate_field(
                        field_name,
                        field.name.span,
                        prev_span,
                        self.source_id.clone(),
                        true,
                    ));
                }
                field_names.insert(field_name.clone());
                field_spans.insert(field_name.clone(), field.name.span);
            }

            fields.push(field);
            self.skip_comments();

            // Optional comma
            if self.check_kind(&TokenKind::Comma) {
                self.advance();
                self.skip_comments();
            } else if !self.check_kind(&TokenKind::RightBrace) {
                return Err(ParseError::unexpected_token(
                    self.peek_kind(),
                    &[TokenKind::Comma, TokenKind::RightBrace],
                    self.peek().span,
                    self.source_id.clone(),
                    Some("in struct field list"),
                ));
            }
        }
        self.pop_context();

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

    /// Parse a struct field definition
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

    /// Parse a return type annotation
    fn parse_return_type(&mut self) -> ParseResult<ReturnTypeNode> {
        let leading_trivia = self.consume_trivia();
        let arrow = self.expect_kind(&TokenKind::Arrow)?;
        let return_type = self.parse_type()?;

        Ok(ReturnTypeNode {
            arrow,
            return_type,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    /// Parse a parameter list
    fn parse_param_list(&mut self) -> ParseResult<ParamListNode> {
        let leading_trivia = self.consume_trivia();
        let open_paren = self.expect_kind(&TokenKind::LeftParen)?;
        let mut params = Vec::new();
        
        // Track parameter names for better error messages
        let mut param_names = HashSet::new();

        while !self.check_kind(&TokenKind::RightParen) && !self.is_at_end() {
            let param = self.parse_parameter()?;
            
            // Check for duplicate parameter names
            if let TokenKind::Ident(param_name) = &param.name.kind {
                if !param_names.insert(param_name.clone()) {
                    // We could make this an error, but for now just track it
                    // The semantic analyzer will catch this
                }
                
                // Add parameter to local scope suggestions
                self.suggestions.add_identifier("local", param_name.clone());
            }
            
            params.push(param);

            if !self.check_kind(&TokenKind::RightParen) {
                self.expect_kind(&TokenKind::Comma)?;
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

    /// Parse a single parameter
    fn parse_parameter(&mut self) -> ParseResult<ParameterNode> {
        let leading_trivia = self.consume_trivia();
        let name = self.expect_ident()?;
        let type_annotation = self.parse_type_annotation()?;

        Ok(ParameterNode {
            name,
            type_annotation,
            comma: None,
            trivia: Trivia {
                leading: leading_trivia,
                trailing: self.consume_trivia(),
            },
        })
    }

    /// Parse a type annotation (: Type)
    fn parse_type_annotation(&mut self) -> ParseResult<TypeAnnotationNode> {
        let leading_trivia = self.consume_trivia();
        let colon = self.expect_kind(&TokenKind::Colon)?;
        
        self.push_context(ParserContext::Type);
        let ty = self.parse_type()?;
        self.pop_context();

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