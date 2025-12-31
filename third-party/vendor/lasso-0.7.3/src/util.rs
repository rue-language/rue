use crate::{keys::Key, reader::RodeoReader, resolver::RodeoResolver, rodeo::Rodeo};
use core::{fmt, iter, marker::PhantomData, num::NonZeroUsize, slice};

/// A continence type for an error from an interner
pub type LassoResult<T> = core::result::Result<T, LassoError>;

/// An error encountered while using an interner
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LassoError {
    kind: LassoErrorKind,
}

impl LassoError {
    /// Gets the kind of error that occurred
    #[cfg_attr(feature = "inline-more", inline)]
    pub const fn kind(&self) -> LassoErrorKind {
        self.kind
    }
}

impl LassoError {
    pub(crate) const fn new(kind: LassoErrorKind) -> Self {
        Self { kind }
    }
}

impl fmt::Display for LassoError {
    #[cfg_attr(feature = "inline-more", inline)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lasso encountered an error: {}", self.kind)
    }
}

#[cfg(not(feature = "no-std"))]
impl std::error::Error for LassoError {}

/// The kind of error that occurred
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum LassoErrorKind {
    /// A memory limit set using [`MemoryLimits`] was reached, and no more memory could be allocated
    ///
    /// [`MemoryLimits`]: crate::MemoryLimits
    MemoryLimitReached,
    /// A [`Key`] implementation returned `None`, meaning it could not produce any more keys
    ///
    /// [`Key`]: crate::Key
    KeySpaceExhaustion,
    /// A memory allocation failed
    FailedAllocation,
}

impl LassoErrorKind {
    /// A memory limit set using [`MemoryLimits`] was reached, and no more memory could be allocated
    ///
    /// [`MemoryLimits`]: crate::MemoryLimits
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn is_memory_limit(self) -> bool {
        self == Self::MemoryLimitReached
    }

    /// A [`Key`] implementation returned `None`, meaning it could not produce any more keys
    ///
    /// [`Key`]: crate::Key
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn is_keyspace_exhaustion(self) -> bool {
        self == Self::KeySpaceExhaustion
    }

    /// A memory allocation failed
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn is_failed_alloc(self) -> bool {
        self == Self::FailedAllocation
    }
}

impl fmt::Display for LassoErrorKind {
    #[cfg_attr(feature = "inline-more", inline)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryLimitReached => f.write_str("The configured memory limit was reached"),
            Self::KeySpaceExhaustion => f.write_str("The key space was exhausted"),
            Self::FailedAllocation => f.write_str("Failed to allocate memory"),
        }
    }
}

/// The amount of strings and bytes that an interner can hold before reallocating
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Capacity {
    /// The number of strings that will be allocated
    pub(crate) strings: usize,
    /// The number of bytes that will be allocated
    pub(crate) bytes: NonZeroUsize,
}

impl Capacity {
    /// Create a new `Capacity` with the number of strings that the interner will hold
    /// and the number of bytes that the interner will hold
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn new(strings: usize, bytes: NonZeroUsize) -> Self {
        Self { strings, bytes }
    }

    /// Create a new `Capacity` with the number of strings that the interner will hold
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn for_strings(strings: usize) -> Self {
        Self {
            strings,
            ..Self::default()
        }
    }

    /// Create a new `Capacity` with the number of bytes that the interner will hold
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn for_bytes(bytes: NonZeroUsize) -> Self {
        Self {
            bytes,
            ..Self::default()
        }
    }

    /// Produces the smallest `Capacity` with enough room for zero strings and a single byte
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn minimal() -> Self {
        Self {
            strings: 0,
            // Safety: 1 is not 0
            bytes: unsafe { NonZeroUsize::new_unchecked(1) },
        }
    }

    /// Returns the number of strings this capacity will allocate
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn strings(&self) -> usize {
        self.strings
    }

    /// Returns the number of bytes this capacity will allocate
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn bytes(&self) -> NonZeroUsize {
        self.bytes
    }
}

/// Creates a `Capacity` that will hold 50 strings and 4096 bytes
impl Default for Capacity {
    #[cfg_attr(feature = "inline-more", inline)]
    fn default() -> Self {
        Self {
            strings: 50,
            // Safety: 4096 is not 0
            bytes: unsafe { NonZeroUsize::new_unchecked(4096) },
        }
    }
}

/// Settings for the memory consumption of an interner
///
/// By default `max_memory_usage` is set to `usize::MAX`
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryLimits {
    /// The maximum memory an interner will allocate
    pub(crate) max_memory_usage: usize,
}

impl MemoryLimits {
    /// Create a new `MemoryLimits` with the number of bytes that the interner can allocate
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn new(max_memory_usage: usize) -> Self {
        Self { max_memory_usage }
    }

    /// Create a new `MemoryLimits` with the number of bytes that the interner can allocate
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn for_memory_usage(max_memory_usage: usize) -> Self {
        Self { max_memory_usage }
    }

    /// Returns the maximum memory usage this `MemoryLimits` can allocate
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn max_memory_usage(&self) -> usize {
        self.max_memory_usage
    }
}

/// Creates a `MemoryLimits` with `max_memory_usage` set to `usize::max_value()`
impl Default for MemoryLimits {
    #[cfg_attr(feature = "inline-more", inline)]
    fn default() -> Self {
        Self {
            max_memory_usage: usize::MAX,
        }
    }
}

/// An iterator over an interner's strings and keys
#[derive(Clone, Debug)]
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Iter<'a, K> {
    iter: iter::Enumerate<slice::Iter<'a, &'a str>>,
    __key: PhantomData<K>,
}

impl<'a, K> Iter<'a, K> {
    #[cfg_attr(feature = "inline-more", inline)]
    pub(crate) fn from_rodeo<S>(rodeo: &'a Rodeo<K, S>) -> Self {
        Self {
            iter: rodeo.strings.iter().enumerate(),
            __key: PhantomData,
        }
    }

    #[cfg_attr(feature = "inline-more", inline)]
    pub(crate) fn from_reader<S>(rodeo: &'a RodeoReader<K, S>) -> Self {
        Self {
            iter: rodeo.strings.iter().enumerate(),
            __key: PhantomData,
        }
    }

    #[cfg_attr(feature = "inline-more", inline)]
    pub(crate) fn from_resolver(rodeo: &'a RodeoResolver<K>) -> Self {
        Self {
            iter: rodeo.strings.iter().enumerate(),
            __key: PhantomData,
        }
    }
}

fn iter_element<'a, K>((key, string): (usize, &&'a str)) -> (K, &'a str)
where
    K: Key,
{
    (
        K::try_from_usize(key).unwrap_or_else(|| unreachable!()),
        *string,
    )
}

impl<'a, K> Iterator for Iter<'a, K>
where
    K: Key,
{
    type Item = (K, &'a str);

    #[cfg_attr(feature = "inline-more", inline)]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(iter_element)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<'a, K> DoubleEndedIterator for Iter<'a, K>
where
    K: Key,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn next_back(&mut self) -> Option<(K, &'a str)> {
        self.iter.next_back().map(iter_element)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn nth_back(&mut self, n: usize) -> Option<(K, &'a str)> {
        self.iter.nth_back(n).map(iter_element)
    }
}

// iter::Enumerate is exact-size if its underlying iterator is exact-size, which slice::Iter is.
impl<'a, K: Key> ExactSizeIterator for Iter<'a, K> {}

// iter::Enumerate is fused if its underlying iterator is fused, which slice::Iter is.
impl<'a, K: Key> iter::FusedIterator for Iter<'a, K> {}

// #[derive(Debug)]
// pub struct LockedIter<'a, K: Key> {
//     iter: iter::Enumerate<slice::Iter<'a, &'a str>>,
//     #[cfg(not(feature = "parking_locks"))]
//     __guard: std::sync::MutexGuard<'a, Vec<&'static str>>,
//     __key: PhantomData<K>,
// }
//
// impl<'a, K: Key> LockedIter<'a, K> {
//     #[cfg_attr(feature = "inline-more", inline)]
//     fn from_threaded<H: BuildHasher + Clone>(rodeo: &'a ThreadedRodeo<K, H>) -> Self {
//         let guard = rodeo.strings.lock().unwrap();
//
//         Self {
//             iter: guard.iter().enumerate(),
//             #[cfg(not(feature = "parking_locks"))]
//             __guard: guard,
//             __key: PhantomData,
//         }
//     }
// }

/// An iterator over an interner's strings
#[derive(Clone, Debug)]
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Strings<'a, K> {
    iter: slice::Iter<'a, &'a str>,
    __key: PhantomData<K>,
}

impl<'a, K> Strings<'a, K> {
    #[cfg_attr(feature = "inline-more", inline)]
    pub(crate) fn from_rodeo<H>(rodeo: &'a Rodeo<K, H>) -> Self {
        Self {
            iter: rodeo.strings.iter(),
            __key: PhantomData,
        }
    }

    #[cfg_attr(feature = "inline-more", inline)]
    pub(crate) fn from_reader<H>(rodeo: &'a RodeoReader<K, H>) -> Self {
        Self {
            iter: rodeo.strings.iter(),
            __key: PhantomData,
        }
    }

    #[cfg_attr(feature = "inline-more", inline)]
    pub(crate) fn from_resolver(rodeo: &'a RodeoResolver<K>) -> Self {
        Self {
            iter: rodeo.strings.iter(),
            __key: PhantomData,
        }
    }
}

impl<'a, K> Iterator for Strings<'a, K> {
    type Item = &'a str;

    #[cfg_attr(feature = "inline-more", inline)]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().copied()
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<'a, K> DoubleEndedIterator for Strings<'a, K>
where
    K: Key,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn next_back(&mut self) -> Option<&'a str> {
        self.iter.next_back().copied()
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn nth_back(&mut self, n: usize) -> Option<&'a str> {
        self.iter.nth_back(n).copied()
    }
}

// slice::Iter is exact-size.
impl<'a, K> ExactSizeIterator for Strings<'a, K> {}

// slice::Iter is fused.
impl<'a, K: Key> iter::FusedIterator for Strings<'a, K> {}

macro_rules! compile {
    ($(
        if #[$meta:meta] {
            $($item:item)*
        } $(else if #[$else_if_meta:meta] {
            $($else_if_item:item)*
        })* $(else {
            $($else_item:item)*
        })?
    )+) => {
        $(
            $(
                #[cfg($meta)]
                $item
            )*

            compile!{
                @inner
                ( $meta, )
                $(else if #[$else_if_meta] {
                    $( $else_if_item )*
                })* $(else {
                    $( $else_item )*
                })?
            }
        )+
    };

    (@recurse
        ($($prev_metas:tt)*)
        ($new_meta:meta)
        $($rem:tt)*
    ) => {
        compile!{
            @inner
            ($( $prev_metas )* $new_meta,)
            $( $rem )*
        }
    };

    (@inner
        $prev_metas:tt
        else if #[$meta:meta] {
            $($else_if_item:item)*
        }
        $($rem:tt)*

    ) => {
        $(
            #[cfg(all(not(any $prev_metas), $meta))]
            $else_if_item
        )*

        compile! {
            @recurse $prev_metas ($meta) $( $rem )*
        }
    };

    (@inner
        $prev_metas:tt
        else {
            $($else_item:item)*
        }
    )=>{
        $(
            #[cfg(not(any $prev_metas))]
            $else_item
        )*
    };

    (@inner ($($prev_metas:tt)*))=>{};
}

macro_rules! index_unchecked {
    ($slice:expr, $idx:expr) => {{
        let elem: &_ = if cfg!(debug_assertions) {
            $slice[$idx]
        } else {
            *$slice.get_unchecked($idx)
        };

        elem
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_capacity() {
        let capacity = Capacity::new(100, NonZeroUsize::new(100).unwrap());
        assert_eq!(100, capacity.strings());
        assert_eq!(100, capacity.bytes.get());

        let capacity = Capacity::default();
        assert_eq!(capacity.strings, capacity.strings());
        assert_eq!(capacity.bytes, capacity.bytes());

        let capacity = Capacity::for_strings(10);
        assert_eq!(capacity.strings(), 10);

        let capacity = Capacity::for_bytes(NonZeroUsize::new(10).unwrap());
        assert_eq!(capacity.bytes().get(), 10);

        let capacity = Capacity::minimal();
        assert_eq!(capacity.strings(), 0);
        assert_eq!(capacity.bytes().get(), 1);
    }

    #[test]
    fn iter_rodeo() {
        let mut rodeo = Rodeo::default();
        let a = rodeo.get_or_intern("A");
        let b = rodeo.get_or_intern("B");
        let c = rodeo.get_or_intern("C");
        let d = rodeo.get_or_intern("D");

        let mut iter = Iter::from_rodeo(&rodeo);

        assert_eq!((4, Some(4)), iter.size_hint());
        assert_eq!(Some((a, "A")), iter.next());
        assert_eq!(Some((b, "B")), iter.next());
        assert_eq!(Some((c, "C")), iter.next());
        assert_eq!(Some((d, "D")), iter.next());
        assert_eq!(None, iter.next());
        assert_eq!((0, Some(0)), iter.size_hint());
    }

    #[test]
    fn iter_reader() {
        let mut rodeo = Rodeo::default();
        let a = rodeo.get_or_intern("A");
        let b = rodeo.get_or_intern("B");
        let c = rodeo.get_or_intern("C");
        let d = rodeo.get_or_intern("D");

        let reader = rodeo.into_reader();
        let mut iter = Iter::from_reader(&reader);

        assert_eq!((4, Some(4)), iter.size_hint());
        assert_eq!(Some((a, "A")), iter.next());
        assert_eq!(Some((b, "B")), iter.next());
        assert_eq!(Some((c, "C")), iter.next());
        assert_eq!(Some((d, "D")), iter.next());
        assert_eq!(None, iter.next());
        assert_eq!((0, Some(0)), iter.size_hint());
    }

    #[test]
    fn iter_resolver() {
        let mut rodeo = Rodeo::default();
        let a = rodeo.get_or_intern("A");
        let b = rodeo.get_or_intern("B");
        let c = rodeo.get_or_intern("C");
        let d = rodeo.get_or_intern("D");

        let resolver = rodeo.into_resolver();
        let mut iter = Iter::from_resolver(&resolver);

        assert_eq!((4, Some(4)), iter.size_hint());
        assert_eq!(Some((a, "A")), iter.next());
        assert_eq!(Some((b, "B")), iter.next());
        assert_eq!(Some((c, "C")), iter.next());
        assert_eq!(Some((d, "D")), iter.next());
        assert_eq!(None, iter.next());
        assert_eq!((0, Some(0)), iter.size_hint());
    }

    #[test]
    fn strings_rodeo() {
        let mut rodeo = Rodeo::default();
        rodeo.get_or_intern("A");
        rodeo.get_or_intern("B");
        rodeo.get_or_intern("C");
        rodeo.get_or_intern("D");

        let mut iter = Strings::from_rodeo(&rodeo);

        assert_eq!((4, Some(4)), iter.size_hint());
        assert_eq!(Some("A"), iter.next());
        assert_eq!(Some("B"), iter.next());
        assert_eq!(Some("C"), iter.next());
        assert_eq!(Some("D"), iter.next());
        assert_eq!(None, iter.next());
        assert_eq!((0, Some(0)), iter.size_hint());
    }

    #[test]
    fn strings_reader() {
        let mut rodeo = Rodeo::default();
        rodeo.get_or_intern("A");
        rodeo.get_or_intern("B");
        rodeo.get_or_intern("C");
        rodeo.get_or_intern("D");

        let reader = rodeo.into_reader();
        let mut iter = Strings::from_reader(&reader);

        assert_eq!((4, Some(4)), iter.size_hint());
        assert_eq!(Some("A"), iter.next());
        assert_eq!(Some("B"), iter.next());
        assert_eq!(Some("C"), iter.next());
        assert_eq!(Some("D"), iter.next());
        assert_eq!(None, iter.next());
        assert_eq!((0, Some(0)), iter.size_hint());
    }

    #[test]
    fn strings_resolver() {
        let mut rodeo = Rodeo::default();
        rodeo.get_or_intern("A");
        rodeo.get_or_intern("B");
        rodeo.get_or_intern("C");
        rodeo.get_or_intern("D");

        let resolver = rodeo.into_resolver();
        let mut iter = Strings::from_resolver(&resolver);

        assert_eq!((4, Some(4)), iter.size_hint());
        assert_eq!(Some("A"), iter.next());
        assert_eq!(Some("B"), iter.next());
        assert_eq!(Some("C"), iter.next());
        assert_eq!(Some("D"), iter.next());
        assert_eq!(None, iter.next());
        assert_eq!((0, Some(0)), iter.size_hint());
    }
}
