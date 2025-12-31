use crate::{
    arenas::AnyArena,
    hasher::RandomState,
    keys::{Key, Spur},
    resolver::RodeoResolver,
    util::{Iter, Strings},
    Rodeo,
};
use alloc::vec::Vec;
use core::{hash::BuildHasher, ops::Index};
use hashbrown::HashMap;

/// A read-only view of a [`Rodeo`] or [`ThreadedRodeo`] that allows contention-free access to interned strings,
/// both key to string resolution and string to key lookups
///
/// The key and hasher types are the same as the `Rodeo` or `ThreadedRodeo` that created it, can be acquired with the
/// `into_reader` methods.
///
/// [`Rodeo`]: crate::Rodeo
/// [`ThreadedRodeo`]: crate::ThreadedRodeo
#[derive(Debug)]
pub struct RodeoReader<K = Spur, S = RandomState> {
    // The logic behind this arrangement is more heavily documented inside of
    // `Rodeo` itself
    map: HashMap<K, (), ()>,
    hasher: S,
    pub(crate) strings: Vec<&'static str>,
    __arena: AnyArena,
}

impl<K, S> RodeoReader<K, S> {
    /// Creates a new RodeoReader
    ///
    /// # Safety
    ///
    /// The references inside of `strings` must be absolutely unique, meaning
    /// that no other references to those strings exist
    ///
    pub(crate) unsafe fn new(
        map: HashMap<K, (), ()>,
        hasher: S,
        strings: Vec<&'static str>,
        arena: AnyArena,
    ) -> Self {
        Self {
            map,
            hasher,
            strings,
            __arena: arena,
        }
    }

    /// Get the key value of a string, returning `None` if it doesn't exist
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// // ThreadedRodeo is interchangeable for Rodeo here
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    ///
    /// let rodeo = rodeo.into_reader();
    /// assert_eq!(Some(key), rodeo.get("Strings of things with wings and dings"));
    ///
    /// assert_eq!(None, rodeo.get("This string isn't interned"));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn get<T>(&self, val: T) -> Option<K>
    where
        T: AsRef<str>,
        S: BuildHasher,
        K: Key,
    {
        let string_slice: &str = val.as_ref();

        // Make a hash of the requested string
        let hash = self.hasher.hash_one(string_slice);

        // Get the map's entry that the string should occupy
        let entry = self.map.raw_entry().from_hash(hash, |key| {
            // Safety: The index given by `key` will be in bounds of the strings vector
            let key_string: &str = unsafe { index_unchecked!(self.strings, key.into_usize()) };

            // Compare the requested string against the key's string
            string_slice == key_string
        });

        entry.map(|(key, ())| *key)
    }

    /// Returns `true` if the given string has been interned
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// // ThreadedRodeo is interchangeable for Rodeo here
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    ///
    /// let rodeo = rodeo.into_reader();
    /// assert!(rodeo.contains("Strings of things with wings and dings"));
    ///
    /// assert!(!rodeo.contains("This string isn't interned"));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn contains<T>(&self, val: T) -> bool
    where
        T: AsRef<str>,
        S: BuildHasher,
        K: Key,
    {
        self.get(val).is_some()
    }

    /// Returns `true` if the given key exists in the current interner
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    /// # use lasso::{Key, Spur};
    ///
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    /// # let key_that_doesnt_exist = Spur::try_from_usize(1000).unwrap();
    ///
    /// let rodeo = rodeo.into_reader();
    /// assert!(rodeo.contains_key(&key));
    /// assert!(!rodeo.contains_key(&key_that_doesnt_exist));
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn contains_key(&self, key: &K) -> bool
    where
        K: Key,
    {
        key.into_usize() < self.strings.len()
    }

    /// Resolves a string by its key. Only keys made by the current Resolver or the creator
    /// of the current Resolver may be used
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
    /// // ThreadedRodeo is interchangeable for Rodeo here
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    ///
    /// let rodeo = rodeo.into_resolver();
    /// assert_eq!("Strings of things with wings and dings", rodeo.resolve(&key));
    /// ```
    ///
    /// [`Key`]: crate::Key
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn resolve<'a>(&'a self, key: &K) -> &'a str
    where
        K: Key,
    {
        // Safety: The call to get_unchecked's safety relies on the Key::into_usize impl
        // being symmetric and the caller having not fabricated a key. If the impl is sound
        // and symmetric, then it will succeed, as the usize used to create it is a valid
        // index into self.strings
        unsafe {
            assert!(key.into_usize() < self.strings.len());
            self.strings.get_unchecked(key.into_usize())
        }
    }

    /// Resolves a string by its key, returning `None` if the key is out of bounds. Only keys
    /// made by the current Resolver or the creator of the current Resolver may be used
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// // ThreadedRodeo is interchangeable for Rodeo here
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    ///
    /// let rodeo = rodeo.into_resolver();
    /// assert_eq!(Some("Strings of things with wings and dings"), rodeo.try_resolve(&key));
    /// ```
    ///
    /// [`Key`]: crate::Key
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn try_resolve<'a>(&'a self, key: &K) -> Option<&'a str>
    where
        K: Key,
    {
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

    /// Resolves a string by its key without bounds checks
    ///
    /// # Safety
    ///
    /// The key must be valid for the current Reader
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// // ThreadedRodeo is interchangeable for Rodeo here
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Strings of things with wings and dings");
    ///
    /// let rodeo = rodeo.into_resolver();
    /// unsafe {
    ///     assert_eq!("Strings of things with wings and dings", rodeo.resolve_unchecked(&key));
    /// }
    /// ```
    ///
    /// [`Key`]: crate::Key
    #[cfg_attr(feature = "inline-more", inline)]
    pub unsafe fn resolve_unchecked<'a>(&'a self, key: &K) -> &'a str
    where
        K: Key,
    {
        unsafe { self.strings.get_unchecked(key.into_usize()) }
    }

    /// Gets the number of interned strings
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// // ThreadedRodeo is interchangeable for Rodeo here
    /// let mut rodeo = Rodeo::default();
    /// rodeo.get_or_intern("Documentation often has little hidden bits in it");
    ///
    /// let rodeo = rodeo.into_reader();
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
    /// // ThreadedRodeo is interchangeable for Rodeo here
    /// let rodeo = Rodeo::default();
    ///
    /// let rodeo = rodeo.into_reader();
    /// assert!(rodeo.is_empty());
    /// ```
    ///
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns an iterator over the interned strings and their key values
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn iter(&self) -> Iter<'_, K> {
        Iter::from_reader(self)
    }

    /// Returns an iterator over the interned strings
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn strings(&self) -> Strings<'_, K> {
        Strings::from_reader(self)
    }

    /// Consumes the current rodeo and makes it into a [`RodeoResolver`], allowing
    /// contention-free access from multiple threads with the lowest possible memory consumption
    ///
    /// # Example
    ///
    /// ```rust
    /// use lasso::Rodeo;
    ///
    /// // ThreadedRodeo is interchangeable for Rodeo here
    /// let mut rodeo = Rodeo::default();
    /// let key = rodeo.get_or_intern("Appear weak when you are strong, and strong when you are weak.");
    /// let reader_rodeo = rodeo.into_reader();
    ///
    /// let resolver_rodeo = reader_rodeo.into_resolver();
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
        let RodeoReader {
            strings, __arena, ..
        } = self;

        // Safety: The current reader no longer contains references to the strings
        // in the vec given to RodeoResolver
        unsafe { RodeoResolver::new(strings, __arena) }
    }
}

unsafe impl<K: Sync, S: Sync> Sync for RodeoReader<K, S> {}
unsafe impl<K: Send, S: Send> Send for RodeoReader<K, S> {}

impl<'a, K: Key, S> IntoIterator for &'a RodeoReader<K, S> {
    type Item = (K, &'a str);
    type IntoIter = Iter<'a, K>;

    #[cfg_attr(feature = "inline-more", inline)]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<K, S> Index<K> for RodeoReader<K, S>
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

impl<K, S> Eq for RodeoReader<K, S> {}

impl<K, S> PartialEq<Self> for RodeoReader<K, S> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &Self) -> bool {
        self.strings == other.strings
    }
}

impl<K, S> PartialEq<RodeoResolver<K>> for RodeoReader<K, S> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &RodeoResolver<K>) -> bool {
        self.strings == other.strings
    }
}

impl<K, S> PartialEq<Rodeo<K, S>> for RodeoReader<K, S> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &Rodeo<K, S>) -> bool {
        self.strings == other.strings
    }
}

compile! {
    if #[feature = "serialize"] {
        use crate::{Capacity, arenas::Arena};
        use alloc::string::String;
        use core::num::NonZeroUsize;
        use hashbrown::hash_map::RawEntryMut;
        use serde::{
            de::{Deserialize, Deserializer},
            ser::{Serialize, Serializer},
        };
    }
}

#[cfg(feature = "serialize")]
impl<K, H> Serialize for RodeoReader<K, H> {
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
impl<'de, K: Key, S: BuildHasher + Default> Deserialize<'de> for RodeoReader<K, S> {
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
            __arena: AnyArena::Arena(arena),
        })
    }
}

#[cfg(test)]
mod tests {
    mod single_threaded {
        #[cfg(feature = "serialize")]
        use crate::RodeoReader;
        use crate::{Key, Rodeo, Spur};

        #[test]
        fn get() {
            let mut rodeo = Rodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = rodeo.into_reader();
            assert_eq!(Some(key), reader.get("A"));

            assert!(reader.get("F").is_none());
        }

        #[test]
        fn resolve() {
            let mut rodeo = Rodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = rodeo.into_reader();
            assert_eq!("A", reader.resolve(&key));
        }

        #[test]
        #[should_panic]
        fn resolve_panics() {
            let reader = Rodeo::default().into_reader();
            reader.resolve(&Spur::try_from_usize(100).unwrap());
        }

        #[test]
        fn try_resolve() {
            let mut rodeo = Rodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = rodeo.into_reader();
            assert_eq!(Some("A"), reader.try_resolve(&key));
            assert_eq!(
                None,
                reader.try_resolve(&Spur::try_from_usize(100).unwrap())
            );
        }

        #[test]
        fn resolve_unchecked() {
            let mut rodeo = Rodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = rodeo.into_reader();
            unsafe {
                assert_eq!("A", reader.resolve_unchecked(&key));
            }
        }

        #[test]
        fn len() {
            let mut rodeo = Rodeo::default();
            rodeo.get_or_intern("A");
            rodeo.get_or_intern("B");
            rodeo.get_or_intern("C");

            let reader = rodeo.into_reader();
            assert_eq!(reader.len(), 3);
        }

        #[test]
        fn empty() {
            let rodeo = Rodeo::default();
            let reader = rodeo.into_reader();

            assert!(reader.is_empty());
        }

        #[test]
        fn iter() {
            let mut rodeo = Rodeo::default();
            let a = rodeo.get_or_intern("a");
            let b = rodeo.get_or_intern("b");
            let c = rodeo.get_or_intern("c");

            let resolver = rodeo.into_reader();
            let mut iter = resolver.iter();

            assert_eq!(Some((a, "a")), iter.next());
            assert_eq!(Some((b, "b")), iter.next());
            assert_eq!(Some((c, "c")), iter.next());
            assert_eq!(None, iter.next());
        }

        #[test]
        fn strings() {
            let mut rodeo = Rodeo::default();
            rodeo.get_or_intern("a");
            rodeo.get_or_intern("b");
            rodeo.get_or_intern("c");

            let resolver = rodeo.into_reader();
            let mut iter = resolver.strings();

            assert_eq!(Some("a"), iter.next());
            assert_eq!(Some("b"), iter.next());
            assert_eq!(Some("c"), iter.next());
            assert_eq!(None, iter.next());
        }

        #[test]
        fn drops() {
            let rodeo = Rodeo::default();
            let _ = rodeo.into_reader();
        }

        #[test]
        fn into_resolver() {
            let mut rodeo = Rodeo::default();
            let key = rodeo.get_or_intern("A");

            let resolver = rodeo.into_reader().into_resolver();
            assert_eq!("A", resolver.resolve(&key));
        }

        #[test]
        #[cfg(not(any(feature = "no-std", feature = "ahasher")))]
        fn debug() {
            let reader = Rodeo::default().into_reader();
            println!("{:?}", reader);
        }

        #[test]
        fn contains() {
            let mut rodeo = Rodeo::default();
            rodeo.get_or_intern("");
            let resolver = rodeo.into_reader();

            assert!(resolver.contains(""));
            assert!(resolver.contains(""));
        }

        #[test]
        fn contains_key() {
            let mut rodeo = Rodeo::default();
            let key = rodeo.get_or_intern("");
            let resolver = rodeo.into_reader();

            assert!(resolver.contains(""));
            assert!(resolver.contains_key(&key));
            assert!(!resolver.contains_key(&Spur::try_from_usize(10000).unwrap()));
        }

        #[test]
        fn into_iterator() {
            let rodeo = ["a", "b", "c", "d", "e"]
                .iter()
                .collect::<Rodeo>()
                .into_reader();

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
        fn index() {
            let mut rodeo = Rodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = rodeo.into_reader();
            assert_eq!("A", &reader[key]);
        }

        #[test]
        #[cfg(feature = "serialize")]
        fn empty_serialize() {
            let rodeo = Rodeo::default().into_reader();

            let ser = serde_json::to_string(&rodeo).unwrap();
            let ser2 = serde_json::to_string(&rodeo).unwrap();
            assert_eq!(ser, ser2);

            let deser: RodeoReader = serde_json::from_str(&ser).unwrap();
            assert!(deser.is_empty());
            let deser2: RodeoReader = serde_json::from_str(&ser2).unwrap();
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
            let rodeo = rodeo.into_reader();

            let ser = serde_json::to_string(&rodeo).unwrap();
            let ser2 = serde_json::to_string(&rodeo).unwrap();
            assert_eq!(ser, ser2);

            let deser: RodeoReader = serde_json::from_str(&ser).unwrap();
            let deser2: RodeoReader = serde_json::from_str(&ser2).unwrap();

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
        fn reader_eq() {
            let a = Rodeo::default();
            let b = Rodeo::default();
            assert_eq!(a.into_reader(), b.into_reader());

            let mut a = Rodeo::default();
            a.get_or_intern("a");
            a.get_or_intern("b");
            a.get_or_intern("c");
            let mut b = Rodeo::default();
            b.get_or_intern("a");
            b.get_or_intern("b");
            b.get_or_intern("c");
            assert_eq!(a.into_reader(), b.into_reader());
        }

        #[test]
        fn resolver_eq() {
            let a = Rodeo::default();
            let b = Rodeo::default();
            assert_eq!(a.into_reader(), b.into_resolver());

            let mut a = Rodeo::default();
            a.get_or_intern("a");
            a.get_or_intern("b");
            a.get_or_intern("c");
            let mut b = Rodeo::default();
            b.get_or_intern("a");
            b.get_or_intern("b");
            b.get_or_intern("c");
            assert_eq!(a.into_reader(), b.into_resolver());
        }
    }

    #[cfg(all(not(any(miri, feature = "no-std")), feature = "multi-threaded"))]
    mod multi_threaded {
        use crate::{Key, RodeoReader, Spur, ThreadedRodeo};
        use std::thread;
        use std::sync::Arc;

        #[test]
        fn get() {
            let rodeo = ThreadedRodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = rodeo.into_reader();
            assert_eq!(Some(key), reader.get("A"));

            assert!(reader.get("F").is_none());
        }

        #[test]
        fn get_threaded() {
            let rodeo = ThreadedRodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = Arc::new(rodeo.into_reader());

            let moved = Arc::clone(&reader);
            thread::spawn(move || {
                assert_eq!(Some(key), moved.get("A"));
                assert!(moved.get("F").is_none());
            });

            assert_eq!(Some(key), reader.get("A"));
            assert!(reader.get("F").is_none());
        }

        #[test]
        fn resolve() {
            let rodeo = ThreadedRodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = rodeo.into_reader();
            assert_eq!("A", reader.resolve(&key));
        }

        #[test]
        fn resolve_threaded() {
            let rodeo = ThreadedRodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = Arc::new(rodeo.into_reader());

            let moved = Arc::clone(&reader);
            thread::spawn(move || {
                assert_eq!("A", moved.resolve(&key));
            });

            assert_eq!("A", reader.resolve(&key));
        }

        #[test]
        fn len() {
            let rodeo = ThreadedRodeo::default();
            rodeo.get_or_intern("A");
            rodeo.get_or_intern("B");
            rodeo.get_or_intern("C");

            let reader = rodeo.into_reader();
            assert_eq!(reader.len(), 3);
        }

        #[test]
        fn empty() {
            let rodeo = ThreadedRodeo::default();
            let reader = rodeo.into_reader();

            assert!(reader.is_empty());
        }

        #[test]
        fn iter() {
            let rodeo = ThreadedRodeo::default();
            let a = rodeo.get_or_intern("a");
            let b = rodeo.get_or_intern("b");
            let c = rodeo.get_or_intern("c");

            let resolver = rodeo.into_resolver();
            let mut iter = resolver.iter();

            assert_eq!(Some((a, "a")), iter.next());
            assert_eq!(Some((b, "b")), iter.next());
            assert_eq!(Some((c, "c")), iter.next());
            assert_eq!(None, iter.next());
        }

        #[test]
        fn strings() {
            let rodeo = ThreadedRodeo::default();
            rodeo.get_or_intern("a");
            rodeo.get_or_intern("b");
            rodeo.get_or_intern("c");

            let resolver = rodeo.into_resolver();
            let mut iter = resolver.strings();

            assert_eq!(Some("a"), iter.next());
            assert_eq!(Some("b"), iter.next());
            assert_eq!(Some("c"), iter.next());
            assert_eq!(None, iter.next());
        }

        #[test]
        fn drops() {
            let rodeo = ThreadedRodeo::default();
            let _ = rodeo.into_reader();
        }

        #[test]
        fn drop_threaded() {
            let rodeo = ThreadedRodeo::default();
            let reader = Arc::new(rodeo.into_reader());

            let moved = Arc::clone(&reader);
            thread::spawn(move || {
                let _ = moved;
            });
        }

        #[test]
        fn into_resolver() {
            let rodeo = ThreadedRodeo::default();
            let key = rodeo.get_or_intern("A");

            let resolver = rodeo.into_reader().into_resolver();
            assert_eq!("A", resolver.resolve(&key));
        }

        #[test]
        #[cfg(not(feature = "no-std"))]
        fn debug() {
            let reader = ThreadedRodeo::default().into_reader();
            println!("{:?}", reader);
        }

        #[test]
        fn contains() {
            let rodeo = ThreadedRodeo::default();
            rodeo.get_or_intern("");
            let resolver = rodeo.into_reader();

            assert!(resolver.contains(""));
            assert!(resolver.contains(""));
        }

        #[test]
        fn contains_key() {
            let rodeo = ThreadedRodeo::default();
            let key = rodeo.get_or_intern("");
            let resolver = rodeo.into_reader();

            assert!(resolver.contains(""));
            assert!(resolver.contains_key(&key));
            assert!(!resolver.contains_key(&Spur::try_from_usize(10000).unwrap()));
        }

        #[test]
        fn into_iterator() {
            let rodeo = ["a", "b", "c", "d", "e"]
                .iter()
                .collect::<ThreadedRodeo>()
                .into_reader();

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
        fn index() {
            let rodeo = ThreadedRodeo::default();
            let key = rodeo.get_or_intern("A");

            let reader = rodeo.into_reader();
            assert_eq!("A", &reader[key]);
        }

        #[test]
        #[cfg(feature = "serialize")]
        fn empty_serialize() {
            let rodeo = ThreadedRodeo::default().into_reader();

            let ser = serde_json::to_string(&rodeo).unwrap();
            let ser2 = serde_json::to_string(&rodeo).unwrap();
            assert_eq!(ser, ser2);

            let deser: RodeoReader = serde_json::from_str(&ser).unwrap();
            assert!(deser.is_empty());
            let deser2: RodeoReader = serde_json::from_str(&ser2).unwrap();
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
            let rodeo = rodeo.into_reader();

            let ser = serde_json::to_string(&rodeo).unwrap();
            let ser2 = serde_json::to_string(&rodeo).unwrap();
            assert_eq!(ser, ser2);

            let deser: RodeoReader = serde_json::from_str(&ser).unwrap();
            let deser2: RodeoReader = serde_json::from_str(&ser2).unwrap();

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
    }
}
