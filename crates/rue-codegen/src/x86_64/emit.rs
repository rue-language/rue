//! X86-64 instruction encoding.
//!
//! This phase converts X86Mir instructions (with physical registers) to
//! machine code bytes.
//!
//! # Stack Frame Layout
//!
//! The x86-64 backend uses a standard RBP-based frame layout. After the prologue
//! executes, the stack looks like this (addresses grow downward):
//!
//! ```text
//! High addresses
//! ┌────────────────────────┐
//! │ arg N (if >6 args)     │ [rbp + 16 + (N-7)*8]  ← Stack arguments
//! │ ...                    │
//! │ arg 7 (first on stack) │ [rbp + 16]
//! ├────────────────────────┤
//! │ return address         │ [rbp + 8]   ← Pushed by CALL
//! ├────────────────────────┤
//! │ saved RBP              │ [rbp + 0]   ← Frame pointer points here
//! ├────────────────────────┤
//! │ callee-saved regs      │ [rbp - 8]   ← R12, R13, etc. (if used)
//! │ (pushed after RBP)     │ [rbp - 16]
//! │ ...                    │
//! ├────────────────────────┤
//! │ local 0                │ [rbp - callee_saved_size - 8]
//! │ local 1                │ [rbp - callee_saved_size - 16]
//! │ ...                    │
//! │ local N-1              │ [rbp - callee_saved_size - N*8]
//! ├────────────────────────┤
//! │ param 0 spill slot     │ [rbp - callee_saved_size - (num_locals+1)*8]
//! │ param 1 spill slot     │ [rbp - callee_saved_size - (num_locals+2)*8]
//! │ ...                    │
//! │ param 5 spill slot     │ [rbp - callee_saved_size - (num_locals+6)*8]
//! ├────────────────────────┤
//! │ (alignment padding)    │   ← Ensures RSP is 16-byte aligned
//! └────────────────────────┘
//! Low addresses (RSP points here)
//! ```
//!
//! ## Prologue Sequence
//!
//! The function prologue sets up the frame:
//!
//! ```asm
//! push rbp                 ; Save caller's frame pointer
//! mov rbp, rsp             ; Establish our frame pointer
//! push r12                 ; Save callee-saved registers (if used)
//! push r13
//! ...
//! sub rsp, N               ; Allocate space for locals + params (16-aligned)
//! mov [rbp-X], rdi         ; Spill register params to stack (first 6 args)
//! mov [rbp-X], rsi
//! ...
//! ```
//!
//! ## Epilogue Sequence
//!
//! The function epilogue restores the caller's state:
//!
//! ```asm
//! lea rsp, [rbp - callee_saved_size]  ; Deallocate locals, preserve callee-saved
//! pop r13                              ; Restore callee-saved in reverse order
//! pop r12
//! pop rbp                              ; Restore caller's frame pointer
//! ret                                  ; Return to caller
//! ```
//!
//! ## Key Invariants
//!
//! 1. **RBP-relative stack arguments**: Stack arguments (7th argument and beyond)
//!    are accessed at fixed positive offsets from RBP (`[rbp + 16]`, `[rbp + 24]`, etc.).
//!    These offsets are stable regardless of callee-saved register usage.
//!
//! 2. **Offset adjustment for locals**: CfgLower generates local variable offsets
//!    assuming `[rbp - 8]` is slot 0. The emit phase adjusts all negative RBP-relative
//!    offsets by `callee_saved_size` to account for registers pushed after RBP setup.
//!    See the `MovRM` and `MovMR` handlers in [`Emitter::emit_inst`].
//!
//! 3. **16-byte stack alignment**: RSP is aligned to 16 bytes after the prologue.
//!    The alignment calculation accounts for the number of callee-saved pushes.
//!
//! 4. **Callee-saved registers**: R12-R15 and RBX are callee-saved per System V AMD64 ABI.
//!    Register allocation determines which are actually used and need saving.
//!
//! 5. **Two-phase stack allocation**: The prologue first pushes callee-saved registers
//!    (variable number), then allocates remaining space via `sub rsp, N`. This allows
//!    calculating the exact alignment padding needed.
//!
//! ## Calling Convention (System V AMD64 ABI)
//!
//! - Arguments 1-6: RDI, RSI, RDX, RCX, R8, R9
//! - Arguments 7+: Pushed right-to-left onto stack (arg7 at [rbp+16])
//! - Return value: RAX (second value in RDX for 128-bit returns)
//! - Caller-saved: RAX, RCX, RDX, RSI, RDI, R8-R11, XMM0-XMM15
//! - Callee-saved: RBX, RBP, R12-R15

use std::collections::HashMap;

use rue_error::{CompileError, CompileResult, ErrorKind};

use super::mir::{LabelId, Reg, X86Inst, X86Mir};
use crate::{EmittedCode, EmittedInst, EmittedRelocation};

/// Format an offset for assembly output (e.g., -8 -> "-8", 16 -> "+16", 0 -> "").
fn format_offset(offset: i32) -> String {
    if offset == 0 {
        String::new()
    } else if offset > 0 {
        format!("+{}", offset)
    } else {
        format!("{}", offset)
    }
}

/// Kind of jump fixup (rel8 or rel32).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixupKind {
    /// 1-byte relative offset (-128 to +127)
    Rel8,
    /// 4-byte relative offset
    Rel32,
}

/// A pending fixup for a forward jump.
struct Fixup {
    /// Offset of the rel8/rel32 displacement in the code.
    offset: usize,
    /// Target label ID.
    label: LabelId,
    /// Kind of fixup (rel8 or rel32).
    kind: FixupKind,
}

/// X86-64 instruction emitter.
///
/// The emitter converts MIR instructions to machine code, producing both
/// raw bytes and human-readable assembly text for each instruction.
pub struct Emitter<'a> {
    mir: &'a X86Mir,
    /// Raw machine code bytes being emitted.
    code: Vec<u8>,
    /// Emitted instructions with asm text (for --emit asm).
    /// Each entry corresponds to a logical instruction with its byte range.
    instructions: Vec<EmittedInst>,
    relocations: Vec<EmittedRelocation>,
    /// Maps label IDs to their byte offsets.
    labels: HashMap<LabelId, usize>,
    /// Forward jumps that need to be patched.
    fixups: Vec<Fixup>,
    /// Total number of local slots including spills (for stack frame size).
    num_locals: u32,
    /// Original number of local variables (for param offset calculation).
    /// CfgLower generates param offsets based on this value.
    num_locals_original: u32,
    /// Number of function parameters.
    num_params: u32,
    /// Callee-saved registers that need to be preserved.
    callee_saved: Vec<Reg>,
    /// String table (indexed by string_id in StringConstPtr/StringConstLen).
    strings: &'a [String],
    /// Byte offset where the current instruction started (for recording).
    inst_start: usize,
}

impl<'a> Emitter<'a> {
    /// Create a new emitter.
    ///
    /// - `num_locals`: Total local slots including spills (for stack frame size)
    /// - `num_locals_original`: Original local count (for param offset calculation)
    /// - `num_params`: Number of function parameters
    /// - `strings`: String table for string constant references
    pub fn new(
        mir: &'a X86Mir,
        num_locals: u32,
        num_locals_original: u32,
        num_params: u32,
        callee_saved: &[Reg],
        strings: &'a [String],
    ) -> Self {
        Self {
            mir,
            code: Vec::new(),
            instructions: Vec::new(),
            relocations: Vec::new(),
            labels: HashMap::new(),
            fixups: Vec::new(),
            num_locals,
            num_locals_original,
            num_params,
            callee_saved: callee_saved.to_vec(),
            strings,
            inst_start: 0,
        }
    }

    // ==================== Instruction recording helpers ====================

    /// Begin recording a new instruction. Call this before emitting bytes.
    fn begin_inst(&mut self) {
        self.inst_start = self.code.len();
    }

    /// End recording an instruction. Captures bytes emitted since begin_inst().
    fn end_inst(&mut self, asm: impl Into<String>) {
        let bytes = self.code[self.inst_start..].to_vec();
        self.instructions.push(EmittedInst::new(bytes, asm));
    }

    /// Record a label (no bytes, just marks a position in the asm output).
    fn record_label(&mut self, name: impl Into<String>) {
        self.instructions.push(EmittedInst::label(name));
    }

    /// Record a comment (no bytes).
    fn record_comment(&mut self, text: impl Into<String>) {
        self.instructions.push(EmittedInst::comment(text));
    }

    // ==================== Main emit entry point ====================

    /// Emit machine code for all instructions.
    ///
    /// Returns (code bytes, relocations).
    pub fn emit(mut self) -> CompileResult<(Vec<u8>, Vec<EmittedRelocation>)> {
        self.emit_internal()?;
        Ok((self.code, self.relocations))
    }

    /// Emit machine code for all instructions, returning full EmittedCode.
    ///
    /// This is the preferred method when you need both bytes and assembly text.
    pub fn emit_all(mut self) -> CompileResult<EmittedCode> {
        self.emit_internal()?;
        Ok(EmittedCode {
            instructions: self.instructions,
            relocations: self.relocations,
        })
    }

    /// Internal implementation of emit.
    fn emit_internal(&mut self) -> CompileResult<()> {
        // Verify no MovRMIndexed or MovMRIndexed survived into emission
        // These should have been lowered by regalloc into MovRM/MovMR
        for (i, inst) in self.mir.iter().enumerate() {
            if matches!(
                inst,
                X86Inst::MovRMIndexed { .. } | X86Inst::MovMRIndexed { .. }
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

        // Emit function prologue if we have local variables, parameters, or callee-saved regs
        if self.num_locals > 0 || self.num_params > 0 || !self.callee_saved.is_empty() {
            self.emit_prologue();
        }

        for inst in self.mir.iter() {
            self.emit_inst(inst);
        }
        self.apply_fixups()
    }

    /// Emit function prologue to set up the stack frame.
    ///
    /// This sets up RBP-based stack frame, then saves callee-saved registers AFTER
    /// setting up RBP. This ensures that stack arguments (beyond the first 6) can
    /// be accessed at fixed offsets from RBP:
    /// - [rbp+0]  = saved rbp
    /// - [rbp+8]  = return address
    /// - [rbp+16] = arg7 (first stack argument, if present)
    /// - [rbp+24] = arg8, etc.
    ///
    /// ```asm
    /// push rbp
    /// mov rbp, rsp
    /// push r12             ; save callee-saved registers AFTER rbp setup
    /// push r13
    /// ...
    /// sub rsp, N           ; N = (num_locals + num_params) * 8, aligned to 16
    /// mov [rbp-X], rdi     ; save param 0
    /// mov [rbp-X], rsi     ; save param 1
    /// ...
    /// ```
    ///
    /// The corresponding epilogue (generated by lower.rs, augmented by emitter) is:
    /// ```asm
    /// mov rsp, rbp         ; deallocate locals and callee-saved in one step
    /// pop rbp              ; restore rbp
    /// ret
    /// ```
    fn emit_prologue(&mut self) {
        self.record_comment("prologue");

        // push rbp: 55
        self.begin_inst();
        self.code.push(0x55);
        self.end_inst("push rbp");

        // mov rbp, rsp: 48 89 E5
        self.begin_inst();
        self.code.push(0x48);
        self.code.push(0x89);
        self.code.push(0xE5);
        self.end_inst("mov rbp, rsp");

        // Save callee-saved registers AFTER rbp setup
        // This ensures stack args are at fixed offsets from rbp
        let callee_saved = self.callee_saved.clone();
        for &reg in &callee_saved {
            self.begin_inst();
            self.emit_push(reg);
            self.end_inst(format!("push {}", reg));
        }

        // Calculate stack space needed:
        // - num_locals slots for local variables
        // - num_params slots for saved parameters (for the first 6 params only)
        // Each slot is 8 bytes, total aligned to 16
        let total_slots = self.num_locals + self.num_params.min(6);
        let needed_bytes = total_slots as i32 * 8;
        // Account for callee-saved pushes for alignment calculation
        let pushes_so_far = callee_saved.len() as i32;
        // We need total stack usage (including callee-saved) to be 16-byte aligned
        // At this point: rbp is set, callee_saved are pushed
        // After sub rsp, N: rsp should be 16-byte aligned
        let current_offset = pushes_so_far * 8;
        let total_needed = current_offset + needed_bytes;
        let stack_size = ((total_needed + 15) / 16) * 16 - current_offset;

        if stack_size > 0 {
            // sub rsp, imm32: 48 81 EC imm32
            self.begin_inst();
            self.code.push(0x48);
            self.code.push(0x81);
            self.code.push(0xEC);
            self.code.extend_from_slice(&stack_size.to_le_bytes());
            self.end_inst(format!("sub rsp, {}", stack_size));
        }

        // Save incoming parameters from registers to the stack.
        // cfg_lower generates offsets assuming [rbp-8] is the first slot, but
        // callee-saved registers are pushed after rbp, shifting everything down.
        // The emit phase adjusts all rbp-relative negative offsets to account
        // for this (see MovRM and MovMR handlers).
        //
        // System V AMD64 ABI: first 6 args in rdi, rsi, rdx, rcx, r8, r9
        const ARG_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];

        // Offset adjustment for callee-saved registers
        let callee_saved_size = callee_saved.len() as i32 * 8;

        for i in 0..self.num_params.min(6) {
            // Use num_locals_original (not num_locals which includes spills)
            // because CfgLower generates param offsets based on the original count
            let slot = self.num_locals_original + i;
            // Skip past callee-saved registers in the offset calculation
            let offset = -callee_saved_size - ((slot as i32 + 1) * 8);
            let reg = ARG_REGS[i as usize];

            // Emit: mov [rbp + offset], reg
            self.begin_inst();
            self.emit_mov_mr(Reg::Rbp, offset, reg);
            self.end_inst(format!("mov [rbp{}], {}", offset, reg));
        }
    }

    /// Apply all fixups for forward jumps.
    fn apply_fixups(&mut self) -> CompileResult<()> {
        for fixup in &self.fixups {
            let target_offset = self.labels.get(&fixup.label).ok_or_else(|| {
                CompileError::without_span(ErrorKind::InternalCodegenError(format!(
                    "undefined label: {}",
                    fixup.label
                )))
            })?;

            match fixup.kind {
                FixupKind::Rel8 => {
                    // Calculate relative offset from the end of the jump instruction
                    // The fixup offset points to the rel8, which is the last byte of the instruction
                    let jump_end = fixup.offset + 1; // rel8 is 1 byte
                    let relative = *target_offset as i64 - jump_end as i64;

                    // rel8 encoding only supports -128 to +127 byte offsets
                    if relative < -128 || relative > 127 {
                        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
                            format!(
                                "jump offset {} exceeds rel8 range (-128..127) for label '{}'; \
                                 consider implementing rel32 fallback",
                                relative, fixup.label
                            ),
                        )));
                    }

                    self.code[fixup.offset] = relative as u8;
                }
                FixupKind::Rel32 => {
                    // Calculate relative offset from the end of the jump instruction
                    // The fixup offset points to the first byte of the 4-byte rel32
                    let jump_end = fixup.offset + 4; // rel32 is 4 bytes
                    let relative = *target_offset as i64 - jump_end as i64;

                    // rel32 encoding supports i32 range
                    if relative < i32::MIN as i64 || relative > i32::MAX as i64 {
                        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
                            format!(
                                "jump offset {} exceeds rel32 range for label '{}'",
                                relative, fixup.label
                            ),
                        )));
                    }

                    let bytes = (relative as i32).to_le_bytes();
                    self.code[fixup.offset..fixup.offset + 4].copy_from_slice(&bytes);
                }
            }
        }
        Ok(())
    }

    /// Emit a single instruction.
    fn emit_inst(&mut self, inst: &X86Inst) {
        match inst {
            X86Inst::MovRI32 { dst, imm } => {
                self.begin_inst();
                self.emit_mov_ri32(dst.as_physical(), *imm);
                self.end_inst(format!("mov {}, {}", dst.as_physical(), *imm));
            }
            X86Inst::MovRI64 { dst, imm } => {
                self.begin_inst();
                self.emit_mov_ri64(dst.as_physical(), *imm);
                self.end_inst(format!("mov {}, {}", dst.as_physical(), *imm));
            }
            X86Inst::MovRR { dst, src } => {
                // Detect epilogue pattern: mov rsp, rbp
                // With our prologue (push rbp; mov rbp,rsp; push callee_saved...),
                // callee-saved registers are BELOW rbp on the stack.
                // We need to restore them before moving rsp to rbp.
                if dst.as_physical() == Reg::Rsp
                    && src.as_physical() == Reg::Rbp
                    && !self.callee_saved.is_empty()
                {
                    self.record_comment("epilogue - restore callee-saved");
                    // Instead of mov rsp, rbp (which skips callee-saved),
                    // first point rsp at the callee-saved area, pop them, then pop rbp
                    let callee_saved_size = (self.callee_saved.len() * 8) as i32;
                    // lea rsp, [rbp - callee_saved_size]
                    self.begin_inst();
                    self.emit_lea_rsp_rbp_offset(-callee_saved_size);
                    self.end_inst(format!("lea rsp, [rbp{}]", -callee_saved_size));
                    // Pop callee-saved in reverse order (last pushed = first popped)
                    let callee_saved: Vec<_> = self.callee_saved.iter().rev().copied().collect();
                    for reg in callee_saved {
                        self.begin_inst();
                        self.emit_pop(reg);
                        self.end_inst(format!("pop {}", reg));
                    }
                    // Now rsp points at saved rbp, ready for pop rbp
                } else {
                    self.begin_inst();
                    self.emit_mov_rr(dst.as_physical(), src.as_physical());
                    self.end_inst(format!("mov {}, {}", dst.as_physical(), src.as_physical()));
                }
            }
            X86Inst::MovRM { dst, base, offset } => {
                // Adjust offset for rbp-relative accesses to account for callee-saved registers.
                // Lower.rs generates offsets assuming [rbp-8] is the first slot, but callee-saved
                // registers are pushed after rbp, so we need to skip past them.
                let adjusted_offset = if *base == Reg::Rbp && *offset < 0 {
                    let callee_saved_size = self.callee_saved.len() as i32 * 8;
                    *offset - callee_saved_size
                } else {
                    *offset
                };
                self.begin_inst();
                self.emit_mov_rm(dst.as_physical(), *base, adjusted_offset);
                self.end_inst(format!(
                    "mov {}, [{}{}]",
                    dst.as_physical(),
                    base,
                    format_offset(adjusted_offset)
                ));
            }
            X86Inst::MovMR { base, offset, src } => {
                // Adjust offset for rbp-relative accesses (same as MovRM above).
                let adjusted_offset = if *base == Reg::Rbp && *offset < 0 {
                    let callee_saved_size = self.callee_saved.len() as i32 * 8;
                    *offset - callee_saved_size
                } else {
                    *offset
                };
                self.begin_inst();
                self.emit_mov_mr(*base, adjusted_offset, src.as_physical());
                self.end_inst(format!(
                    "mov [{}{}], {}",
                    base,
                    format_offset(adjusted_offset),
                    src.as_physical()
                ));
            }
            X86Inst::AddRR { dst, src } => {
                self.begin_inst();
                self.emit_add_rr(dst.as_physical(), src.as_physical());
                self.end_inst(format!("add {}, {}", dst.as_physical(), src.as_physical()));
            }
            X86Inst::AddRR64 { dst, src } => {
                self.begin_inst();
                self.emit_add_rr64(dst.as_physical(), src.as_physical());
                self.end_inst(format!("add {}, {}", dst.as_physical(), src.as_physical()));
            }
            X86Inst::AddRI { dst, imm } => {
                self.begin_inst();
                self.emit_add_ri(dst.as_physical(), *imm);
                self.end_inst(format!("add {}, {}", dst.as_physical(), imm));
            }
            X86Inst::SubRR { dst, src } => {
                self.begin_inst();
                self.emit_sub_rr(dst.as_physical(), src.as_physical());
                self.end_inst(format!("sub {}, {}", dst.as_physical(), src.as_physical()));
            }
            X86Inst::SubRR64 { dst, src } => {
                self.begin_inst();
                self.emit_sub_rr64(dst.as_physical(), src.as_physical());
                self.end_inst(format!("sub {}, {}", dst.as_physical(), src.as_physical()));
            }
            X86Inst::ImulRR { dst, src } => {
                self.begin_inst();
                self.emit_imul_rr(dst.as_physical(), src.as_physical());
                self.end_inst(format!("imul {}, {}", dst.as_physical(), src.as_physical()));
            }
            X86Inst::ImulRR64 { dst, src } => {
                self.begin_inst();
                self.emit_imul_rr64(dst.as_physical(), src.as_physical());
                self.end_inst(format!("imul {}, {}", dst.as_physical(), src.as_physical()));
            }
            X86Inst::Neg { dst } => {
                self.begin_inst();
                self.emit_neg(dst.as_physical());
                self.end_inst(format!("neg {}", dst.as_physical()));
            }
            X86Inst::Neg64 { dst } => {
                self.begin_inst();
                self.emit_neg64(dst.as_physical());
                self.end_inst(format!("neg {}", dst.as_physical()));
            }
            X86Inst::XorRI { dst, imm } => {
                self.begin_inst();
                self.emit_xor_ri(dst.as_physical(), *imm);
                self.end_inst(format!("xor {}, {}", dst.as_physical(), imm));
            }
            X86Inst::AndRR { dst, src } => {
                self.begin_inst();
                self.emit_and_rr(dst.as_physical(), src.as_physical());
                self.end_inst(format!("and {}, {}", dst.as_physical(), src.as_physical()));
            }
            X86Inst::OrRR { dst, src } => {
                self.begin_inst();
                self.emit_or_rr(dst.as_physical(), src.as_physical());
                self.end_inst(format!("or {}, {}", dst.as_physical(), src.as_physical()));
            }
            X86Inst::XorRR { dst, src } => {
                self.begin_inst();
                self.emit_xor_rr(dst.as_physical(), src.as_physical());
                self.end_inst(format!("xor {}, {}", dst.as_physical(), src.as_physical()));
            }
            X86Inst::NotR { dst } => {
                self.begin_inst();
                self.emit_not(dst.as_physical());
                self.end_inst(format!("not {}", dst.as_physical()));
            }
            X86Inst::ShlRCl { dst } => {
                self.begin_inst();
                self.emit_shl_cl(dst.as_physical());
                self.end_inst(format!("shl {}, cl", dst.as_physical()));
            }
            X86Inst::Shl32RCl { dst } => {
                self.begin_inst();
                self.emit_shl32_cl(dst.as_physical());
                self.end_inst(format!("shl {}, cl", dst.as_physical()));
            }
            X86Inst::ShlRI { dst, imm } => {
                self.begin_inst();
                self.emit_shl_imm(dst.as_physical(), *imm);
                self.end_inst(format!("shl {}, {}", dst.as_physical(), imm));
            }
            X86Inst::Shl32RI { dst, imm } => {
                self.begin_inst();
                self.emit_shl32_imm(dst.as_physical(), *imm);
                self.end_inst(format!("shl {}, {}", dst.as_physical(), imm));
            }
            X86Inst::ShrRCl { dst } => {
                self.begin_inst();
                self.emit_shr_cl(dst.as_physical());
                self.end_inst(format!("shr {}, cl", dst.as_physical()));
            }
            X86Inst::Shr32RCl { dst } => {
                self.begin_inst();
                self.emit_shr32_cl(dst.as_physical());
                self.end_inst(format!("shr {}, cl", dst.as_physical()));
            }
            X86Inst::ShrRI { dst, imm } => {
                self.begin_inst();
                self.emit_shr_imm(dst.as_physical(), *imm);
                self.end_inst(format!("shr {}, {}", dst.as_physical(), imm));
            }
            X86Inst::Shr32RI { dst, imm } => {
                self.begin_inst();
                self.emit_shr32_imm(dst.as_physical(), *imm);
                self.end_inst(format!("shr {}, {}", dst.as_physical(), imm));
            }
            X86Inst::SarRCl { dst } => {
                self.begin_inst();
                self.emit_sar_cl(dst.as_physical());
                self.end_inst(format!("sar {}, cl", dst.as_physical()));
            }
            X86Inst::Sar32RCl { dst } => {
                self.begin_inst();
                self.emit_sar32_cl(dst.as_physical());
                self.end_inst(format!("sar {}, cl", dst.as_physical()));
            }
            X86Inst::SarRI { dst, imm } => {
                self.begin_inst();
                self.emit_sar_imm(dst.as_physical(), *imm);
                self.end_inst(format!("sar {}, {}", dst.as_physical(), imm));
            }
            X86Inst::Sar32RI { dst, imm } => {
                self.begin_inst();
                self.emit_sar32_imm(dst.as_physical(), *imm);
                self.end_inst(format!("sar {}, {}", dst.as_physical(), imm));
            }
            X86Inst::Cdq => {
                self.begin_inst();
                self.emit_cdq();
                self.end_inst("cdq");
            }
            X86Inst::IdivR { src } => {
                self.begin_inst();
                self.emit_idiv(src.as_physical());
                self.end_inst(format!("idiv {}", src.as_physical()));
            }
            X86Inst::CmpRR { src1, src2 } => {
                self.begin_inst();
                self.emit_cmp_rr(src1.as_physical(), src2.as_physical());
                self.end_inst(format!(
                    "cmp {}, {}",
                    src1.as_physical(),
                    src2.as_physical()
                ));
            }
            X86Inst::Cmp64RR { src1, src2 } => {
                self.begin_inst();
                self.emit_cmp64_rr(src1.as_physical(), src2.as_physical());
                self.end_inst(format!(
                    "cmp {}, {}",
                    src1.as_physical(),
                    src2.as_physical()
                ));
            }
            X86Inst::CmpRI { src, imm } => {
                self.begin_inst();
                self.emit_cmp_ri(src.as_physical(), *imm);
                self.end_inst(format!("cmp {}, {}", src.as_physical(), imm));
            }
            X86Inst::Cmp64RI { src, imm } => {
                self.begin_inst();
                self.emit_cmp64_ri(src.as_physical(), *imm);
                self.end_inst(format!("cmp {}, {}", src.as_physical(), imm));
            }
            X86Inst::Sete { dst } => {
                self.begin_inst();
                self.emit_setcc(0x94, dst.as_physical());
                self.end_inst(format!("sete {}", dst.as_physical()));
            }
            X86Inst::Setne { dst } => {
                self.begin_inst();
                self.emit_setcc(0x95, dst.as_physical());
                self.end_inst(format!("setne {}", dst.as_physical()));
            }
            X86Inst::Setl { dst } => {
                self.begin_inst();
                self.emit_setcc(0x9C, dst.as_physical());
                self.end_inst(format!("setl {}", dst.as_physical()));
            }
            X86Inst::Setg { dst } => {
                self.begin_inst();
                self.emit_setcc(0x9F, dst.as_physical());
                self.end_inst(format!("setg {}", dst.as_physical()));
            }
            X86Inst::Setle { dst } => {
                self.begin_inst();
                self.emit_setcc(0x9E, dst.as_physical());
                self.end_inst(format!("setle {}", dst.as_physical()));
            }
            X86Inst::Setge { dst } => {
                self.begin_inst();
                self.emit_setcc(0x9D, dst.as_physical());
                self.end_inst(format!("setge {}", dst.as_physical()));
            }
            X86Inst::Setb { dst } => {
                self.begin_inst();
                self.emit_setcc(0x92, dst.as_physical());
                self.end_inst(format!("setb {}", dst.as_physical()));
            }
            X86Inst::Seta { dst } => {
                self.begin_inst();
                self.emit_setcc(0x97, dst.as_physical());
                self.end_inst(format!("seta {}", dst.as_physical()));
            }
            X86Inst::Setbe { dst } => {
                self.begin_inst();
                self.emit_setcc(0x96, dst.as_physical());
                self.end_inst(format!("setbe {}", dst.as_physical()));
            }
            X86Inst::Setae { dst } => {
                self.begin_inst();
                self.emit_setcc(0x93, dst.as_physical());
                self.end_inst(format!("setae {}", dst.as_physical()));
            }
            X86Inst::Movzx { dst, src } => {
                self.begin_inst();
                self.emit_movzx(dst.as_physical(), src.as_physical());
                self.end_inst(format!(
                    "movzx {}, {}",
                    dst.as_physical(),
                    src.as_physical()
                ));
            }
            X86Inst::Movsx8To64 { dst, src } => {
                self.begin_inst();
                self.emit_movsx8_to64(dst.as_physical(), src.as_physical());
                self.end_inst(format!(
                    "movsx {}, {}",
                    dst.as_physical(),
                    src.as_physical()
                ));
            }
            X86Inst::Movsx16To64 { dst, src } => {
                self.begin_inst();
                self.emit_movsx16_to64(dst.as_physical(), src.as_physical());
                self.end_inst(format!(
                    "movsx {}, {}",
                    dst.as_physical(),
                    src.as_physical()
                ));
            }
            X86Inst::Movsx32To64 { dst, src } => {
                self.begin_inst();
                self.emit_movsxd(dst.as_physical(), src.as_physical());
                self.end_inst(format!(
                    "movsxd {}, {}",
                    dst.as_physical(),
                    src.as_physical()
                ));
            }
            X86Inst::Movzx8To64 { dst, src } => {
                self.begin_inst();
                self.emit_movzx8_to64(dst.as_physical(), src.as_physical());
                self.end_inst(format!(
                    "movzx {}, {}",
                    dst.as_physical(),
                    src.as_physical()
                ));
            }
            X86Inst::Movzx16To64 { dst, src } => {
                self.begin_inst();
                self.emit_movzx16_to64(dst.as_physical(), src.as_physical());
                self.end_inst(format!(
                    "movzx {}, {}",
                    dst.as_physical(),
                    src.as_physical()
                ));
            }
            X86Inst::TestRR { src1, src2 } => {
                self.begin_inst();
                self.emit_test_rr(src1.as_physical(), src2.as_physical());
                self.end_inst(format!(
                    "test {}, {}",
                    src1.as_physical(),
                    src2.as_physical()
                ));
            }
            X86Inst::Jz { label } => {
                self.begin_inst();
                self.emit_jcc(0x74, *label);
                self.end_inst(format!("jz {}", label));
            }
            X86Inst::Jnz { label } => {
                self.begin_inst();
                self.emit_jcc(0x75, *label);
                self.end_inst(format!("jnz {}", label));
            }
            X86Inst::Jo { label } => {
                self.begin_inst();
                self.emit_jcc(0x70, *label);
                self.end_inst(format!("jo {}", label));
            }
            X86Inst::Jno { label } => {
                self.begin_inst();
                self.emit_jcc(0x71, *label);
                self.end_inst(format!("jno {}", label));
            }
            X86Inst::Jb { label } => {
                self.begin_inst();
                self.emit_jcc(0x72, *label);
                self.end_inst(format!("jb {}", label));
            }
            X86Inst::Jae { label } => {
                self.begin_inst();
                self.emit_jcc(0x73, *label);
                self.end_inst(format!("jae {}", label));
            }
            X86Inst::Jbe { label } => {
                self.begin_inst();
                self.emit_jcc(0x76, *label);
                self.end_inst(format!("jbe {}", label));
            }
            X86Inst::Jge { label } => {
                self.begin_inst();
                self.emit_jcc(0x7D, *label);
                self.end_inst(format!("jge {}", label));
            }
            X86Inst::Jle { label } => {
                self.begin_inst();
                self.emit_jcc(0x7E, *label);
                self.end_inst(format!("jle {}", label));
            }
            X86Inst::Jmp { label } => {
                self.begin_inst();
                self.emit_jmp(*label);
                self.end_inst(format!("jmp {}", label));
            }
            X86Inst::Label { id } => {
                // Record the current code offset for this label
                self.labels.insert(*id, self.code.len());
                self.record_label(format!("{}", id));
            }
            X86Inst::CallRel { symbol } => {
                self.begin_inst();
                self.emit_call_rel(symbol);
                self.end_inst(format!("call {}", symbol));
            }
            X86Inst::Syscall => {
                self.begin_inst();
                self.emit_syscall();
                self.end_inst("syscall");
            }
            X86Inst::Ret => {
                // If we have callee-saved registers but no standard epilogue from lowerer,
                // we still need to emit the epilogue to restore them.
                // With our prologue order (push rbp; mov rbp,rsp; push callee_saved...; sub rsp,N),
                // we need to: lea rsp,[rbp-callee_size]; pop callee-saved; pop rbp; ret
                // We detect "no frame from MIR" by checking if we have callee_saved but
                // num_locals and num_params are both 0 (meaning lowerer didn't emit epilogue).
                if !self.callee_saved.is_empty() && self.num_locals == 0 && self.num_params == 0 {
                    self.record_comment("epilogue - restore callee-saved");
                    // Point RSP at the callee-saved area (skip any alignment padding)
                    let callee_saved_size = (self.callee_saved.len() * 8) as i32;
                    self.begin_inst();
                    self.emit_lea_rsp_rbp_offset(-callee_saved_size);
                    self.end_inst(format!("lea rsp, [rbp{}]", -callee_saved_size));
                    // Pop callee-saved in reverse order
                    let callee_saved: Vec<_> = self.callee_saved.iter().rev().copied().collect();
                    for reg in callee_saved {
                        self.begin_inst();
                        self.emit_pop(reg);
                        self.end_inst(format!("pop {}", reg));
                    }
                    // Then pop rbp
                    self.begin_inst();
                    self.emit_pop(Reg::Rbp);
                    self.end_inst("pop rbp");
                }
                self.begin_inst();
                self.emit_ret();
                self.end_inst("ret");
            }
            X86Inst::Pop { dst } => {
                // With our new prologue order (push rbp; mov rbp,rsp; push callee_saved...),
                // callee-saved registers are restored in the MovRR handler when it sees
                // the epilogue pattern (mov rsp, rbp). So we just emit a simple pop here.
                self.begin_inst();
                self.emit_pop(dst.as_physical());
                self.end_inst(format!("pop {}", dst.as_physical()));
            }
            X86Inst::Push { src } => {
                self.begin_inst();
                self.emit_push(src.as_physical());
                self.end_inst(format!("push {}", src.as_physical()));
            }
            X86Inst::Lea {
                dst,
                base,
                index: _,
                scale: _,
                disp,
            } => {
                // Adjust displacement for rbp-relative accesses to account for callee-saved registers.
                // Lower.rs generates offsets assuming [rbp-8] is the first slot, but callee-saved
                // registers are pushed after rbp, so we need to skip past them.
                let adjusted_disp = if *base == Reg::Rbp && *disp < 0 {
                    let callee_saved_size = self.callee_saved.len() as i32 * 8;
                    *disp - callee_saved_size
                } else {
                    *disp
                };
                // Simplified: LEA dst, [base + disp] without index
                self.begin_inst();
                self.emit_lea_simple(dst.as_physical(), *base, adjusted_disp);
                self.end_inst(format!(
                    "lea {}, [{}{}]",
                    dst.as_physical(),
                    base,
                    format_offset(adjusted_disp)
                ));
            }
            X86Inst::Shl { dst, count } => {
                let dst_reg = dst.as_physical();
                let cnt = count.as_physical();

                // If count isn't in RCX, move it.
                if cnt != Reg::Rcx {
                    // NB: this clobbers rcx, which is exactly what we want.
                    self.begin_inst();
                    self.emit_mov_rr(Reg::Rcx, cnt);
                    self.end_inst(format!("mov rcx, {}", cnt));
                }

                // If dst is RCX, that's a conflict (dst would be clobbered by the move above
                // or would shift itself by itself).
                assert!(
                    dst_reg != Reg::Rcx,
                    "Shl dst allocated to RCX, but x86 requires count in CL; \
         regalloc must avoid assigning dst=RCX for Shl"
                );

                self.begin_inst();
                self.emit_shl_cl(dst_reg);
                self.end_inst(format!("shl {}, cl", dst_reg));
            }

            X86Inst::MovRMIndexed { dst, base, offset } => {
                // MOV dst, [base_vreg + offset] - but base is VReg
                // After regalloc, base should be in a physical register
                // We emit MOV r64, [r64 + offset] where base has already been loaded
                // For now use a simple indirect load. The regalloc phase should ensure
                // base is already in a register.
                let _ = base;
                let _ = offset;
                // This is handled specially - after regalloc the base VReg is in Rax
                self.begin_inst();
                self.emit_mov_rm(dst.as_physical(), Reg::Rax, 0);
                self.end_inst(format!("mov {}, [rax]", dst.as_physical()));
            }
            X86Inst::MovMRIndexed { base, offset, src } => {
                // MOV [base_vreg + offset], src
                let _ = base;
                let _ = offset;
                // Same as above - base should be in Rax after regalloc
                self.begin_inst();
                self.emit_mov_mr(Reg::Rax, 0, src.as_physical());
                self.end_inst(format!("mov [rax], {}", src.as_physical()));
            }

            X86Inst::StringConstPtr { dst, string_id } => {
                // LEA dst, [rip + offset]  - Load address of string in .rodata
                // This emits a placeholder that will be fixed up by a relocation.
                // The relocation will point to .rodata.str{string_id}
                self.begin_inst();
                self.emit_string_const_ptr(dst.as_physical(), *string_id);
                self.end_inst(format!(
                    "lea {}, [rip + .rodata.str{}]",
                    dst.as_physical(),
                    string_id
                ));
            }

            X86Inst::StringConstLen { dst, string_id } => {
                // MOV dst, imm64 - Load string length as immediate
                let string_len = self
                    .strings
                    .get(*string_id as usize)
                    .map(|s| s.len() as i64)
                    .unwrap_or(0);
                self.begin_inst();
                self.emit_mov_ri64(dst.as_physical(), string_len);
                self.end_inst(format!("mov {}, {}", dst.as_physical(), string_len));
            }

            X86Inst::StringConstCap { dst, string_id: _ } => {
                // MOV dst, 0 - String literals have capacity 0 (rodata, not heap)
                // This distinguishes rodata strings from heap-allocated ones
                self.begin_inst();
                self.emit_mov_ri64(dst.as_physical(), 0);
                self.end_inst(format!("mov {}, 0", dst.as_physical()));
            }
        }
    }

    /// Emit LEA with simple [base + disp] addressing.
    fn emit_lea_simple(&mut self, dst: Reg, base: Reg, disp: i32) {
        let dst_enc = dst.encoding();
        let base_enc = base.encoding();

        let mut rex = 0x48;
        if dst.needs_rex() {
            rex |= 0x04;
        }
        if base.needs_rex() {
            rex |= 0x01;
        }
        self.code.push(rex);

        self.code.push(0x8D);

        // Use the same memory encoding logic as mov.
        self.emit_modrm_memory(dst_enc, base_enc, disp);
    }

    /// Emit SHL r64, CL.
    fn emit_shl_cl(&mut self, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX.W prefix
        let mut rex = 0x48;
        if dst.needs_rex() {
            rex |= 0x01; // REX.B
        }
        self.code.push(rex);

        // SHL r/m64, CL is D3 /4
        self.code.push(0xD3);
        // ModR/M: mod=11 (register), reg=4 (/4), r/m=dst
        self.code.push(0xE0 | (dst_enc & 7));
    }

    /// Emit SHL r32, CL (32-bit shift, masks by 31).
    fn emit_shl32_cl(&mut self, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX prefix only if needed for extended registers (no REX.W for 32-bit)
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // SHL r/m32, CL is D3 /4 (without REX.W)
        self.code.push(0xD3);
        // ModR/M: mod=11 (register), reg=4 (/4), r/m=dst
        self.code.push(0xE0 | (dst_enc & 7));
    }

    /// Emit SHR r32, CL (32-bit logical shift right, masks by 31).
    fn emit_shr32_cl(&mut self, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX prefix only if needed for extended registers (no REX.W for 32-bit)
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // SHR r/m32, CL is D3 /5 (without REX.W)
        self.code.push(0xD3);
        // ModR/M: mod=11 (register), reg=5 (/5), r/m=dst
        self.code.push(0xE8 | (dst_enc & 7));
    }

    /// Emit SAR r32, CL (32-bit arithmetic shift right, masks by 31).
    fn emit_sar32_cl(&mut self, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX prefix only if needed for extended registers (no REX.W for 32-bit)
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // SAR r/m32, CL is D3 /7 (without REX.W)
        self.code.push(0xD3);
        // ModR/M: mod=11 (register), reg=7 (/7), r/m=dst
        self.code.push(0xF8 | (dst_enc & 7));
    }

    /// Emit `mov r32, imm32`.
    ///
    /// Encoding: [REX] B8+rd imm32
    /// - REX.B is needed for r8d-r15d
    /// - B8+rd is the opcode (B8 for eax, B9 for ecx, etc.)
    fn emit_mov_ri32(&mut self, dst: Reg, imm: i32) {
        let enc = dst.encoding();

        // REX prefix if needed (for R8-R15)
        if dst.needs_rex() {
            // REX.B = 1 (0x41)
            self.code.push(0x41);
        }

        // Opcode: B8 + (reg & 7)
        self.code.push(0xB8 + (enc & 7));

        // Immediate (32-bit little-endian)
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// Emit `mov r64, imm64`.
    ///
    /// Encoding: REX.W B8+rd imm64
    /// - REX.W = 1 for 64-bit operand size
    /// - REX.B = 1 for r8-r15
    fn emit_mov_ri64(&mut self, dst: Reg, imm: i64) {
        let enc = dst.encoding();

        // REX prefix: W=1 (0x48), add B=1 (0x01) if needed
        let rex = 0x48 | if dst.needs_rex() { 0x01 } else { 0x00 };
        self.code.push(rex);

        // Opcode: B8 + (reg & 7)
        self.code.push(0xB8 + (enc & 7));

        // Immediate (64-bit little-endian)
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// Emit `mov r64, r64`.
    ///
    /// Encoding: REX.W 89 /r (mov r/m64, r64)
    /// - REX.W = 1 for 64-bit operand size
    /// - REX.R = 1 if src is r8-r15
    /// - REX.B = 1 if dst is r8-r15
    /// - ModR/M byte: mod=11 (register), reg=src, r/m=dst
    fn emit_mov_rr(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix: W=1, R=src.needs_rex, B=dst.needs_rex
        let rex = 0x48
            | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
            | if dst.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Opcode: 89 (mov r/m64, r64)
        self.code.push(0x89);

        // ModR/M: mod=11 (register-to-register), reg=src, r/m=dst
        let modrm = 0xC0 | ((src_enc & 7) << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `mov r64, [base + offset]` - Load from memory.
    ///
    /// Encoding: REX.W 8B /r (mov r64, r/m64)
    fn emit_mov_rm(&mut self, dst: Reg, base: Reg, offset: i32) {
        let dst_enc = dst.encoding();
        let base_enc = base.encoding();

        // REX prefix: W=1 for 64-bit, R for dst, B for base
        let rex = 0x48
            | if dst.needs_rex() { 0x04 } else { 0x00 }  // REX.R
            | if base.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Opcode: 8B (mov r64, r/m64)
        self.code.push(0x8B);

        // ModR/M and optional SIB/displacement
        self.emit_modrm_memory(dst_enc, base_enc, offset);
    }

    /// Emit `mov [base + offset], r64` - Store to memory.
    ///
    /// Encoding: REX.W 89 /r (mov r/m64, r64)
    fn emit_mov_mr(&mut self, base: Reg, offset: i32, src: Reg) {
        let src_enc = src.encoding();
        let base_enc = base.encoding();

        // REX prefix: W=1 for 64-bit, R for src, B for base
        let rex = 0x48
            | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
            | if base.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Opcode: 89 (mov r/m64, r64)
        self.code.push(0x89);

        // ModR/M and optional SIB/displacement
        self.emit_modrm_memory(src_enc, base_enc, offset);
    }

    /// Emit ModR/M byte (and SIB/displacement if needed) for memory operand [base + offset].
    ///
    /// This handles the complex x86 addressing mode encoding.
    fn emit_modrm_memory(&mut self, reg: u8, base: u8, offset: i32) {
        // For RBP-based addressing (common for stack locals), we need:
        // - mod=01 for 8-bit displacement or mod=10 for 32-bit displacement
        // - r/m=101 (RBP) requires a displacement (no [rbp] form exists, only [rbp+disp])
        // - RSP (r/m=100) requires a SIB byte

        let base_bits = base & 7;

        if base_bits == 4 {
            // RSP/R12 - needs SIB byte
            if offset >= -128 && offset <= 127 {
                // mod=01 (8-bit displacement), r/m=100 (SIB follows)
                let modrm = 0x44 | ((reg & 7) << 3);
                self.code.push(modrm);
                // SIB: scale=00, index=100 (none), base=RSP
                self.code.push(0x24);
                // 8-bit displacement
                self.code.push(offset as u8);
            } else {
                // mod=10 (32-bit displacement), r/m=100 (SIB follows)
                let modrm = 0x84 | ((reg & 7) << 3);
                self.code.push(modrm);
                // SIB: scale=00, index=100 (none), base=RSP
                self.code.push(0x24);
                // 32-bit displacement
                self.code.extend_from_slice(&offset.to_le_bytes());
            }
        } else if base_bits == 5 && offset == 0 {
            // RBP/R13 with no displacement - must use [rbp+0] encoding
            // mod=01 (8-bit displacement), r/m=101 (RBP)
            let modrm = 0x45 | ((reg & 7) << 3);
            self.code.push(modrm);
            self.code.push(0x00); // 8-bit displacement of 0
        } else if offset >= -128 && offset <= 127 {
            // 8-bit displacement
            // mod=01, r/m=base
            let modrm = 0x40 | ((reg & 7) << 3) | base_bits;
            self.code.push(modrm);
            self.code.push(offset as u8);
        } else {
            // 32-bit displacement
            // mod=10, r/m=base
            let modrm = 0x80 | ((reg & 7) << 3) | base_bits;
            self.code.push(modrm);
            self.code.extend_from_slice(&offset.to_le_bytes());
        }
    }

    /// Emit `syscall`.
    ///
    /// Encoding: 0F 05
    fn emit_syscall(&mut self) {
        self.code.push(0x0F);
        self.code.push(0x05);
    }

    /// Emit `ret`.
    ///
    /// Encoding: C3
    fn emit_ret(&mut self) {
        self.code.push(0xC3);
    }

    /// Emit `pop r64`.
    ///
    /// Encoding: [REX.B] 58+rd
    /// - REX.B is needed for r8-r15
    fn emit_pop(&mut self, dst: Reg) {
        let enc = dst.encoding();

        // REX prefix if needed (for R8-R15)
        if dst.needs_rex() {
            // REX.B = 1 (0x41)
            self.code.push(0x41);
        }

        // Opcode: 58 + (reg & 7)
        self.code.push(0x58 + (enc & 7));
    }

    /// Emit `push r64`.
    ///
    /// Encoding: [REX.B] 50+rd
    /// - REX.B is needed for r8-r15
    fn emit_push(&mut self, src: Reg) {
        let enc = src.encoding();

        // REX prefix if needed (for R8-R15)
        if src.needs_rex() {
            // REX.B = 1 (0x41)
            self.code.push(0x41);
        }

        // Opcode: 50 + (reg & 7)
        self.code.push(0x50 + (enc & 7));
    }

    /// Emit `lea rsp, [rbp + offset]` - Load effective address into RSP.
    ///
    /// This is used in the epilogue to restore RSP to point at callee-saved registers.
    /// Encoding: REX.W 8D /r (lea r64, m)
    fn emit_lea_rsp_rbp_offset(&mut self, offset: i32) {
        // REX.W prefix for 64-bit operand
        self.code.push(0x48);

        // Opcode: 8D (LEA)
        self.code.push(0x8D);

        // ModR/M: We need to encode [rbp + disp8/disp32]
        // mod=01 for disp8, mod=10 for disp32
        // reg=RSP (100 = 4)
        // r/m=RBP (101 = 5)
        if offset >= -128 && offset <= 127 {
            // mod=01 (disp8), reg=4 (rsp), r/m=5 (rbp)
            // ModR/M = 01 100 101 = 0x65
            self.code.push(0x65);
            self.code.push(offset as u8);
        } else {
            // mod=10 (disp32), reg=4 (rsp), r/m=5 (rbp)
            // ModR/M = 10 100 101 = 0xA5
            self.code.push(0xA5);
            self.code.extend_from_slice(&offset.to_le_bytes());
        }
    }

    /// Emit `call rel32` with a relocation.
    ///
    /// Encoding: E8 rel32
    /// The rel32 is a placeholder (0x00000000) that will be patched by the linker.
    fn emit_call_rel(&mut self, symbol: &str) {
        // Opcode: E8 (call rel32)
        self.code.push(0xE8);

        // The relocation offset points to the rel32 displacement
        let reloc_offset = self.code.len() as u64;

        // Placeholder for rel32 (will be filled by linker)
        self.code.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        // Record relocation using the helper
        self.relocations
            .push(EmittedRelocation::x86_call(reloc_offset, symbol));
    }

    /// Emit LEA dst, [rip + offset] for loading string constant address.
    ///
    /// Encoding: REX.W 8D /r with RIP-relative addressing (ModR/M = 05)
    /// The offset is a placeholder that will be patched by the linker.
    fn emit_string_const_ptr(&mut self, dst: Reg, string_id: u32) {
        let dst_enc = dst.encoding();

        // REX.W prefix (always needed for 64-bit)
        let rex = 0x48 | if dst.needs_rex() { 0x04 } else { 0x00 }; // REX.R
        self.code.push(rex);

        // Opcode: 8D (LEA)
        self.code.push(0x8D);

        // ModR/M: mod=00 (disp32 only), reg=dst, r/m=101 (RIP-relative)
        // For RIP-relative addressing: mod=00, r/m=101
        let modrm = ((dst_enc & 7) << 3) | 0x05;
        self.code.push(modrm);

        // The relocation offset points to the disp32
        let reloc_offset = self.code.len() as u64;

        // Placeholder for disp32 (will be filled by linker)
        self.code.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        // Record relocation to string symbol using the helper
        let symbol = format!(".rodata.str{}", string_id);
        self.relocations
            .push(EmittedRelocation::x86_pc32(reloc_offset, symbol));
    }

    /// Emit `add r32, r32`.
    ///
    /// Encoding: [REX] 01 /r (add r/m32, r32)
    /// We use 32-bit operand size for i32 values.
    fn emit_add_rr(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix if needed
        if src.needs_rex() || dst.needs_rex() {
            let rex = 0x40
                | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
                | if dst.needs_rex() { 0x01 } else { 0x00 }; // REX.B
            self.code.push(rex);
        }

        // Opcode: 01 (add r/m32, r32)
        self.code.push(0x01);

        // ModR/M: mod=11 (register-to-register), reg=src, r/m=dst
        let modrm = 0xC0 | ((src_enc & 7) << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `add r64, r64`.
    ///
    /// Encoding: REX.W 01 /r (add r/m64, r64)
    fn emit_add_rr64(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix: W=1 for 64-bit, R for src, B for dst
        let rex = 0x48
            | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
            | if dst.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Opcode: 01 (add r/m64, r64)
        self.code.push(0x01);

        // ModR/M: mod=11 (register-to-register), reg=src, r/m=dst
        let modrm = 0xC0 | ((src_enc & 7) << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `add r64, imm32` - Add 32-bit sign-extended immediate to 64-bit register.
    ///
    /// Encoding: REX.W 81 /0 imm32 (add r/m64, imm32)
    /// For small immediates (-128..127), uses REX.W 83 /0 imm8.
    fn emit_add_ri(&mut self, dst: Reg, imm: i32) {
        let dst_enc = dst.encoding();

        // REX prefix: W=1 for 64-bit operand, B if needed
        let rex = 0x48 | if dst.needs_rex() { 0x01 } else { 0x00 };
        self.code.push(rex);

        // For small immediates (-128..127), use 83 /0 imm8
        if imm >= -128 && imm <= 127 {
            // Opcode: 83 (group 1, /0 for ADD with imm8)
            self.code.push(0x83);

            // ModR/M: mod=11, reg=0 (ADD), r/m=dst
            let modrm = 0xC0 | (dst_enc & 7);
            self.code.push(modrm);

            // 8-bit immediate
            self.code.push(imm as u8);
        } else {
            // Opcode: 81 (group 1, /0 for ADD with imm32)
            self.code.push(0x81);

            // ModR/M: mod=11, reg=0 (ADD), r/m=dst
            let modrm = 0xC0 | (dst_enc & 7);
            self.code.push(modrm);

            // 32-bit immediate
            self.code.extend_from_slice(&imm.to_le_bytes());
        }
    }

    /// Emit `sub r32, r32`.
    ///
    /// Encoding: [REX] 29 /r (sub r/m32, r32)
    fn emit_sub_rr(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix if needed
        if src.needs_rex() || dst.needs_rex() {
            let rex = 0x40
                | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
                | if dst.needs_rex() { 0x01 } else { 0x00 }; // REX.B
            self.code.push(rex);
        }

        // Opcode: 29 (sub r/m32, r32)
        self.code.push(0x29);

        // ModR/M: mod=11 (register-to-register), reg=src, r/m=dst
        let modrm = 0xC0 | ((src_enc & 7) << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `sub r64, r64`.
    ///
    /// Encoding: REX.W 29 /r (sub r/m64, r64)
    fn emit_sub_rr64(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix: W=1 for 64-bit, R for src, B for dst
        let rex = 0x48
            | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
            | if dst.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Opcode: 29 (sub r/m64, r64)
        self.code.push(0x29);

        // ModR/M: mod=11 (register-to-register), reg=src, r/m=dst
        let modrm = 0xC0 | ((src_enc & 7) << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `imul r32, r32`.
    ///
    /// Encoding: [REX] 0F AF /r (imul r32, r/m32)
    fn emit_imul_rr(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix if needed
        if dst.needs_rex() || src.needs_rex() {
            let rex = 0x40
                | if dst.needs_rex() { 0x04 } else { 0x00 }  // REX.R (dst is reg field)
                | if src.needs_rex() { 0x01 } else { 0x00 }; // REX.B (src is r/m field)
            self.code.push(rex);
        }

        // Opcode: 0F AF (imul r32, r/m32)
        self.code.push(0x0F);
        self.code.push(0xAF);

        // ModR/M: mod=11, reg=dst, r/m=src
        let modrm = 0xC0 | ((dst_enc & 7) << 3) | (src_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `imul r64, r64`.
    ///
    /// Encoding: REX.W 0F AF /r (imul r64, r/m64)
    fn emit_imul_rr64(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix: W=1 for 64-bit, R for dst, B for src
        let rex = 0x48
            | if dst.needs_rex() { 0x04 } else { 0x00 }  // REX.R (dst is reg field)
            | if src.needs_rex() { 0x01 } else { 0x00 }; // REX.B (src is r/m field)
        self.code.push(rex);

        // Opcode: 0F AF (imul r64, r/m64)
        self.code.push(0x0F);
        self.code.push(0xAF);

        // ModR/M: mod=11, reg=dst, r/m=src
        let modrm = 0xC0 | ((dst_enc & 7) << 3) | (src_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `neg r32`.
    ///
    /// Encoding: [REX] F7 /3 (neg r/m32)
    fn emit_neg(&mut self, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX prefix if needed
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // Opcode: F7 (group 3 operations)
        self.code.push(0xF7);

        // ModR/M: mod=11, reg=3 (NEG), r/m=dst
        let modrm = 0xC0 | (3 << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `neg r64`.
    ///
    /// Encoding: REX.W F7 /3 (neg r/m64)
    fn emit_neg64(&mut self, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX prefix: W=1 for 64-bit, B if needed
        let rex = 0x48 | if dst.needs_rex() { 0x01 } else { 0x00 };
        self.code.push(rex);

        // Opcode: F7 (group 3 operations)
        self.code.push(0xF7);

        // ModR/M: mod=11, reg=3 (NEG), r/m=dst
        let modrm = 0xC0 | (3 << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `xor r32, imm32`.
    ///
    /// Encoding: [REX] 81 /6 imm32 (xor r/m32, imm32)
    /// For small immediates we could use 83 /6 imm8 but let's keep it simple.
    fn emit_xor_ri(&mut self, dst: Reg, imm: i32) {
        let dst_enc = dst.encoding();

        // REX prefix if needed
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // For small immediates (-128..127), use 83 /6 imm8
        if imm >= -128 && imm <= 127 {
            // Opcode: 83 (group 1, /6 for XOR with imm8)
            self.code.push(0x83);

            // ModR/M: mod=11, reg=6 (XOR), r/m=dst
            let modrm = 0xC0 | (6 << 3) | (dst_enc & 7);
            self.code.push(modrm);

            // 8-bit immediate
            self.code.push(imm as u8);
        } else {
            // Opcode: 81 (group 1, /6 for XOR with imm32)
            self.code.push(0x81);

            // ModR/M: mod=11, reg=6 (XOR), r/m=dst
            let modrm = 0xC0 | (6 << 3) | (dst_enc & 7);
            self.code.push(modrm);

            // 32-bit immediate
            self.code.extend_from_slice(&imm.to_le_bytes());
        }
    }

    /// Emit `and r32, r32`.
    ///
    /// Encoding: [REX] 21 /r (and r/m32, r32)
    fn emit_and_rr(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix if needed
        if src.needs_rex() || dst.needs_rex() {
            let rex = 0x40
                | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
                | if dst.needs_rex() { 0x01 } else { 0x00 }; // REX.B
            self.code.push(rex);
        }

        // Opcode: 21 (and r/m32, r32)
        self.code.push(0x21);

        // ModR/M: mod=11 (register-to-register), reg=src, r/m=dst
        let modrm = 0xC0 | ((src_enc & 7) << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `or r32, r32`.
    ///
    /// Encoding: [REX] 09 /r (or r/m32, r32)
    fn emit_or_rr(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix if needed
        if src.needs_rex() || dst.needs_rex() {
            let rex = 0x40
                | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
                | if dst.needs_rex() { 0x01 } else { 0x00 }; // REX.B
            self.code.push(rex);
        }

        // Opcode: 09 (or r/m32, r32)
        self.code.push(0x09);

        // ModR/M: mod=11 (register-to-register), reg=src, r/m=dst
        let modrm = 0xC0 | ((src_enc & 7) << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `xor r32, r32`.
    ///
    /// Encoding: [REX] 31 /r (xor r/m32, r32)
    fn emit_xor_rr(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix if needed
        if src.needs_rex() || dst.needs_rex() {
            let rex = 0x40
                | if src.needs_rex() { 0x04 } else { 0x00 }  // REX.R
                | if dst.needs_rex() { 0x01 } else { 0x00 }; // REX.B
            self.code.push(rex);
        }

        // Opcode: 31 (xor r/m32, r32)
        self.code.push(0x31);

        // ModR/M: mod=11 (register-to-register), reg=src, r/m=dst
        let modrm = 0xC0 | ((src_enc & 7) << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `not r32`.
    ///
    /// Encoding: [REX] F7 /2 (not r/m32)
    fn emit_not(&mut self, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX prefix if needed
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // Opcode: F7 (group 3 operations)
        self.code.push(0xF7);

        // ModR/M: mod=11, reg=2 (NOT), r/m=dst
        let modrm = 0xC0 | (2 << 3) | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `shl r64, imm8`.
    ///
    /// Encoding: REX.W C1 /4 imm8 (shl r/m64, imm8)
    fn emit_shl_imm(&mut self, dst: Reg, imm: u8) {
        let dst_enc = dst.encoding();

        // REX.W prefix
        let mut rex = 0x48;
        if dst.needs_rex() {
            rex |= 0x01; // REX.B
        }
        self.code.push(rex);

        // Opcode: C1 (group 2, shift by imm8)
        self.code.push(0xC1);

        // ModR/M: mod=11, reg=4 (SHL), r/m=dst
        self.code.push(0xE0 | (dst_enc & 7));

        // Immediate
        self.code.push(imm);
    }

    /// Emit `shl r32, imm8`.
    ///
    /// Encoding: C1 /4 imm8 (shl r/m32, imm8) - no REX.W for 32-bit
    fn emit_shl32_imm(&mut self, dst: Reg, imm: u8) {
        let dst_enc = dst.encoding();

        // REX prefix only if needed for extended registers (no REX.W for 32-bit)
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // Opcode: C1 (group 2, shift by imm8)
        self.code.push(0xC1);

        // ModR/M: mod=11, reg=4 (SHL), r/m=dst
        self.code.push(0xE0 | (dst_enc & 7));

        // Immediate
        self.code.push(imm);
    }

    /// Emit `shr r64, cl`.
    ///
    /// Encoding: REX.W D3 /5 (shr r/m64, CL)
    fn emit_shr_cl(&mut self, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX.W prefix
        let mut rex = 0x48;
        if dst.needs_rex() {
            rex |= 0x01; // REX.B
        }
        self.code.push(rex);

        // SHR r/m64, CL is D3 /5
        self.code.push(0xD3);

        // ModR/M: mod=11 (register), reg=5 (/5), r/m=dst
        self.code.push(0xE8 | (dst_enc & 7));
    }

    /// Emit `shr r64, imm8`.
    ///
    /// Encoding: REX.W C1 /5 imm8 (shr r/m64, imm8)
    fn emit_shr_imm(&mut self, dst: Reg, imm: u8) {
        let dst_enc = dst.encoding();

        // REX.W prefix
        let mut rex = 0x48;
        if dst.needs_rex() {
            rex |= 0x01; // REX.B
        }
        self.code.push(rex);

        // Opcode: C1 (group 2, shift by imm8)
        self.code.push(0xC1);

        // ModR/M: mod=11, reg=5 (SHR), r/m=dst
        self.code.push(0xE8 | (dst_enc & 7));

        // Immediate
        self.code.push(imm);
    }

    /// Emit `shr r32, imm8`.
    ///
    /// Encoding: C1 /5 imm8 (shr r/m32, imm8) - no REX.W for 32-bit
    fn emit_shr32_imm(&mut self, dst: Reg, imm: u8) {
        let dst_enc = dst.encoding();

        // REX prefix only if needed for extended registers (no REX.W for 32-bit)
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // Opcode: C1 (group 2, shift by imm8)
        self.code.push(0xC1);

        // ModR/M: mod=11, reg=5 (SHR), r/m=dst
        self.code.push(0xE8 | (dst_enc & 7));

        // Immediate
        self.code.push(imm);
    }

    /// Emit `sar r64, cl`.
    ///
    /// Encoding: REX.W D3 /7 (sar r/m64, CL)
    fn emit_sar_cl(&mut self, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX.W prefix
        let mut rex = 0x48;
        if dst.needs_rex() {
            rex |= 0x01; // REX.B
        }
        self.code.push(rex);

        // SAR r/m64, CL is D3 /7
        self.code.push(0xD3);

        // ModR/M: mod=11 (register), reg=7 (/7), r/m=dst
        self.code.push(0xF8 | (dst_enc & 7));
    }

    /// Emit `sar r64, imm8`.
    ///
    /// Encoding: REX.W C1 /7 imm8 (sar r/m64, imm8)
    fn emit_sar_imm(&mut self, dst: Reg, imm: u8) {
        let dst_enc = dst.encoding();

        // REX.W prefix
        let mut rex = 0x48;
        if dst.needs_rex() {
            rex |= 0x01; // REX.B
        }
        self.code.push(rex);

        // Opcode: C1 (group 2, shift by imm8)
        self.code.push(0xC1);

        // ModR/M: mod=11, reg=7 (SAR), r/m=dst
        self.code.push(0xF8 | (dst_enc & 7));

        // Immediate
        self.code.push(imm);
    }

    /// Emit `sar r32, imm8`.
    ///
    /// Encoding: C1 /7 imm8 (sar r/m32, imm8) - no REX.W for 32-bit
    fn emit_sar32_imm(&mut self, dst: Reg, imm: u8) {
        let dst_enc = dst.encoding();

        // REX prefix only if needed for extended registers (no REX.W for 32-bit)
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // Opcode: C1 (group 2, shift by imm8)
        self.code.push(0xC1);

        // ModR/M: mod=11, reg=7 (SAR), r/m=dst
        self.code.push(0xF8 | (dst_enc & 7));

        // Immediate
        self.code.push(imm);
    }

    /// Emit `cdq` - Sign-extend EAX to EDX:EAX.
    ///
    /// Encoding: 99
    fn emit_cdq(&mut self) {
        self.code.push(0x99);
    }

    /// Emit `idiv r32` - Signed divide EDX:EAX by r32.
    ///
    /// Encoding: [REX] F7 /7 (idiv r/m32)
    fn emit_idiv(&mut self, src: Reg) {
        let src_enc = src.encoding();

        // REX prefix if needed
        if src.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // Opcode: F7 (group 3 operations)
        self.code.push(0xF7);

        // ModR/M: mod=11, reg=7 (IDIV), r/m=src
        let modrm = 0xC0 | (7 << 3) | (src_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `test r32, r32`.
    ///
    /// Encoding: [REX] 85 /r (test r/m32, r32)
    fn emit_test_rr(&mut self, src1: Reg, src2: Reg) {
        let src1_enc = src1.encoding();
        let src2_enc = src2.encoding();

        // REX prefix if needed
        if src2.needs_rex() || src1.needs_rex() {
            let rex = 0x40
                | if src2.needs_rex() { 0x04 } else { 0x00 }  // REX.R
                | if src1.needs_rex() { 0x01 } else { 0x00 }; // REX.B
            self.code.push(rex);
        }

        // Opcode: 85 (test r/m32, r32)
        self.code.push(0x85);

        // ModR/M: mod=11, reg=src2, r/m=src1
        let modrm = 0xC0 | ((src2_enc & 7) << 3) | (src1_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `cmp r32, r32`.
    ///
    /// Encoding: [REX] 39 /r (cmp r/m32, r32)
    fn emit_cmp_rr(&mut self, src1: Reg, src2: Reg) {
        let src1_enc = src1.encoding();
        let src2_enc = src2.encoding();

        // REX prefix if needed
        if src2.needs_rex() || src1.needs_rex() {
            let rex = 0x40
                | if src2.needs_rex() { 0x04 } else { 0x00 }  // REX.R
                | if src1.needs_rex() { 0x01 } else { 0x00 }; // REX.B
            self.code.push(rex);
        }

        // Opcode: 39 (cmp r/m32, r32)
        self.code.push(0x39);

        // ModR/M: mod=11, reg=src2, r/m=src1
        let modrm = 0xC0 | ((src2_enc & 7) << 3) | (src1_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `cmp r64, r64`.
    ///
    /// Encoding: REX.W 39 /r (cmp r/m64, r64)
    fn emit_cmp64_rr(&mut self, src1: Reg, src2: Reg) {
        let src1_enc = src1.encoding();
        let src2_enc = src2.encoding();

        // REX.W prefix (always needed for 64-bit operands)
        let rex = 0x48
            | if src2.needs_rex() { 0x04 } else { 0x00 }  // REX.R
            | if src1.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Opcode: 39 (cmp r/m64, r64)
        self.code.push(0x39);

        // ModR/M: mod=11, reg=src2, r/m=src1
        let modrm = 0xC0 | ((src2_enc & 7) << 3) | (src1_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `cmp r32, imm32`.
    ///
    /// Encoding: [REX] 81 /7 imm32 (cmp r/m32, imm32)
    fn emit_cmp_ri(&mut self, src: Reg, imm: i32) {
        let src_enc = src.encoding();

        // REX prefix if needed
        if src.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // Opcode: 81 (group 1, /7 for CMP)
        self.code.push(0x81);

        // ModR/M: mod=11, reg=7 (CMP), r/m=src
        let modrm = 0xC0 | (7 << 3) | (src_enc & 7);
        self.code.push(modrm);

        // Immediate (32-bit little-endian)
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// Emit `cmp r64, imm32` (sign-extended).
    ///
    /// Encoding: REX.W 81 /7 imm32 (cmp r/m64, imm32)
    fn emit_cmp64_ri(&mut self, src: Reg, imm: i32) {
        let src_enc = src.encoding();

        // REX prefix: REX.W for 64-bit operand size, plus REX.B if extended register
        let mut rex = 0x48; // REX.W
        if src.needs_rex() {
            rex |= 0x01; // REX.B
        }
        self.code.push(rex);

        // Opcode: 81 (group 1, /7 for CMP)
        self.code.push(0x81);

        // ModR/M: mod=11, reg=7 (CMP), r/m=src
        let modrm = 0xC0 | (7 << 3) | (src_enc & 7);
        self.code.push(modrm);

        // Immediate (32-bit sign-extended to 64-bit by CPU)
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// Emit `setcc r8` - Set byte based on condition code.
    ///
    /// Encoding: [REX] 0F 9x /0 (setcc r/m8)
    /// The opcode byte (9x) varies by condition.
    fn emit_setcc(&mut self, opcode: u8, dst: Reg) {
        let dst_enc = dst.encoding();

        // REX prefix if needed for extended registers
        // Note: SETcc operates on 8-bit registers, but we use 64-bit names
        if dst.needs_rex() {
            self.code.push(0x41); // REX.B
        }

        // Two-byte opcode: 0F 9x
        self.code.push(0x0F);
        self.code.push(opcode);

        // ModR/M: mod=11, reg=0, r/m=dst
        let modrm = 0xC0 | (dst_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `movzx r32, r8` - Move with zero-extend (byte to dword).
    ///
    /// Encoding: [REX] 0F B6 /r (movzx r32, r/m8)
    fn emit_movzx(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX prefix if needed
        if dst.needs_rex() || src.needs_rex() {
            let rex = 0x40
                | if dst.needs_rex() { 0x04 } else { 0x00 }  // REX.R
                | if src.needs_rex() { 0x01 } else { 0x00 }; // REX.B
            self.code.push(rex);
        }

        // Two-byte opcode: 0F B6 (movzx r32, r/m8)
        self.code.push(0x0F);
        self.code.push(0xB6);

        // ModR/M: mod=11, reg=dst, r/m=src
        let modrm = 0xC0 | ((dst_enc & 7) << 3) | (src_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `movsx r64, r8` - Sign-extend 8-bit to 64-bit.
    ///
    /// Encoding: REX.W 0F BE /r (movsx r64, r/m8)
    fn emit_movsx8_to64(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX.W prefix (always needed for 64-bit destination)
        let rex = 0x48
            | if dst.needs_rex() { 0x04 } else { 0x00 } // REX.R
            | if src.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Two-byte opcode: 0F BE (movsx r64, r/m8)
        self.code.push(0x0F);
        self.code.push(0xBE);

        // ModR/M: mod=11, reg=dst, r/m=src
        let modrm = 0xC0 | ((dst_enc & 7) << 3) | (src_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `movsx r64, r16` - Sign-extend 16-bit to 64-bit.
    ///
    /// Encoding: REX.W 0F BF /r (movsx r64, r/m16)
    fn emit_movsx16_to64(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX.W prefix (always needed for 64-bit destination)
        let rex = 0x48
            | if dst.needs_rex() { 0x04 } else { 0x00 } // REX.R
            | if src.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Two-byte opcode: 0F BF (movsx r64, r/m16)
        self.code.push(0x0F);
        self.code.push(0xBF);

        // ModR/M: mod=11, reg=dst, r/m=src
        let modrm = 0xC0 | ((dst_enc & 7) << 3) | (src_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `movsxd r64, r32` - Sign-extend 32-bit to 64-bit.
    ///
    /// Encoding: REX.W 63 /r (movsxd r64, r/m32)
    fn emit_movsxd(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX.W prefix (always needed for 64-bit destination)
        let rex = 0x48
            | if dst.needs_rex() { 0x04 } else { 0x00 } // REX.R
            | if src.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Opcode: 63 (movsxd r64, r/m32)
        self.code.push(0x63);

        // ModR/M: mod=11, reg=dst, r/m=src
        let modrm = 0xC0 | ((dst_enc & 7) << 3) | (src_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `movzx r64, r8` - Zero-extend 8-bit to 64-bit.
    ///
    /// Encoding: REX.W 0F B6 /r (movzx r64, r/m8)
    fn emit_movzx8_to64(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX.W prefix (always needed for 64-bit destination)
        let rex = 0x48
            | if dst.needs_rex() { 0x04 } else { 0x00 } // REX.R
            | if src.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Two-byte opcode: 0F B6 (movzx r64, r/m8)
        self.code.push(0x0F);
        self.code.push(0xB6);

        // ModR/M: mod=11, reg=dst, r/m=src
        let modrm = 0xC0 | ((dst_enc & 7) << 3) | (src_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `movzx r64, r16` - Zero-extend 16-bit to 64-bit.
    ///
    /// Encoding: REX.W 0F B7 /r (movzx r64, r/m16)
    fn emit_movzx16_to64(&mut self, dst: Reg, src: Reg) {
        let dst_enc = dst.encoding();
        let src_enc = src.encoding();

        // REX.W prefix (always needed for 64-bit destination)
        let rex = 0x48
            | if dst.needs_rex() { 0x04 } else { 0x00 } // REX.R
            | if src.needs_rex() { 0x01 } else { 0x00 }; // REX.B
        self.code.push(rex);

        // Two-byte opcode: 0F B7 (movzx r64, r/m16)
        self.code.push(0x0F);
        self.code.push(0xB7);

        // ModR/M: mod=11, reg=dst, r/m=src
        let modrm = 0xC0 | ((dst_enc & 7) << 3) | (src_enc & 7);
        self.code.push(modrm);
    }

    /// Emit `jmp rel32` - Unconditional jump.
    ///
    /// Encoding: E9 rel32
    /// We always use rel32 to support jumps of any size.
    fn emit_jmp(&mut self, label: LabelId) {
        // Opcode: E9 (jmp rel32)
        self.code.push(0xE9);

        // Record fixup location and emit placeholder for rel32
        let fixup_offset = self.code.len();
        self.code.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // 4-byte placeholder

        self.fixups.push(Fixup {
            offset: fixup_offset,
            label,
            kind: FixupKind::Rel32,
        });
    }

    /// Emit a conditional jump with rel32 encoding.
    ///
    /// The opcode is the condition-specific byte (e.g., 0x74 for JZ, 0x75 for JNZ).
    /// We convert rel8 opcodes to rel32 opcodes (0F 8x form) to support jumps of any size.
    fn emit_jcc(&mut self, opcode: u8, label: LabelId) {
        // Convert rel8 opcode to rel32 opcode
        // rel8 opcodes are 7x (e.g., 74=JZ, 75=JNZ, 70=JO, 71=JNO)
        // rel32 opcodes are 0F 8x (e.g., 0F 84=JZ, 0F 85=JNZ, 0F 80=JO, 0F 81=JNO)
        // The pattern is: rel32_second_byte = rel8_opcode + 0x10
        let rel32_opcode = opcode + 0x10;

        // Emit two-byte opcode
        self.code.push(0x0F);
        self.code.push(rel32_opcode);

        // Record fixup location and emit placeholder for rel32
        let fixup_offset = self.code.len();
        self.code.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // 4-byte placeholder

        self.fixups.push(Fixup {
            offset: fixup_offset,
            label,
            kind: FixupKind::Rel32,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::mir::Operand;
    use super::*;
    use crate::LabelId;

    fn emit_single(inst: X86Inst) -> Vec<u8> {
        let mut mir = X86Mir::new();
        mir.push(inst);
        Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap().0
    }

    #[test]
    fn test_mov_eax_imm32() {
        let code = emit_single(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rax),
            imm: 42,
        });
        // mov eax, 42 -> B8 2A 00 00 00
        assert_eq!(code, vec![0xB8, 0x2A, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_mov_edi_imm32() {
        let code = emit_single(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rdi),
            imm: 60,
        });
        // mov edi, 60 -> BF 3C 00 00 00
        assert_eq!(code, vec![0xBF, 0x3C, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_mov_r10d_imm32() {
        let code = emit_single(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::R10),
            imm: 42,
        });
        // mov r10d, 42 -> 41 BA 2A 00 00 00
        assert_eq!(code, vec![0x41, 0xBA, 0x2A, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_mov_rax_imm64() {
        let code = emit_single(X86Inst::MovRI64 {
            dst: Operand::Physical(Reg::Rax),
            imm: 0x1_0000_0000,
        });
        // mov rax, 0x100000000 -> 48 B8 00 00 00 00 01 00 00 00
        assert_eq!(
            code,
            vec![0x48, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn test_mov_r10_imm64() {
        let code = emit_single(X86Inst::MovRI64 {
            dst: Operand::Physical(Reg::R10),
            imm: 0x1_0000_0000,
        });
        // mov r10, 0x100000000 -> 49 BA 00 00 00 00 01 00 00 00
        assert_eq!(
            code,
            vec![0x49, 0xBA, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn test_mov_rdi_rax() {
        let code = emit_single(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rdi),
            src: Operand::Physical(Reg::Rax),
        });
        // mov rdi, rax -> 48 89 C7
        assert_eq!(code, vec![0x48, 0x89, 0xC7]);
    }

    #[test]
    fn test_mov_rdi_r10() {
        let code = emit_single(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rdi),
            src: Operand::Physical(Reg::R10),
        });
        // mov rdi, r10 -> 4C 89 D7
        assert_eq!(code, vec![0x4C, 0x89, 0xD7]);
    }

    #[test]
    fn test_mov_r10_rdi() {
        let code = emit_single(X86Inst::MovRR {
            dst: Operand::Physical(Reg::R10),
            src: Operand::Physical(Reg::Rdi),
        });
        // mov r10, rdi -> 49 89 FA
        assert_eq!(code, vec![0x49, 0x89, 0xFA]);
    }

    #[test]
    fn test_syscall() {
        let code = emit_single(X86Inst::Syscall);
        assert_eq!(code, vec![0x0F, 0x05]);
    }

    #[test]
    fn test_ret() {
        let code = emit_single(X86Inst::Ret);
        assert_eq!(code, vec![0xC3]);
    }

    #[test]
    fn test_full_exit_sequence() {
        // mov r10d, 42
        // mov rdi, r10
        // mov eax, 60
        // syscall
        let mut mir = X86Mir::new();
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::R10),
            imm: 42,
        });
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rdi),
            src: Operand::Physical(Reg::R10),
        });
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rax),
            imm: 60,
        });
        mir.push(X86Inst::Syscall);

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // 41 BA 2A 00 00 00  mov r10d, 42
        // 4C 89 D7           mov rdi, r10
        // B8 3C 00 00 00     mov eax, 60
        // 0F 05              syscall
        assert_eq!(
            code,
            vec![
                0x41, 0xBA, 0x2A, 0x00, 0x00, 0x00, // mov r10d, 42
                0x4C, 0x89, 0xD7, // mov rdi, r10
                0xB8, 0x3C, 0x00, 0x00, 0x00, // mov eax, 60
                0x0F, 0x05 // syscall
            ]
        );
    }

    #[test]
    fn test_call_rel() {
        use crate::RelocationKind;

        let mut mir = X86Mir::new();
        mir.push(X86Inst::CallRel {
            symbol: "__rue_exit".into(),
        });

        let (code, relocs) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // call rel32 -> E8 00 00 00 00
        assert_eq!(code, vec![0xE8, 0x00, 0x00, 0x00, 0x00]);

        // Should have one relocation
        assert_eq!(relocs.len(), 1);
        assert_eq!(relocs[0].offset, 1); // After the opcode
        assert_eq!(relocs[0].symbol, "__rue_exit");
        assert_eq!(relocs[0].kind, RelocationKind::X86Plt32);
        assert_eq!(relocs[0].addend, -4);
    }

    // =========================================================================
    // Arithmetic instruction tests
    // =========================================================================

    #[test]
    fn test_add_eax_ecx() {
        let code = emit_single(X86Inst::AddRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // add eax, ecx -> 01 C8
        assert_eq!(code, vec![0x01, 0xC8]);
    }

    #[test]
    fn test_add_r10d_r11d() {
        let code = emit_single(X86Inst::AddRR {
            dst: Operand::Physical(Reg::R10),
            src: Operand::Physical(Reg::R11),
        });
        // add r10d, r11d -> 45 01 DA
        assert_eq!(code, vec![0x45, 0x01, 0xDA]);
    }

    #[test]
    fn test_sub_eax_ecx() {
        let code = emit_single(X86Inst::SubRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // sub eax, ecx -> 29 C8
        assert_eq!(code, vec![0x29, 0xC8]);
    }

    #[test]
    fn test_sub_rax_rcx_64() {
        let code = emit_single(X86Inst::SubRR64 {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // sub rax, rcx -> 48 29 C8
        assert_eq!(code, vec![0x48, 0x29, 0xC8]);
    }

    #[test]
    fn test_imul_eax_ecx() {
        let code = emit_single(X86Inst::ImulRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // imul eax, ecx -> 0F AF C1
        assert_eq!(code, vec![0x0F, 0xAF, 0xC1]);
    }

    #[test]
    fn test_neg_eax() {
        let code = emit_single(X86Inst::Neg {
            dst: Operand::Physical(Reg::Rax),
        });
        // neg eax -> F7 D8
        assert_eq!(code, vec![0xF7, 0xD8]);
    }

    #[test]
    fn test_neg_r10d() {
        let code = emit_single(X86Inst::Neg {
            dst: Operand::Physical(Reg::R10),
        });
        // neg r10d -> 41 F7 DA
        assert_eq!(code, vec![0x41, 0xF7, 0xDA]);
    }

    #[test]
    fn test_cdq() {
        let code = emit_single(X86Inst::Cdq);
        // cdq -> 99
        assert_eq!(code, vec![0x99]);
    }

    #[test]
    fn test_idiv_ecx() {
        let code = emit_single(X86Inst::IdivR {
            src: Operand::Physical(Reg::Rcx),
        });
        // idiv ecx -> F7 F9
        assert_eq!(code, vec![0xF7, 0xF9]);
    }

    #[test]
    fn test_idiv_r10d() {
        let code = emit_single(X86Inst::IdivR {
            src: Operand::Physical(Reg::R10),
        });
        // idiv r10d -> 41 F7 FA
        assert_eq!(code, vec![0x41, 0xF7, 0xFA]);
    }

    #[test]
    fn test_test_eax_eax() {
        let code = emit_single(X86Inst::TestRR {
            src1: Operand::Physical(Reg::Rax),
            src2: Operand::Physical(Reg::Rax),
        });
        // test eax, eax -> 85 C0
        assert_eq!(code, vec![0x85, 0xC0]);
    }

    // =========================================================================
    // Memory instruction tests (MovRM, MovMR)
    // =========================================================================

    #[test]
    fn test_mov_rax_rbp_minus_8() {
        // mov rax, [rbp-8] - Load from first local variable slot
        let code = emit_single(X86Inst::MovRM {
            dst: Operand::Physical(Reg::Rax),
            base: Reg::Rbp,
            offset: -8,
        });
        // mov rax, [rbp-8] -> 48 8B 45 F8
        // REX.W=1 (0x48), opcode 8B, ModRM: mod=01 r/m=101(rbp) reg=000(rax), disp8=-8
        assert_eq!(code, vec![0x48, 0x8B, 0x45, 0xF8]);
    }

    #[test]
    fn test_mov_rbp_minus_8_rax() {
        // mov [rbp-8], rax - Store to first local variable slot
        let code = emit_single(X86Inst::MovMR {
            base: Reg::Rbp,
            offset: -8,
            src: Operand::Physical(Reg::Rax),
        });
        // mov [rbp-8], rax -> 48 89 45 F8
        // REX.W=1 (0x48), opcode 89, ModRM: mod=01 r/m=101(rbp) reg=000(rax), disp8=-8
        assert_eq!(code, vec![0x48, 0x89, 0x45, 0xF8]);
    }

    #[test]
    fn test_mov_r10_rbp_minus_16() {
        // mov r10, [rbp-16] - Load from second local with extended register
        let code = emit_single(X86Inst::MovRM {
            dst: Operand::Physical(Reg::R10),
            base: Reg::Rbp,
            offset: -16,
        });
        // mov r10, [rbp-16] -> 4C 8B 55 F0
        // REX.W=1 REX.R=1 (0x4C), opcode 8B, ModRM: mod=01 r/m=101(rbp) reg=010(r10), disp8=-16
        assert_eq!(code, vec![0x4C, 0x8B, 0x55, 0xF0]);
    }

    #[test]
    fn test_mov_rbp_minus_16_r10() {
        // mov [rbp-16], r10 - Store to second local with extended register
        let code = emit_single(X86Inst::MovMR {
            base: Reg::Rbp,
            offset: -16,
            src: Operand::Physical(Reg::R10),
        });
        // mov [rbp-16], r10 -> 4C 89 55 F0
        // REX.W=1 REX.R=1 (0x4C), opcode 89, ModRM: mod=01 r/m=101(rbp) reg=010(r10), disp8=-16
        assert_eq!(code, vec![0x4C, 0x89, 0x55, 0xF0]);
    }

    #[test]
    fn test_mov_rm_large_offset() {
        // mov rax, [rbp-256] - Load with 32-bit displacement (offset too large for 8-bit)
        let code = emit_single(X86Inst::MovRM {
            dst: Operand::Physical(Reg::Rax),
            base: Reg::Rbp,
            offset: -256,
        });
        // mov rax, [rbp-256] -> 48 8B 85 00 FF FF FF
        // REX.W=1, opcode 8B, ModRM: mod=10 r/m=101(rbp) reg=000(rax), disp32=-256
        assert_eq!(code, vec![0x48, 0x8B, 0x85, 0x00, 0xFF, 0xFF, 0xFF]);
    }

    // =========================================================================
    // Pop instruction tests
    // =========================================================================

    #[test]
    fn test_pop_rbp() {
        let code = emit_single(X86Inst::Pop {
            dst: Operand::Physical(Reg::Rbp),
        });
        // pop rbp -> 5D
        assert_eq!(code, vec![0x5D]);
    }

    #[test]
    fn test_pop_rax() {
        let code = emit_single(X86Inst::Pop {
            dst: Operand::Physical(Reg::Rax),
        });
        // pop rax -> 58
        assert_eq!(code, vec![0x58]);
    }

    #[test]
    fn test_pop_r10() {
        let code = emit_single(X86Inst::Pop {
            dst: Operand::Physical(Reg::R10),
        });
        // pop r10 -> 41 5A
        assert_eq!(code, vec![0x41, 0x5A]);
    }

    // =========================================================================
    // Prologue tests
    // =========================================================================

    #[test]
    fn test_prologue_one_local() {
        // With 1 local, we need 8 bytes, aligned to 16 = 16 bytes
        let mir = X86Mir::new();
        let (code, _) = Emitter::new(&mir, 1, 1, 0, &[], &[]).emit().unwrap();

        // push rbp: 55
        // mov rbp, rsp: 48 89 E5
        // sub rsp, 16: 48 81 EC 10 00 00 00
        assert_eq!(
            code,
            vec![
                0x55, // push rbp
                0x48, 0x89, 0xE5, // mov rbp, rsp
                0x48, 0x81, 0xEC, 0x10, 0x00, 0x00, 0x00, // sub rsp, 16
            ]
        );
    }

    #[test]
    fn test_no_prologue_no_locals() {
        // With 0 locals, no prologue should be emitted
        let mir = X86Mir::new();
        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();
        assert!(code.is_empty());
    }

    #[test]
    fn test_shl_r14_cl() {
        let code = emit_single(X86Inst::Shl {
            dst: Operand::Physical(Reg::R14),
            count: Operand::Physical(Reg::Rcx),
        });
        // shl r14, cl -> 49 D3 E6  (REX.W|B because r/m=r14, opcode D3, modrm E0|6)
        assert_eq!(code, vec![0x49, 0xD3, 0xE6]);
    }

    // =========================================================================
    // Comprehensive instruction encoding tests
    // These tests verify correct encoding against Intel x86-64 reference
    // =========================================================================

    // --- 64-bit arithmetic ---

    #[test]
    fn test_add_rax_rcx_64() {
        let code = emit_single(X86Inst::AddRR64 {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // add rax, rcx -> 48 01 C8 (REX.W 01 /r)
        assert_eq!(code, vec![0x48, 0x01, 0xC8]);
    }

    #[test]
    fn test_add_r10_r11_64() {
        let code = emit_single(X86Inst::AddRR64 {
            dst: Operand::Physical(Reg::R10),
            src: Operand::Physical(Reg::R11),
        });
        // add r10, r11 -> 4D 01 DA (REX.WRB 01 /r)
        assert_eq!(code, vec![0x4D, 0x01, 0xDA]);
    }

    #[test]
    fn test_imul_rax_rcx_64() {
        let code = emit_single(X86Inst::ImulRR64 {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // imul rax, rcx -> 48 0F AF C1 (REX.W 0F AF /r)
        assert_eq!(code, vec![0x48, 0x0F, 0xAF, 0xC1]);
    }

    #[test]
    fn test_neg_rax_64() {
        let code = emit_single(X86Inst::Neg64 {
            dst: Operand::Physical(Reg::Rax),
        });
        // neg rax -> 48 F7 D8 (REX.W F7 /3)
        assert_eq!(code, vec![0x48, 0xF7, 0xD8]);
    }

    #[test]
    fn test_neg_r10_64() {
        let code = emit_single(X86Inst::Neg64 {
            dst: Operand::Physical(Reg::R10),
        });
        // neg r10 -> 49 F7 DA (REX.WB F7 /3)
        assert_eq!(code, vec![0x49, 0xF7, 0xDA]);
    }

    // --- Bitwise operations ---

    #[test]
    fn test_and_eax_ecx() {
        let code = emit_single(X86Inst::AndRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // and eax, ecx -> 21 C8 (21 /r)
        assert_eq!(code, vec![0x21, 0xC8]);
    }

    #[test]
    fn test_and_r10d_r11d() {
        let code = emit_single(X86Inst::AndRR {
            dst: Operand::Physical(Reg::R10),
            src: Operand::Physical(Reg::R11),
        });
        // and r10d, r11d -> 45 21 DA (REX.RB 21 /r)
        assert_eq!(code, vec![0x45, 0x21, 0xDA]);
    }

    #[test]
    fn test_or_eax_ecx() {
        let code = emit_single(X86Inst::OrRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // or eax, ecx -> 09 C8 (09 /r)
        assert_eq!(code, vec![0x09, 0xC8]);
    }

    #[test]
    fn test_xor_eax_ecx() {
        let code = emit_single(X86Inst::XorRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // xor eax, ecx -> 31 C8 (31 /r)
        assert_eq!(code, vec![0x31, 0xC8]);
    }

    #[test]
    fn test_xor_ri_small() {
        let code = emit_single(X86Inst::XorRI {
            dst: Operand::Physical(Reg::Rax),
            imm: 1,
        });
        // xor eax, 1 -> 83 F0 01 (83 /6 ib)
        assert_eq!(code, vec![0x83, 0xF0, 0x01]);
    }

    #[test]
    fn test_xor_ri_large() {
        let code = emit_single(X86Inst::XorRI {
            dst: Operand::Physical(Reg::Rax),
            imm: 256,
        });
        // xor eax, 256 -> 81 F0 00 01 00 00 (81 /6 id)
        assert_eq!(code, vec![0x81, 0xF0, 0x00, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn test_not_eax() {
        let code = emit_single(X86Inst::NotR {
            dst: Operand::Physical(Reg::Rax),
        });
        // not eax -> F7 D0 (F7 /2)
        assert_eq!(code, vec![0xF7, 0xD0]);
    }

    #[test]
    fn test_not_r10d() {
        let code = emit_single(X86Inst::NotR {
            dst: Operand::Physical(Reg::R10),
        });
        // not r10d -> 41 F7 D2 (REX.B F7 /2)
        assert_eq!(code, vec![0x41, 0xF7, 0xD2]);
    }

    // --- Shift instructions ---

    #[test]
    fn test_shl_rax_cl() {
        let code = emit_single(X86Inst::ShlRCl {
            dst: Operand::Physical(Reg::Rax),
        });
        // shl rax, cl -> 48 D3 E0 (REX.W D3 /4)
        assert_eq!(code, vec![0x48, 0xD3, 0xE0]);
    }

    #[test]
    fn test_shl32_eax_cl() {
        let code = emit_single(X86Inst::Shl32RCl {
            dst: Operand::Physical(Reg::Rax),
        });
        // shl eax, cl -> D3 E0 (D3 /4)
        assert_eq!(code, vec![0xD3, 0xE0]);
    }

    #[test]
    fn test_shl_rax_imm() {
        let code = emit_single(X86Inst::ShlRI {
            dst: Operand::Physical(Reg::Rax),
            imm: 4,
        });
        // shl rax, 4 -> 48 C1 E0 04 (REX.W C1 /4 ib)
        assert_eq!(code, vec![0x48, 0xC1, 0xE0, 0x04]);
    }

    #[test]
    fn test_shl32_eax_imm() {
        let code = emit_single(X86Inst::Shl32RI {
            dst: Operand::Physical(Reg::Rax),
            imm: 4,
        });
        // shl eax, 4 -> C1 E0 04 (C1 /4 ib)
        assert_eq!(code, vec![0xC1, 0xE0, 0x04]);
    }

    #[test]
    fn test_shr_rax_cl() {
        let code = emit_single(X86Inst::ShrRCl {
            dst: Operand::Physical(Reg::Rax),
        });
        // shr rax, cl -> 48 D3 E8 (REX.W D3 /5)
        assert_eq!(code, vec![0x48, 0xD3, 0xE8]);
    }

    #[test]
    fn test_shr32_eax_cl() {
        let code = emit_single(X86Inst::Shr32RCl {
            dst: Operand::Physical(Reg::Rax),
        });
        // shr eax, cl -> D3 E8 (D3 /5)
        assert_eq!(code, vec![0xD3, 0xE8]);
    }

    #[test]
    fn test_shr_rax_imm() {
        let code = emit_single(X86Inst::ShrRI {
            dst: Operand::Physical(Reg::Rax),
            imm: 4,
        });
        // shr rax, 4 -> 48 C1 E8 04 (REX.W C1 /5 ib)
        assert_eq!(code, vec![0x48, 0xC1, 0xE8, 0x04]);
    }

    #[test]
    fn test_shr32_eax_imm() {
        let code = emit_single(X86Inst::Shr32RI {
            dst: Operand::Physical(Reg::Rax),
            imm: 4,
        });
        // shr eax, 4 -> C1 E8 04 (C1 /5 ib)
        assert_eq!(code, vec![0xC1, 0xE8, 0x04]);
    }

    #[test]
    fn test_sar_rax_cl() {
        let code = emit_single(X86Inst::SarRCl {
            dst: Operand::Physical(Reg::Rax),
        });
        // sar rax, cl -> 48 D3 F8 (REX.W D3 /7)
        assert_eq!(code, vec![0x48, 0xD3, 0xF8]);
    }

    #[test]
    fn test_sar32_eax_cl() {
        let code = emit_single(X86Inst::Sar32RCl {
            dst: Operand::Physical(Reg::Rax),
        });
        // sar eax, cl -> D3 F8 (D3 /7)
        assert_eq!(code, vec![0xD3, 0xF8]);
    }

    #[test]
    fn test_sar_rax_imm() {
        let code = emit_single(X86Inst::SarRI {
            dst: Operand::Physical(Reg::Rax),
            imm: 4,
        });
        // sar rax, 4 -> 48 C1 F8 04 (REX.W C1 /7 ib)
        assert_eq!(code, vec![0x48, 0xC1, 0xF8, 0x04]);
    }

    #[test]
    fn test_sar32_eax_imm() {
        let code = emit_single(X86Inst::Sar32RI {
            dst: Operand::Physical(Reg::Rax),
            imm: 4,
        });
        // sar eax, 4 -> C1 F8 04 (C1 /7 ib)
        assert_eq!(code, vec![0xC1, 0xF8, 0x04]);
    }

    // --- Comparison instructions ---

    #[test]
    fn test_cmp_eax_ecx() {
        let code = emit_single(X86Inst::CmpRR {
            src1: Operand::Physical(Reg::Rax),
            src2: Operand::Physical(Reg::Rcx),
        });
        // cmp eax, ecx -> 39 C8 (39 /r)
        assert_eq!(code, vec![0x39, 0xC8]);
    }

    #[test]
    fn test_cmp_rax_rcx_64() {
        let code = emit_single(X86Inst::Cmp64RR {
            src1: Operand::Physical(Reg::Rax),
            src2: Operand::Physical(Reg::Rcx),
        });
        // cmp rax, rcx -> 48 39 C8 (REX.W 39 /r)
        assert_eq!(code, vec![0x48, 0x39, 0xC8]);
    }

    #[test]
    fn test_cmp_ri() {
        let code = emit_single(X86Inst::CmpRI {
            src: Operand::Physical(Reg::Rax),
            imm: 42,
        });
        // cmp eax, 42 -> 81 F8 2A 00 00 00 (81 /7 id)
        assert_eq!(code, vec![0x81, 0xF8, 0x2A, 0x00, 0x00, 0x00]);
    }

    // --- Set byte instructions ---

    #[test]
    fn test_sete() {
        let code = emit_single(X86Inst::Sete {
            dst: Operand::Physical(Reg::Rax),
        });
        // sete al -> 0F 94 C0 (0F 94 /0)
        assert_eq!(code, vec![0x0F, 0x94, 0xC0]);
    }

    #[test]
    fn test_setne() {
        let code = emit_single(X86Inst::Setne {
            dst: Operand::Physical(Reg::Rax),
        });
        // setne al -> 0F 95 C0 (0F 95 /0)
        assert_eq!(code, vec![0x0F, 0x95, 0xC0]);
    }

    #[test]
    fn test_setl() {
        let code = emit_single(X86Inst::Setl {
            dst: Operand::Physical(Reg::Rax),
        });
        // setl al -> 0F 9C C0 (0F 9C /0)
        assert_eq!(code, vec![0x0F, 0x9C, 0xC0]);
    }

    #[test]
    fn test_setg() {
        let code = emit_single(X86Inst::Setg {
            dst: Operand::Physical(Reg::Rax),
        });
        // setg al -> 0F 9F C0 (0F 9F /0)
        assert_eq!(code, vec![0x0F, 0x9F, 0xC0]);
    }

    #[test]
    fn test_setle() {
        let code = emit_single(X86Inst::Setle {
            dst: Operand::Physical(Reg::Rax),
        });
        // setle al -> 0F 9E C0 (0F 9E /0)
        assert_eq!(code, vec![0x0F, 0x9E, 0xC0]);
    }

    #[test]
    fn test_setge() {
        let code = emit_single(X86Inst::Setge {
            dst: Operand::Physical(Reg::Rax),
        });
        // setge al -> 0F 9D C0 (0F 9D /0)
        assert_eq!(code, vec![0x0F, 0x9D, 0xC0]);
    }

    #[test]
    fn test_setb() {
        let code = emit_single(X86Inst::Setb {
            dst: Operand::Physical(Reg::Rax),
        });
        // setb al -> 0F 92 C0 (0F 92 /0)
        assert_eq!(code, vec![0x0F, 0x92, 0xC0]);
    }

    #[test]
    fn test_seta() {
        let code = emit_single(X86Inst::Seta {
            dst: Operand::Physical(Reg::Rax),
        });
        // seta al -> 0F 97 C0 (0F 97 /0)
        assert_eq!(code, vec![0x0F, 0x97, 0xC0]);
    }

    #[test]
    fn test_setbe() {
        let code = emit_single(X86Inst::Setbe {
            dst: Operand::Physical(Reg::Rax),
        });
        // setbe al -> 0F 96 C0 (0F 96 /0)
        assert_eq!(code, vec![0x0F, 0x96, 0xC0]);
    }

    #[test]
    fn test_setae() {
        let code = emit_single(X86Inst::Setae {
            dst: Operand::Physical(Reg::Rax),
        });
        // setae al -> 0F 93 C0 (0F 93 /0)
        assert_eq!(code, vec![0x0F, 0x93, 0xC0]);
    }

    #[test]
    fn test_setcc_extended_reg() {
        let code = emit_single(X86Inst::Sete {
            dst: Operand::Physical(Reg::R10),
        });
        // sete r10b -> 41 0F 94 C2 (REX.B 0F 94 /0)
        assert_eq!(code, vec![0x41, 0x0F, 0x94, 0xC2]);
    }

    // --- Move with extension ---

    #[test]
    fn test_movzx_eax_cl() {
        let code = emit_single(X86Inst::Movzx {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // movzx eax, cl -> 0F B6 C1 (0F B6 /r)
        assert_eq!(code, vec![0x0F, 0xB6, 0xC1]);
    }

    #[test]
    fn test_movsx8_to64() {
        let code = emit_single(X86Inst::Movsx8To64 {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // movsx rax, cl -> 48 0F BE C1 (REX.W 0F BE /r)
        assert_eq!(code, vec![0x48, 0x0F, 0xBE, 0xC1]);
    }

    #[test]
    fn test_movsx16_to64() {
        let code = emit_single(X86Inst::Movsx16To64 {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // movsx rax, cx -> 48 0F BF C1 (REX.W 0F BF /r)
        assert_eq!(code, vec![0x48, 0x0F, 0xBF, 0xC1]);
    }

    #[test]
    fn test_movsx32_to64() {
        let code = emit_single(X86Inst::Movsx32To64 {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // movsxd rax, ecx -> 48 63 C1 (REX.W 63 /r)
        assert_eq!(code, vec![0x48, 0x63, 0xC1]);
    }

    #[test]
    fn test_movzx8_to64() {
        let code = emit_single(X86Inst::Movzx8To64 {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // movzx rax, cl -> 48 0F B6 C1 (REX.W 0F B6 /r)
        assert_eq!(code, vec![0x48, 0x0F, 0xB6, 0xC1]);
    }

    #[test]
    fn test_movzx16_to64() {
        let code = emit_single(X86Inst::Movzx16To64 {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rcx),
        });
        // movzx rax, cx -> 48 0F B7 C1 (REX.W 0F B7 /r)
        assert_eq!(code, vec![0x48, 0x0F, 0xB7, 0xC1]);
    }

    // --- Add with immediate ---

    #[test]
    fn test_add_ri_small() {
        let code = emit_single(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rax),
            imm: 8,
        });
        // add rax, 8 -> 48 83 C0 08 (REX.W 83 /0 ib)
        assert_eq!(code, vec![0x48, 0x83, 0xC0, 0x08]);
    }

    #[test]
    fn test_add_ri_large() {
        let code = emit_single(X86Inst::AddRI {
            dst: Operand::Physical(Reg::Rax),
            imm: 256,
        });
        // add rax, 256 -> 48 81 C0 00 01 00 00 (REX.W 81 /0 id)
        assert_eq!(code, vec![0x48, 0x81, 0xC0, 0x00, 0x01, 0x00, 0x00]);
    }

    // --- Push/Pop ---

    #[test]
    fn test_push_rax() {
        let code = emit_single(X86Inst::Push {
            src: Operand::Physical(Reg::Rax),
        });
        // push rax -> 50 (50+rd)
        assert_eq!(code, vec![0x50]);
    }

    #[test]
    fn test_push_r10() {
        let code = emit_single(X86Inst::Push {
            src: Operand::Physical(Reg::R10),
        });
        // push r10 -> 41 52 (REX.B 50+rd)
        assert_eq!(code, vec![0x41, 0x52]);
    }

    // --- Jump instructions ---

    #[test]
    fn test_jmp_forward() {
        let mut mir = X86Mir::new();
        mir.push(X86Inst::Jmp {
            label: LabelId::new(0),
        });
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rax),
            imm: 42,
        }); // 5 bytes
        mir.push(X86Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // jmp rel32 -> E9 xx xx xx xx (displacement = 5)
        assert_eq!(code[0], 0xE9);
        // Displacement: target at 10 (5+5), jmp ends at 5, so offset = 10 - 5 = 5
        assert_eq!(&code[1..5], &[0x05, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_jz_forward() {
        let mut mir = X86Mir::new();
        mir.push(X86Inst::Jz {
            label: LabelId::new(0),
        });
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rax),
            imm: 42,
        }); // 5 bytes
        mir.push(X86Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // jz rel32 -> 0F 84 xx xx xx xx (displacement = 5)
        assert_eq!(code[0], 0x0F);
        assert_eq!(code[1], 0x84);
        // Displacement: target at 11 (6+5), jz ends at 6, so offset = 11 - 6 = 5
        assert_eq!(&code[2..6], &[0x05, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_jnz_forward() {
        let mut mir = X86Mir::new();
        mir.push(X86Inst::Jnz {
            label: LabelId::new(0),
        });
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rax),
            imm: 42,
        }); // 5 bytes
        mir.push(X86Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // jnz rel32 -> 0F 85 xx xx xx xx (displacement = 5)
        assert_eq!(code[0], 0x0F);
        assert_eq!(code[1], 0x85);
    }

    #[test]
    fn test_jo_forward() {
        let mut mir = X86Mir::new();
        mir.push(X86Inst::Jo {
            label: LabelId::new(0),
        });
        mir.push(X86Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // jo rel32 -> 0F 80 00 00 00 00
        assert_eq!(&code[0..6], &[0x0F, 0x80, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_jno_forward() {
        let mut mir = X86Mir::new();
        mir.push(X86Inst::Jno {
            label: LabelId::new(0),
        });
        mir.push(X86Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // jno rel32 -> 0F 81 00 00 00 00
        assert_eq!(&code[0..6], &[0x0F, 0x81, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_jb_forward() {
        let mut mir = X86Mir::new();
        mir.push(X86Inst::Jb {
            label: LabelId::new(0),
        });
        mir.push(X86Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // jb rel32 -> 0F 82 00 00 00 00
        assert_eq!(&code[0..6], &[0x0F, 0x82, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_jae_forward() {
        let mut mir = X86Mir::new();
        mir.push(X86Inst::Jae {
            label: LabelId::new(0),
        });
        mir.push(X86Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // jae rel32 -> 0F 83 00 00 00 00
        assert_eq!(&code[0..6], &[0x0F, 0x83, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_jbe_forward() {
        let mut mir = X86Mir::new();
        mir.push(X86Inst::Jbe {
            label: LabelId::new(0),
        });
        mir.push(X86Inst::Label {
            id: LabelId::new(0),
        });

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &[]).emit().unwrap();

        // jbe rel32 -> 0F 86 00 00 00 00
        assert_eq!(&code[0..6], &[0x0F, 0x86, 0x00, 0x00, 0x00, 0x00]);
    }

    // --- LEA instruction ---

    #[test]
    fn test_lea_rax_rbp_minus_8() {
        let code = emit_single(X86Inst::Lea {
            dst: Operand::Physical(Reg::Rax),
            base: Reg::Rbp,
            index: None,
            scale: 1,
            disp: -8,
        });
        // lea rax, [rbp-8] -> 48 8D 45 F8 (REX.W 8D /r)
        assert_eq!(code, vec![0x48, 0x8D, 0x45, 0xF8]);
    }

    // --- String constant ---

    #[test]
    fn test_string_const_ptr() {
        let code = emit_single(X86Inst::StringConstPtr {
            dst: Operand::Physical(Reg::Rax),
            string_id: 0,
        });
        // lea rax, [rip+disp32] -> 48 8D 05 00 00 00 00
        assert_eq!(code, vec![0x48, 0x8D, 0x05, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_string_const_len() {
        let strings = vec!["hello".to_string()];
        let mut mir = X86Mir::new();
        mir.push(X86Inst::StringConstLen {
            dst: Operand::Physical(Reg::Rax),
            string_id: 0,
        });

        let (code, _) = Emitter::new(&mir, 0, 0, 0, &[], &strings).emit().unwrap();

        // mov rax, 5 -> 48 B8 05 00 00 00 00 00 00 00
        assert_eq!(
            code,
            vec![0x48, 0xB8, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    // --- RSP-based memory addressing (requires SIB byte) ---

    #[test]
    fn test_mov_rax_rsp_8() {
        let code = emit_single(X86Inst::MovRM {
            dst: Operand::Physical(Reg::Rax),
            base: Reg::Rsp,
            offset: 8,
        });
        // mov rax, [rsp+8] -> 48 8B 44 24 08 (REX.W 8B ModRM SIB disp8)
        // ModRM: mod=01 reg=000 r/m=100 (SIB) = 0x44
        // SIB: scale=00 index=100 (none) base=100 (RSP) = 0x24
        assert_eq!(code, vec![0x48, 0x8B, 0x44, 0x24, 0x08]);
    }

    #[test]
    fn test_mov_rsp_8_rax() {
        let code = emit_single(X86Inst::MovMR {
            base: Reg::Rsp,
            offset: 8,
            src: Operand::Physical(Reg::Rax),
        });
        // mov [rsp+8], rax -> 48 89 44 24 08
        assert_eq!(code, vec![0x48, 0x89, 0x44, 0x24, 0x08]);
    }

    // --- Extended register encoding ---

    #[test]
    fn test_mov_r15_r14() {
        let code = emit_single(X86Inst::MovRR {
            dst: Operand::Physical(Reg::R15),
            src: Operand::Physical(Reg::R14),
        });
        // mov r15, r14 -> 4D 89 F7 (REX.WRB 89 /r)
        assert_eq!(code, vec![0x4D, 0x89, 0xF7]);
    }

    #[test]
    fn test_mov_r8_imm64() {
        let code = emit_single(X86Inst::MovRI64 {
            dst: Operand::Physical(Reg::R8),
            imm: 0x123456789ABCDEF0u64 as i64,
        });
        // mov r8, imm64 -> 49 B8 F0 DE BC 9A 78 56 34 12 (REX.WB B8+rd imm64)
        assert_eq!(
            code,
            vec![0x49, 0xB8, 0xF0, 0xDE, 0xBC, 0x9A, 0x78, 0x56, 0x34, 0x12]
        );
    }
}
