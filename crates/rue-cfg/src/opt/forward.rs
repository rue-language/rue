//! Value forwarding: store-to-load / copy propagation (RUE-914).
//!
//! Replaces `Load { slot }` results with an SSA value that already holds the
//! slot's contents, so later passes see the value directly instead of a memory
//! read. Runs at `-O2`/`-O3` only, after [`super::simplify`] and before
//! [`super::cse`] — forwarding turns memory reads into ordinary values, which is
//! exactly what feeds common-subexpression elimination (two `a + b`
//! expressions built from forwarded loads become keyable adds).
//!
//! Both rules are *trap-exact*: a `Load` never traps, and the value it is
//! replaced with was already computed earlier on every path that reaches the
//! load, so no computation is added, removed, or reordered — only a redundant
//! memory read is bypassed.
//!
//! A translated by-value parameter is the one exception to ordinary
//! single-write forwarding. Inlining marks the parameter's materialized
//! `Load` value; that ownership-consuming read stays a load, while later reads
//! may still forward through it so a returned value keeps the caller's owner.
//!
//! ## Rule 1 — global single-write forwarding
//!
//! Generalizes constant store-to-load propagation ([`super::constopt`]) from
//! constants to arbitrary SSA values. A local slot *qualifies* when the shared
//! classifier ([`super::slot_facts`], the single owner of the RUE-521
//! write/escape discipline) reports exactly ONE whole-slot write and no other
//! write channel — no projected `PlaceWrite`, no by-ref call argument rooting
//! the slot, not address-taken.
//!
//! For a qualifying slot every `Load` of it is replaced by the single write's
//! stored value. No dominator tree is needed to justify this: with exactly one
//! write site, sema's definite-initialization guarantee means every load
//! executes after the write on every path, so the write's block dominates every
//! load and the stored value — computed *before* the store — is available at
//! each load. This is the same dominance argument [`super::constopt`] makes for
//! constants, now stated for general values. Every *cross-block* forward is
//! verified explicitly against [`crate::dominators`] in all builds, not only
//! `debug_assertions` ones; same-block forwards are skipped, dominance being
//! reflexive.
//!
//! Note a subtlety the value form makes vacuous: the stored value could itself
//! be a `Load` of another slot that some later write modifies between the store
//! and a forwarded load. That would be a hazard if we *re-executed* the load,
//! but we forward the already-computed SSA id (the value as it was at store
//! time), never a recomputation, so there is nothing to invalidate.
//!
//! ## Rule 2 — block-local forwarding for multi-write slots
//!
//! A mutated slot (two or more writes) does not qualify for Rule 1, but within
//! a single block its most recent whole-slot store is still available. A
//! forward walk tracks `last_store[slot] = value` from each `Alloc`/`Store`; a
//! `Load { slot }` with a live entry forwards to that value. Entries are killed
//! when the stored value may no longer be current:
//!
//! - a later `Store`/`Alloc` to the slot replaces the entry;
//! - a `PlaceWrite` whose base is that local (a partial write) kills it;
//! - a `PlaceWrite` through a pointer or an accessor-yielded place, and an
//!   `Intrinsic` handed a pointer-typed argument, each clear the whole table:
//!   both write memory this pass cannot attribute to a slot.
//!
//! The table resets at each block boundary. Two kinds of slot are excluded
//! from it for the whole function rather than killed at a point:
//!
//! - **Address-taken slots** (`@raw`/`@raw_mut`/`@field_ptr`, recorded by CFG
//!   construction and carried across inlining): a raw pointer may alias them
//!   and store between a tracked write and a load (RUE-521 covers reads after
//!   an aliased write, not only place preservation).
//! - **Escaped slots**: any local rooting a by-ref (`inout`/`borrow`)
//!   argument of any call form. Handing a callee the slot's address is not a
//!   one-instruction event — the callee may return or stash the pointer, and a
//!   later write through it lands in the slot with no instruction here to
//!   attribute it to. Killing the entry only at the call left the *following*
//!   stores trackable, so a store, then a write through the escaped pointer,
//!   then a load forwarded the stale store (RUE-2262). A by-ref argument whose
//!   root this pass cannot resolve to a specific local excludes every slot,
//!   matching [`super::slot_facts`].
//!
//! Both sets are whole-function because block index order is not execution
//! order: a back edge can reach a block whose escaping call has not been
//! scanned yet.
//!
//! A `Load` that is itself a by-ref call argument is never forwarded under
//! either rule: the by-ref lowering requires that argument to remain a place.
//!
//! ## Slot identity is not value identity: the well-typedness guard
//!
//! A slot index does not identify a single typed storage location. A
//! zero-sized local reserves *no* frame slots, so its slot index is the same
//! index the next local receives, and the two unrelated locals then share one
//! `$n` in every slot-keyed table here (RUE-2086). Forwarding the value most
//! recently stored to `$n` into a `Load $n` that belongs to the *other* local
//! substitutes a value of the wrong type, and downstream consumers that read
//! an operand's type or materialized slot count off the CFG then plan the
//! wrong machine code — `@ptr_write` through a `ptr mut ()` emitted a real
//! eight-byte store because a `ptr mut` value had been forwarded into its
//! `()`-typed value operand.
//!
//! Neither rule forwards across a type change: a substitution is recorded only
//! when the stored value's type equals the `Load`'s own result type. That keeps
//! the pass's output well-typed by construction — the same invariant sema
//! established and every later consumer assumes — and it costs nothing on a
//! slot that really does hold one type, which is every slot a non-zero-sized
//! local owns.
//!
//! ## Owner roots: a moved value keeps the slot it moved into
//!
//! Forwarding substitutes values, and the CFG's ownership facts are keyed by
//! where a value came from. The verifier gives a whole-slot `Load` (or whole
//! `PlaceRead`) of a local the *owner root* of that slot, a block parameter
//! the root its incoming arguments agree on, and it treats a `Drop` of an
//! owned value as consuming that root until the next whole write of the slot.
//! A move `let t = b` is `v = load b; alloc t = v`, and `t`'s later drop is
//! `drop (load t)`, rooted at `t`. Forwarding `load t` to `v` re-roots that
//! drop at `b`. That is harmless while `b` cannot be written again before the
//! drop, but a mutated `b` can be reinitialized in between: `let t = b;
//! b = S { .. };` then drops `t` at scope end *after* `b`'s reinit, so the
//! forwarded drop consumes `b`'s fresh value in the ownership model, and in a
//! loop the next iteration's `load b` reads a root the verifier (correctly)
//! calls consumed (RUE-2380).
//!
//! So neither rule forwards a load of a value whose type needs drop to a
//! replacement rooted at a *different* local that is not [`SlotWrites::One`]
//! (the only local that can be reinitialized after the move is one with more
//! than one whole write), or at a writable parameter. A single-write root cannot be rewritten while a
//! value moved out of it is still live: its one write is its declaration, and
//! a loop re-executes that only after the local's storage ended. A parameter
//! root counts as reinitializable exactly when the parameter is writable: a
//! `mut self` receiver is owned, by value and reassignable, so `let t = self;
//! self = S { .. };` re-roots `t`'s drop exactly as a mutated local does. An
//! `inout` parameter is writable too but cannot be moved out of, so treating
//! it the same costs nothing, and any other parameter is never written. Each
//! accepted substitution keeps this property, so chains resolved below keep it
//! too.
//!
//! ## Applying substitutions and cleanup
//!
//! Both rules record `subst[load] = value`; all substitutions apply in one
//! [`Cfg::rewrite_value_uses_in_place`] sweep, resolving chains (a forwarded value may
//! itself be a forwarded load) exactly as [`super::cse`] and
//! [`super::peephole`] do. A forwarded `Load` then has no remaining uses.
//! Unlike CSE — which must overwrite a duplicated *trapping* op with a dead
//! placeholder because DCE deliberately preserves possibly-trapping arithmetic
//! (RUE-57) — loads never trap and are not on DCE's side-effect list, so DCE
//! sweeps the orphaned loads on its own. They are left as plain `Load`
//! instructions, not dummied out.

use crate::{BlockId, Cfg, CfgInstData, CfgValue, PlaceBase, Terminator};
use ahash::{AHashMap, AHashSet};
use rue_air::FrozenTypeInternPool;

use super::dce;
use super::slot_facts::{self, SlotWrites};

/// Work counters for one run (RUE-794 convention): one classification scan, one
/// forward rewriting scan, and one batched use-rewrite. No fixpoint loop.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Block-attached instructions visited by the forward rewriting walk.
    pub insts_scanned: u64,
    /// Loads forwarded by Rule 1 (global single-write slots).
    pub loads_forwarded_single_write: u64,
    /// Loads forwarded by Rule 2 (block-local last store of a multi-write slot).
    pub loads_forwarded_block_local: u64,
    /// Distinct cross-block (write, load) pairs verified against the dominator
    /// tree. Zero means no tree was built: every Rule 1 forward was same-block,
    /// where dominance is reflexive. Guards RUE-1844 — a regression that drops
    /// the same-block skip or the dedupe shows up as a non-zero count in
    /// `test_single_write_nonconst_forwarded`.
    pub rule1_dominance_pairs_checked: u64,
    /// Dominator trees computed by this pass. The tree is built only when at
    /// least one distinct cross-block Rule 1 pair needs proof.
    pub dominator_computations: u64,
    /// Loads a rule had a candidate value for but declined to forward because
    /// the stored value's type differs from the load's (RUE-2086 — a slot
    /// shared between a zero-sized local and the local that reuses its index).
    pub loads_declined_type_mismatch: u64,
    /// Loads of an owned value a rule had a candidate for but declined to
    /// forward because the candidate is rooted at a different local or a
    /// parameter that can be reinitialized (RUE-2380; module docs, "Owner
    /// roots").
    pub loads_declined_reinitializable_root: u64,
}

/// Whether `value`'s type is a raw pointer.
///
/// An intrinsic argument of pointer type is the channel through which a
/// pointer-writing intrinsic reaches storage this pass cannot name, so it is
/// what makes such an intrinsic a barrier for the block-local store table.
fn is_pointer_typed(cfg: &Cfg, value: CfgValue) -> bool {
    matches!(
        cfg.get_inst(value).ty.kind(),
        rue_air::TypeKind::PtrConst(_) | rue_air::TypeKind::PtrMut(_)
    )
}

/// Run value forwarding. Call at `-O2`/`-O3` after simplification and before
/// CSE. Ownership-boundary values are always preserved: they describe a
/// semantic transfer across an inline boundary, not an optional optimization
/// policy.
pub fn run(cfg: &mut Cfg, type_pool: &FrozenTypeInternPool) -> Result<Stats, crate::CfgEditError> {
    let mut stats = Stats::default();
    let num_locals = cfg.num_locals() as usize;

    // Both classification and rewriting are restricted to blocks reachable
    // AFTER simplify's constant-terminator folding. A load in a statically
    // dead arm never executes, is never dominated by anything, and must not
    // be forwarded (it tripped the Rule 1 dominance invariant — 2026-07-16
    // optimizer hunt); an unreachable store likewise must not count against
    // (or for) a slot's classification.
    let reachable = dce::compute_reachable_blocks(cfg);

    // ------------------------------------------------------------------
    // Precompute the set of values used as by-ref call arguments. Such a
    // value must stay a place (its address is taken by the callee), so it is
    // never forwarded regardless of rule.
    //
    // The same scan collects the slots whose address may have ESCAPED past
    // this function's view (RUE-2262). Handing a slot's address to a callee
    // is not a one-instruction event: the callee may keep the pointer (return
    // it, stash it in a struct) and any later write through it lands in the
    // slot without an instruction this pass can attribute to it. Killing the
    // slot's entry only at the call therefore leaves the following stores
    // trackable and forwardable, which is the miscompile. An escaped slot is
    // excluded from Rule 2's table for the whole function instead, exactly as
    // an address-taken one is, and the set is whole-function because block
    // order is not execution order: a back edge can reach a block whose
    // escaping call has not been scanned yet.
    // ------------------------------------------------------------------
    let mut byref_arg_values: AHashSet<CfgValue> = AHashSet::new();
    // Slots that may be written through a pointer the optimizer cannot see:
    // address-taken up front (RUE-521), plus every by-ref call-argument root.
    let mut untracked_slot: Vec<bool> = (0..num_locals as u32)
        .map(|slot| cfg.is_address_taken(slot))
        .collect();
    for block in cfg.blocks() {
        for &value in &block.insts {
            if let CfgInstData::Call { args, .. }
            | CfgInstData::AccessorCall { args, .. }
            | CfgInstData::CallIndirect { args, .. } = &cfg.get_inst(value).data
            {
                for arg in cfg.call_args(args) {
                    if !arg.is_by_ref() {
                        continue;
                    }
                    byref_arg_values.insert(arg.value);
                    match &cfg.get_inst(arg.value).data {
                        CfgInstData::Load { slot } => {
                            if let Some(flag) = untracked_slot.get_mut(*slot as usize) {
                                *flag = true;
                            }
                        }
                        CfgInstData::PlaceRead { place } => match place.base {
                            PlaceBase::Local(slot) => {
                                if let Some(flag) = untracked_slot.get_mut(slot as usize) {
                                    *flag = true;
                                }
                            }
                            // A parameter root hands over a parameter slot's
                            // address, which cannot alias a local slot.
                            PlaceBase::Param(_) => {}
                            PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => {
                                untracked_slot.fill(true);
                            }
                        },
                        // A bare scalar parameter has no backing local, so its
                        // ABI slot's address cannot alias one either. This is
                        // the mirror of the rule
                        // `slot_facts::classify_never_written_params` states
                        // for a local root handed to a callee.
                        CfgInstData::Param { .. } => {}
                        // A by-ref root this scan cannot resolve to a slot:
                        // assume every local's address may be the one handed
                        // over, matching `slot_facts`.
                        _ => untracked_slot.fill(true),
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Rule 1 classification: the shared RUE-521 discipline
    // (`super::slot_facts`), restricted to reachable blocks as the
    // dominance invariant requires. Address-taken slots never qualify (and
    // are excluded from the Rule 2 table below too). Nothing here depends
    // on later rewriting, so one scan suffices.
    // ------------------------------------------------------------------
    let slot_class = slot_facts::classify_slot_writes(cfg, Some(&reachable));
    let mut roots = OwnerRoots::default();

    // ------------------------------------------------------------------
    // Forward rewriting walk. One pass over every block-attached instruction
    // produces both rules' substitutions and maintains Rule 2's per-block
    // `last_store` table.
    // ------------------------------------------------------------------
    let mut subst: Vec<Option<CfgValue>> = vec![None; cfg.value_count()];
    // Distinct (single-write block, forwarded load block) pairs for the
    // dominance correctness check. Same-block pairs are never recorded:
    // dominance is reflexive, so they cannot fail. After simplify's block
    // merging most Rule 1 forwards are same-block, so this set is commonly
    // empty and the dominator tree below is never built.
    let mut rule1_dominance_checks: AHashSet<(BlockId, BlockId)> = AHashSet::new();
    // Rule 2 last whole-slot store per local, reset per block.
    let mut last_store: Vec<Option<CfgValue>> = vec![None; num_locals];

    for block_idx in 0..cfg.block_count() {
        let block_id = BlockId::from_raw(block_idx as u32);
        if !reachable.contains(block_idx as u32) {
            continue;
        }
        for slot in last_store.iter_mut() {
            *slot = None;
        }

        for i in 0..cfg.get_block(block_id).insts.len() {
            let value = cfg.get_block(block_id).insts[i];
            stats.insts_scanned += 1;

            match cfg.get_inst(value).data.duplicate_with_owner() {
                CfgInstData::Alloc { slot, init } | CfgInstData::Store { slot, value: init } => {
                    // Address-taken and escaped slots stay out of the
                    // block-local table; ownership-boundary Loads are filtered
                    // by value below.
                    if untracked_slot.get(slot as usize) == Some(&false) {
                        last_store[slot as usize] = Some(init);
                    }
                }
                CfgInstData::Load { slot } => {
                    // A by-ref argument load must remain a place.
                    if byref_arg_values.contains(&value) {
                        continue;
                    }
                    // Only the Load that crosses into a callee-owned
                    // parameter is protected. Other loads from that slot may
                    // still forward to the transfer value, which is how a
                    // returned value hands ownership back to the caller.
                    if cfg.is_ownership_boundary_value(value) {
                        continue;
                    }
                    let load_ty = cfg.get_inst(value).ty;
                    if let Some(SlotWrites::One {
                        value: write_value,
                        block: write_block,
                    }) = slot_class.get(slot as usize).copied()
                    {
                        // Rule 1: global single-write forwarding. The one
                        // write may belong to a different local that shares
                        // this slot index with a zero-sized one (RUE-2086);
                        // a type change is how that shows up.
                        if cfg.get_inst(write_value).ty != load_ty {
                            stats.loads_declined_type_mismatch += 1;
                            continue;
                        }
                        if roots.reinitializable_other_root(
                            cfg,
                            type_pool,
                            &slot_class,
                            &reachable,
                            slot,
                            write_value,
                        ) {
                            stats.loads_declined_reinitializable_root += 1;
                            continue;
                        }
                        subst[value.as_u32() as usize] = Some(write_value);
                        stats.loads_forwarded_single_write += 1;
                        if write_block != block_id {
                            rule1_dominance_checks.insert((write_block, block_id));
                        }
                    } else if untracked_slot.get(slot as usize) == Some(&false) {
                        // Rule 2: block-local forwarding for multi-write slots.
                        if let Some(&Some(stored)) = last_store.get(slot as usize) {
                            // A zero-sized local shares its slot index with the
                            // local that reuses it, so the tracked store may
                            // belong to a different location entirely
                            // (RUE-2086). Only a same-typed value is this
                            // load's.
                            if cfg.get_inst(stored).ty != load_ty {
                                stats.loads_declined_type_mismatch += 1;
                                continue;
                            }
                            if roots.reinitializable_other_root(
                                cfg,
                                type_pool,
                                &slot_class,
                                &reachable,
                                slot,
                                stored,
                            ) {
                                stats.loads_declined_reinitializable_root += 1;
                                continue;
                            }
                            subst[value.as_u32() as usize] = Some(stored);
                            stats.loads_forwarded_block_local += 1;
                        }
                    }
                }
                CfgInstData::PlaceWrite { place, .. } => match place.base {
                    // A partial write invalidates the whole-slot value.
                    PlaceBase::Local(slot) => {
                        if (slot as usize) < num_locals {
                            last_store[slot as usize] = None;
                        }
                    }
                    // A projected write into a parameter's storage cannot
                    // reach a local slot.
                    PlaceBase::Param(_) => {}
                    // A write through a pointer or an accessor-yielded place:
                    // the target cannot be bounded here, so it is a barrier
                    // for the whole table, the same rule
                    // `slot_facts::classify_loop_slot_invariance` applies per
                    // loop.
                    PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => {
                        last_store.fill(None);
                    }
                },
                // An intrinsic handed a POINTER can store through it
                // (`@ptr_write`, `@byte_copy`, `@byte_set`, …), writing memory
                // this pass cannot attribute to a slot, so it is a barrier for
                // the whole table. One that receives no pointer — `@dbg`,
                // `@size_of`, the arithmetic conversions — writes nothing
                // reachable from a local and is not a barrier; making every
                // intrinsic one costs block-local forwarding in every loop
                // that prints.
                //
                // Calls and drops need no arm at all. The only way a callee
                // body can write a caller local is through the local's
                // address, and both ways to produce one (an address-taking
                // intrinsic here, a by-ref argument) already put the slot in
                // `untracked_slot`. This arm is the backstop that keeps the
                // pass sound if an escape channel is ever added that the scan
                // above does not model (RUE-2262).
                CfgInstData::Intrinsic { args, .. }
                    if cfg
                        .intrinsic_args(&args)
                        .iter()
                        .any(|&arg| is_pointer_typed(cfg, arg)) =>
                {
                    last_store.fill(None);
                }
                _ => {}
            }
        }
    }

    let forwarded = stats.loads_forwarded_single_write + stats.loads_forwarded_block_local;
    if forwarded == 0 {
        return Ok(stats);
    }

    // Turn the definite-initialization argument for Rule 1 into an always-on
    // invariant: violating it would make the substitution below silently use a
    // value before its definition in release builds.
    stats.rule1_dominance_pairs_checked = rule1_dominance_checks.len() as u64;
    if !rule1_dominance_checks.is_empty() {
        stats.dominator_computations += 1;
        let dom = crate::dominators::DominatorTree::compute(cfg);
        for (write_block, load_block) in &rule1_dominance_checks {
            assert!(
                dom.dominates(*write_block, *load_block),
                "Rule 1 forwarded a load in {load_block} whose single write \
                 block {write_block} does not dominate it",
            );
        }
    }

    // Resolve chains once, then re-point every use in a single sweep.
    let resolved: Vec<CfgValue> = (0..cfg.value_count())
        .map(|i| resolve(&subst, CfgValue::from_raw(i as u32)))
        .collect();
    // The optimizer owns a private editor and discards it if this edit fails;
    // an in-place sweep avoids cloning that complete editor. The editor may be
    // poisoned on error, which is safe under optimize_with_budget's publish
    // boundary.
    cfg.rewrite_value_uses_in_place(|v| resolved[v.as_u32() as usize])?;

    Ok(stats)
}

/// Owner-root queries for the "Owner roots" rule in the module docs.
///
/// The block-parameter incoming table is built on the first query that meets
/// a block parameter, so a function without owned forwards through a phi pays
/// nothing for it.
#[derive(Default)]
struct OwnerRoots {
    incoming: Option<AHashMap<CfgValue, Vec<CfgValue>>>,
}

impl OwnerRoots {
    /// Whether forwarding a `Load` of `load_slot` to `candidate` would re-root
    /// an owned value at a different local, or a parameter, that can be
    /// written again.
    ///
    /// The candidate's roots mirror the verifier's provenance: a whole-slot
    /// `Load` or whole-local `PlaceRead` roots at that local, a `Param` or
    /// whole-parameter `PlaceRead` at that parameter, and a block parameter at
    /// every root among its incoming arguments (transitively).
    /// Any other value has no root. Taking every incoming root, rather
    /// than only roots all arguments agree on, is the conservative side.
    fn reinitializable_other_root(
        &mut self,
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        slot_class: &[SlotWrites],
        reachable: &dce::BitSet,
        load_slot: u32,
        candidate: CfgValue,
    ) -> bool {
        if !super::classify::materializes_owned_value(cfg, type_pool, candidate) {
            return false;
        }
        let reinitializable = |slot: u32| {
            slot != load_slot
                && !matches!(slot_class.get(slot as usize), Some(SlotWrites::One { .. }))
        };
        let mut visited = AHashSet::new();
        let mut stack = vec![candidate];
        while let Some(value) = stack.pop() {
            if !visited.insert(value) {
                continue;
            }
            match &cfg.get_inst(value).data {
                CfgInstData::Load { slot } => {
                    if reinitializable(*slot) {
                        return true;
                    }
                }
                CfgInstData::PlaceRead { place } => {
                    if let Some(slot) = place.as_local()
                        && reinitializable(slot)
                    {
                        return true;
                    }
                    if let Some(param) = place.as_param()
                        && cfg.is_param_writable(param)
                    {
                        return true;
                    }
                }
                CfgInstData::Param { index } => {
                    if cfg.is_param_writable(*index) {
                        return true;
                    }
                }
                CfgInstData::BlockParam { .. } => {
                    let incoming = self
                        .incoming
                        .get_or_insert_with(|| block_param_incoming(cfg, reachable));
                    if let Some(args) = incoming.get(&value) {
                        stack.extend(args.iter().copied());
                    }
                }
                _ => {}
            }
        }
        false
    }
}

/// Every reachable edge argument, keyed by the block parameter it binds.
fn block_param_incoming(cfg: &Cfg, reachable: &dce::BitSet) -> AHashMap<CfgValue, Vec<CfgValue>> {
    let mut incoming: AHashMap<CfgValue, Vec<CfgValue>> = AHashMap::new();
    let mut bind = |target: BlockId, args: &[CfgValue]| {
        for (&(param, _), &arg) in cfg.get_block(target).params.iter().zip(args) {
            incoming.entry(param).or_default().push(arg);
        }
    };
    for block in cfg.blocks() {
        if !reachable.contains(block.id.as_u32()) {
            continue;
        }
        let terminator = &block.terminator;
        match terminator {
            Terminator::Goto { target, .. } => bind(*target, cfg.get_goto_args(terminator)),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                bind(*then_block, cfg.get_branch_then_args(terminator));
                bind(*else_block, cfg.get_branch_else_args(terminator));
            }
            _ => {}
        }
    }
    incoming
}

/// Walk `subst` chains to the surviving value. A forwarded value can itself be
/// a forwarded load (`let a = 5; let b = a;`), so resolution is iterative;
/// chains are acyclic because a stored value is always defined before its store.
fn resolve(subst: &[Option<CfgValue>], mut v: CfgValue) -> CfgValue {
    while let Some(next) = subst[v.as_u32() as usize] {
        v = next;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgArgMode, CfgCallArg, CfgInst, Place, Projection, Terminator, Type};
    use lasso::{Key, Spur};
    use rue_air::{StructDef, StructId, TypeInternPool};
    use rue_span::Span;

    fn test_struct_id() -> StructId {
        TypeInternPool::new()
            .register_struct(
                Spur::try_from_usize(0).unwrap(),
                StructDef {
                    name: "Test".into(),
                    fields: vec![],
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                    is_builtin: false,
                    is_pub: false,
                    file_id: rue_span::FileId::DEFAULT,
                },
            )
            .0
    }

    fn run(cfg: &mut Cfg) -> Result<Stats, crate::CfgEditError> {
        super::run(cfg, &TypeInternPool::new().freeze())
    }

    fn make_cfg(num_locals: u32) -> Cfg {
        let mut cfg = Cfg::new(Type::I32, num_locals, 0, "test".to_string(), vec![]);
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
    fn test_single_write_nonconst_forwarded() {
        // let s = a + b; s  — the slot has one write (its Alloc) initializing
        // from a non-constant Add. Every Load of s forwards to that Add value.
        let mut cfg = make_cfg(1);
        let a = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let b = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(a, b), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: sum },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 1);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        // RUE-1844: the write and the load share a block, so dominance is
        // reflexive and no pair is recorded — no dominator tree is built.
        assert_eq!(stats.rule1_dominance_pairs_checked, 0);
        assert_eq!(stats.dominator_computations, 0);
        // The return now reads the Add directly.
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == sum
        ));
    }

    #[test]
    fn test_block_local_multiwrite_forwarded_with_kill_on_store() {
        // let mut x = 1; read1 = x; x = 2; read2 = x;
        // Two writes => not a Rule 1 slot; block-local forwarding sends read1
        // to the first value and read2 to the second (the store kills+replaces).
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c1 },
            Type::UNIT,
        );
        let read1 = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let c2 = push(&mut cfg, CfgInstData::Const(2), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Store { slot: 0, value: c2 },
            Type::UNIT,
        );
        let read2 = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(read1, read2), Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(sum) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 2);
        // read1 -> c1, read2 -> c2.
        assert!(matches!(cfg.get_inst(sum).data, CfgInstData::Add(x, y) if x == c1 && y == c2));
    }

    #[test]
    fn test_byref_call_kills_block_local_entry() {
        // let mut x = 1; f(inout x); read = x;
        // The store before the call is killed by the by-ref argument, so the
        // load after the call is NOT forwarded (the callee may have written x).
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c1 },
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
        let read = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(read) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        // Neither load was rewritten: the arg load stays a place, and the load
        // after the call still reads memory.
        assert!(matches!(
            cfg.get_inst(arg_load).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == read
        ));
    }

    #[test]
    fn test_byref_accessor_call_keeps_argument_load_as_place() {
        // AccessorCall has the same by-ref escape semantics as Call: its
        // argument load must remain a place, and the load after the accessor
        // cannot use the value stored before it.
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(1), Type::I32);
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
        let accessor = push(
            &mut cfg,
            CfgInstData::AccessorCall {
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        let read = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(read) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert!(matches!(
            cfg.get_inst(arg_load).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == read
        ));
        assert!(matches!(
            cfg.get_inst(accessor).data,
            CfgInstData::AccessorCall { .. }
        ));
    }

    #[test]
    fn test_escaped_slot_stays_untracked_after_the_call() {
        // let mut x = 1; f(inout x); x = 5; @ptr_write(p, 42); read = x;
        // RUE-2262: the by-ref argument hands `f` the slot's ADDRESS, which it
        // may keep. Killing the entry only AT the call let the FOLLOWING store
        // re-enter the table, and the read after the pointer write forwarded
        // that stale 5. An escaped slot is excluded for the whole function.
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c1 },
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
        let c5 = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Store { slot: 0, value: c5 },
            Type::UNIT,
        );
        let read = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(read) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == read
        ));
    }

    /// Build `let mut x = 1; x = 5; <intrinsic over `arg`>; read = x;` and
    /// return the forwarding stats plus the load the return reads.
    fn intrinsic_barrier_fixture(pointer_typed_argument: bool) -> (Stats, CfgValue, CfgValue) {
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c1 },
            Type::UNIT,
        );
        let c5 = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Store { slot: 0, value: c5 },
            Type::UNIT,
        );
        let pool = TypeInternPool::new();
        let argument_type = if pointer_typed_argument {
            Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32))
        } else {
            Type::I32
        };
        let argument = push(&mut cfg, CfgInstData::Const(0), argument_type);
        let intrinsic_args = cfg.push_intrinsic_args([argument]).unwrap();
        push(
            &mut cfg,
            CfgInstData::Intrinsic {
                operation: rue_air::IntrinsicOperation::DebugI64,
                name: Spur::try_from_usize(0).unwrap(),
                args: intrinsic_args,
            },
            Type::UNIT,
        );
        let read = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(read) });

        let stats = run(&mut cfg).unwrap();
        let returned = match cfg.get_block(cfg.entry).terminator {
            Terminator::Return { value: Some(v) } => v,
            _ => unreachable!("fixture returns a value"),
        };
        (stats, read, returned)
    }

    #[test]
    fn test_pointer_taking_intrinsic_kills_block_local_entries() {
        // An intrinsic handed a pointer may store through it, so it is a
        // barrier for the whole table even though this slot never escaped
        // (the RUE-2262 backstop).
        let (stats, read, returned) = intrinsic_barrier_fixture(true);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert_eq!(returned, read);
    }

    #[test]
    fn test_intrinsic_without_a_pointer_argument_is_not_a_barrier() {
        // `@dbg` and friends receive no pointer, so they cannot reach a local
        // and must not cost the block its tracked store — an unconditional
        // intrinsic barrier stopped forwarding in every loop that prints.
        let (stats, read, returned) = intrinsic_barrier_fixture(false);
        assert_eq!(stats.loads_forwarded_block_local, 1);
        assert_ne!(returned, read);
    }

    #[test]
    fn test_indirect_place_write_kills_block_local_entries() {
        // A `PlaceWrite` through a pointer base writes storage this pass
        // cannot attribute to a slot, so it clears the whole table.
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c1 },
            Type::UNIT,
        );
        let c5 = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Store { slot: 0, value: c5 },
            Type::UNIT,
        );
        let pointer = push(&mut cfg, CfgInstData::Const(0), Type::I64);
        let projections = cfg.push_projections([]).unwrap();
        let place = Place {
            base: PlaceBase::Indirect(pointer),
            base_type: Type::I32,
            projections,
        };
        push(
            &mut cfg,
            CfgInstData::PlaceWrite { place, value: c1 },
            Type::UNIT,
        );
        let read = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(read) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == read
        ));
    }

    #[test]
    fn test_byref_accessor_call_kills_last_store() {
        // With multiple writes, the accessor must clear Rule 2's last-store
        // entry just like an ordinary call does.
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
        let arg_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg
            .push_call_args([CfgCallArg {
                value: arg_load,
                mode: CfgArgMode::Borrow,
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
        let read = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(read) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert!(matches!(
            cfg.get_inst(arg_load).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == read
        ));
    }

    #[test]
    fn test_normal_accessor_call_arg_still_forwards() {
        // A normal accessor argument only reads the slot, so its load retains
        // the existing single-write forwarding optimization.
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
        let accessor = push(
            &mut cfg,
            CfgInstData::AccessorCall {
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::I32,
        );
        cfg.set_terminator(
            cfg.entry,
            Terminator::Return {
                value: Some(accessor),
            },
        );

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 1);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        let accessor_arg = match &cfg.get_inst(accessor).data {
            CfgInstData::AccessorCall { args, .. } => cfg.call_args(args)[0].value,
            other => panic!("expected accessor call, got {other:?}"),
        };
        assert_eq!(accessor_arg, c);
    }

    #[test]
    fn test_address_taken_slot_never_forwarded() {
        // A single-write slot whose address is taken must keep its loads as
        // places (RUE-521): neither rule may forward.
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(42), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg.push_intrinsic_args(vec![load]).unwrap();
        let ptr = push(
            &mut cfg,
            CfgInstData::Intrinsic {
                operation: rue_air::IntrinsicOperation::Raw,
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::I32,
        );
        cfg.mark_address_taken(0);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(ptr) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_ownership_boundary_slot_never_forwarded() {
        // Inlining moves the caller value into a fresh callee-owned slot. The
        // slot has one Alloc, but replacing its Load with the Alloc initializer
        // would erase that ownership transfer and can double-drop the caller's
        // value during post-splice reoptimization.
        let mut cfg = make_cfg(1);
        let value = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: value,
            },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.mark_ownership_boundary(0);
        cfg.mark_ownership_boundary_value(load);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_placewrite_kills_block_local_entry() {
        // let mut x = 1; x.field = ...; read = x;
        // A projected write invalidates the tracked whole-slot value, and it is
        // a second write so Rule 1 is out too. The load is not forwarded.
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c1 },
            Type::UNIT,
        );
        let field_val = push(&mut cfg, CfgInstData::Const(9), Type::I32);
        let projections = cfg
            .push_projections([Projection::Field {
                struct_id: test_struct_id(),
                field_index: 0,
            }])
            .unwrap();
        let place = Place {
            base: PlaceBase::Local(0),
            base_type: Type::I32,
            projections,
        };
        push(
            &mut cfg,
            CfgInstData::PlaceWrite {
                place,
                value: field_val,
            },
            Type::UNIT,
        );
        let read = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(read) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == read
        ));
    }

    #[test]
    fn test_zero_sized_out_of_range_slot_ignored() {
        // A trailing zero-sized local is assigned slot index == num_locals,
        // out of range for the slot tables. Its Alloc/Load must be skipped, not
        // panic (RUE-194), and nothing is forwarded.
        let mut cfg = make_cfg(0);
        let c = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::UNIT);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: None });
        let _ = load;

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_cross_block_multiwrite_not_forwarded() {
        // A multi-write slot's store in block A does not forward into a load in
        // block B: Rule 2 is block-local, and the slot is disqualified for Rule
        // 1. (Store in entry; load in a successor block.)
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
        let block2 = cfg.new_block();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Goto {
                target: block2,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        let load = push_in(&mut cfg, block2, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(block2, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert!(matches!(
            cfg.get_block(block2).terminator,
            Terminator::Return { value: Some(v) } if v == load
        ));
    }

    #[test]
    fn test_single_write_forwards_across_blocks() {
        // A single-write slot forwards even into a different block: the write's
        // block dominates the load's block, so Rule 1 applies. (This also
        // exercises the always-on cross-block dominance assertion.)
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(7), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let block2 = cfg.new_block();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Goto {
                target: block2,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        let load = push_in(&mut cfg, block2, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(block2, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 1);
        // RUE-1844: this pair really is cross-block, so it is recorded and
        // verified against a dominator tree.
        assert_eq!(stats.rule1_dominance_pairs_checked, 1);
        assert_eq!(stats.dominator_computations, 1);
        assert!(matches!(
            cfg.get_block(block2).terminator,
            Terminator::Return { value: Some(v) } if v == c
        ));
    }

    #[test]
    fn test_work_counters_one_scan() {
        // insts_scanned counts every block-attached instruction; a single run
        // suffices (a second finds nothing new).
        let mut cfg = make_cfg(1);
        let a = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        let b = push(&mut cfg, CfgInstData::Param { index: 1 }, Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(a, b), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: sum },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let total_insts: usize = (0..cfg.block_count())
            .map(|i| cfg.get_block(BlockId::from_raw(i as u32)).insts.len())
            .sum();

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.insts_scanned, total_insts as u64);
        assert_eq!(stats.loads_forwarded_single_write, 1);

        // Forwarding leaves the orphaned load in place for DCE (unlike CSE,
        // which dummies its duplicates). A second run WITHOUT DCE therefore
        // re-forwards the still-present load — the pass is idempotent in effect
        // (the load already has no uses) but not in its work counter. Once DCE
        // sweeps the load, a re-run finds nothing.
        super::super::dce::run(&mut cfg);
        let again = run(&mut cfg).unwrap();
        assert_eq!(again.loads_forwarded_single_write, 0);
        assert_eq!(again.loads_forwarded_block_local, 0);
    }
    #[test]
    fn test_load_in_unreachable_block_not_forwarded() {
        // A constant-folded-away arm can still hold a Load of a single-write
        // slot. That load never executes and is dominated by nothing; it must
        // not be forwarded, and must not trip the Rule 1 dominance invariant
        // (2026-07-16 optimizer-hunt ICE at forward.rs debug_assert).
        let mut cfg = Cfg::new(Type::I32, 1, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let init = push_in(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::I32);
        push_in(
            &mut cfg,
            entry,
            CfgInstData::Alloc { slot: 0, init },
            Type::UNIT,
        );
        let dead = cfg.new_block();
        let live = cfg.new_block();
        // entry jumps straight to the live block (as after simplify folded a
        // constant-false branch); the dead block still holds a load.
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target: live,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        let dead_load = push_in(&mut cfg, dead, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(
            dead,
            Terminator::Return {
                value: Some(dead_load),
            },
        );
        let live_load = push_in(&mut cfg, live, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(
            live,
            Terminator::Return {
                value: Some(live_load),
            },
        );

        let stats = run(&mut cfg).unwrap();
        // The reachable load forwards; the unreachable one is untouched.
        assert_eq!(stats.loads_forwarded_single_write, 1);
        assert!(matches!(
            cfg.get_inst(dead_load).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_block(live).terminator,
            Terminator::Return { value: Some(v) } if v == init
        ));
    }

    /// RUE-2086: a zero-sized local reserves no frame slots, so its slot index
    /// is the one the next local receives and the two share `$0`. The
    /// block-local table then holds the *pointer* the second local stored when
    /// the first local's `()`-typed load is reached, and forwarding it made
    /// `@ptr_write` store eight bytes through a zero-sized sentinel address.
    #[test]
    fn test_block_local_declines_forward_across_a_type_change() {
        // `let e: () = (); let pm: ptr mut () = @raw_mut(..); @ptr_write(pm, e)`
        // — both locals are slot 0, so the `()` load must not see the pointer.
        let mut cfg = make_cfg(1);
        let unit = push(&mut cfg, CfgInstData::Const(0), Type::UNIT);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: unit,
            },
            Type::UNIT,
        );
        let pointer = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I64);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: pointer,
            },
            Type::UNIT,
        );
        let pointer_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I64);
        let unit_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::UNIT);
        let sum = push(
            &mut cfg,
            CfgInstData::Add(pointer_load, unit_load),
            Type::I64,
        );
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(sum) });

        let stats = run(&mut cfg).unwrap();
        // The same-typed load still forwards; the `()` load does not.
        assert_eq!(stats.loads_forwarded_block_local, 1);
        assert_eq!(stats.loads_declined_type_mismatch, 1);
        assert!(
            matches!(cfg.get_inst(sum).data, CfgInstData::Add(x, y) if x == pointer && y == unit_load)
        );
        assert!(matches!(
            cfg.get_inst(unit_load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    /// The same guard on Rule 1: one whole-slot write is still not this load's
    /// write when a zero-sized local shares the slot index (RUE-2086).
    #[test]
    fn test_single_write_declines_forward_across_a_type_change() {
        let mut cfg = make_cfg(1);
        let init = push(&mut cfg, CfgInstData::Const(7), Type::I64);
        push(&mut cfg, CfgInstData::Alloc { slot: 0, init }, Type::UNIT);
        let unit_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::UNIT);
        cfg.set_terminator(
            cfg.entry,
            Terminator::Return {
                value: Some(unit_load),
            },
        );

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_declined_type_mismatch, 1);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == unit_load
        ));
    }
    /// The other direction of the same aliasing: the `()` is stored *last* and
    /// a sized load of the shared slot follows.
    ///
    /// Reaching this from source needs the zero-sized local to be re-assigned
    /// after the sized one is initialized (`let mut e: () = (); let y: i64 = 5;
    /// e = ();`), because slot indices only ever advance, so the zero-sized
    /// local's `Alloc` always comes first. That shape is miscompiled the other
    /// way round — the `()` reaches a sized consumer and makes its materialized
    /// slot count zero, so a store or an argument disappears rather than
    /// appearing. The source form is covered by the `cli.zero_sized_slot_sharing`
    /// re-assignment cases (it became oracle-gateable once RUE-2095 keyed the
    /// reference interpreter's own local store by `(slot, Type)`); this test
    /// drives the CFG shape directly, without depending on which consumer
    /// happens to observe the ill-typed operand.
    #[test]
    fn test_block_local_declines_forwarding_a_zero_sized_store_to_a_sized_load() {
        let mut cfg = make_cfg(1);
        let sized = push(&mut cfg, CfgInstData::Const(5), Type::I64);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: sized,
            },
            Type::UNIT,
        );
        // The zero-sized local's re-assignment lands on the same slot.
        let unit = push(&mut cfg, CfgInstData::Const(0), Type::UNIT);
        push(
            &mut cfg,
            CfgInstData::Store {
                slot: 0,
                value: unit,
            },
            Type::UNIT,
        );
        let sized_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I64);
        cfg.set_terminator(
            cfg.entry,
            Terminator::Return {
                value: Some(sized_load),
            },
        );

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_forwarded_block_local, 0);
        assert_eq!(stats.loads_declined_type_mismatch, 1);
        // The sized read stays a load; forwarding the `()` would have made the
        // consumer see a value with no ABI slots.
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == sized_load
        ));
    }

    /// A pool holding one struct with a destructor and one without.
    fn owning_and_plain_struct_pool() -> (FrozenTypeInternPool, Type, Type) {
        let interner = lasso::ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let register = |name: &str, destructor: Option<&str>| {
            Type::new_struct(
                pool.register_struct(
                    interner.get_or_intern(name),
                    StructDef {
                        name: name.into(),
                        fields: Vec::new(),
                        is_copy: false,
                        is_linear: false,
                        declared_linear: false,
                        destructor: destructor.map(Into::into),
                        is_builtin: false,
                        is_pub: false,
                        file_id: rue_span::FileId::DEFAULT,
                    },
                )
                .0,
            )
        };
        let owning = register("Owning", Some("Owning.__drop"));
        let plain = register("Plain", None);
        let pool = pool.freeze();
        assert!(pool.type_needs_drop(owning));
        assert!(!pool.type_needs_drop(plain));
        (pool, owning, plain)
    }

    /// `let mut b = p0; let t = b; [b = p1;] drop t`, with `b` in slot 0 and
    /// `t` in slot 1. Returns the CFG and `t`'s dropped load.
    fn move_out_then_maybe_reinit(ty: Type, reinit: bool) -> (Cfg, CfgValue) {
        let mut cfg = Cfg::new(Type::UNIT, 2, 2, "test".to_string(), vec![false, false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let p0 = push(&mut cfg, CfgInstData::Param { index: 0 }, ty);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: p0 },
            Type::UNIT,
        );
        let moved = push(&mut cfg, CfgInstData::Load { slot: 0 }, ty);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 1,
                init: moved,
            },
            Type::UNIT,
        );
        if reinit {
            let p1 = push(&mut cfg, CfgInstData::Param { index: 1 }, ty);
            push(
                &mut cfg,
                CfgInstData::Store { slot: 0, value: p1 },
                Type::UNIT,
            );
        }
        let dropped = push(&mut cfg, CfgInstData::Load { slot: 1 }, ty);
        push(&mut cfg, CfgInstData::Drop { value: dropped }, Type::UNIT);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: None });
        (cfg, dropped)
    }

    fn drop_operand(cfg: &Cfg) -> CfgValue {
        cfg.get_block(cfg.entry)
            .insts
            .iter()
            .find_map(|&value| match cfg.get_inst(value).data {
                CfgInstData::Drop { value } => Some(value),
                _ => None,
            })
            .expect("the drop survives forwarding")
    }

    #[test]
    fn test_owned_move_not_rerooted_at_reinitialized_local() {
        // RUE-2380: `let t = b; b = ..; drop t`. Forwarding `load t` to the
        // move-out `load b` would make the drop consume `b` after its reinit,
        // so in a loop the next iteration's `load b` reads a consumed root.
        let (pool, owning, _) = owning_and_plain_struct_pool();
        let (mut cfg, dropped) = move_out_then_maybe_reinit(owning, true);
        let stats = super::run(&mut cfg, &pool).unwrap();
        assert_eq!(stats.loads_declined_reinitializable_root, 1);
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(drop_operand(&cfg), dropped);
    }

    #[test]
    fn test_owned_move_forwarded_when_source_single_write() {
        // Without the reinit `b` has one write and cannot be rewritten while
        // `t` is live: the drop may take the moved value directly.
        let (pool, owning, _) = owning_and_plain_struct_pool();
        let (mut cfg, _) = move_out_then_maybe_reinit(owning, false);
        let stats = super::run(&mut cfg, &pool).unwrap();
        assert_eq!(stats.loads_declined_reinitializable_root, 0);
        // `load t` forwards to the move-out `load b`, which in turn forwards
        // to `b`'s single write, the parameter.
        assert!(matches!(
            cfg.get_inst(drop_operand(&cfg)).data,
            CfgInstData::Param { index: 0 }
        ));
    }

    #[test]
    fn test_plain_move_forwarded_across_reinit() {
        // A value without drop glue carries no ownership fact to re-root.
        let (pool, _, plain) = owning_and_plain_struct_pool();
        let (mut cfg, dropped) = move_out_then_maybe_reinit(plain, true);
        let stats = super::run(&mut cfg, &pool).unwrap();
        assert_eq!(stats.loads_declined_reinitializable_root, 0);
        assert_eq!(stats.loads_forwarded_single_write, 1);
        assert_ne!(drop_operand(&cfg), dropped);
    }

    #[test]
    fn test_owned_move_through_block_param_not_rerooted() {
        // `let t = if c { b } else { b }; b = ..; drop t`: the phi's incoming
        // arguments both root at the reinitialized `b`.
        let (pool, owning, _) = owning_and_plain_struct_pool();
        let mut cfg = Cfg::new(Type::UNIT, 2, 3, "test".to_string(), vec![false; 3]);
        let entry = cfg.new_block();
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let join = cfg.new_block();
        cfg.entry = entry;
        let p0 = push_in(&mut cfg, entry, CfgInstData::Param { index: 0 }, owning);
        push_in(
            &mut cfg,
            entry,
            CfgInstData::Alloc { slot: 0, init: p0 },
            Type::UNIT,
        );
        let cond = push_in(&mut cfg, entry, CfgInstData::Param { index: 2 }, Type::BOOL);
        cfg.set_branch(entry, cond, then_block, [], else_block, []);
        let phi = cfg.add_block_param(join, owning);
        for arm in [then_block, else_block] {
            let moved = push_in(&mut cfg, arm, CfgInstData::Load { slot: 0 }, owning);
            cfg.set_goto(arm, join, [moved]);
        }
        push_in(
            &mut cfg,
            join,
            CfgInstData::Alloc { slot: 1, init: phi },
            Type::UNIT,
        );
        let p1 = push_in(&mut cfg, join, CfgInstData::Param { index: 1 }, owning);
        push_in(
            &mut cfg,
            join,
            CfgInstData::Store { slot: 0, value: p1 },
            Type::UNIT,
        );
        let dropped = push_in(&mut cfg, join, CfgInstData::Load { slot: 1 }, owning);
        push_in(
            &mut cfg,
            join,
            CfgInstData::Drop { value: dropped },
            Type::UNIT,
        );
        cfg.set_terminator(join, Terminator::Return { value: None });

        let stats = super::run(&mut cfg, &pool).unwrap();
        assert_eq!(stats.loads_declined_reinitializable_root, 1);
        let drop_operand = cfg
            .get_block(join)
            .insts
            .iter()
            .find_map(|&value| match cfg.get_inst(value).data {
                CfgInstData::Drop { value } => Some(value),
                _ => None,
            })
            .unwrap();
        assert_eq!(drop_operand, dropped);
    }

    #[test]
    fn test_owned_move_not_rerooted_at_reassigned_mut_self() {
        // `fn f(mut self) { let t = self; self = ..; drop t }`: a `mut self`
        // receiver is a by-value, writable parameter the verifier roots as
        // `WritableParam`, so the reassignment reinitializes it exactly as a
        // store reinitializes a mutated local.
        let (pool, owning, _) = owning_and_plain_struct_pool();
        let mut cfg = Cfg::new(
            Type::UNIT,
            1,
            2,
            "test".to_string(),
            rue_air::ParamSlotModes::new(vec![false, false], vec![true, false]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let receiver = push(&mut cfg, CfgInstData::Param { index: 0 }, owning);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: receiver,
            },
            Type::UNIT,
        );
        let fresh = push(&mut cfg, CfgInstData::Param { index: 1 }, owning);
        push(
            &mut cfg,
            CfgInstData::ParamStore {
                param_slot: 0,
                value: fresh,
            },
            Type::UNIT,
        );
        let dropped = push(&mut cfg, CfgInstData::Load { slot: 0 }, owning);
        push(&mut cfg, CfgInstData::Drop { value: dropped }, Type::UNIT);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: None });

        let stats = super::run(&mut cfg, &pool).unwrap();
        assert_eq!(stats.loads_declined_reinitializable_root, 1);
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(drop_operand(&cfg), dropped);
    }

    #[test]
    fn test_owned_move_not_rerooted_by_block_local_rule() {
        // Rule 2: `let mut t = ..; t = b; b = ..; drop t` in one block, both
        // locals mutated. `t`'s last store is the move-out `load b`, rooted
        // at the reinitialized `b`, so `load t` stays.
        let (pool, owning, _) = owning_and_plain_struct_pool();
        let mut cfg = Cfg::new(Type::UNIT, 2, 3, "test".to_string(), vec![false; 3]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let first = push(&mut cfg, CfgInstData::Param { index: 0 }, owning);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: first,
            },
            Type::UNIT,
        );
        let placeholder = push(&mut cfg, CfgInstData::Param { index: 2 }, owning);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 1,
                init: placeholder,
            },
            Type::UNIT,
        );
        let moved = push(&mut cfg, CfgInstData::Load { slot: 0 }, owning);
        push(
            &mut cfg,
            CfgInstData::Store {
                slot: 1,
                value: moved,
            },
            Type::UNIT,
        );
        let fresh = push(&mut cfg, CfgInstData::Param { index: 1 }, owning);
        push(
            &mut cfg,
            CfgInstData::Store {
                slot: 0,
                value: fresh,
            },
            Type::UNIT,
        );
        let dropped = push(&mut cfg, CfgInstData::Load { slot: 1 }, owning);
        push(&mut cfg, CfgInstData::Drop { value: dropped }, Type::UNIT);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: None });

        let stats = super::run(&mut cfg, &pool).unwrap();
        // The move-out `load b` itself still forwards to `b`'s first write,
        // an immutable parameter.
        assert_eq!(stats.loads_forwarded_block_local, 1);
        assert_eq!(stats.loads_forwarded_single_write, 0);
        assert_eq!(stats.loads_declined_reinitializable_root, 1);
        assert_eq!(drop_operand(&cfg), dropped);
    }
}
