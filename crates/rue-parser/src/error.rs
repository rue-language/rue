//! Parser error handling with rich diagnostics

use rue_diagnostic::{
    Diagnostic, DiagnosticBuilder, DiagnosticCode, DiagnosticCollector, E1001, E1002, E1003,
    SourceId, SuggestionEngine,
};
use rue_lexer::{Span, TokenKind};
use std::fmt;

/// Parser error with diagnostic information
#[derive(Debug, Clone)]
pub struct ParseError {
    pub diagnostic: Diagnostic,
}

impl ParseError {
    /// Create a new parse error with a diagnostic
    pub fn new(diagnostic: Diagnostic) -> Self {
        Self { diagnostic }
    }

    /// Create an "unexpected token" error
    pub fn unexpected_token(
        found: &TokenKind,
        expected: &[TokenKind],
        span: Span,
        source_id: SourceId,
        context: Option<&str>,
    ) -> Self {
        let expected_str = if expected.len() == 1 {
            format!("`{}`", token_display(&expected[0]))
        } else {
            let tokens: Vec<_> = expected.iter().map(token_display).collect();
            if tokens.len() == 2 {
                format!("`{}` or `{}`", tokens[0], tokens[1])
            } else {
                let (last, rest) = tokens.split_last().unwrap();
                format!(
                    "one of {}, or `{}`",
                    rest.iter()
                        .map(|t| format!("`{}`", t))
                        .collect::<Vec<_>>()
                        .join(", "),
                    last
                )
            }
        };

        let message = if let Some(ctx) = context {
            format!("unexpected `{}` {}", token_display(found), ctx)
        } else {
            format!("unexpected `{}`", token_display(found))
        };

        let mut builder = DiagnosticBuilder::error(message, source_id)
            .with_code(E1001)
            .with_primary_label(span, format!("expected {}", expected_str));

        // Add context-specific help
        if let Some(ctx) = context {
            builder =
                builder.with_help(format!("while parsing {}, expected {}", ctx, expected_str));
        }

        Self::new(builder.build())
    }

    /// Create an "expected identifier" error
    pub fn expected_identifier(
        found: &TokenKind,
        span: Span,
        source_id: SourceId,
        context: &str,
    ) -> Self {
        let diagnostic =
            DiagnosticBuilder::error(format!("expected identifier {}", context), source_id)
                .with_code(E1001)
                .with_primary_label(
                    span,
                    format!("expected identifier, found `{}`", token_display(found)),
                )
                .with_help("identifiers must start with a letter or underscore")
                .build();

        Self::new(diagnostic)
    }

    /// Create an "unexpected end of input" error
    pub fn unexpected_eof(expected: &str, span: Span, source_id: SourceId) -> Self {
        let diagnostic = DiagnosticBuilder::error("unexpected end of input", source_id)
            .with_code(E1001)
            .with_primary_label(span, format!("expected {}", expected))
            .with_help(format!("add {} to complete the expression", expected))
            .build();

        Self::new(diagnostic)
    }

    /// Create a "duplicate field" error
    pub fn duplicate_field(
        field_name: &str,
        span: Span,
        previous_span: Span,
        source_id: SourceId,
        in_struct: bool,
    ) -> Self {
        let context = if in_struct {
            "struct definition"
        } else {
            "struct literal"
        };

        let diagnostic = DiagnosticBuilder::error(
            format!("duplicate field `{}` in {}", field_name, context),
            source_id,
        )
        .with_code(E1002)
        .with_primary_label(span, "duplicate field")
        .with_secondary_label(previous_span, "field first defined here")
        .with_help("each field can only be defined once")
        .build();

        Self::new(diagnostic)
    }

    /// Create an "unclosed delimiter" error
    pub fn unclosed_delimiter(
        delimiter: &str,
        open_span: Span,
        current_span: Span,
        source_id: SourceId,
    ) -> Self {
        let diagnostic = DiagnosticBuilder::error(format!("unclosed `{}`", delimiter), source_id)
            .with_code(E1003)
            .with_primary_label(open_span, format!("`{}` opened here", delimiter))
            .with_secondary_label(current_span, "expected closing delimiter here")
            .with_help(format!(
                "add `{}` to close the delimiter",
                closing_delimiter(delimiter)
            ))
            .build();

        Self::new(diagnostic)
    }

    /// Create an "invalid array size" error
    pub fn invalid_array_size(size: i64, span: Span, source_id: SourceId) -> Self {
        let diagnostic =
            DiagnosticBuilder::error(format!("invalid array size: {}", size), source_id)
                .with_code(E1002)
                .with_primary_label(span, "array size must be non-negative")
                .with_help("use a positive integer for the array size")
                .build();

        Self::new(diagnostic)
    }

    /// Get the underlying diagnostic
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }

    /// Format with source context
    pub fn format_with_source(&self, source: &str) -> String {
        // This is a compatibility method for the old interface
        // The new diagnostic formatter should be used instead
        format!("{}: {}", self.diagnostic.severity, self.diagnostic.message)
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.diagnostic.message)
    }
}

impl std::error::Error for ParseError {}

impl From<ParseError> for Diagnostic {
    fn from(error: ParseError) -> Self {
        error.diagnostic
    }
}

/// Display user-friendly token names
pub fn token_display(token: &TokenKind) -> &'static str {
    match token {
        TokenKind::Fn => "fn",
        TokenKind::Let => "let",
        TokenKind::If => "if",
        TokenKind::Else => "else",
        TokenKind::While => "while",
        TokenKind::Return => "return",
        TokenKind::Struct => "struct",
        TokenKind::True => "true",
        TokenKind::False => "false",
        TokenKind::I32 => "i32",
        TokenKind::I64 => "i64",
        TokenKind::Bool => "bool",
        TokenKind::Unit => "()",
        TokenKind::LeftParen => "(",
        TokenKind::RightParen => ")",
        TokenKind::LeftBrace => "{",
        TokenKind::RightBrace => "}",
        TokenKind::LeftBracket => "[",
        TokenKind::RightBracket => "]",
        TokenKind::Semicolon => ";",
        TokenKind::Comma => ",",
        TokenKind::Dot => ".",
        TokenKind::Colon => ":",
        TokenKind::Arrow => "->",
        TokenKind::Plus => "+",
        TokenKind::Minus => "-",
        TokenKind::Star => "*",
        TokenKind::Slash => "/",
        TokenKind::Percent => "%",
        TokenKind::Assign => "=",
        TokenKind::Less => "<",
        TokenKind::LessEqual => "<=",
        TokenKind::Greater => ">",
        TokenKind::GreaterEqual => ">=",
        TokenKind::Equal => "==",
        TokenKind::NotEqual => "!=",
        TokenKind::Integer(_) => "integer literal",
        TokenKind::Ident(_) => "identifier",
        TokenKind::Comment(_) => "comment",
        TokenKind::Eof => "end of file",
    }
}

/// Get the closing delimiter for an opening delimiter
fn closing_delimiter(open: &str) -> &'static str {
    match open {
        "(" => ")",
        "{" => "}",
        "[" => "]",
        _ => "",
    }
}

/// Parser context for better error messages
#[derive(Debug, Clone, Copy)]
pub enum ParserContext {
    FunctionDefinition,
    FunctionParameters,
    FunctionBody,
    StructDefinition,
    StructFields,
    Statement,
    Expression,
    Type,
    Pattern,
    Block,
    IfCondition,
    WhileCondition,
    ArrayLiteral,
    TupleLiteral,
    StructLiteral,
}

impl ParserContext {
    pub fn description(&self) -> &'static str {
        match self {
            Self::FunctionDefinition => "function definition",
            Self::FunctionParameters => "function parameters",
            Self::FunctionBody => "function body",
            Self::StructDefinition => "struct definition",
            Self::StructFields => "struct fields",
            Self::Statement => "statement",
            Self::Expression => "expression",
            Self::Type => "type",
            Self::Pattern => "pattern",
            Self::Block => "block",
            Self::IfCondition => "if condition",
            Self::WhileCondition => "while condition",
            Self::ArrayLiteral => "array literal",
            Self::TupleLiteral => "tuple literal",
            Self::StructLiteral => "struct literal",
        }
    }
}
