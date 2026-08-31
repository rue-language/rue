//! AR archive (.a) file parsing.
//!
//! This module parses Unix ar archives, which are used for static libraries.
//! Rust's `crate-type = "staticlib"` produces these.

use crate::elf::{ObjectFile, ObjectSymbols, ParseError};

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
    /// Cooperative cancellation was requested by the caller.
    Canceled,
    /// Invalid ar magic number.
    InvalidMagic,
    /// Archive is too short.
    TooShort,
    /// Invalid header format.
    InvalidHeader(String),
    /// Failed to parse contained object.
    ObjectParse(ParseError),
    /// A member with a recognized object-file magic was malformed or unsupported.
    ObjectMemberParse { member: String, source: ParseError },
    /// Integer overflow in size calculation.
    Overflow,
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::Canceled => write!(f, "archive parsing canceled"),
            ArchiveError::InvalidMagic => write!(f, "invalid ar archive magic"),
            ArchiveError::TooShort => write!(f, "archive too short"),
            ArchiveError::InvalidHeader(s) => write!(f, "invalid archive header: {}", s),
            ArchiveError::ObjectParse(e) => write!(f, "failed to parse object: {}", e),
            ArchiveError::ObjectMemberParse { member, source } => {
                write!(f, "failed to parse object member `{member}`: {source}")
            }
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
        Self::parse_impl(data, false, &mut || false)
    }

    /// Parse an archive while rejecting malformed members that advertise a
    /// supported ELF or Mach-O object-file magic.
    ///
    /// Rust static libraries also contain symbol indexes, metadata, and may
    /// contain LLVM bitcode. Those non-object members remain intentionally
    /// ignored. A member beginning with a recognized native-object magic,
    /// however, must parse successfully instead of silently disappearing.
    #[must_use = "parsing returns a Result that must be checked"]
    pub fn parse_strict_objects(data: &[u8]) -> Result<Self, ArchiveError> {
        Self::parse_impl(data, true, &mut || false)
    }

    /// Parse a strict archive with bounded caller-owned cancellation checks.
    pub fn parse_strict_objects_with_cancellation(
        data: &[u8],
        mut cancellation: impl FnMut() -> bool,
    ) -> Result<Self, ArchiveError> {
        Self::parse_impl(data, true, &mut cancellation)
    }

    fn parse_impl(
        data: &[u8],
        strict_objects: bool,
        cancellation: &mut impl FnMut() -> bool,
    ) -> Result<Self, ArchiveError> {
        let members = collect_members(data, strict_objects, cancellation, |member, cancel| {
            ObjectFile::parse_with_cancellation(member, cancel)
        })?;
        Ok(Archive {
            objects: members.into_iter().map(|member| member.parsed).collect(),
        })
    }
}

/// One object member of an archive, with whatever was decoded from it.
#[derive(Debug)]
pub struct ArchiveMember<'a, T> {
    /// The member's name as recorded in the archive.
    pub name: String,
    /// The member's bytes, borrowed from the archive.
    pub data: &'a [u8],
    /// What `collect_members` decoded from `data`.
    pub parsed: T,
}

/// Walk an archive's members in order, decoding each with `parse_member`.
///
/// The member filtering is here and nowhere else, so every view of an archive
/// agrees on which members exist and in what order — which archive member
/// selection depends on, since it extracts the first eligible provider in
/// member order.
fn collect_members<'a, T>(
    data: &'a [u8],
    strict_objects: bool,
    cancellation: &mut impl FnMut() -> bool,
    mut parse_member: impl FnMut(&'a [u8], &mut dyn FnMut() -> bool) -> Result<T, ParseError>,
) -> Result<Vec<ArchiveMember<'a, T>>, ArchiveError> {
    if cancellation() {
        return Err(ArchiveError::Canceled);
    }
    // Check magic
    if data.len() < AR_MAGIC.len() {
        return Err(ArchiveError::TooShort);
    }
    if &data[..AR_MAGIC.len()] != AR_MAGIC {
        return Err(ArchiveError::InvalidMagic);
    }

    let mut members = Vec::new();
    let mut offset = AR_MAGIC.len();

    loop {
        if cancellation() {
            return Err(ArchiveError::Canceled);
        }
        let header_end = checked_offset_add(offset, HEADER_SIZE)?;
        if header_end > data.len() {
            if strict_objects && offset != data.len() {
                return Err(ArchiveError::TooShort);
            }
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
        let size: usize = size_str
            .parse()
            .map_err(|_| ArchiveError::InvalidHeader(format!("invalid size: '{}'", size_str)))?;

        // Header terminator should be "`\n"
        if &header[58..60] != b"`\n" {
            return Err(ArchiveError::InvalidHeader("invalid terminator".into()));
        }

        offset = header_end;

        // Handle BSD long filename format (#1/N where N is the name length)
        // The real filename is embedded at the start of the member data.
        let (actual_name, name_len) = if name.starts_with("#1/") {
            let name_len: usize = name[3..].trim().parse().map_err(|_| {
                ArchiveError::InvalidHeader(format!("invalid BSD name length: '{}'", &name[3..]))
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
        match parse_member(member_data, &mut *cancellation) {
            Ok(parsed) => members.push(ArchiveMember {
                name: actual_name,
                data: member_data,
                parsed,
            }),
            Err(crate::elf::ParseError::Canceled) => return Err(ArchiveError::Canceled),
            Err(source) if strict_objects && looks_like_native_object(member_data) => {
                return Err(ArchiveError::ObjectMemberParse {
                    member: actual_name,
                    source,
                });
            }
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

    Ok(members)
}

/// An archive indexed for member selection: every member's symbols, plus the
/// bytes needed to decode that member in full once it is actually selected.
///
/// Selection asks only which members define which symbols, and that answer
/// never depends on section contents or relocations — so decoding those for the
/// members a link discards is pure waste. The embedded runtime archive is 297
/// members, of which a typical link extracts one (RUE-1845).
///
/// The validation this performs is correspondingly narrower than
/// [`Archive::parse_strict_objects`]: every member's headers and symbol table
/// must parse, but a member whose *section or relocation contents* are
/// malformed is caught when it is linked rather than when the archive is read.
/// A member that is never selected never reaches the linker, so its contents
/// cannot affect the output.
#[derive(Debug)]
pub struct ArchiveIndex<'a> {
    members: Vec<ArchiveMember<'a, ObjectSymbols>>,
}

impl<'a> ArchiveIndex<'a> {
    /// Index an archive, rejecting members that advertise a supported native
    /// object magic but whose headers or symbol table do not parse.
    #[must_use = "parsing returns a Result that must be checked"]
    pub fn parse_strict_objects(data: &'a [u8]) -> Result<Self, ArchiveError> {
        Self::parse_impl(data, true, &mut || false)
    }

    /// Index a strict archive with bounded caller-owned cancellation checks.
    pub fn parse_strict_objects_with_cancellation(
        data: &'a [u8],
        mut cancellation: impl FnMut() -> bool,
    ) -> Result<Self, ArchiveError> {
        Self::parse_impl(data, true, &mut cancellation)
    }

    fn parse_impl(
        data: &'a [u8],
        strict_objects: bool,
        cancellation: &mut impl FnMut() -> bool,
    ) -> Result<Self, ArchiveError> {
        let members = collect_members(data, strict_objects, cancellation, |member, cancel| {
            ObjectFile::parse_symbols_with_cancellation(member, cancel)
        })?;
        Ok(Self { members })
    }

    /// The indexed members, in archive order.
    #[must_use]
    pub fn members(&self) -> &[ArchiveMember<'a, ObjectSymbols>] {
        &self.members
    }

    /// Returns the number of object members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Returns true if the archive contains no object members.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

impl Archive {
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

fn looks_like_native_object(data: &[u8]) -> bool {
    if data.starts_with(b"\x7fELF") {
        return true;
    }
    let Some(magic) = data
        .get(..4)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_le_bytes)
    else {
        return false;
    };
    matches!(magic, 0xfeed_face | 0xcefa_edfe | 0xfeed_facf | 0xcffa_edfe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_target::Target;

    #[test]
    fn strict_archive_parse_observes_cancellation_between_members() {
        let mut checks = 0_usize;
        let result = Archive::parse_strict_objects_with_cancellation(AR_MAGIC, || {
            checks += 1;
            checks == 2
        });

        assert!(matches!(result, Err(ArchiveError::Canceled)));
        assert_eq!(checks, 2);
    }

    #[test]
    fn strict_single_large_member_parse_has_byte_bounded_cancellation() {
        let object = crate::ObjectBuilder::new(Target::X86_64Linux, "large")
            .code(vec![0_u8; 4 * 1024 * 1024])
            .build();
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            "large.o/",
            "0",
            "0",
            "0",
            "644",
            object.len()
        );
        assert_eq!(header.len(), HEADER_SIZE);
        let mut archive = AR_MAGIC.to_vec();
        archive.extend_from_slice(header.as_bytes());
        archive.extend_from_slice(&object);
        if archive.len() % 2 == 1 {
            archive.push(b'\n');
        }

        let mut checkpoints = 0_usize;
        let result = Archive::parse_strict_objects_with_cancellation(&archive, || {
            checkpoints += 1;
            checkpoints == 32
        });

        assert!(matches!(result, Err(ArchiveError::Canceled)));
        assert_eq!(checkpoints, 32);
    }

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
