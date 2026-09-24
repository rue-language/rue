//! The drop-flag guard mutation (RUE-2367): test support that measures how
//! much of the drop-flag discipline the verifier checks.
//!
//! The builder guards the scope-exit, overwrite and break drop of a place
//! that may have been moved out with a runtime drop flag: `if flag != 0 {
//! drop }` (`build.rs`, `begin_flag_guard`). A builder bug that loses such a
//! guard drops a moved-out value. The mutation deletes a guard from a built
//! CFG, turning its branch into a jump into the guarded body, and asks the
//! verifier whether it notices.

use crate::inst::{BlockId, CfgInstData, Terminator, ValidatedCfg};
use crate::payload::CfgGotoArgs;
use crate::verify::CfgVerificationError;

impl ValidatedCfg {
    /// The blocks ending in a drop-flag guard, in block order: a `Branch` on
    /// `flag != 0`, where `flag` is loaded from a compiler-owned slot (one
    /// with no storage marker) in the branching block, into a body that drops
    /// something or opens a nested guard.
    #[doc(hidden)]
    pub fn drop_flag_guard_blocks(&self) -> Vec<BlockId> {
        let mut declared = ahash::AHashSet::new();
        for block in self.blocks() {
            for &value in &block.insts {
                if let CfgInstData::StorageLive { slot, .. } = self.get_inst(value).data {
                    declared.insert(slot);
                }
            }
        }
        let flag_test = |block: BlockId| -> Option<BlockId> {
            let Terminator::Branch {
                cond, then_block, ..
            } = &self.get_block(block).terminator
            else {
                return None;
            };
            let CfgInstData::Ne(lhs, rhs) = self.get_inst(*cond).data else {
                return None;
            };
            let is_flag = |load, zero| {
                matches!(self.get_inst(load).data,
                    CfgInstData::Load { slot } if !declared.contains(&slot))
                    && matches!(self.get_inst(zero).data, CfgInstData::Const(0))
                    && self.get_block(block).insts.contains(&load)
            };
            (is_flag(lhs, rhs) || is_flag(rhs, lhs)).then_some(*then_block)
        };
        let mut guards = Vec::new();
        for block in self.blocks() {
            let Some(body) = flag_test(block.id) else {
                continue;
            };
            let drops = self
                .get_block(body)
                .insts
                .iter()
                .any(|&value| matches!(self.get_inst(value).data, CfgInstData::Drop { .. }));
            if drops || flag_test(body).is_some() {
                guards.push(block.id);
            }
        }
        guards
    }

    /// Whether the flag the guard ending `guard` tests may be zero there:
    /// some path from the entry reaches the test with a `Store` of zero as
    /// the flag's last write. Deleting a guard whose flag never is only
    /// removes a test that always passes, which the verifier rightly
    /// accepts; the builder emits such guards where it cannot see that a
    /// move is always followed by a reinitialization (a loop head joining
    /// the entry and a back edge).
    ///
    /// The analysis reads only `Store`s of a literal `Const 0` as clearing
    /// the flag, so it panics unless every write to the flag slot is a
    /// `Store` of `Const 0` or `Const 1`, the only writes the builder emits
    /// (`build.rs`, `update_drop_flag`). Without that check a builder change
    /// that cleared a flag some other way would turn a harmful guard
    /// deletion into a "benign" one here.
    ///
    /// # Panics
    ///
    /// When the flag slot receives any other write.
    #[doc(hidden)]
    pub fn drop_flag_guard_may_skip(&self, guard: BlockId) -> bool {
        let Terminator::Branch { cond, .. } = &self.get_block(guard).terminator else {
            return false;
        };
        let CfgInstData::Ne(lhs, rhs) = self.get_inst(*cond).data else {
            return false;
        };
        let flag = [lhs, rhs]
            .into_iter()
            .find_map(|value| match self.get_inst(value).data {
                CfgInstData::Load { slot } => Some((value, slot)),
                _ => None,
            });
        let Some((load, flag)) = flag else {
            return false;
        };
        for block in self.blocks() {
            for &value in &block.insts {
                let data = &self.get_inst(value).data;
                let written = match data {
                    CfgInstData::Store { slot, .. } | CfgInstData::Alloc { slot, .. } => {
                        *slot == flag
                    }
                    CfgInstData::PlaceWrite { place, .. } => {
                        place.base == crate::inst::PlaceBase::Local(flag)
                    }
                    _ => false,
                };
                let flag_value = matches!(data, CfgInstData::Store { value: stored, .. }
                    if matches!(self.get_inst(*stored).data, CfgInstData::Const(0 | 1)));
                assert!(
                    !written || flag_value,
                    "drop flag slot {flag} receives {value} in block {}, which is not a Store \
                     of Const 0 or Const 1",
                    block.id
                );
            }
        }
        // The flag's last write in a block, from its start to `until`.
        let last_write = |block: BlockId, until: Option<crate::inst::CfgValue>| {
            let mut last = None;
            for &value in &self.get_block(block).insts {
                if Some(value) == until {
                    break;
                }
                if let CfgInstData::Store {
                    slot,
                    value: stored,
                } = self.get_inst(value).data
                    && slot == flag
                {
                    last = Some(matches!(self.get_inst(stored).data, CfgInstData::Const(0)));
                }
            }
            last
        };
        let mut may_clear = vec![false; self.block_count()];
        let mut visited = vec![false; self.block_count()];
        let mut work = vec![(self.entry, false)];
        while let Some((block, incoming)) = work.pop() {
            let index = block.as_u32() as usize;
            if visited[index] && (may_clear[index] || !incoming) {
                continue;
            }
            visited[index] = true;
            may_clear[index] |= incoming;
            let out = last_write(block, None).unwrap_or(may_clear[index]);
            let terminator = &self.get_block(block).terminator;
            let successors: Vec<BlockId> = match terminator {
                Terminator::Goto { target, .. } => vec![*target],
                Terminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => vec![*then_block, *else_block],
                Terminator::Switch { cases, default, .. } => self
                    .switch_cases(cases)
                    .iter()
                    .map(|&(_, target)| target)
                    .chain(std::iter::once(*default))
                    .collect(),
                _ => Vec::new(),
            };
            for successor in successors {
                work.push((successor, out));
            }
        }
        last_write(guard, Some(load)).unwrap_or(may_clear[guard.as_u32() as usize])
    }

    /// Delete the drop-flag guards ending `guards` (from
    /// [`Self::drop_flag_guard_blocks`]) so each guarded body always runs,
    /// and verify the result under the post-optimization contract.
    #[doc(hidden)]
    pub fn verify_without_drop_flag_guards(
        &self,
        type_pool: &rue_air::FrozenTypeInternPool,
        guards: &[BlockId],
    ) -> Result<(), CfgVerificationError> {
        let mut mutant = self.0.clone();
        for &block in guards {
            let Terminator::Branch { then_block, .. } = mutant.get_block(block).terminator else {
                panic!("block {block} does not end in a drop-flag guard");
            };
            mutant.get_block_mut(block).terminator = Terminator::Goto {
                target: then_block,
                args: CfgGotoArgs::EMPTY,
            };
        }
        // The graph may be an optimized one, whose dead values stay in the
        // arena detached; the post-optimization contract checks everything
        // else exactly as strictly.
        mutant.verify_after_optimization_with_type_pool(type_pool)
    }
}
