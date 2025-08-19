// Assembler object model with symbols and relocations
//
// This module defines the internal representation for linkable objects
// produced by the assembler, including sections, symbols, and relocations.

use std::collections::HashMap;
use std::fmt;

/// Error type for assembler object operations
#[derive(Debug, Clone)]
pub enum AsmObjectError {
    /// Duplicate symbol definition
    DuplicateSymbol(String),
    /// No current section for operation
    NoCurrentSection,
    /// Section not found
    SectionNotFound(String),
    /// Invalid operation for NOBITS section
    InvalidNobitsOperation,
    /// Alignment error
    AlignmentError { required: u64, current: u64 },
}

impl fmt::Display for AsmObjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSymbol(name) => write!(f, "Duplicate symbol definition: {}", name),
            Self::NoCurrentSection => write!(f, "No current section for operation"),
            Self::SectionNotFound(name) => write!(f, "Section not found: {}", name),
            Self::InvalidNobitsOperation => write!(f, "Cannot emit bytes to NOBITS section"),
            Self::AlignmentError { required, current } => {
                write!(
                    f,
                    "Alignment error: required {}, current {}",
                    required, current
                )
            }
        }
    }
}

impl std::error::Error for AsmObjectError {}

pub type Result<T> = std::result::Result<T, AsmObjectError>;

/// Symbol binding (visibility)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymBind {
    Local,  // Only visible within this object
    Global, // Visible to other objects
    Weak,   // Can be overridden by global symbols
}

/// Symbol visibility (separate from binding)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymVis {
    Default,   // Default visibility
    Hidden,    // Hidden visibility (only visible within shared object)
    Protected, // Protected visibility (cannot be overridden)
}

/// Symbol definition status
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymDef {
    /// Symbol is defined in a section at an offset
    Defined { section_name: String, offset: u64 },
    /// Symbol is undefined (external reference)
    Undefined,
    /// Absolute symbol (has a fixed value)
    Absolute { value: u64 },
    /// Common symbol (needs BSS allocation)
    Common { size: u64, alignment: u64 },
}

/// Relocation kind for x86-64
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocKind {
    /// 64-bit absolute address (R_X86_64_64)
    Abs64,
    /// 32-bit PC-relative address (R_X86_64_PC32)
    Pc32,
    /// 32-bit PC-relative PLT address (R_X86_64_PLT32, treated same as PC32)
    Plt32,
    /// 32-bit PC-relative GOT address (R_X86_64_GOTPCREL)
    GotPcRel,
}

/// Symbol reference in instructions
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolRef {
    /// Reference to an internal label
    Internal(u32), // LabelId
    /// Reference to an external symbol
    External(String),
}

/// A symbol in the object file
#[derive(Debug, Clone)]
pub struct AsmSymbol {
    pub name: String,
    pub bind: SymBind,
    pub vis: SymVis,
    pub def: SymDef,
    pub size: u64,
    pub sym_type: u8, // STT_NOTYPE, STT_FUNC, STT_OBJECT, STT_SECTION, etc.
}

/// A relocation entry
#[derive(Debug, Clone)]
pub struct AsmReloc {
    /// Section containing the relocation site
    pub section_name: String,
    /// Offset within the section where relocation is applied
    pub offset: u64,
    /// Type of relocation
    pub kind: RelocKind,
    /// Symbol being referenced
    pub symbol_name: String,
    /// Addend to add to the symbol value
    pub addend: i64,
}

/// A section in the object file
#[derive(Debug, Clone)]
pub struct AsmSection {
    pub name: String,
    pub data: Vec<u8>,
    pub alignment: u64,
    /// Is this section executable?
    pub executable: bool,
    /// Is this section writable?
    pub writable: bool,
    /// Is this a NOBITS section (BSS)?
    pub is_nobits: bool,
    /// Size of NOBITS section (valid only if is_nobits is true)
    pub nobits_size: u64,
}

/// Linkable object produced by the assembler
#[derive(Debug, Clone)]
pub struct AsmObject {
    /// Sections in this object (.text, .rodata, .data, .bss)
    pub sections: Vec<AsmSection>,
    /// Symbols defined or referenced by this object
    pub symbols: Vec<AsmSymbol>,
    /// Relocations to be applied during linking
    pub relocs: Vec<AsmReloc>,
    /// Map from original symbol index to our symbol vector index
    pub symbol_index_map: HashMap<usize, usize>,
    /// Fast lookup from symbol name to index
    pub symbol_name_map: HashMap<String, usize>,
    /// Fast lookup from section name to index
    pub section_name_map: HashMap<String, usize>,
}

impl AsmObject {
    /// Create a new empty object
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
            symbols: Vec::new(),
            relocs: Vec::new(),
            symbol_index_map: HashMap::new(),
            symbol_name_map: HashMap::new(),
            section_name_map: HashMap::new(),
        }
    }

    /// Add a section to the object
    pub fn add_section(&mut self, section: AsmSection) {
        let index = self.sections.len();
        self.section_name_map.insert(section.name.clone(), index);
        self.sections.push(section);
    }

    /// Add a symbol to the object
    pub fn add_symbol(&mut self, symbol: AsmSymbol) {
        let index = self.symbols.len();
        self.symbol_name_map.insert(symbol.name.clone(), index);
        self.symbols.push(symbol);
    }

    /// Add a symbol to the object with its original index
    pub fn add_symbol_with_index(&mut self, original_index: usize, symbol: AsmSymbol) {
        let our_index = self.symbols.len();
        self.symbol_index_map.insert(original_index, our_index);
        self.symbol_name_map.insert(symbol.name.clone(), our_index);
        self.symbols.push(symbol);
    }

    /// Add a relocation to the object
    pub fn add_relocation(&mut self, reloc: AsmReloc) {
        self.relocs.push(reloc);
    }

    /// Find a section by name
    pub fn find_section(&self, name: &str) -> Option<&AsmSection> {
        self.section_name_map
            .get(name)
            .and_then(|&idx| self.sections.get(idx))
    }

    /// Find a symbol by name
    pub fn find_symbol(&self, name: &str) -> Option<&AsmSymbol> {
        self.symbol_name_map
            .get(name)
            .and_then(|&idx| self.symbols.get(idx))
    }

    /// Get all undefined external symbols
    pub fn undefined_symbols(&self) -> Vec<&AsmSymbol> {
        self.symbols
            .iter()
            .filter(|s| matches!(s.def, SymDef::Undefined))
            .collect()
    }

    /// Get all defined symbols
    pub fn defined_symbols(&self) -> Vec<&AsmSymbol> {
        self.symbols
            .iter()
            .filter(|s| matches!(s.def, SymDef::Defined { .. }))
            .collect()
    }
}

impl Default for AsmObject {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for creating AsmObject instances
pub struct AsmObjectBuilder {
    object: AsmObject,
    current_section: Option<String>,
    section_offsets: HashMap<String, u64>,
    symbol_map: HashMap<String, usize>,
    section_name_map: HashMap<String, usize>,
}

impl AsmObjectBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            object: AsmObject::new(),
            current_section: None,
            section_offsets: HashMap::new(),
            symbol_map: HashMap::new(),
            section_name_map: HashMap::new(),
        }
    }

    /// Start a new section or select existing one
    pub fn start_section(
        &mut self,
        name: String,
        alignment: u64,
        executable: bool,
        nobits: bool,
    ) -> Result<()> {
        // Check if section already exists
        if let Some(&idx) = self.section_name_map.get(&name) {
            // Section exists, just switch to it
            let section = &self.object.sections[idx];
            let current_offset = if section.is_nobits {
                section.nobits_size
            } else {
                section.data.len() as u64
            };
            self.current_section = Some(name.clone());
            self.section_offsets.insert(name, current_offset);
        } else {
            // Create new section
            let writable = match name.as_str() {
                ".text" => false,
                ".rodata" => false,
                ".data" => true,
                ".bss" => true,
                _ => !executable && nobits, // NOBITS sections are typically writable
            };

            let section = AsmSection {
                name: name.clone(),
                data: Vec::new(),
                alignment,
                executable,
                writable,
                is_nobits: nobits,
                nobits_size: 0,
            };

            let idx = self.object.sections.len();
            self.section_name_map.insert(name.clone(), idx);
            self.object.add_section(section);
            self.section_offsets.insert(name.clone(), 0);
            self.current_section = Some(name);
        }
        Ok(())
    }

    /// Add data to the current section and return the offset where bytes were written
    pub fn emit_bytes(&mut self, bytes: &[u8]) -> Result<u64> {
        let section_name = self
            .current_section
            .as_ref()
            .ok_or(AsmObjectError::NoCurrentSection)?;

        let offset = *self.section_offsets.get(section_name).unwrap_or(&0);

        // Find and update the section
        let section_idx = *self
            .section_name_map
            .get(section_name)
            .ok_or_else(|| AsmObjectError::SectionNotFound(section_name.clone()))?;

        let section = &mut self.object.sections[section_idx];

        if section.is_nobits {
            // For NOBITS sections, just update the size
            section.nobits_size += bytes.len() as u64;
        } else {
            // For normal sections, append the bytes
            section.data.extend_from_slice(bytes);
        }

        // Update offset
        self.section_offsets
            .insert(section_name.clone(), offset + bytes.len() as u64);
        Ok(offset)
    }

    /// Get the current byte position in the current section
    pub fn current_offset(&self) -> Result<u64> {
        let section_name = self
            .current_section
            .as_ref()
            .ok_or(AsmObjectError::NoCurrentSection)?;
        Ok(*self.section_offsets.get(section_name).unwrap_or(&0))
    }

    /// Align the current section position to the specified alignment
    pub fn align_to(&mut self, alignment: u64) -> Result<()> {
        if alignment == 0 || !alignment.is_power_of_two() {
            return Ok(()); // Invalid alignment, skip
        }

        let section_name = self
            .current_section
            .as_ref()
            .ok_or(AsmObjectError::NoCurrentSection)?;

        let current_offset = *self.section_offsets.get(section_name).unwrap_or(&0);
        let aligned_offset = (current_offset + alignment - 1) & !(alignment - 1);

        if aligned_offset > current_offset {
            let padding = aligned_offset - current_offset;
            let section_idx = *self
                .section_name_map
                .get(section_name)
                .ok_or_else(|| AsmObjectError::SectionNotFound(section_name.clone()))?;

            let section = &mut self.object.sections[section_idx];

            if section.is_nobits {
                // For NOBITS, just increase the size
                section.nobits_size += padding;
            } else {
                // For normal sections, add padding bytes
                section
                    .data
                    .resize(section.data.len() + padding as usize, 0);
            }

            self.section_offsets
                .insert(section_name.clone(), aligned_offset);
        }

        Ok(())
    }

    /// Get the current section name
    pub fn get_current_section_name(&self) -> Option<String> {
        self.current_section.clone()
    }

    /// Define a symbol at the current position
    pub fn define_symbol(&mut self, name: String, bind: SymBind, size: u64) -> Result<()> {
        self.define_symbol_with_vis(name, bind, SymVis::Default, size)
    }

    /// Define a symbol at the current position with visibility
    pub fn define_symbol_with_vis(
        &mut self,
        name: String,
        bind: SymBind,
        vis: SymVis,
        size: u64,
    ) -> Result<()> {
        // Check if symbol already defined
        if self.symbol_map.contains_key(&name) {
            return Err(AsmObjectError::DuplicateSymbol(name));
        }

        let section_name = self
            .current_section
            .as_ref()
            .ok_or(AsmObjectError::NoCurrentSection)?;

        let offset = *self.section_offsets.get(section_name).unwrap_or(&0);
        let symbol = AsmSymbol {
            name: name.clone(),
            bind,
            vis,
            def: SymDef::Defined {
                section_name: section_name.clone(),
                offset,
            },
            size,
            sym_type: 0, // STT_NOTYPE
        };
        let idx = self.object.symbols.len();
        self.object.add_symbol(symbol);
        self.symbol_map.insert(name, idx);
        Ok(())
    }

    /// Reference an external symbol
    pub fn reference_external(&mut self, name: String) -> Result<()> {
        // Only add if not already present
        if !self.symbol_map.contains_key(&name) {
            let symbol = AsmSymbol {
                name: name.clone(),
                bind: SymBind::Global,
                vis: SymVis::Default,
                def: SymDef::Undefined,
                size: 0,
                sym_type: 0, // STT_NOTYPE
            };
            let idx = self.object.symbols.len();
            self.object.add_symbol(symbol);
            self.symbol_map.insert(name, idx);
        }
        Ok(())
    }

    /// Add a relocation for the current section
    pub fn add_relocation(
        &mut self,
        offset: u64,
        kind: RelocKind,
        symbol_name: String,
        addend: i64,
    ) -> Result<()> {
        let section_name = self
            .current_section
            .as_ref()
            .ok_or(AsmObjectError::NoCurrentSection)?;

        let reloc = AsmReloc {
            section_name: section_name.clone(),
            offset,
            kind,
            symbol_name,
            addend,
        };
        self.object.add_relocation(reloc);
        Ok(())
    }

    /// Patch an 8-bit value at the given offset in the current section
    pub fn patch_u8(&mut self, offset: u64, value: u8) -> Result<()> {
        let section_name = self
            .current_section
            .as_ref()
            .ok_or(AsmObjectError::NoCurrentSection)?
            .clone();
        self.patch_u8_in_section(&section_name, offset, value)
    }

    /// Patch an 8-bit value at the given offset in the specified section
    pub fn patch_u8_in_section(
        &mut self,
        section_name: &str,
        offset: u64,
        value: u8,
    ) -> Result<()> {
        let section_idx = *self
            .section_name_map
            .get(section_name)
            .ok_or_else(|| AsmObjectError::SectionNotFound(section_name.to_string()))?;

        let section = &mut self.object.sections[section_idx];
        if section.is_nobits {
            return Err(AsmObjectError::InvalidNobitsOperation);
        }

        let offset = offset as usize;
        if offset < section.data.len() {
            section.data[offset] = value;
        }
        Ok(())
    }

    /// Patch a 16-bit value at the given offset in the current section
    pub fn patch_u16(&mut self, offset: u64, value: u16) -> Result<()> {
        let section_name = self
            .current_section
            .as_ref()
            .ok_or(AsmObjectError::NoCurrentSection)?
            .clone();
        self.patch_u16_in_section(&section_name, offset, value)
    }

    /// Patch a 16-bit value at the given offset in the specified section
    pub fn patch_u16_in_section(
        &mut self,
        section_name: &str,
        offset: u64,
        value: u16,
    ) -> Result<()> {
        let section_idx = *self
            .section_name_map
            .get(section_name)
            .ok_or_else(|| AsmObjectError::SectionNotFound(section_name.to_string()))?;

        let section = &mut self.object.sections[section_idx];
        if section.is_nobits {
            return Err(AsmObjectError::InvalidNobitsOperation);
        }

        let offset = offset as usize;
        if offset + 2 <= section.data.len() {
            let bytes = value.to_le_bytes();
            section.data[offset..offset + 2].copy_from_slice(&bytes);
        }
        Ok(())
    }

    /// Patch a 32-bit value at the given offset in the current section
    pub fn patch_i32(&mut self, offset: u64, value: i32) -> Result<()> {
        let section_name = self
            .current_section
            .as_ref()
            .ok_or(AsmObjectError::NoCurrentSection)?
            .clone();
        self.patch_i32_in_section(&section_name, offset, value)
    }

    /// Patch a 32-bit value at the given offset in the specified section
    pub fn patch_i32_in_section(
        &mut self,
        section_name: &str,
        offset: u64,
        value: i32,
    ) -> Result<()> {
        let section_idx = *self
            .section_name_map
            .get(section_name)
            .ok_or_else(|| AsmObjectError::SectionNotFound(section_name.to_string()))?;

        let section = &mut self.object.sections[section_idx];
        if section.is_nobits {
            return Err(AsmObjectError::InvalidNobitsOperation);
        }

        let offset = offset as usize;
        if offset + 4 <= section.data.len() {
            let bytes = value.to_le_bytes();
            section.data[offset..offset + 4].copy_from_slice(&bytes);
        }
        Ok(())
    }

    /// Patch a 64-bit value at the given offset in the current section
    pub fn patch_u64(&mut self, offset: u64, value: u64) -> Result<()> {
        let section_name = self
            .current_section
            .as_ref()
            .ok_or(AsmObjectError::NoCurrentSection)?
            .clone();
        self.patch_u64_in_section(&section_name, offset, value)
    }

    /// Patch a 64-bit value at the given offset in the specified section
    pub fn patch_u64_in_section(
        &mut self,
        section_name: &str,
        offset: u64,
        value: u64,
    ) -> Result<()> {
        let section_idx = *self
            .section_name_map
            .get(section_name)
            .ok_or_else(|| AsmObjectError::SectionNotFound(section_name.to_string()))?;

        let section = &mut self.object.sections[section_idx];
        if section.is_nobits {
            return Err(AsmObjectError::InvalidNobitsOperation);
        }

        let offset = offset as usize;
        if offset + 8 <= section.data.len() {
            let bytes = value.to_le_bytes();
            section.data[offset..offset + 8].copy_from_slice(&bytes);
        }
        Ok(())
    }

    /// Build the final object
    pub fn build(self) -> AsmObject {
        self.object
    }
}

impl Default for AsmObjectBuilder {
    fn default() -> Self {
        Self::new()
    }
}
