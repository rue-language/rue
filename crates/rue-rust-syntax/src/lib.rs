//! An experimental Rust syntax frontend designed for incremental and parallel
//! parsing.
//!
//! The crate turns Rust source text into a lossless concrete syntax tree:
//! every byte of the input, including whitespace and comments, is in the
//! tree, and malformed input still produces a full tree plus a list of
//! errors. It is not wired into the Rue compiler; it is groundwork for a
//! possible shared Rust/Rue frontend.
//!
//! # Architecture
//!
//! ```text
//! text ──lex──▶ tokens ──match delimiters──▶ Input ──parse──▶ events ──build──▶ green tree
//!                                                   │                  ▲
//!                                                   └─ deferred groups ┘ (parsed in parallel)
//! ```
//!
//! - [`rue_rust_lexer`] finds token boundaries, context-free and losslessly.
//! - The parser pairs delimiters up front, then parses with lookahead bounded
//!   by the innermost delimited group. A group's parse depends only on its own
//!   tokens.
//! - That lets the parser *defer* bodies (function bodies, `impl`, `trait`
//!   and `mod` blocks, large macro bodies): it skips them in O(1) and the
//!   driver parses them in parallel ([`Parallelism::Parallel`]). Deferral is
//!   invisible in the output: every [`Parallelism`] mode produces the same
//!   tree.
//! - The same property powers [`Document::edit`]: an edit inside a group is
//!   handled by reparsing just that group and splicing it in, and the result
//!   is identical to a from-scratch parse.
//! - The tree is a green/red tree in the style of rust-analyzer's rowan:
//!   immutable, position-independent [`GreenNode`]s shared with `Arc`, and a
//!   [`SyntaxNode`] view that adds offsets and parents. Untouched subtrees are
//!   shared by pointer across edits, which gives later incremental phases a
//!   cheap "did this item change?" test.

mod build;
mod grammar;
mod green;
mod incremental;
mod input;
mod parse;
mod parser;
mod syntax;
mod syntax_kind;
mod token_set;

#[cfg(test)]
mod tests;

pub use green::{GreenElement, GreenNode, GreenToken};
pub use incremental::{Document, Reparse};
pub use parse::{Parallelism, Parse, ParseOptions, SyntaxError, parse};
pub use syntax::{SyntaxElement, SyntaxNode, SyntaxToken, TextRange};
pub use syntax_kind::{Edition, SyntaxKind};
pub use token_set::TokenSet;
