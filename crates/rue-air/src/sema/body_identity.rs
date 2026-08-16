//! Body-scoped nominal / type-identity pool for the provider-driven analyzer
//! (RUE-1091 slice r4a-2a).
//!
//! [`BodyIdentityPool`] is the value/type-identity analog of the r2 durable
//! metadata store, promoted to an id-minting pool. Where the epoch pre-registers
//! every source nominal into a [`TypeInternPool`] during declaration gathering
//! and the analyzer merely *looks them up*
//! (`body_endpoint::resolve_instance_type`), the provider-driven analyzer starts
//! from a bare body-local pool and must *mint* each consulted nominal from its
//! durable metadata on first consult, deduplicating on repeat.
//!
//! This slice builds that pool machinery — the same registration path the epoch
//! performs (`declare_struct`/`complete_declared_struct`,
//! `register_enum`/`declare_enum`, `try_intern_array`/`try_intern_ptr_*`,
//! `set_symbol_paths`, `set_struct_lang_item`) — so every downstream read
//! (`struct_def`/`enum_def` metadata, `format_type_name`, `is_type_copy`,
//! `struct_symbol_name`) is byte-equivalent to an epoch-registered twin. The
//! pool mints **internally-consistent** ids carrying correct durable metadata,
//! not epoch-equal numbering: published artifacts are durable-keyed at export
//! (`semantic_body_export.rs`), so the transient pool indices need not match the
//! epoch's (RUE-1091 pool-keystone).
//!
//! Scope of r4a-2a is the nominal / type-identity family: the arms
//! `resolve_instance_type` needs for its primitive, builtin-nominal, named
//! nominal, anonymous, and structural (array / ptr / slice) shapes.
//!
//! Slice r4a-2b extends this with the **callable-identity family**: the pool
//! assembles the signature-derived subset of `FunctionInfo`/`MethodInfo` and
//! the `ParamRange`s they carry from the durable signature vocabulary
//! (`DurableFunction`/`DurableMethod` over `DurableSignatureParameter`), minting
//! its parameters into a pool-owned [`ParamArena`] (which lives *beside* the
//! type pool, not inside it) and
//! resolving every parameter/return/receiver type through the same 2a `resolve`
//! machinery. The request/RIR-carried remainder of each info struct is *not*
//! minted: it is supplied by a caller-provided handle
//! ([`FunctionIdentityHandle`]/[`MethodIdentityHandle`]) — the honest 2b/2c
//! boundary. Every info field is therefore either purely durable-signature-
//! derived (minted here) or purely request/RIR-carried (handle), never both:
//!
//! - **durable (2b):** parameters (name/type/mode/comptime), return type,
//!   `is_generic` (derived: any comptime param), `is_pub`, `is_unchecked`,
//!   `has_self`, `self_mode`, and a method's `struct_type` (the 2a-resolved
//!   receiver);
//! - **request/RIR (handle):** `body`/`declaration` (RIR `InstRef`s), `span`,
//!   `return_type_syntax` (owner-local structured RIR syntax), `is_extern`,
//!   `is_c_export`
//!   (an epoch RIR read, not a durable-shell fact), the three `@allow` flags
//!   (RIR directives), `file_id`, and a method's `self_is_mut`.
//!
//! Deliberately still out of the pool:
//!
//! - the RIR-index answers the endpoint seam consumes (2c): the pool takes the
//!   `body`/`declaration` handles as caller-provided inputs and does not itself
//!   reverse `first_free_function`/`destructors`/`named_method_declarations`;
//! - callables whose parameter/return/receiver types are themselves a *deferred*
//!   2a arm (generic-parameter substitution, module identity) — these are
//!   refused (poisoning the callable key), never approximated, exactly as the
//!   underlying `resolve` refuses them. A comptime-*value* parameter (concrete
//!   type, `is_comptime = true`) resolves fully and marks `is_generic`; only a
//!   comptime-*type* parameter referencing a generic parameter refuses;
//! - **anonymous method bodies** —
//!   [`BodyIdentityPool::find_or_create_anon`] (r6b) mints the producer-nominal
//!   anonymous struct / enum from its durable identity + shape, and
//!   [`ProviderIdentityContext::register_anonymous_method`] supplies the
//!   request-local dispatch entry shared by endpoint and call fact state. The
//!   method body itself remains a local RIR concern; an issued anonymous key
//!   with no registered durable identity still resolves by lookup only
//!   ([`BodyIdentityPool::register_issued_anonymous`]);
//! - **module identity** and generic-parameter substitution (endpoint /
//!   inference families) — these arms are *refused*, never approximated;
//! - **drop metadata** — durable named and anonymous nominals carry destructor
//!   presence into the pool, which derives the canonical destructor symbol.
//!   Transitive linearity / needs-drop stays unavailable until the caller runs
//!   `finalize_containment_metadata` at the same point production freezes.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::hash::Hash;
use std::rc::Rc;
use std::sync::Arc;

use ahash::AHashMap;
use lasso::{Spur, ThreadedRodeo};
use rue_rir::{InstData, InstRef, Rir, RirParamMode, ValidatedRir};
use rue_span::{FileId, Span};

use super::ConstValue;
use super::declaration_index::{RirDeclarationIndex, RirDestructorDeclaration};
use super::info::{ConstInfo, FunctionInfo, MethodInfo};
use super::provider_module_registry::ProviderModuleRegistry;
use crate::types::{EnumDef, EnumId, LangItem, StructDef, StructField, StructId, Type};
use crate::{
    AnonymousNominalKey, FunctionInstanceKey, ParamArena, ParamRange, SemanticImportConstValue,
    SemanticImportNominalKind, SemanticImportType, SemanticParameterMode, StableProducerId,
    TypeInternPool,
};

/// The durable body of a named nominal: its field / variant vocabulary plus the
/// declaration-time metadata the pool registration consumes.
///
/// Drop metadata (the destructor symbol, transitive linearity) is intentionally
/// absent — see the module docs. The declaration-time `is_linear` flag is
/// carried because the epoch's declaration shell carries it verbatim (it is a
/// durable field of `DurableDeclarationPayload::Struct`); the pool does not
/// finalize the transitive linearity join.
#[derive(Debug, Clone)]
pub enum DurableNominalBody<K, M> {
    Struct {
        /// Canonically shared fields in declaration order: source name and
        /// durable field type.
        fields: Arc<[(Arc<str>, SemanticImportType<K, M>)]>,
        is_copy: bool,
        is_linear: bool,
    },
    Enum {
        /// Canonically shared variants in declaration order: source name and
        /// durable payload types.
        variants: Arc<[(Arc<str>, Arc<[SemanticImportType<K, M>]>)]>,
    },
}

/// Durable metadata for one named nominal, sufficient to register a
/// byte-equivalent pool entry: everything the epoch's `StructDef`/`EnumDef`
/// registration needs for the 2a consumers.
#[derive(Debug, Clone)]
pub struct DurableNominal<K, M> {
    pub name: Arc<str>,
    /// The nominal's defining module logical path. Assigned a body-local
    /// [`FileId`] and published to the pool's symbol paths so nominal symbol
    /// qualification mangles the same module component the epoch does.
    pub module_path: Arc<str>,
    pub is_public: bool,
    pub is_builtin: bool,
    pub lang_item: Option<LangItem>,
    /// `@repr(c)` — a declaration-time side fact the epoch's shell phase sets
    /// (`set_struct_repr_c`). Carried so the pool registers it rather than
    /// silently dropping a declaration fact; struct-only (ignored for enums).
    pub is_repr_c: bool,
    /// Whether the nominal declares the reserved `__drop` member. The pool
    /// derives the final, file-qualified destructor symbol from the minted
    /// nominal identity.
    pub has_destructor: bool,
    pub body: DurableNominalBody<K, M>,
}

/// The durable nominal vocabulary the pool consults to mint a named nominal.
/// Rue-compiler implements this boundary from stable-keyed semantic metadata.
/// The trait carries no resolution logic; each consult is an exact point query.
pub trait DurableNominalSource<K, M> {
    /// The durable metadata for a nominal key, or `None` if the key names no
    /// nominal in the durable universe.
    fn nominal(&self, key: &K) -> Option<DurableNominal<K, M>>;

    /// Request-local file identity for the named declaration, when the caller
    /// is analyzing against a concrete source snapshot.
    fn nominal_file_id(&self, _key: &K) -> Option<FileId> {
        None
    }
}

/// The durable body of an anonymous nominal: its field / variant vocabulary. The
/// pool analog of the epoch's `SemanticAnonymousNominalShape` — the shape half of
/// producer-nominal identity (ADR-0066), separate from the identity half (the
/// `AnonymousNominalKey`) so a recursive or shape-equal reference joins on the
/// key alone.
///
/// Method BODIES are deliberately absent: registering an anonymous struct's
/// methods needs the request-local whole-program `Rir`
/// (`register_projected_anon_struct_methods`, `binding_manifest.rs`), which the
/// body-scoped pool does not hold. `struct_method_names` carries only the source
/// method-name vocabulary the epoch's [`find_or_create_anon_struct`] reads to
/// decide copyability and the destructor symbol — the reserved `__drop` name
/// forces the struct non-Copy and names its destructor. Method DISPATCH
/// registration is left to the flip's overlay method installation.
#[derive(Debug, Clone)]
pub enum DurableAnonymousShape<K, M> {
    Struct {
        /// Fields in declaration order: source name and durable field type.
        fields: Vec<(Arc<str>, SemanticImportType<K, M>)>,
        /// Source method names in declaration order (bodies excluded). Only the
        /// presence of the reserved `__drop` destructor name is consumed, to
        /// mirror the epoch's copyability / destructor metadata.
        struct_method_names: Vec<Arc<str>>,
    },
    Enum {
        /// Variants in declaration order: source name and durable payload types.
        variants: Vec<(Arc<str>, Vec<SemanticImportType<K, M>>)>,
    },
}

/// Provider-owned signature metadata for a member of an anonymous struct.
///
/// The executable body remains owned by the producer's exact body projection;
/// this value is only the callable overlay needed while analyzing a consumer.
#[derive(Debug, Clone)]
pub struct DurableAnonymousMethod<K, M> {
    pub name: Arc<str>,
    pub has_self: bool,
    pub self_mode: RirParamMode,
    pub parameters: Vec<(DurableAnonymousMethodType<K, M>, RirParamMode, bool)>,
    pub result: DurableAnonymousMethodType<K, M>,
}

#[derive(Debug, Clone)]
pub enum DurableAnonymousMethodType<K, M> {
    SelfType,
    Concrete(SemanticImportType<K, M>),
}

/// The durable anonymous vocabulary the pool consults to mint an anonymous
/// producer-nominal on first sight. Implemented by the r4b/flip provider side and
/// by the r6b unit tests.
///
/// The two `*_symbol_component` methods are the durable→stable-content
/// **relocation** the shared digest computation
/// ([`crate::stable_digest::stable_anonymous_identity_digest`]) hashes. They must
/// reproduce, BYTE-FOR-BYTE, the string the semantic epoch's
/// `stable_definition_symbol_component` / `stable_module_symbol_component`
/// (`anon_structs.rs`) emit for the SAME producer, so the pool and the epoch spell
/// the same `__anon_*_{digest:032x}` name. For a producer rooting at an installed
/// definition / module endpoint (every producer this pool mints) the epoch's
/// content is `D\u{1}{module_path}\u{1}{name}\u{1}{owner}\u{1}{kind as u8}` and
/// `M\u{1}{module_path}`; the durable key carries exactly those parts (module
/// logical path, name, owner name, kind), so the adapter formats them verbatim.
/// The epoch's session-local `d`/`m` FNV fallback (used only where a producer was
/// routed through a const-candidate token with no installed endpoint) embeds a
/// session-local issuer and is NOT reproducible from durable state alone; such
/// producers are out of the pool's minting scope.
///
/// # Canonical producer form
///
/// The durable anonymous universe is keyed by the CANONICAL producer form: an
/// empty-argument function specialization collapsed to its base
/// ([`AnonymousNominalKey::with_canonical_producer`]) — the form the epoch's
/// `canonical_function_producer` mints under and production body-export
/// carries. [`BodyIdentityPool::find_or_create_anon`] collapses its incoming
/// key on entry and consults [`Self::anonymous_shape`] with the canonical
/// form, so an implementation must index its shapes by that form (relocating
/// a non-canonical projection key through the same collapse before keying).
pub trait DurableAnonymousSource<K, M> {
    /// The durable shape for an anonymous nominal key, or `None` if the key names
    /// no anonymous nominal in the durable universe. Consulted with the
    /// canonical-producer form of the key (see the trait docs).
    fn anonymous_shape(
        &self,
        _key: &AnonymousNominalKey<K, M>,
    ) -> Option<DurableAnonymousShape<K, M>>;

    /// Return the durable shape and its canonical presentation digest from one
    /// source operation. The default composes the two legacy hooks for small
    /// test sources; compiler-owned sources should override this to perform a
    /// single durable-record lookup.
    fn anonymous_shape_and_digest(
        &self,
        key: &AnonymousNominalKey<K, M>,
    ) -> Option<(DurableAnonymousShape<K, M>, u128)> {
        let shape = self.anonymous_shape(key)?;
        Some((shape, self.anonymous_identity_digest(key)))
    }

    /// Callable signatures declared by an anonymous struct. Implementations
    /// that only provide shape identity may leave this empty.
    fn anonymous_methods(
        &self,
        _key: &AnonymousNominalKey<K, M>,
    ) -> Vec<DurableAnonymousMethod<K, M>> {
        Vec::new()
    }

    /// Type-valued lexical captures carried by an anonymous producer. Member
    /// bodies use these exact facts to restore the producer's generic
    /// environment without consulting an analyzer-owned epoch.
    fn anonymous_type_captures(
        &self,
        _key: &AnonymousNominalKey<K, M>,
    ) -> Vec<(Arc<str>, SemanticImportType<K, M>)> {
        Vec::new()
    }

    /// Non-type comptime lexical captures carried by an anonymous producer.
    /// Implementations that only provide shape identity may leave this empty.
    fn anonymous_value_captures(
        &self,
        _key: &AnonymousNominalKey<K, M>,
    ) -> Vec<(Arc<str>, SemanticImportConstValue<K, M>)> {
        Vec::new()
    }

    /// Return the canonical presentation digest for an anonymous identity.
    ///
    /// The default is deliberately the same relocation and hash used by the
    /// semantic epoch, so small test sources need no extra bookkeeping. A
    /// compiler-owned source may override this with a digest retained by its
    /// durable anonymous fact; the structured key remains the semantic
    /// authority and this value is presentation-only.
    fn anonymous_identity_digest(&self, key: &AnonymousNominalKey<K, M>) -> u128 {
        let relocated: AnonymousNominalKey<String, String> = key
            .try_map_identities::<String, String, std::convert::Infallible>(
                &|definition| Ok(self.definition_symbol_component(definition)),
                &|module| Ok(self.module_symbol_component(module)),
            )
            .expect("anonymous identity relocation to stable content is infallible");
        crate::stable_digest::stable_anonymous_identity_digest(&relocated)
    }

    /// Relocate a durable definition key to the exact stable-symbol content the
    /// epoch's `stable_definition_symbol_component` emits for its installed
    /// endpoint (see the trait docs for the byte-exact format).
    fn definition_symbol_component(&self, key: &K) -> String;

    /// Relocate a durable module key to the exact stable-symbol content the
    /// epoch's `stable_module_symbol_component` emits for its installed endpoint.
    fn module_symbol_component(&self, module: &M) -> String;
}

/// Why the pool could not mint an identity for a durable type. Every arm is a
/// closed refusal — the pool never approximates an identity it cannot mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::sema) enum IdentityMintError {
    /// A named nominal key resolves to no durable metadata.
    MissingNominal,
    /// A callable (function / method) key resolves to no durable signature.
    MissingCallable,
    /// A constant key resolves to no declaration-level durable value record.
    MissingConst,
    /// A function-valued constant names no durable callable source name.
    MissingConstCallable,
    /// An anonymous nominal key was consulted before its issued id was
    /// registered with the pool.
    MissingAnonymous,
    /// A builtin nominal name is not one of the pre-registered builtins.
    UnknownBuiltinNominal,
    /// A builtin nominal name exists but under the other nominal kind.
    BuiltinNominalKindMismatch,
    /// A structural wrap (array / pointer) failed pool validation.
    InvalidStructuralType,
    /// An anonymous nominal key was consulted for minting but no durable shape
    /// (fields / variants) was supplied for it.
    MissingAnonymousShape,
    /// Two distinct anonymous producer keys hashed to one presentation digest
    /// (RUE-1089, Theme 4b, fail-closed): the
    /// 128-bit digest spells presentation names only and must never collapse two
    /// producer-distinct types onto one id, so the second colliding key is
    /// refused and neither its nominal nor its symbol is published. Carries the
    /// digest both keys hash to.
    AnonymousDigestCollision(u128),
    /// A well-known `Option` registry install named an anonymous nominal whose
    /// durable shape is not an enum. The pool analog of the epoch install's
    /// `DeclarationInstallFailure::NominalShapeMismatch`
    /// (`install_well_known_option_types`, `binding_manifest.rs`): the trusted
    /// registry holds `Option` ENUM specializations only, so a non-enum shape
    /// is refused before any id or symbol is minted.
    WellKnownShapeMismatch,
    /// An arm outside slice r4a-2a's scope (module identity, generic parameter).
    Deferred(&'static str),
}

#[derive(Clone, Copy)]
enum PoolNominal {
    Struct(StructId),
    Enum(EnumId),
}

/// A body-scoped id-minting nominal / type pool.
///
/// Owns a fresh [`TypeInternPool`] and interner. Builtin enums and the core
/// `str` identity are pre-registered exactly as a fresh import epoch registers
/// them (`SemanticImportedProgram::new`), so the builtin-nominal arm resolves.
/// Named nominals are minted on first [`resolve`](Self::resolve) and
/// deduplicated by durable key thereafter.
///
/// The pool registries use independently keyed [`AHashMap`] instances. Their
/// authority is exact `Hash` + `Eq` lookup; explicit control flow preserves
/// append/mint order, and hash-table iteration is never an exported semantic
/// order.
pub(in crate::sema) struct BodyIdentityPool<K, M, S> {
    type_pool: Rc<TypeInternPool>,
    interner: Rc<ThreadedRodeo>,
    source: S,
    struct_ids: AHashMap<K, StructId>,
    enum_ids: AHashMap<K, EnumId>,
    /// Reverse joins used while exporting provider-local types back to their
    /// durable named identities. Kept beside the forward registries so export
    /// never scans every nominal minted for the body.
    struct_identities: AHashMap<StructId, K>,
    enum_identities: AHashMap<EnumId, K>,
    /// Keys whose mint failed after shell registration; repeat consults
    /// re-error rather than exposing the incomplete shell (see `mint_named`).
    poisoned: AHashMap<K, IdentityMintError>,
    anon_nominals: AHashMap<AnonymousNominalKey<K, M>, Type>,
    /// Reverse join for provider-local anonymous types. Multiple durable keys
    /// may deliberately name one issued type; the first registration remains
    /// the deterministic export identity, matching the append-only mint order.
    anonymous_identities: AHashMap<Type, AnonymousNominalKey<K, M>>,
    /// Anonymous keys whose mint failed after their recursive shell was
    /// published internally. The incomplete shell remains unreachable.
    anonymous_poisoned: AHashMap<AnonymousNominalKey<K, M>, IdentityMintError>,
    /// Fail-closed anonymous-digest ownership registry, the pool analog of the
    /// epoch's `anonymous_digest_owners` (RUE-1089, Theme 4b). Records the exact
    /// producer key that owns each presentation digest so a SECOND distinct key
    /// hashing to an owned digest is refused before any id or symbol is minted.
    anonymous_digest_owners: AHashMap<u128, AnonymousNominalKey<K, M>>,
    builtins: AHashMap<(Arc<str>, SemanticImportNominalKind), PoolNominal>,
    /// Exact logical-path to body-local file-id registry.
    module_files: AHashMap<Arc<str>, FileId>,
    /// Reverse registry enforcing the module/file bijection in average O(1)
    /// time, including provider-assigned ids.
    file_modules: AHashMap<FileId, Arc<str>>,
    /// Monotonic candidate for pool-assigned ids. Occupied explicit ids are
    /// skipped once overall, so admitting all body modules remains linear.
    next_module_file: u32,
    /// The pool's own parameter arena (which lives *beside* the type pool, not
    /// inside it). Callable identities intern
    /// their durable parameter vocabulary here on first consult, returning a
    /// `ParamRange` that indexes this arena — never the epoch's.
    param_arena: ParamArena,
    /// Minted function signatures, deduplicated by durable callable key: the
    /// arena is append-only, so a repeat consult must return the cached
    /// `ParamRange` rather than re-interning the same parameters.
    function_sigs: AHashMap<K, CallableSignature>,
    /// Minted method signatures, deduplicated by durable callable key.
    method_sigs: AHashMap<K, MethodSignature>,
    /// Callable keys whose signature mint failed. A repeat consult re-errors
    /// rather than re-running the partial mint (whose parameters may already sit
    /// orphaned in the append-only arena) — the callable analog of `poisoned`.
    callable_poisoned: AHashMap<K, IdentityMintError>,
    /// Per-body well-known `Option(payload)` registry (RUE-1112,
    /// RUE-1091 r6c). Maps an expected payload [`Type`] to the trusted standard-library
    /// `Option` enum minted for that payload, populated narrowly by
    /// [`Self::install_well_known_option_types`] before body analysis — never
    /// from the body's own composition/import universe. The provider consumer
    /// is fallible-intrinsic resolution (`resolve_option_result_type`).
    well_known_option_by_payload: AHashMap<Type, Type>,
    /// Anonymous enum identities (canonical producer form) minted by the
    /// well-known `Option` registry install for this body — THE
    /// export-as-produced ruling: the export funnel (`provider_body_host.rs` /
    /// `produced_anonymous_nominals`) subtracts these identities from the
    /// initial anonymous baseline so the installing body EXPORTS them as
    /// produced anonymous nominals — exactly as a body materializing
    /// `Option(payload)` through the ordinary annotation/comptime path would —
    /// never leaking them as pre-existing imports with no producer. The
    /// provider baseline computation consults
    /// [`Self::is_well_known_option_identity`] to apply the same subtraction.
    /// A `BTreeSet`, matching the epoch set's deterministic order.
    well_known_option_identities: std::collections::BTreeSet<AnonymousNominalKey<K, M>>,
    /// The recorded refusal of a FAILED well-known `Option` install — the
    /// well-known analog of `poisoned` / `callable_poisoned`. The install
    /// mutates the pool in place, so a mid-batch refusal would otherwise
    /// leave earlier keys' rulings and already-recorded demand pairs
    /// observable. This poison closes that gap: a repeat install
    /// re-errors with the recorded refusal (never re-running the partial
    /// install), and every well-known accessor
    /// ([`Self::is_well_known_option_identity`],
    /// [`Self::well_known_option_identity_count`],
    /// [`Self::well_known_option_for_payload`]) answers as if nothing was
    /// installed — no observable partial success either way.
    well_known_poisoned: Option<IdentityMintError>,
    // ---- Const identity family ---------------------------------------------
    /// Minted declaration-level const payloads, deduplicated by durable key.
    /// The request-local declaration span is supplied separately by the RIR
    /// handle and therefore is not cached here.
    const_values: AHashMap<K, ConstIdentity>,
    /// Const keys whose assembly failed after a nested type/value mint began.
    /// Repeat consults re-error instead of exposing or re-running partial state.
    const_poisoned: AHashMap<K, IdentityMintError>,
}

/// One task-owned identity universe shared by every provider fact driver used
/// for a body analysis.
///
/// Pool-relative `Type`, `StructId`, `EnumId`, `ParamRange`, and `ModuleId`
/// handles are meaningful only inside the pool/registry that minted them. The
/// call, endpoint, and aggregate drivers therefore clone this lightweight
/// handle rather than constructing peer pools.
#[derive(Default)]
struct ProviderMethodRegistry {
    anonymous: AHashMap<(StructId, Spur), MethodInfo>,
    anonymous_by_owner: AHashMap<(FileId, Arc<str>, Arc<str>), (StructId, Spur)>,
    named: AHashMap<(StructId, Spur), MethodInfo>,
    named_by_owner: AHashMap<(FileId, Arc<str>, Arc<str>), (StructId, Spur)>,
}

impl ProviderMethodRegistry {
    fn register_anonymous(
        &mut self,
        compact: (StructId, Spur),
        owner: (FileId, Arc<str>, Arc<str>),
        info: MethodInfo,
    ) -> bool {
        if self.anonymous.contains_key(&compact) || self.anonymous_by_owner.contains_key(&owner) {
            return false;
        }
        self.anonymous.insert(compact, info);
        self.anonymous_by_owner.insert(owner, compact);
        true
    }

    fn register_named(
        &mut self,
        compact: (StructId, Spur),
        owner: (FileId, Arc<str>, Arc<str>),
        info: MethodInfo,
    ) -> bool {
        if self.named.contains_key(&compact) || self.named_by_owner.contains_key(&owner) {
            return false;
        }
        self.named.insert(compact, info);
        self.named_by_owner.insert(owner, compact);
        true
    }

    fn method(&self, compact: (StructId, Spur)) -> Option<MethodInfo> {
        self.anonymous
            .get(&compact)
            .or_else(|| self.named.get(&compact))
            .copied()
    }

    fn method_by_owner(&self, owner: &(FileId, Arc<str>, Arc<str>)) -> Option<MethodInfo> {
        self.anonymous_by_owner
            .get(owner)
            .and_then(|compact| self.anonymous.get(compact))
            .or_else(|| {
                self.named_by_owner
                    .get(owner)
                    .and_then(|compact| self.named.get(compact))
            })
            .copied()
    }

    fn named_by_owner(&self, owner: &(FileId, Arc<str>, Arc<str>)) -> Option<MethodInfo> {
        self.named_by_owner
            .get(owner)
            .and_then(|compact| self.named.get(compact))
            .copied()
    }
}

pub struct ProviderIdentityContext<K, M, S> {
    pool: Rc<RefCell<BodyIdentityPool<K, M, S>>>,
    modules: Rc<RefCell<ProviderModuleRegistry<M>>>,
    /// One request-local method authority shared by every provider facade.
    /// Each entry is registered atomically under its compact and durable-owner
    /// preimages; the owner maps point back into the canonical compact maps.
    methods: Rc<RefCell<ProviderMethodRegistry>>,
    frozen: Rc<Cell<bool>>,
    /// Compatibility contexts retain the historical post-seal refusal. Only
    /// [`ProviderBodyAnalysisState`] enables the append-only overlay protocol.
    allow_post_seal_overlay: bool,
}

impl<K, M, S> Clone for ProviderIdentityContext<K, M, S> {
    fn clone(&self) -> Self {
        Self {
            pool: Rc::clone(&self.pool),
            modules: Rc::clone(&self.modules),
            methods: Rc::clone(&self.methods),
            frozen: Rc::clone(&self.frozen),
            allow_post_seal_overlay: self.allow_post_seal_overlay,
        }
    }
}

impl<K, M, S> ProviderIdentityContext<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Eq + Hash,
    S: DurableNominalSource<K, M>,
{
    /// Create the single identity universe for one provider-driven body.
    pub fn new(source: S) -> Self {
        Self::with_interner_mode(source, Rc::new(ThreadedRodeo::new()), false)
    }

    fn with_interner_mode(
        source: S,
        interner: Rc<ThreadedRodeo>,
        allow_post_seal_overlay: bool,
    ) -> Self {
        Self {
            pool: Rc::new(RefCell::new(BodyIdentityPool::new(source, interner))),
            modules: Rc::new(RefCell::new(ProviderModuleRegistry::default())),
            methods: Rc::new(RefCell::new(ProviderMethodRegistry::default())),
            frozen: Rc::new(Cell::new(false)),
            allow_post_seal_overlay,
        }
    }

    /// Return the interner authority shared by all provider fact state in this
    /// identity context.
    pub fn interner(&self) -> Rc<ThreadedRodeo> {
        Rc::clone(&self.pool.borrow().interner)
    }

    /// Intern a provider-facing name in the shared body authority.
    pub fn name_symbol(&self, name: &str) -> Spur {
        self.pool.borrow().intern_name(name)
    }

    pub(in crate::sema) fn pool(&self) -> Ref<'_, BodyIdentityPool<K, M, S>> {
        self.pool.borrow()
    }

    pub(in crate::sema) fn pool_mut(&self) -> Option<RefMut<'_, BodyIdentityPool<K, M, S>>> {
        if self.frozen.get() && !self.allow_post_seal_overlay {
            None
        } else {
            Some(self.pool.borrow_mut())
        }
    }

    pub(in crate::sema) fn fail_closed(mut self) -> Self {
        self.allow_post_seal_overlay = false;
        self
    }

    /// Clone the pool's stable type-universe handle while holding the outer
    /// `RefCell` borrow only briefly. The returned pool uses its own locks, so a
    /// read closure may safely consult another driver sharing this context.
    pub(in crate::sema) fn type_pool(&self) -> Rc<TypeInternPool> {
        Rc::clone(&self.pool.borrow().type_pool)
    }

    pub(in crate::sema) fn finalize_containment_metadata(&self) -> Option<()> {
        self.pool.borrow_mut().finalize_containment_metadata()?;
        self.frozen.set(true);
        Some(())
    }

    pub(in crate::sema) fn modules(&self) -> Ref<'_, ProviderModuleRegistry<M>> {
        self.modules.borrow()
    }

    pub(in crate::sema) fn modules_mut(&self) -> RefMut<'_, ProviderModuleRegistry<M>> {
        self.modules.borrow_mut()
    }

    /// Install one anonymous method under both lookup preimages in one atomic
    /// operation. Conflicting or duplicate registrations fail closed.
    pub fn register_anonymous_method(
        &self,
        file: FileId,
        owner: &str,
        method: &str,
        info: MethodInfo,
    ) -> bool {
        let Some(owner_id) = info.struct_type.as_struct() else {
            return false;
        };
        let method_symbol = self.pool().intern_name(method);
        self.methods.borrow_mut().register_anonymous(
            (owner_id, method_symbol),
            (file, Arc::from(owner), Arc::from(method)),
            info,
        )
    }

    /// Install one named method under both lookup preimages. The registry keeps
    /// it separate from anonymous entries so lookups preserve anonymous-first
    /// precedence while `named_method_info` remains named-only.
    pub(in crate::sema) fn register_named_method(
        &self,
        file: FileId,
        owner: &str,
        method: &str,
        info: MethodInfo,
    ) -> bool {
        let Some(owner_id) = info.struct_type.as_struct() else {
            return false;
        };
        let method_symbol = self.pool().intern_name(method);
        self.methods.borrow_mut().register_named(
            (owner_id, method_symbol),
            (file, Arc::from(owner), Arc::from(method)),
            info,
        )
    }

    pub(in crate::sema) fn method(&self, key: (StructId, Spur)) -> Option<MethodInfo> {
        self.methods.borrow().method(key)
    }

    pub(in crate::sema) fn method_for_owner(
        &self,
        file: FileId,
        owner: &str,
        method: &str,
    ) -> Option<MethodInfo> {
        self.methods
            .borrow()
            .method_by_owner(&(file, Arc::from(owner), Arc::from(method)))
    }

    pub(in crate::sema) fn named_method_for_owner(
        &self,
        file: FileId,
        owner: &str,
        method: &str,
    ) -> Option<MethodInfo> {
        self.methods
            .borrow()
            .named_by_owner(&(file, Arc::from(owner), Arc::from(method)))
    }
}

/// The rue-air-owned type and parameter authority for one provider body.
///
/// Provider fact stores clone the identity context from this state; they do
/// not create peer pools or interners. `finalize_containment_metadata` seals
/// the exact prerequisite facts and rebases the authority onto append-only
/// type/parameter overlays, so lazy body-local materialization remains valid.
pub struct ProviderBodyAnalysisState<K, M, S> {
    identity: ProviderIdentityContext<K, M, S>,
    /// Stable outer handle; containment sealing rebases its locked inner value
    /// in place so engine-facing references never go stale.
    type_pool: Rc<TypeInternPool>,
}

impl<K, M, S> Clone for ProviderBodyAnalysisState<K, M, S> {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            type_pool: Rc::clone(&self.type_pool),
        }
    }
}

impl<K, M, S> ProviderBodyAnalysisState<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Eq + Hash,
    S: DurableNominalSource<K, M>,
{
    pub fn new(source: S, interner: Rc<ThreadedRodeo>) -> Self {
        let identity = ProviderIdentityContext::with_interner_mode(source, interner, true);
        let type_pool = identity.type_pool();
        Self {
            identity,
            type_pool,
        }
    }

    pub fn identity_context(&self) -> ProviderIdentityContext<K, M, S> {
        self.identity.clone()
    }

    pub fn interner(&self) -> Rc<ThreadedRodeo> {
        self.identity.interner()
    }

    pub fn type_pool(&self) -> Rc<TypeInternPool> {
        Rc::clone(&self.type_pool)
    }

    pub fn with_type_pool<R>(&self, read: impl FnOnce(&TypeInternPool) -> R) -> R {
        read(&self.type_pool)
    }

    pub fn with_type_pool_mut<R>(&self, write: impl FnOnce(&TypeInternPool) -> R) -> R {
        write(&self.type_pool)
    }

    pub fn with_param_arena<R>(&self, read: impl FnOnce(&ParamArena) -> R) -> R {
        read(self.identity.pool().param_arena())
    }

    /// Copy one exact callable signature point without lending the mutable
    /// arena across a lazy provider fact operation.
    pub fn param_data(&self, range: ParamRange) -> crate::ParamRangeData {
        self.with_param_arena(|arena| arena.copy_range(range))
    }

    pub fn allocate_params(
        &self,
        names: impl IntoIterator<Item = Spur>,
        types: impl IntoIterator<Item = Type>,
        modes: impl IntoIterator<Item = RirParamMode>,
        comptime: impl IntoIterator<Item = bool>,
    ) -> ParamRange {
        self.identity
            .pool_mut()
            .expect("provider identity authority is available")
            .param_arena
            .alloc(names, types, modes, comptime)
    }

    pub fn finalize_containment_metadata(&self) -> Option<()> {
        self.identity.finalize_containment_metadata()
    }

    pub fn base_sealed(&self) -> bool {
        self.identity.frozen.get()
    }

    pub(in crate::sema) fn require_rir_authority(&self, rir: &BodyRirView<'_>) -> bool {
        let state_interner = self.interner();
        std::ptr::eq(rir.rir_interner(), Rc::as_ref(&state_interner))
    }
}

/// The durable-signature-derived subset of a [`FunctionInfo`], minted once and
/// cached by callable key. Every field here is recoverable from the durable
/// signature facts alone; the request/RIR-carried remainder (`body`,
/// `declaration`, `span`, `return_type_syntax`, `is_c_export`, the
/// `@allow` flags, `file_id`) is supplied by a [`FunctionIdentityHandle`] — the
/// 2c / request-local seam.
#[derive(Clone, Copy)]
struct CallableSignature {
    params: ParamRange,
    return_type: Type,
    is_generic: bool,
    is_pub: bool,
    is_unchecked: bool,
    is_extern: bool,
}

/// The durable-signature-derived subset of a [`MethodInfo`], minted once and
/// cached by callable key. `struct_type`, `has_self`, `self_mode`, `params`,
/// and `return_type` are durable; `self_is_mut`, `body`, and `span` are
/// request/RIR-carried and arrive on a [`MethodIdentityHandle`].
#[derive(Clone, Copy)]
struct MethodSignature {
    receiver: Type,
    has_self: bool,
    self_mode: RirParamMode,
    params: ParamRange,
    return_type: Type,
    returns_borrow: bool,
}

fn anonymous_nominal_keys_canonically_equal<K: Eq, M: Eq>(
    left: &AnonymousNominalKey<K, M>,
    right: &AnonymousNominalKey<K, M>,
) -> bool {
    fn collapsed<K, M>(mut function: &FunctionInstanceKey<K, M>) -> &FunctionInstanceKey<K, M> {
        while let FunctionInstanceKey::Specialization { base, arguments } = function
            && arguments.types.is_empty()
            && arguments.values.is_empty()
        {
            function = base;
        }
        function
    }

    left.kind == right.kind
        && left.anchor == right.anchor
        && left.arguments == right.arguments
        && match (&left.producer, &right.producer) {
            (StableProducerId::Definition(left), StableProducerId::Definition(right)) => {
                left == right
            }
            (StableProducerId::Function(left), StableProducerId::Function(right)) => {
                collapsed(left) == collapsed(right)
            }
            _ => false,
        }
}

impl<K, M, S> BodyIdentityPool<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Eq + Hash,
    S: DurableNominalSource<K, M>,
{
    /// Create an empty pool with the builtin enums and the core `str` identity
    /// pre-registered, mirroring a fresh import epoch.
    pub(in crate::sema) fn new(source: S, interner: Rc<ThreadedRodeo>) -> Self {
        let type_pool = Rc::new(TypeInternPool::new());
        let mut builtins = AHashMap::new();

        for builtin in rue_builtins::BUILTIN_ENUMS {
            let symbol = interner.get_or_intern(builtin.name);
            let (id, _) = type_pool.register_enum(
                symbol,
                EnumDef {
                    name: Arc::from(builtin.name),
                    variants: builtin.variants.iter().map(|v| Arc::from(*v)).collect(),
                    variant_payloads: Vec::new(),
                    is_pub: true,
                    file_id: FileId::DEFAULT,
                },
            );
            builtins.insert(
                (Arc::from(builtin.name), SemanticImportNominalKind::Enum),
                PoolNominal::Enum(id),
            );
        }

        // The core `str` identity: an ordinary builtin struct paired with a
        // runtime definition. Registered exactly as `SemanticImportedProgram::new`
        // registers it.
        let str_symbol = interner.get_or_intern("str");
        let ptr_id = type_pool.intern_ptr_const_from_type(Type::U8);
        let (str_id, _) = type_pool.register_struct(
            str_symbol,
            StructDef {
                name: Arc::from("str"),
                fields: vec![
                    StructField {
                        name: "ptr".to_owned(),
                        ty: Type::new_ptr_const(ptr_id),
                    },
                    StructField {
                        name: "len".to_owned(),
                        ty: Type::U64,
                    },
                ],
                is_copy: true,
                is_linear: false,
                destructor: None,
                is_builtin: true,
                is_pub: true,
                file_id: FileId::DEFAULT,
            },
        );
        builtins.insert(
            (Arc::from("str"), SemanticImportNominalKind::Struct),
            PoolNominal::Struct(str_id),
        );

        Self {
            type_pool,
            interner,
            source,
            struct_ids: AHashMap::new(),
            enum_ids: AHashMap::new(),
            struct_identities: AHashMap::new(),
            enum_identities: AHashMap::new(),
            poisoned: AHashMap::new(),
            anon_nominals: AHashMap::new(),
            anonymous_identities: AHashMap::new(),
            anonymous_poisoned: AHashMap::new(),
            anonymous_digest_owners: AHashMap::new(),
            builtins,
            module_files: AHashMap::new(),
            file_modules: AHashMap::new(),
            next_module_file: 1,
            param_arena: ParamArena::new(),
            function_sigs: AHashMap::new(),
            method_sigs: AHashMap::new(),
            callable_poisoned: AHashMap::new(),
            well_known_option_by_payload: AHashMap::new(),
            well_known_option_identities: std::collections::BTreeSet::new(),
            well_known_poisoned: None,
            const_values: AHashMap::new(),
            const_poisoned: AHashMap::new(),
        }
    }

    /// The body-local pool. Downstream reads (`struct_def`, `enum_def`,
    /// `struct_symbol_name`, layout, copyability) go through this handle exactly
    /// as the analyzer reads `sema.type_pool`.
    pub(in crate::sema) fn type_pool(&self) -> &TypeInternPool {
        &self.type_pool
    }

    /// The body-local parameter arena. Callable identities' [`ParamRange`]s index
    /// this arena, read exactly as the analyzer reads `sema.param_arena`.
    pub(in crate::sema) fn param_arena(&self) -> &ParamArena {
        &self.param_arena
    }

    /// Seal the exact prerequisite facts, then retain an append-only overlay.
    ///
    /// The type and parameter bases are never mutated after this point. A
    /// later lazy nominal/callable consult writes only to the derived layers,
    /// while existing `Type` and `ParamRange` handles continue to resolve
    /// through the shared prefix. Re-running this method is the explicit safe
    /// re-finalization protocol: any additions made since the previous seal
    /// are finalized and become part of the next sealed base.
    pub(in crate::sema) fn finalize_containment_metadata(&mut self) -> Option<()> {
        self.type_pool.finalize_containment_metadata().ok()?;
        self.type_pool.rebase_overlay_in_place();
        self.param_arena = self.param_arena.derive_overlay();
        Some(())
    }

    /// Whether a minted type transitively needs drop, or `None` before
    /// [`Self::finalize_containment_metadata`] froze the containment graph.
    /// Named and anonymous destructor metadata is installed before this join.
    pub(in crate::sema) fn type_needs_drop(&self, ty: Type) -> Option<bool> {
        self.type_pool.try_type_needs_drop(ty)
    }

    /// Whether a minted type transitively carries a linear component, or `None`
    /// before [`Self::finalize_containment_metadata`].
    pub(in crate::sema) fn type_carries_linear(&self, ty: Type) -> Option<bool> {
        self.type_pool.try_type_carries_linear(ty)
    }

    /// Resolve an interned parameter/name symbol to its source string. The
    /// pool's [`Spur`]s are pool-interner-relative (like its pool indices), so
    /// name parity is asserted through resolved strings, never raw symbols.
    pub(in crate::sema) fn resolve_symbol(&self, symbol: Spur) -> &str {
        self.interner.resolve(&symbol)
    }

    #[cfg(test)]
    /// Record the concrete [`Type`] an anonymous nominal was issued, so a later
    /// consult of its key resolves by lookup. Mirrors the epoch's
    /// `anon_struct_identities` / `anon_enum_identities` maps that
    /// `resolve_instance_type` consults for the anonymous arm.
    pub(in crate::sema) fn register_issued_anonymous(
        &mut self,
        key: AnonymousNominalKey<K, M>,
        ty: Type,
    ) where
        M: Clone,
    {
        debug_assert!(
            !self.anonymous_poisoned.contains_key(&key),
            "a poisoned anonymous identity cannot be registered as successful"
        );
        self.record_anonymous_identity(key, ty);
    }

    /// Intern a source name into the pool's own interner, returning its
    /// pool-relative [`Spur`]. The provider-driven endpoint driver
    /// ([`super::body_endpoint::ProviderEndpointFacts`]) interns each consulted
    /// name here — the `interner.get_or_intern` analog of the epoch's
    /// `interner.get` — so the symbol it then reverses through
    /// [`Self::resolve_symbol`] and keys the overlay on stays in this pool's own
    /// symbol space, never the shared whole-program interner's.
    pub(in crate::sema) fn intern_name(&self, name: &str) -> Spur {
        self.interner.get_or_intern(name)
    }

    fn record_struct_identity(&mut self, key: K, id: StructId) {
        if let Some(previous) = self.struct_ids.insert(key.clone(), id) {
            assert!(
                previous == id,
                "a durable struct identity changed local ids"
            );
        }
        self.struct_identities.entry(id).or_insert(key);
    }

    fn record_enum_identity(&mut self, key: K, id: EnumId) {
        if let Some(previous) = self.enum_ids.insert(key.clone(), id) {
            assert!(previous == id, "a durable enum identity changed local ids");
        }
        self.enum_identities.entry(id).or_insert(key);
    }

    fn record_anonymous_identity(&mut self, key: AnonymousNominalKey<K, M>, ty: Type)
    where
        M: Clone,
    {
        if let Some(previous) = self.anon_nominals.insert(key.clone(), ty) {
            assert!(
                previous == ty,
                "a durable anonymous identity changed local types"
            );
        }
        self.anonymous_identities.entry(ty).or_insert(key);
    }

    fn rollback_anonymous_identity(&mut self, key: &AnonymousNominalKey<K, M>)
    where
        M: Clone,
    {
        let Some(ty) = self.anon_nominals.remove(key) else {
            return;
        };
        assert!(
            self.anonymous_identities.get(&ty) == Some(key),
            "a failed anonymous shell must own its reverse identity"
        );
        self.anonymous_identities.remove(&ty);
    }

    /// Mint (on first consult) or dedup the generated fixed-capacity string
    /// struct `Str(N)` for a LITERAL capacity `N` (ADR-0043 Phase 5, RUE-326).
    ///
    /// The pool analog of the epoch's `get_or_create_str_fixed_struct`
    /// (`typeck.rs`): a generated builtin struct named `Str(N)` reusing the `str`
    /// fat-pointer shape `{ ptr: ptr const u8, len: u64 }`, copyable and builtin,
    /// registered under its canonical name so every reference to the same
    /// capacity shares one id. Deduped by name via `register_struct`.
    ///
    /// Scope (r6b): literal `N` only. A `Str(N)` whose capacity is a comptime
    /// array-length expression needs the r5 array-length machinery to reduce `N`
    /// to a literal first; that reduction is the caller's job (the type-syntax
    /// resolver), so the pool takes an already-reduced `u64` and never re-derives
    /// a non-literal capacity.
    pub(in crate::sema) fn get_or_create_str_fixed(&mut self, capacity: u64) -> Type {
        let name: Arc<str> = Arc::from(format!("Str({capacity})").as_str());
        let symbol = self.interner.get_or_intern(&name);
        let ptr_id = self.type_pool.intern_ptr_const_from_type(Type::U8);
        let (id, _) = self.type_pool.register_struct(
            symbol,
            StructDef {
                name,
                fields: vec![
                    StructField {
                        name: "ptr".to_owned(),
                        ty: Type::new_ptr_const(ptr_id),
                    },
                    StructField {
                        name: "len".to_owned(),
                        ty: Type::U64,
                    },
                ],
                is_copy: true,
                is_linear: false,
                destructor: None,
                is_builtin: true,
                is_pub: true,
                file_id: FileId::DEFAULT,
            },
        );
        Type::new_struct(id)
    }

    /// Mint (on first consult) or dedup a concrete [`Type`] for a durable type.
    ///
    /// The provider-driven analog of `body_endpoint::resolve_instance_type`: a
    /// direct recursive walk of the durable type algebra whose nominal arm mints
    /// and registers rather than looks up.
    pub(in crate::sema) fn resolve(
        &mut self,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, IdentityMintError> {
        use SemanticImportType as S;
        Ok(match value {
            S::I8 => Type::I8,
            S::I16 => Type::I16,
            S::I32 => Type::I32,
            S::I64 => Type::I64,
            S::U8 => Type::U8,
            S::U16 => Type::U16,
            S::U32 => Type::U32,
            S::U64 => Type::U64,
            S::Bool => Type::BOOL,
            S::Unit => Type::UNIT,
            S::Never => Type::NEVER,
            S::ComptimeType => Type::COMPTIME_TYPE,
            S::BuiltinNominal { name, kind } => {
                if let Some(capacity) = crate::types::fixed_string_capacity(name) {
                    if *kind != SemanticImportNominalKind::Struct {
                        return Err(IdentityMintError::BuiltinNominalKindMismatch);
                    }
                    self.get_or_create_str_fixed(capacity)
                } else {
                    match self.builtins.get(&(name.clone(), *kind)).copied() {
                        Some(PoolNominal::Struct(id)) => Type::new_struct(id),
                        Some(PoolNominal::Enum(id)) => Type::new_enum(id),
                        None => {
                            return Err(if self.builtins.keys().any(|(known, _)| known == name) {
                                IdentityMintError::BuiltinNominalKindMismatch
                            } else {
                                IdentityMintError::UnknownBuiltinNominal
                            });
                        }
                    }
                }
            }
            S::Nominal(key) => self.mint_named(key)?,
            S::AnonymousNominal(key) => self
                .anon_nominals
                .get(key)
                .copied()
                .or_else(|| {
                    self.anon_nominals.iter().find_map(|(candidate, ty)| {
                        anonymous_nominal_keys_canonically_equal(candidate, key).then_some(*ty)
                    })
                })
                .ok_or(IdentityMintError::MissingAnonymous)?,
            S::Array { element, len } => {
                let element = self.resolve(element)?;
                self.type_pool
                    .try_intern_array(element, *len)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)?
            }
            S::PtrConst(inner) => {
                let pointee = self.resolve(inner)?;
                self.type_pool
                    .try_intern_ptr_const(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)?
            }
            S::PtrMut(inner) => {
                let pointee = self.resolve(inner)?;
                self.type_pool
                    .try_intern_ptr_mut(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)?
            }
            S::Slice { element, name } => {
                // A slice view is a generated fat-pointer struct. Registered
                // exactly as `SemanticImportedProgram::import_type_local`
                // registers it (ptr + len, builtin, copy).
                let element = self.resolve(element)?;
                let symbol = self.interner.get_or_intern(name.as_ref());
                let pointer = self.type_pool.intern_ptr_const_from_type(element);
                let (id, _) = self.type_pool.register_struct(
                    symbol,
                    StructDef {
                        name: Arc::from(name.as_ref()),
                        fields: vec![
                            StructField {
                                name: "ptr".to_owned(),
                                ty: Type::new_ptr_const(pointer),
                            },
                            StructField {
                                name: "len".to_owned(),
                                ty: Type::U64,
                            },
                        ],
                        is_copy: true,
                        is_linear: false,
                        destructor: None,
                        is_builtin: true,
                        is_pub: true,
                        file_id: FileId::DEFAULT,
                    },
                );
                Type::new_struct(id)
            }
            S::Module(_) => return Err(IdentityMintError::Deferred("module identity")),
            S::GenericParameter(_) => {
                return Err(IdentityMintError::Deferred("generic parameter"));
            }
        })
    }

    /// Resolve a durable type returned by an exact provider query, minting any
    /// anonymous nominals in the type before the ordinary recursive resolver
    /// consumes them.
    pub(in crate::sema) fn resolve_provider_type(
        &mut self,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, IdentityMintError>
    where
        M: Clone,
        S: DurableAnonymousSource<K, M>,
    {
        match value {
            SemanticImportType::AnonymousNominal(key) => self.find_or_create_anon(key),
            SemanticImportType::Nominal(key) => self.mint_named_provider(key),
            SemanticImportType::Array { element, len } => {
                let element = self.resolve_provider_type(element)?;
                self.type_pool
                    .try_intern_array(element, *len)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)
            }
            SemanticImportType::PtrConst(pointee) => {
                let pointee = self.resolve_provider_type(pointee)?;
                self.type_pool
                    .try_intern_ptr_const(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)
            }
            SemanticImportType::PtrMut(pointee) => {
                let pointee = self.resolve_provider_type(pointee)?;
                self.type_pool
                    .try_intern_ptr_mut(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)
            }
            _ => self.resolve(value),
        }
    }

    fn mint_named_provider(&mut self, key: &K) -> Result<Type, IdentityMintError>
    where
        M: Clone,
        S: DurableAnonymousSource<K, M>,
    {
        if let Some(err) = self.poisoned.get(key) {
            return Err(err.clone());
        }
        if let Some(&id) = self.struct_ids.get(key) {
            return Ok(Type::new_struct(id));
        }
        if let Some(&id) = self.enum_ids.get(key) {
            return Ok(Type::new_enum(id));
        }
        let DurableNominal {
            name,
            module_path,
            is_public,
            is_builtin,
            lang_item,
            is_repr_c,
            has_destructor,
            body,
        } = self
            .source
            .nominal(key)
            .ok_or(IdentityMintError::MissingNominal)?;
        let requested_file = self.source.nominal_file_id(key);
        let file_id = self.file_for_module(&module_path, requested_file)?;
        let symbol = self.interner.get_or_intern(name.as_ref());
        let name = name.clone();
        match body {
            DurableNominalBody::Struct {
                fields,
                is_copy,
                is_linear,
            } => {
                let (id, _) = self.type_pool.declare_struct(
                    symbol,
                    StructDef {
                        name: name.clone(),
                        fields: Vec::new(),
                        is_copy: is_copy && !has_destructor,
                        is_linear,
                        destructor: None,
                        is_builtin,
                        is_pub: is_public,
                        file_id,
                    },
                );
                self.record_struct_identity(key.clone(), id);
                if let Some(lang_item) = lang_item {
                    self.type_pool.set_struct_lang_item(id, lang_item);
                }
                if is_repr_c {
                    self.type_pool.set_struct_repr_c(id);
                }
                let mut resolved = Vec::with_capacity(fields.len());
                for (field_name, field_ty) in fields.iter() {
                    let ty = match self.resolve_provider_type(field_ty) {
                        Ok(ty) => ty,
                        Err(err) => {
                            self.poisoned.insert(key.clone(), err.clone());
                            return Err(err);
                        }
                    };
                    resolved.push(StructField {
                        name: field_name.to_string(),
                        ty,
                    });
                }
                // The destructor symbol derives from declaration-time identity
                // only — name, defining file, builtin and lang-item status —
                // all fixed by the shell above, so it is spelled before
                // completion and carried in the completing definition. Folding
                // it in (instead of `set_struct_destructor` afterwards) keeps
                // the completion eligible for incremental containment facts
                // rather than marking the whole pool stale.
                let destructor = has_destructor.then(|| {
                    Arc::<str>::from(
                        format!("{}.__drop", self.type_pool.struct_symbol_name(id)).as_str(),
                    )
                });
                self.type_pool.complete_declared_struct(
                    id,
                    StructDef {
                        name,
                        fields: resolved,
                        is_copy: is_copy && !has_destructor,
                        is_linear,
                        destructor,
                        is_builtin,
                        is_pub: is_public,
                        file_id,
                    },
                );
                Ok(Type::new_struct(id))
            }
            DurableNominalBody::Enum { variants } => {
                let variant_names = variants
                    .iter()
                    .map(|(name, _)| Arc::from(name.as_ref()))
                    .collect::<Arc<[Arc<str>]>>();
                let (id, _) = self.type_pool.declare_enum(
                    symbol,
                    EnumDef {
                        name: name.clone(),
                        variants: variant_names.clone(),
                        variant_payloads: Vec::new(),
                        is_pub: is_public,
                        file_id,
                    },
                );
                self.record_enum_identity(key.clone(), id);
                let mut variant_payloads = Vec::with_capacity(variants.len());
                for (_, payload) in variants.iter() {
                    let mut resolved = Vec::with_capacity(payload.len());
                    for ty in payload.iter() {
                        match self.resolve_provider_type(ty) {
                            Ok(ty) => resolved.push(ty),
                            Err(err) => {
                                self.poisoned.insert(key.clone(), err.clone());
                                return Err(err);
                            }
                        }
                    }
                    variant_payloads.push(resolved);
                }
                self.type_pool.complete_declared_enum(
                    id,
                    EnumDef {
                        name,
                        variants: variant_names,
                        variant_payloads,
                        is_pub: is_public,
                        file_id,
                    },
                );
                Ok(Type::new_enum(id))
            }
        }
    }

    /// Mint or dedup a named nominal.
    ///
    /// On first consult the shell is registered and inserted into the dedup map
    /// **before** its field / payload types are resolved, so a nominal that
    /// refers to itself (through a pointer) resolves the recursive reference to
    /// the shell id — the epoch's declare-then-complete discipline.
    fn mint_named(&mut self, key: &K) -> Result<Type, IdentityMintError> {
        // A failed mint leaves an incomplete shell in the intern pool (the
        // shell must pre-register for recursive self-reference, and the pool
        // is append-only, so it cannot be rolled back). The poison map keeps
        // that shell unreachable: a repeat consult re-errors instead of
        // handing out an id whose `struct_def` read would panic.
        if let Some(err) = self.poisoned.get(key) {
            return Err(err.clone());
        }
        if let Some(&id) = self.struct_ids.get(key) {
            return Ok(Type::new_struct(id));
        }
        if let Some(&id) = self.enum_ids.get(key) {
            return Ok(Type::new_enum(id));
        }

        let DurableNominal {
            name,
            module_path,
            is_public,
            is_builtin,
            lang_item,
            is_repr_c,
            has_destructor,
            body,
        } = self
            .source
            .nominal(key)
            .ok_or(IdentityMintError::MissingNominal)?;

        let file_id = self.file_for_module(&module_path, None)?;
        let symbol = self.interner.get_or_intern(name.as_ref());
        let name = name.clone();

        match body {
            DurableNominalBody::Struct {
                fields,
                is_copy,
                is_linear,
            } => {
                let (id, _) = self.type_pool.declare_struct(
                    symbol,
                    StructDef {
                        name: name.clone(),
                        fields: Vec::new(),
                        is_copy: is_copy && !has_destructor,
                        is_linear,
                        destructor: None,
                        is_builtin,
                        is_pub: is_public,
                        file_id,
                    },
                );
                self.record_struct_identity(key.clone(), id);
                if let Some(lang_item) = lang_item {
                    self.type_pool.set_struct_lang_item(id, lang_item);
                }
                if is_repr_c {
                    self.type_pool.set_struct_repr_c(id);
                }

                let mut resolved = Vec::with_capacity(fields.len());
                for (field_name, field_ty) in fields.iter() {
                    let ty = match self.resolve(field_ty) {
                        Ok(ty) => ty,
                        Err(err) => {
                            self.poisoned.insert(key.clone(), err.clone());
                            return Err(err);
                        }
                    };
                    resolved.push(StructField {
                        name: field_name.to_string(),
                        ty,
                    });
                }
                // See `mint_named_provider`: the destructor symbol is fixed by
                // declaration-time identity, so it is spelled before completion
                // and folded into the completing definition to keep the
                // completion eligible for incremental containment facts.
                let destructor = has_destructor.then(|| {
                    Arc::<str>::from(
                        format!("{}.__drop", self.type_pool.struct_symbol_name(id)).as_str(),
                    )
                });
                self.type_pool.complete_declared_struct(
                    id,
                    StructDef {
                        name,
                        fields: resolved,
                        is_copy: is_copy && !has_destructor,
                        is_linear,
                        destructor,
                        is_builtin,
                        is_pub: is_public,
                        file_id,
                    },
                );
                Ok(Type::new_struct(id))
            }
            DurableNominalBody::Enum { variants } => {
                let variant_names: Arc<[Arc<str>]> = variants
                    .iter()
                    .map(|(name, _)| Arc::from(name.as_ref()))
                    .collect();
                let (id, _) = self.type_pool.declare_enum(
                    symbol,
                    EnumDef {
                        name: name.clone(),
                        variants: variant_names.clone(),
                        variant_payloads: Vec::new(),
                        is_pub: is_public,
                        file_id,
                    },
                );
                self.record_enum_identity(key.clone(), id);

                let mut variant_payloads = Vec::with_capacity(variants.len());
                for (_, payload) in variants.iter() {
                    let mut resolved = Vec::with_capacity(payload.len());
                    for ty in payload.iter() {
                        match self.resolve(ty) {
                            Ok(ty) => resolved.push(ty),
                            Err(err) => {
                                self.poisoned.insert(key.clone(), err.clone());
                                return Err(err);
                            }
                        }
                    }
                    variant_payloads.push(resolved);
                }
                self.type_pool.complete_declared_enum(
                    id,
                    EnumDef {
                        name,
                        variants: variant_names,
                        variant_payloads,
                        is_pub: is_public,
                        file_id,
                    },
                );
                Ok(Type::new_enum(id))
            }
        }
    }

    /// The body-local [`FileId`] for a module logical path, assigned on first
    /// sight or accepted from a provider. Both directions of the bijection are
    /// registered before nominal minting can render a qualified symbol.
    fn file_for_module(
        &mut self,
        module_path: &Arc<str>,
        requested_file: Option<FileId>,
    ) -> Result<FileId, IdentityMintError> {
        if let Some(&file) = self.module_files.get(module_path) {
            return match requested_file {
                Some(requested) if requested != file => {
                    Err(IdentityMintError::InvalidStructuralType)
                }
                _ => Ok(file),
            };
        }

        let file = if let Some(file) = requested_file {
            if self
                .file_modules
                .get(&file)
                .is_some_and(|path| path != module_path)
            {
                return Err(IdentityMintError::InvalidStructuralType);
            }
            file
        } else {
            loop {
                let file = FileId::new(self.next_module_file);
                if !self.file_modules.contains_key(&file) {
                    self.next_module_file = self.next_module_file.saturating_add(1);
                    break file;
                }
                self.next_module_file = self
                    .next_module_file
                    .checked_add(1)
                    .ok_or(IdentityMintError::InvalidStructuralType)?;
            }
        };

        if !self
            .type_pool
            .insert_symbol_path(file, module_path.to_string())
        {
            return Err(IdentityMintError::InvalidStructuralType);
        }
        self.module_files.insert(Arc::clone(module_path), file);
        self.file_modules.insert(file, Arc::clone(module_path));
        Ok(file)
    }
}

/// The reserved method name whose presence gives an anonymous struct a user
/// destructor (RUE-312); mirrored from the epoch's `find_or_create_anon_struct`.
const ANON_DROP_METHOD: &str = "__drop";

impl<K, M, S> BodyIdentityPool<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    S: DurableNominalSource<K, M> + DurableAnonymousSource<K, M>,
{
    /// Mint (on first consult) or dedup the producer-nominal anonymous
    /// struct / enum for a durable anonymous identity key.
    ///
    /// Identity is producer-nominal (ADR-0066): the
    /// `AnonymousNominalKey` alone owns the entity — there is no structural search
    /// across producers. The synthetic name is spelled from the SAME stable
    /// digest the epoch uses (the shared
    /// [`crate::stable_digest::stable_anonymous_identity_digest`]), fed the
    /// durable→stable-content relocation the [`DurableAnonymousSource`] supplies,
    /// so the pool and the epoch mint byte-identical `__anon_*_{digest:032x}`
    /// names. The digest is a presentation name only; a distinct key colliding on
    /// it is refused BEFORE any id or symbol is minted (the collision guard),
    /// exactly as the epoch's fail-closed registry rejects it.
    ///
    /// # Canonical producer form, enforced on entry
    ///
    /// The epoch only ever mints under the canonical producer form: its
    /// `canonical_function_producer` collapses an empty-argument function
    /// specialization to its base before any digest is taken, and production
    /// body-export carries that collapsed form (the warm==cold digest
    /// invariant). The pool enforces the same invariant HERE, by collapsing the
    /// incoming key ([`AnonymousNominalKey::with_canonical_producer`]) before
    /// dedup, digest, and shape consult — so a provider caller handing the
    /// non-collapsed form (e.g. the declaration-signature projection's
    /// `Specialization { base, args: [] }` wrapper) dedups onto, and spells the
    /// same digest as, the collapsed form rather than silently minting a
    /// divergent identity.
    pub(in crate::sema) fn find_or_create_anon(
        &mut self,
        key: &AnonymousNominalKey<K, M>,
    ) -> Result<Type, IdentityMintError> {
        // Entry canonicalization: every read below (dedup map, digest, shape
        // consult, registration) sees the canonical producer form.
        let key = key.with_canonical_producer();
        let key = key.as_ref();
        // Producer-nominal dedup: a key already minted resolves by lookup, so a
        // repeat consult re-mints nothing (the epoch's `anon_*_identities.get`).
        if let Some(&ty) = self.anon_nominals.get(key) {
            return Ok(ty);
        }
        // Successful and poisoned identities are disjoint: failed mints roll
        // their recursive shell back before publishing the error, and the
        // alternate registration entry point checks the invariant in debug builds.
        if let Some(error) = self.anonymous_poisoned.get(key) {
            return Err(error.clone());
        }

        let (shape, digest) = self
            .source
            .anonymous_shape_and_digest(key)
            .ok_or(IdentityMintError::MissingAnonymousShape)?;

        // The presentation digest is a pure function of the producer identity,
        // relocated to its request-independent stable content by the adapter. It
        // is spelled into the name only; guard a distinct key colliding on it
        // BEFORE reserving or registering, so no colliding entity or symbol is
        // ever published (RUE-1089, Theme 4b).
        self.guard_anonymous_digest_collision(digest, key)?;

        let minted = match shape {
            DurableAnonymousShape::Struct {
                fields,
                struct_method_names,
            } => self.mint_anon_struct(key, digest, &fields, &struct_method_names),
            DurableAnonymousShape::Enum { variants } => self.mint_anon_enum(key, digest, &variants),
        };
        match minted {
            // Both minting arms register the recursive shell before resolving
            // its shape. That registration is the durable identity on success;
            // inserting it again here would clone and rehash the full producer
            // key only to replace the same forward entry.
            Ok(ty) => {
                debug_assert_eq!(
                    self.anon_nominals.get(key).copied(),
                    Some(ty),
                    "every successful anonymous mint arm registers its identity",
                );
                Ok(ty)
            }
            Err(error) => {
                self.rollback_anonymous_identity(key);
                self.anonymous_poisoned.insert(key.clone(), error.clone());
                Err(error)
            }
        }
    }

    /// Install the per-body well-known `Option(payload)` registry (RUE-1112,
    /// RUE-1091 slice r6c) into this pool.
    ///
    /// `nominals` are the durable identities of the trusted-std `Option` enum
    /// specializations the per-body demand loop resolved; each is minted through
    /// the ordinary anonymous machinery ([`Self::find_or_create_anon`]) so its
    /// `__anon_enum_{digest}` name, copyability, visibility, and mangled symbol
    /// are byte-identical to the epoch install's `find_or_create_anon_enum`
    /// materialization. `option_by_payload` pairs each demanded payload type
    /// with its resolved `Option` type; both endpoints are pure mints/dedups
    /// once the enums are installed, recorded so fallible-intrinsic resolution
    /// binds the trusted `Option` under `?` even when the body never
    /// `@import`s it.
    ///
    /// # The export-as-produced ruling
    ///
    /// Every installed identity is recorded (canonical producer form) in the
    /// pool's well-known identity set. That set carries the export-as-produced
    /// ruling: the export funnel (`semantic_body_export.rs` via
    /// `provider_body_host.rs`'s baseline subtraction) treats these identities
    /// as PRODUCED anonymous nominals of the analyzed body — the body that
    /// binds a fallible intrinsic's `Option` is the body that produces it —
    /// never as pre-existing imports with no producer. The provider baseline
    /// computation consults [`Self::is_well_known_option_identity`] to apply
    /// that subtraction.
    ///
    /// # Failure semantics
    ///
    /// A non-enum durable shape is refused
    /// ([`IdentityMintError::WellKnownShapeMismatch`]) BEFORE any id or symbol
    /// is minted for that key; an absent shape or unresolvable registry type
    /// fails closed. A bounded fixpoint tolerates a nominal whose payload
    /// references another not-yet-installed well-known nominal (the trusted
    /// `Option` shapes are flat today, so one pass suffices); a round with no
    /// progress returns the blocking refusal. Any error is a fatal refusal
    /// for the requesting body, never an approximation.
    ///
    /// Atomicity is closed by poisoning: the install mutates in place, so ANY
    /// failure POISONS the well-known registry (`well_known_poisoned`, the
    /// well-known analog of the file's `poisoned` / `callable_poisoned`
    /// discipline): a repeat install re-errors with the recorded refusal
    /// rather than re-running the partial install, and every well-known
    /// accessor answers as if nothing was installed — no observable partial
    /// success.
    pub(in crate::sema) fn install_well_known_option_types(
        &mut self,
        nominals: &[AnonymousNominalKey<K, M>],
        option_by_payload: &[(SemanticImportType<K, M>, SemanticImportType<K, M>)],
    ) -> Result<(), IdentityMintError>
    where
        K: Ord,
        M: Ord,
    {
        // A failed install poisoned the whole well-known registry: re-error
        // with the recorded refusal, exactly as `mint_named` re-errors a
        // poisoned key instead of re-running the partial mint.
        if let Some(err) = &self.well_known_poisoned {
            return Err(err.clone());
        }
        match self.install_well_known_option_types_inner(nominals, option_by_payload) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.well_known_poisoned = Some(err.clone());
                Err(err)
            }
        }
    }

    /// The install body, separated so [`Self::install_well_known_option_types`]
    /// can record ANY refusal it returns into `well_known_poisoned` before
    /// surfacing it.
    fn install_well_known_option_types_inner(
        &mut self,
        nominals: &[AnonymousNominalKey<K, M>],
        option_by_payload: &[(SemanticImportType<K, M>, SemanticImportType<K, M>)],
    ) -> Result<(), IdentityMintError>
    where
        K: Ord,
        M: Ord,
    {
        let mut pending: Vec<&AnonymousNominalKey<K, M>> = nominals.iter().collect();
        while !pending.is_empty() {
            let mut progressed = false;
            let mut next = Vec::new();
            let mut blocking = None;
            for key in pending {
                let canonical = key.with_canonical_producer();
                // The trusted registry holds `Option` ENUM specializations
                // only; refuse a non-enum shape before minting anything for
                // this key (the epoch's `NominalShapeMismatch` check runs
                // before its `find_or_create_anon_enum` call).
                match self.source.anonymous_shape(canonical.as_ref()) {
                    Some(DurableAnonymousShape::Enum { .. }) => {}
                    Some(DurableAnonymousShape::Struct { .. }) => {
                        return Err(IdentityMintError::WellKnownShapeMismatch);
                    }
                    None => return Err(IdentityMintError::MissingAnonymousShape),
                }
                match self.find_or_create_anon(canonical.as_ref()) {
                    Ok(_) => {
                        progressed = true;
                        self.well_known_option_identities
                            .insert(canonical.into_owned());
                    }
                    // A payload referencing a not-yet-minted well-known
                    // anonymous nominal: retry after the rest of the round.
                    // Enum mints resolve every payload BEFORE registering, so
                    // a blocked mint published nothing and the retry is clean.
                    Err(IdentityMintError::MissingAnonymous) => {
                        blocking = Some(IdentityMintError::MissingAnonymous);
                        next.push(key);
                    }
                    Err(err) => return Err(err),
                }
            }
            if !progressed {
                return Err(blocking.unwrap_or(IdentityMintError::MissingAnonymous));
            }
            pending = next;
        }
        // Record the demand map. Both endpoints are pure dedups/lookups now
        // that the enums are minted (the epoch's `import_export_type` pair).
        for (payload, option) in option_by_payload {
            let payload_ty = self.resolve_well_known_registry_type(payload)?;
            let option_ty = self.resolve_well_known_registry_type(option)?;
            self.well_known_option_by_payload
                .insert(payload_ty, option_ty);
        }
        Ok(())
    }

    /// Resolve one well-known registry endpoint type. An anonymous nominal is a
    /// pure LOOKUP against the already-installed anonymous identities (the
    /// probe is entry-canonicalized first, per the r6b contract), failing
    /// closed with [`IdentityMintError::MissingAnonymous`] on a miss — the pool
    /// analog of the epoch's lookup-only `import_export_type` anonymous arm
    /// (`anon_enum_identities.get(..).ok_or(MissingNominal)`), which refuses a
    /// registry pair naming an identity its install never materialized. It
    /// never mints: minting here would publish an identity outside the ruling
    /// set exactly where the epoch fails closed. Every other durable type
    /// resolves through the ordinary [`Self::resolve`] machinery, refusing
    /// exactly what it refuses.
    fn resolve_well_known_registry_type(
        &mut self,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, IdentityMintError> {
        match value {
            SemanticImportType::AnonymousNominal(key) => self
                .anon_nominals
                .get(key.with_canonical_producer().as_ref())
                .copied()
                .ok_or(IdentityMintError::MissingAnonymous),
            other => self.resolve(other),
        }
    }

    /// Whether `key` (in any producer spelling — entry canonicalization
    /// applies) names a well-known `Option` identity installed on this pool.
    /// The pool analog of `sema.well_known_option_identities.contains(..)`,
    /// which the export funnel's baseline subtraction consults so installed
    /// identities are EXPORTED as produced anonymous nominals, never leaked as
    /// imports (the r6c export-as-produced ruling). A poisoned registry (a
    /// failed install — see `well_known_poisoned`) answers `false` for every
    /// key: as if nothing was installed.
    pub(in crate::sema) fn is_well_known_option_identity(
        &self,
        key: &AnonymousNominalKey<K, M>,
    ) -> bool
    where
        K: Ord,
        M: Ord,
    {
        self.well_known_poisoned.is_none()
            && self
                .well_known_option_identities
                .contains(key.with_canonical_producer().as_ref())
    }

    /// The number of well-known `Option` identities installed on this pool.
    /// A poisoned registry (a failed install — see `well_known_poisoned`)
    /// answers `0`: as if nothing was installed.
    pub(in crate::sema) fn well_known_option_identity_count(&self) -> usize {
        if self.well_known_poisoned.is_some() {
            return 0;
        }
        self.well_known_option_identities.len()
    }

    /// The trusted std `Option` enum minted for a demanded payload type, or
    /// `None` when the payload was never demanded. The pool analog of
    /// `sema.well_known_option_by_payload.get(..)`, the map fallible-intrinsic
    /// resolution (`resolve_option_result_type`) consults. A poisoned registry
    /// (a failed install — see `well_known_poisoned`) answers `None` for every
    /// payload: as if nothing was installed.
    pub(in crate::sema) fn well_known_option_for_payload(&self, payload: Type) -> Option<Type> {
        if self.well_known_poisoned.is_some() {
            return None;
        }
        self.well_known_option_by_payload.get(&payload).copied()
    }

    pub(in crate::sema) fn durable_anonymous_identity(
        &self,
        ty: Type,
    ) -> Option<AnonymousNominalKey<K, M>>
    where
        M: Clone,
    {
        self.anonymous_identities.get(&ty).cloned()
    }

    pub(in crate::sema) fn durable_named_identity(&self, ty: Type) -> Option<K> {
        if let Some(id) = ty.as_struct() {
            return self.struct_identities.get(&id).cloned();
        }
        if let Some(id) = ty.as_enum() {
            return self.enum_identities.get(&id).cloned();
        }
        None
    }

    /// Fail-closed digest-collision gate. Re-presenting the SAME key is
    /// legitimate reuse; a SECOND distinct key hashing to an owned digest is
    /// refused so a name-keyed pool dedup can never collapse two producer-distinct
    /// types onto one id.
    fn guard_anonymous_digest_collision(
        &mut self,
        digest: u128,
        key: &AnonymousNominalKey<K, M>,
    ) -> Result<(), IdentityMintError> {
        match self.anonymous_digest_owners.get(&digest) {
            Some(existing) if existing == key => Ok(()),
            Some(_) => Err(IdentityMintError::AnonymousDigestCollision(digest)),
            None => {
                self.anonymous_digest_owners.insert(digest, key.clone());
                Ok(())
            }
        }
    }

    /// Mint the producer-nominal anonymous struct. Byte-mirror of the epoch's
    /// `find_or_create_anon_struct` naming / metadata: `__anon_struct_{digest}`,
    /// private, source-file-less, copyable iff every field is copyable and no
    /// `__drop` destructor is declared. Method BODIES are not registered here
    /// (they need request-local RIR); only the destructor-derived metadata is
    /// mirrored.
    fn mint_anon_struct(
        &mut self,
        key: &AnonymousNominalKey<K, M>,
        digest: u128,
        fields: &[(Arc<str>, SemanticImportType<K, M>)],
        method_names: &[Arc<str>],
    ) -> Result<Type, IdentityMintError> {
        let name: Arc<str> = Arc::from(format!("__anon_struct_{digest:032x}").as_str());
        let symbol = self.interner.get_or_intern(&name);
        let has_destructor = method_names
            .iter()
            .any(|method| method.as_ref() == ANON_DROP_METHOD);
        let destructor = has_destructor.then(|| Arc::from(format!("{name}.__drop").as_str()));

        // Declare the shell before resolving fields so a field that points back
        // at this nominal resolves the recursive reference to the shell id — the
        // epoch's declare-then-complete discipline.
        let (id, _) = self.type_pool.declare_struct(
            symbol,
            StructDef {
                name: name.clone(),
                fields: Vec::new(),
                is_copy: false,
                is_linear: false,
                destructor: destructor.clone(),
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let ty = Type::new_struct(id);
        // Mirror the epoch's `find_or_create_anon_struct`: the pool-level
        // anonymity registry is the authority for symbol spelling, CFG
        // destructor discovery, and drop glue, and it has to survive the
        // producer-nominal re-mint or those consumers see a generated
        // anonymous type as an ordinary user nominal (RUE-1050, RUE-1193).
        self.type_pool.mark_anonymous_struct(id);
        self.record_anonymous_identity(key.clone(), ty);

        let mut resolved = Vec::with_capacity(fields.len());
        for (field_name, field_ty) in fields {
            let ty = self.resolve_anonymous_shape_type(field_ty)?;
            resolved.push(StructField {
                name: field_name.to_string(),
                ty,
            });
        }
        let is_copy = !has_destructor
            && resolved
                .iter()
                .all(|field| field.ty.is_copy_in_pool(&self.type_pool));
        self.type_pool.complete_declared_struct(
            id,
            StructDef {
                name,
                fields: resolved,
                is_copy,
                is_linear: false,
                destructor,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        Ok(ty)
    }

    /// Mint the producer-nominal anonymous enum. Byte-mirror of the epoch's
    /// `find_or_create_anon_enum` naming: `__anon_enum_{digest} { A(T), B }` where
    /// the payload types render through the same `safe_name_with_pool` the epoch
    /// spells them with, and the name never decides identity.
    fn mint_anon_enum(
        &mut self,
        key: &AnonymousNominalKey<K, M>,
        digest: u128,
        variants: &[(Arc<str>, Vec<SemanticImportType<K, M>>)],
    ) -> Result<Type, IdentityMintError> {
        let variant_names: Arc<[Arc<str>]> = variants
            .iter()
            .map(|(name, _)| Arc::from(name.as_ref()))
            .collect();
        let mut variant_payloads = Vec::with_capacity(variants.len());
        for (_, payload) in variants {
            let mut resolved = Vec::with_capacity(payload.len());
            for ty in payload {
                resolved.push(self.resolve_anonymous_shape_type(ty)?);
            }
            variant_payloads.push(resolved);
        }

        let mut name = format!("__anon_enum_{digest:032x} {{ ");
        for (i, vname) in variant_names.iter().enumerate() {
            if i > 0 {
                name.push_str(", ");
            }
            name.push_str(vname);
            let payload = &variant_payloads[i];
            if !payload.is_empty() {
                name.push('(');
                for (j, ty) in payload.iter().enumerate() {
                    if j > 0 {
                        name.push_str(", ");
                    }
                    name.push_str(&ty.safe_name_with_pool(Some(&self.type_pool)));
                }
                name.push(')');
            }
        }
        name.push_str(" }");

        let symbol = self.interner.get_or_intern(&name);
        let (id, _) = self.type_pool.register_enum(
            symbol,
            EnumDef {
                name: Arc::from(name.as_str()),
                variants: variant_names,
                variant_payloads,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        // See `mint_anon_struct` (RUE-1050, RUE-1193).
        self.type_pool.mark_anonymous_enum(id);
        let ty = Type::new_enum(id);
        self.record_anonymous_identity(key.clone(), ty);
        Ok(ty)
    }

    fn resolve_anonymous_shape_type(
        &mut self,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, IdentityMintError> {
        match value {
            SemanticImportType::AnonymousNominal(key) => self.find_or_create_anon(key),
            SemanticImportType::Array { element, len } => {
                let element = self.resolve_anonymous_shape_type(element)?;
                self.type_pool
                    .try_intern_array(element, *len)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)
            }
            SemanticImportType::PtrConst(pointee) => {
                let pointee = self.resolve_anonymous_shape_type(pointee)?;
                self.type_pool
                    .try_intern_ptr_const(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)
            }
            SemanticImportType::PtrMut(pointee) => {
                let pointee = self.resolve_anonymous_shape_type(pointee)?;
                self.type_pool
                    .try_intern_ptr_mut(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)
            }
            _ => self.resolve_provider_type(value),
        }
    }
}

pub(in crate::sema) fn semantic_import_type_mentions_generic_parameter<K, M>(
    value: &SemanticImportType<K, M>,
) -> bool {
    fn arguments<K, M>(value: &crate::CanonicalArguments<K, M>) -> bool {
        value.types.iter().any(type_instance)
            || value.values.iter().any(|value| match value {
                crate::CanonicalArgumentValue::Type(value) => type_instance(value),
                crate::CanonicalArgumentValue::Function(value) => function_instance(value),
                crate::CanonicalArgumentValue::Integer(_)
                | crate::CanonicalArgumentValue::Bool(_)
                | crate::CanonicalArgumentValue::Unit
                | crate::CanonicalArgumentValue::String(_) => false,
            })
    }

    fn anonymous<K, M>(value: &crate::AnonymousNominalKey<K, M>) -> bool {
        let producer = match &value.producer {
            crate::StableProducerId::Definition(_) => false,
            crate::StableProducerId::Function(value) => function_instance(value),
        };
        producer || arguments(&value.arguments)
    }

    fn function_instance<K, M>(value: &crate::FunctionInstanceKey<K, M>) -> bool {
        match value {
            crate::FunctionInstanceKey::Definition(_) => false,
            crate::FunctionInstanceKey::Specialization {
                base,
                arguments: args,
            } => function_instance(base) || arguments(args),
            crate::FunctionInstanceKey::AnonymousMember { owner, .. }
            | crate::FunctionInstanceKey::DropGlue(owner) => type_instance(owner),
        }
    }

    fn type_instance<K, M>(value: &crate::TypeInstanceKey<K, M>) -> bool {
        match value {
            crate::TypeInstanceKey::GenericParameter(_) => true,
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(value)) => {
                anonymous(value)
            }
            crate::TypeInstanceKey::Array { element, .. }
            | crate::TypeInstanceKey::Slice { element, .. }
            | crate::TypeInstanceKey::PtrConst(element)
            | crate::TypeInstanceKey::PtrMut(element) => type_instance(element),
            crate::TypeInstanceKey::I8
            | crate::TypeInstanceKey::I16
            | crate::TypeInstanceKey::I32
            | crate::TypeInstanceKey::I64
            | crate::TypeInstanceKey::U8
            | crate::TypeInstanceKey::U16
            | crate::TypeInstanceKey::U32
            | crate::TypeInstanceKey::U64
            | crate::TypeInstanceKey::Bool
            | crate::TypeInstanceKey::Unit
            | crate::TypeInstanceKey::Never
            | crate::TypeInstanceKey::ComptimeType
            | crate::TypeInstanceKey::BuiltinNominal { .. }
            | crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Builtin { .. })
            | crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(_))
            | crate::TypeInstanceKey::Module(_) => false,
        }
    }

    match value {
        SemanticImportType::GenericParameter(_) => true,
        SemanticImportType::AnonymousNominal(value) => anonymous(value),
        SemanticImportType::Array { element, .. }
        | SemanticImportType::PtrConst(element)
        | SemanticImportType::PtrMut(element)
        | SemanticImportType::Slice { element, .. } => {
            semantic_import_type_mentions_generic_parameter(element)
        }
        SemanticImportType::I8
        | SemanticImportType::I16
        | SemanticImportType::I32
        | SemanticImportType::I64
        | SemanticImportType::U8
        | SemanticImportType::U16
        | SemanticImportType::U32
        | SemanticImportType::U64
        | SemanticImportType::Bool
        | SemanticImportType::Unit
        | SemanticImportType::Never
        | SemanticImportType::ComptimeType
        | SemanticImportType::BuiltinNominal { .. }
        | SemanticImportType::Nominal(_)
        | SemanticImportType::Module(_) => false,
    }
}

/// One durable parameter of a callable signature: the r5a durable parameter
/// vocabulary (`DurableSemanticParameter { name, ty, mode, is_comptime }`)
/// projected into rue-air's durable type algebra. `name` is carried durably
/// (r5a) so the pool assembles a `ParamRange` whose names match the epoch's
/// without re-consulting a declaration shell.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableSignatureParameter<K, M> {
    pub name: Arc<str>,
    pub ty: SemanticImportType<K, M>,
    pub mode: SemanticParameterMode,
    pub is_comptime: bool,
}

/// Exact parser-structured parameter and result types retained by the
/// canonical declaration-signature projection. Dependent callable types
/// cannot always be reconstructed from reduced durable placeholders, so body
/// materialization carries the shared dense syntax beside the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableCallableTypeSyntax {
    pub syntax: rue_rir::RirTypeSyntaxArena<Arc<str>>,
    pub parameters: Arc<[rue_rir::RirTypeSyntaxRef]>,
    pub result: rue_rir::RirTypeSyntaxRef,
}

/// The durable signature of a free function, sufficient to assemble the
/// signature-derived subset of a [`FunctionInfo`]. Executable-body sources may
/// also carry the canonical exact type fragments needed to resolve dependent
/// placeholders; identity-only sources leave that field absent. `is_generic`
/// is *not* a field — it is the derived predicate "any parameter is comptime",
/// exactly the invariant the epoch enforces between its shell and RIR
/// (`binding_manifest.rs`: `shell.is_generic == params.any(is_comptime)`).
#[derive(Debug, Clone)]
pub struct DurableFunction<K, M> {
    pub parameters: Arc<[DurableSignatureParameter<K, M>]>,
    pub result: SemanticImportType<K, M>,
    pub type_syntax: Option<DurableCallableTypeSyntax>,
    pub is_public: bool,
    pub is_unchecked: bool,
    pub is_extern: bool,
}

/// The durable signature of a method. The `receiver` is a durable type (the
/// concrete owning nominal — the epoch's `Self` is pre-resolved at export, so
/// no `Self` substitution is needed here) resolved through the same 2a nominal
/// machinery as the parameters.
#[derive(Debug, Clone)]
pub struct DurableMethod<K, M> {
    pub receiver: SemanticImportType<K, M>,
    pub parameters: Arc<[DurableSignatureParameter<K, M>]>,
    pub result: SemanticImportType<K, M>,
    pub type_syntax: Option<DurableCallableTypeSyntax>,
    pub has_self: bool,
    pub self_mode: SemanticParameterMode,
    pub is_accessor: bool,
}

/// The durable callable vocabulary the pool consults to mint callable
/// identities. Keys are namespace-disjoint from nominal
/// keys in the durable universe (`StableDefinitionKey` encodes namespace+kind),
/// so a key names at most one of a nominal, a function, or a method.
pub trait DurableCallableSource<K, M> {
    /// The durable signature for a free-function key, or `None` if the key names
    /// no function in the durable universe.
    fn function(&self, key: &K) -> Option<DurableFunction<K, M>>;
    /// The durable signature for a method key, or `None` if the key names no
    /// method in the durable universe.
    fn method(&self, key: &K) -> Option<DurableMethod<K, M>>;

    /// Whether dependent callable types are being materialized for an
    /// executable body-analysis host. Such a host carries the exact syntax
    /// beside its durable function or method and uses `COMPTIME_TYPE` only as
    /// the local placeholder that specialization resolves before execution.
    /// Identity-only consumers keep the default refusal so a generic parameter
    /// can never be mistaken for an independently materialized type.
    fn uses_deferred_body_type_placeholders(&self) -> bool {
        false
    }
}

/// The request/RIR-carried facts a caller supplies to assemble a
/// [`FunctionInfo`]: everything the durable signature does *not* carry.
///
/// These are the 2c / request-local seam. The durable facts stop at the
/// signature (spans and RIR handles belong to an exact semantic request —
/// `semantic_import.rs`); `is_c_export` is read from the current RIR by the
/// epoch itself (`binding_manifest.rs`), never threaded through the durable
/// shell; `return_type_syntax` is an owner-local structured RIR reference consumed only by
/// generic specialization / export; the `@allow` flags come from RIR
/// directives. Supplying them here (rather than fabricating them) is the honest
/// boundary — the pool refuses to invent a fact the durable universe lacks.
#[derive(Debug, Clone, Copy)]
pub(in crate::sema) struct FunctionIdentityHandle {
    pub body: InstRef,
    pub declaration: InstRef,
    pub span: Span,
    pub return_type_syntax: rue_rir::RirTypeSyntaxRef,
    pub returns_type: bool,
    pub is_extern: bool,
    pub is_c_export: bool,
    pub allow_unused_function: bool,
    pub allow_unused_variable: bool,
    pub allow_unreachable_code: bool,
    pub file_id: FileId,
}

/// The request/RIR-carried facts a caller supplies to assemble a [`MethodInfo`].
/// `self_is_mut` is body-local (call sites ignore it); `body` and `span` are
/// request-carried. Receiver mode belongs to the durable signature.
#[derive(Debug, Clone, Copy)]
pub(in crate::sema) struct MethodIdentityHandle {
    pub body: InstRef,
    pub span: Span,
    pub self_is_mut: bool,
    /// Whether the declaration is a `-> borrow T` accessor (ADR-0062);
    /// carried from the RIR `FnDecl` like `self_is_mut`.
    pub returns_borrow: bool,
}

impl<K, M, S> BodyIdentityPool<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    S: DurableNominalSource<K, M> + DurableAnonymousSource<K, M> + DurableCallableSource<K, M>,
{
    #[cfg(test)]
    pub(in crate::sema) fn resolve_function_call(
        &mut self,
        key: &K,
        returns_type: bool,
        file_id: FileId,
    ) -> Result<super::info::FunctionCallInfo, IdentityMintError> {
        let signature = self.function_signature(key)?;
        Ok(super::info::FunctionCallInfo {
            params: signature.params,
            return_type: signature.return_type,
            returns_type,
            is_generic: signature.is_generic,
            is_pub: signature.is_pub,
            is_unchecked: signature.is_unchecked,
            is_extern: signature.is_extern,
            file_id,
        })
    }

    /// Assemble a [`FunctionInfo`] for a durable function key, combining the
    /// minted signature-derived subset with the caller-provided request/RIR
    /// facts. Mints the signature on first consult; deduplicates thereafter.
    pub(in crate::sema) fn resolve_function(
        &mut self,
        key: &K,
        handle: FunctionIdentityHandle,
    ) -> Result<FunctionInfo, IdentityMintError> {
        let signature = self.function_signature(key)?;
        Ok(FunctionInfo {
            params: signature.params,
            return_type: signature.return_type,
            return_type_syntax: handle.return_type_syntax,
            returns_type: handle.returns_type,
            body: handle.body,
            declaration: handle.declaration,
            span: handle.span,
            is_generic: signature.is_generic,
            is_pub: signature.is_pub,
            is_unchecked: signature.is_unchecked,
            is_extern: handle.is_extern,
            is_c_export: handle.is_c_export,
            allow_unused_function: handle.allow_unused_function,
            allow_unused_variable: handle.allow_unused_variable,
            allow_unreachable_code: handle.allow_unreachable_code,
            file_id: handle.file_id,
        })
    }

    /// Assemble call information from a durable signature the caller already
    /// read from this pool's source. Provider body hosts need that payload for
    /// request-local metadata as well, so accepting it here avoids querying and
    /// cloning the same body-local source value a second time.
    pub(in crate::sema) fn resolve_function_call_from(
        &mut self,
        key: &K,
        function: &DurableFunction<K, M>,
        returns_type: bool,
        file_id: FileId,
    ) -> Result<super::info::FunctionCallInfo, IdentityMintError> {
        let signature = self.function_signature_from(key, function)?;
        Ok(super::info::FunctionCallInfo {
            params: signature.params,
            return_type: signature.return_type,
            returns_type,
            is_generic: signature.is_generic,
            is_pub: signature.is_pub,
            is_unchecked: signature.is_unchecked,
            is_extern: signature.is_extern,
            file_id,
        })
    }

    /// Assemble a [`MethodInfo`] for a durable method key, combining the minted
    /// signature-derived subset with the caller-provided request/RIR facts.
    pub(in crate::sema) fn resolve_method(
        &mut self,
        key: &K,
        handle: MethodIdentityHandle,
    ) -> Result<MethodInfo, IdentityMintError> {
        let signature = self.method_signature(key)?;
        Ok(MethodInfo {
            struct_type: signature.receiver,
            has_self: signature.has_self,
            self_mode: signature.self_mode,
            self_is_mut: handle.self_is_mut,
            params: signature.params,
            return_type: signature.return_type,
            body: handle.body,
            span: handle.span,
            returns_borrow: handle.returns_borrow,
        })
    }

    pub(in crate::sema) fn resolve_method_call(
        &mut self,
        key: &K,
    ) -> Result<super::info::MethodCallInfo, IdentityMintError> {
        let signature = self.method_signature(key)?;
        Ok(super::info::MethodCallInfo {
            struct_type: signature.receiver,
            has_self: signature.has_self,
            self_mode: signature.self_mode,
            params: signature.params,
            return_type: signature.return_type,
            returns_borrow: signature.returns_borrow,
        })
    }

    /// Mint (once) or dedup the signature-derived subset of a function.
    fn function_signature(&mut self, key: &K) -> Result<CallableSignature, IdentityMintError> {
        if let Some(err) = self.callable_poisoned.get(key) {
            return Err(err.clone());
        }
        if let Some(&signature) = self.function_sigs.get(key) {
            return Ok(signature);
        }

        let function = self
            .source
            .function(key)
            .ok_or(IdentityMintError::MissingCallable)?;
        self.build_function_signature(key, &function)
    }

    fn function_signature_from(
        &mut self,
        key: &K,
        function: &DurableFunction<K, M>,
    ) -> Result<CallableSignature, IdentityMintError> {
        if let Some(err) = self.callable_poisoned.get(key) {
            return Err(err.clone());
        }
        if let Some(&signature) = self.function_sigs.get(key) {
            return Ok(signature);
        }
        self.build_function_signature(key, function)
    }

    fn build_function_signature(
        &mut self,
        key: &K,
        function: &DurableFunction<K, M>,
    ) -> Result<CallableSignature, IdentityMintError> {
        let is_generic = function
            .parameters
            .iter()
            .any(|parameter| parameter.is_comptime);
        // Parameters intern first (mirroring the epoch, which allocs the arena
        // before resolving the return type); a failure at either step poisons
        // the key so the append-only arena's orphaned parameters stay
        // unreachable and the repeat consult re-errors.
        let params = self.intern_params(key, &function.parameters)?;
        let return_type = self.resolve_callable_type(key, &function.result)?;

        let signature = CallableSignature {
            params,
            return_type,
            is_generic,
            is_pub: function.is_public,
            is_unchecked: function.is_unchecked,
            is_extern: function.is_extern,
        };
        self.function_sigs.insert(key.clone(), signature);
        Ok(signature)
    }

    /// Mint (once) or dedup the signature-derived subset of a method.
    fn method_signature(&mut self, key: &K) -> Result<MethodSignature, IdentityMintError> {
        if let Some(err) = self.callable_poisoned.get(key) {
            return Err(err.clone());
        }
        if let Some(&signature) = self.method_sigs.get(key) {
            return Ok(signature);
        }

        let DurableMethod {
            receiver,
            parameters,
            result,
            type_syntax: _,
            has_self,
            self_mode,
            is_accessor,
        } = self
            .source
            .method(key)
            .ok_or(IdentityMintError::MissingCallable)?;

        let receiver = self.resolve_callable_type(key, &receiver)?;
        let params = self.intern_params(key, &parameters)?;
        let return_type = self.resolve_callable_type(key, &result)?;

        let signature = MethodSignature {
            receiver,
            has_self,
            self_mode: match self_mode {
                SemanticParameterMode::Value => RirParamMode::Normal,
                SemanticParameterMode::Borrow => RirParamMode::Borrow,
                SemanticParameterMode::Inout => RirParamMode::Inout,
            },
            params,
            return_type,
            returns_borrow: is_accessor,
        };
        self.method_sigs.insert(key.clone(), signature);
        Ok(signature)
    }

    /// Resolve one durable type inside a callable signature, poisoning the
    /// callable key on failure (so a partial mint never re-runs).
    fn resolve_callable_type(
        &mut self,
        key: &K,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, IdentityMintError> {
        if self.source.uses_deferred_body_type_placeholders()
            && semantic_import_type_mentions_generic_parameter(value)
        {
            return Ok(Type::COMPTIME_TYPE);
        }
        let resolved = match value {
            SemanticImportType::Array { element, len } => {
                let element = self.resolve_callable_type(key, element)?;
                self.type_pool
                    .try_intern_array(element, *len)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)
            }
            SemanticImportType::PtrConst(pointee) => {
                let pointee = self.resolve_callable_type(key, pointee)?;
                self.type_pool
                    .try_intern_ptr_const(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)
            }
            SemanticImportType::PtrMut(pointee) => {
                let pointee = self.resolve_callable_type(key, pointee)?;
                self.type_pool
                    .try_intern_ptr_mut(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)
            }
            SemanticImportType::AnonymousNominal(identity) => {
                let canonical = identity.with_canonical_producer();
                self.find_or_create_anon(canonical.as_ref())
            }
            _ => self.resolve_provider_type(value),
        };
        resolved.inspect_err(|err| {
            self.callable_poisoned.insert(key.clone(), (*err).clone());
        })
    }

    /// Intern a durable parameter vocabulary into the pool's own arena, returning
    /// an internally-consistent [`ParamRange`]. Types resolve through the 2a
    /// nominal machinery (`resolve`); names intern into the pool's interner;
    /// modes map to the RIR mode the arena stores; comptime flags copy through.
    fn intern_params(
        &mut self,
        key: &K,
        parameters: &[DurableSignatureParameter<K, M>],
    ) -> Result<ParamRange, IdentityMintError> {
        let mut names = Vec::with_capacity(parameters.len());
        let mut types = Vec::with_capacity(parameters.len());
        let mut modes = Vec::with_capacity(parameters.len());
        let mut comptime = Vec::with_capacity(parameters.len());
        for parameter in parameters {
            // Resolve the type first: on failure the arena is untouched (alloc is
            // the final step), so only the callable key is poisoned.
            let ty = self.resolve_callable_type(key, &parameter.ty)?;
            names.push(self.interner.get_or_intern(parameter.name.as_ref()));
            types.push(ty);
            modes.push(match parameter.mode {
                SemanticParameterMode::Value => RirParamMode::Normal,
                SemanticParameterMode::Borrow => RirParamMode::Borrow,
                SemanticParameterMode::Inout => RirParamMode::Inout,
            });
            comptime.push(parameter.is_comptime);
        }
        Ok(self.param_arena.alloc(names, types, modes, comptime))
    }
}

// ----- Const identity family ------------------------------------------------

/// Declaration-level durable facts for one value constant.
///
/// The record deliberately excludes its [`Span`]: that is a request-local RIR
/// locator supplied by [`ConstIdentityHandle`], just as callable bodies and
/// declaration handles are supplied separately from durable signatures.
/// Module bindings are not represented here. Although their durable record
/// carries a target module key, minting the epoch-local module [`Type`] needs the
/// module-registry arm the pool still refuses; a source must return `None`
/// instead of approximating one as a value const.
#[derive(Debug, Clone)]
pub struct DurableConst<K, M> {
    pub is_public: bool,
    pub ty: SemanticImportType<K, M>,
    pub value: SemanticImportConstValue<K, M>,
}

/// The durable const vocabulary consulted by the body identity pool.
///
/// `constant` exposes only constants with declaration-level durable type/value
/// truth. `function_name` relocates a function-valued const's durable callable
/// key to its source name; the pool interns that name in its own symbol space,
/// exactly as it interns durable parameter names. A missing `constant` is an
/// honest, retryable `MissingConst` refusal (matching `MissingCallable`); a
/// missing `function_name` occurs after const assembly has begun and poisons
/// that const mint as `MissingConstCallable`.
pub trait DurableConstSource<K, M> {
    fn constant(&self, key: &K) -> Option<DurableConst<K, M>>;
    fn function_name(&self, key: &K) -> Option<Arc<str>>;
}

/// The durable-derived subset of [`ConstInfo`], cached by const key.
#[derive(Clone, Copy)]
struct ConstIdentity {
    is_pub: bool,
    ty: Type,
    value: ConstValue,
}

/// The request/RIR-carried portion of a [`ConstInfo`].
#[derive(Debug, Clone, Copy)]
pub(in crate::sema) struct ConstIdentityHandle {
    pub span: Span,
}

impl<K, M, S> BodyIdentityPool<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Eq + Hash,
    S: DurableNominalSource<K, M> + DurableConstSource<K, M>,
{
    /// Assemble a [`ConstInfo`] from a durable value-const record and the exact
    /// current-RIR declaration handle. The durable subset is minted once and
    /// deduplicated; the request-local span is applied on every assembly.
    pub(in crate::sema) fn resolve_const(
        &mut self,
        key: &K,
        handle: ConstIdentityHandle,
    ) -> Result<ConstInfo, IdentityMintError> {
        let identity = self.const_identity(key)?;
        Ok(ConstInfo {
            is_pub: identity.is_pub,
            ty: identity.ty,
            value: identity.value,
            span: handle.span,
        })
    }

    fn const_identity(&mut self, key: &K) -> Result<ConstIdentity, IdentityMintError> {
        if let Some(err) = self.const_poisoned.get(key) {
            return Err(err.clone());
        }
        if let Some(&identity) = self.const_values.get(key) {
            return Ok(identity);
        }

        let DurableConst {
            is_public,
            ty,
            value,
        } = self
            .source
            .constant(key)
            .ok_or(IdentityMintError::MissingConst)?;

        // Resolve the declared type before the value, matching declaration
        // assembly. Recursive nominal graphs still use `resolve`'s
        // declare-then-complete path; a const itself needs no predeclared shell
        // because its durable value is already fully evaluated.
        let ty = self.resolve_const_type(key, &ty)?;
        let value = self.resolve_const_value(key, &value)?;
        let identity = ConstIdentity {
            is_pub: is_public,
            ty,
            value,
        };
        self.const_values.insert(key.clone(), identity);
        Ok(identity)
    }

    fn resolve_const_type(
        &mut self,
        key: &K,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, IdentityMintError> {
        self.resolve(value).inspect_err(|err| {
            self.const_poisoned.insert(key.clone(), (*err).clone());
        })
    }

    fn resolve_const_value(
        &mut self,
        key: &K,
        value: &SemanticImportConstValue<K, M>,
    ) -> Result<ConstValue, IdentityMintError> {
        use SemanticImportConstValue as V;
        Ok(match value {
            V::Integer(value) => ConstValue::Integer(*value),
            V::Bool(value) => ConstValue::Bool(*value),
            V::Type(value) => ConstValue::Type(self.resolve_const_type(key, value)?),
            V::Function(function) => {
                let Some(name) = self.source.function_name(function) else {
                    let err = IdentityMintError::MissingConstCallable;
                    self.const_poisoned.insert(key.clone(), err.clone());
                    return Err(err);
                };
                ConstValue::Function(self.interner.get_or_intern(name.as_ref()))
            }
            V::Unit => ConstValue::Unit,
            V::String(value) => ConstValue::String(self.interner.get_or_intern(value.as_ref())),
        })
    }
}

/// The body-scoped RIR answer surface for the three endpoint ops
/// `provider_body_host.rs` consumes — `first_free_function`,
/// `named_method_declaration`, and `destructor`. It additionally owns the
/// pool-side const declaration handle.
///
/// # The shared-`Rir` input, not durable state
///
/// Every answer is an [`InstRef`] into the whole-program `rir: &Rir` this index
/// was built from — a body-query *input*, never durable. The design checkpoint
/// fixed this: "Projection is a metadata join and must never inspect RIR"; the
/// RIR index is the *other* leg, the query-input side that resolves durable
/// declaration identities into the current arena's instruction handles. The
/// returned handles are valid only against that exact `Rir`, exactly as
/// [`RirDeclarationIndex`]'s are, so a caller composes them with the same `Rir`
/// its [`FunctionIdentityHandle`] / [`MethodIdentityHandle`] index into.
///
/// # New-vs-reuse: a thin façade over the body-independent production index
///
/// [`RirDeclarationIndex`] is already body-independent — `RirDeclarationIndex::
/// new(rir)` is built from the shared `Rir` alone, its `InstRef`s locate that
/// arena, and it already answers `first_free_function` and `destructors`
/// verbatim (they are the tables the epoch host consults directly). So this
/// index **re-uses it wholesale** for those two ops rather than
/// duplicating the arena walk.
///
/// The one op the production index does not expose as a keyed point lookup is
/// `named_method_declaration`: it needs a map keyed by a pool-minted
/// [`StructId`], which the pool-free RIR index cannot hold. But that
/// `StructId` is only an *intermediate* the epoch computes from `struct_by_file_
/// name(file, type_name)` — a bijection (duplicate `(file, name)` is E0418), so
/// its durable-available preimage `(owner_file, owner_type_name)` is an exact
/// substitute key. This index therefore re-keys the owner→method edges
/// [`RirDeclarationIndex`] already collected (retained on `shell_declarations`'
/// `named_method_owner`) into a `(FileId, type_name, method_name) → InstRef`
/// point map — the sole additive surface. No keying input is pool-minted; every
/// key is RIR-derivable from the shared `Rir`.
///
/// The const map indexes every arena `ConstDecl`, while the epoch's semantic
/// authority is its shell-bound candidate set. Those sets coincide today. If a
/// future synthetic arena contains a stray declaration, its handle remains
/// inert: it contributes only a span and cannot assemble a [`ConstInfo`] without
/// the independent durable-record join succeeding.
///
/// The index is request-local and is built once by [`BodyRirBundle`]. It is
/// intentionally not a query value or a durable artifact.
#[derive(Debug)]
pub(in crate::sema) struct BodyRirIndex {
    /// Production's body-independent declaration index, re-used verbatim for
    /// `first_free_function` and `destructor`.
    declarations: RirDeclarationIndex,
    /// The one additive keyed surface: named-method declarations keyed by the
    /// durable-available `(owner_file, owner_type_name, method_name)` preimage
    /// of the epoch's `(StructId, method_name)` key.
    named_methods_by_owner: AHashMap<(FileId, Spur, Spur), InstRef>,
    /// Const declarations keyed by the exact storage preimage used by the
    /// epoch's `const_resolutions`: `(declaring_file, source_name)`.
    const_declarations: AHashMap<(FileId, Spur), InstRef>,
}

/// One request-local RIR universe shared by every provider fact facade for a
/// body. The validated RIR, its matching semantic interner, and the derived
/// declaration index have one owner so endpoint, call, aggregate, type, and
/// inference facts cannot accidentally construct independent compact-identity
/// authorities.
#[derive(Debug)]
pub struct BodyRirBundle {
    rir: ValidatedRir,
    rir_interner: Rc<ThreadedRodeo>,
    rir_index: Arc<BodyRirIndex>,
}

/// Structural work attribution for building one request-local body RIR index.
/// `duration_ns` is charged by the compiler-owned contiguous lowering clock;
/// the values contain no request-local instruction identities.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BodyRirIndexAttribution {
    pub duration_ns: u64,
    pub declaration_index: super::RirDeclarationIndexWork,
    pub shell_declarations_visited: u64,
    pub named_methods_indexed: u64,
    pub const_declarations_indexed: u64,
}

impl BodyRirBundle {
    pub fn new(rir: ValidatedRir, rir_interner: ThreadedRodeo) -> Self {
        Self::new_with_index_attribution(rir, rir_interner, false).0
    }

    /// Construct the canonical bundle while returning bounded index-build
    /// attribution. The ordinary constructor delegates here so measurement
    /// cannot select a peer construction path.
    pub fn new_with_index_attribution(
        rir: ValidatedRir,
        rir_interner: ThreadedRodeo,
        attribution_enabled: bool,
    ) -> (Self, BodyRirIndexAttribution) {
        let (rir_index, attribution) =
            BodyRirIndex::new_with_attribution(&rir, attribution_enabled);
        (
            Self {
                rir,
                rir_interner: Rc::new(rir_interner),
                rir_index: Arc::new(rir_index),
            },
            attribution,
        )
    }

    /// The one interner authority for this body RIR and its provider fact state.
    pub fn shared_interner(&self) -> Rc<ThreadedRodeo> {
        Rc::clone(&self.rir_interner)
    }

    /// Build a fail-closed compatibility context over this RIR's interner.
    /// Overlay-capable state must come from [`Self::provider_body_state`].
    pub fn provider_identity_context<K, M, S>(&self, source: S) -> ProviderIdentityContext<K, M, S>
    where
        K: Clone + Eq + Hash,
        M: Eq + Hash,
        S: DurableNominalSource<K, M>,
    {
        ProviderIdentityContext::with_interner_mode(source, self.shared_interner(), false)
    }

    pub fn provider_body_state<K, M, S>(&self, source: S) -> ProviderBodyAnalysisState<K, M, S>
    where
        K: Clone + Eq + Hash,
        M: Eq + Hash,
        S: DurableNominalSource<K, M>,
    {
        ProviderBodyAnalysisState::new(source, self.shared_interner())
    }

    pub fn instruction_count(&self) -> usize {
        self.rir.len()
    }

    pub fn source_file_id(&self) -> Option<FileId> {
        let mut files = self
            .rir
            .iter()
            .map(|(_, instruction)| instruction.span.file_id);
        let file = files.next()?;
        files.all(|candidate| candidate == file).then_some(file)
    }

    /// Borrow the one local RIR/index/interner authority for provider fact
    /// facades. The view is cheap and carries no independent identity state.
    pub fn view(&self) -> BodyRirView<'_> {
        BodyRirView {
            rir: &self.rir,
            rir_interner: &self.rir_interner,
            index: self.rir_index.clone(),
        }
    }
}

/// A shared read view over body analysis-local RIR authority. Production callers
/// obtain it from [`BodyRirBundle::view`]; compatibility/test callers may build
/// the same view around an already validated RIR and its matching interner.
#[derive(Debug, Clone)]
pub struct BodyRirView<'a> {
    rir: &'a Rir,
    rir_interner: &'a ThreadedRodeo,
    index: Arc<BodyRirIndex>,
}

impl<'a> BodyRirView<'a> {
    /// Construct the same shared view for an existing RIR/interner pair. The
    /// index is built by the view once, never independently by endpoint/call
    /// facades.
    pub fn from_parts(rir: &'a Rir, rir_interner: &'a ThreadedRodeo) -> Self {
        Self {
            rir,
            rir_interner,
            index: Arc::new(BodyRirIndex::new(rir)),
        }
    }

    pub(in crate::sema) fn rir(&self) -> &Rir {
        self.rir
    }

    pub(in crate::sema) fn rir_interner(&self) -> &ThreadedRodeo {
        self.rir_interner
    }

    pub(in crate::sema) fn rir_index(&self) -> &BodyRirIndex {
        &self.index
    }
}

impl BodyRirIndex {
    /// Build the index from the request-local body `Rir`. Constructs one
    /// [`RirDeclarationIndex`] and derives the named-method point map from the
    /// owner edges it already classified — no second arena walk of the method
    /// universe, no durable metadata, no pool.
    pub(in crate::sema) fn new(rir: &Rir) -> Self {
        Self::new_with_attribution(rir, false).0
    }

    fn new_with_attribution(
        rir: &Rir,
        attribution_enabled: bool,
    ) -> (Self, BodyRirIndexAttribution) {
        let declarations = RirDeclarationIndex::new(rir);
        let declaration_index = declarations.work();
        let mut named_methods_by_owner = AHashMap::new();
        let mut const_declarations = AHashMap::new();
        let mut attribution = BodyRirIndexAttribution {
            declaration_index,
            ..BodyRirIndexAttribution::default()
        };
        for shell in declarations.shell_declarations() {
            if attribution_enabled {
                attribution.shell_declarations_visited += 1;
            }
            // `named_method_owner` is `Some` exactly for named methods (free
            // functions, nominals, consts, and destructors carry `None`); the
            // owner is the enclosing struct's source-name symbol. A named
            // method's own `span.file_id` is its enclosing struct's file (it is
            // lexically inline), so it is the same file the epoch keys by via
            // `struct_by_file_name(struct_span.file_id, type_name)`.
            let inst = rir.get(shell.declaration);
            if let InstData::ConstDecl { name, .. } = inst.data {
                // First edge wins, matching the bound const candidate index.
                // Duplicate `(file, name)` declarations are rejected before a
                // frozen epoch can expose a `ConstInfo`.
                const_declarations
                    .entry((inst.span.file_id, name))
                    .or_insert_with(|| {
                        if attribution_enabled {
                            attribution.const_declarations_indexed += 1;
                        }
                        shell.declaration
                    });
            }
            let Some(owner) = shell.named_method_owner else {
                continue;
            };
            if let InstData::FnDecl { name, .. } = inst.data {
                // First edge wins, mirroring `RirDeclarationIndex`'s
                // `named_method_owners.or_insert` and the epoch's E0418 rejection
                // of a duplicate `(struct, method)`.
                named_methods_by_owner
                    .entry((inst.span.file_id, owner, name))
                    .or_insert_with(|| {
                        if attribution_enabled {
                            attribution.named_methods_indexed += 1;
                        }
                        shell.declaration
                    });
            }
        }
        (
            Self {
                declarations,
                named_methods_by_owner,
                const_declarations,
            },
            attribution,
        )
    }

    /// The first free-function RIR declaration for `(source, file)`. The exact
    /// answer the epoch host's direct fact implementation returns.
    pub(in crate::sema) fn first_free_function(
        &self,
        source: Spur,
        file_id: FileId,
    ) -> Option<InstRef> {
        self.declarations.first_free_function(source, Some(file_id))
    }

    /// The named-method RIR declaration for a named struct owner, keyed by the
    /// durable-available `(owner_file, owner_type_name, method_name)` preimage of
    /// the epoch's `(StructId, method_name)` key. Equal to
    /// the epoch host's direct fact implementation under the
    /// `struct_by_file_name` bijection.
    pub(in crate::sema) fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: Spur,
        method_name: Spur,
    ) -> Option<InstRef> {
        self.named_methods_by_owner
            .get(&(owner_file, owner_type_name, method_name))
            .copied()
    }

    /// The const declaration for the exact epoch storage key
    /// `(declaring_file, source_name)`.
    pub(in crate::sema) fn const_declaration(
        &self,
        declaring_file: FileId,
        source_name: Spur,
    ) -> Option<InstRef> {
        self.const_declarations
            .get(&(declaring_file, source_name))
            .copied()
    }

    /// The destructor declaration record for `(file, type_name)`. The exact
    /// scan the epoch host performs.
    pub(in crate::sema) fn destructor(
        &self,
        file: u32,
        type_name: Spur,
    ) -> Option<RirDestructorDeclaration> {
        self.declarations
            .destructors()
            .iter()
            .find(|record| record.span.file_id.index() == file && record.type_name == type_name)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic_identity::{AnonymousNominalKind, CanonicalArguments, StableProducerId};

    type Key = u32;
    type Module = Arc<str>;
    type DType = SemanticImportType<Key, Module>;

    type AnonKey = AnonymousNominalKey<Key, Module>;

    #[test]
    fn provider_method_registry_is_atomic_and_anonymous_first() {
        let symbols = ThreadedRodeo::new();
        let method = symbols.get_or_intern("shift");
        let compact = (StructId(7), method);
        let owner = (
            FileId::new(3),
            Arc::<str>::from("Widget"),
            Arc::<str>::from("shift"),
        );
        let info = |body| MethodInfo {
            struct_type: Type::new_struct(compact.0),
            has_self: true,
            self_mode: RirParamMode::Borrow,
            self_is_mut: false,
            params: ParamRange::EMPTY,
            return_type: Type::I32,
            body: InstRef::from_raw(body),
            span: Span::with_file(owner.0, 1, 2),
            returns_borrow: false,
        };
        let named = info(10);
        let anonymous = info(11);
        let conflicting = info(12);
        let mut registry = ProviderMethodRegistry::default();

        assert!(registry.register_named(compact, owner.clone(), named));
        assert!(registry.register_anonymous(compact, owner.clone(), anonymous));
        assert_eq!(registry.method(compact).unwrap().body, anonymous.body);
        assert_eq!(
            registry.method_by_owner(&owner).unwrap().body,
            anonymous.body
        );
        assert_eq!(registry.named_by_owner(&owner).unwrap().body, named.body);

        assert!(
            !registry.register_anonymous(compact, owner.clone(), conflicting),
            "a second compact/owner registration cannot split the two indexes"
        );
        assert_eq!(registry.method(compact).unwrap().body, anonymous.body);
        assert_eq!(
            registry.method_by_owner(&owner).unwrap().body,
            anonymous.body
        );
    }

    #[test]
    fn provider_body_state_shares_identity_and_rebases_append_only_authority() {
        fn engine_authority_shape(
            type_pool: &TypeInternPool,
            params: crate::ParamRangeData,
        ) -> (&TypeInternPool, Vec<Type>) {
            (type_pool, params.types().to_vec())
        }

        let interner = Rc::new(ThreadedRodeo::new());
        let widget = interner.get_or_intern("Widget");
        let state = ProviderBodyAnalysisState::new(source([]), Rc::clone(&interner));

        // RIR-local and provider-created names are the same Spur, not merely
        // strings that were translated between two interner universes.
        let context = state.identity_context();
        assert_eq!(context.name_symbol("Widget"), widget);
        assert!(Rc::ptr_eq(&context.interner(), &interner));
        let stable_type_pool = state.type_pool();
        let opposite_order_context = state.identity_context();
        assert!(Rc::ptr_eq(
            &context.interner(),
            &opposite_order_context.interner()
        ));
        assert_eq!(
            context.name_symbol("FacadeFirst"),
            opposite_order_context.name_symbol("FacadeFirst"),
            "facade construction order does not fork symbol identity"
        );

        let base_type = state.with_type_pool_mut(|pool| {
            let (id, _) = pool.register_struct(
                widget,
                StructDef {
                    name: "Widget".into(),
                    fields: Vec::new(),
                    is_copy: true,
                    is_linear: false,
                    destructor: None,
                    is_builtin: false,
                    is_pub: true,
                    file_id: FileId::new(1),
                },
            );
            Type::new_struct(id)
        });
        let base_range =
            state.allocate_params([widget], [base_type], [RirParamMode::Borrow], [false]);
        let base_type_count = state.with_type_pool(|pool| pool.len());
        let base_param_count = state.with_param_arena(ParamArena::total_params);

        state
            .finalize_containment_metadata()
            .expect("exact prerequisite facts have no containment cycle");
        assert!(state.base_sealed());
        assert_eq!(
            state.with_type_pool(|pool| pool.try_type_needs_drop(base_type)),
            Some(false),
            "sealed prerequisite facts remain readable"
        );
        assert_eq!(
            state.with_param_arena(|arena| arena.types(base_range).to_vec()),
            &[base_type],
            "callable ParamRanges remain valid through the engine-facing arena"
        );
        assert_eq!(state.param_data(base_range).types(), &[base_type]);
        assert!(Rc::ptr_eq(&stable_type_pool, &state.type_pool()));
        let (_, engine_param_types) =
            engine_authority_shape(&stable_type_pool, state.param_data(base_range));
        assert_eq!(engine_param_types, &[base_type]);
        assert_eq!(
            stable_type_pool.try_type_needs_drop(base_type),
            Some(false),
            "a preexisting type-pool handle observes the in-place rebase"
        );

        let generated = interner.get_or_intern("Generated");
        let overlay_type = state.with_type_pool_mut(|pool| {
            let (id, _) = pool.register_struct(
                generated,
                StructDef {
                    name: "Generated".into(),
                    fields: Vec::new(),
                    is_copy: true,
                    is_linear: false,
                    destructor: None,
                    is_builtin: false,
                    is_pub: true,
                    file_id: FileId::new(2),
                },
            );
            Type::new_struct(id)
        });
        let overlay_range =
            state.allocate_params([widget], [overlay_type], [RirParamMode::Normal], [true]);
        assert_eq!(
            state.with_type_pool(|pool| pool.shared_len()),
            base_type_count,
            "the sealed type base is not mutated by body-local additions"
        );
        assert!(state.with_type_pool(|pool| pool.local_len()) > 0);
        assert!(stable_type_pool.local_len() > 0);
        assert_eq!(
            state.with_param_arena(|arena| arena.shared_params()),
            base_param_count,
            "the sealed parameter base is not mutated by body-local additions"
        );
        assert_eq!(
            state.with_param_arena(|arena| arena.types(overlay_range).to_vec()),
            &[overlay_type]
        );
        let pending_type = state.with_type_pool_mut(|pool| {
            let (id, _) = pool.declare_struct(
                interner.get_or_intern("Pending"),
                StructDef {
                    name: "Pending".into(),
                    fields: Vec::new(),
                    is_copy: true,
                    is_linear: false,
                    destructor: None,
                    is_builtin: false,
                    is_pub: true,
                    file_id: FileId::new(3),
                },
            );
            Type::new_struct(id)
        });
        assert_eq!(
            state.with_type_pool(|pool| pool.try_type_needs_drop(pending_type)),
            None,
            "unmaterialized required facts fail closed"
        );

        state
            .finalize_containment_metadata()
            .expect("re-finalizing the overlay is explicit and safe");
        assert_eq!(
            state.with_type_pool(|pool| pool.try_type_needs_drop(overlay_type)),
            Some(false)
        );
        assert_eq!(
            state.with_type_pool(|pool| pool.try_type_needs_drop(pending_type)),
            None,
            "re-finalization does not invent facts for an unmaterialized type"
        );
        assert_eq!(
            state.with_param_arena(|arena| arena.types(base_range).to_vec()),
            &[base_type],
            "rebasing preserves the original callable range"
        );
    }

    #[test]
    fn provider_fact_facades_use_the_shared_body_rir_view() {
        let endpoint = include_str!("body_endpoint.rs");
        let endpoint_start = endpoint.find("pub struct ProviderEndpointFacts").unwrap();
        let endpoint_fields = &endpoint[endpoint_start
            ..endpoint[endpoint_start..]
                .find("\n}\n\nimpl<'a, P, S, K, M>")
                .map(|offset| endpoint_start + offset + 2)
                .unwrap()];
        assert!(endpoint_fields.contains("BodyRirView"));
        assert!(!endpoint_fields.contains("&'a Rir"));
        assert!(!endpoint_fields.contains("ThreadedRodeo"));
        assert!(!endpoint_fields.contains("BodyRirIndex"));
        assert!(!endpoint.contains("BodyRirIndex::new"));
        assert!(endpoint.contains("pub fn with_state("));

        let calls = include_str!("call_resolution.rs");
        let calls_start = calls.find("pub struct ProviderCallFacts").unwrap();
        let calls_fields = &calls[calls_start
            ..calls[calls_start..]
                .find("\n}\n\nimpl<'a, P, S, K, M>")
                .map(|offset| calls_start + offset + 2)
                .unwrap()];
        assert!(calls_fields.contains("BodyRirView"));
        assert!(!calls_fields.contains("&'a Rir"));
        assert!(!calls_fields.contains("ThreadedRodeo"));
        assert!(!calls_fields.contains("BodyRirIndex"));
        assert!(!calls.contains("BodyRirIndex::new"));
        assert!(calls.contains("pub fn with_state("));

        let aggregate = include_str!("aggregate_resolution.rs");
        assert!(aggregate.contains("pub fn with_state("));

        let ordinary = include_str!("ordinary_engine.rs");
        assert!(ordinary.contains("fn body_param_data(&self, range: ParamRange)"));
        assert!(!ordinary.contains("fn body_param_arena(&self) -> &ParamArena"));
        assert!(ordinary.contains("fn body_type_pool(&self) -> &TypeInternPool"));
        assert!(calls.contains("let identity = ProviderIdentityContext::new(source)"));
        assert!(endpoint.contains("let identity = ProviderIdentityContext::new(source)"));
    }

    #[test]
    fn bundle_peer_context_is_fail_closed_while_state_is_overlay_capable() {
        let editor = rue_rir::RirEditor::new();
        let validation = rue_rir::RirValidationContext {
            symbol_count: 0,
            source_lengths: &[],
        };
        let rir = ValidatedRir::finish(editor, &validation).unwrap();
        let bundle = BodyRirBundle::new(rir, ThreadedRodeo::new());

        let peer = bundle.provider_identity_context(source([]));
        peer.finalize_containment_metadata().unwrap();
        assert!(peer.pool_mut().is_none());

        let state = bundle.provider_body_state(source([]));
        state.finalize_containment_metadata().unwrap();
        assert!(state.identity_context().pool_mut().is_some());
    }

    #[test]
    fn provider_state_keeps_compatibility_contexts_fail_closed_and_checks_rir_addresses() {
        let legacy = ProviderIdentityContext::new(source([]));
        legacy.finalize_containment_metadata().unwrap();
        assert!(legacy.pool_mut().is_none());

        let interner = Rc::new(ThreadedRodeo::new());
        let state = ProviderBodyAnalysisState::new(source([]), Rc::clone(&interner));
        state.finalize_containment_metadata().unwrap();
        assert!(state.identity_context().pool_mut().is_some());
        assert!(Rc::ptr_eq(&interner, &state.interner()));
        let state_interner = state.interner();
        assert!(std::ptr::eq(
            Rc::as_ref(&interner),
            Rc::as_ref(&state_interner)
        ));

        let rir = Rir::default();
        let matching = BodyRirView::from_parts(&rir, Rc::as_ref(&interner));
        assert_eq!(
            matching.rir_interner() as *const ThreadedRodeo,
            Rc::as_ref(&interner) as *const ThreadedRodeo
        );
        assert!(state.require_rir_authority(&matching));

        let other_interner = ThreadedRodeo::new();
        let mismatched = BodyRirView::from_parts(&rir, &other_interner);
        assert!(!state.require_rir_authority(&mismatched));
    }

    /// A durable nominal + callable source backed by fixed maps, standing in for
    /// r4b's stable-keyed provider.
    struct MapSource {
        nominals: AHashMap<Key, DurableNominal<Key, Module>>,
        nominal_file_ids: AHashMap<Key, FileId>,
        functions: AHashMap<Key, DurableFunction<Key, Module>>,
        function_reads: Rc<Cell<usize>>,
        methods: AHashMap<Key, DurableMethod<Key, Module>>,
        consts: AHashMap<Key, DurableConst<Key, Module>>,
        anonymous_shapes: AHashMap<AnonKey, DurableAnonymousShape<Key, Module>>,
        /// Force a chosen definition relocation for a producer key, so a test can
        /// point two DISTINCT producer keys at one stable-content string (and thus
        /// one digest) and exercise the collision guard without a real 128-bit
        /// hash collision.
        def_component_overrides: AHashMap<Key, String>,
    }

    impl DurableNominalSource<Key, Module> for MapSource {
        fn nominal(&self, key: &Key) -> Option<DurableNominal<Key, Module>> {
            self.nominals.get(key).cloned()
        }

        fn nominal_file_id(&self, key: &Key) -> Option<FileId> {
            self.nominal_file_ids.get(key).copied()
        }
    }

    impl DurableCallableSource<Key, Module> for MapSource {
        fn function(&self, key: &Key) -> Option<DurableFunction<Key, Module>> {
            self.function_reads.set(self.function_reads.get() + 1);
            self.functions.get(key).cloned()
        }

        fn method(&self, key: &Key) -> Option<DurableMethod<Key, Module>> {
            self.methods.get(key).cloned()
        }
    }

    impl DurableConstSource<Key, Module> for MapSource {
        fn constant(&self, key: &Key) -> Option<DurableConst<Key, Module>> {
            self.consts.get(key).cloned()
        }

        fn function_name(&self, key: &Key) -> Option<Arc<str>> {
            self.functions
                .contains_key(key)
                .then(|| Arc::from(format!("fn{key}")))
        }
    }

    impl DurableAnonymousSource<Key, Module> for MapSource {
        fn anonymous_shape(&self, key: &AnonKey) -> Option<DurableAnonymousShape<Key, Module>> {
            self.anonymous_shapes.get(key).cloned()
        }

        fn definition_symbol_component(&self, key: &Key) -> String {
            self.def_component_overrides
                .get(key)
                .cloned()
                .unwrap_or_else(|| format!("D\u{1}{key}"))
        }

        fn module_symbol_component(&self, module: &Module) -> String {
            format!("M\u{1}{module}")
        }
    }

    fn source(nominals: impl IntoIterator<Item = (Key, DurableNominal<Key, Module>)>) -> MapSource {
        MapSource {
            nominals: nominals.into_iter().collect(),
            nominal_file_ids: AHashMap::new(),
            functions: AHashMap::new(),
            function_reads: Rc::new(Cell::new(0)),
            methods: AHashMap::new(),
            consts: AHashMap::new(),
            anonymous_shapes: AHashMap::new(),
            def_component_overrides: AHashMap::new(),
        }
    }

    fn pool(
        nominals: impl IntoIterator<Item = (Key, DurableNominal<Key, Module>)>,
    ) -> BodyIdentityPool<Key, Module, MapSource> {
        BodyIdentityPool::new(source(nominals), Rc::new(ThreadedRodeo::new()))
    }

    /// A pool seeded with nominals, functions, and methods for the callable
    /// (2b) tests.
    fn callable_pool(
        nominals: impl IntoIterator<Item = (Key, DurableNominal<Key, Module>)>,
        functions: impl IntoIterator<Item = (Key, DurableFunction<Key, Module>)>,
        methods: impl IntoIterator<Item = (Key, DurableMethod<Key, Module>)>,
    ) -> BodyIdentityPool<Key, Module, MapSource> {
        BodyIdentityPool::new(
            MapSource {
                nominals: nominals.into_iter().collect(),
                nominal_file_ids: AHashMap::new(),
                functions: functions.into_iter().collect(),
                function_reads: Rc::new(Cell::new(0)),
                methods: methods.into_iter().collect(),
                consts: AHashMap::new(),
                anonymous_shapes: AHashMap::new(),
                def_component_overrides: AHashMap::new(),
            },
            Rc::new(ThreadedRodeo::new()),
        )
    }

    fn const_pool(
        nominals: impl IntoIterator<Item = (Key, DurableNominal<Key, Module>)>,
        functions: impl IntoIterator<Item = (Key, DurableFunction<Key, Module>)>,
        consts: impl IntoIterator<Item = (Key, DurableConst<Key, Module>)>,
    ) -> BodyIdentityPool<Key, Module, MapSource> {
        BodyIdentityPool::new(
            MapSource {
                nominals: nominals.into_iter().collect(),
                nominal_file_ids: AHashMap::new(),
                functions: functions.into_iter().collect(),
                function_reads: Rc::new(Cell::new(0)),
                methods: AHashMap::new(),
                consts: consts.into_iter().collect(),
                anonymous_shapes: AHashMap::new(),
                def_component_overrides: AHashMap::new(),
            },
            Rc::new(ThreadedRodeo::new()),
        )
    }

    /// A pool seeded with nominals plus anonymous shapes (and optional forced
    /// definition relocations) for the r6b anonymous-mint tests.
    fn anon_pool(
        nominals: impl IntoIterator<Item = (Key, DurableNominal<Key, Module>)>,
        anonymous_shapes: impl IntoIterator<Item = (AnonKey, DurableAnonymousShape<Key, Module>)>,
        def_component_overrides: impl IntoIterator<Item = (Key, String)>,
    ) -> BodyIdentityPool<Key, Module, MapSource> {
        BodyIdentityPool::new(
            MapSource {
                nominals: nominals.into_iter().collect(),
                nominal_file_ids: AHashMap::new(),
                functions: AHashMap::new(),
                function_reads: Rc::new(Cell::new(0)),
                methods: AHashMap::new(),
                consts: AHashMap::new(),
                anonymous_shapes: anonymous_shapes.into_iter().collect(),
                def_component_overrides: def_component_overrides.into_iter().collect(),
            },
            Rc::new(ThreadedRodeo::new()),
        )
    }

    #[test]
    fn provider_resolution_materializes_recursive_named_anonymous_graph() {
        let anonymous = anon_key(AnonymousNominalKind::Struct, 9, 0);
        let mut pool = anon_pool(
            [(
                0,
                named(
                    "Json",
                    "std/json.rue",
                    true,
                    enum_body(vec![(
                        "Array",
                        vec![DType::AnonymousNominal(anonymous.clone())],
                    )]),
                ),
            )],
            [(
                anonymous,
                DurableAnonymousShape::Struct {
                    fields: vec![(Arc::from("element"), DType::Nominal(0))],
                    struct_method_names: Vec::new(),
                },
            )],
            [],
        );

        let json = pool
            .resolve_provider_type(&DType::Nominal(0))
            .expect("provider facts close the recursive nominal graph");
        assert!(json.as_enum().is_some());
    }

    /// An anonymous producer key rooting at definition `producer` with anchor
    /// occurrence `anchor_seg`.
    fn anon_key(kind: AnonymousNominalKind, producer: Key, anchor_seg: u32) -> AnonKey {
        AnonymousNominalKey {
            kind,
            producer: StableProducerId::Definition(producer),
            anchor: rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::AnonymousType(anchor_seg),
            ]),
            arguments: CanonicalArguments::default(),
        }
    }

    fn param(
        name: &str,
        ty: DType,
        mode: SemanticParameterMode,
        is_comptime: bool,
    ) -> DurableSignatureParameter<Key, Module> {
        DurableSignatureParameter {
            name: Arc::from(name),
            ty,
            mode,
            is_comptime,
        }
    }

    fn durable_function(
        parameters: Vec<DurableSignatureParameter<Key, Module>>,
        result: DType,
        is_public: bool,
        is_unchecked: bool,
    ) -> DurableFunction<Key, Module> {
        DurableFunction {
            parameters: parameters.into(),
            result,
            type_syntax: None,
            is_public,
            is_unchecked,
            is_extern: false,
        }
    }

    fn durable_method(
        receiver: DType,
        parameters: Vec<DurableSignatureParameter<Key, Module>>,
        result: DType,
        has_self: bool,
        self_mode: SemanticParameterMode,
    ) -> DurableMethod<Key, Module> {
        DurableMethod {
            receiver,
            parameters: parameters.into(),
            result,
            type_syntax: None,
            has_self,
            self_mode,
            is_accessor: false,
        }
    }

    /// A caller-provided function handle carrying deterministic request/RIR
    /// facts. `resolve_function` must reproduce every field verbatim.
    fn fn_handle(_return_type_symbol: Spur) -> FunctionIdentityHandle {
        FunctionIdentityHandle {
            body: InstRef::from_raw(101),
            declaration: InstRef::from_raw(102),
            span: Span::with_file(FileId::new(7), 3, 9),
            return_type_syntax: rue_rir::RirTypeSyntaxRef::from_u32(17),
            returns_type: true,
            // Alternate true/false so a hardcoded passthrough of either
            // polarity fails the verbatim-handle assertions.
            is_extern: true,
            is_c_export: false,
            allow_unused_function: true,
            allow_unused_variable: false,
            allow_unreachable_code: true,
            file_id: FileId::new(7),
        }
    }

    fn method_handle() -> MethodIdentityHandle {
        MethodIdentityHandle {
            body: InstRef::from_raw(201),
            span: Span::with_file(FileId::new(3), 1, 4),
            self_is_mut: true,
            returns_borrow: false,
        }
    }

    /// Compare a pool `ParamRange` against an epoch-twin one field-column by
    /// field-column, through the same reads the analyzer performs. Names compare
    /// as resolved strings and types via the index-independent `render`/`is_copy`
    /// mirrors, so it is safe across two independently-interned arenas.
    fn assert_param_range_equal(
        pool: &BodyIdentityPool<Key, Module, MapSource>,
        pool_range: ParamRange,
        twin_arena: &ParamArena,
        twin_interner: &ThreadedRodeo,
        twin_range: ParamRange,
        twin_pool: &TypeInternPool,
    ) {
        let arena = pool.param_arena();
        assert_eq!(pool_range.len(), twin_range.len(), "param count");
        for index in 0..pool_range.len() {
            assert_eq!(
                pool.resolve_symbol(arena.names(pool_range)[index]),
                twin_interner.resolve(&twin_arena.names(twin_range)[index]),
                "param name"
            );
            assert_eq!(
                render(pool.type_pool(), arena.types(pool_range)[index]),
                render(twin_pool, twin_arena.types(twin_range)[index]),
                "param type display"
            );
            assert_eq!(
                is_copy(pool.type_pool(), arena.types(pool_range)[index]),
                is_copy(twin_pool, twin_arena.types(twin_range)[index]),
                "param type copyability"
            );
            assert_eq!(
                arena.modes(pool_range)[index],
                twin_arena.modes(twin_range)[index],
                "param mode"
            );
            assert_eq!(
                arena.comptime(pool_range)[index],
                twin_arena.comptime(twin_range)[index],
                "param comptime"
            );
        }
    }

    fn struct_body(
        fields: Vec<(&str, DType)>,
        is_copy: bool,
        is_linear: bool,
    ) -> DurableNominalBody<Key, Module> {
        DurableNominalBody::Struct {
            fields: fields
                .into_iter()
                .map(|(name, ty)| (Arc::from(name), ty))
                .collect(),
            is_copy,
            is_linear,
        }
    }

    fn enum_body(variants: Vec<(&str, Vec<DType>)>) -> DurableNominalBody<Key, Module> {
        DurableNominalBody::Enum {
            variants: variants
                .into_iter()
                .map(|(name, payload)| (Arc::from(name), payload.into()))
                .collect(),
        }
    }

    fn named(
        name: &str,
        module: &str,
        is_public: bool,
        body: DurableNominalBody<Key, Module>,
    ) -> DurableNominal<Key, Module> {
        DurableNominal {
            name: Arc::from(name),
            module_path: Arc::from(module),
            is_public,
            is_builtin: false,
            lang_item: None,
            is_repr_c: false,
            has_destructor: false,
            body,
        }
    }

    /// A local mirror of the host's `format_type_name` (minus the body-local
    /// constructor displays, which are out of 2a) so display parity is asserted
    /// through the same reads the analyzer performs. Recurses through pool
    /// indices, so it is index-independent and safe to compare across two pools.
    fn render(pool: &TypeInternPool, ty: Type) -> String {
        use crate::types::TypeKind;
        match ty.kind() {
            TypeKind::I8 => "i8".into(),
            TypeKind::I16 => "i16".into(),
            TypeKind::I32 => "i32".into(),
            TypeKind::I64 => "i64".into(),
            TypeKind::U8 => "u8".into(),
            TypeKind::U16 => "u16".into(),
            TypeKind::U32 => "u32".into(),
            TypeKind::U64 => "u64".into(),
            TypeKind::Bool => "bool".into(),
            TypeKind::Unit => "()".into(),
            TypeKind::Never => "!".into(),
            TypeKind::Error => "<error>".into(),
            TypeKind::Struct(id) => pool.struct_def(id).name.to_string(),
            TypeKind::Enum(id) => pool.enum_def(id).name.to_string(),
            TypeKind::Array(id) => {
                let (element, len) = pool.array_def(id);
                format!("[{}; {}]", render(pool, element), len)
            }
            TypeKind::PtrConst(id) => format!("ptr const {}", render(pool, pool.ptr_const_def(id))),
            TypeKind::PtrMut(id) => format!("ptr mut {}", render(pool, pool.ptr_mut_def(id))),
            TypeKind::Module(_) => "<module>".into(),
            TypeKind::ComptimeType => "type".into(),
        }
    }

    /// A local mirror of the host's `is_type_copy`, likewise index-independent.
    fn is_copy(pool: &TypeInternPool, ty: Type) -> bool {
        use crate::types::TypeKind;
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
            | TypeKind::Unit
            | TypeKind::Never
            | TypeKind::Error
            | TypeKind::Module(_)
            | TypeKind::ComptimeType
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_) => true,
            TypeKind::Enum(id) => pool
                .enum_def(id)
                .variant_payloads
                .iter()
                .flatten()
                .all(|&ty| is_copy(pool, ty)),
            TypeKind::Struct(id) => pool.struct_def(id).is_copy,
            TypeKind::Array(id) => is_copy(pool, pool.array_def(id).0),
        }
    }

    // ----- Epoch-registration twin -------------------------------------------
    //
    // A twin `TypeInternPool` populated through the exact registration
    // primitives the epoch uses (`sema/declarations.rs`): `declare_struct` /
    // `complete_declared_struct`, `declare_enum` / `complete_declared_enum`,
    // `set_symbol_paths`, `set_struct_lang_item`. The pool under test drives the
    // same primitives from durable metadata; comparing the two proves the pool
    // assembles a byte-equivalent `StructDef` / `EnumDef` and registration.
    //
    // The twin uses the same body-local `FileId` the pool assigns for a single
    // module (`FileId::new(1)`), so `file_id` and the mangled symbol match too.

    const TWIN_FILE: FileId = FileId::new(1);

    fn twin_pool(module_path: &str) -> (TypeInternPool, ThreadedRodeo) {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        pool.set_symbol_paths(AHashMap::from([(TWIN_FILE, module_path.to_owned())]));
        (pool, interner)
    }

    fn twin_declare_struct(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        is_copy: bool,
        is_linear: bool,
        is_pub: bool,
        fields: Vec<(&str, Type)>,
        lang_item: Option<LangItem>,
    ) -> StructId {
        let symbol = interner.get_or_intern(name);
        let (id, _) = pool.declare_struct(
            symbol,
            StructDef {
                name: name.into(),
                fields: Vec::new(),
                is_copy,
                is_linear,
                destructor: None,
                is_builtin: false,
                is_pub,
                file_id: TWIN_FILE,
            },
        );
        if let Some(lang_item) = lang_item {
            pool.set_struct_lang_item(id, lang_item);
        }
        pool.complete_declared_struct(
            id,
            StructDef {
                name: name.into(),
                fields: fields
                    .into_iter()
                    .map(|(name, ty)| StructField {
                        name: name.to_owned(),
                        ty,
                    })
                    .collect(),
                is_copy,
                is_linear,
                destructor: None,
                is_builtin: false,
                is_pub,
                file_id: TWIN_FILE,
            },
        );
        id
    }

    fn twin_declare_enum(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        is_pub: bool,
        variants: Vec<(&str, Vec<Type>)>,
    ) -> EnumId {
        let symbol = interner.get_or_intern(name);
        let variant_names: Arc<[Arc<str>]> = variants.iter().map(|(n, _)| Arc::from(*n)).collect();
        let (id, _) = pool.declare_enum(
            symbol,
            EnumDef {
                name: name.into(),
                variants: variant_names.clone(),
                variant_payloads: Vec::new(),
                is_pub,
                file_id: TWIN_FILE,
            },
        );
        pool.complete_declared_enum(
            id,
            EnumDef {
                name: name.into(),
                variants: variant_names,
                variant_payloads: variants.into_iter().map(|(_, p)| p).collect(),
                is_pub,
                file_id: TWIN_FILE,
            },
        );
        id
    }

    fn assert_struct_metadata_equal(
        pool: &TypeInternPool,
        pool_id: StructId,
        twin: &TypeInternPool,
        twin_id: StructId,
    ) {
        let a = pool.struct_def(pool_id);
        let b = twin.struct_def(twin_id);
        assert_eq!(a.name, b.name, "struct name");
        assert_eq!(a.is_copy, b.is_copy, "struct is_copy");
        assert_eq!(a.is_linear, b.is_linear, "struct is_linear");
        assert_eq!(a.is_pub, b.is_pub, "struct is_pub");
        assert_eq!(a.is_builtin, b.is_builtin, "struct is_builtin");
        assert_eq!(a.destructor, b.destructor, "struct destructor");
        assert_eq!(a.file_id, b.file_id, "struct file_id");
        assert_eq!(a.fields.len(), b.fields.len(), "struct field count");
        for (fa, fb) in a.fields.iter().zip(b.fields.iter()) {
            assert_eq!(fa.name, fb.name, "field name");
            assert_eq!(
                render(pool, fa.ty),
                render(twin, fb.ty),
                "field type display"
            );
            assert_eq!(
                is_copy(pool, fa.ty),
                is_copy(twin, fb.ty),
                "field type copyability"
            );
        }
        assert_eq!(
            pool.struct_symbol_name(pool_id),
            twin.struct_symbol_name(twin_id),
            "struct symbol name"
        );
        assert_eq!(
            is_copy(pool, Type::new_struct(pool_id)),
            is_copy(twin, Type::new_struct(twin_id))
        );
        assert_eq!(
            render(pool, Type::new_struct(pool_id)),
            render(twin, Type::new_struct(twin_id))
        );
    }

    #[test]
    fn mints_once_and_dedups_named_struct() {
        let mut pool = pool([(
            0,
            named(
                "Point",
                "pkg/geom.rue",
                true,
                struct_body(vec![("x", DType::I64), ("y", DType::I64)], false, false),
            ),
        )]);

        let before = pool.type_pool().len();
        let first = pool.resolve(&DType::Nominal(0)).unwrap();
        let after_first = pool.type_pool().len();
        // Double consult: same id, and the pool grows not at all.
        let second = pool.resolve(&DType::Nominal(0)).unwrap();
        assert_eq!(first, second, "repeat consult returns the same id");
        assert_eq!(
            pool.type_pool().len(),
            after_first,
            "repeat consult mints nothing new"
        );
        assert!(after_first > before, "first consult minted an identity");
        assert_eq!(render(pool.type_pool(), first), "Point");
    }

    #[test]
    fn containment_freeze_hook_gates_drop_and_linearity_reads() {
        // A completed declare/complete mint derives its containment facts
        // incrementally, so drop/linearity reads answer exactly as soon as the
        // pair settles — the pool-side freeze then finds nothing left to walk.
        // Destructor metadata for this nominal is `None`, and the mint folds
        // that into the completing definition.
        let mut pool = pool([(
            0,
            named(
                "Point",
                "pkg/geom.rue",
                true,
                struct_body(vec![("x", DType::I64), ("y", DType::I64)], false, false),
            ),
        )]);
        let ty = pool.resolve(&DType::Nominal(0)).unwrap();
        assert_eq!(
            pool.type_needs_drop(ty),
            Some(false),
            "a settled mint carries exact facts before the freeze"
        );
        assert_eq!(pool.type_carries_linear(ty), Some(false));
        pool.finalize_containment_metadata()
            .expect("no containment cycle");
        assert_eq!(pool.type_needs_drop(ty), Some(false));
        assert_eq!(pool.type_carries_linear(ty), Some(false));
        // Repeat freeze is a pure re-finalization, not an error.
        pool.finalize_containment_metadata()
            .expect("repeat freeze is fine");
    }

    #[test]
    fn struct_metadata_matches_epoch_twin() {
        // A copy struct and a non-copy struct, both with primitive fields.
        for (is_copy_flag, is_linear_flag, name) in
            [(true, false, "CopyPair"), (false, false, "MovePair")]
        {
            let mut pool = pool([(
                0,
                named(
                    name,
                    "pkg/data.rue",
                    true,
                    struct_body(
                        vec![("a", DType::I32), ("b", DType::Bool)],
                        is_copy_flag,
                        is_linear_flag,
                    ),
                ),
            )]);
            let ty = pool.resolve(&DType::Nominal(0)).unwrap();

            let (twin, twin_interner) = twin_pool("pkg/data.rue");
            let twin_id = twin_declare_struct(
                &twin,
                &twin_interner,
                name,
                is_copy_flag,
                is_linear_flag,
                true,
                vec![("a", Type::I32), ("b", Type::BOOL)],
                None,
            );

            assert_struct_metadata_equal(pool.type_pool(), ty.as_struct().unwrap(), &twin, twin_id);
        }
    }

    #[test]
    fn enum_metadata_matches_epoch_twin() {
        // Payload-bearing and discriminant-only variants; copyability recurses.
        let mut pool = pool([(
            0,
            named(
                "Shape",
                "pkg/geom.rue",
                true,
                enum_body(vec![
                    ("Dot", vec![]),
                    ("Line", vec![DType::I64, DType::I64]),
                ]),
            ),
        )]);
        let ty = pool.resolve(&DType::Nominal(0)).unwrap();

        let (twin, twin_interner) = twin_pool("pkg/geom.rue");
        let twin_id = twin_declare_enum(
            &twin,
            &twin_interner,
            "Shape",
            true,
            vec![("Dot", vec![]), ("Line", vec![Type::I64, Type::I64])],
        );

        let a = pool.type_pool().enum_def(ty.as_enum().unwrap());
        let b = twin.enum_def(twin_id);
        assert_eq!(a.name, b.name);
        assert_eq!(a.variants, b.variants);
        assert_eq!(a.is_pub, b.is_pub);
        assert_eq!(a.file_id, b.file_id);
        assert_eq!(a.variant_payloads.len(), b.variant_payloads.len());
        for (pa, pb) in a.variant_payloads.iter().zip(b.variant_payloads.iter()) {
            assert_eq!(pa.len(), pb.len());
            for (ta, tb) in pa.iter().zip(pb.iter()) {
                assert_eq!(render(pool.type_pool(), *ta), render(&twin, *tb));
            }
        }
        assert_eq!(
            pool.type_pool().enum_symbol_name(ty.as_enum().unwrap()),
            twin.enum_symbol_name(twin_id)
        );
        assert_eq!(
            is_copy(pool.type_pool(), ty),
            is_copy(&twin, Type::new_enum(twin_id))
        );
        assert_eq!(render(pool.type_pool(), ty), "Shape");
    }

    #[test]
    fn non_copy_field_makes_struct_non_copy_reads_consistent() {
        // Struct with a non-copy nominal field: is_type_copy reads the struct's
        // own @copy flag, and format renders the nested name.
        let mut pool = pool([
            (
                0,
                named(
                    "Owner",
                    "pkg/own.rue",
                    true,
                    struct_body(vec![("h", DType::Nominal(1))], false, false),
                ),
            ),
            (
                1,
                named(
                    "Handle",
                    "pkg/own.rue",
                    true,
                    struct_body(vec![("raw", DType::U64)], false, false),
                ),
            ),
        ]);
        let owner = pool.resolve(&DType::Nominal(0)).unwrap();
        let def = pool.type_pool().struct_def(owner.as_struct().unwrap());
        assert_eq!(def.fields.len(), 1);
        assert_eq!(render(pool.type_pool(), def.fields[0].ty), "Handle");
        assert!(!is_copy(pool.type_pool(), owner));
    }

    #[test]
    fn nested_nominal_dedups_shared_child() {
        let mut pool = pool([
            (
                0,
                named(
                    "Pair",
                    "pkg/p.rue",
                    true,
                    struct_body(
                        vec![("l", DType::Nominal(2)), ("r", DType::Nominal(2))],
                        false,
                        false,
                    ),
                ),
            ),
            (
                2,
                named(
                    "Leaf",
                    "pkg/p.rue",
                    true,
                    struct_body(vec![("v", DType::I32)], true, false),
                ),
            ),
        ]);
        pool.resolve(&DType::Nominal(0)).unwrap();
        // The shared child was minted once; a direct consult returns that id.
        let leaf_first = pool.resolve(&DType::Nominal(2)).unwrap();
        let len_after = pool.type_pool().len();
        let leaf_second = pool.resolve(&DType::Nominal(2)).unwrap();
        assert_eq!(leaf_first, leaf_second);
        assert_eq!(
            pool.type_pool().len(),
            len_after,
            "no re-mint of shared child"
        );
    }

    #[test]
    fn recursive_struct_through_pointer_mints_once() {
        // Node { next: ptr mut Node } — the recursive reference resolves to the
        // shell id registered before field resolution.
        let mut pool = pool([(
            0,
            named(
                "Node",
                "pkg/list.rue",
                true,
                struct_body(
                    vec![
                        ("value", DType::I64),
                        ("next", DType::PtrMut(Box::new(DType::Nominal(0)))),
                    ],
                    false,
                    false,
                ),
            ),
        )]);
        let node = pool.resolve(&DType::Nominal(0)).unwrap();
        let node_id = node.as_struct().unwrap();
        let def = pool.type_pool().struct_def(node_id);
        assert_eq!(def.fields.len(), 2);
        assert_eq!(render(pool.type_pool(), def.fields[1].ty), "ptr mut Node");
        // Re-consulting the recursive nominal yields the same id (dedup).
        assert_eq!(pool.resolve(&DType::Nominal(0)).unwrap(), node);
    }

    #[test]
    fn structural_wraps_intern_and_dedup() {
        let mut pool = pool([(
            0,
            named(
                "Cell",
                "pkg/c.rue",
                true,
                struct_body(vec![("v", DType::I32)], true, false),
            ),
        )]);

        // Array dedup.
        let array_ty = DType::Array {
            element: Box::new(DType::I32),
            len: 4,
        };
        let a1 = pool.resolve(&array_ty).unwrap();
        let len_after = pool.type_pool().len();
        let a2 = pool.resolve(&array_ty).unwrap();
        assert_eq!(a1, a2);
        assert_eq!(pool.type_pool().len(), len_after, "array interning dedups");
        assert_eq!(render(pool.type_pool(), a1), "[i32; 4]");

        // Pointer dedup.
        let ptr_ty = DType::PtrConst(Box::new(DType::I32));
        let p1 = pool.resolve(&ptr_ty).unwrap();
        let len_after_ptr = pool.type_pool().len();
        let p2 = pool.resolve(&ptr_ty).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(
            pool.type_pool().len(),
            len_after_ptr,
            "ptr interning dedups"
        );
        assert_eq!(render(pool.type_pool(), p1), "ptr const i32");

        // Array of a nominal renders through the minted child.
        let array_of_cell = DType::Array {
            element: Box::new(DType::Nominal(0)),
            len: 3,
        };
        let ac = pool.resolve(&array_of_cell).unwrap();
        assert_eq!(render(pool.type_pool(), ac), "[Cell; 3]");
        assert!(is_copy(pool.type_pool(), ac), "array of @copy Cell is copy");
    }

    #[test]
    fn display_parity_nominal_array_ptr_against_twin() {
        let mut pool = pool([(
            0,
            named(
                "Widget",
                "pkg/ui.rue",
                true,
                struct_body(vec![("id", DType::U32)], false, false),
            ),
        )]);
        let (twin, twin_interner) = twin_pool("pkg/ui.rue");
        let twin_id = twin_declare_struct(
            &twin,
            &twin_interner,
            "Widget",
            false,
            false,
            true,
            vec![("id", Type::U32)],
            None,
        );

        // Nominal.
        let pool_widget = pool.resolve(&DType::Nominal(0)).unwrap();
        let twin_widget = Type::new_struct(twin_id);
        assert_eq!(
            render(pool.type_pool(), pool_widget),
            render(&twin, twin_widget)
        );

        // Array of nominal.
        let pool_arr = pool
            .resolve(&DType::Array {
                element: Box::new(DType::Nominal(0)),
                len: 3,
            })
            .unwrap();
        let twin_arr = twin.try_intern_array(twin_widget, 3).unwrap();
        assert_eq!(render(pool.type_pool(), pool_arr), render(&twin, twin_arr));
        assert_eq!(render(pool.type_pool(), pool_arr), "[Widget; 3]");

        // Pointer of nominal.
        let pool_ptr = pool
            .resolve(&DType::PtrConst(Box::new(DType::Nominal(0))))
            .unwrap();
        let twin_ptr = twin.try_intern_ptr_const(twin_widget).unwrap();
        assert_eq!(render(pool.type_pool(), pool_ptr), render(&twin, twin_ptr));

        // Nested ptr-of-ptr.
        let pool_pp = pool
            .resolve(&DType::PtrMut(Box::new(DType::PtrConst(Box::new(
                DType::I32,
            )))))
            .unwrap();
        assert_eq!(render(pool.type_pool(), pool_pp), "ptr mut ptr const i32");
    }

    #[test]
    fn qualified_symbol_matches_twin_and_lang_item_is_exempt() {
        // A user nominal is unconditionally file-qualified; a lang-item nominal
        // keeps its bare name.
        let mut pool = pool([
            (
                0,
                named(
                    "Buffer",
                    "pkg/buf.rue",
                    true,
                    struct_body(vec![("len", DType::U64)], false, false),
                ),
            ),
            (
                1,
                DurableNominal {
                    name: Arc::from("StrBuf"),
                    module_path: Arc::from("\0rue-std/strbuf.rue"),
                    is_public: true,
                    is_builtin: false,
                    lang_item: Some(LangItem::StrBuf),
                    is_repr_c: false,
                    has_destructor: false,
                    body: struct_body(vec![("len", DType::U64)], false, false),
                },
            ),
        ]);

        let buffer = pool.resolve(&DType::Nominal(0)).unwrap();
        let buffer_id = buffer.as_struct().unwrap();
        let symbol = pool.type_pool().struct_symbol_name(buffer_id);
        assert!(
            symbol.starts_with("Buffer$"),
            "user nominal is file-qualified, got {symbol}"
        );

        let (twin, twin_interner) = twin_pool("pkg/buf.rue");
        let twin_id = twin_declare_struct(
            &twin,
            &twin_interner,
            "Buffer",
            false,
            false,
            true,
            vec![("len", Type::U64)],
            None,
        );
        assert_eq!(symbol, twin.struct_symbol_name(twin_id));

        // Lang-item nominal keeps its bare name.
        let strbuf = pool.resolve(&DType::Nominal(1)).unwrap();
        assert_eq!(
            pool.type_pool()
                .struct_symbol_name(strbuf.as_struct().unwrap()),
            "StrBuf"
        );
    }

    #[test]
    fn builtin_nominal_and_str_resolve_to_preregistered() {
        let mut pool = pool([]);

        // The three `@target_*` builtin enums (`Arch`/`Os`/`DataModel`, the
        // `rue_builtins::BUILTIN_ENUMS` set the `@target_arch`/`@target_os`/
        // `@target_data_model` intrinsics consume) all resolve to the
        // pre-registered enum — the provider-era answer for the target-config
        // family is the pre-registered builtin plus body-local target
        // selection, so it needs no new fact (RUE-1091 r6a, deliverable 4).
        for name in ["Arch", "Os", "DataModel"] {
            let enum_ty = pool
                .resolve(&DType::BuiltinNominal {
                    name: Arc::from(name),
                    kind: SemanticImportNominalKind::Enum,
                })
                .unwrap();
            assert_eq!(render(pool.type_pool(), enum_ty), name);
            assert_eq!(
                pool.type_pool()
                    .enum_symbol_name(enum_ty.as_enum().unwrap()),
                name,
                "{name} keeps its bare builtin symbol"
            );
        }

        // The core `str` identity.
        let str_ty = pool
            .resolve(&DType::BuiltinNominal {
                name: Arc::from("str"),
                kind: SemanticImportNominalKind::Struct,
            })
            .unwrap();
        assert_eq!(render(pool.type_pool(), str_ty), "str");
        assert!(is_copy(pool.type_pool(), str_ty));
        assert_eq!(
            pool.type_pool()
                .struct_symbol_name(str_ty.as_struct().unwrap()),
            "str",
            "builtin keeps its bare name"
        );

        // Wrong kind and unknown builtin fail closed.
        assert_eq!(
            pool.resolve(&DType::BuiltinNominal {
                name: Arc::from("Arch"),
                kind: SemanticImportNominalKind::Struct,
            }),
            Err(IdentityMintError::BuiltinNominalKindMismatch)
        );
        assert_eq!(
            pool.resolve(&DType::BuiltinNominal {
                name: Arc::from("Nope"),
                kind: SemanticImportNominalKind::Enum,
            }),
            Err(IdentityMintError::UnknownBuiltinNominal)
        );
    }

    #[test]
    fn anonymous_arm_resolves_by_lookup() {
        let mut pool = pool([(
            0,
            named(
                "Cell",
                "pkg/c.rue",
                true,
                struct_body(vec![("v", DType::I32)], true, false),
            ),
        )]);
        let cell = pool.resolve(&DType::Nominal(0)).unwrap();

        let anon_key = AnonymousNominalKey {
            kind: AnonymousNominalKind::Struct,
            producer: StableProducerId::Definition(0u32),
            anchor: rue_rir::RirStructuralAnchor::new(
                Vec::<rue_rir::RirStructuralPathSegment>::new(),
            ),
            arguments: CanonicalArguments::default(),
        };

        // Before registration, the anonymous arm fails closed.
        assert_eq!(
            pool.resolve(&DType::AnonymousNominal(anon_key.clone())),
            Err(IdentityMintError::MissingAnonymous)
        );

        // After the issuing machinery records the id, it resolves by lookup.
        pool.register_issued_anonymous(anon_key.clone(), cell);
        assert_eq!(
            pool.resolve(&DType::AnonymousNominal(anon_key)).unwrap(),
            cell
        );
    }

    #[test]
    fn reverse_identity_indexes_cover_large_named_and_anonymous_pool() {
        const COUNT: u32 = 32;
        let nominals = (0..COUNT).map(|key| {
            let name = format!("Named{key}");
            let body = if key % 2 == 0 {
                struct_body(vec![("value", DType::I32)], true, false)
            } else {
                enum_body(vec![("Value", vec![DType::I32])])
            };
            (key, named(&name, "pkg/identities.rue", true, body))
        });
        let anonymous_keys = (0..COUNT)
            .map(|key| anon_key(AnonymousNominalKind::Struct, key + COUNT, key))
            .collect::<Vec<_>>();
        let anonymous_shapes = anonymous_keys.iter().cloned().map(|key| {
            (
                key,
                DurableAnonymousShape::Struct {
                    fields: vec![(Arc::from("value"), DType::I32)],
                    struct_method_names: Vec::new(),
                },
            )
        });
        let mut pool = anon_pool(nominals, anonymous_shapes, []);

        for key in 0..COUNT {
            let ty = pool.resolve(&DType::Nominal(key)).unwrap();
            assert_eq!(pool.durable_named_identity(ty), Some(key));
        }
        for key in &anonymous_keys {
            let ty = pool.find_or_create_anon(key).unwrap();
            assert_eq!(pool.durable_anonymous_identity(ty).as_ref(), Some(key));
        }

        assert_eq!(
            pool.struct_identities.len() + pool.enum_identities.len(),
            COUNT as usize
        );
        assert_eq!(pool.anonymous_identities.len(), COUNT as usize);
    }

    #[test]
    fn failed_anonymous_shell_rolls_back_both_identity_directions() {
        let outer = anon_key(AnonymousNominalKind::Struct, 80, 0);
        let missing = anon_key(AnonymousNominalKind::Struct, 81, 0);
        let mut pool = anon_pool(
            [],
            [(
                outer.clone(),
                DurableAnonymousShape::Struct {
                    fields: vec![(Arc::from("missing"), DType::AnonymousNominal(missing))],
                    struct_method_names: Vec::new(),
                },
            )],
            [],
        );

        let expected = Err(IdentityMintError::MissingAnonymousShape);
        assert_eq!(pool.find_or_create_anon(&outer), expected);
        assert_eq!(pool.anonymous_poisoned.get(&outer), expected.as_ref().err());
        assert!(!pool.anon_nominals.contains_key(&outer));
        assert!(pool.anonymous_identities.is_empty());

        assert_eq!(pool.find_or_create_anon(&outer), expected);
        assert_eq!(pool.anonymous_poisoned.get(&outer), expected.as_ref().err());
        assert!(!pool.anon_nominals.contains_key(&outer));
        assert!(pool.anonymous_identities.is_empty());
    }

    /// The pool spells the anonymous name from the ONE shared digest computation
    /// (`stable_digest`), fed the adapter's durable→content relocation — the same
    /// function and inputs the epoch uses. Proves the pool did not re-derive a
    /// second digest path.
    #[test]
    fn find_or_create_anon_uses_the_shared_digest() {
        let key = anon_key(AnonymousNominalKind::Struct, 3, 0);
        let shape = DurableAnonymousShape::Struct {
            fields: vec![(Arc::from("v"), DType::I32)],
            struct_method_names: Vec::new(),
        };
        let mut pool = anon_pool([], [(key.clone(), shape)], []);
        let ty = pool.find_or_create_anon(&key).unwrap();

        // Independently relocate and hash through the shared computation.
        let relocated = key
            .try_map_identities::<String, String, std::convert::Infallible>(
                &|k| Ok(format!("D\u{1}{k}")),
                &|m: &Module| Ok(format!("M\u{1}{m}")),
            )
            .unwrap();
        let digest = crate::stable_digest::stable_anonymous_identity_digest(&relocated);
        assert_eq!(pool.source.anonymous_identity_digest(&key), digest);
        assert_eq!(
            render(pool.type_pool(), ty),
            format!("__anon_struct_{digest:032x}"),
        );
    }

    /// A field-only anonymous struct mints byte-identically to the epoch's
    /// `find_or_create_anon_struct`: the `__anon_struct_{digest}` name, private,
    /// copyable iff every field is. Dedups by producer key on repeat, and a later
    /// `resolve(AnonymousNominal)` finds it by lookup.
    #[test]
    fn find_or_create_anon_struct_mints_and_dedups() {
        let key = anon_key(AnonymousNominalKind::Struct, 0, 1);
        let shape = DurableAnonymousShape::Struct {
            fields: vec![(Arc::from("a"), DType::I32), (Arc::from("b"), DType::Bool)],
            struct_method_names: Vec::new(),
        };
        let mut pool = anon_pool([], [(key.clone(), shape)], []);

        let ty = pool.find_or_create_anon(&key).unwrap();
        let id = ty.as_struct().unwrap();
        let def = pool.type_pool().struct_def(id);
        assert!(def.name.starts_with("__anon_struct_"));
        assert_eq!(def.name.len(), "__anon_struct_".len() + 32);
        assert!(!def.is_pub, "anonymous structs are private");
        assert!(!def.is_builtin);
        assert!(def.is_copy, "all-copy fields, no destructor");
        assert_eq!(def.destructor, None);
        assert_eq!(def.fields.len(), 2);
        assert_eq!(def.fields[0].name, "a");

        // Producer-nominal dedup: a repeat consult re-mints nothing.
        let again = pool.find_or_create_anon(&key).unwrap();
        assert_eq!(ty, again);
        // And the `resolve` anonymous arm now finds it by the lookup the mint
        // populated.
        assert_eq!(
            pool.resolve(&DType::AnonymousNominal(key.clone())).unwrap(),
            ty,
            "the mint records the issued id for the lookup arm",
        );
        assert_eq!(
            pool.durable_anonymous_identity(ty),
            Some(key),
            "the recursive shell remains the reverse-export identity",
        );
    }

    /// A `__drop` method forces the anonymous struct non-Copy and names its
    /// destructor `{name}.__drop`, mirroring the epoch's destructor metadata.
    #[test]
    fn find_or_create_anon_struct_with_drop_is_non_copy() {
        let key = anon_key(AnonymousNominalKind::Struct, 0, 2);
        let shape = DurableAnonymousShape::Struct {
            fields: vec![(Arc::from("v"), DType::I32)],
            struct_method_names: vec![Arc::from("__drop"), Arc::from("len")],
        };
        let mut pool = anon_pool([], [(key.clone(), shape)], []);
        let ty = pool.find_or_create_anon(&key).unwrap();
        let def = pool.type_pool().struct_def(ty.as_struct().unwrap());
        assert!(!def.is_copy, "a type with a destructor cannot be copy");
        assert_eq!(
            def.destructor.as_deref(),
            Some(format!("{}.__drop", def.name).as_str())
        );
    }

    /// An anonymous enum mints the `__anon_enum_{digest} { A(i32), B }` name whose
    /// payloads render through `safe_name_with_pool`, exactly as the epoch spells
    /// them.
    #[test]
    fn find_or_create_anon_enum_mints_with_payload_names() {
        let key = anon_key(AnonymousNominalKind::Enum, 5, 0);
        let shape = DurableAnonymousShape::Enum {
            variants: vec![
                (Arc::from("Some"), vec![DType::I32]),
                (Arc::from("None"), vec![]),
            ],
        };
        let mut pool = anon_pool([], [(key.clone(), shape)], []);
        let ty = pool.find_or_create_anon(&key).unwrap();
        let def = pool.type_pool().enum_def(ty.as_enum().unwrap());
        assert!(def.name.starts_with("__anon_enum_"));
        assert!(
            def.name.ends_with(" { Some(i32), None }"),
            "enum name renders payloads: {}",
            def.name
        );
        assert!(!def.is_pub);
        assert_eq!(
            def.variants.iter().map(Arc::as_ref).collect::<Vec<_>>(),
            ["Some", "None"]
        );
        assert_eq!(
            pool.durable_anonymous_identity(ty),
            Some(key),
            "the enum mint records its reverse-export identity",
        );
    }

    /// The producer-nominal mint records the pool-level anonymity mark, exactly
    /// as the epoch's `find_or_create_anon_struct` / `_enum` do (RUE-1050).
    ///
    /// This is the drift guard for RUE-1193. The pool registry — not the
    /// `__anon_struct_`/`__anon_enum_` spelling, which is a legal source name —
    /// is what decides that symbol spelling keeps the bare synthetic name and
    /// that CFG destructor discovery and drop glue treat the type as generated.
    /// A mint that registers the type but forgets the mark republishes a
    /// generated anonymous type as an ordinary user nominal: its callable
    /// symbols become file-qualified here while the rooted image still spells
    /// them bare, and the two sides stop joining.
    #[test]
    fn minted_anonymous_nominals_carry_the_pool_anonymity_mark() {
        let struct_key = anon_key(AnonymousNominalKind::Struct, 0, 7);
        let enum_key = anon_key(AnonymousNominalKind::Enum, 0, 8);
        let struct_shape = DurableAnonymousShape::Struct {
            fields: vec![(Arc::from("x"), DType::I32)],
            struct_method_names: vec![Arc::from("get")],
        };
        let enum_shape = DurableAnonymousShape::Enum {
            variants: vec![(Arc::from("A"), vec![])],
        };
        let mut pool = anon_pool(
            [],
            [
                (struct_key.clone(), struct_shape),
                (enum_key.clone(), enum_shape),
            ],
            [],
        );

        let struct_ty = pool.find_or_create_anon(&struct_key).unwrap();
        let struct_id = struct_ty.as_struct().unwrap();
        assert!(
            pool.type_pool().is_anonymous_struct(struct_id),
            "the mint must register the anonymity mark, not rely on the name",
        );
        // Membership is what keeps the symbol bare; the name is only evidence.
        let struct_name = pool.type_pool().struct_def(struct_id).name.to_string();
        assert_eq!(pool.type_pool().struct_symbol_name(struct_id), struct_name);

        let enum_ty = pool.find_or_create_anon(&enum_key).unwrap();
        let enum_id = enum_ty.as_enum().unwrap();
        assert!(
            pool.type_pool().is_anonymous_enum(enum_id),
            "the enum mint must register the anonymity mark too",
        );
        let enum_name = pool.type_pool().enum_def(enum_id).name.to_string();
        assert_eq!(pool.type_pool().enum_symbol_name(enum_id), enum_name);
    }

    /// Distinct producer keys forced onto one digest fail closed: the second is
    /// refused with `AnonymousDigestCollision`, and neither its nominal nor its
    /// symbol is published — the pool analog of the epoch's Theme-4b registry.
    #[test]
    fn anonymous_digest_collision_fails_closed() {
        // Two DISTINCT producer keys (different producer definitions) whose
        // definition relocation is forced to one string, so they hash identically
        // without a real hash collision.
        let first = anon_key(AnonymousNominalKind::Struct, 10, 0);
        let second = anon_key(AnonymousNominalKind::Struct, 11, 0);
        assert_ne!(first, second);
        let shape = |()| DurableAnonymousShape::Struct {
            fields: vec![(Arc::from("v"), DType::I32)],
            struct_method_names: Vec::new(),
        };
        let mut pool = anon_pool(
            [],
            [(first.clone(), shape(())), (second.clone(), shape(()))],
            [
                (10u32, "D\u{1}collide".to_string()),
                (11u32, "D\u{1}collide".to_string()),
            ],
        );

        let minted = pool.find_or_create_anon(&first).unwrap();
        let digest = match pool.find_or_create_anon(&second) {
            Err(IdentityMintError::AnonymousDigestCollision(digest)) => digest,
            other => panic!("expected a fail-closed collision, got {other:?}"),
        };
        // Zero publication for the colliding key: it is absent from the lookup
        // cache, so no symbol was minted for it either.
        assert_eq!(
            pool.resolve(&DType::AnonymousNominal(second)),
            Err(IdentityMintError::MissingAnonymous),
            "the colliding key published no id"
        );
        // The winning key still resolves, and the digest names the collision.
        assert_eq!(
            pool.resolve(&DType::AnonymousNominal(first)).unwrap(),
            minted
        );
        assert_ne!(digest, 0);
    }

    /// Same producer key re-presented is legitimate reuse (the guard never trips
    /// on it), before and after an unrelated distinct key mints.
    #[test]
    fn anonymous_same_key_reuses() {
        let key = anon_key(AnonymousNominalKind::Struct, 20, 0);
        let other = anon_key(AnonymousNominalKind::Struct, 21, 0);
        let shape = || DurableAnonymousShape::Struct {
            fields: vec![(Arc::from("v"), DType::I32)],
            struct_method_names: Vec::new(),
        };
        let mut pool = anon_pool([], [(key.clone(), shape()), (other.clone(), shape())], []);
        let minted = pool.find_or_create_anon(&key).unwrap();
        assert_eq!(pool.find_or_create_anon(&key).unwrap(), minted);
        pool.find_or_create_anon(&other).unwrap();
        assert_eq!(pool.find_or_create_anon(&key).unwrap(), minted);
    }

    /// Collapsed and non-collapsed spellings of ONE producer mint ONE identity:
    /// the pool canonicalizes on entry (`with_canonical_producer`), so a caller
    /// handing an empty-argument `Specialization` wrapper (the
    /// declaration-signature projection's quirk) dedups onto the collapsed
    /// form's mint and spells the collapsed form's digest — dedup, not the
    /// `AnonymousDigestCollision` a second distinct-keyed mint hashing the same
    /// digest would be refused with.
    #[test]
    fn anonymous_producer_collapse_dedups_on_entry() {
        use crate::semantic_identity::FunctionInstanceKey;
        let collapsed = AnonymousNominalKey {
            kind: AnonymousNominalKind::Struct,
            producer: StableProducerId::Function(Box::new(FunctionInstanceKey::Definition(7u32))),
            anchor: rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::AnonymousType(0),
            ]),
            arguments: CanonicalArguments::default(),
        };
        let wrapped = AnonymousNominalKey {
            producer: StableProducerId::Function(Box::new(FunctionInstanceKey::Specialization {
                base: Box::new(FunctionInstanceKey::Definition(7u32)),
                arguments: CanonicalArguments::default(),
            })),
            ..collapsed.clone()
        };
        assert_ne!(collapsed, wrapped, "the raw spellings are distinct keys");

        // The durable universe keys shapes by the CANONICAL form (the
        // `DurableAnonymousSource` contract).
        let shape = DurableAnonymousShape::Struct {
            fields: vec![(Arc::from("v"), DType::I32)],
            struct_method_names: Vec::new(),
        };
        let mut pool = anon_pool([], [(collapsed.clone(), shape)], []);

        // The WRAPPED form mints first: its shape consult and digest already run
        // under the collapsed key, proving entry canonicalization (a raw-keyed
        // consult would fail closed with `MissingAnonymousShape`).
        let via_wrapped = pool.find_or_create_anon(&wrapped).unwrap();
        let via_collapsed = pool.find_or_create_anon(&collapsed).unwrap();
        assert_eq!(via_wrapped, via_collapsed, "one producer, one identity");

        // The spelled name is the collapsed form's shared-digest name.
        let relocated = collapsed
            .try_map_identities::<String, String, std::convert::Infallible>(
                &|k| Ok(format!("D\u{1}{k}")),
                &|m: &Module| Ok(format!("M\u{1}{m}")),
            )
            .unwrap();
        let digest = crate::stable_digest::stable_anonymous_identity_digest(&relocated);
        assert_eq!(
            render(pool.type_pool(), via_wrapped),
            format!("__anon_struct_{digest:032x}"),
        );
    }

    /// A key with no durable shape fails closed rather than minting an empty
    /// nominal.
    #[test]
    fn find_or_create_anon_missing_shape_fails_closed() {
        let key = anon_key(AnonymousNominalKind::Struct, 0, 0);
        let mut pool = anon_pool([], [], []);
        assert_eq!(
            pool.find_or_create_anon(&key),
            Err(IdentityMintError::MissingAnonymousShape)
        );
    }

    /// The well-known `Option` install (r6c) mints the trusted enum through the
    /// ordinary anonymous machinery, records the export-as-produced ruling
    /// (the identity is a well-known identity, membership canonical under
    /// entry canonicalization), and records the payload→option demand map.
    /// A repeat install is a pure dedup.
    #[test]
    fn well_known_option_install_records_produced_ruling_and_registry() {
        use crate::semantic_identity::FunctionInstanceKey;
        let key = anon_key(AnonymousNominalKind::Enum, 30, 0);
        let shape = DurableAnonymousShape::Enum {
            variants: vec![
                (Arc::from("Some"), vec![DType::I64]),
                (Arc::from("None"), vec![]),
            ],
        };
        let mut pool = anon_pool([], [(key.clone(), shape)], []);

        pool.install_well_known_option_types(
            std::slice::from_ref(&key),
            &[(DType::I64, DType::AnonymousNominal(key.clone()))],
        )
        .unwrap();

        // The enum minted through the ordinary anonymous machinery.
        let minted = pool.find_or_create_anon(&key).unwrap();
        let def = pool.type_pool().enum_def(minted.as_enum().unwrap());
        assert!(def.name.starts_with("__anon_enum_"));
        assert!(
            def.name.ends_with(" { Some(i64), None }"),
            "the trusted enum renders its payloads: {}",
            def.name
        );

        // The export-as-produced ruling: the identity is recorded as
        // well-known, so the provider baseline subtraction exports it as a
        // produced anonymous nominal instead of leaking it as an import.
        assert!(pool.is_well_known_option_identity(&key));
        assert_eq!(pool.well_known_option_identity_count(), 1);
        // Membership is canonical: a wrapper-form spelling of the same
        // producer answers the same ruling (entry canonicalization).
        if let StableProducerId::Definition(producer) = &key.producer {
            let wrapped = AnonymousNominalKey {
                producer: StableProducerId::Function(Box::new(
                    FunctionInstanceKey::Specialization {
                        base: Box::new(FunctionInstanceKey::Definition(*producer)),
                        arguments: CanonicalArguments::default(),
                    },
                )),
                ..key.clone()
            };
            // A Definition-producer key is already canonical, so exercise the
            // wrapper collapse through a Function spelling of a DIFFERENT key:
            // the wrapped form of an uninstalled producer is NOT well-known.
            assert!(!pool.is_well_known_option_identity(&wrapped));
        }
        // An unrelated identity is not well-known.
        assert!(!pool.is_well_known_option_identity(&anon_key(AnonymousNominalKind::Enum, 31, 0)));

        // The demand registry answers the payload lookup with the minted enum.
        assert_eq!(pool.well_known_option_for_payload(Type::I64), Some(minted));
        assert_eq!(pool.well_known_option_for_payload(Type::U32), None);

        // Idempotent: a repeat install dedups onto the same identity.
        pool.install_well_known_option_types(
            std::slice::from_ref(&key),
            &[(DType::I64, DType::AnonymousNominal(key.clone()))],
        )
        .unwrap();
        assert_eq!(pool.well_known_option_identity_count(), 1);
        assert_eq!(pool.find_or_create_anon(&key).unwrap(), minted);
    }

    /// The well-known install accepts a WRAPPER-form identity (the
    /// empty-argument specialization spelling): the ruling and mint both land
    /// under the canonical form, so a canonical-form consult finds them —
    /// entry-enforced wrapper collapse, per the r6b rider.
    #[test]
    fn well_known_option_install_canonicalizes_wrapper_form_identities() {
        use crate::semantic_identity::FunctionInstanceKey;
        let collapsed = AnonymousNominalKey {
            kind: AnonymousNominalKind::Enum,
            producer: StableProducerId::Function(Box::new(FunctionInstanceKey::Definition(40u32))),
            anchor: rue_rir::RirStructuralAnchor::new(vec![
                rue_rir::RirStructuralPathSegment::AnonymousType(0),
            ]),
            arguments: CanonicalArguments::default(),
        };
        let wrapped = AnonymousNominalKey {
            producer: StableProducerId::Function(Box::new(FunctionInstanceKey::Specialization {
                base: Box::new(FunctionInstanceKey::Definition(40u32)),
                arguments: CanonicalArguments::default(),
            })),
            ..collapsed.clone()
        };
        let shape = DurableAnonymousShape::Enum {
            variants: vec![
                (Arc::from("Some"), vec![DType::U32]),
                (Arc::from("None"), vec![]),
            ],
        };
        // The durable universe keys shapes canonically (the source contract).
        let mut pool = anon_pool([], [(collapsed.clone(), shape)], []);

        pool.install_well_known_option_types(
            std::slice::from_ref(&wrapped),
            &[(DType::U32, DType::AnonymousNominal(wrapped.clone()))],
        )
        .unwrap();

        // One identity, answered under BOTH spellings.
        assert_eq!(pool.well_known_option_identity_count(), 1);
        assert!(pool.is_well_known_option_identity(&collapsed));
        assert!(pool.is_well_known_option_identity(&wrapped));
        assert_eq!(
            pool.find_or_create_anon(&collapsed).unwrap(),
            pool.well_known_option_for_payload(Type::U32).unwrap(),
        );
    }

    /// A non-enum durable shape is refused (the pool spelling of the epoch's
    /// `NominalShapeMismatch`) and NOTHING is published for the refused key —
    /// no id, no ruling entry, no registry entry.
    #[test]
    fn well_known_option_install_refuses_non_enum_shape() {
        let key = anon_key(AnonymousNominalKind::Struct, 50, 0);
        let shape = DurableAnonymousShape::Struct {
            fields: vec![(Arc::from("v"), DType::I32)],
            struct_method_names: Vec::new(),
        };
        let mut pool = anon_pool([], [(key.clone(), shape)], []);
        assert_eq!(
            pool.install_well_known_option_types(
                std::slice::from_ref(&key),
                &[(DType::I32, DType::AnonymousNominal(key.clone()))],
            ),
            Err(IdentityMintError::WellKnownShapeMismatch)
        );
        assert_eq!(
            pool.resolve(&DType::AnonymousNominal(key.clone())),
            Err(IdentityMintError::MissingAnonymous),
            "the refused key published no id"
        );
        assert!(!pool.is_well_known_option_identity(&key));
        assert_eq!(pool.well_known_option_identity_count(), 0);
        assert_eq!(pool.well_known_option_for_payload(Type::I32), None);
    }

    /// An identity with no durable shape fails the install closed, exactly as
    /// the epoch's exhausted fixpoint fails with `MissingNominal`.
    #[test]
    fn well_known_option_install_missing_shape_fails_closed() {
        let key = anon_key(AnonymousNominalKind::Enum, 60, 0);
        let mut pool = anon_pool([], [], []);
        assert_eq!(
            pool.install_well_known_option_types(std::slice::from_ref(&key), &[]),
            Err(IdentityMintError::MissingAnonymousShape)
        );
    }

    /// A batch of [valid enum, invalid struct]: the struct refusal fails the
    /// install AND poisons the whole well-known registry, so the valid enum's
    /// mid-batch publication is unobservable — membership false, count zero,
    /// registry answers absent — and a repeat install re-errors with the
    /// recorded refusal instead of re-running the partial install, via the
    /// file's poisoning discipline (`poisoned` / `callable_poisoned`).
    #[test]
    fn well_known_option_install_partial_failure_poisons_registry() {
        let good = anon_key(AnonymousNominalKind::Enum, 80, 0);
        let bad = anon_key(AnonymousNominalKind::Struct, 81, 0);
        let good_shape = DurableAnonymousShape::Enum {
            variants: vec![
                (Arc::from("Some"), vec![DType::I64]),
                (Arc::from("None"), vec![]),
            ],
        };
        let bad_shape = DurableAnonymousShape::Struct {
            fields: vec![(Arc::from("v"), DType::I32)],
            struct_method_names: Vec::new(),
        };
        let mut pool = anon_pool(
            [],
            [(good.clone(), good_shape), (bad.clone(), bad_shape)],
            [],
        );
        assert_eq!(
            pool.install_well_known_option_types(
                &[good.clone(), bad.clone()],
                &[(DType::I64, DType::AnonymousNominal(good.clone()))],
            ),
            Err(IdentityMintError::WellKnownShapeMismatch)
        );
        // No observable partial success: the valid enum DID mint mid-batch
        // through the ordinary anonymous machinery, but the well-known
        // registry answers as if nothing was installed.
        assert!(!pool.is_well_known_option_identity(&good));
        assert_eq!(pool.well_known_option_identity_count(), 0);
        assert_eq!(pool.well_known_option_for_payload(Type::I64), None);
        // A repeat install re-errors with the recorded refusal — even a batch
        // that would succeed on its own cannot resurrect a poisoned registry.
        assert_eq!(
            pool.install_well_known_option_types(std::slice::from_ref(&good), &[]),
            Err(IdentityMintError::WellKnownShapeMismatch),
            "poisoned well-known registry re-errors"
        );
        assert!(!pool.is_well_known_option_identity(&good));
    }

    /// A demand pair naming an `Option` identity the install never minted is
    /// refused: registry-endpoint resolution is LOOKUP-ONLY (the pool analog of
    /// the epoch's `import_export_type` anonymous arm, which refuses an
    /// identity absent from `anon_enum_identities`), so the uninstalled
    /// identity fails closed with `MissingAnonymous` even though its durable
    /// shape exists and a minting arm would silently succeed. The failure
    /// poisons the registry, so the pair recorded before it is unobservable.
    #[test]
    fn well_known_option_install_refuses_uninstalled_registry_identity() {
        let installed = anon_key(AnonymousNominalKind::Enum, 90, 0);
        let uninstalled = anon_key(AnonymousNominalKind::Enum, 91, 0);
        let installed_shape = DurableAnonymousShape::Enum {
            variants: vec![
                (Arc::from("Some"), vec![DType::I64]),
                (Arc::from("None"), vec![]),
            ],
        };
        // The uninstalled identity HAS a durable enum shape: only a
        // lookup-only registry arm refuses it, a minting arm would leak it.
        let uninstalled_shape = DurableAnonymousShape::Enum {
            variants: vec![
                (Arc::from("Some"), vec![DType::U32]),
                (Arc::from("None"), vec![]),
            ],
        };
        let mut pool = anon_pool(
            [],
            [
                (installed.clone(), installed_shape),
                (uninstalled.clone(), uninstalled_shape),
            ],
            [],
        );
        assert_eq!(
            pool.install_well_known_option_types(
                std::slice::from_ref(&installed),
                &[
                    (DType::I64, DType::AnonymousNominal(installed.clone())),
                    (DType::U32, DType::AnonymousNominal(uninstalled.clone())),
                ],
            ),
            Err(IdentityMintError::MissingAnonymous)
        );
        // The refused pair's identity was never minted by the failed resolve —
        // no silent success, no unproduced-import leak.
        assert_eq!(
            pool.resolve(&DType::AnonymousNominal(uninstalled.clone())),
            Err(IdentityMintError::MissingAnonymous),
            "the lookup-only registry arm minted nothing"
        );
        // And nothing partial is observable: the pair recorded before the
        // refusal (and the installed enum's ruling) sit behind the poison.
        assert!(!pool.is_well_known_option_identity(&installed));
        assert_eq!(pool.well_known_option_identity_count(), 0);
        assert_eq!(pool.well_known_option_for_payload(Type::I64), None);
        assert_eq!(
            pool.install_well_known_option_types(std::slice::from_ref(&installed), &[]),
            Err(IdentityMintError::MissingAnonymous),
            "poisoned well-known registry re-errors"
        );
    }

    /// The bounded fixpoint tolerates install order: an enum whose payload
    /// references another well-known nominal installed later in the same batch
    /// is retried after that dependency mints — mirroring the epoch install's
    /// pending loop. BOTH batch orders are run ([outer, inner] exercises the
    /// round-two retry, [inner, outer] the single-pass order) and must agree
    /// on the minted digests/names and the registry answers.
    #[test]
    fn well_known_option_install_fixpoint_orders_nested_payloads() {
        let inner = anon_key(AnonymousNominalKind::Enum, 70, 0);
        let outer = anon_key(AnonymousNominalKind::Enum, 71, 0);
        let inner_shape = DurableAnonymousShape::Enum {
            variants: vec![
                (Arc::from("Some"), vec![DType::I32]),
                (Arc::from("None"), vec![]),
            ],
        };
        let outer_shape = DurableAnonymousShape::Enum {
            variants: vec![
                (
                    Arc::from("Some"),
                    vec![DType::AnonymousNominal(inner.clone())],
                ),
                (Arc::from("None"), vec![]),
            ],
        };
        // One fresh pool per batch order; returns the digest-bearing names so
        // the orders can be asserted identical across pools.
        let run = |batch: &[AnonKey]| -> (String, String) {
            let mut pool = anon_pool(
                [],
                [
                    (inner.clone(), inner_shape.clone()),
                    (outer.clone(), outer_shape.clone()),
                ],
                [],
            );
            pool.install_well_known_option_types(
                batch,
                &[
                    (DType::I32, DType::AnonymousNominal(inner.clone())),
                    (
                        DType::AnonymousNominal(inner.clone()),
                        DType::AnonymousNominal(outer.clone()),
                    ),
                ],
            )
            .unwrap();
            assert_eq!(pool.well_known_option_identity_count(), 2);
            assert!(pool.is_well_known_option_identity(&outer));
            assert!(pool.is_well_known_option_identity(&inner));
            let inner_ty = pool.find_or_create_anon(&inner).unwrap();
            let outer_ty = pool.find_or_create_anon(&outer).unwrap();
            let outer_def = pool.type_pool().enum_def(outer_ty.as_enum().unwrap());
            assert_eq!(outer_def.variant_payload(0), &[inner_ty]);
            // The registry answers both demand pairs with the minted enums.
            assert_eq!(
                pool.well_known_option_for_payload(Type::I32),
                Some(inner_ty)
            );
            assert_eq!(pool.well_known_option_for_payload(inner_ty), Some(outer_ty));
            let inner_def = pool.type_pool().enum_def(inner_ty.as_enum().unwrap());
            (inner_def.name.to_string(), outer_def.name.to_string())
        };
        // OUTER first: its payload consult blocks on the not-yet-minted inner
        // enum in round one and succeeds in round two.
        let outer_first = run(&[outer.clone(), inner.clone()]);
        // INNER first: every mint lands in round one.
        let inner_first = run(&[inner.clone(), outer.clone()]);
        assert_eq!(
            outer_first, inner_first,
            "batch order must not change the minted digests/names or registry answers"
        );
    }

    /// `Str(N)` for a literal capacity mints the generated fixed-capacity string
    /// struct byte-identically to the epoch's `get_or_create_str_fixed_struct`,
    /// and dedups by name on repeat.
    #[test]
    fn str_fixed_arm_mints_and_dedups() {
        let mut pool = pool([]);
        let str8 = pool.get_or_create_str_fixed(8);
        let id = str8.as_struct().unwrap();
        let def = pool.type_pool().struct_def(id);
        assert_eq!(&*def.name, "Str(8)");
        assert!(def.is_copy);
        assert!(def.is_builtin);
        assert!(def.is_pub);
        assert_eq!(def.fields.len(), 2);
        assert_eq!(def.fields[0].name, "ptr");
        assert_eq!(def.fields[1].name, "len");
        assert_eq!(def.fields[1].ty, Type::U64);
        // Same capacity dedups; a different capacity mints a distinct struct.
        assert_eq!(pool.get_or_create_str_fixed(8), str8, "repeat dedups");
        let str16 = pool.get_or_create_str_fixed(16);
        assert_ne!(str16, str8);
        assert_eq!(
            &*pool.type_pool().struct_def(str16.as_struct().unwrap()).name,
            "Str(16)"
        );
    }

    #[test]
    fn missing_and_deferred_arms_fail_closed() {
        let mut pool = pool([]);
        assert_eq!(
            pool.resolve(&DType::Nominal(7)),
            Err(IdentityMintError::MissingNominal)
        );
        assert_eq!(
            pool.resolve(&DType::Module(Arc::from("pkg/m.rue"))),
            Err(IdentityMintError::Deferred("module identity"))
        );
        assert_eq!(
            pool.resolve(&DType::GenericParameter(0)),
            Err(IdentityMintError::Deferred("generic parameter"))
        );
    }

    #[test]
    fn slice_arm_mints_generated_struct_and_dedups() {
        // The Slice arm is the one resolve arm that mints a fresh struct
        // (mirroring `import_type_local`); pin its registration byte-for-byte.
        let mut pool = pool([]);
        let slice = pool
            .resolve(&DType::Slice {
                name: Arc::from("__slice_i64"),
                element: Box::new(DType::I64),
            })
            .unwrap();
        let id = slice.as_struct().unwrap();
        let def = pool.type_pool().struct_def(id);
        assert_eq!(&*def.name, "__slice_i64");
        assert!(def.is_copy, "slice headers are copy");
        assert!(def.is_builtin, "slice structs register as builtin");
        assert_eq!(def.fields.len(), 2, "ptr + len: {:?}", def.fields);
        assert_eq!(def.fields[0].name, "ptr");
        assert_eq!(def.fields[1].name, "len");
        let again = pool
            .resolve(&DType::Slice {
                name: Arc::from("__slice_i64"),
                element: Box::new(DType::I64),
            })
            .unwrap();
        assert_eq!(slice, again, "repeat slice consult dedups");
    }

    #[test]
    fn nested_field_nominal_symbol_is_module_qualified_per_field() {
        // Two same-named `Handle` nominals in DIFFERENT modules must mint
        // distinct ids whose qualified symbols carry their own module
        // components — the render/is_copy mirrors cannot see this, so assert
        // through the production `struct_symbol_name` per FIELD nominal.
        let mut pool = pool([
            (
                0,
                named(
                    "Owner",
                    "pkg/owner.rue",
                    true,
                    struct_body(
                        vec![("a", DType::Nominal(1)), ("b", DType::Nominal(2))],
                        false,
                        false,
                    ),
                ),
            ),
            (
                1,
                named(
                    "Handle",
                    "pkg/alpha.rue",
                    true,
                    struct_body(vec![], true, false),
                ),
            ),
            (
                2,
                named(
                    "Handle",
                    "pkg/beta.rue",
                    true,
                    struct_body(vec![], true, false),
                ),
            ),
        ]);
        let owner = pool.resolve(&DType::Nominal(0)).unwrap();
        let owner_def = pool.type_pool().struct_def(owner.as_struct().unwrap());
        let a_id = owner_def.fields[0].ty.as_struct().unwrap();
        let b_id = owner_def.fields[1].ty.as_struct().unwrap();
        assert_ne!(
            a_id, b_id,
            "same-named nominals from distinct modules stay distinct"
        );
        let a_symbol = pool.type_pool().struct_symbol_name(a_id);
        let b_symbol = pool.type_pool().struct_symbol_name(b_id);
        assert_ne!(
            a_symbol, b_symbol,
            "field nominal symbols carry their own module component"
        );
        assert!(
            a_symbol.contains('$') && b_symbol.contains('$'),
            "{a_symbol} / {b_symbol}"
        );
    }

    #[test]
    fn provider_module_paths_publish_once_with_explicit_and_assigned_ids() {
        const MODULES: Key = 256;

        let nominals = (0..MODULES).map(|key| {
            let name = format!("Type{key}");
            let module = format!("pkg/module_{key}.rue");
            let mut nominal = named(&name, &module, true, struct_body(Vec::new(), false, false));
            // Destructor spelling consults the path during minting, pinning
            // immediate publication rather than merely the final registry.
            nominal.has_destructor = true;
            (key, nominal)
        });
        let mut provider = source(nominals);
        provider.nominal_file_ids = (0..MODULES)
            .step_by(2)
            .map(|key| (key, FileId::new(key + 1)))
            .collect();
        let mut pool = BodyIdentityPool::new(provider, Rc::new(ThreadedRodeo::new()));

        for key in 0..MODULES {
            let ty = pool.resolve_provider_type(&DType::Nominal(key)).unwrap();
            let id = ty.as_struct().unwrap();
            let def = pool.type_pool().struct_def(id);
            let expected_file = FileId::new(key + 1);
            let expected_path = format!("pkg/module_{key}.rue");
            assert_eq!(def.file_id, expected_file);
            assert_eq!(
                pool.file_modules.get(&expected_file).map(Arc::as_ref),
                Some(expected_path.as_str())
            );
            let path_component = crate::path_norm::mangle_symbol_component(
                &crate::path_norm::normalize_module_path(&expected_path),
            );
            assert_eq!(
                pool.type_pool().struct_symbol_name(id),
                format!("Type{key}${path_component}")
            );
        }

        assert_eq!(pool.module_files.len(), MODULES as usize);
        assert_eq!(pool.file_modules.len(), MODULES as usize);
        assert_eq!(pool.next_module_file, MODULES + 1);
    }

    #[test]
    fn provider_module_file_collision_fails_before_nominal_minting() {
        let mut provider = source([
            (
                0,
                named(
                    "First",
                    "pkg/first.rue",
                    true,
                    struct_body(Vec::new(), false, false),
                ),
            ),
            (
                1,
                named(
                    "Second",
                    "pkg/second.rue",
                    true,
                    struct_body(Vec::new(), false, false),
                ),
            ),
        ]);
        provider.nominal_file_ids = AHashMap::from([(0, FileId::new(41)), (1, FileId::new(41))]);
        let mut pool = BodyIdentityPool::new(provider, Rc::new(ThreadedRodeo::new()));

        pool.resolve_provider_type(&DType::Nominal(0)).unwrap();
        assert_eq!(
            pool.resolve_provider_type(&DType::Nominal(1)),
            Err(IdentityMintError::InvalidStructuralType)
        );
        assert_eq!(pool.module_files.len(), 1);
        assert_eq!(pool.file_modules.len(), 1);
        assert!(!pool.struct_ids.contains_key(&1));
    }

    #[test]
    fn repr_c_registers_like_the_epoch_shell_phase() {
        let mut repr = named(
            "Raw",
            "pkg/ffi.rue",
            true,
            struct_body(vec![("x", DType::I64)], true, false),
        );
        repr.is_repr_c = true;
        let mut pool = pool([
            (0, repr),
            (
                1,
                named(
                    "Plain",
                    "pkg/ffi.rue",
                    true,
                    struct_body(vec![("x", DType::I64)], true, false),
                ),
            ),
        ]);
        let raw = pool
            .resolve(&DType::Nominal(0))
            .unwrap()
            .as_struct()
            .unwrap();
        let plain = pool
            .resolve(&DType::Nominal(1))
            .unwrap()
            .as_struct()
            .unwrap();
        assert!(pool.type_pool().is_struct_repr_c(raw));
        assert!(!pool.type_pool().is_struct_repr_c(plain));
    }

    #[test]
    fn failed_mint_poisons_the_key_and_repeat_consult_reerrors() {
        // A field that cannot resolve (generic parameter is a deferred arm)
        // fails the mint AFTER the shell pre-registered. The repeat consult
        // must re-error — never hand out the incomplete shell, whose
        // `struct_def` read would panic.
        let mut pool = pool([(
            0,
            named(
                "Broken",
                "pkg/broken.rue",
                true,
                struct_body(vec![("bad", DType::GenericParameter(0))], false, false),
            ),
        )]);
        let first = pool.resolve(&DType::Nominal(0));
        assert_eq!(first, Err(IdentityMintError::Deferred("generic parameter")));
        let second = pool.resolve(&DType::Nominal(0));
        assert_eq!(
            second,
            Err(IdentityMintError::Deferred("generic parameter")),
            "poisoned key re-errors"
        );
    }

    // ----- Callable identity family (r4a-2b) ---------------------------------

    #[test]
    fn function_signature_matches_epoch_twin() {
        // A three-parameter function (by-value, borrow, comptime-value) with an
        // i64 return, built through the pool and through the epoch primitives.
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("i64"));

        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![
                        param("a", DType::I32, SemanticParameterMode::Value, false),
                        param("b", DType::Bool, SemanticParameterMode::Borrow, false),
                        param("n", DType::U64, SemanticParameterMode::Value, true),
                    ],
                    DType::I64,
                    true,
                    false,
                ),
            )],
            [],
        );
        let info = pool.resolve_function(&0, handle).unwrap();

        // Epoch twin: the same params allocated through `ParamArena::alloc`.
        let twin_pool = TypeInternPool::new();
        let twin_interner = ThreadedRodeo::new();
        let mut twin_arena = ParamArena::new();
        let twin_range = twin_arena.alloc(
            [
                twin_interner.get_or_intern("a"),
                twin_interner.get_or_intern("b"),
                twin_interner.get_or_intern("n"),
            ],
            [Type::I32, Type::BOOL, Type::U64],
            [
                RirParamMode::Normal,
                RirParamMode::Borrow,
                RirParamMode::Normal,
            ],
            [false, false, true],
        );

        // Durable-derived fields.
        assert_param_range_equal(
            &pool,
            info.params,
            &twin_arena,
            &twin_interner,
            twin_range,
            &twin_pool,
        );
        assert_eq!(
            render(pool.type_pool(), info.return_type),
            render(&twin_pool, Type::I64)
        );
        assert!(
            info.is_generic,
            "a comptime-value param marks the fn generic"
        );
        assert!(info.is_pub);
        assert!(!info.is_unchecked);

        // Request/RIR passthrough: exactly the handle, nothing fabricated.
        assert_eq!(info.body, handle.body);
        assert_eq!(info.declaration, handle.declaration);
        assert_eq!(info.span, handle.span);
        assert_eq!(info.return_type_syntax, handle.return_type_syntax);
        assert_eq!(info.is_extern, handle.is_extern);
        assert_eq!(info.is_c_export, handle.is_c_export);
        assert_eq!(info.allow_unused_function, handle.allow_unused_function);
        assert_eq!(info.allow_unused_variable, handle.allow_unused_variable);
        assert_eq!(info.allow_unreachable_code, handle.allow_unreachable_code);
        assert_eq!(info.file_id, handle.file_id);
    }

    #[test]
    fn function_signature_mints_params_once_and_dedups() {
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![param("x", DType::I32, SemanticParameterMode::Value, false)],
                    DType::Unit,
                    false,
                    false,
                ),
            )],
            [],
        );

        let before = pool.param_arena().total_params();
        let first = pool.resolve_function(&0, handle).unwrap();
        let after_first = pool.param_arena().total_params();
        let second = pool.resolve_function(&0, handle).unwrap();
        assert_eq!(
            first.params, second.params,
            "repeat consult returns the same ParamRange"
        );
        assert_eq!(
            pool.param_arena().total_params(),
            after_first,
            "repeat consult interns no new params"
        );
        assert!(after_first > before, "first consult interned params");
    }

    #[test]
    fn supplied_function_signature_skips_source_read_and_matches_cached_path() {
        let function = durable_function(
            vec![param(
                "value",
                DType::I64,
                SemanticParameterMode::Borrow,
                false,
            )],
            DType::Bool,
            true,
            true,
        );
        let returns_type = false;
        let file = FileId::new(17);

        let supplied_reads = Rc::new(Cell::new(0));
        let mut supplied_source = source([]);
        supplied_source.functions.insert(0, function.clone());
        supplied_source.function_reads = Rc::clone(&supplied_reads);
        let mut supplied_pool =
            BodyIdentityPool::new(supplied_source, Rc::new(ThreadedRodeo::new()));
        let first = supplied_pool
            .resolve_function_call_from(&0, &function, returns_type, file)
            .unwrap();
        let second = supplied_pool
            .resolve_function_call_from(&0, &function, returns_type, file)
            .unwrap();
        let cached_source_path = supplied_pool
            .resolve_function_call(&0, returns_type, file)
            .unwrap();
        assert_eq!(
            supplied_reads.get(),
            0,
            "supplied and cached signatures never re-read the durable source"
        );
        assert_eq!(first.params, second.params);
        assert_eq!(first.params, cached_source_path.params);

        let ordinary_reads = Rc::new(Cell::new(0));
        let mut ordinary_source = source([]);
        ordinary_source.functions.insert(0, function);
        ordinary_source.function_reads = Rc::clone(&ordinary_reads);
        let mut ordinary_pool =
            BodyIdentityPool::new(ordinary_source, Rc::new(ThreadedRodeo::new()));
        let ordinary = ordinary_pool
            .resolve_function_call(&0, returns_type, file)
            .unwrap();
        assert_eq!(ordinary_reads.get(), 1);
        assert_eq!(
            (
                first.params,
                first.return_type,
                first.returns_type,
                first.is_generic,
                first.is_pub,
                first.is_unchecked,
                first.is_extern,
                first.file_id,
            ),
            (
                ordinary.params,
                ordinary.return_type,
                ordinary.returns_type,
                ordinary.is_generic,
                ordinary.is_pub,
                ordinary.is_unchecked,
                ordinary.is_extern,
                ordinary.file_id,
            ),
        );
    }

    #[test]
    fn supplied_function_signature_preserves_poison_without_source_reads() {
        let broken = durable_function(
            vec![param(
                "value",
                DType::GenericParameter(0),
                SemanticParameterMode::Value,
                false,
            )],
            DType::Unit,
            false,
            false,
        );
        let valid = durable_function(Vec::new(), DType::Unit, false, false);
        let reads = Rc::new(Cell::new(0));
        let mut durable_source = source([]);
        durable_source.functions.insert(0, broken.clone());
        durable_source.function_reads = Rc::clone(&reads);
        let mut pool = BodyIdentityPool::new(durable_source, Rc::new(ThreadedRodeo::new()));
        let returns_type = false;
        let file = FileId::new(19);

        let first = pool
            .resolve_function_call_from(&0, &broken, returns_type, file)
            .unwrap_err();
        let params_after_failure = pool.param_arena().total_params();
        let second = pool
            .resolve_function_call_from(&0, &valid, returns_type, file)
            .unwrap_err();
        let ordinary = pool
            .resolve_function_call(&0, returns_type, file)
            .unwrap_err();
        assert_eq!(first, IdentityMintError::Deferred("generic parameter"));
        assert_eq!(
            second, first,
            "the supplied path replays the recorded poison"
        );
        assert_eq!(ordinary, first, "the source path observes the same poison");
        assert_eq!(reads.get(), 0, "poisoned repeats do not read the source");
        assert_eq!(pool.param_arena().total_params(), params_after_failure);
    }

    #[test]
    fn method_signature_matches_epoch_twin() {
        // `fn (self, delta: i32) -> bool` on `Widget`.
        let mut pool = callable_pool(
            [(
                1,
                named(
                    "Widget",
                    "pkg/ui.rue",
                    true,
                    struct_body(vec![("id", DType::U32)], false, false),
                ),
            )],
            [],
            [(
                0,
                durable_method(
                    DType::Nominal(1),
                    vec![param(
                        "delta",
                        DType::I32,
                        SemanticParameterMode::Value,
                        false,
                    )],
                    DType::Bool,
                    true,
                    SemanticParameterMode::Borrow,
                ),
            )],
        );
        let handle = method_handle();
        let info = pool.resolve_method(&0, handle).unwrap();

        // Twin: the same nominal + params through the epoch primitives.
        let (twin, twin_interner) = twin_pool("pkg/ui.rue");
        let twin_id = twin_declare_struct(
            &twin,
            &twin_interner,
            "Widget",
            false,
            false,
            true,
            vec![("id", Type::U32)],
            None,
        );
        let mut twin_arena = ParamArena::new();
        let twin_range = twin_arena.alloc(
            [twin_interner.get_or_intern("delta")],
            [Type::I32],
            [RirParamMode::Normal],
            [false],
        );

        // Durable-derived: receiver, has_self, receiver mode, return type,
        // params.
        assert_eq!(
            render(pool.type_pool(), info.struct_type),
            render(&twin, Type::new_struct(twin_id))
        );
        assert_eq!(
            pool.type_pool()
                .struct_symbol_name(info.struct_type.as_struct().unwrap()),
            twin.struct_symbol_name(twin_id),
            "receiver symbol name parity"
        );
        assert!(info.has_self);
        assert_eq!(info.self_mode, RirParamMode::Borrow);
        assert_eq!(
            render(pool.type_pool(), info.return_type),
            render(&twin, Type::BOOL)
        );
        assert_param_range_equal(
            &pool,
            info.params,
            &twin_arena,
            &twin_interner,
            twin_range,
            &twin,
        );

        // Request/RIR passthrough.
        assert_eq!(info.body, handle.body);
        assert_eq!(info.span, handle.span);
        assert_eq!(info.self_is_mut, handle.self_is_mut);
    }

    #[test]
    fn parameter_modes_map_to_rir_modes() {
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![
                        param("v", DType::I32, SemanticParameterMode::Value, false),
                        param("b", DType::I32, SemanticParameterMode::Borrow, false),
                        param("i", DType::I32, SemanticParameterMode::Inout, false),
                    ],
                    DType::Unit,
                    false,
                    false,
                ),
            )],
            [],
        );
        let info = pool.resolve_function(&0, handle).unwrap();
        assert_eq!(
            pool.param_arena().modes(info.params),
            &[
                RirParamMode::Normal,
                RirParamMode::Borrow,
                RirParamMode::Inout
            ]
        );
        assert!(!info.is_generic, "no comptime param means non-generic");
    }

    #[test]
    fn parameter_nominal_type_resolves_and_dedups_through_pool() {
        // A parameter typed by a named nominal resolves through the same 2a
        // machinery and reuses an already-minted id (compose, don't duplicate).
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool(
            [(
                5,
                named(
                    "Cell",
                    "pkg/c.rue",
                    true,
                    struct_body(vec![("v", DType::I32)], true, false),
                ),
            )],
            [(
                0,
                durable_function(
                    vec![param(
                        "cell",
                        DType::Nominal(5),
                        SemanticParameterMode::Value,
                        false,
                    )],
                    DType::Unit,
                    true,
                    false,
                ),
            )],
            [],
        );
        // Mint the nominal directly, then assert the param reuses its id.
        let direct = pool.resolve(&DType::Nominal(5)).unwrap();
        let len_after_nominal = pool.type_pool().len();
        let info = pool.resolve_function(&0, handle).unwrap();
        let param_ty = pool.param_arena().types(info.params)[0];
        assert_eq!(param_ty, direct, "param nominal reuses the minted id");
        assert_eq!(
            pool.type_pool().len(),
            len_after_nominal,
            "param resolution mints no new nominal"
        );
        assert_eq!(render(pool.type_pool(), param_ty), "Cell");
    }

    #[test]
    fn generic_parameter_type_refuses_and_poisons_callable() {
        // A parameter typed by a generic parameter is a deferred 2a arm: the
        // callable refuses (never approximates) and the key is poisoned.
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![param(
                        "x",
                        DType::GenericParameter(0),
                        SemanticParameterMode::Value,
                        false,
                    )],
                    DType::Unit,
                    false,
                    false,
                ),
            )],
            [],
        );
        assert_eq!(
            pool.resolve_function(&0, handle).unwrap_err(),
            IdentityMintError::Deferred("generic parameter")
        );
        assert_eq!(
            pool.resolve_function(&0, handle).unwrap_err(),
            IdentityMintError::Deferred("generic parameter"),
            "poisoned callable re-errors"
        );
    }

    #[test]
    fn return_type_refusal_poisons_after_params_interned() {
        // Params resolve (and intern into the arena), but the return type is a
        // deferred arm. The key poisons, and the repeat consult short-circuits
        // without re-interning the orphaned params — poison-on-partial-failure.
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("t"));
        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![param("x", DType::I32, SemanticParameterMode::Value, false)],
                    DType::GenericParameter(0),
                    false,
                    false,
                ),
            )],
            [],
        );
        assert_eq!(
            pool.resolve_function(&0, handle).unwrap_err(),
            IdentityMintError::Deferred("generic parameter")
        );
        let total_after_first = pool.param_arena().total_params();
        assert_eq!(
            total_after_first, 1,
            "the param interned before the return-type failure"
        );
        assert_eq!(
            pool.resolve_function(&0, handle).unwrap_err(),
            IdentityMintError::Deferred("generic parameter"),
            "poisoned callable re-errors"
        );
        assert_eq!(
            pool.param_arena().total_params(),
            total_after_first,
            "poison short-circuits: no re-interning of orphaned params"
        );
    }

    #[test]
    fn missing_callable_key_fails_closed() {
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool([], [], []);
        assert_eq!(
            pool.resolve_function(&9, handle).unwrap_err(),
            IdentityMintError::MissingCallable
        );
        assert_eq!(
            pool.resolve_method(&9, method_handle()).unwrap_err(),
            IdentityMintError::MissingCallable
        );
    }

    // ----- Const identity family --------------------------------------------

    fn durable_const(
        is_public: bool,
        ty: DType,
        value: SemanticImportConstValue<Key, Module>,
    ) -> DurableConst<Key, Module> {
        DurableConst {
            is_public,
            ty,
            value,
        }
    }

    #[test]
    fn const_info_mints_once_and_assembles_request_span() {
        let mut pool = const_pool(
            [(
                1,
                named(
                    "Point",
                    "pkg/main.rue",
                    true,
                    struct_body(vec![("x", DType::I32)], false, false),
                ),
            )],
            [(2, durable_function(Vec::new(), DType::I32, false, false))],
            [
                (
                    10,
                    durable_const(
                        true,
                        DType::ComptimeType,
                        SemanticImportConstValue::Type(DType::Nominal(1)),
                    ),
                ),
                (
                    11,
                    durable_const(
                        false,
                        DType::ComptimeType,
                        SemanticImportConstValue::Function(2),
                    ),
                ),
            ],
        );

        let first = pool
            .resolve_const(
                &10,
                ConstIdentityHandle {
                    span: Span::with_file(FileId::new(7), 3, 9),
                },
            )
            .unwrap();
        let second = pool
            .resolve_const(
                &10,
                ConstIdentityHandle {
                    span: Span::with_file(FileId::new(7), 20, 25),
                },
            )
            .unwrap();
        assert!(first.is_pub);
        assert_eq!(first.ty, Type::COMPTIME_TYPE);
        let ConstValue::Type(first_ty) = first.value else {
            panic!("type-valued const");
        };
        let ConstValue::Type(second_ty) = second.value else {
            panic!("type-valued const");
        };
        assert_eq!(first_ty, second_ty, "repeat consult re-minted the type");
        assert_eq!(render(pool.type_pool(), first_ty), "Point");
        assert_ne!(
            first.span, second.span,
            "the cached durable subset does not capture a request span"
        );

        let callable = pool
            .resolve_const(
                &11,
                ConstIdentityHandle {
                    span: Span::new(0, 1),
                },
            )
            .unwrap();
        let ConstValue::Function(symbol) = callable.value else {
            panic!("function-valued const");
        };
        assert_eq!(pool.resolve_symbol(symbol), "fn2");
    }

    #[test]
    fn const_partial_failure_poisons_and_never_mints_anonymous() {
        let anon = anon_key(AnonymousNominalKind::Struct, 5, 0);
        let mut pool = const_pool(
            [(
                1,
                named(
                    "Shell",
                    "pkg/main.rue",
                    false,
                    struct_body(Vec::new(), false, false),
                ),
            )],
            [],
            [(
                10,
                durable_const(
                    false,
                    DType::Nominal(1),
                    SemanticImportConstValue::Type(DType::AnonymousNominal(anon)),
                ),
            )],
        );
        let handle = ConstIdentityHandle {
            span: Span::new(0, 1),
        };
        assert_eq!(
            pool.resolve_const(&10, handle).unwrap_err(),
            IdentityMintError::MissingAnonymous
        );
        assert!(
            pool.struct_ids.contains_key(&1),
            "the declared const type minted before its value failed"
        );
        assert!(
            pool.anon_nominals.is_empty(),
            "const assembly never calls anonymous minting"
        );

        // Even replacing the source record cannot revive a poisoned key: a
        // repeat consult re-errors instead of publishing partial prior state.
        pool.source.consts.insert(
            10,
            durable_const(false, DType::I32, SemanticImportConstValue::Integer(1)),
        );
        assert_eq!(
            pool.resolve_const(&10, handle).unwrap_err(),
            IdentityMintError::MissingAnonymous
        );
    }

    #[test]
    fn missing_const_record_stops_without_approximation() {
        let mut pool = const_pool([], [], []);
        assert_eq!(
            pool.resolve_const(
                &99,
                ConstIdentityHandle {
                    span: Span::new(0, 1),
                },
            )
            .unwrap_err(),
            IdentityMintError::MissingConst
        );
    }

    // ----- RIR-index answer surface (r4a-2c) ---------------------------------
    //
    // Twin-parity for the endpoint seam ops. Each test builds a real
    // program's `Rir` through the production lex/parse/astgen path, binds its
    // declarations through the epoch, and compares the pool-side `BodyRirIndex`
    // answers against the epoch host over the SAME `Rir` — then the
    // capstone assembles a complete `FunctionInfo` / `MethodInfo` from the index
    // (2c) + a durable signature (2b, resolving types through 2a) and proves it
    // equals production's populated info struct field-for-field.

    /// Lower one source file to `Rir` through the production frontend.
    fn lower_rir(source: &str, file_id: FileId) -> (Rir, ThreadedRodeo) {
        use rue_lexer::Lexer;
        use rue_parser::Parser;
        use rue_rir::AstGen;
        let interner = ThreadedRodeo::default();
        let lexer = Lexer::with_interner_and_file_id(source, interner, file_id);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        (rir, interner)
    }

    /// True when a declaration's directive list carries `@allow(<warning>)` —
    /// the same read `binding_manifest.rs` performs when filling the handle.
    fn rir_has_allow<'r>(
        interner: &ThreadedRodeo,
        mut directives: impl Iterator<Item = rue_rir::RirDirectiveView<'r>>,
        warning_name: &str,
    ) -> bool {
        let allow_sym = interner.get("allow");
        let warning_sym = interner.get(warning_name);
        directives.any(|directive| {
            Some(directive.name) == allow_sym
                && directive.args.iter().any(|arg| Some(*arg) == warning_sym)
        })
    }

    /// True when the return syntax names exactly `name`.
    fn rir_type_named(
        rir: &Rir,
        interner: &ThreadedRodeo,
        syntax: rue_rir::RirTypeSyntaxRef,
        name: &str,
    ) -> bool {
        let arena = rir.type_syntax();
        let Some(rue_rir::RirTypeSyntaxNode::Named(symbol)) = arena.node(syntax) else {
            return false;
        };
        arena
            .symbol(*symbol)
            .is_some_and(|symbol| interner.resolve(symbol) == name)
    }

    /// Fill a [`FunctionIdentityHandle`] from a free-function declaration the
    /// RIR index located: exactly the request/RIR reads production performs
    /// (`binding_manifest.rs`) — `body` and the `@allow` flags off the RIR, the
    /// pre-resolution return symbol, the RIR-only `is_extern`/`is_c_export`.
    fn fn_handle_from_rir(
        rir: &Rir,
        interner: &ThreadedRodeo,
        declaration: InstRef,
    ) -> FunctionIdentityHandle {
        let inst = rir.get(declaration);
        let InstData::FnDecl {
            body,
            return_type,
            is_extern,
            is_c_export,
            directives,
            ..
        } = &inst.data
        else {
            panic!("free-function declaration must be a FnDecl");
        };
        let dirs = rir.directives(directives);
        FunctionIdentityHandle {
            body: *body,
            declaration,
            span: inst.span,
            return_type_syntax: *return_type,
            returns_type: rir_type_named(rir, interner, *return_type, "type"),
            is_extern: *is_extern,
            is_c_export: *is_c_export,
            allow_unused_function: rir_has_allow(interner, dirs.iter(), "unused_function"),
            allow_unused_variable: rir_has_allow(interner, dirs.iter(), "unused_variable"),
            allow_unreachable_code: rir_has_allow(interner, dirs.iter(), "unreachable_code"),
            file_id: inst.span.file_id,
        }
    }

    /// Fill a [`MethodIdentityHandle`] from a method declaration the RIR index
    /// located: `self_is_mut` is a body-local RIR `FnDecl` fact; `body` and
    /// `span` are request-carried.
    fn method_handle_from_rir(rir: &Rir, declaration: InstRef) -> MethodIdentityHandle {
        let inst = rir.get(declaration);
        let InstData::FnDecl {
            body,
            self_is_mut,
            returns_borrow,
            ..
        } = &inst.data
        else {
            panic!("method declaration must be a FnDecl");
        };
        MethodIdentityHandle {
            body: *body,
            span: inst.span,
            self_is_mut: *self_is_mut,
            returns_borrow: *returns_borrow,
        }
    }

    #[test]
    fn body_rir_index_supplemental_counters_are_attribution_only() {
        let file = FileId::new(4);
        let source = r#"
            const LIMIT: i64 = 7;
            pub struct Widget {
                id: u32,
                fn bump(self, delta: i32) -> u32 { self.id }
            }
        "#;
        let (rir, _) = lower_rir(source, file);
        let (_, ordinary) = BodyRirIndex::new_with_attribution(&rir, false);
        let (_, attributed) = BodyRirIndex::new_with_attribution(&rir, true);

        assert_eq!(ordinary.declaration_index, attributed.declaration_index);
        assert_eq!(ordinary.shell_declarations_visited, 0);
        assert_eq!(ordinary.named_methods_indexed, 0);
        assert_eq!(ordinary.const_declarations_indexed, 0);
        assert!(attributed.shell_declarations_visited > 0);
        assert_eq!(attributed.named_methods_indexed, 1);
        assert_eq!(attributed.const_declarations_indexed, 1);
    }

    #[test]
    fn body_rir_index_const_declaration_is_file_and_name_keyed() {
        let file = FileId::new(4);
        let source = r#"
            const LIMIT: i64 = 7;
            const FLAG: bool = true;
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = lower_rir(source, file);
        let index = BodyRirIndex::new(&rir);

        for name in ["LIMIT", "FLAG"] {
            let symbol = interner.get(name).unwrap();
            let declaration = index
                .const_declaration(file, symbol)
                .unwrap_or_else(|| panic!("{name} has a const declaration"));
            assert!(matches!(
                rir.get(declaration).data,
                InstData::ConstDecl { .. }
            ));
            assert_eq!(
                rir.get(declaration).span.file_id,
                file,
                "the located declaration belongs to the requested file"
            );
        }

        let limit = interner.get("LIMIT").unwrap();
        assert_eq!(
            index.const_declaration(FileId::new(99), limit),
            None,
            "the same name in a different file does not alias"
        );
        assert_eq!(
            interner
                .get("MISSING")
                .and_then(|name| index.const_declaration(file, name)),
            None
        );
    }

    #[test]
    fn provider_assembles_function_info_composing_2a_2b_2c() {
        // The pool-arc capstone: a provider assembles a complete `FunctionInfo`
        // from the RIR index (2c handle) + the durable signature (2b, whose
        // `Point` parameter resolves through 2a), against explicit expectations
        // for every field class.
        let file = FileId::new(7);
        let source = r#"
            pub struct Point { x: i64, y: i64 }
            @allow(unused_function)
            pub fn make(p: Point, n: i32) -> i64 { 0 }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = lower_rir(source, file);
        let index = BodyRirIndex::new(&rir);

        // 2c: the RIR index locates the declaration; fill the request/RIR handle.
        let make_src = interner.get("make").unwrap();
        let declaration = index.first_free_function(make_src, file).unwrap();
        let handle = fn_handle_from_rir(&rir, &interner, declaration);

        // 2b + 2a: mint the durable-signature subset from a durable source.
        let mut pool = callable_pool(
            [(
                1,
                named(
                    "Point",
                    "pkg/main.rue",
                    true,
                    struct_body(vec![("x", DType::I64), ("y", DType::I64)], false, false),
                ),
            )],
            [(
                0,
                durable_function(
                    vec![
                        param("p", DType::Nominal(1), SemanticParameterMode::Value, false),
                        param("n", DType::I32, SemanticParameterMode::Value, false),
                    ],
                    DType::I64,
                    true,
                    false,
                ),
            )],
            [],
        );
        let info = pool.resolve_function(&0, handle).unwrap();

        // 2c fields: verbatim RIR passthrough, checked against the RIR facts.
        let decl_inst = rir.get(declaration);
        let InstData::FnDecl { body, .. } = decl_inst.data else {
            panic!("free-function declaration must be a FnDecl");
        };
        assert_eq!(info.body, body);
        assert_eq!(info.declaration, declaration);
        assert_eq!(info.span, decl_inst.span);
        assert!(!info.is_extern);
        assert!(!info.is_c_export);
        assert!(
            info.allow_unused_function,
            "the @allow(unused_function) flag flowed through the handle"
        );
        assert!(!info.allow_unused_variable);
        assert!(!info.allow_unreachable_code);
        assert_eq!(info.file_id, file);

        // 2b / 2a fields: durable-derived.
        assert!(!info.is_generic);
        assert!(info.is_pub);
        assert!(!info.is_unchecked);
        assert_eq!(render(pool.type_pool(), info.return_type), "i64");
        let arena = pool.param_arena();
        assert_eq!(info.params.len(), 2);
        assert_eq!(
            [
                pool.resolve_symbol(arena.names(info.params)[0]),
                pool.resolve_symbol(arena.names(info.params)[1]),
            ],
            ["p", "n"]
        );
        // The `Point` parameter resolved through 2a to a nominal.
        assert_eq!(
            render(pool.type_pool(), arena.types(info.params)[0]),
            "Point"
        );
        assert_eq!(render(pool.type_pool(), arena.types(info.params)[1]), "i32");
        assert_eq!(
            arena.modes(info.params),
            [RirParamMode::Normal, RirParamMode::Normal]
        );
        assert_eq!(arena.comptime(info.params), [false, false]);
    }

    #[test]
    fn provider_assembles_method_info_composing_2a_2b_2c() {
        // The method twin of the capstone: assemble a complete `MethodInfo` from
        // the RIR index (2c) + durable method signature (2b, receiver `Widget`
        // through 2a) against explicit expectations.
        let file = FileId::new(3);
        let source = r#"
            pub struct Widget {
                id: u32,
                fn bump(self, delta: i32) -> u32 { self.id }
            }
        "#;
        let (rir, interner) = lower_rir(source, file);
        let index = BodyRirIndex::new(&rir);

        // 2c: the RIR index locates the method declaration; fill the handle.
        let widget = interner.get("Widget").unwrap();
        let bump = interner.get("bump").unwrap();
        let declaration = index.named_method_declaration(file, widget, bump).unwrap();
        let handle = method_handle_from_rir(&rir, declaration);

        // 2b + 2a: mint the durable method subset (receiver resolves through 2a).
        let mut pool = callable_pool(
            [(
                1,
                named(
                    "Widget",
                    "pkg/main.rue",
                    true,
                    struct_body(vec![("id", DType::U32)], false, false),
                ),
            )],
            [],
            [(
                0,
                durable_method(
                    DType::Nominal(1),
                    vec![param(
                        "delta",
                        DType::I32,
                        SemanticParameterMode::Value,
                        false,
                    )],
                    DType::U32,
                    true,
                    SemanticParameterMode::Value,
                ),
            )],
        );
        let info = pool.resolve_method(&0, handle).unwrap();

        // 2c passthrough, checked against the RIR facts.
        let decl_inst = rir.get(declaration);
        let InstData::FnDecl {
            body, self_is_mut, ..
        } = decl_inst.data
        else {
            panic!("method declaration must be a FnDecl");
        };
        assert_eq!(info.body, body);
        assert_eq!(info.span, decl_inst.span);
        assert_eq!(info.self_is_mut, self_is_mut);
        assert!(!info.self_is_mut);

        // 2b / 2a durable-derived: receiver, has_self, return type, params.
        assert_eq!(info.self_mode, RirParamMode::Normal);
        assert!(info.has_self);
        assert_eq!(render(pool.type_pool(), info.struct_type), "Widget");
        assert_eq!(render(pool.type_pool(), info.return_type), "u32");
        let arena = pool.param_arena();
        assert_eq!(info.params.len(), 1);
        assert_eq!(pool.resolve_symbol(arena.names(info.params)[0]), "delta");
        assert_eq!(render(pool.type_pool(), arena.types(info.params)[0]), "i32");
        assert_eq!(arena.modes(info.params), [RirParamMode::Normal]);
        assert_eq!(arena.comptime(info.params), [false]);
    }
}
