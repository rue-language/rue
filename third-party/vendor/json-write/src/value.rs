#[cfg(feature = "alloc")]
use alloc::borrow::Cow;
#[cfg(feature = "alloc")]
use alloc::string::String;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use crate::JsonWrite;
use crate::WriteJsonKey;

#[cfg(feature = "alloc")]
pub trait ToJsonValue {
    fn to_json_value(&self) -> String;
}

#[cfg(feature = "alloc")]
impl<T> ToJsonValue for T
where
    T: WriteJsonValue + ?Sized,
{
    fn to_json_value(&self) -> String {
        let mut result = String::new();
        let _ = self.write_json_value(&mut result);
        result
    }
}

pub trait WriteJsonValue {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result;
}

impl WriteJsonValue for bool {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for u8 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for i8 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for u16 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for i16 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for u32 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for i32 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for u64 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for i64 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for u128 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for i128 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write!(writer, "{self}")
    }
}

impl WriteJsonValue for f32 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        if self.is_nan() || self.is_infinite() {
            None::<Self>.write_json_value(writer)
        } else {
            if self % 1.0 == 0.0 {
                write!(writer, "{self}.0")
            } else {
                write!(writer, "{self}")
            }
        }
    }
}

impl WriteJsonValue for f64 {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        if self.is_nan() || self.is_infinite() {
            None::<Self>.write_json_value(writer)
        } else {
            if self % 1.0 == 0.0 {
                write!(writer, "{self}.0")
            } else {
                write!(writer, "{self}")
            }
        }
    }
}

impl WriteJsonValue for char {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        let mut buf = [0; 4];
        let v = self.encode_utf8(&mut buf);
        v.write_json_value(writer)
    }
}

impl WriteJsonValue for str {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write_json_str(self, writer)
    }
}

#[cfg(feature = "alloc")]
impl WriteJsonValue for String {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        self.as_str().write_json_value(writer)
    }
}

#[cfg(feature = "alloc")]
impl WriteJsonValue for Cow<'_, str> {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        self.as_ref().write_json_value(writer)
    }
}

impl<T: WriteJsonValue> WriteJsonValue for Option<T> {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        match self {
            Some(v) => v.write_json_value(writer),
            None => write_json_null(writer),
        }
    }
}

impl<V: WriteJsonValue> WriteJsonValue for [V] {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        writer.open_array()?;
        let mut iter = self.iter();
        if let Some(v) = iter.next() {
            writer.value(v)?;
        }
        for v in iter {
            writer.val_sep()?;
            writer.space()?;
            writer.value(v)?;
        }
        writer.close_array()?;
        Ok(())
    }
}

impl<V: WriteJsonValue, const N: usize> WriteJsonValue for [V; N] {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        self.as_slice().write_json_value(writer)
    }
}

#[cfg(feature = "alloc")]
impl<V: WriteJsonValue> WriteJsonValue for Vec<V> {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        self.as_slice().write_json_value(writer)
    }
}

#[cfg(feature = "alloc")]
impl<K: WriteJsonKey, V: WriteJsonValue> WriteJsonValue for alloc::collections::BTreeMap<K, V> {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write_json_object(self.iter(), writer)
    }
}

#[cfg(feature = "std")]
impl<K: WriteJsonKey, V: WriteJsonValue> WriteJsonValue for std::collections::HashMap<K, V> {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        write_json_object(self.iter(), writer)
    }
}

impl<V: WriteJsonValue + ?Sized> WriteJsonValue for &V {
    fn write_json_value<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        (*self).write_json_value(writer)
    }
}

pub(crate) fn write_json_null<W: JsonWrite + ?Sized>(writer: &mut W) -> core::fmt::Result {
    write!(writer, "null")
}

pub(crate) fn write_json_str<W: JsonWrite + ?Sized>(
    value: &str,
    writer: &mut W,
) -> core::fmt::Result {
    write!(writer, "\"")?;
    format_escaped_str_contents(writer, value)?;
    write!(writer, "\"")?;
    Ok(())
}

fn format_escaped_str_contents<W>(writer: &mut W, value: &str) -> core::fmt::Result
where
    W: ?Sized + JsonWrite,
{
    let mut bytes = value.as_bytes();

    let mut i = 0;
    while i < bytes.len() {
        let (string_run, rest) = bytes.split_at(i);
        let (&byte, rest) = rest.split_first().unwrap();

        let escape = ESCAPE[byte as usize];

        i += 1;
        if escape == 0 {
            continue;
        }

        bytes = rest;
        i = 0;

        // Safety: string_run is a valid utf8 string, since we only split on ascii sequences
        let string_run = unsafe { core::str::from_utf8_unchecked(string_run) };
        if !string_run.is_empty() {
            write!(writer, "{string_run}")?;
        }

        let char_escape = match escape {
            BB => CharEscape::Backspace,
            TT => CharEscape::Tab,
            NN => CharEscape::LineFeed,
            FF => CharEscape::FormFeed,
            RR => CharEscape::CarriageReturn,
            QU => CharEscape::Quote,
            BS => CharEscape::ReverseSolidus,
            UU => CharEscape::AsciiControl(byte),
            // Safety: the escape table does not contain any other type of character.
            _ => unsafe { core::hint::unreachable_unchecked() },
        };
        write_char_escape(writer, char_escape)?;
    }

    // Safety: bytes is a valid utf8 string, since we only split on ascii sequences
    let string_run = unsafe { core::str::from_utf8_unchecked(bytes) };
    if string_run.is_empty() {
        return Ok(());
    }

    write!(writer, "{string_run}")?;
    Ok(())
}

const BB: u8 = b'b'; // \x08
const TT: u8 = b't'; // \x09
const NN: u8 = b'n'; // \x0A
const FF: u8 = b'f'; // \x0C
const RR: u8 = b'r'; // \x0D
const QU: u8 = b'"'; // \x22
const BS: u8 = b'\\'; // \x5C
const UU: u8 = b'u'; // \x00...\x1F except the ones above
const __: u8 = 0;

// Lookup table of escape sequences. A value of b'x' at index i means that byte
// i is escaped as "\x" in JSON. A value of 0 means that byte i is not escaped.
static ESCAPE: [u8; 256] = [
    //   1   2   3   4   5   6   7   8   9   A   B   C   D   E   F
    UU, UU, UU, UU, UU, UU, UU, UU, BB, TT, NN, UU, FF, RR, UU, UU, // 0
    UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, UU, // 1
    __, __, QU, __, __, __, __, __, __, __, __, __, __, __, __, __, // 2
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 3
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 4
    __, __, __, __, __, __, __, __, __, __, __, __, BS, __, __, __, // 5
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 6
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 7
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 8
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 9
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // A
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // B
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // C
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // D
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // E
    __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // F
];

/// Represents a character escape code in a type-safe manner.
enum CharEscape {
    /// An escaped quote `"`
    Quote,
    /// An escaped reverse solidus `\`
    ReverseSolidus,
    /// An escaped backspace character (usually escaped as `\b`)
    Backspace,
    /// An escaped form feed character (usually escaped as `\f`)
    FormFeed,
    /// An escaped line feed character (usually escaped as `\n`)
    LineFeed,
    /// An escaped carriage return character (usually escaped as `\r`)
    CarriageReturn,
    /// An escaped tab character (usually escaped as `\t`)
    Tab,
    /// An escaped ASCII plane control character (usually escaped as
    /// `\u00XX` where `XX` are two hex characters)
    AsciiControl(u8),
}

fn write_char_escape<W>(writer: &mut W, char_escape: CharEscape) -> core::fmt::Result
where
    W: ?Sized + JsonWrite,
{
    let escape_char = match char_escape {
        CharEscape::Quote => '"',
        CharEscape::ReverseSolidus => '\\',
        CharEscape::Backspace => 'b',
        CharEscape::FormFeed => 'f',
        CharEscape::LineFeed => 'n',
        CharEscape::CarriageReturn => 'r',
        CharEscape::Tab => 't',
        CharEscape::AsciiControl(_) => 'u',
    };

    match char_escape {
        CharEscape::AsciiControl(byte) => {
            static HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";
            let first = HEX_DIGITS[(byte >> 4) as usize] as char;
            let second = HEX_DIGITS[(byte & 0xF) as usize] as char;
            write!(writer, "\\{escape_char}00{first}{second}")
        }
        _ => {
            write!(writer, "\\{escape_char}")
        }
    }
}

fn write_json_object<
    'i,
    I: Iterator<Item = (&'i K, &'i V)>,
    K: WriteJsonKey + 'i,
    V: WriteJsonValue + 'i,
    W: JsonWrite + ?Sized,
>(
    mut iter: I,
    writer: &mut W,
) -> core::fmt::Result {
    writer.open_object()?;
    let mut trailing_space = false;
    if let Some((key, value)) = iter.next() {
        writer.space()?;
        writer.key(key)?;
        writer.keyval_sep()?;
        writer.space()?;
        writer.value(value)?;
        trailing_space = true;
    }
    for (key, value) in iter {
        writer.val_sep()?;
        writer.space()?;
        writer.key(key)?;
        writer.keyval_sep()?;
        writer.space()?;
        writer.value(value)?;
    }
    if trailing_space {
        writer.space()?;
    }
    writer.close_object()?;
    Ok(())
}
