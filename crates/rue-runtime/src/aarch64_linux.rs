//! AArch64 Linux syscall implementations.
//!
//! This module provides direct syscall wrappers for Linux on AArch64.
//! No libc is used - we invoke the kernel directly via the `svc` instruction.
//!
//! # Platform Requirements
//!
//! This module only compiles on aarch64 Linux. Attempting to compile on other
//! platforms will result in a compile error.
//!
//! # Syscall Conventions
//!
//! On aarch64 Linux:
//! - Syscall number goes in `x8`
//! - Arguments go in `x0`, `x1`, `x2`, `x3`, `x4`, `x5` (in order)
//! - Return value comes back in `x0`
//! - On error, `x0` contains a negative value representing `-errno`
//!
//! # Linux Syscall Numbers
//!
//! AArch64 Linux uses the "new" syscall interface. Syscall numbers are defined in
//! `/usr/include/asm-generic/unistd.h` and differ from x86_64 Linux.

// Compile-time check for platform requirements
#[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
compile_error!("aarch64_linux module only supports aarch64 Linux");

use core::arch::asm;

/// Linux aarch64 syscall number for exit (from asm-generic/unistd.h).
const SYS_EXIT: u64 = 93;

/// Linux aarch64 syscall number for write (from asm-generic/unistd.h).
const SYS_WRITE: u64 = 64;

/// Standard error file descriptor.
const STDERR: u64 = 2;

/// Standard output file descriptor.
const STDOUT: u64 = 1;

/// Write bytes to a file descriptor.
///
/// This is a thin wrapper around the Linux `write(2)` syscall.
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

    // SAFETY: We're making a syscall with the provided arguments.
    // The caller is responsible for ensuring buf/len are valid.
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_WRITE,
            inlateout("x0") fd as i64 => result,
            in("x1") buf,
            in("x2") len,
        );
    }

    result
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
            "svc #0",
            in("x8") SYS_EXIT,
            in("x0") status as u64,
            options(noreturn)
        );
    }
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
        // Verify our syscall numbers match Linux aarch64
        assert_eq!(SYS_EXIT, 93);
        assert_eq!(SYS_WRITE, 64);
        assert_eq!(STDERR, 2);
        assert_eq!(STDOUT, 1);
    }
}
