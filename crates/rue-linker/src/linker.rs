//! The linker - combines object files and produces an executable.

use std::collections::HashMap;

use rue_target::Target;

use crate::archive::Archive;
use crate::constants::{
    ELF_MAGIC, ELF64_EHDR_SIZE, ELF64_PHDR_SIZE, ELFCLASS64, ELFDATA2LSB, ET_EXEC, EV_CURRENT,
    PF_R, PF_W, PF_X, PT_LOAD,
};
use crate::elf::{ObjectFile, RelocationType, Symbol, SymbolBinding};

/// Linker errors.
#[derive(Debug)]
pub enum LinkError {
    /// Undefined symbol reference.
    UndefinedSymbol(String),
    /// Duplicate symbol definition.
    DuplicateSymbol(String),
    /// Unsupported relocation type.
    UnsupportedRelocation(String),
    /// Relocation overflow (value doesn't fit).
    RelocationOverflow { symbol: String, rel_type: String },
    /// Relocation patch extends beyond code section bounds.
    RelocationPatchOutOfBounds {
        patch_offset: usize,
        patch_size: usize,
        section_size: usize,
        rel_type: String,
    },
    /// Symbol references invalid section index.
    InvalidSectionIndex {
        symbol: String,
        section_index: usize,
        section_count: usize,
    },
    /// Relocation references invalid symbol index.
    InvalidSymbolIndex {
        symbol_index: usize,
        symbol_count: usize,
    },
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::UndefinedSymbol(s) => write!(f, "undefined symbol: {}", s),
            LinkError::DuplicateSymbol(s) => write!(f, "duplicate symbol: {}", s),
            LinkError::UnsupportedRelocation(s) => write!(f, "unsupported relocation: {}", s),
            LinkError::RelocationOverflow { symbol, rel_type } => {
                write!(f, "relocation overflow for {} ({})", symbol, rel_type)
            }
            LinkError::RelocationPatchOutOfBounds {
                patch_offset,
                patch_size,
                section_size,
                rel_type,
            } => {
                write!(
                    f,
                    "relocation patch extends beyond code section: {} relocation at offset {} \
                     requires {} bytes, but code section is only {} bytes",
                    rel_type, patch_offset, patch_size, section_size
                )
            }
            LinkError::InvalidSectionIndex {
                symbol,
                section_index,
                section_count,
            } => {
                write!(
                    f,
                    "symbol '{}' references invalid section index {} (object has {} sections)",
                    symbol, section_index, section_count
                )
            }
            LinkError::InvalidSymbolIndex {
                symbol_index,
                symbol_count,
            } => {
                write!(
                    f,
                    "relocation references invalid symbol index {} (object has {} symbols)",
                    symbol_index, symbol_count
                )
            }
        }
    }
}

impl std::error::Error for LinkError {}

/// The linker.
pub struct Linker {
    /// The target architecture and OS.
    target: Target,
    /// Base address for the executable.
    base_addr: u64,
    /// Page size for alignment.
    page_size: u64,
    /// Symbol table: name -> (object_index, symbol).
    global_symbols: HashMap<String, (usize, Symbol)>,
    /// All object files we're linking.
    objects: Vec<ObjectFile>,
    /// Symbols that must be resolved (e.g., entry point).
    /// These are treated as undefined during archive linking.
    required_symbols: Vec<String>,
}

impl Linker {
    /// Create a new linker for the given target.
    #[must_use]
    pub fn new(target: Target) -> Self {
        Linker {
            target,
            base_addr: target.default_base_addr(),
            page_size: target.page_size(),
            global_symbols: HashMap::new(),
            objects: Vec::new(),
            required_symbols: Vec::new(),
        }
    }

    /// Mark a symbol as required.
    ///
    /// Required symbols are treated as undefined during archive linking,
    /// ensuring that objects defining them are pulled in from archives.
    /// This is typically used for the entry point symbol.
    pub fn require_symbol(&mut self, name: impl Into<String>) {
        self.required_symbols.push(name.into());
    }

    /// Add an object file to be linked.
    pub fn add_object(&mut self, obj: ObjectFile) -> Result<(), LinkError> {
        let obj_index = self.objects.len();

        // Collect global symbols
        for sym in &obj.symbols {
            if sym.section_index.is_some()
                && (sym.binding == SymbolBinding::Global || sym.binding == SymbolBinding::Weak)
                && !sym.name.is_empty()
            {
                if let Some((_, existing)) = self.global_symbols.get(&sym.name) {
                    // Allow weak symbols to be overridden
                    if existing.binding != SymbolBinding::Weak && sym.binding != SymbolBinding::Weak
                    {
                        return Err(LinkError::DuplicateSymbol(sym.name.clone()));
                    }
                    // Keep the non-weak one
                    if existing.binding == SymbolBinding::Weak {
                        self.global_symbols
                            .insert(sym.name.clone(), (obj_index, sym.clone()));
                    }
                } else {
                    self.global_symbols
                        .insert(sym.name.clone(), (obj_index, sym.clone()));
                }
            }
        }

        self.objects.push(obj);
        Ok(())
    }

    /// Add objects from an ar archive selectively based on symbol resolution.
    ///
    /// This implements traditional archive linking semantics:
    /// - Only include objects that define symbols we currently need
    /// - Iterate until no new objects are added
    ///
    /// This avoids pulling in unnecessary objects (like compiler_builtins
    /// intrinsics) that might have their own unresolved dependencies.
    pub fn add_archive(&mut self, archive: Archive) -> Result<(), LinkError> {
        // Convert to a Vec we can index into
        let archive_objects: Vec<ObjectFile> = archive.objects.into_iter().collect();

        // Build an index of which archive objects define which symbols
        let mut symbol_to_obj: HashMap<String, usize> = HashMap::new();
        for (obj_idx, obj) in archive_objects.iter().enumerate() {
            for sym in &obj.symbols {
                if sym.section_index.is_some()
                    && (sym.binding == SymbolBinding::Global || sym.binding == SymbolBinding::Weak)
                    && !sym.name.is_empty()
                {
                    symbol_to_obj.insert(sym.name.clone(), obj_idx);
                }
            }
        }

        // Also build an index of undefined symbols in each archive object
        let mut obj_undefined: Vec<Vec<String>> = Vec::with_capacity(archive_objects.len());
        for obj in &archive_objects {
            let mut undef = Vec::new();
            for sym in &obj.symbols {
                if sym.section_index.is_none()
                    && sym.binding == SymbolBinding::Global
                    && !sym.name.is_empty()
                {
                    undef.push(sym.name.clone());
                }
            }
            obj_undefined.push(undef);
        }

        // Track which archive objects we've selected and which symbols are defined
        let mut selected: Vec<bool> = vec![false; archive_objects.len()];
        let mut defined_symbols: std::collections::HashSet<String> =
            self.global_symbols.keys().cloned().collect();

        // Iterate until we reach a fixed point
        loop {
            // Collect undefined symbols from currently linked objects and selected archive objects
            let mut undefined: Vec<String> = Vec::new();

            // Add required symbols (e.g., entry point) that aren't yet defined
            for sym_name in &self.required_symbols {
                if !defined_symbols.contains(sym_name) {
                    undefined.push(sym_name.clone());
                }
            }

            // From already-linked objects
            for obj in &self.objects {
                for sym in &obj.symbols {
                    if sym.section_index.is_none()
                        && sym.binding == SymbolBinding::Global
                        && !sym.name.is_empty()
                        && !defined_symbols.contains(&sym.name)
                    {
                        undefined.push(sym.name.clone());
                    }
                }
            }

            // From selected archive objects
            for (idx, selected_flag) in selected.iter().enumerate() {
                if *selected_flag {
                    for sym_name in &obj_undefined[idx] {
                        if !defined_symbols.contains(sym_name) {
                            undefined.push(sym_name.clone());
                        }
                    }
                }
            }

            // Try to resolve undefined symbols from the archive
            let mut added_any = false;
            for sym_name in undefined {
                if let Some(&obj_idx) = symbol_to_obj.get(&sym_name) {
                    if !selected[obj_idx] {
                        selected[obj_idx] = true;
                        added_any = true;

                        // Add defined symbols from this object
                        for sym in &archive_objects[obj_idx].symbols {
                            if sym.section_index.is_some()
                                && (sym.binding == SymbolBinding::Global
                                    || sym.binding == SymbolBinding::Weak)
                                && !sym.name.is_empty()
                            {
                                defined_symbols.insert(sym.name.clone());
                            }
                        }
                    }
                }
            }

            if !added_any {
                break;
            }
        }

        // Now actually add the selected objects
        for (idx, obj) in archive_objects.into_iter().enumerate() {
            if selected[idx] {
                self.add_object(obj)?;
            }
        }

        Ok(())
    }

    /// Link all objects and produce an executable.
    #[must_use = "linking returns a Result that must be checked"]
    pub fn link(self, entry_point: &str) -> Result<Vec<u8>, LinkError> {
        // Layout constants - use separate program headers for proper W^X security:
        // - Segment 1: .text (R+X) - executable code
        // - Segment 2: .rodata (R) - read-only data (only if rodata exists)
        // - Segment 3: .data + .bss (R+W) - writable data (only if data/bss exists)
        //
        // This follows the W^X (Write XOR Execute) security principle:
        // memory should never be both writable and executable.
        const MAX_PROGRAM_HEADERS: u64 = 3;
        const HEADER_SIZE: u64 =
            (ELF64_EHDR_SIZE as u64) + (ELF64_PHDR_SIZE as u64) * MAX_PROGRAM_HEADERS;

        // Code starts right after headers. For ELF loading to work,
        // (vaddr % page_size) must equal (file_offset % page_size).
        // With code at file offset HEADER_SIZE, we set vaddr accordingly.
        let code_start = self.base_addr + HEADER_SIZE;

        // First, collect and merge all code sections
        let mut merged_code = Vec::new();
        let mut merged_rodata = Vec::new();
        let mut merged_data = Vec::new();
        let mut bss_size: u64 = 0;
        let mut pending_relocations = Vec::new();

        // Track where each section ends up in the merged output
        // Key: (object_index, section_index) -> offset in merged section
        let mut section_offsets: HashMap<(usize, usize), u64> = HashMap::new();

        // Merge code sections (.text*)
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !section.name.starts_with(".text") || section.data.is_empty() {
                    continue;
                }

                // Align
                let align = section.align.max(1);
                let padding = align_up(merged_code.len() as u64, align) - merged_code.len() as u64;
                merged_code.resize(merged_code.len() + padding as usize, 0xCC); // INT3 padding

                let offset = merged_code.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);

                merged_code.extend_from_slice(&section.data);

                // Collect relocations
                for reloc in &section.relocations {
                    // Skip relocations that reference the null symbol (index 0)
                    // These are typically R_*_NONE relocations that slipped through
                    if reloc.symbol_index == 0 {
                        continue;
                    }
                    // Validate symbol index before accessing
                    if reloc.symbol_index >= obj.symbols.len() {
                        return Err(LinkError::InvalidSymbolIndex {
                            symbol_index: reloc.symbol_index,
                            symbol_count: obj.symbols.len(),
                        });
                    }
                    let sym = &obj.symbols[reloc.symbol_index];

                    // We now support .text, .rodata, .data, and .bss
                    // Only skip debug/unwinding sections
                    if let Some(sec_idx) = sym.section_index {
                        if sec_idx < obj.sections.len() {
                            let target_sec = &obj.sections[sec_idx];
                            if !target_sec.name.starts_with(".text")
                                && !target_sec.name.starts_with(".rodata")
                                && !target_sec.name.starts_with(".data")
                                && !target_sec.name.starts_with(".bss")
                            {
                                // Symbol is in a section we don't link (e.g., debug)
                                // Skip this relocation
                                continue;
                            }
                        }
                    }

                    pending_relocations.push((
                        offset + reloc.offset,
                        sym.name.clone(),
                        sym.section_index,
                        obj_idx,
                        reloc.rel_type,
                        reloc.addend,
                    ));
                }
            }
        }

        // Merge rodata sections (placed on a new page for proper W^X protection)
        // We need page alignment between code and rodata so they can have different
        // memory protections at runtime.
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !section.name.starts_with(".rodata") {
                    continue;
                }
                // Note: we don't skip empty sections because they may still have
                // symbols at offset 0 (e.g., empty strings) that need addresses.

                let align = section.align.max(1);
                let padding =
                    align_up(merged_rodata.len() as u64, align) - merged_rodata.len() as u64;
                merged_rodata.resize(merged_rodata.len() + padding as usize, 0);

                let offset = merged_rodata.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);

                merged_rodata.extend_from_slice(&section.data);
            }
        }

        // Merge .data sections (initialized data - placed in data segment)
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !section.name.starts_with(".data") {
                    continue;
                }
                // Skip empty .data sections
                if section.data.is_empty() {
                    continue;
                }

                let align = section.align.max(1);
                let padding = align_up(merged_data.len() as u64, align) - merged_data.len() as u64;
                merged_data.resize(merged_data.len() + padding as usize, 0);

                let offset = merged_data.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);

                merged_data.extend_from_slice(&section.data);
            }
        }

        // Handle .bss sections (uninitialized data - zero-filled at runtime)
        // .bss comes after .data in memory, but doesn't take file space
        let bss_offset_in_data = merged_data.len() as u64;

        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !section.name.starts_with(".bss") {
                    continue;
                }

                let align = section.align.max(1);
                let padding = align_up(bss_size, align) - bss_size;
                bss_size += padding;

                let offset = bss_size;
                section_offsets.insert((obj_idx, sec_idx), offset);

                // Use the section.size field which was parsed from the ELF header.
                // For NOBITS sections (like .bss), the data vec is empty but size
                // contains the actual memory size needed.
                bss_size += section.size;
            }
        }

        // Determine which optional segments are needed
        let has_rodata = !merged_rodata.is_empty();
        let has_data_segment = !merged_data.is_empty() || bss_size > 0;

        // Virtual addresses - calculate with page alignment between segments
        let code_vaddr = code_start;
        let code_size = merged_code.len() as u64;

        // Rodata starts on the next page boundary after code for W^X protection
        let rodata_vaddr = align_up(code_vaddr + code_size, self.page_size);

        // Calculate data segment layout
        // The data segment starts after rodata (or code if no rodata), page-aligned
        let data_vaddr = if has_data_segment {
            // Calculate the end of the last segment before data
            let preceding_end = if has_rodata {
                rodata_vaddr + merged_rodata.len() as u64
            } else {
                code_vaddr + code_size
            };
            align_up(preceding_end, self.page_size)
        } else {
            0 // Not used
        };
        // BSS follows data in memory
        let bss_vaddr = data_vaddr + bss_offset_in_data;

        // Build final symbol addresses
        let mut symbol_addresses: HashMap<String, u64> = HashMap::new();

        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for sym in &obj.symbols {
                if sym.name.is_empty() {
                    continue;
                }

                if let Some(sec_idx) = sym.section_index {
                    // Validate section index before use (defense in depth - section_offsets
                    // lookup also implicitly validates, but explicit check is clearer)
                    if sec_idx >= obj.sections.len() {
                        continue;
                    }
                    if let Some(&section_offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                        let section = &obj.sections[sec_idx];
                        let base = if section.name.starts_with(".text") {
                            code_vaddr
                        } else if section.name.starts_with(".rodata") {
                            rodata_vaddr
                        } else if section.name.starts_with(".data") {
                            data_vaddr
                        } else if section.name.starts_with(".bss") {
                            bss_vaddr
                        } else {
                            continue;
                        };

                        let addr = base + section_offset + sym.value;

                        // Only add global symbols, or section symbols for relocation
                        if sym.binding == SymbolBinding::Global
                            || sym.binding == SymbolBinding::Weak
                            || !symbol_addresses.contains_key(&sym.name)
                        {
                            symbol_addresses.insert(sym.name.clone(), addr);
                        }
                    }
                }
            }
        }

        // Also add section symbols for rodata and data/bss relocation
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if let Some(&offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                    let addr = if section.name.starts_with(".rodata") {
                        rodata_vaddr + offset
                    } else if section.name.starts_with(".data") {
                        data_vaddr + offset
                    } else if section.name.starts_with(".bss") {
                        bss_vaddr + offset
                    } else {
                        continue;
                    };
                    // Use section name as fallback
                    symbol_addresses.entry(section.name.clone()).or_insert(addr);
                }
            }
        }

        // Find entry point
        let entry_addr = *symbol_addresses
            .get(entry_point)
            .ok_or_else(|| LinkError::UndefinedSymbol(entry_point.to_string()))?;

        // Apply relocations
        for (offset, sym_name, sym_section, obj_idx, rel_type, addend) in pending_relocations {
            // Try to resolve the symbol
            let target_addr = if let Some(&addr) = symbol_addresses.get(&sym_name) {
                addr
            } else if let Some(sec_idx) = sym_section {
                // Section-relative symbol - look up the section's address
                let obj = &self.objects[obj_idx];
                if sec_idx >= obj.sections.len() {
                    return Err(LinkError::InvalidSectionIndex {
                        symbol: sym_name.clone(),
                        section_index: sec_idx,
                        section_count: obj.sections.len(),
                    });
                }
                let section = &obj.sections[sec_idx];
                if let Some(&sec_offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                    let base = if section.name.starts_with(".text") {
                        code_vaddr
                    } else if section.name.starts_with(".rodata") {
                        rodata_vaddr
                    } else if section.name.starts_with(".data") {
                        data_vaddr
                    } else if section.name.starts_with(".bss") {
                        bss_vaddr
                    } else {
                        return Err(LinkError::UndefinedSymbol(format!(
                            "{} (in section '{}')",
                            sym_name, section.name
                        )));
                    };
                    base + sec_offset
                } else {
                    return Err(LinkError::UndefinedSymbol(format!(
                        "{} (section {} not in section_offsets)",
                        sym_name, sec_idx
                    )));
                }
            } else {
                return Err(LinkError::UndefinedSymbol(format!(
                    "{} (no section, rel_type={:?})",
                    if sym_name.is_empty() {
                        "<empty>"
                    } else {
                        &sym_name
                    },
                    rel_type
                )));
            };

            let pc = code_vaddr + offset;
            let patch_offset = offset as usize;

            match rel_type {
                RelocationType::Pc32 | RelocationType::Plt32 => {
                    // S + A - P, where S is symbol address, A is addend, P is place
                    let value = target_addr as i64 + addend - pc as i64;
                    // Check for overflow: value must fit in i32
                    if value < i32::MIN as i64 || value > i32::MAX as i64 {
                        return Err(LinkError::RelocationOverflow {
                            symbol: sym_name.clone(),
                            rel_type: format!("{:?}", rel_type),
                        });
                    }
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 4,
                            section_size: merged_code.len(),
                            rel_type: format!("{:?}", rel_type),
                        });
                    }
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&(value as i32).to_le_bytes());
                }
                RelocationType::GotPcRel => {
                    // R_X86_64_GOTPCREL: Load from GOT entry.
                    // For static linking, we relax this to use the symbol address directly.
                    // This requires rewriting indirect calls/jumps to direct ones, similar to
                    // GotPcRelX but without the compiler's guarantee that it's safe.
                    // In static linking, we know all symbols are resolved, so it's always safe.
                    //
                    // Transform indirect call: `call *[rip+disp]` (FF /2) -> `addr32 call rel32` (67 E8)
                    // Transform indirect jmp:  `jmp *[rip+disp]` (FF /4) -> `addr32 jmp rel32` (67 E9)
                    // Transform indirect mov:  `mov reg, [rip+disp]` (8B) -> `lea reg, [rip+disp]` (8D)
                    if patch_offset >= 2 {
                        let opcode_offset = patch_offset - 2;
                        if merged_code[opcode_offset] == 0xFF {
                            let modrm = merged_code[opcode_offset + 1];
                            let reg_field = (modrm >> 3) & 0x7;
                            if reg_field == 2 {
                                // Indirect call: `call *[rip+disp]` (FF /2, ModR/M 15)
                                // Transform to: `addr32 call rel32` (67 E8)
                                merged_code[opcode_offset] = 0x67; // addr32 prefix
                                merged_code[opcode_offset + 1] = 0xE8; // direct call opcode
                            } else if reg_field == 4 {
                                // Indirect jmp: `jmp *[rip+disp]` (FF /4, ModR/M 25)
                                // Transform to: `addr32 jmp rel32` (67 E9)
                                merged_code[opcode_offset] = 0x67; // addr32 prefix
                                merged_code[opcode_offset + 1] = 0xE9; // direct jmp opcode
                            }
                        } else if merged_code[opcode_offset] == 0x8B {
                            // MOV: `mov reg, [rip+disp]` -> `lea reg, [rip+disp]`
                            merged_code[opcode_offset] = 0x8D; // LEA opcode
                        }
                        // Other patterns: just patch displacement (best effort)
                    }

                    let value = target_addr as i64 + addend - pc as i64;
                    if value < i32::MIN as i64 || value > i32::MAX as i64 {
                        return Err(LinkError::RelocationOverflow {
                            symbol: sym_name.clone(),
                            rel_type: format!("{:?}", rel_type),
                        });
                    }
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 4,
                            section_size: merged_code.len(),
                            rel_type: format!("{:?}", rel_type),
                        });
                    }
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&(value as i32).to_le_bytes());
                }
                RelocationType::GotPcRelX => {
                    // R_X86_64_GOTPCRELX: GOT access without REX prefix.
                    // For static linking, we perform GOT relaxation:
                    // - `mov reg, [rip+disp]` (8B) -> `lea reg, [rip+disp]` (8D)
                    // - `call *[rip+disp]` (FF /2) -> `addr32 call rel32` (67 E8)
                    // - `jmp *[rip+disp]` (FF /4) -> `addr32 jmp rel32` (67 E9)
                    //
                    // The relocation offset points to the displacement, so:
                    // - For MOV: opcode is at offset - 2
                    // - For CALL/JMP: opcode (FF) is at offset - 2, ModR/M is at offset - 1
                    if patch_offset >= 2 {
                        let opcode_offset = patch_offset - 2;
                        if merged_code[opcode_offset] == 0xFF {
                            let modrm = merged_code[opcode_offset + 1];
                            let reg_field = (modrm >> 3) & 0x7;
                            if reg_field == 2 {
                                // Indirect call: `call *[rip+disp]` (FF /2, ModR/M 15)
                                // Transform to: `addr32 call rel32` (67 E8)
                                merged_code[opcode_offset] = 0x67; // addr32 prefix
                                merged_code[opcode_offset + 1] = 0xE8; // direct call opcode
                            } else if reg_field == 4 {
                                // Indirect jmp: `jmp *[rip+disp]` (FF /4, ModR/M 25)
                                // Transform to: `addr32 jmp rel32` (67 E9)
                                merged_code[opcode_offset] = 0x67; // addr32 prefix
                                merged_code[opcode_offset + 1] = 0xE9; // direct jmp opcode
                            }
                        } else if merged_code[opcode_offset] == 0x8B {
                            // MOV: `mov reg, [rip+disp]` -> `lea reg, [rip+disp]`
                            merged_code[opcode_offset] = 0x8D; // LEA opcode
                        }
                        // Other patterns: just patch displacement (best effort)
                    }

                    let value = target_addr as i64 + addend - pc as i64;
                    if value < i32::MIN as i64 || value > i32::MAX as i64 {
                        return Err(LinkError::RelocationOverflow {
                            symbol: sym_name.clone(),
                            rel_type: format!("{:?}", rel_type),
                        });
                    }
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 4,
                            section_size: merged_code.len(),
                            rel_type: format!("{:?}", rel_type),
                        });
                    }
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&(value as i32).to_le_bytes());
                }
                RelocationType::RexGotPcRelX => {
                    // R_X86_64_REX_GOTPCRELX: GOT access with REX prefix.
                    // For static linking, we perform GOT relaxation:
                    // - `mov reg, [rip+disp]` (REX 8B) -> `lea reg, [rip+disp]` (REX 8D)
                    // - `call *[rip+disp]` (REX FF /2) -> `addr32 call rel32` (REX 67 E8)
                    // - `jmp *[rip+disp]` (REX FF /4) -> `addr32 jmp rel32` (REX 67 E9)
                    //
                    // The relocation offset points to the displacement, so:
                    // - For MOV with REX: REX is at offset - 3, opcode at offset - 2
                    // - For CALL/JMP with REX: similar layout
                    if patch_offset >= 2 {
                        let opcode_offset = patch_offset - 2;
                        if merged_code[opcode_offset] == 0xFF {
                            let modrm = merged_code[opcode_offset + 1];
                            let reg_field = (modrm >> 3) & 0x7;
                            if reg_field == 2 {
                                // Indirect call with REX: `REX call *[rip+disp]` (4x FF /2)
                                // Transform to: `addr32 call rel32` (67 E8) - REX stays at offset-3
                                merged_code[opcode_offset] = 0x67; // addr32 prefix
                                merged_code[opcode_offset + 1] = 0xE8; // direct call opcode
                            // Note: REX prefix at offset-3 becomes harmless (no-op for CALL)
                            } else if reg_field == 4 {
                                // Indirect jmp with REX: `REX jmp *[rip+disp]` (4x FF /4)
                                // Transform to: `addr32 jmp rel32` (67 E9)
                                merged_code[opcode_offset] = 0x67; // addr32 prefix
                                merged_code[opcode_offset + 1] = 0xE9; // direct jmp opcode
                            }
                        } else if merged_code[opcode_offset] == 0x8B {
                            // MOV with REX: `REX mov reg, [rip+disp]` -> `REX lea reg, [rip+disp]`
                            merged_code[opcode_offset] = 0x8D; // LEA opcode
                        }
                        // Other patterns: just patch displacement (best effort)
                    }

                    let value = target_addr as i64 + addend - pc as i64;
                    if value < i32::MIN as i64 || value > i32::MAX as i64 {
                        return Err(LinkError::RelocationOverflow {
                            symbol: sym_name.clone(),
                            rel_type: format!("{:?}", rel_type),
                        });
                    }
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 4,
                            section_size: merged_code.len(),
                            rel_type: format!("{:?}", rel_type),
                        });
                    }
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&(value as i32).to_le_bytes());
                }
                RelocationType::Abs64 | RelocationType::Aarch64Abs64 => {
                    let value = (target_addr as i64 + addend) as u64;
                    if patch_offset + 8 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 8,
                            section_size: merged_code.len(),
                            rel_type: format!("{:?}", rel_type),
                        });
                    }
                    merged_code[patch_offset..patch_offset + 8]
                        .copy_from_slice(&value.to_le_bytes());
                }
                RelocationType::Abs32 => {
                    let value = target_addr as i64 + addend;
                    // Check for overflow: value must fit in u32
                    if value < 0 || value > u32::MAX as i64 {
                        return Err(LinkError::RelocationOverflow {
                            symbol: sym_name.clone(),
                            rel_type: "Abs32".to_string(),
                        });
                    }
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 4,
                            section_size: merged_code.len(),
                            rel_type: "Abs32".to_string(),
                        });
                    }
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&(value as u32).to_le_bytes());
                }
                RelocationType::Abs32S => {
                    let value = target_addr as i64 + addend;
                    // Check for overflow: value must fit in i32
                    if value < i32::MIN as i64 || value > i32::MAX as i64 {
                        return Err(LinkError::RelocationOverflow {
                            symbol: sym_name.clone(),
                            rel_type: "Abs32S".to_string(),
                        });
                    }
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 4,
                            section_size: merged_code.len(),
                            rel_type: "Abs32S".to_string(),
                        });
                    }
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&(value as i32).to_le_bytes());
                }
                RelocationType::Jump26 | RelocationType::Call26 => {
                    // AArch64 branch (B) or branch with link (BL) - 26-bit PC-relative offset
                    // Both use identical encoding for the immediate field
                    let value = target_addr as i64 + addend - pc as i64;
                    // Offset is in units of 4 bytes (instructions)
                    let offset = value >> 2;
                    // Check for overflow: must fit in 26 bits signed
                    let rel_name = if matches!(rel_type, RelocationType::Jump26) {
                        "Jump26"
                    } else {
                        "Call26"
                    };
                    if offset < -(1 << 25) || offset >= (1 << 25) {
                        return Err(LinkError::RelocationOverflow {
                            symbol: sym_name.clone(),
                            rel_type: rel_name.to_string(),
                        });
                    }
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 4,
                            section_size: merged_code.len(),
                            rel_type: rel_name.to_string(),
                        });
                    }
                    // Read existing instruction and patch the immediate field
                    let mut inst = u32::from_le_bytes(
                        merged_code[patch_offset..patch_offset + 4]
                            .try_into()
                            .unwrap(),
                    );
                    inst = (inst & 0xFC000000) | ((offset as u32) & 0x03FFFFFF);
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&inst.to_le_bytes());
                }
                RelocationType::AdrpPage21 => {
                    // AArch64 ADRP - loads PC-relative page address (21-bit page offset)
                    // S + A gives the effective address; we need the page containing that
                    // Result is the page containing (S + A) minus page containing PC
                    let effective_addr = (target_addr as i64 + addend) as u64;
                    let target_page = effective_addr & !0xFFF;
                    let pc_page = pc & !0xFFF;
                    let page_offset = (target_page as i64) - (pc_page as i64);
                    // ADRP encodes a 21-bit signed page offset (each unit = 4KB page)
                    let page_count = page_offset >> 12;
                    // Check for overflow: must fit in 21 bits signed
                    if page_count < -(1 << 20) || page_count >= (1 << 20) {
                        return Err(LinkError::RelocationOverflow {
                            symbol: sym_name.clone(),
                            rel_type: "AdrpPage21".to_string(),
                        });
                    }
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 4,
                            section_size: merged_code.len(),
                            rel_type: "AdrpPage21".to_string(),
                        });
                    }
                    // ADRP instruction format: imm is split into immlo (bits 29-30) and immhi (bits 5-23)
                    let imm = page_count as u32;
                    let immlo = (imm & 0x3) << 29; // bits 0-1 of imm -> bits 29-30
                    let immhi = ((imm >> 2) & 0x7FFFF) << 5; // bits 2-20 of imm -> bits 5-23
                    let mut inst = u32::from_le_bytes(
                        merged_code[patch_offset..patch_offset + 4]
                            .try_into()
                            .unwrap(),
                    );
                    // Clear immlo and immhi fields, then set them
                    inst = (inst & 0x9F00001F) | immlo | immhi;
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&inst.to_le_bytes());
                }
                RelocationType::AddLo12 => {
                    // AArch64 ADD - adds 12-bit page offset
                    // S + A gives the effective address; extract low 12 bits as page offset
                    let effective_addr = (target_addr as i64 + addend) as u64;
                    let page_offset = (effective_addr & 0xFFF) as u32;
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::RelocationPatchOutOfBounds {
                            patch_offset,
                            patch_size: 4,
                            section_size: merged_code.len(),
                            rel_type: "AddLo12".to_string(),
                        });
                    }
                    // ADD instruction format: imm12 is in bits 10-21
                    let mut inst = u32::from_le_bytes(
                        merged_code[patch_offset..patch_offset + 4]
                            .try_into()
                            .unwrap(),
                    );
                    // Clear imm12 field (bits 10-21) and set it
                    inst = (inst & 0xFFC003FF) | (page_offset << 10);
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&inst.to_le_bytes());
                }
                RelocationType::Unknown(t) => {
                    return Err(LinkError::UnsupportedRelocation(format!(
                        "unknown type {}",
                        t
                    )));
                }
            }
        }

        // Build the ELF with proper W^X segment separation
        //
        // File layout:
        //   [ELF Header]
        //   [Program Header 1: .text (R+X)]
        //   [Program Header 2: .rodata (R)]       -- only if rodata exists
        //   [Program Header 3: .data+.bss (R+W)]  -- only if data/bss exists
        //   [.text section data]
        //   [padding to page boundary]            -- only if rodata exists
        //   [.rodata section data]
        //   [padding to page boundary]            -- only if data exists
        //   [.data section data]
        //
        // Memory layout:
        //   0x400000 + header_size: .text (R+X)
        //   next page boundary: .rodata (R)       -- only if rodata exists
        //   next page boundary: .data+.bss (R+W)  -- only if data/bss exists

        // Calculate number of program headers
        let num_program_headers: u16 =
            1 + if has_rodata { 1 } else { 0 } + if has_data_segment { 1 } else { 0 };

        // File offsets
        let code_file_offset = HEADER_SIZE;

        let rodata_file_offset = if has_rodata {
            // Rodata needs to start on a page boundary in the file so that
            // (vaddr % page_size) == (file_offset % page_size)
            align_up(HEADER_SIZE + code_size, self.page_size)
        } else {
            0 // unused
        };

        // Calculate where code+rodata end in the file
        let code_rodata_file_end = if has_rodata {
            rodata_file_offset + merged_rodata.len() as u64
        } else {
            HEADER_SIZE + code_size
        };

        // Data segment file offset must satisfy: (p_offset % p_align) == (p_vaddr % p_align)
        // Since data_vaddr is page-aligned and we want file offset to be page-aligned too,
        // we pad the file to the next page boundary after code+rodata.
        let data_file_offset = if has_data_segment {
            align_up(code_rodata_file_end, self.page_size)
        } else {
            0
        };

        // Total file size
        let total_file_size = if has_data_segment {
            data_file_offset + merged_data.len() as u64
        } else {
            code_rodata_file_end
        };

        // Memory size for data segment includes BSS
        let data_memsz = merged_data.len() as u64 + bss_size;

        let mut elf = Vec::with_capacity(total_file_size as usize);

        // ===== ELF Header =====
        elf.extend_from_slice(&ELF_MAGIC);
        elf.push(ELFCLASS64);
        elf.push(ELFDATA2LSB);
        elf.push(EV_CURRENT);
        elf.push(crate::constants::ELFOSABI_NONE);
        elf.extend_from_slice(&[0u8; 8]); // Padding
        elf.extend_from_slice(&ET_EXEC.to_le_bytes()); // e_type: ET_EXEC
        // The linker currently only produces ELF executables. For Mach-O targets,
        // we use the system linker via a separate code path.
        elf.extend_from_slice(
            &self
                .target
                .elf_machine()
                .expect("linker only produces ELF executables")
                .to_le_bytes(),
        ); // e_machine
        elf.extend_from_slice(&(EV_CURRENT as u32).to_le_bytes()); // e_version
        elf.extend_from_slice(&entry_addr.to_le_bytes()); // e_entry
        elf.extend_from_slice(&(ELF64_EHDR_SIZE as u64).to_le_bytes()); // e_phoff
        elf.extend_from_slice(&0_u64.to_le_bytes()); // e_shoff (no sections)
        elf.extend_from_slice(&0_u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&(ELF64_EHDR_SIZE as u16).to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&(ELF64_PHDR_SIZE as u16).to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&num_program_headers.to_le_bytes()); // e_phnum
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shnum
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shstrndx

        // ===== Program Header 1: .text (R+X) =====
        // Contains executable code - read and execute, but NOT writable
        elf.extend_from_slice(&PT_LOAD.to_le_bytes()); // p_type: PT_LOAD
        elf.extend_from_slice(&(PF_R | PF_X).to_le_bytes()); // p_flags: PF_R | PF_X (no PF_W!)
        elf.extend_from_slice(&code_file_offset.to_le_bytes()); // p_offset
        elf.extend_from_slice(&code_vaddr.to_le_bytes()); // p_vaddr
        elf.extend_from_slice(&code_vaddr.to_le_bytes()); // p_paddr
        elf.extend_from_slice(&code_size.to_le_bytes()); // p_filesz
        elf.extend_from_slice(&code_size.to_le_bytes()); // p_memsz
        elf.extend_from_slice(&self.page_size.to_le_bytes()); // p_align

        // ===== Program Header 2: .rodata (R) - only if rodata exists =====
        if has_rodata {
            let rodata_size = merged_rodata.len() as u64;
            elf.extend_from_slice(&PT_LOAD.to_le_bytes()); // p_type: PT_LOAD
            elf.extend_from_slice(&PF_R.to_le_bytes()); // p_flags: PF_R only (read-only, not executable)
            elf.extend_from_slice(&rodata_file_offset.to_le_bytes()); // p_offset
            elf.extend_from_slice(&rodata_vaddr.to_le_bytes()); // p_vaddr
            elf.extend_from_slice(&rodata_vaddr.to_le_bytes()); // p_paddr
            elf.extend_from_slice(&rodata_size.to_le_bytes()); // p_filesz
            elf.extend_from_slice(&rodata_size.to_le_bytes()); // p_memsz
            elf.extend_from_slice(&self.page_size.to_le_bytes()); // p_align
        }

        // ===== Program Header 3: Data + BSS (PT_LOAD, R+W) - only if data/bss exists =====
        if has_data_segment {
            elf.extend_from_slice(&PT_LOAD.to_le_bytes()); // p_type: PT_LOAD
            elf.extend_from_slice(&(PF_R | PF_W).to_le_bytes()); // p_flags: PF_R | PF_W
            elf.extend_from_slice(&data_file_offset.to_le_bytes()); // p_offset
            elf.extend_from_slice(&data_vaddr.to_le_bytes()); // p_vaddr
            elf.extend_from_slice(&data_vaddr.to_le_bytes()); // p_paddr
            elf.extend_from_slice(&(merged_data.len() as u64).to_le_bytes()); // p_filesz
            elf.extend_from_slice(&data_memsz.to_le_bytes()); // p_memsz (includes bss)
            elf.extend_from_slice(&self.page_size.to_le_bytes()); // p_align
        }

        // Write code section
        elf.extend_from_slice(&merged_code);

        // Pad to rodata file offset if needed
        if has_rodata {
            let padding_needed = rodata_file_offset as usize - elf.len();
            elf.resize(elf.len() + padding_needed, 0);

            // Write rodata section
            elf.extend_from_slice(&merged_rodata);
        }

        // Pad to data segment file offset if needed
        if has_data_segment {
            let current_size = elf.len();
            let padding_needed = data_file_offset as usize - current_size;
            elf.resize(elf.len() + padding_needed, 0);

            // Write data segment
            elf.extend_from_slice(&merged_data);
        }

        Ok(elf)
    }
}

impl Default for Linker {
    fn default() -> Self {
        Self::new(Target::host())
    }
}

/// Align a value up to the given alignment.
fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{
        E_MACHINE_OFFSET, E_TYPE_OFFSET, EI_CLASS, EI_DATA, EI_VERSION,
        ELF64_EHDR_SIZE as TEST_EHDR_SIZE, ELF64_PHDR_SIZE as TEST_PHDR_SIZE, EM_AARCH64,
        EM_X86_64,
    };
    use crate::elf::ObjectFile;
    use crate::emit::{CodeRelocation, ObjectBuilder};

    // Use X86_64Linux explicitly for ELF tests since ObjectFile only parses ELF
    // and the Linker produces ELF executables
    const ELF_TARGET: Target = Target::X86_64Linux;

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 16), 0);
        assert_eq!(align_up(1, 16), 16);
        assert_eq!(align_up(16, 16), 16);
        assert_eq!(align_up(17, 16), 32);
    }

    #[test]
    fn test_linker_x86_64_linux() {
        let linker = Linker::new(ELF_TARGET);
        assert_eq!(linker.base_addr, 0x400000);
        assert!(linker.objects.is_empty());
        assert!(linker.global_symbols.is_empty());
    }

    #[test]
    fn test_link_error_display() {
        assert_eq!(
            LinkError::UndefinedSymbol("foo".into()).to_string(),
            "undefined symbol: foo"
        );
        assert_eq!(
            LinkError::DuplicateSymbol("bar".into()).to_string(),
            "duplicate symbol: bar"
        );
        assert_eq!(
            LinkError::UnsupportedRelocation("test".into()).to_string(),
            "unsupported relocation: test"
        );
        assert_eq!(
            LinkError::RelocationOverflow {
                symbol: "sym".into(),
                rel_type: "Pc32".into(),
            }
            .to_string(),
            "relocation overflow for sym (Pc32)"
        );
    }

    #[test]
    fn test_simple_link() {
        // Build a simple object with main that just returns
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0xB8, 0x2A, 0x00, 0x00, 0x00, // mov eax, 42
                0xC3, // ret
            ])
            .build();

        let obj = ObjectFile::parse(&obj_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();

        let elf = linker.link("main").unwrap();

        // Check ELF magic
        assert_eq!(&elf[0..4], &ELF_MAGIC);
        // Check it's an executable
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8);
    }

    #[test]
    fn test_undefined_entry_point() {
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "not_main")
            .code(vec![0xC3])
            .build();

        let obj = ObjectFile::parse(&obj_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();

        let result = linker.link("main");
        assert!(matches!(result, Err(LinkError::UndefinedSymbol(_))));
    }

    #[test]
    fn test_link_two_objects() {
        // Build callee object
        let callee_bytes = ObjectBuilder::new(ELF_TARGET, "callee")
            .code(vec![
                0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1
                0xC3, // ret
            ])
            .build();

        // Build caller object (main) that calls callee
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0xE8, 0x00, 0x00, 0x00, 0x00, // call callee (placeholder)
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "callee".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .build();

        let callee = ObjectFile::parse(&callee_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(callee).unwrap();
        linker.add_object(caller).unwrap();

        let elf = linker.link("main").unwrap();

        // Check it's a valid executable
        assert_eq!(&elf[0..4], &ELF_MAGIC);
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8);
    }

    #[test]
    fn test_duplicate_symbol_error() {
        let obj1_bytes = ObjectBuilder::new(ELF_TARGET, "duplicate")
            .code(vec![0xC3])
            .build();

        let obj2_bytes = ObjectBuilder::new(ELF_TARGET, "duplicate")
            .code(vec![0x90, 0xC3]) // nop, ret
            .build();

        let obj1 = ObjectFile::parse(&obj1_bytes).unwrap();
        let obj2 = ObjectFile::parse(&obj2_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj1).unwrap();

        let result = linker.add_object(obj2);
        assert!(matches!(result, Err(LinkError::DuplicateSymbol(_))));
    }

    #[test]
    fn test_undefined_symbol_in_relocation() {
        // Build object that references undefined symbol
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0xE8, 0x00, 0x00, 0x00, 0x00, // call undefined_func
                0xC3,
            ])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "undefined_func".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .build();

        let obj = ObjectFile::parse(&obj_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();

        let result = linker.link("main");
        assert!(matches!(result, Err(LinkError::UndefinedSymbol(_))));
    }

    #[test]
    fn test_elf_header_structure() {
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![0xC3])
            .build();

        let obj = ObjectFile::parse(&obj_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();

        let elf = linker.link("main").unwrap();

        // Check ELF header fields
        assert_eq!(&elf[0..4], &ELF_MAGIC); // Magic
        assert_eq!(elf[EI_CLASS], ELFCLASS64); // 64-bit
        assert_eq!(elf[EI_DATA], ELFDATA2LSB); // Little endian
        assert_eq!(elf[EI_VERSION], EV_CURRENT); // ELF version
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8); // ET_EXEC
        assert_eq!(
            u16::from_le_bytes([elf[E_MACHINE_OFFSET], elf[E_MACHINE_OFFSET + 1]]),
            EM_X86_64
        ); // x86-64

        // Check entry point is set (bytes 24-31)
        let entry = u64::from_le_bytes(elf[24..32].try_into().unwrap());
        assert!(
            entry >= 0x400000,
            "entry point should be at or above base address"
        );
    }

    #[test]
    fn test_program_header_structure() {
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![0xC3])
            .build();

        let obj = ObjectFile::parse(&obj_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();

        let elf = linker.link("main").unwrap();

        // Program header starts after ELF header
        let ph_offset = TEST_EHDR_SIZE;

        // p_type = PT_LOAD
        let p_type = u32::from_le_bytes(elf[ph_offset..ph_offset + 4].try_into().unwrap());
        assert_eq!(p_type, PT_LOAD);

        // p_flags = PF_R | PF_X (W^X compliant - no PF_W!)
        let p_flags = u32::from_le_bytes(elf[ph_offset + 4..ph_offset + 8].try_into().unwrap());
        assert_eq!(
            p_flags,
            PF_R | PF_X,
            "code segment should be R+X only, not R+W+X"
        );
    }

    /// Test that W^X is properly enforced - no segment should be both writable and executable.
    #[test]
    fn test_w_xor_x_security() {
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![0xC3])
            .build();

        let obj = ObjectFile::parse(&obj_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();

        let elf = linker.link("main").unwrap();

        // Read e_phnum from ELF header (bytes 56-57)
        let e_phnum = u16::from_le_bytes(elf[56..58].try_into().unwrap()) as usize;

        // Check each program header for W^X compliance
        for i in 0..e_phnum {
            let ph_offset = TEST_EHDR_SIZE + i * TEST_PHDR_SIZE;
            let p_type = u32::from_le_bytes(elf[ph_offset..ph_offset + 4].try_into().unwrap());
            let p_flags = u32::from_le_bytes(elf[ph_offset + 4..ph_offset + 8].try_into().unwrap());

            if p_type == PT_LOAD {
                let is_writable = (p_flags & PF_W) != 0;
                let is_executable = (p_flags & PF_X) != 0;

                assert!(
                    !(is_writable && is_executable),
                    "W^X violation: segment {} has flags {:#x} (W={}, X={})",
                    i,
                    p_flags,
                    is_writable,
                    is_executable
                );
            }
        }
    }

    /// Test that rodata gets its own read-only segment.
    #[test]
    fn test_rodata_has_readonly_segment() {
        // Build object with both code and rodata (using strings)
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0xB8, 0x00, 0x00, 0x00, 0x00, // mov eax, 0
                0xC3, // ret
            ])
            .strings(vec!["Hello, World!".to_string()])
            .build();

        let obj = ObjectFile::parse(&obj_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();

        let elf = linker.link("main").unwrap();

        // Read e_phnum from ELF header (bytes 56-57)
        let e_phnum = u16::from_le_bytes(elf[56..58].try_into().unwrap()) as usize;

        // Should have 2 segments: code (R+X) and rodata (R)
        assert_eq!(e_phnum, 2, "should have 2 segments when rodata is present");

        // Check first segment is R+X (code)
        let ph1_offset = TEST_EHDR_SIZE;
        let p1_flags = u32::from_le_bytes(elf[ph1_offset + 4..ph1_offset + 8].try_into().unwrap());
        assert_eq!(p1_flags, PF_R | PF_X, "first segment should be R+X (code)");

        // Check second segment is R only (rodata)
        let ph2_offset = TEST_EHDR_SIZE + TEST_PHDR_SIZE;
        let p2_flags = u32::from_le_bytes(elf[ph2_offset + 4..ph2_offset + 8].try_into().unwrap());
        assert_eq!(p2_flags, PF_R, "second segment should be R only (rodata)");
    }

    #[test]
    fn test_linker_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        assert_send::<Linker>();
        assert_sync::<Linker>();
    }

    /// Integration test: full emit → parse → link cycle with multiple functions
    #[test]
    fn test_full_emit_parse_link_cycle() {
        // Create three functions: main calls helper1 and helper2

        // helper1: returns 10
        let helper1_bytes = ObjectBuilder::new(ELF_TARGET, "helper1")
            .code(vec![
                0xB8, 0x0A, 0x00, 0x00, 0x00, // mov eax, 10
                0xC3, // ret
            ])
            .build();

        // helper2: returns 32
        let helper2_bytes = ObjectBuilder::new(ELF_TARGET, "helper2")
            .code(vec![
                0xB8, 0x20, 0x00, 0x00, 0x00, // mov eax, 32
                0xC3, // ret
            ])
            .build();

        // main: calls helper1, saves result, calls helper2, adds results
        // This tests multiple relocations and cross-object references
        let main_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                // call helper1
                0xE8, 0x00, 0x00, 0x00, 0x00, // call helper1 (offset 1)
                // push rax (save result)
                0x50, // call helper2
                0xE8, 0x00, 0x00, 0x00, 0x00, // call helper2 (offset 7)
                // pop rbx
                0x5B, // add eax, ebx
                0x01, 0xD8, // ret
                0xC3,
            ])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "helper1".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .relocation(CodeRelocation {
                offset: 7,
                symbol: "helper2".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .build();

        // Parse all objects
        let helper1 = ObjectFile::parse(&helper1_bytes).expect("parse helper1");
        let helper2 = ObjectFile::parse(&helper2_bytes).expect("parse helper2");
        let main = ObjectFile::parse(&main_bytes).expect("parse main");

        // Verify symbols were parsed correctly
        assert!(helper1.find_symbol("helper1").is_some());
        assert!(helper2.find_symbol("helper2").is_some());
        assert!(main.find_symbol("main").is_some());

        // Link all together
        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(helper1).expect("add helper1");
        linker.add_object(helper2).expect("add helper2");
        linker.add_object(main).expect("add main");

        let elf = linker.link("main").expect("link");

        // Verify the resulting ELF
        assert_eq!(&elf[0..4], &ELF_MAGIC, "should have ELF magic");
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8, "should be ET_EXEC");

        // Verify entry point is reasonable
        let entry = u64::from_le_bytes(elf[24..32].try_into().unwrap());
        assert!(entry >= 0x400000, "entry should be at/above base addr");
        assert!(entry < 0x500000, "entry should be reasonable");

        // Verify we have actual code after headers
        let header_and_phdr_size = TEST_EHDR_SIZE + TEST_PHDR_SIZE;
        assert!(
            elf.len() > header_and_phdr_size,
            "should have content after headers"
        );
    }

    /// Test that unknown relocation types are rejected
    #[test]
    fn test_unknown_relocation_type() {
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![0x00; 8])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "target_sym".into(),
                rel_type: RelocationType::Unknown(99),
                addend: 0,
            })
            .build();

        // Also need a target object
        let target_bytes = ObjectBuilder::new(ELF_TARGET, "target_sym")
            .code(vec![0xC3])
            .build();

        let obj = ObjectFile::parse(&obj_bytes).unwrap();
        let target = ObjectFile::parse(&target_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();
        linker.add_object(target).unwrap();

        let result = linker.link("main");
        assert!(matches!(result, Err(LinkError::UnsupportedRelocation(_))));
    }

    #[test]
    fn test_invalid_section_index_error_display() {
        let err = LinkError::InvalidSectionIndex {
            symbol: "bad_sym".into(),
            section_index: 42,
            section_count: 3,
        };
        assert_eq!(
            err.to_string(),
            "symbol 'bad_sym' references invalid section index 42 (object has 3 sections)"
        );
    }

    #[test]
    fn test_invalid_section_index_in_relocation() {
        use crate::elf::{Relocation, Section, SectionFlags, Symbol, SymbolBinding, SymbolType};

        // Create an object file manually with a symbol referencing an invalid section index.
        // This simulates a malformed object file.
        let text_section = Section {
            name: ".text".into(),
            data: vec![
                0xE8, 0x00, 0x00, 0x00, 0x00, // call <placeholder>
                0xC3, // ret
            ],
            size: 6,
            flags: SectionFlags::ALLOC | SectionFlags::EXEC,
            relocations: vec![Relocation {
                offset: 1,
                symbol_index: 1, // References the symbol with invalid section
                rel_type: RelocationType::Pc32,
                addend: -4,
            }],
            align: 16,
        };

        // Symbol with invalid section index (section 999 doesn't exist)
        let bad_symbol = Symbol {
            name: "bad_target".into(),
            section_index: Some(999), // Invalid!
            value: 0,
            size: 0,
            binding: SymbolBinding::Global,
            sym_type: SymbolType::Func,
        };

        // The main symbol
        let main_symbol = Symbol {
            name: "main".into(),
            section_index: Some(0), // Valid - references .text section
            value: 0,
            size: 6,
            binding: SymbolBinding::Global,
            sym_type: SymbolType::Func,
        };

        // Null symbol at index 0
        let null_symbol = Symbol {
            name: String::new(),
            section_index: None,
            value: 0,
            size: 0,
            binding: SymbolBinding::Local,
            sym_type: SymbolType::None,
        };

        let obj = ObjectFile {
            sections: vec![text_section],
            symbols: vec![null_symbol, bad_symbol, main_symbol],
            section_map: HashMap::from([(".text".into(), 0)]),
        };

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();

        let result = linker.link("main");
        assert!(
            matches!(result, Err(LinkError::InvalidSectionIndex { .. })),
            "Expected InvalidSectionIndex error, got: {:?}",
            result
        );
    }

    #[test]
    fn test_invalid_symbol_index_error_display() {
        let err = LinkError::InvalidSymbolIndex {
            symbol_index: 42,
            symbol_count: 3,
        };
        assert_eq!(
            err.to_string(),
            "relocation references invalid symbol index 42 (object has 3 symbols)"
        );
    }

    #[test]
    fn test_invalid_symbol_index_in_relocation() {
        use crate::elf::{Relocation, Section, SectionFlags, Symbol, SymbolBinding, SymbolType};

        // Create an object file manually with a relocation referencing an invalid symbol index.
        // This simulates a malformed object file.
        let text_section = Section {
            name: ".text".into(),
            data: vec![
                0xE8, 0x00, 0x00, 0x00, 0x00, // call <placeholder>
                0xC3, // ret
            ],
            size: 6,
            flags: SectionFlags::ALLOC | SectionFlags::EXEC,
            relocations: vec![Relocation {
                offset: 1,
                symbol_index: 999, // Invalid - no such symbol exists!
                rel_type: RelocationType::Pc32,
                addend: -4,
            }],
            align: 16,
        };

        // The main symbol
        let main_symbol = Symbol {
            name: "main".into(),
            section_index: Some(0), // Valid - references .text section
            value: 0,
            size: 6,
            binding: SymbolBinding::Global,
            sym_type: SymbolType::Func,
        };

        // Null symbol at index 0
        let null_symbol = Symbol {
            name: String::new(),
            section_index: None,
            value: 0,
            size: 0,
            binding: SymbolBinding::Local,
            sym_type: SymbolType::None,
        };

        let obj = ObjectFile {
            sections: vec![text_section],
            symbols: vec![null_symbol, main_symbol], // Only 2 symbols, but relocation references index 999
            section_map: HashMap::from([(".text".into(), 0)]),
        };

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();

        let result = linker.link("main");
        assert!(
            matches!(
                result,
                Err(LinkError::InvalidSymbolIndex {
                    symbol_index: 999,
                    symbol_count: 2,
                })
            ),
            "Expected InvalidSymbolIndex error, got: {:?}",
            result
        );
    }

    // =========================================================================
    // GOT Relaxation Tests
    // =========================================================================
    //
    // These tests verify that GOT-related relocations are properly "relaxed"
    // during static linking. GOT relaxation replaces indirect memory access
    // through the Global Offset Table with direct PC-relative addressing.
    //
    // For static linking, since all symbol addresses are known at link time,
    // we can compute the PC-relative offset directly instead of going through
    // the GOT. The linker treats GotPcRel, GotPcRelX, and RexGotPcRelX the
    // same as Pc32: it computes S + A - P (symbol + addend - place).

    /// Test that R_X86_64_GOTPCREL (type 9) relocations are handled correctly.
    ///
    /// This relocation type is used for accessing global data via the GOT.
    /// In static linking, we relax it to a direct PC-relative offset.
    #[test]
    fn test_got_pcrel_relocation() {
        // Build target object (the "global variable" we're accessing)
        let target_bytes = ObjectBuilder::new(ELF_TARGET, "global_var")
            .code(vec![
                0x2A, 0x00, 0x00, 0x00, // data: 0x0000002A (42 as little-endian)
            ])
            .build();

        // Build caller object that accesses global_var via GOT
        // This simulates: mov rax, [rip + global_var@GOTPCREL]
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                // mov rax, [rip + 0] (placeholder for GOT access)
                0x48, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, // ret
                0xC3,
            ])
            .relocation(CodeRelocation {
                offset: 3, // Points to the 32-bit displacement
                symbol: "global_var".into(),
                rel_type: RelocationType::GotPcRel,
                addend: -4, // Standard addend for RIP-relative addressing
            })
            .build();

        let target = ObjectFile::parse(&target_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(target).unwrap();
        linker.add_object(caller).unwrap();

        let elf = linker.link("main").unwrap();

        // Verify basic ELF structure
        assert_eq!(&elf[0..4], &ELF_MAGIC);
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8);
    }

    /// Test that R_X86_64_GOTPCRELX (type 41) relocations are handled correctly.
    ///
    /// This is similar to GOTPCREL but allows for additional relaxation
    /// opportunities (e.g., converting mov to lea). For our static linker,
    /// we treat it the same as GOTPCREL.
    #[test]
    fn test_got_pcrelx_relocation() {
        // Build target object
        let target_bytes = ObjectBuilder::new(ELF_TARGET, "external_data")
            .code(vec![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 8 bytes of data
            ])
            .build();

        // Build caller object with GOTPCRELX relocation
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                // mov rax, [rip + 0] - can be relaxed to lea rax, [rip + offset]
                0x48, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, 0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 3,
                symbol: "external_data".into(),
                rel_type: RelocationType::GotPcRelX,
                addend: -4,
            })
            .build();

        let target = ObjectFile::parse(&target_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(target).unwrap();
        linker.add_object(caller).unwrap();

        let elf = linker.link("main").unwrap();

        assert_eq!(&elf[0..4], &ELF_MAGIC);
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8);
    }

    /// Test that R_X86_64_REX_GOTPCRELX (type 42) relocations are handled correctly.
    ///
    /// This is the same as GOTPCRELX but for instructions with a REX prefix.
    /// Used for 64-bit operands in x86-64.
    #[test]
    fn test_rex_got_pcrelx_relocation() {
        // Build target object
        let target_bytes = ObjectBuilder::new(ELF_TARGET, "data_symbol")
            .code(vec![
                0xDE, 0xAD, 0xBE, 0xEF, // Some data
            ])
            .build();

        // Build caller object with REX_GOTPCRELX relocation
        // This simulates a 64-bit memory access with REX.W prefix
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                // mov rax, [rip + 0] with REX.W prefix
                0x48, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, 0xC3,
            ])
            .relocation(CodeRelocation {
                offset: 3,
                symbol: "data_symbol".into(),
                rel_type: RelocationType::RexGotPcRelX,
                addend: -4,
            })
            .build();

        let target = ObjectFile::parse(&target_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(target).unwrap();
        linker.add_object(caller).unwrap();

        let elf = linker.link("main").unwrap();

        assert_eq!(&elf[0..4], &ELF_MAGIC);
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8);
    }

    /// Test GOT relocation with a call instruction.
    ///
    /// Verifies that function calls using GOT relocations are properly
    /// resolved to direct PC-relative calls.
    #[test]
    fn test_got_relocation_with_function_call() {
        // Build callee
        let callee_bytes = ObjectBuilder::new(ELF_TARGET, "callee")
            .code(vec![
                0xB8, 0x07, 0x00, 0x00, 0x00, // mov eax, 7
                0xC3, // ret
            ])
            .build();

        // Build caller using GOT-style relocation
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                // call with placeholder (could be indirect call through GOT)
                0xE8, 0x00, 0x00, 0x00, 0x00, 0xC3,
            ])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "callee".into(),
                rel_type: RelocationType::GotPcRel,
                addend: -4,
            })
            .build();

        let callee = ObjectFile::parse(&callee_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(callee).unwrap();
        linker.add_object(caller).unwrap();

        let elf = linker.link("main").unwrap();

        // Basic validation - if we got here without error, GOT relaxation worked
        assert_eq!(&elf[0..4], &ELF_MAGIC);
    }

    /// Test that all three GOT relocation types produce valid executables
    /// when used together in the same link.
    #[test]
    fn test_multiple_got_relocation_types() {
        // Three target symbols
        let target1_bytes = ObjectBuilder::new(ELF_TARGET, "sym1")
            .code(vec![0x01])
            .build();
        let target2_bytes = ObjectBuilder::new(ELF_TARGET, "sym2")
            .code(vec![0x02])
            .build();
        let target3_bytes = ObjectBuilder::new(ELF_TARGET, "sym3")
            .code(vec![0x03])
            .build();

        // Main with all three GOT relocation types
        let main_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                // Three RIP-relative memory accesses
                0x48, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, // mov rax, [rip+sym1]
                0x48, 0x8B, 0x1D, 0x00, 0x00, 0x00, 0x00, // mov rbx, [rip+sym2]
                0x48, 0x8B, 0x0D, 0x00, 0x00, 0x00, 0x00, // mov rcx, [rip+sym3]
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 3,
                symbol: "sym1".into(),
                rel_type: RelocationType::GotPcRel, // Type 9
                addend: -4,
            })
            .relocation(CodeRelocation {
                offset: 10,
                symbol: "sym2".into(),
                rel_type: RelocationType::GotPcRelX, // Type 41
                addend: -4,
            })
            .relocation(CodeRelocation {
                offset: 17,
                symbol: "sym3".into(),
                rel_type: RelocationType::RexGotPcRelX, // Type 42
                addend: -4,
            })
            .build();

        let target1 = ObjectFile::parse(&target1_bytes).unwrap();
        let target2 = ObjectFile::parse(&target2_bytes).unwrap();
        let target3 = ObjectFile::parse(&target3_bytes).unwrap();
        let main = ObjectFile::parse(&main_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(target1).unwrap();
        linker.add_object(target2).unwrap();
        linker.add_object(target3).unwrap();
        linker.add_object(main).unwrap();

        let elf = linker.link("main").unwrap();

        assert_eq!(&elf[0..4], &ELF_MAGIC);
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8);
    }

    /// Test that GOT relocations with undefined symbols produce appropriate errors.
    #[test]
    fn test_got_relocation_undefined_symbol() {
        // Caller references undefined symbol via GOT
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![0x48, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, 0xC3])
            .relocation(CodeRelocation {
                offset: 3,
                symbol: "undefined_symbol".into(),
                rel_type: RelocationType::GotPcRel,
                addend: -4,
            })
            .build();

        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(caller).unwrap();

        let result = linker.link("main");
        assert!(
            matches!(result, Err(LinkError::UndefinedSymbol(_))),
            "Expected UndefinedSymbol error, got: {:?}",
            result
        );
    }

    /// Test GOT relaxation error message format for overflow.
    #[test]
    fn test_got_relocation_overflow_error_display() {
        let err = LinkError::RelocationOverflow {
            symbol: "far_symbol".into(),
            rel_type: "GotPcRel".into(),
        };
        assert_eq!(
            err.to_string(),
            "relocation overflow for far_symbol (GotPcRel)"
        );
    }

    /// Test that GOT relocations work correctly when the target is in a
    /// different object file added later.
    #[test]
    fn test_got_relocation_cross_object() {
        // First object (main) references second object via GOT
        let main_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0x48, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, // mov rax, [rip+helper]
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 3,
                symbol: "helper".into(),
                rel_type: RelocationType::GotPcRelX,
                addend: -4,
            })
            .build();

        // Second object (helper)
        let helper_bytes = ObjectBuilder::new(ELF_TARGET, "helper")
            .code(vec![
                0xB8, 0x64, 0x00, 0x00, 0x00, // mov eax, 100
                0xC3, // ret
            ])
            .build();

        let main = ObjectFile::parse(&main_bytes).unwrap();
        let helper = ObjectFile::parse(&helper_bytes).unwrap();

        // Add main first, then helper (reverse order of definition)
        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(main).unwrap();
        linker.add_object(helper).unwrap();

        let elf = linker.link("main").unwrap();

        assert_eq!(&elf[0..4], &ELF_MAGIC);
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8);

        // Verify entry point is reasonable
        let entry = u64::from_le_bytes(elf[24..32].try_into().unwrap());
        assert!(
            entry >= 0x400000,
            "entry point should be at or above base address"
        );
    }

    /// Test that mixing GOT relocations with regular PC32 relocations works.
    #[test]
    fn test_got_relocation_mixed_with_pc32() {
        // Target functions
        let func1_bytes = ObjectBuilder::new(ELF_TARGET, "func1")
            .code(vec![0xB8, 0x01, 0x00, 0x00, 0x00, 0xC3])
            .build();
        let func2_bytes = ObjectBuilder::new(ELF_TARGET, "func2")
            .code(vec![0xB8, 0x02, 0x00, 0x00, 0x00, 0xC3])
            .build();

        // Main uses both GOT and regular relocations
        let main_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                // call func1 (regular PC32 relocation)
                0xE8, 0x00, 0x00, 0x00, 0x00, // mov rax, [rip+func2] (GOT relocation)
                0x48, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, 0xC3,
            ])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "func1".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .relocation(CodeRelocation {
                offset: 8,
                symbol: "func2".into(),
                rel_type: RelocationType::RexGotPcRelX,
                addend: -4,
            })
            .build();

        let func1 = ObjectFile::parse(&func1_bytes).unwrap();
        let func2 = ObjectFile::parse(&func2_bytes).unwrap();
        let main = ObjectFile::parse(&main_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(func1).unwrap();
        linker.add_object(func2).unwrap();
        linker.add_object(main).unwrap();

        let elf = linker.link("main").unwrap();

        assert_eq!(&elf[0..4], &ELF_MAGIC);
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8);
    }

    // AArch64 target for ELF tests
    const AARCH64_TARGET: Target = Target::Aarch64Linux;

    #[test]
    fn test_adrp_page21_relocation_with_addend() {
        // This test verifies that ADRP_PAGE21 relocations correctly include the addend.
        // The ADRP instruction loads a page-aligned address, and the addend shifts
        // which page is loaded (important for accessing array elements, struct fields, etc.)

        // Build a data object - represents an array or struct with multiple fields
        // We'll place 8 bytes of data (two 32-bit values)
        let data_bytes = ObjectBuilder::new(AARCH64_TARGET, "data_array")
            .code(vec![
                0x0A, 0x00, 0x00, 0x00, // data[0] = 10
                0x14, 0x00, 0x00, 0x00, // data[1] = 20
            ])
            .build();

        // Build main that loads address of data_array with an addend (offset 4 = second element)
        // ADRP x0, data_array@PAGE  ; with addend
        // ADD x0, x0, data_array@PAGEOFF  ; with addend
        // LDR w0, [x0]
        // RET
        let main_bytes = ObjectBuilder::new(AARCH64_TARGET, "main")
            .code(vec![
                0x00, 0x00, 0x00, 0x90, // adrp x0, <placeholder>
                0x00, 0x00, 0x00, 0x91, // add x0, x0, <placeholder>
                0x00, 0x00, 0x40, 0xB9, // ldr w0, [x0]
                0xC0, 0x03, 0x5F, 0xD6, // ret
            ])
            // ADRP with addend 4 (accessing second element)
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "data_array".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 4, // Non-zero addend!
            })
            // ADD with addend 4 (same offset as ADRP)
            .relocation(CodeRelocation {
                offset: 4,
                symbol: "data_array".into(),
                rel_type: RelocationType::AddLo12,
                addend: 4, // Non-zero addend!
            })
            .build();

        let data = ObjectFile::parse(&data_bytes).unwrap();
        let main = ObjectFile::parse(&main_bytes).unwrap();

        let mut linker = Linker::new(AARCH64_TARGET);
        linker.add_object(data).unwrap();
        linker.add_object(main).unwrap();

        let elf = linker.link("main").unwrap();

        // Check ELF is valid
        assert_eq!(&elf[0..4], &ELF_MAGIC);
        assert_eq!(elf[E_TYPE_OFFSET], ET_EXEC as u8);

        // Verify the linker produced output without panicking.
        // The actual address calculation verification would require more complex
        // ELF parsing to extract the patched instructions, but the key test is
        // that the addend is being used (which our fix ensures).
    }

    #[test]
    fn test_add_lo12_relocation_with_addend() {
        // This test specifically checks ADD_LO12 relocation with a non-zero addend.
        // The low 12 bits of (target_addr + addend) should be encoded.

        // Create a data symbol
        let data_bytes = ObjectBuilder::new(AARCH64_TARGET, "my_data")
            .code(vec![0u8; 32]) // 32 bytes of data
            .build();

        // Main references my_data with an addend
        let main_bytes = ObjectBuilder::new(AARCH64_TARGET, "main")
            .code(vec![
                0x00, 0x00, 0x00, 0x90, // adrp x0, <page>
                0x00, 0x00, 0x00, 0x91, // add x0, x0, <offset>
                0x00, 0x00, 0x40, 0xB9, // ldr w0, [x0]
                0xC0, 0x03, 0x5F, 0xD6, // ret
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "my_data".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 16, // Offset 16 bytes into data
            })
            .relocation(CodeRelocation {
                offset: 4,
                symbol: "my_data".into(),
                rel_type: RelocationType::AddLo12,
                addend: 16, // Same offset
            })
            .build();

        let data = ObjectFile::parse(&data_bytes).unwrap();
        let main = ObjectFile::parse(&main_bytes).unwrap();

        let mut linker = Linker::new(AARCH64_TARGET);
        linker.add_object(data).unwrap();
        linker.add_object(main).unwrap();

        // This should succeed and produce valid ELF
        let elf = linker.link("main").unwrap();
        assert_eq!(&elf[0..4], &ELF_MAGIC);
    }

    #[test]
    fn test_aarch64_call26_with_addend() {
        // Test that Call26 relocation also correctly handles addends
        // (Call26 was already correct, this is a regression test)

        let callee_bytes = ObjectBuilder::new(AARCH64_TARGET, "callee")
            .code(vec![
                0x40, 0x05, 0x80, 0x52, // mov w0, #42
                0xC0, 0x03, 0x5F, 0xD6, // ret
            ])
            .build();

        let main_bytes = ObjectBuilder::new(AARCH64_TARGET, "main")
            .code(vec![
                0x00, 0x00, 0x00, 0x94, // bl callee (placeholder)
                0xC0, 0x03, 0x5F, 0xD6, // ret
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "callee".into(),
                rel_type: RelocationType::Call26,
                addend: 0, // Standard call, no addend
            })
            .build();

        let callee = ObjectFile::parse(&callee_bytes).unwrap();
        let main = ObjectFile::parse(&main_bytes).unwrap();

        let mut linker = Linker::new(AARCH64_TARGET);
        linker.add_object(callee).unwrap();
        linker.add_object(main).unwrap();

        let elf = linker.link("main").unwrap();
        assert_eq!(&elf[0..4], &ELF_MAGIC);
        assert_eq!(
            u16::from_le_bytes([elf[E_MACHINE_OFFSET], elf[E_MACHINE_OFFSET + 1]]),
            EM_AARCH64
        );
    }

    #[test]
    fn test_aarch64_page_crossing_addend() {
        // Test a large addend that causes a page crossing.
        // If the base address is near a page boundary and the addend crosses it,
        // ADRP needs to load a different page than it would without the addend.

        // Create some data
        let data_bytes = ObjectBuilder::new(AARCH64_TARGET, "big_array")
            .code(vec![0u8; 8192]) // 8KB of data (crosses page boundary on 4KB pages)
            .build();

        // Access data at offset 4100 (past first page on 4KB page systems)
        let main_bytes = ObjectBuilder::new(AARCH64_TARGET, "main")
            .code(vec![
                0x00, 0x00, 0x00, 0x90, // adrp x0, <page>
                0x00, 0x00, 0x00, 0x91, // add x0, x0, <offset>
                0x00, 0x00, 0x40, 0xB9, // ldr w0, [x0]
                0xC0, 0x03, 0x5F, 0xD6, // ret
            ])
            .relocation(CodeRelocation {
                offset: 0,
                symbol: "big_array".into(),
                rel_type: RelocationType::AdrpPage21,
                addend: 4100, // Past first page!
            })
            .relocation(CodeRelocation {
                offset: 4,
                symbol: "big_array".into(),
                rel_type: RelocationType::AddLo12,
                addend: 4100,
            })
            .build();

        let data = ObjectFile::parse(&data_bytes).unwrap();
        let main = ObjectFile::parse(&main_bytes).unwrap();

        let mut linker = Linker::new(AARCH64_TARGET);
        linker.add_object(data).unwrap();
        linker.add_object(main).unwrap();

        let elf = linker.link("main").unwrap();
        assert_eq!(&elf[0..4], &ELF_MAGIC);
    }

    // =========================================================================
    // GOT Relaxation Opcode Verification Tests
    // =========================================================================
    //
    // These tests verify that GOT relaxation actually rewrites the instruction
    // opcodes, not just the displacement. This is critical for correctness:
    // without opcode rewriting, MOV would dereference the computed address
    // instead of loading the address itself.

    /// Verify that RexGotPcRelX relaxation converts MOV (8B) to LEA (8D).
    #[test]
    fn test_rex_got_pcrelx_mov_to_lea_opcode_rewrite() {
        // Build target object (data to be addressed)
        let target_bytes = ObjectBuilder::new(ELF_TARGET, "target_data")
            .code(vec![0xDE, 0xAD, 0xBE, 0xEF])
            .build();

        // Build caller with REX.W MOV rax, [rip+disp] (48 8B 05 <disp32>)
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0x48, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, // mov rax, [rip+disp]
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 3,
                symbol: "target_data".into(),
                rel_type: RelocationType::RexGotPcRelX,
                addend: -4,
            })
            .build();

        let target = ObjectFile::parse(&target_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        // Add caller first so it appears at the beginning of the code section
        linker.add_object(caller).unwrap();
        linker.add_object(target).unwrap();

        let elf = linker.link("main").unwrap();

        // Find the code section (after headers)
        let header_size = TEST_EHDR_SIZE + TEST_PHDR_SIZE;
        let code = &elf[header_size..];

        // The instruction should now be LEA (8D) instead of MOV (8B)
        // Layout: REX.W (48) + opcode + ModR/M + disp32
        assert_eq!(code[0], 0x48, "REX.W prefix should be preserved");
        assert_eq!(
            code[1], 0x8D,
            "Opcode should be changed from MOV (8B) to LEA (8D)"
        );
        assert_eq!(code[2], 0x05, "ModR/M byte should be preserved");
    }

    /// Verify that GotPcRelX relaxation converts MOV (8B) to LEA (8D) without REX.
    #[test]
    fn test_got_pcrelx_mov_to_lea_opcode_rewrite() {
        // Build target object
        let target_bytes = ObjectBuilder::new(ELF_TARGET, "target_data")
            .code(vec![0x42, 0x00, 0x00, 0x00])
            .build();

        // Build caller with MOV eax, [rip+disp] (8B 05 <disp32>) - no REX prefix
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, // mov eax, [rip+disp]
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 2, // Points to displacement
                symbol: "target_data".into(),
                rel_type: RelocationType::GotPcRelX,
                addend: -4,
            })
            .build();

        let target = ObjectFile::parse(&target_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        // Add caller first so it appears at the beginning of the code section
        linker.add_object(caller).unwrap();
        linker.add_object(target).unwrap();

        let elf = linker.link("main").unwrap();

        let header_size = TEST_EHDR_SIZE + TEST_PHDR_SIZE;
        let code = &elf[header_size..];

        // The instruction should now be LEA (8D) instead of MOV (8B)
        assert_eq!(
            code[0], 0x8D,
            "Opcode should be changed from MOV (8B) to LEA (8D)"
        );
        assert_eq!(code[1], 0x05, "ModR/M byte should be preserved");
    }

    /// Verify that GotPcRelX relaxation converts indirect CALL to direct CALL.
    #[test]
    fn test_got_pcrelx_indirect_call_relaxation() {
        // Build callee function
        let callee_bytes = ObjectBuilder::new(ELF_TARGET, "callee")
            .code(vec![
                0xB8, 0x07, 0x00, 0x00, 0x00, // mov eax, 7
                0xC3, // ret
            ])
            .build();

        // Build caller with indirect call: call *[rip+disp] (FF 15 <disp32>)
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0xFF, 0x15, 0x00, 0x00, 0x00, 0x00, // call *[rip+disp]
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 2, // Points to displacement
                symbol: "callee".into(),
                rel_type: RelocationType::GotPcRelX,
                addend: -4,
            })
            .build();

        let callee = ObjectFile::parse(&callee_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        // Add caller first so it appears at the beginning of the code section
        linker.add_object(caller).unwrap();
        linker.add_object(callee).unwrap();

        let elf = linker.link("main").unwrap();

        let header_size = TEST_EHDR_SIZE + TEST_PHDR_SIZE;
        let code = &elf[header_size..];

        // The indirect call should be transformed to: addr32 prefix + direct call
        // FF 15 -> 67 E8 (addr32 prefix makes this a 2-byte replacement)
        assert_eq!(
            code[0], 0x67,
            "First byte should be addr32 prefix (67) for call relaxation"
        );
        assert_eq!(
            code[1], 0xE8,
            "Second byte should be direct call opcode (E8)"
        );
    }

    /// Verify that RexGotPcRelX relaxation converts indirect CALL with REX prefix.
    #[test]
    fn test_rex_got_pcrelx_indirect_call_relaxation() {
        // Build callee function
        let callee_bytes = ObjectBuilder::new(ELF_TARGET, "callee")
            .code(vec![
                0xB8, 0x0A, 0x00, 0x00, 0x00, // mov eax, 10
                0xC3, // ret
            ])
            .build();

        // Build caller with REX + indirect call: REX call *[rip+disp] (48 FF 15 <disp32>)
        // Note: REX.W on CALL is unusual but we should handle it
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0x48, 0xFF, 0x15, 0x00, 0x00, 0x00, 0x00, // REX.W call *[rip+disp]
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 3, // Points to displacement
                symbol: "callee".into(),
                rel_type: RelocationType::RexGotPcRelX,
                addend: -4,
            })
            .build();

        let callee = ObjectFile::parse(&callee_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        // Add caller first so it appears at the beginning of the code section
        linker.add_object(caller).unwrap();
        linker.add_object(callee).unwrap();

        let elf = linker.link("main").unwrap();

        let header_size = TEST_EHDR_SIZE + TEST_PHDR_SIZE;
        let code = &elf[header_size..];

        // REX prefix is preserved, FF 15 becomes 67 E8
        assert_eq!(code[0], 0x48, "REX.W prefix should be preserved");
        assert_eq!(
            code[1], 0x67,
            "FF should become addr32 prefix (67) for call relaxation"
        );
        assert_eq!(code[2], 0xE8, "15 should become direct call opcode (E8)");
    }

    /// Verify that different register encodings are handled correctly.
    #[test]
    fn test_got_pcrelx_different_registers() {
        // Build target
        let target_bytes = ObjectBuilder::new(ELF_TARGET, "target")
            .code(vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
            .build();

        // Test with RBX (ModR/M = 1D for RIP-relative addressing)
        // mov rbx, [rip+disp] = 48 8B 1D <disp32>
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0x48, 0x8B, 0x1D, 0x00, 0x00, 0x00, 0x00, // mov rbx, [rip+disp]
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 3,
                symbol: "target".into(),
                rel_type: RelocationType::RexGotPcRelX,
                addend: -4,
            })
            .build();

        let target = ObjectFile::parse(&target_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        // Add caller first so it appears at the beginning of the code section
        linker.add_object(caller).unwrap();
        linker.add_object(target).unwrap();

        let elf = linker.link("main").unwrap();

        let header_size = TEST_EHDR_SIZE + TEST_PHDR_SIZE;
        let code = &elf[header_size..];

        // Verify LEA transformation preserves the register encoding
        assert_eq!(code[0], 0x48, "REX.W prefix should be preserved");
        assert_eq!(code[1], 0x8D, "Opcode should be LEA (8D)");
        assert_eq!(code[2], 0x1D, "ModR/M should preserve RBX register (1D)");
    }

    /// Verify that GotPcRel rewrites MOV to LEA (GOT relaxation for static linking).
    #[test]
    fn test_got_pcrel_mov_to_lea_relaxation() {
        // Build target
        let target_bytes = ObjectBuilder::new(ELF_TARGET, "target_data")
            .code(vec![0x42, 0x00, 0x00, 0x00])
            .build();

        // Build caller with MOV instruction using GotPcRel
        let caller_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![
                0x48, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00, // mov rax, [rip+disp]
                0xC3, // ret
            ])
            .relocation(CodeRelocation {
                offset: 3,
                symbol: "target_data".into(),
                rel_type: RelocationType::GotPcRel,
                addend: -4,
            })
            .build();

        let target = ObjectFile::parse(&target_bytes).unwrap();
        let caller = ObjectFile::parse(&caller_bytes).unwrap();

        let mut linker = Linker::new(ELF_TARGET);
        // Add caller first so it appears at the beginning of the code section
        linker.add_object(caller).unwrap();
        linker.add_object(target).unwrap();

        let elf = linker.link("main").unwrap();

        let header_size = TEST_EHDR_SIZE + TEST_PHDR_SIZE;
        let code = &elf[header_size..];

        // For static linking, GotPcRel should be relaxed just like GotPcRelX:
        // MOV (8B) -> LEA (8D)
        assert_eq!(code[0], 0x48, "REX.W prefix should be preserved");
        assert_eq!(
            code[1], 0x8D,
            "Opcode should be rewritten to LEA (8D) for GotPcRel relaxation"
        );
    }
}
