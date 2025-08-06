//! Integration of rich diagnostics into the existing parser

use crate::ParseError;
use rue_ast::*;
use rue_diagnostic::{
    Diagnostic, DiagnosticBuilder, DiagnosticCode, E1001, E1002, E1003, E2001, SourceId,
    SourceManager,
};
use rue_lexer::{Span, TokenKind};

/// Enhanced parse error with diagnostic support
impl ParseError {
    /// Create an enhanced parse error with a diagnostic
    pub fn enhanced(
        message: String,
        span: Span,
        source_id: SourceId,
        code: DiagnosticCode,
        help: Option<String>,
    ) -> Self {
        let builder = DiagnosticBuilder::error(message.clone(), source_id)
            .with_code(code)
            .with_primary_span(span);

        let _diagnostic = if let Some(help_text) = help {
            builder.with_help(help_text).build()
        } else {
            builder.build()
        };

        // For now, we still return the simple ParseError
        // In the future, we can migrate to returning the full diagnostic
        ParseError { message, span }
    }

    /// Convert to a diagnostic for rich error reporting
    pub fn to_diagnostic(&self, source_id: SourceId) -> Diagnostic {
        // Try to determine the error type based on the message
        let (code, help) = categorize_error(&self.message);

        let mut builder =
            DiagnosticBuilder::error(&self.message, source_id).with_primary_span(self.span);

        if let Some(code) = code {
            builder = builder.with_code(code);
        }

        if let Some(help_text) = help {
            builder = builder.with_help(help_text);
        }

        builder.build()
    }
}

/// Categorize an error message to provide better diagnostics
fn categorize_error(message: &str) -> (Option<DiagnosticCode>, Option<String>) {
    if message.contains("Expected") && message.contains("found") {
        (
            Some(E1001),
            Some("Check that you have the correct syntax for this construct".to_string()),
        )
    } else if message.contains("Duplicate field") {
        (
            Some(E1002),
            Some("Each field name must be unique".to_string()),
        )
    } else if message.contains("Unexpected end of input") {
        (
            Some(E1003),
            Some("You may be missing a closing delimiter or semicolon".to_string()),
        )
    } else if message.contains("identifier") {
        (
            Some(E2001),
            Some("Identifiers must start with a letter or underscore".to_string()),
        )
    } else {
        (None, None)
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

/// Parse with enhanced diagnostics
pub fn parse_with_diagnostics(source: &str, source_name: &str) -> Result<CstRoot, Vec<Diagnostic>> {
    use rue_lexer::Lexer;

    let mut source_manager = SourceManager::new();
    let source_id = source_manager.add_source(source_name, source);

    // Tokenize
    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(tokens) => tokens,
        Err(e) => {
            let diagnostic = DiagnosticBuilder::error(
                format!("Lexical error: {}", e.message),
                source_id.clone(),
            )
            .with_code(E1001)
            .with_primary_span(Span::new(e.position, e.position + 1))
            .build();
            return Err(vec![diagnostic]);
        }
    };

    // TokenNode is just a type alias for Token
    let token_nodes = tokens;

    // Parse
    match crate::parse(token_nodes) {
        Ok(cst) => Ok(cst),
        Err(e) => {
            let diagnostic = e.to_diagnostic(source_id);
            Err(vec![diagnostic])
        }
    }
}

/// Parse with error recovery, collecting multiple errors
pub fn parse_with_recovery(source: &str, source_name: &str) -> Result<CstRoot, Vec<Diagnostic>> {
    use crate::error_recovery::RecoveringParser;
    use rue_lexer::Lexer;

    let mut source_manager = SourceManager::new();
    let source_id = source_manager.add_source(source_name, source);

    // Tokenize
    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(tokens) => tokens,
        Err(e) => {
            let diagnostic = DiagnosticBuilder::error(
                format!("Lexical error: {}", e.message),
                source_id.clone(),
            )
            .with_code(E1001)
            .with_primary_span(Span::new(e.position, e.position + 1))
            .build();
            return Err(vec![diagnostic]);
        }
    };

    // Parse with recovery
    let parser = RecoveringParser::new(tokens);
    let (cst, errors) = parser.parse_with_recovery();

    if errors.is_empty() {
        Ok(cst.unwrap_or_else(|| CstRoot {
            items: vec![],
            trivia: Trivia {
                leading: vec![],
                trailing: vec![],
            },
        }))
    } else {
        // Convert ParseErrors to Diagnostics
        let diagnostics: Vec<Diagnostic> = errors
            .into_iter()
            .map(|e| e.to_diagnostic(source_id.clone()))
            .collect();

        if let Some(_cst) = cst {
            // We have a partial CST and errors
            // For now, just return the errors
            Err(diagnostics)
        } else {
            Err(diagnostics)
        }
    }
}
