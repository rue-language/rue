//! Rue Runtime Library
//!
//! This crate provides the runtime functions for Rue programs, implemented in
//! no_std Rust for maximum performance and minimal size.
//!
//! The runtime is designed to be built as a static library and linked with Rue
//! executables. All functions follow the System V AMD64 ABI (C calling convention).
//!
//! # Architecture
//!
//! The runtime is organized into several modules:
//!
//! - `abi`: Type definitions, constants, and ABI documentation
//! - `syscall`: Low-level Linux syscall wrappers
//! - `buffered_io`: Buffered stdout implementation
//! - `io`: High-level I/O functions (println, input)
//! - `conversion`: String conversion functions (itoa, atoi)
//! - `memory`: Memory operations (memcpy, memmove, memset, memzero)
//!
//! # Safety
//!
//! Many functions in this crate are `unsafe` because they are called from
//! generated assembly code and must maintain strict invariants. Callers must
//! ensure:
//!
//! - Pointers are valid and properly aligned
//! - Sizes don't cause buffer overflows
//! - No data races occur (Rue is single-threaded)
//!
//! # Example
//!
//! The compiler generates calls to runtime functions:
//!
//! ```rue
//! let x = 42;
//! println(x);
//! ```
//!
//! Compiles to (approximately):
//!
//! ```asm
//! mov rdi, 42
//! call __rue_println_i64
//! ```

// Use no_std except when testing
#![cfg_attr(not(test), no_std)]

// Module declarations
pub mod abi;
pub mod buffered_io;
pub mod conversion;
pub mod io;
pub mod memory;
pub mod syscall;

// Re-export C ABI functions at crate root for easier linking
#[cfg(not(test))]
pub use buffered_io::{__rue_flush_stdout, __rue_write_byte, __rue_write_bytes};

#[cfg(not(test))]
pub use conversion::{__rue_atoi, __rue_itoa};

#[cfg(not(test))]
pub use io::{
    __rue_input, __rue_println_bool, __rue_println_i32, __rue_println_i64, __rue_println_unit,
};

#[cfg(not(test))]
pub use memory::{
    __rue_detect_cpu_features, __rue_memcpy, __rue_memmove, __rue_memset, __rue_memzero,
};

/// Panic handler for no_std
///
/// In case of panic, we exit with error code 255 (EXIT_PANIC).
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe {
        // Exit with error code 255
        core::arch::asm!(
            "mov rax, 60",  // sys_exit
            "mov rdi, 255", // exit code EXIT_PANIC
            "syscall",
            options(noreturn)
        )
    }
}
