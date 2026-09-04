//! Debug intrinsics for the `@dbg` builtin.
//!
//! These functions are called by generated code when `@dbg(expr)` is used.
//! Each type has its own debug function that prints the value followed by
//! a newline to stdout.

use crate::platform;

crate::define_runtime_implementation! {
    /// Debug intrinsic: print a signed 64-bit integer.
    ///
    /// Called by `@dbg(expr)` when the expression is a signed integer type.
    /// Prints the value followed by a newline to stdout.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_dbg_i64(value: i64)
    /// ```
    ///
    /// - `value` is passed in the `rdi` register (System V AMD64 ABI)
    pub extern "C" fn __rue_dbg_i64(value: i64) {
        platform::print_i64(value);
    }
}

crate::define_runtime_implementation! {
    /// Debug intrinsic: print an unsigned 64-bit integer.
    ///
    /// Called by `@dbg(expr)` when the expression is an unsigned integer type.
    /// Prints the value followed by a newline to stdout.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_dbg_u64(value: u64)
    /// ```
    ///
    /// - `value` is passed in the `rdi` register (System V AMD64 ABI)
    pub extern "C" fn __rue_dbg_u64(value: u64) {
        platform::print_u64(value);
    }
}

crate::define_runtime_implementation! {
    /// Debug intrinsic: print a boolean.
    ///
    /// Called by `@dbg(expr)` when the expression is a boolean.
    /// Prints "true" or "false" followed by a newline to stdout.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_dbg_bool(value: i64)
    /// ```
    ///
    /// - `value` is passed in the `rdi` register (System V AMD64 ABI)
    /// - Non-zero values are treated as true, zero as false
    pub extern "C" fn __rue_dbg_bool(value: i64) {
        platform::print_bool(value != 0);
    }
}

crate::define_runtime_implementation! {
    /// Debug intrinsic: print an `f32` or `f64`.
    ///
    /// Called by `@dbg(expr)` when the expression is a floating-point value.
    /// The value arrives as its IEEE-754 bit pattern in an integer register
    /// beside an explicit width discriminator (ADR-0065 §6), the same shape
    /// `__rue_to_string_float` takes. This helper formats *through* that one
    /// float-to-text authority — so `@dbg` and `@to_string` cannot drift, and
    /// an invalid width traps there rather than selecting a width here — then
    /// writes the text and a newline and releases the temporary buffer.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_dbg_float(bits: u64, width: u32)
    /// ```
    pub extern "C" fn __rue_dbg_float(bits: u64, width: u32) {
        let mut formatted = crate::string::StrBufResult {
            ptr: core::ptr::null_mut(),
            cap: 0,
            len: 0,
        };
        // SAFETY: `formatted` is live, aligned, exclusive result storage that
        // nothing else observes. The formatter either traps or publishes an
        // owned allocation of exactly `cap` bytes at alignment 1, which is
        // what is read and then released here.
        unsafe {
            crate::string::__rue_to_string_float(&mut formatted, bits, width);
            platform::write_stdout(core::slice::from_raw_parts(
                formatted.ptr,
                formatted.len as usize,
            ));
            platform::write_stdout(b"\n");
            crate::heap::free(formatted.ptr, formatted.cap, 1);
        }
    }
}

crate::define_runtime_implementation! {
    /// Debug intrinsic: print a string.
    ///
    /// Called by `@dbg(expr)` when the expression is a String type.
    /// Writes the string content followed by a newline to stdout.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64)
    /// ```
    ///
    /// - `ptr` is passed in the first argument register (rdi on x86_64, x0 on aarch64)
    /// - `len` is passed in the second argument register (rsi on x86_64, x1 on aarch64)
    /// - String is passed as a fat pointer (ptr, len) expanded into two arguments
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - `ptr` is non-null and points to a valid UTF-8 string of `len` bytes
    ///   when `len > 0`; it may be null when `len == 0`
    /// - The memory region remains valid for the duration of the call
    pub unsafe extern "C" fn __rue_dbg_str(ptr: *const u8, len: u64) {
        // SAFETY: Creating a slice from the raw pointer is safe because:
        // - The caller (Rue-generated code) guarantees `ptr` points to valid UTF-8
        //   string data of exactly `len` bytes
        // - The String type in Rue ensures the memory is properly allocated and
        //   remains valid for the duration of this call
        // - `ptr` is properly aligned (u8 requires only byte alignment)
        // - The pointed-to memory is initialized (Rue initializes all memory)
        // - We only read from the slice, never write
        if len > 0 {
            // SAFETY: the caller guarantees a non-null pointer valid for `len`
            // initialized bytes when the length is positive.
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
            platform::write_stdout(bytes);
        }
        // Write newline using byte char literal (b"..." has issues with macOS linker)
        let newline = [b'\n'];
        platform::write_stdout(&newline);
    }
}
