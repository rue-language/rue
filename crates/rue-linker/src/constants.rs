//! Constants for ELF and Mach-O file formats.
//!
//! This module provides named constants for the magic numbers used in
//! ELF and Mach-O object files, making the code more readable and
//! less error-prone.

// =============================================================================
// ELF File Format Constants
// =============================================================================

/// ELF magic number: "\x7FELF"
pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

// ELF identification (e_ident) indices and values

/// EI_CLASS index in e_ident
pub const EI_CLASS: usize = 4;
/// EI_DATA index in e_ident
pub const EI_DATA: usize = 5;
/// EI_VERSION index in e_ident
pub const EI_VERSION: usize = 6;
/// EI_OSABI index in e_ident
pub const EI_OSABI: usize = 7;

/// ELFCLASS64: 64-bit objects
pub const ELFCLASS64: u8 = 2;
/// ELFDATA2LSB: Little-endian data encoding
pub const ELFDATA2LSB: u8 = 1;
/// EV_CURRENT: Current ELF version
pub const EV_CURRENT: u8 = 1;
/// ELFOSABI_NONE: System V ABI (also used for Linux)
pub const ELFOSABI_NONE: u8 = 0;

// ELF header sizes

/// Size of the ELF64 file header in bytes
pub const ELF64_EHDR_SIZE: usize = 64;
/// Size of an ELF64 program header entry in bytes
pub const ELF64_PHDR_SIZE: usize = 56;
/// Size of an ELF64 section header entry in bytes
pub const ELF64_SHDR_SIZE: usize = 64;
/// Size of an ELF64 symbol table entry in bytes
pub const ELF64_SYM_SIZE: usize = 24;
/// Size of an ELF64 relocation entry with addend (Rela) in bytes
pub const ELF64_RELA_SIZE: usize = 24;

// ELF header field offsets

/// Offset of e_type in ELF64 header
pub const E_TYPE_OFFSET: usize = 16;
/// Offset of e_machine in ELF64 header
pub const E_MACHINE_OFFSET: usize = 18;
/// Offset of e_shoff in ELF64 header
pub const E_SHOFF_OFFSET: usize = 40;
/// Offset of e_shentsize in ELF64 header
pub const E_SHENTSIZE_OFFSET: usize = 58;
/// Offset of e_shnum in ELF64 header
pub const E_SHNUM_OFFSET: usize = 60;
/// Offset of e_shstrndx in ELF64 header
pub const E_SHSTRNDX_OFFSET: usize = 62;

// ELF file types (e_type)

/// ET_REL: Relocatable file
pub const ET_REL: u16 = 1;
/// ET_EXEC: Executable file
pub const ET_EXEC: u16 = 2;
/// ET_DYN: Shared object file
pub const ET_DYN: u16 = 3;

// Machine types (e_machine)

/// EM_386: Intel 80386
pub const EM_386: u16 = 0x03;
/// EM_X86_64: AMD x86-64
pub const EM_X86_64: u16 = 0x3E;
/// EM_AARCH64: ARM 64-bit
pub const EM_AARCH64: u16 = 0xB7;

// Section header types (sh_type)

/// SHT_NULL: Inactive section header
pub const SHT_NULL: u32 = 0;
/// SHT_PROGBITS: Program-defined data
pub const SHT_PROGBITS: u32 = 1;
/// SHT_SYMTAB: Symbol table
pub const SHT_SYMTAB: u32 = 2;
/// SHT_STRTAB: String table
pub const SHT_STRTAB: u32 = 3;
/// SHT_RELA: Relocation entries with explicit addends
pub const SHT_RELA: u32 = 4;
/// SHT_NOBITS: Section occupies no space in file (e.g., .bss)
pub const SHT_NOBITS: u32 = 8;

// Section header flags (sh_flags)

/// SHF_WRITE: Writable section
pub const SHF_WRITE: u64 = 0x1;
/// SHF_ALLOC: Section occupies memory during execution
pub const SHF_ALLOC: u64 = 0x2;
/// SHF_EXECINSTR: Section contains executable instructions
pub const SHF_EXECINSTR: u64 = 0x4;
/// SHF_INFO_LINK: sh_info contains section header table index
pub const SHF_INFO_LINK: u64 = 0x40;

// Special section indices

/// SHN_UNDEF: Undefined section reference
pub const SHN_UNDEF: u16 = 0;
/// SHN_LORESERVE: Start of reserved section indices
pub const SHN_LORESERVE: u16 = 0xff00;
/// SHN_ABS: Absolute value (not relocated)
pub const SHN_ABS: u16 = 0xfff1;

// Symbol binding (upper 4 bits of st_info)

/// STB_LOCAL: Local symbol
pub const STB_LOCAL: u8 = 0;
/// STB_GLOBAL: Global symbol
pub const STB_GLOBAL: u8 = 1;
/// STB_WEAK: Weak symbol
pub const STB_WEAK: u8 = 2;

// Symbol types (lower 4 bits of st_info)

/// STT_NOTYPE: Symbol type not specified
pub const STT_NOTYPE: u8 = 0;
/// STT_OBJECT: Data object (variable, array, etc.)
pub const STT_OBJECT: u8 = 1;
/// STT_FUNC: Function or other executable code
pub const STT_FUNC: u8 = 2;
/// STT_SECTION: Section symbol
pub const STT_SECTION: u8 = 3;
/// STT_FILE: Source file name
pub const STT_FILE: u8 = 4;

/// Build st_info value from binding and type
#[inline]
pub const fn elf_st_info(binding: u8, sym_type: u8) -> u8 {
    (binding << 4) | (sym_type & 0xf)
}

/// Extract binding from st_info
#[inline]
pub const fn elf_st_bind(info: u8) -> u8 {
    info >> 4
}

/// Extract type from st_info
#[inline]
pub const fn elf_st_type(info: u8) -> u8 {
    info & 0xf
}

// x86-64 relocation types

/// R_X86_64_NONE: No relocation
pub const R_X86_64_NONE: u32 = 0;
/// R_X86_64_64: 64-bit absolute address
pub const R_X86_64_64: u32 = 1;
/// R_X86_64_PC32: 32-bit PC-relative address
pub const R_X86_64_PC32: u32 = 2;
/// R_X86_64_PLT32: 32-bit PLT-relative address
pub const R_X86_64_PLT32: u32 = 4;
/// R_X86_64_GOTPCREL: 32-bit GOT PC-relative
pub const R_X86_64_GOTPCREL: u32 = 9;
/// R_X86_64_32: 32-bit absolute address
pub const R_X86_64_32: u32 = 10;
/// R_X86_64_32S: 32-bit signed absolute address
pub const R_X86_64_32S: u32 = 11;
/// R_X86_64_GOTPCRELX: Relaxable 32-bit GOT PC-relative
pub const R_X86_64_GOTPCRELX: u32 = 41;
/// R_X86_64_REX_GOTPCRELX: Relaxable 32-bit GOT PC-relative with REX prefix
pub const R_X86_64_REX_GOTPCRELX: u32 = 42;

// AArch64 relocation types

/// R_AARCH64_ABS64: 64-bit absolute address
pub const R_AARCH64_ABS64: u32 = 257;
/// R_AARCH64_ADR_PREL_PG_HI21: ADRP instruction page address
pub const R_AARCH64_ADR_PREL_PG_HI21: u32 = 275;
/// R_AARCH64_ADD_ABS_LO12_NC: ADD instruction page offset
pub const R_AARCH64_ADD_ABS_LO12_NC: u32 = 277;
/// R_AARCH64_JUMP26: Unconditional branch
pub const R_AARCH64_JUMP26: u32 = 282;
/// R_AARCH64_CALL26: Branch with link
pub const R_AARCH64_CALL26: u32 = 283;

// Program header types (p_type)

/// PT_LOAD: Loadable segment
pub const PT_LOAD: u32 = 1;

// Program header flags (p_flags)

/// PF_X: Execute permission
pub const PF_X: u32 = 0x1;
/// PF_W: Write permission
pub const PF_W: u32 = 0x2;
/// PF_R: Read permission
pub const PF_R: u32 = 0x4;

// =============================================================================
// Mach-O File Format Constants
// =============================================================================

/// MH_MAGIC_64: Mach-O 64-bit magic number (little-endian)
pub const MH_MAGIC_64: u32 = 0xFEEDFACF;

// Mach-O file types

/// MH_OBJECT: Relocatable object file
pub const MH_OBJECT: u32 = 0x1;

// Mach-O CPU types

/// CPU_TYPE_ARM64: ARM 64-bit (includes CPU_ARCH_ABI64)
pub const CPU_TYPE_ARM64: u32 = 0x0100000C;
/// CPU_SUBTYPE_ARM64_ALL: All ARM64 subtypes
pub const CPU_SUBTYPE_ARM64_ALL: u32 = 0;

// Mach-O load commands

/// LC_SEGMENT_64: 64-bit segment load command
pub const LC_SEGMENT_64: u32 = 0x19;
/// LC_SYMTAB: Symbol table load command
pub const LC_SYMTAB: u32 = 0x2;
/// LC_BUILD_VERSION: Build version load command
pub const LC_BUILD_VERSION: u32 = 0x32;
/// LC_DYSYMTAB: Dynamic symbol table load command
pub const LC_DYSYMTAB: u32 = 0xb;

// Mach-O section flags

/// S_ATTR_PURE_INSTRUCTIONS: Section contains only machine instructions
pub const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x80000000;
/// S_ATTR_SOME_INSTRUCTIONS: Section contains some machine instructions
pub const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x00000400;

// Mach-O platform constants

/// PLATFORM_MACOS: macOS platform identifier
pub const PLATFORM_MACOS: u32 = 1;

// Mach-O symbol types

/// N_EXT: External symbol
pub const N_EXT: u8 = 0x01;
/// N_PEXT: Private external symbol (visible for linking but not exported)
pub const N_PEXT: u8 = 0x10;
/// N_SECT: Symbol defined in section
pub const N_SECT: u8 = 0x0E;
/// N_UNDF: Undefined symbol
pub const N_UNDF: u8 = 0x00;

// ARM64 relocation types (Mach-O)

/// ARM64_RELOC_UNSIGNED: 64-bit absolute address
pub const ARM64_RELOC_UNSIGNED: u32 = 0;
/// ARM64_RELOC_BRANCH26: Branch instruction
pub const ARM64_RELOC_BRANCH26: u32 = 2;
/// ARM64_RELOC_PAGE21: ADRP instruction
pub const ARM64_RELOC_PAGE21: u32 = 3;
/// ARM64_RELOC_PAGEOFF12: Page offset
pub const ARM64_RELOC_PAGEOFF12: u32 = 4;

// Mach-O structure sizes

/// Size of Mach-O 64-bit header
pub const MACHO64_HEADER_SIZE: usize = 32;
/// Size of Mach-O segment_command_64
pub const MACHO64_SEGMENT_CMD_SIZE: usize = 72;
/// Size of Mach-O section_64
pub const MACHO64_SECTION_SIZE: usize = 80;
/// Size of Mach-O symtab_command
pub const MACHO64_SYMTAB_CMD_SIZE: usize = 24;
/// Size of Mach-O build_version_command (without tool entries)
pub const MACHO64_BUILD_VERSION_CMD_SIZE: usize = 24;
/// Size of Mach-O nlist_64 symbol table entry
pub const MACHO64_NLIST_SIZE: usize = 16;
/// Size of Mach-O relocation_info entry
pub const MACHO64_RELOC_SIZE: usize = 8;
/// Size of Mach-O dysymtab_command
pub const MACHO64_DYSYMTAB_CMD_SIZE: usize = 80;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elf_st_info() {
        // STB_GLOBAL | STT_FUNC = 0x12
        assert_eq!(elf_st_info(STB_GLOBAL, STT_FUNC), 0x12);
        // STB_LOCAL | STT_SECTION = 0x03
        assert_eq!(elf_st_info(STB_LOCAL, STT_SECTION), 0x03);
        // STB_WEAK | STT_NOTYPE = 0x20
        assert_eq!(elf_st_info(STB_WEAK, STT_NOTYPE), 0x20);
    }

    #[test]
    fn test_elf_st_bind() {
        assert_eq!(elf_st_bind(0x12), STB_GLOBAL);
        assert_eq!(elf_st_bind(0x03), STB_LOCAL);
        assert_eq!(elf_st_bind(0x20), STB_WEAK);
    }

    #[test]
    fn test_elf_st_type() {
        assert_eq!(elf_st_type(0x12), STT_FUNC);
        assert_eq!(elf_st_type(0x03), STT_SECTION);
        assert_eq!(elf_st_type(0x00), STT_NOTYPE);
    }

    #[test]
    fn test_elf_magic() {
        assert_eq!(&ELF_MAGIC, b"\x7FELF");
    }
}
