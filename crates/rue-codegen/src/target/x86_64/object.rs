//! Object file emission for x86-64
//!
//! Generates relocatable ELF object files (.o) that can be linked with external libraries.

use object::write::{Object, Relocation, StandardSection, Symbol, SymbolSection};
use object::{Architecture, BinaryFormat, Endianness, SymbolFlags, SymbolKind, SymbolScope};
use std::collections::HashMap;

use super::emitter::SectionType;

/// Converts our SectionType to object crate's StandardSection
fn section_type_to_standard(section_type: &SectionType) -> StandardSection {
    match section_type {
        SectionType::Text => StandardSection::Text,
        SectionType::Data => StandardSection::Data,
        SectionType::Bss => StandardSection::UninitializedData,
        SectionType::Rodata => StandardSection::ReadOnlyData,
    }
}

/// Information about a relocation that needs to be applied
#[derive(Debug, Clone)]
pub struct RelocationInfo {
    pub section: SectionType,
    pub offset: u64,
    pub symbol_name: String,
    pub addend: i64,
}

/// Object file emitter for generating relocatable .o files
pub struct ObjectFileEmitter {
    object: Object<'static>,
    section_ids: HashMap<SectionType, object::write::SectionId>,
    symbol_ids: HashMap<String, object::write::SymbolId>,
}

impl ObjectFileEmitter {
    /// Create a new object file emitter
    pub fn new() -> Self {
        let mut object = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);

        // Pre-create standard sections
        let mut section_ids = HashMap::new();

        let text_id = object.section_id(StandardSection::Text);
        section_ids.insert(SectionType::Text, text_id);

        let data_id = object.section_id(StandardSection::Data);
        section_ids.insert(SectionType::Data, data_id);

        let rodata_id = object.section_id(StandardSection::ReadOnlyData);
        section_ids.insert(SectionType::Rodata, rodata_id);

        let bss_id = object.section_id(StandardSection::UninitializedData);
        section_ids.insert(SectionType::Bss, bss_id);

        Self {
            object,
            section_ids,
            symbol_ids: HashMap::new(),
        }
    }

    /// Add section data
    pub fn add_section_data(&mut self, section_type: SectionType, data: Vec<u8>) {
        let section_id = self.section_ids[&section_type];
        self.object.append_section_data(section_id, &data, 16);
    }

    /// Add a defined symbol (e.g., _start, main, __rue_main)
    pub fn add_defined_symbol(
        &mut self,
        name: String,
        section: SectionType,
        offset: u64,
        is_global: bool,
    ) {
        let section_id = self.section_ids[&section];

        let symbol = Symbol {
            name: name.as_bytes().to_vec(),
            value: offset,
            size: 0,
            kind: SymbolKind::Text,
            scope: if is_global {
                SymbolScope::Linkage
            } else {
                SymbolScope::Compilation
            },
            weak: false,
            section: SymbolSection::Section(section_id),
            flags: SymbolFlags::None,
        };

        let symbol_id = self.object.add_symbol(symbol);
        self.symbol_ids.insert(name, symbol_id);
    }

    /// Add an undefined external symbol (e.g., __rue_println_i64)
    pub fn add_undefined_symbol(&mut self, name: String) {
        let symbol = Symbol {
            name: name.as_bytes().to_vec(),
            value: 0,
            size: 0,
            kind: SymbolKind::Text,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Undefined,
            flags: SymbolFlags::None,
        };

        let symbol_id = self.object.add_symbol(symbol);
        self.symbol_ids.insert(name, symbol_id);
    }

    /// Add a relocation for an external symbol
    pub fn add_relocation(&mut self, reloc: RelocationInfo) {
        let section_id = self.section_ids[&reloc.section];

        // Get or create the symbol
        let symbol_id = if let Some(&id) = self.symbol_ids.get(&reloc.symbol_name) {
            id
        } else {
            // Symbol not defined, add as undefined external
            self.add_undefined_symbol(reloc.symbol_name.clone());
            self.symbol_ids[&reloc.symbol_name]
        };

        // PC-relative call uses R_X86_64_PLT32 relocation
        let relocation = Relocation {
            offset: reloc.offset,
            symbol: symbol_id,
            addend: reloc.addend,
            flags: object::RelocationFlags::Elf {
                r_type: object::elf::R_X86_64_PLT32,
            },
        };

        self.object.add_relocation(section_id, relocation).unwrap();
    }

    /// Write the object file to bytes
    pub fn write(self) -> Result<Vec<u8>, String> {
        self.object
            .write()
            .map_err(|e| format!("Failed to write object file: {}", e))
    }
}
