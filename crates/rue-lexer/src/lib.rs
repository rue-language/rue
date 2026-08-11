//! Lexer for the Rue programming language.
//!
//! Converts source text into a sequence of tokens for parsing.
//! Uses logos for efficient tokenization.

mod logos_lexer;

use lasso::Key;
pub use lasso::Spur;
pub use logos_lexer::LogosLexer as Lexer;
pub use rue_span::FileId;
use rue_span::Span;

/// Maximum number of detailed lexer diagnostics retained for one source file.
/// On the next error, lexing appends one
/// [`rue_error::ErrorKind::LexerDiagnosticsOmitted`] summary and stops scanning
/// that failed file. The compiler still advances to later source files.
pub const LEXER_DIAGNOSTIC_BUDGET: usize = 100;

/// Maximum source byte length representable by Rue's `u32` span offsets.
///
/// Published as the per-file source-size ceiling in spec C.3:1. A longer
/// source is rejected with the resource-limit diagnostic E1401 before any span
/// is formed, per the graceful-failure policy in spec C.1:2.
pub const MAX_SOURCE_BYTES: usize = u32::MAX as usize;

/// Maximum number of distinct strings one compilation can intern.
///
/// Interner handles are `lasso::Spur`, a non-zero `u32`, so the usable keys are
/// `1..=u32::MAX` (spec Appendix C.5:1, C.6:1). Identifiers and string literals
/// are the only unbounded, source-driven producers of new interned strings, and
/// the lexer refuses a file that could carry the shared interner past this
/// ceiling rather than letting the interner abort (spec C.1:2).
pub const MAX_INTERNED_STRINGS: usize = u32::MAX as usize;

/// Token kinds in the Rue language.
///
/// This enum is `Copy` since all variants contain only small, copyable data:
/// - Most variants are unit (no data)
/// - `Int` contains a `u64` (8 bytes)
/// - `Ident` and `String` contain a `Spur` (4 bytes, an interned string handle)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    // Keywords
    Fn,
    Let,
    Mut,
    Inout,
    Borrow,
    If,
    Else,
    Match,
    While,
    Loop,
    For,
    In,
    Break,
    Continue,
    Return,
    Yield,
    True,
    False,
    Struct,
    Enum,
    Impl, // impl (reserved; no impl blocks in Rue — methods live in struct bodies)
    Drop,
    Linear,    // linear struct modifier
    SelfValue, // self (value, not type)
    SelfType,  // Self (type, not value) - used in methods to refer to the struct type
    Comptime,  // comptime (compile-time evaluation)
    Pub,       // pub visibility modifier (module system)
    Const,     // const declaration (module system re-exports)
    Checked,   // checked { } block for unchecked operations
    Unchecked, // unchecked fn modifier
    Ptr,       // ptr const T / ptr mut T pointer types
    Extern,    // extern "C" { } foreign declaration block (ADR-0064 C FFI)

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
    Type, // type (the compile-time type of types, spec 2.4:3)

    // Patterns
    Underscore, // _ (wildcard pattern)

    // Literals
    Int(u64),
    /// A floating-point literal (`1.5`, `1e9`, `1.5e-3`), carried as the
    /// interned *source text* of the literal rather than a decoded `f64`
    /// (ADR-0065 §3, RUE-1068). A float literal is a `comptime_float`: an
    /// arbitrary-precision abstract constant that only becomes `f32` or `f64`
    /// when context demands one. Decoding to `f64` here would round the
    /// constant before its target width is known, so the exact digits travel
    /// to the phase that knows the type. Separators are already stripped, so
    /// the interned text is directly parseable by `str::parse`.
    Float(Spur),
    String(Spur),

    // Identifiers
    Ident(Spur),

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
    Amp,      // &
    Pipe,     // |
    Caret,    // ^
    Tilde,    // ~
    LtLt,     // <<
    GtGt,     // >>

    // Compound assignment (RUE-1043)
    PlusEq,    // +=
    MinusEq,   // -=
    StarEq,    // *=
    SlashEq,   // /=
    PercentEq, // %=
    AmpEq,     // &=
    PipeEq,    // |=
    CaretEq,   // ^=
    LtLtEq,    // <<=
    GtGtEq,    // >>=

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket, // [
    RBracket, // ]
    Arrow,    // ->
    FatArrow, // =>
    // `::` is no longer an operator (RUE-488); retained only so the parser can
    // emit a precise "use `.`" diagnostic for a stray `::`.
    ColonColon, // ::
    Colon,
    Semi,
    Comma,
    Dot,      // .
    At,       // @
    Question, // ?

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
            TokenKind::Inout => "'inout'",
            TokenKind::Borrow => "'borrow'",
            TokenKind::If => "'if'",
            TokenKind::Else => "'else'",
            TokenKind::Match => "'match'",
            TokenKind::While => "'while'",
            TokenKind::Loop => "'loop'",
            TokenKind::For => "'for'",
            TokenKind::In => "'in'",
            TokenKind::Break => "'break'",
            TokenKind::Continue => "'continue'",
            TokenKind::Return => "'return'",
            TokenKind::Yield => "'yield'",
            TokenKind::True => "'true'",
            TokenKind::False => "'false'",
            TokenKind::Struct => "'struct'",
            TokenKind::Enum => "'enum'",
            TokenKind::Impl => "'impl'",
            TokenKind::Drop => "'drop'",
            TokenKind::Linear => "'linear'",
            TokenKind::SelfValue => "'self'",
            TokenKind::SelfType => "'Self'",
            TokenKind::Comptime => "'comptime'",
            TokenKind::Pub => "'pub'",
            TokenKind::Const => "'const'",
            TokenKind::Checked => "'checked'",
            TokenKind::Unchecked => "'unchecked'",
            TokenKind::Ptr => "'ptr'",
            TokenKind::Extern => "'extern'",
            TokenKind::I8 => "type 'i8'",
            TokenKind::I16 => "type 'i16'",
            TokenKind::I32 => "type 'i32'",
            TokenKind::I64 => "type 'i64'",
            TokenKind::U8 => "type 'u8'",
            TokenKind::U16 => "type 'u16'",
            TokenKind::U32 => "type 'u32'",
            TokenKind::U64 => "type 'u64'",
            TokenKind::Bool => "type 'bool'",
            TokenKind::Type => "type 'type'",
            TokenKind::Underscore => "'_'",
            TokenKind::Int(_) => "integer",
            TokenKind::Float(_) => "float",
            TokenKind::String(_) => "string",
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
            TokenKind::Amp => "'&'",
            TokenKind::Pipe => "'|'",
            TokenKind::Caret => "'^'",
            TokenKind::Tilde => "'~'",
            TokenKind::Question => "'?'",
            TokenKind::LtLt => "'<<'",
            TokenKind::GtGt => "'>>'",
            TokenKind::PlusEq => "'+='",
            TokenKind::MinusEq => "'-='",
            TokenKind::StarEq => "'*='",
            TokenKind::SlashEq => "'/='",
            TokenKind::PercentEq => "'%='",
            TokenKind::AmpEq => "'&='",
            TokenKind::PipeEq => "'|='",
            TokenKind::CaretEq => "'^='",
            TokenKind::LtLtEq => "'<<='",
            TokenKind::GtGtEq => "'>>='",
            TokenKind::LParen => "'('",
            TokenKind::RParen => "')'",
            TokenKind::LBrace => "'{'",
            TokenKind::RBrace => "'}'",
            TokenKind::LBracket => "'['",
            TokenKind::RBracket => "']'",
            TokenKind::Arrow => "'->'",
            TokenKind::FatArrow => "'=>'",
            TokenKind::ColonColon => "'::'",
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
            TokenKind::Inout => write!(f, "INOUT"),
            TokenKind::Borrow => write!(f, "BORROW"),
            TokenKind::If => write!(f, "IF"),
            TokenKind::Else => write!(f, "ELSE"),
            TokenKind::Match => write!(f, "MATCH"),
            TokenKind::While => write!(f, "WHILE"),
            TokenKind::Loop => write!(f, "LOOP"),
            TokenKind::For => write!(f, "FOR"),
            TokenKind::In => write!(f, "IN"),
            TokenKind::Break => write!(f, "BREAK"),
            TokenKind::Continue => write!(f, "CONTINUE"),
            TokenKind::Return => write!(f, "RETURN"),
            TokenKind::Yield => write!(f, "YIELD"),
            TokenKind::True => write!(f, "TRUE"),
            TokenKind::False => write!(f, "FALSE"),
            TokenKind::Struct => write!(f, "STRUCT"),
            TokenKind::Enum => write!(f, "ENUM"),
            TokenKind::Impl => write!(f, "IMPL"),
            TokenKind::Drop => write!(f, "DROP"),
            TokenKind::Linear => write!(f, "LINEAR"),
            TokenKind::SelfValue => write!(f, "SELF"),
            TokenKind::SelfType => write!(f, "SELFTYPE"),
            TokenKind::Comptime => write!(f, "COMPTIME"),
            TokenKind::Pub => write!(f, "PUB"),
            TokenKind::Const => write!(f, "CONST"),
            TokenKind::Checked => write!(f, "CHECKED"),
            TokenKind::Unchecked => write!(f, "UNCHECKED"),
            TokenKind::Ptr => write!(f, "PTR"),
            TokenKind::Extern => write!(f, "EXTERN"),
            TokenKind::I8 => write!(f, "TYPE(i8)"),
            TokenKind::I16 => write!(f, "TYPE(i16)"),
            TokenKind::I32 => write!(f, "TYPE(i32)"),
            TokenKind::I64 => write!(f, "TYPE(i64)"),
            TokenKind::U8 => write!(f, "TYPE(u8)"),
            TokenKind::U16 => write!(f, "TYPE(u16)"),
            TokenKind::U32 => write!(f, "TYPE(u32)"),
            TokenKind::U64 => write!(f, "TYPE(u64)"),
            TokenKind::Bool => write!(f, "TYPE(bool)"),
            TokenKind::Type => write!(f, "TYPE(type)"),
            TokenKind::Underscore => write!(f, "UNDERSCORE"),
            TokenKind::Int(v) => write!(f, "INT({})", v),
            TokenKind::Float(s) => write!(f, "FLOAT(sym:{})", s.into_usize()),
            TokenKind::String(s) => write!(f, "STRING(sym:{})", s.into_usize()),
            TokenKind::Ident(s) => write!(f, "IDENT(sym:{})", s.into_usize()),
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
            TokenKind::Amp => write!(f, "AMP"),
            TokenKind::Pipe => write!(f, "PIPE"),
            TokenKind::Caret => write!(f, "CARET"),
            TokenKind::Tilde => write!(f, "TILDE"),
            TokenKind::LtLt => write!(f, "LTLT"),
            TokenKind::GtGt => write!(f, "GTGT"),
            TokenKind::PlusEq => write!(f, "PLUSEQ"),
            TokenKind::MinusEq => write!(f, "MINUSEQ"),
            TokenKind::StarEq => write!(f, "STAREQ"),
            TokenKind::SlashEq => write!(f, "SLASHEQ"),
            TokenKind::PercentEq => write!(f, "PERCENTEQ"),
            TokenKind::AmpEq => write!(f, "AMPEQ"),
            TokenKind::PipeEq => write!(f, "PIPEEQ"),
            TokenKind::CaretEq => write!(f, "CARETEQ"),
            TokenKind::LtLtEq => write!(f, "LTLTEQ"),
            TokenKind::GtGtEq => write!(f, "GTGTEQ"),
            TokenKind::LParen => write!(f, "LPAREN"),
            TokenKind::RParen => write!(f, "RPAREN"),
            TokenKind::LBrace => write!(f, "LBRACE"),
            TokenKind::RBrace => write!(f, "RBRACE"),
            TokenKind::LBracket => write!(f, "LBRACKET"),
            TokenKind::RBracket => write!(f, "RBRACKET"),
            TokenKind::Arrow => write!(f, "ARROW"),
            TokenKind::FatArrow => write!(f, "FATARROW"),
            TokenKind::ColonColon => write!(f, "COLONCOLON"),
            TokenKind::Colon => write!(f, "COLON"),
            TokenKind::Semi => write!(f, "SEMI"),
            TokenKind::Comma => write!(f, "COMMA"),
            TokenKind::Dot => write!(f, "DOT"),
            TokenKind::At => write!(f, "AT"),
            TokenKind::Question => write!(f, "QUESTION"),
            TokenKind::Eof => write!(f, "EOF"),
        }
    }
}
