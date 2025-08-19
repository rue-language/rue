// Symbol export traits and implementations for x86-64 emitter
//
// This module defines traits for exporting symbols from the emitter
// in a way that's compatible with the linker's requirements.

use super::emitter::SectionType;
use std::collections::HashMap;

/// Trait for objects that can export symbols for linking
pub trait SymbolExporter {
    /// Export all symbols with their offsets within sections
    fn export_symbols(&self) -> SymbolExport;

    /// Export only global symbols (functions that should be visible externally)
    fn export_global_symbols(&self) -> HashMap<String, usize>;

    /// Export only local symbols (internal labels)
    fn export_local_symbols(&self) -> HashMap<String, usize>;
}

/// Container for exported symbol information
#[derive(Debug, Clone)]
pub struct SymbolExport {
    /// Global symbols (e.g., function entry points)
    pub global_symbols: HashMap<String, SymbolInfo>,
    /// Local symbols (e.g., internal labels)
    pub local_symbols: HashMap<String, SymbolInfo>,
    /// Section data
    pub sections: HashMap<SectionType, Vec<u8>>,
}

/// Information about a single symbol
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    /// Name of the symbol
    pub name: String,
    /// Offset within the section
    pub offset: usize,
    /// Section containing the symbol
    pub section: SectionType,
    /// Whether this symbol is global (visible externally)
    pub is_global: bool,
    /// Size of the symbol (0 for labels)
    pub size: usize,
}

impl SymbolInfo {
    /// Create a new symbol info
    pub fn new(name: String, offset: usize, section: SectionType, is_global: bool) -> Self {
        Self {
            name,
            offset,
            section,
            is_global,
            size: 0,
        }
    }

    /// Create a global symbol
    pub fn global(name: String, offset: usize, section: SectionType) -> Self {
        Self::new(name, offset, section, true)
    }

    /// Create a local symbol
    pub fn local(name: String, offset: usize, section: SectionType) -> Self {
        Self::new(name, offset, section, false)
    }

    /// Set the size of the symbol
    pub fn with_size(mut self, size: usize) -> Self {
        self.size = size;
        self
    }
}

impl SymbolExport {
    /// Create a new empty symbol export
    pub fn new() -> Self {
        Self {
            global_symbols: HashMap::new(),
            local_symbols: HashMap::new(),
            sections: HashMap::new(),
        }
    }

    /// Add a global symbol
    pub fn add_global(&mut self, info: SymbolInfo) {
        self.global_symbols.insert(info.name.clone(), info);
    }

    /// Add a local symbol
    pub fn add_local(&mut self, info: SymbolInfo) {
        self.local_symbols.insert(info.name.clone(), info);
    }

    /// Add section data
    pub fn add_section(&mut self, section_type: SectionType, data: Vec<u8>) {
        self.sections.insert(section_type, data);
    }

    /// Get all symbols as a flat map (for compatibility)
    pub fn flatten(&self) -> HashMap<String, usize> {
        let mut result = HashMap::new();

        // Add global symbols
        for (name, info) in &self.global_symbols {
            if info.section == SectionType::Text {
                result.insert(name.clone(), info.offset);
            }
        }

        // Add local symbols
        for (name, info) in &self.local_symbols {
            if info.section == SectionType::Text {
                result.insert(name.clone(), info.offset);
            }
        }

        result
    }

    /// Get only global symbols as a flat map
    pub fn flatten_globals(&self) -> HashMap<String, usize> {
        self.global_symbols
            .iter()
            .filter(|(_, info)| info.section == SectionType::Text)
            .map(|(name, info)| (name.clone(), info.offset))
            .collect()
    }
}

impl Default for SymbolExport {
    fn default() -> Self {
        Self::new()
    }
}
