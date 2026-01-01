//! Register allocation with liveness analysis.
//!
//! This phase assigns physical registers to virtual registers using liveness
//! information to determine when registers can be reused. When we run out of
//! registers, values are spilled to the stack.
//!
//! The algorithm:
//! 1. Compute live ranges for all virtual registers
//! 2. Perform register coalescing to eliminate redundant moves
//! 3. Sort vregs by live range start (linear scan order)
//! 4. For each vreg, try to assign a register not used by interfering vregs
//! 5. If no register is available, spill the longest-range vreg to stack

use rue_error::{CompileError, CompileResult, ErrorKind};

use super::liveness::{self, LivenessInfo};
use super::mir::{Operand, Reg, VReg, X86Inst, X86Mir};
use crate::alloc_dst;
use crate::index_map::IndexMap;
use crate::regalloc::{
    Allocation, CoalesceCandidate, CoalesceResult, RegAllocDebugInfo, coalesce, linear_scan,
    linear_scan_with_debug,
};

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

/// Register allocator with liveness-based allocation.
pub struct RegAlloc {
    mir: X86Mir,
    /// Maps virtual register to its allocation (register or spill slot).
    allocation: IndexMap<VReg, Option<Allocation<Reg>>>,
    /// Liveness information.
    liveness: LivenessInfo,
    /// Result of register coalescing.
    coalesce_result: CoalesceResult,
    /// Number of spill slots used.
    num_spills: u32,
    /// Callee-saved registers that were used and need to be saved/restored.
    used_callee_saved: Vec<Reg>,
    /// Number of existing local variable slots.
    existing_locals: u32,
}

impl RegAlloc {
    /// Create a new register allocator.
    ///
    /// `existing_locals` is the number of local variable slots already allocated
    /// on the stack (we spill after those).
    pub fn new(mir: X86Mir, existing_locals: u32) -> Self {
        let vreg_count = mir.vreg_count() as usize;
        let mut liveness = liveness::analyze(&mir);

        // Collect coalescing candidates: MovRR where both src and dst are virtual
        let candidates: Vec<CoalesceCandidate> = mir
            .instructions()
            .iter()
            .enumerate()
            .filter_map(|(idx, inst)| {
                if let X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(src),
                } = inst
                {
                    Some(CoalesceCandidate {
                        inst_idx: idx,
                        dst: *dst,
                        src: *src,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Perform register coalescing
        let coalesce_result = coalesce(&candidates, &mut liveness);

        let mut allocation = IndexMap::with_capacity(vreg_count);
        allocation.resize(vreg_count, None);

        Self {
            mir,
            allocation,
            liveness,
            coalesce_result,
            num_spills: 0,
            used_callee_saved: Vec::new(),
            existing_locals,
        }
    }

    /// Get the number of spill slots used.
    pub fn num_spills(&self) -> u32 {
        self.num_spills
    }

    /// Perform register allocation and return the updated MIR.
    pub fn allocate(mut self) -> CompileResult<X86Mir> {
        // Phase 1: Assign physical registers (or spill) to virtual registers
        self.assign_registers();

        // Phase 2: Rewrite instructions to use physical registers and insert spill code
        self.rewrite_instructions()?;

        Ok(self.mir)
    }

    /// Perform register allocation and return the MIR, spill count, and used callee-saved registers.
    pub fn allocate_with_spills(mut self) -> CompileResult<(X86Mir, u32, Vec<Reg>)> {
        // Phase 1: Assign physical registers (or spill) to virtual registers
        self.assign_registers();

        // Phase 2: Rewrite instructions to use physical registers and insert spill code
        self.rewrite_instructions()?;

        let num_spills = self.num_spills;
        let used_callee_saved = self.used_callee_saved;
        Ok((self.mir, num_spills, used_callee_saved))
    }

    /// Perform register allocation and return debug information.
    ///
    /// This is used by `--emit regalloc` to show allocation decisions.
    pub fn allocate_with_debug(
        mut self,
    ) -> CompileResult<(X86Mir, u32, Vec<Reg>, RegAllocDebugInfo<Reg>)> {
        // Phase 1: Assign physical registers with debug info
        let debug_info = self.assign_registers_with_debug();

        // Phase 2: Rewrite instructions to use physical registers and insert spill code
        self.rewrite_instructions()?;

        let num_spills = self.num_spills;
        let used_callee_saved = self.used_callee_saved;
        Ok((self.mir, num_spills, used_callee_saved, debug_info))
    }

    /// Assign physical registers to all virtual registers using linear scan.
    fn assign_registers(&mut self) {
        let (allocation, num_spills, used_callee_saved) = linear_scan(
            self.mir.vreg_count(),
            &self.liveness,
            ALLOCATABLE_REGS,
            self.existing_locals,
        );
        self.allocation = allocation;
        self.num_spills = num_spills;
        self.used_callee_saved = used_callee_saved;
    }

    /// Assign physical registers and also collect debug information.
    fn assign_registers_with_debug(&mut self) -> RegAllocDebugInfo<Reg> {
        let (allocation, num_spills, used_callee_saved, debug_info) = linear_scan_with_debug(
            self.mir.vreg_count(),
            &self.liveness,
            ALLOCATABLE_REGS,
            self.existing_locals,
        );
        self.allocation = allocation;
        self.num_spills = num_spills;
        self.used_callee_saved = used_callee_saved;
        debug_info
    }

    /// Rewrite all instructions to use physical registers and handle spills.
    fn rewrite_instructions(&mut self) -> CompileResult<()> {
        // For spilled vregs, we need to insert load/store operations.
        // This is done by building a new instruction list.
        // Take symbols from old MIR before taking instructions
        let symbols = self.mir.take_symbols();
        let old_instructions = std::mem::take(&mut self.mir).into_instructions();
        let mut new_mir = X86Mir::new();
        // Restore symbols to new MIR
        new_mir.set_symbols(symbols);

        for (idx, inst) in old_instructions.into_iter().enumerate() {
            // Skip eliminated moves (from register coalescing)
            if self.coalesce_result.is_eliminated(idx) {
                continue;
            }
            self.rewrite_inst(&mut new_mir, inst)?;
        }

        self.mir = new_mir;
        Ok(())
    }

    /// Rewrite a single instruction, handling spills.
    fn rewrite_inst(&self, mir: &mut X86Mir, inst: X86Inst) -> CompileResult<()> {
        match inst {
            X86Inst::MovRI32 { dst, imm } => {
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::MovRI32 { dst: dst_op, imm });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::MovRI64 { dst, imm } => {
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::MovRI64 { dst: dst_op, imm });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::MovRR { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
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
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => {
                        mir.push(X86Inst::MovRR { dst, src: src_op });
                    }
                }
            }

            X86Inst::MovRM { dst, base, offset } => {
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::MovRM { dst: dst_op, base, offset });
                    },
                    store |spill_offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset: spill_offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::MovMR { base, offset, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                mir.push(X86Inst::MovMR {
                    base,
                    offset,
                    src: src_op,
                });
            }

            X86Inst::AddRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::AddRR { dst: d, src: s })?;
            }

            X86Inst::AddRR64 { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::AddRR64 { dst: d, src: s })?;
            }

            X86Inst::AddRI { dst, imm } => {
                self.emit_unop_imm(mir, dst, imm, |d, i| X86Inst::AddRI { dst: d, imm: i });
            }

            X86Inst::SubRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::SubRR { dst: d, src: s })?;
            }

            X86Inst::SubRR64 { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::SubRR64 { dst: d, src: s })?;
            }

            X86Inst::ImulRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::ImulRR { dst: d, src: s })?;
            }

            X86Inst::ImulRR64 { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::ImulRR64 { dst: d, src: s })?;
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
                self.emit_binop(mir, dst, src, |d, s| X86Inst::AndRR { dst: d, src: s })?;
            }

            X86Inst::OrRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::OrRR { dst: d, src: s })?;
            }

            X86Inst::XorRR { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| X86Inst::XorRR { dst: d, src: s })?;
            }

            X86Inst::NotR { dst } => {
                self.emit_unop(mir, dst, |d| X86Inst::NotR { dst: d });
            }

            X86Inst::ShlRCl { dst } => {
                self.emit_unop(mir, dst, |d| X86Inst::ShlRCl { dst: d });
            }

            X86Inst::Shl32RCl { dst } => {
                self.emit_unop(mir, dst, |d| X86Inst::Shl32RCl { dst: d });
            }

            X86Inst::ShlRI { dst, imm } => {
                self.emit_unop_imm_u8(mir, dst, imm, |d, i| X86Inst::ShlRI { dst: d, imm: i });
            }

            X86Inst::Shl32RI { dst, imm } => {
                self.emit_unop_imm_u8(mir, dst, imm, |d, i| X86Inst::Shl32RI { dst: d, imm: i });
            }

            X86Inst::ShrRCl { dst } => {
                self.emit_unop(mir, dst, |d| X86Inst::ShrRCl { dst: d });
            }

            X86Inst::Shr32RCl { dst } => {
                self.emit_unop(mir, dst, |d| X86Inst::Shr32RCl { dst: d });
            }

            X86Inst::ShrRI { dst, imm } => {
                self.emit_unop_imm_u8(mir, dst, imm, |d, i| X86Inst::ShrRI { dst: d, imm: i });
            }

            X86Inst::Shr32RI { dst, imm } => {
                self.emit_unop_imm_u8(mir, dst, imm, |d, i| X86Inst::Shr32RI { dst: d, imm: i });
            }

            X86Inst::SarRCl { dst } => {
                self.emit_unop(mir, dst, |d| X86Inst::SarRCl { dst: d });
            }

            X86Inst::Sar32RCl { dst } => {
                self.emit_unop(mir, dst, |d| X86Inst::Sar32RCl { dst: d });
            }

            X86Inst::SarRI { dst, imm } => {
                self.emit_unop_imm_u8(mir, dst, imm, |d, i| X86Inst::SarRI { dst: d, imm: i });
            }

            X86Inst::Sar32RI { dst, imm } => {
                self.emit_unop_imm_u8(mir, dst, imm, |d, i| X86Inst::Sar32RI { dst: d, imm: i });
            }

            X86Inst::IdivR { src } => {
                let src_op = self.load_operand(mir, src, Reg::R10)?;
                mir.push(X86Inst::IdivR { src: src_op });
            }

            X86Inst::TestRR { src1, src2 } => {
                let src1_op = self.load_operand(mir, src1, Reg::Rax)?;
                let src2_op = self.load_operand(mir, src2, Reg::R10)?;
                mir.push(X86Inst::TestRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::CmpRR { src1, src2 } => {
                let src1_op = self.load_operand(mir, src1, Reg::Rax)?;
                let src2_op = self.load_operand(mir, src2, Reg::R10)?;
                mir.push(X86Inst::CmpRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::Cmp64RR { src1, src2 } => {
                let src1_op = self.load_operand(mir, src1, Reg::Rax)?;
                let src2_op = self.load_operand(mir, src2, Reg::R10)?;
                mir.push(X86Inst::Cmp64RR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::CmpRI { src, imm } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                mir.push(X86Inst::CmpRI { src: src_op, imm });
            }

            X86Inst::Cmp64RI { src, imm } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                mir.push(X86Inst::Cmp64RI { src: src_op, imm });
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
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movzx { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movsx8To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movsx8To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movsx16To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movsx16To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movsx32To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movsx32To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movzx8To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movzx8To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movzx16To64 { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movzx16To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Pop { dst } => {
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Pop { dst: dst_op });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Push { src } => {
                let src_op = self.load_operand(mir, src, Reg::Rax)?;
                mir.push(X86Inst::Push { src: src_op });
            }

            X86Inst::Lea {
                dst,
                base,
                index,
                scale,
                disp,
            } => {
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Lea { dst: dst_op, base, index, scale, disp });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Shl { dst, count } => {
                // SHL needs count in RCX
                let count_op = self.load_operand(mir, count, Reg::Rcx)?;
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
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
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
                let base_reg = self.load_operand(mir, base_op, Reg::Rax)?;
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
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
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
                let src_op = self.load_operand(mir, src, Reg::Rdx)?;
                let base_op = Operand::Virtual(base);
                let base_reg = self.load_operand(mir, base_op, Reg::Rax)?;
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

            X86Inst::MovRMSib {
                dst,
                base,
                index,
                scale,
                disp,
            } => {
                // Load base into a register (use Rdx as scratch to avoid conflicts)
                let base_op = self.load_operand(mir, base, Reg::Rdx)?;
                // Load index into a register (use Rcx as scratch)
                // Note: RSP cannot be used as index in SIB encoding
                let index_op = self.load_operand(mir, index, Reg::Rcx)?;

                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(X86Inst::MovRMSib {
                            dst: Operand::Physical(reg),
                            base: base_op,
                            index: index_op,
                            scale,
                            disp,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        // Load into scratch register then store
                        mir.push(X86Inst::MovRMSib {
                            dst: Operand::Physical(Reg::Rax),
                            base: base_op,
                            index: index_op,
                            scale,
                            disp,
                        });
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    }
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => {
                        mir.push(X86Inst::MovRMSib {
                            dst,
                            base: base_op,
                            index: index_op,
                            scale,
                            disp,
                        });
                    }
                }
            }

            X86Inst::MovMRSib {
                base,
                index,
                scale,
                disp,
                src,
            } => {
                // Load base into a register (use Rdx as scratch)
                let base_op = self.load_operand(mir, base, Reg::Rdx)?;
                // Load index into a register (use Rcx as scratch)
                let index_op = self.load_operand(mir, index, Reg::Rcx)?;
                // Load src value
                let src_op = self.load_operand(mir, src, Reg::Rax)?;

                mir.push(X86Inst::MovMRSib {
                    base: base_op,
                    index: index_op,
                    scale,
                    disp,
                    src: src_op,
                });
            }

            X86Inst::StringConstPtr { dst, string_id } => {
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::StringConstPtr { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::StringConstLen { dst, string_id } => {
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::StringConstLen { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::StringConstCap { dst, string_id } => {
                alloc_dst!(self.get_allocation(dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::StringConstCap { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            // Instructions without register operands pass through unchanged
            X86Inst::Cdq => mir.push(X86Inst::Cdq),
            X86Inst::Jz { label } => mir.push(X86Inst::Jz { label }),
            X86Inst::Jnz { label } => mir.push(X86Inst::Jnz { label }),
            X86Inst::Jo { label } => mir.push(X86Inst::Jo { label }),
            X86Inst::Jno { label } => mir.push(X86Inst::Jno { label }),
            X86Inst::Jb { label } => mir.push(X86Inst::Jb { label }),
            X86Inst::Jae { label } => mir.push(X86Inst::Jae { label }),
            X86Inst::Jbe { label } => mir.push(X86Inst::Jbe { label }),
            X86Inst::Jge { label } => mir.push(X86Inst::Jge { label }),
            X86Inst::Jle { label } => mir.push(X86Inst::Jle { label }),
            X86Inst::Jmp { label } => mir.push(X86Inst::Jmp { label }),
            X86Inst::Label { id } => mir.push(X86Inst::Label { id }),
            X86Inst::CallRel { symbol_id } => mir.push(X86Inst::CallRel { symbol_id }),
            X86Inst::Syscall => mir.push(X86Inst::Syscall),
            X86Inst::Ret => mir.push(X86Inst::Ret),
        }
        Ok(())
    }

    /// Get the allocation for an operand (returns None for physical registers).
    ///
    /// For coalesced vregs, this looks up the allocation of the representative vreg.
    fn get_allocation(&self, operand: Operand) -> Option<Allocation<Reg>> {
        match operand {
            Operand::Virtual(vreg) => {
                // Use the representative vreg for coalesced registers
                let rep = self.coalesce_result.representative(vreg);
                self.allocation[rep]
            }
            Operand::Physical(_) => None,
        }
    }

    /// Load an operand into a physical register, inserting a load if spilled
    /// or rematerializing if marked for rematerialization.
    /// Returns the operand to use (either the allocated register or the scratch register).
    ///
    /// For coalesced vregs, this loads the allocation of the representative vreg.
    fn load_operand(
        &self,
        mir: &mut X86Mir,
        operand: Operand,
        scratch: Reg,
    ) -> CompileResult<Operand> {
        match operand {
            Operand::Virtual(vreg) => {
                // Use the representative vreg for coalesced registers
                let rep = self.coalesce_result.representative(vreg);
                match self.allocation[rep] {
                    Some(Allocation::Register(reg)) => Ok(Operand::Physical(reg)),
                    Some(Allocation::Spill(offset)) => {
                        mir.push(X86Inst::MovRM {
                            dst: Operand::Physical(scratch),
                            base: Reg::Rbp,
                            offset,
                        });
                        Ok(Operand::Physical(scratch))
                    }
                    Some(Allocation::Rematerialize(remat_op)) => {
                        // Rematerialize the value instead of loading from stack
                        use crate::regalloc::RematerializeOp;
                        match remat_op {
                            RematerializeOp::Const32(imm) => {
                                mir.push(X86Inst::MovRI32 {
                                    dst: Operand::Physical(scratch),
                                    imm,
                                });
                            }
                            RematerializeOp::Const64(imm) => {
                                mir.push(X86Inst::MovRI64 {
                                    dst: Operand::Physical(scratch),
                                    imm,
                                });
                            }
                            RematerializeOp::StringPtr(string_id) => {
                                mir.push(X86Inst::StringConstPtr {
                                    dst: Operand::Physical(scratch),
                                    string_id,
                                });
                            }
                            RematerializeOp::StringLen(string_id) => {
                                mir.push(X86Inst::StringConstLen {
                                    dst: Operand::Physical(scratch),
                                    string_id,
                                });
                            }
                            RematerializeOp::StringCap(string_id) => {
                                mir.push(X86Inst::StringConstCap {
                                    dst: Operand::Physical(scratch),
                                    string_id,
                                });
                            }
                        }
                        Ok(Operand::Physical(scratch))
                    }
                    None => Err(CompileError::without_span(ErrorKind::LinkError(format!(
                        "internal codegen error: virtual register {} was not allocated",
                        rep.index()
                    )))),
                }
            }
            Operand::Physical(reg) => Ok(Operand::Physical(reg)),
        }
    }

    /// Emit a binary operation (dst = dst op src).
    fn emit_binop<F>(
        &self,
        mir: &mut X86Mir,
        dst: Operand,
        src: Operand,
        make_inst: F,
    ) -> CompileResult<()>
    where
        F: FnOnce(Operand, Operand) -> X86Inst,
    {
        // Load src first (use R10 as scratch to avoid clobbering RAX)
        let src_op = self.load_operand(mir, src, Reg::R10)?;

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
            Some(Allocation::Rematerialize(_)) => {
                unreachable!("destination cannot be rematerializable")
            }
            None => {
                // Physical register
                mir.push(make_inst(dst, src_op));
            }
        }
        Ok(())
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
            Some(Allocation::Rematerialize(_)) => {
                unreachable!("destination cannot be rematerializable")
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
            Some(Allocation::Rematerialize(_)) => {
                unreachable!("destination cannot be rematerializable")
            }
            None => {
                mir.push(make_inst(dst, imm));
            }
        }
    }

    /// Emit a unary operation with u8 immediate (dst = dst op imm).
    fn emit_unop_imm_u8<F>(&self, mir: &mut X86Mir, dst: Operand, imm: u8, make_inst: F)
    where
        F: FnOnce(Operand, u8) -> X86Inst,
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
            Some(Allocation::Rematerialize(_)) => {
                unreachable!("destination cannot be rematerializable")
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
            Some(Allocation::Rematerialize(_)) => {
                unreachable!("destination cannot be rematerializable")
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

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();

        // v0 should be allocated to R12 (first allocatable)
        match &mir.instructions()[0] {
            X86Inst::MovRI32 { dst, imm } => {
                assert_eq!(dst, &Operand::Physical(Reg::R12));
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

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();

        match &mir.instructions()[0] {
            X86Inst::MovRI32 { dst, imm } => {
                assert_eq!(dst, &Operand::Physical(Reg::Rdi));
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

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();

        // Both can be allocated to R12 since they don't interfere
        match (&mir.instructions()[0], &mir.instructions()[1]) {
            (X86Inst::MovRI32 { dst: d0, .. }, X86Inst::MovRI32 { dst: d1, .. }) => {
                // They should both get R12 since v0 is dead before v1 is defined
                assert_eq!(d0, &Operand::Physical(Reg::R12));
                assert_eq!(d1, &Operand::Physical(Reg::R12));
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

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();

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

    // ========================================
    // Spill slot conflict tests
    // ========================================

    #[test]
    fn test_spill_inserts_load_store() {
        // Force a spill and verify load/store instructions are inserted
        let mut mir = X86Mir::new();

        // Create 6 vregs to force spilling (only 5 allocatable regs: R12-R15, Rbx)
        let vregs: Vec<VReg> = (0..6).map(|_| mir.alloc_vreg()).collect();

        // Define all vregs
        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(vreg),
                imm: i as i32,
            });
        }

        // Use all vregs to keep them live
        for &vreg in &vregs {
            mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 1, "Should have exactly 1 spill");

        // Verify there's at least one MovMR (store to stack) and MovRM (load from stack)
        let has_store = mir
            .instructions()
            .iter()
            .any(|inst| matches!(inst, X86Inst::MovMR { base: Reg::Rbp, .. }));
        let has_load = mir
            .instructions()
            .iter()
            .any(|inst| matches!(inst, X86Inst::MovRM { base: Reg::Rbp, .. }));

        assert!(has_store, "Should have a store to stack");
        assert!(has_load, "Should have a load from stack");
    }

    #[test]
    fn test_multiple_spills_unique_offsets() {
        // Force multiple spills and verify they get unique stack offsets
        let mut mir = X86Mir::new();

        // Create 10 vregs to force 5 spills
        let vregs: Vec<VReg> = (0..10).map(|_| mir.alloc_vreg()).collect();

        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(vreg),
                imm: i as i32,
            });
        }

        for &vreg in &vregs {
            mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 5);

        // Collect all unique stack offsets used in loads/stores
        let mut offsets = std::collections::HashSet::new();
        for inst in mir.instructions() {
            match inst {
                X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    ..
                } => {
                    offsets.insert(*offset);
                }
                X86Inst::MovRM {
                    base: Reg::Rbp,
                    offset,
                    ..
                } => {
                    offsets.insert(*offset);
                }
                _ => {}
            }
        }

        // Each spilled vreg should use a unique offset
        assert_eq!(
            offsets.len(),
            5,
            "Each spill should use a unique stack offset"
        );
    }

    #[test]
    fn test_spill_with_existing_locals() {
        // Test that spills are placed after existing local variables
        let mut mir = X86Mir::new();

        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let v2 = mir.alloc_vreg();
        let v3 = mir.alloc_vreg();
        let v4 = mir.alloc_vreg();
        let v5 = mir.alloc_vreg();

        // Define and use all vregs
        for vreg in [v0, v1, v2, v3, v4, v5] {
            mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(vreg),
                imm: 42,
            });
        }
        for vreg in [v0, v1, v2, v3, v4, v5] {
            mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Virtual(vreg),
            });
        }

        // Pass 5 existing locals - spills should start at -48 (= -(5+1)*8)
        let (mir, num_spills, _) = RegAlloc::new(mir, 5).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 1);

        // Find the spill offset
        let spill_offset = mir
            .instructions()
            .iter()
            .find_map(|inst| match inst {
                X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    ..
                } => Some(*offset),
                _ => None,
            })
            .expect("Should have a spill store");

        // First spill with 5 existing locals should be at -48
        assert_eq!(spill_offset, -48);
    }

    // ========================================
    // Large stack frame tests
    // ========================================

    #[test]
    fn test_many_vregs_large_frame() {
        // Test a function with many virtual registers causing a large stack frame
        let mut mir = X86Mir::new();

        // Create 20 vregs (5 registers + 15 spills)
        let vregs: Vec<VReg> = (0..20).map(|_| mir.alloc_vreg()).collect();

        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(vreg),
                imm: i as i32,
            });
        }

        for &vreg in &vregs {
            mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 15);

        // Verify all virtual registers were replaced with physical
        for inst in mir.instructions() {
            match inst {
                X86Inst::MovRI32 { dst, .. } => {
                    assert!(dst.is_physical());
                }
                X86Inst::MovRR { dst, src } => {
                    assert!(dst.is_physical());
                    assert!(src.is_physical());
                }
                X86Inst::MovRM { dst, .. } => {
                    assert!(dst.is_physical());
                }
                X86Inst::MovMR { src, .. } => {
                    assert!(src.is_physical());
                }
                _ => {}
            }
        }
    }

    #[test]
    fn test_spill_with_many_locals() {
        // Test spilling with many existing local variables
        let mut mir = X86Mir::new();

        // 6 vregs forces 1 spill
        let vregs: Vec<VReg> = (0..6).map(|_| mir.alloc_vreg()).collect();

        for vreg in &vregs {
            mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(*vreg),
                imm: 1,
            });
        }
        for vreg in &vregs {
            mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Virtual(*vreg),
            });
        }

        // 50 existing locals - spill at -408 (= -(50+1)*8)
        let (mir, num_spills, _) = RegAlloc::new(mir, 50).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 1);

        let spill_offset = mir
            .instructions()
            .iter()
            .find_map(|inst| match inst {
                X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    ..
                } => Some(*offset),
                _ => None,
            })
            .expect("Should have a spill store");

        assert_eq!(spill_offset, -408);
    }

    #[test]
    fn test_binop_with_spilled_operands() {
        // Test that binary operations work correctly when operands are spilled
        let mut mir = X86Mir::new();

        // Create enough vregs to force spilling
        let vregs: Vec<VReg> = (0..8).map(|_| mir.alloc_vreg()).collect();

        // Initialize all
        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(vreg),
                imm: i as i32,
            });
        }

        // Add using potentially spilled operands
        mir.push(X86Inst::AddRR {
            dst: Operand::Virtual(vregs[0]),
            src: Operand::Virtual(vregs[7]),
        });

        // Use all to keep them live
        for &vreg in &vregs {
            mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert!(num_spills >= 3, "Should have some spills");

        // Verify the AddRR was properly rewritten
        let has_add = mir
            .instructions()
            .iter()
            .any(|inst| matches!(inst, X86Inst::AddRR { dst, src } if dst.is_physical() && src.is_physical()));
        assert!(has_add, "AddRR should be rewritten with physical registers");
    }
}
