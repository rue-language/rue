//! Peephole optimization pass for x86-64.
//!
//! This pass runs after register allocation and applies several categories
//! of optimizations:
//!
//! ## Category 1: Identity instruction removal
//! - `mov r, r` (no-op moves where src == dst)
//! - `add r, 0` (identity arithmetic)
//! - `xor r, 0` (identity XOR)
//! - `shl r, 0` / `shr r, 0` / `sar r, 0` (shifts by 0)
//!
//! ## Category 2: Strength reduction transforms
//! - `mov r, 0` → `xor r, r` (5 bytes → 2 bytes, faster on modern CPUs)
//! - `cmp r, 0` → `test r, r` (same flags, sometimes faster)
//!
//! ## Category 3: Adjacent instruction combining
//! - `add r, a` + `add r, b` → `add r, a+b` (when sum fits in i32)
//!
//! The pass operates in-place on the instruction vector for efficiency.

use super::mir::{Operand, X86Inst};

/// Apply peephole optimizations to the instruction stream.
///
/// This modifies the vector in place, performing transformations and removing
/// redundant instructions. Returns the total number of changes made.
pub fn optimize(instructions: &mut Vec<X86Inst>) -> usize {
    let mut changes = 0;

    // Pass 1: Single-instruction transforms (mov 0 -> xor, cmp 0 -> test)
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
fn transform_single(inst: &X86Inst) -> Option<X86Inst> {
    match inst {
        // mov r, 0 → xor r, r (smaller encoding: 5 bytes → 2 bytes)
        // Also breaks false dependencies on modern CPUs.
        X86Inst::MovRI32 { dst, imm: 0 } => Some(X86Inst::XorRR {
            dst: *dst,
            src: *dst,
        }),

        // cmp r, 0 → test r, r (same flags, often faster)
        // test sets ZF=1 if r==0, same as cmp r, 0
        X86Inst::CmpRI { src, imm: 0 } => Some(X86Inst::TestRR {
            src1: *src,
            src2: *src,
        }),

        // cmp64 r, 0 → test r, r (64-bit version)
        X86Inst::Cmp64RI { src, imm: 0 } => Some(X86Inst::TestRR {
            src1: *src,
            src2: *src,
        }),

        _ => None,
    }
}

/// Combine adjacent instructions where possible.
///
/// Currently handles:
/// - `add r, a` followed by `add r, b` → `add r, a+b`
///
/// Returns the number of combinations made.
fn combine_adjacent(instructions: &mut Vec<X86Inst>) -> usize {
    if instructions.len() < 2 {
        return 0;
    }

    let mut changes = 0;
    let mut i = 0;

    while i + 1 < instructions.len() {
        // Try to combine add chains: add r, a; add r, b → add r, a+b
        if let (
            X86Inst::AddRI {
                dst: dst1,
                imm: imm1,
            },
            X86Inst::AddRI {
                dst: dst2,
                imm: imm2,
            },
        ) = (&instructions[i], &instructions[i + 1])
        {
            if operands_equal(dst1, dst2) {
                // Check for overflow when combining immediates
                if let Some(combined) = imm1.checked_add(*imm2) {
                    // Replace first instruction with combined add
                    instructions[i] = X86Inst::AddRI {
                        dst: *dst1,
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

        i += 1;
    }

    changes
}

/// Check if an instruction is redundant and can be removed.
fn is_redundant(inst: &X86Inst) -> bool {
    match inst {
        // mov r, r where src == dst is a no-op
        X86Inst::MovRR { dst, src } => operands_equal(dst, src),

        // add r, 0 is identity
        X86Inst::AddRI { imm: 0, .. } => true,

        // xor r, 0 is identity (note: xor r, r is NOT redundant - it zeros the register)
        X86Inst::XorRI { imm: 0, .. } => true,

        // Shift by 0 is identity (all shift variants)
        X86Inst::ShlRI { imm: 0, .. } => true,
        X86Inst::Shl32RI { imm: 0, .. } => true,
        X86Inst::ShrRI { imm: 0, .. } => true,
        X86Inst::Shr32RI { imm: 0, .. } => true,
        X86Inst::SarRI { imm: 0, .. } => true,
        X86Inst::Sar32RI { imm: 0, .. } => true,

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

    // ==================== Category 1: Identity Removal Tests ====================

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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_remove_xor_zero() {
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            // This is redundant: xor rax, 0
            X86Inst::XorRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
    }

    #[test]
    fn test_remove_shift_by_zero() {
        // Test all shift variants with imm=0
        let mut instructions = vec![
            X86Inst::ShlRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Shl32RI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::ShrRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Shr32RI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::SarRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Sar32RI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 6);
        assert_eq!(instructions.len(), 1);
        assert!(matches!(instructions[0], X86Inst::Ret));
    }

    #[test]
    fn test_keep_nonzero_shift() {
        let mut instructions = vec![
            X86Inst::ShlRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 2,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 2);
    }

    // ==================== Category 2: Strength Reduction Tests ====================

    #[test]
    fn test_mov_zero_to_xor() {
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
        // Verify transformation: mov rax, 0 -> xor rax, rax
        match &instructions[0] {
            X86Inst::XorRR { dst, src } => {
                assert!(operands_equal(dst, src));
                assert!(matches!(dst, Operand::Physical(Reg::Rax)));
            }
            other => panic!("Expected XorRR, got {:?}", other),
        }
    }

    #[test]
    fn test_mov_nonzero_not_transformed() {
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(instructions[0], X86Inst::MovRI32 { imm: 42, .. }));
    }

    #[test]
    fn test_cmp_zero_to_test() {
        let mut instructions = vec![
            X86Inst::CmpRI {
                src: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
        // Verify transformation: cmp rax, 0 -> test rax, rax
        match &instructions[0] {
            X86Inst::TestRR { src1, src2 } => {
                assert!(operands_equal(src1, src2));
                assert!(matches!(src1, Operand::Physical(Reg::Rax)));
            }
            other => panic!("Expected TestRR, got {:?}", other),
        }
    }

    #[test]
    fn test_cmp64_zero_to_test() {
        let mut instructions = vec![
            X86Inst::Cmp64RI {
                src: Operand::Physical(Reg::Rbx),
                imm: 0,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
        match &instructions[0] {
            X86Inst::TestRR { src1, src2 } => {
                assert!(operands_equal(src1, src2));
                assert!(matches!(src1, Operand::Physical(Reg::Rbx)));
            }
            other => panic!("Expected TestRR, got {:?}", other),
        }
    }

    #[test]
    fn test_cmp_nonzero_not_transformed() {
        let mut instructions = vec![
            X86Inst::CmpRI {
                src: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(instructions[0], X86Inst::CmpRI { imm: 42, .. }));
    }

    // ==================== Category 3: Adjacent Combining Tests ====================

    #[test]
    fn test_combine_adjacent_adds() {
        let mut instructions = vec![
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 10,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 20,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(instructions[0], X86Inst::AddRI { imm: 30, .. }));
    }

    #[test]
    fn test_combine_three_adjacent_adds() {
        let mut instructions = vec![
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 10,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 20,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 5,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 2);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(instructions[0], X86Inst::AddRI { imm: 35, .. }));
    }

    #[test]
    fn test_combine_adds_different_registers() {
        // Adds to different registers should NOT be combined
        let mut instructions = vec![
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 10,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rbx),
                imm: 20,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_combine_adds_overflow_prevention() {
        // When the sum would overflow i32, don't combine
        let mut instructions = vec![
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: i32::MAX,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 1,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        // Should not combine due to overflow
        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 3);
    }

    #[test]
    fn test_combine_adds_with_negative() {
        // Combining positive and negative immediates
        let mut instructions = vec![
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 100,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: -30,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert_eq!(instructions.len(), 2);
        assert!(matches!(instructions[0], X86Inst::AddRI { imm: 70, .. }));
    }

    #[test]
    fn test_combine_adds_to_zero_then_remove() {
        // After combining to 0, the add 0 should be removed
        let mut instructions = vec![
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 50,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: -50,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        // 1 for combining, 1 for removing add 0
        assert_eq!(changes, 2);
        assert_eq!(instructions.len(), 1);
        assert!(matches!(instructions[0], X86Inst::Ret));
    }

    // ==================== Combined Scenario Tests ====================

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

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 3);
        assert_eq!(instructions.len(), 2);
    }

    #[test]
    fn test_combined_transforms_and_removals() {
        // Test that all optimization types work together
        let mut instructions = vec![
            // Transform: mov rax, 0 -> xor rax, rax
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            // Combine: add 10 + add 20 -> add 30
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 10,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 20,
            },
            // Remove: mov rbx, rbx
            X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rbx),
                src: Operand::Physical(Reg::Rbx),
            },
            // Transform: cmp rax, 0 -> test rax, rax
            X86Inst::CmpRI {
                src: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        // 1 (mov 0->xor) + 1 (combine adds) + 1 (remove mov) + 1 (cmp 0->test) = 4
        assert_eq!(changes, 4);
        assert_eq!(instructions.len(), 4);

        // Verify final sequence
        assert!(matches!(instructions[0], X86Inst::XorRR { .. }));
        assert!(matches!(instructions[1], X86Inst::AddRI { imm: 30, .. }));
        assert!(matches!(instructions[2], X86Inst::TestRR { .. }));
        assert!(matches!(instructions[3], X86Inst::Ret));
    }

    #[test]
    fn test_xor_rr_not_removed_when_same_register() {
        // xor rax, rax is NOT redundant - it zeros the register!
        let mut instructions = vec![
            X86Inst::XorRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Physical(Reg::Rax),
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 2);
        // xor rax, rax should remain - it zeros the register
        assert!(matches!(instructions[0], X86Inst::XorRR { .. }));
    }
}
