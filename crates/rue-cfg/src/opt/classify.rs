//! Shared trap / speculation classifier for optimizer passes.
//!
//! DCE, LICM (RUE-927), and unrolling (RUE-928) all need to reason about whether
//! a CFG instruction can *trap* — either abort with a Rue-defined runtime panic
//! or fault during an unsafe memory access. The latter is a conservative
//! classification for speculation and DCE safety, not a statement about Rue's
//! raw-pointer semantics. Historically that knowledge lived only inside DCE's
//! private `has_side_effects` predicate, which conflated two independent axes.
//! ADR-0054 §2 makes this module the single place that names the trapping set,
//! so the two axes cannot drift apart across the passes that consult them.
//!
//! ## Why a module in `opt` (not `CfgInstData` methods)
//!
//! The ADR offers two shapes: an `opt::classify` module or `CfgInstData`-level
//! `may_trap`/`is_speculatable` methods. This crate already keeps its
//! optimizer-facing analyses in `opt/` (`dce`, `forward`, `cse`, …) and its
//! `inst` module deliberately free of pass policy — what "traps" is an optimizer
//! concern, not an intrinsic property of the instruction enum. The classifier
//! also needs the `Cfg` (a `PlaceRead` may trap based on both its base and its
//! indexed projection list, which lives in `Cfg::projections`), so a free
//! function taking `(&Cfg, CfgValue)` matches DCE's existing call shape exactly
//! and lets LICM/unrolling reuse it verbatim.
//!
//! ## The two axes (defined independently)
//!
//! - [`may_trap`]: the instruction can panic or fault at runtime — overflow-checked
//!   arithmetic (`Add`/`Sub`/`Mul`/`Neg`), division checks (`Div`/`Mod`), the
//!   `IntCast` range check (`__rue_intcast_overflow`), and the bounds check of a
//!   `PlaceRead` through an indirect base or an `Index` projection. The indirect
//!   case is conservatively included for fault safety; this is the axis LICM and
//!   unrolling need on its own: hoisting a `may_trap` op into a zero-trip
//!   preheader would manufacture a trap or fault out of thin air.
//! - [`has_observable_side_effect`]: the instruction is visible beyond its
//!   result value — `Call`, `Intrinsic`, `Alloc`, `Store`, `ParamStore`,
//!   `PlaceWrite`, `Drop`, `StorageLive`/`StorageDead`. This is the axis DCE
//!   needs to keep an op live even when its result is unused.
//!
//! These axes are orthogonal: arithmetic is *pure but trapping* (`may_trap` yet
//! no observable effect), while `Store` is *effecting but non-trapping*. That
//! pure-but-trapping case is exactly what motivates the split — DCE must keep it,
//! LICM must refuse to hoist it, and neither can be expressed with a single
//! "has effects" bit.
//!
//! ## `is_speculatable`
//!
//! An op is *speculatable* when it can be evaluated freely on any path — it can
//! neither trap nor be observed. Stated once, as the single definition every
//! consumer relies on:
//!
//! ```text
//! is_speculatable(v)  ==  !may_trap(v) && !has_observable_side_effect(v)
//! ```
//!
//! Bitwise ops, shifts (counts are masked per spec, so they never trap),
//! comparisons, `Not`/`BitNot`, constants, direct non-indexed `PlaceRead`/`Load`,
//! and the aggregate constructors are the speculatable set — the ops LICM may
//! hoist.

use crate::{Cfg, CfgInstData, CfgValue, Projection};

/// Returns `true` if evaluating `value` can trap or fault at runtime.
///
/// The trapping set, named once here per ADR-0054 §2:
/// - overflow-checked arithmetic: `Add`, `Sub`, `Mul`, `Neg`;
/// - division checks: `Div`, `Mod` (divide-by-zero / `INT_MIN / -1`);
/// - `IntCast` (range check, panics via `__rue_intcast_overflow`);
/// - `PlaceRead` through an indirect base (a conservatively classified pointer
///   dereference) or an `Index` projection (array bounds check).
///
/// Bitwise ops and shifts do **not** trap (shift counts are masked per spec).
pub(crate) fn may_trap(cfg: &Cfg, value: CfgValue) -> bool {
    match &cfg.get_inst(value).data {
        // Overflow-checked arithmetic and division checks panic at runtime.
        CfgInstData::Add(..)
        | CfgInstData::Sub(..)
        | CfgInstData::Mul(..)
        | CfgInstData::Div(..)
        | CfgInstData::Mod(..)
        | CfgInstData::Neg(..) => true,

        // IntCast range-checks the narrowing conversion and panics when the
        // value is out of range for the target type.
        CfgInstData::IntCast { .. } => true,

        // A direct PlaceRead is pure for field projections, but an indirect
        // base dereferences a pointer and an array index performs a bounds
        // check. Either can trap or fault before the read produces its result.
        CfgInstData::PlaceRead { place } => {
            matches!(place.base, crate::PlaceBase::Indirect(_))
                || cfg
                    .get_place_projections(place)
                    .iter()
                    .any(|p| matches!(p, Projection::Index { .. }))
        }

        // Everything else is outside this conservative trapping set.
        _ => false,
    }
}

/// Returns `true` if `value` has an effect observable beyond its result value.
///
/// These are the ops DCE must keep live even when their result is unused:
/// `Call`, `Intrinsic`, `Alloc`, `Store`, `ParamStore`, `PlaceWrite`, `Drop`,
/// and `StorageLive`/`StorageDead`. This axis is independent of [`may_trap`]:
/// none of these are counted here for their trap potential (a `Call` may of
/// course trap, but it is kept live for its effect, and it is not part of the
/// `may_trap` speculation set because it is never a hoist candidate anyway).
pub(crate) fn has_observable_side_effect(cfg: &Cfg, value: CfgValue) -> bool {
    match &cfg.get_inst(value).data {
        // Function and intrinsic calls can have arbitrary effects.
        CfgInstData::Call { .. } | CfgInstData::Intrinsic { .. } => true,

        // Memory writes.
        CfgInstData::Alloc { .. }
        | CfgInstData::Store { .. }
        | CfgInstData::ParamStore { .. }
        | CfgInstData::PlaceWrite { .. } => true,

        // Drop runs destructors.
        CfgInstData::Drop { .. } => true,

        // Storage liveness affects stack allocation.
        CfgInstData::StorageLive { .. } | CfgInstData::StorageDead { .. } => true,

        _ => false,
    }
}

/// Returns `true` if `value` may be evaluated freely on any path.
///
/// A speculatable op can neither trap nor be observed, so a pass may move it,
/// duplicate it, or hoist it without changing observable behavior. This is the
/// single definition of the relationship between the two axes; see the module
/// docs:
///
/// ```text
/// is_speculatable(v)  ==  !may_trap(v) && !has_observable_side_effect(v)
/// ```
pub(crate) fn is_speculatable(cfg: &Cfg, value: CfgValue) -> bool {
    !may_trap(cfg, value) && !has_observable_side_effect(cfg, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgInst, CfgInstData, Place, PlaceBase, Projection, Terminator};
    use lasso::ThreadedRodeo;
    use rue_air::Type;
    use rue_span::Span;

    fn make_cfg() -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg
    }

    fn add(cfg: &mut Cfg, data: CfgInstData, ty: Type) -> CfgValue {
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

    fn konst(cfg: &mut Cfg, v: u64) -> CfgValue {
        add(cfg, CfgInstData::Const(v), Type::I32)
    }

    // --- The trap axis: every trapping op reports may_trap and is NOT
    //     speculatable, independent of any observable-effect grouping. ---

    #[test]
    fn arithmetic_may_trap() {
        let mut cfg = make_cfg();
        let a = konst(&mut cfg, 1);
        let b = konst(&mut cfg, 2);
        let ops = [
            add(&mut cfg, CfgInstData::Add(a, b), Type::I32),
            add(&mut cfg, CfgInstData::Sub(a, b), Type::I32),
            add(&mut cfg, CfgInstData::Mul(a, b), Type::I32),
            add(&mut cfg, CfgInstData::Div(a, b), Type::I32),
            add(&mut cfg, CfgInstData::Mod(a, b), Type::I32),
            add(&mut cfg, CfgInstData::Neg(a), Type::I32),
        ];
        for op in ops {
            assert!(may_trap(&cfg, op), "arithmetic must be may_trap");
            assert!(
                !is_speculatable(&cfg, op),
                "trapping arithmetic must NOT be speculatable"
            );
            assert!(
                !has_observable_side_effect(&cfg, op),
                "arithmetic is pure — no observable side effect"
            );
        }
    }

    #[test]
    fn intcast_may_trap() {
        // IntCast range-checks and panics; it must be in the trapping set even
        // though it is otherwise pure (the distinction an earlier LICM draft
        // missed, ADR-0054 §2).
        let mut cfg = make_cfg();
        let a = konst(&mut cfg, 300);
        let cast = add(
            &mut cfg,
            CfgInstData::IntCast {
                value: a,
                from_ty: Type::I32,
            },
            Type::I8,
        );
        assert!(may_trap(&cfg, cast), "IntCast must be may_trap");
        assert!(!is_speculatable(&cfg, cast));
        assert!(!has_observable_side_effect(&cfg, cast));
    }

    #[test]
    fn indexed_place_read_may_trap() {
        // A PlaceRead through an Index projection performs a bounds check.
        let mut cfg = make_cfg();
        let idx = konst(&mut cfg, 0);
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                Type::I32,
                [Projection::Index {
                    array_type: Type::I32,
                    index: idx,
                }],
            )
            .unwrap();
        let read = add(&mut cfg, CfgInstData::PlaceRead { place }, Type::I32);
        assert!(may_trap(&cfg, read), "indexed PlaceRead must be may_trap");
        assert!(!is_speculatable(&cfg, read));
        assert!(!has_observable_side_effect(&cfg, read));
    }

    #[test]
    fn non_indexed_place_read_does_not_trap() {
        // A field/base PlaceRead has no bounds check: pure and speculatable.
        let mut cfg = make_cfg();
        let base = Place::local(0, Type::I32);
        let read = add(&mut cfg, CfgInstData::PlaceRead { place: base }, Type::I32);
        assert!(!may_trap(&cfg, read), "non-indexed PlaceRead must not trap");
        assert!(
            is_speculatable(&cfg, read),
            "non-indexed PlaceRead is speculatable"
        );
    }

    #[test]
    fn indirect_place_read_may_trap() {
        // Dereferencing an indirect base can fault even without projections;
        // classifying only Index projections would let LICM speculate it.
        let mut cfg = make_cfg();
        let type_pool = rue_air::TypeInternPool::new();
        let pointer = add(
            &mut cfg,
            CfgInstData::Const(0),
            Type::new_ptr_const(type_pool.intern_ptr_const_from_type(Type::I32)),
        );
        let place = cfg
            .make_place(PlaceBase::Indirect(pointer), Type::I32, std::iter::empty())
            .unwrap();
        let read_place = place.duplicate_with_owner();
        let read = add(
            &mut cfg,
            CfgInstData::PlaceRead { place: read_place },
            Type::I32,
        );
        assert!(may_trap(&cfg, read), "indirect PlaceRead must be may_trap");
        assert!(!is_speculatable(&cfg, read));
        assert!(!has_observable_side_effect(&cfg, read));

        // PlaceWrite remains on the observable-effect axis; the indirect-base
        // trap classification is intentionally specific to PlaceRead.
        let value = konst(&mut cfg, 1);
        let write = add(
            &mut cfg,
            CfgInstData::PlaceWrite { place, value },
            Type::UNIT,
        );
        assert!(!may_trap(&cfg, write));
        assert!(has_observable_side_effect(&cfg, write));
        assert!(!is_speculatable(&cfg, write));
    }

    // --- The side-effect axis: every observable op reports the effect and is
    //     NOT speculatable, but (except where it also traps) is NOT may_trap. ---

    #[test]
    fn effecting_ops_are_not_speculatable_and_not_trapping() {
        let mut cfg = make_cfg();
        let v = konst(&mut cfg, 7);
        let interner = ThreadedRodeo::new();
        let name = interner.get_or_intern("f");
        let args = cfg.push_call_args(std::iter::empty()).unwrap();
        let iargs = cfg.push_intrinsic_args(std::iter::empty()).unwrap();

        let effecting = [
            add(
                &mut cfg,
                CfgInstData::Call {
                    runtime: None,
                    name,
                    args,
                },
                Type::UNIT,
            ),
            add(
                &mut cfg,
                CfgInstData::Intrinsic {
                    runtime: None,
                    name,
                    args: iargs,
                },
                Type::UNIT,
            ),
            add(&mut cfg, CfgInstData::Alloc { slot: 0, init: v }, Type::I32),
            add(
                &mut cfg,
                CfgInstData::Store { slot: 0, value: v },
                Type::UNIT,
            ),
            add(
                &mut cfg,
                CfgInstData::ParamStore {
                    param_slot: 0,
                    value: v,
                },
                Type::UNIT,
            ),
            add(
                &mut cfg,
                CfgInstData::PlaceWrite {
                    place: Place::local(0, Type::I32),
                    value: v,
                },
                Type::UNIT,
            ),
            add(&mut cfg, CfgInstData::Drop { value: v }, Type::UNIT),
            add(
                &mut cfg,
                CfgInstData::StorageLive {
                    slot: 0,
                    local_ty: Type::I32,
                },
                Type::UNIT,
            ),
            add(
                &mut cfg,
                CfgInstData::StorageDead {
                    slot: 0,
                    local_ty: Type::I32,
                },
                Type::UNIT,
            ),
        ];
        for op in effecting {
            assert!(
                has_observable_side_effect(&cfg, op),
                "op must report an observable side effect"
            );
            assert!(!may_trap(&cfg, op), "these effecting ops are not may_trap");
            assert!(
                !is_speculatable(&cfg, op),
                "effecting ops must NOT be speculatable"
            );
        }
    }

    // --- The pure-and-speculatable set: bitwise, shifts, comparisons,
    //     Not/BitNot, and constants. ---

    #[test]
    fn pure_ops_are_speculatable() {
        let mut cfg = make_cfg();
        let a = konst(&mut cfg, 1);
        let b = konst(&mut cfg, 2);
        let ops = [
            konst(&mut cfg, 42),
            add(&mut cfg, CfgInstData::BoolConst(true), Type::BOOL),
            add(&mut cfg, CfgInstData::BitAnd(a, b), Type::I32),
            add(&mut cfg, CfgInstData::BitOr(a, b), Type::I32),
            add(&mut cfg, CfgInstData::BitXor(a, b), Type::I32),
            add(&mut cfg, CfgInstData::Shl(a, b), Type::I32),
            add(&mut cfg, CfgInstData::Shr(a, b), Type::I32),
            add(&mut cfg, CfgInstData::Eq(a, b), Type::BOOL),
            add(&mut cfg, CfgInstData::Ne(a, b), Type::BOOL),
            add(&mut cfg, CfgInstData::Lt(a, b), Type::BOOL),
            add(&mut cfg, CfgInstData::Gt(a, b), Type::BOOL),
            add(&mut cfg, CfgInstData::Le(a, b), Type::BOOL),
            add(&mut cfg, CfgInstData::Ge(a, b), Type::BOOL),
            add(&mut cfg, CfgInstData::Not(a), Type::BOOL),
            add(&mut cfg, CfgInstData::BitNot(a), Type::I32),
        ];
        for op in ops {
            assert!(
                is_speculatable(&cfg, op),
                "bitwise/shift/comparison/const must be speculatable"
            );
            assert!(!may_trap(&cfg, op), "these ops do not trap");
            assert!(!has_observable_side_effect(&cfg, op));
        }
    }

    // --- The distinction that motivates the split: pure-but-trapping. A single
    //     "has effects" bit cannot express this; the two axes must be
    //     independent. ---

    #[test]
    fn pure_but_trapping_is_distinct_from_effecting() {
        let mut cfg = make_cfg();
        let a = konst(&mut cfg, 1);
        let b = konst(&mut cfg, 2);
        let arith = add(&mut cfg, CfgInstData::Add(a, b), Type::I32);
        let store = add(
            &mut cfg,
            CfgInstData::Store { slot: 0, value: a },
            Type::UNIT,
        );
        let _ = b;

        // Arithmetic: traps but has no observable effect.
        assert!(may_trap(&cfg, arith));
        assert!(!has_observable_side_effect(&cfg, arith));

        // Store: has an observable effect but does not trap.
        assert!(!may_trap(&cfg, store));
        assert!(has_observable_side_effect(&cfg, store));

        // Neither is speculatable, but for opposite reasons.
        assert!(!is_speculatable(&cfg, arith));
        assert!(!is_speculatable(&cfg, store));

        // Give the block a terminator so the CFG is well-formed for any
        // downstream inspection.
        let entry = cfg.entry;
        cfg.set_terminator(entry, Terminator::Return { value: Some(arith) });
    }
}
