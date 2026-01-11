//! Memory intrinsics required by LLVM/rustc in no_std environments.
//!
//! These functions provide the same functionality as libc (memcpy, memmove, etc.)
//! but are implemented in pure Rust without external dependencies.

/// Copy `n` bytes from `src` to `dst`. The memory regions must not overlap.
///
/// # Safety
///
/// - `dst` must be valid for writes of `n` bytes
/// - `src` must be valid for reads of `n` bytes
/// - The memory regions must not overlap
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
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
/// - `dst` must be valid for writes of `n` bytes
/// - `src` must be valid for reads of `n` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
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
/// - `dst` must be valid for writes of `n` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
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

/// Compare `n` bytes of memory at `s1` and `s2`.
///
/// Returns 0 if equal, negative if s1 < s2, positive if s1 > s2.
///
/// # Safety
///
/// - `s1` must be valid for reads of `n` bytes
/// - `s2` must be valid for reads of `n` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
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
/// - `s1` must be valid for reads of `n` bytes
/// - `s2` must be valid for reads of `n` bytes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
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
