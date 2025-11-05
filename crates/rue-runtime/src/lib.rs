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

/// Stub heap initialization
///
/// The current Rue implementation doesn't use heap allocation, so this is a no-op.
/// Kept for compatibility with the startup sequence.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_heap_init() {
    // No-op: Rue doesn't currently use heap allocation
}

/// Stub signal handler setup
///
/// Signal handlers are currently set up in the generated startup code.
/// This stub is provided for compatibility.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_setup_signal_handlers() {
    // No-op: Signal handlers are set up in generated code
}

/// Stub allocation function
///
/// Currently returns a simple bump allocator using a static buffer.
/// This is a minimal implementation for basic struct support.
///
/// # Safety
/// Caller must ensure size is reasonable and doesn't exhaust the heap.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_alloc(size: usize) -> *mut u8 {
    use core::sync::atomic::{AtomicUsize, Ordering};

    // Simple bump allocator with 1MB static buffer
    static mut HEAP: [u8; 1024 * 1024] = [0; 1024 * 1024];
    static HEAP_POS: AtomicUsize = AtomicUsize::new(0);

    let offset = HEAP_POS.fetch_add(size, Ordering::SeqCst);

    // Check if we've exceeded heap size
    const HEAP_SIZE: usize = 1024 * 1024;
    if offset + size > HEAP_SIZE {
        // Out of memory - exit with error
        syscall::sys_exit(254);
    }

    unsafe {
        let heap_ptr = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        heap_ptr.add(offset)
    }
}

/// Stub free function
///
/// Current allocator is a simple bump allocator that doesn't support freeing.
/// This is a no-op for compatibility.
///
/// # Safety
/// This is safe to call but does nothing.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_free(_ptr: *mut u8) {
    // No-op: bump allocator doesn't support freeing
}

/// Function pointer to memzero implementation
///
/// This is used by generated code to call memzero indirectly.
/// Points to __rue_memzero which handles ERMS detection internally.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub static __rue_memzero_ptr: unsafe extern "C" fn(*mut u8, usize) = __rue_memzero;
