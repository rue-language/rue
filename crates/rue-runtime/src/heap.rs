//! Heap allocation for Rue programs.
//!
//! This module provides a simple bump allocator backed by `mmap` for heap
//! allocation. It's designed for simplicity over memory efficiency - memory
//! is only returned to the OS when the program exits.
//!
//! # Design
//!
//! The allocator uses a series of "arenas" - large chunks of memory obtained
//! from the OS via `mmap`. Allocations bump a pointer forward within the
//! current arena. When an arena fills up, a new one is allocated.
//!
//! ```text
//! +------------------+------------------+
//! |     Arena 1      |     Arena 2      |
//! |  [alloc][alloc]  | [alloc][...free] |
//! +------------------+------------------+
//!                           ^
//!                           bump pointer
//! ```
//!
//! # Thread Safety
//!
//! This allocator uses atomic operations for the global arena pointer, making
//! it safe to use from multiple threads. However, concurrent allocations may
//! contend on the bump pointer within an arena. For Rue V1 (single-threaded),
//! this is fine. For heavily concurrent workloads, a thread-local allocator
//! would be more efficient.
//!
//! # Memory Efficiency
//!
//! Individual `free()` calls are no-ops. Memory is only reclaimed when:
//! - The program exits (OS reclaims all memory)
//! - A future allocator implementation adds actual freeing
//!
//! This is a deliberate trade-off: simplicity over memory efficiency.
//! Rue programs are typically short-lived, so memory waste is acceptable.

use crate::platform;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

/// Default arena size: 64 KiB
///
/// This is chosen to be:
/// - Large enough to reduce mmap syscall overhead
/// - Small enough to not waste too much memory for small programs
/// - A multiple of typical page sizes (4 KiB)
const DEFAULT_ARENA_SIZE: usize = 64 * 1024;

/// Header at the start of each arena.
///
/// Arenas are linked together in a singly-linked list. This header
/// sits at the very beginning of each mmap'd region.
///
/// The `offset` field is atomic to allow lock-free bump allocation.
#[repr(C)]
struct ArenaHeader {
    /// Pointer to the next arena in the list (older arenas).
    /// Only written during arena creation, so doesn't need to be atomic.
    next: *mut ArenaHeader,
    /// Total size of this arena (including header).
    /// Immutable after creation.
    size: usize,
    /// Current allocation offset from the start of the arena.
    /// Starts after the header, bumps forward with each allocation.
    /// Atomic to support lock-free concurrent allocation.
    offset: AtomicUsize,
}

/// Global pointer to the current arena.
///
/// This is the head of a linked list of arenas. New arenas are prepended
/// to the list. Using `AtomicPtr` makes this safe to access from multiple
/// threads without any unsafe `Sync` implementations.
static CURRENT_ARENA: AtomicPtr<ArenaHeader> = AtomicPtr::new(ptr::null_mut());

/// Align a value up to the given alignment.
///
/// # Panics
///
/// Alignment must be a power of 2. This is not checked at runtime
/// for performance - callers must ensure valid alignment.
#[inline]
const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// Check if a value is a power of 2.
#[inline]
const fn is_power_of_two(n: u64) -> bool {
    n != 0 && (n & (n - 1)) == 0
}

/// Allocate a new arena of at least `min_size` bytes.
///
/// Returns a pointer to the arena header, or null on failure.
fn alloc_arena(min_size: usize) -> *mut ArenaHeader {
    // Ensure we have room for the header plus the requested size
    let header_size = core::mem::size_of::<ArenaHeader>();

    // Use checked_add to prevent overflow
    let Some(total) = header_size.checked_add(min_size) else {
        return ptr::null_mut();
    };
    let total_size = align_up(total, 4096); // Page-align

    // Use at least the default arena size
    let arena_size = if total_size < DEFAULT_ARENA_SIZE {
        DEFAULT_ARENA_SIZE
    } else {
        total_size
    };

    // Request memory from the OS
    let ptr = platform::mmap(arena_size);
    if ptr.is_null() {
        return ptr::null_mut();
    }

    // Initialize the header
    let header = ptr as *mut ArenaHeader;
    // SAFETY: ptr is valid and properly aligned (mmap returns page-aligned memory)
    unsafe {
        (*header).next = ptr::null_mut();
        (*header).size = arena_size;
        (*header).offset = AtomicUsize::new(header_size); // Start allocations after header
    }

    header
}

/// Allocate memory from the heap.
///
/// # Arguments
///
/// * `size` - Number of bytes to allocate
/// * `align` - Required alignment (must be a power of 2)
///
/// # Returns
///
/// A pointer to the allocated memory, or null on failure.
/// The memory is zero-initialized (from mmap).
///
/// # Failure Conditions
///
/// Returns null if:
/// - `size` is 0
/// - `align` is 0 or not a power of 2
/// - Out of memory (mmap fails)
///
/// # Safety
///
/// The returned pointer (if non-null) is valid and properly aligned.
/// The memory remains valid until the program exits.
pub fn alloc(size: u64, align: u64) -> *mut u8 {
    // Validate arguments
    if size == 0 || align == 0 || !is_power_of_two(align) {
        return ptr::null_mut();
    }

    let size = size as usize;
    let align = align as usize;

    loop {
        // Load current arena
        let arena = CURRENT_ARENA.load(Ordering::Acquire);

        if arena.is_null() {
            // No arena yet - try to create one
            let new_arena = alloc_arena(size);
            if new_arena.is_null() {
                return ptr::null_mut();
            }

            // Try to install our new arena as the current one
            match CURRENT_ARENA.compare_exchange(
                ptr::null_mut(),
                new_arena,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // We installed the arena, now allocate from it
                    return alloc_from_arena(new_arena, size, align);
                }
                Err(_) => {
                    // Someone else beat us - free our arena and retry
                    // SAFETY: new_arena was just created by us and not shared
                    unsafe {
                        platform::munmap(new_arena as *mut u8, (*new_arena).size);
                    }
                    continue;
                }
            }
        }

        // Try to allocate from the current arena
        // SAFETY: arena is non-null and points to valid memory from mmap
        unsafe {
            let arena_size = (*arena).size;

            // Use compare-and-swap loop to atomically bump the offset
            loop {
                let current_offset = (*arena).offset.load(Ordering::Relaxed);

                // Calculate aligned offset with overflow check
                let aligned_offset = align_up(current_offset, align);
                let Some(new_offset) = aligned_offset.checked_add(size) else {
                    return ptr::null_mut();
                };

                if new_offset > arena_size {
                    // Doesn't fit - need a new arena
                    break;
                }

                // Try to claim this space
                match (*arena).offset.compare_exchange_weak(
                    current_offset,
                    new_offset,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // Success! Return the allocated memory
                        return (arena as *mut u8).add(aligned_offset);
                    }
                    Err(_) => {
                        // Someone else modified the offset, retry
                        continue;
                    }
                }
            }

            // Allocation doesn't fit in current arena - create a new one
            let new_arena = alloc_arena(size);
            if new_arena.is_null() {
                return ptr::null_mut();
            }

            // Link new arena to the old one
            (*new_arena).next = arena;

            // Try to install our new arena as the current one
            match CURRENT_ARENA.compare_exchange(
                arena,
                new_arena,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // We installed the arena, now allocate from it
                    return alloc_from_arena(new_arena, size, align);
                }
                Err(_) => {
                    // Someone else changed the arena - free ours and retry
                    platform::munmap(new_arena as *mut u8, (*new_arena).size);
                    continue;
                }
            }
        }
    }
}

/// Allocate from a freshly-created arena (no contention possible).
///
/// This is a helper for the common case where we just created an arena
/// and know we're the only thread using it.
fn alloc_from_arena(arena: *mut ArenaHeader, size: usize, align: usize) -> *mut u8 {
    // SAFETY: arena is valid and we have exclusive access (just created it)
    unsafe {
        let header_size = core::mem::size_of::<ArenaHeader>();
        let aligned_offset = align_up(header_size, align);
        // Note: overflow is impossible here because alloc_arena already ensured
        // the arena is large enough for header_size + size (with alignment padding)
        let new_offset = aligned_offset + size;
        (*arena).offset.store(new_offset, Ordering::Relaxed);
        (arena as *mut u8).add(aligned_offset)
    }
}

/// Free memory previously allocated by `alloc`.
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
/// reclaimed when the program exits. The `size` and `align` parameters
/// are accepted for API compatibility with future allocators that may
/// actually free memory.
///
/// # Safety
///
/// The caller should ensure `ptr` was returned by a previous `alloc` call,
/// but since this is a no-op, invalid pointers are harmless.
#[allow(unused_variables)]
pub fn free(ptr: *mut u8, size: u64, align: u64) {
    // No-op for bump allocator.
    // Memory is reclaimed when the program exits.
    //
    // Future allocators may implement actual freeing by:
    // 1. Finding which arena the pointer belongs to
    // 2. Marking the region as free in a freelist
    // 3. Potentially unmapping arenas when fully free
}

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
/// - If `ptr` is null: behaves like `alloc(new_size, align)`
/// - If `new_size` is 0: behaves like `free(ptr, old_size, align)`, returns null
/// - If `new_size <= old_size`: returns `ptr` unchanged (no shrinking needed)
/// - If `new_size > old_size`: allocates new block, copies old data, returns new pointer
///
/// # Memory Contents
///
/// When growing, the contents up to `min(old_size, new_size)` are preserved.
/// Any additional bytes are zero-initialized (from mmap).
///
/// # Safety
///
/// The old pointer becomes invalid after a successful realloc that changes
/// the address. The caller must use the returned pointer.
pub fn realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8 {
    // Handle null pointer case - just allocate
    if ptr.is_null() {
        return alloc(new_size, align);
    }

    // Handle zero new_size - just free
    if new_size == 0 {
        free(ptr, old_size, align);
        return ptr::null_mut();
    }

    // Validate alignment
    if align == 0 || !is_power_of_two(align) {
        return ptr::null_mut();
    }

    // If shrinking or same size, just return the original pointer
    // (bump allocator can't reclaim the extra space anyway)
    if new_size <= old_size {
        return ptr;
    }

    // Need to grow - allocate new block and copy
    let new_ptr = alloc(new_size, align);
    if new_ptr.is_null() {
        return ptr::null_mut();
    }

    // Copy old data to new location
    // SAFETY: Both pointers are valid, and we're copying old_size bytes
    // which is guaranteed to be <= both allocation sizes
    unsafe {
        ptr::copy_nonoverlapping(ptr, new_ptr, old_size as usize);
    }

    // Note: We don't free the old pointer since free is a no-op.
    // The old memory is "wasted" until program exit.

    new_ptr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(1, 8), 8);
        assert_eq!(align_up(7, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
        assert_eq!(align_up(15, 16), 16);
        assert_eq!(align_up(16, 16), 16);
        assert_eq!(align_up(17, 16), 32);
    }

    #[test]
    fn test_is_power_of_two() {
        assert!(!is_power_of_two(0));
        assert!(is_power_of_two(1));
        assert!(is_power_of_two(2));
        assert!(!is_power_of_two(3));
        assert!(is_power_of_two(4));
        assert!(!is_power_of_two(5));
        assert!(is_power_of_two(8));
        assert!(is_power_of_two(16));
        assert!(is_power_of_two(4096));
        assert!(!is_power_of_two(4097));
    }

    #[test]
    fn test_alloc_zero_size() {
        let ptr = alloc(0, 8);
        assert!(ptr.is_null());
    }

    #[test]
    fn test_alloc_zero_align() {
        let ptr = alloc(16, 0);
        assert!(ptr.is_null());
    }

    #[test]
    fn test_alloc_bad_align() {
        // 3 is not a power of 2
        let ptr = alloc(16, 3);
        assert!(ptr.is_null());
    }

    #[test]
    fn test_alloc_basic() {
        let ptr = alloc(64, 8);
        assert!(!ptr.is_null());
        assert_eq!(ptr as usize % 8, 0); // Check alignment

        // Memory should be usable
        unsafe {
            *ptr = 42;
            assert_eq!(*ptr, 42);
        }
    }

    #[test]
    fn test_alloc_alignment() {
        // Test various alignments
        for align in [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 4096] {
            let ptr = alloc(64, align);
            assert!(!ptr.is_null(), "alloc failed for align={}", align);
            assert_eq!(
                ptr as usize % align as usize,
                0,
                "bad alignment for align={}",
                align
            );
        }
    }

    #[test]
    fn test_alloc_multiple() {
        // Allocate multiple blocks
        let mut ptrs = [ptr::null_mut(); 10];
        for i in 0..10 {
            ptrs[i] = alloc(128, 8);
            assert!(!ptrs[i].is_null());
        }

        // All pointers should be different
        for i in 0..10 {
            for j in (i + 1)..10 {
                assert_ne!(ptrs[i], ptrs[j]);
            }
        }

        // All should be usable
        for (i, ptr) in ptrs.iter().enumerate() {
            unsafe {
                **ptr = i as u8;
            }
        }
        for (i, ptr) in ptrs.iter().enumerate() {
            unsafe {
                assert_eq!(**ptr, i as u8);
            }
        }
    }

    #[test]
    fn test_alloc_large() {
        // Allocate more than the default arena size
        let large_size = DEFAULT_ARENA_SIZE as u64 * 2;
        let ptr = alloc(large_size, 8);
        assert!(!ptr.is_null());

        // Should be usable across the entire range
        unsafe {
            *ptr = 1;
            *ptr.add(large_size as usize - 1) = 2;
            assert_eq!(*ptr, 1);
            assert_eq!(*ptr.add(large_size as usize - 1), 2);
        }
    }

    #[test]
    fn test_free_is_noop() {
        // Free should not crash even with various inputs
        let ptr = alloc(64, 8);
        assert!(!ptr.is_null());

        // Free multiple times (should be fine since it's a no-op)
        free(ptr, 64, 8);
        free(ptr, 64, 8);
        free(ptr::null_mut(), 0, 0);
    }

    #[test]
    fn test_realloc_null_ptr() {
        // realloc with null ptr should behave like alloc
        let ptr = realloc(ptr::null_mut(), 0, 64, 8);
        assert!(!ptr.is_null());
        assert_eq!(ptr as usize % 8, 0);
    }

    #[test]
    fn test_realloc_zero_size() {
        // realloc with zero new_size should behave like free
        let ptr = alloc(64, 8);
        assert!(!ptr.is_null());

        let result = realloc(ptr, 64, 0, 8);
        assert!(result.is_null());
    }

    #[test]
    fn test_realloc_shrink() {
        // Shrinking should return the same pointer
        let ptr = alloc(128, 8);
        assert!(!ptr.is_null());

        // Write some data
        unsafe {
            *ptr = 42;
        }

        // Shrink - should return same pointer
        let new_ptr = realloc(ptr, 128, 64, 8);
        assert_eq!(new_ptr, ptr);

        // Data should still be there
        unsafe {
            assert_eq!(*new_ptr, 42);
        }
    }

    #[test]
    fn test_realloc_same_size() {
        // Same size should return same pointer
        let ptr = alloc(64, 8);
        assert!(!ptr.is_null());

        unsafe {
            *ptr = 99;
        }

        let new_ptr = realloc(ptr, 64, 64, 8);
        assert_eq!(new_ptr, ptr);

        unsafe {
            assert_eq!(*new_ptr, 99);
        }
    }

    #[test]
    fn test_realloc_grow() {
        // Growing should return new pointer with copied data
        let ptr = alloc(32, 8);
        assert!(!ptr.is_null());

        // Write pattern to original
        unsafe {
            for i in 0..32 {
                *ptr.add(i) = i as u8;
            }
        }

        // Grow
        let new_ptr = realloc(ptr, 32, 128, 8);
        assert!(!new_ptr.is_null());
        assert_eq!(new_ptr as usize % 8, 0);

        // Verify original data was copied
        unsafe {
            for i in 0..32 {
                assert_eq!(*new_ptr.add(i), i as u8, "byte {} not copied", i);
            }
        }

        // New area should be writable
        unsafe {
            *new_ptr.add(100) = 0xFF;
            assert_eq!(*new_ptr.add(100), 0xFF);
        }
    }

    #[test]
    fn test_realloc_bad_align() {
        let ptr = alloc(64, 8);
        assert!(!ptr.is_null());

        // Bad alignment should fail
        let result = realloc(ptr, 64, 128, 3);
        assert!(result.is_null());

        // Zero alignment should fail
        let result = realloc(ptr, 64, 128, 0);
        assert!(result.is_null());
    }

    #[test]
    fn test_realloc_grow_large() {
        // Test growing to a large size
        let ptr = alloc(64, 8);
        assert!(!ptr.is_null());

        unsafe {
            *ptr = 0xAB;
        }

        // Grow to larger than default arena
        let large_size = DEFAULT_ARENA_SIZE as u64 * 2;
        let new_ptr = realloc(ptr, 64, large_size, 8);
        assert!(!new_ptr.is_null());

        // Original data preserved
        unsafe {
            assert_eq!(*new_ptr, 0xAB);
            // Can write to end of new allocation
            *new_ptr.add(large_size as usize - 1) = 0xCD;
            assert_eq!(*new_ptr.add(large_size as usize - 1), 0xCD);
        }
    }
}
