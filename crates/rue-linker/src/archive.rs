//! AR archive (.a) file parsing.
//!
//! This module parses Unix ar archives, which are used for static libraries.
//! Rust's `crate-type = "staticlib"` produces these.

use crate::elf::{ObjectFile, ParseError};

/// AR archive magic bytes.
const AR_MAGIC: &[u8] = b"!<arch>\n";

/// Size of an ar member header.
const HEADER_SIZE: usize = 60;

/// A parsed ar archive containing multiple object files.
#[derive(Debug)]
pub struct Archive {
    /// The object files contained in this archive.
    pub objects: Vec<ObjectFile>,
}

/// Error type for archive parsing.
#[derive(Debug)]
pub enum ArchiveError {
    /// Invalid ar magic number.
    InvalidMagic,
    /// Archive is too short.
    TooShort,
    /// Invalid header format.
    InvalidHeader(String),
    /// Failed to parse contained object.
    ObjectParse(ParseError),
    /// Integer overflow in size calculation.
    Overflow,
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::InvalidMagic => write!(f, "invalid ar archive magic"),
            ArchiveError::TooShort => write!(f, "archive too short"),
            ArchiveError::InvalidHeader(s) => write!(f, "invalid archive header: {}", s),
            ArchiveError::ObjectParse(e) => write!(f, "failed to parse object: {}", e),
            ArchiveError::Overflow => write!(f, "integer overflow in archive size calculation"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<ParseError> for ArchiveError {
    fn from(e: ParseError) -> Self {
        ArchiveError::ObjectParse(e)
    }
}

fn checked_offset_add(offset: usize, amount: usize) -> Result<usize, ArchiveError> {
    offset.checked_add(amount).ok_or(ArchiveError::Overflow)
}

impl Archive {
    /// Parse an ar archive from bytes.
    ///
    /// This parses the Unix ar format:
    /// - 8-byte global magic: `!<arch>\n`
    /// - Series of member entries, each with:
    ///   - 60-byte header (name, timestamp, uid, gid, mode, size, terminator)
    ///   - File content (padded to even boundary)
    ///
    /// Special entries like symbol tables (`/`, `//`, `__.SYMDEF`) are skipped.
    #[must_use = "parsing returns a Result that must be checked"]
    pub fn parse(data: &[u8]) -> Result<Self, ArchiveError> {
        // Check magic
        if data.len() < AR_MAGIC.len() {
            return Err(ArchiveError::TooShort);
        }
        if &data[..AR_MAGIC.len()] != AR_MAGIC {
            return Err(ArchiveError::InvalidMagic);
        }

        let mut objects = Vec::new();
        let mut offset = AR_MAGIC.len();

        loop {
            let header_end = checked_offset_add(offset, HEADER_SIZE)?;
            if header_end > data.len() {
                break;
            }

            // Parse header (60 bytes):
            // - Name:      16 bytes (space-padded, may end with '/')
            // - Timestamp: 12 bytes (decimal ASCII)
            // - Owner ID:   6 bytes (decimal ASCII)
            // - Group ID:   6 bytes (decimal ASCII)
            // - Mode:       8 bytes (octal ASCII)
            // - Size:      10 bytes (decimal ASCII)
            // - Terminator: 2 bytes ("`\n")
            let header = &data[offset..header_end];

            // Name: first 16 bytes
            let name = std::str::from_utf8(&header[0..16])
                .map_err(|_| ArchiveError::InvalidHeader("invalid name encoding".into()))?
                .trim();

            // Size: bytes 48-58 (10 bytes), decimal ASCII
            let size_str = std::str::from_utf8(&header[48..58])
                .map_err(|_| ArchiveError::InvalidHeader("invalid size encoding".into()))?
                .trim();
            let size: usize = size_str.parse().map_err(|_| {
                ArchiveError::InvalidHeader(format!("invalid size: '{}'", size_str))
            })?;

            // Header terminator should be "`\n"
            if &header[58..60] != b"`\n" {
                return Err(ArchiveError::InvalidHeader("invalid terminator".into()));
            }

            offset = header_end;

            // Handle BSD long filename format (#1/N where N is the name length)
            // The real filename is embedded at the start of the member data.
            let (actual_name, name_len) = if name.starts_with("#1/") {
                let name_len: usize = name[3..].trim().parse().map_err(|_| {
                    ArchiveError::InvalidHeader(format!(
                        "invalid BSD name length: '{}'",
                        &name[3..]
                    ))
                })?;
                // The actual name is at the start of the member data
                let name_end = checked_offset_add(offset, name_len)?;
                if name_end > data.len() {
                    return Err(ArchiveError::TooShort);
                }
                let actual_name = std::str::from_utf8(&data[offset..name_end])
                    .map_err(|_| ArchiveError::InvalidHeader("invalid BSD name encoding".into()))?
                    .trim_end_matches('\0')
                    .to_string();
                (actual_name, name_len)
            } else {
                (name.to_string(), 0)
            };

            // Skip special entries:
            // - "/" or "/SYM64/" : Symbol table (GNU/LLVM style)
            // - "//" : Long filename table (GNU style)
            // - "__.SYMDEF" or "__.SYMDEF SORTED" : Symbol table (BSD style)
            let is_special = actual_name == "/"
                || actual_name == "//"
                || actual_name == "/SYM64/"
                || actual_name.starts_with("__.SYMDEF");

            if is_special {
                offset = checked_offset_add(offset, size)?;
                // Pad to even boundary
                if offset % 2 == 1 {
                    offset = checked_offset_add(offset, 1)?;
                }
                continue;
            }

            // Read member data (skip over BSD long filename if present)
            let member_start = checked_offset_add(offset, name_len)?;
            let member_size = size.checked_sub(name_len).ok_or(ArchiveError::Overflow)?;
            let end_offset = checked_offset_add(member_start, member_size)?;
            if end_offset > data.len() {
                return Err(ArchiveError::TooShort);
            }
            let member_data = &data[member_start..end_offset];

            // Try to parse as object file (ELF or Mach-O).
            // Non-object members (e.g., LLVM bitcode files from LTO builds) are skipped.
            match ObjectFile::parse(member_data) {
                Ok(obj) => objects.push(obj),
                Err(_) => {
                    // Member is not a valid object file. This is common for:
                    // - LLVM bitcode files (.bc) in LTO-enabled builds
                    // - Rust metadata files
                    // - Other non-object archive members
                    // We silently skip these since we only need object files.
                }
            }

            offset = checked_offset_add(offset, size)?;
            // Pad to even boundary
            if offset % 2 == 1 {
                offset = checked_offset_add(offset, 1)?;
            }
        }

        Ok(Archive { objects })
    }

    /// Returns true if the archive contains no object files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Returns the number of object files in the archive.
    #[must_use]
    pub fn len(&self) -> usize {
        self.objects.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_magic() {
        let data = b"not an archive";
        let result = Archive::parse(data);
        assert!(matches!(result, Err(ArchiveError::InvalidMagic)));
    }

    #[test]
    fn test_too_short() {
        let data = b"!<arch";
        let result = Archive::parse(data);
        assert!(matches!(result, Err(ArchiveError::TooShort)));
    }

    #[test]
    fn test_empty_archive() {
        // Just the magic, no members
        let data = b"!<arch>\n";
        let archive = Archive::parse(data).unwrap();
        assert!(archive.is_empty());
    }

    #[test]
    fn test_archive_error_display() {
        assert_eq!(
            ArchiveError::InvalidMagic.to_string(),
            "invalid ar archive magic"
        );
        assert_eq!(ArchiveError::TooShort.to_string(), "archive too short");
        assert_eq!(
            ArchiveError::InvalidHeader("test".into()).to_string(),
            "invalid archive header: test"
        );
        assert_eq!(
            ArchiveError::Overflow.to_string(),
            "integer overflow in archive size calculation"
        );
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn test_oversized_member_is_too_short() {
        // The largest size representable by the ten-byte field is valid on 64-bit hosts,
        // but this fixture does not contain the claimed member data.
        let mut data = Vec::new();

        // AR magic
        data.extend_from_slice(b"!<arch>\n");

        // Member header (60 bytes):
        // - Name: 16 bytes
        data.extend_from_slice(b"test.o          "); // 16 bytes, space-padded

        // - Timestamp: 12 bytes
        data.extend_from_slice(b"0           "); // 12 bytes

        // - Owner ID: 6 bytes
        data.extend_from_slice(b"0     "); // 6 bytes

        // - Group ID: 6 bytes
        data.extend_from_slice(b"0     "); // 6 bytes

        // - Mode: 8 bytes
        data.extend_from_slice(b"644     "); // 8 bytes

        // - Size: 10 bytes
        data.extend_from_slice(b"9999999999");

        // - Terminator: 2 bytes
        data.extend_from_slice(b"`\n");

        let result = Archive::parse(&data);
        assert!(
            matches!(result, Err(ArchiveError::TooShort)),
            "expected TooShort error, got {:?}",
            result
        );
    }

    #[test]
    fn test_checked_offset_add_overflow() {
        assert_eq!(checked_offset_add(usize::MAX - 1, 1).unwrap(), usize::MAX);
        assert!(matches!(
            checked_offset_add(usize::MAX, 1),
            Err(ArchiveError::Overflow)
        ));
    }
}
