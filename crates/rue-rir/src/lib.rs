//! Rue Intermediate Representation (RIR) - Untyped IR.
//!
//! RIR is the first IR in the Rue compiler pipeline. It is generated from
//! the AST and represents a lowered, linearized form of the program.
//!
//! Key characteristics:
//! - Untyped: type information is not yet resolved
//! - Per-file: generated for each source file
//! - Dense encoding: instructions stored in arrays, referenced by index
//!
//! Inspired by Zig's ZIR (Zig Intermediate Representation).

mod astgen;
mod inst;

pub use astgen::AstGen;
pub use inst::{
    Inst, InstData, InstRef, Rir, RirCallArg, RirDirective, RirParam, RirParamMode, RirPattern,
    RirPrinter,
};
