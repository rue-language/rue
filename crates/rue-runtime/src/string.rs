//! String runtime functions and methods.
//!
//! This module provides runtime support for the source-level `StrBuf` type.
//! Exported `__rue_String_*` symbols retain their historical `String` spelling
//! as part of the runtime ABI:
//! - String equality comparison (`__rue_str_eq`)
//! - Heap allocation wrappers (`__rue_alloc`, `__rue_free`, `__rue_realloc`)
//! - String-specific allocation functions
//! - String constructors (`__rue_String_new`, `__rue_String_with_capacity`)
//! - String query methods (`len`, `capacity`, `is_empty`)
//! - Byte-string search (`contains`, `starts_with`)
//! - String mutation methods (`push_str`, `push`, `clear`, `reserve`)
//! - Integer-to-String formatting (`__rue_to_string`) and concatenation
//!   (`__rue_String_concat`)
//! - String cloning (`__rue_String_clone`)
//! - String dropping (`__rue_drop_String`)

use crate::heap;

/// Minimum capacity for string buffers.
/// This provides room for small appends without immediate reallocation.
pub const STRING_MIN_CAPACITY: u64 = 16;

/// The StringResult struct used for sret (struct return) convention.
/// Caller allocates this on stack, passes pointer to callee.
#[repr(C)]
pub struct StringResult {
    pub ptr: *mut u8,
    pub len: u64,
    pub cap: u64,
}

// Static assertions to verify StringResult layout assumptions.
// These ensure the sret convention works correctly across all platforms.
const _: () = {
    // StringResult is 24 bytes: 8 (ptr) + 8 (len) + 8 (cap)
    assert!(core::mem::size_of::<StringResult>() == 24);
    // StringResult is 8-byte aligned (pointer alignment)
    assert!(core::mem::align_of::<StringResult>() == 8);
};

crate::define_for_all_platforms! {
    /// String equality comparison.
    ///
    /// Called by the `==` operator on String types. Compares two strings
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
    /// Slow path: Compare bytes one by one until a difference is found or
    /// all bytes match.
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

        // Slow path: compare bytes one by one
        // We avoid slice comparison (==) because it generates a call to bcmp,
        // which is a libc function not available in our no_std runtime.
        for i in 0..len1 as usize {
            // SAFETY: Reading from both pointers is safe because:
            // - `i < len1` (which equals len2) is our loop invariant
            // - Caller guarantees `ptr1` is valid for reads of `len1` bytes
            // - Caller guarantees `ptr2` is valid for reads of `len2` bytes
            // - u8 has no alignment requirements
            let b1 = unsafe { *ptr1.add(i) };
            let b2 = unsafe { *ptr2.add(i) };
            if b1 != b2 {
                return 0;
            }
        }
        1
    }
}

// =============================================================================
// Heap Allocation Wrappers
// =============================================================================

crate::define_for_all_platforms! {
    /// Allocate memory from the heap.
    ///
    /// This is the main allocation function for Rue programs. Memory is allocated
    /// from a bump allocator backed by `mmap`.
    ///
    /// # Arguments
    ///
    /// * `size` - Number of bytes to allocate
    /// * `align` - Required alignment (must be a power of 2)
    ///
    /// # Returns
    ///
    /// A pointer to the allocated memory, or null on failure.
    /// The memory is zero-initialized.
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

crate::define_for_all_platforms! {
    /// Free memory previously allocated by `__rue_alloc`.
    ///
    /// # Arguments
    ///
    /// * `ptr` - Pointer to the memory to free
    /// * `size` - Size supplied by the runtime allocation ABI
    /// * `align` - Alignment supplied by the runtime allocation ABI
    ///
    /// # Bump-allocator contract
    ///
    /// This is a **no-op**. Memory is reclaimed when the program exits. The
    /// pointer, size, and alignment are retained in the generated-code ABI even
    /// though this allocator does not use them.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64)
    /// ```
    pub extern "C" fn __rue_free(ptr: *mut u8, size: u64, align: u64) {
        heap::free(ptr, size, align)
    }
}

crate::define_for_all_platforms! {
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
    /// - If `new_size <= old_size`: returns `ptr` unchanged
    /// - If `new_size > old_size`: allocates new block, copies data, returns new pointer
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
        heap::realloc(ptr, old_size, new_size, align)
    }
}

crate::define_for_all_platforms! {
    /// Drop a String, freeing its heap buffer if it was heap-allocated.
    ///
    /// # Arguments
    ///
    /// * `ptr` - Pointer to the string data
    /// * `len` - Length of the string (unused, but part of the String struct)
    /// * `cap` - Capacity of the buffer
    ///
    /// # Behavior
    ///
    /// - If `cap == 0`: The string is a literal pointing to rodata; do nothing.
    /// - If `cap > 0`: The string is heap-allocated; free the buffer.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_drop_String(ptr: *mut u8, len: u64, cap: u64)
    /// ```
    pub extern "C" fn __rue_drop_String(ptr: *mut u8, _len: u64, cap: u64) {
        // Only free heap-allocated strings (cap > 0)
        // Rodata strings have cap == 0 and must not be freed
        if cap > 0 {
            heap::free(ptr, cap, 1);
        }
    }
}

// =============================================================================
// String Construction Functions
// =============================================================================

/// Create an empty String with no allocation.
///
/// Returns an empty String (ptr=null, len=0, cap=0). This represents an empty
/// string that points to no data. Any mutation will trigger heap allocation.
///
/// # ABI (sret convention)
///
/// ```text
/// extern "C" fn __rue_String_new(out: *mut StringResult)
/// ```
///
/// Caller allocates space for the return value and passes pointer.
/// Callee writes (ptr=0, len=0, cap=0) to that pointer.
///
/// # Safety
///
/// `out` must be valid, aligned, writable storage for one [`StringResult`]
/// and must remain exclusively accessible for the call.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn __rue_String_new(out: *mut StringResult) {
    // SAFETY: Writing to `out` is safe because:
    // - Caller (Rue-generated code) allocates stack space and passes a valid pointer
    // - The sret convention guarantees `out` points to properly sized/aligned memory
    // - We have exclusive write access to this memory
    unsafe {
        (*out).ptr = core::ptr::null_mut();
        (*out).len = 0;
        (*out).cap = 0;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid, aligned, exclusively writable [`StringResult`] storage.
pub unsafe extern "C" fn __rue_String_new(out: *mut StringResult) {
    unsafe {
        (*out).ptr = core::ptr::null_mut();
        (*out).len = 0;
        (*out).cap = 0;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid, aligned, exclusively writable [`StringResult`] storage.
pub unsafe extern "C" fn __rue_String_new(out: *mut StringResult) {
    unsafe {
        (*out).ptr = core::ptr::null_mut();
        (*out).len = 0;
        (*out).cap = 0;
    }
}

/// Create an empty String with pre-allocated capacity.
///
/// Allocates a heap buffer with the given capacity (at least STRING_MIN_CAPACITY).
/// Returns a String with len=0 but capacity available for appending.
///
/// # ABI (sret convention)
///
/// ```text
/// extern "C" fn __rue_String_with_capacity(out: *mut StringResult, cap: u64)
/// ```
///
/// # Safety
///
/// `out` must be valid, aligned, writable storage for one [`StringResult`]
/// and must remain exclusively accessible for the call.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn __rue_String_with_capacity(out: *mut StringResult, requested_cap: u64) {
    if requested_cap == 0 {
        unsafe { __rue_String_new(out) };
        return;
    }
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    if ptr.is_null() {
        crate::error::allocation_failure();
    }
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid, aligned, exclusively writable [`StringResult`] storage.
pub unsafe extern "C" fn __rue_String_with_capacity(out: *mut StringResult, requested_cap: u64) {
    if requested_cap == 0 {
        unsafe { __rue_String_new(out) };
        return;
    }
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    if ptr.is_null() {
        crate::error::allocation_failure();
    }
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid, aligned, exclusively writable [`StringResult`] storage.
pub unsafe extern "C" fn __rue_String_with_capacity(out: *mut StringResult, requested_cap: u64) {
    if requested_cap == 0 {
        unsafe { __rue_String_new(out) };
        return;
    }
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    if ptr.is_null() {
        crate::error::allocation_failure();
    }
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}
// =============================================================================
// String query methods
// =============================================================================
//
// These methods take a String (ptr, len, cap) and return a single value.
// They use `borrow self` semantics - the String is not consumed.
//
// ABI: String is passed as 3 separate arguments (ptr, len, cap)
// - x86-64: ptr in rdi, len in rsi, cap in rdx
// - aarch64: ptr in x0, len in x1, cap in x2
//
// Return value is in rax (x86-64) or x0 (aarch64)

/// Get the length of a String in bytes.
///
/// # Arguments
/// * `_ptr` - Pointer to string data (unused, but part of ABI)
/// * `len` - Length in bytes
/// * `_cap` - Capacity (unused, but part of ABI)
///
/// # Returns
/// The length in bytes (u64)
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_String_len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    len
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_String_len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    len
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_String_len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    len
}

/// Get the capacity of a String in bytes.
///
/// Returns 0 for string literals (pointing to rodata).
/// Returns the allocated heap capacity for mutable strings.
///
/// # Arguments
/// * `_ptr` - Pointer to string data (unused, but part of ABI)
/// * `_len` - Length (unused, but part of ABI)
/// * `cap` - Capacity in bytes
///
/// # Returns
/// The capacity in bytes (u64)
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_String_capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
    cap
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_String_capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
    cap
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_String_capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
    cap
}

/// Check if a String is empty.
///
/// # Arguments
/// * `_ptr` - Pointer to string data (unused, but part of ABI)
/// * `len` - Length in bytes
/// * `_cap` - Capacity (unused, but part of ABI)
///
/// # Returns
/// 1 (true) if len == 0, 0 (false) otherwise
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_String_is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    if len == 0 { 1 } else { 0 }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_String_is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    if len == 0 { 1 } else { 0 }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __rue_String_is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    if len == 0 { 1 } else { 0 }
}

// =============================================================================
// String byte access (ADR-0035)
// =============================================================================
//
// Rue's String is a byte string (conventionally UTF-8, not guaranteed valid),
// so byte-level access looks INSIDE the raw bytes. Both operations below take
// a String by borrow (ptr, len, cap) and bounds-check against `len`, trapping
// (exit 101, via `__rue_bounds_check`) on out-of-range access — exactly like
// array indexing. Neither respects UTF-8 char boundaries: any in-range byte
// index / byte range is valid.

crate::define_for_all_platforms! {
    /// Read the byte at `index` in a String, returning it as `u8`.
    ///
    /// Implements `s[i]` for String (ADR-0035). O(1): a single load from
    /// `ptr + index` after a bounds check. An `index >= len` traps at runtime
    /// (index-out-of-bounds panic, exit 101), matching array indexing.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_String_byte_at(ptr: *const u8, len: u64, cap: u64, index: u64) -> u64
    /// ```
    ///
    /// The String is passed by borrow as its three fields (ptr, len, cap);
    /// `cap` is unused but part of the ABI. The byte is returned in the return
    /// register (`rax` / `x0`).
    ///
    /// The result is the language-level `u8` `s[i]`, but the function returns
    /// `u64` deliberately: the Rue calling convention expects a sub-word return
    /// value to occupy the *entire* return register zero-extended (Rue's own
    /// functions emit `mov rax, <u8>`, and the caller reads the full register
    /// without re-narrowing). A Rust `-> u8` leaves the upper bits of `rax`
    /// unspecified, which would poison a later `u8` arithmetic overflow check
    /// on the value. Returning `u64` forces the byte into a clean, fully
    /// zero-extended register that honors that contract.
    ///
    /// # Safety
    ///
    /// When `len > 0`, `ptr` must be non-null and point to a valid buffer of at
    /// least `len` bytes; it may be null when `len == 0`. The in-range read
    /// below is then sound because the bounds check guarantees `index < len`.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_String_byte_at(ptr: *const u8, len: u64, _cap: u64, index: u64) -> u64 {
        if index >= len {
            // Never returns: prints "error: index out of bounds" and exits 101.
            crate::error::__rue_bounds_check();
        }
        // SAFETY: `index < len` (checked above) and the caller guarantees `ptr`
        // is valid for `len` bytes, so `ptr.add(index)` is in bounds. u8 has no
        // alignment requirement. Zero-extend to u64 so the whole return
        // register is clean (see ABI note above).
        unsafe { *ptr.add(index as usize) as u64 }
    }
}

crate::define_for_all_platforms! {
    /// Read the byte at `index` in a `str`, returning it as `u8` (ADR-0043
    /// ADR-0043, RUE-324).
    ///
    /// Implements `s[i]` for the `str` string type. A `str` is `[u8]` + UTF-8:
    /// its bytes are PACKED (1 byte each, in `.rodata` for a literal), so like
    /// `String` this reads a single byte at `ptr + index` after a bounds check.
    /// It is the 2-word (`{ptr, len}`) analog of `__rue_String_byte_at`, without
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
    /// `__rue_String_byte_at` ABI note on why a clean full register is required.
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
        // register is clean (see the `__rue_String_byte_at` ABI note).
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

crate::define_for_all_platforms! {
    /// Decode the Unicode scalar of the character starting at byte `offset`.
    ///
    /// Backs the element projection of `for c in s.chars()` (RUE-220). Returns
    /// the scalar zero-extended into `u64` (the language-level type is `u32`;
    /// see the `__rue_String_byte_at` ABI note on why a clean full register is
    /// returned). Traps on invalid UTF-8.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_String_char_scalar(ptr: *const u8, len: u64, cap: u64, offset: u64) -> u64
    /// ```
    ///
    /// # Safety
    ///
    /// When `len > 0`, `ptr` must be non-null and valid for `len` bytes; it may
    /// be null when `len == 0`.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_String_char_scalar(
        ptr: *const u8,
        len: u64,
        _cap: u64,
        offset: u64,
    ) -> u64 {
        // SAFETY: the for-loop desugaring passes the String's own (ptr, len).
        let (scalar, _width) = unsafe { __rue_decode_utf8_at(ptr, len, offset) };
        scalar as u64
    }
}

crate::define_for_all_platforms! {
    /// Return the byte offset of the character AFTER the one at `offset`.
    ///
    /// Backs the position advance of `for c in s.chars()` (RUE-220): the next
    /// position is `offset + utf8_width`. Traps on invalid UTF-8.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_String_char_next(ptr: *const u8, len: u64, cap: u64, offset: u64) -> u64
    /// ```
    ///
    /// # Safety
    ///
    /// When `len > 0`, `ptr` must be non-null and valid for `len` bytes; it may
    /// be null when `len == 0`.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_String_char_next(
        ptr: *const u8,
        len: u64,
        _cap: u64,
        offset: u64,
    ) -> u64 {
        // SAFETY: the for-loop desugaring passes the String's own (ptr, len).
        let (_scalar, width) = unsafe { __rue_decode_utf8_at(ptr, len, offset) };
        offset + width
    }
}

crate::define_for_all_platforms! {
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

crate::define_for_all_platforms! {
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

crate::define_for_all_platforms! {
    /// Decode the Unicode scalar at byte `offset`, replacing invalid UTF-8.
    ///
    /// Backs the element projection of `for c in s.chars_lossy()` (RUE-17
    /// RUE-17). Like [`__rue_String_char_scalar`] but never traps: an invalid
    /// sequence yields `U+FFFD` (see [`__rue_decode_utf8_lossy_at`]). Returns
    /// the scalar zero-extended into `u64` (language-level type `u32`).
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_String_char_scalar_lossy(ptr: *const u8, len: u64, cap: u64, offset: u64) -> u64
    /// ```
    ///
    /// # Safety
    ///
    /// When `len > 0`, `ptr` must be non-null and valid for `len` bytes; it may
    /// be null when `len == 0`.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_String_char_scalar_lossy(
        ptr: *const u8,
        len: u64,
        _cap: u64,
        offset: u64,
    ) -> u64 {
        // SAFETY: the for-loop desugaring passes the String's own (ptr, len).
        let (scalar, _width) = unsafe { __rue_decode_utf8_lossy_at(ptr, len, offset) };
        scalar as u64
    }
}

crate::define_for_all_platforms! {
    /// Return the byte offset of the character AFTER the one at `offset`,
    /// replacing invalid UTF-8.
    ///
    /// Backs the position advance of `for c in s.chars_lossy()` (RUE-17
    /// RUE-17). Like [`__rue_String_char_next`] but never traps: the advance
    /// past an invalid sequence is the width of its maximal subpart (see
    /// [`__rue_decode_utf8_lossy_at`]), guaranteeing forward progress.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_String_char_next_lossy(ptr: *const u8, len: u64, cap: u64, offset: u64) -> u64
    /// ```
    ///
    /// # Safety
    ///
    /// When `len > 0`, `ptr` must be non-null and valid for `len` bytes; it may
    /// be null when `len == 0`.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_String_char_next_lossy(
        ptr: *const u8,
        len: u64,
        _cap: u64,
        offset: u64,
    ) -> u64 {
        // SAFETY: the for-loop desugaring passes the String's own (ptr, len).
        let (_scalar, width) = unsafe { __rue_decode_utf8_lossy_at(ptr, len, offset) };
        offset + width
    }
}

crate::define_for_all_platforms! {
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

crate::define_for_all_platforms! {
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

crate::define_for_all_platforms! {
    /// Return a NEW String containing the byte range `[start, start + sub_len)`.
    ///
    /// Implements `s.substring(start, sub_len)` (ADR-0035). Because String is a
    /// byte string, ANY byte range is valid (no char-boundary trap, unlike
    /// Rust's `str` slicing); only an out-of-range range (`start + sub_len >
    /// len`, or an overflowing `start + sub_len`) traps (exit 101). The
    /// returned String owns a fresh heap allocation holding a copy of the
    /// bytes, so it can be mutated and dropped independently of the source.
    ///
    /// # ABI (sret convention)
    ///
    /// ```text
    /// extern "C" fn __rue_String_substring(
    ///     out: *mut StringResult, ptr: *const u8, len: u64, cap: u64,
    ///     start: u64, sub_len: u64)
    /// ```
    ///
    /// `out` (the sret pointer) comes first, then the source String's fields
    /// (`cap` unused), then the range arguments.
    ///
    /// # Safety
    ///
    /// `out` must be valid, aligned, exclusively writable storage for one
    /// [`StringResult`]. When `len > 0`, `ptr` must be non-null and valid for
    /// reads of `len` bytes; it may be null when `len == 0`. It must not overlap
    /// `out`.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_String_substring(
        out: *mut StringResult,
        ptr: *const u8,
        len: u64,
        _cap: u64,
        start: u64,
        sub_len: u64,
    ) {
        // Out-of-range (or overflowing) range traps, matching array indexing.
        match start.checked_add(sub_len) {
            Some(end) if end <= len => {}
            _ => crate::error::__rue_bounds_check(),
        }

        if sub_len == 0 {
            // Empty result: a valid literal-like String (cap == 0, ptr null)
            // that drops without freeing.
            // SAFETY: `out` is a valid sret pointer — see __rue_String_new.
            unsafe {
                (*out).ptr = core::ptr::null_mut();
                (*out).len = 0;
                (*out).cap = 0;
            }
            return;
        }

        let new_cap = if sub_len < STRING_MIN_CAPACITY {
            STRING_MIN_CAPACITY
        } else {
            sub_len
        };
        let new_ptr = heap::alloc(new_cap, 1);

        if new_ptr.is_null() {
            crate::error::allocation_failure();
        }

        // SAFETY: `start + sub_len <= len` (checked above) so the source range
        // is in bounds; `new_ptr` was just allocated with `new_cap >= sub_len`
        // bytes; the regions don't overlap (fresh allocation).
        unsafe {
            core::ptr::copy_nonoverlapping(ptr.add(start as usize), new_ptr, sub_len as usize);
            (*out).ptr = new_ptr;
            (*out).len = sub_len;
            (*out).cap = new_cap;
        }
    }
}

// =============================================================================
// Byte-string search predicates (ADR-0035)
// =============================================================================
//
// `s.contains(needle)` and `s.starts_with(prefix)` are byte-level: they compare
// raw bytes and never inspect UTF-8 character boundaries. Both take the receiver
// and argument String by borrow (as three fields each: ptr, len, cap; the caps
// are part of the ABI but unused) and return `u64` (0 or 1) in the full return
// register. This matches the clean-return-register convention documented for
// `__rue_String_byte_at`: generated code consumes these booleans as full-slot
// values, so the runtime must not leave upper return bits unspecified. The
// empty needle / empty prefix always matches.

crate::define_for_all_platforms! {
    /// True (1) iff the bytes of `needle` occur as a contiguous run inside the
    /// receiver's bytes. Uses a naive O(len * needle_len) scan; the empty
    /// needle is always contained.
    ///
    /// # Safety
    ///
    /// Each pointer must be non-null and valid for its associated length when
    /// that length is positive; either pointer may be null when its length is
    /// zero. Every read below is bounded by those lengths.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_String_contains(
        ptr: *const u8,
        len: u64,
        _cap: u64,
        needle_ptr: *const u8,
        needle_len: u64,
        _needle_cap: u64,
    ) -> u64 {
        // The empty needle is contained in every string.
        if needle_len == 0 {
            return 1;
        }
        // A needle longer than the haystack cannot occur.
        if needle_len > len {
            return 0;
        }
        // Try every start offset that still leaves room for the whole needle.
        let last_start = len - needle_len;
        let mut start: u64 = 0;
        while start <= last_start {
            let mut k: u64 = 0;
            while k < needle_len {
                // SAFETY: start + k <= last_start + (needle_len - 1) = len - 1,
                // so the haystack read is in bounds; k < needle_len bounds the
                // needle read. u8 has no alignment requirement.
                let a = unsafe { *ptr.add((start + k) as usize) };
                let b = unsafe { *needle_ptr.add(k as usize) };
                if a != b {
                    break;
                }
                k += 1;
            }
            if k == needle_len {
                return 1;
            }
            start += 1;
        }
        0
    }
}

crate::define_for_all_platforms! {
    /// True (1) iff the bytes of `prefix` are a prefix of the receiver's bytes.
    /// The empty prefix always matches.
    ///
    /// # Safety
    ///
    /// Each pointer must be non-null and valid for its associated length when
    /// that length is positive; either pointer may be null when its length is
    /// zero. Every read below is bounded by those lengths.
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_String_starts_with(
        ptr: *const u8,
        len: u64,
        _cap: u64,
        prefix_ptr: *const u8,
        prefix_len: u64,
        _prefix_cap: u64,
    ) -> u64 {
        // The empty prefix matches every string.
        if prefix_len == 0 {
            return 1;
        }
        // A prefix longer than the string cannot match.
        if prefix_len > len {
            return 0;
        }
        let mut k: u64 = 0;
        while k < prefix_len {
            // SAFETY: k < prefix_len <= len, so both reads are in bounds. u8 has
            // no alignment requirement.
            let a = unsafe { *ptr.add(k as usize) };
            let b = unsafe { *prefix_ptr.add(k as usize) };
            if a != b {
                return 0;
            }
            k += 1;
        }
        1
    }
}

// =============================================================================
// Integer-to-String formatting (ADR-0035)
// =============================================================================

crate::define_for_all_platforms! {
    /// Format an `i64` in base 10 into a freshly heap-allocated `String`.
    ///
    /// Implements the `@to_string(n)` intrinsic. Handles the full `i64` range,
    /// including `i64::MIN` (whose magnitude is not representable as a positive
    /// `i64`), and prefixes negative values with `'-'`.
    ///
    /// # ABI (sret convention)
    ///
    /// ```text
    /// extern "C" fn __rue_to_string(out: *mut StringResult, n: i64)
    /// ```
    ///
    /// `out` (the sret pointer) comes first, then the integer argument. The
    /// returned String owns a fresh heap buffer, so it can be mutated and
    /// dropped independently.
    ///
    /// # Safety
    ///
    /// `out` must be a valid sret pointer — see `__rue_String_new`.
    pub unsafe extern "C" fn __rue_to_string(out: *mut StringResult, n: i64) {
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
        // pointer (see __rue_String_new).
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr().add(i), new_ptr, len as usize);
            (*out).ptr = new_ptr;
            (*out).len = len;
            (*out).cap = new_cap;
        }
    }
}

crate::define_for_all_platforms! {
    /// Format a `u64` in base 10 into a freshly heap-allocated `String`.
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
    /// extern "C" fn __rue_to_string_unsigned(out: *mut StringResult, n: u64)
    /// ```
    ///
    /// `out` (the sret pointer) comes first, then the integer argument. The
    /// returned String owns a fresh heap buffer, so it can be mutated and
    /// dropped independently.
    ///
    /// # Safety
    ///
    /// `out` must be a valid sret pointer — see `__rue_String_new`.
    pub unsafe extern "C" fn __rue_to_string_unsigned(out: *mut StringResult, n: u64) {
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
        // pointer (see __rue_String_new).
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr().add(i), new_ptr, len as usize);
            (*out).ptr = new_ptr;
            (*out).len = len;
            (*out).cap = new_cap;
        }
    }
}

crate::define_for_all_platforms! {
    /// Return a NEW String whose bytes are the concatenation of two Strings.
    ///
    /// Implements `s1 + s2` (RUE-17, ADR-0035). Both operands are
    /// borrowed (passed as their three fields each; the caps are unused but part
    /// of the ABI) and neither is consumed. The result owns a fresh heap buffer.
    ///
    /// # ABI (sret convention)
    ///
    /// ```text
    /// extern "C" fn __rue_String_concat(
    ///     out: *mut StringResult,
    ///     ptr1: *const u8, len1: u64, cap1: u64,
    ///     ptr2: *const u8, len2: u64, cap2: u64)
    /// ```
    ///
    /// # Safety
    ///
    /// Each input pointer must be non-null and valid for its associated length
    /// when that length is positive; it may be null when its length is zero.
    /// `out` must be a valid sret pointer (see `__rue_String_new`).
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn __rue_String_concat(
        out: *mut StringResult,
        ptr1: *const u8,
        len1: u64,
        _cap1: u64,
        ptr2: *const u8,
        len2: u64,
        _cap2: u64,
    ) {
        let Some(total) = len1.checked_add(len2) else {
            crate::error::allocation_failure();
        };

        if total == 0 {
            // Empty result: a valid literal-like String (cap == 0, ptr null)
            // that drops without freeing.
            // SAFETY: `out` is a valid sret pointer — see __rue_String_new.
            unsafe {
                (*out).ptr = core::ptr::null_mut();
                (*out).len = 0;
                (*out).cap = 0;
            }
            return;
        }

        let new_cap = if total < STRING_MIN_CAPACITY {
            STRING_MIN_CAPACITY
        } else {
            total
        };
        let new_ptr = heap::alloc(new_cap, 1);

        if new_ptr.is_null() {
            crate::error::allocation_failure();
        }

        // SAFETY: `new_ptr` has `new_cap >= total = len1 + len2` bytes. The two
        // source ranges are each in bounds for their lengths and don't overlap
        // the fresh allocation; `new_ptr.add(len1)` is the first byte after the
        // copied prefix, leaving room for `len2` more bytes.
        unsafe {
            if len1 > 0 && !ptr1.is_null() {
                core::ptr::copy_nonoverlapping(ptr1, new_ptr, len1 as usize);
            }
            if len2 > 0 && !ptr2.is_null() {
                core::ptr::copy_nonoverlapping(ptr2, new_ptr.add(len1 as usize), len2 as usize);
            }
            (*out).ptr = new_ptr;
            (*out).len = total;
            (*out).cap = new_cap;
        }
    }
}

// =============================================================================
// String clone method
// =============================================================================
//
// Clone creates a deep copy of a String. It uses `borrow self` semantics -
// the original String is not consumed.
//
// ABI (sret convention): out pointer first, then String fields (ptr, len, cap)

/// Clone a String, creating a deep copy.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the source string data
/// * `len` - Length of the string in bytes
/// * `_cap` - Capacity (unused for cloning, but part of ABI)
///
/// # Behavior
///
/// Always allocates a new heap buffer, even for literals (cap == 0).
/// The clone is always heap-allocated so it can be mutated.
///
/// # Safety
///
/// `out` must be valid, aligned, exclusively writable storage for one
/// [`StringResult`]. When `len > 0`, `ptr` must be non-null and valid for reads
/// of `len` bytes; it may be null when `len == 0`. It must not overlap `out`.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn __rue_String_clone(
    out: *mut StringResult,
    ptr: *const u8,
    len: u64,
    _cap: u64,
) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    if new_ptr.is_null() {
        crate::error::allocation_failure();
    }

    if len > 0 && !ptr.is_null() {
        // SAFETY: Copying is safe because:
        // - Caller guarantees `ptr` is valid for reads of `len` bytes
        // - `new_ptr` was just allocated with at least `len` bytes capacity
        // - The regions don't overlap (new_ptr is freshly allocated)
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage. When `len > 0`, `ptr` must be non-null
/// and readable for `len` bytes; it may be null when `len == 0`. Their storage
/// must not overlap.
pub unsafe extern "C" fn __rue_String_clone(
    out: *mut StringResult,
    ptr: *const u8,
    len: u64,
    _cap: u64,
) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    if new_ptr.is_null() {
        crate::error::allocation_failure();
    }

    if len > 0 && !ptr.is_null() {
        // SAFETY: Copying is safe because:
        // - Caller guarantees `ptr` is valid for reads of `len` bytes
        // - `new_ptr` was just allocated with at least `len` bytes capacity
        // - The regions don't overlap (new_ptr is freshly allocated)
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage. When `len > 0`, `ptr` must be non-null
/// and readable for `len` bytes; it may be null when `len == 0`. Their storage
/// must not overlap.
pub unsafe extern "C" fn __rue_String_clone(
    out: *mut StringResult,
    ptr: *const u8,
    len: u64,
    _cap: u64,
) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    if new_ptr.is_null() {
        crate::error::allocation_failure();
    }

    if len > 0 && !ptr.is_null() {
        // SAFETY: Copying is safe because:
        // - Caller guarantees `ptr` is valid for reads of `len` bytes
        // - `new_ptr` was just allocated with at least `len` bytes capacity
        // - The regions don't overlap (new_ptr is freshly allocated)
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

// =============================================================================
// String mutation methods: push_str, push, clear, reserve
// =============================================================================
//
// These methods take a String (ptr, len, cap) and additional arguments,
// then return an updated String (ptr, len, cap) via sret convention.
// They use `inout self` semantics - the String is modified in place.
//
// ABI (sret convention): out pointer first, then String fields and other args
//
// Heap promotion: If cap == 0, the string is a literal pointing to rodata.
// Any mutation first promotes to heap by allocating a new buffer and copying.

/// Append another string's content to this string.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the string data
/// * `len` - Current length in bytes
/// * `cap` - Current capacity (0 for literals)
/// * `other_ptr` - Pointer to the other string's data
/// * `other_len` - Length of the other string
/// * `_other_cap` - Capacity of the other string (unused, but part of ABI)
///
/// # Safety
///
/// `out` must be valid, aligned, exclusively writable storage for one
/// [`StringResult`]. The receiver fields must describe a live String allocation
/// (or a `cap == 0` literal), and `other_ptr` must be readable for `other_len`
/// bytes. The source ranges and `out` must not overlap writes performed here.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn __rue_String_push_str(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    other_ptr: *const u8,
    other_len: u64,
    _other_cap: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, other_len);

    if other_len > 0 && !other_ptr.is_null() {
        // SAFETY: Copying is safe because:
        // - Caller guarantees `other_ptr` is valid for reads of `other_len` bytes
        // - `string_ensure_capacity` guarantees `new_ptr` has room for `len + other_len` bytes
        // - `new_ptr.add(len)` points to the first unused byte after existing content
        // - The regions don't overlap (other is a separate String)
        unsafe {
            core::ptr::copy_nonoverlapping(
                other_ptr,
                new_ptr.add(len as usize),
                other_len as usize,
            );
        }
    }

    let new_len = len + other_len;

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage; both String field triples must describe
/// live storage for their lengths/capacities and must not overlap writes. A
/// pointer may be null only for the canonical empty `(null, 0, 0)` String.
pub unsafe extern "C" fn __rue_String_push_str(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    other_ptr: *const u8,
    other_len: u64,
    _other_cap: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, other_len);

    if other_len > 0 && !other_ptr.is_null() {
        // SAFETY: Copying is safe because:
        // - Caller guarantees `other_ptr` is valid for reads of `other_len` bytes
        // - `string_ensure_capacity` guarantees `new_ptr` has room for `len + other_len` bytes
        // - `new_ptr.add(len)` points to the first unused byte after existing content
        // - The regions don't overlap (other is a separate String)
        unsafe {
            core::ptr::copy_nonoverlapping(
                other_ptr,
                new_ptr.add(len as usize),
                other_len as usize,
            );
        }
    }

    let new_len = len + other_len;

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage; both String field triples must describe
/// live storage for their lengths/capacities and must not overlap writes. A
/// pointer may be null only for the canonical empty `(null, 0, 0)` String.
pub unsafe extern "C" fn __rue_String_push_str(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    other_ptr: *const u8,
    other_len: u64,
    _other_cap: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, other_len);

    if other_len > 0 && !other_ptr.is_null() {
        // SAFETY: Copying is safe because:
        // - Caller guarantees `other_ptr` is valid for reads of `other_len` bytes
        // - `string_ensure_capacity` guarantees `new_ptr` has room for `len + other_len` bytes
        // - `new_ptr.add(len)` points to the first unused byte after existing content
        // - The regions don't overlap (other is a separate String)
        unsafe {
            core::ptr::copy_nonoverlapping(
                other_ptr,
                new_ptr.add(len as usize),
                other_len as usize,
            );
        }
    }

    let new_len = len + other_len;

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

/// Append a single byte to this string.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the string data
/// * `len` - Current length in bytes
/// * `cap` - Current capacity (0 for literals)
/// * `byte` - The byte to append
///
/// # Safety
///
/// `out` must be valid, aligned, exclusively writable storage for one
/// [`StringResult`]. The receiver fields must describe a live String allocation
/// (or a `cap == 0` literal), and `out` must not overlap that allocation.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn __rue_String_push(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    byte: u8,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    // SAFETY: Writing the byte is safe because:
    // - `string_ensure_capacity` guarantees `new_ptr` has room for `len + 1` bytes
    // - `new_ptr.add(len)` points to the first unused byte after existing content
    // - u8 has no alignment requirements
    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    let new_len = len + 1;

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage; `ptr`/`len`/`cap` must describe a live
/// String and must not overlap `out`. `ptr` may be null only for the canonical
/// empty `(null, 0, 0)` String.
pub unsafe extern "C" fn __rue_String_push(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    byte: u8,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    // SAFETY: Writing the byte is safe because:
    // - `string_ensure_capacity` guarantees `new_ptr` has room for `len + 1` bytes
    // - `new_ptr.add(len)` points to the first unused byte after existing content
    // - u8 has no alignment requirements
    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    let new_len = len + 1;

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage; `ptr`/`len`/`cap` must describe a live
/// String and must not overlap `out`. `ptr` may be null only for the canonical
/// empty `(null, 0, 0)` String.
pub unsafe extern "C" fn __rue_String_push(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    byte: u8,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    // SAFETY: Writing the byte is safe because:
    // - `string_ensure_capacity` guarantees `new_ptr` has room for `len + 1` bytes
    // - `new_ptr.add(len)` points to the first unused byte after existing content
    // - u8 has no alignment requirements
    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    let new_len = len + 1;

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

/// Clear the string content, keeping capacity.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the string data
/// * `_len` - Current length in bytes (unused)
/// * `cap` - Current capacity
///
/// # Safety
///
/// `out` must be valid, aligned, exclusively writable storage for one
/// [`StringResult`] and must not overlap the live String described by
/// `ptr`/`cap`.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn __rue_String_clear(
    out: *mut StringResult,
    ptr: *mut u8,
    _len: u64,
    cap: u64,
) {
    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage and must not overlap the live String
/// described by `ptr`/`cap`. `ptr` may be null for the canonical empty
/// `(null, 0, 0)` String.
pub unsafe extern "C" fn __rue_String_clear(
    out: *mut StringResult,
    ptr: *mut u8,
    _len: u64,
    cap: u64,
) {
    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage and must not overlap the live String
/// described by `ptr`/`cap`. `ptr` may be null for the canonical empty
/// `(null, 0, 0)` String.
pub unsafe extern "C" fn __rue_String_clear(
    out: *mut StringResult,
    ptr: *mut u8,
    _len: u64,
    cap: u64,
) {
    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = cap;
    }
}

/// Reserve additional capacity in the string.
///
/// # Arguments
///
/// * `out` - Pointer to StringResult where result will be written
/// * `ptr` - Pointer to the string data
/// * `len` - Current length in bytes
/// * `cap` - Current capacity (0 for literals)
/// * `additional` - Number of additional bytes to reserve
///
/// # Safety
///
/// `out` must be valid, aligned, exclusively writable storage for one
/// [`StringResult`]. The receiver fields must describe a live String allocation
/// (or a `cap == 0` literal), and `out` must not overlap that allocation.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn __rue_String_reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len; // len stays the same for reserve
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage; `ptr`/`len`/`cap` must describe a live
/// String and must not overlap `out`. `ptr` may be null only for the canonical
/// empty `(null, 0, 0)` String.
pub unsafe extern "C" fn __rue_String_reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// # Safety
///
/// `out` must be valid result storage; `ptr`/`len`/`cap` must describe a live
/// String and must not overlap `out`. `ptr` may be null only for the canonical
/// empty `(null, 0, 0)` String.
pub unsafe extern "C" fn __rue_String_reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    // SAFETY: Writing to `out` is safe - see __rue_String_new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

/// Helper function to ensure a string has enough capacity for additional bytes.
///
/// Handles heap promotion (cap == 0) and growth.
///
/// # Arguments
///
/// * `ptr` - Current pointer
/// * `len` - Current length
/// * `cap` - Current capacity (0 for literals)
/// * `additional` - Number of additional bytes needed
///
/// # Returns
///
/// (new_ptr, new_cap) with capacity >= len + additional.
/// Allocation or capacity overflow aborts through the canonical allocation
/// failure path. A zero-sized request preserves the original empty value.
#[inline]
fn string_ensure_capacity(ptr: *mut u8, len: u64, cap: u64, additional: u64) -> (*mut u8, u64) {
    let Some(required) = len.checked_add(additional) else {
        crate::error::allocation_failure();
    };

    if required == 0 {
        return (ptr, cap);
    }

    if cap == 0 {
        // Heap promotion: allocate new buffer and copy existing content
        let new_cap = required.max(STRING_MIN_CAPACITY);
        let new_ptr = heap::alloc(new_cap, 1);
        if new_ptr.is_null() {
            crate::error::allocation_failure();
        }
        if len > 0 && !ptr.is_null() {
            // SAFETY: Copying is safe because:
            // - `ptr` is valid for reads of `len` bytes (string content from rodata or heap)
            // - `new_ptr` was just allocated with at least `len` bytes
            // - The regions don't overlap (new_ptr is freshly allocated from heap,
            //   ptr points to either rodata or a different heap allocation)
            unsafe {
                core::ptr::copy_nonoverlapping(ptr as *const u8, new_ptr, len as usize);
            }
        }
        (new_ptr, new_cap)
    } else if required > cap {
        // Need to grow. The recorded capacity must equal bytes actually owned,
        // so compute the doubled capacity first and allocate exactly that
        // amount. Otherwise a later append could trust nonexistent capacity
        // and write past the allocation (RUE-34).
        let Some(grown_cap) = cap.checked_mul(2) else {
            crate::error::allocation_failure();
        };
        let new_cap = required.max(grown_cap).max(STRING_MIN_CAPACITY);
        let new_ptr = heap::realloc(ptr, cap, new_cap, 1);
        if new_ptr.is_null() {
            // `realloc` leaves the old allocation valid on failure. Do not
            // publish a null replacement or release the original before the
            // canonical trap.
            crate::error::allocation_failure();
        }
        (new_ptr, new_cap)
    } else {
        // Capacity is sufficient
        (ptr, cap)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::process::Command;
    use super::*;

    fn blank_result() -> StringResult {
        StringResult {
            ptr: core::ptr::null_mut(),
            len: u64::MAX,
            cap: u64::MAX,
        }
    }

    fn result_bytes(result: &StringResult) -> &[u8] {
        if result.len == 0 {
            &[]
        } else {
            assert!(!result.ptr.is_null());
            // SAFETY: Runtime constructors tested here publish `ptr` with at
            // least `len` initialized bytes. For empty strings we return above
            // to avoid forming a slice from a null pointer.
            unsafe { core::slice::from_raw_parts(result.ptr, result.len as usize) }
        }
    }

    fn assert_string_result(result: &StringResult, bytes: &[u8]) {
        assert_eq!(result.len, bytes.len() as u64);
        assert_eq!(result_bytes(result), bytes);
        if bytes.is_empty() {
            assert_eq!(result.cap, 0);
        } else {
            assert!(result.cap >= result.len);
        }
    }

    #[test]
    fn test_str_eq_compares_content_and_length() {
        // SAFETY: every pointer/length pair below comes from the live byte
        // arrays declared in this scope.
        unsafe {
            let a = b"same";
            let b = b"same";
            let c = b"same!";
            let d = b"diff";

            assert_eq!(
                __rue_str_eq(a.as_ptr(), a.len() as u64, b.as_ptr(), b.len() as u64),
                1
            );
            assert_eq!(
                __rue_str_eq(a.as_ptr(), a.len() as u64, c.as_ptr(), c.len() as u64),
                0
            );
            assert_eq!(
                __rue_str_eq(a.as_ptr(), a.len() as u64, d.as_ptr(), d.len() as u64),
                0
            );
            assert_eq!(__rue_str_eq(a.as_ptr(), 0, d.as_ptr(), 0), 1);
        }
    }

    #[test]
    fn test_string_new_and_query_methods() {
        let mut out = blank_result();

        // SAFETY: `out` is valid, aligned, exclusively borrowed result storage.
        unsafe { __rue_String_new(&mut out) };

        assert!(out.ptr.is_null());
        assert_eq!(out.len, 0);
        assert_eq!(out.cap, 0);
        assert_eq!(__rue_String_len(out.ptr, out.len, out.cap), 0);
        assert_eq!(__rue_String_capacity(out.ptr, out.len, out.cap), 0);
        assert_eq!(__rue_String_is_empty(out.ptr, out.len, out.cap), 1);

        let bytes = b"abc";
        assert_eq!(__rue_String_len(bytes.as_ptr(), bytes.len() as u64, 0), 3);
        assert_eq!(
            __rue_String_capacity(bytes.as_ptr(), bytes.len() as u64, 99),
            99
        );
        assert_eq!(
            __rue_String_is_empty(bytes.as_ptr(), bytes.len() as u64, 0),
            0
        );
    }

    #[test]
    fn test_string_with_capacity_allocates_at_least_minimum() {
        let mut out = blank_result();

        // SAFETY: `out` is valid, aligned, exclusively borrowed result storage.
        unsafe { __rue_String_with_capacity(&mut out, 3) };

        assert!(!out.ptr.is_null());
        assert_eq!(out.len, 0);
        assert!(out.cap >= STRING_MIN_CAPACITY);
    }

    #[test]
    fn test_string_with_zero_capacity_does_not_allocate() {
        let mut out = blank_result();

        // SAFETY: `out` is valid, aligned, exclusively borrowed result storage.
        unsafe { __rue_String_with_capacity(&mut out, 0) };

        assert!(out.ptr.is_null());
        assert_eq!(out.len, 0);
        assert_eq!(out.cap, 0);
    }

    #[test]
    fn test_ensure_capacity_promotes_literals_and_preserves_bytes() {
        let source = b"abc";

        let (ptr, cap) = string_ensure_capacity(source.as_ptr() as *mut u8, 3, 0, 2);

        assert!(!ptr.is_null());
        assert!(cap >= STRING_MIN_CAPACITY);
        assert!(cap >= 5);
        assert_ne!(ptr as *const u8, source.as_ptr());
        unsafe {
            assert_eq!(core::slice::from_raw_parts(ptr, 3), source);
        }
    }

    #[test]
    fn test_ensure_capacity_grows_real_allocation_to_recorded_capacity() {
        let ptr = heap::alloc(4, 1);
        assert!(!ptr.is_null());
        unsafe {
            core::ptr::copy_nonoverlapping(b"abcd".as_ptr(), ptr, 4);
        }

        let (new_ptr, new_cap) = string_ensure_capacity(ptr, 4, 4, 1);

        assert!(!new_ptr.is_null());
        assert!(new_cap >= 8);
        unsafe {
            assert_eq!(core::slice::from_raw_parts(new_ptr, 4), b"abcd");
            *new_ptr.add((new_cap - 1) as usize) = b'!';
            assert_eq!(*new_ptr.add((new_cap - 1) as usize), b'!');
        }
    }

    #[test]
    fn safe_allocating_paths_use_canonical_trap() {
        const CHILD_ENV: &str = "RUE_STRING_OOM_CHILD";
        if let Some(mode) = self::std::env::var_os(CHILD_ENV) {
            let mode = mode.to_string_lossy();
            crate::heap::fail_allocations_after_for_test(0);
            let mut out = blank_result();
            let one = b"x";
            unsafe {
                match mode.as_ref() {
                    "with_capacity" => __rue_String_with_capacity(&mut out, 8),
                    "substring" => __rue_String_substring(&mut out, one.as_ptr(), 1, 0, 0, 1),
                    "to_string" => __rue_to_string(&mut out, 1),
                    "to_string_unsigned" => __rue_to_string_unsigned(&mut out, 1),
                    "concat" => {
                        __rue_String_concat(&mut out, one.as_ptr(), 1, 0, one.as_ptr(), 1, 0)
                    }
                    "clone" => __rue_String_clone(&mut out, one.as_ptr(), 1, 0),
                    "clone_empty" => __rue_String_clone(&mut out, core::ptr::null(), 0, 0),
                    "push_str" => __rue_String_push_str(
                        &mut out,
                        core::ptr::null_mut(),
                        0,
                        0,
                        one.as_ptr(),
                        1,
                        0,
                    ),
                    "push" => __rue_String_push(&mut out, core::ptr::null_mut(), 0, 0, b'x'),
                    "reserve" => __rue_String_reserve(&mut out, core::ptr::null_mut(), 0, 0, 1),
                    "concat_overflow" => __rue_String_concat(
                        &mut out,
                        core::ptr::null(),
                        u64::MAX,
                        0,
                        core::ptr::null(),
                        1,
                        0,
                    ),
                    "growth_overflow" => {
                        let cap = (u64::MAX / 2) + 1;
                        let _ = string_ensure_capacity(core::ptr::null_mut(), cap, cap, 1);
                    }
                    other => panic!("unknown allocation failure mode: {other}"),
                }
            }
            unreachable!();
        }

        for mode in [
            "with_capacity",
            "substring",
            "to_string",
            "to_string_unsigned",
            "concat",
            "clone",
            "clone_empty",
            "push_str",
            "push",
            "reserve",
            "concat_overflow",
            "growth_overflow",
        ] {
            let output = Command::new(self::std::env::current_exe().expect("current test binary"))
                .args([
                    "--exact",
                    "string::tests::safe_allocating_paths_use_canonical_trap",
                    "--nocapture",
                ])
                .env(CHILD_ENV, mode)
                .output()
                .expect("spawn allocation-failure child");

            assert_eq!(output.status.code(), Some(101), "mode {mode}");
            assert_eq!(output.stderr, b"panic: out of memory\n", "mode {mode}");
        }
    }

    #[test]
    fn test_byte_access_returns_zero_extended_bytes() {
        let bytes = [0x00, 0x7f, 0x80, 0xff];

        // SAFETY: all accesses are within the live `bytes` array.
        unsafe {
            assert_eq!(
                __rue_String_byte_at(bytes.as_ptr(), bytes.len() as u64, 0, 0),
                0
            );
            assert_eq!(
                __rue_String_byte_at(bytes.as_ptr(), bytes.len() as u64, 0, 2),
                0x80
            );
            assert_eq!(
                __rue_str_byte_at(bytes.as_ptr(), bytes.len() as u64, 3),
                0xff
            );
        }
    }

    #[test]
    fn test_utf8_char_scalar_and_next_decode_multibyte_scalars() {
        let bytes = "aé🙂".as_bytes();

        // SAFETY: all offsets are scalar boundaries within the live UTF-8 buffer.
        unsafe {
            assert_eq!(
                __rue_String_char_scalar(bytes.as_ptr(), bytes.len() as u64, 0, 0),
                'a' as u64
            );
            assert_eq!(
                __rue_String_char_next(bytes.as_ptr(), bytes.len() as u64, 0, 0),
                1
            );
            assert_eq!(
                __rue_String_char_scalar(bytes.as_ptr(), bytes.len() as u64, 0, 1),
                'é' as u64
            );
            assert_eq!(
                __rue_String_char_next(bytes.as_ptr(), bytes.len() as u64, 0, 1),
                3
            );
            assert_eq!(
                __rue_String_char_scalar(bytes.as_ptr(), bytes.len() as u64, 0, 3),
                '🙂' as u64
            );
            assert_eq!(
                __rue_String_char_next(bytes.as_ptr(), bytes.len() as u64, 0, 3),
                bytes.len() as u64
            );
        }
    }

    #[test]
    fn test_utf8_lossy_replaces_invalid_sequences_and_advances() {
        const FFFD: u64 = 0xfffd;
        let bytes = [b'a', 0xf0, 0x9f, b'!', 0xff, b'z'];

        // SAFETY: all offsets are within the live `bytes` array; the lossy
        // functions do not require valid UTF-8.
        unsafe {
            assert_eq!(
                __rue_String_char_scalar_lossy(bytes.as_ptr(), bytes.len() as u64, 0, 0),
                b'a' as u64
            );
            assert_eq!(
                __rue_String_char_next_lossy(bytes.as_ptr(), bytes.len() as u64, 0, 0),
                1
            );
            assert_eq!(
                __rue_String_char_scalar_lossy(bytes.as_ptr(), bytes.len() as u64, 0, 1),
                FFFD
            );
            assert_eq!(
                __rue_String_char_next_lossy(bytes.as_ptr(), bytes.len() as u64, 0, 1),
                3
            );
            assert_eq!(
                __rue_String_char_scalar_lossy(bytes.as_ptr(), bytes.len() as u64, 0, 4),
                FFFD
            );
            assert_eq!(
                __rue_String_char_next_lossy(bytes.as_ptr(), bytes.len() as u64, 0, 4),
                5
            );
        }
    }

    #[test]
    fn test_substring_copies_requested_byte_range() {
        let bytes = b"abcdef";
        let mut out = blank_result();

        // SAFETY: `out` is valid result storage and `bytes` is live for its
        // reported length; the requested range is in bounds.
        unsafe { __rue_String_substring(&mut out, bytes.as_ptr(), bytes.len() as u64, 0, 2, 3) };

        assert_string_result(&out, b"cde");
        assert_ne!(out.ptr as *const u8, bytes.as_ptr());
    }

    #[test]
    fn test_substring_empty_range_is_literal_like_empty_string() {
        let bytes = b"abcdef";
        let mut out = blank_result();

        // SAFETY: `out` is valid result storage and the empty range is within
        // the live `bytes` buffer.
        unsafe { __rue_String_substring(&mut out, bytes.as_ptr(), bytes.len() as u64, 0, 3, 0) };

        assert!(out.ptr.is_null());
        assert_eq!(out.len, 0);
        assert_eq!(out.cap, 0);
    }

    #[test]
    fn test_contains_and_starts_with_are_byte_based() {
        let haystack = b"bananas";

        // SAFETY: every pointer/length pair below names a live byte array.
        unsafe {
            assert_eq!(
                __rue_String_contains(
                    haystack.as_ptr(),
                    haystack.len() as u64,
                    0,
                    b"nan".as_ptr(),
                    3,
                    0,
                ),
                1
            );
            assert_eq!(
                __rue_String_contains(
                    haystack.as_ptr(),
                    haystack.len() as u64,
                    0,
                    b"nab".as_ptr(),
                    3,
                    0,
                ),
                0
            );
            assert_eq!(
                __rue_String_starts_with(
                    haystack.as_ptr(),
                    haystack.len() as u64,
                    0,
                    b"ban".as_ptr(),
                    3,
                    0,
                ),
                1
            );
            assert_eq!(
                __rue_String_starts_with(
                    haystack.as_ptr(),
                    haystack.len() as u64,
                    0,
                    b"nan".as_ptr(),
                    3,
                    0,
                ),
                0
            );
            assert_eq!(
                __rue_String_contains(
                    haystack.as_ptr(),
                    haystack.len() as u64,
                    0,
                    b"".as_ptr(),
                    0,
                    0
                ),
                1
            );
            assert_eq!(
                __rue_String_starts_with(
                    haystack.as_ptr(),
                    haystack.len() as u64,
                    0,
                    b"".as_ptr(),
                    0,
                    0,
                ),
                1
            );
        }
    }

    #[test]
    fn test_to_string_formats_signed_extremes() {
        let mut out = blank_result();

        // SAFETY: `out` is valid, aligned, exclusively borrowed result storage.
        unsafe { __rue_to_string(&mut out, 0) };
        assert_string_result(&out, b"0");

        unsafe { __rue_to_string(&mut out, -42) };
        assert_string_result(&out, b"-42");

        unsafe { __rue_to_string(&mut out, i64::MIN) };
        assert_string_result(&out, b"-9223372036854775808");
    }

    #[test]
    fn test_to_string_unsigned_formats_full_u64_range() {
        let mut out = blank_result();

        // SAFETY: `out` is valid, aligned, exclusively borrowed result storage.
        unsafe { __rue_to_string_unsigned(&mut out, 0) };
        assert_string_result(&out, b"0");

        unsafe { __rue_to_string_unsigned(&mut out, u64::MAX) };
        assert_string_result(&out, b"18446744073709551615");
    }

    #[test]
    fn test_concat_allocates_joined_bytes() {
        let left = b"hello, ";
        let right = b"world";
        let mut out = blank_result();

        // SAFETY: `out` is valid result storage and both input buffers are
        // live for their reported lengths.
        unsafe {
            __rue_String_concat(
                &mut out,
                left.as_ptr(),
                left.len() as u64,
                0,
                right.as_ptr(),
                right.len() as u64,
                0,
            )
        };

        assert_string_result(&out, b"hello, world");
    }

    #[test]
    fn test_clone_deep_copies_literal_bytes() {
        let source = b"literal";
        let mut out = blank_result();

        // SAFETY: `out` is valid result storage and `source` is live for its
        // reported length.
        unsafe { __rue_String_clone(&mut out, source.as_ptr(), source.len() as u64, 0) };

        assert_string_result(&out, source);
        assert_ne!(out.ptr as *const u8, source.as_ptr());
    }

    #[test]
    fn test_clone_empty_allocates_mutable_storage() {
        let mut out = blank_result();

        // SAFETY: `out` is valid result storage; a null source pointer is
        // permitted when the source length is zero.
        unsafe { __rue_String_clone(&mut out, core::ptr::null(), 0, 0) };

        assert!(!out.ptr.is_null());
        assert_eq!(out.len, 0);
        assert!(out.cap >= STRING_MIN_CAPACITY);
    }

    #[test]
    fn test_push_promotes_literal_and_appends_byte() {
        let source = b"abc";
        let mut out = blank_result();

        // SAFETY: `out` is valid result storage and `source` is live for its
        // reported length. `cap == 0` identifies borrowed literal storage.
        unsafe {
            __rue_String_push(
                &mut out,
                source.as_ptr() as *mut u8,
                source.len() as u64,
                0,
                b'd',
            )
        };

        assert_string_result(&out, b"abcd");
        assert_ne!(out.ptr as *const u8, source.as_ptr());
    }

    #[test]
    fn test_push_str_appends_without_reallocation_when_capacity_suffices() {
        let mut base = blank_result();
        // SAFETY: `base` is valid, aligned, exclusively borrowed result storage.
        unsafe { __rue_String_with_capacity(&mut base, STRING_MIN_CAPACITY) };
        let start = base.ptr;
        unsafe {
            *base.ptr.add(0) = b'a';
            *base.ptr.add(1) = b'b';
        }
        base.len = 2;

        let mut out = blank_result();
        // SAFETY: `out` is valid result storage; both input buffers are live
        // for their reported lengths and the mutable buffer matches its cap.
        unsafe {
            __rue_String_push_str(&mut out, base.ptr, base.len, base.cap, b"cd".as_ptr(), 2, 0)
        };

        assert_eq!(out.ptr, start);
        assert_string_result(&out, b"abcd");
    }

    #[test]
    fn test_clear_preserves_capacity_and_reserve_grows() {
        let mut base = blank_result();
        // SAFETY: each out-pointer is valid result storage; the carried String
        // pointer, length, and capacity come from the preceding runtime call.
        unsafe { __rue_String_with_capacity(&mut base, 2) };
        let original_cap = base.cap;

        let mut reserved = blank_result();
        unsafe { __rue_String_reserve(&mut reserved, base.ptr, 0, base.cap, original_cap + 1) };
        assert_eq!(reserved.len, 0);
        assert!(reserved.cap > original_cap);

        let mut cleared = blank_result();
        unsafe { __rue_String_clear(&mut cleared, reserved.ptr, 7, reserved.cap) };
        assert_eq!(cleared.ptr, reserved.ptr);
        assert_eq!(cleared.len, 0);
        assert_eq!(cleared.cap, reserved.cap);
    }
}
