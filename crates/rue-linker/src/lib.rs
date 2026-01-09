//! Minimal linker for the Rue compiler.
//!
//! This linker handles:
//! - Reading ELF64 relocatable object files (.o)
//! - Reading ar archives (.a) containing object files
//! - Creating ELF64 relocatable object files
//! - Resolving symbols between objects
//! - Applying relocations
//! - Producing a final executable
//!
//! It's intentionally minimal - just enough to link Rue code with its runtime.

mod archive;
pub mod constants;
mod elf;
mod emit;
mod linker;
pub mod macho;

pub use archive::{Archive, ArchiveError};
pub use elf::{ObjectFile, Relocation, RelocationType, Section, Symbol};
pub use emit::{CodeRelocation, ObjectBuilder};
pub use linker::{LinkError, Linker};
