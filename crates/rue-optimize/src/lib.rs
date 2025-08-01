//! MIR optimization passes
//!
//! This crate contains various optimization passes that operate on MIR,
//! including constant propagation, common subexpression elimination,
//! and dead code elimination.
//!
//! It also provides a Pass Manager Framework for orchestrating optimization
//! passes with features like fixed-point iteration, optimization levels,
//! and detailed pass statistics.

pub mod optimization_profiles;
pub mod pass_manager;
pub mod passes;
pub mod visitor;

pub use optimization_profiles::OptimizationProfileFactory;
pub use pass_manager::{
    OptimizationLevel, Pass, PassConfig, PassManager, PassResult, PassStatistics,
};
pub use passes::{
    const_prop::ConstProp, cse::CommonSubexpressionElimination, dead_code::DeadCodeElimination,
};
pub use visitor::{
    MirFolder, MirMutVisitor, MirVisitor, VisitorControl, fold_block, fold_function, fold_program,
    fold_statement, fold_terminator, fold_value, walk_block, walk_block_mut, walk_function,
    walk_function_mut, walk_program, walk_program_mut, walk_statement, walk_statement_mut,
    walk_terminator, walk_terminator_mut, walk_value, walk_value_mut,
};
