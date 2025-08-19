// A basic linker for linking the runtime
//
// This module implements proper ELF linking with section merging,
// symbol resolution, and relocation application.

use crate::CodegenError;
use crate::linker::archive::Archive;
use crate::linker::asm_object::{AsmObject, AsmReloc, AsmSymbol, RelocKind, SymBind, SymDef};
use std::collections::{HashMap, HashSet};

/// Represents a chunk of a section from an input object
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Index of the object this chunk came from
    object_id: usize,
    /// Name of the section in the input object
    input_section_name: String,
    /// Offset in the merged section where this chunk starts
    merged_offset: u64,
    /// Size of this chunk
    size: u64,
}

/// Represents a merged section
#[derive(Debug, Clone)]
pub struct MergedSection {
    pub name: String,
    pub data: Vec<u8>,
    pub alignment: u64,
    pub base_address: u64,
    pub chunks: Vec<Chunk>,
    pub executable: bool,
    pub writable: bool,
    /// File offset where this section's data begins in the ELF file
    pub file_offset: u64,
    /// Size of NOBITS data (for BSS sections) not included in file_size
    pub nobits_size: u64,
}

/// Symbol resolution entry
#[derive(Debug, Clone)]
struct ResolvedSymbol {
    /// The symbol definition
    symbol: AsmSymbol,
    /// Which object provides this symbol
    object_id: usize,
    /// Resolved address after layout
    address: u64,
}

/// Result of the linking process
#[derive(Debug, Clone)]
pub struct LinkedExecutable {
    pub text_section: Vec<u8>,
    pub rodata_section: Vec<u8>,
    pub got_section: Vec<u8>,
    pub data_section: Vec<u8>,
    pub bss_size: u64,
    pub entry_point: u64,
    pub merged_sections: HashMap<String, MergedSection>,
}

/// Helper function to canonicalize section names (e.g., ".text.foo" -> ".text")
fn canonicalize_section(name: &str) -> &str {
    if name.starts_with(".text.") {
        ".text"
    } else if name.starts_with(".rodata.") {
        ".rodata"
    } else if name.starts_with(".data.") {
        ".data"
    } else if name.starts_with(".bss.") {
        ".bss"
    } else {
        name
    }
}

impl LinkedExecutable {
    /// Convert to ELF executable bytes
    pub fn to_elf(&self) -> Vec<u8> {
        let mut elf = Vec::new();

        // Constants
        let base_addr = 0x400000u64;
        let page_size = 0x1000u64;

        // We'll have 2 program headers
        let num_phdrs = 2u16;
        let phdr_size = 56u16;
        let ehdr_size = 64u16;
        let headers_size = ehdr_size as u64 + (phdr_size as u64 * num_phdrs as u64); // 176 bytes

        // Get section information from stored layout
        let text_section = self.merged_sections.get(".text");
        let rodata_section = self.merged_sections.get(".rodata");
        let got_section = self.merged_sections.get(".got");
        let data_section = self.merged_sections.get(".data");
        let _bss_section = self.merged_sections.get(".bss");

        // Calculate file offsets and sizes from stored section layout
        let text_offset = text_section.map(|s| s.file_offset).unwrap_or(headers_size);
        let text_size = self.text_section.len() as u64;

        let rodata_offset = rodata_section
            .map(|s| s.file_offset)
            .unwrap_or(text_offset + text_size);
        let rodata_size = self.rodata_section.len() as u64;

        let got_offset = got_section
            .map(|s| s.file_offset)
            .unwrap_or(rodata_offset + rodata_size);
        let got_size = self.got_section.len() as u64;

        let data_offset = data_section
            .map(|s| s.file_offset)
            .unwrap_or(got_offset + got_size);
        let _data_size = self.data_section.len() as u64;

        // Text segment covers headers..end_of_rodata
        let text_vaddr = base_addr;
        let text_end = rodata_section
            .map(|s| s.file_offset + s.data.len() as u64)
            .unwrap_or_else(|| {
                text_section
                    .map(|s| s.file_offset + s.data.len() as u64)
                    .unwrap_or(headers_size)
            });
        let text_file_size = text_end; // from file start (0) since we write headers at 0
        let text_mem_size = text_file_size;

        // Data segment starts at min(file_offset of .data, .got) if present
        let data_start_off = [
            data_section.as_ref().map(|s| s.file_offset),
            got_section.as_ref().map(|s| s.file_offset),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(text_end);
        let data_end_off = [
            data_section
                .as_ref()
                .map(|s| s.file_offset + s.data.len() as u64),
            got_section
                .as_ref()
                .map(|s| s.file_offset + s.data.len() as u64),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(data_start_off);

        let data_vaddr = base_addr + data_start_off;
        let data_file_size = data_end_off - data_start_off;
        // Include BSS in mem size:
        let data_mem_size = data_file_size + self.bss_size;

        // Use the actual entry point from the linked executable
        // This was already resolved to the correct virtual address in link()
        let entry_point = self.entry_point;

        // ELF header (64 bytes)
        elf.extend_from_slice(&[
            0x7f, b'E', b'L', b'F', // Magic
            2, 1, 1, 0, // 64-bit, little-endian, current version, System V ABI
            0, 0, 0, 0, 0, 0, 0, 0, // padding
            2, 0, 0x3e, 0, // ET_EXEC, x86-64
            1, 0, 0, 0, // version
        ]);

        // Entry point (8 bytes)
        elf.extend_from_slice(&entry_point.to_le_bytes());

        // Program header offset (8 bytes) - immediately after ELF header
        elf.extend_from_slice(&(ehdr_size as u64).to_le_bytes());

        // Section header offset (8 bytes) - we don't use sections
        elf.extend_from_slice(&0u64.to_le_bytes());

        // Flags (4 bytes)
        elf.extend_from_slice(&0u32.to_le_bytes());

        // Header sizes and counts
        elf.extend_from_slice(&ehdr_size.to_le_bytes()); // ELF header size
        elf.extend_from_slice(&phdr_size.to_le_bytes()); // Program header size
        elf.extend_from_slice(&num_phdrs.to_le_bytes()); // Number of program headers
        elf.extend_from_slice(&0u16.to_le_bytes()); // Section header size
        elf.extend_from_slice(&0u16.to_le_bytes()); // Number of sections
        elf.extend_from_slice(&0u16.to_le_bytes()); // Section name string table index

        // Program header 1: Text segment (R+X)
        elf.extend_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        elf.extend_from_slice(&5u32.to_le_bytes()); // PF_R | PF_X
        elf.extend_from_slice(&0u64.to_le_bytes()); // Offset in file
        elf.extend_from_slice(&text_vaddr.to_le_bytes()); // Virtual address
        elf.extend_from_slice(&text_vaddr.to_le_bytes()); // Physical address
        elf.extend_from_slice(&text_file_size.to_le_bytes()); // Size in file
        elf.extend_from_slice(&text_mem_size.to_le_bytes()); // Size in memory
        elf.extend_from_slice(&page_size.to_le_bytes()); // Alignment

        // Program header 2: Data segment (R+W)
        elf.extend_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        elf.extend_from_slice(&6u32.to_le_bytes()); // PF_R | PF_W
        elf.extend_from_slice(&data_start_off.to_le_bytes()); // Offset in file
        elf.extend_from_slice(&data_vaddr.to_le_bytes()); // Virtual address
        elf.extend_from_slice(&data_vaddr.to_le_bytes()); // Physical address
        elf.extend_from_slice(&data_file_size.to_le_bytes()); // Size in file
        elf.extend_from_slice(&data_mem_size.to_le_bytes()); // Size in memory
        elf.extend_from_slice(&page_size.to_le_bytes()); // Alignment

        // Now write the actual sections at their file offsets
        // Ensure we're at the right offset for .text
        while elf.len() < text_offset as usize {
            elf.push(0);
        }
        elf.extend_from_slice(&self.text_section);

        // Pad to .rodata offset
        while elf.len() < rodata_offset as usize {
            elf.push(0);
        }
        elf.extend_from_slice(&self.rodata_section);

        // Pad to .got offset and write GOT section if it exists
        if !self.got_section.is_empty() {
            while elf.len() < got_offset as usize {
                elf.push(0);
            }
            elf.extend_from_slice(&self.got_section);
        }

        // Pad to .data offset
        while elf.len() < data_offset as usize {
            elf.push(0);
        }
        elf.extend_from_slice(&self.data_section);

        elf
    }
}

/// Comprehensive linker
pub struct Linker {
    /// Input objects (user code + pulled archive members)
    objects: Vec<AsmObject>,
    /// Archives to pull members from
    archives: Vec<Archive>,
    /// Merged sections
    merged_sections: HashMap<String, MergedSection>,
    /// Symbol resolution map
    resolved_symbols: HashMap<String, ResolvedSymbol>,
    /// Current object being added
    next_object_id: usize,
    /// GOT entries: maps symbol name to GOT offset
    got_entries: HashMap<String, u64>,
    /// GOT section data (8-byte entries containing symbol addresses)
    got_section: Vec<u8>,
}

impl Linker {
    /// Create a new linker
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            archives: Vec::new(),
            merged_sections: HashMap::new(),
            resolved_symbols: HashMap::new(),
            next_object_id: 0,
            got_entries: HashMap::new(),
            got_section: Vec::new(),
        }
    }

    /// Add an object to link
    pub fn add_object(&mut self, object: AsmObject) {
        self.objects.push(object);
        self.next_object_id += 1;
    }

    /// Add an archive for member extraction
    pub fn add_archive(&mut self, archive: Archive) {
        self.archives.push(archive);
    }

    /// Perform the complete linking process
    pub fn link(&mut self) -> Result<LinkedExecutable, CodegenError> {
        // Phase 1: Pull needed archive members
        self.pull_archive_members()?;

        // Phase 2: Merge sections with chunk tracking
        self.merge_sections()?;

        // Phase 3: Create GOT section if needed (must be before layout)
        self.create_got_section()?;

        // Phase 4: Assign section layout
        self.assign_section_layout()?;

        // Phase 5: Recompute defined symbol addresses
        self.recompute_symbol_addresses()?;

        // Phase 6: Resolve all symbols
        self.resolve_symbols()?;

        // Phase 7: Apply relocations
        self.apply_relocations()?;

        // Phase 8: Build final executable
        self.build_executable()
    }

    /// Pull archive members to satisfy undefined symbols
    fn pull_archive_members(&mut self) -> Result<(), CodegenError> {
        // First iteration: ensure _start is pulled from the archive
        // This is needed because _start is the entry point and not referenced by user code
        let mut initial_undefined = HashSet::new();
        initial_undefined.insert("_start".to_string());

        // Pull any archive members that provide _start
        let mut new_objects = Vec::new();
        for archive in &mut self.archives {
            let extracted = archive.extract_needed_members(&vec!["_start".to_string()])?;
            new_objects.extend(extracted);
        }
        for obj in new_objects {
            self.add_object(obj);
        }

        // Now continue with the normal iterative pulling process
        loop {
            // Collect current undefined symbols
            let mut undefined = HashSet::new();
            for obj in &self.objects {
                for symbol in obj.undefined_symbols() {
                    undefined.insert(symbol.name.clone());
                }
            }

            // Remove already resolved symbols
            for name in self.resolved_symbols.keys() {
                undefined.remove(name);
            }

            if undefined.is_empty() {
                break;
            }

            // Try to find members in archives
            let undefined_vec: Vec<String> = undefined.into_iter().collect();
            let mut made_progress = false;
            let mut new_objects_to_add = Vec::new();

            for archive in &mut self.archives {
                let new_objects = archive.extract_needed_members(&undefined_vec)?;
                new_objects_to_add.extend(new_objects);
            }

            for obj in new_objects_to_add {
                self.add_object(obj);
                made_progress = true;
            }

            if !made_progress {
                break;
            }
        }

        Ok(())
    }

    /// Merge sections from all objects with chunk tracking
    fn merge_sections(&mut self) -> Result<(), CodegenError> {
        for (object_id, object) in self.objects.iter().enumerate() {
            for section in &object.sections {
                let merged_name = canonicalize_section(&section.name).to_string();

                let merged = self
                    .merged_sections
                    .entry(merged_name.clone())
                    .or_insert_with(|| MergedSection {
                        name: merged_name.clone(),
                        data: Vec::new(),
                        alignment: section.alignment,
                        base_address: 0,
                        chunks: Vec::new(),
                        executable: section.executable,
                        writable: section.writable,
                        file_offset: 0,
                        nobits_size: 0,
                    });

                merged.alignment = merged.alignment.max(section.alignment);

                // Align existing PROGBITS data for this new chunk
                let merged_size_bytes = merged.data.len() as u64;
                let mut merged_bss_bytes = merged.nobits_size;

                if section.is_nobits {
                    // Align logical BSS size
                    merged_bss_bytes = align_to(merged_bss_bytes, section.alignment);
                    let chunk = Chunk {
                        object_id,
                        input_section_name: section.name.clone(),
                        merged_offset: merged_bss_bytes, // offset in logical NOBITS region
                        size: section.nobits_size,
                    };
                    merged.chunks.push(chunk);
                    merged_bss_bytes += section.nobits_size;
                } else {
                    // Align file-backed data region
                    let aligned_off = align_to(merged_size_bytes, section.alignment);
                    let pad = (aligned_off - merged_size_bytes) as usize;
                    merged.data.extend(std::iter::repeat(0u8).take(pad));

                    let chunk = Chunk {
                        object_id,
                        input_section_name: section.name.clone(),
                        merged_offset: aligned_off, // offset within file-backed region
                        size: section.data.len() as u64,
                    };
                    merged.chunks.push(chunk);

                    merged.data.extend(&section.data);
                }

                merged.nobits_size = merged_bss_bytes;
            }
        }

        Ok(())
    }

    /// Create GOT section by scanning for GOTPCREL relocations
    fn create_got_section(&mut self) -> Result<(), CodegenError> {
        // First pass: scan all relocations to find GOTPCREL references
        let mut gotpcrel_symbols = HashSet::new();
        for object in &self.objects {
            for reloc in &object.relocs {
                if reloc.kind == RelocKind::GotPcRel {
                    gotpcrel_symbols.insert(reloc.symbol_name.clone());
                }
            }
        }

        // If no GOTPCREL relocations, no need for GOT
        if gotpcrel_symbols.is_empty() {
            return Ok(());
        }

        // Reserve 8 bytes at the beginning of GOT (ABI requirement)
        // Many x86-64 ABIs expect the first GOT slot to be reserved
        self.got_section.extend_from_slice(&[0u8; 8]);

        // Sort symbols for deterministic GOT ordering
        let mut syms: Vec<String> = gotpcrel_symbols.into_iter().collect();
        syms.sort_unstable();

        // Create GOT entries for each symbol
        for name in syms {
            let off = self.got_section.len() as u64;
            self.got_entries.insert(name, off);
            // Reserve 8 bytes for the symbol address (will be filled later)
            self.got_section.extend_from_slice(&[0u8; 8]);
        }

        // Create the .got merged section
        if !self.got_section.is_empty() {
            let got_section = MergedSection {
                name: ".got".to_string(),
                data: self.got_section.clone(),
                alignment: 8,       // 8-byte alignment for 64-bit pointers
                base_address: 0,    // Will be set in assign_section_layout
                chunks: Vec::new(), // GOT is generated, not from input objects
                executable: false,
                writable: true, // GOT needs to be writable for relocations
                file_offset: 0, // Will be set in assign_section_layout
                nobits_size: 0,
            };
            self.merged_sections.insert(".got".to_string(), got_section);
        }

        Ok(())
    }

    fn assign_section_layout(&mut self) -> Result<(), CodegenError> {
        // Standard section order: .text, .rodata, .got, .data, .bss
        let section_order = [".text", ".rodata", ".got", ".data", ".bss"];

        // Constants
        let base_addr = 0x400000u64;
        let page_size = 0x1000u64;
        let num_phdrs = 2u16;
        let phdr_size = 56u16;
        let ehdr_size = 64u16;
        let headers_size = ehdr_size as u64 + (phdr_size as u64 * num_phdrs as u64); // 176 bytes

        // Start addresses and file offsets
        let mut current_address = base_addr + headers_size;
        let mut current_file_offset = headers_size;

        for section_name in &section_order {
            if let Some(section) = self.merged_sections.get_mut(*section_name) {
                // Validate section name matches key
                if section.name != *section_name {
                    return Err(CodegenError::InvalidOperation(format!(
                        "Section name mismatch: expected {}, got {}",
                        section_name, section.name
                    )));
                }

                // Validate section permissions match expected layout
                match *section_name {
                    ".text" => {
                        if !section.executable {
                            return Err(CodegenError::InvalidOperation(
                                ".text section must be executable".to_string(),
                            ));
                        }
                    }
                    ".data" | ".bss" => {
                        if !section.writable {
                            return Err(CodegenError::InvalidOperation(format!(
                                "{} section must be writable",
                                section_name
                            )));
                        }
                    }
                    ".rodata" => {
                        if section.writable || section.executable {
                            return Err(CodegenError::InvalidOperation(
                                ".rodata section must be read-only".to_string(),
                            ));
                        }
                    }
                    _ => {}
                }

                // Special handling for .data - add page gap before it (for separate RW segment)
                if *section_name == ".data" {
                    current_address = align_to(current_address, page_size);
                    current_file_offset = align_to(current_file_offset, page_size);
                }

                // Align to section alignment for both address and file offset
                current_address = align_to(current_address, section.alignment);
                current_file_offset = align_to(current_file_offset, section.alignment);

                section.base_address = current_address;
                section.file_offset = current_file_offset;

                // Update addresses and offsets
                let section_size = section.data.len() as u64;
                let total_size = section_size + section.nobits_size;

                current_address += total_size;
                // File offset only advances for sections with actual data (.bss doesn't consume file space)
                if section.data.len() > 0 {
                    current_file_offset += section_size;
                }
            }
        }

        Ok(())
    }

    /// Recompute addresses for all defined symbols
    fn recompute_symbol_addresses(&mut self) -> Result<(), CodegenError> {
        for (object_id, object) in self.objects.iter().enumerate() {
            for symbol in &object.symbols {
                match &symbol.def {
                    SymDef::Defined {
                        section_name,
                        offset,
                    } => {
                        let merged_section_name = canonicalize_section(section_name);

                        // Find the merged section
                        if let Some(merged) = self.merged_sections.get(merged_section_name) {
                            // Find the chunk for this object
                            if let Some(chunk) = merged.chunks.iter().find(|c| {
                                c.object_id == object_id && c.input_section_name == *section_name
                            }) {
                                let is_section_symbol =
                                    symbol.sym_type == goblin::elf::sym::STT_SECTION;

                                if !is_section_symbol && *offset >= chunk.size {
                                    return Err(CodegenError::InvalidOperation(format!(
                                        "Symbol {} offset {} exceeds chunk size {}",
                                        symbol.name, offset, chunk.size
                                    )));
                                }

                                let addr = merged.base_address + chunk.merged_offset + offset;

                                // Only export non-local symbols to the global map
                                match symbol.bind {
                                    SymBind::Global | SymBind::Weak => {
                                        // Handle weak/strong precedence
                                        if let Some(existing) =
                                            self.resolved_symbols.get(&symbol.name)
                                        {
                                            match (existing.symbol.bind, symbol.bind) {
                                                (SymBind::Global, SymBind::Global) => {
                                                    return Err(CodegenError::InvalidOperation(
                                                        format!(
                                                            "Duplicate strong symbol '{}' in objects {} and {}",
                                                            symbol.name,
                                                            existing.object_id,
                                                            object_id
                                                        ),
                                                    ));
                                                }
                                                (SymBind::Global, SymBind::Weak) => { /* keep existing */
                                                }
                                                (SymBind::Weak, SymBind::Global) => {
                                                    self.resolved_symbols.insert(
                                                        symbol.name.clone(),
                                                        ResolvedSymbol {
                                                            symbol: symbol.clone(),
                                                            object_id,
                                                            address: addr,
                                                        },
                                                    );
                                                }
                                                (SymBind::Weak, SymBind::Weak) => { /* keep first */
                                                }
                                                _ => {}
                                            }
                                        } else {
                                            self.resolved_symbols.insert(
                                                symbol.name.clone(),
                                                ResolvedSymbol {
                                                    symbol: symbol.clone(),
                                                    object_id,
                                                    address: addr,
                                                },
                                            );
                                        }
                                    }
                                    SymBind::Local => {
                                        // Locals are not inserted; they're resolved per-object in relocation
                                    }
                                }
                            }
                        }
                    }
                    SymDef::Absolute { value } => {
                        // Absolute symbols have fixed addresses
                        let resolved = ResolvedSymbol {
                            symbol: symbol.clone(),
                            object_id,
                            address: *value,
                        };
                        self.resolved_symbols.insert(symbol.name.clone(), resolved);
                    }
                    SymDef::Common {
                        size: _,
                        alignment: _,
                    } => {
                        // Allocate COMMON symbols in BSS
                        // For now, just skip - we'll handle this when we need it
                        // TODO: Allocate space in .bss for COMMON symbols
                    }
                    SymDef::Undefined => {
                        // Skip undefined symbols - they'll be resolved from other objects
                    }
                }
            }
        }

        // Populate GOT entries with resolved symbol addresses
        self.populate_got_entries()?;

        Ok(())
    }

    /// Populate GOT entries with resolved symbol addresses
    fn populate_got_entries(&mut self) -> Result<(), CodegenError> {
        // Update the GOT section data with actual symbol addresses
        for (symbol_name, got_offset) in &self.got_entries {
            if let Some(resolved) = self.resolved_symbols.get(symbol_name) {
                let symbol_addr = resolved.address;
                let offset = *got_offset as usize;

                // Write the 64-bit symbol address into the GOT entry
                if offset + 8 <= self.got_section.len() {
                    self.got_section[offset..offset + 8]
                        .copy_from_slice(&symbol_addr.to_le_bytes());
                } else {
                    return Err(CodegenError::InvalidOperation(format!(
                        "GOT entry offset {} out of bounds for symbol {}",
                        offset, symbol_name
                    )));
                }
            } else {
                return Err(CodegenError::InvalidOperation(format!(
                    "Symbol {} in GOT not resolved",
                    symbol_name
                )));
            }
        }

        // Update the merged GOT section with the populated data
        if let Some(got_section) = self.merged_sections.get_mut(".got") {
            got_section.data = self.got_section.clone();
        }

        Ok(())
    }

    /// Resolve all symbols and check for undefined
    fn resolve_symbols(&mut self) -> Result<(), CodegenError> {
        let mut undefined = Vec::new();

        for object in &self.objects {
            for symbol in object.undefined_symbols() {
                if !self.resolved_symbols.contains_key(&symbol.name) {
                    undefined.push(symbol.name.clone());
                }
            }
        }

        if !undefined.is_empty() {
            return Err(CodegenError::InvalidOperation(format!(
                "Undefined symbols: {}",
                undefined.join(", ")
            )));
        }

        Ok(())
    }

    /// Apply all relocations
    fn apply_relocations(&mut self) -> Result<(), CodegenError> {
        // Collect all relocations with their object IDs first
        let mut all_relocs = Vec::new();
        for (object_id, object) in self.objects.iter().enumerate() {
            for reloc in &object.relocs {
                all_relocs.push((object_id, reloc.clone()));
            }
        }

        // Now apply them
        for (object_id, reloc) in all_relocs {
            self.apply_single_relocation(object_id, &reloc)?;
        }

        Ok(())
    }

    /// Apply a single relocation
    fn apply_single_relocation(
        &mut self,
        object_id: usize,
        reloc: &AsmReloc,
    ) -> Result<(), CodegenError> {
        // Skip debug and exception handling sections - we don't include them in final executable
        if reloc.section_name.starts_with(".debug") || reloc.section_name.starts_with(".eh_frame") {
            return Ok(());
        }

        // Process ALL relocations including locals, sections, and compiler-generated symbols
        // This is critical for the runtime library to work correctly

        // 1) Try global resolution
        let symbol_addr = if let Some(res) = self.resolved_symbols.get(&reloc.symbol_name) {
            res.address as i64
        } else {
            // 2) Local resolution from this object
            let obj = &self.objects[object_id];
            let sym = obj.find_symbol(&reloc.symbol_name).ok_or_else(|| {
                CodegenError::InvalidOperation(format!(
                    "Relocation references unknown symbol {} in object {}",
                    reloc.symbol_name, object_id
                ))
            })?;
            match &sym.def {
                SymDef::Absolute { value } => *value as i64,
                SymDef::Defined {
                    section_name,
                    offset,
                } => {
                    let merged_name = canonicalize_section(section_name);
                    let merged = self.merged_sections.get(merged_name).ok_or_else(|| {
                        CodegenError::InvalidOperation(format!("Unknown section {}", merged_name))
                    })?;
                    let chunk = merged
                        .chunks
                        .iter()
                        .find(|c| c.object_id == object_id && c.input_section_name == *section_name)
                        .ok_or_else(|| {
                            CodegenError::InvalidOperation(
                                "Could not find chunk for local symbol relocation".to_string(),
                            )
                        })?;
                    (merged.base_address + chunk.merged_offset + *offset) as i64
                }
                _ => {
                    return Err(CodegenError::InvalidOperation(
                        "Relocation refers to non-defined/absolute local symbol".into(),
                    ));
                }
            }
        };

        // For GOTPCREL relocations, get GOT section info before mutable borrow
        let got_info = if reloc.kind == RelocKind::GotPcRel {
            let got_offset = self.got_entries.get(&reloc.symbol_name).ok_or_else(|| {
                CodegenError::InvalidOperation(format!(
                    "No GOT entry found for symbol {} in GOTPCREL relocation",
                    reloc.symbol_name
                ))
            })?;

            let got_section = self.merged_sections.get(".got").ok_or_else(|| {
                CodegenError::InvalidOperation(
                    "No .got section found for GOTPCREL relocation".to_string(),
                )
            })?;

            Some((got_section.base_address, *got_offset))
        } else {
            None
        };

        // Map section name to merged section name
        let merged_section_name = canonicalize_section(&reloc.section_name).to_string();

        // Find the merged section containing the relocation site
        let merged = self
            .merged_sections
            .get_mut(&merged_section_name)
            .ok_or_else(|| {
                CodegenError::InvalidOperation(format!(
                    "Relocation in unknown section: {}",
                    reloc.section_name
                ))
            })?;

        // Find the chunk for this object
        let chunk = merged
            .chunks
            .iter()
            .find(|c| c.object_id == object_id && c.input_section_name == reloc.section_name)
            .ok_or_else(|| {
                CodegenError::InvalidOperation("Could not find chunk for relocation".to_string())
            })?;

        // Validate relocation offset is within chunk bounds
        if reloc.offset >= chunk.size {
            return Err(CodegenError::InvalidOperation(format!(
                "Relocation offset {} exceeds chunk size {} for section {}",
                reloc.offset, chunk.size, reloc.section_name
            )));
        }

        // Calculate the actual offset in the merged section
        let merged_offset = chunk.merged_offset + reloc.offset;

        // Apply the relocation based on type
        match reloc.kind {
            RelocKind::Abs64 => {
                // R_X86_64_64: S + A
                let value = (symbol_addr + reloc.addend) as u64;
                let offset = merged_offset as usize;

                if offset + 8 > merged.data.len() {
                    return Err(CodegenError::InvalidOperation(
                        "Relocation offset out of bounds".to_string(),
                    ));
                }

                merged.data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }
            RelocKind::Pc32 | RelocKind::Plt32 => {
                // R_X86_64_PC32 and R_X86_64_PLT32: S + A - P
                // PLT32 is treated the same as PC32 for static linking
                let p = (merged.base_address + merged_offset) as i64;
                let value = symbol_addr + reloc.addend - p;

                // Check if value fits in 32 bits
                if value > i32::MAX as i64 || value < i32::MIN as i64 {
                    return Err(CodegenError::InvalidOperation(format!(
                        "{} relocation overflow: {} does not fit in 32 bits",
                        match reloc.kind {
                            RelocKind::Plt32 => "PLT32",
                            _ => "PC32",
                        },
                        value
                    )));
                }

                let offset = merged_offset as usize;
                if offset + 4 > merged.data.len() {
                    return Err(CodegenError::InvalidOperation(
                        "Relocation offset out of bounds".to_string(),
                    ));
                }

                merged.data[offset..offset + 4].copy_from_slice(&(value as i32).to_le_bytes());
            }
            RelocKind::GotPcRel => {
                // R_X86_64_GOTPCREL: G + GOT + A - P
                // Where G is the GOT entry offset for the symbol

                // Use the pre-computed GOT info to avoid borrowing issues
                let (got_base_address, got_offset) = got_info.ok_or_else(|| {
                    CodegenError::InvalidOperation(
                        "Internal error: GOT info not available for GOTPCREL relocation"
                            .to_string(),
                    )
                })?;

                // Calculate GOT entry address: GOT base + entry offset
                let got_entry_addr = (got_base_address + got_offset) as i64;

                // Calculate relocation value: GOT_entry + A - P
                let p = (merged.base_address + merged_offset) as i64;
                let value = got_entry_addr + reloc.addend - p;

                // Check if value fits in 32 bits
                if value > i32::MAX as i64 || value < i32::MIN as i64 {
                    return Err(CodegenError::InvalidOperation(format!(
                        "GOTPCREL relocation overflow: {} does not fit in 32 bits",
                        value
                    )));
                }

                let offset = merged_offset as usize;
                if offset + 4 > merged.data.len() {
                    return Err(CodegenError::InvalidOperation(
                        "Relocation offset out of bounds".to_string(),
                    ));
                }

                merged.data[offset..offset + 4].copy_from_slice(&(value as i32).to_le_bytes());
            }
        }

        Ok(())
    }

    /// Build the final executable
    fn build_executable(&self) -> Result<LinkedExecutable, CodegenError> {
        let text = self
            .merged_sections
            .get(".text")
            .map(|s| s.data.clone())
            .unwrap_or_default();

        let rodata = self
            .merged_sections
            .get(".rodata")
            .map(|s| s.data.clone())
            .unwrap_or_default();

        let got = self
            .merged_sections
            .get(".got")
            .map(|s| s.data.clone())
            .unwrap_or_default();

        let data = self
            .merged_sections
            .get(".data")
            .map(|s| s.data.clone())
            .unwrap_or_default();

        let bss_size = self
            .merged_sections
            .get(".bss")
            .map(|s| s.nobits_size)
            .unwrap_or(0);

        // Find entry point (_start symbol)
        let entry_point = self
            .resolved_symbols
            .get("_start")
            .map(|s| s.address)
            .ok_or_else(|| CodegenError::InvalidOperation("No _start symbol found".to_string()))?;

        Ok(LinkedExecutable {
            text_section: text,
            rodata_section: rodata,
            got_section: got,
            data_section: data,
            bss_size,
            entry_point,
            merged_sections: self.merged_sections.clone(),
        })
    }
}

/// Align a value to the given alignment
fn align_to(value: u64, alignment: u64) -> u64 {
    if alignment <= 1 {
        value
    } else {
        (value + alignment - 1) & !(alignment - 1)
    }
}

impl Default for Linker {
    fn default() -> Self {
        Self::new()
    }
}
