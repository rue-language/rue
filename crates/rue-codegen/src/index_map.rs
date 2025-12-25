//! Type-safe index map for handle types.
//!
//! This module provides `IndexMap<H, V>`, a vector-backed map that uses
//! handle types (like `VReg`) as keys. This provides type safety over
//! raw `Vec<V>` indexing patterns like `vec[handle.index() as usize]`.

use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

/// A trait for handle types that can be used as keys in an `IndexMap`.
///
/// Handle types are lightweight wrappers around indices (typically `u32`)
/// that provide type safety. Examples include `VReg` and `LabelId`.
pub trait Handle: Copy {
    /// Get the underlying index of this handle.
    fn index(self) -> u32;

    /// Create a handle from an index.
    fn from_index(index: u32) -> Self;
}

/// A vector-backed map using handle types as keys.
///
/// This provides type-safe indexing over raw vector access patterns.
/// Instead of `allocation[vreg.index() as usize]`, you can write
/// `allocation[vreg]`.
///
/// # Example
///
/// ```ignore
/// use rue_codegen::index_map::{IndexMap, Handle};
/// use rue_codegen::VReg;
///
/// let mut map: IndexMap<VReg, Option<i32>> = IndexMap::new();
/// map.resize(10, None);
///
/// let vreg = VReg::new(5);
/// map[vreg] = Some(42);
/// assert_eq!(map[vreg], Some(42));
/// ```
#[derive(Debug, Clone)]
pub struct IndexMap<H, V> {
    data: Vec<V>,
    _marker: PhantomData<H>,
}

impl<H: Handle, V> IndexMap<H, V> {
    /// Create a new empty index map.
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Create an index map with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            data: Vec::with_capacity(capacity),
            _marker: PhantomData,
        }
    }

    /// Get the number of entries in the map.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the map is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Get a reference to a value by handle, returning `None` if out of bounds.
    pub fn get(&self, handle: H) -> Option<&V> {
        self.data.get(handle.index() as usize)
    }

    /// Get a mutable reference to a value by handle, returning `None` if out of bounds.
    pub fn get_mut(&mut self, handle: H) -> Option<&mut V> {
        self.data.get_mut(handle.index() as usize)
    }

    /// Push a value and return the handle for it.
    pub fn push(&mut self, value: V) -> H {
        let index = self.data.len() as u32;
        self.data.push(value);
        H::from_index(index)
    }

    /// Iterate over all values.
    pub fn iter(&self) -> impl Iterator<Item = &V> {
        self.data.iter()
    }

    /// Iterate over all values mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.data.iter_mut()
    }

    /// Iterate over (handle, value) pairs.
    pub fn iter_enumerated(&self) -> impl Iterator<Item = (H, &V)> {
        self.data
            .iter()
            .enumerate()
            .map(|(i, v)| (H::from_index(i as u32), v))
    }
}

impl<H: Handle, V: Clone> IndexMap<H, V> {
    /// Resize the map to contain `new_len` elements, filling new slots with `value`.
    pub fn resize(&mut self, new_len: usize, value: V) {
        self.data.resize(new_len, value);
    }
}

impl<H: Handle, V: Default> IndexMap<H, V> {
    /// Resize the map to contain `new_len` elements, filling new slots with default values.
    pub fn resize_default(&mut self, new_len: usize) {
        self.data.resize_with(new_len, V::default);
    }
}

impl<H: Handle, V> Default for IndexMap<H, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Handle, V> Index<H> for IndexMap<H, V> {
    type Output = V;

    fn index(&self, handle: H) -> &Self::Output {
        &self.data[handle.index() as usize]
    }
}

impl<H: Handle, V> IndexMut<H> for IndexMap<H, V> {
    fn index_mut(&mut self, handle: H) -> &mut Self::Output {
        &mut self.data[handle.index() as usize]
    }
}

impl<H: Handle, V> FromIterator<V> for IndexMap<H, V> {
    fn from_iter<I: IntoIterator<Item = V>>(iter: I) -> Self {
        Self {
            data: iter.into_iter().collect(),
            _marker: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Simple test handle type
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestHandle(u32);

    impl Handle for TestHandle {
        fn index(self) -> u32 {
            self.0
        }

        fn from_index(index: u32) -> Self {
            Self(index)
        }
    }

    #[test]
    fn test_index_map_basic() {
        let mut map: IndexMap<TestHandle, i32> = IndexMap::new();
        map.resize(10, 0);

        let h5 = TestHandle(5);
        map[h5] = 42;
        assert_eq!(map[h5], 42);
    }

    #[test]
    fn test_index_map_push() {
        let mut map: IndexMap<TestHandle, &str> = IndexMap::new();

        let h0 = map.push("first");
        let h1 = map.push("second");

        assert_eq!(h0.index(), 0);
        assert_eq!(h1.index(), 1);
        assert_eq!(map[h0], "first");
        assert_eq!(map[h1], "second");
    }

    #[test]
    fn test_index_map_get() {
        let mut map: IndexMap<TestHandle, i32> = IndexMap::new();
        map.resize(5, 0);

        let valid = TestHandle(3);
        let invalid = TestHandle(10);

        assert!(map.get(valid).is_some());
        assert!(map.get(invalid).is_none());
    }

    #[test]
    fn test_index_map_iter_enumerated() {
        let mut map: IndexMap<TestHandle, i32> = IndexMap::new();
        map.push(10);
        map.push(20);
        map.push(30);

        let pairs: Vec<_> = map.iter_enumerated().collect();
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].0.index(), 0);
        assert_eq!(*pairs[0].1, 10);
        assert_eq!(pairs[2].0.index(), 2);
        assert_eq!(*pairs[2].1, 30);
    }
}
