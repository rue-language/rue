//! The Rue parser with enhanced diagnostics
//!
//! This parser uses a recursive descent approach with rich error messages
//! and error recovery capabilities.

pub mod error;
pub mod expressions;
pub mod items;
pub mod parser;
pub mod simple_snapshot;
pub mod snapshot;
pub mod statements;
pub mod types;

use rue_ast::*;
use rue_diagnostic::{Diagnostic, SourceId, SourceManager};
use rue_lexer::{Lexer, Span, TokenKind};

pub use error::{ParseError, ParserContext};
pub use parser::{ParseResult, Parser};

/// Parse source code into a CST with enhanced diagnostics
pub fn parse_with_diagnostics(source: &str, source_name: &str) -> Result<CstRoot, Vec<Diagnostic>> {
    // Create source manager
    let mut source_manager = SourceManager::new();
    let source_id = source_manager.add_source(source_name, source);

    // Tokenize
    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(tokens) => tokens,
        Err(e) => {
            // Convert lexer error to diagnostic
            let diagnostic = Diagnostic::error(format!("Lexical error: {}", e.message), source_id)
                .with_primary_span(Span::new(e.position, e.position + 1))
                .build();
            return Err(vec![diagnostic]);
        }
    };

    // Convert tokens to TokenNodes with trivia
    let token_nodes = tokens_to_nodes(tokens);

    // Parse
    let parser = Parser::new(token_nodes, source_id);
    match parser.parse() {
        Ok(cst) => Ok(cst),
        Err(e) => Err(vec![e.diagnostic]),
    }
}

/// Parse source code into a CST (compatibility function)
pub fn parse(source: &str) -> ParseResult<CstRoot> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| {
        ParseError::new(
            Diagnostic::error(format!("Lexical error: {}", e.message), SourceId::stdin())
                .with_primary_span(Span::new(e.position, e.position + 1))
                .build(),
        )
    })?;

    let token_nodes = tokens_to_nodes(tokens);
    let parser = Parser::new(token_nodes, SourceId::stdin());
    parser.parse()
}

/// Convert raw tokens to TokenNodes with trivia
fn tokens_to_nodes(tokens: Vec<rue_lexer::Token>) -> Vec<TokenNode> {
    let mut nodes = Vec::new();
    let mut pending_trivia = Vec::new();

    for token in tokens {
        match token.kind {
            TokenKind::Comment(_) => {
                pending_trivia.push(TokenNode {
                    kind: token.kind,
                    span: token.span,
                    trivia: Trivia {
                        leading: vec![],
                        trailing: vec![],
                    },
                });
            }
            _ => {
                nodes.push(TokenNode {
                    kind: token.kind,
                    span: token.span,
                    trivia: Trivia {
                        leading: pending_trivia.clone(),
                        trailing: vec![],
                    },
                });
                pending_trivia.clear();
            }
        }
    }

    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_function() {
        let source = "fn main() -> i32 { 42 }";
        let result = parse(source);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_with_diagnostics() {
        let source = "fn main() -> i32 { 42 }";
        let result = parse_with_diagnostics(source, "test.rue");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_error_diagnostic() {
        let source = "fn main() -> i32 { 42"; // Missing closing brace
        let result = parse_with_diagnostics(source, "test.rue");
        assert!(result.is_err());
        
        if let Err(diagnostics) = result {
            assert!(!diagnostics.is_empty());
            let diag = &diagnostics[0];
            assert!(diag.is_error());
            // Should have a helpful error message about unclosed delimiter
        }
    }
}