//! Logos-based lexer for the Rue programming language.
//!
//! This module provides a lexer implementation using the logos derive macro
//! for efficient tokenization.

use lasso::{Spur, ThreadedRodeo};
use logos::Logos;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::{FileId, Span};

/// Error type for lexing failures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LexError {
    #[default]
    UnexpectedCharacter,
    InvalidInteger,
    InvalidStringEscape,
    UnterminatedString,
}

/// Process a string literal starting from an opening quote.
/// This manually scans for the string content and closing quote,
/// enabling detection of unterminated strings.
fn process_string_from_quote(lex: &mut logos::Lexer<'_, LogosTokenKind>) -> Result<Spur, LexError> {
    // At this point we've matched just the opening quote "
    // We need to scan remainder for string content and closing quote
    let remainder = lex.remainder();
    let mut chars = remainder.chars();
    let mut consumed = 0;
    let mut result = String::new();
    let mut found_close = false;

    while let Some(c) = chars.next() {
        if c == '"' {
            // Found closing quote
            consumed += 1;
            found_close = true;
            break;
        } else if c == '\\' {
            // Escape sequence
            consumed += c.len_utf8();
            match chars.next() {
                Some('\\') => {
                    consumed += 1;
                    result.push('\\');
                }
                Some('"') => {
                    consumed += 1;
                    result.push('"');
                }
                Some('n') => {
                    consumed += 1;
                    result.push('\n');
                }
                Some('t') => {
                    consumed += 1;
                    result.push('\t');
                }
                Some('r') => {
                    consumed += 1;
                    result.push('\r');
                }
                Some('0') => {
                    consumed += 1;
                    result.push('\0');
                }
                Some(other) => {
                    // Invalid escape - consume the char to get better error position
                    consumed += other.len_utf8();
                    lex.bump(consumed);
                    return Err(LexError::InvalidStringEscape);
                }
                None => {
                    // Backslash at end of input
                    lex.bump(consumed);
                    return Err(LexError::UnterminatedString);
                }
            }
        } else if c == '\n' {
            // Newline in string - string is unterminated at this line
            // Don't consume the newline so error span points to string start
            lex.bump(consumed);
            return Err(LexError::UnterminatedString);
        } else {
            consumed += c.len_utf8();
            result.push(c);
        }
    }

    if !found_close {
        // Reached end of input without closing quote
        lex.bump(consumed);
        return Err(LexError::UnterminatedString);
    }

    // Advance past the string content and closing quote
    lex.bump(consumed);

    // Intern the string
    let spur = lex.extras.get_or_intern(&result);
    Ok(spur)
}

/// Token kinds in the Rue language, using logos derive macro.
#[derive(Logos, Debug, Clone, PartialEq, Eq)]
#[logos(error = LexError)]
#[logos(extras = ThreadedRodeo)]
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
    #[token("linear")]
    Linear,
    #[token("self")]
    SelfValue,
    #[token("comptime")]
    Comptime,
    #[token("pub")]
    Pub,
    #[token("const")]
    Const,

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
    #[regex(r"[0-9]+", |lex| lex.slice().parse::<u64>().map_err(|_| LexError::InvalidInteger))]
    Int(u64),

    // String literals - match opening quote and process content manually
    // This allows detection of unterminated strings
    #[token("\"", process_string_from_quote)]
    String(Spur),

    // Identifiers (lower priority than keywords)
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*", |lex| lex.extras.get_or_intern(lex.slice()), priority = 1)]
    Ident(Spur),

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

    // Builtins - use callback to ensure @import is not followed by identifier chars
    // This prevents @importx from being tokenized as @import + x
    #[token("@import", at_import_callback)]
    AtImport,
}

/// Callback for @import token to ensure word boundary.
/// Returns Some(()) if @import is NOT followed by identifier chars, None otherwise.
fn at_import_callback(lex: &mut logos::Lexer<'_, LogosTokenKind>) -> Option<()> {
    match lex.remainder().chars().next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => None,
        _ => Some(()),
    }
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
            LogosTokenKind::Linear => TokenKind::Linear,
            LogosTokenKind::SelfValue => TokenKind::SelfValue,
            LogosTokenKind::Comptime => TokenKind::Comptime,
            LogosTokenKind::Pub => TokenKind::Pub,
            LogosTokenKind::Const => TokenKind::Const,
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
            // AtImport is handled specially in tokenize() to provide the interned "import" Spur
            LogosTokenKind::AtImport => unreachable!("AtImport should be handled specially"),
        }
    }
}

/// Logos-based lexer that converts source text into tokens.
pub struct LogosLexer<'a> {
    source: &'a str,
    interner: ThreadedRodeo,
    file_id: FileId,
}

impl<'a> LogosLexer<'a> {
    /// Create a new lexer for the given source text with a fresh interner.
    ///
    /// Uses the default file ID. For multi-file compilation, use `with_file_id`.
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            interner: ThreadedRodeo::default(),
            file_id: FileId::DEFAULT,
        }
    }

    /// Create a new lexer with an existing interner.
    pub fn with_interner(source: &'a str, interner: ThreadedRodeo) -> Self {
        Self {
            source,
            interner,
            file_id: FileId::DEFAULT,
        }
    }

    /// Create a new lexer with a specific file ID.
    pub fn with_file_id(source: &'a str, file_id: FileId) -> Self {
        Self {
            source,
            interner: ThreadedRodeo::default(),
            file_id,
        }
    }

    /// Create a new lexer with both an existing interner and a specific file ID.
    pub fn with_interner_and_file_id(
        source: &'a str,
        interner: ThreadedRodeo,
        file_id: FileId,
    ) -> Self {
        Self {
            source,
            interner,
            file_id,
        }
    }

    /// Tokenize the entire source, returning all tokens and the interner.
    pub fn tokenize(self) -> CompileResult<(Vec<Token>, ThreadedRodeo)> {
        // Estimate capacity: source length / 4 is a rough heuristic for token density
        let mut tokens = Vec::with_capacity(self.source.len() / 4);

        let mut lexer = LogosTokenKind::lexer_with_extras(self.source, self.interner);

        loop {
            match lexer.next() {
                Some(result) => {
                    let span = lexer.span();
                    match result {
                        Ok(logos_kind) => {
                            // Convert LogosTokenKind to TokenKind, handling @import specially
                            // because it needs to carry the interned "import" symbol
                            let token_kind = if matches!(logos_kind, LogosTokenKind::AtImport) {
                                let import_spur = lexer.extras.get_or_intern("import");
                                TokenKind::AtImport(import_spur)
                            } else {
                                logos_kind.into()
                            };
                            tokens.push(Token {
                                kind: token_kind,
                                span: Span::with_file(
                                    self.file_id,
                                    span.start as u32,
                                    span.end as u32,
                                ),
                            });
                        }
                        Err(lex_error) => {
                            let rue_span =
                                Span::with_file(self.file_id, span.start as u32, span.end as u32);
                            let slice = lexer.slice();
                            let error_char = slice.chars().next().unwrap_or('?');
                            let kind = match lex_error {
                                LexError::InvalidInteger => ErrorKind::InvalidInteger,
                                LexError::UnexpectedCharacter => {
                                    ErrorKind::UnexpectedCharacter(error_char)
                                }
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
                None => break,
            }
        }

        // Add EOF token (logos doesn't emit EOF)
        let eof_pos = self.source.len() as u32;
        tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::point_in_file(self.file_id, eof_pos),
        });

        // Extract the interner from the logos lexer
        let interner = lexer.extras;

        Ok((tokens, interner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to get the string for a symbol from the interner.
    fn get_ident_str<'a>(kind: &TokenKind, interner: &'a ThreadedRodeo) -> Option<&'a str> {
        match kind {
            TokenKind::Ident(sym) => Some(interner.resolve(sym)),
            _ => None,
        }
    }

    /// Helper to get the string for a string literal symbol.
    fn get_string_str<'a>(kind: &TokenKind, interner: &'a ThreadedRodeo) -> Option<&'a str> {
        match kind {
            TokenKind::String(sym) => Some(interner.resolve(sym)),
            _ => None,
        }
    }

    #[test]
    fn test_logos_basic_tokens() {
        let lexer = LogosLexer::new("fn main() -> i32 { 42 }");
        let (tokens, interner) = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("main"));
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
        let lexer = LogosLexer::new("fn main() { $ }");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UnexpectedCharacter('$')));
    }

    #[test]
    fn test_logos_at_token() {
        let lexer = LogosLexer::new("@dbg");
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::At));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("dbg"));
    }

    #[test]
    fn test_logos_at_import_token() {
        // @import should be recognized as a single token with interned "import" Spur
        let lexer = LogosLexer::new("@import");
        let (tokens, interner) = lexer.tokenize().unwrap();
        if let TokenKind::AtImport(spur) = tokens[0].kind {
            assert_eq!(interner.resolve(&spur), "import");
        } else {
            panic!("Expected AtImport token");
        }
        assert!(matches!(tokens[1].kind, TokenKind::Eof));
    }

    #[test]
    fn test_logos_at_import_vs_at_other() {
        // @import as single token vs @other (At + Ident)
        let lexer = LogosLexer::new("@import @other");
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::AtImport(_)));
        assert!(matches!(tokens[1].kind, TokenKind::At));
        assert_eq!(get_ident_str(&tokens[2].kind, &interner), Some("other"));
    }

    #[test]
    fn test_logos_at_import_span() {
        // Verify the span covers the entire @import token
        let lexer = LogosLexer::new("@import");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].span, Span::new(0, 7)); // "@import" is 7 chars
    }

    #[test]
    fn test_logos_at_import_with_parens() {
        // @import("path.rue") pattern
        let lexer = LogosLexer::new(r#"@import("math.rue")"#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::AtImport(_)));
        assert!(matches!(tokens[1].kind, TokenKind::LParen));
        assert_eq!(get_string_str(&tokens[2].kind, &interner), Some("math.rue"));
        assert!(matches!(tokens[3].kind, TokenKind::RParen));
    }

    #[test]
    fn test_logos_at_import_suffix_is_error() {
        // @importx is an invalid token - @import followed by x cannot be a valid construct
        // The lexer produces an error because @import matches but is followed by 'x'
        // which makes it an invalid token sequence
        let lexer = LogosLexer::new("@importx");
        let result = lexer.tokenize();
        // This should error because @importx is neither @import nor @ followed by a space
        assert!(result.is_err());
    }

    #[test]
    fn test_logos_spans() {
        let lexer = LogosLexer::new("fn main");
        let (tokens, _interner) = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].span, Span::new(0, 2)); // "fn"
        assert_eq!(tokens[1].span, Span::new(3, 7)); // "main"
    }

    #[test]
    fn test_logos_arithmetic_operators() {
        let lexer = LogosLexer::new("1 + 2 - 3 * 4 / 5 % 6");
        let (tokens, _interner) = lexer.tokenize().unwrap();

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
        let lexer = LogosLexer::new("a - b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Minus));

        // Arrow
        let lexer = LogosLexer::new("-> i32");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Arrow));

        // Minus followed by non-arrow
        let lexer = LogosLexer::new("-1");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Minus));
        assert!(matches!(tokens[1].kind, TokenKind::Int(1)));
    }

    #[test]
    fn test_logos_let_binding() {
        let lexer = LogosLexer::new("let x = 42;");
        let (tokens, interner) = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Let));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("x"));
        assert!(matches!(tokens[2].kind, TokenKind::Eq));
        assert!(matches!(tokens[3].kind, TokenKind::Int(42)));
        assert!(matches!(tokens[4].kind, TokenKind::Semi));
    }

    #[test]
    fn test_logos_logical_operators() {
        let lexer = LogosLexer::new("!true && false || true");
        let (tokens, _) = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Bang));
        assert!(matches!(tokens[1].kind, TokenKind::True));
        assert!(matches!(tokens[2].kind, TokenKind::AmpAmp));
        assert!(matches!(tokens[3].kind, TokenKind::False));
        assert!(matches!(tokens[4].kind, TokenKind::PipePipe));
        assert!(matches!(tokens[5].kind, TokenKind::True));
    }

    #[test]
    fn test_logos_comparison_operators() {
        let lexer = LogosLexer::new("a == b != c < d > e <= f >= g");
        let (tokens, _) = lexer.tokenize().unwrap();

        assert!(matches!(tokens[1].kind, TokenKind::EqEq));
        assert!(matches!(tokens[3].kind, TokenKind::BangEq));
        assert!(matches!(tokens[5].kind, TokenKind::Lt));
        assert!(matches!(tokens[7].kind, TokenKind::Gt));
        assert!(matches!(tokens[9].kind, TokenKind::LtEq));
        assert!(matches!(tokens[11].kind, TokenKind::GtEq));
    }

    #[test]
    fn test_logos_line_comments() {
        let lexer = LogosLexer::new("fn // comment\nmain");
        let (tokens, interner) = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("main"));
        assert!(matches!(tokens[2].kind, TokenKind::Eof));
    }

    #[test]
    fn test_logos_keywords_vs_identifiers() {
        // Keywords should be recognized
        let lexer = LogosLexer::new("fn let mut if else while break continue true false");
        let (tokens, _) = lexer.tokenize().unwrap();

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
        let lexer = LogosLexer::new("fns lets mutable iff elseif whileloop");
        let (tokens, interner) = lexer.tokenize().unwrap();

        assert_eq!(get_ident_str(&tokens[0].kind, &interner), Some("fns"));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("lets"));
        assert_eq!(get_ident_str(&tokens[2].kind, &interner), Some("mutable"));
        assert_eq!(get_ident_str(&tokens[3].kind, &interner), Some("iff"));
        assert_eq!(get_ident_str(&tokens[4].kind, &interner), Some("elseif"));
        assert_eq!(get_ident_str(&tokens[5].kind, &interner), Some("whileloop"));
    }

    #[test]
    fn test_logos_bitwise_operators() {
        let lexer = LogosLexer::new("a & b | c ^ d ~ e << f >> g");
        let (tokens, interner) = lexer.tokenize().unwrap();

        assert_eq!(get_ident_str(&tokens[0].kind, &interner), Some("a"));
        assert!(matches!(tokens[1].kind, TokenKind::Amp));
        assert_eq!(get_ident_str(&tokens[2].kind, &interner), Some("b"));
        assert!(matches!(tokens[3].kind, TokenKind::Pipe));
        assert_eq!(get_ident_str(&tokens[4].kind, &interner), Some("c"));
        assert!(matches!(tokens[5].kind, TokenKind::Caret));
        assert_eq!(get_ident_str(&tokens[6].kind, &interner), Some("d"));
        assert!(matches!(tokens[7].kind, TokenKind::Tilde));
        assert_eq!(get_ident_str(&tokens[8].kind, &interner), Some("e"));
        assert!(matches!(tokens[9].kind, TokenKind::LtLt));
        assert_eq!(get_ident_str(&tokens[10].kind, &interner), Some("f"));
        assert!(matches!(tokens[11].kind, TokenKind::GtGt));
        assert_eq!(get_ident_str(&tokens[12].kind, &interner), Some("g"));
    }

    #[test]
    fn test_logos_bitwise_vs_logical() {
        // Single & should be bitwise AND
        let lexer = LogosLexer::new("a & b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Amp));

        // Double && should be logical AND
        let lexer = LogosLexer::new("a && b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::AmpAmp));

        // Single | should be bitwise OR
        let lexer = LogosLexer::new("a | b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Pipe));

        // Double || should be logical OR
        let lexer = LogosLexer::new("a || b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::PipePipe));
    }

    #[test]
    fn test_logos_shift_vs_comparison() {
        // << should be left shift
        let lexer = LogosLexer::new("a << b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::LtLt));

        // >> should be right shift
        let lexer = LogosLexer::new("a >> b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::GtGt));

        // < should be less than
        let lexer = LogosLexer::new("a < b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Lt));

        // > should be greater than
        let lexer = LogosLexer::new("a > b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::Gt));

        // <= should be less than or equal
        let lexer = LogosLexer::new("a <= b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::LtEq));

        // >= should be greater than or equal
        let lexer = LogosLexer::new("a >= b");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::GtEq));
    }

    #[test]
    fn test_logos_integer_overflow() {
        // A number too large for u64 should produce InvalidInteger error
        let lexer = LogosLexer::new("99999999999999999999999");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::InvalidInteger));
    }

    #[test]
    fn test_logos_type_keywords() {
        // Type names should be recognized as keywords, not identifiers
        let lexer = LogosLexer::new("i8 i16 i32 i64 u8 u16 u32 u64 bool");
        let (tokens, _) = lexer.tokenize().unwrap();

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
        let lexer = LogosLexer::new("i32x i64ptr boolish u8_data");
        let (tokens, interner) = lexer.tokenize().unwrap();

        assert_eq!(get_ident_str(&tokens[0].kind, &interner), Some("i32x"));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("i64ptr"));
        assert_eq!(get_ident_str(&tokens[2].kind, &interner), Some("boolish"));
        assert_eq!(get_ident_str(&tokens[3].kind, &interner), Some("u8_data"));
    }

    #[test]
    fn test_logos_unterminated_string() {
        // String without closing quote at end of input
        let lexer = LogosLexer::new(r#""hello"#);
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UnterminatedString));

        // String without closing quote followed by newline
        let lexer = LogosLexer::new("\"hello\nworld");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UnterminatedString));

        // Just an opening quote
        let lexer = LogosLexer::new("\"");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UnterminatedString));
    }

    #[test]
    fn test_logos_valid_strings() {
        // Valid complete string
        let lexer = LogosLexer::new(r#""hello""#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(get_string_str(&tokens[0].kind, &interner), Some("hello"));

        // Empty string
        let lexer = LogosLexer::new(r#""""#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(get_string_str(&tokens[0].kind, &interner), Some(""));

        // String with escaped quote
        let lexer = LogosLexer::new(r#""hello\"world""#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(
            get_string_str(&tokens[0].kind, &interner),
            Some("hello\"world")
        );

        // String with escaped backslash
        let lexer = LogosLexer::new(r#""hello\\world""#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(
            get_string_str(&tokens[0].kind, &interner),
            Some("hello\\world")
        );
    }

    #[test]
    fn test_logos_escape_newline() {
        let lexer = LogosLexer::new(r#""line1\nline2""#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(
            get_string_str(&tokens[0].kind, &interner),
            Some("line1\nline2")
        );
    }

    #[test]
    fn test_logos_escape_tab() {
        let lexer = LogosLexer::new(r#""col1\tcol2""#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(
            get_string_str(&tokens[0].kind, &interner),
            Some("col1\tcol2")
        );
    }

    #[test]
    fn test_logos_escape_carriage_return() {
        let lexer = LogosLexer::new(r#""line\r\n""#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(get_string_str(&tokens[0].kind, &interner), Some("line\r\n"));
    }

    #[test]
    fn test_logos_escape_null() {
        let lexer = LogosLexer::new(r#""null\0byte""#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(
            get_string_str(&tokens[0].kind, &interner),
            Some("null\0byte")
        );
    }

    #[test]
    fn test_logos_invalid_escape_q() {
        let lexer = LogosLexer::new(r#""bad\qescape""#);
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::InvalidStringEscape('q')));
    }

    #[test]
    fn test_logos_all_escapes_combined() {
        // Test all escape sequences in one string
        let lexer = LogosLexer::new(r#""\\\"abc\n\t\r\0xyz""#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(
            get_string_str(&tokens[0].kind, &interner),
            Some("\\\"abc\n\t\r\0xyz")
        );
    }

    #[test]
    fn test_interning_deduplicates() {
        // Same identifier appearing multiple times should have same Symbol
        let lexer = LogosLexer::new("x x x");
        let (tokens, _interner) = lexer.tokenize().unwrap();

        let sym0 = match &tokens[0].kind {
            TokenKind::Ident(s) => *s,
            _ => panic!("expected Ident"),
        };
        let sym1 = match &tokens[1].kind {
            TokenKind::Ident(s) => *s,
            _ => panic!("expected Ident"),
        };
        let sym2 = match &tokens[2].kind {
            TokenKind::Ident(s) => *s,
            _ => panic!("expected Ident"),
        };

        assert_eq!(sym0, sym1);
        assert_eq!(sym1, sym2);
    }

    #[test]
    fn test_token_kind_is_copy() {
        // This test ensures TokenKind is Copy by using it in a context that requires Copy
        let lexer = LogosLexer::new("x");
        let (tokens, _) = lexer.tokenize().unwrap();
        let kind = tokens[0].kind; // This would fail if TokenKind weren't Copy
        let _kind2 = kind; // Use both without moving
        let _kind3 = kind;
    }
}
