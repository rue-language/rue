//! Structurally shared, additively extended sorted sequences (RUE-1112).
//!
//! A [`SharedSegments`] holds a canonical sorted sequence as an ordered list of
//! `Arc`-shared, mutually-disjoint sorted segments. A strictly-additive successor
//! appends exactly one new segment and shares every predecessor segment by `Arc`
//! clone — no predecessor element is copied, sorted, or reallocated, and a chain
//! of successors accumulates one segment per acquisition round (bounded by the
//! driver's round bound) rather than flattening. The merged flat slice is
//! materialized at most once, lazily, and only when a consumer needs a contiguous
//! `&[T]`; the acquisition path reads the newest delta segment directly and never
//! triggers it.
//!
//! Equality and hashing are over the LOGICAL merged sequence, computed by a
//! streaming k-way merge with no allocation of the merged buffer, so a flat value
//! and a segmented value with identical content compare and hash identically —
//! the representation is invisible to query memoization.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

/// A canonical sorted sequence represented as an ordered list of `Arc`-shared,
/// mutually-disjoint sorted segments. `cmp` is the canonical total order every
/// segment is sorted by; the segments partition the logical sequence, so their
/// k-way merge is the sorted union.
pub(crate) struct SharedSegments<T> {
    segments: Arc<[Arc<[T]>]>,
    cmp: fn(&T, &T) -> Ordering,
    merged: OnceLock<Arc<[T]>>,
}

impl<T> SharedSegments<T> {
    /// A flat sequence: one segment. `items` must already be canonical (sorted by
    /// `cmp`, deduplicated).
    pub(crate) fn flat(items: Arc<[T]>, cmp: fn(&T, &T) -> Ordering) -> Self {
        Self {
            segments: Arc::from([items]),
            cmp,
            merged: OnceLock::new(),
        }
    }

    /// The first (oldest) segment: the original base carried through every
    /// successor by reference. Two successors of the same base return
    /// `Arc`-pointer-equal first segments, proving the base was never copied.
    pub(crate) fn predecessor_segment(&self) -> &Arc<[T]> {
        &self.segments[0]
    }

    /// The newest delta segment (empty for a flat value). Iterating this avoids
    /// materializing the merged sequence on the acquisition path.
    pub(crate) fn delta_segment(&self) -> &[T] {
        if self.segments.len() > 1 {
            &self.segments[self.segments.len() - 1]
        } else {
            &[]
        }
    }

    /// Every `Arc`-shared segment, oldest first. Two successors of a common
    /// ancestor share every ancestor segment by pointer; consumers use this to
    /// witness structural inheritance without walking predecessor entries.
    pub(crate) fn segments(&self) -> &[Arc<[T]>] {
        &self.segments
    }

    pub(crate) fn len(&self) -> usize {
        self.segments.iter().map(|segment| segment.len()).sum()
    }

    /// Whether any element compares `Equal` under `f`, by binary search over each
    /// sorted segment — O(segments · log n) with no materialization. `f` must be
    /// consistent with the canonical order.
    pub(crate) fn contains_by(&self, mut f: impl FnMut(&T) -> Ordering) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.binary_search_by(&mut f).is_ok())
    }

    /// The element comparing `Equal` under `f`, by binary search over each sorted
    /// segment — with no materialization.
    pub(crate) fn find_by(&self, mut f: impl FnMut(&T) -> Ordering) -> Option<&T> {
        for segment in self.segments.iter() {
            if let Ok(index) = segment.binary_search_by(&mut f) {
                return Some(&segment[index]);
            }
        }
        None
    }

    /// Iterate the logical merged sequence in canonical order without allocating
    /// the merged buffer (a small per-segment cursor vector aside).
    pub(crate) fn iter(&self) -> MergeIter<'_, T> {
        MergeIter {
            cursors: self
                .segments
                .iter()
                .map(|segment| (&**segment, 0))
                .collect(),
            cmp: self.cmp,
        }
    }
}

impl<T: Clone> SharedSegments<T> {
    /// Build a strictly-additive successor that carries every segment of `base`
    /// by `Arc` clone and appends `delta` as one new segment. `delta` must be
    /// disjoint from `base` and is sorted here (only `delta`, never `base`). No
    /// base segment is copied or re-sorted, and the base is NOT flattened, so a
    /// chain of successors stays O(round) segments.
    pub(crate) fn extend(base: &SharedSegments<T>, mut delta: Vec<T>) -> Self {
        delta.sort_by(base.cmp);
        delta.dedup_by(|a, b| (base.cmp)(a, b) == Ordering::Equal);
        let mut segments: Vec<Arc<[T]>> = base.segments.to_vec();
        segments.push(delta.into());
        Self {
            segments: segments.into(),
            cmp: base.cmp,
            merged: OnceLock::new(),
        }
    }

    /// The logical merged sequence as one contiguous slice. A flat value borrows
    /// its single stored `Arc` directly; a segmented value materializes the k-way
    /// merge once into a cached `Arc`. This is the only place a successor ever
    /// allocates a predecessor-sized buffer, and the acquisition path never calls
    /// it (it reads `delta_segment` instead).
    pub(crate) fn as_slice(&self) -> &[T] {
        if self.segments.len() == 1 {
            return &self.segments[0];
        }
        self.merged
            .get_or_init(|| self.iter().cloned().collect::<Vec<_>>().into())
    }
}

/// Streaming k-way merge over the segments (each sorted, mutually disjoint).
pub(crate) struct MergeIter<'a, T> {
    cursors: Vec<(&'a [T], usize)>,
    cmp: fn(&T, &T) -> Ordering,
}

impl<'a, T> Iterator for MergeIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        let mut best: Option<usize> = None;
        for (index, (segment, position)) in self.cursors.iter().enumerate() {
            if let Some(candidate) = segment.get(*position) {
                match best {
                    None => best = Some(index),
                    Some(current) => {
                        let (best_segment, best_position) = self.cursors[current];
                        if (self.cmp)(candidate, &best_segment[best_position]) == Ordering::Less {
                            best = Some(index);
                        }
                    }
                }
            }
        }
        let chosen = best?;
        let segment = self.cursors[chosen].0;
        let position = self.cursors[chosen].1;
        self.cursors[chosen].1 = position + 1;
        Some(&segment[position])
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining: usize = self
            .cursors
            .iter()
            .map(|(segment, position)| segment.len() - position)
            .sum();
        (remaining, Some(remaining))
    }
}

impl<'a, T> ExactSizeIterator for MergeIter<'a, T> {}

impl<T: Clone> Clone for SharedSegments<T> {
    fn clone(&self) -> Self {
        // Share all segments by `Arc` clone; the materialized cache is not carried
        // so a clone holds no predecessor-sized buffer.
        Self {
            segments: Arc::clone(&self.segments),
            cmp: self.cmp,
            merged: OnceLock::new(),
        }
    }
}

impl<T: PartialEq> PartialEq for SharedSegments<T> {
    fn eq(&self, other: &Self) -> bool {
        // Fast path: identical segment lists by pointer.
        if self.segments.len() == other.segments.len()
            && self
                .segments
                .iter()
                .zip(other.segments.iter())
                .all(|(a, b)| Arc::ptr_eq(a, b))
        {
            return true;
        }
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<T: Eq> Eq for SharedSegments<T> {}

impl<T: Hash> Hash for SharedSegments<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Match `<[T] as Hash>`: length then each element in canonical order, so a
        // flat value and a segmented value with identical content hash identically.
        self.len().hash(state);
        for item in self.iter() {
            item.hash(state);
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for SharedSegments<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// An append-only ORDERED sequence (no sort relation) represented as `Arc`-shared
/// concatenated segments. A strictly-additive successor appends exactly one new
/// segment and shares every predecessor segment by `Arc` clone; the logical
/// sequence is the segments in order, back to back. Equality, hashing, and debug
/// rendering are over the logical concatenation, so representation is invisible
/// to memoization. The contiguous slice materializes lazily, at most once per
/// value, and never on a path that only iterates.
pub(crate) struct SharedList<T> {
    segments: Arc<[Arc<[T]>]>,
    merged: OnceLock<Arc<[T]>>,
}

impl<T> SharedList<T> {
    /// A flat sequence: one segment holding the items in order.
    pub(crate) fn flat(items: Arc<[T]>) -> Self {
        Self {
            segments: Arc::from([items]),
            merged: OnceLock::new(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.segments.iter().map(|segment| segment.len()).sum()
    }

    /// The logical sequence in order, streaming across segments.
    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = &T> {
        ListIter {
            segments: &self.segments,
            segment: 0,
            position: 0,
            remaining: self.len(),
        }
    }
}

struct ListIter<'a, T> {
    segments: &'a [Arc<[T]>],
    segment: usize,
    position: usize,
    remaining: usize,
}

impl<'a, T> Iterator for ListIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        loop {
            let segment = self.segments.get(self.segment)?;
            if let Some(item) = segment.get(self.position) {
                self.position += 1;
                self.remaining -= 1;
                return Some(item);
            }
            self.segment += 1;
            self.position = 0;
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<'a, T> ExactSizeIterator for ListIter<'a, T> {}

impl<T: Clone> SharedList<T> {
    /// Build a strictly-additive successor that carries every segment of `base`
    /// by `Arc` clone and appends `delta` in order as one new segment. No base
    /// segment is copied and the base is never flattened.
    pub(crate) fn extend(base: &SharedList<T>, delta: Vec<T>) -> Self {
        let mut segments: Vec<Arc<[T]>> = base.segments.to_vec();
        segments.push(delta.into());
        Self {
            segments: segments.into(),
            merged: OnceLock::new(),
        }
    }

    /// The logical sequence as one contiguous shared slice, materialized lazily
    /// at most once per value; a flat value shares its single segment directly.
    pub(crate) fn as_arc(&self) -> Arc<[T]> {
        self.merged
            .get_or_init(|| {
                if self.segments.len() == 1 {
                    self.segments[0].clone()
                } else {
                    self.iter().cloned().collect::<Vec<_>>().into()
                }
            })
            .clone()
    }
}

impl<T: Clone> Clone for SharedList<T> {
    fn clone(&self) -> Self {
        // Share all segments by `Arc` clone; the materialized cache is not
        // carried so a clone holds no predecessor-sized buffer.
        Self {
            segments: Arc::clone(&self.segments),
            merged: OnceLock::new(),
        }
    }
}

impl<T: PartialEq> PartialEq for SharedList<T> {
    fn eq(&self, other: &Self) -> bool {
        // Fast path: identical segment lists by pointer.
        if self.segments.len() == other.segments.len()
            && self
                .segments
                .iter()
                .zip(other.segments.iter())
                .all(|(a, b)| Arc::ptr_eq(a, b))
        {
            return true;
        }
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<T: Eq> Eq for SharedList<T> {}

impl<T: Hash> Hash for SharedList<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Match `<[T] as Hash>`: length then each element in logical order, so a
        // flat value and a segmented value with identical content hash
        // identically.
        self.len().hash(state);
        for item in self.iter() {
            item.hash(state);
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for SharedList<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    fn cmp(a: &i32, b: &i32) -> Ordering {
        a.cmp(b)
    }

    fn hash_of(segments: &SharedSegments<i32>) -> u64 {
        let mut hasher = DefaultHasher::new();
        segments.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn successor_merges_disjoint_sorted_segments_and_matches_flat() {
        let predecessor: Arc<[i32]> = Arc::from([1, 3, 5, 7]);
        let base = SharedSegments::flat(Arc::clone(&predecessor), cmp);
        let successor = SharedSegments::extend(&base, vec![8, 2, 6]);
        assert_eq!(successor.as_slice(), &[1, 2, 3, 5, 6, 7, 8]);
        assert_eq!(
            successor.iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 3, 5, 6, 7, 8]
        );
        assert_eq!(successor.len(), 7);
        assert_eq!(successor.delta_segment(), &[2, 6, 8]);

        let flat = SharedSegments::flat(Arc::from([1, 2, 3, 5, 6, 7, 8]), cmp);
        assert_eq!(successor, flat);
        assert_eq!(hash_of(&successor), hash_of(&flat));
    }

    #[test]
    fn chained_successors_stay_multi_segment_and_share_every_ancestor() {
        let base = SharedSegments::flat(Arc::from([10, 20, 30]), cmp);
        let first = SharedSegments::extend(&base, vec![15, 25]);
        let second = SharedSegments::extend(&first, vec![5, 35]);

        // The second successor is three segments (no flattening of `first`).
        assert_eq!(second.segments().len(), 3);
        // Every ancestor segment is shared by pointer with `first`.
        assert!(Arc::ptr_eq(&second.segments()[0], &first.segments()[0]));
        assert!(Arc::ptr_eq(&second.segments()[1], &first.segments()[1]));
        assert!(Arc::ptr_eq(
            second.predecessor_segment(),
            base.predecessor_segment()
        ));

        // Logical content and equivalence with a flat value are preserved.
        assert_eq!(
            second.iter().copied().collect::<Vec<_>>(),
            vec![5, 10, 15, 20, 25, 30, 35]
        );
        let flat = SharedSegments::flat(Arc::from([5, 10, 15, 20, 25, 30, 35]), cmp);
        assert_eq!(second, flat);
        assert_eq!(hash_of(&second), hash_of(&flat));
        assert!(second.contains_by(|value| value.cmp(&25)));
        assert!(!second.contains_by(|value| value.cmp(&26)));
        assert_eq!(second.find_by(|value| value.cmp(&35)), Some(&35));
    }

    #[test]
    fn flat_borrows_without_materializing() {
        let items: Arc<[i32]> = Arc::from([1, 2, 3]);
        let flat = SharedSegments::flat(Arc::clone(&items), cmp);
        assert!(std::ptr::eq(flat.as_slice().as_ptr(), items.as_ptr()));
        assert_eq!(flat.delta_segment(), &[] as &[i32]);
    }

    #[test]
    fn shared_list_extends_in_order_sharing_ancestors() {
        let base = SharedList::flat(Arc::from([30, 10, 20]));
        let first = SharedList::extend(&base, vec![5, 25]);
        let second = SharedList::extend(&first, vec![15]);

        // Order is concatenation order, never sorted; ancestors are shared by
        // pointer and never flattened.
        assert_eq!(
            second.iter().copied().collect::<Vec<_>>(),
            vec![30, 10, 20, 5, 25, 15]
        );
        assert_eq!(second.len(), 6);
        assert!(Arc::ptr_eq(&second.segments[0], &first.segments[0]));
        assert!(Arc::ptr_eq(&second.segments[1], &first.segments[1]));

        // Logical equality/hash match a flat value with identical content.
        let flat = SharedList::flat(Arc::from([30, 10, 20, 5, 25, 15]));
        assert_eq!(second, flat);
        assert_eq!(hash_of_list(&second), hash_of_list(&flat));
        assert_eq!(second.as_arc().as_ref(), flat.as_arc().as_ref());

        // A flat value's contiguous form shares its single segment directly.
        let items: Arc<[i32]> = Arc::from([1, 2]);
        let single = SharedList::flat(Arc::clone(&items));
        assert!(std::ptr::eq(single.as_arc().as_ptr(), items.as_ptr()));
    }

    fn hash_of_list(value: &SharedList<i32>) -> u64 {
        use std::hash::{DefaultHasher, Hasher as _};
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }
}
