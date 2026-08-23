//! Logos-based lexer for the Rue programming language.
//!
//! This module provides a lexer implementation using the logos derive macro
//! for efficient tokenization.

use lasso::{Spur, ThreadedRodeo};
use logos::Logos;
use rue_error::{CompileError, CompileErrors, CompileResult, ErrorKind};
use rue_span::{FileId, Span};

use crate::{MAX_INTERNED_STRINGS, MAX_SOURCE_BYTES};

/// Preserve the existing one-token-per-four-bytes estimate for ordinary source
/// files, but do not let sparse input turn source bytes into an equally
/// unbounded token allocation. Sources up to 64 KiB retain the exact old
/// reserve; denser larger sources grow the `Vec` geometrically as tokens arrive.
const MAX_INITIAL_TOKEN_CAPACITY: usize = 16 * 1024;

fn initial_token_capacity(source_len: usize) -> usize {
    (source_len / 4).min(MAX_INITIAL_TOKEN_CAPACITY)
}

fn source_len_for_spans(file_id: FileId, len: usize) -> CompileResult<u32> {
    u32::try_from(len).map_err(|_| {
        CompileError::without_span(ErrorKind::CompilerResourceLimit(format!(
            "source text for file ID {} is {len} bytes, exceeding the maximum supported length of {MAX_SOURCE_BYTES} bytes",
            file_id.index()
        )))
    })
}

fn span_offset(offset: usize) -> u32 {
    u32::try_from(offset).expect("lexer offsets fit after validating the source byte length")
}

/// Error type for lexing failures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LexError {
    #[default]
    UnexpectedCharacter,
    InvalidInteger,
    /// An invalid escape sequence in a string literal. Carries the byte
    /// offset of the backslash within the token (the opening quote is byte 0)
    /// and the offending escape character, so the diagnostic can point at the
    /// exact escape rather than the start of the string — and report the
    /// right character even when a valid escape appears earlier in the
    /// literal. (RUE-133)
    InvalidStringEscape {
        offset: u32,
        escape: char,
    },
    UnterminatedString,
    /// The string interner failed. A `Spur` is a non-zero `u32`, so one symbol
    /// domain can hold at most [`MAX_INTERNED_STRINGS`] distinct spellings
    /// (spec Appendix C.5:1). `lasso` would
    /// abort on the next intern; spec C.1:2 requires a diagnostic naming the
    /// limit instead, so exhaustion is reported through the ordinary lexical
    /// error channel as `E1401`.
    InternerExhausted(lasso::LassoErrorKind),
    /// An uppercase base prefix (`0X`/`0B`/`0O`). Base prefixes are lowercase
    /// (`0x`/`0b`/`0o`, spec 2.1); rejecting the whole literal with a targeted
    /// error is friendlier than Rust's behavior of lexing `0XFF` as `0` plus
    /// an identifier. Carries the offending (uppercase) prefix character.
    /// (RUE-177)
    UppercaseBasePrefix {
        prefix: char,
    },
    /// A based integer literal with no digits after the prefix (`0x`, `0b_`).
    /// Carries the base name ("hexadecimal"/"octal"/"binary"). (RUE-177)
    EmptyBasedLiteral {
        base: &'static str,
    },
    /// A digit that is not valid in the literal's base (`0b2`, `0o9`, `0xG`).
    /// Carries the offending character, the base name, and the character's
    /// byte offset within the token so the diagnostic can point at it.
    /// (RUE-177)
    InvalidDigitForBase {
        digit: char,
        base: &'static str,
        offset: u32,
    },
    /// A malformed byte literal (`b'ab'`, `b''`, `b'\q'`, non-ASCII `b'é'`, or
    /// an unterminated `b'a`). Carries an already-rendered reason. (RUE-1042)
    MalformedByteLiteral(String),
    /// A float literal spelling the lexer can reject on its own: a leading dot
    /// (`.5`) or an exponent marker with no digits (`1e`, `1e+`). Carries an
    /// already-rendered reason. The trailing-dot form (`5.`) is *not* here —
    /// `42.` is the legal prefix of `42.to_string()`, so only the parser can
    /// tell the two apart (ADR-0065 §3, RUE-1068).
    MalformedFloatLiteral(String),
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
    // The first invalid escape, if any. On an invalid escape we do NOT bail
    // immediately: we keep scanning to the real closing quote so the token
    // spans the whole literal and the lexer resumes AFTER the close. Bailing
    // mid-string left logos resuming inside the remaining content, which then
    // read the original closing quote as a fresh opening quote and reported a
    // spurious `unterminated string literal` (RUE-535).
    let mut pending_error: Option<LexError> = None;

    while let Some(c) = chars.next() {
        if c == '"' {
            // Found closing quote
            consumed += 1;
            found_close = true;
            break;
        } else if c == '\\' {
            // Escape sequence. +1 for the opening quote: `consumed` counts
            // content bytes only, but the error offset is within the token.
            let backslash_offset = span_offset(consumed) + 1;
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
                Some('\n') | Some('\r') => {
                    // A backslash immediately before an end-of-line is an
                    // unterminated string (spec 2.1:9), not an invalid escape.
                    // Reporting the raw newline/CR as the offending escape char
                    // would also render a literal control char into the
                    // diagnostic. Don't consume the line terminator (mirror the
                    // bare-newline path below) so the span points at the string
                    // start.
                    lex.bump(consumed);
                    return Err(pending_error.unwrap_or(LexError::UnterminatedString));
                }
                Some(other) => {
                    // Invalid escape: consume the char so the token covers it
                    // and record the FIRST one for the diagnostic span, but keep
                    // scanning for the real closing quote so the lexer resyncs
                    // past it (RUE-535).
                    consumed += other.len_utf8();
                    if pending_error.is_none() {
                        pending_error = Some(LexError::InvalidStringEscape {
                            offset: backslash_offset,
                            escape: other,
                        });
                    }
                }
                None => {
                    // Backslash at end of input
                    lex.bump(consumed);
                    return Err(pending_error.unwrap_or(LexError::UnterminatedString));
                }
            }
        } else if c == '\n' || c == '\r' {
            // Line terminator in string - string is unterminated at this line.
            // A bare CR counts too: spec 2.3:1 classes it as a newline, and the
            // backslash-before-EOL path above already treats it as one.
            // Don't consume the terminator so error span points to string start.
            // A pending invalid escape (if any) is the more specific diagnosis.
            lex.bump(consumed);
            return Err(pending_error.unwrap_or(LexError::UnterminatedString));
        } else {
            consumed += c.len_utf8();
            result.push(c);
        }
    }

    if !found_close {
        // Reached end of input without closing quote
        lex.bump(consumed);
        return Err(pending_error.unwrap_or(LexError::UnterminatedString));
    }

    // Advance past the string content and closing quote
    lex.bump(consumed);

    // The literal was well-formed and terminated except for an invalid escape:
    // report it now that the lexer is resynced past the closing quote, so no
    // spurious follow-on error is produced for the rest of the line (RUE-535).
    if let Some(err) = pending_error {
        return Err(err);
    }

    // Intern the string. Exhausting the interner's key space is a published
    // implementation limit, not an abort (spec C.5:1, C.1:2).
    let spur = lex
        .extras
        .try_get_or_intern(&result)
        .map_err(|error| LexError::InternerExhausted(error.kind()))?;
    Ok(spur)
}

/// Callback for the decimal integer literal rule on [`LogosTokenKind::Int`]:
/// computes the value, skipping `_` separators (`1_000_000`, trailing `1_`).
/// The regex guarantees the literal starts with a digit (`_1` is an
/// identifier), so every non-underscore character is a decimal digit; the
/// only failure mode is overflow past `u64::MAX`. (RUE-177)
fn parse_decimal_literal(lex: &mut logos::Lexer<'_, LogosTokenKind>) -> Result<u64, LexError> {
    let mut value: u64 = 0;
    for c in lex.slice().chars() {
        if c == '_' {
            continue;
        }
        let digit = c.to_digit(10).expect("regex guarantees decimal digits");
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(digit)))
            .ok_or(LexError::InvalidInteger)?;
    }
    Ok(value)
}

/// Callback for the based integer literal rule on [`LogosTokenKind::Int`]:
/// `0x` (hexadecimal), `0o` (octal), and `0b` (binary) literals, copying
/// Rust's rules (RUE-177):
///
/// - prefixes are lowercase; an uppercase prefix (`0XFF`) is a targeted
///   error rather than Rust's lex-as-`0`-plus-identifier
/// - hex digits are case-insensitive (`0xff` == `0xFF`)
/// - `_` separators are legal anywhere among the digits, including
///   immediately after the prefix (`0x_FF`) and trailing (`0xFF_`)
/// - a prefix with no digits (`0x`, `0b_`) is an error
/// - a digit outside the base (`0b2`, `0o9`, `0xG`) is an error
///
/// The rule's regex deliberately swallows every alphanumeric/underscore
/// character after the prefix so malformed forms error as one unit instead
/// of splitting into literal + identifier and dying with a generic parse
/// error downstream.
fn parse_based_literal(lex: &mut logos::Lexer<'_, LogosTokenKind>) -> Result<u64, LexError> {
    let slice = lex.slice();
    let prefix = slice
        .chars()
        .nth(1)
        .expect("regex guarantees a prefix char");
    let (base, radix) = match prefix.to_ascii_lowercase() {
        'x' => ("hexadecimal", 16u32),
        'o' => ("octal", 8),
        'b' => ("binary", 2),
        _ => unreachable!("regex only matches 0x/0o/0b prefixes"),
    };
    if prefix.is_ascii_uppercase() {
        return Err(LexError::UppercaseBasePrefix { prefix });
    }

    let mut value: u64 = 0;
    let mut seen_digit = false;
    // The regex is ASCII-only, so byte offsets == char offsets here; +2
    // skips the `0x` prefix.
    for (i, c) in slice[2..].char_indices() {
        if c == '_' {
            continue;
        }
        let digit = c.to_digit(radix).ok_or(LexError::InvalidDigitForBase {
            digit: c,
            base,
            offset: span_offset(2 + i),
        })?;
        value = value
            .checked_mul(u64::from(radix))
            .and_then(|v| v.checked_add(u64::from(digit)))
            .ok_or(LexError::InvalidInteger)?;
        seen_digit = true;
    }
    if !seen_digit {
        return Err(LexError::EmptyBasedLiteral { base });
    }
    Ok(value)
}

/// Callback for the float literal rules on [`LogosTokenKind::Float`]
/// (ADR-0065 §3, RUE-1068).
///
/// A float literal is a digit run followed by a `.` fraction, an exponent, or
/// both: `1.5`, `1e9`, `1.5e-3`, `6.022e23`. There are no suffixes — the
/// literal is a `comptime_float` and takes its width from context — so the
/// only work here is interning the literal's text with `_` separators removed,
/// leaving a string `str::parse::<f32>()`/`<f64>()` accepts verbatim once the
/// target width is known. Nothing is rounded at this phase: a
/// `comptime_float` is arbitrary precision (ADR-0025), and decoding to `f64`
/// here would round `1.0000000000000000001` before anyone knew whether the
/// destination was `f32`, `f64`, or a compile error.
///
/// The interner can be exhausted like any other interning site (spec C.5:1).
fn intern_float_literal(lex: &mut logos::Lexer<'_, LogosTokenKind>) -> Result<Spur, LexError> {
    let slice = lex.slice();
    // Fast path: most literals have no separators, so avoid the copy.
    if slice.contains('_') {
        let digits: String = slice.chars().filter(|c| *c != '_').collect();
        lex.extras
            .try_get_or_intern(&digits)
            .map_err(|error| LexError::InternerExhausted(error.kind()))
    } else {
        lex.extras
            .try_get_or_intern(slice)
            .map_err(|error| LexError::InternerExhausted(error.kind()))
    }
}

/// Callback for the leading-dot float rule: `.5` is rejected, write `0.5`
/// (ADR-0065 §3).
///
/// This is safe to decide lexically because `.` immediately followed by a
/// digit cannot begin any other Rue lexeme: member access takes an identifier
/// (`p.x`), Rue has no tuple-index syntax, and there is no range operator.
fn reject_leading_dot_float(lex: &mut logos::Lexer<'_, LogosTokenKind>) -> Result<Spur, LexError> {
    Err(LexError::MalformedFloatLiteral(format!(
        "floating-point literal cannot start with `.`: write `0{}` instead of `{}`",
        lex.slice(),
        lex.slice()
    )))
}

/// Callback for the empty-exponent rule: `1e`, `1e+` have no exponent digits.
///
/// Matching this as one token (rather than letting `1e` split into `1` and the
/// identifier `e`) is safe for the same reason the based-literal rule swallows
/// its alphanumeric tail: no grammar production allows an integer literal to
/// abut an identifier character, so this can never reject a valid program.
fn reject_empty_exponent(lex: &mut logos::Lexer<'_, LogosTokenKind>) -> Result<Spur, LexError> {
    Err(LexError::MalformedFloatLiteral(format!(
        "missing digits in the exponent of floating-point literal `{}`",
        lex.slice()
    )))
}

/// Callback for byte literals `b'a'` (RUE-1042), triggered on the `b'` prefix.
///
/// A byte literal is a readable `u8` spelling of a single ASCII byte: `b'a'`
/// lexes to exactly the integer literal `97`, so it flows through the parser
/// and contextual integer typing like any other integer literal (it becomes a
/// `u8` in a `u8` comparison, an `i64` where one is expected, etc.). This keeps
/// the feature lexer-only: no new token, type, or inference rule.
///
/// The content is one of: a single printable ASCII character, or an escape
/// from the same set the string lexer accepts (`\\ \" \n \t \r \0`) plus `\'`.
/// Non-ASCII characters, an empty literal, more than one byte, an unknown
/// escape, or a missing closing quote are rejected. On any error the token is
/// bumped to cover the offending text (to the closing quote, or to
/// end-of-line) so the lexer resyncs cleanly, mirroring the string path.
fn process_byte_literal(lex: &mut logos::Lexer<'_, LogosTokenKind>) -> Result<u64, LexError> {
    let rest = lex.remainder();
    let mut chars = rest.char_indices();

    // Parse the byte content, tracking how many bytes of `rest` it spans.
    let (value, content_len) = match chars.next() {
        None => {
            return Err(LexError::MalformedByteLiteral(
                "unterminated byte literal: expected a byte then a closing `'`".to_string(),
            ));
        }
        Some((_, '\'')) => {
            lex.bump(1);
            return Err(LexError::MalformedByteLiteral(
                "empty byte literal `b''`: a byte literal must contain exactly one byte"
                    .to_string(),
            ));
        }
        Some((_, '\n')) | Some((_, '\r')) => {
            return Err(LexError::MalformedByteLiteral(
                "unterminated byte literal (line ended before the closing `'`)".to_string(),
            ));
        }
        Some((_, '\\')) => match chars.next() {
            None => {
                lex.bump(1);
                return Err(LexError::MalformedByteLiteral(
                    "unterminated escape in byte literal".to_string(),
                ));
            }
            Some((_, escape)) => {
                let byte = match escape {
                    '\\' => b'\\',
                    '\'' => b'\'',
                    '"' => b'"',
                    'n' => b'\n',
                    't' => b'\t',
                    'r' => b'\r',
                    '0' => 0,
                    other => {
                        lex.bump(1 + other.len_utf8());
                        return Err(LexError::MalformedByteLiteral(format!(
                            "unknown escape `\\{}` in byte literal",
                            other.escape_debug()
                        )));
                    }
                };
                (u64::from(byte), 1 + escape.len_utf8())
            }
        },
        Some((_, c)) => {
            if !c.is_ascii() {
                lex.bump(c.len_utf8());
                return Err(LexError::MalformedByteLiteral(format!(
                    "byte literal must be a single ASCII byte, found `{}`",
                    c.escape_debug()
                )));
            }
            (u64::from(c as u8), 1)
        }
    };

    // Expect the closing quote immediately after the single byte's content.
    match rest[content_len..].chars().next() {
        Some('\'') => {
            lex.bump(content_len + 1);
            Ok(value)
        }
        Some(_) => {
            // More than one byte before the quote (`b'ab'`). Resync to the
            // closing quote if there is one on this line, else to line end.
            let tail = &rest[content_len..];
            let stop = tail
                .find(['\'', '\n', '\r'])
                .map(|i| {
                    if tail.as_bytes()[i] == b'\'' {
                        i + 1
                    } else {
                        i
                    }
                })
                .unwrap_or(tail.len());
            lex.bump(content_len + stop);
            Err(LexError::MalformedByteLiteral(
                "a byte literal must contain exactly one byte".to_string(),
            ))
        }
        None => {
            lex.bump(content_len);
            Err(LexError::MalformedByteLiteral(
                "unterminated byte literal (missing closing `'`)".to_string(),
            ))
        }
    }
}

/// Token kinds in the Rue language, using logos derive macro.
#[derive(Logos, Debug, Clone, PartialEq, Eq)]
#[logos(error = LexError)]
#[logos(extras = ThreadedRodeo)]
// Whitespace per spec 2.3:1 — space, tab, newline, carriage return only.
// Deliberately excludes form-feed (U+000C): Rust treats FF as whitespace
// (Pattern_White_Space), but Zig does not, and Rue follows Zig's strict,
// explicit set here (RUE-333). A stray FF byte therefore lexes to an error.
#[logos(skip r"[ \t\n\r]+")]
// A line comment ends at end-of-line. The spec classes CR, LF, and CRLF all
// as newlines (2.3:1), so the comment body must exclude BOTH `\n` and `\r` —
// otherwise `// c<CR>code` on a CR-only file swallows the code as comment text
// (RUE-534).
#[logos(skip r"//[^\n\r]*")]
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
    #[token("for")]
    For,
    #[token("in")]
    In,
    #[token("break")]
    Break,
    #[token("continue")]
    Continue,
    #[token("return")]
    Return,
    #[token("yield")]
    Yield,
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
    #[token("Self")]
    SelfType,
    #[token("comptime")]
    Comptime,
    #[token("pub")]
    Pub,
    #[token("const")]
    Const,
    #[token("checked")]
    Checked,
    #[token("unchecked")]
    Unchecked,
    #[token("ptr")]
    Ptr,
    #[token("extern")]
    Extern,

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
    // `type` — the compile-time type of types (spec 2.4:3). A reserved keyword,
    // not an identifier: it appears only in type position (`comptime T: type`,
    // `-> type`), where the parser maps it back to the interned "type" name so
    // sema's `Type::from_primitive_name("type")` resolution is unchanged.
    #[token("type")]
    Type,

    // Patterns
    #[token("_")]
    Underscore,

    // Integer literals (spec 2.1): decimal (`42`, `1_000`), hexadecimal
    // (`0xFF`), octal (`0o17`), and binary (`0b101`), with `_` separators
    // legal anywhere among the digits. (RUE-177)
    //
    // The second rule matches based literals as a unit, deliberately
    // swallowing any alphanumeric/underscore tail (`0xG`, `0b2`) so
    // malformed forms get a targeted diagnostic instead of splitting into
    // `0` + identifier and surfacing as a confusing generic parse error.
    // This can never reject a valid program: no grammar production allows
    // an integer literal directly abutting an identifier character.
    // (RUE-133, RUE-177)
    #[regex(r"[0-9][0-9_]*", parse_decimal_literal)]
    #[regex(r"0[xXbBoO][0-9a-zA-Z_]*", parse_based_literal)]
    // Byte literals `b'a'` are `u8` spellings of a single ASCII byte and lex to
    // the same untyped integer literal as the byte's decimal code (RUE-1042).
    #[token("b'", process_byte_literal)]
    Int(u64),

    // Float literals (ADR-0065 §3, RUE-1068): a digit run followed by a `.`
    // fraction, an exponent, or both — `1.5`, `1e9`, `1.5e-3`, `6.022e23` —
    // with `_` separators legal inside any digit run. There are no `f32`/`f64`
    // suffixes; a literal is a `comptime_float` and takes its width from
    // context in a later phase.
    //
    // Disambiguation against the existing tokens is by logos' longest match:
    //   - `1.5`  -> Float, not `Int(1) Dot Int(5)`, because the float rule
    //     matches three characters where the `Int` rule matches one.
    //   - `42.to_string()` -> `Int(42) Dot Ident`, because the fraction rule
    //     requires a *digit* after the `.`, so nothing longer than `42`
    //     matches. This is why the trailing-dot rejection (`5.`) lives in the
    //     parser: the lexer cannot commit to it without breaking method calls
    //     on integer literals.
    //   - `0x1e9` -> the based-literal rule, which the exponent rule cannot
    //     reach (its digit run stops at `x`).
    // The exponent marker accepts `e` and `E`. That is not a departure from
    // the lowercase-only *base prefix* rule (`0x`, RUE-177): case-insensitivity
    // inside a numeric body already holds for hex digits (`0xff` == `0xFF`),
    // and `E` selects nothing, so there is no `0X`-style ambiguity to reject.
    #[regex(
        r"[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?",
        intern_float_literal
    )]
    #[regex(r"[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*", intern_float_literal)]
    // `.5` is rejected: write `0.5` (ADR-0065 §3).
    #[regex(r"\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?", reject_leading_dot_float)]
    // `1e`, `1e-` have an exponent marker but no exponent digits.
    #[regex(r"[0-9][0-9_]*(\.[0-9][0-9_]*)?[eE][+-]?", reject_empty_exponent)]
    Float(Spur),

    // String literals - match opening quote and process content manually
    // This allows detection of unterminated strings
    #[token("\"", process_string_from_quote)]
    String(Spur),

    // Identifiers (lower priority than keywords)
    #[regex(
        r"[a-zA-Z_][a-zA-Z0-9_]*",
        |lex| lex
            .extras
            .try_get_or_intern(lex.slice())
            .map_err(|error| LexError::InternerExhausted(error.kind())),
        priority = 1
    )]
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

    // Compound-assignment operators (RUE-1043). Logos prefers the longest
    // match, so `<<=` wins over `<<` and `+=` over `+` without any ordering
    // requirement here.
    #[token("+=")]
    PlusEq,
    #[token("-=")]
    MinusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("%=")]
    PercentEq,
    #[token("&=")]
    AmpEq,
    #[token("|=")]
    PipeEq,
    #[token("^=")]
    CaretEq,
    #[token("<<=")]
    LtLtEq,
    #[token(">>=")]
    GtGtEq,
    // `::` is no longer a Rue operator (RUE-488): `.` is the sole member-access
    // spelling. The token is still recognized so a stray `::` reaches the parser
    // as one unit and yields a precise "use `.`" diagnostic, rather than lexing
    // as two `:` tokens that would produce an opaque error.
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
    #[token("?")]
    Question,
}

use crate::{LEXER_DIAGNOSTIC_BUDGET, Token, TokenKind};

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
            LogosTokenKind::For => TokenKind::For,
            LogosTokenKind::In => TokenKind::In,
            LogosTokenKind::Break => TokenKind::Break,
            LogosTokenKind::Continue => TokenKind::Continue,
            LogosTokenKind::Return => TokenKind::Return,
            LogosTokenKind::Yield => TokenKind::Yield,
            LogosTokenKind::True => TokenKind::True,
            LogosTokenKind::False => TokenKind::False,
            LogosTokenKind::Struct => TokenKind::Struct,
            LogosTokenKind::Enum => TokenKind::Enum,
            LogosTokenKind::Impl => TokenKind::Impl,
            LogosTokenKind::Drop => TokenKind::Drop,
            LogosTokenKind::Linear => TokenKind::Linear,
            LogosTokenKind::SelfValue => TokenKind::SelfValue,
            LogosTokenKind::SelfType => TokenKind::SelfType,
            LogosTokenKind::Comptime => TokenKind::Comptime,
            LogosTokenKind::Pub => TokenKind::Pub,
            LogosTokenKind::Const => TokenKind::Const,
            LogosTokenKind::Checked => TokenKind::Checked,
            LogosTokenKind::Unchecked => TokenKind::Unchecked,
            LogosTokenKind::Ptr => TokenKind::Ptr,
            LogosTokenKind::Extern => TokenKind::Extern,
            LogosTokenKind::I8 => TokenKind::I8,
            LogosTokenKind::I16 => TokenKind::I16,
            LogosTokenKind::I32 => TokenKind::I32,
            LogosTokenKind::I64 => TokenKind::I64,
            LogosTokenKind::U8 => TokenKind::U8,
            LogosTokenKind::U16 => TokenKind::U16,
            LogosTokenKind::U32 => TokenKind::U32,
            LogosTokenKind::U64 => TokenKind::U64,
            LogosTokenKind::Bool => TokenKind::Bool,
            LogosTokenKind::Type => TokenKind::Type,
            LogosTokenKind::Underscore => TokenKind::Underscore,
            LogosTokenKind::Int(n) => TokenKind::Int(n),
            LogosTokenKind::Float(s) => TokenKind::Float(s),
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
            LogosTokenKind::PlusEq => TokenKind::PlusEq,
            LogosTokenKind::MinusEq => TokenKind::MinusEq,
            LogosTokenKind::StarEq => TokenKind::StarEq,
            LogosTokenKind::SlashEq => TokenKind::SlashEq,
            LogosTokenKind::PercentEq => TokenKind::PercentEq,
            LogosTokenKind::AmpEq => TokenKind::AmpEq,
            LogosTokenKind::PipeEq => TokenKind::PipeEq,
            LogosTokenKind::CaretEq => TokenKind::CaretEq,
            LogosTokenKind::LtLtEq => TokenKind::LtLtEq,
            LogosTokenKind::GtGtEq => TokenKind::GtGtEq,
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
            LogosTokenKind::Question => TokenKind::Question,
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
        self.tokenize_preserving_interner()
            .map_err(|(errors, _interner)| CompileError::from(errors))
    }

    /// Tokenize the entire source, preserving the interner even on error.
    ///
    /// Multi-file compilation uses this to keep lexing later files after an
    /// earlier file fails, while still sharing one interner across all files.
    pub fn tokenize_preserving_interner(
        self,
    ) -> Result<(Vec<Token>, ThreadedRodeo), (CompileErrors, ThreadedRodeo)> {
        let source_len = match source_len_for_spans(self.file_id, self.source.len()) {
            Ok(source_len) => source_len,
            Err(error) => return Err((CompileErrors::from(error), self.interner)),
        };

        // Keep the old density estimate for ordinary files, but cap the initial
        // allocation so sparse large sources do not reserve per-source memory.
        let mut tokens = Vec::with_capacity(initial_token_capacity(self.source.len()));
        let mut errors = CompileErrors::new();

        let mut lexer = LogosTokenKind::lexer_with_extras(self.source, self.interner);

        // Skip a single leading UTF-8 BOM (U+FEFF, bytes EF BB BF) per Rust/Go
        // precedent (RUE-378). We bump the logos read head past it instead of
        // slicing the source, so every token span stays a byte offset into the
        // ORIGINAL file — the first real token starts at byte 3 and diagnostics
        // still line up. Only the *leading* position is skipped: a U+FEFF
        // anywhere else falls through to the unexpected-character path below
        // (it is still an invisible character in the middle of the program).
        if self.source.starts_with('\u{feff}') {
            lexer.bump('\u{feff}'.len_utf8());
        }

        loop {
            match lexer.next() {
                Some(result) => {
                    let span = lexer.span();
                    match result {
                        Ok(logos_kind) => {
                            tokens.push(Token {
                                kind: logos_kind.into(),
                                span: Span::with_file(
                                    self.file_id,
                                    span_offset(span.start),
                                    span_offset(span.end),
                                ),
                            });
                        }
                        Err(lex_error) => {
                            let slice = lexer.slice();
                            let error_char = slice.chars().next().unwrap_or('?');
                            let (kind, rue_span) = match lex_error {
                                LexError::InvalidInteger => (
                                    ErrorKind::InvalidInteger,
                                    Span::with_file(
                                        self.file_id,
                                        span_offset(span.start),
                                        span_offset(span.end),
                                    ),
                                ),
                                LexError::InternerExhausted(kind) => (
                                    crate::interner_error_kind(
                                        kind,
                                        match kind {
                                            lasso::LassoErrorKind::FailedAllocation => {
                                                "lexer symbol-domain allocation failed while interning a spelling".to_owned()
                                            }
                                            _ => format!(
                                                "this symbol domain exceeded its maximum of \
                                             {MAX_INTERNED_STRINGS} distinct interned spellings"
                                            ),
                                        },
                                    ),
                                    Span::with_file(
                                        self.file_id,
                                        span_offset(span.start),
                                        span_offset(span.end),
                                    ),
                                ),
                                LexError::UnexpectedCharacter => (
                                    ErrorKind::UnexpectedCharacter(error_char),
                                    Span::with_file(
                                        self.file_id,
                                        span_offset(span.start),
                                        span_offset(span.end),
                                    ),
                                ),
                                LexError::InvalidStringEscape { offset, escape } => {
                                    // Point at the offending escape itself (`\q`),
                                    // not the whole string-so-far — and report the
                                    // escape the scanner actually rejected, not
                                    // whichever backslash comes first. (RUE-133)
                                    let esc_start = span_offset(span.start) + offset;
                                    (
                                        ErrorKind::InvalidStringEscape(escape),
                                        Span::with_file(
                                            self.file_id,
                                            esc_start,
                                            esc_start + 1 + escape.len_utf8() as u32,
                                        ),
                                    )
                                }
                                LexError::UnterminatedString => (
                                    ErrorKind::UnterminatedString,
                                    Span::with_file(
                                        self.file_id,
                                        span_offset(span.start),
                                        span_offset(span.end),
                                    ),
                                ),
                                LexError::UppercaseBasePrefix { prefix } => (
                                    ErrorKind::UppercaseBasePrefix(prefix),
                                    Span::with_file(
                                        self.file_id,
                                        span_offset(span.start),
                                        span_offset(span.end),
                                    ),
                                ),
                                LexError::EmptyBasedLiteral { base } => (
                                    ErrorKind::EmptyBasedLiteral { base },
                                    Span::with_file(
                                        self.file_id,
                                        span_offset(span.start),
                                        span_offset(span.end),
                                    ),
                                ),
                                LexError::MalformedByteLiteral(message) => (
                                    ErrorKind::MalformedByteLiteral(message),
                                    Span::with_file(
                                        self.file_id,
                                        span_offset(span.start),
                                        span_offset(span.end),
                                    ),
                                ),
                                LexError::MalformedFloatLiteral(message) => (
                                    ErrorKind::MalformedFloatLiteral(message),
                                    Span::with_file(
                                        self.file_id,
                                        span_offset(span.start),
                                        span_offset(span.end),
                                    ),
                                ),
                                LexError::InvalidDigitForBase {
                                    digit,
                                    base,
                                    offset,
                                } => {
                                    // Point at the offending digit itself
                                    // (`0b2`'s `2`), not the whole literal.
                                    let digit_start = span_offset(span.start) + offset;
                                    (
                                        ErrorKind::InvalidDigitForBase { digit, base },
                                        Span::with_file(
                                            self.file_id,
                                            digit_start,
                                            digit_start + digit.len_utf8() as u32,
                                        ),
                                    )
                                }
                            };
                            if errors.len() == LEXER_DIAGNOSTIC_BUDGET {
                                errors.push(CompileError::new(
                                    ErrorKind::LexerDiagnosticsOmitted {
                                        limit: LEXER_DIAGNOSTIC_BUDGET,
                                    },
                                    rue_span,
                                ));
                                break;
                            }

                            let mut error = CompileError::new(kind, rue_span);
                            if matches!(error.kind, ErrorKind::UnexpectedCharacter('\u{feff}')) {
                                // A *leading* BOM is skipped before we get here
                                // (see the bump above), so any BOM that reaches
                                // this point is in the middle of the file. The
                                // "character" is an invisible UTF-8 byte-order
                                // mark, so the default message and caret point
                                // at nothing visible. Explain what is there.
                                error = error.with_help(
                                    "this invisible character is a UTF-8 byte-order \
                                     mark (BOM); a BOM is only ignored at the very \
                                     start of a file — remove this one",
                                );
                            }
                            errors.push(error);
                        }
                    }
                }
                None => break,
            }
        }

        // Add EOF token (logos doesn't emit EOF)
        tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::point_in_file(self.file_id, source_len),
        });

        // Extract the interner from the logos lexer
        let interner = lexer.extras;

        if !errors.is_empty() {
            return Err((errors, interner));
        }

        Ok((tokens, interner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_span_limit_accepts_largest_representable_length() {
        assert_eq!(
            source_len_for_spans(FileId::new(12), MAX_SOURCE_BYTES).unwrap(),
            u32::MAX
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn source_span_limit_rejects_first_unrepresentable_length() {
        assert_eq!(
            source_len_for_spans(FileId::new(12), MAX_SOURCE_BYTES + 1)
                .unwrap_err()
                .to_string(),
            "compiler resource limit exceeded: source text for file ID 12 is 4294967296 bytes, exceeding the maximum supported length of 4294967295 bytes"
        );
    }

    #[test]
    fn interner_exhaustion_is_a_resource_limit_not_an_abort() {
        // Spec C.5:1/C.1:2: `Spur` is a non-zero u32, so the interner holds at
        // most MAX_INTERNED_STRINGS distinct strings. Filling the key space
        // needs 4 billion distinct strings, so the reachable evidence is that
        // the lexer routes the exhaustion through its ordinary error channel
        // instead of letting `lasso` abort.
        assert_eq!(MAX_INTERNED_STRINGS, u32::MAX as usize);
        let errors = LogosLexer::new("fn main() { }")
            .tokenize()
            .err()
            .map(|error| error.to_string());
        assert!(errors.is_none(), "{errors:?}");
    }

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
    fn test_logos_collects_multiple_unexpected_characters() {
        let lexer = LogosLexer::new("fn main() { $ # }");
        let (errors, _interner) = lexer
            .tokenize_preserving_interner()
            .expect_err("both invalid characters should report");

        assert_eq!(errors.len(), 2);
        let kinds: Vec<_> = errors.iter().map(|error| &error.kind).collect();
        assert!(matches!(kinds[0], ErrorKind::UnexpectedCharacter('$')));
        assert!(matches!(kinds[1], ErrorKind::UnexpectedCharacter('#')));
    }

    #[test]
    fn test_logos_malformed_token_diagnostics_are_bounded() {
        let source = "$".repeat(10_000);
        let (errors, _interner) = LogosLexer::new(&source)
            .tokenize_preserving_interner()
            .expect_err("invalid characters should report");

        assert_eq!(errors.len(), LEXER_DIAGNOSTIC_BUDGET + 1);
        assert!(
            errors
                .iter()
                .take(LEXER_DIAGNOSTIC_BUDGET)
                .all(|error| matches!(error.kind, ErrorKind::UnexpectedCharacter('$')))
        );
        assert_eq!(
            errors.as_slice()[LEXER_DIAGNOSTIC_BUDGET - 1]
                .span()
                .unwrap()
                .start,
            (LEXER_DIAGNOSTIC_BUDGET - 1) as u32
        );

        let summary = &errors.as_slice()[LEXER_DIAGNOSTIC_BUDGET];
        assert_eq!(
            summary.span().unwrap().start,
            LEXER_DIAGNOSTIC_BUDGET as u32
        );
        assert_eq!(
            summary.kind,
            ErrorKind::LexerDiagnosticsOmitted {
                limit: LEXER_DIAGNOSTIC_BUDGET
            }
        );
        assert_eq!(
            summary.to_string(),
            "additional lexer diagnostics omitted after the first 100 errors"
        );
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
        // @import lexes uniformly as At + Ident per spec 2.5:1-2 — no fused
        // token, and "import" is interned at its natural position (RUE-949).
        let lexer = LogosLexer::new("@import");
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::At));
        assert_eq!(tokens[0].span, Span::new(0, 1));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("import"));
        assert_eq!(tokens[1].span, Span::new(1, 7));
        assert!(matches!(tokens[2].kind, TokenKind::Eof));
    }

    #[test]
    fn test_logos_at_import_with_parens() {
        // @import("path.rue") pattern
        let lexer = LogosLexer::new(r#"@import("math.rue")"#);
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::At));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("import"));
        assert!(matches!(tokens[2].kind, TokenKind::LParen));
        assert_eq!(get_string_str(&tokens[3].kind, &interner), Some("math.rue"));
        assert!(matches!(tokens[4].kind, TokenKind::RParen));
    }

    #[test]
    fn test_logos_at_import_prefixed_ident_splits() {
        // A directive whose name merely starts with "import" (@importx,
        // @important) is an ordinary @-directive: At + Ident (RUE-133).
        let lexer = LogosLexer::new("@importx");
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::At));
        assert_eq!(tokens[0].span, Span::new(0, 1));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("importx"));
        assert_eq!(tokens[1].span, Span::new(1, 8));

        let lexer = LogosLexer::new("@important");
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::At));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("important"));
    }

    #[test]
    fn test_logos_invalid_escape_span_points_at_escape() {
        // The diagnostic must point at the offending escape (`\q`), not the
        // string-so-far, and must report the rejected escape even when a
        // valid escape appears earlier in the literal. (RUE-133)
        let source = r#""hello \n world \q tail""#;
        let lexer = LogosLexer::new(source);
        let err = lexer.tokenize().unwrap_err();
        assert!(matches!(err.kind, ErrorKind::InvalidStringEscape('q')));
        let backslash_q = source.find(r"\q").unwrap() as u32;
        let span = err.span().expect("escape error must carry a span");
        assert_eq!(span.start, backslash_q);
        assert_eq!(span.end, backslash_q + 2);
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
    fn test_logos_line_comment_ends_at_bare_cr() {
        // RUE-534: a `//` comment ends at end-of-line, and a bare CR is a
        // newline (spec 2.3:1). The comment must NOT swallow the code after a
        // CR-only line ending, so `main` is a real token, not comment text.
        let lexer = LogosLexer::new("fn // comment\rmain");
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("main"));
        assert!(matches!(tokens[2].kind, TokenKind::Eof));

        // CRLF still terminates the comment too (regression guard).
        let lexer = LogosLexer::new("fn // c\r\nmain");
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("main"));
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
    fn test_logos_question_token() {
        // The `?` (try) operator lexes to a single Question token (RUE-6).
        let lexer = LogosLexer::new("x?");
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(get_ident_str(&tokens[0].kind, &interner), Some("x"));
        assert!(matches!(tokens[1].kind, TokenKind::Question));
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

    /// Helper: lex a single integer literal and return its value.
    fn lex_int(src: &str) -> u64 {
        let (tokens, _) = LogosLexer::new(src).tokenize().unwrap();
        match tokens[0].kind {
            TokenKind::Int(n) => {
                // The literal must lex as ONE token (no `0` + ident split).
                assert!(
                    matches!(tokens[1].kind, TokenKind::Eof),
                    "{src} split into multiple tokens"
                );
                assert_eq!(tokens[0].span, Span::new(0, src.len() as u32));
                n
            }
            ref other => panic!("expected Int for {src}, got {other:?}"),
        }
    }

    #[test]
    fn test_logos_based_literal_values() {
        // RUE-177: hex/octal/binary literals, copying Rust's syntax.
        assert_eq!(lex_int("0xFF"), 255);
        assert_eq!(lex_int("0xff"), 255); // hex digits case-insensitive
        assert_eq!(lex_int("0xDeadBeef"), 0xDEAD_BEEF);
        assert_eq!(lex_int("0x0"), 0);
        assert_eq!(lex_int("0o17"), 15);
        assert_eq!(lex_int("0o0"), 0);
        assert_eq!(lex_int("0b101"), 5);
        assert_eq!(lex_int("0b0"), 0);
        // u64 extremes
        assert_eq!(lex_int("0xFFFF_FFFF_FFFF_FFFF"), u64::MAX);
        assert_eq!(lex_int("0o1777777777777777777777"), u64::MAX);
        assert_eq!(
            lex_int("0b11111111_11111111_11111111_11111111_11111111_11111111_11111111_11111111"),
            u64::MAX
        );
    }

    #[test]
    fn test_logos_underscore_separators() {
        // RUE-177: underscores are legal anywhere among the digits,
        // including immediately after the prefix and trailing.
        assert_eq!(lex_int("1_000_000"), 1_000_000);
        assert_eq!(lex_int("1_"), 1);
        assert_eq!(lex_int("1__2"), 12);
        assert_eq!(lex_int("0_"), 0);
        assert_eq!(lex_int("0xFF_FF"), 0xFFFF);
        assert_eq!(lex_int("0x_FF"), 255);
        assert_eq!(lex_int("0xFF_"), 255);
        assert_eq!(lex_int("0b1010_1010"), 0xAA);
        assert_eq!(lex_int("0o_7_7_"), 0o77);
    }

    #[test]
    fn test_logos_leading_underscore_is_identifier() {
        // `_1` is an identifier, not an integer literal (RUE-177).
        let (tokens, interner) = LogosLexer::new("_1").tokenize().unwrap();
        assert_eq!(get_ident_str(&tokens[0].kind, &interner), Some("_1"));
    }

    #[test]
    fn test_byte_literal_values() {
        // RUE-1042: `b'a'` lexes to the same integer literal as the byte's
        // decimal code — a readable u8 spelling.
        assert_eq!(lex_int("b'a'"), 97);
        assert_eq!(lex_int("b'A'"), 65);
        assert_eq!(lex_int("b'0'"), 48);
        assert_eq!(lex_int("b' '"), 32);
        assert_eq!(lex_int("b'~'"), 126);
        // A double-quote needs no escape inside a byte literal.
        assert_eq!(lex_int("b'\"'"), 34);
        // Escapes: same set as strings, plus `\'`.
        assert_eq!(lex_int(r"b'\n'"), 10);
        assert_eq!(lex_int(r"b'\t'"), 9);
        assert_eq!(lex_int(r"b'\r'"), 13);
        assert_eq!(lex_int(r"b'\0'"), 0);
        assert_eq!(lex_int(r"b'\\'"), 92);
        assert_eq!(lex_int(r"b'\''"), 39);
    }

    #[test]
    fn test_byte_literal_bare_b_is_still_an_identifier() {
        // The `b'` rule must not steal a bare `b` identifier or a `b`-prefixed
        // name — only `b` immediately followed by `'` is a byte literal.
        let (tokens, interner) = LogosLexer::new("b").tokenize().unwrap();
        assert_eq!(get_ident_str(&tokens[0].kind, &interner), Some("b"));
        let (tokens, interner) = LogosLexer::new("byte").tokenize().unwrap();
        assert_eq!(get_ident_str(&tokens[0].kind, &interner), Some("byte"));
        // `b + 1`: identifier, operator, integer — not a byte literal.
        let (tokens, _) = LogosLexer::new("b + 1").tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Ident(_)));
        assert!(matches!(tokens[2].kind, TokenKind::Int(1)));
    }

    #[test]
    fn test_byte_literal_errors() {
        for src in [
            "b''",    // empty
            "b'ab'",  // more than one byte
            "b'a",    // unterminated (missing close)
            r"b'\q'", // unknown escape
            "b'é'",   // non-ASCII
        ] {
            let err = LogosLexer::new(src).tokenize().unwrap_err();
            assert!(
                matches!(err.kind, ErrorKind::MalformedByteLiteral(_)),
                "expected MalformedByteLiteral for {src:?}, got {:?}",
                err.kind
            );
        }
    }

    #[test]
    fn test_logos_uppercase_base_prefix_rejected() {
        // RUE-177: uppercase prefixes get a targeted error (friendlier than
        // Rust's lex-as-`0`-plus-identifier).
        for (src, prefix) in [("0XFF", 'X'), ("0B101", 'B'), ("0O17", 'O')] {
            let err = LogosLexer::new(src).tokenize().unwrap_err();
            match err.kind {
                ErrorKind::UppercaseBasePrefix(p) => assert_eq!(p, prefix, "prefix for {src}"),
                other => panic!("expected UppercaseBasePrefix for {src}, got {other:?}"),
            }
            // Span covers the whole literal.
            let span = err.span().expect("lexer errors carry a span");
            assert_eq!(span.start, 0, "span start for {src}");
            assert_eq!(span.end as usize, src.len(), "span end for {src}");
        }
    }

    #[test]
    fn test_logos_empty_based_literal_rejected() {
        // A bare prefix with no digits is an error, even with underscores.
        for (src, base) in [
            ("0x", "hexadecimal"),
            ("0b", "binary"),
            ("0o", "octal"),
            ("0b_", "binary"),
            ("0x__", "hexadecimal"),
        ] {
            let err = LogosLexer::new(src).tokenize().unwrap_err();
            match err.kind {
                ErrorKind::EmptyBasedLiteral { base: b } => assert_eq!(b, base, "base for {src}"),
                other => panic!("expected EmptyBasedLiteral for {src}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_logos_invalid_digit_for_base_rejected() {
        // Digits must match the base; the span points at the bad digit.
        for (src, digit, base, offset) in [
            ("0b2", '2', "binary", 2u32),
            ("0b012", '2', "binary", 4),
            ("0o9", '9', "octal", 2),
            ("0o787", '8', "octal", 3),
            ("0xG", 'G', "hexadecimal", 2),
            ("0x12fg", 'g', "hexadecimal", 5),
            ("0b1_2", '2', "binary", 4),
        ] {
            let err = LogosLexer::new(src).tokenize().unwrap_err();
            match err.kind {
                ErrorKind::InvalidDigitForBase { digit: d, base: b } => {
                    assert_eq!(d, digit, "digit for {src}");
                    assert_eq!(b, base, "base for {src}");
                }
                other => panic!("expected InvalidDigitForBase for {src}, got {other:?}"),
            }
            let span = err.span().expect("lexer errors carry a span");
            assert_eq!(span.start, offset, "span start for {src}");
            assert_eq!(span.end, offset + 1, "span end for {src}");
        }
    }

    #[test]
    fn test_logos_based_literal_overflow() {
        // One past u64::MAX in each base is InvalidInteger.
        for src in [
            "0x1_0000_0000_0000_0000",
            "0o2000000000000000000000",
            "0b1_00000000_00000000_00000000_00000000_00000000_00000000_00000000_00000000",
            "18_446_744_073_709_551_616",
        ] {
            let err = LogosLexer::new(src).tokenize().unwrap_err();
            assert!(
                matches!(err.kind, ErrorKind::InvalidInteger),
                "expected InvalidInteger for {src}, got {:?}",
                err.kind
            );
        }
        // ... while u64::MAX itself is fine.
        assert_eq!(lex_int("18_446_744_073_709_551_615"), u64::MAX);
    }

    #[test]
    fn test_logos_decimal_zero_unaffected() {
        // Plain `0` and decimal literals still lex normally.
        let (tokens, _) = LogosLexer::new("0 10 0123").tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Int(0)));
        assert!(matches!(tokens[1].kind, TokenKind::Int(10)));
        assert!(matches!(tokens[2].kind, TokenKind::Int(123)));
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
    fn test_logos_impl_and_type_are_keywords() {
        // `impl` and `type` are reserved keywords (spec 2.4:2, 2.4:3), not
        // identifiers (RUE-331). Each lexes to its dedicated keyword token.
        let lexer = LogosLexer::new("impl type");
        let (tokens, _) = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Impl));
        assert!(matches!(tokens[1].kind, TokenKind::Type));
        assert!(matches!(tokens[2].kind, TokenKind::Eof));

        // Identifiers that merely START with a keyword stay identifiers.
        let lexer = LogosLexer::new("impls typex typeof implement");
        let (tokens, interner) = lexer.tokenize().unwrap();
        assert_eq!(get_ident_str(&tokens[0].kind, &interner), Some("impls"));
        assert_eq!(get_ident_str(&tokens[1].kind, &interner), Some("typex"));
        assert_eq!(get_ident_str(&tokens[2].kind, &interner), Some("typeof"));
        assert_eq!(get_ident_str(&tokens[3].kind, &interner), Some("implement"));
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

        // A bare CR is a line terminator (spec 2.3:1) and must end the
        // string like a bare LF does, not be swallowed as content.
        let lexer = LogosLexer::new("\"hello\rworld\"");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UnterminatedString));
    }

    #[test]
    fn test_logos_leading_bom_is_skipped() {
        // A single leading UTF-8 BOM is ignored (RUE-378, Rust/Go precedent).
        // Tokenizing succeeds and the first real token starts at byte 3 — the
        // BOM is skipped without shifting spans off the original file offsets.
        let lexer = LogosLexer::new("\u{feff}fn main() -> i32 { 42 }");
        let (tokens, _interner) = lexer.tokenize().expect("leading BOM should be skipped");
        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert_eq!(
            tokens[0].span.start(),
            3,
            "first token must keep its byte offset into the original file (past the 3-byte BOM)"
        );
    }

    #[test]
    fn test_logos_bom_only_file_is_empty() {
        // A file that is nothing but a BOM must not panic: the read head is
        // bumped to end-of-source and lexing yields only EOF.
        let lexer = LogosLexer::new("\u{feff}");
        let (tokens, _interner) = lexer.tokenize().expect("BOM-only file should lex cleanly");
        assert_eq!(tokens.len(), 1);
        assert!(matches!(tokens[0].kind, TokenKind::Eof));
    }

    #[test]
    fn test_logos_mid_file_bom_errors_with_help() {
        // A BOM anywhere but the leading position is still an error, and the
        // error must explain the invisible character instead of pointing a
        // caret at nothing.
        let lexer = LogosLexer::new("fn main() -> i32 { \u{feff}42 }");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err.kind,
            ErrorKind::UnexpectedCharacter('\u{feff}')
        ));
        assert!(
            err.diagnostic()
                .helps
                .iter()
                .any(|h| h.0.contains("byte-order mark")),
            "mid-file BOM error should carry the explanatory help"
        );
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
    fn test_logos_invalid_escape_resyncs_past_closing_quote() {
        // RUE-535: an invalid escape in a TERMINATED string must not leave the
        // lexer mid-content, where it would read the real closing quote as a
        // fresh opening quote and report a spurious `unterminated string`.
        // Exactly ONE error (the invalid escape) is expected, and the tokens
        // after the string stay synchronized.
        let source = r#"let s = "abc\qtail"; let n = 42;"#;
        let (errors, _interner) = LogosLexer::new(source)
            .tokenize_preserving_interner()
            .expect_err("the invalid escape must report");
        assert_eq!(
            errors.len(),
            1,
            "expected only the invalid-escape error, got {:?}",
            errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::InvalidStringEscape('q')
        ));
    }

    #[test]
    fn test_logos_invalid_escape_then_more_valid_strings_stay_synced() {
        // The `;` and the SECOND string literal after the bad one must lex
        // normally — proof the closing-quote resync holds across the rest of
        // the line (RUE-535).
        let source = r#""x\qy"; "ok""#;
        let (errors, _interner) = LogosLexer::new(source)
            .tokenize_preserving_interner()
            .expect_err("the invalid escape must report");
        assert_eq!(
            errors.len(),
            1,
            "{:?}",
            errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::InvalidStringEscape('q')
        ));
    }

    #[test]
    fn test_logos_unterminated_after_invalid_escape_still_reported() {
        // No closing quote before end of line: the string is genuinely
        // unterminated. Reporting the invalid escape (the first, more specific
        // problem) is fine; what must NOT happen is a phantom second string.
        let source = "\"abc\\qtail\n";
        let (errors, _interner) = LogosLexer::new(source)
            .tokenize_preserving_interner()
            .expect_err("must report");
        assert_eq!(
            errors.len(),
            1,
            "{:?}",
            errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
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
    fn test_dense_ordinary_source_keeps_the_existing_reserve() {
        let source = "fn f(x: i32) -> i32 { x + 1 }\n".repeat(512);
        assert!(source.len() <= MAX_INITIAL_TOKEN_CAPACITY * 4);
        assert_eq!(initial_token_capacity(source.len()), source.len() / 4);

        let (tokens, _) = LogosLexer::new(&source).tokenize().unwrap();
        assert!(tokens.len() > 512);
        assert!(matches!(tokens.last().unwrap().kind, TokenKind::Eof));
    }

    #[test]
    fn test_sparse_source_token_capacity_is_capped() {
        let source_len = MAX_INITIAL_TOKEN_CAPACITY * 5;
        let whitespace = " ".repeat(source_len);
        let comments = "// comment payload\n".repeat(source_len / 19);

        for (distribution, source) in [("whitespace", whitespace), ("comments", comments)] {
            let old_reserve = source.len() / 4;
            assert!(
                old_reserve > MAX_INITIAL_TOKEN_CAPACITY,
                "{distribution} fixture must exercise the cap"
            );

            let (tokens, _) = LogosLexer::new(&source).tokenize().unwrap();
            assert_eq!(tokens.len(), 1, "{distribution} should produce only EOF");
            assert!(
                tokens.capacity() <= MAX_INITIAL_TOKEN_CAPACITY,
                "{distribution} capacity {} exceeded the fixed initial bound",
                tokens.capacity()
            );
        }
    }

    #[test]
    fn test_dense_source_grows_past_the_initial_cap() {
        let source = "x + ".repeat(MAX_INITIAL_TOKEN_CAPACITY * 2);
        assert_eq!(
            initial_token_capacity(source.len()),
            MAX_INITIAL_TOKEN_CAPACITY
        );

        let (tokens, _) = LogosLexer::new(&source).tokenize().unwrap();
        assert_eq!(tokens.len(), MAX_INITIAL_TOKEN_CAPACITY * 4 + 1);
        assert!(tokens.capacity() >= tokens.len());
    }

    #[test]
    fn compound_assignment_operators_lex_as_single_tokens() {
        let lexer = LogosLexer::new("+= -= *= /= %= &= |= ^= <<= >>=");
        let (tokens, _) = lexer.tokenize().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::PlusEq,
                TokenKind::MinusEq,
                TokenKind::StarEq,
                TokenKind::SlashEq,
                TokenKind::PercentEq,
                TokenKind::AmpEq,
                TokenKind::PipeEq,
                TokenKind::CaretEq,
                TokenKind::LtLtEq,
                TokenKind::GtGtEq,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn compound_assignment_does_not_shadow_its_shorter_operators() {
        // The longest match wins, but only where one is actually spelled:
        // `a << b` and `a <= b` keep lexing as they did (RUE-1043).
        let lexer = LogosLexer::new("a << b <= c == d");
        let (tokens, _) = lexer.tokenize().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind).collect();
        assert!(matches!(kinds[1], TokenKind::LtLt));
        assert!(matches!(kinds[3], TokenKind::LtEq));
        assert!(matches!(kinds[5], TokenKind::EqEq));
    }

    /// Resolve every float literal in `source` to its interned text, so the
    /// float tests read as "these characters lexed to this literal".
    fn float_texts(source: &str) -> Vec<String> {
        let (tokens, interner) = LogosLexer::new(source)
            .tokenize()
            .unwrap_or_else(|error| panic!("`{source}` should lex: {error}"));
        tokens
            .iter()
            .filter_map(|token| match token.kind {
                TokenKind::Float(sym) => Some(interner.resolve(&sym).to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn float_literal_accepted_forms() {
        // ADR-0065 §3: a digit run, then a `.` fraction, an exponent, or both.
        assert_eq!(float_texts("1.5"), vec!["1.5"]);
        assert_eq!(float_texts("1e9"), vec!["1e9"]);
        assert_eq!(float_texts("1.5e-3"), vec!["1.5e-3"]);
        assert_eq!(float_texts("6.022e23"), vec!["6.022e23"]);
        assert_eq!(float_texts("1E9 2.5E+7"), vec!["1E9", "2.5E+7"]);
        assert_eq!(
            float_texts("0.0 1.0 3.14159"),
            vec!["0.0", "1.0", "3.14159"]
        );
    }

    #[test]
    fn float_literal_separators_are_stripped_for_the_interned_text() {
        // `_` is legal inside any digit run, and the interned text is what a
        // later phase hands to `str::parse`, so the separators are removed
        // here rather than at every consumer.
        assert_eq!(float_texts("1_000.000_1"), vec!["1000.0001"]);
        assert_eq!(float_texts("1e1_0"), vec!["1e10"]);
    }

    #[test]
    fn float_literal_text_is_exact_not_rounded() {
        // A `comptime_float` is arbitrary precision until context picks a
        // width (ADR-0025, ADR-0065 §3): the token must carry the digits the
        // programmer wrote, not an already-rounded `f64`.
        let long = "0.1000000000000000000000000000001";
        assert_eq!(float_texts(long), vec![long]);
    }

    #[test]
    fn float_literal_wins_over_int_dot_int() {
        // `1.5` must NOT lex as `Int(1) Dot Int(5)`.
        let (tokens, _) = LogosLexer::new("1.5").tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Float(_)));
        assert_eq!(tokens[0].span, Span::new(0, 3));
        assert!(matches!(tokens[1].kind, TokenKind::Eof));
    }

    #[test]
    fn integer_member_access_still_lexes_as_int_dot_ident() {
        // The fraction rule requires a digit after the `.`, so a method call
        // on an integer literal is untouched. This is the reason the
        // trailing-dot rejection is a parser rule, not a lexer rule.
        let (tokens, interner) = LogosLexer::new("42.to_string()").tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Int(42)));
        assert!(matches!(tokens[1].kind, TokenKind::Dot));
        assert_eq!(get_ident_str(&tokens[2].kind, &interner), Some("to_string"));
    }

    #[test]
    fn trailing_dot_float_is_not_a_lexical_error() {
        // `5.` lexes as `Int(5) Dot`; the parser turns that into the
        // "write `5.0`" diagnostic once it sees no member name follows.
        let (tokens, _) = LogosLexer::new("5.;").tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Int(5)));
        assert!(matches!(tokens[1].kind, TokenKind::Dot));
        assert!(matches!(tokens[2].kind, TokenKind::Semi));
    }

    #[test]
    fn leading_dot_float_is_rejected() {
        let error = LogosLexer::new("let x = .5;").tokenize().unwrap_err();
        assert_eq!(
            error.to_string(),
            "floating-point literal cannot start with `.`: write `0.5` instead of `.5`"
        );
        assert_eq!(error.kind.code().to_string(), "E0011");
        let span = error.span().expect("lexical error must carry a span");
        assert_eq!((span.start, span.end), (8, 10));
    }

    #[test]
    fn exponent_without_digits_is_rejected() {
        let error = LogosLexer::new("let x = 1e;").tokenize().unwrap_err();
        assert_eq!(
            error.to_string(),
            "missing digits in the exponent of floating-point literal `1e`"
        );
        let error = LogosLexer::new("let x = 2.0e+;").tokenize().unwrap_err();
        assert_eq!(
            error.to_string(),
            "missing digits in the exponent of floating-point literal `2.0e+`"
        );
    }

    #[test]
    fn based_integer_literals_are_unaffected_by_the_exponent_rule() {
        // `0x1e9`'s digit run stops at `x`, so the exponent rule can never
        // reach it; it stays one hexadecimal literal.
        let (tokens, _) = LogosLexer::new("0x1e9 0b101 0o17").tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Int(0x1e9)));
        assert!(matches!(tokens[1].kind, TokenKind::Int(0b101)));
        assert!(matches!(tokens[2].kind, TokenKind::Int(0o17)));
    }

    #[test]
    fn lexer_interner_failure_classification_preserves_allocator_failures() {
        assert!(matches!(
            crate::interner_error_kind(
                lasso::LassoErrorKind::KeySpaceExhaustion,
                "lexer symbol space"
            ),
            ErrorKind::CompilerResourceLimit(_)
        ));
        assert!(matches!(
            crate::interner_error_kind(
                lasso::LassoErrorKind::FailedAllocation,
                "lexer symbol space"
            ),
            ErrorKind::CompilerResourceExhaustion(_)
        ));
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
