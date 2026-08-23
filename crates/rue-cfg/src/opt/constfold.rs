//! Constant folding optimization pass.
//!
//! This pass evaluates operations on compile-time constants, replacing
//! instructions like `add v0, v1` (where v0 and v1 are both constants)
//! with a single constant result.
//!
//! ## What gets folded
//!
//! - Binary arithmetic: add, sub, mul, wrapping add/sub/mul, div, mod
//! - Comparisons: eq, ne, lt, gt, le, ge
//! - Bitwise: and, or, xor, shl, shr
//! - Logical: and, or (on booleans)
//! - Unary: neg, not, bitnot
//!
//! ## Overflow handling
//!
//! Arithmetic operations that would overflow at runtime are NOT folded.
//! This ensures the runtime panic behavior is preserved.

use std::cmp::Ordering;

use crate::{Cfg, CfgInstData, CfgValue};
use rue_air::{EnumId, Type};

/// Try to fold a single instruction if it operates on constants.
/// Returns `true` if the instruction was replaced by a constant.
///
/// This is the fold kernel; the sparse worklist driver in
/// [`super::constopt`] decides which instructions to attempt and when to
/// revisit them (RUE-794).
pub(super) fn fold_instruction(cfg: &mut Cfg, value: CfgValue) -> bool {
    // Get the instruction data and type
    let inst = cfg.get_inst(value);
    let ty = inst.ty;
    let span = inst.span;

    // Try to compute a folded result
    let folded = match &inst.data {
        // Binary arithmetic
        CfgInstData::Add(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| checked_add(a, b, ty))
        }
        CfgInstData::Sub(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| checked_sub(a, b, ty))
                // x - x is 0 with no possible overflow, for any x (RUE-912).
                .or_else(|| (lhs == rhs).then_some(CfgInstData::Const(0)))
        }
        CfgInstData::Mul(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| checked_mul(a, b, ty))
                // x * 0 is 0 with no possible overflow, even for non-constant
                // x; x's own instruction stays behind for DCE's trap-aware
                // liveness rules (RUE-912).
                .or_else(|| {
                    (get_const_int(cfg, *lhs) == Some(0) || get_const_int(cfg, *rhs) == Some(0))
                        .then_some(CfgInstData::Const(0))
                })
        }
        CfgInstData::WrappingAdd(lhs, rhs) => fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| {
            Some(ty.integer_semantics()?.wrapping_add_u64(a, b))
        }),
        CfgInstData::WrappingSub(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| {
                Some(ty.integer_semantics()?.wrapping_sub_u64(a, b))
            })
            // x wrapping_sub x is always zero and cannot trap.
            .or_else(|| (lhs == rhs).then_some(CfgInstData::Const(0)))
        }
        CfgInstData::WrappingMul(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| {
                Some(ty.integer_semantics()?.wrapping_mul_u64(a, b))
            })
            // Wrapping multiplication never traps, so either zero operand
            // annihilates the result even when the other operand is dynamic.
            .or_else(|| {
                (get_const_int(cfg, *lhs) == Some(0) || get_const_int(cfg, *rhs) == Some(0))
                    .then_some(CfgInstData::Const(0))
            })
        }
        CfgInstData::Div(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| checked_div(a, b, ty))
        }
        CfgInstData::Mod(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| checked_mod(a, b, ty))
                // x % 1 is 0 for any x: the divisor is a nonzero constant and
                // the implied quotient is x itself, so neither trap can fire.
                // (x % -1 is NOT safe — MIN % -1 traps — and -1's canonical
                // constant is all-ones, which this test rejects.)
                .or_else(|| (get_const_int(cfg, *rhs) == Some(1)).then_some(CfgInstData::Const(0)))
        }

        // Comparisons (result is always bool)
        CfgInstData::Eq(lhs, rhs) => {
            // Try integer comparison first, then enum variant comparison
            fold_comparison(cfg, *lhs, *rhs, |a, b| a == b)
                .or_else(|| fold_enum_comparison(cfg, *lhs, *rhs, |v1, v2| v1 == v2))
        }
        CfgInstData::Ne(lhs, rhs) => {
            // Try integer comparison first, then enum variant comparison
            fold_comparison(cfg, *lhs, *rhs, |a, b| a != b)
                .or_else(|| fold_enum_comparison(cfg, *lhs, *rhs, |v1, v2| v1 != v2))
        }
        CfgInstData::Lt(lhs, rhs) => {
            let lhs_ty = cfg.get_inst(*lhs).ty;
            fold_comparison_ordered(cfg, *lhs, *rhs, lhs_ty, |ordering| {
                ordering == Ordering::Less
            })
        }
        CfgInstData::Gt(lhs, rhs) => {
            let lhs_ty = cfg.get_inst(*lhs).ty;
            fold_comparison_ordered(cfg, *lhs, *rhs, lhs_ty, |ordering| {
                ordering == Ordering::Greater
            })
        }
        CfgInstData::Le(lhs, rhs) => {
            let lhs_ty = cfg.get_inst(*lhs).ty;
            fold_comparison_ordered(cfg, *lhs, *rhs, lhs_ty, |ordering| {
                ordering != Ordering::Greater
            })
        }
        CfgInstData::Ge(lhs, rhs) => {
            let lhs_ty = cfg.get_inst(*lhs).ty;
            fold_comparison_ordered(cfg, *lhs, *rhs, lhs_ty, |ordering| {
                ordering != Ordering::Less
            })
        }

        // Bitwise
        CfgInstData::BitAnd(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| Some(a & b))
                // x & 0 is 0 for any x; bitwise ops never trap (RUE-912).
                .or_else(|| {
                    (get_const_int(cfg, *lhs) == Some(0) || get_const_int(cfg, *rhs) == Some(0))
                        .then_some(CfgInstData::Const(0))
                })
        }
        CfgInstData::BitOr(lhs, rhs) => fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| Some(a | b)),
        CfgInstData::BitXor(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| Some(a ^ b))
                // x ^ x is 0 for any x; bitwise ops never trap (RUE-912).
                .or_else(|| (lhs == rhs).then_some(CfgInstData::Const(0)))
        }
        CfgInstData::Shl(lhs, rhs) => fold_shift(cfg, *lhs, *rhs, ty, true),
        CfgInstData::Shr(lhs, rhs) => fold_shift(cfg, *lhs, *rhs, ty, false),

        // Unary
        CfgInstData::Neg(operand) => fold_unary_arith(cfg, *operand, ty, |v| checked_neg(v, ty)),
        CfgInstData::Not(operand) => fold_not(cfg, *operand),
        // `!v` flips all 64 stored bits, but the operation is defined at the
        // operand's width; truncate so the folded constant matches what the
        // runtime computes (RUE-59).
        CfgInstData::BitNot(operand) => fold_unary_arith(cfg, *operand, ty, |v| {
            Some(ty.integer_semantics()?.bitnot_u64(v))
        }),

        // Everything else is not foldable
        _ => None,
    };

    // If we computed a folded result, replace the instruction
    if let Some(new_data) = folded {
        let inst = cfg.get_inst_mut(value);
        inst.data = new_data;
        inst.span = span; // Preserve original span
        true
    } else {
        false
    }
}

/// Try to fold a binary arithmetic operation on two constant operands.
fn fold_binary_arith<F>(
    cfg: &Cfg,
    lhs: CfgValue,
    rhs: CfgValue,
    _ty: Type,
    op: F,
) -> Option<CfgInstData>
where
    F: FnOnce(u64, u64) -> Option<u64>,
{
    let lhs_val = get_const_int(cfg, lhs)?;
    let rhs_val = get_const_int(cfg, rhs)?;
    let result = op(lhs_val, rhs_val)?;
    Some(CfgInstData::Const(result))
}

/// Try to fold a comparison on two constant operands.
fn fold_comparison<F>(cfg: &Cfg, lhs: CfgValue, rhs: CfgValue, op: F) -> Option<CfgInstData>
where
    F: FnOnce(u64, u64) -> bool,
{
    let lhs_val = get_const_int(cfg, lhs)?;
    let rhs_val = get_const_int(cfg, rhs)?;
    let result = op(lhs_val, rhs_val);
    Some(CfgInstData::BoolConst(result))
}

/// Try to fold an enum variant comparison on two constant operands.
///
/// This enables dead code elimination for platform-specific code like:
/// ```ignore
/// if @target_arch() == Arch::X86_64 { ... }
/// ```
fn fold_enum_comparison<F>(cfg: &Cfg, lhs: CfgValue, rhs: CfgValue, op: F) -> Option<CfgInstData>
where
    F: FnOnce(u32, u32) -> bool,
{
    let (lhs_enum_id, lhs_variant, lhs_payload_len) = get_enum_variant(cfg, lhs)?;
    let (rhs_enum_id, rhs_variant, rhs_payload_len) = get_enum_variant(cfg, rhs)?;

    // Only fold when both operands are the same enum AND payload-less: comparing
    // the variant index alone is correct only for fieldless variants (e.g.
    // `@target_arch() == Arch::X86_64`). For payload-carrying variants,
    // structural equality must also compare the payloads — `Opt::Some(5)` and
    // `Opt::Some(6)` share variant index 1 but are NOT equal — so those must fall
    // through to codegen's `emit_aggregate_equality` rather than folding on the
    // tag alone (RUE-348).
    if lhs_enum_id == rhs_enum_id && lhs_payload_len == 0 && rhs_payload_len == 0 {
        let result = op(lhs_variant, rhs_variant);
        Some(CfgInstData::BoolConst(result))
    } else {
        // Different enum types (a type error, left to pass through for error
        // reporting) or a payload-carrying comparison (folded structurally by
        // codegen instead). Do not fold.
        None
    }
}

/// Try to fold an ordered integer comparison through the shared semantics
/// kernel.  `compare_u64` interprets the same canonical register image as the
/// language does, including signed narrow values.
fn fold_comparison_ordered<F>(
    cfg: &Cfg,
    lhs: CfgValue,
    rhs: CfgValue,
    ty: Type,
    op: F,
) -> Option<CfgInstData>
where
    F: FnOnce(Ordering) -> bool,
{
    let lhs_val = get_const_int(cfg, lhs)?;
    let rhs_val = get_const_int(cfg, rhs)?;

    let integer = ty.integer_semantics()?;
    let result = op(integer.compare_u64(lhs_val, rhs_val));

    Some(CfgInstData::BoolConst(result))
}

/// Try to fold a shift operation.
fn fold_shift(
    cfg: &Cfg,
    lhs: CfgValue,
    rhs: CfgValue,
    ty: Type,
    is_left: bool,
) -> Option<CfgInstData> {
    let lhs_val = get_const_int(cfg, lhs)?;
    let rhs_val = get_const_int(cfg, rhs)?;

    let integer = ty.integer_semantics()?;
    let result = integer.shift_u64(lhs_val, rhs_val, is_left);
    Some(CfgInstData::Const(result))
}

/// Try to fold a unary arithmetic operation on a constant operand.
fn fold_unary_arith<F>(cfg: &Cfg, operand: CfgValue, _ty: Type, op: F) -> Option<CfgInstData>
where
    F: FnOnce(u64) -> Option<u64>,
{
    let val = get_const_int(cfg, operand)?;
    let result = op(val)?;
    Some(CfgInstData::Const(result))
}

/// Try to fold logical not on a constant boolean.
fn fold_not(cfg: &Cfg, operand: CfgValue) -> Option<CfgInstData> {
    let val = get_const_bool(cfg, operand)?;
    Some(CfgInstData::BoolConst(!val))
}

// ============================================================================
// Helper functions
// ============================================================================

/// Get the constant integer value of an instruction, if it's a Const.
fn get_const_int(cfg: &Cfg, value: CfgValue) -> Option<u64> {
    match &cfg.get_inst(value).data {
        CfgInstData::Const(v) => Some(*v),
        _ => None,
    }
}

/// Get the constant boolean value of an instruction, if it's a BoolConst.
fn get_const_bool(cfg: &Cfg, value: CfgValue) -> Option<bool> {
    match &cfg.get_inst(value).data {
        CfgInstData::BoolConst(v) => Some(*v),
        _ => None,
    }
}

/// Get the enum variant info of an instruction, if it's an EnumVariant.
fn get_enum_variant(cfg: &Cfg, value: CfgValue) -> Option<(EnumId, u32, u32)> {
    match &cfg.get_inst(value).data {
        CfgInstData::EnumVariant {
            enum_id,
            variant_index,
            payload,
            ..
        } => Some((
            *enum_id,
            *variant_index,
            cfg.enum_payload(payload).len() as u32,
        )),
        _ => None,
    }
}

// ============================================================================
// Checked arithmetic (returns None if would overflow)
// ============================================================================

fn checked_add(a: u64, b: u64, ty: Type) -> Option<u64> {
    ty.integer_semantics()?.checked_add_u64(a, b)
}

fn checked_sub(a: u64, b: u64, ty: Type) -> Option<u64> {
    ty.integer_semantics()?.checked_sub_u64(a, b)
}

fn checked_mul(a: u64, b: u64, ty: Type) -> Option<u64> {
    ty.integer_semantics()?.checked_mul_u64(a, b)
}

fn checked_div(a: u64, b: u64, ty: Type) -> Option<u64> {
    if b == 0 {
        return None; // Division by zero - don't fold
    }
    ty.integer_semantics()?.checked_div_u64(a, b)
}

fn checked_mod(a: u64, b: u64, ty: Type) -> Option<u64> {
    if b == 0 {
        return None; // Division by zero - don't fold
    }
    ty.integer_semantics()?.checked_rem_u64(a, b)
}

fn checked_neg(a: u64, ty: Type) -> Option<u64> {
    ty.integer_semantics()?.checked_neg_u64(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cfg, CfgInst, CfgInstData, Terminator};
    use rue_span::Span;

    fn make_cfg() -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg
    }

    fn add_const(cfg: &mut Cfg, val: u64, ty: Type) -> CfgValue {
        let entry = cfg.entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(val),
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    fn add_add(cfg: &mut Cfg, lhs: CfgValue, rhs: CfgValue, ty: Type) -> CfgValue {
        let entry = cfg.entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Add(lhs, rhs),
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    fn assert_wrapping_fold(
        ty: Type,
        lhs: u64,
        rhs: u64,
        op: fn(CfgValue, CfgValue) -> CfgInstData,
        expected: u64,
    ) {
        let mut cfg = make_cfg();
        let lhs = add_const(&mut cfg, lhs, ty);
        let rhs = add_const(&mut cfg, rhs, ty);
        let entry = cfg.entry;
        let result = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: op(lhs, rhs),
                ty,
                span: Span::new(0, 0),
            },
        );
        finalize_cfg(&mut cfg, result);

        crate::opt::constopt::run(&mut cfg);

        assert!(
            matches!(cfg.get_inst(result).data, CfgInstData::Const(v) if v == expected),
            "{ty:?}: expected Const({expected}), got {:?}",
            cfg.get_inst(result).data
        );
    }

    fn finalize_cfg(cfg: &mut Cfg, ret_val: CfgValue) {
        let entry = cfg.entry;
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(ret_val),
            },
        );
    }

    #[test]
    fn test_fold_add() {
        let mut cfg = make_cfg();
        let c1 = add_const(&mut cfg, 2, Type::I32);
        let c2 = add_const(&mut cfg, 3, Type::I32);
        let add = add_add(&mut cfg, c1, c2, Type::I32);
        finalize_cfg(&mut cfg, add);

        crate::opt::constopt::run(&mut cfg);

        // The add should be folded to const 5
        match &cfg.get_inst(add).data {
            CfgInstData::Const(5) => {}
            other => panic!("Expected Const(5), got {:?}", other),
        }
    }

    #[test]
    fn test_fold_wrapping_arithmetic_all_integer_types() {
        #[allow(clippy::type_complexity)]
        let cases: [(Type, u64, u64, u64, u64, u64, u64); 8] = [
            (
                Type::I8,
                i8::MAX as u64,
                (i8::MIN as i64) as u64,
                (i8::MIN as i64) as u64,
                i8::MAX as u64,
                64,
                (i8::MIN as i64) as u64,
            ),
            (
                Type::I16,
                i16::MAX as u64,
                (i16::MIN as i64) as u64,
                (i16::MIN as i64) as u64,
                i16::MAX as u64,
                16_384,
                (i16::MIN as i64) as u64,
            ),
            (
                Type::I32,
                i32::MAX as u64,
                (i32::MIN as i64) as u64,
                (i32::MIN as i64) as u64,
                i32::MAX as u64,
                1_073_741_824,
                (i32::MIN as i64) as u64,
            ),
            (
                Type::I64,
                i64::MAX as u64,
                i64::MIN as u64,
                i64::MIN as u64,
                i64::MAX as u64,
                4_611_686_018_427_387_904,
                i64::MIN as u64,
            ),
            (Type::U8, u8::MAX as u64, 0, 0, u8::MAX as u64, 128, 0),
            (Type::U16, u16::MAX as u64, 0, 0, u16::MAX as u64, 32_768, 0),
            (
                Type::U32,
                u32::MAX as u64,
                0,
                0,
                u32::MAX as u64,
                2_147_483_648,
                0,
            ),
            (
                Type::U64,
                u64::MAX,
                0,
                0,
                u64::MAX,
                9_223_372_036_854_775_808,
                0,
            ),
        ];

        for (ty, add_lhs, add_expected, sub_lhs, sub_expected, mul_lhs, mul_expected) in cases {
            assert_wrapping_fold(ty, add_lhs, 1, CfgInstData::WrappingAdd, add_expected);
            assert_wrapping_fold(ty, sub_lhs, 1, CfgInstData::WrappingSub, sub_expected);
            assert_wrapping_fold(ty, mul_lhs, 2, CfgInstData::WrappingMul, mul_expected);
        }
    }

    #[test]
    fn test_fold_dynamic_wrapping_annihilators() {
        let mut cfg = make_cfg();
        let x = cfg.add_inst_to_block(
            cfg.entry,
            CfgInst {
                data: CfgInstData::Param { index: 0 },
                ty: Type::U32,
                span: Span::new(0, 0),
            },
        );
        let zero = add_const(&mut cfg, 0, Type::U32);
        let sub = cfg.add_inst_to_block(
            cfg.entry,
            CfgInst {
                data: CfgInstData::WrappingSub(x, x),
                ty: Type::U32,
                span: Span::new(0, 0),
            },
        );
        let mul = cfg.add_inst_to_block(
            cfg.entry,
            CfgInst {
                data: CfgInstData::WrappingMul(sub, zero),
                ty: Type::U32,
                span: Span::new(0, 0),
            },
        );
        finalize_cfg(&mut cfg, mul);

        crate::opt::constopt::run(&mut cfg);

        assert!(matches!(cfg.get_inst(sub).data, CfgInstData::Const(0)));
        assert!(matches!(cfg.get_inst(mul).data, CfgInstData::Const(0)));
    }

    #[test]
    fn test_fold_overflow_not_folded() {
        let mut cfg = make_cfg();
        // i32::MAX + 1 should overflow
        let c1 = add_const(&mut cfg, i32::MAX as u64, Type::I32);
        let c2 = add_const(&mut cfg, 1, Type::I32);
        let add = add_add(&mut cfg, c1, c2, Type::I32);
        finalize_cfg(&mut cfg, add);

        crate::opt::constopt::run(&mut cfg);

        // The add should NOT be folded (would overflow at runtime)
        match &cfg.get_inst(add).data {
            CfgInstData::Add(_, _) => {}
            other => panic!("Expected Add to remain unfold, got {:?}", other),
        }
    }

    #[test]
    fn test_fold_comparison() {
        let mut cfg = make_cfg();
        let c1 = add_const(&mut cfg, 5, Type::I32);
        let c2 = add_const(&mut cfg, 3, Type::I32);
        let entry = cfg.entry;
        let lt_val = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Lt(c1, c2),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        finalize_cfg(&mut cfg, lt_val);

        crate::opt::constopt::run(&mut cfg);

        // 5 < 3 = false
        match &cfg.get_inst(lt_val).data {
            CfgInstData::BoolConst(false) => {}
            other => panic!("Expected BoolConst(false), got {:?}", other),
        }
    }

    #[test]
    fn test_fold_signed_comparison() {
        let mut cfg = make_cfg();
        // -1 as i32 (all bits set in low 32 bits)
        let c1 = add_const(&mut cfg, (-1i32) as u32 as u64, Type::I32);
        let c2 = add_const(&mut cfg, 0, Type::I32);
        let entry = cfg.entry;
        let lt_val = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Lt(c1, c2),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        finalize_cfg(&mut cfg, lt_val);

        crate::opt::constopt::run(&mut cfg);

        // -1 < 0 = true (signed comparison)
        match &cfg.get_inst(lt_val).data {
            CfgInstData::BoolConst(true) => {}
            other => panic!("Expected BoolConst(true), got {:?}", other),
        }
    }

    fn add_shr(cfg: &mut Cfg, lhs: CfgValue, rhs: CfgValue, ty: Type) -> CfgValue {
        let entry = cfg.entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Shr(lhs, rhs),
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    #[test]
    fn test_fold_signed_shift_right_i8_negative_one() {
        // Test that arithmetic right shift of -1 as i8 gives -1
        // This tests the sign extension fix for narrow types.
        let mut cfg = make_cfg();
        // -1 as i8 is 0xFF, stored as u64 it's 255
        let c1 = add_const(&mut cfg, (-1i8) as u8 as u64, Type::I8);
        let c2 = add_const(&mut cfg, 1, Type::I8);
        let shr = add_shr(&mut cfg, c1, c2, Type::I8);
        finalize_cfg(&mut cfg, shr);

        crate::opt::constopt::run(&mut cfg);

        // -1 >> 1 should be -1, stored canonically (sign-extended to 64 bits)
        match &cfg.get_inst(shr).data {
            CfgInstData::Const(val) => {
                assert_eq!(
                    *val,
                    (-1i64) as u64,
                    "Expected -1 (sign-extended), got 0x{:X}",
                    val
                );
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    #[test]
    fn test_fold_signed_shift_right_i8_negative_eight() {
        // Test that arithmetic right shift of -8 as i8 by 2 gives -2
        // -8 as i8 is 0xF8, -8 >> 2 = -2 = 0xFE
        let mut cfg = make_cfg();
        let c1 = add_const(&mut cfg, (-8i8) as u8 as u64, Type::I8);
        let c2 = add_const(&mut cfg, 2, Type::I8);
        let shr = add_shr(&mut cfg, c1, c2, Type::I8);
        finalize_cfg(&mut cfg, shr);

        crate::opt::constopt::run(&mut cfg);

        // -8 >> 2 should be -2, stored canonically (sign-extended to 64 bits)
        match &cfg.get_inst(shr).data {
            CfgInstData::Const(val) => {
                assert_eq!(
                    *val,
                    (-2i64) as u64,
                    "Expected -2 (sign-extended), got 0x{:X}",
                    val
                );
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    #[test]
    fn test_fold_signed_shift_right_i16_negative_one() {
        // Test that arithmetic right shift of -1 as i16 gives -1
        let mut cfg = make_cfg();
        let c1 = add_const(&mut cfg, (-1i16) as u16 as u64, Type::I16);
        let c2 = add_const(&mut cfg, 4, Type::I16);
        let shr = add_shr(&mut cfg, c1, c2, Type::I16);
        finalize_cfg(&mut cfg, shr);

        crate::opt::constopt::run(&mut cfg);

        // -1 >> 4 should be -1, stored canonically (sign-extended to 64 bits)
        match &cfg.get_inst(shr).data {
            CfgInstData::Const(val) => {
                assert_eq!(
                    *val,
                    (-1i64) as u64,
                    "Expected -1 (sign-extended), got 0x{:X}",
                    val
                );
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    #[test]
    fn test_fold_signed_shift_right_i32_negative_one() {
        // Test that arithmetic right shift of -1 as i32 gives -1
        let mut cfg = make_cfg();
        let c1 = add_const(&mut cfg, (-1i32) as u32 as u64, Type::I32);
        let c2 = add_const(&mut cfg, 8, Type::I32);
        let shr = add_shr(&mut cfg, c1, c2, Type::I32);
        finalize_cfg(&mut cfg, shr);

        crate::opt::constopt::run(&mut cfg);

        // -1 >> 8 should be -1, stored canonically (sign-extended to 64 bits)
        match &cfg.get_inst(shr).data {
            CfgInstData::Const(val) => {
                assert_eq!(
                    *val,
                    (-1i64) as u64,
                    "Expected -1 (sign-extended), got 0x{:X}",
                    val
                );
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    #[test]
    fn test_fold_unsigned_shift_right_u8() {
        // Test that logical right shift of 0xFF as u8 gives 0x7F (not 0xFF)
        let mut cfg = make_cfg();
        let c1 = add_const(&mut cfg, 0xFF, Type::U8);
        let c2 = add_const(&mut cfg, 1, Type::U8);
        let shr = add_shr(&mut cfg, c1, c2, Type::U8);
        finalize_cfg(&mut cfg, shr);

        crate::opt::constopt::run(&mut cfg);

        // 0xFF >> 1 should be 0x7F (logical shift fills with 0)
        match &cfg.get_inst(shr).data {
            CfgInstData::Const(val) => {
                assert_eq!(*val, 0x7F, "Expected 0x7F, got 0x{:X}", val);
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    #[test]
    fn test_fold_shift_count_is_masked_at_operand_width() {
        let mut cfg = make_cfg();
        let c1 = add_const(&mut cfg, 0x81, Type::U8);
        let c2 = add_const(&mut cfg, 8, Type::U8);
        let shr = add_shr(&mut cfg, c1, c2, Type::U8);
        finalize_cfg(&mut cfg, shr);

        crate::opt::constopt::run(&mut cfg);

        match &cfg.get_inst(shr).data {
            CfgInstData::Const(value) => {
                assert_eq!(*value, 0x81, "8 is masked to shift count zero for u8");
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    fn add_bitnot(cfg: &mut Cfg, operand: CfgValue, ty: Type) -> CfgValue {
        let entry = cfg.entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::BitNot(operand),
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    #[test]
    fn test_fold_bitnot_u32_masked() {
        // ~0u32 must fold to 0xFFFF_FFFF, not the unmasked 64-bit !0 (RUE-59).
        let mut cfg = make_cfg();
        let c = add_const(&mut cfg, 0, Type::U32);
        let not = add_bitnot(&mut cfg, c, Type::U32);
        finalize_cfg(&mut cfg, not);

        crate::opt::constopt::run(&mut cfg);

        match &cfg.get_inst(not).data {
            CfgInstData::Const(val) => {
                assert_eq!(*val, 0xFFFF_FFFF, "Expected 0xFFFFFFFF, got 0x{:X}", val);
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    #[test]
    fn test_fold_bitnot_u8_masked() {
        // ~5u8 = 250, masked to 8 bits.
        let mut cfg = make_cfg();
        let c = add_const(&mut cfg, 5, Type::U8);
        let not = add_bitnot(&mut cfg, c, Type::U8);
        finalize_cfg(&mut cfg, not);

        crate::opt::constopt::run(&mut cfg);

        match &cfg.get_inst(not).data {
            CfgInstData::Const(val) => {
                assert_eq!(*val, 250, "Expected 250, got {}", val);
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    #[test]
    fn test_fold_bitnot_i32_sign_extended() {
        // ~5i32 = -6, stored canonically (sign-extended to 64 bits).
        let mut cfg = make_cfg();
        let c = add_const(&mut cfg, 5, Type::I32);
        let not = add_bitnot(&mut cfg, c, Type::I32);
        finalize_cfg(&mut cfg, not);

        crate::opt::constopt::run(&mut cfg);

        match &cfg.get_inst(not).data {
            CfgInstData::Const(val) => {
                assert_eq!(
                    *val,
                    (-6i64) as u64,
                    "Expected -6 (sign-extended), got 0x{:X}",
                    val
                );
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    #[test]
    fn test_fold_bitnot_u64_full_width() {
        // ~0u64 = u64::MAX; no truncation at full width.
        let mut cfg = make_cfg();
        let c = add_const(&mut cfg, 0, Type::U64);
        let not = add_bitnot(&mut cfg, c, Type::U64);
        finalize_cfg(&mut cfg, not);

        crate::opt::constopt::run(&mut cfg);

        match &cfg.get_inst(not).data {
            CfgInstData::Const(val) => {
                assert_eq!(*val, u64::MAX, "Expected u64::MAX, got 0x{:X}", val);
            }
            other => panic!("Expected Const, got {:?}", other),
        }
    }

    // MIN / -1 and MIN % -1 overflow (the quotient -MIN is unrepresentable)
    // and must NOT fold, so the runtime trap survives -O1+ (RUE-147).
    #[test]
    fn test_checked_div_min_by_neg1_refuses_fold() {
        for (min, ty) in [
            (i8::MIN as i64 as u64, Type::I8),
            (i16::MIN as i64 as u64, Type::I16),
            (i32::MIN as i64 as u64, Type::I32),
            (i64::MIN as u64, Type::I64),
        ] {
            let neg1 = -1i64 as u64;
            assert_eq!(checked_div(min, neg1, ty), None, "div MIN/-1 for {ty:?}");
            assert_eq!(checked_mod(min, neg1, ty), None, "mod MIN/-1 for {ty:?}");
        }
    }

    #[test]
    fn test_checked_div_mod_near_min_still_folds() {
        // MIN / 1, (MIN+1) / -1, and MIN % 2 are all representable.
        let min = i32::MIN as i64 as u64;
        let neg1 = -1i64 as u64;
        assert_eq!(checked_div(min, 1, Type::I32), Some(min));
        assert_eq!(
            checked_div(min.wrapping_add(1), neg1, Type::I32),
            Some(i32::MAX as u64)
        );
        assert_eq!(checked_mod(min, 2, Type::I32), Some(0));
    }
}
