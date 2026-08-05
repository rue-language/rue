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
//!
//! ## FLAGS invariant (RUE-152)
//!
//! A rewrite that changes what an instruction does to FLAGS — adding a
//! flags write (`mov r, 0` → `xor r, r`), dropping one (removing
//! `add r, 0` / `xor r, 0`), or changing the flag values an instruction
//! produces (combining add chains alters CF/OF) — is only applied when the
//! FLAGS state at that point is provably dead: scanning forward, the next
//! flags READER must be preceded by another flags WRITER (or a call/ret,
//! which clobbers FLAGS per the ABI). If the scan reaches a label or an
//! unconditional jump first, the flags state escapes this straight-line
//! window and the rewrite is skipped. See [`flags_dead_after`].
//!
//! `cmp r, 0` → `test r, r` needs no gate on x86: both set CF=0, OF=0 and
//! identical ZF/SF/PF from the same value (only undefined AF differs, and
//! no consumer reads AF).

use super::mir::{Operand, X86Inst};
use super::schedule::{reads_flags, writes_flags};

/// Apply peephole optimizations to the instruction stream.
///
/// This modifies the vector in place, performing transformations and removing
/// redundant instructions. Returns the total number of changes made.
pub fn optimize(instructions: &mut Vec<X86Inst>) -> usize {
    let mut changes = 0;

    // Precompute FLAGS-liveness for every index in one backward pass. Pass 1
    // only inspects the FLAGS state of instructions *after* `i`, which it does
    // not mutate during the pass (it only rewrites index `i` in place), so this
    // snapshot is valid for the whole pass. Without it, each `mov r, 0` rewrite
    // rescans the tail via `flags_dead_after`, making pass 1 O(n²) on long
    // flag-neutral runs such as large array-literal stores (RUE-302).
    let flags_dead = compute_flags_dead(instructions);

    // Pass 1: Single-instruction transforms (mov 0 -> xor, cmp 0 -> test)
    for i in 0..instructions.len() {
        if let Some(new_inst) = transform_single(instructions, i, flags_dead[i]) {
            instructions[i] = new_inst;
            changes += 1;
        }
    }

    // Pass 2: Adjacent instruction combining (add chains)
    changes += combine_adjacent(instructions);

    // Pass 3: Remove identity instructions
    let mut i = 0;
    while i < instructions.len() {
        if is_redundant(&instructions[i]) && removal_preserves_flags(instructions, i) {
            instructions.remove(i);
            changes += 1;
        } else {
            i += 1;
        }
    }

    changes
}

/// Check whether the FLAGS state produced at position `idx` is dead: no
/// instruction can observe it before it is overwritten.
///
/// Scans forward from `idx + 1`:
/// - a flags reader first → live (return false)
/// - a flags writer first → dead (overwritten before any read)
/// - call/syscall/ret → dead (FLAGS are clobbered/never live across these
///   per the ABI; our codegen never branches on flags set before a call)
/// - label or unconditional jump → conservatively live (the flags state
///   merges with or flows into code outside this straight-line window)
/// - end of stream → dead
///
/// Shift-by-zero immediates leave FLAGS untouched, so they are skipped even
/// though [`writes_flags`] reports the non-zero forms as writers.
fn flags_dead_after(instructions: &[X86Inst], idx: usize) -> bool {
    for inst in &instructions[idx + 1..] {
        if is_shift_by_zero(inst) {
            continue;
        }
        if reads_flags(inst) {
            return false;
        }
        if writes_flags(inst) {
            return true;
        }
        match inst {
            X86Inst::CallRel { .. } | X86Inst::Syscall | X86Inst::Ret => return true,
            X86Inst::Jmp { .. } | X86Inst::Label { .. } => return false,
            _ => {}
        }
    }
    true
}

/// Compute [`flags_dead_after`] for every index in a single backward pass.
///
/// `result[i] == flags_dead_after(instructions, i)`. Each entry depends only on
/// the instructions after `i`, so scanning once from the end (carrying the
/// answer for `i + 1` into `i`) yields all of them in O(n) instead of O(n) per
/// query (RUE-302).
fn compute_flags_dead(instructions: &[X86Inst]) -> Vec<bool> {
    let n = instructions.len();
    let mut result = vec![true; n];
    // `scan_after` holds `flags_dead_after(j)` for the index just processed,
    // i.e. the result of scanning the tail starting at `j + 1`. Seeded with the
    // empty-tail case (`flags_dead_after(n - 1) == true`).
    let mut scan_after = true;
    for j in (0..n).rev() {
        result[j] = scan_after;
        let inst = &instructions[j];
        scan_after = if is_shift_by_zero(inst) {
            scan_after
        } else if reads_flags(inst) {
            false
        } else if writes_flags(inst) {
            true
        } else {
            match inst {
                X86Inst::CallRel { .. } | X86Inst::Syscall | X86Inst::Ret => true,
                X86Inst::Jmp { .. } | X86Inst::Label { .. } => false,
                _ => scan_after,
            }
        };
    }
    result
}

/// Shift-immediate-by-zero forms: architecturally, a shift count of 0
/// leaves FLAGS unmodified, so these are neither flag writers (despite
/// [`writes_flags`] reporting the non-zero forms) nor unsafe to remove.
fn is_shift_by_zero(inst: &X86Inst) -> bool {
    matches!(
        inst,
        X86Inst::ShlRI { imm: 0, .. }
            | X86Inst::Shl32RI { imm: 0, .. }
            | X86Inst::ShrRI { imm: 0, .. }
            | X86Inst::Shr32RI { imm: 0, .. }
            | X86Inst::SarRI { imm: 0, .. }
            | X86Inst::Sar32RI { imm: 0, .. }
    )
}

/// Check that removing the (redundant) instruction at `idx` does not drop a
/// FLAGS write that a later reader observes (RUE-152).
///
/// `add r, 0` and `xor r, 0` are register no-ops but still set FLAGS;
/// shift-by-zero and `mov r, r` touch no flags at all.
fn removal_preserves_flags(instructions: &[X86Inst], idx: usize) -> bool {
    let inst = &instructions[idx];
    if !writes_flags(inst) || is_shift_by_zero(inst) {
        return true;
    }
    flags_dead_after(instructions, idx)
}

/// Transform the instruction at `idx` to a more efficient form.
///
/// `flags_dead` must equal `flags_dead_after(instructions, idx)` (precomputed by
/// the caller so a whole pass costs O(n), not O(n²)).
///
/// Returns `Some(new_inst)` if a transformation applies, `None` otherwise.
fn transform_single(instructions: &[X86Inst], idx: usize, flags_dead: bool) -> Option<X86Inst> {
    match &instructions[idx] {
        // mov r, 0 → xor r, r (smaller encoding: 5 bytes → 2 bytes)
        // Also breaks false dependencies on modern CPUs.
        // XOR writes FLAGS where MOV does not, so this is only legal where
        // the flags state at this point is dead (RUE-152).
        X86Inst::MovRI32 { dst, imm: 0 } if flags_dead => Some(X86Inst::XorRR {
            dst: *dst,
            src: *dst,
        }),

        // cmp r, 0 → test r, r (same flags, often faster)
        // Flag-equivalent for every consumed flag: both set CF=0, OF=0 and
        // ZF/SF/PF from the operand value itself (see module docs).
        X86Inst::CmpRI { src, imm: 0 } => Some(X86Inst::TestRR {
            src1: *src,
            src2: *src,
        }),

        // cmp64 r, 0 → test64 r, r (64-bit version)
        // Must stay 64-bit: the 32-bit `test` only sets SF/ZF from the low
        // 32 bits, which broke @intCast i64->u64 range checks (RUE-146).
        X86Inst::Cmp64RI { src, imm: 0 } => Some(X86Inst::Test64RR {
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
/// The combined add produces the same final register value but can set
/// CF/OF differently than the second original add, so combining is gated on
/// the flags at that point being dead (RUE-152).
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
            if operands_equal(dst1, dst2) && flags_dead_after(instructions, i + 1) {
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
        // Must be the 64-bit test: a 32-bit `test` would only set SF/ZF from
        // the low 32 bits (RUE-146).
        match &instructions[0] {
            X86Inst::Test64RR { src1, src2 } => {
                assert!(operands_equal(src1, src2));
                assert!(matches!(src1, Operand::Physical(Reg::Rbx)));
            }
            other => panic!("Expected Test64RR, got {:?}", other),
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

    // ==================== FLAGS Hazard Tests (RUE-152) ====================

    use crate::x86_64::mir::LabelId;

    #[test]
    fn test_mov_zero_to_xor_blocked_by_live_flags() {
        // cmp sets flags; jz reads them. The mov in between must NOT become
        // xor (which would clobber the flags jz consumes).
        let mut instructions = vec![
            X86Inst::CmpRI {
                src: Operand::Physical(Reg::Rbx),
                imm: 42,
            },
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Jz {
                label: LabelId::new(0),
            },
            X86Inst::Ret,
        ];

        optimize(&mut instructions);

        assert!(
            matches!(instructions[1], X86Inst::MovRI32 { imm: 0, .. }),
            "mov r, 0 must not become xor while flags are live, got {:?}",
            instructions[1]
        );
    }

    #[test]
    fn test_mov_zero_to_xor_fires_when_flags_rewritten_first() {
        // A flags writer (test) sits between the mov and the reader, so the
        // xor's flags are dead and the rewrite STILL fires.
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::TestRR {
                src1: Operand::Physical(Reg::Rbx),
                src2: Operand::Physical(Reg::Rbx),
            },
            X86Inst::Jz {
                label: LabelId::new(0),
            },
            X86Inst::Ret,
        ];

        optimize(&mut instructions);

        assert!(
            matches!(instructions[0], X86Inst::XorRR { .. }),
            "mov r, 0 should become xor when a writer precedes the reader, got {:?}",
            instructions[0]
        );
    }

    #[test]
    fn test_mov_zero_to_xor_blocked_by_label() {
        // Flags state escapes into a join point; conservatively keep the mov.
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Label {
                id: LabelId::new(0),
            },
            X86Inst::Jz {
                label: LabelId::new(1),
            },
            X86Inst::Ret,
        ];

        optimize(&mut instructions);

        assert!(
            matches!(instructions[0], X86Inst::MovRI32 { imm: 0, .. }),
            "mov r, 0 must not become xor across a label, got {:?}",
            instructions[0]
        );
    }

    #[test]
    fn test_add_zero_kept_when_flags_consumed() {
        // add r, 0 is a register no-op but writes FLAGS; a following jz
        // consumes them, so the add must stay.
        let mut instructions = vec![
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Jz {
                label: LabelId::new(0),
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert!(matches!(instructions[0], X86Inst::AddRI { imm: 0, .. }));
    }

    #[test]
    fn test_xor_zero_kept_when_flags_consumed() {
        let mut instructions = vec![
            X86Inst::XorRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Jae {
                label: LabelId::new(0),
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert!(matches!(instructions[0], X86Inst::XorRI { imm: 0, .. }));
    }

    #[test]
    fn test_combine_adds_blocked_by_flags_reader() {
        // Combining changes CF/OF; jae reads CF, so the adds must not merge.
        let mut instructions = vec![
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 10,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 20,
            },
            X86Inst::Jae {
                label: LabelId::new(0),
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 0);
        assert_eq!(instructions.len(), 4);
        assert!(matches!(instructions[0], X86Inst::AddRI { imm: 10, .. }));
        assert!(matches!(instructions[1], X86Inst::AddRI { imm: 20, .. }));
    }

    #[test]
    fn test_cmp_zero_to_test_fires_with_flags_reader() {
        // cmp -> test is flag-equivalent on x86 and must still fire even
        // with a consumer right after.
        let mut instructions = vec![
            X86Inst::CmpRI {
                src: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::Jz {
                label: LabelId::new(0),
            },
            X86Inst::Ret,
        ];

        let changes = optimize(&mut instructions);

        assert_eq!(changes, 1);
        assert!(matches!(instructions[0], X86Inst::TestRR { .. }));
    }

    #[test]
    fn test_flags_dead_after_call() {
        // FLAGS never live across a call: the xor rewrite fires even though
        // a reader follows the call (it must consume flags set after it).
        let mut instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 0,
            },
            X86Inst::call(0),
            X86Inst::Jz {
                label: LabelId::new(0),
            },
            X86Inst::Ret,
        ];

        optimize(&mut instructions);

        assert!(matches!(instructions[0], X86Inst::XorRR { .. }));
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
