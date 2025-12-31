use crate::{
    arenas::{AnyArena, Arena},
    hasher::RandomState,
    keys::{Key, Spur},
    reader::RodeoReader,
    resolver::RodeoResolver,
    util::{Iter, Strings},
    Capacity, LassoError, LassoErrorKind, LassoResult, MemoryLimits,
};
use alloc::vec::Vec;
use core::{cmp::max, hash::BuildHasher, iter::FromIterator, num::NonZeroUsize, ops::Index};
use hashbrown::{
    hash_map::{RawEntryMut, RawVacantEntryMut},
    HashMap,
};

compile! {
    if #[feature = "serialize"] {
        use alloc::string::String;
        use serde::{
            de::{Deserialize, Deserializer},
            ser::{Serialize, Serializer},
        };
    }
}

/// The map we use to associate keys to strings by the string's hash
type StringMap<K> = HashMap<K, (), ()>;

/// A string interner that caches strings quickly with a minimal memory footprint,
/// returning a unique key to re-access it with `O(1)` times.
///
/// By default Rodeo uses the [`Spur`] type for keys and [`RandomState`] as its hasher
///
/// [`Spur`]: crate::Spur
/// [`RandomState`]: https://doc.rust-lang.org/std/collections/hash_map/struct.RandomState.html
#[derive(Debug)]
pub struct Rodeo<K = Spur, S = RandomState> {
    /// Map that allows `str` -> `key` resolution
    ///
    /// This must be a `HashMap` (for now) since `raw_api`s are only available for maps and not sets.
    /// The value of the map is `()` since the key is symbolically hashed as the string it represents and
    /// the hasher is also `()` so that we only store one hasher, the custom one contained in the `Rodeo` itself
    ///
    /// The keys stored in this map are not hashed as keys, they're inserted
    /// with the hashes of the strings that they point to
    ///
    /// For example, if the string `foo` has the key of `FooKey` and the hash of `0xF00`,
    /// then the hashmap will contain `FooKey` at the hashed location of `0xF00`.
    ///
    /// This allows us to only store references to the internally allocated strings once,
    /// which drastically decreases memory usage
    map: StringMap<K>,
    /// The hasher of the map. This is stored outside of the map so that we can use
    /// custom hashing on the keys of the map without the map itself trying to do something else
    hasher: S,
    /// Vec that allows `key` -> `str` resolution
    pub(crate) strings: Vec<&'static str>,
    /// The arena that holds all allocated strings
    arena: Arena,
}

impl<K> Rodeo<K, RandomState>
where
    K: Key,
{
    /// Create a new Rodeo
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{Rodeo, Spur};
    ///
    /// let mut rodeo: Rodeo<Spur> = Rodeo::new();
    /// let hello = rodeo.get_or_intern("Hello, ");
    /// let world = rodeo.get_or_intern("World!");
    ///
    /// assert_eq!("Hello, ", rodeo.resolve(&hello));
    /// assert_eq!("World!", rodeo.resolve(&world));
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

    /// Create a new Rodeo with the specified capacity. The interner will be able to hold `capacity`
    /// strings without reallocating
    ///
    /// See [`Capacity`] for more information
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{Rodeo, Capacity, Spur};
    ///
    /// let rodeo: Rodeo<Spur> = Rodeo::with_capacity(Capacity::for_strings(10));
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

    /// Create a new Rodeo with the specified memory limits. The interner will be able to hold `max_memory_usage`
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
    /// use lasso::{Rodeo, MemoryLimits, Spur};
    ///
    /// let rodeo: Rodeo<Spur> = Rodeo::with_memory_limits(MemoryLimits::for_memory_usage(4096));
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

    /// Create a new Rodeo with the specified capacity and memory limits. The interner will be able to hold `max_memory_usage`
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
    /// use lasso::{Rodeo, MemoryLimits, Spur};
    ///
    /// let rodeo: Rodeo<Spur> = Rodeo::with_memory_limits(MemoryLimits::for_memory_usage(4096));
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

impl<K, S> Rodeo<K, S>
where
    K: Key,
    S: BuildHasher,
{
    /// Creates an empty Rodeo which will use the given hasher for its internal hashmap
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{Spur, Rodeo};
    /// use std::collections::hash_map::RandomState;
    ///
    /// let rodeo: Rodeo<Spur, RandomState> = Rodeo::with_hasher(RandomState::new());
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

    /// Creates a new Rodeo with the specified capacity that will use the given hasher for its internal hashmap
    ///
    /// See [`Capacity`] for more information
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{Spur, Capacity, Rodeo};
    /// use std::collections::hash_map::RandomState;
    ///
    /// let rodeo: Rodeo<Spur, RandomState> = Rodeo::with_capacity_and_hasher(Capacity::for_strings(10), RandomState::new());
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

    /// Creates a new Rodeo with the specified capacity and memory limits that will use the given hasher for its internal hashmap
    ///
    /// See [`Capacity`] and [`MemoryLimits`] for more information
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{Spur, Capacity, MemoryLimits, Rodeo};
    /// use std::collections::hash_map::RandomState;
    ///
    /// let rodeo: Rodeo<Spur, RandomState> = Rodeo::with_capacity_memory_limits_and_hasher(
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
            map: HashMap::with_capacity_and_hasher(strings, ()),
            hasher: hash_builder,
            strings: Vec::with_capacity(strings),
            arena: Arena::new(bytes, max_memory_usage)
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
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
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
    /// [`Spur`]: crate::Spur
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn get_or_intern<T>(&mut self, val: T) -> K
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
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
    ///
    /// // Interned the string
    /// let key = rodeo.try_get_or_intern("Strings of things with wings and dings").unwrap();
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    ///
    /// // No string was interned, as it was already contained
    /// let key = rodeo.try_get_or_intern("Strings of things with wings and dings").unwrap();
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_get_or_intern<T>(&mut self, val: T) -> LassoResult<K>
    where
        T: AsRef<str>,
    {
        let Self {
            map,
            hasher,
            strings,
            arena,
        } = self;

        let string_slice: &str = val.as_ref();

        // Make a hash of the requested string
        let hash = hasher.hash_one(string_slice);

        // Get the map's entry that the string should occupy
        let key = match get_string_entry_mut(map, strings, hash, string_slice) {
            // The string already exists, so return its key
            RawEntryMut::Occupied(entry) => *entry.into_key(),

            // The string does not yet exist, so insert it and create its key
            RawEntryMut::Vacant(entry) => {
                // Create the key from the vec's index that the string will hold
                let key = K::try_from_usize(strings.len())
                    .ok_or_else(|| LassoError::new(LassoErrorKind::KeySpaceExhaustion))?;

                // Allocate the string in the arena
                // Safety: The returned strings will be dropped before the arena that created them is
                let allocated = unsafe { arena.store_str(string_slice)? };

                // Push the allocated string to the strings vector
                strings.push(allocated);

                // Insert the key with the hash of the string that it points to, reusing the hash we made earlier
                insert_string(entry, strings, hasher, hash, key);

                key
            }
        };

        Ok(key)
    }

    /// Get the key for a static string, interning it if it does not yet exist
    ///
    /// This will not reallocate or copy the given string
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
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
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
    pub fn get_or_intern_static(&mut self, string: &'static str) -> K {
        self.try_get_or_intern_static(string)
            .expect("Failed to get or intern static string")
    }

    /// Get the key for a static string, interning it if it does not yet exist
    ///
    /// This will not reallocate or copy the given string
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
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
    pub fn try_get_or_intern_static(&mut self, string: &'static str) -> LassoResult<K> {
        let Self {
            map,
            hasher,
            strings,
            ..
        } = self;

        // Make a hash of the requested string
        let hash = hasher.hash_one(string);

        // Get the map's entry that the string should occupy
        let key = match get_string_entry_mut(map, strings, hash, string) {
            // The string already exists, so return its key
            RawEntryMut::Occupied(entry) => *entry.into_key(),

            // The string does not yet exist, so insert it and create its key
            RawEntryMut::Vacant(entry) => {
                // Create the key from the vec's index that the string will hold
                let key = K::try_from_usize(strings.len())
                    .ok_or_else(|| LassoError::new(LassoErrorKind::KeySpaceExhaustion))?;

                // Push the static string to the strings vector
                strings.push(string);

                // Insert the key with the hash of the string that it points to, reusing the hash we made earlier
                insert_string(entry, strings, hasher, hash, key);

                key
            }
        };

        Ok(key)
    }

    /// Get the key value of a string, returning `None` if it doesn't exist
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
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
        let string_slice: &str = val.as_ref();

        // Make a hash of the requested string
        let hash = self.hasher.hash_one(string_slice);

        // Get the map's entry that the string should occupy
        self.map
            .raw_entry()
            .from_hash(hash, |key| {
                // Safety: The index given by `key` will be in bounds of the strings vector
                let key_string: &str = unsafe { index_unchecked!(self.strings, key.into_usize()) };

                // Compare the requested string against the key's string
                string_slice == key_string
            })
            .map(|(&key, _)| key)
    }

    /// Returns `true` if the given string has been interned
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
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
}

/// Gets a mutable entry for the given string using its hash
#[inline]
fn get_string_entry_mut<'a, K>(
    map: &'a mut StringMap<K>,
    strings: &[&str],
    hash: u64,
    target: &str,
) -> RawEntryMut<'a, K, (), ()>
where
    K: Key,
{
    map.raw_entry_mut().from_hash(hash, |key| {
        // Safety: The index given by `key` will be in bounds of the strings vector
        let key_string: &str = unsafe { index_unchecked!(strings, key.into_usize()) };

        // Compare the requested string against the key's string
        target == key_string
    })
}

/// Inserts a string into a vacant entry using its given hash
#[inline]
fn insert_string<K, S>(
    entry: RawVacantEntryMut<K, (), ()>,
    strings: &[&str],
    hasher: &S,
    hash: u64,
    key: K,
) where
    K: Key,
    S: BuildHasher,
{
    entry.insert_with_hasher(hash, key, (), |key| {
        // Safety: The index given by `key` will be in bounds of the strings vector
        let key_string: &str = unsafe { index_unchecked!(strings, key.into_usize()) };

        // Insert the string with the given hash
        hasher.hash_one(key_string)
    });
}

impl<K, S> Rodeo<K, S>
where
    K: Key,
{
    /// Returns `true` if the given key exists in the current interner
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    /// # use lasso::{Key, Spur};
    ///
    /// let mut rodeo = Rodeo::default();
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
        key.into_usize() < self.strings.len()
    }

    /// Resolves a string by its key. Only keys made by the current Rodeo may be used
    ///
    /// # Panics
    ///
    /// Panics if the key is out of bounds
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
    ///
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn resolve<'a>(&'a self, key: &K) -> &'a str {
        // Safety: The call to get_unchecked's safety relies on the Key::into_usize impl
        // being symmetric and the caller having not fabricated a key. If the impl is sound
        // and symmetric, then it will succeed, as the usize used to create it is a valid
        // index into self.strings
        unsafe {
            assert!(key.into_usize() < self.strings.len());
            self.strings.get_unchecked(key.into_usize())
        }
    }

    /// Resolves a string by its key, returning `None` if it's out of bounds. Only keys made by the
    /// current Rodeo may be used
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
    ///
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// assert_eq!(Some("Strings of things with wings and dings"), rodeo.try_resolve(&key));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_resolve<'a>(&'a self, key: &K) -> Option<&'a str> {
        // Safety: The call to get_unchecked's safety relies on the Key::into_usize impl
        // being symmetric and the caller having not fabricated a key. If the impl is sound
        // and symmetric, then it will succeed, as the usize used to create it is a valid
        // index into self.strings
        unsafe {
            if key.into_usize() < self.strings.len() {
                Some(self.strings.get_unchecked(key.into_usize()))
            } else {
                None
            }
        }
    }

    /// Resolves a string by its key, without bounds checks
    ///
    /// # Safety
    ///
    /// The key must be valid for the current interner
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
    ///
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// unsafe {
    ///     assert_eq!("Strings of things with wings and dings", rodeo.resolve_unchecked(&key));
    /// }
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn resolve_unchecked<'a>(&'a self, key: &K) -> &'a str {
        unsafe { self.strings.get_unchecked(key.into_usize()) }
    }
}

impl<K, S> Rodeo<K, S> {
    /// Gets the number of interned strings
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
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
    /// use lasso::Rodeo;
    ///
    /// let rodeo = Rodeo::default();
    /// assert!(rodeo.is_empty());
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clears the current interner, invalidating all previously interned keys
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Somewhere over the rainbow...");
    ///
    /// // We can see that the interner currently contains one string
    /// assert_eq!(rodeo.len(), 1);
    /// // And that resolving that string works as expected
    /// assert_eq!(rodeo.resolve(&key), "Somewhere over the rainbow...");
    ///
    /// // But after we clear it...
    /// rodeo.clear();
    ///
    /// // We see it's now empty
    /// assert_eq!(rodeo.len(), 0);
    /// // And that resolving the previously interned string now returns nothing
    /// assert_eq!(rodeo.try_resolve(&key), None);
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn clear(&mut self) {
        self.map.clear();
        self.strings.clear();
        self.arena.clear();
    }

    /// Returns the number of strings that can be interned without a reallocation
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::{Spur, Capacity, Rodeo};
    ///
    /// let rodeo: Rodeo<Spur> = Rodeo::with_capacity(Capacity::for_strings(10));
    /// assert_eq!(rodeo.capacity(), 10);
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn capacity(&self) -> usize {
        self.strings.capacity()
    }

    // TODO: Examples here

    /// Returns an iterator over the interned strings and their key values
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn iter(&self) -> Iter<'_, K> {
        Iter::from_rodeo(self)
    }

    /// Returns an iterator over the interned strings
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn strings(&self) -> Strings<'_, K> {
        Strings::from_rodeo(self)
    }

    /// Set the `Rodeo`'s maximum memory usage while in-flight
    ///
    /// Note that setting the maximum memory usage to below the currently allocated
    /// memory will do nothing
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn set_memory_limits(&mut self, memory_limits: MemoryLimits) {
        self.arena.max_memory_usage = memory_limits.max_memory_usage;
    }

    /// Get the `Rodeo`'s currently allocated memory
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn current_memory_usage(&self) -> usize {
        self.arena.memory_usage()
    }

    /// Get the `Rodeo`'s current maximum of allocated memory
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn max_memory_usage(&self) -> usize {
        self.arena.max_memory_usage
    }
}

impl<K, S> Rodeo<K, S> {
    /// Consumes the current Rodeo, returning a [`RodeoReader`] to allow contention-free access of the interner
    /// from multiple threads
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Appear weak when you are strong, and strong when you are weak.");
    ///
    /// let read_only_rodeo = rodeo.into_reader();
    /// assert_eq!(
    ///     "Appear weak when you are strong, and strong when you are weak.",
    ///     read_only_rodeo.resolve(&key),
    /// );
    /// ```
    ///
    /// [`RodeoReader`]: crate::RodeoReader
    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    pub fn into_reader(self) -> RodeoReader<K, S> {
        let Self {
            map,
            hasher,
            strings,
            arena,
        } = self;

        // Safety: No other references outside of `map` and `strings` to the interned strings exist
        unsafe { RodeoReader::new(map, hasher, strings, AnyArena::Arena(arena)) }
    }

    /// Consumes the current Rodeo, returning a [`RodeoResolver`] to allow contention-free access of the interner
    /// from multiple threads with the lowest possible memory consumption
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Appear weak when you are strong, and strong when you are weak.");
    ///
    /// let resolver_rodeo = rodeo.into_resolver();
    /// assert_eq!(
    ///     "Appear weak when you are strong, and strong when you are weak.",
    ///     resolver_rodeo.resolve(&key),
    /// );
    /// ```
    ///
    /// [`RodeoResolver`]: crate::RodeoResolver
    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    pub fn into_resolver(self) -> RodeoResolver<K> {
        let Rodeo { strings, arena, .. } = self;

        // Safety: No other references to the strings exist
        unsafe { RodeoResolver::new(strings, AnyArena::Arena(arena)) }
    }
}

/// Creates a Rodeo using [`Spur`] as its key and [`RandomState`] as its hasher
///
/// [`Spur`]: crate::Spur
/// [`RandomState`]: index.html#cargo-features
impl Default for Rodeo<Spur, RandomState> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl<K: Send, S: Send> Send for Rodeo<K, S> {}

impl<Str, K, S> FromIterator<Str> for Rodeo<K, S>
where
    Str: AsRef<str>,
    K: Key,
    S: BuildHasher + Default,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = Str>,
    {
        let iter = iter.into_iter();
        let (lower, upper) = iter.size_hint();
        let mut interner = Self::with_capacity_and_hasher(
            Capacity::for_strings(upper.unwrap_or(lower)),
            Default::default(),
        );

        for string in iter {
            interner.get_or_intern(string.as_ref());
        }

        interner
    }
}

impl<K, S> Index<K> for Rodeo<K, S>
where
    K: Key,
    S: BuildHasher,
{
    type Output = str;

    #[cfg_attr(feature = "inline-more", inline)]
    fn index(&self, idx: K) -> &Self::Output {
        self.resolve(&idx)
    }
}

impl<K, S, T> Extend<T> for Rodeo<K, S>
where
    K: Key,
    S: BuildHasher,
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

impl<'a, K: Key, S> IntoIterator for &'a Rodeo<K, S> {
    type Item = (K, &'a str);
    type IntoIter = Iter<'a, K>;

    #[cfg_attr(feature = "inline-more", inline)]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<K, S> Eq for Rodeo<K, S> {}

impl<K, S> PartialEq<Self> for Rodeo<K, S> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &Self) -> bool {
        self.strings == other.strings
    }
}

impl<K, S> PartialEq<RodeoReader<K, S>> for Rodeo<K, S> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &RodeoReader<K, S>) -> bool {
        self.strings == other.strings
    }
}

impl<K, S> PartialEq<RodeoResolver<K>> for Rodeo<K, S> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &RodeoResolver<K>) -> bool {
        self.strings == other.strings
    }
}

/// Clones all of the strings in `source` into the given arena, strings vec and string map
fn clone_strings_into<K, S>(
    source: &[&str],
    arena: &mut Arena,
    strings: &mut Vec<&'static str>,
    map: &mut StringMap<K>,
    hasher: &S,
) -> LassoResult<()>
where
    K: Key,
    S: BuildHasher,
{
    for (idx, source_str) in source.iter().enumerate() {
        // Allocate the string in the new arena
        let allocated = unsafe { arena.store_str(source_str)? };

        // Push the newly allocated string to the `strings` vec
        strings.push(allocated);

        // Hash the allocated string
        let hash = hasher.hash_one(allocated);

        // Insert the allocated string into the string map
        match get_string_entry_mut(map, strings, hash, allocated) {
            RawEntryMut::Vacant(vacant) => {
                let key = K::try_from_usize(idx)
                    .ok_or_else(|| LassoError::new(LassoErrorKind::KeySpaceExhaustion))?;
                insert_string(vacant, strings, hasher, hash, key);
            }

            RawEntryMut::Occupied(_) => {
                unreachable!("keys should be unique within cloned Rodeos")
            }
        }
    }

    Ok(())
}

impl<K, S> Rodeo<K, S>
where
    K: Key,
    S: BuildHasher + Clone,
{
    /// Attempts to clone a new `Rodeo` from the current one, equivalent to [`Clone::clone`]
    /// with the added option of error handling
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_clone(&self) -> LassoResult<Self> {
        // We can figure out the exact size of all strings contained in the current interner
        // which will allow us to allocate a single bucket that exactly fits those strings,
        // minimizing allocations
        let required_capacity =
            NonZeroUsize::new(self.strings.iter().copied().map(str::len).sum::<usize>())
                .unwrap_or(Capacity::default().bytes);

        // Allocate a new arena to fit all strings in
        let mut arena = Arena::new(
            required_capacity,
            max(self.arena.max_memory_usage, required_capacity.get()),
        )?;

        // Allocate all strings contained within the interner within the new arena while
        // also inserting the allocated strings into the new map
        let (mut strings, mut map, hasher) = (
            Vec::with_capacity(self.strings.len()),
            StringMap::<K>::with_capacity_and_hasher(self.map.len(), ()),
            self.hasher.clone(),
        );
        clone_strings_into(&self.strings, &mut arena, &mut strings, &mut map, &hasher)?;

        Ok(Self {
            map,
            hasher,
            strings,
            arena,
        })
    }

    /// Attempts to clone a new `Rodeo` from `source` while reusing the current `Rodeo`'s buffers,
    /// equivalent to [`Clone::clone_from`] with the added option of error handling.
    ///
    /// In the event that this function returns an error, `self` is in an unspecified state
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_clone_from(&mut self, source: &Self) -> LassoResult<()> {
        // Clear the current interner
        self.clear();
        self.hasher = source.hasher.clone();

        // Reserve capacity for the cloned-over strings
        self.strings
            .try_reserve(source.strings.len())
            .map_err(|_| LassoError::new(LassoErrorKind::FailedAllocation))?;

        self.map
            .raw_table_mut()
            .try_reserve(source.map.len(), |_| {
                unreachable!("the target Rodeo's map should be empty while resizing");
            })
            .map_err(|_| LassoError::new(LassoErrorKind::FailedAllocation))?;

        // Clone the values into the target interner
        clone_strings_into(
            &source.strings,
            &mut self.arena,
            &mut self.strings,
            &mut self.map,
            &self.hasher,
        )?;

        Ok(())
    }
}

impl<K, S> Clone for Rodeo<K, S>
where
    K: Key,
    S: BuildHasher + Clone,
{
    #[inline]
    fn clone(&self) -> Self {
        self.try_clone().expect("failed to clone Rodeo")
    }

    #[inline]
    fn clone_from(&mut self, source: &Self) {
        self.try_clone_from(source)
            .expect("failed to clone Rodeo from existing Rodeo");
    }
}

#[cfg(feature = "serialize")]
impl<K, H> Serialize for Rodeo<K, H> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Serialize all of self as a `Vec<String>`
        self.strings.serialize(serializer)
    }
}

#[cfg(feature = "serialize")]
impl<'de, K: Key, S: BuildHasher + Default> Deserialize<'de> for Rodeo<K, S> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let vector: Vec<String> = Vec::deserialize(deserializer)?;
        let capacity = {
            let total_bytes = vector.iter().map(|s| s.len()).sum::<usize>();
            let total_bytes =
                NonZeroUsize::new(total_bytes).unwrap_or_else(|| Capacity::default().bytes());

            Capacity::new(vector.len(), total_bytes)
        };

        let hasher: S = Default::default();
        let mut strings = Vec::with_capacity(capacity.strings);
        let mut map = HashMap::with_capacity_and_hasher(capacity.strings, ());
        let mut arena =
            Arena::new(capacity.bytes, usize::MAX).expect("failed to allocate memory for interner");

        for (key, string) in vector.into_iter().enumerate() {
            let allocated = unsafe {
                arena
                    .store_str(&string)
                    .expect("failed to allocate enough memory")
            };

            let hash = hasher.hash_one(allocated);

            // Get the map's entry that the string should occupy
            let entry = map.raw_entry_mut().from_hash(hash, |key: &K| {
                // Safety: The index given by `key` will be in bounds of the strings vector
                let key_string: &str = unsafe { index_unchecked!(strings, key.into_usize()) };

                // Compare the requested string against the key's string
                allocated == key_string
            });

            match entry {
                RawEntryMut::Occupied(..) => {
                    debug_assert!(false, "re-interned a key while deserializing");
                }
                RawEntryMut::Vacant(entry) => {
                    // Create the key from the vec's index that the string will hold
                    let key =
                        K::try_from_usize(key).expect("failed to create key while deserializing");

                    // Push the allocated string to the strings vector
                    strings.push(allocated);

                    // Insert the key with the hash of the string that it points to, reusing the hash we made earlier
                    entry.insert_with_hasher(hash, key, (), |key| {
                        let key_string: &str =
                            unsafe { index_unchecked!(strings, key.into_usize()) };

                        hasher.hash_one(key_string)
                    });
                }
            }
        }

        Ok(Self {
            map,
            hasher,
            strings,
            arena,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{hasher::RandomState, keys::MicroSpur, Capacity, Key, MemoryLimits, Rodeo, Spur};
    use core::num::NonZeroUsize;

    compile! {
        if #[feature = "no-std"] {
            use alloc::{string::ToString, vec::Vec, boxed::Box};
        }
    }

    #[test]
    fn new() {
        let mut rodeo: Rodeo<Spur> = Rodeo::new();
        rodeo.get_or_intern("Test");
    }

    #[test]
    fn with_capacity() {
        let mut rodeo: Rodeo<Spur> = Rodeo::with_capacity(Capacity::for_strings(10));
        assert_eq!(rodeo.capacity(), 10);

        rodeo.get_or_intern("Test");
        rodeo.get_or_intern("Test1");
        rodeo.get_or_intern("Test2");
        rodeo.get_or_intern("Test3");
        rodeo.get_or_intern("Test4");
        rodeo.get_or_intern("Test5");
        rodeo.get_or_intern("Test6");
        rodeo.get_or_intern("Test7");
        rodeo.get_or_intern("Test8");
        rodeo.get_or_intern("Test9");

        assert_eq!(rodeo.len(), rodeo.capacity());
    }

    #[test]
    fn with_hasher() {
        let mut rodeo: Rodeo<Spur, RandomState> = Rodeo::with_hasher(RandomState::new());
        let key = rodeo.get_or_intern("Test");
        assert_eq!("Test", rodeo.resolve(&key));

        #[cfg(not(miri))]
        {
            let mut rodeo: Rodeo<Spur, ahash::RandomState> =
                Rodeo::with_hasher(ahash::RandomState::new());
            let key = rodeo.get_or_intern("Test");
            assert_eq!("Test", rodeo.resolve(&key));
        }
    }

    #[test]
    fn with_capacity_and_hasher() {
        let mut rodeo: Rodeo<Spur, RandomState> =
            Rodeo::with_capacity_and_hasher(Capacity::for_strings(10), RandomState::new());
        assert_eq!(rodeo.capacity(), 10);

        rodeo.get_or_intern("Test");
        rodeo.get_or_intern("Test1");
        rodeo.get_or_intern("Test2");
        rodeo.get_or_intern("Test3");
        rodeo.get_or_intern("Test4");
        rodeo.get_or_intern("Test5");
        rodeo.get_or_intern("Test6");
        rodeo.get_or_intern("Test7");
        rodeo.get_or_intern("Test8");
        rodeo.get_or_intern("Test9");

        assert_eq!(rodeo.len(), rodeo.capacity());

        #[cfg(not(miri))]
        {
            let mut rodeo: Rodeo<Spur, ahash::RandomState> = Rodeo::with_capacity_and_hasher(
                Capacity::for_strings(10),
                ahash::RandomState::new(),
            );
            assert_eq!(rodeo.capacity(), 10);

            rodeo.get_or_intern("Test");
            rodeo.get_or_intern("Test1");
            rodeo.get_or_intern("Test2");
            rodeo.get_or_intern("Test3");
            rodeo.get_or_intern("Test4");
            rodeo.get_or_intern("Test5");
            rodeo.get_or_intern("Test6");
            rodeo.get_or_intern("Test7");
            rodeo.get_or_intern("Test8");
            rodeo.get_or_intern("Test9");

            assert_eq!(rodeo.len(), rodeo.capacity());
        }
    }

    #[test]
    fn get_or_intern() {
        let mut rodeo = Rodeo::default();
        let a = rodeo.get_or_intern("A");
        assert_eq!(a, rodeo.get_or_intern("A"));

        let b = rodeo.get_or_intern("B");
        assert_eq!(b, rodeo.get_or_intern("B"));

        let c = rodeo.get_or_intern("C");
        assert_eq!(c, rodeo.get_or_intern("C"));
    }

    #[test]
    fn try_get_or_intern() {
        let mut rodeo: Rodeo<MicroSpur> = Rodeo::new();

        for i in 0..u8::MAX as usize - 1 {
            rodeo.get_or_intern(i.to_string());
        }

        let space = rodeo.try_get_or_intern("A").unwrap();
        assert_eq!(Ok(space), rodeo.try_get_or_intern("A"));
        assert_eq!("A", rodeo.resolve(&space));

        assert!(rodeo.try_get_or_intern("C").is_err());
    }

    #[test]
    fn get_or_intern_static() {
        let mut rodeo = Rodeo::default();
        let a = rodeo.get_or_intern_static("A");
        assert_eq!(a, rodeo.get_or_intern_static("A"));

        let b = rodeo.get_or_intern_static("B");
        assert_eq!(b, rodeo.get_or_intern_static("B"));

        let c = rodeo.get_or_intern_static("C");
        assert_eq!(c, rodeo.get_or_intern_static("C"));
    }

    #[test]
    fn try_get_or_intern_static() {
        let mut strings = Vec::new();
        let mut rodeo: Rodeo<MicroSpur> = Rodeo::new();

        for i in 0..u8::MAX as usize - 1 {
            let ptr = Box::into_raw(i.to_string().into_boxed_str());
            strings.push(ptr);

            let static_ref = unsafe { &*ptr };
            rodeo.get_or_intern_static(static_ref);
        }

        let space = rodeo.try_get_or_intern_static("A").unwrap();
        assert_eq!(Ok(space), rodeo.try_get_or_intern_static("A"));
        assert_eq!("A", rodeo.resolve(&space));

        assert!(rodeo.try_get_or_intern_static("C").is_err());

        drop(rodeo);
        for string in strings {
            unsafe { drop(Box::from_raw(string)) };
        }
    }

    #[test]
    fn get() {
        let mut rodeo = Rodeo::default();
        let key = rodeo.get_or_intern("A");

        assert_eq!(Some(key), rodeo.get("A"));
    }

    #[test]
    fn resolve() {
        let mut rodeo = Rodeo::default();
        let key = rodeo.get_or_intern("A");

        assert_eq!("A", rodeo.resolve(&key));
    }

    #[test]
    #[should_panic]
    #[cfg(not(miri))]
    fn resolve_panics() {
        let rodeo = Rodeo::default();
        rodeo.resolve(&Spur::try_from_usize(100).unwrap());
    }

    #[test]
    fn try_resolve() {
        let mut rodeo = Rodeo::default();
        let key = rodeo.get_or_intern("A");

        assert_eq!(Some("A"), rodeo.try_resolve(&key));
        assert_eq!(None, rodeo.try_resolve(&Spur::try_from_usize(100).unwrap()));
    }

    #[test]
    fn resolve_unchecked() {
        let mut rodeo = Rodeo::default();
        let key = rodeo.get_or_intern("A");

        unsafe {
            assert_eq!("A", rodeo.resolve_unchecked(&key));
        }
    }

    #[test]
    fn len() {
        let mut rodeo = Rodeo::default();
        rodeo.get_or_intern("A");
        rodeo.get_or_intern("B");
        rodeo.get_or_intern("C");

        assert_eq!(rodeo.len(), 3);
    }

    #[test]
    fn empty() {
        let rodeo = Rodeo::default();

        assert!(rodeo.is_empty());
    }

    #[test]
    fn clone_rodeo() {
        let mut rodeo = Rodeo::default();
        let key = rodeo.get_or_intern("Test");

        assert_eq!("Test", rodeo.resolve(&key));

        let mut cloned = rodeo.clone();
        assert_eq!("Test", cloned.resolve(&key));

        drop(rodeo);

        assert_eq!("Test", cloned.resolve(&key));

        let extra = cloned.get_or_intern("wheeee");
        assert_eq!("wheeee", cloned.resolve(&extra));
    }

    #[test]
    fn clone_from_rodeo() {
        let mut rodeo = Rodeo::default();
        let key = rodeo.get_or_intern("Test");

        assert_eq!("Test", rodeo.resolve(&key));

        let mut sacrificial = Rodeo::default();
        rodeo.get_or_intern("a");
        rodeo.get_or_intern("b");
        rodeo.get_or_intern("c");
        rodeo.get_or_intern_static("gdfgsdogjdskgrhehdh");

        sacrificial.clone_from(&rodeo);
        assert_eq!("Test", sacrificial.resolve(&key));

        drop(rodeo);

        assert_eq!("Test", sacrificial.resolve(&key));

        let extra = sacrificial.get_or_intern("wheeee");
        assert_eq!("wheeee", sacrificial.resolve(&extra));
    }

    #[test]
    fn drop_rodeo() {
        let _ = Rodeo::default();
    }

    #[test]
    fn iter() {
        let mut rodeo = Rodeo::default();
        let a = rodeo.get_or_intern("a");
        let b = rodeo.get_or_intern("b");
        let c = rodeo.get_or_intern("c");

        let mut rodeo = rodeo.iter();
        assert_eq!(Some((a, "a")), rodeo.next());
        assert_eq!(Some((b, "b")), rodeo.next());
        assert_eq!(Some((c, "c")), rodeo.next());
        assert_eq!(None, rodeo.next());
    }

    #[test]
    fn strings() {
        let mut rodeo = Rodeo::default();
        rodeo.get_or_intern("a");
        rodeo.get_or_intern("b");
        rodeo.get_or_intern("c");

        let mut rodeo = rodeo.strings();
        assert_eq!(Some("a"), rodeo.next());
        assert_eq!(Some("b"), rodeo.next());
        assert_eq!(Some("c"), rodeo.next());
        assert_eq!(None, rodeo.next());
    }

    #[test]
    #[cfg(not(any(feature = "no-std", feature = "ahasher")))]
    fn debug() {
        let rodeo = Rodeo::default();
        println!("{:?}", rodeo);
    }

    // Regression test for https://github.com/Kixiron/lasso/issues/7
    #[test]
    fn wrong_keys() {
        let mut rodeo = Rodeo::default();

        rodeo.get_or_intern("a");
        rodeo.get_or_intern("b");
        rodeo.get_or_intern("c");
        rodeo.get_or_intern("d");
        rodeo.get_or_intern("e");
        rodeo.get_or_intern("f");
        rodeo.get_or_intern("g");
        rodeo.get_or_intern("h");
        rodeo.get_or_intern("i");
        rodeo.get_or_intern("j");
        rodeo.get_or_intern("k");
        rodeo.get_or_intern("l");
        rodeo.get_or_intern("m");
        rodeo.get_or_intern("n");
        rodeo.get_or_intern("o");
        rodeo.get_or_intern("p");
        rodeo.get_or_intern("q");
        rodeo.get_or_intern("r");
        rodeo.get_or_intern("s");
        rodeo.get_or_intern("t");
        rodeo.get_or_intern("u");
        rodeo.get_or_intern("v");
        rodeo.get_or_intern("w");
        rodeo.get_or_intern("x");
        rodeo.get_or_intern("y");
        rodeo.get_or_intern("z");
        rodeo.get_or_intern("aa");
        rodeo.get_or_intern("bb");
        rodeo.get_or_intern("cc");
        rodeo.get_or_intern("dd");
        rodeo.get_or_intern("ee");
        rodeo.get_or_intern("ff");
        rodeo.get_or_intern("gg");
        rodeo.get_or_intern("hh");
        rodeo.get_or_intern("ii");
        rodeo.get_or_intern("jj");
        rodeo.get_or_intern("kk");
        rodeo.get_or_intern("ll");
        rodeo.get_or_intern("mm");
        rodeo.get_or_intern("nn");
        rodeo.get_or_intern("oo");
        rodeo.get_or_intern("pp");
        rodeo.get_or_intern("qq");
        rodeo.get_or_intern("rr");
        rodeo.get_or_intern("ss");
        rodeo.get_or_intern("tt");
        rodeo.get_or_intern("uu");
        rodeo.get_or_intern("vv");
        rodeo.get_or_intern("ww");
        rodeo.get_or_intern("xx");
        rodeo.get_or_intern("yy");
        rodeo.get_or_intern("zz");
        rodeo.get_or_intern("aaa");
        rodeo.get_or_intern("bbb");
        rodeo.get_or_intern("ccc");

        let var = rodeo.get_or_intern("ddd");

        rodeo.get_or_intern("eee");

        let var2 = rodeo.get_or_intern("ddd");
        assert_eq!(var, var2);
    }

    #[test]
    fn memory_exhausted() {
        let mut rodeo: Rodeo<Spur> = Rodeo::with_capacity_and_memory_limits(
            Capacity::for_bytes(NonZeroUsize::new(10).unwrap()),
            MemoryLimits::for_memory_usage(10),
        );

        let string = rodeo.try_get_or_intern("0123456789").unwrap();
        assert_eq!(rodeo.resolve(&string), "0123456789");

        assert!(rodeo.try_get_or_intern("1").is_err());
        assert!(rodeo.try_get_or_intern("1").is_err());
        assert!(rodeo.try_get_or_intern("1").is_err());

        assert_eq!(rodeo.resolve(&string), "0123456789");
    }

    // TODO: Add a reason for should_panic once `Result`s are used
    #[test]
    #[should_panic]
    fn memory_exhausted_panics() {
        let mut rodeo: Rodeo<Spur> = Rodeo::with_capacity_and_memory_limits(
            Capacity::for_bytes(NonZeroUsize::new(10).unwrap()),
            MemoryLimits::for_memory_usage(10),
        );

        let string = rodeo.get_or_intern("0123456789");
        assert_eq!(rodeo.resolve(&string), "0123456789");

        rodeo.get_or_intern("1");
    }

    #[test]
    fn with_capacity_memory_limits_and_hasher() {
        let mut rodeo: Rodeo<Spur, RandomState> = Rodeo::with_capacity_memory_limits_and_hasher(
            Capacity::default(),
            MemoryLimits::default(),
            RandomState::new(),
        );

        rodeo.get_or_intern("Test");
    }

    #[test]
    fn with_capacity_and_memory_limits() {
        let mut rodeo: Rodeo<Spur> =
            Rodeo::with_capacity_and_memory_limits(Capacity::default(), MemoryLimits::default());

        rodeo.get_or_intern("Test");
    }

    #[test]
    fn set_memory_limits() {
        let mut rodeo: Rodeo<Spur> = Rodeo::with_capacity_and_memory_limits(
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
        let mut rodeo: Rodeo<Spur> = Rodeo::with_capacity_and_memory_limits(
            Capacity::for_bytes(NonZeroUsize::new(10).unwrap()),
            MemoryLimits::for_memory_usage(10),
        );

        rodeo.get_or_intern("0123456789");

        assert_eq!(rodeo.current_memory_usage(), 10);
        assert_eq!(rodeo.max_memory_usage(), 10);
    }

    #[test]
    fn contains() {
        let mut rodeo = Rodeo::default();

        assert!(!rodeo.contains(""));
        rodeo.get_or_intern("");

        assert!(rodeo.contains(""));
        assert!(rodeo.contains(""));
    }

    #[test]
    fn contains_key() {
        let mut rodeo = Rodeo::default();

        assert!(!rodeo.contains(""));
        let key = rodeo.get_or_intern("");

        assert!(rodeo.contains(""));
        assert!(rodeo.contains_key(&key));

        assert!(!rodeo.contains_key(&Spur::try_from_usize(10000).unwrap()));
    }

    #[test]
    fn from_iter() {
        let rodeo: Rodeo = ["a", "b", "c", "d", "e"].iter().collect();

        assert!(rodeo.contains("a"));
        assert!(rodeo.contains("b"));
        assert!(rodeo.contains("c"));
        assert!(rodeo.contains("d"));
        assert!(rodeo.contains("e"));
    }

    #[test]
    fn index() {
        let mut rodeo = Rodeo::default();
        let key = rodeo.get_or_intern("A");

        assert_eq!("A", &rodeo[key]);
    }

    #[test]
    fn extend() {
        let mut rodeo = Rodeo::default();
        assert!(rodeo.is_empty());

        rodeo.extend(["a", "b", "c", "d", "e"].iter());
        assert!(rodeo.contains("a"));
        assert!(rodeo.contains("b"));
        assert!(rodeo.contains("c"));
        assert!(rodeo.contains("d"));
        assert!(rodeo.contains("e"));
    }

    #[test]
    fn into_iterator() {
        let rodeo: Rodeo = ["a", "b", "c", "d", "e"].iter().collect();

        for ((key, string), (expected_key, expected_string)) in rodeo.into_iter().zip(
            [(0usize, "a"), (1, "b"), (2, "c"), (3, "d"), (4, "e")]
                .iter()
                .copied(),
        ) {
            assert_eq!(key, Spur::try_from_usize(expected_key).unwrap());
            assert_eq!(string, expected_string);
        }
    }

    #[test]
    #[cfg(feature = "serialize")]
    fn empty_serialize() {
        let rodeo = Rodeo::default();

        let ser = serde_json::to_string(&rodeo).unwrap();
        let ser2 = serde_json::to_string(&rodeo).unwrap();
        assert_eq!(ser, ser2);

        let deser: Rodeo = serde_json::from_str(&ser).unwrap();
        assert!(deser.is_empty());
        let deser2: Rodeo = serde_json::from_str(&ser2).unwrap();
        assert!(deser2.is_empty());
    }

    #[test]
    #[cfg(feature = "serialize")]
    fn filled_serialize() {
        let mut rodeo = Rodeo::default();
        let a = rodeo.get_or_intern("a");
        let b = rodeo.get_or_intern("b");
        let c = rodeo.get_or_intern("c");
        let d = rodeo.get_or_intern("d");

        let ser = serde_json::to_string(&rodeo).unwrap();
        let ser2 = serde_json::to_string(&rodeo).unwrap();
        assert_eq!(ser, ser2);

        let deser: Rodeo = serde_json::from_str(&ser).unwrap();
        let deser2: Rodeo = serde_json::from_str(&ser2).unwrap();

        for (((correct_key, correct_str), (key1, str1)), (key2, str2)) in
            [(a, "a"), (b, "b"), (c, "c"), (d, "d")]
                .iter()
                .copied()
                .zip(&deser)
                .zip(&deser2)
        {
            assert_eq!(correct_key, key1);
            assert_eq!(correct_key, key2);

            assert_eq!(correct_str, str1);
            assert_eq!(correct_str, str2);
        }
    }

    #[test]
    fn rodeo_eq() {
        let a = Rodeo::default();
        let b = Rodeo::default();
        assert_eq!(a, b);

        let mut a = Rodeo::default();
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
        let a = Rodeo::default();
        let b = Rodeo::default().into_resolver();
        assert_eq!(a, b);

        let mut a = Rodeo::default();
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
        let a = Rodeo::default();
        let b = Rodeo::default().into_reader();
        assert_eq!(a, b);

        let mut a = Rodeo::default();
        a.get_or_intern("a");
        a.get_or_intern("b");
        a.get_or_intern("c");
        let mut b = Rodeo::default();
        b.get_or_intern("a");
        b.get_or_intern("b");
        b.get_or_intern("c");
        assert_eq!(a, b.into_reader());
    }
}
