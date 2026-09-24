//! The move-out fact (RUE-2367): no value read from a place after a move out
//! of an overlapping place is dropped, unless the place's runtime drop flag
//! guards the read.
//!
//! The builder marks every move of a value that needs dropping with a
//! `MoveOut` of the moved place, a local or parameter root optionally
//! projected through fields and constant indices, right after the flag's
//! clearing. After the move the new owner drops the value, so this function
//! owes no drop of that place, any place inside it, or any place containing
//! it, until the moved place is written again. The builder drops a place by
//! reading it afresh and dropping the read (scope exit, overwrite, `break`),
//! so the fact checks the read: a value read before the move is the moved
//! value itself, which its new owner may drop. Where a move happens on only
//! some paths, the builder guards the later drop with the place's drop flag
//! (`build.rs`, `emit_guarded`), and the flag proof the consumption facts
//! share (`drop_flag_guarded_blocks`, RUE-2290) says which blocks it
//! protects.
//!
//! Without the marker a builder bug that loses a guard, or drops a place it
//! should have known was moved, reaches native code unless a program happens
//! to observe the double drop (RUE-2319, RUE-2356, RUE-2378, RUE-2380).
//!
//! The fact is deliberately one-sided: it reports drops, never a missing
//! one, and whatever it cannot place exactly (a dropped value that is not a
//! read of a place, such as a forwarded store or a block parameter; a write
//! through a dynamic index; a base that is not a local or parameter) only
//! ever makes it accept more. So does a `MoveOut` whose index projection is
//! not a literal `Const`: the move has no exact place, and it goes unchecked
//! without a signal. A false rejection is an internal error on valid code.

use super::{EntryEdge, OwnerRoot, SEMANTIC_STATE_A, SEMANTIC_STATE_B, Verifier};
use crate::inst::{BlockId, CfgInstData, CfgValue, Place, PlaceBase, Projection};
use crate::verify::{CfgVerificationError, CfgVerificationLocation, DropFlagSlots};

/// One step of a place's path below its owner root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Step {
    Field(u32),
    Index(u64),
    /// An index this pass cannot resolve to a constant.
    AnyIndex,
}

type Path = Box<[Step]>;

/// A place below an owner root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MovedPlace {
    root: OwnerRoot,
    path: Path,
}

/// What an instruction means to the move facts of its root.
#[derive(Debug, Clone)]
enum Event {
    /// A `MoveOut` of the place.
    Move(MovedPlace),
    /// A write of the place: a moved place it names or contains is owned
    /// again.
    Write(MovedPlace),
    /// A read of the place whose value is dropped. The read, not the
    /// `Drop`, is what must not follow a move: a value read before the move
    /// is the moved value itself, which its new owner may drop, as a
    /// discarded temporary or a `match` arm's binding does.
    DroppedRead(MovedPlace),
}

/// Two paths name overlapping storage when one is a prefix of the other,
/// counting an unresolved index as any index.
fn overlaps(left: &[Step], right: &[Step]) -> bool {
    left.iter().zip(right).all(|pair| match pair {
        (Step::AnyIndex, _) | (_, Step::AnyIndex) => true,
        (left, right) => left == right,
    })
}

/// A write of `written` makes the value at `path` owned again: it names
/// `path` or a place containing it, counting an unresolved index as any
/// index. A write strictly inside `path` leaves the rest of it moved out.
fn rewrites(written: &[Step], path: &[Step]) -> bool {
    written.len() <= path.len() && overlaps(written, path)
}

/// `prefix` names `path` or a place containing it, exactly.
fn contains(prefix: &[Step], path: &[Step]) -> bool {
    prefix.len() <= path.len()
        && !prefix.contains(&Step::AnyIndex)
        && prefix.iter().zip(path).all(|(left, right)| left == right)
}

impl Verifier<'_> {
    /// The place `place` names below its owner root. With `exact`, a place
    /// with an unresolved index has none.
    fn moved_place(&self, place: &Place, exact: bool) -> Option<MovedPlace> {
        if !matches!(place.base, PlaceBase::Local(_) | PlaceBase::Param(_)) {
            return None;
        }
        let root = self.place_owner_root(place)?;
        let mut path = Vec::new();
        for projection in self.cfg.get_place_projections(place) {
            path.push(match *projection {
                Projection::Field { field_index, .. } => Step::Field(field_index),
                Projection::Index { index, .. } => match self.cfg.get_inst(index).data {
                    CfgInstData::Const(value) => Step::Index(value),
                    _ if exact => return None,
                    _ => Step::AnyIndex,
                },
            });
        }
        Some(MovedPlace {
            root,
            path: path.into_boxed_slice(),
        })
    }

    /// What instruction `value` (with data `data`) means to the move facts,
    /// if anything. `dropped` says which values some `Drop` consumes.
    fn move_event(
        &self,
        value: CfgValue,
        data: &CfgInstData,
        dropped: &ahash::AHashSet<CfgValue>,
        value_roots: &[Option<OwnerRoot>],
    ) -> Option<Event> {
        match data {
            CfgInstData::MoveOut { place } => self.moved_place(place, true).map(Event::Move),
            CfgInstData::Load { .. }
            | CfgInstData::Param { .. }
            | CfgInstData::PlaceRead { .. }
                if dropped.contains(&value) =>
            {
                let place = match (value_roots[value.as_u32() as usize], data) {
                    (Some(root), _) => Some(MovedPlace {
                        root,
                        path: Box::new([]),
                    }),
                    (None, CfgInstData::PlaceRead { place }) => self.moved_place(place, true),
                    (None, _) => None,
                };
                place.map(Event::DroppedRead)
            }
            CfgInstData::PlaceWrite { place, .. } => {
                self.moved_place(place, false).map(Event::Write)
            }
            CfgInstData::StorageLive { slot, local_ty } => Some(Event::Write(MovedPlace {
                root: OwnerRoot::Local {
                    slot: *slot,
                    ty: *local_ty,
                },
                path: Box::new([]),
            })),
            _ => self.whole_write_root(data).map(|root| {
                Event::Write(MovedPlace {
                    root,
                    path: Box::new([]),
                })
            }),
        }
    }

    /// Check every move-out fact of the function. A function with no
    /// `MoveOut` pays one scan of its reachable instructions and nothing
    /// else; one with moves pays two more scans, and a two-state solve only
    /// for each moved place that some dropped read overlaps.
    pub(super) fn verify_move_facts(
        &self,
        value_roots: &[Option<OwnerRoot>],
        flag_slots: &DropFlagSlots,
        entry_edges: &[Vec<EntryEdge>],
    ) -> Result<(), CfgVerificationError> {
        use ahash::{AHashMap, AHashSet};

        let reachable = || {
            self.cfg
                .blocks()
                .iter()
                .filter(|block| self.dominators().is_reachable(block.id))
        };
        let any_move = reachable().any(|block| {
            block
                .insts
                .iter()
                .any(|&value| matches!(self.cfg.get_inst(value).data, CfgInstData::MoveOut { .. }))
        });
        if !any_move {
            return Ok(());
        }

        let mut dropped = AHashSet::<CfgValue>::new();
        for block in reachable() {
            for &value in &block.insts {
                if let CfgInstData::Drop { value: operand } = self.cfg.get_inst(value).data
                    && self
                        .type_pool
                        .type_needs_drop(self.cfg.get_inst(operand).ty)
                {
                    dropped.insert(operand);
                }
            }
        }

        let mut events: Vec<Vec<(CfgValue, Event)>> = vec![Vec::new(); self.cfg.block_count()];
        let mut moved = Vec::<MovedPlace>::new();
        let mut moved_set = AHashSet::<MovedPlace>::new();
        let mut drops = AHashMap::<OwnerRoot, Vec<Path>>::new();
        for block in reachable() {
            for &value in &block.insts {
                let data = &self.cfg.get_inst(value).data;
                let Some(event) = self.move_event(value, data, &dropped, value_roots) else {
                    continue;
                };
                match &event {
                    Event::Move(place) => {
                        if moved_set.insert(place.clone()) {
                            moved.push(place.clone());
                        }
                    }
                    Event::DroppedRead(place) => {
                        drops
                            .entry(place.root)
                            .or_default()
                            .push(place.path.clone());
                    }
                    Event::Write(_) => {}
                }
                events[block.id.as_u32() as usize].push((value, event));
            }
        }

        for place in moved {
            let conflicts = drops
                .get(&place.root)
                .is_some_and(|paths| paths.iter().any(|path| overlaps(path, &place.path)));
            if conflicts {
                self.verify_move_fact(&place, &events, flag_slots, entry_edges)?;
            }
        }
        Ok(())
    }

    /// The move-out fact of `place`, checked first with no drop-flag
    /// exemption like [`Self::verify_exact_drop_fact`].
    fn verify_move_fact(
        &self,
        place: &MovedPlace,
        events: &[Vec<(CfgValue, Event)>],
        flag_slots: &DropFlagSlots,
        entry_edges: &[Vec<EntryEdge>],
    ) -> Result<(), CfgVerificationError> {
        let Err(error) = self.verify_move_fact_under(place, events, &[]) else {
            return Ok(());
        };
        // The flag of a moved place is cleared at each move of exactly that
        // place, and armed where the place, or one containing it, is written.
        let guarded = self.drop_flag_guarded_blocks(
            flag_slots,
            entry_edges,
            !matches!(place.root, OwnerRoot::Local { .. }),
            &|data| {
                matches!(data, CfgInstData::MoveOut { place: moved }
                    if self.moved_place(moved, true).as_ref() == Some(place))
            },
            &|_, data| match data {
                CfgInstData::PlaceWrite { place: written, .. } => {
                    self.moved_place(written, false).is_some_and(|written| {
                        written.root == place.root && contains(&written.path, &place.path)
                    })
                }
                _ => self.whole_write_root(data) == Some(place.root),
            },
        );
        if !guarded.contains(&true) {
            return Err(error);
        }
        self.verify_move_fact_under(place, events, &guarded)
    }

    /// The move-out fact of `place` with the blocks in `guarded` (indexed by
    /// block; missing entries are unguarded) starting owned.
    fn verify_move_fact_under(
        &self,
        place: &MovedPlace,
        events: &[Vec<(CfgValue, Event)>],
        guarded: &[bool],
    ) -> Result<(), CfgVerificationError> {
        const OWNED: u8 = SEMANTIC_STATE_A;
        const MOVED: u8 = SEMANTIC_STATE_B;
        let guarded = |block: BlockId| guarded.get(block.as_u32() as usize) == Some(&true);
        // The state after `event`, and whether it drops the moved place.
        let step = |state: u8, event: &Event| -> (u8, bool) {
            match event {
                Event::Move(moved) if moved == place => (MOVED, false),
                Event::Write(written)
                    if written.root == place.root && rewrites(&written.path, &place.path) =>
                {
                    (OWNED, false)
                }
                Event::DroppedRead(read)
                    if read.root == place.root && overlaps(&read.path, &place.path) =>
                {
                    (state, state & MOVED != 0)
                }
                _ => (state, false),
            }
        };
        let inputs = self.solve_semantic_fact(|block, mut state| {
            if guarded(block) {
                state = OWNED;
            }
            for (_, event) in &events[block.as_u32() as usize] {
                state = step(state, event).0;
            }
            state
        });

        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            let mut state = inputs[block.id.as_u32() as usize];
            if guarded(block.id) {
                state = OWNED;
            }
            for (value, event) in &events[block.id.as_u32() as usize] {
                let (next, conflict) = step(state, event);
                if conflict {
                    return Err(self.semantic_error(
                        CfgVerificationLocation::Instruction {
                            block: block.id,
                            value: *value,
                        },
                        format_args!(
                            "instruction {} in block {} reads, for a Drop, a place overlapping {:?} at path {:?}, which was moved out on a reaching path with no drop flag guarding the read",
                            value, block.id, place.root, place.path
                        ),
                    ));
                }
                state = next;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Step, contains, overlaps, rewrites};

    #[test]
    fn paths_overlap_when_one_contains_the_other() {
        use Step::*;
        assert!(overlaps(&[], &[Field(1)]));
        assert!(overlaps(&[Field(1), Field(0)], &[Field(1)]));
        assert!(!overlaps(&[Field(1)], &[Field(0)]));
        assert!(overlaps(&[Index(2)], &[AnyIndex, Field(0)]));
        assert!(!overlaps(&[Index(2)], &[Index(3)]));
        assert!(contains(&[Field(1)], &[Field(1), Field(2)]));
        assert!(!contains(&[Field(1), Field(2)], &[Field(1)]));
        assert!(!contains(&[AnyIndex], &[Index(0)]));
        assert!(rewrites(&[Field(1)], &[Field(1)]));
        assert!(rewrites(&[], &[Field(1)]));
        assert!(rewrites(&[AnyIndex], &[Index(2), Field(0)]));
        assert!(!rewrites(&[Field(1), Field(0)], &[Field(1)]));
        assert!(!rewrites(&[Field(0)], &[Field(1)]));
    }
}
