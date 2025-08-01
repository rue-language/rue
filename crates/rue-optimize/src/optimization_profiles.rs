//! Predefined optimization profiles for different optimization levels
//!
//! This module provides factory functions for creating optimization pass
//! sequences for different optimization levels (-O0, -O1, -O2).

use crate::pass_manager::{OptimizationLevel, Pass, PassManager};
use crate::passes::{CommonSubexpressionElimination, ConstProp, DeadCodeElimination};
use rue_ir::mir::MirProgram;

/// Factory for creating optimization passes based on optimization level
pub struct OptimizationProfileFactory;

impl OptimizationProfileFactory {
    /// Create a pass manager configured for the given optimization level
    pub fn create_pass_manager(level: OptimizationLevel) -> PassManager {
        PassManager::for_optimization_level(level)
    }

    /// Run optimization passes for the given level on a MIR program
    pub fn optimize_program(program: &mut MirProgram, level: OptimizationLevel) {
        match level {
            OptimizationLevel::None => {
                // No optimizations
            }
            OptimizationLevel::Basic => {
                Self::run_basic_optimizations(program);
            }
            OptimizationLevel::Full => {
                Self::run_full_optimizations(program);
            }
        }
    }

    /// Run basic optimizations (-O1)
    ///
    /// Basic optimizations focus on simple, fast passes that provide
    /// good code improvements with minimal compilation time overhead.
    fn run_basic_optimizations(program: &mut MirProgram) {
        // Run passes individually for now to avoid complex type system issues
        let mut const_prop = ConstProp::new();
        const_prop.run(program); // Use legacy interface

        let mut dce = DeadCodeElimination::new();
        dce.run(program); // Use legacy interface
    }

    /// Run full optimizations (-O2)
    ///
    /// Full optimizations include all available passes and run them
    /// to a true fixed-point for maximum code quality.
    fn run_full_optimizations(program: &mut MirProgram) {
        // Run multiple iterations manually for fixed-point behavior
        const MAX_ITERATIONS: usize = 10;

        for _iteration in 0..MAX_ITERATIONS {
            let mut changed = false;

            // Run constant propagation
            let mut const_prop = ConstProp::new();
            let result = Pass::run(&mut const_prop, program);
            if result.changed {
                changed = true;
            }

            // Run CSE
            let mut cse = CommonSubexpressionElimination::new();
            let result = Pass::run(&mut cse, program);
            if result.changed {
                changed = true;
            }

            // Run dead code elimination
            let mut dce = DeadCodeElimination::new();
            let result = Pass::run(&mut dce, program);
            if result.changed {
                changed = true;
            }

            // If no pass made changes, we've reached fixed-point
            if !changed {
                break;
            }
        }
    }

    /// Get a description of what optimizations are performed at each level
    pub fn describe_level(level: OptimizationLevel) -> &'static str {
        match level {
            OptimizationLevel::None => {
                "No optimizations are performed. Code is generated as-is from the HIR."
            }
            OptimizationLevel::Basic => {
                "Basic optimizations: constant propagation and dead code elimination. \
                 Fast compilation with good code improvements."
            }
            OptimizationLevel::Full => {
                "Full optimizations: constant propagation, common subexpression elimination, \
                 and dead code elimination. Runs to fixed-point for maximum code quality."
            }
        }
    }

    /// Get the recommended optimization level for different use cases
    pub fn recommend_for_use_case(use_case: &str) -> OptimizationLevel {
        match use_case {
            "debug" | "development" => OptimizationLevel::None,
            "testing" | "ci" => OptimizationLevel::Basic,
            "release" | "production" => OptimizationLevel::Full,
            _ => OptimizationLevel::Basic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_ir::mir::{BasicBlock, BlockId, MirFunction, MirProgram, MirTerminator};
    use rue_ir::types::RueType;
    use rue_lexer::Span;
    use std::collections::HashMap;

    fn create_test_program() -> MirProgram {
        MirProgram {
            functions: vec![MirFunction {
                name: "test".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                span: Span::dummy(),
                temp_types: HashMap::new(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![],
                    terminator: MirTerminator::Return {
                        value: None,
                        span: None,
                    },
                }],
            }],
            function_signatures: HashMap::new(),
        }
    }

    #[test]
    fn test_optimization_levels() {
        let program = create_test_program();

        // Test that each optimization level runs without errors
        for level in [
            OptimizationLevel::None,
            OptimizationLevel::Basic,
            OptimizationLevel::Full,
        ] {
            let mut test_program = program.clone();
            OptimizationProfileFactory::optimize_program(&mut test_program, level);
            // Program should still be valid after optimization
            assert!(!test_program.functions.is_empty());
        }
    }

    #[test]
    fn test_pass_manager_creation() {
        for level in [
            OptimizationLevel::None,
            OptimizationLevel::Basic,
            OptimizationLevel::Full,
        ] {
            let manager = OptimizationProfileFactory::create_pass_manager(level);
            assert_eq!(manager.statistics().total_passes, 0); // Should start empty
        }
    }

    #[test]
    fn test_use_case_recommendations() {
        assert_eq!(
            OptimizationProfileFactory::recommend_for_use_case("debug"),
            OptimizationLevel::None
        );
        assert_eq!(
            OptimizationProfileFactory::recommend_for_use_case("testing"),
            OptimizationLevel::Basic
        );
        assert_eq!(
            OptimizationProfileFactory::recommend_for_use_case("release"),
            OptimizationLevel::Full
        );
        assert_eq!(
            OptimizationProfileFactory::recommend_for_use_case("unknown"),
            OptimizationLevel::Basic
        );
    }

    #[test]
    fn test_level_descriptions() {
        for level in [
            OptimizationLevel::None,
            OptimizationLevel::Basic,
            OptimizationLevel::Full,
        ] {
            let description = OptimizationProfileFactory::describe_level(level);
            assert!(!description.is_empty());
            assert!(description.len() > 20); // Should be a meaningful description
        }
    }
}
