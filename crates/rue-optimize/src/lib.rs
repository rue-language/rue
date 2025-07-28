//! MIR optimization passes
//!
//! This crate contains various optimization passes that operate on MIR,
//! including constant propagation, common subexpression elimination,
//! and dead code elimination.

pub mod passes;

pub use passes::{
    const_prop::ConstProp, cse::CommonSubexpressionElimination, dead_code::DeadCodeElimination,
};
