//! CFG structural verifier (RUE-227).
//!
//! After the CFG is built (and optimized) but before it is lowered to machine
//! code, this pass asserts the structural invariants that codegen relies on.
//! Its whole purpose is to convert a *class* of latent lowering bugs — most
//! notoriously "block has no terminator", which historically surfaced as a
//! `SIGABRT` deep inside `cfg_lower.rs` (RUE-213 / RUE-217 / RUE-224) — into a
//! **loud, early, localized** compiler-bug report naming the function and the
//! offending block.
//!
//! # Why a hard panic (not a `debug_assert`)
//!
//! A CFG that violates these invariants is malformed AIR→CFG output: continuing
//! anyway miscompiles the program (wrong ABI, double-free, silent corruption).
//! Per RUE-45 the guard must fire in **release** builds too, so this uses
//! `panic!`/`assert!`, never `debug_assert!`.
//!
//! # Invariants checked
//!
//! Every arena value has exactly one legal attachment, block parameters agree
//! with their attachment metadata, and every operand is defined before use and
//! dominated by its definition. Targets, variable-length storage slices,
//! local/parameter slots, places, projections, edge arguments, conditions, and
//! returns are validated before any getter or graph traversal can index them.
//! Once those structural preconditions hold, a forward dataflow pass verifies
//! explicit storage lifetimes, explicit Drop consumption, and initialization
//! of unannotated compiler-owned slots such as runtime drop flags.
//!
//! Strict publication checks apply to unreachable blocks too. Their sole
//! reachability exemption is an unreachable block's `None` terminator:
//! construction can leave an orphan block unfinished, but all of its existing
//! contents still have to be structurally valid. The mid-optimization
//! materialization mode separately skips unreachable pre-DCE husks, which can
//! retain stale edges while simplifying the live graph. Semantic dataflow
//! checks only model reachable execution paths. Optimization runs strict
//! verification before DCE can detach dead arena values, then verifies the
//! remaining live graph again after all passes.

use crate::PayloadError;
use crate::dominators::DominatorTree;
use crate::inst::{
    BlockId, Cfg, CfgInstData, CfgValue, Place, PlaceBase, Projection, Terminator, ValidatedCfg,
};
use rue_air::{FrozenTypeInternPool, Type, TypeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OwnerRoot {
    Local { slot: u32, ty: Type },
    OwnedParam { slot: u32, ty: Type },
    WritableParam { slot: u32, ty: Type },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootFact {
    Unresolved,
    Unknown,
    Known(OwnerRoot),
}

const SEMANTIC_STATE_A: u8 = 1;
const SEMANTIC_STATE_B: u8 = 2;

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
struct SemanticWork {
    fact_solves: usize,
    peak_binary_state_slots: usize,
    block_visits: usize,
    edge_visits: usize,
    validation_instruction_visits: usize,
    instruction_operand_visits: usize,
    terminator_operand_visits: usize,
    root_nodes: usize,
    root_edges: usize,
    root_updates: usize,
    root_dependency_visits: usize,
}

#[cfg(test)]
std::thread_local! {
    static SEMANTIC_WORK: std::cell::RefCell<SemanticWork> = const {
        std::cell::RefCell::new(SemanticWork {
            fact_solves: 0,
            peak_binary_state_slots: 0,
            block_visits: 0,
            edge_visits: 0,
            validation_instruction_visits: 0,
            instruction_operand_visits: 0,
            terminator_operand_visits: 0,
            root_nodes: 0,
            root_edges: 0,
            root_updates: 0,
            root_dependency_visits: 0,
        })
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfgVerificationLocation {
    Artifact,
    Instruction { block: BlockId, value: CfgValue },
    Terminator { block: BlockId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgVerificationError {
    function: String,
    location: CfgVerificationLocation,
    message: String,
    payload: Option<PayloadError>,
}

impl CfgVerificationError {
    pub fn location(&self) -> CfgVerificationLocation {
        self.location
    }

    pub fn payload(&self) -> Option<&PayloadError> {
        self.payload.as_ref()
    }
}

impl std::fmt::Display for CfgVerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CFG verification failed in `{}`: {}",
            self.function, self.message
        )
    }
}

impl std::error::Error for CfgVerificationError {}

impl Cfg {
    /// Consume an editor and publish it only after whole-owner verification.
    pub fn finish(
        self,
        type_pool: &FrozenTypeInternPool,
    ) -> Result<ValidatedCfg, CfgVerificationError> {
        self.verify_with_type_pool(type_pool)?;
        Ok(ValidatedCfg(self))
    }

    /// Publish a domain-remapped optimized graph. Optimization deliberately
    /// retains detached dead arena values after removing their block
    /// attachments, so durable import must use the same post-optimization
    /// verification contract as the optimizer itself.
    pub fn finish_after_optimization(
        self,
        type_pool: &FrozenTypeInternPool,
    ) -> Result<ValidatedCfg, CfgVerificationError> {
        self.verify_after_optimization_with_type_pool(type_pool)?;
        Ok(ValidatedCfg(self))
    }

    /// Count every operand use of `needle` across instructions and
    /// terminators.
    ///
    /// The instruction walk mirrors the verifier's exhaustive operand list so
    /// provenance-sensitive consumers retain the established counting behavior.
    pub fn value_use_count(&self, needle: CfgValue) -> usize {
        let mut count = 0;
        for block in self.blocks() {
            for &value in &block.insts {
                let mut operands = Vec::new();
                self.collect_inst_operands(&self.get_inst(value).data, &mut operands);
                count += operands
                    .iter()
                    .filter(|operand| **operand == needle)
                    .count();
            }
            match &block.terminator {
                Terminator::Goto { args: _, .. } => {
                    count += self
                        .get_goto_args(&block.terminator)
                        .iter()
                        .filter(|value| **value == needle)
                        .count();
                }
                Terminator::Branch {
                    cond,
                    then_args: _,
                    else_args: _,
                    ..
                } => {
                    count += usize::from(*cond == needle);
                    count += self
                        .get_branch_then_args(&block.terminator)
                        .iter()
                        .filter(|value| **value == needle)
                        .count();
                    count += self
                        .get_branch_else_args(&block.terminator)
                        .iter()
                        .filter(|value| **value == needle)
                        .count();
                }
                Terminator::Switch { scrutinee, .. } => {
                    count += usize::from(*scrutinee == needle);
                }
                Terminator::Return { value } => {
                    count += usize::from(*value == Some(needle));
                }
                Terminator::Unreachable | Terminator::None => {}
            }
        }
        count
    }

    /// Verify the CFG's structural invariants, panicking with a precise,
    /// function- and block-localized message on any violation.
    ///
    /// See the module docs for the invariants and the rationale for the hard
    /// panic. This is a compiler-bug guard: a well-formed pipeline never trips
    /// it; the check is paid to pin future lowering regressions to their source.
    pub fn verify(&self) -> Result<(), CfgVerificationError> {
        Verifier::new(self, None, true).verify()
    }

    /// Verify with the active semantic type pool, enabling exact aggregate
    /// layouts and projection-chain validation. Production callers must use
    /// this entry point.
    pub fn verify_with_type_pool(
        &self,
        type_pool: &FrozenTypeInternPool,
    ) -> Result<(), CfgVerificationError> {
        Verifier::new(self, Some(type_pool), true).verify()
    }

    /// Verify the optimized live graph. DCE intentionally retains dead values
    /// in the arena after detaching them from blocks, so attachment completeness
    /// is checked before optimization while every remaining attachment and use
    /// is checked again afterward.
    pub(crate) fn verify_after_optimization_with_type_pool(
        &self,
        type_pool: &FrozenTypeInternPool,
    ) -> Result<(), CfgVerificationError> {
        Verifier::new(self, Some(type_pool), false).verify()
    }

    /// Verify a CFG edit whose only intended effect is on blocks reachable from
    /// the entry, tolerating the pre-DCE husks that an in-progress optimization
    /// pipeline leaves in unreachable blocks.
    ///
    /// Like [`Self::verify_after_optimization_with_type_pool`] this tolerates the
    /// detached-but-in-arena dead values that `forward`/`cse` leave for DCE, and
    /// it additionally skips unreachable blocks entirely so a stale husk edge
    /// (an unreachable `goto`/`branch` whose argument arity no longer matches its
    /// target after `simplify` folded a merge parameter away) is not mistaken for
    /// a bug in the edit under test. The newly materialized blocks — an LICM
    /// preheader and the loop it feeds — are reachable, so real materialization
    /// defects (bad arity, ill-typed or dominance-violating edges, a missing
    /// terminator) are still caught.
    pub(crate) fn verify_materialization_with_type_pool(
        &self,
        type_pool: &FrozenTypeInternPool,
    ) -> Result<(), CfgVerificationError> {
        Verifier::materialization(self, Some(type_pool)).verify()
    }

    /// Collect every `CfgValue` operand referenced by an instruction into
    /// `out`. This mirrors the `CfgInstData` variants; a new variant with value
    /// operands must be added here so the verifier keeps seeing all references.
    fn collect_inst_operands(&self, data: &CfgInstData, out: &mut Vec<CfgValue>) {
        match data {
            // No value operands.
            CfgInstData::Const(_)
            | CfgInstData::BoolConst(_)
            | CfgInstData::StringConst(_)
            | CfgInstData::Param { .. }
            | CfgInstData::BlockParam { .. }
            | CfgInstData::Load { .. }
            | CfgInstData::StorageLive { .. }
            | CfgInstData::StorageDead { .. } => {}

            // Binary operations.
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
                out.push(*a);
                out.push(*b);
            }

            // Unary operations.
            CfgInstData::Neg(v) | CfgInstData::Not(v) | CfgInstData::BitNot(v) => out.push(*v),

            CfgInstData::Alloc { init, .. } => out.push(*init),
            CfgInstData::Store { value, .. } => out.push(*value),
            CfgInstData::ParamStore { value, .. } => out.push(*value),

            CfgInstData::PlaceRead { place } => self.collect_place_operands(place, out),
            CfgInstData::PlaceWrite { place, value } => {
                self.collect_place_operands(place, out);
                out.push(*value);
            }

            CfgInstData::Call { args, .. } | CfgInstData::AccessorCall { args, .. } => {
                for arg in self.call_args(args) {
                    out.push(arg.value);
                }
            }
            CfgInstData::Intrinsic { args, .. } => out.extend_from_slice(self.intrinsic_args(args)),

            CfgInstData::StructInit { fields, .. } => {
                out.extend_from_slice(self.struct_fields(fields))
            }
            CfgInstData::ArrayInit { elements } => {
                out.extend_from_slice(self.array_elements(elements))
            }
            CfgInstData::EnumVariant { payload, .. } => {
                out.extend_from_slice(self.enum_payload(payload))
            }
            CfgInstData::EnumPayloadGet { base, .. } => out.push(*base),

            CfgInstData::IntCast { value, .. } => out.push(*value),
            CfgInstData::Drop { value } => out.push(*value),
        }
    }

    /// Collect the `CfgValue` operands hiding inside a place's `Index`
    /// projections.
    fn collect_place_operands(&self, place: &crate::inst::Place, out: &mut Vec<CfgValue>) {
        if let PlaceBase::Accessor(value) | PlaceBase::Indirect(value) = place.base {
            out.push(value);
        }
        for proj in self.get_place_projections(place) {
            if let Projection::Index { index, .. } = proj {
                out.push(*index);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Attachment {
    Param { block: BlockId },
    Inst { block: BlockId, position: usize },
}

struct Verifier<'a> {
    cfg: &'a Cfg,
    type_pool: Option<&'a FrozenTypeInternPool>,
    require_complete_attachments: bool,
    skip_unreachable_blocks: bool,
    attachments: Vec<Option<Attachment>>,
    /// Reachability and dominance for the graph under verification.
    ///
    /// `None` until [`Verifier::verify`] has cleared the structural checks that
    /// make terminator decoding safe — the tree walks every terminator, so it
    /// may only be built once targets are known to be in bounds and payload
    /// slices are known to be valid.
    dominators: Option<DominatorTree>,
    /// Test-only authority injection. A deliberately divergent answer proves
    /// the verifier's slot-range decisions consume the frozen-pool query
    /// instead of a shadow decomposition.
    #[cfg(test)]
    abi_slot_query_override:
        Option<fn(&FrozenTypeInternPool, Type) -> Result<u32, rue_air::TypeValidationError>>,
}

impl<'a> Verifier<'a> {
    fn new(
        cfg: &'a Cfg,
        type_pool: Option<&'a FrozenTypeInternPool>,
        require_complete_attachments: bool,
    ) -> Self {
        Self {
            cfg,
            type_pool,
            require_complete_attachments,
            skip_unreachable_blocks: false,
            attachments: vec![None; cfg.value_count()],
            dominators: None,
            #[cfg(test)]
            abi_slot_query_override: None,
        }
    }

    /// A verifier that ignores blocks unreachable from `cfg.entry`.
    ///
    /// Structural, dominance, and type checks still run against every block
    /// reachable from the entry, so a genuinely malformed graph among the live
    /// blocks is still rejected. This is *only* for mid-pipeline materialization
    /// checks (RUE-927 LICM preheaders): between `simplify` and the pipeline's
    /// final DCE the CFG legitimately carries husk blocks — unreachable blocks
    /// left with stale terminators and edge arguments (for example a folded
    /// `if`'s dead `else` still holding a `goto merge([arg])` after the merge
    /// block's parameter was substituted away). The final `finish_after_optimization`
    /// still sweeps those husks under strict verification once DCE has run.
    fn materialization(cfg: &'a Cfg, type_pool: Option<&'a FrozenTypeInternPool>) -> Self {
        Self {
            skip_unreachable_blocks: true,
            ..Self::new(cfg, type_pool, false)
        }
    }

    /// Reachability and dominance for the graph under verification.
    ///
    /// Only reachable from the per-block sweep in [`Self::verify`], which runs
    /// after the tree is built.
    fn dominators(&self) -> &DominatorTree {
        self.dominators
            .as_ref()
            .expect("dominator tree is built before the per-block sweep queries it")
    }

    fn error(&self, message: impl std::fmt::Display) -> CfgVerificationError {
        CfgVerificationError {
            function: self.cfg.fn_name().to_string(),
            location: CfgVerificationLocation::Artifact,
            message: message.to_string(),
            payload: None,
        }
    }

    fn payload_error(
        &self,
        location: CfgVerificationLocation,
        error: PayloadError,
    ) -> CfgVerificationError {
        CfgVerificationError {
            function: self.cfg.fn_name().to_string(),
            location,
            message: error.to_string(),
            payload: Some(error),
        }
    }

    fn verify(mut self) -> Result<(), CfgVerificationError> {
        self.verify_block_table_and_attachments()?;
        self.verify_targets_and_slices()?;
        // Every target is in bounds and every payload slice is valid by this
        // point, so the shared dominator tree can decode the terminators.
        self.dominators = Some(DominatorTree::compute(self.cfg));

        for block in self.cfg.blocks() {
            let reachable = self.dominators().is_reachable(block.id);
            // Mid-pipeline materialization checks only reason about the live
            // graph: unreachable blocks may legitimately be pre-DCE husks with
            // stale terminators/edge arguments. Every other caller checks them.
            if self.skip_unreachable_blocks && !reachable {
                continue;
            }
            if reachable && matches!(block.terminator, Terminator::None) {
                return Err(self.error(format_args!(
                    "reachable block {} has no terminator",
                    block.id
                )));
            }

            for (position, &value) in block.insts.iter().enumerate() {
                self.verify_inst(block.id, position, value)?;
            }
            if !matches!(block.terminator, Terminator::None) {
                self.verify_terminator_use(block.id, &block.terminator)?;
            }
        }
        self.verify_semantic_dataflow()?;
        Ok(())
    }

    /// Verify the semantic ordering that is explicit in CFG, after structural
    /// verification has made all arena and payload reads safe.
    ///
    /// This intentionally does not reconstruct source ownership. In
    /// particular, a projected place may be partially initialized or moved and
    /// a whole-slot Load may be a copy or a move; CFG has no marker that would
    /// let this pass distinguish those cases soundly. Instead it proves four
    /// bounded invariants that every publication boundary preserves:
    ///
    /// * an exact logical `(slot, type)` storage region is live on every path at
    ///   each local access, and Live/Dead transitions alternate on every path;
    /// * a non-phi SSA instruction result, or an exact whole-owner
    ///   local/by-value-parameter root, is not used after an explicit Drop on
    ///   any path without a fresh dynamic definition or whole write;
    /// * an unannotated compiler-owned slot that is loaded (notably a runtime
    ///   drop flag) has first been initialized by Store/Alloc on every path;
    /// * every tracked nonzero-width storage region is dead at a normal Return.
    ///
    /// Unreachable blocks have no runtime path and are deliberately excluded.
    /// Post-DCE detached arena values are absent from block instruction lists,
    /// so post-optimization verification naturally ignores them too.
    fn verify_semantic_dataflow(&self) -> Result<(), CfgVerificationError> {
        use ahash::AHashSet;

        let mut storage_regions = AHashSet::<(u32, Type)>::new();
        let mut storage_keys = Vec::new();
        let mut droppable_value_set = AHashSet::<CfgValue>::new();
        let mut droppable_values = Vec::new();
        let mut static_roots = vec![RootFact::Unknown; self.cfg.value_count()];

        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            for &(parameter, _) in &block.params {
                static_roots[parameter.as_u32() as usize] = RootFact::Unresolved;
            }
            for &value in &block.insts {
                let inst = self.cfg.get_inst(value);
                match inst.data {
                    CfgInstData::StorageLive { slot, local_ty }
                    | CfgInstData::StorageDead { slot, local_ty } => {
                        // Zero-width locals can share a slot and type while
                        // distinct lexical regions overlap. CFG has no
                        // declaration identity with which to separate them.
                        if self.abi_slot_count(
                            local_ty,
                            block.id,
                            value,
                            "semantic storage marker",
                        )? != 0
                            && storage_regions.insert((slot, local_ty))
                        {
                            storage_keys.push((slot, local_ty));
                        }
                    }
                    CfgInstData::Drop { value: dropped } => {
                        if let Some(pool) = self.type_pool {
                            let dropped_ty = self.cfg.get_inst(dropped).ty;
                            // Validate nominal identities before recursive drop
                            // queries so malformed CFG returns a typed error.
                            self.abi_slot_count(dropped_ty, block.id, value, "Drop operand")?;
                            if pool.type_needs_drop(dropped_ty)
                                && droppable_value_set.insert(dropped)
                            {
                                droppable_values.push(dropped);
                            }
                        }
                    }
                    _ => {}
                }
                if let Some(root) = self.instruction_owner_root(block.id, value)? {
                    static_roots[value.as_u32() as usize] = RootFact::Known(root);
                }
            }
        }

        let value_roots = self.resolve_owner_roots(static_roots);
        let mut drop_roots = vec![None; self.cfg.value_count()];
        let mut owner_roots = Vec::new();
        let mut owner_root_set = AHashSet::new();
        for &value in &droppable_values {
            if let Some(root) = value_roots[value.as_u32() as usize] {
                drop_roots[value.as_u32() as usize] = Some(root);
                if owner_root_set.insert(root) {
                    owner_roots.push(root);
                }
            }
        }

        // Every reachable Load outside a declared storage region is a
        // compiler-owned raw channel. A missing Store/Alloc is itself the bug,
        // so discovery never depends on whether a write survived.
        let mut raw_slots = AHashSet::<(u32, Type)>::new();
        let mut raw_keys = Vec::new();
        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            for &value in &block.insts {
                let inst = self.cfg.get_inst(value);
                if let CfgInstData::Load { slot } = inst.data {
                    let key = (slot, inst.ty);
                    if !storage_regions.contains(&key) && raw_slots.insert(key) {
                        raw_keys.push(key);
                    }
                }
            }
        }

        // Each fact is solved independently with one reusable two-bit state per
        // block. The peak path-state memory is O(B), never O(B * F). Root
        // provenance uses a dependency worklist whose nodes change at most
        // twice, so its time is O(P + A), where A is incoming phi arguments.
        // If O is the number of instruction operand references and T is the
        // number of terminator operand references (including edge arguments),
        // overall semantic time is
        // O(B + E + I + P + A + F * (B + E + I + O + T)). Auxiliary memory is
        // O(B + V + P + A + F); no state dimension is multiplied by F.
        for key in storage_keys {
            self.verify_storage_fact(key)?;
        }
        for key in raw_keys {
            self.verify_raw_init_fact(key)?;
        }
        for &value in &droppable_values {
            self.verify_exact_drop_fact(value)?;
        }
        for root in owner_roots {
            self.verify_owner_root_fact(root, &drop_roots, &value_roots)?;
        }
        Ok(())
    }

    fn instruction_owner_root(
        &self,
        block: BlockId,
        value: CfgValue,
    ) -> Result<Option<OwnerRoot>, CfgVerificationError> {
        let inst = self.cfg.get_inst(value);
        let root = match &inst.data {
            CfgInstData::Load { slot }
                if self.abi_slot_count(inst.ty, block, value, "owner root")? != 0 =>
            {
                Some(OwnerRoot::Local {
                    slot: *slot,
                    ty: inst.ty,
                })
            }
            CfgInstData::PlaceRead { place }
                if (place.as_local().is_some() || place.as_param().is_some())
                    && self.abi_slot_count(place.base_type, block, value, "owner root")? != 0 =>
            {
                self.place_owner_root(place)
            }
            CfgInstData::Param { index }
                if self.abi_slot_count(inst.ty, block, value, "owner root")? != 0 =>
            {
                self.param_owner_root(*index, inst.ty)
            }
            _ => None,
        };
        Ok(root)
    }

    fn param_owner_root(&self, slot: u32, ty: Type) -> Option<OwnerRoot> {
        if self.cfg.is_param_writable(slot) {
            Some(OwnerRoot::WritableParam { slot, ty })
        } else if !self.cfg.is_param_by_ref(slot) {
            Some(OwnerRoot::OwnedParam { slot, ty })
        } else {
            // A shared, non-writable borrow is not an owned root.
            None
        }
    }

    fn place_owner_root(&self, place: &Place) -> Option<OwnerRoot> {
        match place.base {
            PlaceBase::Local(slot) => Some(OwnerRoot::Local {
                slot,
                ty: place.base_type,
            }),
            PlaceBase::Param(slot) => self.param_owner_root(slot, place.base_type),
            PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => None,
        }
    }

    fn whole_write_root(&self, data: &CfgInstData) -> Option<OwnerRoot> {
        match data {
            CfgInstData::Store { slot, value } => Some(OwnerRoot::Local {
                slot: *slot,
                ty: self.cfg.get_inst(*value).ty,
            }),
            CfgInstData::Alloc { slot, init } => Some(OwnerRoot::Local {
                slot: *slot,
                ty: self.cfg.get_inst(*init).ty,
            }),
            CfgInstData::ParamStore { param_slot, value } => {
                self.param_owner_root(*param_slot, self.cfg.get_inst(*value).ty)
            }
            CfgInstData::PlaceWrite { place, .. } if place.as_local().is_some() => {
                Some(OwnerRoot::Local {
                    slot: place.as_local().unwrap(),
                    ty: place.base_type,
                })
            }
            CfgInstData::PlaceWrite { place, .. } if place.as_param().is_some() => {
                self.param_owner_root(place.as_param().unwrap(), place.base_type)
            }
            _ => None,
        }
    }

    fn merge_root_fact(current: RootFact, incoming: RootFact) -> RootFact {
        match (current, incoming) {
            (RootFact::Unknown, _) | (_, RootFact::Unknown) => RootFact::Unknown,
            (current, RootFact::Unresolved) => current,
            (RootFact::Unresolved, incoming) => incoming,
            (RootFact::Known(left), RootFact::Known(right)) if left == right => {
                RootFact::Known(left)
            }
            (RootFact::Known(_), RootFact::Known(_)) => RootFact::Unknown,
        }
    }

    fn resolve_owner_roots(&self, static_roots: Vec<RootFact>) -> Vec<Option<OwnerRoot>> {
        use std::collections::VecDeque;

        let mut param_values = Vec::new();
        let mut param_index = vec![None; self.cfg.value_count()];
        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            for &(value, _) in &block.params {
                let index = param_values.len();
                param_values.push(value);
                param_index[value.as_u32() as usize] = Some(index);
            }
        }

        let mut states = vec![RootFact::Unresolved; param_values.len()];
        let mut dependents = vec![Vec::<usize>::new(); param_values.len()];
        let mut queue = VecDeque::new();
        let mut queued = vec![false; param_values.len()];

        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            self.for_each_semantic_edge(block.id, |target, args| {
                let target_block = self.cfg.get_block(target);
                for (position, &(parameter, _)) in target_block.params.iter().enumerate() {
                    let target_index =
                        param_index[parameter.as_u32() as usize].expect("reachable phi index");
                    let argument = args[position];
                    #[cfg(test)]
                    SEMANTIC_WORK.with(|work| work.borrow_mut().root_edges += 1);
                    if let Some(source_index) = param_index[argument.as_u32() as usize] {
                        dependents[source_index].push(target_index);
                    } else {
                        let incoming = static_roots[argument.as_u32() as usize];
                        let next = Self::merge_root_fact(states[target_index], incoming);
                        if next != states[target_index] {
                            states[target_index] = next;
                            #[cfg(test)]
                            SEMANTIC_WORK.with(|work| work.borrow_mut().root_updates += 1);
                            if !queued[target_index] {
                                queued[target_index] = true;
                                queue.push_back(target_index);
                            }
                        }
                    }
                }
            });
        }

        #[cfg(test)]
        SEMANTIC_WORK.with(|work| work.borrow_mut().root_nodes += param_values.len());

        // First propagate anchored roots and conflicts. Then classify pure
        // unanchored SCCs as Unknown and propagate that result downstream.
        for phase in 0..2 {
            while let Some(source) = queue.pop_front() {
                queued[source] = false;
                let incoming = states[source];
                for &target in &dependents[source] {
                    #[cfg(test)]
                    SEMANTIC_WORK.with(|work| {
                        work.borrow_mut().root_dependency_visits += 1;
                    });
                    let next = Self::merge_root_fact(states[target], incoming);
                    if next != states[target] {
                        states[target] = next;
                        #[cfg(test)]
                        SEMANTIC_WORK.with(|work| work.borrow_mut().root_updates += 1);
                        if !queued[target] {
                            queued[target] = true;
                            queue.push_back(target);
                        }
                    }
                }
            }
            if phase == 0 {
                for (index, state) in states.iter_mut().enumerate() {
                    if *state == RootFact::Unresolved {
                        *state = RootFact::Unknown;
                        #[cfg(test)]
                        SEMANTIC_WORK.with(|work| work.borrow_mut().root_updates += 1);
                        queued[index] = true;
                        queue.push_back(index);
                    }
                }
            }
        }

        let mut roots = static_roots
            .into_iter()
            .map(|fact| match fact {
                RootFact::Known(root) => Some(root),
                RootFact::Unresolved | RootFact::Unknown => None,
            })
            .collect::<Vec<_>>();
        for (index, value) in param_values.into_iter().enumerate() {
            roots[value.as_u32() as usize] = match states[index] {
                RootFact::Known(root) => Some(root),
                RootFact::Unresolved | RootFact::Unknown => None,
            };
        }
        roots
    }

    fn solve_semantic_fact(&self, mut transfer: impl FnMut(BlockId, u8) -> u8) -> Vec<u8> {
        use std::collections::VecDeque;

        let block_count = self.cfg.block_count();
        let mut inputs = vec![0u8; block_count];
        let mut queued = vec![false; block_count];
        let mut queue = VecDeque::new();
        let entry = self.cfg.entry.as_u32() as usize;
        inputs[entry] = SEMANTIC_STATE_A;
        queued[entry] = true;
        queue.push_back(self.cfg.entry);

        #[cfg(test)]
        SEMANTIC_WORK.with(|work| {
            let mut work = work.borrow_mut();
            work.fact_solves += 1;
            work.peak_binary_state_slots = work
                .peak_binary_state_slots
                .max(block_count * 2 + queue.len());
        });

        while let Some(block) = queue.pop_front() {
            let index = block.as_u32() as usize;
            queued[index] = false;
            #[cfg(test)]
            SEMANTIC_WORK.with(|work| work.borrow_mut().block_visits += 1);
            let output = transfer(block, inputs[index]);
            self.for_each_semantic_edge(block, |target, _| {
                if !self.dominators().is_reachable(target) {
                    return;
                }
                #[cfg(test)]
                SEMANTIC_WORK.with(|work| work.borrow_mut().edge_visits += 1);
                let target_index = target.as_u32() as usize;
                let merged = inputs[target_index] | output;
                if merged != inputs[target_index] {
                    inputs[target_index] = merged;
                    if !queued[target_index] {
                        queued[target_index] = true;
                        queue.push_back(target);
                        #[cfg(test)]
                        SEMANTIC_WORK.with(|work| {
                            let mut work = work.borrow_mut();
                            work.peak_binary_state_slots = work
                                .peak_binary_state_slots
                                .max(block_count * 2 + queue.len());
                        });
                    }
                }
            });
        }
        inputs
    }

    fn for_each_semantic_edge(&self, block: BlockId, mut f: impl FnMut(BlockId, &[CfgValue])) {
        let terminator = &self.cfg.get_block(block).terminator;
        match terminator {
            Terminator::Goto { target, .. } => {
                f(*target, self.cfg.get_goto_args(terminator));
            }
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                f(*then_block, self.cfg.get_branch_then_args(terminator));
                f(*else_block, self.cfg.get_branch_else_args(terminator));
            }
            Terminator::Switch { cases, default, .. } => {
                for &(_, target) in self.cfg.switch_cases(cases) {
                    f(target, &[]);
                }
                f(*default, &[]);
            }
            Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
        }
    }

    fn for_each_terminator_operand(
        &self,
        block: BlockId,
        mut f: impl FnMut(CfgValue, &'static str),
    ) {
        let terminator = &self.cfg.get_block(block).terminator;
        match terminator {
            Terminator::Goto { .. } => {
                for &argument in self.cfg.get_goto_args(terminator) {
                    f(argument, "goto argument");
                }
            }
            Terminator::Branch { cond, .. } => {
                f(*cond, "branch condition");
                for &argument in self.cfg.get_branch_then_args(terminator) {
                    f(argument, "branch-then argument");
                }
                for &argument in self.cfg.get_branch_else_args(terminator) {
                    f(argument, "branch-else argument");
                }
            }
            Terminator::Switch { scrutinee, .. } => f(*scrutinee, "switch scrutinee"),
            Terminator::Return { value } => {
                if let Some(value) = value {
                    f(*value, "return value");
                }
            }
            Terminator::Unreachable | Terminator::None => {}
        }
    }

    fn local_storage_access(&self, data: &CfgInstData, result_ty: Type) -> Option<(u32, Type)> {
        match data {
            CfgInstData::Alloc { slot, init } => Some((*slot, self.cfg.get_inst(*init).ty)),
            CfgInstData::Load { slot } => Some((*slot, result_ty)),
            CfgInstData::Store { slot, value } => Some((*slot, self.cfg.get_inst(*value).ty)),
            CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
                match place.base {
                    PlaceBase::Local(slot) => Some((slot, place.base_type)),
                    PlaceBase::Param(_) | PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => None,
                }
            }
            _ => None,
        }
    }

    fn verify_storage_fact(&self, key: (u32, Type)) -> Result<(), CfgVerificationError> {
        const DEAD: u8 = SEMANTIC_STATE_A;
        const LIVE: u8 = SEMANTIC_STATE_B;

        let inputs = self.solve_semantic_fact(|block, mut state| {
            for &value in &self.cfg.get_block(block).insts {
                match self.cfg.get_inst(value).data {
                    CfgInstData::StorageLive { slot, local_ty } if (slot, local_ty) == key => {
                        state = LIVE;
                    }
                    CfgInstData::StorageDead { slot, local_ty } if (slot, local_ty) == key => {
                        state = DEAD;
                    }
                    _ => {}
                }
            }
            state
        });

        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            let mut state = inputs[block.id.as_u32() as usize];
            for &value in &block.insts {
                #[cfg(test)]
                SEMANTIC_WORK.with(|work| {
                    work.borrow_mut().validation_instruction_visits += 1;
                });
                let inst = self.cfg.get_inst(value);
                let location = CfgVerificationLocation::Instruction {
                    block: block.id,
                    value,
                };
                if self.local_storage_access(&inst.data, inst.ty) == Some(key) && state != LIVE {
                    return Err(self.semantic_error(
                        location,
                        format_args!(
                            "instruction {} in block {} accesses local storage ({}, {:?}) that is not live on every reaching path",
                            value, block.id, key.0, key.1
                        ),
                    ));
                }
                match inst.data {
                    CfgInstData::StorageLive { slot, local_ty } if (slot, local_ty) == key => {
                        if state != DEAD {
                            return Err(self.semantic_error(
                                location,
                                format_args!(
                                    "StorageLive instruction {} in block {} starts local storage ({}, {:?}) that is not dead on every reaching path",
                                    value, block.id, slot, local_ty
                                ),
                            ));
                        }
                        state = LIVE;
                    }
                    CfgInstData::StorageDead { slot, local_ty } if (slot, local_ty) == key => {
                        if state != LIVE {
                            return Err(self.semantic_error(
                                location,
                                format_args!(
                                    "StorageDead instruction {} in block {} ends local storage ({}, {:?}) that is not live on every reaching path",
                                    value, block.id, slot, local_ty
                                ),
                            ));
                        }
                        state = DEAD;
                    }
                    _ => {}
                }
            }
            if matches!(block.terminator, Terminator::Return { .. }) && state & LIVE != 0 {
                return Err(self.semantic_error(
                    CfgVerificationLocation::Terminator { block: block.id },
                    format_args!(
                        "return in block {} leaves local storage ({}, {:?}) live on a reaching path",
                        block.id, key.0, key.1
                    ),
                ));
            }
        }
        Ok(())
    }

    fn verify_raw_init_fact(&self, key: (u32, Type)) -> Result<(), CfgVerificationError> {
        const UNINITIALIZED: u8 = SEMANTIC_STATE_A;
        const INITIALIZED: u8 = SEMANTIC_STATE_B;

        let inputs = self.solve_semantic_fact(|block, mut state| {
            for &value in &self.cfg.get_block(block).insts {
                match self.cfg.get_inst(value).data {
                    CfgInstData::Store {
                        slot,
                        value: stored,
                    } if (slot, self.cfg.get_inst(stored).ty) == key => {
                        state = INITIALIZED;
                    }
                    CfgInstData::Alloc { slot, init }
                        if (slot, self.cfg.get_inst(init).ty) == key =>
                    {
                        state = INITIALIZED;
                    }
                    _ => {}
                }
            }
            state
        });

        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            let mut state = inputs[block.id.as_u32() as usize];
            for &value in &block.insts {
                #[cfg(test)]
                SEMANTIC_WORK.with(|work| {
                    work.borrow_mut().validation_instruction_visits += 1;
                });
                let inst = self.cfg.get_inst(value);
                if let CfgInstData::Load { slot } = inst.data
                    && (slot, inst.ty) == key
                    && state & UNINITIALIZED != 0
                {
                    return Err(self.semantic_error(
                        CfgVerificationLocation::Instruction {
                            block: block.id,
                            value,
                        },
                        format_args!(
                            "instruction {} in block {} loads unannotated local storage ({}, {:?}) before it is initialized on every reaching path",
                            value, block.id, slot, inst.ty
                        ),
                    ));
                }
                match inst.data {
                    CfgInstData::Store {
                        slot,
                        value: stored,
                    } if (slot, self.cfg.get_inst(stored).ty) == key => {
                        state = INITIALIZED;
                    }
                    CfgInstData::Alloc { slot, init }
                        if (slot, self.cfg.get_inst(init).ty) == key =>
                    {
                        state = INITIALIZED;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn verify_exact_drop_fact(&self, target: CfgValue) -> Result<(), CfgVerificationError> {
        const FRESH: u8 = SEMANTIC_STATE_A;
        const CONSUMED: u8 = SEMANTIC_STATE_B;

        let inputs = self.solve_semantic_fact(|block, mut state| {
            if self
                .cfg
                .get_block(block)
                .params
                .iter()
                .any(|&(parameter, _)| parameter == target)
            {
                state = FRESH;
            }
            for &value in &self.cfg.get_block(block).insts {
                if value == target {
                    state = FRESH;
                }
                if let CfgInstData::Drop { value: dropped } = self.cfg.get_inst(value).data
                    && dropped == target
                {
                    state = CONSUMED;
                }
            }
            state
        });

        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            let mut state = inputs[block.id.as_u32() as usize];
            if block
                .params
                .iter()
                .any(|&(parameter, _)| parameter == target)
            {
                state = FRESH;
            }
            for &value in &block.insts {
                #[cfg(test)]
                SEMANTIC_WORK.with(|work| {
                    work.borrow_mut().validation_instruction_visits += 1;
                });
                if value == target {
                    state = FRESH;
                }
                let inst = self.cfg.get_inst(value);
                let location = CfgVerificationLocation::Instruction {
                    block: block.id,
                    value,
                };
                if let CfgInstData::Drop { value: dropped } = inst.data
                    && dropped == target
                {
                    if state & CONSUMED != 0 {
                        return Err(self.semantic_error(
                            location,
                            format_args!(
                                "Drop instruction {} in block {} consumes {} after it was already dropped on a reaching path",
                                value, block.id, target
                            ),
                        ));
                    }
                    state = CONSUMED;
                } else {
                    let mut error = None;
                    self.for_each_inst_operand(block.id, value, &inst.data, |operand, role| {
                        #[cfg(test)]
                        SEMANTIC_WORK.with(|work| {
                            work.borrow_mut().instruction_operand_visits += 1;
                        });
                        if error.is_none() && operand == target && state & CONSUMED != 0 {
                            error = Some(self.semantic_error(
                                location,
                                format_args!(
                                    "{} {} in instruction {} in block {} was already dropped on a reaching path",
                                    role, operand, value, block.id
                                ),
                            ));
                        }
                    });
                    if let Some(error) = error {
                        return Err(error);
                    }
                }
            }
            let mut error = None;
            self.for_each_terminator_operand(block.id, |operand, role| {
                #[cfg(test)]
                SEMANTIC_WORK.with(|work| {
                    work.borrow_mut().terminator_operand_visits += 1;
                });
                if error.is_none() && operand == target && state & CONSUMED != 0 {
                    error = Some(self.semantic_error(
                        CfgVerificationLocation::Terminator { block: block.id },
                        format_args!(
                            "{} {} in terminator of block {} was already dropped on a reaching path",
                            role, operand, block.id
                        ),
                    ));
                }
            });
            if let Some(error) = error {
                return Err(error);
            }
        }
        Ok(())
    }

    fn verify_owner_root_fact(
        &self,
        root: OwnerRoot,
        drop_roots: &[Option<OwnerRoot>],
        value_roots: &[Option<OwnerRoot>],
    ) -> Result<(), CfgVerificationError> {
        const FRESH: u8 = SEMANTIC_STATE_A;
        const CONSUMED: u8 = SEMANTIC_STATE_B;

        let inputs = self.solve_semantic_fact(|block, mut state| {
            for &value in &self.cfg.get_block(block).insts {
                let inst = self.cfg.get_inst(value);
                if self.whole_write_root(&inst.data) == Some(root) {
                    state = FRESH;
                }
                if let CfgInstData::Drop { value: dropped } = inst.data
                    && drop_roots[dropped.as_u32() as usize] == Some(root)
                {
                    state = CONSUMED;
                }
            }
            state
        });

        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            let mut state = inputs[block.id.as_u32() as usize];
            for &(parameter, _) in &block.params {
                if value_roots[parameter.as_u32() as usize] == Some(root) && state & CONSUMED != 0 {
                    return Err(self.semantic_error(
                        CfgVerificationLocation::Artifact,
                        format_args!(
                            "block parameter {} in block {} carries already-consumed owner root {:?}",
                            parameter, block.id, root
                        ),
                    ));
                }
            }
            for &value in &block.insts {
                #[cfg(test)]
                SEMANTIC_WORK.with(|work| {
                    work.borrow_mut().validation_instruction_visits += 1;
                });
                let inst = self.cfg.get_inst(value);
                let location = CfgVerificationLocation::Instruction {
                    block: block.id,
                    value,
                };
                if value_roots[value.as_u32() as usize] == Some(root) && state & CONSUMED != 0 {
                    return Err(self.semantic_error(
                        location,
                        format_args!(
                            "instruction {} in block {} reads already-consumed owner root {:?}",
                            value, block.id, root
                        ),
                    ));
                }
                if let CfgInstData::PlaceRead { place } = &inst.data
                    && self.place_owner_root(place) == Some(root)
                    && state & CONSUMED != 0
                {
                    return Err(self.semantic_error(
                        location,
                        format_args!(
                            "instruction {} in block {} reads through already-consumed owner root {:?}",
                            value, block.id, root
                        ),
                    ));
                }
                if !matches!(inst.data, CfgInstData::Drop { .. }) {
                    let mut error = None;
                    self.for_each_inst_operand(block.id, value, &inst.data, |operand, role| {
                        #[cfg(test)]
                        SEMANTIC_WORK.with(|work| {
                            work.borrow_mut().instruction_operand_visits += 1;
                        });
                        if error.is_none()
                            && value_roots[operand.as_u32() as usize] == Some(root)
                            && state & CONSUMED != 0
                        {
                            error = Some(self.semantic_error(
                                location,
                                format_args!(
                                    "{} {} in instruction {} in block {} uses already-consumed owner root {:?}",
                                    role, operand, value, block.id, root
                                ),
                            ));
                        }
                    });
                    if let Some(error) = error {
                        return Err(error);
                    }
                }
                if let CfgInstData::Drop { value: dropped } = inst.data
                    && drop_roots[dropped.as_u32() as usize] == Some(root)
                {
                    if state & CONSUMED != 0 {
                        return Err(self.semantic_error(
                            location,
                            format_args!(
                                "Drop instruction {} in block {} consumes already-consumed owner root {:?}",
                                value, block.id, root
                            ),
                        ));
                    }
                    state = CONSUMED;
                }
                if self.whole_write_root(&inst.data) == Some(root) {
                    state = FRESH;
                }
            }
            let mut error = None;
            self.for_each_terminator_operand(block.id, |operand, role| {
                #[cfg(test)]
                SEMANTIC_WORK.with(|work| {
                    work.borrow_mut().terminator_operand_visits += 1;
                });
                if error.is_none()
                    && value_roots[operand.as_u32() as usize] == Some(root)
                    && state & CONSUMED != 0
                {
                    error = Some(self.semantic_error(
                        CfgVerificationLocation::Terminator { block: block.id },
                        format_args!(
                            "{} {} in terminator of block {} carries already-consumed owner root {:?}",
                            role, operand, block.id, root
                        ),
                    ));
                }
            });
            if let Some(error) = error {
                return Err(error);
            }
        }
        Ok(())
    }

    fn semantic_error(
        &self,
        location: CfgVerificationLocation,
        message: impl std::fmt::Display,
    ) -> CfgVerificationError {
        CfgVerificationError {
            function: self.cfg.fn_name().to_string(),
            location,
            message: message.to_string(),
            payload: None,
        }
    }

    fn verify_block_table_and_attachments(&mut self) -> Result<(), CfgVerificationError> {
        let block_count = self.cfg.block_count();
        if self.cfg.entry.as_u32() as usize >= block_count {
            return Err(self.error(format_args!(
                "entry block {} is out of bounds (only {} blocks exist)",
                self.cfg.entry, block_count
            )));
        }

        for (expected, block) in self.cfg.blocks().iter().enumerate() {
            if block.id.as_u32() as usize != expected {
                return Err(self.error(format_args!(
                    "block table slot {} contains mismatched id {}",
                    expected, block.id
                )));
            }
            for (index, &(value, stored_ty)) in block.params.iter().enumerate() {
                self.attach(
                    value,
                    Attachment::Param { block: block.id },
                    "block parameter",
                )?;
                let inst = self.inst(value, block.id, "block parameter")?;
                match inst.data {
                    CfgInstData::BlockParam { index: actual } if actual == index as u32 => {}
                    CfgInstData::BlockParam { index: actual } => return Err(self.error(format_args!(
                        "block parameter {} in block {} is stored at index {} but declares index {}",
                        value, block.id, index, actual
                    ))),
                    ref other => {
                        return Err(self.error(format_args!(
                        "block parameter {} in block {} has non-BlockParam data {:?}",
                        value, block.id, other
                    )))
                    }
                }
                if inst.ty != stored_ty {
                    return Err(self.error(format_args!(
                        "block parameter {} in block {} stores type {:?} but its value has type {:?}",
                        value, block.id, stored_ty, inst.ty
                    )));
                }
            }
            for (position, &value) in block.insts.iter().enumerate() {
                self.attach(
                    value,
                    Attachment::Inst {
                        block: block.id,
                        position,
                    },
                    "instruction",
                )?;
                if matches!(
                    self.inst(value, block.id, "instruction")?.data,
                    CfgInstData::BlockParam { .. }
                ) {
                    return Err(self.error(format_args!(
                        "ordinary instruction {} in block {} has BlockParam data",
                        value, block.id
                    )));
                }
            }
        }

        if self.require_complete_attachments {
            for (index, attachment) in self.attachments.iter().enumerate() {
                if attachment.is_none() {
                    return Err(self.error(format_args!(
                        "value v{} is unattached (every value must be exactly one block parameter or ordinary instruction)",
                        index
                    )));
                }
            }
        }
        Ok(())
    }

    fn attach(
        &mut self,
        value: CfgValue,
        attachment: Attachment,
        role: &str,
    ) -> Result<(), CfgVerificationError> {
        let index = value.as_u32() as usize;
        if index >= self.attachments.len() {
            return Err(self.error(format_args!(
                "{} {} references an undefined value (only {} values exist)",
                role,
                value,
                self.attachments.len()
            )));
        }
        if let Some(previous) = self.attachments[index] {
            return Err(self.error(format_args!(
                "value {} has duplicate attachments ({}, then {})",
                value,
                Self::attachment_name(previous),
                Self::attachment_name(attachment)
            )));
        }
        self.attachments[index] = Some(attachment);
        Ok(())
    }

    fn attachment_name(attachment: Attachment) -> String {
        match attachment {
            Attachment::Param { block } => format!("parameter in {block}"),
            Attachment::Inst { block, position } => {
                format!("instruction {position} in {block}")
            }
        }
    }

    fn inst(
        &self,
        value: CfgValue,
        block: BlockId,
        role: &str,
    ) -> Result<&crate::inst::CfgInst, CfgVerificationError> {
        if value.as_u32() as usize >= self.cfg.value_count() {
            return Err(self.error(format_args!(
                "{} {} in block {} is undefined (only {} values exist)",
                role,
                value,
                block,
                self.cfg.value_count()
            )));
        }
        Ok(self.cfg.get_inst(value))
    }

    fn verify_targets_and_slices(&self) -> Result<(), CfgVerificationError> {
        for block in self.cfg.blocks() {
            for &value in &block.insts {
                let data = &self.inst(value, block.id, "instruction")?.data;
                let location = CfgVerificationLocation::Instruction {
                    block: block.id,
                    value,
                };
                match data {
                    CfgInstData::Call { args, .. } | CfgInstData::AccessorCall { args, .. } => {
                        self.cfg
                            .checked_call_args(args)
                            .map_err(|error| self.payload_error(location, error))?;
                    }
                    CfgInstData::Intrinsic { args, .. } => {
                        self.cfg
                            .checked_intrinsic_args(args)
                            .map_err(|error| self.payload_error(location, error))?;
                    }
                    CfgInstData::StructInit { fields, .. } => {
                        self.cfg
                            .checked_struct_fields(fields)
                            .map_err(|error| self.payload_error(location, error))?;
                    }
                    CfgInstData::ArrayInit { elements } => {
                        self.cfg
                            .checked_array_elements(elements)
                            .map_err(|error| self.payload_error(location, error))?;
                    }
                    CfgInstData::EnumVariant { payload, .. } => {
                        self.cfg
                            .checked_enum_payload(payload)
                            .map_err(|error| self.payload_error(location, error))?;
                    }
                    CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
                        self.cfg
                            .checked_place_projections(place)
                            .map_err(|error| self.payload_error(location, error))?;
                    }
                    _ => {}
                }
            }
            let location = CfgVerificationLocation::Terminator { block: block.id };
            match &block.terminator {
                Terminator::Goto { target, args } => {
                    self.check_target(block.id, *target, "goto")?;
                    self.cfg
                        .checked_goto_args(args)
                        .map_err(|error| self.payload_error(location, error))?;
                }
                Terminator::Branch {
                    then_block,
                    then_args,
                    else_block,
                    else_args,
                    ..
                } => {
                    self.check_target(block.id, *then_block, "branch then")?;
                    self.check_target(block.id, *else_block, "branch else")?;
                    self.cfg
                        .checked_then_args(then_args)
                        .map_err(|error| self.payload_error(location, error))?;
                    self.cfg
                        .checked_else_args(else_args)
                        .map_err(|error| self.payload_error(location, error))?;
                }
                Terminator::Switch { cases, default, .. } => {
                    let cases = self
                        .cfg
                        .checked_switch_cases(cases)
                        .map_err(|error| self.payload_error(location, error))?;
                    for &(_, target) in cases {
                        self.check_target(block.id, target, "switch case")?;
                    }
                    self.check_target(block.id, *default, "switch default")?;
                }
                Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
            }
        }
        Ok(())
    }

    fn check_target(
        &self,
        from: BlockId,
        target: BlockId,
        role: &str,
    ) -> Result<(), CfgVerificationError> {
        if target.as_u32() as usize >= self.cfg.block_count() {
            return Err(self.error(format_args!(
                "{} target {} from block {} is out of bounds (only {} blocks exist)",
                role,
                target,
                from,
                self.cfg.block_count()
            )));
        }
        Ok(())
    }

    fn verify_inst(
        &self,
        block: BlockId,
        position: usize,
        value: CfgValue,
    ) -> Result<(), CfgVerificationError> {
        let inst = self.inst(value, block, "instruction")?;
        match &inst.data {
            CfgInstData::Param { index } => {
                self.check_param_slot(*index, inst.ty, block, value, "Param")?
            }
            CfgInstData::Alloc { slot, init } => self.check_local_slot(
                *slot,
                self.inst(*init, block, "allocation initializer")?.ty,
                block,
                value,
                "Alloc",
            )?,
            CfgInstData::Load { slot } => {
                self.check_local_slot(*slot, inst.ty, block, value, "Load")?
            }
            CfgInstData::Store {
                slot,
                value: stored,
            } => self.check_local_slot(
                *slot,
                self.inst(*stored, block, "stored value")?.ty,
                block,
                value,
                "Store",
            )?,
            CfgInstData::StorageLive { slot, local_ty } => {
                self.check_local_slot(*slot, *local_ty, block, value, "StorageLive")?
            }
            CfgInstData::StorageDead { slot, local_ty } => {
                self.check_local_slot(*slot, *local_ty, block, value, "StorageDead")?
            }
            CfgInstData::ParamStore {
                param_slot,
                value: stored,
            } => self.check_param_slot(
                *param_slot,
                self.inst(*stored, block, "stored parameter value")?.ty,
                block,
                value,
                "ParamStore",
            )?,
            CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
                self.verify_place(block, value, place)?
            }
            CfgInstData::BlockParam { .. } => unreachable!(),
            _ => {}
        }
        let mut operand_result = Ok(());
        self.for_each_inst_operand(block, value, &inst.data, |operand, role| {
            if operand_result.is_ok() {
                operand_result = self.verify_use(block, Some(position), operand, role);
            }
        });
        operand_result
    }

    fn check_local_slot(
        &self,
        slot: u32,
        ty: Type,
        block: BlockId,
        value: CfgValue,
        role: &str,
    ) -> Result<(), CfgVerificationError> {
        let width = self.abi_slot_count(ty, block, value, role)?;
        let end = slot.checked_add(width);
        if end.is_none_or(|end| end > self.cfg.num_locals()) {
            return Err(self.error(format_args!(
                "{} instruction {} in block {} uses local slot range {}..{} for type {:?}, but only {} local slots exist",
                role,
                value,
                block,
                slot,
                end.map_or_else(|| "overflow".to_string(), |end| end.to_string()),
                ty,
                self.cfg.num_locals()
            )));
        }
        Ok(())
    }

    fn check_param_slot(
        &self,
        slot: u32,
        ty: Type,
        block: BlockId,
        value: CfgValue,
        role: &str,
    ) -> Result<(), CfgVerificationError> {
        // Borrowed and inout parameters carry one physical pointer slot even
        // when their logical type is a multi-slot aggregate. Places retain the
        // logical type so projection validation can follow the pointee shape.
        let width = if self.cfg.is_param_by_ref(slot) {
            1
        } else {
            self.abi_slot_count(ty, block, value, role)?
        };
        let end = slot.checked_add(width);
        if end.is_none_or(|end| end > self.cfg.num_params()) {
            return Err(self.error(format_args!(
                "{} instruction {} in block {} uses parameter slot range {}..{} for type {:?}, but only {} parameter slots exist",
                role,
                value,
                block,
                slot,
                end.map_or_else(|| "overflow".to_string(), |end| end.to_string()),
                ty,
                self.cfg.num_params()
            )));
        }
        Ok(())
    }

    fn abi_slot_count(
        &self,
        ty: Type,
        block: BlockId,
        value: CfgValue,
        role: &str,
    ) -> Result<u32, CfgVerificationError> {
        let Some(pool) = self.type_pool else {
            return Ok(match ty.try_kind() {
                Some(TypeKind::I8)
                | Some(TypeKind::I16)
                | Some(TypeKind::I32)
                | Some(TypeKind::I64)
                | Some(TypeKind::U8)
                | Some(TypeKind::U16)
                | Some(TypeKind::U32)
                | Some(TypeKind::U64)
                | Some(TypeKind::Bool)
                | Some(TypeKind::Error)
                | Some(TypeKind::PtrConst(_))
                | Some(TypeKind::PtrMut(_)) => 1,
                Some(TypeKind::Unit)
                | Some(TypeKind::Never)
                | Some(TypeKind::ComptimeType)
                | Some(TypeKind::Module(_))
                | Some(TypeKind::Struct(_))
                | Some(TypeKind::Array(_))
                | Some(TypeKind::Enum(_))
                | None => 0,
            });
        };

        let canonical_width = || pool.try_abi_slot_count(ty);
        #[cfg(test)]
        let width = self
            .abi_slot_query_override
            .map_or_else(canonical_width, |query| query(pool, ty));
        #[cfg(not(test))]
        let width = canonical_width();

        width.map_err(|error| {
            let kind = ty.try_kind();
            match kind {
                Some(TypeKind::Struct(id)) => self.error(format_args!(
                    "{} instruction {} in block {} references invalid struct type {:?}",
                    role, value, block, id
                )),
                Some(TypeKind::Array(id)) => self.error(format_args!(
                    "{} instruction {} in block {} references invalid array type {:?}",
                    role, value, block, id
                )),
                Some(TypeKind::Enum(id)) => self.error(format_args!(
                    "{} instruction {} in block {} references invalid enum type {:?}",
                    role, value, block, id
                )),
                _ => self.error(format_args!(
                    "{} instruction {} in block {} references invalid type ({error:?})",
                    role, value, block
                )),
            }
        })
    }

    fn is_fixed_str_to_view_coercion(&self, source: Type, result: Type) -> bool {
        let Some(pool) = self.type_pool else {
            return false;
        };
        let (TypeKind::Struct(source_id), TypeKind::Struct(result_id)) =
            (source.kind(), result.kind())
        else {
            return false;
        };
        let (Some(source_def), Some(result_def)) = (
            pool.try_struct_def(source_id),
            pool.try_struct_def(result_id),
        ) else {
            return false;
        };
        let is_fixed_str = rue_air::fixed_string_capacity(&source_def.name).is_some();
        is_fixed_str
            && &*result_def.name == "str"
            && source_def.fields.len() == result_def.fields.len()
            && source_def
                .fields
                .iter()
                .zip(&result_def.fields)
                .all(|(source, result)| source.ty == result.ty)
    }

    fn verify_place(
        &self,
        block: BlockId,
        value: CfgValue,
        place: &Place,
    ) -> Result<(), CfgVerificationError> {
        match place.base {
            PlaceBase::Local(slot) => {
                self.check_local_slot(slot, place.base_type, block, value, "place")?
            }
            PlaceBase::Param(slot) => {
                self.check_param_slot(slot, place.base_type, block, value, "place")?
            }
            PlaceBase::Accessor(producer) => {
                let inst = self.cfg.get_inst(producer);
                if !matches!(inst.data, CfgInstData::AccessorCall { .. })
                    || inst.ty != place.base_type
                {
                    return Err(self.error(format!(
                        "{block}: {value} has an invalid accessor place producer"
                    )));
                }
            }
            PlaceBase::Indirect(pointer) => {
                let inst = self.cfg.get_inst(pointer);
                if !inst.ty.is_ptr() {
                    return Err(self.error(format!(
                        "{block}: {value} has an invalid indirect place pointer"
                    )));
                }
            }
        }
        let projections = self.cfg.get_place_projections(place);
        if let Some(pool) = self.type_pool {
            let mut current_ty = place.base_type;
            for (projection_index, projection) in projections.iter().enumerate() {
                current_ty = match projection {
                    Projection::Field {
                        struct_id,
                        field_index,
                    } => {
                        let Some(def) = pool.try_struct_def(*struct_id) else {
                            return Err(self.error(format_args!(
                                "projection {} in place instruction {} in block {} references invalid struct id {:?}",
                                projection_index, value, block, struct_id
                            )));
                        };
                        let expected = Type::new_struct(*struct_id);
                        if current_ty != expected {
                            return Err(self.error(format_args!(
                                "projection {} in place instruction {} in block {} expects container type {:?}, but previous link produced {:?}",
                                projection_index, value, block, expected, current_ty
                            )));
                        }
                        let Some(field) = def.fields.get(*field_index as usize) else {
                            return Err(self.error(format_args!(
                                "projection {} in place instruction {} in block {} references field {} of struct {:?}, which has {} fields",
                                projection_index,
                                value,
                                block,
                                field_index,
                                struct_id,
                                def.fields.len()
                            )));
                        };
                        field.ty
                    }
                    Projection::Index { array_type, .. } => {
                        let TypeKind::Array(array_id) = array_type.kind() else {
                            return Err(self.error(format_args!(
                                "projection {} in place instruction {} in block {} has non-array container type {:?}",
                                projection_index, value, block, array_type
                            )));
                        };
                        let Some((element_ty, _)) = pool.try_array_def(array_id) else {
                            return Err(self.error(format_args!(
                                "projection {} in place instruction {} in block {} references invalid array id {:?}",
                                projection_index, value, block, array_id
                            )));
                        };
                        if current_ty != *array_type {
                            return Err(self.error(format_args!(
                                "projection {} in place instruction {} in block {} expects container type {:?}, but previous link produced {:?}",
                                projection_index, value, block, array_type, current_ty
                            )));
                        }
                        element_ty
                    }
                };
            }

            let inst = self.cfg.get_inst(value);
            match &inst.data {
                CfgInstData::PlaceRead { .. }
                    if inst.ty != current_ty
                        && !self.is_fixed_str_to_view_coercion(current_ty, inst.ty) =>
                {
                    return Err(self.error(format_args!(
                        "place-read instruction {} in block {} has result type {:?}, but its projection chain produces {:?}",
                        value, block, inst.ty, current_ty
                    )));
                }
                CfgInstData::PlaceWrite { value: stored, .. } => {
                    let stored_ty = self.inst(*stored, block, "place-write value")?.ty;
                    if stored_ty != current_ty {
                        return Err(self.error(format_args!(
                            "place-write instruction {} in block {} stores type {:?}, but its projection chain produces {:?}",
                            value, block, stored_ty, current_ty
                        )));
                    }
                    if inst.ty != Type::UNIT {
                        return Err(self.error(format_args!(
                            "place-write instruction {} in block {} has result type {:?}, expected unit",
                            value, block, inst.ty
                        )));
                    }
                }
                _ => {}
            }
        }

        if let Some(first) = projections.first() {
            let required = match first {
                Projection::Field { struct_id, .. } => Type::new_struct(*struct_id),
                Projection::Index { array_type, .. } => {
                    if !matches!(array_type.kind(), TypeKind::Array(_)) {
                        return Err(self.error(format_args!(
                            "place in instruction {} in block {} has Index projection with non-array container type {:?}",
                            value, block, array_type
                        )));
                    }
                    *array_type
                }
            };
            if place.base_type != required {
                return Err(self.error(format_args!(
                    "place in instruction {} in block {} has logical base type {:?}, but its first projection requires base type {:?}",
                    value, block, place.base_type, required
                )));
            }
        }
        for projection in projections {
            if let Projection::Index { array_type, index } = projection {
                if !matches!(array_type.kind(), TypeKind::Array(_)) {
                    return Err(self.error(format_args!(
                        "place in instruction {} in block {} has Index projection with non-array container type {:?}",
                        value, block, array_type
                    )));
                }
                let index_ty = self.inst(*index, block, "projection index")?.ty;
                if !matches!(
                    index_ty.kind(),
                    TypeKind::I8
                        | TypeKind::I16
                        | TypeKind::I32
                        | TypeKind::I64
                        | TypeKind::U8
                        | TypeKind::U16
                        | TypeKind::U32
                        | TypeKind::U64
                ) {
                    return Err(self.error(format_args!(
                        "projection index {} used by instruction {} in block {} has non-integer type {:?}",
                        index, value, block, index_ty
                    )));
                }
            }
        }
        Ok(())
    }

    fn verify_terminator_use(
        &self,
        block: BlockId,
        term: &Terminator,
    ) -> Result<(), CfgVerificationError> {
        match term {
            Terminator::Goto {
                target,
                args: _,
            } => {
                let args = self.cfg.get_goto_args(term);
                for &arg in args {
                    self.verify_use(block, None, arg, "goto argument")?;
                }
                self.verify_edge(block, *target, args)?;
            }
            Terminator::Branch {
                cond,
                then_block,
                then_args: _,
                else_block,
                else_args: _,
            } => {
                self.verify_use(block, None, *cond, "branch condition")?;
                if self.inst(*cond, block, "branch condition")?.ty != Type::BOOL {
                    return Err(self.error(format_args!(
                        "branch condition {} in block {} has type {:?}, expected bool",
                        cond,
                        block,
                        self.cfg.get_inst(*cond).ty
                    )));
                }
                let then_args = self.cfg.get_branch_then_args(term);
                for &arg in then_args {
                    self.verify_use(block, None, arg, "branch-then argument")?;
                }
                self.verify_edge(block, *then_block, then_args)?;
                let else_args = self.cfg.get_branch_else_args(term);
                for &arg in else_args {
                    self.verify_use(block, None, arg, "branch-else argument")?;
                }
                self.verify_edge(block, *else_block, else_args)?;
            }
            Terminator::Switch {
                scrutinee,
                cases,
                default,
            } => {
                self.verify_use(block, None, *scrutinee, "switch scrutinee")?;
                for &(_, target) in self.cfg.switch_cases(cases) {
                    self.verify_edge(block, target, &[])?;
                }
                self.verify_edge(block, *default, &[])?;
            }
            Terminator::Return { value } => match (self.cfg.return_type(), value) {
                (Type::UNIT, None) => {}
                (Type::UNIT, Some(value)) => return Err(self.error(format_args!(
                    "return in block {} supplies unit value {}; unit-returning functions must use Return {{ value: None }}",
                    block, value
                ))),
                (return_ty, None) => return Err(self.error(format_args!(
                    "return in block {} has no value but function return type is {:?}",
                    block, return_ty
                ))),
                (return_ty, Some(value)) => {
                    self.verify_use(block, None, *value, "return value")?;
                    let value_ty = self.cfg.get_inst(*value).ty;
                    if value_ty != return_ty {
                        return Err(self.error(format_args!(
                            "return value {} in block {} has type {:?}, expected {:?}",
                            value, block, value_ty, return_ty
                        )));
                    }
                }
            },
            Terminator::Unreachable | Terminator::None => {}
        }
        Ok(())
    }

    fn verify_edge(
        &self,
        from: BlockId,
        to: BlockId,
        args: &[CfgValue],
    ) -> Result<(), CfgVerificationError> {
        let params = &self.cfg.get_block(to).params;
        if params.len() != args.len() {
            return Err(self.error(format_args!(
                "edge {} -> {} passes {} block arguments but target expects {}",
                from,
                to,
                args.len(),
                params.len()
            )));
        }
        for (index, (&arg, &(_, param_ty))) in args.iter().zip(params).enumerate() {
            let arg_ty = self.cfg.get_inst(arg).ty;
            if arg_ty != param_ty {
                return Err(self.error(format_args!(
                    "edge {} -> {} argument {} ({}) has type {:?}, target parameter has type {:?} (ill-typed edge)",
                    from, to, index, arg, arg_ty, param_ty
                )));
            }
        }
        Ok(())
    }

    fn verify_use(
        &self,
        use_block: BlockId,
        use_position: Option<usize>,
        value: CfgValue,
        role: &str,
    ) -> Result<(), CfgVerificationError> {
        let _ = self.inst(value, use_block, role)?;
        let Some(attachment) = self.attachments[value.as_u32() as usize] else {
            return Err(self.error(format_args!(
                "{} {} in block {} refers to an unattached value",
                role, value, use_block
            )));
        };
        match attachment {
            Attachment::Param { block } if block == use_block => {}
            Attachment::Inst { block, position } if block == use_block => {
                if use_position.is_some_and(|use_position| position >= use_position) {
                    return Err(self.error(format_args!(
                        "{} {} in block {} is used before its definition at instruction position {}",
                        role, value, use_block, position
                    )));
                }
            }
            Attachment::Param { block: def_block }
            | Attachment::Inst {
                block: def_block, ..
            } => {
                // An unreachable use is exempt: no path reaches it, so no
                // definition can dominate it and nothing it reads can be wrong
                // at runtime. A reachable use must be dominated by its
                // definition — including when the definition sits in a block
                // the entry cannot reach, which never dominates anything.
                let dominators = self.dominators();
                if dominators.is_reachable(use_block) && !dominators.dominates(def_block, use_block)
                {
                    return Err(self.error(format_args!(
                        "{} {} in reachable block {} is defined in block {}, which does not dominate the use",
                        role, value, use_block, def_block
                    )));
                }
            }
        }
        Ok(())
    }

    fn for_each_inst_operand(
        &self,
        block: BlockId,
        value: CfgValue,
        data: &CfgInstData,
        mut f: impl FnMut(CfgValue, &'static str),
    ) {
        match data {
            CfgInstData::Const(_)
            | CfgInstData::BoolConst(_)
            | CfgInstData::StringConst(_)
            | CfgInstData::Param { .. }
            | CfgInstData::BlockParam { .. }
            | CfgInstData::Load { .. }
            | CfgInstData::StorageLive { .. }
            | CfgInstData::StorageDead { .. } => {}
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
                f(*a, "left operand");
                f(*b, "right operand");
            }
            CfgInstData::Neg(v) | CfgInstData::Not(v) | CfgInstData::BitNot(v) => {
                f(*v, "unary operand")
            }
            CfgInstData::Alloc { init, .. } => f(*init, "allocation initializer"),
            CfgInstData::Store { value, .. } | CfgInstData::ParamStore { value, .. } => {
                f(*value, "stored value")
            }
            CfgInstData::PlaceRead { place } => {
                match place.base {
                    PlaceBase::Accessor(producer) | PlaceBase::Indirect(producer) => {
                        f(producer, "place base producer")
                    }
                    PlaceBase::Local(_) | PlaceBase::Param(_) => {}
                }
                for projection in self.cfg.get_place_projections(place) {
                    if let Projection::Index { index, .. } = projection {
                        f(*index, "projection index");
                    }
                }
            }
            CfgInstData::PlaceWrite { place, value } => {
                match place.base {
                    PlaceBase::Accessor(producer) | PlaceBase::Indirect(producer) => {
                        f(producer, "place base producer")
                    }
                    PlaceBase::Local(_) | PlaceBase::Param(_) => {}
                }
                for projection in self.cfg.get_place_projections(place) {
                    if let Projection::Index { index, .. } = projection {
                        f(*index, "projection index");
                    }
                }
                f(*value, "place-write value");
            }
            CfgInstData::Call { args, .. } | CfgInstData::AccessorCall { args, .. } => {
                for arg in self.cfg.call_args(args) {
                    f(arg.value, "call argument");
                }
            }
            CfgInstData::Intrinsic { args, .. } => {
                for &operand in self.cfg.intrinsic_args(args) {
                    f(operand, "intrinsic argument");
                }
            }
            CfgInstData::StructInit { fields, .. } => {
                for &operand in self.cfg.struct_fields(fields) {
                    f(operand, "struct field");
                }
            }
            CfgInstData::ArrayInit { elements } => {
                for &operand in self.cfg.array_elements(elements) {
                    f(operand, "array element");
                }
            }
            CfgInstData::EnumVariant { payload, .. } => {
                for &operand in self.cfg.enum_payload(payload) {
                    f(operand, "enum payload");
                }
            }
            CfgInstData::EnumPayloadGet { base, .. } => f(*base, "enum base"),
            CfgInstData::IntCast { value, .. } => f(*value, "cast operand"),
            CfgInstData::Drop { value } => f(*value, "drop operand"),
        }
        let _ = (block, value);
    }
}

#[cfg(test)]
mod tests {
    use super::Verifier;
    use crate::inst::{
        BlockId, Cfg, CfgInst, CfgInstData, CfgValue, Place, PlaceBase, Projection, Terminator,
    };
    use crate::{CfgVerificationLocation, OptLevel, opt};
    use lasso::ThreadedRodeo;
    use rue_air::{
        FrozenTypeInternPool, StructDef, StructField, StructId, Type, TypeInternPool, TypeKind,
    };
    use rue_span::Span;

    fn unit_cfg() -> Cfg {
        Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![])
    }

    fn register_struct(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        field_types: &[Type],
    ) -> StructId {
        pool.register_struct(
            interner.get_or_intern(name),
            StructDef {
                name: name.into(),
                fields: field_types
                    .iter()
                    .enumerate()
                    .map(|(index, &ty)| StructField {
                        name: format!("field{index}"),
                        ty,
                    })
                    .collect(),
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

    fn register_droppable_struct(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
    ) -> Type {
        let id = pool
            .register_struct(
                interner.get_or_intern(name),
                StructDef {
                    name: name.into(),
                    fields: Vec::new(),
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: Some(format!("{name}.__drop").into()),
                    is_builtin: false,
                    is_pub: false,
                    file_id: rue_span::FileId::DEFAULT,
                },
            )
            .0;
        Type::new_struct(id)
    }

    fn register_nonzero_droppable_struct(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
    ) -> Type {
        let id = pool
            .register_struct(
                interner.get_or_intern(name),
                StructDef {
                    name: name.into(),
                    fields: vec![StructField {
                        name: "payload".into(),
                        ty: Type::I64,
                    }],
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: Some(format!("{name}.__drop").into()),
                    is_builtin: false,
                    is_pub: false,
                    file_id: rue_span::FileId::DEFAULT,
                },
            )
            .0;
        Type::new_struct(id)
    }

    fn init_nonzero_owner(cfg: &mut Cfg, block: BlockId, owner: Type, payload: i64) -> CfgValue {
        let scalar = push(cfg, block, CfgInstData::Const(payload as u64), Type::I64);
        let fields = cfg.push_struct_fields([scalar]).unwrap();
        push(
            cfg,
            block,
            CfgInstData::StructInit {
                struct_id: match owner.kind() {
                    TypeKind::Struct(id) => id,
                    _ => unreachable!(),
                },
                fields,
            },
            owner,
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

    fn cfg_with_load_before_storage_live() -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "storage_order".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let loaded = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, Type::I32);
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(loaded),
            },
        );
        cfg
    }

    #[test]
    fn finish_rejects_local_use_before_storage_live() {
        let error = cfg_with_load_before_storage_live()
            .finish(&FrozenTypeInternPool::new())
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not live on every reaching path")
        );
        assert!(matches!(
            error.location(),
            CfgVerificationLocation::Instruction { .. }
        ));
    }

    #[test]
    fn post_optimization_publication_rejects_local_use_before_storage_live() {
        let error = cfg_with_load_before_storage_live()
            .finish_after_optimization(&FrozenTypeInternPool::new())
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not live on every reaching path")
        );
    }

    #[test]
    fn materialization_publication_rejects_local_use_before_storage_live() {
        let error = cfg_with_load_before_storage_live()
            .verify_materialization_with_type_pool(&FrozenTypeInternPool::new())
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not live on every reaching path")
        );
    }

    #[test]
    fn semantic_verifier_rejects_storage_dead_before_live() {
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "dead_before_live".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let error = cfg.finish(&FrozenTypeInternPool::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not live on every reaching path")
        );
    }

    #[test]
    fn semantic_verifier_does_not_conflate_zero_width_local_lifetimes() {
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "zst_storage".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        for _ in 0..2 {
            push(
                &mut cfg,
                entry,
                CfgInstData::StorageLive {
                    slot: 0,
                    local_ty: Type::UNIT,
                },
                Type::UNIT,
            );
        }
        for _ in 0..2 {
            push(
                &mut cfg,
                entry,
                CfgInstData::StorageDead {
                    slot: 0,
                    local_ty: Type::UNIT,
                },
                Type::UNIT,
            );
        }
        cfg.set_terminator(entry, Terminator::Return { value: None });

        cfg.finish(&FrozenTypeInternPool::new()).unwrap();
    }

    #[test]
    fn semantic_verifier_accepts_balanced_storage_in_a_loop() {
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "storage_loop".to_string(), vec![]);
        let entry = cfg.new_block();
        let header = cfg.new_block();
        let exit = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target: header,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        push(
            &mut cfg,
            header,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        let init = push(&mut cfg, header, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            header,
            CfgInstData::Alloc { slot: 0, init },
            Type::UNIT,
        );
        push(
            &mut cfg,
            header,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        let again = push(&mut cfg, header, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(header, again, header, [], exit, []);
        cfg.set_terminator(exit, Terminator::Return { value: None });

        cfg.finish(&FrozenTypeInternPool::new()).unwrap();
    }

    #[test]
    fn semantic_verifier_rejects_path_dependent_storage_lifetime() {
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "storage_join".to_string(), vec![]);
        let entry = cfg.new_block();
        let live_arm = cfg.new_block();
        let dead_arm = cfg.new_block();
        let join = cfg.new_block();
        cfg.entry = entry;
        let cond = push(&mut cfg, entry, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(entry, cond, live_arm, [], dead_arm, []);
        push(
            &mut cfg,
            live_arm,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        for block in [live_arm, dead_arm] {
            cfg.set_terminator(
                block,
                Terminator::Goto {
                    target: join,
                    args: crate::payload::CfgGotoArgs::EMPTY,
                },
            );
        }
        push(&mut cfg, join, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(join, Terminator::Return { value: None });

        let error = cfg.finish(&FrozenTypeInternPool::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not live on every reaching path")
        );
    }

    #[test]
    fn semantic_verifier_rejects_drop_flag_read_before_all_paths_initialize_it() {
        let mut cfg = Cfg::new(Type::UNIT, 1, 1, "drop_flag_join".to_string(), vec![false]);
        let entry = cfg.new_block();
        let init_arm = cfg.new_block();
        let skip_arm = cfg.new_block();
        let join = cfg.new_block();
        cfg.entry = entry;
        let cond = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::BOOL);
        cfg.set_branch(entry, cond, init_arm, [], skip_arm, []);
        let flag = push(&mut cfg, init_arm, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            init_arm,
            CfgInstData::Store {
                slot: 0,
                value: flag,
            },
            Type::UNIT,
        );
        for block in [init_arm, skip_arm] {
            cfg.set_terminator(
                block,
                Terminator::Goto {
                    target: join,
                    args: crate::payload::CfgGotoArgs::EMPTY,
                },
            );
        }
        push(&mut cfg, join, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(join, Terminator::Return { value: None });

        let error = cfg.finish(&FrozenTypeInternPool::new()).unwrap_err();
        assert!(error.to_string().contains("before it is initialized"));
    }

    #[test]
    fn semantic_verifier_rejects_hidden_slot_load_when_no_write_survives() {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "missing_hidden_init".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let loaded = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(loaded),
            },
        );

        let error = cfg.finish(&FrozenTypeInternPool::new()).unwrap_err();
        assert!(error.to_string().contains("before it is initialized"));
    }

    #[test]
    fn semantic_verifier_accepts_param_only_flag_initialized_on_divergent_paths() {
        let mut cfg = Cfg::new(
            Type::I32,
            1,
            1,
            "drop_flag_divergent".to_string(),
            vec![false],
        );
        let entry = cfg.new_block();
        let true_arm = cfg.new_block();
        let false_arm = cfg.new_block();
        let join = cfg.new_block();
        cfg.entry = entry;
        let cond = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::BOOL);
        cfg.set_branch(entry, cond, true_arm, [], false_arm, []);
        for (block, value) in [(true_arm, 1), (false_arm, 0)] {
            let flag = push(&mut cfg, block, CfgInstData::Const(value), Type::I32);
            push(
                &mut cfg,
                block,
                CfgInstData::Store {
                    slot: 0,
                    value: flag,
                },
                Type::UNIT,
            );
            cfg.set_goto(block, join, []);
        }
        let flag = push(&mut cfg, join, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(join, Terminator::Return { value: Some(flag) });

        cfg.finish(&FrozenTypeInternPool::new()).unwrap();
    }

    #[test]
    fn semantic_verifier_rejects_use_after_explicit_drop() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_droppable_struct(&pool, &interner, "ConsumedOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(owner, 0, 0, "use_after_drop".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let fields = cfg.push_struct_fields([]).unwrap();
        let owned = push(
            &mut cfg,
            entry,
            CfgInstData::StructInit {
                struct_id: match owner.kind() {
                    TypeKind::Struct(id) => id,
                    _ => unreachable!(),
                },
                fields,
            },
            owner,
        );
        push(
            &mut cfg,
            entry,
            CfgInstData::Drop { value: owned },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(owned) });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already dropped on a reaching path")
        );
    }

    #[test]
    fn semantic_verifier_rejects_double_drop() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_droppable_struct(&pool, &interner, "DoubleDropOwner");
        let owner_id = match owner.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "double_drop".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let fields = cfg.push_struct_fields([]).unwrap();
        let owned = push(
            &mut cfg,
            entry,
            CfgInstData::StructInit {
                struct_id: owner_id,
                fields,
            },
            owner,
        );
        for _ in 0..2 {
            push(
                &mut cfg,
                entry,
                CfgInstData::Drop { value: owned },
                Type::UNIT,
            );
        }
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already dropped on a reaching path")
        );
    }

    #[test]
    fn semantic_verifier_rejects_duplicate_drop_through_fresh_load() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "FreshLoadOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "fresh_load_drop".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        let init = init_nonzero_owner(&mut cfg, entry, owner, 1);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc { slot: 0, init },
            Type::UNIT,
        );
        for _ in 0..2 {
            let loaded = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, owner);
            push(
                &mut cfg,
                entry,
                CfgInstData::Drop { value: loaded },
                Type::UNIT,
            );
        }
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(error.to_string().contains("already-consumed owner root"));
    }

    #[test]
    fn semantic_verifier_rejects_pre_drop_load_used_by_later_store() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "StoredConsumedOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            1,
            0,
            "stored_consumed_owner".to_string(),
            vec![],
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        let initial = init_nonzero_owner(&mut cfg, entry, owner, 1);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc {
                slot: 0,
                init: initial,
            },
            Type::UNIT,
        );
        let stale = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, owner);
        let dropped = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, owner);
        push(
            &mut cfg,
            entry,
            CfgInstData::Drop { value: dropped },
            Type::UNIT,
        );
        push(
            &mut cfg,
            entry,
            CfgInstData::Store {
                slot: 0,
                value: stale,
            },
            Type::UNIT,
        );
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("uses already-consumed owner root")
        );
    }

    #[test]
    fn semantic_verifier_rejects_duplicate_drop_through_fresh_param() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "FreshParamOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            0,
            1,
            "fresh_param_drop".to_string(),
            vec![false],
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        for _ in 0..2 {
            let parameter = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, owner);
            push(
                &mut cfg,
                entry,
                CfgInstData::Drop { value: parameter },
                Type::UNIT,
            );
        }
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(error.to_string().contains("already-consumed owner root"));
    }

    #[test]
    fn semantic_verifier_rejects_pre_drop_param_used_by_later_aggregate() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "AggregatedParamOwner");
        let wrapper_id = register_struct(&pool, &interner, "OwnerWrapper", &[owner]);
        let wrapper = Type::new_struct(wrapper_id);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            0,
            1,
            "aggregated_consumed_param".to_string(),
            vec![false],
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let stale = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, owner);
        let dropped = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, owner);
        push(
            &mut cfg,
            entry,
            CfgInstData::Drop { value: dropped },
            Type::UNIT,
        );
        let fields = cfg.push_struct_fields([stale]).unwrap();
        push(
            &mut cfg,
            entry,
            CfgInstData::StructInit {
                struct_id: wrapper_id,
                fields,
            },
            wrapper,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("uses already-consumed owner root")
        );
    }

    #[test]
    fn semantic_verifier_rejects_duplicate_drop_through_fresh_inout_param() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "FreshInoutOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            0,
            1,
            "fresh_inout_drop".to_string(),
            rue_air::ParamSlotModes::new(vec![true], vec![true]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        for _ in 0..2 {
            let parameter = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, owner);
            push(
                &mut cfg,
                entry,
                CfgInstData::Drop { value: parameter },
                Type::UNIT,
            );
        }
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(error.to_string().contains("already-consumed owner root"));
    }

    #[test]
    fn semantic_verifier_accepts_whole_inout_reset_between_drops() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "ResetInoutOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            0,
            1,
            "reset_inout_drop".to_string(),
            rue_air::ParamSlotModes::new(vec![true], vec![true]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let parameter = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, owner);
        push(
            &mut cfg,
            entry,
            CfgInstData::Drop { value: parameter },
            Type::UNIT,
        );
        let replacement = init_nonzero_owner(&mut cfg, entry, owner, 1);
        push(
            &mut cfg,
            entry,
            CfgInstData::ParamStore {
                param_slot: 0,
                value: replacement,
            },
            Type::UNIT,
        );
        let parameter = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, owner);
        push(
            &mut cfg,
            entry,
            CfgInstData::Drop { value: parameter },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        cfg.finish(&pool).unwrap();
    }

    #[test]
    fn semantic_verifier_accepts_whole_inout_place_write_reset_between_drops() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "PlaceResetInoutOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            0,
            1,
            "place_reset_inout_drop".to_string(),
            rue_air::ParamSlotModes::new(vec![true], vec![true]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let parameter = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, owner);
        push(
            &mut cfg,
            entry,
            CfgInstData::Drop { value: parameter },
            Type::UNIT,
        );
        let replacement = init_nonzero_owner(&mut cfg, entry, owner, 1);
        push(
            &mut cfg,
            entry,
            CfgInstData::PlaceWrite {
                place: Place::param(0, owner),
                value: replacement,
            },
            Type::UNIT,
        );
        let parameter = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, owner);
        push(
            &mut cfg,
            entry,
            CfgInstData::Drop { value: parameter },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        cfg.finish(&pool).unwrap();
    }

    #[test]
    fn semantic_verifier_rejects_unknown_phi_double_drop() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_droppable_struct(&pool, &interner, "UnknownPhiOwner");
        let owner_id = match owner.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "unknown_phi_drop".to_string(), vec![]);
        let entry = cfg.new_block();
        let left = cfg.new_block();
        let right = cfg.new_block();
        let join = cfg.new_block();
        let parameter = cfg.add_block_param(join, owner);
        cfg.entry = entry;
        let cond = push(&mut cfg, entry, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(entry, cond, left, [], right, []);
        for block in [left, right] {
            let fields = cfg.push_struct_fields([]).unwrap();
            let value = push(
                &mut cfg,
                block,
                CfgInstData::StructInit {
                    struct_id: owner_id,
                    fields,
                },
                owner,
            );
            cfg.set_goto(block, join, [value]);
        }
        for _ in 0..2 {
            push(
                &mut cfg,
                join,
                CfgInstData::Drop { value: parameter },
                Type::UNIT,
            );
        }
        cfg.set_terminator(join, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already dropped on a reaching path")
        );
    }

    #[test]
    fn semantic_verifier_rejects_conflicting_root_phi_double_drop() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "ConflictingPhiOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 2, 0, "conflicting_phi_drop".to_string(), vec![]);
        let entry = cfg.new_block();
        let left = cfg.new_block();
        let right = cfg.new_block();
        let join = cfg.new_block();
        let parameter = cfg.add_block_param(join, owner);
        cfg.entry = entry;
        for slot in 0..2 {
            push(
                &mut cfg,
                entry,
                CfgInstData::StorageLive {
                    slot,
                    local_ty: owner,
                },
                Type::UNIT,
            );
            let initial = init_nonzero_owner(&mut cfg, entry, owner, i64::from(slot));
            push(
                &mut cfg,
                entry,
                CfgInstData::Alloc {
                    slot,
                    init: initial,
                },
                Type::UNIT,
            );
        }
        let cond = push(&mut cfg, entry, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(entry, cond, left, [], right, []);
        let left_value = push(&mut cfg, left, CfgInstData::Load { slot: 0 }, owner);
        cfg.set_goto(left, join, [left_value]);
        let right_value = push(&mut cfg, right, CfgInstData::Load { slot: 1 }, owner);
        cfg.set_goto(right, join, [right_value]);
        for _ in 0..2 {
            push(
                &mut cfg,
                join,
                CfgInstData::Drop { value: parameter },
                Type::UNIT,
            );
        }
        for slot in 0..2 {
            push(
                &mut cfg,
                join,
                CfgInstData::StorageDead {
                    slot,
                    local_ty: owner,
                },
                Type::UNIT,
            );
        }
        cfg.set_terminator(join, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already dropped on a reaching path")
        );
    }

    #[test]
    fn semantic_verifier_rejects_consumed_unknown_phi_as_outgoing_argument() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_droppable_struct(&pool, &interner, "OutgoingPhiOwner");
        let owner_id = match owner.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "outgoing_phi_drop".to_string(), vec![]);
        let entry = cfg.new_block();
        let middle = cfg.new_block();
        let tail = cfg.new_block();
        let middle_param = cfg.add_block_param(middle, owner);
        cfg.add_block_param(tail, owner);
        cfg.entry = entry;
        let fields = cfg.push_struct_fields([]).unwrap();
        let value = push(
            &mut cfg,
            entry,
            CfgInstData::StructInit {
                struct_id: owner_id,
                fields,
            },
            owner,
        );
        cfg.set_goto(entry, middle, [value]);
        push(
            &mut cfg,
            middle,
            CfgInstData::Drop {
                value: middle_param,
            },
            Type::UNIT,
        );
        cfg.set_goto(middle, tail, [middle_param]);
        cfg.set_terminator(tail, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already dropped on a reaching path")
        );
    }

    #[test]
    fn semantic_verifier_accepts_fresh_unknown_phi_each_loop_entry() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_droppable_struct(&pool, &interner, "FreshPhiLoopOwner");
        let owner_id = match owner.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "fresh_phi_loop".to_string(), vec![]);
        let entry = cfg.new_block();
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();
        let parameter = cfg.add_block_param(header, owner);
        cfg.entry = entry;
        let fields = cfg.push_struct_fields([]).unwrap();
        let initial = push(
            &mut cfg,
            entry,
            CfgInstData::StructInit {
                struct_id: owner_id,
                fields,
            },
            owner,
        );
        cfg.set_goto(entry, header, [initial]);
        push(
            &mut cfg,
            header,
            CfgInstData::Drop { value: parameter },
            Type::UNIT,
        );
        let again = push(&mut cfg, header, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(header, again, body, [], exit, []);
        let fields = cfg.push_struct_fields([]).unwrap();
        let next = push(
            &mut cfg,
            body,
            CfgInstData::StructInit {
                struct_id: owner_id,
                fields,
            },
            owner,
        );
        cfg.set_goto(body, header, [next]);
        cfg.set_terminator(exit, Terminator::Return { value: None });

        cfg.finish(&pool).unwrap();
    }

    #[test]
    fn semantic_verifier_propagates_exact_root_through_block_parameter() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "BlockParamOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "block_param_drop".to_string(), vec![]);
        let entry = cfg.new_block();
        let tail = cfg.new_block();
        let parameter = cfg.add_block_param(tail, owner);
        cfg.entry = entry;
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        let init = init_nonzero_owner(&mut cfg, entry, owner, 1);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc { slot: 0, init },
            Type::UNIT,
        );
        let loaded = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, owner);
        cfg.set_goto(entry, tail, [loaded]);
        push(
            &mut cfg,
            tail,
            CfgInstData::Drop { value: parameter },
            Type::UNIT,
        );
        let loaded_again = push(&mut cfg, tail, CfgInstData::Load { slot: 0 }, owner);
        push(
            &mut cfg,
            tail,
            CfgInstData::Drop {
                value: loaded_again,
            },
            Type::UNIT,
        );
        push(
            &mut cfg,
            tail,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        cfg.set_terminator(tail, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(error.to_string().contains("already-consumed owner root"));
    }

    #[test]
    fn semantic_verifier_accepts_reset_loop_carried_owner_root() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "LoopCarriedOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "loop_carried_owner".to_string(), vec![]);
        let entry = cfg.new_block();
        let header = cfg.new_block();
        let exit = cfg.new_block();
        let parameter = cfg.add_block_param(header, owner);
        cfg.entry = entry;
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        let initial = init_nonzero_owner(&mut cfg, entry, owner, 0);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc {
                slot: 0,
                init: initial,
            },
            Type::UNIT,
        );
        let initial = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, owner);
        cfg.set_goto(entry, header, [initial]);

        push(
            &mut cfg,
            header,
            CfgInstData::Drop { value: parameter },
            Type::UNIT,
        );
        let replacement = init_nonzero_owner(&mut cfg, header, owner, 1);
        push(
            &mut cfg,
            header,
            CfgInstData::Store {
                slot: 0,
                value: replacement,
            },
            Type::UNIT,
        );
        let replacement = push(&mut cfg, header, CfgInstData::Load { slot: 0 }, owner);
        let again = push(&mut cfg, header, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(header, again, header, [replacement], exit, []);
        push(
            &mut cfg,
            exit,
            CfgInstData::Drop { value: replacement },
            Type::UNIT,
        );
        push(
            &mut cfg,
            exit,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        cfg.set_terminator(exit, Terminator::Return { value: None });

        cfg.finish(&pool).unwrap();
    }

    #[test]
    fn semantic_verifier_rejects_consumed_owner_root_on_backedge() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "ConsumedBackedgeOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "consumed_backedge".to_string(), vec![]);
        let entry = cfg.new_block();
        let header = cfg.new_block();
        let exit = cfg.new_block();
        let parameter = cfg.add_block_param(header, owner);
        cfg.entry = entry;
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        let initial = init_nonzero_owner(&mut cfg, entry, owner, 0);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc {
                slot: 0,
                init: initial,
            },
            Type::UNIT,
        );
        let initial = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, owner);
        cfg.set_goto(entry, header, [initial]);
        push(
            &mut cfg,
            header,
            CfgInstData::Drop { value: parameter },
            Type::UNIT,
        );
        let again = push(&mut cfg, header, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(header, again, header, [parameter], exit, []);
        push(
            &mut cfg,
            exit,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already dropped on a reaching path")
        );
    }

    #[test]
    fn semantic_verifier_treats_loop_body_definitions_as_fresh_dynamic_values() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_droppable_struct(&pool, &interner, "LoopOwner");
        let owner_id = match owner.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "drop_loop".to_string(), vec![]);
        let entry = cfg.new_block();
        let header = cfg.new_block();
        let exit = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target: header,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        let fields = cfg.push_struct_fields([]).unwrap();
        let owned = push(
            &mut cfg,
            header,
            CfgInstData::StructInit {
                struct_id: owner_id,
                fields,
            },
            owner,
        );
        push(
            &mut cfg,
            header,
            CfgInstData::Drop { value: owned },
            Type::UNIT,
        );
        let again = push(&mut cfg, header, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(header, again, header, [], exit, []);
        cfg.set_terminator(exit, Terminator::Return { value: None });

        cfg.finish(&pool).unwrap();
    }

    #[test]
    fn semantic_verifier_ignores_drop_events_for_trivial_values() {
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "trivial_drop_loop".to_string(), vec![]);
        let entry = cfg.new_block();
        let header = cfg.new_block();
        let exit = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target: header,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        let value = push(&mut cfg, header, CfgInstData::Const(0), Type::I32);
        push(&mut cfg, header, CfgInstData::Drop { value }, Type::UNIT);
        let again = push(&mut cfg, header, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(header, again, header, [], exit, []);
        cfg.set_terminator(exit, Terminator::Return { value: None });

        cfg.finish(&FrozenTypeInternPool::new()).unwrap();
    }

    #[test]
    fn semantic_verifier_does_not_consume_trivial_local_root() {
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "trivial_local_drop".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        let init = push(&mut cfg, entry, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc { slot: 0, init },
            Type::UNIT,
        );
        for _ in 0..2 {
            let value = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, Type::I32);
            push(&mut cfg, entry, CfgInstData::Drop { value }, Type::UNIT);
        }
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        cfg.finish(&FrozenTypeInternPool::new()).unwrap();
    }

    #[test]
    fn semantic_verifier_rejects_normal_return_with_live_storage() {
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "live_at_return".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let error = cfg.finish(&FrozenTypeInternPool::new()).unwrap_err();
        assert!(error.to_string().contains("leaves local storage"));
    }

    #[test]
    fn semantic_verifier_accepts_balanced_early_returns() {
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "balanced_returns".to_string(), vec![]);
        let entry = cfg.new_block();
        let left = cfg.new_block();
        let right = cfg.new_block();
        cfg.entry = entry;
        let cond = push(&mut cfg, entry, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(entry, cond, left, [], right, []);
        for block in [left, right] {
            push(
                &mut cfg,
                block,
                CfgInstData::StorageLive {
                    slot: 0,
                    local_ty: Type::I32,
                },
                Type::UNIT,
            );
            push(
                &mut cfg,
                block,
                CfgInstData::StorageDead {
                    slot: 0,
                    local_ty: Type::I32,
                },
                Type::UNIT,
            );
            cfg.set_terminator(block, Terminator::Return { value: None });
        }

        cfg.finish(&FrozenTypeInternPool::new()).unwrap();
    }

    #[test]
    fn semantic_verifier_exempts_panicking_and_nonterminating_paths_from_storage_dead() {
        let mut panic_cfg = Cfg::new(Type::UNIT, 1, 0, "panic_path".to_string(), vec![]);
        let panic_entry = panic_cfg.new_block();
        panic_cfg.entry = panic_entry;
        push(
            &mut panic_cfg,
            panic_entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        panic_cfg.set_terminator(panic_entry, Terminator::Unreachable);
        panic_cfg.finish(&FrozenTypeInternPool::new()).unwrap();

        let mut loop_cfg = Cfg::new(Type::UNIT, 1, 0, "nonterminating_path".to_string(), vec![]);
        let loop_entry = loop_cfg.new_block();
        let forever = loop_cfg.new_block();
        loop_cfg.entry = loop_entry;
        push(
            &mut loop_cfg,
            loop_entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: Type::I32,
            },
            Type::UNIT,
        );
        loop_cfg.set_goto(loop_entry, forever, []);
        loop_cfg.set_goto(forever, forever, []);
        loop_cfg.finish(&FrozenTypeInternPool::new()).unwrap();
    }

    #[test]
    fn semantic_verifier_reports_invalid_drop_type_without_panicking() {
        let foreign_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let foreign_owner = register_droppable_struct(&foreign_pool, &interner, "ForeignOwner");
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "invalid_drop_type".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let malformed = push(&mut cfg, entry, CfgInstData::Const(0), foreign_owner);
        push(
            &mut cfg,
            entry,
            CfgInstData::Drop { value: malformed },
            Type::UNIT,
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let error = cfg.finish(&FrozenTypeInternPool::new()).unwrap_err();
        assert!(error.to_string().contains("references invalid struct type"));
    }

    #[test]
    fn semantic_verifier_bounds_state_for_many_live_regions_across_long_chain() {
        const REGIONS: u32 = 64;
        const CHAIN: usize = 64;
        let mut cfg = Cfg::new(
            Type::UNIT,
            REGIONS,
            0,
            "bounded_regions".to_string(),
            vec![],
        );
        let entry = cfg.new_block();
        let chain = (0..CHAIN).map(|_| cfg.new_block()).collect::<Vec<_>>();
        cfg.entry = entry;
        for slot in 0..REGIONS {
            push(
                &mut cfg,
                entry,
                CfgInstData::StorageLive {
                    slot,
                    local_ty: Type::I32,
                },
                Type::UNIT,
            );
        }
        cfg.set_goto(entry, chain[0], []);
        for pair in chain.windows(2) {
            cfg.set_goto(pair[0], pair[1], []);
        }
        let tail = *chain.last().unwrap();
        for slot in 0..REGIONS {
            push(
                &mut cfg,
                tail,
                CfgInstData::StorageDead {
                    slot,
                    local_ty: Type::I32,
                },
                Type::UNIT,
            );
        }
        cfg.set_terminator(tail, Terminator::Return { value: None });

        super::SEMANTIC_WORK.with(|work| *work.borrow_mut() = Default::default());
        cfg.finish(&FrozenTypeInternPool::new()).unwrap();
        super::SEMANTIC_WORK.with(|work| {
            let work = *work.borrow();
            let blocks = CHAIN + 1;
            assert_eq!(work.fact_solves, REGIONS as usize);
            assert!(work.peak_binary_state_slots <= blocks * 3);
            assert!(work.block_visits <= REGIONS as usize * blocks);
            assert!(work.edge_visits <= REGIONS as usize * (blocks - 1));
            assert_eq!(
                work.validation_instruction_visits,
                REGIONS as usize * REGIONS as usize * 2
            );
            assert_eq!(work.instruction_operand_visits, 0);
            assert_eq!(work.terminator_operand_visits, 0);
        });
    }

    #[test]
    fn semantic_verifier_resolves_reverse_phi_chain_with_linear_work() {
        const PHIS: usize = 128;
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "ReversePhiOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "reverse_phi_chain".to_string(), vec![]);
        let entry = cfg.new_block();
        let blocks = (0..PHIS).map(|_| cfg.new_block()).collect::<Vec<_>>();
        let parameters = blocks
            .iter()
            .map(|&block| cfg.add_block_param(block, owner))
            .collect::<Vec<_>>();
        cfg.entry = entry;
        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        let initial = init_nonzero_owner(&mut cfg, entry, owner, 0);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc {
                slot: 0,
                init: initial,
            },
            Type::UNIT,
        );
        let initial = push(&mut cfg, entry, CfgInstData::Load { slot: 0 }, owner);
        cfg.set_goto(entry, blocks[PHIS - 1], [initial]);
        for index in (1..PHIS).rev() {
            cfg.set_goto(blocks[index], blocks[index - 1], [parameters[index]]);
        }
        let tail = blocks[0];
        push(
            &mut cfg,
            tail,
            CfgInstData::Drop {
                value: parameters[0],
            },
            Type::UNIT,
        );
        let duplicate = push(&mut cfg, tail, CfgInstData::Load { slot: 0 }, owner);
        push(
            &mut cfg,
            tail,
            CfgInstData::Drop { value: duplicate },
            Type::UNIT,
        );
        push(
            &mut cfg,
            tail,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: owner,
            },
            Type::UNIT,
        );
        cfg.set_terminator(tail, Terminator::Return { value: None });

        super::SEMANTIC_WORK.with(|work| *work.borrow_mut() = Default::default());
        let error = cfg.finish(&pool).unwrap_err();
        assert!(error.to_string().contains("already-consumed owner root"));
        super::SEMANTIC_WORK.with(|work| {
            let work = *work.borrow();
            assert_eq!(work.root_nodes, PHIS);
            assert_eq!(work.root_edges, PHIS);
            assert!(work.root_updates <= PHIS * 2);
            assert!(work.root_dependency_visits <= (PHIS - 1) * 2);
            assert!(work.validation_instruction_visits <= work.fact_solves * 9);
            assert!(work.instruction_operand_visits > 0);
            assert!(work.instruction_operand_visits <= work.fact_solves * 3);
            assert!(work.terminator_operand_visits >= PHIS * 2);
            assert!(work.terminator_operand_visits <= work.fact_solves * PHIS);
        });
    }

    #[test]
    fn verify_accepts_terminated_block() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let v = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(42),
                ty: Type::I32,
                span: Span::new(0, 2),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(v) });
        cfg.verify().unwrap(); // must not panic
    }

    fn cfg_with_field_place(base_type: Type, struct_id: StructId) -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "field_place".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                base_type,
                [Projection::Field {
                    struct_id,
                    field_index: 0,
                }],
            )
            .unwrap();
        let read = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(read) });
        cfg
    }

    #[test]
    fn verify_accepts_matching_place_base_type() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let struct_id = register_struct(&pool, &interner, "FieldBase", &[Type::I32]);
        cfg_with_field_place(Type::new_struct(struct_id), struct_id)
            .verify()
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "first projection requires base type")]
    fn verify_rejects_field_projection_with_wrong_base_type() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let struct_id = register_struct(&pool, &interner, "WrongBase", &[Type::I32]);
        cfg_with_field_place(Type::I32, struct_id).verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "first projection requires base type")]
    fn verify_rejects_index_projection_with_wrong_base_type_on_write() {
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "index_place".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let index = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::U64,
                span: Span::new(0, 1),
            },
        );
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        let array_type = TypeInternPool::new()
            .try_intern_array(Type::I32, 1)
            .unwrap();
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                Type::I32,
                [Projection::Index { array_type, index }],
            )
            .unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceWrite { place, value },
                ty: Type::UNIT,
                span: Span::new(0, 1),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "Index projection with non-array container type Type::I32")]
    fn verify_rejects_non_array_index_projection_type() {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "index_type".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let index = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::U64,
                span: Span::new(0, 1),
            },
        );
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                Type::I32,
                [Projection::Index {
                    array_type: Type::I32,
                    index,
                }],
            )
            .unwrap();
        let read = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(read) });

        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "has no terminator")]
    fn verify_catches_missing_terminator() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        // Deliberately leave the reachable entry block with Terminator::None.
        cfg.verify().unwrap();
    }

    #[test]
    fn verify_ignores_unreachable_unterminated_block() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(value) });
        // An orphan block, never wired in, with no terminator: must be skipped.
        let _orphan = cfg.new_block();
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "block arguments")]
    fn verify_catches_arity_mismatch() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.new_block();
        // Target expects one block parameter...
        let param = cfg.add_block_param(target, Type::I32);
        cfg.set_terminator(target, Terminator::Return { value: Some(param) });
        // ...but the goto edge passes zero arguments.
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        cfg.verify().unwrap();
    }

    /// Build `entry --goto(one arg of `arg_ty`)--> target(param: i32)`.
    fn cfg_with_typed_edge(arg_ty: Type) -> Cfg {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.new_block();
        let param = cfg.add_block_param(target, Type::I32);
        cfg.set_terminator(target, Terminator::Return { value: Some(param) });
        let arg = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: arg_ty,
                span: Span::new(0, 1),
            },
        );
        let args = cfg.push_goto_args(vec![arg]).unwrap();
        cfg.set_terminator(entry, Terminator::Goto { target, args });
        cfg
    }

    #[test]
    fn verify_accepts_well_typed_edge() {
        cfg_with_typed_edge(Type::I32).verify().unwrap(); // must not panic
    }

    #[test]
    #[should_panic(expected = "ill-typed edge")]
    fn verify_catches_edge_type_mismatch() {
        // The RUE-347 shape: a unit value passed into an i32 block parameter.
        cfg_with_typed_edge(Type::UNIT).verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "ill-typed edge")]
    fn verify_catches_edge_type_mismatch_in_unreachable_block() {
        // Ill-typed edges parked in unreachable blocks are exactly where
        // divergence-handling bugs hide (RUE-347) — they must still be caught.
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let entry_value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(entry_value),
            },
        );
        // Unreachable-but-terminated pair with a unit→i32 edge.
        let orphan_from = cfg.new_block();
        let orphan_to = cfg.new_block();
        let param = cfg.add_block_param(orphan_to, Type::I32);
        cfg.set_terminator(orphan_to, Terminator::Return { value: Some(param) });
        let arg = cfg.add_inst_to_block(
            orphan_from,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::UNIT,
                span: Span::new(0, 1),
            },
        );
        let args = cfg.push_goto_args(vec![arg]).unwrap();
        cfg.set_terminator(
            orphan_from,
            Terminator::Goto {
                target: orphan_to,
                args,
            },
        );
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "value v0 is unattached")]
    fn verify_rejects_unattached_value() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst(CfgInst {
            data: CfgInstData::Const(0),
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "duplicate attachments")]
    fn verify_rejects_duplicate_attachment() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.get_block_mut(entry).insts.push(value);
        cfg.set_terminator(entry, Terminator::Return { value: Some(value) });
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "declares index 1")]
    fn verify_rejects_wrong_block_param_index() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let param = cfg.add_block_param(entry, Type::I32);
        cfg.get_inst_mut(param).data = CfgInstData::BlockParam { index: 1 };
        cfg.set_terminator(entry, Terminator::Return { value: Some(param) });
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "has non-BlockParam data")]
    fn verify_rejects_wrong_block_param_data() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let param = cfg.add_block_param(entry, Type::I32);
        cfg.get_inst_mut(param).data = CfgInstData::Const(0);
        cfg.set_terminator(entry, Terminator::Return { value: Some(param) });
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "stores type Type::U64")]
    fn verify_rejects_wrong_block_param_stored_type() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let param = cfg.add_block_param(entry, Type::I32);
        cfg.get_block_mut(entry).params[0].1 = Type::U64;
        cfg.set_terminator(entry, Terminator::Return { value: Some(param) });
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "used before its definition")]
    fn verify_rejects_use_before_definition() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let later = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        let earlier = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Neg(later),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.get_block_mut(entry).insts.swap(0, 1);
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(earlier),
            },
        );
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "does not dominate the use")]
    fn verify_rejects_cross_block_non_dominating_use() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let left = cfg.new_block();
        let right = cfg.new_block();
        let cond = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::BoolConst(true),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Branch {
                cond,
                then_block: left,
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block: right,
                else_args: crate::payload::CfgElseArgs::EMPTY,
            },
        );
        let value = cfg.add_inst_to_block(
            left,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(left, Terminator::Return { value: Some(value) });
        cfg.set_terminator(right, Terminator::Return { value: Some(value) });
        cfg.verify().unwrap();
    }

    /// A terminator-less reachable entry plus two *unreachable* blocks, where
    /// the second reads a value defined in the first. Neither orphan dominates
    /// the other, so this is the shape that separates "unreachable uses are
    /// exempt" from "unreachable definitions dominate nothing". The caller
    /// terminates the entry, which is what picks between the two.
    fn cfg_with_unreachable_cross_block_use() -> (Cfg, CfgValue) {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;

        let orphan_def = cfg.new_block();
        let orphaned = cfg.add_inst_to_block(
            orphan_def,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(orphan_def, Terminator::Unreachable);

        let orphan_use = cfg.new_block();
        cfg.set_terminator(
            orphan_use,
            Terminator::Return {
                value: Some(orphaned),
            },
        );
        (cfg, orphaned)
    }

    #[test]
    fn verify_exempts_uses_inside_unreachable_blocks() {
        // No path reaches the use, so no definition can dominate it and there
        // is nothing to get wrong at run time. The orphans' structural checks
        // still run, per `verify_checks_slots_in_unreachable_blocks`.
        let (mut cfg, _) = cfg_with_unreachable_cross_block_use();
        let entry = cfg.entry;
        let live = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(live) });
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "does not dominate the use")]
    fn verify_rejects_reachable_use_of_unreachable_definition() {
        // Same graph, except the entry returns the orphan's value. An
        // unreachable definition dominates nothing, so a reachable use of it
        // is rejected.
        let (mut cfg, orphaned) = cfg_with_unreachable_cross_block_use();
        let entry = cfg.entry;
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(orphaned),
            },
        );
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "branch condition")]
    fn verify_rejects_non_bool_branch_condition() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.new_block();
        let cond = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Branch {
                cond,
                then_block: target,
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block: target,
                else_args: crate::payload::CfgElseArgs::EMPTY,
            },
        );
        cfg.set_terminator(target, Terminator::Unreachable);
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "has no value but function return type")]
    fn verify_rejects_missing_nonunit_return_value() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(entry, Terminator::Return { value: None });
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "unit-returning functions must use")]
    fn verify_rejects_explicit_unit_return_value() {
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "unit_return".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::UNIT,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(value) });
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "entry block bb7 is out of bounds")]
    fn verify_rejects_invalid_entry_before_indexing() {
        let mut cfg = unit_cfg();
        cfg.new_block();
        cfg.entry = BlockId::from_raw(7);
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "goto target bb9")]
    fn verify_rejects_invalid_target_before_indexing() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target: BlockId::from_raw(9),
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        cfg.verify().unwrap();
    }

    #[test]
    #[should_panic(expected = "local slot range 0..1")]
    fn verify_checks_slots_in_unreachable_blocks() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(value) });
        let orphan = cfg.new_block();
        cfg.add_inst_to_block(
            orphan,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.verify_with_type_pool(&FrozenTypeInternPool::new())
            .unwrap();
    }

    /// Build `entry: Return` plus an *unreachable* husk `orphan_from --goto(one
    /// arg)--> orphan_to` where `orphan_to` declares zero parameters. This is the
    /// pre-DCE shape LICM's preheader materialization trips over (RUE-927): a
    /// folded `if`'s dead predecessor still passing an argument to a merge block
    /// whose parameter `simplify` substituted away.
    fn cfg_with_unreachable_husk_edge() -> Cfg {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let entry_value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(entry_value),
            },
        );
        let orphan_from = cfg.new_block();
        let orphan_to = cfg.new_block();
        // orphan_to has zero parameters, but the husk edge passes one argument.
        cfg.set_terminator(orphan_to, Terminator::Return { value: None });
        let arg = cfg.add_inst_to_block(
            orphan_from,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        let args = cfg.push_goto_args(vec![arg]).unwrap();
        cfg.set_terminator(
            orphan_from,
            Terminator::Goto {
                target: orphan_to,
                args,
            },
        );
        cfg
    }

    #[test]
    #[should_panic(expected = "block arguments")]
    fn strict_verify_rejects_unreachable_husk_edge() {
        // The strict verifier every real pipeline boundary uses still checks
        // unreachable blocks, so the husk's stale arity is caught.
        cfg_with_unreachable_husk_edge().verify().unwrap();
    }

    #[test]
    fn materialization_verify_tolerates_unreachable_husk_edge() {
        // The mid-pipeline materialization verifier skips unreachable blocks, so
        // the transient pre-DCE husk does not masquerade as a materialization bug.
        cfg_with_unreachable_husk_edge()
            .verify_materialization_with_type_pool(&FrozenTypeInternPool::new())
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "block arguments")]
    fn materialization_verify_still_catches_reachable_arity_mismatch() {
        // A malformed edge among the *reachable* blocks — exactly what a botched
        // preheader materialization would produce — is still rejected.
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.new_block();
        let param = cfg.add_block_param(target, Type::I32);
        cfg.set_terminator(target, Terminator::Return { value: Some(param) });
        // Reachable edge passes zero arguments to a one-parameter block.
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        cfg.verify_materialization_with_type_pool(&FrozenTypeInternPool::new())
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "parameter slot range 0..1")]
    fn verify_rejects_invalid_parameter_slot() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Param { index: 0 },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&FrozenTypeInternPool::new())
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "local slot range 0..2")]
    fn verify_rejects_multi_slot_local_overflow() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let pair = register_struct(&pool, &interner, "Pair", &[Type::I32, Type::I32]);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "wide_local".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                ty: Type::new_struct(pair),
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    fn verify_slot_ranges_consume_the_frozen_pool_authority() {
        fn divergent_width(
            _pool: &FrozenTypeInternPool,
            _ty: Type,
        ) -> Result<u32, rue_air::TypeValidationError> {
            Ok(7)
        }

        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let pair = register_struct(&pool, &interner, "Pair", &[Type::I32, Type::I32]);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 2, 0, "canonical_width".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                ty: Type::new_struct(pair),
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);

        let mut verifier = Verifier::new(&cfg, Some(&pool), true);
        verifier.abi_slot_query_override = Some(divergent_width);
        let error = verifier.verify().unwrap_err();
        assert!(error.to_string().contains("local slot range 0..7"));
    }

    #[test]
    fn verify_reports_invalid_type_encoding_without_unwinding() {
        let mut cfg = Cfg::new(
            Type::UNIT,
            1,
            0,
            "invalid_type_encoding".to_string(),
            vec![],
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                // SAFETY: Type is one u32 field and every u32 is memory-valid;
                // malformedness is semantic. The raw constructor is
                // intentionally AIR-private, so reproduce packed-storage
                // corruption at this verifier boundary.
                ty: unsafe { std::mem::transmute::<u32, Type>(0x100) },
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cfg.finish(&FrozenTypeInternPool::new())
        }));
        let error = outcome
            .expect("malformed type verification must not unwind")
            .unwrap_err();
        assert!(error.to_string().contains("InvalidEncoding"));
    }

    #[test]
    fn verify_accepts_multi_slot_logical_type_in_one_by_ref_parameter_slot() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let pair = register_struct(&pool, &interner, "BorrowedPair", &[Type::I32, Type::I32]);
        let pool = pool.freeze();
        let pair_ty = Type::new_struct(pair);
        let mut cfg = Cfg::new(Type::UNIT, 0, 1, "borrowed_pair".to_string(), vec![true]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg
            .make_place(
                PlaceBase::Param(0),
                pair_ty,
                [Projection::Field {
                    struct_id: pair,
                    field_index: 1,
                }],
            )
            .unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    #[should_panic(expected = "references invalid struct id")]
    fn verify_rejects_invalid_struct_projection_id() {
        let pool = FrozenTypeInternPool::new();
        let source_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let foreign_struct = register_struct(&source_pool, &interner, "Foreign", &[Type::I32]);
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "invalid_struct".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                Type::I32,
                [Projection::Field {
                    struct_id: foreign_struct,
                    field_index: 0,
                }],
            )
            .unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    #[should_panic(expected = "references invalid array id")]
    fn verify_rejects_invalid_array_projection_id() {
        let pool = FrozenTypeInternPool::new();
        let array_type = TypeInternPool::new()
            .try_intern_array(Type::I32, 1)
            .unwrap();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "invalid_array".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let index = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::U64,
                span: Span::new(0, 0),
            },
        );
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                Type::I32,
                [Projection::Index { array_type, index }],
            )
            .unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    #[should_panic(expected = "references field 1")]
    fn verify_rejects_out_of_bounds_field_projection() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let record = register_struct(&pool, &interner, "Record", &[Type::I32]);
        let pool = pool.freeze();
        let record_ty = Type::new_struct(record);
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "bad_field".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                record_ty,
                [Projection::Field {
                    struct_id: record,
                    field_index: 1,
                }],
            )
            .unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    #[should_panic(expected = "previous link produced")]
    fn verify_rejects_broken_nested_projection_continuity() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let inner = register_struct(&pool, &interner, "Inner", &[Type::I32]);
        let wrong = register_struct(&pool, &interner, "Wrong", &[Type::I32]);
        let outer = register_struct(&pool, &interner, "Outer", &[Type::new_struct(inner)]);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "broken_chain".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                Type::new_struct(outer),
                [
                    Projection::Field {
                        struct_id: outer,
                        field_index: 0,
                    },
                    Projection::Field {
                        struct_id: wrong,
                        field_index: 0,
                    },
                ],
            )
            .unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    #[should_panic(expected = "projection chain produces Type::I32")]
    fn verify_rejects_place_read_result_type_mismatch() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let record = register_struct(&pool, &interner, "ResultRecord", &[Type::I32]);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "bad_result".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                Type::new_struct(record),
                [Projection::Field {
                    struct_id: record,
                    field_index: 0,
                }],
            )
            .unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    fn verify_rejects_invalid_extra_slice_before_slicing() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::ArrayInit {
                    elements: crate::payload::CfgArrayElements::malformed(u32::MAX, 2),
                },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        let error = cfg.verify().unwrap_err();
        assert_eq!(error.payload().unwrap().family(), "array elements");
        assert_eq!(
            error.location(),
            CfgVerificationLocation::Instruction {
                block: entry,
                value: CfgValue::from_raw(0),
            }
        );
    }

    #[test]
    fn verify_rejects_invalid_call_argument_slice_before_slicing() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Call {
                    runtime: None,
                    name: lasso::Spur::default(),
                    args: crate::payload::CfgCallArgs::malformed(1, 1),
                },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        let error = cfg.verify().unwrap_err();
        assert_eq!(error.payload().unwrap().family(), "call arguments");
        assert_eq!(
            error.location(),
            CfgVerificationLocation::Instruction {
                block: entry,
                value: CfgValue::from_raw(0),
            }
        );
    }

    #[test]
    fn verify_rejects_invalid_switch_case_slice_before_slicing() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let value = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Switch {
                scrutinee: value,
                cases: crate::payload::CfgSwitchCases::malformed(1, 1),
                default: entry,
            },
        );
        let error = cfg.verify().unwrap_err();
        assert_eq!(error.payload().unwrap().family(), "switch cases");
        assert_eq!(
            error.location(),
            CfgVerificationLocation::Terminator { block: entry }
        );
    }

    #[test]
    fn verify_rejects_invalid_projection_slice_before_slicing() {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "projection_slice".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead {
                    place: Place {
                        base: PlaceBase::Local(0),
                        base_type: Type::I32,
                        projections: crate::payload::CfgProjections::malformed(1, 1),
                    },
                },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        let error = cfg.verify().unwrap_err();
        assert_eq!(error.payload().unwrap().family(), "projections");
        assert_eq!(
            error.location(),
            CfgVerificationLocation::Instruction {
                block: entry,
                value: CfgValue::from_raw(0),
            }
        );
    }

    #[test]
    #[should_panic(expected = "projection index v0")]
    fn verify_rejects_non_integer_projection_index() {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "projection_index".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let index = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::BoolConst(false),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        let array_type = TypeInternPool::new()
            .try_intern_array(Type::I32, 1)
            .unwrap();
        let place = cfg
            .make_place(
                PlaceBase::Local(0),
                array_type,
                [Projection::Index { array_type, index }],
            )
            .unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        cfg.verify().unwrap();
    }

    #[test]
    fn optimize_verifies_before_and_after_dce() {
        for level in [OptLevel::O0, OptLevel::O1] {
            let mut cfg = unit_cfg();
            let entry = cfg.new_block();
            cfg.entry = entry;
            cfg.add_inst_to_block(
                entry,
                CfgInst {
                    data: CfgInstData::Const(99),
                    ty: Type::I32,
                    span: Span::new(0, 0),
                },
            );
            let result = cfg.add_inst_to_block(
                entry,
                CfgInst {
                    data: CfgInstData::Const(42),
                    ty: Type::I32,
                    span: Span::new(0, 0),
                },
            );
            cfg.set_terminator(
                entry,
                Terminator::Return {
                    value: Some(result),
                },
            );
            let pool = FrozenTypeInternPool::new();
            let cfg = cfg.finish(&pool).unwrap();
            opt::optimize(cfg, level, &pool).unwrap();
        }
    }

    #[test]
    fn o1_cannot_hide_unattached_value_with_dce() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst(CfgInst {
            data: CfgInstData::Const(99),
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        let result = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(42),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(result),
            },
        );
        let error = cfg.finish(&FrozenTypeInternPool::new()).unwrap_err();
        assert!(error.to_string().contains("is unattached"));
    }
}
