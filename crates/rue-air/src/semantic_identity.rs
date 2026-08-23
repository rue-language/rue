//! Canonical semantic identities shared by live bindings and durable keys.

use std::sync::Arc;

use rue_runtime_abi::{ReservedExportId, RuntimeHelperId};

/// Fixed-key mixer for identity digests.
///
/// The SplitMix64 finalizer `rue_query::StableHasher` uses, kept here so a
/// digest can be taken without the semantic IR depending on the query engine.
/// Fixed-key is the whole point: these digests select the order of maps whose
/// iteration the compiler observes, so a per-process seed would make the
/// compiler non-deterministic rather than subtly wrong.
///
/// One 64-bit lane rather than `StableHasher`'s two. The digest here is only
/// ever an accelerator: [`Node`] verifies equality structurally and breaks an
/// ordering tie structurally, so a collision costs one slow comparison and can
/// never produce a wrong answer. Over the ~274k nodes a fresh Lattice compile
/// builds, a 64-bit space makes that slow path astronomically rare, and the
/// second lane would double the cost of the one operation this whole change
/// exists to make cheap.
#[derive(Debug, Clone)]
struct IdentityHasher {
    state: u64,
}

impl IdentityHasher {
    const KEY: u64 = 0x2545_F491_4F6C_DD1D;

    const fn new() -> Self {
        Self { state: Self::KEY }
    }

    /// Bijective 64-bit finalizer (the SplitMix64 mixer).
    #[inline]
    const fn mix(mut word: u64) -> u64 {
        word ^= word >> 30;
        word = word.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        word ^= word >> 27;
        word = word.wrapping_mul(0x94D0_49BB_1331_11EB);
        word ^ (word >> 31)
    }

    #[inline]
    fn absorb(&mut self, word: u64) {
        self.state = Self::mix(self.state ^ word);
    }

    fn digest(&self) -> u64 {
        self.state
    }
}

impl std::hash::Hasher for IdentityHasher {
    fn write(&mut self, bytes: &[u8]) {
        // Whole words first, then one tail word carrying the remainder's
        // length in its top byte so a short tail cannot alias a longer one.
        // An exact multiple of eight still absorbs that word, which is what
        // separates `b"abcdefgh"` from `b"abcdefgh\0"`.
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.absorb(u64::from_le_bytes(chunk.try_into().expect("eight bytes")));
        }
        let remainder = chunks.remainder();
        let mut tail = [0u8; 8];
        tail[..remainder.len()].copy_from_slice(remainder);
        self.absorb(u64::from_le_bytes(tail) ^ ((remainder.len() as u64) << 56));
    }

    // Integers are absorbed little-endian rather than through `to_ne_bytes`,
    // so a digest does not depend on the host's byte order.
    fn write_u8(&mut self, value: u8) {
        self.absorb(u64::from(value));
    }

    fn write_u16(&mut self, value: u16) {
        self.absorb(u64::from(value));
    }

    fn write_u32(&mut self, value: u32) {
        self.absorb(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.absorb(value);
    }

    fn write_u128(&mut self, value: u128) {
        self.absorb(value as u64);
        self.absorb((value >> 64) as u64);
    }

    fn write_i128(&mut self, value: i128) {
        self.write_u128(value as u128);
    }

    fn write_usize(&mut self, value: usize) {
        self.absorb(value as u64);
    }

    fn finish(&self) -> u64 {
        self.state
    }
}

/// One recursive edge of a canonical identity, carrying the digest of the
/// subtree behind it.
///
/// These keys are compared and hashed far more often than they are built --
/// on a fresh Lattice compile, roughly ten times more often -- and both
/// operations used to walk the whole tree: `Hash` always, `Ord` until two keys
/// diverged. Digesting each node once at construction turns both into integer
/// work. Construction stays proportional to the node rather than the subtree,
/// because a parent absorbs its children's digests rather than their contents.
///
/// `Ord` compares digests and falls back to the structural comparison only on
/// a tie -- the ADR-0074 shape, a stable digest with a cold structural
/// tiebreak. It is a total order and consistent with `Eq`: equal values imply
/// an equal digest, so a digest difference always implies a value difference,
/// and a digest tie is decided structurally exactly as before. `Hash` writes
/// the digest, which is consistent with that `Eq` for the same reason.
#[derive(Debug)]
pub struct Node<T> {
    digest: u64,
    value: Arc<T>,
}

impl<T: std::hash::Hash> Node<T> {
    /// Digests `value` once and shares it behind an `Arc`.
    pub fn new(value: T) -> Self {
        Self {
            digest: Self::digest_of(&value),
            value: Arc::new(value),
        }
    }

    /// Adopts an already-shared value, digesting it once.
    pub fn from_arc(value: Arc<T>) -> Self {
        Self {
            digest: Self::digest_of(&value),
            value,
        }
    }

    fn digest_of(value: &T) -> u64 {
        let mut hasher = IdentityHasher::new();
        value.hash(&mut hasher);
        hasher.digest()
    }
}

impl<T> Node<T> {
    /// The shared value, for a caller that needs to keep the sharing.
    pub fn as_arc(&self) -> &Arc<T> {
        &self.value
    }
}

impl<T: Clone> Node<T> {
    /// Takes the value out, reclaiming it without a copy when this edge is its
    /// last holder. A consumer converting an identity into another
    /// representation used to move the value; keep that move where the sharing
    /// allows it rather than paying a deep clone for the wrapper.
    pub fn into_inner(self) -> T {
        Arc::try_unwrap(self.value).unwrap_or_else(|shared| (*shared).clone())
    }
}

impl<T> Clone for Node<T> {
    fn clone(&self) -> Self {
        Self {
            digest: self.digest,
            value: Arc::clone(&self.value),
        }
    }
}

impl<T> std::ops::Deref for Node<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> AsRef<T> for Node<T> {
    fn as_ref(&self) -> &T {
        &self.value
    }
}

impl<T: std::hash::Hash> From<T> for Node<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: PartialEq> PartialEq for Node<T> {
    fn eq(&self, other: &Self) -> bool {
        // Cloning an edge shares it, so the identical-pointer case is common
        // enough to answer before touching the digest.
        Arc::ptr_eq(&self.value, &other.value)
            || (self.digest == other.digest && self.value == other.value)
    }
}

impl<T: Eq> Eq for Node<T> {}

/// Bounded on `PartialOrd` rather than `Ord` so a `#[derive(PartialOrd)]` on a
/// payload that holds `Node` edges can prove it: the derive supplies only
/// `PartialOrd` for the type parameters. Digest-first ordering extends `T`'s
/// order faithfully exactly when that order is total, which every canonical
/// identity's is -- they all derive `Ord` alongside it. `Node` is a building
/// block for those identities, not a general-purpose smart pointer, so that is
/// the contract rather than a gap.
impl<T: PartialOrd> PartialOrd for Node<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if Arc::ptr_eq(&self.value, &other.value) {
            return Some(std::cmp::Ordering::Equal);
        }
        match self.digest.cmp(&other.digest) {
            std::cmp::Ordering::Equal => self.value.partial_cmp(&other.value),
            ordering => Some(ordering),
        }
    }
}

impl<T: Ord> Ord for Node<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if Arc::ptr_eq(&self.value, &other.value) {
            return std::cmp::Ordering::Equal;
        }
        match self.digest.cmp(&other.digest) {
            std::cmp::Ordering::Equal => self.value.cmp(&other.value),
            ordering => ordering,
        }
    }
}

/// Writes the digest, which is the point: a probe that used to walk the whole
/// subtree writes eight bytes instead.
///
/// This is only sound because no durable name reads `Hash` any more.
/// `stable_anonymous_identity_digest` used to mint `__anon_struct_{digest}`
/// through it, so writing the digest here would have renamed every anonymous
/// nominal in every program; that path walks the explicit encoding in
/// `stable_digest::DurableEncode` instead. Anything else that needs the
/// structure rather than an accelerator belongs there too, not here.
impl<T> std::hash::Hash for Node<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.digest);
    }
}

/// The kind of an anonymous nominal is part of its identity even when its
/// producer, structural site, and arguments happen to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnonymousNominalKind {
    Struct,
    Enum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnonymousMemberKind {
    Method,
    AssociatedFunction,
    Destructor,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnonymousMemberKey {
    pub kind: AnonymousMemberKind,
    pub name: Arc<str>,
}

/// A canonical specialization value. Strings are represented by content;
/// interner symbols are never stable values.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalArgumentValue<D, M> {
    Integer(i128),
    Bool(bool),
    Type(Node<TypeInstanceKey<D, M>>),
    Function(Node<FunctionInstanceKey<D, M>>),
    Unit,
    String(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalArguments<D, M> {
    /// Type-valued comptime arguments in their declaration-relative order.
    pub types: Arc<[TypeInstanceKey<D, M>]>,
    /// Non-type comptime arguments in their declaration-relative order.
    pub values: Arc<[CanonicalArgumentValue<D, M>]>,
}

// The two streams deliberately avoid storing a redundant tag per element.
// Their mixed positional order is reconstructed only against the base
// function's durable parameter schema (`parameter_comptime` plus the
// corresponding semantic parameter types). Every specialization key includes
// that base function identity, so arguments are never compared without the
// schema which tells consumers how to interleave these declaration-ordered
// streams.

impl<D, M> Default for CanonicalArguments<D, M> {
    fn default() -> Self {
        Self {
            types: Arc::new([]),
            values: Arc::new([]),
        }
    }
}

/// The stable producer of an anonymous nominal. A specialized or anonymous
/// function is a producer in its own right.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableProducerId<D, M> {
    Definition(D),
    Function(Node<FunctionInstanceKey<D, M>>),
}

/// Canonical identity of one anonymous nominal: the kind of type it is, the
/// producer whose reduction minted it, and where in that producer's body it
/// sits.
///
/// The comptime arguments the producer was applied to are deliberately *not* a
/// field here. They used to be, next to a producer that already carried them,
/// so the identity of one nesting level named the level below it twice. That
/// made every identity a strict binary tree over a graph whose distinct nodes
/// are merely linear in the nesting depth, and every walk of one -- relocation,
/// the durable name encoding, the durable key encoding, structural equality,
/// retained-size accounting -- doubled per level. A ten-deep `Pair(Pair(..))`
/// cost a second, a twenty-deep one hung the compiler outright with no
/// diagnostic, and the `MAX_SPECIALIZATION_ROUNDS` depth guard was permanently
/// out of reach: `f(comptime T: type, ..)` calling `f(Pair(T), ..)` hung long
/// before round 64 instead of reporting E1200 (RUE-1699).
///
/// Reading the arguments back off the producer keeps the identity relation
/// exactly as it was, because the two were always minted together from one
/// source: `canonical_function_producer` builds the producer *from* the
/// arguments and the specialization boundary in `specialize.rs` does the same,
/// while every producer that took no arguments paired itself with an empty
/// stream. So `arguments` was never independent information -- it was either
/// the producer specialization's own streams or empty.
/// [`AnonymousNominalKey::producer_arguments`] is that read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnonymousNominalKey<D, M> {
    pub kind: AnonymousNominalKind,
    pub producer: StableProducerId<D, M>,
    pub anchor: rue_rir::RirStructuralAnchor,
}

impl<D, M> AnonymousNominalKey<D, M> {
    /// The comptime arguments this nominal's producer was applied to, or
    /// `None` for a producer that took none: a plain definition, an
    /// unspecialized function, an anonymous member, or drop glue.
    ///
    /// Callers that render or inspect those arguments read them here rather
    /// than from a second copy; see the type's own documentation for why there
    /// is no second copy to read.
    pub fn producer_arguments(&self) -> Option<&CanonicalArguments<D, M>> {
        match &self.producer {
            StableProducerId::Definition(_) => None,
            StableProducerId::Function(function) => match function.as_ref() {
                FunctionInstanceKey::Specialization { arguments, .. } => Some(arguments),
                FunctionInstanceKey::Definition(_)
                | FunctionInstanceKey::AnonymousMember { .. }
                | FunctionInstanceKey::DropGlue(_) => None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NominalInstanceKey<D, M> {
    Builtin {
        kind: AnonymousNominalKind,
        name: Arc<str>,
    },
    Named(D),
    Anonymous(Node<AnonymousNominalKey<D, M>>),
}

/// Canonical identity of a concrete type instance. `D` and `M` are the
/// definition and module identity domains selected by the owning semantic
/// boundary (issuer-scoped tokens in AIR, durable keys in the compiler).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypeInstanceKey<D, M> {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,
    Unit,
    Never,
    ComptimeType,
    BuiltinNominal {
        kind: AnonymousNominalKind,
        name: Arc<str>,
    },
    Nominal(NominalInstanceKey<D, M>),
    Array {
        element: Node<Self>,
        len: u64,
    },
    Slice {
        element: Node<Self>,
        name: Arc<str>,
    },
    PtrConst(Node<Self>),
    PtrMut(Node<Self>),
    Module(M),
    GenericParameter(u32),
}

/// Canonical identity of one source or synthesized function instance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FunctionInstanceKey<D, M> {
    Definition(D),
    Specialization {
        base: Node<FunctionInstanceKey<D, M>>,
        arguments: CanonicalArguments<D, M>,
    },
    AnonymousMember {
        owner: Node<TypeInstanceKey<D, M>>,
        member: AnonymousMemberKey,
    },
    DropGlue(Node<TypeInstanceKey<D, M>>),
}

impl<D, M> CanonicalArgumentValue<D, M> {
    /// Relocate every definition and module identity carried by this stable
    /// argument value without changing its language-level value.
    pub fn try_map_identities<D2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<CanonicalArgumentValue<D2, M2>, E> {
        Ok(match self {
            Self::Integer(value) => CanonicalArgumentValue::Integer(*value),
            Self::Bool(value) => CanonicalArgumentValue::Bool(*value),
            Self::Type(value) => CanonicalArgumentValue::Type(Node::new(
                value.try_map_identities(definition, module)?,
            )),
            Self::Function(value) => CanonicalArgumentValue::Function(Node::new(
                value.try_map_identities(definition, module)?,
            )),
            Self::Unit => CanonicalArgumentValue::Unit,
            Self::String(value) => CanonicalArgumentValue::String(value.clone()),
        })
    }
}

impl<D, M> CanonicalArguments<D, M> {
    pub fn try_map_identities<D2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<CanonicalArguments<D2, M2>, E> {
        let types = self
            .types
            .iter()
            .map(|value| value.try_map_identities(definition, module))
            .collect::<Result<Vec<_>, _>>()?;
        let values = self
            .values
            .iter()
            .map(|value| value.try_map_identities(definition, module))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CanonicalArguments {
            types: types.into(),
            values: values.into(),
        })
    }
}

impl<D, M> AnonymousNominalKey<D, M> {
    /// Relocate the complete recursive identity graph without changing its
    /// language-level identity. Durable body projection and current-request
    /// validation deliberately share this traversal.
    ///
    /// The producer is the whole reach of the key: the comptime arguments it
    /// was minted under live inside that producer's specialization, so this
    /// walks each nesting level once (RUE-1699).
    pub fn try_map_identities<D2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<AnonymousNominalKey<D2, M2>, E> {
        Ok(AnonymousNominalKey {
            kind: self.kind,
            producer: match &self.producer {
                StableProducerId::Definition(value) => {
                    StableProducerId::Definition(definition(value)?)
                }
                StableProducerId::Function(value) => StableProducerId::Function(Node::new(
                    value.try_map_identities(definition, module)?,
                )),
            },
            anchor: self.anchor.clone(),
        })
    }
}

impl<D: Clone + std::hash::Hash, M: Clone + std::hash::Hash> AnonymousNominalKey<D, M> {
    /// The canonical producer form of this key: every empty-argument function
    /// `Specialization` wrapper on the producer's base spine collapsed to its
    /// base (see [`FunctionInstanceKey::with_collapsed_empty_specializations`]).
    ///
    /// This is the form the semantic epoch's `canonical_function_producer`
    /// always mints under and the form production body-export carries (the
    /// warm==cold digest invariant requires it), so every identity consumer
    /// that digests or dedups producer-nominal keys must compare keys in this
    /// form. Returns `Cow::Borrowed` when the key is already canonical — the
    /// collapse is a pure normalization, never a change of identity.
    pub fn with_canonical_producer(&self) -> std::borrow::Cow<'_, Self> {
        match &self.producer {
            StableProducerId::Definition(_) => std::borrow::Cow::Borrowed(self),
            StableProducerId::Function(function) => {
                match function.with_collapsed_empty_specializations() {
                    std::borrow::Cow::Borrowed(_) => std::borrow::Cow::Borrowed(self),
                    std::borrow::Cow::Owned(collapsed) => std::borrow::Cow::Owned(Self {
                        kind: self.kind,
                        producer: StableProducerId::Function(Node::new(collapsed)),
                        anchor: self.anchor.clone(),
                    }),
                }
            }
        }
    }
}

impl<D: Clone + std::hash::Hash, M: Clone + std::hash::Hash> FunctionInstanceKey<D, M> {
    /// Collapse every `Specialization` wrapper carrying no arguments to its
    /// base, recursively along the base spine — the canonical producer form
    /// the epoch's `canonical_function_producer` emits
    /// (`Function(Specialization { base, args: [] })` ≡ `Function(base)`).
    ///
    /// The collapse covers the function-producer spine only: an
    /// `AnonymousMember` owner or `DropGlue` pointee is a type instance the
    /// epoch never spells through `canonical_function_producer`, so it is
    /// carried verbatim. Returns `Cow::Borrowed` when the spine is already
    /// collapsed.
    pub fn with_collapsed_empty_specializations(&self) -> std::borrow::Cow<'_, Self> {
        match self {
            Self::Specialization { base, arguments }
                if arguments.types.is_empty() && arguments.values.is_empty() =>
            {
                std::borrow::Cow::Owned(base.with_collapsed_empty_specializations().into_owned())
            }
            Self::Specialization { base, arguments } => {
                match base.with_collapsed_empty_specializations() {
                    std::borrow::Cow::Borrowed(_) => std::borrow::Cow::Borrowed(self),
                    std::borrow::Cow::Owned(base) => {
                        std::borrow::Cow::Owned(Self::Specialization {
                            base: Node::new(base),
                            arguments: arguments.clone(),
                        })
                    }
                }
            }
            _ => std::borrow::Cow::Borrowed(self),
        }
    }
}

impl<D, M> TypeInstanceKey<D, M> {
    pub fn try_map_identities<D2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<TypeInstanceKey<D2, M2>, E> {
        Ok(match self {
            Self::I8 => TypeInstanceKey::I8,
            Self::I16 => TypeInstanceKey::I16,
            Self::I32 => TypeInstanceKey::I32,
            Self::I64 => TypeInstanceKey::I64,
            Self::U8 => TypeInstanceKey::U8,
            Self::U16 => TypeInstanceKey::U16,
            Self::U32 => TypeInstanceKey::U32,
            Self::U64 => TypeInstanceKey::U64,
            Self::Bool => TypeInstanceKey::Bool,
            Self::Unit => TypeInstanceKey::Unit,
            Self::Never => TypeInstanceKey::Never,
            Self::ComptimeType => TypeInstanceKey::ComptimeType,
            Self::BuiltinNominal { kind, name } => TypeInstanceKey::BuiltinNominal {
                kind: *kind,
                name: name.clone(),
            },
            Self::Nominal(value) => TypeInstanceKey::Nominal(match value {
                NominalInstanceKey::Builtin { kind, name } => NominalInstanceKey::Builtin {
                    kind: *kind,
                    name: name.clone(),
                },
                NominalInstanceKey::Named(value) => NominalInstanceKey::Named(definition(value)?),
                NominalInstanceKey::Anonymous(value) => NominalInstanceKey::Anonymous(Node::new(
                    value.try_map_identities(definition, module)?,
                )),
            }),
            Self::Array { element, len } => TypeInstanceKey::Array {
                element: Node::new(element.try_map_identities(definition, module)?),
                len: *len,
            },
            Self::Slice { element, name } => TypeInstanceKey::Slice {
                element: Node::new(element.try_map_identities(definition, module)?),
                name: name.clone(),
            },
            Self::PtrConst(value) => {
                TypeInstanceKey::PtrConst(Node::new(value.try_map_identities(definition, module)?))
            }
            Self::PtrMut(value) => {
                TypeInstanceKey::PtrMut(Node::new(value.try_map_identities(definition, module)?))
            }
            Self::Module(value) => TypeInstanceKey::Module(module(value)?),
            Self::GenericParameter(index) => TypeInstanceKey::GenericParameter(*index),
        })
    }
}

impl<D, M> FunctionInstanceKey<D, M> {
    pub fn try_map_identities<D2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<FunctionInstanceKey<D2, M2>, E> {
        Ok(match self {
            Self::Definition(value) => FunctionInstanceKey::Definition(definition(value)?),
            Self::Specialization { base, arguments } => FunctionInstanceKey::Specialization {
                base: Node::new(base.try_map_identities(definition, module)?),
                arguments: arguments.try_map_identities(definition, module)?,
            },
            Self::AnonymousMember { owner, member } => FunctionInstanceKey::AnonymousMember {
                owner: Node::new(owner.try_map_identities(definition, module)?),
                member: member.clone(),
            },
            Self::DropGlue(value) => FunctionInstanceKey::DropGlue(Node::new(
                value.try_map_identities(definition, module)?,
            )),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompilerCallableId {
    ProgramEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableCallableId<D, M> {
    Function(FunctionInstanceKey<D, M>),
    Runtime(RuntimeHelperId),
    Compiler(CompilerCallableId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocalAtomKind {
    String,
    ReadOnlyData,
    WritableData,
}

/// Logical occurrence identity for data owned by a function record. The
/// structural anchor is definition-relative and independent of source spans,
/// pool allocation, string content, and request-local dense indices.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalAtomId<D, M> {
    pub producer: FunctionInstanceKey<D, M>,
    pub kind: LocalAtomKind,
    pub anchor: rue_rir::RirStructuralAnchor,
}

impl<D, M> LocalAtomId<D, M> {
    pub fn try_map_identities<D2: std::hash::Hash, M2: std::hash::Hash, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<LocalAtomId<D2, M2>, E> {
        Ok(LocalAtomId {
            producer: self.producer.try_map_identities(definition, module)?,
            kind: self.kind,
            anchor: self.anchor.clone(),
        })
    }
}

/// One occurrence-preserving local data record. `dense_id` is only the current
/// request's projection into its content table; aliases may share it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAtomRecord<D, M> {
    pub identity: LocalAtomId<D, M>,
    pub content: Arc<str>,
    pub dense_id: u32,
}

/// Request-independent representation of one local-data occurrence. Dense
/// table indices are deliberately excluded: they are reconstructed when a
/// durable semantic body is installed into a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBodyLocalAtom<D, M> {
    pub identity: LocalAtomId<D, M>,
    pub content: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableSymbolId<D, M> {
    Callable(StableCallableId<D, M>),
    ReservedRuntime(ReservedExportId),
    LocalAtom(LocalAtomId<D, M>),
}

/// The exhaustive namespace of a semantically bound definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableDefinitionNamespace {
    Value,
    Type,
    Destructor,
    Method,
}

/// Reviewable inventory of every stable semantic namespace.
pub const STABLE_DEFINITION_NAMESPACES: &[StableDefinitionNamespace] = &[
    StableDefinitionNamespace::Value,
    StableDefinitionNamespace::Type,
    StableDefinitionNamespace::Destructor,
    StableDefinitionNamespace::Method,
];

// One reviewable source generates the stable kind enum, inventory, namespace,
// and ownership policy. Adding a kind cannot compile until all taxonomy fields
// are supplied here.
macro_rules! stable_definition_kind_schema {
    ($consumer:ident) => {
        $consumer! {
            Function, Value, true, false;
            Struct, Type, false, false;
            Enum, Type, false, false;
            ValueConst, Value, false, false;
            ModuleBinding, Value, false, false;
            Destructor, Destructor, true, true;
            Method, Method, true, true;
            AssociatedFunction, Method, true, true;
        }
    };
}

macro_rules! define_stable_definition_kind_schema {
    ($( $kind:ident, $namespace:ident, $owns_body:literal, $requires_owner:literal; )*) => {
        /// The exhaustive kind of a semantically bound definition.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum StableDefinitionKind {
            $( $kind, )*
        }

        /// Reviewable inventory of every stable semantic definition kind.
        pub const STABLE_DEFINITION_KINDS: &[StableDefinitionKind] = &[
            $( StableDefinitionKind::$kind, )*
        ];

        impl StableDefinitionKind {
            /// The only namespace in which this kind can be issued.
            pub const fn namespace(self) -> StableDefinitionNamespace {
                match self {
                    $( Self::$kind => StableDefinitionNamespace::$namespace, )*
                }
            }

            /// Whether this definition owns an executable semantic body.
            pub const fn owns_body(self) -> bool {
                match self {
                    $( Self::$kind => $owns_body, )*
                }
            }

            /// Whether this definition must name an owning nominal type.
            pub const fn requires_owner(self) -> bool {
                match self {
                    $( Self::$kind => $requires_owner, )*
                }
            }
        }
    };
}

stable_definition_kind_schema!(define_stable_definition_kind_schema);

#[cfg(test)]
mod tests {
    use ahash::AHashSet;
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn taxonomy_declares_every_kind_namespace_and_owner_shape_once() {
        use StableDefinitionKind as K;
        use StableDefinitionNamespace as N;

        let cases = [
            (K::Function, N::Value, true, false),
            (K::Struct, N::Type, false, false),
            (K::Enum, N::Type, false, false),
            (K::ValueConst, N::Value, false, false),
            (K::ModuleBinding, N::Value, false, false),
            (K::Destructor, N::Destructor, true, true),
            (K::Method, N::Method, true, true),
            (K::AssociatedFunction, N::Method, true, true),
        ];
        for (kind, namespace, owns_body, requires_owner) in cases {
            assert_eq!(kind.namespace(), namespace);
            assert_eq!(kind.owns_body(), owns_body);
            assert_eq!(kind.requires_owner(), requires_owner);
        }
    }

    #[test]
    fn generic_identity_algebra_has_exact_equality_ordering_and_hashing() {
        use rue_rir::RirStructuralPathSegment as S;

        type T = TypeInstanceKey<&'static str, &'static str>;
        // The comptime arguments hang off the producer specialization rather
        // than off the key itself (RUE-1699), so the corpus reaches a nested
        // edge the way a real identity does.
        let make = |definition, module, path| {
            T::Nominal(NominalInstanceKey::Anonymous(Node::new(
                AnonymousNominalKey {
                    kind: AnonymousNominalKind::Struct,
                    producer: StableProducerId::Function(Node::new(
                        FunctionInstanceKey::Specialization {
                            base: Node::new(FunctionInstanceKey::Definition(definition)),
                            arguments: CanonicalArguments {
                                types: Arc::from([T::Module(module)]),
                                values: Arc::new([]),
                            },
                        },
                    )),
                    anchor: rue_rir::RirStructuralAnchor::new(path),
                },
            )))
        };
        let baseline = make("make", "pkg", vec![S::Body, S::AnonymousType(0)]);
        let same = make("make", "pkg", vec![S::Body, S::AnonymousType(0)]);
        let moved = make(
            "make",
            "pkg",
            vec![S::Body, S::Statement(0), S::AnonymousType(0)],
        );
        let other_definition = make("other", "pkg", vec![S::Body, S::AnonymousType(0)]);

        assert_eq!(baseline, same);
        assert_ne!(baseline, moved);
        assert_ne!(baseline, other_definition);
        assert_eq!(
            BTreeSet::from([baseline.clone(), same.clone(), moved.clone()]).len(),
            2
        );
        assert_eq!(AHashSet::from([baseline, same, moved]).len(), 2);
    }
}
