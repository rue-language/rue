//! Register allocation with liveness analysis.
//!
//! This phase assigns physical registers to virtual registers using liveness
//! information to determine when registers can be reused. When we run out of
//! registers, values are spilled to the stack.
//!
//! The algorithm:
//! 1. Compute live ranges for all virtual registers
//! 2. Sort vregs by live range start (linear scan order)
//! 3. For each vreg, try to assign a register not used by interfering vregs
//! 4. If no register is available, spill the longest-range vreg to stack

use std::collections::HashSet;

use super::liveness::{self, LiveRange, LivenessInfo};
use super::mir::{Operand, Reg, VReg, X86Inst, X86Mir};

/// Available registers for allocation.
///
/// We ONLY use callee-saved registers for general allocation. This ensures
/// that values survive across function calls without needing explicit save/restore.
/// Caller-saved registers (rax, rcx, rdx, rsi, rdi, r8-r11) are avoided because
/// they get clobbered by function calls.
///
/// We also avoid:
/// - rsp (stack pointer)
/// - rbp (frame pointer)
/// - rax, rdx (used implicitly by idiv, and rax for scratch)
///
/// When we run out of callee-saved registers, values are spilled to the stack.
const ALLOCATABLE_REGS: &[Reg] = &[
    Reg::R12, // Callee-saved
    Reg::R13, // Callee-saved
    Reg::R14, // Callee-saved
    Reg::R15, // Callee-saved
    Reg::Rbx, // Callee-saved
];

/// Allocation result for a virtual register.
#[derive(Debug, Clone, Copy)]
enum Allocation {
    /// Allocated to a physical register.
    Register(Reg),
    /// Spilled to a stack slot (offset from RBP).
    Spill(i32),
}

/// Register allocator with liveness-based allocation.
pub struct RegAlloc {
    mir: X86Mir,
    /// Maps virtual register index to allocation.
    allocation: Vec<Option<Allocation>>,
    /// Liveness information.
    liveness: LivenessInfo,
    /// Next spill slot offset (negative from RBP).
    /// Starts after the existing local variables.
    next_spill_offset: i32,
    /// Number of spill slots used.
    num_spills: u32,
    /// Callee-saved registers that were used and need to be saved/restored.
    used_callee_saved: Vec<Reg>,
}

impl RegAlloc {
    /// Create a new register allocator.
    ///
    /// `existing_locals` is the number of local variable slots already allocated
    /// on the stack (we spill after those).
    pub fn new(mir: X86Mir, existing_locals: u32) -> Self {
        let vreg_count = mir.vreg_count() as usize;
        let liveness = liveness::analyze(&mir);

        // Spill slots start after existing locals
        // Each local is 8 bytes, slot 0 is at [rbp-8], etc.
        let next_spill_offset = -((existing_locals as i32 + 1) * 8);

        Self {
            mir,
            allocation: vec![None; vreg_count],
            liveness,
            next_spill_offset,
            num_spills: 0,
            used_callee_saved: Vec::new(),
        }
    }

    /// Get the number of spill slots used.
    pub fn num_spills(&self) -> u32 {
        self.num_spills
    }

    /// Perform register allocation and return the updated MIR.
    pub fn allocate(mut self) -> X86Mir {
        // Phase 1: Assign physical registers (or spill) to virtual registers
        self.assign_registers();

        // Phase 2: Rewrite instructions to use physical registers and insert spill code
        self.rewrite_instructions();

        self.mir
    }

    /// Perform register allocation and return the MIR, spill count, and used callee-saved registers.
    pub fn allocate_with_spills(mut self) -> (X86Mir, u32, Vec<Reg>) {
        // Phase 1: Assign physical registers (or spill) to virtual registers
        self.assign_registers();

        // Phase 2: Rewrite instructions to use physical registers and insert spill code
        self.rewrite_instructions();

        let num_spills = self.num_spills;
        let used_callee_saved = self.used_callee_saved;
        (self.mir, num_spills, used_callee_saved)
    }

    /// Assign physical registers to all virtual registers using linear scan.
    fn assign_registers(&mut self) {
        // Collect vregs with their live ranges, sorted by start position
        let mut vregs_by_start: Vec<(VReg, LiveRange)> = Vec::new();
        for vreg_idx in 0..self.mir.vreg_count() {
            let vreg = VReg::new(vreg_idx);
            if let Some(&range) = self.liveness.range(vreg) {
                vregs_by_start.push((vreg, range));
            }
        }
        vregs_by_start.sort_by_key(|(_, range)| range.start);

        // Track which registers are currently in use and when they become free
        // Map from register to (vreg using it, end of that vreg's live range)
        let mut active: Vec<(VReg, Reg, usize)> = Vec::new(); // (vreg, reg, end)

        for (vreg, range) in vregs_by_start {
            // Expire old intervals - registers whose vregs are no longer live
            active.retain(|&(_, _, end)| end >= range.start);

            // Find registers currently in use
            let used_regs: HashSet<Reg> = active.iter().map(|&(_, reg, _)| reg).collect();

            // Try to find a free register
            let mut allocated_reg = None;
            for &reg in ALLOCATABLE_REGS {
                if !used_regs.contains(&reg) {
                    allocated_reg = Some(reg);
                    break;
                }
            }

            if let Some(reg) = allocated_reg {
                // Assign this register
                self.allocation[vreg.index() as usize] = Some(Allocation::Register(reg));
                active.push((vreg, reg, range.end));
                // Track callee-saved register usage
                if !self.used_callee_saved.contains(&reg) {
                    self.used_callee_saved.push(reg);
                }
            } else {
                // No free register - need to spill
                // Strategy: spill the vreg with the longest remaining live range
                // (including the current one)

                // Find the vreg with the longest remaining range
                let mut longest_idx = None;
                let mut longest_end = range.end;
                for (i, &(_, _, end)) in active.iter().enumerate() {
                    if end > longest_end {
                        longest_end = end;
                        longest_idx = Some(i);
                    }
                }

                if let Some(idx) = longest_idx {
                    // Spill the existing vreg with longest range
                    let (spilled_vreg, freed_reg, _) = active.remove(idx);
                    let spill_offset = self.alloc_spill_slot();
                    self.allocation[spilled_vreg.index() as usize] =
                        Some(Allocation::Spill(spill_offset));

                    // Give the freed register to the current vreg
                    self.allocation[vreg.index() as usize] = Some(Allocation::Register(freed_reg));
                    active.push((vreg, freed_reg, range.end));
                } else {
                    // Current vreg has the longest range, spill it
                    let spill_offset = self.alloc_spill_slot();
                    self.allocation[vreg.index() as usize] = Some(Allocation::Spill(spill_offset));
                }
            }
        }
    }

    /// Allocate a new spill slot on the stack.
    fn alloc_spill_slot(&mut self) -> i32 {
        let offset = self.next_spill_offset;
        self.next_spill_offset -= 8;
        self.num_spills += 1;
        offset
    }

    /// Rewrite all instructions to use physical registers and handle spills.
    fn rewrite_instructions(&mut self) {
        // For spilled vregs, we need to insert load/store operations.
        // This is done by building a new instruction list.
        let old_instructions = std::mem::take(&mut self.mir).into_instructions();
        let mut new_mir = X86Mir::new();

        for inst in old_instructions {
            self.rewrite_inst(&mut new_mir, inst);
        }

        self.mir = new_mir;
    }

    /// Rewrite a single instruction, handling spills.
    fn rewrite_inst(&self, mir: &mut X86Mir, inst: X86Inst) {
        match inst {
            X86Inst::MovRI32 { dst, imm } => {
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::MovRI32 {
                            dst: Operand::Physical(reg),
                            imm,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        // Store immediate to stack via a temp register
                        // Use RAX as scratch (not in ALLOCATABLE_REGS)
                        mir.push(X86Inst::MovRI32 {
                            dst: Operand::Physical(Reg::Rax),
                            imm,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        // Physical register, pass through
                        mir.push(X86Inst::MovRI32 { dst, imm });
                    }
                }
            }

            X86Inst::MovRI64 { dst, imm } => match self.get_allocation(dst) {
                Some(Allocation::Register(reg)) => {
                    mir.push(X86Inst::MovRI64 {
                        dst: Operand::Physical(reg),
                        imm,
                    });
                }
                Some(Allocation::Spill(offset)) => {
                    mir.push(X86Inst::MovRI64 {
                        dst: Operand::Physical(Reg::Rax),
                        imm,
                    });
                    mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset,
                        src: Operand::Physical(Reg::Rax),
                    });
                }
                None => {
                    mir.push(X86Inst::MovRI64 { dst, imm });
                }
            },

            X86Inst::MovRR { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                let dst_alloc = self.get_allocation(dst);

                match dst_alloc {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::MovRR {
                            dst: Operand::Physical(reg),
                            src: src_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        // Move src to RAX (if not already), then store to stack
                        if src_op != Operand::Physical(Reg::Rax) {
                            mir.push(X86Inst::MovRR {
                                dst: Operand::Physical(Reg::Rax),
                                src: src_op,
                            });
                        }
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        mir.push(X86Inst::MovRR { dst, src: src_op });
                    }
                }
            }

            X86Inst::MovRM { dst, base, offset } => {
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::MovRM {
                            dst: Operand::Physical(reg),
                            base,
                            offset,
                        });
                    }
                    Some(Allocation::Spill(spill_offset)) => {
                        // Load to RAX, then store to spill slot
                        mir.push(X86Inst::MovRM {
                            dst: Operand::Physical(Reg::Rax),
                            base,
                            offset,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset: spill_offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        mir.push(X86Inst::MovRM { dst, base, offset });
                    }
                }
            }

            X86Inst::MovMR { base, offset, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                mir.push(X86Inst::MovMR {
                    base,
                    offset,
                    src: src_op,
                });
            }

            X86Inst::AddRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::AddRR { dst: d, src: s });
            }

            X86Inst::AddRR64 { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::AddRR64 { dst: d, src: s });
            }

            X86Inst::AddRI { dst, imm } => {
                self.emit_unop_imm(mir, dst, imm, |d, i| X86Inst::AddRI { dst: d, imm: i });
            }

            X86Inst::SubRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::SubRR { dst: d, src: s });
            }

            X86Inst::SubRR64 { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::SubRR64 { dst: d, src: s });
            }

            X86Inst::ImulRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::ImulRR { dst: d, src: s });
            }

            X86Inst::ImulRR64 { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::ImulRR64 { dst: d, src: s });
            }

            X86Inst::Neg { dst } => {
                self.emit_unop(mir, dst, |d| X86Inst::Neg { dst: d });
            }

            X86Inst::Neg64 { dst } => {
                self.emit_unop(mir, dst, |d| X86Inst::Neg64 { dst: d });
            }

            X86Inst::XorRI { dst, imm } => {
                self.emit_unop_imm(mir, dst, imm, |d, i| X86Inst::XorRI { dst: d, imm: i });
            }

            X86Inst::AndRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::AndRR { dst: d, src: s });
            }

            X86Inst::OrRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::OrRR { dst: d, src: s });
            }

            X86Inst::IdivR { src } => {
                let src_op = self.load_operand(mir, src, Reg::R10);
                mir.push(X86Inst::IdivR { src: src_op });
            }

            X86Inst::TestRR { src1, src2 } => {
                let src1_op = self.load_operand(mir, src1, Reg::Rax);
                let src2_op = self.load_operand(mir, src2, Reg::R10);
                mir.push(X86Inst::TestRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::CmpRR { src1, src2 } => {
                let src1_op = self.load_operand(mir, src1, Reg::Rax);
                let src2_op = self.load_operand(mir, src2, Reg::R10);
                mir.push(X86Inst::CmpRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::Cmp64RR { src1, src2 } => {
                let src1_op = self.load_operand(mir, src1, Reg::Rax);
                let src2_op = self.load_operand(mir, src2, Reg::R10);
                mir.push(X86Inst::Cmp64RR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::CmpRI { src, imm } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                mir.push(X86Inst::CmpRI { src: src_op, imm });
            }

            X86Inst::Sete { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Sete { dst: d });
            }

            X86Inst::Setne { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Setne { dst: d });
            }

            X86Inst::Setl { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Setl { dst: d });
            }

            X86Inst::Setg { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Setg { dst: d });
            }

            X86Inst::Setle { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Setle { dst: d });
            }

            X86Inst::Setge { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Setge { dst: d });
            }

            X86Inst::Setb { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Setb { dst: d });
            }

            X86Inst::Seta { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Seta { dst: d });
            }

            X86Inst::Setbe { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Setbe { dst: d });
            }

            X86Inst::Setae { dst } => {
                self.emit_setcc(mir, dst, |d| X86Inst::Setae { dst: d });
            }

            X86Inst::Movzx { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::Movzx {
                            dst: Operand::Physical(reg),
                            src: src_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(X86Inst::Movzx {
                            dst: Operand::Physical(Reg::Rax),
                            src: src_op,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        mir.push(X86Inst::Movzx { dst, src: src_op });
                    }
                }
            }

            X86Inst::Movsx8To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::Movsx8To64 {
                            dst: Operand::Physical(reg),
                            src: src_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(X86Inst::Movsx8To64 {
                            dst: Operand::Physical(Reg::Rax),
                            src: src_op,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        mir.push(X86Inst::Movsx8To64 { dst, src: src_op });
                    }
                }
            }

            X86Inst::Movsx16To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::Movsx16To64 {
                            dst: Operand::Physical(reg),
                            src: src_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(X86Inst::Movsx16To64 {
                            dst: Operand::Physical(Reg::Rax),
                            src: src_op,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        mir.push(X86Inst::Movsx16To64 { dst, src: src_op });
                    }
                }
            }

            X86Inst::Movsx32To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::Movsx32To64 {
                            dst: Operand::Physical(reg),
                            src: src_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(X86Inst::Movsx32To64 {
                            dst: Operand::Physical(Reg::Rax),
                            src: src_op,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        mir.push(X86Inst::Movsx32To64 { dst, src: src_op });
                    }
                }
            }

            X86Inst::Movzx8To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::Movzx8To64 {
                            dst: Operand::Physical(reg),
                            src: src_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(X86Inst::Movzx8To64 {
                            dst: Operand::Physical(Reg::Rax),
                            src: src_op,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        mir.push(X86Inst::Movzx8To64 { dst, src: src_op });
                    }
                }
            }

            X86Inst::Movzx16To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::Movzx16To64 {
                            dst: Operand::Physical(reg),
                            src: src_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(X86Inst::Movzx16To64 {
                            dst: Operand::Physical(Reg::Rax),
                            src: src_op,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        mir.push(X86Inst::Movzx16To64 { dst, src: src_op });
                    }
                }
            }

            X86Inst::Pop { dst } => match self.get_allocation(dst) {
                Some(Allocation::Register(reg)) => {
                    mir.push(X86Inst::Pop {
                        dst: Operand::Physical(reg),
                    });
                }
                Some(Allocation::Spill(offset)) => {
                    mir.push(X86Inst::Pop {
                        dst: Operand::Physical(Reg::Rax),
                    });
                    mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset,
                        src: Operand::Physical(Reg::Rax),
                    });
                }
                None => {
                    mir.push(X86Inst::Pop { dst });
                }
            },

            X86Inst::Push { src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax);
                mir.push(X86Inst::Push { src: src_op });
            }

            X86Inst::Lea {
                dst,
                base,
                index,
                scale,
                disp,
            } => match self.get_allocation(dst) {
                Some(Allocation::Register(reg)) => {
                    mir.push(X86Inst::Lea {
                        dst: Operand::Physical(reg),
                        base,
                        index,
                        scale,
                        disp,
                    });
                }
                Some(Allocation::Spill(offset)) => {
                    mir.push(X86Inst::Lea {
                        dst: Operand::Physical(Reg::Rax),
                        base,
                        index,
                        scale,
                        disp,
                    });
                    mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset,
                        src: Operand::Physical(Reg::Rax),
                    });
                }
                None => {
                    mir.push(X86Inst::Lea {
                        dst,
                        base,
                        index,
                        scale,
                        disp,
                    });
                }
            },

            X86Inst::Shl { dst, count } => {
                // SHL needs count in RCX
                let count_op = self.load_operand(mir, count, Reg::Rcx);
                if count_op != Operand::Physical(Reg::Rcx) {
                    mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rcx),
                        src: count_op,
                    });
                }

                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::Shl {
                            dst: Operand::Physical(reg),
                            count: Operand::Physical(Reg::Rcx),
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(X86Inst::MovRM {
                            dst: Operand::Physical(Reg::Rax),
                            base: Reg::Rbp,
                            offset,
                        });
                        mir.push(X86Inst::Shl {
                            dst: Operand::Physical(Reg::Rax),
                            count: Operand::Physical(Reg::Rcx),
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    None => {
                        mir.push(X86Inst::Shl {
                            dst,
                            count: Operand::Physical(Reg::Rcx),
                        });
                    }
                }
            }

            X86Inst::MovRMIndexed { dst, base, offset } => {
                // Load base vreg into scratch register
                let base_op = Operand::Virtual(base);
                let base_reg = self.load_operand(mir, base_op, Reg::Rax);
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => Reg::Rax,
                };

                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::MovRM {
                            dst: Operand::Physical(reg),
                            base: base_phys,
                            offset,
                        });
                    }
                    Some(Allocation::Spill(spill_off)) => {
                        mir.push(X86Inst::MovRM {
                            dst: Operand::Physical(Reg::Rdx),
                            base: base_phys,
                            offset,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset: spill_off,
                            src: Operand::Physical(Reg::Rdx),
                        });
                    }
                    None => {
                        mir.push(X86Inst::MovRM {
                            dst,
                            base: base_phys,
                            offset,
                        });
                    }
                }
            }

            X86Inst::MovMRIndexed { base, offset, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rdx);
                let base_op = Operand::Virtual(base);
                let base_reg = self.load_operand(mir, base_op, Reg::Rax);
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => Reg::Rax,
                };
                mir.push(X86Inst::MovMR {
                    base: base_phys,
                    offset,
                    src: src_op,
                });
            }

            X86Inst::StringConstPtr { dst, string_id } => match self.get_allocation(dst) {
                Some(Allocation::Register(reg)) => {
                    mir.push(X86Inst::StringConstPtr {
                        dst: Operand::Physical(reg),
                        string_id,
                    });
                }
                Some(Allocation::Spill(offset)) => {
                    mir.push(X86Inst::StringConstPtr {
                        dst: Operand::Physical(Reg::Rax),
                        string_id,
                    });
                    mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset,
                        src: Operand::Physical(Reg::Rax),
                    });
                }
                None => {
                    mir.push(X86Inst::StringConstPtr { dst, string_id });
                }
            },

            X86Inst::StringConstLen { dst, string_id } => match self.get_allocation(dst) {
                Some(Allocation::Register(reg)) => {
                    mir.push(X86Inst::StringConstLen {
                        dst: Operand::Physical(reg),
                        string_id,
                    });
                }
                Some(Allocation::Spill(offset)) => {
                    mir.push(X86Inst::StringConstLen {
                        dst: Operand::Physical(Reg::Rax),
                        string_id,
                    });
                    mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset,
                        src: Operand::Physical(Reg::Rax),
                    });
                }
                None => {
                    mir.push(X86Inst::StringConstLen { dst, string_id });
                }
            },

            // Instructions without register operands pass through unchanged
            X86Inst::Cdq => mir.push(X86Inst::Cdq),
            X86Inst::Jz { label } => mir.push(X86Inst::Jz { label }),
            X86Inst::Jnz { label } => mir.push(X86Inst::Jnz { label }),
            X86Inst::Jo { label } => mir.push(X86Inst::Jo { label }),
            X86Inst::Jno { label } => mir.push(X86Inst::Jno { label }),
            X86Inst::Jb { label } => mir.push(X86Inst::Jb { label }),
            X86Inst::Jae { label } => mir.push(X86Inst::Jae { label }),
            X86Inst::Jbe { label } => mir.push(X86Inst::Jbe { label }),
            X86Inst::Jmp { label } => mir.push(X86Inst::Jmp { label }),
            X86Inst::Label { id } => mir.push(X86Inst::Label { id }),
            X86Inst::CallRel { symbol } => mir.push(X86Inst::CallRel { symbol }),
            X86Inst::Syscall => mir.push(X86Inst::Syscall),
            X86Inst::Ret => mir.push(X86Inst::Ret),
        }
    }

    /// Get the allocation for an operand (returns None for physical registers).
    fn get_allocation(&self, operand: Operand) -> Option<Allocation> {
        match operand {
            Operand::Virtual(vreg) => self.allocation[vreg.index() as usize],
            Operand::Physical(_) => None,
        }
    }

    /// Load an operand into a physical register, inserting a load if spilled.
    /// Returns the operand to use (either the allocated register or the scratch register).
    fn load_operand(&self, mir: &mut X86Mir, operand: Operand, scratch: Reg) -> Operand {
        match operand {
            Operand::Virtual(vreg) => match self.allocation[vreg.index() as usize] {
                Some(Allocation::Register(reg)) => Operand::Physical(reg),
                Some(Allocation::Spill(offset)) => {
                    mir.push(X86Inst::MovRM {
                        dst: Operand::Physical(scratch),
                        base: Reg::Rbp,
                        offset,
                    });
                    Operand::Physical(scratch)
                }
                None => panic!("vreg {} not allocated", vreg.index()),
            },
            Operand::Physical(reg) => Operand::Physical(reg),
        }
    }

    /// Emit a binary operation (dst = dst op src).
    fn emit_binop<F>(&self, mir: &mut X86Mir, dst: Operand, src: Operand, make_inst: F)
    where
        F: FnOnce(Operand, Operand) -> X86Inst,
    {
        // Load src first (use R10 as scratch to avoid clobbering RAX)
        let src_op = self.load_operand(mir, src, Reg::R10);

        match self.get_allocation(dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src_op));
            }
            Some(Allocation::Spill(offset)) => {
                // Load dst from stack to RAX
                mir.push(X86Inst::MovRM {
                    dst: Operand::Physical(Reg::Rax),
                    base: Reg::Rbp,
                    offset,
                });
                // Perform operation
                mir.push(make_inst(Operand::Physical(Reg::Rax), src_op));
                // Store result back to stack
                mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Physical(Reg::Rax),
                });
            }
            None => {
                // Physical register
                mir.push(make_inst(dst, src_op));
            }
        }
    }

    /// Emit a unary operation (dst = op dst).
    fn emit_unop<F>(&self, mir: &mut X86Mir, dst: Operand, make_inst: F)
    where
        F: FnOnce(Operand) -> X86Inst,
    {
        match self.get_allocation(dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg)));
            }
            Some(Allocation::Spill(offset)) => {
                // Load from stack
                mir.push(X86Inst::MovRM {
                    dst: Operand::Physical(Reg::Rax),
                    base: Reg::Rbp,
                    offset,
                });
                // Perform operation
                mir.push(make_inst(Operand::Physical(Reg::Rax)));
                // Store back
                mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Physical(Reg::Rax),
                });
            }
            None => {
                mir.push(make_inst(dst));
            }
        }
    }

    /// Emit a unary operation with immediate (dst = dst op imm).
    fn emit_unop_imm<F>(&self, mir: &mut X86Mir, dst: Operand, imm: i32, make_inst: F)
    where
        F: FnOnce(Operand, i32) -> X86Inst,
    {
        match self.get_allocation(dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), imm));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(X86Inst::MovRM {
                    dst: Operand::Physical(Reg::Rax),
                    base: Reg::Rbp,
                    offset,
                });
                mir.push(make_inst(Operand::Physical(Reg::Rax), imm));
                mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Physical(Reg::Rax),
                });
            }
            None => {
                mir.push(make_inst(dst, imm));
            }
        }
    }

    /// Emit a setcc instruction (dst = flags ? 1 : 0).
    fn emit_setcc<F>(&self, mir: &mut X86Mir, dst: Operand, make_inst: F)
    where
        F: FnOnce(Operand) -> X86Inst,
    {
        match self.get_allocation(dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg)));
            }
            Some(Allocation::Spill(offset)) => {
                // setcc writes a byte, so we use RAX and store
                mir.push(make_inst(Operand::Physical(Reg::Rax)));
                mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Physical(Reg::Rax),
                });
            }
            None => {
                mir.push(make_inst(dst));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_allocation() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();

        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 42,
        });

        let mir = RegAlloc::new(mir, 0).allocate();

        // v0 should be allocated to R12 (first allocatable)
        match &mir.instructions()[0] {
            X86Inst::MovRI32 { dst, imm } => {
                assert_eq!(*dst, Operand::Physical(Reg::R12));
                assert_eq!(*imm, 42);
            }
            _ => panic!("expected MovRI32"),
        }
    }

    #[test]
    fn test_physical_reg_preserved() {
        let mut mir = X86Mir::new();

        // Instruction with physical register should be unchanged
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rdi),
            imm: 60,
        });

        let mir = RegAlloc::new(mir, 0).allocate();

        match &mir.instructions()[0] {
            X86Inst::MovRI32 { dst, imm } => {
                assert_eq!(*dst, Operand::Physical(Reg::Rdi));
                assert_eq!(*imm, 60);
            }
            _ => panic!("expected MovRI32"),
        }
    }

    #[test]
    fn test_non_interfering_regs_can_share() {
        // Two vregs with non-overlapping live ranges can share a register
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();

        // v0 = 1 (defined, immediately dead since not used)
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 1,
        });
        // v1 = 2 (defined after v0 is dead)
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v1),
            imm: 2,
        });

        let mir = RegAlloc::new(mir, 0).allocate();

        // Both can be allocated to R12 since they don't interfere
        match (&mir.instructions()[0], &mir.instructions()[1]) {
            (X86Inst::MovRI32 { dst: d0, .. }, X86Inst::MovRI32 { dst: d1, .. }) => {
                // They should both get R12 since v0 is dead before v1 is defined
                assert_eq!(*d0, Operand::Physical(Reg::R12));
                assert_eq!(*d1, Operand::Physical(Reg::R12));
            }
            _ => panic!("expected two MovRI32"),
        }
    }

    #[test]
    fn test_interfering_regs_get_different() {
        // Two vregs with overlapping live ranges must use different registers
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();

        // v0 = 1
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 1,
        });
        // v1 = 2 (v0 still live)
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v1),
            imm: 2,
        });
        // use v0 (extends v0's live range to here)
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rdi),
            src: Operand::Virtual(v0),
        });

        let mir = RegAlloc::new(mir, 0).allocate();

        // v0 and v1 should get different registers
        let d0 = match &mir.instructions()[0] {
            X86Inst::MovRI32 { dst, .. } => *dst,
            _ => panic!("expected MovRI32"),
        };
        let d1 = match &mir.instructions()[1] {
            X86Inst::MovRI32 { dst, .. } => *dst,
            _ => panic!("expected MovRI32"),
        };

        assert_ne!(d0, d1, "interfering vregs should get different registers");
    }
}
