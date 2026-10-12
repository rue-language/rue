//! Coordinate and URI conversions between the compiler and LSP clients.
//!
//! The compiler reports UTF-8 byte offsets. LSP positions are zero-based
//! lines plus UTF-16 code-unit columns (the protocol default, and the only
//! encoding every client supports). Every conversion goes through
//! [`LineIndex`], so the server has one place where those two coordinate
//! systems meet.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// A zero-based LSP position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    pub fn to_json(self) -> Value {
        json!({ "line": self.line, "character": self.character })
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            line: u32::try_from(value.get("line")?.as_u64()?).ok()?,
            character: u32::try_from(value.get("character")?.as_u64()?).ok()?,
        })
    }
}

/// A half-open LSP range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub fn to_json(self) -> Value {
        json!({ "start": self.start.to_json(), "end": self.end.to_json() })
    }
}

/// Line starts of one source text, for byte-offset/position conversion.
#[derive(Clone, Debug)]
pub struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Self { line_starts }
    }

    /// The LSP position of a byte offset. An offset past the end of the text
    /// clamps to the end; an offset inside a multi-byte character counts the
    /// character's code units before it, so it never splits a surrogate pair.
    pub fn position(&self, text: &str, offset: usize) -> Position {
        let offset = offset.min(text.len());
        let line = self.line_starts.partition_point(|start| *start <= offset) - 1;
        let line_start = self.line_starts[line];
        let character = text[line_start..]
            .char_indices()
            .take_while(|(index, _)| line_start + index < offset)
            .map(|(_, ch)| ch.len_utf16())
            .sum::<usize>();
        Position {
            line: line as u32,
            character: character as u32,
        }
    }

    pub fn range(&self, text: &str, start: usize, end: usize) -> Range {
        Range {
            start: self.position(text, start),
            end: self.position(text, end.max(start)),
        }
    }

    /// The byte offset of an LSP position. A column past the end of its line
    /// clamps to the line end, as the protocol requires; a line past the end
    /// of the text clamps to the end of the text.
    pub fn offset(&self, text: &str, position: Position) -> usize {
        let Some(&line_start) = self.line_starts.get(position.line as usize) else {
            return text.len();
        };
        let line_end = self
            .line_starts
            .get(position.line as usize + 1)
            .map_or(text.len(), |next| next - 1);
        let mut units = 0usize;
        for (index, ch) in text[line_start..line_end].char_indices() {
            if units >= position.character as usize {
                return line_start + index;
            }
            units += ch.len_utf16();
        }
        line_end
    }
}

/// The filesystem path a `file:` URI names, or `None` for any other scheme.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///path` has an empty authority; `file://localhost/path` names
    // this host explicitly. Any other authority is a remote file.
    let path = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost")?
    };
    let decoded = percent_decode(path)?;
    // Windows clients send `file:///c:/...`; the leading slash is not part of
    // the drive path.
    if cfg!(windows) {
        let bytes = decoded.as_bytes();
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
            return Some(PathBuf::from(&decoded[1..]));
        }
    }
    Some(PathBuf::from(decoded))
}

/// The `file:` URI of an absolute filesystem path.
pub fn path_to_uri(path: &Path) -> String {
    let spelled = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !spelled.starts_with('/') {
        uri.push('/');
    }
    for byte in spelled.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_utf16_code_units() {
        let text = "a\nßx😀y\n";
        let index = LineIndex::new(text);
        // `ß` is two UTF-8 bytes and one UTF-16 unit; `😀` is four bytes
        // and two units.
        let y = text.find('y').unwrap();
        assert_eq!(
            index.position(text, y),
            Position {
                line: 1,
                character: 4
            }
        );
        assert_eq!(index.offset(text, index.position(text, y)), y);
    }

    #[test]
    fn offsets_clamp_to_line_and_text_ends() {
        let text = "ab\ncd";
        let index = LineIndex::new(text);
        assert_eq!(
            index.offset(
                text,
                Position {
                    line: 0,
                    character: 99
                }
            ),
            2
        );
        assert_eq!(
            index.offset(
                text,
                Position {
                    line: 9,
                    character: 0
                }
            ),
            text.len()
        );
        assert_eq!(
            index.position(text, 999),
            Position {
                line: 1,
                character: 2
            }
        );
    }

    #[test]
    fn file_uris_round_trip_through_percent_encoding() {
        let path = Path::new("/tmp/my project/main.rue");
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///tmp/my%20project/main.rue");
        assert_eq!(uri_to_path(&uri).unwrap(), path);
        assert_eq!(
            uri_to_path("file://localhost/a/b.rue").unwrap(),
            Path::new("/a/b.rue")
        );
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
        assert_eq!(uri_to_path("file://remote/a.rue"), None);
    }
}
