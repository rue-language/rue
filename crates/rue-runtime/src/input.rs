//! Input functions for the Rue runtime
//!
//! This module provides functions for reading input from stdin.

/// Linux syscall numbers
const SYS_READ: i64 = 0;
const SYS_EXIT: i64 = 60;

/// File descriptors
const STDIN_FD: i32 = 0;

/// Exit codes
const EXIT_CODE_SYSCALL_FAILED: i32 = 253;

/// Raw syscall wrapper for 3 arguments
#[inline(always)]
unsafe fn syscall3(syscall_num: i64, arg1: i64, arg2: i64, arg3: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") syscall_num,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// Raw syscall wrapper for 1 argument
#[inline(always)]
unsafe fn syscall1(syscall_num: i64, arg1: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") syscall_num,
            in("rdi") arg1,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags)
        );
    }
    ret
}

/// Read input from stdin
///
/// This function reads input from stdin and returns the number of bytes read.
/// The input is null-terminated for convenience.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_input(buffer: *mut u8, buffer_size: usize) -> usize {
    if buffer.is_null() || buffer_size == 0 {
        return 0;
    }

    // Reserve one byte for null terminator
    let read_size = if buffer_size > 1 { buffer_size - 1 } else { 0 };

    if read_size == 0 {
        unsafe {
            *buffer = 0; // Null terminate
        }
        return 0;
    }

    let result = unsafe { syscall3(SYS_READ, STDIN_FD as i64, buffer as i64, read_size as i64) };

    if result < 0 {
        // Exit on syscall failure
        unsafe {
            syscall1(SYS_EXIT, EXIT_CODE_SYSCALL_FAILED as i64);
        }
        // This should never be reached
        return 0;
    }

    let bytes_read = result as usize;

    // Remove trailing newline if present - using manual pointer access
    let final_length = if bytes_read > 0 {
        let last_byte = unsafe { *buffer.add(bytes_read - 1) };
        if last_byte == b'\n' {
            bytes_read - 1
        } else {
            bytes_read
        }
    } else {
        0
    };

    // Null terminate
    unsafe {
        *buffer.add(final_length) = 0;
    }

    final_length
}
