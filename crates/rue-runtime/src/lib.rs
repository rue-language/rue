//! Rue Runtime Library
//!
//! This crate provides minimal runtime support for Rue programs.
//! It's designed to be compiled as a staticlib and linked into
//! Rue executables.
//!
//! # Overview
//!
//! The Rue compiler generates machine code that calls into this runtime
//! for certain operations that can't be efficiently or safely inlined:
//!
//! - **Process exit**: When `main()` returns, generated code calls [`__rue_exit`](entry::__rue_exit)
//!   with the return value as the exit code.
//! - **Runtime errors**: Division by zero and integer overflow trigger calls to
//!   error handlers in the [`error`] module.
//! - **Debug output**: The `@dbg` builtin calls functions in the [`debug`] module.
//! - **String operations**: String equality, allocation, and methods are in the [`string`] module.
//! - **I/O operations**: Input functions like `readLine()` are in the [`io`] module.
//!
//! # Platform Requirements
//!
//! This runtime supports the following platforms:
//!
//! - **x86-64 Linux**
//! - **aarch64 Linux**
//! - **aarch64 macOS**
//!
//! It uses direct syscalls and contains platform-specific assembly.
//! Attempting to compile on other platforms will result in a compile error.
//!
//! # Design Philosophy
//!
//! The runtime is deliberately minimal:
//!
//! - **`#![no_std]`**: No dependency on the Rust standard library or libc.
//!   All OS interaction happens via direct syscalls.
//! - **Zero allocations**: The runtime never allocates memory (except when explicitly
//!   requested via `__rue_alloc`).
//! - **Small code size**: Compiled with `-Copt-level=z` and LTO for minimal footprint.
//!
//! # Calling Conventions
//!
//! All public functions use the C ABI (`extern "C"`) and are `#[no_mangle]` so
//! they can be called from Rue-generated machine code. The compiler generates
//! `call` instructions to these symbol names.
//!
//! # Exit Codes
//!
//! Rue programs use the following exit codes by convention:
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0 | Success (or whatever `main()` returned) |
//! | 1 | Panic (from Rust runtime, shouldn't happen in normal operation) |
//! | 101 | Runtime error (division by zero, overflow) |
//!
//! # Integration with the Compiler
//!
//! The `rue-linker` crate links this runtime library into every Rue executable.
//! The runtime is compiled as a static library (`.a` file) and its symbols are
//! referenced by generated code in `rue-codegen`.
//!
//! Specifically:
//! - `rue-codegen/src/x86_64/emit.rs` generates `call __rue_*` instructions
//! - `rue-linker` links the runtime archive into the final ELF executable

#![no_std]
// Doc comments before macro invocations are intentional - they document the functions
// that the macro generates. Rust can't attach them automatically, but they serve as
// documentation for readers of this source file.
#![allow(unused_doc_comments)]

// Platform-specific implementations
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod x86_64_linux;

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
mod aarch64_macos;

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
mod aarch64_linux;

// Heap allocation (available on all supported platforms)
#[cfg(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux")
))]
mod heap;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use x86_64_linux as platform;

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
use aarch64_macos as platform;

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
use aarch64_linux as platform;

// Compile error for unsupported platforms
#[cfg(not(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux")
)))]
compile_error!(
    "rue-runtime only supports x86-64 Linux, aarch64 Linux, and aarch64 macOS. \
     Other platforms are not currently supported."
);

// ============================================================================
// Platform-agnostic macro for reducing code duplication
// ============================================================================
//
// Many runtime functions have identical implementations across platforms,
// differing only in their `#[cfg]` attributes. This macro generates all
// three platform-specific versions from a single definition.

/// Define a function for all supported platforms with identical implementation.
///
/// This macro generates three `#[cfg]`-gated versions of the same function,
/// one for each supported platform (x86_64 Linux, aarch64 macOS, aarch64 Linux).
///
/// # Usage
///
/// ```ignore
/// define_for_all_platforms! {
///     /// Documentation for the function
///     pub extern "C" fn function_name(arg: Type) -> ReturnType {
///         // implementation
///     }
/// }
/// ```
#[macro_export]
macro_rules! define_for_all_platforms {
    (
        $(#[$meta:meta])*
        pub extern "C" fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) $(-> $ret:ty)? $body:block
    ) => {
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        $(#[$meta])*
        #[unsafe(no_mangle)]
        pub extern "C" fn $name($($arg : $arg_ty),*) $(-> $ret)? $body

        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        $(#[$meta])*
        #[unsafe(no_mangle)]
        pub extern "C" fn $name($($arg : $arg_ty),*) $(-> $ret)? $body

        #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
        $(#[$meta])*
        #[unsafe(no_mangle)]
        pub extern "C" fn $name($($arg : $arg_ty),*) $(-> $ret)? $body
    };
}

// ============================================================================
// Runtime modules
// ============================================================================

pub mod debug;
pub mod entry;
pub mod error;
pub mod io;
pub mod memory;
pub mod parse;
pub mod random;
pub mod string;

// Re-export StringResult for use by other modules
pub use string::StringResult;

// Re-export platform functions for tests
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub use x86_64_linux::{exit, write, write_all, write_stderr};

#[cfg(all(test, target_arch = "aarch64", target_os = "macos"))]
pub use aarch64_macos::{exit, write, write_all, write_stderr};

#[cfg(all(test, target_arch = "aarch64", target_os = "linux"))]
pub use aarch64_linux::{exit, write, write_all, write_stderr};
