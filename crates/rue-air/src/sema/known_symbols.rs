//! Pre-interned known symbols for fast comparison.
//!
//! This module provides `KnownSymbols`, a struct that holds pre-interned `Spur`
//! values for commonly compared strings like intrinsic names. By interning these
//! strings once at initialization, we can compare symbols directly (integer
//! comparison) instead of resolving to strings and doing string comparison.
//!
//! # Performance
//!
//! Each `interner.resolve()` call involves a hash table lookup. While individual
//! lookups are fast, the cumulative cost across many intrinsic dispatches can be
//! significant. Pre-interning known symbols reduces intrinsic dispatch from
//! O(string_length) to O(1).
//!
//! # Usage
//!
//! ```ignore
//! let known = KnownSymbols::new(interner);
//!
//! // Fast symbol comparison instead of string comparison
//! if name == known.dbg {
//!     // Handle @dbg intrinsic
//! } else if name == known.cast {
//!     // Handle @cast intrinsic
//! }
//! ```

use lasso::{Spur, ThreadedRodeo};

/// Pre-interned symbols for known strings.
///
/// This struct is created once during `SemaContext` construction and provides
/// fast symbol comparison for intrinsic dispatch and other common lookups.
#[derive(Debug, Clone, Copy)]
pub struct KnownSymbols {
    // Intrinsic names
    /// The `dbg` intrinsic symbol.
    pub dbg: Spur,
    /// The `intCast` intrinsic symbol (deprecated, use `cast`).
    pub int_cast: Spur,
    /// The `cast` intrinsic symbol.
    pub cast: Spur,
    /// The `panic` intrinsic symbol.
    pub panic: Spur,
    /// The `assert` intrinsic symbol.
    pub assert: Spur,
    /// The `read_line` intrinsic symbol.
    pub read_line: Spur,
    /// The `parse_i32` intrinsic symbol.
    pub parse_i32: Spur,
    /// The `parse_i64` intrinsic symbol.
    pub parse_i64: Spur,
    /// The `parse_u32` intrinsic symbol.
    pub parse_u32: Spur,
    /// The `parse_u64` intrinsic symbol.
    pub parse_u64: Spur,
    /// The `test_preview_gate` intrinsic symbol.
    pub test_preview_gate: Spur,
    /// The `import` builtin symbol.
    pub import: Spur,
    /// The `random_u32` intrinsic symbol.
    pub random_u32: Spur,
    /// The `random_u64` intrinsic symbol.
    pub random_u64: Spur,

    // Type intrinsics
    /// The `size_of` type intrinsic symbol.
    pub size_of: Spur,
    /// The `align_of` type intrinsic symbol.
    pub align_of: Spur,

    // Builtin type names
    /// The `String` type name symbol.
    pub string_type: Spur,

    // Special function names
    /// The `main` function name symbol.
    pub main_fn: Spur,
}

impl KnownSymbols {
    /// Create a new `KnownSymbols` by interning all known strings.
    ///
    /// This should be called once during `SemaContext` construction.
    pub fn new(interner: &ThreadedRodeo) -> Self {
        Self {
            // Intrinsic names
            dbg: interner.get_or_intern_static("dbg"),
            int_cast: interner.get_or_intern_static("intCast"),
            cast: interner.get_or_intern_static("cast"),
            panic: interner.get_or_intern_static("panic"),
            assert: interner.get_or_intern_static("assert"),
            read_line: interner.get_or_intern_static("read_line"),
            parse_i32: interner.get_or_intern_static("parse_i32"),
            parse_i64: interner.get_or_intern_static("parse_i64"),
            parse_u32: interner.get_or_intern_static("parse_u32"),
            parse_u64: interner.get_or_intern_static("parse_u64"),
            test_preview_gate: interner.get_or_intern_static("test_preview_gate"),
            import: interner.get_or_intern_static("import"),
            random_u32: interner.get_or_intern_static("random_u32"),
            random_u64: interner.get_or_intern_static("random_u64"),

            // Type intrinsics
            size_of: interner.get_or_intern_static("size_of"),
            align_of: interner.get_or_intern_static("align_of"),

            // Builtin type names
            string_type: interner.get_or_intern_static("String"),

            // Special function names
            main_fn: interner.get_or_intern_static("main"),
        }
    }

    /// Check if a symbol matches any of the parse intrinsics.
    ///
    /// Returns the parse intrinsic name as a string if it matches, or None.
    pub fn get_parse_intrinsic_name(&self, sym: Spur) -> Option<&'static str> {
        if sym == self.parse_i32 {
            Some("parse_i32")
        } else if sym == self.parse_i64 {
            Some("parse_i64")
        } else if sym == self.parse_u32 {
            Some("parse_u32")
        } else if sym == self.parse_u64 {
            Some("parse_u64")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_symbols_creation() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        // Verify symbols can be resolved back to their expected strings
        assert_eq!(interner.resolve(&known.dbg), "dbg");
        assert_eq!(interner.resolve(&known.int_cast), "intCast");
        assert_eq!(interner.resolve(&known.cast), "cast");
        assert_eq!(interner.resolve(&known.panic), "panic");
        assert_eq!(interner.resolve(&known.assert), "assert");
        assert_eq!(interner.resolve(&known.read_line), "read_line");
        assert_eq!(interner.resolve(&known.parse_i32), "parse_i32");
        assert_eq!(interner.resolve(&known.parse_i64), "parse_i64");
        assert_eq!(interner.resolve(&known.parse_u32), "parse_u32");
        assert_eq!(interner.resolve(&known.parse_u64), "parse_u64");
        assert_eq!(
            interner.resolve(&known.test_preview_gate),
            "test_preview_gate"
        );
        assert_eq!(interner.resolve(&known.import), "import");
        assert_eq!(interner.resolve(&known.random_u32), "random_u32");
        assert_eq!(interner.resolve(&known.random_u64), "random_u64");
        assert_eq!(interner.resolve(&known.size_of), "size_of");
        assert_eq!(interner.resolve(&known.align_of), "align_of");
        assert_eq!(interner.resolve(&known.string_type), "String");
        assert_eq!(interner.resolve(&known.main_fn), "main");
    }

    #[test]
    fn known_symbols_comparison() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        // Interning the same string should return the same Spur
        let dbg_sym = interner.get_or_intern("dbg");
        assert_eq!(dbg_sym, known.dbg);

        let main_sym = interner.get_or_intern("main");
        assert_eq!(main_sym, known.main_fn);
    }

    #[test]
    fn get_parse_intrinsic_name() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        assert_eq!(
            known.get_parse_intrinsic_name(known.parse_i32),
            Some("parse_i32")
        );
        assert_eq!(
            known.get_parse_intrinsic_name(known.parse_i64),
            Some("parse_i64")
        );
        assert_eq!(
            known.get_parse_intrinsic_name(known.parse_u32),
            Some("parse_u32")
        );
        assert_eq!(
            known.get_parse_intrinsic_name(known.parse_u64),
            Some("parse_u64")
        );
        assert_eq!(known.get_parse_intrinsic_name(known.dbg), None);
    }

    #[test]
    fn known_symbols_is_copy() {
        // KnownSymbols should be Copy since it only contains Spur values
        fn assert_copy<T: Copy>() {}
        assert_copy::<KnownSymbols>();
    }
}
