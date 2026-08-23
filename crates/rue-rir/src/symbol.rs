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
//!
//! # Spelling memos
//!
//! Sharing the equality space turned a per-body insertion into a per-body
//! lookup, but a lookup still needs the rendered text, so the names that feed
//! those lookups were still rendered once per body that mentioned them. Two
//! families of them are a total function of data that does not vary across the
//! generation's bodies, so the generation memoizes them:
//!
//! - a name joined from names the space has already interned — `Owner.method`,
//!   `Owner::method` — is a function of the two component handles and the
//!   separator ([`SharedSymbolSpace::derived_symbol`]);
//! - a generated anonymous nominal's name and its destructor's are a function
//!   of the nominal's producer digest
//!   ([`SharedSymbolSpace::keyed_symbol_spelling`],
//!   [`SharedSymbolSpace::keyed_name`]).
//!
//! Only the first body to need a spelling renders it. The memos hold no
//! authority over how a name is spelled: each caller passes its own renderer,
//! which stays the one place that form is spelled (RUE-1236).

use ahash::AHashMap;
use lasso::{Key, Spur, ThreadedRodeo};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

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
    spellings: Arc<Spellings>,
    intern_lock: Arc<Mutex<()>>,
    interner_failure: Arc<Mutex<Option<lasso::LassoErrorKind>>>,
    max_entries: usize,
}

/// How many shards a spelling memo is split across.
///
/// Sharded because every body of a revision reads these memos while the writes
/// are the program's distinct spellings — a few hundred against tens of
/// thousands of reads on the reference workloads — so one lock word would
/// bounce between workers for no reason.
const SPELLING_SHARDS: usize = 16;

/// The two component handles and the join variant a derived spelling is built
/// from.
///
/// The handles are used for equality and hashing only, which is what ADR-0076
/// §2 leaves a handle: the memo is process-local scratch inside one generation,
/// never an order, never a published datum, and never an emitted byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DerivedKey {
    left: Spur,
    right: Spur,
    variant: u8,
}

/// The generation's spelling memos: names joined from interned components, and
/// names that are a total function of a 128-bit identity digest.
#[derive(Debug, Default)]
struct Spellings {
    derived: Sharded<DerivedKey, Spur>,
    /// Digest-keyed names the space also holds a handle for.
    keyed_symbols: Sharded<(u128, u8), (Arc<str>, Spur)>,
    /// Digest-keyed names that are text only. Kept apart from the interned
    /// ones so a name the unmemoized path never interned cannot reach the
    /// interner through a shared entry, which the retention counters read from
    /// the interner would see.
    keyed_names: Sharded<(u128, u8), Arc<str>>,
}

/// A sharded, append-only memo for one generation's spellings.
#[derive(Debug)]
struct Sharded<K, V> {
    shards: [RwLock<AHashMap<K, V>>; SPELLING_SHARDS],
}

impl<K, V> Sharded<K, V>
where
    K: Eq + std::hash::Hash,
    V: Clone,
{
    /// The memoized value for `key`, rendering it with `render` on a miss.
    ///
    /// `shard` is the caller's mixing of the key, because what mixes a key well
    /// depends on the key; it must be a function of `key` alone, or one key
    /// lands in two shards and the memo silently stops sharing.
    ///
    /// `render` runs without the shard lock held, because rendering a spelling
    /// interns it and the interner takes locks of its own. Two workers can
    /// therefore both miss and both render; they render the same text, so the
    /// interner answers both with the same handle and the second write replaces
    /// an equal entry.
    fn memoize(&self, shard: usize, key: K, render: impl FnOnce() -> V) -> V {
        let shard = &self.shards[shard % SPELLING_SHARDS];
        if let Some(value) = shard
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return value.clone();
        }
        let value = render();
        shard
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, value.clone());
        value
    }
}

/// A digest-keyed memo's shard. The digest is already a hash, so its low bits
/// choose the lock; the choice is never an order (ADR-0076 §2).
fn keyed_shard(digest: u128, variant: u8) -> usize {
    (digest as usize).wrapping_add(usize::from(variant))
}

impl<K, V> Default for Sharded<K, V> {
    fn default() -> Self {
        Self {
            shards: std::array::from_fn(|_| RwLock::new(AHashMap::new())),
        }
    }
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
            spellings: Arc::new(Spellings::default()),
            intern_lock: Arc::new(Mutex::new(())),
            interner_failure: Arc::new(Mutex::new(None)),
            max_entries: u32::MAX as usize,
        }
    }

    /// Construct a space with an explicit symbol bound. Production uses the
    /// published `u32` key-space ceiling; tests inject a small bound through
    /// the session-owned space rather than mutating process/thread state.
    #[doc(hidden)]
    pub fn with_owner_bound(max_entries: usize) -> Self {
        Self::with_generation_and_max(1, max_entries)
    }

    fn with_generation_and_max(generation: u64, max_entries: usize) -> Self {
        Self {
            interner: Arc::new(ThreadedRodeo::new()),
            generation,
            live: Arc::new(AtomicBool::new(true)),
            spellings: Arc::new(Spellings::default()),
            intern_lock: Arc::new(Mutex::new(())),
            interner_failure: Arc::new(Mutex::new(None)),
            max_entries,
        }
    }

    /// The shared interner. Handles resolved through it are valid for every
    /// body of this generation.
    #[must_use]
    pub fn interner(&self) -> &Arc<ThreadedRodeo> {
        &self.interner
    }

    /// Fallibly insert a spelling into this generation's shared equality
    /// space. The lock covers the get/check/insert sequence so the bound is
    /// deterministic under concurrent provider workers.
    pub fn try_intern(&self, text: impl AsRef<str>) -> Result<Spur, lasso::LassoErrorKind> {
        let text = text.as_ref();
        if let Some(symbol) = self.interner.get(text) {
            return Ok(symbol);
        }
        let _guard = self
            .intern_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(symbol) = self.interner.get(text) {
            return Ok(symbol);
        }
        if self.interner.len() >= self.max_entries {
            let kind = lasso::LassoErrorKind::KeySpaceExhaustion;
            let mut failure = self
                .interner_failure
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if failure.is_none() {
                *failure = Some(kind);
            }
            return Err(kind);
        }
        match self.interner.try_get_or_intern(text) {
            Ok(symbol) => Ok(symbol),
            Err(error) => {
                let kind = error.kind();
                let mut failure = self
                    .interner_failure
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if failure.is_none() {
                    *failure = Some(kind);
                }
                Err(kind)
            }
        }
    }

    /// The first interner failure observed by this generation, if any.
    pub fn interner_failure(&self) -> Option<lasso::LassoErrorKind> {
        *self
            .interner_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The handle for a spelling joined from two spellings this space already
    /// issued handles for, rendering it at most once per generation.
    ///
    /// `render` is the caller's single spelling authority for the joined form
    /// (RUE-1236); this method decides how often it runs, never how it spells.
    /// `variant` distinguishes joins of the same two components that render
    /// differently — the `.` and `::` member separators.
    ///
    /// The memoized association is exactly the one `get_or_intern(render())`
    /// would produce: within a generation, equal component handles mean equal
    /// component text, so the rendered text and therefore the issued handle are
    /// the same for every caller. A miss renders and interns, so the set of
    /// interned strings — and the retention counters read from it — is what it
    /// would have been without the memo; a hit only skips re-deriving a string
    /// the interner already holds. Racing misses render twice and intern the
    /// same text, which the interner answers with the same handle.
    ///
    /// The memo lives inside the generation, so ADR-0076 §4 governs it
    /// unchanged: it is append-only for the generation's life, it never
    /// outlives it, and a body carrying a retired generation is refused at the
    /// authority check rather than reading a stale association.
    #[cfg(test)]
    pub fn derived_symbol(
        &self,
        left: Spur,
        right: Spur,
        variant: u8,
        render: impl FnOnce() -> String,
    ) -> Spur {
        self.try_derived_symbol(left, right, variant, render)
            .unwrap_or_default()
    }

    /// Fallible form of [`Self::derived_symbol`] used by compiler-owned
    /// materialization. The memo is only published after the interner accepts
    /// the spelling, so an exhaustion cannot masquerade as a valid handle.
    pub fn try_derived_symbol(
        &self,
        left: Spur,
        right: Spur,
        variant: u8,
        render: impl FnOnce() -> String,
    ) -> Result<Spur, lasso::LassoErrorKind> {
        let key = DerivedKey {
            left,
            right,
            variant,
        };
        // The handles' ordinals inside the issuing interner are a dense
        // counter, so mixing the two components spreads consecutive members of
        // one owner across shards. This is a shard choice, never an order
        // (ADR-0076 §2): a different assignment changes only which lock is
        // taken.
        let shard =
            (left.into_usize() ^ (right.into_usize() << 2)).wrapping_add(usize::from(variant));
        if let Some(value) = self.spellings.derived.shards[shard % SPELLING_SHARDS]
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .copied()
        {
            return Ok(value);
        }
        let value = self.try_intern(render())?;
        self.spellings.derived.shards[shard % SPELLING_SHARDS]
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, value);
        Ok(value)
    }

    /// The spelling for a name that is a total function of a 128-bit identity
    /// digest, rendered at most once per generation, with the handle the space
    /// issued for it.
    ///
    /// Generated anonymous nominals are named after their producer digest, so
    /// every body that mints one re-derives a name it shares with every other
    /// body that mints the same nominal — a hundred-fold redundancy on the
    /// reference workloads. `variant` separates names derived from one digest:
    /// the nominal's own spelling and its destructor's.
    ///
    /// `render` is the caller's single spelling authority (RUE-1236) and must
    /// depend on nothing but the digest, which is what makes the association
    /// stable across the generation's bodies. Everything the memo promises
    /// about interning and retirement is [`Self::derived_symbol`]'s promise,
    /// for the same reasons.
    #[cfg(test)]
    pub fn keyed_symbol_spelling(
        &self,
        digest: u128,
        variant: u8,
        render: impl FnOnce() -> String,
    ) -> (Arc<str>, Spur) {
        self.try_keyed_symbol_spelling(digest, variant, render)
            .unwrap_or_else(|_| (Arc::from("<interner-exhausted>"), Spur::default()))
    }

    /// Fallible form of [`Self::keyed_symbol_spelling`].
    pub fn try_keyed_symbol_spelling(
        &self,
        digest: u128,
        variant: u8,
        render: impl FnOnce() -> String,
    ) -> Result<(Arc<str>, Spur), lasso::LassoErrorKind> {
        let key = (digest, variant);
        let shard =
            &self.spellings.keyed_symbols.shards[keyed_shard(digest, variant) % SPELLING_SHARDS];
        if let Some(value) = shard
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned()
        {
            return Ok(value);
        }
        let name: Arc<str> = Arc::from(render().as_str());
        let symbol = self.try_intern(&name)?;
        shard
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, (name.clone(), symbol));
        Ok((name, symbol))
    }

    /// The spelling for a digest-derived name the caller does not intern.
    ///
    /// [`Self::keyed_symbol_spelling`] without the interner call, for a name
    /// that is only ever text — a generated destructor's name, which travels in
    /// the nominal's definition rather than as a symbol. Memoizing it must not
    /// intern it: the retention counters are read from the interner, so a
    /// spelling the unmemoized path never interned must not appear because the
    /// memo found it convenient.
    pub fn keyed_name(
        &self,
        digest: u128,
        variant: u8,
        render: impl FnOnce() -> String,
    ) -> Arc<str> {
        self.spellings
            .keyed_names
            .memoize(keyed_shard(digest, variant), (digest, variant), || {
                Arc::from(render().as_str())
            })
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
        self.next_generation_with_owner_bound(u32::MAX as usize)
    }

    /// Mint a fresh generation with an owner-injected bound. The bound is
    /// normally the published key-space ceiling; tests use this constructor
    /// through the compiler session so the production path has no mutable
    /// process or thread state.
    #[must_use]
    #[doc(hidden)]
    pub fn next_generation_with_owner_bound(&self, max_entries: usize) -> SharedSymbolSpace {
        let generation = self.minted.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        SharedSymbolSpace::with_generation_and_max(generation, max_entries)
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

    #[test]
    fn interner_failure_latch_preserves_the_first_failure_kind() {
        let space = SharedSymbolSpace::with_owner_bound(0);
        *space
            .interner_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(lasso::LassoErrorKind::FailedAllocation);

        assert_eq!(
            space.try_intern("later-key"),
            Err(lasso::LassoErrorKind::KeySpaceExhaustion)
        );
        assert_eq!(
            space.interner_failure(),
            Some(lasso::LassoErrorKind::FailedAllocation),
            "a later key-space failure must not replace the allocator failure"
        );
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

    /// A derived spelling is rendered once per generation and answers with the
    /// handle `get_or_intern` of the same text would have issued, without
    /// interning anything the unmemoized path would not have interned.
    #[test]
    fn a_derived_spelling_is_rendered_once_per_generation() {
        let space = SharedSymbolSpace::private();
        let owner = space.interner().get_or_intern("__anon_struct_00");
        let member = space.interner().get_or_intern("len");
        let interned_before = space.interner().len();

        let renders = std::cell::Cell::new(0_usize);
        let spell = || {
            renders.set(renders.get() + 1);
            "__anon_struct_00.len".to_owned()
        };

        let first = space.derived_symbol(owner, member, 1, spell);
        let second = space.clone().derived_symbol(owner, member, 1, spell);
        assert_eq!(first, second);
        assert_eq!(renders.get(), 1, "the second body reuses the association");
        assert_eq!(
            first,
            space.interner().get_or_intern("__anon_struct_00.len"),
            "the memo answers with the handle the interner issued"
        );
        assert_eq!(
            space.interner().len(),
            interned_before + 1,
            "one spelling was interned, exactly as the unmemoized path would"
        );
    }

    /// The join variant is part of the association, so the two member
    /// separators cannot answer for each other.
    #[test]
    fn derived_spellings_separate_the_join_variants() {
        let space = SharedSymbolSpace::private();
        let owner = space.interner().get_or_intern("Owner");
        let member = space.interner().get_or_intern("make");
        let method = space.derived_symbol(owner, member, 1, || "Owner.make".to_owned());
        let associated = space.derived_symbol(owner, member, 0, || "Owner::make".to_owned());
        assert_ne!(method, associated);
        assert_eq!(space.interner().resolve(&method), "Owner.make");
        assert_eq!(space.interner().resolve(&associated), "Owner::make");
    }

    /// A peer generation memoizes independently: an association never crosses
    /// the generation that minted the handles it is keyed on.
    #[test]
    fn derived_spellings_do_not_cross_generations() {
        let generations = SymbolSpaceGenerations::default();
        let first = generations.next_generation();
        let second = generations.next_generation();
        let owner = first.interner().get_or_intern("Owner");
        let member = first.interner().get_or_intern("len");
        first.derived_symbol(owner, member, 1, || "Owner.len".to_owned());

        let peer_owner = second.interner().get_or_intern("Owner");
        let peer_member = second.interner().get_or_intern("len");
        let renders = std::cell::Cell::new(0_usize);
        second.derived_symbol(peer_owner, peer_member, 1, || {
            renders.set(renders.get() + 1);
            "Owner.len".to_owned()
        });
        assert_eq!(renders.get(), 1, "the peer generation renders for itself");
        assert_eq!(second.interner().len(), 3);
    }

    /// A digest-derived name is rendered once per generation, and the interning
    /// arm interns while the text-only arm does not.
    #[test]
    fn keyed_spellings_render_once_and_intern_only_when_asked() {
        let space = SharedSymbolSpace::private();
        let digest = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef_u128;
        let renders = std::cell::Cell::new(0_usize);
        let spell = || {
            renders.set(renders.get() + 1);
            format!("__anon_struct_{digest:032x}")
        };

        let (name, symbol) = space.keyed_symbol_spelling(digest, 0, spell);
        let (again, same) = space.clone().keyed_symbol_spelling(digest, 0, spell);
        assert_eq!(renders.get(), 1);
        assert_eq!(name, again);
        assert_eq!(symbol, same);
        assert_eq!(space.interner().resolve(&symbol), name.as_ref());
        assert_eq!(space.interner().len(), 1);

        let destructor = space.keyed_name(digest, 1, || format!("{name}.__drop"));
        let destructor_again = space.keyed_name(digest, 1, || unreachable!("memoized"));
        assert_eq!(destructor, destructor_again);
        assert_eq!(destructor.as_ref(), format!("{name}.__drop"));
        assert_eq!(
            space.interner().len(),
            1,
            "a text-only spelling never reaches the interner"
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
