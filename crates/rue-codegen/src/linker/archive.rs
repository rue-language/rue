// Archive (.a) file support for the linker
//
// This module provides functionality to parse and extract members from
// static library archives (.a files) for linking.

use crate::CodegenError;
use crate::linker::ObjectFile;
use crate::linker::asm_object::{
    AsmObject, AsmSection, AsmSymbol, RelocKind, SymBind, SymDef, SymVis,
};
use std::collections::{HashMap, HashSet};

/// Represents a static library archive (.a file)
pub struct Archive {
    /// Map from symbol names to archive member names that define them
    symbol_index: HashMap<String, String>,
    /// Archive data
    data: Vec<u8>,
    /// Members that have been extracted
    extracted_members: HashSet<String>,
    /// Map from member name to archive order (for deterministic ordering)
    member_order: HashMap<String, usize>,
}

impl Archive {
    /// Parse an archive from file path
    pub fn from_path(path: &str) -> Result<Self, CodegenError> {
        let data = std::fs::read(path).map_err(|_| CodegenError::Io)?;
        Self::from_bytes(path.to_string(), data)
    }

    /// Parse an archive from bytes
    pub fn from_bytes(_path: String, data: Vec<u8>) -> Result<Self, CodegenError> {
        let mut archive = Self {
            symbol_index: HashMap::new(),
            data,
            extracted_members: HashSet::new(),
            member_order: HashMap::new(),
        };

        archive.build_symbol_index()?;
        Ok(archive)
    }

    /// Build the symbol index by scanning all archive members
    fn build_symbol_index(&mut self) -> Result<(), CodegenError> {
        use goblin::archive::Archive as GoblinArchive;

        let archive = GoblinArchive::parse(&self.data).map_err(|e| {
            CodegenError::InvalidOperation(format!("Failed to parse archive: {}", e))
        })?;

        // TODO: Add check for thin archives when goblin supports it
        // Thin archives have members stored externally and are not supported

        // Track archive order for deterministic resolution
        let mut order = 0usize;

        // Iterate through members and extract symbol information
        for member_name in archive.members() {
            // Skip special members like symbol table
            if member_name.starts_with('/') || member_name.starts_with("__.SYMDEF") {
                continue;
            }

            // Record member order for deterministic extraction
            self.member_order
                .entry(member_name.to_string())
                .or_insert_with(|| {
                    let idx = order;
                    order += 1;
                    idx
                });

            // Extract the member data
            if let Ok(member_data) = archive.extract(member_name, &self.data) {
                // Try to parse as ELF object
                if let Ok(obj) = goblin::Object::parse(member_data) {
                    if let goblin::Object::Elf(elf) = obj {
                        // Add global symbols to index
                        for sym in &elf.syms {
                            if let Some(name) = elf.strtab.get_at(sym.st_name) {
                                // Only index global symbols
                                let binding = sym.st_bind();
                                // Check visibility - only index visible symbols
                                let visibility = sym.st_other & 0x3;
                                let is_visible = visibility == goblin::elf::sym::STV_DEFAULT
                                    || visibility == goblin::elf::sym::STV_PROTECTED;

                                if is_visible
                                    && (binding == goblin::elf::sym::STB_GLOBAL
                                        || binding == goblin::elf::sym::STB_WEAK)
                                {
                                    // Check if symbol is defined (has a section)
                                    if sym.st_shndx
                                        != goblin::elf::section_header::SHN_UNDEF as usize
                                    {
                                        // Keep first definition only (deterministic symbol resolution)
                                        self.symbol_index
                                            .entry(name.to_string())
                                            .or_insert_with(|| member_name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Get the list of members that define given undefined symbols
    pub fn find_members_for_symbols(&self, undefined: &[String]) -> Vec<String> {
        let mut members = Vec::new();
        let mut seen = HashSet::new();

        for symbol in undefined {
            if let Some(member) = self.symbol_index.get(symbol) {
                if !self.extracted_members.contains(member) && !seen.contains(member) {
                    seen.insert(member.clone());
                    members.push(member.clone());
                }
            }
        }

        // Sort by archive order for deterministic extraction
        members.sort_by_key(|m| self.member_order.get(m).copied().unwrap_or(usize::MAX));
        members
    }

    /// Extract a member from the archive as an ObjectFile
    pub fn extract_member(&mut self, member_name: &str) -> Result<ObjectFile, CodegenError> {
        use goblin::archive::Archive as GoblinArchive;

        let archive = GoblinArchive::parse(&self.data).map_err(|e| {
            CodegenError::InvalidOperation(format!("Failed to parse archive: {}", e))
        })?;

        let member_data = archive.extract(member_name, &self.data).map_err(|e| {
            CodegenError::InvalidOperation(format!(
                "Failed to extract member {}: {}",
                member_name, e
            ))
        })?;

        self.extracted_members.insert(member_name.to_string());

        // Parse as ObjectFile
        ObjectFile::parse(member_name.to_string(), member_data)
    }

    /// Extract a member as an AsmObject (for new linking pipeline)
    pub fn extract_member_as_asm(&mut self, member_name: &str) -> Result<AsmObject, CodegenError> {
        use goblin::archive::Archive as GoblinArchive;

        let archive = GoblinArchive::parse(&self.data).map_err(|e| {
            CodegenError::InvalidOperation(format!("Failed to parse archive: {}", e))
        })?;

        let member_data = archive.extract(member_name, &self.data).map_err(|e| {
            CodegenError::InvalidOperation(format!(
                "Failed to extract member {}: {}",
                member_name, e
            ))
        })?;

        self.extracted_members.insert(member_name.to_string());

        // Convert to AsmObject
        self.parse_elf_to_asm(member_data)
    }

    /// Parse ELF data into an AsmObject
    fn parse_elf_to_asm(&self, data: &[u8]) -> Result<AsmObject, CodegenError> {
        use goblin::elf::reloc::RelocSection;
        use goblin::elf::{Elf, section_header::SHT_NOBITS, section_header::SHT_PROGBITS};

        let elf = Elf::parse(data)
            .map_err(|e| CodegenError::InvalidOperation(format!("Failed to parse ELF: {}", e)))?;

        let mut obj = AsmObject::new();

        // Process sections
        for section in &elf.section_headers {
            let name = elf
                .shdr_strtab
                .get_at(section.sh_name)
                .unwrap_or("")
                .to_string();

            // Skip non-allocatable sections and sections we don't need
            if section.sh_flags & (goblin::elf::section_header::SHF_ALLOC as u64) == 0 {
                continue;
            }

            // Skip exception handling and debug sections
            if name.starts_with(".eh_frame")
                || name.starts_with(".debug")
                || name.starts_with(".note")
                || name.starts_with(".comment")
                || name.starts_with(".group")
            {
                continue;
            }

            let alignment = if section.sh_addralign > 0 {
                section.sh_addralign
            } else {
                1
            };

            let executable =
                section.sh_flags & (goblin::elf::section_header::SHF_EXECINSTR as u64) != 0;
            let writable = section.sh_flags & (goblin::elf::section_header::SHF_WRITE as u64) != 0;

            let (data, is_nobits, nobits_size) = if section.sh_type == SHT_NOBITS {
                // BSS section - no data, just size
                (Vec::new(), true, section.sh_size)
            } else if section.sh_type == SHT_PROGBITS {
                // Regular section with data
                let start = section.sh_offset as usize;
                let end = start + section.sh_size as usize;
                if end > data.len() {
                    return Err(CodegenError::InvalidOperation(format!(
                        "ELF section {} out of file bounds (off={} size={} len={})",
                        name,
                        start,
                        section.sh_size,
                        data.len()
                    )));
                }
                (data[start..end].to_vec(), false, 0)
            } else {
                (Vec::new(), false, 0)
            };

            obj.add_section(AsmSection {
                name: name.clone(),
                data,
                alignment,
                executable,
                writable,
                is_nobits,
                nobits_size,
            });

            // Also add a section symbol for this section
            // This is needed for relocations with symbol index 0
            obj.add_symbol(AsmSymbol {
                name: format!(".SECTION.{}", name),
                bind: SymBind::Local,
                vis: SymVis::Default,
                def: SymDef::Defined {
                    section_name: name,
                    offset: 0,
                },
                size: section.sh_size,
                sym_type: goblin::elf::sym::STT_SECTION,
            });
        }

        // Process symbols - including locals and section symbols
        for (sym_idx, sym) in elf.syms.iter().enumerate() {
            // Skip the null symbol at index 0 - it's always undefined
            // We'll handle it specially in relocations if needed
            if sym_idx == 0 {
                // Don't add the NULL symbol - it's not needed
                continue;
            }

            let name = elf.strtab.get_at(sym.st_name).unwrap_or("").to_string();

            // Handle section symbols - they have empty names but are important
            let sym_type = sym.st_type();
            let is_section_symbol = sym_type == goblin::elf::sym::STT_SECTION;

            // For section symbols, use a special name format
            let final_name = if is_section_symbol && name.is_empty() {
                if let Some(section) = elf.section_headers.get(sym.st_shndx as usize) {
                    let section_name = elf
                        .shdr_strtab
                        .get_at(section.sh_name)
                        .unwrap_or("")
                        .to_string();
                    format!(".SECTION.{}", section_name)
                } else {
                    format!(".SECTION.{}", sym_idx)
                }
            } else if name.is_empty() {
                // Other symbols with empty names - give them a unique identifier
                format!(".LOCAL.{}", sym_idx)
            } else {
                name
            };

            let bind = match sym.st_bind() {
                goblin::elf::sym::STB_LOCAL => SymBind::Local,
                goblin::elf::sym::STB_GLOBAL => SymBind::Global,
                goblin::elf::sym::STB_WEAK => SymBind::Weak,
                _ => SymBind::Local, // Default to local for unknown bindings
            };

            let def = if sym.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize {
                SymDef::Undefined
            } else if sym.st_shndx == goblin::elf::section_header::SHN_ABS as usize {
                // Absolute symbol
                SymDef::Absolute {
                    value: sym.st_value,
                }
            } else if sym.st_shndx == goblin::elf::section_header::SHN_COMMON as usize {
                // Common symbol - needs allocation in BSS
                SymDef::Common {
                    size: sym.st_size,
                    alignment: sym.st_value, // For COMMON, st_value is alignment
                }
            } else if let Some(section) = elf.section_headers.get(sym.st_shndx as usize) {
                let section_name = elf
                    .shdr_strtab
                    .get_at(section.sh_name)
                    .unwrap_or("")
                    .to_string();
                SymDef::Defined {
                    section_name,
                    offset: sym.st_value,
                }
            } else {
                continue;
            };

            obj.add_symbol_with_index(
                sym_idx,
                AsmSymbol {
                    name: final_name,
                    bind,
                    vis: SymVis::Default,
                    def,
                    size: sym.st_size,
                    sym_type,
                },
            );
        }

        // Process relocations
        for section in &elf.section_headers {
            if section.sh_type == goblin::elf::section_header::SHT_RELA {
                // Find the section this relocation applies to
                let target_section_idx = section.sh_info as usize;
                if let Some(target_section) = elf.section_headers.get(target_section_idx) {
                    let section_name = elf
                        .shdr_strtab
                        .get_at(target_section.sh_name)
                        .unwrap_or("")
                        .to_string();

                    // Parse relocations
                    let rela_start = section.sh_offset as usize;
                    let rela_size = section.sh_size as usize;
                    let rela_data = &data[rela_start..rela_start + rela_size];
                    let is_rela = true;
                    let ctx = goblin::container::Ctx::new(
                        goblin::container::Container::Big,
                        goblin::container::Endian::Little,
                    );

                    if let Ok(relas) = RelocSection::parse(rela_data, 0, rela_size, is_rela, ctx) {
                        // Process relocations directly
                        for rela in relas.iter() {
                            let sym_idx = rela.r_sym;

                            // Special case for symbol index 0 - this is the NULL symbol
                            // Relocations with symbol 0 are absolute/section-relative
                            let symbol_name = if sym_idx == 0 {
                                // For symbol 0, the relocation is relative to the section itself
                                // We need to make sure we have a section symbol defined for this
                                format!(".SECTION.{}", section_name)
                            } else if let Some(&our_idx) = obj.symbol_index_map.get(&sym_idx) {
                                obj.symbols[our_idx].name.clone()
                            } else {
                                // Symbol not found - this shouldn't happen
                                eprintln!(
                                    "Warning: Relocation references unknown symbol index {}",
                                    sym_idx
                                );
                                continue;
                            };

                            let kind = match rela.r_type {
                                goblin::elf::reloc::R_X86_64_64 => RelocKind::Abs64,
                                goblin::elf::reloc::R_X86_64_PC32 => RelocKind::Pc32,
                                goblin::elf::reloc::R_X86_64_PLT32 => RelocKind::Plt32,
                                goblin::elf::reloc::R_X86_64_GOTPCREL => RelocKind::GotPcRel,
                                _ => continue,
                            };

                            obj.add_relocation(crate::linker::asm_object::AsmReloc {
                                section_name: section_name.clone(),
                                offset: rela.r_offset,
                                kind,
                                symbol_name,
                                addend: rela.r_addend.unwrap_or(0),
                            });
                        }
                    }
                }
            }
        }

        Ok(obj)
    }

    /// Perform iterative member extraction based on undefined symbols
    pub fn extract_needed_members(
        &mut self,
        undefined: &[String],
    ) -> Result<Vec<AsmObject>, CodegenError> {
        use std::collections::VecDeque;

        let mut extracted = Vec::new();
        let mut work: VecDeque<String> = undefined.iter().cloned().collect();
        let mut seen = HashSet::new();

        while let Some(sym) = work.pop_front() {
            // Skip if we've already processed this symbol
            if !seen.insert(sym.clone()) {
                continue;
            }

            // Find the member that defines this symbol
            if let Some(member) = self.symbol_index.get(&sym).cloned() {
                // Skip if already extracted
                if self.extracted_members.contains(&member) {
                    continue;
                }

                // Extract the member
                let obj = self.extract_member_as_asm(&member)?;

                // Queue its undefined symbols for processing
                for undef in obj.undefined_symbols() {
                    if !seen.contains(&undef.name) {
                        work.push_back(undef.name.clone());
                    }
                }

                extracted.push(obj);
            }
        }

        Ok(extracted)
    }
}
