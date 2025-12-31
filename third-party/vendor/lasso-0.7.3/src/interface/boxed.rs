use super::{Interner, IntoReader, IntoResolver, Reader, Resolver};
use crate::{Key, LassoResult};
#[cfg(feature = "no-std")]
use alloc::boxed::Box;

impl<K, I> Interner<K> for Box<I>
where
    K: Key,
    I: Interner<K> + ?Sized + 'static,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn get_or_intern(&mut self, val: &str) -> K {
        (**self).get_or_intern(val)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn try_get_or_intern(&mut self, val: &str) -> LassoResult<K> {
        (**self).try_get_or_intern(val)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn get_or_intern_static(&mut self, val: &'static str) -> K {
        (**self).get_or_intern_static(val)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn try_get_or_intern_static(&mut self, val: &'static str) -> LassoResult<K> {
        self.try_get_or_intern(val)
    }
}

impl<K, I> IntoReader<K> for Box<I>
where
    K: Key,
    I: IntoReader<K> + ?Sized + 'static,
{
    type Reader = <I as IntoReader<K>>::Reader;

    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    fn into_reader(self) -> Self::Reader
    where
        Self: 'static,
    {
        I::into_reader_boxed(self)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    fn into_reader_boxed(self: Box<Self>) -> Self::Reader
    where
        Self: 'static,
    {
        (*self).into_reader()
    }
}

impl<K, I> Reader<K> for Box<I>
where
    K: Key,
    I: Reader<K> + ?Sized + 'static,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn get(&self, val: &str) -> Option<K> {
        (**self).get(val)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn contains(&self, val: &str) -> bool {
        (**self).contains(val)
    }
}

impl<K, I> IntoResolver<K> for Box<I>
where
    K: Key,
    I: IntoResolver<K> + ?Sized + 'static,
{
    type Resolver = <I as IntoResolver<K>>::Resolver;

    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    fn into_resolver(self) -> Self::Resolver
    where
        Self: 'static,
    {
        I::into_resolver_boxed(self)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    #[must_use]
    fn into_resolver_boxed(self: Box<Self>) -> Self::Resolver
    where
        Self: 'static,
    {
        (*self).into_resolver()
    }
}

impl<K, I> Resolver<K> for Box<I>
where
    K: Key,
    I: Resolver<K> + ?Sized + 'static,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn resolve<'a>(&'a self, key: &K) -> &'a str {
        (**self).resolve(key)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn try_resolve<'a>(&'a self, key: &K) -> Option<&'a str> {
        (**self).try_resolve(key)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    unsafe fn resolve_unchecked<'a>(&'a self, key: &K) -> &'a str {
        unsafe { (**self).resolve_unchecked(key) }
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn contains_key(&self, key: &K) -> bool {
        (**self).contains_key(key)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn len(&self) -> usize {
        (**self).len()
    }
}
