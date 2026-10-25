//! A lossless, context-free lexer for Rust source text.
//!
//! This crate is the bottom layer of an experimental Rust frontend built for
//! incremental and parallel parsing. Its contract is deliberately small:
//!
//! - **Lossless.** Every byte of the input belongs to exactly one token,
//!   including whitespace and comments, so the concatenated token texts
//!   reproduce the source.
//! - **Infallible.** Lexing never fails. Malformed input becomes a token with
//!   an error flag (for example an unterminated string) or [`TokenKind::Unknown`];
//!   reporting is left to later layers, which know the edition and context.
//! - **Context-free at token boundaries.** [`first_token`] is a pure function of
//!   the remaining text. Lexing from any token boundary therefore yields the
//!   same tokens the whole-file lex produced from that point. The incremental
//!   reparser relies on this to relex only an edited region.
//! - **Edition-independent.** Keywords, reserved prefixes and raw lifetimes are
//!   classified by the parser, which knows the edition. The lexer only finds
//!   token boundaries.
//!
//! Punctuation is lexed one character at a time. Multi-character operators
//! such as `>>=` are glued by the parser, which is the only layer that knows
//! whether `>>` closes two generic argument lists or is a shift.

mod cursor;
mod ident;
mod validate;

#[cfg(test)]
mod tests;

pub use ident::{is_id_continue, is_id_start, is_ident, is_whitespace};
pub use validate::{doc_comment_error, literal_error};

use cursor::Cursor;

/// One token: its kind and its length in bytes.
///
/// Tokens carry no offsets. Offsets are recovered by summing lengths, which
/// keeps tokens position-independent: the same token sequence is valid at any
/// place in a file, which is what lets a subtree's tokens be reused after an
/// edit shifts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token {
    pub kind: TokenKind,
    pub len: u32,
}

impl Token {
    pub const fn new(kind: TokenKind, len: u32) -> Token {
        Token { kind, len }
    }
}

/// The kind of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// `// comment`, `/// outer doc`, `//! inner doc`.
    LineComment { doc_style: Option<DocStyle> },
    /// `/* comment */`, possibly nested, possibly unterminated.
    BlockComment {
        doc_style: Option<DocStyle>,
        terminated: bool,
    },
    /// Any run of Rust whitespace (`Pattern_White_Space`).
    Whitespace,
    /// `#!/usr/bin/env ...` on the first line. Only produced by [`tokenize`].
    Shebang,

    /// An identifier or keyword: `foo`, `fn`, `_`.
    Ident,
    /// `r#foo`.
    RawIdent,
    /// An identifier immediately followed by `#`, `"` or `'`: a prefix that is
    /// reserved since the 2021 edition (`foo#`, `foo"..."`). The token covers
    /// only the identifier; the parser decides by edition whether this is an
    /// error or an ordinary identifier.
    UnknownPrefix,

    /// A literal. `suffix_start` is the byte offset of the suffix within the
    /// token (equal to `len` when there is no suffix), as in `1u8` or `"s"x`.
    Literal {
        kind: LiteralKind,
        suffix_start: u32,
    },
    /// `'a`. `starts_with_number` flags the invalid `'1`.
    Lifetime { starts_with_number: bool },
    /// `'r#a`.
    RawLifetime,

    /// `;`
    Semi,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `(`
    OpenParen,
    /// `)`
    CloseParen,
    /// `{`
    OpenBrace,
    /// `}`
    CloseBrace,
    /// `[`
    OpenBracket,
    /// `]`
    CloseBracket,
    /// `@`
    At,
    /// `#`
    Pound,
    /// `~`
    Tilde,
    /// `?`
    Question,
    /// `:`
    Colon,
    /// `$`
    Dollar,
    /// `=`
    Eq,
    /// `!`
    Bang,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `-`
    Minus,
    /// `&`
    And,
    /// `|`
    Or,
    /// `+`
    Plus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `^`
    Caret,
    /// `%`
    Percent,

    /// A character that starts no token.
    Unknown,
}

impl TokenKind {
    /// Whitespace, comments and the shebang: tokens the parser skips.
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace
                | TokenKind::LineComment { .. }
                | TokenKind::BlockComment { .. }
                | TokenKind::Shebang
        )
    }
}

/// Whether a doc comment documents the enclosing item (`//!`) or the next one
/// (`///`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocStyle {
    Outer,
    Inner,
}

/// The kind of a literal token.
///
/// Literal contents (escapes, digit validity for the base) are not checked
/// here; the flags record only what determines the token's extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiteralKind {
    /// `12`, `0x1f`, `0b`. `empty_int` is set when a base prefix has no digits.
    Int { base: Base, empty_int: bool },
    /// `1.0`, `1e10`, `1.`. `empty_exponent` is set for `1e`.
    Float { base: Base, empty_exponent: bool },
    /// `'a'`
    Char { terminated: bool },
    /// `b'a'`
    Byte { terminated: bool },
    /// `"abc"`
    Str { terminated: bool },
    /// `b"abc"`
    ByteStr { terminated: bool },
    /// `c"abc"`
    CStr { terminated: bool },
    /// `r#"abc"#`. `n_hashes` is `None` when the literal is malformed
    /// (unterminated, a bad starter, or more than 255 hashes).
    RawStr { n_hashes: Option<u8> },
    /// `br#"abc"#`
    RawByteStr { n_hashes: Option<u8> },
    /// `cr#"abc"#`
    RawCStr { n_hashes: Option<u8> },
}

/// The base of a numeric literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Base {
    Binary = 2,
    Octal = 8,
    Decimal = 10,
    Hexadecimal = 16,
}

/// Lexes the first token of `input`.
///
/// Returns `None` only for empty input. This is a pure function of `input`;
/// it never treats a leading `#!` as a shebang (see [`strip_shebang`]).
pub fn first_token(input: &str) -> Option<Token> {
    Cursor::new(input).advance_token()
}

/// Lexes a whole file.
///
/// A leading byte order mark becomes a [`TokenKind::Whitespace`] token and a
/// shebang line a single [`TokenKind::Shebang`] token. The returned token
/// lengths always sum to `input.len()`.
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(input.len() / 4 + 1);
    let mut rest = input;
    // rustc ignores a leading byte order mark; keep it as whitespace so the
    // tokens still cover the text.
    if let Some(after_bom) = rest.strip_prefix('\u{feff}') {
        tokens.push(Token::new(TokenKind::Whitespace, 3));
        rest = after_bom;
    }
    if let Some(len) = strip_shebang(rest) {
        tokens.push(Token::new(TokenKind::Shebang, len as u32));
        rest = &rest[len..];
    }
    tokenize_from(rest, &mut tokens);
    tokens
}

/// Lexes `input` as a fragment that starts at a token boundary, appending to
/// `out`. Unlike [`tokenize`], no shebang is recognized.
pub fn tokenize_from(input: &str, out: &mut Vec<Token>) {
    let mut cursor = Cursor::new(input);
    while let Some(token) = cursor.advance_token() {
        out.push(token);
    }
}

/// Returns the length of the shebang line at the start of `input`, if any.
///
/// `#!` starts a shebang unless the next non-trivia token is `[`, in which case
/// it is an inner attribute (`#![allow(x)]`, possibly split over lines).
pub fn strip_shebang(input: &str) -> Option<usize> {
    let tail = input.strip_prefix("#!")?;
    let mut cursor = Cursor::new(tail);
    let next = loop {
        match cursor.advance_token() {
            None => break None,
            Some(token) => match token.kind {
                TokenKind::Whitespace
                | TokenKind::LineComment { doc_style: None }
                | TokenKind::BlockComment {
                    doc_style: None, ..
                } => continue,
                kind => break Some(kind),
            },
        }
    };
    if next == Some(TokenKind::OpenBracket) {
        return None;
    }
    Some(2 + tail.lines().next().unwrap_or_default().len())
}
