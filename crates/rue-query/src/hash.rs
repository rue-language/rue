//! Stable hashing and durable node/key identity (ADR-0074).

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock, Weak};

use crate::*;

#[cfg(test)]
use std::cell::Cell;

/// Compile-time keys of the stable key digest (ADR-0074).
///
/// These are constants, never a per-process seed. Two runs of the same
/// compiler over the same inputs must derive identical digests, because the
/// digest orders published dependencies and denominates the retained charge:
/// a seeded hasher would make both artifacts differ per process.
pub(crate) const STABLE_HASH_LOW_KEY: u64 = 0x243F_6A88_85A3_08D3;
pub(crate) const STABLE_HASH_HIGH_KEY: u64 = 0x1319_8A2E_0370_7344;

/// Content-derived 128-bit digest of one typed query key (ADR-0074).
///
/// Ordering is the ordering of the digest read as one 128-bit integer, high
/// half most significant, so `(family, stable_hash)` is a deterministic total
/// preorder over nodes that never touches presentation text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableKeyHash {
    high: u64,
    low: u64,
}

impl StableKeyHash {
    /// The digest as one 128-bit integer, high half most significant.
    pub const fn to_u128(self) -> u128 {
        ((self.high as u128) << 64) | self.low as u128
    }
}

/// Bijective 64-bit finalizer (the SplitMix64 mixer).
#[inline]
pub(crate) const fn stable_hash_mix(mut word: u64) -> u64 {
    word ^= word >> 30;
    word = word.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    word ^= word >> 27;
    word = word.wrapping_mul(0x94D0_49BB_1331_11EB);
    word ^ (word >> 31)
}

/// The fixed hasher behind [`StableKeyHash`].
///
/// It is an ordinary [`Hasher`], so a key's [`QueryKey::stable_hash`] absorbs
/// its typed fields with the same `Hash` calls a derive would emit. Integers
/// are absorbed little-endian rather than through `to_ne_bytes`, so the digest
/// does not depend on the host's byte order either.
///
/// The hasher can also *record* the byte stream a key feeds it, in field
/// order. That recording is the structural collision witness: it comes from
/// the same typed fields as the digest, so two keys whose 128-bit digests
/// collide are still separated deterministically without formatting either.
#[derive(Debug, Clone)]
pub struct StableHasher {
    low: u64,
    high: u64,
    absorbed: u64,
    witness: Option<Vec<u8>>,
}

impl Default for StableHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl StableHasher {
    /// Creates a hasher keyed with the fixed compile-time constants.
    pub const fn new() -> Self {
        Self {
            low: STABLE_HASH_LOW_KEY,
            high: STABLE_HASH_HIGH_KEY,
            absorbed: 0,
            witness: None,
        }
    }

    /// Creates a hasher that also records the byte stream it is fed.
    pub fn recording() -> Self {
        Self {
            witness: Some(Vec::new()),
            ..Self::new()
        }
    }

    /// The recorded byte stream, for a hasher built by [`Self::recording`].
    pub fn into_witness(self) -> Vec<u8> {
        self.witness
            .expect("only a recording hasher yields a witness")
    }

    /// Records one field's bytes in the witness, in the order they were fed.
    #[inline]
    fn note(&mut self, bytes: &[u8]) {
        if let Some(witness) = &mut self.witness {
            witness.extend_from_slice(bytes);
        }
    }

    #[inline]
    fn absorb(&mut self, word: u64) {
        self.absorbed = self.absorbed.wrapping_add(1);
        // Two lanes injecting the same word differently: a collision has to
        // survive both, which is what makes the 128-bit pair meaningful.
        self.low = stable_hash_mix(self.low ^ word);
        self.high =
            stable_hash_mix(self.high.rotate_left(31) ^ word.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    }

    /// The complete 128-bit digest of everything absorbed so far.
    pub fn finish128(&self) -> StableKeyHash {
        #[cfg(test)]
        if FORCED_STABLE_HASH_COLLISION.with(Cell::get) {
            // Test-only hasher override: every key of every family digests
            // alike, so the collision path carries the whole runtime.
            return StableKeyHash { high: 0, low: 0 };
        }
        let low = stable_hash_mix(self.low ^ self.absorbed);
        let high = stable_hash_mix(self.high ^ low ^ self.absorbed.rotate_left(32));
        StableKeyHash { high, low }
    }
}

#[cfg(test)]
thread_local! {
    /// Collapses every digest this thread computes to one value, so a test can
    /// drive the collision path without weakening any key's own field hashing.
    /// It is thread-local rather than global so it cannot leak into the other
    /// tests sharing this process.
    static FORCED_STABLE_HASH_COLLISION: Cell<bool> = const { Cell::new(false) };
}

/// Forces [`StableKeyHash`] collisions on this thread for the guard's lifetime.
#[cfg(test)]
pub(crate) struct ForcedStableHashCollision;

#[cfg(test)]
impl ForcedStableHashCollision {
    pub(crate) fn enter() -> Self {
        FORCED_STABLE_HASH_COLLISION.with(|forced| forced.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for ForcedStableHashCollision {
    fn drop(&mut self) {
        FORCED_STABLE_HASH_COLLISION.with(|forced| forced.set(false));
    }
}

impl Hasher for StableHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.note(bytes);
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let word = u64::from_le_bytes(chunk.try_into().expect("an exact eight-byte chunk"));
            self.absorb(word);
        }
        let remainder = chunks.remainder();
        let mut tail = [0_u8; 8];
        tail[..remainder.len()].copy_from_slice(remainder);
        // The high byte of a short tail is always zero, so folding the length
        // in there separates `[1]` from `[1, 0]` without losing a byte.
        self.absorb(u64::from_le_bytes(tail) ^ ((remainder.len() as u64) << 56));
    }

    fn write_u8(&mut self, value: u8) {
        self.note(&value.to_le_bytes());
        self.absorb(value as u64);
    }

    fn write_u16(&mut self, value: u16) {
        self.note(&value.to_le_bytes());
        self.absorb(value as u64);
    }

    fn write_u32(&mut self, value: u32) {
        self.note(&value.to_le_bytes());
        self.absorb(value as u64);
    }

    fn write_u64(&mut self, value: u64) {
        self.note(&value.to_le_bytes());
        self.absorb(value);
    }

    fn write_u128(&mut self, value: u128) {
        self.note(&value.to_le_bytes());
        self.absorb(value as u64);
        self.absorb((value >> 64) as u64);
    }

    fn write_usize(&mut self, value: usize) {
        self.note(&(value as u64).to_le_bytes());
        self.absorb(value as u64);
    }

    fn write_i8(&mut self, value: i8) {
        self.write_u8(value as u8);
    }

    fn write_i16(&mut self, value: i16) {
        self.write_u16(value as u16);
    }

    fn write_i32(&mut self, value: i32) {
        self.write_u32(value as u32);
    }

    fn write_i64(&mut self, value: i64) {
        self.write_u64(value as u64);
    }

    fn write_i128(&mut self, value: i128) {
        self.write_u128(value as u128);
    }

    fn write_isize(&mut self, value: isize) {
        self.write_usize(value as usize);
    }

    fn finish(&self) -> u64 {
        self.finish128().low
    }
}

/// The complete content-derived digest of one typed key.
pub fn stable_key_hash<K: QueryKey>(key: &K) -> StableKeyHash {
    let mut hasher = StableHasher::new();
    key.stable_hash(&mut hasher);
    hasher.finish128()
}

/// The structural collision witness of one typed key.
///
/// This is the canonical byte stream [`QueryKey::stable_hash`] feeds, in field
/// order: the same typed fields [`stable_key_hash`] digests, and nothing else.
/// Two keys that collide in 128 bits are ordered and compared by this stream,
/// so a collision never consults presentation text — which is explicitly
/// allowed to collide for unequal keys and therefore could not decide identity.
///
/// A key whose `stable_hash` absorbs a strict subset of the fields its `Eq`
/// compares makes those variants structurally indistinguishable: they share a
/// digest and a witness and so compare equal here, exactly as they already
/// shared one display identity. Typed `Eq` remains authoritative for memo
/// lookup, and the canonical publication order breaks the remaining tie on the
/// node incarnation.
pub fn stable_key_witness<K: QueryKey>(key: &K) -> Vec<u8> {
    let mut hasher = StableHasher::recording();
    key.stable_hash(&mut hasher);
    hasher.into_witness()
}

/// A logical key suitable for a retained query family.
///
/// `Hash` must agree with `Eq`: keys that compare equal must hash equal. The
/// memo map is keyed by the typed key itself, so hash collisions never conflate
/// distinct keys — they are resolved by exact `Self::eq`. Implementors that
/// embed `Arc<[T]>` or map/set payloads must derive or write `Hash`
/// consistently with their `Eq`.
pub trait QueryKey: Clone + Eq + Hash + Send + Sync + 'static {
    /// A deterministic user-visible identity within the family.
    ///
    /// This text is presentation only and may collide. Exact `Self::eq`
    /// remains authoritative for memo-node lookup, and since ADR-0074 no
    /// runtime contract reads this text on the ordinary path: it is formatted
    /// on first diagnostic, cycle render, abort, or `Debug` need.
    fn stable_identity(&self) -> String;

    /// A shareable form of the presentation identity.
    ///
    /// Keys which flow unchanged through several query families may override
    /// this method to retain and format the identity once. An override must
    /// return the same text as [`Self::stable_identity`]. The default keeps
    /// simple keys allocation-equivalent to that method.
    fn shared_stable_identity(&self) -> Arc<str> {
        self.stable_identity().into()
    }

    /// Absorbs this key's typed fields into the stable digest (ADR-0074).
    ///
    /// The digest is structural: it orders published dependencies and
    /// denominates the retained charge, so it must be derived from the key's
    /// own fields, exactly as a `Hash` derive would enumerate them. Never
    /// absorb [`Self::stable_identity`]'s text, an address, an allocation
    /// order, or anything a schedule can change.
    ///
    /// Equal keys must produce equal digests, so an implementation may absorb
    /// a strict subset of the fields `Eq` compares — a deliberately coarse
    /// digest costs a cold collision tiebreak, never correctness. Absorbing a
    /// field `Eq` ignores is a bug.
    fn stable_hash(&self, hasher: &mut StableHasher);
}

/// Collision-free identity of one immutable input leaf.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputIdentity {
    family: Arc<str>,
    key: Arc<str>,
}

impl InputIdentity {
    /// Creates a family/key input identity.
    pub fn new(family: impl Into<Arc<str>>, key: impl Into<Arc<str>>) -> Self {
        Self {
            family: family.into(),
            key: key.into(),
        }
    }

    /// Stable input-family name.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Stable key within the input family.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Exact value stamp of one leaf in an immutable input revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputObservation {
    /// Collision-free input identity.
    pub input: InputIdentity,
    /// Family-owned exact value stamp. This is not a memo-node identity.
    pub stamp: u64,
}

/// An immutable input revision pinned by one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision {
    pub(crate) id: u64,
    pub(crate) compatibility: u64,
}

impl Revision {
    /// Creates a revision. Equal compatibility tokens assert equivalent inputs.
    pub const fn new(id: u64, compatibility: u64) -> Self {
        Self { id, compatibility }
    }

    /// The immutable publication identity.
    pub const fn id(self) -> u64 {
        self.id
    }

    /// Returns whether retained work may be validated across two revisions.
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.compatibility == other.compatibility
    }
}

/// The typed key behind one node identity.
///
/// The source owns the key, so an identity can still name itself and still
/// answer a structural comparison after its node has been evicted — a retained
/// observation outlives the node it names, and a later cycle or diagnostic
/// must still render it.
pub(crate) trait TypedKeyView: Send + Sync {
    /// Presentation text, formatted here and nowhere else.
    fn format(&self) -> Arc<str>;

    /// The structural collision witness of the typed key.
    fn witness(&self) -> Vec<u8>;
}

pub(crate) struct TypedKeySource<K> {
    pub(crate) key: K,
    /// Live runtime to attribute a memo-node materialization to. A cold
    /// identity the runtime built precisely because it already needed a name
    /// carries a never-upgradable handle: its formatting is counted by the
    /// abort-fallback or structured-wait counter at the call site instead.
    pub(crate) core: Weak<RuntimeCore>,
}

impl<K: QueryKey> TypedKeyView for TypedKeySource<K> {
    fn format(&self) -> Arc<str> {
        let text = self.key.shared_stable_identity();
        if let Some(core) = self.core.upgrade() {
            core.metrics.record_memo_node_identity(text.len());
        }
        text
    }

    fn witness(&self) -> Vec<u8> {
        stable_key_witness(&self.key)
    }
}

/// A key that is nothing but its own presentation text.
///
/// Tests build display-only identities from a name; every runtime caller has
/// a typed key and reaches `NodeIdentity::from_key` instead.
#[cfg(test)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct TextKey(pub(crate) Arc<str>);

#[cfg(test)]
impl QueryKey for TextKey {
    fn stable_identity(&self) -> String {
        self.0.to_string()
    }

    fn shared_stable_identity(&self) -> Arc<str> {
        self.0.clone()
    }

    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.0.hash(hasher);
    }
}

/// Canonical structural identity of one logical memo node (ADR-0074).
///
/// The identity is the pair `(family, stable_hash)`: a shared family name plus
/// the content-derived 128-bit digest of the typed key. Equality, ordering,
/// and hashing are integer comparisons over that pair. When two identities of
/// one family share a digest, they are separated by the *structural collision
/// witness* — the canonical byte stream the typed key feeds into that digest.
///
/// Presentation text is never consulted for identity. `stable_identity()` is
/// explicitly allowed to collide for unequal keys, so it could not decide one:
/// it is lazily formatted and used only to name a node in a diagnostic, a
/// rendered cycle, an abort, or a `Debug` dump.
///
/// The identity is shared by the node, its terminals, and every dependency
/// observation. Runtime-created identities also carry a weak, non-owning route
/// back to the exact erased node.
#[derive(Clone)]
pub struct NodeIdentity {
    pub(crate) inner: Arc<NodeIdentityData>,
}

pub(crate) struct NodeIdentityData {
    pub(crate) family: Arc<str>,
    stable_hash: StableKeyHash,
    /// Presentation text. Preformatted for the cold identities the runtime
    /// builds only when it already needs a name; lazily filled from `key` for
    /// memo-node identities.
    pub(crate) text: OnceLock<Arc<str>>,
    key: Arc<dyn TypedKeyView>,
    runtime_identity: Option<u64>,
    node: Option<Weak<dyn ErasedNode>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ExactNodeIdentity {
    pub(crate) display: NodeIdentity,
    pub(crate) incarnation: u64,
}

impl NodeIdentity {
    /// Construct a collision-safe identity for a typed key without requiring
    /// a live registered family node. This is used when reconciling retained
    /// query publications with nested observations.
    pub fn from_typed_key<K: QueryKey>(family: impl Into<Arc<str>>, key: &K) -> Self {
        Self::from_key(family.into(), key)
    }

    /// A display-only identity whose text is already known.
    ///
    /// The digest comes from the text because for this identity the text *is*
    /// the key. Every runtime caller reaches it through [`Self::from_key`],
    /// which digests the typed key instead.
    #[cfg(test)]
    pub(crate) fn new(family: Arc<str>, key: Arc<str>) -> Self {
        Self::from_key(family, &TextKey(key))
    }

    /// A display-only identity for a typed key the caller already has to name.
    ///
    /// Used by the cold paths that exist only to render a name — an aborted
    /// nested request and a structured wait edge on a rendered cycle — so the
    /// text is formatted eagerly here and the lazy slot starts filled.
    pub(crate) fn from_key<K: QueryKey>(family: Arc<str>, key: &K) -> Self {
        let text = OnceLock::new();
        let _ = text.set(key.shared_stable_identity());
        Self {
            inner: Arc::new(NodeIdentityData {
                family,
                stable_hash: stable_key_hash(key),
                text,
                key: Arc::new(TypedKeySource {
                    key: key.clone(),
                    core: Weak::new(),
                }),
                runtime_identity: None,
                node: None,
            }),
        }
    }

    /// The identity of one live memo-node incarnation.
    ///
    /// The digest is computed here from the typed key; the presentation text
    /// is not, and is formatted only if something later asks for a name.
    pub(crate) fn registered<K: QueryKey>(
        family: Arc<str>,
        key: Arc<TypedKeySource<K>>,
        stable_hash: StableKeyHash,
        runtime_identity: u64,
        node: Weak<dyn ErasedNode>,
    ) -> Self {
        Self {
            inner: Arc::new(NodeIdentityData {
                family,
                stable_hash,
                text: OnceLock::new(),
                key,
                runtime_identity: Some(runtime_identity),
                node: Some(node),
            }),
        }
    }

    pub(crate) fn registered_node(
        &self,
        runtime_identity: u64,
        incarnation: u64,
    ) -> Option<Arc<dyn ErasedNode>> {
        if self.inner.runtime_identity != Some(runtime_identity) {
            return None;
        }
        let node = self.inner.node.as_ref()?.upgrade()?;
        (node.incarnation() == incarnation).then_some(node)
    }

    /// Stable family name.
    pub fn family(&self) -> &str {
        &self.inner.family
    }

    /// The content-derived digest of this node's typed key (ADR-0074).
    ///
    /// This is the structural half of the identity: ordering, equality, and
    /// hashing are defined on `(family, stable_hash)`, and it is what callers
    /// should key their own per-node tables by instead of the text.
    pub fn stable_hash(&self) -> StableKeyHash {
        self.inner.stable_hash
    }

    /// Family-defined presentation key, formatted on first demand.
    ///
    /// This is the ADR-0074 cold path. Calling it on a memo-node identity
    /// formats the typed key once and counts one
    /// `display_identities.memo_node_materializations`. This text is
    /// presentation only and may be equal for unequal keys.
    pub fn key(&self) -> &str {
        if let Some(text) = self.inner.text.get() {
            return text;
        }
        self.inner.text.get_or_init(|| self.inner.key.format())
    }

    /// Compares `(family, stable_hash)`, which never touches presentation text.
    fn structural_cmp(&self, other: &Self) -> std::cmp::Ordering {
        let family_order = if Arc::ptr_eq(&self.inner.family, &other.inner.family) {
            std::cmp::Ordering::Equal
        } else {
            self.family().cmp(other.family())
        };
        family_order.then_with(|| self.inner.stable_hash.cmp(&other.inner.stable_hash))
    }

    /// Breaks a 128-bit digest collision on the typed keys' own content.
    ///
    /// This is the cold path: it is reached only when two identities of one
    /// family share a digest, which for content-derived digests means either
    /// the same key or a genuine collision. It allocates each stream on
    /// demand rather than retaining one per node, because the ordinary path
    /// answers on the integer pair above and never gets here.
    fn collision_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.inner.key.witness().cmp(&other.inner.key.witness())
    }
}

impl fmt::Debug for NodeIdentity {
    /// `Debug` names the node, so it deliberately materializes the text.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NodeIdentity")
            .field("family", &self.inner.family)
            .field("key", &self.key())
            .finish()
    }
}

impl PartialEq for NodeIdentity {
    /// Distinct identities never compare equal. `(family, stable_hash)` decides
    /// every ordinary answer; two distinct keys of one family that collide in
    /// 128 bits are separated by the structural collision witness.
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.inner, &other.inner) {
            return true;
        }
        self.structural_cmp(other) == std::cmp::Ordering::Equal
            && self.collision_cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for NodeIdentity {}

impl PartialOrd for NodeIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NodeIdentity {
    /// `(family, stable_hash, structural_collision_witness)`. The witness is
    /// absent from the fast path: it is computed only when the digests tie.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if Arc::ptr_eq(&self.inner, &other.inner) {
            return std::cmp::Ordering::Equal;
        }
        self.structural_cmp(other)
            .then_with(|| self.collision_cmp(other))
    }
}

impl Hash for NodeIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.family().hash(state);
        self.inner.stable_hash.hash(state);
    }
}
