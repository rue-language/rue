//! Character classes from the Rust reference's lexical grammar.

/// Whether `c` is Rust whitespace: the Unicode `Pattern_White_Space` set.
pub const fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'   // tab
        | '\u{000A}' // line feed
        | '\u{000B}' // vertical tab
        | '\u{000C}' // form feed
        | '\u{000D}' // carriage return
        | '\u{0020}' // space
        | '\u{0085}' // next line
        | '\u{200E}' // left-to-right mark
        | '\u{200F}' // right-to-left mark
        | '\u{2028}' // line separator
        | '\u{2029}' // paragraph separator
    )
}

/// Whether `c` can start an identifier: `_` or `XID_Start`.
#[inline]
pub fn is_id_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic() || (!c.is_ascii() && unicode_ident::is_xid_start(c))
}

/// Whether `c` can continue an identifier: `XID_Continue`.
#[inline]
pub fn is_id_continue(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric() || (!c.is_ascii() && unicode_ident::is_xid_continue(c))
}

/// Whether `s` is a single (non-raw) identifier or keyword.
pub fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if is_id_start(c) => chars.all(is_id_continue),
        _ => false,
    }
}
