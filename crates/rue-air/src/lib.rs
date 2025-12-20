//! Analyzed Intermediate Representation (AIR) - Typed IR.
//!
//! AIR is the second IR in the Rue compiler pipeline. It is generated from
//! RIR after semantic analysis and type checking.
//!
//! Key characteristics:
//! - Fully typed: all types are resolved
//! - Per-function: generated lazily for each function
//! - Ready for codegen: can be lowered directly to machine code
//!
//! Inspired by Zig's AIR (Analyzed Intermediate Representation).

mod inst;
mod sema;
mod types;

pub use inst::{Air, AirInst, AirInstData, AirPattern, AirRef};
pub use sema::{AnalyzedFunction, Sema, SemaOutput};
pub use types::{ArrayTypeDef, ArrayTypeId, StructDef, StructField, StructId, Type};
