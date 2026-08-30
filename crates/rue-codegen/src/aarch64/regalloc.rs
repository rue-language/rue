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
use super::mir;
use super::mir::VReg;
use super::mir::{
    Aarch64Inst, Aarch64Mir, Operand, Reg, SCRATCH_SOURCE_A, SCRATCH_SOURCE_B, SCRATCH_SOURCE_C,
    SCRATCH_VALUE,
};
use crate::alloc_dst;
use crate::regalloc::{
    Allocation, AllocationContext, CoalesceCandidate, LivenessInfo, LoopInfo, RegAllocBackend,
    RegAllocDebugInfo, RegAllocDriver, RegisterFile, RematerializeOp, RewriteBuffer, SaveClasses,
};

/// Caller-saved registers offered to intervals no instruction clobbers while
/// they are live — in particular, intervals that cross no call.
///
/// AAPCS64 makes X9-X15 caller-saved temporaries. X9-X12 are this pass's own
/// rewrite scratch and X15 is the emitter's address scratch, leaving X13 and
/// X14. See [`mir::RESERVED_REGS`] for the per-register reasoning; the
/// assertion below keeps the two in agreement.
const CALLER_SAVED_REGS: &[Reg] = &[Reg::X13, Reg::X14];

/// Callee-saved registers, the only home for a value that must survive a call.
///
/// Each one used obliges the prologue to save it and the epilogue to restore
/// it, so allocation reaches for these only after the caller-saved class above
/// is exhausted or ineligible.
const CALLEE_SAVED_REGS: &[Reg] = &[
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

/// Every allocatable register, in preference order: caller-saved first.
///
/// This is the flattening of [`CALLER_SAVED_REGS`] and [`CALLEE_SAVED_REGS`];
/// [`register_classes`](Aarch64Backend::register_classes) is what allocation
/// actually consults, and it keeps the two classes apart. When neither class
/// has a register available, values are spilled to the stack.
const ALLOCATABLE_REGS: &[Reg] = &[
    Reg::X13,
    Reg::X14,
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

// No allocatable register may carry a reserved role, and the flattened list
// must stay the concatenation of the two classes.
const _: () = {
    let mut index = 0;
    while index < ALLOCATABLE_REGS.len() {
        assert!(
            !mir::is_reserved(ALLOCATABLE_REGS[index]),
            "an allocatable register must not also be reserved for an ABI \
             position, rewrite scratch, or a platform role"
        );
        index += 1;
    }
    assert!(ALLOCATABLE_REGS.len() == CALLER_SAVED_REGS.len() + CALLEE_SAVED_REGS.len());
    let mut index = 0;
    while index < CALLER_SAVED_REGS.len() {
        assert!(ALLOCATABLE_REGS[index] as u8 == CALLER_SAVED_REGS[index] as u8);
        index += 1;
    }
    let mut index = 0;
    while index < CALLEE_SAVED_REGS.len() {
        assert!(
            ALLOCATABLE_REGS[CALLER_SAVED_REGS.len() + index] as u8
                == CALLEE_SAVED_REGS[index] as u8
        );
        index += 1;
    }
};

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
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::MovImm { dst: dst_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::MovRR { dst, src } => {
                let src_op = Self::load_operand(context, mir, src, SCRATCH_VALUE)?;
                let dst_alloc = Self::get_allocation(context, dst);

                match dst_alloc {
                    Some(Allocation::Register(reg)) => {
                        mir.push(Aarch64Inst::MovRR {
                            dst: Operand::Physical(reg),
                            src: src_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        if src_op != Operand::Physical(SCRATCH_VALUE) {
                            mir.push(Aarch64Inst::MovRR {
                                dst: Operand::Physical(SCRATCH_VALUE),
                                src: src_op,
                            });
                        }
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
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
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Ldr { dst: dst_op, base, offset });
                    },
                    store |spill_offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
                            base: Reg::Fp,
                            offset: spill_offset,
                        });
                    },
                );
            }

            Aarch64Inst::Str { src, base, offset } => {
                let src_op = Self::load_operand(context, mir, src, SCRATCH_VALUE)?;
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
                let src1_op = Self::load_operand(context, mir, src1, SCRATCH_SOURCE_A)?;
                let src2_op = Self::load_operand(context, mir, src2, SCRATCH_SOURCE_B)?;
                let src3_op = Self::load_operand(context, mir, src3, SCRATCH_SOURCE_C)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
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
                            src: Operand::Physical(SCRATCH_VALUE),
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
                let src1_op = Self::load_operand(context, mir, src1, SCRATCH_SOURCE_A)?;
                let src2_op = Self::load_operand(context, mir, src2, SCRATCH_SOURCE_B)?;
                let src3_op = Self::load_operand(context, mir, src3, SCRATCH_SOURCE_C)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
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
                            src: Operand::Physical(SCRATCH_VALUE),
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
                let src_op = Self::load_operand(context, mir, src, SCRATCH_SOURCE_A)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::EorImm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
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
                let src1_op = Self::load_operand(context, mir, src1, SCRATCH_VALUE)?;
                let src2_op = Self::load_operand(context, mir, src2, SCRATCH_SOURCE_A)?;
                mir.push(Aarch64Inst::CmpRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            Aarch64Inst::Cmp64RR { src1, src2 } => {
                let src1_op = Self::load_operand(context, mir, src1, SCRATCH_VALUE)?;
                let src2_op = Self::load_operand(context, mir, src2, SCRATCH_SOURCE_A)?;
                mir.push(Aarch64Inst::Cmp64RR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            Aarch64Inst::CmpImm { src, imm } => {
                let src_op = Self::load_operand(context, mir, src, SCRATCH_VALUE)?;
                mir.push(Aarch64Inst::CmpImm { src: src_op, imm });
            }

            Aarch64Inst::Cbz { src, label } => {
                let src_op = Self::load_operand(context, mir, src, SCRATCH_VALUE)?;
                mir.push(Aarch64Inst::Cbz { src: src_op, label });
            }

            Aarch64Inst::Cbnz { src, label } => {
                let src_op = Self::load_operand(context, mir, src, SCRATCH_VALUE)?;
                mir.push(Aarch64Inst::Cbnz { src: src_op, label });
            }

            Aarch64Inst::Cset { dst, cond } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Cset { dst: dst_op, cond });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::TstRR { src1, src2 } => {
                let src1_op = Self::load_operand(context, mir, src1, SCRATCH_VALUE)?;
                let src2_op = Self::load_operand(context, mir, src2, SCRATCH_SOURCE_A)?;
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

            Aarch64Inst::Uxtw { dst, src } => {
                Self::emit_binop(context, mir, dst, src, |d, s| Aarch64Inst::Uxtw {
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
                let src1_op = Self::load_operand(context, mir, src1, SCRATCH_VALUE)?;
                let src2_op = Self::load_operand(context, mir, src2, SCRATCH_SOURCE_A)?;
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
                    Some(Allocation::Spill(_)) => Operand::Physical(SCRATCH_VALUE),
                    Some(Allocation::Rematerialize(_)) => {
                        unreachable!("destination cannot be rematerializable")
                    }
                    None => dst1,
                };
                let dst2_phys = match Self::get_allocation(context, dst2) {
                    Some(Allocation::Register(reg)) => Operand::Physical(reg),
                    Some(Allocation::Spill(_)) => Operand::Physical(SCRATCH_SOURCE_A),
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
                        src: Operand::Physical(SCRATCH_VALUE),
                        base: Reg::Fp,
                        offset: off,
                    });
                }
                if let Some(Allocation::Spill(off)) = Self::get_allocation(context, dst2) {
                    mir.push_after(Aarch64Inst::Str {
                        src: Operand::Physical(SCRATCH_SOURCE_A),
                        base: Reg::Fp,
                        offset: off,
                    });
                }
            }

            Aarch64Inst::LdrIndexed { dst, base } => {
                // Load base vreg into scratch, then emit load with the result allocation
                let base_op = Operand::Virtual(base);
                let base_reg = Self::load_operand(context, mir, base_op, SCRATCH_VALUE)?;
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => SCRATCH_VALUE,
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
                            dst: Operand::Physical(SCRATCH_SOURCE_A),
                            base: base_phys,
                            offset: 0,
                        });
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_SOURCE_A),
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
                let src_op = Self::load_operand(context, mir, src, SCRATCH_VALUE)?;
                let base_vreg_op = Operand::Virtual(base);
                let base_reg = Self::load_operand(context, mir, base_vreg_op, SCRATCH_SOURCE_A)?;
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => SCRATCH_SOURCE_A,
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
                let base_reg =
                    Self::load_operand(context, mir, Operand::Virtual(base), SCRATCH_VALUE)?;
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
                            dst: Operand::Physical(SCRATCH_SOURCE_A),
                            base: base_phys,
                            offset: addr_offset,
                            width,
                            signed,
                        });
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_SOURCE_A),
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
                let src_op = Self::load_operand(context, mir, src, SCRATCH_VALUE)?;
                let base_reg =
                    Self::load_operand(context, mir, Operand::Virtual(base), SCRATCH_SOURCE_A)?;
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
                let base_reg = Self::load_operand(context, mir, base_op, SCRATCH_VALUE)?;
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => SCRATCH_VALUE,
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
                            dst: Operand::Physical(SCRATCH_SOURCE_A),
                            base: base_phys,
                            offset,
                        });
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_SOURCE_A),
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
                let src_op = Self::load_operand(context, mir, src, SCRATCH_VALUE)?;
                let base_vreg_op = Operand::Virtual(base);
                let base_reg = Self::load_operand(context, mir, base_vreg_op, SCRATCH_SOURCE_A)?;
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => SCRATCH_SOURCE_A,
                };
                mir.push(Aarch64Inst::Str {
                    src: src_op,
                    base: base_phys,
                    offset,
                });
            }

            Aarch64Inst::LslImm { dst, src, imm } => {
                let src_op = Self::load_operand(context, mir, src, SCRATCH_SOURCE_A)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::LslImm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::Lsl32Imm { dst, src, imm } => {
                let src_op = Self::load_operand(context, mir, src, SCRATCH_SOURCE_A)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Lsl32Imm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::Lsr32Imm { dst, src, imm } => {
                let src_op = Self::load_operand(context, mir, src, SCRATCH_SOURCE_A)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Lsr32Imm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::Asr32Imm { dst, src, imm } => {
                let src_op = Self::load_operand(context, mir, src, SCRATCH_SOURCE_A)?;
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::Asr32Imm { dst: dst_op, src: src_op, imm });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::StringConstPtr { dst, string_id } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::StringConstPtr { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::StringConstLen { dst, string_id } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::StringConstLen { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
                            base: Reg::Fp,
                            offset,
                        });
                    },
                );
            }

            Aarch64Inst::StringConstCap { dst, string_id } => {
                alloc_dst!(Self::get_allocation(context, dst), dst, SCRATCH_VALUE =>
                    emit |dst_op| {
                        mir.push(Aarch64Inst::StringConstCap { dst: dst_op, string_id });
                    },
                    store |offset| {
                        mir.push_after(Aarch64Inst::Str {
                            src: Operand::Physical(SCRATCH_VALUE),
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
            Aarch64Inst::Bl { symbol_id, returns } => {
                mir.push(Aarch64Inst::Bl { symbol_id, returns })
            }
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
        let src_op = Self::load_operand(context, mir, src, SCRATCH_SOURCE_A)?;
        match Self::get_allocation(context, dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src_op));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(make_inst(Operand::Physical(SCRATCH_VALUE), src_op));
                mir.push_after(Aarch64Inst::Str {
                    src: Operand::Physical(SCRATCH_VALUE),
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
        let src1_op = Self::load_operand(context, mir, src1, SCRATCH_SOURCE_A)?;
        let src2_op = Self::load_operand(context, mir, src2, SCRATCH_SOURCE_B)?;
        match Self::get_allocation(context, dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src1_op, src2_op));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(make_inst(
                    Operand::Physical(SCRATCH_VALUE),
                    src1_op,
                    src2_op,
                ));
                mir.push_after(Aarch64Inst::Str {
                    src: Operand::Physical(SCRATCH_VALUE),
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
        let src_op = Self::load_operand(context, mir, src, SCRATCH_SOURCE_A)?;
        match Self::get_allocation(context, dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src_op, imm));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(make_inst(Operand::Physical(SCRATCH_VALUE), src_op, imm));
                mir.push_after(Aarch64Inst::Str {
                    src: Operand::Physical(SCRATCH_VALUE),
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

    fn defs(inst: &Self::Inst) -> crate::liveness::VRegList {
        liveness::defs(inst)
    }

    fn rematerialization(inst: &Self::Inst) -> Option<(VReg, RematerializeOp)> {
        match inst {
            Aarch64Inst::MovImm {
                dst: Operand::Virtual(dst),
                imm,
            } => Some((*dst, RematerializeOp::Const64(*imm))),
            Aarch64Inst::StringConstPtr {
                dst: Operand::Virtual(dst),
                string_id,
            } => Some((*dst, RematerializeOp::StringPtr(*string_id))),
            Aarch64Inst::StringConstLen {
                dst: Operand::Virtual(dst),
                string_id,
            } => Some((*dst, RematerializeOp::StringLen(*string_id))),
            Aarch64Inst::StringConstCap {
                dst: Operand::Virtual(dst),
                string_id,
            } => Some((*dst, RematerializeOp::StringCap(*string_id))),
            _ => None,
        }
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

    fn register_file() -> RegisterFile<'static, Self::Reg> {
        // General-purpose only: `Reg` names no floating-point register yet, so
        // the `Fp` class of the file is empty and no interval can select it
        // (RUE-1067). The floats series adds the V/D registers here.
        RegisterFile::gp_only(SaveClasses {
            caller_saved: CALLER_SAVED_REGS,
            callee_saved: CALLEE_SAVED_REGS,
            // AArch64 instructions are a fixed four bytes and encode every
            // general register in the same five-bit field, so no allocatable
            // register is cheaper to address than another and the RUE-1227
            // preference has nothing to trade. Reusing a callee-saved register
            // in place of a caller-saved one here would only add pressure.
            compact_callee_saved: &[],
        })
    }

    fn for_each_physical_operand<F>(inst: &Self::Inst, mut visit: F)
    where
        F: FnMut(Self::Reg),
    {
        for reg in super::schedule::regs_read(inst) {
            visit(reg);
        }
        for reg in super::schedule::regs_written(inst) {
            visit(reg);
        }
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
    use super::Aarch64Backend;
    use super::liveness;
    use super::{
        ALLOCATABLE_REGS, Aarch64Inst, Aarch64Mir, CALLEE_SAVED_REGS, CALLER_SAVED_REGS, Operand,
        Reg, RegAlloc, SCRATCH_SOURCE_A, SCRATCH_VALUE, VReg,
    };
    use crate::reg_class::RegClass;
    use crate::regalloc::{Allocation, RegAllocBackend, RematerializeOp};
    use ahash::AHashSet;

    #[test]
    fn the_register_file_is_general_purpose_only() {
        // The whole of RUE-1067's claim on this backend: the class dimension
        // exists, and the floating-point half of it is empty. A change that
        // populates `Fp` here without the rest of the floats series would make
        // allocation hand out registers no instruction can encode, so this
        // guard fails loudly instead.
        let file = <Aarch64Backend as RegAllocBackend>::register_file();
        let gp = file.class(RegClass::Gp);
        let fp = file.class(RegClass::Fp);

        assert_eq!(gp.caller_saved, CALLER_SAVED_REGS);
        assert_eq!(gp.callee_saved, CALLEE_SAVED_REGS);
        assert!(!gp.is_empty());
        assert!(
            fp.is_empty(),
            "no floating-point register is allocatable yet"
        );
        assert_eq!(file.len(), ALLOCATABLE_REGS.len());
        assert_eq!(file.caller_saved_flattened(), CALLER_SAVED_REGS.to_vec());
    }

    #[test]
    fn physical_operand_visitor_preserves_read_then_write_order() {
        let inst = Aarch64Inst::AddRR {
            dst: Operand::Physical(Reg::X2),
            src1: Operand::Physical(Reg::X0),
            src2: Operand::Physical(Reg::X1),
        };
        let mut observed = Vec::new();
        <Aarch64Backend as RegAllocBackend>::for_each_physical_operand(&inst, |reg| {
            observed.push(reg);
        });
        assert_eq!(observed, [Reg::X0, Reg::X1, Reg::X2]);
    }

    #[test]
    fn allocation_liveness_classes_every_vreg_as_general_purpose() {
        // Liveness is where the class table reaches allocation, so this is the
        // end-to-end statement that lowering mints one class only.
        let mut mir = Aarch64Mir::new();
        let vregs: Vec<VReg> = (0..3).map(|_| mir.alloc_vreg()).collect();
        define_loaded_values(&mut mir, &vregs);

        let info = liveness::analyze(&mir);

        assert_eq!(info.vreg_classes.len(), mir.vreg_count());
        assert_eq!(info.vreg_classes.count_in(RegClass::Fp), 0);
        for &vreg in &vregs {
            assert_eq!(info.class_of(vreg), RegClass::Gp);
        }
    }

    fn define_loaded_values(mir: &mut Aarch64Mir, vregs: &[VReg]) {
        for (index, &vreg) in vregs.iter().enumerate() {
            mir.push(Aarch64Inst::Ldr {
                dst: Operand::Virtual(vreg),
                base: Reg::X1,
                offset: (index as i32) * 8,
            });
        }
    }

    #[test]
    fn test_simple_allocation() {
        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v0),
            imm: 42,
        });

        let mir = RegAlloc::new(mir, 0).allocate().unwrap();

        // v0 should be allocated to the first allocatable register, which is
        // caller-saved: nothing clobbers it while v0 is live (RUE-1146).
        match &mir.instructions()[0] {
            Aarch64Inst::MovImm { dst, imm } => {
                assert_eq!(dst, &Operand::Physical(ALLOCATABLE_REGS[0]));
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

        define_loaded_values(&mut mir, &vregs);

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

        // Five more simultaneously-live values than there are registers
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len() + 5)
            .map(|_| mir.alloc_vreg())
            .collect();

        // Define all vregs
        define_loaded_values(&mut mir, &vregs);

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

        // One more simultaneously-live value than there are registers
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len() + 1)
            .map(|_| mir.alloc_vreg())
            .collect();

        define_loaded_values(&mut mir, &vregs);

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
            Some(Aarch64Inst::Ldr { base: Reg::X1, .. })
        ));
    }

    #[test]
    fn test_production_rematerializes_constants_and_strings_without_spills() {
        let mut mir = Aarch64Mir::new();
        // Two more simultaneously-live values than there are registers, so the
        // callee-saved-only baseline below has to spill exactly twice.
        let count = ALLOCATABLE_REGS.len() + 2;
        let vregs: Vec<VReg> = (0..count).map(|_| mir.alloc_vreg()).collect();

        for (index, &vreg) in vregs[..count - 2].iter().enumerate() {
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(vreg),
                imm: index as i64,
            });
        }
        mir.push(Aarch64Inst::StringConstPtr {
            dst: Operand::Virtual(vregs[count - 2]),
            string_id: 7,
        });
        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(vregs[count - 1]),
            imm: 99,
        });
        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let baseline_liveness = liveness::analyze(&mir);
        let baseline_loops = liveness::analyze_loops(&mir);
        let (_, baseline_spills, _) = crate::regalloc::linear_scan_with_cost_model(
            mir.vreg_count(),
            &baseline_liveness,
            ALLOCATABLE_REGS,
            0,
            &crate::regalloc::CostModel::default(),
            &baseline_loops,
        );
        let (mir, num_spills, _, debug) = RegAlloc::new(mir, 0).allocate_with_debug().unwrap();

        assert_eq!(baseline_spills, 2);
        assert!(num_spills < baseline_spills);
        assert_eq!(num_spills, 0, "rematerialized values need no frame slots");
        assert!(debug.allocations.iter().any(|(_, allocation)| matches!(
            allocation,
            crate::regalloc::Allocation::Rematerialize(
                crate::regalloc::RematerializeOp::StringPtr(7)
            )
        )));
        assert!(debug.allocations.iter().any(|(_, allocation)| matches!(
            allocation,
            crate::regalloc::Allocation::Rematerialize(crate::regalloc::RematerializeOp::Const64(
                99
            ))
        )));
        assert!(!mir.instructions().iter().any(|inst| matches!(
            inst,
            Aarch64Inst::Str { base: Reg::Fp, .. } | Aarch64Inst::Ldr { base: Reg::Fp, .. }
        )));
    }

    #[test]
    fn test_rematerialization_recipe_survives_coalescing() {
        let mut mir = Aarch64Mir::new();
        let loaded: Vec<VReg> = (0..ALLOCATABLE_REGS.len())
            .map(|_| mir.alloc_vreg())
            .collect();
        let constant = mir.alloc_vreg();
        let moved = mir.alloc_vreg();

        define_loaded_values(&mut mir, &loaded);
        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(constant),
            imm: 42,
        });
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(moved),
            src: Operand::Virtual(constant),
        });
        for &vreg in &loaded {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Virtual(moved),
        });

        let (mir, num_spills, _, debug) = RegAlloc::new(mir, 0).allocate_with_debug().unwrap();

        assert_eq!(num_spills, 0);
        assert!(debug.allocations.contains(&(
            constant.index(),
            Allocation::Rematerialize(RematerializeOp::Const64(42))
        )));
        assert!(mir.instructions().iter().any(|inst| matches!(
            inst,
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X9),
                imm: 42
            }
        )));
    }

    #[test]
    fn test_rematerialization_requires_identical_definitions() {
        fn allocate(second_value: i64) -> (u32, Allocation<Reg>) {
            let mut mir = Aarch64Mir::new();
            let loaded: Vec<VReg> = (0..ALLOCATABLE_REGS.len())
                .map(|_| mir.alloc_vreg())
                .collect();
            let constant = mir.alloc_vreg();

            define_loaded_values(&mut mir, &loaded);
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(constant),
                imm: 42,
            });
            mir.push(Aarch64Inst::MovImm {
                dst: Operand::Virtual(constant),
                imm: second_value,
            });
            for &vreg in &loaded {
                mir.push(Aarch64Inst::MovRR {
                    dst: Operand::Physical(Reg::X0),
                    src: Operand::Virtual(vreg),
                });
            }
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(constant),
            });

            let (_, num_spills, _, debug) = RegAlloc::new(mir, 0).allocate_with_debug().unwrap();
            let allocation = debug
                .allocations
                .iter()
                .find_map(|(vreg, allocation)| (*vreg == constant.index()).then_some(*allocation))
                .unwrap();
            (num_spills, allocation)
        }

        assert_eq!(
            allocate(42),
            (0, Allocation::Rematerialize(RematerializeOp::Const64(42)))
        );
        assert!(matches!(allocate(43), (1, Allocation::Spill(_))));
    }

    #[test]
    fn test_rematerialization_rejects_in_place_updates() {
        let mut mir = Aarch64Mir::new();
        let loaded: Vec<VReg> = (0..ALLOCATABLE_REGS.len())
            .map(|_| mir.alloc_vreg())
            .collect();
        let updated = mir.alloc_vreg();

        define_loaded_values(&mut mir, &loaded);
        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(updated),
            imm: 42,
        });
        mir.push(Aarch64Inst::AddImm {
            dst: Operand::Virtual(updated),
            src: Operand::Virtual(updated),
            imm: 1,
        });
        for &vreg in &loaded {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Virtual(updated),
        });

        let (_, num_spills, _, debug) = RegAlloc::new(mir, 0).allocate_with_debug().unwrap();

        assert_eq!(num_spills, 1);
        assert!(
            debug
                .allocations
                .iter()
                .any(|(vreg, allocation)| *vreg == updated.index()
                    && matches!(allocation, Allocation::Spill(_)))
        );
    }

    #[test]
    fn caller_saved_registers_absorb_pressure_before_spilling() {
        // A call-free function with more simultaneously-live values than there
        // are callee-saved registers still fits in registers, because the
        // caller-saved class is available to it (RUE-1146).
        let mut mir = Aarch64Mir::new();
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len())
            .map(|_| mir.alloc_vreg())
            .collect();

        define_loaded_values(&mut mir, &vregs);
        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        assert!(
            vregs.len() > CALLEE_SAVED_REGS.len(),
            "the fixture must exceed what the callee-saved class alone can hold"
        );

        let (mir, num_spills, used_callee_saved) =
            RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 0, "every value should fit in a register");
        assert!(
            !mir.instructions().iter().any(|inst| matches!(
                inst,
                Aarch64Inst::Str { base: Reg::Fp, .. } | Aarch64Inst::Ldr { base: Reg::Fp, .. }
            )),
            "no value should touch the frame"
        );

        let assigned: Vec<Reg> = mir
            .instructions()
            .iter()
            .filter_map(|inst| match inst {
                Aarch64Inst::Ldr {
                    dst: Operand::Physical(reg),
                    base: Reg::X1,
                    ..
                } => Some(*reg),
                _ => None,
            })
            .collect();
        for &reg in CALLER_SAVED_REGS {
            assert!(
                assigned.contains(&reg),
                "the caller-saved class should be used before spilling, missing {reg}"
            );
        }
        for &reg in &used_callee_saved {
            assert!(
                CALLEE_SAVED_REGS.contains(&reg),
                "only callee-saved registers oblige the prologue, got {reg}"
            );
        }
    }

    #[test]
    fn call_free_function_saves_no_callee_saved_registers() {
        // With the caller-saved class available, a small call-free function
        // needs nothing preserved across its own body, so frame planning sees
        // an empty callee-saved set (RUE-1146, and the reason RUE-1195 had to
        // land first).
        let mut mir = Aarch64Mir::new();
        let vregs: Vec<VReg> = (0..CALLER_SAVED_REGS.len())
            .map(|_| mir.alloc_vreg())
            .collect();

        define_loaded_values(&mut mir, &vregs);
        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let (_, num_spills, used_callee_saved) =
            RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 0);
        assert!(
            used_callee_saved.is_empty(),
            "a call-free function this small should save nothing, got {used_callee_saved:?}"
        );
    }

    #[test]
    fn cross_call_values_never_take_a_caller_saved_register() {
        // Every value here is defined before a call and used after it, so none
        // may live in a caller-saved register — the call destroys them all.
        let mut mir = Aarch64Mir::new();
        let symbol = mir.intern_symbol("callee");
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len())
            .map(|_| mir.alloc_vreg())
            .collect();

        define_loaded_values(&mut mir, &vregs);
        mir.push(Aarch64Inst::call(symbol));
        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, used_callee_saved) =
            RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        // The values that exceed the callee-saved class spill rather than
        // borrowing a caller-saved register.
        assert_eq!(num_spills as usize, CALLER_SAVED_REGS.len());
        for &reg in &used_callee_saved {
            assert!(CALLEE_SAVED_REGS.contains(&reg));
        }
        for inst in mir.instructions() {
            if let Aarch64Inst::Ldr {
                dst: Operand::Physical(reg),
                base: Reg::X1,
                ..
            } = inst
            {
                assert!(
                    !CALLER_SAVED_REGS.contains(reg),
                    "a value live across a call must not be in caller-saved {reg}"
                );
            }
        }
    }

    #[test]
    fn trap_crossing_values_still_take_caller_saved_registers() {
        // The same fixture as `cross_call_values_never_take_a_caller_saved_register`,
        // except the call is the overflow trap. It never returns, so no value
        // is live across it and the caller-saved class stays available
        // (RUE-1224).
        let mut mir = Aarch64Mir::new();
        let symbol = mir.intern_symbol(rue_runtime_abi::RuntimeHelperId::Overflow.symbol());
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len())
            .map(|_| mir.alloc_vreg())
            .collect();

        define_loaded_values(&mut mir, &vregs);
        mir.push(Aarch64Inst::Bl {
            symbol_id: symbol,
            returns: rue_runtime_abi::ReturnBehavior::Never,
        });
        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(
            num_spills, 0,
            "the trap is not a barrier, so every value should still fit in a register"
        );
        let assigned: Vec<Reg> = mir
            .instructions()
            .iter()
            .filter_map(|inst| match inst {
                Aarch64Inst::Ldr {
                    dst: Operand::Physical(reg),
                    base: Reg::X1,
                    ..
                } => Some(*reg),
                _ => None,
            })
            .collect();
        for &reg in CALLER_SAVED_REGS {
            assert!(
                assigned.contains(&reg),
                "a value spanning only the trap should be allowed caller-saved {reg}"
            );
        }
    }

    #[test]
    fn a_never_returning_call_ends_liveness() {
        // Modelling the trap as non-returning is what makes the clobber test
        // above correct: nothing propagates past it.
        let mut mir = Aarch64Mir::new();
        let value = mir.alloc_vreg();
        let symbol = mir.intern_symbol(rue_runtime_abi::RuntimeHelperId::Overflow.symbol());

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(value),
            imm: 42,
        });
        mir.push(Aarch64Inst::Bl {
            symbol_id: symbol,
            returns: rue_runtime_abi::ReturnBehavior::Never,
        });
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Virtual(value),
        });
        mir.push(Aarch64Inst::Ret);

        let debug = liveness::analyze_debug(&mir);
        assert!(
            debug.instructions[1].live_out.is_empty(),
            "a never-returning call must have an empty live-out set; found: {:?}",
            debug.instructions[1].live_out
        );
    }

    #[test]
    #[should_panic(expected = "lowering named the allocatable register")]
    fn lowering_may_not_name_an_allocatable_register_as_a_physical_operand() {
        let mut mir = Aarch64Mir::new();
        let value = mir.alloc_vreg();
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(value),
            src: Operand::Physical(ALLOCATABLE_REGS[0]),
        });

        let _ = RegAlloc::new(mir, 0).allocate();
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
        mir.push(Aarch64Inst::call(symbol));
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
            mir.instructions().iter().any(
                |inst| matches!(inst, Aarch64Inst::Bl { symbol_id, .. } if *symbol_id == symbol)
            )
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
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len() + 1)
            .map(|_| mir.alloc_vreg())
            .collect();
        let loop_label = mir.alloc_label();

        define_loaded_values(&mut mir, &vregs);
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
        // The loop body starts one instruction past the label, which the
        // per-value loads above precede.
        assert!(
            loop_info.depth(vregs.len() + 1) > 0,
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

        // Five more simultaneously-live values than there are registers
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len() + 5)
            .map(|_| mir.alloc_vreg())
            .collect();

        define_loaded_values(&mut mir, &vregs);

        for &vreg in &vregs {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();

        assert_eq!(num_spills, 5);

        // Collect all unique stack offsets used in loads/stores
        let mut offsets = AHashSet::new();
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

        // One more simultaneously-live value than there are registers
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len() + 1)
            .map(|_| mir.alloc_vreg())
            .collect();

        define_loaded_values(&mut mir, &vregs);
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

        // Fifteen more simultaneously-live values than there are registers
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len() + 15)
            .map(|_| mir.alloc_vreg())
            .collect();

        define_loaded_values(&mut mir, &vregs);

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

        // One more simultaneously-live value than there are registers
        let vregs: Vec<VReg> = (0..ALLOCATABLE_REGS.len() + 1)
            .map(|_| mir.alloc_vreg())
            .collect();

        define_loaded_values(&mut mir, &vregs);
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

        // Three more simultaneously-live values than there are registers
        let count = ALLOCATABLE_REGS.len() + 3;
        let vregs: Vec<VReg> = (0..count).map(|_| mir.alloc_vreg()).collect();

        define_loaded_values(&mut mir, &vregs);

        // Add using potentially spilled operands
        mir.push(Aarch64Inst::AddRR {
            dst: Operand::Virtual(vregs[0]),
            src1: Operand::Virtual(vregs[count - 2]),
            src2: Operand::Virtual(vregs[count - 1]),
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
    fn test_uxtw_spilled_source_reloads_before_move() {
        let mut mir = Aarch64Mir::new();
        let count = ALLOCATABLE_REGS.len() + 2;
        let vregs: Vec<VReg> = (0..count).map(|_| mir.alloc_vreg()).collect();
        define_loaded_values(&mut mir, &vregs);
        mir.push(Aarch64Inst::Uxtw {
            dst: Operand::Virtual(vregs[0]),
            src: Operand::Virtual(vregs[count - 1]),
        });
        for &vreg in &vregs[1..] {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }
        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();
        assert_eq!(num_spills, 2);
        let index = mir
            .instructions()
            .iter()
            .position(|inst| matches!(inst, Aarch64Inst::Uxtw { .. }))
            .expect("rewritten Uxtw should be present");
        assert!(matches!(
            mir.instructions().get(index.checked_sub(1).unwrap()),
            Some(Aarch64Inst::Ldr {
                dst: Operand::Physical(dst),
                base: Reg::Fp,
                ..
            }) if *dst == SCRATCH_SOURCE_A
        ));
        assert!(matches!(
            mir.instructions().get(index),
            Some(Aarch64Inst::Uxtw {
                dst: Operand::Physical(dst),
                src: Operand::Physical(src),
            }) if *src == SCRATCH_SOURCE_A && *dst != *src
        ));
        assert!(!matches!(
            mir.instructions().get(index + 1),
            Some(Aarch64Inst::Str { .. })
        ));
    }

    #[test]
    fn test_uxtw_spilled_destination_stores_after_move() {
        let mut mir = Aarch64Mir::new();
        let count = ALLOCATABLE_REGS.len() + 2;
        let vregs: Vec<VReg> = (0..count).map(|_| mir.alloc_vreg()).collect();
        define_loaded_values(&mut mir, &vregs);
        mir.push(Aarch64Inst::Uxtw {
            dst: Operand::Virtual(vregs[count - 1]),
            src: Operand::Virtual(vregs[count - 2]),
        });
        for &vreg in &vregs[1..] {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }
        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();
        assert_eq!(num_spills, 1);
        let index = mir
            .instructions()
            .iter()
            .position(|inst| matches!(inst, Aarch64Inst::Uxtw { .. }))
            .expect("rewritten Uxtw should be present");
        assert!(matches!(
            mir.instructions().get(index),
            Some(Aarch64Inst::Uxtw {
                dst: Operand::Physical(dst),
                src: Operand::Physical(src),
            }) if *dst == SCRATCH_VALUE && *src != *dst
        ));
        assert!(matches!(
            mir.instructions().get(index + 1),
            Some(Aarch64Inst::Str {
                base: Reg::Fp,
                src: Operand::Physical(src),
                ..
            }) if *src == SCRATCH_VALUE
        ));
    }

    #[test]
    fn test_uxtw_both_spilled_reloads_then_stores() {
        let mut mir = Aarch64Mir::new();
        let count = ALLOCATABLE_REGS.len() + 3;
        let vregs: Vec<VReg> = (0..count).map(|_| mir.alloc_vreg()).collect();
        define_loaded_values(&mut mir, &vregs);
        mir.push(Aarch64Inst::Uxtw {
            dst: Operand::Virtual(vregs[count - 1]),
            src: Operand::Virtual(vregs[count - 2]),
        });
        for &vreg in &vregs[1..] {
            mir.push(Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Virtual(vreg),
            });
        }
        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills().unwrap();
        assert_eq!(num_spills, 2);
        let index = mir
            .instructions()
            .iter()
            .position(|inst| matches!(inst, Aarch64Inst::Uxtw { .. }))
            .expect("rewritten Uxtw should be present");
        assert!(matches!(
            mir.instructions().get(index.checked_sub(1).unwrap()),
            Some(Aarch64Inst::Ldr {
                dst: Operand::Physical(dst),
                base: Reg::Fp,
                ..
            }) if *dst == SCRATCH_SOURCE_A
        ));
        assert!(matches!(
            mir.instructions().get(index),
            Some(Aarch64Inst::Uxtw {
                dst: Operand::Physical(dst),
                src: Operand::Physical(src),
            }) if *dst == SCRATCH_VALUE && *src == SCRATCH_SOURCE_A
        ));
        assert!(matches!(
            mir.instructions().get(index + 1),
            Some(Aarch64Inst::Str {
                base: Reg::Fp,
                src: Operand::Physical(src),
                ..
            }) if *src == SCRATCH_VALUE
        ));
    }

    #[test]
    fn test_uxtw_preserves_src_dst_identity() {
        let mut mir = Aarch64Mir::new();
        let vreg = mir.alloc_vreg();
        mir.push(Aarch64Inst::Ldr {
            dst: Operand::Virtual(vreg),
            base: Reg::X1,
            offset: 0,
        });
        mir.push(Aarch64Inst::Uxtw {
            dst: Operand::Virtual(vreg),
            src: Operand::Virtual(vreg),
        });
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Virtual(vreg),
        });
        let mir = RegAlloc::new(mir, 0).allocate().unwrap();
        let inst = mir
            .instructions()
            .iter()
            .find_map(|inst| match inst {
                Aarch64Inst::Uxtw { dst, src } => Some((dst, src)),
                _ => None,
            })
            .expect("identity Uxtw should be preserved");
        assert!(
            matches!((inst.0, inst.1), (Operand::Physical(dst), Operand::Physical(src)) if dst == src)
        );
    }

    #[test]
    fn test_spilled_binary_destination_has_ordered_rewrite() {
        let mut mir = Aarch64Mir::new();
        // The first value is never used again, so `count` must exceed the
        // register count by two for the rest to outnumber the registers.
        let count = ALLOCATABLE_REGS.len() + 2;
        let vregs: Vec<VReg> = (0..count).map(|_| mir.alloc_vreg()).collect();

        define_loaded_values(&mut mir, &vregs);
        mir.push(Aarch64Inst::AddRR {
            dst: Operand::Virtual(vregs[count - 1]),
            src1: Operand::Virtual(vregs[count - 2]),
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
