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
//!   trap-free invariant computations out of loops into their preheaders
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
    /// Superset of `-O2`: adds loop-invariant code motion (RUE-927), which
    /// hoists trap-free loop-invariant computations into each loop's preheader.
    /// Trapping invariant ops are never moved (ADR-0054 §2). Further speculative
    /// transforms (unrolling, RUE-928) will be added here.
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
    let mut cfg = cfg.into_editor();

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
                simplify::run(&mut cfg)?;

                // Value forwarding / copy propagation (RUE-914), at -O2/-O3 only.
                // Runs after simplify and before CSE: it turns redundant `Load`s
                // into the SSA value already holding the slot's contents (a global
                // single-write rule plus a block-local last-store rule), which is
                // exactly what lets CSE key expressions built over those loads.
                // Both rules are trap-exact — a load never traps and the forwarded
                // value is already computed — so the orphaned loads fall to DCE.
                if matches!(level, OptLevel::O2 | OptLevel::O3) {
                    forward::run(&mut cfg)?;
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
                }

                // Dead code elimination: remove unused values and unreachable blocks
                dce::run(&mut cfg);
            }
        }
        Ok(())
    })();

    publish_optimization(cfg, pass_result, type_pool)
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
}
