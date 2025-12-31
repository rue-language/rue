use crate::{LassoError, LassoErrorKind, LassoResult};
use alloc::alloc::{alloc, dealloc, Layout};
use core::{
    hint,
    mem::{align_of, size_of},
    num::NonZeroUsize,
    ptr::{self, addr_of_mut, NonNull},
    slice,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

pub(super) struct AtomicBucketList {
    /// The first bucket in the list, will be null if the list currently
    /// has no buckets
    head: AtomicPtr<AtomicBucket>,
}

impl AtomicBucketList {
    /// Create a new bucket list
    pub fn new(first_bucket_capacity: NonZeroUsize) -> LassoResult<Self> {
        let bucket = AtomicBucket::with_capacity(first_bucket_capacity)?;

        Ok(Self {
            head: AtomicPtr::new(bucket.as_ptr()),
        })
    }

    pub fn iter(&self) -> AtomicBucketIter<'_> {
        AtomicBucketIter {
            current: &self.head,
        }
    }

    /// Get the number of buckets within the current list
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Returns `true` if there's no buckets within the current list
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn push_front(&self, bucket: BucketRef) {
        let bucket_ptr = bucket.as_ptr();
        let mut head_ptr = self.head.load(Ordering::Acquire);

        loop {
            // The new bucket will become the head of the list, so we rewrite its next
            // pointer to point to the next bucket (the previous head of the list)
            unsafe {
                addr_of_mut!((*bucket_ptr).next).write(AtomicPtr::new(head_ptr));
            }

            // Replace the old head pointer with the pointer to our new bucket
            let exchange = self.head.compare_exchange_weak(
                head_ptr,
                bucket_ptr,
                Ordering::AcqRel,
                Ordering::Acquire,
            );

            // The exchange failed, set the head pointer to the new head node
            if let Err(new_head) = exchange {
                head_ptr = new_head;

            // Otherwise we succeeded swapping the pointers and are done
            } else {
                break;
            }
        }
    }
}

impl Drop for AtomicBucketList {
    fn drop(&mut self) {
        // Safety: We should have exclusive access to all buckets
        unsafe {
            let mut head_ptr = self.head.load(Ordering::Acquire);

            while !head_ptr.is_null() {
                // Keep ahold of the current pointer so we can operate over it
                let current_ptr = head_ptr;

                // Grab the next pointer and set it to be the next in line for
                // deallocation
                head_ptr = (*head_ptr).next.load(Ordering::Acquire);

                // Get the layout of the current bucket so we can deallocate it
                let capacity = (*current_ptr).capacity;
                let layout = AtomicBucket::layout(capacity)
                    .expect("buckets with invalid capacities can't be constructed");

                // Deallocate all memory that the bucket allocated
                dealloc(current_ptr.cast(), layout);
            }
        }
    }
}

pub(super) struct AtomicBucketIter<'a> {
    current: &'a AtomicPtr<AtomicBucket>,
}

impl<'a> Iterator for AtomicBucketIter<'a> {
    type Item = BucketRef;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.current.load(Ordering::Acquire);

        NonNull::new(current).map(|current| {
            // Safety: `current` is valid and not null
            self.current = unsafe { &(*current.as_ptr()).next };

            // Safety: `current` points to a valid bucket
            unsafe { BucketRef::new(current) }
        })
    }
}

/// A unique reference to an atomic bucket
#[repr(transparent)]
pub(super) struct UniqueBucketRef {
    bucket: BucketRef,
}

impl UniqueBucketRef {
    /// Create a new unique bucket ref
    ///
    /// # Safety
    ///
    /// The pointer must have exclusive, mutable and unique access to the pointed-to
    /// bucket
    #[inline]
    const unsafe fn new(bucket: NonNull<AtomicBucket>) -> Self {
        Self {
            bucket: unsafe { BucketRef::new(bucket) },
        }
    }

    #[inline]
    pub const fn as_ptr(&self) -> *mut AtomicBucket {
        self.bucket.as_ptr()
    }

    /// Get the current bucket's length
    #[inline]
    pub fn len(&self) -> usize {
        unsafe { *(*addr_of_mut!((*self.as_ptr()).len)).get_mut() }
    }

    /// Get the current bucket's capacity
    #[inline]
    pub fn capacity(&self) -> NonZeroUsize {
        self.bucket.capacity()
    }

    /// Set the bucket's length
    ///
    /// # Safety
    ///
    /// `new_length` must be less than or equal to the current capacity
    /// and all bytes up to `new_length` must be initialized and valid
    /// utf-8
    #[inline]
    pub unsafe fn set_len(&mut self, new_length: usize) {
        debug_assert!(
            new_length <= self.capacity().get(),
            "the bucket length {} should always be less than the bucket's capacity {}",
            new_length,
            self.capacity(),
        );

        // Safety: We have exclusive access to the bucket
        unsafe { *(*addr_of_mut!((*self.as_ptr()).len)).get_mut() = new_length };
    }

    /// Push a slice of bytes to the current bucket
    ///
    /// # Safety
    ///
    /// The returned `&'static str` (and all copies of it) must be dropped
    /// before the current bucket is, as this bucket contains the backing
    /// memory for the string.
    /// Additionally, the underlying [`AtomicBucket`] must have enough room
    /// to store the entire slice and the given slice must be valid utf-8 data.
    ///
    pub unsafe fn push_slice(&mut self, slice: &[u8]) -> &'static str {
        let len = self.len();

        if cfg!(debug_assertions) {
            let capacity = self.capacity().get();

            debug_assert_ne!(len, capacity);
            debug_assert!(slice.len() <= capacity - len);
        }

        // Get a pointer to the start of the free data
        let ptr = unsafe { addr_of_mut!((*self.as_ptr())._data).cast::<u8>().add(len) };

        // Make the slice that we'll fill with the string's data
        let target = unsafe { slice::from_raw_parts_mut(ptr, slice.len()) };
        // Copy the data from the source string into the bucket's buffer
        target.copy_from_slice(slice);

        // Increment the index so that the string we just added isn't overwritten
        // Safety: All bytes are initialized and the length is <= capacity
        unsafe { self.set_len(len + slice.len()) };

        // Create a string from that slice
        // Safety: The source string was valid utf8, so the created buffer will be as well

        unsafe { core::str::from_utf8_unchecked(target) }
    }

    #[inline]
    pub(crate) const fn into_ref(self) -> BucketRef {
        self.bucket
    }
}

/// A reference to an [`AtomicBucket`]
#[repr(transparent)]
pub(super) struct BucketRef {
    bucket: NonNull<AtomicBucket>,
}

impl BucketRef {
    /// Create a new [`BucketRef`]
    ///
    /// # Safety
    ///
    /// `bucket` must be a valid pointer to an [`AtomicBucket`]
    const unsafe fn new(bucket: NonNull<AtomicBucket>) -> Self {
        Self { bucket }
    }

    #[inline]
    pub const fn as_ptr(&self) -> *mut AtomicBucket {
        self.bucket.as_ptr()
    }

    /// Get the bucket's length
    #[inline]
    fn length(&self) -> &AtomicUsize {
        // Safety: `bucket` is a valid pointer to a bucket
        unsafe { &(*self.as_ptr()).len }
    }

    /// Get the bucket's capacity
    #[inline]
    fn capacity(&self) -> NonZeroUsize {
        // Safety: `bucket` is a valid pointer to a bucket
        unsafe { (*self.as_ptr()).capacity }
    }

    /// Get a slice pointer to the specified data range
    #[inline]
    pub unsafe fn slice_mut(&self, start: usize) -> *mut u8 {
        unsafe { addr_of_mut!((*self.as_ptr())._data).cast::<u8>().add(start) }
    }

    pub fn try_inc_length(&self, additional: usize) -> Result<usize, ()> {
        debug_assert_ne!(additional, 0);

        let length = self.length();
        let capacity = self.capacity().get();

        // TODO: Add backoff to this loop so we don't thrash it
        let mut len = length.load(Ordering::Acquire);
        for _ in 0..100 {
            let new_length = len + additional;
            if new_length <= capacity {
                match length.compare_exchange_weak(
                    len,
                    new_length,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        debug_assert!(len < capacity && len + additional <= capacity);
                        return Ok(len);
                    }
                    Err(loaded) => {
                        hint::spin_loop();
                        len = loaded;
                    }
                }
            } else {
                break;
            }
        }

        Err(())
    }
}

#[repr(C)]
pub(super) struct AtomicBucket {
    /// The next bucket in the list, will be null if this is the last bucket
    next: AtomicPtr<Self>,

    /// The start of uninitialized memory within `items`
    ///
    /// Invariant: `len` will always be less than or equal to `capacity`
    len: AtomicUsize,

    /// The total number of bytes allocated within the bucket
    capacity: NonZeroUsize,

    /// The inline allocated data of this bucket
    ///
    /// Invariant: Never touch this field manually, it contains uninitialized data up
    /// to the length of `capacity`
    _data: [u8; 0],
}

impl AtomicBucket {
    /// Allocates a bucket with space for `capacity` items
    pub(crate) fn with_capacity(capacity: NonZeroUsize) -> LassoResult<UniqueBucketRef> {
        // Create the bucket's layout
        let layout = Self::layout(capacity)?;
        debug_assert_ne!(layout.size(), 0);

        // Allocate memory for the bucket
        // Safety: The given layout has a non-zero size
        let ptr = unsafe {
            NonNull::new(alloc(layout))
                .ok_or_else(|| LassoError::new(LassoErrorKind::FailedAllocation))?
                .cast::<Self>()
        };

        // Write to the fields of the bucket
        // Safety: We have exclusive access to the bucket and can write
        //         to its uninitialized fields
        unsafe {
            let ptr = ptr.as_ptr();

            addr_of_mut!((*ptr).next).write(AtomicPtr::new(ptr::null_mut()));
            addr_of_mut!((*ptr).len).write(AtomicUsize::new(0));
            addr_of_mut!((*ptr).capacity).write(capacity);

            // We leave the allocated data uninitialized, future writers will
            // initialize it as-needed
        }

        // Safety: We have exclusive access to the bucket
        Ok(unsafe { UniqueBucketRef::new(ptr) })
    }

    /// Create the layout for a bucket
    ///
    /// # Safety
    ///
    /// `capacity` must be a power of two that won't overflow when rounded up
    ///
    fn layout(capacity: NonZeroUsize) -> LassoResult<Layout> {
        let next = Layout::new::<AtomicPtr<Self>>();
        let len = Layout::new::<usize>();
        let cap = Layout::new::<NonZeroUsize>();

        // Safety: Align will always be a non-zero power of two and the
        //         size will not overflow when rounded up
        debug_assert!(
            Layout::from_size_align(size_of::<u8>() * capacity.get(), align_of::<u8>()).is_ok()
        );
        let data = unsafe {
            Layout::from_size_align_unchecked(size_of::<u8>() * capacity.get(), align_of::<u8>())
        };

        next.extend(len)
            .and_then(|(layout, _)| layout.extend(cap))
            .and_then(|(layout, _)| layout.extend(data))
            .map(|(layout, _)| layout.pad_to_align())
            .map_err(|_| LassoError::new(LassoErrorKind::FailedAllocation))
    }
}

unsafe impl Send for AtomicBucket {}
unsafe impl Sync for AtomicBucket {}
