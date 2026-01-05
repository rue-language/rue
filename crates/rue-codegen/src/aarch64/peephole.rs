//! Peephole optimization pass for AArch64.
//!
//! This pass runs after register allocation and applies several categories
//! of optimizations:
//!
//! ## Category 1: Identity instruction removal
//! - `mov r, r` (no-op moves where src == dst)
//! - `add r, r, #0` / `sub r, r, #0` (identity arithmetic)
//! - `lsl r, r, #0` / `lsr r, r, #0` / `asr r, r, #0` (shifts by 0)
//! - `eor r, r, #0` (XOR with 0 is identity)
//!
//! ## Category 2: Strength reduction transforms
//! - `mov r, #0` → `mov r, xzr` (use zero register, smaller encoding possible)
//! - `cmp r, #0` → `tst r, r` (same flags, sometimes faster)
//!
//! ## Category 3: Adjacent instruction combining
//! - `add r, r, #a` + `add r, r, #b` → `add r, r, #(a+b)` (when sum fits in i32)
//!
//! The pass operates in-place on the instruction vector for efficiency.

use super::mir::{Aarch64Inst, Operand, Reg};

/// Apply peephole optimizations to the instruction stream.
///
/// This modifies the vector in place, performing transformations and removing
/// redundant instructions. Returns the total number of changes made.
pub fn optimize(instructions: &mut Vec<Aarch64Inst>) -> usize {
    let mut changes = 0;

    // Pass 1: Single-instruction transforms (mov 0 -> mov xzr, cmp 0 -> tst)
    for inst in instructions.iter_mut() {
        if let Some(new_inst) = transform_single(inst) {
            *inst = new_inst;
            changes += 1;
        }
    }

    // Pass 2: Adjacent instruction combining (add chains)
    changes += combine_adjacent(instructions);

    // Pass 3: Remove identity instructions
    let before = instructions.len();
    instructions.retain(|inst| !is_redundant(inst));
    changes += before - instructions.len();

    changes
}

/// Transform a single instruction to a more efficient form.
///
/// Returns `Some(new_inst)` if a transformation was applied, `None` otherwise.
fn transform_single(inst: &Aarch64Inst) -> Option<Aarch64Inst> {
    match inst {
        // mov r, #0 → mov r, xzr (use zero register)
        // On AArch64, using the zero register is often more efficient.
        Aarch64Inst::MovImm { dst, imm: 0 } => Some(Aarch64Inst::MovRR {
            dst: *dst,
            src: Operand::Physical(Reg::Xzr),
        }),

        // cmp r, #0 → tst r, r (same flags, sometimes faster)
        // tst sets ZF=1 if r==0, same as cmp r, 0
        Aarch64Inst::CmpImm { src, imm: 0 } => Some(Aarch64Inst::TstRR {
            src1: *src,
            src2: *src,
        }),

        _ => None,
    }
}

/// Combine adjacent instructions where possible.
///
/// Currently handles:
/// - `add r, r, #a` followed by `add r, r, #b` → `add r, r, #(a+b)`
/// - `sub r, r, #a` followed by `sub r, r, #b` → `sub r, r, #(a+b)`
///
/// Returns the number of combinations made.
fn combine_adjacent(instructions: &mut Vec<Aarch64Inst>) -> usize {
    if instructions.len() < 2 {
        return 0;
    }

    let mut changes = 0;
    let mut i = 0;

    while i + 1 < instructions.len() {
        // Try to combine add chains: add r, r, #a; add r, r, #b → add r, r, #(a+b)
        if let (
            Aarch64Inst::AddImm {
                dst: dst1,
                src: src1,
                imm: imm1,
            },
            Aarch64Inst::AddImm {
                dst: dst2,
                src: src2,
                imm: imm2,
            },
        ) = (&instructions[i], &instructions[i + 1])
        {
            // Only combine if dst == src for both (i.e., add r, r, #imm pattern)
            // and both operations are on the same register
            if operands_equal(dst1, src1)
                && operands_equal(dst2, src2)
                && operands_equal(dst1, dst2)
            {
                // Check for overflow when combining immediates
                if let Some(combined) = imm1.checked_add(*imm2) {
                    // Replace first instruction with combined add
                    instructions[i] = Aarch64Inst::AddImm {
                        dst: *dst1,
                        src: *src1,
                        imm: combined,
                    };
                    // Remove second instruction
                    instructions.remove(i + 1);
                    changes += 1;
                    // Don't increment i - there might be more adds to combine
                    continue;
                }
            }
        }

        // Try to combine sub chains: sub r, r, #a; sub r, r, #b → sub r, r, #(a+b)
        if let (
            Aarch64Inst::SubImm {
                dst: dst1,
                src: src1,
                imm: imm1,
            },
            Aarch64Inst::SubImm {
                dst: dst2,
                src: src2,
                imm: imm2,
            },
        ) = (&instructions[i], &instructions[i + 1])
        {
            if operands_equal(dst1, src1)
                && operands_equal(dst2, src2)
                && operands_equal(dst1, dst2)
            {
                if let Some(combined) = imm1.checked_add(*imm2) {
                    instructions[i] = Aarch64Inst::SubImm {
                        dst: *dst1,
                        src: *src1,
                        imm: combined,
                    };
                    instructions.remove(i + 1);
                    changes += 1;
                    continue;
                }
            }
        }

        i += 1;
    }

    changes
}

/// Check if an instruction is redundant and can be removed.
fn is_redundant(inst: &Aarch64Inst) -> bool {
    match inst {
        // mov r, r where src == dst is a no-op
        // Exception: mov r, xzr is NOT redundant - it zeros the register!
        Aarch64Inst::MovRR { dst, src } => {
            operands_equal(dst, src) && !matches!(src, Operand::Physical(Reg::Xzr))
        }

        // add r, r, #0 is identity
        Aarch64Inst::AddImm { dst, src, imm: 0 } => operands_equal(dst, src),

        // sub r, r, #0 is identity
        Aarch64Inst::SubImm { dst, src, imm: 0 } => operands_equal(dst, src),

        // Shift by 0 is identity (all shift variants)
        Aarch64Inst::LslImm { dst, src, imm: 0 } => operands_equal(dst, src),
        Aarch64Inst::Lsl32Imm { dst, src, imm: 0 } => operands_equal(dst, src),
        Aarch64Inst::Lsr32Imm { dst, src, imm: 0 } => operands_equal(dst, src),
        Aarch64Inst::Lsr64Imm { dst, src, imm: 0 } => operands_equal(dst, src),
        Aarch64Inst::Asr32Imm { dst, src, imm: 0 } => operands_equal(dst, src),
        Aarch64Inst::Asr64Imm { dst, src, imm: 0 } => operands_equal(dst, src),

        // XOR with 0 is identity (but XOR r, r, r is NOT redundant - it zeros!)
        Aarch64Inst::EorImm { dst, src, imm: 0 } => operands_equal(dst, src),

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

    // ==================== Category 1: Identity Removal Tests ====================

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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_remove_shift_by_zero() {
        // Test all shift variants with imm=0
        let mut instructions = vec![
            Aarch64Inst::LslImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Lsl32Imm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Lsr32Imm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Lsr64Imm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Asr32Imm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Asr64Imm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 6);
        assert_eq!(instructions.len(), 1);
        assert!(matches!(instructions[0], Aarch64Inst::Ret));
    }

    #[test]
    fn test_keep_nonzero_shift() {
        let mut instructions = vec![
            Aarch64Inst::LslImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 2,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 2);
    }

    #[test]
    fn test_remove_eor_zero() {
        let mut instructions = vec![
            Aarch64Inst::EorImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 1);
    }

    // ==================== Category 2: Strength Reduction Tests ====================

    #[test]
    fn test_mov_zero_to_mov_xzr() {
        let mut instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
        // Verify transformation: mov x0, #0 -> mov x0, xzr
        match &instructions[0] {
            Aarch64Inst::MovRR { dst, src } => {
                assert!(matches!(dst, Operand::Physical(Reg::X0)));
                assert!(matches!(src, Operand::Physical(Reg::Xzr)));
            }
            other => panic!("Expected MovRR, got {:?}", other),
        }
    }

    #[test]
    fn test_mov_nonzero_not_transformed() {
        let mut instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(
            instructions[0],
            Aarch64Inst::MovImm { imm: 42, .. }
        ));
    }

    #[test]
    fn test_cmp_zero_to_tst() {
        let mut instructions = vec![
            Aarch64Inst::CmpImm {
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
        // Verify transformation: cmp x0, #0 -> tst x0, x0
        match &instructions[0] {
            Aarch64Inst::TstRR { src1, src2 } => {
                assert!(operands_equal(src1, src2));
                assert!(matches!(src1, Operand::Physical(Reg::X0)));
            }
            other => panic!("Expected TstRR, got {:?}", other),
        }
    }

    #[test]
    fn test_cmp_nonzero_not_transformed() {
        let mut instructions = vec![
            Aarch64Inst::CmpImm {
                src: Operand::Physical(Reg::X0),
                imm: 42,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(
            instructions[0],
            Aarch64Inst::CmpImm { imm: 42, .. }
        ));
    }

    // ==================== Category 3: Adjacent Combining Tests ====================

    #[test]
    fn test_combine_adjacent_adds() {
        let mut instructions = vec![
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 10,
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 20,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(
            instructions[0],
            Aarch64Inst::AddImm { imm: 30, .. }
        ));
    }

    #[test]
    fn test_combine_three_adjacent_adds() {
        let mut instructions = vec![
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 10,
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 20,
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 5,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 2);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(
            instructions[0],
            Aarch64Inst::AddImm { imm: 35, .. }
        ));
    }

    #[test]
    fn test_combine_adjacent_subs() {
        let mut instructions = vec![
            Aarch64Inst::SubImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 10,
            },
            Aarch64Inst::SubImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 20,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(
            instructions[0],
            Aarch64Inst::SubImm { imm: 30, .. }
        ));
    }

    #[test]
    fn test_combine_adds_different_registers() {
        // Adds to different registers should NOT be combined
        let mut instructions = vec![
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 10,
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X1),
                src: Operand::Physical(Reg::X1),
                imm: 20,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_combine_adds_overflow_prevention() {
        // When the sum would overflow i32, don't combine
        let mut instructions = vec![
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: i32::MAX,
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 1,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        // Should not combine due to overflow
        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_combine_adds_to_zero_then_remove() {
        // After combining to 0, the add 0 should be removed
        let mut instructions = vec![
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 50,
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: -50,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        // 1 for combining, 1 for removing add 0
        assert_eq!(changes, 2);
        assert_eq!(instructions.len(), 1);
        assert!(matches!(instructions[0], Aarch64Inst::Ret));
    }

    // ==================== Combined Scenario Tests ====================

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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 3);
        assert_eq!(instructions.len(), 2);
    }

    #[test]
    fn test_combined_transforms_and_removals() {
        // Test that all optimization types work together
        let mut instructions = vec![
            // Transform: mov x0, #0 -> mov x0, xzr
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 0,
            },
            // Combine: add 10 + add 20 -> add 30
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 10,
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 20,
            },
            // Remove: mov x1, x1
            Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X1),
                src: Operand::Physical(Reg::X1),
            },
            // Transform: cmp x0, #0 -> tst x0, x0
            Aarch64Inst::CmpImm {
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        // 1 (mov 0->mov xzr) + 1 (combine adds) + 1 (remove mov) + 1 (cmp 0->tst) = 4
        assert_eq!(changes, 4);
        assert_eq!(instructions.len(), 4);

        // Verify final sequence
        assert!(matches!(instructions[0], Aarch64Inst::MovRR { .. }));
        assert!(matches!(
            instructions[1],
            Aarch64Inst::AddImm { imm: 30, .. }
        ));
        assert!(matches!(instructions[2], Aarch64Inst::TstRR { .. }));
        assert!(matches!(instructions[3], Aarch64Inst::Ret));
    }

    #[test]
    fn test_mov_xzr_not_removed() {
        // mov x0, xzr is NOT redundant - it zeros the register!
        let mut instructions = vec![
            Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::Xzr),
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 2);
        // mov x0, xzr should remain - it zeros the register
        assert!(matches!(instructions[0], Aarch64Inst::MovRR { .. }));
    }
}
