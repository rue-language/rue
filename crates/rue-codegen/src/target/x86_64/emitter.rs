use crate::util::align_to_16;
use rue_ir::pir::Label;
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};
use std::collections::HashMap;

/// ELF section types
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SectionType {
    Text,   // .text - executable code
    Data,   // .data - initialized writable data
    Bss,    // .bss - uninitialized writable data
    Rodata, // .rodata - read-only data
}

/// Information about a section
#[derive(Debug, Clone)]
pub struct SectionInfo {
    pub section_type: SectionType,
    pub data: Vec<u8>,
    pub labels: HashMap<u32, usize>, // label_id -> offset within section
}

/// Type alias for complex return type from get_sections_and_symbols
type SectionsAndSymbols<'a> = (
    &'a HashMap<SectionType, SectionInfo>,
    HashMap<String, (SectionType, usize)>,
);

/// x86-64 machine code emitter
/// Converts X8664Instr to raw bytes
pub struct X86Emitter {
    // Section-aware state
    current_section: SectionType,
    sections: HashMap<SectionType, SectionInfo>,

    // Global state
    label_positions: HashMap<u32, (SectionType, usize)>, // label_id -> (section, offset)
    pending_fixups: Vec<(SectionType, usize, LabelRef)>, // (section, offset, target)
    function_labels: HashMap<String, Label>,
    runtime_label_count: u32,
}

impl X86Emitter {
    pub fn new() -> Self {
        let mut sections = HashMap::new();
        sections.insert(
            SectionType::Text,
            SectionInfo {
                section_type: SectionType::Text,
                data: Vec::new(),
                labels: HashMap::new(),
            },
        );

        Self {
            current_section: SectionType::Text,
            sections,
            label_positions: HashMap::new(),
            pending_fixups: Vec::new(),
            function_labels: HashMap::new(),
            runtime_label_count: 0,
        }
    }

    pub fn set_function_labels(
        &mut self,
        labels: HashMap<String, Label>,
        runtime_label_count: u32,
    ) {
        // We'll use this to map function names to their label IDs
        self.function_labels = labels;
        self.runtime_label_count = runtime_label_count;
    }

    /// Emit machine instructions to bytes
    pub fn emit_all(&mut self, instructions: &[X8664Instr]) -> Result<Vec<u8>, String> {
        // First pass: emit code and record label positions
        for instr in instructions {
            self.emit_instruction(instr)?;
        }

        // Second pass: fix up label references
        self.resolve_fixups()?;

        // Return .text section data
        let text_data = &self.sections.get(&SectionType::Text).unwrap().data;
        Ok(text_data.clone())
    }

    /// Get the current section's data buffer for writing
    fn current_section_data(&mut self) -> &mut Vec<u8> {
        &mut self.sections.get_mut(&self.current_section).unwrap().data
    }

    fn emit_instruction(&mut self, instr: &X8664Instr) -> Result<(), String> {
        match instr {
            X8664Instr::MovRR { dest, src } => {
                self.emit_rex_if_needed(true, Some(src), Some(dest));
                self.current_section_data().push(0x89); // MOV r/m64, r64
                let modrm = 0xc0 | (self.register_code(src) << 3) | self.register_code(dest);
                self.current_section_data().push(modrm);
            }

            X8664Instr::MovRI32 { dest, imm } => {
                self.emit_rex_if_needed(true, None, Some(dest));
                self.current_section_data().push(0xc7); // MOV r/m64, imm32
                let modrm = 0xc0 | self.register_code(dest);
                self.current_section_data().push(modrm);
                self.current_section_data()
                    .extend_from_slice(&imm.to_le_bytes());
            }

            X8664Instr::MovRI64 { dest, imm } => {
                let reg_code = self.register_code(dest);
                if self.needs_rex_b(dest) {
                    self.current_section_data().push(0x49); // REX.WB
                    self.current_section_data().push(0xb8 + reg_code);
                } else {
                    self.current_section_data().push(0x48); // REX.W
                    self.current_section_data().push(0xb8 + reg_code);
                }
                self.current_section_data()
                    .extend_from_slice(&imm.to_le_bytes());
            }

            X8664Instr::MovRM { dest, base, offset } => {
                self.emit_mem_op(0x8b, dest, base, *offset, false)?;
            }

            X8664Instr::MovMR { base, offset, src } => {
                self.emit_mem_op(0x89, src, base, *offset, false)?;
            }
            X8664Instr::MovMR8 { base, offset, src } => {
                self.emit_mem_op(0x88, src, base, *offset, true)?;
            }
            X8664Instr::MovRM8 { dest, base, offset } => {
                self.emit_mem_op(0x8a, dest, base, *offset, true)?;
            }

            X8664Instr::AddRR { dest, src } => {
                self.emit_rex_if_needed(true, Some(src), Some(dest));
                self.current_section_data().push(0x01); // ADD r/m64, r64
                let modrm = 0xc0 | (self.register_code(src) << 3) | self.register_code(dest);
                self.current_section_data().push(modrm);
            }

            X8664Instr::AddRI { dest, imm } => {
                self.emit_rex_if_needed(true, None, Some(dest));
                if *imm >= -128 && *imm <= 127 {
                    self.current_section_data().push(0x83); // ADD r/m64, imm8
                    let modrm = 0xc0 | self.register_code(dest);
                    self.current_section_data().push(modrm);
                    self.current_section_data().push(*imm as u8);
                } else {
                    self.current_section_data().push(0x81); // ADD r/m64, imm32
                    let modrm = 0xc0 | self.register_code(dest);
                    self.current_section_data().push(modrm);
                    self.current_section_data()
                        .extend_from_slice(&imm.to_le_bytes());
                }
            }

            X8664Instr::SubRR { dest, src } => {
                self.emit_rex_if_needed(true, Some(src), Some(dest));
                self.current_section_data().push(0x29); // SUB r/m64, r64
                let modrm = 0xc0 | (self.register_code(src) << 3) | self.register_code(dest);
                self.current_section_data().push(modrm);
            }

            X8664Instr::SubRI { dest, imm } => {
                self.emit_rex_if_needed(true, None, Some(dest));
                if *imm >= -128 && *imm <= 127 {
                    self.current_section_data().push(0x83); // SUB r/m64, imm8
                    let modrm = 0xe8 | self.register_code(dest); // /5
                    self.current_section_data().push(modrm);
                    self.current_section_data().push(*imm as u8);
                } else {
                    self.current_section_data().push(0x81); // SUB r/m64, imm32
                    let modrm = 0xe8 | self.register_code(dest); // /5
                    self.current_section_data().push(modrm);
                    self.current_section_data()
                        .extend_from_slice(&imm.to_le_bytes());
                }
            }

            X8664Instr::AndRR { dest, src } => {
                self.emit_rex_if_needed(true, Some(src), Some(dest));
                self.current_section_data().push(0x21); // AND r/m64, r64
                let modrm = 0xc0 | (self.register_code(src) << 3) | self.register_code(dest);
                self.current_section_data().push(modrm);
            }
            X8664Instr::AndRI { dest, imm } => {
                self.emit_rex_if_needed(true, None, Some(dest));
                if *imm >= -128 && *imm <= 127 {
                    self.current_section_data().push(0x83); // AND r/m64, imm8
                    let modrm = 0xe0 | self.register_code(dest); // /4 for AND
                    self.current_section_data().push(modrm);
                    self.current_section_data().push(*imm as u8);
                } else {
                    self.current_section_data().push(0x81); // AND r/m64, imm32
                    let modrm = 0xe0 | self.register_code(dest); // /4 for AND
                    self.current_section_data().push(modrm);
                    self.current_section_data()
                        .extend_from_slice(&imm.to_le_bytes());
                }
            }

            X8664Instr::Shl { dest, count: _ } => {
                // count must be in RCX
                self.emit_rex_if_needed(true, None, Some(dest));
                self.current_section_data().push(0xd3); // SHL r/m64, CL
                let modrm = 0xe0 | self.register_code(dest); // /4
                self.current_section_data().push(modrm);
            }

            X8664Instr::Sar { dest, count: _ } => {
                // count must be in RCX
                self.emit_rex_if_needed(true, None, Some(dest));
                self.current_section_data().push(0xd3); // SAR r/m64, CL
                let modrm = 0xf8 | self.register_code(dest); // /7
                self.current_section_data().push(modrm);
            }

            X8664Instr::ImulRR { dest, src } => {
                self.emit_rex_if_needed(true, Some(dest), Some(src));
                self.current_section_data().push(0x0f);
                self.current_section_data().push(0xaf); // IMUL r64, r/m64
                let modrm = 0xc0 | (self.register_code(dest) << 3) | self.register_code(src);
                self.current_section_data().push(modrm);
            }

            X8664Instr::ImulRI { dest, imm } => {
                self.emit_rex_if_needed(true, Some(dest), Some(dest));
                if *imm >= -128 && *imm <= 127 {
                    self.current_section_data().push(0x6b); // IMUL r64, r/m64, imm8
                    let modrm = 0xc0 | (self.register_code(dest) << 3) | self.register_code(dest);
                    self.current_section_data().push(modrm);
                    self.current_section_data().push(*imm as u8);
                } else {
                    self.current_section_data().push(0x69); // IMUL r64, r/m64, imm32
                    let modrm = 0xc0 | (self.register_code(dest) << 3) | self.register_code(dest);
                    self.current_section_data().push(modrm);
                    self.current_section_data()
                        .extend_from_slice(&imm.to_le_bytes());
                }
            }

            X8664Instr::Idiv { divisor } => {
                self.emit_rex_if_needed(true, None, Some(divisor));
                self.current_section_data().push(0xf7); // IDIV r/m64
                let modrm = 0xf8 | self.register_code(divisor); // /7
                self.current_section_data().push(modrm);
            }

            X8664Instr::Cqo => {
                self.current_section_data().push(0x48); // REX.W
                self.current_section_data().push(0x99); // CQO
            }

            X8664Instr::CmpRR { left, right } => {
                self.emit_rex_if_needed(true, Some(right), Some(left));
                self.current_section_data().push(0x39); // CMP r/m64, r64
                let modrm = 0xc0 | (self.register_code(right) << 3) | self.register_code(left);
                self.current_section_data().push(modrm);
            }

            X8664Instr::CmpRI { reg, imm } => {
                self.emit_rex_if_needed(true, None, Some(reg));
                if *imm >= -128 && *imm <= 127 {
                    self.current_section_data().push(0x83); // CMP r/m64, imm8
                    let modrm = 0xf8 | self.register_code(reg); // /7
                    self.current_section_data().push(modrm);
                    self.current_section_data().push(*imm as u8);
                } else {
                    self.current_section_data().push(0x81); // CMP r/m64, imm32
                    let modrm = 0xf8 | self.register_code(reg); // /7
                    self.current_section_data().push(modrm);
                    self.current_section_data()
                        .extend_from_slice(&imm.to_le_bytes());
                }
            }

            X8664Instr::SetCC { dest, cc } => {
                let opcode = match cc {
                    ConditionCode::Equal => 0x94,        // SETE
                    ConditionCode::NotEqual => 0x95,     // SETNE
                    ConditionCode::Less => 0x9c,         // SETL
                    ConditionCode::LessEqual => 0x9e,    // SETLE
                    ConditionCode::Greater => 0x9f,      // SETG
                    ConditionCode::GreaterEqual => 0x9d, // SETGE
                    ConditionCode::Below => 0x92,        // SETB
                    ConditionCode::BelowEqual => 0x96,   // SETBE
                    ConditionCode::Above => 0x97,        // SETA
                    ConditionCode::AboveEqual => 0x93,   // SETAE
                };

                // Need a REX prefix when the destination is r8-r15 so that
                // the low-byte form (r8b…r15b) is addressable.
                // W=0 (8-bit op), R=0, X=0, B = dest.needs_rex()
                self.emit_rex_if_needed(false, None, Some(dest));
                self.current_section_data().push(0x0f);
                self.current_section_data().push(opcode);
                // SetCC uses /0 encoding, so reg field must be 000
                let modrm = 0xc0 | self.register_code(dest);
                self.current_section_data().push(modrm);
            }

            X8664Instr::Movzx { dest, src } => {
                // While REX.W=1 is not canonical for MOVZX (should use REX.W=0),
                // we keep it for compatibility as some code may depend on this behavior
                self.emit_rex_if_needed(true, Some(dest), Some(src));
                self.current_section_data().push(0x0f);
                self.current_section_data().push(0xb6); // MOVZX r64, r/m8 (with REX.W=1)
                let modrm = 0xc0 | (self.register_code(dest) << 3) | self.register_code(src);
                self.current_section_data().push(modrm);
            }

            X8664Instr::Movsxd { dest, src } => {
                self.emit_rex_if_needed(true, Some(dest), Some(src));
                self.current_section_data().push(0x63); // MOVSXD r64, r/m32
                let modrm = 0xc0 | (self.register_code(dest) << 3) | self.register_code(src);
                self.current_section_data().push(modrm);
            }

            X8664Instr::Push { reg } => {
                let reg_code = self.register_code(reg);
                if self.needs_rex_b(reg) {
                    self.current_section_data().push(0x41); // REX.B
                }
                self.current_section_data().push(0x50 + reg_code);
            }

            X8664Instr::Pop { reg } => {
                let reg_code = self.register_code(reg);
                if self.needs_rex_b(reg) {
                    self.current_section_data().push(0x41); // REX.B
                }
                self.current_section_data().push(0x58 + reg_code);
            }

            X8664Instr::Call { target } => {
                // For now, we only support relative calls
                self.current_section_data().push(0xe8); // CALL rel32
                let fixup_pos = self.current_section_data().len();
                self.current_section_data().extend_from_slice(&[0, 0, 0, 0]); // Placeholder
                self.pending_fixups.push((
                    self.current_section.clone(),
                    fixup_pos,
                    LabelRef::Global(target.clone()),
                ));
            }

            X8664Instr::Ret => {
                self.current_section_data().push(0xc3); // RET
            }

            X8664Instr::Jmp { target } => {
                self.current_section_data().push(0xe9); // JMP rel32
                let fixup_pos = self.current_section_data().len();
                self.current_section_data().extend_from_slice(&[0, 0, 0, 0]); // Placeholder
                self.pending_fixups
                    .push((self.current_section.clone(), fixup_pos, target.clone()));
            }

            X8664Instr::JmpCC { cc, target } => {
                let opcode = match cc {
                    ConditionCode::Equal => 0x84,        // JE
                    ConditionCode::NotEqual => 0x85,     // JNE
                    ConditionCode::Less => 0x8c,         // JL
                    ConditionCode::LessEqual => 0x8e,    // JLE
                    ConditionCode::Greater => 0x8f,      // JG
                    ConditionCode::GreaterEqual => 0x8d, // JGE
                    ConditionCode::Below => 0x82,        // JB
                    ConditionCode::BelowEqual => 0x86,   // JBE
                    ConditionCode::Above => 0x87,        // JA
                    ConditionCode::AboveEqual => 0x83,   // JAE
                };

                self.current_section_data().push(0x0f);
                self.current_section_data().push(opcode);
                let fixup_pos = self.current_section_data().len();
                self.current_section_data().extend_from_slice(&[0, 0, 0, 0]); // Placeholder
                self.pending_fixups
                    .push((self.current_section.clone(), fixup_pos, target.clone()));
            }

            X8664Instr::Label { id } => {
                let current_pos = self.sections.get(&self.current_section).unwrap().data.len();
                self.label_positions
                    .insert(*id, (self.current_section.clone(), current_pos));

                // Update section labels
                self.sections
                    .get_mut(&self.current_section)
                    .unwrap()
                    .labels
                    .insert(*id, current_pos);
            }

            X8664Instr::Syscall => {
                self.current_section_data().push(0x0f);
                self.current_section_data().push(0x05);
            }

            X8664Instr::EnterFrame => {
                // push rbp
                self.current_section_data().push(0x55);
                // mov rbp, rsp
                self.current_section_data().push(0x48);
                self.current_section_data().push(0x89);
                self.current_section_data().push(0xe5);
            }

            X8664Instr::LeaveFrame => {
                // mov rsp, rbp
                self.current_section_data().push(0x48);
                self.current_section_data().push(0x89);
                self.current_section_data().push(0xec);
                // pop rbp
                self.current_section_data().push(0x5d);
            }

            X8664Instr::AllocStack { size } => {
                if *size == 0 {
                    return Ok(());
                }

                // Align stack allocation to maintain 16-byte alignment
                let aligned_size = align_to_16(*size as u64) as u32;

                self.emit_rex_if_needed(true, None, Some(&X86Register::Rsp));
                if aligned_size <= 127 {
                    self.current_section_data().push(0x83); // SUB r/m64, imm8
                    self.current_section_data().push(0xec); // /5 RSP
                    self.current_section_data().push(aligned_size as u8);
                } else {
                    self.current_section_data().push(0x81); // SUB r/m64, imm32
                    self.current_section_data().push(0xec); // /5 RSP
                    self.current_section_data()
                        .extend_from_slice(&aligned_size.to_le_bytes());
                }
            }

            X8664Instr::LeaLabel { dest, label } => {
                // lea dest, [rip + offset]
                let reg_code = self.register_code(dest);
                self.emit_rex_if_needed(true, Some(dest), None);
                self.current_section_data().push(0x8d); // LEA
                self.current_section_data().push((reg_code << 3) | 0x05); // ModRM: mod=00, reg=dest, rm=101 (RIP+disp32)

                // Record fixup for the label
                let fixup_pos = self.current_section_data().len();
                self.current_section_data().extend_from_slice(&[0, 0, 0, 0]); // Placeholder
                self.pending_fixups.push((
                    self.current_section.clone(),
                    fixup_pos,
                    LabelRef::Global(label.clone()),
                ));
            }

            X8664Instr::Cld => {
                // cld - Clear direction flag
                self.current_section_data().push(0xfc);
            }

            X8664Instr::RepStosb => {
                // rep stosb - Repeat store byte
                self.current_section_data().push(0xf3); // REP prefix
                self.current_section_data().push(0xaa); // STOSB
            }

            X8664Instr::XorRR { dest, src } => {
                self.emit_rex_if_needed(true, Some(src), Some(dest));
                self.current_section_data().push(0x31); // XOR r/m64, r64
                let modrm = 0xc0 | (self.register_code(src) << 3) | self.register_code(dest);
                self.current_section_data().push(modrm);
            }

            X8664Instr::Ud2 => {
                // UD2 - undefined instruction
                self.current_section_data().push(0x0f);
                self.current_section_data().push(0x0b);
            }
            X8664Instr::Section { name } => {
                let new_section = match name.as_str() {
                    ".text" => SectionType::Text,
                    ".data" => SectionType::Data,
                    ".bss" => SectionType::Bss,
                    ".rodata" => SectionType::Rodata,
                    _ => return Err(format!("Unsupported section: {name}")),
                };

                // Switch to new section
                self.current_section = new_section.clone();

                // Ensure section exists
                if !self.sections.contains_key(&new_section) {
                    self.sections.insert(
                        new_section.clone(),
                        SectionInfo {
                            section_type: new_section.clone(),
                            data: Vec::new(),
                            labels: HashMap::new(),
                        },
                    );
                }
            }

            X8664Instr::DataBytes { bytes } => {
                // Emit raw bytes directly
                self.current_section_data().extend_from_slice(bytes);
            }

            X8664Instr::ReserveBytes { count } => {
                match self.current_section {
                    SectionType::Bss => {
                        // In .bss, just track the size - no actual data in file
                        // For BSS sections, we use the data.len() to track the virtual size
                        // but we don't store actual bytes
                        let section = self.sections.get_mut(&SectionType::Bss).unwrap();
                        // Extend data field to track size, but this data won't be written to file
                        section.data.resize(section.data.len() + *count as usize, 0);
                    }
                    _ => {
                        // In other sections, reserve actual zero bytes
                        let section = self.sections.get_mut(&self.current_section).unwrap();
                        section.data.resize(section.data.len() + *count as usize, 0);
                    }
                }
            }
            X8664Instr::RepMovsb => {
                // rep movsb
                self.current_section_data().extend_from_slice(&[0xF3, 0xA4]);
            }
            X8664Instr::Std => {
                // std - set direction flag
                self.current_section_data().push(0xFD);
            }
        }
        Ok(())
    }

    fn resolve_fixups(&mut self) -> Result<(), String> {
        for (section, fixup_pos, target) in &self.pending_fixups {
            let (target_section, target_pos) = match target {
                LabelRef::Local(id) => self
                    .label_positions
                    .get(id)
                    .cloned()
                    .ok_or_else(|| format!("Undefined label: {id}"))?,
                LabelRef::Global(name) => {
                    // Look up the function name to get its label ID
                    let label_id = self
                        .function_labels
                        .get(name)
                        .ok_or_else(|| format!("Unknown function: {name}"))?;

                    // Get the position of this label
                    let machine_id = label_id.to_machine_id(self.runtime_label_count);
                    self.label_positions
                        .get(&machine_id)
                        .cloned()
                        .ok_or_else(|| format!("Undefined label for function: {name}"))?
                }
            };

            // Calculate relative offset accounting for section layout in memory
            let offset = if section == &target_section {
                // Same section: simple relative addressing
                let current_pos = fixup_pos + 4; // Position after the offset
                (target_pos as i32) - (current_pos as i32)
            } else {
                // Cross-section: calculate based on expected memory layout
                self.calculate_cross_section_offset(
                    section,
                    *fixup_pos,
                    &target_section,
                    target_pos,
                )?
            };

            let offset_bytes = offset.to_le_bytes();
            let section_data = &mut self.sections.get_mut(section).unwrap().data;
            for (i, &byte) in offset_bytes.iter().enumerate() {
                section_data[fixup_pos + i] = byte;
            }
        }
        Ok(())
    }

    /// Calculate cross-section offset based on expected ELF memory layout
    fn calculate_cross_section_offset(
        &self,
        from_section: &SectionType,
        from_offset: usize,
        to_section: &SectionType,
        to_offset: usize,
    ) -> Result<i32, String> {
        // Calculate expected memory layout matching ELF writer
        // Base address for sections (matches ELF writer)
        const BASE_ADDR: u64 = 0x400000;
        const EHDR_SIZE: usize = 0x40;
        const PHDR_SIZE: usize = 0x38;

        // Calculate expected section offsets (matches ELF writer logic)
        // This must match the layout calculation in generate_elf_with_sections
        let program_header_count = if self.sections.contains_key(&SectionType::Bss)
            || self.sections.contains_key(&SectionType::Data)
        {
            2
        } else {
            1
        };
        let headers_size = EHDR_SIZE + (PHDR_SIZE * program_header_count);
        let mut current_file_offset = align_to_16(headers_size as u64) as usize;

        // Section layout: .text, .data, .bss (matches ELF writer)
        let mut section_addresses = HashMap::new();

        // .text section
        if let Some(text_section) = self.sections.get(&SectionType::Text) {
            section_addresses.insert(SectionType::Text, BASE_ADDR + current_file_offset as u64);
            current_file_offset += text_section.data.len();
            current_file_offset = align_to_16(current_file_offset as u64) as usize;
        }

        // .data section
        if let Some(data_section) = self.sections.get(&SectionType::Data) {
            section_addresses.insert(SectionType::Data, BASE_ADDR + current_file_offset as u64);
            current_file_offset += data_section.data.len();
            current_file_offset = align_to_16(current_file_offset as u64) as usize;
        }

        // .bss section (virtual address, no file space)
        if let Some(_bss_section) = self.sections.get(&SectionType::Bss) {
            section_addresses.insert(SectionType::Bss, BASE_ADDR + current_file_offset as u64);
        }

        // Calculate addresses
        let from_addr = section_addresses
            .get(from_section)
            .ok_or_else(|| format!("Section not found: {from_section:?}"))?
            + from_offset as u64
            + 4; // +4 for instruction pointer after the displacement

        let to_addr = section_addresses
            .get(to_section)
            .ok_or_else(|| format!("Section not found: {to_section:?}"))?
            + to_offset as u64;

        // Calculate relative offset
        let offset = (to_addr as i64) - (from_addr as i64);

        // Check if offset fits in i32
        if offset < i32::MIN as i64 || offset > i32::MAX as i64 {
            return Err(format!("Cross-section offset too large: {offset}"));
        }

        Ok(offset as i32)
    }

    /// Get the 3-bit register encoding for ModR/M or SIB bytes
    /// This returns the low 3 bits of the register number (0-7)
    /// For R8-R15, the high bit is encoded in the REX prefix
    fn register_code(&self, reg: &X86Register) -> u8 {
        match reg {
            X86Register::Rax => 0,
            X86Register::Rcx => 1,
            X86Register::Rdx => 2,
            X86Register::Rbx => 3,
            X86Register::Rsp => 4,
            X86Register::Rbp => 5,
            X86Register::Rsi => 6,
            X86Register::Rdi => 7,
            X86Register::R8 => 0,  // Requires REX.B/REX.R/REX.X
            X86Register::R9 => 1,  // Requires REX.B/REX.R/REX.X
            X86Register::R10 => 2, // Requires REX.B/REX.R/REX.X
            X86Register::R11 => 3, // Requires REX.B/REX.R/REX.X
            X86Register::R12 => 4, // Requires REX.B/REX.R/REX.X
            X86Register::R13 => 5, // Requires REX.B/REX.R/REX.X
            X86Register::R14 => 6, // Requires REX.B/REX.R/REX.X
            X86Register::R15 => 7, // Requires REX.B/REX.R/REX.X
        }
    }

    fn needs_rex_b(&self, reg: &X86Register) -> bool {
        reg.needs_rex()
    }

    fn needs_rex_r(&self, reg: &X86Register) -> bool {
        reg.needs_rex() // Same registers need REX.R when used in reg field
    }

    /// Emit REX prefix if needed for the given registers
    ///
    /// # Arguments
    /// * `w` - Whether to set REX.W (64-bit operand size)
    /// * `r_reg` - X86Register used in the reg field (affects REX.R)
    /// * `b_reg` - X86Register used in the r/m field (affects REX.B)
    ///
    /// # Important
    /// The order of r_reg and b_reg matters! For instructions like `add rax, rbx`:
    /// - r_reg is the source register (rbx)
    /// - b_reg is the destination register (rax)
    fn emit_rex_if_needed(
        &mut self,
        w: bool,
        r_reg: Option<&X86Register>,
        b_reg: Option<&X86Register>,
    ) {
        self.emit_rex_with_sib(w, r_reg, b_reg, None)
    }

    /// Emit REX prefix with support for SIB byte index register
    /// w: 64-bit operand size (REX.W)
    /// r_reg: X86Register in ModR/M reg field (REX.R)
    /// b_reg: X86Register in ModR/M r/m field or SIB base field (REX.B)
    /// x_reg: X86Register in SIB index field (REX.X)
    fn emit_rex_with_sib(
        &mut self,
        w: bool,
        r_reg: Option<&X86Register>,
        b_reg: Option<&X86Register>,
        x_reg: Option<&X86Register>,
    ) {
        let mut rex = 0x40;

        if w {
            rex |= 0x08; // REX.W
        }

        if let Some(reg) = r_reg {
            if self.needs_rex_r(reg) {
                rex |= 0x04; // REX.R
            }
        }

        if let Some(reg) = x_reg {
            if reg.needs_rex() {
                rex |= 0x02; // REX.X for SIB index
            }
        }

        if let Some(reg) = b_reg {
            if self.needs_rex_b(reg) {
                rex |= 0x01; // REX.B
            }
        }

        // For SetCC and other 8-bit operations on RSP, RBP, RSI, RDI,
        // we need REX even if it's just the base prefix to access the
        // low byte (SPL, BPL, SIL, DIL instead of AH, CH, DH, BH)
        if rex != 0x40
            || (!w
                && b_reg.is_some_and(|r| {
                    matches!(
                        r,
                        X86Register::Rsp | X86Register::Rbp | X86Register::Rsi | X86Register::Rdi
                    )
                }))
        {
            self.current_section_data().push(rex);
        }
    }

    /// Emit a memory operation (load or store) with proper ModR/M and SIB encoding
    ///
    /// # Arguments
    /// * `opcode` - The instruction opcode (e.g., 0x8B for MOV r64, r/m64)
    /// * `reg` - The register operand (source for stores, dest for loads)
    /// * `base` - The base register for memory addressing
    /// * `offset` - The displacement from the base register
    /// * `is_byte` - Whether this is a byte operation (affects REX prefix)
    fn emit_mem_op(
        &mut self,
        opcode: u8,
        reg: &X86Register,
        base: &X86Register,
        offset: i32,
        is_byte: bool,
    ) -> Result<(), String> {
        // Emit REX prefix if needed
        if is_byte {
            // For byte operations, we need REX if using R8-R15 or accessing high byte regs
            if reg.needs_rex() || base.needs_rex() {
                let mut rex = 0x40;
                if reg.needs_rex() {
                    rex |= 0x04; // REX.R
                }
                if base.needs_rex() {
                    rex |= 0x01; // REX.B
                }
                self.current_section_data().push(rex);
            }
        } else {
            self.emit_rex_if_needed(true, Some(reg), Some(base));
        }

        // Emit opcode
        self.current_section_data().push(opcode);

        // Special handling for RSP/R12 which require SIB byte
        let base_code = self.register_code(base);
        let needs_sib = base_code == 4; // RSP or R12
        let needs_disp32_for_rbp = base_code == 5; // RBP or R13

        // Choose between mod values based on offset
        if offset == 0 && !needs_disp32_for_rbp {
            // ModR/M byte: mod=00 (no disp), reg=reg, r/m=base
            let modrm = (self.register_code(reg) << 3) | base_code;
            self.current_section_data().push(modrm);
            if needs_sib {
                // SIB byte: scale=00, index=100 (none), base=100 (RSP)
                self.current_section_data().push(0x24);
            }
        } else if (-128..=127).contains(&offset) && !(offset == 0 && needs_disp32_for_rbp) {
            // ModR/M byte: mod=01 (disp8), reg=reg, r/m=base
            let modrm = 0x40 | (self.register_code(reg) << 3) | base_code;
            self.current_section_data().push(modrm);
            if needs_sib {
                // SIB byte: scale=00, index=100 (none), base=100 (RSP)
                self.current_section_data().push(0x24);
            }
            self.current_section_data().push(offset as u8);
        } else {
            // ModR/M byte: mod=10 (disp32), reg=reg, r/m=base
            let modrm = 0x80 | (self.register_code(reg) << 3) | base_code;
            self.current_section_data().push(modrm);
            if needs_sib {
                // SIB byte: scale=00, index=100 (none), base=100 (RSP)
                self.current_section_data().push(0x24);
            }
            self.current_section_data()
                .extend_from_slice(&offset.to_le_bytes());
        }

        Ok(())
    }

    /// Get the emitted code and symbol table for ELF generation
    pub fn get_output(&self) -> (&[u8], HashMap<String, usize>) {
        let mut symbols = HashMap::new();

        // Convert local labels to symbols - only include .text section labels for compatibility
        for (id, (section, pos)) in &self.label_positions {
            if *section == SectionType::Text {
                symbols.insert(format!("L{id}"), *pos);
            }
        }

        // Add function names to symbol table
        for (name, label_id) in &self.function_labels {
            let machine_id = label_id.to_machine_id(self.runtime_label_count);
            if let Some((section, pos)) = self.label_positions.get(&machine_id) {
                if *section == SectionType::Text {
                    symbols.insert(name.clone(), *pos);
                }
            }
        }

        // Note: We do NOT add global symbols from fixups to the symbol table
        // because the fixup positions are where calls are made FROM, not where functions are located.
        // The actual function positions are already added above in the function_labels loop.

        // Return .text section data
        let text_data = &self.sections.get(&SectionType::Text).unwrap().data;
        (text_data, symbols)
    }

    /// Get all sections and their symbols (new interface for multi-section ELF generation)
    pub fn get_sections_and_symbols(&self) -> SectionsAndSymbols<'_> {
        let mut symbols = HashMap::new();

        // Convert local labels to symbols with section information
        for (id, (section, pos)) in &self.label_positions {
            symbols.insert(format!("L{id}"), (section.clone(), *pos));
        }

        // Add function names to symbol table with section information
        for (name, label_id) in &self.function_labels {
            let machine_id = label_id.to_machine_id(self.runtime_label_count);
            if let Some((section, pos)) = self.label_positions.get(&machine_id) {
                symbols.insert(name.clone(), (section.clone(), *pos));
            }
        }

        (&self.sections, symbols)
    }

    /// Get data section content and BSS size for ELF generation
    pub fn get_data_and_bss(&self) -> (&[u8], usize) {
        let data_section = self
            .sections
            .get(&SectionType::Data)
            .map(|s| s.data.as_slice())
            .unwrap_or(&[]);
        let bss_size = self
            .sections
            .get(&SectionType::Bss)
            .map(|s| s.data.len())
            .unwrap_or(0);
        (data_section, bss_size)
    }
}

impl Default for X86Emitter {
    fn default() -> Self {
        Self::new()
    }
}
