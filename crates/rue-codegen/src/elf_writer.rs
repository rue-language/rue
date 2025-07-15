use std::collections::HashMap;

/// ELF file writer for x86-64 Linux executables
pub struct ElfWriter {
    base_addr: u64,
    page_size: u64,
}

impl ElfWriter {
    pub fn new() -> Self {
        Self {
            base_addr: 0x400000,
            page_size: 0x1000,
        }
    }

    /// Generate a complete ELF executable
    pub fn generate_elf(&self, machine_code: &[u8], symbols: &HashMap<String, usize>) -> Vec<u8> {
        let mut elf = Vec::new();

        // Find the _start symbol position
        let start_offset = symbols
            .get("_start")
            .or_else(|| symbols.get("L999")) // Legacy label ID for _start
            .copied()
            .unwrap_or(0);

        let entry_point = self.base_addr + 0x78 + start_offset as u64;

        // ELF Header
        self.write_elf_header(&mut elf, entry_point);

        // Program Header
        self.write_program_header(&mut elf, machine_code.len());

        // Machine code
        elf.extend_from_slice(machine_code);

        elf
    }

    fn write_elf_header(&self, elf: &mut Vec<u8>, entry_point: u64) {
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
        elf.extend_from_slice(&64u64.to_le_bytes()); // Program header offset
        elf.extend_from_slice(&0u64.to_le_bytes()); // Section header offset (none)
        elf.extend_from_slice(&0u32.to_le_bytes()); // Processor-specific flags
        elf.extend_from_slice(&64u16.to_le_bytes()); // ELF header size
        elf.extend_from_slice(&56u16.to_le_bytes()); // Program header entry size
        elf.extend_from_slice(&1u16.to_le_bytes()); // Program header entry count
        elf.extend_from_slice(&0u16.to_le_bytes()); // Section header entry size
        elf.extend_from_slice(&0u16.to_le_bytes()); // Section header entry count
        elf.extend_from_slice(&0u16.to_le_bytes()); // Section name string table index
    }

    fn write_program_header(&self, elf: &mut Vec<u8>, code_size: usize) {
        let headers_size = 120u64; // ELF header (64) + Program header (56)
        let total_file_size = headers_size + code_size as u64;

        // Memory size should be page-aligned for proper loading
        let total_memory_size = if total_file_size > self.page_size {
            total_file_size.div_ceil(self.page_size) * self.page_size
        } else {
            total_file_size
        };

        // Program header for LOAD segment
        elf.extend_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        elf.extend_from_slice(&5u32.to_le_bytes()); // PF_R | PF_X (readable, executable)
        elf.extend_from_slice(&0u64.to_le_bytes()); // Offset in file
        elf.extend_from_slice(&self.base_addr.to_le_bytes()); // Virtual address
        elf.extend_from_slice(&self.base_addr.to_le_bytes()); // Physical address
        elf.extend_from_slice(&total_file_size.to_le_bytes()); // Size in file
        elf.extend_from_slice(&total_memory_size.to_le_bytes()); // Size in memory
        elf.extend_from_slice(&self.page_size.to_le_bytes()); // Alignment
    }
}

impl Default for ElfWriter {
    fn default() -> Self {
        Self::new()
    }
}
