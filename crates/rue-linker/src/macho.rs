//! Mach-O executable builder.
//!
//! This module provides type-safe abstractions for building Mach-O executables.
//! Each load command type is represented as a struct that knows how to serialize itself.
//!
//! # Architecture
//!
//! A Mach-O executable consists of:
//! 1. **Header** - Magic, CPU type, file type, number of load commands
//! 2. **Load Commands** - Variable-length commands describing segments, entry point, etc.
//! 3. **Segment Data** - The actual code, data, and metadata
//!
//! # Usage
//!
//! ```ignore
//! let mut builder = MachOBuilder::new();
//! builder.add_segment(pagezero);
//! builder.add_segment(text_segment);
//! builder.add_load_command(LoadDylinker::new("/usr/lib/dyld"));
//! builder.add_load_command(LoadDylib::new("/usr/lib/libSystem.B.dylib"));
//! builder.add_load_command(EntryPoint::new(entry_offset));
//! let bytes = builder.build()?;
//! ```

use crate::constants::*;

/// ARM64 macOS page size (16KB).
pub const PAGE_SIZE: u64 = 0x4000;

/// Default VM base address for executables.
pub const VM_BASE: u64 = 0x100000000;

// =============================================================================
// Load Command Trait
// =============================================================================

/// Trait for Mach-O load commands.
///
/// Each load command type implements this trait to provide its command ID,
/// size, and serialization logic.
pub trait LoadCommand {
    /// The load command type (e.g., LC_SEGMENT_64, LC_MAIN).
    fn cmd(&self) -> u32;

    /// Total size of this load command in bytes (must be 8-byte aligned).
    fn cmdsize(&self) -> u32;

    /// Write the load command to the buffer.
    fn write(&self, buf: &mut Vec<u8>);
}

// =============================================================================
// Segment Commands
// =============================================================================

/// A section within a segment.
#[derive(Debug, Clone)]
pub struct Section64 {
    /// Section name (e.g., "__text").
    pub sectname: [u8; 16],
    /// Segment name (e.g., "__TEXT").
    pub segname: [u8; 16],
    /// Virtual memory address.
    pub addr: u64,
    /// Size in bytes.
    pub size: u64,
    /// File offset.
    pub offset: u32,
    /// Alignment as power of 2.
    pub align: u32,
    /// File offset of relocations.
    pub reloff: u32,
    /// Number of relocations.
    pub nreloc: u32,
    /// Section flags.
    pub flags: u32,
    /// Reserved.
    pub reserved1: u32,
    /// Reserved.
    pub reserved2: u32,
    /// Reserved (64-bit).
    pub reserved3: u32,
}

impl Section64 {
    /// Create a new section with the given name.
    pub fn new(sectname: &str, segname: &str) -> Self {
        let mut sect = [0u8; 16];
        let mut seg = [0u8; 16];
        let sectname_bytes = sectname.as_bytes();
        let segname_bytes = segname.as_bytes();
        sect[..sectname_bytes.len().min(16)]
            .copy_from_slice(&sectname_bytes[..sectname_bytes.len().min(16)]);
        seg[..segname_bytes.len().min(16)]
            .copy_from_slice(&segname_bytes[..segname_bytes.len().min(16)]);
        Self {
            sectname: sect,
            segname: seg,
            addr: 0,
            size: 0,
            offset: 0,
            align: 0,
            reloff: 0,
            nreloc: 0,
            flags: 0,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
        }
    }

    /// Set section as containing pure instructions.
    pub fn with_code_flags(mut self) -> Self {
        self.flags = S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS;
        self
    }

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.sectname);
        buf.extend_from_slice(&self.segname);
        buf.extend_from_slice(&self.addr.to_le_bytes());
        buf.extend_from_slice(&self.size.to_le_bytes());
        buf.extend_from_slice(&self.offset.to_le_bytes());
        buf.extend_from_slice(&self.align.to_le_bytes());
        buf.extend_from_slice(&self.reloff.to_le_bytes());
        buf.extend_from_slice(&self.nreloc.to_le_bytes());
        buf.extend_from_slice(&self.flags.to_le_bytes());
        buf.extend_from_slice(&self.reserved1.to_le_bytes());
        buf.extend_from_slice(&self.reserved2.to_le_bytes());
        buf.extend_from_slice(&self.reserved3.to_le_bytes());
    }
}

/// LC_SEGMENT_64: A 64-bit segment load command.
#[derive(Debug, Clone)]
pub struct Segment64 {
    /// Segment name (e.g., "__TEXT").
    pub segname: [u8; 16],
    /// Virtual memory address.
    pub vmaddr: u64,
    /// Virtual memory size.
    pub vmsize: u64,
    /// File offset.
    pub fileoff: u64,
    /// File size.
    pub filesize: u64,
    /// Maximum VM protection.
    pub maxprot: u32,
    /// Initial VM protection.
    pub initprot: u32,
    /// Segment flags.
    pub flags: u32,
    /// Sections in this segment.
    pub sections: Vec<Section64>,
}

impl Segment64 {
    /// Create a new segment with the given name.
    pub fn new(name: &str) -> Self {
        let mut segname = [0u8; 16];
        let name_bytes = name.as_bytes();
        segname[..name_bytes.len().min(16)]
            .copy_from_slice(&name_bytes[..name_bytes.len().min(16)]);
        Self {
            segname,
            vmaddr: 0,
            vmsize: 0,
            fileoff: 0,
            filesize: 0,
            maxprot: 0,
            initprot: 0,
            flags: 0,
            sections: Vec::new(),
        }
    }

    /// Create a __PAGEZERO segment.
    pub fn pagezero() -> Self {
        let mut seg = Self::new("__PAGEZERO");
        seg.vmaddr = 0;
        seg.vmsize = VM_BASE; // 4GB null guard page
        seg.fileoff = 0;
        seg.filesize = 0;
        seg.maxprot = 0;
        seg.initprot = 0;
        seg
    }

    /// Set protection flags.
    pub fn with_protection(mut self, prot: u32) -> Self {
        self.maxprot = prot;
        self.initprot = prot;
        self
    }

    /// Add a section to this segment.
    pub fn add_section(&mut self, section: Section64) {
        self.sections.push(section);
    }
}

impl LoadCommand for Segment64 {
    fn cmd(&self) -> u32 {
        LC_SEGMENT_64
    }

    fn cmdsize(&self) -> u32 {
        (MACHO64_SEGMENT_CMD_SIZE + MACHO64_SECTION_SIZE * self.sections.len()) as u32
    }

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.cmd().to_le_bytes());
        buf.extend_from_slice(&self.cmdsize().to_le_bytes());
        buf.extend_from_slice(&self.segname);
        buf.extend_from_slice(&self.vmaddr.to_le_bytes());
        buf.extend_from_slice(&self.vmsize.to_le_bytes());
        buf.extend_from_slice(&self.fileoff.to_le_bytes());
        buf.extend_from_slice(&self.filesize.to_le_bytes());
        buf.extend_from_slice(&self.maxprot.to_le_bytes());
        buf.extend_from_slice(&self.initprot.to_le_bytes());
        buf.extend_from_slice(&(self.sections.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.flags.to_le_bytes());

        for section in &self.sections {
            section.write(buf);
        }
    }
}

// =============================================================================
// Dynamic Linking Commands
// =============================================================================

/// LC_LOAD_DYLINKER: Specifies the dynamic linker path.
#[derive(Debug, Clone)]
pub struct LoadDylinker {
    /// Path to the dynamic linker (typically "/usr/lib/dyld").
    pub path: String,
}

impl LoadDylinker {
    /// Create a load dylinker command with the default path.
    pub fn new() -> Self {
        Self {
            path: "/usr/lib/dyld".to_string(),
        }
    }

    /// Create with a custom path.
    pub fn with_path(path: &str) -> Self {
        Self {
            path: path.to_string(),
        }
    }
}

impl Default for LoadDylinker {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadCommand for LoadDylinker {
    fn cmd(&self) -> u32 {
        LC_LOAD_DYLINKER
    }

    fn cmdsize(&self) -> u32 {
        // dylinker_command: cmd (4) + cmdsize (4) + name offset (4) + string + padding
        let base_size = 12;
        let string_size = self.path.len() + 1; // null terminated
        let total = base_size + string_size;
        // Round up to 8-byte alignment
        ((total + 7) / 8 * 8) as u32
    }

    fn write(&self, buf: &mut Vec<u8>) {
        let cmdsize = self.cmdsize();
        buf.extend_from_slice(&self.cmd().to_le_bytes());
        buf.extend_from_slice(&cmdsize.to_le_bytes());
        buf.extend_from_slice(&12u32.to_le_bytes()); // name offset (after cmd, cmdsize, offset)
        buf.extend_from_slice(self.path.as_bytes());
        buf.push(0); // null terminator

        // Pad to 8-byte alignment
        let written = 12 + self.path.len() + 1;
        let padding = cmdsize as usize - written;
        for _ in 0..padding {
            buf.push(0);
        }
    }
}

/// LC_LOAD_DYLIB: Load a dynamic library.
#[derive(Debug, Clone)]
pub struct LoadDylib {
    /// Path to the dynamic library.
    pub path: String,
    /// Library timestamp.
    pub timestamp: u32,
    /// Current version (encoded).
    pub current_version: u32,
    /// Compatibility version (encoded).
    pub compat_version: u32,
}

impl LoadDylib {
    /// Create a load dylib command for libSystem.
    pub fn libsystem() -> Self {
        Self {
            path: "/usr/lib/libSystem.B.dylib".to_string(),
            timestamp: 2,
            current_version: 0x05130000, // 1315.0.0
            compat_version: 0x00010000,  // 1.0.0
        }
    }

    /// Create with a custom path.
    pub fn with_path(path: &str) -> Self {
        Self {
            path: path.to_string(),
            timestamp: 2,
            current_version: 0x00010000,
            compat_version: 0x00010000,
        }
    }
}

impl LoadCommand for LoadDylib {
    fn cmd(&self) -> u32 {
        LC_LOAD_DYLIB
    }

    fn cmdsize(&self) -> u32 {
        // dylib_command: cmd (4) + cmdsize (4) + dylib struct (16) + string + padding
        // dylib struct: name offset (4) + timestamp (4) + current_version (4) + compat_version (4)
        let base_size = 24;
        let string_size = self.path.len() + 1;
        let total = base_size + string_size;
        ((total + 7) / 8 * 8) as u32
    }

    fn write(&self, buf: &mut Vec<u8>) {
        let cmdsize = self.cmdsize();
        buf.extend_from_slice(&self.cmd().to_le_bytes());
        buf.extend_from_slice(&cmdsize.to_le_bytes());
        buf.extend_from_slice(&24u32.to_le_bytes()); // name offset
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        buf.extend_from_slice(&self.current_version.to_le_bytes());
        buf.extend_from_slice(&self.compat_version.to_le_bytes());
        buf.extend_from_slice(self.path.as_bytes());
        buf.push(0);

        let written = 24 + self.path.len() + 1;
        let padding = cmdsize as usize - written;
        for _ in 0..padding {
            buf.push(0);
        }
    }
}

// =============================================================================
// Entry Point Commands
// =============================================================================

/// LC_MAIN: Entry point for dynamic executables.
#[derive(Debug, Clone)]
pub struct EntryPoint {
    /// File offset of entry point (relative to __TEXT segment).
    pub entryoff: u64,
    /// Initial stack size (0 for default).
    pub stacksize: u64,
}

impl EntryPoint {
    /// Create an entry point command.
    pub fn new(entryoff: u64) -> Self {
        Self {
            entryoff,
            stacksize: 0,
        }
    }
}

impl LoadCommand for EntryPoint {
    fn cmd(&self) -> u32 {
        LC_MAIN
    }

    fn cmdsize(&self) -> u32 {
        MACHO64_ENTRY_POINT_CMD_SIZE as u32
    }

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.cmd().to_le_bytes());
        buf.extend_from_slice(&self.cmdsize().to_le_bytes());
        buf.extend_from_slice(&self.entryoff.to_le_bytes());
        buf.extend_from_slice(&self.stacksize.to_le_bytes());
    }
}

/// LC_UNIXTHREAD: Thread state for static executables.
#[derive(Debug, Clone)]
pub struct UnixThread {
    /// Entry point virtual address.
    pub entry_addr: u64,
}

impl UnixThread {
    /// Create a unix thread command with the given entry address.
    pub fn new(entry_addr: u64) -> Self {
        Self { entry_addr }
    }
}

impl LoadCommand for UnixThread {
    fn cmd(&self) -> u32 {
        LC_UNIXTHREAD
    }

    fn cmdsize(&self) -> u32 {
        MACHO64_UNIXTHREAD_ARM64_SIZE as u32
    }

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.cmd().to_le_bytes());
        buf.extend_from_slice(&self.cmdsize().to_le_bytes());
        buf.extend_from_slice(&ARM_THREAD_STATE64.to_le_bytes());
        buf.extend_from_slice(&ARM_THREAD_STATE64_COUNT.to_le_bytes());

        // ARM_THREAD_STATE64: x0-x28 (29 registers)
        for _ in 0..29 {
            buf.extend_from_slice(&0u64.to_le_bytes());
        }
        // fp (x29)
        buf.extend_from_slice(&0u64.to_le_bytes());
        // lr (x30)
        buf.extend_from_slice(&0u64.to_le_bytes());
        // sp
        buf.extend_from_slice(&0u64.to_le_bytes());
        // pc - entry point
        buf.extend_from_slice(&self.entry_addr.to_le_bytes());
        // cpsr
        buf.extend_from_slice(&0u32.to_le_bytes());
        // pad
        buf.extend_from_slice(&0u32.to_le_bytes());
    }
}

// =============================================================================
// Symbol Table Commands
// =============================================================================

/// LC_SYMTAB: Symbol table.
#[derive(Debug, Clone, Default)]
pub struct Symtab {
    /// File offset of symbol table.
    pub symoff: u32,
    /// Number of symbols.
    pub nsyms: u32,
    /// File offset of string table.
    pub stroff: u32,
    /// Size of string table.
    pub strsize: u32,
}

impl Symtab {
    /// Create an empty symtab command.
    pub fn new() -> Self {
        Self::default()
    }
}

impl LoadCommand for Symtab {
    fn cmd(&self) -> u32 {
        LC_SYMTAB
    }

    fn cmdsize(&self) -> u32 {
        MACHO64_SYMTAB_CMD_SIZE as u32
    }

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.cmd().to_le_bytes());
        buf.extend_from_slice(&self.cmdsize().to_le_bytes());
        buf.extend_from_slice(&self.symoff.to_le_bytes());
        buf.extend_from_slice(&self.nsyms.to_le_bytes());
        buf.extend_from_slice(&self.stroff.to_le_bytes());
        buf.extend_from_slice(&self.strsize.to_le_bytes());
    }
}

/// LC_DYSYMTAB: Dynamic symbol table info.
#[derive(Debug, Clone, Default)]
pub struct Dysymtab {
    pub ilocalsym: u32,
    pub nlocalsym: u32,
    pub iextdefsym: u32,
    pub nextdefsym: u32,
    pub iundefsym: u32,
    pub nundefsym: u32,
    pub tocoff: u32,
    pub ntoc: u32,
    pub modtaboff: u32,
    pub nmodtab: u32,
    pub extrefsymoff: u32,
    pub nextrefsyms: u32,
    pub indirectsymoff: u32,
    pub nindirectsyms: u32,
    pub extreloff: u32,
    pub nextrel: u32,
    pub locreloff: u32,
    pub nlocrel: u32,
}

impl Dysymtab {
    /// Create an empty dysymtab command.
    pub fn new() -> Self {
        Self::default()
    }
}

impl LoadCommand for Dysymtab {
    fn cmd(&self) -> u32 {
        LC_DYSYMTAB
    }

    fn cmdsize(&self) -> u32 {
        MACHO64_DYSYMTAB_CMD_SIZE as u32
    }

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.cmd().to_le_bytes());
        buf.extend_from_slice(&self.cmdsize().to_le_bytes());
        buf.extend_from_slice(&self.ilocalsym.to_le_bytes());
        buf.extend_from_slice(&self.nlocalsym.to_le_bytes());
        buf.extend_from_slice(&self.iextdefsym.to_le_bytes());
        buf.extend_from_slice(&self.nextdefsym.to_le_bytes());
        buf.extend_from_slice(&self.iundefsym.to_le_bytes());
        buf.extend_from_slice(&self.nundefsym.to_le_bytes());
        buf.extend_from_slice(&self.tocoff.to_le_bytes());
        buf.extend_from_slice(&self.ntoc.to_le_bytes());
        buf.extend_from_slice(&self.modtaboff.to_le_bytes());
        buf.extend_from_slice(&self.nmodtab.to_le_bytes());
        buf.extend_from_slice(&self.extrefsymoff.to_le_bytes());
        buf.extend_from_slice(&self.nextrefsyms.to_le_bytes());
        buf.extend_from_slice(&self.indirectsymoff.to_le_bytes());
        buf.extend_from_slice(&self.nindirectsyms.to_le_bytes());
        buf.extend_from_slice(&self.extreloff.to_le_bytes());
        buf.extend_from_slice(&self.nextrel.to_le_bytes());
        buf.extend_from_slice(&self.locreloff.to_le_bytes());
        buf.extend_from_slice(&self.nlocrel.to_le_bytes());
    }
}

// =============================================================================
// Mach-O Builder
// =============================================================================

/// Builder for Mach-O executables.
///
/// Accumulates segments and load commands, then serializes to bytes.
pub struct MachOBuilder {
    /// Segments (also load commands).
    segments: Vec<Segment64>,
    /// Other load commands (dylinker, dylib, entry point, etc.).
    commands: Vec<Box<dyn LoadCommand>>,
    /// Header flags.
    flags: u32,
    /// Code to include in __TEXT segment.
    code: Vec<u8>,
    /// Entry point offset within code.
    entry_offset: u64,
}

impl MachOBuilder {
    /// Create a new Mach-O builder.
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            commands: Vec::new(),
            flags: 0,
            code: Vec::new(),
            entry_offset: 0,
        }
    }

    /// Set header flags.
    pub fn with_flags(mut self, flags: u32) -> Self {
        self.flags = flags;
        self
    }

    /// Set the code and entry point.
    pub fn with_code(mut self, code: Vec<u8>, entry_offset: u64) -> Self {
        self.code = code;
        self.entry_offset = entry_offset;
        self
    }

    /// Add a segment.
    pub fn add_segment(&mut self, segment: Segment64) {
        self.segments.push(segment);
    }

    /// Add a load command.
    pub fn add_command<C: LoadCommand + 'static>(&mut self, cmd: C) {
        self.commands.push(Box::new(cmd));
    }

    /// Build a static executable (using LC_UNIXTHREAD).
    ///
    /// Note: Static executables don't run on macOS ARM64 due to security restrictions.
    pub fn build_static(mut self) -> Vec<u8> {
        // Calculate sizes
        let load_commands_size: usize = self
            .segments
            .iter()
            .map(|s| s.cmdsize() as usize)
            .sum::<usize>()
            + self
                .commands
                .iter()
                .map(|c| c.cmdsize() as usize)
                .sum::<usize>();

        let header_size = MACHO64_HEADER_SIZE + load_commands_size;
        let text_file_offset = align_up(header_size as u64, 16) as usize;

        // Calculate entry point address
        let text_vm_addr = VM_BASE + text_file_offset as u64;
        let entry_addr = text_vm_addr + self.entry_offset;

        // Update __TEXT segment with actual values
        for seg in &mut self.segments {
            if &seg.segname[..6] == b"__TEXT" {
                let segment_size = text_file_offset as u64 + self.code.len() as u64;
                seg.vmaddr = VM_BASE;
                seg.vmsize = segment_size;
                seg.fileoff = 0;
                seg.filesize = segment_size;

                // Update section
                for sect in &mut seg.sections {
                    if &sect.sectname[..6] == b"__text" {
                        sect.addr = text_vm_addr;
                        sect.size = self.code.len() as u64;
                        sect.offset = text_file_offset as u32;
                    }
                }
            }
        }

        // Add LC_UNIXTHREAD with entry address
        self.add_command(UnixThread::new(entry_addr));

        // Recalculate load commands size after adding UNIXTHREAD
        let load_commands_size: usize = self
            .segments
            .iter()
            .map(|s| s.cmdsize() as usize)
            .sum::<usize>()
            + self
                .commands
                .iter()
                .map(|c| c.cmdsize() as usize)
                .sum::<usize>();
        let num_commands = self.segments.len() + self.commands.len();

        // Build the file
        let mut buf = Vec::new();

        // Header
        buf.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
        buf.extend_from_slice(&CPU_TYPE_ARM64.to_le_bytes());
        buf.extend_from_slice(&CPU_SUBTYPE_ARM64_ALL.to_le_bytes());
        buf.extend_from_slice(&MH_EXECUTE.to_le_bytes());
        buf.extend_from_slice(&(num_commands as u32).to_le_bytes());
        buf.extend_from_slice(&(load_commands_size as u32).to_le_bytes());
        buf.extend_from_slice(&self.flags.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // reserved

        // Load commands - segments first
        for seg in &self.segments {
            seg.write(&mut buf);
        }

        // Other commands
        for cmd in &self.commands {
            cmd.write(&mut buf);
        }

        // Pad to text offset
        while buf.len() < text_file_offset {
            buf.push(0);
        }

        // Code
        buf.extend_from_slice(&self.code);

        buf
    }

    /// Build a dynamic executable (using LC_MAIN + dyld).
    ///
    /// This produces an executable that can run on macOS after ad-hoc code signing.
    /// The binary includes padding for codesign to add LC_CODE_SIGNATURE.
    pub fn build_dynamic(mut self) -> Vec<u8> {
        // For dynamic executables, we need:
        // 1. LC_LOAD_DYLINKER - loads /usr/lib/dyld
        // 2. LC_LOAD_DYLIB - loads libSystem
        // 3. LC_MAIN - entry point (offset from __TEXT start)
        // 4. LC_SYMTAB + LC_DYSYMTAB - symbol tables
        // 5. __LINKEDIT segment for code signature

        // Add __LINKEDIT segment if not present
        let has_linkedit = self
            .segments
            .iter()
            .any(|s| &s.segname[..10] == b"__LINKEDIT");
        if !has_linkedit {
            let linkedit = Segment64::new("__LINKEDIT").with_protection(VM_PROT_READ);
            self.segments.push(linkedit);
        }

        // Add dyld and libSystem
        self.add_command(LoadDylinker::new());
        self.add_command(LoadDylib::libsystem());

        // Calculate load commands size (before adding remaining commands)
        let initial_cmd_size: usize = self
            .segments
            .iter()
            .map(|s| s.cmdsize() as usize)
            .sum::<usize>()
            + self
                .commands
                .iter()
                .map(|c| c.cmdsize() as usize)
                .sum::<usize>();

        // We'll add symtab, dysymtab, and entry point
        let remaining_cmd_size =
            MACHO64_SYMTAB_CMD_SIZE + MACHO64_DYSYMTAB_CMD_SIZE + MACHO64_ENTRY_POINT_CMD_SIZE;

        let load_commands_size = initial_cmd_size + remaining_cmd_size;
        let header_size = MACHO64_HEADER_SIZE + load_commands_size;

        // Leave extra padding for codesign to add LC_CODE_SIGNATURE
        let codesign_padding = 256;
        let text_file_offset = align_up((header_size + codesign_padding) as u64, 16) as usize;

        // __TEXT segment spans from file offset 0 to the first page boundary after code
        let text_segment_file_size = PAGE_SIZE as usize;

        // __LINKEDIT comes after __TEXT
        let linkedit_file_offset = text_segment_file_size;
        let linkedit_vm_addr = VM_BASE + linkedit_file_offset as u64;
        // Minimal LINKEDIT content: just a string table with null byte
        let linkedit_file_size = 16usize;
        let linkedit_vm_size = PAGE_SIZE;

        // Symbol table points into LINKEDIT
        let symtab_off = linkedit_file_offset as u32;
        let strtab_off = symtab_off;
        let strtab_size = 1u32; // Single null byte

        // LC_MAIN uses offset from start of __TEXT segment (file offset 0)
        let entryoff = text_file_offset as u64 + self.entry_offset;

        // Update segments with actual values
        for seg in &mut self.segments {
            if &seg.segname[..6] == b"__TEXT" {
                seg.vmaddr = VM_BASE;
                seg.vmsize = text_segment_file_size as u64;
                seg.fileoff = 0;
                seg.filesize = text_segment_file_size as u64;

                for sect in &mut seg.sections {
                    if &sect.sectname[..6] == b"__text" {
                        sect.addr = VM_BASE + text_file_offset as u64;
                        sect.size = self.code.len() as u64;
                        sect.offset = text_file_offset as u32;
                    }
                }
            } else if &seg.segname[..10] == b"__LINKEDIT" {
                seg.vmaddr = linkedit_vm_addr;
                seg.vmsize = linkedit_vm_size;
                seg.fileoff = linkedit_file_offset as u64;
                seg.filesize = linkedit_file_size as u64;
            }
        }

        // Add entry point and symbol tables
        self.add_command(EntryPoint::new(entryoff));

        let symtab = Symtab {
            symoff: symtab_off,
            nsyms: 0,
            stroff: strtab_off,
            strsize: strtab_size,
        };
        self.add_command(symtab);
        self.add_command(Dysymtab::new());

        // Final counts
        let load_commands_size: usize = self
            .segments
            .iter()
            .map(|s| s.cmdsize() as usize)
            .sum::<usize>()
            + self
                .commands
                .iter()
                .map(|c| c.cmdsize() as usize)
                .sum::<usize>();
        let num_commands = self.segments.len() + self.commands.len();

        // Flags for dynamic executable
        let flags = self.flags | MH_DYLDLINK | MH_NOUNDEFS | MH_TWOLEVEL | MH_PIE;

        // Build the file
        let mut buf = Vec::new();

        // Header
        buf.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
        buf.extend_from_slice(&CPU_TYPE_ARM64.to_le_bytes());
        buf.extend_from_slice(&CPU_SUBTYPE_ARM64_ALL.to_le_bytes());
        buf.extend_from_slice(&MH_EXECUTE.to_le_bytes());
        buf.extend_from_slice(&(num_commands as u32).to_le_bytes());
        buf.extend_from_slice(&(load_commands_size as u32).to_le_bytes());
        buf.extend_from_slice(&flags.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());

        // Load commands - segments first
        for seg in &self.segments {
            seg.write(&mut buf);
        }
        for cmd in &self.commands {
            cmd.write(&mut buf);
        }

        // Pad to text offset (includes codesign padding)
        while buf.len() < text_file_offset {
            buf.push(0);
        }

        // Code
        buf.extend_from_slice(&self.code);

        // Pad to LINKEDIT offset
        while buf.len() < linkedit_file_offset {
            buf.push(0);
        }

        // LINKEDIT content (minimal string table)
        buf.push(0); // null byte for empty string table

        // Pad LINKEDIT to declared size
        while buf.len() < linkedit_file_offset + linkedit_file_size {
            buf.push(0);
        }

        buf
    }
}

impl Default for MachOBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Align a value up to the given alignment.
fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_dylinker_size() {
        let cmd = LoadDylinker::new();
        // "/usr/lib/dyld" = 13 chars + null = 14
        // base 12 + 14 = 26, rounded up to 32 (8-byte aligned)
        assert_eq!(cmd.cmdsize(), 32);
    }

    #[test]
    fn test_load_dylib_size() {
        let cmd = LoadDylib::libsystem();
        // "/usr/lib/libSystem.B.dylib" = 26 chars + null = 27
        // base 24 + 27 = 51, rounded to 56
        assert_eq!(cmd.cmdsize(), 56);
    }

    #[test]
    fn test_segment_write() {
        let seg = Segment64::pagezero();
        let mut buf = Vec::new();
        seg.write(&mut buf);
        assert_eq!(buf.len(), MACHO64_SEGMENT_CMD_SIZE);
    }

    #[test]
    fn test_builder_static() {
        // Simple test: build a minimal executable
        let code = vec![
            0x40, 0x05, 0x80, 0x52, // mov w0, #42
            0xc0, 0x03, 0x5f, 0xd6, // ret
        ];

        let mut builder = MachOBuilder::new().with_code(code, 0);

        builder.add_segment(Segment64::pagezero());

        let mut text = Segment64::new("__TEXT").with_protection(VM_PROT_READ | VM_PROT_EXECUTE);
        let section = Section64::new("__text", "__TEXT").with_code_flags();
        text.add_section(section);
        builder.add_segment(text);

        let binary = builder.build_static();

        // Check magic
        assert_eq!(&binary[0..4], &MH_MAGIC_64.to_le_bytes());
    }

    #[test]
    fn test_builder_dynamic() {
        // Build a dynamic executable
        let code = vec![
            0x40, 0x05, 0x80, 0x52, // mov w0, #42
            0xc0, 0x03, 0x5f, 0xd6, // ret
        ];

        let mut builder = MachOBuilder::new().with_code(code, 0);

        builder.add_segment(Segment64::pagezero());

        let mut text = Segment64::new("__TEXT").with_protection(VM_PROT_READ | VM_PROT_EXECUTE);
        let section = Section64::new("__text", "__TEXT").with_code_flags();
        text.add_section(section);
        builder.add_segment(text);

        let binary = builder.build_dynamic();

        // Check magic
        assert_eq!(&binary[0..4], &MH_MAGIC_64.to_le_bytes());

        // Check we have the right flags (MH_DYLDLINK | MH_NOUNDEFS | MH_TWOLEVEL | MH_PIE)
        let flags = u32::from_le_bytes([binary[24], binary[25], binary[26], binary[27]]);
        assert!(flags & MH_DYLDLINK != 0, "Should have MH_DYLDLINK flag");
        assert!(flags & MH_PIE != 0, "Should have MH_PIE flag");

        // Check file size is reasonable (should include __TEXT and __LINKEDIT)
        assert!(
            binary.len() >= PAGE_SIZE as usize,
            "Should be at least one page"
        );
    }
}
