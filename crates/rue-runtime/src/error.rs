//! Runtime error handlers.
//!
//! These functions are called by generated code when runtime errors occur:
//! - Division by zero
//! - Integer overflow
//! - Integer cast overflow
//! - Array bounds check failure
//!
//! All errors exit with code 101 after writing an error message to stderr.

use crate::platform;

crate::define_for_all_platforms! {
    /// Runtime error: division by zero.
    ///
    /// Called when a division or modulo operation has a zero divisor. This is
    /// typically triggered by a conditional jump inserted by the compiler before
    /// division operations.
    ///
    /// # Behavior
    ///
    /// 1. Writes `"error: division by zero\n"` to stderr (best-effort)
    /// 2. Exits with code 101
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_div_by_zero() -> !
    /// ```
    ///
    /// No arguments. Never returns.
    pub extern "C" fn __rue_div_by_zero() -> ! {
        // Build error message byte-by-byte to avoid macOS linker bug with byte strings
        let mut msg = [0u8; 24];
        msg[0] = b'e';
        msg[1] = b'r';
        msg[2] = b'r';
        msg[3] = b'o';
        msg[4] = b'r';
        msg[5] = b':';
        msg[6] = b' ';
        msg[7] = b'd';
        msg[8] = b'i';
        msg[9] = b'v';
        msg[10] = b'i';
        msg[11] = b's';
        msg[12] = b'i';
        msg[13] = b'o';
        msg[14] = b'n';
        msg[15] = b' ';
        msg[16] = b'b';
        msg[17] = b'y';
        msg[18] = b' ';
        msg[19] = b'z';
        msg[20] = b'e';
        msg[21] = b'r';
        msg[22] = b'o';
        msg[23] = b'\n';
        platform::write_stderr(&msg);
        platform::exit(101)
    }
}

crate::define_for_all_platforms! {
    /// Runtime error: integer overflow.
    ///
    /// Called when an arithmetic operation overflows. This is typically triggered
    /// by a conditional jump inserted by the compiler after arithmetic operations
    /// that check the overflow flag.
    ///
    /// # Behavior
    ///
    /// 1. Writes `"error: integer overflow\n"` to stderr (best-effort)
    /// 2. Exits with code 101
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_overflow() -> !
    /// ```
    ///
    /// No arguments. Never returns.
    pub extern "C" fn __rue_overflow() -> ! {
        // Build error message byte-by-byte to avoid macOS linker bug with byte strings
        let mut msg = [0u8; 24];
        msg[0] = b'e';
        msg[1] = b'r';
        msg[2] = b'r';
        msg[3] = b'o';
        msg[4] = b'r';
        msg[5] = b':';
        msg[6] = b' ';
        msg[7] = b'i';
        msg[8] = b'n';
        msg[9] = b't';
        msg[10] = b'e';
        msg[11] = b'g';
        msg[12] = b'e';
        msg[13] = b'r';
        msg[14] = b' ';
        msg[15] = b'o';
        msg[16] = b'v';
        msg[17] = b'e';
        msg[18] = b'r';
        msg[19] = b'f';
        msg[20] = b'l';
        msg[21] = b'o';
        msg[22] = b'w';
        msg[23] = b'\n';
        platform::write_stderr(&msg);
        platform::exit(101)
    }
}

crate::define_for_all_platforms! {
    /// Runtime error: integer cast overflow.
    ///
    /// Called when `@intCast` would produce a value that cannot be represented
    /// in the target type. For example, casting `-1i32` to `u8` or `256u32` to `u8`.
    ///
    /// # Behavior
    ///
    /// 1. Writes `"error: integer cast overflow\n"` to stderr (best-effort)
    /// 2. Exits with code 101
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_intcast_overflow() -> !
    /// ```
    ///
    /// No arguments. Never returns.
    pub extern "C" fn __rue_intcast_overflow() -> ! {
        // Build error message byte-by-byte to avoid macOS linker bug with byte strings
        let mut msg = [0u8; 29];
        msg[0] = b'e';
        msg[1] = b'r';
        msg[2] = b'r';
        msg[3] = b'o';
        msg[4] = b'r';
        msg[5] = b':';
        msg[6] = b' ';
        msg[7] = b'i';
        msg[8] = b'n';
        msg[9] = b't';
        msg[10] = b'e';
        msg[11] = b'g';
        msg[12] = b'e';
        msg[13] = b'r';
        msg[14] = b' ';
        msg[15] = b'c';
        msg[16] = b'a';
        msg[17] = b's';
        msg[18] = b't';
        msg[19] = b' ';
        msg[20] = b'o';
        msg[21] = b'v';
        msg[22] = b'e';
        msg[23] = b'r';
        msg[24] = b'f';
        msg[25] = b'l';
        msg[26] = b'o';
        msg[27] = b'w';
        msg[28] = b'\n';
        platform::write_stderr(&msg);
        platform::exit(101)
    }
}

crate::define_for_all_platforms! {
    /// Runtime error: index out of bounds.
    ///
    /// Called when an array index operation accesses an element outside the
    /// valid range [0, length). The compiler inserts a bounds check before
    /// each array access that compares the index against the array length.
    ///
    /// # Behavior
    ///
    /// 1. Writes `"error: index out of bounds\n"` to stderr (best-effort)
    /// 2. Exits with code 101
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_bounds_check() -> !
    /// ```
    ///
    /// No arguments. Never returns.
    ///
    /// # Design Notes
    ///
    /// Unlike some languages that include the index and length in the error
    /// message, we keep this simple for minimal runtime size. The compiler
    /// already performs compile-time checks for constant indices, so this
    /// handler is only reached for dynamic indices that fail at runtime.
    pub extern "C" fn __rue_bounds_check() -> ! {
        // Build error message byte-by-byte to avoid macOS linker bug with byte strings
        let mut msg = [0u8; 27];
        msg[0] = b'e';
        msg[1] = b'r';
        msg[2] = b'r';
        msg[3] = b'o';
        msg[4] = b'r';
        msg[5] = b':';
        msg[6] = b' ';
        msg[7] = b'i';
        msg[8] = b'n';
        msg[9] = b'd';
        msg[10] = b'e';
        msg[11] = b'x';
        msg[12] = b' ';
        msg[13] = b'o';
        msg[14] = b'u';
        msg[15] = b't';
        msg[16] = b' ';
        msg[17] = b'o';
        msg[18] = b'f';
        msg[19] = b' ';
        msg[20] = b'b';
        msg[21] = b'o';
        msg[22] = b'u';
        msg[23] = b'n';
        msg[24] = b'd';
        msg[25] = b's';
        msg[26] = b'\n';
        platform::write_stderr(&msg);
        platform::exit(101)
    }
}

crate::define_for_all_platforms! {
    /// Runtime error: invalid UTF-8 during decoding.
    ///
    /// Called when `s.chars()` iteration (RUE-220, ADR-0035) decodes a byte
    /// sequence that is not well-formed UTF-8. Rue's `String` is a byte string
    /// that may hold arbitrary bytes; the "trap, don't corrupt" discipline
    /// applies at the decode boundary, so interpreting invalid bytes as
    /// Unicode scalars traps loudly; the lossy `.chars_lossy()` variant
    /// substitutes `U+FFFD` instead of trapping.
    ///
    /// # Behavior
    ///
    /// 1. Writes `"error: invalid UTF-8\n"` to stderr (best-effort)
    /// 2. Exits with code 101
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_invalid_utf8() -> !
    /// ```
    ///
    /// No arguments. Never returns.
    pub extern "C" fn __rue_invalid_utf8() -> ! {
        // Build error message byte-by-byte to avoid macOS linker bug with byte strings
        let mut msg = [0u8; 21];
        msg[0] = b'e';
        msg[1] = b'r';
        msg[2] = b'r';
        msg[3] = b'o';
        msg[4] = b'r';
        msg[5] = b':';
        msg[6] = b' ';
        msg[7] = b'i';
        msg[8] = b'n';
        msg[9] = b'v';
        msg[10] = b'a';
        msg[11] = b'l';
        msg[12] = b'i';
        msg[13] = b'd';
        msg[14] = b' ';
        msg[15] = b'U';
        msg[16] = b'T';
        msg[17] = b'F';
        msg[18] = b'-';
        msg[19] = b'8';
        msg[20] = b'\n';
        platform::write_stderr(&msg);
        platform::exit(101)
    }
}

crate::define_for_all_platforms! {
    /// Runtime error: explicit `@panic(msg)` with a message.
    ///
    /// Called by code generated for the `@panic("...")` intrinsic. Writes
    /// `"panic: "`, the user-supplied message bytes, and a trailing newline to
    /// stderr, then exits with code 101 (the same abort path as the other
    /// runtime traps: division by zero, overflow, bounds, cast overflow).
    ///
    /// # Behavior
    ///
    /// 1. Writes `"panic: {msg}\n"` to stderr (best-effort)
    /// 2. Exits with code 101
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_panic(ptr: *const u8, len: u64) -> !
    /// ```
    ///
    /// - `ptr` / `len` are the message string's fat-pointer fields (the `cap`
    ///   field is not needed and is not passed).
    /// - Never returns.
    ///
    /// # Safety
    ///
    /// The caller (Rue-generated code) guarantees `ptr` points to `len` valid,
    /// initialized, byte-aligned bytes that stay valid for the call.
    pub extern "C" fn __rue_panic(ptr: *const u8, len: u64) -> ! {
        // "panic: " built byte-by-byte to avoid the macOS byte-string linker bug.
        let mut prefix = [0u8; 7];
        prefix[0] = b'p';
        prefix[1] = b'a';
        prefix[2] = b'n';
        prefix[3] = b'i';
        prefix[4] = b'c';
        prefix[5] = b':';
        prefix[6] = b' ';
        platform::write_stderr(&prefix);
        // SAFETY: the caller guarantees `ptr`/`len` describe a valid byte range.
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
        platform::write_stderr(bytes);
        let newline = [b'\n'];
        platform::write_stderr(&newline);
        platform::exit(101)
    }
}

crate::define_for_all_platforms! {
    /// Runtime error: explicit `@panic()` with no message.
    ///
    /// Called by code generated for the message-less `@panic()` intrinsic.
    ///
    /// # Behavior
    ///
    /// 1. Writes `"panic\n"` to stderr (best-effort)
    /// 2. Exits with code 101
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_panic_no_msg() -> !
    /// ```
    ///
    /// No arguments. Never returns.
    pub extern "C" fn __rue_panic_no_msg() -> ! {
        let mut msg = [0u8; 6];
        msg[0] = b'p';
        msg[1] = b'a';
        msg[2] = b'n';
        msg[3] = b'i';
        msg[4] = b'c';
        msg[5] = b'\n';
        platform::write_stderr(&msg);
        platform::exit(101)
    }
}

crate::define_for_all_platforms! {
    /// Runtime error: failed `@assert(cond)` (no message).
    ///
    /// Called by code generated for `@assert(cond)` when `cond` is false.
    /// The message-carrying form `@assert(cond, "msg")` routes to
    /// [`__rue_panic`] instead so the user's text is shown.
    ///
    /// # Behavior
    ///
    /// 1. Writes `"assertion failed\n"` to stderr (best-effort)
    /// 2. Exits with code 101
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_assert_failed() -> !
    /// ```
    ///
    /// No arguments. Never returns.
    pub extern "C" fn __rue_assert_failed() -> ! {
        let mut msg = [0u8; 17];
        msg[0] = b'a';
        msg[1] = b's';
        msg[2] = b's';
        msg[3] = b'e';
        msg[4] = b'r';
        msg[5] = b't';
        msg[6] = b'i';
        msg[7] = b'o';
        msg[8] = b'n';
        msg[9] = b' ';
        msg[10] = b'f';
        msg[11] = b'a';
        msg[12] = b'i';
        msg[13] = b'l';
        msg[14] = b'e';
        msg[15] = b'd';
        msg[16] = b'\n';
        platform::write_stderr(&msg);
        platform::exit(101)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_error_message_lengths() {
        // Verify message lengths match the array sizes used in the runtime functions
        assert_eq!(b"error: division by zero\n".len(), 24);
        assert_eq!(b"error: integer overflow\n".len(), 24);
        assert_eq!(b"error: integer cast overflow\n".len(), 29);
        assert_eq!(b"error: index out of bounds\n".len(), 27);
        assert_eq!(b"panic: ".len(), 7);
        assert_eq!(b"panic\n".len(), 6);
        assert_eq!(b"assertion failed\n".len(), 17);
    }

    #[test]
    fn test_error_messages_are_valid_utf8() {
        // Error messages should be valid UTF-8 for proper display
        let div_msg = b"error: division by zero\n";
        let overflow_msg = b"error: integer overflow\n";
        let intcast_msg = b"error: integer cast overflow\n";
        let bounds_msg = b"error: index out of bounds\n";

        assert!(core::str::from_utf8(div_msg).is_ok());
        assert!(core::str::from_utf8(overflow_msg).is_ok());
        assert!(core::str::from_utf8(intcast_msg).is_ok());
        assert!(core::str::from_utf8(bounds_msg).is_ok());
    }
}
