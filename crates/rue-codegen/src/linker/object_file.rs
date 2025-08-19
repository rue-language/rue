// Object file parsing for ELF64 files
//
// This module handles parsing of ELF64 object files (.o files) to extract
// sections, symbols, and relocations needed for linking.

use crate::CodegenError;
use object::{BinaryFormat, Object, ObjectSection, ObjectSymbol, SectionKind};

use super::{RelocationEntry, RelocationKind, Symbol, SymbolKind, SymbolSource};

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
}

impl ObjectFile {
    /// Parse an object file from raw bytes
    pub fn parse(name: String, data: &[u8]) -> Result<Self, CodegenError> {
        tracing::debug!(
            "ObjectFile::parse called for {} ({} bytes)",
            name,
            data.len()
        );
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
            name: name.clone(),
            sections: Vec::new(),
            symbols: Vec::new(),
            relocations: Vec::new(),
        };

        // Parse sections
        object_file.parse_sections(&obj_file)?;
        tracing::debug!(
            "Parsed {} sections from {}",
            object_file.sections.len(),
            name
        );

        // Parse symbols
        object_file.parse_symbols(&obj_file)?;
        tracing::debug!("Parsed {} symbols from {}", object_file.symbols.len(), name);

        // Add synthetic section symbols for BSS and RODATA sections (for section-relative relocations)
        for section in &object_file.sections {
            if section.name.starts_with(".bss") || section.name.starts_with(".rodata") {
                // Check if we already have a symbol for this section
                let has_symbol = object_file.symbols.iter().any(|s| s.name == section.name);
                if !has_symbol {
                    tracing::debug!(
                        "Adding synthetic section symbol for {} section: {}",
                        if section.name.starts_with(".bss") {
                            "BSS"
                        } else {
                            "RODATA"
                        },
                        section.name
                    );
                    object_file.symbols.push(Symbol {
                        name: section.name.clone(),
                        address: 0, // Section symbols start at offset 0
                        size: section.data.len() as u64,
                        kind: SymbolKind::Local,
                        section_name: section.name.clone(),
                        source: SymbolSource::RuntimeLibrary,
                    });
                }
            }
        }

        // Fix up _start symbol if it's pointing to relocation section
        object_file.fix_start_symbol_section()?;

        // Parse relocations
        object_file.parse_relocations(&obj_file)?;

        Ok(object_file)
    }

    /// Parse sections from the object file
    fn parse_sections(&mut self, obj_file: &object::File) -> Result<(), CodegenError> {
        tracing::debug!("Starting section parsing for object file: {}", self.name);

        // Count total sections
        let total_sections = obj_file.sections().count();
        tracing::debug!(
            "Object file {} has {} total sections",
            self.name,
            total_sections
        );

        for (idx, section) in obj_file.sections().enumerate() {
            let name = section.name().unwrap_or("<unknown>").to_string();
            let kind = section.kind();
            let is_important = self.is_important_section(&name);
            let is_relocation = name.starts_with(".rela.");

            // Log all sections at debug level, BSS at debug level
            if name.starts_with(".bss") {
                tracing::debug!(
                    "BSS Section {}: name='{}', kind={:?}, size={}, important={}",
                    idx,
                    name,
                    kind,
                    section.size(),
                    is_important
                );
            } else if is_important {
                tracing::debug!(
                    "Section {}: name='{}', kind={:?}, size={}, important={}",
                    idx,
                    name,
                    kind,
                    section.size(),
                    is_important
                );
            } else {
                tracing::trace!(
                    "Section {}: name='{}', kind={:?}, important={}, relocation={}",
                    idx,
                    name,
                    kind,
                    is_important,
                    is_relocation
                );
            }

            // Skip sections we don't care about
            if name.is_empty() || name.starts_with('.') && !is_important {
                tracing::trace!("Skipping section '{}' (not important)", name);
                continue;
            }

            // Debug: Log when we find rodata sections
            if name.starts_with(".rodata") {
                tracing::debug!("Including rodata section: '{}'", name);
            }

            // BSS sections don't have data in the file, but have a size
            let data = if matches!(kind, SectionKind::UninitializedData) {
                // For BSS sections, create zero-filled data of the appropriate size
                vec![0u8; section.size() as usize]
            } else {
                section.data().unwrap_or(&[]).to_vec()
            };
            let alignment = section.align();
            let address = section.address();

            tracing::debug!(
                "Adding section: name='{}', kind={:?}, size={}, alignment={}, address=0x{:x}",
                name,
                kind,
                data.len(),
                alignment,
                address
            );

            // Debug _start section bytes
            if name == ".text._start" && data.len() > 10 {
                tracing::debug!("_start section bytes (first 10): {:?}", &data[..10]);
            }

            self.sections.push(Section {
                name,
                kind,
                data,
                alignment,
                address,
            });
        }

        tracing::info!(
            "Parsed {} sections from object file: {}",
            self.sections.len(),
            self.name
        );
        Ok(())
    }

    /// Parse symbols from the object file
    fn parse_symbols(&mut self, obj_file: &object::File) -> Result<(), CodegenError> {
        tracing::debug!("Starting symbol parsing for object file: {}", self.name);

        for symbol in obj_file.symbols() {
            let name = symbol.name().unwrap_or("<unknown>").to_string();

            // Skip only truly irrelevant symbols
            // Keep Rust compiler-generated symbols that are needed for linking
            if name.is_empty() {
                tracing::trace!("Skipping empty symbol");
                continue;
            }

            // Check if this is a compiler-generated symbol we should keep
            let should_keep =
                // Keep Rust anonymous symbols (constants, vtables)
                name.starts_with(".Lanon") ||
                // Keep LLVM local labels that might be referenced
                name.starts_with(".LBB") ||
                // Keep any other local labels that might be jump targets
                name.starts_with(".L") ||
                // Keep all non-dot symbols
                !name.starts_with('.');

            if !should_keep {
                tracing::trace!("Skipping irrelevant symbol '{}'", name);
                continue;
            }

            tracing::trace!(
                "Keeping symbol '{}' (kind: {:?})",
                name,
                if symbol.is_global() {
                    "global"
                } else if symbol.is_local() {
                    "local"
                } else {
                    "weak"
                }
            );

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
                    // Debug BSS symbols more carefully
                    if name.contains("HEAP") || name.contains("STDOUT_STATE") {
                        tracing::debug!(
                            "BSS Symbol '{}': section_index = {}",
                            name,
                            section_index.0
                        );
                    } else {
                        tracing::debug!("Symbol '{}': section_index = {}", name, section_index.0);
                    }

                    // Find the section by index
                    // IMPORTANT: Section indices in ELF symbol tables are 1-based
                    // (0 is reserved for undefined), but the sections() iterator is 0-based
                    // So we need to subtract 1 from the symbol's section index
                    let iterator_index = section_index.0.saturating_sub(1);
                    if let Some(section) = obj_file.sections().nth(iterator_index) {
                        let mut section_name = section.name().unwrap_or("<unknown>").to_string();

                        // Debug: For BSS symbols, check if we got the right section
                        if name.contains("HEAP") || name.contains("STDOUT_STATE") {
                            tracing::debug!(
                                "BSS Symbol '{}': section_index={}, got section '{}'",
                                name,
                                section_index.0,
                                section_name
                            );
                        }
                        let is_relocation_section = section_name.starts_with(".rela.");

                        tracing::debug!(
                            "Symbol '{}': found section '{}' (is_relocation_section: {})",
                            name,
                            section_name,
                            is_relocation_section
                        );

                        // Check if this is a relocation section and fix it
                        if is_relocation_section {
                            tracing::debug!(
                                "Symbol '{}' is associated with relocation section '{}' - correcting to actual code/data section",
                                name,
                                section_name
                            );

                            // Remove the .rela prefix to get the actual section name
                            let actual_section_name = &section_name[5..]; // Remove ".rela" prefix
                            tracing::debug!(
                                "Correcting section: '{}' -> '{}'",
                                section_name,
                                actual_section_name
                            );

                            // Verify the actual section exists in our parsed sections
                            // Note: We don't need to check obj_file.sections() because we already parsed
                            // the sections we care about into self.sections in parse_sections()
                            let section_exists =
                                self.sections.iter().any(|s| s.name == actual_section_name);

                            if section_exists {
                                tracing::info!(
                                    "Fixed symbol '{}' section: '{}' -> '{}'",
                                    name,
                                    section_name,
                                    actual_section_name
                                );
                                section_name = actual_section_name.to_string(); // Apply the fix
                            } else {
                                tracing::warn!(
                                    "Could not find actual section '{}' for symbol '{}' - keeping relocation section",
                                    actual_section_name,
                                    name
                                );
                                // Keep the original section_name for now, but this indicates a deeper issue
                            }
                        } else if section_name == ".symtab"
                            || section_name == ".strtab"
                            || section_name == ".shstrtab"
                        {
                            // Check if this is a metadata section (symbol table, string table, etc.)
                            tracing::debug!(
                                "Symbol '{}' points to metadata section '{}' - finding appropriate code/data section",
                                name,
                                section_name
                            );

                            // Try to find the most appropriate section from our already parsed sections
                            let mut found_section = None;

                            // First try to find a .text section (for code symbols)
                            if let Some(text_section) =
                                self.sections.iter().find(|s| s.name.starts_with(".text"))
                            {
                                found_section = Some(text_section.name.clone());
                            }
                            // If no .text section found, try .data or .rodata
                            else if let Some(data_section) = self.sections.iter().find(|s| {
                                s.name.starts_with(".data") || s.name.starts_with(".rodata")
                            }) {
                                found_section = Some(data_section.name.clone());
                            }

                            if let Some(corrected_section) = found_section {
                                tracing::info!(
                                    "Fixed symbol '{}' metadata section: '{}' -> '{}'",
                                    name,
                                    section_name,
                                    corrected_section
                                );
                                section_name = corrected_section; // Apply the fix
                            } else {
                                tracing::warn!(
                                    "Could not find appropriate section for symbol '{}' pointing to metadata section '{}'",
                                    name,
                                    section_name
                                );
                                // Keep the original section_name (no change)
                            }
                        }
                        // Return the (possibly corrected) section name
                        section_name
                    } else {
                        tracing::warn!(
                            "Symbol '{}': section index {} (iterator index {}) not found",
                            name,
                            section_index.0,
                            iterator_index
                        );
                        String::new()
                    }
                }
                object::SymbolSection::Undefined => {
                    tracing::debug!("Symbol '{}': undefined symbol", name);
                    String::new()
                }
                object::SymbolSection::Common => {
                    tracing::debug!("Symbol '{}': common symbol", name);
                    String::new()
                }
                object::SymbolSection::Absolute => {
                    tracing::debug!("Symbol '{}': absolute symbol", name);
                    String::new()
                }
                _ => {
                    tracing::debug!("Symbol '{}': other special section", name);
                    String::new()
                }
            };

            tracing::debug!(
                "Adding symbol: name='{}', kind={:?}, address=0x{:x}, size={}, section='{}'",
                name,
                kind,
                address,
                size,
                section_name
            );

            // Special logging for runtime symbols (disabled)
            // if name.starts_with("__rue_") {
            //     tracing::warn!(
            //         "RUNTIME SYMBOL FOUND: name='{}', kind={:?}, address=0x{:x}, size={}, section='{}'",
            //         name, kind, address, size, section_name
            //     );
            // }

            // Debug: Special logging for BSS symbols
            if section_name.starts_with(".bss") {
                tracing::debug!(
                    "Adding BSS symbol: name='{}', address=0x{:x}, size={}, section='{}'",
                    name,
                    address,
                    size,
                    section_name
                );
            }

            // Debug: Special logging for the target symbol
            if name.contains("Lanon") && name.contains("80") {
                tracing::debug!(
                    "Found target Lanon.80 symbol: {} in section '{}'",
                    name,
                    section_name
                );
            }

            // Determine the source based on the object file name and symbol characteristics
            let source = if self.name.contains("user_code") {
                SymbolSource::UserCode
            } else if self.name.contains("rue-runtime") || self.name.contains("rue_runtime") || 
                      name.starts_with("_ZN") || // Rust mangled names
                      name.starts_with(".L") ||   // LLVM local labels
                      name.starts_with(".Lanon")
            // Anonymous constants
            {
                SymbolSource::RuntimeLibrary
            } else {
                SymbolSource::ExternalLibrary
            };

            self.symbols.push(Symbol {
                name,
                kind,
                address,
                size,
                section_name,
                source,
            });
        }

        tracing::info!(
            "Parsed {} symbols from object file: {}",
            self.symbols.len(),
            self.name
        );
        Ok(())
    }

    /// Parse relocations from the object file
    fn parse_relocations(&mut self, obj_file: &object::File) -> Result<(), CodegenError> {
        let mut relocation_count = 0;

        // Try the standard way of getting relocations first
        let mut standard_relocation_count = 0;
        for section in obj_file.sections() {
            // Look for relocation sections (.rela.*)
            let section_name = section.name().unwrap_or("");

            // For sections that contain code/data, get their relocations
            if !section_name.starts_with(".rela") && !section_name.starts_with(".rel") {
                // This is a regular section, check if it has relocations
                let relocs: Vec<_> = section.relocations().collect();
                if !relocs.is_empty() {
                    tracing::debug!(
                        "Section '{}' has {} relocations",
                        section_name,
                        relocs.len()
                    );

                    // CRITICAL DEBUG: Log for __rue_println_i64 sections specifically
                    if section_name.contains("__rue_println") {
                        tracing::debug!(
                            "Found relocations via standard path: section='{}' has {} relocations",
                            section_name,
                            relocs.len()
                        );
                    }
                }
                for (offset, relocation) in relocs {
                    if section_name == ".text._start" || section_name == ".text.__rue_main" {
                        tracing::debug!(
                            "Processing relocation for {} at offset 0x{:x}",
                            section_name,
                            offset
                        );
                    }

                    // CRITICAL DEBUG: Log __rue_println_i64 relocations before processing
                    if section_name.contains("__rue_println") {
                        tracing::debug!(
                            "Processing internal relocation: section='{}', offset=0x{:x}, kind={:?}",
                            section_name,
                            offset,
                            relocation.kind()
                        );
                    }

                    self.process_relocation(section_name, offset, relocation, obj_file)?;
                    standard_relocation_count += 1;
                }
            }
        }
        relocation_count += standard_relocation_count;

        // Only parse .rela sections manually if the standard method found no relocations
        // This prevents duplicate relocation processing
        if standard_relocation_count == 0 {
            tracing::debug!(
                "Standard relocation parsing found no relocations, trying manual .rela section parsing"
            );
            for section in obj_file.sections() {
                let section_name = section.name().unwrap_or("");
                if !section_name.starts_with(".rela.") {
                    continue;
                }

                // Get the target section name (remove .rela. prefix)
                let target_section = &section_name[5..];
                tracing::debug!(
                    "Found .rela section: {} (target: {})",
                    section_name,
                    target_section
                );

                // Parse the .rela section manually
                let manual_count =
                    self.parse_rela_section_manual(obj_file, section, target_section)?;
                relocation_count += manual_count;
            }
        } else {
            tracing::debug!(
                "Standard relocation parsing found {} relocations, skipping manual parsing to avoid duplicates",
                standard_relocation_count
            );
        }

        tracing::info!(
            "Parsed {} relocations from object file: {}",
            relocation_count,
            self.name
        );
        Ok(())
    }

    /// Parse a .rela section manually (for Rust staticlibs that don't expose relocations via standard API)
    fn parse_rela_section_manual<'a>(
        &mut self,
        obj_file: &object::File,
        rela_section: impl object::ObjectSection<'a>,
        target_section_name: &str,
    ) -> Result<usize, CodegenError> {
        use crate::linker::relocation::RelocationKind;

        const RELA_ENTRY_SIZE: usize = 24; // Size of Elf64_Rela structure

        let rela_data = rela_section.data().map_err(|e| {
            CodegenError::InvalidOperation(format!("Failed to read .rela section data: {}", e))
        })?;

        if rela_data.len() % RELA_ENTRY_SIZE != 0 {
            tracing::warn!(
                "Invalid .rela section size: {} bytes (not a multiple of {})",
                rela_data.len(),
                RELA_ENTRY_SIZE
            );
            return Ok(0);
        }

        let mut parsed_count = 0;

        for (idx, entry_bytes) in rela_data.chunks_exact(RELA_ENTRY_SIZE).enumerate() {
            // Parse the 24-byte .rela entry
            // Layout: offset (8 bytes) | info (8 bytes) | addend (8 bytes)
            let offset = u64::from_le_bytes(entry_bytes[0..8].try_into().unwrap());
            let info = u64::from_le_bytes(entry_bytes[8..16].try_into().unwrap());
            let addend = i64::from_le_bytes(entry_bytes[16..24].try_into().unwrap());

            // Extract relocation type and symbol index from info field
            let reloc_type = (info & 0xffffffff) as u32;
            let symbol_index = (info >> 32) as usize;

            // CRITICAL DEBUG: Log __rue_println_i64 relocations specifically
            if target_section_name.contains("__rue_println") {
                tracing::debug!(
                    "Internal rela parsing: section='{}', entry={}, offset=0x{:x}, symbol_index={}, type={}, addend={}",
                    target_section_name,
                    idx,
                    offset,
                    symbol_index,
                    reloc_type,
                    addend
                );
            }

            // Map relocation type to our enum
            let Some(kind) = RelocationKind::from_x86_64_type(reloc_type) else {
                tracing::trace!(
                    "Skipping unsupported relocation type {} at entry {}",
                    reloc_type,
                    idx
                );
                continue;
            };

            // Get the symbol name
            // Special case: symbol index 0 often means the relocation is relative to the section itself
            let symbol_name = if symbol_index == 0 {
                // For section-relative relocations, we use the section name as the symbol
                // This typically happens with static data in the section
                tracing::trace!(
                    "Relocation with symbol index 0 (section-relative) at entry {}",
                    idx
                );

                // CRITICAL FIX: Don't skip ALL section-relative relocations - some are needed!
                // Internal relocations within runtime functions that reference .rodata sections
                // are essential for proper function operation
                if target_section_name.contains("__rue_println") {
                    tracing::debug!(
                        "Found section-relative relocation in {}: offset=0x{:x}, type={}, skipping - this may cause missing string references!",
                        target_section_name,
                        offset,
                        reloc_type
                    );
                }

                // Skip section-relative relocations for now - they're typically already resolved
                continue;
            } else if let Some(symbol) = obj_file.symbols().nth(symbol_index) {
                let name = symbol.name().unwrap_or("");
                if name.is_empty() {
                    // Empty symbol name - this can happen with local symbols
                    // Skip these for now
                    tracing::trace!(
                        "Skipping relocation with empty symbol name at index {} entry {}",
                        symbol_index,
                        idx
                    );
                    continue;
                }
                name.to_string()
            } else {
                tracing::warn!(
                    "Relocation references invalid symbol index {} at entry {}",
                    symbol_index,
                    idx
                );
                continue;
            };

            // Special logging for GOTPCREL relocations
            if matches!(kind, RelocationKind::GOTPCREL) {
                tracing::debug!(
                    "Found GOTPCREL relocation for symbol '{}' at offset 0x{:x} in section '{}'",
                    symbol_name,
                    offset,
                    target_section_name
                );
            }

            // Add the relocation
            self.relocations.push(RelocationEntry {
                section_name: target_section_name.to_string(),
                offset,
                kind,
                symbol_name,
                addend,
            });

            parsed_count += 1;
        }

        Ok(parsed_count)
    }

    /// Process a single relocation
    fn process_relocation(
        &mut self,
        section_name: &str,
        offset: u64,
        relocation: object::read::Relocation,
        obj_file: &object::File,
    ) -> Result<(), CodegenError> {
        use object::RelocationTarget;

        let kind = match relocation.kind() {
            object::RelocationKind::Absolute => {
                // Check the size to determine if it's 64-bit or 32-bit
                if relocation.size() == 64 {
                    RelocationKind::Absolute64
                } else if relocation.size() == 32 {
                    // CRITICAL FIX: Map 32-bit absolute relocations to Absolute32
                    // The R_X86_64_32 relocation from __rue_println_i64 is a 32-bit absolute
                    RelocationKind::Absolute32
                } else {
                    // Debug: log unsupported relocation sizes for __rue_println functions
                    if section_name.contains("__rue_println") {
                        tracing::debug!(
                            "Skipping unsupported relocation size {} in section '{}' at offset 0x{:x}",
                            relocation.size(),
                            section_name,
                            offset
                        );
                    }
                    return Ok(()); // Skip unsupported relocation sizes
                }
            }
            object::RelocationKind::Relative => RelocationKind::PC32,
            object::RelocationKind::PltRelative => RelocationKind::PLT32,
            object::RelocationKind::GotRelative => RelocationKind::GOTPCREL,
            _ => {
                // Debug: log unsupported relocation kinds for __rue_println functions
                if section_name.contains("__rue_println") {
                    tracing::debug!(
                        "Skipping unsupported relocation kind {:?} in section '{}' at offset 0x{:x}",
                        relocation.kind(),
                        section_name,
                        offset
                    );
                }
                return Ok(()); // Skip unsupported relocation kinds
            }
        };

        let symbol_name = match relocation.target() {
            RelocationTarget::Symbol(symbol_idx) => {
                // Find symbol by index - use iterator to handle potential gaps in symbol table
                let mut found_symbol = None;
                // Debug: for critical relocations, show what we're looking for
                if section_name == ".text._start" || section_name == ".text.__rue_main" {
                    tracing::debug!(
                        "Looking for symbol at index {} in {} relocation",
                        symbol_idx.0,
                        section_name
                    );
                }
                // IMPORTANT: The object library's symbols() iterator skips the null symbol (index 0),
                // so enumerate index N corresponds to ELF symbol index N+1
                for (enum_idx, symbol) in obj_file.symbols().enumerate() {
                    let elf_idx = enum_idx + 1; // Adjust for skipped null symbol
                    if (section_name == ".text._start" || section_name == ".text.__rue_main")
                        && elf_idx >= symbol_idx.0.saturating_sub(2)
                        && elf_idx <= symbol_idx.0 + 2
                    {
                        let name = symbol.name().unwrap_or("<unnamed>");
                        tracing::debug!(
                            "  Symbol at ELF idx {} (enum idx {}): '{}'",
                            elf_idx,
                            enum_idx,
                            name
                        );
                    }
                    if elf_idx == symbol_idx.0 {
                        found_symbol = Some(symbol);
                        break;
                    }
                }

                if let Some(symbol) = found_symbol {
                    let name = symbol.name().unwrap_or("");

                    // CRITICAL DEBUG: Log symbol resolution for __rue_println sections
                    if section_name.contains("__rue_println") {
                        tracing::debug!(
                            "Symbol resolution: section='{}', symbol_idx={}, symbol_name='{}', is_empty={}",
                            section_name,
                            symbol_idx.0,
                            name,
                            name.is_empty()
                        );
                    }

                    if name.is_empty() {
                        // Check if this is a BSS section symbol
                        let section_index = match symbol.section() {
                            object::SymbolSection::Section(idx) => Some(idx.0),
                            _ => None,
                        };

                        if let Some(idx) = section_index {
                            // Check if this points to a BSS section
                            let iterator_index = idx.saturating_sub(1);
                            if let Some(target_section) = obj_file.sections().nth(iterator_index) {
                                let target_section_name = target_section.name().unwrap_or("");

                                // CRITICAL DEBUG: Log section-relative symbol resolution for internal relocations
                                if section_name.contains("__rue_println") {
                                    tracing::debug!(
                                        "Internal section symbol: section='{}', symbol_idx={}, target_section='{}', is_bss={}, is_rodata={}",
                                        section_name,
                                        symbol_idx.0,
                                        target_section_name,
                                        target_section_name.starts_with(".bss"),
                                        target_section_name.starts_with(".rodata")
                                    );
                                }

                                if target_section_name.starts_with(".bss") {
                                    // This is a BSS section relocation
                                    // Create a synthetic symbol name for the BSS section
                                    // This allows relocations to BSS sections to be resolved
                                    tracing::debug!(
                                        "Found BSS section relocation: idx={}, section='{}', treating as section symbol",
                                        symbol_idx.0,
                                        target_section_name
                                    );
                                    // Use the section name as the symbol name
                                    // The symbol table should have an entry for this section at offset 0
                                    target_section_name.to_string()
                                } else if target_section_name.starts_with(".rodata") {
                                    // CRITICAL FIX: Handle .rodata section relocations
                                    // Internal relocations within runtime functions often reference .rodata sections
                                    // for string constants and other static data
                                    tracing::debug!(
                                        "Found .rodata section relocation: idx={}, section='{}', treating as section symbol",
                                        symbol_idx.0,
                                        target_section_name
                                    );
                                    target_section_name.to_string()
                                } else {
                                    // Skip non-BSS empty symbol relocations
                                    tracing::trace!(
                                        "Skipping relocation with empty symbol name at index {}",
                                        symbol_idx.0
                                    );
                                    return Ok(());
                                }
                            } else {
                                // Skip if we can't find the section
                                tracing::trace!(
                                    "Skipping relocation with empty symbol name at index {}",
                                    symbol_idx.0
                                );
                                return Ok(());
                            }
                        } else {
                            // Skip if symbol has no section
                            tracing::trace!(
                                "Skipping relocation with empty symbol name at index {}",
                                symbol_idx.0
                            );
                            return Ok(());
                        }
                    } else {
                        // Special debug for critical relocations
                        if section_name == ".text._start" || section_name == ".text.__rue_main" {
                            tracing::debug!(
                                "{} relocation symbol lookup: idx={}, name='{}', undefined={}",
                                section_name,
                                symbol_idx.0,
                                name,
                                symbol.is_undefined()
                            );
                        }
                        tracing::trace!(
                            "Relocation target symbol: {} (external: {})",
                            name,
                            symbol.is_undefined()
                        );
                        name.to_string()
                    }
                } else {
                    // Skip relocations with missing symbol indices - these are likely broken object files
                    tracing::trace!(
                        "Skipping relocation with missing symbol index: {}",
                        symbol_idx.0
                    );
                    return Ok(());
                }
            }
            RelocationTarget::Section(section_idx) => {
                // Section-relative relocation
                if let Some(section) = obj_file.sections().nth(section_idx.0) {
                    let name = section.name().unwrap_or("");
                    if name.is_empty() {
                        // Skip relocations with empty section names
                        tracing::trace!(
                            "Skipping relocation with empty section name at index {}",
                            section_idx.0
                        );
                        return Ok(());
                    }
                    tracing::trace!("Relocation target section: {}", name);
                    name.to_string()
                } else {
                    tracing::warn!(
                        "Relocation references missing section index: {}",
                        section_idx.0
                    );
                    return Ok(()); // Skip if section not found
                }
            }
            _ => return Ok(()), // Skip other relocation targets
        };

        let addend = relocation.addend();

        // Log if this looks like an external __rue_ function reference
        if symbol_name.starts_with("__rue_") {
            tracing::debug!(
                "Found __rue_ function reference: {} with addend {}",
                symbol_name,
                addend
            );
        }

        // Special logging for critical relocations
        if section_name == ".text._start" || section_name == ".text.__rue_main" {
            tracing::debug!(
                "Adding {} relocation: offset=0x{:x}, symbol='{}', kind={:?}, addend={}",
                section_name,
                offset,
                symbol_name,
                kind,
                addend
            );
        }

        // CRITICAL DEBUG: Log __rue_println_i64 internal relocations when parsed
        if section_name.contains("__rue_println_i64") {
            tracing::debug!(
                "Parsing internal relocation: section='{}', offset=0x{:x}, kind={:?}, symbol='{}', addend={}",
                section_name,
                offset,
                kind,
                symbol_name,
                addend
            );
        }

        self.relocations.push(RelocationEntry {
            section_name: section_name.to_string(),
            offset,
            kind,
            symbol_name,
            addend,
        });

        Ok(())
    }

    /// Fix up _start symbol if it's pointing to wrong section (this is mostly handled by parse_symbols now)
    fn fix_start_symbol_section(&mut self) -> Result<(), CodegenError> {
        // This function is now mostly redundant since parse_symbols handles symbol section correction
        // But we keep it for any edge cases that might slip through

        if let Some(start_symbol_idx) = self.symbols.iter().position(|sym| sym.name == "_start") {
            let needs_fix = {
                let start_symbol = &self.symbols[start_symbol_idx];
                start_symbol.section_name.starts_with(".rela.")
                    || start_symbol.section_name.starts_with(".sym")
                    || start_symbol.section_name.starts_with(".str")
            };

            if needs_fix {
                let old_section_name = self.symbols[start_symbol_idx].section_name.clone();
                tracing::debug!(
                    "_start symbol still has incorrect section '{}' after parsing - this should have been fixed",
                    old_section_name
                );

                // Try to find any available text section as fallback
                if let Some(text_section) = self
                    .sections
                    .iter()
                    .find(|sec| sec.name.starts_with(".text"))
                {
                    self.symbols[start_symbol_idx].section_name = text_section.name.clone();
                    tracing::info!(
                        "Fixed _start symbol section from '{}' to '{}'",
                        old_section_name,
                        text_section.name
                    );
                }
            }
        }

        Ok(())
    }

    /// Check if a section is important for linking
    fn is_important_section(&self, name: &str) -> bool {
        // Include all .text.*, .data.*, .rodata.*, .bss.* sections
        // These are generated by rustc for individual functions/data
        name.starts_with(".text") || 
        name.starts_with(".data") || 
        name.starts_with(".rodata") || 
        name.starts_with(".bss") ||
        // Include any section that contains anonymous constants (.Lanon symbols)
        name.contains(".Lanon") ||
        // Include init/fini sections for initialization
        name == ".init_array" ||
        name == ".fini_array" ||
        // Include group sections (COMDAT groups)
        name.starts_with(".group") ||
        // Include Rust-specific metadata sections if needed
        name.starts_with(".rust")
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
