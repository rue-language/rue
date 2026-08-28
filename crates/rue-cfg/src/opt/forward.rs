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
//! constants, now stated for general values. (`debug_assertions` builds verify
//! it explicitly against [`crate::dominators`].)
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
//! - any call (including an `AccessorCall`) with a by-ref argument rooted at
//!   that local kills it (the callee may write through the pointer). A by-ref
//!   argument rooted at a shape this pass cannot identify as a specific local
//!   or parameter clears the whole table conservatively.
//!
//! The table resets at each block boundary. Address-taken slots are excluded
//! from it entirely: a raw pointer may alias them and store between a tracked
//! write and a load (RUE-521 covers reads after an aliased write, not only
//! place preservation).
//!
//! A `Load` that is itself a by-ref call argument is never forwarded under
//! either rule: the by-ref lowering requires that argument to remain a place.
//!
//! ## Applying substitutions and cleanup
//!
//! Both rules record `subst[load] = value`; all substitutions apply in one
//! [`Cfg::rewrite_value_uses`] sweep, resolving chains (a forwarded value may
//! itself be a forwarded load) exactly as [`super::cse`] and
//! [`super::peephole`] do. A forwarded `Load` then has no remaining uses.
//! Unlike CSE — which must overwrite a duplicated *trapping* op with a dead
//! placeholder because DCE deliberately preserves possibly-trapping arithmetic
//! (RUE-57) — loads never trap and are not on DCE's side-effect list, so DCE
//! sweeps the orphaned loads on its own. They are left as plain `Load`
//! instructions, not dummied out.

use crate::{BlockId, Cfg, CfgInstData, CfgValue, PlaceBase};
use ahash::AHashSet;

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
}

/// Run value forwarding. Call at `-O2`/`-O3` after simplification and before
/// CSE. Ownership-boundary values are always preserved: they describe a
/// semantic transfer across an inline boundary, not an optional optimization
/// policy.
pub fn run(cfg: &mut Cfg) -> Result<Stats, crate::CfgEditError> {
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
    // ------------------------------------------------------------------
    let mut byref_arg_values: AHashSet<CfgValue> = AHashSet::new();
    for block in cfg.blocks() {
        for &value in &block.insts {
            if let CfgInstData::Call { args, .. } | CfgInstData::AccessorCall { args, .. } =
                &cfg.get_inst(value).data
            {
                for arg in cfg.call_args(args) {
                    if arg.is_by_ref() {
                        byref_arg_values.insert(arg.value);
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

    // ------------------------------------------------------------------
    // Forward rewriting walk. One pass over every block-attached instruction
    // produces both rules' substitutions and maintains Rule 2's per-block
    // `last_store` table.
    // ------------------------------------------------------------------
    let mut subst: Vec<Option<CfgValue>> = vec![None; cfg.value_count()];
    // (single-write block, forwarded load block) pairs for the dominance
    // correctness check.
    let mut rule1_dominance_checks: Vec<(BlockId, BlockId)> = Vec::new();
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
                    // Address-taken slots stay out of the block-local table;
                    // ownership-boundary Loads are filtered by value below.
                    if !cfg.is_address_taken(slot) && (slot as usize) < num_locals {
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
                    if let Some(SlotWrites::One {
                        value: write_value,
                        block: write_block,
                    }) = slot_class.get(slot as usize).copied()
                    {
                        // Rule 1: global single-write forwarding.
                        subst[value.as_u32() as usize] = Some(write_value);
                        stats.loads_forwarded_single_write += 1;
                        rule1_dominance_checks.push((write_block, block_id));
                    } else if !cfg.is_address_taken(slot) {
                        // Rule 2: block-local forwarding for multi-write slots.
                        if let Some(&Some(stored)) = last_store.get(slot as usize) {
                            subst[value.as_u32() as usize] = Some(stored);
                            stats.loads_forwarded_block_local += 1;
                        }
                    }
                }
                CfgInstData::PlaceWrite { place, .. } => {
                    // A partial write invalidates the whole-slot value.
                    if let PlaceBase::Local(slot) = place.base {
                        if (slot as usize) < num_locals {
                            last_store[slot as usize] = None;
                        }
                    }
                }
                // Both call forms can pass a place to code that writes through
                // it, so a by-ref argument invalidates the tracked value.
                CfgInstData::Call { args, .. } | CfgInstData::AccessorCall { args, .. } => {
                    let mut clear_all = false;
                    for arg in cfg.call_args(&args).to_vec() {
                        if !arg.is_by_ref() {
                            continue;
                        }
                        match &cfg.get_inst(arg.value).data {
                            CfgInstData::Load { slot } => {
                                if (*slot as usize) < num_locals {
                                    last_store[*slot as usize] = None;
                                }
                            }
                            CfgInstData::PlaceRead { place } => match place.base {
                                // A local base: the callee may write that local.
                                PlaceBase::Local(slot) => {
                                    if (slot as usize) < num_locals {
                                        last_store[slot as usize] = None;
                                    }
                                }
                                // A parameter base writes through a parameter,
                                // not a local slot: nothing to kill.
                                PlaceBase::Param(_) => {}
                                PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => {
                                    clear_all = true;
                                }
                            },
                            // An unidentifiable by-ref root: be safe and clear
                            // the whole table at this call.
                            _ => clear_all = true,
                        }
                    }
                    if clear_all {
                        for slot in last_store.iter_mut() {
                            *slot = None;
                        }
                    }
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
    if !rule1_dominance_checks.is_empty() {
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
    cfg.rewrite_value_uses(|v| resolved[v.as_u32() as usize])?;

    Ok(stats)
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
                runtime: None,
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
        // exercises the debug dominance assertion.)
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
}
