//! Intermediate representations for the Rue compiler
//!
//! This crate defines various IRs used throughout the compilation pipeline.

pub mod hir;
pub mod mir;
pub mod mir_lowering;
pub mod mir_passes;
pub mod mir_verifier;
pub mod target;
pub mod types;

#[cfg(test)]
mod mir_test;

#[cfg(test)]
mod mir_lowering_test;
