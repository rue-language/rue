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
//!   block-local common-subexpression elimination (RUE-913)
//! - `-O3`: `-O2` plus loop-invariant code motion (RUE-927), which hoists
//!   trap-free invariant computations out of loops into their preheaders, and
//!   bounded constant-trip full unrolling (RUE-928)
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
    /// SSA value already holding the slot's contents, followed by block-local
    /// common-subexpression elimination (RUE-913), which replaces duplicate
    /// pure computations within a block with their first occurrence.
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
    pub loops_analyzed: u64,
    pub loops_unrolled: u64,
    pub budget_refusals: u64,
    /// CFG values cloned by O3 growth transforms in this invocation.
    pub code_growth_used: u64,
    /// CFG blocks cloned by O3 growth transforms in this invocation.
    pub code_growth_blocks_used: u64,
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
                constopt::run(&mut cfg);

                // Peephole algebraic simplification (RUE-912): rewire trap-free
                // identities (x+0, x*1, ...) to their operand and strength-reduce
                // unsigned division/modulo by powers of two. Runs after constopt
                // so propagated constants are visible; annihilators that produce
                // constants (x*0, x-x, ...) live in the constfold kernel inside
                // the worklist instead, because their results cascade.
                peephole::run(&mut cfg)?;

                // CFG simplification (RUE-910, RUE-911): fold constant-condition
                // Branch/Switch terminators into Gotos so dead arms drop out of
                // reachability, then thread empty forwarding blocks and merge
                // single-predecessor Goto chains into straight-line blocks
                // before DCE prunes the leftovers.
                let simplify_stats = simplify::run(&mut cfg)?;
                // Folding a control value can expose stores and loads on the
                // surviving path (notably drop flags). Re-run the sparse
                // constant/folding cleanup when control flow changed so those
                // newly unreachable ownership actions are removed before
                // forwarding and CSE inspect the graph.
                if simplify_stats.branches_folded > 0 || simplify_stats.switches_folded > 0 {
                    dce::run(&mut cfg);
                    constopt::run(&mut cfg);
                    peephole::run(&mut cfg)?;
                    simplify::run(&mut cfg)?;
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
                    // Forwarding can expose constants in a branch condition
                    // without changing the Load instruction itself. Fold and
                    // simplify that newly constant control flow before CSE;
                    // this is essential for drop-flag stores revealed after a
                    // selector branch is folded.
                    if forward_stats.loads_forwarded_single_write > 0
                        || forward_stats.loads_forwarded_block_local > 0
                    {
                        constopt::run(&mut cfg);
                        simplify::run(&mut cfg)?;
                    }
                }

                // Block-local common-subexpression elimination (RUE-913), at
                // -O2/-O3 only (ADR-0044 places CSE at the release-default level).
                // Runs after forwarding — expressions over forwarded loads are now
                // keyable — and before DCE, which sweeps the dead placeholders each
                // replaced duplicate (and each forwarded load) leaves behind.
                if matches!(level, OptLevel::O2 | OptLevel::O3) {
                    cse::run(&mut cfg)?;
                }

                // Loop-invariant code motion (RUE-927), at -O3 only — the first
                // pass gated strictly above -O2 (ADR-0054 Phase 2). Runs after
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
                    licm::run(&mut cfg, type_pool)?;
                    // Full constant-trip unrolling follows LICM and is
                    // followed by a mandatory cleanup fixpoint. Analyses are
                    // recomputed by the pass after every CFG mutation.
                    let unroll = unroll::run_with_budget(&mut cfg, &mut budget)?;
                    stats.loops_analyzed = unroll.loops_analyzed;
                    stats.loops_unrolled = unroll.loops_unrolled;
                    stats.budget_refusals = unroll.budget_refusals;
                    stats.code_growth_used = budget.used_values().saturating_sub(initial_values);
                    stats.code_growth_blocks_used =
                        budget.used_blocks().saturating_sub(initial_blocks);
                    constopt::run(&mut cfg);
                    simplify::run(&mut cfg)?;
                    dce::run(&mut cfg);
                }

                // Dead code elimination: remove unused values and unreachable blocks
                dce::run(&mut cfg);
            }
        }
        Ok(())
    })();

    publish_optimization(cfg, pass_result, type_pool).map(|cfg| (cfg, stats, budget))
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
