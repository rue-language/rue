//! The equality-only handle into the shared symbol interner, and the
//! revision-scoped space that issues it (ADR-0076).
//!
//! A [`lasso::Spur`] is an index into whatever interner issued it, assigned in
//! first-intern order. [`SharedSymbolSpace`] holds one append-only interner per
//! revision generation of the semantic engine, shared across every body of that
//! generation, so first-intern order is a function of worker scheduling and a
//! handle's numeric value is not reproducible between runs.
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
//! The escape hatch is [`SymbolHandle::issuing_interner_ordinal`], named so
//! that every remaining value-bearing use is one grep away and each one has to
//! say which interner's index space it is speaking about.
//!
//! # The two spaces
//!
//! ADR-0076 §1 splits what used to be one per-body interner in two:
//!
//! - the **shared equality space**, this module's [`SharedSymbolSpace`]. Strings
//!   are interned once per revision generation; handles are equality-only and
//!   cross bodies freely within their generation.
//! - the **body-private dense encoding space**, which is the packed RIR symbol
//!   section's ordinals. Those ordinals *are* the encoding, so they stay dense
//!   and body-local; a body decodes them into shared handles through a dense
//!   remap built once per materialization. The dense space holds that remap
//!   only — it never re-interns the strings.

use lasso::{Key, Spur, ThreadedRodeo};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
    /// published payload, or a deterministic counter, because under a shared
    /// revision interner the value is assigned in worker-scheduling order.
    /// It is legitimate only where the value round-trips inside one artifact
    /// against the very interner that issued it, and every caller owes an
    /// explanation of which interner it means.
    #[must_use]
    pub fn issuing_interner_ordinal(self) -> usize {
        self.0.into_usize()
    }
}

/// One append-only symbol interner, branded with the revision generation that
/// minted it (ADR-0076 §1 and §4).
///
/// Every body of a generation shares one interner, so a string is interned once
/// per revision instead of once per body. The space is append-only within its
/// generation; when the owner retires it with [`Self::supersede`], every clone
/// reports [`Self::is_live`] as `false` at once, and a body still holding it
/// fails its RIR authority check rather than being silently reused. The
/// interner itself stays alive for as long as some holder needs to resolve the
/// handles it already issued — refusal, not a dangling handle, is the
/// fail-closed behavior.
#[derive(Debug, Clone)]
pub struct SharedSymbolSpace {
    interner: Arc<ThreadedRodeo>,
    generation: u64,
    live: Arc<AtomicBool>,
}

impl SharedSymbolSpace {
    /// A space with exactly one generation that nothing else can retire, for
    /// callers that own a single body: fixtures, tests, and the compatibility
    /// identity contexts that never cross a revision boundary.
    #[must_use]
    pub fn private() -> Self {
        SymbolSpaceGenerations::default().next_generation()
    }

    /// Adopt an already-populated interner as a private, single-generation
    /// space. For harnesses that lex and parse a single body and then analyze
    /// it against the parse interner; the revision-shared space is minted by
    /// [`SymbolSpaceGenerations`] instead.
    #[must_use]
    pub fn adopt(interner: Arc<ThreadedRodeo>) -> Self {
        Self {
            interner,
            generation: 1,
            live: Arc::new(AtomicBool::new(true)),
        }
    }

    /// The shared interner. Handles resolved through it are valid for every
    /// body of this generation.
    #[must_use]
    pub fn interner(&self) -> &Arc<ThreadedRodeo> {
        &self.interner
    }

    /// Whether this generation is still current. A retired space fails closed
    /// at the authority check.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    /// Retire this generation for every holder of it.
    ///
    /// Called by the owner when the revision it belongs to stops being served.
    /// Retirement is one-way: a generation is never revived, because a body
    /// that observed it as retired has already been abandoned.
    pub fn supersede(&self) {
        self.live.store(false, Ordering::Release);
    }

    /// The generation ordinal, for diagnostics only. It counts mints within one
    /// owner; it is never a symbol value and never an emitted datum.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// The mint for [`SharedSymbolSpace`] generations.
///
/// Minting does not retire anything: an owner serving several revisions at once
/// keeps a generation per revision and retires each one explicitly when it stops
/// serving that revision. Retiring on every mint would let two concurrently
/// pinned revisions retire each other's space and abandon each other's bodies
/// indefinitely.
#[derive(Debug, Default)]
pub struct SymbolSpaceGenerations {
    minted: AtomicU64,
}

impl SymbolSpaceGenerations {
    /// Mint a fresh append-only interner as the next generation.
    #[must_use]
    pub fn next_generation(&self) -> SharedSymbolSpace {
        SharedSymbolSpace {
            interner: Arc::new(ThreadedRodeo::new()),
            generation: self.minted.fetch_add(1, Ordering::AcqRel).wrapping_add(1),
            live: Arc::new(AtomicBool::new(true)),
        }
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
    use super::{SharedSymbolSpace, SymbolHandle, SymbolSpaceGenerations};

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

    /// The equality space is shared: two bodies of one generation see the same
    /// handle for the same string, which is the redundancy ADR-0076 removes.
    #[test]
    fn one_generation_interns_a_string_once() {
        let generations = SymbolSpaceGenerations::default();
        let space = generations.next_generation();
        let first_body = space.clone();
        let second_body = space.clone();
        assert_eq!(
            first_body
                .interner()
                .get_or_intern("__anon_struct_00.member"),
            second_body
                .interner()
                .get_or_intern("__anon_struct_00.member"),
        );
        assert_eq!(space.interner().len(), 1);
    }

    /// Retiring a generation retires every outstanding holder of it at once
    /// (ADR-0076 §4): the superseded space is not live, so the authority check
    /// that consults it fails closed. Minting a peer generation does not retire
    /// anything, so an owner may serve two revisions at once.
    #[test]
    fn a_superseded_generation_is_not_live() {
        let generations = SymbolSpaceGenerations::default();
        let first = generations.next_generation();
        let holder = first.clone();
        let second = generations.next_generation();
        assert!(first.is_live(), "a peer mint does not retire a generation");
        assert!(second.is_live());
        assert_ne!(first.generation(), second.generation());

        first.supersede();
        assert!(!first.is_live());
        assert!(!holder.is_live(), "every clone observes the retirement");
        assert!(second.is_live());
        assert_eq!(
            holder.interner().len(),
            0,
            "a retired space still resolves the handles it issued"
        );
    }

    /// A private space has no owner that could retire it.
    #[test]
    fn a_private_space_stays_live() {
        let space = SharedSymbolSpace::private();
        assert!(space.is_live());
        let peer = SharedSymbolSpace::private();
        assert!(space.is_live());
        assert!(peer.is_live());
    }
}
