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
//! can pass empty iterators if names aren't needed.

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

/// Central storage for all function/method parameter data.
///
/// Stores parameter names, types, modes, and comptime flags in separate
/// contiguous arrays for optimal cache locality during type checking.
#[derive(Debug, Default)]
pub struct ParamArena {
    /// All parameter names, indexed by position in the arena.
    /// For functions without named args, this may contain placeholder values.
    names: Vec<Spur>,

    /// All parameter types, indexed by position in the arena.
    types: Vec<Type>,

    /// All parameter modes (Normal, Inout, Borrow, Comptime).
    modes: Vec<RirParamMode>,

    /// Whether each parameter is a comptime parameter.
    comptime: Vec<bool>,
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
            names: Vec::with_capacity(capacity),
            types: Vec::with_capacity(capacity),
            modes: Vec::with_capacity(capacity),
            comptime: Vec::with_capacity(capacity),
        }
    }

    /// Returns the total number of parameters stored in the arena.
    pub fn total_params(&self) -> usize {
        self.types.len()
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
        let start = self.types.len() as u32;

        // Extend all vectors from the iterators
        self.names.extend(names);
        let names_len = self.names.len() - start as usize;

        self.types.extend(types);
        let types_len = self.types.len() - start as usize;

        self.modes.extend(modes);
        let modes_len = self.modes.len() - start as usize;

        self.comptime.extend(comptime);
        let comptime_len = self.comptime.len() - start as usize;

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

    /// Allocates storage for a method's parameters (without comptime flags).
    ///
    /// Methods don't have comptime parameters, so this is a convenience method
    /// that fills the comptime array with `false` values.
    pub fn alloc_method(
        &mut self,
        names: impl IntoIterator<Item = Spur>,
        types: impl IntoIterator<Item = Type>,
    ) -> ParamRange {
        let start = self.types.len() as u32;

        // Collect names and types
        self.names.extend(names);
        let names_len = self.names.len() - start as usize;

        self.types.extend(types);
        let types_len = self.types.len() - start as usize;

        // Verify lengths match
        assert_eq!(
            names_len, types_len,
            "ParamArena::alloc_method: names ({}) and types ({}) have different lengths",
            names_len, types_len
        );

        // Methods use Normal mode and are not comptime by default
        self.modes
            .extend(std::iter::repeat(RirParamMode::Normal).take(types_len));
        self.comptime
            .extend(std::iter::repeat(false).take(types_len));

        ParamRange::new(start, types_len as u32)
    }

    /// Returns the parameter names for a range.
    #[inline]
    pub fn names(&self, range: ParamRange) -> &[Spur] {
        &self.names[range.start()..range.end()]
    }

    /// Returns the parameter types for a range.
    #[inline]
    pub fn types(&self, range: ParamRange) -> &[Type] {
        &self.types[range.start()..range.end()]
    }

    /// Returns the parameter modes for a range.
    #[inline]
    pub fn modes(&self, range: ParamRange) -> &[RirParamMode] {
        &self.modes[range.start()..range.end()]
    }

    /// Returns the comptime flags for a range.
    #[inline]
    pub fn comptime(&self, range: ParamRange) -> &[bool] {
        &self.comptime[range.start()..range.end()]
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
            [Type::I32, Type::Bool, Type::I64],
            [
                RirParamMode::Normal,
                RirParamMode::Inout,
                RirParamMode::Borrow,
            ],
            [false, false, true],
        );

        assert_eq!(range.len(), 3);
        assert_eq!(arena.types(range), &[Type::I32, Type::Bool, Type::I64]);
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
        let range2 = arena.alloc([c], [Type::Bool], [RirParamMode::Inout], [false]);

        // Verify they don't overlap
        assert_eq!(range1.len(), 2);
        assert_eq!(range2.len(), 1);
        assert_eq!(arena.total_params(), 3);

        // Verify data is correct for each range
        assert_eq!(arena.types(range1), &[Type::I32, Type::I32]);
        assert_eq!(arena.types(range2), &[Type::Bool]);
    }

    #[test]
    fn test_alloc_method() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");

        let range = arena.alloc_method([x, y], [Type::I32, Type::Bool]);

        assert_eq!(range.len(), 2);
        assert_eq!(arena.names(range), &[x, y]);
        assert_eq!(arena.types(range), &[Type::I32, Type::Bool]);
        // Methods default to Normal mode and non-comptime
        assert_eq!(
            arena.modes(range),
            &[RirParamMode::Normal, RirParamMode::Normal]
        );
        assert_eq!(arena.comptime(range), &[false, false]);
    }

    #[test]
    fn test_iter() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");

        let range = arena.alloc(
            [x, y],
            [Type::I32, Type::Bool],
            [RirParamMode::Normal, RirParamMode::Inout],
            [false, true],
        );

        let items: Vec<_> = arena.iter(range).collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], (&x, &Type::I32, &RirParamMode::Normal, &false));
        assert_eq!(items[1], (&y, &Type::Bool, &RirParamMode::Inout, &true));
    }

    #[test]
    fn test_name_type_pairs() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");

        let range = arena.alloc(
            [x, y],
            [Type::I32, Type::Bool],
            [RirParamMode::Normal, RirParamMode::Normal],
            [false, false],
        );

        let pairs: Vec<_> = arena.name_type_pairs(range).collect();
        assert_eq!(pairs, vec![(&x, &Type::I32), (&y, &Type::Bool)]);
    }

    #[test]
    fn test_type_mode_pairs() {
        let mut arena = ParamArena::new();
        let mut rodeo = Rodeo::default();

        let x = make_spur(&mut rodeo, "x");
        let y = make_spur(&mut rodeo, "y");

        let range = arena.alloc(
            [x, y],
            [Type::I32, Type::Bool],
            [RirParamMode::Normal, RirParamMode::Inout],
            [false, false],
        );

        let pairs: Vec<_> = arena.type_mode_pairs(range).collect();
        assert_eq!(
            pairs,
            vec![
                (&Type::I32, &RirParamMode::Normal),
                (&Type::Bool, &RirParamMode::Inout)
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
