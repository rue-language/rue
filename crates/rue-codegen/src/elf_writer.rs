use std::collections::HashMap;

// ELF structure sizes
const EHDR_SIZE: usize = 0x40; // 64 bytes
const PHDR_SIZE: usize = 0x38; // 56 bytes
const SHDR_SIZE: usize = 0x40; // 64 bytes

// Page size for alignment
const PAGE_SIZE: u64 = 0x1000; // 4 KiB

// Segment flags
const PF_X: u32 = 0x1; // Execute
#[allow(dead_code)]
const PF_W: u32 = 0x2; // Write
const PF_R: u32 = 0x4; // Read
const P_FLAGS_RX: u32 = PF_R | PF_X; // Read + Execute (no write for text segment)

// Section types
#[allow(dead_code)]
const SHT_NULL: u32 = 0;
const SHT_PROGBITS: u32 = 1;
const SHT_STRTAB: u32 = 3;

// Section flags
const SHF_ALLOC: u64 = 0x2;
const SHF_EXECINSTR: u64 = 0x4;

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

    /// Generate a complete ELF executable
    pub fn generate_elf(&self, machine_code: &[u8], symbols: &HashMap<String, usize>) -> Vec<u8> {
        let mut elf = Vec::new();

        // Calculate sizes and offsets
        let headers_size = EHDR_SIZE + PHDR_SIZE;

        // Section header string table
        let shstrtab = b"\0.text\0.shstrtab\0";
        let text_name_offset = 1; // ".text" starts at offset 1
        let shstrtab_name_offset = 7; // ".shstrtab" starts at offset 7

        // Layout:
        // - ELF header (64 bytes)
        // - Program header (56 bytes)
        // - Machine code (aligned to 16 bytes)
        // - String table
        // - Section headers (at end)

        // Align code_offset to 16 bytes
        let code_offset = align_up(headers_size, 16);
        let code_size = machine_code.len();
        let code_end = code_offset + code_size;

        // Align string table to 8 bytes
        let shstrtab_offset = align_up(code_end, 8);
        let shstrtab_size = shstrtab.len();

        // Section headers at the end, aligned to 8 bytes
        let section_headers_offset = align_up(shstrtab_offset + shstrtab_size, 8);

        // Find the _start symbol position
        debug_assert!(
            symbols.contains_key("_start"),
            "_start symbol must be present in symbols table"
        );
        let start_offset = symbols
            .get("_start")
            .copied()
            .expect("_start symbol not found");

        let entry_point = self.base_addr + code_offset as u64 + start_offset as u64;

        // ELF Header
        self.write_elf_header(&mut elf, entry_point, section_headers_offset as u64);

        // Program Header
        self.write_program_header(&mut elf, machine_code.len());

        // Padding to align code
        if elf.len() < code_offset {
            elf.resize(code_offset, 0);
        }

        // Machine code
        elf.extend_from_slice(machine_code);

        // Padding before string table
        if elf.len() < shstrtab_offset {
            elf.resize(shstrtab_offset, 0);
        }

        // String table
        elf.extend_from_slice(shstrtab);

        // Padding before section headers
        if elf.len() < section_headers_offset {
            elf.resize(section_headers_offset, 0);
        }

        // Section headers
        // Section 0: NULL section (required)
        self.write_null_section_header(&mut elf);

        // Section 1: .text
        self.write_text_section_header(
            &mut elf,
            code_offset as u64,
            code_size as u64,
            text_name_offset,
        );

        // Section 2: .shstrtab
        self.write_shstrtab_section_header(
            &mut elf,
            shstrtab_offset as u64,
            shstrtab_size as u64,
            shstrtab_name_offset,
        );

        elf
    }

    fn write_elf_header(&self, elf: &mut Vec<u8>, entry_point: u64, section_headers_offset: u64) {
        // ELF identification
        elf.extend_from_slice(&[0x7f, 0x45, 0x4c, 0x46]); // ELF magic
        elf.push(0x02); // 64-bit
        elf.push(0x01); // Little endian
        elf.push(0x01); // ELF version
        elf.push(0x00); // System V ABI
        elf.extend_from_slice(&[0; 8]); // Padding

        // ELF header fields
        elf.extend_from_slice(&2u16.to_le_bytes()); // ET_EXEC - Executable file
        elf.extend_from_slice(&0x3eu16.to_le_bytes()); // EM_X86_64 - AMD x86-64
        elf.extend_from_slice(&1u32.to_le_bytes()); // EV_CURRENT - Version
        elf.extend_from_slice(&entry_point.to_le_bytes()); // Entry point address
        elf.extend_from_slice(&(EHDR_SIZE as u64).to_le_bytes()); // Program header offset
        elf.extend_from_slice(&section_headers_offset.to_le_bytes()); // Section header offset
        elf.extend_from_slice(&0u32.to_le_bytes()); // Processor-specific flags
        elf.extend_from_slice(&(EHDR_SIZE as u16).to_le_bytes()); // ELF header size
        elf.extend_from_slice(&(PHDR_SIZE as u16).to_le_bytes()); // Program header entry size
        elf.extend_from_slice(&1u16.to_le_bytes()); // Program header entry count
        elf.extend_from_slice(&(SHDR_SIZE as u16).to_le_bytes()); // Section header entry size
        elf.extend_from_slice(&3u16.to_le_bytes()); // Section header entry count
        elf.extend_from_slice(&2u16.to_le_bytes()); // Section name string table index (.shstrtab)
    }

    fn write_program_header(&self, elf: &mut Vec<u8>, code_size: usize) {
        let headers_size = (EHDR_SIZE + PHDR_SIZE) as u64;
        let total_file_size = headers_size + code_size as u64;

        // Memory size should be page-aligned for proper loading
        let total_memory_size = if total_file_size > PAGE_SIZE {
            total_file_size.div_ceil(PAGE_SIZE) * PAGE_SIZE
        } else {
            total_file_size
        };

        // Program header for LOAD segment
        elf.extend_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        elf.extend_from_slice(&P_FLAGS_RX.to_le_bytes()); // PF_R | PF_X (readable, executable - no write)
        elf.extend_from_slice(&0u64.to_le_bytes()); // Offset in file
        elf.extend_from_slice(&self.base_addr.to_le_bytes()); // Virtual address
        elf.extend_from_slice(&self.base_addr.to_le_bytes()); // Physical address
        elf.extend_from_slice(&total_file_size.to_le_bytes()); // Size in file
        elf.extend_from_slice(&total_memory_size.to_le_bytes()); // Size in memory
        elf.extend_from_slice(&PAGE_SIZE.to_le_bytes()); // Alignment
    }

    fn write_null_section_header(&self, elf: &mut Vec<u8>) {
        // NULL section (all zeros)
        elf.extend_from_slice(&[0; SHDR_SIZE]);
    }

    fn write_text_section_header(
        &self,
        elf: &mut Vec<u8>,
        offset: u64,
        size: u64,
        name_offset: u32,
    ) {
        elf.extend_from_slice(&name_offset.to_le_bytes()); // sh_name
        elf.extend_from_slice(&SHT_PROGBITS.to_le_bytes()); // sh_type: SHT_PROGBITS
        elf.extend_from_slice(&(SHF_ALLOC | SHF_EXECINSTR).to_le_bytes()); // sh_flags: SHF_ALLOC | SHF_EXECINSTR
        elf.extend_from_slice(&(self.base_addr + offset).to_le_bytes()); // sh_addr
        elf.extend_from_slice(&offset.to_le_bytes()); // sh_offset
        elf.extend_from_slice(&size.to_le_bytes()); // sh_size
        elf.extend_from_slice(&0u32.to_le_bytes()); // sh_link
        elf.extend_from_slice(&0u32.to_le_bytes()); // sh_info
        elf.extend_from_slice(&16u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&0u64.to_le_bytes()); // sh_entsize
    }

    fn write_shstrtab_section_header(
        &self,
        elf: &mut Vec<u8>,
        offset: u64,
        size: u64,
        name_offset: u32,
    ) {
        elf.extend_from_slice(&name_offset.to_le_bytes()); // sh_name
        elf.extend_from_slice(&SHT_STRTAB.to_le_bytes()); // sh_type: SHT_STRTAB
        elf.extend_from_slice(&0u64.to_le_bytes()); // sh_flags
        elf.extend_from_slice(&0u64.to_le_bytes()); // sh_addr
        elf.extend_from_slice(&offset.to_le_bytes()); // sh_offset
        elf.extend_from_slice(&size.to_le_bytes()); // sh_size
        elf.extend_from_slice(&0u32.to_le_bytes()); // sh_link
        elf.extend_from_slice(&0u32.to_le_bytes()); // sh_info
        elf.extend_from_slice(&1u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&0u64.to_le_bytes()); // sh_entsize
    }
}

/// Align a value up to the specified alignment
fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

impl Default for ElfWriter {
    fn default() -> Self {
        Self::new()
    }
}
