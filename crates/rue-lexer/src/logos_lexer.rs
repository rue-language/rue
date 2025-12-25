//! Logos-based lexer for the Rue programming language.
//!
//! This module provides a lexer implementation using the logos derive macro
//! for efficient tokenization.

use logos::Logos;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::Span;

/// Error type for lexing failures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LexError {
    #[default]
    UnexpectedCharacter,
    InvalidInteger,
    InvalidStringEscape,
    UnterminatedString,
}

/// Process a string literal, handling escape sequences.
/// Input includes the surrounding quotes.
fn process_string_literal(lex: &mut logos::Lexer<LogosTokenKind>) -> Result<String, LexError> {
    let slice = lex.slice();
    // Remove surrounding quotes
    let inner = &slice[1..slice.len() - 1];

    let mut result = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => result.push('\\'),
                Some('"') => result.push('"'),
                Some(_) | None => return Err(LexError::InvalidStringEscape),
            }
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

/// Token kinds in the Rue language, using logos derive macro.
#[derive(Logos, Debug, Clone, PartialEq, Eq)]
#[logos(error = LexError)]
#[logos(skip r"[ \t\n\r\f]+")]
#[logos(skip r"//[^\n]*")]
pub enum LogosTokenKind {
    // Keywords - logos prefers longer/specific matches over shorter/generic ones
    #[token("fn")]
    Fn,
    #[token("let")]
    Let,
    #[token("mut")]
    Mut,
    #[token("inout")]
    Inout,
    #[token("borrow")]
    Borrow,
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("match")]
    Match,
    #[token("while")]
    While,
    #[token("loop")]
    Loop,
    #[token("break")]
    Break,
    #[token("continue")]
    Continue,
    #[token("return")]
    Return,
    #[token("true")]
    True,
    #[token("false")]
    False,
    #[token("struct")]
    Struct,
    #[token("enum")]
    Enum,
    #[token("impl")]
    Impl,
    #[token("drop")]
    Drop,
    #[token("self")]
    SelfValue,

    // Type keywords
    #[token("i8")]
    I8,
    #[token("i16")]
    I16,
    #[token("i32")]
    I32,
    #[token("i64")]
    I64,
    #[token("u8")]
    U8,
    #[token("u16")]
    U16,
    #[token("u32")]
    U32,
    #[token("u64")]
    U64,
    #[token("bool")]
    Bool,

    // Patterns
    #[token("_")]
    Underscore,

    // Integer literals
    #[regex(r"[0-9]+", |lex| lex.slice().parse::<u64>().ok())]
    Int(u64),

    // String literals
    #[regex(r#""([^"\\]|\\.)*""#, process_string_literal)]
    String(String),

    // Identifiers (lower priority than keywords)
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*", |lex| lex.slice().to_string(), priority = 1)]
    Ident(String),

    // Multi-character operators (logos automatically prefers longer matches)
    #[token("==")]
    EqEq,
    #[token("!=")]
    BangEq,
    #[token("<=")]
    LtEq,
    #[token(">=")]
    GtEq,
    #[token("&&")]
    AmpAmp,
    #[token("||")]
    PipePipe,
    #[token("<<")]
    LtLt,
    #[token(">>")]
    GtGt,
    #[token("->")]
    Arrow,
    #[token("=>")]
    FatArrow,
    #[token("::")]
    ColonColon,

    // Single-character operators
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("=")]
    Eq,
    #[token("!")]
    Bang,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("&")]
    Amp,
    #[token("|")]
    Pipe,
    #[token("^")]
    Caret,
    #[token("~")]
    Tilde,

    // Punctuation
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(":")]
    Colon,
    #[token(";")]
    Semi,
    #[token(",")]
    Comma,
    #[token(".")]
    Dot,
    #[token("@")]
    At,
}

use crate::{Token, TokenKind};

impl From<LogosTokenKind> for TokenKind {
    fn from(logos_kind: LogosTokenKind) -> Self {
        match logos_kind {
            LogosTokenKind::Fn => TokenKind::Fn,
            LogosTokenKind::Let => TokenKind::Let,
            LogosTokenKind::Mut => TokenKind::Mut,
            LogosTokenKind::Inout => TokenKind::Inout,
            LogosTokenKind::Borrow => TokenKind::Borrow,
            LogosTokenKind::If => TokenKind::If,
            LogosTokenKind::Else => TokenKind::Else,
            LogosTokenKind::Match => TokenKind::Match,
            LogosTokenKind::While => TokenKind::While,
            LogosTokenKind::Loop => TokenKind::Loop,
            LogosTokenKind::Break => TokenKind::Break,
            LogosTokenKind::Continue => TokenKind::Continue,
            LogosTokenKind::Return => TokenKind::Return,
            LogosTokenKind::True => TokenKind::True,
            LogosTokenKind::False => TokenKind::False,
            LogosTokenKind::Struct => TokenKind::Struct,
            LogosTokenKind::Enum => TokenKind::Enum,
            LogosTokenKind::Impl => TokenKind::Impl,
            LogosTokenKind::Drop => TokenKind::Drop,
            LogosTokenKind::SelfValue => TokenKind::SelfValue,
            LogosTokenKind::I8 => TokenKind::I8,
            LogosTokenKind::I16 => TokenKind::I16,
            LogosTokenKind::I32 => TokenKind::I32,
            LogosTokenKind::I64 => TokenKind::I64,
            LogosTokenKind::U8 => TokenKind::U8,
            LogosTokenKind::U16 => TokenKind::U16,
            LogosTokenKind::U32 => TokenKind::U32,
            LogosTokenKind::U64 => TokenKind::U64,
            LogosTokenKind::Bool => TokenKind::Bool,
            LogosTokenKind::Underscore => TokenKind::Underscore,
            LogosTokenKind::Int(n) => TokenKind::Int(n),
            LogosTokenKind::String(s) => TokenKind::String(s),
            LogosTokenKind::Ident(s) => TokenKind::Ident(s),
            LogosTokenKind::EqEq => TokenKind::EqEq,
            LogosTokenKind::BangEq => TokenKind::BangEq,
            LogosTokenKind::LtEq => TokenKind::LtEq,
            LogosTokenKind::GtEq => TokenKind::GtEq,
            LogosTokenKind::AmpAmp => TokenKind::AmpAmp,
            LogosTokenKind::PipePipe => TokenKind::PipePipe,
            LogosTokenKind::LtLt => TokenKind::LtLt,
            LogosTokenKind::GtGt => TokenKind::GtGt,
            LogosTokenKind::Arrow => TokenKind::Arrow,
            LogosTokenKind::FatArrow => TokenKind::FatArrow,
            LogosTokenKind::ColonColon => TokenKind::ColonColon,
            LogosTokenKind::Plus => TokenKind::Plus,
            LogosTokenKind::Minus => TokenKind::Minus,
            LogosTokenKind::Star => TokenKind::Star,
            LogosTokenKind::Slash => TokenKind::Slash,
            LogosTokenKind::Percent => TokenKind::Percent,
            LogosTokenKind::Eq => TokenKind::Eq,
            LogosTokenKind::Bang => TokenKind::Bang,
            LogosTokenKind::Lt => TokenKind::Lt,
            LogosTokenKind::Gt => TokenKind::Gt,
            LogosTokenKind::Amp => TokenKind::Amp,
            LogosTokenKind::Pipe => TokenKind::Pipe,
            LogosTokenKind::Caret => TokenKind::Caret,
            LogosTokenKind::Tilde => TokenKind::Tilde,
            LogosTokenKind::LParen => TokenKind::LParen,
            LogosTokenKind::RParen => TokenKind::RParen,
            LogosTokenKind::LBrace => TokenKind::LBrace,
            LogosTokenKind::RBrace => TokenKind::RBrace,
            LogosTokenKind::LBracket => TokenKind::LBracket,
            LogosTokenKind::RBracket => TokenKind::RBracket,
            LogosTokenKind::Colon => TokenKind::Colon,
            LogosTokenKind::Semi => TokenKind::Semi,
            LogosTokenKind::Comma => TokenKind::Comma,
            LogosTokenKind::Dot => TokenKind::Dot,
            LogosTokenKind::At => TokenKind::At,
        }
    }
}

/// Logos-based lexer that converts source text into tokens.
pub struct LogosLexer<'a> {
    source: &'a str,
}

impl<'a> LogosLexer<'a> {
    /// Create a new lexer for the given source text.
    pub fn new(source: &'a str) -> Self {
        Self { source }
    }

    /// Tokenize the entire source, returning all tokens.
    pub fn tokenize(&mut self) -> CompileResult<Vec<Token>> {
        // Estimate capacity: source length / 4 is a rough heuristic for token density
        let mut tokens = Vec::with_capacity(self.source.len() / 4);

        for (result, span) in LogosTokenKind::lexer(self.source).spanned() {
            match result {
                Ok(logos_kind) => {
                    tokens.push(Token {
                        kind: logos_kind.into(),
                        span: Span::new(span.start as u32, span.end as u32),
                    });
                }
                Err(lex_error) => {
                    let rue_span = Span::new(span.start as u32, span.end as u32);
                    let slice = &self.source[span.clone()];
                    let error_char = slice.chars().next().unwrap_or('?');
                    let kind = match lex_error {
                        LexError::InvalidInteger => ErrorKind::InvalidInteger,
                        LexError::UnexpectedCharacter => ErrorKind::UnexpectedCharacter(error_char),
                        LexError::InvalidStringEscape => {
                            // Find the escape character after backslash
                            let escape_char = slice
                                .find('\\')
                                .and_then(|pos| slice[pos + 1..].chars().next())
                                .unwrap_or('?');
                            ErrorKind::InvalidStringEscape(escape_char)
                        }
                        LexError::UnterminatedString => ErrorKind::UnterminatedString,
                    };
                    return Err(CompileError::new(kind, rue_span));
                }
            }
        }

        // Add EOF token (logos doesn't emit EOF)
        let eof_pos = self.source.len() as u32;
        tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::point(eof_pos),
        });

        Ok(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logos_basic_tokens() {
        let mut lexer = LogosLexer::new("fn main() -> i32 { 42 }");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(ref s) if s == "main"));
        assert!(matches!(tokens[2].kind, TokenKind::LParen));
        assert!(matches!(tokens[3].kind, TokenKind::RParen));
        assert!(matches!(tokens[4].kind, TokenKind::Arrow));
        assert!(matches!(tokens[5].kind, TokenKind::I32));
        assert!(matches!(tokens[6].kind, TokenKind::LBrace));
        assert!(matches!(tokens[7].kind, TokenKind::Int(42)));
        assert!(matches!(tokens[8].kind, TokenKind::RBrace));
        assert!(matches!(tokens[9].kind, TokenKind::Eof));
    }

    #[test]
    fn test_logos_unexpected_character() {
        let mut lexer = LogosLexer::new("fn main() { $ }");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UnexpectedCharacter('$')));
    }

    #[test]
    fn test_logos_at_token() {
        let mut lexer = LogosLexer::new("@dbg");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::At));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(ref s) if s == "dbg"));
    }

    #[test]
    fn test_logos_spans() {
        let mut lexer = LogosLexer::new("fn main");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].span, Span::new(0, 2)); // "fn"
        assert_eq!(tokens[1].span, Span::new(3, 7)); // "main"
    }

    #[test]
    fn test_logos_arithmetic_operators() {
        let mut lexer = LogosLexer::new("1 + 2 - 3 * 4 / 5 % 6");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Int(1)));
        assert!(matches!(tokens[1].kind, TokenKind::Plus));
        assert!(matches!(tokens[2].kind, TokenKind::Int(2)));
        assert!(matches!(tokens[3].kind, TokenKind::Minus));
        assert!(matches!(tokens[4].kind, TokenKind::Int(3)));
        assert!(matches!(tokens[5].kind, TokenKind::Star));
        assert!(matches!(tokens[6].kind, TokenKind::Int(4)));
        assert!(matches!(tokens[7].kind, TokenKind::Slash));
        assert!(matches!(tokens[8].kind, TokenKind::Int(5)));
        assert!(matches!(tokens[9].kind, TokenKind::Percent));
        assert!(matches!(tokens[10].kind, TokenKind::Int(6)));
    }

    #[test]
    fn test_logos_minus_vs_arrow() {
        // Minus alone
        let mut lexer = LogosLexer::new("a - b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Minus));

        // Arrow
        let mut lexer = LogosLexer::new("-> i32");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Arrow));

        // Minus followed by non-arrow
        let mut lexer = LogosLexer::new("-1");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Minus));
        assert!(matches!(tokens[1].kind, TokenKind::Int(1)));
    }

    #[test]
    fn test_logos_let_binding() {
        let mut lexer = LogosLexer::new("let x = 42;");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Let));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(ref s) if s == "x"));
        assert!(matches!(tokens[2].kind, TokenKind::Eq));
        assert!(matches!(tokens[3].kind, TokenKind::Int(42)));
        assert!(matches!(tokens[4].kind, TokenKind::Semi));
    }

    #[test]
    fn test_logos_logical_operators() {
        let mut lexer = LogosLexer::new("!true && false || true");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Bang));
        assert!(matches!(tokens[1].kind, TokenKind::True));
        assert!(matches!(tokens[2].kind, TokenKind::AmpAmp));
        assert!(matches!(tokens[3].kind, TokenKind::False));
        assert!(matches!(tokens[4].kind, TokenKind::PipePipe));
        assert!(matches!(tokens[5].kind, TokenKind::True));
    }

    #[test]
    fn test_logos_comparison_operators() {
        let mut lexer = LogosLexer::new("a == b != c < d > e <= f >= g");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[1].kind, TokenKind::EqEq));
        assert!(matches!(tokens[3].kind, TokenKind::BangEq));
        assert!(matches!(tokens[5].kind, TokenKind::Lt));
        assert!(matches!(tokens[7].kind, TokenKind::Gt));
        assert!(matches!(tokens[9].kind, TokenKind::LtEq));
        assert!(matches!(tokens[11].kind, TokenKind::GtEq));
    }

    #[test]
    fn test_logos_line_comments() {
        let mut lexer = LogosLexer::new("fn // comment\nmain");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(ref s) if s == "main"));
        assert!(matches!(tokens[2].kind, TokenKind::Eof));
    }

    #[test]
    fn test_logos_keywords_vs_identifiers() {
        // Keywords should be recognized
        let mut lexer = LogosLexer::new("fn let mut if else while break continue true false");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert!(matches!(tokens[1].kind, TokenKind::Let));
        assert!(matches!(tokens[2].kind, TokenKind::Mut));
        assert!(matches!(tokens[3].kind, TokenKind::If));
        assert!(matches!(tokens[4].kind, TokenKind::Else));
        assert!(matches!(tokens[5].kind, TokenKind::While));
        assert!(matches!(tokens[6].kind, TokenKind::Break));
        assert!(matches!(tokens[7].kind, TokenKind::Continue));
        assert!(matches!(tokens[8].kind, TokenKind::True));
        assert!(matches!(tokens[9].kind, TokenKind::False));

        // Identifiers that start with keywords should be identifiers
        let mut lexer = LogosLexer::new("fns lets mutable iff elseif whileloop");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Ident(ref s) if s == "fns"));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(ref s) if s == "lets"));
        assert!(matches!(tokens[2].kind, TokenKind::Ident(ref s) if s == "mutable"));
        assert!(matches!(tokens[3].kind, TokenKind::Ident(ref s) if s == "iff"));
        assert!(matches!(tokens[4].kind, TokenKind::Ident(ref s) if s == "elseif"));
        assert!(matches!(tokens[5].kind, TokenKind::Ident(ref s) if s == "whileloop"));
    }

    #[test]
    fn test_logos_bitwise_operators() {
        let mut lexer = LogosLexer::new("a & b | c ^ d ~ e << f >> g");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Ident(ref s) if s == "a"));
        assert!(matches!(tokens[1].kind, TokenKind::Amp));
        assert!(matches!(tokens[2].kind, TokenKind::Ident(ref s) if s == "b"));
        assert!(matches!(tokens[3].kind, TokenKind::Pipe));
        assert!(matches!(tokens[4].kind, TokenKind::Ident(ref s) if s == "c"));
        assert!(matches!(tokens[5].kind, TokenKind::Caret));
        assert!(matches!(tokens[6].kind, TokenKind::Ident(ref s) if s == "d"));
        assert!(matches!(tokens[7].kind, TokenKind::Tilde));
        assert!(matches!(tokens[8].kind, TokenKind::Ident(ref s) if s == "e"));
        assert!(matches!(tokens[9].kind, TokenKind::LtLt));
        assert!(matches!(tokens[10].kind, TokenKind::Ident(ref s) if s == "f"));
        assert!(matches!(tokens[11].kind, TokenKind::GtGt));
        assert!(matches!(tokens[12].kind, TokenKind::Ident(ref s) if s == "g"));
    }

    #[test]
    fn test_logos_bitwise_vs_logical() {
        // Single & should be bitwise AND
        let mut lexer = LogosLexer::new("a & b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Amp));

        // Double && should be logical AND
        let mut lexer = LogosLexer::new("a && b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::AmpAmp));

        // Single | should be bitwise OR
        let mut lexer = LogosLexer::new("a | b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Pipe));

        // Double || should be logical OR
        let mut lexer = LogosLexer::new("a || b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::PipePipe));
    }

    #[test]
    fn test_logos_shift_vs_comparison() {
        // << should be left shift
        let mut lexer = LogosLexer::new("a << b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::LtLt));

        // >> should be right shift
        let mut lexer = LogosLexer::new("a >> b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::GtGt));

        // < should be less than
        let mut lexer = LogosLexer::new("a < b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Lt));

        // > should be greater than
        let mut lexer = LogosLexer::new("a > b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Gt));

        // <= should be less than or equal
        let mut lexer = LogosLexer::new("a <= b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::LtEq));

        // >= should be greater than or equal
        let mut lexer = LogosLexer::new("a >= b");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::GtEq));
    }

    #[test]
    fn test_logos_type_keywords() {
        // Type names should be recognized as keywords, not identifiers
        let mut lexer = LogosLexer::new("i8 i16 i32 i64 u8 u16 u32 u64 bool");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::I8));
        assert!(matches!(tokens[1].kind, TokenKind::I16));
        assert!(matches!(tokens[2].kind, TokenKind::I32));
        assert!(matches!(tokens[3].kind, TokenKind::I64));
        assert!(matches!(tokens[4].kind, TokenKind::U8));
        assert!(matches!(tokens[5].kind, TokenKind::U16));
        assert!(matches!(tokens[6].kind, TokenKind::U32));
        assert!(matches!(tokens[7].kind, TokenKind::U64));
        assert!(matches!(tokens[8].kind, TokenKind::Bool));

        // Identifiers that start with type names should be identifiers
        let mut lexer = LogosLexer::new("i32x i64ptr boolish u8_data");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Ident(ref s) if s == "i32x"));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(ref s) if s == "i64ptr"));
        assert!(matches!(tokens[2].kind, TokenKind::Ident(ref s) if s == "boolish"));
        assert!(matches!(tokens[3].kind, TokenKind::Ident(ref s) if s == "u8_data"));
    }
}
