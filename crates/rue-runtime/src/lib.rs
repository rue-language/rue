//! Runtime library for Rue language
//!
//! This crate provides the runtime functions embedded in Rue executables.
//! It generates assembly code for basic I/O operations and syscalls.

pub mod constants;
pub mod functions;
pub mod helpers;
pub mod machine_runtime;
pub mod syscalls;

pub use machine_runtime::generate_runtime;

#[cfg(test)]
mod test;
