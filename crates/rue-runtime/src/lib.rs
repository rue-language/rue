//! Rue Runtime Library
//!
//! This crate provides the complete runtime for Rue programs, including both
//! CRT0 (C runtime startup) functionality and all runtime functions.
//! It consolidates what was previously split between rue-runtime and rue-crt0.
//!
//! The crate is designed to be built as a static library and linked
//! with Rue executables to provide both startup code (_start) and
//! runtime services.

#![cfg_attr(not(test), no_std)]

// All modules are included when not in test mode
#[cfg(not(test))]
pub mod allocator;

#[cfg(not(test))]
pub mod buffered_io;

#[cfg(not(test))]
pub mod conversions;

#[cfg(not(test))]
pub mod formatting;

#[cfg(not(test))]
pub mod input;

#[cfg(not(test))]
pub mod intrinsics;

#[cfg(not(test))]
pub mod memory;

#[cfg(not(test))]
pub mod startup;

#[cfg(not(test))]
pub mod system;

// Re-export all the __rue_ functions at the crate root for easier linking
#[cfg(not(test))]
pub use allocator::{__rue_alloc, __rue_free, __rue_heap_init, __rue_heap_reset};

#[cfg(not(test))]
pub use buffered_io::{__rue_flush_stdout, __rue_write_byte, __rue_write_bytes};

#[cfg(not(test))]
pub use conversions::{__rue_atoi, __rue_itoa, __rue_to_i32, __rue_to_i64};

#[cfg(not(test))]
pub use formatting::{
    __rue_println_bool, __rue_println_i32, __rue_println_i64, __rue_println_unit,
};

#[cfg(not(test))]
pub use input::__rue_input;

#[cfg(not(test))]
pub use memory::{__rue_memcmp, __rue_memcpy, __rue_memmove, __rue_memset};

#[cfg(not(test))]
pub use startup::{__rue_exit, __rue_main, _start};

#[cfg(not(test))]
pub use system::{
    __rue_get_cpu_features, __rue_setup_signal_handlers, __rue_sigfpe_handler,
    __rue_sigsegv_handler,
};

// Panic handler for no_std
// In case of panic, we just exit with error code 255
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe {
        // Exit with error code 126 on panic in runtime
        core::arch::asm!(
            "mov rax, 60",  // SYS_EXIT
            "mov rdi, 126", // Exit code 126
            "syscall",
            options(noreturn)
        );
    }
}

// When testing, provide an empty module
#[cfg(test)]
mod tests {}
