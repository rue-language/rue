//! Liveness analysis for x86-64 register allocation.
//!
//! This module provides x86-64 specific instruction information for liveness analysis.
//! The actual dataflow algorithm is shared via [`crate::liveness`].

use super::mir::{Operand, Reg, ReturnBehavior, X86Inst, X86Mir};
use crate::liveness::{
    LivenessAdapter, SuccessorList, VRegList, branch_successor, conditional_successors,
    fallthrough_successor,
};
use crate::reg_class::VRegClasses;
use crate::vreg::LabelId;
use ahash::AHashMap;

// Re-export shared types from the regalloc module
pub use crate::regalloc::{InstructionLiveness, LiveRange, LivenessDebugInfo, LoopInfo};

/// Type alias for x86_64-specific liveness info.
pub type LivenessInfo = crate::regalloc::LivenessInfo<Reg>;

struct X86LivenessAdapter<'a> {
    mir: &'a X86Mir,
}

impl LivenessAdapter for X86LivenessAdapter<'_> {
    type Inst = X86Inst;
    type Reg = Reg;

    fn instructions(&self) -> &[Self::Inst] {
        self.mir.instructions()
    }

    fn vreg_count(&self) -> u32 {
        self.mir.vreg_count()
    }

    fn vreg_classes(&self) -> &VRegClasses {
        self.mir.vreg_classes()
    }

    fn label(&self, inst: &Self::Inst) -> Option<LabelId> {
        get_label(inst)
    }

    fn successors(
        &self,
        idx: usize,
        inst: &Self::Inst,
        label_to_idx: &AHashMap<LabelId, usize>,
    ) -> SuccessorList {
        get_successors(idx, inst, label_to_idx, self.instructions().len())
    }

    fn uses(&self, inst: &Self::Inst) -> VRegList {
        uses(inst)
    }

    fn defs(&self, inst: &Self::Inst) -> VRegList {
        defs(inst)
    }

    fn clobbers(&self, inst: &Self::Inst) -> &'static [Self::Reg] {
        inst.clobbers()
    }

    fn is_non_returning(&self, inst: &Self::Inst) -> bool {
        inst.is_non_returning()
    }
}

/// Compute liveness information for X86Mir.
pub fn analyze(mir: &X86Mir) -> LivenessInfo {
    crate::liveness::analyze_adapter(&X86LivenessAdapter { mir })
}

/// Compute detailed liveness debug information for X86Mir.
pub fn analyze_debug(mir: &X86Mir) -> LivenessDebugInfo {
    crate::liveness::analyze_debug_adapter(&X86LivenessAdapter { mir })
}

/// Compute allocator liveness and its diagnostic projection together.
pub(crate) fn analyze_with_debug(mir: &X86Mir) -> (LivenessInfo, LivenessDebugInfo) {
    crate::liveness::analyze_with_debug_adapter(&X86LivenessAdapter { mir })
}

/// Compute loop information for X86Mir.
pub fn analyze_loops(mir: &X86Mir) -> LoopInfo {
    crate::liveness::analyze_loops_adapter(&X86LivenessAdapter { mir })
}

// ============================================================================
// X86-64 specific instruction information
// ============================================================================

/// Get the label ID if this instruction is a label.
fn get_label(inst: &X86Inst) -> Option<LabelId> {
    match inst {
        X86Inst::Label { id } => Some(*id),
        _ => None,
    }
}

/// Get successor instruction indices for control flow analysis.
fn get_successors(
    idx: usize,
    inst: &X86Inst,
    label_to_idx: &AHashMap<LabelId, usize>,
    num_insts: usize,
) -> SuccessorList {
    match inst {
        // Unconditional jump - only successor is the target
        X86Inst::Jmp { label } => branch_successor(*label, label_to_idx),
        // Conditional branches - successor is both target and fall-through
        X86Inst::Jz { label }
        | X86Inst::Jnz { label }
        | X86Inst::Jo { label }
        | X86Inst::Jno { label }
        | X86Inst::Jb { label }
        | X86Inst::Jae { label }
        | X86Inst::Jbe { label }
        | X86Inst::Jge { label }
        | X86Inst::Jle { label } => conditional_successors(idx, *label, label_to_idx, num_insts),
        // Return and trap have no successors
        X86Inst::Ret | X86Inst::Ud2 => SuccessorList::new(),
        // A call to a helper the runtime ABI manifest declares
        // `ReturnBehavior::Never` aborts the process. Control never comes back,
        // so it has no successors and nothing is live after it (RUE-1224).
        X86Inst::CallRel {
            returns: ReturnBehavior::Never,
            ..
        } => SuccessorList::new(),
        // Ordinary calls fall through (the callee returns)
        X86Inst::CallRel { .. } => fallthrough_successor(idx, num_insts),
        // All other instructions fall through to the next
        _ => fallthrough_successor(idx, num_insts),
    }
}

/// Get virtual registers used (read) by an instruction.
pub fn uses(inst: &X86Inst) -> VRegList {
    let mut result = VRegList::new();

    let add_if_virtual = |op: &Operand, vec: &mut VRegList| {
        if let Operand::Virtual(vreg) = op {
            vec.push(*vreg);
        }
    };

    match inst {
        X86Inst::MovRI32 { .. } | X86Inst::MovRI64 { .. } => {
            // Only defines, no uses
        }
        X86Inst::MovRR { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        X86Inst::MovRM { .. } | X86Inst::NarrowLoadRM { .. } => {
            // Reads from memory (base is physical), defines dst
        }
        X86Inst::MovMR { src, .. } | X86Inst::NarrowStoreMR { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        X86Inst::AddRR { dst, src }
        | X86Inst::AddRR64 { dst, src }
        | X86Inst::SubRR { dst, src }
        | X86Inst::SubRR64 { dst, src } => {
            // dst is both read and written (dst = dst op src)
            add_if_virtual(dst, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::AddRI { dst, .. } => {
            // dst is both read and written (dst = dst + imm)
            add_if_virtual(dst, &mut result);
        }
        X86Inst::ImulRR { dst, src } | X86Inst::ImulRR64 { dst, src } => {
            add_if_virtual(dst, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::Neg { dst } | X86Inst::Neg64 { dst } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::XorRI { dst, .. } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::AndRR { dst, src }
        | X86Inst::OrRR { dst, src }
        | X86Inst::XorRR { dst, src }
        | X86Inst::And64RR { dst, src }
        | X86Inst::Or64RR { dst, src }
        | X86Inst::Xor64RR { dst, src } => {
            add_if_virtual(dst, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::NotR { dst } | X86Inst::Not64R { dst } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::ShlRCl { dst }
        | X86Inst::Shl32RCl { dst }
        | X86Inst::ShrRCl { dst }
        | X86Inst::Shr32RCl { dst }
        | X86Inst::SarRCl { dst }
        | X86Inst::Sar32RCl { dst } => {
            // dst is both read and written, CL is implicit physical register
            add_if_virtual(dst, &mut result);
        }
        X86Inst::ShlRI { dst, .. }
        | X86Inst::Shl32RI { dst, .. }
        | X86Inst::ShrRI { dst, .. }
        | X86Inst::Shr32RI { dst, .. }
        | X86Inst::SarRI { dst, .. }
        | X86Inst::Sar32RI { dst, .. } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::IdivR { src }
        | X86Inst::DivR { src }
        | X86Inst::Idiv64R { src }
        | X86Inst::Div64R { src }
        | X86Inst::MulR { src }
        | X86Inst::Mul64R { src } => {
            add_if_virtual(src, &mut result);
            // Div/Idiv also implicitly use RAX and RDX, Mul uses RAX (physical)
        }
        X86Inst::TestRR { src1, src2 } | X86Inst::Test64RR { src1, src2 } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        X86Inst::CmpRR { src1, src2 } | X86Inst::Cmp64RR { src1, src2 } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        X86Inst::CmpRI { src, .. } | X86Inst::Cmp64RI { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        X86Inst::Sete { .. }
        | X86Inst::Setne { .. }
        | X86Inst::Setl { .. }
        | X86Inst::Setg { .. }
        | X86Inst::Setle { .. }
        | X86Inst::Setge { .. }
        | X86Inst::Setb { .. }
        | X86Inst::Seta { .. }
        | X86Inst::Setbe { .. }
        | X86Inst::Setae { .. } => {
            // Only defines dst, reads flags (implicit)
        }
        X86Inst::Movzx { src, .. }
        | X86Inst::Movsx8To64 { src, .. }
        | X86Inst::Movsx16To64 { src, .. }
        | X86Inst::Movsx32To64 { src, .. }
        | X86Inst::Movzx32To64 { src, .. }
        | X86Inst::Movzx8To64 { src, .. }
        | X86Inst::Movzx16To64 { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        X86Inst::Pop { .. } => {
            // Only defines
        }
        X86Inst::Push { src } => {
            add_if_virtual(src, &mut result);
        }
        X86Inst::Lea { .. } => {
            // LEA only defines dst, it does not read dst's previous value.
            // base is a physical register (Reg), so no vreg use.
        }
        X86Inst::Shl { dst, count } => {
            add_if_virtual(dst, &mut result);
            add_if_virtual(count, &mut result);
        }
        X86Inst::MovRMIndexed { base, .. } | X86Inst::NarrowLoadIndexed { base, .. } => {
            // base is a VReg
            result.push(*base);
        }
        X86Inst::MovMRIndexed { base, src, .. } | X86Inst::NarrowStoreIndexed { base, src, .. } => {
            result.push(*base);
            add_if_virtual(src, &mut result);
        }
        X86Inst::MovRMSib { base, index, .. } => {
            // SIB load: reads base and index
            add_if_virtual(base, &mut result);
            add_if_virtual(index, &mut result);
        }
        X86Inst::MovMRSib {
            base, index, src, ..
        } => {
            // SIB store: reads base, index, and src
            add_if_virtual(base, &mut result);
            add_if_virtual(index, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::StringConstPtr { .. }
        | X86Inst::StringConstLen { .. }
        | X86Inst::StringConstCap { .. } => {
            // Only defines, no uses
        }
        X86Inst::Cdq
        | X86Inst::Cqo
        | X86Inst::Jz { .. }
        | X86Inst::Jnz { .. }
        | X86Inst::Jo { .. }
        | X86Inst::Jno { .. }
        | X86Inst::Jb { .. }
        | X86Inst::Jae { .. }
        | X86Inst::Jbe { .. }
        | X86Inst::Jge { .. }
        | X86Inst::Jle { .. }
        | X86Inst::Jmp { .. }
        | X86Inst::Label { .. }
        | X86Inst::CallRel { .. }
        | X86Inst::Syscall
        | X86Inst::Ret
        | X86Inst::Ud2 => {
            // No vreg operands
        }
    }

    result
}

/// Get virtual registers defined (written) by an instruction.
pub fn defs(inst: &X86Inst) -> VRegList {
    let mut result = VRegList::new();

    let add_if_virtual = |op: &Operand, vec: &mut VRegList| {
        if let Operand::Virtual(vreg) = op {
            vec.push(*vreg);
        }
    };

    match inst {
        X86Inst::MovRI32 { dst, .. } | X86Inst::MovRI64 { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovRR { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovRM { dst, .. } | X86Inst::NarrowLoadRM { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovMR { .. } | X86Inst::NarrowStoreMR { .. } => {
            // Writes to memory, not to a register
        }
        X86Inst::AddRR { dst, .. }
        | X86Inst::AddRR64 { dst, .. }
        | X86Inst::AddRI { dst, .. }
        | X86Inst::SubRR { dst, .. }
        | X86Inst::SubRR64 { dst, .. }
        | X86Inst::ImulRR { dst, .. }
        | X86Inst::ImulRR64 { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Neg { dst } | X86Inst::Neg64 { dst } | X86Inst::XorRI { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::AndRR { dst, .. }
        | X86Inst::OrRR { dst, .. }
        | X86Inst::XorRR { dst, .. }
        | X86Inst::And64RR { dst, .. }
        | X86Inst::Or64RR { dst, .. }
        | X86Inst::Xor64RR { dst, .. }
        | X86Inst::NotR { dst }
        | X86Inst::Not64R { dst }
        | X86Inst::ShlRCl { dst }
        | X86Inst::Shl32RCl { dst }
        | X86Inst::ShlRI { dst, .. }
        | X86Inst::Shl32RI { dst, .. }
        | X86Inst::ShrRCl { dst }
        | X86Inst::Shr32RCl { dst }
        | X86Inst::ShrRI { dst, .. }
        | X86Inst::Shr32RI { dst, .. }
        | X86Inst::SarRCl { dst }
        | X86Inst::Sar32RCl { dst }
        | X86Inst::SarRI { dst, .. }
        | X86Inst::Sar32RI { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::IdivR { .. }
        | X86Inst::DivR { .. }
        | X86Inst::Idiv64R { .. }
        | X86Inst::Div64R { .. }
        | X86Inst::MulR { .. }
        | X86Inst::Mul64R { .. } => {
            // Implicitly defines RAX (quotient / product low) and RDX
            // (remainder / product high), but those are physical
        }
        X86Inst::TestRR { .. }
        | X86Inst::Test64RR { .. }
        | X86Inst::CmpRR { .. }
        | X86Inst::Cmp64RR { .. }
        | X86Inst::CmpRI { .. }
        | X86Inst::Cmp64RI { .. } => {
            // Only sets flags, no register def
        }
        X86Inst::Sete { dst }
        | X86Inst::Setne { dst }
        | X86Inst::Setl { dst }
        | X86Inst::Setg { dst }
        | X86Inst::Setle { dst }
        | X86Inst::Setge { dst }
        | X86Inst::Setb { dst }
        | X86Inst::Seta { dst }
        | X86Inst::Setbe { dst }
        | X86Inst::Setae { dst } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Movzx { dst, .. }
        | X86Inst::Movsx8To64 { dst, .. }
        | X86Inst::Movsx16To64 { dst, .. }
        | X86Inst::Movsx32To64 { dst, .. }
        | X86Inst::Movzx32To64 { dst, .. }
        | X86Inst::Movzx8To64 { dst, .. }
        | X86Inst::Movzx16To64 { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Pop { dst } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Push { .. } => {
            // Only reads, no definition
        }
        X86Inst::Lea { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Shl { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovRMIndexed { dst, .. } | X86Inst::NarrowLoadIndexed { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovMRIndexed { .. } | X86Inst::NarrowStoreIndexed { .. } => {
            // Writes to memory
        }
        X86Inst::MovRMSib { dst, .. } => {
            // SIB load defines dst
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovMRSib { .. } => {
            // SIB store writes to memory, no register def
        }
        X86Inst::StringConstPtr { dst, .. }
        | X86Inst::StringConstLen { dst, .. }
        | X86Inst::StringConstCap { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Cdq
        | X86Inst::Cqo
        | X86Inst::Jz { .. }
        | X86Inst::Jnz { .. }
        | X86Inst::Jo { .. }
        | X86Inst::Jno { .. }
        | X86Inst::Jb { .. }
        | X86Inst::Jae { .. }
        | X86Inst::Jbe { .. }
        | X86Inst::Jge { .. }
        | X86Inst::Jle { .. }
        | X86Inst::Jmp { .. }
        | X86Inst::Label { .. }
        | X86Inst::CallRel { .. }
        | X86Inst::Syscall
        | X86Inst::Ret
        | X86Inst::Ud2 => {
            // No vreg operands
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clobbers_are_borrowed_static_slices() {
        let mir = X86Mir::new();
        let adapter = X86LivenessAdapter { mir: &mir };
        let empty = X86Inst::Ret;
        let call = X86Inst::call(7);
        let div = X86Inst::IdivR {
            src: Operand::Physical(Reg::Rcx),
        };
        let syscall = X86Inst::Syscall;

        // This compile-time signature check protects the ownership contract:
        // the adapter cannot satisfy it by returning a temporary Vec.
        fn require_static(_: &'static [Reg]) {}
        require_static(adapter.clobbers(&empty));
        require_static(adapter.clobbers(&call));
        require_static(adapter.clobbers(&div));
        require_static(adapter.clobbers(&syscall));

        assert!(adapter.clobbers(&empty).is_empty());
        let call_clobbers = adapter.clobbers(&call);
        assert_eq!(
            call_clobbers,
            &[
                Reg::Rax,
                Reg::Rcx,
                Reg::Rdx,
                Reg::Rsi,
                Reg::Rdi,
                Reg::R8,
                Reg::R9,
                Reg::R10,
                Reg::R11,
            ]
        );
        assert!(!call_clobbers.contains(&Reg::Rsp));
        assert!(!call_clobbers.contains(&Reg::Rbp));
        assert!(!call_clobbers.contains(&Reg::R12));
        assert_eq!(adapter.clobbers(&div), &[Reg::Rax, Reg::Rdx]);
        let syscall_clobbers = adapter.clobbers(&syscall);
        assert_eq!(syscall_clobbers, &[Reg::Rax, Reg::Rcx, Reg::R11]);
        assert!(!syscall_clobbers.contains(&Reg::Rsp));
        assert!(!syscall_clobbers.contains(&Reg::Rbp));
        assert!(!syscall_clobbers.contains(&Reg::R12));

        let mut mir = X86Mir::new();
        mir.push(empty);
        mir.push(call);
        mir.push(div);
        mir.push(syscall);
        let info = analyze(&mir);
        assert!(info.clobbers_at(0).is_empty());
        assert!(std::ptr::eq(
            info.clobbers_at(1),
            mir.instructions()[1].clobbers()
        ));
        assert!(std::ptr::eq(
            info.clobbers_at(2),
            mir.instructions()[2].clobbers()
        ));
        assert!(std::ptr::eq(
            info.clobbers_at(3),
            mir.instructions()[3].clobbers()
        ));
    }

    #[test]
    fn test_simple_liveness() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();

        // v0 = 42
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 42,
        });
        // v1 = v0
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        let info = analyze(&mir);

        // v0 is defined at 0, last used at 1
        assert_eq!(info.range(v0), Some(&LiveRange::new(0, 1)));
        // v1 is defined at 1, last used at 1 (no further use)
        assert_eq!(info.range(v1), Some(&LiveRange::new(1, 1)));
    }

    #[test]
    fn test_overlapping_ranges() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let v2 = mir.alloc_vreg();

        // v0 = 1
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 1,
        });
        // v1 = 2
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v1),
            imm: 2,
        });
        // v2 = v0
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v2),
            src: Operand::Virtual(v0),
        });
        // v2 += v1
        mir.push(X86Inst::AddRR {
            dst: Operand::Virtual(v2),
            src: Operand::Virtual(v1),
        });

        let info = analyze(&mir);

        // v0 and v1 should interfere (both live at instruction 2)
        assert!(info.interferes(v0, v1));
    }

    #[test]
    fn test_non_overlapping_ranges() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();

        // v0 = 1
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 1,
        });
        // (v0 is dead after this, not used again)
        // v1 = 2
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v1),
            imm: 2,
        });

        let info = analyze(&mir);

        // v0 and v1 don't interfere (v0 is not used after being defined)
        assert!(!info.interferes(v0, v1));
    }

    #[test]
    fn test_empty_mir() {
        let mir = X86Mir::new();
        let info = analyze(&mir);

        assert!(info.ranges.is_empty());
        assert!(info.clobbers_at.is_empty());
        assert_eq!(info.instruction_count(), 0);
    }

    #[test]
    fn test_liveness_across_branch() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let label_else = mir.alloc_label();
        let label_end = mir.alloc_label();

        // v0 = 1
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 1,
        });

        // cmp v0, 0
        mir.push(X86Inst::CmpRI {
            src: Operand::Virtual(v0),
            imm: 0,
        });

        // jz label_else
        mir.push(X86Inst::Jz { label: label_else });

        // v1 = v0 (then branch)
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        // jmp label_end
        mir.push(X86Inst::Jmp { label: label_end });

        // label_else:
        mir.push(X86Inst::Label { id: label_else });

        // v1 = 2 (else branch)
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v1),
            imm: 2,
        });

        // label_end:
        mir.push(X86Inst::Label { id: label_end });

        // Use v1 at the end
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Virtual(v1),
        });

        mir.push(X86Inst::Ret);

        let info = analyze(&mir);

        // v0 should be live from definition (0) through use in CMP (1) and MOV (3)
        let v0_range = info.range(v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0);
        assert!(
            v0_range.end >= 3,
            "v0 should be live through its last use at instruction 3"
        );

        // v1 should be live from first definition through final use
        let v1_range = info.range(v1).expect("v1 should have a range");
        assert!(v1_range.end >= 8, "v1 should be live until the MOV to RAX");
    }

    #[test]
    fn test_liveness_with_conditional_branch() {
        // Test Jnz handling
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let skip_label = mir.alloc_label();

        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 42,
        });

        mir.push(X86Inst::CmpRI {
            src: Operand::Virtual(v0),
            imm: 0,
        });

        mir.push(X86Inst::Jnz { label: skip_label });

        // v0 is used after the conditional branch
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Virtual(v0),
        });

        mir.push(X86Inst::Label { id: skip_label });

        mir.push(X86Inst::Ret);

        let info = analyze(&mir);

        // v0 should be live from 0 through at least instruction 3 (use in MOV)
        let v0_range = info.range(v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0);
        assert!(v0_range.end >= 3);
    }

    #[test]
    fn test_liveness_loop_pattern() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let loop_label = mir.alloc_label();

        // v0 = 10
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 10,
        });

        // loop:
        mir.push(X86Inst::Label { id: loop_label });

        // v1 = v0
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        // v0 = v0 - 1
        mir.push(X86Inst::AddRI {
            dst: Operand::Virtual(v0),
            imm: -1,
        });

        // cmp v0, 0
        mir.push(X86Inst::CmpRI {
            src: Operand::Virtual(v0),
            imm: 0,
        });

        // jnz loop
        mir.push(X86Inst::Jnz { label: loop_label });

        mir.push(X86Inst::Ret);

        let info = analyze(&mir);

        // v0 should be live throughout the loop (from def to last use in CMP)
        let v0_range = info.range(v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0);
        assert!(v0_range.end >= 4, "v0 should be live through CMP");
    }

    #[test]
    fn test_ud2_is_terminal() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let label = mir.alloc_label();

        // v0 = 42
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 42,
        });

        // ud2 (unconditional trap; execution never falls through)
        mir.push(X86Inst::Ud2);

        // label:
        mir.push(X86Inst::Label { id: label });

        // Use v0 in the next layout block
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Virtual(v0),
        });

        mir.push(X86Inst::Ret);

        let debug = analyze_debug(&mir);

        // The use of v0 after the label must not flow backward through UD2.
        let ud2 = &debug.instructions[1];
        assert!(
            ud2.live_out.is_empty(),
            "UD2 is terminal and must have an empty live-out set; found: {:?}",
            ud2.live_out
        );
    }

    #[test]
    fn test_lea_only_defines_dst() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();

        // lea v0, [rbp-8]
        mir.push(X86Inst::Lea {
            dst: Operand::Virtual(v0),
            base: Reg::Rbp,
            disp: -8,
        });

        let info = analyze(&mir);

        // v0 should have a range starting at 0 (where it's defined)
        let v0_range = info.range(v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0, "LEA defines v0 at instruction 0");

        // The instruction's uses should NOT include v0
        let inst_uses = uses(&mir.instructions()[0]);
        assert!(
            !inst_uses.contains(&v0),
            "LEA should not list dst in uses(); found uses: {:?}",
            inst_uses
        );
    }
}
