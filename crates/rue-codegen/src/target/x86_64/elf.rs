use std::collections::HashMap;

/// ELF file writer for x86-64 Linux executables
pub struct ElfWriter {
    base_addr: u64,
}

impl ElfWriter {
    pub fn new() -> Self {
        Self {
            base_addr: 0x400000,
        }
    }

    /// Generate a complete ELF executable using the object crate
    pub fn generate_elf(&self, machine_code: &[u8], symbols: &HashMap<String, usize>) -> Vec<u8> {
        self.generate_elf_with_sections(machine_code, symbols, &[], 0)
    }

    /// Generate ELF executable with support for .data and .bss sections
    pub fn generate_elf_with_sections(
        &self,
        machine_code: &[u8],
        symbols: &HashMap<String, usize>,
        data_section: &[u8],
        bss_size: usize,
    ) -> Vec<u8> {
        // Validate that _start symbol exists
        debug_assert!(
            symbols.contains_key("_start"),
            "_start symbol must be present in symbols table"
        );

        // Step 1: Use object crate to generate a proper relocatable object
        // Use the working implementation directly with the actual machine code and sections
        let start_offset = *symbols
            .get("_start")
            .expect("_start symbol must be present in symbols table");

        self.create_executable_elf(machine_code, data_section, bss_size, start_offset)
    }

    /// Create a proper executable ELF file (ET_EXEC) with program headers
    fn create_executable_elf(
        &self,
        machine_code: &[u8],
        data_section: &[u8],
        bss_size: usize,
        start_offset: usize,
    ) -> Vec<u8> {
        const ELF_HEADER_SIZE: usize = 64;
        const PROGRAM_HEADER_SIZE: usize = 56;
        const PAGE_SIZE: usize = 0x1000;

        // Calculate layout
        let headers_size = ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE;
        let text_offset = align_up(headers_size, 16);
        let text_size = machine_code.len();
        let data_offset = align_up(text_offset + text_size, 8);
        let data_size = data_section.len();
        let file_size = if data_size > 0 {
            data_offset + data_size
        } else {
            text_offset + text_size
        };
        let memory_size = align_up(file_size + bss_size, PAGE_SIZE);

        let mut elf = Vec::with_capacity(file_size);

        // ELF Header
        elf.extend_from_slice(&[
            0x7f, b'E', b'L', b'F', // Magic
            2,    // 64-bit
            1,    // Little endian
            1,    // Current version
            0,    // System V ABI
            0, 0, 0, 0, 0, 0, 0, 0, // Padding
        ]);

        // e_type: ET_EXEC (2)
        elf.extend_from_slice(&2u16.to_le_bytes());
        // e_machine: EM_X86_64 (62)
        elf.extend_from_slice(&62u16.to_le_bytes());
        // e_version
        elf.extend_from_slice(&1u32.to_le_bytes());
        // e_entry - entry point address
        elf.extend_from_slice(
            &(self.base_addr + text_offset as u64 + start_offset as u64).to_le_bytes(),
        );
        // e_phoff - program header offset
        elf.extend_from_slice(&(ELF_HEADER_SIZE as u64).to_le_bytes());
        // e_shoff - section header offset (0 = none)
        elf.extend_from_slice(&0u64.to_le_bytes());
        // e_flags
        elf.extend_from_slice(&0u32.to_le_bytes());
        // e_ehsize - ELF header size
        elf.extend_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes());
        // e_phentsize - program header entry size
        elf.extend_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes());
        // e_phnum - number of program headers
        elf.extend_from_slice(&1u16.to_le_bytes());
        // e_shentsize - section header entry size
        elf.extend_from_slice(&0u16.to_le_bytes());
        // e_shnum - number of section headers
        elf.extend_from_slice(&0u16.to_le_bytes());
        // e_shstrndx - section header string table index
        elf.extend_from_slice(&0u16.to_le_bytes());

        // Program Header (single PT_LOAD segment)
        // p_type: PT_LOAD (1)
        elf.extend_from_slice(&1u32.to_le_bytes());
        // p_flags: PF_X | PF_W | PF_R (7) - RWX permissions
        elf.extend_from_slice(&7u32.to_le_bytes());
        // p_offset - offset in file
        elf.extend_from_slice(&0u64.to_le_bytes());
        // p_vaddr - virtual address
        elf.extend_from_slice(&self.base_addr.to_le_bytes());
        // p_paddr - physical address (same as vaddr)
        elf.extend_from_slice(&self.base_addr.to_le_bytes());
        // p_filesz - size in file
        elf.extend_from_slice(&(file_size as u64).to_le_bytes());
        // p_memsz - size in memory (includes BSS)
        elf.extend_from_slice(&(memory_size as u64).to_le_bytes());
        // p_align - alignment
        elf.extend_from_slice(&(PAGE_SIZE as u64).to_le_bytes());

        // Pad to text section start
        while elf.len() < text_offset {
            elf.push(0);
        }

        // Add .text section (machine code)
        elf.extend_from_slice(machine_code);

        // Add .data section if present
        if data_size > 0 {
            // Pad to data section start
            while elf.len() < data_offset {
                elf.push(0);
            }
            elf.extend_from_slice(data_section);
        }

        elf
    }
}

/// Align a value up to the specified alignment
fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

impl Default for ElfWriter {
    fn default() -> Self {
        Self::new()
    }
}
