//! String runtime functions and methods.
//!
//! This module provides all runtime support for the String type:
//! - String equality comparison (`__rue_str_eq`)
//! - Heap allocation wrappers (`__rue_alloc`, `__rue_free`, `__rue_realloc`)
//! - String-specific allocation functions
//! - String constructors (`String__new`, `String__with_capacity`)
//! - String query methods (`len`, `capacity`, `is_empty`)
//! - String mutation methods (`push_str`, `push`, `clear`, `reserve`)
//! - String cloning (`String__clone`)
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
    /// extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u8
    /// ```
    ///
    /// - `ptr1` is passed in the first argument register (rdi on x86_64, x0 on aarch64)
    /// - `len1` is passed in the second argument register (rsi on x86_64, x1 on aarch64)
    /// - `ptr2` is passed in the third argument register (rdx on x86_64, x2 on aarch64)
    /// - `len2` is passed in the fourth argument register (rcx on x86_64, x3 on aarch64)
    /// - Returns 1 if strings are equal, 0 otherwise (in `al`/`w0` register)
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
    /// - `ptr1` points to a valid buffer of at least `len1` bytes
    /// - `ptr2` points to a valid buffer of at least `len2` bytes
    /// - Both pointers remain valid for the duration of the call
    pub extern "C" fn __rue_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u8 {
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
    /// * `size` - Size of the allocation (for future compatibility)
    /// * `align` - Alignment of the allocation (for future compatibility)
    ///
    /// # Current Implementation
    ///
    /// This is a **no-op** in the current bump allocator. Memory is only
    /// reclaimed when the program exits. The size and align parameters are
    /// accepted for API compatibility with future allocators.
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
    pub extern "C" fn __rue_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8 {
        heap::realloc(ptr, old_size, new_size, align)
    }
}

// =============================================================================
// String-Specific Allocation Functions
// =============================================================================

crate::define_for_all_platforms! {
    /// Allocate a new string buffer with the given capacity.
    ///
    /// # Arguments
    ///
    /// * `cap` - Desired capacity in bytes (will be at least STRING_MIN_CAPACITY)
    ///
    /// # Returns
    ///
    /// A pointer to the allocated buffer, or null on failure.
    /// The memory is zero-initialized.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_string_alloc(cap: u64) -> *mut u8
    /// ```
    pub extern "C" fn __rue_string_alloc(cap: u64) -> *mut u8 {
        let actual_cap = if cap < STRING_MIN_CAPACITY {
            STRING_MIN_CAPACITY
        } else {
            cap
        };
        heap::alloc(actual_cap, 1) // Strings are byte-aligned
    }
}

crate::define_for_all_platforms! {
    /// Reallocate a string buffer to a new capacity.
    ///
    /// Implements the growth strategy: 2x current capacity, minimum STRING_MIN_CAPACITY.
    ///
    /// # Arguments
    ///
    /// * `ptr` - Pointer to the existing buffer (or null for new allocation)
    /// * `old_cap` - Current capacity (used for copying data)
    /// * `new_cap` - Desired new capacity (will grow by at least 2x if larger)
    ///
    /// # Returns
    ///
    /// A pointer to the new buffer with old data copied, or null on failure.
    ///
    /// # Growth Strategy
    ///
    /// If `new_cap > old_cap`, the actual capacity will be:
    /// - `max(new_cap, old_cap * 2, STRING_MIN_CAPACITY)`
    ///
    /// This amortizes allocation cost over many appends.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_string_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8
    /// ```
    pub extern "C" fn __rue_string_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8 {
        // Calculate actual new capacity with growth strategy
        let grown_cap = old_cap.saturating_mul(2);
        let actual_cap = new_cap.max(grown_cap).max(STRING_MIN_CAPACITY);

        // Use the general realloc, which handles null ptr and copying
        heap::realloc(ptr, old_cap, actual_cap, 1)
    }
}

crate::define_for_all_platforms! {
    /// Clone a string by allocating a new buffer and copying the content.
    ///
    /// # Arguments
    ///
    /// * `ptr` - Pointer to the source string data
    /// * `len` - Length of the string in bytes
    ///
    /// # Returns
    ///
    /// A pointer to a new buffer containing a copy of the string data,
    /// or null on allocation failure.
    ///
    /// The new buffer has capacity equal to len (minimum STRING_MIN_CAPACITY).
    ///
    /// # Safety
    ///
    /// The caller must ensure `ptr` points to valid memory of at least `len` bytes.
    ///
    /// # ABI
    ///
    /// ```text
    /// extern "C" fn __rue_string_clone(ptr: *const u8, len: u64) -> *mut u8
    /// ```
    pub extern "C" fn __rue_string_clone(ptr: *const u8, len: u64) -> *mut u8 {
        // Allocate new buffer with capacity >= len
        let cap = len.max(STRING_MIN_CAPACITY);
        let new_ptr = heap::alloc(cap, 1);
        if new_ptr.is_null() {
            return new_ptr;
        }

        // Copy the string content
        if len > 0 && !ptr.is_null() {
            // SAFETY: Copying is safe because:
            // - Caller guarantees `ptr` is valid for reads of `len` bytes
            // - `new_ptr` was just allocated with at least `len` bytes capacity
            // - The regions don't overlap (new_ptr is freshly allocated)
            // - u8 has no alignment requirements
            unsafe {
                core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
            }
        }

        new_ptr
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
/// extern "C" fn String__new(out: *mut StringResult)
/// ```
///
/// Caller allocates space for the return value and passes pointer.
/// Callee writes (ptr=0, len=0, cap=0) to that pointer.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__new(out: *mut StringResult) {
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
pub extern "C" fn String__new(out: *mut StringResult) {
    unsafe {
        (*out).ptr = core::ptr::null_mut();
        (*out).len = 0;
        (*out).cap = 0;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__new(out: *mut StringResult) {
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
/// extern "C" fn String__with_capacity(out: *mut StringResult, cap: u64)
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__with_capacity(out: *mut StringResult, requested_cap: u64) {
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__with_capacity(out: *mut StringResult, requested_cap: u64) {
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__with_capacity(out: *mut StringResult, requested_cap: u64) {
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}
// =============================================================================
// String Query Methods (Phase 6: len, capacity, is_empty)
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
pub extern "C" fn String__len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    len
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    len
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
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
pub extern "C" fn String__capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
    cap
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
    cap
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
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
pub extern "C" fn String__is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u8 {
    if len == 0 { 1 } else { 0 }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u8 {
    if len == 0 { 1 } else { 0 }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u8 {
    if len == 0 { 1 } else { 0 }
}

// =============================================================================
// String Clone Method (Phase 8)
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
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clone(out: *mut StringResult, ptr: *const u8, len: u64, _cap: u64) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    // Check for allocation failure before copy to avoid UB
    if new_ptr.is_null() {
        // SAFETY: Writing to `out` is safe - see String__new for rationale
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
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

    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clone(out: *mut StringResult, ptr: *const u8, len: u64, _cap: u64) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    // Check for allocation failure before copy to avoid UB
    if new_ptr.is_null() {
        // SAFETY: Writing to `out` is safe - see String__new for rationale
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
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

    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clone(out: *mut StringResult, ptr: *const u8, len: u64, _cap: u64) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    // Check for allocation failure before copy to avoid UB
    if new_ptr.is_null() {
        // SAFETY: Writing to `out` is safe - see String__new for rationale
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
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

    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

// =============================================================================
// String Mutation Methods (Phase 7: push_str, push, clear, reserve)
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
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push_str(
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

    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push_str(
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

    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push_str(
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

    // SAFETY: Writing to `out` is safe - see String__new for rationale
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
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push(out: *mut StringResult, ptr: *mut u8, len: u64, cap: u64, byte: u8) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    // SAFETY: Writing the byte is safe because:
    // - `string_ensure_capacity` guarantees `new_ptr` has room for `len + 1` bytes
    // - `new_ptr.add(len)` points to the first unused byte after existing content
    // - u8 has no alignment requirements
    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    let new_len = len + 1;

    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push(out: *mut StringResult, ptr: *mut u8, len: u64, cap: u64, byte: u8) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    // SAFETY: Writing the byte is safe because:
    // - `string_ensure_capacity` guarantees `new_ptr` has room for `len + 1` bytes
    // - `new_ptr.add(len)` points to the first unused byte after existing content
    // - u8 has no alignment requirements
    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    let new_len = len + 1;

    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = new_len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push(out: *mut StringResult, ptr: *mut u8, len: u64, cap: u64, byte: u8) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    // SAFETY: Writing the byte is safe because:
    // - `string_ensure_capacity` guarantees `new_ptr` has room for `len + 1` bytes
    // - `new_ptr.add(len)` points to the first unused byte after existing content
    // - u8 has no alignment requirements
    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    let new_len = len + 1;

    // SAFETY: Writing to `out` is safe - see String__new for rationale
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
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clear(out: *mut StringResult, ptr: *mut u8, _len: u64, cap: u64) {
    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clear(out: *mut StringResult, ptr: *mut u8, _len: u64, cap: u64) {
    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clear(out: *mut StringResult, ptr: *mut u8, _len: u64, cap: u64) {
    // SAFETY: Writing to `out` is safe - see String__new for rationale
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
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len; // len stays the same for reserve
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    // SAFETY: Writing to `out` is safe - see String__new for rationale
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    // SAFETY: Writing to `out` is safe - see String__new for rationale
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
/// Returns (null, 0) if allocation fails.
#[inline]
fn string_ensure_capacity(ptr: *mut u8, len: u64, cap: u64, additional: u64) -> (*mut u8, u64) {
    let required = len.saturating_add(additional);

    if cap == 0 {
        // Heap promotion: allocate new buffer and copy existing content
        let new_cap = required.max(STRING_MIN_CAPACITY);
        let new_ptr = heap::alloc(new_cap, 1);
        // Check for allocation failure before copy to avoid UB
        if new_ptr.is_null() {
            return (core::ptr::null_mut(), 0);
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
        // Need to grow: use the realloc function which implements growth strategy
        let new_ptr = heap::realloc(ptr, cap, required, 1);
        // Check for allocation failure
        if new_ptr.is_null() {
            return (core::ptr::null_mut(), 0);
        }
        // Calculate actual new capacity (realloc uses 2x growth strategy)
        let grown_cap = cap.saturating_mul(2);
        let new_cap = required.max(grown_cap).max(STRING_MIN_CAPACITY);
        (new_ptr, new_cap)
    } else {
        // Capacity is sufficient
        (ptr, cap)
    }
}
