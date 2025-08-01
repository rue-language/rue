// High-level code generation using HIR (High-level Intermediate Representation)
//
// This crate handles code generation from HIR to executable binaries.
// The old AST-based code generation has been removed in favor of HIR-based compilation.

// Internal modules
pub mod backend;
pub mod constants;
mod regalloc;
pub mod runtime;
mod util;
mod x86_64_backend;

// Target-specific modules
pub mod target;

// Re-export runtime functionality for use by rue-compiler
pub use backend::RuntimeProvider;

// Re-export target types for external use
pub use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};

// Re-export register allocator for use by rue-compiler
pub use regalloc::RegisterAllocator;

// Re-export x86-64 backend types
pub use x86_64_backend::{LoweringError, X8664Codegen};

// Re-export internals for advanced usage
pub mod internals {
    pub use crate::regalloc::RegisterAllocator;
    pub use crate::target::x86_64::X86Emitter;
    pub use crate::target::x86_64::format_instructions_as_assembly;
}

#[derive(Debug, Clone, PartialEq)]
pub enum CodegenError {
    UnsupportedInstruction(String),
    UndefinedLabel(String),
    RegisterAllocation(String),
    Io,
    InvalidOperation(String),
    MirVerificationFailed,
    Lowering(LoweringError),
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodegenError::UnsupportedInstruction(msg) => {
                write!(f, "Unsupported instruction: {msg}")
            }
            CodegenError::UndefinedLabel(label) => write!(f, "Undefined label: {label}"),
            CodegenError::RegisterAllocation(msg) => {
                write!(f, "Register allocation error: {msg}")
            }
            CodegenError::Io => write!(f, "I/O error"),
            CodegenError::InvalidOperation(msg) => write!(f, "Invalid operation: {msg}"),
            CodegenError::MirVerificationFailed => {
                write!(f, "MIR verification failed")
            }
            CodegenError::Lowering(err) => write!(f, "Lowering error: {err}"),
        }
    }
}

impl std::error::Error for CodegenError {}
