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
//! - `add r, r, #a` + `add r, r, #b` → `add r, r, #(a+b)` (when the sum fits
//!   the ordinary AArch64 immediate sequence)
//!
//! The pass operates in-place on the instruction vector for efficiency.
//!
//! ## NZCV invariant (RUE-152)
//!
//! A rewrite may not change the flag values a later reader observes.
//! `cmp r, #0` and `tst r, r` produce the same N, Z and V (V=0 for both)
//! but OPPOSITE C: `cmp` with 0 never borrows (C=1) while `tst` clears C.
//! The rewrite is therefore only applied when every reader of the resulting
//! flags — every `b.cond`/`cset` before the next flags writer — uses a
//! C-free condition (eq/ne and the signed lt/gt/le/ge, which read N, Z and
//! V only). If the flags escape this straight-line window (a label or an
//! unconditional branch is reached first), the rewrite is skipped. See
//! [`cmp_zero_to_tst_safe`].
//!
//! The removals and the add/sub combining here only touch non-flag-setting
//! instructions (`add`/`sub`/`mov`/shifts/`eor` without the S suffix), so
//! they need no flags gate.

use super::mir::{Aarch64Inst, Cond, MAX_ADD_SUB_IMMEDIATE, Operand, Reg};
use super::schedule::writes_flags;

/// Apply peephole optimizations to the instruction stream.
///
/// This modifies the vector in place, performing transformations and removing
/// redundant instructions. Returns the total number of changes made.
pub fn optimize(instructions: &mut Vec<Aarch64Inst>) -> usize {
    let mut changes = 0;

    // Pass 1: Single-instruction transforms (mov 0 -> mov xzr, cmp 0 -> tst)
    for i in 0..instructions.len() {
        if let Some(new_inst) = transform_single(instructions, i) {
            instructions[i] = new_inst;
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

/// Check whether `cmp r, #0` at `idx` may be rewritten to `tst r, r`.
///
/// The two differ only in C (cmp sets it, tst clears it), so the rewrite is
/// safe iff no reader of these flags consumes C. Scans forward from
/// `idx + 1`:
/// - `b.cond` / `cset` with a C-reading condition (hs/lo/hi/ls) → unsafe
/// - `b.cond` / `cset` with a C-free condition, or `b.vs`/`b.vc` (V is 0
///   after both forms) → fine; keep scanning for more readers
/// - a flags writer → safe (flags overwritten before any further read)
/// - call/svc/ret → safe (NZCV is not live across these per the AAPCS)
/// - label or unconditional branch → conservatively unsafe (flags escape)
/// - end of stream → safe
fn cmp_zero_to_tst_safe(instructions: &[Aarch64Inst], idx: usize) -> bool {
    for inst in &instructions[idx + 1..] {
        match inst {
            Aarch64Inst::BCond { cond, .. } | Aarch64Inst::Cset { cond, .. } => {
                if cond_reads_carry(*cond) {
                    return false;
                }
            }
            // V is 0 after both cmp r, #0 and tst r, r.
            Aarch64Inst::Bvs { .. } | Aarch64Inst::Bvc { .. } => {}
            // An unconditional branch or a label ends the straight-line
            // window. Cbz/Cbnz are conditional branches whose TAKEN edge also
            // escapes the window (the cmp's flags are live-out along it), so
            // they are escape boundaries too — matching x86's treatment.
            Aarch64Inst::B { .. }
            | Aarch64Inst::Label { .. }
            | Aarch64Inst::Cbz { .. }
            | Aarch64Inst::Cbnz { .. } => return false,
            Aarch64Inst::Bl { .. } | Aarch64Inst::Svc { .. } | Aarch64Inst::Ret => return true,
            inst if writes_flags(inst) => return true,
            _ => {}
        }
    }
    true
}

/// Conditions that read the carry flag — exactly the unsigned comparisons.
fn cond_reads_carry(cond: Cond) -> bool {
    matches!(cond, Cond::Hs | Cond::Lo | Cond::Hi | Cond::Ls)
}

/// Transform the instruction at `idx` to a more efficient form.
///
/// Returns `Some(new_inst)` if a transformation applies, `None` otherwise.
fn transform_single(instructions: &[Aarch64Inst], idx: usize) -> Option<Aarch64Inst> {
    match &instructions[idx] {
        // mov r, #0 → mov r, xzr (use zero register)
        // On AArch64, using the zero register is often more efficient.
        // Neither form touches NZCV, so no flags gate is needed.
        Aarch64Inst::MovImm { dst, imm: 0 } => Some(Aarch64Inst::MovRR {
            dst: *dst,
            src: Operand::Physical(Reg::Xzr),
        }),

        // cmp r, #0 → tst r, r (same N/Z/V, sometimes faster)
        // C FLIPS (cmp sets it, tst clears it), so only legal when no
        // consumer reads C — see cmp_zero_to_tst_safe (RUE-152).
        Aarch64Inst::CmpImm { src, imm: 0 } if cmp_zero_to_tst_safe(instructions, idx) => {
            Some(Aarch64Inst::TstRR {
                src1: *src,
                src2: *src,
            })
        }

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
                // Keep the combined MIR immediate within the ordinary
                // AArch64 immediate sequence; larger values use emitter
                // materialization and should not be synthesized here.
                if let Some(combined) = imm1.checked_add(*imm2).filter(|combined| {
                    (-MAX_ADD_SUB_IMMEDIATE..=MAX_ADD_SUB_IMMEDIATE).contains(combined)
                }) {
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
                if let Some(combined) = imm1
                    .checked_add(*imm2)
                    .filter(|combined| (0..=MAX_ADD_SUB_IMMEDIATE).contains(combined))
                {
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
    fn test_combine_adds_at_encoding_limit_but_not_above() {
        let physical = Operand::Physical(Reg::X0);
        let mut at_limit = vec![
            Aarch64Inst::AddImm {
                dst: physical,
                src: physical,
                imm: MAX_ADD_SUB_IMMEDIATE - 1,
            },
            Aarch64Inst::AddImm {
                dst: physical,
                src: physical,
                imm: 1,
            },
            Aarch64Inst::Ret,
        ];
        assert_eq!(optimize(&mut at_limit), 1);
        assert!(matches!(
            at_limit[0],
            Aarch64Inst::AddImm {
                imm: MAX_ADD_SUB_IMMEDIATE,
                ..
            }
        ));

        let mut above_limit = vec![
            Aarch64Inst::AddImm {
                dst: physical,
                src: physical,
                imm: MAX_ADD_SUB_IMMEDIATE,
            },
            Aarch64Inst::AddImm {
                dst: physical,
                src: physical,
                imm: 1,
            },
            Aarch64Inst::Ret,
        ];
        assert_eq!(optimize(&mut above_limit), 0);
        assert_eq!(above_limit.len(), 3);
    }

    #[test]
    fn test_combine_adds_signed_limit_but_not_below() {
        let physical = Operand::Physical(Reg::X0);
        let mut at_limit = vec![
            Aarch64Inst::AddImm {
                dst: physical,
                src: physical,
                imm: -MAX_ADD_SUB_IMMEDIATE + 1,
            },
            Aarch64Inst::AddImm {
                dst: physical,
                src: physical,
                imm: -1,
            },
            Aarch64Inst::Ret,
        ];
        assert_eq!(optimize(&mut at_limit), 1);
        assert!(matches!(
            at_limit[0],
            Aarch64Inst::AddImm { imm, .. } if imm == -MAX_ADD_SUB_IMMEDIATE
        ));

        let mut below_limit = vec![
            Aarch64Inst::AddImm {
                dst: physical,
                src: physical,
                imm: -MAX_ADD_SUB_IMMEDIATE,
            },
            Aarch64Inst::AddImm {
                dst: physical,
                src: physical,
                imm: -1,
            },
            Aarch64Inst::Ret,
        ];
        assert_eq!(optimize(&mut below_limit), 0);
        assert_eq!(below_limit.len(), 3);
    }

    #[test]
    fn test_combine_subs_at_encoding_limit_but_not_above() {
        let physical = Operand::Physical(Reg::X0);
        let mut at_limit = vec![
            Aarch64Inst::SubImm {
                dst: physical,
                src: physical,
                imm: MAX_ADD_SUB_IMMEDIATE - 1,
            },
            Aarch64Inst::SubImm {
                dst: physical,
                src: physical,
                imm: 1,
            },
            Aarch64Inst::Ret,
        ];
        assert_eq!(optimize(&mut at_limit), 1);
        assert!(matches!(
            at_limit[0],
            Aarch64Inst::SubImm {
                imm: MAX_ADD_SUB_IMMEDIATE,
                ..
            }
        ));

        let mut above_limit = vec![
            Aarch64Inst::SubImm {
                dst: physical,
                src: physical,
                imm: MAX_ADD_SUB_IMMEDIATE,
            },
            Aarch64Inst::SubImm {
                dst: physical,
                src: physical,
                imm: 1,
            },
            Aarch64Inst::Ret,
        ];
        assert_eq!(optimize(&mut above_limit), 0);
        assert_eq!(above_limit.len(), 3);
    }

    #[test]
    fn test_combine_subs_rejects_negative_domain() {
        let physical = Operand::Physical(Reg::X0);
        let mut instructions = vec![
            Aarch64Inst::SubImm {
                dst: physical,
                src: physical,
                imm: -1,
            },
            Aarch64Inst::SubImm {
                dst: physical,
                src: physical,
                imm: -1,
            },
            Aarch64Inst::Ret,
        ];

        assert_eq!(optimize(&mut instructions), 0);
        assert_eq!(instructions.len(), 3);
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

    // ==================== NZCV Hazard Tests (RUE-152) ====================

    use crate::aarch64::mir::{Cond, LabelId};

    #[test]
    fn test_cmp_zero_to_tst_fires_with_carry_free_consumer() {
        // b.eq reads only Z, identical under tst: the rewrite STILL fires.
        let mut instructions = vec![
            Aarch64Inst::CmpImm {
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::BCond {
                cond: Cond::Eq,
                label: LabelId::new(0),
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert!(matches!(instructions[0], Aarch64Inst::TstRR { .. }));
    }

    #[test]
    fn test_cmp_zero_to_tst_fires_with_signed_consumer() {
        // Signed conditions read N/Z/V, all identical under tst (V=0 both).
        let mut instructions = vec![
            Aarch64Inst::CmpImm {
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Cset {
                dst: Operand::Physical(Reg::X1),
                cond: Cond::Lt,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert!(matches!(instructions[0], Aarch64Inst::TstRR { .. }));
    }

    #[test]
    fn test_cmp_zero_to_tst_blocked_by_unsigned_branch() {
        // b.hs reads C, which cmp #0 sets (no borrow) but tst clears: the
        // rewrite would flip the branch. It must NOT fire.
        let mut instructions = vec![
            Aarch64Inst::CmpImm {
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::BCond {
                cond: Cond::Hs,
                label: LabelId::new(0),
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert!(matches!(
            instructions[0],
            Aarch64Inst::CmpImm { imm: 0, .. }
        ));
    }

    #[test]
    fn test_cmp_zero_to_tst_blocked_by_unsigned_cset() {
        let mut instructions = vec![
            Aarch64Inst::CmpImm {
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Cset {
                dst: Operand::Physical(Reg::X1),
                cond: Cond::Lo,
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert!(matches!(
            instructions[0],
            Aarch64Inst::CmpImm { imm: 0, .. }
        ));
    }

    #[test]
    fn test_cmp_zero_to_tst_blocked_by_label() {
        // Flags escape into a join point; conservatively keep the cmp.
        let mut instructions = vec![
            Aarch64Inst::CmpImm {
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::Label {
                id: LabelId::new(0),
            },
            Aarch64Inst::BCond {
                cond: Cond::Eq,
                label: LabelId::new(1),
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert!(matches!(
            instructions[0],
            Aarch64Inst::CmpImm { imm: 0, .. }
        ));
    }

    #[test]
    fn test_cmp_zero_to_tst_unsigned_consumer_after_writer_ok() {
        // The unsigned consumer reads flags from the LATER cmp, not ours:
        // the rewrite fires.
        let mut instructions = vec![
            Aarch64Inst::CmpImm {
                src: Operand::Physical(Reg::X0),
                imm: 0,
            },
            Aarch64Inst::CmpRR {
                src1: Operand::Physical(Reg::X1),
                src2: Operand::Physical(Reg::X2),
            },
            Aarch64Inst::BCond {
                cond: Cond::Hs,
                label: LabelId::new(0),
            },
            Aarch64Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert!(matches!(instructions[0], Aarch64Inst::TstRR { .. }));
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
