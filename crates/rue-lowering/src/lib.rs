//! IR lowering phases
//!
//! This crate handles IR transformations between different representations:
//! - HIR to MIR lowering
//! - MIR verification
//! - MIR to PIR lowering
//!
//! Note: MIR optimizations are handled separately in the `rue-optimize` crate.
//! Note: PIR to target instruction lowering is handled in the `rue-codegen` crate.

pub mod hir2_to_mir;
pub mod hir_to_mir;
pub mod mir_to_pir;
pub mod verifier;

pub use hir_to_mir::MirBuilder;
pub use hir2_to_mir::lower_hir2_to_mir;
pub use mir_to_pir::MirToPir;
pub use verifier::MirVerifier;

#[cfg(test)]
mod hir2_to_mir_test;
#[cfg(test)]
mod hir_to_mir_test;
#[cfg(test)]
mod mir_lowering_test;
#[cfg(test)]
mod mir_test;
