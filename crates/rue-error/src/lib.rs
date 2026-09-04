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

pub mod ice;

use rue_span::Span;

/// The rue compiler version.
///
/// Single source of truth: the CLI's `--version` banner and ICE reports
/// (via the `ice!`/`ice_error!` macros) both read this constant. Buck2
/// builds don't set `CARGO_PKG_VERSION`, so it can't come from the
/// environment.
pub const VERSION: &str = "0.1.0";
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use thiserror::Error;

/// Classify a Lasso interner failure at the compiler diagnostic boundary.
/// Key-space and configured-memory limits are E1401; an allocator failure is
/// E1402. Keeping this mapping here prevents phase crates from losing the
/// allocator's original error kind or duplicating an inverted classifier.
pub fn interner_error_kind(kind: lasso::LassoErrorKind, message: impl Into<String>) -> ErrorKind {
    if kind.is_failed_alloc() {
        ErrorKind::CompilerResourceExhaustion(message.into())
    } else {
        ErrorKind::CompilerResourceLimit(message.into())
    }
}

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
/// - **E1200-E1299**: Comptime errors
/// - **E1300-E1399**: Unchecked-code errors (raw pointers, `checked` blocks)
/// - **E1400-E1499**: Compiler-input errors
/// - **E1500-E1599**: Driver/host errors (source loading and build environment)
/// - **E9000-E9999**: Internal compiler errors
///
/// Once assigned, error codes must never change to maintain stability for
/// documentation, search engines, and user bookmarks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode(pub u16);

/// Compiler-owned diagnostic category derived from the permanent numeric
/// bands documented on [`ErrorCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCodeCategory {
    Lexer,
    Parser,
    Semantic,
    StructAndEnum,
    ControlFlow,
    Match,
    Intrinsic,
    LiteralAndOperator,
    Array,
    LinkerAndTarget,
    PreviewFeature,
    Comptime,
    UncheckedCode,
    CompilerInput,
    DriverAndHost,
    InternalCompiler,
}

impl ErrorCodeCategory {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Lexer => "Lexer",
            Self::Parser => "Parser",
            Self::Semantic => "Semantic",
            Self::StructAndEnum => "Struct and enum",
            Self::ControlFlow => "Control flow",
            Self::Match => "Match",
            Self::Intrinsic => "Intrinsic",
            Self::LiteralAndOperator => "Literal and operator",
            Self::Array => "Array",
            Self::LinkerAndTarget => "Linker and target",
            Self::PreviewFeature => "Preview feature",
            Self::Comptime => "Comptime",
            Self::UncheckedCode => "Unchecked code",
            Self::CompilerInput => "Compiler input",
            Self::DriverAndHost => "Driver and host",
            Self::InternalCompiler => "Internal compiler",
        }
    }
}

/// Public lifecycle promise for an active diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCodeStability {
    /// The number is permanent and will not be reassigned after retirement.
    Permanent,
}

impl ErrorCodeStability {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Permanent => "Permanent",
        }
    }
}

/// Stable compiler-owned metadata for a public diagnostic code.
///
/// The inventory is derived from the [`ErrorCode`] declarations in this file,
/// so adding or retiring a code changes the compiler metadata and every
/// machine-readable consumer together. `title` is the deterministic display
/// form of `name`; it is metadata, not diagnostic prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorCodeMetadata {
    /// Numeric code, rendered as `E` followed by four digits by [`fmt::Display`].
    pub code: ErrorCode,
    /// Stable symbolic name of the associated [`ErrorCode`] constant.
    pub name: &'static str,
    /// Stable human-readable title derived from `name`.
    pub title: String,
    /// Semantic category determined by the code's permanent numeric band.
    pub category: ErrorCodeCategory,
    /// Lifecycle promise for this active public code.
    pub stability: ErrorCodeStability,
    /// Repository-relative source authority for the declaration.
    pub source_path: &'static str,
}

fn error_code_category(code: ErrorCode) -> Option<ErrorCodeCategory> {
    Some(match code.0 {
        1..=99 => ErrorCodeCategory::Lexer,
        100..=199 => ErrorCodeCategory::Parser,
        200..=399 => ErrorCodeCategory::Semantic,
        400..=499 => ErrorCodeCategory::StructAndEnum,
        500..=599 => ErrorCodeCategory::ControlFlow,
        600..=699 => ErrorCodeCategory::Match,
        700..=799 => ErrorCodeCategory::Intrinsic,
        800..=899 => ErrorCodeCategory::LiteralAndOperator,
        900..=999 => ErrorCodeCategory::Array,
        1000..=1099 => ErrorCodeCategory::LinkerAndTarget,
        1100..=1199 => ErrorCodeCategory::PreviewFeature,
        1200..=1299 => ErrorCodeCategory::Comptime,
        1300..=1399 => ErrorCodeCategory::UncheckedCode,
        1400..=1499 => ErrorCodeCategory::CompilerInput,
        1500..=1599 => ErrorCodeCategory::DriverAndHost,
        9000..=9999 => ErrorCodeCategory::InternalCompiler,
        _ => return None,
    })
}

/// A runnable example attached to an error-code explanation.
///
/// The fields are deliberately presentation-neutral so command-line and future
/// machine-readable consumers can project the same compiler-owned record. A
/// multi-file example uses `// --- path` comment lines to delimit files, with
/// the root file first. The comments make the source listing readable while
/// the compiler validation test materializes the named files verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCodeExample {
    pub title: &'static str,
    pub source: &'static str,
    /// The result the canonical compiler must produce for this example.
    pub outcome: ErrorCodeExampleOutcome,
}

/// The compiler result promised by an [`ErrorCodeExample`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCodeExampleOutcome {
    /// Compilation fails with the code that owns the example.
    EmitsThisCode,
    /// Compilation succeeds.
    Compiles,
}

/// A canonical local reference attached to an error-code explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCodeReference {
    pub title: &'static str,
    pub path: &'static str,
    pub rule: Option<&'static str>,
}

/// Compiler-owned long-form information for one public diagnostic code.
///
/// `metadata` points into [`error_code_metadata`], preserving the exact
/// [`ErrorCode`] identity and symbolic name rather than copying them into a
/// second registry. The remaining fields form a structured internal result
/// that can be projected to other formats without scraping rendered prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorCodeExplanation {
    pub metadata: &'static ErrorCodeMetadata,
    pub explanation: &'static str,
    pub likely_cause: &'static str,
    pub examples: &'static [ErrorCodeExample],
    pub references: &'static [ErrorCodeReference],
}

#[derive(Debug, Clone, Copy)]
struct ErrorCodeExplanationDeclaration {
    code: ErrorCode,
    explanation: &'static str,
    likely_cause: &'static str,
    examples: &'static [ErrorCodeExample],
    references: &'static [ErrorCodeReference],
}

/// Maximum supported syntactic nesting depth.
///
/// The parser (recursive-descent through bracketed constructs) and AstGen
/// (recursive AST→RIR lowering) both bound their recursion by this value and
/// emit [`ErrorCode::NESTING_LIMIT_EXCEEDED`] (E0482) instead of overflowing
/// the stack on pathologically nested input (RUE-42). Chosen generously so
/// real code never hits it, but low enough that the guarded recursion stays
/// well within the stack the parser runs on. Published as the nesting-depth
/// limit in spec C.6:3; the diagnosable-failure requirement is spec C.1:2.
pub const MAX_NESTING_DEPTH: usize = 256;

macro_rules! define_error_codes {
    (retired: [$($retired:literal),* $(,)?]; $( $(#[$meta:meta])* $name:ident = $value:literal $(=> {
        explanation: $explanation:literal,
        likely_cause: $likely_cause:literal,
        examples: [$($example:expr),* $(,)?],
        references: [$($reference:expr),* $(,)?] $(,)?
    })?; )*) => {
        impl ErrorCode {
            $(
                $(#[$meta])*
                pub const $name: Self = Self($value);
            )*
        }

        const ERROR_CODE_DECLARATIONS: &[(ErrorCode, &str)] = &[
            $((ErrorCode::$name, stringify!($name))),*
        ];

        /// Numeric codes that were public and are now retired.
        ///
        /// Retired codes remain permanently unavailable for reassignment.
        /// Reserved gaps for work that has not shipped are deliberately absent.
        pub const RETIRED_ERROR_CODES: &[ErrorCode] = &[$(ErrorCode($retired)),*];

        const ERROR_CODE_EXPLANATION_DECLARATIONS: &[ErrorCodeExplanationDeclaration] = &[
            $($(
                ErrorCodeExplanationDeclaration {
                    code: ErrorCode::$name,
                    explanation: $explanation,
                    likely_cause: $likely_cause,
                    examples: &[$($example),*],
                    references: &[$($reference),*],
                },
            )?)*
        ];
    };
}

define_error_codes! {
    retired: [5, 101, 408, 409, 422, 438, 498, 708];

    // ========================================================================
    // Lexer errors (E0001-E0099)
    // ========================================================================
    UNEXPECTED_CHARACTER = 1 => {
        explanation: "Rue found a source character that cannot begin any token in its current position.",
        likely_cause: "The source contains an unsupported punctuation character, a non-ASCII character outside a comment or string, or an invisible character such as a byte-order mark away from the start of the file.",
        examples: [
            ErrorCodeExample { title: "Unsupported punctuation", source: "fn main() {\n    $\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Keep non-ASCII text inside a string", source: "fn main() {\n    let greeting = \"héllo\";\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "ASCII source positions", path: "docs/spec/src/02-lexical-structure/_index.md", rule: Some("2.0:6") },
            ErrorCodeReference { title: "Byte-order marks", path: "docs/spec/src/02-lexical-structure/_index.md", rule: Some("2.0:8") },
        ],
    };
    INVALID_INTEGER = 2 => {
        explanation: "Rue could not represent the value of an integer literal while tokenizing it.",
        likely_cause: "The literal's value is greater than the largest unsigned 64-bit integer, 18446744073709551615. Digit separators do not change that value.",
        examples: [
            ErrorCodeExample { title: "Literal above the tokenization limit", source: "fn main() {\n    let too_large = 18446744073709551616;\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Largest accepted literal value", source: "fn main() {\n    let largest: u64 = 18446744073709551615;\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Integer literal syntax", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:3") },
            ErrorCodeReference { title: "Integer tokenization limit", path: "docs/spec/src/appendices/C-implementation-limits.md", rule: Some("C.2:1") },
        ],
    };
    INVALID_STRING_ESCAPE = 3 => {
        explanation: "A string literal contains a backslash escape that Rue does not recognize.",
        likely_cause: "A backslash requests an escape other than backslash, double quote, newline, tab, carriage return, or the null byte.",
        examples: [
            ErrorCodeExample { title: "Unknown string escape", source: "fn main() {\n    let text = \"bad\\q\";\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use a supported escape", source: "fn main() {\n    let text = \"line one\\nline two\";\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "String escape sequences", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:7") },
            ErrorCodeReference { title: "Invalid string escapes", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:8") },
        ],
    };
    UNTERMINATED_STRING = 4 => {
        explanation: "A string literal reached the end of its line or the end of the source file without a closing double quote.",
        likely_cause: "The closing `\"` is missing, or a physical newline was placed inside the literal instead of being written with the `\\n` escape.",
        examples: [
            ErrorCodeExample { title: "Missing closing quote", source: "fn main() {\n    let text = \"unfinished\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Close the string on the same line", source: "fn main() {\n    let text = \"finished\";\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "String literal syntax", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:6") },
            ErrorCodeReference { title: "Unterminated strings", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:9") },
        ],
    };
    // E0005 (UNSUPPORTED_INTEGER_BASE) is retired: 0x/0o/0b literals are now
    // valid Rue syntax (RUE-177). Do not reuse the code.
    UPPERCASE_BASE_PREFIX = 6 => {
        explanation: "An integer literal uses an uppercase base prefix. Rue requires the prefix itself to be lowercase, although hexadecimal digits after `0x` may be uppercase.",
        likely_cause: "The literal begins with `0X`, `0O`, or `0B`, perhaps copied from a language that accepts uppercase prefixes. Write `0x`, `0o`, or `0b` instead.",
        examples: [
            ErrorCodeExample { title: "Uppercase hexadecimal prefix", source: "fn main() {\n    let value = 0XFF;\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Lowercase prefix with uppercase digits", source: "fn main() -> i32 {\n    0xFF\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Lowercase base prefixes", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:22") }],
    };
    EMPTY_BASED_LITERAL = 7 => {
        explanation: "A binary, octal, or hexadecimal integer literal has no digits after its base prefix.",
        likely_cause: "The source contains only `0b`, `0o`, or `0x`, possibly followed by separators. At least one digit valid for that base is required.",
        examples: [
            ErrorCodeExample { title: "Hexadecimal prefix without digits", source: "fn main() {\n    let value = 0x;\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Supply a hexadecimal digit", source: "fn main() -> i32 {\n    0x0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Based literals require digits", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:20") }],
    };
    INVALID_DIGIT_FOR_BASE = 8 => {
        explanation: "A based integer literal contains a digit or letter that is not valid for its declared base.",
        likely_cause: "A binary literal contains a digit other than `0` or `1`, an octal literal contains `8`, `9`, or a letter, or a hexadecimal literal contains a letter beyond `A` through `F`.",
        examples: [
            ErrorCodeExample { title: "Invalid binary digit", source: "fn main() {\n    let value = 0b102;\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use digits from the declared base", source: "fn main() -> i32 {\n    0b101\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Digits valid for each base", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:21") }],
    };
    MALFORMED_BYTE_LITERAL = 9 => {
        explanation: "A byte literal does not contain exactly one ASCII byte or one supported escape sequence between its single quotes.",
        likely_cause: "The literal is empty, contains multiple or non-ASCII characters, uses an unknown escape, or reaches a line ending or end-of-file before its closing single quote.",
        examples: [
            ErrorCodeExample { title: "More than one byte", source: "fn main() {\n    let value = b'ab';\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Write one byte", source: "fn main() -> i32 {\n    b'a'\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Byte literal syntax", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:26") },
            ErrorCodeReference { title: "Malformed byte literals", path: "docs/spec/src/02-lexical-structure/01-tokens.md", rule: Some("2.1:27") },
        ],
    };
    /// The per-file lexer diagnostic budget was exceeded. The detailed
    /// diagnostics before this summary remain available.
    LEXER_DIAGNOSTICS_OMITTED = 10 => {
        explanation: "Rue stopped lexing one source file after retaining its first 100 lexical diagnostics. The earlier diagnostics remain available, and other source files are still processed.",
        likely_cause: "The file contains many characters or malformed literals that cannot be tokenized, often because generated input is corrupt or the file is not Rue source. Fix the first reported errors before compiling again.",
        examples: [ErrorCodeExample { title: "More than 100 lexical errors", source: "$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$$", outcome: ErrorCodeExampleOutcome::EmitsThisCode }],
        references: [ErrorCodeReference { title: "Source decomposition into tokens", path: "docs/spec/src/02-lexical-structure/_index.md", rule: Some("2.0:1") }],
    };
    /// A floating-point literal written with a leading dot (`.5`) or a
    /// trailing dot (`5.`); ADR-0065 §3 requires `0.5` / `5.0`. Sits in the
    /// lexical band even though the trailing-dot form is diagnosed by the
    /// parser — see [`ErrorKind::MalformedFloatLiteral`] for why that half
    /// cannot be decided from the lexeme alone. (RUE-1068)
    MALFORMED_FLOAT_LITERAL = 11 => {
        explanation: "A floating-point literal has a forbidden spelling: it begins with a decimal point, ends with one, or has an exponent marker without exponent digits.",
        likely_cause: "The literal was written as `.5`, `5.`, `1e`, or `1e+`. Put a digit on both sides of a decimal point and provide at least one exponent digit, for example `0.5`, `5.0`, or `1e9`.",
        examples: [
            ErrorCodeExample { title: "Leading dot diagnosed by the lexer", source: "fn main() {\n    let value = .5;\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Trailing dot diagnosed by the parser", source: "fn main() {\n    let value = 5.;\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
        ],
        references: [ErrorCodeReference { title: "Maximal-munch tokenization", path: "docs/spec/src/02-lexical-structure/_index.md", rule: Some("2.0:2") }],
    };

    // ========================================================================
    // Parser errors (E0100-E0199)
    // ========================================================================
    UNEXPECTED_TOKEN = 100 => {
        explanation: "Rue encountered a token that cannot appear at this point in the grammar. End-of-file is treated as a token for this diagnostic, so incomplete input such as a missing closing brace also reports E0100; retired E0101 is not used.",
        likely_cause: "A delimiter or required syntax element is missing, an extra punctuation mark or keyword appears in the construct, or the source ends before the current construct is complete. The diagnostic names what the parser expected and the token it found; fix the earliest parser error first because later errors may be recovery fallout.",
        examples: [
            ErrorCodeExample { title: "Unexpected end of file", source: "fn main() {", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Close the function body", source: "fn main() {}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Function syntax", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:2") }],
    };
    // E0101 was UNEXPECTED_EOF, deleted as never-produced: the parser reports
    // end-of-file as UnexpectedToken { found: "end of file" }. Keep the
    // number retired rather than reusing it.
    PARSE_ERROR = 102 => {
        explanation: "Rue recognized the surrounding grammar but rejected a more specific syntactic or parser-level rule that is clearer as a targeted message than as an expected-token list.",
        likely_cause: "Common causes include an unknown or misplaced directive, invalid directive arguments, an empty element in a comma-separated list, or trying to continue a block-like statement with a binary operator. Read the diagnostic's specific message before changing nearby punctuation.",
        examples: [
            ErrorCodeExample { title: "Unknown directive", source: "@important\nfn main() {}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Remove the unknown directive", source: "fn main() {}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Unknown builtins are rejected", path: "docs/spec/src/02-lexical-structure/05-builtins.md", rule: Some("2.5:4") }],
    };
    /// The per-file parser recovery diagnostic budget was exceeded. The
    /// detailed diagnostics before this summary remain available.
    PARSER_DIAGNOSTICS_OMITTED = 103 => {
        explanation: "Rue retained the first 100 diagnostics selected by the parser's diagnostic policy for one source file and omitted additional diagnostics from that file. E0103 is the deterministic summary of that per-file diagnostic budget; the retained errors remain available, and parsing continues for other loaded files.",
        likely_cause: "The file contains many independent syntax errors or repeats one malformed generated construct enough times to exhaust parser recovery's budget. Fix the first retained errors and compile again; later diagnostics often disappear once the parser can recover from the earliest malformed construct.",
        examples: [ErrorCodeExample { title: "More than 100 parser errors", source: "fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}fn f(,) {}", outcome: ErrorCodeExampleOutcome::EmitsThisCode }],
        references: [ErrorCodeReference { title: "Eager syntax processing of loaded files", path: "docs/spec/src/10-modules/05-program-composition.md", rule: Some("10.5:4") }],
    };

    // ========================================================================
    // Semantic errors (E0200-E0399)
    // ========================================================================
    NO_MAIN_FUNCTION = 200 => {
        explanation: "Rue could not find the program entry-point function `main` in the root module.",
        likely_cause: "The entry function is missing, is misspelled, or is declared only in an imported module. An executable Rue program must define `main` in its root source module.",
        examples: [
            ErrorCodeExample {
                title: "Root module without main",
                source: "fn start() -> i32 {\n    0\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Define the root entry point",
                source: "fn main() -> i32 {\n    0\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [
            ErrorCodeReference {
                title: "Program entry point",
                path: "docs/spec/src/06-items/01-functions.md",
                rule: Some("6.1:7"),
            },
            ErrorCodeReference {
                title: "Root-module entry point",
                path: "docs/spec/src/06-items/01-functions.md",
                rule: Some("6.1:38"),
            },
        ],
    };
    UNDEFINED_VARIABLE = 201 => {
        explanation: "Rue could not resolve a variable, constant, or enum type name used in an expression.",
        likely_cause: "The name is misspelled, is outside its lexical scope, or belongs to another module and was used without that module's binding.",
        examples: [
            ErrorCodeExample {
                title: "Undefined local",
                source: "fn main() -> i32 {\n    answer\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Define the name before use",
                source: "fn main() -> i32 {\n    let answer = 42;\n    answer\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [
            ErrorCodeReference {
                title: "Module visibility and name resolution",
                path: "docs/spec/src/10-modules/03-visibility.md",
                rule: Some("10.3:8"),
            },
        ],
    };
    UNDEFINED_FUNCTION = 202 => {
        explanation: "Rue could not resolve the function named by a call expression.",
        likely_cause: "The function name is misspelled, is outside the current module's visible scope, or should be called through an imported module binding.",
        examples: [
            ErrorCodeExample {
                title: "Call to an undefined function",
                source: "fn main() -> i32 {\n    compute()\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Define the called function",
                source: "fn compute() -> i32 { 42 }\nfn main() -> i32 {\n    compute()\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [ErrorCodeReference {
            title: "Module visibility and name resolution",
            path: "docs/spec/src/10-modules/03-visibility.md",
            rule: Some("10.3:8"),
        }],
    };
    ASSIGN_TO_IMMUTABLE = 203 => {
        explanation: "Rue rejected an assignment because its target belongs to a binding that was not declared mutable.",
        likely_cause: "A variable, array, or struct value was introduced with `let` and later used as an assignment target. Bind it with `let mut` when mutation is intended.",
        examples: [
            ErrorCodeExample {
                title: "Assignment to an immutable binding",
                source: "fn main() -> i32 {\n    let value = 0;\n    value = 42;\n    value\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Declare the binding mutable",
                source: "fn main() -> i32 {\n    let mut value = 0;\n    value = 42;\n    value\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [ErrorCodeReference {
            title: "Variable assignment",
            path: "docs/spec/src/05-statements/02-assignment.md",
            rule: Some("5.2:3"),
        }],
    };
    UNKNOWN_TYPE = 204 => {
        explanation: "Rue could not resolve a name used where a type was required.",
        likely_cause: "The type name is misspelled, is outside the current module's visible scope, or belongs to an imported module but was written without that module binding.",
        examples: [
            ErrorCodeExample {
                title: "Unknown annotation type",
                source: "fn main() -> i32 {\n    let value: Number = 42;\n    value\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Use a type in scope",
                source: "fn main() -> i32 {\n    let value: i32 = 42;\n    value\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [ErrorCodeReference {
            title: "Module visibility and name resolution",
            path: "docs/spec/src/10-modules/03-visibility.md",
            rule: Some("10.3:8"),
        }],
    };
    USE_AFTER_MOVE = 205 => {
        explanation: "Rue found a use of a move-type value after ownership of that value had already been transferred.",
        likely_cause: "A struct or another non-`Copy` value was assigned, passed by value, or returned and then used again. Use the new owner, borrow the original when ownership need not transfer, or reinitialize the moved place before reusing it.",
        examples: [
            ErrorCodeExample {
                title: "Use after ownership moves",
                source: "struct Point { x: i32 }\nfn main() -> i32 {\n    let point = Point { x: 42 };\n    let moved = point;\n    point.x\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Use the new owner",
                source: "struct Point { x: i32 }\nfn main() -> i32 {\n    let point = Point { x: 42 };\n    let moved = point;\n    moved.x\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [ErrorCodeReference {
            title: "Use after move",
            path: "docs/spec/src/03-types/08-move-semantics.md",
            rule: Some("3.8:5"),
        }],
    };
    TYPE_MISMATCH = 206 => {
        explanation: "An expression's type was incompatible with the type required by its surrounding context.",
        likely_cause: "A return value, call argument, assignment, annotation, operator operand, or other expected-type position received a different concrete type without an allowed coercion.",
        examples: [
            ErrorCodeExample {
                title: "Wrong return type",
                source: "fn main() -> i32 {\n    true\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Return the declared type",
                source: "fn main() -> i32 {\n    0\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [ErrorCodeReference {
            title: "Type compatibility",
            path: "docs/spec/src/03-types/11-type-inference.md",
            rule: Some("3.11:8"),
        }],
    };
    WRONG_ARGUMENT_COUNT = 207 => {
        explanation: "A call-like construct supplied the wrong number of values or bindings. This applies to function and built-in calls, enum tuple-variant construction (including a payload variant used as a bare value), and enum payload patterns with an explicit binding list.",
        likely_cause: "A function or built-in call has a missing or extra argument; an enum value supplies the wrong number of payload values; a payload-carrying variant was used without constructing its payload; or a match pattern's parenthesized bindings do not match the variant's payload arity.",
        examples: [
            ErrorCodeExample {
                title: "Missing call argument",
                source: "fn identity(value: i32) -> i32 { value }\nfn main() -> i32 {\n    identity()\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Supply every argument",
                source: "fn identity(value: i32) -> i32 { value }\nfn main() -> i32 {\n    identity(42)\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [
            ErrorCodeReference {
                title: "Call argument arity and modes",
                path: "docs/spec/src/04-expressions/10-call-expressions.md",
                rule: Some("4.10:3"),
            },
            ErrorCodeReference {
                title: "Enum payload-pattern arity",
                path: "docs/spec/src/04-expressions/07-match-expressions.md",
                rule: Some("4.7:30"),
            },
            ErrorCodeReference {
                title: "Enum tuple-variant construction",
                path: "docs/spec/src/06-items/03-enums.md",
                rule: Some("6.3:16"),
            },
        ],
    };
    MOVE_WHILE_CALL_LOANED = 208 => {
        explanation: "One call both loaned a non-`Copy` value through `borrow` or `inout` and tried to move that same value into another by-value argument.",
        likely_cause: "Two arguments are rooted in the same binding, with one passed by reference and the other consuming the value. The loan spans the entire call, so the move would leave it referring to moved-from storage.",
        examples: [
            ErrorCodeExample {
                title: "Move and loan in one call",
                source: "struct Resource { id: i32 }\nfn use_both(inout left: Resource, right: Resource) {}\nfn main() {\n    let mut resource = Resource { id: 1 };\n    use_both(inout resource, resource);\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Use distinct owners",
                source: "struct Resource { id: i32 }\nfn use_both(inout left: Resource, right: Resource) {}\nfn main() {\n    let mut left = Resource { id: 1 };\n    let right = Resource { id: 2 };\n    use_both(inout left, right);\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [ErrorCodeReference {
            title: "Moves that overlap call loans",
            path: "docs/spec/src/06-items/01-functions.md",
            rule: Some("6.1:36"),
        }],
    };
    UNEXPECTED_CALL_ARGUMENT_MODE = 209 => {
        explanation: "A call marked an argument `borrow` or `inout`, but the corresponding parameter is an ordinary unmarked parameter.",
        likely_cause: "The call-site mode does not exactly match the function signature. Remove the keyword for a by-value parameter, or change the parameter mode if the function is meant to borrow or mutate caller-owned storage.",
        examples: [
            ErrorCodeExample {
                title: "Borrow marker for an unmarked parameter",
                source: "fn take(value: i32) -> i32 { value }\nfn main() -> i32 {\n    let value = 42;\n    take(borrow value)\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Match the parameter mode",
                source: "fn take(value: i32) -> i32 { value }\nfn main() -> i32 {\n    let value = 42;\n    take(value)\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [ErrorCodeReference {
            title: "Call argument arity and modes",
            path: "docs/spec/src/04-expressions/10-call-expressions.md",
            rule: Some("4.10:3"),
        }],
    };
    /// Whole-value assignment to a second-class `inout str` view. The view
    /// grants exclusive access to the caller's bytes; it is not a first-class
    /// string header that may be rebound (RUE-641).
    STR_VIEW_REASSIGNMENT = 210 => {
        explanation: "Rue rejected whole-value assignment to an `inout str` view. The view may access caller-owned bytes, but its two-word view header is not an assignable string value.",
        likely_cause: "Code inside a function with an `inout str` parameter tried to replace the parameter binding, such as `text = \"new\"`. Mutate through operations supported by the view, or replace the caller's concrete buffer outside the view-taking function.",
        examples: [
            ErrorCodeExample {
                title: "Reassign an exclusive string view",
                source: "fn replace(inout text: str) {\n    text = \"new\";\n}\nfn main() {\n    let mut text: Str(8) = \"old\";\n    replace(inout text);\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Read through the view without rebinding it",
                source: "fn length(inout text: str) -> u64 {\n    text.len()\n}\nfn main() -> i32 {\n    let mut text: Str(8) = \"old\";\n    @intCast(length(inout text))\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [ErrorCodeReference {
            title: "First-class strings and borrowed views",
            path: "docs/spec/src/03-types/07-string-type.md",
            rule: Some("3.7:58"),
        }],
    };
    /// The executable entry point has parameters or a return type other than
    /// `i32` or `()` (spec 6.1:8, RUE-778). The runtime calls `main` with no
    /// arguments and consumes either its status code or the unit value, so a
    /// different source signature would violate the entry ABI.
    INVALID_MAIN_SIGNATURE = 211 => {
        explanation: "The root module's `main` function does not match Rue's fixed executable entry signature.",
        likely_cause: "`main` declares a runtime or `comptime` parameter, or returns a type other than `i32` or `()`. The runtime supplies no source-level arguments and accepts only those two return forms.",
        examples: [
            ErrorCodeExample {
                title: "Entry point with a parameter",
                source: "fn main(value: i32) -> i32 {\n    value\n}",
                outcome: ErrorCodeExampleOutcome::EmitsThisCode,
            },
            ErrorCodeExample {
                title: "Use the executable entry signature",
                source: "fn main() -> i32 {\n    0\n}",
                outcome: ErrorCodeExampleOutcome::Compiles,
            },
        ],
        references: [ErrorCodeReference {
            title: "Main function signature",
            path: "docs/spec/src/06-items/01-functions.md",
            rule: Some("6.1:8"),
        }],
    };
    // E0250-E0261 form the borrow-accessor block (ADR-0062, RUE-662). The
    // ownership/borrow family's E04xx band is at its ceiling (E0499), so
    // accessor diagnostics live here in the semantic band instead.
    /// An accessor result (`v.get_ref(i)`) was returned from the enclosing
    /// function. The result is a second-class borrowed place scoped to the
    /// enclosing full expression (ADR-0062); returning it would let the loan
    /// outlive the receiver access that justifies it.
    ACCESSOR_RESULT_RETURNED = 250;
    /// An accessor result was stored — assigned to a variable, field, or
    /// element. The result is a second-class borrowed place (ADR-0062); a
    /// stored copy would be a stored borrow, which Rue does not have.
    ACCESSOR_RESULT_STORED = 251;
    /// An accessor result was bound by a plain `let`. The result's extent is
    /// the enclosing full expression (ADR-0062), so a binding would outlive
    /// the loan. Use the result directly within one expression instead.
    ACCESSOR_RESULT_BOUND = 252;
    /// An accessor result was captured into an aggregate (struct literal or
    /// array literal). The result is a second-class borrowed place
    /// (ADR-0062); an aggregate member holding it would be a stored borrow.
    ACCESSOR_RESULT_CAPTURED = 253;
    /// An accessor body's non-diverging control flow does not end in the
    /// single trailing `yield` (ADR-0062 phase 1): the final statement is not
    /// a `yield`, a `yield` appears before the end, or the body contains a
    /// `return`/`?` exit. Guards before the yield may only diverge (trap,
    /// `@panic`) or fall through.
    ACCESSOR_BODY_MISSING_YIELD = 254;
    /// The operand of an accessor's `yield` is not a place rooted at the
    /// receiver parameter (`self`). An accessor hands out a projection of its
    /// receiver (ADR-0062); yielding a local, temporary, or unrelated place
    /// would dangle once the accessor's frame is gone.
    ACCESSOR_YIELD_NOT_RECEIVER_ROOTED = 255;
    /// A `yield` expression appears outside the body of a `-> borrow T`
    /// accessor. `yield` is the accessor body's exit form (ADR-0062).
    YIELD_OUTSIDE_ACCESSOR = 256;
    /// An accessor result and receiver use different modes: `-> borrow T`
    /// requires `borrow self`, while `-> inout T` requires `inout self`.
    ACCESSOR_REQUIRES_BORROW_SELF = 257;
    /// A value with drop glue was read out of an accessor result by value.
    /// The result is a borrowed place, not an owner (ADR-0062); copying a
    /// drop-glue value out of it would mint an aliasing second owner — the
    /// same double-free the E0711 gate closes (RUE-651). Only trivially
    /// droppable element values may be read out by value.
    ACCESSOR_RESULT_MOVED = 258;
    /// A root was used incompatibly in the same full expression as an
    /// accessor result: an exclusive result conflicts with any other loan,
    /// while a shared result conflicts with an exclusive use (ADR-0062).
    ACCESSOR_LOAN_CONFLICT = 259;
    /// An accessor declares a parameter mode other than by-value (`borrow`,
    /// `inout`, or `comptime` on a non-receiver parameter). Phase 1 accessor
    /// arguments are by-value guard inputs (ADR-0062); by-ref accessor
    /// parameters are deferred with the coroutine form (RUE-1012).
    ACCESSOR_PARAM_MODE_UNSUPPORTED = 260;
    /// An accessor call re-entered an accessor whose expansion is already in
    /// progress: `fn xr(borrow self) -> borrow i64 { yield self.xr(); }`, or
    /// the same cycle through several accessors. An accessor call compiles by
    /// inlining its body at the call site (ADR-0062), so an accessor-call
    /// cycle has no finite expansion.
    ACCESSOR_RECURSION = 261;
    /// Two `test "name" { .. }` declarations in one module share a name
    /// (ADR-0083 §1, RUE-1618). A test's name is its identity within its
    /// module, so a duplicate makes the pair unaddressable by a filter or a
    /// stable test ID. Like the accessor block above, it sits in the semantic
    /// band rather than with its E04xx duplicate-definition siblings because
    /// that band is at its ceiling (E0499).
    DUPLICATE_TEST_DEFINITION = 262;

    // ========================================================================
    // Struct/enum errors (E0400-E0499)
    // ========================================================================
    MISSING_FIELDS = 400 => {
        explanation: "A struct value was constructed without an initializer for every field declared by its type.",
        likely_cause: "A field was omitted from the struct literal, often after the struct definition gained a new field. Supply each declared field exactly once; initializer order does not matter.",
        examples: [
            ErrorCodeExample { title: "Omitted struct field", source: "struct Point { x: i32, y: i32 }\nfn main() -> i32 {\n    let point = Point { x: 10 };\n    point.x\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Initialize every field", source: "struct Point { x: i32, y: i32 }\nfn main() -> i32 {\n    let point = Point { x: 10, y: 32 };\n    point.x + point.y\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Complete struct initialization", path: "docs/spec/src/03-types/06-struct-types.md", rule: Some("3.6:5") }],
    };
    UNKNOWN_FIELD = 401 => {
        explanation: "A struct literal, field access, or another field-naming operation uses an identifier that is not a field of the relevant struct type.",
        likely_cause: "The field name is misspelled, belongs to a different struct, or the struct definition was changed without updating its construction or use.",
        examples: [
            ErrorCodeExample { title: "Access an unknown field", source: "struct Point { x: i32, y: i32 }\nfn main() -> i32 {\n    let point = Point { x: 10, y: 32 };\n    point.z\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use a declared field", source: "struct Point { x: i32, y: i32 }\nfn main() -> i32 {\n    let point = Point { x: 10, y: 32 };\n    point.y\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Struct literal fields", path: "docs/spec/src/03-types/06-struct-types.md", rule: Some("3.6:15") },
            ErrorCodeReference { title: "Valid field-access names", path: "docs/spec/src/04-expressions/12-field-access.md", rule: Some("4.12:4") },
        ],
    };
    DUPLICATE_FIELD = 402 => {
        explanation: "A struct declaration defines the same field name more than once, or a struct literal supplies more than one initializer for the same field.",
        likely_cause: "A field declaration or initializer was duplicated, possibly after a rename or copy-and-paste edit. Give every declared field a unique name, and initialize each field at most once in a struct literal.",
        examples: [
            ErrorCodeExample { title: "Repeated field initializer", source: "struct Point { x: i32, y: i32 }\nfn main() -> i32 {\n    let point = Point { x: 10, x: 20, y: 32 };\n    point.y\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Initialize each field once", source: "struct Point { x: i32, y: i32 }\nfn main() -> i32 {\n    let point = Point { x: 10, y: 32 };\n    point.x + point.y\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Unique struct field declarations", path: "docs/spec/src/03-types/06-struct-types.md", rule: Some("3.6:6") },
            ErrorCodeReference { title: "Struct literal field matching", path: "docs/spec/src/03-types/06-struct-types.md", rule: Some("3.6:15") },
        ],
    };
    COPY_STRUCT_NON_COPY_FIELD = 403 => {
        explanation: "A struct marked `@copy` contains a field whose type has move semantics. Implicitly duplicating the outer value would also have to duplicate that non-Copy field.",
        likely_cause: "A field is a struct without `@copy`, a move-typed aggregate, or another type that cannot be implicitly duplicated. Remove `@copy` from the outer struct or make every field type Copy when that is semantically valid.",
        examples: [
            ErrorCodeExample { title: "Non-Copy field in a @copy struct", source: "struct Inner { value: i32 }\n@copy\nstruct Outer { inner: Inner }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use only Copy field types", source: "@copy\nstruct Inner { value: i32 }\n@copy\nstruct Outer { inner: Inner }\nfn main() -> i32 {\n    let outer = Outer { inner: Inner { value: 42 } };\n    let duplicate = outer;\n    outer.inner.value + duplicate.inner.value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Copy struct field requirement", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:18") }],
    };
    /// Retained compatibility metadata for compilers that reserved built-in
    /// nominal spellings. Current Rue reserves no type names (spec 6.0:3), so
    /// production compilation does not emit this code.
    RESERVED_TYPE_NAME = 404 => {
        explanation: "This code is retained for compatibility with older Rue compilers that rejected a user-defined type whose name was reserved for a built-in nominal. Current Rue reserves no type-name spellings and does not emit E0404.",
        likely_cause: "If E0404 appears in stored output or from an older compiler, that compiler was enforcing a historical built-in type-name reservation. With a current compiler, user-defined type names participate in ordinary lexical and module lookup.",
        examples: [ErrorCodeExample { title: "Built-in spellings are ordinary type names", source: "struct StrBuf { value: i32 }\nfn main() -> i32 {\n    StrBuf { value: 42 }.value\n}", outcome: ErrorCodeExampleOutcome::Compiles }],
        references: [ErrorCodeReference { title: "Ordinary type-name lookup", path: "docs/spec/src/06-items/_index.md", rule: Some("6.0:3") }],
    };
    DUPLICATE_TYPE_DEFINITION = 405 => {
        explanation: "A module defines more than one struct or enum with the same type name.",
        likely_cause: "A type declaration was duplicated, or a struct and enum in the same module were given the same name. Rename or remove one declaration; separate modules may independently use the same type name.",
        examples: [
            ErrorCodeExample { title: "Duplicate type name", source: "struct Point { x: i32 }\nenum Point { Origin }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use unique type names in one module", source: "struct Point { x: i32 }\nenum Position { Origin }\nfn main() -> i32 {\n    Point { x: 42 }.x\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Type-name uniqueness", path: "docs/spec/src/06-items/_index.md", rule: Some("6.0:2") }],
    };
    LINEAR_VALUE_NOT_CONSUMED = 406 => {
        explanation: "A value carrying a linear obligation reached the end of its scope without being consumed.",
        likely_cause: "A binding of a declared-linear type, or an aggregate containing a linear value, was left live. Move it into a by-value consumer, return it to transfer the obligation, consume its declared-linear value through a field projection, or explicitly drop it when `@drop` is appropriate.",
        examples: [
            ErrorCodeExample { title: "Unconsumed linear value", source: "linear struct Token { value: i32 }\nfn main() -> i32 {\n    let token = Token { value: 42 };\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Pass the value to a consumer", source: "linear struct Token { value: i32 }\nfn consume(token: Token) -> i32 { token.value }\nfn main() -> i32 {\n    let token = Token { value: 42 };\n    consume(token)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Linear values must be consumed", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:32") },
            ErrorCodeReference { title: "Linear consumption by use", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:33") },
        ],
    };
    LINEAR_STRUCT_COPY = 407 => {
        explanation: "A struct was declared both `linear` and `@copy`. Linear values must have one tracked consumption, while Copy values may be duplicated implicitly.",
        likely_cause: "The `@copy` directive was applied to a `linear struct`, combining incompatible ownership promises. Remove `@copy` and consume each linear value, or remove `linear` if freely duplicating the value is the intended behavior.",
        examples: [
            ErrorCodeExample { title: "Linear struct marked @copy", source: "@copy\nlinear struct Token { value: i32 }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Keep the value linear and consume it", source: "linear struct Token { value: i32 }\nfn consume(token: Token) -> i32 { token.value }\nfn main() -> i32 {\n    let token = Token { value: 42 };\n    consume(token)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Linear structs cannot be Copy", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:37") }],
    };
    // 408, 409 retired with the @handle directive (RUE-199).
    DUPLICATE_METHOD = 410 => {
        explanation: "A struct definition declares more than one method or associated function with the same name. Both declaration forms share the struct's callable-member name space, so each callable member name must be unique within that struct.",
        likely_cause: "A method or associated function was copied, renamed incompletely, or generated twice in one struct definition. Remove one declaration or give the callable members distinct names.",
        examples: [
            ErrorCodeExample { title: "Duplicate method declaration", source: "struct Point {\n    x: i32,\n\n    fn value(self) -> i32 { self.x }\n    fn value(self) -> i32 { self.x }\n}\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Give each method a unique name", source: "struct Point {\n    x: i32,\n\n    fn value(self) -> i32 { self.x }\n    fn doubled(self) -> i32 { self.x * 2 }\n}\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Unique method names", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:16") }],
    };
    UNDEFINED_METHOD = 411 => {
        explanation: "A method call names no method available for the receiver's type. Rue resolves user-defined methods on the receiver's struct and supported built-in receiver methods, including operations on strings and slices.",
        likely_cause: "The method name is misspelled, belongs to another receiver type, or is unavailable for this built-in receiver kind; an associated function may also have been intended. Check the receiver expression's type and the callable members that type provides.",
        examples: [
            ErrorCodeExample { title: "Unknown method name", source: "struct Point { x: i32 }\nfn main() -> i32 {\n    let point = Point { x: 42 };\n    point.value()\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Declare the method on the receiver type", source: "struct Point {\n    x: i32,\n\n    fn value(self) -> i32 { self.x }\n}\nfn main() -> i32 {\n    let point = Point { x: 42 };\n    point.value()\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Undefined method calls", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:21") }],
    };
    UNDEFINED_ASSOC_FN = 412 => {
        explanation: "An associated-style call names no callable member on the selected type. For a struct, the missing member would be an associated function declared without `self`; this code also covers a missing called variant on an inline comptime-produced enum type.",
        likely_cause: "The member or type name is wrong, a struct method requiring a receiver value was intended, or an inline comptime-produced enum has no variant with the called name. Check the selected type's declarations and use the call form that matches the intended member.",
        examples: [
            ErrorCodeExample { title: "Unknown associated function", source: "struct Point { x: i32 }\nfn main() -> i32 {\n    Point.origin().x\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Declare and call an associated function", source: "struct Point {\n    x: i32,\n\n    fn origin() -> Point { Point { x: 0 } }\n}\nfn main() -> i32 {\n    Point.origin().x\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Associated-function calls", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:13") },
            ErrorCodeReference { title: "Tuple-variant construction", path: "docs/spec/src/06-items/03-enums.md", rule: Some("6.3:16") },
        ],
    };
    METHOD_CALL_ON_NON_STRUCT = 413 => {
        explanation: "A method-call expression uses a receiver whose type does not provide a matching method. User-defined methods belong to struct types; compiler-provided built-in receiver operations are resolved separately.",
        likely_cause: "The receiver has an unexpected scalar or other non-struct type, or method syntax was used where an ordinary function or operator was intended. Check the receiver expression's type and choose an operation available for that type.",
        examples: [
            ErrorCodeExample { title: "Method call on an integer", source: "fn main() -> i32 {\n    let value: i32 = 42;\n    value.missing()\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use an operation defined for the value", source: "fn main() -> i32 {\n    let value: i32 = 40;\n    value + 2\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Method receivers must support methods", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:20") }],
    };
    METHOD_CALLED_AS_ASSOC_FN = 414 => {
        explanation: "A function declared with a `self` receiver was called through its type as though it were an associated function. A method needs a receiver value to supply `self`.",
        likely_cause: "The call uses `Type.method()` instead of `receiver.method()`, or the declaration accidentally includes a `self` parameter. Construct or obtain a receiver value, or remove `self` if the function is meant to be associated with the type rather than an instance.",
        examples: [
            ErrorCodeExample { title: "Method called through its type", source: "struct Point {\n    x: i32,\n\n    fn value(self) -> i32 { self.x }\n}\nfn main() -> i32 {\n    Point.value()\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Call the method on a receiver", source: "struct Point {\n    x: i32,\n\n    fn value(self) -> i32 { self.x }\n}\nfn main() -> i32 {\n    let point = Point { x: 42 };\n    point.value()\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Methods require receiver syntax", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:23") }],
    };
    ASSOC_FN_CALLED_AS_METHOD = 415 => {
        explanation: "A function declared without a `self` receiver was called on a value as though it were a method. An associated function is selected through its struct type instead.",
        likely_cause: "The call uses `receiver.function()` instead of `Type.function()`, or the declaration is missing its intended `self` parameter. Call it through the type, or add the appropriate receiver parameter when instance access is intended.",
        examples: [
            ErrorCodeExample { title: "Associated function called on a value", source: "struct Point {\n    x: i32,\n\n    fn origin() -> Point { Point { x: 0 } }\n}\nfn main() -> i32 {\n    let point = Point { x: 42 };\n    point.origin().x\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Call the associated function through its type", source: "struct Point {\n    x: i32,\n\n    fn origin() -> Point { Point { x: 0 } }\n}\nfn main() -> i32 {\n    Point.origin().x\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Associated functions require type syntax", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:22") }],
    };
    DUPLICATE_DESTRUCTOR = 416 => {
        explanation: "More than one user-defined destructor is declared for the same struct type. Rue permits at most one destructor because dropping a value has one canonical user cleanup action.",
        likely_cause: "A `drop fn` declaration was duplicated, or cleanup for one type was split across multiple destructor declarations. Combine the cleanup into a single destructor for that struct.",
        examples: [
            ErrorCodeExample { title: "Two destructors for one struct", source: "struct Resource { value: i32 }\ndrop fn Resource(self) {}\ndrop fn Resource(self) {}\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Keep one destructor per struct", source: "struct Resource { value: i32 }\ndrop fn Resource(self) {}\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "One destructor per struct", path: "docs/spec/src/03-types/09-destructors.md", rule: Some("3.9:26") }],
    };
    DESTRUCTOR_UNKNOWN_TYPE = 417 => {
        explanation: "A top-level `drop fn` names a type that is not a struct defined in the same module. Destructor target lookup is module-local, so a struct elsewhere in the program does not satisfy the declaration.",
        likely_cause: "The type name is misspelled, the struct declaration is missing from this module, or the name denotes a non-struct type. Declare the struct in the same module and make the destructor's type name match it exactly.",
        examples: [
            ErrorCodeExample { title: "Destructor for an unknown type", source: "drop fn Resource(self) {}\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Define the destructor's struct type", source: "struct Resource { value: i32 }\ndrop fn Resource(self) {}\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Destructor target types", path: "docs/spec/src/03-types/09-destructors.md", rule: Some("3.9:27") }],
    };
    DUPLICATE_CONSTANT = 418 => {
        explanation: "A source file declares two top-level constants with the same name. Constant names are module-scoped, and a const/const collision within one file is ambiguous.",
        likely_cause: "A constant declaration was copied, generated twice, or renamed to an existing constant's name. Remove one declaration or give each constant in the file a distinct name; another module may independently use the same name.",
        examples: [
            ErrorCodeExample { title: "Duplicate constant name", source: "const LIMIT: i32 = 40;\nconst LIMIT: i32 = 42;\nfn main() -> i32 { LIMIT }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use distinct constant names", source: "const BASE: i32 = 40;\nconst LIMIT: i32 = 42;\nfn main() -> i32 { LIMIT - BASE }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Module-scoped top-level names", path: "docs/spec/src/10-modules/05-program-composition.md", rule: Some("10.5:1") }],
    };
    CONST_EXPR_NOT_SUPPORTED = 434 => {
        explanation: "Rue encountered an expression that is not supported as a constant value. Constant initializers accept the language's comptime-evaluable forms, string literals, module expressions, and function references used as callable aliases; ordinary runtime computation is rejected. A callable alias is not a first-class runtime value and is supported only as the callee of a call.",
        likely_cause: "A constant initializer calls a runtime function, constructs an aggregate value, or otherwise depends on evaluation available only while the program runs; alternatively, code uses a callable alias as an ordinary value. Replace an initializer with supported constant operations and constant references, move the computation into a function, or invoke a callable alias directly.",
        examples: [
            ErrorCodeExample { title: "Call a runtime function in a constant initializer", source: "fn answer() -> i32 { 42 }\nconst ANSWER: i32 = answer();\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use comptime-evaluable constant arithmetic", source: "const BASE: i32 = 40;\nconst ANSWER: i32 = BASE + 2;\nfn main() -> i32 { ANSWER }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Comptime-evaluable constant initializers", path: "docs/spec/src/06-items/05-constants.md", rule: Some("6.5:2") },
            ErrorCodeReference { title: "Callable function aliases", path: "docs/spec/src/06-items/05-constants.md", rule: Some("6.5:15") },
        ],
    };
    DUPLICATE_VARIANT = 419 => {
        explanation: "An enum declaration defines the same variant name more than once. Variant names identify the alternatives of one enum and must be unique within that enum.",
        likely_cause: "A variant was copied, generated twice, or renamed to a name already used in the same enum. Remove the duplicate or give every variant in the enum a distinct name; another enum may independently use the same variant name.",
        examples: [
            ErrorCodeExample { title: "Duplicate variant name", source: "enum Color { Red, Green, Red }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use unique variant names", source: "enum Color { Red, Green, Blue }\nfn main() -> i32 {\n    let color = Color.Blue;\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Unique enum variant names", path: "docs/spec/src/06-items/03-enums.md", rule: Some("6.3:3") }],
    };
    UNKNOWN_VARIANT = 420 => {
        explanation: "An enum variant expression or pattern names a variant that is not declared by the resolved enum type.",
        likely_cause: "The variant name is misspelled, belongs to another enum, or was removed or renamed while uses remained. Check the variants declared by the named enum and use the exact matching name.",
        examples: [
            ErrorCodeExample { title: "Unknown variant on a known enum", source: "enum Color { Red, Green, Blue }\nfn main() -> i32 {\n    let color = Color.Yellow;\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use a declared variant", source: "enum Color { Red, Green, Blue }\nfn main() -> i32 {\n    let color = Color.Green;\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Enum variant references", path: "docs/spec/src/06-items/03-enums.md", rule: Some("6.3:4") },
            ErrorCodeReference { title: "Unknown variants", path: "docs/spec/src/06-items/03-enums.md", rule: Some("6.3:5") },
        ],
    };
    UNKNOWN_ENUM_TYPE = 421 => {
        explanation: "An enum variant pattern names an enum type that cannot be resolved in that pattern's scope. This diagnostic is specific to enum-pattern lookup; an unresolved bare name in a value expression follows ordinary undefined-name diagnostics instead.",
        likely_cause: "The pattern's enum name is misspelled, is not visible in the current module, or names a different kind of item. Use an enum type available in the pattern's scope and spell its path correctly.",
        examples: [
            ErrorCodeExample { title: "Unknown enum type in a pattern", source: "enum Color { Red, Blue }\nfn main() -> i32 {\n    let color = Color.Red;\n    match color {\n        Shade.Red => 1,\n        _ => 0,\n    }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use the resolved enum type", source: "enum Color { Red, Blue }\nfn main() -> i32 {\n    let color = Color.Red;\n    match color {\n        Color.Red => 1,\n        Color.Blue => 0,\n    }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Enum variant pattern syntax", path: "docs/spec/src/04-expressions/07-match-expressions.md", rule: Some("4.7:2") },
            ErrorCodeReference { title: "Enum variant name lookup", path: "docs/spec/src/06-items/03-enums.md", rule: Some("6.3:4") },
        ],
    };
    // 422 (FIELD_WRONG_ORDER) is retired: struct-literal fields may now be
    // given in any order, matched to declared fields by name (RUE-9). Do not
    // reuse the number.
    FIELD_ACCESS_ON_NON_STRUCT = 423 => {
        explanation: "A field-based operation was applied to a non-struct type. Both `.field` access and `@offset_of(Type, field)` require the value or type being inspected to be a struct.",
        likely_cause: "The expression before `.field` has an unexpected scalar, enum, array, or other non-struct type, or the first argument to `@offset_of` is not a struct type. Check the relevant value or type and name fields only on a struct.",
        examples: [
            ErrorCodeExample { title: "Field access on an integer", source: "fn main() -> i32 {\n    let value: i32 = 42;\n    value.answer\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Access a field on a struct", source: "struct Answer { value: i32 }\nfn main() -> i32 {\n    let answer = Answer { value: 42 };\n    answer.value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Field-access base types", path: "docs/spec/src/04-expressions/12-field-access.md", rule: Some("4.12:3") },
            ErrorCodeReference { title: "Offset-of arguments", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:92") },
            ErrorCodeReference { title: "Invalid offset-of types", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:95") },
        ],
    };
    INVALID_ASSIGNMENT_TARGET = 424 => {
        explanation: "An assignment target is syntactically accepted but does not resolve to a writable place. Writable roots include `let mut` locals, `inout` parameters, and receivers declared `mut self` or `inout self` within their method body; field and index projections from those roots remain places. An exclusive place-returning accessor over writable storage may also form or continue a place. Bare `self`, `borrow self`, ordinary values, and shared accessor results are not writable roots. Other diagnostics separately report places whose roots exist but lack permission to mutate.",
        likely_cause: "The left-hand side is a temporary value, a value-returning or shared method call, or an accessor chain that does not yield an exclusive place. Assign through a mutable local, an `inout` parameter, an appropriately mutable receiver, a field or array element rooted in one, or an exclusive accessor that denotes writable storage.",
        examples: [
            ErrorCodeExample { title: "Assign through a value-returning method", source: "struct Counter {\n    value: i32,\n\n    fn current(self) -> i32 { self.value }\n}\nfn main() {\n    let mut counter = Counter { value: 0 };\n    counter.current() = 42;\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Assign through a mutable field place", source: "struct Counter { value: i32 }\nfn main() -> i32 {\n    let mut counter = Counter { value: 0 };\n    counter.value = 42;\n    counter.value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Assignment stores into a place", path: "docs/spec/src/05-statements/02-assignment.md", rule: Some("5.2:1") },
            ErrorCodeReference { title: "Assignment-place forms", path: "docs/spec/src/05-statements/02-assignment.md", rule: Some("5.2:2") },
        ],
    };
    INOUT_NON_LVALUE = 425 => {
        explanation: "An explicit `inout` argument or ordinary `inout self` receiver does not name mutable caller-owned storage. Exclusive access must write back through an addressable place: a mutable variable, field, or array element.",
        likely_cause: "A literal, arithmetic expression, constant, or call result was passed `inout` or used directly as an `inout self` receiver. Bind the value to a mutable local first, or use an existing mutable place whose changes should remain visible after the call.",
        examples: [
            ErrorCodeExample { title: "Pass a computed value inout", source: "fn increment(inout value: i32) { value = value + 1; }\nfn main() -> i32 {\n    increment(inout 40 + 2);\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use a mutable place", source: "fn increment(inout value: i32) { value = value + 1; }\nfn main() -> i32 {\n    let mut value = 41;\n    increment(inout value);\n    value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Inout arguments require places", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:17") },
            ErrorCodeReference { title: "By-reference receivers require places", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:27") },
        ],
    };
    INOUT_EXCLUSIVE_ACCESS = 426 => {
        explanation: "One call grants more than one exclusive `inout` access to the same root variable through its arguments, receiver, or accessor-rooted places. Rue grants exclusive access at root granularity, so even different fields or elements of one aggregate cannot be independently loaned by the same call.",
        likely_cause: "Two exclusive accesses use the same variable directly or project from the same struct or array. Split the operation into separate calls, or place independently accessed values in distinct root variables.",
        examples: [
            ErrorCodeExample { title: "Loan distinct fields of one root", source: "struct Pair { left: i32, right: i32 }\nfn swap(inout a: i32, inout b: i32) {\n    let old = a;\n    a = b;\n    b = old;\n}\nfn main() -> i32 {\n    let mut pair = Pair { left: 1, right: 2 };\n    swap(inout pair.left, inout pair.right);\n    pair.left\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Loan distinct root variables", source: "fn swap(inout a: i32, inout b: i32) {\n    let old = a;\n    a = b;\n    b = old;\n}\nfn main() -> i32 {\n    let mut left = 1;\n    let mut right = 2;\n    swap(inout left, inout right);\n    left + right\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "One exclusive argument loan per root", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:20") },
            ErrorCodeReference { title: "Receiver loans participate in exclusivity", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:28") },
            ErrorCodeReference { title: "Accessor exclusivity is root-based", path: "docs/spec/src/06-items/06-borrow-accessors.md", rule: Some("6.6:16") },
        ],
    };
    BORROW_NON_LVALUE = 427 => {
        explanation: "A place-required by-reference position does not name addressable storage. This includes implicit receiver autoref, shared or exclusive accessor receivers, and view coercions whose source must remain in place; unlike an ordinary explicit `borrow` operand, these positions cannot create promoted or temporary storage.",
        likely_cause: "A `borrow self` method, a borrow or inout accessor, or a place-required view coercion was applied directly to a computed value such as a function or method result. Bind the result to a local before borrowing it. Ordinary explicit `borrow` function arguments may still use value expressions because those operands are elaborated separately.",
        examples: [
            ErrorCodeExample { title: "Borrow a computed method receiver", source: "struct Item { value: i32, fn read(borrow self) -> i32 { self.value } }\nfn make() -> Item { Item { value: 42 } }\nfn main() -> i32 { make().read() }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Bind the receiver to a place", source: "struct Item { value: i32, fn read(borrow self) -> i32 { self.value } }\nfn make() -> Item { Item { value: 42 } }\nfn main() -> i32 {\n    let item = make();\n    item.read()\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "By-reference receivers require places", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:27") },
            ErrorCodeReference { title: "Accessor receivers require places", path: "docs/spec/src/06-items/06-borrow-accessors.md", rule: Some("6.6:8") },
            ErrorCodeReference { title: "Explicit borrow operands may be values", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:39") },
            ErrorCodeReference { title: "Slice coercion sources require places", path: "docs/spec/src/07-arrays/02-slices.md", rule: Some("7.2:13") },
        ],
    };
    MUTATE_BORROWED_VALUE = 428 => {
        explanation: "Code attempts to mutate storage held under a shared loan, such as a `borrow` parameter or a collection borrowed by `for`. The restriction covers the borrowed binding itself and every field or element projected from it, including passing any such place `inout`.",
        likely_cause: "Code assigns through a shared-borrowed parameter or mutates a collection while iterating it, possibly by forwarding borrowed storage to a mutating operation. Use `inout` when caller-visible mutation is intended, perform the mutation outside the iteration, or limit the operation to reads.",
        examples: [
            ErrorCodeExample { title: "Mutate a field through a shared borrow", source: "struct Item { value: i32 }\nfn change(borrow item: Item) { item.value = 42; }\nfn main() -> i32 {\n    let item = Item { value: 0 };\n    change(borrow item);\n    item.value\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Read through a shared borrow", source: "struct Item { value: i32 }\nfn read(borrow item: Item) -> i32 { item.value }\nfn main() -> i32 {\n    let item = Item { value: 42 };\n    read(borrow item)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Borrowed storage is read-only", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:24") },
            ErrorCodeReference { title: "For iteration holds a shared loan", path: "docs/spec/src/04-expressions/08-loop-expressions.md", rule: Some("4.8:26") },
        ],
    };
    MOVE_OUT_OF_BORROW = 429 => {
        explanation: "Code tries to move a non-Copy value out of storage held under a shared loan, such as a `borrow` parameter or an element borrowed by `for`. Neither the whole borrowed value nor a non-Copy field or element belongs to the current operation, so transferring its ownership would invalidate the loaned storage.",
        likely_cause: "A borrowed value or one of its non-Copy projections was returned, stored in an owned aggregate, or passed to a by-value consumer. Read Copy data through the loan, pass the value onward as `borrow`, or change the API or iteration so ownership transfers explicitly.",
        examples: [
            ErrorCodeExample { title: "Move a borrowed value into a consumer", source: "struct Item { value: i32 }\nfn consume(item: Item) -> i32 { item.value }\nfn invalid(borrow item: Item) -> i32 { consume(item) }\nfn main() -> i32 {\n    let item = Item { value: 42 };\n    invalid(borrow item)\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Borrow the value in the nested call", source: "struct Item { value: i32 }\nfn read(borrow item: Item) -> i32 { item.value }\nfn valid(borrow item: Item) -> i32 { read(borrow item) }\nfn main() -> i32 {\n    let item = Item { value: 42 };\n    valid(borrow item)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Borrowed values cannot be moved", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:25") },
            ErrorCodeReference { title: "For iteration holds a shared loan", path: "docs/spec/src/04-expressions/08-loop-expressions.md", rule: Some("4.8:26") },
        ],
    };
    BORROW_INOUT_CONFLICT = 430 => {
        explanation: "A call creates overlapping shared and exclusive loans of the same root variable. Rue's law of exclusivity permits either one `inout` access or any number of `borrow` accesses to a root. A nested `inout` during argument evaluation conflicts when the enclosing loan is shared or its operand has already been materialized as a view; an address-passed enclosing `inout` may instead observe the sequenced mutation.",
        likely_cause: "One argument borrows a variable while another passes that variable, one of its projections, or a conflicting nested access `inout`. Separate the accesses into distinct calls, preserve any needed read in a value first, or operate on distinct root variables.",
        examples: [
            ErrorCodeExample { title: "Borrow and mutate fields of one root", source: "struct Pair { left: i32, right: i32 }\nfn copy_into(borrow source: i32, inout target: i32) { target = source; }\nfn main() -> i32 {\n    let mut pair = Pair { left: 42, right: 0 };\n    copy_into(borrow pair.left, inout pair.right);\n    pair.right\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Share the same root read-only", source: "fn add(borrow a: i32, borrow b: i32) -> i32 { a + b }\nfn main() -> i32 {\n    let value = 21;\n    add(borrow value, borrow value)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Shared and exclusive loan conflicts", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:30") },
            ErrorCodeReference { title: "Receiver loans participate in exclusivity", path: "docs/spec/src/06-items/04-impl-blocks.md", rule: Some("6.4:28") },
        ],
    };
    INOUT_KEYWORD_MISSING = 431 => {
        explanation: "A call passes an argument to an `inout` parameter without the required `inout` mode: the argument is unmarked or is marked `borrow`. The call site must select exclusive mutable access exactly.",
        likely_cause: "The callee's parameter was changed to `inout`, or the call uses no marker or the wrong marker. Add or replace the marker with `inout` before an addressable mutable argument, or change the parameter mode if mutation and write-back are not intended.",
        examples: [
            ErrorCodeExample { title: "Omit the inout call-site marker", source: "fn increment(inout value: i32) { value = value + 1; }\nfn main() -> i32 {\n    let mut value = 41;\n    increment(value);\n    value\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Mark the exclusive argument", source: "fn increment(inout value: i32) { value = value + 1; }\nfn main() -> i32 {\n    let mut value = 41;\n    increment(inout value);\n    value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Call argument modes must match exactly", path: "docs/spec/src/04-expressions/10-call-expressions.md", rule: Some("4.10:3") },
            ErrorCodeReference { title: "Inout markers at call sites", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:16") },
        ],
    };
    BORROW_KEYWORD_MISSING = 432 => {
        explanation: "A call passes an argument to a `borrow` parameter without the required `borrow` mode: the argument is unmarked or is marked `inout`. The call site must select shared read-only access exactly.",
        likely_cause: "The callee's parameter was changed to `borrow`, or the call uses no marker or the wrong marker. Add or replace the marker with `borrow`, or change the parameter mode if the callee should take the value by ownership.",
        examples: [
            ErrorCodeExample { title: "Omit the borrow call-site marker", source: "fn read(borrow value: i32) -> i32 { value }\nfn main() -> i32 {\n    let value = 42;\n    read(value)\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Mark the shared argument", source: "fn read(borrow value: i32) -> i32 { value }\nfn main() -> i32 {\n    let value = 42;\n    read(borrow value)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Call argument modes must match exactly", path: "docs/spec/src/04-expressions/10-call-expressions.md", rule: Some("4.10:3") },
            ErrorCodeReference { title: "Borrow markers at call sites", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:23") },
        ],
    };
    EMPTY_STRUCT = 433 => {
        explanation: "An anonymous struct declaration contains neither fields nor methods. Such a declaration has no observable structure or behavior and is rejected; this rule does not apply to named zero-sized structs or anonymous structs that declare methods.",
        likely_cause: "A comptime type producer returned a bare `struct {}` placeholder. Add a field or method to the anonymous type, or declare a named empty struct when a named zero-sized marker type is intended.",
        examples: [
            ErrorCodeExample { title: "Produce an empty anonymous struct", source: "fn Empty() -> type { struct {} }\nfn main() -> i32 {\n    let E = Empty();\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Give the anonymous struct behavior", source: "fn Utility() -> type {\n    struct { fn answer() -> i32 { 42 } }\n}\nfn main() -> i32 {\n    let U = Utility();\n    U.answer()\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Empty anonymous structs", path: "docs/spec/src/04-expressions/14-comptime.md", rule: Some("4.14:9") }],
    };
    RESERVED_FUNCTION_NAME = 435 => {
        explanation: "A user-defined function has a name reserved for a runtime or code-generation helper. Rue reserves every name beginning with `__rue_`, the entry-point symbols `_start` and `_main`, and the fixed memory-routine names `memcpy`, `memmove`, `memset`, `memcmp`, and `bcmp` so user code cannot collide with compiler- or runtime-emitted symbols.",
        likely_cause: "The function was given an internal-looking name or the name of a platform entry point or memory routine. Rename the function to an ordinary user identifier that is outside the reserved set.",
        examples: [
            ErrorCodeExample { title: "Define a reserved runtime-helper name", source: "fn __rue_helper() -> i32 { 42 }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use an ordinary user function name", source: "fn rue_helper() -> i32 { 42 }\nfn main() -> i32 { rue_helper() }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Reserved function names", path: "docs/spec/src/06-items/_index.md", rule: Some("6.0:5") }],
    };
    DUPLICATE_FUNCTION_DEFINITION = 436 => {
        explanation: "One source file defines the same top-level function name more than once, or reuses a top-level name across functions, constants, and user-defined types. E0436 covers both function/function duplicates and collisions between different top-level declaration kinds; the metadata name and code are shared intentionally.",
        likely_cause: "A declaration was copied, renamed to an existing item, or generated twice in one module. Remove or rename one declaration. The same spelling may be used independently in another source file because top-level names are module-scoped.",
        examples: [
            ErrorCodeExample { title: "Define a function twice in one module", source: "fn answer() -> i32 { 40 }\nfn answer() -> i32 { 42 }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Give top-level functions distinct names", source: "fn base() -> i32 { 40 }\nfn answer() -> i32 { base() + 2 }\nfn main() -> i32 { answer() }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Module-scoped top-level name uniqueness", path: "docs/spec/src/10-modules/05-program-composition.md", rule: Some("10.5:1") }],
    };
    MOVE_OUT_OF_INOUT = 437 => {
        explanation: "A function tries to move a non-Copy value, or a non-Copy part of one, out of an `inout` parameter. The callee has exclusive mutable access to caller-owned storage but does not own that storage's value, so moving it would leave the caller with an invalid or partially moved value.",
        likely_cause: "The parameter was returned, assigned to a new owner, or passed to a by-value parameter. Mutate or replace it in place, forward it to another `inout` parameter, borrow it, or read only Copy fields. Move-then-reinitialize is also rejected because Rue does not track reinitialization of `inout` parameters.",
        examples: [
            ErrorCodeExample { title: "Move an inout value to a new owner", source: "struct Item { value: i32 }\nfn take(item: Item) -> i32 { item.value }\nfn inspect(inout item: Item) -> i32 { take(item) }\nfn main() -> i32 {\n    let mut item = Item { value: 42 };\n    inspect(inout item)\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Read a Copy field without moving the inout value", source: "struct Item { value: i32 }\nfn inspect(inout item: Item) -> i32 { item.value }\nfn main() -> i32 {\n    let mut item = Item { value: 42 };\n    inspect(inout item)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Inout parameters cannot be moved out", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:35") }],
    };
    // 438 (BY_REF_ARG_NOT_PLAIN_VARIABLE) is retired: by-ref arguments may
    // now be field/index projections (RUE-143). Do not reuse the number.
    // 439-441 are reserved by in-flight work; next free code is 444.
    MOVE_SELF_OUT_OF_DESTRUCTOR = 442 => {
        explanation: "A user-defined destructor moves its whole `self` value to a new owner. Rue runs a destructor before automatically dropping the value's remaining contents; if `self` escaped to another owner, that owner would later drop it and re-enter the same destructor.",
        likely_cause: "The destructor passes `self` by value, returns it, binds it to another owned variable, or invokes a by-value receiver method. Keep `self` in place and perform cleanup by reading Copy fields, borrowing, or mutating in place instead; moving a non-Copy field is separately rejected.",
        examples: [
            ErrorCodeExample { title: "Move self into a call from its destructor", source: "struct Resource { value: i32 }\nfn consume(resource: Resource) -> i32 { resource.value }\ndrop fn Resource(self) { consume(self); }\nfn main() -> i32 {\n    let resource = Resource { value: 42 };\n    resource.value\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Read self without moving it", source: "struct Resource { value: i32 }\ndrop fn Resource(self) { @dbg(self.value); }\nfn main() -> i32 {\n    let resource = Resource { value: 42 };\n    resource.value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Destructors cannot move self", path: "docs/spec/src/03-types/09-destructors.md", rule: Some("3.9:33") }],
    };
    LINEAR_VALUE_NOT_CONSUMED_ON_ALL_PATHS = 443 => {
        explanation: "A linear value is consumed along some reachable control-flow paths but remains live along others that leave its scope. Linear obligations must be discharged on every path; otherwise an unconsumed path would implicitly drop the value.",
        likely_cause: "An `if`, `match`, loop, or early exit consumes the value in only some alternatives. Restructure the control flow so every path that leaves the scope passes, returns, or otherwise moves the linear value to a consumer.",
        examples: [
            ErrorCodeExample { title: "Consume a linear value in only one branch", source: "linear struct Token { value: i32 }\nfn consume(token: Token) -> i32 { token.value }\nfn main() -> i32 {\n    let token = Token { value: 42 };\n    if true { consume(token) } else { 0 }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Consume the linear value in every branch", source: "linear struct Token { value: i32 }\nfn consume(token: Token) -> i32 { token.value }\nfn main() -> i32 {\n    let token = Token { value: 42 };\n    if true { consume(token) } else { consume(token) }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Linear consumption on every control-flow path", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:50") }],
    };
    // 444-455 are reserved by in-flight work.
    MOVE_FIELD_OUT_OF_DESTRUCTOR_TYPE = 456 => {
        explanation: "Rue rejected a move of a field out of a value whose type has a user-defined destructor. The destructor and the automatic field cleanup that follows it must receive the whole value, so moving one field away would let them observe or drop moved-from storage. Moving the whole value is still allowed, as are `borrow` and `inout` access to its fields.",
        likely_cause: "A field with move semantics was assigned to a new binding, returned, or passed to a by-value parameter while one of its enclosing values has a user-defined destructor. Keep the field in place and borrow it, read only Copy data from it, or move the whole enclosing value to transfer ownership together with its destructor.",
        examples: [
            ErrorCodeExample { title: "Move a field away from a destructor-bearing value", source: "struct Payload { value: i32 }\nstruct Resource { payload: Payload }\ndrop fn Resource(self) { @dbg(self.payload.value); }\nfn main() -> i32 {\n    let resource = Resource { payload: Payload { value: 42 } };\n    let payload = resource.payload;\n    payload.value\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Read Copy data without moving the field", source: "struct Payload { value: i32 }\nstruct Resource { payload: Payload }\ndrop fn Resource(self) { @dbg(self.payload.value); }\nfn main() -> i32 {\n    let resource = Resource { payload: Payload { value: 42 } };\n    resource.payload.value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Fields of destructor-bearing values cannot be moved", path: "docs/spec/src/03-types/09-destructors.md", rule: Some("3.9:34") }],
    };
    COPY_STRUCT_WITH_DESTRUCTOR = 457 => {
        explanation: "A struct declared `@copy` also has a user-defined destructor. Copy values may be duplicated implicitly and those copies are not tracked as ownership transfers, so running the destructor for every copy could clean up the same logical resource more than once.",
        likely_cause: "A `drop fn` was added for a struct already marked `@copy`, or `@copy` was added to a resource-owning type. Remove `@copy` so each value has one tracked owner, or remove the destructor when the type is genuinely safe to duplicate and requires no user-defined cleanup.",
        examples: [
            ErrorCodeExample { title: "Give a Copy struct a destructor", source: "@copy\nstruct Resource { value: i32 }\ndrop fn Resource(self) { @dbg(self.value); }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Keep the destructor on a move-only struct", source: "struct Resource { value: i32 }\ndrop fn Resource(self) { @dbg(self.value); }\nfn main() -> i32 {\n    let resource = Resource { value: 42 };\n    resource.value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Copy types cannot have user-defined destructors", path: "docs/spec/src/03-types/09-destructors.md", rule: Some("3.9:31") }],
    };
    // 458-459 are reserved by in-flight work.
    PRIVATE_UNQUALIFIED_ACCESS = 460 => {
        explanation: "A comptime type constructor reached through a module binding was applied in a type annotation from outside its defining directory, but the constructor is private. Type-position application performs the same visibility check as other cross-module access; the historical metadata name `PRIVATE_UNQUALIFIED_ACCESS` does not make unqualified lookup or ordinary private member access part of E0460.",
        likely_cause: "A type annotation names a module-qualified function returning `type`, such as `lib.Secret(i32)`, but that function lacks `pub` and the referencing file is in another directory. Mark the constructor `pub` when it is part of the module's interface, or keep the use within the constructor's defining directory.",
        examples: [
            ErrorCodeExample { title: "Apply a private type constructor across directories", source: "// --- main.rue\nconst lib = @import(\"sub/lib.rue\");\nfn main() -> i32 {\n    let value: lib.Secret(i32) = lib.make();\n    value.item\n}\n// --- sub/lib.rue\nfn Secret(comptime T: type) -> type { struct { item: T } }\npub fn make() -> Secret(i32) {\n    let S = Secret(i32);\n    S { item: 42 }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Export the type constructor", source: "// --- main.rue\nconst lib = @import(\"sub/lib.rue\");\nfn main() -> i32 {\n    let value: lib.Secret(i32) = lib.make();\n    value.item\n}\n// --- sub/lib.rue\npub fn Secret(comptime T: type) -> type { struct { item: T } }\npub fn make() -> Secret(i32) {\n    let S = Secret(i32);\n    S { item: 42 }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Type-constructor visibility exception", path: "docs/spec/src/10-modules/03-visibility.md", rule: Some("10.3:7") },
            ErrorCodeReference { title: "Module-qualified comptime type constructors", path: "docs/spec/src/10-modules/04-module-bindings.md", rule: Some("10.4:16") },
        ],
    };
    CONST_INITIALIZER_CYCLE = 461 => {
        explanation: "The dependency graph of constant initializers contains a cycle, so there is no first constant Rue can evaluate. Declaration order does not break the cycle: forward references are allowed, but direct self-reference and indirect cycles are compile-time errors.",
        likely_cause: "Two or more constants refer to one another through their initializers, or a constant refers to itself. Replace one dependency with a concrete base value or restructure the initializers into an acyclic dependency chain.",
        examples: [
            ErrorCodeExample { title: "Create a constant-initializer cycle", source: "const FIRST: i32 = SECOND + 1;\nconst SECOND: i32 = FIRST - 1;\nfn main() -> i32 { FIRST }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Give the dependency chain a base value", source: "const FIRST: i32 = SECOND + 1;\nconst SECOND: i32 = 41;\nfn main() -> i32 { FIRST }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Constant initializers must be acyclic", path: "docs/spec/src/06-items/05-constants.md", rule: Some("6.5:8") }],
    };
    // 462-473 are reserved by in-flight work.
    LINEAR_FIELD_DROPPED_BY_DESTRUCTURE = 474 => {
        explanation: "A field access on a struct declared `linear` consumes and destructures the smallest enclosing declared-linear place. Rue rejects that access when the residue contains another linear value, because destroying the residue would silently discard its must-consume obligation. A struct that is only linear by infection instead uses ordinary partial-move rules.",
        likely_cause: "Code projects one field from a declared-linear struct while a sibling field, nested field, or residual array element also carries a linear value. Consume the whole declared-linear value, or restructure the operation so every linear component is explicitly consumed.",
        examples: [
            ErrorCodeExample { title: "Destructure a declared-linear value with linear residue", source: "linear struct Token { value: i32 }\nlinear struct Holder { token: Token, tag: i32 }\nfn main() -> i32 {\n    let holder = Holder { token: Token { value: 1 }, tag: 41 };\n    holder.tag\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Consume the declared-linear value explicitly", source: "linear struct Token { value: i32 }\nlinear struct Holder { token: Token, tag: i32 }\nfn main() -> i32 {\n    let holder = Holder { token: Token { value: 1 }, tag: 41 };\n    @drop(holder);\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Declared-linear destructuring cannot drop linear residue", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:60") }],
    };
    CONST_MISSING_TYPE_ANNOTATION = 475 => {
        explanation: "A value constant must declare its type explicitly. Rue does not infer a type for value constants; module bindings and callable function aliases are different constant forms and remain unannotated because no source-level type names those values.",
        likely_cause: "A declaration such as `const LIMIT = 42;` supplies a runtime value but omits the `: Type` annotation. Add the intended type after the constant name; do not add an annotation to an `@import` module binding or callable alias.",
        examples: [
            ErrorCodeExample { title: "Omit a value constant's type", source: "const LIMIT = 42;\nfn main() -> i32 { LIMIT }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Annotate the value constant", source: "const LIMIT: i32 = 42;\nfn main() -> i32 { LIMIT }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Value constants require type annotations", path: "docs/spec/src/06-items/05-constants.md", rule: Some("6.5:4") }],
    };
    // 476-477 are reserved by in-flight work.
    LINEAR_VALUE_DISCARDED = 478 => {
        explanation: "A discarded expression result is dropped rather than transferred to a consumer. Rue rejects discarding a value whose type carries a linear obligation, including with a bare expression statement or `let _ = value;`, because an implicit drop is not consumption. `@drop(value)` is the explicit-discard operation and consumes its operand.",
        likely_cause: "A function call or other expression produces a linear value but its result is ignored, or a wildcard `let` was used in an attempt to consume it. Pass or return the value, bind and consume it, or use `@drop` when intentional destruction is the desired consumption.",
        examples: [
            ErrorCodeExample { title: "Discard a linear value with a wildcard binding", source: "linear struct Token { value: i32 }\nfn make() -> Token { Token { value: 42 } }\nfn main() -> i32 {\n    let _ = make();\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Explicitly drop the linear value", source: "linear struct Token { value: i32 }\nfn make() -> Token { Token { value: 42 } }\nfn main() -> i32 {\n    let token = make();\n    @drop(token);\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Wildcard bindings discard rather than consume", path: "docs/spec/src/05-statements/01-let-statements.md", rule: Some("5.1:16") },
            ErrorCodeReference { title: "Explicit drop consumes its operand", path: "docs/spec/src/03-types/09-destructors.md", rule: Some("3.9:37") },
        ],
    };
    // 479 is reserved by in-flight work.
    ASSIGN_TO_PARTIALLY_MOVED_ARRAY = 480 => {
        explanation: "After a non-Copy array element is moved out, Rue tracks the array as partially moved. Assigning to an element, or through an element to one of its fields, cannot re-establish per-element ownership and is rejected even when the assignment targets the same constant index. Whole-array reinitialization restores ownership for an affine array; when the element type carries a linear value, it is legal only after every element has first been consumed.",
        likely_cause: "Code moves an element from an array and later writes an element or nested field of that array. For an affine array, replace the whole array value instead. For an array carrying linear values, explicitly consume every remaining live element before whole-array reinitialization, or restructure the code so no element is moved before the write.",
        examples: [
            ErrorCodeExample { title: "Write the element that was moved out", source: "struct Item { value: i32 }\nfn take(item: Item) -> i32 { item.value }\nfn main() -> i32 {\n    let mut items = [Item { value: 1 }, Item { value: 2 }];\n    let first = take(items[0]);\n    items[0] = Item { value: 9 };\n    first\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Reinitialize the whole array", source: "struct Item { value: i32 }\nfn take(item: Item) -> i32 { item.value }\nfn main() -> i32 {\n    let mut items = [Item { value: 1 }, Item { value: 2 }];\n    let first = take(items[0]);\n    items = [Item { value: 9 }, Item { value: 10 }];\n    first + items[0].value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Partially moved arrays reject element writes", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:72") },
            ErrorCodeReference { title: "Linear array reinitialization requires every element to be consumed", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:77") },
        ],
    };
    /// An array length `[T; N]` where `N` is not a usable compile-time constant
    /// (a runtime variable, a negative/non-integer value, or an undefined
    /// name). Named lengths must resolve via the const evaluator (RUE-16).
    INVALID_ARRAY_LENGTH = 481 => {
        explanation: "An array length must be a non-negative integer value known at compile time. Integer literals, annotated integer constants, comptime parameters, and comptime-evaluable function calls may provide the length; runtime variables, undefined names, non-integer values, and negative results cannot.",
        likely_cause: "The expression after the semicolon in `[T; N]` depends on runtime state, resolves to a value other than a non-negative integer, or names a missing or unannotated constant. Replace it with a valid compile-time integer expression and annotate any value constant it uses.",
        examples: [
            ErrorCodeExample { title: "Use a negative array length", source: "const LENGTH: i32 = -1;\nfn main() -> i32 {\n    let values: [i32; LENGTH] = [];\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use an annotated non-negative constant", source: "const LENGTH: i32 = 2;\nfn main() -> i32 {\n    let values: [i32; LENGTH] = [20, 22];\n    values[0] + values[1]\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Array lengths are non-negative compile-time integers", path: "docs/spec/src/03-types/05-array-types.md", rule: Some("3.5:2") }],
    };
    /// Source nests deeper than the compiler's fixed recursion limit
    /// (`MAX_NESTING_DEPTH`). A parser/AstGen guard reports this instead of
    /// overflowing the stack on pathologically nested input (RUE-42). It is a
    /// resource-limit diagnostic rather than a struct/enum error, but the
    /// reserved code lives in this block.
    NESTING_LIMIT_EXCEEDED = 482 => {
        explanation: "The source exceeds Rue's fixed syntactic nesting limit of 256 levels. The parser and syntax-to-IR lowering enforce this resource bound and emit E0482 instead of allowing deeply recursive syntax processing to overflow the compiler stack.",
        likely_cause: "Generated or hand-written code contains more than 256 nested parentheses, blocks, types, field or method chains, operators, or `else if` branches. Flatten the expression, introduce intermediate bindings or helper functions, or correct a generator that produced pathological nesting.",
        examples: [ErrorCodeExample { title: "Exceed the syntactic nesting limit", source: "fn main() -> i32 { ((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((7)))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))) }", outcome: ErrorCodeExampleOutcome::EmitsThisCode }],
        references: [
            ErrorCodeReference { title: "Syntactic nesting depth limit", path: "docs/spec/src/appendices/C-implementation-limits.md", rule: Some("C.6:3") },
            ErrorCodeReference { title: "Implementation limits fail diagnostically", path: "docs/spec/src/appendices/C-implementation-limits.md", rule: Some("C.1:2") },
        ],
    };
    /// A struct or enum (transitively) contains itself by value with no
    /// pointer indirection, violating recursive-type well-formedness (RUE-264).
    /// Analogous to Rust's E0072 and a sibling of [`Self::CONST_INITIALIZER_CYCLE`]
    /// (E0461) for the type-definition graph.
    RECURSIVE_TYPE_INFINITE_SIZE = 483 => {
        explanation: "A struct or enum contains itself by value, directly or through other inline fields, payloads, or array elements. A non-zero by-value cycle has no finite size or layout. For uniform structural well-formedness, Rue also treats zero-length array edges as recursive-type edges. A raw pointer has a fixed size independent of its pointee, so pointer indirection can break the inline containment cycle.",
        likely_cause: "A recursive data structure stores its next node directly, or two aggregate types contain one another without indirection. Change at least one edge of every by-value cycle to a pointer or redesign the representation so recursive values are stored out of line.",
        examples: [
            ErrorCodeExample { title: "Contain a type directly inside itself", source: "struct Node { next: Node, value: i32 }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Break recursion with pointer indirection", source: "struct Node { next: ptr const Node, value: i32 }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Recursive value types require indirection", path: "docs/spec/src/03-types/_index.md", rule: Some("3.0:5") }],
    };
    /// A pattern binds the same identifier more than once (e.g. an enum
    /// payload pattern `Rect(w, w)`). Every binding in a pattern must be a
    /// fresh name (spec 4.7:30); reusing one silently shadows the earlier
    /// binding and discards its value (RUE-269, analogous to Rust E0416).
    DUPLICATE_PATTERN_BINDING = 484 => {
        explanation: "Each named payload position in one match pattern introduces a fresh binding. Reusing an identifier in the same pattern would shadow an earlier payload and lose access to that value, so Rue rejects the pattern. The wildcard `_` introduces no binding and may repeat.",
        likely_cause: "Two fields of an enum payload pattern were given the same variable name, often when their roles or types are similar. Give every value that must remain accessible a distinct name, or use `_` for a payload that may legally be discarded.",
        examples: [
            ErrorCodeExample { title: "Bind two payloads to one name", source: "enum Shape { Rect(i32, i32) }\nfn main() -> i32 {\n    match Shape.Rect(20, 22) {\n        Shape.Rect(side, side) => side,\n    }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Bind both payloads distinctly", source: "enum Shape { Rect(i32, i32) }\nfn main() -> i32 {\n    match Shape.Rect(20, 22) {\n        Shape.Rect(width, height) => width + height,\n    }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Payload patterns bind fresh names", path: "docs/spec/src/04-expressions/07-match-expressions.md", rule: Some("4.7:30") }],
    };
    /// `@raw`/`@raw_mut` applied to a non-place operand (literal, arithmetic,
    /// call result). A raw pointer must address an addressable place (spec
    /// 9.1:12, ADR-0028); taking the "address" of a temporary value would
    /// reinterpret the value's bits as a pointer (RUE-274).
    RAW_REQUIRES_PLACE = 485 => {
        explanation: "`@raw` and `@raw_mut` form pointers to existing storage, so their operand must be an addressable place such as a local variable, field, or array element. Literals, arithmetic expressions, and call results are temporary values rather than places and cannot supply a stable address. Raw-pointer operations must also appear in a `checked` block.",
        likely_cause: "Code applies `@raw` or `@raw_mut` directly to a computed value. Store the value in a local first and take that place's address inside `checked`, or point at an existing field or array element whose lifetime covers the pointer use.",
        examples: [
            ErrorCodeExample { title: "Take the address of a temporary value", source: "fn main() -> i32 {\n    let pointer: ptr const i32 = checked { @raw(40 + 2) };\n    checked { @ptr_read(pointer) }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Take the address of a local place", source: "fn main() -> i32 {\n    let value: i32 = 42;\n    let pointer: ptr const i32 = checked { @raw(value) };\n    checked { @ptr_read(pointer) }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Raw pointer constructors address places", path: "docs/spec/src/09-unchecked-code/02-intrinsics.md", rule: Some("9.2:6a") },
            ErrorCodeReference { title: "Raw pointer operations require checked", path: "docs/spec/src/09-unchecked-code/01-syntax.md", rule: Some("9.1:12") },
        ],
    };
    /// A match-arm payload position that binds nothing — an explicit `_`
    /// discard, or a position covered by the all-wildcard bare variant
    /// pattern `E.A` — names a payload field whose type carries a linear
    /// value (RUE-1592, spec 4.7:30). Such a position is a fresh *unnameable*
    /// binding (formal core §2 elaboration note), so its must-consume
    /// obligation (3.8:52) could never be discharged. Bind the field by name
    /// and consume it — or `@drop` it (E0478's escape hatch, RUE-187).
    LINEAR_PAYLOAD_DISCARDED = 486 => {
        explanation: "A wildcard payload position still receives the matched value, but gives the program no name with which to consume it. When that payload carries a linear obligation, either an explicit `_` or the equivalent bare variant pattern would silently discard the value, so Rue rejects the pattern.",
        likely_cause: "A match arm uses `_` for a linear payload, or uses a bare variant name whose payload includes a linear value. Bind each linear payload by name and consume it in the arm; `_` remains valid for payloads that do not carry linear values.",
        examples: [
            ErrorCodeExample { title: "Discard a linear match payload", source: "linear struct Token { value: i32 }\nenum Event { Token(Token), Empty }\nfn main() -> i32 {\n    match Event.Token(Token { value: 42 }) {\n        Event.Token(_) => 1,\n        Event.Empty => 0,\n    }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Bind and consume the linear payload", source: "linear struct Token { value: i32 }\nenum Event { Token(Token), Empty }\nfn main() -> i32 {\n    match Event.Token(Token { value: 42 }) {\n        Event.Token(token) => { @drop(token); 1 },\n        Event.Empty => 0,\n    }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Match payload bindings", path: "docs/spec/src/04-expressions/07-match-expressions.md", rule: Some("4.7:30") },
            ErrorCodeReference { title: "Linear values must be consumed", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:50") },
        ],
    };
    /// A slice type `[T]` was written in return position (`fn f(...) -> [T]`).
    /// A slice is a *second-class* fat-pointer view (ADR-0037, ADR-0043,
    /// RUE-322): it is valid only in argument position and may not be
    /// returned, since the view would outlive the borrow it aliases.
    SLICE_RETURN_NOT_ALLOWED = 487 => {
        explanation: "A slice `[T]` is a second-class borrowed view, not an owning value. It is valid only as a function parameter and cannot be a return type, because returning it would allow the view to outlive the caller-owned storage that it aliases.",
        likely_cause: "A function declares `[T]` as its return type, often while trying to return a view into an array parameter. Return an owning array or another first-class value instead, or perform the operation while the slice is available as a parameter.",
        examples: [
            ErrorCodeExample { title: "Return a second-class slice", source: "fn view(borrow values: [i32]) -> [i32] { values }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use the slice within the call", source: "fn first(borrow values: [i64]) -> i32 { @intCast(values[0]) }\nfn main() -> i32 {\n    let values: [i64; 2] = [42, 0];\n    first(borrow values)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Slices cannot be returned", path: "docs/spec/src/07-arrays/02-slices.md", rule: Some("7.2:4") }],
    };
    /// A slice type `[T]` was written as a struct field type. A slice is
    /// second-class (ADR-0037, ADR-0043, RUE-322) and cannot be stored in an
    /// aggregate — storing it would let the view escape its borrow's scope.
    SLICE_IN_AGGREGATE_FIELD = 488 => {
        explanation: "A slice `[T]` is a second-class view whose lifetime is limited to a function call. Storing one in a struct field or enum payload would turn that temporary view into aggregate state and could let it escape the storage it borrows.",
        likely_cause: "A struct field or enum payload was declared with slice type `[T]`. Store an owning fixed array `[T; N]` or another owning container, and accept a slice only at the function boundary where it is used.",
        examples: [
            ErrorCodeExample { title: "Store a slice in a struct", source: "struct View { values: [i32] }\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Store an owning fixed array", source: "struct Values { values: [i32; 2] }\nfn main() -> i32 {\n    let values = Values { values: [20, 22] };\n    values.values[0] + values.values[1]\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Slices cannot be aggregate fields", path: "docs/spec/src/07-arrays/02-slices.md", rule: Some("7.2:5") }],
    };
    /// A slice type `[T]` was written in a binding position other than a
    /// parameter — a `let` local or a `const`. A slice is second-class
    /// (ADR-0037, ADR-0043, RUE-322): it may only name a function parameter,
    /// so it cannot be bound past its argument scope.
    SLICE_ESCAPES_SCOPE = 489 => {
        explanation: "A slice `[T]` may name only a function parameter. Giving a local or constant binding slice type would extend a second-class borrowed view beyond the argument scope whose access rules keep the aliased storage valid.",
        likely_cause: "A `let` or `const` declaration has an explicit `[T]` annotation, commonly to preserve a view received by a function. Keep the view in its parameter binding and use it there, or bind an owning array or container instead.",
        examples: [
            ErrorCodeExample { title: "Bind a slice to a local", source: "fn main() -> i32 {\n    let values = [20, 22];\n    let view: [i32] = values;\n    view[0]\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Read a slice parameter directly", source: "fn sum(borrow values: [i64]) -> i32 { @intCast(values[0] + values[1]) }\nfn main() -> i32 {\n    let values: [i64; 2] = [20, 22];\n    sum(borrow values)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Slices cannot escape argument scope", path: "docs/spec/src/07-arrays/02-slices.md", rule: Some("7.2:6") }],
    };
    /// `@field_ptr(...)` was applied to something other than a field-access
    /// expression `s.field` (RUE-301). `@field_ptr` is *compiler-mediated
    /// field access*: it forms a raw pointer to the field the compiler placed,
    /// so its operand MUST name a struct field. Use `@raw`/`@raw_mut` for the
    /// address of a whole variable or array element.
    FIELD_PTR_REQUIRES_FIELD = 490 => {
        explanation: "`@field_ptr` asks the compiler for the address of a field under its chosen struct layout, so its operand must syntactically be a field-access place such as `value.field`. A whole variable, array element, or computed value does not identify a struct field for this layout-mediated operation.",
        likely_cause: "Code passed a local or another non-field expression to `@field_ptr`. Select the intended struct field, or use `@raw` or `@raw_mut` when the goal is to address a whole place. Raw-pointer intrinsics must appear in a `checked` block.",
        examples: [
            ErrorCodeExample { title: "Request a field pointer for a whole value", source: "struct Pair { value: i32 }\nfn main() -> i32 {\n    let pair = Pair { value: 42 };\n    let pointer = checked { @field_ptr(pair) };\n    checked { @ptr_read(pointer) }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Request a pointer to a named field", source: "struct Pair { value: i32 }\nfn main() -> i32 {\n    let pair = Pair { value: 42 };\n    let pointer: ptr mut i32 = checked { @field_ptr(pair.value) };\n    checked { @ptr_read(pointer) }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Field pointer operands", path: "docs/spec/src/09-unchecked-code/02-intrinsics.md", rule: Some("9.2:17") }],
    };
    /// A function or method parameter list names the same parameter twice
    /// (e.g. `fn f(x: i32, x: i32)`). Every parameter in a single list must
    /// have a distinct name (spec 6.1); a repeated name silently shadows the
    /// earlier binding on a first-wins basis, so the declaration is rejected
    /// (RUE-349, a sibling of [`Self::DUPLICATE_PATTERN_BINDING`] (E0484) and
    /// analogous to Rust's E0415).
    DUPLICATE_PARAMETER = 491 => {
        explanation: "Every named parameter in one function or method parameter list must have a distinct name. Repeating a name would make one argument inaccessible by shadowing its binding, so Rue rejects the declaration rather than choosing one occurrence.",
        likely_cause: "Two parameters were given the same name, often after copying a declaration or when two arguments have the same type. Rename them to express their distinct roles; a method receiver does not conflict with the ordinary parameter-name set.",
        examples: [
            ErrorCodeExample { title: "Repeat a function parameter name", source: "fn add(value: i32, value: i32) -> i32 { value }\nfn main() -> i32 { add(20, 22) }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Give parameters distinct names", source: "fn add(left: i32, right: i32) -> i32 { left + right }\nfn main() -> i32 { add(20, 22) }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Parameter names are distinct", path: "docs/spec/src/06-items/01-functions.md", rule: Some("6.1:34") }],
    };
    /// A string literal assigned to a fixed-capacity string `Str(N)` does not
    /// fit: its UTF-8 byte length exceeds the capacity `N` (ADR-0043 Phase 5,
    /// RUE-326). `Str(N)` is the fixed string rung (`[u8; N]` + UTF-8) with no
    /// heap, so an over-long literal cannot be stored — the fit is checked at
    /// compile time.
    STR_FIXED_CAPACITY_EXCEEDED = 492 => {
        explanation: "`Str(N)` stores at most `N` UTF-8 bytes in its inline buffer. When a string literal's encoded byte length exceeds that fixed capacity, the value cannot be represented without truncation, so construction is rejected at compile time.",
        likely_cause: "A literal assigned or passed as `Str(N)` is longer than `N` bytes. Increase the declared capacity, shorten the literal, or use the growable `StrBuf` type when the required capacity is not fixed.",
        examples: [
            ErrorCodeExample { title: "Exceed a fixed string capacity", source: "fn main() -> i32 {\n    let value: Str(3) = \"hello\";\n    @intCast(value.len())\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Choose sufficient fixed capacity", source: "fn main() -> i32 {\n    let value: Str(5) = \"hello\";\n    @intCast(value.len())\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Fixed string literal capacity", path: "docs/spec/src/03-types/07-string-type.md", rule: Some("3.7:51") }],
    };
    /// Assignment to an initialized place that holds a live linear value
    /// (RUE-387). Overwriting the place would implicitly drop (silently
    /// consume) the old linear value, which linearity forbids — a theorem-5
    /// soundness hole (spec 3.9:18 overwrite-drop is carved out for linear
    /// types). The value must be moved/consumed explicitly first; the sole
    /// exception is reinitializing a place that was provably moved out on
    /// every path (the spec 3.8:55/56 reinit idiom), which holds nothing to
    /// destroy.
    LINEAR_VALUE_OVERWRITTEN = 493 => {
        explanation: "Assigning to a place normally drops its previous live value before storing the replacement. Rue cannot perform that implicit drop when the place carries a linear value, because linear values must be consumed explicitly. Reinitialization is legal only after the destination was provably moved out on every incoming path.",
        likely_cause: "An assignment replaces an initialized linear local, field, or array element. Consume or move the old value first; if the place is moved out on every path, assigning a replacement then reinitializes empty storage instead of overwriting a live value.",
        examples: [
            ErrorCodeExample { title: "Overwrite a live linear local", source: "linear struct Token { value: i32 }\nfn main() -> i32 {\n    let mut token = Token { value: 1 };\n    token = Token { value: 2 };\n    @drop(token);\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Reinitialize after consuming the old value", source: "linear struct Token { value: i32 }\nfn consume(token: Token) { @drop(token); }\nfn main() -> i32 {\n    let mut token = Token { value: 1 };\n    consume(token);\n    token = Token { value: 2 };\n    @drop(token);\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Linear assignment and reinitialization", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:77") },
            ErrorCodeReference { title: "Assignment drops overwritten values", path: "docs/spec/src/03-types/09-destructors.md", rule: Some("3.9:18") },
        ],
    };
    /// Assignment to an `inout` parameter (or a place rooted at one) whose
    /// type carries a linear value (RUE-387). The parameter names the
    /// caller's storage, which holds a live linear value; reassigning it
    /// would implicitly drop that value in the caller. An `inout` place can
    /// never be proven moved-out (moving out of a by-ref binding is itself
    /// rejected), so a linear `inout` assignment is always ill-formed.
    LINEAR_VALUE_OVERWRITTEN_THROUGH_INOUT = 494 => {
        explanation: "An `inout` parameter aliases the caller's initialized storage. Assigning a linear value to that parameter or a linear place rooted at it would implicitly drop the caller's live value. Because a by-reference binding cannot be moved out to establish empty storage, this assignment is always rejected.",
        likely_cause: "A function tries to replace a linear `inout` argument. Pass ownership by value and return a replacement, mutate non-linear fields in place, or otherwise arrange for the caller to consume the old value explicitly rather than overwriting it through the loan.",
        examples: [
            ErrorCodeExample { title: "Overwrite a linear inout parameter", source: "linear struct Token { value: i32 }\nfn replace(inout token: Token) { token = Token { value: 42 }; }\nfn main() -> i32 {\n    let mut token = Token { value: 1 };\n    replace(inout token);\n    @drop(token);\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Mutate a non-linear field through inout", source: "linear struct Token { value: i32 }\nfn update(inout token: Token) { token.value = 42; }\nfn main() -> i32 {\n    let mut token = Token { value: 1 };\n    update(inout token);\n    @drop(token);\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Linear assignment cannot overwrite a live value", path: "docs/spec/src/03-types/08-move-semantics.md", rule: Some("3.8:77") }],
    };
    /// A string *buffer* — `StrBuf` (growable) or `Str(N)` (fixed) — was used
    /// where a *first-class* `str` value is required: a bare `str` parameter,
    /// a `str` binding, a `str` return, or a `str` struct field (ADR-0043
    /// two-types model, RUE-386). A first-class `str` is static-backed and may
    /// escape; a buffer's bytes live in caller-owned local/heap storage, so
    /// letting it become a first-class `str` produces a dangling view once the
    /// buffer is dropped (the verified RUE-386 segfault). A buffer coerces only
    /// to a second-class `borrow str` / `inout str` view.
    BUFFER_NOT_FIRST_CLASS_STR = 495 => {
        explanation: "`StrBuf` and `Str(N)` own mutable string storage, while first-class `str` values are static-backed and may freely escape. Treating a buffer as first-class `str` could leave a copied view pointing at storage after its owner is dropped, so buffers coerce only to second-class `borrow str` or `inout str` views.",
        likely_cause: "A string buffer is passed to a bare `str` parameter, returned as `str`, or stored in a `str` binding or field. Use `borrow str` for read-only access, `inout str` for exclusive local-buffer access, or keep the owning buffer type across the boundary.",
        examples: [
            ErrorCodeExample { title: "Pass a buffer as first-class str", source: "fn length(value: str) -> u64 { value.len() }\nfn main() -> i32 {\n    let value: Str(8) = \"hello\";\n    @intCast(length(value))\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Borrow the buffer for string access", source: "fn length(borrow value: str) -> u64 { value.len() }\nfn main() -> i32 {\n    let value: Str(8) = \"hello\";\n    @intCast(length(borrow value))\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Buffers are not first-class strings", path: "docs/spec/src/03-types/07-string-type.md", rule: Some("3.7:59") }],
    };
    /// A first-class / static-backed `str` value was supplied as the operand
    /// of an `inout str` parameter (ADR-0043 two-types model, RUE-386). An
    /// exclusive `str` view requires *local* provenance — a `StrBuf`/`Str(N)`
    /// buffer the caller owns — because a static `str` lives in read-only
    /// `.rodata` (exclusive mutation would fault) and, being `Copy`, can have
    /// two roots over one static buffer that per-root exclusivity cannot see.
    INOUT_STR_REQUIRES_LOCAL_BUFFER = 496 => {
        explanation: "An `inout str` parameter is an exclusive view into caller-owned mutable string storage, so its operand must have local-buffer provenance from `StrBuf` or `Str(N)`. A first-class `str` is static-backed, immutable, and copyable; granting exclusive access could write read-only memory and cannot establish unique ownership.",
        likely_cause: "A first-class `str`, usually a string literal or `str` binding, is passed with `inout`. Use `borrow str` when only reading static text, or copy the content into a local `StrBuf` or `Str(N)` before requesting an exclusive view.",
        examples: [
            ErrorCodeExample { title: "Request an exclusive view of static str", source: "fn length(inout value: str) -> u64 { value.len() }\nfn main() -> i32 {\n    let mut value: str = \"hello\";\n    @intCast(length(inout value))\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use a local buffer for an exclusive view", source: "fn length(inout value: str) -> u64 { value.len() }\nfn main() -> i32 {\n    let mut value: Str(8) = \"hello\";\n    @intCast(length(inout value))\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Exclusive string views require local provenance", path: "docs/spec/src/03-types/07-string-type.md", rule: Some("3.7:60") }],
    };
    /// A borrowed `str` *view* — the binding of a `borrow str` / `inout str`
    /// parameter — was used where a first-class `str` value is required
    /// (returned, stored in a struct field, bound to a `str` local, or passed
    /// to a bare `str` parameter) (ADR-0043 two-types model, RUE-386). A view
    /// is second-class: it may only be read (`.len()`, byte indexing) or
    /// re-borrowed, never escape the call as a first-class value.
    STR_VIEW_NOT_FIRST_CLASS = 497 => {
        explanation: "The binding of a `borrow str` or `inout str` parameter is a second-class view valid only for that call. It may be read or re-borrowed, but converting it to a first-class `str` by returning, storing, or rebinding it would let the view escape the lifetime of the borrowed buffer.",
        likely_cause: "A function returns a borrowed string parameter as `str`, stores it in a `str` field, or passes it to a bare `str` parameter. Keep all use within the borrow, re-borrow for another view parameter, or produce a separate owning value instead.",
        examples: [
            ErrorCodeExample { title: "Return a borrowed string view", source: "fn escape(borrow value: str) -> str { value }\nfn main() -> i32 {\n    let value: Str(5) = \"hello\";\n    let escaped = escape(borrow value);\n    @intCast(escaped.len())\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Read the string view within its call", source: "fn length(borrow value: str) -> u64 { value.len() }\nfn main() -> i32 {\n    let value: Str(8) = \"hello\";\n    @intCast(length(borrow value))\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Borrowed string views cannot escape", path: "docs/spec/src/03-types/07-string-type.md", rule: Some("3.7:59") }],
    };
    // E0498 (CONTAINER_ELEMENT_HAS_DESTRUCTOR) was retired by RUE-646: owning
    // growable containers now run each live element's drop glue before freeing
    // their buffer (Rust's `Vec<T>` discipline), so a destructor-bearing element
    // type is legal. The code number is left retired (a gap is fine) rather than
    // reused. Linear elements are still rejected — see E0499 below.
    /// An owning growable container (e.g. `ArrayBuf(T)`) was instantiated with a
    /// `linear` element type, via the `@require_droppable(T)` gate. Until
    /// container/element multiplicity propagation is designed (RUE-388), such
    /// containers cannot yet track element linearity, so a linear element would
    /// be leaked (never consumed); the instantiation is rejected instead.
    CONTAINER_ELEMENT_IS_LINEAR = 499 => {
        explanation: "An owning growable container can run ordinary drop glue for each live element, but it cannot yet propagate and enforce a linear element's exactly-once consumption obligation. `@require_droppable(T)` therefore rejects a linear element type at container instantiation rather than allowing elements to leak or be implicitly dropped.",
        likely_cause: "A generic owning container guarded by `@require_droppable` was instantiated with a linear type. Use a non-linear element type, choose a representation whose API explicitly transfers every element, or wait for container/element multiplicity propagation to be designed.",
        examples: [
            ErrorCodeExample { title: "Instantiate a droppable-gated container with a linear element", source: "fn Container(comptime T: type) -> type {\n    @require_droppable(T);\n    struct { value: T }\n}\nlinear struct Token { value: i32 }\nconst Bad = Container(Token);\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use a non-linear container element", source: "fn Container(comptime T: type) -> type {\n    @require_droppable(T);\n    struct { value: T }\n}\nconst Good = Container(i32);\nfn main() -> i32 {\n    let value = Good { value: 42 };\n    value.value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Owning-container element-type gate intrinsic", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:5a") }],
    };

    // ========================================================================
    // Control flow errors (E0500-E0599)
    // ========================================================================
    BREAK_OUTSIDE_LOOP = 500 => {
        explanation: "A `break` expression exits the innermost enclosing `loop` or `while` loop. Outside a loop there is no control-flow target to exit, so Rue rejects the expression instead of assigning it a destination.",
        likely_cause: "A `break` was placed in a function, conditional, or other block that is not nested inside a loop, or it was intended to leave a function. Put the expression inside the loop it should exit; use `return` when the intended target is the enclosing function.",
        examples: [
            ErrorCodeExample { title: "Break from a function body", source: "fn main() -> i32 {\n    break;\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Break from an enclosing loop", source: "fn main() -> i32 {\n    loop {\n        break;\n    }\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Break exits the innermost loop", path: "docs/spec/src/04-expressions/08-loop-expressions.md", rule: Some("4.8:7") },
            ErrorCodeReference { title: "Break and continue require a loop", path: "docs/spec/src/04-expressions/08-loop-expressions.md", rule: Some("4.8:9") },
        ],
    };
    CONTINUE_OUTSIDE_LOOP = 501 => {
        explanation: "A `continue` expression skips the rest of the current iteration of the innermost enclosing `loop` or `while` loop. Outside a loop there is no next iteration to begin, so Rue rejects the expression.",
        likely_cause: "A `continue` was placed in a function, conditional, or other block that is not nested inside a loop. Move it into the loop whose iteration should be skipped, or use another control-flow construct when execution should leave the function or conditional.",
        examples: [
            ErrorCodeExample { title: "Continue from a function body", source: "fn main() -> i32 {\n    continue;\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Continue an enclosing loop", source: "fn main() -> i32 {\n    let mut value = 0;\n    while value < 1 {\n        value = value + 1;\n        continue;\n    }\n    value\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Continue advances the innermost loop", path: "docs/spec/src/04-expressions/08-loop-expressions.md", rule: Some("4.8:8") },
            ErrorCodeReference { title: "Break and continue require a loop", path: "docs/spec/src/04-expressions/08-loop-expressions.md", rule: Some("4.8:9") },
        ],
    };
    BREAK_WITH_VALUE = 502 => {
        explanation: "Rue loops currently produce only `()` when exited by `break`. A `break` therefore cannot carry a value operand: `break expression` is rejected even when the surrounding `loop` appears in a value position.",
        likely_cause: "Code used `break value` expecting that value to become the result of a `loop`. Store the value in a mutable binding before a valueless `break`, or restructure the code so the desired value is produced after the loop.",
        examples: [
            ErrorCodeExample { title: "Break with a value operand", source: "fn main() -> i32 {\n    loop {\n        break 42;\n    }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Store the value before breaking", source: "fn main() -> i32 {\n    let mut answer = 0;\n    loop {\n        answer = 42;\n        break;\n    }\n    answer\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Loops exited by break produce unit", path: "docs/spec/src/04-expressions/08-loop-expressions.md", rule: Some("4.8:21") },
            ErrorCodeReference { title: "Break has no value operand", path: "docs/spec/src/04-expressions/08-loop-expressions.md", rule: Some("4.8:22") },
        ],
    };
    /// The `?` operator was used in a function whose return type is not an
    /// `Option` (RUE-6, ADR-0038): `?` early-returns `None`, so the enclosing
    /// function must return an `Option`.
    QUESTION_OUTSIDE_OPTION_FN = 503 => {
        explanation: "The operand of `?` is a trusted standard-library `Option`, but the enclosing function does not return a trusted standard-library `Option`. On `None`, `?` immediately returns `None` from that function, so its declared return type must be `std.option.Option(U)`. The success payload types may differ.",
        likely_cause: "An Option-producing call was followed by `?` inside a function returning a plain value, `Result`, or a user-defined Option-shaped enum. Change the function to return the standard `Option`, handle `Some` and `None` explicitly with `match`, or remove `?`.",
        examples: [
            ErrorCodeExample { title: "Propagate Option from a plain-value function", source: "fn main() -> i32 {\n    let value = @parse_i32(\"42\")?;\n    value\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Return trusted Option before propagating", source: "const option = @import(\"std/option.rue\");\nfn parse() -> option.Option(i32) {\n    let O = option.Option(i32);\n    let value = @parse_i32(\"42\")?;\n    O.Some(value)\n}\nfn main() -> i32 {\n    parse();\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Try operand and enclosing producer compatibility", path: "docs/spec/src/04-expressions/15-try-expressions.md", rule: Some("4.15:4") },
            ErrorCodeReference { title: "None returns from the enclosing function", path: "docs/spec/src/04-expressions/15-try-expressions.md", rule: Some("4.15:7") },
        ],
    };
    /// The `?` operator was applied to a value that is neither a trusted
    /// standard `Option` nor a trusted standard `Result` (RUE-6, ADR-0038).
    QUESTION_ON_NON_OPTION = 504 => {
        explanation: "The operand of `?` is neither an exact specialization of the trusted `std.option.Option` producer nor an exact specialization of the trusted `std.result.Result` producer. Only those two standard-library types have Rue's built-in try behavior; an enum with the same variants is still a distinct, ordinary type.",
        likely_cause: "The operand is a plain value, another type, or a user-defined `Some`/`None` or `Ok`/`Err` lookalike. Apply `?` to a value produced by the standard `Option` or `Result`, or handle the operand's cases explicitly.",
        examples: [
            ErrorCodeExample { title: "Apply try to a plain integer", source: "fn main() -> i32 {\n    let value = 42?;\n    value\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Apply try to a trusted Option", source: "const option = @import(\"std/option.rue\");\nfn some_value() -> option.Option(i32) {\n    let O = option.Option(i32);\n    O.Some(42)\n}\nfn propagate() -> option.Option(i32) {\n    let O = option.Option(i32);\n    let value = some_value()?;\n    O.Some(value)\n}\nfn main() -> i32 {\n    propagate();\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Trusted try operand producers", path: "docs/spec/src/04-expressions/15-try-expressions.md", rule: Some("4.15:3") }],
    };
    QUESTION_OUTSIDE_RESULT_FN = 505 => {
        explanation: "The operand of `?` is a trusted standard-library `Result`, but the enclosing function does not return a trusted standard-library `Result`. On `Err(e)`, `?` immediately returns an `Err(e)` constructed with the enclosing function's Result type, so that return type must be `std.result.Result(U, E)`.",
        likely_cause: "A Result-producing call was followed by `?` inside a function returning a plain value, `Option`, or a user-defined Result-shaped enum. Change the function to return the standard `Result`, handle `Ok` and `Err` explicitly with `match`, or remove `?`.",
        examples: [
            ErrorCodeExample { title: "Propagate Result from a plain-value function", source: "const result = @import(\"std/result.rue\");\nfn fallible() -> result.Result(i32, i32) {\n    let R = result.Result(i32, i32);\n    R.Ok(42)\n}\nfn main() -> i32 {\n    fallible()?\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Return trusted Result before propagating", source: "const result = @import(\"std/result.rue\");\nfn fallible() -> result.Result(i32, i32) {\n    let R = result.Result(i32, i32);\n    R.Ok(42)\n}\nfn run() -> result.Result(i32, i32) {\n    let R = result.Result(i32, i32);\n    let value = fallible()?;\n    R.Ok(value)\n}\nfn main() -> i32 {\n    run();\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Try operand and enclosing producer compatibility", path: "docs/spec/src/04-expressions/15-try-expressions.md", rule: Some("4.15:4") }],
    };
    QUESTION_ERR_TYPE_MISMATCH = 506 => {
        explanation: "The operand and enclosing function both use the trusted standard-library `Result`, but their error types are different. `?` returns the operand's `Err(e)` payload through the enclosing function's own Result producer, and Rue does not perform error conversion, so the two error types must be identical.",
        likely_cause: "A called operation returns `Result(T, E1)` while its caller declares `Result(U, E2)` with a different error type. Make the error types match or explicitly map the error with `match` before propagating it; `?` does not invoke a conversion.",
        examples: [
            ErrorCodeExample { title: "Propagate a different Result error type", source: "const result = @import(\"std/result.rue\");\nfn fallible() -> result.Result(i32, i32) {\n    let R = result.Result(i32, i32);\n    R.Ok(42)\n}\nfn run() -> result.Result(i32, bool) {\n    let R = result.Result(i32, bool);\n    let value = fallible()?;\n    R.Ok(value)\n}\nfn main() -> i32 {\n    let R = result.Result(i32, bool);\n    match run() { R.Ok(value) => value, R.Err(_) => 0 }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use the same Result error type", source: "const result = @import(\"std/result.rue\");\nfn fallible() -> result.Result(i32, i32) {\n    let R = result.Result(i32, i32);\n    R.Ok(42)\n}\nfn run() -> result.Result(i32, i32) {\n    let R = result.Result(i32, i32);\n    let value = fallible()?;\n    R.Ok(value)\n}\nfn main() -> i32 {\n    run();\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Result error types must be identical", path: "docs/spec/src/04-expressions/15-try-expressions.md", rule: Some("4.15:4") }],
    };

    // ========================================================================
    // Match errors (E0600-E0699)
    // ========================================================================
    NON_EXHAUSTIVE_MATCH = 600 => {
        explanation: "A `match` must cover every possible value of its scrutinee type. Boolean matches need both literal cases, enum matches need every variant, and integer matches need an irrefutable wildcard arm; a wildcard also makes a boolean or enum match exhaustive.",
        likely_cause: "One boolean literal or enum variant is missing, an integer match lists only specific values, or a match on an externally non-exhaustive enum omits the wildcard needed to cover variants that may be added outside the current module.",
        examples: [
            ErrorCodeExample { title: "Omit one boolean case", source: "fn main() -> i32 {\n    match true {\n        true => 1,\n    }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Cover both boolean cases", source: "fn main() -> i32 {\n    match true {\n        true => 1,\n        false => 0,\n    }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Match patterns must be exhaustive", path: "docs/spec/src/04-expressions/07-match-expressions.md", rule: Some("4.7:9") },
            ErrorCodeReference { title: "Exhaustiveness rules by scrutinee type", path: "docs/spec/src/04-expressions/07-match-expressions.md", rule: Some("4.7:10") },
            ErrorCodeReference { title: "External non-exhaustive enums require a wildcard", path: "docs/spec/src/06-items/03-enums.md", rule: Some("6.3:21") },
        ],
    };
    EMPTY_MATCH = 601 => {
        explanation: "A `match` with no arms is valid only when its scrutinee is an enum with zero variants. Every inhabited type has at least one possible value, so an empty arm list cannot cover it.",
        likely_cause: "A match body was left empty while being drafted, or code attempted to use an empty match with an integer, boolean, or enum that has one or more variants. Add arms covering the scrutinee or reserve the empty form for a zero-variant enum.",
        examples: [
            ErrorCodeExample { title: "Leave an integer match empty", source: "fn main() -> i32 {\n    match 0 {}\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Add an exhaustive wildcard arm", source: "fn main() -> i32 {\n    match 0 {\n        _ => 0,\n    }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Empty matches require a zero-variant enum", path: "docs/spec/src/04-expressions/07-match-expressions.md", rule: Some("4.7:26") },
            ErrorCodeReference { title: "Empty matches have never type", path: "docs/spec/src/04-expressions/07-match-expressions.md", rule: Some("4.7:27") },
        ],
    };
    INVALID_MATCH_TYPE = 602 => {
        explanation: "Rue supports `match` scrutinees whose type is an integer, `bool`, or an enum. A value of any other type has no supported pattern domain, so the match is rejected before its arms are analyzed.",
        likely_cause: "Code tries to match a string, struct, array, pointer, or another unsupported value directly. Match a supported field or discriminating value instead, or express the condition with another construct.",
        examples: [
            ErrorCodeExample { title: "Match a string value", source: "fn main() -> i32 {\n    match \"hello\" {\n        _ => 0,\n    }\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Match a supported integer value", source: "fn main() -> i32 {\n    match 1 {\n        _ => 0,\n    }\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Supported match pattern forms", path: "docs/spec/src/04-expressions/07-match-expressions.md", rule: Some("4.7:2") },
            ErrorCodeReference { title: "Patterns must match the scrutinee type", path: "docs/spec/src/04-expressions/07-match-expressions.md", rule: Some("4.7:13") },
        ],
    };

    // ========================================================================
    // Intrinsic errors (E0700-E0799)
    // ========================================================================
    UNKNOWN_INTRINSIC = 700 => {
        explanation: "Rue rejects an `@` call when its name is not available as an intrinsic in that source context. Besides unknown names, this includes reserved spellings such as `@cast` that are intentionally rejected and internal intrinsics such as `@place` used outside their permitted compiler-defined context.",
        likely_cause: "The intrinsic name is misspelled or obsolete, belongs to another language, names a reserved spelling that directs callers to another intrinsic, or is restricted to an internal context. Check the intrinsic quick reference and use an available, case-sensitive source spelling.",
        examples: [
            ErrorCodeExample { title: "Call an unknown intrinsic", source: "fn main() -> i32 {\n    @maximum(i32)\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use a recognized intrinsic", source: "fn main() -> i32 {\n    @int_max(i32)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Unknown intrinsic names", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:5") },
            ErrorCodeReference { title: "Source intrinsic inventory", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:5a") },
        ],
    };
    INTRINSIC_WRONG_ARG_COUNT = 701 => {
        explanation: "Each intrinsic declares an accepted argument count or range. Rue checks the number of supplied arguments against that arity contract before applying its type and behavior rules.",
        likely_cause: "An argument is missing, an extra comma-separated argument was supplied, or the call was written for a different version of the intrinsic's signature.",
        examples: [
            ErrorCodeExample { title: "Omit a required intrinsic argument", source: "fn main() -> i32 {\n    @dbg();\n    0\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Supply the declared argument count", source: "fn main() -> i32 {\n    @dbg(42);\n    0\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Declared intrinsic signatures", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:3") },
            ErrorCodeReference { title: "Intrinsic argument counts", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:4") },
        ],
    };
    INTRINSIC_TYPE_MISMATCH = 702 => {
        explanation: "An intrinsic argument has the wrong kind or type for the selected intrinsic. Intrinsic signatures distinguish type arguments from expression arguments and may further restrict accepted value types.",
        likely_cause: "A value was supplied where a type is required, a type was supplied where an expression is required, or the argument's type is outside the intrinsic's documented domain.",
        examples: [
            ErrorCodeExample { title: "Use a non-integer type with an integer-bound intrinsic", source: "fn main() -> i32 {\n    @int_max(bool)\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use an integer type", source: "fn main() -> i32 {\n    @int_max(i32)\n}", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Expression and type intrinsic arguments", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:2a") },
            ErrorCodeReference { title: "Integer-bound intrinsic operand types", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:129") },
        ],
    };
    IMPORT_REQUIRES_STRING_LITERAL = 703 => {
        explanation: "`@import` requires its module specifier to appear directly as a string literal. Rue resolves the source graph before general expression evaluation, so a variable or computed string cannot select a module.",
        likely_cause: "The import path was stored in a local or constant, constructed by an expression, or supplied as a non-string value instead of being written literally inside `@import(...)`.",
        examples: [
            ErrorCodeExample { title: "Import through a string variable", source: "// --- main.rue\nfn main() -> i32 {\n    let path = \"helper.rue\";\n    let helper = @import(path);\n    helper.answer()\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Write the import path as a literal", source: "// --- main.rue\nconst helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.answer() }\n// --- helper.rue\npub fn answer() -> i32 { 42 }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Import argument form", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:80") },
            ErrorCodeReference { title: "Import string literal requirement", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:84") },
        ],
    };
    MODULE_NOT_FOUND = 704 => {
        explanation: "The one candidate file selected by an `@import` specifier was not present in the compilation's source graph. Extensioned paths name that exact file; extensionless paths name only a directory facade.",
        likely_cause: "The path is misspelled, the file was not supplied to the compiler, the path is relative to a different importing file than expected, or `.rue` was omitted when a file module rather than a directory facade was intended.",
        examples: [
            ErrorCodeExample { title: "Import a missing file module", source: "// --- main.rue\nfn main() -> i32 {\n    let helper = @import(\"helper.rue\");\n    helper.answer()\n}", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Provide the file at the selected path", source: "// --- main.rue\nconst helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.answer() }\n// --- helper.rue\npub fn answer() -> i32 { 42 }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Missing imported modules", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:83") },
            ErrorCodeReference { title: "Import candidate selection", path: "docs/spec/src/10-modules/02-import-resolution.md", rule: Some("10.2:1") },
        ],
    };
    STD_LIB_NOT_FOUND = 705 => {
        explanation: "`@import(\"std\")` requested the trusted standard-library facade, but neither the project's vendored standard library nor the configured toolchain standard library supplied it.",
        likely_cause: "The Rue installation is incomplete, `RUE_STD_PATH` does not name a valid standard-library directory, or a source-only compiler embedding requested `std` without supplying trusted standard-library sources.",
        examples: [
            ErrorCodeExample { title: "Import std when no standard library is available", source: "// --- main.rue\nconst std = @import(\"std\");\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Remove an unnecessary standard-library import", source: "// --- main.rue\nfn main() -> i32 { 0 }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [ErrorCodeReference { title: "Standard-library resolution", path: "docs/spec/src/10-modules/02-import-resolution.md", rule: Some("10.2:6") }],
    };
    PRIVATE_MEMBER_ACCESS = 706 => {
        explanation: "A module-qualified access crossed a directory boundary to a private member. The top-level item, or the enclosing type for an enum variant or associated function, was not declared `pub`. Rue privacy is directory-based: private members remain usable by files in their defining directory but are hidden from importers in other directories.",
        likely_cause: "A function, constant, type, enum, or module re-export needed by an external importer is missing `pub`; an associated function or variant belongs to a private enclosing type; or the caller was moved outside the defining directory.",
        examples: [
            ErrorCodeExample { title: "Access a private function across directories", source: "// --- main.rue\nconst lib = @import(\"sub/lib.rue\");\nfn main() -> i32 { lib.secret() }\n// --- sub/lib.rue\nfn secret() -> i32 { 42 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Export the cross-directory member", source: "// --- main.rue\nconst lib = @import(\"sub/lib.rue\");\nfn main() -> i32 { lib.answer() }\n// --- sub/lib.rue\npub fn answer() -> i32 { 42 }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Private cross-directory access", path: "docs/spec/src/10-modules/03-visibility.md", rule: Some("10.3:3") },
            ErrorCodeReference { title: "Uniform module-member privacy", path: "docs/spec/src/10-modules/04-module-bindings.md", rule: Some("10.4:18") },
        ],
    };
    UNKNOWN_MODULE_MEMBER = 707 => {
        explanation: "A qualified name was not declared as a member of the module named on its left. Module membership is per file; loading another file with a same-named declaration does not add that declaration to this module.",
        likely_cause: "The member name is misspelled, the wrong module binding is used, the declaration lives in another file, or a needed member was removed or renamed.",
        examples: [
            ErrorCodeExample { title: "Name a member absent from the imported module", source: "// --- main.rue\nconst helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.missing() }\n// --- helper.rue\npub fn answer() -> i32 { 42 }", outcome: ErrorCodeExampleOutcome::EmitsThisCode },
            ErrorCodeExample { title: "Use a member declared by that module", source: "// --- main.rue\nconst helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.answer() }\n// --- helper.rue\npub fn answer() -> i32 { 42 }", outcome: ErrorCodeExampleOutcome::Compiles },
        ],
        references: [
            ErrorCodeReference { title: "Unknown imported module members", path: "docs/spec/src/04-expressions/13-intrinsics.md", rule: Some("4.13:90") },
            ErrorCodeReference { title: "Per-file module membership", path: "docs/spec/src/10-modules/04-module-bindings.md", rule: Some("10.4:17") },
        ],
    };
    // 708 (AMBIGUOUS_MODULE) is retired (ADR-0078): an extensionless import
    // names the directory facade alone and a file module is spelled with its
    // extension, so the file/facade ambiguity no longer exists. Keep the
    // number retired rather than reusing it.
    CANNOT_INFER_CAST_TARGET = 709;
    CANNOT_INFER_POINTEE_TYPE = 710;
    CONTAINER_ELEMENT_NOT_TRIVIALLY_DROPPABLE = 711;
    /// A comptime-constant `align` argument to a byte-allocation intrinsic
    /// (`@alloc`/`@alloc_zeroed`/`@free`/`@realloc`/`@resize`, ADR-0059) was
    /// zero or not a power of two. Alignment must be a power of two.
    INTRINSIC_ALIGN_NOT_POWER_OF_TWO = 712;
    /// A relative `@import` path resolves outside the project root (the root
    /// source file's directory), so it can receive no project-relative module
    /// identity (ADR-0078).
    IMPORT_ESCAPES_ROOT = 713;
    /// An `@import` specifier is not a relative path: it is empty, or it is
    /// absolute. Resolution is defined for relative paths only (spec 10.2:1-2),
    /// and an absolute specifier would additionally bind the program to one
    /// machine's layout, which project-root-relative identity exists to avoid.
    IMPORT_SPECIFIER_NOT_RELATIVE = 714;
    /// Two logical import spellings resolve to one physical file. Importing a
    /// physical file under more than one logical identity would make the
    /// source graph ambiguous (RUE-1705).
    IMPORT_SPELLINGS_SAME_FILE = 715;

    // ========================================================================
    // Literal/operator errors (E0800-E0899)
    // ========================================================================
    LITERAL_OUT_OF_RANGE = 800;
    CANNOT_NEGATE = 801;
    CHAINED_COMPARISON = 802;

    // ========================================================================
    // Array errors (E0900-E0999)
    // ========================================================================
    INDEX_ON_NON_ARRAY = 900;
    ARRAY_LENGTH_MISMATCH = 901;
    INDEX_OUT_OF_BOUNDS = 902;
    TYPE_ANNOTATION_REQUIRED = 903;
    MOVE_OUT_OF_INDEX = 904;
    ARRAY_REPEAT_NON_COPY = 905;
    TYPE_TOO_LARGE = 906;
    FUNCTION_FRAME_TOO_LARGE = 907;
    /// A frame array with non-slot-width elements cannot yet be borrowed as a
    /// slice because slice pointer semantics use the compact element image
    /// while frames keep a full-slot representation (RUE-1595).
    SLICE_FRAME_ARRAY_NOT_SUPPORTED = 908;

    // ------------------------------------------------------------------
    // Bit-reinterpretation errors (E0950-E0959)
    // ------------------------------------------------------------------
    /// `@bitCast` was written between integer types of different widths
    /// (RUE-952, spec 4.13:120). A bit reinterpretation renames the bits it is
    /// given; it neither invents nor discards any, so the source and target
    /// **must** share a width. The value-changing conversion is `@intCast`.
    BIT_CAST_WIDTH_MISMATCH = 950;

    // ========================================================================
    // Linker/target errors (E1000-E1099)
    // ========================================================================
    LINK_ERROR = 1000;
    UNSUPPORTED_TARGET = 1001;

    // ========================================================================
    // Preview feature errors (E1100-E1199)
    // ========================================================================
    PREVIEW_FEATURE_REQUIRED = 1100;
    EXTERN_SIGNATURE_TYPE_UNSUPPORTED = 1101;
    EXTERN_AGGREGATE_NOT_REPR_C = 1102;
    EXTERN_ARRAY_BY_VALUE = 1103;
    REPR_C_STRUCT_INELIGIBLE = 1104;
    EXTERN_VARIADIC_UNSUPPORTED = 1105;
    EXPORT_SIGNATURE_UNSUPPORTED = 1106;
    /// The same C symbol is declared by two `extern "C"` foreign declarations
    /// whose Rue signatures disagree (RUE-1218, spec 9.3:5). A foreign
    /// declaration names an external C symbol rather than a Rue callable, so
    /// every module that declares it is describing *one* function; disagreeing
    /// descriptions cannot both be right, and the program links against a
    /// single definition. Sits in the FFI band beside the other `extern "C"`
    /// signature rules (E1101-E1103) because it is a foreign-declaration rule,
    /// not a general duplicate-definition rule — an ordinary function's
    /// identity is module-local (RUE-1125), so only foreign declarations can
    /// collide across modules at all.
    FOREIGN_SIGNATURE_CONFLICT = 1107;
    /// An `extern "C"` foreign declaration names `main`, the program entry
    /// point (RUE-1220, spec 9.3:6). A foreign declaration names the C symbol
    /// it declares (RUE-1125), so this one takes the bare `main` symbol the
    /// runtime start glue calls (spec 6.1:38) — in the root module it collides
    /// with the entry point's own definition, and in any other module a call
    /// through it recurses into the program's own `main`. Sits in the FFI band
    /// beside the other `extern "C"` rules, and is the import-side mirror of
    /// E1106's rejection of a `pub extern "C" fn` export named `main`.
    FOREIGN_ENTRY_POINT_DECLARATION = 1108;
    /// A typed float reached the explicit ADR-0065 Phase 4 lowering boundary.
    /// The source feature is enabled, but architecture lowering is not yet.
    FLOAT_NOT_YET_IMPLEMENTED = 1109;

    // ========================================================================
    // Comptime errors (E1200-E1299)
    // ========================================================================
    COMPTIME_EVALUATION_FAILED = 1200;
    COMPTIME_ARG_NOT_CONST = 1201;

    // ========================================================================
    // Unchecked-code errors (E1300-E1399)
    // ========================================================================
    UNCHECKED_OP_REQUIRES_CHECKED = 1300;

    // ========================================================================
    // Compiler-input errors (E1400-E1499)
    // ========================================================================
    /// Invalid metadata supplied at a compiler API boundary.
    INVALID_COMPILER_INPUT = 1400;
    COMPILER_RESOURCE_LIMIT = 1401;
    COMPILER_RESOURCE_EXHAUSTION = 1402;
    OUTPUT_PUBLICATION = 1403;
    UNSATISFIED_TRUSTED_TOOLCHAIN_INPUT = 1404;

    // ========================================================================
    // Driver/host errors (E1500-E1599)
    // ========================================================================
    // These codes classify failures raised outside the compiler diagnostic
    // graph. They are used by the CLI/host's machine-readable source-load
    // boundary and therefore do not map from an ErrorKind variant below.
    /// An ordinary source-loading or driver I/O failure.
    DRIVER_SOURCE_LOAD = 1500;
    /// A required trusted toolchain input is missing, unreadable, or malformed.
    DRIVER_TOOLCHAIN_INTEGRITY = 1501;
    /// Hermetic policy denied a trusted toolchain input during acquisition.
    DRIVER_HERMETIC_DENIAL = 1502;

    // ========================================================================
    // Internal compiler errors (E9000-E9999)
    // ========================================================================
    INTERNAL_ERROR = 9000;
    INTERNAL_CODEGEN_ERROR = 9001;
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:04}", self.0)
    }
}

/// Error returned when parsing a public diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ParseErrorCodeError {
    #[error("error code must have the form E followed by exactly four decimal digits")]
    Malformed,
    #[error("unknown or retired error code {0}")]
    Unknown(ErrorCode),
}

impl std::str::FromStr for ErrorCode {
    type Err = ParseErrorCodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let digits = value
            .strip_prefix('E')
            .filter(|digits| digits.len() == 4 && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or(ParseErrorCodeError::Malformed)?;
        let code = ErrorCode(
            digits
                .parse::<u16>()
                .expect("four decimal digits always fit in u16"),
        );
        error_code_metadata()
            .binary_search_by_key(&code.0, |entry| entry.code.0)
            .map(|_| code)
            .map_err(|_| ParseErrorCodeError::Unknown(code))
    }
}

/// Return every public compiler error code in numeric order.
///
/// `define_error_codes!` remains the single structural authority: it emits the
/// associated constants and the declaration table consumed here.
/// Unit tests below cross-check this inventory against the exhaustive
/// [`ErrorKind::code`] mapping (apart from the documented driver boundary).
pub fn error_code_metadata() -> &'static [ErrorCodeMetadata] {
    static METADATA: std::sync::OnceLock<Vec<ErrorCodeMetadata>> = std::sync::OnceLock::new();
    METADATA.get_or_init(|| {
        let mut metadata = Vec::with_capacity(ERROR_CODE_DECLARATIONS.len());
        for &(code, name) in ERROR_CODE_DECLARATIONS {
            let mut words = name.split('_');
            let first = words.next().expect("ErrorCode name must not be empty");
            let mut title = first.to_ascii_lowercase();
            title[..1].make_ascii_uppercase();
            for word in words {
                title.push(' ');
                title.push_str(&word.to_ascii_lowercase());
            }
            metadata.push(ErrorCodeMetadata {
                code,
                name,
                title,
                category: error_code_category(code)
                    .expect("declared ErrorCode must occupy a documented category band"),
                stability: ErrorCodeStability::Permanent,
                source_path: "crates/rue-error/src/lib.rs",
            });
        }
        metadata.sort_by_key(|entry| entry.code.0);
        metadata
    })
}

/// Return the compiler-owned long-form explanation for `code`, when this
/// production tranche covers it.
///
/// Explanation declarations are emitted by the same macro invocation as the
/// [`ErrorCode`] constants. At initialization they are joined to the canonical
/// metadata inventory, so consumers cannot observe a copied code identity or
/// symbolic-name registry.
pub fn error_code_explanation(code: ErrorCode) -> Option<&'static ErrorCodeExplanation> {
    static EXPLANATIONS: std::sync::OnceLock<Vec<ErrorCodeExplanation>> =
        std::sync::OnceLock::new();
    EXPLANATIONS
        .get_or_init(|| {
            ERROR_CODE_EXPLANATION_DECLARATIONS
                .iter()
                .map(|declaration| {
                    let metadata_index = error_code_metadata()
                        .binary_search_by_key(&declaration.code.0, |entry| entry.code.0)
                        .expect("explanation macro entry must have canonical metadata");
                    ErrorCodeExplanation {
                        metadata: &error_code_metadata()[metadata_index],
                        explanation: declaration.explanation,
                        likely_cause: declaration.likely_cause,
                        examples: declaration.examples,
                        references: declaration.references,
                    }
                })
                .collect()
        })
        .iter()
        .find(|explanation| explanation.metadata.code == code)
}

// ============================================================================
// Boxed Error Payloads
// ============================================================================
//
// # Boxing Policy
//
// Large error variants are boxed to reduce the size of ErrorKind.
// This keeps Result<T, CompileError> smaller on the stack.
// Errors are cold paths, so the extra indirection is acceptable.
//
// ## When to Box
//
// Box error payloads when the variant data is **≥ 72 bytes** (3 or more Strings).
//
// Basic sizes on 64-bit systems:
// - String: 24 bytes
// - Vec<T>: 24 bytes
// - Box<T>: 8 bytes (pointer)
// - Cow<'static, str>: 24 bytes
//
// Examples:
// - 1 String: 24 bytes → inline
// - 2 Strings: 48 bytes → inline
// - 3 Strings: 72 bytes → **box**
// - String + Vec<String>: 48 bytes → inline (unless Vec typically large)
//
// ## Pattern
//
// Use a dedicated struct for boxed payloads:
//
// ```rust
// #[derive(Debug, Clone, PartialEq, Eq)]
// pub struct LargeErrorPayload {
//     pub field1: String,
//     pub field2: String,
//     pub field3: String,
// }
//
// #[error("message")]
// LargeError(Box<LargeErrorPayload>),
// ```
//
// ## Current Status
//
// As of 2026-01-11:
// - ErrorKind size: 56 bytes
// - Boxed variants: 3 (MissingFields, CopyStructNonCopyField,
//   IntrinsicTypeMismatch)
// - All boxed variants contain 3+ Strings or String + Vec
// - Policy is consistently applied

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

/// Payload for `ErrorKind::ReprCStructIneligible`.
///
/// A `@repr(c)` struct failed the reject-don't-guess eligibility check
/// (ADR-0064 Amendment 1). The structured `field_path` mirrors the failing
/// predicate's field path (the RUE-504 machine-readable exposure direction);
/// `reason` is the rendered human explanation naming the reject-list entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReprCIneligibleError {
    /// The `@repr(c)` struct that is not FFI-eligible.
    pub struct_name: String,
    /// Dotted field path to the offending field, empty when the struct body
    /// itself is the problem (e.g. an empty struct).
    pub field_path: String,
    /// The offending type as rendered for the user.
    pub failing_type: String,
    /// Human phrase naming the reject-list reason.
    pub reason: String,
}

/// Payload for `ErrorKind::ForeignSignatureConflict`.
///
/// Two `extern "C"` foreign declarations name the same C symbol with
/// disagreeing Rue signatures (RUE-1218). Both rendered signatures are carried
/// so the diagnostic states the disagreement rather than only pointing at the
/// two declaration sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignSignatureConflictError {
    /// The C symbol both declarations name.
    pub symbol: String,
    /// The later declaration's signature, as rendered for the user.
    pub declared: String,
    /// The earlier declaration's signature, as rendered for the user.
    pub previously_declared: String,
}

/// Payload for `ErrorKind::LinearFieldDroppedByDestructure`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearFieldDroppedByDestructureError {
    pub struct_name: String,
    pub accessed: String,
    pub dropped: String,
}

/// Payload for `ErrorKind::IntrinsicTypeMismatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrinsicTypeMismatchError {
    pub name: String,
    pub expected: String,
    pub found: String,
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
    /// C FFI: `extern "C"` foreign-function declarations and static-archive
    /// linking (ADR-0064, RUE-1055). Gated until every phase of the guaranteed
    /// target-C boundary is proven on both backends.
    CFfi,
    /// Floating point: `f32`/`f64`, IEEE-754 arithmetic, and `comptime_float`
    /// literals (ADR-0065, RUE-714). Gated until every phase of the M9 rollout
    /// — types and inference, both backends, the dtoa runtime — is complete
    /// (ADR-0065 Phase 10). The lexer and parser accept a float literal only
    /// with this flag; Phase 4 types it, while executable builds still stop at
    /// [`ErrorKind::FloatNotYetImplemented`] until backend phases land.
    Floats,
    /// Public enums may promise that importing matches include a wildcard.
    NonExhaustiveEnums,
    /// Test declarations: the `test "name" { .. }` language item (ADR-0083
    /// §1, RUE-1618). The gate covers a parser change, so any request whose
    /// closure contains a test item — an executable build included, which
    /// parses test items for the unused-item scan — needs the flag to compile
    /// at all. `rue test` will not enable it implicitly.
    TestDeclarations,
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
            PreviewFeature::CFfi => "c_ffi",
            PreviewFeature::Floats => "floats",
            PreviewFeature::NonExhaustiveEnums => "non_exhaustive_enums",
            PreviewFeature::TestDeclarations => "test_declarations",
        }
    }

    /// Get the ADR number documenting this feature.
    #[allow(unreachable_code)]
    pub fn adr(&self) -> &'static str {
        match *self {
            PreviewFeature::TestInfra => "ADR-0005",
            PreviewFeature::CFfi => "ADR-0064",
            PreviewFeature::Floats => "ADR-0065",
            PreviewFeature::NonExhaustiveEnums => "ADR-0005",
            PreviewFeature::TestDeclarations => "ADR-0083",
        }
    }

    /// Get all available preview features.
    pub fn all() -> &'static [PreviewFeature] {
        &[
            PreviewFeature::TestInfra,
            PreviewFeature::CFfi,
            PreviewFeature::Floats,
            PreviewFeature::NonExhaustiveEnums,
            PreviewFeature::TestDeclarations,
        ]
    }

    /// The `help:` line every preview-gate diagnostic carries: which flag
    /// enables the feature, and which ADR governs it.
    ///
    /// This is the single authority for that wording. Three sites raise
    /// [`ErrorKind::PreviewFeatureRequired`] — body analysis, the comptime
    /// host, and the request-level closure gate — and a user who hits any of
    /// them is being told the same thing, so the sentence is assembled once
    /// here rather than hand-formatted at each site where the three could
    /// silently drift apart.
    pub fn enable_help(self) -> String {
        format!(
            "use --preview {} to enable this feature ({})",
            self.name(),
            self.adr()
        )
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
            "c_ffi" => Ok(PreviewFeature::CFfi),
            "floats" => Ok(PreviewFeature::Floats),
            "non_exhaustive_enums" => Ok(PreviewFeature::NonExhaustiveEnums),
            "test_declarations" => Ok(PreviewFeature::TestDeclarations),
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// Rebind every source location carried by this diagnostic while
    /// preserving its kind and explanatory payload.
    pub fn map_spans(mut self, mut map: impl FnMut(Span) -> Span) -> Self {
        self.span = self.span.map(&mut map);
        for label in &mut self.diagnostic.labels {
            label.span = map(label.span);
        }
        for suggestion in &mut self.diagnostic.suggestions {
            suggestion.span = map(suggestion.span);
        }
        self
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

/// Payload for [`ErrorKind::PrivateUnqualifiedAccess`], boxed to keep
/// `ErrorKind` within its 64-byte size budget (three inline `String`s
/// exceed it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateUnqualifiedAccessData {
    /// The kind of item ("function", "struct", "enum", "constant").
    pub item_kind: String,
    /// The item's name as written at the reference site.
    pub name: String,
    /// The path of the file that defines the private item.
    pub defining_file: String,
}

/// The kind of compilation error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ErrorKind {
    // Lexer errors
    // escape_debug keeps printable characters as-is but renders control and
    // other invisible characters (NUL, VT, BOM, ...) as visible escapes like
    // \u{b}, so a hostile byte can't corrupt the one-line message.
    #[error("unexpected character: {}", .0.escape_debug())]
    UnexpectedCharacter(char),
    #[error("invalid integer literal")]
    InvalidInteger,
    #[error("invalid escape sequence: \\{}", .0.escape_debug())]
    InvalidStringEscape(char),
    #[error("unterminated string literal")]
    UnterminatedString,
    /// An uppercase integer-literal base prefix (`0X`/`0B`/`0O`). Base
    /// prefixes are lowercase (spec 2.1); a targeted error is friendlier
    /// than splitting into `0` + identifier and dying with a generic parse
    /// error. (RUE-177)
    #[error("invalid base prefix `0{0}`: base prefixes are lowercase (`0x`, `0o`, `0b`)")]
    UppercaseBasePrefix(char),
    /// A based integer literal with no digits after the prefix (`0x`,
    /// `0b_`). (RUE-177)
    #[error("missing digits after base prefix in {base} integer literal")]
    EmptyBasedLiteral { base: &'static str },
    /// A digit that is not valid for the literal's base (`0b2`, `0o9`,
    /// `0xG`). (RUE-177)
    #[error("invalid digit `{digit}` in {base} integer literal")]
    InvalidDigitForBase { digit: char, base: &'static str },
    /// A malformed byte literal (`b'ab'`, `b''`, `b'\q'`, `b'é'`, an
    /// unterminated `b'a`). Carries a specific, already-rendered reason built
    /// by the lexer. Byte literals (`b'a'`) are readable `u8` spellings of a
    /// single ASCII byte (RUE-1042).
    #[error("{0}")]
    MalformedByteLiteral(String),
    /// A malformed floating-point literal: a leading dot (`.5`) or a trailing
    /// dot (`5.`). ADR-0065 §3 rejects both spellings for readability — write
    /// `0.5` and `5.0`. Carries a specific, already-rendered reason.
    ///
    /// The leading-dot form is a lexical decision (`.` immediately followed by
    /// a digit can never start any other Rue lexeme), so the lexer reports it.
    /// The trailing-dot form is not decidable from the lexeme alone: `42.` is
    /// the prefix of the legal method call `42.to_string()`, so the *parser*
    /// reports it once the token after the `.` proves no member name follows.
    /// Both share this kind — and E0011 — because both diagnose the same
    /// numeric-literal spelling rule.
    #[error("{0}")]
    MalformedFloatLiteral(String),
    /// Summary emitted after lexing reaches its per-file diagnostic budget.
    /// `limit` is the number of detailed diagnostics retained.
    #[error("additional lexer diagnostics omitted after the first {limit} errors")]
    LexerDiagnosticsOmitted { limit: usize },

    // Parser errors
    #[error("expected {expected}, found {found}")]
    UnexpectedToken {
        expected: Cow<'static, str>,
        found: Cow<'static, str>,
    },
    /// A custom parse error with a specific message.
    ///
    /// Used for parser-generated errors that don't fit the "expected X, found Y" pattern.
    #[error("{0}")]
    ParseError(String),
    /// Summary emitted after parser recovery reaches its per-file diagnostic
    /// budget. `limit` is the number of detailed diagnostics retained.
    #[error("additional parser diagnostics omitted after the first {limit} errors")]
    ParserDiagnosticsOmitted { limit: usize },
    /// Source is nested more deeply than the compiler supports. Reported by
    /// the parser's nesting pre-scan and by the AstGen depth guard so that
    /// pathologically nested input yields a clean diagnostic instead of a
    /// stack overflow (RUE-42). `limit` is the maximum supported nesting
    /// depth (`MAX_NESTING_DEPTH`).
    #[error("expression nests too deeply; exceeds the maximum nesting depth of {limit}")]
    NestingLimitExceeded { limit: usize },

    // Semantic errors
    #[error("no main function found")]
    NoMainFunction,
    /// The `main` declaration does not match the runtime entry ABI (RUE-778).
    #[error("invalid main function signature: {reason}")]
    InvalidMainSignature { reason: &'static str },
    #[error("undefined variable '{0}'")]
    UndefinedVariable(String),
    #[error("undefined function '{0}'")]
    UndefinedFunction(String),
    #[error("cannot assign to immutable variable '{0}'")]
    AssignToImmutable(String),
    #[error("unknown type '{0}'")]
    UnknownType(String),
    /// An array length `[T; N]` where `N` is not a usable compile-time
    /// constant: a runtime variable, a non-integer/negative value, or an
    /// undefined name (RUE-16). `reason` explains the specific problem.
    #[error("invalid array length: {reason}")]
    InvalidArrayLength { reason: String },
    /// Use of a value after it has been moved.
    #[error("use of moved value '{0}'")]
    UseAfterMove(String),
    #[error("type mismatch: expected {expected}, found {found}")]
    TypeMismatch { expected: String, found: String },
    #[error("{}", format_argument_count(*.expected, *.found))]
    WrongArgumentCount { expected: usize, found: usize },
    /// A pattern binds the same identifier more than once (spec 4.7:30):
    /// e.g. an enum payload pattern `Rect(w, w)`. Reusing a name silently
    /// shadows the earlier binding and discards its value (RUE-269).
    #[error("identifier '{name}' is bound more than once in the same pattern")]
    DuplicatePatternBinding { name: String },

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
    /// User-defined function collides with a runtime/codegen helper symbol
    #[error("cannot define function `{function_name}`: name is reserved for a runtime helper")]
    ReservedFunctionName { function_name: String },
    /// Duplicate type definition
    #[error("duplicate type definition: `{type_name}` is already defined")]
    DuplicateTypeDefinition { type_name: String },
    /// Duplicate function definition
    #[error("duplicate function definition: `{function_name}` is already defined")]
    DuplicateFunctionDefinition { function_name: String },
    /// Definitions with the same name but different kinds cannot coexist.
    #[error("duplicate definition: `{name}` conflicts with an existing item of a different kind")]
    DuplicateMixedKindDefinition { name: String },

    /// Two `test "name" { .. }` declarations in one module share a name
    /// (ADR-0083 §1). Keyed among test declarations only: a test never
    /// collides with a function, type, or constant of the same spelling.
    #[error("duplicate test definition: \"{test_name}\" is already defined in this module")]
    DuplicateTestDefinition { test_name: String },
    /// Linear value was not consumed before going out of scope
    #[error("linear value '{0}' must be consumed but was dropped")]
    LinearValueNotConsumed(String),
    /// Linear value was consumed on some control-flow paths but not all of them
    #[error("linear value '{0}' is not consumed on all paths")]
    LinearValueNotConsumedOnAllPaths(String),
    /// A discarded expression value (e.g. an expression statement or a loop
    /// body result) carries a linear value, which would be implicitly dropped
    #[error("discarded value of type '{type_name}' carries a linear value and must be consumed")]
    LinearValueDiscarded { type_name: String },
    /// A match-arm payload position that binds nothing — a `_` discard, or a
    /// position covered by the all-wildcard bare variant pattern — names a
    /// payload field whose type carries a linear value (RUE-1592, spec
    /// 4.7:30). Such a position elaborates to a fresh *unnameable* binding
    /// (formal core §2), and an unnameable binding can never be consumed, so
    /// the must-consume obligation of 3.8:52 could never be discharged.
    #[error(
        "discarded payload {position} carries a linear value of type '{type_name}' and must be consumed"
    )]
    LinearPayloadDiscarded { position: String, type_name: String },
    /// Assignment to a place holding a live linear value (RUE-387): the
    /// overwrite would implicitly drop the old linear value.
    #[error("assignment would overwrite a live linear value of type '{type_name}'")]
    LinearValueOverwritten { type_name: String },
    /// Assignment to an `inout` parameter whose type carries a linear value
    /// (RUE-387): reassigning it would implicitly drop the caller's value.
    #[error(
        "assignment to `inout` parameter would overwrite the caller's live linear value of type '{type_name}'"
    )]
    LinearValueOverwrittenThroughInout { type_name: String },
    /// Linear struct cannot be marked @copy
    #[error("linear struct '{0}' cannot be marked @copy")]
    LinearStructCopy(String),
    /// Field access destructures a **declared**-`linear` struct, implicitly
    /// dropping another field that carries a linear value (spec 3.8:60).
    ///
    /// Only a declared-`linear` struct destructures whole-value this way. A
    /// struct that is linear merely because a field carries a linear value
    /// (infectious linearity, 3.8:58) takes an ordinary partial move on field
    /// access, so its siblings survive and an unconsumed linear sub-place is
    /// reported at scope exit by the must-consume family instead (RUE-1591).
    #[error(
        "accessing field '{accessed}' of linear struct '{struct_name}' would implicitly drop linear field '{dropped}'",
        struct_name = .0.struct_name,
        accessed = .0.accessed,
        dropped = .0.dropped
    )]
    LinearFieldDroppedByDestructure(Box<LinearFieldDroppedByDestructureError>),
    /// Duplicate method definition in impl blocks for the same type
    #[error("duplicate method '{method_name}' for type '{type_name}'")]
    DuplicateMethod {
        type_name: String,
        method_name: String,
    },
    /// A function or method parameter list names the same parameter twice.
    /// The span points at the second (duplicate) occurrence (RUE-349).
    #[error("duplicate parameter name '{name}'")]
    DuplicateParameter { name: String },
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
        "'{type_name}.{method_name}' is a method, not an associated function; call it on a receiver, e.g. `receiver.{method_name}()`"
    )]
    MethodCalledAsAssocFn {
        type_name: String,
        method_name: String,
    },
    /// Calling an associated function (without self) as a method
    #[error(
        "'{function_name}' is an associated function, not a method; call it on the type, e.g. `{type_name}.{function_name}()`"
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
    /// Expression not supported in const context
    #[error("{expr_kind} is not supported in const context")]
    ConstExprNotSupported { expr_kind: String },
    /// A constant initializer (transitively) refers back to the constant
    /// being defined, so no evaluation order exists.
    #[error("cycle detected in constant initializers: {cycle}")]
    ConstInitializerCycle { cycle: String },
    /// A struct or enum (transitively) contains itself by value with no
    /// pointer indirection, so it has infinite size and no valid layout
    /// (RUE-264). `cycle` names the containment path (e.g. `A -> B -> A`).
    #[error(
        "recursive type '{name}' has infinite size (contains itself by value: {cycle}); break the cycle with a pointer (`ptr const {name}` / `ptr mut {name}`)"
    )]
    RecursiveTypeInfiniteSize { name: String, cycle: String },
    /// A value constant declared without a type annotation. Annotations are
    /// required on value constants (spec 6.5:4, RUE-179); only module
    /// bindings (`const m = @import(...)` and aliases) are exempt.
    #[error("missing type annotation on constant '{name}'")]
    ConstMissingTypeAnnotation { name: String },

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
    #[error("field access on non-struct type '{found}'")]
    FieldAccessOnNonStruct { found: String },
    #[error("invalid assignment target")]
    InvalidAssignmentTarget,
    /// `@raw`/`@raw_mut` operand is not an addressable place
    #[error(
        "@raw requires an addressable place (a variable, field, or array element), not a temporary value"
    )]
    RawRequiresPlace,
    /// `@field_ptr` operand is not a field-access expression `s.field`
    #[error("@field_ptr requires a field access expression (for example `s.field`)")]
    FieldPtrRequiresField,
    /// A string literal does not fit the fixed-capacity string `Str(N)` it is
    /// assigned to: its UTF-8 byte length exceeds `N` (ADR-0043 Phase 5,
    /// RUE-326).
    #[error(
        "string literal of {byte_len} bytes does not fit in `Str({capacity})` (capacity {capacity} bytes)"
    )]
    StrFixedCapacityExceeded { capacity: u64, byte_len: u64 },
    /// A string buffer (`StrBuf`/`Str(N)`) flowed into a first-class `str`
    /// slot (ADR-0043 two-types model, RUE-386). `site` names the position
    /// ("as a parameter argument", "in a binding", "as a return value", "in a
    /// struct field").
    #[error("a string buffer (`{found}`) cannot be used as a first-class `str` {site}")]
    BufferNotFirstClassStr { found: String, site: String },
    /// A first-class / static `str` value was passed as an `inout str` operand
    /// (ADR-0043 two-types model, RUE-386).
    #[error("a first-class `str` value cannot be passed as `inout str`")]
    InoutStrRequiresLocalBuffer,
    /// A whole value was assigned to an `inout str` parameter. The parameter
    /// is a second-class view over caller-owned bytes, so rebinding its header
    /// would overwrite storage whose concrete buffer representation is not
    /// `str` (RUE-641).
    #[error("an `inout str` view cannot be reassigned as a whole value")]
    StrViewReassignment,
    /// A borrowed `str` view (a `borrow`/`inout str` parameter) escaped into a
    /// first-class `str` slot (ADR-0043 two-types model, RUE-386). `site`
    /// names the position, as in [`Self::BufferNotFirstClassStr`].
    #[error("a borrowed `str` view cannot be used as a first-class `str` {site}")]
    StrViewNotFirstClass { site: String },
    /// An owning growable container (`ArrayBuf(T)`) was instantiated with a
    /// `linear` element type, via `@require_droppable` (RUE-388). The container
    /// cannot yet track element linearity, so a linear element would be leaked
    /// (never consumed); the instantiation is rejected.
    #[error(
        "`@require_droppable` requires a trivially-droppable type, but `{ty}` is `linear` — an owning growable container (e.g. `ArrayBuf`) cannot yet track element linearity, so the element would be leaked (RUE-388)"
    )]
    ContainerElementIsLinear { ty: String },
    /// A by-copy element read (`ArrayBuf(T)::get`/`get_or`) was attempted on an
    /// element type that owns resources — one with drop glue (a destructor, or a
    /// field/payload that has one). Copying such an element by `@ptr_read` leaves
    /// the copy and the still-live slot both pointing at the same owned buffer, so
    /// both run drop glue at scope exit: a double-free (RUE-651). The read is
    /// rejected; use the borrow-returning `get_ref` accessor for an in-place
    /// read, or move the element out with `pop`/`pop_or` instead (one owner).
    /// Mirrors Swift's rule that a non-copyable element cannot use a by-value
    /// `get` subscript.
    #[error(
        "cannot read an element of type `{ty}` by copy: it owns resources (has drop glue), so a by-copy read would alias the owned value and double-free it at scope exit — use `get_ref` for an in-place read, or move it out with `pop`/`pop_or` (RUE-651)"
    )]
    ContainerElementNotTriviallyDroppable { ty: String },
    /// Inout argument is not an lvalue (variable, field, or array element)
    #[error("inout argument must be an lvalue (variable, field, or array element)")]
    InoutNonLvalue,
    /// Same variable passed to multiple inout parameters in a single call
    #[error("cannot pass same variable '{variable}' to multiple inout parameters")]
    InoutExclusiveAccess { variable: String },
    /// A shared by-reference position that requires an existing place and has
    /// no elaboration (RUE-953): a method's `borrow self` receiver, an
    /// accessor's receiver, and the array source of a slice coercion. An
    /// explicit `borrow` *argument* that names no place is elaborated into a
    /// promoted static or a hidden temporary instead of reaching this.
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
    /// A moving (by-value) use of a variable inside an argument list that also
    /// passes the same variable `inout`/`borrow`: the loan spans the whole
    /// call, so the move would leave it aliasing moved-from storage — a double
    /// free in safe code (law of exclusivity, RUE-523).
    #[error("cannot move '{variable}' into a call that also passes it '{loan_mode}'")]
    MoveWhileCallLoaned { variable: String, loan_mode: String },
    /// Argument to inout parameter is missing `inout` keyword at call site
    #[error("argument to inout parameter must use 'inout' keyword")]
    InoutKeywordMissing,
    /// Argument to borrow parameter is missing `borrow` keyword at call site
    #[error("argument to borrow parameter must use 'borrow' keyword")]
    BorrowKeywordMissing,
    /// An explicitly-mode-marked argument targets an ordinary unmarked
    /// parameter. Source argument modes must match parameter modes exactly.
    #[error("unexpected `{mode}` argument: the corresponding parameter is unmarked")]
    UnexpectedCallArgumentMode { mode: &'static str },
    /// Cannot move a value out of an inout parameter (would leave the caller's
    /// variable moved-from)
    #[error("cannot move out of inout parameter '{variable}'")]
    MoveOutOfInout { variable: String },
    /// An accessor result was returned from the enclosing function
    /// (ADR-0062: the borrowed place is scoped to its full expression).
    #[error(
        "cannot return an accessor result: `{method}` yields a second-class borrow of `{root}`, valid only within the calling expression"
    )]
    AccessorResultReturned { method: String, root: String },
    /// An accessor result was assigned into a variable, field, or element.
    #[error(
        "cannot store an accessor result: `{method}` yields a second-class borrow of `{root}`, valid only within the calling expression"
    )]
    AccessorResultStored { method: String, root: String },
    /// An accessor result was bound by a plain `let`.
    #[error(
        "cannot bind an accessor result with `let`: `{method}` yields a second-class borrow of `{root}`, valid only within the calling expression"
    )]
    AccessorResultBound { method: String, root: String },
    /// An accessor result was captured into a struct or array literal.
    #[error(
        "cannot capture an accessor result in an aggregate: `{method}` yields a second-class borrow of `{root}`, valid only within the calling expression"
    )]
    AccessorResultCaptured { method: String, root: String },
    /// An accessor body's non-diverging control flow does not end in the
    /// single trailing `yield` (ADR-0062 phase 1).
    #[error(
        "an accessor body must end in a single trailing `yield`: every non-diverging path must fall through to it, and no code may follow it"
    )]
    AccessorBodyMissingYield,
    /// An accessor body contains an exit that bypasses its trailing `yield`:
    /// a second `yield`, a `return`, or a `?` (ADR-0062 phase 1). Shares
    /// E0254 with [`ErrorKind::AccessorBodyMissingYield`]; the two halves of
    /// 6.6:6 differ only in which way the body fails to end in exactly one
    /// `yield`.
    #[error(
        "an accessor body must end in a single trailing `yield`, but this body contains {found}: guards may only diverge or fall through to the trailing `yield`"
    )]
    AccessorBodyOtherExit { found: String },
    /// The operand of an accessor's `yield` is not a place rooted at the
    /// receiver parameter.
    #[error("an accessor must yield a place rooted at `self`, not {found}")]
    AccessorYieldNotReceiverRooted { found: String },
    /// A `yield` expression outside an accessor body.
    #[error("`yield` is only valid inside the body of a `-> borrow` or `-> inout` accessor")]
    YieldOutsideAccessor,
    /// An accessor result and receiver use different reference modes.
    #[error("accessor receiver/result modes do not match: {found}")]
    AccessorRequiresBorrowSelf { found: String },
    /// A drop-glue value read out of an accessor result by value.
    #[error(
        "cannot copy a value of type `{ty}` out of an accessor result: it owns resources (has drop glue), and the result is a borrow, not an owner"
    )]
    AccessorResultMoved { ty: String },
    /// Exclusive use of a root while an accessor result borrows it in the
    /// same full expression.
    #[error(
        "cannot use '{variable}' {conflict} while an accessor result borrows it in the same expression"
    )]
    AccessorLoanConflict {
        variable: String,
        conflict: &'static str,
    },
    /// An accessor parameter with a non-by-value mode.
    #[error(
        "an accessor parameter must be by-value: `{mode}` accessor parameters are not supported"
    )]
    AccessorParamModeUnsupported { mode: String },
    /// An accessor call re-entered an accessor already being expanded.
    #[error(
        "recursive accessor `{method}`: an accessor may not invoke itself, directly or through other accessors, in its own body"
    )]
    AccessorRecursion { method: String },
    /// Cannot move `self` out of a destructor body (RUE-139). The compiler
    /// drops a value by running its destructor and THEN dropping its fields;
    /// moving `self` to a new owner (a call argument, another binding, ...)
    /// would make that owner drop it again — re-entering the destructor in
    /// infinite recursion.
    #[error(
        "cannot move `self` out of the destructor for '{type_name}': the new owner would drop it again, re-entering this destructor"
    )]
    MoveSelfOutOfDestructor { type_name: String },
    /// Cannot move a field out of a value whose struct type has a
    /// user-defined destructor (RUE-158, the spirit of Rust's E0509).
    /// The destructor always runs on the whole value when it is dropped:
    /// it would observe the moved-out field (use-after-free for heap
    /// fields), and the automatic field cleanup after the destructor would
    /// drop the field a second time.
    #[error(
        "cannot move field `{field_name}` out of a value of type '{struct_name}', which has a destructor"
    )]
    MoveFieldOutOfDestructorType {
        struct_name: String,
        field_name: String,
    },
    /// A `@copy` struct cannot have a user-defined destructor (RUE-159, the
    /// spirit of Rust's E0184). Copies are implicit and untracked, so each
    /// copy would run the destructor again — double cleanup of the same
    /// logical resource.
    #[error("cannot define a destructor for '{type_name}': `@copy` types cannot have destructors")]
    CopyStructWithDestructor { type_name: String },

    // Control flow errors
    #[error("'break' outside of loop")]
    BreakOutsideLoop,
    #[error("'continue' outside of loop")]
    ContinueOutsideLoop,
    /// `break` with a value operand (e.g. `break 42`); break does not carry a value
    #[error("'break' with a value is not supported")]
    BreakWithValue,
    /// The `?` operator used in a function that does not return an `Option`.
    #[error(
        "the `?` operator can only be used in a function that returns an `Option` (found return type `{return_type}`)"
    )]
    QuestionOutsideOptionFn { return_type: String },
    /// The `?` operator applied to a value that is neither an `Option` nor a `Result`.
    #[error("the `?` operator can only be applied to an `Option` or `Result` (found `{found}`)")]
    QuestionOnNonOption { found: String },
    /// `?` on a `Result` in a function that does not return a `Result` (ADR-0038).
    #[error(
        "the `?` operator on a `Result` requires the enclosing function to return a `Result` (found return type `{return_type}`)"
    )]
    QuestionOutsideResultFn { return_type: String },
    /// `?` on a `Result` whose error type differs from the function's error type.
    /// Rue has no error conversion (no `From`/`Try`) until traits exist, so the
    /// error types must match exactly (ADR-0038).
    #[error(
        "the `?` operator requires matching error types: the operand is `Err({operand_err})` but the function returns `Err({fn_err})` (no error conversion until traits exist)"
    )]
    QuestionErrTypeMismatch { operand_err: String, fn_err: String },

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
    #[error(
        "cannot infer the target type of '@{0}'; add a type annotation \
         (e.g. `let n: i32 = @{0}(...)`) or use it where a specific integer \
         type is expected"
    )]
    CannotInferCastTarget(String),
    /// `@bitCast` between integer types of different widths (RUE-952).
    /// Reinterpretation is width-preserving by construction, so the diagnostic
    /// names both widths and points at `@intCast` for the value-changing
    /// conversion.
    #[error(
        "'@bitCast' requires the source and target to have the same width, but \
         `{from}` is {from_bits}-bit and `{to}` is {to_bits}-bit"
    )]
    BitCastWidthMismatch {
        from: String,
        to: String,
        from_bits: u32,
        to_bits: u32,
    },
    #[error(
        "cannot infer the pointee type of '@{0}'; add a type annotation \
         (e.g. `let p: ptr mut T = @{0}(...)`) or use it where a specific \
         `ptr mut T` is expected"
    )]
    CannotInferPointeeType(String),
    #[error(
        "the `align` argument to '@{name}' must be a power of two, but a \
         constant value of {value} was given"
    )]
    IntrinsicAlignNotPowerOfTwo { name: String, value: u64 },

    // Module errors
    #[error("@import requires a string literal argument")]
    ImportRequiresStringLiteral,
    #[error("cannot find module '{path}'")]
    ModuleNotFound {
        path: String,
        /// Candidates that were tried (for error message)
        candidates: Vec<String>,
    },
    #[error(
        "import '{path}' escapes the project root: '{candidate}' is outside the root source file's directory"
    )]
    ImportEscapesRoot { path: String, candidate: String },
    #[error("@import requires a relative path, but the specifier is empty")]
    ImportSpecifierEmpty,
    #[error(
        "@import requires a relative path, but '{path}' is absolute; paths resolve \
         relative to the importing file"
    )]
    ImportSpecifierAbsolute { path: String },
    #[error(
        "import spellings '{first}' and '{second}' refer to the same physical file; \
         import it under one name"
    )]
    ImportSpellingsSameFile { first: String, second: String },
    #[error("standard library not found")]
    StdLibNotFound,
    #[error("{item_kind} `{name}` is private")]
    PrivateMemberAccess { item_kind: String, name: String },
    #[error("{} `{}` is private (defined in `{}`)", .0.item_kind, .0.name, .0.defining_file)]
    PrivateUnqualifiedAccess(Box<PrivateUnqualifiedAccessData>),
    #[error("module `{module_name}` has no member `{member_name}`")]
    UnknownModuleMember {
        module_name: String,
        member_name: String,
    },

    // Literal errors
    #[error("literal value {value} is out of range for type '{ty}'")]
    LiteralOutOfRange { value: u64, ty: String },

    // Operator errors
    #[error("cannot apply unary operator `-` to type '{0}'")]
    CannotNegate(String),
    #[error("comparison operators cannot be chained")]
    ChainedComparison,

    // Array errors
    #[error("cannot index into non-array type '{found}'")]
    IndexOnNonArray { found: String },
    #[error("{}", format_array_length_mismatch(*.expected, *.found))]
    ArrayLengthMismatch { expected: u64, found: u64 },
    // `index` is i128 so a constant u64 index above i64::MAX is still
    // reported exactly (RUE-532) — the old i64 narrowing made such an index
    // look non-constant and skip the compile-time bounds check entirely.
    #[error("index out of bounds: the length is {length} but the index is {index}")]
    IndexOutOfBounds { index: i128, length: u64 },
    #[error("type annotation required for empty array")]
    TypeAnnotationRequired,
    /// Cannot move or destructure through an array index position that the
    /// per-element ownership tracker cannot follow: dynamic (non-constant)
    /// indices, and indexing that is not rooted directly at an array variable.
    /// A constant index into an array variable moves just that element out
    /// (spec 3.8:68, RUE-186). Declared-linear destructuring also uses this
    /// diagnostic when a dynamic index makes its consumed place untrackable
    /// (RUE-1755), including when the selected leaf is Copy.
    #[error("cannot move out of indexed position")]
    MoveOutOfIndex { element_type: String },
    /// Array-repeat literal `[value; count]` whose element type is not Copy.
    /// A repeat literal materializes `count` copies of a single value, so the
    /// element type must be Copy (matching Rust's `[v; N]: Copy` requirement,
    /// RUE-235).
    #[error("array-repeat literal requires a Copy element type, but '{element_type}' is not Copy")]
    ArrayRepeatNonCopy { element_type: String },
    /// A type's layout exceeds the implementation's maximum object size
    /// (Appendix C practical limit; RUE-561 — unchecked u32 slot arithmetic
    /// previously ICEd on 2 GiB arrays and silently truncated 32 GiB ones to
    /// zero-sized).
    ///
    /// The limit that is actually enforced is a count of 8-byte ABI slots, not
    /// a byte total: a layout spends one slot per scalar, per struct field, and
    /// per array element, whatever the element's own width. The message names
    /// the slot ceiling so it reports the limit the compiler checks, as spec
    /// C.1:2 requires (RUE-1272).
    #[error(
        "type '{type_name}' exceeds the maximum supported object size \
         ({max_slots} ABI slots; a layout spends one 8-byte slot per scalar, \
         struct field, and array element)"
    )]
    TypeTooLarge { type_name: String, max_slots: u64 },
    /// A function's cumulative locals, parameter homes, sret cell, spills, or
    /// transient outgoing call area exceeds the backend displacement budget.
    #[error(
        "function stack frame or outgoing call area exceeds the maximum supported size \
         ({max_bytes} bytes)"
    )]
    FunctionFrameTooLarge { max_bytes: u64 },
    /// A frame-resident array whose element's compact memory image differs from
    /// its full-slot frame representation cannot yet be coerced to a borrowed
    /// slice. The source-level coercion is rejected before it synthesizes a
    /// pointer that would mix those representations (RUE-1595).
    #[error(
        "a frame array with non-slot-width elements cannot yet coerce or borrow as a slice `[T]` (element type `{element_type}`)"
    )]
    SliceFrameArrayNotSupported { element_type: String },
    /// Assignment into an array (element write, or a write through an element)
    /// while one or more of its elements are moved out (RUE-186). Reinstating
    /// per-element ownership through writes is not supported; the whole array
    /// must be reinitialized instead.
    #[error("cannot assign into '{array}': one of its elements has been moved out")]
    AssignToPartiallyMovedArray { array: String },

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

    /// A type appeared in an `extern "C"` signature that the current FFI phase
    /// cannot classify. C FFI P3 (ADR-0064, RUE-1057) supports every integer and
    /// pointer scalar (`i8`/`u8`/`i16`/`u16`/`i32`/`u32`/`i64`/`u64`, `bool` as
    /// `_Bool`, and raw pointers) *and* C-classifiable `@repr(c)` aggregates.
    /// What remains rejected here: a Rue enum (not FFI-safe in v0), and — until
    /// RUE-714 adds the type (P5) — floating-point.
    #[error(
        "type `{ty}` is not supported in an `extern \"C\"` signature: \
         C FFI (ADR-0064) supports integer and pointer scalars and \
         C-classifiable `@repr(c)` aggregates — enums are not FFI-safe and \
         floating-point awaits RUE-714"
    )]
    ExternSignatureTypeUnsupported {
        /// The rejected type, as rendered for the user.
        ty: String,
    },

    /// An aggregate type appeared in an `extern "C"` signature without the
    /// `@repr(c)` guarantee marker (ADR-0064 Amendment 1). The marker is the
    /// guarantee trigger: any aggregate crossing the C boundary must opt in
    /// explicitly, never be silently promoted to C layout.
    #[error(
        "aggregate type `{ty}` in an `extern \"C\"` signature must be marked \
         `@repr(c)`: aggregates crossing the C boundary opt in to C layout \
         explicitly (ADR-0064)"
    )]
    ExternAggregateNotReprC {
        /// The unmarked aggregate type, as rendered for the user.
        ty: String,
    },

    /// A fixed-size array appeared directly as an `extern "C"` parameter or
    /// return type (ADR-0064 Amendment 1). C decays an array argument to a
    /// pointer and has no by-value array parameter, so a fixed array is only
    /// eligible as a struct *field*, never as a direct signature type.
    #[error(
        "fixed-size array `{ty}` cannot appear directly in an `extern \"C\"` \
         signature: C decays arrays to pointers — pass a pointer (`ptr const T`) \
         instead, or wrap the array in a `@repr(c)` struct (ADR-0064)"
    )]
    ExternArrayByValue {
        /// The rejected array type, as rendered for the user.
        ty: String,
    },

    /// A C variadic marker (`...`) appeared in an `extern "C"` parameter list.
    /// Variadic foreign calls are rejected in v0 (ADR-0064 secondary ruling B,
    /// P6): the target-C classifier implements a fixed-signature calling
    /// convention only, and matching C's variadic argument-promotion and
    /// register-save-area contract is a later, separate design. The parser
    /// recognizes the `...` token specifically so the boundary reports this
    /// rather than a generic "unexpected token".
    #[error(
        "variadic parameters (`...`) are not supported in an `extern \"C\"` \
         signature: C variadic calls are rejected in v0 (ADR-0064)"
    )]
    ExternVariadicUnsupported,

    /// A `pub extern "C" fn` Rue-to-C export (ADR-0064 P4) has a signature the
    /// export-thunk lowering cannot bridge yet. The P4 export thunk marshals
    /// only integer/pointer scalars that fit the target's argument-register
    /// budget: an aggregate parameter or return, or a scalar parameter list
    /// wider than the register budget, is rejected here rather than silently
    /// mis-marshaled, and an export whose C name collides with the program
    /// entry point (`main`) is rejected to keep the entry symbol unambiguous.
    #[error("`pub extern \"C\" fn` export `{name}` is not supported: {reason} (ADR-0064 P4)")]
    ExportSignatureUnsupported {
        /// The export's C symbol name.
        name: String,
        /// Why the export cannot be lowered, phrased for the user.
        reason: String,
    },

    /// Two `extern "C"` foreign declarations name the same C symbol with
    /// disagreeing Rue signatures (RUE-1218, spec 9.3:5). A foreign declaration
    /// is a description of an *external* function, not a Rue definition, so its
    /// internal symbol is the raw C name it declares (RUE-1125) — two modules
    /// declaring the same symbol produce one undefined symbol for the linker.
    /// Identical redeclarations are legal, matching C; disagreeing ones cannot
    /// both describe the definition that will be linked in, and without this
    /// rule the last-collected signature silently wins for every call site.
    #[error(
        "`extern \"C\"` symbol `{}` is declared with conflicting signatures: `{}` here, \
         but `{}` earlier",
        .0.symbol,
        .0.declared,
        .0.previously_declared
    )]
    ForeignSignatureConflict(Box<ForeignSignatureConflictError>),

    /// An `extern "C"` foreign declaration names `main` (RUE-1220, spec
    /// 9.3:6). The declared C symbol is the program's own entry point, which
    /// belongs to the runtime start glue (`_start`/`__main`, spec 6.1:38), so
    /// there is no external `main` for such a declaration to describe: in a
    /// non-root module it silently binds the program's own entry point and a
    /// call through it recurses, and in the root module it collides with the
    /// entry point's definition. Rejected in every module and for every
    /// signature, mirroring E1106's rejection of an export named `main`.
    #[error(
        "`extern \"C\"` declaration of `main` names the program entry point, which is not an \
         external function (ADR-0064)"
    )]
    ForeignEntryPointDeclaration,

    /// A `@repr(c)` struct failed the reject-don't-guess eligibility check
    /// (ADR-0064 Amendment 1): an empty struct, an enum/aggregate field without
    /// its own `@repr(c)` marker, or a linear / destructor-bearing field. The
    /// marker is rejected rather than guessing a C representation.
    #[error("`@repr(c)` struct `{}` is not FFI-eligible: {}", .0.struct_name, .0.reason)]
    ReprCStructIneligible(Box<ReprCIneligibleError>),

    /// A typed float reached CFG construction before architecture-specific
    /// lowering exists. Rejecting here prevents integer-oriented lowering from
    /// silently reinterpreting its bits.
    #[error(
        "floating-point lowering is not yet implemented after typed AIR \
         (ADR-0065 Phase 4 boundary, RUE-1070)"
    )]
    FloatNotYetImplemented,

    /// A slice type `[T]` appeared in return position — forbidden because a
    /// slice is second-class (ADR-0037, ADR-0043, RUE-322).
    #[error(
        "a slice type `[T]` cannot be returned: slices are second-class views \
         valid only in argument position (ADR-0043)"
    )]
    SliceReturnNotAllowed,

    /// A slice type `[T]` appeared as a struct field type — forbidden because
    /// a slice is second-class (ADR-0037, ADR-0043, RUE-322).
    #[error(
        "a slice type `[T]` cannot be stored in a struct field: slices are \
         second-class views valid only in argument position (ADR-0043)"
    )]
    SliceInAggregateField,

    /// A slice type `[T]` appeared in a `let` or `const` binding — forbidden
    /// because a slice is second-class (ADR-0037, ADR-0043, RUE-322).
    #[error(
        "a slice type `[T]` can only name a function parameter: slices are \
         second-class views and cannot be bound past their argument scope (ADR-0043)"
    )]
    SliceEscapesScope,

    // Comptime errors
    #[error("comptime evaluation failed: {reason}")]
    ComptimeEvaluationFailed { reason: String },

    #[error("comptime parameter requires a compile-time known value")]
    ComptimeArgNotConst { param_name: String },

    // Unchecked-code errors
    #[error("{what} requires a `checked` block")]
    UncheckedOpRequiresChecked { what: String },

    // Compiler-input errors
    #[error("invalid compiler input: {0}")]
    InvalidCompilerInput(String),

    /// A source-driven request exceeded a documented bounded compiler
    /// representation (spec Appendix C). This is a normal diagnostic, not an
    /// ICE: spec C.1:2 requires a diagnosable compile-time failure rather than
    /// a wrapped index or an abnormal termination.
    #[error("compiler resource limit exceeded: {0}")]
    CompilerResourceLimit(String),

    /// The compiler could not acquire memory for an otherwise valid request.
    /// This is a normal environmental failure, not an ICE.
    #[error("compiler resource exhaustion: {0}")]
    CompilerResourceExhaustion(String),

    /// The compiled executable could not be published to the output path
    /// (write, permission, signing, or rename failure). Publication is
    /// atomic: on this error no partial output artifact remains (RUE-781).
    #[error("output publication failed: {0}")]
    OutputPublication(String),

    /// A required trusted toolchain input — a standard-library module the
    /// program's reached bodies demand (RUE-1112) — was absent from the
    /// compilation and the caller did not supply it. The standard library is a
    /// toolchain guarantee, so a stable no-filesystem entry that observes an
    /// unsatisfied demand reports this deterministic contract failure at its
    /// boundary rather than an ICE: the CLI host acquires the module and retries,
    /// while an embedder that omits a guaranteed toolchain input gets a clear,
    /// distinguishable contract error.
    #[error("unsatisfied trusted toolchain input: {0}")]
    UnsatisfiedTrustedToolchainInput(String),

    // Internal compiler errors (bugs in the compiler itself)
    #[error("internal compiler producer invariant: {0}")]
    CompilerProducerInvariant(String),
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
            ErrorKind::UppercaseBasePrefix(_) => ErrorCode::UPPERCASE_BASE_PREFIX,
            ErrorKind::EmptyBasedLiteral { .. } => ErrorCode::EMPTY_BASED_LITERAL,
            ErrorKind::MalformedByteLiteral(_) => ErrorCode::MALFORMED_BYTE_LITERAL,
            ErrorKind::MalformedFloatLiteral(_) => ErrorCode::MALFORMED_FLOAT_LITERAL,
            ErrorKind::InvalidDigitForBase { .. } => ErrorCode::INVALID_DIGIT_FOR_BASE,
            ErrorKind::LexerDiagnosticsOmitted { .. } => ErrorCode::LEXER_DIAGNOSTICS_OMITTED,

            // Parser errors (E0100-E0199)
            ErrorKind::UnexpectedToken { .. } => ErrorCode::UNEXPECTED_TOKEN,
            ErrorKind::ParseError(_) => ErrorCode::PARSE_ERROR,
            ErrorKind::ParserDiagnosticsOmitted { .. } => ErrorCode::PARSER_DIAGNOSTICS_OMITTED,
            ErrorKind::NestingLimitExceeded { .. } => ErrorCode::NESTING_LIMIT_EXCEEDED,

            // Semantic errors (E0200-E0399)
            ErrorKind::NoMainFunction => ErrorCode::NO_MAIN_FUNCTION,
            ErrorKind::InvalidMainSignature { .. } => ErrorCode::INVALID_MAIN_SIGNATURE,
            ErrorKind::UndefinedVariable(_) => ErrorCode::UNDEFINED_VARIABLE,
            ErrorKind::UndefinedFunction(_) => ErrorCode::UNDEFINED_FUNCTION,
            ErrorKind::AssignToImmutable(_) => ErrorCode::ASSIGN_TO_IMMUTABLE,
            ErrorKind::UnknownType(_) => ErrorCode::UNKNOWN_TYPE,
            ErrorKind::InvalidArrayLength { .. } => ErrorCode::INVALID_ARRAY_LENGTH,
            ErrorKind::UseAfterMove(_) => ErrorCode::USE_AFTER_MOVE,
            ErrorKind::TypeMismatch { .. } => ErrorCode::TYPE_MISMATCH,
            ErrorKind::WrongArgumentCount { .. } => ErrorCode::WRONG_ARGUMENT_COUNT,
            ErrorKind::DuplicatePatternBinding { .. } => ErrorCode::DUPLICATE_PATTERN_BINDING,
            ErrorKind::StrViewReassignment => ErrorCode::STR_VIEW_REASSIGNMENT,

            // Struct/enum errors (E0400-E0499)
            ErrorKind::MissingFields(_) => ErrorCode::MISSING_FIELDS,
            ErrorKind::UnknownField { .. } => ErrorCode::UNKNOWN_FIELD,
            ErrorKind::DuplicateField { .. } => ErrorCode::DUPLICATE_FIELD,
            ErrorKind::EmptyStruct => ErrorCode::EMPTY_STRUCT,
            ErrorKind::CopyStructNonCopyField(_) => ErrorCode::COPY_STRUCT_NON_COPY_FIELD,
            ErrorKind::ReservedTypeName { .. } => ErrorCode::RESERVED_TYPE_NAME,
            ErrorKind::ReservedFunctionName { .. } => ErrorCode::RESERVED_FUNCTION_NAME,
            ErrorKind::DuplicateTypeDefinition { .. } => ErrorCode::DUPLICATE_TYPE_DEFINITION,
            ErrorKind::DuplicateTestDefinition { .. } => ErrorCode::DUPLICATE_TEST_DEFINITION,
            ErrorKind::DuplicateFunctionDefinition { .. }
            | ErrorKind::DuplicateMixedKindDefinition { .. } => {
                ErrorCode::DUPLICATE_FUNCTION_DEFINITION
            }
            ErrorKind::LinearValueNotConsumed(_) => ErrorCode::LINEAR_VALUE_NOT_CONSUMED,
            ErrorKind::LinearValueNotConsumedOnAllPaths(_) => {
                ErrorCode::LINEAR_VALUE_NOT_CONSUMED_ON_ALL_PATHS
            }
            ErrorKind::LinearValueDiscarded { .. } => ErrorCode::LINEAR_VALUE_DISCARDED,
            ErrorKind::LinearPayloadDiscarded { .. } => ErrorCode::LINEAR_PAYLOAD_DISCARDED,
            ErrorKind::LinearValueOverwritten { .. } => ErrorCode::LINEAR_VALUE_OVERWRITTEN,
            ErrorKind::LinearValueOverwrittenThroughInout { .. } => {
                ErrorCode::LINEAR_VALUE_OVERWRITTEN_THROUGH_INOUT
            }
            ErrorKind::LinearStructCopy(_) => ErrorCode::LINEAR_STRUCT_COPY,
            ErrorKind::LinearFieldDroppedByDestructure(_) => {
                ErrorCode::LINEAR_FIELD_DROPPED_BY_DESTRUCTURE
            }
            ErrorKind::DuplicateMethod { .. } => ErrorCode::DUPLICATE_METHOD,
            ErrorKind::DuplicateParameter { .. } => ErrorCode::DUPLICATE_PARAMETER,
            ErrorKind::UndefinedMethod { .. } => ErrorCode::UNDEFINED_METHOD,
            ErrorKind::UndefinedAssocFn { .. } => ErrorCode::UNDEFINED_ASSOC_FN,
            ErrorKind::MethodCallOnNonStruct { .. } => ErrorCode::METHOD_CALL_ON_NON_STRUCT,
            ErrorKind::MethodCalledAsAssocFn { .. } => ErrorCode::METHOD_CALLED_AS_ASSOC_FN,
            ErrorKind::AssocFnCalledAsMethod { .. } => ErrorCode::ASSOC_FN_CALLED_AS_METHOD,
            ErrorKind::DuplicateDestructor { .. } => ErrorCode::DUPLICATE_DESTRUCTOR,
            ErrorKind::DestructorUnknownType { .. } => ErrorCode::DESTRUCTOR_UNKNOWN_TYPE,
            ErrorKind::DuplicateConstant { .. } => ErrorCode::DUPLICATE_CONSTANT,
            ErrorKind::ConstExprNotSupported { .. } => ErrorCode::CONST_EXPR_NOT_SUPPORTED,
            ErrorKind::ConstInitializerCycle { .. } => ErrorCode::CONST_INITIALIZER_CYCLE,
            ErrorKind::RecursiveTypeInfiniteSize { .. } => ErrorCode::RECURSIVE_TYPE_INFINITE_SIZE,
            ErrorKind::ConstMissingTypeAnnotation { .. } => {
                ErrorCode::CONST_MISSING_TYPE_ANNOTATION
            }
            ErrorKind::DuplicateVariant { .. } => ErrorCode::DUPLICATE_VARIANT,
            ErrorKind::UnknownVariant { .. } => ErrorCode::UNKNOWN_VARIANT,
            ErrorKind::UnknownEnumType(_) => ErrorCode::UNKNOWN_ENUM_TYPE,
            ErrorKind::FieldAccessOnNonStruct { .. } => ErrorCode::FIELD_ACCESS_ON_NON_STRUCT,
            ErrorKind::InvalidAssignmentTarget => ErrorCode::INVALID_ASSIGNMENT_TARGET,
            ErrorKind::RawRequiresPlace => ErrorCode::RAW_REQUIRES_PLACE,
            ErrorKind::FieldPtrRequiresField => ErrorCode::FIELD_PTR_REQUIRES_FIELD,
            ErrorKind::StrFixedCapacityExceeded { .. } => ErrorCode::STR_FIXED_CAPACITY_EXCEEDED,
            ErrorKind::BufferNotFirstClassStr { .. } => ErrorCode::BUFFER_NOT_FIRST_CLASS_STR,
            ErrorKind::InoutStrRequiresLocalBuffer => ErrorCode::INOUT_STR_REQUIRES_LOCAL_BUFFER,
            ErrorKind::StrViewNotFirstClass { .. } => ErrorCode::STR_VIEW_NOT_FIRST_CLASS,
            ErrorKind::ContainerElementIsLinear { .. } => ErrorCode::CONTAINER_ELEMENT_IS_LINEAR,
            ErrorKind::InoutNonLvalue => ErrorCode::INOUT_NON_LVALUE,
            ErrorKind::InoutExclusiveAccess { .. } => ErrorCode::INOUT_EXCLUSIVE_ACCESS,
            ErrorKind::BorrowNonLvalue => ErrorCode::BORROW_NON_LVALUE,
            ErrorKind::MutateBorrowedValue { .. } => ErrorCode::MUTATE_BORROWED_VALUE,
            ErrorKind::MoveOutOfBorrow { .. } => ErrorCode::MOVE_OUT_OF_BORROW,
            ErrorKind::BorrowInoutConflict { .. } => ErrorCode::BORROW_INOUT_CONFLICT,
            ErrorKind::MoveWhileCallLoaned { .. } => ErrorCode::MOVE_WHILE_CALL_LOANED,
            ErrorKind::AccessorResultReturned { .. } => ErrorCode::ACCESSOR_RESULT_RETURNED,
            ErrorKind::AccessorResultStored { .. } => ErrorCode::ACCESSOR_RESULT_STORED,
            ErrorKind::AccessorResultBound { .. } => ErrorCode::ACCESSOR_RESULT_BOUND,
            ErrorKind::AccessorResultCaptured { .. } => ErrorCode::ACCESSOR_RESULT_CAPTURED,
            ErrorKind::AccessorBodyMissingYield | ErrorKind::AccessorBodyOtherExit { .. } => {
                ErrorCode::ACCESSOR_BODY_MISSING_YIELD
            }
            ErrorKind::AccessorYieldNotReceiverRooted { .. } => {
                ErrorCode::ACCESSOR_YIELD_NOT_RECEIVER_ROOTED
            }
            ErrorKind::YieldOutsideAccessor => ErrorCode::YIELD_OUTSIDE_ACCESSOR,
            ErrorKind::AccessorRequiresBorrowSelf { .. } => {
                ErrorCode::ACCESSOR_REQUIRES_BORROW_SELF
            }
            ErrorKind::AccessorResultMoved { .. } => ErrorCode::ACCESSOR_RESULT_MOVED,
            ErrorKind::AccessorLoanConflict { .. } => ErrorCode::ACCESSOR_LOAN_CONFLICT,
            ErrorKind::AccessorParamModeUnsupported { .. } => {
                ErrorCode::ACCESSOR_PARAM_MODE_UNSUPPORTED
            }
            ErrorKind::AccessorRecursion { .. } => ErrorCode::ACCESSOR_RECURSION,
            ErrorKind::InoutKeywordMissing => ErrorCode::INOUT_KEYWORD_MISSING,
            ErrorKind::BorrowKeywordMissing => ErrorCode::BORROW_KEYWORD_MISSING,
            ErrorKind::UnexpectedCallArgumentMode { .. } => {
                ErrorCode::UNEXPECTED_CALL_ARGUMENT_MODE
            }
            ErrorKind::MoveOutOfInout { .. } => ErrorCode::MOVE_OUT_OF_INOUT,
            ErrorKind::MoveSelfOutOfDestructor { .. } => ErrorCode::MOVE_SELF_OUT_OF_DESTRUCTOR,
            ErrorKind::MoveFieldOutOfDestructorType { .. } => {
                ErrorCode::MOVE_FIELD_OUT_OF_DESTRUCTOR_TYPE
            }
            ErrorKind::CopyStructWithDestructor { .. } => ErrorCode::COPY_STRUCT_WITH_DESTRUCTOR,

            // Control flow errors (E0500-E0599)
            ErrorKind::BreakOutsideLoop => ErrorCode::BREAK_OUTSIDE_LOOP,
            ErrorKind::ContinueOutsideLoop => ErrorCode::CONTINUE_OUTSIDE_LOOP,
            ErrorKind::BreakWithValue => ErrorCode::BREAK_WITH_VALUE,
            ErrorKind::QuestionOutsideOptionFn { .. } => ErrorCode::QUESTION_OUTSIDE_OPTION_FN,
            ErrorKind::QuestionOnNonOption { .. } => ErrorCode::QUESTION_ON_NON_OPTION,
            ErrorKind::QuestionOutsideResultFn { .. } => ErrorCode::QUESTION_OUTSIDE_RESULT_FN,
            ErrorKind::QuestionErrTypeMismatch { .. } => ErrorCode::QUESTION_ERR_TYPE_MISMATCH,

            // Match errors (E0600-E0699)
            ErrorKind::NonExhaustiveMatch => ErrorCode::NON_EXHAUSTIVE_MATCH,
            ErrorKind::EmptyMatch => ErrorCode::EMPTY_MATCH,
            ErrorKind::InvalidMatchType(_) => ErrorCode::INVALID_MATCH_TYPE,

            // Intrinsic errors (E0700-E0799)
            ErrorKind::UnknownIntrinsic(_) => ErrorCode::UNKNOWN_INTRINSIC,
            ErrorKind::IntrinsicWrongArgCount { .. } => ErrorCode::INTRINSIC_WRONG_ARG_COUNT,
            ErrorKind::IntrinsicTypeMismatch(_) => ErrorCode::INTRINSIC_TYPE_MISMATCH,
            ErrorKind::CannotInferCastTarget(_) => ErrorCode::CANNOT_INFER_CAST_TARGET,
            ErrorKind::BitCastWidthMismatch { .. } => ErrorCode::BIT_CAST_WIDTH_MISMATCH,
            ErrorKind::CannotInferPointeeType(_) => ErrorCode::CANNOT_INFER_POINTEE_TYPE,
            ErrorKind::IntrinsicAlignNotPowerOfTwo { .. } => {
                ErrorCode::INTRINSIC_ALIGN_NOT_POWER_OF_TWO
            }
            ErrorKind::ContainerElementNotTriviallyDroppable { .. } => {
                ErrorCode::CONTAINER_ELEMENT_NOT_TRIVIALLY_DROPPABLE
            }
            ErrorKind::ImportRequiresStringLiteral => ErrorCode::IMPORT_REQUIRES_STRING_LITERAL,
            ErrorKind::ModuleNotFound { .. } => ErrorCode::MODULE_NOT_FOUND,
            ErrorKind::ImportEscapesRoot { .. } => ErrorCode::IMPORT_ESCAPES_ROOT,
            ErrorKind::ImportSpecifierEmpty | ErrorKind::ImportSpecifierAbsolute { .. } => {
                ErrorCode::IMPORT_SPECIFIER_NOT_RELATIVE
            }
            ErrorKind::ImportSpellingsSameFile { .. } => ErrorCode::IMPORT_SPELLINGS_SAME_FILE,
            ErrorKind::StdLibNotFound => ErrorCode::STD_LIB_NOT_FOUND,
            ErrorKind::PrivateMemberAccess { .. } => ErrorCode::PRIVATE_MEMBER_ACCESS,
            ErrorKind::PrivateUnqualifiedAccess(_) => ErrorCode::PRIVATE_UNQUALIFIED_ACCESS,
            ErrorKind::UnknownModuleMember { .. } => ErrorCode::UNKNOWN_MODULE_MEMBER,

            // Literal/operator errors (E0800-E0899)
            ErrorKind::LiteralOutOfRange { .. } => ErrorCode::LITERAL_OUT_OF_RANGE,
            ErrorKind::CannotNegate(_) => ErrorCode::CANNOT_NEGATE,
            ErrorKind::ChainedComparison => ErrorCode::CHAINED_COMPARISON,

            // Array errors (E0900-E0999)
            ErrorKind::IndexOnNonArray { .. } => ErrorCode::INDEX_ON_NON_ARRAY,
            ErrorKind::ArrayLengthMismatch { .. } => ErrorCode::ARRAY_LENGTH_MISMATCH,
            ErrorKind::IndexOutOfBounds { .. } => ErrorCode::INDEX_OUT_OF_BOUNDS,
            ErrorKind::TypeAnnotationRequired => ErrorCode::TYPE_ANNOTATION_REQUIRED,
            ErrorKind::MoveOutOfIndex { .. } => ErrorCode::MOVE_OUT_OF_INDEX,
            ErrorKind::ArrayRepeatNonCopy { .. } => ErrorCode::ARRAY_REPEAT_NON_COPY,
            ErrorKind::TypeTooLarge { .. } => ErrorCode::TYPE_TOO_LARGE,
            ErrorKind::FunctionFrameTooLarge { .. } => ErrorCode::FUNCTION_FRAME_TOO_LARGE,
            ErrorKind::SliceFrameArrayNotSupported { .. } => {
                ErrorCode::SLICE_FRAME_ARRAY_NOT_SUPPORTED
            }
            ErrorKind::AssignToPartiallyMovedArray { .. } => {
                ErrorCode::ASSIGN_TO_PARTIALLY_MOVED_ARRAY
            }

            // Linker/target errors (E1000-E1099)
            ErrorKind::LinkError(_) => ErrorCode::LINK_ERROR,
            ErrorKind::UnsupportedTarget(_) => ErrorCode::UNSUPPORTED_TARGET,

            // Preview feature errors (E1100-E1199)
            ErrorKind::PreviewFeatureRequired { .. } => ErrorCode::PREVIEW_FEATURE_REQUIRED,
            ErrorKind::ExternSignatureTypeUnsupported { .. } => {
                ErrorCode::EXTERN_SIGNATURE_TYPE_UNSUPPORTED
            }
            ErrorKind::ExternAggregateNotReprC { .. } => ErrorCode::EXTERN_AGGREGATE_NOT_REPR_C,
            ErrorKind::ExternArrayByValue { .. } => ErrorCode::EXTERN_ARRAY_BY_VALUE,
            ErrorKind::ExternVariadicUnsupported => ErrorCode::EXTERN_VARIADIC_UNSUPPORTED,
            ErrorKind::ExportSignatureUnsupported { .. } => ErrorCode::EXPORT_SIGNATURE_UNSUPPORTED,
            ErrorKind::ForeignSignatureConflict(_) => ErrorCode::FOREIGN_SIGNATURE_CONFLICT,
            ErrorKind::ForeignEntryPointDeclaration => ErrorCode::FOREIGN_ENTRY_POINT_DECLARATION,
            ErrorKind::ReprCStructIneligible(_) => ErrorCode::REPR_C_STRUCT_INELIGIBLE,
            ErrorKind::FloatNotYetImplemented => ErrorCode::FLOAT_NOT_YET_IMPLEMENTED,
            ErrorKind::SliceReturnNotAllowed => ErrorCode::SLICE_RETURN_NOT_ALLOWED,
            ErrorKind::SliceInAggregateField => ErrorCode::SLICE_IN_AGGREGATE_FIELD,
            ErrorKind::SliceEscapesScope => ErrorCode::SLICE_ESCAPES_SCOPE,

            // Comptime errors (E1200-E1299)
            ErrorKind::ComptimeEvaluationFailed { .. } => ErrorCode::COMPTIME_EVALUATION_FAILED,
            ErrorKind::ComptimeArgNotConst { .. } => ErrorCode::COMPTIME_ARG_NOT_CONST,

            // Unchecked-code errors (E1300-E1399)
            ErrorKind::UncheckedOpRequiresChecked { .. } => {
                ErrorCode::UNCHECKED_OP_REQUIRES_CHECKED
            }

            // Compiler-input errors (E1400-E1499)
            ErrorKind::InvalidCompilerInput(_) => ErrorCode::INVALID_COMPILER_INPUT,
            ErrorKind::CompilerResourceLimit(_) => ErrorCode::COMPILER_RESOURCE_LIMIT,
            ErrorKind::CompilerResourceExhaustion(_) => ErrorCode::COMPILER_RESOURCE_EXHAUSTION,
            ErrorKind::OutputPublication(_) => ErrorCode::OUTPUT_PUBLICATION,
            ErrorKind::UnsatisfiedTrustedToolchainInput(_) => {
                ErrorCode::UNSATISFIED_TRUSTED_TOOLCHAIN_INPUT
            }

            // Internal compiler errors (E9000-E9999)
            ErrorKind::CompilerProducerInvariant(_) | ErrorKind::InternalError(_) => {
                ErrorCode::INTERNAL_ERROR
            }
            ErrorKind::InternalCodegenError(_) => ErrorCode::INTERNAL_CODEGEN_ERROR,
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// Rebind every source location in every contained diagnostic.
    pub fn map_spans(self, mut map: impl FnMut(Span) -> Span) -> Self {
        Self {
            errors: self
                .errors
                .into_iter()
                .map(|error| error.map_spans(&mut map))
                .collect(),
        }
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
    /// let output = analyze(input);
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
    /// A lookup's fallback sentinel is compared with the same sentinel to test absence.
    #[error("integer sentinel used to test lookup absence")]
    SentinelLookup,
    /// A declared test candidate outside the request's module closure declares
    /// test items, so nothing roots them (ADR-0083 §1).
    ///
    /// This is a request-level warning with no source site: the file is not in
    /// the closure, so the compiler holds no span inside it. It is never an
    /// error — sibling build targets legitimately share a `srcs` glob, so an
    /// unread candidate may belong to another root's tree.
    #[error(
        "test file '{path}' declares {tests} test{plural} but no module in the compiled closure imports it",
        plural = if *tests == 1 { "" } else { "s" }
    )]
    UnimportedTestFile { path: String, tests: u32 },
    /// A declared test candidate outside the request's module closure could not
    /// be read or parsed, so whether it declares tests is unknown (ADR-0083 §1).
    #[error("test file '{path}' is outside the compiled closure and could not be parsed")]
    UnimportedTestFileUnparsable { path: String },
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
    /// Returns the declaration name for warning kinds that should use line
    /// numbers to disambiguate duplicate names.
    pub fn unused_variable_name(&self) -> Option<&str> {
        match self {
            WarningKind::UnusedVariable(name) | WarningKind::UnusedFunction(name) => Some(name),
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

    /// Every ErrorCode constant must map to a distinct number: two diagnostics
    /// sharing an E-code would be indistinguishable to users and tests.
    /// The macro-emitted declaration table cannot omit a constant because both
    /// are expanded from the same entry.
    #[test]
    fn test_error_codes_are_unique() {
        let mut seen: std::collections::HashMap<u16, &str> = std::collections::HashMap::new();
        for &(code, name) in ERROR_CODE_DECLARATIONS {
            if let Some(prev) = seen.insert(code.0, name) {
                panic!("duplicate ErrorCode {}: {prev} and {name}", code.0);
            }
        }
        assert!(
            seen.len() > 50,
            "expected to find the ErrorCode constants (found {})",
            seen.len()
        );
    }

    #[test]
    fn test_error_code_metadata_is_complete_unique_and_stably_ordered() {
        let metadata = error_code_metadata();
        assert!(metadata.len() > 50);
        assert!(
            metadata
                .windows(2)
                .all(|pair| pair[0].code.0 < pair[1].code.0),
            "metadata must be unique and ordered by numeric code"
        );
        assert!(metadata.iter().all(|entry| {
            !entry.name.is_empty()
                && !entry.title.is_empty()
                && !entry.category.title().is_empty()
                && entry.stability == ErrorCodeStability::Permanent
                && entry.source_path == "crates/rue-error/src/lib.rs"
        }));

        assert_eq!(
            error_code_category(ErrorCode::UNEXPECTED_CHARACTER),
            Some(ErrorCodeCategory::Lexer)
        );
        assert_eq!(
            error_code_category(ErrorCode::COMPILER_RESOURCE_LIMIT),
            Some(ErrorCodeCategory::CompilerInput)
        );
        assert_eq!(error_code_category(ErrorCode(1600)), None);

        assert_eq!(metadata.len(), ERROR_CODE_DECLARATIONS.len());
        assert_eq!(metadata, error_code_metadata(), "metadata must reproduce");
    }

    #[test]
    fn retired_error_codes_are_complete_ordered_and_disjoint_from_active_codes() {
        assert!(
            RETIRED_ERROR_CODES
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0),
            "retired codes must be unique and numerically ordered"
        );
        for retired in RETIRED_ERROR_CODES {
            assert!(
                error_code_metadata()
                    .binary_search_by_key(&retired.0, |entry| entry.code.0)
                    .is_err(),
                "retired code {retired} must not remain active"
            );
            assert_eq!(
                retired.to_string().parse::<ErrorCode>(),
                Err(ParseErrorCodeError::Unknown(*retired))
            );
        }
    }

    #[test]
    fn error_code_parsing_uses_the_canonical_inventory() {
        assert_eq!("E0201".parse(), Ok(ErrorCode::UNDEFINED_VARIABLE));
        assert_eq!(
            "E0005".parse::<ErrorCode>(),
            Err(ParseErrorCodeError::Unknown(ErrorCode(5)))
        );
        assert_eq!(
            "E9999".parse::<ErrorCode>(),
            Err(ParseErrorCodeError::Unknown(ErrorCode(9999)))
        );
        for malformed in ["0201", "e0201", "E201", "E02010", "E02A1", ""] {
            assert_eq!(
                malformed.parse::<ErrorCode>(),
                Err(ParseErrorCodeError::Malformed),
                "{malformed:?} must not be accepted"
            );
        }
    }

    #[test]
    fn explanations_are_macro_owned_and_retain_metadata_identity() {
        let mut seen = HashSet::new();
        for declaration in ERROR_CODE_EXPLANATION_DECLARATIONS {
            assert!(seen.insert(declaration.code), "duplicate explanation code");
            let explanation = error_code_explanation(declaration.code)
                .expect("each macro explanation declaration must be queryable");
            let metadata = error_code_metadata()
                .iter()
                .find(|entry| entry.code == declaration.code)
                .expect("each explanation must reference declared metadata");
            assert!(std::ptr::eq(explanation.metadata, metadata));
            assert!(!explanation.explanation.is_empty());
            assert!(!explanation.likely_cause.is_empty());
            assert!((1..=2).contains(&explanation.examples.len()));
            assert!(!explanation.references.is_empty());
            for example in explanation.examples {
                assert!(!example.title.is_empty());
                assert!(!example.source.is_empty());
            }
            for reference in explanation.references {
                assert!(!reference.title.is_empty());
                assert!(reference.path.starts_with("docs/spec/src/"));
                assert!(reference.path.ends_with(".md"));
                assert!(reference.rule.is_some_and(|rule| !rule.is_empty()));
            }
        }

        let explanation = error_code_explanation(ErrorCode::UNDEFINED_VARIABLE)
            .expect("E0201 retains its canonical explanation");
        assert_eq!(explanation.metadata.name, "UNDEFINED_VARIABLE");
        assert_eq!(explanation.references[0].rule, Some("10.3:8"));
    }

    #[test]
    fn semantic_foundation_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::NO_MAIN_FUNCTION,
            ErrorCode::UNDEFINED_VARIABLE,
            ErrorCode::UNDEFINED_FUNCTION,
            ErrorCode::ASSIGN_TO_IMMUTABLE,
            ErrorCode::UNKNOWN_TYPE,
            ErrorCode::USE_AFTER_MOVE,
            ErrorCode::TYPE_MISMATCH,
            ErrorCode::WRONG_ARGUMENT_COUNT,
            ErrorCode::MOVE_WHILE_CALL_LOANED,
            ErrorCode::UNEXPECTED_CALL_ARGUMENT_MODE,
            ErrorCode::STR_VIEW_REASSIGNMENT,
            ErrorCode::INVALID_MAIN_SIGNATURE,
        ];
        let actual = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (200..=211)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );
    }

    #[test]
    fn active_control_flow_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::BREAK_OUTSIDE_LOOP,
            ErrorCode::CONTINUE_OUTSIDE_LOOP,
            ErrorCode::BREAK_WITH_VALUE,
            ErrorCode::QUESTION_OUTSIDE_OPTION_FN,
            ErrorCode::QUESTION_ON_NON_OPTION,
            ErrorCode::QUESTION_OUTSIDE_RESULT_FN,
            ErrorCode::QUESTION_ERR_TYPE_MISMATCH,
        ];
        let active = error_code_metadata()
            .iter()
            .filter_map(|metadata| {
                (500..=506)
                    .contains(&metadata.code.0)
                    .then_some(metadata.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (500..=506)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );
    }

    #[test]
    fn active_match_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::NON_EXHAUSTIVE_MATCH,
            ErrorCode::EMPTY_MATCH,
            ErrorCode::INVALID_MATCH_TYPE,
        ];
        let active = error_code_metadata()
            .iter()
            .filter_map(|metadata| {
                (600..=699)
                    .contains(&metadata.code.0)
                    .then_some(metadata.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (600..=699)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );
    }

    #[test]
    fn active_intrinsic_and_module_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::UNKNOWN_INTRINSIC,
            ErrorCode::INTRINSIC_WRONG_ARG_COUNT,
            ErrorCode::INTRINSIC_TYPE_MISMATCH,
            ErrorCode::IMPORT_REQUIRES_STRING_LITERAL,
            ErrorCode::MODULE_NOT_FOUND,
            ErrorCode::STD_LIB_NOT_FOUND,
            ErrorCode::PRIVATE_MEMBER_ACCESS,
            ErrorCode::UNKNOWN_MODULE_MEMBER,
        ];
        let active = error_code_metadata()
            .iter()
            .filter(|metadata| (700..=708).contains(&metadata.code.0))
            .map(|metadata| {
                assert_eq!(metadata.source_path, "crates/rue-error/src/lib.rs");
                metadata.code
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (700..=708)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );

        let retired = ErrorCode(708);
        assert!(RETIRED_ERROR_CODES.contains(&retired));
        assert_eq!(error_code_explanation(retired), None);
        assert_eq!(
            retired.to_string().parse::<ErrorCode>(),
            Err(ParseErrorCodeError::Unknown(retired))
        );
    }

    #[test]
    fn active_lexer_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::UNEXPECTED_CHARACTER,
            ErrorCode::INVALID_INTEGER,
            ErrorCode::INVALID_STRING_ESCAPE,
            ErrorCode::UNTERMINATED_STRING,
            ErrorCode::UPPERCASE_BASE_PREFIX,
            ErrorCode::EMPTY_BASED_LITERAL,
            ErrorCode::INVALID_DIGIT_FOR_BASE,
            ErrorCode::MALFORMED_BYTE_LITERAL,
            ErrorCode::LEXER_DIAGNOSTICS_OMITTED,
            ErrorCode::MALFORMED_FLOAT_LITERAL,
        ];
        let active = error_code_metadata()
            .iter()
            .filter_map(|metadata| (1..=11).contains(&metadata.code.0).then_some(metadata.code))
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (1..=11)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );
        assert_eq!(error_code_explanation(ErrorCode(5)), None);
    }

    #[test]
    fn active_parser_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::UNEXPECTED_TOKEN,
            ErrorCode::PARSE_ERROR,
            ErrorCode::PARSER_DIAGNOSTICS_OMITTED,
        ];
        let active = error_code_metadata()
            .iter()
            .filter_map(|metadata| {
                (100..=103)
                    .contains(&metadata.code.0)
                    .then_some(metadata.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (100..=103)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );
        assert!(RETIRED_ERROR_CODES.contains(&ErrorCode(101)));
        assert_eq!(error_code_explanation(ErrorCode(101)), None);
    }

    #[test]
    fn active_struct_foundation_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::MISSING_FIELDS,
            ErrorCode::UNKNOWN_FIELD,
            ErrorCode::DUPLICATE_FIELD,
            ErrorCode::COPY_STRUCT_NON_COPY_FIELD,
            ErrorCode::RESERVED_TYPE_NAME,
            ErrorCode::DUPLICATE_TYPE_DEFINITION,
            ErrorCode::LINEAR_VALUE_NOT_CONSUMED,
            ErrorCode::LINEAR_STRUCT_COPY,
        ];
        let active = error_code_metadata()
            .iter()
            .filter(|metadata| (400..=409).contains(&metadata.code.0))
            .map(|metadata| {
                assert_eq!(metadata.source_path, "crates/rue-error/src/lib.rs");
                metadata.code
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (400..=409)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );
        for retired in [ErrorCode(408), ErrorCode(409)] {
            assert!(RETIRED_ERROR_CODES.contains(&retired));
            assert_eq!(error_code_explanation(retired), None);
            assert_eq!(
                retired.to_string().parse::<ErrorCode>(),
                Err(ParseErrorCodeError::Unknown(retired))
            );
        }
    }

    #[test]
    fn active_method_and_item_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::DUPLICATE_METHOD,
            ErrorCode::UNDEFINED_METHOD,
            ErrorCode::UNDEFINED_ASSOC_FN,
            ErrorCode::METHOD_CALL_ON_NON_STRUCT,
            ErrorCode::METHOD_CALLED_AS_ASSOC_FN,
            ErrorCode::ASSOC_FN_CALLED_AS_METHOD,
            ErrorCode::DUPLICATE_DESTRUCTOR,
            ErrorCode::DESTRUCTOR_UNKNOWN_TYPE,
            ErrorCode::DUPLICATE_CONSTANT,
        ];
        let active = error_code_metadata()
            .iter()
            .filter(|metadata| (410..=418).contains(&metadata.code.0))
            .map(|metadata| {
                assert_eq!(metadata.source_path, "crates/rue-error/src/lib.rs");
                metadata.code
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (410..=418)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );
    }

    #[test]
    fn active_enum_and_place_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::DUPLICATE_VARIANT,
            ErrorCode::UNKNOWN_VARIANT,
            ErrorCode::UNKNOWN_ENUM_TYPE,
            ErrorCode::FIELD_ACCESS_ON_NON_STRUCT,
            ErrorCode::INVALID_ASSIGNMENT_TARGET,
        ];
        let active = error_code_metadata()
            .iter()
            .filter(|metadata| (419..=424).contains(&metadata.code.0))
            .map(|metadata| {
                assert_eq!(metadata.source_path, "crates/rue-error/src/lib.rs");
                metadata.code
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (419..=424)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );
        let retired = ErrorCode(422);
        assert!(RETIRED_ERROR_CODES.contains(&retired));
        assert_eq!(error_code_explanation(retired), None);
        assert_eq!(
            retired.to_string().parse::<ErrorCode>(),
            Err(ParseErrorCodeError::Unknown(retired))
        );
    }

    #[test]
    fn active_place_and_borrow_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::INOUT_NON_LVALUE,
            ErrorCode::INOUT_EXCLUSIVE_ACCESS,
            ErrorCode::BORROW_NON_LVALUE,
            ErrorCode::MUTATE_BORROWED_VALUE,
            ErrorCode::MOVE_OUT_OF_BORROW,
            ErrorCode::BORROW_INOUT_CONFLICT,
            ErrorCode::INOUT_KEYWORD_MISSING,
            ErrorCode::BORROW_KEYWORD_MISSING,
            ErrorCode::EMPTY_STRUCT,
        ];
        let active = error_code_metadata()
            .iter()
            .filter(|metadata| (425..=433).contains(&metadata.code.0))
            .map(|metadata| {
                assert_eq!(metadata.source_path, "crates/rue-error/src/lib.rs");
                metadata.code
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (425..=433)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );
    }

    #[test]
    fn active_const_item_and_ownership_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::CONST_EXPR_NOT_SUPPORTED,
            ErrorCode::RESERVED_FUNCTION_NAME,
            ErrorCode::DUPLICATE_FUNCTION_DEFINITION,
            ErrorCode::MOVE_OUT_OF_INOUT,
            ErrorCode::MOVE_SELF_OUT_OF_DESTRUCTOR,
            ErrorCode::LINEAR_VALUE_NOT_CONSUMED_ON_ALL_PATHS,
        ];
        let active = error_code_metadata()
            .iter()
            .filter(|metadata| (434..=443).contains(&metadata.code.0))
            .map(|metadata| {
                assert_eq!(metadata.source_path, "crates/rue-error/src/lib.rs");
                metadata.code
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (434..=443)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );

        let retired = ErrorCode(438);
        assert!(RETIRED_ERROR_CODES.contains(&retired));
        assert_eq!(error_code_explanation(retired), None);
        assert_eq!(
            retired.to_string().parse::<ErrorCode>(),
            Err(ParseErrorCodeError::Unknown(retired))
        );

        for reserved in [ErrorCode(439), ErrorCode(440), ErrorCode(441)] {
            assert!(!RETIRED_ERROR_CODES.contains(&reserved));
            assert!(
                error_code_metadata()
                    .iter()
                    .all(|metadata| metadata.code != reserved)
            );
            assert_eq!(error_code_explanation(reserved), None);
            assert_eq!(
                reserved.to_string().parse::<ErrorCode>(),
                Err(ParseErrorCodeError::Unknown(reserved))
            );
        }
    }

    #[test]
    fn active_destructor_privacy_and_const_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::MOVE_FIELD_OUT_OF_DESTRUCTOR_TYPE,
            ErrorCode::COPY_STRUCT_WITH_DESTRUCTOR,
            ErrorCode::PRIVATE_UNQUALIFIED_ACCESS,
            ErrorCode::CONST_INITIALIZER_CYCLE,
        ];
        let active = error_code_metadata()
            .iter()
            .filter(|metadata| (456..=461).contains(&metadata.code.0))
            .map(|metadata| {
                assert_eq!(metadata.source_path, "crates/rue-error/src/lib.rs");
                metadata.code
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (456..=461)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );

        for reserved in [ErrorCode(458), ErrorCode(459)] {
            assert!(!RETIRED_ERROR_CODES.contains(&reserved));
            assert!(
                error_code_metadata()
                    .iter()
                    .all(|metadata| metadata.code != reserved)
            );
            assert_eq!(error_code_explanation(reserved), None);
            assert_eq!(
                reserved.to_string().parse::<ErrorCode>(),
                Err(ParseErrorCodeError::Unknown(reserved))
            );
        }
    }

    #[test]
    fn active_ownership_array_resource_type_and_pointer_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::LINEAR_FIELD_DROPPED_BY_DESTRUCTURE,
            ErrorCode::CONST_MISSING_TYPE_ANNOTATION,
            ErrorCode::LINEAR_VALUE_DISCARDED,
            ErrorCode::ASSIGN_TO_PARTIALLY_MOVED_ARRAY,
            ErrorCode::INVALID_ARRAY_LENGTH,
            ErrorCode::NESTING_LIMIT_EXCEEDED,
            ErrorCode::RECURSIVE_TYPE_INFINITE_SIZE,
            ErrorCode::DUPLICATE_PATTERN_BINDING,
            ErrorCode::RAW_REQUIRES_PLACE,
        ];
        let active = error_code_metadata()
            .iter()
            .filter(|metadata| (474..=485).contains(&metadata.code.0))
            .map(|metadata| {
                assert_eq!(metadata.source_path, "crates/rue-error/src/lib.rs");
                metadata.code
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (474..=485)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );

        for reserved in [ErrorCode(476), ErrorCode(477), ErrorCode(479)] {
            assert!(!RETIRED_ERROR_CODES.contains(&reserved));
            assert!(
                error_code_metadata()
                    .iter()
                    .all(|metadata| metadata.code != reserved)
            );
            assert!(
                ERROR_CODE_EXPLANATION_DECLARATIONS
                    .iter()
                    .all(|declaration| declaration.code != reserved)
            );
            assert_eq!(error_code_explanation(reserved), None);
            assert_eq!(
                reserved.to_string().parse::<ErrorCode>(),
                Err(ParseErrorCodeError::Unknown(reserved))
            );
        }
    }

    #[test]
    fn active_slice_string_and_container_explanation_band_is_complete_and_bounded() {
        let expected = [
            ErrorCode::LINEAR_PAYLOAD_DISCARDED,
            ErrorCode::SLICE_RETURN_NOT_ALLOWED,
            ErrorCode::SLICE_IN_AGGREGATE_FIELD,
            ErrorCode::SLICE_ESCAPES_SCOPE,
            ErrorCode::FIELD_PTR_REQUIRES_FIELD,
            ErrorCode::DUPLICATE_PARAMETER,
            ErrorCode::STR_FIXED_CAPACITY_EXCEEDED,
            ErrorCode::LINEAR_VALUE_OVERWRITTEN,
            ErrorCode::LINEAR_VALUE_OVERWRITTEN_THROUGH_INOUT,
            ErrorCode::BUFFER_NOT_FIRST_CLASS_STR,
            ErrorCode::INOUT_STR_REQUIRES_LOCAL_BUFFER,
            ErrorCode::STR_VIEW_NOT_FIRST_CLASS,
            ErrorCode::CONTAINER_ELEMENT_IS_LINEAR,
        ];
        let active = error_code_metadata()
            .iter()
            .filter(|metadata| (486..=499).contains(&metadata.code.0))
            .map(|metadata| {
                assert_eq!(metadata.source_path, "crates/rue-error/src/lib.rs");
                metadata.code
            })
            .collect::<Vec<_>>();
        assert_eq!(active, expected);
        let explained = ERROR_CODE_EXPLANATION_DECLARATIONS
            .iter()
            .filter_map(|declaration| {
                (486..=499)
                    .contains(&declaration.code.0)
                    .then_some(declaration.code)
            })
            .collect::<Vec<_>>();
        assert_eq!(explained, expected);
        assert!(
            expected
                .into_iter()
                .all(|code| error_code_explanation(code).is_some())
        );

        let retired = ErrorCode(498);
        assert!(RETIRED_ERROR_CODES.contains(&retired));
        assert!(
            error_code_metadata()
                .iter()
                .all(|metadata| metadata.code != retired)
        );
        assert!(
            ERROR_CODE_EXPLANATION_DECLARATIONS
                .iter()
                .all(|declaration| declaration.code != retired)
        );
        assert_eq!(error_code_explanation(retired), None);
        assert_eq!(
            retired.to_string().parse::<ErrorCode>(),
            Err(ParseErrorCodeError::Unknown(retired))
        );
    }

    /// `ErrorKind::code()` must cover the compiler-declared ErrorCode constants without
    /// accidental aliases. Intentional aliases (such as the two E0436
    /// duplicate-definition variants) share one match arm, and every constant
    /// must be used (a constant nobody maps to is dead, and usually a sign that
    /// a variant was repointed at someone else's code). Driver/host codes are
    /// intentionally outside this mapping because they classify `SourceLoadError`
    /// values at the CLI boundary rather than `ErrorKind` values.
    ///
    /// `test_error_codes_are_unique` (above) guards constant *values*; this
    /// test guards the variant -> constant *mapping*. It scans the body of
    /// `fn code()` in this file's source, so it can't go stale as variants
    /// are added. It deliberately pins uniqueness, not specific code values.
    #[test]
    fn test_error_kind_to_code_mapping_is_injective() {
        let src = include_str!("lib.rs");

        // Collect all declared ErrorCode constant names. Driver/host codes are
        // consumed by the CLI's SourceLoadError renderer, not ErrorKind::code().
        let mut declared: std::collections::HashSet<&str> = ERROR_CODE_DECLARATIONS
            .iter()
            .map(|(_, name)| *name)
            .collect();
        assert!(
            declared.len() > 50,
            "expected to find the ErrorCode constants (found {})",
            declared.len()
        );
        let driver_codes: std::collections::HashSet<_> = declared
            .iter()
            .copied()
            .filter(|name| name.starts_with("DRIVER_"))
            .collect();
        assert_eq!(
            driver_codes,
            std::collections::HashSet::from([
                "DRIVER_SOURCE_LOAD",
                "DRIVER_TOOLCHAIN_INTEGRITY",
                "DRIVER_HERMETIC_DENIAL",
            ])
        );
        declared.retain(|name| !name.starts_with("DRIVER_"));

        // Extract the body of `fn code()` by brace matching.
        let start = src
            .find("pub fn code(&self) -> ErrorCode {")
            .expect("fn code() not found in lib.rs");
        let open = start + src[start..].find('{').unwrap();
        let mut depth = 0usize;
        let mut end = open;
        for (i, ch) in src[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &src[open..=end];

        // Count every `ErrorCode::CONST` reference in the match arms.
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut rest = body;
        while let Some(pos) = rest.find("ErrorCode::") {
            rest = &rest[pos + "ErrorCode::".len()..];
            let name_len = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let name = &rest[..name_len];
            *counts.entry(name).or_insert(0) += 1;
        }

        let duplicates: Vec<_> = counts.iter().filter(|&(_, &c)| c > 1).collect();
        assert!(
            duplicates.is_empty(),
            "ErrorCode constants mapped by more than one ErrorKind variant in code(): {:?}",
            duplicates
        );

        let referenced: std::collections::HashSet<&str> = counts.keys().copied().collect();
        let orphans: Vec<_> = declared.difference(&referenced).collect();
        assert!(
            orphans.is_empty(),
            "ErrorCode constants declared but never mapped by any ErrorKind variant: {:?}",
            orphans
        );
        let undeclared: Vec<_> = referenced.difference(&declared).collect();
        assert!(
            undeclared.is_empty(),
            "code() references ErrorCode constants that are not declared: {:?}",
            undeclared
        );
    }

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
    fn test_unexpected_character_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedCharacter('@'));
        assert_eq!(error.to_string(), "unexpected character: @");
    }

    #[test]
    fn test_unexpected_character_escapes_invisible_chars() {
        // Control and format characters must not appear raw in the one-line
        // message; escape_debug renders them as visible escapes.
        let vt = CompileError::without_span(ErrorKind::UnexpectedCharacter('\u{b}'));
        assert_eq!(vt.to_string(), "unexpected character: \\u{b}");
        let nul = CompileError::without_span(ErrorKind::UnexpectedCharacter('\0'));
        assert_eq!(nul.to_string(), "unexpected character: \\0");
        let bom = CompileError::without_span(ErrorKind::UnexpectedCharacter('\u{feff}'));
        assert_eq!(bom.to_string(), "unexpected character: \\u{feff}");
        // Printable characters are unchanged, including non-ASCII.
        let acute = CompileError::without_span(ErrorKind::UnexpectedCharacter('é'));
        assert_eq!(acute.to_string(), "unexpected character: é");
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
        assert_eq!(
            names,
            "test_infra, c_ffi, floats, non_exhaustive_enums, test_declarations"
        );
    }

    #[test]
    fn test_preview_feature_floats() {
        // ADR-0065 (RUE-714): the floating-point preview feature, gating the
        // literal grammar in Phases 2-3 and everything M9 adds after it.
        let feature: PreviewFeature = "floats".parse().unwrap();
        assert_eq!(feature, PreviewFeature::Floats);
        assert_eq!(feature.name(), "floats");
        assert_eq!(feature.adr(), "ADR-0065");
        assert!(PreviewFeature::all().contains(&PreviewFeature::Floats));
    }

    #[test]
    fn test_float_literal_error_codes() {
        // The two halves of the float gate: E1100 without `--preview floats`
        // (the shared preview-gate kind), E1109 with it, plus the E0011
        // spelling rule from ADR-0065 §3.
        assert_eq!(
            ErrorKind::FloatNotYetImplemented.code(),
            ErrorCode::FLOAT_NOT_YET_IMPLEMENTED
        );
        assert_eq!(ErrorCode::FLOAT_NOT_YET_IMPLEMENTED.to_string(), "E1109");
        assert_eq!(
            ErrorKind::MalformedFloatLiteral("boom".to_string()).code(),
            ErrorCode::MALFORMED_FLOAT_LITERAL
        );
        assert_eq!(ErrorCode::MALFORMED_FLOAT_LITERAL.to_string(), "E0011");
        assert_eq!(
            ErrorKind::PreviewFeatureRequired {
                feature: PreviewFeature::Floats,
                what: "a floating-point literal".to_string(),
            }
            .code(),
            ErrorCode::PREVIEW_FEATURE_REQUIRED
        );
    }

    #[test]
    fn test_slice_second_class_escape_codes() {
        // ADR-0043 Phase 1 (RUE-322): the second-class-escape diagnostics for
        // a slice type used outside argument position.
        assert_eq!(
            ErrorKind::SliceReturnNotAllowed.code(),
            ErrorCode::SLICE_RETURN_NOT_ALLOWED
        );
        assert_eq!(
            ErrorKind::SliceInAggregateField.code(),
            ErrorCode::SLICE_IN_AGGREGATE_FIELD
        );
        assert_eq!(
            ErrorKind::SliceEscapesScope.code(),
            ErrorCode::SLICE_ESCAPES_SCOPE
        );
        // Codes are contiguous with the discarded-linear-payload gate
        // (E0486, RUE-1592).
        assert_eq!(ErrorCode::SLICE_RETURN_NOT_ALLOWED.0, 487);
        assert_eq!(ErrorCode::SLICE_IN_AGGREGATE_FIELD.0, 488);
        assert_eq!(ErrorCode::SLICE_ESCAPES_SCOPE.0, 489);
    }

    #[test]
    fn test_two_types_string_model_codes() {
        // ADR-0043 two-types model (RUE-386): first-class `str` vs. views.
        assert_eq!(
            ErrorKind::BufferNotFirstClassStr {
                found: "StrBuf".to_string(),
                site: "as a parameter argument".to_string(),
            }
            .code(),
            ErrorCode::BUFFER_NOT_FIRST_CLASS_STR
        );
        assert_eq!(
            ErrorKind::InoutStrRequiresLocalBuffer.code(),
            ErrorCode::INOUT_STR_REQUIRES_LOCAL_BUFFER
        );
        assert_eq!(
            ErrorKind::StrViewReassignment.code(),
            ErrorCode::STR_VIEW_REASSIGNMENT
        );
        assert_eq!(
            ErrorKind::StrViewNotFirstClass {
                site: "as a return value".to_string(),
            }
            .code(),
            ErrorCode::STR_VIEW_NOT_FIRST_CLASS
        );
        assert_eq!(ErrorCode::BUFFER_NOT_FIRST_CLASS_STR.0, 495);
        assert_eq!(ErrorCode::INOUT_STR_REQUIRES_LOCAL_BUFFER.0, 496);
        assert_eq!(ErrorCode::STR_VIEW_NOT_FIRST_CLASS.0, 497);
        assert_eq!(ErrorCode::STR_VIEW_REASSIGNMENT.0, 210);
    }

    #[test]
    fn test_preview_feature_test_infra_roundtrip() {
        use std::str::FromStr;
        let f = PreviewFeature::from_str("test_infra").unwrap();
        assert_eq!(f, PreviewFeature::TestInfra);
        assert_eq!(f.name(), "test_infra");
    }

    #[test]
    fn test_preview_feature_stabilized_are_unknown() {
        // for_loops, method_receivers, enum_payloads, array_repeat,
        // field_init_shorthand, inline_type_ctor_paths, raw_bytes,
        // aggregate_layout, slices, and borrow_accessors were stabilized (no
        // longer gated) — their names must now be rejected.
        for name in [
            "for_loops",
            "method_receivers",
            "enum_payloads",
            "array_repeat",
            "field_init_shorthand",
            "inline_type_ctor_paths",
            "raw_bytes",
            "aggregate_layout",
            "slices",
            "borrow_accessors",
        ] {
            assert!(
                name.parse::<PreviewFeature>().is_err(),
                "{name} should be an unknown preview feature after stabilization"
            );
        }
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
        assert_eq!(
            ErrorKind::LexerDiagnosticsOmitted { limit: 100 }.code(),
            ErrorCode::LEXER_DIAGNOSTICS_OMITTED
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
            ErrorKind::ParseError("custom error".into()).code(),
            ErrorCode::PARSE_ERROR
        );
        assert_eq!(
            ErrorKind::ParserDiagnosticsOmitted { limit: 100 }.code(),
            ErrorCode::PARSER_DIAGNOSTICS_OMITTED
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
        assert_eq!(
            ErrorKind::BreakWithValue.code(),
            ErrorCode::BREAK_WITH_VALUE
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
    fn driver_error_codes_are_stable_and_distinct() {
        assert_eq!(ErrorCode::DRIVER_SOURCE_LOAD, ErrorCode(1500));
        assert_eq!(ErrorCode::DRIVER_TOOLCHAIN_INTEGRITY, ErrorCode(1501));
        assert_eq!(ErrorCode::DRIVER_HERMETIC_DENIAL, ErrorCode(1502));
        assert_ne!(
            ErrorCode::DRIVER_SOURCE_LOAD,
            ErrorCode::DRIVER_TOOLCHAIN_INTEGRITY
        );
        assert_ne!(
            ErrorCode::DRIVER_TOOLCHAIN_INTEGRITY,
            ErrorCode::DRIVER_HERMETIC_DENIAL
        );
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

    #[test]
    fn test_invalid_compiler_input_error_code_and_message() {
        let kind = ErrorKind::InvalidCompilerInput("duplicate file ID 7".into());
        assert_eq!(kind.code(), ErrorCode::INVALID_COMPILER_INPUT);
        assert_eq!(ErrorCode::INVALID_COMPILER_INPUT.0, 1400);
        assert_eq!(
            kind.to_string(),
            "invalid compiler input: duplicate file ID 7"
        );
    }

    #[test]
    fn compiler_resource_failures_have_distinct_non_ice_codes() {
        let limit = ErrorKind::CompilerResourceLimit("AIR words".into());
        let exhaustion = ErrorKind::CompilerResourceExhaustion("AIR reserve".into());
        let invariant = ErrorKind::CompilerProducerInvariant("bad AIR".into());
        assert_eq!(limit.code(), ErrorCode::COMPILER_RESOURCE_LIMIT);
        assert_eq!(exhaustion.code(), ErrorCode::COMPILER_RESOURCE_EXHAUSTION);
        assert_eq!(invariant.code(), ErrorCode::INTERNAL_ERROR);
        assert_ne!(limit.code(), exhaustion.code());
        assert!(!limit.to_string().contains("internal compiler"));
        assert!(!exhaustion.to_string().contains("internal compiler"));
        assert!(invariant.to_string().contains("internal compiler"));
    }

    // ========================================================================
    // ErrorKind size and boxing policy tests
    // ========================================================================

    #[test]
    fn test_error_kind_size() {
        // Measure the size of ErrorKind to understand current memory usage
        let size = std::mem::size_of::<ErrorKind>();

        // Enforce the size limit to prevent regression.
        // Target: ≤ 64 bytes (enum tag + largest inline variant)
        //
        // Current: 56 bytes (as of 2026-01-11)
        // - Enum discriminant: 8 bytes
        // - Largest inline variant: 48 bytes (2 Strings or 2 Cows)
        //
        // If this fails, check which variants are > 48 bytes and box them.
        assert!(
            size <= 64,
            "ErrorKind is {} bytes, exceeds 64-byte limit. \
             Consider boxing large variants (≥ 72 bytes / 3+ Strings). \
             See the boxing policy documentation above ErrorKind.",
            size
        );
    }

    #[test]
    fn test_error_kind_variant_sizes() {
        use std::mem::size_of;

        // Measure individual variant data sizes to identify which ones should be boxed
        println!("String: {} bytes", size_of::<String>());
        println!("Vec<String>: {} bytes", size_of::<Vec<String>>());
        println!(
            "Cow<'static, str>: {} bytes",
            size_of::<Cow<'static, str>>()
        );

        // Inline variants (currently unboxed)
        println!("TypeMismatch data: {} bytes", size_of::<(String, String)>());
        println!("UnknownField data: {} bytes", size_of::<(String, String)>());
        println!(
            "DuplicateField data: {} bytes",
            size_of::<(String, String)>()
        );
        println!(
            "ModuleNotFound data: {} bytes",
            size_of::<(String, Vec<String>)>()
        );

        // Boxed variants (currently boxed)
        println!(
            "MissingFieldsError: {} bytes",
            size_of::<MissingFieldsError>()
        );
        println!(
            "CopyStructNonCopyFieldError: {} bytes",
            size_of::<CopyStructNonCopyFieldError>()
        );
        println!(
            "IntrinsicTypeMismatchError: {} bytes",
            size_of::<IntrinsicTypeMismatchError>()
        );
    }
}
