//! Object file emission.
//!
//! Creates ELF64 relocatable object files from machine code and relocation info.

use rue_target::Target;

use crate::elf::RelocationType;

/// Information needed to create an object file.
pub struct ObjectBuilder {
    /// The target architecture and OS.
    target: Target,
    /// Name of the function being compiled.
    pub name: String,
    /// The machine code bytes.
    pub code: Vec<u8>,
    /// Relocations needed in the code.
    pub relocations: Vec<CodeRelocation>,
}

/// A relocation in generated code.
#[derive(Debug, Clone)]
pub struct CodeRelocation {
    /// Offset in the code section.
    pub offset: u64,
    /// Symbol name to reference.
    pub symbol: String,
    /// Relocation type.
    pub rel_type: RelocationType,
    /// Addend.
    pub addend: i64,
}

impl ObjectBuilder {
    /// Create a new object builder for the given target.
    pub fn new(target: Target, name: impl Into<String>) -> Self {
        ObjectBuilder {
            target,
            name: name.into(),
            code: Vec::new(),
            relocations: Vec::new(),
        }
    }

    /// Set the machine code.
    pub fn code(mut self, code: Vec<u8>) -> Self {
        self.code = code;
        self
    }

    /// Add a relocation.
    pub fn relocation(mut self, reloc: CodeRelocation) -> Self {
        self.relocations.push(reloc);
        self
    }

    /// Build a relocatable object file for the target.
    ///
    /// Generates ELF64 for Linux targets and Mach-O for macOS targets.
    pub fn build(self) -> Vec<u8> {
        if self.target.is_macho() {
            self.build_macho()
        } else {
            self.build_elf()
        }
    }

    /// Build an ELF64 relocatable object file.
    fn build_elf(self) -> Vec<u8> {
        let mut elf = Vec::new();

        // We'll create a minimal ELF with:
        // - ELF header
        // - Section headers at the end
        // - Sections: null, .text, .symtab, .strtab, .shstrtab, .rela.text

        // String tables
        let mut shstrtab = vec![0u8]; // Section header string table
        let mut strtab = vec![0u8];   // Symbol string table

        // Add section names to shstrtab
        let shstrtab_text = shstrtab.len();
        shstrtab.extend_from_slice(b".text\0");
        let shstrtab_symtab = shstrtab.len();
        shstrtab.extend_from_slice(b".symtab\0");
        let shstrtab_strtab = shstrtab.len();
        shstrtab.extend_from_slice(b".strtab\0");
        let shstrtab_shstrtab = shstrtab.len();
        shstrtab.extend_from_slice(b".shstrtab\0");
        let shstrtab_rela = shstrtab.len();
        shstrtab.extend_from_slice(b".rela.text\0");

        // Add symbol names to strtab
        let strtab_name = strtab.len();
        strtab.extend_from_slice(self.name.as_bytes());
        strtab.push(0);

        // Collect external symbols for relocations
        let mut extern_symbols: Vec<String> = Vec::new();
        let mut extern_symbol_offsets: Vec<usize> = Vec::new();
        for reloc in &self.relocations {
            if !extern_symbols.contains(&reloc.symbol) {
                extern_symbol_offsets.push(strtab.len());
                strtab.extend_from_slice(reloc.symbol.as_bytes());
                strtab.push(0);
                extern_symbols.push(reloc.symbol.clone());
            }
        }

        // Build symbol table
        // Symbol 0: null
        // Symbol 1: file (optional, skip for simplicity)
        // Symbol 2: .text section
        // Symbol 3: the function (global)
        // Symbol 4+: external symbols (undefined)

        let mut symtab = Vec::new();

        // Symbol 0: null symbol (24 bytes)
        symtab.extend_from_slice(&[0u8; 24]);

        // Symbol 1: .text section symbol
        symtab.extend_from_slice(&0_u32.to_le_bytes()); // st_name (empty)
        symtab.push(0x03); // st_info: STB_LOCAL, STT_SECTION
        symtab.push(0);    // st_other
        symtab.extend_from_slice(&1_u16.to_le_bytes()); // st_shndx: .text section
        symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_value
        symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_size

        // Symbol 2: the function (global)
        symtab.extend_from_slice(&(strtab_name as u32).to_le_bytes()); // st_name
        symtab.push(0x12); // st_info: STB_GLOBAL, STT_FUNC
        symtab.push(0);    // st_other
        symtab.extend_from_slice(&1_u16.to_le_bytes()); // st_shndx: .text
        symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_value
        symtab.extend_from_slice(&(self.code.len() as u64).to_le_bytes()); // st_size

        // External symbols (undefined)
        let first_extern_sym = 3_usize;
        for (i, _sym) in extern_symbols.iter().enumerate() {
            symtab.extend_from_slice(&(extern_symbol_offsets[i] as u32).to_le_bytes()); // st_name
            symtab.push(0x10); // st_info: STB_GLOBAL, STT_NOTYPE
            symtab.push(0);    // st_other
            symtab.extend_from_slice(&0_u16.to_le_bytes()); // st_shndx: SHN_UNDEF
            symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_value
            symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_size
        }

        // Build relocation table
        let mut rela = Vec::new();
        for reloc in &self.relocations {
            let sym_idx = extern_symbols.iter().position(|s| s == &reloc.symbol).unwrap() + first_extern_sym;
            let r_type: u32 = match reloc.rel_type {
                RelocationType::Abs64 => 1,
                RelocationType::Pc32 => 2,
                RelocationType::Plt32 => 4,
                RelocationType::Abs32 => 10,
                RelocationType::Abs32S => 11,
                RelocationType::Call26 => 283, // R_AARCH64_CALL26
                RelocationType::Unknown(t) => t,
            };
            let r_info = ((sym_idx as u64) << 32) | (r_type as u64);

            rela.extend_from_slice(&reloc.offset.to_le_bytes());
            rela.extend_from_slice(&r_info.to_le_bytes());
            rela.extend_from_slice(&reloc.addend.to_le_bytes());
        }

        // Calculate section layout
        const ELF_HEADER_SIZE: usize = 64;
        const SECTION_HEADER_SIZE: usize = 64;
        const NUM_SECTIONS: usize = 6; // null, .text, .symtab, .strtab, .shstrtab, .rela.text

        // Sections start right after ELF header
        let text_offset = ELF_HEADER_SIZE;
        let text_size = self.code.len();

        let symtab_offset = align_up(text_offset + text_size, 8);
        let symtab_size = symtab.len();

        let strtab_offset = symtab_offset + symtab_size;
        let strtab_size = strtab.len();

        let shstrtab_offset = strtab_offset + strtab_size;
        let shstrtab_size = shstrtab.len();

        let rela_offset = align_up(shstrtab_offset + shstrtab_size, 8);
        let rela_size = rela.len();

        let sh_offset = align_up(rela_offset + rela_size, 8);

        // === ELF Header ===
        elf.extend_from_slice(&[
            0x7F, b'E', b'L', b'F', // Magic
            2,    // 64-bit
            1,    // Little endian
            1,    // ELF version
            0,    // System V ABI
            0, 0, 0, 0, 0, 0, 0, 0, // Padding
        ]);
        elf.extend_from_slice(&1_u16.to_le_bytes()); // e_type: ET_REL
        elf.extend_from_slice(&self.target.elf_machine().to_le_bytes()); // e_machine
        elf.extend_from_slice(&1_u32.to_le_bytes()); // e_version
        elf.extend_from_slice(&0_u64.to_le_bytes()); // e_entry (none for relocatable)
        elf.extend_from_slice(&0_u64.to_le_bytes()); // e_phoff (no program headers)
        elf.extend_from_slice(&(sh_offset as u64).to_le_bytes()); // e_shoff
        elf.extend_from_slice(&0_u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_phnum
        elf.extend_from_slice(&(SECTION_HEADER_SIZE as u16).to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&(NUM_SECTIONS as u16).to_le_bytes()); // e_shnum
        elf.extend_from_slice(&4_u16.to_le_bytes()); // e_shstrndx: .shstrtab is section 4

        // === Sections ===
        // .text
        elf.extend_from_slice(&self.code);

        // Padding to symtab
        while elf.len() < symtab_offset {
            elf.push(0);
        }
        elf.extend_from_slice(&symtab);

        // .strtab
        elf.extend_from_slice(&strtab);

        // .shstrtab
        elf.extend_from_slice(&shstrtab);

        // Padding to rela
        while elf.len() < rela_offset {
            elf.push(0);
        }
        elf.extend_from_slice(&rela);

        // Padding to section headers
        while elf.len() < sh_offset {
            elf.push(0);
        }

        // === Section Headers ===

        // Section 0: null
        elf.extend_from_slice(&[0u8; 64]);

        // Section 1: .text
        elf.extend_from_slice(&(shstrtab_text as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&1_u32.to_le_bytes()); // sh_type: SHT_PROGBITS
        elf.extend_from_slice(&0x6_u64.to_le_bytes()); // sh_flags: SHF_ALLOC | SHF_EXECINSTR
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(text_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(text_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_link
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_info
        elf.extend_from_slice(&16_u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_entsize

        // Section 2: .symtab
        elf.extend_from_slice(&(shstrtab_symtab as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&2_u32.to_le_bytes()); // sh_type: SHT_SYMTAB
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_flags
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(symtab_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(symtab_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&3_u32.to_le_bytes()); // sh_link: .strtab
        elf.extend_from_slice(&2_u32.to_le_bytes()); // sh_info: first non-local symbol index
        elf.extend_from_slice(&8_u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&24_u64.to_le_bytes()); // sh_entsize

        // Section 3: .strtab
        elf.extend_from_slice(&(shstrtab_strtab as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&3_u32.to_le_bytes()); // sh_type: SHT_STRTAB
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_flags
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(strtab_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(strtab_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_link
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_info
        elf.extend_from_slice(&1_u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_entsize

        // Section 4: .shstrtab
        elf.extend_from_slice(&(shstrtab_shstrtab as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&3_u32.to_le_bytes()); // sh_type: SHT_STRTAB
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_flags
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(shstrtab_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(shstrtab_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_link
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_info
        elf.extend_from_slice(&1_u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_entsize

        // Section 5: .rela.text
        elf.extend_from_slice(&(shstrtab_rela as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&4_u32.to_le_bytes()); // sh_type: SHT_RELA
        elf.extend_from_slice(&0x40_u64.to_le_bytes()); // sh_flags: SHF_INFO_LINK
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(rela_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(rela_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&2_u32.to_le_bytes()); // sh_link: .symtab
        elf.extend_from_slice(&1_u32.to_le_bytes()); // sh_info: .text section
        elf.extend_from_slice(&8_u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&24_u64.to_le_bytes()); // sh_entsize

        elf
    }

    /// Build a Mach-O relocatable object file.
    ///
    /// This creates a minimal Mach-O object file for AArch64 macOS with:
    /// - Mach-O header
    /// - LC_SEGMENT_64 load command containing __TEXT,__text section
    /// - LC_SYMTAB load command
    /// - Code section data
    /// - Relocation entries
    /// - Symbol table
    /// - String table
    fn build_macho(self) -> Vec<u8> {
        let mut macho = Vec::new();

        // Mach-O constants
        const MH_MAGIC_64: u32 = 0xFEEDFACF;
        const MH_OBJECT: u32 = 0x1;
        const CPU_TYPE_ARM64: u32 = 0x0100000C; // CPU_TYPE_ARM | CPU_ARCH_ABI64
        const CPU_SUBTYPE_ARM64_ALL: u32 = 0;
        const LC_SEGMENT_64: u32 = 0x19;
        const LC_SYMTAB: u32 = 0x2;
        const LC_BUILD_VERSION: u32 = 0x32;
        const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x80000000;
        const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x00000400;

        // Platform constants for LC_BUILD_VERSION
        const PLATFORM_MACOS: u32 = 1;

        // ARM64 relocation types
        const ARM64_RELOC_BRANCH26: u32 = 2;

        // Calculate sizes and offsets
        const HEADER_SIZE: usize = 32;
        const SEGMENT_CMD_SIZE: usize = 72;
        const SECTION_SIZE: usize = 80;
        const SYMTAB_CMD_SIZE: usize = 24;
        const BUILD_VERSION_CMD_SIZE: usize = 24; // Without tool entries
        const NLIST_SIZE: usize = 16;
        const RELOC_SIZE: usize = 8;

        let num_sections = 1; // __text
        let segment_cmd_total = SEGMENT_CMD_SIZE + (SECTION_SIZE * num_sections);
        // Three load commands: LC_SEGMENT_64, LC_BUILD_VERSION, LC_SYMTAB
        let load_commands_size = segment_cmd_total + BUILD_VERSION_CMD_SIZE + SYMTAB_CMD_SIZE;
        let header_and_commands = HEADER_SIZE + load_commands_size;

        // Align section data to 4 bytes (required for ARM64)
        let section_offset = align_up(header_and_commands, 4);
        let section_size = self.code.len();

        // Relocations follow section data
        let reloc_offset = align_up(section_offset + section_size, 4);
        let num_relocs = self.relocations.len();

        // On macOS, all external C symbols get a leading underscore prefix.
        // This applies to ALL symbols, regardless of their original name.
        // e.g., "main" -> "_main", "__rue_exit" -> "___rue_exit"
        let macho_name = format!("_{}", self.name);

        // Build string table
        // Format: starts with a space for empty string, then null-terminated strings
        let mut strtab = vec![0x20, 0x00]; // Start with " \0" (space + null)

        // The function symbol name (with underscore prefix for macOS)
        let func_name_offset = strtab.len();
        strtab.extend_from_slice(macho_name.as_bytes());
        strtab.push(0);

        // External symbol names (for relocations)
        // All symbols need underscore prefix for macOS
        let mut extern_symbols: Vec<String> = Vec::new();
        let mut extern_name_offsets: Vec<usize> = Vec::new();
        for reloc in &self.relocations {
            // Always add underscore prefix for macOS
            let macho_sym = format!("_{}", reloc.symbol);
            if !extern_symbols.contains(&macho_sym) {
                extern_name_offsets.push(strtab.len());
                strtab.extend_from_slice(macho_sym.as_bytes());
                strtab.push(0);
                extern_symbols.push(macho_sym);
            }
        }

        // Symbol table follows relocations
        let symtab_offset = align_up(reloc_offset + (num_relocs * RELOC_SIZE), 4);
        let num_local_syms = 1; // Just our function
        let num_extern_syms = extern_symbols.len();
        let num_syms = num_local_syms + num_extern_syms;

        // String table follows symbol table
        let strtab_offset = symtab_offset + (num_syms * NLIST_SIZE);
        let strtab_size = strtab.len();

        // === Mach-O Header ===
        macho.extend_from_slice(&MH_MAGIC_64.to_le_bytes()); // magic
        macho.extend_from_slice(&CPU_TYPE_ARM64.to_le_bytes()); // cputype
        macho.extend_from_slice(&CPU_SUBTYPE_ARM64_ALL.to_le_bytes()); // cpusubtype
        macho.extend_from_slice(&MH_OBJECT.to_le_bytes()); // filetype
        macho.extend_from_slice(&3_u32.to_le_bytes()); // ncmds (LC_SEGMENT_64 + LC_BUILD_VERSION + LC_SYMTAB)
        macho.extend_from_slice(&(load_commands_size as u32).to_le_bytes()); // sizeofcmds
        macho.extend_from_slice(&0_u32.to_le_bytes()); // flags
        macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved (64-bit padding)

        // === LC_SEGMENT_64 ===
        macho.extend_from_slice(&LC_SEGMENT_64.to_le_bytes()); // cmd
        macho.extend_from_slice(&(segment_cmd_total as u32).to_le_bytes()); // cmdsize

        // segname: 16-byte null-padded string (empty for object files)
        let mut segname = [0u8; 16];
        macho.extend_from_slice(&segname);

        macho.extend_from_slice(&0_u64.to_le_bytes()); // vmaddr
        macho.extend_from_slice(&(section_size as u64).to_le_bytes()); // vmsize
        macho.extend_from_slice(&(section_offset as u64).to_le_bytes()); // fileoff
        macho.extend_from_slice(&(section_size as u64).to_le_bytes()); // filesize
        macho.extend_from_slice(&0x7_u32.to_le_bytes()); // maxprot (rwx)
        macho.extend_from_slice(&0x5_u32.to_le_bytes()); // initprot (rx)
        macho.extend_from_slice(&(num_sections as u32).to_le_bytes()); // nsects
        macho.extend_from_slice(&0_u32.to_le_bytes()); // flags

        // === Section: __text ===
        // sectname: 16-byte null-padded
        let mut sectname = [0u8; 16];
        sectname[..6].copy_from_slice(b"__text");
        macho.extend_from_slice(&sectname);

        // segname: 16-byte null-padded
        segname[..6].copy_from_slice(b"__TEXT");
        macho.extend_from_slice(&segname);

        macho.extend_from_slice(&0_u64.to_le_bytes()); // addr
        macho.extend_from_slice(&(section_size as u64).to_le_bytes()); // size
        macho.extend_from_slice(&(section_offset as u32).to_le_bytes()); // offset
        macho.extend_from_slice(&2_u32.to_le_bytes()); // align (2^2 = 4 byte alignment)
        macho.extend_from_slice(&(reloc_offset as u32).to_le_bytes()); // reloff
        macho.extend_from_slice(&(num_relocs as u32).to_le_bytes()); // nreloc
        macho.extend_from_slice(&(S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS).to_le_bytes()); // flags
        macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved1
        macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved2
        macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved3 (64-bit only)

        // === LC_BUILD_VERSION ===
        // This tells the linker which macOS version this was built for
        macho.extend_from_slice(&LC_BUILD_VERSION.to_le_bytes()); // cmd
        macho.extend_from_slice(&(BUILD_VERSION_CMD_SIZE as u32).to_le_bytes()); // cmdsize
        macho.extend_from_slice(&PLATFORM_MACOS.to_le_bytes()); // platform
        // minos: macOS 11.0.0 (Big Sur) - encoded as major.minor.patch in nibbles
        // 11.0.0 = 0x000B0000
        macho.extend_from_slice(&0x000B0000_u32.to_le_bytes()); // minos (11.0.0)
        macho.extend_from_slice(&0x000B0000_u32.to_le_bytes()); // sdk (11.0.0)
        macho.extend_from_slice(&0_u32.to_le_bytes()); // ntools

        // === LC_SYMTAB ===
        macho.extend_from_slice(&LC_SYMTAB.to_le_bytes()); // cmd
        macho.extend_from_slice(&(SYMTAB_CMD_SIZE as u32).to_le_bytes()); // cmdsize
        macho.extend_from_slice(&(symtab_offset as u32).to_le_bytes()); // symoff
        macho.extend_from_slice(&(num_syms as u32).to_le_bytes()); // nsyms
        macho.extend_from_slice(&(strtab_offset as u32).to_le_bytes()); // stroff
        macho.extend_from_slice(&(strtab_size as u32).to_le_bytes()); // strsize

        // === Section Data ===
        // Pad to section offset
        while macho.len() < section_offset {
            macho.push(0);
        }
        macho.extend_from_slice(&self.code);

        // === Relocations ===
        // Pad to relocation offset
        while macho.len() < reloc_offset {
            macho.push(0);
        }

        // Mach-O relocation format for ARM64:
        // r_address: 32-bit offset
        // r_symbolnum: 24-bit symbol index
        // r_pcrel: 1-bit (PC-relative)
        // r_length: 2-bit (0=byte, 1=word, 2=long, 3=quad)
        // r_extern: 1-bit (1 = symbol index, 0 = section ordinal)
        // r_type: 4-bit relocation type
        for reloc in &self.relocations {
            // Look up the underscore-prefixed version of the symbol
            let macho_sym = format!("_{}", reloc.symbol);
            let sym_idx = extern_symbols.iter().position(|s| s == &macho_sym).unwrap();
            // External symbols start after local symbols
            let sym_num = (num_local_syms + sym_idx) as u32;

            // r_address (4 bytes)
            macho.extend_from_slice(&(reloc.offset as u32).to_le_bytes());

            // r_symbolnum (24 bits) | r_pcrel (1) | r_length (2) | r_extern (1) | r_type (4)
            // For ARM64_RELOC_BRANCH26: pcrel=1, length=2 (4 bytes), extern=1
            let r_type = match reloc.rel_type {
                RelocationType::Call26 => ARM64_RELOC_BRANCH26,
                _ => ARM64_RELOC_BRANCH26, // Default to branch for now
            };
            let info: u32 = (sym_num & 0x00FFFFFF)  // r_symbolnum (bits 0-23)
                | (1 << 24)  // r_pcrel (bit 24)
                | (2 << 25)  // r_length (bits 25-26) - 2 means 4 bytes
                | (1 << 27)  // r_extern (bit 27)
                | (r_type << 28);  // r_type (bits 28-31)
            macho.extend_from_slice(&info.to_le_bytes());
        }

        // === Symbol Table ===
        // Pad to symbol table offset
        while macho.len() < symtab_offset {
            macho.push(0);
        }

        // nlist_64 structure:
        // n_strx: 4 bytes (string table index)
        // n_type: 1 byte
        // n_sect: 1 byte (1-indexed section number)
        // n_desc: 2 bytes
        // n_value: 8 bytes

        // Symbol constants
        const N_EXT: u8 = 0x01;  // External symbol
        const N_SECT: u8 = 0x0E; // Defined in section

        // Local symbol: the function itself
        macho.extend_from_slice(&(func_name_offset as u32).to_le_bytes()); // n_strx
        macho.push(N_EXT | N_SECT); // n_type: external, defined in section
        macho.push(1); // n_sect: section 1 (__text)
        macho.extend_from_slice(&0_u16.to_le_bytes()); // n_desc
        macho.extend_from_slice(&0_u64.to_le_bytes()); // n_value (offset in section)

        // External symbols (undefined)
        const N_UNDF: u8 = 0x00; // Undefined symbol
        for (i, _sym) in extern_symbols.iter().enumerate() {
            macho.extend_from_slice(&(extern_name_offsets[i] as u32).to_le_bytes()); // n_strx
            macho.push(N_EXT | N_UNDF); // n_type: external, undefined
            macho.push(0); // n_sect: NO_SECT
            macho.extend_from_slice(&0_u16.to_le_bytes()); // n_desc
            macho.extend_from_slice(&0_u64.to_le_bytes()); // n_value
        }

        // === String Table ===
        macho.extend_from_slice(&strtab);

        macho
    }
}

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::ObjectFile;
    use rue_target::Target;

    // Use X86_64Linux explicitly for ELF tests since ObjectFile only parses ELF
    const ELF_TARGET: Target = Target::X86_64Linux;

    #[test]
    fn test_simple_elf_object() {
        // Create a simple ELF object with just a ret instruction
        let obj = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![0xC3]) // ret
            .build();

        // Check ELF magic
        assert_eq!(&obj[0..4], b"\x7FELF");
        // Check it's relocatable
        assert_eq!(obj[16], 1); // ET_REL
    }

    #[test]
    fn test_simple_macho_object() {
        // Create a simple Mach-O object
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "main")
            .code(vec![0xD6, 0x5F, 0x03, 0xC0]) // ret (ARM64)
            .build();

        // Check Mach-O magic (little-endian)
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());
        // Check it's MH_OBJECT (file type at offset 12)
        assert_eq!(&obj[12..16], &0x1_u32.to_le_bytes());
    }

    #[test]
    fn test_elf_object_with_relocation() {
        // Create ELF object that calls an external function
        let obj = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0xE8, 0x00, 0x00, 0x00, 0x00, // call (placeholder)
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "external_func".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .build();

        // Basic validation
        assert_eq!(&obj[0..4], b"\x7FELF");
    }

    #[test]
    fn test_macho_object_with_relocation() {
        // Create Mach-O object that calls an external function
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "_main")
            .code(vec![
                0x00, 0x00, 0x00, 0x94, // bl (placeholder)
                0xD6, 0x5F, 0x03, 0xC0, // ret
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "_external_func".into(),
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .build();

        // Basic Mach-O validation
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());
    }

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(1, 8), 8);
        assert_eq!(align_up(7, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
        assert_eq!(align_up(16, 16), 16);
        assert_eq!(align_up(17, 16), 32);
    }

    #[test]
    fn test_roundtrip_simple() {
        // Create an ELF object and verify we can parse it back
        let built = ObjectBuilder::new(ELF_TARGET, "test_func")
            .code(vec![0x48, 0x89, 0xC0, 0xC3]) // mov rax, rax; ret
            .build();

        let parsed = ObjectFile::parse(&built).expect("should parse built object");

        // Verify the symbol exists
        let sym = parsed.find_symbol("test_func");
        assert!(sym.is_some(), "should find test_func symbol");
        let sym = sym.unwrap();
        assert!(sym.section_index.is_some(), "symbol should be defined");
    }

    #[test]
    fn test_roundtrip_with_relocation() {
        let built = ObjectBuilder::new(ELF_TARGET, "caller")
            .code(vec![
                0xE8, 0x00, 0x00, 0x00, 0x00, // call (placeholder)
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "callee".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .build();

        let parsed = ObjectFile::parse(&built).expect("should parse built object");

        // Verify the caller symbol exists
        let caller = parsed.find_symbol("caller");
        assert!(caller.is_some(), "should find caller symbol");

        // Verify the callee symbol exists (as undefined)
        let callee = parsed.find_symbol("callee");
        assert!(callee.is_some(), "should find callee symbol");
        let callee = callee.unwrap();
        assert!(callee.section_index.is_none(), "callee should be undefined");

        // Verify relocations exist
        let text_section = parsed.sections.iter()
            .find(|s| s.name == ".text")
            .expect("should have .text section");
        assert_eq!(text_section.relocations.len(), 1, "should have one relocation");
        assert_eq!(text_section.relocations[0].offset, 1);
        assert_eq!(text_section.relocations[0].addend, -4);
    }

    #[test]
    fn test_multiple_relocations() {
        let built = ObjectBuilder::new(ELF_TARGET, "multi_caller")
            .code(vec![
                0xE8, 0x00, 0x00, 0x00, 0x00, // call func1
                0xE8, 0x00, 0x00, 0x00, 0x00, // call func2
                0xE8, 0x00, 0x00, 0x00, 0x00, // call func1 again
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "func1".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .relocation(CodeRelocation {
                offset: 6,
                symbol: "func2".into(),
                rel_type: RelocationType::Plt32,
                addend: -4,
            })
            .relocation(CodeRelocation {
                offset: 11,
                symbol: "func1".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .build();

        let parsed = ObjectFile::parse(&built).expect("should parse built object");

        // Verify the text section has 3 relocations
        let text_section = parsed.sections.iter()
            .find(|s| s.name == ".text")
            .expect("should have .text section");
        assert_eq!(text_section.relocations.len(), 3, "should have three relocations");

        // func1 should only appear once in the symbol table
        let func1_count = parsed.symbols.iter()
            .filter(|s| s.name == "func1")
            .count();
        assert_eq!(func1_count, 1, "func1 should appear once in symbol table");
    }

    #[test]
    fn test_empty_code() {
        let built = ObjectBuilder::new(ELF_TARGET, "empty_func")
            .code(vec![])
            .build();

        let parsed = ObjectFile::parse(&built).expect("should parse empty object");
        let sym = parsed.find_symbol("empty_func");
        assert!(sym.is_some());
    }

    #[test]
    fn test_various_relocation_types() {
        let built = ObjectBuilder::new(ELF_TARGET, "reloc_test")
            .code(vec![0u8; 32])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "abs64_sym".into(),
                rel_type: RelocationType::Abs64,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 8,
                symbol: "abs32_sym".into(),
                rel_type: RelocationType::Abs32,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 12,
                symbol: "abs32s_sym".into(),
                rel_type: RelocationType::Abs32S,
                addend: 0,
            })
            .build();

        let parsed = ObjectFile::parse(&built).expect("should parse object with various reloc types");

        let text_section = parsed.sections.iter()
            .find(|s| s.name == ".text")
            .expect("should have .text section");
        assert_eq!(text_section.relocations.len(), 3);
    }
}
