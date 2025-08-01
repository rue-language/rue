//! Intermediate representations for the Rue compiler
//!
//! This crate defines the data structures for various intermediate representations
//! used throughout the compilation pipeline. It contains only pure data structures
//! and their basic operations - no transformations or analysis passes.

pub mod cfg;
pub mod hir;
pub mod mir;
pub mod mir_lowering;
pub mod mir_verifier;
pub mod target;
pub mod types;
