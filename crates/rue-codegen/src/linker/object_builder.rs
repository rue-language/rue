// Object file builder for creating ELF64 object files
//
// This module provides a builder pattern for creating ELF64 object files
// from generated machine code and symbols. This is used to package user
// code into an object file format that can be linked with runtime libraries.

use crate::CodegenError;
use object::write::{Object, Relocation, StandardSection, Symbol, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationFlags, SymbolFlags, SymbolKind, SymbolScope,
};
use std::collections::HashMap;

/// Builder for creating ELF64 object files from generated code
pub struct ObjectFileBuilder {
    /// The object being built
    object: Object<'static>,
    /// Map of symbol names to their offsets in the text section
    symbols: HashMap<String, (usize, SymbolScope)>,
    /// Section ID for the user text section
    text_section_id: Option<object::write::SectionId>,
}

impl ObjectFileBuilder {
    /// Create a new object file builder for x86-64 ELF
    pub fn new() -> Self {
        let object = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);

        // Don't pre-create sections - let them be created on demand
        // This ensures section IDs match what we expect

        Self {
            object,
            symbols: HashMap::new(),
            text_section_id: None,
        }
    }

    /// Add text (code) section data
    pub fn add_text_section(&mut self, code: &[u8]) -> Result<(), CodegenError> {
        // Create a custom section for user code to avoid overwriting runtime sections
        // User code goes into .text.user instead of .text
        let section = self.object.add_section(
            Vec::new(),
            b".text.user".to_vec(),
            object::SectionKind::Text,
        );
        self.object.section_mut(section).set_data(code.to_vec(), 16); // 16-byte alignment for x86-64
        self.text_section_id = Some(section);
        Ok(())
    }

    /// Add a symbol to the object file
    pub fn add_symbol(
        &mut self,
        name: String,
        offset: usize,
        is_global: bool,
    ) -> Result<(), CodegenError> {
        let scope = if is_global {
            SymbolScope::Dynamic
        } else {
            SymbolScope::Compilation
        };
        self.symbols.insert(name, (offset, scope));
        Ok(())
    }

    /// Add data section content
    pub fn add_data_section(&mut self, data: &[u8]) -> Result<(), CodegenError> {
        if !data.is_empty() {
            let section = self.object.section_id(StandardSection::Data);
            self.object.section_mut(section).set_data(data.to_vec(), 8);
        }
        Ok(())
    }

    /// Set BSS section size
    pub fn set_bss_size(&mut self, size: usize) -> Result<(), CodegenError> {
        if size > 0 {
            let section = self.object.section_id(StandardSection::UninitializedData);
            self.object.section_mut(section).append_bss(size as u64, 8);
        }
        Ok(())
    }

    /// Add a relocation for an external symbol reference
    pub fn add_external_relocation(
        &mut self,
        symbol_name: String,
        offset: u64,
    ) -> Result<(), CodegenError> {
        tracing::info!(
            "Adding external relocation: symbol='{}' at offset=0x{:x}",
            symbol_name,
            offset
        );

        // Create an undefined symbol for the external reference
        let symbol = Symbol {
            name: symbol_name.clone().into_bytes(),
            value: 0,
            size: 0,
            kind: SymbolKind::Text,
            scope: SymbolScope::Dynamic,
            weak: false,
            section: SymbolSection::Undefined,
            flags: SymbolFlags::None,
        };
        let symbol_id = self.object.add_symbol(symbol);

        // Add the relocation to the text section
        let text_section = self
            .text_section_id
            .ok_or_else(|| CodegenError::InvalidOperation("Text section not created yet".into()))?;
        let relocation = Relocation {
            offset,
            symbol: symbol_id,
            addend: -4, // Standard PLT32 addend for call instructions
            flags: RelocationFlags::Elf {
                r_type: object::elf::R_X86_64_PLT32,
            },
        };

        tracing::debug!(
            "Created PLT32 relocation: offset=0x{:x}, addend=-4, symbol_id={:?}",
            offset,
            symbol_id
        );

        self.object
            .add_relocation(text_section, relocation)
            .map_err(|e| {
                CodegenError::InvalidOperation(format!("Failed to add relocation: {}", e))
            })?;

        tracing::info!(
            "Successfully added external relocation for '{}'",
            symbol_name
        );
        Ok(())
    }

    /// Build the final object file
    pub fn build(mut self) -> Result<Vec<u8>, CodegenError> {
        // Add all symbols to the object
        let text_section = self
            .text_section_id
            .ok_or_else(|| CodegenError::InvalidOperation("Text section not created".into()))?;

        for (name, (offset, scope)) in self.symbols {
            let mut symbol = Symbol {
                name: name.into_bytes(),
                value: offset as u64,
                size: 0,
                kind: SymbolKind::Text,
                scope,
                weak: false,
                section: SymbolSection::Section(text_section),
                flags: SymbolFlags::None,
            };

            // Special handling for 'main' - always make it global
            if std::str::from_utf8(&symbol.name).unwrap_or("") == "main" {
                symbol.scope = SymbolScope::Dynamic;
            }

            self.object.add_symbol(symbol);
        }

        // Write the object file to bytes
        self.object.write().map_err(|e| {
            CodegenError::InvalidOperation(format!("Failed to write object file: {}", e))
        })
    }
}

/// Create an object file from user code and symbols
pub fn create_user_object(
    code: &[u8],
    symbols: &HashMap<String, usize>,
    data: &[u8],
    bss_size: usize,
) -> Result<Vec<u8>, CodegenError> {
    create_user_object_with_externals(code, symbols, data, bss_size, &[])
}

/// Create an object file from user code, symbols, and external references
pub fn create_user_object_with_externals(
    code: &[u8],
    symbols: &HashMap<String, usize>,
    data: &[u8],
    bss_size: usize,
    external_refs: &[(String, usize)],
) -> Result<Vec<u8>, CodegenError> {
    let mut builder = ObjectFileBuilder::new();

    // Add text section
    builder.add_text_section(code)?;

    // Add symbols
    for (name, offset) in symbols {
        // Make all user symbols global by default
        // User code symbols should be globally visible for linking
        let is_global = true;
        builder.add_symbol(name.clone(), *offset, is_global)?;
    }

    // Add external symbol relocations
    for (symbol_name, offset) in external_refs {
        builder.add_external_relocation(symbol_name.clone(), *offset as u64)?;
    }

    // Add data section if present
    if !data.is_empty() {
        builder.add_data_section(data)?;
    }

    // Set BSS size if present
    if bss_size > 0 {
        builder.set_bss_size(bss_size)?;
    }

    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_simple_object() {
        let code = vec![0x90; 16]; // NOP instructions
        let mut symbols = HashMap::new();
        symbols.insert("main".to_string(), 0);

        let result = create_user_object(&code, &symbols, &[], 0);
        assert!(result.is_ok());

        let object_bytes = result.unwrap();
        assert!(!object_bytes.is_empty());

        // Verify it's a valid ELF file (magic number)
        assert_eq!(&object_bytes[0..4], b"\x7fELF");
    }

    #[test]
    fn test_object_with_multiple_symbols() {
        let code = vec![0x90; 64];
        let mut symbols = HashMap::new();
        symbols.insert("main".to_string(), 0);
        symbols.insert("helper".to_string(), 16);
        symbols.insert("_start".to_string(), 32);

        let result = create_user_object(&code, &symbols, &[], 0);
        assert!(result.is_ok());
    }

    #[test]
    fn test_object_with_data_and_bss() {
        let code = vec![0x90; 32];
        let data = vec![0x42; 16];
        let mut symbols = HashMap::new();
        symbols.insert("main".to_string(), 0);

        let result = create_user_object(&code, &symbols, &data, 256);
        assert!(result.is_ok());
    }
}
