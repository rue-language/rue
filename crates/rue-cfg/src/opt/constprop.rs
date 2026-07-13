//! Store-to-load constant propagation for local slots.
//!
//! CfgBuilder materializes every `let` as a stack slot: the initializer
//! becomes an `Alloc` and each later use a `Load`. Constant folding only
//! sees literal `Const` operands, so without this pass
//! `let a = -2147483648; let q = a / -1;` never folds even though `a` is a
//! compile-time constant (RUE-154).
//!
//! ## What it does
//!
//! For each local slot with exactly ONE whole-slot write (its `Alloc`, or a
//! single `Store`) whose value is a `Const`/`BoolConst`, and that is never
//! written through any other channel, every `Load` of the slot is replaced
//! by that constant. Interleaved with constfold (see [`super::optimize`]),
//! this lets constants flow through chains of single-assignment lets.
//!
//! ## Safety argument
//!
//! - Slots written more than once (mutated variables) are skipped.
//! - Partial or aliased writes disqualify the slot: projected `PlaceWrite`,
//!   and by-ref (`inout`/`borrow`) call arguments — those pass
//!   the slot's ADDRESS to the callee, which may write through it, and the
//!   by-ref lowering requires the argument to stay a place (`Load`) anyway.
//! - Dominance: with a single write site, sema's definite-initialization
//!   guarantee means every `Load` executes after (an execution of) the
//!   write, and the write always stores the same constant.

use crate::{Cfg, CfgInstData, PlaceBase};

/// State of a local slot while scanning for writes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    /// No write seen yet.
    NoWrite,
    /// Exactly one whole-slot write of this constant payload.
    OneConstWrite(ConstPayload),
    /// Multiple writes, a non-constant write, a partial write, or the
    /// slot's address escapes (by-ref call argument).
    Disqualified,
}

/// The constant payloads we propagate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConstPayload {
    Int(u64),
    Bool(bool),
}

/// Run store-to-load constant propagation on the CFG.
///
/// Returns `true` if any `Load` was replaced (callers re-run constfold to
/// fold the newly exposed constant operands).
pub fn run(cfg: &mut Cfg) -> bool {
    let mut slots = vec![SlotState::NoWrite; cfg.num_locals() as usize];

    // Slots whose address escapes through an address-taking intrinsic
    // (`@raw`/`@raw_mut`/`@field_ptr`, recorded by CfgBuilder) must keep
    // their Loads as places: codegen lowers those intrinsics by taking the
    // operand's address, so rewriting the Load into a Const would make the
    // constant be dereferenced as an address (RUE-521 O1+ segfault). Same
    // reasoning as the by-ref call-argument disqualification below.
    for (slot, state) in slots.iter_mut().enumerate() {
        if cfg.is_address_taken(slot as u32) {
            *state = SlotState::Disqualified;
        }
    }

    // Zero-sized locals (`[T; 0]`, `()`, empty structs) occupy 0 slots, so a
    // zero-sized local at the end of the frame is assigned a slot index equal
    // to `num_locals` — out of range for `slots`. Its Alloc/Load move no
    // bytes, so there is nothing to track or rewrite: skip out-of-range slots
    // (RUE-194).
    let record_write = |slots: &mut Vec<SlotState>, slot: u32, payload: Option<ConstPayload>| {
        let Some(state) = slots.get_mut(slot as usize) else {
            return;
        };
        *state = match (*state, payload) {
            (SlotState::NoWrite, Some(c)) => SlotState::OneConstWrite(c),
            _ => SlotState::Disqualified,
        };
    };

    // Pass 1: classify every local slot by the writes it receives. Only
    // instructions still attached to a block matter; values orphaned by
    // earlier passes never execute.
    for block_idx in 0..cfg.block_count() {
        let block_id = crate::BlockId::from_raw(block_idx as u32);
        for i in 0..cfg.get_block(block_id).insts.len() {
            let value = cfg.get_block(block_id).insts[i];
            match &cfg.get_inst(value).data {
                CfgInstData::Alloc { slot, init } | CfgInstData::Store { slot, value: init } => {
                    let payload = match &cfg.get_inst(*init).data {
                        CfgInstData::Const(v) => Some(ConstPayload::Int(*v)),
                        CfgInstData::BoolConst(b) => Some(ConstPayload::Bool(*b)),
                        _ => None,
                    };
                    record_write(&mut slots, *slot, payload);
                }
                CfgInstData::PlaceWrite { place, .. } => {
                    if let PlaceBase::Local(slot) = place.base {
                        record_write(&mut slots, slot, None);
                    }
                }
                // By-ref call arguments pass the ADDRESS of the argument
                // place: the callee may write through it (inout), and the
                // by-ref lowering needs the arg to remain a Load/PlaceRead.
                // Disqualify any local the argument is rooted at.
                CfgInstData::Call {
                    args_start,
                    args_len,
                    ..
                } => {
                    let byref_args: Vec<_> = cfg
                        .get_call_args(*args_start, *args_len)
                        .iter()
                        .filter(|a| a.is_by_ref())
                        .map(|a| a.value)
                        .collect();
                    for arg_value in byref_args {
                        match &cfg.get_inst(arg_value).data {
                            CfgInstData::Load { slot } => {
                                record_write(&mut slots, *slot, None);
                            }
                            CfgInstData::PlaceRead { place } => {
                                if let PlaceBase::Local(slot) = place.base {
                                    record_write(&mut slots, slot, None);
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

    // Pass 2: rewrite Loads of single-constant slots.
    let mut changed = false;
    for value_idx in 0..cfg.value_count() {
        let value = crate::CfgValue::from_raw(value_idx as u32);
        let slot = match &cfg.get_inst(value).data {
            CfgInstData::Load { slot } => *slot,
            _ => continue,
        };
        if let Some(&SlotState::OneConstWrite(payload)) = slots.get(slot as usize) {
            cfg.get_inst_mut(value).data = match payload {
                ConstPayload::Int(v) => CfgInstData::Const(v),
                ConstPayload::Bool(b) => CfgInstData::BoolConst(b),
            };
            changed = true;
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgArgMode, CfgCallArg, CfgInst, CfgValue, Terminator, Type};
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

        assert!(run(&mut cfg));
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

        assert!(run(&mut cfg));
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

        assert!(!run(&mut cfg));
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
        let (args_start, args_len) = cfg.push_extra(vec![load]);
        let raw_sym = Spur::try_from_usize(0).unwrap();
        let ptr = push(
            &mut cfg,
            CfgInstData::Intrinsic {
                name: raw_sym,
                args_start,
                args_len,
            },
            Type::I32,
        );
        cfg.mark_address_taken(0);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(ptr) });

        assert!(!run(&mut cfg));
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_non_const_init_not_propagated() {
        let mut cfg = make_cfg(2);
        let c = push(&mut cfg, CfgInstData::Const(3), Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(c, c), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: sum },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        assert!(!run(&mut cfg));
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
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
        let (args_start, args_len) = cfg.push_call_args([CfgCallArg {
            value: arg_load,
            mode: CfgArgMode::Inout,
        }]);
        push(
            &mut cfg,
            CfgInstData::Call {
                name: Spur::try_from_usize(0).unwrap(),
                args_start,
                args_len,
            },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        assert!(!run(&mut cfg));
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

        assert!(!run(&mut cfg));
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
        let (args_start, args_len) = cfg.push_call_args([CfgCallArg {
            value: arg_load,
            mode: CfgArgMode::Normal,
        }]);
        push(
            &mut cfg,
            CfgInstData::Call {
                name: Spur::try_from_usize(0).unwrap(),
                args_start,
                args_len,
            },
            Type::UNIT,
        );
        cfg.set_terminator(cfg.entry, Terminator::Return { value: None });

        assert!(run(&mut cfg));
        assert!(matches!(cfg.get_inst(arg_load).data, CfgInstData::Const(5)));
    }
}
