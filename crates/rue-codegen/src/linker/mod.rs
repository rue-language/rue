// Minimal object file linker for the Rue compiler
//
// This module provides functionality to link external assembly object files
// into the final Rue executable. It supports basic ELF64 object file parsing,
// symbol resolution, and relocation application.

use crate::CodegenError;
use object::SectionKind;
use std::collections::HashMap;

pub mod object_file;
pub mod relocation;
pub mod symbol;

#[cfg(test)]
mod tests;

pub use object_file::ObjectFile;
pub use relocation::{RelocationEntry, RelocationKind};
pub use symbol::{Symbol, SymbolKind, SymbolTable};

/// Tracks where a section from a specific object file was placed in the merged section
#[derive(Debug, Clone)]
struct SectionPlacement {
    object_file_name: String,
    section_name: String,
    offset_in_merged: u64,
}

/// A minimal object file linker that can link assembly object files into Rue executables.
pub struct Linker {
    object_files: Vec<ObjectFile>,
    merged_sections: HashMap<String, MergedSection>,
    symbol_table: SymbolTable,
    section_placements: Vec<SectionPlacement>,
}

/// Represents a merged section from multiple object files
#[derive(Debug, Clone)]
pub struct MergedSection {
    pub name: String,
    pub kind: SectionKind,
    pub data: Vec<u8>,
    pub alignment: u64,
    pub base_address: u64,
    /// Total size of the section in memory (for BSS, this is the uninitialized space)
    pub size: u64,
}

/// Result of the linking process
#[derive(Debug, Clone)]
pub struct LinkedResult {
    pub text_section: Vec<u8>,
    pub rodata_section: Vec<u8>,
    pub bss_size: u64,
    pub symbols: SymbolTable,
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
            section_placements: Vec::new(),
        }
    }

    /// Add an object file to the linker from raw bytes
    pub fn add_object_file(&mut self, name: String, data: &[u8]) -> Result<(), CodegenError> {
        let object_file = ObjectFile::parse(name, data)?;
        self.object_files.push(object_file);
        Ok(())
    }

    /// Add an object file to the linker from a file path
    ///
    /// Supports both individual .o files and .a archive files.
    /// If the file is an archive, all object files within it will be added.
    pub fn add_object_file_from_path(&mut self, path: &str) -> Result<(), CodegenError> {
        let data = std::fs::read(path).map_err(|_| CodegenError::Io)?;

        // Try to parse as an archive first
        if let Ok(archive) = object::read::archive::ArchiveFile::parse(&*data) {
            // It's an archive - extract and add each member
            for member_result in archive.members() {
                let member = member_result.map_err(|e| {
                    CodegenError::InvalidOperation(format!("Failed to read archive member: {}", e))
                })?;

                let member_name = String::from_utf8_lossy(member.name()).to_string();
                let member_data = member.data(&*data).map_err(|e| {
                    CodegenError::InvalidOperation(format!(
                        "Failed to extract archive member data: {}",
                        e
                    ))
                })?;

                // Skip non-object files (like symbol tables)
                if member_name.ends_with(".o") || member_data.starts_with(b"\x7fELF") {
                    let full_name = format!("{}:{}", path, member_name);
                    self.add_object_file(full_name, member_data)?;
                }
            }
            Ok(())
        } else {
            // Not an archive - treat as a single object file
            let name = path.to_string();
            self.add_object_file(name, &data)
        }
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

        // Step 2: Calculate base addresses for merged sections
        self.calculate_base_addresses();

        // Step 3: Build unified symbol table
        self.build_symbol_table()?;

        // Step 4: Apply relocations
        self.apply_relocations()?;

        // Step 5: Build final result
        Ok(self.build_result())
    }

    /// Calculate base addresses for merged sections based on standard executable layout
    fn calculate_base_addresses(&mut self) {
        // Standard ELF executable base address
        const BASE_ADDR: u64 = 0x400000;
        const ELF_HEADER_SIZE: u64 = 64;
        const PROGRAM_HEADER_SIZE: u64 = 56;
        const PAGE_SIZE: u64 = 0x1000;

        // Calculate starting offset (after headers)
        let headers_size = ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE;
        let mut current_addr = BASE_ADDR + align_to(headers_size, 16);

        // Assign addresses to sections in standard order: .text, .rodata, .data, .bss
        for section_name in [".text", ".rodata", ".data", ".bss"] {
            if let Some(section) = self.merged_sections.get_mut(section_name) {
                // Align to section's required alignment
                current_addr = align_to(current_addr, section.alignment);
                section.base_address = current_addr;

                // Move to next section using size (for BSS, size > data.len())
                current_addr += section.size;

                // Align to reasonable boundary between sections
                current_addr = align_to(current_addr, 8);
            }
        }

        // Handle any other sections not in the standard list
        let mut other_sections: Vec<String> = self
            .merged_sections
            .keys()
            .filter(|name| !matches!(name.as_str(), ".text" | ".rodata" | ".data" | ".bss"))
            .cloned()
            .collect();
        other_sections.sort();

        for section_name in other_sections {
            if let Some(section) = self.merged_sections.get_mut(&section_name) {
                current_addr = align_to(current_addr, section.alignment);
                section.base_address = current_addr;
                current_addr += section.size;
                current_addr = align_to(current_addr, 8);
            }
        }
    }

    /// Merge sections from all object files
    fn merge_sections(&mut self) -> Result<(), CodegenError> {
        for object_file in &self.object_files {
            for section in &object_file.sections {
                let is_bss = section.kind == SectionKind::UninitializedData;

                let entry = self
                    .merged_sections
                    .entry(section.name.clone())
                    .or_insert_with(|| MergedSection {
                        name: section.name.clone(),
                        kind: section.kind,
                        data: Vec::new(),
                        alignment: section.alignment,
                        base_address: 0, // Will be set later
                        size: 0,
                    });

                // Align the current position to the section's alignment
                let current_pos = if is_bss {
                    entry.size
                } else {
                    entry.data.len() as u64
                };
                let aligned_pos = align_to(current_pos, section.alignment);

                if is_bss {
                    // For BSS, we only track size, not data
                    entry.size = aligned_pos + section.size;
                } else {
                    // For regular sections, pad data and append
                    entry.data.resize(aligned_pos as usize, 0);
                    entry.data.extend_from_slice(&section.data);
                    entry.size = entry.data.len() as u64;
                }

                // Record where this section was placed in the merged section
                self.section_placements.push(SectionPlacement {
                    object_file_name: object_file.name.clone(),
                    section_name: section.name.clone(),
                    offset_in_merged: aligned_pos,
                });

                // Use the maximum alignment
                entry.alignment = entry.alignment.max(section.alignment);
            }
        }

        Ok(())
    }

    /// Build unified symbol table
    fn build_symbol_table(&mut self) -> Result<(), CodegenError> {
        for object_file in &self.object_files {
            for symbol in &object_file.symbols {
                // Find the offset where this symbol's section was placed
                let offset = self
                    .section_placements
                    .iter()
                    .find(|placement| {
                        placement.object_file_name == object_file.name
                            && placement.section_name == symbol.section_name
                    })
                    .map(|placement| placement.offset_in_merged)
                    .unwrap_or(0);

                // Get the base address of the merged section
                let base_address = self
                    .merged_sections
                    .get(&symbol.section_name)
                    .map(|section| section.base_address)
                    .unwrap_or(0);

                // Create adjusted symbol with corrected absolute address
                let mut adjusted_symbol = symbol.clone();
                adjusted_symbol.address = base_address + offset + symbol.address;

                self.symbol_table.add_symbol(adjusted_symbol);
            }
        }
        Ok(())
    }

    /// Apply relocations to merged sections
    fn apply_relocations(&mut self) -> Result<(), CodegenError> {
        // Collect all relocations first to avoid borrowing issues
        let mut all_relocations = Vec::new();
        for object_file in &self.object_files {
            for relocation in &object_file.relocations {
                all_relocations.push(relocation.clone());
            }
        }

        // Apply all relocations
        for relocation in &all_relocations {
            self.apply_relocation(relocation)?;
        }
        Ok(())
    }

    /// Apply a single relocation
    fn apply_relocation(&mut self, relocation: &RelocationEntry) -> Result<(), CodegenError> {
        // Find the target symbol
        let symbol = self
            .symbol_table
            .get_symbol(&relocation.symbol_name)
            .ok_or_else(|| {
                CodegenError::InvalidOperation(format!(
                    "Undefined symbol: {}",
                    relocation.symbol_name
                ))
            })?;

        // Get the section containing the relocation site
        let section = self
            .merged_sections
            .get_mut(&relocation.section_name)
            .ok_or_else(|| {
                CodegenError::InvalidOperation(format!(
                    "Section not found: {}",
                    relocation.section_name
                ))
            })?;

        // Calculate the target address
        let target_address = symbol.address;

        // Apply the relocation based on its kind
        match relocation.kind {
            RelocationKind::Absolute64 => {
                // R_X86_64_64: 64-bit absolute address
                let value = (target_address as i64 + relocation.addend) as u64;
                let offset = relocation.offset as usize;

                // Check for overflow and bounds
                let end_offset = offset.checked_add(8).ok_or_else(|| {
                    CodegenError::InvalidOperation("Relocation offset would overflow".to_string())
                })?;

                if end_offset > section.data.len() {
                    return Err(CodegenError::InvalidOperation(
                        "Relocation offset out of bounds".to_string(),
                    ));
                }
                section.data[offset..end_offset].copy_from_slice(&value.to_le_bytes());
            }
            RelocationKind::PC32 => {
                // R_X86_64_PC32: 32-bit PC-relative address
                let pc = section.base_address + relocation.offset;
                let value = (target_address as i64 - pc as i64 + relocation.addend) as i32;
                let offset = relocation.offset as usize;

                // Check for overflow and bounds
                let end_offset = offset.checked_add(4).ok_or_else(|| {
                    CodegenError::InvalidOperation("Relocation offset would overflow".to_string())
                })?;

                if end_offset > section.data.len() {
                    return Err(CodegenError::InvalidOperation(
                        "Relocation offset out of bounds".to_string(),
                    ));
                }
                section.data[offset..end_offset].copy_from_slice(&value.to_le_bytes());
            }
        }

        Ok(())
    }

    /// Build the final linked result
    fn build_result(&self) -> LinkedResult {
        let text_section = self
            .merged_sections
            .get(".text")
            .map(|s| s.data.clone())
            .unwrap_or_default();

        let rodata_section = self
            .merged_sections
            .get(".rodata")
            .map(|s| s.data.clone())
            .unwrap_or_default();

        let bss_size = self
            .merged_sections
            .get(".bss")
            .map(|s| s.size)
            .unwrap_or(0);

        LinkedResult {
            text_section,
            rodata_section,
            bss_size,
            symbols: self.symbol_table.clone(),
        }
    }
}

/// Align a value to the given alignment
fn align_to(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        value
    } else {
        // Alignment must be a power of 2 for bitwise alignment to work correctly
        debug_assert!(
            alignment.is_power_of_two(),
            "Alignment must be a power of 2, got {}",
            alignment
        );
        (value + alignment - 1) & !(alignment - 1)
    }
}
