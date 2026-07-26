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

use super::liveness;
#[cfg(test)]
use super::mir::VReg;
use super::mir::{Operand, Reg, X86Inst, X86Mir};
use crate::alloc_dst;
use crate::regalloc::{
    Allocation, AllocationContext, CoalesceCandidate, LivenessInfo, LoopInfo, RegAllocBackend,
    RegAllocDebugInfo, RegAllocDriver, RewriteBuffer,
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

/// Zero-sized adapter for target-specific analysis and instruction rewriting.
struct X86Backend;

/// Register allocator with shared assignment and rewrite orchestration.
pub struct RegAlloc {
    driver: RegAllocDriver<X86Backend>,
}

impl RegAlloc {
    pub fn new(mir: X86Mir, existing_locals: u32) -> Self {
        Self {
            driver: RegAllocDriver::new(mir, existing_locals),
        }
    }

    pub(crate) fn new_with_artifacts(
        mir: X86Mir,
        existing_locals: u32,
        capture_liveness: bool,
    ) -> Self {
        Self {
            driver: RegAllocDriver::new_with_artifacts(mir, existing_locals, capture_liveness),
        }
    }

    pub fn num_spills(&self) -> u32 {
        self.driver.num_spills()
    }

    pub fn allocate(self) -> CompileResult<X86Mir> {
        self.driver.allocate()
    }

    pub fn allocate_with_spills(self) -> CompileResult<(X86Mir, u32, Vec<Reg>)> {
        self.driver.allocate_with_spills()
    }

    pub fn allocate_with_debug(
        self,
    ) -> CompileResult<(X86Mir, u32, Vec<Reg>, RegAllocDebugInfo<Reg>)> {
        self.driver.allocate_with_debug()
    }

    pub(crate) fn allocate_with_artifacts(
        self,
        capture_regalloc: bool,
    ) -> CompileResult<(
        X86Mir,
        u32,
        Vec<Reg>,
        Option<crate::LivenessDebugInfo>,
        Option<RegAllocDebugInfo<Reg>>,
    )> {
        self.driver.allocate_with_artifacts(capture_regalloc)
    }

    fn rewrite_inst(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<X86Inst>,
        inst: X86Inst,
    ) -> CompileResult<()> {
        match inst {
            X86Inst::MovRI32 { dst, imm } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::MovRI32 { dst: dst_op, imm });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::MovRI64 { dst, imm } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::MovRI64 { dst: dst_op, imm });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::MovRR { dst, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                let dst_alloc = Self::get_allocation(context, dst);

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
                        mir.push_after(X86Inst::MovMR {
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
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::MovRM { dst: dst_op, base, offset });
                    },
                    store |spill_offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset: spill_offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::MovMR { base, offset, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                mir.push(X86Inst::MovMR {
                    base,
                    offset,
                    src: src_op,
                });
            }

            X86Inst::Movzx8RM { dst, base, offset } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movzx8RM { dst: dst_op, base, offset });
                    },
                    store |spill_offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset: spill_offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::MovMR8 { base, offset, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rdx)?;
                mir.push(X86Inst::MovMR8 {
                    base,
                    offset,
                    src: src_op,
                });
            }

            X86Inst::AddRR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::AddRR {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::AddRR64 { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::AddRR64 {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::AddRI { dst, imm } => {
                Self::emit_unop_imm(context, mir, dst, imm, |d, i| X86Inst::AddRI {
                    dst: d,
                    imm: i,
                });
            }

            X86Inst::SubRR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::SubRR {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::SubRR64 { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::SubRR64 {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::ImulRR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::ImulRR {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::ImulRR64 { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::ImulRR64 {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::Neg { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::Neg { dst: d });
            }

            X86Inst::Neg64 { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::Neg64 { dst: d });
            }

            X86Inst::XorRI { dst, imm } => {
                Self::emit_unop_imm(context, mir, dst, imm, |d, i| X86Inst::XorRI {
                    dst: d,
                    imm: i,
                });
            }

            X86Inst::AndRR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::AndRR {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::OrRR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::OrRR {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::XorRR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::XorRR {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::And64RR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::And64RR {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::Or64RR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::Or64RR {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::Xor64RR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| X86Inst::Xor64RR {
                    dst: d,
                    src: s,
                })?;
            }

            X86Inst::NotR { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::NotR { dst: d });
            }

            X86Inst::Not64R { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::Not64R { dst: d });
            }

            X86Inst::ShlRCl { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::ShlRCl { dst: d });
            }

            X86Inst::Shl32RCl { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::Shl32RCl { dst: d });
            }

            X86Inst::ShlRI { dst, imm } => {
                Self::emit_unop_imm_u8(context, mir, dst, imm, |d, i| X86Inst::ShlRI {
                    dst: d,
                    imm: i,
                });
            }

            X86Inst::Shl32RI { dst, imm } => {
                Self::emit_unop_imm_u8(context, mir, dst, imm, |d, i| X86Inst::Shl32RI {
                    dst: d,
                    imm: i,
                });
            }

            X86Inst::ShrRCl { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::ShrRCl { dst: d });
            }

            X86Inst::Shr32RCl { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::Shr32RCl { dst: d });
            }

            X86Inst::ShrRI { dst, imm } => {
                Self::emit_unop_imm_u8(context, mir, dst, imm, |d, i| X86Inst::ShrRI {
                    dst: d,
                    imm: i,
                });
            }

            X86Inst::Shr32RI { dst, imm } => {
                Self::emit_unop_imm_u8(context, mir, dst, imm, |d, i| X86Inst::Shr32RI {
                    dst: d,
                    imm: i,
                });
            }

            X86Inst::SarRCl { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::SarRCl { dst: d });
            }

            X86Inst::Sar32RCl { dst } => {
                Self::emit_unop(context, mir, dst, |d| X86Inst::Sar32RCl { dst: d });
            }

            X86Inst::SarRI { dst, imm } => {
                Self::emit_unop_imm_u8(context, mir, dst, imm, |d, i| X86Inst::SarRI {
                    dst: d,
                    imm: i,
                });
            }

            X86Inst::Sar32RI { dst, imm } => {
                Self::emit_unop_imm_u8(context, mir, dst, imm, |d, i| X86Inst::Sar32RI {
                    dst: d,
                    imm: i,
                });
            }

            X86Inst::IdivR { src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::R10)?;
                mir.push(X86Inst::IdivR { src: src_op });
            }

            X86Inst::DivR { src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::R10)?;
                mir.push(X86Inst::DivR { src: src_op });
            }

            X86Inst::Idiv64R { src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::R10)?;
                mir.push(X86Inst::Idiv64R { src: src_op });
            }

            X86Inst::Div64R { src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::R10)?;
                mir.push(X86Inst::Div64R { src: src_op });
            }

            X86Inst::MulR { src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::R10)?;
                mir.push(X86Inst::MulR { src: src_op });
            }

            X86Inst::Mul64R { src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::R10)?;
                mir.push(X86Inst::Mul64R { src: src_op });
            }

            X86Inst::TestRR { src1, src2 } => {
                let src1_op = Self::load_operand(context, mir, src1, Reg::Rax)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::R10)?;
                mir.push(X86Inst::TestRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::Test64RR { src1, src2 } => {
                let src1_op = Self::load_operand(context, mir, src1, Reg::Rax)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::R10)?;
                mir.push(X86Inst::Test64RR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::CmpRR { src1, src2 } => {
                let src1_op = Self::load_operand(context, mir, src1, Reg::Rax)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::R10)?;
                mir.push(X86Inst::CmpRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::Cmp64RR { src1, src2 } => {
                let src1_op = Self::load_operand(context, mir, src1, Reg::Rax)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::R10)?;
                mir.push(X86Inst::Cmp64RR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            X86Inst::CmpRI { src, imm } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                mir.push(X86Inst::CmpRI { src: src_op, imm });
            }

            X86Inst::Cmp64RI { src, imm } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                mir.push(X86Inst::Cmp64RI { src: src_op, imm });
            }

            X86Inst::Sete { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Sete { dst: d });
            }

            X86Inst::Setne { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Setne { dst: d });
            }

            X86Inst::Setl { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Setl { dst: d });
            }

            X86Inst::Setg { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Setg { dst: d });
            }

            X86Inst::Setle { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Setle { dst: d });
            }

            X86Inst::Setge { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Setge { dst: d });
            }

            X86Inst::Setb { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Setb { dst: d });
            }

            X86Inst::Seta { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Seta { dst: d });
            }

            X86Inst::Setbe { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Setbe { dst: d });
            }

            X86Inst::Setae { dst } => {
                Self::emit_setcc(context, mir, dst, |d| X86Inst::Setae { dst: d });
            }

            X86Inst::Movzx { dst, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movzx { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movsx8To64 { dst, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movsx8To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movsx16To64 { dst, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movsx16To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movsx32To64 { dst, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movsx32To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movzx8To64 { dst, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movzx8To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Movzx16To64 { dst, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Movzx16To64 { dst: dst_op, src: src_op });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Pop { dst } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Pop { dst: dst_op });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Push { src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;
                mir.push(X86Inst::Push { src: src_op });
            }

            X86Inst::Lea { dst, base, disp } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::Lea { dst: dst_op, base, disp });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::Shl { dst, count } => {
                // SHL needs count in RCX
                let count_op = Self::load_operand(context, mir, count, Reg::Rcx)?;
                if count_op != Operand::Physical(Reg::Rcx) {
                    mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rcx),
                        src: count_op,
                    });
                }

                match Self::get_allocation(context, dst) {
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
                        mir.push_after(X86Inst::MovMR {
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
                // Lifecycle boundary: Mov*Indexed are pre-regalloc pseudos
                // whose address base is still virtual. After this pass, emit
                // only sees concrete memory operations with physical bases.
                // Load base vreg into scratch register
                let base_op = Operand::Virtual(base);
                let base_reg = Self::load_operand(context, mir, base_op, Reg::Rax)?;
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => Reg::Rax,
                };

                match Self::get_allocation(context, dst) {
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
                        mir.push_after(X86Inst::MovMR {
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
                // Lifecycle boundary: see MovRMIndexed above.
                let src_op = Self::load_operand(context, mir, src, Reg::Rdx)?;
                let base_op = Operand::Virtual(base);
                let base_reg = Self::load_operand(context, mir, base_op, Reg::Rax)?;
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

            X86Inst::NarrowLoadIndexed {
                dst,
                base,
                offset,
                width,
                signed,
            } => {
                let base_reg = Self::load_operand(context, mir, Operand::Virtual(base), Reg::Rax)?;
                let base_phys = base_reg.as_physical();
                match Self::get_allocation(context, dst) {
                    Some(Allocation::Register(reg)) => mir.push(X86Inst::NarrowLoadRM {
                        dst: Operand::Physical(reg),
                        base: base_phys,
                        offset,
                        width,
                        signed,
                    }),
                    Some(Allocation::Spill(spill_off)) => {
                        mir.push(X86Inst::NarrowLoadRM {
                            dst: Operand::Physical(Reg::Rdx),
                            base: base_phys,
                            offset,
                            width,
                            signed,
                        });
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset: spill_off,
                            src: Operand::Physical(Reg::Rdx),
                        });
                    }
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => mir.push(X86Inst::NarrowLoadRM {
                        dst,
                        base: base_phys,
                        offset,
                        width,
                        signed,
                    }),
                }
            }

            X86Inst::NarrowStoreIndexed {
                base,
                src,
                offset,
                width,
            } => {
                let src_op = Self::load_operand(context, mir, src, Reg::Rdx)?;
                let base_reg = Self::load_operand(context, mir, Operand::Virtual(base), Reg::Rax)?;
                mir.push(X86Inst::NarrowStoreMR {
                    base: base_reg.as_physical(),
                    src: src_op,
                    offset,
                    width,
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
                let base_op = Self::load_operand(context, mir, base, Reg::Rdx)?;
                // Load index into a register (use Rcx as scratch)
                // Note: RSP cannot be used as index in SIB encoding
                let index_op = Self::load_operand(context, mir, index, Reg::Rcx)?;

                match Self::get_allocation(context, dst) {
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
                        mir.push_after(X86Inst::MovMR {
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
                let base_op = Self::load_operand(context, mir, base, Reg::Rdx)?;
                // Load index into a register (use Rcx as scratch)
                let index_op = Self::load_operand(context, mir, index, Reg::Rcx)?;
                // Load src value
                let src_op = Self::load_operand(context, mir, src, Reg::Rax)?;

                mir.push(X86Inst::MovMRSib {
                    base: base_op,
                    index: index_op,
                    scale,
                    disp,
                    src: src_op,
                });
            }

            X86Inst::StringConstPtr { dst, string_id } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::StringConstPtr { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::StringConstLen { dst, string_id } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::StringConstLen { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            X86Inst::StringConstCap { dst, string_id } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::Rax =>
                    emit |dst_op| {
                        mir.push(X86Inst::StringConstCap { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push_after(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Physical(Reg::Rax),
                        });
                    },
                );
            }

            // Instructions without register operands pass through unchanged
            X86Inst::Cdq => mir.push(X86Inst::Cdq),
            X86Inst::Cqo => mir.push(X86Inst::Cqo),
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
            X86Inst::Ud2 => mir.push(X86Inst::Ud2),
            // The physical-base narrow forms are produced by this pass from the
            // indexed pseudos above with already-allocated operands; they never
            // appear in the pre-allocation input, so pass them through unchanged.
            X86Inst::NarrowLoadRM {
                dst,
                base,
                offset,
                width,
                signed,
            } => mir.push(X86Inst::NarrowLoadRM {
                dst,
                base,
                offset,
                width,
                signed,
            }),
            X86Inst::NarrowStoreMR {
                base,
                src,
                offset,
                width,
            } => mir.push(X86Inst::NarrowStoreMR {
                base,
                src,
                offset,
                width,
            }),
        }
        Ok(())
    }

    /// Get the allocation for an operand (returns None for physical registers).
    ///
    /// For coalesced vregs, this looks up the allocation of the representative vreg.
    fn get_allocation(
        context: &AllocationContext<'_, Reg>,
        operand: Operand,
    ) -> Option<Allocation<Reg>> {
        match operand {
            Operand::Virtual(vreg) => context.allocation(vreg),
            Operand::Physical(_) => None,
        }
    }

    /// Load an operand into a physical register, inserting a load if spilled
    /// or rematerializing if marked for rematerialization.
    /// Returns the operand to use (either the allocated register or the scratch register).
    ///
    /// For coalesced vregs, this loads the allocation of the representative vreg.
    fn load_operand(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<X86Inst>,
        operand: Operand,
        scratch: Reg,
    ) -> CompileResult<Operand> {
        match operand {
            Operand::Virtual(vreg) => {
                match context.allocation(vreg) {
                    Some(Allocation::Register(reg)) => Ok(Operand::Physical(reg)),
                    Some(Allocation::Spill(offset)) => {
                        mir.push_before(X86Inst::MovRM {
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
                                mir.push_before(X86Inst::MovRI32 {
                                    dst: Operand::Physical(scratch),
                                    imm,
                                });
                            }
                            RematerializeOp::Const64(imm) => {
                                mir.push_before(X86Inst::MovRI64 {
                                    dst: Operand::Physical(scratch),
                                    imm,
                                });
                            }
                            RematerializeOp::StringPtr(string_id) => {
                                mir.push_before(X86Inst::StringConstPtr {
                                    dst: Operand::Physical(scratch),
                                    string_id,
                                });
                            }
                            RematerializeOp::StringLen(string_id) => {
                                mir.push_before(X86Inst::StringConstLen {
                                    dst: Operand::Physical(scratch),
                                    string_id,
                                });
                            }
                            RematerializeOp::StringCap(string_id) => {
                                mir.push_before(X86Inst::StringConstCap {
                                    dst: Operand::Physical(scratch),
                                    string_id,
                                });
                            }
                        }
                        Ok(Operand::Physical(scratch))
                    }
                    None => Err(CompileError::without_span(ErrorKind::LinkError(format!(
                        "internal codegen error: virtual register {} was not allocated",
                        vreg.index()
                    )))),
                }
            }
            Operand::Physical(reg) => Ok(Operand::Physical(reg)),
        }
    }

    /// Emit a binary operation (dst = dst op src).
    fn emit_binop<F>(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<X86Inst>,
        dst: Operand,
        src: Operand,
        make_inst: F,
    ) -> CompileResult<()>
    where
        F: FnOnce(Operand, Operand) -> X86Inst,
    {
        // Load src first (use R10 as scratch to avoid clobbering RAX)
        let src_op = Self::load_operand(context, mir, src, Reg::R10)?;

        match Self::get_allocation(context, dst) {
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
                mir.push_after(X86Inst::MovMR {
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
    fn emit_unop<F>(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<X86Inst>,
        dst: Operand,
        make_inst: F,
    ) where
        F: FnOnce(Operand) -> X86Inst,
    {
        match Self::get_allocation(context, dst) {
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
                mir.push_after(X86Inst::MovMR {
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
    fn emit_unop_imm<F>(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<X86Inst>,
        dst: Operand,
        imm: i32,
        make_inst: F,
    ) where
        F: FnOnce(Operand, i32) -> X86Inst,
    {
        match Self::get_allocation(context, dst) {
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
                mir.push_after(X86Inst::MovMR {
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
    fn emit_unop_imm_u8<F>(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<X86Inst>,
        dst: Operand,
        imm: u8,
        make_inst: F,
    ) where
        F: FnOnce(Operand, u8) -> X86Inst,
    {
        match Self::get_allocation(context, dst) {
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
                mir.push_after(X86Inst::MovMR {
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
    fn emit_setcc<F>(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<X86Inst>,
        dst: Operand,
        make_inst: F,
    ) where
        F: FnOnce(Operand) -> X86Inst,
    {
        match Self::get_allocation(context, dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg)));
            }
            Some(Allocation::Spill(offset)) => {
                // setcc writes a byte, so we use RAX and store
                mir.push(make_inst(Operand::Physical(Reg::Rax)));
                mir.push_after(X86Inst::MovMR {
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

impl RegAllocBackend for X86Backend {
    type Mir = X86Mir;
    type Inst = X86Inst;
    type Reg = Reg;

    fn vreg_count(mir: &Self::Mir) -> u32 {
        mir.vreg_count()
    }

    fn instructions(mir: &Self::Mir) -> &[Self::Inst] {
        mir.instructions()
    }

    fn analyze(mir: &Self::Mir) -> LivenessInfo<Self::Reg> {
        liveness::analyze(mir)
    }

    fn analyze_with_debug(
        mir: &Self::Mir,
    ) -> (LivenessInfo<Self::Reg>, crate::regalloc::LivenessDebugInfo) {
        liveness::analyze_with_debug(mir)
    }

    fn analyze_loops(mir: &Self::Mir) -> LoopInfo {
        liveness::analyze_loops(mir)
    }

    fn coalesce_candidates(instructions: &[Self::Inst]) -> Vec<CoalesceCandidate> {
        instructions
            .iter()
            .enumerate()
            .filter_map(|(idx, inst)| match inst {
                X86Inst::MovRR {
                    dst: Operand::Virtual(dst),
                    src: Operand::Virtual(src),
                } => Some(CoalesceCandidate {
                    inst_idx: idx,
                    dst: *dst,
                    src: *src,
                }),
                _ => None,
            })
            .collect()
    }

    fn allocatable_regs() -> &'static [Self::Reg] {
        ALLOCATABLE_REGS
    }

    fn new_mir() -> Self::Mir {
        X86Mir::new()
    }

    fn take_symbols(mir: &mut Self::Mir) -> Vec<String> {
        mir.take_symbols()
    }

    fn set_symbols(mir: &mut Self::Mir, symbols: Vec<String>) {
        mir.set_symbols(symbols);
    }

    fn into_instructions(mir: Self::Mir) -> Vec<Self::Inst> {
        mir.into_instructions()
    }

    fn push(mir: &mut Self::Mir, inst: Self::Inst) {
        mir.push(inst);
    }

    fn rewrite_inst(
        context: &AllocationContext<'_, Self::Reg>,
        buffer: &mut RewriteBuffer<Self::Inst>,
        inst: Self::Inst,
    ) -> CompileResult<()> {
        RegAlloc::rewrite_inst(context, buffer, inst)
    }
}

#[cfg(test)]
mod tests {
    use super::liveness;
    use super::{Operand, Reg, RegAlloc, VReg, X86Inst, X86Mir};

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

    #[test]
    fn test_indexed_memory_pseudos_lowered_before_emit() {
        let mut mir = X86Mir::new();
        let base = mir.alloc_vreg();
        let src = mir.alloc_vreg();
        let loaded = mir.alloc_vreg();

        mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(base),
            imm: 1024,
        });
        mir.push(X86Inst::MovRI64 {
            dst: Operand::Virtual(src),
            imm: 7,
        });
        mir.push(X86Inst::MovRMIndexed {
            dst: Operand::Virtual(loaded),
            base,
            offset: 16,
        });
        mir.push(X86Inst::MovMRIndexed {
            base,
            offset: 24,
            src: Operand::Virtual(src),
        });
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rdi),
            src: Operand::Virtual(loaded),
        });

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();

        assert!(
            mir.instructions().iter().all(|inst| !matches!(
                inst,
                X86Inst::MovRMIndexed { .. } | X86Inst::MovMRIndexed { .. }
            )),
            "regalloc must eliminate indexed memory pseudos before emit"
        );
        assert!(
            mir.instructions().iter().any(|inst| matches!(
                inst,
                X86Inst::MovRM {
                    dst: Operand::Physical(_),
                    base,
                    offset: 16,
                } if *base != Reg::Rbp
            )),
            "MovRMIndexed should lower to MovRM with a physical address base"
        );
        assert!(
            mir.instructions().iter().any(|inst| matches!(
                inst,
                X86Inst::MovMR {
                    base,
                    offset: 24,
                    src: Operand::Physical(_),
                } if *base != Reg::Rbp
            )),
            "MovMRIndexed should lower to MovMR with a physical address base"
        );
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

        let instructions = mir.instructions();
        let load_index = instructions
            .iter()
            .position(|inst| matches!(inst, X86Inst::MovRM { base: Reg::Rbp, .. }))
            .expect("spill load should be present");
        assert!(matches!(
            instructions.get(load_index + 1),
            Some(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Physical(_),
            })
        ));

        let store_index = instructions
            .iter()
            .position(|inst| matches!(inst, X86Inst::MovMR { base: Reg::Rbp, .. }))
            .expect("spill store should be present");
        assert!(matches!(
            store_index
                .checked_sub(1)
                .and_then(|index| instructions.get(index)),
            Some(X86Inst::MovRI32 { .. })
        ));
    }

    #[test]
    fn test_idiv_preserves_fixed_register_constraints() {
        let mut mir = X86Mir::new();
        let divisor = mir.alloc_vreg();

        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(divisor),
            imm: 3,
        });
        mir.push(X86Inst::Cdq);
        mir.push(X86Inst::IdivR {
            src: Operand::Virtual(divisor),
        });

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();
        let src = mir
            .instructions()
            .iter()
            .find_map(|inst| match inst {
                X86Inst::IdivR {
                    src: Operand::Physical(reg),
                } => Some(*reg),
                _ => None,
            })
            .expect("idiv should retain a physical divisor");

        assert!(!matches!(src, Reg::Rax | Reg::Rdx));
    }

    #[test]
    fn test_call_survival_and_symbol_reconstruction() {
        let mut mir = X86Mir::new();
        let value = mir.alloc_vreg();
        let symbol = mir.intern_symbol("callee");

        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(value),
            imm: 42,
        });
        mir.push(X86Inst::CallRel { symbol_id: symbol });
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rdi),
            src: Operand::Virtual(value),
        });

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();
        let value_reg = mir
            .instructions()
            .iter()
            .find_map(|inst| match inst {
                X86Inst::MovRI32 {
                    dst: Operand::Physical(reg),
                    ..
                } => Some(*reg),
                _ => None,
            })
            .expect("value definition should be physical");

        assert!(matches!(
            value_reg,
            Reg::Rbx | Reg::R12 | Reg::R13 | Reg::R14 | Reg::R15
        ));
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::CallRel { symbol_id } if *symbol_id == symbol))
        );
        assert!(mir.instructions().iter().any(|inst| matches!(
            inst,
            X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Physical(reg),
            } if *reg == value_reg
        )));
        assert_eq!(mir.get_symbol(symbol), "callee");
    }

    #[test]
    fn test_loop_pressure_uses_loop_aware_spilling() {
        let mut mir = X86Mir::new();
        let vregs: Vec<VReg> = (0..6).map(|_| mir.alloc_vreg()).collect();
        let loop_label = mir.alloc_label();

        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(vreg),
                imm: i as i32,
            });
        }
        mir.push(X86Inst::Label { id: loop_label });
        mir.push(X86Inst::AddRI {
            dst: Operand::Virtual(vregs[0]),
            imm: 1,
        });
        mir.push(X86Inst::CmpRI {
            src: Operand::Virtual(vregs[0]),
            imm: 10,
        });
        mir.push(X86Inst::Jnz { label: loop_label });
        for &vreg in &vregs {
            mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Virtual(vreg),
            });
        }

        let loop_info = liveness::analyze_loops(&mir);
        assert!(
            loop_info.depth(7) > 0,
            "loop body should have nonzero depth"
        );

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();
        assert!(num_spills > 0);
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::MovMR { base: Reg::Rbp, .. }))
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Label { id } if *id == loop_label))
        );
    }

    #[test]
    fn test_allocate_with_debug_is_deterministic() {
        let make_mir = || {
            let mut mir = X86Mir::new();
            let vregs: Vec<VReg> = (0..6).map(|_| mir.alloc_vreg()).collect();
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
            mir
        };

        let (_, spills_a, regs_a, debug_a) =
            RegAlloc::new(make_mir(), 0).allocate_with_debug().unwrap();
        let (_, spills_b, regs_b, debug_b) =
            RegAlloc::new(make_mir(), 0).allocate_with_debug().unwrap();

        assert_eq!(spills_a, spills_b);
        assert_eq!(regs_a, regs_b);
        assert_eq!(debug_a.to_string(), debug_b.to_string());
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

    #[test]
    fn test_spilled_binary_destination_has_ordered_rewrite() {
        let mut mir = X86Mir::new();
        let vregs: Vec<VReg> = (0..7).map(|_| mir.alloc_vreg()).collect();

        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(X86Inst::MovRI32 {
                dst: Operand::Virtual(vreg),
                imm: i as i32,
            });
        }
        mir.push(X86Inst::AddRR {
            dst: Operand::Virtual(vregs[6]),
            src: Operand::Virtual(vregs[5]),
        });
        for &vreg in &vregs[1..] {
            mir.push(X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rdi),
                src: Operand::Virtual(vreg),
            });
        }

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();
        let add_index = mir
            .instructions()
            .iter()
            .position(|inst| matches!(inst, X86Inst::AddRR { .. }))
            .expect("rewritten AddRR should be present");
        assert!(matches!(
            add_index
                .checked_sub(1)
                .and_then(|index| mir.instructions().get(index)),
            Some(X86Inst::MovRM { base: Reg::Rbp, .. })
        ));
        assert!(matches!(
            mir.instructions().get(add_index + 1),
            Some(X86Inst::MovMR { base: Reg::Rbp, .. })
        ));
    }
}
