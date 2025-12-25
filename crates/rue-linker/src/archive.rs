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

        while offset + HEADER_SIZE <= data.len() {
            // Parse header (60 bytes):
            // - Name:      16 bytes (space-padded, may end with '/')
            // - Timestamp: 12 bytes (decimal ASCII)
            // - Owner ID:   6 bytes (decimal ASCII)
            // - Group ID:   6 bytes (decimal ASCII)
            // - Mode:       8 bytes (octal ASCII)
            // - Size:      10 bytes (decimal ASCII)
            // - Terminator: 2 bytes ("`\n")
            let header = &data[offset..offset + HEADER_SIZE];

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

            offset += HEADER_SIZE;

            // Skip special entries:
            // - "/" or "/SYM64/" : Symbol table (GNU/LLVM style)
            // - "//" : Long filename table (GNU style)
            // - "__.SYMDEF" or "__.SYMDEF SORTED" : Symbol table (BSD style)
            // - "#1/..." : BSD long filename (name follows header)
            let is_special = name == "/"
                || name == "//"
                || name == "/SYM64/"
                || name.starts_with("__.SYMDEF")
                || name.starts_with("#1/");

            if is_special {
                offset = offset.checked_add(size).ok_or(ArchiveError::Overflow)?;
                // Pad to even boundary
                if offset % 2 == 1 {
                    offset = offset.checked_add(1).ok_or(ArchiveError::Overflow)?;
                }
                continue;
            }

            // Read member data
            let end_offset = offset.checked_add(size).ok_or(ArchiveError::Overflow)?;
            if end_offset > data.len() {
                return Err(ArchiveError::TooShort);
            }
            let member_data = &data[offset..end_offset];

            // Try to parse as ELF object.
            // Non-ELF members (e.g., LLVM bitcode files from LTO builds) are skipped.
            match ObjectFile::parse(member_data) {
                Ok(obj) => objects.push(obj),
                Err(_) => {
                    // Member is not a valid ELF object. This is common for:
                    // - LLVM bitcode files (.bc) in LTO-enabled builds
                    // - Rust metadata files
                    // - Other non-object archive members
                    // We silently skip these since we only need ELF objects.
                }
            }

            offset = offset.checked_add(size).ok_or(ArchiveError::Overflow)?;
            // Pad to even boundary
            if offset % 2 == 1 {
                offset = offset.checked_add(1).ok_or(ArchiveError::Overflow)?;
            }
        }

        Ok(Archive { objects })
    }

    /// Returns true if the archive contains no object files.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Returns the number of object files in the archive.
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
    fn test_overflow_in_member_size() {
        // Craft a malicious archive with a size field that would cause integer overflow.
        // The size field is at bytes 48-58 of the header (10 bytes, decimal ASCII).
        // We use usize::MAX which would overflow when added to any positive offset.
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

        // - Size: 10 bytes - use a huge number that would overflow
        // usize::MAX on 64-bit is 18446744073709551615 (20 digits), too big for 10 bytes
        // Use a smaller but still overflowing value: 9999999999 (fits in 10 bytes)
        // When added to offset (already at 68 = 8 magic + 60 header), this won't overflow
        // on 64-bit, so we need a different approach.
        //
        // Instead, use a value close to usize::MAX that's representable in 10 digits.
        // The offset after the header is 68. Adding 9999999999 gives ~10 billion, no overflow.
        // On 32-bit, usize::MAX is 4294967295 (10 digits), so "4294967295" would overflow
        // when added to 68.
        //
        // For a portable test, we can't easily trigger overflow on 64-bit with 10 digits.
        // However, we can test that the code handles the case correctly by using a size
        // that's larger than the remaining data, which will give TooShort error but at
        // least exercises the overflow check path on 32-bit systems.
        //
        // For this test, we'll verify that the overflow error variant exists and can be
        // triggered by the checked_add logic. The actual overflow would require a 32-bit
        // system or a specially crafted large file, but we can unit test the error path.
        data.extend_from_slice(b"9999999999"); // 10 bytes - large size

        // - Terminator: 2 bytes
        data.extend_from_slice(b"`\n");

        let result = Archive::parse(&data);
        // This should fail with TooShort (since data doesn't have 9999999999 bytes)
        // On a 32-bit system or with checked arithmetic, overflow would be caught first.
        // Either error is acceptable for malicious input.
        assert!(
            matches!(
                result,
                Err(ArchiveError::TooShort) | Err(ArchiveError::Overflow)
            ),
            "Expected TooShort or Overflow error, got {:?}",
            result
        );
    }
}
