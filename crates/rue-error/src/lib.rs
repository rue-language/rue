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
        }
    }

    /// Get the ADR number documenting this feature.
    #[allow(unreachable_code)]
    pub fn adr(&self) -> &'static str {
        match *self {
            PreviewFeature::TestInfra => "ADR-0005",
        }
    }

    /// Get all available preview features.
    pub fn all() -> &'static [PreviewFeature] {
        &[PreviewFeature::TestInfra]
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
    /// @copy struct contains a field with non-Copy type
    #[error("@copy struct '{struct_name}' has field '{field_name}' with non-Copy type '{field_type}'", struct_name = .0.struct_name, field_name = .0.field_name, field_type = .0.field_type)]
    CopyStructNonCopyField(Box<CopyStructNonCopyFieldError>),
    /// User-defined type collides with a built-in type name
    #[error("cannot define type `{type_name}`: name is reserved for built-in type")]
    ReservedTypeName { type_name: String },
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

    // Internal compiler errors (bugs in the compiler itself)
    #[error("internal compiler error: {0}")]
    InternalError(String),

    // Codegen internal errors (compiler bugs)
    #[error("internal codegen error: {0}")]
    InternalCodegenError(String),
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
        assert_eq!(names, "test_infra");
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
}
