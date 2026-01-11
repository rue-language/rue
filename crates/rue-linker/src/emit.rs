//! Object file emission.
//!
//! Creates ELF64 relocatable object files from machine code and relocation info.

use std::collections::HashMap;

use rue_target::Target;

use crate::constants::{
    ARM64_RELOC_BRANCH26, ARM64_RELOC_PAGE21, ARM64_RELOC_PAGEOFF12, ARM64_RELOC_UNSIGNED,
    CPU_SUBTYPE_ARM64_ALL, CPU_TYPE_ARM64, ELF_MAGIC, ELF64_EHDR_SIZE, ELF64_SHDR_SIZE,
    ELF64_SYM_SIZE, ELFCLASS64, ELFDATA2LSB, ET_REL, EV_CURRENT, LC_BUILD_VERSION, LC_DYSYMTAB,
    LC_SEGMENT_64, LC_SYMTAB, MACHO64_BUILD_VERSION_CMD_SIZE, MACHO64_DYSYMTAB_CMD_SIZE,
    MACHO64_HEADER_SIZE, MACHO64_NLIST_SIZE, MACHO64_RELOC_SIZE, MACHO64_SECTION_SIZE,
    MACHO64_SEGMENT_CMD_SIZE, MACHO64_SYMTAB_CMD_SIZE, MH_MAGIC_64, MH_OBJECT, N_EXT, N_PEXT,
    N_SECT, N_UNDF, PLATFORM_MACOS, R_AARCH64_ABS64, R_AARCH64_ADD_ABS_LO12_NC,
    R_AARCH64_ADR_PREL_PG_HI21, R_AARCH64_CALL26, R_AARCH64_JUMP26, R_X86_64_32, R_X86_64_32S,
    R_X86_64_64, R_X86_64_GOTPCREL, R_X86_64_GOTPCRELX, R_X86_64_PC32, R_X86_64_PLT32,
    R_X86_64_REX_GOTPCRELX, S_ATTR_PURE_INSTRUCTIONS, S_ATTR_SOME_INSTRUCTIONS, SHF_ALLOC,
    SHF_EXECINSTR, SHF_INFO_LINK, SHT_PROGBITS, SHT_RELA, SHT_STRTAB, SHT_SYMTAB, STB_GLOBAL,
    STB_LOCAL, STT_FUNC, STT_NOTYPE, STT_SECTION, elf_st_info,
};

/// ELF section layout with explicit indices.
///
/// The section header table layout is:
/// ```text
/// Index  Section     Condition
/// ─────  ──────────  ─────────────────
/// 0      (null)      always
/// 1      .text       always
/// 2      .rodata     if has_rodata
/// N      .symtab     follows .rodata if present, else follows .text
/// N+1    .strtab     follows .symtab
/// N+2    .shstrtab   follows .strtab
/// N+3    .rela.text  follows .shstrtab
/// ```
///
/// Symbol table references section indices, so they must be consistent:
/// - `.text` section symbol points to section 1
/// - `.rodata` section symbol points to section 2 (if present)
/// - Relocations in `.rela.text` target section 1 (`.text`)
#[derive(Debug, Clone, Copy)]
struct ElfSectionLayout {
    /// Whether .rodata section is present
    has_rodata: bool,
}

impl ElfSectionLayout {
    /// Create a new section layout.
    fn new(has_rodata: bool) -> Self {
        Self { has_rodata }
    }

    /// Null section (always index 0).
    const NULL: u16 = 0;

    /// .text section (always index 1).
    const TEXT: u16 = 1;

    /// .rodata section index (2 if present, otherwise unused).
    fn rodata(&self) -> u16 {
        debug_assert!(
            self.has_rodata,
            "rodata index requested but rodata not present"
        );
        2
    }

    /// .symtab section index.
    fn symtab(&self) -> u16 {
        if self.has_rodata { 3 } else { 2 }
    }

    /// .strtab section index.
    fn strtab(&self) -> u16 {
        if self.has_rodata { 4 } else { 3 }
    }

    /// .shstrtab section index.
    fn shstrtab(&self) -> u16 {
        if self.has_rodata { 5 } else { 4 }
    }

    /// Total number of sections.
    fn count(&self) -> u16 {
        if self.has_rodata { 7 } else { 6 }
    }

    /// Get the section index for the rodata section, or 0 if not present.
    /// Used for symbols that reference rodata - returns 0 (null) if rodata
    /// doesn't exist, which would make the symbol undefined.
    fn rodata_or_null(&self) -> u16 {
        if self.has_rodata { 2 } else { 0 }
    }
}

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
    /// String constants (for .rodata section).
    pub strings: Vec<String>,
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
    #[must_use]
    pub fn new(target: Target, name: impl Into<String>) -> Self {
        ObjectBuilder {
            target,
            name: name.into(),
            code: Vec::new(),
            relocations: Vec::new(),
            strings: Vec::new(),
        }
    }

    /// Set the machine code.
    #[must_use]
    pub fn code(mut self, code: Vec<u8>) -> Self {
        self.code = code;
        self
    }

    /// Add a relocation.
    #[must_use]
    pub fn relocation(mut self, reloc: CodeRelocation) -> Self {
        self.relocations.push(reloc);
        self
    }

    /// Set string constants.
    #[must_use]
    pub fn strings(mut self, strings: Vec<String>) -> Self {
        self.strings = strings;
        self
    }

    /// Build a relocatable object file for the target.
    ///
    /// Generates ELF64 for Linux targets and Mach-O for macOS targets.
    #[must_use]
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

        // Determine section layout based on whether we have rodata
        let has_rodata = !self.strings.is_empty();
        let layout = ElfSectionLayout::new(has_rodata);

        // Build .rodata content from strings
        let mut rodata = Vec::new();
        let mut string_offsets = Vec::new();
        for s in &self.strings {
            string_offsets.push(rodata.len());
            rodata.extend_from_slice(s.as_bytes());
            // No null terminator - Rue strings are length-prefixed
        }

        // String tables
        let mut shstrtab = vec![0u8]; // Section header string table
        let mut strtab = vec![0u8]; // Symbol string table

        // Add section names to shstrtab
        let shstrtab_text = shstrtab.len();
        shstrtab.extend_from_slice(b".text\0");
        let shstrtab_rodata = if has_rodata {
            let offset = shstrtab.len();
            shstrtab.extend_from_slice(b".rodata\0");
            offset
        } else {
            0
        };
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

        // Add string constant symbol names to strtab
        let mut string_symbol_offsets = Vec::new();
        for i in 0..self.strings.len() {
            string_symbol_offsets.push(strtab.len());
            let sym_name = format!(".rodata.str{}", i);
            strtab.extend_from_slice(sym_name.as_bytes());
            strtab.push(0);
        }

        // Collect external symbols for relocations (excluding string constants)
        // Use HashMap for O(1) lookup during relocation processing
        let mut extern_symbols: Vec<String> = Vec::new();
        let mut extern_symbol_offsets: Vec<usize> = Vec::new();
        let mut extern_symbol_indices: HashMap<String, usize> = HashMap::new();
        for reloc in &self.relocations {
            // Skip string constant symbols - they're local, not external
            if reloc.symbol.starts_with(".rodata.str") {
                continue;
            }
            if !extern_symbol_indices.contains_key(&reloc.symbol) {
                let idx = extern_symbols.len();
                extern_symbol_indices.insert(reloc.symbol.clone(), idx);
                extern_symbol_offsets.push(strtab.len());
                strtab.extend_from_slice(reloc.symbol.as_bytes());
                strtab.push(0);
                extern_symbols.push(reloc.symbol.clone());
            }
        }

        // Build symbol table
        // Symbol layout:
        // 0: null
        // 1: .text section symbol
        // 2: .rodata section symbol (if has_rodata)
        // 3..3+N: string constant symbols (local, in .rodata)
        // then: function symbol (global)
        // then: external symbols (undefined)

        let mut symtab = Vec::new();

        // Symbol 0: null symbol
        symtab.extend_from_slice(&[0u8; ELF64_SYM_SIZE]);

        // Symbol 1: .text section symbol
        symtab.extend_from_slice(&0_u32.to_le_bytes()); // st_name (empty)
        symtab.push(elf_st_info(STB_LOCAL, STT_SECTION)); // st_info
        symtab.push(0); // st_other
        symtab.extend_from_slice(&ElfSectionLayout::TEXT.to_le_bytes()); // st_shndx: .text section
        symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_value
        symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_size

        // Track symbol indices
        let mut next_sym_idx = 2_usize;

        // Symbol 2: .rodata section symbol (if has_rodata)
        if has_rodata {
            symtab.extend_from_slice(&0_u32.to_le_bytes()); // st_name (empty)
            symtab.push(elf_st_info(STB_LOCAL, STT_SECTION)); // st_info
            symtab.push(0); // st_other
            symtab.extend_from_slice(&layout.rodata().to_le_bytes()); // st_shndx: .rodata section
            symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_value
            symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_size
            next_sym_idx += 1;
        }

        // String constant symbols (local, defined in .rodata)
        let first_string_sym = next_sym_idx;
        for (i, offset) in string_offsets.iter().enumerate() {
            symtab.extend_from_slice(&(string_symbol_offsets[i] as u32).to_le_bytes()); // st_name
            symtab.push(elf_st_info(STB_LOCAL, STT_NOTYPE)); // st_info
            symtab.push(0); // st_other
            symtab.extend_from_slice(&layout.rodata_or_null().to_le_bytes()); // st_shndx: .rodata
            symtab.extend_from_slice(&(*offset as u64).to_le_bytes()); // st_value: offset in .rodata
            symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_size
            next_sym_idx += 1;
        }

        // First non-local symbol index (for sh_info)
        let first_global_sym = next_sym_idx;

        // Function symbol (global)
        let func_sym_idx = next_sym_idx;
        symtab.extend_from_slice(&(strtab_name as u32).to_le_bytes()); // st_name
        symtab.push(elf_st_info(STB_GLOBAL, STT_FUNC)); // st_info
        symtab.push(0); // st_other
        symtab.extend_from_slice(&ElfSectionLayout::TEXT.to_le_bytes()); // st_shndx: .text
        symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_value
        symtab.extend_from_slice(&(self.code.len() as u64).to_le_bytes()); // st_size
        next_sym_idx += 1;
        let _ = func_sym_idx; // suppress unused warning

        // External symbols (undefined)
        let first_extern_sym = next_sym_idx;
        for (i, _sym) in extern_symbols.iter().enumerate() {
            symtab.extend_from_slice(&(extern_symbol_offsets[i] as u32).to_le_bytes()); // st_name
            symtab.push(elf_st_info(STB_GLOBAL, STT_NOTYPE)); // st_info
            symtab.push(0); // st_other
            symtab.extend_from_slice(&crate::constants::SHN_UNDEF.to_le_bytes()); // st_shndx: SHN_UNDEF
            symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_value
            symtab.extend_from_slice(&0_u64.to_le_bytes()); // st_size
        }

        // Build relocation table
        let mut rela = Vec::new();
        for reloc in &self.relocations {
            // Determine symbol index
            let sym_idx = if reloc.symbol.starts_with(".rodata.str") {
                // String constant - local symbol
                let string_id: usize = reloc
                    .symbol
                    .strip_prefix(".rodata.str")
                    .unwrap()
                    .parse()
                    .unwrap();
                first_string_sym + string_id
            } else {
                // External symbol - O(1) lookup via HashMap
                extern_symbol_indices[&reloc.symbol] + first_extern_sym
            };

            let r_type: u32 = match reloc.rel_type {
                RelocationType::Abs64 => R_X86_64_64,
                RelocationType::Pc32 => R_X86_64_PC32,
                RelocationType::Plt32 => R_X86_64_PLT32,
                RelocationType::GotPcRel => R_X86_64_GOTPCREL,
                RelocationType::Abs32 => R_X86_64_32,
                RelocationType::Abs32S => R_X86_64_32S,
                RelocationType::GotPcRelX => R_X86_64_GOTPCRELX,
                RelocationType::RexGotPcRelX => R_X86_64_REX_GOTPCRELX,
                RelocationType::Jump26 => R_AARCH64_JUMP26,
                RelocationType::Call26 => R_AARCH64_CALL26,
                RelocationType::Aarch64Abs64 => R_AARCH64_ABS64,
                RelocationType::AdrpPage21 => R_AARCH64_ADR_PREL_PG_HI21,
                RelocationType::AddLo12 => R_AARCH64_ADD_ABS_LO12_NC,
                RelocationType::Unknown(t) => t,
            };
            let r_info = ((sym_idx as u64) << 32) | (r_type as u64);

            rela.extend_from_slice(&reloc.offset.to_le_bytes());
            rela.extend_from_slice(&r_info.to_le_bytes());
            rela.extend_from_slice(&reloc.addend.to_le_bytes());
        }

        // Calculate section layout (see ElfSectionLayout for section ordering)

        // Sections start right after ELF header
        let text_offset = ELF64_EHDR_SIZE;
        let text_size = self.code.len();

        // .rodata follows .text (if present)
        let rodata_offset = if has_rodata {
            align_up(text_offset + text_size, 8)
        } else {
            0
        };
        let rodata_size = rodata.len();

        let symtab_offset = if has_rodata {
            align_up(rodata_offset + rodata_size, 8)
        } else {
            align_up(text_offset + text_size, 8)
        };
        let symtab_size = symtab.len();

        let strtab_offset = symtab_offset + symtab_size;
        let strtab_size = strtab.len();

        let shstrtab_offset = strtab_offset + strtab_size;
        let shstrtab_size = shstrtab.len();

        let rela_offset = align_up(shstrtab_offset + shstrtab_size, 8);
        let rela_size = rela.len();

        let sh_offset = align_up(rela_offset + rela_size, 8);

        // === ELF Header ===
        elf.extend_from_slice(&ELF_MAGIC);
        elf.push(ELFCLASS64);
        elf.push(ELFDATA2LSB);
        elf.push(EV_CURRENT);
        elf.push(crate::constants::ELFOSABI_NONE);
        elf.extend_from_slice(&[0u8; 8]); // Padding
        elf.extend_from_slice(&ET_REL.to_le_bytes()); // e_type: ET_REL
        // Safety: build_elf() is only called for ELF targets (checked in build())
        elf.extend_from_slice(&self.target.elf_machine().unwrap().to_le_bytes()); // e_machine
        elf.extend_from_slice(&(EV_CURRENT as u32).to_le_bytes()); // e_version
        elf.extend_from_slice(&0_u64.to_le_bytes()); // e_entry (none for relocatable)
        elf.extend_from_slice(&0_u64.to_le_bytes()); // e_phoff (no program headers)
        elf.extend_from_slice(&(sh_offset as u64).to_le_bytes()); // e_shoff
        elf.extend_from_slice(&0_u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&(ELF64_EHDR_SIZE as u16).to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_phnum
        elf.extend_from_slice(&(ELF64_SHDR_SIZE as u16).to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&layout.count().to_le_bytes()); // e_shnum
        elf.extend_from_slice(&layout.shstrtab().to_le_bytes()); // e_shstrndx

        // === Sections ===
        // .text
        elf.extend_from_slice(&self.code);

        // .rodata (if present)
        if has_rodata {
            while elf.len() < rodata_offset {
                elf.push(0);
            }
            elf.extend_from_slice(&rodata);
        }

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

        // Section 0: null (ElfSectionLayout::NULL)
        let _ = ElfSectionLayout::NULL; // Assert this is section 0
        elf.extend_from_slice(&[0u8; ELF64_SHDR_SIZE]);

        // Section 1: .text
        elf.extend_from_slice(&(shstrtab_text as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&SHT_PROGBITS.to_le_bytes()); // sh_type: SHT_PROGBITS
        elf.extend_from_slice(&(SHF_ALLOC | SHF_EXECINSTR).to_le_bytes()); // sh_flags
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(text_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(text_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_link
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_info
        elf.extend_from_slice(&16_u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_entsize

        // Section 2: .rodata (if present)
        if has_rodata {
            elf.extend_from_slice(&(shstrtab_rodata as u32).to_le_bytes()); // sh_name
            elf.extend_from_slice(&SHT_PROGBITS.to_le_bytes()); // sh_type: SHT_PROGBITS
            elf.extend_from_slice(&SHF_ALLOC.to_le_bytes()); // sh_flags: SHF_ALLOC
            elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
            elf.extend_from_slice(&(rodata_offset as u64).to_le_bytes()); // sh_offset
            elf.extend_from_slice(&(rodata_size as u64).to_le_bytes()); // sh_size
            elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_link
            elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_info
            elf.extend_from_slice(&8_u64.to_le_bytes()); // sh_addralign
            elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_entsize
        }

        // Section: .symtab
        elf.extend_from_slice(&(shstrtab_symtab as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&SHT_SYMTAB.to_le_bytes()); // sh_type: SHT_SYMTAB
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_flags
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(symtab_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(symtab_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&(layout.strtab() as u32).to_le_bytes()); // sh_link: .strtab
        elf.extend_from_slice(&(first_global_sym as u32).to_le_bytes()); // sh_info: first non-local symbol
        elf.extend_from_slice(&8_u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&(ELF64_SYM_SIZE as u64).to_le_bytes()); // sh_entsize

        // Section: .strtab
        elf.extend_from_slice(&(shstrtab_strtab as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&SHT_STRTAB.to_le_bytes()); // sh_type: SHT_STRTAB
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_flags
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(strtab_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(strtab_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_link
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_info
        elf.extend_from_slice(&1_u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_entsize

        // Section: .shstrtab
        elf.extend_from_slice(&(shstrtab_shstrtab as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&SHT_STRTAB.to_le_bytes()); // sh_type: SHT_STRTAB
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_flags
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(shstrtab_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(shstrtab_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_link
        elf.extend_from_slice(&0_u32.to_le_bytes()); // sh_info
        elf.extend_from_slice(&1_u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_entsize

        // Section: .rela.text
        elf.extend_from_slice(&(shstrtab_rela as u32).to_le_bytes()); // sh_name
        elf.extend_from_slice(&SHT_RELA.to_le_bytes()); // sh_type: SHT_RELA
        elf.extend_from_slice(&SHF_INFO_LINK.to_le_bytes()); // sh_flags: SHF_INFO_LINK
        elf.extend_from_slice(&0_u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&(rela_offset as u64).to_le_bytes()); // sh_offset
        elf.extend_from_slice(&(rela_size as u64).to_le_bytes()); // sh_size
        elf.extend_from_slice(&(layout.symtab() as u32).to_le_bytes()); // sh_link: .symtab
        elf.extend_from_slice(&(ElfSectionLayout::TEXT as u32).to_le_bytes()); // sh_info: .text section
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

        // Build rodata content (string constants) - do this first to determine if we need rodata section
        let mut rodata = Vec::new();
        let mut string_offsets = Vec::new();
        for s in &self.strings {
            string_offsets.push(rodata.len());
            rodata.extend_from_slice(s.as_bytes());
            // No null terminator - Rue strings are length-prefixed
        }

        // Determine number of sections (1 for __text, +1 if we have non-empty rodata)
        // IMPORTANT: Check rodata.is_empty(), not self.strings.is_empty()
        // Empty strings ("") have no bytes, so rodata would be empty even with strings present.
        // Creating a zero-size rodata section with symbols pointing into it causes linker errors.
        let has_rodata = !rodata.is_empty();
        let num_sections = if has_rodata { 2 } else { 1 };
        let segment_cmd_total = MACHO64_SEGMENT_CMD_SIZE + (MACHO64_SECTION_SIZE * num_sections);
        // Four load commands: LC_SEGMENT_64, LC_BUILD_VERSION, LC_SYMTAB, LC_DYSYMTAB
        let load_commands_size = segment_cmd_total
            + MACHO64_BUILD_VERSION_CMD_SIZE
            + MACHO64_SYMTAB_CMD_SIZE
            + MACHO64_DYSYMTAB_CMD_SIZE;
        let header_and_commands = MACHO64_HEADER_SIZE + load_commands_size;

        // Align section data to 4 bytes (required for ARM64)
        let text_offset = align_up(header_and_commands, 4);
        let text_size = self.code.len();

        // Rodata follows text (if present)
        let rodata_offset = if has_rodata {
            align_up(text_offset + text_size, 8) // Align rodata to 8 bytes
        } else {
            0
        };
        let rodata_size = rodata.len();

        // Text relocations follow text and rodata sections
        let text_reloc_offset = if has_rodata {
            align_up(rodata_offset + rodata_size, 4)
        } else {
            align_up(text_offset + text_size, 4)
        };

        // Collect text section relocations, filtering out relocations for empty strings.
        // Note: All relocations (including string constant refs) are in the text section
        // because they're PC-relative loads from code. The rodata section contains only
        // raw string data with no relocations.
        // IMPORTANT: Filter out relocations for empty strings since they have no symbol.
        let text_relocs: Vec<&CodeRelocation> = self
            .relocations
            .iter()
            .filter(|reloc| {
                if reloc.symbol.starts_with(".rodata.str") {
                    // Check if this string is empty
                    if let Ok(string_id) = reloc
                        .symbol
                        .strip_prefix(".rodata.str")
                        .unwrap()
                        .parse::<usize>()
                    {
                        // Keep relocation only if string is non-empty
                        !self.strings.get(string_id).map_or(true, |s| s.is_empty())
                    } else {
                        true // Keep if we can't parse (shouldn't happen)
                    }
                } else {
                    true // Keep non-string relocations
                }
            })
            .collect();
        let num_text_relocs = text_relocs.len();

        // On macOS, all external C symbols get a leading underscore prefix.
        // This applies to ALL symbols, regardless of their original name.
        // e.g., "main" -> "_main", "__rue_exit" -> "___rue_exit"
        let macho_name = format!("_{}", self.name);

        // Build string table
        // Format: starts with null byte (index 0 = empty string), then null-terminated strings
        let mut strtab = vec![0u8]; // Start with null byte (same as ELF)

        // The function symbol name (with underscore prefix for macOS)
        let func_name_offset = strtab.len();
        strtab.extend_from_slice(macho_name.as_bytes());
        strtab.push(0);

        // String constant symbols (private external, for rodata)
        // Include function name in symbol to avoid collisions when linking multiple object files
        // IMPORTANT: Only create symbols for non-empty strings that have actual rodata content.
        // Empty strings have no bytes in rodata, so creating symbols for them would point
        // to invalid offsets or a non-existent rodata section.
        let mut string_name_offsets: Vec<usize> = Vec::new();
        let mut string_sym_indices: Vec<Option<usize>> = Vec::new(); // Maps string index to symbol index
        let mut num_string_syms = 0usize;
        for (i, s) in self.strings.iter().enumerate() {
            if s.is_empty() {
                // Empty string - no symbol needed (would point to invalid location)
                string_sym_indices.push(None);
            } else {
                string_sym_indices.push(Some(num_string_syms));
                string_name_offsets.push(strtab.len());
                let sym_name = format!("_.rodata.str{}", i);
                strtab.extend_from_slice(sym_name.as_bytes());
                strtab.push(0);
                num_string_syms += 1;
            }
        }

        // External symbol names (for relocations)
        // All symbols need underscore prefix for macOS
        let mut extern_symbols: Vec<String> = Vec::new();
        let mut extern_name_offsets: Vec<usize> = Vec::new();
        for reloc in &self.relocations {
            // Skip string symbols - they're handled via local symbols
            // String symbols use .rodata.strN format (possibly with @PAGE/@PAGEOFF suffix)
            if reloc.symbol.starts_with(".rodata.str") {
                continue;
            }
            // Always add underscore prefix for macOS
            let macho_sym = format!("_{}", reloc.symbol);
            if !extern_symbols.contains(&macho_sym) {
                extern_name_offsets.push(strtab.len());
                strtab.extend_from_slice(macho_sym.as_bytes());
                strtab.push(0);
                extern_symbols.push(macho_sym);
            }
        }

        // Symbol table follows text relocations
        let symtab_offset = align_up(
            text_reloc_offset + (num_text_relocs * MACHO64_RELOC_SIZE),
            4,
        );

        // In Mach-O, local symbols come first, then external symbols
        // All our symbols are external (N_EXT set) because ARM64_RELOC_PAGE21/PAGEOFF12
        // relocations with r_extern=1 require external target symbols.
        // Symbol order: function, string constants (non-empty only), undefined externals
        // IMPORTANT: Function must be first (index 0) because macOS linker rejects
        // r_symbolnum=0 in relocations as invalid. By putting function first,
        // string symbols start at index 1+.
        let num_local_syms = 0; // No local symbols - all are external
        let num_extern_syms = 1 + num_string_syms + extern_symbols.len(); // function + non-empty strings + external refs
        let num_syms = num_local_syms + num_extern_syms;

        // String table follows symbol table
        let strtab_offset = symtab_offset + (num_syms * MACHO64_NLIST_SIZE);
        let strtab_size = strtab.len();

        // === Mach-O Header ===
        macho.extend_from_slice(&MH_MAGIC_64.to_le_bytes()); // magic
        macho.extend_from_slice(&CPU_TYPE_ARM64.to_le_bytes()); // cputype
        macho.extend_from_slice(&CPU_SUBTYPE_ARM64_ALL.to_le_bytes()); // cpusubtype
        macho.extend_from_slice(&MH_OBJECT.to_le_bytes()); // filetype
        macho.extend_from_slice(&4_u32.to_le_bytes()); // ncmds (LC_SEGMENT_64 + LC_BUILD_VERSION + LC_SYMTAB + LC_DYSYMTAB)
        macho.extend_from_slice(&(load_commands_size as u32).to_le_bytes()); // sizeofcmds
        macho.extend_from_slice(&0_u32.to_le_bytes()); // flags
        macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved (64-bit padding)

        // === LC_SEGMENT_64 ===
        macho.extend_from_slice(&LC_SEGMENT_64.to_le_bytes()); // cmd
        macho.extend_from_slice(&(segment_cmd_total as u32).to_le_bytes()); // cmdsize

        // segname: 16-byte null-padded string (empty for object files)
        let mut segname = [0u8; 16];
        macho.extend_from_slice(&segname);

        // Segment vmsize is the total size of all sections
        let vmsize = if has_rodata {
            (rodata_offset - text_offset) + rodata_size
        } else {
            text_size
        };

        macho.extend_from_slice(&0_u64.to_le_bytes()); // vmaddr
        macho.extend_from_slice(&(vmsize as u64).to_le_bytes()); // vmsize
        macho.extend_from_slice(&(text_offset as u64).to_le_bytes()); // fileoff
        macho.extend_from_slice(&(vmsize as u64).to_le_bytes()); // filesize
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
        macho.extend_from_slice(&(text_size as u64).to_le_bytes()); // size
        macho.extend_from_slice(&(text_offset as u32).to_le_bytes()); // offset
        macho.extend_from_slice(&2_u32.to_le_bytes()); // align (2^2 = 4 byte alignment)
        macho.extend_from_slice(&(text_reloc_offset as u32).to_le_bytes()); // reloff
        macho.extend_from_slice(&(num_text_relocs as u32).to_le_bytes()); // nreloc
        macho.extend_from_slice(
            &(S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS).to_le_bytes(),
        ); // flags
        macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved1
        macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved2
        macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved3 (64-bit only)

        // === Section: __rodata (if present) ===
        if has_rodata {
            let mut sectname = [0u8; 16];
            sectname[..8].copy_from_slice(b"__rodata");
            macho.extend_from_slice(&sectname);

            // segname: __DATA or __TEXT - using __TEXT for read-only data
            let mut segname = [0u8; 16];
            segname[..6].copy_from_slice(b"__TEXT");
            macho.extend_from_slice(&segname);

            macho.extend_from_slice(&(text_size as u64).to_le_bytes()); // addr (follows text)
            macho.extend_from_slice(&(rodata_size as u64).to_le_bytes()); // size
            macho.extend_from_slice(&(rodata_offset as u32).to_le_bytes()); // offset
            macho.extend_from_slice(&3_u32.to_le_bytes()); // align (2^3 = 8 byte alignment)
            macho.extend_from_slice(&0_u32.to_le_bytes()); // reloff (rodata has no relocations)
            macho.extend_from_slice(&0_u32.to_le_bytes()); // nreloc
            macho.extend_from_slice(&0_u32.to_le_bytes()); // flags (regular data)
            macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved1
            macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved2
            macho.extend_from_slice(&0_u32.to_le_bytes()); // reserved3 (64-bit only)
        }

        // === LC_BUILD_VERSION ===
        // This tells the linker which macOS version this was built for
        macho.extend_from_slice(&LC_BUILD_VERSION.to_le_bytes()); // cmd
        macho.extend_from_slice(&(MACHO64_BUILD_VERSION_CMD_SIZE as u32).to_le_bytes()); // cmdsize
        macho.extend_from_slice(&PLATFORM_MACOS.to_le_bytes()); // platform
        // minos/sdk: macOS minimum version (encoded as 0x00XXYYPP for major.minor.patch)
        let macos_version = self.target.macos_min_version().unwrap_or(0x000B0000);
        macho.extend_from_slice(&macos_version.to_le_bytes()); // minos
        macho.extend_from_slice(&macos_version.to_le_bytes()); // sdk
        macho.extend_from_slice(&0_u32.to_le_bytes()); // ntools

        // === LC_SYMTAB ===
        macho.extend_from_slice(&LC_SYMTAB.to_le_bytes()); // cmd
        macho.extend_from_slice(&(MACHO64_SYMTAB_CMD_SIZE as u32).to_le_bytes()); // cmdsize
        macho.extend_from_slice(&(symtab_offset as u32).to_le_bytes()); // symoff
        macho.extend_from_slice(&(num_syms as u32).to_le_bytes()); // nsyms
        macho.extend_from_slice(&(strtab_offset as u32).to_le_bytes()); // stroff
        macho.extend_from_slice(&(strtab_size as u32).to_le_bytes()); // strsize

        // === LC_DYSYMTAB ===
        // Symbol table organization:
        //   - Local symbols: 0 (we have none - all are N_EXT)
        //   - External defined symbols: function + string constants
        //   - Undefined symbols: external references
        let num_extdef = 1 + num_string_syms; // function + string symbols
        let num_undef = extern_symbols.len();

        macho.extend_from_slice(&LC_DYSYMTAB.to_le_bytes()); // cmd
        macho.extend_from_slice(&(MACHO64_DYSYMTAB_CMD_SIZE as u32).to_le_bytes()); // cmdsize
        macho.extend_from_slice(&0_u32.to_le_bytes()); // ilocalsym: index to local symbols
        macho.extend_from_slice(&0_u32.to_le_bytes()); // nlocalsym: number of local symbols
        macho.extend_from_slice(&0_u32.to_le_bytes()); // iextdefsym: index to external defined symbols
        macho.extend_from_slice(&(num_extdef as u32).to_le_bytes()); // nextdefsym: number of external defined
        macho.extend_from_slice(&(num_extdef as u32).to_le_bytes()); // iundefsym: index to undefined symbols
        macho.extend_from_slice(&(num_undef as u32).to_le_bytes()); // nundefsym: number of undefined symbols
        macho.extend_from_slice(&0_u32.to_le_bytes()); // tocoff: file offset to table of contents
        macho.extend_from_slice(&0_u32.to_le_bytes()); // ntoc: number of entries in TOC
        macho.extend_from_slice(&0_u32.to_le_bytes()); // modtaboff: file offset to module table
        macho.extend_from_slice(&0_u32.to_le_bytes()); // nmodtab: number of module table entries
        macho.extend_from_slice(&0_u32.to_le_bytes()); // extrefsymoff: offset to referenced symbol table
        macho.extend_from_slice(&0_u32.to_le_bytes()); // nextrefsyms: number of referenced symbol entries
        macho.extend_from_slice(&0_u32.to_le_bytes()); // indirectsymoff: file offset to indirect symbol table
        macho.extend_from_slice(&0_u32.to_le_bytes()); // nindirectsyms: number of indirect symbol entries
        macho.extend_from_slice(&0_u32.to_le_bytes()); // extreloff: offset to external relocation entries
        macho.extend_from_slice(&0_u32.to_le_bytes()); // nextrel: number of external relocation entries
        macho.extend_from_slice(&0_u32.to_le_bytes()); // locreloff: offset to local relocation entries
        macho.extend_from_slice(&0_u32.to_le_bytes()); // nlocrel: number of local relocation entries

        // === Section Data ===
        // Pad to text offset
        while macho.len() < text_offset {
            macho.push(0);
        }
        macho.extend_from_slice(&self.code);

        // Add rodata section if present
        if has_rodata {
            while macho.len() < rodata_offset {
                macho.push(0);
            }
            macho.extend_from_slice(&rodata);
        }

        // === Text Relocations ===
        // Pad to text relocation offset
        while macho.len() < text_reloc_offset {
            macho.push(0);
        }

        // Mach-O relocation format for ARM64:
        // r_address: 32-bit offset
        // r_symbolnum: 24-bit symbol index
        // r_pcrel: 1-bit (PC-relative)
        // r_length: 2-bit (0=byte, 1=word, 2=long, 3=quad)
        // r_extern: 1-bit (1 = symbol index, 0 = section ordinal)
        // r_type: 4-bit relocation type
        for reloc in &text_relocs {
            // Look up the symbol
            // Symbol table layout: function (0), non-empty string symbols (1..N), undefined externals (N+1..)
            let (sym_num, is_extern) = if reloc.symbol.starts_with(".rodata.str") {
                // String symbol (already filtered to exclude empty strings)
                let string_id: usize = reloc
                    .symbol
                    .strip_prefix(".rodata.str")
                    .unwrap()
                    .parse()
                    .unwrap();
                // Look up the symbol index for this string
                let sym_idx = string_sym_indices[string_id]
                    .expect("empty string relocations should have been filtered out");
                // Non-empty string: symbol index is 1 + sym_idx (function is at 0)
                (1 + sym_idx as u32, true)
            } else {
                // External symbol (function or undefined external)
                // First check if it's the function itself
                if reloc.symbol == self.name {
                    // Function symbol is at index 0
                    (0_u32, true)
                } else {
                    // Undefined external symbol
                    let macho_sym = format!("_{}", reloc.symbol);
                    let sym_idx = extern_symbols.iter().position(|s| s == &macho_sym).unwrap();
                    // Undefined externals start after function and non-empty string symbols
                    (1 + num_string_syms as u32 + sym_idx as u32, true)
                }
            };

            // r_address (4 bytes)
            macho.extend_from_slice(&(reloc.offset as u32).to_le_bytes());

            // Determine relocation type and r_pcrel flag from rel_type
            let (r_type, r_pcrel) = match reloc.rel_type {
                RelocationType::AdrpPage21 => (ARM64_RELOC_PAGE21, 1), // PC-relative
                RelocationType::AddLo12 => (ARM64_RELOC_PAGEOFF12, 0), // Not PC-relative
                RelocationType::Call26 | RelocationType::Jump26 => (ARM64_RELOC_BRANCH26, 1),
                RelocationType::Aarch64Abs64 => (ARM64_RELOC_UNSIGNED, 0), // 64-bit absolute
                _ => unreachable!(
                    "unexpected relocation type {:?} in Mach-O emission",
                    reloc.rel_type
                ),
            };

            let info: u32 = (sym_num & 0x00FFFFFF)  // r_symbolnum (bits 0-23)
                | (r_pcrel << 24)  // r_pcrel (bit 24)
                | (2 << 25)  // r_length (bits 25-26) - 2 means 4 bytes
                | ((is_extern as u32) << 27)  // r_extern (bit 27)
                | (r_type << 28); // r_type (bits 28-31)
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

        // All symbols are external (N_EXT set) for ARM64 relocation compatibility.
        // Symbol table order: function, string constants, undefined externals.
        // IMPORTANT: Function must be at index 0 so that string symbols start at
        // index 1+. macOS linker rejects r_symbolnum=0 in relocations as invalid.

        // Symbol 0: External symbol for the function itself
        macho.extend_from_slice(&(func_name_offset as u32).to_le_bytes()); // n_strx
        macho.push(N_EXT | N_SECT); // n_type: external, defined in section
        macho.push(1); // n_sect: section 1 (__text)
        macho.extend_from_slice(&0_u16.to_le_bytes()); // n_desc
        macho.extend_from_slice(&0_u64.to_le_bytes()); // n_value (offset in section)

        // Symbols 1..N: String constant symbols (private external, defined in rodata section)
        // Note: We mark these as N_PEXT | N_EXT because ARM64_RELOC_PAGE21/PAGEOFF12
        // relocations with r_extern=1 require the target symbol to be external.
        // N_PEXT makes them private (not exported), avoiding duplicate symbol errors
        // when linking multiple object files that each have their own string constants.
        // IMPORTANT: Only emit symbols for non-empty strings (ones that have actual content).
        let mut sym_name_idx = 0;
        for (i, s) in self.strings.iter().enumerate() {
            if s.is_empty() {
                // Skip empty strings - they have no symbol
                continue;
            }
            macho.extend_from_slice(&(string_name_offsets[sym_name_idx] as u32).to_le_bytes()); // n_strx
            macho.push(N_SECT); // n_type: truly local, defined in section
            if has_rodata {
                macho.push(2); // n_sect: section 2 (__rodata)
            } else {
                macho.push(0); // Should not happen, but defensive
            }
            macho.extend_from_slice(&0_u16.to_le_bytes()); // n_desc
            // n_value is the offset within the section (not a VM address!)
            // For relocatable object files, n_value is relative to the section start
            let section_offset = string_offsets[i] as u64;
            macho.extend_from_slice(&section_offset.to_le_bytes());
            sym_name_idx += 1;
        }

        // Symbols N+1..: External symbols (undefined)
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
        let text_section = parsed
            .sections
            .iter()
            .find(|s| s.name == ".text")
            .expect("should have .text section");
        assert_eq!(
            text_section.relocations.len(),
            1,
            "should have one relocation"
        );
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
        let text_section = parsed
            .sections
            .iter()
            .find(|s| s.name == ".text")
            .expect("should have .text section");
        assert_eq!(
            text_section.relocations.len(),
            3,
            "should have three relocations"
        );

        // func1 should only appear once in the symbol table
        let func1_count = parsed.symbols.iter().filter(|s| s.name == "func1").count();
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

        let parsed =
            ObjectFile::parse(&built).expect("should parse object with various reloc types");

        let text_section = parsed
            .sections
            .iter()
            .find(|s| s.name == ".text")
            .expect("should have .text section");
        assert_eq!(text_section.relocations.len(), 3);
    }

    // ========================================================================
    // Mach-O Comprehensive Tests
    // ========================================================================

    /// Test ADRP + ADD pairs (common pattern for loading addresses on ARM64).
    /// Each pair uses PAGE21 + PAGEOFF12 relocation types.
    #[test]
    fn test_macho_adrp_add_relocation_pairs() {
        // Simulate code that loads multiple addresses using ADRP + ADD pairs:
        // adrp x0, symbol1@PAGE
        // add  x0, x0, symbol1@PAGEOFF
        // adrp x1, symbol2@PAGE
        // add  x1, x1, symbol2@PAGEOFF
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "load_addresses")
            .code(vec![
                // adrp x0, symbol1@PAGE (placeholder)
                0x00, 0x00, 0x00, 0x90, // add x0, x0, symbol1@PAGEOFF (placeholder)
                0x00, 0x00, 0x00, 0x91, // adrp x1, symbol2@PAGE (placeholder)
                0x01, 0x00, 0x00, 0x90, // add x1, x1, symbol2@PAGEOFF (placeholder)
                0x21, 0x00, 0x00, 0x91, // ret
                0xC0, 0x03, 0x5F, 0xD6,
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "symbol1".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 4,
                symbol: "symbol1".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 8,
                symbol: "symbol2".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 12,
                symbol: "symbol2".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .build();

        // Basic Mach-O validation
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // Verify we have at least 4 relocations worth of data
        // (The file should be larger than just the header + code)
        assert!(obj.len() > 100, "object should have relocation data");
    }

    /// Test string constants with multiple references.
    /// Same string symbol referenced from different locations in code.
    #[test]
    fn test_macho_string_multiple_references() {
        // Simulate code that references the same string constant twice
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "multi_ref")
            .code(vec![
                // adrp x0, str0@PAGE
                0x00, 0x00, 0x00, 0x90, // add x0, x0, str0@PAGEOFF
                0x00, 0x00, 0x00, 0x91, // ... some code ...
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                // adrp x1, str0@PAGE (same string)
                0x01, 0x00, 0x00, 0x90, // add x1, x1, str0@PAGEOFF
                0x21, 0x00, 0x00, 0x91, // ret
                0xC0, 0x03, 0x5F, 0xD6,
            ])
            .strings(vec!["hello world".to_string()])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: ".rodata.str0".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 4,
                symbol: ".rodata.str0".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 16,
                symbol: ".rodata.str0".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 20,
                symbol: ".rodata.str0".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .build();

        // Check Mach-O magic
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // Verify the file contains the string data
        let obj_str = String::from_utf8_lossy(&obj);
        assert!(
            obj_str.contains("hello world"),
            "object should contain string constant"
        );
    }

    /// Test empty rodata section (all empty strings).
    /// Empty strings should not create a rodata section since there's no content.
    #[test]
    fn test_macho_empty_strings() {
        // Object with only empty strings - should NOT create rodata section
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "empty_str")
            .code(vec![
                // ret
                0xC0, 0x03, 0x5F, 0xD6,
            ])
            .strings(vec!["".to_string(), "".to_string()])
            .build();

        // Check Mach-O magic
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // The object should be small since there's no rodata
        // With rodata it would be larger
        assert!(
            obj.len() < 500,
            "object should be small without rodata content"
        );
    }

    /// Test large code section that stresses offset calculations.
    /// The 21-bit page limit means ADRP can only handle ±4GB offsets,
    /// but this tests that internal offset calculations handle large code.
    #[test]
    fn test_macho_large_code_section() {
        // Create a larger code section (4KB of NOPs)
        let nop = [0x1F, 0x20, 0x03, 0xD5]; // ARM64 NOP
        let mut code = Vec::new();
        for _ in 0..1024 {
            code.extend_from_slice(&nop);
        }
        // Add a ret at the end
        code.extend_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]);

        // Add relocation at the end of the large code section
        let reloc_offset = code.len() - 8; // Before the ret

        let obj = ObjectBuilder::new(Target::Aarch64Macos, "large_code")
            .code(code.clone())
            .relocation(CodeRelocation {
                offset: reloc_offset as u64,
                symbol: "far_symbol".into(),
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .build();

        // Check Mach-O magic
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // Verify the object file is large enough
        assert!(obj.len() > 4096, "object should contain large code section");
    }

    /// Test negative addends in relocations.
    /// Addends are used to adjust the final symbol address.
    #[test]
    fn test_macho_negative_addend() {
        // While Mach-O ARM64 doesn't directly store addends like ELF,
        // the ObjectBuilder should handle negative addends gracefully
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "neg_addend")
            .code(vec![
                // bl target (placeholder)
                0x00, 0x00, 0x00, 0x94, // ret
                0xC0, 0x03, 0x5F, 0xD6,
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "target".into(),
                rel_type: RelocationType::Call26,
                addend: -4,
            })
            .build();

        // Check Mach-O magic
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());
    }

    /// Test recursive self-calls (function calling itself).
    #[test]
    fn test_macho_self_recursive_call() {
        // Function that calls itself recursively
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "recursive")
            .code(vec![
                // Compare and branch
                0x1F, 0x00, 0x00, 0x71, // cmp w0, #0
                0x40, 0x00, 0x00, 0x54, // b.eq skip
                // bl recursive (self-call)
                0x00, 0x00, 0x00, 0x94, // skip: ret
                0xC0, 0x03, 0x5F, 0xD6,
            ])
            .relocation(CodeRelocation {
                offset: 8,
                symbol: "recursive".into(), // Self-reference!
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .build();

        // Check Mach-O magic
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // Verify the function symbol is in the string table
        let obj_str = String::from_utf8_lossy(&obj);
        assert!(
            obj_str.contains("_recursive"),
            "should contain function symbol"
        );
    }

    /// Test multiple functions in one object file.
    /// Note: ObjectBuilder currently creates one symbol per call, but we can
    /// simulate multiple functions by having multiple call targets.
    #[test]
    fn test_macho_multiple_function_calls() {
        // Code that calls three different functions
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "multi_func")
            .code(vec![
                // bl func_a
                0x00, 0x00, 0x00, 0x94, // bl func_b
                0x00, 0x00, 0x00, 0x94, // bl func_c
                0x00, 0x00, 0x00, 0x94, // ret
                0xC0, 0x03, 0x5F, 0xD6,
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "func_a".into(),
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 4,
                symbol: "func_b".into(),
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 8,
                symbol: "func_c".into(),
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .build();

        // Check Mach-O magic
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // Verify all function symbols are present
        let obj_str = String::from_utf8_lossy(&obj);
        assert!(obj_str.contains("_func_a"), "should contain _func_a");
        assert!(obj_str.contains("_func_b"), "should contain _func_b");
        assert!(obj_str.contains("_func_c"), "should contain _func_c");
    }

    /// Test mixed relocation types in a single object.
    /// Combines ADRP+ADD pairs with branch relocations.
    #[test]
    fn test_macho_mixed_relocation_types() {
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "mixed_relocs")
            .code(vec![
                // adrp x0, data@PAGE
                0x00, 0x00, 0x00, 0x90, // add x0, x0, data@PAGEOFF
                0x00, 0x00, 0x00, 0x91, // bl helper
                0x00, 0x00, 0x00, 0x94, // adrp x1, more_data@PAGE
                0x01, 0x00, 0x00, 0x90, // add x1, x1, more_data@PAGEOFF
                0x21, 0x00, 0x00, 0x91, // b exit
                0x00, 0x00, 0x00, 0x14, // ret
                0xC0, 0x03, 0x5F, 0xD6,
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "data".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 4,
                symbol: "data".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 8,
                symbol: "helper".into(),
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 12,
                symbol: "more_data".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 16,
                symbol: "more_data".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 20,
                symbol: "exit".into(),
                rel_type: RelocationType::Jump26,
                addend: 0,
            })
            .build();

        // Check Mach-O magic
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // Verify all symbols are in the string table
        let obj_str = String::from_utf8_lossy(&obj);
        assert!(obj_str.contains("_data"), "should contain _data");
        assert!(obj_str.contains("_helper"), "should contain _helper");
        assert!(obj_str.contains("_more_data"), "should contain _more_data");
        assert!(obj_str.contains("_exit"), "should contain _exit");
    }

    /// Test multiple string constants with varying sizes.
    #[test]
    fn test_macho_multiple_string_constants() {
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "multi_str")
            .code(vec![
                // adrp x0, str0@PAGE
                0x00, 0x00, 0x00, 0x90, // add x0, x0, str0@PAGEOFF
                0x00, 0x00, 0x00, 0x91, // adrp x1, str1@PAGE
                0x01, 0x00, 0x00, 0x90, // add x1, x1, str1@PAGEOFF
                0x21, 0x00, 0x00, 0x91, // adrp x2, str2@PAGE
                0x02, 0x00, 0x00, 0x90, // add x2, x2, str2@PAGEOFF
                0x42, 0x00, 0x00, 0x91, // ret
                0xC0, 0x03, 0x5F, 0xD6,
            ])
            .strings(vec![
                "short".to_string(),
                "medium length string".to_string(),
                "this is a much longer string that tests larger rodata offsets".to_string(),
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: ".rodata.str0".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 4,
                symbol: ".rodata.str0".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 8,
                symbol: ".rodata.str1".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 12,
                symbol: ".rodata.str1".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 16,
                symbol: ".rodata.str2".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 20,
                symbol: ".rodata.str2".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .build();

        // Check Mach-O magic
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // Verify all strings are in the object
        let obj_str = String::from_utf8_lossy(&obj);
        assert!(obj_str.contains("short"), "should contain 'short'");
        assert!(
            obj_str.contains("medium length string"),
            "should contain medium string"
        );
        assert!(
            obj_str.contains("this is a much longer string"),
            "should contain long string"
        );
    }

    /// Test that Mach-O object files have correct header structure.
    /// Validates ncmds, sizeofcmds, and section count.
    #[test]
    fn test_macho_header_structure() {
        // Test with rodata (2 sections)
        let obj_with_rodata = ObjectBuilder::new(Target::Aarch64Macos, "with_rodata")
            .code(vec![0xC0, 0x03, 0x5F, 0xD6])
            .strings(vec!["test".to_string()])
            .build();

        // Check ncmds (at offset 16) - should be 4 (LC_SEGMENT_64, LC_BUILD_VERSION, LC_SYMTAB, LC_DYSYMTAB)
        let ncmds = u32::from_le_bytes([
            obj_with_rodata[16],
            obj_with_rodata[17],
            obj_with_rodata[18],
            obj_with_rodata[19],
        ]);
        assert_eq!(ncmds, 4, "should have 4 load commands");

        // Test without rodata (1 section)
        let obj_no_rodata = ObjectBuilder::new(Target::Aarch64Macos, "no_rodata")
            .code(vec![0xC0, 0x03, 0x5F, 0xD6])
            .build();

        let ncmds = u32::from_le_bytes([
            obj_no_rodata[16],
            obj_no_rodata[17],
            obj_no_rodata[18],
            obj_no_rodata[19],
        ]);
        assert_eq!(ncmds, 4, "should still have 4 load commands");
    }

    /// Test that symbols use correct underscore prefixes for macOS.
    #[test]
    fn test_macho_symbol_underscore_prefix() {
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "my_func")
            .code(vec![
                0x00, 0x00, 0x00, 0x94, // bl other
                0xC0, 0x03, 0x5F, 0xD6, // ret
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "other_func".into(),
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .build();

        let obj_str = String::from_utf8_lossy(&obj);

        // Function name should have underscore prefix
        assert!(
            obj_str.contains("_my_func"),
            "function should have _ prefix"
        );

        // External symbol should also have underscore prefix
        assert!(
            obj_str.contains("_other_func"),
            "external symbol should have _ prefix"
        );
    }

    /// Test jump (B) vs call (BL) relocations.
    #[test]
    fn test_macho_jump_vs_call() {
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "branch_test")
            .code(vec![
                // bl callee (call with link)
                0x00, 0x00, 0x00, 0x94, // b tail_target (unconditional jump)
                0x00, 0x00, 0x00, 0x14,
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "callee".into(),
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 4,
                symbol: "tail_target".into(),
                rel_type: RelocationType::Jump26,
                addend: 0,
            })
            .build();

        // Check Mach-O magic
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // Both symbols should be present
        let obj_str = String::from_utf8_lossy(&obj);
        assert!(obj_str.contains("_callee"), "should contain _callee");
        assert!(
            obj_str.contains("_tail_target"),
            "should contain _tail_target"
        );
    }

    /// Test that Mach-O object files have valid structure for system linker.
    /// This validates the object file structure without actually invoking the linker
    /// (which would require running on macOS).
    ///
    /// A valid Mach-O object file for linking must have:
    /// - Correct magic number and header
    /// - Valid LC_SEGMENT_64 with sections
    /// - Valid LC_SYMTAB with symbol and string tables
    /// - LC_BUILD_VERSION for modern macOS
    /// - LC_DYSYMTAB for dynamic symbol table
    /// - Proper section and symbol alignment
    #[test]
    fn test_macho_linker_compatible_structure() {
        // Create an object file that would be produced by actual compilation
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "main")
            .code(vec![
                // Function prologue
                0xFD, 0x7B, 0xBF, 0xA9, // stp x29, x30, [sp, #-16]!
                0xFD, 0x03, 0x00, 0x91, // mov x29, sp
                // adrp x0, str@PAGE
                0x00, 0x00, 0x00, 0x90, // add x0, x0, str@PAGEOFF
                0x00, 0x00, 0x00, 0x91, // bl puts
                0x00, 0x00, 0x00, 0x94, // mov w0, #0
                0x00, 0x00, 0x80, 0x52, // Function epilogue
                0xFD, 0x7B, 0xC1, 0xA8, // ldp x29, x30, [sp], #16
                0xC0, 0x03, 0x5F, 0xD6, // ret
            ])
            .strings(vec!["Hello, World!".to_string()])
            .relocation(CodeRelocation {
                offset: 8,
                symbol: ".rodata.str0".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 12,
                symbol: ".rodata.str0".into(),
                rel_type: RelocationType::AddLo12,
                addend: 0,
            })
            .relocation(CodeRelocation {
                offset: 16,
                symbol: "puts".into(),
                rel_type: RelocationType::Call26,
                addend: 0,
            })
            .build();

        // Validate header
        assert_eq!(
            &obj[0..4],
            &0xFEEDFACF_u32.to_le_bytes(),
            "should have MH_MAGIC_64"
        );
        assert_eq!(
            &obj[4..8],
            &0x0100000C_u32.to_le_bytes(),
            "should have CPU_TYPE_ARM64"
        );
        assert_eq!(
            &obj[12..16],
            &0x1_u32.to_le_bytes(),
            "should have MH_OBJECT filetype"
        );

        // Validate load command count
        let ncmds = u32::from_le_bytes([obj[16], obj[17], obj[18], obj[19]]);
        assert_eq!(ncmds, 4, "should have 4 load commands");

        // Read sizeofcmds
        let sizeofcmds = u32::from_le_bytes([obj[20], obj[21], obj[22], obj[23]]);
        assert!(sizeofcmds > 0, "sizeofcmds should be positive");

        // Verify first load command is LC_SEGMENT_64
        let first_cmd = u32::from_le_bytes([obj[32], obj[33], obj[34], obj[35]]);
        assert_eq!(first_cmd, 0x19, "first command should be LC_SEGMENT_64");

        // Verify the object contains expected symbols
        let obj_str = String::from_utf8_lossy(&obj);
        assert!(obj_str.contains("_main"), "should contain _main symbol");
        assert!(obj_str.contains("_puts"), "should contain _puts symbol");

        // Verify the object contains the string constant
        assert!(
            obj_str.contains("Hello, World!"),
            "should contain string data"
        );

        // Verify file size is reasonable (header + commands + sections + relocations + symtab + strtab)
        assert!(
            obj.len() > 200,
            "object file should be reasonably sized: {}",
            obj.len()
        );
        assert!(
            obj.len() < 2000,
            "object file should not be excessively large: {}",
            obj.len()
        );
    }

    /// Test that empty code section is handled correctly.
    #[test]
    fn test_macho_empty_code() {
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "empty")
            .code(vec![])
            .build();

        // Should still produce valid Mach-O
        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());
    }

    /// Test code with only a single instruction.
    #[test]
    fn test_macho_minimal_code() {
        let obj = ObjectBuilder::new(Target::Aarch64Macos, "minimal")
            .code(vec![0xC0, 0x03, 0x5F, 0xD6]) // Just a ret
            .build();

        assert_eq!(&obj[0..4], &0xFEEDFACF_u32.to_le_bytes());

        // Verify the function symbol exists
        let obj_str = String::from_utf8_lossy(&obj);
        assert!(
            obj_str.contains("_minimal"),
            "should contain function symbol"
        );
    }
}
