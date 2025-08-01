//! Runtime instruction generation for Rue language
//!
//! This module handles the generation of machine instructions for runtime functions
//! that are embedded in Rue executables. It provides assembly code for basic I/O
//! operations, syscalls, and other runtime support functions.

pub mod x86_64;

pub use x86_64::{RuntimeContext, generate_runtime};
