//! Arena-based storage for function and method parameter data.
//!
//! # Overview
//!
//! The `ParamArena` provides centralized, contiguous storage for all parameter data
//! in the compiler. Instead of each `FunctionInfo` and `MethodInfo` owning their own
//! `Vec`s of parameter data, they store a `ParamRange` index into this arena.
//!
//! # Memory Benefits
//!
//! Before (per function with N params):
//! - `param_names: Vec<Spur>` - 24 bytes header + 4*N bytes
//! - `param_types: Vec<Type>` - 24 bytes header + 4*N bytes
//! - `param_modes: Vec<RirParamMode>` - 24 bytes header + N bytes
//! - `param_comptime: Vec<bool>` - 24 bytes header + N bytes
//! - Total: ~96 bytes overhead + ~10*N bytes data
//!
//! After (per function):
//! - `ParamRange` - 8 bytes (start + len)
//! - ~88 bytes saved per function
//!
//! # Cache Locality
//!
//! All parameter data for all functions is stored contiguously, improving cache
//! behavior when iterating over function signatures during type checking.
//!
//! # Usage
//!
//! ```ignore
//! // During declaration gathering, allocate params for a function:
//! let range = arena.alloc(
//!     names.into_iter(),
//!     types.into_iter(),
//!     modes.into_iter(),
//!     comptime.into_iter(),
//! );
//!
//! // Store the range in FunctionInfo instead of the Vec data
//! let func_info = FunctionInfo {
//!     params: range,  // Just 8 bytes instead of 4 Vecs
//!     return_type,
//!     // ...
//! };
//!
//! // Later, access the data through the arena:
//! let types = arena.types(range);  // &[Type]
//! let modes = arena.modes(range);  // &[RirParamMode]
//! ```
//!
//! # Design Decisions
//!
//! ## Separate Vecs vs Interleaved Storage
//!
//! We use separate Vecs for each field (names, types, modes, comptime) rather than
//! an interleaved `Vec<ParamData>` for better cache locality when accessing only
//! one field. Type checking often iterates just over types, while mode checking
//! iterates just over modes.
//!
//! ## Index Type
//!
//! Uses u32 indices (4GB of params is sufficient for any reasonable program).
//! This matches the existing pattern in rue-air (e.g., `Type` uses u32 indices).
//!
//! ## Append-Only Design
//!
//! The arena is append-only during declaration gathering. Once all functions and
//! methods are registered, the data is never modified. This simplifies the
//! implementation and enables safe shared references.
//!
//! ## Method-Specific Allocation
//!
//! Methods always store parameter names (needed for named argument checking).
//! Functions only need names for generic functions (for type substitution).
//! To handle this, both `alloc` and `alloc_method` store names, but functions
//! can pass empty iterators if names aren't needed. Method allocation also
//! requires the source modes and comptime flags: defaulting either would erase
//! part of the callable signature and can desynchronize caller and callee ABI.

use std::sync::Arc;

use crate::types::Type;
use lasso::Spur;
use rue_rir::RirParamMode;

/// A range of parameters in the `ParamArena`.
///
/// This is a lightweight handle (8 bytes) that replaces four `Vec`s (96+ bytes)
/// in `FunctionInfo` and `MethodInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ParamRange {
    /// Starting index into the arena's storage.
    start: u32,
    /// Number of parameters in this range.
    len: u32,
}

impl ParamRange {
    /// Creates an empty parameter range.
    pub const EMPTY: ParamRange = ParamRange { start: 0, len: 0 };

    /// Returns the number of parameters in this range.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns true if this range contains no parameters.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Creates a new range with the given start and length.
    fn new(start: u32, len: u32) -> Self {
        Self { start, len }
    }

    /// Returns the start index.
    fn start(&self) -> usize {
        self.start as usize
    }

    /// Returns the end index (exclusive).
    fn end(&self) -> usize {
        self.start as usize + self.len as usize
    }
}

/// An owned copy of the exact parameter facts selected by one [`ParamRange`].
///
/// This is the safe engine-facing shape for arenas that may be rebased while
/// lazy facts are materialized: callers copy one point's facts and never hold
/// a borrow across a later append or rebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamRangeData {
    names: Vec<Spur>,
    types: Vec<Type>,
    modes: Vec<RirParamMode>,
    comptime: Vec<bool>,
}

impl ParamRangeData {
    pub fn names(&self) -> &[Spur] {
        &self.names
    }

    pub fn types(&self) -> &[Type] {
        &self.types
    }

    pub fn modes(&self) -> &[RirParamMode] {
        &self.modes
    }

    pub fn comptime(&self) -> &[bool] {
        &self.comptime
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Spur, &Type, &RirParamMode, &bool)> {
        self.names
            .iter()
            .zip(&self.types)
            .zip(&self.modes)
            .zip(&self.comptime)
            .map(|(((name, ty), mode), comptime)| (name, ty, mode, comptime))
    }
}

/// The contiguous parameter storage one arena layer owns.
#[derive(Debug, Default, Clone)]
struct ParamStore {
    /// All parameter names, indexed by position in the arena.
    /// For functions without named args, this may contain placeholder values.
    names: Vec<Spur>,

    /// All parameter types, indexed by position in the arena.
    types: Vec<Type>,

    /// All parameter modes (Normal, Inout, Borrow).
    modes: Vec<RirParamMode>,

    /// Whether each parameter is a comptime parameter.
    comptime: Vec<bool>,
}

/// Central storage for all function/method parameter data.
///
/// Stores parameter names, types, modes, and comptime flags in separate
/// contiguous arrays for optimal cache locality during type checking.
///
/// # Immutable base, append-only local layer (RUE-1135)
///
/// An arena may sit on top of an immutable `base` arena shared by every body of
/// one request. Positions below `base_len` live in the base and are never
/// written again; every allocation appends to the local layer and is numbered
/// from `base_len` upwards, so a `ParamRange` means the same thing in the base
/// and in each body layered on it. This is a true base plus append-only overlay
/// rather than copy-on-write: a body that allocates parameters copies nothing
/// that the base already holds. A range never straddles the boundary, because
/// each range is allocated in one call into one layer.
#[derive(Debug, Default, Clone)]
pub struct ParamArena {
    /// The shared immutable prefix, if this arena was derived from one. A base
    /// arena is itself always flat (`base: None`), so lookup is one branch.
    base: Option<Arc<ParamStore>>,
    /// Number of parameters owned by `base`. Local position `i` is arena
    /// position `base_len + i`.
    base_len: usize,
    /// Parameters this arena allocated itself.
    local: ParamStore,
}

impl ParamArena {
    /// Creates a new, empty parameter arena.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new arena with pre-allocated capacity.
    ///
    /// Use when the approximate number of total parameters is known.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            base: None,
            base_len: 0,
            local: ParamStore {
                names: Vec::with_capacity(capacity),
                types: Vec::with_capacity(capacity),
                modes: Vec::with_capacity(capacity),
                comptime: Vec::with_capacity(capacity),
            },
        }
    }

    /// A fresh arena that shares this one's parameters as an immutable base and
    /// appends its own allocations above them (RUE-1135).
    ///
    /// Deriving from an already-sealed arena — one that has a base and has
    /// allocated nothing of its own — shares that base's `Arc` and copies
    /// nothing, which is what makes every per-body derivation O(1). Otherwise
    /// one flat base is materialized first, which also keeps every derived arena
    /// exactly one layer deep so a lookup stays a single branch.
    pub(crate) fn derive_overlay(&self) -> Self {
        let base_len = self.total_params();
        let base = match (&self.base, self.local.types.is_empty()) {
            // Already flat: share the store this arena owns.
            (None, _) => Arc::new(self.local.clone()),
            // Nothing was appended locally: reuse the base store as-is.
            (Some(base), true) => Arc::clone(base),
            // A layered arena that grew: flatten once so the derived arena stays
            // one level deep.
            (Some(base), false) => {
                let mut flat = (**base).clone();
                flat.names.extend_from_slice(&self.local.names);
                flat.types.extend_from_slice(&self.local.types);
                flat.modes.extend_from_slice(&self.local.modes);
                flat.comptime.extend_from_slice(&self.local.comptime);
                Arc::new(flat)
            }
        };
        Self {
            base: Some(base),
            base_len,
            local: ParamStore::default(),
        }
    }

    /// Returns the total number of parameters stored in the arena.
    pub fn total_params(&self) -> usize {
        self.base_len + self.local.types.len()
    }

    /// Copy the exact facts selected by `range` without lending arena storage.
    pub fn copy_range(&self, range: ParamRange) -> ParamRangeData {
        ParamRangeData {
            names: self.names(range).to_vec(),
            types: self.types(range).to_vec(),
            modes: self.modes(range).to_vec(),
            comptime: self.comptime(range).to_vec(),
        }
    }

    /// The layer a range was allocated in, plus the range's offset inside it.
    /// A range never straddles the base boundary: each is allocated in one call.
    #[inline]
    fn layer(&self, range: ParamRange) -> (&ParamStore, usize, usize) {
        if range.start() >= self.base_len {
            let start = range.start() - self.base_len;
            (&self.local, start, start + range.len())
        } else {
            let base = self
                .base
                .as_deref()
                .expect("a range below base_len requires a base layer");
            assert!(
                range.end() <= self.base_len,
                "a parameter range must not straddle the shared base boundary"
            );
            (base, range.start(), range.end())
        }
    }

    /// Allocates storage for a function's parameters.
    ///
    /// All iterators must yield the same number of elements.
    ///
    /// # Panics
    ///
    /// Panics if the iterators yield different numbers of elements.
    pub fn alloc(
        &mut self,
        names: impl IntoIterator<Item = Spur>,
        types: impl IntoIterator<Item = Type>,
        modes: impl IntoIterator<Item = RirParamMode>,
        comptime: impl IntoIterator<Item = bool>,
    ) -> ParamRange {
        let local_start = self.local.types.len();
        let start = self.total_params() as u32;

        // Extend all vectors from the iterators. Allocation is append-only and
        // always lands in the local layer, never in the shared base.
        self.local.names.extend(names);
        let names_len = self.local.names.len() - local_start;

        self.local.types.extend(types);
        let types_len = self.local.types.len() - local_start;

        self.local.modes.extend(modes);
        let modes_len = self.local.modes.len() - local_start;

        self.local.comptime.extend(comptime);
        let comptime_len = self.local.comptime.len() - local_start;

        // Verify all lengths match
        assert_eq!(
            names_len, types_len,
            "ParamArena::alloc: names ({}) and types ({}) have different lengths",
            names_len, types_len
        );
        assert_eq!(
            types_len, modes_len,
            "ParamArena::alloc: types ({}) and modes ({}) have different lengths",
            types_len, modes_len
        );
        assert_eq!(
            modes_len, comptime_len,
            "ParamArena::alloc: modes ({}) and comptime ({}) have different lengths",
            modes_len, comptime_len
        );

        ParamRange::new(start, types_len as u32)
    }

    /// Allocates storage for a method's explicit parameters.
    ///
    /// This deliberately requires the same complete signature data as
    /// [`Self::alloc`]. A method parameter may be `borrow`, `inout`, or
    /// `comptime`; synthesizing Normal/non-comptime defaults here would make the
    /// call-site signature disagree with the method body's ABI (RUE-634).
    pub fn alloc_method(
        &mut self,
        names: impl IntoIterator<Item = Spur>,
        types: impl IntoIterator<Item = Type>,
        modes: impl IntoIterator<Item = RirParamMode>,
        comptime: impl IntoIterator<Item = bool>,
    ) -> ParamRange {
        self.alloc(names, types, modes, comptime)
    }

    /// Returns the parameter names for a range.
    #[inline]
    pub fn names(&self, range: ParamRange) -> &[Spur] {
        let (store, start, end) = self.layer(range);
        &store.names[start..end]
    }

    /// Returns the parameter types for a range.
    #[inline]
    pub fn types(&self, range: ParamRange) -> &[Type] {
        let (store, start, end) = self.layer(range);
        &store.types[start..end]
    }

    /// Returns the parameter modes for a range.
    #[inline]
    pub fn modes(&self, range: ParamRange) -> &[RirParamMode] {
        let (store, start, end) = self.layer(range);
        &store.modes[start..end]
    }

    /// Returns the comptime flags for a range.
    #[inline]
    pub fn comptime(&self, range: ParamRange) -> &[bool] {
        let (store, start, end) = self.layer(range);
        &store.comptime[start..end]
    }

    /// Parameters this arena reads from a shared immutable base without copying.
    pub fn shared_params(&self) -> usize {
        self.base_len
    }

    /// Returns an iterator over all parameter data for a range.
    ///
    /// Useful for cases where you need to iterate all fields together.
    pub fn iter(
        &self,
        range: ParamRange,
    ) -> impl Iterator<Item = (&Spur, &Type, &RirParamMode, &bool)> {
        self.names(range)
            .iter()
            .zip(self.types(range))
            .zip(self.modes(range))
            .zip(self.comptime(range))
            .map(|(((name, ty), mode), comptime)| (name, ty, mode, comptime))
    }

    /// Returns an iterator over (name, type) pairs for a range.
    ///
    /// Useful for method parameter type checking.
    pub fn name_type_pairs(&self, range: ParamRange) -> impl Iterator<Item = (&Spur, &Type)> {
        self.names(range).iter().zip(self.types(range))
    }

    /// Returns an iterator over (type, mode) pairs for a range.
    ///
    /// Useful for function call argument checking.
    pub fn type_mode_pairs(
        &self,
        range: ParamRange,
    ) -> impl Iterator<Item = (&Type, &RirParamMode)> {
        self.types(range).iter().zip(self.modes(range))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::Rodeo;

    fn make_spur(rodeo: &mut Rodeo, s: &str) -> Spur {
        rodeo.get_or_intern(s)
    }

    #[test]
    fn test_empty_arena() {
        let arena = ParamArena::new();
        assert_eq!(arena.total_params(), 0);
    }

    #[test]
    fn test_empty_range() {
        let range = ParamRange::EMPTY;
        assert!(range.is_empty());
        assert_eq!(range.len(), 0);
    }

    #[test]
    fn test_alloc_single_param() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let name = make_spur(&mut rodeo, "x");
        let range = arena.alloc([name], [Type::I32], [RirParamMode::Normal], [false]);

        assert_eq!(range.len(), 1);
        assert_eq!(arena.total_params(), 1);
        assert_eq!(arena.names(range), &[name]);
        assert_eq!(arena.types(range), &[Type::I32]);
        assert_eq!(arena.modes(range), &[RirParamMode::Normal]);
        assert_eq!(arena.comptime(range), &[false]);
    }

    #[test]
    fn test_alloc_multiple_params() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");
        let z = make_spur(&mut rodeo, "z");

        let range = arena.alloc(
            [x, y, z],
            [Type::I32, Type::BOOL, Type::I64],
            [
                RirParamMode::Normal,
                RirParamMode::Inout,
                RirParamMode::Borrow,
            ],
            [false, false, true],
        );

        assert_eq!(range.len(), 3);
        assert_eq!(arena.types(range), &[Type::I32, Type::BOOL, Type::I64]);
        assert_eq!(
            arena.modes(range),
            &[
                RirParamMode::Normal,
                RirParamMode::Inout,
                RirParamMode::Borrow
            ]
        );
        assert_eq!(arena.comptime(range), &[false, false, true]);
    }

    #[test]
    fn test_multiple_functions() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        // First function with 2 params
        let a = make_spur(&mut rodeo, "a");
        let b = make_spur(&mut rodeo, "b");
        let range1 = arena.alloc(
            [a, b],
            [Type::I32, Type::I32],
            [RirParamMode::Normal, RirParamMode::Normal],
            [false, false],
        );

        // Second function with 1 param
        let c = make_spur(&mut rodeo, "c");
        let range2 = arena.alloc([c], [Type::BOOL], [RirParamMode::Inout], [false]);

        // Verify they don't overlap
        assert_eq!(range1.len(), 2);
        assert_eq!(range2.len(), 1);
        assert_eq!(arena.total_params(), 3);

        // Verify data is correct for each range
        assert_eq!(arena.types(range1), &[Type::I32, Type::I32]);
        assert_eq!(arena.types(range2), &[Type::BOOL]);
    }

    #[test]
    fn test_alloc_method() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");

        let range = arena.alloc_method(
            [x, y],
            [Type::I32, Type::BOOL],
            [RirParamMode::Borrow, RirParamMode::Inout],
            [false, true],
        );

        assert_eq!(range.len(), 2);
        assert_eq!(arena.names(range), &[x, y]);
        assert_eq!(arena.types(range), &[Type::I32, Type::BOOL]);
        assert_eq!(
            arena.modes(range),
            &[RirParamMode::Borrow, RirParamMode::Inout]
        );
        assert_eq!(arena.comptime(range), &[false, true]);
    }

    #[test]
    fn test_iter() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");

        let range = arena.alloc(
            [x, y],
            [Type::I32, Type::BOOL],
            [RirParamMode::Normal, RirParamMode::Inout],
            [false, true],
        );

        let items: Vec<_> = arena.iter(range).collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], (&x, &Type::I32, &RirParamMode::Normal, &false));
        assert_eq!(items[1], (&y, &Type::BOOL, &RirParamMode::Inout, &true));
    }

    #[test]
    fn test_name_type_pairs() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");

        let range = arena.alloc(
            [x, y],
            [Type::I32, Type::BOOL],
            [RirParamMode::Normal, RirParamMode::Normal],
            [false, false],
        );

        let pairs: Vec<_> = arena.name_type_pairs(range).collect();
        assert_eq!(pairs, vec![(&x, &Type::I32), (&y, &Type::BOOL)]);
    }

    #[test]
    fn test_type_mode_pairs() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");

        let range = arena.alloc(
            [x, y],
            [Type::I32, Type::BOOL],
            [RirParamMode::Normal, RirParamMode::Inout],
            [false, false],
        );

        let pairs: Vec<_> = arena.type_mode_pairs(range).collect();
        assert_eq!(
            pairs,
            vec![
                (&Type::I32, &RirParamMode::Normal),
                (&Type::BOOL, &RirParamMode::Inout)
            ]
        );
    }

    #[test]
    #[should_panic(expected = "names (2) and types (1) have different lengths")]
    fn test_alloc_mismatched_lengths_panics() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");

        // This should panic - 2 names but only 1 type
        let _ = arena.alloc([x, y], [Type::I32], [RirParamMode::Normal], [false]);
    }

    #[test]
    fn test_with_capacity() {
        let arena = ParamArena::with_capacity(100);
        assert_eq!(arena.total_params(), 0);
        // Can't easily test capacity, but this ensures the method works
    }
}
