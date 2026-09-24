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
//! local/parameter slots, places, projections, edge arguments, conditions,
//! returns, and intrinsic call signatures are validated before any getter or
//! graph traversal can index them.
//! Once those structural preconditions hold, a forward dataflow pass verifies
//! explicit storage lifetimes, explicit Drop consumption, initialization of
//! unannotated compiler-owned slots such as runtime drop flags, and that no
//! place is dropped after a `MoveOut` on a path its drop flag does not guard.
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
use crate::payload::CfgIntrinsicArgs;
mod moves;

use rue_air::{
    AirArgMode, FrozenTypeInternPool, IntrinsicAirArgument, IntrinsicAirArgumentSource,
    IntrinsicOperation, Type, TypeKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OwnerRoot {
    Local { slot: u32, ty: Type },
    OwnedParam { slot: u32, ty: Type },
    WritableParam { slot: u32, ty: Type },
}

/// One reachable edge into a block, as the drop-flag guards read it
/// (RUE-2290): the true edge of a `Branch`, or anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryEdge {
    Then { cond: CfgValue, from: BlockId },
    Other,
}

/// A write to a compiler-owned slot that the drop-flag discipline allows: a
/// `Store` of zero or of a nonzero constant at the type the slot is loaded at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagWrite {
    Cleared,
    Armed,
}

/// What the drop-flag guards know about compiler-owned slots independently of
/// any one consumption fact, so every fact of a verifier run shares it
/// (RUE-2290, RUE-2347).
struct DropFlagSlots {
    /// Every reachable `Load` outside a declared storage region, with the type
    /// it is loaded at.
    raw_slots: ahash::AHashSet<(u32, Type)>,
    /// The slot numbers of `raw_slots`.
    slot_numbers: ahash::AHashSet<u32>,
    /// Each block's last write to each compiler-owned slot it writes, in the
    /// order the slots are first written; `None` is a write the discipline
    /// does not allow.
    block_writes: Vec<Vec<(u32, Option<FlagWrite>)>>,
    /// The compiler-owned slots cleared anywhere reachable, in first-seen
    /// order: only these can be a drop's flag.
    cleared_slots: Vec<u32>,
    /// Per slot, solved on first use: whether it is cleared on every path
    /// into each block.
    cleared_on_entry: std::cell::RefCell<ahash::AHashMap<u32, std::rc::Rc<Vec<bool>>>>,
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
    flag_clearing_solves: usize,
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
            flag_clearing_solves: 0,
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

    /// Verify against an empty fixture pool: the entry point for unit tests
    /// whose graphs mention only scalar types.
    ///
    /// The verifier reads aggregate widths, drop obligations, and projection
    /// chains from the pool, so a graph naming a struct, array, or enum type
    /// must be verified against the pool that defines it.
    #[cfg(test)]
    pub(crate) fn verify_with_fixture_pool(&self) -> Result<(), CfgVerificationError> {
        self.verify_with_type_pool(&FrozenTypeInternPool::new())
    }

    /// Verify the CFG's structural invariants against the active semantic type
    /// pool, reporting a precise, function- and block-localized message on any
    /// violation.
    ///
    /// See the module docs for the invariants and the rationale for the hard
    /// panic callers apply to the result. This is a compiler-bug guard: a
    /// well-formed pipeline never trips it; the check is paid to pin future
    /// lowering regressions to their source. The pool is what makes exact
    /// aggregate layouts and projection-chain validation possible, so it is
    /// required rather than optional.
    pub fn verify_with_type_pool(
        &self,
        type_pool: &FrozenTypeInternPool,
    ) -> Result<(), CfgVerificationError> {
        Verifier::new(self, type_pool, true).verify()
    }

    /// Verify the optimized live graph. DCE intentionally retains dead values
    /// in the arena after detaching them from blocks, so attachment completeness
    /// is checked before optimization while every remaining attachment and use
    /// is checked again afterward.
    pub(crate) fn verify_after_optimization_with_type_pool(
        &self,
        type_pool: &FrozenTypeInternPool,
    ) -> Result<(), CfgVerificationError> {
        Verifier::new(self, type_pool, false).verify()
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
        Verifier::materialization(self, type_pool).verify()
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
            | CfgInstData::FnAddr { .. }
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

            CfgInstData::PlaceRead { place } | CfgInstData::MoveOut { place } => {
                self.collect_place_operands(place, out)
            }
            CfgInstData::PlaceWrite { place, value } => {
                self.collect_place_operands(place, out);
                out.push(*value);
            }

            CfgInstData::Call { args, .. } | CfgInstData::AccessorCall { args, .. } => {
                for arg in self.call_args(args) {
                    out.push(arg.value);
                }
            }
            CfgInstData::CallIndirect { callee, args } => {
                out.push(*callee);
                for arg in self.call_args(args) {
                    out.push(arg.value);
                }
            }
            CfgInstData::Intrinsic { args, .. } => out.extend_from_slice(self.intrinsic_args(args)),

            CfgInstData::StructInit { fields, .. } => {
                out.extend_from_slice(self.struct_fields(fields))
            }
            CfgInstData::ArrayInit { elements, .. } => {
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

/// Name an intrinsic operand's structural origin for a verification report.
/// The address-taking intrinsics accept only a place-shaped operand, so the
/// origin is as much a part of the diagnosis as the type is.
fn describe_operand_source(source: IntrinsicAirArgumentSource) -> &'static str {
    match source {
        IntrinsicAirArgumentSource::Value => "a computed value",
        IntrinsicAirArgumentSource::Load => "a local load",
        IntrinsicAirArgumentSource::Param => "a parameter",
        IntrinsicAirArgumentSource::PlaceRead {
            terminal_field: true,
        } => "a field place read",
        IntrinsicAirArgumentSource::PlaceRead {
            terminal_field: false,
        } => "a place read",
    }
}

struct Verifier<'a> {
    cfg: &'a Cfg,
    type_pool: &'a FrozenTypeInternPool,
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
        type_pool: &'a FrozenTypeInternPool,
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
    fn materialization(cfg: &'a Cfg, type_pool: &'a FrozenTypeInternPool) -> Self {
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
    /// This intentionally does not reconstruct source ownership. A
    /// whole-slot Load may be a copy or a move; the builder's `MoveOut`
    /// markers name the moves of values that need dropping, and nothing
    /// else is inferred. It proves five bounded invariants that every
    /// publication boundary preserves:
    ///
    /// * an exact logical `(slot, type)` storage region is live on every path at
    ///   each local access, and Live/Dead transitions alternate on every path;
    /// * a non-phi SSA instruction result, or an exact whole-owner
    ///   local/by-value-parameter root, is not used after an explicit Drop on
    ///   any path without a fresh dynamic definition, a whole write, or a
    ///   runtime drop-flag guard that proves the root still owned (RUE-2290);
    /// * an unannotated compiler-owned slot that is loaded (notably a runtime
    ///   drop flag) has first been initialized by Store/Alloc on every path;
    /// * every tracked nonzero-width storage region is dead at a normal Return;
    /// * no value read after a `MoveOut` of an overlapping place, with no
    ///   write of it in between, is dropped, unless the place's runtime drop
    ///   flag guards the read (RUE-2367, `verify/moves.rs`).
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
        // The block defining each reachable value, where its exact-value
        // consumption fact starts (see `verify_exact_drop_fact_under`).
        let mut defining_blocks = vec![None; self.cfg.value_count()];

        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            for &(parameter, _) in &block.params {
                static_roots[parameter.as_u32() as usize] = RootFact::Unresolved;
                defining_blocks[parameter.as_u32() as usize] = Some(block.id);
            }
            for &value in &block.insts {
                defining_blocks[value.as_u32() as usize] = Some(block.id);
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
                        let dropped_ty = self.cfg.get_inst(dropped).ty;
                        // Validate nominal identities before recursive drop
                        // queries so malformed CFG returns a typed error.
                        self.abi_slot_count(dropped_ty, block.id, value, "Drop operand")?;
                        if self.type_pool.type_needs_drop(dropped_ty)
                            && droppable_value_set.insert(dropped)
                        {
                            droppable_values.push(dropped);
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
        // O(B + E + I + P + A + F * (B + E + I + O + T)); the drop-flag guard
        // scan and the second check it enables run only for a consumption
        // fact that fails without them, and are within its share plus a
        // lookup per cleared flag slot at each of the fact's drops.
        // The target-independent flag facts are shared: one scan, and one
        // O(B + E + W) solve per cleared slot, W the flag writes. Auxiliary
        // memory is O(B + V + P + A + F) plus the entry-edge table, O(E), and
        // O(W + B) per solved flag slot; no state dimension is multiplied by F.
        for key in storage_keys {
            self.verify_storage_fact(key)?;
        }
        for key in raw_keys {
            self.verify_raw_init_fact(key)?;
        }
        // How each reachable block is entered, for the drop-flag guards the
        // two consumption facts share (RUE-2290): one table, O(B + E).
        let mut entry_edges: Vec<Vec<EntryEdge>> = vec![Vec::new(); self.cfg.block_count()];
        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            if let Terminator::Branch {
                cond,
                then_block,
                else_block,
                ..
            } = &block.terminator
            {
                entry_edges[then_block.as_u32() as usize].push(EntryEdge::Then {
                    cond: *cond,
                    from: block.id,
                });
                entry_edges[else_block.as_u32() as usize].push(EntryEdge::Other);
            } else {
                self.for_each_semantic_edge(block.id, |target, _| {
                    entry_edges[target.as_u32() as usize].push(EntryEdge::Other);
                });
            }
        }
        // The drop-flag facts that do not depend on the target, computed once
        // and shared by every consumption fact (RUE-2347).
        let flag_slots = self.drop_flag_slots(raw_slots);
        for &value in &droppable_values {
            // Structural verification has checked that every reachable use,
            // this Drop included, is dominated by its definition, so the
            // definition lies in a reachable block.
            let start = defining_blocks[value.as_u32() as usize]
                .expect("a dropped value's definition dominates its Drop, so it is reachable");
            self.verify_exact_drop_fact(value, start, &flag_slots, &entry_edges)?;
        }
        for root in owner_roots {
            self.verify_owner_root_fact(
                root,
                &drop_roots,
                &value_roots,
                &flag_slots,
                &entry_edges,
            )?;
        }
        self.verify_move_facts(&value_roots, &flag_slots, &entry_edges)?;
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

    fn solve_semantic_fact(&self, transfer: impl FnMut(BlockId, u8) -> u8) -> Vec<u8> {
        self.solve_semantic_fact_from(self.cfg.entry, transfer, None)
    }

    /// Solve a two-state fact forward from `start`, which is entered in
    /// state A. Blocks not reachable from `start` keep input 0 (no state).
    /// When `reached` is given, every block that received a state is pushed
    /// onto it once, so a caller can check only those blocks.
    fn solve_semantic_fact_from(
        &self,
        start: BlockId,
        mut transfer: impl FnMut(BlockId, u8) -> u8,
        mut reached: Option<&mut Vec<BlockId>>,
    ) -> Vec<u8> {
        use std::collections::VecDeque;

        let block_count = self.cfg.block_count();
        let mut inputs = vec![0u8; block_count];
        let mut queued = vec![false; block_count];
        let mut queue = VecDeque::new();
        let start_index = start.as_u32() as usize;
        inputs[start_index] = SEMANTIC_STATE_A;
        queued[start_index] = true;
        queue.push_back(start);
        if let Some(reached) = reached.as_deref_mut() {
            reached.push(start);
        }

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
                    if inputs[target_index] == 0
                        && let Some(reached) = reached.as_deref_mut()
                    {
                        reached.push(target);
                    }
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

    /// The exact-value consumption fact of `target`. It is first checked with
    /// no drop-flag exemption, and only a failure pays for the drop-flag proof
    /// and a second check under it: the exemption only ever turns a block
    /// fresh, so it cannot make a passing fact fail.
    fn verify_exact_drop_fact(
        &self,
        target: CfgValue,
        start: BlockId,
        flag_slots: &DropFlagSlots,
        entry_edges: &[Vec<EntryEdge>],
    ) -> Result<(), CfgVerificationError> {
        let Err(error) = self.verify_exact_drop_fact_under(target, start, &[]) else {
            return Ok(());
        };
        // Store-to-load forwarding can make both the explicit and the guarded
        // exit drop name the value itself rather than a load of its slot; the
        // value's whole write is then its store into that slot, or its own
        // definition.
        let guarded = self.drop_flag_guarded_blocks(
            flag_slots,
            entry_edges,
            false,
            &|data| matches!(data, CfgInstData::Drop { value: dropped } if *dropped == target),
            &|value, data| {
                value == target
                    || matches!(data, CfgInstData::Alloc { init, .. } if *init == target)
                    || matches!(data, CfgInstData::Store { value, .. } if *value == target)
            },
        );
        if !guarded.contains(&true) {
            return Err(error);
        }
        self.verify_exact_drop_fact_under(target, start, &guarded)
    }

    /// The exact-value fact with the blocks in `guarded` (indexed by block;
    /// missing entries are unguarded) starting fresh.
    ///
    /// The fact is solved and checked only over the blocks reachable from
    /// `start`, the block defining `target`. SSA definitions dominate their
    /// uses, so no other block can use or drop `target`, and the value is
    /// fresh at its definition whatever reaches the block. This keeps each
    /// fact's cost proportional to the region its value can reach instead
    /// of the whole CFG.
    ///
    /// Soundness depends on the order in `verify()`: the structural pass
    /// (`verify_use`, through `verify_inst` and `verify_terminator_use`)
    /// has already rejected every reachable use, Drop and edge argument not
    /// dominated by its definition before `verify_semantic_dataflow` runs.
    /// A caller that skips or reorders that pass would let a drop outside
    /// the definition's region go unchecked here.
    fn verify_exact_drop_fact_under(
        &self,
        target: CfgValue,
        start: BlockId,
        guarded: &[bool],
    ) -> Result<(), CfgVerificationError> {
        const FRESH: u8 = SEMANTIC_STATE_A;
        const CONSUMED: u8 = SEMANTIC_STATE_B;
        let guarded = |block: BlockId| guarded.get(block.as_u32() as usize) == Some(&true);
        let mut reached = Vec::new();
        let inputs = self.solve_semantic_fact_from(
            start,
            |block, mut state| {
                if guarded(block) {
                    state = FRESH;
                }
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
            },
            Some(&mut reached),
        );

        // Block order, so the first error reported is the one a scan of
        // the whole CFG would find.
        reached.sort_unstable_by_key(|block| block.as_u32());
        for block in reached.into_iter().map(|block| self.cfg.get_block(block)) {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            let mut state = inputs[block.id.as_u32() as usize];
            if guarded(block.id) {
                state = FRESH;
            }
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

    /// Every write to a compiler-owned slot, through any channel: a `Store` of
    /// a constant at the type the slot is loaded at is a clearing or an arming;
    /// a `Store` of anything else, or at another type, an `Alloc`, or a
    /// `PlaceWrite` based on the slot is a write the discipline does not allow
    /// (`None`), and the slot exempts nothing.
    fn flag_write(
        &self,
        flag_slots: &DropFlagSlots,
        data: &CfgInstData,
    ) -> Option<(u32, Option<FlagWrite>)> {
        let (slot, write) = match data {
            CfgInstData::Store {
                slot,
                value: stored,
            } => {
                if !flag_slots.slot_numbers.contains(slot) {
                    return None;
                }
                let stored = self.cfg.get_inst(*stored);
                let write = if flag_slots.raw_slots.contains(&(*slot, stored.ty)) {
                    match stored.data {
                        CfgInstData::Const(0) => Some(FlagWrite::Cleared),
                        CfgInstData::Const(_) => Some(FlagWrite::Armed),
                        _ => None,
                    }
                } else {
                    None
                };
                (*slot, write)
            }
            CfgInstData::Alloc { slot, .. } => (*slot, None),
            CfgInstData::PlaceWrite { place, .. } => match place.base {
                PlaceBase::Local(slot) => (slot, None),
                PlaceBase::Param(_) | PlaceBase::Accessor(_) | PlaceBase::Indirect(_) => {
                    return None;
                }
            },
            _ => return None,
        };
        flag_slots
            .slot_numbers
            .contains(&slot)
            .then_some((slot, write))
    }

    /// One scan of the reachable instructions for the drop-flag facts that do
    /// not depend on the target: each block's writes to compiler-owned slots,
    /// and which slots are ever cleared.
    fn drop_flag_slots(&self, raw_slots: ahash::AHashSet<(u32, Type)>) -> DropFlagSlots {
        let slot_numbers = raw_slots.iter().map(|&(slot, _)| slot).collect();
        let mut flag_slots = DropFlagSlots {
            raw_slots,
            slot_numbers,
            block_writes: vec![Vec::new(); self.cfg.block_count()],
            cleared_slots: Vec::new(),
            cleared_on_entry: Default::default(),
        };
        if flag_slots.slot_numbers.is_empty() {
            return flag_slots;
        }
        let mut cleared = ahash::AHashSet::new();
        for block in self.cfg.blocks() {
            if !self.dominators().is_reachable(block.id) {
                continue;
            }
            let mut writes: Vec<(u32, Option<FlagWrite>)> = Vec::new();
            for &value in &block.insts {
                let Some((slot, write)) =
                    self.flag_write(&flag_slots, &self.cfg.get_inst(value).data)
                else {
                    continue;
                };
                if write == Some(FlagWrite::Cleared) && cleared.insert(slot) {
                    flag_slots.cleared_slots.push(slot);
                }
                match writes.iter_mut().find(|(written, _)| *written == slot) {
                    Some(entry) => entry.1 = write,
                    None => writes.push((slot, write)),
                }
            }
            flag_slots.block_writes[block.id.as_u32() as usize] = writes;
        }
        flag_slots
    }

    /// Whether `slot` is cleared on every path into `block`: its last write on
    /// each reaching path is a clearing, and some write reaches (the entry
    /// counts as not cleared). One two-state solve per slot per verifier run,
    /// over the per-block write summaries, shared by every fact.
    fn flag_cleared_at_entry(&self, flag_slots: &DropFlagSlots, slot: u32, block: BlockId) -> bool {
        const NOT_CLEARED: u8 = SEMANTIC_STATE_A;
        const CLEARED: u8 = SEMANTIC_STATE_B;
        let known = flag_slots.cleared_on_entry.borrow().get(&slot).cloned();
        let inputs = match known {
            Some(inputs) => inputs,
            None => {
                #[cfg(test)]
                SEMANTIC_WORK.with(|work| work.borrow_mut().flag_clearing_solves += 1);
                let states = self.solve_semantic_fact(|block, mut state| {
                    for &(written, write) in &flag_slots.block_writes[block.as_u32() as usize] {
                        if written == slot {
                            state = if write == Some(FlagWrite::Cleared) {
                                CLEARED
                            } else {
                                NOT_CLEARED
                            };
                        }
                    }
                    state
                });
                let inputs =
                    std::rc::Rc::new(states.iter().map(|&state| state == CLEARED).collect());
                flag_slots
                    .cleared_on_entry
                    .borrow_mut()
                    .insert(slot, std::rc::Rc::clone(&inputs));
                inputs
            }
        };
        inputs[block.as_u32() as usize]
    }

    /// The blocks a runtime drop flag proves to see an owned target (RUE-2290),
    /// indexed by block.
    ///
    /// The builder lowers `@drop(x)` in one arm of an `if` as an explicit `Drop`
    /// preceded by a `Store` of zero to the binding's hidden flag slot, and
    /// guards the later scope-exit drop of the same binding with `flag != 0`
    /// (`build.rs`, `update_drop_flag` and `emit_guarded`). The consumption
    /// facts are path-insensitive, so without this they would see the guarded
    /// exit drop as a use after the explicit drop. The flag is not declared to
    /// the verifier, so the exemption is earned rather than assumed. A
    /// compiler-owned slot (`flag_slots`) is the target's flag only when, on
    /// every reachable block:
    ///
    /// * each drop of the target is reached, on every path, through a store of
    ///   zero to the slot with no other write to it in between, except a drop
    ///   in the slot's own guard body (a block entered only through the true
    ///   edges of `slot != 0` tests), which is the guarded exit drop itself.
    ///   The store is usually earlier in the drop's own block; a `match`
    ///   clears the flag where it moves the scrutinee, before its switch, and
    ///   drops the moved value in an arm (RUE-2347);
    /// * each nonzero store to the slot happens where the target has been
    ///   whole-written on every reaching path with no drop of it since (a
    ///   by-value parameter counts as written at entry);
    /// * nothing else writes the slot, through any channel (`Store`, `Alloc`,
    ///   `PlaceWrite`), at any type;
    /// * after a guard-body drop the slot is not loaded again on any path
    ///   before it is written, so the stale flag proves nothing.
    ///
    /// Then `slot != 0` at a test implies the target has not been dropped
    /// since its last whole write, and the guard body — each test's load of
    /// the flag being in the branching block and followed there by no store to
    /// the flag and no drop of the target — starts fresh. A read anywhere else
    /// after the explicit drop is still an error, and a slot that breaks the
    /// discipline exempts nothing, so a flag-maintenance bug in the builder
    /// stays loud.
    ///
    /// `drops_target` and `writes_target` name the fact's own notion of a drop
    /// and a whole write, so the owner-root and the exact-value facts share
    /// the proof; `entry_initialized` says the target is whole at function
    /// entry with no write in the graph, as a by-value parameter is. What does
    /// not depend on the target (`flag_slots`: each block's flag writes, the
    /// cleared slots, and the per-slot every-path clearing solve) is computed
    /// once per verifier run and shared by every fact. It runs only for a
    /// fact that fails without the exemption. Per fact: nothing when
    /// no slot is ever cleared; otherwise two scans of the reachable
    /// instructions and one of the entry edges, a lookup per cleared slot at
    /// each drop of the target, one two-state solve for where the target is
    /// written, and one per flag that has a guard-body drop.
    fn drop_flag_guarded_blocks(
        &self,
        flag_slots: &DropFlagSlots,
        entry_edges: &[Vec<EntryEdge>],
        entry_initialized: bool,
        drops_target: &dyn Fn(&CfgInstData) -> bool,
        writes_target: &dyn Fn(CfgValue, &CfgInstData) -> bool,
    ) -> Vec<bool> {
        use ahash::{AHashMap, AHashSet};

        let block_count = self.cfg.block_count();
        let mut guarded = vec![false; block_count];
        if flag_slots.cleared_slots.is_empty() {
            return guarded;
        }
        let reachable = |block: BlockId| self.dominators().is_reachable(block);
        let flag_write = |data: &CfgInstData| self.flag_write(flag_slots, data);

        // Candidates: every compiler-owned slot that some drop of the target
        // sees cleared, either by the last write to it earlier in the drop's
        // own block or, with none there, on every path into that block. A
        // `match` moves its scrutinee (clearing the flag) in the block that
        // ends in the switch and drops it in an arm below it (RUE-2347).
        // Candidacy only bounds the work; the discipline below is the proof.
        let mut candidates = AHashSet::<u32>::new();
        for block in self.cfg.blocks() {
            if !reachable(block.id) {
                continue;
            }
            let mut last_write = AHashMap::<u32, Option<FlagWrite>>::new();
            for &value in &block.insts {
                let data = &self.cfg.get_inst(value).data;
                if let Some((slot, write)) = flag_write(data) {
                    last_write.insert(slot, write);
                } else if drops_target(data) {
                    for &slot in &flag_slots.cleared_slots {
                        let cleared = match last_write.get(&slot) {
                            Some(write) => *write == Some(FlagWrite::Cleared),
                            None => self.flag_cleared_at_entry(flag_slots, slot, block.id),
                        };
                        if cleared {
                            candidates.insert(slot);
                        }
                    }
                }
            }
        }
        if candidates.is_empty() {
            return guarded;
        }

        // The guard body of a candidate: a block entered only through true
        // edges of `slot != 0` tests of that one slot.
        let tested_slot = |cond: CfgValue, branching: BlockId| -> Option<u32> {
            let CfgInstData::Ne(lhs, rhs) = self.cfg.get_inst(cond).data else {
                return None;
            };
            let load_of_flag = |load: CfgValue, zero: CfgValue| -> Option<(CfgValue, u32)> {
                match self.cfg.get_inst(load).data {
                    CfgInstData::Load { slot }
                        if candidates.contains(&slot)
                            && matches!(self.cfg.get_inst(zero).data, CfgInstData::Const(0)) =>
                    {
                        Some((load, slot))
                    }
                    _ => None,
                }
            };
            let (load, slot) = load_of_flag(lhs, rhs).or_else(|| load_of_flag(rhs, lhs))?;
            // The test must read the flag in the branching block itself, with
            // nothing after the read that could stale it before the branch.
            let insts = &self.cfg.get_block(branching).insts;
            let position = insts.iter().position(|&value| value == load)?;
            insts[position + 1..]
                .iter()
                .all(|&value| {
                    let data = &self.cfg.get_inst(value).data;
                    flag_write(data).is_none_or(|(written, _)| written != slot)
                        && !drops_target(data)
                })
                .then_some(slot)
        };
        let mut guard_flag: Vec<Option<u32>> = vec![None; block_count];
        for block in self.cfg.blocks() {
            if !reachable(block.id) {
                continue;
            }
            let edges = &entry_edges[block.id.as_u32() as usize];
            let mut slots = edges.iter().map(|edge| match *edge {
                EntryEdge::Then { cond, from } => tested_slot(cond, from),
                EntryEdge::Other => None,
            });
            if let Some(Some(first)) = slots.next()
                && slots.all(|slot| slot == Some(first))
            {
                guard_flag[block.id.as_u32() as usize] = Some(first);
            }
        }

        // Where the target is whole: written on every reaching path with no
        // drop since. The whole write and the arming need not share a block;
        // inlining puts a callee's parameter store in the caller's block and
        // the arming in the callee's entry.
        const WRITTEN: u8 = SEMANTIC_STATE_A;
        const UNWRITTEN: u8 = SEMANTIC_STATE_B;
        let target_written = |block: BlockId, mut state: u8| -> u8 {
            if block == self.cfg.entry {
                state = if entry_initialized {
                    WRITTEN
                } else {
                    UNWRITTEN
                };
            }
            for &value in &self.cfg.get_block(block).insts {
                let data = &self.cfg.get_inst(value).data;
                if drops_target(data) {
                    state = UNWRITTEN;
                } else if writes_target(value, data) {
                    state = WRITTEN;
                }
            }
            state
        };
        let written = self.solve_semantic_fact(target_written);

        // The discipline, checked on every reachable block; a slot that breaks
        // it is no flag of the target. A guard-body drop is recorded for the
        // staleness solve below.
        let mut flags = candidates.clone();
        let mut guard_drops = AHashMap::<u32, AHashSet<CfgValue>>::new();
        for block in self.cfg.blocks() {
            if !reachable(block.id) {
                continue;
            }
            let body_of = guard_flag[block.id.as_u32() as usize];
            let mut last_write = AHashMap::<u32, FlagWrite>::new();
            let mut target_state = written[block.id.as_u32() as usize];
            if block.id == self.cfg.entry {
                target_state = if entry_initialized {
                    WRITTEN
                } else {
                    UNWRITTEN
                };
            }
            for &value in &block.insts {
                let data = &self.cfg.get_inst(value).data;
                if let Some((slot, write)) = flag_write(data) {
                    if !candidates.contains(&slot) {
                        continue;
                    }
                    match write {
                        Some(FlagWrite::Cleared) => {
                            last_write.insert(slot, FlagWrite::Cleared);
                        }
                        Some(FlagWrite::Armed) => {
                            if target_state & UNWRITTEN != 0 {
                                flags.remove(&slot);
                            }
                            last_write.insert(slot, FlagWrite::Armed);
                        }
                        None => {
                            flags.remove(&slot);
                        }
                    }
                } else if drops_target(data) {
                    for &slot in &candidates {
                        match last_write.get(&slot) {
                            Some(FlagWrite::Cleared) => continue,
                            None if self.flag_cleared_at_entry(flag_slots, slot, block.id) => {
                                continue;
                            }
                            _ => {}
                        }
                        if body_of == Some(slot) {
                            guard_drops.entry(slot).or_default().insert(value);
                        } else {
                            flags.remove(&slot);
                        }
                    }
                    target_state = UNWRITTEN;
                } else if writes_target(value, data) {
                    target_state = WRITTEN;
                }
            }
        }

        // After a guard-body drop the flag still reads nonzero; no path may
        // load it again before a store to it.
        const CLEAN: u8 = SEMANTIC_STATE_A;
        const STALE: u8 = SEMANTIC_STATE_B;
        for (&slot, drops) in &guard_drops {
            if !flags.contains(&slot) {
                continue;
            }
            let mut loaded_stale = false;
            self.solve_semantic_fact(|block, mut state| {
                for &value in &self.cfg.get_block(block).insts {
                    match self.cfg.get_inst(value).data {
                        CfgInstData::Load { slot: loaded } if loaded == slot => {
                            if state & STALE != 0 {
                                loaded_stale = true;
                            }
                        }
                        CfgInstData::Store { slot: written, .. } if written == slot => {
                            state = CLEAN;
                        }
                        _ if drops.contains(&value) => {
                            state = STALE;
                        }
                        _ => {}
                    }
                }
                state
            });
            if loaded_stale {
                flags.remove(&slot);
            }
        }

        for (index, body_of) in guard_flag.iter().enumerate() {
            guarded[index] = body_of.is_some_and(|slot| flags.contains(&slot));
        }
        guarded
    }

    /// The owner-root consumption fact of `root`, checked first with no
    /// drop-flag exemption like [`Self::verify_exact_drop_fact`].
    fn verify_owner_root_fact(
        &self,
        root: OwnerRoot,
        drop_roots: &[Option<OwnerRoot>],
        value_roots: &[Option<OwnerRoot>],
        flag_slots: &DropFlagSlots,
        entry_edges: &[Vec<EntryEdge>],
    ) -> Result<(), CfgVerificationError> {
        let Err(error) = self.verify_owner_root_fact_under(root, drop_roots, value_roots, &[])
        else {
            return Ok(());
        };
        let guarded = self.drop_flag_guarded_blocks(
            flag_slots,
            entry_edges,
            !matches!(root, OwnerRoot::Local { .. }),
            &|data| {
                matches!(data, CfgInstData::Drop { value: dropped }
                    if drop_roots[dropped.as_u32() as usize] == Some(root))
            },
            &|_, data| self.whole_write_root(data) == Some(root),
        );
        if !guarded.contains(&true) {
            return Err(error);
        }
        self.verify_owner_root_fact_under(root, drop_roots, value_roots, &guarded)
    }

    /// The owner-root fact with the blocks in `guarded` (indexed by block;
    /// missing entries are unguarded) starting fresh.
    fn verify_owner_root_fact_under(
        &self,
        root: OwnerRoot,
        drop_roots: &[Option<OwnerRoot>],
        value_roots: &[Option<OwnerRoot>],
        guarded: &[bool],
    ) -> Result<(), CfgVerificationError> {
        const FRESH: u8 = SEMANTIC_STATE_A;
        const CONSUMED: u8 = SEMANTIC_STATE_B;
        let guarded = |block: BlockId| guarded.get(block.as_u32() as usize) == Some(&true);
        let inputs = self.solve_semantic_fact(|block, mut state| {
            if guarded(block) {
                state = FRESH;
            }
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
            if guarded(block.id) {
                state = FRESH;
            }
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
                    CfgInstData::Call { args, .. }
                    | CfgInstData::AccessorCall { args, .. }
                    | CfgInstData::CallIndirect { args, .. } => {
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
                    CfgInstData::ArrayInit { elements, .. } => {
                        self.cfg
                            .checked_array_elements(elements)
                            .map_err(|error| self.payload_error(location, error))?;
                    }
                    CfgInstData::EnumVariant { payload, .. } => {
                        self.cfg
                            .checked_enum_payload(payload)
                            .map_err(|error| self.payload_error(location, error))?;
                    }
                    CfgInstData::PlaceRead { place }
                    | CfgInstData::PlaceWrite { place, .. }
                    | CfgInstData::MoveOut { place } => {
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
            CfgInstData::PlaceRead { place }
            | CfgInstData::PlaceWrite { place, .. }
            | CfgInstData::MoveOut { place } => self.verify_place(block, value, place)?,
            CfgInstData::Call { .. } | CfgInstData::AccessorCall { .. } => {
                self.verify_call_contract(block, value, inst.ty)?
            }
            CfgInstData::FnAddr { .. } => {
                if !inst.ty.is_function() {
                    return Err(self.semantic_error(
                        CfgVerificationLocation::Instruction { block, value },
                        format_args!(
                            "fn_addr {} in block {} must have a `fn` type; found {:?}",
                            value, block, inst.ty
                        ),
                    ));
                }
            }
            CfgInstData::CallIndirect { callee, .. } => {
                let callee_ty = self.inst(*callee, block, "indirect callee")?.ty;
                if !callee_ty.is_function() {
                    return Err(self.semantic_error(
                        CfgVerificationLocation::Instruction { block, value },
                        format_args!(
                            "call_indirect {} in block {} calls through {:?}, which is not a `fn` type",
                            value, block, callee_ty
                        ),
                    ));
                }
                self.verify_call_contract(block, value, inst.ty)?
            }
            CfgInstData::Intrinsic {
                operation, args, ..
            } => self.verify_intrinsic_operands(block, value, *operation, inst.ty, args)?,
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

    /// Re-prove an ordinary or accessor call against the effective AIR
    /// contract captured when this CFG value was built. Optimizations may
    /// substitute operands, modes, or result metadata, but they must not
    /// change the callee contract.
    fn verify_call_contract(
        &self,
        block: BlockId,
        value: CfgValue,
        result_ty: Type,
    ) -> Result<(), CfgVerificationError> {
        let Some(contract) = self.cfg.call_contract(value) else {
            return Err(self.semantic_error(
                CfgVerificationLocation::Instruction { block, value },
                format_args!(
                    "call instruction {} in block {} has no established AIR call contract",
                    value, block
                ),
            ));
        };
        let args = match &self.cfg.get_inst(value).data {
            CfgInstData::Call { args, .. }
            | CfgInstData::AccessorCall { args, .. }
            | CfgInstData::CallIndirect { args, .. } => self.cfg.call_args(args),
            _ => unreachable!("call contract attached to a non-call instruction"),
        };
        if result_ty != contract.result {
            return Err(self.semantic_error(
                CfgVerificationLocation::Instruction { block, value },
                format_args!(
                    "call instruction {} in block {} no longer has its established result type {:?}; found {:?}",
                    value, block, contract.result, result_ty
                ),
            ));
        }
        if args.len() != contract.arguments.len() {
            return Err(self.semantic_error(
                CfgVerificationLocation::Instruction { block, value },
                format_args!(
                    "call instruction {} in block {} no longer has its established argument count {}; found {}",
                    value, block, contract.arguments.len(), args.len()
                ),
            ));
        }
        for (index, (arg, expected)) in args.iter().zip(contract.arguments.iter()).enumerate() {
            let actual_ty = self.inst(arg.value, block, "call argument")?.ty;
            if actual_ty != expected.ty || arg.mode != expected.mode {
                return Err(self.semantic_error(
                    CfgVerificationLocation::Instruction { block, value },
                    format_args!(
                        "call instruction {} in block {} argument {} no longer satisfies its established contract: expected {:?} with mode {:?}, found {:?} with mode {:?}",
                        value, block, index, expected.ty, expected.mode, actual_ty, arg.mode
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Re-prove an intrinsic's operand and result types against the single
    /// authority that accepted them on AIR.
    ///
    /// CFG construction proves this contract once (`build.rs`), but both
    /// backends derive an intrinsic operand's marshalled width from the
    /// operand's *own* CFG type, so an optimization pass that substitutes a
    /// differently typed value into an operand silently rewrites the lowered
    /// call's ABI. That is the one channel RUE-2086's store-to-load hazard
    /// flowed through, and it reached codegen only because nothing after
    /// optimization re-checked operand types. Re-running
    /// [`IntrinsicOperation::validate_call`] keeps AIR, CFG construction and
    /// the optimized graph on one contract rather than a second, drifting
    /// copy of the signature table (RUE-2094).
    ///
    /// The operand's structural origin is read back out of the CFG, so the
    /// address-taking family (`@raw`/`@raw_mut`/`@field_ptr`) is held to the
    /// same "still a place read" requirement codegen depends on when it takes
    /// the operand's address (RUE-521).
    fn verify_intrinsic_operands(
        &self,
        block: BlockId,
        value: CfgValue,
        operation: IntrinsicOperation,
        result_ty: Type,
        args: &CfgIntrinsicArgs,
    ) -> Result<(), CfgVerificationError> {
        let operands = self.cfg.intrinsic_args(args);
        let mut arguments = Vec::with_capacity(operands.len());
        for &operand in operands {
            let inst = self.inst(operand, block, "intrinsic argument")?;
            arguments.push(IntrinsicAirArgument {
                ty: inst.ty,
                mode: AirArgMode::Normal,
                source: self.intrinsic_operand_source(operand),
            });
        }
        // An error type is a diagnostic placeholder, not a claim about a
        // value's ABI: a graph still carrying one is already reported and its
        // signature relationships are not meaningful.
        if result_ty == Type::ERROR || arguments.iter().any(|argument| argument.ty == Type::ERROR) {
            return Ok(());
        }
        if operation.validate_call(self.type_pool, &arguments, result_ty) {
            return Ok(());
        }
        let operand_summary = if operands.is_empty() {
            "no operands".to_string()
        } else {
            operands
                .iter()
                .zip(&arguments)
                .enumerate()
                .map(|(index, (operand, argument))| {
                    format!(
                        "operand {} ({}) has type {:?} from {}",
                        index,
                        operand,
                        argument.ty,
                        describe_operand_source(argument.source)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        Err(self.semantic_error(
            CfgVerificationLocation::Instruction { block, value },
            format_args!(
                "intrinsic {:?} instruction {} in block {} no longer satisfies its call signature: {}, result type is {:?}",
                operation, value, block, operand_summary, result_ty
            ),
        ))
    }

    /// The structural origin of an intrinsic operand as the optimized CFG
    /// presents it, in the vocabulary the shared AIR validator speaks.
    ///
    /// The three place-shaped instructions accepted here are exactly the three
    /// `rue_codegen::value_plan::addressable_value_plan` can lower — it returns
    /// `None` for everything else — so this classification is neither tighter
    /// nor looser than code generation's own requirement for an operand whose
    /// address is taken. A fourth place-shaped instruction has to be added in
    /// both places, and in `rue_air::intrinsic_air_argument_with_place_lookup`,
    /// which is the AIR-side original.
    ///
    /// Unlike every other verifier arm this reads an operand's *payload*, not
    /// just its type, and it runs before `verify_use` has proved that operand
    /// attached — post-optimization verification deliberately tolerates
    /// detached dead values in the arena. So the projection slice is taken
    /// through the checked accessor: a corrupt range degrades to `Value` and is
    /// reported as an ill-typed operand, rather than panicking inside the
    /// unchecked payload view.
    fn intrinsic_operand_source(&self, operand: CfgValue) -> IntrinsicAirArgumentSource {
        match &self.cfg.get_inst(operand).data {
            CfgInstData::Load { .. } => IntrinsicAirArgumentSource::Load,
            CfgInstData::Param { .. } => IntrinsicAirArgumentSource::Param,
            CfgInstData::PlaceRead { place } => IntrinsicAirArgumentSource::PlaceRead {
                terminal_field: matches!(
                    self.cfg
                        .checked_place_projections(place)
                        .ok()
                        .and_then(<[Projection]>::last),
                    Some(Projection::Field { .. })
                ),
            },
            _ => IntrinsicAirArgumentSource::Value,
        }
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
        let canonical_width = || self.type_pool.try_abi_slot_count(ty);
        #[cfg(test)]
        let width = self
            .abi_slot_query_override
            .map_or_else(canonical_width, |query| query(self.type_pool, ty));
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
        let pool = self.type_pool;
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
        let is_fixed_str = matches!(
            pool.text_view_kind(source_id),
            Some(rue_air::TextViewKind::StrFixed(_))
        );
        is_fixed_str
            && pool.text_view_kind(result_id) == Some(rue_air::TextViewKind::Str)
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
        let pool = self.type_pool;
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
            | CfgInstData::FnAddr { .. }
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
            CfgInstData::PlaceRead { place } | CfgInstData::MoveOut { place } => {
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
            CfgInstData::CallIndirect { callee, args } => {
                f(*callee, "indirect callee");
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
            CfgInstData::ArrayInit { elements, .. } => {
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
        BlockId, Cfg, CfgArgMode, CfgCallArg, CfgCallContract, CfgCallContractArg, CfgInst,
        CfgInstData, CfgValue, Place, PlaceBase, Projection, Terminator,
    };
    use crate::{CfgVerificationLocation, OptLevel, opt};
    use lasso::ThreadedRodeo;
    use rue_air::{
        FrozenTypeInternPool, IntrinsicOperation, StructDef, StructField, StructId, Type,
        TypeInternPool, TypeKind,
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

    /// The CFG `build.rs` emits for `let r: R = R {}; if c { @drop(r); } 0`
    /// (RUE-2290): the flag in slot 1 is armed at the binding, cleared before
    /// the explicit drop in one arm, and tested before the scope-exit drop.
    /// `read_in_join` adds an unguarded read of the root at the join;
    /// `guard_slot` chooses which slot the exit-drop guard tests.
    /// How `conditional_explicit_drop_cfg` deviates from the builder's shape,
    /// each deviation a way the flag could stop proving ownership.
    #[derive(Default, Clone, Copy)]
    struct ConditionalDropShape {
        /// An unguarded read of the root at the join.
        read_in_join: bool,
        /// The slot the exit-drop guard tests; 0 (the default) means the
        /// root's flag, slot 1.
        guard_slot: u32,
        /// The skip arm also drops the root, without clearing the flag.
        skip_arm_drops: bool,
        /// The join re-arms the flag without a whole write of the root,
        /// through the given channel.
        rearm_in_join: Option<RearmChannel>,
        /// A second path (a goto from the skip arm) enters the guard body.
        extra_guard_entry: bool,
        /// After the guarded drop, the flag is tested again to guard a second
        /// read of the root.
        retest_after_exit_drop: bool,
        /// After the guarded drop, a second branch on the join's own test
        /// value (as common-subexpression elimination would leave it: no
        /// second load of the flag) guards a second drop of the root.
        rebranch_on_stale_test: bool,
    }

    /// A way the join can write the flag slot other than the builder's `Store`
    /// of an `I32` constant.
    #[derive(Clone, Copy)]
    enum RearmChannel {
        /// The builder's own channel.
        StoreConst,
        /// A `Store` of a constant at a type the flag is never loaded at.
        StoreOtherType,
        /// An `Alloc` of the slot.
        Alloc,
        /// A `PlaceWrite` based on the slot.
        PlaceWrite,
    }

    fn conditional_explicit_drop_cfg(shape: ConditionalDropShape) -> (Cfg, FrozenTypeInternPool) {
        let ConditionalDropShape {
            read_in_join,
            guard_slot,
            skip_arm_drops,
            rearm_in_join,
            extra_guard_entry,
            retest_after_exit_drop,
            rebranch_on_stale_test,
        } = shape;
        let guard_slot = if guard_slot == 0 { 1 } else { guard_slot };
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "MaybeDroppedOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            3,
            1,
            "conditional_drop".to_string(),
            vec![false],
        );
        let entry = cfg.new_block();
        let drop_arm = cfg.new_block();
        let skip_arm = cfg.new_block();
        let join = cfg.new_block();
        let exit_drop = cfg.new_block();
        let exit = cfg.new_block();
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
        let owned = init_nonzero_owner(&mut cfg, entry, owner, 5);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc {
                slot: 0,
                init: owned,
            },
            Type::UNIT,
        );
        for slot in [1, 2] {
            let armed = push(&mut cfg, entry, CfgInstData::Const(1), Type::I32);
            push(
                &mut cfg,
                entry,
                CfgInstData::Store { slot, value: armed },
                Type::UNIT,
            );
        }
        let cond = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::BOOL);
        cfg.set_branch(entry, cond, drop_arm, [], skip_arm, []);

        let dropped = push(&mut cfg, drop_arm, CfgInstData::Load { slot: 0 }, owner);
        let cleared = push(&mut cfg, drop_arm, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            drop_arm,
            CfgInstData::Store {
                slot: 1,
                value: cleared,
            },
            Type::UNIT,
        );
        push(
            &mut cfg,
            drop_arm,
            CfgInstData::Drop { value: dropped },
            Type::UNIT,
        );
        cfg.set_goto(drop_arm, join, []);
        if skip_arm_drops {
            let dropped = push(&mut cfg, skip_arm, CfgInstData::Load { slot: 0 }, owner);
            push(
                &mut cfg,
                skip_arm,
                CfgInstData::Drop { value: dropped },
                Type::UNIT,
            );
        }
        if extra_guard_entry {
            cfg.set_goto(skip_arm, exit_drop, []);
        } else {
            cfg.set_goto(skip_arm, join, []);
        }

        if read_in_join {
            push(&mut cfg, join, CfgInstData::Load { slot: 0 }, owner);
        }
        match rearm_in_join {
            None => {}
            Some(RearmChannel::StoreConst) => {
                let armed = push(&mut cfg, join, CfgInstData::Const(1), Type::I32);
                push(
                    &mut cfg,
                    join,
                    CfgInstData::Store {
                        slot: 1,
                        value: armed,
                    },
                    Type::UNIT,
                );
            }
            Some(RearmChannel::StoreOtherType) => {
                let armed = push(&mut cfg, join, CfgInstData::Const(1), Type::BOOL);
                push(
                    &mut cfg,
                    join,
                    CfgInstData::Store {
                        slot: 1,
                        value: armed,
                    },
                    Type::UNIT,
                );
            }
            Some(RearmChannel::Alloc) => {
                let armed = push(&mut cfg, join, CfgInstData::Const(1), Type::I32);
                push(
                    &mut cfg,
                    join,
                    CfgInstData::Alloc {
                        slot: 1,
                        init: armed,
                    },
                    Type::UNIT,
                );
            }
            Some(RearmChannel::PlaceWrite) => {
                let armed = push(&mut cfg, join, CfgInstData::Const(1), Type::I32);
                push(
                    &mut cfg,
                    join,
                    CfgInstData::PlaceWrite {
                        place: Place::local(1, Type::I32),
                        value: armed,
                    },
                    Type::UNIT,
                );
            }
        }
        let flag = push(
            &mut cfg,
            join,
            CfgInstData::Load { slot: guard_slot },
            Type::I32,
        );
        let zero = push(&mut cfg, join, CfgInstData::Const(0), Type::I32);
        let live = push(&mut cfg, join, CfgInstData::Ne(flag, zero), Type::BOOL);
        cfg.set_branch(join, live, exit_drop, [], exit, []);

        let remaining = push(&mut cfg, exit_drop, CfgInstData::Load { slot: 0 }, owner);
        push(
            &mut cfg,
            exit_drop,
            CfgInstData::Drop { value: remaining },
            Type::UNIT,
        );
        if rebranch_on_stale_test {
            // The join's test value, not a fresh load, guards a second drop:
            // only the adjacency clause (the load must sit in the branching
            // block) can see that the flag it read is stale here.
            let second = cfg.new_block();
            let again = cfg.new_block();
            cfg.set_goto(exit_drop, second, []);
            cfg.set_branch(second, live, again, [], exit, []);
            let twice = push(&mut cfg, again, CfgInstData::Load { slot: 0 }, owner);
            push(
                &mut cfg,
                again,
                CfgInstData::Drop { value: twice },
                Type::UNIT,
            );
            cfg.set_goto(again, exit, []);
        } else {
            cfg.set_goto(exit_drop, exit, []);
        }

        if retest_after_exit_drop {
            let retest = cfg.new_block();
            let dead = cfg.new_block();
            let flag = push(&mut cfg, exit, CfgInstData::Load { slot: 1 }, Type::I32);
            let zero = push(&mut cfg, exit, CfgInstData::Const(0), Type::I32);
            let live = push(&mut cfg, exit, CfgInstData::Ne(flag, zero), Type::BOOL);
            cfg.set_branch(exit, live, retest, [], dead, []);
            push(&mut cfg, retest, CfgInstData::Load { slot: 0 }, owner);
            cfg.set_goto(retest, dead, []);
            push(
                &mut cfg,
                dead,
                CfgInstData::StorageDead {
                    slot: 0,
                    local_ty: owner,
                },
                Type::UNIT,
            );
            cfg.set_terminator(dead, Terminator::Return { value: None });
            return (cfg, pool);
        }
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
        (cfg, pool)
    }

    /// The same program after store-to-load forwarding at `-O2`: both drops
    /// name the initial value itself, so the exact-value fact, not the
    /// owner-root fact, is the one that must understand the flag.
    fn forwarded_conditional_drop_cfg(clears_flag: bool) -> (Cfg, FrozenTypeInternPool) {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "ForwardedOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 2, 1, "forwarded_drop".to_string(), vec![false]);
        let entry = cfg.new_block();
        let drop_arm = cfg.new_block();
        let skip_arm = cfg.new_block();
        let join = cfg.new_block();
        let exit_drop = cfg.new_block();
        let exit = cfg.new_block();
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
        let owned = init_nonzero_owner(&mut cfg, entry, owner, 5);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc {
                slot: 0,
                init: owned,
            },
            Type::UNIT,
        );
        let armed = push(&mut cfg, entry, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            entry,
            CfgInstData::Store {
                slot: 1,
                value: armed,
            },
            Type::UNIT,
        );
        let cond = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::BOOL);
        cfg.set_branch(entry, cond, drop_arm, [], skip_arm, []);

        if clears_flag {
            let cleared = push(&mut cfg, drop_arm, CfgInstData::Const(0), Type::I32);
            push(
                &mut cfg,
                drop_arm,
                CfgInstData::Store {
                    slot: 1,
                    value: cleared,
                },
                Type::UNIT,
            );
        }
        push(
            &mut cfg,
            drop_arm,
            CfgInstData::Drop { value: owned },
            Type::UNIT,
        );
        cfg.set_goto(drop_arm, join, []);
        cfg.set_goto(skip_arm, join, []);

        let flag = push(&mut cfg, join, CfgInstData::Load { slot: 1 }, Type::I32);
        let zero = push(&mut cfg, join, CfgInstData::Const(0), Type::I32);
        let live = push(&mut cfg, join, CfgInstData::Ne(flag, zero), Type::BOOL);
        cfg.set_branch(join, live, exit_drop, [], exit, []);

        push(
            &mut cfg,
            exit_drop,
            CfgInstData::Drop { value: owned },
            Type::UNIT,
        );
        cfg.set_goto(exit_drop, exit, []);

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
        (cfg, pool)
    }

    /// How `match_scrutinee_drop_cfg` deviates from the builder's shape.
    #[derive(Default, Clone, Copy)]
    struct MatchDropShape {
        /// Both drops name the initial value itself, as store-to-load
        /// forwarding leaves them at `-O2`.
        forwarded: bool,
        /// One arm re-arms the flag before joining the other arm's drop. The
        /// target is still whole-written on that path, so only the check that
        /// the clearing reaches the drop on every path can reject it.
        rearm_on_one_arm: bool,
    }

    /// The CFG `build.rs` emits for a `match` on a destructor-bearing binding
    /// in one arm of an `if` (RUE-2347): the flag in slot 1 is armed at the
    /// binding, cleared where the `match` moves its scrutinee (before the
    /// switch, here a branch on a second parameter), and the moved value is
    /// dropped in an arm below that block; the scope-exit drop is guarded by
    /// the flag.
    fn match_scrutinee_drop_cfg(shape: MatchDropShape) -> (Cfg, FrozenTypeInternPool) {
        let MatchDropShape {
            forwarded,
            rearm_on_one_arm,
        } = shape;
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "MatchedOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            2,
            2,
            "match_scrutinee_drop".to_string(),
            vec![false, false],
        );
        let entry = cfg.new_block();
        let scrutinize = cfg.new_block();
        let first_arm = cfg.new_block();
        let second_arm = cfg.new_block();
        let skip = cfg.new_block();
        let join = cfg.new_block();
        let exit_drop = cfg.new_block();
        let exit = cfg.new_block();
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
        let owned = init_nonzero_owner(&mut cfg, entry, owner, 5);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc {
                slot: 0,
                init: owned,
            },
            Type::UNIT,
        );
        let armed = push(&mut cfg, entry, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            entry,
            CfgInstData::Store {
                slot: 1,
                value: armed,
            },
            Type::UNIT,
        );
        let cond = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::BOOL);
        cfg.set_branch(entry, cond, scrutinize, [], skip, []);

        let moved = if forwarded {
            owned
        } else {
            push(&mut cfg, scrutinize, CfgInstData::Load { slot: 0 }, owner)
        };
        let cleared = push(&mut cfg, scrutinize, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            scrutinize,
            CfgInstData::Store {
                slot: 1,
                value: cleared,
            },
            Type::UNIT,
        );
        let which = push(
            &mut cfg,
            scrutinize,
            CfgInstData::Param { index: 1 },
            Type::BOOL,
        );
        cfg.set_branch(scrutinize, which, first_arm, [], second_arm, []);

        push(
            &mut cfg,
            first_arm,
            CfgInstData::Drop { value: moved },
            Type::UNIT,
        );
        cfg.set_goto(first_arm, join, []);
        if rearm_on_one_arm {
            let armed = push(&mut cfg, second_arm, CfgInstData::Const(1), Type::I32);
            push(
                &mut cfg,
                second_arm,
                CfgInstData::Store {
                    slot: 1,
                    value: armed,
                },
                Type::UNIT,
            );
            cfg.set_goto(second_arm, first_arm, []);
        } else {
            push(
                &mut cfg,
                second_arm,
                CfgInstData::Drop { value: moved },
                Type::UNIT,
            );
            cfg.set_goto(second_arm, join, []);
        }
        cfg.set_goto(skip, join, []);

        let flag = push(&mut cfg, join, CfgInstData::Load { slot: 1 }, Type::I32);
        let zero = push(&mut cfg, join, CfgInstData::Const(0), Type::I32);
        let live = push(&mut cfg, join, CfgInstData::Ne(flag, zero), Type::BOOL);
        cfg.set_branch(join, live, exit_drop, [], exit, []);

        let remaining = if forwarded {
            owned
        } else {
            push(&mut cfg, exit_drop, CfgInstData::Load { slot: 0 }, owner)
        };
        push(
            &mut cfg,
            exit_drop,
            CfgInstData::Drop { value: remaining },
            Type::UNIT,
        );
        cfg.set_goto(exit_drop, exit, []);

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
        (cfg, pool)
    }

    /// A shape the flag must not exempt. Each rejection here would also be
    /// rejected with the drop-flag exemption absent, so what a rejecting test
    /// pins is that the clause it deviates on is load-bearing: stubbing that
    /// clause out makes the shape verify.
    fn assert_consumed_root(shape: ConditionalDropShape) {
        let (cfg, pool) = conditional_explicit_drop_cfg(shape);
        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("reads already-consumed owner root"),
            "{error}"
        );
    }

    #[test]
    fn semantic_verifier_accepts_flag_guarded_exit_drop_after_conditional_explicit_drop() {
        let (cfg, pool) = conditional_explicit_drop_cfg(ConditionalDropShape::default());
        cfg.finish(&pool).unwrap();
    }

    #[test]
    fn semantic_verifier_rejects_unguarded_read_after_conditional_explicit_drop() {
        assert_consumed_root(ConditionalDropShape {
            read_in_join: true,
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_exit_drop_guarded_by_a_flag_the_drop_did_not_clear() {
        assert_consumed_root(ConditionalDropShape {
            guard_slot: 2,
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_guarded_exit_drop_when_another_drop_skips_the_clearing() {
        assert_consumed_root(ConditionalDropShape {
            skip_arm_drops: true,
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_guarded_exit_drop_after_a_rearm_without_a_whole_write() {
        assert_consumed_root(ConditionalDropShape {
            rearm_in_join: Some(RearmChannel::StoreConst),
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_guarded_exit_drop_after_a_rearm_at_another_type() {
        assert_consumed_root(ConditionalDropShape {
            rearm_in_join: Some(RearmChannel::StoreOtherType),
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_guarded_exit_drop_after_an_alloc_rearm() {
        assert_consumed_root(ConditionalDropShape {
            rearm_in_join: Some(RearmChannel::Alloc),
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_guarded_exit_drop_after_a_place_write_rearm() {
        assert_consumed_root(ConditionalDropShape {
            rearm_in_join: Some(RearmChannel::PlaceWrite),
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_a_second_drop_guarded_by_the_stale_test_value() {
        assert_consumed_root(ConditionalDropShape {
            rebranch_on_stale_test: true,
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_guard_body_with_an_unguarded_entry() {
        assert_consumed_root(ConditionalDropShape {
            extra_guard_entry: true,
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_a_second_test_of_the_flag_after_the_guarded_drop() {
        assert_consumed_root(ConditionalDropShape {
            retest_after_exit_drop: true,
            ..Default::default()
        });
    }

    #[test]
    fn semantic_verifier_accepts_forwarded_guarded_drop_after_conditional_explicit_drop() {
        let (cfg, pool) = forwarded_conditional_drop_cfg(true);
        cfg.finish(&pool).unwrap();
    }

    #[test]
    fn semantic_verifier_rejects_forwarded_guarded_drop_without_the_clearing() {
        let (cfg, pool) = forwarded_conditional_drop_cfg(false);
        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("after it was already dropped on a reaching path"),
            "{error}"
        );
    }

    #[test]
    fn semantic_verifier_accepts_guarded_exit_drop_after_a_match_arm_drops_the_scrutinee() {
        let (cfg, pool) = match_scrutinee_drop_cfg(MatchDropShape::default());
        cfg.finish(&pool).unwrap();
    }

    #[test]
    fn semantic_verifier_accepts_forwarded_guarded_exit_drop_after_a_match_arm_drops_the_scrutinee()
    {
        let (cfg, pool) = match_scrutinee_drop_cfg(MatchDropShape {
            forwarded: true,
            ..Default::default()
        });
        cfg.finish(&pool).unwrap();
    }

    #[test]
    fn semantic_verifier_rejects_match_arm_drop_reached_by_a_path_that_rearms_the_flag() {
        let (cfg, pool) = match_scrutinee_drop_cfg(MatchDropShape {
            rearm_on_one_arm: true,
            ..Default::default()
        });
        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("reads already-consumed owner root"),
            "{error}"
        );
    }

    #[test]
    fn semantic_verifier_rejects_forwarded_match_arm_drop_reached_by_a_path_that_rearms_the_flag() {
        let (cfg, pool) = match_scrutinee_drop_cfg(MatchDropShape {
            forwarded: true,
            rearm_on_one_arm: true,
        });
        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("after it was already dropped on a reaching path"),
            "{error}"
        );
    }

    /// A conditional move out of a local (RUE-2367), as the builder lowers
    /// `let y = x.f` (or `let y = x`) in one arm of an `if`.
    #[derive(Default, Clone, Copy)]
    struct ConditionalMoveShape {
        /// The field of the pair moved out; `None` moves the whole pair.
        moved_field: Option<u32>,
        /// The field of the pair the exit drop reads; `None` reads the whole
        /// pair.
        dropped_field: Option<u32>,
        /// The moved flag guards the exit drop.
        guarded: bool,
        /// The arm writes the moved place again after the move.
        rewritten: bool,
        /// The arm writes only a scalar inside the moved field after the
        /// move, which leaves the rest of the field moved out.
        subfield_rewritten: bool,
        /// The moved value is a discarded temporary the arm drops at once,
        /// instead of the initializer of another local.
        temporary: bool,
    }

    fn conditional_move_cfg(shape: ConditionalMoveShape) -> (Cfg, FrozenTypeInternPool) {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "MovedOwner");
        let pair_id = register_struct(&pool, &interner, "MovedPair", &[owner, owner]);
        let pair = Type::new_struct(pair_id);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            4,
            1,
            "conditional_move".to_string(),
            vec![false],
        );
        let entry = cfg.new_block();
        let move_arm = cfg.new_block();
        let skip_arm = cfg.new_block();
        let join = cfg.new_block();
        let exit_drop = cfg.new_block();
        let exit = cfg.new_block();
        cfg.entry = entry;
        let place_of = |cfg: &mut Cfg, field: Option<u32>| match field {
            None => (Place::local(0, pair), pair),
            Some(field_index) => (
                cfg.make_place(
                    PlaceBase::Local(0),
                    pair,
                    [Projection::Field {
                        struct_id: pair_id,
                        field_index,
                    }],
                )
                .unwrap(),
                owner,
            ),
        };

        push(
            &mut cfg,
            entry,
            CfgInstData::StorageLive {
                slot: 0,
                local_ty: pair,
            },
            Type::UNIT,
        );
        let first = init_nonzero_owner(&mut cfg, entry, owner, 1);
        let second = init_nonzero_owner(&mut cfg, entry, owner, 2);
        let fields = cfg.push_struct_fields([first, second]).unwrap();
        let init = push(
            &mut cfg,
            entry,
            CfgInstData::StructInit {
                struct_id: pair_id,
                fields,
            },
            pair,
        );
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc { slot: 0, init },
            Type::UNIT,
        );
        let armed = push(&mut cfg, entry, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            entry,
            CfgInstData::Store {
                slot: 1,
                value: armed,
            },
            Type::UNIT,
        );
        let cond = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::BOOL);
        cfg.set_branch(entry, cond, move_arm, [], skip_arm, []);

        let (moved_place, moved_ty) = place_of(&mut cfg, shape.moved_field);
        let moved = match shape.moved_field {
            None => push(&mut cfg, move_arm, CfgInstData::Load { slot: 0 }, pair),
            Some(_) => push(
                &mut cfg,
                move_arm,
                CfgInstData::PlaceRead {
                    place: moved_place.duplicate_with_owner(),
                },
                moved_ty,
            ),
        };
        let cleared = push(&mut cfg, move_arm, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            move_arm,
            CfgInstData::Store {
                slot: 1,
                value: cleared,
            },
            Type::UNIT,
        );
        push(
            &mut cfg,
            move_arm,
            CfgInstData::MoveOut {
                place: moved_place.duplicate_with_owner(),
            },
            Type::UNIT,
        );
        if shape.temporary {
            push(
                &mut cfg,
                move_arm,
                CfgInstData::Drop { value: moved },
                Type::UNIT,
            );
        } else {
            push(
                &mut cfg,
                move_arm,
                CfgInstData::StorageLive {
                    slot: 2,
                    local_ty: moved_ty,
                },
                Type::UNIT,
            );
            push(
                &mut cfg,
                move_arm,
                CfgInstData::Alloc {
                    slot: 2,
                    init: moved,
                },
                Type::UNIT,
            );
            let binding = push(&mut cfg, move_arm, CfgInstData::Load { slot: 2 }, moved_ty);
            push(
                &mut cfg,
                move_arm,
                CfgInstData::Drop { value: binding },
                Type::UNIT,
            );
            push(
                &mut cfg,
                move_arm,
                CfgInstData::StorageDead {
                    slot: 2,
                    local_ty: moved_ty,
                },
                Type::UNIT,
            );
        }
        if shape.rewritten {
            let fresh = match shape.moved_field {
                None => {
                    let first = init_nonzero_owner(&mut cfg, move_arm, owner, 3);
                    let second = init_nonzero_owner(&mut cfg, move_arm, owner, 4);
                    let fields = cfg.push_struct_fields([first, second]).unwrap();
                    push(
                        &mut cfg,
                        move_arm,
                        CfgInstData::StructInit {
                            struct_id: pair_id,
                            fields,
                        },
                        pair,
                    )
                }
                Some(_) => init_nonzero_owner(&mut cfg, move_arm, owner, 3),
            };
            push(
                &mut cfg,
                move_arm,
                CfgInstData::PlaceWrite {
                    place: moved_place.duplicate_with_owner(),
                    value: fresh,
                },
                Type::UNIT,
            );
            let rearmed = push(&mut cfg, move_arm, CfgInstData::Const(1), Type::I32);
            push(
                &mut cfg,
                move_arm,
                CfgInstData::Store {
                    slot: 1,
                    value: rearmed,
                },
                Type::UNIT,
            );
        }
        if shape.subfield_rewritten {
            let field_index = shape
                .moved_field
                .expect("a subfield write needs a moved field");
            let TypeKind::Struct(owner_id) = owner.kind() else {
                unreachable!()
            };
            let subfield = cfg
                .make_place(
                    PlaceBase::Local(0),
                    pair,
                    [
                        Projection::Field {
                            struct_id: pair_id,
                            field_index,
                        },
                        Projection::Field {
                            struct_id: owner_id,
                            field_index: 0,
                        },
                    ],
                )
                .unwrap();
            let scalar = push(&mut cfg, move_arm, CfgInstData::Const(5), Type::I64);
            push(
                &mut cfg,
                move_arm,
                CfgInstData::PlaceWrite {
                    place: subfield,
                    value: scalar,
                },
                Type::UNIT,
            );
        }
        cfg.set_goto(move_arm, join, []);
        cfg.set_goto(skip_arm, join, []);

        if shape.guarded {
            let flag = push(&mut cfg, join, CfgInstData::Load { slot: 1 }, Type::I32);
            let zero = push(&mut cfg, join, CfgInstData::Const(0), Type::I32);
            let live = push(&mut cfg, join, CfgInstData::Ne(flag, zero), Type::BOOL);
            cfg.set_branch(join, live, exit_drop, [], exit, []);
        } else {
            cfg.set_goto(join, exit_drop, []);
        }
        let (dropped_place, dropped_ty) = place_of(&mut cfg, shape.dropped_field);
        let remaining = match shape.dropped_field {
            None => push(&mut cfg, exit_drop, CfgInstData::Load { slot: 0 }, pair),
            Some(_) => push(
                &mut cfg,
                exit_drop,
                CfgInstData::PlaceRead {
                    place: dropped_place,
                },
                dropped_ty,
            ),
        };
        push(
            &mut cfg,
            exit_drop,
            CfgInstData::Drop { value: remaining },
            Type::UNIT,
        );
        cfg.set_goto(exit_drop, exit, []);
        push(
            &mut cfg,
            exit,
            CfgInstData::StorageDead {
                slot: 0,
                local_ty: pair,
            },
            Type::UNIT,
        );
        cfg.set_terminator(exit, Terminator::Return { value: None });
        (cfg, pool)
    }

    fn assert_moved_out_drop_rejected(shape: ConditionalMoveShape) {
        let (cfg, pool) = conditional_move_cfg(shape);
        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error.to_string().contains("moved out on a reaching path"),
            "{error}"
        );
    }

    fn assert_move_accepted(shape: ConditionalMoveShape) {
        let (cfg, pool) = conditional_move_cfg(shape);
        if let Err(error) = cfg.finish(&pool) {
            panic!("{error}");
        }
    }

    #[test]
    fn semantic_verifier_rejects_unguarded_drop_after_conditional_move() {
        assert_moved_out_drop_rejected(ConditionalMoveShape::default());
    }

    #[test]
    fn semantic_verifier_accepts_flag_guarded_drop_after_conditional_move() {
        assert_move_accepted(ConditionalMoveShape {
            guarded: true,
            ..ConditionalMoveShape::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_unguarded_drop_after_a_write_inside_the_moved_place() {
        // Writing `x.a.payload` after moving `x.a` rewrites only part of it;
        // the rest of `x.a` is still moved out, so dropping `x.a` is not
        // owed.
        assert_moved_out_drop_rejected(ConditionalMoveShape {
            moved_field: Some(0),
            dropped_field: Some(0),
            subfield_rewritten: true,
            ..ConditionalMoveShape::default()
        });
    }

    #[test]
    fn semantic_verifier_accepts_unguarded_drop_after_move_and_rewrite() {
        assert_move_accepted(ConditionalMoveShape {
            rewritten: true,
            ..ConditionalMoveShape::default()
        });
        assert_move_accepted(ConditionalMoveShape {
            moved_field: Some(0),
            dropped_field: Some(0),
            rewritten: true,
            ..ConditionalMoveShape::default()
        });
    }

    #[test]
    fn semantic_verifier_accepts_the_new_owner_dropping_the_moved_value() {
        assert_move_accepted(ConditionalMoveShape {
            guarded: true,
            temporary: true,
            ..ConditionalMoveShape::default()
        });
    }

    #[test]
    fn semantic_verifier_rejects_unguarded_drop_overlapping_a_moved_field() {
        for dropped_field in [Some(0), None] {
            assert_moved_out_drop_rejected(ConditionalMoveShape {
                moved_field: Some(0),
                dropped_field,
                ..ConditionalMoveShape::default()
            });
        }
    }

    #[test]
    fn semantic_verifier_accepts_unguarded_drop_of_a_sibling_of_a_moved_field() {
        assert_move_accepted(ConditionalMoveShape {
            moved_field: Some(0),
            dropped_field: Some(1),
            ..ConditionalMoveShape::default()
        });
    }

    #[test]
    fn semantic_verifier_accepts_flag_guarded_drop_of_a_conditionally_moved_field() {
        assert_move_accepted(ConditionalMoveShape {
            moved_field: Some(0),
            dropped_field: Some(0),
            guarded: true,
            ..ConditionalMoveShape::default()
        });
    }

    /// `rounds` sequential `match`es of one flag-guarded binding (RUE-2347's
    /// review): each round whole-writes and arms the binding, moves it with
    /// the flag cleared before a branch, and drops the moved value in both
    /// arms; the scope exit is guarded by the flag.
    fn sequential_match_drops_cfg(rounds: usize) -> (Cfg, FrozenTypeInternPool) {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_nonzero_droppable_struct(&pool, &interner, "RematchedOwner");
        let pool = pool.freeze();
        let mut cfg = Cfg::new(
            Type::UNIT,
            2,
            1,
            "sequential_matches".to_string(),
            vec![false],
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
        let mut current = entry;
        for round in 0..rounds {
            let owned = init_nonzero_owner(&mut cfg, current, owner, round as i64);
            push(
                &mut cfg,
                current,
                CfgInstData::Store {
                    slot: 0,
                    value: owned,
                },
                Type::UNIT,
            );
            let armed = push(&mut cfg, current, CfgInstData::Const(1), Type::I32);
            push(
                &mut cfg,
                current,
                CfgInstData::Store {
                    slot: 1,
                    value: armed,
                },
                Type::UNIT,
            );
            let moved = push(&mut cfg, current, CfgInstData::Load { slot: 0 }, owner);
            let cleared = push(&mut cfg, current, CfgInstData::Const(0), Type::I32);
            push(
                &mut cfg,
                current,
                CfgInstData::Store {
                    slot: 1,
                    value: cleared,
                },
                Type::UNIT,
            );
            let which = push(
                &mut cfg,
                current,
                CfgInstData::Param { index: 0 },
                Type::BOOL,
            );
            let first_arm = cfg.new_block();
            let second_arm = cfg.new_block();
            let next = cfg.new_block();
            cfg.set_branch(current, which, first_arm, [], second_arm, []);
            for arm in [first_arm, second_arm] {
                push(
                    &mut cfg,
                    arm,
                    CfgInstData::Drop { value: moved },
                    Type::UNIT,
                );
                cfg.set_goto(arm, next, []);
            }
            current = next;
        }
        let exit_drop = cfg.new_block();
        let exit = cfg.new_block();
        let flag = push(&mut cfg, current, CfgInstData::Load { slot: 1 }, Type::I32);
        let zero = push(&mut cfg, current, CfgInstData::Const(0), Type::I32);
        let live = push(&mut cfg, current, CfgInstData::Ne(flag, zero), Type::BOOL);
        cfg.set_branch(current, live, exit_drop, [], exit, []);
        let remaining = push(&mut cfg, exit_drop, CfgInstData::Load { slot: 0 }, owner);
        push(
            &mut cfg,
            exit_drop,
            CfgInstData::Drop { value: remaining },
            Type::UNIT,
        );
        cfg.set_goto(exit_drop, exit, []);
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
        (cfg, pool)
    }

    #[test]
    fn semantic_verifier_solves_each_flag_clearing_once_across_facts() {
        const ROUNDS: usize = 32;
        let (cfg, pool) = sequential_match_drops_cfg(ROUNDS);
        super::SEMANTIC_WORK.with(|work| *work.borrow_mut() = Default::default());
        cfg.finish(&pool).unwrap();
        super::SEMANTIC_WORK.with(|work| {
            let work = *work.borrow();
            // One owner-root fact and one exact-value fact per moved value
            // all read the same flag; its every-path clearing is solved once.
            assert_eq!(work.flag_clearing_solves, 1, "{work:?}");
            // Each fact is checked without the drop-flag exemption first and
            // only a failing one builds the flag proof, so most facts are
            // solved once. Without that first check every fact would be
            // solved twice, about 2 * ROUNDS.
            assert!(work.fact_solves < 2 * ROUNDS, "{work:?}");
        });
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
            // The dropped phi's exact-value fact scans only the blocks its
            // definition reaches (the tail), not the whole chain: fewer
            // terminator operands than one chain walk would visit, and at
            // least the tail's four instructions.
            assert!(work.terminator_operand_visits < PHIS);
            assert!(work.validation_instruction_visits >= 4);
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
        cfg.verify_with_fixture_pool().unwrap(); // must not panic
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
        let pool = pool.freeze();
        cfg_with_field_place(Type::new_struct(struct_id), struct_id)
            .verify_with_type_pool(&pool)
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "but previous link produced Type::I32")]
    fn verify_rejects_field_projection_with_wrong_base_type() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let struct_id = register_struct(&pool, &interner, "WrongBase", &[Type::I32]);
        let pool = pool.freeze();
        cfg_with_field_place(Type::I32, struct_id)
            .verify_with_type_pool(&pool)
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "but previous link produced Type::I32")]
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
        let pool = TypeInternPool::new();
        let array_type = pool.try_intern_array(Type::I32, 1).unwrap();
        let pool = pool.freeze();
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

        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    #[should_panic(expected = "has non-array container type Type::I32")]
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

        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg_with_typed_edge(Type::I32)
            .verify_with_fixture_pool()
            .unwrap(); // must not panic
    }

    #[test]
    #[should_panic(expected = "ill-typed edge")]
    fn verify_catches_edge_type_mismatch() {
        // The RUE-347 shape: a unit value passed into an i32 block parameter.
        cfg_with_typed_edge(Type::UNIT)
            .verify_with_fixture_pool()
            .unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
    }

    #[test]
    #[should_panic(expected = "has no value but function return type")]
    fn verify_rejects_missing_nonunit_return_value() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(entry, Terminator::Return { value: None });
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
    }

    #[test]
    #[should_panic(expected = "entry block bb7 is out of bounds")]
    fn verify_rejects_invalid_entry_before_indexing() {
        let mut cfg = unit_cfg();
        cfg.new_block();
        cfg.entry = BlockId::from_raw(7);
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg.verify_with_fixture_pool().unwrap();
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
        cfg_with_unreachable_husk_edge()
            .verify_with_fixture_pool()
            .unwrap();
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

        let mut verifier = Verifier::new(&cfg, &pool, true);
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

    /// Build `@ptr_write(param0, operand)` where `param0` is a `*mut i64`,
    /// returning the graph and the pool that defines the pointer type. The
    /// caller chooses the written operand's type: `Type::I64` is the shape a
    /// well-formed pipeline produces, and anything else is the ill-typed
    /// substitution RUE-2086's store-to-load hazard fed to codegen.
    fn ptr_write_cfg(written_ty: Type) -> (Cfg, FrozenTypeInternPool) {
        let pool = TypeInternPool::new();
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I64));
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 0, 1, "ptr_write".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let pointer = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Param { index: 0 },
                ty: ptr_ty,
                span: Span::new(0, 0),
            },
        );
        let written = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: written_ty,
                span: Span::new(0, 0),
            },
        );
        let args = cfg.push_intrinsic_args([pointer, written]).unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Intrinsic {
                    operation: IntrinsicOperation::PtrWrite,
                    name: ThreadedRodeo::default().get_or_intern("ptr_write"),
                    args,
                },
                ty: Type::UNIT,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });
        (cfg, pool)
    }

    /// The channel RUE-2086 flowed through: codegen reads an intrinsic
    /// operand's marshalled width off the operand's own type, so a pass that
    /// substitutes a zero-sized value for a `*mut i64`'s pointee silently
    /// drops the store instead of miscompiling it later (RUE-2094).
    #[test]
    #[should_panic(
        expected = "intrinsic PtrWrite instruction v2 in block bb0 no longer satisfies its call signature"
    )]
    fn verify_rejects_intrinsic_operand_of_the_wrong_type() {
        let (cfg, pool) = ptr_write_cfg(Type::UNIT);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    /// The report names the offending operand, its type, and its origin, so a
    /// compiler developer reads the substitution straight out of the message.
    #[test]
    fn intrinsic_operand_report_names_the_operand_and_its_type() {
        let (cfg, pool) = ptr_write_cfg(Type::UNIT);
        let message = cfg.verify_with_type_pool(&pool).unwrap_err().to_string();
        assert!(
            message.contains("operand 1 (v1) has type Type::UNIT"),
            "{message}"
        );
        assert!(message.contains("result type is Type::UNIT"), "{message}");
    }

    /// The matching well-typed graph is accepted: the check re-proves the
    /// construction-time contract, it does not tighten it.
    #[test]
    fn verify_accepts_a_well_typed_intrinsic_operand() {
        let (cfg, pool) = ptr_write_cfg(Type::I64);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    fn callback_type(pool: &TypeInternPool) -> Type {
        pool.try_intern_function(rue_air::FunctionTypeDef {
            params: vec![rue_air::FunctionTypeParam {
                mode: rue_air::FunctionParamMode::Value,
                ty: Type::I64,
            }],
            result: Type::I64,
        })
        .unwrap()
    }

    /// A callback bound by `fn_addr` and called by `call_indirect` (ADR-0096)
    /// verifies exactly as a direct call does: the callee value must have a
    /// `fn` type and the established contract must hold.
    #[test]
    fn verify_accepts_an_indirect_call_through_a_fn_typed_value() {
        let pool = TypeInternPool::new();
        let callback = callback_type(&pool);
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::I64, 0, 0, "indirect".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::FnAddr {
                    name: ThreadedRodeo::default().get_or_intern("callee"),
                },
                ty: callback,
                span: Span::new(0, 0),
            },
        );
        let argument = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(7),
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        let args = cfg
            .push_call_args([CfgCallArg {
                value: argument,
                mode: CfgArgMode::Normal,
            }])
            .unwrap();
        let call = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::CallIndirect {
                    callee: target,
                    args,
                },
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        cfg.set_call_contract(
            call,
            CfgCallContract::new(
                [CfgCallContractArg {
                    ty: Type::I64,
                    mode: CfgArgMode::Normal,
                }],
                Type::I64,
            ),
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(call) });
        cfg.verify_with_type_pool(&pool).unwrap();

        let rendered = cfg.to_string();
        assert!(rendered.contains("fn_addr @"), "{rendered}");
        assert!(rendered.contains("call_indirect v0(v1)"), "{rendered}");
    }

    #[test]
    fn verify_rejects_a_fn_addr_without_a_fn_type() {
        let pool = TypeInternPool::new().freeze();
        let mut cfg = Cfg::new(Type::I64, 0, 0, "indirect".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::FnAddr {
                    name: ThreadedRodeo::default().get_or_intern("callee"),
                },
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(target),
            },
        );
        let message = cfg.verify_with_type_pool(&pool).unwrap_err().to_string();
        assert!(message.contains("must have a `fn` type"), "{message}");
    }

    #[test]
    fn verify_rejects_an_indirect_call_through_a_non_fn_value() {
        let pool = TypeInternPool::new().freeze();
        let mut cfg = Cfg::new(Type::I64, 0, 0, "indirect".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        let args = cfg.push_call_args([]).unwrap();
        let call = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::CallIndirect {
                    callee: target,
                    args,
                },
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        cfg.set_call_contract(call, CfgCallContract::new([], Type::I64));
        cfg.set_terminator(entry, Terminator::Return { value: Some(call) });
        let message = cfg.verify_with_type_pool(&pool).unwrap_err().to_string();
        assert!(message.contains("which is not a `fn` type"), "{message}");
    }

    fn ordinary_call_cfg(
        argument_ty: Type,
        argument_mode: CfgArgMode,
    ) -> (Cfg, FrozenTypeInternPool, CfgValue, CfgValue) {
        let pool = TypeInternPool::new().freeze();
        let mut cfg = Cfg::new(Type::I64, 0, 0, "ordinary_call".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let argument = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(7),
                ty: argument_ty,
                span: Span::new(0, 0),
            },
        );
        let args = cfg
            .push_call_args([CfgCallArg {
                value: argument,
                mode: argument_mode,
            }])
            .unwrap();
        let call = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Call {
                    runtime: None,
                    name: ThreadedRodeo::default().get_or_intern("callee"),
                    args,
                },
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        cfg.set_call_contract(
            call,
            crate::inst::CfgCallContract::new(
                [crate::inst::CfgCallContractArg {
                    ty: Type::I64,
                    mode: CfgArgMode::Normal,
                }],
                Type::I64,
            ),
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(call) });
        (cfg, pool, argument, call)
    }

    #[test]
    #[should_panic(expected = "argument 0 no longer satisfies its established contract")]
    fn verify_rejects_ordinary_call_operand_substitution() {
        let (cfg, pool, _, _) = ordinary_call_cfg(Type::UNIT, CfgArgMode::Normal);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    #[should_panic(expected = "argument 0 no longer satisfies its established contract")]
    fn verify_rejects_ordinary_call_mode_substitution() {
        let (cfg, pool, _, _) = ordinary_call_cfg(Type::I64, CfgArgMode::Borrow);
        cfg.verify_with_type_pool(&pool).unwrap();
    }

    #[test]
    fn verify_rejects_ordinary_call_without_an_established_contract() {
        let pool = TypeInternPool::new().freeze();
        let interner = ThreadedRodeo::new();
        let mut cfg = Cfg::new(Type::I64, 0, 0, "missing_contract".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let args = cfg.push_call_args([]).unwrap();
        let call = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Call {
                    runtime: None,
                    name: interner.get_or_intern("callee"),
                    args,
                },
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(call) });

        let error = cfg.verify_with_type_pool(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("has no established AIR call contract")
        );
    }

    #[test]
    fn verify_rejects_ordinary_call_argument_count_substitution() {
        let pool = TypeInternPool::new().freeze();
        let interner = ThreadedRodeo::new();
        let mut cfg = Cfg::new(Type::I64, 0, 0, "call_arity".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let args = cfg.push_call_args([]).unwrap();
        let call = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Call {
                    runtime: None,
                    name: interner.get_or_intern("callee"),
                    args,
                },
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        cfg.set_call_contract(
            call,
            CfgCallContract::new(
                [CfgCallContractArg {
                    ty: Type::I64,
                    mode: CfgArgMode::Normal,
                }],
                Type::I64,
            ),
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(call) });

        let error = cfg.verify_with_type_pool(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("established argument count 1; found 0")
        );
    }

    #[test]
    fn verify_rejects_ordinary_call_result_substitution() {
        let (mut cfg, pool, _, call) = ordinary_call_cfg(Type::I64, CfgArgMode::Normal);
        cfg.replace_inst_type(call, Type::BOOL).unwrap();

        let error = cfg.verify_with_type_pool(&pool).unwrap_err();
        assert!(error.to_string().contains("established result type"));
    }

    #[test]
    fn verify_rechecks_accessor_call_against_its_established_contract() {
        let pool = TypeInternPool::new().freeze();
        let interner = ThreadedRodeo::new();
        let mut cfg = Cfg::new(Type::I64, 0, 0, "accessor_contract".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let argument = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(7),
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        let args = cfg
            .push_call_args([CfgCallArg {
                value: argument,
                mode: CfgArgMode::Borrow,
            }])
            .unwrap();
        let call = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::AccessorCall {
                    name: interner.get_or_intern("accessor"),
                    args,
                },
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        cfg.set_call_contract(
            call,
            CfgCallContract::new(
                [CfgCallContractArg {
                    ty: Type::I64,
                    mode: CfgArgMode::Normal,
                }],
                Type::I64,
            ),
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(call) });

        let error = cfg.verify_with_type_pool(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("argument 0 no longer satisfies its established contract")
        );
    }

    /// `@raw_mut` lowers by taking its operand's ADDRESS, so the operand has
    /// to still be a place read after optimization. A constant-folded operand
    /// is the RUE-521 shape, and it is a type-system violation the shared AIR
    /// validator already knows how to reject.
    #[test]
    #[should_panic(expected = "from a computed value")]
    fn verify_rejects_address_taking_intrinsic_operand_folded_to_a_constant() {
        let pool = TypeInternPool::new();
        let ptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I64));
        let pool = pool.freeze();
        let mut cfg = Cfg::new(Type::UNIT, 1, 0, "raw_mut".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let folded = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(7),
                ty: Type::I64,
                span: Span::new(0, 0),
            },
        );
        let args = cfg.push_intrinsic_args([folded]).unwrap();
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Intrinsic {
                    operation: IntrinsicOperation::RawMut,
                    name: ThreadedRodeo::default().get_or_intern("raw_mut"),
                    args,
                },
                ty: ptr_ty,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });
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
                    shape: rue_air::ArrayInitShape::Elementwise,
                },
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Unreachable);
        let error = cfg.verify_with_fixture_pool().unwrap_err();
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
        let error = cfg.verify_with_fixture_pool().unwrap_err();
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
        let error = cfg.verify_with_fixture_pool().unwrap_err();
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
        let error = cfg.verify_with_fixture_pool().unwrap_err();
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
        let pool = TypeInternPool::new();
        let array_type = pool.try_intern_array(Type::I32, 1).unwrap();
        let pool = pool.freeze();
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
        cfg.verify_with_type_pool(&pool).unwrap();
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

    // The exact-value drop fact starts at the dropped value's defining block
    // (RUE-2356). These pin the loop and phi shapes where that block is not
    // the entry: a value defined in a loop header, one defined before the
    // loop outside the entry block, and a phi carried around a back edge.
    fn exact_drop_owner(name: &str) -> (Type, StructId, FrozenTypeInternPool) {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let owner = register_droppable_struct(&pool, &interner, name);
        let id = match owner.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };
        (owner, id, pool.freeze())
    }

    fn exact_drop_init(cfg: &mut Cfg, block: BlockId, owner: Type, id: StructId) -> CfgValue {
        let fields = cfg.push_struct_fields([]).unwrap();
        push(
            cfg,
            block,
            CfgInstData::StructInit {
                struct_id: id,
                fields,
            },
            owner,
        )
    }

    /// Value defined in a loop header, dropped in the body, then dropped again
    /// on a body exit that leaves the loop.
    #[test]
    fn exact_drop_fact_rejects_loop_header_value_dropped_in_body_and_on_exit() {
        let (owner, id, pool) = exact_drop_owner("B1Owner");
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "b1".to_string(), vec![]);
        let entry = cfg.new_block();
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();
        let exit2 = cfg.new_block();
        cfg.entry = entry;
        cfg.set_goto(entry, header, []);
        let v = exact_drop_init(&mut cfg, header, owner, id);
        let c = push(&mut cfg, header, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(header, c, body, [], exit, []);
        push(&mut cfg, body, CfgInstData::Drop { value: v }, Type::UNIT);
        let c2 = push(&mut cfg, body, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(body, c2, header, [], exit2, []);
        push(&mut cfg, exit, CfgInstData::Drop { value: v }, Type::UNIT);
        cfg.set_terminator(exit, Terminator::Return { value: None });
        push(&mut cfg, exit2, CfgInstData::Drop { value: v }, Type::UNIT);
        cfg.set_terminator(exit2, Terminator::Return { value: None });
        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already dropped on a reaching path"),
            "{error}"
        );
    }

    /// Value defined before a loop in a non-entry block, dropped inside the
    /// loop body: the second iteration drops it again.
    #[test]
    fn exact_drop_fact_rejects_pre_loop_value_dropped_every_iteration() {
        let (owner, id, pool) = exact_drop_owner("B2Owner");
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "b2".to_string(), vec![]);
        let entry = cfg.new_block();
        let pre = cfg.new_block();
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();
        cfg.entry = entry;
        cfg.set_goto(entry, pre, []);
        let v = exact_drop_init(&mut cfg, pre, owner, id);
        cfg.set_goto(pre, header, []);
        let c = push(&mut cfg, header, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(header, c, body, [], exit, []);
        push(&mut cfg, body, CfgInstData::Drop { value: v }, Type::UNIT);
        cfg.set_goto(body, header, []);
        cfg.set_terminator(exit, Terminator::Return { value: None });
        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already dropped on a reaching path"),
            "{error}"
        );
    }

    /// A phi dropped in its join, then passed back to the same join on the
    /// back edge.
    #[test]
    fn exact_drop_fact_rejects_phi_passed_back_after_its_drop() {
        let (owner, id, pool) = exact_drop_owner("B4Owner");
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "b4".to_string(), vec![]);
        let entry = cfg.new_block();
        let join = cfg.new_block();
        let exit = cfg.new_block();
        let p = cfg.add_block_param(join, owner);
        cfg.entry = entry;
        let v = exact_drop_init(&mut cfg, entry, owner, id);
        cfg.set_goto(entry, join, [v]);
        push(&mut cfg, join, CfgInstData::Drop { value: p }, Type::UNIT);
        let c = push(&mut cfg, join, CfgInstData::BoolConst(false), Type::BOOL);
        cfg.set_branch(join, c, join, [p], exit, []);
        cfg.set_terminator(exit, Terminator::Return { value: None });
        let error = cfg.finish(&pool).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already dropped on a reaching path"),
            "{error}"
        );
    }
}
