//! The equality-only handle into a body's symbol interner (ADR-0076).
//!
//! A [`lasso::Spur`] is an index into whatever interner issued it, assigned in
//! first-intern order. Today every body owns a private `ThreadedRodeo`, so that
//! order is a deterministic function of the body's own traversal. ADR-0076
//! shares one append-only interner across the bodies of a revision, at which
//! point first-intern order becomes a function of worker scheduling and a
//! handle's numeric value stops being reproducible.
//!
//! [`SymbolHandle`] is the type that states that contract in the type system.
//! It wraps a `Spur` and deliberately offers less than one:
//!
//! - no `Ord`/`PartialOrd`, so it cannot be a sort key, a `BTreeMap`/`BTreeSet`
//!   key, or an operand of a comparison;
//! - no `lasso::Key` implementation, so `into_usize()` is not in scope and a
//!   handle's numeric value cannot silently become a name, a word in a
//!   published payload, or the seed of a deterministic counter.
//!
//! What remains is equality, hashing, and resolution against the interner that
//! issued the handle. Anything that needs an order takes it from the symbol's
//! text or from a stable identity (ADR-0074's structural hashes).
//!
//! The escape hatch is [`SymbolHandle::body_local_ordinal`], named so that
//! every remaining value-bearing use is one grep away and each one has to say
//! which body-local dense space it is speaking about.

use lasso::{Key, Spur};

/// An equality-only handle into one body's symbol interner.
///
/// See the module documentation for the ADR-0076 contract this type carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct SymbolHandle(Spur);

impl SymbolHandle {
    /// Adopt an interner-issued handle.
    #[must_use]
    pub const fn new(spur: Spur) -> Self {
        Self(spur)
    }

    /// The underlying interner key, for the interner calls that need one
    /// (`resolve`, `try_resolve`) and for the code that has not migrated yet.
    #[must_use]
    pub const fn spur(self) -> Spur {
        self.0
    }

    /// This handle's numeric value inside the interner that issued it.
    ///
    /// ADR-0076 forbids letting that value reach an emitted artifact, a
    /// published payload, or a deterministic counter. It is legitimate only
    /// where the issuing interner is a body-private dense table whose ordinals
    /// are the encoding — the packed RIR symbol section is the one such space
    /// in the compiler today — and every caller owes an explanation of which
    /// dense space it means.
    #[must_use]
    pub fn body_local_ordinal(self) -> usize {
        self.0.into_usize()
    }
}

impl From<Spur> for SymbolHandle {
    fn from(spur: Spur) -> Self {
        Self::new(spur)
    }
}

impl From<SymbolHandle> for Spur {
    fn from(handle: SymbolHandle) -> Self {
        handle.spur()
    }
}

#[cfg(test)]
mod tests {
    use super::SymbolHandle;

    /// The contract is the *absence* of ordering, which no runtime assertion can
    /// observe. This test states it as a trait-bound witness instead: a handle
    /// satisfies the traits equality-only use needs, and the negative half is
    /// enforced by the compiler at every would-be `sort`/`BTreeMap` site.
    #[test]
    fn handle_is_equality_only() {
        fn assert_equality_only<T: Copy + Eq + std::hash::Hash>() {}
        assert_equality_only::<SymbolHandle>();

        let interner = lasso::ThreadedRodeo::new();
        let first = SymbolHandle::new(interner.get_or_intern("alpha"));
        let second = SymbolHandle::new(interner.get_or_intern("beta"));
        assert_ne!(first, second);
        assert_eq!(first, SymbolHandle::new(interner.get_or_intern("alpha")));
        assert_eq!(interner.resolve(&first.spur()), "alpha");
    }
}
