//! Intermediate representations for the Rue compiler
//!
//! This crate defines the data structures for various intermediate representations
//! used throughout the compilation pipeline. It contains only pure data structures
//! and their basic operations - no transformations or analysis passes.

pub mod ast;
pub mod cfg;
pub mod hir;
pub mod hir2;
pub mod hir2_builder;
pub mod mir;
pub mod pir;
pub mod types;

#[cfg(test)]
mod debug_offsets_test;

#[cfg(test)]
mod hir2_tests;
