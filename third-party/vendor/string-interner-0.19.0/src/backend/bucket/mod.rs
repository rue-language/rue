#![cfg(feature = "backends")]

mod fixed_str;
mod interned_str;

use self::{fixed_str::FixedString, interned_str::InternedStr};
use super::Backend;
use crate::{symbol::expect_valid_symbol, DefaultSymbol, Symbol};
use alloc::{string::String, vec::Vec};
use core::{iter::Enumerate, marker::PhantomData, slice};

/// An interner backend that reduces memory allocations by using string buckets.
///
/// # Note
///
/// Implementation inspired by matklad's blog post that can be found here:
/// <https://matklad.github.io/2020/03/22/fast-simple-rust-interner.html>
///
/// # Usage Hint
///
/// Use when deallocations or copy overhead is costly or when
/// interning of static strings is especially common.
///
/// # Usage
///
/// - **Fill:** Efficiency of filling an empty string interner.
/// - **Resolve:** Efficiency of interned string look-up given a symbol.
/// - **Allocations:** The number of allocations performed by the backend.
/// - **Footprint:** The total heap memory consumed by the backend.
/// - **Contiguous:** True if the returned symbols have contiguous values.
/// - **Iteration:** Efficiency of iterating over the interned strings.
///
/// Rating varies between **bad**, **ok**, **good** and **best**.
///
/// | Scenario    |  Rating  |
/// |:------------|:--------:|
/// | Fill        | **good** |
/// | Resolve     | **best**   |
/// | Allocations | **good** |
/// | Footprint   | **ok**   |
/// | Supports `get_or_intern_static` | **yes** |
/// | `Send` + `Sync` | **yes** |
/// | Contiguous  | **yes**  |
/// | Iteration   | **best** |
#[derive(Debug)]
pub struct BucketBackend<S = DefaultSymbol> {
    spans: Vec<InternedStr>,
    head: FixedString,
    full: Vec<String>,
    marker: PhantomData<fn() -> S>,
}

/// # Safety
///
/// The bucket backend requires a manual [`Send`] impl because it is self
/// referential. When cloning a bucket backend a deep clone is performed and
/// all references to itself are updated for the clone.
unsafe impl<S> Send for BucketBackend<S> where S: Symbol {}

/// # Safety
///
/// The bucket backend requires a manual [`Send`] impl because it is self
/// referential. Those references won't escape its own scope and also
/// the bucket backend has no interior mutability.
unsafe impl<S> Sync for BucketBackend<S> where S: Symbol {}

impl<S> Default for BucketBackend<S> {
    #[cfg_attr(feature = "inline-more", inline)]
    fn default() -> Self {
        Self {
            spans: Vec::new(),
            head: FixedString::default(),
            full: Vec::new(),
            marker: Default::default(),
        }
    }
}

impl<S> Backend for BucketBackend<S>
where
    S: Symbol,
{
    type Symbol = S;
    type Iter<'a>
        = Iter<'a, S>
    where
        Self: 'a;

    #[cfg_attr(feature = "inline-more", inline)]
    fn with_capacity(cap: usize) -> Self {
        Self {
            spans: Vec::with_capacity(cap),
            head: FixedString::with_capacity(cap),
            full: Vec::new(),
            marker: Default::default(),
        }
    }

    #[inline]
    fn intern(&mut self, string: &str) -> Self::Symbol {
        // SAFETY: This is safe because we never hand out the returned
        //         interned string instance to the outside and only operate
        //         on it within this backend.
        let interned = unsafe { self.alloc(string) };
        self.push_span(interned)
    }

    #[cfg_attr(feature = "inline-more", inline)]
    fn intern_static(&mut self, string: &'static str) -> Self::Symbol {
        let interned = InternedStr::new(string);
        self.push_span(interned)
    }

    fn shrink_to_fit(&mut self) {
        self.spans.shrink_to_fit();
        // Commenting out the below line fixes: https://github.com/Robbepop/string-interner/issues/46
        // self.head.shrink_to_fit();
        self.full.shrink_to_fit();
    }

    #[inline]
    fn resolve(&self, symbol: Self::Symbol) -> Option<&str> {
        self.spans.get(symbol.to_usize()).map(InternedStr::as_str)
    }

    #[inline]
    unsafe fn resolve_unchecked(&self, symbol: Self::Symbol) -> &str {
        // SAFETY: The function is marked unsafe so that the caller guarantees
        //         that required invariants are checked.
        unsafe { self.spans.get_unchecked(symbol.to_usize()).as_str() }
    }

    #[inline]
    fn iter(&self) -> Self::Iter<'_> {
        Iter::new(self)
    }
}

impl<S> BucketBackend<S>
where
    S: Symbol,
{
    /// Returns the next available symbol.
    fn next_symbol(&self) -> S {
        expect_valid_symbol(self.spans.len())
    }

    /// Pushes the given interned string into the spans and returns its symbol.
    fn push_span(&mut self, interned: InternedStr) -> S {
        let symbol = self.next_symbol();
        self.spans.push(interned);
        symbol
    }

    /// Interns a new string into the backend and returns a reference to it.
    unsafe fn alloc(&mut self, string: &str) -> InternedStr {
        let cap = self.head.capacity();
        if cap < self.head.len() + string.len() {
            let new_cap = (usize::max(cap, string.len()) + 1).next_power_of_two();
            let new_head = FixedString::with_capacity(new_cap);
            let old_head = core::mem::replace(&mut self.head, new_head);
            self.full.push(old_head.finish());
        }
        self.head
            .push_str(string)
            .expect("encountered invalid head capacity (2)")
    }
}

impl<S> Clone for BucketBackend<S> {
    fn clone(&self) -> Self {
        // For performance reasons we copy all cloned strings into a single cloned
        // head string leaving the cloned `full` empty.
        let new_head_cap =
            self.head.capacity() + self.full.iter().fold(0, |lhs, rhs| lhs + rhs.len());
        let mut head = FixedString::with_capacity(new_head_cap);
        let mut spans = Vec::with_capacity(self.spans.len());
        for span in &self.spans {
            let string = span.as_str();
            let interned = head
                .push_str(string)
                .expect("encountered invalid head capacity");
            spans.push(interned);
        }
        Self {
            spans,
            head,
            full: Vec::new(),
            marker: Default::default(),
        }
    }
}

impl<S> Eq for BucketBackend<S> where S: Symbol {}

impl<S> PartialEq for BucketBackend<S>
where
    S: Symbol,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn eq(&self, other: &Self) -> bool {
        self.spans == other.spans
    }
}

impl<'a, S> IntoIterator for &'a BucketBackend<S>
where
    S: Symbol,
{
    type Item = (S, &'a str);
    type IntoIter = Iter<'a, S>;

    #[cfg_attr(feature = "inline-more", inline)]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct Iter<'a, S> {
    iter: Enumerate<slice::Iter<'a, InternedStr>>,
    symbol_marker: PhantomData<fn() -> S>,
}

impl<'a, S> Iter<'a, S> {
    #[cfg_attr(feature = "inline-more", inline)]
    pub fn new(backend: &'a BucketBackend<S>) -> Self {
        Self {
            iter: backend.spans.iter().enumerate(),
            symbol_marker: Default::default(),
        }
    }
}

impl<'a, S> Iterator for Iter<'a, S>
where
    S: Symbol,
{
    type Item = (S, &'a str);

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter
            .next()
            .map(|(id, interned)| (expect_valid_symbol(id), interned.as_str()))
    }
}
