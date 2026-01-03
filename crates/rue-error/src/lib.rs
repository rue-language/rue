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
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use thiserror::Error;

// ============================================================================
// Error Codes
// ============================================================================
//
// Every error kind has a unique, stable error code for searchability.
// Codes are assigned by category and must never change once assigned.
// See issue rue-0c9y for the design rationale.

/// A unique error code for each error type.
///
/// Error codes are formatted as `E` followed by a 4-digit zero-padded number
/// (e.g., `E0001`, `E0042`). They are assigned by category:
///
/// - **E0001-E0099**: Lexer errors (tokenization)
/// - **E0100-E0199**: Parser errors (syntax)
/// - **E0200-E0399**: Semantic errors (types, names, scopes)
/// - **E0400-E0499**: Struct/enum errors
/// - **E0500-E0599**: Control flow errors
/// - **E0600-E0699**: Match errors
/// - **E0700-E0799**: Intrinsic errors
/// - **E0800-E0899**: Literal/operator errors
/// - **E0900-E0999**: Array errors
/// - **E1000-E1099**: Linker/target errors
/// - **E1100-E1199**: Preview feature errors
/// - **E9000-E9999**: Internal compiler errors
///
/// Once assigned, error codes must never change to maintain stability for
/// documentation, search engines, and user bookmarks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode(pub u16);

impl ErrorCode {
    // ========================================================================
    // Lexer errors (E0001-E0099)
    // ========================================================================
    pub const UNEXPECTED_CHARACTER: Self = Self(1);
    pub const INVALID_INTEGER: Self = Self(2);
    pub const INVALID_STRING_ESCAPE: Self = Self(3);
    pub const UNTERMINATED_STRING: Self = Self(4);

    // ========================================================================
    // Parser errors (E0100-E0199)
    // ========================================================================
    pub const UNEXPECTED_TOKEN: Self = Self(100);
    pub const UNEXPECTED_EOF: Self = Self(101);
    pub const PARSE_ERROR: Self = Self(102);

    // ========================================================================
    // Semantic errors (E0200-E0399)
    // ========================================================================
    pub const NO_MAIN_FUNCTION: Self = Self(200);
    pub const UNDEFINED_VARIABLE: Self = Self(201);
    pub const UNDEFINED_FUNCTION: Self = Self(202);
    pub const ASSIGN_TO_IMMUTABLE: Self = Self(203);
    pub const UNKNOWN_TYPE: Self = Self(204);
    pub const USE_AFTER_MOVE: Self = Self(205);
    pub const TYPE_MISMATCH: Self = Self(206);
    pub const WRONG_ARGUMENT_COUNT: Self = Self(207);

    // ========================================================================
    // Struct/enum errors (E0400-E0499)
    // ========================================================================
    pub const MISSING_FIELDS: Self = Self(400);
    pub const UNKNOWN_FIELD: Self = Self(401);
    pub const DUPLICATE_FIELD: Self = Self(402);
    pub const COPY_STRUCT_NON_COPY_FIELD: Self = Self(403);
    pub const RESERVED_TYPE_NAME: Self = Self(404);
    pub const DUPLICATE_TYPE_DEFINITION: Self = Self(405);
    pub const LINEAR_VALUE_NOT_CONSUMED: Self = Self(406);
    pub const LINEAR_STRUCT_COPY: Self = Self(407);
    pub const HANDLE_STRUCT_MISSING_METHOD: Self = Self(408);
    pub const HANDLE_METHOD_WRONG_SIGNATURE: Self = Self(409);
    pub const DUPLICATE_METHOD: Self = Self(410);
    pub const UNDEFINED_METHOD: Self = Self(411);
    pub const UNDEFINED_ASSOC_FN: Self = Self(412);
    pub const METHOD_CALL_ON_NON_STRUCT: Self = Self(413);
    pub const METHOD_CALLED_AS_ASSOC_FN: Self = Self(414);
    pub const ASSOC_FN_CALLED_AS_METHOD: Self = Self(415);
    pub const DUPLICATE_DESTRUCTOR: Self = Self(416);
    pub const DESTRUCTOR_UNKNOWN_TYPE: Self = Self(417);
    pub const DUPLICATE_CONSTANT: Self = Self(418);
    pub const DUPLICATE_VARIANT: Self = Self(419);
    pub const UNKNOWN_VARIANT: Self = Self(420);
    pub const UNKNOWN_ENUM_TYPE: Self = Self(421);
    pub const FIELD_WRONG_ORDER: Self = Self(422);
    pub const FIELD_ACCESS_ON_NON_STRUCT: Self = Self(423);
    pub const INVALID_ASSIGNMENT_TARGET: Self = Self(424);
    pub const INOUT_NON_LVALUE: Self = Self(425);
    pub const INOUT_EXCLUSIVE_ACCESS: Self = Self(426);
    pub const BORROW_NON_LVALUE: Self = Self(427);
    pub const MUTATE_BORROWED_VALUE: Self = Self(428);
    pub const MOVE_OUT_OF_BORROW: Self = Self(429);
    pub const BORROW_INOUT_CONFLICT: Self = Self(430);
    pub const INOUT_KEYWORD_MISSING: Self = Self(431);
    pub const BORROW_KEYWORD_MISSING: Self = Self(432);
    pub const EMPTY_STRUCT: Self = Self(433);

    // ========================================================================
    // Control flow errors (E0500-E0599)
    // ========================================================================
    pub const BREAK_OUTSIDE_LOOP: Self = Self(500);
    pub const CONTINUE_OUTSIDE_LOOP: Self = Self(501);

    // ========================================================================
    // Match errors (E0600-E0699)
    // ========================================================================
    pub const NON_EXHAUSTIVE_MATCH: Self = Self(600);
    pub const EMPTY_MATCH: Self = Self(601);
    pub const INVALID_MATCH_TYPE: Self = Self(602);

    // ========================================================================
    // Intrinsic errors (E0700-E0799)
    // ========================================================================
    pub const UNKNOWN_INTRINSIC: Self = Self(700);
    pub const INTRINSIC_WRONG_ARG_COUNT: Self = Self(701);
    pub const INTRINSIC_TYPE_MISMATCH: Self = Self(702);
    pub const IMPORT_REQUIRES_STRING_LITERAL: Self = Self(703);
    pub const MODULE_NOT_FOUND: Self = Self(704);

    // ========================================================================
    // Literal/operator errors (E0800-E0899)
    // ========================================================================
    pub const LITERAL_OUT_OF_RANGE: Self = Self(800);
    pub const CANNOT_NEGATE_UNSIGNED: Self = Self(801);
    pub const CHAINED_COMPARISON: Self = Self(802);

    // ========================================================================
    // Array errors (E0900-E0999)
    // ========================================================================
    pub const INDEX_ON_NON_ARRAY: Self = Self(900);
    pub const ARRAY_LENGTH_MISMATCH: Self = Self(901);
    pub const INDEX_OUT_OF_BOUNDS: Self = Self(902);
    pub const TYPE_ANNOTATION_REQUIRED: Self = Self(903);
    pub const MOVE_OUT_OF_INDEX: Self = Self(904);

    // ========================================================================
    // Linker/target errors (E1000-E1099)
    // ========================================================================
    pub const LINK_ERROR: Self = Self(1000);
    pub const UNSUPPORTED_TARGET: Self = Self(1001);

    // ========================================================================
    // Preview feature errors (E1100-E1199)
    // ========================================================================
    pub const PREVIEW_FEATURE_REQUIRED: Self = Self(1100);

    // ========================================================================
    // Comptime errors (E1200-E1299)
    // ========================================================================
    pub const COMPTIME_EVALUATION_FAILED: Self = Self(1200);
    pub const COMPTIME_ARG_NOT_CONST: Self = Self(1201);

    // ========================================================================
    // Internal compiler errors (E9000-E9999)
    // ========================================================================
    pub const INTERNAL_ERROR: Self = Self(9000);
    pub const INTERNAL_CODEGEN_ERROR: Self = Self(9001);
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:04}", self.0)
    }
}

// ============================================================================
// Boxed Error Payloads
// ============================================================================
//
// Large error variants are boxed to reduce the size of ErrorKind.
// This keeps Result<T, CompileError> smaller on the stack.
// Errors are cold paths, so the extra indirection is acceptable.

/// Payload for `ErrorKind::MissingFields`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingFieldsError {
    pub struct_name: String,
    pub missing_fields: Vec<String>,
}

/// Payload for `ErrorKind::CopyStructNonCopyField`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyStructNonCopyFieldError {
    pub struct_name: String,
    pub field_name: String,
    pub field_type: String,
}

/// Payload for `ErrorKind::IntrinsicTypeMismatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrinsicTypeMismatchError {
    pub name: String,
    pub expected: String,
    pub found: String,
}

/// Payload for `ErrorKind::FieldWrongOrder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldWrongOrderError {
    pub struct_name: String,
    pub expected_field: String,
    pub found_field: String,
}

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
/// See ADR-0005 for the full design.
///
/// When all preview features are stabilized, this enum may be empty.
/// New preview features are added here as development begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PreviewFeature {
    /// Testing infrastructure feature - permanently unstable.
    /// Used to verify the preview feature gating mechanism works.
    TestInfra,
    /// Affine types and mutable value semantics.
    /// See ADR-0008 for the full design.
    AffineMvs,
    /// Module system with @import and pub visibility.
    /// See ADR-0026 for the full design.
    Modules,
    /// Random number intrinsics (@random_u32, @random_u64).
    /// See ADR-0027 for the full design.
    Random,
}

/// Error returned when parsing a preview feature name fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsePreviewFeatureError(String);

impl fmt::Display for ParsePreviewFeatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown preview feature '{}'", self.0)
    }
}

impl std::error::Error for ParsePreviewFeatureError {}

impl PreviewFeature {
    /// Get the CLI name for this feature (used with `--preview`).
    #[allow(unreachable_code)]
    pub fn name(&self) -> &'static str {
        match *self {
            PreviewFeature::TestInfra => "test_infra",
            PreviewFeature::AffineMvs => "affine_mvs",
            PreviewFeature::Modules => "modules",
            PreviewFeature::Random => "random",
        }
    }

    /// Get the ADR number documenting this feature.
    #[allow(unreachable_code)]
    pub fn adr(&self) -> &'static str {
        match *self {
            PreviewFeature::TestInfra => "ADR-0005",
            PreviewFeature::AffineMvs => "ADR-0008",
            PreviewFeature::Modules => "ADR-0026",
            PreviewFeature::Random => "ADR-0027",
        }
    }

    /// Get all available preview features.
    pub fn all() -> &'static [PreviewFeature] {
        &[
            PreviewFeature::TestInfra,
            PreviewFeature::AffineMvs,
            PreviewFeature::Modules,
            PreviewFeature::Random,
        ]
    }

    /// Get a comma-separated list of all feature names (for help text).
    pub fn all_names() -> String {
        if Self::all().is_empty() {
            "(none)".to_string()
        } else {
            Self::all()
                .iter()
                .map(|f| f.name())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

impl std::str::FromStr for PreviewFeature {
    type Err = ParsePreviewFeatureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "test_infra" => Ok(PreviewFeature::TestInfra),
            "affine_mvs" => Ok(PreviewFeature::AffineMvs),
            "modules" => Ok(PreviewFeature::Modules),
            "random" => Ok(PreviewFeature::Random),
            _ => Err(ParsePreviewFeatureError(s.to_string())),
        }
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

/// How confident we are that a suggested fix is correct.
///
/// This follows rustc's conventions for suggestion applicability levels.
/// IDEs and tools can use this to decide whether to auto-apply suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Applicability {
    /// The suggestion is definitely correct and can be safely auto-applied.
    ///
    /// Use this when the fix is guaranteed to compile and preserve semantics.
    MachineApplicable,

    /// The suggestion might be correct but should be reviewed by a human.
    ///
    /// Use this when the fix will likely work but may change behavior in
    /// edge cases, or when there are multiple equally valid options.
    MaybeIncorrect,

    /// The suggestion contains placeholders that the user must fill in.
    ///
    /// Use this when the fix shows the general shape but needs specific
    /// values like variable names or types.
    HasPlaceholders,

    /// The suggestion is just a hint and may not even compile.
    ///
    /// Use this for illustrative suggestions that show concepts rather
    /// than working code.
    Unspecified,
}

impl Default for Applicability {
    fn default() -> Self {
        Self::Unspecified
    }
}

impl std::fmt::Display for Applicability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Applicability::MachineApplicable => write!(f, "MachineApplicable"),
            Applicability::MaybeIncorrect => write!(f, "MaybeIncorrect"),
            Applicability::HasPlaceholders => write!(f, "HasPlaceholders"),
            Applicability::Unspecified => write!(f, "Unspecified"),
        }
    }
}

/// A suggested code fix that can be applied to resolve a diagnostic.
///
/// Suggestions provide machine-readable fix information that IDEs and
/// tools can use to offer quick-fix actions.
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// Human-readable description of what the suggestion does.
    pub message: String,
    /// The span of code to replace.
    pub span: Span,
    /// The replacement text.
    pub replacement: String,
    /// How confident we are that this fix is correct.
    pub applicability: Applicability,
}

impl Suggestion {
    /// Create a new suggestion with unspecified applicability.
    pub fn new(message: impl Into<String>, span: Span, replacement: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: replacement.into(),
            applicability: Applicability::Unspecified,
        }
    }

    /// Create a suggestion that is safe to auto-apply.
    pub fn machine_applicable(
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: replacement.into(),
            applicability: Applicability::MachineApplicable,
        }
    }

    /// Create a suggestion that may need human review.
    pub fn maybe_incorrect(
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: replacement.into(),
            applicability: Applicability::MaybeIncorrect,
        }
    }

    /// Create a suggestion with placeholders.
    pub fn with_placeholders(
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: replacement.into(),
            applicability: Applicability::HasPlaceholders,
        }
    }

    /// Set the applicability of this suggestion.
    pub fn with_applicability(mut self, applicability: Applicability) -> Self {
        self.applicability = applicability;
        self
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
    /// Code suggestions that can be applied to fix the issue.
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    /// Create an empty diagnostic.
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if this diagnostic has any content.
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
            && self.notes.is_empty()
            && self.helps.is_empty()
            && self.suggestions.is_empty()
    }
}

// ============================================================================
// Generic Diagnostic Wrapper
// ============================================================================

/// A compilation diagnostic (error or warning) with optional source location.
///
/// This is a generic wrapper that holds a diagnostic kind along with optional
/// source location and rich diagnostic information (labels, notes, helps).
///
/// Use the type aliases [`CompileError`] and [`CompileWarning`] for the
/// specific error and warning types.
///
/// Diagnostics can include rich information using the builder methods:
/// ```ignore
/// CompileError::new(ErrorKind::TypeMismatch { ... }, span)
///     .with_label("expected because of this", other_span)
///     .with_note("types must match exactly")
///     .with_help("consider adding a type conversion")
/// ```
#[derive(Debug, Clone)]
#[must_use = "compiler diagnostics should not be ignored"]
pub struct DiagnosticWrapper<K> {
    /// The specific kind of diagnostic.
    pub kind: K,
    span: Option<Span>,
    diagnostic: Diagnostic,
}

impl<K> DiagnosticWrapper<K> {
    /// Create a new diagnostic with the given kind and span.
    #[inline]
    pub fn new(kind: K, span: Span) -> Self {
        Self {
            kind,
            span: Some(span),
            diagnostic: Diagnostic::new(),
        }
    }

    /// Create a diagnostic without a source location.
    ///
    /// Use this for diagnostics that don't correspond to a specific source
    /// location, such as "no main function found" or linker errors.
    #[inline]
    pub fn without_span(kind: K) -> Self {
        Self {
            kind,
            span: None,
            diagnostic: Diagnostic::new(),
        }
    }

    /// Returns true if this diagnostic has source location information.
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

    /// Add a code suggestion that can be applied to fix the issue.
    ///
    /// Suggestions provide machine-readable fix information for IDEs and tools.
    #[inline]
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.diagnostic.suggestions.push(suggestion);
        self
    }
}

impl<K: fmt::Display> fmt::Display for DiagnosticWrapper<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl<K: fmt::Display + fmt::Debug> std::error::Error for DiagnosticWrapper<K> {}

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
pub type CompileError = DiagnosticWrapper<ErrorKind>;

// Helper functions for complex error formatting in thiserror attributes

fn format_argument_count(expected: usize, found: usize) -> String {
    if expected == 1 {
        format!("expected {} argument, found {}", expected, found)
    } else {
        format!("expected {} arguments, found {}", expected, found)
    }
}

fn format_missing_fields(err: &MissingFieldsError) -> String {
    if err.missing_fields.len() == 1 {
        format!(
            "missing field '{}' in struct '{}'",
            err.missing_fields[0], err.struct_name
        )
    } else {
        let fields = err
            .missing_fields
            .iter()
            .map(|f| format!("'{}'", f))
            .collect::<Vec<_>>()
            .join(", ");
        format!("missing fields {} in struct '{}'", fields, err.struct_name)
    }
}

fn format_intrinsic_arg_count(name: &str, expected: usize, found: usize) -> String {
    if expected == 1 {
        format!(
            "intrinsic '@{}' expects {} argument, found {}",
            name, expected, found
        )
    } else {
        format!(
            "intrinsic '@{}' expects {} arguments, found {}",
            name, expected, found
        )
    }
}

fn format_array_length_mismatch(expected: u64, found: u64) -> String {
    if expected == 1 {
        format!(
            "expected array of {} element, found {} elements",
            expected, found
        )
    } else {
        format!(
            "expected array of {} elements, found {} elements",
            expected, found
        )
    }
}

/// The kind of compilation error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ErrorKind {
    // Lexer errors
    #[error("unexpected character: {0}")]
    UnexpectedCharacter(char),
    #[error("invalid integer literal")]
    InvalidInteger,
    #[error("invalid escape sequence: \\{0}")]
    InvalidStringEscape(char),
    #[error("unterminated string literal")]
    UnterminatedString,

    // Parser errors
    #[error("expected {expected}, found {found}")]
    UnexpectedToken {
        expected: Cow<'static, str>,
        found: Cow<'static, str>,
    },
    #[error("unexpected end of file, expected {expected}")]
    UnexpectedEof { expected: Cow<'static, str> },
    /// A custom parse error with a specific message.
    ///
    /// Used for parser-generated errors that don't fit the "expected X, found Y" pattern.
    #[error("{0}")]
    ParseError(String),

    // Semantic errors
    #[error("no main function found")]
    NoMainFunction,
    #[error("undefined variable '{0}'")]
    UndefinedVariable(String),
    #[error("undefined function '{0}'")]
    UndefinedFunction(String),
    #[error("cannot assign to immutable variable '{0}'")]
    AssignToImmutable(String),
    #[error("unknown type '{0}'")]
    UnknownType(String),
    /// Use of a value after it has been moved.
    #[error("use of moved value '{0}'")]
    UseAfterMove(String),
    #[error("type mismatch: expected {expected}, found {found}")]
    TypeMismatch { expected: String, found: String },
    #[error("{}", format_argument_count(*.expected, *.found))]
    WrongArgumentCount { expected: usize, found: usize },

    // Struct errors
    #[error("{}", format_missing_fields(.0))]
    MissingFields(Box<MissingFieldsError>),
    #[error("unknown field '{field_name}' in struct '{struct_name}'")]
    UnknownField {
        struct_name: String,
        field_name: String,
    },
    #[error("duplicate field '{field_name}' in struct '{struct_name}'")]
    DuplicateField {
        struct_name: String,
        field_name: String,
    },
    /// Anonymous struct with no fields is not allowed
    #[error("empty struct is not allowed")]
    EmptyStruct,
    /// @copy struct contains a field with non-Copy type
    #[error("@copy struct '{struct_name}' has field '{field_name}' with non-Copy type '{field_type}'", struct_name = .0.struct_name, field_name = .0.field_name, field_type = .0.field_type)]
    CopyStructNonCopyField(Box<CopyStructNonCopyFieldError>),
    /// User-defined type collides with a built-in type name
    #[error("cannot define type `{type_name}`: name is reserved for built-in type")]
    ReservedTypeName { type_name: String },
    /// Duplicate type definition
    #[error("duplicate type definition: `{type_name}` is already defined")]
    DuplicateTypeDefinition { type_name: String },
    /// Linear value was not consumed before going out of scope
    #[error("linear value '{0}' must be consumed but was dropped")]
    LinearValueNotConsumed(String),
    /// Linear struct cannot be marked @copy
    #[error("linear struct '{0}' cannot be marked @copy")]
    LinearStructCopy(String),
    /// @handle struct missing required .handle() method
    #[error("struct '{struct_name}' is marked @handle but has no `handle` method")]
    HandleStructMissingMethod { struct_name: String },
    /// @handle struct's .handle() method has wrong signature
    #[error(
        "struct '{struct_name}' has `handle` method with wrong signature: expected `fn handle(self: {struct_name}) -> {struct_name}`, found `{found_signature}`"
    )]
    HandleMethodWrongSignature {
        struct_name: String,
        found_signature: String,
    },
    /// Duplicate method definition in impl blocks for the same type
    #[error("duplicate method '{method_name}' for type '{type_name}'")]
    DuplicateMethod {
        type_name: String,
        method_name: String,
    },
    /// Method not found on a type
    #[error("no method named '{method_name}' found for type '{type_name}'")]
    UndefinedMethod {
        type_name: String,
        method_name: String,
    },
    /// Associated function not found on a type
    #[error("no associated function named '{function_name}' found for type '{type_name}'")]
    UndefinedAssocFn {
        type_name: String,
        function_name: String,
    },
    /// Method call on non-struct type
    #[error("no method named '{method_name}' on type '{found}'")]
    MethodCallOnNonStruct { found: String, method_name: String },
    /// Calling a method (with self) as an associated function
    #[error(
        "'{type_name}::{method_name}' is a method, not an associated function; use receiver.{method_name}() syntax"
    )]
    MethodCalledAsAssocFn {
        type_name: String,
        method_name: String,
    },
    /// Calling an associated function (without self) as a method
    #[error(
        "'{function_name}' is an associated function, not a method; use {type_name}::{function_name}() syntax"
    )]
    AssocFnCalledAsMethod {
        type_name: String,
        function_name: String,
    },

    // Destructor errors
    /// Duplicate destructor for the same type
    #[error("duplicate destructor for type '{type_name}'")]
    DuplicateDestructor { type_name: String },
    /// Destructor for unknown type
    #[error("unknown type '{type_name}' in destructor")]
    DestructorUnknownType { type_name: String },

    // Constant errors
    /// Duplicate constant declaration
    #[error("duplicate {kind} '{name}'")]
    DuplicateConstant { name: String, kind: String },

    // Enum errors
    #[error("duplicate variant '{variant_name}' in enum '{enum_name}'")]
    DuplicateVariant {
        enum_name: String,
        variant_name: String,
    },
    #[error("unknown variant '{variant_name}' in enum '{enum_name}'")]
    UnknownVariant {
        enum_name: String,
        variant_name: String,
    },
    #[error("unknown enum type '{0}'")]
    UnknownEnumType(String),
    #[error("struct '{struct_name}' fields must be initialized in declaration order: expected '{expected_field}', found '{found_field}'", struct_name = .0.struct_name, expected_field = .0.expected_field, found_field = .0.found_field)]
    FieldWrongOrder(Box<FieldWrongOrderError>),
    #[error("field access on non-struct type '{found}'")]
    FieldAccessOnNonStruct { found: String },
    #[error("invalid assignment target")]
    InvalidAssignmentTarget,
    /// Inout argument is not an lvalue (variable, field, or array element)
    #[error("inout argument must be an lvalue (variable, field, or array element)")]
    InoutNonLvalue,
    /// Same variable passed to multiple inout parameters in a single call
    #[error("cannot pass same variable '{variable}' to multiple inout parameters")]
    InoutExclusiveAccess { variable: String },
    /// Borrow argument is not an lvalue (variable, field, or array element)
    #[error("borrow argument must be a variable, field, or array element")]
    BorrowNonLvalue,
    /// Cannot mutate a borrowed value
    #[error("cannot mutate borrowed value '{variable}'")]
    MutateBorrowedValue { variable: String },
    /// Cannot move out of a borrowed value
    #[error("cannot move out of borrowed value '{variable}'")]
    MoveOutOfBorrow { variable: String },
    /// Same variable passed to both borrow and inout parameters (law of exclusivity)
    #[error("cannot borrow '{variable}' while it is mutably borrowed (inout)")]
    BorrowInoutConflict { variable: String },
    /// Argument to inout parameter is missing `inout` keyword at call site
    #[error("argument to inout parameter must use 'inout' keyword")]
    InoutKeywordMissing,
    /// Argument to borrow parameter is missing `borrow` keyword at call site
    #[error("argument to borrow parameter must use 'borrow' keyword")]
    BorrowKeywordMissing,

    // Control flow errors
    #[error("'break' outside of loop")]
    BreakOutsideLoop,
    #[error("'continue' outside of loop")]
    ContinueOutsideLoop,

    // Match errors
    #[error("match is not exhaustive")]
    NonExhaustiveMatch,
    #[error("match expression has no arms")]
    EmptyMatch,
    #[error("cannot match on type '{0}', expected integer, bool, or enum")]
    InvalidMatchType(String),

    // Intrinsic errors
    #[error("unknown intrinsic '@{0}'")]
    UnknownIntrinsic(String),
    #[error("{}", format_intrinsic_arg_count(name, *.expected, *.found))]
    IntrinsicWrongArgCount {
        name: String,
        expected: usize,
        found: usize,
    },
    #[error("intrinsic '@{name}' expects {expected}, found {found}", name = .0.name, expected = .0.expected, found = .0.found)]
    IntrinsicTypeMismatch(Box<IntrinsicTypeMismatchError>),

    // Module errors
    #[error("@import requires a string literal argument")]
    ImportRequiresStringLiteral,
    #[error("cannot find module '{path}'")]
    ModuleNotFound {
        path: String,
        /// Candidates that were tried (for error message)
        candidates: Vec<String>,
    },

    // Literal errors
    #[error("literal value {value} is out of range for type '{ty}'")]
    LiteralOutOfRange { value: u64, ty: String },

    // Operator errors
    #[error("cannot apply unary operator `-` to type '{0}'")]
    CannotNegateUnsigned(String),
    #[error("comparison operators cannot be chained")]
    ChainedComparison,

    // Array errors
    #[error("cannot index into non-array type '{found}'")]
    IndexOnNonArray { found: String },
    #[error("{}", format_array_length_mismatch(*.expected, *.found))]
    ArrayLengthMismatch { expected: u64, found: u64 },
    #[error("index out of bounds: the length is {length} but the index is {index}")]
    IndexOutOfBounds { index: i64, length: u64 },
    #[error("type annotation required for empty array")]
    TypeAnnotationRequired,
    /// Cannot move non-Copy element out of array index position
    #[error("cannot move out of indexed position: element type '{element_type}' is not Copy")]
    MoveOutOfIndex { element_type: String },

    // Linker errors
    #[error("link error: {0}")]
    LinkError(String),

    // Target errors
    #[error("unsupported target: {0}")]
    UnsupportedTarget(String),

    // Preview feature errors
    #[error("{what} requires preview feature `{}`", .feature.name())]
    PreviewFeatureRequired {
        feature: PreviewFeature,
        what: String,
    },

    // Comptime errors
    #[error("comptime evaluation failed: {reason}")]
    ComptimeEvaluationFailed { reason: String },

    #[error("comptime parameter requires a compile-time known value")]
    ComptimeArgNotConst { param_name: String },

    // Internal compiler errors (bugs in the compiler itself)
    #[error("internal compiler error: {0}")]
    InternalError(String),

    // Codegen internal errors (compiler bugs)
    #[error("internal codegen error: {0}")]
    InternalCodegenError(String),
}

impl ErrorKind {
    /// Get the error code for this error kind.
    ///
    /// Every error kind has a unique, stable error code that can be used
    /// for documentation lookup and searchability.
    pub fn code(&self) -> ErrorCode {
        match self {
            // Lexer errors (E0001-E0099)
            ErrorKind::UnexpectedCharacter(_) => ErrorCode::UNEXPECTED_CHARACTER,
            ErrorKind::InvalidInteger => ErrorCode::INVALID_INTEGER,
            ErrorKind::InvalidStringEscape(_) => ErrorCode::INVALID_STRING_ESCAPE,
            ErrorKind::UnterminatedString => ErrorCode::UNTERMINATED_STRING,

            // Parser errors (E0100-E0199)
            ErrorKind::UnexpectedToken { .. } => ErrorCode::UNEXPECTED_TOKEN,
            ErrorKind::UnexpectedEof { .. } => ErrorCode::UNEXPECTED_EOF,
            ErrorKind::ParseError(_) => ErrorCode::PARSE_ERROR,

            // Semantic errors (E0200-E0399)
            ErrorKind::NoMainFunction => ErrorCode::NO_MAIN_FUNCTION,
            ErrorKind::UndefinedVariable(_) => ErrorCode::UNDEFINED_VARIABLE,
            ErrorKind::UndefinedFunction(_) => ErrorCode::UNDEFINED_FUNCTION,
            ErrorKind::AssignToImmutable(_) => ErrorCode::ASSIGN_TO_IMMUTABLE,
            ErrorKind::UnknownType(_) => ErrorCode::UNKNOWN_TYPE,
            ErrorKind::UseAfterMove(_) => ErrorCode::USE_AFTER_MOVE,
            ErrorKind::TypeMismatch { .. } => ErrorCode::TYPE_MISMATCH,
            ErrorKind::WrongArgumentCount { .. } => ErrorCode::WRONG_ARGUMENT_COUNT,

            // Struct/enum errors (E0400-E0499)
            ErrorKind::MissingFields(_) => ErrorCode::MISSING_FIELDS,
            ErrorKind::UnknownField { .. } => ErrorCode::UNKNOWN_FIELD,
            ErrorKind::DuplicateField { .. } => ErrorCode::DUPLICATE_FIELD,
            ErrorKind::EmptyStruct => ErrorCode::EMPTY_STRUCT,
            ErrorKind::CopyStructNonCopyField(_) => ErrorCode::COPY_STRUCT_NON_COPY_FIELD,
            ErrorKind::ReservedTypeName { .. } => ErrorCode::RESERVED_TYPE_NAME,
            ErrorKind::DuplicateTypeDefinition { .. } => ErrorCode::DUPLICATE_TYPE_DEFINITION,
            ErrorKind::LinearValueNotConsumed(_) => ErrorCode::LINEAR_VALUE_NOT_CONSUMED,
            ErrorKind::LinearStructCopy(_) => ErrorCode::LINEAR_STRUCT_COPY,
            ErrorKind::HandleStructMissingMethod { .. } => ErrorCode::HANDLE_STRUCT_MISSING_METHOD,
            ErrorKind::HandleMethodWrongSignature { .. } => {
                ErrorCode::HANDLE_METHOD_WRONG_SIGNATURE
            }
            ErrorKind::DuplicateMethod { .. } => ErrorCode::DUPLICATE_METHOD,
            ErrorKind::UndefinedMethod { .. } => ErrorCode::UNDEFINED_METHOD,
            ErrorKind::UndefinedAssocFn { .. } => ErrorCode::UNDEFINED_ASSOC_FN,
            ErrorKind::MethodCallOnNonStruct { .. } => ErrorCode::METHOD_CALL_ON_NON_STRUCT,
            ErrorKind::MethodCalledAsAssocFn { .. } => ErrorCode::METHOD_CALLED_AS_ASSOC_FN,
            ErrorKind::AssocFnCalledAsMethod { .. } => ErrorCode::ASSOC_FN_CALLED_AS_METHOD,
            ErrorKind::DuplicateDestructor { .. } => ErrorCode::DUPLICATE_DESTRUCTOR,
            ErrorKind::DestructorUnknownType { .. } => ErrorCode::DESTRUCTOR_UNKNOWN_TYPE,
            ErrorKind::DuplicateConstant { .. } => ErrorCode::DUPLICATE_CONSTANT,
            ErrorKind::DuplicateVariant { .. } => ErrorCode::DUPLICATE_VARIANT,
            ErrorKind::UnknownVariant { .. } => ErrorCode::UNKNOWN_VARIANT,
            ErrorKind::UnknownEnumType(_) => ErrorCode::UNKNOWN_ENUM_TYPE,
            ErrorKind::FieldWrongOrder(_) => ErrorCode::FIELD_WRONG_ORDER,
            ErrorKind::FieldAccessOnNonStruct { .. } => ErrorCode::FIELD_ACCESS_ON_NON_STRUCT,
            ErrorKind::InvalidAssignmentTarget => ErrorCode::INVALID_ASSIGNMENT_TARGET,
            ErrorKind::InoutNonLvalue => ErrorCode::INOUT_NON_LVALUE,
            ErrorKind::InoutExclusiveAccess { .. } => ErrorCode::INOUT_EXCLUSIVE_ACCESS,
            ErrorKind::BorrowNonLvalue => ErrorCode::BORROW_NON_LVALUE,
            ErrorKind::MutateBorrowedValue { .. } => ErrorCode::MUTATE_BORROWED_VALUE,
            ErrorKind::MoveOutOfBorrow { .. } => ErrorCode::MOVE_OUT_OF_BORROW,
            ErrorKind::BorrowInoutConflict { .. } => ErrorCode::BORROW_INOUT_CONFLICT,
            ErrorKind::InoutKeywordMissing => ErrorCode::INOUT_KEYWORD_MISSING,
            ErrorKind::BorrowKeywordMissing => ErrorCode::BORROW_KEYWORD_MISSING,

            // Control flow errors (E0500-E0599)
            ErrorKind::BreakOutsideLoop => ErrorCode::BREAK_OUTSIDE_LOOP,
            ErrorKind::ContinueOutsideLoop => ErrorCode::CONTINUE_OUTSIDE_LOOP,

            // Match errors (E0600-E0699)
            ErrorKind::NonExhaustiveMatch => ErrorCode::NON_EXHAUSTIVE_MATCH,
            ErrorKind::EmptyMatch => ErrorCode::EMPTY_MATCH,
            ErrorKind::InvalidMatchType(_) => ErrorCode::INVALID_MATCH_TYPE,

            // Intrinsic errors (E0700-E0799)
            ErrorKind::UnknownIntrinsic(_) => ErrorCode::UNKNOWN_INTRINSIC,
            ErrorKind::IntrinsicWrongArgCount { .. } => ErrorCode::INTRINSIC_WRONG_ARG_COUNT,
            ErrorKind::IntrinsicTypeMismatch(_) => ErrorCode::INTRINSIC_TYPE_MISMATCH,
            ErrorKind::ImportRequiresStringLiteral => ErrorCode::IMPORT_REQUIRES_STRING_LITERAL,
            ErrorKind::ModuleNotFound { .. } => ErrorCode::MODULE_NOT_FOUND,

            // Literal/operator errors (E0800-E0899)
            ErrorKind::LiteralOutOfRange { .. } => ErrorCode::LITERAL_OUT_OF_RANGE,
            ErrorKind::CannotNegateUnsigned(_) => ErrorCode::CANNOT_NEGATE_UNSIGNED,
            ErrorKind::ChainedComparison => ErrorCode::CHAINED_COMPARISON,

            // Array errors (E0900-E0999)
            ErrorKind::IndexOnNonArray { .. } => ErrorCode::INDEX_ON_NON_ARRAY,
            ErrorKind::ArrayLengthMismatch { .. } => ErrorCode::ARRAY_LENGTH_MISMATCH,
            ErrorKind::IndexOutOfBounds { .. } => ErrorCode::INDEX_OUT_OF_BOUNDS,
            ErrorKind::TypeAnnotationRequired => ErrorCode::TYPE_ANNOTATION_REQUIRED,
            ErrorKind::MoveOutOfIndex { .. } => ErrorCode::MOVE_OUT_OF_INDEX,

            // Linker/target errors (E1000-E1099)
            ErrorKind::LinkError(_) => ErrorCode::LINK_ERROR,
            ErrorKind::UnsupportedTarget(_) => ErrorCode::UNSUPPORTED_TARGET,

            // Preview feature errors (E1100-E1199)
            ErrorKind::PreviewFeatureRequired { .. } => ErrorCode::PREVIEW_FEATURE_REQUIRED,

            // Comptime errors (E1200-E1299)
            ErrorKind::ComptimeEvaluationFailed { .. } => ErrorCode::COMPTIME_EVALUATION_FAILED,
            ErrorKind::ComptimeArgNotConst { .. } => ErrorCode::COMPTIME_ARG_NOT_CONST,

            // Internal compiler errors (E9000-E9999)
            ErrorKind::InternalError(_) => ErrorCode::INTERNAL_ERROR,
            ErrorKind::InternalCodegenError(_) => ErrorCode::INTERNAL_CODEGEN_ERROR,
        }
    }
}

impl CompileError {
    /// Create an error at a specific position (zero-length span).
    #[inline]
    pub fn at(kind: ErrorKind, pos: u32) -> Self {
        Self {
            kind,
            span: Some(Span::point(pos)),
            diagnostic: Diagnostic::new(),
        }
    }
}

/// Result type for compilation operations.
pub type CompileResult<T> = Result<T, CompileError>;

// ============================================================================
// Multiple Error Collection
// ============================================================================

/// A collection of compilation errors.
///
/// This type supports collecting multiple errors during compilation to provide
/// users with more comprehensive diagnostics. Instead of stopping at the first
/// error, the compiler can continue and report multiple issues at once.
///
/// # Usage
///
/// Use `CompileErrors` when a compilation phase can detect multiple independent
/// errors. For example, semantic analysis can report multiple type errors in
/// different functions.
///
/// ```ignore
/// let mut errors = CompileErrors::new();
/// errors.push(CompileError::new(ErrorKind::TypeMismatch { ... }, span1));
/// errors.push(CompileError::new(ErrorKind::UndefinedVariable("x".into()), span2));
///
/// if !errors.is_empty() {
///     return Err(errors);
/// }
/// ```
///
/// # Error Semantics
///
/// - An empty `CompileErrors` represents no errors (not a failure)
/// - A non-empty `CompileErrors` represents one or more compilation failures
/// - When converted to a single `CompileError`, the first error is used
#[derive(Debug, Clone)]
pub struct CompileErrors {
    errors: Vec<CompileError>,
}

impl CompileErrors {
    /// Create a new empty error collection.
    pub fn new() -> Self {
        Self { errors: Vec::new() }
    }

    /// Create an error collection from a single error.
    pub fn from_error(error: CompileError) -> Self {
        Self {
            errors: vec![error],
        }
    }

    /// Add an error to the collection.
    pub fn push(&mut self, error: CompileError) {
        self.errors.push(error);
    }

    /// Extend this collection with errors from another collection.
    pub fn extend(&mut self, other: CompileErrors) {
        self.errors.extend(other.errors);
    }

    /// Returns true if there are no errors.
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Returns the number of errors.
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// Get the first error, if any.
    pub fn first(&self) -> Option<&CompileError> {
        self.errors.first()
    }

    /// Iterate over all errors.
    pub fn iter(&self) -> impl Iterator<Item = &CompileError> {
        self.errors.iter()
    }

    /// Convert into an iterator over errors.
    pub fn into_iter(self) -> impl Iterator<Item = CompileError> {
        self.errors.into_iter()
    }

    /// Get all errors as a slice.
    pub fn as_slice(&self) -> &[CompileError] {
        &self.errors
    }

    /// Check if the collection contains errors and return as a result.
    ///
    /// Returns `Ok(())` if empty, or `Err(self)` if there are errors.
    pub fn into_result(self) -> Result<(), CompileErrors> {
        if self.is_empty() { Ok(()) } else { Err(self) }
    }

    /// Fail with these errors if non-empty, otherwise return the value.
    ///
    /// This is useful for combining error checking with a result:
    /// ```ignore
    /// let output = SemaOutput { ... };
    /// errors.into_result_with(output)
    /// ```
    pub fn into_result_with<T>(self, value: T) -> Result<T, CompileErrors> {
        if self.is_empty() {
            Ok(value)
        } else {
            Err(self)
        }
    }
}

impl Default for CompileErrors {
    fn default() -> Self {
        Self::new()
    }
}

impl From<CompileError> for CompileErrors {
    fn from(error: CompileError) -> Self {
        Self::from_error(error)
    }
}

impl From<Vec<CompileError>> for CompileErrors {
    fn from(errors: Vec<CompileError>) -> Self {
        Self { errors }
    }
}

impl From<CompileErrors> for CompileError {
    /// Convert a collection to a single error.
    ///
    /// Uses the first error in the collection. If the collection is empty,
    /// returns an internal error (this indicates a compiler bug).
    fn from(errors: CompileErrors) -> Self {
        debug_assert!(
            !errors.is_empty(),
            "converting empty CompileErrors to CompileError"
        );
        errors.errors.into_iter().next().unwrap_or_else(|| {
            CompileError::without_span(ErrorKind::InternalError(
                "empty error collection converted to single error".into(),
            ))
        })
    }
}

impl fmt::Display for CompileErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.errors.len() {
            0 => write!(f, "no errors"),
            1 => write!(f, "{}", self.errors[0]),
            n => write!(
                f,
                "{} (and {} more error{})",
                self.errors[0],
                n - 1,
                if n == 2 { "" } else { "s" }
            ),
        }
    }
}

impl std::error::Error for CompileErrors {}

/// Result type for operations that can produce multiple errors.
pub type MultiErrorResult<T> = Result<T, CompileErrors>;

// ============================================================================
// Error Helper Traits
// ============================================================================

/// Extension trait for converting `Option<T>` to `CompileResult<T>`.
///
/// This trait simplifies the common pattern of converting lookup failures
/// (returning `None`) into compilation errors with source spans.
///
/// # Example
/// ```ignore
/// use rue_error::{OptionExt, ErrorKind};
///
/// let result = ctx.locals.get(name)
///     .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;
/// ```
pub trait OptionExt<T> {
    /// Convert `None` to a `CompileError` with the given kind and span.
    fn ok_or_compile_error(self, kind: ErrorKind, span: Span) -> CompileResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    #[inline]
    fn ok_or_compile_error(self, kind: ErrorKind, span: Span) -> CompileResult<T> {
        self.ok_or_else(|| CompileError::new(kind, span))
    }
}

/// The kind of compilation warning.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WarningKind {
    /// A variable was declared but never used.
    #[error("unused variable '{0}'")]
    UnusedVariable(String),
    /// A function was declared but never called.
    #[error("unused function '{0}'")]
    UnusedFunction(String),
    /// Code that will never be executed.
    #[error("unreachable code")]
    UnreachableCode,
    /// A pattern that will never be matched because a previous pattern already covers it.
    #[error("unreachable pattern '{0}'")]
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
pub type CompileWarning = DiagnosticWrapper<WarningKind>;

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
            expected: Cow::Borrowed("identifier"),
            found: Cow::Borrowed("'+'"),
        });
        assert_eq!(error.to_string(), "expected identifier, found '+'");
    }

    #[test]
    fn test_unexpected_eof_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedEof {
            expected: Cow::Borrowed("'}'"),
        });
        assert_eq!(error.to_string(), "unexpected end of file, expected '}'");
    }

    #[test]
    fn test_parse_error_message() {
        let error =
            CompileError::without_span(ErrorKind::ParseError("custom parse error".to_string()));
        assert_eq!(error.to_string(), "custom parse error");
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
        assert!(diag.suggestions.is_empty());
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
    fn test_diagnostic_not_empty_with_suggestion() {
        let mut diag = Diagnostic::new();
        diag.suggestions
            .push(Suggestion::new("try this", Span::new(0, 10), "replacement"));
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
    fn test_suggestion_creation() {
        let span = Span::new(10, 20);
        let suggestion = Suggestion::new("try this fix", span, "new_code");
        assert_eq!(suggestion.message, "try this fix");
        assert_eq!(suggestion.span, span);
        assert_eq!(suggestion.replacement, "new_code");
        assert_eq!(suggestion.applicability, Applicability::Unspecified);
    }

    #[test]
    fn test_suggestion_machine_applicable() {
        let span = Span::new(0, 5);
        let suggestion = Suggestion::machine_applicable("rename variable", span, "new_name");
        assert_eq!(suggestion.applicability, Applicability::MachineApplicable);
    }

    #[test]
    fn test_suggestion_maybe_incorrect() {
        let span = Span::new(0, 5);
        let suggestion = Suggestion::maybe_incorrect("try adding mut", span, "mut x");
        assert_eq!(suggestion.applicability, Applicability::MaybeIncorrect);
    }

    #[test]
    fn test_suggestion_with_placeholders() {
        let span = Span::new(0, 5);
        let suggestion = Suggestion::with_placeholders("add type annotation", span, ": <type>");
        assert_eq!(suggestion.applicability, Applicability::HasPlaceholders);
    }

    #[test]
    fn test_suggestion_with_applicability() {
        let span = Span::new(0, 5);
        let suggestion = Suggestion::new("fix", span, "new_code")
            .with_applicability(Applicability::MachineApplicable);
        assert_eq!(suggestion.applicability, Applicability::MachineApplicable);
    }

    #[test]
    fn test_applicability_display() {
        assert_eq!(
            Applicability::MachineApplicable.to_string(),
            "MachineApplicable"
        );
        assert_eq!(Applicability::MaybeIncorrect.to_string(), "MaybeIncorrect");
        assert_eq!(
            Applicability::HasPlaceholders.to_string(),
            "HasPlaceholders"
        );
        assert_eq!(Applicability::Unspecified.to_string(), "Unspecified");
    }

    #[test]
    fn test_applicability_default() {
        assert_eq!(Applicability::default(), Applicability::Unspecified);
    }

    #[test]
    fn test_error_with_suggestion() {
        let span = Span::new(10, 20);
        let error =
            CompileError::new(ErrorKind::AssignToImmutable("x".to_string()), span).with_suggestion(
                Suggestion::machine_applicable("add mut", Span::new(4, 5), "mut x"),
            );

        let diag = error.diagnostic();
        assert_eq!(diag.suggestions.len(), 1);
        assert_eq!(diag.suggestions[0].message, "add mut");
        assert_eq!(diag.suggestions[0].replacement, "mut x");
        assert_eq!(
            diag.suggestions[0].applicability,
            Applicability::MachineApplicable
        );
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
    fn test_preview_feature_test_infra() {
        let feature: PreviewFeature = "test_infra".parse().unwrap();
        assert_eq!(feature, PreviewFeature::TestInfra);
        assert_eq!(feature.name(), "test_infra");
        assert_eq!(feature.adr(), "ADR-0005");
    }

    #[test]
    fn test_preview_feature_from_str_unknown() {
        assert!("unknown".parse::<PreviewFeature>().is_err());
        assert!("".parse::<PreviewFeature>().is_err());
    }

    #[test]
    fn test_parse_preview_feature_error_display() {
        let err = "bad_feature".parse::<PreviewFeature>().unwrap_err();
        assert_eq!(err.to_string(), "unknown preview feature 'bad_feature'");
    }

    #[test]
    fn test_preview_feature_all_contains_test_infra() {
        let all = PreviewFeature::all();
        assert!(all.contains(&PreviewFeature::TestInfra));
    }

    #[test]
    fn test_preview_feature_all_names() {
        let names = PreviewFeature::all_names();
        assert_eq!(names, "test_infra, affine_mvs, modules, random");
    }

    // ========================================================================
    // OptionExt trait tests
    // ========================================================================

    #[test]
    fn test_option_ext_some() {
        let span = Span::new(10, 20);
        let result: CompileResult<i32> =
            Some(42).ok_or_compile_error(ErrorKind::InvalidInteger, span);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_option_ext_none() {
        let span = Span::new(10, 20);
        let result: CompileResult<i32> = None.ok_or_compile_error(ErrorKind::InvalidInteger, span);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.span(), Some(span));
        assert!(matches!(error.kind, ErrorKind::InvalidInteger));
    }

    #[test]
    fn test_option_ext_with_complex_error() {
        let span = Span::new(5, 15);
        let result: CompileResult<String> =
            None.ok_or_compile_error(ErrorKind::UndefinedVariable("foo".to_string()), span);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.to_string(), "undefined variable 'foo'");
    }

    // ========================================================================
    // CompileErrors tests
    // ========================================================================

    #[test]
    fn test_compile_errors_new_is_empty() {
        let errors = CompileErrors::new();
        assert!(errors.is_empty());
        assert_eq!(errors.len(), 0);
    }

    #[test]
    fn test_compile_errors_from_error() {
        let error = CompileError::without_span(ErrorKind::InvalidInteger);
        let errors = CompileErrors::from_error(error);
        assert!(!errors.is_empty());
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_compile_errors_push() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn test_compile_errors_extend() {
        let mut errors1 = CompileErrors::new();
        errors1.push(CompileError::without_span(ErrorKind::InvalidInteger));

        let mut errors2 = CompileErrors::new();
        errors2.push(CompileError::without_span(ErrorKind::NoMainFunction));
        errors2.push(CompileError::without_span(ErrorKind::BreakOutsideLoop));

        errors1.extend(errors2);
        assert_eq!(errors1.len(), 3);
    }

    #[test]
    fn test_compile_errors_first() {
        let mut errors = CompileErrors::new();
        assert!(errors.first().is_none());

        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));

        let first = errors.first().unwrap();
        assert!(matches!(first.kind, ErrorKind::InvalidInteger));
    }

    #[test]
    fn test_compile_errors_iter() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));

        let kinds: Vec<_> = errors.iter().map(|e| &e.kind).collect();
        assert_eq!(kinds.len(), 2);
    }

    #[test]
    fn test_compile_errors_into_result_empty() {
        let errors = CompileErrors::new();
        assert!(errors.into_result().is_ok());
    }

    #[test]
    fn test_compile_errors_into_result_non_empty() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        assert!(errors.into_result().is_err());
    }

    #[test]
    fn test_compile_errors_into_result_with() {
        let errors = CompileErrors::new();
        let result = errors.into_result_with(42);
        assert_eq!(result.unwrap(), 42);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        let result = errors.into_result_with(42);
        assert!(result.is_err());
    }

    #[test]
    fn test_compile_errors_from_single_error() {
        let error = CompileError::without_span(ErrorKind::InvalidInteger);
        let errors: CompileErrors = error.into();
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_compile_errors_to_single_error() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));

        let error: CompileError = errors.into();
        // Should get the first error
        assert!(matches!(error.kind, ErrorKind::InvalidInteger));
    }

    /// Test that empty CompileErrors conversion doesn't panic in release builds.
    /// In debug builds, this triggers a debug_assert panic (as expected).
    /// This test verifies the graceful fallback behavior in release mode.
    #[test]
    #[cfg_attr(debug_assertions, ignore)]
    fn test_empty_compile_errors_to_single_error() {
        // Converting an empty CompileErrors should not panic in release;
        // instead it should return an InternalError.
        let empty = CompileErrors::new();
        let error: CompileError = empty.into();

        // Should get an InternalError with a descriptive message
        match &error.kind {
            ErrorKind::InternalError(msg) => {
                assert!(msg.contains("empty error collection"));
            }
            other => panic!("expected InternalError, got {:?}", other),
        }
    }

    #[test]
    fn test_compile_errors_display_empty() {
        let errors = CompileErrors::new();
        assert_eq!(errors.to_string(), "no errors");
    }

    #[test]
    fn test_compile_errors_display_single() {
        let errors =
            CompileErrors::from_error(CompileError::without_span(ErrorKind::InvalidInteger));
        assert_eq!(errors.to_string(), "invalid integer literal");
    }

    #[test]
    fn test_compile_errors_display_multiple() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));
        assert_eq!(
            errors.to_string(),
            "invalid integer literal (and 1 more error)"
        );

        errors.push(CompileError::without_span(ErrorKind::BreakOutsideLoop));
        assert_eq!(
            errors.to_string(),
            "invalid integer literal (and 2 more errors)"
        );
    }

    // ========================================================================
    // Error code tests
    // ========================================================================

    #[test]
    fn test_error_code_display() {
        assert_eq!(ErrorCode::TYPE_MISMATCH.to_string(), "E0206");
        assert_eq!(ErrorCode::UNDEFINED_VARIABLE.to_string(), "E0201");
        assert_eq!(ErrorCode::INTERNAL_ERROR.to_string(), "E9000");
        assert_eq!(ErrorCode(1).to_string(), "E0001");
        assert_eq!(ErrorCode(42).to_string(), "E0042");
        assert_eq!(ErrorCode(1234).to_string(), "E1234");
    }

    #[test]
    fn test_error_kind_code_lexer() {
        assert_eq!(
            ErrorKind::UnexpectedCharacter('@').code(),
            ErrorCode::UNEXPECTED_CHARACTER
        );
        assert_eq!(ErrorKind::InvalidInteger.code(), ErrorCode::INVALID_INTEGER);
        assert_eq!(
            ErrorKind::InvalidStringEscape('n').code(),
            ErrorCode::INVALID_STRING_ESCAPE
        );
        assert_eq!(
            ErrorKind::UnterminatedString.code(),
            ErrorCode::UNTERMINATED_STRING
        );
    }

    #[test]
    fn test_error_kind_code_parser() {
        assert_eq!(
            ErrorKind::UnexpectedToken {
                expected: "identifier".into(),
                found: "+".into()
            }
            .code(),
            ErrorCode::UNEXPECTED_TOKEN
        );
        assert_eq!(
            ErrorKind::UnexpectedEof {
                expected: "}".into()
            }
            .code(),
            ErrorCode::UNEXPECTED_EOF
        );
        assert_eq!(
            ErrorKind::ParseError("custom error".into()).code(),
            ErrorCode::PARSE_ERROR
        );
    }

    #[test]
    fn test_error_kind_code_semantic() {
        assert_eq!(
            ErrorKind::NoMainFunction.code(),
            ErrorCode::NO_MAIN_FUNCTION
        );
        assert_eq!(
            ErrorKind::UndefinedVariable("x".into()).code(),
            ErrorCode::UNDEFINED_VARIABLE
        );
        assert_eq!(
            ErrorKind::UndefinedFunction("foo".into()).code(),
            ErrorCode::UNDEFINED_FUNCTION
        );
        assert_eq!(
            ErrorKind::TypeMismatch {
                expected: "i32".into(),
                found: "bool".into()
            }
            .code(),
            ErrorCode::TYPE_MISMATCH
        );
    }

    #[test]
    fn test_error_kind_code_control_flow() {
        assert_eq!(
            ErrorKind::BreakOutsideLoop.code(),
            ErrorCode::BREAK_OUTSIDE_LOOP
        );
        assert_eq!(
            ErrorKind::ContinueOutsideLoop.code(),
            ErrorCode::CONTINUE_OUTSIDE_LOOP
        );
    }

    #[test]
    fn test_error_kind_code_internal() {
        assert_eq!(
            ErrorKind::InternalError("bug".into()).code(),
            ErrorCode::INTERNAL_ERROR
        );
        assert_eq!(
            ErrorKind::InternalCodegenError("codegen bug".into()).code(),
            ErrorCode::INTERNAL_CODEGEN_ERROR
        );
    }

    #[test]
    fn test_error_code_equality() {
        assert_eq!(ErrorCode::TYPE_MISMATCH, ErrorCode(206));
        assert_ne!(ErrorCode::TYPE_MISMATCH, ErrorCode::UNDEFINED_VARIABLE);
    }

    #[test]
    fn test_error_code_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ErrorCode::TYPE_MISMATCH);
        set.insert(ErrorCode::UNDEFINED_VARIABLE);
        assert_eq!(set.len(), 2);
        assert!(set.contains(&ErrorCode::TYPE_MISMATCH));
    }
}
