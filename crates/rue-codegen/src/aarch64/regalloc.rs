//! Register allocation for AArch64.
//!
//! This module allocates physical registers to virtual registers using
//! liveness analysis and linear scan allocation.

use std::collections::HashSet;

use super::liveness::{self, LiveRange, LivenessInfo};
use super::mir::{Aarch64Inst, Aarch64Mir, Operand, Reg, VReg};

/// Available registers for allocation.
///
/// We use callee-saved registers (X19-X28) for general allocation.
/// This ensures values survive across function calls.
///
/// We avoid:
/// - X0-X7: Argument/return registers
/// - X8: Indirect result location
/// - X9-X15: Caller-saved temporaries (but X9 used as scratch)
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

/// Allocation result for a virtual register.
#[derive(Debug, Clone, Copy)]
enum Allocation {
    /// Allocated to a physical register.
    Register(Reg),
    /// Spilled to a stack slot (offset from FP).
    Spill(i32),
}

/// Register allocator for AArch64.
pub struct RegAlloc {
    mir: Aarch64Mir,
    allocation: Vec<Option<Allocation>>,
    liveness: LivenessInfo,
    next_spill_offset: i32,
    num_spills: u32,
    used_callee_saved: Vec<Reg>,
}

impl RegAlloc {
    /// Create a new register allocator.
    pub fn new(mir: Aarch64Mir, existing_locals: u32) -> Self {
        let vreg_count = mir.vreg_count() as usize;
        let liveness = liveness::analyze(&mir);
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
    pub fn allocate(mut self) -> Aarch64Mir {
        self.assign_registers();
        self.rewrite_instructions();
        self.mir
    }

    /// Perform register allocation and return the MIR, spill count, and used callee-saved registers.
    pub fn allocate_with_spills(mut self) -> (Aarch64Mir, u32, Vec<Reg>) {
        self.assign_registers();
        self.rewrite_instructions();
        let num_spills = self.num_spills;
        let used_callee_saved = self.used_callee_saved;
        (self.mir, num_spills, used_callee_saved)
    }

    /// Assign physical registers to all virtual registers using linear scan.
    fn assign_registers(&mut self) {
        let mut vregs_by_start: Vec<(VReg, LiveRange)> = Vec::new();
        for vreg_idx in 0..self.mir.vreg_count() {
            let vreg = VReg::new(vreg_idx);
            if let Some(&range) = self.liveness.range(vreg) {
                vregs_by_start.push((vreg, range));
            }
        }
        vregs_by_start.sort_by_key(|(_, range)| range.start);

        let mut active: Vec<(VReg, Reg, usize)> = Vec::new();

        for (vreg, range) in vregs_by_start {
            active.retain(|&(_, _, end)| end >= range.start);

            let used_regs: HashSet<Reg> = active.iter().map(|&(_, reg, _)| reg).collect();

            let mut allocated_reg = None;
            for &reg in ALLOCATABLE_REGS {
                if !used_regs.contains(&reg) {
                    allocated_reg = Some(reg);
                    break;
                }
            }

            if let Some(reg) = allocated_reg {
                self.allocation[vreg.index() as usize] = Some(Allocation::Register(reg));
                active.push((vreg, reg, range.end));
                if !self.used_callee_saved.contains(&reg) {
                    self.used_callee_saved.push(reg);
                }
            } else {
                let mut longest_idx = None;
                let mut longest_end = range.end;
                for (i, &(_, _, end)) in active.iter().enumerate() {
                    if end > longest_end {
                        longest_end = end;
                        longest_idx = Some(i);
                    }
                }

                if let Some(idx) = longest_idx {
                    let (spilled_vreg, freed_reg, _) = active.remove(idx);
                    let spill_offset = self.alloc_spill_slot();
                    self.allocation[spilled_vreg.index() as usize] =
                        Some(Allocation::Spill(spill_offset));
                    self.allocation[vreg.index() as usize] = Some(Allocation::Register(freed_reg));
                    active.push((vreg, freed_reg, range.end));
                } else {
                    let spill_offset = self.alloc_spill_slot();
                    self.allocation[vreg.index() as usize] = Some(Allocation::Spill(spill_offset));
                }
            }
        }
    }

    fn alloc_spill_slot(&mut self) -> i32 {
        let offset = self.next_spill_offset;
        self.next_spill_offset -= 8;
        self.num_spills += 1;
        offset
    }

    fn rewrite_instructions(&mut self) {
        let old_instructions = std::mem::take(&mut self.mir).into_instructions();
        let mut new_mir = Aarch64Mir::new();

        for inst in old_instructions {
            self.rewrite_inst(&mut new_mir, inst);
        }

        self.mir = new_mir;
    }

    fn rewrite_inst(&self, mir: &mut Aarch64Mir, inst: Aarch64Inst) {
        match inst {
            Aarch64Inst::MovImm { dst, imm } => {
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(Aarch64Inst::MovImm {
                            dst: Operand::Physical(reg),
                            imm,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        // Use X9 as scratch
                        mir.push(Aarch64Inst::MovImm {
                            dst: Operand::Physical(Reg::X9),
                            imm,
                        });
                        mir.push(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    None => {
                        mir.push(Aarch64Inst::MovImm { dst, imm });
                    }
                }
            }

            Aarch64Inst::MovRR { dst, src } => {
                let src_op = self.load_operand(mir, src, Reg::X9);
                let dst_alloc = self.get_allocation(dst);

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
                        mir.push(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    None => {
                        mir.push(Aarch64Inst::MovRR { dst, src: src_op });
                    }
                }
            }

            Aarch64Inst::Ldr { dst, base, offset } => match self.get_allocation(dst) {
                Some(Allocation::Register(reg)) => {
                    mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Physical(reg),
                        base,
                        offset,
                    });
                }
                Some(Allocation::Spill(spill_offset)) => {
                    mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Physical(Reg::X9),
                        base,
                        offset,
                    });
                    mir.push(Aarch64Inst::Str {
                        src: Operand::Physical(Reg::X9),
                        base: Reg::Fp,
                        offset: spill_offset,
                    });
                }
                None => {
                    mir.push(Aarch64Inst::Ldr { dst, base, offset });
                }
            },

            Aarch64Inst::Str { src, base, offset } => {
                let src_op = self.load_operand(mir, src, Reg::X9);
                mir.push(Aarch64Inst::Str {
                    src: src_op,
                    base,
                    offset,
                });
            }

            Aarch64Inst::AddRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::AddRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::AddsRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::AddsRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::AddsRR64 { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::AddsRR64 {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::AddImm { dst, src, imm } => {
                self.emit_binop_imm(mir, dst, src, imm, |d, s, i| Aarch64Inst::AddImm {
                    dst: d,
                    src: s,
                    imm: i,
                });
            }

            Aarch64Inst::SubRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::SubRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::SubsRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::SubsRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::SubsRR64 { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::SubsRR64 {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::SubImm { dst, src, imm } => {
                self.emit_binop_imm(mir, dst, src, imm, |d, s, i| Aarch64Inst::SubImm {
                    dst: d,
                    src: s,
                    imm: i,
                });
            }

            Aarch64Inst::MulRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::MulRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::SmullRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::SmullRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::UmullRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::UmullRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::SmulhRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::SmulhRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::UmulhRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::UmulhRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::Lsr64Imm { dst, src, imm } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Lsr64Imm {
                    dst: d,
                    src: s,
                    imm,
                });
            }

            Aarch64Inst::Asr64Imm { dst, src, imm } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Asr64Imm {
                    dst: d,
                    src: s,
                    imm,
                });
            }

            Aarch64Inst::SdivRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::SdivRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::Msub {
                dst,
                src1,
                src2,
                src3,
            } => {
                // Use X10, X11, X12 for sources to avoid conflict with X9 used for spilled dst.
                // X9 is reserved for the destination when it's spilled.
                let src1_op = self.load_operand(mir, src1, Reg::X10);
                let src2_op = self.load_operand(mir, src2, Reg::X11);
                let src3_op = self.load_operand(mir, src3, Reg::X12);
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(Aarch64Inst::Msub {
                            dst: Operand::Physical(reg),
                            src1: src1_op,
                            src2: src2_op,
                            src3: src3_op,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(Aarch64Inst::Msub {
                            dst: Operand::Physical(Reg::X9),
                            src1: src1_op,
                            src2: src2_op,
                            src3: src3_op,
                        });
                        mir.push(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    None => {
                        mir.push(Aarch64Inst::Msub {
                            dst,
                            src1: src1_op,
                            src2: src2_op,
                            src3: src3_op,
                        });
                    }
                }
            }

            Aarch64Inst::Neg { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Neg { dst: d, src: s });
            }

            Aarch64Inst::Negs { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Negs { dst: d, src: s });
            }

            Aarch64Inst::Negs32 { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Negs32 { dst: d, src: s });
            }

            Aarch64Inst::AndRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::AndRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::OrrRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::OrrRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::EorRR { dst, src1, src2 } => {
                self.emit_ternop(mir, dst, src1, src2, |d, s1, s2| Aarch64Inst::EorRR {
                    dst: d,
                    src1: s1,
                    src2: s2,
                });
            }

            Aarch64Inst::EorImm { dst, src, imm } => {
                let src_op = self.load_operand(mir, src, Reg::X10);
                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(Aarch64Inst::EorImm {
                            dst: Operand::Physical(reg),
                            src: src_op,
                            imm,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(Aarch64Inst::EorImm {
                            dst: Operand::Physical(Reg::X9),
                            src: src_op,
                            imm,
                        });
                        mir.push(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    None => {
                        mir.push(Aarch64Inst::EorImm {
                            dst,
                            src: src_op,
                            imm,
                        });
                    }
                }
            }

            Aarch64Inst::CmpRR { src1, src2 } => {
                let src1_op = self.load_operand(mir, src1, Reg::X9);
                let src2_op = self.load_operand(mir, src2, Reg::X10);
                mir.push(Aarch64Inst::CmpRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            Aarch64Inst::Cmp64RR { src1, src2 } => {
                let src1_op = self.load_operand(mir, src1, Reg::X9);
                let src2_op = self.load_operand(mir, src2, Reg::X10);
                mir.push(Aarch64Inst::Cmp64RR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            Aarch64Inst::CmpImm { src, imm } => {
                let src_op = self.load_operand(mir, src, Reg::X9);
                mir.push(Aarch64Inst::CmpImm { src: src_op, imm });
            }

            Aarch64Inst::Cbz { src, label } => {
                let src_op = self.load_operand(mir, src, Reg::X9);
                mir.push(Aarch64Inst::Cbz { src: src_op, label });
            }

            Aarch64Inst::Cbnz { src, label } => {
                let src_op = self.load_operand(mir, src, Reg::X9);
                mir.push(Aarch64Inst::Cbnz { src: src_op, label });
            }

            Aarch64Inst::Cset { dst, cond } => match self.get_allocation(dst) {
                Some(Allocation::Register(reg)) => {
                    mir.push(Aarch64Inst::Cset {
                        dst: Operand::Physical(reg),
                        cond,
                    });
                }
                Some(Allocation::Spill(offset)) => {
                    mir.push(Aarch64Inst::Cset {
                        dst: Operand::Physical(Reg::X9),
                        cond,
                    });
                    mir.push(Aarch64Inst::Str {
                        src: Operand::Physical(Reg::X9),
                        base: Reg::Fp,
                        offset,
                    });
                }
                None => {
                    mir.push(Aarch64Inst::Cset { dst, cond });
                }
            },

            Aarch64Inst::TstRR { src1, src2 } => {
                let src1_op = self.load_operand(mir, src1, Reg::X9);
                let src2_op = self.load_operand(mir, src2, Reg::X10);
                mir.push(Aarch64Inst::TstRR {
                    src1: src1_op,
                    src2: src2_op,
                });
            }

            Aarch64Inst::Sxtb { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Sxtb { dst: d, src: s });
            }

            Aarch64Inst::Sxth { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Sxth { dst: d, src: s });
            }

            Aarch64Inst::Sxtw { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Sxtw { dst: d, src: s });
            }

            Aarch64Inst::Uxtb { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Uxtb { dst: d, src: s });
            }

            Aarch64Inst::Uxth { dst, src } => {
                self.emit_binop(mir, dst, src, |d, s| Aarch64Inst::Uxth { dst: d, src: s });
            }

            Aarch64Inst::StpPre { src1, src2, offset } => {
                let src1_op = self.load_operand(mir, src1, Reg::X9);
                let src2_op = self.load_operand(mir, src2, Reg::X10);
                mir.push(Aarch64Inst::StpPre {
                    src1: src1_op,
                    src2: src2_op,
                    offset,
                });
            }

            Aarch64Inst::LdpPost { dst1, dst2, offset } => {
                // LDP only defines, doesn't read vregs
                let dst1_phys = match self.get_allocation(dst1) {
                    Some(Allocation::Register(reg)) => Operand::Physical(reg),
                    Some(Allocation::Spill(_)) => Operand::Physical(Reg::X9),
                    None => dst1,
                };
                let dst2_phys = match self.get_allocation(dst2) {
                    Some(Allocation::Register(reg)) => Operand::Physical(reg),
                    Some(Allocation::Spill(_)) => Operand::Physical(Reg::X10),
                    None => dst2,
                };
                mir.push(Aarch64Inst::LdpPost {
                    dst1: dst1_phys,
                    dst2: dst2_phys,
                    offset,
                });
                // Handle spills
                if let Some(Allocation::Spill(off)) = self.get_allocation(dst1) {
                    mir.push(Aarch64Inst::Str {
                        src: Operand::Physical(Reg::X9),
                        base: Reg::Fp,
                        offset: off,
                    });
                }
                if let Some(Allocation::Spill(off)) = self.get_allocation(dst2) {
                    mir.push(Aarch64Inst::Str {
                        src: Operand::Physical(Reg::X10),
                        base: Reg::Fp,
                        offset: off,
                    });
                }
            }

            Aarch64Inst::LdrIndexed { dst, base } => {
                // Load base vreg into scratch, then emit load with the result allocation
                let base_op = Operand::Virtual(base);
                let base_reg = self.load_operand(mir, base_op, Reg::X9);
                let base_phys = match base_reg {
                    Operand::Physical(r) => r,
                    _ => Reg::X9,
                };

                match self.get_allocation(dst) {
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
                        mir.push(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X10),
                            base: Reg::Fp,
                            offset,
                        });
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
                let src_op = self.load_operand(mir, src, Reg::X9);
                let base_vreg_op = Operand::Virtual(base);
                let base_reg = self.load_operand(mir, base_vreg_op, Reg::X10);
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

            Aarch64Inst::LslImm { dst, src, imm } => {
                let src_op = self.load_operand(mir, src, Reg::X10);

                match self.get_allocation(dst) {
                    Some(Allocation::Register(reg)) => {
                        mir.push(Aarch64Inst::LslImm {
                            dst: Operand::Physical(reg),
                            src: src_op,
                            imm,
                        });
                    }
                    Some(Allocation::Spill(offset)) => {
                        mir.push(Aarch64Inst::LslImm {
                            dst: Operand::Physical(Reg::X9),
                            src: src_op,
                            imm,
                        });
                        mir.push(Aarch64Inst::Str {
                            src: Operand::Physical(Reg::X9),
                            base: Reg::Fp,
                            offset,
                        });
                    }
                    None => {
                        mir.push(Aarch64Inst::LslImm {
                            dst,
                            src: src_op,
                            imm,
                        });
                    }
                }
            }

            // Pass-through instructions
            Aarch64Inst::B { label } => mir.push(Aarch64Inst::B { label }),
            Aarch64Inst::BCond { cond, label } => mir.push(Aarch64Inst::BCond { cond, label }),
            Aarch64Inst::Bvs { label } => mir.push(Aarch64Inst::Bvs { label }),
            Aarch64Inst::Bvc { label } => mir.push(Aarch64Inst::Bvc { label }),
            Aarch64Inst::Label { id } => mir.push(Aarch64Inst::Label { id }),
            Aarch64Inst::Bl { symbol } => mir.push(Aarch64Inst::Bl { symbol }),
            Aarch64Inst::Ret => mir.push(Aarch64Inst::Ret),
        }
    }

    fn get_allocation(&self, operand: Operand) -> Option<Allocation> {
        match operand {
            Operand::Virtual(vreg) => self.allocation[vreg.index() as usize],
            Operand::Physical(_) => None,
        }
    }

    fn load_operand(&self, mir: &mut Aarch64Mir, operand: Operand, scratch: Reg) -> Operand {
        match operand {
            Operand::Virtual(vreg) => match self.allocation[vreg.index() as usize] {
                Some(Allocation::Register(reg)) => Operand::Physical(reg),
                Some(Allocation::Spill(offset)) => {
                    mir.push(Aarch64Inst::Ldr {
                        dst: Operand::Physical(scratch),
                        base: Reg::Fp,
                        offset,
                    });
                    Operand::Physical(scratch)
                }
                None => panic!("vreg {} not allocated", vreg.index()),
            },
            Operand::Physical(reg) => Operand::Physical(reg),
        }
    }

    fn emit_binop<F>(&self, mir: &mut Aarch64Mir, dst: Operand, src: Operand, make_inst: F)
    where
        F: FnOnce(Operand, Operand) -> Aarch64Inst,
    {
        let src_op = self.load_operand(mir, src, Reg::X10);
        match self.get_allocation(dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src_op));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(make_inst(Operand::Physical(Reg::X9), src_op));
                mir.push(Aarch64Inst::Str {
                    src: Operand::Physical(Reg::X9),
                    base: Reg::Fp,
                    offset,
                });
            }
            None => {
                mir.push(make_inst(dst, src_op));
            }
        }
    }

    fn emit_ternop<F>(
        &self,
        mir: &mut Aarch64Mir,
        dst: Operand,
        src1: Operand,
        src2: Operand,
        make_inst: F,
    ) where
        F: FnOnce(Operand, Operand, Operand) -> Aarch64Inst,
    {
        let src1_op = self.load_operand(mir, src1, Reg::X10);
        let src2_op = self.load_operand(mir, src2, Reg::X11);
        match self.get_allocation(dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src1_op, src2_op));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(make_inst(Operand::Physical(Reg::X9), src1_op, src2_op));
                mir.push(Aarch64Inst::Str {
                    src: Operand::Physical(Reg::X9),
                    base: Reg::Fp,
                    offset,
                });
            }
            None => {
                mir.push(make_inst(dst, src1_op, src2_op));
            }
        }
    }

    fn emit_binop_imm<F>(
        &self,
        mir: &mut Aarch64Mir,
        dst: Operand,
        src: Operand,
        imm: i32,
        make_inst: F,
    ) where
        F: FnOnce(Operand, Operand, i32) -> Aarch64Inst,
    {
        let src_op = self.load_operand(mir, src, Reg::X10);
        match self.get_allocation(dst) {
            Some(Allocation::Register(reg)) => {
                mir.push(make_inst(Operand::Physical(reg), src_op, imm));
            }
            Some(Allocation::Spill(offset)) => {
                mir.push(make_inst(Operand::Physical(Reg::X9), src_op, imm));
                mir.push(Aarch64Inst::Str {
                    src: Operand::Physical(Reg::X9),
                    base: Reg::Fp,
                    offset,
                });
            }
            None => {
                mir.push(make_inst(dst, src_op, imm));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_allocation() {
        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v0),
            imm: 42,
        });

        let mir = RegAlloc::new(mir, 0).allocate();

        match &mir.instructions()[0] {
            Aarch64Inst::MovImm { dst, imm } => {
                assert_eq!(*dst, Operand::Physical(Reg::X19));
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

        let mir = RegAlloc::new(mir, 0).allocate();

        match &mir.instructions()[0] {
            Aarch64Inst::MovImm { dst, imm } => {
                assert_eq!(*dst, Operand::Physical(Reg::X0));
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
        let result = RegAlloc::new(mir, 0).allocate();

        // Verify the Msub instruction was generated
        let has_msub = result
            .instructions()
            .iter()
            .any(|inst| matches!(inst, Aarch64Inst::Msub { .. }));
        assert!(
            has_msub,
            "MSUB instruction should be present after allocation"
        );
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

        let mir = RegAlloc::new(mir, 0).allocate();

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

        let (mir, num_spills, _) = RegAlloc::new(mir, 0).allocate_with_spills();

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
}
