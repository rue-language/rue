//! Incremental reparsing after a text edit.
//!
//! An edit is handled by reparsing the smallest *reparse root* (a block, an
//! item list, a match arm list, a token tree, ...) that strictly contains it,
//! and splicing the new subtree into the old tree. Everything outside that
//! group keeps its green nodes, shared by pointer with the previous tree.
//!
//! The splice is exact, not a heuristic: the result is always identical to
//! parsing the edited text from scratch. That rests on three facts.
//!
//! 1. **Lexing.** The edit lies strictly between the group's delimiters. The
//!    tokens before the opener are unaffected (no token's extent depends on
//!    text past a following `{`, `(` or `[`), and lexing after the closer
//!    depends only on the text after it. So if relexing the group's new text
//!    on its own still yields a single group whose closer is its last token,
//!    the whole file's token stream is the old one with the group's tokens
//!    replaced.
//! 2. **Delimiters.** If the new group is balanced, with no closer that lacks
//!    an opener inside it, delimiter matching pairs the same outer delimiters
//!    as before (see `Input::is_single_balanced_group`).
//! 3. **Parsing.** The grammar parses a group through `Parser::group`, which
//!    hides everything outside it, and the outer parse never looks inside a
//!    reparse root. So the group's subtree is a function of its own tokens,
//!    and the rest of the tree is unchanged.
//!
//! When a check fails, the reparser tries the next enclosing root or falls
//! back to a full parse.

use crate::input::Input;
use crate::parse::{Parse, ParseOptions, parse, parse_window, sorted};
use crate::{GreenElement, GreenNode, SyntaxKind, TextRange, grammar::REPARSE_ROOTS};

/// A source file kept parsed across edits.
pub struct Document {
    text: String,
    parse: Parse,
    options: ParseOptions,
}

/// How [`Document::edit`] brought the tree up to date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reparse {
    /// Only the group of `kind` now at `range` was reparsed.
    Group { kind: SyntaxKind, range: TextRange },
    /// The whole file was reparsed.
    Full,
}

/// Why a reparse root could not absorb an edit.
enum Rejection {
    /// The edit moved the group's lexical boundary (for example, an opened
    /// string literal swallowed the closer). An enclosing group may still
    /// contain the damage.
    Boundary,
    /// The group's delimiters no longer balance. Every enclosing group is
    /// unbalanced by the same edit, so only a full parse can help.
    Unbalanced,
}

/// A reparse root on the path to the edit.
struct Candidate {
    /// Child indices from the root to the node.
    path: Vec<usize>,
    offset: u32,
    node: GreenNode,
}

impl Document {
    pub fn new(text: impl Into<String>, options: ParseOptions) -> Document {
        let text = text.into();
        let parse = parse(&text, options);
        Document {
            text,
            parse,
            options,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn parse(&self) -> &Parse {
        &self.parse
    }

    /// Replaces `delete` with `insert` and updates the tree.
    ///
    /// # Panics
    ///
    /// If `delete` is out of bounds or splits a UTF-8 character.
    pub fn edit(&mut self, delete: TextRange, insert: &str) -> Reparse {
        let (start, end) = (delete.start as usize, delete.end as usize);
        assert!(start <= end && end <= self.text.len(), "edit out of bounds");
        let mut new_text = String::with_capacity(self.text.len() - (end - start) + insert.len());
        new_text.push_str(&self.text[..start]);
        new_text.push_str(insert);
        new_text.push_str(&self.text[end..]);
        let delta = insert.len() as i64 - delete.len() as i64;

        let candidates = candidates(&self.parse.green, delete);
        for candidate in candidates.iter().rev() {
            match self.reparse_group(&new_text, candidate, delta) {
                Ok(reparse) => {
                    self.text = new_text;
                    return reparse;
                }
                Err(Rejection::Boundary) => continue,
                Err(Rejection::Unbalanced) => break,
            }
        }

        self.parse = parse(&new_text, self.options);
        self.text = new_text;
        Reparse::Full
    }

    fn reparse_group(
        &mut self,
        new_text: &str,
        candidate: &Candidate,
        delta: i64,
    ) -> Result<Reparse, Rejection> {
        let kind = candidate.node.kind();
        let start = candidate.offset;
        let old_end = start + candidate.node.text_len();
        let new_end = (old_end as i64 + delta) as u32;
        let group_text = &new_text[start as usize..new_end as usize];

        let mut tokens = Vec::new();
        rue_rust_lexer::tokenize_from(group_text, &mut tokens);
        let delimited = matches!(
            (tokens.first(), tokens.last()),
            (Some(first), Some(last))
                if first.kind == opener_token(&candidate.node)
                    && is_closer(last.kind)
                    && tokens.len() >= 2
        );
        if !delimited {
            return Err(Rejection::Boundary);
        }
        let input = Input::new(group_text, &tokens, self.options.edition);
        if !input.is_single_balanced_group() {
            return Err(Rejection::Unbalanced);
        }

        let built = parse_window(&input, 0, input.len(), kind, self.options);
        assert_eq!(built.node.kind(), kind);
        assert_eq!(built.node.text_len(), new_end - start);

        self.parse.green = replace_at(&self.parse.green, &candidate.path, built.node);

        let mut errors = Vec::with_capacity(self.parse.errors.len() + built.errors.len());
        for error in self.parse.errors.drain(..) {
            if error.range.start <= start {
                errors.push(error);
            } else if error.range.start >= old_end {
                errors.push(crate::SyntaxError {
                    range: error.range.shifted(delta),
                    message: error.message,
                });
            }
            // Errors strictly inside the old group are replaced below.
        }
        errors.extend(built.errors.into_iter().map(|error| crate::SyntaxError {
            range: error.range.shifted(start as i64),
            message: error.message,
        }));
        self.parse.errors = sorted(errors);

        Ok(Reparse::Group {
            kind,
            range: TextRange::new(start, new_end),
        })
    }
}

/// The reparse roots whose interior strictly contains `edit`, outermost
/// first.
fn candidates(root: &GreenNode, edit: TextRange) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut node = root.clone();
    let mut offset = 0u32;
    let mut path = Vec::new();
    loop {
        if REPARSE_ROOTS.contains(&node.kind()) && is_closed_group(&node) {
            // Strictly inside the delimiters: after the opener's end and at
            // or before the closer's start.
            let interior = TextRange::new(offset + 1, offset + node.text_len() - 1);
            if interior.contains_range(edit) {
                out.push(Candidate {
                    path: path.clone(),
                    offset,
                    node: node.clone(),
                });
            }
        }
        let mut child_offset = offset;
        let mut next = None;
        for (index, child) in node.children().iter().enumerate() {
            let child_end = child_offset + child.text_len();
            if child_offset <= edit.start && edit.end <= child_end {
                if let GreenElement::Node(child) = child {
                    next = Some((index, child.clone(), child_offset));
                }
                break;
            }
            child_offset = child_end;
        }
        match next {
            Some((index, child, child_offset)) => {
                path.push(index);
                node = child;
                offset = child_offset;
            }
            None => return out,
        }
    }
}

/// Whether `node` starts with an opening delimiter and ends with its closer.
fn is_closed_group(node: &GreenNode) -> bool {
    let children = node.children();
    match (children.first(), children.last()) {
        (Some(GreenElement::Token(first)), Some(GreenElement::Token(last))) => {
            children.len() >= 2 && first.kind().closing_delimiter() == Some(last.kind())
        }
        _ => false,
    }
}

fn opener_token(node: &GreenNode) -> rue_rust_lexer::TokenKind {
    use rue_rust_lexer::TokenKind;
    match node.children().first().map(GreenElement::kind) {
        Some(SyntaxKind::L_PAREN) => TokenKind::OpenParen,
        Some(SyntaxKind::L_BRACK) => TokenKind::OpenBracket,
        _ => TokenKind::OpenBrace,
    }
}

fn is_closer(kind: rue_rust_lexer::TokenKind) -> bool {
    use rue_rust_lexer::TokenKind;
    matches!(
        kind,
        TokenKind::CloseParen | TokenKind::CloseBracket | TokenKind::CloseBrace
    )
}

fn replace_at(node: &GreenNode, path: &[usize], new: GreenNode) -> GreenNode {
    let Some((&index, rest)) = path.split_first() else {
        return new;
    };
    let child = match &node.children()[index] {
        GreenElement::Node(child) => child,
        GreenElement::Token(_) => unreachable!("candidate paths go through nodes"),
    };
    node.replace_child(index, GreenElement::Node(replace_at(child, rest, new)))
}
