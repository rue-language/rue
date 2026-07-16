//! Input/Output functions.
//!
//! This module provides I/O operations for Rue programs:
//! - `__rue_read_line` - Read a line from standard input
//! - `__rue_print` - Write a String's raw bytes to stdout (no newline)
//! - `__rue_println` - Write a String's raw bytes to stdout, then a newline

use crate::heap;
use crate::platform;
use crate::string::STRING_MIN_CAPACITY;
pub use rue_runtime_abi::OptionStrBufResult;

// =============================================================================
// String output (RUE-1: print / println)
// =============================================================================
//
// `print(s)` and `println(s)` write the raw bytes of a `String` to stdout via
// the same `write(1, ptr, len)` syscall path `@dbg` uses, but they emit the
// string's actual bytes rather than a debug format. Unlike `@dbg`, `print`
// adds nothing and `println` adds exactly one `\n`.
//
// The String argument is passed by borrow, flattened into three ABI slots
// (ptr, len, cap) exactly like the borrowed operands of `s1 + s2` and the
// arguments to `s.contains(needle)`. `cap` is part of the ABI but unused here:
// output only needs the pointer and length, and the borrow means neither
// function takes ownership, so the caller's String stays valid and is dropped
// by its owner as usual.

crate::define_runtime_implementation! {
    /// Write a String's raw bytes to stdout with no trailing newline.
    ///
    /// Called by the `print(s: String)` builtin free function (RUE-1). Writes
    /// exactly `len` bytes starting at `ptr` to file descriptor 1.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_print(ptr: *const u8, len: u64, cap: u64)
    /// ```
    ///
    /// - `ptr` / `len` / `cap` are the flattened fields of a borrowed String
    ///   (first three argument registers). `cap` is unused.
    ///
    /// # Safety
    ///
    /// When `len > 0`, `ptr` must be non-null and point to `len` valid,
    /// initialized bytes that remain valid for the duration of the call. It
    /// may be null when `len == 0`. The borrow ABI guarantees the live-buffer
    /// requirement: the String is not consumed and outlives the call.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_print(ptr: *const u8, len: u64, _cap: u64) {
        // SAFETY: `ptr` points to `len` valid bytes owned by the caller's
        // String (borrowed, not consumed), and we only read from the slice.
        if len > 0 {
            // SAFETY: the caller guarantees a non-null pointer valid for `len`
            // initialized bytes when the length is positive.
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
            platform::write_stdout(bytes);
        }
    }
}

crate::define_runtime_implementation! {
    /// Write a shared two-word `str` view to stdout.
    pub unsafe extern "C" fn __rue_str_print(ptr: *const u8, len: u64) {
        if len > 0 {
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
            platform::write_stdout(bytes);
        }
    }
}

crate::define_runtime_implementation! {
    /// Write a String's raw bytes to stdout followed by a single newline.
    ///
    /// Called by the `println(s: String)` builtin free function (RUE-1). Writes
    /// exactly `len` bytes starting at `ptr` to file descriptor 1, then a lone
    /// `\n`.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_println(ptr: *const u8, len: u64, cap: u64)
    /// ```
    ///
    /// - `ptr` / `len` / `cap` are the flattened fields of a borrowed String
    ///   (first three argument registers). `cap` is unused.
    ///
    /// # Safety
    ///
    /// See [`__rue_print`].
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_println(ptr: *const u8, len: u64, _cap: u64) {
        // SAFETY: see `__rue_print` — `ptr` is valid for `len` borrowed bytes.
        if len > 0 {
            // SAFETY: see `__rue_print`.
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
            platform::write_stdout(bytes);
        }
        // Byte-array literal (not `b"\n"`) to avoid a macOS linker quirk seen
        // with `b"..."` in `__rue_dbg_str`.
        let newline = [b'\n'];
        platform::write_stdout(&newline);
    }
}

crate::define_runtime_implementation! {
    /// Write a shared two-word `str` view followed by a newline.
    pub unsafe extern "C" fn __rue_str_println(ptr: *const u8, len: u64) {
        if len > 0 {
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
            platform::write_stdout(bytes);
        }
        platform::write_stdout(b"\n");
    }
}

/// Initial buffer size for reading lines.
/// This is a reasonable size for most interactive input.
const READ_LINE_INITIAL_CAPACITY: u64 = 128;

/// Read a line from standard input, returning `Option(StrBuf)`.
///
/// Reads bytes from stdin (file descriptor 0) until a newline character (`\n`)
/// is encountered or EOF is reached. Yields the line (excluding the trailing
/// newline) as `Some(StrBuf)`, or `None` at end-of-input (RUE-6, ADR-0038).
///
/// # Returns
///
/// Writes the whole `Option(StrBuf)` to `out` via sret:
/// - On success (line read, or partial line at EOF): `disc = some_disc` and the
///   StrBuf fields `ptr`/`len`/`cap` are populated.
/// - At EOF with no data read: `disc = none_disc` and the payload slots are
///   zeroed (a `None` never drops its payload, so the zeros are inert).
///
/// `some_disc`/`none_disc` are the caller's `Some`/`None` variant indices,
/// passed in so the runtime need not know the compiler's discriminant
/// assignment.
///
/// # Panics
///
/// - If a read error occurs: panics with "input error"
/// - If memory allocation fails: panics with "out of memory"
///
/// EOF is **not** a panic — it is reported as `None`.
///
/// # ABI (sret convention)
///
/// ```text
/// extern "C" fn __rue_read_line(out: *mut OptionStrBufResult, some_disc: u64, none_disc: u64)
/// ```
///
/// Caller allocates space for the return value and passes pointer.
///
/// # Safety
///
/// `out` must be valid, aligned, writable storage for one
/// [`OptionStrBufResult`] and must remain exclusively accessible for the call.
#[allow(non_snake_case)]
pub unsafe fn __rue_read_line(out: *mut OptionStrBufResult, some_disc: u64, none_disc: u64) {
    read_line_impl(out, some_disc, none_disc);
}

/// Implementation of read_line shared across platforms.
///
/// This function reads from stdin byte-by-byte until:
/// - A newline character is found (returns `Some` line without the newline)
/// - EOF is reached with some data (returns `Some` partial line)
/// - EOF is reached with no data (returns `None`)
/// - A read error occurs (panics)
#[inline]
fn read_line_impl(out: *mut OptionStrBufResult, some_disc: u64, none_disc: u64) {
    // Allocate initial buffer
    let mut cap = READ_LINE_INITIAL_CAPACITY;
    let mut ptr = heap::alloc(cap, 1);
    if ptr.is_null() {
        crate::error::allocation_failure();
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
                // EOF with no data: this is not an error (RUE-6, ADR-0038).
                // Free the empty buffer and report `None` so a read-until-EOF
                // loop can terminate cleanly instead of trapping.
                heap::free(ptr, cap, 1);
                // SAFETY: `out` points to caller-allocated space for the
                // Option(StrBuf) result (4 slots). We write `None` and zero
                // the payload; a `None` never has its payload dropped, so the
                // zeroed String slots are inert.
                unsafe {
                    (*out).disc = none_disc;
                    (*out).ptr = core::ptr::null_mut();
                    (*out).len = 0;
                    (*out).cap = 0;
                }
                return;
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
            let Some(new_cap) = cap.checked_mul(2) else {
                crate::error::allocation_failure();
            };
            let new_cap = new_cap.max(STRING_MIN_CAPACITY);
            let new_ptr = heap::realloc(ptr, cap, new_cap, 1);
            if new_ptr.is_null() {
                // Keep the old allocation intact until the canonical trap.
                crate::error::allocation_failure();
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

    // Return `Some(string)`.
    // SAFETY: `out` is caller-provided, aligned, exclusive result storage.
    unsafe {
        (*out).disc = some_disc;
        (*out).ptr = ptr;
        (*out).len = len;
        (*out).cap = cap;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::process::Command;

    #[test]
    fn read_line_allocation_failure_uses_canonical_trap() {
        const CHILD_ENV: &str = "RUE_READ_LINE_OOM_CHILD";
        if self::std::env::var_os(CHILD_ENV).is_some() {
            crate::heap::fail_allocations_after_for_test(0);
            let mut out = super::OptionStrBufResult {
                disc: 0,
                ptr: core::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            super::read_line_impl(&mut out, 1, 0);
            unreachable!();
        }

        let output = Command::new(self::std::env::current_exe().expect("current test binary"))
            .args([
                "--exact",
                "io::tests::read_line_allocation_failure_uses_canonical_trap",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("spawn allocation-failure child");

        assert_eq!(output.status.code(), Some(101));
        assert_eq!(output.stderr, b"panic: out of memory\n");
    }
}
