//! Syntax policy shared by parser implementations.
//!
//! These modules own behavior that is defined in terms of Rue tokens, AST
//! nodes, and diagnostics. Parser-library adapters remain with their parser.

pub(crate) mod condition;
pub(crate) mod diagnostics;
pub(crate) mod nesting;
pub(crate) mod recovery;
