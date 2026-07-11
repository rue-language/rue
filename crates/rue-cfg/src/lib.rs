//! Control Flow Graph IR for the Rue compiler.
//!
//! This crate provides a CFG-based intermediate representation that sits
//! between AIR (typed, structured) and X86Mir (machine-specific).
//!
//! The CFG representation makes control flow explicit through basic blocks
//! and terminators, which is essential for:
//! - Linear type checking
//! - Drop elaboration
//! - Liveness analysis
//! - Other dataflow analyses
//!
//! ## Pipeline
//!
//! ```text
//! AIR (structured) → CFG (explicit control flow) → X86Mir (machine code)
//! ```

mod build;
mod inst;
pub mod opt;
mod verify;

use rue_error::{CompileError, CompileWarning};

pub use build::CfgBuilder;
pub use inst::{
    BasicBlock, BlockId, Cfg, CfgArgMode, CfgCallArg, CfgDisplay, CfgInst, CfgInstData, CfgValue,
    Place, PlaceBase, Projection, Terminator,
};
pub use opt::OptLevel;

// Re-export types from rue-air that we use
pub use rue_air::{StructDef, StructId, Type, TypeKind};

/// Output from CFG construction.
///
/// Contains the constructed CFG along with any warnings detected during
/// construction (e.g., unreachable code).
pub struct CfgOutput {
    /// The constructed control flow graph.
    pub cfg: Cfg,
    /// Warnings detected during CFG construction.
    pub warnings: Vec<CompileWarning>,
    /// Internal-compiler-error diagnostics detected during CFG construction
    /// (RUE-7). Non-empty only for malformed AIR that upstream passes should
    /// have ruled out (e.g. an un-specialized `CallGeneric`). The driver must
    /// treat a non-empty `errors` as a hard failure and abort before
    /// optimizing or lowering the (now-discarded) CFG.
    pub errors: Vec<CompileError>,
}
