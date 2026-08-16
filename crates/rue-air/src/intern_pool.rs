//! Type intern pool for efficient type representation.
//!
//! This module implements canonical composite-type storage inspired by Zig's
//! `InternPool`. Compact [`Type`] handles enable:
//!
//! - O(1) type equality (u32 comparison)
//! - Efficient memory usage
//! - Clean parallel compilation (no per-function type merging)
//! - Canonical identities for generic instantiations
//!
//! # Architecture
//!
//! The `TypeInternPool` serves as a canonical repository for all composite types:
//! - **Structs and enums** are nominal types (same name = same type)
//! - **Arrays** are structural types (same element type + length = same type)
//!
//! [`Type`] is the compact compiler-facing handle. Composite `StructId`,
//! `EnumId`, `ArrayTypeId`, and pointer IDs are opaque typed storage identities.
//! The pool stores canonical [`Type`] values directly in structural keys and
//! children, so definitions and structural identities resolve through one pool
//! (ADR-0024).
//!
//! # Thread Safety
//!
//! The pool uses `RwLock` for thread-safe access during parallel compilation:
//! - Read lock for lookups (common case)
//! - Write lock for insertions (rare, during declaration gathering)

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::{Arc, LazyLock, PoisonError, RwLock};

use lasso::Spur;
use rue_span::FileId;

use crate::layout::{Layout, LayoutKind, PaddingRange};
use crate::path_norm::{mangle_symbol_component, normalize_module_path};
use crate::type_encoding;
use crate::types::{
    ArrayTypeId, EnumDef, EnumId, LangItem, PtrConstTypeId, PtrMutTypeId, StructDef, StructField,
    StructId, Type, TypeKind,
};

/// Type data stored in the intern pool.
///
/// This is NOT Copy - it lives in the pool. Structural children are canonical
/// [`Type`] handles owned by this pool's semantic epoch.
///
/// # Type Categories
///
/// - **Struct** and **Enum** are nominal types: identity comes from the name
/// - **Array**, **PtrConst**, and **PtrMut** are structural types: identity comes from element/pointee type
#[derive(Debug, Clone)]
pub enum TypeData {
    /// Private anonymous-construction slot. No live [`Type`] is issued for it.
    ReservedStruct,

    /// Named struct identity whose definition has not completed yet.
    DeclaredStruct(StructData),

    /// Named enum identity whose definition has not completed yet.
    DeclaredEnum(EnumData),

    /// User-defined struct (nominal type).
    ///
    /// Two structs with the same fields but different names are different types.
    Struct(StructData),

    /// User-defined enum (nominal type).
    ///
    /// Two enums with the same variants but different names are different types.
    Enum(EnumData),

    /// Fixed-size array (structural type).
    ///
    /// Arrays with the same element type and length are the same type,
    /// regardless of where they were defined.
    Array {
        element: Type,
        abi_slots: u32,
        len: u64,
    },

    /// Raw const pointer (structural type).
    ///
    /// `ptr const T` - pointer to immutable data.
    PtrConst { pointee: Type },

    /// Raw mut pointer (structural type).
    ///
    /// `ptr mut T` - pointer to mutable data.
    PtrMut { pointee: Type },
}

/// Why a compact [`Type`] cannot be used for a requested pool operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeValidationError {
    InvalidEncoding,
    PoolIndexOutOfRange,
    KindMismatch,
    ReservedEntry,
    IncompleteDefinition,
    ComptimeStructuralChild,
    ModuleStructuralChild,
    RecoveryType,
}

/// Canonical properties derived from the by-value containment graph.
///
/// The mutable pool may temporarily leave an entry unanalyzed while named
/// declarations are incomplete. Semantic finalization computes the whole graph
/// in one bounded pass; types created later by specialization derive their facts
/// from already-finalized children as they are interned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TypeContainmentFacts {
    carries_linear: bool,
    needs_drop: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TypeDerivedFacts {
    containment: TypeContainmentFacts,
    abi_slots: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeContainmentCycle {
    pub(crate) root: Type,
    pub(crate) path: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TypeContainmentWork {
    pub(crate) nodes: usize,
    pub(crate) edges: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TypeContainmentMetrics {
    pub(crate) finalize_checks: usize,
    pub(crate) nodes: usize,
    pub(crate) edges: usize,
}

impl TypeData {
    fn is_incomplete(&self) -> bool {
        matches!(
            self,
            Self::ReservedStruct | Self::DeclaredStruct(_) | Self::DeclaredEnum(_)
        )
    }

    fn kind(&self) -> PoolEntryKind {
        match self {
            Self::ReservedStruct | Self::DeclaredStruct(_) | Self::Struct(_) => {
                PoolEntryKind::Struct
            }
            Self::DeclaredEnum(_) | Self::Enum(_) => PoolEntryKind::Enum,
            Self::Array { .. } => PoolEntryKind::Array,
            Self::PtrConst { .. } => PoolEntryKind::PtrConst,
            Self::PtrMut { .. } => PoolEntryKind::PtrMut,
        }
    }

    fn set_abi_slots(&mut self, abi_slots: u32) {
        match self {
            Self::Struct(data) => data.abi_slots = abi_slots,
            Self::Enum(data) => data.abi_slots = abi_slots,
            Self::Array {
                abi_slots: stored, ..
            } => *stored = abi_slots,
            Self::PtrConst { .. } | Self::PtrMut { .. } => {}
            Self::ReservedStruct | Self::DeclaredStruct(_) | Self::DeclaredEnum(_) => {
                unreachable!("incomplete type has no derived ABI width")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolEntryKind {
    Struct,
    Enum,
    Array,
    PtrConst,
    PtrMut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationMode {
    StructuralChild,
    Complete,
    CompleteChild,
}

struct TypeVisitSet {
    inline: [Type; 64],
    len: usize,
    overflow: Option<HashSet<Type>>,
}

impl TypeVisitSet {
    fn new() -> Self {
        Self {
            inline: [Type::UNIT; 64],
            len: 0,
            overflow: None,
        }
    }

    fn insert(&mut self, ty: Type) -> bool {
        if let Some(overflow) = &mut self.overflow {
            return overflow.insert(ty);
        }
        if self.inline[..self.len].contains(&ty) {
            return false;
        }
        if self.len < self.inline.len() {
            self.inline[self.len] = ty;
            self.len += 1;
            return true;
        }
        let mut overflow = HashSet::with_capacity(self.len + 1);
        overflow.extend(self.inline);
        let inserted = overflow.insert(ty);
        self.overflow = Some(overflow);
        inserted
    }
}

impl ValidationMode {
    fn requires_complete(self) -> bool {
        matches!(self, Self::Complete | Self::CompleteChild)
    }

    fn is_structural_child(self) -> bool {
        matches!(self, Self::StructuralChild | Self::CompleteChild)
    }

    fn child(self) -> Self {
        if self.requires_complete() {
            Self::CompleteChild
        } else {
            Self::StructuralChild
        }
    }
}

/// Declaration-order lookup for a nominal's member names.
///
/// A struct's fields and an enum's variants are fixed when the definition is
/// installed, so the position of every member name is known once and never
/// changes. Building it there replaces the per-lookup walk of `String`
/// comparisons the bare definitions used with one hash and a binary search
/// (RUE-1219).
///
/// The index stores `(name hash, declaration position)` sorted by hash rather
/// than owning the names: one allocation per nominal instead of one per member,
/// which matters because installation is the common case and lookup is the rare
/// one for small nominals. A probe therefore *proposes* positions and the caller
/// confirms each against the real member name, so a hash collision costs an
/// extra comparison and never a wrong answer.
///
/// A duplicate name resolves to the *first* declaration position, matching the
/// linear scan this replaces: duplicates are a declaration error reported
/// elsewhere, and until then both resolve the way they always did.
#[derive(Clone, Default)]
struct MemberNameIndex {
    /// `(hash, position)` sorted by hash, then by position so equal-hash runs
    /// stay in declaration order.
    entries: Box<[(u64, u32)]>,
}

impl MemberNameIndex {
    /// FNV-1a. Chosen for being short enough to inline over the two-to-ten
    /// byte member names that dominate, and fixed rather than randomly seeded
    /// so a pool built twice from the same declarations probes identically.
    fn hash(name: &str) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in name.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    fn build<'a>(names: impl ExactSizeIterator<Item = &'a str>) -> Self {
        let mut entries: Vec<(u64, u32)> = names
            .enumerate()
            .map(|(position, name)| (Self::hash(name), position as u32))
            .collect();
        entries.sort_unstable();
        Self {
            entries: entries.into_boxed_slice(),
        }
    }

    /// The declaration positions whose member name hashes to `name`'s hash, in
    /// declaration order. Callers confirm the name itself.
    fn candidates(&self, name: &str) -> impl Iterator<Item = usize> + '_ {
        let hash = Self::hash(name);
        let start = self.entries.partition_point(|(entry, _)| *entry < hash);
        self.entries[start..]
            .iter()
            .take_while(move |(entry, _)| *entry == hash)
            .map(|(_, position)| *position as usize)
    }
}

/// A struct definition as the pool stores it: the definition plus the field
/// lookup built from it.
///
/// The index travels with the definition rather than living beside it in the
/// pool entry, so a reader that already holds the shared definition resolves a
/// field name without reacquiring the pool lock, and the frozen pool's
/// borrow-returning reads get the same lookup (RUE-1219).
///
/// Only destructor assignment, destructor requalification, and the linearity
/// marker mutate an installed definition, and none of them touch `fields`;
/// [`StructDefEntry::metadata_mut`] is the narrow door they go through, so the
/// index cannot drift from the fields it indexes.
#[derive(Clone)]
pub struct StructDefEntry {
    def: StructDef,
    field_index: MemberNameIndex,
}

impl StructDefEntry {
    pub(crate) fn new(def: StructDef) -> Self {
        let field_index =
            MemberNameIndex::build(def.fields.iter().map(|field| field.name.as_str()));
        Self { def, field_index }
    }

    /// Find a field by name and return its declaration index and definition.
    pub fn find_field(&self, name: &str) -> Option<(usize, &StructField)> {
        self.find_field_with_observer(name, || {})
    }

    /// Indexed field lookup with a hook for each candidate-name comparison.
    ///
    /// The hook is a test seam for proving callers use the member index rather
    /// than scanning `fields`; the no-op production caller is inlined away.
    #[inline(always)]
    pub(crate) fn find_field_with_observer(
        &self,
        name: &str,
        mut observe_candidate: impl FnMut(),
    ) -> Option<(usize, &StructField)> {
        self.field_index
            .candidates(name)
            .map(|index| (index, &self.def.fields[index]))
            .find(|(_, field)| {
                observe_candidate();
                field.name == name
            })
    }

    /// Mutable access to the declaration metadata of an installed definition.
    ///
    /// Callers may update drop and linearity metadata only. Replacing `fields`
    /// would invalidate the field index built at installation.
    fn metadata_mut(&mut self) -> &mut StructDef {
        &mut self.def
    }
}

impl Deref for StructDefEntry {
    type Target = StructDef;

    fn deref(&self) -> &StructDef {
        &self.def
    }
}

/// The lookup index is derived from `fields` and carries no information the
/// definition does not already state, so it stays out of the rendering that
/// durable-output comparisons hash and diff.
impl std::fmt::Debug for StructDefEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.def.fmt(f)
    }
}

/// An enum definition as the pool stores it: the definition plus the variant
/// lookup built from it. See [`StructDefEntry`].
#[derive(Clone)]
pub struct EnumDefEntry {
    def: EnumDef,
    variant_index: MemberNameIndex,
}

impl EnumDefEntry {
    pub(crate) fn new(def: EnumDef) -> Self {
        let variant_index =
            MemberNameIndex::build(def.variants.iter().map(|variant| variant.as_ref()));
        Self { def, variant_index }
    }

    /// Find a variant by name and return its declaration index.
    pub fn find_variant(&self, name: &str) -> Option<usize> {
        self.variant_index
            .candidates(name)
            .find(|&index| &*self.def.variants[index] == name)
    }
}

impl Deref for EnumDefEntry {
    type Target = EnumDef;

    fn deref(&self) -> &EnumDef {
        &self.def
    }
}

/// See [`StructDefEntry`]'s `Debug`: the variant index is derived state.
impl std::fmt::Debug for EnumDefEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.def.fmt(f)
    }
}

/// Data for a struct type in the intern pool.
///
/// The pool entry for a nominal struct and its definition.
///
/// The definition is held behind an `Arc` because it is effectively immutable
/// once installed: only destructor assignment, destructor requalification, and
/// the linearity marker write to it, each once per nominal during declaration
/// finalization (through `Arc::make_mut`). Every other read — including the
/// mutable pool's `struct_def` accessors, which cannot hand out a borrow across
/// their `RwLock` — is a refcount bump instead of a deep clone of the name,
/// field vector, and per-field names (RUE-1147).
#[derive(Debug, Clone)]
pub struct StructData {
    /// The name symbol (interned string).
    pub name: Spur,
    /// Flattened runtime ABI width, derived with containment metadata.
    abi_slots: u32,
    /// The canonical struct definition stored at this pool index.
    pub def: Arc<StructDefEntry>,
}

/// Data for an enum type in the intern pool.
///
/// The pool entry for a nominal enum and its definition. The definition is
/// `Arc`-held for the same reason as [`StructData::def`].
#[derive(Debug, Clone)]
pub struct EnumData {
    /// The name symbol (interned string).
    pub name: Spur,
    /// Flattened runtime ABI width, derived with containment metadata.
    abi_slots: u32,
    /// The canonical enum definition stored at this pool index.
    pub def: Arc<EnumDefEntry>,
}

/// Declaration-only struct metadata available before field resolution.
///
/// Fields are deliberately absent so declaration consumers cannot mistake a
/// nominal shell for a complete definition.
///
/// The names are shared handles, not copies: the pool cannot lend a borrow
/// across its `RwLock`, and most readers want only `file_id`, `is_pub`, or a
/// drop fact, so a read is a pair of refcount bumps rather than two string
/// allocations (RUE-1219).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructDeclarationMetadata {
    pub name: Arc<str>,
    pub is_copy: bool,
    pub is_linear: bool,
    pub destructor: Option<Arc<str>>,
    pub is_builtin: bool,
    pub is_pub: bool,
    pub file_id: FileId,
}

/// Declaration-only enum metadata available before payload resolution.
///
/// Variant names are declaration metadata; payloads are intentionally absent
/// until the enum reaches the complete state. Names are shared for the same
/// reason as [`StructDeclarationMetadata`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnumDeclarationMetadata {
    pub name: Arc<str>,
    pub variants: Arc<[Arc<str>]>,
    pub is_pub: bool,
    pub file_id: FileId,
}

/// Thread-safe intern pool for all composite types.
///
/// The pool is designed to be built during declaration gathering (sequential)
/// and then queried during function body analysis (potentially parallel).
///
/// # Thread Safety
///
/// Uses `RwLock` for interior mutability:
/// - Read lock for lookups (most common)
/// - Write lock for insertions (only during declaration gathering)
///
/// # Usage
///
/// ```ignore
/// let pool = TypeInternPool::new();
///
/// // Register nominal types (structs/enums)
/// let (struct_type, is_new) = pool.register_struct(name_spur, struct_def);
///
/// // Intern structural types (arrays)
/// let array_type = pool.try_intern_array(element_type, 10)?;
///
/// // Look up type data
/// if let Some(data) = pool.try_get(some_type) {
///     match data {
///         TypeData::Struct(s) => println!("struct {}", s.def.name),
///         TypeData::Enum(e) => println!("enum {}", e.def.name),
///         TypeData::Array { element, len, .. } => println!("array of {:?}; {}", element, len),
///     }
/// }
/// ```
#[derive(Debug)]
pub struct TypeInternPool {
    inner: RwLock<TypeInternPoolInner>,
}

/// Immutable type metadata used after semantic analysis completes.
///
/// Semantic analysis is the only phase allowed to extend or update the type
/// universe. [`TypeInternPool::freeze`] consumes that mutable universe after
/// specialization and anonymous-type/destructor discovery have reached their
/// fixed point. CFG construction and code generation receive this type instead:
/// nominal reads borrow definitions directly, and iteration takes no lock and
/// allocates no temporary ID vector.
#[derive(Debug, Clone)]
pub struct FrozenTypeInternPool {
    inner: Arc<TypeInternPoolInner>,
    /// Whole-universe validation is immutable after freezing. A successful
    /// certificate lets every backend fact query validate only its compact
    /// root handle instead of rewalking the same reachable type graph.
    success_validation: Result<(), TypeValidationError>,
}

/// A semantic epoch's canonical type universe, optionally layered on an
/// immutable base shared with every other body of the same request (RUE-1135).
///
/// # Base plus append-only overlay
///
/// Pool indices below `base_len` live in `base` and are read straight out of
/// it; interning appends to this layer and numbers entries from `base_len`
/// upwards, so a canonical `Type` means the same thing in the base and in every
/// epoch layered on it. This is a true base plus append-only overlay, not
/// copy-on-write of the inner store: a body that interns a type copies nothing
/// the base already holds.
///
/// `overrides`/`override_facts` are the escape hatch that keeps the layering
/// *sound* rather than merely fast. A phase that does mutate an already-interned
/// entry — destructor assignment, containment finalization — writes a body-local
/// copy of that one entry instead of touching the shared base. Those maps are
/// empty on the body path, which is why reads check them only when non-empty.
#[derive(Debug, Clone)]
struct TypeInternPoolInner {
    /// The shared immutable prefix, if this universe was derived from one. A
    /// base is itself always flat, so a lookup is one branch deep.
    base: Option<Arc<TypeInternPoolInner>>,

    /// Number of entries owned by `base`. Local entry `i` is pool index
    /// `base_len + i`.
    base_len: usize,

    /// Base entries this epoch rewrote, keyed by pool index. Empty unless a
    /// mutation reached below `base_len`.
    overrides: HashMap<usize, TypeData>,

    /// Base containment facts this epoch rewrote, keyed by pool index.
    override_facts: HashMap<usize, Option<TypeContainmentFacts>>,

    /// Composite type data this epoch interned itself, from `base_len` up.
    types: Vec<TypeData>,

    /// Structural type deduplication: (element, len) -> canonical array `Type`.
    array_map: HashMap<(Type, u64), Type>,

    /// Structural type deduplication: pointee -> canonical ptr const `Type`.
    ptr_const_map: HashMap<Type, Type>,

    /// Structural type deduplication: pointee -> canonical ptr mut `Type`.
    ptr_mut_map: HashMap<Type, Type>,

    /// Ownership facts indexed in lockstep with `types` (so also from
    /// `base_len` up). `None` is permitted only while declaration shells or a
    /// metadata mutation await the next canonical containment pass.
    containment_facts: Vec<Option<TypeContainmentFacts>>,

    /// Number of entries in this universe whose containment facts are `None`:
    /// declaration shells, reserved slots, and completions whose incremental
    /// derivation had an unavailable child. Maintained exactly — `push_entry`
    /// and `set_facts` track every `None`/`Some` transition, and the full pass
    /// recomputes it from what remained unavailable — so the O(1) clean-state
    /// check never scans the pool.
    pending_facts: usize,

    /// Whether facts that are *present* may be wrong. Set only by
    /// [`Self::invalidate_containment_metadata`] (destructor mutation after
    /// facts may already have propagated into ancestors); cleared only by the
    /// canonical full pass. Unlike `pending_facts`, which fails closed
    /// per-entry, a stale pool must refuse every derived read globally.
    facts_stale: bool,

    /// Whether any containment-fact slot changed availability since the last
    /// full pass. A pass leaves incomplete shells (and, transitively, their
    /// by-value ancestors) factless on purpose; re-running it before any
    /// slot turned factless — or factful, which can unblock such an ancestor —
    /// would recompute the identical result, so the full-pass trigger requires
    /// this bit alongside `pending_facts > 0`. Reads of the surviving factless
    /// entries keep failing closed per-entry.
    unsettled_facts: bool,
    containment_metrics: TypeContainmentMetrics,

    /// Structs and enums the compiler generated for anonymous types, recorded
    /// at creation and layered like `types`.
    ///
    /// `__anon_struct_N` / `__anon_enum_N` are legal source declarations — only
    /// `__rue_*` and `_start` are reserved — so "was this generated?" is
    /// membership here, never a name prefix (RUE-1050). This is the authority
    /// for consumers below semantic analysis, which cannot see the sema-side
    /// registry: CFG destructor discovery, drop glue, and symbol spelling
    /// (`struct_symbol_name` / `enum_symbol_name`).
    ///
    /// Every path that mints a generated anonymous nominal must mark it here:
    /// the epoch-local `find_or_create_anon_struct` / `_enum` and the
    /// producer-nominal `mint_anon_struct` / `mint_anon_enum` mirror at the
    /// provider boundary. A pool that carries the type but not the mark spells
    /// a generated anonymous type as if it were a user nominal and desyncs the
    /// callable symbols joined across that boundary (RUE-1193).
    anonymous_structs: HashSet<StructId>,
    anonymous_enums: HashSet<EnumId>,

    /// Nominal struct lookup: (defining file, source name) -> canonical `Type`.
    struct_by_file_name: HashMap<(FileId, Spur), Type>,

    /// Nominal enum lookup: (defining file, source name) -> canonical `Type`.
    enum_by_file_name: HashMap<(FileId, Spur), Type>,

    /// Relocation-stable logical identity for each defining source file.
    symbol_paths: Arc<HashMap<FileId, String>>,

    /// Explicit language-item assignments issued by a trusted frontend or
    /// durable semantic import boundary.
    struct_lang_items: Arc<HashMap<StructId, LangItem>>,

    /// Reverse index enforcing one canonical nominal for each language item.
    lang_item_structs: Arc<HashMap<LangItem, StructId>>,

    /// Structs carrying the `@repr(c)` guarantee marker (ADR-0064 Amendment 1,
    /// RUE-1063). A side map, like `struct_lang_items`, so the marker travels
    /// with the type universe (and into the frozen pool the FFI predicates and
    /// classifier consult) without widening `StructDef` or its durable form. A
    /// layout no-op today; the guarantee that pins C representation and anchors
    /// FFI-safety.
    repr_c_structs: Arc<HashSet<StructId>>,

    /// Latched once a registration asked for a pool index past the published
    /// 24-bit `Type` payload ceiling (spec Appendix C.6:1,
    /// [`MAX_COMPOSITE_TYPES`]).
    ///
    /// Interning runs from hundreds of infallible sites across declaration
    /// collection, type resolution, and specialization, so the ceiling cannot
    /// be reported by threading a `Result` back through all of them. Instead
    /// the pool records the rejection here, stops growing, and semantic
    /// analysis converts the latch into an `E1401` diagnostic at its next
    /// boundary — spec C.1:2 requires a diagnostic, never a wrapped 24-bit
    /// index or an abort. A latched pool is a dying pool: registrations after
    /// the ceiling reuse the final entry, and no artifact built from it is ever
    /// published.
    capacity_exceeded: bool,
}

/// The published ceiling on distinct composite types (structs, enums, arrays,
/// pointers, modules) in one compilation: a live [`Type`] is a `u32` carrying an
/// 8-bit kind tag and a 24-bit type-pool index (spec Appendix C.6:1).
pub const MAX_COMPOSITE_TYPES: u32 = type_encoding::MAX_PAYLOAD + 1;

/// The user-facing text for a compilation refused by the composite-type
/// ceiling (spec Appendix C.6:1, under the C.1:2 policy).
///
/// Shared so that the declaration-binding boundary — which stops the
/// compilation as soon as the latch is visible — and the per-body CFG boundary
/// that backstops it name the same limit in the same words.
pub fn composite_type_limit_message() -> String {
    format!(
        "this compilation defines more distinct composite types (structs, enums, arrays, \
         pointers, modules) than the implementation limit of {MAX_COMPOSITE_TYPES} — a live type \
         handle is a u32 holding an 8-bit kind tag and a 24-bit type-pool index (spec Appendix \
         C.6:1)"
    )
}

fn checked_pool_index(index: usize) -> Option<u32> {
    let index = u32::try_from(index).ok()?;
    (index <= type_encoding::MAX_PAYLOAD).then_some(index)
}

fn complete_type_handle(index: usize, data: &TypeData) -> Type {
    let index = checked_pool_index(index).expect("type pool index invariant");
    match data {
        TypeData::Struct(_) => Type::new_struct(StructId::from_pool_index(index)),
        TypeData::Enum(_) => Type::new_enum(EnumId::from_pool_index(index)),
        TypeData::Array { .. } => Type::new_array(ArrayTypeId::from_pool_index(index)),
        TypeData::PtrConst { .. } => Type::new_ptr_const(PtrConstTypeId::from_pool_index(index)),
        TypeData::PtrMut { .. } => Type::new_ptr_mut(PtrMutTypeId::from_pool_index(index)),
        TypeData::ReservedStruct | TypeData::DeclaredStruct(_) | TypeData::DeclaredEnum(_) => {
            unreachable!("complete type-pool traversal contains an incomplete entry")
        }
    }
}

/// The struct definition read back for a handle that the composite-type
/// capacity latch aliased onto an entry of a different kind, and only for such
/// a handle (see [`TypeInternPoolInner::next_pool_index`]).
///
/// The compilation this pool belongs to is already failing with `E1401` and
/// nothing built from the pool will be published, so the read only has to be
/// answerable without aborting (spec C.1:2). A field-less, drop-less,
/// non-linear struct is the answer that keeps every dependent walk — ABI slot
/// counting, containment, layout — finite and free of further aliased reads.
static ALIASED_STRUCT_DEF: LazyLock<StructDefEntry> = LazyLock::new(|| {
    StructDefEntry::new(StructDef {
        name: Arc::from("<type-pool capacity exceeded>"),
        fields: Vec::new(),
        is_copy: false,
        is_linear: false,
        destructor: None,
        is_builtin: false,
        is_pub: false,
        file_id: FileId::DEFAULT,
    })
});

/// The enum counterpart of [`ALIASED_STRUCT_DEF`]. A variant-less enum has no
/// payload to walk and no discriminant to widen.
static ALIASED_ENUM_DEF: LazyLock<EnumDefEntry> = LazyLock::new(|| {
    EnumDefEntry::new(EnumDef {
        name: Arc::from("<type-pool capacity exceeded>"),
        variants: Arc::from([] as [Arc<str>; 0]),
        variant_payloads: Vec::new(),
        is_pub: false,
        file_id: FileId::DEFAULT,
    })
});

/// Round `offset` up to the next multiple of `align` (a power of two, always at
/// least 1). Saturating so an already-oversized aggregate cannot wrap; the slot
/// budget guard (`MAX_TYPE_SLOTS`) rejects genuinely oversized types earlier.
fn align_up(offset: u64, align: u64) -> u64 {
    debug_assert!(align >= 1, "alignment is at least 1");
    let bump = align.saturating_sub(1);
    offset.saturating_add(bump) & !bump
}

/// The computed compact layout of a struct: its total `size` including tail
/// padding, its `alignment` (the maximum field alignment, minimum 1), the
/// declaration-order byte `field_offsets`, and the interior/tail
/// `padding_ranges`. ADR-0052 "Resolved at acceptance": `size` is rounded up to
/// `alignment` so `stride == size`.
struct CompactAggregateLayout {
    size: u64,
    alignment: u64,
    field_offsets: Vec<u64>,
    padding_ranges: Vec<PaddingRange>,
}

/// The computed compact layout of a tagged enum: an unsigned tag of `tag_size`
/// (with `tag_align == tag_size`) at offset 0, the payload placed at
/// `payload_offset` (the maximum variant alignment), the whole aggregate's
/// `size`/`alignment`, and each variant's payload-field byte offsets relative to
/// the aggregate base.
struct CompactEnumLayout {
    tag_size: u64,
    tag_align: u64,
    payload_offset: u64,
    size: u64,
    alignment: u64,
    variants: Vec<Vec<u64>>,
}

/// The compact `(size, alignment)` of an enum's tag: the smallest unsigned
/// integer that can represent every discriminant (ADR-0052). One variant (or
/// zero) still needs a byte; `u8` covers up to 256 variants, `u16` up to 65536,
/// `u32` beyond. Alignment equals size (natural scalar alignment).
fn compact_enum_tag(variant_count: usize) -> (u64, u64) {
    let width = if variant_count <= 256 {
        1
    } else if variant_count <= 65_536 {
        2
    } else {
        4
    };
    (width, width)
}

impl TypeInternPoolInner {
    fn empty() -> Self {
        Self {
            base: None,
            base_len: 0,
            overrides: HashMap::new(),
            override_facts: HashMap::new(),
            types: Vec::new(),
            array_map: HashMap::new(),
            ptr_const_map: HashMap::new(),
            ptr_mut_map: HashMap::new(),
            containment_facts: Vec::new(),
            pending_facts: 0,
            facts_stale: false,
            unsettled_facts: false,
            containment_metrics: TypeContainmentMetrics::default(),
            anonymous_structs: HashSet::new(),
            anonymous_enums: HashSet::new(),
            struct_by_file_name: HashMap::new(),
            enum_by_file_name: HashMap::new(),
            symbol_paths: Arc::default(),
            struct_lang_items: Arc::default(),
            lang_item_structs: Arc::default(),
            repr_c_structs: Arc::default(),
            capacity_exceeded: false,
        }
    }

    /// Total entries visible in this universe: the shared base plus this
    /// epoch's own appends.
    #[inline]
    fn entry_count(&self) -> usize {
        self.base_len + self.types.len()
    }

    /// The entry at `index`, or `None` if the index is past the universe.
    #[inline]
    fn try_entry(&self, index: usize) -> Option<&TypeData> {
        if index >= self.base_len {
            return self.types.get(index - self.base_len);
        }
        if !self.overrides.is_empty()
            && let Some(entry) = self.overrides.get(&index)
        {
            return Some(entry);
        }
        self.base.as_ref().and_then(|base| base.types.get(index))
    }

    #[inline]
    fn entry(&self, index: usize) -> &TypeData {
        self.try_entry(index)
            .unwrap_or_else(|| panic!("type pool index {index} is out of range"))
    }

    /// A mutable handle to the entry at `index`.
    ///
    /// An index inside the shared base is promoted into this epoch's private
    /// `overrides` first: the base is immutable and shared with sibling bodies,
    /// so it is copied one entry at a time and never written through.
    fn try_entry_mut(&mut self, index: usize) -> Option<&mut TypeData> {
        if index >= self.base_len {
            return self.types.get_mut(index - self.base_len);
        }
        if !self.overrides.contains_key(&index) {
            let entry = self.base.as_ref()?.types.get(index)?.clone();
            self.overrides.insert(index, entry);
        }
        self.overrides.get_mut(&index)
    }

    fn entry_mut(&mut self, index: usize) -> &mut TypeData {
        self.try_entry_mut(index)
            .unwrap_or_else(|| panic!("type pool index {index} is out of range"))
    }

    /// Append one entry with its containment facts, returning its pool index.
    ///
    /// A pool that already ran past the published composite-type ceiling stops
    /// growing: the entry is dropped and the final legal index is returned, so
    /// the store cannot hold entries that no `Type` handle can address.
    fn push_entry(&mut self, entry: TypeData, facts: Option<TypeContainmentFacts>) -> usize {
        if self.capacity_exceeded {
            return type_encoding::MAX_PAYLOAD as usize;
        }
        let index = self.entry_count();
        self.types.push(entry);
        self.containment_facts.push(facts);
        if facts.is_none() {
            self.pending_facts += 1;
            self.unsettled_facts = true;
        }
        index
    }

    fn facts_at(&self, index: usize) -> Option<Option<TypeContainmentFacts>> {
        if index >= self.base_len {
            return self.containment_facts.get(index - self.base_len).copied();
        }
        if !self.override_facts.is_empty()
            && let Some(facts) = self.override_facts.get(&index)
        {
            return Some(*facts);
        }
        self.base
            .as_ref()
            .and_then(|base| base.containment_facts.get(index).copied())
    }

    fn set_facts(&mut self, index: usize, value: Option<TypeContainmentFacts>) {
        let previous = self
            .facts_at(index)
            .expect("set_facts targets an existing pool entry");
        match (previous.is_some(), value.is_some()) {
            (true, false) => self.pending_facts += 1,
            (false, true) => {
                self.pending_facts = self
                    .pending_facts
                    .checked_sub(1)
                    .expect("pending containment-fact accounting underflow");
            }
            _ => {}
        }
        if value.is_none() || previous.is_none() {
            self.unsettled_facts = true;
        }
        if index >= self.base_len {
            self.containment_facts[index - self.base_len] = value;
        } else {
            self.override_facts.insert(index, value);
        }
    }

    fn lookup_array(&self, key: &(Type, u64)) -> Option<Type> {
        self.array_map.get(key).copied().or_else(|| {
            self.base
                .as_ref()
                .and_then(|base| base.array_map.get(key).copied())
        })
    }

    fn lookup_ptr_const(&self, pointee: &Type) -> Option<Type> {
        self.ptr_const_map.get(pointee).copied().or_else(|| {
            self.base
                .as_ref()
                .and_then(|base| base.ptr_const_map.get(pointee).copied())
        })
    }

    fn lookup_ptr_mut(&self, pointee: &Type) -> Option<Type> {
        self.ptr_mut_map.get(pointee).copied().or_else(|| {
            self.base
                .as_ref()
                .and_then(|base| base.ptr_mut_map.get(pointee).copied())
        })
    }

    fn lookup_struct_by_file_name(&self, key: &(FileId, Spur)) -> Option<Type> {
        self.struct_by_file_name.get(key).copied().or_else(|| {
            self.base
                .as_ref()
                .and_then(|base| base.struct_by_file_name.get(key).copied())
        })
    }

    fn lookup_enum_by_file_name(&self, key: &(FileId, Spur)) -> Option<Type> {
        self.enum_by_file_name.get(key).copied().or_else(|| {
            self.base
                .as_ref()
                .and_then(|base| base.enum_by_file_name.get(key).copied())
        })
    }

    /// Collapse the base and this epoch's overlay into one flat universe.
    ///
    /// Used where a layered representation cannot be carried further: freezing
    /// the pool for post-semantic phases, and re-basing a derived pool so every
    /// overlay stays exactly one level deep.
    fn flatten(&self) -> Self {
        let Some(base) = self.base.as_ref() else {
            return self.clone();
        };
        let mut flat = (**base).clone();
        for (&index, entry) in &self.overrides {
            flat.types[index] = entry.clone();
        }
        for (&index, facts) in &self.override_facts {
            flat.containment_facts[index] = *facts;
        }
        flat.types.extend(self.types.iter().cloned());
        flat.containment_facts
            .extend(self.containment_facts.iter().copied());
        // Flattening already walks every entry, so the pending count is
        // recomputed exactly instead of merging the two layers' counters
        // (an override may have completed a base entry counted by the base).
        flat.pending_facts = flat
            .containment_facts
            .iter()
            .filter(|facts| facts.is_none())
            .count();
        flat.facts_stale |= self.facts_stale;
        flat.unsettled_facts |= self.unsettled_facts;
        flat.containment_metrics.finalize_checks += self.containment_metrics.finalize_checks;
        flat.containment_metrics.nodes += self.containment_metrics.nodes;
        flat.containment_metrics.edges += self.containment_metrics.edges;
        flat.array_map.extend(self.array_map.iter());
        flat.ptr_const_map.extend(self.ptr_const_map.iter());
        flat.ptr_mut_map.extend(self.ptr_mut_map.iter());
        flat.anonymous_structs
            .extend(self.anonymous_structs.iter().copied());
        flat.anonymous_enums
            .extend(self.anonymous_enums.iter().copied());
        flat.struct_by_file_name
            .extend(self.struct_by_file_name.iter());
        flat.enum_by_file_name.extend(self.enum_by_file_name.iter());
        flat.symbol_paths = self.symbol_paths.clone();
        flat.struct_lang_items = self.struct_lang_items.clone();
        flat.lang_item_structs = self.lang_item_structs.clone();
        flat.repr_c_structs = self.repr_c_structs.clone();
        flat.capacity_exceeded |= self.capacity_exceeded;
        flat
    }

    /// A fresh universe that reads this one as an immutable base and appends its
    /// own entries above it (RUE-1135).
    ///
    /// Deriving from an already-sealed universe — one that has a base and has
    /// appended nothing of its own — shares that base's `Arc` and copies
    /// nothing, which is what makes every per-body derivation O(1). Deriving
    /// from an unsealed or grown universe materializes one flat base first;
    /// finalizing containment metadata before body analysis is what keeps
    /// repeated derivation on the shared-base path.
    fn derive_overlay(&self) -> Self {
        let untouched =
            self.types.is_empty() && self.overrides.is_empty() && self.override_facts.is_empty();
        let base = match (&self.base, untouched) {
            (Some(base), true) => Arc::clone(base),
            (Some(_), false) => Arc::new(self.flatten()),
            (None, _) => Arc::new(self.clone()),
        };
        let pending_facts = base.pending_facts;
        let facts_stale = base.facts_stale;
        let unsettled_facts = base.unsettled_facts;
        let capacity_exceeded = base.capacity_exceeded;
        Self {
            base_len: base.entry_count(),
            base: Some(base),
            overrides: HashMap::new(),
            override_facts: HashMap::new(),
            types: Vec::new(),
            array_map: HashMap::new(),
            ptr_const_map: HashMap::new(),
            ptr_mut_map: HashMap::new(),
            containment_facts: Vec::new(),
            pending_facts,
            facts_stale,
            unsettled_facts,
            containment_metrics: TypeContainmentMetrics::default(),
            // Empty locally; `is_anonymous_*` consults the base for entries
            // below `base_len`, matching how `types` is layered.
            anonymous_structs: HashSet::new(),
            anonymous_enums: HashSet::new(),
            struct_by_file_name: HashMap::new(),
            enum_by_file_name: HashMap::new(),
            symbol_paths: Arc::clone(&self.symbol_paths),
            struct_lang_items: Arc::clone(&self.struct_lang_items),
            lang_item_structs: Arc::clone(&self.lang_item_structs),
            repr_c_structs: Arc::clone(&self.repr_c_structs),
            capacity_exceeded,
        }
    }

    /// Whether this struct was generated for an anonymous type, consulting the
    /// shared base for entries below `base_len`.
    fn is_anonymous_struct(&self, id: StructId) -> bool {
        self.anonymous_structs.contains(&id)
            || self
                .base
                .as_ref()
                .is_some_and(|base| base.is_anonymous_struct(id))
    }

    /// Enum counterpart of [`Self::is_anonymous_struct`].
    fn is_anonymous_enum(&self, id: EnumId) -> bool {
        self.anonymous_enums.contains(&id)
            || self
                .base
                .as_ref()
                .is_some_and(|base| base.is_anonymous_enum(id))
    }

    /// The pool index the next registration will occupy.
    ///
    /// Once the 24-bit `Type` payload is exhausted there is no representable
    /// index left, so this latches [`Self::capacity_exceeded`] and hands back
    /// the final legal index instead of panicking. `push_entry` then refuses to
    /// grow, so the pool stops changing and every later registration resolves to
    /// that same entry; semantic analysis reports `E1401` at its next boundary
    /// and nothing built from the pool is published (spec C.1:2). Reusing an
    /// existing, fully formed index — rather than fabricating one — keeps every
    /// pool read in range; the public accessors additionally re-check the kind
    /// tag against the entry, so an aliased handle degrades to `None` rather
    /// than to a mistyped entry.
    ///
    /// # The latch window
    ///
    /// The latch is set inside declaration collection, type resolution, or
    /// specialization, and the diagnostic is reported at the declaration-binding
    /// boundary (with the per-body CFG query as a backstop for a universe that
    /// latches later). Registration continues in between (spec C.1:2 forbids an
    /// abort), so aliased handles are *read* inside that window — most
    /// immediately by `incremental_facts`, which walks the field types of the
    /// very entry being registered, and then by every layout, ABI, and
    /// containment query the remaining declarations trigger. An aliased handle
    /// carries the kind tag its registration asked for while the entry it names
    /// keeps the kind it was created with, so the `&`-returning definition
    /// accessors below cannot assert the two agree; inside the window they
    /// degrade to an empty definition of the requested kind
    /// ([`ALIASED_STRUCT_DEF`], [`ALIASED_ENUM_DEF`]) instead of panicking. A
    /// kind mismatch with no latch is still a producer bug and still panics.
    fn next_pool_index(&mut self) -> u32 {
        match checked_pool_index(self.entry_count()) {
            Some(index) => index,
            None => {
                self.capacity_exceeded = true;
                type_encoding::MAX_PAYLOAD
            }
        }
    }

    /// Whether this universe (or the base it reads) ran past the published
    /// composite-type ceiling.
    fn capacity_exceeded(&self) -> bool {
        self.capacity_exceeded
            || self
                .base
                .as_ref()
                .is_some_and(|base| base.capacity_exceeded())
    }

    fn by_value_child_index(&self, ty: Type) -> Option<usize> {
        match ty.kind() {
            TypeKind::Struct(id) => Some(id.pool_index() as usize),
            TypeKind::Enum(id) => Some(id.pool_index() as usize),
            TypeKind::Array(id) => Some(id.pool_index() as usize),
            _ => None,
        }
    }

    fn containment_edges(&self) -> Vec<Vec<usize>> {
        (0..self.entry_count())
            .map(|index| match self.entry(index) {
                TypeData::Struct(data) => data
                    .def
                    .fields
                    .iter()
                    .filter_map(|field| self.by_value_child_index(field.ty))
                    .collect(),
                TypeData::Enum(data) => data
                    .def
                    .variant_payloads
                    .iter()
                    .flatten()
                    .filter_map(|&ty| self.by_value_child_index(ty))
                    .collect(),
                // Preserve the language's recursive-type diagnostic even for
                // zero-length arrays: arrays are inline structural edges. The
                // fact fold below gives a zero-length node zero ownership
                // multiplicity, so it carries neither linearity nor drop glue.
                TypeData::Array { element, .. } => {
                    self.by_value_child_index(*element).into_iter().collect()
                }
                TypeData::ReservedStruct
                | TypeData::DeclaredStruct(_)
                | TypeData::DeclaredEnum(_)
                | TypeData::PtrConst { .. }
                | TypeData::PtrMut { .. } => Vec::new(),
            })
            .collect()
    }

    fn containment_cycle_path(&self, path: &[usize], repeated: usize) -> Vec<String> {
        path.iter()
            .copied()
            .chain(std::iter::once(repeated))
            .filter_map(|index| match self.entry(index) {
                TypeData::Struct(data) => Some(data.def.name.to_string()),
                TypeData::Enum(data) => Some(data.def.name.to_string()),
                TypeData::Array { .. }
                | TypeData::PtrConst { .. }
                | TypeData::PtrMut { .. }
                | TypeData::ReservedStruct
                | TypeData::DeclaredStruct(_)
                | TypeData::DeclaredEnum(_) => None,
            })
            .collect()
    }

    fn type_for_index(&self, index: usize) -> Type {
        match self.entry(index) {
            TypeData::Struct(_) | TypeData::DeclaredStruct(_) => {
                Type::new_struct(StructId::from_pool_index(index as u32))
            }
            TypeData::Enum(_) | TypeData::DeclaredEnum(_) => {
                Type::new_enum(EnumId::from_pool_index(index as u32))
            }
            TypeData::Array { .. } => Type::new_array(ArrayTypeId::from_pool_index(index as u32)),
            TypeData::PtrConst { .. } => {
                Type::new_ptr_const(PtrConstTypeId::from_pool_index(index as u32))
            }
            TypeData::PtrMut { .. } => {
                Type::new_ptr_mut(PtrMutTypeId::from_pool_index(index as u32))
            }
            TypeData::ReservedStruct => Type::new_struct(StructId::from_pool_index(index as u32)),
        }
    }

    /// Compute cycle, ownership, and ABI-width facts from the one canonical
    /// by-value graph. The explicit DFS stack makes both cycle detection and
    /// postorder construction independent of the host call stack.
    fn finalize_containment_metadata(
        &mut self,
    ) -> Result<TypeContainmentWork, TypeContainmentCycle> {
        self.containment_metrics.finalize_checks += 1;
        debug_assert_eq!(self.types.len(), self.containment_facts.len());
        let entry_count = self.entry_count();
        // New complete entries derive their facts incrementally from already
        // finalized children. If every entry is available, no containment edge
        // changed and a repeated endpoint finalization is a true O(1) no-op.
        // Entries without facts (shells, reserved slots, completions with an
        // unavailable child) and destructor mutation force the canonical full
        // pass below exactly when graph-wide propagation may be required.
        // Factless survivors of the last pass alone do not: the pass already
        // proved them uncomputable, and nothing turned factless since
        // (`unsettled_facts`), so re-walking the graph would change nothing.
        let pass_required = self.facts_stale || (self.pending_facts > 0 && self.unsettled_facts);
        if !pass_required {
            return Ok(TypeContainmentWork::default());
        }
        let edges = self.containment_edges();
        let work = TypeContainmentWork {
            nodes: edges.len(),
            edges: edges.iter().map(Vec::len).sum(),
        };
        self.containment_metrics.nodes += work.nodes;
        self.containment_metrics.edges += work.edges;
        let mut color = vec![0u8; edges.len()];
        let mut postorder = Vec::with_capacity(edges.len());
        let mut path = Vec::new();

        for root in 0..edges.len() {
            if color[root] != 0 {
                continue;
            }
            color[root] = 1;
            path.push(root);
            let mut stack = vec![(root, 0usize)];
            while let Some((node, next_child)) = stack.last_mut() {
                if let Some(&child) = edges[*node].get(*next_child) {
                    *next_child += 1;
                    match color[child] {
                        0 => {
                            color[child] = 1;
                            path.push(child);
                            stack.push((child, 0));
                        }
                        1 => {
                            return Err(TypeContainmentCycle {
                                root: self.type_for_index(root),
                                path: self.containment_cycle_path(&path, child),
                            });
                        }
                        2 => {}
                        _ => unreachable!("containment DFS color"),
                    }
                } else {
                    let (finished, _) = stack.pop().expect("non-empty DFS stack");
                    let popped = path.pop().expect("DFS path matches stack");
                    debug_assert_eq!(finished, popped);
                    color[finished] = 2;
                    postorder.push(finished);
                }
            }
        }

        // Keep incomplete declaration shells, and complete types containing
        // them, fail-closed. The loop deliberately skips any node without an
        // exact by-value definition; availability is tracked separately so
        // write-back cannot turn that absence into semantic defaults.
        let mut facts = vec![TypeContainmentFacts::default(); entry_count];
        let mut facts_available = vec![false; entry_count];
        for &index in &postorder {
            let mut value = match self.entry(index) {
                TypeData::Struct(data) => TypeContainmentFacts {
                    carries_linear: data.def.is_linear,
                    needs_drop: data.def.destructor.is_some(),
                },
                TypeData::Enum(_)
                | TypeData::Array { .. }
                | TypeData::PtrConst { .. }
                | TypeData::PtrMut { .. } => TypeContainmentFacts::default(),
                TypeData::ReservedStruct
                | TypeData::DeclaredStruct(_)
                | TypeData::DeclaredEnum(_) => continue,
            };
            let has_values = !matches!(self.entry(index), TypeData::Array { len: 0, .. });
            let mut available = true;
            if has_values {
                for &child in &edges[index] {
                    if !facts_available[child] {
                        available = false;
                        break;
                    }
                    value.carries_linear |= facts[child].carries_linear;
                    value.needs_drop |= facts[child].needs_drop;
                }
            }
            if available {
                facts[index] = value;
                facts_available[index] = true;
            }
        }

        // The same postorder is the canonical dynamic-programming order for
        // flattened ABI widths. Store each result directly in the padding of
        // its type entry, so frozen consumers get O(1) reads without a second
        // per-universe side table or a larger retained entry.
        for &index in &postorder {
            if !facts_available[index] {
                continue;
            }
            let child_slots = |ty: Type| {
                if let Some(child) = self.by_value_child_index(ty) {
                    facts_available[child].then(|| self.entry_abi_slots(child))
                } else {
                    Some(Self::leaf_derived_facts(ty).abi_slots)
                }
            };
            let abi_slots = match self.entry(index) {
                TypeData::Struct(data) => data.def.fields.iter().try_fold(0u32, |total, field| {
                    Some(total.saturating_add(child_slots(field.ty)?))
                }),
                TypeData::Enum(data) => {
                    let mut max_payload = 0u32;
                    let mut available = true;
                    for variant in &data.def.variant_payloads {
                        let mut payload = 0u32;
                        for &ty in variant {
                            let Some(slots) = child_slots(ty) else {
                                available = false;
                                break;
                            };
                            payload = payload.saturating_add(slots);
                        }
                        if !available {
                            break;
                        }
                        max_payload = max_payload.max(payload);
                    }
                    available.then(|| 1u32.saturating_add(max_payload))
                }
                TypeData::Array { element, len, .. } => {
                    if *len == 0 {
                        Some(0)
                    } else {
                        child_slots(*element).map(|slots| {
                            u32::try_from(u64::from(slots).saturating_mul(*len)).unwrap_or(u32::MAX)
                        })
                    }
                }
                TypeData::PtrConst { .. } | TypeData::PtrMut { .. } => Some(1),
                TypeData::ReservedStruct
                | TypeData::DeclaredStruct(_)
                | TypeData::DeclaredEnum(_) => None,
            };
            if let Some(abi_slots) = abi_slots {
                self.set_entry_abi_slots(index, abi_slots);
            }
        }

        for (index, value) in facts.iter().copied().enumerate() {
            if facts_available[index]
                && value.carries_linear
                && let TypeData::Struct(data) = self.entry_mut(index)
            {
                Arc::make_mut(&mut data.def).metadata_mut().is_linear = true;
            }
        }
        let mut still_pending = 0usize;
        for (index, value) in facts.into_iter().enumerate() {
            let available = facts_available[index];
            if !available {
                still_pending += 1;
            }
            self.set_facts(index, available.then_some(value));
        }
        // The pass consumed every mutation recorded so far: present facts are
        // exact again, and the pending count is exactly what the pass could
        // not compute (incomplete shells and their by-value ancestors).
        self.pending_facts = still_pending;
        self.facts_stale = false;
        self.unsettled_facts = false;
        Ok(work)
    }

    fn facts_for_type(&self, ty: Type) -> Option<TypeContainmentFacts> {
        // A stale pool may hold facts that are present but wrong (a destructor
        // was attached after they propagated), so it refuses every composite
        // read until the full pass repairs it. A merely *incomplete* pool does
        // not: a stored fact is exact the moment it is written — incremental
        // derivation only consumes children whose own facts are available —
        // and entries without facts fail closed individually below.
        if self.facts_stale
            && matches!(
                ty.kind(),
                TypeKind::Struct(_) | TypeKind::Enum(_) | TypeKind::Array(_)
            )
        {
            return None;
        }
        match ty.kind() {
            TypeKind::Struct(id) => self.facts_at(id.pool_index() as usize)?,
            TypeKind::Enum(id) => self.facts_at(id.pool_index() as usize)?,
            TypeKind::Array(id) => self.facts_at(id.pool_index() as usize)?,
            _ => Some(TypeContainmentFacts::default()),
        }
    }

    fn leaf_derived_facts(ty: Type) -> TypeDerivedFacts {
        let abi_slots = match ty.kind() {
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Error
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_) => 1,
            TypeKind::Unit
            | TypeKind::Never
            | TypeKind::ComptimeType
            | TypeKind::Module(_)
            | TypeKind::Struct(_)
            | TypeKind::Enum(_)
            | TypeKind::Array(_) => 0,
        };
        TypeDerivedFacts {
            abi_slots,
            ..TypeDerivedFacts::default()
        }
    }

    fn derived_facts_for_type(&self, ty: Type) -> Option<TypeDerivedFacts> {
        Some(TypeDerivedFacts {
            containment: self.facts_for_type(ty)?,
            abi_slots: self.stored_abi_slot_count(ty)?,
        })
    }

    fn stored_abi_slot_count(&self, ty: Type) -> Option<u32> {
        // Same refusal shape as `facts_for_type`: a stale pool refuses
        // globally. An entry's stored width is written together with its
        // containment facts (by incremental derivation or the full pass), so
        // fact availability is also the width's per-entry availability marker;
        // without it the field still holds the meaningless placeholder `0`.
        if self.facts_stale
            && matches!(
                ty.kind(),
                TypeKind::Struct(_) | TypeKind::Enum(_) | TypeKind::Array(_)
            )
        {
            return None;
        }
        match ty.kind() {
            TypeKind::Struct(id) => match self.try_entry(id.pool_index() as usize)? {
                TypeData::Struct(data) => {
                    self.facts_at(id.pool_index() as usize)??;
                    Some(data.abi_slots)
                }
                _ => None,
            },
            TypeKind::Enum(id) => match self.try_entry(id.pool_index() as usize)? {
                TypeData::Enum(data) => {
                    self.facts_at(id.pool_index() as usize)??;
                    Some(data.abi_slots)
                }
                _ => None,
            },
            TypeKind::Array(id) => match self.try_entry(id.pool_index() as usize)? {
                TypeData::Array { abi_slots, .. } => {
                    self.facts_at(id.pool_index() as usize)??;
                    Some(*abi_slots)
                }
                _ => None,
            },
            _ => Some(Self::leaf_derived_facts(ty).abi_slots),
        }
    }

    fn entry_abi_slots(&self, index: usize) -> u32 {
        match self.entry(index) {
            TypeData::Struct(data) => data.abi_slots,
            TypeData::Enum(data) => data.abi_slots,
            TypeData::Array { abi_slots, .. } => *abi_slots,
            TypeData::PtrConst { .. } | TypeData::PtrMut { .. } => 1,
            TypeData::ReservedStruct | TypeData::DeclaredStruct(_) | TypeData::DeclaredEnum(_) => {
                unreachable!("unavailable child has no derived ABI width")
            }
        }
    }

    fn set_entry_abi_slots(&mut self, index: usize, abi_slots: u32) {
        let current = match self.entry(index) {
            TypeData::Struct(data) => Some(data.abi_slots),
            TypeData::Enum(data) => Some(data.abi_slots),
            TypeData::Array { abi_slots, .. } => Some(*abi_slots),
            TypeData::PtrConst { .. } | TypeData::PtrMut { .. } => return,
            TypeData::ReservedStruct | TypeData::DeclaredStruct(_) | TypeData::DeclaredEnum(_) => {
                return;
            }
        };
        if current == Some(abi_slots) {
            return;
        }
        match self.entry_mut(index) {
            TypeData::Struct(data) => data.abi_slots = abi_slots,
            TypeData::Enum(data) => data.abi_slots = abi_slots,
            TypeData::Array {
                abi_slots: stored, ..
            } => *stored = abi_slots,
            TypeData::PtrConst { .. }
            | TypeData::PtrMut { .. }
            | TypeData::ReservedStruct
            | TypeData::DeclaredStruct(_)
            | TypeData::DeclaredEnum(_) => unreachable!(),
        }
    }

    fn incremental_facts(&self, entry: &TypeData) -> Option<TypeDerivedFacts> {
        // Only a stale pool refuses incremental derivation globally: its
        // present facts may be wrong, and consuming one would launder the
        // staleness into a fresh entry. Entries *without* facts do not poison
        // the pool as a whole — every child fact consumed below goes through
        // `derived_facts_for_type`, which returns `None` for a factless child
        // (its containment facts and stored width are gated on the same
        // per-entry availability), so a derivation over unavailable inputs
        // fails closed here and the entry waits for the canonical full pass.
        // This is what lets a declare→complete pair settle its own facts even
        // while the entry being completed is itself still counted pending.
        if self.facts_stale {
            return None;
        }
        let mut facts = match entry {
            TypeData::Struct(data) => TypeDerivedFacts {
                containment: TypeContainmentFacts {
                    carries_linear: data.def.is_linear,
                    needs_drop: data.def.destructor.is_some(),
                },
                abi_slots: 0,
            },
            TypeData::Enum(_) => TypeDerivedFacts {
                abi_slots: 1,
                ..TypeDerivedFacts::default()
            },
            TypeData::Array { .. } => TypeDerivedFacts::default(),
            TypeData::PtrConst { .. } | TypeData::PtrMut { .. } => TypeDerivedFacts {
                abi_slots: 1,
                ..TypeDerivedFacts::default()
            },
            TypeData::ReservedStruct | TypeData::DeclaredStruct(_) | TypeData::DeclaredEnum(_) => {
                return None;
            }
        };
        match entry {
            TypeData::Struct(data) => {
                for field in &data.def.fields {
                    let child = self.derived_facts_for_type(field.ty)?;
                    facts.containment.carries_linear |= child.containment.carries_linear;
                    facts.containment.needs_drop |= child.containment.needs_drop;
                    facts.abi_slots = facts.abi_slots.saturating_add(child.abi_slots);
                }
            }
            TypeData::Enum(data) => {
                let mut max_payload_slots = 0u32;
                for variant in &data.def.variant_payloads {
                    let mut payload_slots = 0u32;
                    for &ty in variant {
                        let child = self.derived_facts_for_type(ty)?;
                        facts.containment.carries_linear |= child.containment.carries_linear;
                        facts.containment.needs_drop |= child.containment.needs_drop;
                        payload_slots = payload_slots.saturating_add(child.abi_slots);
                    }
                    max_payload_slots = max_payload_slots.max(payload_slots);
                }
                facts.abi_slots = facts.abi_slots.saturating_add(max_payload_slots);
            }
            TypeData::Array { element, len, .. } if *len != 0 => {
                let child = self.derived_facts_for_type(*element)?;
                facts.containment = child.containment;
                facts.abi_slots = u32::try_from(u64::from(child.abi_slots).saturating_mul(*len))
                    .unwrap_or(u32::MAX);
            }
            TypeData::Array { .. } => {}
            TypeData::PtrConst { .. } | TypeData::PtrMut { .. } => {}
            TypeData::ReservedStruct | TypeData::DeclaredStruct(_) | TypeData::DeclaredEnum(_) => {
                unreachable!()
            }
        }
        Some(facts)
    }

    fn invalidate_containment_metadata(&mut self) {
        self.facts_stale = true;
    }

    #[inline]
    fn data(&self, index: u32) -> &TypeData {
        self.entry(index as usize)
    }

    fn try_struct_def(&self, id: StructId) -> Option<&StructDefEntry> {
        self.try_struct_def_arc(id).map(Arc::as_ref)
    }

    /// The shared handle behind a completed struct definition.
    ///
    /// Callers that cannot hold a borrow — the mutable pool's accessors, which
    /// would otherwise leak `RwLock` scope — clone this handle instead of the
    /// definition it points at.
    fn try_struct_def_arc(&self, id: StructId) -> Option<&Arc<StructDefEntry>> {
        match self.try_entry(id.0 as usize)? {
            TypeData::Struct(data) => Some(&data.def),
            _ => None,
        }
    }

    /// Declaration resolution may inspect metadata, but never fields, from a
    /// nominal shell. Definition, layout, durable, and backend reads use the
    /// complete-only helpers above and below.
    fn struct_declaration_metadata(&self, id: StructId) -> Option<StructDeclarationMetadata> {
        match self.try_entry(id.0 as usize)? {
            TypeData::DeclaredStruct(data) => Some(StructDeclarationMetadata {
                name: data.def.name.clone(),
                is_copy: data.def.is_copy,
                is_linear: data.def.is_linear,
                destructor: data.def.destructor.clone(),
                is_builtin: data.def.is_builtin,
                is_pub: data.def.is_pub,
                file_id: data.def.file_id,
            }),
            _ => None,
        }
    }

    fn struct_metadata(&self, id: StructId) -> Option<StructDeclarationMetadata> {
        match self.try_entry(id.0 as usize)? {
            TypeData::DeclaredStruct(data) | TypeData::Struct(data) => {
                Some(StructDeclarationMetadata {
                    name: data.def.name.clone(),
                    is_copy: data.def.is_copy,
                    is_linear: data.def.is_linear,
                    destructor: data.def.destructor.clone(),
                    is_builtin: data.def.is_builtin,
                    is_pub: data.def.is_pub,
                    file_id: data.def.file_id,
                })
            }
            _ => None,
        }
    }

    fn struct_def(&self, id: StructId) -> &StructDefEntry {
        match self.try_struct_def(id) {
            Some(def) => def,
            // Only a capacity-latched pool can hand out a struct handle that
            // names a non-struct entry; see `next_pool_index`. Anywhere else a
            // kind mismatch is a producer bug and must stay loud.
            None if self.capacity_exceeded() => &ALIASED_STRUCT_DEF,
            None => panic!("Expected struct at pool index {}", id.0),
        }
    }

    fn struct_def_mut(&mut self, id: StructId) -> &mut StructDef {
        let pool_index = id.pool_index() as usize;
        match self.try_entry_mut(pool_index) {
            Some(TypeData::Struct(data)) => Arc::make_mut(&mut data.def).metadata_mut(),
            other => panic!(
                "Expected complete struct at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    fn try_enum_def(&self, id: EnumId) -> Option<&EnumDefEntry> {
        self.try_enum_def_arc(id).map(Arc::as_ref)
    }

    /// The shared handle behind a completed enum definition. See
    /// [`TypeInternPoolInner::try_struct_def_arc`].
    fn try_enum_def_arc(&self, id: EnumId) -> Option<&Arc<EnumDefEntry>> {
        match self.try_entry(id.0 as usize)? {
            TypeData::Enum(data) => Some(&data.def),
            _ => None,
        }
    }

    fn enum_declaration_metadata(&self, id: EnumId) -> Option<EnumDeclarationMetadata> {
        match self.try_entry(id.0 as usize)? {
            TypeData::DeclaredEnum(data) => Some(EnumDeclarationMetadata {
                name: data.def.name.clone(),
                variants: data.def.variants.clone(),
                is_pub: data.def.is_pub,
                file_id: data.def.file_id,
            }),
            _ => None,
        }
    }

    fn enum_metadata(&self, id: EnumId) -> Option<EnumDeclarationMetadata> {
        match self.try_entry(id.0 as usize)? {
            TypeData::DeclaredEnum(data) | TypeData::Enum(data) => Some(EnumDeclarationMetadata {
                name: data.def.name.clone(),
                variants: data.def.variants.clone(),
                is_pub: data.def.is_pub,
                file_id: data.def.file_id,
            }),
            _ => None,
        }
    }

    fn enum_def(&self, id: EnumId) -> &EnumDefEntry {
        match self.try_enum_def(id) {
            Some(def) => def,
            // See `struct_def`: an aliased handle is only possible inside the
            // capacity-latch window.
            None if self.capacity_exceeded() => &ALIASED_ENUM_DEF,
            None => panic!("Expected enum at pool index {}", id.0),
        }
    }

    fn array_def(&self, id: ArrayTypeId) -> (Type, u64) {
        match self.data(id.0) {
            TypeData::Array { element, len, .. } => (*element, *len),
            // See `struct_def`: inside the capacity-latch window an array
            // handle can name an entry of another kind. A zero-length array of
            // the error type terminates every dependent walk.
            _ if self.capacity_exceeded() => (Type::ERROR, 0),
            other => panic!("Expected array at pool index {}, got {:?}", id.0, other),
        }
    }

    fn try_array_def(&self, id: ArrayTypeId) -> Option<(Type, u64)> {
        match self.try_entry(id.0 as usize)? {
            TypeData::Array { element, len, .. } => Some((*element, *len)),
            _ => None,
        }
    }

    fn ptr_const_def(&self, id: PtrConstTypeId) -> Type {
        match self.data(id.pool_index()) {
            TypeData::PtrConst { pointee } => *pointee,
            // See `struct_def`: aliasing is confined to the capacity-latch
            // window, and the error type terminates the pointee walk.
            _ if self.capacity_exceeded() => Type::ERROR,
            other => panic!(
                "Expected ptr const at pool index {}, got {:?}",
                id.pool_index(),
                other
            ),
        }
    }

    fn ptr_mut_def(&self, id: PtrMutTypeId) -> Type {
        match self.data(id.pool_index()) {
            TypeData::PtrMut { pointee } => *pointee,
            // See `ptr_const_def`.
            _ if self.capacity_exceeded() => Type::ERROR,
            other => panic!(
                "Expected ptr mut at pool index {}, got {:?}",
                id.pool_index(),
                other
            ),
        }
    }

    fn validate_structural_child(&self, ty: Type) -> Result<(), TypeValidationError> {
        self.validate_type_inner(
            ty,
            ValidationMode::StructuralChild,
            &mut TypeVisitSet::new(),
        )
    }

    fn validate_complete_type(&self, ty: Type) -> Result<(), TypeValidationError> {
        self.validate_type_inner(ty, ValidationMode::Complete, &mut TypeVisitSet::new())
    }

    /// Validate the complete immutable universe in one graph traversal.
    ///
    /// Reusing one visit set across roots is important: starting a fresh walk
    /// for every entry makes a chain of aggregate types quadratic even though
    /// each node and edge needs to be checked only once.
    fn validate_for_success(&self) -> Result<(), TypeValidationError> {
        let mut visited = TypeVisitSet::new();
        for (index, entry) in self.types.iter().enumerate() {
            assert!(self.base.is_none(), "a frozen pool is flat");
            let index = checked_pool_index(index).expect("type pool index invariant");
            let ty = match entry {
                TypeData::Struct(_) => Type::new_struct(StructId::from_pool_index(index)),
                TypeData::Enum(_) => Type::new_enum(EnumId::from_pool_index(index)),
                TypeData::Array { .. } => Type::new_array(ArrayTypeId::from_pool_index(index)),
                TypeData::PtrConst { .. } => {
                    Type::new_ptr_const(PtrConstTypeId::from_pool_index(index))
                }
                TypeData::PtrMut { .. } => Type::new_ptr_mut(PtrMutTypeId::from_pool_index(index)),
                TypeData::ReservedStruct
                | TypeData::DeclaredStruct(_)
                | TypeData::DeclaredEnum(_) => {
                    return Err(TypeValidationError::IncompleteDefinition);
                }
            };
            self.validate_type_inner(ty, ValidationMode::Complete, &mut visited)?;
        }
        Ok(())
    }

    /// Validate a query's root handle without rewalking a frozen pool's
    /// already-validated graph. This catches invalid encodings, out-of-range
    /// indices, and wrong-kind compact handles while keeping canonical fact
    /// queries O(1) and independent of the host call stack.
    fn validate_complete_root(&self, ty: Type) -> Result<(), TypeValidationError> {
        let kind = ty.try_kind().ok_or(TypeValidationError::InvalidEncoding)?;
        let (index, expected) = match kind {
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Unit
            | TypeKind::Never
            | TypeKind::ComptimeType
            | TypeKind::Module(_) => return Ok(()),
            TypeKind::Error => return Err(TypeValidationError::RecoveryType),
            TypeKind::Struct(id) => (id.pool_index(), PoolEntryKind::Struct),
            TypeKind::Enum(id) => (id.pool_index(), PoolEntryKind::Enum),
            TypeKind::Array(id) => (id.pool_index(), PoolEntryKind::Array),
            TypeKind::PtrConst(id) => (id.pool_index(), PoolEntryKind::PtrConst),
            TypeKind::PtrMut(id) => (id.pool_index(), PoolEntryKind::PtrMut),
        };
        let entry = self
            .try_entry(index as usize)
            .ok_or(TypeValidationError::PoolIndexOutOfRange)?;
        if entry.kind() != expected {
            return if matches!(entry, TypeData::ReservedStruct) {
                Err(TypeValidationError::ReservedEntry)
            } else {
                Err(TypeValidationError::KindMismatch)
            };
        }
        if matches!(
            entry,
            TypeData::DeclaredStruct(_) | TypeData::DeclaredEnum(_)
        ) {
            return Err(TypeValidationError::IncompleteDefinition);
        }
        Ok(())
    }

    fn validate_type_inner(
        &self,
        ty: Type,
        mode: ValidationMode,
        visited: &mut TypeVisitSet,
    ) -> Result<(), TypeValidationError> {
        let kind = ty.try_kind().ok_or(TypeValidationError::InvalidEncoding)?;
        match kind {
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Unit
            | TypeKind::Never => return Ok(()),
            TypeKind::Error => {
                return if mode.requires_complete() {
                    Err(TypeValidationError::RecoveryType)
                } else {
                    Ok(())
                };
            }
            TypeKind::ComptimeType => {
                return if mode.is_structural_child() {
                    Err(TypeValidationError::ComptimeStructuralChild)
                } else {
                    Ok(())
                };
            }
            TypeKind::Module(_) => {
                return if mode.is_structural_child() {
                    Err(TypeValidationError::ModuleStructuralChild)
                } else {
                    Ok(())
                };
            }
            _ => {}
        }

        if !visited.insert(ty) {
            return Ok(());
        }

        let (index, expected) = match kind {
            TypeKind::Struct(id) => (id.pool_index(), PoolEntryKind::Struct),
            TypeKind::Enum(id) => (id.pool_index(), PoolEntryKind::Enum),
            TypeKind::Array(id) => (id.pool_index(), PoolEntryKind::Array),
            TypeKind::PtrConst(id) => (id.pool_index(), PoolEntryKind::PtrConst),
            TypeKind::PtrMut(id) => (id.pool_index(), PoolEntryKind::PtrMut),
            _ => unreachable!("primitive and non-pool kinds returned above"),
        };
        let entry = self
            .try_entry(index as usize)
            .ok_or(TypeValidationError::PoolIndexOutOfRange)?;
        if entry.kind() != expected {
            return if matches!(entry, TypeData::ReservedStruct) {
                Err(TypeValidationError::ReservedEntry)
            } else {
                Err(TypeValidationError::KindMismatch)
            };
        }

        match entry {
            TypeData::ReservedStruct => Err(TypeValidationError::ReservedEntry),
            TypeData::DeclaredStruct(_) | TypeData::DeclaredEnum(_) => {
                if mode.requires_complete() {
                    Err(TypeValidationError::IncompleteDefinition)
                } else {
                    Ok(())
                }
            }
            TypeData::Struct(data) => data
                .def
                .fields
                .iter()
                .try_for_each(|field| self.validate_type_inner(field.ty, mode.child(), visited)),
            TypeData::Enum(data) => data
                .def
                .variant_payloads
                .iter()
                .flatten()
                .try_for_each(|&child| self.validate_type_inner(child, mode.child(), visited)),
            TypeData::Array { element, .. } => {
                self.validate_type_inner(*element, mode.child(), visited)
            }
            TypeData::PtrConst { pointee } | TypeData::PtrMut { pointee } => {
                self.validate_type_inner(*pointee, mode.child(), visited)
            }
        }
    }

    fn abi_slot_count(&self, ty: Type) -> u32 {
        // Complete type universes derive widths together with ownership facts
        // from the canonical by-value DAG. Provisional semantic construction
        // can still reach this method while that metadata is dirty, so retain
        // the recursive computation as a fail-safe for that phase only.
        if let Some(abi_slots) = self.stored_abi_slot_count(ty) {
            return abi_slots;
        }
        self.compute_abi_slot_count(ty)
    }

    fn compute_abi_slot_count(&self, ty: Type) -> u32 {
        match ty.kind() {
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Error
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_) => 1,
            TypeKind::Unit | TypeKind::Never | TypeKind::ComptimeType | TypeKind::Module(_) => 0,
            TypeKind::Struct(id) => self.struct_def(id).fields.iter().fold(0, |total, field| {
                total.saturating_add(self.compute_abi_slot_count(field.ty))
            }),
            TypeKind::Array(id) => {
                let (element, length) = self.array_def(id);
                let slots = u64::from(self.compute_abi_slot_count(element));
                u32::try_from(slots.saturating_mul(length)).unwrap_or(u32::MAX)
            }
            TypeKind::Enum(id) => {
                let def = self.enum_def(id);
                let payload = (0..def.variant_count())
                    .map(|index| {
                        def.variant_payload(index).iter().fold(0u32, |total, &ty| {
                            total.saturating_add(self.compute_abi_slot_count(ty))
                        })
                    })
                    .max()
                    .unwrap_or(0);
                1u32.saturating_add(payload)
            }
        }
    }

    /// Byte offset of the field at `field_index` within `struct_id`: the field
    /// placement respecting natural alignment and interior padding. Shared by
    /// `@offset_of` and the layout authority so `@offset_of` and physical field
    /// addressing agree by construction. This physical byte offset is
    /// deliberately *not* the codegen slot offset (`struct_field_slot_offset`),
    /// which stays a slot-count index into the internal value representation
    /// (ADR-0052's three-representation split).
    fn struct_field_offset(&self, struct_id: StructId, field_index: u32) -> u64 {
        self.compact_struct_layout(struct_id)
            .field_offsets
            .get(field_index as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Byte offset of payload field `field_index` of `variant_index` within
    /// `enum_id`. The payload begins at the tag-plus-alignment offset and
    /// preceding fields are placed with natural alignment. This mirrors
    /// [`Self::layout`]'s [`LayoutKind::Enum`] `variants`. Like
    /// `struct_field_offset`, this physical byte offset is distinct from
    /// codegen's slot offset into the value representation.
    fn enum_payload_field_offset(
        &self,
        enum_id: EnumId,
        variant_index: u32,
        field_index: u32,
    ) -> u64 {
        let enum_layout = self.compact_enum_layout(enum_id);
        enum_layout
            .variants
            .get(variant_index as usize)
            .and_then(|fields| fields.get(field_index as usize))
            .copied()
            .unwrap_or(enum_layout.payload_offset)
    }

    /// Slot-count offset of struct field `field_index`: the summed
    /// [`Self::abi_slot_count`] of every preceding field. This is the *internal
    /// value decomposition* offset (ADR-0052 representation 2): code
    /// generation's stack/register slot model stays slot-based (RUE-975) even
    /// though the physical layout authority reports compact byte offsets, so
    /// this slot index and [`Self::struct_field_offset`]'s compact byte offset
    /// intentionally diverge.
    fn struct_field_slot_offset(&self, struct_id: StructId, field_index: u32) -> u32 {
        let fields = &self.struct_def(struct_id).fields;
        let mut slots = 0u32;
        for field in fields.iter().take(field_index as usize) {
            slots = slots.saturating_add(self.abi_slot_count(field.ty));
        }
        slots
    }

    /// Slot-count offset of enum payload field `field_index` of `variant_index`:
    /// the discriminant slot (1) plus the summed [`Self::abi_slot_count`] of the
    /// variant's preceding payload fields. Like [`Self::struct_field_slot_offset`]
    /// this is the compact-independent internal value-decomposition offset.
    fn enum_payload_slot_offset(
        &self,
        enum_id: EnumId,
        variant_index: u32,
        field_index: u32,
    ) -> u32 {
        let def = self.enum_def(enum_id);
        let mut slots = 1u32;
        for &field_ty in def
            .variant_payload(variant_index as usize)
            .iter()
            .take(field_index as usize)
        {
            slots = slots.saturating_add(self.abi_slot_count(field_ty));
        }
        slots
    }

    /// Compute the canonical physical [`Layout`] of `ty` (ADR-0052).
    ///
    /// `size` includes tail padding, `stride == size`, alignment is the natural
    /// alignment, and `kind` records the compact field offsets, element stride,
    /// tag width, and payload placement.
    fn layout(&self, ty: Type) -> Layout {
        self.compact_layout_of(ty)
    }

    // ----- Compact native layout (ADR-0052 "Resolved at acceptance") -----

    /// The compact `(size, alignment)` of `ty` under the natural LP64 table.
    ///
    /// Scalars use their byte width and natural alignment; `bool` is one byte;
    /// pointers and the error-recovery scalar are eight bytes, eight-aligned.
    /// Aggregates recurse through their struct/array/enum layout. `size` always
    /// includes tail padding and is a multiple of `alignment`, so it doubles as
    /// the array element stride. Any zero-sized result is normalized to
    /// alignment 1 and stride 0 (ADR-0052's uniform zero-sized-type rule, which
    /// includes zero-length arrays and all-zero-sized structs).
    fn compact_size_align(&self, ty: Type) -> (u64, u64) {
        let (size, alignment) = match ty.kind() {
            TypeKind::I8 | TypeKind::U8 | TypeKind::Bool => (1, 1),
            TypeKind::I16 | TypeKind::U16 => (2, 2),
            TypeKind::I32 | TypeKind::U32 => (4, 4),
            TypeKind::I64 | TypeKind::U64 => (8, 8),
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) | TypeKind::Error => (8, 8),
            TypeKind::Unit | TypeKind::Never | TypeKind::ComptimeType | TypeKind::Module(_) => {
                (0, 1)
            }
            TypeKind::Struct(id) => {
                let layout = self.compact_struct_layout(id);
                (layout.size, layout.alignment)
            }
            TypeKind::Array(id) => {
                let (element, count) = self.array_def(id);
                let (element_size, element_align) = self.compact_size_align(element);
                (element_size.saturating_mul(count), element_align.max(1))
            }
            TypeKind::Enum(id) => {
                let layout = self.compact_enum_layout(id);
                (layout.size, layout.alignment)
            }
        };
        if size == 0 { (0, 1) } else { (size, alignment) }
    }

    /// Compact struct layout: declaration-order field byte offsets at their
    /// natural alignment with interior and tail padding, struct alignment equal
    /// to the maximum field alignment (minimum 1), and size rounded up to that
    /// alignment (so `stride == size`).
    fn compact_struct_layout(&self, struct_id: StructId) -> CompactAggregateLayout {
        let fields = &self.struct_def(struct_id).fields;
        let mut field_offsets = Vec::with_capacity(fields.len());
        let mut padding_ranges = Vec::new();
        let mut offset = 0u64;
        let mut alignment = 1u64;
        for field in fields {
            let (field_size, field_align) = self.compact_size_align(field.ty);
            let placed = align_up(offset, field_align);
            if placed > offset {
                padding_ranges.push(PaddingRange {
                    start: offset,
                    end: placed,
                });
            }
            field_offsets.push(placed);
            offset = placed.saturating_add(field_size);
            alignment = alignment.max(field_align);
        }
        let size = align_up(offset, alignment);
        if size > offset {
            padding_ranges.push(PaddingRange {
                start: offset,
                end: size,
            });
        }
        CompactAggregateLayout {
            size,
            alignment,
            field_offsets,
            padding_ranges,
        }
    }

    /// Pack one enum variant's payload fields like a struct starting at offset
    /// zero, returning the field offsets (relative to the payload start), the
    /// packed payload size, and the maximum field alignment (minimum 1).
    fn compact_variant_payload(&self, payload: &[Type]) -> (Vec<u64>, u64, u64) {
        let mut offsets = Vec::with_capacity(payload.len());
        let mut offset = 0u64;
        let mut alignment = 1u64;
        for &field_ty in payload {
            let (field_size, field_align) = self.compact_size_align(field_ty);
            let placed = align_up(offset, field_align);
            offsets.push(placed);
            offset = placed.saturating_add(field_size);
            alignment = alignment.max(field_align);
        }
        (offsets, offset, alignment)
    }

    /// Compact enum layout: a smallest-sufficient unsigned tag at offset 0, the
    /// payload placed at the maximum variant alignment, and variant field byte
    /// offsets relative to the aggregate base.
    fn compact_enum_layout(&self, enum_id: EnumId) -> CompactEnumLayout {
        let def = self.enum_def(enum_id);
        let variant_count = def.variant_count();
        let (tag_size, tag_align) = compact_enum_tag(variant_count);

        let mut payload_align = 1u64;
        let mut payload_size = 0u64;
        let mut variant_local: Vec<(Vec<u64>, u64)> = Vec::with_capacity(variant_count);
        for variant in 0..variant_count {
            let (offsets, packed, align) =
                self.compact_variant_payload(def.variant_payload(variant));
            payload_align = payload_align.max(align);
            payload_size = payload_size.max(packed);
            variant_local.push((offsets, align));
        }

        let payload_offset = align_up(tag_size, payload_align);
        let alignment = tag_align.max(payload_align);
        let size = align_up(payload_offset.saturating_add(payload_size), alignment);
        let variants = variant_local
            .into_iter()
            .map(|(offsets, _align)| {
                offsets
                    .into_iter()
                    .map(|local| payload_offset.saturating_add(local))
                    .collect()
            })
            .collect();

        CompactEnumLayout {
            tag_size,
            tag_align,
            payload_offset,
            size,
            alignment,
            variants,
        }
    }

    /// Build the full compact [`Layout`] of `ty`, including the per-kind
    /// addressing detail.
    fn compact_layout_of(&self, ty: Type) -> Layout {
        let (size, alignment) = self.compact_size_align(ty);
        let stride = size;
        let kind = match ty.kind() {
            TypeKind::Struct(id) => {
                let layout = self.compact_struct_layout(id);
                LayoutKind::Struct {
                    field_offsets: layout.field_offsets,
                    padding_ranges: layout.padding_ranges,
                }
            }
            TypeKind::Array(id) => {
                let (element, count) = self.array_def(id);
                LayoutKind::Array {
                    element: Box::new(self.compact_layout_of(element)),
                    count,
                }
            }
            TypeKind::Enum(id) => {
                let layout = self.compact_enum_layout(id);
                LayoutKind::Enum {
                    tag: Box::new(Layout {
                        size: layout.tag_size,
                        alignment: layout.tag_align,
                        stride: layout.tag_size,
                        kind: LayoutKind::Scalar,
                    }),
                    payload_offset: layout.payload_offset,
                    variants: layout.variants,
                }
            }
            _ => LayoutKind::Scalar,
        };
        Layout {
            size,
            alignment,
            stride,
            kind,
        }
    }

    /// The byte ranges of `ty`'s compact memory image that no leaf field
    /// occupies: interior and tail struct padding, an enum's tag-to-payload gap
    /// and tail padding, and any gaps between an enum's union payload positions.
    ///
    /// These are exactly the bytes ADR-0052 ruling 5 (deterministic zero on
    /// construction) requires cleared wherever a compact image is materialized,
    /// and the complement of the value-decomposition slots the codegen image map
    /// writes — so zeroing these ranges and then storing the fields
    /// deterministically initializes every byte of the image.
    fn compact_image_padding_ranges(&self, ty: Type) -> Vec<PaddingRange> {
        let (size, _) = self.compact_size_align(ty);
        if size == 0 {
            return Vec::new();
        }
        let mut covered: Vec<(u64, u64)> = Vec::new();
        self.collect_compact_leaf_ranges(ty, 0, &mut covered);
        // Complement the covered leaf ranges against `[0, size)`.
        covered.sort_by_key(|&(start, _)| start);
        let mut ranges = Vec::new();
        let mut cursor = 0u64;
        for (start, end) in covered {
            if start > cursor {
                ranges.push(PaddingRange {
                    start: cursor,
                    end: start,
                });
            }
            cursor = cursor.max(end);
        }
        if cursor < size {
            ranges.push(PaddingRange {
                start: cursor,
                end: size,
            });
        }
        ranges
    }

    /// Append the absolute byte ranges every leaf scalar of `ty`'s compact image
    /// occupies (offset by `base`) to `out`. A struct recurses through its
    /// fields; an array through its elements; an enum contributes its tag range
    /// plus every variant's every payload-field range (their union is the
    /// variant-independent payload image); a scalar contributes its own byte
    /// span. The counterpart of the codegen image map, kept here so the layout
    /// authority is the single source of which bytes are padding.
    fn collect_compact_leaf_ranges(&self, ty: Type, base: u64, out: &mut Vec<(u64, u64)>) {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let layout = self.compact_struct_layout(id);
                let fields = &self.struct_def(id).fields;
                for (field, &offset) in fields.iter().zip(layout.field_offsets.iter()) {
                    self.collect_compact_leaf_ranges(field.ty, base + offset, out);
                }
            }
            TypeKind::Array(id) => {
                let (element, count) = self.array_def(id);
                let (stride, _) = self.compact_size_align(element);
                for k in 0..count {
                    self.collect_compact_leaf_ranges(element, base + k * stride, out);
                }
            }
            TypeKind::Enum(id) => {
                let layout = self.compact_enum_layout(id);
                out.push((base, base + layout.tag_size));
                let def = self.enum_def(id);
                for variant in 0..def.variant_count() {
                    let payload = def.variant_payload(variant);
                    for (field_index, &field_ty) in payload.iter().enumerate() {
                        let offset = layout.variants[variant][field_index];
                        self.collect_compact_leaf_ranges(field_ty, base + offset, out);
                    }
                }
            }
            _ => {
                let (size, _) = self.compact_size_align(ty);
                if size > 0 {
                    out.push((base, base + size));
                }
            }
        }
    }

    fn file_symbol_component(&self, file_id: FileId) -> String {
        self.symbol_paths
            .get(&file_id)
            .map(|path| mangle_symbol_component(&normalize_module_path(path)))
            // Standalone TypeInternPool is a phase-local test/embedding API.
            // Supported semantic construction installs complete logical paths
            // before nominal symbols can be requested.
            .unwrap_or_else(|| file_id.index().to_string())
    }

    fn struct_symbol_name(&self, id: StructId) -> String {
        let data = match self.data(id.0) {
            TypeData::DeclaredStruct(data) | TypeData::Struct(data) => data,
            other => panic!("Expected struct at pool index {}, got {:?}", id.0, other),
        };
        // Every named user nominal is unconditionally file-qualified (ADR-0066,
        // RUE-1089): producer-nominal identity means two same-named types in
        // different files are distinct, so their symbols must never depend on
        // whether a collision happened to be observed. Builtins keep their bare
        // source names because they pair with runtime-provided definitions.
        // Generated anonymous structs already carry a globally-unique synthetic
        // name (`__anon_struct_<digest>`) that distinguishes every producer, and
        // their destructor/member symbols are spelled from that bare name;
        // qualifying them would only desynchronize those spellings, so they are
        // exempt. That exemption is REGISTRY MEMBERSHIP, never the generated-name
        // prefix: `__anon_struct_N` is a legal source declaration (only `__rue_*`
        // and `_start` are reserved, RUE-125), and a prefix test hands such a
        // declaration the bare symbol of a generated type it collides with
        // (RUE-1050, RUE-1193).
        // Language-item builtins (`str`, `StrBuf`, `Str(N)`) also keep their bare
        // names: they pair with runtime-provided definitions, and the lang-item
        // marker survives durable import even when `is_builtin` is not carried.
        if data.def.is_builtin
            || self.struct_lang_items.contains_key(&id)
            || self.is_anonymous_struct(id)
        {
            return data.def.name.to_string();
        }
        format!(
            "{}${}",
            data.def.name,
            self.file_symbol_component(data.def.file_id)
        )
    }

    fn enum_symbol_name(&self, id: EnumId) -> String {
        let data = match self.data(id.0) {
            TypeData::DeclaredEnum(data) | TypeData::Enum(data) => data,
            other => panic!("Expected enum at pool index {}, got {:?}", id.0, other),
        };
        // See `struct_symbol_name`: unconditional qualification, with the
        // reserved built-in enums and the registry-marked generated anonymous
        // enums (`__anon_enum_<digest> { … }`) keeping their bare names.
        if rue_builtins::is_reserved_enum_name(&data.def.name) || self.is_anonymous_enum(id) {
            return data.def.name.to_string();
        }
        format!(
            "{}${}",
            data.def.name,
            self.file_symbol_component(data.def.file_id)
        )
    }

    fn safe_type_name(&self, ty: Type) -> String {
        match ty.try_kind() {
            Some(TypeKind::Struct(id)) => self
                .struct_metadata(id)
                .map(|metadata| metadata.name.to_string())
                .unwrap_or_else(|| format!("<struct#{}>", id.0)),
            Some(TypeKind::Enum(id)) => self
                .enum_metadata(id)
                .map(|metadata| metadata.name.to_string())
                .unwrap_or_else(|| format!("<enum#{}>", id.0)),
            Some(TypeKind::Array(id)) => self
                .try_array_def(id)
                .map(|(element, len)| format!("[{}; {}]", self.safe_type_name(element), len))
                .unwrap_or_else(|| format!("<array#{}>", id.0)),
            Some(TypeKind::PtrConst(id)) => match self.try_entry(id.pool_index() as usize) {
                Some(TypeData::PtrConst { pointee }) => {
                    format!("ptr const {}", self.safe_type_name(*pointee))
                }
                _ => format!("<ptr const#{}>", id.0),
            },
            Some(TypeKind::PtrMut(id)) => match self.try_entry(id.pool_index() as usize) {
                Some(TypeData::PtrMut { pointee }) => {
                    format!("ptr mut {}", self.safe_type_name(*pointee))
                }
                _ => format!("<ptr mut#{}>", id.0),
            },
            Some(_) => ty.name().to_string(),
            None => format!("<invalid type encoding: {:#x}>", ty.raw_encoding()),
        }
    }

    fn is_copy_type(&self, ty: Type) -> bool {
        ty.as_struct()
            .map(|id| {
                self.struct_metadata(id)
                    .map(|metadata| metadata.is_copy)
                    .expect("struct type must have declaration metadata")
            })
            .unwrap_or_else(|| ty.is_copy())
    }

    fn stats(&self) -> TypeInternPoolStats {
        let mut stats = TypeInternPoolStats {
            struct_count: 0,
            enum_count: 0,
            array_count: 0,
            total: self.entry_count(),
        };
        for index in 0..self.entry_count() {
            match self.entry(index) {
                TypeData::DeclaredStruct(_) | TypeData::Struct(_) => stats.struct_count += 1,
                TypeData::DeclaredEnum(_) | TypeData::Enum(_) => stats.enum_count += 1,
                TypeData::Array { .. } => stats.array_count += 1,
                TypeData::ReservedStruct | TypeData::PtrConst { .. } | TypeData::PtrMut { .. } => {}
            }
        }
        stats
    }
}

impl TypeInternPool {
    /// Create a new empty pool.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(TypeInternPoolInner::empty()),
        }
    }

    /// Whether this universe ran past the published ceiling of
    /// [`MAX_COMPOSITE_TYPES`] distinct composite types (spec Appendix C.6:1).
    ///
    /// Composite interning is infallible at hundreds of call sites, so the pool
    /// latches the rejection and stops growing instead of panicking or wrapping
    /// its 24-bit index. Semantic analysis polls this at its diagnostic
    /// boundaries and rejects the compilation with `E1401` naming the limit, as
    /// spec C.1:2 requires.
    pub fn capacity_exceeded(&self) -> bool {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .capacity_exceeded()
    }

    /// Snapshot only the compact handles for this completed semantic epoch.
    ///
    /// Aggregate export needs to enumerate the visible types before consuming
    /// the pool, but it does not need a second owned copy of their definitions,
    /// lookup indexes, or containment metadata. Keeping this operation on the
    /// mutable pool avoids a deep clone-and-freeze immediately before the
    /// original pool is frozen for publication.
    pub(crate) fn complete_type_handles(&self) -> Vec<Type> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        (0..inner.entry_count())
            .map(|index| complete_type_handle(index, inner.entry(index)))
            .collect()
    }

    /// Consume the completed semantic type universe for backend-facing reads.
    ///
    /// This is the last legal mutation boundary. Request-local symbol interners
    /// remain separate: type definitions retain stable string names rather than
    /// storing a [`Spur`] from a CFG or codegen request.
    pub fn freeze(self) -> FrozenTypeInternPool {
        let inner = self
            .inner
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        // Post-semantic phases index the pool directly and never intern, so the
        // frozen universe is materialized flat: the request-scoped base and this
        // epoch's overlay collapse into one store here (RUE-1135).
        let mut inner = inner.flatten();
        // A pool that ran past the published composite-type ceiling stopped
        // completing declaration shells on purpose (spec C.6:1); its
        // compilation is failing with `E1401` and nothing frozen from it is
        // published, so incompleteness here is expected rather than a producer
        // bug.
        if !inner.capacity_exceeded()
            && let Some((index, entry)) = inner
                .types
                .iter()
                .enumerate()
                .find(|(_, entry)| entry.is_incomplete())
        {
            panic!("cannot freeze incomplete type-pool entry {index}: {entry:?}");
        }
        inner
            .finalize_containment_metadata()
            .unwrap_or_else(|cycle| {
                panic!(
                    "cannot freeze cyclic by-value type graph: {}",
                    cycle.path.join(" -> ")
                )
            });
        let success_validation = inner.validate_for_success();
        FrozenTypeInternPool {
            inner: Arc::new(inner),
            success_validation,
        }
    }

    /// Set relocation-stable source identities for type-derived symbols.
    pub(crate) fn set_symbol_paths(&self, symbol_paths: HashMap<FileId, String>) {
        self.inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .symbol_paths = Arc::new(symbol_paths);
    }

    /// Publish one relocation-stable source identity without rebuilding the
    /// complete file table. Returns `false` when the file id is already owned
    /// by a different logical path; publishing the same pair is idempotent.
    pub(crate) fn insert_symbol_path(&self, file_id: FileId, symbol_path: String) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        let symbol_paths = Arc::make_mut(&mut inner.symbol_paths);
        match symbol_paths.get(&file_id) {
            Some(existing) => existing == &symbol_path,
            None => {
                symbol_paths.insert(file_id, symbol_path);
                true
            }
        }
    }

    /// Apply a type-pool mutation atomically through an isolated snapshot.
    ///
    /// The live write lock remains held while `operation` works on the
    /// snapshot, so a successful replacement preserves canonical allocation
    /// order and cannot overwrite concurrent interning. Failure discards the
    /// entire pool snapshot, including every vector and reverse-map mutation.
    /// State owned beside the pool (such as a symbol interner) needs its own
    /// transaction or preflight boundary.
    ///
    /// This intentionally simple boundary deep-clones the whole pool while
    /// holding its global write lock. Callers should account for that per-
    /// operation cost; it is a correctness mechanism, not a cheap fine-grained
    /// mutation primitive.
    pub(crate) fn transaction<T, E>(
        &self,
        operation: impl FnOnce(&TypeInternPool) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut live = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        let scratch = TypeInternPool {
            inner: RwLock::new(live.clone()),
        };
        let result = operation(&scratch)?;
        *live = scratch
            .inner
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        Ok(result)
    }

    /// Return the flattened runtime ABI width of `ty` in eight-byte slots.
    ///
    /// This is the canonical slot decomposition shared by sema, CFG temporary
    /// allocation, and code generation (ADR-0052 representation 2, the internal
    /// value model). It is distinct from the compact physical byte layout that
    /// observes or addresses memory, which [`Self::layout`] reports. Aggregate
    /// arithmetic saturates; sema rejects layouts that exceed the representable
    /// slot range before they can be materialized.
    pub fn abi_slot_count(&self, ty: Type) -> u32 {
        self.try_abi_slot_count(ty)
            .expect("layout requires a complete, non-recovery type graph")
    }

    pub fn try_abi_slot_count(&self, ty: Type) -> Result<u32, TypeValidationError> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.validate_complete_type(ty)?;
        Ok(inner.abi_slot_count(ty))
    }

    /// Semantic construction may need a phase-scoped provisional width before
    /// the complete type graph is available. The successful sema boundary
    /// validates the complete graph before layout or backend consumption.
    pub(crate) fn provisional_abi_slot_count(&self, ty: Type) -> u32 {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .abi_slot_count(ty)
    }

    /// Provisional [`Layout`] of `ty` for use during semantic analysis, before
    /// the type graph is frozen. Companion to [`Self::provisional_abi_slot_count`];
    /// `@size_of` and `@align_of` read the byte size and alignment from here.
    pub(crate) fn provisional_layout(&self, ty: Type) -> Layout {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .layout(ty)
    }

    /// Provisional byte offset of a struct field for `@offset_of`, matching the
    /// field placement code generation later addresses. See
    /// [`Self::provisional_layout`].
    pub(crate) fn provisional_struct_field_offset(
        &self,
        struct_id: StructId,
        field_index: u32,
    ) -> u64 {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .struct_field_offset(struct_id, field_index)
    }

    /// Validate an encoding-valid type against this pool while allowing the
    /// recovery and declared-shell states needed during semantic construction.
    ///
    /// Validation is relative to this owner pool. Compact handles from another
    /// epoch can have coincidentally equal bits; epoch-branded artifacts and
    /// durable import boundaries establish ownership before this check.
    pub fn validate_structural_child(&self, ty: Type) -> Result<(), TypeValidationError> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .validate_structural_child(ty)
    }

    /// Validate that `ty` and its reachable pool graph are complete and contain
    /// no recovery-only `<error>` node.
    ///
    /// Validation is relative to this owner pool. Compact handles from another
    /// epoch can have coincidentally equal bits; epoch-branded artifacts and
    /// durable import boundaries establish ownership before this check.
    pub fn validate_complete_type(&self, ty: Type) -> Result<(), TypeValidationError> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .validate_complete_type(ty)
    }

    /// Register a new struct (nominal - no deduplication).
    ///
    /// Returns the pool-issued `StructId` and whether it was newly inserted.
    /// If a struct with this name in the same defining file already exists, returns the existing
    /// StructId.
    pub fn register_struct(&self, name: Spur, def: StructDef) -> (StructId, bool) {
        let key = (def.file_id, name);
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = inner.lookup_struct_by_file_name(&key) {
                return (
                    existing.as_struct().expect("struct lookup kind invariant"),
                    false,
                );
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(existing) = inner.lookup_struct_by_file_name(&key) {
            return (
                existing.as_struct().expect("struct lookup kind invariant"),
                false,
            );
        }

        // Create new struct type
        let pool_index = inner.next_pool_index();
        let struct_id = StructId::from_pool_index(pool_index);
        let ty = Type::new_struct(struct_id);

        let mut entry = TypeData::Struct(StructData {
            name,
            abi_slots: 0,
            def: Arc::new(StructDefEntry::new(def)),
        });
        let facts = inner.incremental_facts(&entry);
        if let Some(facts) = facts {
            entry.set_abi_slots(facts.abi_slots);
        }
        if facts.is_some_and(|facts| facts.containment.carries_linear) {
            let TypeData::Struct(data) = &mut entry else {
                unreachable!()
            };
            Arc::make_mut(&mut data.def).metadata_mut().is_linear = true;
        }
        inner.push_entry(entry, facts.map(|facts| facts.containment));
        inner.struct_by_file_name.insert(key, ty);

        (struct_id, true)
    }

    /// Register a named struct identity whose definition will be completed
    /// after declaration type references have been resolved.
    pub(crate) fn declare_struct(&self, name: Spur, shell: StructDef) -> (StructId, bool) {
        let key = (shell.file_id, name);
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = inner.lookup_struct_by_file_name(&key) {
                return (
                    existing.as_struct().expect("struct lookup kind invariant"),
                    false,
                );
            }
        }

        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = inner.lookup_struct_by_file_name(&key) {
            return (
                existing.as_struct().expect("struct lookup kind invariant"),
                false,
            );
        }

        let pool_index = inner.next_pool_index();
        let ty = Type::new_struct(StructId::from_pool_index(pool_index));
        inner.push_entry(
            TypeData::DeclaredStruct(StructData {
                name,
                abi_slots: 0,
                def: Arc::new(StructDefEntry::new(shell)),
            }),
            None,
        );
        inner.struct_by_file_name.insert(key, ty);
        (StructId::from_pool_index(pool_index), true)
    }

    /// Reserve a struct ID without registering the full definition yet.
    ///
    /// This is used for anonymous structs where we need to know the ID before
    /// we can construct the name (which includes the ID). Call `complete_struct_registration`
    /// with the reserved ID to finish registration.
    ///
    /// # Returns
    ///
    /// Returns the reserved `StructId`. The caller MUST call `complete_struct_registration`
    /// with this ID before any other pool operations that might read this entry.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let struct_id = pool.reserve_struct_id();
    /// let name = format!("__anon_struct_{}", struct_id.0);
    /// let name_spur = interner.get_or_intern(&name);
    /// let def = StructDef { name: name.clone(), ... };
    /// pool.complete_struct_registration(struct_id, name_spur, def);
    /// ```
    pub(crate) fn reserve_struct_id(&self) -> StructId {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        let pool_index = inner.next_pool_index();
        inner.push_entry(TypeData::ReservedStruct, None);

        StructId::from_pool_index(pool_index)
    }

    /// Complete the registration of a previously reserved struct ID.
    ///
    /// This must be called after `reserve_struct_id` to fill in the actual struct data.
    /// The struct will be registered with the provided name for lookup purposes.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - The struct_id wasn't created by `reserve_struct_id`
    /// - The slot at struct_id doesn't contain a placeholder struct
    /// - A struct with the given name already exists
    pub(crate) fn complete_struct_registration(
        &self,
        struct_id: StructId,
        name: Spur,
        def: StructDef,
    ) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        // A pool that already ran past the published composite-type ceiling
        // (spec Appendix C.6:1) hands every later registration the same final
        // index, so the slot named here is not the reserved one this call
        // expects. The compilation is already being failed with `E1401`; skip
        // the completion instead of asserting (spec C.1:2 forbids an abort).
        if inner.capacity_exceeded() {
            return;
        }
        let pool_index = struct_id.0 as usize;

        // Verify this is a valid reserved slot
        assert!(
            pool_index < inner.entry_count(),
            "Invalid reserved struct ID: index {} out of bounds (len {})",
            pool_index,
            inner.entry_count()
        );

        assert!(
            matches!(inner.try_entry(pool_index), Some(TypeData::ReservedStruct)),
            "pool index {} is not a reserved struct entry",
            pool_index
        );

        assert!(
            inner
                .lookup_struct_by_file_name(&(def.file_id, name))
                .is_none(),
            "Struct with this name already exists"
        );

        // Update the placeholder with actual data
        let key = (def.file_id, name);
        let mut entry = TypeData::Struct(StructData {
            name,
            abi_slots: 0,
            def: Arc::new(StructDefEntry::new(def)),
        });
        let facts = inner.incremental_facts(&entry);
        if let Some(facts) = facts {
            entry.set_abi_slots(facts.abi_slots);
        }
        if facts.is_some_and(|facts| facts.containment.carries_linear) {
            let TypeData::Struct(data) = &mut entry else {
                unreachable!()
            };
            Arc::make_mut(&mut data.def).metadata_mut().is_linear = true;
        }
        *inner.entry_mut(pool_index) = entry;
        inner.set_facts(pool_index, facts.map(|facts| facts.containment));

        // Register in the defining-file lookup.
        inner
            .struct_by_file_name
            .insert(key, Type::new_struct(struct_id));
    }

    /// Complete a named struct declaration exactly once.
    pub(crate) fn complete_declared_struct(&self, struct_id: StructId, def: StructDef) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        // A pool that already ran past the published composite-type ceiling
        // (spec Appendix C.6:1) hands every later registration the same final
        // index, so the slot named here is not the reserved one this call
        // expects. The compilation is already being failed with `E1401`; skip
        // the completion instead of asserting (spec C.1:2 forbids an abort).
        if inner.capacity_exceeded() {
            return;
        }
        let pool_index = struct_id.pool_index() as usize;
        let name = match inner
            .try_entry(pool_index)
            .unwrap_or_else(|| panic!("Invalid declared struct ID: {pool_index}"))
        {
            TypeData::DeclaredStruct(data) => {
                assert_eq!(
                    data.def.file_id, def.file_id,
                    "completed struct changed defining file"
                );
                assert_eq!(
                    data.def.name.as_ref(),
                    def.name.as_ref(),
                    "completed struct changed textual name"
                );
                data.name
            }
            other => panic!(
                "pool index {} is not a declared struct entry: {:?}",
                pool_index, other
            ),
        };
        // The completing definition carries the declaration's full metadata —
        // fields, explicit linear marker, and (on paths that know it at
        // completion time) the destructor symbol — so this is the earliest
        // point exact facts can derive incrementally from already-available
        // children, exactly as `complete_struct_registration` does. A child
        // still awaiting facts (out-of-dependency-order completion) leaves
        // this entry factless for the canonical full pass, and a destructor
        // attached later goes through `set_struct_destructor`, which marks
        // the whole pool stale.
        let mut entry = TypeData::Struct(StructData {
            name,
            abi_slots: 0,
            def: Arc::new(StructDefEntry::new(def)),
        });
        let facts = inner.incremental_facts(&entry);
        if let Some(facts) = facts {
            entry.set_abi_slots(facts.abi_slots);
        }
        if facts.is_some_and(|facts| facts.containment.carries_linear) {
            let TypeData::Struct(data) = &mut entry else {
                unreachable!()
            };
            Arc::make_mut(&mut data.def).metadata_mut().is_linear = true;
        }
        *inner.entry_mut(pool_index) = entry;
        inner.set_facts(pool_index, facts.map(|facts| facts.containment));
    }

    /// Register a new enum (nominal - no deduplication).
    ///
    /// Returns the pool-issued `EnumId` and whether it was newly inserted.
    /// If an enum with this name in the same defining file already exists, returns the existing
    /// EnumId.
    pub fn register_enum(&self, name: Spur, def: EnumDef) -> (EnumId, bool) {
        let key = (def.file_id, name);
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = inner.lookup_enum_by_file_name(&key) {
                return (
                    existing.as_enum().expect("enum lookup kind invariant"),
                    false,
                );
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(existing) = inner.lookup_enum_by_file_name(&key) {
            return (
                existing.as_enum().expect("enum lookup kind invariant"),
                false,
            );
        }

        // Create new enum type
        let pool_index = inner.next_pool_index();
        let enum_id = EnumId::from_pool_index(pool_index);
        let ty = Type::new_enum(enum_id);

        let mut entry = TypeData::Enum(EnumData {
            name,
            abi_slots: 0,
            def: Arc::new(EnumDefEntry::new(def)),
        });
        let facts = inner.incremental_facts(&entry);
        if let Some(facts) = facts {
            entry.set_abi_slots(facts.abi_slots);
        }
        inner.push_entry(entry, facts.map(|facts| facts.containment));
        inner.enum_by_file_name.insert(key, ty);

        (enum_id, true)
    }

    /// Register a named enum identity whose definition will be completed after
    /// payload type references have been resolved.
    pub(crate) fn declare_enum(&self, name: Spur, shell: EnumDef) -> (EnumId, bool) {
        let key = (shell.file_id, name);
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = inner.lookup_enum_by_file_name(&key) {
                return (
                    existing.as_enum().expect("enum lookup kind invariant"),
                    false,
                );
            }
        }

        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = inner.lookup_enum_by_file_name(&key) {
            return (
                existing.as_enum().expect("enum lookup kind invariant"),
                false,
            );
        }

        let pool_index = inner.next_pool_index();
        let ty = Type::new_enum(EnumId::from_pool_index(pool_index));
        inner.push_entry(
            TypeData::DeclaredEnum(EnumData {
                name,
                abi_slots: 0,
                def: Arc::new(EnumDefEntry::new(shell)),
            }),
            None,
        );
        inner.enum_by_file_name.insert(key, ty);
        (EnumId::from_pool_index(pool_index), true)
    }

    /// Complete a named enum declaration exactly once.
    pub(crate) fn complete_declared_enum(&self, enum_id: EnumId, def: EnumDef) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        // A pool that already ran past the published composite-type ceiling
        // (spec Appendix C.6:1) hands every later registration the same final
        // index, so the slot named here is not the reserved one this call
        // expects. The compilation is already being failed with `E1401`; skip
        // the completion instead of asserting (spec C.1:2 forbids an abort).
        if inner.capacity_exceeded() {
            return;
        }
        let pool_index = enum_id.pool_index() as usize;
        let name = match inner
            .try_entry(pool_index)
            .unwrap_or_else(|| panic!("Invalid declared enum ID: {pool_index}"))
        {
            TypeData::DeclaredEnum(data) => {
                assert_eq!(
                    data.def.file_id, def.file_id,
                    "completed enum changed defining file"
                );
                assert_eq!(
                    data.def.name.as_ref(),
                    def.name.as_ref(),
                    "completed enum changed textual name"
                );
                data.name
            }
            other => panic!(
                "pool index {} is not a declared enum entry: {:?}",
                pool_index, other
            ),
        };
        // See `complete_declared_struct`: completion is the earliest point
        // exact facts can derive incrementally; unavailable payload children
        // leave the entry factless for the canonical full pass.
        let mut entry = TypeData::Enum(EnumData {
            name,
            abi_slots: 0,
            def: Arc::new(EnumDefEntry::new(def)),
        });
        let facts = inner.incremental_facts(&entry);
        if let Some(facts) = facts {
            entry.set_abi_slots(facts.abi_slots);
        }
        *inner.entry_mut(pool_index) = entry;
        inner.set_facts(pool_index, facts.map(|facts| facts.containment));
    }

    /// Intern an array after validating its canonical child in this pool.
    pub fn try_intern_array(&self, element: Type, len: u64) -> Result<Type, TypeValidationError> {
        let key = (element, len);

        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            inner.validate_structural_child(element)?;
            if let Some(existing) = inner.lookup_array(&key) {
                return Ok(existing);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        inner.validate_structural_child(element)?;

        // Double-check after acquiring write lock
        if let Some(existing) = inner.lookup_array(&key) {
            return Ok(existing);
        }

        // Create new array type
        let pool_index = inner.next_pool_index();
        let ty = Type::new_array(ArrayTypeId::from_pool_index(pool_index));

        let mut entry = TypeData::Array {
            element,
            abi_slots: 0,
            len,
        };
        let facts = inner.incremental_facts(&entry);
        if let Some(facts) = facts {
            entry.set_abi_slots(facts.abi_slots);
        }
        inner.push_entry(entry, facts.map(|facts| facts.containment));
        inner.array_map.insert(key, ty);

        Ok(ty)
    }

    /// Intern a const pointer after validating its canonical child in this pool.
    pub fn try_intern_ptr_const(&self, pointee: Type) -> Result<Type, TypeValidationError> {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            inner.validate_structural_child(pointee)?;
            if let Some(existing) = inner.lookup_ptr_const(&pointee) {
                return Ok(existing);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        inner.validate_structural_child(pointee)?;

        // Double-check after acquiring write lock
        if let Some(existing) = inner.lookup_ptr_const(&pointee) {
            return Ok(existing);
        }

        // Create new pointer type
        let pool_index = inner.next_pool_index();
        let ty = Type::new_ptr_const(PtrConstTypeId::from_pool_index(pool_index));

        let entry = TypeData::PtrConst { pointee };
        let facts = inner.incremental_facts(&entry);
        inner.push_entry(entry, facts.map(|facts| facts.containment));
        inner.ptr_const_map.insert(pointee, ty);

        Ok(ty)
    }

    /// Intern a mutable pointer after validating its canonical child in this pool.
    pub fn try_intern_ptr_mut(&self, pointee: Type) -> Result<Type, TypeValidationError> {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            inner.validate_structural_child(pointee)?;
            if let Some(existing) = inner.lookup_ptr_mut(&pointee) {
                return Ok(existing);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        inner.validate_structural_child(pointee)?;

        // Double-check after acquiring write lock
        if let Some(existing) = inner.lookup_ptr_mut(&pointee) {
            return Ok(existing);
        }

        // Create new pointer type
        let pool_index = inner.next_pool_index();
        let ty = Type::new_ptr_mut(PtrMutTypeId::from_pool_index(pool_index));

        let entry = TypeData::PtrMut { pointee };
        let facts = inner.incremental_facts(&entry);
        inner.push_entry(entry, facts.map(|facts| facts.containment));
        inner.ptr_mut_map.insert(pointee, ty);

        Ok(ty)
    }

    /// Look up a struct by defining file and source name.
    pub fn get_struct_by_file_name(&self, file_id: FileId, name: Spur) -> Option<Type> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.lookup_struct_by_file_name(&(file_id, name))
    }

    /// Look up an enum by defining file and source name.
    pub fn get_enum_by_file_name(&self, file_id: FileId, name: Spur) -> Option<Type> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.lookup_enum_by_file_name(&(file_id, name))
    }

    /// Look up an array type by element and length.
    pub fn get_array(&self, element: Type, len: u64) -> Option<Type> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.lookup_array(&(element, len))
    }

    /// Get type data for a composite type.
    ///
    /// Returns `None` for primitives, malformed/out-of-range handles, reserved
    /// or declared entries, or a handle whose encoded category disagrees with
    /// its pool entry. Declaration state is exposed only through narrow
    /// crate-private metadata queries.
    pub fn get(&self, ty: Type) -> Option<TypeData> {
        let (pool_index, expected) = match ty.try_kind()? {
            TypeKind::Struct(id) => (id.pool_index(), PoolEntryKind::Struct),
            TypeKind::Enum(id) => (id.pool_index(), PoolEntryKind::Enum),
            TypeKind::Array(id) => (id.pool_index(), PoolEntryKind::Array),
            TypeKind::PtrConst(id) => (id.pool_index(), PoolEntryKind::PtrConst),
            TypeKind::PtrMut(id) => (id.pool_index(), PoolEntryKind::PtrMut),
            _ => return None,
        };
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let entry = inner.try_entry(pool_index as usize)?;
        (entry.kind() == expected
            && !matches!(
                entry,
                TypeData::ReservedStruct | TypeData::DeclaredStruct(_) | TypeData::DeclaredEnum(_)
            ))
        .then(|| entry.clone())
    }

    /// Check if this is a struct type.
    pub fn is_struct(&self, ty: Type) -> bool {
        let Some(id) = ty.as_struct() else {
            return false;
        };
        matches!(
            self.inner
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .try_entry(id.pool_index() as usize),
            Some(TypeData::DeclaredStruct(_) | TypeData::Struct(_))
        )
    }

    /// Check if this is an enum type.
    pub fn is_enum(&self, ty: Type) -> bool {
        let Some(id) = ty.as_enum() else {
            return false;
        };
        matches!(
            self.inner
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .try_entry(id.pool_index() as usize),
            Some(TypeData::DeclaredEnum(_) | TypeData::Enum(_))
        )
    }

    /// Check if this is an array type.
    pub fn is_array(&self, ty: Type) -> bool {
        matches!(self.get(ty), Some(TypeData::Array { .. }))
    }

    /// Get the struct definition if this is a struct type.
    pub fn get_struct_def(&self, ty: Type) -> Option<Arc<StructDefEntry>> {
        match self.get(ty)? {
            TypeData::Struct(data) => Some(data.def),
            _ => None,
        }
    }

    /// Get the enum definition if this is an enum type.
    pub fn get_enum_def(&self, ty: Type) -> Option<Arc<EnumDefEntry>> {
        match self.get(ty)? {
            TypeData::Enum(data) => Some(data.def),
            _ => None,
        }
    }

    /// Get array info (element type, length) if this is an array type.
    pub fn get_array_info(&self, ty: Type) -> Option<(Type, u64)> {
        match self.get(ty)? {
            TypeData::Array { element, len, .. } => Some((element, len)),
            _ => None,
        }
    }

    // ========================================================================
    // Direct nominal-ID access
    // ========================================================================
    //
    // These methods access struct and enum definitions through opaque IDs
    // issued by this pool.

    /// Get a struct definition by StructId.
    ///
    /// This method resolves the pool-issued identity and returns the shared
    /// handle to its definition. The `RwLock` is released before the handle is
    /// returned, so the read costs one refcount bump rather than a deep clone
    /// of the definition (RUE-1147). [`FrozenTypeInternPool::struct_def`] is
    /// the borrow-returning counterpart for consumers that own the pool.
    ///
    /// # Panics
    ///
    /// Panics if the StructId doesn't correspond to a struct in the pool —
    /// unless the composite-type capacity latch has aliased handles onto the
    /// final entry, in which case the read degrades to an empty definition
    /// while the compilation fails with `E1401` (spec C.1:2).
    #[track_caller]
    pub fn struct_def(&self, struct_id: StructId) -> Arc<StructDefEntry> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        match inner.try_struct_def_arc(struct_id) {
            Some(def) => Arc::clone(def),
            None if inner.capacity_exceeded() => Arc::new(ALIASED_STRUCT_DEF.clone()),
            None => panic!("Expected complete struct at pool index {}", struct_id.0),
        }
    }

    /// Get a struct definition without panicking on an invalid or wrong-kind ID.
    pub fn try_struct_def(&self, struct_id: StructId) -> Option<Arc<StructDefEntry>> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.try_struct_def_arc(struct_id).map(Arc::clone)
    }

    pub(crate) fn struct_declaration_metadata(
        &self,
        struct_id: StructId,
    ) -> Option<StructDeclarationMetadata> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .struct_declaration_metadata(struct_id)
    }

    pub(crate) fn struct_metadata(&self, struct_id: StructId) -> Option<StructDeclarationMetadata> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .struct_metadata(struct_id)
    }

    /// Record that this struct was generated for an anonymous type.
    ///
    /// Called by anonymous-type creation, which is the only authority on
    /// generated-ness: the `__anon_struct_N` spelling is a legal source name
    /// and must never be used to infer it (RUE-1050).
    pub fn mark_anonymous_struct(&self, struct_id: StructId) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        inner.anonymous_structs.insert(struct_id);
    }

    /// Enum counterpart of [`Self::mark_anonymous_struct`].
    pub fn mark_anonymous_enum(&self, enum_id: EnumId) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        inner.anonymous_enums.insert(enum_id);
    }

    /// Whether this struct was generated for an anonymous type.
    pub fn is_anonymous_struct(&self, struct_id: StructId) -> bool {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.is_anonymous_struct(struct_id)
    }

    /// Whether this enum was generated for an anonymous type.
    pub fn is_anonymous_enum(&self, enum_id: EnumId) -> bool {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.is_anonymous_enum(enum_id)
    }

    /// Return the stable standard-library identity carried by a nominal type.
    pub fn struct_lang_item(&self, struct_id: StructId) -> Option<LangItem> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.struct_lang_items.get(&struct_id).copied()
    }

    /// Return the nominal type carrying a stable standard-library identity.
    pub fn lang_item_type(&self, lang_item: LangItem) -> Option<Type> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .lang_item_structs
            .get(&lang_item)
            .copied()
            .map(Type::new_struct)
    }

    /// Assign an explicitly authorized language item to a registered nominal.
    pub fn set_struct_lang_item(&self, struct_id: StructId, lang_item: LangItem) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        assert!(
            matches!(
                inner.try_entry(struct_id.0 as usize),
                Some(TypeData::DeclaredStruct(_) | TypeData::Struct(_))
            ),
            "language items can only be assigned to registered structs"
        );
        if let Some(existing) = inner.lang_item_structs.get(&lang_item) {
            assert_eq!(
                *existing, struct_id,
                "a language item can only identify one canonical struct"
            );
        }
        if let Some(existing) = inner.struct_lang_items.get(&struct_id) {
            assert_eq!(
                *existing, lang_item,
                "a struct can only carry one language item"
            );
        }
        Arc::make_mut(&mut inner.struct_lang_items).insert(struct_id, lang_item);
        Arc::make_mut(&mut inner.lang_item_structs).insert(lang_item, struct_id);
    }

    /// Whether a nominal is the canonical trusted standard-library StrBuf.
    pub fn is_strbuf(&self, struct_id: StructId) -> bool {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.struct_lang_items.get(&struct_id) == Some(&LangItem::StrBuf)
    }

    /// Record that a struct carries the `@repr(c)` guarantee marker (ADR-0064
    /// Amendment 1). Set during type-name registration; read by the FFI
    /// predicates and extern-signature enforcement.
    pub fn set_struct_repr_c(&self, struct_id: StructId) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        Arc::make_mut(&mut inner.repr_c_structs).insert(struct_id);
    }

    /// Whether a struct carries the `@repr(c)` guarantee marker.
    pub fn is_struct_repr_c(&self, struct_id: StructId) -> bool {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.repr_c_structs.contains(&struct_id)
    }

    /// Get an enum definition by EnumId.
    ///
    /// This method resolves the pool-issued identity and returns the shared
    /// handle to its definition, on the same terms as
    /// [`Self::struct_def`] (RUE-1147).
    ///
    /// # Panics
    ///
    /// Panics if the EnumId doesn't correspond to an enum in the pool — with
    /// the same capacity-latch exemption as [`Self::struct_def`].
    #[track_caller]
    pub fn enum_def(&self, enum_id: EnumId) -> Arc<EnumDefEntry> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        match inner.try_enum_def_arc(enum_id) {
            Some(def) => Arc::clone(def),
            None if inner.capacity_exceeded() => Arc::new(ALIASED_ENUM_DEF.clone()),
            None => panic!("Expected complete enum at pool index {}", enum_id.0),
        }
    }

    /// Get an enum definition without panicking on an invalid or wrong-kind ID.
    pub fn try_enum_def(&self, enum_id: EnumId) -> Option<Arc<EnumDefEntry>> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.try_enum_def_arc(enum_id).map(Arc::clone)
    }

    pub(crate) fn enum_variant_count(&self, enum_id: EnumId) -> Option<usize> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.try_enum_def(enum_id).map(|def| def.variant_count())
    }

    pub(crate) fn enum_variant_payload_len(
        &self,
        enum_id: EnumId,
        variant: usize,
    ) -> Option<usize> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let def = inner.try_enum_def(enum_id)?;
        (variant < def.variant_count()).then(|| def.variant_payload(variant).len())
    }

    pub(crate) fn enum_variant_payload_type(
        &self,
        enum_id: EnumId,
        variant: usize,
        field: usize,
    ) -> Option<Type> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let def = inner.try_enum_def(enum_id)?;
        (variant < def.variant_count())
            .then(|| def.variant_payload(variant).get(field).copied())
            .flatten()
    }

    pub(crate) fn enum_declaration_metadata(
        &self,
        enum_id: EnumId,
    ) -> Option<EnumDeclarationMetadata> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .enum_declaration_metadata(enum_id)
    }

    pub(crate) fn enum_metadata(&self, enum_id: EnumId) -> Option<EnumDeclarationMetadata> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .enum_metadata(enum_id)
    }

    /// The symbol-name component for functions derived from a struct —
    /// methods (`P.get`), associated functions (`P::make`), destructors
    /// (`P.__drop`), and drop glue (`__rue_drop_P`) — RUE-571.
    ///
    /// Same-named nominal types across files are legal (RUE-558), but these
    /// symbols are program-wide identities. Every named user nominal is
    /// unconditionally qualified with the defining file
    /// (`P$left_2fmodel_2erue`) (ADR-0066, RUE-1089). `$` cannot appear in a
    /// source identifier, so a qualified name can never collide with a real
    /// type. Builtins remain bare so their symbols pair with runtime-provided
    /// definitions.
    ///
    /// Every layer that names a function after a type — sema (definition and
    /// call sites), the drop-glue generator in `rue-compiler`, and both
    /// codegen backends — must derive the name through this ONE helper so
    /// definitions and calls meet at link time.
    pub fn struct_symbol_name(&self, struct_id: StructId) -> String {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.struct_symbol_name(struct_id)
    }

    /// The symbol-name component for an enum's drop glue (`__rue_drop_E`),
    /// unconditionally file-qualified for named user enums (ADR-0066,
    /// RUE-1089), while builtins remain bare. See
    /// [`Self::struct_symbol_name`] — same rule, same reason.
    pub fn enum_symbol_name(&self, enum_id: EnumId) -> String {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.enum_symbol_name(enum_id)
    }

    /// Assign a complete struct's destructor symbol exactly once.
    ///
    /// Destructor discovery is a semantic metadata-finalization step. It
    /// cannot replace fields or any other completed definition data.
    pub(crate) fn set_struct_destructor(&self, struct_id: StructId, symbol: String) {
        assert!(
            symbol.ends_with(".__drop"),
            "destructor symbol must end with .__drop"
        );
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        // A capacity-latched pool aliases later registrations onto the final
        // entry, so `struct_id` need not name a complete struct and the
        // assertions below would be checking someone else's definition. The
        // compilation is already failing with `E1401`; skip the metadata
        // finalization instead of aborting (spec C.1:2).
        if inner.capacity_exceeded() {
            return;
        }
        let def = inner.struct_def_mut(struct_id);
        assert!(!def.is_copy, "a copy struct cannot acquire a destructor");
        assert!(
            def.destructor.is_none(),
            "struct destructor metadata can only be assigned once"
        );
        def.destructor = Some(Arc::from(symbol));
        inner.invalidate_containment_metadata();
    }

    /// Finalize the canonical by-value graph after declaration fields,
    /// payloads, destructors, and explicit linear markers are known.
    pub(crate) fn finalize_containment_metadata(
        &self,
    ) -> Result<TypeContainmentWork, TypeContainmentCycle> {
        self.inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .finalize_containment_metadata()
    }

    #[cfg(test)]
    pub(crate) fn containment_metrics(&self) -> TypeContainmentMetrics {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .containment_metrics
    }

    /// Test-only visibility into the exact count of factless entries.
    #[cfg(test)]
    pub(crate) fn pending_containment_facts(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .pending_facts
    }

    pub(crate) fn try_type_carries_linear(&self, ty: Type) -> Option<bool> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .facts_for_type(ty)
            .map(|facts| facts.carries_linear)
    }

    pub(crate) fn try_type_needs_drop(&self, ty: Type) -> Option<bool> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .facts_for_type(ty)
            .map(|facts| facts.needs_drop)
    }

    pub(crate) fn type_carries_linear(&self, ty: Type) -> bool {
        self.try_type_carries_linear(ty)
            .expect("linearity query requires finalized containment metadata")
    }

    pub(crate) fn type_needs_drop(&self, ty: Type) -> bool {
        self.try_type_needs_drop(ty)
            .expect("drop query requires finalized containment metadata")
    }

    /// Get an array type definition by ArrayTypeId.
    ///
    /// This method resolves the pool-issued identity and returns its element
    /// type and length as a tuple.
    ///
    /// # Returns
    ///
    /// Returns `(element_type, length)` where `element_type` is the array's element type
    /// and `length` is the array's fixed size.
    ///
    /// # Panics
    ///
    /// Panics if the ArrayTypeId doesn't correspond to an array in the pool.
    pub fn array_def(&self, array_id: ArrayTypeId) -> (Type, u64) {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.array_def(array_id)
    }

    /// Get an array definition without panicking on an invalid or wrong-kind ID.
    pub fn try_array_def(&self, array_id: ArrayTypeId) -> Option<(Type, u64)> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.try_array_def(array_id)
    }

    /// Intern an array on an invariant-proven semantic-construction path.
    pub fn intern_array_from_type(&self, element_type: Type, len: u64) -> ArrayTypeId {
        self.try_intern_array(element_type, len)
            .expect("array child must be representable in this type pool")
            .as_array()
            .expect("array interning returns an array Type")
    }

    /// Intern a ptr const type from a Type pointee.
    ///
    /// # Panics
    ///
    /// Panics if the pointee type contains a struct/enum that isn't in the pool.
    pub fn intern_ptr_const_from_type(&self, pointee_type: Type) -> PtrConstTypeId {
        self.try_intern_ptr_const(pointee_type)
            .expect("pointer child must be representable in this type pool")
            .as_ptr_const()
            .expect("const-pointer interning returns a const-pointer Type")
    }

    /// Intern a ptr mut type from a Type pointee.
    ///
    /// # Panics
    ///
    /// Panics if the pointee type contains a struct/enum that isn't in the pool.
    pub fn intern_ptr_mut_from_type(&self, pointee_type: Type) -> PtrMutTypeId {
        self.try_intern_ptr_mut(pointee_type)
            .expect("pointer child must be representable in this type pool")
            .as_ptr_mut()
            .expect("mutable-pointer interning returns a mutable-pointer Type")
    }

    /// Get ptr const pointee type if this is a ptr const type.
    pub fn ptr_const_def(&self, ptr_id: PtrConstTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.ptr_const_def(ptr_id)
    }

    /// Get ptr mut pointee type if this is a ptr mut type.
    pub fn ptr_mut_def(&self, ptr_id: PtrMutTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.ptr_mut_def(ptr_id)
    }

    /// Get all struct IDs registered in the pool.
    ///
    /// Returns a vector of all StructId values, useful for iterating over all
    /// structs (e.g., for drop glue synthesis).
    pub fn all_struct_ids(&self) -> Vec<StructId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        // Every index of the whole universe, base included: an overlay's local
        // vector is offset by `base_len` and enumerating it alone would both
        // hide the base's nominals and misnumber the overlay's own.
        (0..inner.entry_count())
            .filter_map(|index| match inner.entry(index) {
                TypeData::DeclaredStruct(_) | TypeData::Struct(_) => {
                    Some(StructId::from_pool_index(
                        checked_pool_index(index).expect("type pool index invariant"),
                    ))
                }
                _ => None,
            })
            .collect()
    }

    /// Get all enum IDs registered in the pool.
    ///
    /// Returns a vector of all EnumId values, useful for iterating over all
    /// enums.
    pub fn all_enum_ids(&self) -> Vec<EnumId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        (0..inner.entry_count())
            .filter_map(|index| match inner.entry(index) {
                TypeData::DeclaredEnum(_) | TypeData::Enum(_) => Some(EnumId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )),
                _ => None,
            })
            .collect()
    }

    /// Get all array IDs registered in the pool.
    ///
    /// Returns a vector of all ArrayTypeId values, useful for iterating over all
    /// arrays (e.g., for drop glue synthesis).
    pub fn all_array_ids(&self) -> Vec<ArrayTypeId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        (0..inner.entry_count())
            .filter_map(|index| match inner.entry(index) {
                TypeData::Array { .. } => Some(ArrayTypeId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )),
                _ => None,
            })
            .collect()
    }

    /// Get the number of composite types in the pool.
    pub fn len(&self) -> usize {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.entry_count()
    }

    /// Composite types this epoch interned itself — what a body-local overlay
    /// actually paid for, excluding everything read from its shared base.
    pub fn local_len(&self) -> usize {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.types.len() + inner.overrides.len()
    }

    /// Composite types read from a shared immutable base without copying.
    pub fn shared_len(&self) -> usize {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.base_len
    }

    /// Rebase this pool in place while preserving the outer handle identity.
    ///
    /// Body analysis can retain an `Rc<TypeInternPool>` while lazy facts append
    /// after containment sealing. Replacing the pool value would leave that
    /// handle observing the old universe; swapping only the locked inner keeps
    /// every handle pointed at the current append-only overlay.
    pub(crate) fn rebase_overlay_in_place(&self) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        *inner = inner.derive_overlay();
    }

    /// Check if the pool is empty (no composite types).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get statistics about the pool contents.
    pub fn stats(&self) -> TypeInternPoolStats {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.stats()
    }

    pub(crate) fn safe_type_name(&self, ty: Type) -> String {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .safe_type_name(ty)
    }

    pub(crate) fn is_copy_type(&self, ty: Type) -> bool {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_copy_type(ty)
    }
}

impl FrozenTypeInternPool {
    pub fn new() -> Self {
        TypeInternPool::new().freeze()
    }

    /// Whether the universe this pool was frozen from ran past the published
    /// composite-type ceiling. See [`TypeInternPool::capacity_exceeded`].
    pub fn capacity_exceeded(&self) -> bool {
        self.inner.capacity_exceeded()
    }

    /// Whether `ty` transitively contains a linear value by value.
    pub fn type_carries_linear(&self, ty: Type) -> bool {
        self.inner
            .validate_complete_root(ty)
            .expect("containment query requires a complete canonical type handle");
        self.inner
            .facts_for_type(ty)
            .expect("frozen type pool has complete containment metadata")
            .carries_linear
    }

    /// Whether dropping `ty` requires a destructor or nested drop glue.
    pub fn type_needs_drop(&self, ty: Type) -> bool {
        self.inner
            .validate_complete_root(ty)
            .expect("containment query requires a complete canonical type handle");
        self.inner
            .facts_for_type(ty)
            .expect("frozen type pool has complete containment metadata")
            .needs_drop
    }

    /// Return the flattened runtime ABI width of `ty` in eight-byte slots.
    pub fn abi_slot_count(&self, ty: Type) -> u32 {
        self.validate_complete_type(ty)
            .expect("backend layout requires a complete, non-recovery type graph");
        self.inner.abi_slot_count(ty)
    }

    /// Canonical physical [`Layout`] of `ty`: the one authority code generation
    /// consumes for byte size, alignment, stride, and field/element/payload
    /// offsets. Reports the compact native layout (ADR-0052): natural scalar
    /// widths and alignments, declaration-order struct fields with padding,
    /// ascending array stride, and smallest-sufficient enum tags.
    pub fn layout(&self, ty: Type) -> Layout {
        self.validate_complete_type(ty)
            .expect("backend layout requires a complete, non-recovery type graph");
        self.inner.layout(ty)
    }

    /// The byte ranges of `ty`'s compact memory image that hold padding rather
    /// than a leaf field (ADR-0052 ruling 5). Code generation zeros exactly these
    /// ranges wherever it materializes a compact image — heap enum stores, sret
    /// buffers, and by-value argument buffers — so the padding is deterministically
    /// zero on construction. Empty for a type with no interior or tail padding
    /// (all-eight-byte-leaf aggregates and scalars).
    pub fn compact_image_padding_ranges(&self, ty: Type) -> Vec<PaddingRange> {
        self.validate_complete_type(ty)
            .expect("backend layout requires a complete, non-recovery type graph");
        self.inner.compact_image_padding_ranges(ty)
    }

    /// Byte offset of a struct field within its aggregate, the shared source for
    /// `@offset_of` and field addressing during lowering.
    pub fn struct_field_offset(&self, struct_id: StructId, field_index: u32) -> u64 {
        self.inner.struct_field_offset(struct_id, field_index)
    }

    /// Byte offset of an enum variant's payload field, the shared source for
    /// `@offset_of`-style physical addressing. Distinct from the slot offset.
    pub fn enum_payload_field_offset(
        &self,
        enum_id: EnumId,
        variant_index: u32,
        field_index: u32,
    ) -> u64 {
        self.inner
            .enum_payload_field_offset(enum_id, variant_index, field_index)
    }

    /// Slot-count offset of a struct field: the internal value-decomposition
    /// offset code generation's slot-based stack/register model uses, kept
    /// independent of the compact physical layout (ADR-0052; RUE-975).
    pub fn struct_field_slot_offset(&self, struct_id: StructId, field_index: u32) -> u32 {
        self.inner.struct_field_slot_offset(struct_id, field_index)
    }

    /// Slot-count offset of an enum variant's payload field: the internal
    /// value-decomposition offset (discriminant slot plus preceding payload
    /// slots), kept independent of the compact physical layout.
    pub fn enum_payload_slot_offset(
        &self,
        enum_id: EnumId,
        variant_index: u32,
        field_index: u32,
    ) -> u32 {
        self.inner
            .enum_payload_slot_offset(enum_id, variant_index, field_index)
    }

    /// Validate a complete type relative to this frozen owner pool.
    ///
    /// Coincidentally equal compact bits from a foreign epoch are not
    /// distinguishable here; artifact branding and durable boundaries establish
    /// ownership before validation.
    pub fn validate_complete_type(&self, ty: Type) -> Result<(), TypeValidationError> {
        if self.success_validation.is_ok() {
            self.inner.validate_complete_root(ty)
        } else {
            // Recovery pools remain queryable while diagnostics are assembled.
            // Preserve their root-specific result instead of letting one
            // invalid entry taint unrelated valid roots.
            self.inner.validate_complete_type(ty)
        }
    }

    /// Validate every pool entry before crossing the successful sema-to-CFG
    /// boundary. Freeze remains recovery-tolerant; the operation-specific
    /// success boundary rejects recovery-only graphs.
    pub fn validate_for_success(&self) -> Result<(), TypeValidationError> {
        self.success_validation
    }

    /// Borrow a completed nominal struct definition without locking or cloning.
    pub fn struct_def(&self, id: StructId) -> &StructDefEntry {
        self.inner.struct_def(id)
    }

    pub fn try_struct_def(&self, id: StructId) -> Option<&StructDefEntry> {
        self.inner.try_struct_def(id)
    }

    /// Borrow a completed nominal enum definition without locking or cloning.
    pub fn enum_def(&self, id: EnumId) -> &EnumDefEntry {
        self.inner.enum_def(id)
    }

    pub fn try_enum_def(&self, id: EnumId) -> Option<&EnumDefEntry> {
        self.inner.try_enum_def(id)
    }

    pub fn array_def(&self, id: ArrayTypeId) -> (Type, u64) {
        self.inner.array_def(id)
    }

    pub fn try_array_def(&self, id: ArrayTypeId) -> Option<(Type, u64)> {
        self.inner.try_array_def(id)
    }

    pub fn ptr_const_def(&self, id: PtrConstTypeId) -> Type {
        self.inner.ptr_const_def(id)
    }

    pub fn ptr_mut_def(&self, id: PtrMutTypeId) -> Type {
        self.inner.ptr_mut_def(id)
    }

    /// Look up an already-completed mutable pointer type without modifying the pool.
    pub fn get_ptr_mut_by_type(&self, pointee_type: Type) -> Option<PtrMutTypeId> {
        self.inner.validate_complete_type(pointee_type).ok()?;
        self.inner.ptr_mut_map.get(&pointee_type)?.as_ptr_mut()
    }

    pub fn struct_lang_item(&self, id: StructId) -> Option<LangItem> {
        self.inner.struct_lang_items.get(&id).copied()
    }

    /// Whether this struct was generated for an anonymous type (RUE-1050).
    pub fn is_anonymous_struct(&self, id: StructId) -> bool {
        self.inner.is_anonymous_struct(id)
    }

    /// Whether this enum was generated for an anonymous type (RUE-1050).
    pub fn is_anonymous_enum(&self, id: EnumId) -> bool {
        self.inner.is_anonymous_enum(id)
    }

    pub fn lang_item_type(&self, item: LangItem) -> Option<Type> {
        self.inner
            .lang_item_structs
            .get(&item)
            .copied()
            .map(Type::new_struct)
    }

    pub fn is_strbuf(&self, id: StructId) -> bool {
        self.struct_lang_item(id) == Some(LangItem::StrBuf)
    }

    /// Whether a struct carries the `@repr(c)` guarantee marker (ADR-0064
    /// Amendment 1). The marker set during semantic analysis travels into the
    /// frozen pool for the FFI predicates and the classifier.
    pub fn is_struct_repr_c(&self, id: StructId) -> bool {
        self.inner.repr_c_structs.contains(&id)
    }

    pub fn struct_symbol_name(&self, id: StructId) -> String {
        self.inner.struct_symbol_name(id)
    }

    pub fn enum_symbol_name(&self, id: EnumId) -> String {
        self.inner.enum_symbol_name(id)
    }

    pub fn all_struct_ids(&self) -> impl Iterator<Item = StructId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter(|(_, data)| matches!(data, TypeData::Struct(_)))
            .map(|(index, _)| {
                StructId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )
            })
    }

    pub fn all_enum_ids(&self) -> impl Iterator<Item = EnumId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter(|(_, data)| matches!(data, TypeData::Enum(_)))
            .map(|(index, _)| {
                EnumId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )
            })
    }

    pub fn all_array_ids(&self) -> impl Iterator<Item = ArrayTypeId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter(|(_, data)| matches!(data, TypeData::Array { .. }))
            .map(|(index, _)| {
                ArrayTypeId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )
            })
    }

    pub fn all_ptr_const_ids(&self) -> impl Iterator<Item = PtrConstTypeId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter(|(_, data)| matches!(data, TypeData::PtrConst { .. }))
            .map(|(index, _)| {
                PtrConstTypeId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )
            })
    }

    pub fn all_ptr_mut_ids(&self) -> impl Iterator<Item = PtrMutTypeId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter(|(_, data)| matches!(data, TypeData::PtrMut { .. }))
            .map(|(index, _)| {
                PtrMutTypeId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )
            })
    }

    /// Iterate over every canonical composite type in pool storage order.
    ///
    /// The returned [`Type`] handles preserve global allocation order without
    /// exposing the raw pool positions used to encode their typed payloads.
    pub fn all_types(&self) -> impl ExactSizeIterator<Item = Type> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .map(|(index, data)| complete_type_handle(index, data))
    }

    pub fn len(&self) -> usize {
        self.inner.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.types.is_empty()
    }

    pub fn stats(&self) -> TypeInternPoolStats {
        self.inner.stats()
    }

    pub(crate) fn safe_type_name(&self, ty: Type) -> String {
        self.inner.safe_type_name(ty)
    }

    pub(crate) fn is_copy_type(&self, ty: Type) -> bool {
        self.inner.is_copy_type(ty)
    }
}

impl Default for FrozenTypeInternPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for TypeInternPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for TypeInternPool {
    /// Clone the pool by copying all type data into a new pool.
    ///
    /// This is used when analysis needs an independent copy of the pool while
    /// preserving the already-interned type data.
    fn clone(&self) -> Self {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        Self {
            inner: RwLock::new(inner.clone()),
        }
    }
}

impl crate::ffi_predicates::FfiTypePool for TypeInternPool {
    fn ffi_struct_is_repr_c(&self, id: StructId) -> bool {
        self.is_struct_repr_c(id)
    }
    fn ffi_struct_is_linear(&self, id: StructId) -> bool {
        self.struct_def(id).is_linear
    }
    fn ffi_struct_has_destructor(&self, id: StructId) -> bool {
        self.struct_def(id).destructor.is_some()
    }
    fn ffi_struct_fields(&self, id: StructId) -> Vec<(String, Type)> {
        self.struct_def(id)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect()
    }
    fn ffi_array_element(&self, id: ArrayTypeId) -> Type {
        self.array_def(id).0
    }
}

impl crate::ffi_predicates::FfiTypePool for FrozenTypeInternPool {
    fn ffi_struct_is_repr_c(&self, id: StructId) -> bool {
        self.is_struct_repr_c(id)
    }
    fn ffi_struct_is_linear(&self, id: StructId) -> bool {
        self.struct_def(id).is_linear
    }
    fn ffi_struct_has_destructor(&self, id: StructId) -> bool {
        self.struct_def(id).destructor.is_some()
    }
    fn ffi_struct_fields(&self, id: StructId) -> Vec<(String, Type)> {
        self.struct_def(id)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect()
    }
    fn ffi_array_element(&self, id: ArrayTypeId) -> Type {
        self.array_def(id).0
    }
}

/// Statistics about the intern pool contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeInternPoolStats {
    pub struct_count: usize,
    pub enum_count: usize,
    pub array_count: usize,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StructField;
    use lasso::ThreadedRodeo;

    // ========================================================================
    // TypeInternPool tests
    // ========================================================================

    #[test]
    fn test_pool_new() {
        let pool = TypeInternPool::new();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    fn struct_def(name: &str, fields: Vec<StructField>) -> StructDef {
        StructDef {
            name: name.into(),
            fields,
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: FileId::DEFAULT,
        }
    }

    fn enum_def(name: &str) -> EnumDef {
        EnumDef {
            name: name.into(),
            variants: Arc::from([]),
            variant_payloads: vec![],
            is_pub: false,
            file_id: FileId::DEFAULT,
        }
    }

    #[test]
    fn published_composite_type_ceiling_matches_the_payload_width() {
        // Spec Appendix C.6:1: a live `Type` is a u32 with an 8-bit kind tag
        // and a 24-bit pool index, so the pool addresses 2^24 entries.
        assert_eq!(MAX_COMPOSITE_TYPES, 16_777_216);
        assert_eq!(MAX_COMPOSITE_TYPES, type_encoding::MAX_PAYLOAD + 1);
    }

    #[test]
    fn ordinary_interning_never_latches_the_composite_type_ceiling() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        assert!(!pool.capacity_exceeded());
        pool.register_struct(interner.get_or_intern("Pair"), struct_def("Pair", vec![]));
        pool.try_intern_array(Type::I32, 4).unwrap();
        pool.try_intern_ptr_const(Type::I32).unwrap();
        assert!(!pool.capacity_exceeded());
        assert!(!pool.freeze().capacity_exceeded());
    }

    /// Drive the pool into the state `next_pool_index` reaches once the
    /// composite-type ceiling is exhausted, without materializing 2^24 entries.
    fn latch_composite_type_ceiling(pool: &TypeInternPool) {
        pool.inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .capacity_exceeded = true;
    }

    #[test]
    fn latched_pool_degrades_wrong_kind_definition_reads_instead_of_aborting() {
        // RUE-1226 / spec C.1:2: between the capacity latch and the boundary
        // that reports E1401, registrations alias the final entry, so a handle
        // can carry a kind tag the entry it names does not have. Reading one
        // must not abort the compiler.
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (struct_id, _) = pool.register_struct(
            interner.get_or_intern("Owner"),
            struct_def(
                "Owner",
                vec![StructField {
                    name: "value".into(),
                    ty: Type::I32,
                }],
            ),
        );
        latch_composite_type_ceiling(&pool);

        let aliased_enum = EnumId::from_pool_index(struct_id.pool_index());
        assert_eq!(pool.enum_def(aliased_enum).variant_count(), 0);

        let frozen = pool.freeze();
        assert!(frozen.capacity_exceeded());
        assert_eq!(frozen.enum_def(aliased_enum).variant_count(), 0);
        assert_eq!(frozen.struct_def(struct_id).fields.len(), 1);
        // Every other `&`-returning definition accessor degrades the same way,
        // so a containment or ABI walk that meets an aliased handle terminates.
        assert_eq!(
            frozen.array_def(ArrayTypeId::from_pool_index(struct_id.pool_index())),
            (Type::ERROR, 0)
        );
        assert_eq!(
            frozen.ptr_const_def(PtrConstTypeId::from_pool_index(struct_id.pool_index())),
            Type::ERROR
        );
        assert_eq!(
            frozen.ptr_mut_def(PtrMutTypeId::from_pool_index(struct_id.pool_index())),
            Type::ERROR
        );
        // The aliased enum is walkable: no payload, no further aliased reads,
        // so an ABI or containment walk that meets it terminates rather than
        // recursing into a mistyped entry.
        assert_eq!(frozen.inner.abi_slot_count(Type::new_enum(aliased_enum)), 1);
        // The validating, backend-facing entry points still refuse the aliased
        // handle. They are not reached in a latched compilation: declaration
        // binding stops it, and the per-body CFG query backstops that before it
        // projects domains or queries layouts.
        assert_eq!(
            frozen
                .inner
                .validate_complete_type(Type::new_enum(aliased_enum)),
            Err(TypeValidationError::KindMismatch)
        );
    }

    #[test]
    #[should_panic(expected = "Expected complete enum at pool index")]
    fn a_wrong_kind_handle_without_the_latch_is_still_a_producer_bug() {
        // The degradation above is scoped to the latch window; a kind mismatch
        // in a healthy pool stays loud, so the ICE surface does not grow.
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (struct_id, _) =
            pool.register_struct(interner.get_or_intern("Owner"), struct_def("Owner", vec![]));
        let _ = pool.enum_def(EnumId::from_pool_index(struct_id.pool_index()));
    }

    #[test]
    fn latched_pool_skips_destructor_metadata_finalization() {
        // `set_struct_destructor` asserts against the definition it finds, but
        // inside the latch window the handle need not name it. The compilation
        // is already failing with E1401; the finalization is skipped instead.
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (struct_id, _) =
            pool.register_struct(interner.get_or_intern("Owner"), struct_def("Owner", vec![]));
        latch_composite_type_ceiling(&pool);
        pool.set_struct_destructor(struct_id, "Owner.__drop".to_string());
        assert!(pool.struct_def(struct_id).destructor.is_none());
    }

    #[test]
    fn checked_pool_index_enforces_type_payload_capacity() {
        let maximum = type_encoding::MAX_PAYLOAD as usize;
        assert_eq!(
            checked_pool_index(maximum),
            Some(type_encoding::MAX_PAYLOAD)
        );
        assert_eq!(checked_pool_index(maximum + 1), None);
    }

    #[test]
    fn declared_struct_has_identity_before_single_completion() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Node");
        let (id, is_new) = pool.declare_struct(name, struct_def("Node", vec![]));
        assert!(is_new);

        let interned = Type::new_struct(id);
        assert!(pool.get(interned).is_none());
        assert!(pool.is_struct(interned));
        assert!(pool.get_struct_def(interned).is_none());
        assert!(pool.try_struct_def(id).is_none());
        assert_eq!(&*pool.struct_declaration_metadata(id).unwrap().name, "Node");
        assert_eq!(
            pool.validate_complete_type(interned),
            Err(TypeValidationError::IncompleteDefinition)
        );

        // The declared identity is legal in a recursive pointer graph before
        // the nominal definition completes.
        let next_id = pool.intern_ptr_mut_from_type(Type::new_struct(id));
        let next = Type::new_ptr_mut(next_id);
        pool.complete_declared_struct(
            id,
            struct_def(
                "Node",
                vec![StructField {
                    name: "next".into(),
                    ty: next,
                }],
            ),
        );

        assert!(matches!(pool.get(interned), Some(TypeData::Struct(_))));
        assert_eq!(pool.get_struct_def(interned).unwrap().fields[0].ty, next);
        let frozen = pool.freeze();
        assert_eq!(frozen.ptr_mut_def(next_id), Type::new_struct(id));
    }

    #[test]
    #[should_panic(expected = "is not a declared struct entry")]
    fn declared_struct_cannot_complete_twice() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Once");
        let (id, _) = pool.declare_struct(name, struct_def("Once", vec![]));
        pool.complete_declared_struct(id, struct_def("Once", vec![]));
        pool.complete_declared_struct(id, struct_def("Once", vec![]));
    }

    #[test]
    #[should_panic(expected = "completed struct changed textual name")]
    fn declared_struct_completion_rejects_name_change() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Before");
        let (id, _) = pool.declare_struct(name, struct_def("Before", vec![]));
        pool.complete_declared_struct(id, struct_def("After", vec![]));
    }

    #[test]
    #[should_panic(expected = "is not a declared enum entry")]
    fn declared_enum_cannot_complete_twice() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Once");
        let (id, _) = pool.declare_enum(name, enum_def("Once"));
        pool.complete_declared_enum(id, enum_def("Once"));
        pool.complete_declared_enum(id, enum_def("Once"));
    }

    #[test]
    #[should_panic(expected = "completed enum changed textual name")]
    fn declared_enum_completion_rejects_name_change() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Before");
        let (id, _) = pool.declare_enum(name, enum_def("Before"));
        pool.complete_declared_enum(id, enum_def("After"));
    }

    #[test]
    #[should_panic(expected = "is not a declared struct entry")]
    fn declared_completion_rejects_wrong_nominal_kind() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Choice");
        let (id, _) = pool.declare_enum(name, enum_def("Choice"));
        pool.complete_declared_struct(
            StructId::from_pool_index(id.pool_index()),
            struct_def("Choice", vec![]),
        );
    }

    #[test]
    #[should_panic(expected = "cannot freeze incomplete type-pool entry")]
    fn freeze_rejects_declared_entry() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Later");
        pool.declare_struct(name, struct_def("Later", vec![]));
        let _ = pool.freeze();
    }

    #[test]
    #[should_panic(expected = "cannot freeze incomplete type-pool entry")]
    fn freeze_rejects_reserved_entry() {
        let pool = TypeInternPool::new();
        pool.reserve_struct_id();
        let _ = pool.freeze();
    }

    #[test]
    fn error_recovery_structural_types_may_freeze() {
        let pool = TypeInternPool::new();
        let array_id = pool.intern_array_from_type(Type::ERROR, 1);
        let frozen = pool.freeze();
        assert_eq!(frozen.len(), 1);
        assert_eq!(frozen.array_def(array_id), (Type::ERROR, 1));
        assert_eq!(
            frozen.validate_for_success(),
            Err(TypeValidationError::RecoveryType)
        );
        assert_eq!(frozen.validate_complete_type(Type::I64), Ok(()));
        assert_eq!(
            frozen.validate_complete_type(Type::new_array(array_id)),
            Err(TypeValidationError::RecoveryType)
        );
    }

    #[test]
    fn frozen_success_certificate_preserves_exact_root_validation() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (leaf, _) = pool.register_struct(
            declarations.get_or_intern("Leaf"),
            struct_def(
                "Leaf",
                vec![StructField {
                    name: "value".into(),
                    ty: Type::I64,
                }],
            ),
        );
        let array = pool.try_intern_array(Type::new_struct(leaf), 4).unwrap();
        let frozen = pool.freeze();

        assert_eq!(frozen.success_validation, Ok(()));
        assert_eq!(frozen.validate_for_success(), Ok(()));
        assert_eq!(frozen.validate_complete_type(array), Ok(()));
        assert_eq!(frozen.abi_slot_count(array), 4);

        let wrong_kind = Type::new_enum(EnumId::from_pool_index(leaf.pool_index()));
        assert_eq!(
            frozen.validate_complete_type(wrong_kind),
            Err(TypeValidationError::KindMismatch)
        );
        let out_of_range = Type::new_struct(StructId::from_pool_index(10_000));
        assert_eq!(
            frozen.validate_complete_type(out_of_range),
            Err(TypeValidationError::PoolIndexOutOfRange)
        );
    }

    #[test]
    fn public_layout_rejects_incomplete_and_recovery_graphs() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = declarations.get_or_intern("Later");
        let (declared, _) = pool.declare_struct(name, struct_def("Later", vec![]));
        let declared_ty = Type::new_struct(declared);

        assert_eq!(
            pool.try_abi_slot_count(declared_ty),
            Err(TypeValidationError::IncompleteDefinition)
        );

        let recovery_array = pool.try_intern_array(Type::ERROR, 3).unwrap();
        assert_eq!(
            pool.try_abi_slot_count(recovery_array),
            Err(TypeValidationError::RecoveryType)
        );
        assert_eq!(pool.provisional_abi_slot_count(recovery_array), 3);
    }

    #[test]
    fn checked_structural_interning_fails_closed_for_illegal_children() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = declarations.get_or_intern("Owner");
        let (owner, _) = pool.register_struct(name, struct_def("Owner", vec![]));

        assert_eq!(
            pool.try_intern_array(Type::COMPTIME_TYPE, 1),
            Err(TypeValidationError::ComptimeStructuralChild)
        );
        assert_eq!(
            pool.try_intern_ptr_const(Type::new_module(crate::ModuleId::new(0))),
            Err(TypeValidationError::ModuleStructuralChild)
        );
        assert_eq!(
            pool.try_intern_ptr_mut(Type::from_u32(13)),
            Err(TypeValidationError::InvalidEncoding)
        );
        assert_eq!(
            pool.try_intern_array(Type::new_array(ArrayTypeId::from_pool_index(owner.0)), 1),
            Err(TypeValidationError::KindMismatch)
        );
        assert_eq!(
            pool.try_intern_array(Type::new_struct(StructId::from_pool_index(99)), 1),
            Err(TypeValidationError::PoolIndexOutOfRange)
        );

        let reserved = pool.reserve_struct_id();
        assert_eq!(
            pool.try_intern_ptr_mut(Type::new_struct(reserved)),
            Err(TypeValidationError::ReservedEntry)
        );
    }

    #[test]
    fn freeze_preserves_complete_nominals_and_borrows_stable_definitions() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = declarations.get_or_intern("Owner");
        let (owner, _) = pool.register_struct(
            name,
            StructDef {
                name: "Owner".into(),
                fields: vec![StructField {
                    name: "value".into(),
                    ty: Type::I64,
                }],
                is_copy: false,
                is_linear: false,
                destructor: Some("Owner.__drop".into()),
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let owner_type = Type::new_struct(owner);
        let mutable_symbol = pool.struct_symbol_name(owner);
        let mutable_name = owner_type.safe_name_with_pool(Some(&pool));
        let mutable_slots = pool.abi_slot_count(owner_type);
        let mutable_stats = pool.stats();

        let frozen = pool.freeze();
        let first = frozen.struct_def(owner);
        let second = frozen.struct_def(owner);
        assert!(std::ptr::eq(first, second));
        assert_eq!(frozen.all_struct_ids().collect::<Vec<_>>(), [owner]);
        assert_eq!(frozen.struct_symbol_name(owner), mutable_symbol);
        assert_eq!(
            owner_type.safe_name_with_frozen_pool(Some(&frozen)),
            mutable_name
        );
        assert_eq!(frozen.abi_slot_count(owner_type), mutable_slots);
        assert_eq!(frozen.stats(), mutable_stats);

        // Destructor provenance crosses the boundary as a stable string. A
        // backend request chooses its own symbol universe and interns it there.
        let request_symbols = ThreadedRodeo::default();
        let destructor = first.destructor.as_deref().unwrap();
        let request_symbol = request_symbols.get_or_intern(destructor);
        assert_eq!(request_symbols.resolve(&request_symbol), "Owner.__drop");
    }

    #[test]
    fn struct_metadata_finalization_is_narrow_and_monotonic() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let mut owner_def = struct_def(
            "Owner",
            vec![StructField {
                name: "value".into(),
                ty: Type::I64,
            }],
        );
        owner_def.is_linear = true;
        let (owner, _) = pool.register_struct(declarations.get_or_intern("Owner"), owner_def);

        pool.set_struct_destructor(owner, "Owner.__drop".into());

        let def = pool.struct_def(owner);
        assert!(def.is_linear);
        assert_eq!(def.destructor.as_deref(), Some("Owner.__drop"));
        assert_eq!(&*def.name, "Owner");
        assert_eq!(def.fields.len(), 1);
        assert_eq!(def.fields[0].ty, Type::I64);
    }

    #[test]
    fn containment_metadata_work_is_linear_for_eight_thousand_types() {
        const COUNT: usize = 8_000;
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let ids = (0..COUNT)
            .map(|_| pool.reserve_struct_id())
            .collect::<Vec<_>>();

        for (index, &id) in ids.iter().enumerate() {
            let name = format!("Chain{index}");
            let fields = ids
                .get(index + 1)
                .map(|&next| {
                    vec![StructField {
                        name: "next".into(),
                        ty: Type::new_struct(next),
                    }]
                })
                .unwrap_or_default();
            let mut def = struct_def(&name, fields);
            if index + 1 == COUNT {
                def.is_linear = true;
                def.destructor = Some(format!("{name}.__drop").into());
            }
            pool.complete_struct_registration(id, declarations.get_or_intern(&name), def);
        }

        let work = pool.finalize_containment_metadata().unwrap();
        assert_eq!(work.nodes, COUNT);
        assert_eq!(work.edges, COUNT - 1);
        assert!(pool.type_carries_linear(Type::new_struct(ids[0])));
        assert!(pool.type_needs_drop(Type::new_struct(ids[0])));
        assert_eq!(
            pool.finalize_containment_metadata().unwrap(),
            TypeContainmentWork::default(),
            "an unchanged containment graph must not be rescanned"
        );
        for _ in 0..128 {
            assert_eq!(
                pool.finalize_containment_metadata().unwrap(),
                TypeContainmentWork::default()
            );
        }
        assert_eq!(
            pool.containment_metrics(),
            TypeContainmentMetrics {
                finalize_checks: 130,
                nodes: COUNT,
                edges: COUNT - 1,
            },
            "aggregate meters prove every clean check was O(1) and graph work ran once"
        );
    }

    #[test]
    fn provider_style_declare_complete_batches_settle_without_graph_work() {
        const COUNT: usize = 4_000;
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let ids = (0..COUNT)
            .map(|index| {
                let name = format!("Imported{index}");
                pool.declare_struct(
                    declarations.get_or_intern(&name),
                    struct_def(&name, Vec::new()),
                )
                .0
            })
            .collect::<Vec<_>>();

        // Durable provider materialization declares recursive shells first,
        // then completes them while unwinding, so every completion sees its
        // by-value children already finalized. Each derives its facts
        // incrementally at completion; no accessor ever pays a graph walk.
        for (index, &id) in ids.iter().enumerate().rev() {
            let name = format!("Imported{index}");
            let fields = ids
                .get(index + 1)
                .map(|&child| {
                    vec![StructField {
                        name: "child".into(),
                        ty: Type::new_struct(child),
                    }]
                })
                .unwrap_or_default();
            pool.complete_declared_struct(id, struct_def(&name, fields));
        }

        assert_eq!(pool.pending_containment_facts(), 0);
        assert_eq!(
            pool.finalize_containment_metadata().unwrap(),
            TypeContainmentWork::default()
        );
        assert_eq!(
            pool.finalize_containment_metadata().unwrap(),
            TypeContainmentWork::default()
        );
        assert_eq!(
            pool.containment_metrics(),
            TypeContainmentMetrics {
                finalize_checks: 2,
                nodes: 0,
                edges: 0,
            },
            "completed declare/complete pairs must not trigger a full pass"
        );
        assert!(!pool.type_needs_drop(Type::new_struct(ids[0])));
    }

    #[test]
    fn out_of_dependency_order_completion_falls_back_to_the_full_pass() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (a, _) = pool.declare_struct(
            declarations.get_or_intern("Outer"),
            struct_def("Outer", Vec::new()),
        );
        let (b, _) = pool.declare_struct(
            declarations.get_or_intern("Inner"),
            struct_def("Inner", Vec::new()),
        );
        assert_eq!(pool.pending_containment_facts(), 2);

        // Completing the parent while its by-value child is still a factless
        // shell cannot derive facts incrementally; the parent stays factless
        // and reads of it fail closed.
        pool.complete_declared_struct(
            a,
            struct_def(
                "Outer",
                vec![StructField {
                    name: "inner".into(),
                    ty: Type::new_struct(b),
                }],
            ),
        );
        assert_eq!(pool.pending_containment_facts(), 2);
        assert_eq!(pool.try_type_needs_drop(Type::new_struct(a)), None);

        let mut inner_def = struct_def("Inner", Vec::new());
        inner_def.destructor = Some("Inner.__drop".into());
        pool.complete_declared_struct(b, inner_def);
        assert_eq!(pool.pending_containment_facts(), 1);
        assert_eq!(pool.try_type_needs_drop(Type::new_struct(b)), Some(true));
        assert_eq!(pool.try_type_needs_drop(Type::new_struct(a)), None);

        // The canonical full pass computes the out-of-order parent.
        let work = pool.finalize_containment_metadata().unwrap();
        assert_eq!(work.nodes, 2);
        assert_eq!(work.edges, 1);
        assert_eq!(pool.pending_containment_facts(), 0);
        assert!(pool.type_needs_drop(Type::new_struct(a)));
        assert_eq!(pool.abi_slot_count(Type::new_struct(a)), 0);
        assert_eq!(
            pool.finalize_containment_metadata().unwrap(),
            TypeContainmentWork::default()
        );
    }

    #[test]
    fn destructor_in_completion_matches_late_destructor_metadata() {
        // The mint paths fold a known destructor into the completing
        // definition; the frontend attaches it after completion through
        // `set_struct_destructor`. Both must derive identical facts.
        fn build(pool: &TypeInternPool, declarations: &ThreadedRodeo, late: bool) -> (Type, Type) {
            let (owner, _) = pool.declare_struct(
                declarations.get_or_intern("Owner"),
                struct_def("Owner", Vec::new()),
            );
            let mut def = struct_def(
                "Owner",
                vec![StructField {
                    name: "value".into(),
                    ty: Type::I64,
                }],
            );
            if !late {
                def.destructor = Some("Owner.__drop".into());
            }
            pool.complete_declared_struct(owner, def);
            if late {
                pool.set_struct_destructor(owner, "Owner.__drop".into());
            }
            let (wrapper, _) = pool.register_struct(
                declarations.get_or_intern("Wrapper"),
                struct_def(
                    "Wrapper",
                    vec![StructField {
                        name: "owner".into(),
                        ty: Type::new_struct(owner),
                    }],
                ),
            );
            pool.finalize_containment_metadata().unwrap();
            (Type::new_struct(owner), Type::new_struct(wrapper))
        }

        let declarations = ThreadedRodeo::default();
        let folded_pool = TypeInternPool::new();
        let late_pool = TypeInternPool::new();
        let (folded_owner, folded_wrapper) = build(&folded_pool, &declarations, false);
        let (late_owner, late_wrapper) = build(&late_pool, &declarations, true);

        for (folded, late) in [(folded_owner, late_owner), (folded_wrapper, late_wrapper)] {
            assert_eq!(
                folded_pool.type_needs_drop(folded),
                late_pool.type_needs_drop(late)
            );
            assert_eq!(
                folded_pool.type_carries_linear(folded),
                late_pool.type_carries_linear(late)
            );
            assert_eq!(
                folded_pool.abi_slot_count(folded),
                late_pool.abi_slot_count(late)
            );
        }
        let folded_def = folded_pool.struct_def(folded_owner.as_struct().unwrap());
        let late_def = late_pool.struct_def(late_owner.as_struct().unwrap());
        assert_eq!(folded_def.destructor, late_def.destructor);
        assert_eq!(folded_def.is_copy, late_def.is_copy);
        assert_eq!(folded_def.is_linear, late_def.is_linear);
        assert!(folded_pool.type_needs_drop(folded_wrapper));

        // The folded path needed no graph walk at all; the late path paid
        // exactly one full pass for the staleness `set_struct_destructor`
        // introduced.
        assert_eq!(
            folded_pool.containment_metrics(),
            TypeContainmentMetrics {
                finalize_checks: 1,
                nodes: 0,
                edges: 0,
            }
        );
        assert_eq!(
            late_pool.containment_metrics(),
            TypeContainmentMetrics {
                finalize_checks: 1,
                nodes: 2,
                edges: 1,
            }
        );
    }

    #[test]
    fn late_destructor_marks_facts_stale_and_updates_ancestors() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (owner, _) = pool.register_struct(
            declarations.get_or_intern("Owner"),
            struct_def("Owner", Vec::new()),
        );
        let (wrapper, _) = pool.register_struct(
            declarations.get_or_intern("Wrapper"),
            struct_def(
                "Wrapper",
                vec![StructField {
                    name: "owner".into(),
                    ty: Type::new_struct(owner),
                }],
            ),
        );
        assert_eq!(
            pool.finalize_containment_metadata().unwrap(),
            TypeContainmentWork::default()
        );
        assert_eq!(
            pool.try_type_needs_drop(Type::new_struct(wrapper)),
            Some(false)
        );

        // Present facts may now be wrong pool-wide, so reads fail closed even
        // though no entry is factless, and the next finalization re-walks the
        // graph to repropagate the destructor into ancestors.
        pool.set_struct_destructor(owner, "Owner.__drop".into());
        assert_eq!(pool.pending_containment_facts(), 0);
        assert_eq!(pool.try_type_needs_drop(Type::new_struct(wrapper)), None);
        assert_eq!(pool.try_type_needs_drop(Type::new_struct(owner)), None);

        let work = pool.finalize_containment_metadata().unwrap();
        assert_eq!(work.nodes, 2);
        assert_eq!(work.edges, 1);
        assert!(pool.type_needs_drop(Type::new_struct(owner)));
        assert!(pool.type_needs_drop(Type::new_struct(wrapper)));
    }

    #[test]
    fn provider_style_late_composites_interleaved_with_access_stay_constant_work() {
        const COUNT: usize = 4_000;
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let mut child = Type::I64;

        // Late generated composites are complete at insertion time. Their facts
        // derive from the already-finalized child, so an interleaved accessor
        // performs one O(1) dirty check rather than rescanning the accumulated
        // graph. This is the adversarial shape that would expose O(k²) work.
        for index in 0..COUNT {
            let name = format!("Late{index}");
            let mut def = struct_def(
                &name,
                vec![StructField {
                    name: "child".into(),
                    ty: child,
                }],
            );
            if index == 0 {
                def.is_linear = true;
                def.destructor = Some(format!("{name}.__drop").into());
            }
            let (id, inserted) = pool.register_struct(declarations.get_or_intern(&name), def);
            assert!(inserted);
            assert_eq!(
                pool.finalize_containment_metadata().unwrap(),
                TypeContainmentWork::default()
            );
            child = Type::new_struct(id);
            assert!(pool.type_carries_linear(child));
            assert!(pool.type_needs_drop(child));
        }

        assert_eq!(
            pool.containment_metrics(),
            TypeContainmentMetrics {
                finalize_checks: COUNT,
                nodes: 0,
                edges: 0,
            },
            "interleaved late-composite access must not rescan prior nodes"
        );
    }

    #[test]
    fn late_types_derive_facts_and_zero_arrays_and_pointers_terminate_containment() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let mut resource = struct_def("Resource", vec![]);
        resource.is_linear = true;
        resource.destructor = Some("Resource.__drop".into());
        let (resource, _) = pool.register_struct(declarations.get_or_intern("Resource"), resource);
        pool.finalize_containment_metadata().unwrap();

        let empty_array = pool
            .try_intern_array(Type::new_struct(resource), 0)
            .unwrap();
        let one_array = pool
            .try_intern_array(Type::new_struct(resource), 1)
            .unwrap();
        let pointer = pool.try_intern_ptr_mut(Type::new_struct(resource)).unwrap();
        let choice = EnumDef {
            name: "Choice".into(),
            variants: Arc::from(["Some".into(), "None".into()]),
            variant_payloads: vec![vec![one_array], vec![pointer]],
            is_pub: false,
            file_id: FileId::DEFAULT,
        };
        let (choice, _) = pool.register_enum(declarations.get_or_intern("Choice"), choice);
        let (wrapper, _) = pool.register_struct(
            declarations.get_or_intern("Wrapper"),
            struct_def(
                "Wrapper",
                vec![StructField {
                    name: "choice".into(),
                    ty: Type::new_enum(choice),
                }],
            ),
        );

        assert!(!pool.type_carries_linear(empty_array));
        assert!(!pool.type_needs_drop(empty_array));
        assert!(pool.type_carries_linear(one_array));
        assert!(pool.type_needs_drop(one_array));
        assert!(!pool.type_carries_linear(pointer));
        assert!(!pool.type_needs_drop(pointer));
        assert!(pool.type_carries_linear(Type::new_enum(choice)));
        assert!(pool.type_needs_drop(Type::new_enum(choice)));
        assert!(pool.struct_def(wrapper).is_linear);

        let frozen = pool.freeze();
        assert!(frozen.type_carries_linear(Type::new_struct(wrapper)));
        assert!(frozen.type_needs_drop(Type::new_struct(wrapper)));
    }

    #[test]
    fn frozen_abi_widths_share_canonical_containment_facts() {
        assert_eq!(std::mem::size_of::<Spur>(), 4);
        assert_eq!(std::mem::size_of::<StructData>(), 16);
        assert_eq!(std::mem::size_of::<EnumData>(), 16);
        assert_eq!(std::mem::size_of::<TypeData>(), 24);
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (pair, _) = pool.register_struct(
            declarations.get_or_intern("Pair"),
            struct_def(
                "Pair",
                vec![
                    StructField {
                        name: "left".into(),
                        ty: Type::I64,
                    },
                    StructField {
                        name: "right".into(),
                        ty: Type::U8,
                    },
                ],
            ),
        );
        // Force the graph-wide finalization path. Every type registered below
        // starts without facts and is derived once in canonical postorder.
        pool.set_struct_destructor(pair, "Pair.__drop".into());
        let pairs = pool.try_intern_array(Type::new_struct(pair), 3).unwrap();
        let saturated = pool.try_intern_array(Type::I64, u64::MAX).unwrap();
        let pointer = pool.try_intern_ptr_const(Type::new_struct(pair)).unwrap();
        let choice = EnumDef {
            name: "Choice".into(),
            variants: Arc::from(["Many".into(), "One".into()]),
            variant_payloads: vec![vec![pairs, Type::I8], vec![pointer]],
            is_pub: false,
            file_id: FileId::DEFAULT,
        };
        let (choice, _) = pool.register_enum(declarations.get_or_intern("Choice"), choice);
        let (wrapper, _) = pool.register_struct(
            declarations.get_or_intern("Wrapper"),
            struct_def(
                "Wrapper",
                vec![
                    StructField {
                        name: "choice".into(),
                        ty: Type::new_enum(choice),
                    },
                    StructField {
                        name: "pointer".into(),
                        ty: pointer,
                    },
                ],
            ),
        );

        pool.finalize_containment_metadata().unwrap();
        let frozen = pool.freeze();
        let pair_ty = Type::new_struct(pair);
        let choice_ty = Type::new_enum(choice);
        let wrapper_ty = Type::new_struct(wrapper);

        assert_eq!(frozen.inner.stored_abi_slot_count(pair_ty), Some(2));
        assert_eq!(frozen.inner.stored_abi_slot_count(pairs), Some(6));
        assert_eq!(
            frozen.inner.stored_abi_slot_count(saturated),
            Some(u32::MAX)
        );
        assert_eq!(frozen.inner.stored_abi_slot_count(pointer), Some(1));
        assert_eq!(frozen.inner.stored_abi_slot_count(choice_ty), Some(8));
        assert_eq!(frozen.inner.stored_abi_slot_count(wrapper_ty), Some(9));
        assert_eq!(frozen.abi_slot_count(wrapper_ty), 9);
        assert_eq!(frozen.struct_field_slot_offset(wrapper, 1), 8);
        assert_eq!(frozen.enum_payload_slot_offset(choice, 0, 1), 7);
    }

    #[test]
    #[should_panic(expected = "complete canonical type handle")]
    fn frozen_containment_queries_reject_wrong_kind_handles() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (owner, _) = pool.register_struct(
            declarations.get_or_intern("Owner"),
            struct_def("Owner", vec![]),
        );
        let frozen = pool.freeze();
        let wrong_kind = Type::new_array(ArrayTypeId::from_pool_index(owner.pool_index()));
        let _ = frozen.type_needs_drop(wrong_kind);
    }

    #[test]
    #[should_panic(expected = "struct destructor metadata can only be assigned once")]
    fn struct_destructor_metadata_cannot_be_assigned_twice() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (owner, _) = pool.register_struct(
            declarations.get_or_intern("Owner"),
            struct_def("Owner", vec![]),
        );
        pool.set_struct_destructor(owner, "Owner.__drop".into());
        pool.set_struct_destructor(owner, "Owner.__drop".into());
    }

    #[test]
    fn frozen_all_types_preserves_global_storage_order_without_exposing_positions() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let (owner, _) = pool.register_struct(
            declarations.get_or_intern("Owner"),
            struct_def("Owner", vec![]),
        );
        let array = pool.try_intern_array(Type::new_struct(owner), 3).unwrap();
        pool.rebase_overlay_in_place();
        let (choice, _) =
            pool.register_enum(declarations.get_or_intern("Choice"), enum_def("Choice"));
        let pointer = pool.try_intern_ptr_const(array).unwrap();

        let expected = [
            Type::new_struct(owner),
            array,
            Type::new_enum(choice),
            pointer,
        ];
        assert_eq!(pool.complete_type_handles(), expected);

        let frozen = pool.freeze();
        assert_eq!(frozen.all_types().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn test_pool_register_struct() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Point");

        let def = StructDef {
            name: "Point".into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        let (struct_id, is_new) = pool.register_struct(name, def.clone());
        assert!(is_new);
        assert_eq!(struct_id.pool_index(), 0); // First entry in pool
        assert_eq!(pool.len(), 1);

        // Registering the same name returns the existing type
        let (struct_id2, is_new2) = pool.register_struct(name, def);
        assert!(!is_new2);
        assert_eq!(struct_id, struct_id2);
        assert_eq!(pool.len(), 1); // No new type added
    }

    #[test]
    fn language_item_reverse_index_is_unique_and_deterministic() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let make_def = |name: &str, file_id| StructDef {
            name: name.into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: true,
            file_id,
        };
        let (canonical, _) = pool.register_struct(
            interner.get_or_intern("CanonicalStrBuf"),
            make_def("CanonicalStrBuf", FileId::DEFAULT),
        );
        pool.set_struct_lang_item(canonical, LangItem::StrBuf);
        pool.set_struct_lang_item(canonical, LangItem::StrBuf);
        assert_eq!(
            pool.lang_item_type(LangItem::StrBuf),
            Some(Type::new_struct(canonical))
        );
    }

    #[test]
    #[should_panic(expected = "a language item can only identify one canonical struct")]
    fn duplicate_language_item_assignment_is_rejected() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let make_def = |name: &str, file_id| StructDef {
            name: name.into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: true,
            file_id,
        };
        let (canonical, _) = pool.register_struct(
            interner.get_or_intern("CanonicalStrBuf"),
            make_def("CanonicalStrBuf", FileId::DEFAULT),
        );
        pool.set_struct_lang_item(canonical, LangItem::StrBuf);
        let other_file = FileId::new(1);
        let (duplicate, _) = pool.register_struct(
            interner.get_or_intern("OtherStrBuf"),
            make_def("OtherStrBuf", other_file),
        );
        pool.set_struct_lang_item(duplicate, LangItem::StrBuf);
    }

    #[test]
    fn test_pool_register_enum() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Color");

        let def = EnumDef {
            name: "Color".into(),
            variants: Arc::from(["Red".into(), "Green".into(), "Blue".into()]),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        let (enum_id, is_new) = pool.register_enum(name, def.clone());
        assert!(is_new);
        assert_eq!(enum_id.pool_index(), 0); // First entry in pool
        assert_eq!(pool.len(), 1);

        // Registering the same name returns the existing type
        let (enum_id2, is_new2) = pool.register_enum(name, def);
        assert!(!is_new2);
        assert_eq!(enum_id, enum_id2);
    }

    #[test]
    fn test_pool_intern_array() {
        let pool = TypeInternPool::new();

        // Intern [i32; 5]
        let arr1 = pool.try_intern_array(Type::I32, 5).unwrap();
        assert!(arr1.is_array());
        assert_eq!(pool.len(), 1);

        // Interning the same array returns the same type
        let arr2 = pool.try_intern_array(Type::I32, 5).unwrap();
        assert_eq!(arr1, arr2);
        assert_eq!(pool.len(), 1);

        // Different length is a different type
        let arr3 = pool.try_intern_array(Type::I32, 10).unwrap();
        assert_ne!(arr1, arr3);
        assert_eq!(pool.len(), 2);

        // Different element type is a different type
        let arr4 = pool.try_intern_array(Type::I64, 5).unwrap();
        assert_ne!(arr1, arr4);
        assert_eq!(pool.len(), 3);
    }

    #[test]
    fn test_pool_get_struct_by_file_name() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Point");

        assert!(
            pool.get_struct_by_file_name(rue_span::FileId::DEFAULT, name)
                .is_none()
        );

        let def = StructDef {
            name: "Point".into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        let (struct_id, _) = pool.register_struct(name, def);
        let expected = Type::new_struct(struct_id);
        assert_eq!(
            pool.get_struct_by_file_name(rue_span::FileId::DEFAULT, name),
            Some(expected)
        );
    }

    #[test]
    fn test_pool_get_enum_by_file_name() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Status");

        assert!(
            pool.get_enum_by_file_name(rue_span::FileId::DEFAULT, name)
                .is_none()
        );

        let def = EnumDef {
            name: "Status".into(),
            variants: Arc::from(["Active".into(), "Inactive".into()]),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        let (enum_id, _) = pool.register_enum(name, def);
        let expected = Type::new_enum(enum_id);
        assert_eq!(
            pool.get_enum_by_file_name(rue_span::FileId::DEFAULT, name),
            Some(expected)
        );
    }

    #[test]
    fn test_pool_get_array() {
        let pool = TypeInternPool::new();

        assert!(pool.get_array(Type::I32, 5).is_none());

        let arr = pool.try_intern_array(Type::I32, 5).unwrap();
        assert_eq!(pool.get_array(Type::I32, 5), Some(arr));
        assert!(pool.get_array(Type::I32, 10).is_none());
    }

    #[test]
    fn test_pool_get_type_data() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Primitive types return None
        assert!(pool.get(Type::I32).is_none());

        // Register a struct
        let struct_name = interner.get_or_intern("Point");
        let struct_def = StructDef {
            name: "Point".into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (struct_id, _) = pool.register_struct(struct_name, struct_def);
        let struct_ty = Type::new_struct(struct_id);

        // Get struct data
        let data = pool.get(struct_ty).expect("should get struct data");
        assert!(matches!(data, TypeData::Struct(_)));

        // Intern an array
        let arr_ty = pool.try_intern_array(Type::I32, 10).unwrap();
        let arr_data = pool.get(arr_ty).expect("should get array data");
        match arr_data {
            TypeData::Array { element, len, .. } => {
                assert_eq!(element, Type::I32);
                assert_eq!(len, 10);
            }
            _ => panic!("expected array data"),
        }
    }

    #[test]
    fn test_pool_type_checks() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let struct_name = interner.get_or_intern("Point");
        let struct_def = StructDef {
            name: "Point".into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (struct_id, _) = pool.register_struct(struct_name, struct_def);
        let struct_ty = Type::new_struct(struct_id);

        let enum_name = interner.get_or_intern("Color");
        let enum_def = EnumDef {
            name: "Color".into(),
            variants: Arc::from(["Red".into()]),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (enum_id, _) = pool.register_enum(enum_name, enum_def);
        let enum_ty = Type::new_enum(enum_id);

        let array_ty = pool.try_intern_array(Type::I32, 5).unwrap();

        // Check is_struct
        assert!(pool.is_struct(struct_ty));
        assert!(!pool.is_struct(enum_ty));
        assert!(!pool.is_struct(array_ty));
        assert!(!pool.is_struct(Type::I32));

        // Check is_enum
        assert!(!pool.is_enum(struct_ty));
        assert!(pool.is_enum(enum_ty));
        assert!(!pool.is_enum(array_ty));
        assert!(!pool.is_enum(Type::I32));

        // Check is_array
        assert!(!pool.is_array(struct_ty));
        assert!(!pool.is_array(enum_ty));
        assert!(pool.is_array(array_ty));
        assert!(!pool.is_array(Type::I32));
    }

    #[test]
    fn test_pool_get_struct_def() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let name = interner.get_or_intern("Point");
        let def = StructDef {
            name: "Point".into(),
            fields: vec![],
            is_copy: true,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (struct_id, _) = pool.register_struct(name, def.clone());

        // Direct nominal-ID lookup returns the canonical definition.
        let retrieved = pool.struct_def(struct_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.is_copy, def.is_copy);

        // The pool encoding resolves to the same definition.
        let interned = Type::new_struct(struct_id);
        let retrieved2 = pool
            .get_struct_def(interned)
            .expect("should get struct def");
        assert_eq!(retrieved2.name, def.name);

        // Non-struct returns None for get_struct_def
        let array_ty = pool.try_intern_array(Type::I32, 5).unwrap();
        assert!(pool.get_struct_def(array_ty).is_none());
        assert!(pool.get_struct_def(Type::I32).is_none());
    }

    #[test]
    fn test_pool_get_enum_def() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let name = interner.get_or_intern("Status");
        let def = EnumDef {
            name: "Status".into(),
            variants: Arc::from(["A".into(), "B".into()]),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (enum_id, _) = pool.register_enum(name, def.clone());

        // Direct nominal-ID lookup returns the canonical definition.
        let retrieved = pool.enum_def(enum_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.variants.len(), 2);

        // The pool encoding resolves to the same definition.
        let interned = Type::new_enum(enum_id);
        let retrieved2 = pool.get_enum_def(interned).expect("should get enum def");
        assert_eq!(retrieved2.name, def.name);

        // Non-enum returns None for get_enum_def
        let array_ty = pool.try_intern_array(Type::I32, 5).unwrap();
        assert!(pool.get_enum_def(array_ty).is_none());
        assert!(pool.get_enum_def(Type::I32).is_none());
    }

    #[test]
    fn test_pool_get_array_info() {
        let pool = TypeInternPool::new();

        let array_ty = pool.try_intern_array(Type::I64, 100).unwrap();
        let (element, len) = pool
            .get_array_info(array_ty)
            .expect("should get array info");
        assert_eq!(element, Type::I64);
        assert_eq!(len, 100);

        // Non-array returns None
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("X");
        let def = StructDef {
            name: "X".into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (struct_id, _) = pool.register_struct(name, def);
        let struct_ty = Type::new_struct(struct_id);
        assert!(pool.get_array_info(struct_ty).is_none());
        assert!(pool.get_array_info(Type::I32).is_none());
    }

    #[test]
    fn test_pool_stats() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let stats = pool.stats();
        assert_eq!(stats.struct_count, 0);
        assert_eq!(stats.enum_count, 0);
        assert_eq!(stats.array_count, 0);
        assert_eq!(stats.total, 0);

        // Add some types
        let s1 = interner.get_or_intern("S1");
        let s2 = interner.get_or_intern("S2");
        let e1 = interner.get_or_intern("E1");

        let def = StructDef {
            name: "S1".into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        pool.register_struct(s1, def.clone());
        pool.register_struct(
            s2,
            StructDef {
                name: "S2".into(),
                ..def
            },
        );

        pool.register_enum(
            e1,
            EnumDef {
                name: "E1".into(),
                variants: Arc::from([]),
                variant_payloads: Vec::new(),
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        );

        pool.try_intern_array(Type::I32, 5).unwrap();
        pool.try_intern_array(Type::I32, 10).unwrap();
        pool.try_intern_array(Type::BOOL, 3).unwrap();

        let stats = pool.stats();
        assert_eq!(stats.struct_count, 2);
        assert_eq!(stats.enum_count, 1);
        assert_eq!(stats.array_count, 3);
        assert_eq!(stats.total, 6);
    }

    #[test]
    fn test_pool_nested_arrays() {
        let pool = TypeInternPool::new();

        // Create [i32; 3]
        let inner = pool.try_intern_array(Type::I32, 3).unwrap();

        // Create [[i32; 3]; 4]
        let outer = pool.try_intern_array(inner, 4).unwrap();

        // Verify structure
        let (outer_elem, outer_len) = pool.get_array_info(outer).expect("outer array info");
        assert_eq!(outer_elem, inner);
        assert_eq!(outer_len, 4);

        let (inner_elem, inner_len) = pool.get_array_info(inner).expect("inner array info");
        assert_eq!(inner_elem, Type::I32);
        assert_eq!(inner_len, 3);
    }

    // ========================================================================
    // Thread safety tests
    // ========================================================================

    #[test]
    fn test_pool_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let pool = Arc::new(TypeInternPool::new());
        let interner = Arc::new(ThreadedRodeo::default());

        // Pre-register names for thread safety
        let names: Vec<Spur> = (0..100)
            .map(|i| interner.get_or_intern(format!("Type{}", i)))
            .collect();

        let handles: Vec<_> = (0..10)
            .map(|thread_id| {
                let pool = Arc::clone(&pool);
                let names = names.clone();
                thread::spawn(move || {
                    // Each thread registers 10 types
                    for i in 0..10 {
                        let idx = thread_id * 10 + i;
                        let name = names[idx];
                        let def = StructDef {
                            name: format!("Type{}", idx).into(),
                            fields: vec![],
                            is_copy: false,
                            is_linear: false,
                            destructor: None,
                            is_builtin: false,
                            is_pub: false,
                            file_id: rue_span::FileId::DEFAULT,
                        };
                        pool.register_struct(name, def);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("thread panicked");
        }

        // All 100 types should be registered
        assert_eq!(pool.len(), 100);

        // Each name should map to a valid type
        for name in &names {
            assert!(
                pool.get_struct_by_file_name(rue_span::FileId::DEFAULT, *name)
                    .is_some()
            );
        }
    }

    #[test]
    fn test_pool_concurrent_array_interning() {
        use std::sync::Arc;
        use std::thread;

        let pool = Arc::new(TypeInternPool::new());

        // Multiple threads try to intern the same array type
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || pool.try_intern_array(Type::I32, 42).unwrap())
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();

        // All threads should get the same type
        let first = results[0];
        for result in &results {
            assert_eq!(*result, first);
        }

        // Only one array type should be in the pool
        assert_eq!(pool.stats().array_count, 1);
    }

    // ========================================================================
    // Struct ID reservation tests
    // ========================================================================

    #[test]
    fn test_pool_reserve_and_complete_struct() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Reserve an ID
        let struct_id = pool.reserve_struct_id();
        assert_eq!(struct_id.pool_index(), 0);
        assert_eq!(pool.len(), 1); // Placeholder was pushed

        // Use the ID to create a name
        let name_str = format!("__anon_struct_{}", struct_id.0);
        let name = interner.get_or_intern(&name_str);

        let def = StructDef {
            name: name_str.as_str().into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        // Complete registration
        pool.complete_struct_registration(struct_id, name, def);

        // Verify registration succeeded
        assert_eq!(pool.len(), 1); // No new entry, just updated
        assert!(
            pool.get_struct_by_file_name(rue_span::FileId::DEFAULT, name)
                .is_some()
        );

        // Can retrieve the struct definition
        let retrieved = pool.struct_def(struct_id);
        assert_eq!(retrieved.name.as_ref(), name_str);
    }

    /// RUE-571: a struct name registered by two files yields file-qualified
    /// symbol names; a unique name stays bare; builtins are never qualified.
    #[test]
    fn test_struct_symbol_name_qualifies_all_named_types() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let mk = |name: &str, file: u32, is_builtin: bool| StructDef {
            name: name.into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin,
            is_pub: true,
            file_id: rue_span::FileId::new(file),
        };

        let p_sym = interner.get_or_intern("P");
        let (p1, _) = pool.register_struct(p_sym, mk("P", 1, false));
        let (p2, _) = pool.register_struct(p_sym, mk("P", 2, false));
        let q_sym = interner.get_or_intern("Q");
        let (q, _) = pool.register_struct(q_sym, mk("Q", 1, false));
        let b_sym = interner.get_or_intern("StrBufTest");
        let (b1, _) = pool.register_struct(b_sym, mk("StrBufTest", 0, true));
        let (b2, _) = pool.register_struct(b_sym, mk("StrBufTest", 3, false));

        // Every named user struct is unconditionally file-qualified (ADR-0066,
        // RUE-1089), whether or not a collision is observed.
        assert_eq!(pool.struct_symbol_name(p1), "P$1");
        assert_eq!(pool.struct_symbol_name(p2), "P$2");
        // A unique name is qualified too.
        assert_eq!(pool.struct_symbol_name(q), "Q$1");
        // A builtin is never qualified; the user struct of the same name still
        // is, so the pair stays distinct.
        assert_eq!(pool.struct_symbol_name(b1), "StrBufTest");
        assert_eq!(pool.struct_symbol_name(b2), "StrBufTest$3");
    }

    /// RUE-1193: the bare-symbol exemption is registry membership, not the
    /// generated-name spelling. A source declaration may legally spell the exact
    /// generated form -- including the full 32-hex-digit one -- and must still be
    /// file-qualified, or it collides with the generated type it names.
    #[test]
    fn anonymity_exemption_is_membership_not_the_generated_name_spelling() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let struct_def = |name: &str, file: u32| StructDef {
            name: name.into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: true,
            file_id: rue_span::FileId::new(file),
        };
        let digest_name = "__anon_struct_daa98b889bc477390889f83e53d5c4c3";

        // Source declarations wearing the generated spelling.
        let short_sym = interner.get_or_intern("__anon_struct_5");
        let (short, _) = pool.register_struct(short_sym, struct_def("__anon_struct_5", 1));
        let digest_sym = interner.get_or_intern(digest_name);
        let (lookalike, _) = pool.register_struct(digest_sym, struct_def(digest_name, 1));

        // The type the compiler actually generated, marked at creation.
        let generated_sym = interner.get_or_intern("__anon_struct_deadbeef");
        let (generated, _) =
            pool.register_struct(generated_sym, struct_def("__anon_struct_deadbeef", 0));
        pool.mark_anonymous_struct(generated);

        assert_eq!(pool.struct_symbol_name(short), "__anon_struct_5$1");
        assert_eq!(
            pool.struct_symbol_name(lookalike),
            format!("{digest_name}$1")
        );
        assert_eq!(pool.struct_symbol_name(generated), "__anon_struct_deadbeef");

        // The enum half of the same rule.
        let enum_def = |name: &str, file: u32| EnumDef {
            name: name.into(),
            variants: Arc::from(["A".into()]),
            variant_payloads: vec![vec![]],
            is_pub: true,
            file_id: rue_span::FileId::new(file),
        };
        let source_enum_sym = interner.get_or_intern("__anon_enum_5");
        let (source_enum, _) = pool.register_enum(source_enum_sym, enum_def("__anon_enum_5", 1));
        let generated_enum_sym = interner.get_or_intern("__anon_enum_deadbeef { A }");
        let (generated_enum, _) = pool.register_enum(
            generated_enum_sym,
            enum_def("__anon_enum_deadbeef { A }", 0),
        );
        pool.mark_anonymous_enum(generated_enum);

        assert_eq!(pool.enum_symbol_name(source_enum), "__anon_enum_5$1");
        assert_eq!(
            pool.enum_symbol_name(generated_enum),
            "__anon_enum_deadbeef { A }"
        );
    }

    /// RUE-1193: symbol spelling reads the anonymity registry, so the registry
    /// has to survive every representation the pool takes on downstream --
    /// freezing for post-semantic phases and the per-body overlay derivation.
    /// A mark that does not travel spells a generated anonymous type as if it
    /// were a user nominal.
    #[test]
    fn anonymity_marks_survive_freezing_and_overlay_derivation() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = "__anon_struct_deadbeef";
        let symbol = interner.get_or_intern(name);
        let (generated, _) = pool.register_struct(
            symbol,
            StructDef {
                name: name.into(),
                fields: vec![],
                is_copy: true,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::new(0),
            },
        );
        pool.mark_anonymous_struct(generated);

        assert!(pool.is_anonymous_struct(generated));
        assert_eq!(pool.struct_symbol_name(generated), name);

        let cloned = pool.clone();
        assert!(cloned.is_anonymous_struct(generated));
        assert_eq!(cloned.struct_symbol_name(generated), name);

        // The per-body overlay reads the mark through its shared base.
        cloned.rebase_overlay_in_place();
        assert!(cloned.is_anonymous_struct(generated));
        assert_eq!(cloned.struct_symbol_name(generated), name);

        let frozen = pool.freeze();
        assert!(frozen.is_anonymous_struct(generated));
        assert_eq!(frozen.struct_symbol_name(generated), name);
    }

    #[test]
    fn type_symbol_names_use_stable_paths_and_survive_pool_clone() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let left_id = FileId::new(42);
        let right_id = FileId::new(7);
        pool.set_symbol_paths(HashMap::from([
            (left_id, "left/shared.rue".to_string()),
            (right_id, "right/shared.rue".to_string()),
        ]));

        let payload = interner.get_or_intern("Payload");
        let struct_def = |file_id| StructDef {
            name: "Payload".into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: true,
            file_id,
        };
        let (left_struct, _) = pool.register_struct(payload, struct_def(left_id));
        let (right_struct, _) = pool.register_struct(payload, struct_def(right_id));

        let choice = interner.get_or_intern("Choice");
        let enum_def = |file_id| EnumDef {
            name: "Choice".into(),
            variants: Arc::from(["Value".into()]),
            variant_payloads: vec![vec![]],
            is_pub: true,
            file_id,
        };
        let (left_enum, _) = pool.register_enum(choice, enum_def(left_id));
        let (right_enum, _) = pool.register_enum(choice, enum_def(right_id));

        let cloned = pool.clone();
        assert_eq!(
            cloned.struct_symbol_name(left_struct),
            "Payload$left_2fshared_2erue"
        );
        assert_eq!(
            cloned.struct_symbol_name(right_struct),
            "Payload$right_2fshared_2erue"
        );
        assert_eq!(
            cloned.enum_symbol_name(left_enum),
            "Choice$left_2fshared_2erue"
        );
        assert_eq!(
            cloned.enum_symbol_name(right_enum),
            "Choice$right_2fshared_2erue"
        );
    }

    #[test]
    fn test_pool_reserve_multiple_structs() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Reserve multiple IDs
        let id1 = pool.reserve_struct_id();
        let id2 = pool.reserve_struct_id();
        let id3 = pool.reserve_struct_id();

        assert_eq!(id1.pool_index(), 0);
        assert_eq!(id2.pool_index(), 1);
        assert_eq!(id3.pool_index(), 2);
        assert_eq!(pool.len(), 3);

        // Complete them in any order (here: reverse)
        for (i, id) in [(2, id3), (1, id2), (0, id1)] {
            let name_str = format!("__anon_struct_{}", i);
            let name = interner.get_or_intern(&name_str);
            let def = StructDef {
                name: name_str.into(),
                fields: vec![],
                is_copy: false,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            };
            pool.complete_struct_registration(id, name, def);
        }

        // All three should be registered
        assert_eq!(pool.stats().struct_count, 3);
    }

    #[test]
    fn public_get_hides_reserved_entries() {
        let pool = TypeInternPool::new();
        let reserved = pool.reserve_struct_id();
        let synthesized = Type::new_struct(reserved);

        assert!(pool.get(synthesized).is_none());
        assert!(!pool.is_struct(synthesized));
        assert_eq!(
            pool.validate_structural_child(synthesized),
            Err(TypeValidationError::ReservedEntry)
        );
    }

    // Compile-time assertion that TypeInternPool is Send + Sync
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_pool_is_send_sync() {
        assert_send_sync::<TypeInternPool>();
        assert_send_sync::<FrozenTypeInternPool>();
    }

    #[test]
    fn test_ptr_type_error_name_shows_pointee() {
        // Diagnostics must render the pointee type, not a bare `<ptr const>`
        // placeholder that makes "expected X, found X" messages useless
        // (RUE-8). Verify `safe_name_with_pool` resolves the pointee through
        // the pool for both const and mut pointers, including nested pointers.
        let pool = TypeInternPool::new();

        let pc = pool.intern_ptr_const_from_type(Type::I32);
        assert_eq!(
            Type::new_ptr_const(pc).safe_name_with_pool(Some(&pool)),
            "ptr const i32"
        );

        let pm = pool.intern_ptr_mut_from_type(Type::U64);
        assert_eq!(
            Type::new_ptr_mut(pm).safe_name_with_pool(Some(&pool)),
            "ptr mut u64"
        );

        // Nested: ptr const (ptr mut i32)
        let inner = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32));
        let outer = pool.intern_ptr_const_from_type(inner);
        assert_eq!(
            Type::new_ptr_const(outer).safe_name_with_pool(Some(&pool)),
            "ptr const ptr mut i32"
        );

        // Without a pool, fall back to a stable id-tagged placeholder.
        assert_eq!(
            Type::new_ptr_const(pc).safe_name_with_pool(None),
            format!("<ptr const#{}>", pc.0)
        );
    }

    #[test]
    fn frozen_pointer_lookup_rejects_direct_and_nested_recovery_types() {
        let pool = TypeInternPool::new();
        let direct = pool.intern_ptr_mut_from_type(Type::ERROR);
        let error_array = Type::new_array(pool.intern_array_from_type(Type::ERROR, 2));
        let nested = pool.intern_ptr_mut_from_type(error_array);
        let valid = pool.intern_ptr_mut_from_type(Type::U8);
        let frozen = pool.freeze();

        assert_eq!(frozen.get_ptr_mut_by_type(Type::ERROR), None);
        assert_eq!(frozen.get_ptr_mut_by_type(error_array), None);
        assert_eq!(frozen.get_ptr_mut_by_type(Type::U8), Some(valid));
        assert_eq!(frozen.ptr_mut_def(direct), Type::ERROR);
        assert_eq!(frozen.ptr_mut_def(nested), error_array);
    }

    // ========================================================================
    // Canonical layout authority (ADR-0052)
    // ========================================================================

    use crate::layout::LayoutKind;

    #[test]
    fn layout_empty_struct_is_zero_sized() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let (id, _) =
            pool.register_struct(interner.get_or_intern("Empty"), struct_def("Empty", vec![]));
        let frozen = pool.freeze();
        let layout = frozen.layout(Type::new_struct(id));
        assert_eq!(layout.size, 0);
        assert_eq!(layout.alignment, 1);
    }

    #[test]
    fn compact_layout_reports_natural_scalar_widths_and_alignments() {
        let pool = TypeInternPool::new();
        let ptr = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::I32));
        let frozen = pool.freeze();
        for (ty, size, align) in [
            (Type::I8, 1, 1),
            (Type::U8, 1, 1),
            (Type::BOOL, 1, 1),
            (Type::I16, 2, 2),
            (Type::U16, 2, 2),
            (Type::I32, 4, 4),
            (Type::U32, 4, 4),
            (Type::I64, 8, 8),
            (Type::U64, 8, 8),
            (ptr, 8, 8),
        ] {
            let layout = frozen.layout(ty);
            assert_eq!(layout.size, size, "{ty:?} size");
            assert_eq!(layout.alignment, align, "{ty:?} align");
            assert_eq!(layout.stride, size, "{ty:?} stride == size");
            assert_eq!(layout.kind, LayoutKind::Scalar, "{ty:?} kind");
        }
    }

    #[test]
    fn compact_layout_zero_sized_types_are_size_zero_align_one_stride_zero() {
        let pool = TypeInternPool::new();
        let empty_array = Type::new_array(pool.intern_array_from_type(Type::I32, 0));
        let frozen = pool.freeze();
        for ty in [Type::UNIT, Type::NEVER, empty_array] {
            let layout = frozen.layout(ty);
            assert_eq!(layout.size, 0, "{ty:?} size");
            assert_eq!(layout.alignment, 1, "{ty:?} align");
            assert_eq!(layout.stride, 0, "{ty:?} stride");
        }
    }

    #[test]
    fn compact_layout_struct_packs_fields_with_interior_and_tail_padding() {
        // Padded { a: u8, b: i32, c: u8 }: a@0, pad[1,4), b@4, c@8, tail pad
        // [9,12). size 12, align 4.
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let (id, _) = pool.register_struct(
            interner.get_or_intern("Padded"),
            struct_def(
                "Padded",
                vec![
                    StructField {
                        name: "a".into(),
                        ty: Type::U8,
                    },
                    StructField {
                        name: "b".into(),
                        ty: Type::I32,
                    },
                    StructField {
                        name: "c".into(),
                        ty: Type::U8,
                    },
                ],
            ),
        );
        let frozen = pool.freeze();
        let layout = frozen.layout(Type::new_struct(id));
        assert_eq!(layout.size, 12);
        assert_eq!(layout.alignment, 4);
        assert_eq!(layout.stride, 12);
        match &layout.kind {
            LayoutKind::Struct {
                field_offsets,
                padding_ranges,
            } => {
                assert_eq!(field_offsets, &[0, 4, 8]);
                assert_eq!(
                    padding_ranges,
                    &[
                        PaddingRange { start: 1, end: 4 },
                        PaddingRange { start: 9, end: 12 },
                    ]
                );
            }
            other => panic!("expected struct layout, got {other:?}"),
        }
        // @offset_of-facing physical offsets are compact...
        assert_eq!(frozen.struct_field_offset(id, 1), 4);
        // ...while the codegen slot offsets stay slot-based (representation 2).
        assert_eq!(frozen.struct_field_slot_offset(id, 0), 0);
        assert_eq!(frozen.struct_field_slot_offset(id, 1), 1);
        assert_eq!(frozen.struct_field_slot_offset(id, 2), 2);
    }

    #[test]
    fn compact_image_padding_ranges_cover_struct_interior_and_tail_gaps() {
        // Padded { a: u8, b: i32, c: u8 }: a@0, pad[1,4), b@4, c@8, tail[9,12).
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let (id, _) = pool.register_struct(
            interner.get_or_intern("Padded"),
            struct_def(
                "Padded",
                vec![
                    StructField {
                        name: "a".into(),
                        ty: Type::U8,
                    },
                    StructField {
                        name: "b".into(),
                        ty: Type::I32,
                    },
                    StructField {
                        name: "c".into(),
                        ty: Type::U8,
                    },
                ],
            ),
        );
        let frozen = pool.freeze();
        assert_eq!(
            frozen.compact_image_padding_ranges(Type::new_struct(id)),
            vec![
                PaddingRange { start: 1, end: 4 },
                PaddingRange { start: 9, end: 12 },
            ]
        );
    }

    #[test]
    fn compact_image_padding_ranges_cover_enum_tag_gap_and_tail() {
        // Wide { A(u8, i32), B }: u8 tag@0, payload@4 (i32 alignment). Payload
        // packs u8@0,i32@4 => u8 abs@4, i32 abs@8; size = align_up(4+8,4) = 12.
        // Padding: tag-to-payload [1,4) and the gap [5,8) after the u8 field.
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let def = EnumDef {
            name: "Wide".into(),
            variants: Arc::from(["A".into(), "B".into()]),
            variant_payloads: vec![vec![Type::U8, Type::I32], vec![]],
            is_pub: false,
            file_id: FileId::DEFAULT,
        };
        let (id, _) = pool.register_enum(interner.get_or_intern("Wide"), def);
        let frozen = pool.freeze();
        assert_eq!(
            frozen.compact_image_padding_ranges(Type::new_enum(id)),
            vec![
                PaddingRange { start: 1, end: 4 },
                PaddingRange { start: 5, end: 8 },
            ]
        );
    }

    #[test]
    fn compact_image_padding_ranges_empty_for_packed_all_i64_struct() {
        // An all-eight-byte-leaf struct is slot-identical: no padding to zero.
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let (id, _) = pool.register_struct(
            interner.get_or_intern("Packed"),
            struct_def(
                "Packed",
                vec![
                    StructField {
                        name: "a".into(),
                        ty: Type::I64,
                    },
                    StructField {
                        name: "b".into(),
                        ty: Type::I64,
                    },
                ],
            ),
        );
        let frozen = pool.freeze();
        assert!(
            frozen
                .compact_image_padding_ranges(Type::new_struct(id))
                .is_empty()
        );
    }

    #[test]
    fn compact_layout_array_strides_by_compact_element_size() {
        let pool = TypeInternPool::new();
        let array_ty = pool.try_intern_array(Type::I32, 3).unwrap();
        let frozen = pool.freeze();
        let layout = frozen.layout(array_ty);
        assert_eq!(layout.size, 12);
        assert_eq!(layout.stride, 12);
        match layout.kind {
            LayoutKind::Array { element, count } => {
                assert_eq!(count, 3);
                assert_eq!(element.size, 4);
                assert_eq!(element.stride, 4, "compact indexing strides by 4");
            }
            other => panic!("expected array layout, got {other:?}"),
        }
    }

    #[test]
    fn compact_layout_enum_uses_smallest_tag_and_max_variant_alignment() {
        // Shape { Pair(i32, i64), One(i32) }: u8 tag@0, payload aligned to 8
        // (the i64), so payload_offset 8. Largest payload packs i32@0,i64@8 =>
        // 16 bytes; size = align_up(8 + 16, 8) = 24.
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let def = EnumDef {
            name: "Shape".into(),
            variants: Arc::from(["Pair".into(), "One".into()]),
            variant_payloads: vec![vec![Type::I32, Type::I64], vec![Type::I32]],
            is_pub: false,
            file_id: FileId::DEFAULT,
        };
        let (id, _) = pool.register_enum(interner.get_or_intern("Shape"), def);
        let frozen = pool.freeze();
        let layout = frozen.layout(Type::new_enum(id));
        assert_eq!(layout.alignment, 8);
        assert_eq!(layout.size, 24);
        match layout.kind {
            LayoutKind::Enum {
                tag,
                payload_offset,
                variants,
            } => {
                assert_eq!(tag.size, 1, "smallest sufficient tag is u8");
                assert_eq!(tag.alignment, 1);
                assert_eq!(payload_offset, 8, "payload at max variant alignment");
                assert_eq!(variants, vec![vec![8, 16], vec![8]]);
            }
            other => panic!("expected enum layout, got {other:?}"),
        }
    }
}
