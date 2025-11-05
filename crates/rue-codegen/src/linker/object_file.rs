// Object file parsing for ELF64 files
//
// This module handles parsing of ELF64 object files (.o files) to extract
// sections, symbols, and relocations needed for linking.

use crate::CodegenError;
use object::{BinaryFormat, Object, ObjectSection, ObjectSymbol, RelocationTarget, SectionKind};

use super::{RelocationEntry, RelocationKind, Symbol, SymbolKind};

/// Represents a parsed object file with all relevant information for linking
#[derive(Debug, Clone)]
pub struct ObjectFile {
    pub name: String,
    pub sections: Vec<Section>,
    pub symbols: Vec<Symbol>,
    pub relocations: Vec<RelocationEntry>,
}

/// Represents a section within an object file
#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub kind: SectionKind,
    pub data: Vec<u8>,
    pub alignment: u64,
    pub address: u64,
    /// Size of the section in bytes (for BSS, this may be larger than data.len())
    pub size: u64,
}

impl ObjectFile {
    /// Parse an object file from raw bytes
    pub fn parse(name: String, data: &[u8]) -> Result<Self, CodegenError> {
        let obj_file = object::File::parse(data).map_err(|e| {
            CodegenError::InvalidOperation(format!("Failed to parse object file: {}", e))
        })?;

        // Verify this is an ELF64 file
        if obj_file.format() != BinaryFormat::Elf {
            return Err(CodegenError::InvalidOperation(
                "Object file must be in ELF format".to_string(),
            ));
        }

        if !obj_file.is_64() {
            return Err(CodegenError::InvalidOperation(
                "Object file must be 64-bit ELF".to_string(),
            ));
        }

        let mut object_file = ObjectFile {
            name,
            sections: Vec::new(),
            symbols: Vec::new(),
            relocations: Vec::new(),
        };

        // Parse sections
        object_file.parse_sections(&obj_file)?;

        // Parse symbols
        object_file.parse_symbols(&obj_file)?;

        // Parse relocations
        object_file.parse_relocations(&obj_file)?;

        Ok(object_file)
    }

    /// Parse sections from the object file
    fn parse_sections(&mut self, obj_file: &object::File) -> Result<(), CodegenError> {
        for section in obj_file.sections() {
            let name = section.name().unwrap_or("<unknown>").to_string();

            // Skip sections we don't care about
            if name.is_empty() || name.starts_with('.') && !self.is_important_section(&name) {
                continue;
            }

            let kind = section.kind();
            let data = section.data().unwrap_or(&[]).to_vec();
            let alignment = section.align();
            let address = section.address();
            let size = section.size();

            self.sections.push(Section {
                name,
                kind,
                data,
                alignment,
                address,
                size,
            });
        }

        Ok(())
    }

    /// Parse symbols from the object file
    fn parse_symbols(&mut self, obj_file: &object::File) -> Result<(), CodegenError> {
        for symbol in obj_file.symbols() {
            let name = symbol.name().unwrap_or("<unknown>").to_string();

            // Skip unnamed symbols and compiler-generated symbols
            if name.is_empty() || name.starts_with('.') {
                continue;
            }

            let kind = if symbol.is_global() {
                SymbolKind::Global
            } else if symbol.is_local() {
                SymbolKind::Local
            } else {
                SymbolKind::Weak
            };

            let address = symbol.address();
            let size = symbol.size();

            // Determine section name if symbol is defined
            let section_name = match symbol.section() {
                object::SymbolSection::Section(section_index) => {
                    // Find the section by index
                    if let Some(section) = obj_file.sections().nth(section_index.0) {
                        section.name().unwrap_or("<unknown>").to_string()
                    } else {
                        String::new()
                    }
                }
                _ => String::new(), // Undefined symbol or special section
            };

            self.symbols.push(Symbol {
                name,
                kind,
                address,
                size,
                section_name,
            });
        }

        Ok(())
    }

    /// Parse relocations from the object file
    fn parse_relocations(&mut self, obj_file: &object::File) -> Result<(), CodegenError> {
        for section in obj_file.sections() {
            // Look for relocation sections (.rela.*)
            let section_name = section.name().unwrap_or("");
            if !section_name.starts_with(".rela.") {
                continue;
            }

            // Get the target section name (remove .rela. prefix)
            let target_section = &section_name[6..];

            for (offset, relocation) in section.relocations() {
                let kind = match relocation.kind() {
                    object::RelocationKind::Absolute => {
                        // Check the size to determine if it's 64-bit
                        if relocation.size() == 64 {
                            RelocationKind::Absolute64
                        } else {
                            continue; // Skip unsupported relocation sizes
                        }
                    }
                    object::RelocationKind::Relative => RelocationKind::PC32,
                    _ => continue, // Skip unsupported relocation kinds
                };

                let symbol_name = match relocation.target() {
                    RelocationTarget::Symbol(symbol_idx) => {
                        // Find symbol by index
                        if let Some(symbol) = obj_file.symbols().nth(symbol_idx.0) {
                            symbol.name().unwrap_or("<unknown>").to_string()
                        } else {
                            continue; // Skip if symbol not found
                        }
                    }
                    RelocationTarget::Section(section_idx) => {
                        // Section-relative relocation
                        if let Some(section) = obj_file.sections().nth(section_idx.0) {
                            section.name().unwrap_or("<unknown>").to_string()
                        } else {
                            continue; // Skip if section not found
                        }
                    }
                    _ => continue, // Skip other relocation targets
                };

                let addend = relocation.addend();

                self.relocations.push(RelocationEntry {
                    section_name: target_section.to_string(),
                    offset,
                    kind,
                    symbol_name,
                    addend,
                });
            }
        }

        Ok(())
    }

    /// Check if a section is important for linking
    fn is_important_section(&self, name: &str) -> bool {
        matches!(name, ".text" | ".data" | ".rodata" | ".bss")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_important_section() {
        let obj = ObjectFile {
            name: "test".to_string(),
            sections: Vec::new(),
            symbols: Vec::new(),
            relocations: Vec::new(),
        };

        assert!(obj.is_important_section(".text"));
        assert!(obj.is_important_section(".data"));
        assert!(obj.is_important_section(".rodata"));
        assert!(obj.is_important_section(".bss"));
        assert!(!obj.is_important_section(".debug_info"));
        assert!(!obj.is_important_section(".symtab"));
        assert!(!obj.is_important_section(".strtab"));
    }
}
