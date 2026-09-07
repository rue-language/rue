//! Pre-interned known symbols for fast comparison.
//!
//! This module provides `KnownSymbols`, a struct that holds pre-interned `Spur`
//! values for the strings semantic analysis compares against. By interning
//! these strings once at initialization, we can compare symbols directly
//! (integer comparison) instead of resolving to strings and doing string
//! comparison.
//!
//! # Where the spellings come from
//!
//! The intrinsic rows are not listed here. They are interned by walking
//! [`IntrinsicName::ALL`], the one intrinsic table in `rue-builtins`, so this
//! table cannot drift from the one the AIR operation, canonical RIR packing,
//! and comptime classification read. Adding an intrinsic there gives it a
//! symbol here with no edit, and [`KnownSymbols::classify_intrinsic`] is the
//! single entry point that turns a body's symbol back into a typed row.
//!
//! # Performance
//!
//! Each `interner.resolve()` call involves a hash table lookup. While individual
//! lookups are fast, the cumulative cost across many intrinsic dispatches can be
//! significant. Pre-interning known symbols keeps intrinsic dispatch on integer
//! comparisons; classification binary-searches the interned rows.
//!
//! # Usage
//!
//! ```ignore
//! let known = KnownSymbols::new(interner);
//!
//! match known.classify_intrinsic(name) {
//!     Some(IntrinsicName::Dbg) => { /* handle @dbg */ }
//!     Some(IntrinsicName::Cast) => { /* handle @cast */ }
//!     _ => {}
//! }
//! ```

#[cfg(test)]
use lasso::ThreadedRodeo;
use lasso::{Key, Spur};
use rue_builtins::IntrinsicName;
use rue_rir::SharedSymbolSpace;

/// How many intrinsic rows the one table declares.
const INTRINSIC_COUNT: usize = IntrinsicName::ALL.len();

/// Pre-interned symbols for known strings.
///
/// This struct is created once during semantic-analysis setup and provides fast
/// symbol comparison for intrinsic dispatch and other common lookups.
#[derive(Debug, Clone, Copy)]
pub struct KnownSymbols {
    /// One interned spelling per intrinsic row, indexed by the row itself.
    intrinsics: [Spur; INTRINSIC_COUNT],
    /// The same rows ordered by interned key, so classifying a body's symbol
    /// is a binary search rather than a walk of every row.
    by_symbol: [(usize, IntrinsicName); INTRINSIC_COUNT],
    /// The `print` builtin free function - writes a String to stdout (RUE-1).
    /// Builtin free functions are ordinary call names, not intrinsics, so they
    /// are not rows of the intrinsic table.
    pub print: Spur,
    /// The `println` builtin free function - writes a String plus a newline to
    /// stdout (RUE-1).
    pub println: Spur,
}

impl KnownSymbols {
    /// Create a new `KnownSymbols` by interning all known strings.
    ///
    /// This should be called once during semantic-analysis setup.
    #[cfg(test)]
    pub fn new(interner: &ThreadedRodeo) -> Self {
        Self::with_intern(|text| {
            Ok(interner
                .try_get_or_intern_static(text)
                .expect("test known-symbol fixture must fit its private interner"))
        })
        .expect("test known-symbol fixture must fit its private interner")
    }

    /// Build the known-symbol table through the revision-owned interner
    /// policy. Exhaustion is latched by the shared space and reported at the
    /// provider query boundary; no normal-build mutable override is involved.
    pub fn new_in_space(space: &SharedSymbolSpace) -> Result<Self, lasso::LassoErrorKind> {
        Self::with_intern(|text| space.try_intern(text))
    }

    /// The interned spelling of one intrinsic row.
    pub fn intrinsic(&self, name: IntrinsicName) -> Spur {
        self.intrinsics[name as usize]
    }

    /// The intrinsic row a body's symbol names, if it names one at all.
    ///
    /// This is the only classification step between a source spelling and the
    /// typed row every consumer dispatches on; a symbol that is not a row is
    /// the unknown intrinsic semantic analysis reports (E0700).
    pub fn classify_intrinsic(&self, symbol: Spur) -> Option<IntrinsicName> {
        let key = symbol.into_usize();
        let index = self
            .by_symbol
            .binary_search_by_key(&key, |(interned, _)| *interned)
            .ok()?;
        Some(self.by_symbol[index].1)
    }

    fn with_intern(
        mut intern: impl FnMut(&'static str) -> Result<Spur, lasso::LassoErrorKind>,
    ) -> Result<Self, lasso::LassoErrorKind> {
        let mut intrinsics = Vec::with_capacity(INTRINSIC_COUNT);
        for name in IntrinsicName::ALL {
            intrinsics.push(intern(name.spelling())?);
        }
        let intrinsics: [Spur; INTRINSIC_COUNT] = intrinsics
            .try_into()
            .expect("one interned symbol per intrinsic row");

        let mut by_symbol = IntrinsicName::ALL.map(|name| (0usize, name));
        for entry in &mut by_symbol {
            entry.0 = intrinsics[entry.1 as usize].into_usize();
        }
        by_symbol.sort_unstable_by_key(|(interned, _)| *interned);

        Ok(Self {
            intrinsics,
            by_symbol,
            print: intern("print")?,
            println: intern("println")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_intrinsic_row_interns_its_own_spelling() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        for name in IntrinsicName::ALL {
            let symbol = known.intrinsic(name);
            assert_eq!(interner.resolve(&symbol), name.spelling());
            assert_eq!(known.classify_intrinsic(symbol), Some(name));
        }
        assert_eq!(interner.resolve(&known.print), "print");
        assert_eq!(interner.resolve(&known.println), "println");
    }

    #[test]
    fn a_non_intrinsic_symbol_does_not_classify() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        for spelling in ["print", "println", "not_an_intrinsic", "Point"] {
            let symbol = interner.get_or_intern(spelling);
            assert_eq!(
                known.classify_intrinsic(symbol),
                IntrinsicName::from_spelling(spelling),
                "`{spelling}` must classify exactly as the one table says"
            );
        }
    }

    #[test]
    fn known_symbols_comparison() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        // Interning the same string should return the same Spur
        let dbg_sym = interner.get_or_intern("dbg");
        assert_eq!(dbg_sym, known.intrinsic(IntrinsicName::Dbg));
    }

    #[test]
    fn operation_spellings_select_a_table_row() {
        let interner = ThreadedRodeo::new();
        let known = KnownSymbols::new(&interner);

        for operation in crate::IntrinsicOperation::ALL {
            let name = operation.intrinsic_name();
            assert_eq!(operation.expected_spelling(), name.spelling());
            assert_eq!(
                known.classify_intrinsic(known.intrinsic(name)),
                Some(name),
                "{operation:?} must select an interned row"
            );
        }
    }

    #[test]
    fn known_symbols_is_copy() {
        // KnownSymbols should be Copy since it only contains Spur values
        fn assert_copy<T: Copy>() {}
        assert_copy::<KnownSymbols>();
    }
}
