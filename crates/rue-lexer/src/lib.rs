//! Lexer for the Rue programming language.
//!
//! Converts source text into a sequence of tokens for parsing.
//! Uses logos for efficient tokenization.

mod logos_lexer;

use lasso::Key;
pub use lasso::Spur;
pub use logos_lexer::{LexedFragment, LexedFragments, LexedSource, LogosLexer as Lexer};
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

/// Maximum number of distinct strings in any one compilation-owned symbol
/// domain.
///
/// Interner handles are `lasso::Spur`, a non-zero `u32`, so the usable keys are
/// `1..=u32::MAX` (spec Appendix C.5:1, C.6:1). Identifiers, literals, parser
/// primitives, and compiler-generated spellings ultimately belong to
/// compilation-owned symbol domains. Lexer/parser staging, canonical semantic
/// lowering, and revision-shared AIR destinations are separate domains; each
/// has this per-domain key-space ceiling. Every post-lexer producer must route
/// new spellings through its owning fallible insertion boundary rather than
/// letting the interner abort (spec C.1:2).
pub const MAX_INTERNED_STRINGS: usize = u32::MAX as usize;

/// The one fallible entry point for the compilation-owned symbol interner.
/// Existing spellings remain readable when the boundary is reached; only a
/// new spelling consumes capacity.
pub fn try_intern(
    interner: &lasso::ThreadedRodeo,
    text: impl AsRef<str>,
) -> Result<Spur, lasso::LassoErrorKind> {
    interner
        .try_get_or_intern(text)
        .map_err(|error| error.kind())
}

/// Preserve Lasso's distinction between a published key-space limit and a
/// failed allocation when exposing the compiler diagnostic.
pub fn interner_error_kind(
    kind: lasso::LassoErrorKind,
    message: impl Into<String>,
) -> rue_error::ErrorKind {
    rue_error::interner_error_kind(kind, message)
}

/// Stable diagnostic for exhaustion of the compilation-owned symbol interner.
pub fn interner_exhausted_error() -> rue_error::CompileError {
    rue_error::CompileError::without_span(rue_error::ErrorKind::CompilerResourceLimit(format!(
        "this symbol domain exceeded its maximum of {MAX_INTERNED_STRINGS} distinct interned spellings"
    )))
}

/// Every keyword token, ordered by spelling.
///
/// Keywords are the reserved words of spec 2.4:2 together with the reserved
/// type names of 2.4:3 — the words the token table takes before the identifier
/// rule, so no program can bind one. `self` and `Self` are keywords by that
/// rule; `_` is not, because spec 2.4 lists no wildcard and the token table
/// classifies `Underscore` as a pattern token. A consumer that needs "may not
/// be an identifier" therefore excludes the wildcard on its own.
///
/// This list and `TokenKind::keyword_spelling` are the only places a keyword
/// is written down: [`KEYWORDS`] is computed from them, and the match in
/// `keyword_spelling` is exhaustive, so a new `TokenKind` variant does not
/// compile until it says whether it is a keyword.
const KEYWORD_TOKENS: &[TokenKind] = &[
    TokenKind::SelfType,
    TokenKind::Bool,
    TokenKind::Borrow,
    TokenKind::Break,
    TokenKind::Checked,
    TokenKind::Comptime,
    TokenKind::Const,
    TokenKind::Continue,
    TokenKind::Drop,
    TokenKind::Else,
    TokenKind::Enum,
    TokenKind::Extern,
    TokenKind::False,
    TokenKind::Fn,
    TokenKind::For,
    TokenKind::I16,
    TokenKind::I32,
    TokenKind::I64,
    TokenKind::I8,
    TokenKind::If,
    TokenKind::Impl,
    TokenKind::In,
    TokenKind::Inout,
    TokenKind::Let,
    TokenKind::Linear,
    TokenKind::Loop,
    TokenKind::Match,
    TokenKind::Mut,
    TokenKind::Ptr,
    TokenKind::Pub,
    TokenKind::Return,
    TokenKind::SelfValue,
    TokenKind::Struct,
    TokenKind::True,
    TokenKind::Type,
    TokenKind::U16,
    TokenKind::U32,
    TokenKind::U64,
    TokenKind::U8,
    TokenKind::Unchecked,
    TokenKind::While,
    TokenKind::Yield,
];

const KEYWORD_SPELLINGS: [&str; KEYWORD_TOKENS.len()] = {
    let mut spellings = [""; KEYWORD_TOKENS.len()];
    let mut index = 0;
    while index < KEYWORD_TOKENS.len() {
        spellings[index] = match KEYWORD_TOKENS[index].keyword_spelling() {
            Some(spelling) => spelling,
            None => panic!("KEYWORD_TOKENS lists a token that is not a keyword"),
        };
        index += 1;
    }
    spellings
};

/// Every keyword spelling, ordered by spelling.
///
/// Consumers outside the lexer — the fuzz identifier generator, the
/// specification keyword tables, the website syntax definition — read this
/// slice or are tested against it rather than repeating the list.
pub const KEYWORDS: &[&str] = &KEYWORD_SPELLINGS;

/// Whether `word` is a keyword and so cannot be used as an identifier.
///
/// The wildcard `_` is not a keyword, though it is equally unavailable as an
/// identifier; a caller that generates identifiers must reject it separately.
pub fn is_keyword(word: &str) -> bool {
    KEYWORDS.contains(&word)
}

/// Every floating-point type name a program may write, ordered by spelling.
///
/// These are deliberately absent from [`KEYWORDS`]: `f32` and `f64` are
/// ordinary identifiers naming builtin types (spec 3.12:2), so they do not
/// steal value-position names, and the token table classifies them as
/// `Ident`. `comptime_float` is absent for a different reason — it is the
/// inferred type of a float literal and no program may name it (spec 3.12:3),
/// exactly as `comptime_int` is unnameable.
///
/// This is the one list; the parser positions that must recognize a float type
/// name lexically read it rather than repeating the spellings, and rue-air's
/// `Type::from_primitive_name` is tested against it (RUE-1989).
pub const FLOAT_TYPE_NAMES: &[&str] = &["f32", "f64"];

/// Whether `word` is one of the [`FLOAT_TYPE_NAMES`].
pub fn is_float_type_name(word: &str) -> bool {
    FLOAT_TYPE_NAMES.contains(&word)
}

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
    /// The source spelling of this token when it is a keyword.
    ///
    /// The match is exhaustive by design: a new `TokenKind` variant does not
    /// compile until it says whether it is a keyword, which is what keeps
    /// [`KEYWORDS`] complete.
    pub const fn keyword_spelling(self) -> Option<&'static str> {
        Some(match self {
            TokenKind::Fn => "fn",
            TokenKind::Let => "let",
            TokenKind::Mut => "mut",
            TokenKind::Inout => "inout",
            TokenKind::Borrow => "borrow",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::Match => "match",
            TokenKind::While => "while",
            TokenKind::Loop => "loop",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Return => "return",
            TokenKind::Yield => "yield",
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Struct => "struct",
            TokenKind::Enum => "enum",
            TokenKind::Impl => "impl",
            TokenKind::Drop => "drop",
            TokenKind::Linear => "linear",
            TokenKind::SelfValue => "self",
            TokenKind::SelfType => "Self",
            TokenKind::Comptime => "comptime",
            TokenKind::Pub => "pub",
            TokenKind::Const => "const",
            TokenKind::Checked => "checked",
            TokenKind::Unchecked => "unchecked",
            TokenKind::Ptr => "ptr",
            TokenKind::Extern => "extern",
            TokenKind::I8 => "i8",
            TokenKind::I16 => "i16",
            TokenKind::I32 => "i32",
            TokenKind::I64 => "i64",
            TokenKind::U8 => "u8",
            TokenKind::U16 => "u16",
            TokenKind::U32 => "u32",
            TokenKind::U64 => "u64",
            TokenKind::Bool => "bool",
            TokenKind::Type => "type",
            // Not keywords: the wildcard pattern, the literal and identifier
            // classes, every operator and punctuator, and end of file.
            TokenKind::Underscore
            | TokenKind::Int(_)
            | TokenKind::Float(_)
            | TokenKind::String(_)
            | TokenKind::Ident(_)
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::Eq
            | TokenKind::EqEq
            | TokenKind::Bang
            | TokenKind::BangEq
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::LtEq
            | TokenKind::GtEq
            | TokenKind::AmpAmp
            | TokenKind::PipePipe
            | TokenKind::Amp
            | TokenKind::Pipe
            | TokenKind::Caret
            | TokenKind::Tilde
            | TokenKind::LtLt
            | TokenKind::GtGt
            | TokenKind::PlusEq
            | TokenKind::MinusEq
            | TokenKind::StarEq
            | TokenKind::SlashEq
            | TokenKind::PercentEq
            | TokenKind::AmpEq
            | TokenKind::PipeEq
            | TokenKind::CaretEq
            | TokenKind::LtLtEq
            | TokenKind::GtGtEq
            | TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::LBrace
            | TokenKind::RBrace
            | TokenKind::LBracket
            | TokenKind::RBracket
            | TokenKind::Arrow
            | TokenKind::FatArrow
            | TokenKind::ColonColon
            | TokenKind::Colon
            | TokenKind::Semi
            | TokenKind::Comma
            | TokenKind::Dot
            | TokenKind::At
            | TokenKind::Question
            | TokenKind::Eof => return None,
        })
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_type_names_are_sorted_unique_identifiers() {
        let mut sorted = FLOAT_TYPE_NAMES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, FLOAT_TYPE_NAMES.to_vec());

        for spelling in FLOAT_TYPE_NAMES {
            assert!(
                !is_keyword(spelling),
                "{spelling} names a builtin type without reserving the identifier"
            );
            assert!(is_float_type_name(spelling));
            let (tokens, _) = Lexer::new(spelling)
                .tokenize()
                .unwrap_or_else(|error| panic!("float type {spelling} failed to lex: {error}"));
            assert!(matches!(tokens[0].kind, TokenKind::Ident(_)));
        }

        // Unnameable comptime-only types are not float type names (spec 3.12:3).
        assert!(!is_float_type_name("comptime_float"));
    }

    #[test]
    fn keywords_are_sorted_and_unique() {
        let mut sorted = KEYWORDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, KEYWORDS.to_vec());
    }

    #[test]
    fn every_keyword_spelling_lexes_to_its_own_token() {
        for spelling in KEYWORDS {
            let (tokens, _) = Lexer::new(spelling)
                .tokenize()
                .unwrap_or_else(|error| panic!("keyword {spelling} failed to lex: {error}"));
            assert_eq!(
                tokens.len(),
                2,
                "keyword {spelling} lexed to {tokens:?} instead of one token and EOF"
            );
            assert_eq!(tokens[0].kind.keyword_spelling(), Some(*spelling));
            assert_eq!(tokens[1].kind, TokenKind::Eof);
        }
    }

    #[test]
    fn the_wildcard_is_a_pattern_token_not_a_keyword() {
        // Spec 2.4 lists no wildcard, so `_` is absent from KEYWORDS even
        // though it is equally unavailable as an identifier.
        let (tokens, _) = Lexer::new("_").tokenize().expect("`_` lexes");
        assert_eq!(tokens[0].kind, TokenKind::Underscore);
        assert_eq!(TokenKind::Underscore.keyword_spelling(), None);
        assert!(!is_keyword("_"));
    }

    #[test]
    fn non_keyword_tokens_have_no_keyword_spelling() {
        for kind in [
            TokenKind::Int(7),
            TokenKind::Plus,
            TokenKind::LBrace,
            TokenKind::Arrow,
            TokenKind::Eof,
        ] {
            assert_eq!(kind.keyword_spelling(), None, "{kind} is not a keyword");
        }
    }

    #[test]
    fn is_keyword_answers_for_reserved_words_only() {
        assert!(is_keyword("fn"));
        assert!(is_keyword("Self"));
        assert!(is_keyword("type"));
        assert!(!is_keyword("main"));
        assert!(!is_keyword("Fn"));
        assert!(!is_keyword(""));
    }
}
