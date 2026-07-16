//! Block-local value numbering / common-subexpression elimination (RUE-913).
//!
//! Within a single basic block, two pure instructions that compute the same
//! value from the same operands are redundant: the second can be replaced by
//! the first. This pass runs only at `-O2`/`-O3` (ADR-0044 places CSE at the
//! release-default level), after [`super::simplify`] — merged straight-line
//! blocks expose more intra-block duplicates — and before [`super::dce`], which
//! sweeps the placeholders this pass leaves behind.
//!
//! ## Algorithm
//!
//! One forward walk of each block's instructions, with a value-number table
//! (`key -> first value that computed it`) reset per block. The walk is strictly
//! block-local: it never numbers a value against one from another block, so no
//! dominance analysis is needed — the first occurrence always precedes the
//! duplicate in the same block and therefore dominates it.
//!
//! ### What is keyed
//!
//! Only pure-by-value instructions whose result is a deterministic function of
//! their SSA operands: `Const`, `BoolConst`, `StringConst`, the binary
//! arithmetic/comparison/bitwise/shift ops, and the unary `Neg`/`Not`/`BitNot`.
//! These read only SSA values, never memory, so there are no memory barriers to
//! track. Deliberately NOT keyed:
//!
//! * `Load`/`PlaceRead` — read memory, which needs versioning to dedupe safely
//!   (a store between two loads can change the result). Out of scope here.
//! * `Call`/`Intrinsic` — side effects; two calls are not interchangeable.
//! * everything else (allocs, stores, struct/array/enum construction, casts,
//!   drops, storage markers) — either side-effecting or not a pure value.
//!
//! Constants MUST be keyed: two separately materialized `Const(1)` instructions
//! would otherwise give `x + 1` and `x + 1` different operand ids, defeating the
//! deduplication of the adds above them.
//!
//! ### The key
//!
//! `(opcode, resolved operand ids, result type)`. Operands are resolved through
//! the substitution map built so far, so chains dedupe: in
//! `c = a+b; d = a+b; e = c+1; f = d+1`, numbering `d` records `d -> c`, which
//! makes `f`'s resolved key equal `e`'s. The result type is part of the key so
//! same-shape ops that produce different types never merge. For the commutative
//! ops only — `Add`, `Mul`, `Eq`, `Ne`, `BitAnd`, `BitOr`, `BitXor` — the two
//! operand ids are sorted so `Add(a,b)` and `Add(b,a)` share a key.
//! `Sub`/`Div`/`Mod`/`Shl`/`Shr` and the ordered comparisons are order-sensitive
//! and keyed as written.
//!
//! ### Replacing a duplicate
//!
//! On a repeat, `subst[dup] = first` is recorded and the duplicate's instruction
//! data is overwritten with `Const(0)` — a dead, side-effect-free placeholder.
//! DCE deliberately preserves possibly-trapping arithmetic (RUE-57), so simply
//! orphaning a duplicate `Add`/`Div`/… would leave it in the emitted code.
//! Replacing the SECOND occurrence is trap-exact: the first occurrence has
//! identical operands and executes earlier in the same block, so it dominates
//! the duplicate and traps if and only if the duplicate would have — the
//! duplicate's trap is fully redundant. The FIRST occurrence is never touched.
//!
//! After every block, if anything was substituted, all uses are re-pointed at
//! the surviving first values in ONE [`Cfg::rewrite_value_uses`] sweep (the same
//! batched work discipline as [`super::peephole`] and [`super::simplify`],
//! RUE-794). DCE then removes the now-unused `Const(0)` placeholders.

use std::collections::HashMap;

use crate::{BlockId, Cfg, CfgInstData, CfgValue, Type};

/// Work counters for one run (RUE-794 convention): a single forward scan of
/// every block, then one batched use-rewrite. There is no fixpoint loop.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Total block-attached instructions visited by the forward walk.
    pub insts_scanned: u64,
    /// Duplicate pure instructions replaced by their first occurrence.
    pub duplicates_replaced: u64,
}

/// Value-number key for a pure-by-value instruction. Constants carry their
/// literal; other ops carry their resolved operand ids. The result [`Type`] is
/// part of every variant so differently typed results never share a number.
#[derive(PartialEq, Eq, Hash)]
enum VnKey {
    Const(u64, Type),
    BoolConst(bool, Type),
    StringConst(u32, Type),
    /// `(opcode tag, operand, type)`.
    Unary(u8, CfgValue, Type),
    /// `(opcode tag, lhs, rhs, type)`. For commutative ops the operands are
    /// pre-sorted by the caller so `op(a,b)` and `op(b,a)` collide.
    Binary(u8, CfgValue, CfgValue, Type),
}

/// Walk `subst` chains to the surviving value. Chains are at most one hop today
/// (a first occurrence never gets a substitution), but the loop is robust to
/// any depth.
fn resolve(subst: &[Option<CfgValue>], mut v: CfgValue) -> CfgValue {
    while let Some(next) = subst[v.as_u32() as usize] {
        v = next;
    }
    v
}

/// Sort a commutative op's operands so the key is order-independent.
fn commutative(tag: u8, a: CfgValue, b: CfgValue, ty: Type) -> VnKey {
    let (lo, hi) = if a.as_u32() <= b.as_u32() {
        (a, b)
    } else {
        (b, a)
    };
    VnKey::Binary(tag, lo, hi, ty)
}

/// Compute the value-number key for `value`, resolving operands through the
/// substitution map. Returns `None` for instructions this pass does not number
/// (memory reads, calls, and everything with side effects or non-value results).
fn key_of(cfg: &Cfg, value: CfgValue, r: impl Fn(CfgValue) -> CfgValue) -> Option<VnKey> {
    let inst = cfg.get_inst(value);
    let ty = inst.ty;
    Some(match inst.data {
        CfgInstData::Const(v) => VnKey::Const(v, ty),
        CfgInstData::BoolConst(b) => VnKey::BoolConst(b, ty),
        CfgInstData::StringConst(s) => VnKey::StringConst(s, ty),

        // Commutative binary ops: operands sorted.
        CfgInstData::Add(a, b) => commutative(0, r(a), r(b), ty),
        CfgInstData::Mul(a, b) => commutative(1, r(a), r(b), ty),
        CfgInstData::Eq(a, b) => commutative(2, r(a), r(b), ty),
        CfgInstData::Ne(a, b) => commutative(3, r(a), r(b), ty),
        CfgInstData::BitAnd(a, b) => commutative(4, r(a), r(b), ty),
        CfgInstData::BitOr(a, b) => commutative(5, r(a), r(b), ty),
        CfgInstData::BitXor(a, b) => commutative(6, r(a), r(b), ty),

        // Order-sensitive binary ops: operands kept as written.
        CfgInstData::Sub(a, b) => VnKey::Binary(7, r(a), r(b), ty),
        CfgInstData::Div(a, b) => VnKey::Binary(8, r(a), r(b), ty),
        CfgInstData::Mod(a, b) => VnKey::Binary(9, r(a), r(b), ty),
        CfgInstData::Lt(a, b) => VnKey::Binary(10, r(a), r(b), ty),
        CfgInstData::Gt(a, b) => VnKey::Binary(11, r(a), r(b), ty),
        CfgInstData::Le(a, b) => VnKey::Binary(12, r(a), r(b), ty),
        CfgInstData::Ge(a, b) => VnKey::Binary(13, r(a), r(b), ty),
        CfgInstData::Shl(a, b) => VnKey::Binary(14, r(a), r(b), ty),
        CfgInstData::Shr(a, b) => VnKey::Binary(15, r(a), r(b), ty),

        // Unary ops.
        CfgInstData::Neg(a) => VnKey::Unary(16, r(a), ty),
        CfgInstData::Not(a) => VnKey::Unary(17, r(a), ty),
        CfgInstData::BitNot(a) => VnKey::Unary(18, r(a), ty),

        _ => return None,
    })
}

/// Run block-local CSE. Call at `-O2`/`-O3` after simplification (more
/// duplicates are exposed once blocks are merged) and before DCE (which sweeps
/// the dead placeholders).
pub fn run(cfg: &mut Cfg) -> Stats {
    let mut stats = Stats::default();
    // `subst[dup] = first` for every replaced duplicate. Persists across blocks
    // (each entry points earlier within its own block, so global resolution
    // stays correct), while the value-number table is per-block.
    let mut subst: Vec<Option<CfgValue>> = vec![None; cfg.value_count()];

    for block_idx in 0..cfg.block_count() {
        let block_id = BlockId::from_raw(block_idx as u32);
        let mut table: HashMap<VnKey, CfgValue> = HashMap::new();

        for i in 0..cfg.get_block(block_id).insts.len() {
            let value = cfg.get_block(block_id).insts[i];
            stats.insts_scanned += 1;

            let Some(key) = key_of(cfg, value, |v| resolve(&subst, v)) else {
                continue;
            };

            match table.get(&key) {
                Some(&first) => {
                    // Redundant with an earlier, dominating computation. Record
                    // the substitution and neutralize the duplicate; its trap
                    // (if any) is subsumed by the first occurrence's.
                    subst[value.as_u32() as usize] = Some(first);
                    cfg.get_inst_mut(value).data = CfgInstData::Const(0);
                    stats.duplicates_replaced += 1;
                }
                None => {
                    table.insert(key, value);
                }
            }
        }
    }

    if stats.duplicates_replaced > 0 {
        // Resolve chains once, then re-point every use in a single sweep.
        let resolved: Vec<CfgValue> = (0..cfg.value_count())
            .map(|i| resolve(&subst, CfgValue::from_raw(i as u32)))
            .collect();
        cfg.rewrite_value_uses(|v| resolved[v.as_u32() as usize]);
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgInst, Terminator, Type};
    use rue_span::Span;

    fn make_cfg() -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg
    }

    fn push(cfg: &mut Cfg, data: CfgInstData, ty: Type) -> CfgValue {
        let entry = cfg.entry;
        push_in(cfg, entry, data, ty)
    }

    fn push_in(cfg: &mut Cfg, block: BlockId, data: CfgInstData, ty: Type) -> CfgValue {
        cfg.add_inst_to_block(
            block,
            CfgInst {
                data,
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    #[test]
    fn test_duplicate_add_replaced_second_only() {
        // add1 = x + y; add2 = x + y (dup). The second becomes a placeholder,
        // uses re-point at the first, and the first is untouched.
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let y = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let add1 = push(&mut cfg, CfgInstData::Add(x, y), Type::I32);
        let add2 = push(&mut cfg, CfgInstData::Add(x, y), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(add2) });

        let stats = run(&mut cfg);
        assert_eq!(stats.duplicates_replaced, 1);
        // The return now reads the first add directly.
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == add1
        ));
        // First occurrence untouched; duplicate is a dead placeholder.
        assert!(matches!(cfg.get_inst(add1).data, CfgInstData::Add(a, b) if a == x && b == y));
        assert!(matches!(cfg.get_inst(add2).data, CfgInstData::Const(0)));
    }

    #[test]
    fn test_trapping_div_deduped_first_kept() {
        // Div on non-constant operands can trap; CSE still dedupes the second,
        // and the first (which dominates and traps identically) stays in place.
        let mut cfg = make_cfg();
        let a = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let b = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let div1 = push(&mut cfg, CfgInstData::Div(a, b), Type::I32);
        let div2 = push(&mut cfg, CfgInstData::Div(a, b), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(div2) });

        let stats = run(&mut cfg);
        assert_eq!(stats.duplicates_replaced, 1);
        assert!(matches!(cfg.get_inst(div1).data, CfgInstData::Div(x, y) if x == a && y == b));
        assert!(matches!(cfg.get_inst(div2).data, CfgInstData::Const(0)));
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == div1
        ));
    }

    #[test]
    fn test_commutative_add_dedupes() {
        // Add(a, b) and Add(b, a) share a key.
        let mut cfg = make_cfg();
        let a = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let b = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let add1 = push(&mut cfg, CfgInstData::Add(a, b), Type::I32);
        let add2 = push(&mut cfg, CfgInstData::Add(b, a), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(add2) });

        let stats = run(&mut cfg);
        assert_eq!(stats.duplicates_replaced, 1);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == add1
        ));
    }

    #[test]
    fn test_noncommutative_sub_not_deduped() {
        // Sub(a, b) and Sub(b, a) are different values; neither is replaced.
        let mut cfg = make_cfg();
        let a = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let b = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let sub1 = push(&mut cfg, CfgInstData::Sub(a, b), Type::I32);
        let sub2 = push(&mut cfg, CfgInstData::Sub(b, a), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(sub2) });

        let stats = run(&mut cfg);
        assert_eq!(stats.duplicates_replaced, 0);
        assert!(matches!(cfg.get_inst(sub1).data, CfgInstData::Sub(..)));
        assert!(matches!(cfg.get_inst(sub2).data, CfgInstData::Sub(..)));
    }

    #[test]
    fn test_constants_dedupe_and_enable_add_chain() {
        // Two separately materialized Const(1) both feed x + 1. Numbering the
        // constants makes the two adds share a key, so both the redundant
        // constant AND the redundant add are eliminated (3 keyed insts, 2 dups).
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let one_a = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        let one_b = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        let add1 = push(&mut cfg, CfgInstData::Add(x, one_a), Type::I32);
        let add2 = push(&mut cfg, CfgInstData::Add(x, one_b), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(add2) });

        let stats = run(&mut cfg);
        assert_eq!(stats.duplicates_replaced, 2);
        // Surviving const and add are the first occurrences.
        assert!(matches!(cfg.get_inst(one_a).data, CfgInstData::Const(1)));
        assert!(matches!(cfg.get_inst(one_b).data, CfgInstData::Const(0)));
        assert!(matches!(cfg.get_inst(add1).data, CfgInstData::Add(a, o) if a == x && o == one_a));
        assert!(matches!(cfg.get_inst(add2).data, CfgInstData::Const(0)));
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == add1
        ));
    }

    #[test]
    fn test_loads_not_deduped() {
        // Memory reads are out of scope: two identical Loads both survive.
        let mut cfg = make_cfg();
        let load1 = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let load2 = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let add = push(&mut cfg, CfgInstData::Add(load1, load2), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(add) });

        let stats = run(&mut cfg);
        assert_eq!(stats.duplicates_replaced, 0);
        assert!(matches!(
            cfg.get_inst(load1).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_inst(load2).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_cross_block_not_deduped() {
        // The same expression in two blocks stays: numbering is block-local.
        let mut cfg = make_cfg();
        let a = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let b = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let add1 = push(&mut cfg, CfgInstData::Add(a, b), Type::I32);
        let block2 = cfg.new_block();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Goto {
                target: block2,
                args_start: 0,
                args_len: 0,
            },
        );
        let add2 = push_in(&mut cfg, block2, CfgInstData::Add(a, b), Type::I32);
        cfg.set_terminator(block2, Terminator::Return { value: Some(add2) });

        let stats = run(&mut cfg);
        assert_eq!(stats.duplicates_replaced, 0);
        assert!(matches!(cfg.get_inst(add1).data, CfgInstData::Add(..)));
        assert!(matches!(cfg.get_inst(add2).data, CfgInstData::Add(..)));
    }

    #[test]
    fn test_work_counters_one_scan() {
        // insts_scanned counts every block-attached instruction; a single run
        // suffices (no fixpoint) — a second run finds nothing new.
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let y = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let _add1 = push(&mut cfg, CfgInstData::Add(x, y), Type::I32);
        let add2 = push(&mut cfg, CfgInstData::Add(x, y), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(add2) });

        let total_insts: usize = (0..cfg.block_count())
            .map(|i| cfg.get_block(BlockId::from_raw(i as u32)).insts.len())
            .sum();

        let stats = run(&mut cfg);
        assert_eq!(stats.insts_scanned, total_insts as u64);
        assert_eq!(stats.duplicates_replaced, 1);

        // Idempotent: nothing left to eliminate.
        let again = run(&mut cfg);
        assert_eq!(again.duplicates_replaced, 0);
    }
}
