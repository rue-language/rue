//! Single owner of the local-slot write/escape classification behind
//! store-to-load forwarding (the RUE-521 safety knowledge).
//!
//! Constant store-to-load propagation ([`super::constopt`]) and value
//! forwarding's Rule 1 ([`super::forward`]) both replace a `Load { slot }`
//! with the slot's single written value, and both are sound only for slots
//! that nothing else can write. Each pass used to restate the scan that
//! decides this; a rule learned in one (a new escape channel, a new write
//! form) could silently miss the other. Following the [`super::classify`]
//! precedent (ADR-0054 §2), this module names the discipline once and both
//! passes consume it.
//!
//! ## What qualifies a slot
//!
//! A local slot is [`SlotWrites::One`] when it receives exactly ONE whole-slot
//! write among block-attached instructions — its `Alloc`, or a single `Store`
//! — and is never written through any other channel. (Only block-attached
//! instructions matter; values orphaned by earlier passes never execute.)
//! Everything else is [`SlotWrites::Disqualified`]:
//!
//! - **Multiple whole-slot writes** (mutated variables): which write a load
//!   observes depends on the path taken.
//! - **A projected `PlaceWrite` whose base is the local**: a partial write
//!   means the slot's contents are no longer the whole-slot write's value.
//! - **By-ref (`inout`/`borrow`) call arguments** rooted at a `Load` or
//!   `PlaceRead` of the local, on either call form (`Call`/`AccessorCall`):
//!   those pass the slot's ADDRESS to the callee, which may write through it,
//!   and the by-ref lowering requires the argument to stay a place anyway.
//! - **Address-taken slots** (`@raw`/`@raw_mut`/`@field_ptr`, recorded by
//!   CfgBuilder): codegen lowers those intrinsics by taking the operand's
//!   address, so a rewritten `Load` would make the forwarded value be
//!   dereferenced as an address, and a raw pointer may write the slot behind
//!   the optimizer's back (RUE-521 O1+ segfault). These slots are
//!   disqualified upfront, before any write is seen.
//!
//! ## Dominance
//!
//! With a single write site, sema's definite-initialization guarantee means
//! every `Load` executes after (an execution of) the write, so the write's
//! block dominates every load and the stored value — computed *before* the
//! store — is available at each load. [`SlotWrites::One`] records the writing
//! block so [`super::forward`] can verify that argument explicitly against
//! [`crate::dominators`] in `debug_assertions` builds.
//!
//! ## Zero-sized locals (RUE-194)
//!
//! A zero-sized local (`[T; 0]`, `()`, an empty struct) occupies 0 slots, so
//! a trailing one is assigned a slot index equal to `num_locals` — out of
//! range for the classification table. Its `Alloc`/`Load` move no bytes, so
//! there is nothing to track or rewrite: out-of-range slots are skipped, not
//! a panic.
//!
//! ## Why callers differ on reachability
//!
//! [`super::forward`] restricts the scan (and its rewriting walk) to blocks
//! reachable AFTER [`super::simplify`]'s constant-terminator folding: a load
//! in a statically dead arm never executes and is dominated by nothing, so
//! forwarding it would violate the Rule 1 dominance invariant (it tripped the
//! debug assertion — 2026-07-16 optimizer hunt), and an unreachable store
//! must not count against (or for) a slot's classification either.
//! [`super::constopt`] runs before simplify has folded anything, scans all
//! blocks, and needs no dominator reasoning for its rewrite — an unreachable
//! write can only *narrow* its result (disqualify a slot or leave a dead
//! constant unpropagated), never unsoundly widen it — so it deliberately
//! passes `None` rather than paying for a reachability computation.

use super::dce::BitSet;
use crate::{BlockId, Cfg, CfgInstData, CfgValue, PlaceBase};

/// Classification of a local slot's writes.
#[derive(Clone, Copy)]
pub(super) enum SlotWrites {
    /// No write seen yet.
    None,
    /// Exactly one whole-slot write, initializing from `value` in `block`.
    One {
        /// The written value. It carries no constness promise: `constopt`
        /// waits for it to become constant, `forward` forwards it as-is.
        value: CfgValue,
        /// The block containing the write, for the dominance check in
        /// `forward` (see the module docs). `constopt` ignores it.
        block: BlockId,
    },
    /// Multiple writes, a partial/aliased write, an address escape, or a
    /// by-ref pass. Loads of this slot are never forwarded from a single
    /// write (forwarding's block-local Rule 2 may still apply).
    Disqualified,
}

/// Classify every local slot by the writes it receives.
///
/// `reachable` selects which blocks the scan observes: `None` scans all
/// blocks (`constopt`'s semantics), `Some(set)` only the blocks in the set
/// (`forward`'s semantics — see "Why callers differ on reachability" in the
/// module docs). Nothing here depends on which values are constant or on any
/// later rewriting, so one scan per pass run suffices.
pub(super) fn classify_slot_writes(cfg: &Cfg, reachable: Option<&BitSet>) -> Vec<SlotWrites> {
    let num_locals = cfg.num_locals() as usize;
    let mut slot_writes = vec![SlotWrites::None; num_locals];

    // Address-taken slots are disqualified upfront (RUE-521; module docs).
    for (slot, state) in slot_writes.iter_mut().enumerate() {
        if cfg.is_address_taken(slot as u32) {
            *state = SlotWrites::Disqualified;
        }
    }

    // Out-of-range slots (trailing zero-sized locals) are skipped (RUE-194;
    // module docs).
    fn record_write(slot_writes: &mut [SlotWrites], slot: u32, write: Option<(CfgValue, BlockId)>) {
        let Some(state) = slot_writes.get_mut(slot as usize) else {
            return;
        };
        *state = match (&*state, write) {
            (SlotWrites::None, Some((value, block))) => SlotWrites::One { value, block },
            _ => SlotWrites::Disqualified,
        };
    }

    for block in cfg.blocks() {
        if let Some(reachable) = reachable
            && !reachable.contains(block.id.as_u32())
        {
            continue;
        }
        for &value in &block.insts {
            match &cfg.get_inst(value).data {
                CfgInstData::Alloc { slot, init } | CfgInstData::Store { slot, value: init } => {
                    record_write(&mut slot_writes, *slot, Some((*init, block.id)));
                }
                CfgInstData::PlaceWrite { place, .. } => {
                    if let PlaceBase::Local(slot) = place.base {
                        record_write(&mut slot_writes, slot, None);
                    }
                }
                // By-ref arguments on either call form pass the ADDRESS of
                // the argument place: disqualify any local the argument
                // roots (module docs).
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

    slot_writes
}
