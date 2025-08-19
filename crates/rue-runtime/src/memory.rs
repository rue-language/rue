//! Memory management functions for the Rue runtime
//!
//! This module provides low-level memory operations and utilities.

/// Copy memory from source to destination
///
/// This is a simple implementation of memcpy
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if dest.is_null() || src.is_null() || n == 0 {
        return dest;
    }

    // Simple byte-by-byte copy
    for i in 0..n {
        unsafe {
            *dest.add(i) = *src.add(i);
        }
    }

    dest
}

/// Set memory to a specific value
///
/// This is a simple implementation of memset
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_memset(ptr: *mut u8, value: i32, n: usize) -> *mut u8 {
    if ptr.is_null() || n == 0 {
        return ptr;
    }

    let byte_value = value as u8;

    for i in 0..n {
        unsafe {
            *ptr.add(i) = byte_value;
        }
    }

    ptr
}

/// Compare two memory regions
///
/// This is a simple implementation of memcmp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    if s1.is_null() || s2.is_null() || n == 0 {
        return 0;
    }

    for i in 0..n {
        let byte1 = unsafe { *s1.add(i) };
        let byte2 = unsafe { *s2.add(i) };

        if byte1 < byte2 {
            return -1;
        } else if byte1 > byte2 {
            return 1;
        }
    }

    0
}

/// Move memory from source to destination (handles overlapping regions)
///
/// This is a simple implementation of memmove
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rue_memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if dest.is_null() || src.is_null() || n == 0 {
        return dest;
    }

    // Check for overlap
    if dest < src as *mut u8 || dest >= unsafe { src.add(n) } as *mut u8 {
        // No overlap or dest is before src, copy forward
        unsafe { __rue_memcpy(dest, src, n) }
    } else {
        // Overlap with dest after src, copy backward
        for i in (0..n).rev() {
            unsafe {
                *dest.add(i) = *src.add(i);
            }
        }
        dest
    }
}
