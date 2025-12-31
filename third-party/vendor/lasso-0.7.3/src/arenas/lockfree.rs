use crate::{
    arenas::atomic_bucket::{AtomicBucket, AtomicBucketList},
    Capacity, LassoError, LassoErrorKind, LassoResult, MemoryLimits,
};
use core::{
    fmt::{self, Debug},
    num::NonZeroUsize,
    slice, str,
    sync::atomic::{AtomicUsize, Ordering},
};

/// An arena allocator that dynamically grows in size when needed, allocating memory in large chunks
pub(crate) struct LockfreeArena {
    /// All the internal buckets, storing all allocated and unallocated items
    // TODO: We could keep around a second list of buckets to store filled buckets
    //       in to keep us from having to iterate over them, need more tests to
    //       see what the impact of that is
    buckets: AtomicBucketList,
    /// The default capacity of each bucket
    ///
    /// Invariant: `bucket_capacity` must never be zero
    bucket_capacity: AtomicUsize,
    memory_usage: AtomicUsize,
    max_memory_usage: AtomicUsize,
}

impl LockfreeArena {
    /// Create a new Arena with the default bucket size of 4096 bytes
    pub fn new(capacity: NonZeroUsize, max_memory_usage: usize) -> LassoResult<Self> {
        Ok(Self {
            // Allocate one bucket
            buckets: AtomicBucketList::new(capacity)?,
            bucket_capacity: AtomicUsize::new(capacity.get()),
            // The current capacity is whatever size the bucket we just allocated is
            memory_usage: AtomicUsize::new(capacity.get()),
            max_memory_usage: AtomicUsize::new(max_memory_usage),
        })
    }

    #[inline]
    pub(crate) fn current_memory_usage(&self) -> usize {
        self.memory_usage.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn set_max_memory_usage(&self, max_memory_usage: usize) {
        self.max_memory_usage
            .store(max_memory_usage, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn get_max_memory_usage(&self) -> usize {
        self.max_memory_usage.load(Ordering::Relaxed)
    }

    fn set_bucket_capacity(&self, capacity: usize) {
        debug_assert_ne!(capacity, 0);
        self.bucket_capacity.store(capacity, Ordering::Relaxed);
    }

    /// Doesn't actually allocate anything, but increments `self.memory_usage` and returns `None` if
    /// the attempted amount surpasses `max_memory_usage`
    // TODO: Make this return a `Result`
    fn allocate_memory(&self, requested_mem: usize) -> LassoResult<()> {
        if self.memory_usage.load(Ordering::Relaxed) + requested_mem
            > self.max_memory_usage.load(Ordering::Relaxed)
        {
            Err(LassoError::new(LassoErrorKind::MemoryLimitReached))
        } else {
            self.memory_usage
                .fetch_add(requested_mem, Ordering::Relaxed);

            Ok(())
        }
    }

    /// Store a slice in the Arena, returning `None` if memory is exhausted
    ///
    /// # Safety
    ///
    /// The reference passed back must be dropped before the arena that created it is
    ///
    pub unsafe fn store_str(&self, string: &str) -> LassoResult<&'static str> {
        // If the string is empty, simply return an empty string.
        // This ensures that only strings with lengths greater
        // than zero will be allocated within the arena
        if string.is_empty() {
            return Ok("");
        }

        let slice = string.as_bytes();
        debug_assert_ne!(slice.len(), 0);

        // Iterate over all of the buckets within the list while attempting to find one
        // that has enough space to fit our string within it
        //
        // This is a tradeoff between allocation speed and memory usage. As-is we prioritize
        // allocation speed in exchange for potentially missing possible reuse situations
        // and then allocating more memory than is strictly necessary. In practice this shouldn't
        // really matter, but it's worth that the opposite tradeoff can be made by adding bounded
        // retries within this loop, the worst-case performance suffers in exchange for potentially
        // better memory usage.
        for bucket in self.buckets.iter() {
            if let Ok(start) = bucket.try_inc_length(slice.len()) {
                // Safety: We now have exclusive access to `bucket[start..start + slice.len()]`
                let allocated = unsafe { bucket.slice_mut(start) };
                // Copy the given slice into the allocation
                unsafe { allocated.copy_from_nonoverlapping(slice.as_ptr(), slice.len()) };

                // Return the successfully allocated string
                let string = unsafe {
                    str::from_utf8_unchecked(slice::from_raw_parts(allocated, slice.len()))
                };
                return Ok(string);
            }

            // Otherwise the bucket doesn't have sufficient capacity for the string
            // so we carry on searching through allocated buckets
        }

        // If we couldn't find a pre-existing bucket with enough room in it, allocate our own bucket

        let next_capacity = self.bucket_capacity.load(Ordering::Relaxed) * 2;
        debug_assert_ne!(next_capacity, 0);

        // If the current string's length is greater than the doubled current capacity, allocate a bucket exactly the
        // size of the large string and push it back in the buckets vector. This ensures that obscenely large strings will
        // not permanently affect the resource consumption of the interner
        if slice.len() > next_capacity {
            // Check that we haven't exhausted our memory limit
            self.allocate_memory(slice.len())?;

            // Safety: `len` will never be zero since we explicitly handled zero-length strings
            //         at the beginning of the function
            let non_zero_len = unsafe { NonZeroUsize::new_unchecked(slice.len()) };
            debug_assert_ne!(slice.len(), 0);

            let mut bucket = AtomicBucket::with_capacity(non_zero_len)?;

            // Safety: The new bucket will have exactly enough room for the string and we have
            //         exclusive access to the bucket since we just created it
            let allocated_string = unsafe { bucket.push_slice(slice) };
            self.buckets.push_front(bucket.into_ref());

            Ok(allocated_string)
        } else {
            let memory_usage = self.current_memory_usage();
            let max_memory_usage = self.get_max_memory_usage();

            // If trying to use the doubled capacity will surpass our memory limit, just allocate as much as we can
            if memory_usage + next_capacity > max_memory_usage {
                let remaining_memory = max_memory_usage.saturating_sub(memory_usage);

                // Check that we haven't exhausted our memory limit
                self.allocate_memory(remaining_memory)?;

                // Set the capacity to twice of what it currently is to allow for fewer allocations as more strings are interned
                let mut bucket = AtomicBucket::with_capacity(
                    NonZeroUsize::new(remaining_memory)
                        .ok_or_else(|| LassoError::new(LassoErrorKind::MemoryLimitReached))?,
                )?;

                // Safety: The new bucket will have exactly enough room for the string and we have
                //         exclusive access to the bucket since we just created it
                let allocated_string = unsafe { bucket.push_slice(slice) };
                // TODO: Push the bucket to the back or something so that we can get it somewhat out
                //       of the search path, reduce the `n` in the `O(n)` list traversal
                self.buckets.push_front(bucket.into_ref());

                Ok(allocated_string)

            // Otherwise just allocate a normal doubled bucket
            } else {
                // Check that we haven't exhausted our memory limit
                self.allocate_memory(next_capacity)?;

                // Set the capacity to twice of what it currently is to allow for fewer allocations as more strings are interned
                self.set_bucket_capacity(next_capacity);

                // Safety: `next_capacity` will never be zero
                let capacity = unsafe { NonZeroUsize::new_unchecked(next_capacity) };
                debug_assert_ne!(next_capacity, 0);

                let mut bucket = AtomicBucket::with_capacity(capacity)?;

                // Safety: The new bucket will have enough room for the string
                let allocated_string = unsafe { bucket.push_slice(slice) };
                self.buckets.push_front(bucket.into_ref());

                Ok(allocated_string)
            }
        }
    }
}

impl Default for LockfreeArena {
    fn default() -> Self {
        Self::new(
            Capacity::default().bytes,
            MemoryLimits::default().max_memory_usage,
        )
        .expect("failed to create default arena")
    }
}

impl Debug for LockfreeArena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct TotalBuckets(usize);

        impl Debug for TotalBuckets {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                if self.0 == 1 {
                    f.write_str("...1 bucket")
                } else {
                    write!(f, "...{} buckets", self.0)
                }
            }
        }

        f.debug_struct("Arena")
            .field("buckets", &TotalBuckets(self.buckets.len()))
            .field(
                "bucket_capacity",
                &self.bucket_capacity.load(Ordering::Relaxed),
            )
            .field("memory_usage", &self.memory_usage.load(Ordering::Relaxed))
            .field(
                "max_memory_usage",
                &self.max_memory_usage.load(Ordering::Relaxed),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string() {
        let arena = LockfreeArena::default();

        unsafe {
            let idx = arena.store_str("test");

            assert_eq!(idx, Ok("test"));
        }
    }

    #[test]
    fn empty_str() {
        let arena = LockfreeArena::default();

        unsafe {
            let zst = arena.store_str("");
            let zst1 = arena.store_str("");
            let zst2 = arena.store_str("");

            assert_eq!(zst, Ok(""));
            assert_eq!(zst1, Ok(""));
            assert_eq!(zst2, Ok(""));
        }
    }

    #[test]
    fn exponential_allocations() {
        let arena = LockfreeArena::default();

        let mut len = 4096;
        for _ in 0..10 {
            let large_string = "a".repeat(len);
            let arena_string = unsafe { arena.store_str(&large_string) };
            assert_eq!(arena_string, Ok(large_string.as_str()));

            len *= 2;
        }
    }

    #[test]
    fn memory_exhausted() {
        let arena = LockfreeArena::new(NonZeroUsize::new(10).unwrap(), 10).unwrap();

        unsafe {
            assert!(arena.store_str("0123456789").is_ok());

            // ZSTs take up zero bytes
            arena.store_str("").unwrap();

            let err = arena.store_str("a").unwrap_err();
            assert!(err.kind().is_memory_limit());

            let err = arena.store_str("dfgsagdfgsdf").unwrap_err();
            assert!(err.kind().is_memory_limit());
        }
    }

    #[test]
    fn allocate_too_much() {
        let arena = LockfreeArena::new(NonZeroUsize::new(1).unwrap(), 10).unwrap();

        unsafe {
            let err = arena.store_str("abcdefghijklmnopqrstuvwxyz").unwrap_err();
            assert!(err.kind().is_memory_limit());
        }
    }

    #[test]
    fn allocate_more_than_double() {
        let arena = LockfreeArena::new(NonZeroUsize::new(1).unwrap(), 1000).unwrap();

        unsafe {
            assert!(arena.store_str("abcdefghijklmnopqrstuvwxyz").is_ok());
        }
    }
}
