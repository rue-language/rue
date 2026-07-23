//! The linker - combines object files and produces an executable.

use std::collections::HashMap;

use rue_target::Target;

use crate::archive::Archive;
use crate::constants::{
    ELF_MAGIC, ELF64_EHDR_SIZE, ELF64_PHDR_SIZE, ELFCLASS64, ELFDATA2LSB, ET_EXEC, EV_CURRENT,
    PF_R, PF_W, PF_X, PT_LOAD,
};
use crate::elf::Section;
use crate::elf::{ObjectFile, RelocationType, Symbol, SymbolBinding};
use crate::util::align_up;

/// Which merged output buffer a relocation's patch site lives in.
///
/// Each pending relocation carries its home buffer and base address so
/// relocations from `.text`, `.rodata`, and `.data` patch the correct merged
/// section (RUE-131 item 1).
#[derive(Clone, Copy, Debug)]
enum PatchHome {
    /// Patch site is in the merged text buffer (base = code vaddr).
    Text,
    /// Patch site is in the merged rodata buffer (base = rodata vaddr).
    Rodata,
    /// Patch site is in the merged data buffer (base = data vaddr).
    Data,
}

/// The output-segment class a section merges into. Derived purely from the
/// section name so both the ELF (`link_elf`) and Mach-O (`link_macho`) paths
/// classify sections through the single [`classify_section`] source of truth
/// instead of re-deriving it inline (RUE-336).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SectionKind {
    /// Executable code (`.text*` / `__TEXT,__text` / `__text*`).
    Text,
    /// Read-only data (`.rodata*` / `.cstring*` / Mach-O `__const`/`__cstring`).
    Rodata,
    /// Writable initialized data (`.data*` / `__DATA,__data` / `__data*`).
    Data,
    /// Zero-initialized data (`.bss*` / `__DATA,__bss` / `__bss*`).
    Bss,
    /// Anything else (debug info, notes, etc.) — not merged into a segment.
    Other,
}

/// Classify a section by name, accepting both ELF (`.text*`, `.rodata*`, …) and
/// Mach-O (`__TEXT,__text`, `__DATA,__const`, …) conventions. This is the sole
/// section-classification authority; every merge/base-address decision routes
/// through it so the classes cannot drift out of sync (RUE-336).
///
/// The prefix sets are mutually exclusive, so the check order is immaterial.
fn classify_section(name: &str) -> SectionKind {
    if name.starts_with(".text") || name == "__TEXT,__text" || name.starts_with("__text") {
        SectionKind::Text
    } else if name.starts_with(".rodata")
        || name == "__DATA,__const"
        || name == "__TEXT,__const"
        || name == "__TEXT,__cstring"
        || name == "__TEXT,__rodata"
        || name.starts_with("__const")
        || name.starts_with(".cstring")
    {
        SectionKind::Rodata
    } else if name.starts_with(".data") || name == "__DATA,__data" || name.starts_with("__data") {
        SectionKind::Data
    } else if name.starts_with(".bss") || name == "__DATA,__bss" || name.starts_with("__bss") {
        SectionKind::Bss
    } else {
        SectionKind::Other
    }
}

/// Is this a code section? Accepts both ELF (`.text*`) and Mach-O
/// (`__TEXT,__text` / `__text*`) names.
fn is_text_section(name: &str) -> bool {
    classify_section(name) == SectionKind::Text
}

/// Is this a read-only data section? Accepts both ELF and Mach-O names.
fn is_rodata_section(name: &str) -> bool {
    classify_section(name) == SectionKind::Rodata
}

/// Is this a writable data section? Accepts both ELF and Mach-O names.
fn is_data_section(name: &str) -> bool {
    classify_section(name) == SectionKind::Data
}

/// Is this a zero-initialized (bss) section? Accepts both ELF and Mach-O names.
fn is_bss_section(name: &str) -> bool {
    classify_section(name) == SectionKind::Bss
}

/// Number of bytes written by a supported relocation.
///
/// Keeping this next to the shared patcher gives collection and application
/// one width authority. That lets the collector reject a malformed patch site
/// while it still knows the source section's exact bounds.
fn relocation_patch_size(rel_type: RelocationType) -> Result<usize, LinkError> {
    match rel_type {
        RelocationType::Abs64 | RelocationType::Aarch64Abs64 => Ok(8),
        RelocationType::Pc32
        | RelocationType::Plt32
        | RelocationType::GotPcRel
        | RelocationType::RexGotPcRelX
        | RelocationType::GotPcRelX
        | RelocationType::Abs32
        | RelocationType::Abs32S
        | RelocationType::Jump26
        | RelocationType::Call26
        | RelocationType::AdrpPage21
        | RelocationType::AddLo12
        | RelocationType::Ldst8Lo12
        | RelocationType::Ldst16Lo12
        | RelocationType::Ldst32Lo12
        | RelocationType::Ldst64Lo12
        | RelocationType::Ldst128Lo12 => Ok(4),
        RelocationType::Unknown(t) => Err(LinkError::UnsupportedRelocation(format!(
            "unknown type {t}"
        ))),
    }
}

/// Return an overflow-safe native range for a relocation patch site.
fn checked_patch_range(
    patch_offset: u64,
    patch_size: usize,
    section_size: usize,
    rel_type: RelocationType,
) -> Result<std::ops::Range<usize>, LinkError> {
    let patch_size_u64 = patch_size as u64;
    let section_size_u64 = section_size as u64;
    let patch_end = patch_offset.checked_add(patch_size_u64).ok_or_else(|| {
        LinkError::RelocationPatchOutOfBounds {
            patch_offset,
            patch_size,
            section_size,
            rel_type: format!("{rel_type:?}"),
        }
    })?;
    if patch_end > section_size_u64 {
        return Err(LinkError::RelocationPatchOutOfBounds {
            patch_offset,
            patch_size,
            section_size,
            rel_type: format!("{rel_type:?}"),
        });
    }

    // Both values are bounded by a native slice length above. Keep the
    // conversions checked anyway so malformed input never reaches a panic even
    // if the supported target-width assumptions change.
    let start =
        usize::try_from(patch_offset).map_err(|_| LinkError::RelocationPatchOutOfBounds {
            patch_offset,
            patch_size,
            section_size,
            rel_type: format!("{rel_type:?}"),
        })?;
    let end = usize::try_from(patch_end).map_err(|_| LinkError::RelocationPatchOutOfBounds {
        patch_offset,
        patch_size,
        section_size,
        rel_type: format!("{rel_type:?}"),
    })?;
    Ok(start..end)
}

/// Compose a section's merged offset with a section-relative relocation.
fn checked_relocation_offset(
    base_offset: u64,
    relocation_offset: u64,
    rel_type: RelocationType,
) -> Result<u64, LinkError> {
    base_offset
        .checked_add(relocation_offset)
        .ok_or_else(|| LinkError::RelocationOffsetOverflow {
            base_offset,
            relocation_offset,
            rel_type: format!("{rel_type:?}"),
        })
}

/// Apply a single already-resolved relocation by patching `buf` in place.
///
/// Shared by both `link_elf` and `link_macho` (RUE-335): the per-kind patch
/// encoding is identical across object formats, so it lives here once. The
/// caller resolves the format-specific inputs first (which
/// merged buffer the patch site lives in, its base virtual address, and the
/// symbol's resolved address) and passes them in.
///
/// Parameters use the usual ELF relocation notation:
/// * `buf`          — merged output buffer containing the patch site.
/// * `patch_offset` — byte offset of the patch site within `buf`.
/// * `place`        — virtual address of the patch site (`P` = base vaddr + offset).
/// * `target_addr`  — resolved virtual address of the referenced symbol (`S`).
/// * `addend`       — relocation addend (`A`).
/// * `rel_type`     — relocation kind.
/// * `sym_name`     — referenced symbol name, used only for diagnostics.
fn apply_relocation(
    buf: &mut [u8],
    patch_offset: u64,
    place: u64,
    target_addr: u64,
    addend: i64,
    rel_type: RelocationType,
    sym_name: &str,
) -> Result<(), LinkError> {
    let patch_size = relocation_patch_size(rel_type)?;
    let patch_range = checked_patch_range(patch_offset, patch_size, buf.len(), rel_type)?;
    let patch_offset = patch_range.start;
    let pc = place; // `P` in the S + A - P PC-relative arms below
    match rel_type {
        RelocationType::Pc32 | RelocationType::Plt32 => {
            // S + A - P, where S is symbol address, A is addend, P is place
            let value = target_addr as i64 + addend - pc as i64;
            // Check for overflow: value must fit in i32
            if value < i32::MIN as i64 || value > i32::MAX as i64 {
                return Err(LinkError::RelocationOverflow {
                    symbol: sym_name.to_string(),
                    rel_type: format!("{:?}", rel_type),
                });
            }
            buf[patch_range.clone()].copy_from_slice(&(value as i32).to_le_bytes());
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
                if buf[opcode_offset] == 0xFF {
                    let modrm = buf[opcode_offset + 1];
                    let reg_field = (modrm >> 3) & 0x7;
                    if reg_field == 2 {
                        // Indirect call: `call *[rip+disp]` (FF /2, ModR/M 15)
                        // Transform to: `addr32 call rel32` (67 E8)
                        buf[opcode_offset] = 0x67; // addr32 prefix
                        buf[opcode_offset + 1] = 0xE8; // direct call opcode
                    } else if reg_field == 4 {
                        // Indirect jmp: `jmp *[rip+disp]` (FF /4, ModR/M 25)
                        // Transform to: `addr32 jmp rel32` (67 E9)
                        buf[opcode_offset] = 0x67; // addr32 prefix
                        buf[opcode_offset + 1] = 0xE9; // direct jmp opcode
                    }
                } else if buf[opcode_offset] == 0x8B {
                    // MOV: `mov reg, [rip+disp]` -> `lea reg, [rip+disp]`
                    buf[opcode_offset] = 0x8D; // LEA opcode
                }
                // Other patterns: just patch displacement (best effort)
            }

            let value = target_addr as i64 + addend - pc as i64;
            if value < i32::MIN as i64 || value > i32::MAX as i64 {
                return Err(LinkError::RelocationOverflow {
                    symbol: sym_name.to_string(),
                    rel_type: format!("{:?}", rel_type),
                });
            }
            buf[patch_range.clone()].copy_from_slice(&(value as i32).to_le_bytes());
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
                if buf[opcode_offset] == 0xFF {
                    let modrm = buf[opcode_offset + 1];
                    let reg_field = (modrm >> 3) & 0x7;
                    if reg_field == 2 {
                        // Indirect call: `call *[rip+disp]` (FF /2, ModR/M 15)
                        // Transform to: `addr32 call rel32` (67 E8)
                        buf[opcode_offset] = 0x67; // addr32 prefix
                        buf[opcode_offset + 1] = 0xE8; // direct call opcode
                    } else if reg_field == 4 {
                        // Indirect jmp: `jmp *[rip+disp]` (FF /4, ModR/M 25)
                        // Transform to: `addr32 jmp rel32` (67 E9)
                        buf[opcode_offset] = 0x67; // addr32 prefix
                        buf[opcode_offset + 1] = 0xE9; // direct jmp opcode
                    }
                } else if buf[opcode_offset] == 0x8B {
                    // MOV: `mov reg, [rip+disp]` -> `lea reg, [rip+disp]`
                    buf[opcode_offset] = 0x8D; // LEA opcode
                }
                // Other patterns: just patch displacement (best effort)
            }

            let value = target_addr as i64 + addend - pc as i64;
            if value < i32::MIN as i64 || value > i32::MAX as i64 {
                return Err(LinkError::RelocationOverflow {
                    symbol: sym_name.to_string(),
                    rel_type: format!("{:?}", rel_type),
                });
            }
            buf[patch_range.clone()].copy_from_slice(&(value as i32).to_le_bytes());
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
                if buf[opcode_offset] == 0xFF {
                    let modrm = buf[opcode_offset + 1];
                    let reg_field = (modrm >> 3) & 0x7;
                    if reg_field == 2 {
                        // Indirect call with REX: `REX call *[rip+disp]` (4x FF /2)
                        // Transform to: `addr32 call rel32` (67 E8) - REX stays at offset-3
                        buf[opcode_offset] = 0x67; // addr32 prefix
                        buf[opcode_offset + 1] = 0xE8; // direct call opcode
                    // Note: REX prefix at offset-3 becomes harmless (no-op for CALL)
                    } else if reg_field == 4 {
                        // Indirect jmp with REX: `REX jmp *[rip+disp]` (4x FF /4)
                        // Transform to: `addr32 jmp rel32` (67 E9)
                        buf[opcode_offset] = 0x67; // addr32 prefix
                        buf[opcode_offset + 1] = 0xE9; // direct jmp opcode
                    }
                } else if buf[opcode_offset] == 0x8B {
                    // MOV with REX: `REX mov reg, [rip+disp]` -> `REX lea reg, [rip+disp]`
                    buf[opcode_offset] = 0x8D; // LEA opcode
                }
                // Other patterns: just patch displacement (best effort)
            }

            let value = target_addr as i64 + addend - pc as i64;
            if value < i32::MIN as i64 || value > i32::MAX as i64 {
                return Err(LinkError::RelocationOverflow {
                    symbol: sym_name.to_string(),
                    rel_type: format!("{:?}", rel_type),
                });
            }
            buf[patch_range.clone()].copy_from_slice(&(value as i32).to_le_bytes());
        }
        RelocationType::Abs64 | RelocationType::Aarch64Abs64 => {
            // 64-bit absolute address (pointer slots in rodata/data).
            let value = (target_addr as i64 + addend) as u64;
            buf[patch_range.clone()].copy_from_slice(&value.to_le_bytes());
        }
        RelocationType::Abs32 => {
            let value = target_addr as i64 + addend;
            // Check for overflow: value must fit in u32
            if value < 0 || value > u32::MAX as i64 {
                return Err(LinkError::RelocationOverflow {
                    symbol: sym_name.to_string(),
                    rel_type: "Abs32".to_string(),
                });
            }
            buf[patch_range.clone()].copy_from_slice(&(value as u32).to_le_bytes());
        }
        RelocationType::Abs32S => {
            let value = target_addr as i64 + addend;
            // Check for overflow: value must fit in i32
            if value < i32::MIN as i64 || value > i32::MAX as i64 {
                return Err(LinkError::RelocationOverflow {
                    symbol: sym_name.to_string(),
                    rel_type: "Abs32S".to_string(),
                });
            }
            buf[patch_range.clone()].copy_from_slice(&(value as i32).to_le_bytes());
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
                    symbol: sym_name.to_string(),
                    rel_type: rel_name.to_string(),
                });
            }
            // Read existing instruction and patch the immediate field
            let mut inst = u32::from_le_bytes(buf[patch_range.clone()].try_into().unwrap());
            inst = (inst & 0xFC000000) | ((offset as u32) & 0x03FFFFFF);
            buf[patch_range.clone()].copy_from_slice(&inst.to_le_bytes());
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
                    symbol: sym_name.to_string(),
                    rel_type: "AdrpPage21".to_string(),
                });
            }
            // ADRP instruction format: imm is split into immlo (bits 29-30) and immhi (bits 5-23)
            let imm = page_count as u32;
            let immlo = (imm & 0x3) << 29; // bits 0-1 of imm -> bits 29-30
            let immhi = ((imm >> 2) & 0x7FFFF) << 5; // bits 2-20 of imm -> bits 5-23
            let mut inst = u32::from_le_bytes(buf[patch_range.clone()].try_into().unwrap());
            // Clear immlo and immhi fields, then set them
            inst = (inst & 0x9F00001F) | immlo | immhi;
            buf[patch_range.clone()].copy_from_slice(&inst.to_le_bytes());
        }
        RelocationType::AddLo12 => {
            // AArch64 ADD/load-store PAGEOFF12 - low 12 bits of the effective
            // address. S + A gives the effective address; extract its low 12 bits.
            let effective_addr = (target_addr as i64 + addend) as u64;
            let lo12 = (effective_addr & 0xFFF) as u32;
            let mut inst = u32::from_le_bytes(buf[patch_range.clone()].try_into().unwrap());

            // PAGEOFF12 applies to both ADD and load/store instructions. For
            // loads/stores the imm12 field is SCALED by the access size encoded
            // in the instruction — writing the raw byte offset would multiply
            // the effective displacement by the access size at runtime.
            // (RUE-131 item 5a.) Rue's codegen only ever emits this on an ADD
            // (the `else` branch), where the value is unscaled; handling the
            // load/store form keeps both linker paths correct for any input.
            let imm = if inst & 0x3B00_0000 == 0x3900_0000 {
                // Load/store register, unsigned immediate class:
                // scale = size bits [31:30]; 128-bit vector ops
                // (size == 0b00, V == 1, opc<1> == 1) scale by 16.
                let mut scale = inst >> 30;
                if scale == 0 && inst & 0x0480_0000 == 0x0480_0000 {
                    scale = 4;
                }
                if lo12 & ((1u32 << scale) - 1) != 0 {
                    return Err(LinkError::RelocationOverflow {
                        symbol: sym_name.to_string(),
                        rel_type: format!(
                            "AddLo12 (offset 0x{lo12:x} misaligned for {}-byte access)",
                            1u32 << scale
                        ),
                    });
                }
                lo12 >> scale
            } else {
                // ADD immediate: unscaled
                lo12
            };

            // ADD instruction format: imm12 is in bits 10-21
            // Clear imm12 field (bits 10-21) and set it
            inst = (inst & 0xFFC003FF) | (imm << 10);
            buf[patch_range.clone()].copy_from_slice(&inst.to_le_bytes());
        }
        RelocationType::Ldst8Lo12
        | RelocationType::Ldst16Lo12
        | RelocationType::Ldst32Lo12
        | RelocationType::Ldst64Lo12
        | RelocationType::Ldst128Lo12 => {
            // R_AARCH64_LDST{8,16,32,64,128}_ABS_LO12_NC: the scaled low 12 bits
            // of the effective address for a load/store instruction. Result is
            // S + A; the low 12 bits, scaled DOWN by the access width, land in
            // the imm12 field (instruction bits [21:10]). Unlike ADD_ABS_LO12_NC
            // the width is fixed by the relocation type rather than inferred from
            // the instruction, and the result must be aligned to the access width
            // (the `_NC` variants have no overflow check, but a misaligned offset
            // cannot be represented in the scaled imm12 and is a malformed input).
            let (scale, rel_name): (u32, &str) = match rel_type {
                RelocationType::Ldst8Lo12 => (0, "Ldst8Lo12"),
                RelocationType::Ldst16Lo12 => (1, "Ldst16Lo12"),
                RelocationType::Ldst32Lo12 => (2, "Ldst32Lo12"),
                RelocationType::Ldst64Lo12 => (3, "Ldst64Lo12"),
                RelocationType::Ldst128Lo12 => (4, "Ldst128Lo12"),
                _ => unreachable!(),
            };
            let effective_addr = (target_addr as i64 + addend) as u64;
            let lo12 = (effective_addr & 0xFFF) as u32;
            // The result must be aligned to the access width; otherwise the low
            // scale bits would be lost by the >> below and the reference would be
            // silently corrupted.
            if lo12 & ((1u32 << scale) - 1) != 0 {
                return Err(LinkError::RelocationOverflow {
                    symbol: sym_name.to_string(),
                    rel_type: format!(
                        "{rel_name} (offset 0x{lo12:x} misaligned for {}-byte access)",
                        1u32 << scale
                    ),
                });
            }
            let imm = lo12 >> scale;
            let mut inst = u32::from_le_bytes(buf[patch_range.clone()].try_into().unwrap());
            // Load/store imm12 is in bits 10-21, same field position as ADD.
            inst = (inst & 0xFFC003FF) | (imm << 10);
            buf[patch_range].copy_from_slice(&inst.to_le_bytes());
        }
        RelocationType::Unknown(t) => {
            return Err(LinkError::UnsupportedRelocation(format!(
                "unknown type {}",
                t
            )));
        }
    }
    Ok(())
}

/// A relocation waiting to be applied once all section addresses are known.
struct PendingRelocation {
    /// Patch offset within the home buffer.
    offset: u64,
    /// Name of the referenced symbol.
    sym_name: String,
    /// Section the referenced symbol is defined in (None if undefined).
    sym_section: Option<usize>,
    /// Binding of the referenced symbol. LOCAL symbols must resolve within
    /// their own object — resolving them by name across all objects lets
    /// same-named locals in different objects shadow each other.
    /// (RUE-131 item 2)
    sym_binding: SymbolBinding,
    /// Index of the object the relocation (and any local symbol) lives in.
    obj_idx: usize,
    rel_type: RelocationType,
    addend: i64,
    /// Which merged buffer the patch site lives in.
    home: PatchHome,
}

/// Collect a merged section's relocations into `pending`, validating symbol
/// indices and skipping null-symbol relocations and relocations against
/// sections we don't link (debug/unwind).
fn collect_section_relocations(
    obj: &ObjectFile,
    section: &Section,
    merged_offset: u64,
    obj_idx: usize,
    home: PatchHome,
    pending: &mut Vec<PendingRelocation>,
) -> Result<(), LinkError> {
    for reloc in &section.relocations {
        // Validate symbol index before accessing
        if reloc.symbol_index >= obj.symbols.len() {
            return Err(LinkError::InvalidSymbolIndex {
                symbol_index: reloc.symbol_index,
                symbol_count: obj.symbols.len(),
            });
        }
        let sym = &obj.symbols[reloc.symbol_index];

        // Skip relocations that reference the null symbol (empty name)
        // These are typically R_*_NONE relocations that slipped through
        if sym.name.is_empty() {
            continue;
        }

        // We link text, rodata, data, and bss; skip relocations whose
        // TARGET symbol lives in a section we don't link (e.g. debug).
        if let Some(sec_idx) = sym.section_index {
            if sec_idx < obj.sections.len() {
                let target_sec = &obj.sections[sec_idx];
                if !is_text_section(&target_sec.name)
                    && !is_rodata_section(&target_sec.name)
                    && !is_data_section(&target_sec.name)
                    && !is_bss_section(&target_sec.name)
                {
                    continue;
                }
            }
        }

        let patch_size = relocation_patch_size(reloc.rel_type)?;
        checked_patch_range(reloc.offset, patch_size, section.data.len(), reloc.rel_type)?;
        let offset = checked_relocation_offset(merged_offset, reloc.offset, reloc.rel_type)?;

        pending.push(PendingRelocation {
            offset,
            sym_name: sym.name.clone(),
            sym_section: sym.section_index,
            sym_binding: sym.binding,
            obj_idx,
            rel_type: reloc.rel_type,
            addend: reloc.addend,
            home,
        });
    }
    Ok(())
}

/// Final symbol addresses, split by binding scope.
///
/// Global (and weak) symbols are visible across all objects and keyed by
/// name. LOCAL symbols (including section symbols and compiler-generated
/// constants like `l_anon.*` / `.rodata.str*`) are only visible within their
/// defining object, so they are keyed by `(object index, name)` — a single
/// name-keyed map let same-named locals from different objects shadow each
/// other, binding every reference to whichever object happened to win.
/// (RUE-131 item 2)
#[derive(Default)]
struct SymbolAddresses {
    globals: HashMap<String, u64>,
    locals: HashMap<(usize, String), u64>,
}

impl SymbolAddresses {
    /// Resolve a relocation's symbol reference: locals within the referencing
    /// object only, globals/weaks by name.
    fn resolve(&self, obj_idx: usize, name: &str, binding: SymbolBinding) -> Option<u64> {
        match binding {
            SymbolBinding::Local => self.locals.get(&(obj_idx, name.to_string())).copied(),
            SymbolBinding::Global | SymbolBinding::Weak => self.globals.get(name).copied(),
        }
    }
}

/// Linker errors.
#[derive(Debug)]
pub enum LinkError {
    /// Undefined symbol reference.
    UndefinedSymbol(String),
    /// Duplicate symbol definition.
    DuplicateSymbol(String),
    /// Object architecture doesn't match the link target.
    ArchMismatch {
        object_machine: String,
        target: String,
    },
    /// Unsupported relocation type.
    UnsupportedRelocation(String),
    /// Relocation overflow (value doesn't fit).
    RelocationOverflow { symbol: String, rel_type: String },
    /// Relocation patch extends beyond its containing section.
    RelocationPatchOutOfBounds {
        patch_offset: u64,
        patch_size: usize,
        section_size: usize,
        rel_type: String,
    },
    /// Merging a section-relative relocation offset overflowed.
    RelocationOffsetOverflow {
        base_offset: u64,
        relocation_offset: u64,
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
    /// Feature not yet implemented.
    NotImplemented(&'static str),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::UndefinedSymbol(s) => write!(f, "undefined symbol: {}", s),
            LinkError::DuplicateSymbol(s) => write!(f, "duplicate symbol: {}", s),
            LinkError::ArchMismatch {
                object_machine,
                target,
            } => write!(
                f,
                "object architecture mismatch: object is {}, link target is {}",
                object_machine, target
            ),
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
                    "relocation patch extends beyond section: {} relocation at offset {} \
                     requires {} bytes, but section is only {} bytes",
                    rel_type, patch_offset, patch_size, section_size
                )
            }
            LinkError::RelocationOffsetOverflow {
                base_offset,
                relocation_offset,
                rel_type,
            } => {
                write!(
                    f,
                    "relocation offset overflow: {rel_type} relocation base {base_offset} \
                     plus section offset {relocation_offset} exceeds u64"
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
            LinkError::NotImplemented(feature) => {
                write!(f, "{} not yet implemented", feature)
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
        // Refuse objects compiled for a different architecture — without this
        // check an "aarch64" binary could silently embed x86 machine code
        // (confirmed during the linker review: 22 x86 syscalls linked into an
        // aarch64 output without complaint). (RUE-131 item 10, RUE-36)
        let expected = match self.target {
            Target::X86_64Linux => crate::elf::ElfMachine::X86_64,
            Target::Aarch64Linux | Target::Aarch64Macos => crate::elf::ElfMachine::Aarch64,
        };
        if obj.machine != expected {
            return Err(LinkError::ArchMismatch {
                object_machine: format!("{:?}", obj.machine),
                target: format!("{:?}", self.target),
            });
        }

        let obj_index = self.objects.len();

        // Collect global symbols
        for sym in &obj.symbols {
            if sym.section_index.is_some()
                && (sym.binding == SymbolBinding::Global || sym.binding == SymbolBinding::Weak)
                && !sym.name.is_empty()
            {
                if let Some((_, existing)) = self.global_symbols.get(&sym.name) {
                    // Two strong definitions collide; weak symbols defer.
                    if existing.binding != SymbolBinding::Weak && sym.binding != SymbolBinding::Weak
                    {
                        return Err(LinkError::DuplicateSymbol(sym.name.clone()));
                    }
                    // A strong definition overrides a weak one. Among weak
                    // definitions the first wins, as required by standard
                    // linker semantics (RUE-131 item 9).
                    if existing.binding == SymbolBinding::Weak
                        && sym.binding == SymbolBinding::Global
                    {
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
    ///
    /// # Extraction order (RUE-848)
    ///
    /// When several members define the same symbol, extraction is
    /// *first-eligible in archive member order*: the earliest member that
    /// provides a needed symbol is pulled in, matching GNU `ld` / `ld64`
    /// archive semantics. The outer fixed-point loop is the conventional
    /// *rescan* boundary — pulling one member can expose new undefined symbols,
    /// which are resolved on the next pass — while within a single symbol
    /// lookup the choice of member is a *one-pass*, order-deterministic pick.
    /// This path is object-format neutral, so it governs ELF and Mach-O archive
    /// members identically. Post-extraction weak/strong resolution among the
    /// members actually pulled in is still owned by [`Self::add_object`].
    pub fn add_archive(&mut self, archive: Archive) -> Result<(), LinkError> {
        // Convert to a Vec we can index into
        let archive_objects: Vec<ObjectFile> = archive.objects.into_iter().collect();

        // Map each symbol to every member that defines it, in archive member
        // order. A last-writer-wins `HashMap::insert` index would instead bind
        // each symbol to its *last* provider, so selection could extract a later
        // member over an earlier one and silently change weak/strong resolution
        // when a user or foreign archive ships multiple providers (RUE-848).
        // Retaining the ordered provider list keeps selection first-eligible and
        // independent of hash iteration order.
        let mut symbol_providers: HashMap<String, Vec<usize>> = HashMap::new();
        for (obj_idx, obj) in archive_objects.iter().enumerate() {
            for sym in &obj.symbols {
                if sym.section_index.is_some()
                    && (sym.binding == SymbolBinding::Global || sym.binding == SymbolBinding::Weak)
                    && !sym.name.is_empty()
                {
                    symbol_providers
                        .entry(sym.name.clone())
                        .or_default()
                        .push(obj_idx);
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
                let Some(providers) = symbol_providers.get(&sym_name) else {
                    continue;
                };
                // First-eligible extraction: pull the earliest member (in
                // archive order) that provides the symbol and is not already
                // selected. `providers` is ordered, so this pick is
                // deterministic regardless of hash iteration order.
                let Some(&obj_idx) = providers.iter().find(|&&idx| !selected[idx]) else {
                    continue;
                };
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
    ///
    /// Automatically selects the output format (ELF or Mach-O) based on
    /// the target platform.
    #[must_use = "linking returns a Result that must be checked"]
    pub fn link(self, entry_point: &str) -> Result<Vec<u8>, LinkError> {
        if self.target.is_macho() {
            self.link_macho(entry_point)
        } else {
            self.link_elf(entry_point)
        }
    }

    /// Link all objects and produce a Mach-O executable.
    ///
    /// This generates a dynamic executable using LC_MAIN that can run on macOS
    /// after ad-hoc code signing with `codesign -s - <binary>`.
    fn link_macho(self, entry_point: &str) -> Result<Vec<u8>, LinkError> {
        use crate::constants::{VM_PROT_EXECUTE, VM_PROT_READ};
        use crate::macho::{MachOBuilder, Section64, Segment64, VM_BASE};

        // Merge sections and collect relocations
        // For Mach-O, we put code AND rodata in the __TEXT segment (same as clang/ld64).
        // Only writable data goes in __DATA.
        let mut merged_text = Vec::new(); // Code + rodata (goes in __TEXT)
        let mut merged_data = Vec::new(); // Writable data (goes in __DATA)
        let mut bss_size: u64 = 0;
        let mut section_offsets: HashMap<(usize, usize), u64> = HashMap::new();
        let mut pending_relocations: Vec<PendingRelocation> = Vec::new();

        // Merge code sections (.text* / __text)
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !is_text_section(&section.name) || section.data.is_empty() {
                    continue;
                }

                // Align to section alignment (minimum 4 for ARM64 instructions)
                let align = section.align.max(4);
                let current_len = merged_text.len() as u64;
                let aligned_len = align_up(current_len, align);
                let padding = (aligned_len - current_len) as usize;
                // Use BRK #0 (0x00, 0x00, 0x20, 0xD4) as padding for ARM64
                for _ in 0..padding / 4 {
                    merged_text.extend_from_slice(&[0x00, 0x00, 0x20, 0xD4]);
                }
                // Handle non-aligned padding
                for _ in 0..(padding % 4) {
                    merged_text.push(0x00);
                }

                let offset = merged_text.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);
                merged_text.extend_from_slice(&section.data);

                collect_section_relocations(
                    obj,
                    section,
                    offset,
                    obj_idx,
                    PatchHome::Text,
                    &mut pending_relocations,
                )?;
            }
        }

        // Merge rodata sections directly into __TEXT (after code).
        // This keeps rodata addresses close to code for PC-relative addressing.
        // Patch sites in rodata live in the merged text buffer, so their home
        // is Text and their relocations are applied against that base.
        // (RUE-131 items 8 and 11)
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !is_rodata_section(&section.name) {
                    continue;
                }
                // Note: empty sections are kept because they may still have
                // symbols at offset 0 (e.g., empty strings) that need addresses.

                // Align rodata (8-byte alignment is common for data)
                let align = section.align.max(8);
                let padding = align_up(merged_text.len() as u64, align) - merged_text.len() as u64;
                merged_text.resize(merged_text.len() + padding as usize, 0);

                let offset = merged_text.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);
                merged_text.extend_from_slice(&section.data);

                collect_section_relocations(
                    obj,
                    section,
                    offset,
                    obj_idx,
                    PatchHome::Text,
                    &mut pending_relocations,
                )?;
            }
        }

        // Merge data sections (their patch sites live in the merged data
        // buffer — patching them against merged_text bounds/contents was
        // RUE-131 item 8; the ELF side got the same fix via PatchHome in #928)
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !is_data_section(&section.name) || section.data.is_empty() {
                    continue;
                }

                let align = section.align.max(1);
                let padding = align_up(merged_data.len() as u64, align) - merged_data.len() as u64;
                merged_data.resize(merged_data.len() + padding as usize, 0);

                let offset = merged_data.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);
                merged_data.extend_from_slice(&section.data);

                collect_section_relocations(
                    obj,
                    section,
                    offset,
                    obj_idx,
                    PatchHome::Data,
                    &mut pending_relocations,
                )?;
            }
        }

        // Handle bss sections
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if !is_bss_section(&section.name) {
                    continue;
                }

                let align = section.align.max(1);
                let padding = align_up(bss_size, align) - bss_size;
                bss_size += padding;

                let offset = bss_size;
                section_offsets.insert((obj_idx, sec_idx), offset);
                bss_size += section.size;
            }
        }

        // Compute the final file/VM layout before placing symbols, using the
        // same single-source-of-truth computation as build_dynamic(). __DATA
        // begins wherever the actual __TEXT layout ends.
        // (RUE-131 items 3 and 8)
        let mut layout_builder = MachOBuilder::new()
            .with_code(merged_text.clone(), 0) // only the length matters for layout
            .with_data(merged_data.clone())
            .with_bss(bss_size);
        layout_builder.add_segment(Segment64::pagezero());
        let mut text_segment =
            Segment64::new("__TEXT").with_protection(VM_PROT_READ | VM_PROT_EXECUTE);
        text_segment.add_section(Section64::new("__text", "__TEXT").with_code_flags());
        layout_builder.add_segment(text_segment);
        // build_dynamic adds __LINKEDIT itself; adding it here keeps this
        // builder's command sizes identical (compute_dynamic_layout counts it
        // either way).
        layout_builder.add_segment(Segment64::new("__LINKEDIT").with_protection(VM_PROT_READ));
        let layout = layout_builder.dynamic_layout();

        let text_file_offset = layout.text_file_offset as u64;
        // Code+rodata are mapped at this virtual address
        let text_vaddr = VM_BASE + text_file_offset;
        // __DATA segment virtual address (0 if there is no writable data)
        let data_vaddr = layout.data_vm_addr;
        // bss begins right after the 8-aligned initialized data within the
        // __DATA segment; `merged_data.len()` is the complete offset and must
        // be added exactly once. (RUE-131 item 4)
        let bss_vaddr = if bss_size > 0 {
            data_vaddr + align_up(merged_data.len() as u64, 8)
        } else {
            0
        };
        tracing::trace!(
            text_file_offset = format_args!("0x{text_file_offset:x}"),
            data_vaddr = format_args!("0x{data_vaddr:x}"),
            "calculated Mach-O layout"
        );

        // Build symbol table mapping: name -> final virtual address.
        // Globals by name; locals by (object, name) so same-named locals in
        // different objects can't shadow each other (RUE-131 item 2).
        let mut symbol_addresses = SymbolAddresses::default();

        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for sym in &obj.symbols {
                if sym.name.is_empty() {
                    continue;
                }

                if let Some(sec_idx) = sym.section_index {
                    if sec_idx >= obj.sections.len() {
                        continue;
                    }
                    if let Some(&section_offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                        let section = &obj.sections[sec_idx];
                        let addr =
                            if is_text_section(&section.name) || is_rodata_section(&section.name) {
                                // Text and rodata are both in merged_text
                                text_vaddr + section_offset + sym.value
                            } else if is_data_section(&section.name) {
                                // Writable data in __DATA segment
                                data_vaddr + section_offset + sym.value
                            } else if is_bss_section(&section.name) {
                                bss_vaddr + section_offset + sym.value
                            } else {
                                continue;
                            };

                        match sym.binding {
                            SymbolBinding::Local => {
                                symbol_addresses
                                    .locals
                                    .insert((obj_idx, sym.name.clone()), addr);
                            }
                            SymbolBinding::Global | SymbolBinding::Weak => {
                                // Honor the winner chosen in add_object for
                                // duplicate weak definitions.
                                let winner = self
                                    .global_symbols
                                    .get(&sym.name)
                                    .map(|(winning_obj, _)| *winning_obj);
                                if winner.is_none() || winner == Some(obj_idx) {
                                    symbol_addresses.globals.insert(sym.name.clone(), addr);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Find entry point (always a global symbol).
        //
        // The global symbol table is keyed by source-level names: parsing strips
        // the single leading underscore that Mach-O adds to every symbol (see
        // `strip_macho_underscore`). The caller passes the entry point by its
        // on-disk Mach-O name (e.g. the runtime's `_main` is requested here as
        // its emitted `__main`), so strip it the same way before looking it up.
        // We still try the raw and one-more-prefixed forms so an already
        // source-level entry name, or a differently-prefixed one, resolves too.
        // Getting the strip right made `_foo`/`__foo` distinct but also shortened
        // the runtime entry to `_main`, so this lookup must strip in lockstep
        // (RUE-919).
        let stripped_entry = crate::util::strip_macho_underscore(entry_point);
        let entry_vaddr = symbol_addresses
            .globals
            .get(entry_point)
            .or_else(|| symbol_addresses.globals.get(stripped_entry))
            .or_else(|| symbol_addresses.globals.get(&format!("_{}", entry_point)))
            .copied()
            .ok_or_else(|| LinkError::UndefinedSymbol(entry_point.to_string()))?;
        let entry_offset = entry_vaddr - text_vaddr;

        // Apply relocations
        for PendingRelocation {
            offset: patch_offset,
            sym_name,
            sym_section,
            sym_binding,
            obj_idx,
            rel_type,
            addend,
            home,
        } in pending_relocations
        {
            // Pick the buffer the patch site lives in and its base vaddr.
            // (PatchHome::Rodata is unused here: Mach-O merges rodata into
            // the text buffer.)
            let (buf, base_vaddr): (&mut Vec<u8>, u64) = match home {
                PatchHome::Text => (&mut merged_text, text_vaddr),
                PatchHome::Rodata => unreachable!("Mach-O rodata is merged into the text buffer"),
                PatchHome::Data => (&mut merged_data, data_vaddr),
            };

            // Resolve the symbol (locals only within their own object), with
            // the same fallbacks as the ELF path: the symbol's own section,
            // then 0 for undefined weaks. Any other unresolved symbol is an
            // error because this output emits no dynamic binding information;
            // leaving its relocation unpatched would produce invalid code.
            // (RUE-131 item 7)
            let target_vaddr =
                if let Some(addr) = symbol_addresses.resolve(obj_idx, &sym_name, sym_binding) {
                    addr
                } else if let Some(sec_idx) = sym_section {
                    // Section-relative symbol - resolve via the section's address
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
                        if is_text_section(&section.name) || is_rodata_section(&section.name) {
                            text_vaddr + sec_offset
                        } else if is_data_section(&section.name) {
                            data_vaddr + sec_offset
                        } else if is_bss_section(&section.name) {
                            bss_vaddr + sec_offset
                        } else {
                            return Err(LinkError::UndefinedSymbol(format!(
                                "{} (in section '{}')",
                                sym_name, section.name
                            )));
                        }
                    } else {
                        return Err(LinkError::UndefinedSymbol(format!(
                            "{} (section {} not in section_offsets)",
                            sym_name, sec_idx
                        )));
                    }
                } else if sym_binding == SymbolBinding::Weak {
                    // An undefined WEAK symbol resolves to address 0 (RUE-131 item 9)
                    0
                } else {
                    return Err(LinkError::UndefinedSymbol(format!(
                        "{} (no section, rel_type={:?})",
                        sym_name, rel_type
                    )));
                };

            let patch_vaddr = checked_relocation_offset(base_vaddr, patch_offset, rel_type)?;

            tracing::trace!(
                symbol = %sym_name,
                patch_offset = format_args!("0x{patch_offset:x}"),
                rel_type = ?rel_type,
                target_vaddr = format_args!("0x{target_vaddr:x}"),
                patch_vaddr = format_args!("0x{patch_vaddr:x}"),
                addend,
                "relocating symbol"
            );

            // Patch the site. The per-kind encoding — including the bounds
            // check against ITS OWN buffer (RUE-131 item 8) — is shared with
            // the ELF path via `apply_relocation` (RUE-335).
            apply_relocation(
                buf,
                patch_offset,
                patch_vaddr,
                target_vaddr,
                addend,
                rel_type,
                &sym_name,
            )?;
        }

        // Now build the binary with the relocated code.
        // Note: rodata is already included in merged_text (code + rodata in __TEXT);
        // only writable data goes to the __DATA segment.
        tracing::trace!(
            merged_text_size = format_args!("0x{:x}", merged_text.len()),
            "building Mach-O with relocated code"
        );
        let mut builder = MachOBuilder::new()
            .with_code(merged_text, entry_offset)
            .with_data(merged_data)
            .with_bss(bss_size);

        // Add __PAGEZERO segment (catches null pointer dereferences)
        builder.add_segment(Segment64::pagezero());

        // Add __TEXT segment with __text section
        let mut text_segment =
            Segment64::new("__TEXT").with_protection(VM_PROT_READ | VM_PROT_EXECUTE);
        let text_section = Section64::new("__text", "__TEXT").with_code_flags();
        text_segment.add_section(text_section);
        builder.add_segment(text_segment);

        // Build as dynamic executable (uses LC_MAIN + dyld)
        let (bytes, calculated_offset, _data_vm_addr) = builder.build_dynamic();

        // Verify our pre-calculated offset matches (sanity check)
        if text_file_offset != calculated_offset {
            tracing::warn!(
                pre_calculated = format_args!("{text_file_offset:#x}"),
                actual = format_args!("{calculated_offset:#x}"),
                "text_file_offset mismatch"
            );
        }

        Ok(bytes)
    }

    /// Link all objects and produce an ELF executable.
    fn link_elf(self, entry_point: &str) -> Result<Vec<u8>, LinkError> {
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
        let mut merged_text = Vec::new();
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
                if classify_section(&section.name) != SectionKind::Text || section.data.is_empty() {
                    continue;
                }

                // Align
                let align = section.align.max(1);
                let padding = align_up(merged_text.len() as u64, align) - merged_text.len() as u64;
                merged_text.resize(merged_text.len() + padding as usize, 0xCC); // INT3 padding

                let offset = merged_text.len() as u64;
                section_offsets.insert((obj_idx, sec_idx), offset);

                merged_text.extend_from_slice(&section.data);

                collect_section_relocations(
                    obj,
                    section,
                    offset,
                    obj_idx,
                    PatchHome::Text,
                    &mut pending_relocations,
                )?;
            }
        }

        // Merge rodata sections (placed on a new page for proper W^X protection)
        // We need page alignment between code and rodata so they can have different
        // memory protections at runtime.
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if classify_section(&section.name) != SectionKind::Rodata {
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

                collect_section_relocations(
                    obj,
                    section,
                    offset,
                    obj_idx,
                    PatchHome::Rodata,
                    &mut pending_relocations,
                )?;
            }
        }

        // Merge .data sections (initialized data - placed in data segment)
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if classify_section(&section.name) != SectionKind::Data {
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

                collect_section_relocations(
                    obj,
                    section,
                    offset,
                    obj_idx,
                    PatchHome::Data,
                    &mut pending_relocations,
                )?;
            }
        }

        // Handle .bss sections (uninitialized data - zero-filled at runtime)
        // .bss comes after .data in memory, but doesn't take file space
        let bss_offset_in_data = merged_data.len() as u64;

        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if classify_section(&section.name) != SectionKind::Bss {
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
        let code_size = merged_text.len() as u64;

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

        // Build final symbol addresses: globals by name, locals by
        // (object, name) so same-named locals can't shadow each other.
        let mut symbol_addresses = SymbolAddresses::default();

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
                        let base = match classify_section(&section.name) {
                            SectionKind::Text => code_vaddr,
                            SectionKind::Rodata => rodata_vaddr,
                            SectionKind::Data => data_vaddr,
                            SectionKind::Bss => bss_vaddr,
                            SectionKind::Other => continue,
                        };

                        let addr = base + section_offset + sym.value;

                        match sym.binding {
                            SymbolBinding::Local => {
                                symbol_addresses
                                    .locals
                                    .insert((obj_idx, sym.name.clone()), addr);
                            }
                            SymbolBinding::Global | SymbolBinding::Weak => {
                                // For duplicate weak definitions the winner was
                                // already chosen in add_object (first weak wins,
                                // strong overrides); honor that choice here
                                // instead of letting the last definition win.
                                let winner = self
                                    .global_symbols
                                    .get(&sym.name)
                                    .map(|(winning_obj, _)| *winning_obj);
                                if winner.is_none() || winner == Some(obj_idx) {
                                    symbol_addresses.globals.insert(sym.name.clone(), addr);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Also add section symbols for text, rodata, data, and bss relocation
        // This is needed for internal calls within object files that reference section names
        // (e.g., .text._ZN11rue_runtime4heap5alloc... for Rust runtime internal calls).
        // Section symbols are local to their object, so key them per object.
        for (obj_idx, obj) in self.objects.iter().enumerate() {
            for (sec_idx, section) in obj.sections.iter().enumerate() {
                if let Some(&offset) = section_offsets.get(&(obj_idx, sec_idx)) {
                    let addr = match classify_section(&section.name) {
                        SectionKind::Text => code_vaddr + offset,
                        SectionKind::Rodata => rodata_vaddr + offset,
                        SectionKind::Data => data_vaddr + offset,
                        SectionKind::Bss => bss_vaddr + offset,
                        SectionKind::Other => continue,
                    };
                    // Use section name as fallback
                    symbol_addresses
                        .locals
                        .entry((obj_idx, section.name.clone()))
                        .or_insert(addr);
                }
            }
        }

        // Find entry point (always a global symbol)
        let entry_addr = *symbol_addresses
            .globals
            .get(entry_point)
            .ok_or_else(|| LinkError::UndefinedSymbol(entry_point.to_string()))?;

        // Apply relocations
        for PendingRelocation {
            offset,
            sym_name,
            sym_section,
            sym_binding,
            obj_idx,
            rel_type,
            addend,
            home,
        } in pending_relocations
        {
            // Pick the buffer the patch site lives in and its base vaddr.
            // PC-relative relocations in rodata/data measure from their own
            // section's address, not the code segment's.
            let (buf, base_vaddr): (&mut Vec<u8>, u64) = match home {
                PatchHome::Text => (&mut merged_text, code_vaddr),
                PatchHome::Rodata => (&mut merged_rodata, rodata_vaddr),
                PatchHome::Data => (&mut merged_data, data_vaddr),
            };
            // Try to resolve the symbol (locals only within their own object)
            let target_addr =
                if let Some(addr) = symbol_addresses.resolve(obj_idx, &sym_name, sym_binding) {
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
                        let base = match classify_section(&section.name) {
                            SectionKind::Text => code_vaddr,
                            SectionKind::Rodata => rodata_vaddr,
                            SectionKind::Data => data_vaddr,
                            SectionKind::Bss => bss_vaddr,
                            SectionKind::Other => {
                                return Err(LinkError::UndefinedSymbol(format!(
                                    "{} (in section '{}')",
                                    sym_name, section.name
                                )));
                            }
                        };
                        base + sec_offset
                    } else {
                        return Err(LinkError::UndefinedSymbol(format!(
                            "{} (section {} not in section_offsets)",
                            sym_name, sec_idx
                        )));
                    }
                } else if sym_binding == SymbolBinding::Weak {
                    // An undefined WEAK symbol resolves to address 0 rather than
                    // erroring — standard ELF semantics, lets code do null checks
                    // against optional symbols. (RUE-131 item 9)
                    0
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

            let pc = checked_relocation_offset(base_vaddr, offset, rel_type)?;

            // Patch the site; the per-kind encoding is shared with the
            // Mach-O path via `apply_relocation` (RUE-335).
            apply_relocation(buf, offset, pc, target_addr, addend, rel_type, &sym_name)?;
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
        // This path (link_elf) produces ELF executables. Mach-O targets are
        // emitted directly by link_macho — the crate uses no system linker.
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

        // Pad to the reserved code file offset before writing .text. The text
        // PT_LOAD's p_offset, code_vaddr, and e_entry all reserve HEADER_SIZE
        // (space for the maximum number of program headers), but only
        // num_program_headers were actually written above. Without this pad a
        // link with fewer than the reserved number of LOAD segments would place
        // .text at a smaller file offset than its header advertises — a corrupt,
        // non-loadable executable (and it must be *padded*, not shrunk, because
        // the ELF rule p_offset % page == p_vaddr % page is baked into
        // code_vaddr). Real compiles always emit 3 segments, so this was latent;
        // it is reachable via the Linker API with a code-only / code+rodata
        // object.
        if (elf.len() as u64) < code_file_offset {
            elf.resize(code_file_offset as usize, 0);
        }

        // Write code section
        elf.extend_from_slice(&merged_text);

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
        Self::new(
            Target::host()
                .expect("Rue linker cannot choose a default target on this unsupported host"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{
        E_MACHINE_OFFSET, E_SHNUM_OFFSET, E_SHOFF_OFFSET, E_TYPE_OFFSET, EI_CLASS, EI_DATA,
        EI_VERSION, ELF64_EHDR_SIZE as TEST_EHDR_SIZE, ELF64_PHDR_SIZE as TEST_PHDR_SIZE,
        EM_AARCH64, EM_X86_64, SHT_RELA,
    };
    use crate::elf::{ObjectFile, SectionFlags};
    use crate::emit::{CodeRelocation, ObjectBuilder};

    // Use X86_64Linux explicitly for ELF tests since ObjectFile only parses ELF
    // and the Linker produces ELF executables
    const ELF_TARGET: Target = Target::X86_64Linux;

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
        assert_eq!(
            LinkError::RelocationOffsetOverflow {
                base_offset: u64::MAX,
                relocation_offset: 1,
                rel_type: "Abs64".into(),
            }
            .to_string(),
            format!(
                "relocation offset overflow: Abs64 relocation base {} plus section offset 1 \
                 exceeds u64",
                u64::MAX
            )
        );
    }

    #[test]
    fn relocation_patch_range_rejects_overflow_and_crossing_end() {
        for (patch_offset, patch_size, section_size) in [(u64::MAX, 4, 8), (1, 8, 8)] {
            let err = checked_patch_range(
                patch_offset,
                patch_size,
                section_size,
                RelocationType::Abs64,
            )
            .unwrap_err();
            assert!(matches!(
                err,
                LinkError::RelocationPatchOutOfBounds {
                    patch_offset: actual,
                    patch_size: actual_size,
                    section_size: actual_section,
                    ..
                } if actual == patch_offset
                    && actual_size == patch_size
                    && actual_section == section_size
            ));
        }

        assert_eq!(
            checked_patch_range(0, 8, 8, RelocationType::Abs64).unwrap(),
            0..8
        );
    }

    #[test]
    fn relocation_offset_composition_rejects_u64_overflow() {
        let err = checked_relocation_offset(u64::MAX, 1, RelocationType::Abs64).unwrap_err();
        assert!(matches!(
            err,
            LinkError::RelocationOffsetOverflow {
                base_offset: u64::MAX,
                relocation_offset: 1,
                ..
            }
        ));
    }

    #[test]
    fn shared_relocation_patcher_rejects_unrepresentable_offset() {
        let mut buf = vec![0u8; 4];
        let err = apply_relocation(
            &mut buf,
            u64::MAX,
            0,
            0,
            0,
            RelocationType::GotPcRel,
            "malformed",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            LinkError::RelocationPatchOutOfBounds {
                patch_offset: u64::MAX,
                ..
            }
        ));
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
    fn test_code_only_link_text_at_advertised_offset() {
        // A code-only object links to fewer than the reserved number of LOAD
        // segments. The text PT_LOAD's p_offset reserves the max-header
        // HEADER_SIZE, so the .text bytes must be *padded* to sit at that offset
        // rather than immediately after the (fewer) written headers — otherwise
        // the executable is corrupt. Regression for the rue-linker review finding.
        let code = vec![0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3]; // mov eax, 42; ret
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(code.clone())
            .build();
        let obj = ObjectFile::parse(&obj_bytes).unwrap();
        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();
        let elf = linker.link("main").unwrap();

        // e_phoff is at ELF64 header offset 32; the text R+X segment is written
        // as the first program header, whose p_offset is at byte 8 of the phdr.
        let e_phoff = u64::from_le_bytes(elf[32..40].try_into().unwrap()) as usize;
        let text_p_offset =
            u64::from_le_bytes(elf[e_phoff + 8..e_phoff + 16].try_into().unwrap()) as usize;
        // The .text bytes must physically sit at the offset the header advertises.
        assert_eq!(
            &elf[text_p_offset..text_p_offset + code.len()],
            &code[..],
            "code must be at the file offset the text program header advertises"
        );
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

    /// Parse the program headers of a linked ELF, returning
    /// (p_flags, p_offset, p_vaddr) per header.
    fn parse_program_headers(elf: &[u8]) -> Vec<(u32, u64, u64)> {
        let e_phoff = u64::from_le_bytes(elf[0x20..0x28].try_into().unwrap()) as usize;
        let e_phnum = u16::from_le_bytes(elf[0x38..0x3A].try_into().unwrap()) as usize;
        (0..e_phnum)
            .map(|i| {
                let ph = &elf[e_phoff + i * TEST_PHDR_SIZE..];
                let p_flags = u32::from_le_bytes(ph[4..8].try_into().unwrap());
                let p_offset = u64::from_le_bytes(ph[8..16].try_into().unwrap());
                let p_vaddr = u64::from_le_bytes(ph[16..24].try_into().unwrap());
                (p_flags, p_offset, p_vaddr)
            })
            .collect()
    }

    /// Hand-build an object with a `.text` (main: ret) plus one extra section
    /// carrying an Abs64 relocation at offset 0 against `main`.
    fn object_with_reloc_section(name: &str, flags: SectionFlags) -> ObjectFile {
        use crate::elf::{Relocation, Section, SymbolType};

        let text = Section {
            name: ".text".into(),
            data: vec![0xC3], // ret
            size: 1,
            flags: SectionFlags::ALLOC | SectionFlags::EXEC,
            relocations: vec![],
            align: 16,
        };
        let extra = Section {
            name: name.into(),
            // 8 zero bytes — the unrelocated pointer slot
            data: vec![0u8; 8],
            size: 8,
            flags,
            relocations: vec![Relocation {
                offset: 0,
                symbol_index: 0,
                rel_type: RelocationType::Abs64,
                addend: 0,
            }],
            align: 8,
        };
        let symbols = vec![Symbol {
            name: "main".into(),
            section_index: Some(0),
            value: 0,
            size: 1,
            binding: SymbolBinding::Global,
            sym_type: SymbolType::Func,
        }];
        let mut section_map = HashMap::new();
        section_map.insert(".text".into(), 0);
        section_map.insert(name.to_string(), 1);
        ObjectFile {
            sections: vec![text, extra],
            symbols,
            section_map,
            machine: crate::elf::ElfMachine::X86_64,
            format: crate::elf::ObjectFormat::Elf,
        }
    }

    #[test]
    fn link_rejects_relocation_outside_its_source_section() {
        let mut obj = object_with_reloc_section(".rodata", SectionFlags::ALLOC);
        obj.sections[1].relocations[0].offset = u64::MAX;

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();
        let err = linker.link("main").unwrap_err();

        assert!(matches!(
            err,
            LinkError::RelocationPatchOutOfBounds {
                patch_offset: u64::MAX,
                patch_size: 8,
                section_size: 8,
                ..
            }
        ));
    }

    #[test]
    fn parsed_elf_with_max_relocation_offset_returns_link_error() {
        let mut bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![0xE8, 0, 0, 0, 0, 0xC3])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "callee".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .build();

        let section_headers = u64::from_le_bytes(
            bytes[E_SHOFF_OFFSET..E_SHOFF_OFFSET + 8]
                .try_into()
                .unwrap(),
        ) as usize;
        let section_header_size = u16::from_le_bytes(bytes[58..60].try_into().unwrap()) as usize;
        let section_count = u16::from_le_bytes(
            bytes[E_SHNUM_OFFSET..E_SHNUM_OFFSET + 2]
                .try_into()
                .unwrap(),
        ) as usize;
        let relocation_data = (0..section_count)
            .find_map(|index| {
                let header = section_headers + index * section_header_size;
                let section_type =
                    u32::from_le_bytes(bytes[header + 4..header + 8].try_into().unwrap());
                (section_type == SHT_RELA).then(|| {
                    u64::from_le_bytes(bytes[header + 24..header + 32].try_into().unwrap()) as usize
                })
            })
            .expect("ObjectBuilder must emit a relocation section");
        bytes[relocation_data..relocation_data + 8].copy_from_slice(&u64::MAX.to_le_bytes());

        let obj = ObjectFile::parse(&bytes).expect("malformed patch offset remains parseable");
        let parsed_relocation = obj
            .sections
            .iter()
            .flat_map(|section| &section.relocations)
            .next()
            .expect("parsed object must retain its relocation");
        assert_eq!(parsed_relocation.offset, u64::MAX);

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();
        let err = linker.link("main").unwrap_err();
        assert!(matches!(
            err,
            LinkError::RelocationPatchOutOfBounds {
                patch_offset: u64::MAX,
                patch_size: 4,
                section_size: 6,
                ..
            }
        ));
    }

    /// RUE-131 item 1: relocations in `.rodata` used to be silently dropped,
    /// leaving null pointers in every linked binary (e.g. panic-Location file
    /// pointers). The pointer slot must contain main's virtual address.
    #[test]
    fn test_rodata_relocation_applied() {
        let obj = object_with_reloc_section(".rodata", SectionFlags::ALLOC);
        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();
        let elf = linker.link("main").unwrap();

        let phdrs = parse_program_headers(&elf);
        let (_, text_off, text_vaddr) = phdrs
            .iter()
            .copied()
            .find(|(f, ..)| *f == (PF_R | PF_X))
            .expect("R+X segment");
        let (_, ro_off, _) = phdrs
            .iter()
            .copied()
            .find(|(f, ..)| *f == PF_R)
            .expect("R-only rodata segment");

        // main is the first byte of merged text; its vaddr is where the R+X
        // segment maps its file content. Code starts at text_off within it.
        let main_vaddr = text_vaddr + (TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE) as u64 - text_off;
        let slot = u64::from_le_bytes(
            elf[ro_off as usize..ro_off as usize + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            slot, main_vaddr,
            "Abs64 relocation in .rodata must be applied (was silently dropped)"
        );
        assert_ne!(slot, 0, "pointer slot must not be null");
    }

    /// RUE-131 item 1, `.data` flavor: same drop, writable segment.
    #[test]
    fn test_data_relocation_applied() {
        let obj = object_with_reloc_section(".data", SectionFlags::ALLOC | SectionFlags::WRITE);
        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();
        let elf = linker.link("main").unwrap();

        let phdrs = parse_program_headers(&elf);
        let (_, text_off, text_vaddr) = phdrs
            .iter()
            .copied()
            .find(|(f, ..)| *f == (PF_R | PF_X))
            .expect("R+X segment");
        let (_, data_off, _) = phdrs
            .iter()
            .copied()
            .find(|(f, ..)| *f == (PF_R | PF_W))
            .expect("RW data segment");

        let main_vaddr = text_vaddr + (TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE) as u64 - text_off;
        let slot = u64::from_le_bytes(
            elf[data_off as usize..data_off as usize + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            slot, main_vaddr,
            "Abs64 relocation in .data must be applied (was silently dropped)"
        );
    }

    /// R_AARCH64_LDST64_ABS_LO12_NC (type 286): the ADRP+LDR pair the aarch64
    /// runtime emits against 64-bit statics. The scaled low 12 bits of S + A go
    /// into the imm12 field (instruction bits [21:10]), scaled DOWN by 8.
    #[test]
    fn test_ldst64_lo12_relocation_scales_and_patches() {
        // `LDR x0, [x0, #0]` (64-bit unsigned-offset load) = 0xF9400000.
        // S = 0x2678, A = 0 -> effective low12 = 0x678 (8-byte aligned).
        // imm12 = 0x678 >> 3 = 0xCF (207); placed at bits [21:10]:
        //   0xCF << 10 = 0x0003_3C00, so the patched word = 0xF9433C00.
        let mut buf = 0xF940_0000u32.to_le_bytes().to_vec();
        apply_relocation(
            &mut buf,
            0,
            0x1000,
            0x2678,
            0,
            RelocationType::Ldst64Lo12,
            "sym",
        )
        .unwrap();
        let patched = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        assert_eq!(patched, 0xF943_3C00);
        // imm12 field (bits [21:10]) reads back as 0x678 >> 3 = 0xCF.
        assert_eq!((patched >> 10) & 0xFFF, 0xCF);
    }

    /// A 32-bit load (LDST32, type 285) must scale by 4, not 8: proves the
    /// per-width scaling is keyed off the relocation type.
    #[test]
    fn test_ldst32_lo12_relocation_scales_by_four() {
        // `LDR w0, [x0, #0]` (32-bit unsigned-offset load) = 0xB9400000.
        // S = 0x2674 -> low12 = 0x674 (4-byte aligned); 0x674 >> 2 = 0x19D.
        //   0x19D << 10 = 0x0006_7400, so the patched word = 0xB9467400.
        let mut buf = 0xB940_0000u32.to_le_bytes().to_vec();
        apply_relocation(
            &mut buf,
            0,
            0x1000,
            0x2674,
            0,
            RelocationType::Ldst32Lo12,
            "sym",
        )
        .unwrap();
        let patched = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        assert_eq!(patched, 0xB946_7400);
        assert_eq!((patched >> 10) & 0xFFF, 0x19D);
    }

    /// A result that is not aligned to the access width cannot be represented in
    /// the scaled imm12, so it must be reported as a link error rather than
    /// silently truncated.
    #[test]
    fn test_ldst64_lo12_relocation_rejects_misaligned() {
        // S = 0x2674 -> low12 = 0x674, not 8-byte aligned.
        let mut buf = 0xF940_0000u32.to_le_bytes().to_vec();
        let err = apply_relocation(
            &mut buf,
            0,
            0x1000,
            0x2674,
            0,
            RelocationType::Ldst64Lo12,
            "sym",
        )
        .unwrap_err();
        assert!(matches!(err, LinkError::RelocationOverflow { .. }));
    }

    /// RUE-131 item 9: among multiple WEAK definitions, the first wins; a
    /// strong definition overrides a weak one in either order.
    #[test]
    fn test_weak_symbol_first_wins_strong_overrides() {
        use crate::elf::{Relocation, Section, SymbolType};
        let make_obj = |name: &str, code: Vec<u8>, binding: SymbolBinding| ObjectFile {
            sections: vec![Section {
                name: ".text".into(),
                data: code,
                size: 1,
                flags: SectionFlags::ALLOC | SectionFlags::EXEC,
                relocations: vec![],
                align: 16,
            }],
            symbols: vec![Symbol {
                name: name.into(),
                section_index: Some(0),
                value: 0,
                size: 1,
                binding,
                sym_type: SymbolType::Func,
            }],
            section_map: HashMap::from([(".text".into(), 0)]),
            machine: crate::elf::ElfMachine::X86_64,
            format: crate::elf::ObjectFormat::Elf,
        };
        let _ = Relocation {
            offset: 0,
            symbol_index: 0,
            rel_type: RelocationType::Abs64,
            addend: 0,
        };

        // weak then weak: first wins
        let mut linker = Linker::new(ELF_TARGET);
        linker
            .add_object(make_obj("f", vec![0xC3], SymbolBinding::Weak))
            .unwrap();
        linker
            .add_object(make_obj("f", vec![0x90], SymbolBinding::Weak))
            .unwrap();
        let (obj_idx, _) = linker.global_symbols.get("f").unwrap();
        assert_eq!(*obj_idx, 0, "first weak definition must win");

        // weak then strong: strong overrides
        let mut linker = Linker::new(ELF_TARGET);
        linker
            .add_object(make_obj("f", vec![0xC3], SymbolBinding::Weak))
            .unwrap();
        linker
            .add_object(make_obj("f", vec![0x90], SymbolBinding::Global))
            .unwrap();
        let (obj_idx, sym) = linker.global_symbols.get("f").unwrap();
        assert_eq!(*obj_idx, 1, "strong definition must override weak");
        assert_eq!(sym.binding, SymbolBinding::Global);

        // strong then weak: strong kept
        let mut linker = Linker::new(ELF_TARGET);
        linker
            .add_object(make_obj("f", vec![0xC3], SymbolBinding::Global))
            .unwrap();
        linker
            .add_object(make_obj("f", vec![0x90], SymbolBinding::Weak))
            .unwrap();
        let (obj_idx, _) = linker.global_symbols.get("f").unwrap();
        assert_eq!(
            *obj_idx, 0,
            "strong definition must be kept over later weak"
        );
    }

    /// Build an archive member that defines `defs` (each `(name, binding)` as a
    /// section-anchored definition) and references `undefs` as undefined global
    /// symbols. Each member gets a one-byte `.text` section so distinct members
    /// stay individually linkable. Used by the RUE-848 archive-order tests.
    fn archive_member(defs: &[(&str, SymbolBinding)], undefs: &[&str]) -> ObjectFile {
        use crate::elf::SymbolType;
        let mut symbols: Vec<Symbol> = defs
            .iter()
            .map(|(name, binding)| Symbol {
                name: (*name).into(),
                section_index: Some(0),
                value: 0,
                size: 1,
                binding: *binding,
                sym_type: SymbolType::Func,
            })
            .collect();
        symbols.extend(undefs.iter().map(|name| Symbol {
            name: (*name).into(),
            section_index: None,
            value: 0,
            size: 0,
            binding: SymbolBinding::Global,
            sym_type: SymbolType::None,
        }));
        ObjectFile {
            sections: vec![Section {
                name: ".text".into(),
                data: vec![0xC3],
                size: 1,
                flags: SectionFlags::ALLOC | SectionFlags::EXEC,
                relocations: vec![],
                align: 16,
            }],
            symbols,
            section_map: HashMap::from([(".text".into(), 0)]),
            machine: crate::elf::ElfMachine::X86_64,
            format: crate::elf::ObjectFormat::Elf,
        }
    }

    /// RUE-848: two members strong-define the same symbol. First-eligible archive
    /// order must pull only the earliest member, so no duplicate-symbol collision
    /// occurs and the later provider is left out. A last-writer-wins index would
    /// instead select the later member.
    #[test]
    fn test_archive_duplicate_strong_selects_first_member() {
        let archive = Archive {
            objects: vec![
                archive_member(
                    &[
                        ("dup", SymbolBinding::Global),
                        ("marker_a", SymbolBinding::Global),
                    ],
                    &[],
                ),
                archive_member(
                    &[
                        ("dup", SymbolBinding::Global),
                        ("marker_b", SymbolBinding::Global),
                    ],
                    &[],
                ),
            ],
        };
        let mut linker = Linker::new(ELF_TARGET);
        linker.require_symbol("dup");
        linker
            .add_archive(archive)
            .expect("first-eligible extraction avoids the duplicate collision");

        assert!(
            linker.global_symbols.contains_key("marker_a"),
            "first member must be pulled"
        );
        assert!(
            !linker.global_symbols.contains_key("marker_b"),
            "later duplicate provider must be left out"
        );
        let (_, sym) = linker.global_symbols.get("dup").expect("dup is defined");
        assert_eq!(sym.binding, SymbolBinding::Global);
    }

    /// RUE-848: two members weakly define the same symbol. The first provider in
    /// archive order wins, matching `add_object`'s "first weak wins" rule.
    #[test]
    fn test_archive_duplicate_weak_selects_first_member() {
        let archive = Archive {
            objects: vec![
                archive_member(
                    &[
                        ("dup", SymbolBinding::Weak),
                        ("marker_a", SymbolBinding::Global),
                    ],
                    &[],
                ),
                archive_member(
                    &[
                        ("dup", SymbolBinding::Weak),
                        ("marker_b", SymbolBinding::Global),
                    ],
                    &[],
                ),
            ],
        };
        let mut linker = Linker::new(ELF_TARGET);
        linker.require_symbol("dup");
        linker.add_archive(archive).unwrap();

        assert!(
            linker.global_symbols.contains_key("marker_a"),
            "first weak provider must be pulled"
        );
        assert!(
            !linker.global_symbols.contains_key("marker_b"),
            "second weak provider must be left out"
        );
        let (_, sym) = linker.global_symbols.get("dup").unwrap();
        assert_eq!(
            sym.binding,
            SymbolBinding::Weak,
            "the first (weak) provider defines dup"
        );
    }

    /// RUE-848: an earlier weak provider and a later strong provider. Archive
    /// extraction is order-driven — the first member satisfying the reference is
    /// pulled, regardless of binding — so the weak member wins and the strong
    /// later member is not extracted. This matches GNU `ld`/`ld64` archive-order
    /// semantics; the previous last-writer-wins index would have flipped this to
    /// the strong member.
    #[test]
    fn test_archive_weak_then_strong_selects_first_eligible() {
        let archive = Archive {
            objects: vec![
                archive_member(
                    &[
                        ("dup", SymbolBinding::Weak),
                        ("marker_a", SymbolBinding::Global),
                    ],
                    &[],
                ),
                archive_member(
                    &[
                        ("dup", SymbolBinding::Global),
                        ("marker_b", SymbolBinding::Global),
                    ],
                    &[],
                ),
            ],
        };
        let mut linker = Linker::new(ELF_TARGET);
        linker.require_symbol("dup");
        linker.add_archive(archive).unwrap();

        assert!(
            linker.global_symbols.contains_key("marker_a"),
            "earlier weak provider is first-eligible"
        );
        assert!(
            !linker.global_symbols.contains_key("marker_b"),
            "later strong provider is not extracted for an already-satisfied symbol"
        );
        let (_, sym) = linker.global_symbols.get("dup").unwrap();
        assert_eq!(
            sym.binding,
            SymbolBinding::Weak,
            "first-eligible archive order keeps the weak provider"
        );
    }

    /// RUE-848: a symbol already satisfied by a previously linked object must not
    /// pull any archive member that also provides it.
    #[test]
    fn test_archive_already_satisfied_pulls_no_member() {
        let mut linker = Linker::new(ELF_TARGET);
        linker
            .add_object(archive_member(&[("dup", SymbolBinding::Global)], &[]))
            .unwrap();
        let objects_before = linker.objects.len();

        let archive = Archive {
            objects: vec![archive_member(
                &[
                    ("dup", SymbolBinding::Global),
                    ("marker", SymbolBinding::Global),
                ],
                &[],
            )],
        };
        linker.require_symbol("dup");
        linker.add_archive(archive).unwrap();

        assert_eq!(
            linker.objects.len(),
            objects_before,
            "no archive member should be extracted"
        );
        assert!(
            !linker.global_symbols.contains_key("marker"),
            "the already-satisfied symbol pulls nothing"
        );
    }

    /// RUE-848: pulling one member exposes a new undefined symbol that only a
    /// later member provides. The fixed-point rescan must transitively select
    /// that later member.
    #[test]
    fn test_archive_transitively_selects_later_member() {
        let archive = Archive {
            objects: vec![
                archive_member(&[("entry", SymbolBinding::Global)], &["helper"]),
                archive_member(&[("helper", SymbolBinding::Global)], &[]),
            ],
        };
        let mut linker = Linker::new(ELF_TARGET);
        linker.require_symbol("entry");
        linker.add_archive(archive).unwrap();

        assert!(
            linker.global_symbols.contains_key("entry"),
            "the required member is pulled"
        );
        assert!(
            linker.global_symbols.contains_key("helper"),
            "the later member providing the transitively undefined symbol is pulled on rescan"
        );
    }

    /// RUE-131 item 9: a relocation against an UNDEFINED weak symbol resolves
    /// to address 0 instead of an undefined-symbol error.
    #[test]
    fn test_undefined_weak_resolves_to_zero() {
        use crate::elf::{Relocation, Section, SymbolType};
        let obj = ObjectFile {
            sections: vec![Section {
                name: ".text".into(),
                // mov rax, imm64 (placeholder patched by Abs64); ret
                data: vec![0x48, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0, 0xC3],
                size: 11,
                flags: SectionFlags::ALLOC | SectionFlags::EXEC,
                relocations: vec![Relocation {
                    offset: 2,
                    symbol_index: 1,
                    rel_type: RelocationType::Abs64,
                    addend: 0,
                }],
                align: 16,
            }],
            symbols: vec![
                Symbol {
                    name: "main".into(),
                    section_index: Some(0),
                    value: 0,
                    size: 11,
                    binding: SymbolBinding::Global,
                    sym_type: SymbolType::Func,
                },
                Symbol {
                    name: "optional_hook".into(),
                    section_index: None, // undefined
                    value: 0,
                    size: 0,
                    binding: SymbolBinding::Weak,
                    sym_type: SymbolType::None,
                },
            ],
            section_map: HashMap::from([(".text".into(), 0)]),
            machine: crate::elf::ElfMachine::X86_64,
            format: crate::elf::ObjectFormat::Elf,
        };

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj).unwrap();
        let elf = linker.link("main").unwrap();

        // The link must not error, and the mov's imm64 (the bytes following
        // the 0x48 0xB8 opcode in the output) must be 0 — the weak resolution.
        let opcode_pos = elf
            .windows(2)
            .position(|w| w == [0x48, 0xB8])
            .expect("mov rax, imm64 opcode present in linked output");
        let imm = u64::from_le_bytes(elf[opcode_pos + 2..opcode_pos + 10].try_into().unwrap());
        assert_eq!(imm, 0, "undefined weak symbol must resolve to address 0");
    }

    /// RUE-131 item 10: an object compiled for a different architecture must
    /// be refused — before this check, an "aarch64" output could silently
    /// embed x86 machine code.
    #[test]
    fn test_arch_mismatch_rejected() {
        let obj_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![0xC3])
            .build();
        let mut obj = ObjectFile::parse(&obj_bytes).unwrap();
        // Simulate an object built for the other architecture.
        obj.machine = crate::elf::ElfMachine::Aarch64;

        let mut linker = Linker::new(ELF_TARGET);
        let result = linker.add_object(obj);
        assert!(matches!(result, Err(LinkError::ArchMismatch { .. })));
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
        // The 2nd program header sits right after the ELF header + 1st phdr
        // (headers are contiguous from e_phoff); it is NOT the code offset.
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
        let header_and_phdr_size = TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE;
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
            machine: crate::elf::ElfMachine::X86_64,
            format: crate::elf::ObjectFormat::Elf,
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
            machine: crate::elf::ElfMachine::X86_64,
            format: crate::elf::ObjectFormat::Elf,
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
        let header_size = TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE;
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

        let header_size = TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE;
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

        let header_size = TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE;
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

        let header_size = TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE;
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

        let header_size = TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE;
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

        let header_size = TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE;
        let code = &elf[header_size..];

        // For static linking, GotPcRel should be relaxed just like GotPcRelX:
        // MOV (8B) -> LEA (8D)
        assert_eq!(code[0], 0x48, "REX.W prefix should be preserved");
        assert_eq!(
            code[1], 0x8D,
            "Opcode should be rewritten to LEA (8D) for GotPcRel relaxation"
        );
    }

    // =========================================================================
    // Mach-O Linker Tests
    // =========================================================================

    #[test]
    fn test_macho_minimal_executable() {
        use crate::constants::{MACHO64_HEADER_SIZE, MH_EXECUTE, MH_MAGIC_64};
        use crate::elf::{Section, SectionFlags, Symbol, SymbolBinding, SymbolType};

        // ARM64 code for: return 42
        // MOV W0, #42  -> 0x52800540
        // RET          -> 0xD65F03C0
        let code = vec![
            0x40, 0x05, 0x80, 0x52, // MOV W0, #42
            0xC0, 0x03, 0x5F, 0xD6, // RET
        ];

        let text_section = Section {
            name: "__text".to_string(),
            data: code,
            size: 8,
            flags: SectionFlags::ALLOC | SectionFlags::EXEC,
            relocations: vec![],
            align: 4,
        };

        let main_symbol = Symbol {
            name: "_main".to_string(),
            section_index: Some(0),
            value: 0,
            size: 8,
            binding: SymbolBinding::Global,
            sym_type: SymbolType::Func,
        };

        let obj = ObjectFile {
            sections: vec![text_section],
            symbols: vec![main_symbol],
            section_map: [("__text".to_string(), 0)].into_iter().collect(),
            machine: crate::elf::ElfMachine::Aarch64,
            format: crate::elf::ObjectFormat::MachO,
        };

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj).unwrap();

        let macho = linker.link("_main").unwrap();

        // Verify Mach-O header
        assert!(
            macho.len() > MACHO64_HEADER_SIZE,
            "File should be larger than header"
        );

        // Check magic number
        let magic = u32::from_le_bytes([macho[0], macho[1], macho[2], macho[3]]);
        assert_eq!(magic, MH_MAGIC_64, "Should have Mach-O 64-bit magic");

        // Check file type
        let filetype = u32::from_le_bytes([macho[12], macho[13], macho[14], macho[15]]);
        assert_eq!(filetype, MH_EXECUTE, "Should be executable file type");

        // Verify the code is present somewhere in the file
        // Look for our MOV W0, #42 instruction
        let mov_pattern = [0x40, 0x05, 0x80, 0x52];
        let found = macho.windows(4).any(|w| w == mov_pattern);
        assert!(found, "Code should be present in the executable");
    }

    #[test]
    fn test_macho_entry_point_lookup() {
        use crate::elf::{Section, SectionFlags, Symbol, SymbolBinding, SymbolType};

        // Simple ARM64 RET instruction
        let code = vec![0xC0, 0x03, 0x5F, 0xD6]; // RET

        let text_section = Section {
            name: "__text".to_string(),
            data: code,
            size: 4,
            flags: SectionFlags::ALLOC | SectionFlags::EXEC,
            relocations: vec![],
            align: 4,
        };

        // Symbol without underscore prefix (linker should try both)
        let main_symbol = Symbol {
            name: "main".to_string(),
            section_index: Some(0),
            value: 0,
            size: 4,
            binding: SymbolBinding::Global,
            sym_type: SymbolType::Func,
        };

        let obj = ObjectFile {
            sections: vec![text_section],
            symbols: vec![main_symbol],
            section_map: [("__text".to_string(), 0)].into_iter().collect(),
            machine: crate::elf::ElfMachine::Aarch64,
            format: crate::elf::ObjectFormat::MachO,
        };

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj).unwrap();

        // Should find "main" even though we look for it without underscore
        let result = linker.link("main");
        assert!(result.is_ok(), "Should find entry point 'main'");
    }

    #[test]
    fn test_macho_undefined_entry_point() {
        use crate::elf::{Section, SectionFlags, Symbol, SymbolBinding, SymbolType};

        let code = vec![0xC0, 0x03, 0x5F, 0xD6]; // RET

        let text_section = Section {
            name: "__text".to_string(),
            data: code,
            size: 4,
            flags: SectionFlags::ALLOC | SectionFlags::EXEC,
            relocations: vec![],
            align: 4,
        };

        let other_symbol = Symbol {
            name: "other_func".to_string(),
            section_index: Some(0),
            value: 0,
            size: 4,
            binding: SymbolBinding::Global,
            sym_type: SymbolType::Func,
        };

        let obj = ObjectFile {
            sections: vec![text_section],
            symbols: vec![other_symbol],
            section_map: [("__text".to_string(), 0)].into_iter().collect(),
            machine: crate::elf::ElfMachine::Aarch64,
            format: crate::elf::ObjectFormat::MachO,
        };

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj).unwrap();

        // Should fail because there's no "main" symbol
        let result = linker.link("main");
        assert!(
            matches!(result, Err(LinkError::UndefinedSymbol(_))),
            "Should return UndefinedSymbol error"
        );
    }

    // =========================================================================
    // RUE-131 items 2, 5, 7, 8, 11: symbol scoping and Mach-O reloc semantics
    // =========================================================================

    use crate::elf::{Relocation, SymbolType};

    /// Hand-build an ObjectFile with one section, symbols, and relocations.
    fn make_obj(
        machine: crate::elf::ElfMachine,
        sections: Vec<Section>,
        symbols: Vec<Symbol>,
    ) -> ObjectFile {
        let section_map = sections
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i))
            .collect();
        ObjectFile {
            sections,
            symbols,
            section_map,
            machine,
            format: match machine {
                crate::elf::ElfMachine::X86_64 => crate::elf::ObjectFormat::Elf,
                crate::elf::ElfMachine::Aarch64 => crate::elf::ObjectFormat::MachO,
            },
        }
    }

    fn text_section(name: &str, data: Vec<u8>, relocations: Vec<Relocation>) -> Section {
        Section {
            name: name.into(),
            size: data.len() as u64,
            data,
            flags: SectionFlags::ALLOC | SectionFlags::EXEC,
            relocations,
            align: 16,
        }
    }

    fn sym(name: &str, section: Option<usize>, value: u64, binding: SymbolBinding) -> Symbol {
        Symbol {
            name: name.into(),
            section_index: section,
            value,
            size: 0,
            binding,
            sym_type: SymbolType::Func,
        }
    }

    /// Walk a linked Mach-O's load commands, returning each LC_SEGMENT_64 as
    /// (name, vmaddr, fileoff, filesize).
    fn macho_segments(macho: &[u8]) -> Vec<(String, u64, u64, u64)> {
        use crate::constants::LC_SEGMENT_64;
        let ncmds = u32::from_le_bytes(macho[16..20].try_into().unwrap()) as usize;
        let mut segments = Vec::new();
        let mut off = crate::constants::MACHO64_HEADER_SIZE;
        for _ in 0..ncmds {
            let cmd = u32::from_le_bytes(macho[off..off + 4].try_into().unwrap());
            let cmdsize = u32::from_le_bytes(macho[off + 4..off + 8].try_into().unwrap()) as usize;
            if cmd == LC_SEGMENT_64 {
                let name_bytes = &macho[off + 8..off + 24];
                let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(16);
                let name = String::from_utf8_lossy(&name_bytes[..end]).to_string();
                let vmaddr = u64::from_le_bytes(macho[off + 24..off + 32].try_into().unwrap());
                let fileoff = u64::from_le_bytes(macho[off + 40..off + 48].try_into().unwrap());
                let filesize = u64::from_le_bytes(macho[off + 48..off + 56].try_into().unwrap());
                segments.push((name, vmaddr, fileoff, filesize));
            }
            off += cmdsize;
        }
        segments
    }

    /// Read the LC_MAIN entryoff from a linked Mach-O (== the file offset of
    /// merged code when the entry function is at code offset 0).
    fn macho_entryoff(macho: &[u8]) -> u64 {
        use crate::constants::LC_MAIN;
        let ncmds = u32::from_le_bytes(macho[16..20].try_into().unwrap()) as usize;
        let mut off = crate::constants::MACHO64_HEADER_SIZE;
        for _ in 0..ncmds {
            let cmd = u32::from_le_bytes(macho[off..off + 4].try_into().unwrap());
            let cmdsize = u32::from_le_bytes(macho[off + 4..off + 8].try_into().unwrap()) as usize;
            if cmd == LC_MAIN {
                return u64::from_le_bytes(macho[off + 8..off + 16].try_into().unwrap());
            }
            off += cmdsize;
        }
        panic!("no LC_MAIN in linked output");
    }

    fn read_u64_at(bytes: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap())
    }

    fn read_u32_at(bytes: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
    }

    /// RUE-131 item 2 (ELF): LOCAL symbols with the same name in different
    /// objects must each resolve within their own object. A single name-keyed
    /// symbol map let one object's "helper" shadow the other's.
    #[test]
    fn test_local_symbols_resolve_within_object_elf() {
        let abs64_at_8 = |symbol_index| {
            vec![Relocation {
                offset: 8,
                symbol_index,
                rel_type: RelocationType::Abs64,
                addend: 0,
            }]
        };
        // Both objects define a LOCAL "helper" — at offset 16 in obj0, at
        // offset 4 in obj1 — and each takes its own helper's address.
        let obj0 = make_obj(
            crate::elf::ElfMachine::X86_64,
            vec![text_section(".text", vec![0u8; 24], abs64_at_8(1))],
            vec![
                sym("main", Some(0), 0, SymbolBinding::Global),
                sym("helper", Some(0), 16, SymbolBinding::Local),
            ],
        );
        let obj1 = make_obj(
            crate::elf::ElfMachine::X86_64,
            vec![text_section(".text", vec![0u8; 24], abs64_at_8(1))],
            vec![
                sym("second", Some(0), 0, SymbolBinding::Global),
                sym("helper", Some(0), 4, SymbolBinding::Local),
            ],
        );

        let mut linker = Linker::new(ELF_TARGET);
        linker.add_object(obj0).unwrap();
        linker.add_object(obj1).unwrap();
        let elf = linker.link("main").unwrap();

        // Code is written right after the (single, text-only) program header
        // in the FILE, but virtual addresses are always computed assuming the
        // maximum 3 program headers.
        let code_off = TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE;
        let code_vaddr = 0x400000 + (TEST_EHDR_SIZE + 3 * TEST_PHDR_SIZE) as u64;
        // obj0 .text at merged offset 0; obj1 .text at 32 (24 aligned to 16).
        assert_eq!(
            read_u64_at(&elf, code_off + 8),
            code_vaddr + 16,
            "obj0's relocation must bind to obj0's local helper"
        );
        assert_eq!(
            read_u64_at(&elf, code_off + 32 + 8),
            code_vaddr + 32 + 4,
            "obj1's relocation must bind to obj1's local helper, not obj0's"
        );
    }

    /// RUE-131 item 2 (Mach-O): same-named locals (e.g. each object's
    /// `l_anon` constants) must not shadow each other.
    #[test]
    fn test_local_symbols_resolve_within_object_macho() {
        use crate::macho::VM_BASE;
        let abs64_at_8 = |symbol_index| {
            vec![Relocation {
                offset: 8,
                symbol_index,
                rel_type: RelocationType::Aarch64Abs64,
                addend: 0,
            }]
        };
        let mut t0 = text_section("__text", vec![0u8; 24], abs64_at_8(1));
        t0.align = 4;
        let mut t1 = text_section("__text", vec![0u8; 24], abs64_at_8(1));
        t1.align = 4;
        let obj0 = make_obj(
            crate::elf::ElfMachine::Aarch64,
            vec![t0],
            vec![
                sym("main", Some(0), 0, SymbolBinding::Global),
                sym("helper", Some(0), 16, SymbolBinding::Local),
            ],
        );
        let obj1 = make_obj(
            crate::elf::ElfMachine::Aarch64,
            vec![t1],
            vec![
                sym("other", Some(0), 0, SymbolBinding::Global),
                sym("helper", Some(0), 4, SymbolBinding::Local),
            ],
        );

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj0).unwrap();
        linker.add_object(obj1).unwrap();
        let macho = linker.link("main").unwrap();

        // main is at code offset 0, so LC_MAIN's entryoff == the code's file
        // offset; code is mapped at VM_BASE + entryoff.
        let code_off = macho_entryoff(&macho) as usize;
        let code_vaddr = VM_BASE + code_off as u64;
        // obj0 __text at merged offset 0; obj1 __text at 24 (already 4-aligned).
        assert_eq!(
            read_u64_at(&macho, code_off + 8),
            code_vaddr + 16,
            "obj0's relocation must bind to obj0's local helper"
        );
        assert_eq!(
            read_u64_at(&macho, code_off + 24 + 8),
            code_vaddr + 24 + 4,
            "obj1's relocation must bind to obj1's local helper, not obj0's"
        );
    }

    /// RUE-131 item 11: a relocation in a rodata section must be applied on
    /// the Mach-O path (rodata relocations used to not be collected at all,
    /// and ARM64_RELOC_UNSIGNED wasn't handled).
    #[test]
    fn test_macho_rodata_relocation_applied() {
        use crate::macho::VM_BASE;
        let text = {
            let mut s = text_section("__text", vec![0xC0, 0x03, 0x5F, 0xD6], vec![]);
            s.align = 4;
            s
        };
        let rodata = Section {
            name: "__TEXT,__const".into(),
            data: vec![0u8; 8], // unrelocated pointer slot
            size: 8,
            flags: SectionFlags::ALLOC,
            relocations: vec![Relocation {
                offset: 0,
                symbol_index: 0, // -> main
                rel_type: RelocationType::Aarch64Abs64,
                addend: 0,
            }],
            align: 8,
        };
        let obj = make_obj(
            crate::elf::ElfMachine::Aarch64,
            vec![text, rodata],
            vec![sym("main", Some(0), 0, SymbolBinding::Global)],
        );

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj).unwrap();
        let macho = linker.link("main").unwrap();

        let code_off = macho_entryoff(&macho) as usize;
        // rodata is merged into the text buffer after the 4-byte code,
        // aligned to 8.
        let slot = read_u64_at(&macho, code_off + 8);
        assert_eq!(
            slot,
            VM_BASE + code_off as u64,
            "Abs64 slot in rodata must hold main's address (was silently dropped)"
        );
    }

    /// RUE-131 items 3 & 8 through link_macho itself: with __TEXT larger than
    /// one page, data symbols must be placed where __DATA actually lands (not
    /// the old hardcoded VM_BASE + one page), and patch sites in __DATA must
    /// be patched in the data buffer (not bounds-checked/patched against
    /// merged_text).
    #[test]
    fn test_macho_multipage_text_and_data_relocs() {
        use crate::macho::{PAGE_SIZE, VM_BASE};

        // > one page of code so __TEXT spans two pages.
        let code_len = PAGE_SIZE as usize + 0x100;
        let mut code = vec![0u8; code_len];
        // ret at the start (content is irrelevant; never executed)
        code[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]);
        let text = {
            let mut s = text_section(
                "__text",
                code,
                vec![Relocation {
                    offset: 16, // pointer slot in text -> gvar (in __DATA)
                    symbol_index: 1,
                    rel_type: RelocationType::Aarch64Abs64,
                    addend: 0,
                }],
            );
            s.align = 4;
            s
        };
        let data = Section {
            name: "__DATA,__data".into(),
            data: vec![0u8; 16],
            size: 16,
            flags: SectionFlags::ALLOC | SectionFlags::WRITE,
            relocations: vec![Relocation {
                offset: 0, // pointer slot in data -> main (in __TEXT)
                symbol_index: 0,
                rel_type: RelocationType::Aarch64Abs64,
                addend: 0,
            }],
            align: 8,
        };
        let obj = make_obj(
            crate::elf::ElfMachine::Aarch64,
            vec![text, data],
            vec![
                sym("main", Some(0), 0, SymbolBinding::Global),
                sym("gvar", Some(1), 8, SymbolBinding::Global),
            ],
        );

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj).unwrap();
        let macho = linker.link("main").unwrap();

        let code_off = macho_entryoff(&macho) as usize;
        let segments = macho_segments(&macho);
        let (_, text_vmaddr, _, text_filesize) = segments
            .iter()
            .find(|(n, ..)| n == "__TEXT")
            .cloned()
            .expect("__TEXT segment");
        let (_, data_vmaddr, data_fileoff, _) = segments
            .iter()
            .find(|(n, ..)| n == "__DATA")
            .cloned()
            .expect("__DATA segment");

        assert_eq!(text_vmaddr, VM_BASE);
        assert!(
            text_filesize >= (code_off + code_len) as u64,
            "__TEXT must contain headers + all the code"
        );
        assert!(
            data_vmaddr >= VM_BASE + 2 * PAGE_SIZE,
            "__DATA must start after the (multi-page) __TEXT, not at the \
             hardcoded second page (got {data_vmaddr:#x})"
        );

        // Text-side slot points at gvar inside __DATA.
        assert_eq!(
            read_u64_at(&macho, code_off + 16),
            data_vmaddr + 8,
            "text slot must hold gvar's real address in __DATA"
        );
        // Data-side slot points back at main.
        assert_eq!(
            read_u64_at(&macho, data_fileoff as usize),
            VM_BASE + code_off as u64,
            "data slot must be patched in the DATA buffer"
        );
    }

    /// RUE-131 item 7: a relocation against an unresolved symbol must be a
    /// link error on the Mach-O path (it was silently skipped, leaving a
    /// garbage instruction for runtime).
    #[test]
    fn test_macho_unresolved_symbol_errors() {
        let text = {
            let mut s = text_section(
                "__text",
                vec![
                    0x00, 0x00, 0x00, 0x94, // bl <nope>
                    0xC0, 0x03, 0x5F, 0xD6, // ret
                ],
                vec![Relocation {
                    offset: 0,
                    symbol_index: 1,
                    rel_type: RelocationType::Call26,
                    addend: 0,
                }],
            );
            s.align = 4;
            s
        };
        let obj = make_obj(
            crate::elf::ElfMachine::Aarch64,
            vec![text],
            vec![
                sym("main", Some(0), 0, SymbolBinding::Global),
                sym("nope", None, 0, SymbolBinding::Global), // undefined
            ],
        );

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj).unwrap();
        let result = linker.link("main");
        assert!(
            matches!(result, Err(LinkError::UndefinedSymbol(ref s)) if s.contains("nope")),
            "unresolved symbol must error (was silently skipped), got: {:?}",
            result
        );
    }

    /// RUE-131 item 9 parity: an undefined WEAK symbol resolves to 0 on the
    /// Mach-O path too, instead of erroring.
    #[test]
    fn test_macho_undefined_weak_resolves_to_zero() {
        let text = {
            let mut s = text_section(
                "__text",
                vec![0u8; 16],
                vec![Relocation {
                    offset: 8,
                    symbol_index: 1,
                    rel_type: RelocationType::Aarch64Abs64,
                    addend: 0,
                }],
            );
            s.align = 4;
            s
        };
        let obj = make_obj(
            crate::elf::ElfMachine::Aarch64,
            vec![text],
            vec![
                sym("main", Some(0), 0, SymbolBinding::Global),
                sym("optional_hook", None, 0, SymbolBinding::Weak),
            ],
        );

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj).unwrap();
        let macho = linker.link("main").unwrap();
        let code_off = macho_entryoff(&macho) as usize;
        assert_eq!(
            read_u64_at(&macho, code_off + 8),
            0,
            "undefined weak symbol must resolve to address 0"
        );
    }

    /// RUE-131 item 5a: ARM64_RELOC_PAGEOFF12 on a load/store must scale the
    /// page offset by the access size; on an ADD it stays unscaled. Writing
    /// the raw byte offset into an LDR multiplies the displacement by the
    /// access size at runtime.
    #[test]
    fn test_macho_pageoff12_scaled_for_loads() {
        use crate::macho::{PAGE_SIZE, VM_BASE};

        // ldr x0, [x8, #imm]  (64-bit load, imm12 scaled by 8)
        // add x0, x0, #imm    (unscaled)
        // ret
        let text = {
            let mut s = text_section(
                "__text",
                vec![
                    0x00, 0x01, 0x40, 0xF9, // ldr x0, [x8]
                    0x00, 0x00, 0x00, 0x91, // add x0, x0, #0
                    0xC0, 0x03, 0x5F, 0xD6, // ret
                ],
                vec![
                    Relocation {
                        offset: 0,
                        symbol_index: 1,
                        rel_type: RelocationType::AddLo12,
                        addend: 0,
                    },
                    Relocation {
                        offset: 4,
                        symbol_index: 1,
                        rel_type: RelocationType::AddLo12,
                        addend: 0,
                    },
                ],
            );
            s.align = 4;
            s
        };
        let data = Section {
            name: "__DATA,__data".into(),
            data: vec![0u8; 16],
            size: 16,
            flags: SectionFlags::ALLOC | SectionFlags::WRITE,
            relocations: vec![],
            align: 8,
        };
        let obj = make_obj(
            crate::elf::ElfMachine::Aarch64,
            vec![text, data],
            vec![
                sym("main", Some(0), 0, SymbolBinding::Global),
                sym("gvar", Some(1), 8, SymbolBinding::Global),
            ],
        );

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj).unwrap();
        let macho = linker.link("main").unwrap();

        let code_off = macho_entryoff(&macho) as usize;
        let segments = macho_segments(&macho);
        let (_, data_vmaddr, ..) = segments
            .iter()
            .find(|(n, ..)| n == "__DATA")
            .cloned()
            .expect("__DATA segment");
        // __DATA lands on a page boundary, so gvar's low 12 bits are its
        // section offset.
        assert_eq!(data_vmaddr % PAGE_SIZE, 0);
        assert!(data_vmaddr > VM_BASE);
        let lo12 = ((data_vmaddr + 8) & 0xFFF) as u32;
        assert_eq!(lo12, 8);

        let ldr = read_u32_at(&macho, code_off);
        assert_eq!(
            ldr,
            0xF9400100 | ((lo12 >> 3) << 10),
            "LDR imm12 must be the byte offset scaled by the 8-byte access size"
        );
        let add = read_u32_at(&macho, code_off + 4);
        assert_eq!(
            add,
            0x91000000 | (lo12 << 10),
            "ADD imm12 must be the unscaled byte offset"
        );
    }

    /// RUE-131 item 2, end-to-end through emit → parse → link_macho: every
    /// object emits its own LOCAL `.rodata.str0` symbol, so with name-keyed
    /// resolution the second object's string references bound to the FIRST
    /// object's string. Each ADD's imm12 must address its own object's string.
    #[test]
    fn test_macho_full_cycle_same_named_string_locals() {
        use crate::macho::VM_BASE;
        let make = |name: &str, s: &str| {
            ObjectBuilder::new(Target::Aarch64Macos, name)
                .code(vec![
                    0x00, 0x00, 0x00, 0x90, // adrp x0, str@PAGE
                    0x00, 0x00, 0x00, 0x91, // add x0, x0, str@PAGEOFF
                    0xC0, 0x03, 0x5F, 0xD6, // ret
                ])
                .strings(vec![s.to_string()])
                .relocation(CodeRelocation {
                    offset: 0,
                    symbol: ".rodata.str0".into(),
                    rel_type: RelocationType::AdrpPage21,
                    addend: 0,
                })
                .relocation(CodeRelocation {
                    offset: 4,
                    symbol: ".rodata.str0".into(),
                    rel_type: RelocationType::AddLo12,
                    addend: 0,
                })
                .build()
        };

        let obj0 = ObjectFile::parse(&make("main", "AAAA")).unwrap();
        let obj1 = ObjectFile::parse(&make("other", "BBBB")).unwrap();

        let mut linker = Linker::new(Target::Aarch64Macos);
        linker.add_object(obj0).unwrap();
        linker.add_object(obj1).unwrap();
        let macho = linker.link("main").unwrap();

        let code_off = macho_entryoff(&macho) as usize;
        // Merged layout: 12 bytes code per object (24 total), then obj0's
        // rodata at 24 (4 bytes "AAAA"), then obj1's rodata at 32 (8-aligned).
        assert_eq!(&macho[code_off + 24..code_off + 28], b"AAAA");
        assert_eq!(&macho[code_off + 32..code_off + 36], b"BBBB");

        // VM_BASE is page-aligned, so each string's low 12 address bits are
        // (code file offset + rodata offset) & 0xFFF.
        assert_eq!(VM_BASE & 0xFFF, 0);
        let lo12_of = |rodata_off: usize| ((code_off + rodata_off) & 0xFFF) as u32;
        let imm12_of = |insn: u32| (insn >> 10) & 0xFFF;

        let add0 = read_u32_at(&macho, code_off + 4);
        assert_eq!(
            imm12_of(add0),
            lo12_of(24),
            "obj0's ADD must address obj0's string"
        );
        let add1 = read_u32_at(&macho, code_off + 12 + 4);
        assert_eq!(
            imm12_of(add1),
            lo12_of(32),
            "obj1's ADD must address obj1's string, not obj0's"
        );
    }

    /// RUE-131 item 11: archive linking where resolving one member's
    /// undefined symbols pulls in a second member.
    #[test]
    fn test_archive_selects_multiple_members() {
        // main calls f (archive member 1), f calls g (archive member 2)
        let main_bytes = ObjectBuilder::new(ELF_TARGET, "main")
            .code(vec![0xE8, 0x00, 0x00, 0x00, 0x00, 0xC3])
            .relocation(CodeRelocation {
                offset: 1,
                symbol: "f".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .build();
        let f_bytes = ObjectBuilder::new(ELF_TARGET, "f")
            .code(vec![
                0xB8, 0x11, 0x00, 0x00, 0x00, // mov eax, 0x11 (marker)
                0xE8, 0x00, 0x00, 0x00, 0x00, // call g
                0xC3,
            ])
            .relocation(CodeRelocation {
                offset: 6,
                symbol: "g".into(),
                rel_type: RelocationType::Pc32,
                addend: -4,
            })
            .build();
        let g_bytes = ObjectBuilder::new(ELF_TARGET, "g")
            .code(vec![
                0xB8, 0x22, 0x00, 0x00, 0x00, // mov eax, 0x22 (marker)
                0xC3,
            ])
            .build();

        let archive = Archive {
            objects: vec![
                ObjectFile::parse(&f_bytes).unwrap(),
                ObjectFile::parse(&g_bytes).unwrap(),
            ],
        };

        let mut linker = Linker::new(ELF_TARGET);
        linker
            .add_object(ObjectFile::parse(&main_bytes).unwrap())
            .unwrap();
        linker.add_archive(archive).unwrap();
        let elf = linker.link("main").expect("both members must be selected");

        // Both members' marker instructions must be present in the output.
        let has_f = elf.windows(5).any(|w| w == [0xB8, 0x11, 0x00, 0x00, 0x00]);
        let has_g = elf.windows(5).any(|w| w == [0xB8, 0x22, 0x00, 0x00, 0x00]);
        assert!(has_f, "archive member defining f must be linked");
        assert!(
            has_g,
            "archive member defining g must be linked (pulled in by f)"
        );
    }
}
