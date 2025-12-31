mod boxed;
mod rodeo;
mod rodeo_reader;
mod rodeo_resolver;
mod tests;
mod threaded_ref;
mod threaded_rodeo;

use crate::{Key, LassoResult, Spur};
#[cfg(feature = "no-std")]
use alloc::boxed::Box;

/// A generic interface over any underlying interner, allowing storing and accessing
/// interned strings
///
/// Note that because single-threaded [`Rodeo`](crate::Rodeo)s require mutable access to use, this
/// trait does so as well. For use with [`ThreadedRodeo`](crate::ThreadedRodeo), the trait is
/// implemented for `&ThreadedRodeo` as well to allow access through shared references.
pub trait Interner<K = Spur>: Reader<K> + Resolver<K> {
    /// Get the key for a string, interning it if it does not yet exist
    ///
    /// # Panics
    ///
    /// Panics if the key's [`try_from_usize`](Key::try_from_usize) function fails. With the default
    /// keys, this means that you've interned more strings than it can handle. (For [`Spur`] this
    /// means that `u32::MAX - 1` unique strings were interned)
    ///
    fn get_or_intern(&mut self, val: &str) -> K;

    /// Get the key for a string, interning it if it does not yet exist
    fn try_get_or_intern(&mut self, val: &str) -> LassoResult<K>;

    /// Get the key for a static string, interning it if it does not yet exist
    ///
    /// This will not reallocate or copy the given string
    ///
    /// # Panics
    ///
    /// Panics if the key's [`try_from_usize`](Key::try_from_usize) function fails. With the default
    /// keys, this means that you've interned more strings than it can handle. (For [`Spur`] this
    /// means that `u32::MAX - 1` unique strings were interned)
    ///
    fn get_or_intern_static(&mut self, val: &'static str) -> K;

    /// Get the key for a static string, interning it if it does not yet exist
    ///
    /// This will not reallocate or copy the given string
    fn try_get_or_intern_static(&mut self, val: &'static str) -> LassoResult<K>;
}

impl<T, K> Interner<K> for &mut T
where
    T: Interner<K>,
{
    #[inline]
    fn get_or_intern(&mut self, val: &str) -> K {
        <T as Interner<K>>::get_or_intern(self, val)
    }

    #[inline]
    fn try_get_or_intern(&mut self, val: &str) -> LassoResult<K> {
        <T as Interner<K>>::try_get_or_intern(self, val)
    }

    #[inline]
    fn get_or_intern_static(&mut self, val: &'static str) -> K {
        <T as Interner<K>>::get_or_intern_static(self, val)
    }

    #[inline]
    fn try_get_or_intern_static(&mut self, val: &'static str) -> LassoResult<K> {
        <T as Interner<K>>::try_get_or_intern_static(self, val)
    }
}

/// A generic interface over interners that can be turned into both a [`Reader`] and a [`Resolver`]
/// directly.
pub trait IntoReaderAndResolver<K = Spur>: IntoReader<K> + IntoResolver<K>
where
    K: Key,
{
}

/// A generic interface over interners that can be turned into a [`Reader`].
pub trait IntoReader<K = Spur>: Interner<K>
where
    K: Key,
{
    /// The type of [`Reader`] the interner will be converted into
    type Reader: Reader<K>;

    /// Consumes the current [`Interner`] and converts it into a [`Reader`] to allow
    /// contention-free access of the interner from multiple threads
    #[must_use]
    fn into_reader(self) -> Self::Reader
    where
        Self: 'static;

    /// An implementation detail to allow calling [`Interner::into_reader()`] on dynamically
    /// dispatched interners
    #[doc(hidden)]
    #[must_use]
    fn into_reader_boxed(self: Box<Self>) -> Self::Reader
    where
        Self: 'static;
}

/// A generic interface that allows using any underlying interner for
/// both its reading and resolution capabilities, allowing both
/// `str -> key` and `key -> str` lookups
pub trait Reader<K = Spur>: Resolver<K> {
    /// Get a key for the given string value if it exists
    fn get(&self, val: &str) -> Option<K>;

    /// Returns `true` if the current interner contains the given string
    fn contains(&self, val: &str) -> bool;
}

impl<T, K> Reader<K> for &T
where
    T: Reader<K>,
{
    #[inline]
    fn get(&self, val: &str) -> Option<K> {
        <T as Reader<K>>::get(self, val)
    }

    #[inline]
    fn contains(&self, val: &str) -> bool {
        <T as Reader<K>>::contains(self, val)
    }
}

impl<T, K> Reader<K> for &mut T
where
    T: Reader<K>,
{
    #[inline]
    fn get(&self, val: &str) -> Option<K> {
        <T as Reader<K>>::get(self, val)
    }

    #[inline]
    fn contains(&self, val: &str) -> bool {
        <T as Reader<K>>::contains(self, val)
    }
}

/// A generic interface over [`Reader`]s that can be turned into a [`Resolver`].
pub trait IntoResolver<K = Spur>: Reader<K>
where
    K: Key,
{
    /// The type of [`Resolver`] the reader will be converted into
    type Resolver: Resolver<K>;

    /// Consumes the current [`Reader`] and makes it into a [`Resolver`], allowing
    /// contention-free access from multiple threads with the lowest possible memory consumption
    #[must_use]
    fn into_resolver(self) -> Self::Resolver
    where
        Self: 'static;

    /// An implementation detail to allow calling [`Reader::into_resolver()`] on dynamically
    /// dispatched readers
    #[doc(hidden)]
    #[must_use]
    fn into_resolver_boxed(self: Box<Self>) -> Self::Resolver
    where
        Self: 'static;
}

/// A generic interface that allows using any underlying interner only
/// for its resolution capabilities, allowing only `key -> str` lookups
pub trait Resolver<K = Spur> {
    /// Resolves the given key into a string
    ///
    /// # Panics
    ///
    /// Panics if the key is not contained in the current [`Resolver`]
    ///
    fn resolve<'a>(&'a self, key: &K) -> &'a str;

    /// Attempts to resolve the given key into a string, returning `None`
    /// if it cannot be found
    fn try_resolve<'a>(&'a self, key: &K) -> Option<&'a str>;

    /// Resolves a string by its key without preforming bounds checks
    ///
    /// # Safety
    ///
    /// The key must be valid for the current [`Resolver`]
    ///
    unsafe fn resolve_unchecked<'a>(&'a self, key: &K) -> &'a str;

    /// Returns `true` if the current interner contains the given key
    fn contains_key(&self, key: &K) -> bool;

    /// Gets the number of currently interned strings
    fn len(&self) -> usize;

    /// Returns `true` if there are no currently interned strings
    #[cfg_attr(feature = "inline-more", inline)]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T, K> Resolver<K> for &T
where
    T: Resolver<K>,
{
    #[inline]
    fn resolve<'a>(&'a self, key: &K) -> &'a str {
        <T as Resolver<K>>::resolve(self, key)
    }

    #[inline]
    fn try_resolve<'a>(&'a self, key: &K) -> Option<&'a str> {
        <T as Resolver<K>>::try_resolve(self, key)
    }

    #[inline]
    unsafe fn resolve_unchecked<'a>(&'a self, key: &K) -> &'a str {
        unsafe { <T as Resolver<K>>::resolve_unchecked(self, key) }
    }

    #[inline]
    fn contains_key(&self, key: &K) -> bool {
        <T as Resolver<K>>::contains_key(self, key)
    }

    #[inline]
    fn len(&self) -> usize {
        <T as Resolver<K>>::len(self)
    }
}

impl<T, K> Resolver<K> for &mut T
where
    T: Resolver<K>,
{
    #[inline]
    fn resolve<'a>(&'a self, key: &K) -> &'a str {
        <T as Resolver<K>>::resolve(self, key)
    }

    #[inline]
    fn try_resolve<'a>(&'a self, key: &K) -> Option<&'a str> {
        <T as Resolver<K>>::try_resolve(self, key)
    }

    #[inline]
    unsafe fn resolve_unchecked<'a>(&'a self, key: &K) -> &'a str {
        unsafe { <T as Resolver<K>>::resolve_unchecked(self, key) }
    }

    #[inline]
    fn contains_key(&self, key: &K) -> bool {
        <T as Resolver<K>>::contains_key(self, key)
    }

    #[inline]
    fn len(&self) -> usize {
        <T as Resolver<K>>::len(self)
    }
}
