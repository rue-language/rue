//! Rue Runtime Library
//!
//! This crate provides optimized runtime functions for Rue programs,
//! implemented in no_std Rust for maximum performance and minimal size.
//!
//! The crate is designed to be built as a static library and linked
//! with Rue executables to provide efficient runtime services.

// Use no_std except when testing
#![cfg_attr(not(test), no_std)]

// Include buffered_io module when not in test mode
#[cfg(not(test))]
pub mod buffered_io;

// Re-export the C ABI functions at the crate root for easier linking
#[cfg(not(test))]
pub use buffered_io::{__rue_flush_stdout, __rue_write_byte, __rue_write_bytes};

// Panic handler for no_std
// In case of panic, we just exit with error code 255
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe {
        // Exit with error code 255
        core::arch::asm!(
            "mov rax, 60",  // sys_exit
            "mov rdi, 255", // exit code 255
            "syscall",
            options(noreturn)
        )
    }
}
