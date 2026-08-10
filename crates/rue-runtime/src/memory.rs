//! Memory intrinsics required by LLVM/rustc in no_std environments.
//!
//! These functions provide the same functionality as libc (memcpy, memmove, etc.)
//! but are implemented in pure Rust without external dependencies.

/// Copy `n` bytes from `src` to `dst`. The memory regions must not overlap.
///
/// # Safety
///
/// - When `n > 0`, `dst` must be non-null and valid for writes of `n` bytes
/// - When `n > 0`, `src` must be non-null and valid for reads of `n` bytes
/// - Either pointer may be null when `n == 0`
/// - The memory regions must not overlap
pub unsafe fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let mut i = 0;
    while i < n {
        // SAFETY: We are within bounds because:
        // - `i < n` is our loop invariant
        // - Caller guarantees `dst` is valid for writes of `n` bytes
        // - Caller guarantees `src` is valid for reads of `n` bytes
        // - Caller guarantees the regions don't overlap
        // The byte-by-byte copy is safe because u8 has no alignment requirements.
        unsafe { *dst.add(i) = *src.add(i) };
        i += 1;
    }
    dst
}

/// Copy `n` bytes from `src` to `dst`. The memory regions may overlap.
///
/// # Safety
///
/// - When `n > 0`, `dst` must be non-null and valid for writes of `n` bytes
/// - When `n > 0`, `src` must be non-null and valid for reads of `n` bytes
/// - Either pointer may be null when `n == 0`
pub unsafe fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dst as usize) < (src as usize) {
        // Copy forwards when dst is before src (or they don't overlap)
        let mut i = 0;
        while i < n {
            // SAFETY: We are within bounds because:
            // - `i < n` is our loop invariant
            // - Caller guarantees `dst` is valid for writes of `n` bytes
            // - Caller guarantees `src` is valid for reads of `n` bytes
            // Forward copy is correct when dst < src because we write to lower
            // addresses before reading from them.
            unsafe { *dst.add(i) = *src.add(i) };
            i += 1;
        }
    } else {
        // Copy backwards to handle overlap when dst >= src
        let mut i = n;
        while i > 0 {
            i -= 1;
            // SAFETY: We are within bounds because:
            // - After decrement, `i < n` (we started at n and decremented before use)
            // - Caller guarantees `dst` is valid for writes of `n` bytes
            // - Caller guarantees `src` is valid for reads of `n` bytes
            // Backward copy is correct when dst >= src because we write to higher
            // addresses before reading from them.
            unsafe { *dst.add(i) = *src.add(i) };
        }
    }
    dst
}

/// Fill `n` bytes of memory at `dst` with the byte `c`.
///
/// # Safety
///
/// - When `n > 0`, `dst` must be non-null and valid for writes of `n` bytes
/// - `dst` may be null when `n == 0`
pub unsafe fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    let byte = c as u8;
    let mut i = 0;
    while i < n {
        // SAFETY: We are within bounds because:
        // - `i < n` is our loop invariant
        // - Caller guarantees `dst` is valid for writes of `n` bytes
        // The byte write is safe because u8 has no alignment requirements.
        unsafe { *dst.add(i) = byte };
        i += 1;
    }
    dst
}

/// `@byte_copy(dst, src, size)` runtime helper: copy `size` bytes from `src`
/// into the non-overlapping region at `dst` (ADR-0058, RUE-937).
///
/// # Safety
///
/// - When `size > 0`, `dst` must be valid for writes of `size` bytes and `src`
///   valid for reads of `size` bytes; the regions must not overlap.
/// - Either pointer may be null when `size == 0` (a no-op).
pub unsafe fn __rue_byte_copy(dst: *mut u8, src: *const u8, size: u64) {
    // SAFETY: the caller upholds `memcpy`'s non-overlap and validity contract.
    unsafe { memcpy(dst, src, size as usize) };
}

/// `@byte_set(dst, value, size)` runtime helper: write the low byte of `value`
/// to each of `size` bytes at `dst` (ADR-0058, RUE-937).
///
/// # Safety
///
/// - When `size > 0`, `dst` must be valid for writes of `size` bytes.
/// - `dst` may be null when `size == 0` (a no-op).
pub unsafe fn __rue_byte_set(dst: *mut u8, value: u64, size: u64) {
    // SAFETY: the caller upholds `memset`'s validity contract. `memset` masks
    // the fill value to its low byte, matching the intrinsic's `u8` operand.
    unsafe { memset(dst, value as i32, size as usize) };
}

/// `@byte_move(dst, src, size)` runtime helper: copy `size` bytes from `src`
/// into `dst` as if through a temporary buffer, so the two regions may overlap
/// (RUE-964). This is the memmove-shaped sibling of [`__rue_byte_copy`].
///
/// # Safety
///
/// - When `size > 0`, `dst` must be valid for writes of `size` bytes and `src`
///   valid for reads of `size` bytes. The regions may overlap.
/// - Either pointer may be null when `size == 0` (a no-op).
pub unsafe fn __rue_byte_move(dst: *mut u8, src: *const u8, size: u64) {
    // SAFETY: the caller upholds `memmove`'s validity contract, which is
    // `memcpy`'s minus the non-overlap requirement.
    unsafe { memmove(dst, src, size as usize) };
}

/// Compare `n` bytes of memory at `s1` and `s2`.
///
/// Returns 0 if equal, negative if s1 < s2, positive if s1 > s2.
///
/// # Safety
///
/// - When `n > 0`, both pointers must be non-null and valid for reads of `n` bytes
/// - Either pointer may be null when `n == 0`
pub unsafe fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        // SAFETY: We are within bounds because:
        // - `i < n` is our loop invariant
        // - Caller guarantees `s1` is valid for reads of `n` bytes
        // - Caller guarantees `s2` is valid for reads of `n` bytes
        // The byte reads are safe because u8 has no alignment requirements.
        let a = unsafe { *s1.add(i) };
        let b = unsafe { *s2.add(i) };
        if a != b {
            return (a as i32) - (b as i32);
        }
        i += 1;
    }
    0
}

/// Compare `n` bytes of memory at `s1` and `s2` for equality.
///
/// Returns 0 if equal, non-zero if different.
///
/// This is a simplified version of `memcmp` that only tests for equality,
/// not ordering. Some compilers (including rustc/LLVM) may generate calls
/// to `bcmp` for slice equality comparisons in no_std environments.
///
/// # Safety
///
/// - When `n > 0`, both pointers must be non-null and valid for reads of `n` bytes
/// - Either pointer may be null when `n == 0`
pub unsafe fn bcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        // SAFETY: We are within bounds because:
        // - `i < n` is our loop invariant
        // - Caller guarantees `s1` is valid for reads of `n` bytes
        // - Caller guarantees `s2` is valid for reads of `n` bytes
        // The byte reads are safe because u8 has no alignment requirements.
        let a = unsafe { *s1.add(i) };
        let b = unsafe { *s2.add(i) };
        if a != b {
            return 1;
        }
        i += 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bcmp_equal() {
        let a = b"hello world";
        let b = b"hello world";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_not_equal() {
        let a = b"hello world";
        let b = b"hello xorld";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_ne!(result, 0);
    }

    #[test]
    fn test_bcmp_empty() {
        let a = b"";
        let b = b"";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), 0) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_first_byte_differs() {
        let a = b"abc";
        let b = b"xbc";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_ne!(result, 0);
    }

    #[test]
    fn test_bcmp_last_byte_differs() {
        let a = b"abc";
        let b = b"abx";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), a.len()) };
        assert_ne!(result, 0);
    }

    #[test]
    fn test_bcmp_partial_comparison() {
        // Compare only first 3 bytes - they're the same
        let a = b"abcdef";
        let b = b"abcxyz";
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), 3) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_single_byte_equal() {
        let a = [42u8];
        let b = [42u8];
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), 1) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bcmp_single_byte_differs() {
        let a = [42u8];
        let b = [43u8];
        let result = unsafe { bcmp(a.as_ptr(), b.as_ptr(), 1) };
        assert_ne!(result, 0);
    }
}
