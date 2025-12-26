//! AArch64 instruction encoding.
//!
//! This phase converts Aarch64Mir instructions (with physical registers) to
//! machine code bytes.

use std::collections::HashMap;

use rue_error::{CompileError, CompileResult, ErrorKind};

use super::mir::{Aarch64Inst, Aarch64Mir, Cond, LabelId, Reg};
use crate::EmittedRelocation;

// ========== AArch64 Instruction Encoding Constants ==========
//
// These constants represent the base opcodes for AArch64 instructions.
// The format is: OPCODE_<mnemonic>_<variant>
//
// Reference: ARM Architecture Reference Manual for A-profile architecture

// Move instructions
/// MOVZ Xd, #imm16 - Move wide with zero (64-bit)
const OPCODE_MOVZ_X: u32 = 0xD2800000;
/// MOVN Xd, #imm16 - Move wide with NOT (64-bit)
const OPCODE_MOVN_X: u32 = 0x92800000;
/// MOVK Xd, #imm16, LSL #0 - Move wide with keep (64-bit, shift 0)
const OPCODE_MOVK_X_LSL0: u32 = 0xF2800000;
/// MOVK Xd, #imm16, LSL #16 - Move wide with keep (64-bit, shift 16)
const OPCODE_MOVK_X_LSL16: u32 = 0xF2A00000;
/// MOVK Xd, #imm16, LSL #32 - Move wide with keep (64-bit, shift 32)
const OPCODE_MOVK_X_LSL32: u32 = 0xF2C00000;
/// MOVK Xd, #imm16, LSL #48 - Move wide with keep (64-bit, shift 48)
const OPCODE_MOVK_X_LSL48: u32 = 0xF2E00000;
/// ORR Xd, XZR, Xm - MOV alias (64-bit register move)
const OPCODE_MOV_RR: u32 = 0xAA0003E0;

// Load/Store instructions
/// LDR Xt, [Xn, #imm12] - Load register (unsigned offset, 64-bit)
const OPCODE_LDR_UOFF: u32 = 0xF9400000;
/// LDUR Xt, [Xn, #simm9] - Load register (unscaled offset, 64-bit)
const OPCODE_LDUR: u32 = 0xF8400000;
/// STR Xt, [Xn, #imm12] - Store register (unsigned offset, 64-bit)
const OPCODE_STR_UOFF: u32 = 0xF9000000;
/// STUR Xt, [Xn, #simm9] - Store register (unscaled offset, 64-bit)
const OPCODE_STUR: u32 = 0xF8000000;
/// STR Xt, [SP, #simm9]! - Store register (pre-index)
const OPCODE_STR_PRE: u32 = 0xF8000C00;
/// LDR Xt, [SP], #simm9 - Load register (post-index)
const OPCODE_LDR_POST: u32 = 0xF8400400;
/// STP Xt1, Xt2, [SP, #simm7]! - Store pair (pre-index, 64-bit)
const OPCODE_STP_PRE: u32 = 0xA9800000;
/// LDP Xt1, Xt2, [SP], #simm7 - Load pair (post-index, 64-bit)
const OPCODE_LDP_POST: u32 = 0xA8C00000;

// Arithmetic instructions (64-bit)
/// ADD Xd, Xn, Xm - Add (64-bit, no flags)
const OPCODE_ADD_X: u32 = 0x8B000000;
/// ADD Xd, Xn, #imm12 - Add immediate (64-bit)
const OPCODE_ADD_IMM_X: u32 = 0x91000000;
/// SUB Xd, Xn, Xm - Subtract (64-bit, no flags)
const OPCODE_SUB_X: u32 = 0xCB000000;
/// SUB Xd, Xn, #imm12 - Subtract immediate (64-bit)
const OPCODE_SUB_IMM_X: u32 = 0xD1000000;
/// SUBS Xd, Xn, Xm - Subtract and set flags (64-bit)
const OPCODE_SUBS_X: u32 = 0xEB000000;
/// MUL Xd, Xn, Xm - Multiply (alias for MADD Xd, Xn, Xm, XZR)
const OPCODE_MUL_X: u32 = 0x9B007C00;
/// SMULL Xd, Wn, Wm - Signed multiply long (32->64)
const OPCODE_SMULL: u32 = 0x9B207C00;

// Arithmetic instructions (32-bit, for i32 operations)
/// ADDS Wd, Wn, Wm - Add and set flags (32-bit)
const OPCODE_ADDS_W: u32 = 0x2B000000;
/// SUBS Wd, Wn, Wm - Subtract and set flags (32-bit)
const OPCODE_SUBS_W: u32 = 0x6B000000;
// Arithmetic instructions (64-bit, for i64/u64 operations)
/// ADDS Xd, Xn, Xm - Add and set flags (64-bit)
const OPCODE_ADDS_X: u32 = 0xAB000000;
/// SDIV Wd, Wn, Wm - Signed divide (32-bit)
const OPCODE_SDIV_W: u32 = 0x1AC00C00;
/// MSUB Wd, Wn, Wm, Wa - Multiply-subtract (32-bit)
const OPCODE_MSUB_W: u32 = 0x1B008000;

// Logical instructions
/// AND Xd, Xn, Xm - Bitwise AND (64-bit)
const OPCODE_AND_X: u32 = 0x8A000000;
/// ANDS Xd, Xn, Xm - Bitwise AND and set flags (64-bit)
const OPCODE_ANDS_X: u32 = 0xEA000000;
/// ORR Xd, Xn, Xm - Bitwise OR (64-bit)
const OPCODE_ORR_X: u32 = 0xAA000000;
/// EOR Xd, Xn, Xm - Bitwise XOR (64-bit)
const OPCODE_EOR_X: u32 = 0xCA000000;
/// EOR Xd, Xn, #imm - Bitwise XOR with bitmask immediate (64-bit, N=1)
const OPCODE_EOR_IMM_X: u32 = 0xD2400000;
/// MVN Xd, Xm - Bitwise NOT (alias for ORN Xd, XZR, Xm)
const OPCODE_ORN_X: u32 = 0xAA200000;
/// LSLV Xd, Xn, Xm - Logical shift left variable (64-bit)
const OPCODE_LSLV_X: u32 = 0x9AC02000;
/// LSLV Wd, Wn, Wm - Logical shift left variable (32-bit)
const OPCODE_LSLV_W: u32 = 0x1AC02000;
/// LSRV Xd, Xn, Xm - Logical shift right variable (64-bit)
const OPCODE_LSRV_X: u32 = 0x9AC02400;
/// LSRV Wd, Wn, Wm - Logical shift right variable (32-bit)
const OPCODE_LSRV_W: u32 = 0x1AC02400;
/// ASRV Xd, Xn, Xm - Arithmetic shift right variable (64-bit)
const OPCODE_ASRV_X: u32 = 0x9AC02800;
/// ASRV Wd, Wn, Wm - Arithmetic shift right variable (32-bit)
const OPCODE_ASRV_W: u32 = 0x1AC02800;

// Compare instructions
/// CMP Xn, #imm12 - Compare immediate (alias for SUBS XZR, Xn, #imm12)
const OPCODE_CMP_IMM_X: u32 = 0xF1000000;

// PC-relative addressing
/// ADRP Xd, label - PC-relative address to 4KB page
const OPCODE_ADRP: u32 = 0x90000000;

// Branch instructions
/// B label - Unconditional branch
const OPCODE_B: u32 = 0x14000000;
/// B.cond label - Conditional branch
const OPCODE_BCOND: u32 = 0x54000000;
/// CBZ Xt, label - Compare and branch if zero (64-bit)
const OPCODE_CBZ_X: u32 = 0xB4000000;
/// BL symbol - Branch with link
const OPCODE_BL: u32 = 0x94000000;
/// RET - Return (branch to LR)
const OPCODE_RET: u32 = 0xD65F03C0;

// Conditional select
/// CSINC Xd, XZR, XZR, invert(cond) - CSET alias
const OPCODE_CSINC_CSET: u32 = 0x9A9F07E0;

// Bit field instructions
/// SBFM Xd, Xn, #immr, #imms - Signed bit field move (64-bit)
const OPCODE_SBFM_X: u32 = 0x93400000;
/// UBFM Xd, Xn, #immr, #imms - Unsigned bit field move (64-bit)
const OPCODE_UBFM_X: u32 = 0xD3400000;
/// SBFM Wd, Wn, #immr, #imms - Signed bit field move (32-bit)
const OPCODE_SBFM_W: u32 = 0x13000000;
/// UBFM Wd, Wn, #immr, #imms - Unsigned bit field move (32-bit)
const OPCODE_UBFM_W: u32 = 0x53000000;
/// LSR Xd, Xn, #imm - Logical shift right (64-bit, encoded as UBFM with imms=63)
const OPCODE_LSR_X: u32 = 0xD340FC00;
/// ASR Xd, Xn, #imm - Arithmetic shift right (64-bit, encoded as SBFM with imms=63)
const OPCODE_ASR_X: u32 = 0x9340FC00;

// Multiply-accumulate instructions
/// UMULL Xd, Wn, Wm - Unsigned multiply long (32->64, alias for UMADDL Xd, Wn, Wm, XZR)
const OPCODE_UMULL: u32 = 0x9BA07C00;
/// SMULH Xd, Xn, Xm - Signed multiply high (upper 64 bits of 64x64)
const OPCODE_SMULH: u32 = 0x9B407C00;
/// UMULH Xd, Xn, Xm - Unsigned multiply high (upper 64 bits of 64x64)
const OPCODE_UMULH: u32 = 0x9BC07C00;

// Immediate masks for instruction encoding
/// Mask for 16-bit immediate chunks
const IMM16_MASK: u64 = 0xFFFF;
/// Branch offset mask for B instruction (imm26)
const BRANCH_OFFSET_MASK: u32 = 0x03FFFFFF;
/// Branch opcode mask for B instruction
const BRANCH_OPCODE_MASK: u32 = 0xFC000000;
/// Conditional branch offset mask (imm19)
const COND_BRANCH_OFFSET_MASK: u32 = 0x7FFFF;
/// Conditional branch opcode mask
const COND_BRANCH_OPCODE_MASK: u32 = 0xFF00001F;

/// A pending fixup for a forward branch.
struct Fixup {
    /// Offset of the instruction in the code.
    offset: usize,
    /// Target label ID.
    label: LabelId,
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
    labels: HashMap<LabelId, usize>,
    fixups: Vec<Fixup>,
    num_locals: u32,
    num_params: u32,
    callee_saved: Vec<Reg>,
    /// Whether a stack frame was emitted (prologue was executed).
    has_frame: bool,
    /// String constants (for StringConstPtr/StringConstLen)
    strings: &'a [String],
}

impl<'a> Emitter<'a> {
    /// Create a new emitter.
    pub fn new(
        mir: &'a Aarch64Mir,
        num_locals: u32,
        num_params: u32,
        callee_saved: &[Reg],
        strings: &'a [String],
    ) -> Self {
        Self {
            mir,
            code: Vec::new(),
            relocations: Vec::new(),
            labels: HashMap::new(),
            fixups: Vec::new(),
            num_locals,
            num_params,
            callee_saved: callee_saved.to_vec(),
            has_frame: false,
            strings,
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
    pub fn emit(mut self) -> CompileResult<(Vec<u8>, Vec<EmittedRelocation>)> {
        // Verify no LdrIndexed/StrIndexed variants survived into emission
        // These should have been lowered by regalloc into Ldr/Str with physical registers
        for (i, inst) in self.mir.iter().enumerate() {
            if matches!(
                inst,
                Aarch64Inst::LdrIndexed { .. }
                    | Aarch64Inst::StrIndexed { .. }
                    | Aarch64Inst::LdrIndexedOffset { .. }
                    | Aarch64Inst::StrIndexedOffset { .. }
            ) {
                return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
                    format!(
                        "post-regalloc verification failed: instruction {} is {:?}, \
                         which should have been lowered by regalloc",
                        i, inst
                    ),
                )));
            }
        }

        if self.num_locals > 0 || self.num_params > 0 || !self.callee_saved.is_empty() {
            self.has_frame = true;
            self.emit_prologue();
        }

        for inst in self.mir.iter() {
            self.emit_inst(inst);
        }

        self.apply_fixups();
        Ok((self.code, self.relocations))
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
            Reg::X0,
            Reg::X1,
            Reg::X2,
            Reg::X3,
            Reg::X4,
            Reg::X5,
            Reg::X6,
            Reg::X7,
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
                self.emit_adds_rr32(rd, rn, rm);
            }

            Aarch64Inst::AddsRR64 { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_adds_rr64(rd, rn, rm);
            }

            Aarch64Inst::AddImm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                // Adjust offset for FP-relative address calculations (same as Ldr/Str).
                // This is used when computing addresses of locals for inout arguments.
                let adjusted_imm = if rn == Reg::Fp && *imm < 0 {
                    let callee_saved_size = self.callee_saved_stack_size();
                    *imm - callee_saved_size
                } else {
                    *imm
                };
                if adjusted_imm < 0 {
                    // Negative immediate: use SUB with the absolute value
                    self.emit_sub_imm(rd, rn, (-adjusted_imm) as u32);
                } else {
                    self.emit_add_imm(rd, rn, adjusted_imm as u32);
                }
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
                self.emit_subs_rr32(rd, rn, rm);
            }

            Aarch64Inst::SubsRR64 { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_subs_rr64(rd, rn, rm);
            }

            Aarch64Inst::SubImm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                // Adjust immediate for FP-relative addresses to account for callee-saved registers.
                // When computing addresses relative to FP, we need to skip past the callee-saved
                // register save area.
                let adjusted_imm = if rn == Reg::Fp {
                    *imm as u32 + self.callee_saved_stack_size() as u32
                } else {
                    *imm as u32
                };
                self.emit_sub_imm(rd, rn, adjusted_imm);
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

            Aarch64Inst::UmullRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_umull(rd, rn, rm);
            }

            Aarch64Inst::SmulhRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_smulh(rd, rn, rm);
            }

            Aarch64Inst::UmulhRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_umulh(rd, rn, rm);
            }

            Aarch64Inst::Lsr64Imm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_lsr_imm(rd, rn, *imm);
            }

            Aarch64Inst::Asr64Imm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_asr_imm(rd, rn, *imm);
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
                // NEGS is SUBS from XZR (64-bit for i64/u64 overflow detection)
                self.emit_subs_rr64(rd, Reg::Xzr, rm);
            }

            Aarch64Inst::Negs32 { dst, src } => {
                let rd = dst.as_physical();
                let rm = src.as_physical();
                // NEGS is SUBS from WZR (32-bit for i32/u32 and sub-word overflow detection)
                self.emit_subs_rr32(rd, Reg::Xzr, rm);
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

            Aarch64Inst::MvnRR { dst, src } => {
                let rd = dst.as_physical();
                let rm = src.as_physical();
                self.emit_mvn_rr(rd, rm);
            }

            Aarch64Inst::LslRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_lslv_rr(rd, rn, rm);
            }

            Aarch64Inst::Lsl32RR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_lslv32_rr(rd, rn, rm);
            }

            Aarch64Inst::LsrRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_lsrv_rr(rd, rn, rm);
            }

            Aarch64Inst::Lsr32RR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_lsrv32_rr(rd, rn, rm);
            }

            Aarch64Inst::AsrRR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_asrv_rr(rd, rn, rm);
            }

            Aarch64Inst::Asr32RR { dst, src1, src2 } => {
                let rd = dst.as_physical();
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                self.emit_asrv32_rr(rd, rn, rm);
            }

            Aarch64Inst::CmpRR { src1, src2 } => {
                let rn = src1.as_physical();
                let rm = src2.as_physical();
                // CMP is SUBS with WZR destination (32-bit form for i32 and sub-word types)
                self.emit_subs_rr32(Reg::Xzr, rn, rm);
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
                self.emit_cbz(rt, *label, false);
            }

            Aarch64Inst::Cbnz { src, label } => {
                let rt = src.as_physical();
                self.emit_cbz(rt, *label, true);
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
                self.emit_b(*label);
            }

            Aarch64Inst::BCond { cond, label } => {
                self.emit_bcond(*cond, *label);
            }

            Aarch64Inst::Bvs { label } => {
                // B.VS = branch if overflow set (cond = 0110)
                self.emit_bcond_raw(6, *label);
            }

            Aarch64Inst::Bvc { label } => {
                // B.VC = branch if overflow clear (cond = 0111)
                self.emit_bcond_raw(7, *label);
            }

            Aarch64Inst::Label { id } => {
                self.labels.insert(*id, self.code.len());
            }

            Aarch64Inst::Bl { symbol } => {
                self.emit_bl(symbol);
            }

            Aarch64Inst::Ret => {
                if self.has_frame {
                    self.emit_epilogue();
                }
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

            // These instructions should be caught by the verification at the start of emit()
            Aarch64Inst::LdrIndexed { .. }
            | Aarch64Inst::StrIndexed { .. }
            | Aarch64Inst::LdrIndexedOffset { .. }
            | Aarch64Inst::StrIndexedOffset { .. } => {
                unreachable!(
                    "LdrIndexed/StrIndexed variants should be caught by emit() verification"
                )
            }

            Aarch64Inst::LslImm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_lsl_imm(rd, rn, *imm);
            }

            Aarch64Inst::Lsl32Imm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_lsl32_imm(rd, rn, *imm);
            }

            Aarch64Inst::Lsr32Imm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_lsr32_imm(rd, rn, *imm);
            }

            Aarch64Inst::Asr32Imm { dst, src, imm } => {
                let rd = dst.as_physical();
                let rn = src.as_physical();
                self.emit_asr32_imm(rd, rn, *imm);
            }

            Aarch64Inst::StringConstPtr { dst, string_id } => {
                // Load string pointer using ADRP + ADD for PC-relative addressing
                // This requires relocations to be resolved by the linker
                let rd = dst.as_physical();
                let string_id = *string_id;

                // ADRP dst, <symbol>
                // This loads the 4KB-aligned page address of the symbol
                let offset = self.code.len();
                let adrp = OPCODE_ADRP | rd.encoding() as u32;
                self.emit_u32(adrp);

                // Record relocation for ADRP using the helper
                let symbol = format!(".rodata.str{}", string_id);
                self.relocations
                    .push(EmittedRelocation::aarch64_adrp(offset as u64, &symbol));

                // ADD dst, dst, <symbol>
                // This adds the offset within the page
                let offset = self.code.len();
                let add = OPCODE_ADD_IMM_X | (rd.encoding() as u32) << 5 | rd.encoding() as u32;
                self.emit_u32(add);

                // Record relocation for ADD using the helper
                self.relocations
                    .push(EmittedRelocation::aarch64_add_lo12(offset as u64, &symbol));
            }

            Aarch64Inst::StringConstLen { dst, string_id } => {
                // Load string length as an immediate
                // Look up the actual string to get its length
                let rd = dst.as_physical();
                let string_id = *string_id as usize;

                let len = if string_id < self.strings.len() {
                    self.strings[string_id].len() as i64
                } else {
                    // Invalid string_id - emit 0
                    0
                };

                // Emit the length as an immediate
                self.emit_mov_imm(rd, len);
            }

            Aarch64Inst::StringConstCap { dst, string_id: _ } => {
                // String literals have capacity 0 (rodata, not heap)
                // This distinguishes rodata strings from heap-allocated ones
                let rd = dst.as_physical();
                self.emit_mov_imm(rd, 0);
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
                        let inst = u32::from_le_bytes(
                            self.code[fixup.offset..fixup.offset + 4]
                                .try_into()
                                .unwrap(),
                        );
                        let new_inst =
                            (inst & BRANCH_OPCODE_MASK) | ((offset as u32) & BRANCH_OFFSET_MASK);
                        self.code[fixup.offset..fixup.offset + 4]
                            .copy_from_slice(&new_inst.to_le_bytes());
                    }
                    FixupKind::CondBranch => {
                        // Conditional branch: imm19
                        let inst = u32::from_le_bytes(
                            self.code[fixup.offset..fixup.offset + 4]
                                .try_into()
                                .unwrap(),
                        );
                        let new_inst = (inst & COND_BRANCH_OPCODE_MASK)
                            | (((offset as u32) & COND_BRANCH_OFFSET_MASK) << 5);
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
        // Use MOVN for negative values that would encode more efficiently.
        // MOVN loads the inverted 16-bit immediate and inverts all bits.
        // For example, -1 (0xFFFFFFFFFFFFFFFF) can be encoded as MOVN Xd, #0
        // since ~0 = 0xFFFFFFFFFFFFFFFF.

        let uimm = imm as u64;

        // Check if MOVN would be more efficient by counting how many
        // 16-bit chunks are all 1s vs all 0s
        let chunks = [
            (uimm >> 0) & IMM16_MASK,
            (uimm >> 16) & IMM16_MASK,
            (uimm >> 32) & IMM16_MASK,
            (uimm >> 48) & IMM16_MASK,
        ];

        let zeros = chunks.iter().filter(|&&c| c == 0).count();
        let ones = chunks.iter().filter(|&&c| c == IMM16_MASK).count();

        if ones > zeros {
            // Use MOVN: find first chunk that isn't all 1s
            let inverted = !uimm;
            let inv_chunks = [
                (inverted >> 0) & IMM16_MASK,
                (inverted >> 16) & IMM16_MASK,
                (inverted >> 32) & IMM16_MASK,
                (inverted >> 48) & IMM16_MASK,
            ];

            // Find first non-zero inverted chunk for MOVN
            let (first_idx, first_val) = inv_chunks
                .iter()
                .enumerate()
                .find(|&(_, &v)| v != 0)
                .map(|(i, &v)| (i, v))
                .unwrap_or((0, 0));

            // MOVN Xd, #imm16, LSL #(first_idx * 16)
            let hw = first_idx as u32;
            let inst =
                OPCODE_MOVN_X | (hw << 21) | ((first_val as u32) << 5) | rd.encoding() as u32;
            self.emit_u32(inst);

            // Use MOVK for remaining non-IMM16_MASK chunks in original value
            for (i, &chunk) in chunks.iter().enumerate() {
                if i != first_idx && chunk != IMM16_MASK {
                    let base = match i {
                        0 => OPCODE_MOVK_X_LSL0,
                        1 => OPCODE_MOVK_X_LSL16,
                        2 => OPCODE_MOVK_X_LSL32,
                        3 => OPCODE_MOVK_X_LSL48,
                        _ => unreachable!(),
                    };
                    let inst = base | ((chunk as u32) << 5) | rd.encoding() as u32;
                    self.emit_u32(inst);
                }
            }
        } else {
            // Use MOVZ/MOVK sequence for non-negative or sparse values
            let inst = OPCODE_MOVZ_X | ((uimm & IMM16_MASK) << 5) as u32 | rd.encoding() as u32;
            self.emit_u32(inst);

            // If more bits needed, use MOVK
            if (uimm >> 16) & IMM16_MASK != 0 {
                let inst = OPCODE_MOVK_X_LSL16
                    | (((uimm >> 16) & IMM16_MASK) << 5) as u32
                    | rd.encoding() as u32;
                self.emit_u32(inst);
            }
            if (uimm >> 32) & IMM16_MASK != 0 {
                let inst = OPCODE_MOVK_X_LSL32
                    | (((uimm >> 32) & IMM16_MASK) << 5) as u32
                    | rd.encoding() as u32;
                self.emit_u32(inst);
            }
            if (uimm >> 48) & IMM16_MASK != 0 {
                let inst = OPCODE_MOVK_X_LSL48
                    | (((uimm >> 48) & IMM16_MASK) << 5) as u32
                    | rd.encoding() as u32;
                self.emit_u32(inst);
            }
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
            let inst = OPCODE_MOV_RR | (rs.encoding() as u32) << 16 | rd.encoding() as u32;
            self.emit_u32(inst);
        }
    }

    fn emit_ldr(&mut self, rd: Reg, base: Reg, offset: i32) {
        // LDR Xt, [Xn, #imm]
        // Scaled offset (divide by 8 for 64-bit)
        if offset % 8 == 0 && offset >= 0 && offset < 32768 {
            let imm12 = (offset / 8) as u32;
            let inst = OPCODE_LDR_UOFF
                | (imm12 << 10)
                | (base.encoding() as u32) << 5
                | rd.encoding() as u32;
            self.emit_u32(inst);
        } else {
            // Use unscaled offset (LDUR)
            let imm9 = (offset as u32) & 0x1FF;
            let inst =
                OPCODE_LDUR | (imm9 << 12) | (base.encoding() as u32) << 5 | rd.encoding() as u32;
            self.emit_u32(inst);
        }
    }

    fn emit_str(&mut self, rs: Reg, base: Reg, offset: i32) {
        // STR Xt, [Xn, #imm]
        if offset % 8 == 0 && offset >= 0 && offset < 32768 {
            let imm12 = (offset / 8) as u32;
            let inst = OPCODE_STR_UOFF
                | (imm12 << 10)
                | (base.encoding() as u32) << 5
                | rs.encoding() as u32;
            self.emit_u32(inst);
        } else {
            // Use unscaled offset (STUR)
            let imm9 = (offset as u32) & 0x1FF;
            let inst =
                OPCODE_STUR | (imm9 << 12) | (base.encoding() as u32) << 5 | rs.encoding() as u32;
            self.emit_u32(inst);
        }
    }

    fn emit_str_pre(&mut self, rs: Reg, offset: i32) {
        // STR Xt, [SP, #imm]!
        let imm9 = (offset as u32) & 0x1FF;
        let inst =
            OPCODE_STR_PRE | (imm9 << 12) | (Reg::Sp.encoding() as u32) << 5 | rs.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_ldr_post(&mut self, rd: Reg, offset: i32) {
        // LDR Xt, [SP], #imm
        let imm9 = (offset as u32) & 0x1FF;
        let inst = OPCODE_LDR_POST
            | (imm9 << 12)
            | (Reg::Sp.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_stp_pre(&mut self, rt1: Reg, rt2: Reg, offset: i32) {
        // STP Xt1, Xt2, [SP, #imm]!
        let imm7 = ((offset / 8) as u32) & 0x7F;
        let inst = OPCODE_STP_PRE
            | (imm7 << 15)
            | (rt2.encoding() as u32) << 10
            | (Reg::Sp.encoding() as u32) << 5
            | rt1.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_ldp_post(&mut self, rt1: Reg, rt2: Reg, offset: i32) {
        // LDP Xt1, Xt2, [SP], #imm
        let imm7 = ((offset / 8) as u32) & 0x7F;
        let inst = OPCODE_LDP_POST
            | (imm7 << 15)
            | (rt2.encoding() as u32) << 10
            | (Reg::Sp.encoding() as u32) << 5
            | rt1.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_add_rr(&mut self, rd: Reg, rn: Reg, rm: Reg, _set_flags: bool) {
        // Use 64-bit ADD (no flags)
        let inst = OPCODE_ADD_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_adds_rr32(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // ADDS Wd, Wn, Wm - 32-bit add with flags for i32 overflow detection
        let inst = OPCODE_ADDS_W
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_adds_rr64(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // ADDS Xd, Xn, Xm - 64-bit add with flags for i64/u64 overflow detection
        let inst = OPCODE_ADDS_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_add_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        // ADD Xd, Xn, #imm
        let inst = OPCODE_ADD_IMM_X
            | ((imm & 0xFFF) << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sub_rr(&mut self, rd: Reg, rn: Reg, rm: Reg, set_flags: bool) {
        // Use 64-bit SUB, optionally with flags
        let base = if set_flags {
            OPCODE_SUBS_X
        } else {
            OPCODE_SUB_X
        };
        let inst = base
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_subs_rr32(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // SUBS Wd, Wn, Wm - 32-bit subtract with flags for i32 overflow detection
        let inst = OPCODE_SUBS_W
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_subs_rr64(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // SUBS Xd, Xn, Xm - 64-bit subtract with flags for i64/u64 overflow detection
        let inst = OPCODE_SUBS_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sub64_rr(&mut self, rd: Reg, rn: Reg, rm: Reg, set_flags: bool) {
        // 64-bit subtract for comparing 64-bit values (e.g., SMULL results).
        let base = if set_flags {
            OPCODE_SUBS_X
        } else {
            OPCODE_SUB_X
        };
        let inst = base
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sub_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        // SUB Xd, Xn, #imm
        let inst = OPCODE_SUB_IMM_X
            | ((imm & 0xFFF) << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_mul(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // MUL Xd, Xn, Xm (alias for MADD Xd, Xn, Xm, XZR)
        let inst = OPCODE_MUL_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_smull(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // SMULL Xd, Wn, Wm
        let inst = OPCODE_SMULL
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_umull(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // UMULL Xd, Wn, Wm (unsigned multiply long 32x32->64)
        let inst = OPCODE_UMULL
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_smulh(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // SMULH Xd, Xn, Xm (high 64 bits of 64x64 signed multiply)
        let inst = OPCODE_SMULH
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_umulh(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // UMULH Xd, Xn, Xm (high 64 bits of 64x64 unsigned multiply)
        let inst = OPCODE_UMULH
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_lsr_imm(&mut self, rd: Reg, rn: Reg, imm: u8) {
        // LSR Xd, Xn, #imm (64-bit logical shift right by immediate)
        // Encoded as UBFM Xd, Xn, #imm, #63
        let inst = OPCODE_LSR_X
            | ((imm as u32 & 0x3F) << 16)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_asr_imm(&mut self, rd: Reg, rn: Reg, imm: u8) {
        // ASR Xd, Xn, #imm (64-bit arithmetic shift right by immediate)
        // Encoded as SBFM Xd, Xn, #imm, #63
        let inst = OPCODE_ASR_X
            | ((imm as u32 & 0x3F) << 16)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sdiv(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // SDIV Wd, Wn, Wm (32-bit for proper i32 signed division)
        let inst = OPCODE_SDIV_W
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_msub(&mut self, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        // MSUB Wd, Wn, Wm, Wa (32-bit for proper i32 arithmetic)
        let inst = OPCODE_MSUB_W
            | (rm.encoding() as u32) << 16
            | (ra.encoding() as u32) << 10
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_and_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // AND Xd, Xn, Xm
        let inst = OPCODE_AND_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_ands_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // ANDS Xd, Xn, Xm
        let inst = OPCODE_ANDS_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_orr_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // ORR Xd, Xn, Xm
        let inst = OPCODE_ORR_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_eor_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // EOR Xd, Xn, Xm
        let inst = OPCODE_EOR_X
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
            let inst = OPCODE_EOR_IMM_X | (rn.encoding() as u32) << 5 | rd.encoding() as u32;
            self.emit_u32(inst);
        } else {
            // Fallback: use X9 as a scratch register to load the immediate.
            // This is safe because X9 is a caller-saved scratch register that
            // is not used for register allocation (we only allocate X19-X28).
            self.emit_mov_imm(Reg::X9, imm as i64);
            self.emit_eor_rr(rd, rn, Reg::X9);
        }
    }

    fn emit_mvn_rr(&mut self, rd: Reg, rm: Reg) {
        // MVN Xd, Xm (alias for ORN Xd, XZR, Xm)
        let inst = OPCODE_ORN_X
            | (rm.encoding() as u32) << 16
            | (Reg::Xzr.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_lslv_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // LSLV Xd, Xn, Xm - Logical shift left variable (64-bit)
        let inst = OPCODE_LSLV_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_lslv32_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // LSLV Wd, Wn, Wm - Logical shift left variable (32-bit)
        let inst = OPCODE_LSLV_W
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_lsrv_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // LSRV Xd, Xn, Xm - Logical shift right variable (64-bit)
        let inst = OPCODE_LSRV_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_lsrv32_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // LSRV Wd, Wn, Wm - Logical shift right variable (32-bit)
        let inst = OPCODE_LSRV_W
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_asrv_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // ASRV Xd, Xn, Xm - Arithmetic shift right variable (64-bit)
        let inst = OPCODE_ASRV_X
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_asrv32_rr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        // ASRV Wd, Wn, Wm - Arithmetic shift right variable (32-bit)
        let inst = OPCODE_ASRV_W
            | (rm.encoding() as u32) << 16
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_cmp_imm(&mut self, rn: Reg, imm: u32) {
        // CMP Xn, #imm (alias for SUBS XZR, Xn, #imm)
        let inst = OPCODE_CMP_IMM_X
            | ((imm & 0xFFF) << 10)
            | (rn.encoding() as u32) << 5
            | Reg::Xzr.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_cbz(&mut self, rt: Reg, label: LabelId, is_nz: bool) {
        // CBZ/CBNZ Xt, label
        let op = if is_nz { 1 } else { 0 };
        let offset = self.code.len();

        // Placeholder instruction
        let inst = OPCODE_CBZ_X | (op << 24) | rt.encoding() as u32;
        self.emit_u32(inst);

        self.fixups.push(Fixup {
            offset,
            label,
            kind: FixupKind::CondBranch,
        });
    }

    fn emit_cset(&mut self, rd: Reg, cond: Cond) {
        // CSET Xd, cond (alias for CSINC Xd, XZR, XZR, invert(cond))
        let inv_cond = cond.invert().encoding();
        let inst = OPCODE_CSINC_CSET | (inv_cond as u32) << 12 | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_sbfm(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        // SBFM Xd, Xn, #immr, #imms (used for SXTB, SXTH, SXTW)
        let inst = OPCODE_SBFM_X
            | (immr << 16)
            | (imms << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_ubfm(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        // UBFM Xd, Xn, #immr, #imms (used for UXTB, UXTH)
        let inst = OPCODE_UBFM_X
            | (immr << 16)
            | (imms << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_b(&mut self, label: LabelId) {
        // B label
        let offset = self.code.len();
        self.emit_u32(OPCODE_B);

        self.fixups.push(Fixup {
            offset,
            label,
            kind: FixupKind::Branch,
        });
    }

    fn emit_bcond(&mut self, cond: Cond, label: LabelId) {
        let offset = self.code.len();
        let inst = OPCODE_BCOND | cond.encoding() as u32;
        self.emit_u32(inst);

        self.fixups.push(Fixup {
            offset,
            label,
            kind: FixupKind::CondBranch,
        });
    }

    fn emit_bcond_raw(&mut self, cond: u8, label: LabelId) {
        let offset = self.code.len();
        let inst = OPCODE_BCOND | cond as u32;
        self.emit_u32(inst);

        self.fixups.push(Fixup {
            offset,
            label,
            kind: FixupKind::CondBranch,
        });
    }

    fn emit_bl(&mut self, symbol: &str) {
        // BL symbol - requires relocation
        let offset = self.code.len();
        self.emit_u32(OPCODE_BL);

        // Record relocation using the helper
        self.relocations
            .push(EmittedRelocation::aarch64_call(offset as u64, symbol));
    }

    fn emit_ret(&mut self) {
        // RET (branch to LR)
        self.emit_u32(OPCODE_RET);
    }

    fn emit_lsl_imm(&mut self, rd: Reg, rn: Reg, shift: u8) {
        // LSL Xd, Xn, #shift is an alias for UBFM Xd, Xn, #(-shift mod 64), #(63-shift)
        // For 64-bit: UBFM with sf=1, N=1
        // immr = -shift mod 64 = (64 - shift) mod 64
        // imms = 63 - shift
        let shift = shift as u32;
        let immr = (64 - shift) & 0x3F;
        let imms = 63 - shift;
        let inst = OPCODE_UBFM_X
            | (immr << 16)
            | (imms << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_lsl32_imm(&mut self, rd: Reg, rn: Reg, shift: u8) {
        // LSL Wd, Wn, #shift is an alias for UBFM Wd, Wn, #(-shift mod 32), #(31-shift)
        // For 32-bit: UBFM with sf=0, N=0
        // immr = -shift mod 32 = (32 - shift) mod 32
        // imms = 31 - shift
        let shift = shift as u32;
        let immr = (32 - shift) & 0x1F;
        let imms = 31 - shift;
        let inst = OPCODE_UBFM_W
            | (immr << 16)
            | (imms << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_lsr32_imm(&mut self, rd: Reg, rn: Reg, imm: u8) {
        // LSR Wd, Wn, #imm (32-bit logical shift right by immediate)
        // Encoded as UBFM Wd, Wn, #imm, #31
        let imms = 31u32;
        let inst = OPCODE_UBFM_W
            | ((imm as u32 & 0x1F) << 16)
            | (imms << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }

    fn emit_asr32_imm(&mut self, rd: Reg, rn: Reg, imm: u8) {
        // ASR Wd, Wn, #imm (32-bit arithmetic shift right by immediate)
        // Encoded as SBFM Wd, Wn, #imm, #31
        let imms = 31u32;
        let inst = OPCODE_SBFM_W
            | ((imm as u32 & 0x1F) << 16)
            | (imms << 10)
            | (rn.encoding() as u32) << 5
            | rd.encoding() as u32;
        self.emit_u32(inst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper struct that owns the MIR to avoid lifetime issues
    struct TestEmitter {
        code: Vec<u8>,
    }

    impl TestEmitter {
        fn new() -> Self {
            TestEmitter { code: Vec::new() }
        }

        fn emit_u32(&mut self, inst: u32) {
            self.code.extend_from_slice(&inst.to_le_bytes());
        }

        /// Emit a mov immediate using the same logic as Emitter
        fn emit_mov_imm(&mut self, rd: Reg, imm: i64) {
            let uimm = imm as u64;

            let chunks = [
                (uimm >> 0) & 0xFFFF,
                (uimm >> 16) & 0xFFFF,
                (uimm >> 32) & 0xFFFF,
                (uimm >> 48) & 0xFFFF,
            ];

            let zeros = chunks.iter().filter(|&&c| c == 0).count();
            let ones = chunks.iter().filter(|&&c| c == 0xFFFF).count();

            if ones > zeros {
                let inverted = !uimm;
                let inv_chunks = [
                    (inverted >> 0) & 0xFFFF,
                    (inverted >> 16) & 0xFFFF,
                    (inverted >> 32) & 0xFFFF,
                    (inverted >> 48) & 0xFFFF,
                ];

                let (first_idx, first_val) = inv_chunks
                    .iter()
                    .enumerate()
                    .find(|&(_, &v)| v != 0)
                    .map(|(i, &v)| (i, v))
                    .unwrap_or((0, 0));

                let hw = first_idx as u32;
                let inst =
                    OPCODE_MOVN_X | (hw << 21) | ((first_val as u32) << 5) | rd.encoding() as u32;
                self.emit_u32(inst);

                for (i, &chunk) in chunks.iter().enumerate() {
                    if i != first_idx && chunk != 0xFFFF {
                        let base = match i {
                            0 => OPCODE_MOVK_X_LSL0,
                            1 => OPCODE_MOVK_X_LSL16,
                            2 => OPCODE_MOVK_X_LSL32,
                            3 => OPCODE_MOVK_X_LSL48,
                            _ => unreachable!(),
                        };
                        let inst = base | ((chunk as u32) << 5) | rd.encoding() as u32;
                        self.emit_u32(inst);
                    }
                }
            } else {
                let inst = OPCODE_MOVZ_X | ((uimm & 0xFFFF) << 5) as u32 | rd.encoding() as u32;
                self.emit_u32(inst);

                if (uimm >> 16) & 0xFFFF != 0 {
                    let inst = OPCODE_MOVK_X_LSL16
                        | (((uimm >> 16) & 0xFFFF) << 5) as u32
                        | rd.encoding() as u32;
                    self.emit_u32(inst);
                }
                if (uimm >> 32) & 0xFFFF != 0 {
                    let inst = OPCODE_MOVK_X_LSL32
                        | (((uimm >> 32) & 0xFFFF) << 5) as u32
                        | rd.encoding() as u32;
                    self.emit_u32(inst);
                }
                if (uimm >> 48) & 0xFFFF != 0 {
                    let inst = OPCODE_MOVK_X_LSL48
                        | (((uimm >> 48) & 0xFFFF) << 5) as u32
                        | rd.encoding() as u32;
                    self.emit_u32(inst);
                }
            }
        }
    }

    #[test]
    fn test_movn_for_minus_one() {
        // -1 should use MOVN for efficient encoding
        let mut emitter = TestEmitter::new();
        emitter.emit_mov_imm(Reg::X0, -1);

        // -1 (0xFFFFFFFFFFFFFFFF) should be encoded as MOVN X0, #0
        // MOVN: 0x92800000, with rd=0
        // Since all chunks are 0xFFFF, we use MOVN with the first inverted chunk (0)
        assert_eq!(emitter.code.len(), 4, "MOVN -1 should be 1 instruction");

        let inst = u32::from_le_bytes(emitter.code[0..4].try_into().unwrap());
        // Check it's a MOVN instruction (top bits 0x92800000)
        assert_eq!(inst & 0xFF800000, 0x92800000, "Should be MOVN");
        // Check destination is X0
        assert_eq!(inst & 0x1F, 0, "Destination should be X0");
    }

    #[test]
    fn test_movz_for_small_positive() {
        // Small positive numbers should use MOVZ
        let mut emitter = TestEmitter::new();
        emitter.emit_mov_imm(Reg::X1, 42);

        assert_eq!(
            emitter.code.len(),
            4,
            "Small immediate should be 1 instruction"
        );

        let inst = u32::from_le_bytes(emitter.code[0..4].try_into().unwrap());
        // MOVZ: 0xD2800000
        assert_eq!(inst & 0xFF800000, 0xD2800000, "Should be MOVZ");
        // Check destination is X1
        assert_eq!(inst & 0x1F, 1, "Destination should be X1");
        // Check immediate (42 << 5)
        assert_eq!((inst >> 5) & 0xFFFF, 42, "Immediate should be 42");
    }

    #[test]
    fn test_movn_for_negative_with_few_non_ff_chunks() {
        // -256 = 0xFFFFFFFFFFFFFF00
        // This has 3 chunks as 0xFFFF and one as 0xFF00
        // MOVN should be more efficient
        let mut emitter = TestEmitter::new();
        emitter.emit_mov_imm(Reg::X2, -256);

        // Should use MOVN followed by MOVK for the 0xFF00 chunk
        // The inverted value is 0x00000000000000FF
        // First chunk inverted is 0xFF, so MOVN X2, #0xFF
        let inst = u32::from_le_bytes(emitter.code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF800000, 0x92800000, "Should be MOVN");
    }

    #[test]
    fn test_movz_movk_for_large_positive() {
        // 0x1234_5678 requires MOVZ + MOVK
        let mut emitter = TestEmitter::new();
        emitter.emit_mov_imm(Reg::X3, 0x12345678);

        // Should be MOVZ for low 16 bits + MOVK for high 16 bits
        assert_eq!(
            emitter.code.len(),
            8,
            "Large positive should be 2 instructions"
        );

        let inst1 = u32::from_le_bytes(emitter.code[0..4].try_into().unwrap());
        let inst2 = u32::from_le_bytes(emitter.code[4..8].try_into().unwrap());

        // First instruction: MOVZ X3, #0x5678
        // MOVZ uses top bits 0xD28 (sf=1, opc=10, hw=00)
        assert_eq!(inst1 & 0xFFE00000, 0xD2800000, "First should be MOVZ");
        assert_eq!((inst1 >> 5) & 0xFFFF, 0x5678, "Low 16 bits");
        assert_eq!(inst1 & 0x1F, 3, "Destination should be X3");

        // Second instruction: MOVK X3, #0x1234, LSL #16
        // MOVK LSL#16 uses top bits 0xF2A (sf=1, opc=11, hw=01)
        assert_eq!(
            inst2 & 0xFFE00000,
            0xF2A00000,
            "Second should be MOVK LSL#16"
        );
        assert_eq!((inst2 >> 5) & 0xFFFF, 0x1234, "High 16 bits");
        assert_eq!(inst2 & 0x1F, 3, "Destination should be X3");
    }

    #[test]
    fn test_zero_immediate() {
        let mut emitter = TestEmitter::new();
        emitter.emit_mov_imm(Reg::X4, 0);

        assert_eq!(emitter.code.len(), 4, "Zero should be 1 instruction");

        let inst = u32::from_le_bytes(emitter.code[0..4].try_into().unwrap());
        // MOVZ X4, #0
        assert_eq!(inst & 0xFF800000, 0xD2800000, "Should be MOVZ");
        assert_eq!((inst >> 5) & 0xFFFF, 0, "Immediate should be 0");
    }

    #[test]
    fn test_opcode_constants() {
        // Verify opcode constants are correct
        assert_eq!(OPCODE_MOVZ_X, 0xD2800000);
        assert_eq!(OPCODE_MOVN_X, 0x92800000);
        assert_eq!(OPCODE_B, 0x14000000);
        assert_eq!(OPCODE_RET, 0xD65F03C0);
        assert_eq!(OPCODE_ADD_X, 0x8B000000);
        assert_eq!(OPCODE_SUB_X, 0xCB000000);
    }

    // =========================================================================
    // Comprehensive instruction encoding tests
    // These tests verify correct encoding against ARM Reference Manual
    // =========================================================================

    use crate::LabelId;
    use crate::aarch64::mir::Operand;

    /// Helper to emit a single instruction and return the encoded bytes
    fn emit_single(inst: Aarch64Inst) -> Vec<u8> {
        let mut mir = Aarch64Mir::new();
        mir.push(inst);
        Emitter::new(&mir, 0, 0, &[], &[]).emit().unwrap().0
    }

    // --- Move instructions ---

    #[test]
    fn test_mov_rr_x0_x1() {
        let code = emit_single(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
        });
        // mov x0, x1 -> orr x0, xzr, x1
        // 0xAA0103E0: sf=1, opc=01, shift=00, N=0, Rm=1, imm6=0, Rn=31(xzr), Rd=0
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFFE0FFE0, 0xAA0003E0, "Should be MOV (ORR) pattern");
        assert_eq!(inst & 0x1F, 0, "Rd should be X0");
        assert_eq!((inst >> 16) & 0x1F, 1, "Rm should be X1");
    }

    #[test]
    fn test_mov_imm_small() {
        let code = emit_single(Aarch64Inst::MovImm {
            dst: Operand::Physical(Reg::X0),
            imm: 42,
        });
        // mov x0, #42 -> movz x0, #42
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF800000, 0xD2800000, "Should be MOVZ");
        assert_eq!(inst & 0x1F, 0, "Rd should be X0");
        assert_eq!((inst >> 5) & 0xFFFF, 42, "Immediate should be 42");
    }

    // --- Arithmetic instructions ---

    #[test]
    fn test_add_rr() {
        let code = emit_single(Aarch64Inst::AddRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // add x0, x1, x2 -> 0x8B020020
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(
            inst & 0xFF200000,
            0x8B000000,
            "Should be ADD (shifted register)"
        );
        assert_eq!(inst & 0x1F, 0, "Rd should be X0");
        assert_eq!((inst >> 5) & 0x1F, 1, "Rn should be X1");
        assert_eq!((inst >> 16) & 0x1F, 2, "Rm should be X2");
    }

    #[test]
    fn test_adds_rr_32bit() {
        let code = emit_single(Aarch64Inst::AddsRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // adds w0, w1, w2 (32-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0x2B000000, "Should be ADDS 32-bit");
        assert_eq!(inst & 0x1F, 0, "Rd should be X0");
    }

    #[test]
    fn test_adds_rr_64bit() {
        let code = emit_single(Aarch64Inst::AddsRR64 {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // adds x0, x1, x2 (64-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0xAB000000, "Should be ADDS 64-bit");
    }

    #[test]
    fn test_add_imm() {
        let code = emit_single(Aarch64Inst::AddImm {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
            imm: 16,
        });
        // add x0, x1, #16
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF000000, 0x91000000, "Should be ADD immediate");
        assert_eq!(inst & 0x1F, 0, "Rd should be X0");
        assert_eq!((inst >> 5) & 0x1F, 1, "Rn should be X1");
        assert_eq!((inst >> 10) & 0xFFF, 16, "Immediate should be 16");
    }

    #[test]
    fn test_sub_rr() {
        let code = emit_single(Aarch64Inst::SubRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // sub x0, x1, x2 -> 0xCB020020
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(
            inst & 0xFF200000,
            0xCB000000,
            "Should be SUB (shifted register)"
        );
    }

    #[test]
    fn test_subs_rr_32bit() {
        let code = emit_single(Aarch64Inst::SubsRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // subs w0, w1, w2 (32-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0x6B000000, "Should be SUBS 32-bit");
    }

    #[test]
    fn test_subs_rr_64bit() {
        let code = emit_single(Aarch64Inst::SubsRR64 {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // subs x0, x1, x2 (64-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0xEB000000, "Should be SUBS 64-bit");
    }

    #[test]
    fn test_mul_rr() {
        let code = emit_single(Aarch64Inst::MulRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // mul x0, x1, x2 (alias for madd x0, x1, x2, xzr)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // MUL: 0x9B007C00 (MADD with Ra=XZR)
        assert_eq!(inst & 0xFFE0FC00, 0x9B007C00, "Should be MUL pattern");
    }

    #[test]
    fn test_smull_rr() {
        let code = emit_single(Aarch64Inst::SmullRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // smull x0, w1, w2
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFFE0FC00, 0x9B207C00, "Should be SMULL pattern");
    }

    #[test]
    fn test_sdiv_rr() {
        let code = emit_single(Aarch64Inst::SdivRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // sdiv w0, w1, w2 (32-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(
            inst & 0xFFE0FC00,
            0x1AC00C00,
            "Should be SDIV 32-bit pattern"
        );
    }

    #[test]
    fn test_msub() {
        let code = emit_single(Aarch64Inst::Msub {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
            src3: Operand::Physical(Reg::X3),
        });
        // msub w0, w1, w2, w3 (32-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(
            inst & 0xFFE08000,
            0x1B008000,
            "Should be MSUB 32-bit pattern"
        );
    }

    #[test]
    fn test_neg() {
        let code = emit_single(Aarch64Inst::Neg {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
        });
        // neg x0, x1 -> sub x0, xzr, x1
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0xCB000000, "Should be SUB pattern");
        // Rn (bits 5-9) should be XZR (31)
        assert_eq!((inst >> 5) & 0x1F, 31, "Rn should be XZR");
    }

    // --- Logical instructions ---

    #[test]
    fn test_and_rr() {
        let code = emit_single(Aarch64Inst::AndRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // and x0, x1, x2
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0x8A000000, "Should be AND pattern");
    }

    #[test]
    fn test_orr_rr() {
        let code = emit_single(Aarch64Inst::OrrRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // orr x0, x1, x2
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0xAA000000, "Should be ORR pattern");
    }

    #[test]
    fn test_eor_rr() {
        let code = emit_single(Aarch64Inst::EorRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // eor x0, x1, x2
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0xCA000000, "Should be EOR pattern");
    }

    #[test]
    fn test_mvn_rr() {
        let code = emit_single(Aarch64Inst::MvnRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
        });
        // mvn x0, x1 -> orn x0, xzr, x1
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0xAA200000, "Should be ORN pattern");
        // Rn (bits 5-9) should be XZR (31)
        assert_eq!((inst >> 5) & 0x1F, 31, "Rn should be XZR");
    }

    // --- Shift instructions ---

    #[test]
    fn test_lsl_imm() {
        let code = emit_single(Aarch64Inst::LslImm {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
            imm: 4,
        });
        // lsl x0, x1, #4 (alias for ubfm)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // UBFM for LSL: sf=1, opc=10, N=1 -> 0xD3400000
        assert_eq!(inst & 0xFFC00000, 0xD3400000, "Should be UBFM 64-bit");
    }

    #[test]
    fn test_lsl32_imm() {
        let code = emit_single(Aarch64Inst::Lsl32Imm {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
            imm: 4,
        });
        // lsl w0, w1, #4 (32-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // UBFM 32-bit: sf=0
        assert_eq!(inst & 0xFF800000, 0x53000000, "Should be UBFM 32-bit");
    }

    #[test]
    fn test_lsr_rr() {
        let code = emit_single(Aarch64Inst::LsrRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // lsr x0, x1, x2 (lsrv 64-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFFE0FC00, 0x9AC02400, "Should be LSRV 64-bit");
    }

    #[test]
    fn test_asr_rr() {
        let code = emit_single(Aarch64Inst::AsrRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        // asr x0, x1, x2 (asrv 64-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFFE0FC00, 0x9AC02800, "Should be ASRV 64-bit");
    }

    // --- Comparison instructions ---

    #[test]
    fn test_cmp_rr() {
        let code = emit_single(Aarch64Inst::CmpRR {
            src1: Operand::Physical(Reg::X0),
            src2: Operand::Physical(Reg::X1),
        });
        // cmp w0, w1 -> subs wzr, w0, w1 (32-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0x6B000000, "Should be SUBS 32-bit");
        // Rd should be WZR (31)
        assert_eq!(inst & 0x1F, 31, "Rd should be WZR");
    }

    #[test]
    fn test_cmp64_rr() {
        let code = emit_single(Aarch64Inst::Cmp64RR {
            src1: Operand::Physical(Reg::X0),
            src2: Operand::Physical(Reg::X1),
        });
        // cmp x0, x1 -> subs xzr, x0, x1 (64-bit)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0xEB000000, "Should be SUBS 64-bit");
        // Rd should be XZR (31)
        assert_eq!(inst & 0x1F, 31, "Rd should be XZR");
    }

    #[test]
    fn test_cmp_imm() {
        let code = emit_single(Aarch64Inst::CmpImm {
            src: Operand::Physical(Reg::X0),
            imm: 0,
        });
        // cmp x0, #0 -> subs xzr, x0, #0
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF000000, 0xF1000000, "Should be SUBS immediate");
        // Rd should be XZR (31)
        assert_eq!(inst & 0x1F, 31, "Rd should be XZR");
    }

    #[test]
    fn test_cset() {
        let code = emit_single(Aarch64Inst::Cset {
            dst: Operand::Physical(Reg::X0),
            cond: Cond::Eq,
        });
        // cset x0, eq -> csinc x0, xzr, xzr, ne
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // CSINC: sf=1, op=0, S=0, opcode2=01 -> 1001 1010 1xxx xxxx xxxx xxxx xxxx xxxx
        // Mask out Rd (bits 0-4), Rn (bits 5-9), cond (bits 12-15), Rm (bits 16-20)
        // Fixed opcode bits: 1001 1010 100x xxxx 0000 01xx xxx0 0000
        assert_eq!(inst & 0xFFE00C00, 0x9A800400, "Should be CSINC/CSET opcode");
        assert_eq!(inst & 0x1F, 0, "Rd should be X0");
    }

    #[test]
    fn test_tst_rr() {
        let code = emit_single(Aarch64Inst::TstRR {
            src1: Operand::Physical(Reg::X0),
            src2: Operand::Physical(Reg::X1),
        });
        // tst x0, x1 -> ands xzr, x0, x1
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF200000, 0xEA000000, "Should be ANDS pattern");
        // Rd should be XZR (31)
        assert_eq!(inst & 0x1F, 31, "Rd should be XZR");
    }

    // --- Sign/Zero extension ---

    #[test]
    fn test_sxtb() {
        let code = emit_single(Aarch64Inst::Sxtb {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
        });
        // sxtb x0, x1 -> sbfm x0, x1, #0, #7
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // SBFM 64-bit: sf=1, opc=00, N=1 -> 0x93400000
        assert_eq!(inst & 0xFFC00000, 0x93400000, "Should be SBFM 64-bit");
        // imms should be 7
        assert_eq!((inst >> 10) & 0x3F, 7, "imms should be 7");
    }

    #[test]
    fn test_sxth() {
        let code = emit_single(Aarch64Inst::Sxth {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
        });
        // sxth x0, x1 -> sbfm x0, x1, #0, #15
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // SBFM 64-bit: sf=1, opc=00, N=1 -> 0x93400000
        assert_eq!(inst & 0xFFC00000, 0x93400000, "Should be SBFM 64-bit");
        // imms should be 15
        assert_eq!((inst >> 10) & 0x3F, 15, "imms should be 15");
    }

    #[test]
    fn test_sxtw() {
        let code = emit_single(Aarch64Inst::Sxtw {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
        });
        // sxtw x0, x1 -> sbfm x0, x1, #0, #31
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // SBFM 64-bit: sf=1, opc=00, N=1 -> 0x93400000
        assert_eq!(inst & 0xFFC00000, 0x93400000, "Should be SBFM 64-bit");
        // imms should be 31
        assert_eq!((inst >> 10) & 0x3F, 31, "imms should be 31");
    }

    #[test]
    fn test_uxtb() {
        let code = emit_single(Aarch64Inst::Uxtb {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
        });
        // uxtb x0, x1 -> ubfm x0, x1, #0, #7
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // UBFM 64-bit: sf=1, opc=10, N=1 -> 0xD3400000
        assert_eq!(inst & 0xFFC00000, 0xD3400000, "Should be UBFM 64-bit");
        // imms should be 7
        assert_eq!((inst >> 10) & 0x3F, 7, "imms should be 7");
    }

    #[test]
    fn test_uxth() {
        let code = emit_single(Aarch64Inst::Uxth {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
        });
        // uxth x0, x1 -> ubfm x0, x1, #0, #15
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // UBFM 64-bit: sf=1, opc=10, N=1 -> 0xD3400000
        assert_eq!(inst & 0xFFC00000, 0xD3400000, "Should be UBFM 64-bit");
        // imms should be 15
        assert_eq!((inst >> 10) & 0x3F, 15, "imms should be 15");
    }

    // --- Load/Store instructions ---

    #[test]
    fn test_ldr_scaled_offset() {
        let code = emit_single(Aarch64Inst::Ldr {
            dst: Operand::Physical(Reg::X0),
            base: Reg::Fp,
            offset: 16,
        });
        // ldr x0, [fp, #16] (scaled unsigned offset)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(
            inst & 0xFFC00000,
            0xF9400000,
            "Should be LDR unsigned offset"
        );
        // imm12 = offset/8 = 2, at bits 10-21
        assert_eq!((inst >> 10) & 0xFFF, 2, "imm12 should be 2");
    }

    #[test]
    fn test_str_scaled_offset() {
        let code = emit_single(Aarch64Inst::Str {
            src: Operand::Physical(Reg::X0),
            base: Reg::Fp,
            offset: 16,
        });
        // str x0, [fp, #16] (scaled unsigned offset)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(
            inst & 0xFFC00000,
            0xF9000000,
            "Should be STR unsigned offset"
        );
    }

    // --- Control flow ---

    #[test]
    fn test_ret() {
        let code = emit_single(Aarch64Inst::Ret);
        // ret -> 0xD65F03C0
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst, 0xD65F03C0, "Should be RET");
    }

    #[test]
    fn test_b_forward() {
        let mut mir = Aarch64Mir::new();
        mir.push(Aarch64Inst::B {
            label: LabelId::new(0),
        });
        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Physical(Reg::X0),
            imm: 42,
        }); // 4 bytes
        mir.push(Aarch64Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, &[], &[]).emit().unwrap();

        // b forward -> 0x14000002 (offset = 2 instructions = 8 bytes / 4)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFC000000, 0x14000000, "Should be B opcode");
        assert_eq!(inst & 0x03FFFFFF, 2, "Offset should be 2 instructions");
    }

    #[test]
    fn test_bcond_eq() {
        let mut mir = Aarch64Mir::new();
        mir.push(Aarch64Inst::BCond {
            cond: Cond::Eq,
            label: LabelId::new(0),
        });
        mir.push(Aarch64Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, &[], &[]).emit().unwrap();

        // b.eq -> 0x54000000 + condition (eq = 0)
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF00001F, 0x54000000, "Should be B.cond with EQ");
    }

    #[test]
    fn test_cbz() {
        let mut mir = Aarch64Mir::new();
        mir.push(Aarch64Inst::Cbz {
            src: Operand::Physical(Reg::X0),
            label: LabelId::new(0),
        });
        mir.push(Aarch64Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, &[], &[]).emit().unwrap();

        // cbz x0, label
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF00001F, 0xB4000000, "Should be CBZ");
        assert_eq!(inst & 0x1F, 0, "Rt should be X0");
    }

    #[test]
    fn test_cbnz() {
        let mut mir = Aarch64Mir::new();
        mir.push(Aarch64Inst::Cbnz {
            src: Operand::Physical(Reg::X0),
            label: LabelId::new(0),
        });
        mir.push(Aarch64Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, &[], &[]).emit().unwrap();

        // cbnz x0, label
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFF00001F, 0xB5000000, "Should be CBNZ");
    }

    #[test]
    fn test_bl() {
        use crate::RelocationKind;

        let mut mir = Aarch64Mir::new();
        mir.push(Aarch64Inst::Bl {
            symbol: "test_func".to_string(),
        });

        let (code, relocs) = Emitter::new(&mir, 0, 0, &[], &[]).emit().unwrap();

        // bl -> 0x94000000
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(inst & 0xFC000000, 0x94000000, "Should be BL opcode");

        // Should have a relocation
        assert_eq!(relocs.len(), 1, "Should have one relocation");
        assert_eq!(relocs[0].symbol, "test_func", "Symbol should match");
        assert_eq!(
            relocs[0].kind,
            RelocationKind::Aarch64Call26,
            "Should be Call26 relocation"
        );
        assert_eq!(relocs[0].addend, 0, "Addend should be 0");
    }

    // --- Stack operations ---

    #[test]
    fn test_stp_pre() {
        let code = emit_single(Aarch64Inst::StpPre {
            src1: Operand::Physical(Reg::Fp),
            src2: Operand::Physical(Reg::Lr),
            offset: -16,
        });
        // stp fp, lr, [sp, #-16]!
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // STP pre-index: 0xA9800000 base
        assert_eq!(inst & 0xFFC00000, 0xA9800000, "Should be STP pre-index");
        assert_eq!(inst & 0x1F, 29, "Rt1 should be FP (29)");
        assert_eq!((inst >> 10) & 0x1F, 30, "Rt2 should be LR (30)");
    }

    #[test]
    fn test_ldp_post() {
        let code = emit_single(Aarch64Inst::LdpPost {
            dst1: Operand::Physical(Reg::Fp),
            dst2: Operand::Physical(Reg::Lr),
            offset: 16,
        });
        // ldp fp, lr, [sp], #16
        let inst = u32::from_le_bytes(code[0..4].try_into().unwrap());
        // LDP post-index: 0xA8C00000 base
        assert_eq!(inst & 0xFFC00000, 0xA8C00000, "Should be LDP post-index");
    }

    // --- Condition code encoding ---

    #[test]
    fn test_condition_codes() {
        // Verify condition code encodings match ARM spec
        assert_eq!(Cond::Eq.encoding(), 0b0000);
        assert_eq!(Cond::Ne.encoding(), 0b0001);
        assert_eq!(Cond::Hs.encoding(), 0b0010);
        assert_eq!(Cond::Lo.encoding(), 0b0011);
        assert_eq!(Cond::Hi.encoding(), 0b1000);
        assert_eq!(Cond::Ls.encoding(), 0b1001);
        assert_eq!(Cond::Ge.encoding(), 0b1010);
        assert_eq!(Cond::Lt.encoding(), 0b1011);
        assert_eq!(Cond::Gt.encoding(), 0b1100);
        assert_eq!(Cond::Le.encoding(), 0b1101);
    }

    #[test]
    fn test_condition_invert() {
        assert_eq!(Cond::Eq.invert(), Cond::Ne);
        assert_eq!(Cond::Ne.invert(), Cond::Eq);
        assert_eq!(Cond::Lt.invert(), Cond::Ge);
        assert_eq!(Cond::Gt.invert(), Cond::Le);
        assert_eq!(Cond::Le.invert(), Cond::Gt);
        assert_eq!(Cond::Ge.invert(), Cond::Lt);
    }

    // --- Register encoding ---

    #[test]
    fn test_register_encoding() {
        assert_eq!(Reg::X0.encoding(), 0);
        assert_eq!(Reg::X1.encoding(), 1);
        assert_eq!(Reg::X19.encoding(), 19);
        assert_eq!(Reg::Fp.encoding(), 29);
        assert_eq!(Reg::Lr.encoding(), 30);
        assert_eq!(Reg::Sp.encoding(), 31);
        assert_eq!(Reg::Xzr.encoding(), 31);
    }
}
