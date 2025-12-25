//! The linker - combines object files and produces an executable.

use std::collections::HashMap;

use rue_target::Target;

use crate::archive::Archive;
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
    pub fn link(self, entry_point: &str) -> Result<Vec<u8>, LinkError> {
        // Layout constants - use a single program header for simplicity
        const ELF_HEADER_SIZE: u64 = 64;
        const PROGRAM_HEADER_SIZE: u64 = 56;
        const NUM_PROGRAM_HEADERS: u64 = 1;
        const HEADER_SIZE: u64 = ELF_HEADER_SIZE + PROGRAM_HEADER_SIZE * NUM_PROGRAM_HEADERS;

        // Code starts right after headers. For ELF loading to work,
        // (vaddr % page_size) must equal (file_offset % page_size).
        // With code at file offset HEADER_SIZE, we set vaddr accordingly.
        let code_start = self.base_addr + HEADER_SIZE;

        // First, collect and merge all code sections
        let mut merged_code = Vec::new();
        let mut merged_rodata = Vec::new();
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
                merged_code.extend(std::iter::repeat(0xCC).take(padding as usize)); // INT3 padding

                let offset = merged_code.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);

                merged_code.extend_from_slice(&section.data);

                // Collect relocations
                for reloc in &section.relocations {
                    let sym = &obj.symbols[reloc.symbol_index];
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

        // Merge rodata sections (placed right after code, no page break)
        let rodata_offset_in_merged = merged_code.len();

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
                merged_rodata.extend(std::iter::repeat(0).take(padding as usize));

                let offset = merged_rodata.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);

                merged_rodata.extend_from_slice(&section.data);
            }
        }

        // Virtual addresses - rodata follows code directly
        let code_vaddr = code_start;
        let rodata_vaddr = code_vaddr + rodata_offset_in_merged as u64;

        // Build final symbol addresses
        let mut symbol_addresses: HashMap<String, u64> = HashMap::new();

        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for sym in &obj.symbols {
                if sym.name.is_empty() {
                    continue;
                }

                if let Some(sec_idx) = sym.section_index {
                    if let Some(&section_offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                        let section = &obj.sections[sec_idx];
                        let base = if section.name.starts_with(".text") {
                            code_vaddr
                        } else if section.name.starts_with(".rodata") {
                            rodata_vaddr
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

        // Also add section symbols for rodata relocation
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if section.name.starts_with(".rodata") {
                    if let Some(&offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                        let addr = rodata_vaddr + offset;
                        // Use section name as fallback
                        symbol_addresses.entry(section.name.clone()).or_insert(addr);
                    }
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
                let section = &obj.sections[sec_idx];
                if let Some(&sec_offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                    let base = if section.name.starts_with(".text") {
                        code_vaddr
                    } else if section.name.starts_with(".rodata") {
                        rodata_vaddr
                    } else {
                        return Err(LinkError::UndefinedSymbol(sym_name));
                    };
                    base + sec_offset
                } else {
                    return Err(LinkError::UndefinedSymbol(sym_name));
                }
            } else {
                return Err(LinkError::UndefinedSymbol(sym_name.clone()));
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
                        return Err(LinkError::UnsupportedRelocation(format!(
                            "patch offset {} out of bounds",
                            patch_offset
                        )));
                    }
                    merged_code[patch_offset..patch_offset + 4]
                        .copy_from_slice(&(value as i32).to_le_bytes());
                }
                RelocationType::Abs64 | RelocationType::Aarch64Abs64 => {
                    let value = (target_addr as i64 + addend) as u64;
                    if patch_offset + 8 > merged_code.len() {
                        return Err(LinkError::UnsupportedRelocation(format!(
                            "patch offset {} out of bounds",
                            patch_offset
                        )));
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
                        return Err(LinkError::UnsupportedRelocation(format!(
                            "patch offset {} out of bounds",
                            patch_offset
                        )));
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
                        return Err(LinkError::UnsupportedRelocation(format!(
                            "patch offset {} out of bounds",
                            patch_offset
                        )));
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
                        return Err(LinkError::UnsupportedRelocation(format!(
                            "patch offset {} out of bounds",
                            patch_offset
                        )));
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
                    // target_addr must be page-aligned, PC is the instruction address
                    // Result is the page containing target minus page containing PC
                    let target_page = target_addr & !0xFFF;
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
                        return Err(LinkError::UnsupportedRelocation(format!(
                            "patch offset {} out of bounds",
                            patch_offset
                        )));
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
                    // target_addr's low 12 bits are the offset within the page
                    let page_offset = (target_addr & 0xFFF) as u32;
                    if patch_offset + 4 > merged_code.len() {
                        return Err(LinkError::UnsupportedRelocation(format!(
                            "patch offset {} out of bounds",
                            patch_offset
                        )));
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

        // Build the ELF - single segment with code + rodata
        let file_offset = HEADER_SIZE;
        let total_size = merged_code.len() + merged_rodata.len();

        let mut elf = Vec::with_capacity((HEADER_SIZE as usize) + total_size);

        // ===== ELF Header (64 bytes) =====
        elf.extend_from_slice(&[
            0x7F, b'E', b'L', b'F', // Magic
            2,    // 64-bit
            1,    // Little endian
            1,    // ELF version
            0,    // System V ABI
            0, 0, 0, 0, 0, 0, 0, 0, // Padding
        ]);
        elf.extend_from_slice(&2_u16.to_le_bytes()); // e_type: ET_EXEC
        elf.extend_from_slice(&self.target.elf_machine().to_le_bytes()); // e_machine
        elf.extend_from_slice(&1_u32.to_le_bytes()); // e_version
        elf.extend_from_slice(&entry_addr.to_le_bytes()); // e_entry
        elf.extend_from_slice(&ELF_HEADER_SIZE.to_le_bytes()); // e_phoff
        elf.extend_from_slice(&0_u64.to_le_bytes()); // e_shoff (no sections)
        elf.extend_from_slice(&0_u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&(NUM_PROGRAM_HEADERS as u16).to_le_bytes()); // e_phnum
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shnum
        elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shstrndx

        // ===== Single Program Header (PT_LOAD, R+W+X) =====
        elf.extend_from_slice(&1_u32.to_le_bytes()); // p_type: PT_LOAD
        elf.extend_from_slice(&0x7_u32.to_le_bytes()); // p_flags: PF_R | PF_W | PF_X
        elf.extend_from_slice(&file_offset.to_le_bytes()); // p_offset
        elf.extend_from_slice(&code_vaddr.to_le_bytes()); // p_vaddr
        elf.extend_from_slice(&code_vaddr.to_le_bytes()); // p_paddr
        elf.extend_from_slice(&(total_size as u64).to_le_bytes()); // p_filesz
        elf.extend_from_slice(&(total_size as u64).to_le_bytes()); // p_memsz
        elf.extend_from_slice(&self.page_size.to_le_bytes()); // p_align

        // Write code
        elf.extend_from_slice(&merged_code);

        // Write rodata (immediately follows code)
        elf.extend_from_slice(&merged_rodata);

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
        assert_eq!(&elf[0..4], b"\x7FELF");
        // Check it's an executable
        assert_eq!(elf[16], 2); // ET_EXEC
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
        assert_eq!(&elf[0..4], b"\x7FELF");
        assert_eq!(elf[16], 2); // ET_EXEC
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
        assert_eq!(&elf[0..4], b"\x7FELF"); // Magic
        assert_eq!(elf[4], 2); // 64-bit
        assert_eq!(elf[5], 1); // Little endian
        assert_eq!(elf[6], 1); // ELF version
        assert_eq!(elf[16], 2); // ET_EXEC
        assert_eq!(u16::from_le_bytes([elf[18], elf[19]]), 0x3E); // x86-64

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

        // Program header starts at offset 64 (after ELF header)
        let ph_offset = 64;

        // p_type = PT_LOAD (1)
        let p_type = u32::from_le_bytes(elf[ph_offset..ph_offset + 4].try_into().unwrap());
        assert_eq!(p_type, 1);

        // p_flags = PF_R | PF_W | PF_X (7)
        let p_flags = u32::from_le_bytes(elf[ph_offset + 4..ph_offset + 8].try_into().unwrap());
        assert_eq!(p_flags, 7);
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
        assert_eq!(&elf[0..4], b"\x7FELF", "should have ELF magic");
        assert_eq!(elf[16], 2, "should be ET_EXEC");

        // Verify entry point is reasonable
        let entry = u64::from_le_bytes(elf[24..32].try_into().unwrap());
        assert!(entry >= 0x400000, "entry should be at/above base addr");
        assert!(entry < 0x500000, "entry should be reasonable");

        // Verify we have actual code after headers (offset 120 = 64 + 56)
        assert!(elf.len() > 120, "should have content after headers");
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
}
