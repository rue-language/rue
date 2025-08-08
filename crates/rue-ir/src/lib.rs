//! Intermediate representations for the Rue compiler
//!
//! This crate defines the data structures for various intermediate representations
//! used throughout the compilation pipeline. It contains only pure data structures
//! and their basic operations - no transformations or analysis passes.

pub mod ast;
pub mod cfg;
pub mod hir;
pub mod hir_builder;
pub mod mir;
pub mod mir_verifier;
pub mod pir;
pub mod types;

#[cfg(test)]
mod debug_offsets_test;
#[cfg(test)]
mod hir_tests;
