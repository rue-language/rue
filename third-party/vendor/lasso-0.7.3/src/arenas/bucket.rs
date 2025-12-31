use crate::{LassoError, LassoErrorKind, LassoResult};
use alloc::alloc::{alloc, dealloc, Layout};
use core::{
    mem::{align_of, size_of},
    num::NonZeroUsize,
    ptr::NonNull,
    slice,
};

/// A bucket to hold a number of stored items
pub(super) struct Bucket {
    /// The start of uninitialized memory within `items`
    index: usize,
    /// A pointer to the start of the data
    items: NonNull<u8>,
    /// The total number of Ts that can be stored
    capacity: NonZeroUsize,
}

impl Bucket {
    /// Allocates a bucket with space for `capacity` items
    pub(crate) fn with_capacity(capacity: NonZeroUsize) -> LassoResult<Self> {
        unsafe {
            debug_assert!(Layout::from_size_align(
                size_of::<u8>() * capacity.get(),
                align_of::<u8>(),
            )
            .is_ok());

            // Safety: Align will always be a non-zero power of two and the
            //         size will not overflow when rounded up
            let layout = Layout::from_size_align_unchecked(
                size_of::<u8>() * capacity.get(),
                align_of::<u8>(),
            );

            // Allocate the bucket's memory
            let items = NonNull::new(alloc(layout))
                // TODO: When `Result`s are piped through return this as a unique error
                .ok_or_else(|| LassoError::new(LassoErrorKind::FailedAllocation))?
                .cast();

            Ok(Self {
                index: 0,
                capacity,
                items,
            })
        }
    }

    /// Get the number of available slots for the current bucket
    pub(crate) fn free_elements(&self) -> usize {
        self.capacity.get() - self.index
    }

    /// Returns whether the current bucket is full
    pub(crate) fn is_full(&self) -> bool {
        self.index == self.capacity.get()
    }

    /// Marks the bucket as being totally unused, meaning that all of `capacity`
    /// is valid for allocations
    pub(crate) fn clear(&mut self) {
        self.index = 0;
    }

    /// Push a slice to the current bucket, returning a pointer to it
    ///
    /// # Safety
    ///
    /// The current bucket must have room for all bytes of the slice and
    /// the caller promises to forget the reference before the arena is dropped.
    /// Additionally, `slice` must be valid UTF-8 and should come from an `&str`
    ///
    pub(crate) unsafe fn push_slice(&mut self, slice: &[u8]) -> &'static str {
        debug_assert!(!self.is_full());
        debug_assert!(slice.len() <= self.capacity.get() - self.index);

        unsafe {
            // Get a pointer to the start of free bytes
            let ptr = self.items.as_ptr().add(self.index);

            // Make the slice that we'll fill with the string's data
            let target = slice::from_raw_parts_mut(ptr, slice.len());
            // Copy the data from the source string into the bucket's buffer
            target.copy_from_slice(slice);
            // Increment the index so that the string we just made isn't overwritten
            self.index += slice.len();

            // Create a string from that slice
            // Safety: The source string was valid utf8, so the created buffer will be as well
            core::str::from_utf8_unchecked(target)
        }
    }
}

impl Drop for Bucket {
    fn drop(&mut self) {
        // Safety: We have exclusive access to the pointers since the contract of
        //         `store_str` should be withheld
        unsafe {
            let items = self.items.as_ptr();

            debug_assert!(Layout::from_size_align(
                size_of::<u8>() * self.capacity.get(),
                align_of::<u8>(),
            )
            .is_ok());

            // Deallocate all memory that the bucket allocated
            dealloc(
                items,
                // Safety: Align will always be a non-zero power of two and the
                //         size will not overflow when rounded up
                Layout::from_size_align_unchecked(
                    size_of::<u8>() * self.capacity.get(),
                    align_of::<u8>(),
                ),
            );
        }
    }
}

unsafe impl Send for Bucket {}
unsafe impl Sync for Bucket {}
