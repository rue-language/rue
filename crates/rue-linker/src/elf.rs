//! Object file parsing (ELF and Mach-O).
//!
//! Parses relocatable object files to extract:
//! - Sections (code and data)
//! - Symbols (defined and undefined)
//! - Relocations (patches to apply)
//!
//! Supports both ELF64 (Linux) and Mach-O 64-bit (macOS) formats.
//! The format is auto-detected based on magic bytes.

use crate::constants::{
    // Mach-O constants
    ARM64_RELOC_ADDEND,
    ARM64_RELOC_BRANCH26,
    ARM64_RELOC_GOT_LOAD_PAGE21,
    ARM64_RELOC_GOT_LOAD_PAGEOFF12,
    ARM64_RELOC_PAGE21,
    ARM64_RELOC_PAGEOFF12,
    ARM64_RELOC_UNSIGNED,
    E_MACHINE_OFFSET,
    E_SHENTSIZE_OFFSET,
    E_SHNUM_OFFSET,
    E_SHOFF_OFFSET,
    E_SHSTRNDX_OFFSET,
    E_TYPE_OFFSET,
    ELF_MAGIC,
    ELF64_EHDR_SIZE,
    ELF64_RELA_SIZE,
    ELF64_SHDR_SIZE,
    ELF64_SYM_SIZE,
    ELFCLASS64,
    ELFDATA2LSB,
    EM_AARCH64,
    EM_X86_64,
    ET_REL,
    LC_SEGMENT_64,
    LC_SYMTAB,
    MACHO64_HEADER_SIZE,
    MACHO64_NLIST_SIZE,
    MACHO64_RELOC_SIZE,
    MACHO64_SECTION_SIZE,
    MACHO64_SEGMENT_CMD_SIZE,
    MACHO64_SYMTAB_CMD_SIZE,
    MH_MAGIC_64,
    MH_OBJECT,
    N_ABS,
    N_EXT,
    N_SECT,
    N_TYPE,
    N_UNDF,
    R_AARCH64_ABS64,
    R_AARCH64_ADD_ABS_LO12_NC,
    R_AARCH64_ADR_PREL_PG_HI21,
    R_AARCH64_CALL26,
    R_AARCH64_JUMP26,
    R_AARCH64_LDST8_ABS_LO12_NC,
    R_AARCH64_LDST16_ABS_LO12_NC,
    R_AARCH64_LDST32_ABS_LO12_NC,
    R_AARCH64_LDST64_ABS_LO12_NC,
    R_AARCH64_LDST128_ABS_LO12_NC,
    R_X86_64_32,
    R_X86_64_32S,
    R_X86_64_64,
    R_X86_64_GOTPCREL,
    R_X86_64_GOTPCRELX,
    R_X86_64_PC32,
    R_X86_64_PLT32,
    R_X86_64_REX_GOTPCRELX,
    SHN_LORESERVE,
    SHN_UNDEF,
    SHT_NULL,
    SHT_RELA,
    SHT_STRTAB,
    SHT_SYMTAB,
    STB_GLOBAL,
    STB_LOCAL,
    STB_WEAK,
    STT_FILE,
    STT_FUNC,
    STT_NOTYPE,
    STT_OBJECT,
    STT_SECTION,
};
use ahash::AHashMap;
use std::sync::Arc;

/// Helper to read a u16 from a byte slice at a given offset.
/// Panics if offset + 2 > slice.len(), so caller must ensure bounds.
#[inline]
fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Helper to read a u32 from a byte slice at a given offset.
/// Panics if offset + 4 > slice.len(), so caller must ensure bounds.
#[inline]
fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Helper to read a u64 from a byte slice at a given offset.
/// Panics if offset + 8 > slice.len(), so caller must ensure bounds.
#[inline]
fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

/// Helper to read an i64 from a byte slice at a given offset.
/// Panics if offset + 8 > slice.len(), so caller must ensure bounds.
#[inline]
fn read_i64(data: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

/// Helper to read a null-terminated C string from a byte slice.
/// Reads until a null byte or end of slice, returning the string as UTF-8.
#[inline]
fn read_cstring(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).to_string()
}

fn check_parse_cancellation(cancellation: &mut impl FnMut() -> bool) -> Result<(), ParseError> {
    if cancellation() {
        Err(ParseError::Canceled)
    } else {
        Ok(())
    }
}

fn clone_bytes_with_cancellation(
    data: &[u8],
    cancellation: &mut impl FnMut() -> bool,
) -> Result<Vec<u8>, ParseError> {
    let mut bytes = Vec::with_capacity(data.len());
    for chunk in data.chunks(64 * 1024) {
        check_parse_cancellation(cancellation)?;
        bytes.extend_from_slice(chunk);
    }
    Ok(bytes)
}

fn read_cstring_with_cancellation(
    data: &[u8],
    cancellation: &mut impl FnMut() -> bool,
) -> Result<String, ParseError> {
    let mut end = 0;
    for chunk in data.chunks(64 * 1024) {
        check_parse_cancellation(cancellation)?;
        if let Some(relative) = chunk.iter().position(|&byte| byte == 0) {
            end += relative;
            break;
        }
        end += chunk.len();
    }
    let bytes = &data[..end];
    let mut output = String::with_capacity(bytes.len());
    let mut offset = 0_usize;
    while offset < bytes.len() {
        check_parse_cancellation(cancellation)?;
        let mut chunk_end = bytes.len().min(offset.saturating_add(64 * 1024));
        loop {
            match std::str::from_utf8(&bytes[offset..chunk_end]) {
                Ok(valid) => {
                    output.push_str(valid);
                    offset = chunk_end;
                    break;
                }
                Err(error) => {
                    let valid_end = offset + error.valid_up_to();
                    if let Some(error_len) = error.error_len() {
                        // SAFETY: `valid_up_to` identifies the valid UTF-8 prefix.
                        output.push_str(unsafe {
                            std::str::from_utf8_unchecked(&bytes[offset..valid_end])
                        });
                        output.push('\u{FFFD}');
                        offset = valid_end + error_len;
                        break;
                    }
                    if chunk_end == bytes.len() {
                        // SAFETY: `valid_up_to` identifies the valid UTF-8 prefix.
                        output.push_str(unsafe {
                            std::str::from_utf8_unchecked(&bytes[offset..valid_end])
                        });
                        output.push('\u{FFFD}');
                        offset = bytes.len();
                        break;
                    }
                    // Complete a scalar split at the artificial chunk edge.
                    // Rechecking at most a few extra bytes preserves the exact
                    // whole-slice lossy conversion while remaining bounded.
                    chunk_end = bytes.len().min(chunk_end.saturating_add(3));
                }
            }
        }
    }
    Ok(output)
}

fn read_utf8_cstring_with_cancellation(
    data: &[u8],
    offset: usize,
    cancellation: &mut impl FnMut() -> bool,
) -> Result<String, ParseError> {
    if offset > data.len() {
        return Err(ParseError::InvalidStringTable);
    }
    let mut end = offset;
    for chunk in data[offset..].chunks(64 * 1024) {
        check_parse_cancellation(cancellation)?;
        if let Some(relative) = chunk.iter().position(|&byte| byte == 0) {
            end += relative;
            break;
        }
        end += chunk.len();
    }
    let bytes = clone_bytes_with_cancellation(&data[offset..end], cancellation)?;
    let mut validated = 0_usize;
    while validated < bytes.len() {
        check_parse_cancellation(cancellation)?;
        let mut chunk_end = bytes.len().min(validated.saturating_add(64 * 1024));
        loop {
            match std::str::from_utf8(&bytes[validated..chunk_end]) {
                Ok(_) => break,
                Err(error) if error.error_len().is_some() || chunk_end == bytes.len() => {
                    return Err(ParseError::InvalidStringTable);
                }
                Err(_) => {
                    // The chunk ended inside a valid scalar. Extend by at most
                    // the UTF-8 continuation width so validation remains
                    // byte-bounded without changing whole-string semantics.
                    chunk_end = bytes.len().min(chunk_end.saturating_add(3));
                }
            }
        }
        validated = chunk_end;
    }
    check_parse_cancellation(cancellation)?;
    // Every byte range above was validated from a scalar boundary.
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}

/// A parsed ELF64 relocatable object file.
#[derive(Debug)]
pub struct ObjectFile {
    /// All sections in the object file.
    pub sections: Vec<Section>,
    /// All symbols (both defined and undefined).
    pub symbols: Vec<Symbol>,
    /// Section name to index mapping.
    pub section_map: AHashMap<String, usize>,
    /// The machine architecture this object was compiled for. The linker
    /// validates this against the link target — without it, an "aarch64"
    /// binary could silently embed x86 code (RUE-131 item 10, RUE-36).
    pub machine: ElfMachine,
    /// Container format used by this relocatable object.
    pub format: ObjectFormat,
}

/// A compiler-owned link input whose section atoms remain shared with the
/// code-generation query.  Unlike [`ObjectFile`], this representation has no
/// object-container bytes to parse and each atom can be consumed without a
/// transient flattening allocation.
#[derive(Debug, Clone)]
pub struct StructuredObject {
    /// Sections in object-local order.
    pub(crate) sections: Vec<StructuredSection>,
    /// Symbols use the section indices in `sections` and relocation symbol
    /// indices use this vector, just like [`ObjectFile`].
    pub(crate) symbols: Vec<Symbol>,
    /// The machine architecture of this input.
    pub(crate) machine: ElfMachine,
    /// The logical object format selected by the target.
    pub(crate) format: ObjectFormat,
}

/// One section of a [`StructuredObject`].  Atoms are deliberately retained as
/// independent `Arc` byte containers so the linker only copies into its final
/// merged image.
#[derive(Debug, Clone)]
pub(crate) struct StructuredSection {
    pub(crate) name: String,
    pub(crate) atoms: Arc<[Arc<[u8]>]>,
    pub(crate) size: u64,
    pub(crate) flags: SectionFlags,
    pub(crate) relocations: Vec<Relocation>,
    pub(crate) align: u64,
}

/// A relocation in a retained compiler function object. The compiler maps its
/// backend relocation enum to this linker-owned enum before calling the
/// function-object constructor; section and symbol conventions stay here.
#[derive(Debug, Clone)]
pub struct StructuredRelocation {
    pub offset: u64,
    pub symbol: String,
    pub rel_type: RelocationType,
    pub addend: i64,
}

/// Relocatable object container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    Elf,
    MachO,
}

/// A section from an object file.
#[derive(Debug, Clone)]
pub struct Section {
    /// Section name (e.g., ".text.rue_print").
    pub name: String,
    /// Section contents (empty for NOBITS sections like .bss).
    pub data: Vec<u8>,
    /// Section size in memory (may differ from data.len() for NOBITS sections).
    pub size: u64,
    /// Section flags.
    pub flags: SectionFlags,
    /// Relocations that apply to this section.
    pub relocations: Vec<Relocation>,
    /// Alignment requirement.
    pub align: u64,
}

impl StructuredObject {
    /// Expose the retained atom containers for linker-owned consumers without
    /// exposing the object-construction representation itself.
    pub fn section_atoms(&self, index: usize) -> Option<&[Arc<[u8]>]> {
        self.sections
            .get(index)
            .map(|section| section.atoms.as_ref())
    }

    /// Construct the canonical structured object emitted for one retained Rue
    /// function. The linker owns section names, local string-symbol naming,
    /// symbol indices, target format, and Mach-O alignment conventions.
    #[must_use]
    pub fn function(
        target: rue_target::Target,
        defined_symbol: impl Into<String>,
        text_atoms: Arc<[Arc<[u8]>]>,
        rodata_atoms: Arc<[Arc<[u8]>]>,
        relocations: Vec<StructuredRelocation>,
    ) -> Self {
        let machine = match target {
            rue_target::Target::X86_64Linux => ElfMachine::X86_64,
            rue_target::Target::Aarch64Linux | rue_target::Target::Aarch64Macos => {
                ElfMachine::Aarch64
            }
        };
        let format = if target.is_macho() {
            ObjectFormat::MachO
        } else {
            ObjectFormat::Elf
        };
        let text_size = atoms_size(&text_atoms);
        let rodata_size = atoms_size(&rodata_atoms);
        let mut symbols = vec![Symbol {
            name: defined_symbol.into(),
            section_index: Some(0),
            value: 0,
            size: text_size,
            binding: SymbolBinding::Global,
            sym_type: SymbolType::Func,
        }];
        let mut symbol_indices = AHashMap::new();
        let mut rodata_offset = 0_u64;
        for (index, atom) in rodata_atoms.iter().enumerate() {
            if !(target.is_macho() && atom.is_empty()) {
                let name = format!(".rodata.str{index}");
                symbol_indices.insert(name.clone(), symbols.len());
                symbols.push(Symbol {
                    name,
                    section_index: Some(1),
                    value: rodata_offset,
                    size: 0,
                    binding: SymbolBinding::Local,
                    sym_type: SymbolType::None,
                });
            }
            rodata_offset += atom.len() as u64;
        }
        let linker_relocations = relocations
            .into_iter()
            .filter_map(|relocation| {
                if target.is_macho()
                    && relocation.symbol.starts_with(".rodata.str")
                    && relocation
                        .symbol
                        .strip_prefix(".rodata.str")
                        .and_then(|index| index.parse::<usize>().ok())
                        .and_then(|index| rodata_atoms.get(index))
                        .is_some_and(|atom| atom.is_empty())
                {
                    return None;
                }
                let symbol_index = if relocation.symbol == symbols[0].name {
                    0
                } else if let Some(&index) = symbol_indices.get(&relocation.symbol) {
                    index
                } else {
                    let index = symbols.len();
                    symbol_indices.insert(relocation.symbol.clone(), index);
                    symbols.push(Symbol {
                        name: relocation.symbol,
                        section_index: None,
                        value: 0,
                        size: 0,
                        binding: SymbolBinding::Global,
                        sym_type: SymbolType::None,
                    });
                    index
                };
                Some(Relocation {
                    offset: relocation.offset,
                    symbol_index,
                    rel_type: relocation.rel_type,
                    addend: relocation.addend,
                })
            })
            .collect();
        let mut sections = vec![StructuredSection {
            name: ".text".into(),
            atoms: text_atoms,
            size: text_size,
            flags: SectionFlags::ALLOC | SectionFlags::EXEC,
            relocations: linker_relocations,
            align: if target.is_macho() { 4 } else { 16 },
        }];
        let has_rodata = if target.is_macho() {
            rodata_size != 0
        } else {
            !rodata_atoms.is_empty()
        };
        if has_rodata {
            sections.push(StructuredSection {
                name: ".rodata".into(),
                atoms: rodata_atoms,
                size: rodata_size,
                flags: SectionFlags::ALLOC,
                relocations: Vec::new(),
                // ObjectBuilder emits both ELF and Mach-O rodata at 8-byte
                // alignment; structured admission must preserve that layout.
                align: 8,
            });
        }
        Self {
            sections,
            symbols,
            machine,
            format,
        }
    }
}

fn atoms_size(atoms: &[Arc<[u8]>]) -> u64 {
    atoms.iter().map(|atom| atom.len() as u64).sum()
}

/// Section flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SectionFlags(u64);

impl SectionFlags {
    /// Section is writable.
    pub const WRITE: SectionFlags = SectionFlags(0x1);
    /// Section is allocated (loaded into memory).
    pub const ALLOC: SectionFlags = SectionFlags(0x2);
    /// Section is executable.
    pub const EXEC: SectionFlags = SectionFlags(0x4);

    /// Create empty flags.
    #[must_use]
    pub const fn empty() -> Self {
        SectionFlags(0)
    }

    /// Check if flags contain a specific flag.
    #[must_use]
    pub const fn contains(self, other: SectionFlags) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for SectionFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        SectionFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for SectionFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// A symbol from an object file.
#[derive(Debug, Clone)]
pub struct Symbol {
    /// Symbol name.
    pub name: String,
    /// Section index this symbol is defined in (None if undefined).
    pub section_index: Option<usize>,
    /// Offset within the section.
    pub value: u64,
    /// Symbol size.
    pub size: u64,
    /// Symbol binding (local, global, weak).
    pub binding: SymbolBinding,
    /// Symbol type (function, object, etc.).
    pub sym_type: SymbolType,
}

/// Symbol binding type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolBinding {
    Local,
    Global,
    Weak,
}

/// Symbol type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolType {
    None,
    Object,
    Func,
    Section,
    File,
}

/// A relocation entry.
#[derive(Debug, Clone)]
pub struct Relocation {
    /// Offset within the section to patch.
    pub offset: u64,
    /// Symbol index this relocation refers to.
    pub symbol_index: usize,
    /// Relocation type.
    pub rel_type: RelocationType,
    /// Addend value.
    pub addend: i64,
}

/// Machine type for ELF files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfMachine {
    /// x86-64 (EM_X86_64 = 0x3E)
    X86_64,
    /// AArch64 (EM_AARCH64 = 0xB7)
    Aarch64,
}

/// Relocation types we support (x86-64 and AArch64).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationType {
    /// R_X86_64_64: 64-bit absolute address.
    Abs64,
    /// R_X86_64_PC32: 32-bit PC-relative address.
    Pc32,
    /// R_X86_64_PLT32: 32-bit PLT-relative (treated as PC32 for static linking).
    Plt32,
    /// R_X86_64_GOTPCREL: 32-bit PC-relative GOT offset.
    /// For static linking, we relax this to a direct PC-relative reference.
    GotPcRel,
    /// R_X86_64_REX_GOTPCRELX: Relaxable 32-bit PC-relative GOT offset (with REX prefix).
    /// For static linking, we relax this to a direct PC-relative reference.
    RexGotPcRelX,
    /// R_X86_64_GOTPCRELX: Relaxable 32-bit PC-relative GOT offset.
    /// For static linking, we relax this to a direct PC-relative reference.
    GotPcRelX,
    /// R_X86_64_32: 32-bit absolute address.
    Abs32,
    /// R_X86_64_32S: 32-bit signed absolute address.
    Abs32S,
    /// R_AARCH64_JUMP26: AArch64 unconditional branch instruction.
    Jump26,
    /// R_AARCH64_CALL26: AArch64 branch with link instruction.
    Call26,
    /// R_AARCH64_ABS64: AArch64 64-bit absolute address.
    Aarch64Abs64,
    /// R_AARCH64_ADR_PREL_PG_HI21: AArch64 ADRP instruction page address.
    AdrpPage21,
    /// R_AARCH64_ADD_ABS_LO12_NC: AArch64 ADD instruction page offset.
    AddLo12,
    /// R_AARCH64_LDST8_ABS_LO12_NC: 8-bit load/store page offset (imm12 << 0).
    Ldst8Lo12,
    /// R_AARCH64_LDST16_ABS_LO12_NC: 16-bit load/store page offset (imm12 << 1).
    Ldst16Lo12,
    /// R_AARCH64_LDST32_ABS_LO12_NC: 32-bit load/store page offset (imm12 << 2).
    Ldst32Lo12,
    /// R_AARCH64_LDST64_ABS_LO12_NC: 64-bit load/store page offset (imm12 << 3).
    Ldst64Lo12,
    /// R_AARCH64_LDST128_ABS_LO12_NC: 128-bit load/store page offset (imm12 << 4).
    Ldst128Lo12,
    /// ARM64_RELOC_GOT_LOAD_PAGE21 (Mach-O): ADRP of a GOT entry's page.
    /// This linker produces fully static executables with no GOT, so the load
    /// is relaxed to direct addressing (the aarch64 analogue of the x86-64
    /// GotPcRelX relaxation): the ADRP is retargeted at the symbol itself,
    /// and the paired GOT_LOAD_PAGEOFF12 LDR becomes an ADD (RUE-707).
    GotLoadAdrpPage21,
    /// ARM64_RELOC_GOT_LOAD_PAGEOFF12 (Mach-O): LDR of a GOT entry within its
    /// page, relaxed to an ADD of the symbol's low 12 bits (see
    /// [`RelocationType::GotLoadAdrpPage21`]).
    GotLoadPageOff12,
    /// Unknown relocation type.
    Unknown(u32),
}

impl RelocationType {
    fn from_elf(r_type: u32, machine: ElfMachine) -> Self {
        match machine {
            ElfMachine::X86_64 => match r_type {
                R_X86_64_64 => RelocationType::Abs64,
                R_X86_64_PC32 => RelocationType::Pc32,
                R_X86_64_PLT32 => RelocationType::Plt32,
                R_X86_64_GOTPCREL => RelocationType::GotPcRel,
                R_X86_64_32 => RelocationType::Abs32,
                R_X86_64_32S => RelocationType::Abs32S,
                R_X86_64_GOTPCRELX => RelocationType::GotPcRelX,
                R_X86_64_REX_GOTPCRELX => RelocationType::RexGotPcRelX,
                _ => RelocationType::Unknown(r_type),
            },
            ElfMachine::Aarch64 => match r_type {
                R_AARCH64_ABS64 => RelocationType::Aarch64Abs64,
                R_AARCH64_ADR_PREL_PG_HI21 => RelocationType::AdrpPage21,
                R_AARCH64_ADD_ABS_LO12_NC => RelocationType::AddLo12,
                R_AARCH64_LDST8_ABS_LO12_NC => RelocationType::Ldst8Lo12,
                R_AARCH64_LDST16_ABS_LO12_NC => RelocationType::Ldst16Lo12,
                R_AARCH64_LDST32_ABS_LO12_NC => RelocationType::Ldst32Lo12,
                R_AARCH64_LDST64_ABS_LO12_NC => RelocationType::Ldst64Lo12,
                R_AARCH64_LDST128_ABS_LO12_NC => RelocationType::Ldst128Lo12,
                R_AARCH64_JUMP26 => RelocationType::Jump26,
                R_AARCH64_CALL26 => RelocationType::Call26,
                _ => RelocationType::Unknown(r_type),
            },
        }
    }
}

/// Error type for object file parsing.
#[derive(Debug)]
pub enum ParseError {
    /// Cooperative cancellation was requested by the caller.
    Canceled,
    /// File is too short.
    TooShort,
    /// Invalid ELF magic number.
    InvalidMagic,
    /// Not a 64-bit ELF file.
    Not64Bit,
    /// Not a little-endian ELF file.
    NotLittleEndian,
    /// Not a relocatable object file.
    NotRelocatable,
    /// Unsupported machine architecture.
    UnsupportedMachine(u16),
    /// Unsupported Mach-O CPU architecture.
    UnsupportedMachOCpu(u32),
    /// Invalid section header.
    InvalidSection(String),
    /// Invalid symbol table.
    InvalidSymbol(String),
    /// Invalid string table.
    InvalidStringTable,
    /// Invalid section header string table index.
    InvalidShstrndx,
    /// Section data out of bounds.
    SectionOutOfBounds(String),
    /// Relocation data out of bounds.
    RelocationOutOfBounds,
    /// Unknown object file format (not ELF or Mach-O).
    UnknownFormat,
    /// Feature not yet implemented.
    NotImplemented(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Canceled => write!(f, "object parsing canceled"),
            ParseError::TooShort => write!(f, "file is too short to be a valid object file"),
            ParseError::InvalidMagic => write!(f, "invalid ELF magic number"),
            ParseError::Not64Bit => write!(f, "not a 64-bit ELF file"),
            ParseError::NotLittleEndian => write!(f, "not a little-endian ELF file"),
            ParseError::NotRelocatable => write!(f, "not a relocatable object file"),
            ParseError::UnsupportedMachine(m) => {
                write!(f, "unsupported ELF machine type: 0x{:x}", m)
            }
            ParseError::UnsupportedMachOCpu(cpu) => {
                write!(f, "unsupported Mach-O CPU type: 0x{:x}", cpu)
            }
            ParseError::InvalidSection(s) => write!(f, "invalid section: {}", s),
            ParseError::InvalidSymbol(s) => write!(f, "invalid symbol: {}", s),
            ParseError::InvalidStringTable => write!(f, "invalid string table"),
            ParseError::InvalidShstrndx => write!(f, "invalid section header string table index"),
            ParseError::SectionOutOfBounds(s) => write!(f, "section data out of bounds: {}", s),
            ParseError::RelocationOutOfBounds => write!(f, "relocation data out of bounds"),
            ParseError::UnknownFormat => {
                write!(f, "unknown object file format (not ELF or Mach-O)")
            }
            ParseError::NotImplemented(feature) => write!(f, "{} not yet implemented", feature),
        }
    }
}

impl std::error::Error for ParseError {}

impl ObjectFile {
    /// Parse a relocatable object file (ELF or Mach-O).
    ///
    /// Automatically detects the format based on magic bytes and dispatches
    /// to the appropriate parser.
    #[must_use = "parsing returns a Result that must be checked"]
    pub fn parse(data: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with_cancellation(data, || false)
    }

    /// Parse an object with bounded caller-owned cancellation checkpoints.
    pub fn parse_with_cancellation(
        data: &[u8],
        mut cancellation: impl FnMut() -> bool,
    ) -> Result<Self, ParseError> {
        check_parse_cancellation(&mut cancellation)?;
        // Need at least 4 bytes to check magic
        if data.len() < 4 {
            return Err(ParseError::TooShort);
        }

        // Dispatch based on magic bytes
        if data[0..4] == ELF_MAGIC {
            Self::parse_elf(data, &mut cancellation)
        } else if data.len() >= 4 && read_u32(data, 0) == MH_MAGIC_64 {
            Self::parse_macho(data, &mut cancellation)
        } else {
            Err(ParseError::UnknownFormat)
        }
    }

    /// Parse a Mach-O 64-bit relocatable object file.
    ///
    /// Extracts sections, symbols, and relocations from a Mach-O object file.
    fn parse_macho(
        data: &[u8],
        cancellation: &mut impl FnMut() -> bool,
    ) -> Result<Self, ParseError> {
        // Minimum size check for Mach-O header
        if data.len() < MACHO64_HEADER_SIZE {
            return Err(ParseError::TooShort);
        }

        // Verify magic (already checked in parse(), but be defensive)
        let magic = read_u32(data, 0);
        if magic != MH_MAGIC_64 {
            return Err(ParseError::InvalidMagic);
        }
        let cpu = read_u32(data, 4);
        if cpu != crate::constants::CPU_TYPE_ARM64 {
            return Err(ParseError::UnsupportedMachOCpu(cpu));
        }

        // Parse header
        let filetype = read_u32(data, 12);
        if filetype != MH_OBJECT {
            return Err(ParseError::NotRelocatable);
        }

        let ncmds = read_u32(data, 16) as usize;
        let sizeofcmds = read_u32(data, 20) as usize;

        // Verify load commands fit
        if data.len() < MACHO64_HEADER_SIZE + sizeofcmds {
            return Err(ParseError::TooShort);
        }

        // Parse load commands to find segments and symbol table
        let mut sections = Vec::new();
        let mut section_map = AHashMap::new();
        let mut symtab_offset = 0usize;
        let mut symtab_count = 0usize;
        let mut strtab_offset = 0usize;
        let mut strtab_size = 0usize;

        // Track section info for relocations (offset, nreloc, reloff)
        let mut section_reloc_info: Vec<(usize, usize, usize)> = Vec::new();

        // Each section's address within the object's VM layout. Mach-O symbol
        // n_value fields and non-extern UNSIGNED patch-site contents are
        // addresses in this layout, NOT section-relative offsets — we need the
        // section addr to convert them.
        let mut section_addrs: Vec<u64> = Vec::new();

        let mut cmd_offset = MACHO64_HEADER_SIZE;
        for _ in 0..ncmds {
            check_parse_cancellation(cancellation)?;
            if cmd_offset + 8 > data.len() {
                return Err(ParseError::TooShort);
            }

            let cmd = read_u32(data, cmd_offset);
            let cmdsize = read_u32(data, cmd_offset + 4) as usize;

            if cmd_offset + cmdsize > data.len() {
                return Err(ParseError::TooShort);
            }

            match cmd {
                LC_SEGMENT_64 => {
                    // Parse segment_command_64
                    if cmdsize < MACHO64_SEGMENT_CMD_SIZE {
                        return Err(ParseError::InvalidSection("segment too small".into()));
                    }

                    let nsects = read_u32(data, cmd_offset + 64) as usize;

                    // Parse sections in this segment
                    let mut sect_offset = cmd_offset + MACHO64_SEGMENT_CMD_SIZE;
                    for _ in 0..nsects {
                        check_parse_cancellation(cancellation)?;
                        if sect_offset + MACHO64_SECTION_SIZE > data.len() {
                            return Err(ParseError::TooShort);
                        }

                        // sectname: 16 bytes at offset 0
                        let sectname = read_cstring(&data[sect_offset..sect_offset + 16]);
                        // segname: 16 bytes at offset 16
                        let segname = read_cstring(&data[sect_offset + 16..sect_offset + 32]);
                        let full_name = format!("{},{}", segname, sectname);

                        // addr: u64 at offset 32
                        let addr = read_u64(data, sect_offset + 32);
                        // size: u64 at offset 40
                        let size = read_u64(data, sect_offset + 40);
                        // offset: u32 at offset 48
                        let offset = read_u32(data, sect_offset + 48) as usize;
                        // align: u32 at offset 52. Mach-O stores alignment as a
                        // power-of-2 exponent; `1 << exp` is UB / panics for an
                        // exponent >= 64 (a malformed or oversized field). Use a
                        // checked shift and reject out-of-range values with a
                        // clean error, mirroring the ELF path's checked
                        // arithmetic (RUE-334).
                        let align_exp = read_u32(data, sect_offset + 52);
                        let Some(align) = 1u64.checked_shl(align_exp) else {
                            return Err(ParseError::InvalidSection(format!(
                                "section {} has out-of-range alignment exponent {}",
                                full_name, align_exp
                            )));
                        };
                        // reloff: u32 at offset 56
                        let reloff = read_u32(data, sect_offset + 56) as usize;
                        // nreloc: u32 at offset 60
                        let nreloc = read_u32(data, sect_offset + 60) as usize;
                        // flags: u32 at offset 64
                        let flags = read_u32(data, sect_offset + 64);

                        // Determine section flags.
                        //
                        // ALLOC means "part of the loaded image". Mach-O has no
                        // per-section ALLOC bit, so it is derived: everything is
                        // loaded except debug sections, which carry S_ATTR_DEBUG
                        // and live in the __DWARF segment. The distinction is
                        // load-bearing — the linker refuses to silently drop a
                        // relocation into an ALLOC section it cannot place, and
                        // marking DWARF as ALLOC would turn that safety net into
                        // spurious link failures (RUE-1647).
                        let mut section_flags = SectionFlags::empty();
                        if flags & crate::constants::S_ATTR_DEBUG == 0 && segname != "__DWARF" {
                            section_flags |= SectionFlags::ALLOC;
                        }
                        if flags & crate::constants::S_ATTR_PURE_INSTRUCTIONS != 0 {
                            section_flags |= SectionFlags::EXEC;
                        }
                        // Writability is a property of the segment. __DATA is
                        // writable for the life of the process; __DATA_CONST is
                        // not (it is read-only once fixups are applied, and this
                        // linker applies them all statically), so it must not
                        // pick up WRITE from a prefix match here.
                        if segname == "__DATA" {
                            section_flags |= SectionFlags::WRITE;
                        }

                        // Read section data. Compute the end offset with a
                        // checked add so a malformed offset/size near usize::MAX
                        // is rejected instead of wrapping (RUE-334), mirroring
                        // the ELF path's checked_add bounds validation.
                        let section_data = if size > 0 && offset > 0 {
                            let end = offset
                                .checked_add(size as usize)
                                .ok_or_else(|| ParseError::SectionOutOfBounds(full_name.clone()))?;
                            if end > data.len() {
                                return Err(ParseError::SectionOutOfBounds(full_name.clone()));
                            }
                            clone_bytes_with_cancellation(&data[offset..end], cancellation)?
                        } else {
                            Vec::new()
                        };

                        let section_index = sections.len();
                        section_reloc_info.push((section_index, nreloc, reloff));
                        section_addrs.push(addr);
                        section_map.insert(full_name.clone(), section_index);

                        sections.push(Section {
                            name: full_name,
                            data: section_data,
                            size,
                            flags: section_flags,
                            relocations: Vec::new(),
                            align,
                        });

                        sect_offset += MACHO64_SECTION_SIZE;
                    }
                }
                LC_SYMTAB => {
                    // symtab_command
                    // symoff: u32 at offset 8
                    // nsyms: u32 at offset 12
                    // stroff: u32 at offset 16
                    // strsize: u32 at offset 20
                    //
                    // The loop only established that `cmd_offset + cmdsize` is
                    // inside the file, so a command declaring a short cmdsize
                    // would let these four reads run past the buffer and panic.
                    // A malformed object must be an Err, never a panic
                    // (RUE-1645).
                    if cmdsize < MACHO64_SYMTAB_CMD_SIZE {
                        return Err(ParseError::InvalidSymbol(format!(
                            "LC_SYMTAB load command declares cmdsize {cmdsize}, \
                             but the command is {MACHO64_SYMTAB_CMD_SIZE} bytes"
                        )));
                    }
                    symtab_offset = read_u32(data, cmd_offset + 8) as usize;
                    symtab_count = read_u32(data, cmd_offset + 12) as usize;
                    strtab_offset = read_u32(data, cmd_offset + 16) as usize;
                    strtab_size = read_u32(data, cmd_offset + 20) as usize;
                }
                _ => {
                    // Skip other load commands (LC_BUILD_VERSION, LC_DYSYMTAB, etc.)
                }
            }

            cmd_offset += cmdsize;
        }

        // Parse symbols
        let mut symbols = Vec::new();
        if symtab_count > 0 {
            // Verify bounds. Compute the table ends with checked mul/add so a
            // malformed count/offset can't overflow and wrap past the length
            // check (RUE-334), mirroring the ELF path's checked arithmetic.
            let symtab_end = symtab_count
                .checked_mul(MACHO64_NLIST_SIZE)
                .and_then(|n| symtab_offset.checked_add(n))
                .ok_or_else(|| ParseError::InvalidSymbol("symbol table out of bounds".into()))?;
            let strtab_end = strtab_offset
                .checked_add(strtab_size)
                .ok_or_else(|| ParseError::InvalidSymbol("string table out of bounds".into()))?;
            if symtab_end > data.len() || strtab_end > data.len() {
                return Err(ParseError::InvalidSymbol(
                    "symbol table out of bounds".into(),
                ));
            }

            let strtab = &data[strtab_offset..strtab_end];

            for i in 0..symtab_count {
                check_parse_cancellation(cancellation)?;
                let sym_offset = symtab_offset + i * MACHO64_NLIST_SIZE;

                // nlist_64 structure:
                // n_strx: u32 at offset 0
                // n_type: u8 at offset 4
                // n_sect: u8 at offset 5
                // n_desc: u16 at offset 6
                // n_value: u64 at offset 8

                let n_strx = read_u32(data, sym_offset) as usize;
                let n_type = data[sym_offset + 4];
                let n_sect = data[sym_offset + 5];
                let n_value = read_u64(data, sym_offset + 8);

                // Read symbol name from string table
                let mut name = if n_strx < strtab.len() {
                    read_cstring_with_cancellation(&strtab[n_strx..], cancellation)?
                } else {
                    String::new()
                };

                // Undo the single leading underscore that Mach-O emission adds to
                // every symbol. Emission prepends EXACTLY ONE `_` regardless of the
                // original name, so the exact inverse strips EXACTLY ONE. This
                // round-trips for any number of leading underscores (RUE-919):
                // - "main"          -> emitted "_main"          -> "main"
                // - "_foo"          -> emitted "__foo"          -> "_foo"
                // - "__foo"         -> emitted "___foo"         -> "__foo"
                // - ".rodata.str0"  -> emitted "_.rodata.str0"  -> ".rodata.str0"
                // The previous "preserve double underscore" logic was off by one and
                // collapsed "_foo" and "__foo" onto the same "__foo" symbol.
                name = crate::util::strip_macho_underscore(&name).to_string();

                // Determine binding (external or local)
                // N_PEXT (0x10) makes a symbol private even if N_EXT is set
                // Private external symbols should be treated as local to avoid duplicate symbol errors
                let binding = if n_type & N_EXT != 0 && n_type & 0x10 == 0 {
                    // External but not private -> Global
                    SymbolBinding::Global
                } else {
                    // Local or private external -> Local
                    SymbolBinding::Local
                };

                // Determine if symbol is defined (has a section) or undefined
                let sym_type_bits = n_type & N_TYPE;
                let section_index = if sym_type_bits == N_SECT && n_sect > 0 {
                    // n_sect is 1-indexed
                    Some((n_sect - 1) as usize)
                } else if sym_type_bits == N_UNDF {
                    None // Undefined symbol
                } else if sym_type_bits == N_ABS {
                    None // Absolute symbol (no section)
                } else {
                    None
                };

                // Determine symbol type based on section flags
                let sym_type = if section_index.is_some() {
                    // Check if it's in a code section
                    if let Some(idx) = section_index {
                        if idx < sections.len() && sections[idx].flags.contains(SectionFlags::EXEC)
                        {
                            SymbolType::Func
                        } else {
                            SymbolType::Object
                        }
                    } else {
                        SymbolType::None
                    }
                } else {
                    SymbolType::None
                };

                // Mach-O n_value for defined symbols is an address within the
                // object's VM layout, not a section-relative offset (unlike
                // ELF st_value in ET_REL files). Convert to section-relative
                // by subtracting the owning section's addr, since the linker
                // computes final addresses as merged-section-base + value.
                // (Sections emitted at addr 0 — like our own __text — are
                // unaffected.) A value below its section start is malformed;
                // retaining that raw address as a section-relative value lets
                // it escape into a later merged section.
                let value = match section_index {
                    Some(idx) => {
                        let sect_addr = section_addrs.get(idx).copied().ok_or_else(|| {
                            ParseError::InvalidSymbol(format!(
                                "symbol '{}' references invalid section {}",
                                name, idx
                            ))
                        })?;
                        n_value.checked_sub(sect_addr).ok_or_else(|| {
                            ParseError::InvalidSymbol(format!(
                                "symbol '{}' value 0x{:x} is below section {} address 0x{:x}",
                                name, n_value, idx, sect_addr
                            ))
                        })?
                    }
                    None => n_value,
                };

                symbols.push(Symbol {
                    name,
                    section_index,
                    value,
                    size: 0, // Mach-O doesn't store symbol size
                    binding,
                    sym_type,
                });
            }
        }

        // The anchor a non-extern relocation resolves to: the first named
        // symbol at offset 0 of each section. Precomputed once so each
        // relocation probes a map instead of rescanning the whole symbol
        // table — that scan was O(nreloc × nsyms) per object, and
        // rustc-produced Mach-O objects use non-extern relocations heavily
        // (RUE-1665). Synthesized anchors register here as they are minted.
        let mut section_anchors: AHashMap<usize, usize> = AHashMap::new();
        for (index, symbol) in symbols.iter().enumerate() {
            if let Some(section) = symbol.section_index
                && symbol.value == 0
                && !symbol.name.is_empty()
            {
                section_anchors.entry(section).or_insert(index);
            }
        }

        // Parse relocations for each section
        for (section_index, nreloc, reloff) in section_reloc_info {
            check_parse_cancellation(cancellation)?;
            if nreloc == 0 {
                continue;
            }

            // Checked mul/add so a malformed nreloc/reloff can't overflow past
            // the length check (RUE-334), mirroring the ELF reloc bounds guard.
            let reloc_end = nreloc
                .checked_mul(MACHO64_RELOC_SIZE)
                .and_then(|n| reloff.checked_add(n))
                .ok_or(ParseError::RelocationOutOfBounds)?;
            if reloc_end > data.len() {
                return Err(ParseError::RelocationOutOfBounds);
            }

            // Mach-O ARM64 relocation_info has no addend field. Addends come
            // from two places (RUE-131 item 5b):
            // - an ARM64_RELOC_ADDEND entry immediately PRECEDING the
            //   relocation it modifies (used for BRANCH26/PAGE21/PAGEOFF12),
            // - the bytes at the patch site for ARM64_RELOC_UNSIGNED
            //   ("implicit" / embedded addend).
            let mut pending_addend: i64 = 0;

            for i in 0..nreloc {
                check_parse_cancellation(cancellation)?;
                let rel_offset = reloff + i * MACHO64_RELOC_SIZE;

                // relocation_info structure:
                // r_address: i32 at offset 0 (offset in section)
                // r_info: u32 at offset 4
                //   bits 0-23: r_symbolnum
                //   bit 24: r_pcrel
                //   bits 25-26: r_length
                //   bit 27: r_extern
                //   bits 28-31: r_type

                let r_address = read_u32(data, rel_offset) as u64;
                let r_info = read_u32(data, rel_offset + 4);

                let r_symbolnum = r_info & 0x00FFFFFF;
                let _r_pcrel = (r_info >> 24) & 1;
                let r_length = (r_info >> 25) & 3;
                let r_extern = (r_info >> 27) & 1;
                let r_type = (r_info >> 28) & 0xF;

                // ARM64_RELOC_ADDEND is not a relocation itself: its
                // r_symbolnum field is a signed 24-bit addend that applies to
                // the NEXT relocation entry.
                if r_type == ARM64_RELOC_ADDEND {
                    pending_addend = (((r_symbolnum << 8) as i32) >> 8) as i64;
                    continue;
                }

                let symbol_index = if r_extern == 1 {
                    // External relocation: r_symbolnum is a symbol index
                    let idx = r_symbolnum as usize;
                    if idx >= symbols.len() {
                        return Err(ParseError::InvalidSymbol(format!(
                            "relocation references invalid symbol index {}",
                            idx
                        )));
                    }
                    idx
                } else {
                    // Non-extern relocation: r_symbolnum is a 1-indexed
                    // section number; the target is an address inside that
                    // section. We resolve it to a symbol at the section start
                    // (offset folded into the addend below).
                    // r_symbolnum == 0 is invalid for a non-extern relocation;
                    // checked subtraction prevents malformed input from
                    // wrapping into an out-of-bounds index. (RUE-131 item 6)
                    let Some(section_number) = r_symbolnum.checked_sub(1) else {
                        return Err(ParseError::InvalidSymbol(format!(
                            "non-extern relocation at 0x{:x} has r_symbolnum 0",
                            r_address
                        )));
                    };
                    let target_section = section_number as usize;
                    if target_section >= sections.len() {
                        return Err(ParseError::InvalidSymbol(format!(
                            "non-extern relocation at 0x{:x} references invalid section {}",
                            r_address, target_section
                        )));
                    }
                    // Reuse any named symbol at offset 0 of the section;
                    // otherwise synthesize a local section anchor. (This used
                    // to require a GLOBAL symbol at offset 0 and error out
                    // otherwise — real objects routinely have only local
                    // symbols, or none at all, at a section start.
                    // RUE-131 item 5c)
                    *section_anchors.entry(target_section).or_insert_with(|| {
                        symbols.push(Symbol {
                            name: sections[target_section].name.clone(),
                            section_index: Some(target_section),
                            value: 0,
                            size: 0,
                            binding: SymbolBinding::Local,
                            sym_type: SymbolType::Section,
                        });
                        symbols.len() - 1
                    })
                };

                // Convert Mach-O relocation type to our type
                let rel_type = match r_type {
                    ARM64_RELOC_UNSIGNED => RelocationType::Aarch64Abs64,
                    ARM64_RELOC_BRANCH26 => RelocationType::Call26, // Could be Jump26, but works either way
                    ARM64_RELOC_PAGE21 => RelocationType::AdrpPage21,
                    ARM64_RELOC_PAGEOFF12 => RelocationType::AddLo12,
                    ARM64_RELOC_GOT_LOAD_PAGE21 => RelocationType::GotLoadAdrpPage21,
                    ARM64_RELOC_GOT_LOAD_PAGEOFF12 => RelocationType::GotLoadPageOff12,
                    _ => RelocationType::Unknown(r_type),
                };

                // ARM64_RELOC_UNSIGNED carries its addend embedded in the
                // bytes at the patch site (4 or 8 bytes per r_length). For a
                // non-extern UNSIGNED the stored value is the target's address
                // in the object's VM layout; convert it to an offset from the
                // section-start symbol we resolved to above.
                let mut addend = pending_addend;
                pending_addend = 0;
                if r_type == ARM64_RELOC_UNSIGNED {
                    let site = r_address as usize;
                    let sec_data = &sections[section_index].data;
                    let embedded = match r_length {
                        2 => {
                            if site + 4 > sec_data.len() {
                                return Err(ParseError::RelocationOutOfBounds);
                            }
                            read_u32(sec_data, site) as i32 as i64
                        }
                        3 => {
                            if site + 8 > sec_data.len() {
                                return Err(ParseError::RelocationOutOfBounds);
                            }
                            read_i64(sec_data, site)
                        }
                        _ => {
                            return Err(ParseError::InvalidSymbol(format!(
                                "UNSIGNED relocation at 0x{:x} has unsupported length {}",
                                r_address, r_length
                            )));
                        }
                    };
                    addend += embedded;
                    if r_extern == 0 {
                        // Stored value is target address in object VM layout;
                        // re-base it onto the section anchor.
                        let target_section = (r_symbolnum - 1) as usize;
                        addend -= section_addrs.get(target_section).copied().unwrap_or(0) as i64;
                    }
                }

                sections[section_index].relocations.push(Relocation {
                    offset: r_address,
                    symbol_index,
                    rel_type,
                    addend,
                });
            }
        }

        Ok(ObjectFile {
            sections,
            symbols,
            section_map,
            // The Mach-O parser only accepts CPU_TYPE_ARM64 objects.
            machine: ElfMachine::Aarch64,
            format: ObjectFormat::MachO,
        })
    }

    /// Parse an ELF64 relocatable object file.
    fn parse_elf(data: &[u8], cancellation: &mut impl FnMut() -> bool) -> Result<Self, ParseError> {
        check_parse_cancellation(cancellation)?;
        // Check minimum size for ELF header
        if data.len() < ELF64_EHDR_SIZE {
            return Err(ParseError::TooShort);
        }

        // We already verified ELF magic in parse(), but verify again for safety
        if data[0..4] != ELF_MAGIC {
            return Err(ParseError::InvalidMagic);
        }

        // Check 64-bit
        if data[4] != ELFCLASS64 {
            return Err(ParseError::Not64Bit);
        }

        // Check little-endian
        if data[5] != ELFDATA2LSB {
            return Err(ParseError::NotLittleEndian);
        }

        // Check relocatable file (e_type == ET_REL)
        let e_type = u16::from_le_bytes([data[E_TYPE_OFFSET], data[E_TYPE_OFFSET + 1]]);
        if e_type != ET_REL {
            return Err(ParseError::NotRelocatable);
        }

        // Check machine type (x86-64 or aarch64)
        let e_machine = u16::from_le_bytes([data[E_MACHINE_OFFSET], data[E_MACHINE_OFFSET + 1]]);
        let machine = match e_machine {
            EM_X86_64 => ElfMachine::X86_64,
            EM_AARCH64 => ElfMachine::Aarch64,
            _ => return Err(ParseError::UnsupportedMachine(e_machine)),
        };

        // Parse header fields - safe because we checked data.len() >= ELF64_EHDR_SIZE above
        let e_shoff = read_u64(data, E_SHOFF_OFFSET) as usize;
        let e_shentsize = read_u16(data, E_SHENTSIZE_OFFSET) as usize;
        let e_shnum = read_u16(data, E_SHNUM_OFFSET) as usize;
        let e_shstrndx = read_u16(data, E_SHSTRNDX_OFFSET) as usize;

        // ELF64 section headers are 64 bytes
        if e_shentsize < ELF64_SHDR_SIZE && e_shnum > 0 {
            return Err(ParseError::InvalidSection(
                "section header size too small".into(),
            ));
        }

        // Parse section headers
        // A truncated table may claim the maximum u16 section count, so cap
        // reservations by the number of complete headers present in `data`.
        // Valid tables still reserve their exact declared count.
        let section_capacity = if e_shentsize >= ELF64_SHDR_SIZE {
            e_shnum.min(data.len().saturating_sub(e_shoff) / e_shentsize)
        } else {
            0
        };
        let mut sections = Vec::with_capacity(section_capacity);
        let mut section_map = AHashMap::with_capacity(section_capacity);
        let mut symtab_idx = None;
        let mut strtab_idx = None;

        // First pass: collect section info
        struct RawSection {
            name_offset: u32,
            sh_type: u32,
            flags: u64,
            offset: u64,
            size: u64,
            info: u32,
            align: u64,
            entsize: u64,
        }

        let mut raw_sections = Vec::with_capacity(section_capacity);

        for i in 0..e_shnum {
            check_parse_cancellation(cancellation)?;
            let sh_offset = e_shoff + i * e_shentsize;
            if sh_offset + e_shentsize > data.len() {
                return Err(ParseError::InvalidSection(
                    "section header out of bounds".into(),
                ));
            }

            let sh = &data[sh_offset..sh_offset + e_shentsize];
            // Bounds are guaranteed by the check above (sh_offset + e_shentsize <= data.len())
            // and e_shentsize >= 64 for valid ELF64 section headers
            let name_offset = read_u32(sh, 0);
            let sh_type = read_u32(sh, 4);
            let flags = read_u64(sh, 8);
            let _addr = read_u64(sh, 16);
            let offset = read_u64(sh, 24);
            let size = read_u64(sh, 32);
            let link = read_u32(sh, 40);
            let info = read_u32(sh, 44);
            let align = read_u64(sh, 48);
            let entsize = read_u64(sh, 56);

            if sh_type == SHT_SYMTAB {
                symtab_idx = Some(i);
                strtab_idx = Some(link as usize);
            }

            raw_sections.push(RawSection {
                name_offset,
                sh_type,
                flags,
                offset,
                size,
                info,
                align,
                entsize,
            });
        }

        // Get section name string table
        if e_shstrndx >= raw_sections.len() {
            return Err(ParseError::InvalidShstrndx);
        }
        let shstrtab = &raw_sections[e_shstrndx];
        let shstrtab_end = shstrtab
            .offset
            .checked_add(shstrtab.size)
            .ok_or_else(|| ParseError::SectionOutOfBounds("shstrtab overflow".into()))?;
        if shstrtab_end as usize > data.len() {
            return Err(ParseError::SectionOutOfBounds("shstrtab".into()));
        }
        let shstrtab_data = &data[shstrtab.offset as usize..shstrtab_end as usize];

        // Second pass: create sections with names
        for (i, raw) in raw_sections.iter().enumerate() {
            check_parse_cancellation(cancellation)?;
            let name = read_utf8_cstring_with_cancellation(
                shstrtab_data,
                raw.name_offset as usize,
                cancellation,
            )?;

            // Skip null section, symtab, strtab, rela sections (we'll handle them separately)
            if raw.sh_type == SHT_NULL
                || raw.sh_type == SHT_SYMTAB
                || raw.sh_type == SHT_STRTAB
                || raw.sh_type == SHT_RELA
            {
                sections.push(Section {
                    name: name.clone(),
                    data: Vec::new(),
                    size: 0,
                    flags: SectionFlags::empty(),
                    relocations: Vec::new(),
                    align: raw.align,
                });
                if !name.is_empty() {
                    section_map.insert(name, i);
                }
                continue;
            }

            // For NOBITS sections (like .bss), don't read data from file.
            // The size is tracked in raw.size but there's no file content.
            let section_data = if raw.sh_type == crate::constants::SHT_NOBITS {
                Vec::new()
            } else if raw.size > 0 && raw.offset > 0 {
                let section_end = raw
                    .offset
                    .checked_add(raw.size)
                    .ok_or_else(|| ParseError::SectionOutOfBounds(format!("{} overflow", name)))?;
                if section_end as usize > data.len() {
                    return Err(ParseError::SectionOutOfBounds(name.clone()));
                }
                clone_bytes_with_cancellation(
                    &data[raw.offset as usize..section_end as usize],
                    cancellation,
                )?
            } else {
                Vec::new()
            };

            let mut flags = SectionFlags::empty();
            if raw.flags & crate::constants::SHF_WRITE != 0 {
                flags |= SectionFlags::WRITE;
            }
            if raw.flags & crate::constants::SHF_ALLOC != 0 {
                flags |= SectionFlags::ALLOC;
            }
            if raw.flags & crate::constants::SHF_EXECINSTR != 0 {
                flags |= SectionFlags::EXEC;
            }

            sections.push(Section {
                name: name.clone(),
                data: section_data,
                size: raw.size,
                flags,
                relocations: Vec::new(),
                align: raw.align,
            });

            if !name.is_empty() {
                section_map.insert(name, i);
            }
        }

        // Parse symbol table
        let mut symbols = Vec::new();

        if let (Some(symtab_i), Some(strtab_i)) = (symtab_idx, strtab_idx) {
            // `strtab_i` is the `.symtab` header's `sh_link` field verbatim, so
            // a malformed object can name a section index the file does not
            // have. Reject it instead of indexing `raw_sections` and panicking
            // (RUE-1645).
            if strtab_i >= raw_sections.len() {
                return Err(ParseError::InvalidSymbol(format!(
                    "symbol table sh_link {} is out of bounds (have {} sections)",
                    strtab_i,
                    raw_sections.len()
                )));
            }
            let symtab = &raw_sections[symtab_i];
            let strtab = &raw_sections[strtab_i];

            // Validate strtab bounds
            let strtab_end = strtab
                .offset
                .checked_add(strtab.size)
                .ok_or_else(|| ParseError::InvalidSymbol("strtab overflow".into()))?;
            if strtab_end as usize > data.len() {
                return Err(ParseError::InvalidSymbol("strtab out of bounds".into()));
            }
            let strtab_data = &data[strtab.offset as usize..strtab_end as usize];

            // Validate symtab bounds
            let symtab_end = symtab
                .offset
                .checked_add(symtab.size)
                .ok_or_else(|| ParseError::InvalidSymbol("symtab overflow".into()))?;
            if symtab_end as usize > data.len() {
                return Err(ParseError::InvalidSymbol("symtab out of bounds".into()));
            }
            let symtab_data = &data[symtab.offset as usize..symtab_end as usize];

            if symtab.entsize == 0 {
                return Err(ParseError::InvalidSymbol("zero entsize".into()));
            }
            let sym_count = symtab.size / symtab.entsize;
            // Avoid speculative allocation for an undersized entry width: the
            // existing per-entry bounds check must reject that malformed table.
            if symtab.entsize >= ELF64_SYM_SIZE as u64 {
                if let Ok(capacity) = usize::try_from(sym_count) {
                    let _ = symbols.try_reserve(capacity);
                }
            }
            for i in 0..sym_count {
                check_parse_cancellation(cancellation)?;
                let sym_offset = (i * symtab.entsize) as usize;
                if sym_offset + ELF64_SYM_SIZE > symtab_data.len() {
                    return Err(ParseError::InvalidSymbol(
                        "symbol entry out of bounds".into(),
                    ));
                }
                let sym = &symtab_data[sym_offset..sym_offset + ELF64_SYM_SIZE];

                // Bounds guaranteed by check above (sym_offset + 24 <= symtab_data.len())
                let st_name = read_u32(sym, 0);
                let st_info = sym[4];
                let _st_other = sym[5];
                let st_shndx = read_u16(sym, 6);
                let st_value = read_u64(sym, 8);
                let st_size = read_u64(sym, 16);

                let mut name = read_utf8_cstring_with_cancellation(
                    strtab_data,
                    st_name as usize,
                    cancellation,
                )?;

                // For section symbols (STT_SECTION), the name in the string table is empty.
                // Use the section name instead, which is needed for resolving relocations
                // that target other sections (e.g., .text._ZN... internal runtime calls).
                let sym_type_raw = st_info & 0xf;
                if sym_type_raw == STT_SECTION
                    && name.is_empty()
                    && st_shndx != SHN_UNDEF
                    && (st_shndx as usize) < raw_sections.len()
                {
                    // Get the section name
                    let sec = &raw_sections[st_shndx as usize];
                    if let Ok(section_name) = read_utf8_cstring_with_cancellation(
                        shstrtab_data,
                        sec.name_offset as usize,
                        cancellation,
                    ) {
                        name = section_name;
                    }
                }

                let binding = match st_info >> 4 {
                    STB_LOCAL => SymbolBinding::Local,
                    STB_GLOBAL => SymbolBinding::Global,
                    STB_WEAK => SymbolBinding::Weak,
                    _ => SymbolBinding::Local,
                };

                let sym_type = match st_info & 0xf {
                    STT_NOTYPE => SymbolType::None,
                    STT_OBJECT => SymbolType::Object,
                    STT_FUNC => SymbolType::Func,
                    STT_SECTION => SymbolType::Section,
                    STT_FILE => SymbolType::File,
                    _ => SymbolType::None,
                };

                let section_index = if st_shndx == SHN_UNDEF || st_shndx >= SHN_LORESERVE {
                    None
                } else {
                    let idx = st_shndx as usize;
                    if idx >= raw_sections.len() {
                        return Err(ParseError::InvalidSymbol(format!(
                            "section index {} out of bounds (have {} sections)",
                            idx,
                            raw_sections.len()
                        )));
                    }
                    Some(idx)
                };

                symbols.push(Symbol {
                    name,
                    section_index,
                    value: st_value,
                    size: st_size,
                    binding,
                    sym_type,
                });
            }
        }

        // Parse relocations
        for raw in raw_sections.iter() {
            check_parse_cancellation(cancellation)?;
            if raw.sh_type != SHT_RELA {
                continue;
            }

            let target_section = raw.info as usize;
            if target_section >= sections.len() {
                continue;
            }

            // Validate relocation section bounds
            let rela_end = raw
                .offset
                .checked_add(raw.size)
                .ok_or(ParseError::RelocationOutOfBounds)?;
            if rela_end as usize > data.len() {
                return Err(ParseError::RelocationOutOfBounds);
            }
            let rela_data = &data[raw.offset as usize..rela_end as usize];

            if raw.entsize == 0 {
                continue; // Skip malformed relocation sections
            }
            let rela_count = raw.size / raw.entsize;
            // As with symbols, malformed undersized entries must reach the
            // existing bounds check without a large speculative allocation.
            if raw.entsize >= ELF64_RELA_SIZE as u64 {
                if let Ok(capacity) = usize::try_from(rela_count) {
                    let _ = sections[target_section].relocations.try_reserve(capacity);
                }
            }

            for j in 0..rela_count {
                check_parse_cancellation(cancellation)?;
                let rela_offset = (j * raw.entsize) as usize;
                if rela_offset + ELF64_RELA_SIZE > rela_data.len() {
                    return Err(ParseError::RelocationOutOfBounds);
                }
                let rela = &rela_data[rela_offset..rela_offset + ELF64_RELA_SIZE];

                // Bounds guaranteed by check above (rela_offset + 24 <= rela_data.len())
                let r_offset = read_u64(rela, 0);
                let r_info = read_u64(rela, 8);
                let r_addend = read_i64(rela, 16);

                let r_sym = (r_info >> 32) as usize;
                let r_type = (r_info & 0xffffffff) as u32;

                // Skip R_*_NONE relocations (type 0) - these are no-ops used for padding
                if r_type == 0 {
                    continue;
                }

                sections[target_section].relocations.push(Relocation {
                    offset: r_offset,
                    symbol_index: r_sym,
                    rel_type: RelocationType::from_elf(r_type, machine),
                    addend: r_addend,
                });
            }
        }

        Ok(ObjectFile {
            sections,
            symbols,
            section_map,
            machine,
            format: ObjectFormat::Elf,
        })
    }

    /// Find a symbol by name.
    #[must_use]
    pub fn find_symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.name == name)
    }

    /// Get all global/defined symbols.
    #[must_use]
    pub fn defined_symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().filter(|s| {
            s.section_index.is_some()
                && (s.binding == SymbolBinding::Global || s.binding == SymbolBinding::Weak)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{EI_CLASS, EI_DATA, EI_VERSION, ELF64_SHDR_SIZE as TEST_SHDR_SIZE};

    #[test]
    fn chunked_cstring_conversion_matches_whole_slice_at_utf8_boundaries() {
        const CHUNK: usize = 64 * 1024;
        for suffix in [
            "€tail".as_bytes(),
            &[0xE2, 0x28, 0xA1, b'x'][..],
            &[0xF0, 0x9F, 0x92][..],
        ] {
            let mut data = vec![b'a'; CHUNK - 1];
            data.extend_from_slice(suffix);
            data.push(0);
            data.extend_from_slice(b"ignored");
            let expected_end = data.iter().position(|byte| *byte == 0).unwrap();
            let expected = String::from_utf8_lossy(&data[..expected_end]);
            assert_eq!(
                read_cstring_with_cancellation(&data, &mut || false).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn strict_chunked_cstring_preserves_utf8_boundary_validation() {
        const CHUNK: usize = 64 * 1024;
        let mut valid = vec![b'a'; CHUNK - 1];
        valid.extend_from_slice("€tail".as_bytes());
        valid.push(0);
        assert_eq!(
            read_utf8_cstring_with_cancellation(&valid, 0, &mut || false).unwrap(),
            std::str::from_utf8(&valid[..valid.len() - 1]).unwrap()
        );

        let mut invalid = vec![b'a'; CHUNK - 1];
        invalid.extend_from_slice(&[0xE2, 0x28, 0xA1, 0]);
        assert!(matches!(
            read_utf8_cstring_with_cancellation(&invalid, 0, &mut || false),
            Err(ParseError::InvalidStringTable)
        ));
    }

    #[test]
    fn structured_object_preserves_arc_atom_ownership() {
        let text = Arc::<[u8]>::from(*b"ret");
        let object = StructuredObject {
            sections: vec![StructuredSection {
                name: ".text".into(),
                atoms: Arc::from([text.clone()]),
                size: text.len() as u64,
                flags: SectionFlags::ALLOC | SectionFlags::EXEC,
                relocations: Vec::new(),
                align: 16,
            }],
            symbols: Vec::new(),
            machine: ElfMachine::X86_64,
            format: ObjectFormat::Elf,
        };
        assert!(Arc::ptr_eq(&object.sections[0].atoms[0], &text));
        assert_eq!(object.sections[0].size, 3);
    }

    #[test]
    fn test_parse_error_display() {
        assert_eq!(
            ParseError::InvalidMagic.to_string(),
            "invalid ELF magic number"
        );
        assert_eq!(
            ParseError::TooShort.to_string(),
            "file is too short to be a valid object file"
        );
        assert_eq!(
            ParseError::UnknownFormat.to_string(),
            "unknown object file format (not ELF or Mach-O)"
        );
        assert_eq!(
            ParseError::NotImplemented("Mach-O object file parsing").to_string(),
            "Mach-O object file parsing not yet implemented"
        );
        assert_eq!(ParseError::Not64Bit.to_string(), "not a 64-bit ELF file");
        assert_eq!(
            ParseError::NotLittleEndian.to_string(),
            "not a little-endian ELF file"
        );
        assert_eq!(
            ParseError::NotRelocatable.to_string(),
            "not a relocatable object file"
        );
        assert_eq!(
            ParseError::UnsupportedMachine(0x99).to_string(),
            "unsupported ELF machine type: 0x99"
        );
        assert_eq!(
            ParseError::InvalidSection("test".into()).to_string(),
            "invalid section: test"
        );
        assert_eq!(
            ParseError::InvalidSymbol("test".into()).to_string(),
            "invalid symbol: test"
        );
        assert_eq!(
            ParseError::InvalidStringTable.to_string(),
            "invalid string table"
        );
        assert_eq!(
            ParseError::InvalidShstrndx.to_string(),
            "invalid section header string table index"
        );
        assert_eq!(
            ParseError::SectionOutOfBounds("test".into()).to_string(),
            "section data out of bounds: test"
        );
        assert_eq!(
            ParseError::RelocationOutOfBounds.to_string(),
            "relocation data out of bounds"
        );
    }

    #[test]
    fn test_too_short() {
        // File with less than 4 bytes - can't even check magic
        let data = [0u8; 3];
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::TooShort)
        ));
    }

    #[test]
    fn test_elf_too_short_for_header() {
        // File with valid ELF magic but too short for header
        let mut data = [0u8; 32];
        data[0..4].copy_from_slice(&ELF_MAGIC);
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::TooShort)
        ));
    }

    #[test]
    fn test_unknown_format() {
        // File with unrecognized magic bytes
        let mut data = [0u8; ELF64_EHDR_SIZE];
        data[0..4].copy_from_slice(b"NOTF");
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::UnknownFormat)
        ));
    }

    #[test]
    fn test_macho_parse_basic() {
        // Create a minimal Mach-O object file using ObjectBuilder
        use crate::emit::ObjectBuilder;
        use rue_target::Target;

        let obj_bytes = ObjectBuilder::new(Target::Aarch64Macos, "test_func")
            .code(vec![0xD6, 0x5F, 0x03, 0xC0]) // ret instruction
            .build();

        // Parse the Mach-O file
        let obj = ObjectFile::parse(&obj_bytes).expect("should parse Mach-O");

        // Should have at least one section (__TEXT,__text)
        assert!(!obj.sections.is_empty());
        assert!(obj.section_map.contains_key("__TEXT,__text"));

        // Should have the function symbol (underscore prefix stripped during parsing)
        let func_sym = obj.symbols.iter().find(|s| s.name == "test_func");
        assert!(func_sym.is_some(), "should find test_func symbol");

        // The function should be defined in a section
        let sym = func_sym.unwrap();
        assert!(sym.section_index.is_some());
    }

    /// Mach-O emit -> parse must round-trip the function symbol name for any
    /// number of leading underscores. The emitter prepends exactly one `_` and
    /// the parser strips exactly one, so distinct identifiers such as `_foo` and
    /// `__foo` stay distinct rather than collapsing (RUE-919).
    #[test]
    fn test_macho_symbol_name_round_trip() {
        use crate::emit::ObjectBuilder;
        use rue_target::Target;

        for name in ["foo", "_foo", "__foo", "___foo"] {
            let obj_bytes = ObjectBuilder::new(Target::Aarch64Macos, name)
                .code(vec![0xC0, 0x03, 0x5F, 0xD6]) // ret
                .build();
            let obj = ObjectFile::parse(&obj_bytes)
                .unwrap_or_else(|e| panic!("should parse Mach-O for {name:?}: {e:?}"));
            let found = obj.symbols.iter().find(|s| s.name == name);
            assert!(
                found.is_some(),
                "Mach-O round-trip lost symbol {name:?}; symbols = {:?}",
                obj.symbols.iter().map(|s| &s.name).collect::<Vec<_>>(),
            );
        }
    }

    /// ELF emit -> parse must NOT add or strip underscores: names pass through
    /// verbatim on both supported ELF targets. This guards the underscore-count
    /// boundary on the ELF path so the Mach-O-only strip never leaks into it
    /// (RUE-919).
    #[test]
    fn test_elf_symbol_name_round_trip() {
        use crate::emit::ObjectBuilder;
        use rue_target::Target;

        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            for name in ["foo", "_foo", "__foo", "___foo"] {
                let obj_bytes = ObjectBuilder::new(target, name)
                    .code(vec![0xC3]) // x86 ret (payload is irrelevant to symbol names)
                    .build();
                let obj = ObjectFile::parse(&obj_bytes).unwrap_or_else(|e| {
                    panic!("should parse ELF for {name:?} ({target:?}): {e:?}")
                });
                let found = obj.symbols.iter().find(|s| s.name == name);
                assert!(
                    found.is_some(),
                    "ELF round-trip lost symbol {name:?} ({target:?}); symbols = {:?}",
                    obj.symbols.iter().map(|s| &s.name).collect::<Vec<_>>(),
                );
            }
        }
    }

    // ---------------------------------------------------------------------
    // Hand-built Mach-O objects for parser tests (RUE-131 items 5b/5c).
    // ObjectBuilder only emits extern relocations with zero addends, so these
    // tests construct the raw bytes for the cases real (rustc/clang) objects
    // produce: embedded addends, ARM64_RELOC_ADDEND pairs, and non-extern
    // (section-based) relocations.
    // ---------------------------------------------------------------------

    struct TestMachoSection {
        sectname: &'static str,
        segname: &'static str,
        addr: u64,
        data: Vec<u8>,
        /// Raw (r_address, r_info) relocation entries.
        relocs: Vec<(u32, u32)>,
    }

    struct TestMachoSymbol {
        name: &'static str,
        n_type: u8,
        /// 1-indexed section number (0 = NO_SECT).
        n_sect: u8,
        n_value: u64,
    }

    /// Pack a Mach-O relocation_info r_info word.
    fn macho_r_info(symbolnum: u32, pcrel: bool, length: u32, ext: bool, rtype: u32) -> u32 {
        (symbolnum & 0x00FF_FFFF)
            | ((pcrel as u32) << 24)
            | ((length & 0x3) << 25)
            | ((ext as u32) << 27)
            | ((rtype & 0xF) << 28)
    }

    /// Build a minimal Mach-O object: one LC_SEGMENT_64 holding `sections`,
    /// plus an LC_SYMTAB with `symbols`.
    fn build_test_macho(sections: &[TestMachoSection], symbols: &[TestMachoSymbol]) -> Vec<u8> {
        let nsects = sections.len();
        let seg_cmd_size = MACHO64_SEGMENT_CMD_SIZE + MACHO64_SECTION_SIZE * nsects;
        let cmds_size = seg_cmd_size + crate::constants::MACHO64_SYMTAB_CMD_SIZE;
        let header_end = MACHO64_HEADER_SIZE + cmds_size;

        // Lay out: section data, then relocation entries, then symtab, then strtab
        let align8 = |v: usize| (v + 7) & !7;
        let mut cursor = header_end;
        let mut data_offsets = Vec::new();
        for s in sections {
            cursor = align8(cursor);
            data_offsets.push(cursor);
            cursor += s.data.len();
        }
        let mut reloc_offsets = Vec::new();
        for s in sections {
            cursor = align8(cursor);
            reloc_offsets.push(cursor);
            cursor += s.relocs.len() * MACHO64_RELOC_SIZE;
        }
        let symtab_off = align8(cursor);
        let strtab_off = symtab_off + symbols.len() * MACHO64_NLIST_SIZE;

        let mut strtab = vec![0u8];
        let mut name_offsets = Vec::new();
        for sym in symbols {
            name_offsets.push(strtab.len());
            strtab.extend_from_slice(sym.name.as_bytes());
            strtab.push(0);
        }

        let mut buf = Vec::new();
        // Header
        buf.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
        buf.extend_from_slice(&0x0100000C_u32.to_le_bytes()); // CPU_TYPE_ARM64
        buf.extend_from_slice(&0_u32.to_le_bytes()); // cpusubtype
        buf.extend_from_slice(&MH_OBJECT.to_le_bytes());
        buf.extend_from_slice(&2_u32.to_le_bytes()); // ncmds
        buf.extend_from_slice(&(cmds_size as u32).to_le_bytes());
        buf.extend_from_slice(&0_u32.to_le_bytes()); // flags
        buf.extend_from_slice(&0_u32.to_le_bytes()); // reserved

        // LC_SEGMENT_64
        buf.extend_from_slice(&LC_SEGMENT_64.to_le_bytes());
        buf.extend_from_slice(&(seg_cmd_size as u32).to_le_bytes());
        buf.extend_from_slice(&[0u8; 16]); // segname (empty for objects)
        buf.extend_from_slice(&0_u64.to_le_bytes()); // vmaddr
        buf.extend_from_slice(&0_u64.to_le_bytes()); // vmsize
        buf.extend_from_slice(&0_u64.to_le_bytes()); // fileoff
        buf.extend_from_slice(&0_u64.to_le_bytes()); // filesize
        buf.extend_from_slice(&7_u32.to_le_bytes()); // maxprot
        buf.extend_from_slice(&5_u32.to_le_bytes()); // initprot
        buf.extend_from_slice(&(nsects as u32).to_le_bytes());
        buf.extend_from_slice(&0_u32.to_le_bytes()); // flags

        for (i, s) in sections.iter().enumerate() {
            let mut sect = [0u8; 16];
            sect[..s.sectname.len()].copy_from_slice(s.sectname.as_bytes());
            let mut seg = [0u8; 16];
            seg[..s.segname.len()].copy_from_slice(s.segname.as_bytes());
            buf.extend_from_slice(&sect);
            buf.extend_from_slice(&seg);
            buf.extend_from_slice(&s.addr.to_le_bytes());
            buf.extend_from_slice(&(s.data.len() as u64).to_le_bytes());
            buf.extend_from_slice(&(data_offsets[i] as u32).to_le_bytes());
            buf.extend_from_slice(&3_u32.to_le_bytes()); // align 2^3
            buf.extend_from_slice(&(reloc_offsets[i] as u32).to_le_bytes());
            buf.extend_from_slice(&(s.relocs.len() as u32).to_le_bytes());
            let flags: u32 = if s.sectname == "__text" {
                0x80000000 // S_ATTR_PURE_INSTRUCTIONS
            } else {
                0
            };
            buf.extend_from_slice(&flags.to_le_bytes());
            buf.extend_from_slice(&0_u32.to_le_bytes()); // reserved1
            buf.extend_from_slice(&0_u32.to_le_bytes()); // reserved2
            buf.extend_from_slice(&0_u32.to_le_bytes()); // reserved3
        }

        // LC_SYMTAB
        buf.extend_from_slice(&LC_SYMTAB.to_le_bytes());
        buf.extend_from_slice(&(crate::constants::MACHO64_SYMTAB_CMD_SIZE as u32).to_le_bytes());
        buf.extend_from_slice(&(symtab_off as u32).to_le_bytes());
        buf.extend_from_slice(&(symbols.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(strtab_off as u32).to_le_bytes());
        buf.extend_from_slice(&(strtab.len() as u32).to_le_bytes());

        // Section data
        for (i, s) in sections.iter().enumerate() {
            buf.resize(data_offsets[i], 0);
            buf.extend_from_slice(&s.data);
        }
        // Relocations
        for (i, s) in sections.iter().enumerate() {
            buf.resize(reloc_offsets[i], 0);
            for (r_address, r_info) in &s.relocs {
                buf.extend_from_slice(&r_address.to_le_bytes());
                buf.extend_from_slice(&r_info.to_le_bytes());
            }
        }
        // Symbols
        buf.resize(symtab_off, 0);
        for (i, sym) in symbols.iter().enumerate() {
            buf.extend_from_slice(&(name_offsets[i] as u32).to_le_bytes());
            buf.push(sym.n_type);
            buf.push(sym.n_sect);
            buf.extend_from_slice(&0_u16.to_le_bytes());
            buf.extend_from_slice(&sym.n_value.to_le_bytes());
        }
        // String table
        buf.extend_from_slice(&strtab);
        buf
    }

    /// RUE-131 item 5b: ARM64_RELOC_UNSIGNED carries its addend in the bytes
    /// at the patch site, which the parser must preserve.
    #[test]
    fn test_macho_unsigned_reads_embedded_addend() {
        let mut slot = vec![0u8; 8];
        slot[0] = 0x10; // embedded addend = 0x10
        let obj_bytes = build_test_macho(
            &[TestMachoSection {
                sectname: "__const",
                segname: "__TEXT",
                addr: 0,
                data: slot,
                // extern UNSIGNED, 8 bytes, against symbol 0
                relocs: vec![(0, macho_r_info(0, false, 3, true, ARM64_RELOC_UNSIGNED))],
            }],
            &[TestMachoSymbol {
                name: "_foo",
                n_type: N_EXT | N_UNDF,
                n_sect: 0,
                n_value: 0,
            }],
        );

        let obj = ObjectFile::parse(&obj_bytes).expect("parse");
        let relocs = &obj.sections[0].relocations;
        assert_eq!(relocs.len(), 1);
        assert_eq!(relocs[0].rel_type, RelocationType::Aarch64Abs64);
        assert_eq!(
            relocs[0].addend, 0x10,
            "embedded addend must be read from the patch site"
        );
    }

    /// RUE-131 item 5b: an ARM64_RELOC_ADDEND entry supplies the addend for
    /// the relocation that follows it; it is not itself a relocation.
    #[test]
    fn test_macho_addend_reloc_pairs_with_next() {
        let obj_bytes = build_test_macho(
            &[TestMachoSection {
                sectname: "__text",
                segname: "__TEXT",
                addr: 0,
                data: vec![0u8; 8],
                relocs: vec![
                    // ADDEND +0x14 followed by BRANCH26 against symbol 0
                    (0, macho_r_info(0x14, false, 2, false, ARM64_RELOC_ADDEND)),
                    (0, macho_r_info(0, true, 2, true, ARM64_RELOC_BRANCH26)),
                    // ADDEND -8 (sign-extended 24-bit) + PAGE21
                    (
                        4,
                        macho_r_info(0x00FF_FFF8, false, 2, false, ARM64_RELOC_ADDEND),
                    ),
                    (4, macho_r_info(0, true, 2, true, ARM64_RELOC_PAGE21)),
                ],
            }],
            &[TestMachoSymbol {
                name: "_foo",
                n_type: N_EXT | N_UNDF,
                n_sect: 0,
                n_value: 0,
            }],
        );

        let obj = ObjectFile::parse(&obj_bytes).expect("parse");
        let relocs = &obj.sections[0].relocations;
        assert_eq!(relocs.len(), 2, "ADDEND entries are not relocations");
        assert_eq!(relocs[0].rel_type, RelocationType::Call26);
        assert_eq!(relocs[0].addend, 0x14);
        assert_eq!(relocs[1].rel_type, RelocationType::AdrpPage21);
        assert_eq!(relocs[1].addend, -8, "24-bit addend must be sign-extended");
    }

    /// RUE-131 item 5c: a non-extern relocation against a section with no
    /// symbol at offset 0 used to fail with "no function symbol found". The
    /// parser now synthesizes a local section anchor, and (item 5b) re-bases
    /// the embedded target address onto it.
    #[test]
    fn test_macho_non_extern_unsigned_synthesizes_anchor() {
        // __cstring lives at addr 0x10 in the object's VM layout; the pointer
        // slot holds the address of byte 3 within it (0x13).
        let mut slot = vec![0u8; 8];
        slot[0] = 0x13;
        let obj_bytes = build_test_macho(
            &[
                TestMachoSection {
                    sectname: "__const",
                    segname: "__TEXT",
                    addr: 0,
                    data: slot,
                    // non-extern UNSIGNED against section 2 (__cstring)
                    relocs: vec![(0, macho_r_info(2, false, 3, false, ARM64_RELOC_UNSIGNED))],
                },
                TestMachoSection {
                    sectname: "__cstring",
                    segname: "__TEXT",
                    addr: 0x10,
                    data: b"hi there".to_vec(),
                    relocs: vec![],
                },
            ],
            // No symbol at offset 0 of __cstring at all
            &[TestMachoSymbol {
                name: "_main",
                n_type: N_EXT | N_SECT,
                n_sect: 1,
                n_value: 0,
            }],
        );

        let obj = ObjectFile::parse(&obj_bytes).expect("parse must not require a Global at 0");
        let relocs = &obj.sections[0].relocations;
        assert_eq!(relocs.len(), 1);
        let anchor = &obj.symbols[relocs[0].symbol_index];
        assert_eq!(anchor.section_index, Some(1), "anchor must be in __cstring");
        assert_eq!(anchor.value, 0);
        assert_eq!(anchor.binding, SymbolBinding::Local);
        assert_eq!(
            relocs[0].addend, 3,
            "embedded target address must be re-based onto the section anchor"
        );
    }

    /// Non-extern relocations reuse an existing named symbol at offset 0 of
    /// the target section rather than synthesizing a duplicate anchor, and
    /// every relocation against the same section shares that one anchor. This
    /// pins the semantics of the precomputed section→anchor index that
    /// replaced the per-relocation symbol-table scan (RUE-1665).
    #[test]
    fn test_macho_non_extern_reuses_existing_section_anchor() {
        let mut first_slot = vec![0u8; 8];
        first_slot[0] = 0x13; // address of byte 3 within __cstring (addr 0x10)
        let mut second_slot = vec![0u8; 8];
        second_slot[0] = 0x15; // address of byte 5 within __cstring
        let mut data = first_slot;
        data.extend_from_slice(&second_slot);
        let obj_bytes = build_test_macho(
            &[
                TestMachoSection {
                    sectname: "__const",
                    segname: "__TEXT",
                    addr: 0,
                    data,
                    // Two non-extern UNSIGNED relocations against section 2.
                    relocs: vec![
                        (0, macho_r_info(2, false, 3, false, ARM64_RELOC_UNSIGNED)),
                        (8, macho_r_info(2, false, 3, false, ARM64_RELOC_UNSIGNED)),
                    ],
                },
                TestMachoSection {
                    sectname: "__cstring",
                    segname: "__TEXT",
                    addr: 0x10,
                    data: b"hi there".to_vec(),
                    relocs: vec![],
                },
            ],
            &[
                TestMachoSymbol {
                    name: "_main",
                    n_type: N_EXT | N_SECT,
                    n_sect: 1,
                    n_value: 0,
                },
                // A LOCAL symbol at offset 0 of __cstring (n_value is the
                // section's VM-layout address).
                TestMachoSymbol {
                    name: "l_str",
                    n_type: N_SECT,
                    n_sect: 2,
                    n_value: 0x10,
                },
            ],
        );

        let obj = ObjectFile::parse(&obj_bytes).expect("parse");
        assert_eq!(
            obj.symbols.len(),
            2,
            "an existing offset-0 symbol must be reused, not duplicated"
        );
        let relocs = &obj.sections[0].relocations;
        assert_eq!(relocs.len(), 2);
        assert_eq!(obj.symbols[relocs[0].symbol_index].name, "l_str");
        assert_eq!(
            relocs[0].symbol_index, relocs[1].symbol_index,
            "relocations against one section share one anchor"
        );
        assert_eq!(relocs[0].addend, 3);
        assert_eq!(relocs[1].addend, 5);
    }

    /// Mach-O n_value is an address in the object's VM layout, not a
    /// section-relative offset; the parser must subtract the section's addr.
    #[test]
    fn test_macho_symbol_value_normalized_to_section_offset() {
        let obj_bytes = build_test_macho(
            &[
                TestMachoSection {
                    sectname: "__text",
                    segname: "__TEXT",
                    addr: 0,
                    data: vec![0u8; 16],
                    relocs: vec![],
                },
                TestMachoSection {
                    sectname: "__cstring",
                    segname: "__TEXT",
                    addr: 0x10,
                    data: vec![0u8; 8],
                    relocs: vec![],
                },
            ],
            &[
                TestMachoSymbol {
                    name: "_main",
                    n_type: N_EXT | N_SECT,
                    n_sect: 1,
                    n_value: 4, // 4 into __text (addr 0)
                },
                TestMachoSymbol {
                    name: "_str",
                    n_type: N_SECT, // local
                    n_sect: 2,
                    n_value: 0x10 + 5, // 5 into __cstring (addr 0x10)
                },
            ],
        );

        let obj = ObjectFile::parse(&obj_bytes).expect("parse");
        let main = obj.find_symbol("main").expect("main");
        assert_eq!(main.value, 4);
        let s = obj.find_symbol("str").expect("str");
        assert_eq!(
            s.value, 5,
            "n_value must be converted to a section-relative offset"
        );
        assert_eq!(s.binding, SymbolBinding::Local);
    }

    #[test]
    fn test_macho_symbol_value_below_section_address_is_rejected() {
        let obj_bytes = build_test_macho(
            &[TestMachoSection {
                sectname: "__cstring",
                segname: "__TEXT",
                addr: 0x10,
                data: vec![0u8; 8],
                relocs: vec![],
            }],
            &[TestMachoSymbol {
                name: "_bad",
                n_type: N_EXT | N_SECT,
                n_sect: 1,
                n_value: 0x0f,
            }],
        );

        let error = ObjectFile::parse(&obj_bytes).unwrap_err();
        assert!(matches!(
            error,
            ParseError::InvalidSymbol(message)
                if message.contains("bad")
                    && message.contains("0xf")
                    && message.contains("0x10")
        ));
    }

    #[test]
    fn test_macho_too_short() {
        // File with Mach-O magic but too short should return TooShort
        let data = [0xCF, 0xFA, 0xED, 0xFE]; // MH_MAGIC_64 only (4 bytes)
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::TooShort)
        ));
    }

    /// RUE-334: an out-of-range Mach-O section alignment exponent must be
    /// rejected with a clean Err, not overflow the `1 << exp` shift (which
    /// panics in debug / is UB in release). The parser's shift is now checked.
    #[test]
    fn test_macho_out_of_range_alignment() {
        let mut obj_bytes = build_test_macho(
            &[TestMachoSection {
                sectname: "__text",
                segname: "__TEXT",
                addr: 0,
                data: vec![0u8; 8],
                relocs: vec![],
            }],
            &[],
        );

        // The align field is a u32 at offset 52 within section 0's header,
        // which begins right after the segment command.
        let align_field = MACHO64_HEADER_SIZE + MACHO64_SEGMENT_CMD_SIZE + 52;

        // Sanity: the unmodified object parses fine (align exponent 3).
        assert!(ObjectFile::parse(&obj_bytes).is_ok());

        // An exponent >= 64 would overflow `1u64 << exp`. Must be rejected.
        obj_bytes[align_field..align_field + 4].copy_from_slice(&64_u32.to_le_bytes());
        assert!(matches!(
            ObjectFile::parse(&obj_bytes),
            Err(ParseError::InvalidSection(_))
        ));

        // u32::MAX is the pathological worst case; also cleanly rejected.
        obj_bytes[align_field..align_field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            ObjectFile::parse(&obj_bytes),
            Err(ParseError::InvalidSection(_))
        ));

        // Exponent 63 is the largest in-range value and must still parse.
        obj_bytes[align_field..align_field + 4].copy_from_slice(&63_u32.to_le_bytes());
        assert!(ObjectFile::parse(&obj_bytes).is_ok());
    }

    #[test]
    fn test_macho_not_relocatable() {
        // Create a file with MH_EXECUTE type instead of MH_OBJECT
        let mut data = vec![0u8; 64];
        data[0..4].copy_from_slice(&MH_MAGIC_64.to_le_bytes()); // magic
        data[4..8].copy_from_slice(&0x0100000C_u32.to_le_bytes()); // CPU_TYPE_ARM64
        data[8..12].copy_from_slice(&0_u32.to_le_bytes()); // CPU_SUBTYPE_ARM64_ALL
        data[12..16].copy_from_slice(&2_u32.to_le_bytes()); // MH_EXECUTE (not MH_OBJECT)
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::NotRelocatable)
        ));
    }

    #[test]
    fn test_not_64bit() {
        let mut data = [0u8; ELF64_EHDR_SIZE];
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = 1; // 32-bit
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::Not64Bit)
        ));
    }

    #[test]
    fn test_not_little_endian() {
        let mut data = [0u8; ELF64_EHDR_SIZE];
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[EI_DATA] = 2; // Big endian
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::NotLittleEndian)
        ));
    }

    #[test]
    fn test_not_relocatable() {
        let mut data = [0u8; ELF64_EHDR_SIZE];
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[EI_DATA] = ELFDATA2LSB;
        data[E_TYPE_OFFSET..E_TYPE_OFFSET + 2]
            .copy_from_slice(&crate::constants::ET_EXEC.to_le_bytes()); // ET_EXEC instead of ET_REL
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::NotRelocatable)
        ));
    }

    #[test]
    fn test_unsupported_machine() {
        let mut data = [0u8; ELF64_EHDR_SIZE];
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[EI_DATA] = ELFDATA2LSB;
        data[E_TYPE_OFFSET..E_TYPE_OFFSET + 2].copy_from_slice(&ET_REL.to_le_bytes());
        data[E_MACHINE_OFFSET..E_MACHINE_OFFSET + 2]
            .copy_from_slice(&crate::constants::EM_386.to_le_bytes()); // EM_386 (unsupported)
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::UnsupportedMachine(0x03))
        ));
    }

    #[test]
    fn test_section_header_size_too_small() {
        let mut data = [0u8; ELF64_EHDR_SIZE];
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[EI_DATA] = ELFDATA2LSB;
        data[E_TYPE_OFFSET..E_TYPE_OFFSET + 2].copy_from_slice(&ET_REL.to_le_bytes());
        data[E_MACHINE_OFFSET..E_MACHINE_OFFSET + 2].copy_from_slice(&EM_X86_64.to_le_bytes());
        data[E_SHENTSIZE_OFFSET..E_SHENTSIZE_OFFSET + 2].copy_from_slice(&32_u16.to_le_bytes()); // e_shentsize = 32 (too small)
        data[E_SHNUM_OFFSET..E_SHNUM_OFFSET + 2].copy_from_slice(&1_u16.to_le_bytes()); // e_shnum = 1
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::InvalidSection(_))
        ));
    }

    #[test]
    fn test_large_section_count_short_table_fails_before_reserving_claimed_count() {
        let mut data = [0u8; ELF64_EHDR_SIZE];
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[EI_DATA] = ELFDATA2LSB;
        data[E_TYPE_OFFSET..E_TYPE_OFFSET + 2].copy_from_slice(&ET_REL.to_le_bytes());
        data[E_MACHINE_OFFSET..E_MACHINE_OFFSET + 2].copy_from_slice(&EM_X86_64.to_le_bytes());
        data[E_SHOFF_OFFSET..E_SHOFF_OFFSET + 8]
            .copy_from_slice(&(ELF64_EHDR_SIZE as u64).to_le_bytes());
        data[E_SHENTSIZE_OFFSET..E_SHENTSIZE_OFFSET + 2]
            .copy_from_slice(&(TEST_SHDR_SIZE as u16).to_le_bytes());
        data[E_SHNUM_OFFSET..E_SHNUM_OFFSET + 2].copy_from_slice(&u16::MAX.to_le_bytes());

        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::InvalidSection(message))
                if message == "section header out of bounds"
        ));
    }

    #[test]
    fn test_invalid_shstrndx() {
        let mut data = [0u8; ELF64_EHDR_SIZE];
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[EI_DATA] = ELFDATA2LSB;
        data[E_TYPE_OFFSET..E_TYPE_OFFSET + 2].copy_from_slice(&ET_REL.to_le_bytes());
        data[E_MACHINE_OFFSET..E_MACHINE_OFFSET + 2].copy_from_slice(&EM_X86_64.to_le_bytes());
        data[E_SHENTSIZE_OFFSET..E_SHENTSIZE_OFFSET + 2]
            .copy_from_slice(&(TEST_SHDR_SIZE as u16).to_le_bytes()); // e_shentsize = 64
        data[E_SHNUM_OFFSET..E_SHNUM_OFFSET + 2].copy_from_slice(&0_u16.to_le_bytes()); // e_shnum = 0
        data[E_SHSTRNDX_OFFSET..E_SHSTRNDX_OFFSET + 2].copy_from_slice(&5_u16.to_le_bytes()); // e_shstrndx = 5 (invalid)
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::InvalidShstrndx)
        ));
    }

    #[test]
    fn test_section_out_of_bounds() {
        // Create a minimal valid ELF header with one section that points out of bounds
        let mut data = vec![0u8; ELF64_EHDR_SIZE + TEST_SHDR_SIZE]; // header + one section header
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[EI_DATA] = ELFDATA2LSB;
        data[E_TYPE_OFFSET..E_TYPE_OFFSET + 2].copy_from_slice(&ET_REL.to_le_bytes());
        data[E_MACHINE_OFFSET..E_MACHINE_OFFSET + 2].copy_from_slice(&EM_X86_64.to_le_bytes());
        data[E_SHOFF_OFFSET..E_SHOFF_OFFSET + 8]
            .copy_from_slice(&(ELF64_EHDR_SIZE as u64).to_le_bytes()); // e_shoff = 64
        data[E_SHENTSIZE_OFFSET..E_SHENTSIZE_OFFSET + 2]
            .copy_from_slice(&(TEST_SHDR_SIZE as u16).to_le_bytes()); // e_shentsize = 64
        data[E_SHNUM_OFFSET..E_SHNUM_OFFSET + 2].copy_from_slice(&1_u16.to_le_bytes()); // e_shnum = 1
        data[E_SHSTRNDX_OFFSET..E_SHSTRNDX_OFFSET + 2].copy_from_slice(&0_u16.to_le_bytes()); // e_shstrndx = 0

        // Section header at offset 64
        // sh_type = SHT_STRTAB (3) to make it a string table
        let sh_offset = ELF64_EHDR_SIZE;
        data[sh_offset + 4..sh_offset + 8].copy_from_slice(&SHT_STRTAB.to_le_bytes()); // sh_type = SHT_STRTAB
        // sh_offset pointing way out of bounds
        data[sh_offset + 24..sh_offset + 32].copy_from_slice(&1000_u64.to_le_bytes());
        data[sh_offset + 32..sh_offset + 40].copy_from_slice(&100_u64.to_le_bytes()); // size

        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::SectionOutOfBounds(_))
        ));
    }

    #[test]
    fn test_section_flags() {
        let empty = SectionFlags::empty();
        assert!(!empty.contains(SectionFlags::WRITE));
        assert!(!empty.contains(SectionFlags::ALLOC));
        assert!(!empty.contains(SectionFlags::EXEC));

        let write_alloc = SectionFlags::WRITE | SectionFlags::ALLOC;
        assert!(write_alloc.contains(SectionFlags::WRITE));
        assert!(write_alloc.contains(SectionFlags::ALLOC));
        assert!(!write_alloc.contains(SectionFlags::EXEC));

        let mut flags = SectionFlags::empty();
        flags |= SectionFlags::EXEC;
        assert!(flags.contains(SectionFlags::EXEC));
    }

    #[test]
    fn test_relocation_type_from_elf_x86_64() {
        use ElfMachine::X86_64;
        assert_eq!(
            RelocationType::from_elf(R_X86_64_64, X86_64),
            RelocationType::Abs64
        );
        assert_eq!(
            RelocationType::from_elf(R_X86_64_PC32, X86_64),
            RelocationType::Pc32
        );
        assert_eq!(
            RelocationType::from_elf(R_X86_64_PLT32, X86_64),
            RelocationType::Plt32
        );
        assert_eq!(
            RelocationType::from_elf(R_X86_64_32, X86_64),
            RelocationType::Abs32
        );
        assert_eq!(
            RelocationType::from_elf(R_X86_64_32S, X86_64),
            RelocationType::Abs32S
        );
        assert_eq!(
            RelocationType::from_elf(99, X86_64),
            RelocationType::Unknown(99)
        );
    }

    #[test]
    fn test_relocation_type_from_elf_aarch64() {
        use ElfMachine::Aarch64;
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_ABS64, Aarch64),
            RelocationType::Aarch64Abs64
        );
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_ADR_PREL_PG_HI21, Aarch64),
            RelocationType::AdrpPage21
        );
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_ADD_ABS_LO12_NC, Aarch64),
            RelocationType::AddLo12
        );
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_JUMP26, Aarch64),
            RelocationType::Jump26
        );
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_CALL26, Aarch64),
            RelocationType::Call26
        );
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_LDST8_ABS_LO12_NC, Aarch64),
            RelocationType::Ldst8Lo12
        );
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_LDST16_ABS_LO12_NC, Aarch64),
            RelocationType::Ldst16Lo12
        );
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_LDST32_ABS_LO12_NC, Aarch64),
            RelocationType::Ldst32Lo12
        );
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_LDST64_ABS_LO12_NC, Aarch64),
            RelocationType::Ldst64Lo12
        );
        assert_eq!(
            RelocationType::from_elf(R_AARCH64_LDST128_ABS_LO12_NC, Aarch64),
            RelocationType::Ldst128Lo12
        );
        assert_eq!(
            RelocationType::from_elf(99, Aarch64),
            RelocationType::Unknown(99)
        );
    }

    #[test]
    fn test_symbol_binding_and_type() {
        // Test that the enum variants are distinct
        assert_ne!(SymbolBinding::Local, SymbolBinding::Global);
        assert_ne!(SymbolBinding::Global, SymbolBinding::Weak);

        assert_ne!(SymbolType::None, SymbolType::Func);
        assert_ne!(SymbolType::Func, SymbolType::Object);
    }

    #[test]
    fn test_read_helpers() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(read_u16(&data, 0), 0x0201);
        assert_eq!(read_u32(&data, 0), 0x04030201);
        assert_eq!(read_u64(&data, 0), 0x0807060504030201);
        assert_eq!(read_i64(&data, 0), 0x0807060504030201_i64);
    }

    #[test]
    fn test_empty_object_file() {
        // Create a minimal valid ELF with no sections
        let mut data = vec![0u8; ELF64_EHDR_SIZE];
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[EI_DATA] = ELFDATA2LSB;
        data[E_TYPE_OFFSET..E_TYPE_OFFSET + 2].copy_from_slice(&ET_REL.to_le_bytes());
        data[E_MACHINE_OFFSET..E_MACHINE_OFFSET + 2].copy_from_slice(&EM_X86_64.to_le_bytes());
        data[E_SHENTSIZE_OFFSET..E_SHENTSIZE_OFFSET + 2]
            .copy_from_slice(&(TEST_SHDR_SIZE as u16).to_le_bytes()); // e_shentsize = 64
        data[E_SHNUM_OFFSET..E_SHNUM_OFFSET + 2].copy_from_slice(&0_u16.to_le_bytes()); // e_shnum = 0
        data[E_SHSTRNDX_OFFSET..E_SHSTRNDX_OFFSET + 2].copy_from_slice(&0_u16.to_le_bytes()); // e_shstrndx = 0

        // This should fail because shstrndx=0 but there are no sections
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::InvalidShstrndx)
        ));
    }

    #[test]
    fn test_symbol_section_index_out_of_bounds() {
        // Tests that a symbol with a section index exceeding the section count
        // returns an error rather than panicking.
        //
        // Layout:
        // - ELF header (64 bytes)
        // - Section headers at offset 64:
        //   - [0] NULL section
        //   - [1] .shstrtab (section name string table)
        //   - [2] .strtab (symbol string table)
        //   - [3] .symtab (symbol table)
        // - Data area:
        //   - .shstrtab strings
        //   - .strtab strings
        //   - .symtab entries

        const NUM_SECTIONS: usize = 4;
        const SHDR_START: usize = ELF64_EHDR_SIZE;
        const SHDR_TOTAL_SIZE: usize = TEST_SHDR_SIZE * NUM_SECTIONS;
        const DATA_START: usize = SHDR_START + SHDR_TOTAL_SIZE;

        // Section name string table: "\0.shstrtab\0.strtab\0.symtab\0"
        let shstrtab_data = b"\0.shstrtab\0.strtab\0.symtab\0";
        let shstrtab_offset = DATA_START;
        let shstrtab_size = shstrtab_data.len();

        // Symbol string table: "\0test_symbol\0"
        let strtab_data = b"\0test_symbol\0";
        let strtab_offset = shstrtab_offset + shstrtab_size;
        let strtab_size = strtab_data.len();

        // Symbol table: one symbol entry (24 bytes) with section index = 99 (way out of bounds)
        let symtab_offset = strtab_offset + strtab_size;
        let mut sym_entry = [0u8; ELF64_SYM_SIZE];
        // st_name = 1 (offset to "test_symbol" in strtab)
        sym_entry[0..4].copy_from_slice(&1_u32.to_le_bytes());
        // st_info = STB_GLOBAL << 4 | STT_NOTYPE
        sym_entry[4] = crate::constants::elf_st_info(STB_GLOBAL, STT_NOTYPE);
        // st_other = 0
        sym_entry[5] = 0;
        // st_shndx = 99 (out of bounds - we only have 4 sections)
        sym_entry[6..8].copy_from_slice(&99_u16.to_le_bytes());
        // st_value = 0
        sym_entry[8..16].copy_from_slice(&0_u64.to_le_bytes());
        // st_size = 0
        sym_entry[16..24].copy_from_slice(&0_u64.to_le_bytes());

        let total_size = symtab_offset + ELF64_SYM_SIZE;
        let mut data = vec![0u8; total_size];

        // ELF header
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[EI_DATA] = ELFDATA2LSB;
        data[EI_VERSION] = crate::constants::EV_CURRENT;
        data[E_TYPE_OFFSET..E_TYPE_OFFSET + 2].copy_from_slice(&ET_REL.to_le_bytes());
        data[E_MACHINE_OFFSET..E_MACHINE_OFFSET + 2].copy_from_slice(&EM_X86_64.to_le_bytes());
        data[E_SHOFF_OFFSET..E_SHOFF_OFFSET + 8]
            .copy_from_slice(&(SHDR_START as u64).to_le_bytes()); // e_shoff
        data[E_SHENTSIZE_OFFSET..E_SHENTSIZE_OFFSET + 2]
            .copy_from_slice(&(TEST_SHDR_SIZE as u16).to_le_bytes()); // e_shentsize
        data[E_SHNUM_OFFSET..E_SHNUM_OFFSET + 2]
            .copy_from_slice(&(NUM_SECTIONS as u16).to_le_bytes()); // e_shnum
        data[E_SHSTRNDX_OFFSET..E_SHSTRNDX_OFFSET + 2].copy_from_slice(&1_u16.to_le_bytes()); // e_shstrndx = 1

        // Section header helper
        fn write_shdr(
            data: &mut [u8],
            index: usize,
            sh_name: u32,
            sh_type: u32,
            sh_offset: u64,
            sh_size: u64,
            sh_link: u32,
            sh_entsize: u64,
        ) {
            let base = SHDR_START + index * TEST_SHDR_SIZE;
            data[base..base + 4].copy_from_slice(&sh_name.to_le_bytes());
            data[base + 4..base + 8].copy_from_slice(&sh_type.to_le_bytes());
            data[base + 24..base + 32].copy_from_slice(&sh_offset.to_le_bytes());
            data[base + 32..base + 40].copy_from_slice(&sh_size.to_le_bytes());
            data[base + 40..base + 44].copy_from_slice(&sh_link.to_le_bytes());
            data[base + 56..base + 64].copy_from_slice(&sh_entsize.to_le_bytes());
        }

        // [0] NULL section
        write_shdr(&mut data, 0, 0, SHT_NULL, 0, 0, 0, 0);

        // [1] .shstrtab (name at offset 1 in shstrtab)
        write_shdr(
            &mut data,
            1,
            1, // ".shstrtab" starts at offset 1
            SHT_STRTAB,
            shstrtab_offset as u64,
            shstrtab_size as u64,
            0,
            0,
        );

        // [2] .strtab (name at offset 11 in shstrtab)
        write_shdr(
            &mut data,
            2,
            11, // ".strtab" starts at offset 11
            SHT_STRTAB,
            strtab_offset as u64,
            strtab_size as u64,
            0,
            0,
        );

        // [3] .symtab (name at offset 19 in shstrtab, sh_link = 2 for strtab)
        write_shdr(
            &mut data,
            3,
            19, // ".symtab" starts at offset 19
            SHT_SYMTAB,
            symtab_offset as u64,
            ELF64_SYM_SIZE as u64,
            2, // sh_link = strtab section
            ELF64_SYM_SIZE as u64,
        );

        // Write section data
        data[shstrtab_offset..shstrtab_offset + shstrtab_size].copy_from_slice(shstrtab_data);
        data[strtab_offset..strtab_offset + strtab_size].copy_from_slice(strtab_data);
        data[symtab_offset..symtab_offset + ELF64_SYM_SIZE].copy_from_slice(&sym_entry);

        // Parse should fail with InvalidSymbol due to section index out of bounds
        let result = ObjectFile::parse(&data);
        assert!(
            matches!(&result, Err(ParseError::InvalidSymbol(msg)) if msg.contains("section index")),
            "Expected InvalidSymbol error about section index, got: {:?}",
            result
        );
    }

    /// File offset of section header `index` in an ELF object, for the
    /// malformed-input tests below that corrupt one field of a valid object.
    fn shdr_offset(data: &[u8], index: usize) -> usize {
        let e_shoff =
            u64::from_le_bytes(data[E_SHOFF_OFFSET..E_SHOFF_OFFSET + 8].try_into().unwrap())
                as usize;
        let e_shentsize = u16::from_le_bytes(
            data[E_SHENTSIZE_OFFSET..E_SHENTSIZE_OFFSET + 2]
                .try_into()
                .unwrap(),
        ) as usize;
        e_shoff + index * e_shentsize
    }

    /// Index of the first section header whose `sh_type` equals `sh_type`.
    fn find_section_header(data: &[u8], sh_type: u32) -> usize {
        let e_shnum =
            u16::from_le_bytes(data[E_SHNUM_OFFSET..E_SHNUM_OFFSET + 2].try_into().unwrap())
                as usize;
        (0..e_shnum)
            .find(|&i| {
                let base = shdr_offset(data, i);
                u32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap()) == sh_type
            })
            .expect("section header of the requested type")
    }

    fn elf_object_bytes() -> Vec<u8> {
        crate::emit::ObjectBuilder::new(rue_target::Target::X86_64Linux, "main")
            .code(vec![0xC3])
            .build()
    }

    /// A `.symtab` whose `sh_link` names a section index the file does not have
    /// must be rejected, not indexed into. The parser took `sh_link` straight
    /// from the header and later used it as `&raw_sections[strtab_i]`
    /// (RUE-1645).
    #[test]
    fn malformed_symtab_sh_link_out_of_range_is_rejected() {
        let mut data = elf_object_bytes();
        let symtab = shdr_offset(&data, find_section_header(&data, SHT_SYMTAB));
        // sh_link is a u32 at offset 40 of the section header.
        data[symtab + 40..symtab + 44].copy_from_slice(&0xFFFF_u32.to_le_bytes());

        let result = ObjectFile::parse(&data);
        assert!(
            matches!(&result, Err(ParseError::InvalidSymbol(msg)) if msg.contains("sh_link")),
            "expected InvalidSymbol about sh_link, got: {result:?}"
        );
    }

    /// A symbol whose `st_name` points past the end of its string table must be
    /// rejected: the name reader sliced `strtab[start..end]`, and Rust panics on
    /// an out-of-range range start (RUE-1645).
    #[test]
    fn malformed_symbol_name_offset_past_strtab_is_rejected() {
        let mut data = elf_object_bytes();
        let symtab_header = shdr_offset(&data, find_section_header(&data, SHT_SYMTAB));
        let symtab_offset = u64::from_le_bytes(
            data[symtab_header + 24..symtab_header + 32]
                .try_into()
                .unwrap(),
        ) as usize;
        // Entry 0 is the reserved null symbol; corrupt entry 1's st_name.
        let sym = symtab_offset + ELF64_SYM_SIZE;
        data[sym..sym + 4].copy_from_slice(&0x00FF_FFFF_u32.to_le_bytes());

        let result = ObjectFile::parse(&data);
        assert!(
            matches!(&result, Err(ParseError::InvalidStringTable)),
            "expected InvalidStringTable, got: {result:?}"
        );
    }

    /// The same out-of-range slice start is reachable through a section's
    /// `sh_name` offset into `.shstrtab` (RUE-1645).
    #[test]
    fn malformed_section_name_offset_past_shstrtab_is_rejected() {
        let mut data = elf_object_bytes();
        let text = shdr_offset(
            &data,
            find_section_header(&data, crate::constants::SHT_PROGBITS),
        );
        data[text..text + 4].copy_from_slice(&0x00FF_FFFF_u32.to_le_bytes());

        let result = ObjectFile::parse(&data);
        assert!(
            matches!(&result, Err(ParseError::InvalidStringTable)),
            "expected InvalidStringTable, got: {result:?}"
        );
    }

    /// An `LC_SYMTAB` load command declaring a `cmdsize` smaller than the
    /// `symtab_command` struct must be rejected. Only `cmd_offset + cmdsize <=
    /// data.len()` was checked, so a final 8-byte command let the four u32
    /// field reads run past the buffer (RUE-1645).
    #[test]
    fn malformed_macho_symtab_command_too_small_is_rejected() {
        let mut data = vec![0u8; MACHO64_HEADER_SIZE + 8];
        data[0..4].copy_from_slice(&MH_MAGIC_64.to_le_bytes());
        data[4..8].copy_from_slice(&crate::constants::CPU_TYPE_ARM64.to_le_bytes());
        data[12..16].copy_from_slice(&MH_OBJECT.to_le_bytes());
        data[16..20].copy_from_slice(&1_u32.to_le_bytes()); // ncmds
        data[20..24].copy_from_slice(&8_u32.to_le_bytes()); // sizeofcmds
        // The single load command: LC_SYMTAB with a truncated cmdsize.
        data[MACHO64_HEADER_SIZE..MACHO64_HEADER_SIZE + 4]
            .copy_from_slice(&LC_SYMTAB.to_le_bytes());
        data[MACHO64_HEADER_SIZE + 4..MACHO64_HEADER_SIZE + 8]
            .copy_from_slice(&8_u32.to_le_bytes());

        let result = ObjectFile::parse(&data);
        assert!(
            matches!(&result, Err(ParseError::InvalidSymbol(msg)) if msg.contains("LC_SYMTAB")),
            "expected InvalidSymbol about LC_SYMTAB, got: {result:?}"
        );
    }
}
