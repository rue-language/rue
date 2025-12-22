//! AArch64 macOS syscall implementations.
//!
//! This module provides direct syscall wrappers for macOS on Apple Silicon.
//! No libc is used - we invoke the kernel directly via the `svc` instruction.
//!
//! # Platform Requirements
//!
//! This module only compiles on aarch64 macOS. Attempting to compile on other
//! platforms will result in a compile error.
//!
//! # Syscall Conventions
//!
//! On aarch64 macOS (Darwin):
//! - Syscall number goes in `x16`
//! - Arguments go in `x0`, `x1`, `x2`, `x3`, `x4`, `x5` (in order)
//! - Return value comes back in `x0`
//! - On error, the carry flag is set and `x0` contains the errno
//! - `x16` and `x17` may be clobbered
//!
//! # Darwin Syscall Numbers
//!
//! macOS uses the BSD syscall interface. Syscall numbers are defined in
//! `<sys/syscall.h>` and are different from Linux.

// Compile-time check for platform requirements
#[cfg(not(all(target_arch = "aarch64", target_os = "macos")))]
compile_error!("aarch64_macos module only supports aarch64 macOS");

use core::arch::asm;

/// macOS syscall number for exit (SYS_exit).
const SYS_EXIT: u64 = 1;

/// macOS syscall number for write (SYS_write).
const SYS_WRITE: u64 = 4;

/// macOS syscall number for mmap (SYS_mmap).
const SYS_MMAP: u64 = 197;

/// macOS syscall number for munmap (SYS_munmap).
const SYS_MUNMAP: u64 = 73;

// mmap protection flags (same as Linux)
const PROT_READ: u64 = 0x1;
const PROT_WRITE: u64 = 0x2;

// mmap flags
const MAP_PRIVATE: u64 = 0x0002;
const MAP_ANONYMOUS: u64 = 0x1000; // MAP_ANON on macOS

/// Standard error file descriptor.
const STDERR: u64 = 2;

/// Standard output file descriptor.
const STDOUT: u64 = 1;

/// Write bytes to a file descriptor.
///
/// This is a thin wrapper around the macOS `write(2)` syscall.
///
/// # Arguments
///
/// * `fd` - File descriptor to write to
/// * `buf` - Pointer to the buffer containing data to write
/// * `len` - Number of bytes to write
///
/// # Returns
///
/// On success, returns the number of bytes written (which may be less than `len`
/// if the write was interrupted or the pipe/socket buffer is full).
///
/// On error, returns a negative value representing `-errno`.
///
/// # Safety
///
/// The caller must ensure:
/// - `buf` points to a valid memory region of at least `len` bytes
/// - The memory region remains valid for the duration of the syscall
pub fn write(fd: u64, buf: *const u8, len: usize) -> i64 {
    let result: i64;
    let err_flag: u64;

    // SAFETY: We're making a syscall with the provided arguments.
    // The caller is responsible for ensuring buf/len are valid.
    unsafe {
        asm!(
            "svc #0x80",
            // Check carry flag for error
            "cset {err}, cs",
            inlateout("x16") SYS_WRITE => _,
            in("x0") fd,
            in("x1") buf,
            in("x2") len,
            lateout("x0") result,
            err = out(reg) err_flag,
            // x17 may be clobbered by the syscall
            out("x17") _,
        );
    }

    // If carry flag was set, result is errno (positive), negate it
    if err_flag != 0 { -result } else { result }
}

/// Write all bytes to a file descriptor, handling partial writes.
///
/// This function loops until all bytes are written or an unrecoverable error occurs.
/// It handles partial writes by advancing the buffer pointer and retrying.
///
/// # Arguments
///
/// * `fd` - File descriptor to write to
/// * `buf` - Slice of bytes to write
///
/// # Returns
///
/// * `Ok(())` - All bytes were successfully written
/// * `Err(errno)` - A syscall error occurred (errno is positive)
pub fn write_all(fd: u64, mut buf: &[u8]) -> Result<(), i64> {
    while !buf.is_empty() {
        let result = write(fd, buf.as_ptr(), buf.len());
        if result < 0 {
            // Syscall error - return the errno (as positive)
            return Err(-result);
        }
        if result == 0 {
            // This shouldn't happen for stderr, but handle it to avoid infinite loop.
            return Err(5); // EIO - I/O error
        }
        // Advance past the bytes we successfully wrote
        buf = &buf[result as usize..];
    }
    Ok(())
}

/// Write a message to stderr.
///
/// This is a best-effort write operation. If writing fails, the error is silently
/// ignored because we're typically about to exit anyway.
pub fn write_stderr(msg: &[u8]) {
    let _ = write_all(STDERR, msg);
}

/// Write a message to stdout.
///
/// This is a best-effort write operation similar to `write_stderr`.
pub fn write_stdout(msg: &[u8]) {
    let _ = write_all(STDOUT, msg);
}

/// Convert a signed 64-bit integer to a decimal string and write it to stdout.
///
/// Handles negative numbers by printing a leading '-'.
pub fn print_i64(value: i64) {
    // Buffer for decimal digits (max 20 digits for i64 + sign + newline)
    let mut buf = [0u8; 22];
    let mut pos = buf.len() - 1;

    // Always end with newline
    buf[pos] = b'\n';
    pos -= 1;

    let is_negative = value < 0;
    // Handle the absolute value (special case for i64::MIN)
    let mut abs_value = if value == i64::MIN {
        9223372036854775808u64
    } else if is_negative {
        (-value) as u64
    } else {
        value as u64
    };

    // Generate digits in reverse order
    if abs_value == 0 {
        buf[pos] = b'0';
        pos -= 1;
    } else {
        while abs_value > 0 {
            buf[pos] = b'0' + (abs_value % 10) as u8;
            abs_value /= 10;
            pos -= 1;
        }
    }

    // Add sign if negative
    if is_negative {
        buf[pos] = b'-';
        pos -= 1;
    }

    write_stdout(&buf[pos + 1..]);
}

/// Convert an unsigned 64-bit integer to a decimal string and write it to stdout.
pub fn print_u64(value: u64) {
    let mut buf = [0u8; 22];
    let mut pos = buf.len() - 1;

    buf[pos] = b'\n';
    pos -= 1;

    let mut val = value;

    if val == 0 {
        buf[pos] = b'0';
        pos -= 1;
    } else {
        while val > 0 {
            buf[pos] = b'0' + (val % 10) as u8;
            val /= 10;
            pos -= 1;
        }
    }

    write_stdout(&buf[pos + 1..]);
}

/// Print a boolean value to stdout ("true\n" or "false\n").
pub fn print_bool(value: bool) {
    if value {
        write_stdout(b"true\n");
    } else {
        write_stdout(b"false\n");
    }
}

/// Exit the process with the given status code.
///
/// This performs a direct syscall to `exit(2)` and never returns.
pub fn exit(status: i32) -> ! {
    // SAFETY: The exit syscall is always safe to call and never returns.
    unsafe {
        asm!(
            "svc #0x80",
            in("x16") SYS_EXIT,
            in("x0") status as u64,
            options(noreturn)
        );
    }
}

/// Copy bytes from src to dst without relying on compiler builtins.
///
/// This function performs a byte-by-byte copy to avoid LLVM lowering the copy
/// to a `memcpy` call, which would be undefined in our `#![no_std]` environment.
///
/// # Safety
///
/// The caller must ensure:
/// - `src` and `dst` do not overlap
/// - Both pointers are valid for `len` bytes
/// - Both pointers are properly aligned (byte alignment is always satisfied)
#[inline(always)]
unsafe fn copy_bytes(src: *const u8, dst: *mut u8, len: usize) {
    for i in 0..len {
        *dst.add(i) = *src.add(i);
    }
}

/// Allocate memory using mmap.
///
/// # Performance Note
///
/// This implementation uses mmap for every allocation, which incurs syscall
/// overhead and allocates at page granularity (typically 16KB on Apple Silicon).
/// This is simple but inefficient for small allocations. A future optimization
/// would be to implement a proper allocator (bump allocator or free-list) on top
/// of mmap. See ADR-019 Phase 1 for the planned improvement.
///
/// # Arguments
///
/// * `size` - Number of bytes to allocate
///
/// # Returns
///
/// Pointer to allocated memory, or null on failure.
///
/// # Safety
///
/// The caller must ensure the returned pointer is eventually freed with `dealloc`.
pub fn alloc(size: u64) -> *mut u8 {
    if size == 0 {
        return core::ptr::null_mut();
    }

    let result: u64;
    let err_flag: u64;

    // SAFETY: mmap syscall with anonymous mapping
    unsafe {
        asm!(
            "svc #0x80",
            "cset {err}, cs",
            inlateout("x16") SYS_MMAP => _,
            in("x0") 0u64,                            // addr: let kernel choose
            in("x1") size,                            // length
            in("x2") PROT_READ | PROT_WRITE,          // prot
            in("x3") MAP_PRIVATE | MAP_ANONYMOUS,     // flags
            in("x4") u64::MAX,                        // fd: -1 for anonymous
            in("x5") 0u64,                            // offset
            lateout("x0") result,
            err = out(reg) err_flag,
            out("x17") _,
        );
    }

    // On error, carry flag is set and result contains errno
    if err_flag != 0 {
        core::ptr::null_mut()
    } else {
        result as *mut u8
    }
}

/// Deallocate memory previously allocated with `alloc`.
///
/// # Arguments
///
/// * `ptr` - Pointer to memory to deallocate
/// * `size` - Size of the allocation (must match the original allocation)
///
/// # Safety
///
/// The caller must ensure:
/// - `ptr` was returned by a previous call to `alloc`
/// - `size` matches the size passed to `alloc`
/// - The memory has not already been deallocated
pub fn dealloc(ptr: *mut u8, size: u64) {
    if ptr.is_null() || size == 0 {
        return;
    }

    // SAFETY: munmap syscall
    unsafe {
        asm!(
            "svc #0x80",
            inlateout("x16") SYS_MUNMAP => _,
            in("x0") ptr,
            in("x1") size,
            lateout("x0") _,
            out("x17") _,
        );
    }
    // We ignore errors from munmap - there's not much we can do about them
}

/// Reallocate memory to a new size.
///
/// # Arguments
///
/// * `ptr` - Pointer to existing allocation (or null for new allocation)
/// * `old_size` - Current size of allocation
/// * `new_size` - Desired new size
///
/// # Returns
///
/// Pointer to reallocated memory, or null on failure.
/// On failure, the original allocation is still valid.
///
/// # Safety
///
/// The caller must ensure:
/// - `ptr` was returned by a previous call to `alloc` or `realloc`, or is null
/// - `old_size` matches the current allocation size
pub fn realloc(ptr: *mut u8, old_size: u64, new_size: u64) -> *mut u8 {
    if new_size == 0 {
        dealloc(ptr, old_size);
        return core::ptr::null_mut();
    }

    if ptr.is_null() || old_size == 0 {
        return alloc(new_size);
    }

    // Simple implementation: allocate new, copy, free old
    let new_ptr = alloc(new_size);
    if new_ptr.is_null() {
        return core::ptr::null_mut();
    }

    // Copy the old data
    let copy_size = if old_size < new_size {
        old_size
    } else {
        new_size
    };
    // SAFETY: Both pointers are valid for their respective sizes, and they
    // don't overlap since new_ptr is a fresh allocation.
    unsafe {
        copy_bytes(ptr, new_ptr, copy_size as usize);
    }

    dealloc(ptr, old_size);
    new_ptr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_to_stderr() {
        let msg = b"test message\n";
        let result = write(STDERR, msg.as_ptr(), msg.len());
        assert_eq!(result, msg.len() as i64);
    }

    #[test]
    fn test_write_empty() {
        let result = write(STDERR, core::ptr::null(), 0);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_write_invalid_fd() {
        let msg = b"test";
        let result = write(999, msg.as_ptr(), msg.len());
        // Should return negative errno for bad file descriptor
        assert!(result < 0);
        assert_eq!(-result, 9); // EBADF
    }

    #[test]
    fn test_write_all_success() {
        let msg = b"write_all test\n";
        let result = write_all(STDERR, msg);
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_all_empty() {
        let result = write_all(STDERR, b"");
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_all_invalid_fd() {
        let msg = b"test";
        let result = write_all(999, msg);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 9); // EBADF
    }

    #[test]
    fn test_syscall_constants() {
        // Verify our syscall numbers match macOS
        assert_eq!(SYS_EXIT, 1);
        assert_eq!(SYS_WRITE, 4);
        assert_eq!(STDERR, 2);
        assert_eq!(STDOUT, 1);
    }
}
