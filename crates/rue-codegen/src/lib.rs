// High-level code generation using HIR (High-level Intermediate Representation)
//
// This crate handles code generation from HIR to executable binaries.
// The old AST-based code generation has been removed in favor of HIR-based compilation.

// Internal modules
pub mod assembler;
pub mod backend;
pub mod linker;
mod regalloc;
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

// Re-export linker types
pub use linker::{ObjectFile, RelocationEntry, RelocationKind, Symbol, SymbolKind, SymbolTable};

// Re-export linker and assembler types
pub use assembler::Assembler;
pub use linker::archive::Archive;
pub use linker::asm_object::{AsmObject, AsmObjectBuilder};
pub use linker::{LinkedExecutable, Linker};

// Internals are now private - removed since they're unused

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CodegenError {
    #[error("Unsupported instruction: {0}")]
    UnsupportedInstruction(String),

    #[error("Undefined label: {0}")]
    UndefinedLabel(String),

    #[error("Register allocation error: {0}")]
    RegisterAllocation(String),

    #[error("I/O error")]
    Io,

    #[error("Invalid operation: {0}")]
    InvalidOperation(String),

    #[error("MIR verification failed")]
    MirVerificationFailed,

    #[error("Lowering error: {0}")]
    Lowering(#[from] LoweringError),
}
