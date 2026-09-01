//! Compact snapshot index from CFG values to the instructions that use them.
//!
//! The index is CSR-shaped over the values that actually appear as operands:
//! one sparse bucket selects a contiguous run in `users`. Duplicate operands
//! remain duplicate edges, and users retain the order in which `rebuild`
//! receives them. Both properties are observable by sparse optimizer
//! worklists.
//!
//! This is deliberately a snapshot rather than an incremental SSA use list.
//! Moving an instruction between blocks does not invalidate it, but changing
//! any instruction operand does. The pass that owns such a mutation must
//! either finish consuming the old snapshot under an explicitly documented
//! snapshot algorithm, or call `invalidate`/`rebuild` before another lookup.

use crate::{Cfg, CfgValue, Type, inst::CfgOwnerIdentity};

/// Why a requested use-index operation does not name the indexed CFG domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CfgUseIndexError {
    Invalidated,
    WrongOwner,
    ValueDomainChanged,
    ValueOutOfRange(CfgValue),
    ValueTypeChanged(CfgValue),
}

/// Reusable CSR storage for a snapshot of instruction operand uses.
#[derive(Default)]
pub(super) struct CfgUseIndex {
    bucket_generation: Vec<u64>,
    bucket_index: Vec<usize>,
    generation: u64,
    keys: Vec<CfgValue>,
    key_types: Vec<Type>,
    offsets: Vec<usize>,
    users: Vec<CfgValue>,
    cursors: Vec<usize>,
    value_count: usize,
    owner: Option<CfgOwnerIdentity>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct CfgUseIndexWork {
    pub users_visited: u64,
    pub edges_visited: u64,
    /// Dense entries initialized only on amortized domain growth or the
    /// theoretical generation wrap, never once per sparse refill.
    pub domain_entries_initialized: u64,
}

impl CfgUseIndex {
    /// Refill the index with the supplied users, preserving their iteration
    /// order and every repeated operand edge.
    ///
    /// Existing allocations are retained. An invalid user or operand leaves
    /// the index invalid rather than exposing a partially refilled snapshot.
    pub(super) fn rebuild<I>(
        &mut self,
        cfg: &Cfg,
        user_order: I,
    ) -> Result<CfgUseIndexWork, CfgUseIndexError>
    where
        I: Iterator<Item = CfgValue> + Clone,
    {
        self.invalidate();
        let value_count = cfg.value_count();
        let mut work = CfgUseIndexWork::default();

        if self.bucket_generation.len() < value_count {
            let added = value_count - self.bucket_generation.len();
            self.bucket_generation.resize(value_count, 0);
            self.bucket_index.resize(value_count, 0);
            work.domain_entries_initialized += added as u64;
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.bucket_generation.fill(0);
            self.generation = 1;
            work.domain_entries_initialized += self.bucket_generation.len() as u64;
        }

        self.keys.clear();
        self.key_types.clear();
        self.offsets.clear();
        self.cursors.clear();
        self.users.clear();

        for user in user_order.clone() {
            work.users_visited += 1;
            if user.as_u32() as usize >= value_count {
                return Err(CfgUseIndexError::ValueOutOfRange(user));
            }
            let mut invalid = None;
            super::dce::visit_instruction_uses(cfg, user, |operand| {
                work.edges_visited += 1;
                let operand_idx = operand.as_u32() as usize;
                if operand_idx >= value_count {
                    invalid = Some(operand);
                } else {
                    if self.bucket_generation[operand_idx] != self.generation {
                        self.bucket_generation[operand_idx] = self.generation;
                        self.bucket_index[operand_idx] = self.keys.len();
                        self.keys.push(operand);
                        self.key_types.push(cfg.get_inst(operand).ty);
                        self.offsets.push(0);
                    }
                    self.offsets[self.bucket_index[operand_idx]] += 1;
                }
            });
            if let Some(operand) = invalid {
                return Err(CfgUseIndexError::ValueOutOfRange(operand));
            }
        }

        let mut total = 0;
        for count in &mut self.offsets {
            let next = total + *count;
            *count = total;
            total = next;
        }
        self.offsets.push(total);
        self.cursors
            .extend_from_slice(&self.offsets[..self.keys.len()]);
        self.users.resize(total, CfgValue::from_raw(0));

        for user in user_order {
            work.users_visited += 1;
            super::dce::visit_instruction_uses(cfg, user, |operand| {
                work.edges_visited += 1;
                let operand_idx = operand.as_u32() as usize;
                let cursor = &mut self.cursors[self.bucket_index[operand_idx]];
                self.users[*cursor] = user;
                *cursor += 1;
            });
        }

        self.value_count = value_count;
        self.owner = Some(cfg.owner_identity().clone());
        Ok(work)
    }

    /// Return the snapshot users of `value` after exact owner, value-domain,
    /// and value-type validation.
    pub(super) fn users<'a>(
        &'a self,
        cfg: &Cfg,
        value: CfgValue,
    ) -> Result<&'a [CfgValue], CfgUseIndexError> {
        let Some(owner) = &self.owner else {
            return Err(CfgUseIndexError::Invalidated);
        };
        if !owner.same_owner(cfg.owner_identity()) {
            return Err(CfgUseIndexError::WrongOwner);
        }
        if cfg.value_count() != self.value_count {
            return Err(CfgUseIndexError::ValueDomainChanged);
        }
        let idx = value.as_u32() as usize;
        if idx >= self.value_count {
            return Err(CfgUseIndexError::ValueOutOfRange(value));
        }
        if self.bucket_generation[idx] != self.generation {
            return Ok(&self.users[0..0]);
        }
        let bucket = self.bucket_index[idx];
        assert_eq!(self.keys[bucket], value);
        let indexed_type = self.key_types[bucket];
        if cfg.get_inst(value).ty != indexed_type {
            return Err(CfgUseIndexError::ValueTypeChanged(value));
        }
        Ok(&self.users[self.offsets[bucket]..self.offsets[bucket + 1]])
    }

    /// Make stale adjacency unobservable while retaining all allocations.
    pub(super) fn invalidate(&mut self) {
        self.owner = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgInst, CfgInstData};
    use rue_span::Span;

    fn make_cfg() -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "uses".to_string(), vec![]);
        cfg.entry = cfg.new_block();
        cfg
    }

    fn push(cfg: &mut Cfg, data: CfgInstData, ty: Type) -> CfgValue {
        cfg.add_inst_to_block(
            cfg.entry,
            CfgInst {
                data,
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    fn all_values(cfg: &Cfg) -> impl Iterator<Item = CfgValue> + Clone {
        (0..cfg.value_count()).map(|i| CfgValue::from_raw(i as u32))
    }

    #[test]
    fn preserves_user_and_duplicate_operand_order() {
        let mut cfg = make_cfg();
        let operand = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        let first = push(&mut cfg, CfgInstData::Add(operand, operand), Type::I32);
        let second = push(&mut cfg, CfgInstData::Neg(operand), Type::I32);
        let mut index = CfgUseIndex::default();

        index.rebuild(&cfg, [second, first].into_iter()).unwrap();

        assert_eq!(index.users(&cfg, operand).unwrap(), &[second, first, first]);
    }

    #[test]
    fn rejects_counterfeit_owner_value_and_type_identities() {
        let mut cfg = make_cfg();
        let value = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(&mut cfg, CfgInstData::Neg(value), Type::I32);
        let mut index = CfgUseIndex::default();
        index.rebuild(&cfg, all_values(&cfg)).unwrap();

        let cloned_peer = cfg.clone();
        assert_eq!(
            index.users(&cloned_peer, value),
            Err(CfgUseIndexError::WrongOwner)
        );

        let mut peer = make_cfg();
        push(&mut peer, CfgInstData::Const(1), Type::I32);
        push(&mut peer, CfgInstData::Neg(value), Type::I32);
        assert_eq!(index.users(&peer, value), Err(CfgUseIndexError::WrongOwner));
        assert_eq!(
            index.users(&cfg, CfgValue::from_raw(2)),
            Err(CfgUseIndexError::ValueOutOfRange(CfgValue::from_raw(2)))
        );

        cfg.replace_inst_type(value, Type::U32).unwrap();
        assert_eq!(
            index.users(&cfg, value),
            Err(CfgUseIndexError::ValueTypeChanged(value))
        );
    }

    #[test]
    fn same_storage_peer_replacement_cannot_counterfeit_owner() {
        let mut cfg = make_cfg();
        let value = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(&mut cfg, CfgInstData::Neg(value), Type::I32);
        let mut index = CfgUseIndex::default();
        index.rebuild(&cfg, all_values(&cfg)).unwrap();

        let mut peer = make_cfg();
        push(&mut peer, CfgInstData::Const(1), Type::I32);
        push(&mut peer, CfgInstData::Neg(value), Type::I32);
        let old_owner = std::mem::replace(&mut cfg, peer);
        drop(old_owner);

        assert_eq!(index.users(&cfg, value), Err(CfgUseIndexError::WrongOwner));
    }

    #[test]
    fn mutation_requires_invalidation_or_rebuild() {
        let mut cfg = make_cfg();
        let old = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        let new = push(&mut cfg, CfgInstData::Const(2), Type::I32);
        let user = push(&mut cfg, CfgInstData::Neg(old), Type::I32);
        let mut index = CfgUseIndex::default();
        index.rebuild(&cfg, all_values(&cfg)).unwrap();
        assert_eq!(index.users(&cfg, old).unwrap(), &[user]);

        cfg.get_inst_mut(user).data = CfgInstData::Neg(new);
        index.invalidate();
        assert_eq!(index.users(&cfg, old), Err(CfgUseIndexError::Invalidated));
        index.rebuild(&cfg, all_values(&cfg)).unwrap();
        assert!(index.users(&cfg, old).unwrap().is_empty());
        assert_eq!(index.users(&cfg, new).unwrap(), &[user]);
    }

    #[test]
    fn empty_large_and_refill_storage_reuse() {
        let empty = make_cfg();
        let mut index = CfgUseIndex::default();
        index.rebuild(&empty, all_values(&empty)).unwrap();
        assert_eq!(index.offsets, [0]);
        assert!(index.users.is_empty());

        let mut large = make_cfg();
        let root = push(&mut large, CfgInstData::Const(0), Type::I32);
        for _ in 0..4096 {
            push(&mut large, CfgInstData::Neg(root), Type::I32);
        }
        index.rebuild(&large, all_values(&large)).unwrap();
        assert_eq!(index.users(&large, root).unwrap().len(), 4096);
        let capacities = (
            index.bucket_generation.capacity(),
            index.bucket_index.capacity(),
            index.keys.capacity(),
            index.key_types.capacity(),
            index.offsets.capacity(),
            index.users.capacity(),
            index.cursors.capacity(),
        );

        let work = index
            .rebuild(&large, [CfgValue::from_raw(4096)].into_iter())
            .unwrap();
        assert_eq!(work.users_visited, 2);
        assert_eq!(work.edges_visited, 2);
        assert_eq!(work.domain_entries_initialized, 0);
        assert_eq!(
            capacities,
            (
                index.bucket_generation.capacity(),
                index.bucket_index.capacity(),
                index.keys.capacity(),
                index.key_types.capacity(),
                index.offsets.capacity(),
                index.users.capacity(),
                index.cursors.capacity(),
            )
        );
    }

    #[test]
    fn malformed_operand_never_publishes_partial_index() {
        let mut cfg = make_cfg();
        let invalid = CfgValue::from_raw(99);
        push(&mut cfg, CfgInstData::Neg(invalid), Type::I32);
        let mut index = CfgUseIndex::default();

        assert_eq!(
            index.rebuild(&cfg, all_values(&cfg)),
            Err(CfgUseIndexError::ValueOutOfRange(invalid))
        );
        assert_eq!(
            index.users(&cfg, CfgValue::from_raw(0)),
            Err(CfgUseIndexError::Invalidated)
        );
    }
}
