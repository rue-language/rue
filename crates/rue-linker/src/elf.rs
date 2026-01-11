//! Object file parsing (ELF and Mach-O).
//!
//! Parses relocatable object files to extract:
//! - Sections (code and data)
//! - Symbols (defined and undefined)
//! - Relocations (patches to apply)
//!
//! Supports both ELF64 (Linux) and Mach-O 64-bit (macOS) formats.
//! The format is auto-detected based on magic bytes.

use std::collections::HashMap;

use crate::constants::{
    // Mach-O constants
    ARM64_RELOC_BRANCH26,
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

/// A parsed ELF64 relocatable object file.
#[derive(Debug)]
pub struct ObjectFile {
    /// All sections in the object file.
    pub sections: Vec<Section>,
    /// All symbols (both defined and undefined).
    pub symbols: Vec<Symbol>,
    /// Section name to index mapping.
    pub section_map: HashMap<String, usize>,
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
            ParseError::TooShort => write!(f, "file is too short to be a valid object file"),
            ParseError::InvalidMagic => write!(f, "invalid ELF magic number"),
            ParseError::Not64Bit => write!(f, "not a 64-bit ELF file"),
            ParseError::NotLittleEndian => write!(f, "not a little-endian ELF file"),
            ParseError::NotRelocatable => write!(f, "not a relocatable object file"),
            ParseError::UnsupportedMachine(m) => {
                write!(f, "unsupported ELF machine type: 0x{:x}", m)
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
        // Need at least 4 bytes to check magic
        if data.len() < 4 {
            return Err(ParseError::TooShort);
        }

        // Dispatch based on magic bytes
        if data[0..4] == ELF_MAGIC {
            Self::parse_elf(data)
        } else if data.len() >= 4 && read_u32(data, 0) == MH_MAGIC_64 {
            Self::parse_macho(data)
        } else {
            Err(ParseError::UnknownFormat)
        }
    }

    /// Parse a Mach-O 64-bit relocatable object file.
    ///
    /// Extracts sections, symbols, and relocations from a Mach-O object file.
    fn parse_macho(data: &[u8]) -> Result<Self, ParseError> {
        // Minimum size check for Mach-O header
        if data.len() < MACHO64_HEADER_SIZE {
            return Err(ParseError::TooShort);
        }

        // Verify magic (already checked in parse(), but be defensive)
        let magic = read_u32(data, 0);
        if magic != MH_MAGIC_64 {
            return Err(ParseError::InvalidMagic);
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
        let mut section_map = HashMap::new();
        let mut symtab_offset = 0usize;
        let mut symtab_count = 0usize;
        let mut strtab_offset = 0usize;
        let mut strtab_size = 0usize;

        // Track section info for relocations (offset, nreloc, reloff)
        let mut section_reloc_info: Vec<(usize, usize, usize)> = Vec::new();

        let mut cmd_offset = MACHO64_HEADER_SIZE;
        for _ in 0..ncmds {
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
                        if sect_offset + MACHO64_SECTION_SIZE > data.len() {
                            return Err(ParseError::TooShort);
                        }

                        // sectname: 16 bytes at offset 0
                        let sectname = read_cstring(&data[sect_offset..sect_offset + 16]);
                        // segname: 16 bytes at offset 16
                        let segname = read_cstring(&data[sect_offset + 16..sect_offset + 32]);
                        let full_name = format!("{},{}", segname, sectname);

                        // addr: u64 at offset 32
                        // size: u64 at offset 40
                        let size = read_u64(data, sect_offset + 40);
                        // offset: u32 at offset 48
                        let offset = read_u32(data, sect_offset + 48) as usize;
                        // align: u32 at offset 52
                        let align = 1u64 << read_u32(data, sect_offset + 52);
                        // reloff: u32 at offset 56
                        let reloff = read_u32(data, sect_offset + 56) as usize;
                        // nreloc: u32 at offset 60
                        let nreloc = read_u32(data, sect_offset + 60) as usize;
                        // flags: u32 at offset 64
                        let flags = read_u32(data, sect_offset + 64);

                        // Determine section flags
                        let mut section_flags = SectionFlags::empty();
                        section_flags |= SectionFlags::ALLOC;
                        // S_ATTR_PURE_INSTRUCTIONS = 0x80000000
                        if flags & 0x80000000 != 0 {
                            section_flags |= SectionFlags::EXEC;
                        }
                        // Check segment name for writability
                        if segname == "__DATA" {
                            section_flags |= SectionFlags::WRITE;
                        }

                        // Read section data
                        let section_data = if size > 0 && offset > 0 {
                            if offset + size as usize > data.len() {
                                return Err(ParseError::SectionOutOfBounds(full_name.clone()));
                            }
                            data[offset..offset + size as usize].to_vec()
                        } else {
                            Vec::new()
                        };

                        let section_index = sections.len();
                        section_reloc_info.push((section_index, nreloc, reloff));
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
            // Verify bounds
            let symtab_end = symtab_offset + symtab_count * MACHO64_NLIST_SIZE;
            let strtab_end = strtab_offset + strtab_size;
            if symtab_end > data.len() || strtab_end > data.len() {
                return Err(ParseError::InvalidSymbol(
                    "symbol table out of bounds".into(),
                ));
            }

            let strtab = &data[strtab_offset..strtab_end];

            for i in 0..symtab_count {
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
                    read_cstring(&strtab[n_strx..])
                } else {
                    String::new()
                };

                // Strip leading underscore from Mach-O symbols (but preserve double underscores)
                // On macOS, all symbols get ONE leading underscore added, so:
                // - "main" becomes "_main" -> strip to "main"
                // - "__main" becomes "___main" -> strip ONE to "__main"
                // - ".rodata.str0" becomes "_.rodata.str0" -> strip to ".rodata.str0"
                // We need to strip exactly one leading underscore to match what relocations expect
                if name.starts_with('_') && !name.starts_with("__") {
                    name = name[1..].to_string();
                } else if name.starts_with("___") {
                    // Triple underscore: original had __, so strip one to get back to __
                    name = name[1..].to_string();
                }

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

                symbols.push(Symbol {
                    name,
                    section_index,
                    value: n_value,
                    size: 0, // Mach-O doesn't store symbol size
                    binding,
                    sym_type,
                });
            }
        }

        // Parse relocations for each section
        for (section_index, nreloc, reloff) in section_reloc_info {
            if nreloc == 0 {
                continue;
            }

            let reloc_end = reloff + nreloc * MACHO64_RELOC_SIZE;
            if reloc_end > data.len() {
                return Err(ParseError::RelocationOutOfBounds);
            }

            for i in 0..nreloc {
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
                let _r_length = (r_info >> 25) & 3;
                let r_extern = (r_info >> 27) & 1;
                let r_type = (r_info >> 28) & 0xF;

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
                    // Local/section relocation: r_symbolnum is 1-indexed section number
                    // Find the function symbol for this section (should be at offset 0)
                    let target_section = (r_symbolnum - 1) as usize;
                    let idx = symbols
                        .iter()
                        .position(|s| {
                            s.section_index == Some(target_section)
                                && s.value == 0
                                && s.binding == SymbolBinding::Global
                        })
                        .ok_or_else(|| {
                            ParseError::InvalidSymbol(format!(
                                "no function symbol found for section {} (reloc at 0x{:x})",
                                target_section, r_address
                            ))
                        })?;
                    idx
                };

                // Convert Mach-O relocation type to our type
                let rel_type = match r_type {
                    ARM64_RELOC_UNSIGNED => RelocationType::Aarch64Abs64,
                    ARM64_RELOC_BRANCH26 => RelocationType::Call26, // Could be Jump26, but works either way
                    ARM64_RELOC_PAGE21 => RelocationType::AdrpPage21,
                    ARM64_RELOC_PAGEOFF12 => RelocationType::AddLo12,
                    _ => RelocationType::Unknown(r_type),
                };

                sections[section_index].relocations.push(Relocation {
                    offset: r_address,
                    symbol_index,
                    rel_type,
                    addend: 0, // Mach-O ARM64 uses implicit addends (in instruction)
                });
            }
        }

        Ok(ObjectFile {
            sections,
            symbols,
            section_map,
        })
    }

    /// Parse an ELF64 relocatable object file.
    fn parse_elf(data: &[u8]) -> Result<Self, ParseError> {
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
        let mut sections = Vec::new();
        let mut section_map = HashMap::new();
        let mut symtab_idx = None;
        let mut strtab_idx = None;

        // First pass: collect section info
        struct RawSection {
            name_offset: u32,
            sh_type: u32,
            flags: u64,
            offset: u64,
            size: u64,
            link: u32,
            info: u32,
            align: u64,
            entsize: u64,
        }

        let mut raw_sections = Vec::new();

        for i in 0..e_shnum {
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
                link,
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

        // Helper to read null-terminated string
        let read_string = |strtab: &[u8], offset: usize| -> Result<String, ParseError> {
            let start = offset;
            let mut end = start;
            while end < strtab.len() && strtab[end] != 0 {
                end += 1;
            }
            String::from_utf8(strtab[start..end].to_vec())
                .map_err(|_| ParseError::InvalidStringTable)
        };

        // Second pass: create sections with names
        for (i, raw) in raw_sections.iter().enumerate() {
            let name = read_string(shstrtab_data, raw.name_offset as usize)?;

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
                data[raw.offset as usize..section_end as usize].to_vec()
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
            for i in 0..sym_count {
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

                let mut name = read_string(strtab_data, st_name as usize)?;

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
                    if let Ok(section_name) = read_string(shstrtab_data, sec.name_offset as usize) {
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

            for j in 0..rela_count {
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

    #[test]
    fn test_macho_too_short() {
        // File with Mach-O magic but too short should return TooShort
        let data = [0xCF, 0xFA, 0xED, 0xFE]; // MH_MAGIC_64 only (4 bytes)
        assert!(matches!(
            ObjectFile::parse(&data),
            Err(ParseError::TooShort)
        ));
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
}
