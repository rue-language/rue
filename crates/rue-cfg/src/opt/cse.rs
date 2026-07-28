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
//! arithmetic (including wrapping arithmetic)/comparison/bitwise/shift ops, the
//! unary `Neg`/`Not`/`BitNot`, and reads of a never-written parameter (see
//! below). These read only SSA values, never memory, so there are no memory
//! barriers to track. Deliberately NOT keyed:
//!
//! * `Load`/`PlaceRead` — read memory, which needs versioning to dedupe safely
//!   (a store between two loads can change the result). Out of scope here.
//! * `Call`/`Intrinsic` — side effects; two calls are not interchangeable.
//! * everything else (allocs, stores, struct/array/enum construction, casts,
//!   drops, storage markers) — either side-effecting or not a pure value.
//!
//! ### Never-written parameter reads (RUE-914)
//!
//! Each `Param { index }` re-materializes the parameter's ABI-slot value, so two
//! reads of the same parameter produce separate SSA ids. That defeats CSE of
//! expressions over parameters: in `let s = a + b; let t = a + b;` the two `a`
//! reads and the two `b` reads carry different ids, so the two adds never key
//! equal. Keying `Param { index }` collapses repeated reads of the same
//! parameter to their first occurrence, which in turn lets the adds dedupe.
//!
//! This is sound ONLY for a parameter that is never written, so its every read
//! yields the identical value. A `Param { index }`'s `index` is the parameter's
//! ABI slot (the same numbering as [`Cfg::is_param_writable`] and a
//! `ParamStore`'s `param_slot`; both flow from sema's `abi_slot`). A slot is
//! treated as never-written when it is (a) not logically writable
//! (`is_param_writable(slot) == false`, i.e. `borrow` or plain by-value —
//! never `inout`, and never a `mut self` receiver slot) and (b) receives no
//! `ParamStore` anywhere among the block-attached instructions. Plain
//! by-value (`Normal`) parameters are immutable in the callee (assignment is
//! rejected, E0203) and `borrow` parameters cannot be written, so only
//! writable slots — excluded by (a) — and any slot a `ParamStore` targets —
//! excluded by (b) — are left out. (As defense in depth, a projected
//! `PlaceWrite` whose base is a parameter slot also excludes it, though such a
//! write only ever targets an already-excluded `inout` slot.)
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
//! ops only — `Add`, `Mul`, `WrappingAdd`, `WrappingMul`, `Eq`, `Ne`, `BitAnd`,
//! `BitOr`, `BitXor` — the two operand ids are sorted so `Add(a,b)` and
//! `Add(b,a)` share a key. `Sub`/`WrappingSub`/`Div`/`Mod`/`Shl`/`Shr` and the
//! ordered comparisons are order-sensitive and keyed as written.
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
    /// A read of a never-written parameter, keyed by its ABI slot and type
    /// (RUE-914). Only inserted for slots proven never-written; every such read
    /// yields the identical value.
    Param(u32, Type),
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
fn key_of(
    cfg: &Cfg,
    value: CfgValue,
    never_written_param: &[bool],
    r: impl Fn(CfgValue) -> CfgValue,
) -> Option<VnKey> {
    let inst = cfg.get_inst(value);
    let ty = inst.ty;
    Some(match inst.data {
        CfgInstData::Const(v) => VnKey::Const(v, ty),
        CfgInstData::BoolConst(b) => VnKey::BoolConst(b, ty),
        CfgInstData::StringConst(s) => VnKey::StringConst(s, ty),

        // A read of a never-written parameter is a pure value: key it only when
        // the slot is proven never-written (RUE-914).
        CfgInstData::Param { index }
            if never_written_param
                .get(index as usize)
                .copied()
                .unwrap_or(false) =>
        {
            VnKey::Param(index, ty)
        }

        // Commutative binary ops: operands sorted.
        CfgInstData::Add(a, b) => commutative(0, r(a), r(b), ty),
        CfgInstData::Mul(a, b) => commutative(1, r(a), r(b), ty),
        CfgInstData::Eq(a, b) => commutative(2, r(a), r(b), ty),
        CfgInstData::Ne(a, b) => commutative(3, r(a), r(b), ty),
        CfgInstData::BitAnd(a, b) => commutative(4, r(a), r(b), ty),
        CfgInstData::BitOr(a, b) => commutative(5, r(a), r(b), ty),
        CfgInstData::BitXor(a, b) => commutative(6, r(a), r(b), ty),
        CfgInstData::WrappingAdd(a, b) => commutative(19, r(a), r(b), ty),
        CfgInstData::WrappingMul(a, b) => commutative(20, r(a), r(b), ty),

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
        CfgInstData::WrappingSub(a, b) => VnKey::Binary(21, r(a), r(b), ty),

        // Unary ops.
        CfgInstData::Neg(a) => VnKey::Unary(16, r(a), ty),
        CfgInstData::Not(a) => VnKey::Unary(17, r(a), ty),
        CfgInstData::BitNot(a) => VnKey::Unary(18, r(a), ty),

        _ => return None,
    })
}

/// Compute, per parameter ABI slot, whether the parameter is never written and
/// so its reads are all the identical pure value (RUE-914). A slot is
/// never-written when it is not logically writable by-ref (`inout`) and receives
/// no `ParamStore` (nor, defensively, a projected `PlaceWrite`) among the
/// block-attached instructions. Indexed by ABI slot; `Param { index }`'s `index`
/// is exactly that slot.
fn never_written_params(cfg: &Cfg) -> Vec<bool> {
    let num_params = cfg.num_params() as usize;
    let mut never_written = vec![false; num_params];
    for (slot, flag) in never_written.iter_mut().enumerate() {
        // A parameter whose ADDRESS escapes through @raw/@raw_mut/@field_ptr
        // is mutable through the raw pointer (@ptr_write), so its reads are
        // NOT interchangeable even though nothing writes it directly — keying
        // such reads merged a post-write read into the stale pre-write value
        // (found by the 2026-07-16 optimizer hunt).
        *flag = !cfg.is_param_writable(slot as u32) && !cfg.is_param_address_taken(slot as u32);
    }

    for block in cfg.blocks() {
        for &value in &block.insts {
            match &cfg.get_inst(value).data {
                CfgInstData::ParamStore { param_slot, .. } => {
                    if let Some(flag) = never_written.get_mut(*param_slot as usize) {
                        *flag = false;
                    }
                }
                CfgInstData::PlaceWrite { place, .. } => {
                    if let crate::PlaceBase::Param(slot) = place.base {
                        if let Some(flag) = never_written.get_mut(slot as usize) {
                            *flag = false;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    never_written
}

/// Run block-local CSE. Call at `-O2`/`-O3` after simplification (more
/// duplicates are exposed once blocks are merged) and before DCE (which sweeps
/// the dead placeholders).
pub fn run(cfg: &mut Cfg) -> Result<Stats, crate::CfgEditError> {
    let mut stats = Stats::default();
    // `subst[dup] = first` for every replaced duplicate. Persists across blocks
    // (each entry points earlier within its own block, so global resolution
    // stays correct), while the value-number table is per-block.
    let mut subst: Vec<Option<CfgValue>> = vec![None; cfg.value_count()];

    let never_written_param = never_written_params(cfg);

    for block_idx in 0..cfg.block_count() {
        let block_id = BlockId::from_raw(block_idx as u32);
        let mut table: HashMap<VnKey, CfgValue> = HashMap::new();

        for i in 0..cfg.get_block(block_id).insts.len() {
            let value = cfg.get_block(block_id).insts[i];
            stats.insts_scanned += 1;

            let Some(key) = key_of(cfg, value, &never_written_param, |v| resolve(&subst, v)) else {
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
        cfg.rewrite_value_uses(|v| resolved[v.as_u32() as usize])?;
    }

    Ok(stats)
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

        let stats = run(&mut cfg).unwrap();
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

        let stats = run(&mut cfg).unwrap();
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

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.duplicates_replaced, 1);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == add1
        ));
    }

    #[test]
    fn test_wrapping_arithmetic_dedupes_with_correct_commutativity() {
        let mut cfg = make_cfg();
        let a = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let b = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let add1 = push(&mut cfg, CfgInstData::WrappingAdd(a, b), Type::I32);
        let add2 = push(&mut cfg, CfgInstData::WrappingAdd(b, a), Type::I32);
        let mul1 = push(&mut cfg, CfgInstData::WrappingMul(a, b), Type::I32);
        let mul2 = push(&mut cfg, CfgInstData::WrappingMul(b, a), Type::I32);
        let sub1 = push(&mut cfg, CfgInstData::WrappingSub(a, b), Type::I32);
        let sub2 = push(&mut cfg, CfgInstData::WrappingSub(a, b), Type::I32);
        let reversed_sub = push(&mut cfg, CfgInstData::WrappingSub(b, a), Type::I32);
        let result = push(&mut cfg, CfgInstData::Add(add2, sub2), Type::I32);
        cfg.set_terminator(
            cfg.entry,
            Terminator::Return {
                value: Some(result),
            },
        );

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.duplicates_replaced, 3);
        assert!(matches!(cfg.get_inst(add2).data, CfgInstData::Const(0)));
        assert!(matches!(cfg.get_inst(mul2).data, CfgInstData::Const(0)));
        assert!(matches!(cfg.get_inst(sub2).data, CfgInstData::Const(0)));
        assert!(matches!(
            cfg.get_inst(result).data,
            CfgInstData::Add(x, y) if x == add1 && y == sub1
        ));
        assert!(matches!(
            cfg.get_inst(mul1).data,
            CfgInstData::WrappingMul(..)
        ));
        assert!(matches!(
            cfg.get_inst(reversed_sub).data,
            CfgInstData::WrappingSub(x, y) if x == b && y == a
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

        let stats = run(&mut cfg).unwrap();
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

        let stats = run(&mut cfg).unwrap();
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

        let stats = run(&mut cfg).unwrap();
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
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        let add2 = push_in(&mut cfg, block2, CfgInstData::Add(a, b), Type::I32);
        cfg.set_terminator(block2, Terminator::Return { value: Some(add2) });

        let stats = run(&mut cfg).unwrap();
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

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.insts_scanned, total_insts as u64);
        assert_eq!(stats.duplicates_replaced, 1);

        // Idempotent: nothing left to eliminate.
        let again = run(&mut cfg).unwrap();
        assert_eq!(again.duplicates_replaced, 0);
    }

    #[test]
    fn test_never_written_param_reads_dedupe() {
        // Two separate reads of the same never-written parameter carry different
        // SSA ids; keying `Param { index }` collapses the second to the first.
        let mut cfg = Cfg::new(Type::I32, 0, 2, "f".to_string(), vec![false, false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let a1 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let a2 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(a1, a2), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(sum) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.duplicates_replaced, 1);
        // The add now reads the first parameter value twice.
        assert!(matches!(cfg.get_inst(sum).data, CfgInstData::Add(x, y) if x == a1 && y == a1));
        assert!(matches!(cfg.get_inst(a2).data, CfgInstData::Const(0)));
    }

    #[test]
    fn test_writable_inout_param_reads_not_deduped() {
        // An `inout` (writable) parameter may change between reads, so its reads
        // must NOT be numbered together.
        let mut cfg = Cfg::new(
            Type::I32,
            0,
            1,
            "f".to_string(),
            rue_air::ParamSlotModes::new(vec![true], vec![true]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let a1 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let a2 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(a1, a2), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(sum) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.duplicates_replaced, 0);
        assert!(matches!(
            cfg.get_inst(a1).data,
            CfgInstData::Param { index: 0 }
        ));
        assert!(matches!(
            cfg.get_inst(a2).data,
            CfgInstData::Param { index: 0 }
        ));
    }

    #[test]
    fn test_paramstore_written_param_reads_not_deduped() {
        // A by-value parameter targeted by a `ParamStore` (however that arises)
        // is written, so its reads must not be numbered together either.
        let mut cfg = Cfg::new(Type::I32, 0, 1, "f".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let a1 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let c = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::ParamStore {
                param_slot: 0,
                value: c,
            },
            Type::UNIT,
        );
        let a2 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(a1, a2), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(sum) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.duplicates_replaced, 0);
    }

    #[test]
    fn test_forward_then_cse_dedupes_param_expression() {
        // The RUE-914 acceptance shape:
        //   fn f(a: i32, b: i32) -> i32 { let s = a + b; let t = a + b; s + t }
        // CfgBuilder materializes each parameter use as a fresh `Param`, spills
        // `s`/`t` to slots, and reads them back. Running the release pipeline
        // stages in order — constopt, forward, cse — must leave the two adds
        // computed once: forwarding turns the `Load`s of `s`/`t` into the two
        // add values, param keying collapses the duplicated `a`/`b` reads, and
        // CSE then sees the second add as a duplicate of the first.
        let mut cfg = Cfg::new(Type::I32, 2, 2, "f".to_string(), vec![false, false]);
        let entry = cfg.new_block();
        cfg.entry = entry;

        // let s = a + b;  (slot 0)
        let a1 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let b1 = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let add1 = push(&mut cfg, CfgInstData::Add(a1, b1), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: add1,
            },
            Type::UNIT,
        );
        // let t = a + b;  (slot 1) — fresh param reads per source use.
        let a2 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let b2 = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let add2 = push(&mut cfg, CfgInstData::Add(a2, b2), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 1,
                init: add2,
            },
            Type::UNIT,
        );
        // s + t
        let load_s = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let load_t = push(&mut cfg, CfgInstData::Load { slot: 1 }, Type::I32);
        let result = push(&mut cfg, CfgInstData::Add(load_s, load_t), Type::I32);
        cfg.set_terminator(
            cfg.entry,
            Terminator::Return {
                value: Some(result),
            },
        );

        super::super::constopt::run(&mut cfg);
        let fwd = super::super::forward::run(&mut cfg).unwrap();
        // Both `Load`s forwarded to their slot's single write (the adds).
        assert_eq!(fwd.loads_forwarded_single_write, 2);

        let cse = run(&mut cfg).unwrap();
        // a2, b2, and the second add are all duplicates.
        assert_eq!(cse.duplicates_replaced, 3);
        // The result now adds the single surviving add to itself.
        assert!(
            matches!(cfg.get_inst(result).data, CfgInstData::Add(x, y) if x == add1 && y == add1),
            "result should be add1 + add1, got {:?}",
            cfg.get_inst(result).data
        );
        assert!(matches!(cfg.get_inst(add2).data, CfgInstData::Const(0)));
    }
    #[test]
    fn test_address_taken_param_not_keyed() {
        // A parameter whose address escapes via @raw_mut can be mutated
        // through @ptr_write, so its repeated reads must NOT dedupe
        // (2026-07-16 optimizer-hunt miscompile: post-write read merged into
        // the stale pre-write value).
        let mut cfg = Cfg::new(Type::I32, 0, 1, "test".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.mark_param_address_taken(0);
        let p1 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let p2 = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(p1, p2), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(sum) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(
            stats.duplicates_replaced, 0,
            "address-taken param reads must not dedupe"
        );
        assert!(matches!(cfg.get_inst(p2).data, CfgInstData::Param { .. }));
    }
}
