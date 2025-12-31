//! Implementations of [`Interner`], [`Reader`] and [`Resolver`] for [`ThreadedRodeo`]
#![cfg(feature = "multi-threaded")]

use crate::*;
#[cfg(feature = "no-std")]
use alloc::boxed::Box;
use core::hash::{BuildHasher, Hash};

impl<K, S> Interner<K> for ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn get_or_intern(&mut self, val: &str) -> K {
        (*self).get_or_intern(val)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn try_get_or_intern(&mut self, val: &str) -> LassoResult<K> {
        (*self).try_get_or_intern(val)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn get_or_intern_static(&mut self, val: &'static str) -> K {
        (*self).get_or_intern_static(val)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn try_get_or_intern_static(&mut self, val: &'static str) -> LassoResult<K> {
        (*self).try_get_or_intern_static(val)
    }
}

impl<K, S> IntoReaderAndResolver<K> for ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
}

impl<K, S> IntoReader<K> for ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    type Reader = RodeoReader<K, S>;

    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    fn into_reader(self) -> Self::Reader
    where
        Self: 'static,
    {
        self.into_reader()
    }

    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    fn into_reader_boxed(self: Box<Self>) -> Self::Reader
    where
        Self: 'static,
    {
        ThreadedRodeo::into_reader(*self)
    }
}

impl<K, S> Reader<K> for ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn get(&self, val: &str) -> Option<K> {
        self.get(val)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn contains(&self, val: &str) -> bool {
        self.contains(val)
    }
}

impl<K, S> IntoResolver<K> for ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    type Resolver = RodeoResolver<K>;

    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    fn into_resolver(self) -> Self::Resolver
    where
        Self: 'static,
    {
        self.into_resolver()
    }

    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    fn into_resolver_boxed(self: Box<Self>) -> Self::Resolver
    where
        Self: 'static,
    {
        ThreadedRodeo::into_resolver(*self)
    }
}

impl<K, S> Resolver<K> for ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn resolve<'a>(&'a self, key: &K) -> &'a str {
        self.resolve(key)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn try_resolve<'a>(&'a self, key: &K) -> Option<&'a str> {
        self.try_resolve(key)
    }

    /// [`ThreadedRodeo`] does not actually have a `resolve_unchecked()` method,
    /// so this just forwards to the normal [`ThreadedRodeo::resolve()`] method
    #[cfg_attr(feature = "inline-more", inline)]
    unsafe fn resolve_unchecked<'a>(&'a self, key: &K) -> &'a str {
        self.resolve(key)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn contains_key(&self, key: &K) -> bool {
        self.contains_key(key)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn len(&self) -> usize {
        self.len()
    }
}
