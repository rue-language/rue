//! Error types for the Rue compiler.
//!
//! This crate provides the error infrastructure used throughout the compilation
//! pipeline. Errors carry source location information for diagnostic rendering.
//!
//! # Diagnostic System
//!
//! Errors and warnings can include rich diagnostic information:
//! - **Labels**: Secondary spans pointing to related code locations
//! - **Notes**: Informational context about the error/warning
//! - **Helps**: Actionable suggestions for fixing the issue
//!
//! Example:
//! ```ignore
//! CompileError::new(ErrorKind::TypeMismatch { ... }, span)
//!     .with_label("expected because of this", other_span)
//!     .with_help("consider using a type conversion")
//! ```

use rue_span::Span;
use std::collections::HashSet;
use std::fmt;

// ============================================================================
// Preview Features
// ============================================================================

/// A preview feature that can be enabled with `--preview`.
///
/// Preview features are in-progress language additions that:
/// - May change or be removed before stabilization
/// - Require explicit opt-in via `--preview <feature>`
/// - Allow incremental implementation to be merged to main
///
/// See ADR-020 for the full design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewFeature {
    /// Mutable strings with heap allocation and concatenation (ADR-019).
    MutableStrings,
    /// Hindley-Milner type inference (ADR-0007).
    HmInference,
    /// Struct methods and impl blocks (ADR-0009).
    Methods,
    /// Destructors for automatic cleanup (ADR-0010).
    Destructors,
}

impl PreviewFeature {
    /// Get the CLI name for this feature (used with `--preview`).
    pub fn name(&self) -> &'static str {
        match self {
            PreviewFeature::MutableStrings => "mutable_strings",
            PreviewFeature::HmInference => "hm_inference",
            PreviewFeature::Methods => "methods",
            PreviewFeature::Destructors => "destructors",
        }
    }

    /// Parse a feature name from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "mutable_strings" => Some(PreviewFeature::MutableStrings),
            "hm_inference" => Some(PreviewFeature::HmInference),
            "methods" => Some(PreviewFeature::Methods),
            "destructors" => Some(PreviewFeature::Destructors),
            _ => None,
        }
    }

    /// Get the ADR number documenting this feature.
    pub fn adr(&self) -> &'static str {
        match self {
            PreviewFeature::MutableStrings => "ADR-019",
            PreviewFeature::HmInference => "ADR-0007",
            PreviewFeature::Methods => "ADR-0009",
            PreviewFeature::Destructors => "ADR-0010",
        }
    }

    /// Get all available preview features.
    pub fn all() -> &'static [PreviewFeature] {
        &[
            PreviewFeature::MutableStrings,
            PreviewFeature::HmInference,
            PreviewFeature::Methods,
            PreviewFeature::Destructors,
        ]
    }

    /// Get a comma-separated list of all feature names (for help text).
    pub fn all_names() -> String {
        Self::all()
            .iter()
            .map(|f| f.name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl fmt::Display for PreviewFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// A set of enabled preview features.
pub type PreviewFeatures = HashSet<PreviewFeature>;

// ============================================================================
// Diagnostic Types
// ============================================================================

/// A secondary label pointing to related code.
///
/// Labels appear as additional annotations in the source snippet,
/// helping users understand the relationship between different parts of code.
#[derive(Debug, Clone)]
pub struct Label {
    /// The message explaining this location's relevance.
    pub message: String,
    /// The source location to highlight.
    pub span: Span,
}

impl Label {
    /// Create a new label with a message and span.
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

/// An informational note providing context.
///
/// Notes appear as footer messages and explain why something happened
/// or provide additional context about the diagnostic.
#[derive(Debug, Clone)]
pub struct Note(pub String);

impl Note {
    /// Create a new note.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An actionable help suggestion.
///
/// Helps appear as footer messages and suggest specific actions
/// the user can take to resolve the issue.
#[derive(Debug, Clone)]
pub struct Help(pub String);

impl Help {
    /// Create a new help suggestion.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for Help {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Rich diagnostic information for errors and warnings.
///
/// This struct collects all supplementary information that can be
/// attached to a diagnostic message.
#[derive(Debug, Clone, Default)]
pub struct Diagnostic {
    /// Secondary labels pointing to related code locations.
    pub labels: Vec<Label>,
    /// Informational notes providing context.
    pub notes: Vec<Note>,
    /// Actionable help suggestions.
    pub helps: Vec<Help>,
}

impl Diagnostic {
    /// Create an empty diagnostic.
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if this diagnostic has any content.
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.notes.is_empty() && self.helps.is_empty()
    }
}

// ============================================================================
// Compile Errors
// ============================================================================

/// A compilation error with optional source location information.
///
/// Some errors (like `NoMainFunction` or `LinkError`) don't have a meaningful
/// source location. Use `has_span()` to check before rendering location info.
///
/// Errors can include rich diagnostic information using the builder methods:
/// ```ignore
/// CompileError::new(ErrorKind::TypeMismatch { ... }, span)
///     .with_label("expected because of this", other_span)
///     .with_note("types must match exactly")
///     .with_help("consider adding a type conversion")
/// ```
#[derive(Debug, Clone)]
pub struct CompileError {
    pub kind: ErrorKind,
    span: Option<Span>,
    diagnostic: Diagnostic,
}

/// The kind of compilation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    // Lexer errors
    UnexpectedCharacter(char),
    InvalidInteger,
    InvalidStringEscape(char),
    UnterminatedString,

    // Parser errors
    UnexpectedToken {
        expected: &'static str,
        found: String,
    },
    UnexpectedEof {
        expected: &'static str,
    },

    // Semantic errors
    NoMainFunction,
    UndefinedVariable(String),
    UndefinedFunction(String),
    AssignToImmutable(String),
    UnknownType(String),
    TypeMismatch {
        expected: String,
        found: String,
    },
    WrongArgumentCount {
        expected: usize,
        found: usize,
    },

    // Struct errors
    MissingFields {
        struct_name: String,
        missing_fields: Vec<String>,
    },
    UnknownField {
        struct_name: String,
        field_name: String,
    },
    DuplicateField {
        struct_name: String,
        field_name: String,
    },

    // Enum errors
    DuplicateVariant {
        enum_name: String,
        variant_name: String,
    },
    UnknownVariant {
        enum_name: String,
        variant_name: String,
    },
    UnknownEnumType(String),
    FieldWrongOrder {
        struct_name: String,
        expected_field: String,
        found_field: String,
    },
    FieldAccessOnNonStruct {
        found: String,
    },
    InvalidAssignmentTarget,

    // Control flow errors
    BreakOutsideLoop,
    ContinueOutsideLoop,

    // Match errors
    NonExhaustiveMatch,
    EmptyMatch,
    InvalidMatchType(String),

    // Intrinsic errors
    UnknownIntrinsic(String),
    IntrinsicWrongArgCount {
        name: String,
        expected: usize,
        found: usize,
    },
    IntrinsicTypeMismatch {
        name: String,
        expected: String,
        found: String,
    },

    // Literal errors
    LiteralOutOfRange {
        value: u64,
        ty: String,
    },

    // Operator errors
    CannotNegateUnsigned(String),
    ChainedComparison,

    // Array errors
    IndexOnNonArray {
        found: String,
    },
    ArrayLengthMismatch {
        expected: u64,
        found: u64,
    },
    IndexOutOfBounds {
        index: i64,
        length: u64,
    },
    TypeAnnotationRequired,

    // Linker errors
    LinkError(String),

    // Target errors
    UnsupportedTarget(String),

    // Preview feature errors
    PreviewFeatureRequired {
        feature: PreviewFeature,
        what: String,
    },

    // Move semantics errors
    UseAfterMove {
        var_name: String,
    },
}

impl CompileError {
    /// Create a new error with the given kind and span.
    #[inline]
    pub fn new(kind: ErrorKind, span: Span) -> Self {
        Self {
            kind,
            span: Some(span),
            diagnostic: Diagnostic::new(),
        }
    }

    /// Create an error without a source location.
    ///
    /// Use this for errors that don't correspond to a specific source location,
    /// such as "no main function found" or linker errors.
    #[inline]
    pub fn without_span(kind: ErrorKind) -> Self {
        Self {
            kind,
            span: None,
            diagnostic: Diagnostic::new(),
        }
    }

    /// Create an error at a specific position (zero-length span).
    #[inline]
    pub fn at(kind: ErrorKind, pos: u32) -> Self {
        Self {
            kind,
            span: Some(Span::point(pos)),
            diagnostic: Diagnostic::new(),
        }
    }

    /// Returns true if this error has source location information.
    #[inline]
    pub fn has_span(&self) -> bool {
        self.span.is_some()
    }

    /// Get the span, if present.
    #[inline]
    pub fn span(&self) -> Option<Span> {
        self.span
    }

    /// Get the diagnostic information.
    #[inline]
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }

    /// Add a secondary label pointing to related code.
    ///
    /// Labels appear as additional annotations in the source snippet.
    #[inline]
    pub fn with_label(mut self, message: impl Into<String>, span: Span) -> Self {
        self.diagnostic.labels.push(Label::new(message, span));
        self
    }

    /// Add an informational note.
    ///
    /// Notes appear as footer messages providing context.
    #[inline]
    pub fn with_note(mut self, message: impl Into<String>) -> Self {
        self.diagnostic.notes.push(Note::new(message));
        self
    }

    /// Add a help suggestion.
    ///
    /// Helps appear as footer messages with actionable advice.
    #[inline]
    pub fn with_help(mut self, message: impl Into<String>) -> Self {
        self.diagnostic.helps.push(Help::new(message));
        self
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for CompileError {}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::UnexpectedCharacter(c) => write!(f, "unexpected character: {}", c),
            ErrorKind::InvalidInteger => write!(f, "invalid integer literal"),
            ErrorKind::InvalidStringEscape(c) => write!(f, "invalid escape sequence: \\{}", c),
            ErrorKind::UnterminatedString => write!(f, "unterminated string literal"),
            ErrorKind::UnexpectedToken { expected, found } => {
                write!(f, "expected {}, found {}", expected, found)
            }
            ErrorKind::UnexpectedEof { expected } => {
                write!(f, "unexpected end of file, expected {}", expected)
            }
            ErrorKind::NoMainFunction => write!(f, "no main function found"),
            ErrorKind::UndefinedVariable(name) => write!(f, "undefined variable '{}'", name),
            ErrorKind::UndefinedFunction(name) => write!(f, "undefined function '{}'", name),
            ErrorKind::AssignToImmutable(name) => {
                write!(f, "cannot assign to immutable variable '{}'", name)
            }
            ErrorKind::UnknownType(name) => write!(f, "unknown type '{}'", name),
            ErrorKind::TypeMismatch { expected, found } => {
                write!(f, "type mismatch: expected {}, found {}", expected, found)
            }
            ErrorKind::WrongArgumentCount { expected, found } => {
                if *expected == 1 {
                    write!(f, "expected {} argument, found {}", expected, found)
                } else {
                    write!(f, "expected {} arguments, found {}", expected, found)
                }
            }
            ErrorKind::MissingFields {
                struct_name,
                missing_fields,
            } => {
                if missing_fields.len() == 1 {
                    write!(
                        f,
                        "missing field '{}' in struct '{}'",
                        missing_fields[0], struct_name
                    )
                } else {
                    let fields = missing_fields
                        .iter()
                        .map(|f| format!("'{}'", f))
                        .collect::<Vec<_>>()
                        .join(", ");
                    write!(f, "missing fields {} in struct '{}'", fields, struct_name)
                }
            }
            ErrorKind::UnknownField {
                struct_name,
                field_name,
            } => {
                write!(
                    f,
                    "unknown field '{}' in struct '{}'",
                    field_name, struct_name
                )
            }
            ErrorKind::DuplicateField {
                struct_name,
                field_name,
            } => {
                write!(
                    f,
                    "duplicate field '{}' in struct '{}'",
                    field_name, struct_name
                )
            }
            ErrorKind::DuplicateVariant {
                enum_name,
                variant_name,
            } => {
                write!(
                    f,
                    "duplicate variant '{}' in enum '{}'",
                    variant_name, enum_name
                )
            }
            ErrorKind::UnknownVariant {
                enum_name,
                variant_name,
            } => {
                write!(
                    f,
                    "unknown variant '{}' in enum '{}'",
                    variant_name, enum_name
                )
            }
            ErrorKind::UnknownEnumType(name) => {
                write!(f, "unknown enum type '{}'", name)
            }
            ErrorKind::FieldWrongOrder {
                struct_name,
                expected_field,
                found_field,
            } => {
                write!(
                    f,
                    "struct '{}' fields must be initialized in declaration order: expected '{}', found '{}'",
                    struct_name, expected_field, found_field
                )
            }
            ErrorKind::FieldAccessOnNonStruct { found } => {
                write!(f, "field access on non-struct type '{}'", found)
            }
            ErrorKind::InvalidAssignmentTarget => {
                write!(f, "invalid assignment target")
            }
            ErrorKind::BreakOutsideLoop => write!(f, "'break' outside of loop"),
            ErrorKind::ContinueOutsideLoop => write!(f, "'continue' outside of loop"),
            ErrorKind::NonExhaustiveMatch => write!(f, "match is not exhaustive"),
            ErrorKind::EmptyMatch => write!(f, "match expression has no arms"),
            ErrorKind::InvalidMatchType(ty) => {
                write!(
                    f,
                    "cannot match on type '{}', expected integer, bool, or enum",
                    ty
                )
            }
            ErrorKind::UnknownIntrinsic(name) => write!(f, "unknown intrinsic '@{}'", name),
            ErrorKind::IntrinsicWrongArgCount {
                name,
                expected,
                found,
            } => {
                if *expected == 1 {
                    write!(
                        f,
                        "intrinsic '@{}' expects {} argument, found {}",
                        name, expected, found
                    )
                } else {
                    write!(
                        f,
                        "intrinsic '@{}' expects {} arguments, found {}",
                        name, expected, found
                    )
                }
            }
            ErrorKind::IntrinsicTypeMismatch {
                name,
                expected,
                found,
            } => {
                write!(
                    f,
                    "intrinsic '@{}' expects {}, found {}",
                    name, expected, found
                )
            }
            ErrorKind::LiteralOutOfRange { value, ty } => {
                write!(
                    f,
                    "literal value {} is out of range for type '{}'",
                    value, ty
                )
            }
            ErrorKind::CannotNegateUnsigned(ty) => {
                write!(f, "cannot apply unary operator `-` to type '{}'", ty)
            }
            ErrorKind::ChainedComparison => {
                write!(f, "comparison operators cannot be chained")
            }
            ErrorKind::IndexOnNonArray { found } => {
                write!(f, "cannot index into non-array type '{}'", found)
            }
            ErrorKind::ArrayLengthMismatch { expected, found } => {
                if *expected == 1 {
                    write!(
                        f,
                        "expected array of {} element, found {} elements",
                        expected, found
                    )
                } else {
                    write!(
                        f,
                        "expected array of {} elements, found {} elements",
                        expected, found
                    )
                }
            }
            ErrorKind::IndexOutOfBounds { index, length } => {
                write!(
                    f,
                    "index out of bounds: the length is {} but the index is {}",
                    length, index
                )
            }
            ErrorKind::TypeAnnotationRequired => {
                write!(f, "type annotation required for empty array")
            }
            ErrorKind::LinkError(msg) => write!(f, "link error: {}", msg),
            ErrorKind::UnsupportedTarget(msg) => write!(f, "unsupported target: {}", msg),
            ErrorKind::PreviewFeatureRequired { feature, what } => {
                write!(f, "{} requires preview feature `{}`", what, feature.name())
            }
            ErrorKind::UseAfterMove { var_name } => {
                write!(f, "use of moved value '{}'", var_name)
            }
        }
    }
}

/// Result type for compilation operations.
pub type CompileResult<T> = Result<T, CompileError>;

/// The kind of compilation warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarningKind {
    /// A variable was declared but never used.
    UnusedVariable(String),
    /// A function was declared but never called.
    UnusedFunction(String),
    /// Code that will never be executed.
    UnreachableCode,
    /// A pattern that will never be matched because a previous pattern already covers it.
    UnreachablePattern(String),
}

/// A compilation warning with optional source location information.
///
/// Warnings don't stop compilation but indicate potential issues in the code.
///
/// Warnings can include rich diagnostic information using the builder methods:
/// ```ignore
/// CompileWarning::new(WarningKind::UnusedVariable("x".into()), span)
///     .with_help("if this is intentional, prefix it with an underscore: `_x`")
/// ```
#[derive(Debug, Clone)]
pub struct CompileWarning {
    pub kind: WarningKind,
    span: Option<Span>,
    diagnostic: Diagnostic,
}

impl CompileWarning {
    /// Create a new warning with the given kind and span.
    #[inline]
    pub fn new(kind: WarningKind, span: Span) -> Self {
        Self {
            kind,
            span: Some(span),
            diagnostic: Diagnostic::new(),
        }
    }

    /// Create a warning without a source location.
    #[inline]
    pub fn without_span(kind: WarningKind) -> Self {
        Self {
            kind,
            span: None,
            diagnostic: Diagnostic::new(),
        }
    }

    /// Returns true if this warning has source location information.
    #[inline]
    pub fn has_span(&self) -> bool {
        self.span.is_some()
    }

    /// Get the span, if present.
    #[inline]
    pub fn span(&self) -> Option<Span> {
        self.span
    }

    /// Get the diagnostic information.
    #[inline]
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }

    /// Add a secondary label pointing to related code.
    ///
    /// Labels appear as additional annotations in the source snippet.
    #[inline]
    pub fn with_label(mut self, message: impl Into<String>, span: Span) -> Self {
        self.diagnostic.labels.push(Label::new(message, span));
        self
    }

    /// Add an informational note.
    ///
    /// Notes appear as footer messages providing context.
    #[inline]
    pub fn with_note(mut self, message: impl Into<String>) -> Self {
        self.diagnostic.notes.push(Note::new(message));
        self
    }

    /// Add a help suggestion.
    ///
    /// Helps appear as footer messages with actionable advice.
    #[inline]
    pub fn with_help(mut self, message: impl Into<String>) -> Self {
        self.diagnostic.helps.push(Help::new(message));
        self
    }
}

impl fmt::Display for CompileWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl WarningKind {
    /// Returns the variable name if this is an UnusedVariable warning.
    pub fn unused_variable_name(&self) -> Option<&str> {
        match self {
            WarningKind::UnusedVariable(name) => Some(name),
            _ => None,
        }
    }

    /// Format the warning message with an optional line number.
    ///
    /// When `line_number` is Some, the line number is appended to the message
    /// for warnings that have a name (like unused variables). This helps
    /// disambiguate when multiple variables share the same name.
    pub fn format_with_line(&self, line_number: Option<usize>) -> String {
        match (self, line_number) {
            (WarningKind::UnusedVariable(name), Some(line)) => {
                format!("unused variable '{}' (line {})", name, line)
            }
            (WarningKind::UnusedFunction(name), Some(line)) => {
                format!("unused function '{}' (line {})", name, line)
            }
            _ => self.to_string(),
        }
    }
}

impl fmt::Display for WarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WarningKind::UnusedVariable(name) => write!(f, "unused variable '{}'", name),
            WarningKind::UnusedFunction(name) => write!(f, "unused function '{}'", name),
            WarningKind::UnreachableCode => write!(f, "unreachable code"),
            WarningKind::UnreachablePattern(pat) => write!(f, "unreachable pattern '{}'", pat),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_with_span() {
        let span = Span::new(10, 20);
        let error = CompileError::new(ErrorKind::InvalidInteger, span);

        assert!(error.has_span());
        assert_eq!(error.span(), Some(span));
        assert_eq!(error.to_string(), "invalid integer literal");
    }

    #[test]
    fn test_error_without_span() {
        let error = CompileError::without_span(ErrorKind::NoMainFunction);

        assert!(!error.has_span());
        assert_eq!(error.span(), None);
        assert_eq!(error.to_string(), "no main function found");
    }

    #[test]
    fn test_error_at_position() {
        let error = CompileError::at(ErrorKind::InvalidInteger, 42);

        assert!(error.has_span());
        assert_eq!(error.span(), Some(Span::point(42)));
    }

    #[test]
    fn test_unexpected_character_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedCharacter('@'));
        assert_eq!(error.to_string(), "unexpected character: @");
    }

    #[test]
    fn test_unexpected_token_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedToken {
            expected: "identifier",
            found: "'+'".to_string(),
        });
        assert_eq!(error.to_string(), "expected identifier, found '+'");
    }

    #[test]
    fn test_unexpected_eof_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedEof { expected: "'}'" });
        assert_eq!(error.to_string(), "unexpected end of file, expected '}'");
    }

    #[test]
    fn test_undefined_variable_message() {
        let error = CompileError::without_span(ErrorKind::UndefinedVariable("foo".to_string()));
        assert_eq!(error.to_string(), "undefined variable 'foo'");
    }

    #[test]
    fn test_undefined_function_message() {
        let error = CompileError::without_span(ErrorKind::UndefinedFunction("bar".to_string()));
        assert_eq!(error.to_string(), "undefined function 'bar'");
    }

    #[test]
    fn test_assign_to_immutable_message() {
        let error = CompileError::without_span(ErrorKind::AssignToImmutable("x".to_string()));
        assert_eq!(error.to_string(), "cannot assign to immutable variable 'x'");
    }

    #[test]
    fn test_unknown_type_message() {
        let error = CompileError::without_span(ErrorKind::UnknownType("Foo".to_string()));
        assert_eq!(error.to_string(), "unknown type 'Foo'");
    }

    #[test]
    fn test_type_mismatch_message() {
        let error = CompileError::without_span(ErrorKind::TypeMismatch {
            expected: "i32".to_string(),
            found: "bool".to_string(),
        });
        assert_eq!(error.to_string(), "type mismatch: expected i32, found bool");
    }

    #[test]
    fn test_wrong_argument_count_singular() {
        let error = CompileError::without_span(ErrorKind::WrongArgumentCount {
            expected: 1,
            found: 3,
        });
        assert_eq!(error.to_string(), "expected 1 argument, found 3");
    }

    #[test]
    fn test_wrong_argument_count_plural() {
        let error = CompileError::without_span(ErrorKind::WrongArgumentCount {
            expected: 2,
            found: 0,
        });
        assert_eq!(error.to_string(), "expected 2 arguments, found 0");
    }

    #[test]
    fn test_link_error_message() {
        let error =
            CompileError::without_span(ErrorKind::LinkError("undefined symbol".to_string()));
        assert_eq!(error.to_string(), "link error: undefined symbol");
    }

    #[test]
    fn test_error_kind_equality() {
        assert_eq!(ErrorKind::InvalidInteger, ErrorKind::InvalidInteger);
        assert_eq!(ErrorKind::NoMainFunction, ErrorKind::NoMainFunction);
        assert_ne!(ErrorKind::InvalidInteger, ErrorKind::NoMainFunction);
    }

    #[test]
    fn test_error_implements_std_error() {
        fn assert_error<T: std::error::Error>() {}
        assert_error::<CompileError>();
    }

    // ========================================================================
    // Diagnostic tests
    // ========================================================================

    #[test]
    fn test_diagnostic_empty_by_default() {
        let diag = Diagnostic::new();
        assert!(diag.is_empty());
        assert!(diag.labels.is_empty());
        assert!(diag.notes.is_empty());
        assert!(diag.helps.is_empty());
    }

    #[test]
    fn test_diagnostic_not_empty_with_label() {
        let mut diag = Diagnostic::new();
        diag.labels.push(Label::new("test", Span::new(0, 10)));
        assert!(!diag.is_empty());
    }

    #[test]
    fn test_diagnostic_not_empty_with_note() {
        let mut diag = Diagnostic::new();
        diag.notes.push(Note::new("test note"));
        assert!(!diag.is_empty());
    }

    #[test]
    fn test_diagnostic_not_empty_with_help() {
        let mut diag = Diagnostic::new();
        diag.helps.push(Help::new("test help"));
        assert!(!diag.is_empty());
    }

    #[test]
    fn test_label_creation() {
        let span = Span::new(10, 20);
        let label = Label::new("expected type here", span);
        assert_eq!(label.message, "expected type here");
        assert_eq!(label.span, span);
    }

    #[test]
    fn test_note_display() {
        let note = Note::new("types must match exactly");
        assert_eq!(note.to_string(), "types must match exactly");
    }

    #[test]
    fn test_help_display() {
        let help = Help::new("consider adding a type annotation");
        assert_eq!(help.to_string(), "consider adding a type annotation");
    }

    #[test]
    fn test_error_with_label() {
        let span = Span::new(10, 20);
        let label_span = Span::new(0, 5);
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            span,
        )
        .with_label("expected because of this", label_span);

        let diag = error.diagnostic();
        assert_eq!(diag.labels.len(), 1);
        assert_eq!(diag.labels[0].message, "expected because of this");
        assert_eq!(diag.labels[0].span, label_span);
    }

    #[test]
    fn test_error_with_note() {
        let span = Span::new(10, 20);
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            span,
        )
        .with_note("if and else branches must have compatible types");

        let diag = error.diagnostic();
        assert_eq!(diag.notes.len(), 1);
        assert_eq!(
            diag.notes[0].to_string(),
            "if and else branches must have compatible types"
        );
    }

    #[test]
    fn test_error_with_help() {
        let span = Span::new(10, 20);
        let error = CompileError::new(ErrorKind::AssignToImmutable("x".to_string()), span)
            .with_help("consider making `x` mutable: `let mut x`");

        let diag = error.diagnostic();
        assert_eq!(diag.helps.len(), 1);
        assert_eq!(
            diag.helps[0].to_string(),
            "consider making `x` mutable: `let mut x`"
        );
    }

    #[test]
    fn test_error_with_multiple_diagnostics() {
        let span = Span::new(10, 20);
        let label_span = Span::new(0, 5);
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            span,
        )
        .with_label("then branch is here", label_span)
        .with_note("if and else branches must have compatible types")
        .with_help("consider using a type conversion");

        let diag = error.diagnostic();
        assert_eq!(diag.labels.len(), 1);
        assert_eq!(diag.notes.len(), 1);
        assert_eq!(diag.helps.len(), 1);
    }

    #[test]
    fn test_error_diagnostic_empty_by_default() {
        let span = Span::new(10, 20);
        let error = CompileError::new(ErrorKind::InvalidInteger, span);
        assert!(error.diagnostic().is_empty());
    }

    #[test]
    fn test_warning_with_help() {
        let span = Span::new(10, 20);
        let warning = CompileWarning::new(WarningKind::UnusedVariable("foo".to_string()), span)
            .with_help("if this is intentional, prefix it with an underscore: `_foo`");

        let diag = warning.diagnostic();
        assert_eq!(diag.helps.len(), 1);
        assert_eq!(
            diag.helps[0].to_string(),
            "if this is intentional, prefix it with an underscore: `_foo`"
        );
    }

    #[test]
    fn test_warning_with_label_and_note() {
        let span = Span::new(20, 25);
        let diverging_span = Span::new(10, 18);
        let warning = CompileWarning::new(WarningKind::UnreachableCode, span)
            .with_label(
                "any code following this expression is unreachable",
                diverging_span,
            )
            .with_note("this warning occurs because the preceding expression diverges");

        let diag = warning.diagnostic();
        assert_eq!(diag.labels.len(), 1);
        assert_eq!(diag.labels[0].span, diverging_span);
        assert_eq!(diag.notes.len(), 1);
    }

    #[test]
    fn test_warning_diagnostic_empty_by_default() {
        let span = Span::new(10, 20);
        let warning = CompileWarning::new(WarningKind::UnreachableCode, span);
        assert!(warning.diagnostic().is_empty());
    }

    // ========================================================================
    // Preview feature tests
    // ========================================================================

    #[test]
    fn test_preview_feature_name() {
        assert_eq!(PreviewFeature::MutableStrings.name(), "mutable_strings");
    }

    #[test]
    fn test_preview_feature_from_str() {
        assert_eq!(
            PreviewFeature::from_str("mutable_strings"),
            Some(PreviewFeature::MutableStrings)
        );
        assert_eq!(PreviewFeature::from_str("unknown"), None);
        assert_eq!(PreviewFeature::from_str(""), None);
    }

    #[test]
    fn test_preview_feature_adr() {
        assert_eq!(PreviewFeature::MutableStrings.adr(), "ADR-019");
    }

    #[test]
    fn test_preview_feature_all() {
        let all = PreviewFeature::all();
        assert!(!all.is_empty());
        assert!(all.contains(&PreviewFeature::MutableStrings));
    }

    #[test]
    fn test_preview_feature_all_names() {
        let names = PreviewFeature::all_names();
        assert!(names.contains("mutable_strings"));
    }

    #[test]
    fn test_preview_feature_display() {
        assert_eq!(
            format!("{}", PreviewFeature::MutableStrings),
            "mutable_strings"
        );
    }

    #[test]
    fn test_preview_feature_required_error() {
        let span = Span::new(10, 20);
        let error = CompileError::new(
            ErrorKind::PreviewFeatureRequired {
                feature: PreviewFeature::MutableStrings,
                what: "string concatenation".to_string(),
            },
            span,
        );
        assert_eq!(
            error.to_string(),
            "string concatenation requires preview feature `mutable_strings`"
        );
    }
}
