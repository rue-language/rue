// Object file linker for the Rue compiler
//
// This module provides functionality to link external assembly object files
// into the final Rue executable. It supports basic ELF64 object file parsing,
// symbol resolution, and relocation application.

use crate::CodegenError;
use object::SectionKind;
use std::collections::HashMap;

pub mod archive;
pub mod asm_object;
pub mod instruction_rewriter;
pub mod linker;
pub mod object_builder;
pub mod object_file;
pub mod relocation;
pub mod symbol;
pub mod two_pass_relocator;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod integration_tests;

pub use object_builder::{
    ObjectFileBuilder, create_user_object, create_user_object_with_externals,
};
pub use object_file::ObjectFile;
pub use relocation::{RelocationEntry, RelocationKey, RelocationKind};
pub use symbol::{Symbol, SymbolKind, SymbolSource, SymbolTable};
pub use two_pass_relocator::{OffsetRemapping, TwoPassRelocator};

/// A minimal object file linker that can link assembly object files into Rue executables.
pub struct Linker {
    object_files: Vec<ObjectFile>,
    merged_sections: HashMap<String, MergedSection>,
    symbol_table: SymbolTable,
    /// Maps original section names to their offset within the merged section
    section_offsets: HashMap<String, SectionOffset>,
}

/// Represents a merged section from multiple object files
#[derive(Debug, Clone)]
pub struct MergedSection {
    pub name: String,
    pub kind: SectionKind,
    pub data: Vec<u8>,
    pub alignment: u64,
    pub base_address: u64,
}

/// Represents the location of an original section within a merged section
#[derive(Debug, Clone, PartialEq)]
pub struct SectionOffset {
    pub merged_section_name: String,
    pub offset_within_merged: u64,
    /// Amount of alignment padding added before this section's data
    pub alignment_padding: u64,
}

/// Represents the layout of an ELF executable
#[derive(Debug)]
struct ElfLayout {
    base_address: u64,
    page_size: usize,
    text_offset: usize,
    text_size: usize,
    rodata_offset: usize,
    rodata_size: usize,
    shstrtab_offset: usize,
    section_headers_offset: usize,
    file_size: usize,
    memory_size: usize,
    num_sections: u16,
    rodata_section_idx: u16,
    bss_section_idx: u16,
    shstrtab_section_idx: u16,
}

/// Result of the linking process
#[derive(Debug, Clone)]
pub struct LinkedResult {
    pub text_section: Vec<u8>,
    pub rodata_section: Vec<u8>,
    pub bss_size: u64,
    pub symbols: SymbolTable,
}

/// Result of linking to an executable
#[derive(Debug, Clone)]
pub struct LinkedExecutable {
    pub executable: Vec<u8>,
}

impl Default for Linker {
    fn default() -> Self {
        Self::new()
    }
}

impl Linker {
    /// Create a new empty linker
    pub fn new() -> Self {
        Self {
            object_files: Vec::new(),
            merged_sections: HashMap::new(),
            symbol_table: SymbolTable::new(),
            section_offsets: HashMap::new(),
        }
    }

    /// Builder-style method to add an object file and return self
    pub fn with_object_file(mut self, name: String, data: &[u8]) -> Result<Self, CodegenError> {
        self.add_object_file(name, data)?;
        Ok(self)
    }

    /// Builder-style method to add an object file from path and return self
    pub fn with_object_file_from_path(mut self, path: &str) -> Result<Self, CodegenError> {
        self.add_object_file_from_path(path)?;
        Ok(self)
    }

    /// Builder-style method to add user code and return self
    pub fn with_user_code(
        mut self,
        code: &[u8],
        symbols: &HashMap<String, usize>,
    ) -> Result<Self, CodegenError> {
        self.add_user_code_object(code, symbols)?;
        Ok(self)
    }

    /// Add an object file to the linker from raw bytes
    pub fn add_object_file(&mut self, name: String, data: &[u8]) -> Result<(), CodegenError> {
        let object_file = ObjectFile::parse(name, data)?;
        self.object_files.push(object_file);
        Ok(())
    }

    /// Add user code as an object file
    /// This is a convenience method that creates an object file from user code
    pub fn add_user_code_object(
        &mut self,
        code: &[u8],
        symbols: &HashMap<String, usize>,
    ) -> Result<(), CodegenError> {
        // Create an object file from the user code
        let object_data = create_user_object(code, symbols, &[], 0)?;
        self.add_object_file("user_code.o".to_string(), &object_data)
    }

    /// Add user code as an object file with external symbol references
    pub fn add_user_code_object_with_externals(
        &mut self,
        code: &[u8],
        symbols: &HashMap<String, usize>,
        external_refs: &[(String, usize)],
    ) -> Result<(), CodegenError> {
        tracing::info!(
            "Creating user object file: code_size={}, symbols={}, external_refs={}",
            code.len(),
            symbols.len(),
            external_refs.len()
        );

        // Create an object file from the user code with external references
        let object_data = create_user_object_with_externals(code, symbols, &[], 0, external_refs)?;

        tracing::info!(
            "Created user object file: object_data_size={}",
            object_data.len()
        );

        // Debug: Write the object file for inspection (disabled temporarily)
        // if let Err(e) = std::fs::write("/tmp/user_code.o", &object_data) {
        //     tracing::warn!("Failed to write debug object file: {}", e);
        // } else {
        //     tracing::debug!("Wrote user object file to /tmp/user_code.o for inspection");
        // }

        tracing::info!("Adding user object file to linker");

        self.add_object_file("user_code.o".to_string(), &object_data)
    }

    /// Add an object file to the linker from a file path
    pub fn add_object_file_from_path(&mut self, path: &str) -> Result<(), CodegenError> {
        tracing::debug!("Loading object/archive file from: {}", path);
        let data = std::fs::read(path).map_err(|e| {
            let error_msg = format!("Failed to read file {}: {}", path, e);
            tracing::debug!("{}", error_msg);
            CodegenError::InvalidOperation(error_msg)
        })?;
        let name = path.to_string();

        // Check if this is an archive file (.a)
        if path.ends_with(".a") {
            tracing::debug!("File {} is an archive, using add_archive", path);
            self.add_archive(name, &data)
        } else {
            tracing::debug!("File {} is an object file, using add_object_file", path);
            self.add_object_file(name, &data)
        }
    }

    /// Add an archive file (.a) containing multiple object files
    pub fn add_archive(&mut self, name: String, data: &[u8]) -> Result<(), CodegenError> {
        use object::read::archive::ArchiveFile;

        tracing::debug!("Loading archive: {} ({} bytes)", name, data.len());
        let archive = ArchiveFile::parse(data).map_err(|e| {
            CodegenError::InvalidOperation(format!("Failed to parse archive file: {}", e))
        })?;

        tracing::debug!("Archive parsed successfully: {}", name);

        // Extract and add each object file from the archive
        for member in archive.members() {
            let member = member.map_err(|e| {
                CodegenError::InvalidOperation(format!("Failed to read archive member: {}", e))
            })?;

            let member_name = std::str::from_utf8(member.name())
                .unwrap_or("<invalid>")
                .to_string();

            let member_data = member.data(data).map_err(|e| {
                CodegenError::InvalidOperation(format!(
                    "Failed to extract archive member data: {}",
                    e
                ))
            })?;

            // Parse and add the object file
            // Skip if it's not a valid object file (e.g., symbol index)
            tracing::debug!("Processing archive member: {}", member_name);
            match ObjectFile::parse(format!("{}({})", name, member_name), member_data) {
                Ok(object_file) => {
                    tracing::debug!(
                        "Successfully parsed object file: {} with {} sections",
                        member_name,
                        object_file.sections.len()
                    );
                    // Check for BSS sections
                    for section in &object_file.sections {
                        if section.name.starts_with(".bss") {
                            tracing::debug!(
                                "Archive member {} has BSS section: {} (size: {})",
                                member_name,
                                section.name,
                                section.data.len()
                            );
                        }
                    }
                    self.object_files.push(object_file);
                }
                Err(e) => {
                    tracing::debug!(
                        "Skipping archive member {} (parse error: {})",
                        member_name,
                        e
                    );
                }
            }
        }

        Ok(())
    }

    /// Get a reference to the symbol table
    pub fn symbol_table(&self) -> &SymbolTable {
        &self.symbol_table
    }

    /// Get a mutable reference to the symbol table
    pub fn symbol_table_mut(&mut self) -> &mut SymbolTable {
        &mut self.symbol_table
    }

    /// Link all added object files together
    pub fn link(&mut self) -> Result<LinkedResult, CodegenError> {
        // Step 1: Merge sections from all object files
        self.merge_sections()?;

        // Check text section size after merge
        if let Some(text) = self.merged_sections.get(".text") {
            tracing::warn!(
                "STEP 1 - After merge_sections, .text size is 0x{:x}",
                text.data.len()
            );
        }

        // Step 2: Build unified symbol table
        self.build_symbol_table()?;
        if let Some(text) = self.merged_sections.get(".text") {
            tracing::warn!(
                "STEP 2 - After build_symbol_table, .text size is 0x{:x}",
                text.data.len()
            );
        }

        // Step 3: Assign section base addresses
        self.assign_section_addresses()?;
        if let Some(text) = self.merged_sections.get(".text") {
            tracing::warn!(
                "STEP 3 - After assign_section_addresses, .text size is 0x{:x}",
                text.data.len()
            );
        }

        // Step 4: Update symbol addresses based on final section layout
        self.update_symbol_addresses()?;
        if let Some(text) = self.merged_sections.get(".text") {
            tracing::warn!(
                "STEP 4 - After update_symbol_addresses, .text size is 0x{:x}",
                text.data.len()
            );
        }

        // Step 5: Apply relocations
        self.apply_relocations()?;

        // Check text section size before build_result
        if let Some(text) = self.merged_sections.get(".text") {
            tracing::warn!(
                "STEP 6 - Before build_result, .text size is 0x{:x}",
                text.data.len()
            );
        }

        // Step 6: Build final result
        Ok(self.build_result())
    }

    /// Fix missing call instruction relocations in all functions
    fn fix_missing_call_relocations(&self, linked: &mut LinkedResult) -> Result<(), CodegenError> {
        tracing::debug!("Scanning entire text section for missing call relocations");

        // Scan the entire text section for unrelocated call instructions
        let mut fixes = Vec::new();
        for i in 0..linked.text_section.len().saturating_sub(4) {
            if linked.text_section[i] == 0xe8 {
                // Found a call instruction
                let call_offset = i;
                let displacement_bytes = &linked.text_section[i + 1..i + 5];
                let displacement = u32::from_le_bytes([
                    displacement_bytes[0],
                    displacement_bytes[1],
                    displacement_bytes[2],
                    displacement_bytes[3],
                ]) as i32;

                // Check if this looks like an unrelocated call (very large displacement)
                if displacement.abs() > 0x10000 {
                    // > 64KB indicates likely unrelocated call
                    tracing::debug!(
                        "Found call at 0x{:x} with displacement 0x{:x} (abs: {})",
                        call_offset,
                        displacement,
                        displacement.abs()
                    );

                    // Try to find appropriate symbol to fix this call
                    // Calculate what the original target address was supposed to be
                    let pc = call_offset + 5; // PC after the call instruction
                    let original_target = (pc as i32).wrapping_add(displacement) as u32 as u64;

                    tracing::debug!(
                        "Original call target was supposed to be 0x{:x}",
                        original_target
                    );

                    // Find the closest symbol to the original target address
                    let mut target_symbol = None;
                    let mut target_name = "";
                    let mut closest_distance = u64::MAX;

                    // List of common runtime functions to check
                    let runtime_functions = [
                        "__rue_main",
                        "main",
                        "__rue_exit",
                        "__rue_heap_init",
                        "__rue_println_i64",
                        "__rue_println_i32",
                        "__rue_write_byte",
                        "__rue_alloc",
                        "__rue_input",
                        "_start",
                    ];

                    for func_name in &runtime_functions {
                        if let Some(symbol) = linked.symbols.get_symbol(func_name) {
                            let distance = if symbol.address >= original_target {
                                symbol.address - original_target
                            } else {
                                original_target - symbol.address
                            };

                            if distance < closest_distance {
                                closest_distance = distance;
                                target_symbol = Some(symbol);
                                target_name = func_name;
                            }
                        }
                    }

                    // If no specific runtime function found, fall back to __rue_main
                    if target_symbol.is_none() {
                        if let Some(rue_main_symbol) = linked.symbols.get_symbol("__rue_main") {
                            target_symbol = Some(rue_main_symbol);
                            target_name = "__rue_main";
                        }
                    }

                    if let Some(symbol) = target_symbol {
                        let target = symbol.address as usize;
                        let correct_displacement = (target as i32) - (pc as i32);

                        tracing::debug!(
                            "Fixing call at 0x{:x} to {} (distance: {}): old displacement=0x{:x}, new displacement=0x{:x}, target=0x{:x}, original_target=0x{:x}",
                            call_offset,
                            target_name,
                            closest_distance,
                            displacement,
                            correct_displacement,
                            target,
                            original_target
                        );

                        fixes.push((call_offset, correct_displacement));
                    } else {
                        tracing::warn!(
                            "Found unrelocated call at 0x{:x} but couldn't determine target symbol",
                            call_offset
                        );
                    }
                }
            }
        }

        // Apply the fixes
        let fix_count = fixes.len();
        for (call_offset, correct_displacement) in fixes {
            let fixed_bytes = correct_displacement.to_le_bytes();
            linked.text_section[call_offset + 1..call_offset + 5].copy_from_slice(&fixed_bytes);
        }

        tracing::info!("Applied {} call instruction fixes", fix_count);

        Ok(())
    }

    /// Link all added object files together and generate an executable
    pub fn link_executable(&mut self) -> Result<Vec<u8>, CodegenError> {
        // Perform standard linking
        let linked = self.link()?;

        // Validate that the _start entry point exists for executable generation
        if linked.symbols.get_symbol("_start").is_none() {
            return Err(CodegenError::InvalidOperation(
                "_start symbol not found in linked result. Ensure the runtime provides this symbol.".to_string()
            ));
        }

        // Post-linking fix disabled - proper relocations are now working correctly
        // through the TwoPassRelocator system. The distance-based workaround was
        // interfering with correctly applied relocations.
        // self.fix_missing_call_relocations(&mut linked)?;

        // Find the _start symbol which is our entry point (we already validated it exists)
        let start_symbol = linked.symbols.get_symbol("_start").unwrap();

        tracing::info!("_start symbol address: 0x{:x}", start_symbol.address);
        tracing::info!("Text section size: {}", linked.text_section.len());

        // The _start symbol's address is the absolute address after symbol resolution
        // We need to calculate the offset within the text section by subtracting the text section base
        let text_section_base = self
            .merged_sections
            .get(".text")
            .map(|section| section.base_address)
            .unwrap_or(0);

        // Handle the case where _start's address might be less than text section base
        let entry_point_offset = if start_symbol.address < text_section_base {
            // _start is at the very beginning, so offset is 0
            tracing::warn!(
                "_start address (0x{:x}) is less than text section base (0x{:x}), using offset 0",
                start_symbol.address,
                text_section_base
            );
            0usize
        } else {
            (start_symbol.address - text_section_base) as usize
        };

        // Check what's at the entry point offset in the text section
        if entry_point_offset < linked.text_section.len()
            && entry_point_offset + 10 <= linked.text_section.len()
        {
            let entry_bytes = &linked.text_section[entry_point_offset..entry_point_offset + 10];
            tracing::info!(
                "Bytes at entry point offset 0x{:x}: {:?}",
                entry_point_offset,
                entry_bytes
            );
        } else {
            tracing::warn!(
                "Entry point offset 0x{:x} is out of bounds for text section size {}",
                entry_point_offset,
                linked.text_section.len()
            );
        }

        // Generate the ELF executable
        // Use the same approach as the x86_64 ELF writer
        self.create_executable_elf(
            &linked.text_section,
            &linked.rodata_section,
            linked.bss_size as usize,
            entry_point_offset,
        )
    }

    /// Build section name string table
    fn build_section_name_table() -> (Vec<u8>, Vec<usize>) {
        let section_names = [
            "",          // Index 0: NULL section (required)
            ".text",     // Index 1
            ".rodata",   // Index 2 (if needed)
            ".bss",      // Index 3 (if needed)
            ".shstrtab", // Index 4: Section header string table
        ];

        let mut shstrtab = Vec::new();
        let mut section_name_offsets = Vec::new();

        for name in &section_names {
            section_name_offsets.push(shstrtab.len());
            shstrtab.extend_from_slice(name.as_bytes());
            shstrtab.push(0); // null terminator
        }

        (shstrtab, section_name_offsets)
    }

    /// Calculate ELF layout and section indices
    fn calculate_elf_layout(
        text_section: &[u8],
        rodata_section: &[u8],
        bss_size: usize,
        shstrtab: &[u8],
    ) -> ElfLayout {
        const ELF_HEADER_SIZE: usize = 64;
        const PROGRAM_HEADER_SIZE: usize = 56;
        const SECTION_HEADER_SIZE: usize = 64;
        const PAGE_SIZE: usize = 0x1000;
        const BASE_ADDRESS: u64 = 0x400000;

        // Count actual sections we need
        let mut num_sections = 2; // NULL + .text
        let mut rodata_section_idx = 0u16;
        let mut bss_section_idx = 0u16;

        if !rodata_section.is_empty() {
            rodata_section_idx = num_sections;
            num_sections += 1;
        }
        if bss_size > 0 {
            bss_section_idx = num_sections;
            num_sections += 1;
        }
        let shstrtab_section_idx = num_sections;
        num_sections += 1;

        // Calculate layout
        let headers_size = ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE;
        let text_offset = align_to(headers_size as u64, 16) as usize;
        let text_size = text_section.len();

        let rodata_offset = if !rodata_section.is_empty() {
            align_to((text_offset + text_size) as u64, 8) as usize
        } else {
            0
        };
        let rodata_size = rodata_section.len();

        // String table comes after sections
        let shstrtab_offset = if rodata_size > 0 {
            align_to((rodata_offset + rodata_size) as u64, 8) as usize
        } else {
            align_to((text_offset + text_size) as u64, 8) as usize
        };

        // Section headers come after string table
        let section_headers_offset =
            align_to((shstrtab_offset + shstrtab.len()) as u64, 8) as usize;

        let file_size = section_headers_offset + (num_sections as usize * SECTION_HEADER_SIZE);
        let memory_size = align_to((file_size + bss_size) as u64, PAGE_SIZE as u64) as usize;

        ElfLayout {
            base_address: BASE_ADDRESS,
            page_size: PAGE_SIZE,
            text_offset,
            text_size,
            rodata_offset,
            rodata_size,
            shstrtab_offset,
            section_headers_offset,
            file_size,
            memory_size,
            num_sections,
            rodata_section_idx,
            bss_section_idx,
            shstrtab_section_idx,
        }
    }

    /// Write ELF header
    fn write_elf_header(elf: &mut Vec<u8>, layout: &ElfLayout, entry_point_offset: usize) {
        const ELF_HEADER_SIZE: usize = 64;
        const PROGRAM_HEADER_SIZE: usize = 56;
        const SECTION_HEADER_SIZE: usize = 64;

        // ELF header
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
        // e_entry (entry point address)
        let entry_address =
            layout.base_address + layout.text_offset as u64 + entry_point_offset as u64;
        elf.extend_from_slice(&entry_address.to_le_bytes());
        // e_phoff (program header offset)
        elf.extend_from_slice(&(ELF_HEADER_SIZE as u64).to_le_bytes());
        // e_shoff (section header offset)
        elf.extend_from_slice(&(layout.section_headers_offset as u64).to_le_bytes());
        // e_flags
        elf.extend_from_slice(&0u32.to_le_bytes());
        // e_ehsize
        elf.extend_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes());
        // e_phentsize
        elf.extend_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes());
        // e_phnum
        elf.extend_from_slice(&1u16.to_le_bytes());
        // e_shentsize
        elf.extend_from_slice(&(SECTION_HEADER_SIZE as u16).to_le_bytes());
        // e_shnum
        elf.extend_from_slice(&layout.num_sections.to_le_bytes());
        // e_shstrndx (string table section index)
        elf.extend_from_slice(&layout.shstrtab_section_idx.to_le_bytes());
    }

    /// Write program header
    fn write_program_header(elf: &mut Vec<u8>, layout: &ElfLayout) {
        // Program header
        // p_type: PT_LOAD (1)
        elf.extend_from_slice(&1u32.to_le_bytes());
        // p_flags: PF_X | PF_R | PF_W (7)
        elf.extend_from_slice(&7u32.to_le_bytes());
        // p_offset
        elf.extend_from_slice(&0u64.to_le_bytes());
        // p_vaddr
        elf.extend_from_slice(&layout.base_address.to_le_bytes());
        // p_paddr
        elf.extend_from_slice(&layout.base_address.to_le_bytes());
        // p_filesz
        elf.extend_from_slice(&(layout.file_size as u64).to_le_bytes());
        // p_memsz
        elf.extend_from_slice(&(layout.memory_size as u64).to_le_bytes());
        // p_align
        elf.extend_from_slice(&(layout.page_size as u64).to_le_bytes());
    }

    /// Write section headers
    fn write_section_headers(
        elf: &mut Vec<u8>,
        layout: &ElfLayout,
        section_name_offsets: &[usize],
        bss_size: usize,
        shstrtab_size: usize,
    ) {
        const SECTION_HEADER_SIZE: usize = 64;

        // Section 0: NULL section (required)
        elf.extend_from_slice(&[0u8; SECTION_HEADER_SIZE]);

        // Section 1: .text section
        // sh_name
        elf.extend_from_slice(&(section_name_offsets[1] as u32).to_le_bytes());
        // sh_type: SHT_PROGBITS (1)
        elf.extend_from_slice(&1u32.to_le_bytes());
        // sh_flags: SHF_ALLOC | SHF_EXECINSTR (6)
        elf.extend_from_slice(&6u64.to_le_bytes());
        // sh_addr
        elf.extend_from_slice(&(layout.base_address + layout.text_offset as u64).to_le_bytes());
        // sh_offset
        elf.extend_from_slice(&(layout.text_offset as u64).to_le_bytes());
        // sh_size
        elf.extend_from_slice(&(layout.text_size as u64).to_le_bytes());
        // sh_link
        elf.extend_from_slice(&0u32.to_le_bytes());
        // sh_info
        elf.extend_from_slice(&0u32.to_le_bytes());
        // sh_addralign
        elf.extend_from_slice(&16u64.to_le_bytes());
        // sh_entsize
        elf.extend_from_slice(&0u64.to_le_bytes());

        // Section 2: .rodata section (if needed)
        if layout.rodata_section_idx > 0 {
            // sh_name
            elf.extend_from_slice(&(section_name_offsets[2] as u32).to_le_bytes());
            // sh_type: SHT_PROGBITS (1)
            elf.extend_from_slice(&1u32.to_le_bytes());
            // sh_flags: SHF_ALLOC (2)
            elf.extend_from_slice(&2u64.to_le_bytes());
            // sh_addr
            elf.extend_from_slice(
                &(layout.base_address + layout.rodata_offset as u64).to_le_bytes(),
            );
            // sh_offset
            elf.extend_from_slice(&(layout.rodata_offset as u64).to_le_bytes());
            // sh_size
            elf.extend_from_slice(&(layout.rodata_size as u64).to_le_bytes());
            // sh_link
            elf.extend_from_slice(&0u32.to_le_bytes());
            // sh_info
            elf.extend_from_slice(&0u32.to_le_bytes());
            // sh_addralign
            elf.extend_from_slice(&8u64.to_le_bytes());
            // sh_entsize
            elf.extend_from_slice(&0u64.to_le_bytes());
        }

        // Section 3: .bss section (if needed)
        if layout.bss_section_idx > 0 {
            // sh_name
            elf.extend_from_slice(&(section_name_offsets[3] as u32).to_le_bytes());
            // sh_type: SHT_NOBITS (8)
            elf.extend_from_slice(&8u32.to_le_bytes());
            // sh_flags: SHF_ALLOC | SHF_WRITE (3)
            elf.extend_from_slice(&3u64.to_le_bytes());
            // sh_addr (after loaded segments)
            let bss_addr = layout.base_address
                + if layout.rodata_size > 0 {
                    layout.rodata_offset as u64 + layout.rodata_size as u64
                } else {
                    layout.text_offset as u64 + layout.text_size as u64
                };
            elf.extend_from_slice(&bss_addr.to_le_bytes());
            // sh_offset (no file content)
            elf.extend_from_slice(&0u64.to_le_bytes());
            // sh_size
            elf.extend_from_slice(&(bss_size as u64).to_le_bytes());
            // sh_link
            elf.extend_from_slice(&0u32.to_le_bytes());
            // sh_info
            elf.extend_from_slice(&0u32.to_le_bytes());
            // sh_addralign
            elf.extend_from_slice(&8u64.to_le_bytes());
            // sh_entsize
            elf.extend_from_slice(&0u64.to_le_bytes());
        }

        // Last section: .shstrtab (section header string table)
        // sh_name
        elf.extend_from_slice(&(section_name_offsets[4] as u32).to_le_bytes());
        // sh_type: SHT_STRTAB (3)
        elf.extend_from_slice(&3u32.to_le_bytes());
        // sh_flags: 0
        elf.extend_from_slice(&0u64.to_le_bytes());
        // sh_addr: 0 (not loaded)
        elf.extend_from_slice(&0u64.to_le_bytes());
        // sh_offset
        elf.extend_from_slice(&(layout.shstrtab_offset as u64).to_le_bytes());
        // sh_size
        elf.extend_from_slice(&(shstrtab_size as u64).to_le_bytes());
        // sh_link
        elf.extend_from_slice(&0u32.to_le_bytes());
        // sh_info
        elf.extend_from_slice(&0u32.to_le_bytes());
        // sh_addralign
        elf.extend_from_slice(&1u64.to_le_bytes());
        // sh_entsize
        elf.extend_from_slice(&0u64.to_le_bytes());
    }

    /// Create a proper executable ELF file (ET_EXEC) with program headers and section headers
    fn create_executable_elf(
        &self,
        text_section: &[u8],
        rodata_section: &[u8],
        bss_size: usize,
        entry_point_offset: usize,
    ) -> Result<Vec<u8>, CodegenError> {
        // Build section name string table
        let (shstrtab, section_name_offsets) = Self::build_section_name_table();

        // Calculate ELF layout
        let layout = Self::calculate_elf_layout(text_section, rodata_section, bss_size, &shstrtab);

        // Create ELF file with pre-allocated capacity
        let mut elf = Vec::with_capacity(layout.file_size);

        // Write ELF header
        Self::write_elf_header(&mut elf, &layout, entry_point_offset);

        // Write program header
        Self::write_program_header(&mut elf, &layout);

        // Pad to text section start
        while elf.len() < layout.text_offset {
            elf.push(0);
        }

        // Add text section
        elf.extend_from_slice(text_section);

        // Add rodata section if present
        if layout.rodata_size > 0 {
            while elf.len() < layout.rodata_offset {
                elf.push(0);
            }
            elf.extend_from_slice(rodata_section);
        }

        // Add section header string table
        while elf.len() < layout.shstrtab_offset {
            elf.push(0);
        }
        elf.extend_from_slice(&shstrtab);

        // Write section headers
        while elf.len() < layout.section_headers_offset {
            elf.push(0);
        }
        Self::write_section_headers(
            &mut elf,
            &layout,
            &section_name_offsets,
            bss_size,
            shstrtab.len(),
        );

        Ok(elf)
    }

    /// Merge sections from all object files
    fn merge_sections(&mut self) -> Result<(), CodegenError> {
        tracing::info!(
            "Merging sections from {} object files",
            self.object_files.len()
        );

        // List all object files
        for (i, obj) in self.object_files.iter().enumerate() {
            tracing::debug!("Object file {}: {}", i, obj.name);
        }

        // Process _start section first to place it at the beginning of .text
        self.process_start_section_first()?;

        for object_file in &self.object_files {
            tracing::warn!(
                "DEBUG: Processing object file: {} with {} sections",
                object_file.name,
                object_file.sections.len()
            );

            // Check current .text size before processing this object file
            if let Some(text_section) = self.merged_sections.get(".text") {
                tracing::warn!(
                    "DEBUG: Before processing object file '{}', .text size is 0x{:x}",
                    object_file.name,
                    text_section.data.len()
                );
            }
            for section in &object_file.sections {
                // Skip runtime sections since we already processed them
                let is_runtime_object = object_file.name.contains("rue_runtime")
                    || object_file.name.contains("rue-runtime")
                    || object_file.name.contains("librue_runtime");

                if is_runtime_object && section.name.starts_with(".text.") {
                    // Skip all .text.* sections from runtime library as they were already processed
                    tracing::debug!(
                        "Skipping already-processed runtime section: {}",
                        section.name
                    );
                    continue;
                }
                tracing::debug!(
                    "  Section: {} (size: {} bytes)",
                    section.name,
                    section.data.len()
                );

                // Add debug logging for user code sections
                if object_file.name.contains("user_code") && section.name == ".text" {
                    tracing::debug!(
                        "User code object contains .text section with {} bytes - this may overwrite runtime functions!",
                        section.data.len()
                    );
                }

                // Add specific debug logging for println functions
                if section.name.contains("println") {
                    tracing::warn!(
                        "DEBUG: Processing println section '{}' with {} bytes: {:?}",
                        section.name,
                        section.data.len(),
                        &section.data[..std::cmp::min(20, section.data.len())]
                    );
                }
                // Normalize section names - merge all .text.* sections into .text
                // Use constants to avoid repeated string allocations
                const TEXT_SECTION: &str = ".text";
                const RODATA_SECTION: &str = ".rodata";
                const DATA_SECTION: &str = ".data";
                const BSS_SECTION: &str = ".bss";

                let normalized_name = if section.name.starts_with(TEXT_SECTION) {
                    TEXT_SECTION.to_string()
                } else if section.name.starts_with(RODATA_SECTION) {
                    RODATA_SECTION.to_string()
                } else if section.name.starts_with(DATA_SECTION) {
                    DATA_SECTION.to_string()
                } else if section.name.starts_with(BSS_SECTION) {
                    BSS_SECTION.to_string()
                } else {
                    section.name.clone()
                };

                // Clone normalized_name once before using it in both places
                let merged_section_name = normalized_name.clone();
                let entry = self
                    .merged_sections
                    .entry(normalized_name.clone())
                    .or_insert_with(|| MergedSection {
                        name: section.name.clone(),
                        kind: section.kind,
                        data: Vec::new(),
                        alignment: section.alignment,
                        base_address: 0, // Will be set later
                    });

                // Align the current data to the section's alignment
                let current_size = entry.data.len() as u64;
                let aligned_size = align_to(current_size, section.alignment);
                let alignment_padding = aligned_size - current_size;
                entry.data.resize(aligned_size as usize, 0);

                // Record where this original section's data will be placed within the merged section
                let offset_within_merged = aligned_size;

                tracing::trace!(
                    "Section {} will be placed at offset 0x{:x} within merged section {}",
                    section.name,
                    offset_within_merged,
                    merged_section_name
                );

                // Add specific debug logging for println functions
                if section.name.contains("println") {
                    tracing::warn!(
                        "DEBUG: println section '{}' will be placed at offset 0x{:x} within merged section '{}', current merged size before: 0x{:x}, aligned size: 0x{:x}",
                        section.name,
                        offset_within_merged,
                        merged_section_name,
                        current_size,
                        aligned_size
                    );
                }

                // Debug text sections specifically
                if normalized_name == ".text" {
                    tracing::debug!(
                        "Merging text section '{}' at offset 0x{:x}, current size: 0x{:x}, aligned_size: 0x{:x}, adding {} bytes",
                        section.name,
                        offset_within_merged,
                        current_size,
                        aligned_size,
                        section.data.len()
                    );
                }

                // Debug user code placement
                if object_file.name.contains("user_code") {
                    tracing::debug!(
                        "Placing user code section '{}' at offset 0x{:x} in merged section '{}'",
                        section.name,
                        offset_within_merged,
                        merged_section_name
                    );
                }

                self.section_offsets.insert(
                    section.name.clone(),
                    SectionOffset {
                        merged_section_name,
                        offset_within_merged,
                        alignment_padding,
                    },
                );

                // Append the section data
                let size_before = entry.data.len();
                entry.data.extend_from_slice(&section.data);
                let size_after = entry.data.len();

                // Debug every .text section addition
                if normalized_name == ".text" {
                    tracing::warn!(
                        "DEBUG: Appended section '{}' ({} bytes) to .text: size_before=0x{:x}, size_after=0x{:x}, growth=0x{:x}",
                        section.name,
                        section.data.len(),
                        size_before,
                        size_after,
                        size_after - size_before
                    );
                }

                // Add specific debug logging for println functions after merging
                if section.name.contains("println") {
                    tracing::warn!(
                        "DEBUG: After merging println section '{}', total .text size is now: 0x{:x} bytes",
                        section.name,
                        entry.data.len()
                    );
                }

                // Debug user code merging
                if object_file.name.contains("user_code") {
                    tracing::debug!(
                        "After merging user code section '{}' ({} bytes), {} section size is now: 0x{:x} bytes",
                        section.name,
                        section.data.len(),
                        normalized_name,
                        entry.data.len()
                    );
                }

                // No padding between sections - real ELF linkers place sections contiguously
                // Padding was causing issues with section offset calculations

                // Use the maximum alignment
                entry.alignment = entry.alignment.max(section.alignment);
            }

            // Check current .text size after processing this object file
            if let Some(text_section) = self.merged_sections.get(".text") {
                tracing::warn!(
                    "DEBUG: After processing object file '{}', .text size is 0x{:x}",
                    object_file.name,
                    text_section.data.len()
                );
            }
        }

        // Final debug: what's the actual merged section size?
        if let Some(text_section) = self.merged_sections.get(".text") {
            tracing::warn!(
                "DEBUG: End of merge_sections - final .text section size is 0x{:x}",
                text_section.data.len()
            );
        }

        Ok(())
    }

    /// Process runtime sections first to place them at the beginning of .text
    fn process_start_section_first(&mut self) -> Result<(), CodegenError> {
        // Define the order of critical runtime sections
        // These should be placed at the beginning of .text in this specific order
        let critical_sections = [".text._start", ".text.__rue_main", ".text.__rue_exit"];

        // First, process the critical sections in order
        for section_name in &critical_sections {
            for object_file in &self.object_files {
                for section in &object_file.sections {
                    if section.name == *section_name {
                        tracing::info!(
                            "Processing critical runtime section {}: {} bytes",
                            section_name,
                            section.data.len()
                        );
                        if section_name == &".text._start" {
                            tracing::debug!("_start section full bytes: {:?}", section.data);
                        }

                        let normalized_name = ".text".to_string();
                        let merged_section_name = normalized_name.clone();
                        let entry =
                            self.merged_sections
                                .entry(normalized_name)
                                .or_insert_with(|| MergedSection {
                                    name: section.name.clone(),
                                    kind: section.kind,
                                    data: Vec::new(),
                                    alignment: section.alignment,
                                    base_address: 0,
                                });

                        // Calculate offset for this section
                        let current_size = entry.data.len() as u64;
                        let aligned_size = if section_name == &".text._start" {
                            // _start goes at offset 0, no alignment padding
                            0u64
                        } else {
                            align_to(current_size, section.alignment)
                        };
                        let alignment_padding = aligned_size - current_size;

                        if alignment_padding > 0 {
                            entry.data.resize(aligned_size as usize, 0);
                        }

                        let offset_within_merged = aligned_size;
                        self.section_offsets.insert(
                            section.name.clone(),
                            SectionOffset {
                                merged_section_name,
                                offset_within_merged,
                                alignment_padding,
                            },
                        );

                        tracing::info!(
                            "Runtime section {} will be placed at offset 0x{:x} within merged .text section",
                            section_name,
                            offset_within_merged
                        );

                        // Append the section data
                        entry.data.extend_from_slice(&section.data);
                        entry.alignment = entry.alignment.max(section.alignment);
                        break;
                    }
                }
            }
        }

        // Second pass: process ALL other runtime library .text.* sections
        // These are runtime functions that should be placed before user code
        let mut runtime_sections = Vec::new();

        for object_file in &self.object_files {
            // Check if this is a runtime library object file
            let is_runtime = object_file.name.contains("rue_runtime")
                || object_file.name.contains("rue-runtime")
                || object_file.name.contains("librue_runtime");

            if is_runtime {
                for section in &object_file.sections {
                    // Process .text.* sections that aren't already handled
                    if section.name.starts_with(".text.")
                        && !critical_sections.contains(&section.name.as_str())
                    {
                        runtime_sections.push((section.name.clone(), section));
                    }
                }
            }
        }

        // Sort runtime sections by name for consistency
        runtime_sections.sort_by(|a, b| a.0.cmp(&b.0));

        // Process all other runtime .text.* sections
        for (section_name, section) in runtime_sections {
            tracing::info!(
                "Processing runtime section {}: {} bytes",
                section_name,
                section.data.len()
            );

            // Special debug logging for println functions
            if section_name.contains("println") {
                tracing::warn!(
                    "Processing println runtime section '{}' with {} bytes at start of merge",
                    section_name,
                    section.data.len()
                );
            }

            let normalized_name = ".text".to_string();
            let merged_section_name = normalized_name.clone();
            let entry = self
                .merged_sections
                .entry(normalized_name)
                .or_insert_with(|| MergedSection {
                    name: section.name.clone(),
                    kind: section.kind,
                    data: Vec::new(),
                    alignment: section.alignment,
                    base_address: 0,
                });

            // Align current data to the section's alignment
            let current_size = entry.data.len() as u64;
            let aligned_size = align_to(current_size, section.alignment);
            let alignment_padding = aligned_size - current_size;
            entry.data.resize(aligned_size as usize, 0);

            let offset_within_merged = aligned_size;
            self.section_offsets.insert(
                section.name.clone(),
                SectionOffset {
                    merged_section_name,
                    offset_within_merged,
                    alignment_padding,
                },
            );

            tracing::info!(
                "Runtime section {} will be placed at offset 0x{:x} within merged .text section",
                section_name,
                offset_within_merged
            );

            // Append the section data
            entry.data.extend_from_slice(&section.data);
            entry.alignment = entry.alignment.max(section.alignment);
        }

        tracing::debug!("Finished processing all runtime sections");
        Ok(())
    }

    /// Build unified symbol table
    fn build_symbol_table(&mut self) -> Result<(), CodegenError> {
        tracing::debug!(
            "Building unified symbol table from {} object files",
            self.object_files.len()
        );
        for object_file in &self.object_files {
            tracing::debug!("Processing symbols from object file: {}", object_file.name);
            for symbol in &object_file.symbols {
                tracing::trace!(
                    "Adding symbol: {} ({:?}) at 0x{:x}",
                    symbol.name,
                    symbol.kind,
                    symbol.address
                );
                self.symbol_table.add_symbol(symbol.clone());
            }
        }
        Ok(())
    }

    /// Update symbol addresses after transformations have modified section sizes
    fn update_symbol_addresses_after_transformations(
        &mut self,
        offset_remappings: &HashMap<String, OffsetRemapping>,
    ) {
        // We need to update addresses for all symbols that point into modified sections
        tracing::debug!(
            "Updating symbol addresses for {} modified sections",
            offset_remappings.len()
        );
        for (section_name, remapping) in offset_remappings {
            tracing::debug!("Processing symbols in section '{}'", section_name);

            // Get all symbols in this section and update their addresses
            let symbols_to_update: Vec<(String, u64)> = self.symbol_table
                .symbols()
                .into_iter()
                .filter_map(|symbol| {
                    // Check if this symbol is in the modified section
                    if symbol.section_name == *section_name {
                        // The symbol's address is relative to the start of the section
                        // We need to remap it based on the transformations
                        let original_offset = symbol.address as usize;
                        let new_offset = remapping.remap_offset(original_offset);

                        // Always show debug for main symbol
                        if symbol.name == "main" {
                            tracing::debug!(
                                "Main symbol found in section '{}': original=0x{:x}, new=0x{:x}, changed={}",
                                section_name, original_offset, new_offset, new_offset != original_offset
                            );
                        }

                        if new_offset != original_offset {
                            tracing::debug!(
                                "Updating symbol '{}' in section '{}': 0x{:x} -> 0x{:x}",
                                symbol.name, section_name, original_offset, new_offset
                            );
                            Some((symbol.name.clone(), new_offset as u64))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect();

            tracing::debug!(
                "Found {} symbols to update in section '{}'",
                symbols_to_update.len(),
                section_name
            );

            // Apply the updates
            for (name, new_address) in symbols_to_update {
                tracing::debug!("Actually updating symbol '{}' to 0x{:x}", name, new_address);
                self.symbol_table.update_symbol_address(&name, new_address);
            }
        }
    }

    /// Apply relocations to merged sections using two-pass system
    fn apply_relocations(&mut self) -> Result<(), CodegenError> {
        use std::collections::HashSet;

        // Collect all relocations first to avoid borrowing issues
        // Use a HashSet to track unique relocations and avoid duplicates
        let mut seen_relocations = HashSet::new();
        let mut unique_relocations = Vec::new();

        for object_file in &self.object_files {
            for relocation in &object_file.relocations {
                // Use the dedup_key method to get a unique identifier for this relocation
                let reloc_key = relocation.dedup_key();

                // Only add if we haven't seen this exact relocation before
                if seen_relocations.insert(reloc_key) {
                    unique_relocations.push(relocation.clone());

                    // Debug: Log __rue_println_i64 internal relocations specifically
                    if relocation.section_name.contains("__rue_println_i64") {
                        tracing::debug!(
                            "Internal relocation preserved: section='{}', offset=0x{:x}, kind={:?}, symbol='{}', addend={}",
                            relocation.section_name,
                            relocation.offset,
                            relocation.kind,
                            relocation.symbol_name,
                            relocation.addend
                        );
                    }
                } else {
                    // Log what was filtered as duplicate
                    if relocation.section_name.contains("__rue_println_i64")
                        || relocation.symbol_name.contains("println")
                    {
                        tracing::debug!(
                            "Filtered relocation as duplicate: section='{}', offset=0x{:x}, kind={:?}, symbol='{}', addend={}",
                            relocation.section_name,
                            relocation.offset,
                            relocation.kind,
                            relocation.symbol_name,
                            relocation.addend
                        );
                    } else {
                        tracing::debug!(
                            "Filtered relocation as duplicate: section='{}', offset=0x{:x}, kind={:?}, symbol='{}', addend={}",
                            relocation.section_name,
                            relocation.offset,
                            relocation.kind,
                            relocation.symbol_name,
                            relocation.addend
                        );
                    }
                }
            }
        }

        tracing::info!(
            "Applying {} unique relocations (filtered from {} total) using two-pass system",
            unique_relocations.len(),
            self.object_files
                .iter()
                .map(|o| o.relocations.len())
                .sum::<usize>()
        );

        // Use the two-pass relocator for sophisticated relocation processing
        let relocator = TwoPassRelocator::new(&self.symbol_table, &self.section_offsets);
        let offset_remappings =
            relocator.apply_relocations(&mut self.merged_sections, &unique_relocations)?;

        // Update symbol addresses for symbols in sections that were modified
        self.update_symbol_addresses_after_transformations(&offset_remappings);

        Ok(())
    }

    /// Build the final linked result
    fn build_result(&self) -> LinkedResult {
        // DEBUG: Check println_i64 bytes in merged_sections before building result
        if let Some(text_section) = self.merged_sections.get(".text") {
            let println_offset = 0x147a; // After transformations
            if println_offset + 10 <= text_section.data.len() {
                let bytes = &text_section.data[println_offset..println_offset + 10];
                tracing::debug!(
                    "BUILD_RESULT: Bytes at 0x{:x} in .text section: {:02x?}",
                    println_offset,
                    bytes
                );
                // Also check at original offset
                let original_offset = 0x1480;
                if original_offset + 10 <= text_section.data.len() {
                    let original_bytes = &text_section.data[original_offset..original_offset + 10];
                    tracing::debug!(
                        "BUILD_RESULT: Bytes at ORIGINAL 0x{:x} in .text section: {:02x?}",
                        original_offset,
                        original_bytes
                    );
                }
            }
        }

        let text_section = self
            .merged_sections
            .get(".text")
            .map(|s| {
                tracing::debug!(
                    "build_result: .text section from merged_sections has size 0x{:x}",
                    s.data.len()
                );
                s.data.clone()
            })
            .unwrap_or_default();

        let rodata_section = self
            .merged_sections
            .get(".rodata")
            .map(|s| s.data.clone())
            .unwrap_or_default();

        let bss_size = self
            .merged_sections
            .get(".bss")
            .map(|s| s.data.len() as u64)
            .unwrap_or(0);

        LinkedResult {
            text_section,
            rodata_section,
            bss_size,
            symbols: self.symbol_table.clone(),
        }
    }

    /// Assign base addresses to merged sections
    fn assign_section_addresses(&mut self) -> Result<(), CodegenError> {
        // These should match the ELF layout constants
        const BASE_ADDRESS: u64 = 0x400000;
        const HEADERS_SIZE: u64 = 0x80; // ELF header + program header

        // Start after headers
        let mut current_address = HEADERS_SIZE;

        // Assign addresses to sections in order: .text, .rodata, .data, .bss
        let section_order = [".text", ".rodata", ".data", ".bss"];

        for section_name in &section_order {
            if let Some(section) = self.merged_sections.get_mut(*section_name) {
                // Align the address
                current_address = align_to(current_address, 8);

                // Assign the address (this is the offset from BASE_ADDRESS)
                section.base_address = current_address;

                // Move to next section
                current_address = current_address + section.data.len() as u64;
                tracing::debug!(
                    "Assigned base address 0x{:x} (virtual: 0x{:x}) to section {} (size: {} bytes)",
                    section.base_address,
                    BASE_ADDRESS + section.base_address,
                    section_name,
                    section.data.len()
                );
            }
        }

        Ok(())
    }

    /// Update symbol addresses based on final section layout
    fn update_symbol_addresses(&mut self) -> Result<(), CodegenError> {
        tracing::info!(
            "Starting update_symbol_addresses for {} symbols",
            self.symbol_table.symbols().len()
        );

        // We need to find each symbol's section and update its address
        // First, build a mapping of section names to their base addresses
        let section_bases: HashMap<String, u64> = self
            .merged_sections
            .iter()
            .map(|(name, section)| (name.clone(), section.base_address))
            .collect();

        tracing::info!("Section base addresses: {:?}", section_bases);
        tracing::info!("Section offsets mapping: {:?}", self.section_offsets);

        // Update symbol addresses by adding the section base address
        // We need to update the symbols in all the specialized tables
        let mut symbols_to_update = Vec::new();

        // Collect all symbols that need updating
        let mut symbols_checked = 0;
        let mut symbols_with_sections = 0;

        for symbol in self.symbol_table.symbols() {
            symbols_checked += 1;

            if !symbol.section_name.is_empty() {
                symbols_with_sections += 1;
                if symbol.name == "_start" {
                    tracing::info!(
                        "_start symbol: section_name='{}', address=0x{:x}",
                        symbol.section_name,
                        symbol.address
                    );
                }

                // Calculate the final address using the section offset mapping
                let new_address = if let Some(section_offset) =
                    self.section_offsets.get(&symbol.section_name)
                {
                    // This symbol's section was merged - use the precise offset mapping
                    if let Some(&base_addr) = section_bases.get(&section_offset.merged_section_name)
                    {
                        // CRITICAL FIX: Runtime library symbols already have final addresses
                        // from the merge process, while user symbols need base address added.
                        //
                        // For runtime symbols: symbol.address is already the final offset within merged section
                        // For user symbols: symbol.address is relative to their original section
                        let final_addr = if symbol.source == SymbolSource::RuntimeLibrary {
                            // Runtime symbols: address is already the offset within merged section
                            // Add base_addr to get final virtual address
                            base_addr + section_offset.offset_within_merged + symbol.address
                        } else {
                            // User symbols: calculate from section offset + symbol offset
                            base_addr + section_offset.offset_within_merged + symbol.address
                        };

                        // DEBUG: Track println_i64 through section_offsets path
                        if symbol.name == "__rue_println_i64" {
                            tracing::debug!(
                                "Symbol update via section_offsets: __rue_println_i64 source={:?}, offset_within_merged=0x{:x}, symbol.address=0x{:x}, final=0x{:x} (base=0x{:x})",
                                symbol.source,
                                section_offset.offset_within_merged,
                                symbol.address,
                                final_addr,
                                base_addr
                            );
                        }

                        // Debug BSS symbols
                        if symbol.section_name.starts_with(".bss") {
                            tracing::debug!(
                                "BSS symbol '{}' calculation: base_addr=0x{:x} + offset_in_merged=0x{:x} + symbol_offset=0x{:x} = 0x{:x}",
                                symbol.name,
                                base_addr,
                                section_offset.offset_within_merged,
                                symbol.address,
                                final_addr
                            );
                        }

                        if symbol.name == "_start" {
                            tracing::info!(
                                "_start symbol calculation: base_addr=0x{:x} + offset_in_merged=0x{:x} + symbol_offset=0x{:x} = 0x{:x}",
                                base_addr,
                                section_offset.offset_within_merged,
                                symbol.address,
                                final_addr
                            );
                        }

                        final_addr
                    } else {
                        tracing::warn!(
                            "Merged section '{}' not found in bases for symbol '{}'",
                            section_offset.merged_section_name,
                            symbol.name
                        );
                        continue;
                    }
                } else {
                    // DEBUG: Track println_i64 specifically
                    if symbol.name == "__rue_println_i64" {
                        tracing::debug!(
                            "Symbol update: __rue_println_i64 not found in section_offsets. section_name='{}', current address=0x{:x}",
                            symbol.section_name,
                            symbol.address
                        );
                        tracing::debug!(
                            "Available section_offsets keys: {:?}",
                            self.section_offsets.keys().collect::<Vec<_>>()
                        );
                    }

                    // Fallback: use the normalized section name approach (for backwards compatibility)
                    // Handle relocation sections: .rela.text.* -> .text, .rela.rodata.* -> .rodata
                    let normalized_name = if symbol.section_name.starts_with(".rela.text") {
                        ".text".to_string()
                    } else if symbol.section_name.starts_with(".rela.rodata") {
                        ".rodata".to_string()
                    } else if symbol.section_name.starts_with(".rela.data") {
                        ".data".to_string()
                    } else if symbol.section_name.starts_with(".rela.bss") {
                        ".bss".to_string()
                    } else if symbol.section_name.starts_with(".text") {
                        ".text".to_string()
                    } else if symbol.section_name.starts_with(".rodata") {
                        ".rodata".to_string()
                    } else if symbol.section_name.starts_with(".data") {
                        ".data".to_string()
                    } else if symbol.section_name.starts_with(".bss") {
                        ".bss".to_string()
                    } else {
                        symbol.section_name.clone()
                    };

                    if let Some(&base_addr) = section_bases.get(&normalized_name) {
                        // CRITICAL FIX: Runtime library symbols already have final addresses
                        // from the merge process, while user symbols need base address added.
                        //
                        // Runtime symbols get their final positions during process_start_section_first()
                        // and merge_sections(), so their addresses are already absolute within the merged section.
                        // User symbols have addresses relative to their original section and need adjustment.
                        let final_addr = if symbol.source == SymbolSource::RuntimeLibrary {
                            // Runtime symbols: For symbols like _start that are placed at section beginning,
                            // the symbol address is 0 but they need the base address added
                            // Special handling for _start which is always at the beginning of .text
                            if symbol.name == "_start" || symbol.address == 0 {
                                base_addr
                            } else {
                                // Other runtime symbols already have final positions from merge
                                symbol.address
                            }
                        } else {
                            // User symbols: address is relative to original section, add base to get file offset
                            base_addr + symbol.address
                        };

                        // DEBUG: Track println_i64 specifically
                        if symbol.name == "__rue_println_i64" {
                            tracing::debug!(
                                "Symbol update fallback: __rue_println_i64 source={:?}, address=0x{:x}, final=0x{:x} (base=0x{:x}) [runtime={}]",
                                symbol.source,
                                symbol.address,
                                final_addr,
                                base_addr,
                                symbol.source == SymbolSource::RuntimeLibrary
                            );
                        }

                        tracing::debug!(
                            "Calculation for symbol '{}' (source={:?}) in section '{}': final_addr=0x{:x} (base=0x{:x}, symbol.address=0x{:x}) [runtime_symbol={}]",
                            symbol.name,
                            symbol.source,
                            symbol.section_name,
                            final_addr,
                            base_addr,
                            symbol.address,
                            symbol.source == SymbolSource::RuntimeLibrary
                        );

                        final_addr
                    } else {
                        if symbol.name == "_start" {
                            tracing::debug!(
                                "_start symbol normalized to '{}' but section not found in bases",
                                normalized_name
                            );
                        }
                        continue;
                    }
                };

                symbols_to_update.push((symbol.name.clone(), symbol.kind, new_address));
            }
        }

        tracing::info!(
            "Checked {} symbols, {} have sections, updating {} symbols",
            symbols_checked,
            symbols_with_sections,
            symbols_to_update.len()
        );

        // Update the symbols
        for (name, _kind, new_address) in symbols_to_update {
            // Try to get the symbol to log the old address
            let old_address = if let Some(symbol) = self.symbol_table.get_symbol(&name) {
                symbol.address
            } else if let Some(symbol) = self.symbol_table.get_local_symbol(&name) {
                symbol.address
            } else {
                tracing::warn!("Could not find symbol {} for address logging", name);
                0
            };

            if name == "_start" {
                tracing::info!(
                    "Updating _start symbol address from 0x{:x} to 0x{:x}",
                    old_address,
                    new_address
                );
            } else {
                tracing::debug!(
                    "Updated symbol {} address from 0x{:x} to 0x{:x}",
                    name,
                    old_address,
                    new_address
                );
            }

            // Update the symbol address - this method handles all symbol types
            self.symbol_table.update_symbol_address(&name, new_address);
        }

        Ok(())
    }
}

/// Align a value to the given alignment
fn align_to(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        value
    } else {
        (value + alignment - 1) & !(alignment - 1)
    }
}
