//! CFG optimization passes.
//!
//! This module provides optimization passes that transform CFG -> CFG,
//! improving code quality without changing program semantics.
//!
//! ## Optimization Levels
//!
//! Rue follows standard compiler conventions for optimization levels:
//!
//! - `-O0`: No optimization (default)
//! - `-O1`: Basic optimizations (constant folding, peephole, CFG
//!   simplification, dead code elimination)
//! - `-O2`: `-O1` plus value forwarding / copy propagation (RUE-914) and
//!   dominator-scoped common-subexpression elimination (RUE-913, RUE-1874)
//! - `-O3`: `-O2` plus canonical loop-preheader normalization, loop-invariant
//!   code motion (RUE-927), which hoists trap-free invariant computations out
//!   of loops into their preheaders, and bounded constant-trip full unrolling
//!   (RUE-928)
//!
//! ## Pipeline
//!
//! Optimizations run after CFG construction and before lowering to MIR:
//!
//! ```text
//! AIR -> CfgBuilder -> CFG -> [optimize] -> CfgLower -> MIR
//! ```

mod classify;
mod constfold;
mod constopt;
mod cse;
mod dce;
mod forward;
mod licm;
mod loops;
mod peephole;
mod simplify;
mod slot_facts;
mod unroll;

use crate::{CfgEditError, CfgVerificationError, ValidatedCfg};
use rue_air::FrozenTypeInternPool;

/// Optimization level, following standard compiler conventions.
///
/// Controls which optimization passes are run during compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    /// No optimization (`-O0`).
    ///
    /// Produces unoptimized code that closely matches the source structure.
    /// Useful for debugging and faster compilation.
    #[default]
    O0,

    /// Basic optimizations (`-O1`).
    ///
    /// Enables fundamental optimizations:
    /// - Constant folding
    /// - Dead code elimination
    O1,

    /// Standard optimizations (`-O2`).
    ///
    /// Superset of `-O1`: runs everything `-O1` does, then value forwarding /
    /// copy propagation (RUE-914), which replaces redundant `Load`s with the
    /// SSA value already holding the slot's contents, followed by
    /// dominator-scoped common-subexpression elimination (RUE-913, RUE-1874),
    /// which replaces duplicate pure computations with a dominating occurrence.
    O2,

    /// Aggressive optimizations (`-O3`).
    ///
    /// Superset of `-O2`: adds loop-invariant code motion (RUE-927), bounded
    /// constant-trip full unrolling (RUE-928), and their mandatory cleanup.
    /// Trapping invariant ops are never moved (ADR-0054 §2).
    O3,
}

impl OptLevel {
    /// Returns the name of this optimization level (e.g., "O0", "O1").
    pub fn name(&self) -> &'static str {
        match self {
            OptLevel::O0 => "O0",
            OptLevel::O1 => "O1",
            OptLevel::O2 => "O2",
            OptLevel::O3 => "O3",
        }
    }

    /// Returns all available optimization levels.
    pub fn all() -> &'static [OptLevel] {
        &[OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::O3]
    }

    /// Returns a comma-separated string of all level names (for help text).
    pub fn all_names() -> &'static str {
        "-O0, -O1, -O2, -O3"
    }
}

/// Error returned when parsing an optimization level fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOptLevelError(String);

impl std::fmt::Display for ParseOptLevelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown optimization level '{}'", self.0)
    }
}

impl std::error::Error for ParseOptLevelError {}

/// Recoverable failure while optimizing a validated CFG.
#[derive(Debug)]
pub enum CfgOptimizationError {
    /// An optimizer payload edit could not be represented or allocated.
    Edit(CfgEditError),
    /// The optimized graph failed the publication-time verification boundary.
    Verification(CfgVerificationError),
}

/// Bounded optimizer work published alongside the optimized CFG.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OptimizationStats {
    pub constopt_fold_attempts: u64,
    pub constopt_folded: u64,
    pub constopt_loads_rewritten: u64,
    pub peephole_divmods_reduced: u64,
    pub peephole_identities_rewired: u64,
    pub simplify_blocks_scanned: u64,
    pub simplify_branches_folded: u64,
    pub simplify_switches_folded: u64,
    pub simplify_edges_threaded: u64,
    pub simplify_forwarders_resolved: u64,
    pub simplify_blocks_merged: u64,
    pub forward_insts_scanned: u64,
    pub forward_loads_single_write: u64,
    pub forward_loads_block_local: u64,
    pub forward_rule1_dominance_pairs_checked: u64,
    pub forward_dominator_computations: u64,
    pub cse_insts_scanned: u64,
    pub cse_duplicates_replaced: u64,
    pub cse_max_table_entries: u64,
    pub cse_dominator_computations: u64,
    pub preheader_normalization_forest_computations: u64,
    pub preheader_normalization_loops_examined: u64,
    pub preheader_normalization_preheaders_materialized: u64,
    pub preheader_normalization_verifier_dominator_computations: u64,
    pub licm_forest_computations: u64,
    pub licm_def_block_scans: u64,
    pub licm_loops_analyzed: u64,
    pub licm_instructions_examined: u64,
    pub licm_slot_fact_instructions_scanned: u64,
    pub licm_slot_fact_entries_initialized: u64,
    pub licm_slot_fact_workspace_growths: u64,
    pub licm_candidate_dependencies: u64,
    pub licm_worklist_pops: u64,
    pub licm_invariants_hoisted: u64,
    pub licm_hoist_workspace_growths: u64,
    pub unroll_forest_computations: u64,
    pub loops_analyzed: u64,
    pub loops_unrolled: u64,
    pub budget_refusals: u64,
    pub unroll_shape_refusals: u64,
    pub unroll_blocks_cloned: u64,
    pub unroll_values_cloned: u64,
    pub unroll_instructions_cloned: u64,
    /// Dominator computations performed by the final publication verifier.
    /// A successfully returned optimizer result contributes exactly one.
    pub publication_verifier_dominator_computations: u64,
    /// CFG values cloned by O3 growth transforms in this invocation.
    pub code_growth_used: u64,
    /// CFG blocks cloned by O3 growth transforms in this invocation.
    pub code_growth_blocks_used: u64,
}

impl OptimizationStats {
    fn add_constopt(&mut self, pass: constopt::Stats) {
        self.constopt_fold_attempts += pass.fold_attempts;
        self.constopt_folded += pass.folded;
        self.constopt_loads_rewritten += pass.loads_rewritten;
    }

    fn add_peephole(&mut self, pass: peephole::Stats) {
        self.peephole_divmods_reduced += pass.divmods_reduced;
        self.peephole_identities_rewired += pass.identities_rewired;
    }

    fn add_simplify(&mut self, pass: simplify::Stats) {
        self.simplify_blocks_scanned += pass.blocks_scanned;
        self.simplify_branches_folded += pass.branches_folded;
        self.simplify_switches_folded += pass.switches_folded;
        self.simplify_edges_threaded += pass.edges_threaded;
        self.simplify_forwarders_resolved += pass.forwarders_resolved;
        self.simplify_blocks_merged += pass.blocks_merged;
    }

    fn add_forward(&mut self, pass: forward::Stats) {
        self.forward_insts_scanned += pass.insts_scanned;
        self.forward_loads_single_write += pass.loads_forwarded_single_write;
        self.forward_loads_block_local += pass.loads_forwarded_block_local;
        self.forward_rule1_dominance_pairs_checked += pass.rule1_dominance_pairs_checked;
        self.forward_dominator_computations += pass.dominator_computations;
    }

    fn add_cse(&mut self, pass: cse::Stats) {
        self.cse_insts_scanned += pass.insts_scanned;
        self.cse_duplicates_replaced += pass.duplicates_replaced;
        self.cse_max_table_entries = self.cse_max_table_entries.max(pass.max_table_entries);
        self.cse_dominator_computations += pass.dominator_computations;
    }

    fn add_preheader_normalization(&mut self, pass: loops::PreheaderNormalizationStats) {
        self.preheader_normalization_forest_computations += pass.forest_computations;
        self.preheader_normalization_loops_examined += pass.loops_examined;
        self.preheader_normalization_preheaders_materialized += pass.preheaders_materialized;
        self.preheader_normalization_verifier_dominator_computations +=
            pass.verifier_dominator_computations;
    }

    fn add_licm(&mut self, pass: licm::Stats) {
        self.licm_forest_computations += pass.forest_computations;
        self.licm_def_block_scans += pass.def_block_scans;
        self.licm_loops_analyzed += pass.loops_analyzed;
        self.licm_instructions_examined += pass.instructions_examined;
        self.licm_slot_fact_instructions_scanned += pass.slot_fact_instructions_scanned;
        self.licm_slot_fact_entries_initialized += pass.slot_fact_entries_initialized;
        self.licm_slot_fact_workspace_growths += pass.slot_fact_workspace_growths;
        self.licm_candidate_dependencies += pass.candidate_dependencies;
        self.licm_worklist_pops += pass.worklist_pops;
        self.licm_invariants_hoisted += pass.invariants_hoisted;
        self.licm_hoist_workspace_growths += pass.hoist_workspace_growths;
    }

    fn add_unroll(&mut self, pass: unroll::Stats) {
        self.unroll_forest_computations += pass.forest_computations;
        self.loops_analyzed += pass.loops_analyzed;
        self.loops_unrolled += pass.loops_unrolled;
        self.budget_refusals += pass.budget_refusals;
        self.unroll_shape_refusals += pass.shape_refusals;
        self.unroll_blocks_cloned += pass.blocks_cloned;
        self.unroll_values_cloned += pass.values_cloned;
        self.unroll_instructions_cloned += pass.instructions_cloned;
    }
}

/// Exact growth attributed to one bounded transform operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeGrowth {
    pub values: u64,
    pub blocks: u64,
}

/// The per-function budget shared by O3 inlining and constant-trip unrolling.
///
/// The budget counts values and basic blocks allocated by growth transforms.
/// Keeping the consumed amounts in this small value object lets the
/// whole-program batch carry unrolling's charge into the later inlining phase
/// and lets the re-optimization after a splice spend only what remains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeGrowthBudget {
    max_total_values: u64,
    max_total_blocks: u64,
    used_values: u64,
    used_blocks: u64,
}

impl CodeGrowthBudget {
    pub const O3_MAX_TOTAL_VALUES: u64 = 256;
    /// The measured Lattice population has at most 68 blocks per CFG; 256 is
    /// a conservative bounded-work ceiling that preserves ordinary fixed-loop
    /// unrolling while bounding block-heavy growth.
    pub const O3_MAX_TOTAL_BLOCKS: u64 = 256;

    pub const fn o3() -> Self {
        Self {
            max_total_values: Self::O3_MAX_TOTAL_VALUES,
            max_total_blocks: Self::O3_MAX_TOTAL_BLOCKS,
            used_values: 0,
            used_blocks: 0,
        }
    }

    pub const fn with_used(used_values: u64, used_blocks: u64) -> Self {
        Self {
            max_total_values: Self::O3_MAX_TOTAL_VALUES,
            max_total_blocks: Self::O3_MAX_TOTAL_BLOCKS,
            used_values,
            used_blocks,
        }
    }

    pub const fn used(self) -> u64 {
        self.used_values
    }

    pub const fn used_values(self) -> u64 {
        self.used_values
    }

    pub const fn used_blocks(self) -> u64 {
        self.used_blocks
    }

    pub const fn remaining(self) -> u64 {
        self.max_total_values.saturating_sub(self.used_values)
    }

    pub const fn remaining_blocks(self) -> u64 {
        self.max_total_blocks.saturating_sub(self.used_blocks)
    }

    /// Convert an exact splice/unroll delta into a bounded policy charge.
    /// Ordinary never-returning bodies can have no value delta while still
    /// copying blocks and terminators, so every accepted zero-value site
    /// consumes at least one value unit. Accessor calls are not general
    /// inlining candidates.
    pub const fn charge_for_growth(growth: CodeGrowth) -> CodeGrowth {
        CodeGrowth {
            values: if growth.values == 0 { 1 } else { growth.values },
            blocks: growth.blocks,
        }
    }

    pub const fn can_charge(self, growth: CodeGrowth) -> bool {
        let Some(new_values) = self.used_values.checked_add(growth.values) else {
            return false;
        };
        let Some(new_blocks) = self.used_blocks.checked_add(growth.blocks) else {
            return false;
        };
        new_values <= self.max_total_values && new_blocks <= self.max_total_blocks
    }

    pub fn try_charge(&mut self, growth: CodeGrowth) -> bool {
        if !self.can_charge(growth) {
            return false;
        }
        self.used_values += growth.values;
        self.used_blocks += growth.blocks;
        true
    }
}

impl std::fmt::Display for CfgOptimizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Edit(error) => write!(f, "CFG optimizer edit was rejected: {error}"),
            Self::Verification(error) => error.fmt(f),
        }
    }
}

impl CfgOptimizationError {
    /// Classify an optimization failure for the user.
    ///
    /// An optimizer payload edit that outgrew the compact `u32` payload
    /// representation is an implementation-limit rejection (`E1401`) naming the
    /// limit, per spec C.1:2; every other optimization failure is a compiler
    /// bug and stays an internal error.
    pub fn error_kind(&self, context: &str) -> rue_error::ErrorKind {
        match self {
            Self::Edit(error) => error.error_kind(context),
            Self::Verification(_) => {
                rue_error::ErrorKind::InternalError(format!("{context}: {self}"))
            }
        }
    }
}

impl std::error::Error for CfgOptimizationError {}

impl From<CfgEditError> for CfgOptimizationError {
    fn from(error: CfgEditError) -> Self {
        Self::Edit(error)
    }
}

impl From<CfgVerificationError> for CfgOptimizationError {
    fn from(error: CfgVerificationError) -> Self {
        Self::Verification(error)
    }
}

impl std::str::FromStr for OptLevel {
    type Err = ParseOptLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "O0" | "0" => Ok(OptLevel::O0),
            "O1" | "1" => Ok(OptLevel::O1),
            "O2" | "2" => Ok(OptLevel::O2),
            "O3" | "3" => Ok(OptLevel::O3),
            _ => Err(ParseOptLevelError(s.to_string())),
        }
    }
}

impl std::fmt::Display for OptLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "-{}", self.name())
    }
}

/// Run optimization passes on a CFG at the given level.
///
/// This is the main entry point for CFG optimization. It runs the
/// appropriate passes based on the optimization level.
///
/// # Arguments
///
/// * `cfg` - The control flow graph to optimize (modified in place)
/// * `level` - The optimization level to use
///
/// # Example
///
/// ```ignore
/// let mut cfg = CfgBuilder::build(...);
/// optimize(&mut cfg, OptLevel::O1, &type_pool);
/// // cfg is now optimized
/// ```
pub fn optimize(
    cfg: ValidatedCfg,
    level: OptLevel,
    type_pool: &FrozenTypeInternPool,
) -> Result<ValidatedCfg, CfgOptimizationError> {
    optimize_with_stats(cfg, level, type_pool).map(|(cfg, _)| cfg)
}

pub fn optimize_with_stats(
    cfg: ValidatedCfg,
    level: OptLevel,
    type_pool: &FrozenTypeInternPool,
) -> Result<(ValidatedCfg, OptimizationStats), CfgOptimizationError> {
    let (cfg, stats, _) = optimize_with_budget(cfg, level, type_pool, CodeGrowthBudget::o3())?;
    Ok((cfg, stats))
}

/// Optimize with an existing O3 growth budget. The returned budget includes
/// all growth accepted by this invocation and is used by the batch driver to
/// coordinate later interprocedural inlining with earlier unrolling.
pub fn optimize_with_budget(
    cfg: ValidatedCfg,
    level: OptLevel,
    type_pool: &FrozenTypeInternPool,
    mut budget: CodeGrowthBudget,
) -> Result<(ValidatedCfg, OptimizationStats, CodeGrowthBudget), CfgOptimizationError> {
    let initial_values = budget.used_values();
    let initial_blocks = budget.used_blocks();
    let mut cfg = cfg.into_editor();
    let mut stats = OptimizationStats::default();

    // Every pass below works on this private editor. If an in-place payload
    // rewrite poisons it, publish_optimization returns the pass error before
    // the editor can be published, so no caller observes partial state.
    let pass_result = (|| {
        match level {
            OptLevel::O0 => {
                // No optimization
            }
            OptLevel::O1 | OptLevel::O2 | OptLevel::O3 => {
                // Constant folding interleaved with store-to-load constant
                // propagation: folding a let's initializer can expose a constant
                // store, and propagating it into Loads can expose new foldable
                // operations (chains of single-assignment lets, RUE-154). The
                // sparse worklist driver reaches that fixpoint by revisiting an
                // instruction only when one of its inputs becomes constant, so
                // deep chains stay linear instead of forcing quadratic full-CFG
                // rescans (RUE-794).
                let constopt_stats = constopt::run(&mut cfg);
                stats.add_constopt(constopt_stats);

                // Peephole algebraic simplification (RUE-912): rewire trap-free
                // identities (x+0, x*1, ...) to their operand and strength-reduce
                // unsigned division/modulo by powers of two. Runs after constopt
                // so propagated constants are visible; annihilators that produce
                // constants (x*0, x-x, ...) live in the constfold kernel inside
                // the worklist instead, because their results cascade.
                let peephole_stats = peephole::run(&mut cfg)?;
                stats.add_peephole(peephole_stats);

                // CFG simplification (RUE-910, RUE-911): fold constant-condition
                // Branch/Switch terminators into Gotos so dead arms drop out of
                // reachability, then thread empty forwarding blocks and merge
                // single-predecessor Goto chains into straight-line blocks
                // before DCE prunes the leftovers.
                let simplify_stats = simplify::run(&mut cfg)?;
                stats.add_simplify(simplify_stats);
                // Folding a control value can expose stores and loads on the
                // surviving path (notably drop flags). Re-run the sparse
                // constant/folding cleanup when control flow changed so those
                // newly unreachable ownership actions are removed before
                // forwarding and CSE inspect the graph.
                if simplify_stats.branches_folded > 0 || simplify_stats.switches_folded > 0 {
                    dce::run(&mut cfg);
                    let constopt_stats = constopt::run(&mut cfg);
                    stats.add_constopt(constopt_stats);
                    let peephole_stats = peephole::run(&mut cfg)?;
                    stats.add_peephole(peephole_stats);
                    let simplify_stats = simplify::run(&mut cfg)?;
                    stats.add_simplify(simplify_stats);
                }

                // Value forwarding / copy propagation (RUE-914), at -O2/-O3 only.
                // Runs after simplify and before CSE: it turns redundant `Load`s
                // into the SSA value already holding the slot's contents (a global
                // single-write rule plus a block-local last-store rule), which is
                // exactly what lets CSE key expressions built over those loads.
                // Both rules are trap-exact — a load never traps and the forwarded
                // value is already computed — so the orphaned loads fall to DCE.
                if matches!(level, OptLevel::O2 | OptLevel::O3) {
                    let forward_stats = forward::run(&mut cfg)?;
                    stats.add_forward(forward_stats);
                    // Forwarding can expose constants in a branch condition
                    // without changing the Load instruction itself. Fold and
                    // simplify that newly constant control flow before CSE;
                    // this is essential for drop-flag stores revealed after a
                    // selector branch is folded.
                    if forward_stats.loads_forwarded_single_write > 0
                        || forward_stats.loads_forwarded_block_local > 0
                    {
                        let constopt_stats = constopt::run(&mut cfg);
                        stats.add_constopt(constopt_stats);
                        let simplify_stats = simplify::run(&mut cfg)?;
                        stats.add_simplify(simplify_stats);
                    }
                }

                // Dominator-scoped common-subexpression elimination (RUE-913,
                // RUE-1874), at -O2/-O3 only (ADR-0044 places CSE at the
                // release-default level). Runs after forwarding — expressions over
                // forwarded loads are now keyable — and before DCE, which sweeps the
                // dead placeholders each replaced duplicate (and each forwarded
                // load) leaves behind.
                if matches!(level, OptLevel::O2 | OptLevel::O3) {
                    let cse_stats = cse::run(&mut cfg)?;
                    stats.add_cse(cse_stats);
                }

                // Canonical loop normalization and LICM (RUE-927), at -O3
                // only — the first work gated strictly above -O2 (ADR-0054
                // Phases 1-2). Normalization establishes a preheader for every
                // natural loop to a fixed point before either loop transform.
                // LICM then computes one current forest and changes no CFG
                // edges. Runs after
                // the whole -O1/-O2 sequence so the invariant operands it keys
                // on are as exposed as constant folding, simplification,
                // forwarding, and CSE can make them, and before DCE, which
                // sweeps anything the moves orphan. It hoists ONLY trap-free
                // (`is_speculatable`) invariant ops into each loop's preheader;
                // trapping invariant ops never move, because hoisting one into
                // a zero-trip preheader would manufacture a trap the source
                // never runs (the inverse of RUE-57). It recomputes dominators
                // + loops per the ADR's recompute rule.
                if matches!(level, OptLevel::O3) {
                    let preheader_stats = loops::normalize_preheaders(&mut cfg, type_pool)?;
                    stats.add_preheader_normalization(preheader_stats);
                    let licm_stats = licm::run(&mut cfg, type_pool)?;
                    stats.add_licm(licm_stats);
                    // Full constant-trip unrolling follows LICM and is
                    // followed by a mandatory cleanup fixpoint. Analyses are
                    // recomputed by the pass after every CFG mutation.
                    let unroll = unroll::run_with_budget(&mut cfg, &mut budget)?;
                    stats.add_unroll(unroll);
                    stats.code_growth_used = budget.used_values().saturating_sub(initial_values);
                    stats.code_growth_blocks_used =
                        budget.used_blocks().saturating_sub(initial_blocks);
                    let constopt_stats = constopt::run(&mut cfg);
                    stats.add_constopt(constopt_stats);
                    let simplify_stats = simplify::run(&mut cfg)?;
                    stats.add_simplify(simplify_stats);
                    dce::run(&mut cfg);
                }

                // Dead code elimination: remove unused values and unreachable blocks
                dce::run(&mut cfg);
            }
        }
        Ok(())
    })();

    publish_optimization(cfg, pass_result, type_pool).map(|cfg| {
        stats.publication_verifier_dominator_computations += 1;
        (cfg, stats, budget)
    })
}

fn publish_optimization(
    cfg: crate::CfgEditor,
    pass_result: Result<(), CfgOptimizationError>,
    type_pool: &FrozenTypeInternPool,
) -> Result<ValidatedCfg, CfgOptimizationError> {
    pass_result?;
    // A pass that allocated past the published per-function block/value ceiling
    // (spec Appendix C.6:1) latched instead of wrapping an identity; report the
    // limit here rather than letting verification raise an E9000 about the
    // aliased identity it handed back.
    if let Some(error) = cfg.latched_capacity_error() {
        return Err(error.into());
    }
    // Recheck the graph handed to codegen. DCE deliberately leaves detached
    // dead values in the arena, so attachment completeness was established by
    // the strict pre-pass check above; all live attachments and uses are still
    // checked here. See `crate::verify`.
    cfg.finish_after_optimization(type_pool).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockId, Cfg, CfgInst, CfgInstData, CfgValue, Terminator, Type};
    use rue_span::Span;

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
    fn optimizer_rewrite_boundaries_are_explicitly_in_place() {
        // These passes run inside optimize_with_budget's private editor. Keep
        // this guard next to the pipeline so a future cleanup cannot silently
        // reintroduce a whole-CFG transactional clone at each pass boundary.
        for source in [
            include_str!("cse.rs"),
            include_str!("forward.rs"),
            include_str!("peephole.rs"),
            include_str!("simplify.rs"),
        ] {
            assert_eq!(source.matches("rewrite_value_uses(").count(), 0);
            assert_eq!(source.matches("rewrite_value_uses_in_place(").count(), 1);
        }
    }

    #[test]
    fn unrolling_edits_the_graph_in_place() {
        // RUE-1842, following RUE-1663: `unroll_one` used to run against a
        // whole-CFG transactional clone that protected nothing. An `Err`
        // propagates through `optimize_with_budget`'s `?` into
        // `publish_optimization`, whose first statement is `pass_result?`, so
        // the preserved original could never be read. RUE-1663 removed this
        // clone class from the sibling passes but explicitly left unroll out,
        // and the guard above does not watch it — hence this one.
        let source = include_str!("unroll.rs");
        assert_eq!(
            source.matches("let mut edited = cfg.clone();").count(),
            0,
            "unroll reacquired a transactional whole-CFG clone"
        );
        // The pristine read snapshot `unroll_one` needs is now `LoopSource`,
        // which copies the loop body rather than the function. Assert against
        // the production half only — the pass's own tests clone graphs to
        // compare them, and that is not what this watches.
        // `unrolling_never_clones_the_whole_graph` is the behavioural half, and
        // catches a clone spelled any other way.
        let production = source
            .split_once("#[cfg(test)]\nmod tests {")
            .map(|(before, _)| before)
            .expect("unroll.rs keeps its tests in one trailing module");
        assert_eq!(
            production.matches("cfg.clone()").count(),
            0,
            "unroll reacquired a whole-CFG clone"
        );
    }

    #[test]
    fn preheader_materialization_edits_the_graph_in_place() {
        // Preheader materialization runs inside optimize_with_budget's private
        // editor. A failed normalization edit propagates into `pass_result`,
        // and `publish_optimization` checks that result before publishing the
        // poisoned editor, so a second whole-CFG rollback owner is redundant.
        // Inspect production code only: loop-analysis tests clone graphs for
        // before/after and failure-injection fixtures.
        let source = include_str!("loops.rs");
        let production = source
            .split_once("#[cfg(test)]\nmod tests {")
            .map(|(before, _)| before)
            .expect("loops.rs keeps its tests in one trailing module");
        assert_eq!(
            production.matches("cfg.clone()").count(),
            0,
            "preheader materialization reacquired a whole-CFG clone"
        );
    }

    #[test]
    fn empty_canonical_preheader_is_reclaimed_before_publication() {
        // The entry Branch cannot itself be the loop preheader, so O3
        // normalization must split its loop edge. With no invariant work to
        // retain, final simplify threads around that empty block and DCE
        // reclaims it: the CFG handed to codegen has the same block population
        // as O2, which performs no normalization.
        let pool = rue_air::TypeInternPool::new().freeze();
        let mut cfg = Cfg::new(Type::UNIT, 0, 1, "test".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();
        let cond = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::BOOL);
        cfg.set_terminator(
            entry,
            Terminator::Branch {
                cond,
                then_block: header,
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block: exit,
                else_args: crate::payload::CfgElseArgs::EMPTY,
            },
        );
        cfg.set_terminator(
            header,
            Terminator::Branch {
                cond,
                then_block: body,
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block: exit,
                else_args: crate::payload::CfgElseArgs::EMPTY,
            },
        );
        cfg.set_terminator(
            body,
            Terminator::Goto {
                target: header,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        cfg.set_terminator(exit, Terminator::Return { value: None });
        let cfg = cfg.finish(&pool).unwrap();

        let o2 = optimize(cfg.clone(), OptLevel::O2, &pool).unwrap();
        let o3 = optimize(cfg, OptLevel::O3, &pool).unwrap();
        assert_eq!(o3.block_count(), o2.block_count());
        assert_eq!(o3.block_count(), 3);
    }

    #[test]
    fn nonempty_canonical_preheader_survives_final_cleanup() {
        // The body computes a dynamic but trap-free invariant used by the next
        // header iteration. LICM moves it into the materialized preheader; the
        // final cleanup may remove the now-empty body, but must retain the
        // nonempty preheader as the defining block outside the final loop.
        let pool = rue_air::TypeInternPool::new().freeze();
        let mut cfg = Cfg::new(
            Type::I32,
            0,
            3,
            "test".to_string(),
            vec![false, false, false],
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();
        let a = push(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::I32);
        let b = push(&mut cfg, entry, CfgInstData::Param { index: 1 }, Type::I32);
        let cond = push(&mut cfg, entry, CfgInstData::Param { index: 2 }, Type::BOOL);
        let current = cfg.add_block_param(header, Type::I32);
        let result = cfg.add_block_param(exit, Type::I32);
        let then_args = cfg.push_then_args([a]).unwrap();
        let else_args = cfg.push_else_args([a]).unwrap();
        cfg.set_terminator(
            entry,
            Terminator::Branch {
                cond,
                then_block: header,
                then_args,
                else_block: exit,
                else_args,
            },
        );
        let else_args = cfg.push_else_args([current]).unwrap();
        cfg.set_terminator(
            header,
            Terminator::Branch {
                cond,
                then_block: body,
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block: exit,
                else_args,
            },
        );
        let invariant = push(&mut cfg, body, CfgInstData::BitOr(a, b), Type::I32);
        let args = cfg.push_goto_args([invariant]).unwrap();
        cfg.set_terminator(
            body,
            Terminator::Goto {
                target: header,
                args,
            },
        );
        cfg.set_terminator(
            exit,
            Terminator::Return {
                value: Some(result),
            },
        );
        let cfg = cfg.finish(&pool).unwrap();
        let optimized = optimize(cfg, OptLevel::O3, &pool).unwrap();

        let defining_block = optimized
            .block_ids()
            .find(|&block| optimized.get_block(block).insts.contains(&invariant))
            .expect("the live invariant remains attached");
        let dom = crate::dominators::DominatorTree::compute(&optimized);
        let forest = loops::loops(&optimized, &dom);
        assert_eq!(forest.len(), 1);
        assert_eq!(
            loops::preheader(&optimized, forest.get(0)),
            Some(defining_block)
        );
        assert!(!forest.get(0).contains(defining_block));
    }

    #[test]
    fn optimizer_edit_failures_preserve_the_failure_kind() {
        let cfg = crate::Cfg::new(crate::Type::UNIT, 0, 0, "test".to_string(), vec![]);
        let type_pool = rue_air::TypeInternPool::new().freeze();
        let error = CfgEditError::CapacityFailure {
            family: "optimizer failure injection",
        };
        let propagated = publish_optimization(cfg, Err(error.into()), &type_pool).unwrap_err();
        assert!(matches!(
            propagated,
            CfgOptimizationError::Edit(CfgEditError::CapacityFailure {
                family: "optimizer failure injection"
            })
        ));
    }

    #[test]
    fn test_opt_level_from_str() {
        assert_eq!("O0".parse::<OptLevel>().unwrap(), OptLevel::O0);
        assert_eq!("O1".parse::<OptLevel>().unwrap(), OptLevel::O1);
        assert_eq!("O2".parse::<OptLevel>().unwrap(), OptLevel::O2);
        assert_eq!("O3".parse::<OptLevel>().unwrap(), OptLevel::O3);

        // Also accept just the number
        assert_eq!("0".parse::<OptLevel>().unwrap(), OptLevel::O0);
        assert_eq!("1".parse::<OptLevel>().unwrap(), OptLevel::O1);
        assert_eq!("2".parse::<OptLevel>().unwrap(), OptLevel::O2);
        assert_eq!("3".parse::<OptLevel>().unwrap(), OptLevel::O3);

        // Invalid
        assert!("O4".parse::<OptLevel>().is_err());
        assert!("fast".parse::<OptLevel>().is_err());
    }

    #[test]
    fn test_opt_level_display() {
        assert_eq!(format!("{}", OptLevel::O0), "-O0");
        assert_eq!(format!("{}", OptLevel::O1), "-O1");
        assert_eq!(format!("{}", OptLevel::O2), "-O2");
        assert_eq!(format!("{}", OptLevel::O3), "-O3");
    }

    #[test]
    fn test_opt_level_default() {
        assert_eq!(OptLevel::default(), OptLevel::O0);
    }

    #[test]
    fn code_growth_budget_is_checked_and_carries_usage() {
        let mut budget = CodeGrowthBudget::o3();
        let growth = CodeGrowth {
            values: 200,
            blocks: 20,
        };
        assert!(budget.try_charge(growth));
        assert_eq!(budget.used(), 200);
        assert_eq!(budget.remaining(), 56);
        assert_eq!(budget.used_blocks(), 20);
        assert_eq!(budget.remaining_blocks(), 236);
        assert!(!budget.try_charge(CodeGrowth {
            values: 57,
            blocks: 1,
        }));
        assert_eq!(budget.used(), 200);
        assert!(!budget.try_charge(CodeGrowth {
            values: 1,
            blocks: 237,
        }));
        assert!(!budget.try_charge(CodeGrowth {
            values: u64::MAX,
            blocks: 0,
        }));
        let carried = CodeGrowthBudget::with_used(budget.used(), budget.used_blocks());
        assert_eq!(carried.remaining(), 56);
        assert_eq!(carried.remaining_blocks(), 236);
    }
}
