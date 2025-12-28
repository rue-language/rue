//! Constant folding optimization pass.
//!
//! This pass evaluates operations on compile-time constants, replacing
//! instructions like `add v0, v1` (where v0 and v1 are both constants)
//! with a single constant result.
//!
//! ## What gets folded
//!
//! - Binary arithmetic: add, sub, mul, div, mod
//! - Comparisons: eq, ne, lt, gt, le, ge
//! - Bitwise: and, or, xor, shl, shr
//! - Logical: and, or (on booleans)
//! - Unary: neg, not, bitnot
//!
//! ## Overflow handling
//!
//! Arithmetic operations that would overflow at runtime are NOT folded.
//! This ensures the runtime panic behavior is preserved.

use crate::{Cfg, CfgInstData, CfgValue};
use rue_air::Type;

/// Run constant folding on the CFG.
///
/// This iterates over all instructions and replaces operations on
/// constants with constant results.
pub fn run(cfg: &mut Cfg) {
    // We iterate by index since we need to look up other instructions
    // while potentially modifying the current one.
    let value_count = cfg.value_count();

    for i in 0..value_count {
        let value = CfgValue::from_raw(i as u32);
        fold_instruction(cfg, value);
    }
}

/// Try to fold a single instruction if it operates on constants.
fn fold_instruction(cfg: &mut Cfg, value: CfgValue) {
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
        }
        CfgInstData::Mul(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| checked_mul(a, b, ty))
        }
        CfgInstData::Div(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| checked_div(a, b, ty))
        }
        CfgInstData::Mod(lhs, rhs) => {
            fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| checked_mod(a, b, ty))
        }

        // Comparisons (result is always bool)
        CfgInstData::Eq(lhs, rhs) => fold_comparison(cfg, *lhs, *rhs, |a, b| a == b),
        CfgInstData::Ne(lhs, rhs) => fold_comparison(cfg, *lhs, *rhs, |a, b| a != b),
        CfgInstData::Lt(lhs, rhs) => {
            let lhs_ty = cfg.get_inst(*lhs).ty;
            fold_comparison_signed(cfg, *lhs, *rhs, lhs_ty, |a, b| a < b, |a, b| a < b)
        }
        CfgInstData::Gt(lhs, rhs) => {
            let lhs_ty = cfg.get_inst(*lhs).ty;
            fold_comparison_signed(cfg, *lhs, *rhs, lhs_ty, |a, b| a > b, |a, b| a > b)
        }
        CfgInstData::Le(lhs, rhs) => {
            let lhs_ty = cfg.get_inst(*lhs).ty;
            fold_comparison_signed(cfg, *lhs, *rhs, lhs_ty, |a, b| a <= b, |a, b| a <= b)
        }
        CfgInstData::Ge(lhs, rhs) => {
            let lhs_ty = cfg.get_inst(*lhs).ty;
            fold_comparison_signed(cfg, *lhs, *rhs, lhs_ty, |a, b| a >= b, |a, b| a >= b)
        }

        // Bitwise
        CfgInstData::BitAnd(lhs, rhs) => fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| Some(a & b)),
        CfgInstData::BitOr(lhs, rhs) => fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| Some(a | b)),
        CfgInstData::BitXor(lhs, rhs) => fold_binary_arith(cfg, *lhs, *rhs, ty, |a, b| Some(a ^ b)),
        CfgInstData::Shl(lhs, rhs) => fold_shift(cfg, *lhs, *rhs, ty, true),
        CfgInstData::Shr(lhs, rhs) => fold_shift(cfg, *lhs, *rhs, ty, false),

        // Unary
        CfgInstData::Neg(operand) => fold_unary_arith(cfg, *operand, ty, |v| checked_neg(v, ty)),
        CfgInstData::Not(operand) => fold_not(cfg, *operand),
        CfgInstData::BitNot(operand) => fold_unary_arith(cfg, *operand, ty, |v| Some(!v)),

        // Everything else is not foldable
        _ => None,
    };

    // If we computed a folded result, replace the instruction
    if let Some(new_data) = folded {
        let inst = cfg.get_inst_mut(value);
        inst.data = new_data;
        inst.span = span; // Preserve original span
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

/// Try to fold a comparison that needs signed semantics for signed types.
fn fold_comparison_signed<Fs, Fu>(
    cfg: &Cfg,
    lhs: CfgValue,
    rhs: CfgValue,
    ty: Type,
    signed_op: Fs,
    unsigned_op: Fu,
) -> Option<CfgInstData>
where
    Fs: FnOnce(i64, i64) -> bool,
    Fu: FnOnce(u64, u64) -> bool,
{
    let lhs_val = get_const_int(cfg, lhs)?;
    let rhs_val = get_const_int(cfg, rhs)?;

    let result = if is_signed(ty) {
        // Sign-extend the values to i64 based on the type
        let lhs_signed = sign_extend(lhs_val, ty) as i64;
        let rhs_signed = sign_extend(rhs_val, ty) as i64;
        signed_op(lhs_signed, rhs_signed)
    } else {
        unsigned_op(lhs_val, rhs_val)
    };

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

    // Shift amount must be less than the bit width
    let bits = type_bits(ty);
    if rhs_val >= bits as u64 {
        // Would be UB at runtime - don't fold
        return None;
    }

    let result = if is_left {
        lhs_val << rhs_val
    } else if is_signed(ty) {
        // Arithmetic right shift for signed types
        ((lhs_val as i64) >> rhs_val) as u64
    } else {
        // Logical right shift for unsigned types
        lhs_val >> rhs_val
    };

    // Mask to the type's bit width
    let result = mask_to_type(result, ty);
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

/// Check if a type is signed.
fn is_signed(ty: Type) -> bool {
    matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64)
}

/// Get the bit width of a type.
fn type_bits(ty: Type) -> u32 {
    match ty {
        Type::I8 | Type::U8 => 8,
        Type::I16 | Type::U16 => 16,
        Type::I32 | Type::U32 => 32,
        Type::I64 | Type::U64 => 64,
        Type::Bool => 1,
        _ => 64, // Default for other types
    }
}

/// Mask a value to the bit width of a type.
fn mask_to_type(val: u64, ty: Type) -> u64 {
    match ty {
        Type::I8 | Type::U8 => val & 0xFF,
        Type::I16 | Type::U16 => val & 0xFFFF,
        Type::I32 | Type::U32 => val & 0xFFFF_FFFF,
        Type::I64 | Type::U64 => val,
        _ => val,
    }
}

// ============================================================================
// Checked arithmetic (returns None if would overflow)
// ============================================================================

fn checked_add(a: u64, b: u64, ty: Type) -> Option<u64> {
    if is_signed(ty) {
        let (a, b) = sign_extend_operands(a, b, ty);
        let result = (a as i64).checked_add(b as i64)?;
        // Check for overflow in the target type
        if !fits_in_signed_type(result, ty) {
            return None;
        }
        Some(result as u64)
    } else {
        let result = a.checked_add(b)?;
        if !fits_in_unsigned_type(result, ty) {
            return None;
        }
        Some(result)
    }
}

fn checked_sub(a: u64, b: u64, ty: Type) -> Option<u64> {
    if is_signed(ty) {
        let (a, b) = sign_extend_operands(a, b, ty);
        let result = (a as i64).checked_sub(b as i64)?;
        if !fits_in_signed_type(result, ty) {
            return None;
        }
        Some(result as u64)
    } else {
        a.checked_sub(b)
    }
}

fn checked_mul(a: u64, b: u64, ty: Type) -> Option<u64> {
    if is_signed(ty) {
        let (a, b) = sign_extend_operands(a, b, ty);
        let result = (a as i64).checked_mul(b as i64)?;
        if !fits_in_signed_type(result, ty) {
            return None;
        }
        Some(result as u64)
    } else {
        let result = a.checked_mul(b)?;
        if !fits_in_unsigned_type(result, ty) {
            return None;
        }
        Some(result)
    }
}

fn checked_div(a: u64, b: u64, ty: Type) -> Option<u64> {
    if b == 0 {
        return None; // Division by zero - don't fold
    }
    if is_signed(ty) {
        let (a, b) = sign_extend_operands(a, b, ty);
        // Check for i64::MIN / -1 which overflows
        let result = (a as i64).checked_div(b as i64)?;
        Some(result as u64)
    } else {
        Some(a / b)
    }
}

fn checked_mod(a: u64, b: u64, ty: Type) -> Option<u64> {
    if b == 0 {
        return None; // Division by zero - don't fold
    }
    if is_signed(ty) {
        let (a, b) = sign_extend_operands(a, b, ty);
        // Check for i64::MIN % -1 which overflows
        let result = (a as i64).checked_rem(b as i64)?;
        Some(result as u64)
    } else {
        Some(a % b)
    }
}

fn checked_neg(a: u64, ty: Type) -> Option<u64> {
    if is_signed(ty) {
        let a = sign_extend(a, ty);
        let result = (a as i64).checked_neg()?;
        if !fits_in_signed_type(result, ty) {
            return None;
        }
        Some(result as u64)
    } else {
        // Unsigned negation: 0 - a, wrapping
        // Only 0 doesn't overflow
        if a == 0 {
            Some(0)
        } else {
            None // Would underflow
        }
    }
}

/// Sign-extend a value based on its type.
fn sign_extend(val: u64, ty: Type) -> u64 {
    match ty {
        Type::I8 => (val as i8) as i64 as u64,
        Type::I16 => (val as i16) as i64 as u64,
        Type::I32 => (val as i32) as i64 as u64,
        Type::I64 => val,
        _ => val,
    }
}

/// Sign-extend both operands.
fn sign_extend_operands(a: u64, b: u64, ty: Type) -> (u64, u64) {
    (sign_extend(a, ty), sign_extend(b, ty))
}

/// Check if a signed result fits in the target type.
fn fits_in_signed_type(val: i64, ty: Type) -> bool {
    match ty {
        Type::I8 => val >= i8::MIN as i64 && val <= i8::MAX as i64,
        Type::I16 => val >= i16::MIN as i64 && val <= i16::MAX as i64,
        Type::I32 => val >= i32::MIN as i64 && val <= i32::MAX as i64,
        Type::I64 => true, // i64 can hold any i64
        _ => true,
    }
}

/// Check if an unsigned result fits in the target type.
fn fits_in_unsigned_type(val: u64, ty: Type) -> bool {
    match ty {
        Type::U8 => val <= u8::MAX as u64,
        Type::U16 => val <= u16::MAX as u64,
        Type::U32 => val <= u32::MAX as u64,
        Type::U64 => true,
        _ => true,
    }
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

    fn add_bool_const(cfg: &mut Cfg, val: bool) -> CfgValue {
        let entry = cfg.entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::BoolConst(val),
                ty: Type::Bool,
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

        run(&mut cfg);

        // The add should be folded to const 5
        match &cfg.get_inst(add).data {
            CfgInstData::Const(5) => {}
            other => panic!("Expected Const(5), got {:?}", other),
        }
    }

    #[test]
    fn test_fold_overflow_not_folded() {
        let mut cfg = make_cfg();
        // i32::MAX + 1 should overflow
        let c1 = add_const(&mut cfg, i32::MAX as u64, Type::I32);
        let c2 = add_const(&mut cfg, 1, Type::I32);
        let add = add_add(&mut cfg, c1, c2, Type::I32);
        finalize_cfg(&mut cfg, add);

        run(&mut cfg);

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
                ty: Type::Bool,
                span: Span::new(0, 0),
            },
        );
        finalize_cfg(&mut cfg, lt_val);

        run(&mut cfg);

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
                ty: Type::Bool,
                span: Span::new(0, 0),
            },
        );
        finalize_cfg(&mut cfg, lt_val);

        run(&mut cfg);

        // -1 < 0 = true (signed comparison)
        match &cfg.get_inst(lt_val).data {
            CfgInstData::BoolConst(true) => {}
            other => panic!("Expected BoolConst(true), got {:?}", other),
        }
    }
}
