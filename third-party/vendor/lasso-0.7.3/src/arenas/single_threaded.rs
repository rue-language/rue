use crate::{
    arenas::bucket::Bucket, Capacity, LassoError, LassoErrorKind, LassoResult, MemoryLimits,
};
use alloc::{format, vec, vec::Vec};
use core::{fmt, num::NonZeroUsize};

/// An arena allocator that dynamically grows in size when needed, allocating memory in large chunks
pub(crate) struct Arena {
    /// All the internal buckets, storing all allocated and unallocated items
    buckets: Vec<Bucket>,
    /// The default capacity of each bucket
    bucket_capacity: NonZeroUsize,
    memory_usage: usize,
    pub(crate) max_memory_usage: usize,
}

impl Arena {
    /// Create a new Arena with the default bucket size of 4096 bytes
    pub fn new(capacity: NonZeroUsize, max_memory_usage: usize) -> LassoResult<Self> {
        Ok(Self {
            // Allocate one bucket
            buckets: vec![Bucket::with_capacity(capacity)?],
            bucket_capacity: capacity,
            // The current capacity is whatever size the bucket we just allocated is
            memory_usage: capacity.get(),
            max_memory_usage,
        })
    }

    pub const fn memory_usage(&self) -> usize {
        self.memory_usage
    }

    /// Marks all buckets as being totally unused, meaning they have their current
    /// capacity but all memory they contain is treated as freshly allocated and
    /// therefore valid to be allocated
    pub fn clear(&mut self) {
        for bucket in &mut self.buckets {
            bucket.clear();
        }
    }

    /// Doesn't actually allocate anything, but increments `self.memory_usage` and returns `None` if
    /// the attempted amount surpasses `max_memory_usage`
    // TODO: Make this return a `Result`
    fn allocate_memory(&mut self, requested_mem: usize) -> LassoResult<()> {
        if self.memory_usage + requested_mem > self.max_memory_usage {
            Err(LassoError::new(LassoErrorKind::MemoryLimitReached))
        } else {
            self.memory_usage += requested_mem;

            Ok(())
        }
    }

    /// Store a slice in the Arena, returning `None` if memory is exhausted
    ///
    /// # Safety
    ///
    /// The reference passed back must be dropped before the arena that created it is
    ///
    pub unsafe fn store_str(&mut self, string: &str) -> LassoResult<&'static str> {
        // If the string is empty, simply return an empty string.
        // This ensures that only strings with lengths greater
        // than zero will be allocated within the arena
        if string.is_empty() {
            return Ok("");
        }

        let slice = string.as_bytes();
        let len = slice.len();
        debug_assert_ne!(len, 0);

        if let Some(bucket) = self
            .buckets
            .last_mut()
            .filter(|bucket| bucket.free_elements() >= len)
        {
            // Safety: The bucket found has enough room for the slice
            let allocated = unsafe { bucket.push_slice(slice) };

            return Ok(allocated);
        }

        // SPEED: This portion of the code could be pulled into a cold path

        let next_capacity = self.bucket_capacity.get() * 2;

        // If the current string's length is greater than the doubled current capacity, allocate a bucket exactly the
        // size of the large string and push it back in the buckets vector. This ensures that obscenely large strings will
        // not permanently affect the resource consumption of the interner
        if len > next_capacity {
            // Check that we haven't exhausted our memory limit
            self.allocate_memory(len)?;

            // Safety: len will always be >= 1
            let mut bucket = Bucket::with_capacity(unsafe { NonZeroUsize::new_unchecked(len) })?;

            // Safety: The new bucket will have exactly enough room for the string
            let allocated_string = unsafe { bucket.push_slice(slice) };
            self.buckets
                .insert(self.buckets.len().saturating_sub(2), bucket);

            Ok(allocated_string)

        // If trying to use the doubled capacity will surpass our memory limit, just allocate as much as we can
        } else if self.memory_usage + next_capacity > self.max_memory_usage {
            let remaining_memory = self.max_memory_usage.saturating_sub(self.memory_usage);
            // Check that we haven't exhausted our memory limit
            self.allocate_memory(remaining_memory)?;

            // Set the capacity to twice of what it currently is to allow for fewer allocations as more strings are interned
            let mut bucket = Bucket::with_capacity(
                NonZeroUsize::new(remaining_memory)
                    .ok_or_else(|| LassoError::new(LassoErrorKind::MemoryLimitReached))?,
            )?;

            // Safety: The new bucket will have enough room for the string
            let allocated_string = unsafe { bucket.push_slice(slice) };
            self.buckets.push(bucket);

            Ok(allocated_string)

        // Otherwise just allocate a normal doubled bucket
        } else {
            // Check that we haven't exhausted our memory limit
            self.allocate_memory(next_capacity)?;

            // Set the capacity to twice of what it currently is to allow for fewer allocations as more strings are interned
            // Safety: capacity will always be >= 1
            self.bucket_capacity = unsafe { NonZeroUsize::new_unchecked(next_capacity) };
            let mut bucket = Bucket::with_capacity(self.bucket_capacity)?;

            // Safety: The new bucket will have enough room for the string
            let allocated_string = unsafe { bucket.push_slice(slice) };
            self.buckets.push(bucket);

            Ok(allocated_string)
        }
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new(
            Capacity::default().bytes,
            MemoryLimits::default().max_memory_usage,
        )
        .expect("failed to create arena")
    }
}

impl fmt::Debug for Arena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arena")
            .field(
                "buckets",
                &format!(
                    "... {} bucket{}",
                    self.buckets.len(),
                    if self.buckets.len() == 1 { "" } else { "s" },
                ),
            )
            .field("bucket_capacity", &self.bucket_capacity)
            .field("memory_usage", &self.memory_usage)
            .field("max_memory_usage", &self.max_memory_usage)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string() {
        let mut arena = Arena::default();

        unsafe {
            let idx = arena.store_str("test");

            assert_eq!(idx, Ok("test"));
        }
    }

    #[test]
    fn empty_str() {
        let mut arena = Arena::default();

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
        let mut arena = Arena::default();

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
        let mut arena = Arena::new(NonZeroUsize::new(10).unwrap(), 10).unwrap();

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
        let mut arena = Arena::new(NonZeroUsize::new(1).unwrap(), 10).unwrap();

        unsafe {
            let err = arena.store_str("abcdefghijklmnopqrstuvwxyz").unwrap_err();
            assert!(err.kind().is_memory_limit());
        }
    }

    #[test]
    fn allocate_more_than_double() {
        let mut arena = Arena::new(NonZeroUsize::new(1).unwrap(), 1000).unwrap();

        unsafe {
            assert!(arena.store_str("abcdefghijklmnopqrstuvwxyz").is_ok());
        }
    }
}
