//! Input/Output functions.
//!
//! This module provides I/O operations for Rue programs:
//! - `__rue_read_line` - Read a line from standard input

use crate::heap;
use crate::platform;
use crate::string::{STRING_MIN_CAPACITY, StringResult};

/// Initial buffer size for reading lines.
/// This is a reasonable size for most interactive input.
const READ_LINE_INITIAL_CAPACITY: u64 = 128;

/// Read a line from standard input.
///
/// Reads bytes from stdin (file descriptor 0) until a newline character (`\n`)
/// is encountered or EOF is reached. Returns the line as a String (excluding
/// the trailing newline).
///
/// # Returns
///
/// Returns the string data via sret convention. Writes to `out`:
/// - ptr: Pointer to the string data (heap-allocated)
/// - len: Length of the string in bytes (excluding newline)
/// - cap: Capacity of the allocated buffer
///
/// # Panics
///
/// - If EOF is reached with no data read: panics with "unexpected end of input"
/// - If a read error occurs: panics with "input error"
///
/// # ABI (sret convention)
///
/// ```text
/// extern "C" fn __rue_read_line(out: *mut StringResult)
/// ```
///
/// Caller allocates space for the return value and passes pointer.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_read_line(out: *mut StringResult) {
    read_line_impl(out);
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_read_line(out: *mut StringResult) {
    read_line_impl(out);
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_read_line(out: *mut StringResult) {
    read_line_impl(out);
}

/// Implementation of read_line shared across platforms.
///
/// This function reads from stdin byte-by-byte until:
/// - A newline character is found (returns line without the newline)
/// - EOF is reached with some data (returns partial line)
/// - EOF is reached with no data (panics)
/// - A read error occurs (panics)
#[inline]
fn read_line_impl(out: *mut StringResult) {
    // Allocate initial buffer
    let mut cap = READ_LINE_INITIAL_CAPACITY;
    let mut ptr = heap::alloc(cap, 1);
    if ptr.is_null() {
        // Allocation failed - panic
        let mut msg = [0u8; 21];
        msg[0] = b'e';
        msg[1] = b'r';
        msg[2] = b'r';
        msg[3] = b'o';
        msg[4] = b'r';
        msg[5] = b':';
        msg[6] = b' ';
        msg[7] = b'o';
        msg[8] = b'u';
        msg[9] = b't';
        msg[10] = b' ';
        msg[11] = b'o';
        msg[12] = b'f';
        msg[13] = b' ';
        msg[14] = b'm';
        msg[15] = b'e';
        msg[16] = b'm';
        msg[17] = b'o';
        msg[18] = b'r';
        msg[19] = b'y';
        msg[20] = b'\n';
        platform::write_stderr(&msg);
        platform::exit(101);
    }

    let mut len: u64 = 0;
    let mut byte_buf = [0u8; 1];

    loop {
        // Read one byte at a time
        let result = platform::read(platform::STDIN, byte_buf.as_mut_ptr(), 1);

        if result < 0 {
            // Read error - free buffer and panic
            heap::free(ptr, cap, 1);
            let mut msg = [0u8; 19];
            msg[0] = b'e';
            msg[1] = b'r';
            msg[2] = b'r';
            msg[3] = b'o';
            msg[4] = b'r';
            msg[5] = b':';
            msg[6] = b' ';
            msg[7] = b'i';
            msg[8] = b'n';
            msg[9] = b'p';
            msg[10] = b'u';
            msg[11] = b't';
            msg[12] = b' ';
            msg[13] = b'e';
            msg[14] = b'r';
            msg[15] = b'r';
            msg[16] = b'o';
            msg[17] = b'r';
            msg[18] = b'\n';
            platform::write_stderr(&msg);
            platform::exit(101);
        }

        if result == 0 {
            // EOF reached
            if len == 0 {
                // EOF with no data - free buffer and panic
                heap::free(ptr, cap, 1);
                // Build error message in buffer (like print_i64 does)
                let mut msg = [0u8; 31];
                msg[0] = b'e';
                msg[1] = b'r';
                msg[2] = b'r';
                msg[3] = b'o';
                msg[4] = b'r';
                msg[5] = b':';
                msg[6] = b' ';
                msg[7] = b'u';
                msg[8] = b'n';
                msg[9] = b'e';
                msg[10] = b'x';
                msg[11] = b'p';
                msg[12] = b'e';
                msg[13] = b'c';
                msg[14] = b't';
                msg[15] = b'e';
                msg[16] = b'd';
                msg[17] = b' ';
                msg[18] = b'e';
                msg[19] = b'n';
                msg[20] = b'd';
                msg[21] = b' ';
                msg[22] = b'o';
                msg[23] = b'f';
                msg[24] = b' ';
                msg[25] = b'i';
                msg[26] = b'n';
                msg[27] = b'p';
                msg[28] = b'u';
                msg[29] = b't';
                msg[30] = b'\n';
                platform::write_stderr(&msg);
                platform::exit(101);
            }
            // EOF with data - return partial line
            break;
        }

        // Got a byte
        let byte = byte_buf[0];

        // Check for newline - line is complete (don't include the newline)
        if byte == b'\n' {
            break;
        }

        // Need to store this byte - ensure we have capacity
        if len >= cap {
            // Grow the buffer (2x strategy)
            let new_cap = cap.saturating_mul(2).max(STRING_MIN_CAPACITY);
            let new_ptr = heap::realloc(ptr, cap, new_cap, 1);
            if new_ptr.is_null() {
                // Realloc failed - free old buffer and panic
                heap::free(ptr, cap, 1);
                platform::write_stderr(b"error: out of memory\n");
                platform::exit(101);
            }
            ptr = new_ptr;
            cap = new_cap;
        }

        // Store the byte
        // SAFETY: Writing is safe because:
        // - We checked `len < cap` above and grew the buffer if needed
        // - `ptr` points to valid heap memory from our allocation
        // - u8 has no alignment requirements
        unsafe {
            *ptr.add(len as usize) = byte;
        }
        len += 1;
    }

    // Return the string
    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = ptr;
        (*out).len = len;
        (*out).cap = cap;
    }
}
