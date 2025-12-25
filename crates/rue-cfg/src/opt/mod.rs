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
//! - `-O1`: Basic optimizations (constant folding, dead code elimination)
//! - `-O2`: Standard optimizations (same as -O1 for now)
//! - `-O3`: Aggressive optimizations (same as -O2 for now)
//!
//! ## Pipeline
//!
//! Optimizations run after CFG construction and before lowering to MIR:
//!
//! ```text
//! AIR -> CfgBuilder -> CFG -> [optimize] -> CfgLower -> MIR
//! ```

mod constfold;
mod dce;

use crate::Cfg;

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
    /// Currently the same as O1. Future optimization passes will be
    /// added at this level.
    O2,

    /// Aggressive optimizations (`-O3`).
    ///
    /// Currently the same as O2. Future aggressive optimizations
    /// (that may increase compile time significantly) will be added here.
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
/// optimize(&mut cfg, OptLevel::O1);
/// // cfg is now optimized
/// ```
pub fn optimize(cfg: &mut Cfg, level: OptLevel) {
    match level {
        OptLevel::O0 => {
            // No optimization
        }
        OptLevel::O1 | OptLevel::O2 | OptLevel::O3 => {
            // Constant folding: fold operations on compile-time constants
            constfold::run(cfg);

            // Dead code elimination: remove unused values and unreachable blocks
            dce::run(cfg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
