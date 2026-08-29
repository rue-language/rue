//! Runtime primitives for text views, allocation, and integer formatting.
//!
//! Growable strings are implemented by the source-defined `std.strbuf.StrBuf`.
//! This module retains only operations that are genuinely runtime-owned: core
//! `str` comparison/indexing/UTF-8 iteration, heap access, and formatting
//! results returned through the `StrBuf` three-word representation.

use crate::heap;
pub use rue_runtime_abi::StrBufResult;

/// Minimum capacity for runtime-produced `StrBuf` values.
pub const STRING_MIN_CAPACITY: u64 = 16;

crate::define_runtime_implementation! {
    /// `str` equality comparison.
    ///
    /// Called by the `==` operator on `str` values. Compares two strings
    /// represented as fat pointers (pointer + length pairs).
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u64
    /// ```
    ///
    /// - `ptr1` is passed in the first argument register (rdi on x86_64, x0 on aarch64)
    /// - `len1` is passed in the second argument register (rsi on x86_64, x1 on aarch64)
    /// - `ptr2` is passed in the third argument register (rdx on x86_64, x2 on aarch64)
    /// - `len2` is passed in the fourth argument register (rcx on x86_64, x3 on aarch64)
    /// - Returns 1 if strings are equal, 0 otherwise (in the full `rax`/`x0` register)
    ///
    /// # Implementation
    ///
    /// Fast path: If lengths differ, strings cannot be equal (returns 0).
    /// Slow path: Compare through the runtime's alignment-safe chunked byte
    /// equality primitive.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - When `len1 > 0`, `ptr1` is non-null and points to a valid buffer of at
    ///   least `len1` bytes; it may be null when `len1 == 0`
    /// - When `len2 > 0`, `ptr2` is non-null and points to a valid buffer of at
    ///   least `len2` bytes; it may be null when `len2 == 0`
    /// - Both pointers remain valid for the duration of the call
    pub unsafe extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u64 {
        // Fast path 1: different lengths means not equal
        if len1 != len2 {
            return 0;
        }

        // Fast path 2: pointer equality - if both point to same memory with same length,
        // they're equal. This is especially useful for comparing string literals to themselves
        // since they point to the same rodata location.
        if ptr1 == ptr2 {
            return 1;
        }

        // Use the canonical runtime comparison authority. This avoids Rust
        // slice equality, which can lower to an external libc bcmp symbol,
        // while retaining the same pointer-validity contract.
        // SAFETY: Equal lengths and the caller's contract make both ranges
        // valid for the comparison.
        (unsafe { crate::memory::bcmp(ptr1, ptr2, len1 as usize) == 0 }) as u64
    }
}

// =============================================================================
// Heap Allocation Wrappers
// =============================================================================

crate::define_runtime_implementation! {
    /// Allocate memory from the heap.
    ///
    /// This is the main allocation function for Rue programs. Small allocations
    /// are recycled by size class and large allocations use dedicated mappings.
    ///
    /// # Arguments
    ///
    /// * `size` - Number of bytes to allocate
    /// * `align` - Required alignment (must be a power of 2)
    ///
    /// # Returns
    ///
    /// A pointer to the allocated memory, or null on failure.
    /// The memory is uninitialized.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_alloc(size: u64, align: u64) -> *mut u8
    /// ```
    ///
    /// - `size` is passed in the first argument register (rdi on x86_64, x0 on aarch64)
    /// - `align` is passed in the second argument register (rsi on x86_64, x1 on aarch64)
    /// - Returns pointer in rax (x86_64) or x0 (aarch64)
    pub extern "C" fn __rue_alloc(size: u64, align: u64) -> *mut u8 {
        heap::alloc(size, align)
    }
}

crate::define_runtime_implementation! {
    /// Allocate zero-initialized memory from the heap.
    ///
    /// Identical to `__rue_alloc` except that every byte of the returned block
    /// reads as zero (RUE-968).
    ///
    /// # Arguments
    ///
    /// * `size` - Number of bytes to allocate
    /// * `align` - Required alignment (must be a power of 2)
    ///
    /// # Returns
    ///
    /// A pointer to zeroed memory, or null on failure.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_alloc_zeroed(size: u64, align: u64) -> *mut u8
    /// ```
    pub extern "C" fn __rue_alloc_zeroed(size: u64, align: u64) -> *mut u8 {
        heap::alloc_zeroed(size, align)
    }
}

crate::define_runtime_implementation! {
    /// Free memory previously allocated by `__rue_alloc`.
    ///
    /// # Arguments
    ///
    /// * `ptr` - Pointer to the memory to free
    /// * `size` - Size supplied by the runtime allocation ABI
    /// * `align` - Alignment supplied by the runtime allocation ABI
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64)
    /// ```
    /// # Safety
    ///
    /// A non-null `ptr` must identify a live Rue allocation described by
    /// exactly `size` and `align`, and it must not be used after this call.
    pub unsafe extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64) {
        // SAFETY: inherited from the exported helper's caller contract.
        unsafe { heap::free(ptr, size, align) }
    }
}

crate::define_runtime_implementation! {
    /// Reallocate memory to a new size.
    ///
    /// # Arguments
    ///
    /// * `ptr` - Pointer to the existing allocation (or null for new allocation)
    /// * `old_size` - Size of the existing allocation (ignored if ptr is null)
    /// * `new_size` - Desired new size
    /// * `align` - Required alignment (must be a power of 2)
    ///
    /// # Returns
    ///
    /// A pointer to the reallocated memory, or null on failure.
    ///
    /// # Behavior
    ///
    /// - If `ptr` is null: behaves like `__rue_alloc(new_size, align)`
    /// - If `new_size` is 0: frees the memory and returns null
    /// - If both layouts share one storage class: returns `ptr` unchanged
    /// - Otherwise: allocates a new block, copies the preserved prefix, and frees `ptr`
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8
    /// ```
    ///
    /// # Safety
    ///
    /// If `ptr` is non-null and `new_size > old_size`, it must be valid for
    /// reads of `old_size` bytes. It must be either null or a pointer returned
    /// by this runtime allocator with the supplied allocation layout.
    pub unsafe extern "C" fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8 {
        // SAFETY: inherited from the exported helper's caller contract.
        unsafe { heap::realloc(ptr, old_size, new_size, align) }
    }
}

crate::define_runtime_implementation! {
    /// Resize an allocation in place, without ever moving it.
    ///
    /// # Arguments
    ///
    /// * `ptr` - Pointer to the existing allocation
    /// * `old_size` - Size the allocation currently carries
    /// * `new_size` - Desired new size
    /// * `align` - Alignment the allocation was made with
    ///
    /// # Returns
    ///
    /// `1` when the block now describes `new_size` bytes at the same address —
    /// the caller must hand `new_size` back at deallocation — and `0` when the
    /// request was refused, in which case nothing changed and `old_size` still
    /// describes the block.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_resize(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> i64
    /// ```
    ///
    /// The `i64` result is the C-ABI word carrying a Rue `bool`.
    ///
    /// # Safety
    ///
    /// A non-null `ptr` must be a pointer returned by this runtime allocator
    /// that is live with exactly the supplied `old_size` and `align`.
    pub unsafe extern "C" fn __rue_resize(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> i64 {
        // SAFETY: inherited from the exported helper's caller contract.
        i64::from(unsafe { heap::resize(ptr, old_size, new_size, align) })
    }
}

// =============================================================================
// Core `str` byte access and UTF-8 iteration
// =============================================================================// =============================================================================
// A `str` is a packed byte view; indexing bounds-checks against `len` and does
// not impose UTF-8 scalar boundaries.
// =============================================================================
//
// Rue's `str` is a byte string (conventionally UTF-8, not guaranteed valid),
// so byte-level access looks INSIDE the raw bytes. Both operations below take
// a String by borrow (ptr, len, cap) and bounds-check against `len`, trapping
// (exit 101, via `__rue_bounds_check`) on out-of-range access — exactly like
// array indexing. Neither respects UTF-8 char boundaries: any in-range byte
// index / byte range is valid.

crate::define_runtime_implementation! {
    /// Read the byte at `index` in a `str`, returning it as `u8` (ADR-0043
    /// ADR-0043, RUE-324).
    ///
    /// Implements `s[i]` for the `str` string type. A `str` is `[u8]` + UTF-8:
    /// its bytes are PACKED (1 byte each, in `.rodata` for a literal), so like
    /// `String` this reads a single byte at `ptr + index` after a bounds check.
    /// It is the 2-word (`{ptr, len}`) analog of `__rue_str_byte_at`, without
    /// the (nonexistent) `cap` word. An `index >= len` traps at runtime
    /// (index-out-of-bounds panic, exit 101), matching array/`String` indexing.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_str_byte_at(ptr: *const u8, len: u64, index: u64) -> u64
    /// ```
    ///
    /// The `str` is passed by value as its two fields (ptr, len); the byte is
    /// returned zero-extended in the return register — see the
    /// `__rue_str_byte_at` ABI note on why a clean full register is required.
    ///
    /// # Safety
    ///
    /// When `len > 0`, `ptr` must be non-null and point to a valid buffer of at
    /// least `len` bytes; it may be null when `len == 0`. The in-range read
    /// below is then sound because the bounds check guarantees `index < len`.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_str_byte_at(ptr: *const u8, len: u64, index: u64) -> u64 {
        if index >= len {
            // Never returns: prints "error: index out of bounds" and exits 101.
            crate::error::__rue_bounds_check();
        }
        // SAFETY: `index < len` (checked above) and the caller guarantees `ptr`
        // is valid for `len` bytes, so `ptr.add(index)` is in bounds. u8 has no
        // alignment requirement. Zero-extend to u64 so the whole return
        // register is clean (see the `__rue_str_byte_at` ABI note).
        unsafe { *ptr.add(index as usize) as u64 }
    }
}

/// Strictly decode one UTF-8 scalar starting at byte `offset` in the `len`-byte
/// buffer at `ptr`, returning `(scalar, width_in_bytes)`.
///
/// This backs `for c in s.chars()` (RUE-220, ADR-0035). Rue's `String` is a
/// byte string that may hold arbitrary bytes; decoding is where invalidity is
/// caught, so any malformed, truncated, overlong, or surrogate sequence traps
/// (`__rue_invalid_utf8`, exit 101) rather than producing `U+FFFD` (the lossy
/// `.chars_lossy()` variant substitutes it instead of trapping). Callers only invoke this when
/// `offset < len` (the loop's end test guarantees a byte remains), but a
/// multi-byte sequence that runs past `len` is a truncated sequence and traps.
///
/// # Safety
///
/// When `len > 0`, `ptr` must be non-null and valid for `len` bytes; it may be
/// null when `len == 0`.
unsafe fn __rue_decode_utf8_at(ptr: *const u8, len: u64, offset: u64) -> (u32, u64) {
    let len = len as usize;
    let i = offset as usize;
    if i >= len {
        crate::error::__rue_invalid_utf8();
    }
    // SAFETY: `i < len` and the caller guarantees `ptr` is valid for `len`
    // bytes, so `ptr.add(i)` is in bounds.
    let b0 = unsafe { *ptr.add(i) };
    // ASCII fast path (single byte).
    if b0 < 0x80 {
        return (b0 as u32, 1);
    }
    // Lead byte determines the sequence width, the minimum non-overlong code
    // point, and the initial code-point bits. Leads 0xC0/0xC1 (always overlong)
    // and 0x80..=0xBF (continuation) and 0xF5..=0xFF (> U+10FFFF) are invalid.
    let width: usize;
    let min: u32;
    let mut cp: u32;
    if (0xC2..=0xDF).contains(&b0) {
        width = 2;
        min = 0x80;
        cp = (b0 as u32) & 0x1F;
    } else if (0xE0..=0xEF).contains(&b0) {
        width = 3;
        min = 0x800;
        cp = (b0 as u32) & 0x0F;
    } else if (0xF0..=0xF4).contains(&b0) {
        width = 4;
        min = 0x10000;
        cp = (b0 as u32) & 0x07;
    } else {
        crate::error::__rue_invalid_utf8();
    }
    if i + width > len {
        crate::error::__rue_invalid_utf8();
    }
    let mut k = 1usize;
    while k < width {
        // SAFETY: `i + width <= len` (checked above), so `i + k` is in bounds.
        let b = unsafe { *ptr.add(i + k) };
        if b & 0xC0 != 0x80 {
            crate::error::__rue_invalid_utf8();
        }
        cp = (cp << 6) | ((b as u32) & 0x3F);
        k += 1;
    }
    // Reject overlong encodings, UTF-16 surrogates, and out-of-range scalars.
    if cp < min || (0xD800..=0xDFFF).contains(&cp) || cp > 0x10FFFF {
        crate::error::__rue_invalid_utf8();
    }
    (cp, width as u64)
}

crate::define_runtime_implementation! {
    /// Decode one scalar through the shared two-word `str` view ABI.
    pub unsafe extern "C" fn __rue_str_char_scalar(
        ptr: *const u8,
        len: u64,
        offset: u64,
    ) -> u64 {
        let (scalar, _width) = unsafe { __rue_decode_utf8_at(ptr, len, offset) };
        scalar as u64
    }
}

crate::define_runtime_implementation! {
    /// Advance one scalar through the shared two-word `str` view ABI.
    pub unsafe extern "C" fn __rue_str_char_next(
        ptr: *const u8,
        len: u64,
        offset: u64,
    ) -> u64 {
        let (_scalar, width) = unsafe { __rue_decode_utf8_at(ptr, len, offset) };
        offset + width
    }
}

/// Leniently decode one UTF-8 scalar starting at byte `offset` in the `len`-byte
/// buffer at `ptr`, returning `(scalar, width_in_bytes)`.
///
/// This backs `for c in s.chars_lossy()` (RUE-17, ADR-0035). Unlike the
/// strict [`__rue_decode_utf8_at`], which traps at the decode boundary, this
/// **never traps**: an invalid, truncated, overlong, or surrogate sequence
/// yields the Unicode replacement scalar `U+FFFD` and advances past its
/// *maximal subpart of an ill-formed subsequence* — one byte for a lone
/// invalid lead/continuation, more when a partly-valid multi-byte sequence
/// breaks or runs off the end. This is the Unicode-recommended substitution
/// (matching Rust's `String::from_utf8_lossy`), so a valid string decodes
/// identically to `.chars()` while garbage bytes are replaced instead of
/// aborting. Lossiness is opt-in: the default `.chars()` still traps.
///
/// # Safety
///
/// When `len > 0`, `ptr` must be non-null and valid for `len` bytes; it may be
/// null when `len == 0`.
unsafe fn __rue_decode_utf8_lossy_at(ptr: *const u8, len: u64, offset: u64) -> (u32, u64) {
    const FFFD: u32 = 0xFFFD;
    let len = len as usize;
    let i = offset as usize;
    if i >= len {
        return (FFFD, 1);
    }
    // SAFETY: `i < len` and the caller guarantees `ptr` is valid for `len`
    // bytes, so `ptr.add(i)` is in bounds.
    let b0 = unsafe { *ptr.add(i) };
    // ASCII fast path (single byte).
    if b0 < 0x80 {
        return (b0 as u32, 1);
    }
    // The lead byte fixes the sequence width and the valid range of the FIRST
    // continuation byte. That first-byte range is what enforces non-overlong
    // (0xE0 needs >= 0xA0, 0xF0 needs >= 0x90), non-surrogate (0xED needs
    // <= 0x9F), and in-range (0xF4 needs <= 0x8F) — so a full-width sequence
    // that clears these checks is always a valid scalar. Leads 0x80..=0xC1 and
    // 0xF5..=0xFF can never begin a sequence: substitute one U+FFFD, step 1.
    let (width, second_lo, second_hi) = match b0 {
        0xC2..=0xDF => (2usize, 0x80u8, 0xBFu8),
        0xE0 => (3, 0xA0, 0xBF),
        0xE1..=0xEC => (3, 0x80, 0xBF),
        0xED => (3, 0x80, 0x9F),
        0xEE..=0xEF => (3, 0x80, 0xBF),
        0xF0 => (4, 0x90, 0xBF),
        0xF1..=0xF3 => (4, 0x80, 0xBF),
        0xF4 => (4, 0x80, 0x8F),
        _ => return (FFFD, 1),
    };
    let mask = match width {
        2 => 0x1F,
        3 => 0x0F,
        _ => 0x07,
    };
    let mut cp = (b0 as u32) & mask;
    // First continuation: uses the width-specific range above.
    if i + 1 >= len {
        return (FFFD, 1);
    }
    // SAFETY: `i + 1 < len`, in bounds.
    let b1 = unsafe { *ptr.add(i + 1) };
    if b1 < second_lo || b1 > second_hi {
        return (FFFD, 1);
    }
    cp = (cp << 6) | ((b1 as u32) & 0x3F);
    // Remaining continuations (positions 3..width) accept the generic
    // 0x80..=0xBF range. On a break, the maximal subpart is everything
    // consumed so far.
    let mut consumed = 2usize;
    while consumed < width {
        if i + consumed >= len {
            return (FFFD, consumed as u64);
        }
        // SAFETY: `i + consumed < len`, in bounds.
        let b = unsafe { *ptr.add(i + consumed) };
        if b & 0xC0 != 0x80 {
            return (FFFD, consumed as u64);
        }
        cp = (cp << 6) | ((b as u32) & 0x3F);
        consumed += 1;
    }
    (cp, width as u64)
}

crate::define_runtime_implementation! {
    /// Decode one scalar lossily through the shared two-word `str` view ABI.
    pub unsafe extern "C" fn __rue_str_char_scalar_lossy(
        ptr: *const u8,
        len: u64,
        offset: u64,
    ) -> u64 {
        let (scalar, _width) = unsafe { __rue_decode_utf8_lossy_at(ptr, len, offset) };
        scalar as u64
    }
}

crate::define_runtime_implementation! {
    /// Advance one scalar lossily through the shared two-word `str` view ABI.
    pub unsafe extern "C" fn __rue_str_char_next_lossy(
        ptr: *const u8,
        len: u64,
        offset: u64,
    ) -> u64 {
        let (_scalar, width) = unsafe { __rue_decode_utf8_lossy_at(ptr, len, offset) };
        offset + width
    }
}

// =============================================================================
// Integer-to-`StrBuf` formatting
// =============================================================================// =============================================================================
// Integer-to-`StrBuf` formatting (ADR-0035)
// =============================================================================

crate::define_runtime_implementation! {
    /// Format an `i64` in base 10 into a freshly heap-allocated `StrBuf`.
    ///
    /// Implements the `@to_string(n)` intrinsic. Handles the full `i64` range,
    /// including `i64::MIN` (whose magnitude is not representable as a positive
    /// `i64`), and prefixes negative values with `'-'`.
    ///
    /// # ABI (sret convention)
    ///
    /// ```text
    /// extern "C" fn __rue_to_string(out: *mut StrBufResult, n: i64)
    /// ```
    ///
    /// `out` (the sret pointer) comes first, then the integer argument. The
    /// returned `StrBuf` owns a fresh heap buffer, so it can be mutated and
    /// dropped independently.
    ///
    /// # Safety
    ///
    /// `out` must be a valid sret pointer — the caller supplies valid result storage.
    pub unsafe extern "C" fn __rue_to_string(out: *mut StrBufResult, n: i64) {
        // "-9223372036854775808" (i64::MIN) is the longest result: 20 bytes.
        let mut buf = [0u8; 20];
        let negative = n < 0;
        // Magnitude as u64. `wrapping_neg` yields the correct magnitude even for
        // i64::MIN, whose positive value does not fit in an i64.
        let mut mag = n as u64;
        if negative {
            mag = mag.wrapping_neg();
        }
        // Emit decimal digits least-significant first, filling `buf` from the
        // back. `i` walks left toward the front of the buffer.
        let mut i = buf.len();
        if mag == 0 {
            i -= 1;
            buf[i] = b'0';
        } else {
            while mag > 0 {
                i -= 1;
                buf[i] = b'0' + (mag % 10) as u8;
                mag /= 10;
            }
        }
        if negative {
            i -= 1;
            buf[i] = b'-';
        }

        let len = (buf.len() - i) as u64;
        let new_cap = if len < STRING_MIN_CAPACITY {
            STRING_MIN_CAPACITY
        } else {
            len
        };
        let new_ptr = heap::alloc(new_cap, 1);

        if new_ptr.is_null() {
            crate::error::allocation_failure();
        }

        // SAFETY: `new_ptr` was just allocated with `new_cap >= len` bytes; the
        // source range `buf[i..]` holds exactly `len` initialized bytes; the
        // regions don't overlap (fresh allocation). `out` is a valid sret
        // pointer (the caller supplies valid result storage).
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr().add(i), new_ptr, len as usize);
            (*out).ptr = new_ptr;
            (*out).len = len;
            (*out).cap = new_cap;
        }
    }
}

crate::define_runtime_implementation! {
    /// Format a `u64` in base 10 into a freshly heap-allocated `StrBuf`.
    ///
    /// Implements `@to_string(n)` for unsigned integers (RUE-314). The compiler
    /// zero-extends narrower unsigned operands (`u8`/`u16`/`u32`) to `u64`
    /// before the call, so this handles the full `u64` range — a value with the
    /// high bit set prints as its unsigned magnitude (e.g. `u64::MAX` prints
    /// `18446744073709551615`), never as a negative number. There is no sign
    /// prefix.
    ///
    /// # ABI (sret convention)
    ///
    /// ```text
    /// extern "C" fn __rue_to_string_unsigned(out: *mut StrBufResult, n: u64)
    /// ```
    ///
    /// `out` (the sret pointer) comes first, then the integer argument. The
    /// returned `StrBuf` owns a fresh heap buffer, so it can be mutated and
    /// dropped independently.
    ///
    /// # Safety
    ///
    /// `out` must be a valid sret pointer — the caller supplies valid result storage.
    pub unsafe extern "C" fn __rue_to_string_unsigned(out: *mut StrBufResult, n: u64) {
        // "18446744073709551615" (u64::MAX) is the longest result: 20 bytes.
        let mut buf = [0u8; 20];
        let mut mag = n;
        // Emit decimal digits least-significant first, filling `buf` from the
        // back. `i` walks left toward the front of the buffer.
        let mut i = buf.len();
        if mag == 0 {
            i -= 1;
            buf[i] = b'0';
        } else {
            while mag > 0 {
                i -= 1;
                buf[i] = b'0' + (mag % 10) as u8;
                mag /= 10;
            }
        }

        let len = (buf.len() - i) as u64;
        let new_cap = if len < STRING_MIN_CAPACITY {
            STRING_MIN_CAPACITY
        } else {
            len
        };
        let new_ptr = heap::alloc(new_cap, 1);

        if new_ptr.is_null() {
            crate::error::allocation_failure();
        }

        // SAFETY: `new_ptr` was just allocated with `new_cap >= len` bytes; the
        // source range `buf[i..]` holds exactly `len` initialized bytes; the
        // regions don't overlap (fresh allocation). `out` is a valid sret
        // pointer (the caller supplies valid result storage).
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr().add(i), new_ptr, len as usize);
            (*out).ptr = new_ptr;
            (*out).len = len;
            (*out).cap = new_cap;
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::process::Command;
    use super::*;

    fn blank_result() -> StrBufResult {
        StrBufResult {
            ptr: core::ptr::null_mut(),
            len: u64::MAX,
            cap: u64::MAX,
        }
    }

    fn result_bytes(result: &StrBufResult) -> &[u8] {
        assert!(!result.ptr.is_null());
        // SAFETY: Formatting functions publish at least `len` initialized
        // bytes in a live allocation.
        unsafe { core::slice::from_raw_parts(result.ptr, result.len as usize) }
    }

    #[test]
    fn str_eq_compares_content_and_length() {
        let a = b"same";
        let b = b"same";
        let c = b"same!";
        let d = b"diff";
        // SAFETY: every pointer/length pair names a live byte array.
        unsafe {
            assert_eq!(__rue_str_eq(a.as_ptr(), 4, b.as_ptr(), 4), 1);
            assert_eq!(__rue_str_eq(a.as_ptr(), 4, a.as_ptr(), 4), 1);
            assert_eq!(__rue_str_eq(a.as_ptr(), 4, c.as_ptr(), 5), 0);
            assert_eq!(__rue_str_eq(a.as_ptr(), 4, d.as_ptr(), 4), 0);
            assert_eq!(__rue_str_eq(a.as_ptr(), 0, d.as_ptr(), 0), 1);
        }
    }

    #[test]
    fn str_eq_zero_length_accepts_null_and_distinct_pointers() {
        let byte = 0u8;
        // SAFETY: Both views have zero length, so null pointers are valid.
        unsafe {
            assert_eq!(__rue_str_eq(core::ptr::null(), 0, &byte, 0), 1);
            assert_eq!(__rue_str_eq(&byte, 0, core::ptr::null(), 0), 1);
        }
    }

    #[test]
    fn str_eq_handles_chunk_boundaries_and_unaligned_slices() {
        let chunk_size = core::mem::size_of::<u64>();
        for length in 0..=(chunk_size * 4 + 3) {
            for left_offset in 0..chunk_size {
                for right_offset in 0..chunk_size {
                    let mut left = self::std::vec![0xa5; 128];
                    let mut right = self::std::vec![0x5a; 128];
                    for index in 0..=(chunk_size * 4 + 3) {
                        let value = (index as u8).wrapping_mul(37).wrapping_add(11);
                        left[left_offset + index] = value;
                        right[right_offset + index] = value;
                    }
                    // SAFETY: Both pointers name valid buffers for `length`
                    // bytes; offsets intentionally cover every byte alignment.
                    unsafe {
                        assert_eq!(
                            __rue_str_eq(
                                left.as_ptr().add(left_offset),
                                length as u64,
                                right.as_ptr().add(right_offset),
                                length as u64,
                            ),
                            1
                        );
                    }
                    for mismatch in 0..length {
                        left[left_offset + mismatch] ^= 1;
                        // SAFETY: The changed byte remains within the valid
                        // string range and the two ranges have equal lengths.
                        unsafe {
                            assert_eq!(
                                __rue_str_eq(
                                    left.as_ptr().add(left_offset),
                                    length as u64,
                                    right.as_ptr().add(right_offset),
                                    length as u64,
                                ),
                                0
                            );
                        }
                        left[left_offset + mismatch] ^= 1;
                    }
                }
            }
        }
    }

    #[test]
    fn str_byte_access_returns_zero_extended_bytes() {
        let bytes = [0x00, 0x7f, 0x80, 0xff];
        // SAFETY: every index is within the live array.
        unsafe {
            assert_eq!(__rue_str_byte_at(bytes.as_ptr(), 4, 0), 0);
            assert_eq!(__rue_str_byte_at(bytes.as_ptr(), 4, 2), 0x80);
            assert_eq!(__rue_str_byte_at(bytes.as_ptr(), 4, 3), 0xff);
        }
    }

    #[test]
    fn strict_utf8_iteration_decodes_multibyte_scalars() {
        let bytes = "aé🙂".as_bytes();
        // SAFETY: every offset is a scalar boundary in the live UTF-8 buffer.
        unsafe {
            assert_eq!(__rue_str_char_scalar(bytes.as_ptr(), 7, 0), 'a' as u64);
            assert_eq!(__rue_str_char_next(bytes.as_ptr(), 7, 0), 1);
            assert_eq!(__rue_str_char_scalar(bytes.as_ptr(), 7, 1), 'é' as u64);
            assert_eq!(__rue_str_char_next(bytes.as_ptr(), 7, 1), 3);
            assert_eq!(__rue_str_char_scalar(bytes.as_ptr(), 7, 3), '🙂' as u64);
            assert_eq!(__rue_str_char_next(bytes.as_ptr(), 7, 3), 7);
        }
    }

    #[test]
    fn lossy_utf8_iteration_replaces_invalid_sequences() {
        const FFFD: u64 = 0xfffd;
        let bytes = [b'a', 0xf0, 0x9f, b'!', 0xff, b'z'];
        // SAFETY: every offset is within the live buffer; lossy decoding accepts
        // invalid UTF-8.
        unsafe {
            assert_eq!(
                __rue_str_char_scalar_lossy(bytes.as_ptr(), 6, 0),
                b'a' as u64
            );
            assert_eq!(__rue_str_char_next_lossy(bytes.as_ptr(), 6, 0), 1);
            assert_eq!(__rue_str_char_scalar_lossy(bytes.as_ptr(), 6, 1), FFFD);
            assert_eq!(__rue_str_char_next_lossy(bytes.as_ptr(), 6, 1), 3);
            assert_eq!(__rue_str_char_scalar_lossy(bytes.as_ptr(), 6, 4), FFFD);
            assert_eq!(__rue_str_char_next_lossy(bytes.as_ptr(), 6, 4), 5);
        }
    }

    #[test]
    fn integer_formatting_covers_extremes() {
        let mut out = blank_result();
        // SAFETY: `out` is valid, aligned, exclusive result storage.
        unsafe { __rue_to_string(&mut out, i64::MIN) };
        assert_eq!(result_bytes(&out), b"-9223372036854775808");

        unsafe { __rue_to_string_unsigned(&mut out, u64::MAX) };
        assert_eq!(result_bytes(&out), b"18446744073709551615");
    }

    #[test]
    fn formatting_allocation_failure_uses_canonical_trap() {
        const CHILD_ENV: &str = "RUE_STRING_OOM_CHILD";
        if let Some(mode) = self::std::env::var_os(CHILD_ENV) {
            crate::heap::fail_allocations_after_for_test(0);
            let mut out = blank_result();
            unsafe {
                match mode.to_string_lossy().as_ref() {
                    "signed" => __rue_to_string(&mut out, 1),
                    "unsigned" => __rue_to_string_unsigned(&mut out, 1),
                    other => panic!("unknown allocation failure mode: {other}"),
                }
            }
            unreachable!();
        }

        for mode in ["signed", "unsigned"] {
            let output = Command::new(self::std::env::current_exe().expect("current test binary"))
                .args([
                    "--exact",
                    "string::tests::formatting_allocation_failure_uses_canonical_trap",
                    "--nocapture",
                ])
                .env(CHILD_ENV, mode)
                .output()
                .expect("spawn allocation-failure child");
            assert_eq!(output.status.code(), Some(101), "mode {mode}");
            assert_eq!(output.stderr, b"panic: out of memory\n", "mode {mode}");
        }
    }
}
