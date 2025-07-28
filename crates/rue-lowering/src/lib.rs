//! HIR to MIR lowering and MIR verification
//!
//! This crate handles the transformation from High-level IR (HIR) to
//! Mid-level IR (MIR), as well as MIR verification passes.

pub mod hir_to_mir;
pub mod verifier;

pub use hir_to_mir::MirBuilder;
pub use verifier::MirVerifier;

#[cfg(test)]
mod hir_to_mir_test;
#[cfg(test)]
mod mir_lowering_test;
#[cfg(test)]
mod mir_test;
