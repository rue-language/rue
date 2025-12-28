//! Peephole optimization pass for AArch64.
//!
//! This pass runs after register allocation and removes redundant instructions
//! such as:
//! - `mov x, x` (no-op moves where src == dst)
//! - `add x, x, #0` / `sub x, x, #0` (identity arithmetic)
//!
//! The pass operates in-place on the instruction vector for efficiency.

use super::mir::{Aarch64Inst, Operand};

/// Apply peephole optimizations to the instruction stream.
///
/// This modifies the vector in place, removing redundant instructions.
/// Returns the number of instructions removed.
pub fn optimize(instructions: &mut Vec<Aarch64Inst>) -> usize {
    let original_count = instructions.len();

    instructions.retain(|inst| !is_redundant(inst));

    original_count - instructions.len()
}

/// Check if an instruction is redundant and can be removed.
fn is_redundant(inst: &Aarch64Inst) -> bool {
    match inst {
        // mov r, r where src == dst is a no-op
        Aarch64Inst::MovRR { dst, src } => operands_equal(dst, src),

        // add r, r, #0 is identity
        Aarch64Inst::AddImm { dst, src, imm: 0 } => operands_equal(dst, src),

        // sub r, r, #0 is identity
        Aarch64Inst::SubImm { dst, src, imm: 0 } => operands_equal(dst, src),

        // Other identity patterns could be added here in the future:
        // - lsl r, r, #0 (shift by 0)
        // - lsr r, r, #0 (shift by 0)
        // - and r, r, #-1 (AND with all 1s)
        // - orr r, r, #0 (OR with 0)
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
    use crate::aarch64::mir::Reg;

    #[test]
    fn test_remove_redundant_mov() {
        let mut instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            // This is redundant: mov x0, x0
            Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
            },
            Aarch64Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 1);
        assert_eq!(instructions.len(), 2);
        // Verify the mov x0, x0 was removed
        assert!(matches!(instructions[0], Aarch64Inst::MovImm { .. }));
        assert!(matches!(instructions[1], Aarch64Inst::Ret));
    }

    #[test]
    fn test_keep_useful_mov() {
        let mut instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            // This is NOT redundant: mov x1, x0
            Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X1),
                src: Operand::Physical(Reg::X0),
            },
            Aarch64Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_remove_add_zero() {
        let mut instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            // This is redundant: add x0, x0, #0
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 1);
        assert_eq!(instructions.len(), 2);
    }

    #[test]
    fn test_remove_sub_zero() {
        let mut instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            // This is redundant: sub x0, x0, #0
            Aarch64Inst::SubImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 1);
        assert_eq!(instructions.len(), 2);
    }

    #[test]
    fn test_keep_add_nonzero() {
        let mut instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            // This is NOT redundant: add x0, x0, #1
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 1,
            },
            Aarch64Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_keep_add_different_dst() {
        let mut instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            // This is NOT redundant even with imm=0: add x1, x0, #0 (dst != src)
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X1),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_multiple_redundant_instructions() {
        let mut instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X1),
                src: Operand::Physical(Reg::X1),
            },
            Aarch64Inst::Ret,
        ];

        let removed = optimize(&mut instructions);

        assert_eq!(removed, 3);
        assert_eq!(instructions.len(), 2);
    }
}
