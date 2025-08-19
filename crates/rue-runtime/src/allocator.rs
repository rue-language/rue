//! Simple bump allocator for the Rue runtime
//!
//! This module provides a basic bump allocator using static memory:
//! - Fixed 64KB heap allocated in .bss section
//! - Allocation only (no free) - suitable for aggregates and temporary data
//! - 16-byte alignment for all allocations (SSE/AVX compatible)
//! - Can be upgraded to sys_brk or mmap later
use core::ptr;

/// Debug trace helper (duplicated from startup.rs to avoid circular deps)
#[cfg(debug_assertions)]
#[inline(always)]
unsafe fn debug_trace(msg: &[u8]) {
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") 1_i64, // SYS_WRITE
            in("rdi") 2_i64, // stderr
            in("rsi") msg.as_ptr() as i64,
            in("rdx") msg.len() as i64,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
}

/// No-op version for release builds
#[cfg(not(debug_assertions))]
#[inline(always)]
unsafe fn debug_trace(_msg: &[u8]) {
    // No-op in release builds
}

/// Heap size (64KB)
const HEAP_SIZE: usize = 64 * 1024;

/// Alignment for allocations (16 bytes for SSE/AVX)
const ALIGNMENT: usize = 16;

/// Static heap memory
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

/// Current heap pointer - using static mut instead of AtomicUsize
static mut HEAP_PTR: usize = 0;

/// Initialize the heap allocator
///
/// This must be called before any allocations are made
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_heap_init() {
    #[cfg(debug_assertions)]
    unsafe {
        debug_trace(b"[DEBUG] heap_init called\n")
    };

    unsafe {
        HEAP_PTR = 0;
    }

    #[cfg(debug_assertions)]
    unsafe {
        debug_trace(b"[DEBUG] heap_init complete\n")
    };
}

/// Reset the heap allocator
///
/// This resets the heap pointer to the beginning, effectively "freeing" all allocations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_heap_reset() {
    unsafe {
        HEAP_PTR = 0;
    }
}

/// Allocate memory from the heap
///
/// Returns a pointer to the allocated memory, or null if out of memory
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_alloc(size: usize) -> *mut u8 {
    if size == 0 {
        return ptr::null_mut();
    }

    // Align the size up to ALIGNMENT bytes
    let aligned_size = (size + ALIGNMENT - 1) & !(ALIGNMENT - 1);

    unsafe {
        let current_ptr = HEAP_PTR;
        let new_ptr = current_ptr + aligned_size;

        if new_ptr > HEAP_SIZE {
            return ptr::null_mut(); // Out of memory
        }

        HEAP_PTR = new_ptr;

        let result = HEAP.as_mut_ptr().add(current_ptr);

        // Zero the allocated memory
        ptr::write_bytes(result, 0, aligned_size);

        result
    }
}

/// Free memory (no-op for bump allocator)
///
/// This is a no-op since bump allocators don't support individual deallocation
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_free(_ptr: *mut u8) {
    // No-op for bump allocator
}
