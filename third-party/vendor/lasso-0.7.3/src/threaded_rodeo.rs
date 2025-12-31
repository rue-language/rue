use crate::{
    arenas::{AnyArena, LockfreeArena},
    hasher::RandomState,
    keys::{Key, Spur},
    reader::RodeoReader,
    resolver::RodeoResolver,
    Capacity, LassoError, LassoErrorKind, LassoResult, MemoryLimits, Rodeo,
};
use core::{
    fmt::{Debug, Formatter, Result as FmtResult},
    hash::{BuildHasher, Hash},
    iter::{self, FromIterator},
    ops::Index,
    sync::atomic::{AtomicUsize, Ordering},
};
use dashmap::{mapref::entry::Entry, DashMap, SharedValue};
use hashbrown::{hash_map::RawEntryMut, HashMap};

macro_rules! index_unchecked_mut {
    ($slice:expr, $idx:expr) => {{
        let elem: &mut _ = if cfg!(debug_assertions) {
            &mut $slice[$idx]
        } else {
            $slice.get_unchecked_mut($idx)
        };

        elem
    }};
}

/// A concurrent string interner that caches strings quickly with a minimal memory footprint,
/// returning a unique key to re-access it with `O(1)` internment and resolution.
///
/// This struct is only available with the `multi-threaded` feature!
/// By default ThreadedRodeo uses the [`Spur`] type for keys and [`RandomState`] as the hasher
///
/// [`Spur`]: crate::Spur
/// [`ahash::RandomState`]: https://docs.rs/ahash/0.3.2/ahash/struct.RandomState.html
/// [`RandomState`]: index.html#cargo-features
pub struct ThreadedRodeo<K = Spur, S = RandomState> {
    // TODO: Should this be migrated over to the scheme that `Rodeo` uses for string storage?
    //       Need benchmarks to see the perf impact of two dashmap lookups and see if that's worth
    //       the storage impact of extra string pointers lying around
    /// Map that allows str to key resolution
    map: DashMap<&'static str, K, S>,
    /// Map that allows key to str resolution
    pub(crate) strings: DashMap<K, &'static str, S>,
    /// The current key value
    key: AtomicUsize,
    /// The arena where all strings are stored
    arena: LockfreeArena,
}

// TODO: More parity functions with std::HashMap

impl<K> ThreadedRodeo<K, RandomState>
where
    K: Key + Hash,
{
    /// Create a new ThreadedRodeo
    ///
    /// # Example
    ///
    /// ```rust
    /// # #[cfg(miri)]
    /// # fn main() {}
    /// #
    /// # #[cfg(not(miri))]
    /// # fn main() {
    /// use lasso::{ThreadedRodeo, Spur};
    /// use std::{thread, sync::Arc};
    ///
    /// let lasso: Arc<ThreadedRodeo<Spur>> = Arc::new(ThreadedRodeo::new());
    /// let hello = lasso.get_or_intern("Hello, ");
    ///
    /// let l = Arc::clone(&lasso);
    /// let world = thread::spawn(move || {
    ///     l.get_or_intern("World!")
    /// })
    /// .join()
    /// .unwrap();
    ///
    /// assert_eq!("Hello, ", lasso.resolve(&hello));
    /// assert_eq!("World!", lasso.resolve(&world));
    /// # }
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn new() -> Self {
        Self::with_capacity_memory_limits_and_hasher(
            Capacity::default(),
            MemoryLimits::default(),
            RandomState::new(),
        )
    }

    /// Create a new ThreadedRodeo with the specified capacity. The interner will be able to hold `capacity`
    /// strings without reallocating. If capacity is 0, the interner will not allocate.
    ///
    /// See [`Capacity`] for more details
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{ThreadedRodeo, Capacity, Spur};
    ///
    /// let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_capacity(Capacity::for_strings(10));
    /// ```
    ///
    /// [`Capacity`]: crate::Capacity
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn with_capacity(capacity: Capacity) -> Self {
        Self::with_capacity_memory_limits_and_hasher(
            capacity,
            MemoryLimits::default(),
            RandomState::new(),
        )
    }

    /// Create a new ThreadedRodeo with the specified memory limits. The interner will be able to hold `max_memory_usage`
    /// bytes of interned strings until it will start returning `None` from `try_get_or_intern` or panicking from
    /// `get_or_intern`.
    ///
    /// Note: If the capacity of the interner is greater than the memory limit, then that will be the effective maximum
    /// for allocated memory
    ///
    /// See [`MemoryLimits`] for more information
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{ThreadedRodeo, MemoryLimits, Spur};
    ///
    /// let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_memory_limits(MemoryLimits::for_memory_usage(4096));
    /// ```
    ///
    /// [`MemoryLimits`]: crate::MemoryLimits
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn with_memory_limits(memory_limits: MemoryLimits) -> Self {
        Self::with_capacity_memory_limits_and_hasher(
            Capacity::default(),
            memory_limits,
            RandomState::new(),
        )
    }

    /// Create a new ThreadedRodeo with the specified capacity and memory limits. The interner will be able to hold `max_memory_usage`
    /// bytes of interned strings until it will start returning `None` from `try_get_or_intern` or panicking from
    /// `get_or_intern`.
    ///
    /// Note: If the capacity of the interner is greater than the memory limit, then that will be the effective maximum
    /// for allocated memory
    ///
    /// See [`Capacity`] [`MemoryLimits`] for more information
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{ThreadedRodeo, MemoryLimits, Spur};
    ///
    /// let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_memory_limits(MemoryLimits::for_memory_usage(4096));
    /// ```
    ///
    /// [`Capacity`]: crate::Capacity
    /// [`MemoryLimits`]: crate::MemoryLimits
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn with_capacity_and_memory_limits(
        capacity: Capacity,
        memory_limits: MemoryLimits,
    ) -> Self {
        Self::with_capacity_memory_limits_and_hasher(capacity, memory_limits, RandomState::new())
    }
}

impl<K, S> ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    /// Creates an empty ThreadedRodeo which will use the given hasher for its internal hashmap
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{Spur, ThreadedRodeo};
    /// use std::collections::hash_map::RandomState;
    ///
    /// let rodeo: ThreadedRodeo<Spur, RandomState> = ThreadedRodeo::with_hasher(RandomState::new());
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn with_hasher(hash_builder: S) -> Self {
        Self::with_capacity_memory_limits_and_hasher(
            Capacity::default(),
            MemoryLimits::default(),
            hash_builder,
        )
    }

    /// Creates a new ThreadedRodeo with the specified capacity that will use the given hasher for its internal hashmap
    ///
    /// See [`Capacity`] for more details
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{Spur, Capacity, ThreadedRodeo};
    /// use std::collections::hash_map::RandomState;
    ///
    /// let rodeo: ThreadedRodeo<Spur, RandomState> = ThreadedRodeo::with_capacity_and_hasher(Capacity::for_strings(10), RandomState::new());
    /// ```
    ///
    /// [`Capacity`]: crate::Capacity
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn with_capacity_and_hasher(capacity: Capacity, hash_builder: S) -> Self {
        Self::with_capacity_memory_limits_and_hasher(
            capacity,
            MemoryLimits::default(),
            hash_builder,
        )
    }

    /// Creates a new ThreadedRodeo with the specified capacity and memory limits that will use the given hasher for its internal hashmap
    ///
    /// See [`Capacity`] and [`MemoryLimits`] for more information
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{Spur, Capacity, MemoryLimits, ThreadedRodeo};
    /// use std::collections::hash_map::RandomState;
    ///
    /// let rodeo: ThreadedRodeo<Spur, RandomState> = ThreadedRodeo::with_capacity_memory_limits_and_hasher(
    ///     Capacity::for_strings(10),
    ///     MemoryLimits::for_memory_usage(4096),
    ///     RandomState::new(),
    /// );
    /// ```
    ///
    /// [`Capacity`]: crate::Capacity
    /// [`MemoryLimits`]: crate::MemoryLimits
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn with_capacity_memory_limits_and_hasher(
        capacity: Capacity,
        memory_limits: MemoryLimits,
        hash_builder: S,
    ) -> Self {
        let Capacity { strings, bytes } = capacity;
        let MemoryLimits { max_memory_usage } = memory_limits;

        Self {
            map: DashMap::with_capacity_and_hasher(strings, hash_builder.clone()),
            strings: DashMap::with_capacity_and_hasher(strings, hash_builder),
            key: AtomicUsize::new(0),
            arena: LockfreeArena::new(bytes, max_memory_usage)
                .expect("failed to allocate memory for interner"),
        }
    }

    /// Get the key for a string, interning it if it does not yet exist
    ///
    /// # Panics
    ///
    /// Panics if the key's `try_from_usize` function fails. With the default keys, this means that
    /// you've interned more strings than it can handle. (For [`Spur`] this means that `u32::MAX - 1`
    /// unique strings were interned)
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    ///
    /// // Interned the string
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    ///
    /// // No string was interned, as it was already contained
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn get_or_intern<T>(&self, val: T) -> K
    where
        T: AsRef<str>,
    {
        self.try_get_or_intern(val)
            .expect("Failed to get or intern string")
    }

    /// Get the key for a string, interning it if it does not yet exist
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    ///
    /// // Interned the string
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    ///
    /// // No string was interned, as it was already contained
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_get_or_intern<T>(&self, val: T) -> LassoResult<K>
    where
        T: AsRef<str>,
    {
        let string_slice = val.as_ref();

        if let Some(key) = self.map.get(string_slice) {
            Ok(*key)
        } else {
            // Determine which shard will have our `string_slice` key.
            let hash = self.map.hasher().hash_one(string_slice);
            let shard_key = self.map.determine_shard(hash as usize);
            // Grab the shard and a write lock on it.
            let mut shard = self.map.shards().get(shard_key).unwrap().write();
            // Try getting the value for the `string_slice` key. If we get `Some`, nothing to do.
            // Just return the value, which is the key go to use to resolve the string. If we
            // get `None`, an entry for the string doesn't exist yet. Store string in the arena,
            // update the maps accordingly, and return the key.
            let key = match shard.find_or_find_insert_slot(
                hash,
                |(k, _)| *k == string_slice,
                |(k, _)| self.map.hasher().hash_one(k),
            ) {
                // Safety: occupied_bucket is valid to borrow, which we keep short
                Ok(occupied_bucket) => unsafe { *occupied_bucket.as_ref().1.get() },
                Err(insert_slot) => {
                    // Safety: The drop impl removes all references before the arena is dropped
                    let string: &'static str = unsafe { self.arena.store_str(string_slice)? };

                    let key = K::try_from_usize(self.key.fetch_add(1, Ordering::SeqCst))
                        .ok_or_else(|| LassoError::new(LassoErrorKind::KeySpaceExhaustion))?;

                    self.strings.insert(key, string);
                    // Safety: insert_slot was just returned by find_insert_slot and we have not mutated the shard.
                    unsafe {
                        shard.insert_in_slot(hash, insert_slot, (string, SharedValue::new(key)));
                    }

                    key
                }
            };

            Ok(key)
        }
    }

    /// Get the key for a static string, interning it if it does not yet exist
    ///
    /// This will not reallocate or copy the given string but will instead just store it
    ///
    /// # Panics
    ///
    /// Panics if the key's `try_from_usize` function fails. With the default keys, this means that
    /// you've interned more strings than it can handle. (For [`Spur`] this means that `u32::MAX - 1`
    /// unique strings were interned)
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let mut rodeo = ThreadedRodeo::default();
    ///
    /// // Interned the string
    /// let key = rodeo.get_or_intern_static("Strings of things with wings and dings");
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    ///
    /// // No string was interned, as it was already contained
    /// let key = rodeo.get_or_intern_static("Strings of things with wings and dings");
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn get_or_intern_static(&self, string: &'static str) -> K {
        self.try_get_or_intern_static(string)
            .expect("Failed to get or intern static string")
    }

    /// Get the key for a static string, interning it if it does not yet exist
    ///
    /// This will not reallocate and copy the given string but will instead just store it
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let mut rodeo = ThreadedRodeo::default();
    ///
    /// // Interned the string
    /// let key = rodeo.try_get_or_intern_static("Strings of things with wings and dings").unwrap();
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    ///
    /// // No string was interned, as it was already contained
    /// let key = rodeo.try_get_or_intern_static("Strings of things with wings and dings").unwrap();
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_get_or_intern_static(&self, string: &'static str) -> LassoResult<K> {
        if let Some(key) = self.map.get(string) {
            Ok(*key)
        } else {
            let key = match self.map.entry(string) {
                Entry::Occupied(o) => *o.get(),
                Entry::Vacant(v) => {
                    let key = K::try_from_usize(self.key.fetch_add(1, Ordering::SeqCst))
                        .ok_or_else(|| LassoError::new(LassoErrorKind::KeySpaceExhaustion))?;
                    self.strings.insert(key, string);
                    v.insert(key);

                    key
                }
            };

            Ok(key)
        }
    }

    /// Get the key value of a string, returning `None` if it doesn't exist
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    ///
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert_eq!(Some(key), rodeo.get("Strings of things with wings and dings"));
    ///
    /// assert_eq!(None, rodeo.get("This string isn't interned"));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn get<T>(&self, val: T) -> Option<K>
    where
        T: AsRef<str>,
    {
        self.map.get(val.as_ref()).map(|k| *k)
    }

    /// Returns `true` if the given string has been interned
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    ///
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert!(rodeo.contains("Strings of things with wings and dings"));
    ///
    /// assert!(!rodeo.contains("This string isn't interned"));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn contains<T>(&self, val: T) -> bool
    where
        T: AsRef<str>,
    {
        self.get(val).is_some()
    }

    /// Returns `true` if the given key exists in the current interner
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    /// # use lasso::{Key, Spur};
    ///
    /// let mut rodeo = ThreadedRodeo::default();
    /// # let key_that_doesnt_exist = Spur::try_from_usize(1000).unwrap();
    ///
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert!(rodeo.contains_key(&key));
    ///
    /// assert!(!rodeo.contains_key(&key_that_doesnt_exist));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn contains_key(&self, key: &K) -> bool {
        self.strings.get(key).is_some()
    }

    /// Resolves a string by its key. Only keys made by the current ThreadedRodeo may be used
    ///
    /// # Panics
    ///
    /// Panics if the key is out of bounds
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    ///
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn resolve<'a>(&'a self, key: &K) -> &'a str {
        *self.strings.get(key).expect("Key out of bounds")
    }

    /// Resolves a string by its key, returning `None` if it is out of bounds. Only keys made by the current
    /// ThreadedRodeo may be used
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    ///
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert_eq!(Some("Strings of things with wings and dings"), rodeo.try_resolve(&key));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_resolve<'a>(&'a self, key: &K) -> Option<&'a str> {
        self.strings.get(key).map(|s| *s)
    }

    /// Gets the number of interned strings
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    /// rodeo.get_or_intern("Documentation often has little hidden bits in it");
    ///
    /// assert_eq!(rodeo.len(), 1);
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Returns `true` if there are no currently interned strings
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    /// assert!(rodeo.is_empty());
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the number of strings that can be interned without a reallocation
    ///
    /// This is an unreliable measurement since the underlying hashmap is unreliable in its
    /// capacity measurement
    ///
    /// # Example
    ///
    /// ```no_run
    /// use lasso::{Spur, Capacity, ThreadedRodeo};
    ///
    /// let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_capacity(Capacity::for_strings(10));
    /// assert_eq!(rodeo.capacity(), 10);
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn capacity(&self) -> usize {
        self.strings.capacity()
    }

    /// Returns an iterator over the interned strings and their key values
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn iter(&self) -> Iter<'_, K, S> {
        Iter::new(self)
    }

    /// Returns an iterator over the interned strings
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn strings(&self) -> Strings<'_, K, S> {
        Strings::new(self)
    }

    /// Set the `ThreadedRodeo`'s maximum memory usage while in-flight
    ///
    /// Note that setting the maximum memory usage to below the currently allocated
    /// memory will do nothing
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn set_memory_limits(&self, memory_limits: MemoryLimits) {
        self.arena
            .set_max_memory_usage(memory_limits.max_memory_usage);
    }

    /// Get the `ThreadedRodeo`'s currently allocated memory
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn current_memory_usage(&self) -> usize {
        self.arena.current_memory_usage()
    }

    /// Get the `ThreadedRodeo`'s current maximum of allocated memory
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn max_memory_usage(&self) -> usize {
        self.arena.get_max_memory_usage()
    }

    /// Consumes the current ThreadedRodeo, returning a [`RodeoReader`] to allow contention-free access of the interner
    /// from multiple threads
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    /// let key = rodeo.get_or_intern("Appear weak when you are strong, and strong when you are weak.");
    ///
    /// let rodeo_reader = rodeo.into_reader();
    /// assert_eq!(
    ///     "Appear weak when you are strong, and strong when you are weak.",
    ///     rodeo_reader.resolve(&key),
    /// );
    /// ```
    ///
    /// [`RodeoReader`]: crate::RodeoReader
    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    pub fn into_reader(self) -> RodeoReader<K, S> {
        // Take the strings vec from the old lasso
        let strings: Vec<&'static str> = {
            let mut strings = iter::from_fn(|| Some(None))
                .take(self.strings.len())
                .collect::<Vec<Option<&'static str>>>();

            for shard in self.strings.shards() {
                for (key, val) in shard.write().drain() {
                    unsafe {
                        // Safety: The keys of the dashmap are valid indices
                        *index_unchecked_mut!(strings, key.into_usize()) = Some(val.into_inner());
                    }
                }
            }

            strings.into_iter().map(|s| s.unwrap()).collect()
        };

        // Drain the DashMap by draining each of its buckets and creating a new hashmap to store their values
        let (map, hasher) = {
            let mut map: HashMap<K, (), ()> = HashMap::with_capacity_and_hasher(strings.len(), ());
            let hasher = self.map.hasher().clone();

            for shard in self.map.shards() {
                for (string, key) in shard.write().drain() {
                    let string: &str = string;

                    // Hash the string to use as the key's hash (See `Rodeo`'s documentation for details)
                    let hash = hasher.hash_one(string);

                    // Get the entry of the hashmap and insert the key with our new, custom hash
                    let entry = map.raw_entry_mut().from_hash(hash, |key| {
                        // Safety: The index given by `key` will be in bounds of the strings vector
                        let key_string: &str =
                            unsafe { index_unchecked!(strings, key.into_usize()) };

                        // Compare the requested string against the key's string
                        string == key_string
                    });

                    match entry {
                        RawEntryMut::Occupied(_) => {
                            unreachable!("Keys in the hashmap are unique, so entries should never be occupied");
                        }

                        RawEntryMut::Vacant(entry) => {
                            // Insert the key with the hash of the string that it points to, reusing the hash we made earlier
                            entry.insert_with_hasher(hash, *key.get(), (), |key| {
                                let key_string: &str =
                                    unsafe { index_unchecked!(strings, key.into_usize()) };

                                hasher.hash_one(key_string)
                            });
                        }
                    }
                }
            }

            (map, hasher)
        };

        // Safety: No other references outside of `map` and `strings` to the interned strings exist
        unsafe { RodeoReader::new(map, hasher, strings, AnyArena::Lockfree(self.arena)) }
    }

    /// Consumes the current ThreadedRodeo, returning a [`RodeoResolver`] to allow contention-free access of the interner
    /// from multiple threads with the lowest possible memory consumption
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::ThreadedRodeo;
    ///
    /// let rodeo = ThreadedRodeo::default();
    /// let key = rodeo.get_or_intern("Appear weak when you are strong, and strong when you are weak.");
    ///
    /// let rodeo_resolver = rodeo.into_resolver();
    /// assert_eq!(
    ///     "Appear weak when you are strong, and strong when you are weak.",
    ///     rodeo_resolver.resolve(&key),
    /// );
    /// ```
    ///
    /// [`RodeoResolver`]: crate::RodeoResolver
    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    pub fn into_resolver(self) -> RodeoResolver<K> {
        let mut strings = iter::from_fn(|| Some(None))
            .take(self.strings.len())
            .collect::<Vec<Option<&'static str>>>();

        for shard in self.strings.shards() {
            for (key, val) in shard.write().drain() {
                unsafe {
                    // Safety: The keys of the dashmap are valid indices
                    *index_unchecked_mut!(strings, key.into_usize()) = Some(val.into_inner());
                }
            }
        }

        // Safety: No other references to the strings exist
        unsafe {
            RodeoResolver::new(
                strings.into_iter().map(|s| s.unwrap()).collect(),
                AnyArena::Lockfree(self.arena),
            )
        }
    }
}

/// Creates a ThreadedRodeo using [`Spur`] as its key and [`RandomState`] as its hasher
///
/// [`Spur`]: crate::Spur
/// [`RandomState`]: index.html#cargo-features
impl Default for ThreadedRodeo<Spur, RandomState> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn default() -> Self {
        Self::new()
    }
}

impl<K, S> Debug for ThreadedRodeo<K, S>
where
    K: Key + Hash + Debug,
    S: BuildHasher + Clone,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("ThreadedRodeo")
            .field("map", &self.map)
            .field("strings", &self.strings)
            .field("arena", &self.arena)
            .finish()
    }
}

unsafe impl<K: Sync, S: Sync> Sync for ThreadedRodeo<K, S> {}
unsafe impl<K: Send, S: Send> Send for ThreadedRodeo<K, S> {}

impl<Str, K, S> FromIterator<Str> for ThreadedRodeo<K, S>
where
    Str: AsRef<str>,
    K: Key + Hash,
    S: BuildHasher + Clone + Default,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = Str>,
    {
        let iter = iter.into_iter();
        let (lower, upper) = iter.size_hint();
        let interner = Self::with_capacity_and_hasher(
            Capacity::for_strings(upper.unwrap_or(lower)),
            Default::default(),
        );

        for string in iter {
            interner.get_or_intern(string.as_ref());
        }

        interner
    }
}

impl<K, S> Index<K> for ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    type Output = str;

    #[cfg_attr(feature = "inline-more", inline)]
    fn index(&self, idx: K) -> &Self::Output {
        self.resolve(&idx)
    }
}

impl<K, S, T> Extend<T> for ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
    T: AsRef<str>,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn extend<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = T>,
    {
        for s in iter {
            self.get_or_intern(s.as_ref());
        }
    }
}

impl<K, S> Eq for ThreadedRodeo<K, S>
where
    K: Eq + Hash,
    S: Clone + BuildHasher,
{
}

impl<K, S> PartialEq<Self> for ThreadedRodeo<K, S>
where
    K: Eq + Hash,
    S: Clone + BuildHasher,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &Self) -> bool {
        self.strings.len() == other.strings.len()
            && self.strings.iter().all(|left| {
                other
                    .strings
                    .get(left.key())
                    .map(|s| s.value() == left.value())
                    == Some(true)
            })
    }
}

impl<K, S> PartialEq<Rodeo<K, S>> for ThreadedRodeo<K, S>
where
    K: Eq + Hash + Key,
    S: Clone + BuildHasher,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &Rodeo<K, S>) -> bool {
        self.strings.len() == other.strings.len()
            && other.strings.iter().enumerate().all(|(key, string)| {
                K::try_from_usize(key)
                    .and_then(|key| self.strings.get(&key))
                    .map(|s| s.value() == string)
                    == Some(true)
            })
    }
}

impl<K, S> PartialEq<RodeoReader<K, S>> for ThreadedRodeo<K, S>
where
    K: Eq + Hash + Key,
    S: Clone + BuildHasher,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &RodeoReader<K, S>) -> bool {
        self.strings.len() == other.strings.len()
            && other.strings.iter().enumerate().all(|(key, string)| {
                K::try_from_usize(key)
                    .and_then(|key| self.strings.get(&key))
                    .map(|s| s.value() == string)
                    == Some(true)
            })
    }
}

impl<K, S> PartialEq<RodeoResolver<K>> for ThreadedRodeo<K, S>
where
    K: Eq + Hash + Key,
    S: Clone + BuildHasher,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &RodeoResolver<K>) -> bool {
        self.strings.len() == other.strings.len()
            && other.strings.iter().enumerate().all(|(key, string)| {
                K::try_from_usize(key)
                    .and_then(|key| self.strings.get(&key))
                    .map(|s| s.value() == string)
                    == Some(true)
            })
    }
}

compile! {
    if #[feature = "serialize"] {
        use alloc::string::String;
        use core::num::NonZeroUsize;
        use serde::{
            de::{Deserialize, Deserializer},
            ser::{Serialize, Serializer},
        };
    }
}

#[cfg(feature = "serialize")]
impl<K, H> Serialize for ThreadedRodeo<K, H>
where
    K: Copy + Eq + Hash + Serialize,
    H: Clone + BuildHasher,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Serialize all of self as a `HashMap<String, K>`
        let mut map = HashMap::with_capacity(self.map.len());
        for entry in self.map.iter() {
            map.insert(*entry.key(), entry.value().to_owned());
        }

        map.serialize(serializer)
    }
}

#[cfg(feature = "serialize")]
impl<'de, K, S> Deserialize<'de> for ThreadedRodeo<K, S>
where
    K: Key + Eq + Hash + Deserialize<'de>,
    S: BuildHasher + Clone + Default,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let deser_map: HashMap<String, K> = HashMap::deserialize(deserializer)?;
        let capacity = {
            let total_bytes = deser_map.keys().map(|s| s.len()).sum::<usize>();
            let total_bytes =
                NonZeroUsize::new(total_bytes).unwrap_or_else(|| Capacity::default().bytes());

            Capacity::new(deser_map.len(), total_bytes)
        };

        let hasher = S::default();
        let map = DashMap::with_capacity_and_hasher(capacity.strings, hasher.clone());
        let strings = DashMap::with_capacity_and_hasher(capacity.strings, hasher);
        let mut highest = 0;
        let arena = LockfreeArena::new(capacity.bytes, usize::MAX)
            .expect("failed to allocate memory for interner");

        for (string, key) in deser_map {
            if key.into_usize() > highest {
                highest = key.into_usize();
            }

            let allocated = unsafe {
                arena
                    .store_str(&string)
                    .expect("failed to allocate enough memory")
            };

            map.insert(allocated, key);
            strings.insert(key, allocated);
        }

        Ok(Self {
            map,
            strings,
            key: AtomicUsize::new(highest),
            arena,
        })
    }
}

/// An iterator over an interner's strings and keys
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Iter<'a, K, S> {
    iter: dashmap::iter::Iter<'a, K, &'static str, S, DashMap<K, &'static str, S>>,
}

impl<'a, K, S> Iter<'a, K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    #[cfg_attr(feature = "inline-more", inline)]
    pub(crate) fn new(rodeo: &'a ThreadedRodeo<K, S>) -> Self {
        Self {
            iter: rodeo.strings.iter(),
        }
    }
}

impl<'a, K, S> Iterator for Iter<'a, K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    type Item = (K, &'a str);

    #[cfg_attr(feature = "inline-more", inline)]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|r| (*r.key(), *r.value()))
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<K, S> Debug for Iter<'_, K, S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("Iter").finish_non_exhaustive()
    }
}

/// An iterator over an interner's strings
#[derive(Debug)]
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Strings<'a, K, S> {
    iter: Iter<'a, K, S>,
}

impl<'a, K, S> Strings<'a, K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    #[cfg_attr(feature = "inline-more", inline)]
    pub(crate) fn new(rodeo: &'a ThreadedRodeo<K, S>) -> Self {
        Self {
            iter: Iter::new(rodeo),
        }
    }
}

impl<'a, K, S> Iterator for Strings<'a, K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    type Item = &'a str;

    #[cfg_attr(feature = "inline-more", inline)]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|(_, v)| v)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hasher::RandomState, Capacity, MemoryLimits, MicroSpur};
    use core::num::NonZeroUsize;

    #[cfg(not(any(miri, feature = "no-std")))]
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    compile! {
        if #[feature = "no-std"] {
            use alloc::string::ToString;
        } else {
            use std::string::ToString;
        }
    }

    #[test]
    fn new() {
        let _: ThreadedRodeo<Spur> = ThreadedRodeo::new();
    }

    #[test]
    fn with_capacity() {
        let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_capacity(Capacity::for_strings(10));
        // DashMap's capacity isn't reliable
        let _cap = rodeo.capacity();
    }

    #[test]
    fn with_hasher() {
        let rodeo: ThreadedRodeo<Spur, RandomState> =
            ThreadedRodeo::with_hasher(RandomState::new());

        let key = rodeo.get_or_intern("Test");
        assert_eq!("Test", rodeo.resolve(&key));
    }

    #[test]
    fn with_capacity_and_hasher() {
        let rodeo: ThreadedRodeo<Spur, RandomState> =
            ThreadedRodeo::with_capacity_and_hasher(Capacity::for_strings(10), RandomState::new());

        let key = rodeo.get_or_intern("Test");
        assert_eq!("Test", rodeo.resolve(&key));
    }

    #[test]
    fn get_or_intern() {
        let rodeo = ThreadedRodeo::default();

        let a = rodeo.get_or_intern("A");
        assert_eq!(a, rodeo.get_or_intern("A"));

        let b = rodeo.get_or_intern("B");
        assert_eq!(b, rodeo.get_or_intern("B"));

        let c = rodeo.get_or_intern("C");
        assert_eq!(c, rodeo.get_or_intern("C"));
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn get_or_intern_threaded() {
        let rodeo = Arc::new(ThreadedRodeo::default());

        let moved = Arc::clone(&rodeo);
        thread::spawn(move || {
            let a = moved.get_or_intern("A");
            assert_eq!(a, moved.get_or_intern("A"));

            let b = moved.get_or_intern("B");
            assert_eq!(b, moved.get_or_intern("B"));

            let c = moved.get_or_intern("C");
            assert_eq!(c, moved.get_or_intern("C"));
        });

        let a = rodeo.get_or_intern("A");
        assert_eq!(a, rodeo.get_or_intern("A"));

        let b = rodeo.get_or_intern("B");
        assert_eq!(b, rodeo.get_or_intern("B"));

        let c = rodeo.get_or_intern("C");
        assert_eq!(c, rodeo.get_or_intern("C"));
    }

    #[test]
    fn try_get_or_intern() {
        let rodeo: ThreadedRodeo<MicroSpur> = ThreadedRodeo::new();

        for i in 0..u8::MAX as usize - 1 {
            rodeo.get_or_intern(i.to_string());
        }

        let space = rodeo.try_get_or_intern("A").unwrap();
        assert_eq!("A", rodeo.resolve(&space));
        let space = rodeo.try_get_or_intern("A").unwrap();
        assert_eq!("A", rodeo.resolve(&space));

        assert!(rodeo.try_get_or_intern("C").is_err());
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn try_get_or_intern_threaded() {
        let rodeo: Arc<ThreadedRodeo<MicroSpur>> = Arc::new(ThreadedRodeo::new());

        for i in 0..u8::MAX as usize - 1 {
            rodeo.get_or_intern(i.to_string());
        }

        let moved = Arc::clone(&rodeo);
        thread::spawn(move || {
            let a = moved.try_get_or_intern("A");
            assert_eq!(a, moved.try_get_or_intern("A"));
            let a = moved.try_get_or_intern("A");
            assert_eq!(a, moved.try_get_or_intern("A"));

            let b = moved.try_get_or_intern("B");
            assert_eq!(b, moved.try_get_or_intern("B"));
            let b = moved.try_get_or_intern("B");
            assert_eq!(b, moved.try_get_or_intern("B"));

            let c = moved.try_get_or_intern("C");
            assert_eq!(c, moved.try_get_or_intern("C"));
            let c = moved.try_get_or_intern("C");
            assert_eq!(c, moved.try_get_or_intern("C"));
        });

        let a = rodeo.try_get_or_intern("A");
        assert_eq!(a, rodeo.try_get_or_intern("A"));
        let a = rodeo.try_get_or_intern("A");
        assert_eq!(a, rodeo.try_get_or_intern("A"));

        let b = rodeo.try_get_or_intern("B");
        assert_eq!(b, rodeo.try_get_or_intern("B"));
        let b = rodeo.try_get_or_intern("B");
        assert_eq!(b, rodeo.try_get_or_intern("B"));

        let c = rodeo.try_get_or_intern("C");
        assert_eq!(c, rodeo.try_get_or_intern("C"));
        let c = rodeo.try_get_or_intern("C");
        assert_eq!(c, rodeo.try_get_or_intern("C"));
    }

    #[test]
    fn get_or_intern_static() {
        let rodeo = ThreadedRodeo::default();

        let a = rodeo.get_or_intern_static("A");
        assert_eq!(a, rodeo.get_or_intern_static("A"));

        let b = rodeo.get_or_intern_static("B");
        assert_eq!(b, rodeo.get_or_intern_static("B"));

        let c = rodeo.get_or_intern_static("C");
        assert_eq!(c, rodeo.get_or_intern_static("C"));
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn try_get_or_intern_static_threaded() {
        let rodeo: Arc<ThreadedRodeo<MicroSpur>> = Arc::new(ThreadedRodeo::new());

        let moved = Arc::clone(&rodeo);
        thread::spawn(move || {
            let a = moved.try_get_or_intern_static("A");
            assert_eq!(a, moved.try_get_or_intern("A"));
            let a = moved.try_get_or_intern_static("A");
            assert_eq!(a, moved.try_get_or_intern("A"));

            let b = moved.try_get_or_intern_static("B");
            assert_eq!(b, moved.try_get_or_intern("B"));
            let b = moved.try_get_or_intern_static("B");
            assert_eq!(b, moved.try_get_or_intern("B"));

            let c = moved.try_get_or_intern_static("C");
            assert_eq!(c, moved.try_get_or_intern("C"));
            let c = moved.try_get_or_intern_static("C");
            assert_eq!(c, moved.try_get_or_intern("C"));
        });

        let a = rodeo.try_get_or_intern_static("A");
        assert_eq!(a, rodeo.try_get_or_intern("A"));
        let a = rodeo.try_get_or_intern_static("A");
        assert_eq!(a, rodeo.try_get_or_intern("A"));

        let b = rodeo.try_get_or_intern_static("B");
        assert_eq!(b, rodeo.try_get_or_intern("B"));
        let b = rodeo.try_get_or_intern_static("B");
        assert_eq!(b, rodeo.try_get_or_intern("B"));

        let c = rodeo.try_get_or_intern_static("C");
        assert_eq!(c, rodeo.try_get_or_intern("C"));
        let c = rodeo.try_get_or_intern_static("C");
        assert_eq!(c, rodeo.try_get_or_intern("C"));
    }

    #[test]
    fn get() {
        let rodeo = ThreadedRodeo::default();
        let key = rodeo.get_or_intern("A");

        assert_eq!(Some(key), rodeo.get("A"));
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn get_threaded() {
        let rodeo = Arc::new(ThreadedRodeo::default());
        let key = rodeo.get_or_intern("A");

        let moved = Arc::clone(&rodeo);
        thread::spawn(move || {
            assert_eq!(Some(key), moved.get("A"));
        });

        assert_eq!(Some(key), rodeo.get("A"));
    }

    #[test]
    fn resolve() {
        let rodeo = ThreadedRodeo::default();
        let key = rodeo.get_or_intern("A");

        assert_eq!("A", rodeo.resolve(&key));
    }

    #[test]
    #[should_panic]
    #[cfg(not(miri))]
    fn resolve_panics() {
        let rodeo = ThreadedRodeo::default();
        rodeo.resolve(&Spur::try_from_usize(100).unwrap());
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn resolve_threaded() {
        let rodeo = Arc::new(ThreadedRodeo::default());
        let key = rodeo.get_or_intern("A");

        let moved = Arc::clone(&rodeo);
        thread::spawn(move || {
            assert_eq!("A", moved.resolve(&key));
        });

        assert_eq!("A", rodeo.resolve(&key));
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std", tarpaulin)))]
    fn resolve_panics_threaded() {
        let rodeo = Arc::new(ThreadedRodeo::default());
        let key = rodeo.get_or_intern("A");

        let moved = Arc::clone(&rodeo);
        let handle = thread::spawn(move || {
            assert_eq!("A", moved.resolve(&key));
            moved.resolve(&Spur::try_from_usize(100).unwrap());
        });

        assert_eq!("A", rodeo.resolve(&key));
        assert!(handle.join().is_err());
    }

    #[test]
    fn try_resolve() {
        let rodeo = ThreadedRodeo::default();
        let key = rodeo.get_or_intern("A");

        assert_eq!(Some("A"), rodeo.try_resolve(&key));
        assert_eq!(None, rodeo.try_resolve(&Spur::try_from_usize(100).unwrap()));
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn try_resolve_threaded() {
        let rodeo = Arc::new(ThreadedRodeo::default());
        let key = rodeo.get_or_intern("A");

        let moved = Arc::clone(&rodeo);
        thread::spawn(move || {
            assert_eq!(Some("A"), moved.try_resolve(&key));
            assert_eq!(None, moved.try_resolve(&Spur::try_from_usize(100).unwrap()));
        });

        assert_eq!(Some("A"), rodeo.try_resolve(&key));
        assert_eq!(None, rodeo.try_resolve(&Spur::try_from_usize(100).unwrap()));
    }

    #[test]
    fn len() {
        let rodeo = ThreadedRodeo::default();
        rodeo.get_or_intern("A");
        rodeo.get_or_intern("B");
        rodeo.get_or_intern("C");

        assert_eq!(rodeo.len(), 3);
    }

    #[test]
    fn empty() {
        let rodeo = ThreadedRodeo::default();

        assert!(rodeo.is_empty());
    }

    // #[test]
    // fn clone() {
    //     let rodeo = ThreadedRodeo::default();
    //     let key = rodeo.get_or_intern("Test");
    //
    //     assert_eq!("Test", rodeo.resolve(&key));
    //
    //     let cloned = rodeo.clone();
    //     assert_eq!("Test", cloned.resolve(&key));
    //
    //     drop(rodeo);
    //
    //     assert_eq!("Test", cloned.resolve(&key));
    // }

    #[test]
    fn drops() {
        let _ = ThreadedRodeo::default();
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn drop_threaded() {
        let rodeo = Arc::new(ThreadedRodeo::default());

        let moved = Arc::clone(&rodeo);
        thread::spawn(move || {
            let _ = moved;
        });
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn debug() {
        let rodeo = ThreadedRodeo::default();
        println!("{:?}", rodeo);
    }

    #[test]
    fn iter() {
        let rodeo = ThreadedRodeo::default();
        rodeo.get_or_intern_static("A");
        rodeo.get_or_intern_static("B");
        rodeo.get_or_intern_static("C");
        let values: Vec<_> = rodeo.iter().map(|(k, v)| (k.into_usize(), v)).collect();
        assert_eq!(values.len(), 3);
        assert!(values.contains(&(0, "A")));
        assert!(values.contains(&(1, "B")));
        assert!(values.contains(&(2, "C")));
    }

    #[test]
    fn strings() {
        let rodeo = ThreadedRodeo::default();
        rodeo.get_or_intern_static("A");
        rodeo.get_or_intern_static("B");
        rodeo.get_or_intern_static("C");
        let strings: Vec<_> = rodeo.strings().collect();
        assert_eq!(strings.len(), 3);
        assert!(strings.contains(&"A"));
        assert!(strings.contains(&"B"));
        assert!(strings.contains(&"C"));
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn debug_iter() {
        let rodeo = ThreadedRodeo::default();
        println!("{:?}", rodeo.iter());
        println!("{:#?}", rodeo.iter());
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn debug_strings() {
        let rodeo = ThreadedRodeo::default();
        println!("{:?}", rodeo.strings());
        println!("{:#?}", rodeo.strings());
    }

    #[test]
    fn into_resolver() {
        let rodeo = ThreadedRodeo::default();
        let key = rodeo.get_or_intern("A");

        let resolver = rodeo.into_resolver();
        assert_eq!("A", resolver.resolve(&key));
    }

    #[test]
    fn into_reader() {
        let rodeo = ThreadedRodeo::default();
        let key = rodeo.get_or_intern("A");

        let reader = rodeo.into_reader();
        assert_eq!("A", reader.resolve(&key));
    }

    #[test]
    fn memory_exhausted() {
        let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_capacity_and_memory_limits(
            Capacity::for_bytes(NonZeroUsize::new(10).unwrap()),
            MemoryLimits::for_memory_usage(10),
        );

        let string = rodeo.try_get_or_intern("0123456789").unwrap();
        assert_eq!(rodeo.resolve(&string), "0123456789");

        assert!(rodeo.try_get_or_intern("a").is_err());
        assert!(rodeo.try_get_or_intern("a").is_err());
        assert!(rodeo.try_get_or_intern("a").is_err());

        assert_eq!(rodeo.resolve(&string), "0123456789");
    }

    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn memory_exhausted_threaded() {
        let rodeo: Arc<ThreadedRodeo<Spur>> =
            Arc::new(ThreadedRodeo::with_capacity_and_memory_limits(
                Capacity::for_bytes(NonZeroUsize::new(10).unwrap()),
                MemoryLimits::for_memory_usage(10),
            ));

        let moved = Arc::clone(&rodeo);
        thread::spawn(move || {
            let string = moved.try_get_or_intern("0123456789").unwrap();
            assert_eq!(moved.resolve(&string), "0123456789");

            assert!(moved.try_get_or_intern("a").is_err());
            assert!(moved.try_get_or_intern("a").is_err());
            assert!(moved.try_get_or_intern("a").is_err());

            assert_eq!(moved.resolve(&string), "0123456789");
        });

        let string = rodeo.try_get_or_intern("0123456789").unwrap();
        assert_eq!(rodeo.resolve(&string), "0123456789");

        assert!(rodeo.try_get_or_intern("a").is_err());
        assert!(rodeo.try_get_or_intern("a").is_err());
        assert!(rodeo.try_get_or_intern("a").is_err());

        assert_eq!(rodeo.resolve(&string), "0123456789");
    }

    // TODO: Add a reason for should_panic once `Result`s are used
    #[test]
    #[should_panic]
    fn memory_exhausted_panics() {
        let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_capacity_and_memory_limits(
            Capacity::for_bytes(NonZeroUsize::new(10).unwrap()),
            MemoryLimits::for_memory_usage(10),
        );

        let string = rodeo.get_or_intern("0123456789");
        assert_eq!(rodeo.resolve(&string), "0123456789");

        rodeo.get_or_intern("a");
    }

    #[test]
    fn with_capacity_memory_limits_and_hasher() {
        let rodeo: ThreadedRodeo<Spur, RandomState> =
            ThreadedRodeo::with_capacity_memory_limits_and_hasher(
                Capacity::default(),
                MemoryLimits::default(),
                RandomState::new(),
            );

        rodeo.get_or_intern("Test");
    }

    #[test]
    fn with_capacity_and_memory_limits() {
        let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_capacity_and_memory_limits(
            Capacity::default(),
            MemoryLimits::default(),
        );

        rodeo.get_or_intern("Test");
    }

    #[test]
    fn set_memory_limits() {
        let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_capacity_and_memory_limits(
            Capacity::for_bytes(NonZeroUsize::new(10).unwrap()),
            MemoryLimits::for_memory_usage(10),
        );

        let string1 = rodeo.try_get_or_intern("0123456789").unwrap();
        assert_eq!(rodeo.resolve(&string1), "0123456789");

        assert!(rodeo.try_get_or_intern("a").is_err());
        assert!(rodeo.try_get_or_intern("a").is_err());
        assert!(rodeo.try_get_or_intern("a").is_err());

        assert_eq!(rodeo.resolve(&string1), "0123456789");

        rodeo.set_memory_limits(MemoryLimits::for_memory_usage(20));

        let string2 = rodeo.try_get_or_intern("9876543210").unwrap();
        assert_eq!(rodeo.resolve(&string2), "9876543210");

        assert!(rodeo.try_get_or_intern("a").is_err());
        assert!(rodeo.try_get_or_intern("a").is_err());
        assert!(rodeo.try_get_or_intern("a").is_err());

        assert_eq!(rodeo.resolve(&string1), "0123456789");
        assert_eq!(rodeo.resolve(&string2), "9876543210");
    }

    #[test]
    fn memory_usage_stats() {
        let rodeo: ThreadedRodeo<Spur> = ThreadedRodeo::with_capacity_and_memory_limits(
            Capacity::for_bytes(NonZeroUsize::new(10).unwrap()),
            MemoryLimits::for_memory_usage(10),
        );

        rodeo.get_or_intern("0123456789");

        assert_eq!(rodeo.current_memory_usage(), 10);
        assert_eq!(rodeo.max_memory_usage(), 10);
    }

    #[test]
    fn contains() {
        let rodeo = ThreadedRodeo::default();

        assert!(!rodeo.contains(""));
        rodeo.get_or_intern("");

        assert!(rodeo.contains(""));
        assert!(rodeo.contains(""));
    }

    #[test]
    fn contains_key() {
        let rodeo = ThreadedRodeo::default();

        assert!(!rodeo.contains(""));
        let key = rodeo.get_or_intern("");

        assert!(rodeo.contains(""));
        assert!(rodeo.contains_key(&key));

        assert!(!rodeo.contains_key(&Spur::try_from_usize(10000).unwrap()));
    }

    #[test]
    fn from_iter() {
        let rodeo: ThreadedRodeo = ["a", "b", "c", "d", "e"].iter().collect();

        assert!(rodeo.contains("a"));
        assert!(rodeo.contains("b"));
        assert!(rodeo.contains("c"));
        assert!(rodeo.contains("d"));
        assert!(rodeo.contains("e"));
    }

    #[test]
    fn index() {
        let rodeo = ThreadedRodeo::default();
        let key = rodeo.get_or_intern("A");

        assert_eq!("A", &rodeo[key]);
    }

    #[test]
    fn extend() {
        let mut rodeo = ThreadedRodeo::default();
        assert!(rodeo.is_empty());

        rodeo.extend(["a", "b", "c", "d", "e"].iter());
        assert!(rodeo.contains("a"));
        assert!(rodeo.contains("b"));
        assert!(rodeo.contains("c"));
        assert!(rodeo.contains("d"));
        assert!(rodeo.contains("e"));
    }

    #[test]
    #[cfg(feature = "serialize")]
    fn empty_serialize() {
        let rodeo = ThreadedRodeo::default();

        let ser = serde_json::to_string(&rodeo).unwrap();
        let ser2 = serde_json::to_string(&rodeo).unwrap();
        assert_eq!(ser, ser2);

        let deser: ThreadedRodeo = serde_json::from_str(&ser).unwrap();
        assert!(deser.is_empty());
        let deser2: ThreadedRodeo = serde_json::from_str(&ser2).unwrap();
        assert!(deser2.is_empty());
    }

    #[test]
    #[cfg(feature = "serialize")]
    fn filled_serialize() {
        let rodeo = ThreadedRodeo::default();
        let a = rodeo.get_or_intern("a");
        let b = rodeo.get_or_intern("b");
        let c = rodeo.get_or_intern("c");
        let d = rodeo.get_or_intern("d");

        let ser = serde_json::to_string(&rodeo).unwrap();
        let ser2 = serde_json::to_string(&rodeo).unwrap();

        let deser: ThreadedRodeo = serde_json::from_str(&ser).unwrap();
        let deser2: ThreadedRodeo = serde_json::from_str(&ser2).unwrap();

        for (correct_key, correct_str) in [(a, "a"), (b, "b"), (c, "c"), (d, "d")].iter().copied() {
            assert_eq!(correct_key, deser.get(correct_str).unwrap());
            assert_eq!(correct_key, deser2.get(correct_str).unwrap());

            assert_eq!(correct_str, deser.resolve(&correct_key));
            assert_eq!(correct_str, deser2.resolve(&correct_key));
        }
    }

    #[test]
    fn threaded_rodeo_eq() {
        let a = ThreadedRodeo::default();
        let b = ThreadedRodeo::default();
        assert_eq!(a, b);

        let a = ThreadedRodeo::default();
        a.get_or_intern("a");
        a.get_or_intern("b");
        a.get_or_intern("c");
        let b = ThreadedRodeo::default();
        b.get_or_intern("a");
        b.get_or_intern("b");
        b.get_or_intern("c");
        assert_eq!(a, b);
    }

    #[test]
    fn rodeo_eq() {
        let a = ThreadedRodeo::default();
        let b = Rodeo::default();
        assert_eq!(a, b);

        let a = ThreadedRodeo::default();
        a.get_or_intern("a");
        a.get_or_intern("b");
        a.get_or_intern("c");
        let mut b = Rodeo::default();
        b.get_or_intern("a");
        b.get_or_intern("b");
        b.get_or_intern("c");
        assert_eq!(a, b);
    }

    #[test]
    fn resolver_eq() {
        let a = ThreadedRodeo::default();
        let b = Rodeo::default().into_resolver();
        assert_eq!(a, b);

        let a = ThreadedRodeo::default();
        a.get_or_intern("a");
        a.get_or_intern("b");
        a.get_or_intern("c");
        let mut b = Rodeo::default();
        b.get_or_intern("a");
        b.get_or_intern("b");
        b.get_or_intern("c");
        assert_eq!(a, b.into_resolver());
    }

    #[test]
    fn reader_eq() {
        let a = ThreadedRodeo::default();
        let b = Rodeo::default().into_reader();
        assert_eq!(a, b);

        let a = ThreadedRodeo::default();
        a.get_or_intern("a");
        a.get_or_intern("b");
        a.get_or_intern("c");
        let mut b = Rodeo::default();
        b.get_or_intern("a");
        b.get_or_intern("b");
        b.get_or_intern("c");
        assert_eq!(a, b.into_reader());
    }

    // Test for race conditions on key insertion
    // https://github.com/Kixiron/lasso/issues/18
    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn get_or_intern_threaded_racy() {
        const THREADS: usize = 10;

        let mut handles = Vec::with_capacity(THREADS);
        let barrier = Arc::new(Barrier::new(THREADS));
        let rodeo = Arc::new(ThreadedRodeo::default());
        let expected = Spur::try_from_usize(0).unwrap();

        for _ in 0..THREADS {
            let moved_rodeo = Arc::clone(&rodeo);
            let moved_barrier = Arc::clone(&barrier);

            handles.push(thread::spawn(move || {
                moved_barrier.wait();
                assert_eq!(expected, moved_rodeo.get_or_intern("A"));
                assert_eq!(expected, moved_rodeo.get_or_intern("A"));
                assert_eq!(expected, moved_rodeo.get_or_intern("A"));
                assert_eq!(expected, moved_rodeo.get_or_intern("A"));
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    // Test for race conditions on key insertion
    // https://github.com/Kixiron/lasso/issues/18
    #[test]
    #[cfg(not(any(miri, feature = "no-std")))]
    fn get_or_intern_static_threaded_racy() {
        const THREADS: usize = 10;

        let mut handles = Vec::with_capacity(THREADS);
        let barrier = Arc::new(Barrier::new(THREADS));
        let rodeo = Arc::new(ThreadedRodeo::default());
        let expected = Spur::try_from_usize(0).unwrap();

        for _ in 0..THREADS {
            let moved_rodeo = Arc::clone(&rodeo);
            let moved_barrier = Arc::clone(&barrier);

            handles.push(thread::spawn(move || {
                moved_barrier.wait();
                assert_eq!(expected, moved_rodeo.get_or_intern_static("A"));
                assert_eq!(expected, moved_rodeo.get_or_intern_static("A"));
                assert_eq!(expected, moved_rodeo.get_or_intern_static("A"));
                assert_eq!(expected, moved_rodeo.get_or_intern_static("A"));
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
