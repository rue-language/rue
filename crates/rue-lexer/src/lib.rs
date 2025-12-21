//! Lexer for the Rue programming language.
//!
//! Converts source text into a sequence of tokens for parsing.
//! Uses logos for efficient tokenization.

mod logos_lexer;

pub use logos_lexer::LogosLexer as Lexer;
use rue_span::Span;

/// Token kinds in the Rue language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Keywords
    Fn,
    Let,
    Mut,
    If,
    Else,
    Match,
    While,
    Loop,
    Break,
    Continue,
    Return,
    True,
    False,
    Struct,

    // Type keywords
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,

    // Patterns
    Underscore, // _ (wildcard pattern)

    // Literals
    Int(u64),

    // Identifiers
    Ident(String),

    // Operators
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Percent,  // %
    Eq,       // =
    EqEq,     // ==
    Bang,     // !
    BangEq,   // !=
    Lt,       // <
    Gt,       // >
    LtEq,     // <=
    GtEq,     // >=
    AmpAmp,   // &&
    PipePipe, // ||

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket, // [
    RBracket, // ]
    Arrow,    // ->
    FatArrow, // =>
    Colon,
    Semi,
    Comma,
    Dot, // .
    At,  // @

    // Special
    Eof,
}

impl TokenKind {
    /// Get a human-readable name for this token kind.
    pub fn name(&self) -> &'static str {
        match self {
            TokenKind::Fn => "'fn'",
            TokenKind::Let => "'let'",
            TokenKind::Mut => "'mut'",
            TokenKind::If => "'if'",
            TokenKind::Else => "'else'",
            TokenKind::Match => "'match'",
            TokenKind::While => "'while'",
            TokenKind::Loop => "'loop'",
            TokenKind::Break => "'break'",
            TokenKind::Continue => "'continue'",
            TokenKind::Return => "'return'",
            TokenKind::True => "'true'",
            TokenKind::False => "'false'",
            TokenKind::Struct => "'struct'",
            TokenKind::I8 => "type 'i8'",
            TokenKind::I16 => "type 'i16'",
            TokenKind::I32 => "type 'i32'",
            TokenKind::I64 => "type 'i64'",
            TokenKind::U8 => "type 'u8'",
            TokenKind::U16 => "type 'u16'",
            TokenKind::U32 => "type 'u32'",
            TokenKind::U64 => "type 'u64'",
            TokenKind::Bool => "type 'bool'",
            TokenKind::Underscore => "'_'",
            TokenKind::Int(_) => "integer",
            TokenKind::Ident(_) => "identifier",
            TokenKind::Plus => "'+'",
            TokenKind::Minus => "'-'",
            TokenKind::Star => "'*'",
            TokenKind::Slash => "'/'",
            TokenKind::Percent => "'%'",
            TokenKind::Eq => "'='",
            TokenKind::EqEq => "'=='",
            TokenKind::Bang => "'!'",
            TokenKind::BangEq => "'!='",
            TokenKind::Lt => "'<'",
            TokenKind::Gt => "'>'",
            TokenKind::LtEq => "'<='",
            TokenKind::GtEq => "'>='",
            TokenKind::AmpAmp => "'&&'",
            TokenKind::PipePipe => "'||'",
            TokenKind::LParen => "'('",
            TokenKind::RParen => "')'",
            TokenKind::LBrace => "'{'",
            TokenKind::RBrace => "'}'",
            TokenKind::LBracket => "'['",
            TokenKind::RBracket => "']'",
            TokenKind::Arrow => "'->'",
            TokenKind::FatArrow => "'=>'",
            TokenKind::Colon => "':'",
            TokenKind::Semi => "';'",
            TokenKind::Comma => "','",
            TokenKind::Dot => "'.'",
            TokenKind::At => "'@'",
            TokenKind::Eof => "end of file",
        }
    }
}

/// A token with its kind and source span.
#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:>4}..{:<4} {}",
            self.span.start, self.span.end, self.kind
        )
    }
}

impl std::fmt::Display for TokenKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenKind::Fn => write!(f, "FN"),
            TokenKind::Let => write!(f, "LET"),
            TokenKind::Mut => write!(f, "MUT"),
            TokenKind::If => write!(f, "IF"),
            TokenKind::Else => write!(f, "ELSE"),
            TokenKind::Match => write!(f, "MATCH"),
            TokenKind::While => write!(f, "WHILE"),
            TokenKind::Loop => write!(f, "LOOP"),
            TokenKind::Break => write!(f, "BREAK"),
            TokenKind::Continue => write!(f, "CONTINUE"),
            TokenKind::Return => write!(f, "RETURN"),
            TokenKind::True => write!(f, "TRUE"),
            TokenKind::False => write!(f, "FALSE"),
            TokenKind::Struct => write!(f, "STRUCT"),
            TokenKind::I8 => write!(f, "TYPE(i8)"),
            TokenKind::I16 => write!(f, "TYPE(i16)"),
            TokenKind::I32 => write!(f, "TYPE(i32)"),
            TokenKind::I64 => write!(f, "TYPE(i64)"),
            TokenKind::U8 => write!(f, "TYPE(u8)"),
            TokenKind::U16 => write!(f, "TYPE(u16)"),
            TokenKind::U32 => write!(f, "TYPE(u32)"),
            TokenKind::U64 => write!(f, "TYPE(u64)"),
            TokenKind::Bool => write!(f, "TYPE(bool)"),
            TokenKind::Underscore => write!(f, "UNDERSCORE"),
            TokenKind::Int(v) => write!(f, "INT({})", v),
            TokenKind::Ident(s) => write!(f, "IDENT({})", s),
            TokenKind::Plus => write!(f, "PLUS"),
            TokenKind::Minus => write!(f, "MINUS"),
            TokenKind::Star => write!(f, "STAR"),
            TokenKind::Slash => write!(f, "SLASH"),
            TokenKind::Percent => write!(f, "PERCENT"),
            TokenKind::Eq => write!(f, "EQ"),
            TokenKind::EqEq => write!(f, "EQEQ"),
            TokenKind::Bang => write!(f, "BANG"),
            TokenKind::BangEq => write!(f, "BANGEQ"),
            TokenKind::Lt => write!(f, "LT"),
            TokenKind::Gt => write!(f, "GT"),
            TokenKind::LtEq => write!(f, "LTEQ"),
            TokenKind::GtEq => write!(f, "GTEQ"),
            TokenKind::AmpAmp => write!(f, "AMPAMP"),
            TokenKind::PipePipe => write!(f, "PIPEPIPE"),
            TokenKind::LParen => write!(f, "LPAREN"),
            TokenKind::RParen => write!(f, "RPAREN"),
            TokenKind::LBrace => write!(f, "LBRACE"),
            TokenKind::RBrace => write!(f, "RBRACE"),
            TokenKind::LBracket => write!(f, "LBRACKET"),
            TokenKind::RBracket => write!(f, "RBRACKET"),
            TokenKind::Arrow => write!(f, "ARROW"),
            TokenKind::FatArrow => write!(f, "FATARROW"),
            TokenKind::Colon => write!(f, "COLON"),
            TokenKind::Semi => write!(f, "SEMI"),
            TokenKind::Comma => write!(f, "COMMA"),
            TokenKind::Dot => write!(f, "DOT"),
            TokenKind::At => write!(f, "AT"),
            TokenKind::LBracket => write!(f, "LBRACKET"),
            TokenKind::RBracket => write!(f, "RBRACKET"),
            TokenKind::Eof => write!(f, "EOF"),
        }
    }
}
