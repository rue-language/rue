//! Single owner of slot write/escape classification for memory-reading
//! optimizations (the RUE-521 and RUE-1869 safety knowledge).
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
//! - **Ownership-transfer boundaries** are tracked by CFG inlining at the
//!   translated parameter `Load` values, not here at slot classification. The
//!   same slot may return ownership to the caller on one path and be dropped
//!   in the callee on another, so a slot-wide disqualification would be too
//!   coarse. Constopt and forwarding consult the per-value marker instead.
//!
//! ## Loop-scoped invariance
//!
//! LICM asks [`LoopSlotFactsWorkspace::classify_loop_slot_invariance`] whether
//! a direct local or parameter root can change in one natural loop. Direct
//! `Alloc`, `Store`, `ParamStore`, and rooted `PlaceWrite` instructions
//! invalidate only their exact slot. Address-taken roots are invalid from the
//! outset. Opaque calls, intrinsics, drops, and indirect writes remain
//! whole-loop barriers because their possible targets cannot be bounded here;
//! refining them belongs to general alias/effect analysis, not a private LICM
//! rule. The caller supplies one whole-function reachability set per loop-forest
//! sweep, and this module intersects it with the loop body so disconnected
//! instructions do not count.
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

/// Per-slot memory invariance within one loop body.
pub(super) struct LoopSlotFacts<'a> {
    cfg: &'a Cfg,
    local_generation: &'a [u64],
    param_generation: &'a [u64],
    generation: u64,
    unknown_memory_target: bool,
}

impl LoopSlotFacts<'_> {
    /// Whether a direct read of local `slot` is unchanged by the loop.
    pub(super) fn local_is_invariant(&self, slot: u32) -> bool {
        !self.unknown_memory_target
            && !self.cfg.is_address_taken(slot)
            && self
                .local_generation
                .get(slot as usize)
                .is_some_and(|generation| *generation != self.generation)
    }

    /// Whether a direct read of parameter ABI slot `slot` is unchanged by the loop.
    pub(super) fn param_is_invariant(&self, slot: u32) -> bool {
        !self.unknown_memory_target
            && !self.cfg.is_param_address_taken(slot)
            && self
                .param_generation
                .get(slot as usize)
                .is_some_and(|generation| *generation != self.generation)
    }
}

/// Reusable storage for loop-scoped slot facts.
///
/// Generation stamps make reset proportional to the writes observed in the
/// loop scan (which simply overwrite their stamp), not to the function's slot
/// count. The tables only grow when a later CFG exposes more slots.
#[derive(Default)]
pub(super) struct LoopSlotFactsWorkspace {
    local_generation: Vec<u64>,
    param_generation: Vec<u64>,
    generation: u64,
}

/// Explicit work performed by one loop-fact classification.
#[derive(Default)]
pub(super) struct LoopSlotFactWork {
    pub(super) instructions_scanned: u64,
    pub(super) entries_initialized: u64,
    pub(super) workspace_growths: u64,
}

/// Classify the memory reachable from direct local and parameter reads during
/// one loop.
///
/// Whole and projected writes with a direct root invalidate exactly that root.
/// Calls, intrinsics, accessor calls, indirect/accessor-rooted writes, and drops
/// remain conservative barriers because this analysis cannot bound their
/// memory target. Address-taken roots are invalidated up front, even when the
/// escape occurred outside the loop.
impl LoopSlotFactsWorkspace {
    pub(super) fn classify_loop_slot_invariance<'a>(
        &'a mut self,
        cfg: &'a Cfg,
        body: &[BlockId],
        reachable: &BitSet,
    ) -> (LoopSlotFacts<'a>, LoopSlotFactWork) {
        let mut work = LoopSlotFactWork::default();
        let local_count = cfg.num_locals() as usize;
        let param_count = cfg.num_params() as usize;
        if self.local_generation.len() < local_count || self.param_generation.len() < param_count {
            work.workspace_growths = 1;
        }
        if self.local_generation.len() < local_count {
            work.entries_initialized += (local_count - self.local_generation.len()) as u64;
            self.local_generation.resize(local_count, 0);
        }
        if self.param_generation.len() < param_count {
            work.entries_initialized += (param_count - self.param_generation.len()) as u64;
            self.param_generation.resize(param_count, 0);
        }

        self.generation = match self.generation.checked_add(1) {
            Some(generation) => generation,
            None => {
                // This requires 2^64 loop analyses in one run, but retaining a
                // correct bounded fallback keeps the stamp discipline total.
                self.local_generation.fill(0);
                self.param_generation.fill(0);
                work.entries_initialized +=
                    (self.local_generation.len() + self.param_generation.len()) as u64;
                1
            }
        };
        let generation = self.generation;
        let mut unknown_memory_target = false;

        fn mark(facts: &mut [u64], slot: u32, generation: u64) {
            if let Some(fact) = facts.get_mut(slot as usize) {
                *fact = generation;
            }
        }

        for &block in body {
            if !reachable.contains(block.as_u32()) {
                continue;
            }
            for &value in &cfg.get_block(block).insts {
                work.instructions_scanned += 1;
                match &cfg.get_inst(value).data {
                    CfgInstData::Alloc { slot, .. } | CfgInstData::Store { slot, .. } => {
                        mark(&mut self.local_generation, *slot, generation);
                    }
                    CfgInstData::ParamStore { param_slot, .. } => {
                        mark(&mut self.param_generation, *param_slot, generation);
                    }
                    CfgInstData::PlaceWrite { place, .. } => match place.base {
                        PlaceBase::Local(slot) => {
                            mark(&mut self.local_generation, slot, generation)
                        }
                        PlaceBase::Param(slot) => {
                            mark(&mut self.param_generation, slot, generation)
                        }
                        PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => {
                            unknown_memory_target = true;
                        }
                    },
                    CfgInstData::Call { args, .. } | CfgInstData::AccessorCall { args, .. } => {
                        // Retain exact by-ref roots in the shared facts as well
                        // as the conservative unknown-call barrier.
                        for arg in cfg.call_args(args) {
                            if !arg.is_by_ref() {
                                continue;
                            }
                            match &cfg.get_inst(arg.value).data {
                                CfgInstData::Load { slot } => {
                                    mark(&mut self.local_generation, *slot, generation)
                                }
                                CfgInstData::Param { index } => {
                                    mark(&mut self.param_generation, *index, generation)
                                }
                                CfgInstData::PlaceRead { place } => match place.base {
                                    PlaceBase::Local(slot) => {
                                        mark(&mut self.local_generation, slot, generation)
                                    }
                                    PlaceBase::Param(slot) => {
                                        mark(&mut self.param_generation, slot, generation)
                                    }
                                    PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => {}
                                },
                                _ => {}
                            }
                        }
                        unknown_memory_target = true;
                    }
                    CfgInstData::Intrinsic { .. } | CfgInstData::Drop { .. } => {
                        unknown_memory_target = true;
                    }
                    _ => {}
                }
            }
        }

        (
            LoopSlotFacts {
                cfg,
                local_generation: &self.local_generation,
                param_generation: &self.param_generation,
                generation,
                unknown_memory_target,
            },
            work,
        )
    }
}

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

    // Address-taken slots are disqualified upfront (RUE-521). Ownership
    // transfer is tracked per materialized Param Load by the callers below:
    // the same slot can return ownership to the caller on one path and be
    // dropped in the callee on another, so a slot-wide classification would be
    // too coarse.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgInst, Projection, Terminator, Type};
    use rue_span::Span;

    fn cfg(locals: u32, params: u32) -> Cfg {
        Cfg::new(
            Type::UNIT,
            locals,
            params,
            "test".to_string(),
            rue_air::ParamSlotModes::new(
                vec![false; params as usize],
                vec![false; params as usize],
            ),
        )
    }

    fn push(cfg: &mut Cfg, block: BlockId, data: CfgInstData, ty: Type) -> CfgValue {
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
    fn loop_facts_are_per_root_reachable_and_range_checked() {
        let mut cfg = cfg(2, 2);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let dead = cfg.new_block();
        let value = push(&mut cfg, entry, CfgInstData::Const(1), Type::I32);
        let index = push(&mut cfg, entry, CfgInstData::Const(0), Type::I32);
        let projected = cfg
            .make_place(
                PlaceBase::Local(1),
                Type::I32,
                [Projection::Index {
                    array_type: Type::I32,
                    index,
                }],
            )
            .unwrap();
        push(
            &mut cfg,
            entry,
            CfgInstData::PlaceWrite {
                place: projected,
                value,
            },
            Type::UNIT,
        );
        push(
            &mut cfg,
            entry,
            CfgInstData::ParamStore {
                param_slot: 1,
                value,
            },
            Type::UNIT,
        );
        // A counterfeit same-slot write in a disconnected block is not an
        // executable loop effect even if the caller includes it in `body`.
        push(
            &mut cfg,
            dead,
            CfgInstData::Store { slot: 0, value },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });
        cfg.set_terminator(dead, Terminator::Return { value: None });

        let reachable = super::super::dce::compute_reachable_blocks(&cfg);
        let mut workspace = LoopSlotFactsWorkspace::default();
        let (facts, work) =
            workspace.classify_loop_slot_invariance(&cfg, &[entry, dead], &reachable);
        assert_eq!(work.instructions_scanned, 4);
        assert_eq!(work.entries_initialized, 4);
        assert_eq!(work.workspace_growths, 1);
        assert!(facts.local_is_invariant(0));
        assert!(
            !facts.local_is_invariant(1),
            "projected write kills its root"
        );
        assert!(facts.param_is_invariant(0));
        assert!(!facts.param_is_invariant(1));
        assert!(
            !facts.local_is_invariant(2),
            "out-of-range local is counterfeit"
        );
        assert!(
            !facts.param_is_invariant(2),
            "out-of-range param is counterfeit"
        );

        let (facts, work) = workspace.classify_loop_slot_invariance(&cfg, &[entry], &reachable);
        assert_eq!(work.instructions_scanned, 4);
        assert_eq!(work.entries_initialized, 0);
        assert_eq!(work.workspace_growths, 0);
        assert!(facts.local_is_invariant(0));
        assert!(!facts.local_is_invariant(1));
    }

    #[test]
    fn address_taken_roots_never_qualify() {
        let mut cfg = cfg(2, 2);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(entry, Terminator::Return { value: None });
        cfg.mark_address_taken(0);
        cfg.mark_param_address_taken(0);

        let reachable = super::super::dce::compute_reachable_blocks(&cfg);
        let mut workspace = LoopSlotFactsWorkspace::default();
        let (facts, work) = workspace.classify_loop_slot_invariance(&cfg, &[entry], &reachable);
        assert_eq!(work.instructions_scanned, 0);
        assert_eq!(work.entries_initialized, 4);
        assert_eq!(work.workspace_growths, 1);
        assert!(!facts.local_is_invariant(0));
        assert!(facts.local_is_invariant(1));
        assert!(!facts.param_is_invariant(0));
        assert!(facts.param_is_invariant(1));
    }
}
