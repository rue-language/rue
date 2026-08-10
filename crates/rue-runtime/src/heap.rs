//! Rue runtime integration for the standalone recycling allocator.
//!
//! Allocation policy and bookkeeping live in `rue-allocator`. This module
//! supplies the runtime's direct-syscall page mapper and exposes the small API
//! used by runtime helpers and internal consumers.

use crate::platform;
#[cfg(test)]
use core::ptr;
use rue_allocator::{Allocator, PageMapper};

#[cfg(test)]
extern crate std;

#[cfg(test)]
std::thread_local! {
    /// Test-only fault seam at the raw allocator boundary. Production builds
    /// contain no counter or alternate allocation path; native CI on each
    /// supported platform uses this to exercise safe consumers deterministically.
    static ALLOCATIONS_BEFORE_FAILURE: core::cell::Cell<Option<usize>> = const {
        core::cell::Cell::new(None)
    };
}

#[cfg(test)]
pub(crate) fn fail_allocations_after_for_test(successful_allocations: usize) {
    ALLOCATIONS_BEFORE_FAILURE.set(Some(successful_allocations));
}

#[cfg(test)]
pub(crate) fn disable_allocation_failure_for_test() {
    ALLOCATIONS_BEFORE_FAILURE.set(None);
}

#[cfg(test)]
fn allocation_permitted_for_test() -> bool {
    ALLOCATIONS_BEFORE_FAILURE.get().is_none_or(|remaining| {
        if remaining == 0 {
            false
        } else {
            ALLOCATIONS_BEFORE_FAILURE.set(Some(remaining - 1));
            true
        }
    })
}

struct RuntimePageMapper;

// SAFETY: the platform mmap/munmap pair provides exclusive page-aligned
// mappings and supports independent concurrent syscall invocations.
unsafe impl PageMapper for RuntimePageMapper {
    #[inline]
    fn allocation_permitted() -> bool {
        #[cfg(test)]
        {
            allocation_permitted_for_test()
        }
        #[cfg(not(test))]
        {
            true
        }
    }

    #[inline]
    fn map(size: usize) -> *mut u8 {
        platform::mmap(size)
    }

    #[inline]
    unsafe fn unmap(pointer: *mut u8, size: usize) {
        // SAFETY: rue-allocator only supplies complete mappings previously
        // returned by this mapper with their original page-rounded size.
        platform::munmap(pointer, size);
    }
}

static ALLOCATOR: Allocator<RuntimePageMapper> = Allocator::new();

/// Allocate uninitialized storage for the supplied layout.
pub fn alloc(size: u64, align: u64) -> *mut u8 {
    ALLOCATOR.allocate(size, align)
}

/// Release storage previously returned by [`alloc`] or [`realloc`].
///
/// A null pointer is accepted and has no effect.
///
/// # Safety
///
/// A non-null `pointer` must identify a live allocation with exactly `size` and
/// `align`, and it must not be accessed after this call.
pub unsafe fn free(pointer: *mut u8, size: u64, align: u64) {
    // SAFETY: inherited from this function's caller contract.
    unsafe { ALLOCATOR.deallocate(pointer, size, align) };
}

/// Allocate storage for the supplied layout with every byte set to zero.
///
/// Returns null on the same failures as [`alloc`]. The allocator has no cheaper
/// zeroing path than writing the bytes today (its arenas are recycled), so this
/// allocates and clears; the intrinsic exists so the guarantee is expressible
/// and so a future zero-page mapping is a runtime-local change (RUE-968).
pub fn alloc_zeroed(size: u64, align: u64) -> *mut u8 {
    let pointer = ALLOCATOR.allocate(size, align);
    if !pointer.is_null() {
        // SAFETY: a successful allocation is writable for `size` bytes.
        unsafe { crate::memory::memset(pointer, 0, size as usize) };
    }
    pointer
}

/// Try to relabel an allocation as `new_size` bytes without moving it.
///
/// Reports whether the block now describes `new_size` bytes at the same
/// address. A `false` result changed nothing: the caller keeps `old_size`.
///
/// # Safety
///
/// A non-null `pointer` must identify a live allocation for `old_size` and
/// `align`.
pub unsafe fn resize(pointer: *mut u8, old_size: u64, new_size: u64, align: u64) -> bool {
    // SAFETY: inherited from this function's caller contract.
    unsafe { ALLOCATOR.resize_in_place(pointer, old_size, new_size, align) }
}

/// Resize an allocation, preserving `min(old_size, new_size)` bytes.
///
/// Allocation failure returns null and leaves the old allocation live and
/// unchanged.
///
/// # Safety
///
/// A non-null `pointer` must identify a live allocation for `old_size` and
/// `align` and be readable for `old_size` bytes.
pub unsafe fn realloc(pointer: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8 {
    // SAFETY: inherited from this function's caller contract.
    unsafe { ALLOCATOR.reallocate(pointer, old_size, new_size, align) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_layouts_return_null() {
        assert!(alloc(0, 8).is_null());
        assert!(alloc(16, 0).is_null());
        assert!(alloc(16, 3).is_null());
        assert!(alloc(16, 8192).is_null());
        assert!(alloc(u64::MAX - 124, 1).is_null());
    }

    #[test]
    fn allocation_is_aligned_and_usable() {
        for align in [1u64, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 4096] {
            let pointer = alloc(64, align);
            assert!(!pointer.is_null(), "alloc failed for align={align}");
            assert_eq!(pointer as usize % align as usize, 0);
            unsafe {
                pointer.write(42);
                assert_eq!(pointer.read(), 42);
                free(pointer, 64, align);
            }
        }
    }

    #[test]
    fn small_allocation_is_recycled() {
        let first = alloc(6000, 8);
        assert!(!first.is_null());
        // SAFETY: first is live with this exact layout.
        unsafe { free(first, 6000, 8) };

        let second = alloc(6000, 8);
        assert_eq!(second, first);
        // SAFETY: second is live with this exact layout.
        unsafe { free(second, 6000, 8) };
    }

    #[test]
    fn large_allocation_is_usable_and_releasable() {
        let size = 200 * 1024u64;
        let pointer = alloc(size, 8);
        assert!(!pointer.is_null());
        unsafe {
            pointer.write(1);
            pointer.add(size as usize - 1).write(2);
            assert_eq!(pointer.read(), 1);
            assert_eq!(pointer.add(size as usize - 1).read(), 2);
            free(pointer, size, 8);
        }
    }

    #[test]
    fn realloc_within_class_keeps_pointer() {
        let pointer = alloc(65, 8);
        assert!(!pointer.is_null());
        unsafe { pointer.write(99) };

        // SAFETY: pointer is live and readable for 65 bytes.
        let grown = unsafe { realloc(pointer, 65, 120, 8) };
        assert_eq!(grown, pointer);
        assert_eq!(unsafe { grown.read() }, 99);
        // SAFETY: grown now has the new logical layout.
        unsafe { free(grown, 120, 8) };
    }

    #[test]
    fn realloc_between_classes_copies_data() {
        let pointer = alloc(32, 8);
        assert!(!pointer.is_null());
        unsafe {
            for index in 0..32 {
                pointer.add(index).write(index as u8);
            }
        }

        // SAFETY: pointer is live and readable for 32 bytes.
        let grown = unsafe { realloc(pointer, 32, 128, 8) };
        assert!(!grown.is_null());
        assert_ne!(grown, pointer);
        unsafe {
            for index in 0..32 {
                assert_eq!(grown.add(index).read(), index as u8);
            }
            free(grown, 128, 8);
        }
    }

    #[test]
    fn realloc_zero_frees_and_null_realloc_allocates() {
        let pointer = alloc(64, 8);
        assert!(!pointer.is_null());
        // SAFETY: pointer is live with this layout.
        assert!(unsafe { realloc(pointer, 64, 0, 8) }.is_null());

        // SAFETY: null is explicitly accepted.
        let fresh = unsafe { realloc(ptr::null_mut(), 0, 64, 8) };
        assert!(!fresh.is_null());
        // SAFETY: fresh is live with this layout.
        unsafe { free(fresh, 64, 8) };
    }

    #[test]
    fn failed_realloc_preserves_old_allocation() {
        let pointer = alloc(32, 8);
        assert!(!pointer.is_null());
        unsafe {
            for (index, byte) in [11u8, 22, 33, 44].into_iter().enumerate() {
                pointer.add(index).write(byte);
            }
        }

        fail_allocations_after_for_test(0);
        // SAFETY: pointer is live and readable for 32 bytes.
        let grown = unsafe { realloc(pointer, 32, 128, 8) };
        disable_allocation_failure_for_test();

        assert!(grown.is_null());
        assert_eq!(
            unsafe { core::slice::from_raw_parts(pointer, 4) },
            &[11, 22, 33, 44]
        );
        // SAFETY: failed realloc left pointer live.
        unsafe { free(pointer, 32, 8) };
    }

    #[test]
    fn zeroed_allocation_reads_as_zero_and_reports_failure_as_null() {
        let pointer = alloc_zeroed(96, 8);
        assert!(!pointer.is_null());
        // SAFETY: the allocation is live and readable for 96 bytes.
        assert!(
            unsafe { core::slice::from_raw_parts(pointer, 96) }
                .iter()
                .all(|byte| *byte == 0)
        );
        // SAFETY: pointer is live with this layout.
        unsafe { free(pointer, 96, 8) };

        assert!(alloc_zeroed(0, 8).is_null());
        assert!(alloc_zeroed(16, 3).is_null());
    }

    #[test]
    fn in_place_resize_keeps_the_pointer_or_changes_nothing() {
        let pointer = alloc(65, 8);
        assert!(!pointer.is_null());
        // SAFETY: the allocation is live and writable for 65 bytes.
        unsafe { pointer.write(7) };

        // SAFETY: pointer is live with layout (65, 8).
        assert!(unsafe { resize(pointer, 65, 120, 8) });
        // SAFETY: an accepted in-place resize never moves or clears the block.
        assert_eq!(unsafe { pointer.read() }, 7);

        // SAFETY: pointer is live with layout (120, 8).
        assert!(!unsafe { resize(pointer, 120, 200 * 1024, 8) });
        // SAFETY: the refusal left the (120, 8) layout in force.
        unsafe { free(pointer, 120, 8) };
    }

    #[test]
    fn null_free_is_a_noop() {
        // SAFETY: null is explicitly accepted regardless of layout.
        unsafe { free(ptr::null_mut(), 0, 0) };
    }
}
