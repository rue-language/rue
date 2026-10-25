//! The character cursor that finds token boundaries.
//!
//! The rules follow the Rust reference's lexical grammar and match rustc's
//! token boundaries, including its recovery choices for malformed input (for
//! example, where an unterminated character literal stops).

use crate::{
    Base, DocStyle, LiteralKind, Token, TokenKind, is_id_continue, is_id_start, is_whitespace,
};
use std::str::Chars;

const EOF_CHAR: char = '\0';

pub(crate) struct Cursor<'a> {
    len_remaining: usize,
    chars: Chars<'a>,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(input: &'a str) -> Cursor<'a> {
        Cursor {
            len_remaining: input.len(),
            chars: input.chars(),
        }
    }

    /// The next character without consuming it, or `EOF_CHAR` at the end.
    /// `EOF_CHAR` is also a valid character, so callers that care check
    /// [`Cursor::is_eof`].
    fn first(&self) -> char {
        self.chars.clone().next().unwrap_or(EOF_CHAR)
    }

    fn second(&self) -> char {
        let mut iter = self.chars.clone();
        iter.next();
        iter.next().unwrap_or(EOF_CHAR)
    }

    fn third(&self) -> char {
        let mut iter = self.chars.clone();
        iter.next();
        iter.next();
        iter.next().unwrap_or(EOF_CHAR)
    }

    fn is_eof(&self) -> bool {
        self.chars.as_str().is_empty()
    }

    fn pos_within_token(&self) -> u32 {
        (self.len_remaining - self.chars.as_str().len()) as u32
    }

    fn reset_pos_within_token(&mut self) {
        self.len_remaining = self.chars.as_str().len();
    }

    fn bump(&mut self) -> Option<char> {
        self.chars.next()
    }

    fn eat_while(&mut self, mut predicate: impl FnMut(char) -> bool) {
        while predicate(self.first()) && !self.is_eof() {
            self.bump();
        }
    }

    pub(crate) fn advance_token(&mut self) -> Option<Token> {
        let first_char = self.bump()?;
        let kind = match first_char {
            '/' => match self.first() {
                '/' => self.line_comment(),
                '*' => self.block_comment(),
                _ => TokenKind::Slash,
            },

            c if is_whitespace(c) => {
                self.eat_while(is_whitespace);
                TokenKind::Whitespace
            }

            // Raw identifier, raw string, or an identifier starting with `r`.
            'r' => match (self.first(), self.second()) {
                ('#', c) if is_id_start(c) => {
                    self.bump();
                    self.eat_identifier();
                    TokenKind::RawIdent
                }
                ('#', _) | ('"', _) => {
                    let n_hashes = self.raw_double_quoted_string();
                    self.literal(LiteralKind::RawStr { n_hashes })
                }
                _ => self.ident_or_unknown_prefix(),
            },

            'b' => self.prefixed_string(
                |terminated| LiteralKind::ByteStr { terminated },
                |n_hashes| LiteralKind::RawByteStr { n_hashes },
                true,
            ),
            'c' => self.prefixed_string(
                |terminated| LiteralKind::CStr { terminated },
                |n_hashes| LiteralKind::RawCStr { n_hashes },
                false,
            ),

            c if is_id_start(c) => self.ident_or_unknown_prefix(),

            c @ '0'..='9' => {
                let kind = self.number(c);
                self.literal(kind)
            }

            ';' => TokenKind::Semi,
            ',' => TokenKind::Comma,
            '.' => TokenKind::Dot,
            '(' => TokenKind::OpenParen,
            ')' => TokenKind::CloseParen,
            '{' => TokenKind::OpenBrace,
            '}' => TokenKind::CloseBrace,
            '[' => TokenKind::OpenBracket,
            ']' => TokenKind::CloseBracket,
            '@' => TokenKind::At,
            '#' => TokenKind::Pound,
            '~' => TokenKind::Tilde,
            '?' => TokenKind::Question,
            ':' => TokenKind::Colon,
            '$' => TokenKind::Dollar,
            '=' => TokenKind::Eq,
            '!' => TokenKind::Bang,
            '<' => TokenKind::Lt,
            '>' => TokenKind::Gt,
            '-' => TokenKind::Minus,
            '&' => TokenKind::And,
            '|' => TokenKind::Or,
            '+' => TokenKind::Plus,
            '*' => TokenKind::Star,
            '^' => TokenKind::Caret,
            '%' => TokenKind::Percent,

            '\'' => self.lifetime_or_char(),

            '"' => {
                let terminated = self.double_quoted_string();
                self.literal(LiteralKind::Str { terminated })
            }

            _ => TokenKind::Unknown,
        };
        let token = Token::new(kind, self.pos_within_token());
        self.reset_pos_within_token();
        Some(token)
    }

    /// Finishes a literal by consuming its suffix.
    fn literal(&mut self, kind: LiteralKind) -> TokenKind {
        let suffix_start = self.pos_within_token();
        self.eat_literal_suffix();
        TokenKind::Literal { kind, suffix_start }
    }

    fn line_comment(&mut self) -> TokenKind {
        self.bump(); // second '/'
        let doc_style = match self.first() {
            // `//!` is an inner doc comment.
            '!' => Some(DocStyle::Inner),
            // `////` (more than three slashes) is not a doc comment.
            '/' if self.second() != '/' => Some(DocStyle::Outer),
            _ => None,
        };
        self.eat_while(|c| c != '\n');
        TokenKind::LineComment { doc_style }
    }

    fn block_comment(&mut self) -> TokenKind {
        self.bump(); // '*'
        let doc_style = match self.first() {
            '!' => Some(DocStyle::Inner),
            // `/***` (more than two stars) and `/**/` are not doc comments.
            '*' if !matches!(self.second(), '*' | '/') => Some(DocStyle::Outer),
            _ => None,
        };

        let mut depth = 1usize;
        while let Some(c) = self.bump() {
            match c {
                '/' if self.first() == '*' => {
                    self.bump();
                    depth += 1;
                }
                '*' if self.first() == '/' => {
                    self.bump();
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        TokenKind::BlockComment {
            doc_style,
            terminated: depth == 0,
        }
    }

    fn eat_identifier(&mut self) {
        if !is_id_start(self.first()) {
            return;
        }
        self.bump();
        self.eat_while(is_id_continue);
    }

    fn ident_or_unknown_prefix(&mut self) -> TokenKind {
        // The first character has already been consumed.
        self.eat_while(is_id_continue);
        match self.first() {
            '#' | '"' | '\'' => TokenKind::UnknownPrefix,
            _ => TokenKind::Ident,
        }
    }

    /// Lexes after a `b` or `c`: a byte/byte-string/C-string literal, its raw
    /// form, or an identifier starting with that letter. Only `b` has a
    /// single-quoted (byte) form.
    fn prefixed_string(
        &mut self,
        quoted: fn(bool) -> LiteralKind,
        raw: fn(Option<u8>) -> LiteralKind,
        allow_byte: bool,
    ) -> TokenKind {
        match (self.first(), self.second()) {
            ('\'', _) if allow_byte => {
                self.bump();
                let terminated = self.single_quoted_string();
                self.literal(LiteralKind::Byte { terminated })
            }
            ('"', _) => {
                self.bump();
                let terminated = self.double_quoted_string();
                self.literal(quoted(terminated))
            }
            ('r', '"') | ('r', '#') => {
                self.bump();
                let n_hashes = self.raw_double_quoted_string();
                self.literal(raw(n_hashes))
            }
            _ => self.ident_or_unknown_prefix(),
        }
    }

    fn number(&mut self, first_digit: char) -> LiteralKind {
        let mut base = Base::Decimal;
        if first_digit == '0' {
            // A base prefix, or a decimal number with a leading zero.
            match self.first() {
                'b' => {
                    base = Base::Binary;
                    self.bump();
                    if !self.eat_decimal_digits() {
                        return LiteralKind::Int {
                            base,
                            empty_int: true,
                        };
                    }
                }
                'o' => {
                    base = Base::Octal;
                    self.bump();
                    if !self.eat_decimal_digits() {
                        return LiteralKind::Int {
                            base,
                            empty_int: true,
                        };
                    }
                }
                'x' => {
                    base = Base::Hexadecimal;
                    self.bump();
                    if !self.eat_hexadecimal_digits() {
                        return LiteralKind::Int {
                            base,
                            empty_int: true,
                        };
                    }
                }
                '0'..='9' | '_' => {
                    self.eat_decimal_digits();
                }
                // `0.` and `0e` continue below as possible floats.
                '.' | 'e' | 'E' => {}
                _ => {
                    return LiteralKind::Int {
                        base,
                        empty_int: false,
                    };
                }
            }
        } else {
            self.eat_decimal_digits();
        }

        match self.first() {
            // `1.` is a float unless followed by another `.` (a range, `1..2`)
            // or an identifier start (a field or method, `1.foo()`).
            '.' if self.second() != '.' && !is_id_start(self.second()) => {
                self.bump();
                let mut empty_exponent = false;
                if self.first().is_ascii_digit() {
                    self.eat_decimal_digits();
                    if matches!(self.first(), 'e' | 'E') {
                        self.bump();
                        empty_exponent = !self.eat_float_exponent();
                    }
                }
                LiteralKind::Float {
                    base,
                    empty_exponent,
                }
            }
            'e' | 'E' => {
                self.bump();
                let empty_exponent = !self.eat_float_exponent();
                LiteralKind::Float {
                    base,
                    empty_exponent,
                }
            }
            _ => LiteralKind::Int {
                base,
                empty_int: false,
            },
        }
    }

    fn eat_decimal_digits(&mut self) -> bool {
        let mut has_digits = false;
        loop {
            match self.first() {
                '_' => {
                    self.bump();
                }
                '0'..='9' => {
                    has_digits = true;
                    self.bump();
                }
                _ => break,
            }
        }
        has_digits
    }

    fn eat_hexadecimal_digits(&mut self) -> bool {
        let mut has_digits = false;
        loop {
            match self.first() {
                '_' => {
                    self.bump();
                }
                '0'..='9' | 'a'..='f' | 'A'..='F' => {
                    has_digits = true;
                    self.bump();
                }
                _ => break,
            }
        }
        has_digits
    }

    /// Eats an optional sign and the exponent digits; returns whether any
    /// digit was found.
    fn eat_float_exponent(&mut self) -> bool {
        if matches!(self.first(), '-' | '+') {
            self.bump();
        }
        self.eat_decimal_digits()
    }

    fn eat_literal_suffix(&mut self) {
        self.eat_identifier();
    }

    fn lifetime_or_char(&mut self) -> TokenKind {
        // `'x'` is a character even when `x` could start a lifetime.
        let can_be_a_lifetime = if self.second() == '\'' {
            false
        } else {
            is_id_start(self.first()) || self.first().is_ascii_digit()
        };

        if !can_be_a_lifetime {
            let terminated = self.single_quoted_string();
            return self.literal(LiteralKind::Char { terminated });
        }

        if self.first() == 'r' && self.second() == '#' && is_id_start(self.third()) {
            self.bump();
            self.bump();
            self.eat_identifier();
            return TokenKind::RawLifetime;
        }

        let starts_with_number = self.first().is_ascii_digit();
        self.bump();
        self.eat_while(is_id_continue);

        if self.first() == '\'' {
            // `'ab'`: a malformed multi-character literal. Treating it as one
            // character token keeps the rest of the line lexing sensibly.
            self.bump();
            self.literal(LiteralKind::Char { terminated: true })
        } else {
            TokenKind::Lifetime { starts_with_number }
        }
    }

    /// Lexes the body of a `'...'` literal after the opening quote.
    fn single_quoted_string(&mut self) -> bool {
        // The common one-character case.
        if self.second() == '\'' && self.first() != '\\' {
            self.bump();
            self.bump();
            return true;
        }

        loop {
            match self.first() {
                '\'' => {
                    self.bump();
                    return true;
                }
                // A comment or newline cannot be inside a character literal;
                // stop so recovery resumes at a plausible boundary.
                '/' => break,
                '\n' if self.second() != '\'' => break,
                EOF_CHAR if self.is_eof() => break,
                '\\' => {
                    self.bump();
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
        false
    }

    /// Lexes the body of a `"..."` literal after the opening quote.
    fn double_quoted_string(&mut self) -> bool {
        while let Some(c) = self.bump() {
            match c {
                '"' => return true,
                '\\' if self.first() == '\\' || self.first() == '"' => {
                    self.bump();
                }
                _ => {}
            }
        }
        false
    }

    /// Lexes `#*"..."#*` after a raw prefix (`r`, `br` or `cr`). Returns the
    /// hash count, or `None` if malformed.
    fn raw_double_quoted_string(&mut self) -> Option<u8> {
        let mut n_start_hashes: u32 = 0;
        while self.first() == '#' {
            n_start_hashes += 1;
            self.bump();
        }

        if self.first() != '"' {
            // `r#` followed by something other than a quote (`r##x`).
            return None;
        }
        self.bump();

        loop {
            self.eat_while(|c| c != '"');
            if self.is_eof() {
                return None;
            }
            self.bump(); // '"'

            let mut n_end_hashes = 0;
            while self.first() == '#' && n_end_hashes < n_start_hashes {
                n_end_hashes += 1;
                self.bump();
            }
            if n_end_hashes == n_start_hashes {
                return u8::try_from(n_start_hashes).ok();
            }
        }
    }
}
