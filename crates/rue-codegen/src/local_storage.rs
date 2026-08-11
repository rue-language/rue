//! Marker-driven local frame-slot planning (RUE-768).
//!
//! The CFG brackets every local's storage with `StorageLive`/`StorageDead`
//! markers. Codegen used to discard both as no-ops and give every CFG local
//! slot its own frame cell for the whole function (an identity map through
//! [`CfgLowerContext::frame_slot`](crate::cfg_lower::CfgLowerContext::frame_slot)),
//! so a temporary confined to one arm of a `match` still cost a cell in every
//! other arm.
//!
//! This module turns those markers into a frame-slot assignment: locals whose
//! storage windows are **provably disjoint** share frame cells, and everything
//! else keeps a private region. It is the local-slot counterpart of
//! [`ParamStoragePlan`](crate::param_storage::ParamStoragePlan) (RUE-1170) and
//! feeds the same single funnel, so the frame layout and the code that
//! addresses it cannot drift apart.
//!
//! ## The unit of allocation
//!
//! A *slot entity* is one contiguous run of CFG local slots that must move
//! through the frame as a unit. Its span is the widest reference rooted at its
//! base slot: a `[Shape; 4]` local whose element type is three slots wide is
//! one twelve-slot entity, never twelve independent cells, because every
//! aggregate access is emitted as `frame_slot(base) + k`. Entities are built
//! from *all* slot references, not just the markers, so an aggregate can never
//! be split across two frame regions.
//!
//! ## What may share
//!
//! An entity is shareable only when every one of these holds:
//!
//! 1. It has at least one `StorageLive` marker — without a window there is
//!    nothing to prove.
//! 2. Every reference to it happens at a program point where its window is
//!    open, under a may-be-live dataflow over the markers (`StorageLive` gens,
//!    `StorageDead` kills, the entry state is empty, and a block's entry state
//!    is the union over its reachable predecessors). A reference outside the
//!    window would mean the markers do not describe the real lifetime, so such
//!    an entity is kept private.
//! 3. Its address is not exposed to a raw-pointer intrinsic
//!    (`@raw`/`@raw_mut`/`@field_ptr`). Those produce a first-class pointer
//!    that can outlive the operand's window; a private cell keeps such code
//!    behaving exactly as it did before.
//!
//! Two shareable entities may occupy overlapping frame regions only when the
//! dataflow proves their windows are never simultaneously open. Everything
//! else — an unreferenced slot, or an entity that fails any test above — is
//! modelled as interfering with every other entity and therefore gets a region
//! of its own.
//!
//! By-reference call arguments (`inout`/`borrow`) need no special rule. Taking
//! a callee-visible address of a local is an ordinary reference, so it must
//! fall inside the local's window, and Rue scopes the borrow to the call: the
//! reference semantics in `rue-oracle` model `inout`/`borrow` as
//! copy-in/copy-out, so a borrow outliving its call is not a Rue program. A
//! borrow that keeps a local in use *later* than its last direct load
//! therefore holds the whole window open, and the window — not the last direct
//! use — is what this analysis merges on.
//!
//! ## Failing closed
//!
//! Any shape the analysis cannot model exactly — a reference rooted inside
//! another entity's span, a slot range that leaves the local area, an
//! implausibly large function, a dataflow that does not converge, or a layout
//! the disjointness verifier rejects — produces the identity plan, which is
//! exactly the pre-RUE-768 layout. Sharing less is always sound; sharing
//! wrongly is silent data corruption.
//!
//! The disjointness verifier runs twice and is **always on**, release builds
//! included: [`verify_placement`] re-checks each merge as it is made, and
//! [`verify_sharing`] re-derives the invariant from the finished layout. This
//! crate deliberately carries no `debug_assert!`s (see
//! `scripts/validate-debug-assert-policy.py`), because a check that vanishes
//! in the shipped compiler is not a barrier against wrong generated code.

use lasso::ThreadedRodeo;
use rue_air::FrozenTypeInternPool;
use rue_cfg::{BasicBlock, BlockId, Cfg, CfgInstData, CfgValue, PlaceBase, Type};

/// Functions with more local slots than this keep the identity layout. The
/// analysis is quadratic in entity count; real functions are orders of
/// magnitude below the cap, and exceeding it costs only the optimization.
const MAX_PLANNED_LOCAL_SLOTS: u32 = 4096;

/// Work-product ceiling: the dataflow walks every block per entity and the
/// audit/coloring are quadratic in entities, so slots x blocks approximates
/// the planning cost. Generated mega-functions (the large examples) blow this
/// budget and keep the identity layout — measured on harbor, planning them
/// costs double-digit percent of total compile time for frame bytes nobody
/// observes — while every human-scale function stays orders of magnitude
/// below it (RUE-768 integration measurement).
const MAX_PLANNED_COST: u64 = 65_536;

/// Iteration cap for the may-be-live fixpoint. A gen/kill bit-vector problem
/// solved round-robin in (roughly) reverse postorder converges in about as
/// many passes as the CFG has nested loops, so this is orders of magnitude
/// more headroom than any real function needs; a CFG that somehow exceeds it
/// gets the identity plan instead of an unbounded solve.
const MAX_DATAFLOW_ROUNDS: u32 = 64;

/// A fixed-width bitset over entity indices.
#[derive(Clone, PartialEq, Eq, Debug)]
struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    fn new(bits: usize) -> Self {
        Self {
            words: vec![0; bits.div_ceil(64)],
        }
    }

    fn set(&mut self, bit: usize) {
        self.words[bit / 64] |= 1 << (bit % 64);
    }

    fn clear(&mut self, bit: usize) {
        self.words[bit / 64] &= !(1 << (bit % 64));
    }

    fn get(&self, bit: usize) -> bool {
        self.words[bit / 64] & (1 << (bit % 64)) != 0
    }

    fn union_with(&mut self, other: &Self) {
        for (dst, src) in self.words.iter_mut().zip(&other.words) {
            *dst |= *src;
        }
    }

    fn fill(&mut self, bits: usize) {
        for bit in 0..bits {
            self.set(bit);
        }
    }

    fn ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(word, bits)| {
            (0..64).filter_map(move |offset| {
                (bits & (1_u64 << offset) != 0).then_some(word * 64 + offset)
            })
        })
    }
}

/// One contiguous run of CFG local slots allocated as a unit.
#[derive(Clone, Debug)]
struct SlotEntity {
    /// First CFG local slot of the run.
    base: u32,
    /// CFG local slots the run covers (always at least one).
    span: u32,
    /// May this entity's frame region overlap a non-interfering entity's?
    shareable: bool,
    /// Assigned first frame local slot, filled in by region assignment.
    offset: u32,
}

/// A pair of entities the verifier rejected: their frame regions overlap even
/// though their storage windows can be open at the same time.
#[derive(Debug, PartialEq, Eq)]
struct SharingViolation {
    first: usize,
    second: usize,
}

/// The per-function local frame-slot assignment.
#[derive(Debug, Clone)]
pub(crate) struct LocalSlotPlan {
    /// CFG local slot -> emitted frame local slot.
    map: Vec<u32>,
    /// Frame local slots the assignment occupies.
    frame_local_slots: u32,
}

impl LocalSlotPlan {
    /// The historical plan: every CFG local slot keeps its own frame cell at
    /// its own index. This is the fallback whenever the analysis cannot prove
    /// a sharing decision, and the baseline the planned path shrinks from.
    pub(crate) fn identity(num_locals: u32) -> Self {
        Self {
            map: (0..num_locals).collect(),
            frame_local_slots: num_locals,
        }
    }

    /// Plan local frame slots for `cfg` from its storage markers.
    pub(crate) fn plan(
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        interner: &ThreadedRodeo,
    ) -> Self {
        Self::try_plan(cfg, type_pool, interner).unwrap_or_else(|| Self::identity(cfg.num_locals()))
    }

    /// The emitted frame local slot for CFG local slot `slot`.
    ///
    /// Slots inside one entity keep their relative order, so a multi-slot
    /// aggregate access emitted as `frame_slot(base) + k` still addresses that
    /// aggregate's own cells.
    ///
    /// A zero-slot (ZST) local occupies no cells, so the CFG is allowed to
    /// root one at `num_locals` itself — the slot one past the local area,
    /// which the plan therefore does not map. Such a slot keeps its CFG
    /// number, exactly as the pre-RUE-768 identity layout gave it, and the
    /// canonical ZST place address it yields (RUE-605) still names zero bytes.
    pub(crate) fn frame_slot(&self, slot: u32) -> u32 {
        self.map.get(slot as usize).copied().unwrap_or(slot)
    }

    /// Frame local slots the plan occupies (also the base of the emitted
    /// parameter area).
    pub(crate) fn frame_local_slots(&self) -> u32 {
        self.frame_local_slots
    }

    /// Whether any two entities were merged onto shared cells.
    #[cfg(test)]
    pub(crate) fn shares_any_slot(&self) -> bool {
        self.frame_local_slots < self.map.len() as u32
    }

    fn try_plan(
        cfg: &Cfg,
        type_pool: &FrozenTypeInternPool,
        interner: &ThreadedRodeo,
    ) -> Option<Self> {
        let num_locals = cfg.num_locals();
        if num_locals == 0 || num_locals > MAX_PLANNED_LOCAL_SLOTS {
            return None;
        }
        if u64::from(num_locals) * cfg.blocks().len() as u64 > MAX_PLANNED_COST {
            return None;
        }

        let refs = collect_slot_references(cfg, type_pool, interner)?;
        let mut entities = build_entities(num_locals, &refs)?;
        if entities.len() < 2 {
            return None;
        }

        let preds = cfg.compute_predecessors();
        let reachable = reachable_blocks(cfg, &preds);
        let live_in = solve_may_be_live(cfg, &preds, &entities, &refs, &reachable)?;
        let mut interference =
            audit_and_collect_interference(cfg, &mut entities, &refs, &reachable, &live_in);
        // An entity that cannot be proved safe interferes with everything, so
        // region assignment gives it a private run of cells.
        let count = entities.len();
        for index in 0..count {
            if entities[index].shareable {
                continue;
            }
            interference[index].fill(count);
            for row in interference.iter_mut() {
                row.set(index);
            }
        }

        // `assign_regions` re-verifies every merge as it makes it, and the
        // finished layout is re-verified as a whole below. Both checks are
        // always on, in release builds as well as debug ones: a layout the
        // verifier rejects is discarded outright, and the identity plan —
        // which cannot share anything — is emitted instead.
        let frame_local_slots = assign_regions(&mut entities, &interference)?;
        verify_sharing(&entities, &interference).ok()?;
        if frame_local_slots >= num_locals {
            // Nothing was saved; keep the identity map so every downstream
            // consumer sees the simplest possible numbering.
            return None;
        }

        let mut map = vec![u32::MAX; num_locals as usize];
        for entity in &entities {
            for k in 0..entity.span {
                map[(entity.base + k) as usize] = entity.offset + k;
            }
        }
        if map.contains(&u32::MAX) {
            // Some local slot fell outside every entity's run — the entities
            // do not tile the local area, so no mapping is safe to emit.
            return None;
        }
        Some(Self {
            map,
            frame_local_slots,
        })
    }
}

/// One reference to a run of local slots made by one CFG instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SlotReference {
    base: u32,
    span: u32,
    kind: ReferenceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReferenceKind {
    /// `StorageLive`: opens the window.
    Live,
    /// `StorageDead`: closes the window.
    Dead,
    /// An ordinary access that must fall inside the window.
    Access,
    /// An access that additionally hands the slot's address to a raw-pointer
    /// intrinsic, whose result can outlive the window.
    AddressEscape,
}

/// Per-instruction slot references, indexed by `CfgValue` raw index.
struct SlotReferences {
    per_value: Vec<Option<SlotReference>>,
}

impl SlotReferences {
    fn get(&self, value: CfgValue) -> Option<SlotReference> {
        self.per_value[value.as_u32() as usize]
    }
}

/// Collect every local-slot reference in `cfg`.
///
/// Returns `None` when a reference runs off the end of the local slot area,
/// which means this module's model of the function is wrong and no sharing may
/// happen.
fn collect_slot_references(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
) -> Option<SlotReferences> {
    let num_locals = cfg.num_locals();
    let span_of = |ty: Type| crate::types::type_slot_count(type_pool, ty).max(1);
    let mut per_value = vec![None; cfg.value_count()];
    let escaping = address_escaping_operands(cfg, interner);

    for block in cfg.blocks() {
        for &value in &block.insts {
            let inst = cfg.get_inst(value);
            let reference = match &inst.data {
                CfgInstData::StorageLive { slot, local_ty } => SlotReference {
                    base: *slot,
                    span: span_of(*local_ty),
                    kind: ReferenceKind::Live,
                },
                CfgInstData::StorageDead { slot, local_ty } => SlotReference {
                    base: *slot,
                    span: span_of(*local_ty),
                    kind: ReferenceKind::Dead,
                },
                // An `Alloc`'s own type is unit; the run it writes is the
                // initializer's.
                CfgInstData::Alloc { slot, init } => SlotReference {
                    base: *slot,
                    span: span_of(cfg.get_inst(*init).ty),
                    kind: ReferenceKind::Access,
                },
                CfgInstData::Load { slot } => SlotReference {
                    base: *slot,
                    span: span_of(inst.ty),
                    kind: ReferenceKind::Access,
                },
                CfgInstData::Store { slot, value } => SlotReference {
                    base: *slot,
                    span: span_of(cfg.get_inst(*value).ty),
                    kind: ReferenceKind::Access,
                },
                CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
                    match place.base {
                        PlaceBase::Local(slot) => SlotReference {
                            base: slot,
                            span: span_of(place.base_type),
                            kind: ReferenceKind::Access,
                        },
                        _ => continue,
                    }
                }
                _ => continue,
            };
            // Parameter-range slots are the parameter plan's business
            // (RUE-1170); only the local area is planned here.
            if reference.base >= num_locals {
                continue;
            }
            if reference.base.checked_add(reference.span)? > num_locals {
                return None;
            }
            let mut reference = reference;
            if escaping[value.as_u32() as usize] {
                reference.kind = ReferenceKind::AddressEscape;
            }
            per_value[value.as_u32() as usize] = Some(reference);
        }
    }
    Some(SlotReferences { per_value })
}

/// Flags, indexed by `CfgValue`, for operands whose address is handed to a
/// raw-pointer intrinsic (`@raw`/`@raw_mut`/`@field_ptr` — the `PlaceAddress`
/// operation in `value_plan`). The pointer those produce is a first-class
/// value that can outlive the operand's storage window, so the slot behind it
/// never shares.
fn address_escaping_operands(cfg: &Cfg, interner: &ThreadedRodeo) -> Vec<bool> {
    let mut escaping = vec![false; cfg.value_count()];
    for block in cfg.blocks() {
        for &value in &block.insts {
            let data = &cfg.get_inst(value).data;
            let CfgInstData::Intrinsic { name, .. } = data else {
                continue;
            };
            if !matches!(interner.resolve(name), "raw" | "raw_mut" | "field_ptr") {
                continue;
            }
            for &arg in cfg.get_intrinsic_args(data) {
                escaping[arg.as_u32() as usize] = true;
            }
        }
    }
    escaping
}

/// Build the slot entities: one contiguous run per reference root, plus a
/// private single cell for every local slot nothing references.
///
/// Returns `None` when a reference is rooted strictly inside another run,
/// which would mean one aggregate is addressed through two different bases.
fn build_entities(num_locals: u32, refs: &SlotReferences) -> Option<Vec<SlotEntity>> {
    let mut span_at = vec![0_u32; num_locals as usize];
    let mut has_live = vec![false; num_locals as usize];
    let mut escapes = vec![false; num_locals as usize];
    for reference in refs.per_value.iter().flatten() {
        let index = reference.base as usize;
        span_at[index] = span_at[index].max(reference.span);
        match reference.kind {
            ReferenceKind::Live => has_live[index] = true,
            ReferenceKind::AddressEscape => escapes[index] = true,
            ReferenceKind::Dead | ReferenceKind::Access => {}
        }
    }

    let mut entities: Vec<SlotEntity> = Vec::new();
    let mut slot = 0_u32;
    while slot < num_locals {
        let span = span_at[slot as usize];
        if span == 0 {
            // Nothing names this slot. Reserve it privately anyway: dropping
            // it outright would be a second layout change resting on the
            // reference scan being exhaustive, and a reserved cell costs
            // exactly what today's identity layout costs.
            entities.push(SlotEntity {
                base: slot,
                span: 1,
                shareable: false,
                offset: 0,
            });
            slot += 1;
            continue;
        }
        let end = slot.checked_add(span)?;
        if end > num_locals {
            return None;
        }
        // A run may not be re-rooted part way through.
        if span_at[slot as usize + 1..end as usize]
            .iter()
            .any(|&interior| interior != 0)
        {
            return None;
        }
        entities.push(SlotEntity {
            base: slot,
            span,
            shareable: has_live[slot as usize] && !escapes[slot as usize],
            offset: 0,
        });
        slot = end;
    }
    Some(entities)
}

/// Blocks reachable from the entry block. Unreachable code never executes, so
/// it neither constrains nor is constrained by the sharing decision.
fn reachable_blocks(cfg: &Cfg, preds: &[Vec<BlockId>]) -> Vec<bool> {
    let blocks = cfg.blocks().len();
    let mut reachable = vec![false; blocks];
    let mut successors: Vec<Vec<BlockId>> = vec![Vec::new(); blocks];
    for (block, block_preds) in preds.iter().enumerate() {
        for pred in block_preds {
            successors[pred.as_u32() as usize].push(BlockId::from_raw(block as u32));
        }
    }
    let entry = cfg.entry.as_u32() as usize;
    if entry >= blocks {
        return reachable;
    }
    let mut worklist = vec![entry];
    reachable[entry] = true;
    while let Some(block) = worklist.pop() {
        for successor in &successors[block] {
            let successor = successor.as_u32() as usize;
            if !reachable[successor] {
                reachable[successor] = true;
                worklist.push(successor);
            }
        }
    }
    reachable
}

/// Index of the entity based exactly at `base`, if there is one.
fn entity_at_base(entities: &[SlotEntity], base: u32) -> Option<usize> {
    entities
        .binary_search_by_key(&base, |entity| entity.base)
        .ok()
}

/// Solve the may-be-live dataflow over the storage markers, returning every
/// block's live-in set.
fn solve_may_be_live(
    cfg: &Cfg,
    preds: &[Vec<BlockId>],
    entities: &[SlotEntity],
    refs: &SlotReferences,
    reachable: &[bool],
) -> Option<Vec<BitSet>> {
    let blocks = cfg.blocks().len();
    let bits = entities.len();
    let mut live_in = vec![BitSet::new(bits); blocks];
    let mut live_out = vec![BitSet::new(bits); blocks];

    for _ in 0..MAX_DATAFLOW_ROUNDS {
        let mut changed = false;
        for block in cfg.blocks() {
            let index = block.id.as_u32() as usize;
            if !reachable[index] {
                continue;
            }
            let mut state = BitSet::new(bits);
            for pred in &preds[index] {
                let pred = pred.as_u32() as usize;
                if reachable[pred] {
                    state.union_with(&live_out[pred]);
                }
            }
            if state != live_in[index] {
                live_in[index] = state.clone();
                changed = true;
            }
            transfer_block(entities, refs, block, &mut state);
            if state != live_out[index] {
                live_out[index] = state;
                changed = true;
            }
        }
        if !changed {
            return Some(live_in);
        }
    }
    None
}

/// Apply one block's markers to `state`.
fn transfer_block(
    entities: &[SlotEntity],
    refs: &SlotReferences,
    block: &BasicBlock,
    state: &mut BitSet,
) {
    for &value in &block.insts {
        let Some(reference) = refs.get(value) else {
            continue;
        };
        let Some(entity) = entity_at_base(entities, reference.base) else {
            continue;
        };
        match reference.kind {
            ReferenceKind::Live => state.set(entity),
            ReferenceKind::Dead => state.clear(entity),
            ReferenceKind::Access | ReferenceKind::AddressEscape => {}
        }
    }
}

/// Record every entity live in `state` as interfering with every other.
fn record_simultaneous(state: &BitSet, interference: &mut [BitSet]) {
    for entity in state.ones() {
        interference[entity].union_with(state);
    }
}

/// Walk every reachable block once to (a) demote entities referenced outside
/// their window and (b) record which entities' windows can be open together.
fn audit_and_collect_interference(
    cfg: &Cfg,
    entities: &mut [SlotEntity],
    refs: &SlotReferences,
    reachable: &[bool],
    live_in: &[BitSet],
) -> Vec<BitSet> {
    let bits = entities.len();
    let mut interference = vec![BitSet::new(bits); bits];

    for block in cfg.blocks() {
        let index = block.id.as_u32() as usize;
        if !reachable[index] {
            continue;
        }
        let mut state = live_in[index].clone();
        record_simultaneous(&state, &mut interference);
        for &value in &block.insts {
            let Some(reference) = refs.get(value) else {
                continue;
            };
            let Some(entity) = entity_at_base(entities, reference.base) else {
                continue;
            };
            match reference.kind {
                ReferenceKind::Live => {
                    state.set(entity);
                    record_simultaneous(&state, &mut interference);
                }
                ReferenceKind::Dead => {
                    state.clear(entity);
                    record_simultaneous(&state, &mut interference);
                }
                ReferenceKind::Access | ReferenceKind::AddressEscape => {
                    if !state.get(entity) {
                        // The markers do not describe this slot's real
                        // lifetime; keep it private.
                        entities[entity].shareable = false;
                    }
                }
            }
        }
    }

    // An entity never interferes with itself for placement purposes.
    for (index, row) in interference.iter_mut().enumerate() {
        row.clear(index);
    }
    interference
}

/// Assign each entity a contiguous frame region, sharing cells between
/// entities the interference graph proves never overlap. Returns the frame
/// local slots the assignment occupies, or `None` when a placement fails the
/// disjointness check — the caller then falls back to the identity layout.
///
/// Entities are placed in ascending base-slot order so the layout is a pure
/// function of the CFG, and each placement is re-checked against every
/// already-placed entity before the next one is made. The check is always on
/// (this crate carries no `debug_assert!`s): a wrongly shared cell is silent
/// data corruption, so it must not be possible to build a release compiler
/// that skips it.
fn assign_regions(entities: &mut [SlotEntity], interference: &[BitSet]) -> Option<u32> {
    let ceiling: u32 = entities.iter().map(|entity| entity.span).sum();
    let mut total = 0_u32;
    for index in 0..entities.len() {
        let span = entities[index].span;
        let mut busy = vec![false; (ceiling + span) as usize];
        for placed in 0..index {
            if !interference[index].get(placed) {
                continue;
            }
            let other = &entities[placed];
            for cell in other.offset..other.offset + other.span {
                busy[cell as usize] = true;
            }
        }
        let mut offset = 0_u32;
        while busy[offset as usize..(offset + span) as usize]
            .iter()
            .any(|&cell| cell)
        {
            offset += 1;
        }
        entities[index].offset = offset;
        // Re-check the merge the moment it is made: a region overlapping an
        // already-placed entity is only legal when the two provably never have
        // open windows at the same time.
        verify_placement(entities, interference, index).ok()?;
        total = total.max(offset + span);
    }
    Some(total)
}

/// Do two placed entities occupy overlapping frame regions?
fn regions_overlap(a: &SlotEntity, b: &SlotEntity) -> bool {
    a.offset < b.offset + b.span && b.offset < a.offset + a.span
}

/// Check one just-placed entity against every entity placed before it. This is
/// the per-merge check, kept linear so the whole assignment stays quadratic.
fn verify_placement(
    entities: &[SlotEntity],
    interference: &[BitSet],
    index: usize,
) -> Result<(), SharingViolation> {
    for first in 0..index {
        if regions_overlap(&entities[first], &entities[index]) && interference[first].get(index) {
            return Err(SharingViolation {
                first,
                second: index,
            });
        }
    }
    Ok(())
}

/// Re-derive the sharing invariant from a finished layout: two entities may
/// occupy overlapping frame regions only when they never interfere.
fn verify_sharing(
    entities: &[SlotEntity],
    interference: &[BitSet],
) -> Result<(), SharingViolation> {
    for index in 0..entities.len() {
        verify_placement(entities, interference, index)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(base: u32, span: u32) -> SlotEntity {
        SlotEntity {
            base,
            span,
            shareable: true,
            offset: 0,
        }
    }

    fn placed(offset: u32, span: u32) -> SlotEntity {
        SlotEntity {
            base: offset,
            span,
            shareable: true,
            offset,
        }
    }

    fn interference_graph(count: usize, edges: &[(usize, usize)]) -> Vec<BitSet> {
        let mut graph = vec![BitSet::new(count); count];
        for &(a, b) in edges {
            graph[a].set(b);
            graph[b].set(a);
        }
        graph
    }

    #[test]
    fn disjoint_entities_share_one_region() {
        // Two single-slot temporaries whose windows never overlap collapse
        // onto one cell; a third that overlaps both keeps its own.
        let mut entities = vec![entity(0, 1), entity(1, 1), entity(2, 1)];
        let graph = interference_graph(3, &[(0, 2), (1, 2)]);
        let total = assign_regions(&mut entities, &graph).expect("legal layout");
        assert_eq!(entities[0].offset, 0);
        assert_eq!(entities[1].offset, 0, "disjoint windows share a cell");
        assert_eq!(entities[2].offset, 1);
        assert_eq!(total, 2);
        assert_eq!(verify_sharing(&entities, &graph), Ok(()));
    }

    #[test]
    fn overlapping_entities_never_share() {
        let mut entities = vec![entity(0, 1), entity(1, 1), entity(2, 1)];
        let graph = interference_graph(3, &[(0, 1), (0, 2), (1, 2)]);
        let total = assign_regions(&mut entities, &graph).expect("legal layout");
        assert_eq!(
            entities.iter().map(|e| e.offset).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(total, 3);
        assert_eq!(verify_sharing(&entities, &graph), Ok(()));
    }

    #[test]
    fn multi_slot_entities_move_as_units() {
        // A three-slot aggregate and a two-slot one with disjoint windows
        // overlay; a one-slot entity live across both sits below them.
        let mut entities = vec![entity(0, 1), entity(1, 3), entity(4, 2)];
        let graph = interference_graph(3, &[(0, 1), (0, 2)]);
        let total = assign_regions(&mut entities, &graph).expect("legal layout");
        assert_eq!(entities[0].offset, 0);
        assert_eq!(entities[1].offset, 1);
        assert_eq!(
            entities[2].offset, 1,
            "the two-slot run overlays the three-slot run's cells"
        );
        assert_eq!(total, 4);
        assert_eq!(verify_sharing(&entities, &graph), Ok(()));
    }

    /// The verifier must reject a deliberately bad merge. Without this the
    /// clean verdict on real layouts would prove nothing.
    #[test]
    fn injected_bad_merge_is_caught() {
        let graph = interference_graph(2, &[(0, 1)]);
        // Two entities whose windows overlap, forced onto the same cell.
        let entities = vec![placed(0, 1), placed(0, 1)];
        assert_eq!(
            verify_sharing(&entities, &graph),
            Err(SharingViolation {
                first: 0,
                second: 1
            })
        );

        // The same violation through a partial overlap of multi-slot runs: the
        // three-slot run at 0..3 and the two-slot run at 2..4 share cell 2.
        let entities = vec![placed(0, 3), placed(2, 2)];
        assert_eq!(
            verify_sharing(&entities, &graph),
            Err(SharingViolation {
                first: 0,
                second: 1
            })
        );

        // Non-interfering entities at the very same offset are fine.
        let graph = interference_graph(2, &[]);
        let entities = vec![placed(0, 3), placed(0, 2)];
        assert_eq!(verify_sharing(&entities, &graph), Ok(()));
    }

    #[test]
    fn identity_plan_maps_every_slot_to_itself() {
        let plan = LocalSlotPlan::identity(4);
        assert_eq!(plan.frame_local_slots(), 4);
        for slot in 0..4 {
            assert_eq!(plan.frame_slot(slot), slot);
        }
        assert!(!plan.shares_any_slot());
    }

    /// A zero-slot (ZST) local reserves no cells, so the CFG verifier lets a
    /// reference sit at `num_locals` itself — including slot 0 of a function
    /// whose only local is zero-sized. Mapping such a slot must not be an
    /// out-of-range panic (it ICE'd the `[MustUse; 0]` CLI case).
    #[test]
    fn zero_slot_locals_past_the_area_keep_their_slot_number() {
        let empty = LocalSlotPlan::identity(0);
        assert_eq!(empty.frame_slot(0), 0);
        let two = LocalSlotPlan::identity(2);
        assert_eq!(two.frame_slot(2), 2);
    }

    #[test]
    fn bitset_round_trips() {
        let mut set = BitSet::new(130);
        set.set(0);
        set.set(64);
        set.set(129);
        assert_eq!(set.ones().collect::<Vec<_>>(), [0, 64, 129]);
        set.clear(64);
        assert_eq!(set.ones().collect::<Vec<_>>(), [0, 129]);
        let mut other = BitSet::new(130);
        other.set(5);
        other.union_with(&set);
        assert_eq!(other.ones().collect::<Vec<_>>(), [0, 5, 129]);
    }
}
