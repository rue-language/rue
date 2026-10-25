//! The parse driver: lexing, deferred-group scheduling, and tree assembly.

use crate::build::{Built, build};
use crate::input::Input;
use crate::parser::Parser;
use crate::{Edition, GreenNode, SyntaxKind, SyntaxKind::*, SyntaxNode, TextRange, grammar};
use rayon::prelude::*;
use std::fmt;

/// How a parse uses the deferred-group machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Parallelism {
    /// Parse everything inline, in one pass.
    Inline,
    /// Defer bodies and parse them one after another. Produces the same tree
    /// as the other modes; useful for testing deferral without threads.
    Deferred,
    /// Defer bodies and parse them on the rayon thread pool.
    #[default]
    Parallel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParseOptions {
    pub edition: Edition,
    pub parallelism: Parallelism,
}

/// A syntax error with its location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SyntaxError {
    pub range: TextRange,
    pub message: String,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.range, self.message)
    }
}

/// The result of parsing a file: a lossless tree and the errors found.
///
/// Parsing never fails: malformed input produces `ERROR` nodes and entries in
/// [`Parse::errors`], and the tree still covers every byte of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse {
    pub(crate) green: GreenNode,
    pub(crate) errors: Vec<SyntaxError>,
}

impl Parse {
    pub fn green(&self) -> &GreenNode {
        &self.green
    }

    pub fn syntax_node(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    /// Errors in source order.
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    pub fn debug_dump(&self) -> String {
        let mut out = self.syntax_node().debug_dump();
        for error in &self.errors {
            out.push_str(&format!("error {error}\n"));
        }
        out
    }
}

/// Parses a Rust source file.
pub fn parse(text: &str, options: ParseOptions) -> Parse {
    let _span = tracing::debug_span!("rust_parse", bytes = text.len()).entered();
    let tokens = rue_rust_lexer::tokenize(text);
    let input = Input::new(text, &tokens, options.edition);
    let built = parse_window(&input, 0, input.len(), SOURCE_FILE, options);
    let parse = Parse {
        green: built.node,
        errors: sorted(built.errors),
    };
    tracing::debug!(
        tokens = tokens.len(),
        errors = parse.errors.len(),
        "rust parse complete"
    );
    parse
}

/// Errors are produced in event order, which is source order except where a
/// lexical error and a parser error meet; sorting by position (stably, so
/// equal positions keep event order) gives one canonical order that an
/// incremental reparse can maintain.
pub(crate) fn sorted(mut errors: Vec<SyntaxError>) -> Vec<SyntaxError> {
    errors.sort_by_key(|error| error.range.start);
    errors
}

/// Parses parser tokens `start..end` of `input` as a `kind` node:
/// `SOURCE_FILE` for a whole file, otherwise a reparse root.
pub(crate) fn parse_window(
    input: &Input<'_>,
    start: usize,
    end: usize,
    kind: SyntaxKind,
    options: ParseOptions,
) -> Built {
    // A statement list's own nested blocks are cheap and numerous; deferring
    // them would cost more in scheduling than it saves.
    let defer = options.parallelism != Parallelism::Inline && kind != STMT_LIST;
    let mut parser = Parser::new(input, start, end, defer);
    if kind == SOURCE_FILE {
        grammar::source_file(&mut parser);
    } else {
        grammar::parse_group(&mut parser, kind);
    }
    let (events, deferred) = parser.finish();

    let raw_ends: Vec<usize> = deferred
        .iter()
        .map(|group| input.raw_index(group.end - 1) + 1)
        .collect();
    let parse_one = |group: &crate::parser::DeferredGroup| {
        Some(parse_window(
            input,
            group.start,
            group.end,
            group.kind,
            options,
        ))
    };
    let results: Vec<Option<Built>> =
        if options.parallelism == Parallelism::Parallel && deferred.len() > 1 {
            deferred.par_iter().map(parse_one).collect()
        } else {
            deferred.iter().map(parse_one).collect()
        };

    let (raw_start, raw_end) = if kind == SOURCE_FILE {
        (0, input.raw.len())
    } else {
        (input.raw_index(start), input.raw_index(end - 1) + 1)
    };
    build(input, raw_start, raw_end, events, results, &raw_ends)
}
