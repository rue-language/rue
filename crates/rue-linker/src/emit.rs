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

    /// Build the ELF64 relocatable object file.
    pub fn build(self) -> Vec<u8> {
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
}

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::ObjectFile;
    use rue_target::Target;

    #[test]
    fn test_simple_object() {
        // Create a simple object with just a ret instruction
        let obj = ObjectBuilder::new(Target::host(), "main")
            .code(vec![0xC3]) // ret
            .build();

        // Check ELF magic
        assert_eq!(&obj[0..4], b"\x7FELF");
        // Check it's relocatable
        assert_eq!(obj[16], 1); // ET_REL
    }

    #[test]
    fn test_object_with_relocation() {
        // Create object that calls an external function
        let obj = ObjectBuilder::new(Target::host(), "main")
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
        // Create an object and verify we can parse it back
        let built = ObjectBuilder::new(Target::host(), "test_func")
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
        let built = ObjectBuilder::new(Target::host(), "caller")
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
        let built = ObjectBuilder::new(Target::host(), "multi_caller")
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
        let built = ObjectBuilder::new(Target::host(), "empty_func")
            .code(vec![])
            .build();

        let parsed = ObjectFile::parse(&built).expect("should parse empty object");
        let sym = parsed.find_symbol("empty_func");
        assert!(sym.is_some());
    }

    #[test]
    fn test_various_relocation_types() {
        let built = ObjectBuilder::new(Target::host(), "reloc_test")
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
