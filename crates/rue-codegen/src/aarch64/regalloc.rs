//! Register allocation for AArch64.
//!
//! This module allocates physical registers to virtual registers using
//! liveness analysis and linear scan allocation.
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
use super::mir::{Aarch64Inst, Aarch64Mir, Operand, Reg};
use crate::alloc_dst;
use crate::regalloc::{
    Allocation, AllocationContext, CoalesceCandidate, LivenessInfo, LoopInfo, RegAllocBackend,
    RegAllocDebugInfo, RegAllocDriver, RewriteBuffer,
};

/// Available registers for allocation.
///
/// We use callee-saved registers (X19-X28) for general allocation.
/// This ensures values survive across function calls.
///
/// We avoid:
/// - X0-X7: Argument/return registers
/// - X8: Indirect result location
/// - X9-X15: Caller-saved temporaries (X9-X12 are spill scratches; X15 is
///   reserved as the emitter's large-offset address scratch — never allocate)
/// - X16-X17: IP0, IP1 (linker scratch)
/// - X18: Platform register (reserved on macOS)
/// - X29 (FP): Frame pointer
/// - X30 (LR): Link register
/// - SP: Stack pointer
const ALLOCATABLE_REGS: &[Reg] = &[
    Reg::X19,
    Reg::X20,
    Reg::X21,
    Reg::X22,
    Reg::X23,
    Reg::X24,
    Reg::X25,
    Reg::X26,
    Reg::X27,
    Reg::X28,
];

/// Zero-sized adapter for target-specific analysis and instruction rewriting.
struct Aarch64Backend;

/// Register allocator with shared assignment and rewrite orchestration.
pub struct RegAlloc {
    driver: RegAllocDriver<Aarch64Backend>,
}

impl RegAlloc {
    pub fn new(mir: Aarch64Mir, existing_locals: u32) -> Self {
        Self {
            driver: RegAllocDriver::new(mir, existing_locals),
        }
    }

    pub(crate) fn new_with_artifacts(
        mir: Aarch64Mir,
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

    pub fn allocate(self) -> CompileResult<Aarch64Mir> {
        self.driver.allocate()
    }

    pub fn allocate_with_spills(self) -> CompileResult<(Aarch64Mir, u32, Vec<Reg>)> {
        self.driver.allocate_with_spills()
    }

    pub fn allocate_with_debug(
        self,
    ) -> CompileResult<(Aarch64Mir, u32, Vec<Reg>, RegAllocDebugInfo<Reg>)> {
        self.driver.allocate_with_debug()
    }

    pub(crate) fn allocate_with_artifacts(
        self,
        capture_regalloc: bool,
    ) -> CompileResult<(
        Aarch64Mir,
        u32,
        Vec<Reg>,
        Option<crate::LivenessDebugInfo>,
        Option<RegAllocDebugInfo<Reg>>,
    )> {
        self.driver.allocate_with_artifacts(capture_regalloc)
    }

    fn rewrite_inst(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<Aarch64Inst>,
        inst: Aarch64Inst,
    ) -> CompileResult<()> {
        match inst {
            Aarch64Inst::MovImm { dst, imm } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::MovImm { dst: dst_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::MovRR { dst, src } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X9)?;
                let dst_alloc = Self::get_allocation(context, dst);

                match dst_alloc {
                    Some(Allocation::Register(reg)) => {
                        mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Physical(reg),
                            src: src_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        if src_op != Operand::Physical(Reg::X9) {
                            mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(Reg::X9),
                                src: src_op,
                            });
                        }
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => {
                        mir.push(Aarch64Inst::MovRR { dst, src: src_op });
                    }
                }
            }

            Aarch64Inst::Ldr { dst, base, offset } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Ldr { dst: dst_op, base, offset });
                    },
                    store |spill_offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset: spill_offset,
                        });
                    },
                );
            }

            Aarch64Inst::Str { src, base, offset } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X9)?;
                mir.push(Aarch64Inst::Str {
                    src: src_op,
                    base,
                    offset,
                });
            }

            Aarch64Inst::AddRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::AddRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::AddsRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::AddsRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::AddsRR64 { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::AddsRR64 {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::AddImm { dst, src, imm } => {
                Self::emit_binop_imm(context, mir, dst, src, imm, |d, s, i| Aarch64Inst::AddImm {
                    dst: d,
                    src: s,
                    imm: i,
                })?;
            }

            Aarch64Inst::SubRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::SubRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::SubsRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::SubsRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::SubsRR64 { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::SubsRR64 {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::SubImm { dst, src, imm } => {
                Self::emit_binop_imm(context, mir, dst, src, imm, |d, s, i| Aarch64Inst::SubImm {
                    dst: d,
                    src: s,
                    imm: i,
                })?;
            }

            Aarch64Inst::MulRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::MulRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::SmullRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::SmullRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::UmullRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::UmullRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::SmulhRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::SmulhRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::UmulhRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::UmulhRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::Lsr64Imm { dst, src, imm } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Lsr64Imm {
                    dst: d,
                    src: s,
                    imm,
                })?;
            }

            Aarch64Inst::Asr64Imm { dst, src, imm } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Asr64Imm {
                    dst: d,
                    src: s,
                    imm,
                })?;
            }

            Aarch64Inst::SdivRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::SdivRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::UdivRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::UdivRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::Sdiv64RR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::Sdiv64RR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::Udiv64RR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::Udiv64RR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::Msub {
                dst,
                src1,
                src2,
                src3,
            } => {
                // Use X10, X11, X12 for sources to avoid conflict with X9 used for spilled dst.
                // X9 is reserved for the destination when it's spilled.
                let src1_op = Self::load_operand(context, mir, src1, Reg::X10)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::X11)?;
                let src3_op = Self::load_operand(context, mir, src3, Reg::X12)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Msub {
                            dst: dst_op,
                            src1: src1_op,
                            src2: src2_op,
                            src3: src3_op,
                        });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::Msub64 {
                dst,
                src1,
                src2,
                src3,
            } => {
                // Use X10, X11, X12 for sources to avoid conflict with X9 used for spilled dst.
                let src1_op = Self::load_operand(context, mir, src1, Reg::X10)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::X11)?;
                let src3_op = Self::load_operand(context, mir, src3, Reg::X12)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Msub64 {
                            dst: dst_op,
                            src1: src1_op,
                            src2: src2_op,
                            src3: src3_op,
                        });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::Neg { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Neg {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::Negs { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Negs {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::Negs32 { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Negs32 {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::AndRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::AndRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::OrrRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::OrrRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::EorRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::EorRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::EorImm { dst, src, imm } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X10)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::EorImm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::MvnRR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::MvnRR {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::Mvn32RR { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Mvn32RR {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::LslRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::LslRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::Lsl32RR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::Lsl32RR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::LsrRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::LsrRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::Lsr32RR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::Lsr32RR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::AsrRR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::AsrRR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::Asr32RR { dst, src1, src2 } => {
                Self::emit_ternop(context, mir, dst, src1, src2, |d, s1, s2| {
                    Aarch64Inst::Asr32RR {
                        dst: d,
                        src1: s1,
                        src2: s2,
                    }
                })?;
            }

            Aarch64Inst::CmpRR { src1, src2 } => {
                let src1_op = Self::load_operand(context, mir, src1, Reg::X9)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::X10)?;
                mir.push(Aarch64Inst::CmpRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            Aarch64Inst::Cmp64RR { src1, src2 } => {
                let src1_op = Self::load_operand(context, mir, src1, Reg::X9)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::X10)?;
                mir.push(Aarch64Inst::Cmp64RR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            Aarch64Inst::CmpImm { src, imm } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X9)?;
                mir.push(Aarch64Inst::CmpImm { src: src_op, imm });
            }

            Aarch64Inst::Cbz { src, label } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X9)?;
                mir.push(Aarch64Inst::Cbz { src: src_op, label });
            }

            Aarch64Inst::Cbnz { src, label } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X9)?;
                mir.push(Aarch64Inst::Cbnz { src: src_op, label });
            }

            Aarch64Inst::Cset { dst, cond } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Cset { dst: dst_op, cond });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::TstRR { src1, src2 } => {
                let src1_op = Self::load_operand(context, mir, src1, Reg::X9)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::X10)?;
                mir.push(Aarch64Inst::TstRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            Aarch64Inst::Sxtb { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Sxtb {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::Sxth { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Sxth {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::Sxtw { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Sxtw {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::Uxtb { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Uxtb {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::Uxth { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Uxth {
                    dst: d,
                    src: s,
                })?;
            }

            Aarch64Inst::StpPre { src1, src2, offset } => {
                let src1_op = Self::load_operand(context, mir, src1, Reg::X9)?;
                let src2_op = Self::load_operand(context, mir, src2, Reg::X10)?;
                mir.push(Aarch64Inst::StpPre {
                    src1: src1_op,
                    src2: src2_op,
                    offset,
                });
            }

            Aarch64Inst::LdpPost { dst1, dst2, offset } => {
                // LDP only defines, doesn't read vregs
                let dst1_phys = match Self::get_allocation(context, dst1) {
                    Some(Allocation::Register(reg)) => Operand::Physical(reg),
                    Some(Allocation::Spill(_)) => Operand::Physical(Reg::X9),
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => dst1,
                };
                let dst2_phys = match Self::get_allocation(context, dst2) {
                    Some(Allocation::Register(reg)) => Operand::Physical(reg),
                    Some(Allocation::Spill(_)) => Operand::Physical(Reg::X10),
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => dst2,
                };
                mir.push(Aarch64Inst::LdpPost {
                    dst1: dst1_phys,
                    dst2: dst2_phys,
                    offset,
                });
                // Handle spills
                if let Some(Allocation::Spill(off)) = Self::get_allocation(context, dst1) {
                    mir.push_after(Aarch64Inst::Str {
                        src: Operand::Physical(Reg::X9),
                        base: Reg::Fp,
                        offset: off,
                    });
                }
                if let Some(Allocation::Spill(off)) = Self::get_allocation(context, dst2) {
                    mir.push_after(Aarch64Inst::Str {
                        src: Operand::Physical(Reg::X10),
                        base: Reg::Fp,
                        offset: off,
                    });
                }
            }

            Aarch64Inst::LdrIndexed { dst, base } => {
                // Load base vreg into scratch, then emit load with the result allocation
                let base_op = Operand::Virtual(base);
                let base_reg = Self::load_operand(context, mir, base_op, Reg::X9)?;
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => Reg::X9,
                };

                match Self::get_allocation(context, dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Physical(reg),
                            base: base_phys,
                            offset: 0,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Physical(Reg::X10),
                            base: base_phys,
                            offset: 0,
                        });
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X10),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => {
                        mir.push(Aarch64Inst::Ldr {
                            dst,
                            base: base_phys,
                            offset: 0,
                        });
                    }
                }
            }

            Aarch64Inst::StrIndexed { src, base } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X9)?;
                let base_vreg_op = Operand::Virtual(base);
                let base_reg = Self::load_operand(context, mir, base_vreg_op, Reg::X10)?;
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => Reg::X10,
                };
                mir.push(Aarch64Inst::Str {
                    src: src_op,
                    base: base_phys,
                    offset: 0,
                });
            }

            Aarch64Inst::NarrowLoadIndexed {
                dst,
                base,
                offset: addr_offset,
                width,
                signed,
            } => {
                let base_reg = Self::load_operand(context, mir, Operand::Virtual(base), Reg::X9)?;
                let base_phys = base_reg.as_physical();
                match Self::get_allocation(context, dst) {
                    Some(Allocation::Register(reg)) => mir.push(Aarch64Inst::NarrowLoad {
                        dst: Operand::Physical(reg),
                        base: base_phys,
                        offset: addr_offset,
                        width,
                        signed,
                    }),
                    Some(Allocation::Spill(offset)) => {
                        mir.push(Aarch64Inst::NarrowLoad {
                            dst: Operand::Physical(Reg::X10),
                            base: base_phys,
                            offset: addr_offset,
                            width,
                            signed,
                        });
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X10),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => mir.push(Aarch64Inst::NarrowLoad {
                        dst,
                        base: base_phys,
                        offset: addr_offset,
                        width,
                        signed,
                    }),
                }
            }

            Aarch64Inst::NarrowStoreIndexed {
                src,
                base,
                offset,
                width,
            } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X9)?;
                let base_reg = Self::load_operand(context, mir, Operand::Virtual(base), Reg::X10)?;
                mir.push(Aarch64Inst::NarrowStore {
                    src: src_op,
                    base: base_reg.as_physical(),
                    offset,
                    width,
                });
            }

            Aarch64Inst::LdrIndexedOffset { dst, base, offset } => {
                // Load base vreg into scratch, then emit load with offset
                let base_op = Operand::Virtual(base);
                let base_reg = Self::load_operand(context, mir, base_op, Reg::X9)?;
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => Reg::X9,
                };

                match Self::get_allocation(context, dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Physical(reg),
                            base: base_phys,
                            offset,
                        });
                    }
                    Some(Allocation::Spill(spill_offset)) => {
                        mir.push(Aarch64Inst::Ldr {
                            dst: Operand::Physical(Reg::X10),
                            base: base_phys,
                            offset,
                        });
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X10),
                            base: Reg::Fp,
                            offset: spill_offset,
                        });
                    }
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => {
                        mir.push(Aarch64Inst::Ldr {
                            dst,
                            base: base_phys,
                            offset,
                        });
                    }
                }
            }

            Aarch64Inst::StrIndexedOffset { src, base, offset } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X9)?;
                let base_vreg_op = Operand::Virtual(base);
                let base_reg = Self::load_operand(context, mir, base_vreg_op, Reg::X10)?;
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => Reg::X10,
                };
                mir.push(Aarch64Inst::Str {
                    src: src_op,
                    base: base_phys,
                    offset,
                });
            }

            Aarch64Inst::LslImm { dst, src, imm } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X10)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::LslImm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::Lsl32Imm { dst, src, imm } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X10)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Lsl32Imm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::Lsr32Imm { dst, src, imm } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X10)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Lsr32Imm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::Asr32Imm { dst, src, imm } => {
                let src_op = Self::load_operand(context, mir, src, Reg::X10)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Asr32Imm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::StringConstPtr { dst, string_id } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::StringConstPtr { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::StringConstLen { dst, string_id } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::StringConstLen { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::StringConstCap { dst, string_id } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, Reg::X9 =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::StringConstCap { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            // Pass-through instructions
            Aarch64Inst::B { label } => mir.push(Aarch64Inst::B { label }),
            Aarch64Inst::BCond { cond, label } => mir.push(Aarch64Inst::BCond { cond, label }),
            Aarch64Inst::Bvs { label } => mir.push(Aarch64Inst::Bvs { label }),
            Aarch64Inst::Bvc { label } => mir.push(Aarch64Inst::Bvc { label }),
            Aarch64Inst::Label { id } => mir.push(Aarch64Inst::Label { id }),
            Aarch64Inst::Bl { symbol_id } => mir.push(Aarch64Inst::Bl { symbol_id }),
            Aarch64Inst::Ret => mir.push(Aarch64Inst::Ret),
            Aarch64Inst::Brk => mir.push(Aarch64Inst::Brk),
            Aarch64Inst::Svc { imm } => mir.push(Aarch64Inst::Svc { imm }),
            // The physical-base narrow forms are produced by this pass from the
            // indexed pseudos above with already-allocated operands; they never
            // appear in the pre-allocation input, so pass them through unchanged.
            Aarch64Inst::NarrowLoad {
                dst,
                base,
                offset,
                width,
                signed,
            } => mir.push(Aarch64Inst::NarrowLoad {
                dst,
                base,
                offset,
                width,
                signed,
            }),
            Aarch64Inst::NarrowStore {
                src,
                base,
                offset,
                width,
            } => mir.push(Aarch64Inst::NarrowStore {
                src,
                base,
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
        mir: &mut RewriteBuffer<Aarch64Inst>,
        operand: Operand,
        scratch: Reg,
    ) -> CompileResult<Operand> {
        match operand {
            Operand::Virtual(vreg) => {
                match context.allocation(vreg) {
                    Some(Allocation::Register(reg)) => Ok(Operand::Physical(reg)),
                    Some(Allocation::Spill(offset)) => {
                        mir.push_before(Aarch64Inst::Ldr {
                            dst: Operand::Physical(scratch),
                            base: Reg::Fp,
                            offset,
                        });
                        Ok(Operand::Physical(scratch))
                    }
                    Some(Allocation::Rematerialize(remat_op)) => {
                        // Rematerialize the value instead of loading from stack
                        use crate::regalloc::RematerializeOp;
                        match remat_op {
                            RematerializeOp::Const32(imm) => {
                                mir.push_before(Aarch64Inst::MovImm {
                                    dst: Operand::Physical(scratch),
                                    imm: imm as i64,
                                });
                            }
                            RematerializeOp::Const64(imm) => {
                                mir.push_before(Aarch64Inst::MovImm {
                                    dst: Operand::Physical(scratch),
                                    imm,
                                });
                            }
                            RematerializeOp::StringPtr(string_id) => {
                                mir.push_before(Aarch64Inst::StringConstPtr {
                                    dst: Operand::Physical(scratch),
                                    string_id,
                                });
                            }
                            RematerializeOp::StringLen(string_id) => {
                                mir.push_before(Aarch64Inst::StringConstLen {
                                    dst: Operand::Physical(scratch),
                                    string_id,
                                });
                            }
                            RematerializeOp::StringCap(string_id) => {
                                mir.push_before(Aarch64Inst::StringConstCap {
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

    fn emit_binop<F>(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<Aarch64Inst>,
        dst: Operand,
        src: Operand,
        make_inst: F,
    ) -> CompileResult<()>
    where
        F: FnOnce(Operand, Operand) -> Aarch64Inst,
    {
        let src_op = Self::load_operand(context, mir, src, Reg::X10)?;
        match Self::get_allocation(context, dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src_op));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(make_inst(Operand::Physical(Reg::X9), src_op));
                mir.push_after(Aarch64Inst::Str {
                    src: Operand::Physical(Reg::X9),
                    base: Reg::Fp,
                    offset,
                });
            }
            Some(Allocation::Rematerialize(_)) => {
                unreachable!("destination cannot be rematerializable")
            }
            None => {
                mir.push(make_inst(dst, src_op));
            }
        }
        Ok(())
    }

    fn emit_ternop<F>(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<Aarch64Inst>,
        dst: Operand,
        src1: Operand,
        src2: Operand,
        make_inst: F,
    ) -> CompileResult<()>
    where
        F: FnOnce(Operand, Operand, Operand) -> Aarch64Inst,
    {
        let src1_op = Self::load_operand(context, mir, src1, Reg::X10)?;
        let src2_op = Self::load_operand(context, mir, src2, Reg::X11)?;
        match Self::get_allocation(context, dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src1_op, src2_op));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(make_inst(Operand::Physical(Reg::X9), src1_op, src2_op));
                mir.push_after(Aarch64Inst::Str {
                    src: Operand::Physical(Reg::X9),
                    base: Reg::Fp,
                    offset,
                });
            }
            Some(Allocation::Rematerialize(_)) => {
                unreachable!("destination cannot be rematerializable")
            }
            None => {
                mir.push(make_inst(dst, src1_op, src2_op));
            }
        }
        Ok(())
    }

    fn emit_binop_imm<F>(
        context: &AllocationContext<'_, Reg>,
        mir: &mut RewriteBuffer<Aarch64Inst>,
        dst: Operand,
        src: Operand,
        imm: i32,
        make_inst: F,
    ) -> CompileResult<()>
    where
        F: FnOnce(Operand, Operand, i32) -> Aarch64Inst,
    {
        let src_op = Self::load_operand(context, mir, src, Reg::X10)?;
        match Self::get_allocation(context, dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src_op, imm));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(make_inst(Operand::Physical(Reg::X9), src_op, imm));
                mir.push_after(Aarch64Inst::Str {
                    src: Operand::Physical(Reg::X9),
                    base: Reg::Fp,
                    offset,
                });
            }
            Some(Allocation::Rematerialize(_)) => {
                unreachable!("destination cannot be rematerializable")
            }
            None => {
                mir.push(make_inst(dst, src_op, imm));
            }
        }
        Ok(())
    }
}

impl RegAllocBackend for Aarch64Backend {
    type Mir = Aarch64Mir;
    type Inst = Aarch64Inst;
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
                Aarch64Inst::MovRR {
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
        Aarch64Mir::new()
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
    use super::{Aarch64Inst, Aarch64Mir, Operand, Reg, RegAlloc, VReg};

    #[test]
    fn test_simple_allocation() {
        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v0),
            imm: 42,
        });

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();

        match &mir.instructions()[0] {
            Aarch64Inst::MovImm { dst, imm } => {
                assert_eq!(dst, &Operand::Physical(Reg::X19));
                assert_eq!(*imm, 42);
            }
            _ => panic!("expected MovImm"),
        }
    }

    #[test]
    fn test_physical_reg_preserved() {
        let mut mir = Aarch64Mir::new();

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Physical(Reg::X0),
            imm: 60,
        });

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();

        match &mir.instructions()[0] {
            Aarch64Inst::MovImm { dst, imm } => {
                assert_eq!(dst, &Operand::Physical(Reg::X0));
                assert_eq!(*imm, 60);
            }
            _ => panic!("expected MovImm"),
        }
    }

    #[test]
    fn test_msub_scratch_registers() {
        // Test that Msub uses X10, X11, X12 for sources, not X9 which is used for dst spill.
        // This verifies the fix for the scratch register conflict bug.
        let mut mir = Aarch64Mir::new();

        // Create 11 vregs to force spilling (we only have 10 allocatable regs: X19-X28)
        let vregs: Vec<VReg> = (0..11).map(|_| mir.alloc_vreg()).collect();

        // Define all vregs
        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(vreg),
                imm: i as i64,
            });
        }

        // Use Msub with the last vreg as destination (likely to be spilled)
        // msub dst, src1, src2, src3 computes: dst = src3 - (src1 * src2)
        mir.push(Aarch64Inst::Msub {
            dst: Operand::Virtual(vregs[10]),
            src1: Operand::Virtual(vregs[0]),
            src2: Operand::Virtual(vregs[1]),
            src3: Operand::Virtual(vregs[2]),
        });

        // Use all vregs to keep them live
        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        // Allocate - this should succeed without panicking
        let result = RegAlloc::new(mir, 0).allocate().unwrap();

        // Verify the Msub instruction was generated
        let msub = result
            .instructions()
            .iter()
            .find_map(|inst| match inst {
                Aarch64Inst::Msub {
                    dst: Operand::Physical(dst),
                    src1: Operand::Physical(src1),
                    src2: Operand::Physical(src2),
                    src3: Operand::Physical(src3),
                } => Some((*dst, *src1, *src2, *src3)),
                _ => None,
            })
            .expect("MSUB should have only physical operands after allocation");
        assert!(!matches!(msub.1, Reg::X9));
        assert!(!matches!(msub.2, Reg::X9));
        assert!(!matches!(msub.3, Reg::X9));
    }

    #[test]
    fn test_multiple_vregs_allocation() {
        // Test allocation of multiple virtual registers
        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let v2 = mir.alloc_vreg();

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v0),
            imm: 1,
        });
        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v1),
            imm: 2,
        });
        mir.push(Aarch64Inst::AddRR {
            dst: Operand::Virtual(v2),
            src1: Operand::Virtual(v0),
            src2: Operand::Virtual(v1),
        });

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();

        // Verify all instructions have physical registers
        for inst in mir.instructions() {
            match inst {
                Aarch64Inst::MovImm { dst, .. } => {
                    assert!(dst.is_physical(), "dst should be physical");
                }
                Aarch64Inst::AddRR { dst, src1, src2 } => {
                    assert!(dst.is_physical(), "dst should be physical");
                    assert!(src1.is_physical(), "src1 should be physical");
                    assert!(src2.is_physical(), "src2 should be physical");
                }
                _ => {}
            }
        }
    }

    #[test]
    fn test_spilling() {
        // Test that spilling works correctly when we run out of registers
        let mut mir = Aarch64Mir::new();

        // Create more vregs than available registers (10 allocatable)
        let vregs: Vec<VReg> = (0..15).map(|_| mir.alloc_vreg()).collect();

        // Define all vregs
        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(vreg),
                imm: i as i64,
            });
        }

        // Use all vregs to keep them live
        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        // With 15 vregs and 10 allocatable registers, we should have spills
        assert!(
            num_spills >= 5,
            "Should have at least 5 spills, got {}",
            num_spills
        );

        // Verify all virtual registers are replaced with physical
        for inst in mir.instructions() {
            match inst {
                Aarch64Inst::MovImm { dst, .. } => {
                    assert!(dst.is_physical());
                }
                Aarch64Inst::MovRR { dst, src } => {
                    assert!(dst.is_physical());
                    assert!(src.is_physical());
                }
                Aarch64Inst::Ldr { dst, .. } => {
                    assert!(dst.is_physical());
                }
                Aarch64Inst::Str { src, .. } => {
                    assert!(src.is_physical());
                }
                _ => {}
            }
        }
    }

    // ========================================
    // Spill slot conflict tests
    // ========================================

    #[test]
    fn test_spill_inserts_load_store() {
        // Force a spill and verify load/store instructions are inserted
        let mut mir = Aarch64Mir::new();

        // Create 11 vregs to force spilling (10 allocatable regs: X19-X28)
        let vregs: Vec<VReg> = (0..11).map(|_| mir.alloc_vreg()).collect();

        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(vreg),
                imm: i as i64,
            });
        }

        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 1, "Should have exactly 1 spill");

        // Verify there's at least one Str (store to stack) and Ldr (load from stack)
        let has_store = mir
            .instructions()
            .iter()
            .any(|inst| matches!(inst, Aarch64Inst::Str { base: Reg::Fp, .. }));
        let has_load = mir
            .instructions()
            .iter()
            .any(|inst| matches!(inst, Aarch64Inst::Ldr { base: Reg::Fp, .. }));

        assert!(has_store, "Should have a store to stack");
        assert!(has_load, "Should have a load from stack");

        let instructions = mir.instructions();
        let load_index = instructions
            .iter()
            .position(|inst| matches!(inst, Aarch64Inst::Ldr { base: Reg::Fp, .. }))
            .expect("spill load should be present");
        assert!(matches!(
            instructions.get(load_index + 1),
            Some(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(_),
            })
        ));

        let store_index = instructions
            .iter()
            .position(|inst| matches!(inst, Aarch64Inst::Str { base: Reg::Fp, .. }))
            .expect("spill store should be present");
        assert!(matches!(
            store_index
                .checked_sub(1)
                .and_then(|index| instructions.get(index)),
            Some(Aarch64Inst::MovImm { .. })
        ));
    }

    #[test]
    fn test_call_survival_and_symbol_reconstruction() {
        let mut mir = Aarch64Mir::new();
        let value = mir.alloc_vreg();
        let symbol = mir.intern_symbol("callee");

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(value),
            imm: 42,
        });
        mir.push(Aarch64Inst::Bl { symbol_id: symbol });
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Virtual(value),
        });

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();
        let value_reg = mir
            .instructions()
            .iter()
            .find_map(|inst| match inst {
                Aarch64Inst::MovImm {
                    dst: Operand::Physical(reg),
                    ..
                } => Some(*reg),
                _ => None,
            })
            .expect("value definition should be physical");

        assert!(value_reg.is_callee_saved());
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Bl { symbol_id } if *symbol_id == symbol))
        );
        assert!(mir.instructions().iter().any(|inst| matches!(
            inst,
            Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(reg),
            } if *reg == value_reg
        )));
        assert_eq!(mir.get_symbol(symbol), "callee");
    }

    #[test]
    fn test_loop_pressure_uses_loop_aware_spilling() {
        let mut mir = Aarch64Mir::new();
        let vregs: Vec<VReg> = (0..11).map(|_| mir.alloc_vreg()).collect();
        let loop_label = mir.alloc_label();

        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(vreg),
                imm: i as i64,
            });
        }
        mir.push(Aarch64Inst::Label { id: loop_label });
        mir.push(Aarch64Inst::SubImm {
            dst: Operand::Virtual(vregs[0]),
            src: Operand::Virtual(vregs[0]),
            imm: 1,
        });
        mir.push(Aarch64Inst::Cbnz {
            src: Operand::Virtual(vregs[0]),
            label: loop_label,
        });
        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let loop_info = liveness::analyze_loops(&mir);
        assert!(
            loop_info.depth(12) > 0,
            "loop body should have nonzero depth"
        );

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();
        assert!(num_spills > 0);
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Str { base: Reg::Fp, .. }))
        );
        assert!(
            mir.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Label { id } if *id == loop_label))
        );
    }

    #[test]
    fn test_allocate_with_debug_is_deterministic() {
        let make_mir = || {
            let mut mir = Aarch64Mir::new();
            let vregs: Vec<VReg> = (0..11).map(|_| mir.alloc_vreg()).collect();
            for (i, &vreg) in vregs.iter().enumerate() {
                mir.push(Aarch64Inst::MovImm {
                    dst: Operand::Virtual(vreg),
                    imm: i as i64,
                });
            }
            for &vreg in &vregs {
                mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Physical(Reg::X0),
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
        let mut mir = Aarch64Mir::new();

        // Create 15 vregs to force 5 spills (10 allocatable regs)
        let vregs: Vec<VReg> = (0..15).map(|_| mir.alloc_vreg()).collect();

        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(vreg),
                imm: i as i64,
            });
        }

        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 5);

        // Collect all unique stack offsets used in loads/stores
        let mut offsets = std::collections::HashSet::new();
        for inst in mir.instructions() {
            match inst {
                Aarch64Inst::Str {
                    base: Reg::Fp,
                    offset,
                    ..
                } => {
                    offsets.insert(*offset);
                }
                Aarch64Inst::Ldr {
                    base: Reg::Fp,
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
        let mut mir = Aarch64Mir::new();

        // 11 vregs forces 1 spill
        let vregs: Vec<VReg> = (0..11).map(|_| mir.alloc_vreg()).collect();

        for vreg in &vregs {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(*vreg),
                imm: 42,
            });
        }
        for vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(*vreg),
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
                Aarch64Inst::Str {
                    base: Reg::Fp,
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
        let mut mir = Aarch64Mir::new();

        // Create 25 vregs (10 registers + 15 spills)
        let vregs: Vec<VReg> = (0..25).map(|_| mir.alloc_vreg()).collect();

        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(vreg),
                imm: i as i64,
            });
        }

        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 15);

        // Verify all virtual registers were replaced with physical
        for inst in mir.instructions() {
            match inst {
                Aarch64Inst::MovImm { dst, .. } => {
                    assert!(dst.is_physical());
                }
                Aarch64Inst::MovRR { dst, src } => {
                    assert!(dst.is_physical());
                    assert!(src.is_physical());
                }
                Aarch64Inst::Ldr { dst, .. } => {
                    assert!(dst.is_physical());
                }
                Aarch64Inst::Str { src, .. } => {
                    assert!(src.is_physical());
                }
                _ => {}
            }
        }
    }

    #[test]
    fn test_spill_with_many_locals() {
        // Test spilling with many existing local variables
        let mut mir = Aarch64Mir::new();

        // 11 vregs forces 1 spill
        let vregs: Vec<VReg> = (0..11).map(|_| mir.alloc_vreg()).collect();

        for vreg in &vregs {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(*vreg),
                imm: 1,
            });
        }
        for vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
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
                Aarch64Inst::Str {
                    base: Reg::Fp,
                    offset,
                    ..
                } => Some(*offset),
                _ => None,
            })
            .expect("Should have a spill store");

        assert_eq!(spill_offset, -408);
    }

    #[test]
    fn test_ternop_with_spilled_operands() {
        // Test that ternary operations work correctly when operands are spilled
        let mut mir = Aarch64Mir::new();

        // Create enough vregs to force spilling
        let vregs: Vec<VReg> = (0..13).map(|_| mir.alloc_vreg()).collect();

        // Initialize all
        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(vreg),
                imm: i as i64,
            });
        }

        // Add using potentially spilled operands
        mir.push(Aarch64Inst::AddRR {
            dst: Operand::Virtual(vregs[0]),
            src1: Operand::Virtual(vregs[11]),
            src2: Operand::Virtual(vregs[12]),
        });

        // Use all to keep them live
        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert!(num_spills >= 3, "Should have some spills");

        // Verify the AddRR was properly rewritten
        let has_add = mir.instructions().iter().any(|inst| {
            matches!(inst, Aarch64Inst::AddRR { dst, src1, src2 }
                if dst.is_physical() && src1.is_physical() && src2.is_physical())
        });
        assert!(has_add, "AddRR should be rewritten with physical registers");
    }

    #[test]
    fn test_spilled_binary_destination_has_ordered_rewrite() {
        let mut mir = Aarch64Mir::new();
        let vregs: Vec<VReg> = (0..12).map(|_| mir.alloc_vreg()).collect();

        for (i, &vreg) in vregs.iter().enumerate() {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(vreg),
                imm: i as i64,
            });
        }
        mir.push(Aarch64Inst::AddRR {
            dst: Operand::Virtual(vregs[11]),
            src1: Operand::Virtual(vregs[10]),
            src2: Operand::Virtual(vregs[0]),
        });
        for &vreg in &vregs[1..] {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();
        let add_index = mir
            .instructions()
            .iter()
            .position(|inst| matches!(inst, Aarch64Inst::AddRR { .. }))
            .expect("rewritten AddRR should be present");
        assert!(matches!(
            add_index
                .checked_sub(1)
                .and_then(|index| mir.instructions().get(index)),
            Some(Aarch64Inst::Ldr { base: Reg::Fp, .. })
        ));
        assert!(matches!(
            mir.instructions().get(add_index + 1),
            Some(Aarch64Inst::Str { base: Reg::Fp, .. })
        ));
    }
}
