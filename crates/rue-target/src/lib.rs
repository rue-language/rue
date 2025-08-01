//! Target architecture definitions for the Rue compiler
//!
//! This crate contains target-specific types that are shared across multiple
//! crates in the compilation pipeline. By keeping these types in a separate
//! crate, we avoid circular dependencies between rue-codegen and rue-runtime.

pub mod x86_64;

// Re-export commonly used types
pub use x86_64::{ConditionCode, LabelRef, X86Register, X8664Instr};
