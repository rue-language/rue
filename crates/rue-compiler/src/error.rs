//! Unified error type for the Rue compiler
//!
//! This module provides a unified error type that aggregates all the different
//! error types from various compilation phases.

use rue_codegen::{CodegenError, LoweringError};
use rue_lexer::{LexError, Span};
use rue_parser::ParseError;
use rue_semantic::SemanticError;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RueError {
    #[error("lexical error at position {}: {}", .0.position, .0.message)]
    Lexical(#[from] LexError),

    #[error("{0}")]
    Parse(#[from] ParseError),

    #[error("{0}")]
    Semantic(#[from] SemanticError),

    #[error("lowering error: {0}")]
    Lowering(#[from] LoweringError),

    #[error("codegen error: {0}")]
    Codegen(#[from] CodegenError),

    #[error("compilation error: {0}")]
    Compile(String),

    #[error("MIR verification failed")]
    MirVerificationFailed,

    #[error("I/O error: {0}")]
    Io(String),
}

impl RueError {
    /// Format the error with source context if available
    pub fn format_with_source(&self, source: &str) -> String {
        match self {
            RueError::Parse(e) => e.format_with_source(source),
            RueError::Semantic(e) => e.format_with_source(source),
            _ => self.to_string(),
        }
    }

    /// Get the span of the error if available
    pub fn span(&self) -> Option<Span> {
        match self {
            RueError::Parse(e) => Some(e.span),
            RueError::Semantic(e) => Some(e.span),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RueError {
    fn from(error: std::io::Error) -> Self {
        RueError::Io(error.to_string())
    }
}
