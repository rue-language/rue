//! Formatting functions for the Rue runtime
//!
//! This module provides functions for formatting and printing different data types.

use crate::buffered_io::BufferedStdout;
use crate::conversions::__rue_itoa;

/// Print an i64 value followed by a newline
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_println_i64(value: i64) {
    let mut buffer = [0u8; 21]; // Enough for i64::MIN + null terminator
    let len = unsafe { __rue_itoa(value, buffer.as_mut_ptr()) };

    // Write the number
    let _ = BufferedStdout::write_bytes(&buffer[..len]);

    // Write newline
    let _ = BufferedStdout::write_byte(b'\n');
}

/// Print an i32 value followed by a newline
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_println_i32(value: i32) {
    unsafe { __rue_println_i64(value as i64) };
}

/// Print a boolean value followed by a newline
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_println_bool(value: bool) {
    let text: &[u8] = if value { b"true" } else { b"false" };
    let _ = BufferedStdout::write_bytes(text);
    let _ = BufferedStdout::write_byte(b'\n');
}

/// Print unit value (empty line)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_println_unit() {
    let _ = BufferedStdout::write_byte(b'\n');
}
