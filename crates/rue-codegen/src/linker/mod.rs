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
    pub fn add_object_file_from_path(&mut self, path: &str) -> Result<(), CodegenError> {
        let data = std::fs::read(path).map_err(|_| CodegenError::Io)?;
        let name = path.to_string();
        self.add_object_file(name, &data)
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

        // Step 2: Build unified symbol table
        self.build_symbol_table()?;

        // Step 3: Apply relocations
        self.apply_relocations()?;

        // Step 4: Build final result
        Ok(self.build_result())
    }

    /// Merge sections from all object files
    fn merge_sections(&mut self) -> Result<(), CodegenError> {
        for object_file in &self.object_files {
            for section in &object_file.sections {
                let entry = self
                    .merged_sections
                    .entry(section.name.clone())
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
                entry.data.resize(aligned_size as usize, 0);

                // Record where this section was placed in the merged section
                self.section_placements.push(SectionPlacement {
                    object_file_name: object_file.name.clone(),
                    section_name: section.name.clone(),
                    offset_in_merged: aligned_size,
                });

                // Append the section data
                entry.data.extend_from_slice(&section.data);

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

                // Create adjusted symbol with corrected address
                let mut adjusted_symbol = symbol.clone();
                adjusted_symbol.address += offset;

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
                if offset + 8 > section.data.len() {
                    return Err(CodegenError::InvalidOperation(
                        "Relocation offset out of bounds".to_string(),
                    ));
                }
                section.data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }
            RelocationKind::PC32 => {
                // R_X86_64_PC32: 32-bit PC-relative address
                let pc = section.base_address + relocation.offset;
                let value = (target_address as i64 - pc as i64 + relocation.addend) as i32;
                let offset = relocation.offset as usize;
                if offset + 4 > section.data.len() {
                    return Err(CodegenError::InvalidOperation(
                        "Relocation offset out of bounds".to_string(),
                    ));
                }
                section.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
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
            .map(|s| s.data.len() as u64)
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
        (value + alignment - 1) & !(alignment - 1)
    }
}
