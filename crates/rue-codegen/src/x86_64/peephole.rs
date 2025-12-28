//! Peephole optimization pass for x86-64.
//!
//! This pass runs after register allocation and removes redundant instructions
//! such as:
//! - `mov r, r` (no-op moves where src == dst)
//! - `add r, 0` / `sub r, 0` (identity arithmetic)
//!
//! The pass operates in-place on the instruction vector for efficiency.

use super::mir::{Operand, X86Inst};

/// Apply peephole optimizations to the instruction stream.
///
/// This modifies the vector in place, removing redundant instructions.
/// Returns the number of instructions removed.
pub fn optimize(instructions: &mut Vec<X86Inst>) -> usize {
    let original_count = instructions.len();

    instructions.retain(|inst| !is_redundant(inst));

    original_count - instructions.len()
}

/// Check if an instruction is redundant and can be removed.
fn is_redundant(inst: &X86Inst) -> bool {
    match inst {
        // mov r, r where src == dst is a no-op
        X86Inst::MovRR { dst, src } => operands_equal(dst, src),

        // add r, 0 is identity
        X86Inst::AddRI { imm: 0, .. } => true,

        // Other identity patterns could be added here in the future:
        // - sub r, 0 (would need SubRI instruction)
        // - xor r, 0 (would need XorRI with 0)
        // - and r, -1 (would need AndRI)
        // - or r, 0 (would need OrRI)
        _ => false,
    }
}

/// Check if two operands refer to the same physical register.
///
/// This only works correctly after register allocation, when all operands
/// are physical registers.
fn operands_equal(a: &Operand, b: &Operand) -> bool {
    match (a, b) {
        (Operand::Physical(ra), Operand::Physical(rb)) => ra == rb,
        // Virtual registers are not compared - peephole runs after regalloc
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86_64::mir::Reg;

    #[test]
    fn test_remove_redundant_mov() {
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            // This is redundant: mov rax, rax
            X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Physical(Reg::Rax),
            },
            X86Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 1);
        assert_eq!(instructions.len(), 2);
        // Verify the mov rax, rax was removed
        assert!(matches!(instructions[0], X86Inst::MovRI32 { .. }));
        assert!(matches!(instructions[1], X86Inst::Ret));
    }

    #[test]
    fn test_keep_useful_mov() {
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            // This is NOT redundant: mov rbx, rax
            X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rbx),
                src: Operand::Physical(Reg::Rax),
            },
            X86Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_remove_add_zero() {
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            // This is redundant: add rax, 0
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 1);
        assert_eq!(instructions.len(), 2);
    }

    #[test]
    fn test_keep_add_nonzero() {
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            // This is NOT redundant: add rax, 1
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 1,
            },
            X86Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_multiple_redundant_instructions() {
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Physical(Reg::Rax),
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rbx),
                src: Operand::Physical(Reg::Rbx),
            },
            X86Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 3);
        assert_eq!(instructions.len(), 2);
    }
}
