//! Sparse worklist driver for constant folding and constant propagation.
//!
//! The optimizer used to interleave full constant-folding and store-to-load
//! propagation scans in a loop until a fixpoint. A dependency chain
//! (`let x1 = x0 + 1; let x2 = x1 + 1; ...`) exposes only one new constant
//! per round, so ordinary straight-line programs made -O1 quadratic in
//! function size (RUE-794). This driver reaches the same fixpoint sparsely:
//! every instruction gets one seed fold attempt, and after that an
//! instruction is revisited only when one of its operands actually becomes
//! constant. Each value becomes constant at most once, so total work is
//! bounded by the instruction count plus the def-use edge count.
//!
//! The fold kernel (one instruction at a time) lives in [`super::constfold`];
//! this module owns the driver and the store-to-load propagation for local
//! slots.
//!
//! ## Store-to-load propagation
//!
//! CfgBuilder materializes every `let` as a stack slot: the initializer
//! becomes an `Alloc` and each later use a `Load`. Constant folding only
//! sees literal `Const` operands, so without propagation
//! `let a = -2147483648; let q = a / -1;` never folds even though `a` is a
//! compile-time constant (RUE-154).
//!
//! For each local slot with exactly ONE whole-slot write (its `Alloc`, or a
//! single `Store`) whose value is or becomes a `Const`/`BoolConst`, and that
//! is never written through any other channel, every `Load` of the slot is
//! replaced by that constant.
//!
//! ## Safety argument
//!
//! - Slots written more than once (mutated variables) are skipped.
//! - Partial or aliased writes disqualify the slot: projected `PlaceWrite`,
//!   address-taking intrinsics (`@raw`/`@raw_mut`/`@field_ptr`, RUE-521), and
//!   by-ref (`inout`/`borrow`) call arguments — those pass the slot's ADDRESS
//!   to the callee, which may write through it, and the by-ref lowering
//!   requires the argument to stay a place (`Load`) anyway.
//! - Dominance: with a single write site, sema's definite-initialization
//!   guarantee means every `Load` executes after (an execution of) the
//!   write, and the write always stores the same constant.

use std::collections::VecDeque;

use crate::{Cfg, CfgInstData, CfgValue, PlaceBase};

use super::constfold;

/// Work counters for one run.
///
/// These exist so tests can prove the driver does bounded work (RUE-794):
/// `fold_attempts` is structurally O(values + def-use edges) — one seed
/// attempt per value plus one re-attempt per operand→user edge whose operand
/// became constant — where the old rescan loop performed
/// O(values × rounds) attempts.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Calls into the fold kernel.
    pub fold_attempts: u64,
    /// Instructions the fold kernel replaced with a constant.
    pub folded: u64,
    /// `Load`s replaced by their slot's single constant write.
    pub loads_rewritten: u64,
}

/// State of a local slot while scanning for writes.
#[derive(Clone, Copy)]
enum SlotWrite {
    /// No write seen yet.
    None,
    /// Exactly one whole-slot write, initializing from this value.
    ///
    /// The value need not be constant yet: if the seed pass or a later event
    /// turns it into one, the slot's loads are rewritten at that point. (The
    /// old rescan loop re-derived slot constness from scratch every round;
    /// deferring through `init_watchers` is the sparse equivalent.)
    One(CfgValue),
    /// Multiple writes, a non-whole-slot write, or an address escape.
    /// Loads of this slot are never rewritten.
    Disqualified,
}

/// Run sparse constant folding and store-to-load constant propagation to a
/// fixpoint.
pub fn run(cfg: &mut Cfg) -> Stats {
    let value_count = cfg.value_count();
    let num_locals = cfg.num_locals() as usize;
    let mut stats = Stats::default();

    // ------------------------------------------------------------------
    // Classify every local slot by the writes it receives. Only
    // instructions still attached to a block matter; values orphaned by
    // earlier passes never execute. Nothing here depends on which values
    // are constant, so one scan suffices for the whole run.
    // ------------------------------------------------------------------
    let mut slot_writes = vec![SlotWrite::None; num_locals];

    // Slots whose address escapes through an address-taking intrinsic
    // (`@raw`/`@raw_mut`/`@field_ptr`, recorded by CfgBuilder) must keep
    // their Loads as places: codegen lowers those intrinsics by taking the
    // operand's address, so rewriting the Load into a Const would make the
    // constant be dereferenced as an address (RUE-521 O1+ segfault). Same
    // reasoning as the by-ref call-argument disqualification below.
    for (slot, state) in slot_writes.iter_mut().enumerate() {
        if cfg.is_address_taken(slot as u32) {
            *state = SlotWrite::Disqualified;
        }
    }

    // Zero-sized locals (`[T; 0]`, `()`, empty structs) occupy 0 slots, so a
    // zero-sized local at the end of the frame is assigned a slot index equal
    // to `num_locals` — out of range for `slot_writes`. Its Alloc/Load move
    // no bytes, so there is nothing to track or rewrite: skip out-of-range
    // slots (RUE-194).
    fn record_write(slot_writes: &mut [SlotWrite], slot: u32, init: Option<CfgValue>) {
        let Some(state) = slot_writes.get_mut(slot as usize) else {
            return;
        };
        *state = match (*state, init) {
            (SlotWrite::None, Some(v)) => SlotWrite::One(v),
            _ => SlotWrite::Disqualified,
        };
    }

    for block in cfg.blocks() {
        for &value in &block.insts {
            match &cfg.get_inst(value).data {
                CfgInstData::Alloc { slot, init } | CfgInstData::Store { slot, value: init } => {
                    record_write(&mut slot_writes, *slot, Some(*init));
                }
                CfgInstData::PlaceWrite { place, .. } => {
                    if let PlaceBase::Local(slot) = place.base {
                        record_write(&mut slot_writes, slot, None);
                    }
                }
                // By-ref arguments on either call form pass the ADDRESS of
                // the argument place: the callee may write through it
                // (inout), and lowering needs the arg to remain a
                // Load/PlaceRead. Disqualify any local the argument roots.
                CfgInstData::Call { args, .. } | CfgInstData::AccessorCall { args, .. } => {
                    for arg in cfg.call_args(args) {
                        if !arg.is_by_ref() {
                            continue;
                        }
                        match &cfg.get_inst(arg.value).data {
                            CfgInstData::Load { slot } => {
                                record_write(&mut slot_writes, *slot, None);
                            }
                            CfgInstData::PlaceRead { place } => {
                                if let PlaceBase::Local(slot) = place.base {
                                    record_write(&mut slot_writes, slot, None);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------------
    // Build the sparse edges, over ALL values (matching the old passes,
    // which scanned every value; orphaned instructions are folded too and
    // swept by DCE afterwards):
    //
    // - `users[v]`: foldable instructions that read v as an operand, to
    //   re-attempt when v becomes constant.
    // - `loads_of_slot[s]`: the Loads to rewrite when slot s's single write
    //   becomes constant.
    // - `init_watchers[v]`: slots whose single whole-slot write initializes
    //   from v, resolved when v becomes constant.
    // ------------------------------------------------------------------
    let mut users: Vec<Vec<CfgValue>> = vec![Vec::new(); value_count];
    let mut loads_of_slot: Vec<Vec<CfgValue>> = vec![Vec::new(); num_locals];
    for i in 0..value_count {
        let value = CfgValue::from_raw(i as u32);
        match &cfg.get_inst(value).data {
            CfgInstData::Add(a, b)
            | CfgInstData::Sub(a, b)
            | CfgInstData::Mul(a, b)
            | CfgInstData::WrappingAdd(a, b)
            | CfgInstData::WrappingSub(a, b)
            | CfgInstData::WrappingMul(a, b)
            | CfgInstData::Div(a, b)
            | CfgInstData::Mod(a, b)
            | CfgInstData::Eq(a, b)
            | CfgInstData::Ne(a, b)
            | CfgInstData::Lt(a, b)
            | CfgInstData::Gt(a, b)
            | CfgInstData::Le(a, b)
            | CfgInstData::Ge(a, b)
            | CfgInstData::BitAnd(a, b)
            | CfgInstData::BitOr(a, b)
            | CfgInstData::BitXor(a, b)
            | CfgInstData::Shl(a, b)
            | CfgInstData::Shr(a, b) => {
                users[a.as_u32() as usize].push(value);
                users[b.as_u32() as usize].push(value);
            }
            CfgInstData::Neg(a) | CfgInstData::Not(a) | CfgInstData::BitNot(a) => {
                users[a.as_u32() as usize].push(value);
            }
            CfgInstData::Load { slot } => {
                if let Some(loads) = loads_of_slot.get_mut(*slot as usize) {
                    loads.push(value);
                }
            }
            _ => {}
        }
    }

    let mut init_watchers: Vec<Vec<u32>> = vec![Vec::new(); value_count];
    for (slot, state) in slot_writes.iter().enumerate() {
        if let SlotWrite::One(init) = state {
            init_watchers[init.as_u32() as usize].push(slot as u32);
        }
    }

    // ------------------------------------------------------------------
    // Seed: one fold attempt per instruction, in index order. Every value
    // that is constant — originally or by folding — enters the event queue
    // exactly once.
    // ------------------------------------------------------------------
    let mut became_const: VecDeque<CfgValue> = VecDeque::new();
    for i in 0..value_count {
        let value = CfgValue::from_raw(i as u32);
        if const_payload(cfg, value).is_some() {
            became_const.push_back(value);
        } else {
            stats.fold_attempts += 1;
            if constfold::fold_instruction(cfg, value) {
                stats.folded += 1;
                became_const.push_back(value);
            }
        }
    }

    // ------------------------------------------------------------------
    // Event loop: when a value becomes constant, re-attempt its users and
    // resolve any slot whose single write it initializes. Rewritten Loads
    // are themselves newly constant and re-enter the queue, which is what
    // lets constants flow through chains of single-assignment lets.
    // ------------------------------------------------------------------
    while let Some(value) = became_const.pop_front() {
        let idx = value.as_u32() as usize;

        for &user in &users[idx] {
            stats.fold_attempts += 1;
            if constfold::fold_instruction(cfg, user) {
                stats.folded += 1;
                became_const.push_back(user);
            }
        }

        // Take the watcher list so each slot resolves exactly once.
        for slot in std::mem::take(&mut init_watchers[idx]) {
            let payload = const_payload(cfg, value)
                .expect("became_const events carry Const/BoolConst values");
            for &load in &loads_of_slot[slot as usize] {
                cfg.get_inst_mut(load).data = payload.duplicate_with_owner();
                stats.loads_rewritten += 1;
                became_const.push_back(load);
            }
        }
    }

    stats
}

/// The constant payloads propagation understands. Strings and aggregates are
/// not propagated (matching the fold kernel, which only produces these two).
fn const_payload(cfg: &Cfg, value: CfgValue) -> Option<CfgInstData> {
    match &cfg.get_inst(value).data {
        CfgInstData::Const(v) => Some(CfgInstData::Const(*v)),
        CfgInstData::BoolConst(b) => Some(CfgInstData::BoolConst(*b)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgArgMode, CfgCallArg, CfgInst, Terminator, Type};
    use lasso::{Key, Spur};
    use rue_span::Span;

    fn make_cfg(num_locals: u32) -> Cfg {
        let mut cfg = Cfg::new(Type::I32, num_locals, 0, "test".to_string(), vec![]);
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
    fn test_propagates_single_const_alloc() {
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(7), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg);
        assert_eq!(stats.loads_rewritten, 1);
        match &cfg.get_inst(load).data {
            CfgInstData::Const(7) => {}
            other => panic!("Expected Const(7), got {:?}", other),
        }
    }

    #[test]
    fn test_propagates_bool_const() {
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::BoolConst(true), Type::BOOL);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::BOOL);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg);
        assert_eq!(stats.loads_rewritten, 1);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::BoolConst(true)
        ));
    }

    #[test]
    fn test_mutated_slot_not_propagated() {
        // let a = 1; a = 2; a -> two writes, must not propagate either.
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c1 },
            Type::UNIT,
        );
        let c2 = push(&mut cfg, CfgInstData::Const(2), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Store { slot: 0, value: c2 },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg);
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_address_taken_slot_not_propagated() {
        // let x = 42; @raw(x) — CfgBuilder marks x's slot address-taken, so
        // its Load must survive as a place even though the slot has exactly
        // one constant write. Rewriting it to Const(42) makes codegen
        // dereference 42 as an address (RUE-521 O1+ segfault).
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(42), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg.push_intrinsic_args(vec![load]).unwrap();
        let raw_sym = Spur::try_from_usize(0).unwrap();
        let ptr = push(
            &mut cfg,
            CfgInstData::Intrinsic {
                runtime: None,
                name: raw_sym,
                args,
            },
            Type::I32,
        );
        cfg.mark_address_taken(0);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(ptr) });

        let stats = run(&mut cfg);
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_non_const_init_not_propagated() {
        // The slot's single write is a Param — never constant, never
        // propagated.
        let mut cfg = make_cfg(1);
        let p = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: p },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg);
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_folded_init_propagates_after_seed() {
        // let a = 3 + 4; a — at classification time the init is a non-const
        // Add. The old driver caught this on its second rescan round; the
        // sparse driver must catch it when the Add's fold event resolves the
        // watching slot.
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(3), Type::I32);
        let c2 = push(&mut cfg, CfgInstData::Const(4), Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(c1, c2), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: sum },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg);
        assert_eq!(stats.folded, 1);
        assert_eq!(stats.loads_rewritten, 1);
        assert!(matches!(cfg.get_inst(load).data, CfgInstData::Const(7)));
    }

    #[test]
    fn test_byref_call_arg_disqualifies_slot() {
        // f(inout a): the callee may rewrite a, and the by-ref lowering
        // needs the Load to remain a place. Nothing may be propagated.
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let arg_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg
            .push_call_args([CfgCallArg {
                value: arg_load,
                mode: CfgArgMode::Inout,
            }])
            .unwrap();
        push(
            &mut cfg,
            CfgInstData::Call {
                runtime: None,
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg);
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(arg_load).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_byref_accessor_call_arg_disqualifies_slot() {
        // An accessor can write through a by-ref argument too, so constant
        // propagation must not substitute either the place argument or a
        // later load with the pre-accessor constant.
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let arg_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg
            .push_call_args([CfgCallArg {
                value: arg_load,
                mode: CfgArgMode::Inout,
            }])
            .unwrap();
        push(
            &mut cfg,
            CfgInstData::AccessorCall {
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg);
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(arg_load).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_zero_slot_local_out_of_range_is_ignored() {
        // A zero-sized local ([T; 0], unit) occupies 0 slots, so a trailing
        // one is assigned slot index == num_locals — out of range for the
        // slot table. Its Alloc/Load must be skipped, not panic (RUE-194).
        let mut cfg = make_cfg(0);
        let c = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::UNIT);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: None });

        let stats = run(&mut cfg);
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_normal_call_arg_still_propagates() {
        // f(a) by value: the slot is only read; propagation is fine.
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let arg_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg
            .push_call_args([CfgCallArg {
                value: arg_load,
                mode: CfgArgMode::Normal,
            }])
            .unwrap();
        push(
            &mut cfg,
            CfgInstData::Call {
                runtime: None,
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        cfg.set_terminator(cfg.entry, Terminator::Return { value: None });

        let stats = run(&mut cfg);
        assert_eq!(stats.loads_rewritten, 1);
        assert!(matches!(cfg.get_inst(arg_load).data, CfgInstData::Const(5)));
    }

    /// Build the CFG shape CfgBuilder produces for a `let x{i} = x{i-1} + 1`
    /// chain of the given length: Alloc slot 0 with a constant, then each
    /// binding Loads the previous slot, adds a constant, and Allocs the next
    /// slot. Returns the final Load.
    fn build_let_chain(cfg: &mut Cfg, len: u32) -> CfgValue {
        let zero = push(cfg, CfgInstData::Const(0), Type::I64);
        push(
            cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: zero,
            },
            Type::UNIT,
        );
        for i in 1..len {
            let prev = push(cfg, CfgInstData::Load { slot: i - 1 }, Type::I64);
            let one = push(cfg, CfgInstData::Const(1), Type::I64);
            let sum = push(cfg, CfgInstData::Add(prev, one), Type::I64);
            push(cfg, CfgInstData::Alloc { slot: i, init: sum }, Type::UNIT);
        }
        push(cfg, CfgInstData::Load { slot: len - 1 }, Type::I64)
    }

    fn build_wrapping_let_chain(cfg: &mut Cfg, len: u32) -> CfgValue {
        let zero = push(cfg, CfgInstData::Const(0), Type::U64);
        push(
            cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: zero,
            },
            Type::UNIT,
        );
        for i in 1..len {
            let prev = push(cfg, CfgInstData::Load { slot: i - 1 }, Type::U64);
            let one = push(cfg, CfgInstData::Const(1), Type::U64);
            let sum = push(cfg, CfgInstData::WrappingAdd(prev, one), Type::U64);
            push(cfg, CfgInstData::Alloc { slot: i, init: sum }, Type::UNIT);
        }
        push(cfg, CfgInstData::Load { slot: len - 1 }, Type::U64)
    }

    #[test]
    fn test_let_chain_fully_propagates() {
        // The RUE-794 workload: every binding's constant is exposed only by
        // propagating the previous one. The whole chain must still fold.
        let len = 100;
        let mut cfg = make_cfg(len);
        let last = build_let_chain(&mut cfg, len);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(last) });

        run(&mut cfg);
        match &cfg.get_inst(last).data {
            CfgInstData::Const(v) => assert_eq!(*v, (len - 1) as u64),
            other => panic!("Expected Const({}), got {:?}", len - 1, other),
        }
    }

    #[test]
    fn test_let_chain_work_is_linear() {
        // Structural work bound (RUE-794 acceptance criteria): the driver
        // performs one seed fold attempt per value plus at most one
        // re-attempt per operand→user edge (≤ 2 per foldable instruction),
        // regardless of chain depth. The old rescan loop needed O(len)
        // full-CFG rounds here — ~len²/8 attempts — so this bound fails
        // loudly on any regression to global rescanning.
        for len in [100, 200, 400] {
            let mut cfg = make_cfg(len);
            let last = build_let_chain(&mut cfg, len);
            cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(last) });

            let stats = run(&mut cfg);
            let values = cfg.value_count() as u64;
            assert!(
                stats.fold_attempts <= 3 * values,
                "chain len {}: {} fold attempts for {} values exceeds the \
                 sparse bound",
                len,
                stats.fold_attempts,
                values
            );
            // Every Add folded and every Load was rewritten.
            assert_eq!(stats.folded, (len - 1) as u64);
            assert_eq!(stats.loads_rewritten, len as u64);
        }
    }

    #[test]
    fn test_wrapping_let_chain_work_is_linear() {
        for len in [100, 200, 400] {
            let mut cfg = make_cfg(len);
            let last = build_wrapping_let_chain(&mut cfg, len);
            cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(last) });

            let stats = run(&mut cfg);
            let values = cfg.value_count() as u64;
            assert!(
                stats.fold_attempts <= 3 * values,
                "wrapping chain len {}: {} fold attempts for {} values exceeds the sparse bound",
                len,
                stats.fold_attempts,
                values
            );
            assert_eq!(stats.folded, (len - 1) as u64);
            assert_eq!(stats.loads_rewritten, len as u64);
            assert!(matches!(
                cfg.get_inst(last).data,
                CfgInstData::Const(v) if v == (len - 1) as u64
            ));
        }
    }
}
