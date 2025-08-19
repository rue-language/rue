// Assembler that converts X8664Instr to AsmObject with relocations
//
// This module handles the conversion from high-level instructions to
// a linkable object format with proper symbol references and relocations.

use crate::linker::asm_object::{AsmObject, AsmObjectBuilder, RelocKind, SymBind};
use rue_ir::pir::Label;
use rue_target::{LabelRef, X86Register, X8664Instr};
use std::collections::HashMap;

/// Assembler that produces linkable objects
pub struct Assembler {
    builder: AsmObjectBuilder,
    label_offsets: HashMap<u32, (String, u64)>, // (section_name, offset)
    external_symbols: HashMap<String, bool>,
    // Track jump instructions that need fixup: (section_name, offset, target_label, is_conditional)
    jump_fixups: Vec<(String, u64, LabelRef, bool)>,
}

impl Assembler {
    /// Create a new assembler
    pub fn new() -> Self {
        let mut builder = AsmObjectBuilder::new();
        // Start with .text section (PROGBITS, executable)
        builder
            .start_section(".text".to_string(), 16, true, false)
            .expect("should emit bytes");

        Self {
            builder,
            label_offsets: HashMap::new(),
            external_symbols: HashMap::new(),
            jump_fixups: Vec::new(),
        }
    }

    /// Assemble instructions into an AsmObject
    pub fn assemble(
        mut self,
        instructions: &[X8664Instr],
        function_labels: &HashMap<String, Label>,
    ) -> AsmObject {
        // First pass: collect labels and determine which symbols are external
        let mut _offset = 0u64;
        for instr in instructions {
            match instr {
                X8664Instr::Label { id: _ } => {
                    // Labels are recorded during the emit phase when we know which section we're in
                    // For now just skip to avoid double-tracking
                }
                X8664Instr::Call { target } => {
                    // Check if this is an external symbol
                    if !self.is_internal_function(target, function_labels) {
                        self.external_symbols.insert(target.clone(), true);
                    }
                }
                _ => {}
            }

            // Update offset based on instruction size
            _offset += self.instruction_size(instr);
        }

        // Second pass: emit code and relocations
        for instr in instructions {
            self.emit_instruction(instr, function_labels);
        }

        // Apply jump fixups
        self.apply_jump_fixups();

        self.builder.build()
    }

    /// Check if a function name refers to an internal label
    fn is_internal_function(&self, name: &str, function_labels: &HashMap<String, Label>) -> bool {
        // Check if it's in the function labels map
        if function_labels.contains_key(name) {
            return true;
        }

        // Check common runtime functions that should be external
        if name.starts_with("__rue_flush_") || name.starts_with("__rue_write_") {
            return false;
        }

        // Otherwise assume internal
        true
    }

    /// Calculate instruction size in bytes
    fn instruction_size(&self, instr: &X8664Instr) -> u64 {
        match instr {
            X8664Instr::Label { .. } => 0,
            X8664Instr::Call { .. } => 5,    // E8 + 4-byte displacement
            X8664Instr::Ret => 1,            // C3
            X8664Instr::EnterFrame => 4,     // push rbp; mov rbp, rsp
            X8664Instr::LeaveFrame => 4,     // mov rsp, rbp; pop rbp
            X8664Instr::MovRR { .. } => 3,   // REX.W + 89/8B + ModRM
            X8664Instr::MovRI32 { .. } => 7, // REX.W + C7 + ModRM + 4-byte immediate
            X8664Instr::MovRI64 { .. } => 10, // REX.W + B8-BF + 8-byte immediate
            X8664Instr::MovRM { .. } => 4,   // With simple displacement
            X8664Instr::MovMR { .. } => 4,
            X8664Instr::AddRR { .. } => 3, // REX.W + 01/03 + ModRM
            X8664Instr::SubRR { .. } => 3,
            X8664Instr::Push { .. } => 2, // REX + 50-57
            X8664Instr::Pop { .. } => 2,
            X8664Instr::Jmp { .. } => 5,   // E9 + 4-byte displacement
            X8664Instr::JmpCC { .. } => 6, // 0F 80-8F + 4-byte displacement
            X8664Instr::CmpRR { .. } => 3,
            X8664Instr::CmpRI { .. } => 7, // REX.W + 81 /7 + 4-byte immediate
            X8664Instr::Syscall => 2,      // 0F 05
            X8664Instr::AllocStack { size } => {
                if *size <= 127 {
                    4 // sub rsp, imm8
                } else {
                    7 // sub rsp, imm32
                }
            }
            X8664Instr::LeaLabel { .. } => 7, // lea reg, [rip+disp32]
            X8664Instr::XorRR { .. } => 3,    // REX.W + 31 + ModRM
            X8664Instr::TestRR { .. } => 3,   // REX.W + 85 + ModRM
            X8664Instr::ImulRR { .. } => 4,   // REX.W + 0F AF + ModRM
            X8664Instr::ImulRI { imm, .. } => {
                if *imm >= -128 && *imm <= 127 { 4 } else { 7 } // imm8 vs imm32
            }
            X8664Instr::ImulRI32 { imm, dest, .. } => {
                let rex_needed = dest.needs_rex();
                if *imm >= -128 && *imm <= 127 {
                    if rex_needed { 4 } else { 3 } // imm8
                } else {
                    if rex_needed { 7 } else { 6 } // imm32
                }
            }
            X8664Instr::ImulRR32 { dest, src, .. } => {
                if dest.needs_rex() || src.needs_rex() {
                    4
                } else {
                    3
                }
            }
            X8664Instr::ImulRI64 { .. } => 7, // Conservative estimate
            X8664Instr::Idiv { .. } => 3,     // REX.W + F7 + ModRM
            X8664Instr::AndRR { .. } => 3,    // REX.W + 21 + ModRM
            X8664Instr::AndRI { dest, .. } => {
                if *dest == rue_target::X86Register::Rax {
                    6
                } else {
                    7
                }
            }
            X8664Instr::SetCC { dest, .. } => {
                if dest.needs_rex() { 4 } else { 3 } // 0F + opcode + ModRM
            }
            X8664Instr::AddRI { dest, imm } => {
                if *imm >= -128 && *imm <= 127 {
                    4 // imm8
                } else if *dest == rue_target::X86Register::Rax {
                    6 // special RAX encoding
                } else {
                    7 // full encoding
                }
            }
            X8664Instr::AddRI32 { dest, imm } => {
                let rex_needed = dest.needs_rex();
                if *imm >= -128 && *imm <= 127 {
                    if rex_needed { 4 } else { 3 } // imm8
                } else if *dest == rue_target::X86Register::Rax {
                    5 // special EAX encoding
                } else {
                    if rex_needed { 7 } else { 6 } // full encoding
                }
            }
            X8664Instr::AddRR32 { dest, src } => {
                if dest.needs_rex() || src.needs_rex() {
                    3
                } else {
                    2
                }
            }
            X8664Instr::SubRI { dest, imm } => {
                if *imm >= -128 && *imm <= 127 {
                    4 // imm8
                } else if *dest == rue_target::X86Register::Rax {
                    6 // special RAX encoding
                } else {
                    7 // full encoding
                }
            }
            X8664Instr::SubRI32 { dest, imm } => {
                let rex_needed = dest.needs_rex();
                if *imm >= -128 && *imm <= 127 {
                    if rex_needed { 4 } else { 3 } // imm8
                } else if *dest == rue_target::X86Register::Rax {
                    5 // special EAX encoding
                } else {
                    if rex_needed { 7 } else { 6 } // full encoding
                }
            }
            X8664Instr::SubRR32 { dest, src } => {
                if dest.needs_rex() || src.needs_rex() {
                    3
                } else {
                    2
                }
            }
            X8664Instr::MovMR8 { base, offset, src } => {
                let rex_needed = base.needs_rex()
                    || src.needs_rex()
                    || matches!(
                        base,
                        rue_target::X86Register::Rsp
                            | rue_target::X86Register::Rbp
                            | rue_target::X86Register::Rsi
                            | rue_target::X86Register::Rdi
                    )
                    || matches!(
                        src,
                        rue_target::X86Register::Rsp
                            | rue_target::X86Register::Rbp
                            | rue_target::X86Register::Rsi
                            | rue_target::X86Register::Rdi
                    );
                let rex_size = if rex_needed { 1 } else { 0 };
                let sib_needed = matches!(
                    base,
                    rue_target::X86Register::Rsp | rue_target::X86Register::R12
                );
                let sib_size = if sib_needed { 1 } else { 0 };
                if *offset == 0
                    && !matches!(
                        base,
                        rue_target::X86Register::Rbp | rue_target::X86Register::R13
                    )
                {
                    2 + rex_size + sib_size
                } else if *offset >= -128 && *offset <= 127 {
                    3 + rex_size + sib_size
                } else {
                    6 + rex_size + sib_size
                }
            }
            X8664Instr::MovRM8 { base, offset, dest } => {
                let rex_needed = base.needs_rex()
                    || dest.needs_rex()
                    || matches!(
                        base,
                        rue_target::X86Register::Rsp
                            | rue_target::X86Register::Rbp
                            | rue_target::X86Register::Rsi
                            | rue_target::X86Register::Rdi
                    )
                    || matches!(
                        dest,
                        rue_target::X86Register::Rsp
                            | rue_target::X86Register::Rbp
                            | rue_target::X86Register::Rsi
                            | rue_target::X86Register::Rdi
                    );
                let rex_size = if rex_needed { 1 } else { 0 };
                let sib_needed = matches!(
                    base,
                    rue_target::X86Register::Rsp | rue_target::X86Register::R12
                );
                let sib_size = if sib_needed { 1 } else { 0 };
                if *offset == 0
                    && !matches!(
                        base,
                        rue_target::X86Register::Rbp | rue_target::X86Register::R13
                    )
                {
                    2 + rex_size + sib_size
                } else if *offset >= -128 && *offset <= 127 {
                    3 + rex_size + sib_size
                } else {
                    6 + rex_size + sib_size
                }
            }
            X8664Instr::MovMR16 { base, offset, src } => {
                let rex_needed = base.needs_rex() || src.needs_rex();
                let rex_size = if rex_needed { 1 } else { 0 };
                if *offset == 0 && *base != rue_target::X86Register::Rbp {
                    3 + rex_size // 66 prefix + opcode + ModRM
                } else if *offset >= -128 && *offset <= 127 {
                    4 + rex_size // + disp8
                } else {
                    7 + rex_size // + disp32
                }
            }
            X8664Instr::MovRM16 { base, offset, dest } => {
                let rex_needed = base.needs_rex() || dest.needs_rex();
                let rex_size = if rex_needed { 1 } else { 0 };
                if *offset == 0 && *base != rue_target::X86Register::Rbp {
                    3 + rex_size // 66 prefix + opcode + ModRM
                } else if *offset >= -128 && *offset <= 127 {
                    4 + rex_size // + disp8
                } else {
                    7 + rex_size // + disp32
                }
            }
            X8664Instr::MovMR32 { base, offset, src } => {
                let rex_needed = base.needs_rex() || src.needs_rex();
                let rex_size = if rex_needed { 1 } else { 0 };
                if *offset == 0 && *base != rue_target::X86Register::Rbp {
                    2 + rex_size
                } else if *offset >= -128 && *offset <= 127 {
                    3 + rex_size
                } else {
                    6 + rex_size
                }
            }
            X8664Instr::MovRM32 { base, offset, dest } => {
                let rex_needed = base.needs_rex() || dest.needs_rex();
                let rex_size = if rex_needed { 1 } else { 0 };
                if *offset == 0 && *base != rue_target::X86Register::Rbp {
                    2 + rex_size
                } else if *offset >= -128 && *offset <= 127 {
                    3 + rex_size
                } else {
                    6 + rex_size
                }
            }
            X8664Instr::Movzx { .. } => 4, // REX.W + 0F B6 + ModRM
            X8664Instr::Movzx8to32 { dest, src } => {
                if dest.needs_rex() || src.needs_rex() {
                    4
                } else {
                    3
                }
            }
            X8664Instr::Movsxd { .. } => 3, // REX.W + 63 + ModRM
            X8664Instr::Shl { .. } => 3,    // REX.W + D3 + ModRM
            X8664Instr::Sar { .. } => 3,    // REX.W + D3 + ModRM
            X8664Instr::ShrRI { imm, .. } => {
                if *imm == 1 { 3 } else { 4 } // special encoding for shift by 1
            }
            X8664Instr::IncR { .. } => 3, // REX.W + FF + ModRM
            X8664Instr::DecR { .. } => 3, // REX.W + FF + ModRM
            X8664Instr::BtRI { .. } => 5, // REX.W + 0F BA + ModRM + imm8
            X8664Instr::CallIndirect { reg } => {
                if reg.needs_rex() {
                    3
                } else {
                    2
                }
            }
            X8664Instr::Loop { .. } => 2, // E2 + rel8
            X8664Instr::DataBytes { bytes } => bytes.len() as u64,
            X8664Instr::ReserveBytes { count } => *count as u64,
            X8664Instr::Section { .. } => 0, // Section directive doesn't emit bytes
            X8664Instr::Cqo => 2,            // REX.W + 99
            _ => 10,                         // Conservative estimate for other instructions
        }
    }

    /// Emit an instruction with proper encoding
    fn emit_instruction(&mut self, instr: &X8664Instr, function_labels: &HashMap<String, Label>) {
        match instr {
            X8664Instr::Label { id } => {
                // Record the actual offset from the builder with current section
                if let Some(ref section_name) = self.builder.get_current_section_name() {
                    let offset = self.builder.current_offset().unwrap_or(0);
                    self.label_offsets
                        .insert(*id, (section_name.to_string(), offset));
                    // Define the label symbol
                    if let Some(name) = self.find_label_name(*id, function_labels) {
                        let _ = self.builder.define_symbol(name, SymBind::Global, 0);
                    }
                } else {
                    panic!("Label defined without current section");
                }
            }
            X8664Instr::Call { target } => {
                self.emit_call(target, function_labels);
            }
            X8664Instr::Ret => {
                let _ = self.builder.emit_bytes(&[0xC3]);
            }
            X8664Instr::EnterFrame => {
                // push rbp
                let _ = self.builder.emit_bytes(&[0x55]);
                // mov rbp, rsp
                let _ = self.builder.emit_bytes(&[0x48, 0x89, 0xE5]);
            }
            X8664Instr::LeaveFrame => {
                // mov rsp, rbp
                let _ = self.builder.emit_bytes(&[0x48, 0x89, 0xEC]);
                // pop rbp
                let _ = self.builder.emit_bytes(&[0x5D]);
            }
            X8664Instr::Syscall => {
                let _ = self.builder.emit_bytes(&[0x0F, 0x05]);
            }
            X8664Instr::MovRR { dest, src } => {
                self.emit_mov_rr(*dest, *src);
            }
            X8664Instr::MovRI32 { dest, imm } => {
                self.emit_mov_ri32(*dest, *imm);
            }
            X8664Instr::MovRI64 { dest, imm } => {
                self.emit_mov_ri64(*dest, *imm);
            }
            X8664Instr::Push { reg } => {
                self.emit_push(*reg);
            }
            X8664Instr::Pop { reg } => {
                self.emit_pop(*reg);
            }
            X8664Instr::AddRR { dest, src } => {
                self.emit_add_rr(*dest, *src);
            }
            X8664Instr::SubRR { dest, src } => {
                self.emit_sub_rr(*dest, *src);
            }
            X8664Instr::CmpRR { left, right } => {
                self.emit_cmp_rr(*left, *right);
            }
            X8664Instr::CmpRI { reg, imm } => {
                self.emit_cmp_ri(*reg, (*imm) as i64);
            }
            X8664Instr::AllocStack { size } => {
                self.emit_alloc_stack((*size) as u64);
            }
            X8664Instr::Jmp { target } => {
                self.emit_jmp(target);
            }
            X8664Instr::JmpCC { cc, target } => {
                self.emit_jmp_cc(*cc, target);
            }
            X8664Instr::LeaLabel { dest, label } => {
                self.emit_lea_label(*dest, label);
            }
            X8664Instr::XorRR { dest, src } => {
                self.emit_xor_rr(*dest, *src);
            }
            X8664Instr::TestRR { left, right } => {
                self.emit_test_rr(*left, *right);
            }
            X8664Instr::ImulRR { dest, src } => {
                self.emit_imul_rr(*dest, *src);
            }
            X8664Instr::ImulRI { dest, imm } => {
                self.emit_imul_ri(*dest, *imm);
            }
            X8664Instr::ImulRI32 { dest, imm } => {
                self.emit_imul_ri32(*dest, *imm);
            }
            X8664Instr::ImulRR32 { dest, src } => {
                self.emit_imul_rr32(*dest, *src);
            }
            X8664Instr::ImulRI64 { dest, imm64 } => {
                self.emit_imul_ri64(*dest, *imm64);
            }
            X8664Instr::Idiv { divisor } => {
                self.emit_idiv(*divisor);
            }
            X8664Instr::AndRR { dest, src } => {
                self.emit_and_rr(*dest, *src);
            }
            X8664Instr::AndRI { dest, imm } => {
                self.emit_and_ri(*dest, *imm);
            }
            X8664Instr::SetCC { dest, cc } => {
                self.emit_setcc(*dest, *cc);
            }
            X8664Instr::AddRI { dest, imm } => {
                self.emit_add_ri(*dest, *imm);
            }
            X8664Instr::AddRI32 { dest, imm } => {
                self.emit_add_ri32(*dest, *imm);
            }
            X8664Instr::AddRR32 { dest, src } => {
                self.emit_add_rr32(*dest, *src);
            }
            X8664Instr::SubRI { dest, imm } => {
                self.emit_sub_ri(*dest, *imm);
            }
            X8664Instr::SubRI32 { dest, imm } => {
                self.emit_sub_ri32(*dest, *imm);
            }
            X8664Instr::SubRR32 { dest, src } => {
                self.emit_sub_rr32(*dest, *src);
            }
            X8664Instr::MovMR8 { base, offset, src } => {
                self.emit_mov_mr8(*base, *offset, *src);
            }
            X8664Instr::MovRM8 { dest, base, offset } => {
                self.emit_mov_rm8(*dest, *base, *offset);
            }
            X8664Instr::MovMR16 { base, offset, src } => {
                self.emit_mov_mr16(*base, *offset, *src);
            }
            X8664Instr::MovRM16 { dest, base, offset } => {
                self.emit_mov_rm16(*dest, *base, *offset);
            }
            X8664Instr::MovMR32 { base, offset, src } => {
                self.emit_mov_mr32(*base, *offset, *src);
            }
            X8664Instr::MovRM32 { dest, base, offset } => {
                self.emit_mov_rm32(*dest, *base, *offset);
            }
            X8664Instr::MovRM { dest, base, offset } => {
                self.emit_mov_rm(*dest, *base, *offset);
            }
            X8664Instr::MovMR { base, offset, src } => {
                self.emit_mov_mr(*base, *offset, *src);
            }
            X8664Instr::Movzx { dest, src } => {
                self.emit_movzx(*dest, *src);
            }
            X8664Instr::Movzx8to32 { dest, src } => {
                self.emit_movzx_8to32(*dest, *src);
            }
            X8664Instr::Movsxd { dest, src } => {
                self.emit_movsxd(*dest, *src);
            }
            X8664Instr::Shl { dest, count } => {
                self.emit_shl(*dest, *count);
            }
            X8664Instr::Sar { dest, count } => {
                self.emit_sar(*dest, *count);
            }
            X8664Instr::ShrRI { dest, imm } => {
                self.emit_shr_ri(*dest, *imm);
            }
            X8664Instr::IncR { reg } => {
                self.emit_inc_r(*reg);
            }
            X8664Instr::DecR { reg } => {
                self.emit_dec_r(*reg);
            }
            X8664Instr::BtRI { reg, bit } => {
                self.emit_bt_ri(*reg, *bit);
            }
            X8664Instr::CallIndirect { reg } => {
                self.emit_call_indirect(*reg);
            }
            X8664Instr::Loop { target } => {
                self.emit_loop(target);
            }
            X8664Instr::DataBytes { bytes } => {
                self.emit_data_bytes(bytes);
            }
            X8664Instr::ReserveBytes { count } => {
                self.emit_reserve_bytes(*count);
            }
            X8664Instr::Section { name } => {
                // Switch to a different section with proper section types
                let _ = match name.as_str() {
                    ".text" => self.builder.start_section(".text".into(), 16, true, false), // PROGBITS, RX
                    ".rodata" => self
                        .builder
                        .start_section(".rodata".into(), 16, false, false), // PROGBITS, R
                    ".data" => self.builder.start_section(".data".into(), 8, false, false), // PROGBITS, RW (NOT NOBITS)
                    ".bss" => self.builder.start_section(".bss".into(), 8, false, true), // NOBITS, RW
                    _ => self.builder.start_section(name.clone(), 1, false, false),
                };
            }
            X8664Instr::Cqo => {
                self.emit_cqo();
            }
            _ => {
                // For now, emit placeholder bytes for unimplemented instructions
                let _ = self.builder.emit_bytes(&[0x90]); // NOP
            }
        }
    }

    /// Emit a CALL instruction with relocation
    fn emit_call(&mut self, target: &str, _function_labels: &HashMap<String, Label>) {
        let offset = self.builder.emit_bytes(&[0xE8]).unwrap_or(0); // CALL opcode

        // Always emit relocation for calls - the linker will resolve them
        let _ = self.builder.emit_bytes(&[0, 0, 0, 0]); // Placeholder
        let _ = self.builder.add_relocation(
            offset + 1, // Offset of displacement field
            RelocKind::Pc32,
            target.to_string(),
            -4, // Addend for PC-relative call
        );

        // Mark as external if not internal
        if self.external_symbols.contains_key(target) {
            let _ = self.builder.reference_external(target.to_string());
        }

        // Note: We don't track current_offset anymore since we use builder.current_offset()
    }

    /// Emit MOV r64, r64
    fn emit_mov_rr(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W
        if src_rex {
            rex |= 0x04;
        } // REX.R (extends ModRM.reg field - source)
        if dest_rex {
            rex |= 0x01;
        } // REX.B (extends ModRM.r/m field - destination)

        self.builder
            .emit_bytes(&[rex, 0x89, 0xC0 | (src_code << 3) | dest_code])
            .expect("should emit bytes");
    }

    /// Emit MOV r32, imm32 (sign-extended to 64-bit)
    fn emit_mov_ri32(&mut self, dest: X86Register, imm: i32) {
        let (dest_code, dest_rex) = register_encoding(dest);

        // For 32-bit immediate move, we use the 32-bit encoding which zero-extends
        // But we want sign-extension, so we use the REX.W prefix
        let mut rex = 0x48; // REX.W for 64-bit operation
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        // Use C7 /0 for mov r/m32, imm32 with sign extension to 64-bit
        let _ = self.builder.emit_bytes(&[rex, 0xC7, 0xC0 | dest_code]);
        let _ = self.builder.emit_bytes(&imm.to_le_bytes());
    }

    /// Emit MOV r64, imm64
    fn emit_mov_ri64(&mut self, dest: X86Register, imm: i64) {
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        let _ = self.builder.emit_bytes(&[rex, 0xB8 + dest_code]);
        let _ = self.builder.emit_bytes(&(imm as u64).to_le_bytes());
    }

    /// Emit PUSH r64
    fn emit_push(&mut self, reg: X86Register) {
        let (reg_code, reg_rex) = register_encoding(reg);

        if reg_rex {
            let _ = self.builder.emit_bytes(&[0x41, 0x50 + reg_code]);
        } else {
            let _ = self.builder.emit_bytes(&[0x50 + reg_code]);
        }
    }

    /// Emit POP r64
    fn emit_pop(&mut self, reg: X86Register) {
        let (reg_code, reg_rex) = register_encoding(reg);

        if reg_rex {
            let _ = self.builder.emit_bytes(&[0x41, 0x58 + reg_code]);
        } else {
            let _ = self.builder.emit_bytes(&[0x58 + reg_code]);
        }
    }

    /// Emit ADD r64, r64
    fn emit_add_rr(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x01, 0xC0 | (src_code << 3) | dest_code])
            .expect("should emit bytes");
    }

    /// Emit SUB r64, r64
    fn emit_sub_rr(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x29, 0xC0 | (src_code << 3) | dest_code])
            .expect("should emit bytes");
    }

    /// Emit CMP r64, r64
    fn emit_cmp_rr(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x39, 0xC0 | (src_code << 3) | dest_code])
            .expect("should emit bytes");
    }

    /// Emit CMP r64, imm32
    fn emit_cmp_ri(&mut self, reg: X86Register, imm: i64) {
        let (reg_code, reg_rex) = register_encoding(reg);

        let mut rex = 0x48; // REX.W
        if reg_rex {
            rex |= 0x01;
        } // REX.B

        if reg == X86Register::Rax && imm >= -128 && imm <= 127 {
            // Special encoding for RAX
            let _ = self.builder.emit_bytes(&[rex, 0x3D]);
            let _ = self.builder.emit_bytes(&(imm as i32).to_le_bytes());
        } else {
            let _ = self.builder.emit_bytes(&[rex, 0x81, 0xF8 | reg_code]);
            let _ = self.builder.emit_bytes(&(imm as i32).to_le_bytes());
        }
    }

    /// Emit SUB rsp, imm
    fn emit_alloc_stack(&mut self, size: u64) {
        if size <= 127 {
            // sub rsp, imm8
            let _ = self.builder.emit_bytes(&[0x48, 0x83, 0xEC, size as u8]);
        } else {
            // sub rsp, imm32
            let _ = self.builder.emit_bytes(&[0x48, 0x81, 0xEC]);
            let _ = self.builder.emit_bytes(&(size as u32).to_le_bytes());
        }
    }

    /// Emit JMP with label reference
    fn emit_jmp(&mut self, target: &LabelRef) {
        let opcode_offset = self.builder.emit_bytes(&[0xE9]).unwrap_or(0); // JMP rel32
        // Track this location for fixup with section info (offset points to the displacement field)
        if let Some(section_name) = self.builder.get_current_section_name() {
            self.jump_fixups
                .push((section_name, opcode_offset + 1, target.clone(), false));
        } else {
            panic!("Jump emitted without current section");
        }
        let _ = self.builder.emit_bytes(&[0, 0, 0, 0]); // Placeholder
    }

    /// Emit conditional jump
    fn emit_jmp_cc(&mut self, cc: rue_target::ConditionCode, target: &LabelRef) {
        let opcode = match cc {
            rue_target::ConditionCode::Equal => 0x84,
            rue_target::ConditionCode::NotEqual => 0x85,
            rue_target::ConditionCode::Less => 0x8C,
            rue_target::ConditionCode::LessEqual => 0x8E,
            rue_target::ConditionCode::Greater => 0x8F,
            rue_target::ConditionCode::GreaterEqual => 0x8D,
            rue_target::ConditionCode::Below => 0x82,
            rue_target::ConditionCode::BelowEqual => 0x86,
            rue_target::ConditionCode::Above => 0x87,
            rue_target::ConditionCode::AboveEqual => 0x83,
        };

        let opcode_offset = self.builder.emit_bytes(&[0x0F, opcode]).unwrap_or(0);
        // Track this location for fixup with section info (offset points to the displacement field)
        if let Some(section_name) = self.builder.get_current_section_name() {
            self.jump_fixups
                .push((section_name, opcode_offset + 2, target.clone(), true));
        } else {
            panic!("Conditional jump emitted without current section");
        }
        let _ = self.builder.emit_bytes(&[0, 0, 0, 0]); // Placeholder
    }

    /// Emit LEA with label
    fn emit_lea_label(&mut self, dest: X86Register, label: &str) {
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x04;
        } // REX.R

        // lea reg, [rip+disp32]
        let _ = self
            .builder
            .emit_bytes(&[rex, 0x8D, 0x05 | (dest_code << 3)]);
        let offset = self.builder.emit_bytes(&[0, 0, 0, 0]).unwrap_or(0); // Placeholder for RIP-relative offset

        // Add relocation for the label (PC-relative, with -4 adjustment for the disp32 field)
        let _ = self.builder.add_relocation(
            offset, // Point to the start of the disp32 field
            RelocKind::Pc32,
            label.to_string(),
            -4, // Adjust for the 4 bytes of the disp32 field itself
        );
    }

    /// Emit XOR r64, r64
    fn emit_xor_rr(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x31, 0xC0 | (src_code << 3) | dest_code])
            .expect("should emit bytes");
    }

    /// Emit TEST r64, r64
    fn emit_test_rr(&mut self, left: X86Register, right: X86Register) {
        let (left_code, left_rex) = register_encoding(left);
        let (right_code, right_rex) = register_encoding(right);

        let mut rex = 0x48; // REX.W
        if right_rex {
            rex |= 0x04;
        } // REX.R
        if left_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x85, 0xC0 | (right_code << 3) | left_code])
            .expect("should emit bytes");
    }

    /// Emit IMUL r64, r64 (two-operand form)
    fn emit_imul_rr(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x04;
        } // REX.R
        if src_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x0F, 0xAF, 0xC0 | (dest_code << 3) | src_code])
            .expect("should emit bytes");
    }

    /// Emit IMUL r64, imm32 (sign-extended to 64-bit)
    fn emit_imul_ri(&mut self, dest: X86Register, imm: i32) {
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        if imm >= -128 && imm <= 127 {
            // imul r64, imm8
            self.builder
                .emit_bytes(&[rex, 0x6B, 0xC0 | dest_code, imm as u8])
                .expect("should emit bytes");
        } else {
            // imul r64, imm32
            self.builder
                .emit_bytes(&[rex, 0x69, 0xC0 | dest_code])
                .expect("should emit bytes");
            self.builder
                .emit_bytes(&imm.to_le_bytes())
                .expect("should emit bytes");
        }
    }

    /// Emit IMUL r32, imm32 (32-bit)
    fn emit_imul_ri32(&mut self, dest: X86Register, imm: i32) {
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x40; // REX without W for 32-bit
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        if imm >= -128 && imm <= 127 {
            // imul r32, imm8
            if dest_rex {
                self.builder
                    .emit_bytes(&[rex, 0x6B, 0xC0 | dest_code, imm as u8])
                    .expect("should emit bytes");
            } else {
                self.builder
                    .emit_bytes(&[0x6B, 0xC0 | dest_code, imm as u8])
                    .expect("should emit bytes");
            }
        } else {
            // imul r32, imm32
            if dest_rex {
                let _ = self.builder.emit_bytes(&[rex, 0x69, 0xC0 | dest_code]);
            } else {
                let _ = self.builder.emit_bytes(&[0x69, 0xC0 | dest_code]);
            }
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        }
    }

    /// Emit IMUL r32, r32 (32-bit)
    fn emit_imul_rr32(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x40; // REX without W for 32-bit
        if dest_rex {
            rex |= 0x04;
        } // REX.R
        if src_rex {
            rex |= 0x01;
        } // REX.B

        if dest_rex || src_rex {
            self.builder
                .emit_bytes(&[rex, 0x0F, 0xAF, 0xC0 | (dest_code << 3) | src_code])
                .expect("should emit bytes");
        } else {
            self.builder
                .emit_bytes(&[0x0F, 0xAF, 0xC0 | (dest_code << 3) | src_code])
                .expect("should emit bytes");
        }
    }

    /// Emit IMUL r64, imm64 (using mov + imul for large immediates)
    fn emit_imul_ri64(&mut self, dest: X86Register, imm64: i64) {
        // For 64-bit immediates, we need to use a temporary register
        // This is a complex case that typically requires multiple instructions
        // For now, use a simpler approach if the immediate fits in 32 bits
        if imm64 >= i32::MIN as i64 && imm64 <= i32::MAX as i64 {
            self.emit_imul_ri(dest, imm64 as i32);
        } else {
            // For true 64-bit immediates, we'd need to load into a temp register first
            // This is a placeholder implementation
            self.emit_imul_ri(dest, (imm64 & 0xFFFFFFFF) as i32);
        }
    }

    /// Emit IDIV r64 (signed division)
    fn emit_idiv(&mut self, divisor: X86Register) {
        let (divisor_code, divisor_rex) = register_encoding(divisor);

        let mut rex = 0x48; // REX.W
        if divisor_rex {
            rex |= 0x01;
        } // REX.B

        let _ = self.builder.emit_bytes(&[rex, 0xF7, 0xF8 | divisor_code]);
    }

    /// Emit AND r64, r64
    fn emit_and_rr(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x21, 0xC0 | (src_code << 3) | dest_code])
            .expect("should emit bytes");
    }

    /// Emit AND r64, imm32
    fn emit_and_ri(&mut self, dest: X86Register, imm: i32) {
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        if dest == X86Register::Rax {
            // Special encoding for RAX
            let _ = self.builder.emit_bytes(&[rex, 0x25]);
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        } else {
            let _ = self.builder.emit_bytes(&[rex, 0x81, 0xE0 | dest_code]);
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        }
    }

    /// Emit SETcc r8 (set byte based on condition code)
    fn emit_setcc(&mut self, dest: X86Register, cc: rue_target::ConditionCode) {
        let (dest_code, dest_rex) = register_encoding(dest);

        let opcode = match cc {
            rue_target::ConditionCode::Equal => 0x94,
            rue_target::ConditionCode::NotEqual => 0x95,
            rue_target::ConditionCode::Less => 0x9C,
            rue_target::ConditionCode::LessEqual => 0x9E,
            rue_target::ConditionCode::Greater => 0x9F,
            rue_target::ConditionCode::GreaterEqual => 0x9D,
            rue_target::ConditionCode::Below => 0x92,
            rue_target::ConditionCode::BelowEqual => 0x96,
            rue_target::ConditionCode::Above => 0x97,
            rue_target::ConditionCode::AboveEqual => 0x93,
        };

        // For byte operations, we need REX prefix if using R8-R15 or RSP/RBP/RSI/RDI
        let needs_rex = dest_rex || needs_byte_rex(dest);

        if needs_rex {
            let mut rex = 0x40; // REX prefix (no W for byte operations)
            if dest_rex {
                rex |= 0x01;
            } // REX.B
            self.builder
                .emit_bytes(&[rex, 0x0F, opcode, 0xC0 | dest_code])
                .expect("should emit bytes");
        } else {
            self.builder
                .emit_bytes(&[0x0F, opcode, 0xC0 | dest_code])
                .expect("should emit bytes");
        }
    }

    /// Emit ADD r64, imm32
    fn emit_add_ri(&mut self, dest: X86Register, imm: i32) {
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        if imm >= -128 && imm <= 127 {
            // add r64, imm8
            self.builder
                .emit_bytes(&[rex, 0x83, 0xC0 | dest_code, imm as u8])
                .expect("should emit bytes");
        } else if dest == X86Register::Rax {
            // Special encoding for RAX
            let _ = self.builder.emit_bytes(&[rex, 0x05]);
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        } else {
            let _ = self.builder.emit_bytes(&[rex, 0x81, 0xC0 | dest_code]);
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        }
    }

    /// Emit ADD r32, imm32 (32-bit)
    fn emit_add_ri32(&mut self, dest: X86Register, imm: i32) {
        let (dest_code, dest_rex) = register_encoding(dest);

        if imm >= -128 && imm <= 127 {
            // add r32, imm8
            if dest_rex {
                self.builder
                    .emit_bytes(&[0x41, 0x83, 0xC0 | dest_code, imm as u8])
                    .expect("should emit bytes");
            } else {
                self.builder
                    .emit_bytes(&[0x83, 0xC0 | dest_code, imm as u8])
                    .expect("should emit bytes");
            }
        } else if dest == X86Register::Rax {
            // Special encoding for EAX
            let _ = self.builder.emit_bytes(&[0x05]);
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        } else {
            if dest_rex {
                let _ = self.builder.emit_bytes(&[0x41, 0x81, 0xC0 | dest_code]);
            } else {
                let _ = self.builder.emit_bytes(&[0x81, 0xC0 | dest_code]);
            }
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        }
    }

    /// Emit ADD r32, r32 (32-bit)
    fn emit_add_rr32(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x40; // REX without W for 32-bit
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        if src_rex || dest_rex {
            self.builder
                .emit_bytes(&[rex, 0x01, 0xC0 | (src_code << 3) | dest_code])
                .expect("should emit bytes");
        } else {
            self.builder
                .emit_bytes(&[0x01, 0xC0 | (src_code << 3) | dest_code])
                .expect("should emit bytes");
        }
    }

    /// Emit SUB r64, imm32
    fn emit_sub_ri(&mut self, dest: X86Register, imm: i32) {
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        if imm >= -128 && imm <= 127 {
            // sub r64, imm8
            self.builder
                .emit_bytes(&[rex, 0x83, 0xE8 | dest_code, imm as u8])
                .expect("should emit bytes");
        } else if dest == X86Register::Rax {
            // Special encoding for RAX
            let _ = self.builder.emit_bytes(&[rex, 0x2D]);
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        } else {
            let _ = self.builder.emit_bytes(&[rex, 0x81, 0xE8 | dest_code]);
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        }
    }

    /// Emit SUB r32, imm32 (32-bit)
    fn emit_sub_ri32(&mut self, dest: X86Register, imm: i32) {
        let (dest_code, dest_rex) = register_encoding(dest);

        if imm >= -128 && imm <= 127 {
            // sub r32, imm8
            if dest_rex {
                self.builder
                    .emit_bytes(&[0x41, 0x83, 0xE8 | dest_code, imm as u8])
                    .expect("should emit bytes");
            } else {
                self.builder
                    .emit_bytes(&[0x83, 0xE8 | dest_code, imm as u8])
                    .expect("should emit bytes");
            }
        } else if dest == X86Register::Rax {
            // Special encoding for EAX
            let _ = self.builder.emit_bytes(&[0x2D]);
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        } else {
            if dest_rex {
                let _ = self.builder.emit_bytes(&[0x41, 0x81, 0xE8 | dest_code]);
            } else {
                let _ = self.builder.emit_bytes(&[0x81, 0xE8 | dest_code]);
            }
            let _ = self.builder.emit_bytes(&imm.to_le_bytes());
        }
    }

    /// Emit SUB r32, r32 (32-bit)
    fn emit_sub_rr32(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x40; // REX without W for 32-bit
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        if src_rex || dest_rex {
            self.builder
                .emit_bytes(&[rex, 0x29, 0xC0 | (src_code << 3) | dest_code])
                .expect("should emit bytes");
        } else {
            self.builder
                .emit_bytes(&[0x29, 0xC0 | (src_code << 3) | dest_code])
                .expect("should emit bytes");
        }
    }

    /// Emit MOV [base + offset], src (8-bit store)
    fn emit_mov_mr8(&mut self, base: X86Register, offset: i32, src: X86Register) {
        let (base_code, base_rex) = register_encoding(base);
        let (src_code, src_rex) = register_encoding(src);

        // For byte operations, we need REX prefix if:
        // 1. Using R8-R15 (as usual)
        // 2. Using RSP/RBP/RSI/RDI to access SPL/BPL/SIL/DIL instead of AH/CH/DH/BH
        let needs_rex = src_rex || base_rex || needs_byte_rex(src) || needs_byte_rex(base);

        if needs_rex {
            let mut rex = 0x40; // REX prefix (no W for byte operations)

            if src_rex {
                rex |= 0x04;
            } // REX.R for R8-R15
            if base_rex {
                rex |= 0x01;
            } // REX.B for R8-R15
            let _ = self.builder.emit_bytes(&[rex]);
        }

        let _ = self.builder.emit_bytes(&[0x88]); // MOV r/m8, r8 opcode

        // RSP/R12 need SIB byte
        let needs_sib = needs_sib_byte(base);

        if offset == 0 && base != X86Register::Rbp && base != X86Register::R13 {
            // [base] without displacement
            let modrm = (src_code << 3) | base_code;
            let _ = self.builder.emit_bytes(&[modrm]);
            if needs_sib {
                let _ = self.builder.emit_bytes(&[0x24]); // SIB: scale=00, index=100 (none), base=100 (RSP)
            }
        } else if offset >= -128 && offset <= 127 {
            // [base + disp8]
            let modrm = 0x40 | (src_code << 3) | base_code;
            let _ = self.builder.emit_bytes(&[modrm]);
            if needs_sib {
                let _ = self.builder.emit_bytes(&[0x24]); // SIB: scale=00, index=100 (none), base=100 (RSP)
            }
            let _ = self.builder.emit_bytes(&[offset as u8]);
        } else {
            // [base + disp32]
            let modrm = 0x80 | (src_code << 3) | base_code;
            let _ = self.builder.emit_bytes(&[modrm]);
            if needs_sib {
                let _ = self.builder.emit_bytes(&[0x24]); // SIB: scale=00, index=100 (none), base=100 (RSP)
            }
            let _ = self.builder.emit_bytes(&offset.to_le_bytes());
        }
    }

    /// Emit MOV dest, [base + offset] (8-bit load)
    fn emit_mov_rm8(&mut self, dest: X86Register, base: X86Register, offset: i32) {
        let (base_code, base_rex) = register_encoding(base);
        let (dest_code, dest_rex) = register_encoding(dest);

        // For byte operations, we need REX prefix if:
        // 1. Using R8-R15 (as usual)
        // 2. Using RSP/RBP/RSI/RDI to access SPL/BPL/SIL/DIL instead of AH/CH/DH/BH
        let needs_rex = dest_rex || base_rex || needs_byte_rex(dest) || needs_byte_rex(base);

        if needs_rex {
            let mut rex = 0x40; // REX prefix (no W for byte operations)
            if dest_rex {
                rex |= 0x04;
            } // REX.R
            if base_rex {
                rex |= 0x01;
            } // REX.B
            let _ = self.builder.emit_bytes(&[rex]);
        }

        let _ = self.builder.emit_bytes(&[0x8A]); // MOV r8, r/m8 opcode

        // RSP/R12 need SIB byte
        let needs_sib = needs_sib_byte(base);

        if offset == 0 && base != X86Register::Rbp && base != X86Register::R13 {
            // [base] without displacement
            let modrm = (dest_code << 3) | base_code;
            let _ = self.builder.emit_bytes(&[modrm]);
            if needs_sib {
                let _ = self.builder.emit_bytes(&[0x24]); // SIB: scale=00, index=100 (none), base=100 (RSP)
            }
        } else if offset >= -128 && offset <= 127 {
            // [base + disp8]
            let modrm = 0x40 | (dest_code << 3) | base_code;
            let _ = self.builder.emit_bytes(&[modrm]);
            if needs_sib {
                let _ = self.builder.emit_bytes(&[0x24]); // SIB: scale=00, index=100 (none), base=100 (RSP)
            }
            let _ = self.builder.emit_bytes(&[offset as u8]);
        } else {
            // [base + disp32]
            let modrm = 0x80 | (dest_code << 3) | base_code;
            let _ = self.builder.emit_bytes(&[modrm]);
            if needs_sib {
                let _ = self.builder.emit_bytes(&[0x24]); // SIB: scale=00, index=100 (none), base=100 (RSP)
            }
            let _ = self.builder.emit_bytes(&offset.to_le_bytes());
        }
    }

    /// Emit MOV [base + offset], src (16-bit store)
    fn emit_mov_mr16(&mut self, base: X86Register, offset: i32, src: X86Register) {
        let (base_code, base_rex) = register_encoding(base);
        let (src_code, src_rex) = register_encoding(src);

        // 16-bit prefix
        let _ = self.builder.emit_bytes(&[0x66]);

        let mut rex = 0x40; // REX prefix (no W for 16-bit operations)
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if base_rex {
            rex |= 0x01;
        } // REX.B

        if src_rex || base_rex {
            let _ = self.builder.emit_bytes(&[rex]);
        }

        if offset == 0 && base != X86Register::Rbp {
            // [base] without displacement
            self.builder
                .emit_bytes(&[0x89, (src_code << 3) | base_code])
                .expect("should emit bytes");
        } else if offset >= -128 && offset <= 127 {
            // [base + disp8]
            self.builder
                .emit_bytes(&[0x89, 0x40 | (src_code << 3) | base_code, offset as u8])
                .expect("should emit bytes");
        } else {
            // [base + disp32]
            self.builder
                .emit_bytes(&[0x89, 0x80 | (src_code << 3) | base_code])
                .expect("should emit bytes");
            let _ = self.builder.emit_bytes(&offset.to_le_bytes());
        }
    }

    /// Emit MOV dest, [base + offset] (16-bit load)
    fn emit_mov_rm16(&mut self, dest: X86Register, base: X86Register, offset: i32) {
        let (base_code, base_rex) = register_encoding(base);
        let (dest_code, dest_rex) = register_encoding(dest);

        // 16-bit prefix
        let _ = self.builder.emit_bytes(&[0x66]);

        let mut rex = 0x40; // REX prefix (no W for 16-bit operations)
        if dest_rex {
            rex |= 0x04;
        } // REX.R
        if base_rex {
            rex |= 0x01;
        } // REX.B

        if dest_rex || base_rex {
            let _ = self.builder.emit_bytes(&[rex]);
        }

        if offset == 0 && base != X86Register::Rbp {
            // [base] without displacement
            self.builder
                .emit_bytes(&[0x8B, (dest_code << 3) | base_code])
                .expect("should emit bytes");
        } else if offset >= -128 && offset <= 127 {
            // [base + disp8]
            self.builder
                .emit_bytes(&[0x8B, 0x40 | (dest_code << 3) | base_code, offset as u8])
                .expect("should emit bytes");
        } else {
            // [base + disp32]
            self.builder
                .emit_bytes(&[0x8B, 0x80 | (dest_code << 3) | base_code])
                .expect("should emit bytes");
            let _ = self.builder.emit_bytes(&offset.to_le_bytes());
        }
    }

    /// Emit MOV [base + offset], src (32-bit store)
    fn emit_mov_mr32(&mut self, base: X86Register, offset: i32, src: X86Register) {
        let (base_code, base_rex) = register_encoding(base);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x40; // REX prefix (no W for 32-bit operations)
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if base_rex {
            rex |= 0x01;
        } // REX.B

        if src_rex || base_rex {
            let _ = self.builder.emit_bytes(&[rex]);
        }

        if offset == 0 && base != X86Register::Rbp {
            // [base] without displacement
            self.builder
                .emit_bytes(&[0x89, (src_code << 3) | base_code])
                .expect("should emit bytes");
        } else if offset >= -128 && offset <= 127 {
            // [base + disp8]
            self.builder
                .emit_bytes(&[0x89, 0x40 | (src_code << 3) | base_code, offset as u8])
                .expect("should emit bytes");
        } else {
            // [base + disp32]
            self.builder
                .emit_bytes(&[0x89, 0x80 | (src_code << 3) | base_code])
                .expect("should emit bytes");
            let _ = self.builder.emit_bytes(&offset.to_le_bytes());
        }
    }

    /// Emit MOV dest, [base + offset] (32-bit load)
    fn emit_mov_rm32(&mut self, dest: X86Register, base: X86Register, offset: i32) {
        let (base_code, base_rex) = register_encoding(base);
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x40; // REX prefix (no W for 32-bit operations)
        if dest_rex {
            rex |= 0x04;
        } // REX.R
        if base_rex {
            rex |= 0x01;
        } // REX.B

        if dest_rex || base_rex {
            let _ = self.builder.emit_bytes(&[rex]);
        }

        if offset == 0 && base != X86Register::Rbp {
            // [base] without displacement
            self.builder
                .emit_bytes(&[0x8B, (dest_code << 3) | base_code])
                .expect("should emit bytes");
        } else if offset >= -128 && offset <= 127 {
            // [base + disp8]
            self.builder
                .emit_bytes(&[0x8B, 0x40 | (dest_code << 3) | base_code, offset as u8])
                .expect("should emit bytes");
        } else {
            // [base + disp32]
            self.builder
                .emit_bytes(&[0x8B, 0x80 | (dest_code << 3) | base_code])
                .expect("should emit bytes");
            let _ = self.builder.emit_bytes(&offset.to_le_bytes());
        }
    }

    /// Emit MOV dest, [base + offset] (64-bit load)
    fn emit_mov_rm(&mut self, dest: X86Register, base: X86Register, offset: i32) {
        let (base_code, base_rex) = register_encoding(base);
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x48; // REX.W for 64-bit
        if dest_rex {
            rex |= 0x04;
        } // REX.R
        if base_rex {
            rex |= 0x01;
        } // REX.B

        if offset == 0 && base != X86Register::Rbp {
            // [base] without displacement
            self.builder
                .emit_bytes(&[rex, 0x8B, (dest_code << 3) | base_code])
                .expect("should emit bytes");
        } else if offset >= -128 && offset <= 127 {
            // [base + disp8]
            self.builder
                .emit_bytes(&[rex, 0x8B, 0x40 | (dest_code << 3) | base_code, offset as u8])
                .expect("should emit bytes");
        } else {
            // [base + disp32]
            self.builder
                .emit_bytes(&[rex, 0x8B, 0x80 | (dest_code << 3) | base_code])
                .expect("should emit bytes");
            self.builder
                .emit_bytes(&offset.to_le_bytes())
                .expect("should emit bytes");
        }
    }

    /// Emit MOV [base + offset], src (64-bit store)
    fn emit_mov_mr(&mut self, base: X86Register, offset: i32, src: X86Register) {
        let (base_code, base_rex) = register_encoding(base);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W for 64-bit
        if src_rex {
            rex |= 0x04;
        } // REX.R
        if base_rex {
            rex |= 0x01;
        } // REX.B

        if offset == 0 && base != X86Register::Rbp {
            // [base] without displacement
            self.builder
                .emit_bytes(&[rex, 0x89, (src_code << 3) | base_code])
                .expect("should emit bytes");
        } else if offset >= -128 && offset <= 127 {
            // [base + disp8]
            self.builder
                .emit_bytes(&[rex, 0x89, 0x40 | (src_code << 3) | base_code, offset as u8])
                .expect("should emit bytes");
        } else {
            // [base + disp32]
            self.builder
                .emit_bytes(&[rex, 0x89, 0x80 | (src_code << 3) | base_code])
                .expect("should emit bytes");
            let _ = self.builder.emit_bytes(&offset.to_le_bytes());
        }
    }

    /// Emit MOVZX r64, r8 (zero extend byte to qword)
    fn emit_movzx(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x04;
        } // REX.R
        if src_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x0F, 0xB6, 0xC0 | (dest_code << 3) | src_code])
            .expect("should emit bytes");
    }

    /// Emit MOVZX r32, r8 (zero extend byte to dword)
    fn emit_movzx_8to32(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x40; // REX without W for 32-bit result
        if dest_rex {
            rex |= 0x04;
        } // REX.R
        if src_rex {
            rex |= 0x01;
        } // REX.B

        if dest_rex || src_rex {
            self.builder
                .emit_bytes(&[rex, 0x0F, 0xB6, 0xC0 | (dest_code << 3) | src_code])
                .expect("should emit bytes");
        } else {
            self.builder
                .emit_bytes(&[0x0F, 0xB6, 0xC0 | (dest_code << 3) | src_code])
                .expect("should emit bytes");
        }
    }

    /// Emit MOVSXD r64, r32 (sign extend dword to qword)
    fn emit_movsxd(&mut self, dest: X86Register, src: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);
        let (src_code, src_rex) = register_encoding(src);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x04;
        } // REX.R
        if src_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x63, 0xC0 | (dest_code << 3) | src_code])
            .expect("should emit bytes");
    }

    /// Emit SHL dest, count (shift left by CL register)
    fn emit_shl(&mut self, dest: X86Register, count: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);

        // Count must be CL register
        assert_eq!(count, X86Register::Rcx, "SHL count must be CL (RCX)");

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        let _ = self.builder.emit_bytes(&[rex, 0xD3, 0xE0 | dest_code]);
    }

    /// Emit SAR dest, count (arithmetic shift right by CL register)
    fn emit_sar(&mut self, dest: X86Register, count: X86Register) {
        let (dest_code, dest_rex) = register_encoding(dest);

        // Count must be CL register
        assert_eq!(count, X86Register::Rcx, "SAR count must be CL (RCX)");

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        let _ = self.builder.emit_bytes(&[rex, 0xD3, 0xF8 | dest_code]);
    }

    /// Emit SHR dest, imm8 (logical shift right by immediate)
    fn emit_shr_ri(&mut self, dest: X86Register, imm: u8) {
        let (dest_code, dest_rex) = register_encoding(dest);

        let mut rex = 0x48; // REX.W
        if dest_rex {
            rex |= 0x01;
        } // REX.B

        if imm == 1 {
            // shr r64, 1 (special encoding)
            let _ = self.builder.emit_bytes(&[rex, 0xD1, 0xE8 | dest_code]);
        } else {
            // shr r64, imm8
            let _ = self.builder.emit_bytes(&[rex, 0xC1, 0xE8 | dest_code, imm]);
        }
    }

    /// Emit INC r64 (increment register)
    fn emit_inc_r(&mut self, reg: X86Register) {
        let (reg_code, reg_rex) = register_encoding(reg);

        let mut rex = 0x48; // REX.W
        if reg_rex {
            rex |= 0x01;
        } // REX.B

        let _ = self.builder.emit_bytes(&[rex, 0xFF, 0xC0 | reg_code]);
    }

    /// Emit DEC r64 (decrement register)
    fn emit_dec_r(&mut self, reg: X86Register) {
        let (reg_code, reg_rex) = register_encoding(reg);

        let mut rex = 0x48; // REX.W
        if reg_rex {
            rex |= 0x01;
        } // REX.B

        let _ = self.builder.emit_bytes(&[rex, 0xFF, 0xC8 | reg_code]);
    }

    /// Emit BT r64, imm8 (bit test)
    fn emit_bt_ri(&mut self, reg: X86Register, bit: u8) {
        let (reg_code, reg_rex) = register_encoding(reg);

        let mut rex = 0x48; // REX.W
        if reg_rex {
            rex |= 0x01;
        } // REX.B

        self.builder
            .emit_bytes(&[rex, 0x0F, 0xBA, 0xE0 | reg_code, bit])
            .expect("should emit bytes");
    }

    /// Emit CALL [reg] (indirect call through register)
    fn emit_call_indirect(&mut self, reg: X86Register) {
        let (reg_code, reg_rex) = register_encoding(reg);

        if reg_rex {
            let _ = self.builder.emit_bytes(&[0x41, 0xFF, 0xD0 | reg_code]);
        } else {
            let _ = self.builder.emit_bytes(&[0xFF, 0xD0 | reg_code]);
        }
    }

    /// Emit LOOP target (decrement RCX and jump if not zero)
    fn emit_loop(&mut self, _target: &LabelRef) {
        let _ = self.builder.emit_bytes(&[0xE2]); // LOOP rel8
        let _ = self.builder.emit_bytes(&[0]); // Placeholder for 8-bit displacement
        // TODO: Handle label resolution for 8-bit displacement
    }

    /// Emit raw data bytes
    fn emit_data_bytes(&mut self, bytes: &[u8]) {
        let _ = self.builder.emit_bytes(bytes);
    }

    /// Emit zero-initialized space reservation
    fn emit_reserve_bytes(&mut self, count: u32) {
        // Emit zeros for the reserved space
        let zeros = vec![0u8; count as usize];
        let _ = self.builder.emit_bytes(&zeros);
    }

    /// Emit CQO (sign extend RAX to RDX:RAX)
    fn emit_cqo(&mut self) {
        let _ = self.builder.emit_bytes(&[0x48, 0x99]); // REX.W + CQO
    }

    /// Find label name from ID
    fn find_label_name(&self, id: u32, function_labels: &HashMap<String, Label>) -> Option<String> {
        for (name, label) in function_labels {
            if label.id() == id {
                return Some(name.clone());
            }
        }
        None
    }

    /// Apply fixups for jump instructions
    fn apply_jump_fixups(&mut self) {
        for (fixup_section, fixup_offset, target, _is_conditional) in &self.jump_fixups {
            let target_info = match target {
                LabelRef::Local(label_id) => {
                    // Look up the label offset and section info
                    match self.label_offsets.get(label_id) {
                        Some((target_section, target_offset)) => {
                            Some((target_section.clone(), *target_offset))
                        }
                        None => {
                            panic!("Jump references non-existent label with ID {}", label_id);
                        }
                    }
                }
                LabelRef::Global(_) => {
                    // Global labels not supported for jumps yet
                    None
                }
            };

            if let Some((target_section, target_offset)) = target_info {
                // Verify both jump and target are in the same section for relative addressing
                if fixup_section != &target_section {
                    panic!(
                        "Cross-section jump not supported: jump in '{}' targets label in '{}'",
                        fixup_section, target_section
                    );
                }

                // Calculate relative offset from the end of the jump instruction
                // fixup_offset points to the start of the displacement field
                // We need to add 4 to get to the end of the instruction
                let jump_end = fixup_offset + 4;

                let relative_offset = target_offset as i32 - jump_end as i32;

                // Patch the displacement using section-aware patching
                self.builder
                    .patch_i32_in_section(fixup_section, *fixup_offset, relative_offset)
                    .expect("this function shouldn't fail");
            }
        }
    }
}

/// Get register encoding (3-bit code and REX bit)
fn register_encoding(reg: X86Register) -> (u8, bool) {
    match reg {
        X86Register::Rax => (0, false),
        X86Register::Rcx => (1, false),
        X86Register::Rdx => (2, false),
        X86Register::Rbx => (3, false),
        X86Register::Rsp => (4, false),
        X86Register::Rbp => (5, false),
        X86Register::Rsi => (6, false),
        X86Register::Rdi => (7, false),
        X86Register::R8 => (0, true),
        X86Register::R9 => (1, true),
        X86Register::R10 => (2, true),
        X86Register::R11 => (3, true),
        X86Register::R12 => (4, true),
        X86Register::R13 => (5, true),
        X86Register::R14 => (6, true),
        X86Register::R15 => (7, true),
    }
}

/// Check if a register needs REX prefix for byte operations
/// This is only for RSP/RBP/RSI/RDI to access SPL/BPL/SIL/DIL instead of AH/CH/DH/BH
/// R8-R15 already need REX through register_encoding() so they're not included here
fn needs_byte_rex(reg: X86Register) -> bool {
    matches!(
        reg,
        X86Register::Rsp | X86Register::Rbp | X86Register::Rsi | X86Register::Rdi
    )
}

/// Check if a register requires SIB byte as base
fn needs_sib_byte(reg: X86Register) -> bool {
    matches!(reg, X86Register::Rsp | X86Register::R12)
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}
