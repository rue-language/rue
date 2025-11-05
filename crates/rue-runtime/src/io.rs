//! I/O operation functions
//!
//! Provides println and input functions for Rue programs.
//! Uses buffered stdout for efficient output.

use crate::abi::*;
use crate::buffered_io::BufferedStdout;
use crate::conversion::{__rue_atoi, __rue_itoa};
use crate::syscall::sys_read;

/// Print an i64 value followed by a newline
///
/// Uses itoa for conversion and buffered stdout for output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_println_i64(value: i64) {
    let mut buf = [0u8; ITOA_BUFFER_SIZE];
    let len = unsafe { __rue_itoa(value, buf.as_mut_ptr()) };

    // Write the number
    let _ = BufferedStdout::write_bytes(&buf[..len]);

    // Write newline
    let _ = BufferedStdout::write_byte(b'\n');
}

/// Print an i32 value followed by a newline
///
/// Delegates to println_i64 after casting.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_println_i32(value: i32) {
    unsafe { __rue_println_i64(value as i64) }
}

/// Print a boolean value followed by a newline
///
/// Prints "true" or "false".
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_println_bool(value: bool) {
    let s: &[u8] = if value { b"true" } else { b"false" };
    let _ = BufferedStdout::write_bytes(s);
    let _ = BufferedStdout::write_byte(b'\n');
}

/// Print a unit value "()" followed by a newline
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_println_unit() {
    let _ = BufferedStdout::write_bytes(b"()");
    let _ = BufferedStdout::write_byte(b'\n');
}

/// Read an integer from stdin
///
/// Reads a line from stdin, parses it as an integer, and returns the value.
/// Returns 0 if parsing fails or on read error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_input() -> i64 {
    let mut buf = [0u8; INPUT_BUFFER_SIZE];

    // Read from stdin
    let result = unsafe { sys_read(STDIN_FD, buf.as_mut_ptr(), buf.len()) };

    match result {
        Ok(bytes_read) if bytes_read > 0 => {
            // Parse the input
            unsafe { __rue_atoi(buf.as_ptr(), bytes_read) }
        }
        _ => 0, // Error or EOF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests can't easily test the actual I/O without mocking,
    // but we can test that they compile and don't crash with valid inputs

    #[test]
    fn test_println_i64_compiles() {
        // Just verify this compiles and links
        // Actual output would go to stdout in test environment
        unsafe {
            __rue_println_i64(42);
        }
    }

    #[test]
    fn test_println_i32_compiles() {
        unsafe {
            __rue_println_i32(42);
        }
    }

    #[test]
    fn test_println_bool_compiles() {
        unsafe {
            __rue_println_bool(true);
            __rue_println_bool(false);
        }
    }

    #[test]
    fn test_println_unit_compiles() {
        unsafe {
            __rue_println_unit();
        }
    }
}
