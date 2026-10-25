//! The parser's view of a lexed text: non-trivia tokens, their jointness, and
//! the delimiter structure.

use crate::{Edition, SyntaxKind, SyntaxKind::*};
use rue_rust_lexer::{LiteralKind, Token, TokenKind};

/// Marks a `group_end` entry whose opener has no matching closer.
const UNCLOSED: u32 = 1 << 31;
/// `group_end` value for tokens that are not opening delimiters.
const NOT_AN_OPENER: u32 = u32::MAX;

/// Lexed text prepared for parsing.
///
/// Trivia is invisible to the parser; it sees only "parser tokens", each of
/// which maps back to one raw lexer token. The tree builder re-inserts the
/// trivia from the raw token list.
pub(crate) struct Input<'t> {
    pub(crate) text: &'t str,
    pub(crate) raw: &'t [Token],
    /// `raw_kinds[i]` is the syntax kind of raw token `i`, with keywords
    /// resolved for the edition.
    pub(crate) raw_kinds: Vec<SyntaxKind>,
    /// `raw_starts[i]` is the byte offset of raw token `i`; one extra entry
    /// holds the text length.
    pub(crate) raw_starts: Vec<u32>,
    /// The kinds of the parser tokens.
    kinds: Vec<SyntaxKind>,
    /// `raw_index[i]` is the raw token behind parser token `i`.
    raw_index: Vec<u32>,
    /// Bit `i` is set when parser token `i + 1` immediately follows parser
    /// token `i` with no trivia between them, so that the two may be glued
    /// into one composite token such as `::`.
    joint: Vec<u64>,
    /// For each opening delimiter, the parser index of its closer, or (with
    /// `UNCLOSED` set) of the token at which the group was cut short.
    group_end: Vec<u32>,
    pub(crate) edition: Edition,
}

/// The extent of a delimited group, from its opener.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Group {
    /// The parser index of the closer, or of the first token after the group
    /// when it is unclosed.
    pub(crate) end: usize,
    pub(crate) closed: bool,
}

impl<'t> Input<'t> {
    pub(crate) fn new(text: &'t str, raw: &'t [Token], edition: Edition) -> Input<'t> {
        let mut raw_kinds = Vec::with_capacity(raw.len());
        let mut raw_starts = Vec::with_capacity(raw.len() + 1);
        let mut kinds = Vec::with_capacity(raw.len() / 2);
        let mut raw_index = Vec::with_capacity(raw.len() / 2);
        let mut offset = 0u32;
        for (i, token) in raw.iter().enumerate() {
            raw_starts.push(offset);
            let token_text = &text[offset as usize..(offset + token.len) as usize];
            let kind = syntax_kind(token.kind, token_text, edition);
            raw_kinds.push(kind);
            if !kind.is_trivia() {
                kinds.push(kind);
                raw_index.push(i as u32);
            }
            offset += token.len;
        }
        raw_starts.push(offset);
        assert_eq!(offset as usize, text.len(), "tokens must cover the text");

        let mut joint = vec![0u64; kinds.len().div_ceil(64)];
        for i in 1..kinds.len() {
            if raw_index[i] == raw_index[i - 1] + 1 {
                joint[(i - 1) / 64] |= 1 << ((i - 1) % 64);
            }
        }

        let group_end = match_delimiters(&mut kinds);

        Input {
            text,
            raw,
            raw_kinds,
            raw_starts,
            kinds,
            raw_index,
            joint,
            group_end,
            edition,
        }
    }

    /// The number of parser tokens.
    pub(crate) fn len(&self) -> usize {
        self.kinds.len()
    }

    pub(crate) fn kind(&self, i: usize) -> SyntaxKind {
        self.kinds[i]
    }

    /// Whether parser token `i + 1` immediately follows token `i`.
    pub(crate) fn is_joint(&self, i: usize) -> bool {
        self.joint[i / 64] & (1 << (i % 64)) != 0
    }

    pub(crate) fn raw_index(&self, i: usize) -> usize {
        self.raw_index[i] as usize
    }

    /// The lexer token behind parser token `i`.
    pub(crate) fn lexer_token(&self, i: usize) -> Token {
        self.raw[self.raw_index(i)]
    }

    /// The text of parser token `i`.
    pub(crate) fn text(&self, i: usize) -> &'t str {
        let raw = self.raw_index(i);
        &self.text[self.raw_starts[raw] as usize..self.raw_starts[raw + 1] as usize]
    }

    /// The group opened by parser token `i`, if it is an opening delimiter.
    pub(crate) fn group(&self, i: usize) -> Option<Group> {
        let end = self.group_end[i];
        if end == NOT_AN_OPENER {
            return None;
        }
        Some(Group {
            end: (end & !UNCLOSED) as usize,
            closed: end & UNCLOSED == 0,
        })
    }

    /// Whether the input is exactly one balanced group: its first token opens
    /// a group closed by its last token, and no closer inside is unmatched.
    ///
    /// Under these conditions the delimiter matching of the group's tokens is
    /// independent of the text around them, which is what makes reparsing the
    /// group in isolation sound.
    pub(crate) fn is_single_balanced_group(&self) -> bool {
        let n = self.len();
        if n < 2 {
            return false;
        }
        match self.group(0) {
            Some(group) if group.closed && group.end == n - 1 => {}
            _ => return false,
        }
        // `match_delimiters` turns unmatched closers into `ERROR`; a closer
        // that is unmatched here might match an enclosing opener in the file.
        (1..n - 1).all(|i| self.kinds[i] != ERROR || !self.text(i).starts_with([')', ']', '}']))
    }
}

/// Pairs delimiters with a stack, recording each opener's extent.
///
/// A closer pops the nearest opener of its own kind; openers above it are
/// unclosed and end at that closer. A closer with no opener of its kind on the
/// stack is unmatched: its kind becomes `ERROR`, so the grammar never has to
/// consider a closer that does not end the current group. This is the same
/// recovery rustc's token-tree builder uses, and it makes the outcome of
/// matching a balanced group independent of what surrounds it.
fn match_delimiters(kinds: &mut [SyntaxKind]) -> Vec<u32> {
    let mut group_end = vec![NOT_AN_OPENER; kinds.len()];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..kinds.len() {
        let kind = kinds[i];
        if kind.closing_delimiter().is_some() {
            stack.push(i);
        } else if kind.is_closing_delimiter() {
            match stack
                .iter()
                .rposition(|&open| kinds[open].closing_delimiter() == Some(kind))
            {
                Some(depth) => {
                    for &open in &stack[depth + 1..] {
                        group_end[open] = i as u32 | UNCLOSED;
                    }
                    group_end[stack[depth]] = i as u32;
                    stack.truncate(depth);
                }
                None => kinds[i] = ERROR,
            }
        }
    }
    for open in stack {
        group_end[open] = kinds.len() as u32 | UNCLOSED;
    }
    group_end
}

/// Classifies one raw lexer token.
fn syntax_kind(kind: TokenKind, text: &str, edition: Edition) -> SyntaxKind {
    match kind {
        TokenKind::Whitespace => WHITESPACE,
        TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } => COMMENT,
        TokenKind::Shebang => SHEBANG,
        TokenKind::Ident => {
            if text == "_" {
                UNDERSCORE
            } else {
                SyntaxKind::from_keyword(text, edition).unwrap_or(IDENT)
            }
        }
        TokenKind::RawIdent | TokenKind::UnknownPrefix => IDENT,
        TokenKind::Literal { kind, .. } => match kind {
            LiteralKind::Int { .. } => INT_NUMBER,
            LiteralKind::Float { .. } => FLOAT_NUMBER,
            LiteralKind::Char { .. } => CHAR,
            LiteralKind::Byte { .. } => BYTE,
            LiteralKind::Str { .. } | LiteralKind::RawStr { .. } => STRING,
            LiteralKind::ByteStr { .. } | LiteralKind::RawByteStr { .. } => BYTE_STRING,
            LiteralKind::CStr { .. } | LiteralKind::RawCStr { .. } => C_STRING,
        },
        TokenKind::Lifetime { .. } | TokenKind::RawLifetime => LIFETIME_IDENT,
        TokenKind::Semi => SEMICOLON,
        TokenKind::Comma => COMMA,
        TokenKind::Dot => DOT,
        TokenKind::OpenParen => L_PAREN,
        TokenKind::CloseParen => R_PAREN,
        TokenKind::OpenBrace => L_CURLY,
        TokenKind::CloseBrace => R_CURLY,
        TokenKind::OpenBracket => L_BRACK,
        TokenKind::CloseBracket => R_BRACK,
        TokenKind::At => AT,
        TokenKind::Pound => POUND,
        TokenKind::Tilde => TILDE,
        TokenKind::Question => QUESTION,
        TokenKind::Colon => COLON,
        TokenKind::Dollar => DOLLAR,
        TokenKind::Eq => EQ,
        TokenKind::Bang => BANG,
        TokenKind::Lt => L_ANGLE,
        TokenKind::Gt => R_ANGLE,
        TokenKind::Minus => MINUS,
        TokenKind::And => AMP,
        TokenKind::Or => PIPE,
        TokenKind::Plus => PLUS,
        TokenKind::Star => STAR,
        TokenKind::Slash => SLASH,
        TokenKind::Caret => CARET,
        TokenKind::Percent => PERCENT,
        TokenKind::Unknown => ERROR,
    }
}

/// The lexical error carried by a raw token, if any.
pub(crate) fn lexer_error(kind: TokenKind, text: &str, edition: Edition) -> Option<String> {
    let message = match kind {
        TokenKind::BlockComment {
            terminated: false, ..
        } => "unterminated block comment",
        TokenKind::LineComment { doc_style: Some(_) }
        | TokenKind::BlockComment {
            doc_style: Some(_), ..
        } => rue_rust_lexer::doc_comment_error(text)?,
        TokenKind::Literal { kind, suffix_start } => match kind {
            LiteralKind::Int {
                empty_int: true, ..
            } => "missing digits after the integer base prefix",
            LiteralKind::Float {
                empty_exponent: true,
                ..
            } => "expected at least one digit in exponent",
            LiteralKind::Char { terminated: false } => "unterminated character literal",
            LiteralKind::Byte { terminated: false } => "unterminated byte literal",
            LiteralKind::Str { terminated: false } => "unterminated string literal",
            LiteralKind::ByteStr { terminated: false } => "unterminated byte string literal",
            LiteralKind::CStr { terminated: false } => "unterminated C string literal",
            LiteralKind::RawStr { n_hashes: None }
            | LiteralKind::RawByteStr { n_hashes: None }
            | LiteralKind::RawCStr { n_hashes: None } => "malformed raw string literal",
            _ => rue_rust_lexer::literal_error(kind, text, suffix_start)?,
        },
        TokenKind::Lifetime {
            starts_with_number: true,
        } => "lifetimes cannot start with a number",
        TokenKind::RawLifetime if edition < Edition::Edition2021 => {
            "raw lifetimes are not supported before the 2021 edition"
        }
        TokenKind::UnknownPrefix if edition >= Edition::Edition2021 => {
            return Some(format!("prefix `{text}` is unknown"));
        }
        TokenKind::Unknown => return Some(format!("unknown start of token: {text:?}")),
        _ => return None,
    };
    Some(message.to_string())
}
