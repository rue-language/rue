//! Structurally shared, additively extended sorted sequences (RUE-1112).
//!
//! A [`SharedSegments`] holds a canonical sorted sequence as an ordered list of
//! `Arc`-shared, mutually-disjoint sorted segments. Successors use size-tiered
//! compaction: untouched tiers stay shared, while adjacent tiers of the same
//! magnitude merge. Segment depth is therefore bounded by the number of bits in
//! `usize`, and each element participates in at most logarithmically many merges.
//! The exact newest delta and direct-parent lineage remain available to the
//! acquisition path even when its storage tier was compacted.
//!
//! Equality and hashing are over the LOGICAL merged sequence, computed by a
//! streaming k-way merge with no allocation of the merged buffer, so a flat value
//! and a segmented value with identical content compare and hash identically —
//! the representation is invisible to query memoization.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

/// A size-tiered sequence has at most one segment at each possible `usize`
/// magnitude, plus the possible empty flat base.
pub(crate) const MAX_SIZE_TIERED_SEGMENTS: usize = usize::BITS as usize + 1;

fn size_tier(len: usize) -> u32 {
    if len == 0 {
        0
    } else {
        usize::BITS - len.leading_zeros() - 1
    }
}

/// Append one immutable segment and merge tail tiers until their size tiers are
/// strictly descending from oldest to newest. The merge callback preserves the
/// sequence-specific logical order.
pub(crate) fn push_size_tiered_segment<T: ?Sized>(
    segments: &mut Vec<Arc<T>>,
    mut appended: Arc<T>,
    len: impl Fn(&T) -> usize,
    mut merge: impl FnMut(&T, &T) -> Arc<T>,
) {
    while let Some(previous) = segments.last() {
        if size_tier(len(previous)) > size_tier(len(&appended)) {
            break;
        }
        let previous = segments.pop().expect("the tail segment exists");
        appended = merge(&previous, &appended);
    }
    segments.push(appended);
    assert!(segments.len() <= MAX_SIZE_TIERED_SEGMENTS);
}

#[derive(Debug)]
struct SegmentLineage<T> {
    parent: Option<Arc<SegmentLineage<T>>>,
    delta: Arc<[T]>,
    root_segment: Arc<[T]>,
}

/// A canonical sorted sequence represented as an ordered list of `Arc`-shared,
/// mutually-disjoint sorted segments. `cmp` is the canonical total order every
/// segment is sorted by; the segments partition the logical sequence, so their
/// k-way merge is the sorted union.
pub(crate) struct SharedSegments<T> {
    segments: Arc<[Arc<[T]>]>,
    cmp: fn(&T, &T) -> Ordering,
    merged: Arc<OnceLock<Arc<[T]>>>,
    lineage: Arc<SegmentLineage<T>>,
}

impl<T> SharedSegments<T> {
    /// A flat sequence: one segment. `items` must already be canonical (sorted by
    /// `cmp`, deduplicated).
    pub(crate) fn flat(items: Arc<[T]>, cmp: fn(&T, &T) -> Ordering) -> Self {
        Self {
            segments: Arc::from([Arc::clone(&items)]),
            cmp,
            merged: Arc::new(OnceLock::new()),
            lineage: Arc::new(SegmentLineage {
                parent: None,
                delta: Arc::from([]),
                root_segment: items,
            }),
        }
    }

    /// The original flat segment retained as a stable lineage witness. It is not
    /// necessarily one of the current compacted storage tiers.
    pub(crate) fn predecessor_segment(&self) -> &Arc<[T]> {
        &self.lineage.root_segment
    }

    /// The exact newest logical delta (empty for a flat value), independent of
    /// whether its physical storage tier was compacted.
    pub(crate) fn delta_segment(&self) -> &[T] {
        &self.lineage.delta
    }

    /// Every bounded storage tier, oldest first.
    pub(crate) fn segments(&self) -> &[Arc<[T]>] {
        &self.segments
    }

    /// Return the exact appended values when `ancestor` is this value or its
    /// direct structural parent. Pointer identity makes this a constant-time
    /// ancestry proof even when size-tiered compaction replaced storage tiers.
    pub(crate) fn direct_delta_from(&self, ancestor: &Self) -> Option<&[T]> {
        if Arc::ptr_eq(&self.lineage, &ancestor.lineage) {
            return Some(&[]);
        }
        self.lineage
            .parent
            .as_ref()
            .is_some_and(|parent| Arc::ptr_eq(parent, &ancestor.lineage))
            .then_some(self.lineage.delta.as_ref())
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

    /// Every element comparing `Equal` under `f`, by binary search for the equal
    /// range of each sorted segment — O(segments · log n + matches), with no
    /// materialization and no whole-sequence scan. `f` must be consistent with
    /// the canonical order, so each segment's matches are contiguous. Matches are
    /// yielded segment by segment; a caller needing canonical order across
    /// segments sorts the (small) result itself.
    pub(crate) fn for_each_matching<'a>(
        &'a self,
        f: impl Fn(&T) -> Ordering,
        mut visit: impl FnMut(&'a T),
    ) {
        for segment in self.segments.iter() {
            let start = segment.partition_point(|value| f(value) == Ordering::Less);
            let end = segment.partition_point(|value| f(value) != Ordering::Greater);
            for value in &segment[start..end] {
                visit(value);
            }
        }
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
    /// Build a strictly-additive successor. `delta` must be disjoint from `base`
    /// and is sorted here. Untouched size tiers remain shared; tail tiers merge
    /// only when necessary to preserve the bounded-depth invariant.
    pub(crate) fn extend(base: &SharedSegments<T>, mut delta: Vec<T>) -> Self {
        delta.sort_by(base.cmp);
        delta.dedup_by(|a, b| (base.cmp)(a, b) == Ordering::Equal);
        let delta: Arc<[T]> = delta.into();
        let mut segments: Vec<Arc<[T]>> = base.segments.to_vec();
        if !delta.is_empty() {
            push_size_tiered_segment(
                &mut segments,
                Arc::clone(&delta),
                <[T]>::len,
                |left, right| merge_sorted(left, right, base.cmp),
            );
        }
        Self {
            segments: segments.into(),
            cmp: base.cmp,
            merged: Arc::new(OnceLock::new()),
            lineage: Arc::new(SegmentLineage {
                parent: Some(Arc::clone(&base.lineage)),
                delta,
                root_segment: Arc::clone(&base.lineage.root_segment),
            }),
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

fn merge_sorted<T: Clone>(left: &[T], right: &[T], cmp: fn(&T, &T) -> Ordering) -> Arc<[T]> {
    let mut merged = Vec::with_capacity(left.len() + right.len());
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        if cmp(&right[right_index], &left[left_index]) == Ordering::Less {
            merged.push(right[right_index].clone());
            right_index += 1;
        } else {
            merged.push(left[left_index].clone());
            left_index += 1;
        }
    }
    merged.extend_from_slice(&left[left_index..]);
    merged.extend_from_slice(&right[right_index..]);
    merged.into()
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
        Self {
            segments: Arc::clone(&self.segments),
            cmp: self.cmp,
            merged: Arc::clone(&self.merged),
            lineage: Arc::clone(&self.lineage),
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
/// size-tiered concatenated segments. Untouched tiers stay shared and compacted
/// tail tiers preserve concatenation order. Equality, hashing, and debug
/// rendering are over the logical concatenation, so representation is invisible
/// to memoization. Clones share the lazily materialized contiguous slice.
pub(crate) struct SharedList<T> {
    segments: Arc<[Arc<[T]>]>,
    merged: Arc<OnceLock<Arc<[T]>>>,
}

impl<T> SharedList<T> {
    /// A flat sequence: one segment holding the items in order.
    pub(crate) fn flat(items: Arc<[T]>) -> Self {
        Self {
            segments: Arc::from([items]),
            merged: Arc::new(OnceLock::new()),
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
    /// Build a strictly-additive successor, compacting equal-magnitude tail
    /// tiers while preserving concatenation order.
    pub(crate) fn extend(base: &SharedList<T>, delta: Vec<T>) -> Self {
        let mut segments: Vec<Arc<[T]>> = base.segments.to_vec();
        let delta: Arc<[T]> = delta.into();
        if !delta.is_empty() {
            push_size_tiered_segment(&mut segments, delta, <[T]>::len, |left, right| {
                left.iter()
                    .chain(right.iter())
                    .cloned()
                    .collect::<Vec<_>>()
                    .into()
            });
        }
        Self {
            segments: segments.into(),
            merged: Arc::new(OnceLock::new()),
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
        Self {
            segments: Arc::clone(&self.segments),
            merged: Arc::clone(&self.merged),
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
    fn chained_successors_compact_tail_tiers_and_preserve_direct_lineage() {
        let base = SharedSegments::flat(Arc::from([10, 20, 30]), cmp);
        let first = SharedSegments::extend(&base, vec![15, 25]);
        let second = SharedSegments::extend(&first, vec![5, 35]);

        // The first extension merges equal-magnitude tiers. The next smaller
        // delta stays separate and shares the untouched compacted tier.
        assert_eq!(first.segments().len(), 1);
        assert_eq!(second.segments().len(), 2);
        assert!(Arc::ptr_eq(&second.segments()[0], &first.segments()[0]));
        assert_eq!(second.direct_delta_from(&first), Some(&[5, 35][..]));
        assert!(first.direct_delta_from(&base).is_some());
        assert!(second.direct_delta_from(&base).is_none());

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
    fn shared_list_extends_in_order_with_size_tiered_compaction() {
        let base = SharedList::flat(Arc::from([30, 10, 20]));
        let first = SharedList::extend(&base, vec![5, 25]);
        let second = SharedList::extend(&first, vec![15]);

        // Order is concatenation order, never sorted. The equal-magnitude first
        // extension compacts, then the smaller tail remains separate.
        assert_eq!(
            second.iter().copied().collect::<Vec<_>>(),
            vec![30, 10, 20, 5, 25, 15]
        );
        assert_eq!(second.len(), 6);
        assert_eq!(first.segments.len(), 1);
        assert_eq!(second.segments.len(), 2);
        assert!(Arc::ptr_eq(&second.segments[0], &first.segments[0]));

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

    #[test]
    fn one_item_rounds_keep_depth_logarithmic_and_share_materialization() {
        let mut value = SharedSegments::flat(Arc::from([0]), cmp);
        let mut max_depth = value.segments().len();
        for item in 1..=4096 {
            let successor = SharedSegments::extend(&value, vec![item]);
            assert_eq!(successor.direct_delta_from(&value), Some(&[item][..]));
            max_depth = max_depth.max(successor.segments().len());
            assert!(successor.segments().len() <= MAX_SIZE_TIERED_SEGMENTS);
            value = successor;
        }

        assert!(max_depth <= 13, "4097 entries need at most 13 size tiers");
        assert_eq!(value.len(), 4097);
        let clone = value.clone();
        let materialized = value.as_slice().as_ptr();
        assert_eq!(clone.as_slice().as_ptr(), materialized);
    }

    fn hash_of_list(value: &SharedList<i32>) -> u64 {
        use std::hash::{DefaultHasher, Hasher as _};
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }
}
