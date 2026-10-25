//! Validation of literal and doc-comment contents.
//!
//! The lexer only finds where a literal ends. Whether its contents are well
//! formed (escapes, digits valid for the base, a single character in a
//! character literal) is checked here, with rustc's diagnostics. These checks
//! are lexical: rustc applies them to every literal, including those inside
//! macro invocations. Suffix validity is not checked here, because a suffix
//! is only an error once the literal is used as an expression.
//!
//! Sources are not line-ending normalized, so a carriage return directly
//! followed by a line feed is accepted wherever rustc (which normalizes
//! `\r\n` to `\n` when loading a file) would accept a line feed.

use crate::{Base, LiteralKind};

/// The first problem with the contents of a literal token, if any.
///
/// `text` is the whole token, including any prefix, quotes and suffix.
/// Unterminated literals are not reported here; their token kind already
/// says so.
pub fn literal_error(kind: LiteralKind, text: &str, suffix_start: u32) -> Option<&'static str> {
    let body = &text[..suffix_start as usize];
    match kind {
        LiteralKind::Int { base, empty_int } => {
            if empty_int {
                return None;
            }
            let digits = if base == Base::Decimal {
                body
            } else {
                &body[2..]
            };
            let invalid = digits.chars().any(|c| c != '_' && !c.is_digit(base as u32));
            if invalid {
                return Some(match base {
                    Base::Binary => "invalid digit for a base 2 literal",
                    Base::Octal => "invalid digit for a base 8 literal",
                    _ => return None,
                });
            }
            None
        }
        LiteralKind::Float { base, .. } => match base {
            Base::Binary => Some("binary float literal is not supported"),
            Base::Octal => Some("octal float literal is not supported"),
            Base::Hexadecimal => Some("hexadecimal float literal is not supported"),
            Base::Decimal => None,
        },
        LiteralKind::Char { terminated: true } => quoted_char(&body[1..body.len() - 1], Mode::Char),
        LiteralKind::Byte { terminated: true } => quoted_char(&body[2..body.len() - 1], Mode::Byte),
        LiteralKind::Str { terminated: true } => string(&body[1..body.len() - 1], Mode::Str),
        LiteralKind::ByteStr { terminated: true } => {
            string(&body[2..body.len() - 1], Mode::ByteStr)
        }
        LiteralKind::CStr { terminated: true } => string(&body[2..body.len() - 1], Mode::CStr),
        LiteralKind::RawStr { n_hashes: Some(n) } => raw_string(raw_body(body, 1, n), Mode::Str),
        LiteralKind::RawByteStr { n_hashes: Some(n) } => {
            raw_string(raw_body(body, 2, n), Mode::ByteStr)
        }
        LiteralKind::RawCStr { n_hashes: Some(n) } => raw_string(raw_body(body, 2, n), Mode::CStr),
        _ => None,
    }
}

/// The first problem with a doc comment's text, if any.
pub fn doc_comment_error(text: &str) -> Option<&'static str> {
    has_bare_cr(text).then_some("bare CR not allowed in doc-comment")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Char,
    Byte,
    Str,
    ByteStr,
    CStr,
}

impl Mode {
    fn is_byte(self) -> bool {
        matches!(self, Mode::Byte | Mode::ByteStr)
    }
}

fn raw_body(body: &str, prefix_len: usize, n_hashes: u8) -> &str {
    let n = n_hashes as usize;
    &body[prefix_len + n + 1..body.len() - 1 - n]
}

/// A carriage return not followed by a line feed. A carriage return at the
/// very end may be the first half of a `\r\n` the token stops before.
fn has_bare_cr(text: &str) -> bool {
    let bytes = text.as_bytes();
    (0..bytes.len()).any(|i| bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] != b'\n')
}

fn quoted_char(content: &str, mode: Mode) -> Option<&'static str> {
    let mut chars = content.chars();
    let Some(first) = chars.next() else {
        return Some(if mode == Mode::Byte {
            "empty byte literal"
        } else {
            "empty character literal"
        });
    };
    match first {
        '\\' => {
            if let Some(error) = escape(&mut chars, mode) {
                return Some(error);
            }
        }
        '\n' | '\r' | '\t' | '\'' => return Some("character constant must be escaped"),
        c if mode == Mode::Byte && !c.is_ascii() => {
            return Some("non-ASCII character in byte literal");
        }
        _ => {}
    }
    if chars.next().is_some() {
        return Some(if mode == Mode::Byte {
            "byte literal may only contain one byte"
        } else {
            "character literal may only contain one codepoint"
        });
    }
    None
}

fn string(content: &str, mode: Mode) -> Option<&'static str> {
    let mut chars = content.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if chars.as_str().starts_with('\n') || chars.as_str().starts_with("\r\n") {
                    // A line continuation skips the line break and the
                    // following indentation.
                    let rest = chars.as_str().trim_start_matches([' ', '\t', '\n', '\r']);
                    chars = rest.chars();
                } else if let Some(error) = escape(&mut chars, mode) {
                    return Some(error);
                }
            }
            '\r' if !chars.as_str().starts_with('\n') => {
                return Some("bare CR not allowed in string, use `\\r` instead");
            }
            '\0' if mode == Mode::CStr => {
                return Some("null characters in C string literals are not supported");
            }
            c if mode == Mode::ByteStr && !c.is_ascii() => {
                return Some("non-ASCII character in byte string literal");
            }
            _ => {}
        }
    }
    None
}

fn raw_string(content: &str, mode: Mode) -> Option<&'static str> {
    if has_bare_cr(content) || content.ends_with('\r') {
        return Some("bare CR not allowed in raw string");
    }
    match mode {
        Mode::ByteStr if !content.is_ascii() => {
            Some("non-ASCII character in raw byte string literal")
        }
        Mode::CStr if content.contains('\0') => {
            Some("null characters in C string literals are not supported")
        }
        _ => None,
    }
}

/// Checks the escape after a backslash, consuming it.
fn escape(chars: &mut std::str::Chars<'_>, mode: Mode) -> Option<&'static str> {
    let null_in_c_string = Some("null characters in C string literals are not supported");
    match chars.next() {
        Some('"' | '\'' | 'n' | 'r' | 't' | '\\') => None,
        Some('0') if mode == Mode::CStr => null_in_c_string,
        Some('0') => None,
        Some('x') => {
            let mut value = 0u32;
            for _ in 0..2 {
                match chars.next() {
                    Some(c) if c.is_ascii_hexdigit() => {
                        value = value * 16 + c.to_digit(16).unwrap_or(0)
                    }
                    Some(_) => return Some("invalid character in numeric character escape"),
                    None => return Some("numeric character escape is too short"),
                }
            }
            if value > 0x7F && matches!(mode, Mode::Char | Mode::Str) {
                return Some("out of range hex escape");
            }
            if value == 0 && mode == Mode::CStr {
                return null_in_c_string;
            }
            None
        }
        Some('u') => {
            if mode.is_byte() {
                return Some("unicode escape in byte string");
            }
            if chars.next() != Some('{') {
                return Some("incorrect unicode escape sequence");
            }
            let mut value = 0u32;
            let mut digits = 0;
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some('_') if digits == 0 => return Some("invalid start of unicode escape"),
                    Some('_') => {}
                    Some(c) if c.is_ascii_hexdigit() => {
                        digits += 1;
                        if digits > 6 {
                            return Some("overlong unicode escape");
                        }
                        value = value * 16 + c.to_digit(16).unwrap_or(0);
                    }
                    Some(_) => return Some("invalid character in unicode escape"),
                    None => return Some("unterminated unicode escape"),
                }
            }
            if digits == 0 {
                return Some("empty unicode escape");
            }
            if char::from_u32(value).is_none() {
                return Some("invalid unicode character escape");
            }
            if value == 0 && mode == Mode::CStr {
                return null_in_c_string;
            }
            None
        }
        _ => Some("unknown character escape"),
    }
}
