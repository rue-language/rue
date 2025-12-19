//! AArch64 instruction encoding.
//!
//! This phase converts Aarch64Mir instructions (with physical registers) to
//! machine code bytes.

use std::collections::HashMap;

use super::mir::{Aarch64Inst, Aarch64Mir, Cond, Reg};
use crate::EmittedRelocation;

/// A pending fixup for a forward branch.
struct Fixup {
    /// Offset of the instruction in the code.
    offset: usize,
    /// Target label name.
    label: String,
    /// Kind of branch (for calculating offset).
    kind: FixupKind,
}

#[derive(Clone, Copy, Debug)]
enum FixupKind {
    /// Unconditional branch (B)
    Branch,
    /// Conditional branch (B.cond, CBZ, CBNZ)
    CondBranch,
}

/// AArch64 instruction emitter.
pub struct Emitter<'a> {
    mir: &'a Aarch64Mir,
    code: Vec<u8>,
    relocations: Vec<EmittedRelocation>,
    labels: HashMap<String, usize>,
    fixups: Vec<Fixup>,
    num_locals: u32,
    num_params: u32,
    callee_saved: Vec<Reg>,
}

impl<'a> Emitter<'a> {
    /// Create a new emitter.
    pub fn new(mir: &'a Aarch64Mir, num_locals: u32, num_params: u32, callee_saved: &[Reg]) -> Self {
        Self {
            mir,
            code: Vec::new(),
            relocations: Vec::new(),
            labels: HashMap::new(),
            fixups: Vec::new(),
            num_locals,
            num_params,
            callee_saved: callee_saved.to_vec(),
        }
    }

    /// Calculate the total stack space used by callee-saved registers.
    /// On AArch64, registers are saved in pairs (16 bytes per pair).
    fn callee_saved_stack_size(&self) -> i32 {
        // Registers are saved in pairs, rounded up
        let num_regs = self.callee_saved.len();
        let pairs = (num_regs + 1) / 2;
        (pairs * 16) as i32
    }

    /// Emit machine code for all instructions.
    pub fn emit(mut self) -> (Vec<u8>, Vec<EmittedRelocation>) {
        if self.num_locals > 0 || self.num_params > 0 || !self.callee_saved.is_empty() {
            self.emit_prologue();
        }

        for inst in self.mir.iter() {
            self.emit_inst(inst);
        }

        self.apply_fixups();
        (self.code, self.relocations)
    }

    /// Emit function prologue.
    fn emit_prologue(&mut self) {
        // stp x29, x30, [sp, #-16]!   ; Save FP and LR
        self.emit_stp_pre(Reg::Fp, Reg::Lr, -16);

        // mov x29, sp                 ; Set up frame pointer
        self.emit_mov_rr(Reg::Fp, Reg::Sp);

        // Save callee-saved registers in pairs.
        // Each STP/STR pre-index instruction decrements SP by 16 bytes.
        let callee_saved = self.callee_saved.clone();
        let mut i = 0;
        while i + 1 < callee_saved.len() {
            // STP with pre-index: [SP, #-16]! decrements SP by 16 and stores the pair
            self.emit_stp_pre(callee_saved[i], callee_saved[i + 1], -16);
            i += 2;
        }
        // Handle odd register
        if i < callee_saved.len() {
            // STR with pre-index: [SP, #-16]! decrements SP by 16 and stores the register
            self.emit_str_pre(callee_saved[i], -16);
        }

        // Allocate space for locals and spilled params
        let total_slots = self.num_locals + self.num_params.min(8);
        if total_slots > 0 {
            let stack_size = ((total_slots as i32 * 8 + 15) / 16) * 16;
            if stack_size > 0 {
                self.emit_sub_imm(Reg::Sp, Reg::Sp, stack_size as u32);
            }
        }

        // Save incoming parameters from registers to the stack.
        // Parameters are stored after locals AND after callee-saved registers:
        //   [fp-16] = first callee-saved pair (or single reg)
        //   [fp-16-callee_saved_size-8] = local 0
        //   [fp-16-callee_saved_size-16] = local 1
        //   ...
        //   [fp-16-callee_saved_size-(num_locals+1)*8] = param 0
        //
        // AAPCS64: first 8 args in x0-x7
        let param_regs = [
            Reg::X0, Reg::X1, Reg::X2, Reg::X3, Reg::X4, Reg::X5, Reg::X6, Reg::X7,
        ];
        let callee_saved_size = self.callee_saved_stack_size();
        for i in 0..self.num_params.min(8) as usize {
            let slot = self.num_locals + i as u32;
            // Skip past callee-saved registers in the offset calculation
            let offset = -callee_saved_size - ((slot as i32 + 1) * 8);
            self.emit_str(param_regs[i], Reg::Fp, offset);
        }
    }

    /// Emit a single instruction.
    fn emit_inst(&mut self, inst: &Aarch64Inst) {
        match inst {
            Aarch64Inst::MovImm { dst, imm } => {
                let rd = dst.as_physical();
                self.emit_mov_imm(rd, *imm);
            }

            Aarch64Inst::MovRR { dst, src } => {
                let rd = dst.as_physical();
                let rs = src.as_physical();
                self.emit_mov_rr(rd, rs);
            }

            Aarch64Inst::Ldr { dst, base, offset } => {
                let rd = dst.as_physical();
                // Adjust offset for FP-relative accesses to account for callee-saved registers.
                // Lower.rs generates offsets assuming [fp-8] is the first slot, but callee-saved
                // registers are stored after fp is set, so we need to skip past them.
                let adjusted_offset = if *base == Reg::Fp && *offset < 0 {
                    let callee_saved_size = self.callee_saved_stack_size();
                    *offset - callee_saved_size
                } else {
                    *offset
                };
                self.emit_ldr(rd, *base, adjusted_offset);
            }

            Aarch64Inst::Str { src, base, offset } => {
                let rs = src.as_physical();
                // Adjust offset for FP-relative accesses (same as Ldr above).
                let adjusted_offset = if *base == Reg::Fp && *offset < 0 {
                    let callee_saved_size = self.callee_saved_stack_size();
                    *offset - callee_saved_size
                } else {
                    *offset
                };
                self.emit_str(rs, *base, adjusted_offset);
            }

            Aarch64Inst::AddRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_add_rr(rd, rn, rm, false);
            }

            Aarch64Inst::AddsRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_add_rr(rd, rn, rm, true);
            }

            Aarch64Inst::AddImm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_add_imm(rd, rn, *imm as u32);
            }

            Aarch64Inst::SubRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_sub_rr(rd, rn, rm, false);
            }

            Aarch64Inst::SubsRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_sub_rr(rd, rn, rm, true);
            }

            Aarch64Inst::SubImm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_sub_imm(rd, rn, *imm as u32);
            }

            Aarch64Inst::MulRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_mul(rd, rn, rm);
            }

            Aarch64Inst::SmullRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_smull(rd, rn, rm);
            }

            Aarch64Inst::SdivRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_sdiv(rd, rn, rm);
            }

            Aarch64Inst::Msub {
                dst,
                src1,
                src2,
                src3,
            } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                let ra = src3.as_physical();
                self.emit_msub(rd, rn, rm, ra);
            }

            Aarch64Inst::Neg { dst, src } => {
                let rd = dst.as_physical();
                let rm = src.as_physical();
                // NEG is SUB from XZR
                self.emit_sub_rr(rd, Reg::Xzr, rm, false);
            }

            Aarch64Inst::Negs { dst, src } => {
                let rd = dst.as_physical();
                let rm = src.as_physical();
                // NEGS is SUBS from WZR (32-bit for proper i32 overflow detection)
                self.emit_sub_rr(rd, Reg::Xzr, rm, true);
            }

            Aarch64Inst::AndRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_and_rr(rd, rn, rm);
            }

            Aarch64Inst::OrrRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_orr_rr(rd, rn, rm);
            }

            Aarch64Inst::EorRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_eor_rr(rd, rn, rm);
            }

            Aarch64Inst::EorImm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_eor_imm(rd, rn, *imm);
            }

            Aarch64Inst::CmpRR { src1, src2 } => {
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                // CMP is SUBS with XZR destination (32-bit form for i32)
                self.emit_sub_rr(Reg::Xzr, rn, rm, true);
            }

            Aarch64Inst::Cmp64RR { src1, src2 } => {
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                // 64-bit CMP is SUBS with XZR destination (64-bit form)
                self.emit_sub64_rr(Reg::Xzr, rn, rm, true);
            }

            Aarch64Inst::CmpImm { src, imm } => {
                let rn = src.as_physical();
                self.emit_cmp_imm(rn, *imm as u32);
            }

            Aarch64Inst::Cbz { src, label } => {
                let rt = src.as_physical();
                self.emit_cbz(rt, label, false);
            }

            Aarch64Inst::Cbnz { src, label } => {
                let rt = src.as_physical();
                self.emit_cbz(rt, label, true);
            }

            Aarch64Inst::Cset { dst, cond } => {
                let rd = dst.as_physical();
                self.emit_cset(rd, *cond);
            }

            Aarch64Inst::TstRR { src1, src2 } => {
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                // TST is ANDS with XZR destination
                self.emit_ands_rr(Reg::Xzr, rn, rm);
            }

            Aarch64Inst::Sxtb { dst, src } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_sbfm(rd, rn, 0, 7);
            }

            Aarch64Inst::Sxth { dst, src } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_sbfm(rd, rn, 0, 15);
            }

            Aarch64Inst::Sxtw { dst, src } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_sbfm(rd, rn, 0, 31);
            }

            Aarch64Inst::Uxtb { dst, src } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_ubfm(rd, rn, 0, 7);
            }

            Aarch64Inst::Uxth { dst, src } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_ubfm(rd, rn, 0, 15);
            }

            Aarch64Inst::B { label } => {
                self.emit_b(label);
            }

            Aarch64Inst::BCond { cond, label } => {
                self.emit_bcond(*cond, label);
            }

            Aarch64Inst::Bvs { label } => {
                // B.VS = branch if overflow set (cond = 0110)
                self.emit_bcond_raw(6, label);
            }

            Aarch64Inst::Bvc { label } => {
                // B.VC = branch if overflow clear (cond = 0111)
                self.emit_bcond_raw(7, label);
            }

            Aarch64Inst::Label { name } => {
                self.labels.insert(name.clone(), self.code.len());
            }

            Aarch64Inst::Bl { symbol } => {
                self.emit_bl(symbol);
            }

            Aarch64Inst::Ret => {
                self.emit_epilogue();
                self.emit_ret();
            }

            Aarch64Inst::StpPre { src1, src2, offset } => {
                let rt1 = src1.as_physical();
                let rt2 = src2.as_physical();
                self.emit_stp_pre(rt1, rt2, *offset);
            }

            Aarch64Inst::LdpPost { dst1, dst2, offset } => {
                let rt1 = dst1.as_physical();
                let rt2 = dst2.as_physical();
                self.emit_ldp_post(rt1, rt2, *offset);
            }
        }
    }

    /// Emit function epilogue (restore SP from FP, restore callee-saved, LDP FP/LR).
    fn emit_epilogue(&mut self) {
        // Stack layout after prologue:
        //   FP -> [saved FP/LR]
        //   FP-16 -> [callee-saved pair 1]
        //   FP-32 -> [callee-saved pair 2 or odd reg]
        //   ...
        //   FP-callee_saved_size -> [locals]
        //   SP -> bottom of stack frame
        //
        // We need to restore SP to point just below the callee-saved area
        // so the post-increment pops work correctly.

        // Add to SP to deallocate locals (restore SP to FP - callee_saved_size)
        // which is where SP was right after pushing all callee-saved registers
        if self.num_locals > 0 || self.num_params > 0 {
            let total_slots = self.num_locals + self.num_params.min(8);
            let stack_size = ((total_slots as i32 * 8 + 15) / 16) * 16;
            if stack_size > 0 {
                self.emit_add_imm(Reg::Sp, Reg::Sp, stack_size as u32);
            }
        }

        // Restore callee-saved registers (in reverse order)
        let callee_saved = self.callee_saved.clone();
        let num_pairs = callee_saved.len() / 2;
        let has_odd = callee_saved.len() % 2 == 1;

        // Odd register first (was pushed last)
        if has_odd {
            let idx = callee_saved.len() - 1;
            self.emit_ldr_post(callee_saved[idx], 16);
        }

        // Pairs in reverse
        for i in (0..num_pairs).rev() {
            let idx = i * 2;
            self.emit_ldp_post(callee_saved[idx], callee_saved[idx + 1], 16);
        }

        // ldp x29, x30, [sp], #16     ; Restore FP and LR
        self.emit_ldp_post(Reg::Fp, Reg::Lr, 16);
    }

    /// Apply pending fixups for forward branches.
    fn apply_fixups(&mut self) {
        for fixup in &self.fixups {
            if let Some(&target) = self.labels.get(&fixup.label) {
                let offset = (target as i64 - fixup.offset as i64) / 4;
                match fixup.kind {
                    FixupKind::Branch => {
                        // B instruction: imm26
                        let inst =
                            u32::from_le_bytes(self.code[fixup.offset..fixup.offset + 4].try_into().unwrap());
                        let new_inst = (inst & 0xFC000000) | ((offset as u32) & 0x03FFFFFF);
                        self.code[fixup.offset..fixup.offset + 4]
                            .copy_from_slice(&new_inst.to_le_bytes());
                    }
                    FixupKind::CondBranch => {
                        // Conditional branch: imm19
                        let inst =
                            u32::from_le_bytes(self.code[fixup.offset..fixup.offset + 4].try_into().unwrap());
                        let new_inst = (inst & 0xFF00001F) | (((offset as u32) & 0x7FFFF) << 5);
                        self.code[fixup.offset..fixup.offset + 4]
                            .copy_from_slice(&new_inst.to_le_bytes());
                    }
                }
            }
        }
    }

    // ========== Instruction encoding helpers ==========

    fn emit_u32(&mut self, inst: u32) {
        self.code.extend_from_slice(&inst.to_le_bytes());
    }

    fn emit_mov_imm(&mut self, rd: Reg, imm: i64) {
        // For now, use a simple MOVZ/MOVK sequence
        // MOVZ loads 16 bits and zeros the rest
        // MOVK keeps other bits and loads 16 bits

        let imm = imm as u64;

        // MOVZ Xd, #imm16, LSL #0
        let inst = 0xD2800000 | ((imm & 0xFFFF) << 5) as u32 | rd.encoding() as u32;
        self.emit_u32(inst);

        // If more bits needed, use MOVK
        if (imm >> 16) & 0xFFFF != 0 {
            let inst =
                0xF2A00000 | (((imm >> 16) & 0xFFFF) << 5) as u32 | rd.encoding() as u32;
            self.emit_u32(inst);
        }
        if (imm >> 32) & 0xFFFF != 0 {
            let inst =
                0xF2C00000 | (((imm >> 32) & 0xFFFF) << 5) as u32 | rd.encoding() as u32;
            self.emit_u32(inst);
        }
        if (imm >> 48) & 0xFFFF != 0 {
            let inst =
                0xF2E00000 | (((imm >> 48) & 0xFFFF) << 5) as u32 | rd.encoding() as u32;
            self.emit_u32(inst);
        }
    }

    fn emit_mov_rr(&mut self, rd: Reg, rs: Reg) {
        // Handle SP specially: in data processing instructions, register 31 is XZR, not SP.
        // To copy SP to another register (or vice versa), use ADD Xd, Xn, #0.
        if rs == Reg::Sp || rd == Reg::Sp {
            // ADD Xd, Xn, #0 (immediate)
            // In ADD immediate, register 31 in Rn position is SP
            self.emit_add_imm(rd, rs, 0);
        } else {
            // MOV Xd, Xn is encoded as ORR Xd, XZR, Xn
            let inst = 0xAA0003E0 | (rs.encoding() as u32) << 16 | rd.encoding() as u32;
            self.emit_u32(inst);
        }
    }

    fn emit_ldr(&mut self, rd: Reg, base: Reg, offset: i32) {
        // LDR Xt, [Xn, #imm]
        // Scaled offset (divide by 8 for 64-bit)
        if offset % 8 == 0 && offset >= 0 && offset < 32768 {
            let imm12 = (offset / 8) as u32;
            let inst = 0xF9400000
                | (imm12 << 10)
                | (base.encoding() as u32) << 5
                | rd.encoding() as u32;
            self.emit_u32(inst);
        } else {
            // Use unscaled offset
            let imm9 = (offset as u32) & 0x1FF;
            let inst = 0xF8400000
                | (imm9 << 12)
                | (base.encoding() as u32) << 5
                | rd.encoding() as u32;
            self.emit_u32(inst);
        }
    }

    fn emit_str(&mut self, rs: Reg, base: Reg, offset: i32) {
        // STR Xt, [Xn, #imm]
        if offset % 8 == 0 && offset >= 0 && offset < 32768 {
            let imm12 = (offset / 8) as u32;
            let inst = 0xF9000000
                | (imm12 << 10)
                | (base.encoding() as u32) << 5
                | rs.encoding() as u32;
            self.emit_u32(inst);
        } else {
            // Use unscaled offset (STUR)
            let imm9 = (offset as u32) & 0x1FF;
            let inst = 0xF8000000
                | (imm9 << 12)
                | (base.encoding() as u32) << 5
                | rs.encoding() as u32;
            self.emit_u32(inst);
        }
    }

    fn emit_str_pre(&mut self, rs: Reg, offset: i32) {
        // STR Xt, [SP, #imm]!
        let imm9 = (offset as u32) & 0x1FF;
        let inst = 0xF8000C00
            | (imm9 << 12)
            | (Reg::Sp.encoding() as u32) << 5
            | rs.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_ldr_post(&mut self, rd: Reg, offset: i32) {
        // LDR Xt, [SP], #imm
        let imm9 = (offset as u32) & 0x1FF;
        let inst = 0xF8400400
            | (imm9 << 12)
            | (Reg::Sp.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_stp_pre(&mut self, rt1: Reg, rt2: Reg, offset: i32) {
        // STP Xt1, Xt2, [SP, #imm]!
        let imm7 = ((offset / 8) as u32) & 0x7F;
        let inst = 0xA9800000
            | (imm7 << 15)
            | (rt2.encoding() as u32) << 10
            | (Reg::Sp.encoding() as u32) << 5
            | rt1.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_ldp_post(&mut self, rt1: Reg, rt2: Reg, offset: i32) {
        // LDP Xt1, Xt2, [SP], #imm
        let imm7 = ((offset / 8) as u32) & 0x7F;
        let inst = 0xA8C00000
            | (imm7 << 15)
            | (rt2.encoding() as u32) << 10
            | (Reg::Sp.encoding() as u32) << 5
            | rt1.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_add_rr(&mut self, rd: Reg, rn: Reg, rm: Reg, set_flags: bool) {
        // When set_flags is true, use 32-bit (W) form for proper i32 overflow detection.
        // ADD Xd, Xn, Xm:  0x8B000000 (64-bit)
        // ADD Wd, Wn, Wm:  0x0B000000 (32-bit)
        // ADDS Xd, Xn, Xm: 0xAB000000 (64-bit)
        // ADDS Wd, Wn, Wm: 0x2B000000 (32-bit)
        let base = if set_flags { 0x2B000000 } else { 0x8B000000 };
        let inst = base
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_add_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        // ADD Xd, Xn, #imm
        let inst = 0x91000000
            | ((imm & 0xFFF) << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sub_rr(&mut self, rd: Reg, rn: Reg, rm: Reg, set_flags: bool) {
        // When set_flags is true, use 32-bit (W) form for proper i32 overflow detection.
        // SUB Xd, Xn, Xm:  0xCB000000 (64-bit)
        // SUB Wd, Wn, Wm:  0x4B000000 (32-bit)
        // SUBS Xd, Xn, Xm: 0xEB000000 (64-bit)
        // SUBS Wd, Wn, Wm: 0x6B000000 (32-bit)
        let base = if set_flags { 0x6B000000 } else { 0xCB000000 };
        let inst = base
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sub64_rr(&mut self, rd: Reg, rn: Reg, rm: Reg, set_flags: bool) {
        // 64-bit subtract for comparing 64-bit values (e.g., SMULL results).
        // SUB Xd, Xn, Xm:  0xCB000000 (64-bit)
        // SUBS Xd, Xn, Xm: 0xEB000000 (64-bit with flags)
        let base = if set_flags { 0xEB000000 } else { 0xCB000000 };
        let inst = base
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sub_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        // SUB Xd, Xn, #imm
        let inst = 0xD1000000
            | ((imm & 0xFFF) << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_mul(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // MUL Xd, Xn, Xm (alias for MADD Xd, Xn, Xm, XZR)
        let inst = 0x9B007C00
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_smull(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // SMULL Xd, Wn, Wm
        let inst = 0x9B207C00
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sdiv(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // SDIV Wd, Wn, Wm (32-bit for proper i32 signed division)
        // 64-bit: 0x9AC00C00
        // 32-bit: 0x1AC00C00
        let inst = 0x1AC00C00
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_msub(&mut self, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        // MSUB Wd, Wn, Wm, Wa (32-bit for proper i32 arithmetic)
        // 64-bit: 0x9B008000
        // 32-bit: 0x1B008000
        let inst = 0x1B008000
            | (rm.encoding() as u32) << 16
            | (ra.encoding() as u32) << 10
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_and_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // AND Xd, Xn, Xm
        let inst = 0x8A000000
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_ands_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // ANDS Xd, Xn, Xm
        let inst = 0xEA000000
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_orr_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // ORR Xd, Xn, Xm
        let inst = 0xAA000000
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_eor_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // EOR Xd, Xn, Xm
        let inst = 0xCA000000
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_eor_imm(&mut self, rd: Reg, rn: Reg, imm: u64) {
        // EOR Xd, Xn, #imm - uses bitmask immediate encoding
        // For simplicity, only handle simple patterns
        // For #1, the encoding is N=1, immr=0, imms=0
        if imm == 1 {
            let inst = 0xD2400000 | (rn.encoding() as u32) << 5 | rd.encoding() as u32;
            self.emit_u32(inst);
        } else {
            // Fallback: use a temp register
            self.emit_mov_imm(Reg::X9, imm as i64);
            self.emit_eor_rr(rd, rn, Reg::X9);
        }
    }

    fn emit_cmp_imm(&mut self, rn: Reg, imm: u32) {
        // CMP Xn, #imm (alias for SUBS XZR, Xn, #imm)
        let inst = 0xF1000000
            | ((imm & 0xFFF) << 10)
            | (rn.encoding() as u32) << 5
            | Reg::Xzr.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_cbz(&mut self, rt: Reg, label: &str, is_nz: bool) {
        // CBZ/CBNZ Xt, label
        let op = if is_nz { 1 } else { 0 };
        let offset = self.code.len();

        // Placeholder instruction
        let inst = 0xB4000000 | (op << 24) | rt.encoding() as u32;
        self.emit_u32(inst);

        self.fixups.push(Fixup {
            offset,
            label: label.to_string(),
            kind: FixupKind::CondBranch,
        });
    }

    fn emit_cset(&mut self, rd: Reg, cond: Cond) {
        // CSET Xd, cond (alias for CSINC Xd, XZR, XZR, invert(cond))
        let inv_cond = cond.invert().encoding();
        let inst = 0x9A9F07E0 | (inv_cond as u32) << 12 | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sbfm(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        // SBFM Xd, Xn, #immr, #imms (used for SXTB, SXTH, SXTW)
        let inst = 0x93400000
            | (immr << 16)
            | (imms << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_ubfm(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        // UBFM Xd, Xn, #immr, #imms (used for UXTB, UXTH)
        let inst = 0xD3400000
            | (immr << 16)
            | (imms << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_b(&mut self, label: &str) {
        // B label
        let offset = self.code.len();
        let inst = 0x14000000;
        self.emit_u32(inst);

        self.fixups.push(Fixup {
            offset,
            label: label.to_string(),
            kind: FixupKind::Branch,
        });
    }

    fn emit_bcond(&mut self, cond: Cond, label: &str) {
        let offset = self.code.len();
        let inst = 0x54000000 | cond.encoding() as u32;
        self.emit_u32(inst);

        self.fixups.push(Fixup {
            offset,
            label: label.to_string(),
            kind: FixupKind::CondBranch,
        });
    }

    fn emit_bcond_raw(&mut self, cond: u8, label: &str) {
        let offset = self.code.len();
        let inst = 0x54000000 | cond as u32;
        self.emit_u32(inst);

        self.fixups.push(Fixup {
            offset,
            label: label.to_string(),
            kind: FixupKind::CondBranch,
        });
    }

    fn emit_bl(&mut self, symbol: &str) {
        // BL symbol - requires relocation
        let offset = self.code.len();
        let inst = 0x94000000;
        self.emit_u32(inst);

        self.relocations.push(EmittedRelocation {
            offset: offset as u64,
            symbol: symbol.to_string(),
            addend: 0,
        });
    }

    fn emit_ret(&mut self) {
        // RET (branch to LR)
        let inst = 0xD65F03C0;
        self.emit_u32(inst);
    }
}
