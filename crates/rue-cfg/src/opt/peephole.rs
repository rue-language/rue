//! Peephole algebraic simplification with trap-exact scoping (RUE-912).
//!
//! Two kinds of local rewrite run after the sparse constant driver, both
//! restricted to shapes that are *exactly* trap-equivalent to the original:
//!
//! * **Strength reduction**: unsigned `x / 2^k` becomes `x >> k` and
//!   unsigned `x % 2^k` becomes `x & (2^k - 1)`. Unsigned division by a
//!   nonzero constant cannot trap and neither can shifts or masks, so the
//!   rewrite is exact. Signed division is deliberately NOT reduced — an
//!   arithmetic shift rounds toward negative infinity while Rue division
//!   rounds toward zero. `x * 2^k` is likewise NOT reduced to a shift:
//!   multiplication traps on overflow and shifts do not, so the rewrite
//!   would elide a mandatory trap (the RUE-57 class).
//!
//! * **Identity rewiring**: operations that provably return one operand
//!   unchanged and cannot trap — `x+0`, `0+x`, `x-0`, `x*1`, `1*x`, their
//!   wrapping-arithmetic counterparts, `x/1`, `x|0`, `x^0`, `x<<0`, `x>>0`,
//!   `x&x`, `x|x`, `!!x` — have every use re-pointed at the operand through one
//!   batched
//!   [`Cfg::rewrite_value_uses_in_place`] sweep. The identity instruction itself is
//!   rewritten to a dead placeholder constant: DCE conservatively keeps any
//!   arithmetic that might trap, so simply orphaning an `Add` would leave it
//!   in the emitted code — but these shapes were selected precisely because
//!   they cannot trap, so replacing them with a side-effect-free placeholder
//!   is exact and lets DCE drop them.
//!
//! Annihilator shapes whose result is a *constant* (`x*0`, `x&0`, `x-x`,
//! `x^x`, `x%1`) live in the [`super::constfold`] kernel instead: their
//! results can cascade into further folding, so they belong inside the
//! sparse worklist where operand-became-constant events re-attempt users.
//! Identity rewires can never create a constant (a constant operand would
//! already have folded the whole instruction), so they run once, after the
//! worklist drains.
//!
//! Constant scans here read only pristine, pre-rewrite instruction data:
//! alias targets are collected first, placeholders are written after, so a
//! placeholder constant can never be mistaken for a real operand of a later
//! shape.

use crate::{BlockId, Cfg, CfgInst, CfgInstData, CfgValue};
use rue_air::TypeKind;

/// Work counters for one run (RUE-794 convention): one scan over the blocks
/// for strength reduction, one scan over the values for identities, one
/// batched use-rewrite.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Unsigned `Div`/`Mod` by a power of two rewritten to `Shr`/`BitAnd`.
    pub divmods_reduced: u64,
    /// Identity operations whose uses were re-pointed at their operand.
    pub identities_rewired: u64,
}

/// Run peephole simplification. Call after the sparse constant driver so
/// propagated constants are visible, and before DCE so placeholders and
/// newly dead operands are swept.
pub fn run(cfg: &mut Cfg) -> Result<Stats, crate::CfgEditError> {
    let mut stats = Stats::default();
    reduce_unsigned_div_mod(cfg, &mut stats);
    rewire_identities(cfg, &mut stats)?;
    Ok(stats)
}

/// Is this one of the unsigned integer types? (Deliberately not
/// `!is_signed`: bool, unit, and aggregate-typed values must not qualify.)
fn is_unsigned(kind: TypeKind) -> bool {
    matches!(
        kind,
        TypeKind::U8 | TypeKind::U16 | TypeKind::U32 | TypeKind::U64
    )
}

fn const_int(cfg: &Cfg, value: CfgValue) -> Option<u64> {
    match cfg.get_inst(value).data {
        CfgInstData::Const(v) => Some(v),
        _ => None,
    }
}

/// Rewrite unsigned `Div`/`Mod` by a constant power of two (>= 2; division
/// by 1 is an identity handled by [`rewire_identities`]) into `Shr`/`BitAnd`.
///
/// The shift-amount / mask constant is a new instruction inserted into the
/// block immediately before the rewritten operation, so evaluation order and
/// dominance are preserved.
fn reduce_unsigned_div_mod(cfg: &mut Cfg, stats: &mut Stats) {
    for block_idx in 0..cfg.block_count() {
        let block_id = BlockId::from_raw(block_idx as u32);
        // (position, new constant to insert before it)
        let mut insertions: Vec<(usize, CfgValue)> = Vec::new();

        for i in 0..cfg.get_block(block_id).insts.len() {
            let value = cfg.get_block(block_id).insts[i];
            let inst = cfg.get_inst(value);
            if !is_unsigned(inst.ty.kind()) {
                continue;
            }
            let (numerator, divisor, is_div) = match inst.data {
                CfgInstData::Div(a, b) => (a, b, true),
                CfgInstData::Mod(a, b) => (a, b, false),
                _ => continue,
            };
            let ty = inst.ty;
            let span = inst.span;
            let Some(d) = const_int(cfg, divisor) else {
                continue;
            };
            if d < 2 || !d.is_power_of_two() {
                continue;
            }

            let new_data = if is_div {
                CfgInstData::Const(d.trailing_zeros() as u64)
            } else {
                CfgInstData::Const(d - 1)
            };
            let amount = cfg.add_inst(CfgInst {
                data: new_data,
                ty,
                span,
            });
            cfg.get_inst_mut(value).data = if is_div {
                CfgInstData::Shr(numerator, amount)
            } else {
                CfgInstData::BitAnd(numerator, amount)
            };
            insertions.push((i, amount));
            stats.divmods_reduced += 1;
        }

        // Insert back-to-front so earlier indices stay valid.
        for (i, amount) in insertions.into_iter().rev() {
            cfg.get_block_mut(block_id).insts.insert(i, amount);
        }
    }
}

/// If `value` is a can't-trap operation that returns one operand unchanged,
/// return that operand.
fn identity_target(cfg: &Cfg, value: CfgValue) -> Option<CfgValue> {
    let is0 = |v: CfgValue| const_int(cfg, v) == Some(0);
    let is1 = |v: CfgValue| const_int(cfg, v) == Some(1);
    match cfg.get_inst(value).data {
        // x + 0 and x - 0 cannot overflow; x * 1 and x / 1 cannot overflow
        // or divide by zero.
        CfgInstData::Add(a, b) => {
            if is0(b) {
                Some(a)
            } else if is0(a) {
                Some(b)
            } else {
                None
            }
        }
        CfgInstData::Sub(a, b) if is0(b) => Some(a),
        CfgInstData::Mul(a, b) => {
            if is1(b) {
                Some(a)
            } else if is1(a) {
                Some(b)
            } else {
                None
            }
        }
        CfgInstData::WrappingAdd(a, b) => {
            if is0(b) {
                Some(a)
            } else if is0(a) {
                Some(b)
            } else {
                None
            }
        }
        CfgInstData::WrappingSub(a, b) if is0(b) => Some(a),
        CfgInstData::WrappingMul(a, b) => {
            if is1(b) {
                Some(a)
            } else if is1(a) {
                Some(b)
            } else {
                None
            }
        }
        CfgInstData::Div(a, b) if is1(b) => Some(a),
        // Bitwise ops and shifts never trap. (x & 0, x ^ x fold to constants
        // in the constfold kernel instead.)
        CfgInstData::BitOr(a, b) | CfgInstData::BitXor(a, b) if is0(b) => Some(a),
        CfgInstData::BitOr(a, b) | CfgInstData::BitXor(a, b) if is0(a) => Some(b),
        CfgInstData::BitAnd(a, b) | CfgInstData::BitOr(a, b) if a == b => Some(a),
        CfgInstData::Shl(a, b) | CfgInstData::Shr(a, b) if is0(b) => Some(a),
        // Boolean double negation.
        CfgInstData::Not(a) => match cfg.get_inst(a).data {
            CfgInstData::Not(inner) => Some(inner),
            _ => None,
        },
        _ => None,
    }
}

/// Re-point every use of an identity operation at its operand, then replace
/// the identity instruction with a dead placeholder constant for DCE.
fn rewire_identities(cfg: &mut Cfg, stats: &mut Stats) -> Result<(), crate::CfgEditError> {
    let value_count = cfg.value_count();

    // Collect aliases against pristine data before mutating anything.
    let mut alias: Vec<Option<CfgValue>> = vec![None; value_count];
    let mut any = false;
    for i in 0..value_count {
        let value = CfgValue::from_raw(i as u32);
        if let Some(target) = identity_target(cfg, value) {
            alias[i] = Some(target);
            any = true;
        }
    }
    if !any {
        return Ok(());
    }

    // Dummy the identity instructions out. Their shapes were selected to be
    // trap-free, so a side-effect-free placeholder is exact; with all uses
    // re-pointed below, DCE removes it.
    for (i, target) in alias.iter().enumerate() {
        if target.is_some() {
            cfg.get_inst_mut(CfgValue::from_raw(i as u32)).data = CfgInstData::Const(0);
            stats.identities_rewired += 1;
        }
    }

    // Resolve alias-of-alias chains (x+0 feeding y = (x+0)|0), then rewrite
    // every use in one sweep.
    let resolve = |mut v: CfgValue| -> CfgValue {
        while let Some(next) = alias[v.as_u32() as usize] {
            v = next;
        }
        v
    };
    // optimize_with_budget owns this private editor and rejects/discards it
    // on an edit error, so a poisoned in-place sweep is safe here and avoids a
    // second whole-CFG copy.
    cfg.rewrite_value_uses_in_place(resolve)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Terminator, Type};
    use rue_span::Span;

    fn make_cfg() -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg
    }

    fn push(cfg: &mut Cfg, data: CfgInstData, ty: Type) -> CfgValue {
        let entry = cfg.entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data,
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    #[test]
    fn test_add_zero_rewired_to_operand() {
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let zero = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        let add = push(&mut cfg, CfgInstData::Add(x, zero), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(add) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.identities_rewired, 1);
        // The return now reads x directly; the add is a dead placeholder.
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == x
        ));
        assert!(matches!(cfg.get_inst(add).data, CfgInstData::Const(0)));
    }

    #[test]
    fn test_wrapping_identity_chain_rewired_to_operand() {
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let zero = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        let one = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        let add = push(&mut cfg, CfgInstData::WrappingAdd(zero, x), Type::I32);
        let sub = push(&mut cfg, CfgInstData::WrappingSub(add, zero), Type::I32);
        let mul = push(&mut cfg, CfgInstData::WrappingMul(one, sub), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(mul) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.identities_rewired, 3);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == x
        ));
        assert!(matches!(cfg.get_inst(add).data, CfgInstData::Const(0)));
        assert!(matches!(cfg.get_inst(sub).data, CfgInstData::Const(0)));
        assert!(matches!(cfg.get_inst(mul).data, CfgInstData::Const(0)));
    }

    #[test]
    fn test_identity_chain_resolves_transitively() {
        // ((x + 0) | 0) << 0 — every layer is an identity; the final use
        // must land on x itself.
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let zero = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        let add = push(&mut cfg, CfgInstData::Add(x, zero), Type::I32);
        let or = push(&mut cfg, CfgInstData::BitOr(add, zero), Type::I32);
        let shl = push(&mut cfg, CfgInstData::Shl(or, zero), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(shl) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.identities_rewired, 3);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == x
        ));
    }

    #[test]
    fn test_double_not_rewired() {
        let mut cfg = make_cfg();
        let b = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::BOOL);
        let not1 = push(&mut cfg, CfgInstData::Not(b), Type::BOOL);
        let not2 = push(&mut cfg, CfgInstData::Not(not1), Type::BOOL);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(not2) });

        run(&mut cfg).unwrap();
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == b
        ));
        // The inner Not survives untouched (it may have other uses); only
        // the outer double-negation was rewired.
        assert!(matches!(cfg.get_inst(not1).data, CfgInstData::Not(_)));
    }

    #[test]
    fn test_same_operand_and_or_rewired() {
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::U32);
        let and = push(&mut cfg, CfgInstData::BitAnd(x, x), Type::U32);
        let or = push(&mut cfg, CfgInstData::BitOr(and, and), Type::U32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(or) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.identities_rewired, 2);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == x
        ));
    }

    #[test]
    fn test_non_identity_untouched() {
        // x + 1 and x wrapping_mul 2 must not be rewired.
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let one = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        let two = push(&mut cfg, CfgInstData::Const(2), Type::I32);
        let add = push(&mut cfg, CfgInstData::Add(x, one), Type::I32);
        let mul = push(&mut cfg, CfgInstData::WrappingMul(add, two), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(mul) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.identities_rewired, 0);
        assert!(matches!(cfg.get_inst(add).data, CfgInstData::Add(..)));
        assert!(matches!(
            cfg.get_inst(mul).data,
            CfgInstData::WrappingMul(..)
        ));
    }

    #[test]
    fn test_unsigned_div_by_pow2_becomes_shr() {
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::U32);
        let eight = push(&mut cfg, CfgInstData::Const(8), Type::U32);
        let div = push(&mut cfg, CfgInstData::Div(x, eight), Type::U32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(div) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.divmods_reduced, 1);
        let CfgInstData::Shr(num, amount) = cfg.get_inst(div).data else {
            panic!("expected Shr, got {:?}", cfg.get_inst(div).data);
        };
        assert_eq!(num, x);
        assert!(matches!(cfg.get_inst(amount).data, CfgInstData::Const(3)));
        // The shift amount is attached immediately before the rewritten op.
        let insts = &cfg.get_block(cfg.entry).insts;
        let div_pos = insts.iter().position(|&v| v == div).unwrap();
        assert_eq!(insts[div_pos - 1], amount);
    }

    #[test]
    fn test_unsigned_mod_by_pow2_becomes_mask() {
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::U64);
        let sixteen = push(&mut cfg, CfgInstData::Const(16), Type::U64);
        let modulo = push(&mut cfg, CfgInstData::Mod(x, sixteen), Type::U64);
        cfg.set_terminator(
            cfg.entry,
            Terminator::Return {
                value: Some(modulo),
            },
        );

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.divmods_reduced, 1);
        let CfgInstData::BitAnd(num, mask) = cfg.get_inst(modulo).data else {
            panic!("expected BitAnd, got {:?}", cfg.get_inst(modulo).data);
        };
        assert_eq!(num, x);
        assert!(matches!(cfg.get_inst(mask).data, CfgInstData::Const(15)));
    }

    #[test]
    fn test_signed_div_by_pow2_not_reduced() {
        // Signed division rounds toward zero; an arithmetic shift rounds
        // toward negative infinity. The rewrite must not happen.
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let two = push(&mut cfg, CfgInstData::Const(2), Type::I32);
        let div = push(&mut cfg, CfgInstData::Div(x, two), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(div) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.divmods_reduced, 0);
        assert!(matches!(cfg.get_inst(div).data, CfgInstData::Div(..)));
    }

    #[test]
    fn test_non_pow2_divisor_not_reduced() {
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::U32);
        let six = push(&mut cfg, CfgInstData::Const(6), Type::U32);
        let div = push(&mut cfg, CfgInstData::Div(x, six), Type::U32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(div) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.divmods_reduced, 0);
        assert!(matches!(cfg.get_inst(div).data, CfgInstData::Div(..)));
    }

    #[test]
    fn test_mul_by_pow2_not_reduced_to_shift() {
        // Multiplication traps on overflow; a shift does not. The strength
        // reduction that elides that trap is forbidden (RUE-57 class).
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::U32);
        let eight = push(&mut cfg, CfgInstData::Const(8), Type::U32);
        let mul = push(&mut cfg, CfgInstData::Mul(x, eight), Type::U32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(mul) });

        run(&mut cfg).unwrap();
        assert!(matches!(cfg.get_inst(mul).data, CfgInstData::Mul(..)));
    }
}
