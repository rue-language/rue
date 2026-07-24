//! Endpoint / definition-selection resolution for exact-one-body analysis.
//!
//! Family 1A of the RUE-1091 analyzer rewire (slice r1a). The endpoint,
//! nominal, and module reads that `one_body.rs` performs while turning a
//! canonical body request or reference into a concrete declaration, method,
//! destructor, or materialized [`Type`] no longer touch the semantic epoch
//! tables directly. They flow through [`BodyEndpointProvider`] — the
//! value/definition-world analog of [`crate::SemanticTypeSyntaxProvider`] — so
//! the selection *logic* is provider-generic and a later slice can supply the
//! same facts from a body-fact provider + overlay instead of the epoch `Sema`.
//!
//! [`EpochFacts`] is the one production implementation: each operation is the
//! verbatim epoch-table read the inline `one_body.rs` code performed, so the
//! hoist is byte-identical. Every operation is `&self` and returns owned or
//! `Copy` data, so a caller inside an `&mut Sema` stack constructs a
//! short-lived [`EpochFacts`] per resolution (mirroring `SemaTypeSyntaxProvider`)
//! without retaining a borrow across the surrounding mutations.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use lasso::{Spur, ThreadedRodeo};
use rue_rir::{InstRef, Rir};
use rue_span::FileId;

use super::BodySema;
use super::anon_structs::IssuedAnonymousNominalKey;
use super::body_identity::{BodyIdentityPool, BodyRirIndex, DurableNominalSource};
use super::declaration_index::RirDestructorDeclaration;
use super::info::{FunctionInfo, MethodInfo};
use super::provider::BodyFactProvider;
use crate::intern_pool::TypeInternPool;
use crate::types::{EnumId, ModuleId, StructId, Type};
use crate::{
    SemanticBodyExportFailure, SemanticDefinitionEndpoint, SemanticDefinitionToken,
    SemanticImportNominalKind, SemanticImportType, SemanticModuleEndpoint, SemanticModuleToken,
    StableDefinitionKind, TypeInstanceKey,
};

/// The exact endpoint/definition-selection fact boundary consumed by
/// `one_body.rs`. Every operation answers one point query against the
/// declaration universe and returns owned/`Copy` data — no borrowed epoch table
/// or live `Sema` handle escapes.
pub(in crate::sema) trait BodyEndpointProvider {
    /// Intern a source name to its symbol, or `None` if never interned. Mirrors
    /// `interner.get`.
    fn name_symbol(&self, name: &str) -> Option<Spur>;

    /// The current stable endpoint for a definition token.
    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint>;

    /// The current stable endpoint for a module token.
    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint>;

    /// The internal free-function symbol declared as `(file, name)`.
    fn function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur>;

    /// The struct id declared as `(file, name)`.
    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId>;

    /// The enum id declared as `(file, name)`.
    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId>;

    /// The built-in or compiler-generated struct id for a bare name, preferring
    /// the built-in table. Mirrors `builtin_structs.or_else(generated_structs)`.
    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId>;

    /// The compiler-generated struct id for a bare name.
    fn generated_struct(&self, name: Spur) -> Option<StructId>;

    /// The built-in enum id for a bare name.
    fn builtin_enum(&self, name: Spur) -> Option<EnumId>;

    /// The struct id for an issued anonymous nominal identity.
    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId>;

    /// The enum id for an issued anonymous nominal identity.
    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId>;

    /// Whether a bare name is a built-in or compiler-generated struct.
    fn is_builtin_or_generated_struct(&self, name: Spur) -> bool;

    /// Whether a bare name is a built-in enum.
    fn is_builtin_enum(&self, name: Spur) -> bool;

    /// The signature/binding info for an internal free-function symbol.
    fn function_info(&self, name: Spur) -> Option<FunctionInfo>;

    /// The signature/binding info for a `(struct, name)` member.
    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo>;

    /// The source name a specialized/internal function name derives from.
    /// Mirrors `Sema::source_function_name` (identity when unmapped).
    fn source_function_name(&self, name: Spur) -> Spur;

    /// The first free-function RIR declaration for `(source, file)`.
    fn first_free_function(&self, source: Spur, file_id: FileId) -> Option<InstRef>;

    /// The named-method RIR declaration for `(struct, name)`.
    fn named_method_declaration(&self, struct_id: StructId, name: Spur) -> Option<InstRef>;

    /// The destructor declaration record for `(file, type_name)`.
    fn destructor(&self, file: u32, type_name: Spur) -> Option<RirDestructorDeclaration>;

    /// The module id whose definition lives in `file`.
    fn module_id_for_file(&self, file: u32) -> Option<ModuleId>;

    /// Intern an array type, or `None` on a type-validation failure.
    fn intern_array(&self, element: Type, len: u64) -> Option<Type>;

    /// Intern a `ptr const` type, or `None` on a type-validation failure.
    fn intern_ptr_const(&self, pointee: Type) -> Option<Type>;

    /// Intern a `ptr mut` type, or `None` on a type-validation failure.
    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type>;
}

/// The production [`BodyEndpointProvider`]: every operation is the verbatim
/// epoch-table read the inline `one_body.rs` code performed.
pub(in crate::sema) struct EpochFacts<'s, 'a> {
    sema: &'s BodySema<'a>,
}

/// Construct a short-lived [`EpochFacts`] borrowing `sema` for the duration of
/// one endpoint resolution.
pub(in crate::sema) fn endpoint_facts<'s, 'a>(sema: &'s BodySema<'a>) -> EpochFacts<'s, 'a> {
    EpochFacts { sema }
}

impl BodyEndpointProvider for EpochFacts<'_, '_> {
    fn name_symbol(&self, name: &str) -> Option<Spur> {
        self.sema.interner.get(name)
    }

    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint> {
        self.sema.stable_definition_endpoints.get(&token).cloned()
    }

    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint> {
        self.sema.stable_module_endpoints.get(&token).copied()
    }

    fn function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur> {
        self.sema.functions_by_file_name.get(&(file, name)).copied()
    }

    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.sema.structs_by_file_name.get(&(file, name)).copied()
    }

    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.sema.enums_by_file_name.get(&(file, name)).copied()
    }

    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.sema
            .builtin_structs
            .get(&name)
            .or_else(|| self.sema.generated_structs.get(&name))
            .copied()
    }

    fn generated_struct(&self, name: Spur) -> Option<StructId> {
        self.sema.generated_structs.get(&name).copied()
    }

    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.sema.builtin_enums.get(&name).copied()
    }

    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        self.sema.anon_struct_identities.get(identity).copied()
    }

    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        self.sema.anon_enum_identities.get(identity).copied()
    }

    fn is_builtin_or_generated_struct(&self, name: Spur) -> bool {
        self.sema.builtin_structs.contains_key(&name)
            || self.sema.generated_structs.contains_key(&name)
    }

    fn is_builtin_enum(&self, name: Spur) -> bool {
        self.sema.builtin_enums.contains_key(&name)
    }

    fn function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.sema.function_info(name).copied()
    }

    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.sema.method_info((struct_id, name)).copied()
    }

    fn source_function_name(&self, name: Spur) -> Spur {
        self.sema.source_function_name(name)
    }

    fn first_free_function(&self, source: Spur, file_id: FileId) -> Option<InstRef> {
        self.sema
            .declaration_index
            .first_free_function(source, Some(file_id))
    }

    fn named_method_declaration(&self, struct_id: StructId, name: Spur) -> Option<InstRef> {
        self.sema
            .named_method_declarations
            .get(&(struct_id, name))
            .copied()
    }

    fn destructor(&self, file: u32, type_name: Spur) -> Option<RirDestructorDeclaration> {
        self.sema
            .declaration_index
            .destructors()
            .iter()
            .find(|record| record.span.file_id.index() == file && record.type_name == type_name)
            .copied()
    }

    fn module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        (0..self.sema.module_registry.len())
            .map(|index| ModuleId::new(index as u32))
            .find(|id| self.sema.module_registry.get_def(*id).file_id.index() == file)
    }

    fn intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.sema.type_pool.try_intern_array(element, len).ok()
    }

    fn intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.sema.type_pool.try_intern_ptr_const(pointee).ok()
    }

    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.sema.type_pool.try_intern_ptr_mut(pointee).ok()
    }
}

/// Resolve a definition-token function reference to its internal free-function
/// symbol. The endpoint, name interning, and by-file lookup all fail to the
/// same `MissingStableIdentity` in the caller, so a single `None` is faithful.
pub(in crate::sema) fn resolve_free_function_symbol<P: BodyEndpointProvider>(
    facts: &P,
    token: SemanticDefinitionToken,
) -> Option<Spur> {
    let endpoint = facts.definition_endpoint(token)?;
    let symbol = facts.name_symbol(endpoint.name.as_ref())?;
    facts.function_by_file_name(FileId::new(endpoint.file), symbol)
}

/// Materialize a canonical type-instance key into a concrete [`Type`]. The
/// provider-generic form of `one_body::materialize_instance_type`: an exact
/// transcription of the epoch code, with every table read routed through
/// `facts` and every failure mapped to `MissingStableIdentity`.
pub(in crate::sema) fn resolve_instance_type<P: BodyEndpointProvider>(
    facts: &P,
    value: &crate::TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
) -> Result<Type, crate::SemanticBodyExportFailure> {
    use crate::{AnonymousNominalKind as AK, NominalInstanceKey as N, TypeInstanceKey as T};
    let missing = || crate::SemanticBodyExportFailure::MissingStableIdentity;
    Ok(match value {
        T::I8 => Type::I8,
        T::I16 => Type::I16,
        T::I32 => Type::I32,
        T::I64 => Type::I64,
        T::U8 => Type::U8,
        T::U16 => Type::U16,
        T::U32 => Type::U32,
        T::U64 => Type::U64,
        T::Bool => Type::BOOL,
        T::Unit => Type::UNIT,
        T::Never => Type::NEVER,
        T::ComptimeType => Type::COMPTIME_TYPE,
        T::BuiltinNominal { kind, name } => {
            let symbol = facts.name_symbol(name.as_ref()).ok_or_else(missing)?;
            match kind {
                AK::Struct => Type::new_struct(
                    facts
                        .builtin_or_generated_struct(symbol)
                        .ok_or_else(missing)?,
                ),
                AK::Enum => Type::new_enum(facts.builtin_enum(symbol).ok_or_else(missing)?),
            }
        }
        T::Nominal(N::Builtin { kind, name }) => {
            let symbol = facts.name_symbol(name.as_ref()).ok_or_else(missing)?;
            match kind {
                AK::Struct => Type::new_struct(
                    facts
                        .builtin_or_generated_struct(symbol)
                        .ok_or_else(missing)?,
                ),
                AK::Enum => Type::new_enum(facts.builtin_enum(symbol).ok_or_else(missing)?),
            }
        }
        T::Nominal(N::Named(token)) => {
            let endpoint = facts.definition_endpoint(*token).ok_or_else(missing)?;
            let symbol = facts
                .name_symbol(endpoint.name.as_ref())
                .ok_or_else(missing)?;
            match endpoint.kind {
                StableDefinitionKind::Struct => Type::new_struct(
                    facts
                        .struct_by_file_name(FileId::new(endpoint.file), symbol)
                        .ok_or_else(missing)?,
                ),
                StableDefinitionKind::Enum => Type::new_enum(
                    facts
                        .enum_by_file_name(FileId::new(endpoint.file), symbol)
                        .ok_or_else(missing)?,
                ),
                _ => return Err(missing()),
            }
        }
        T::Nominal(N::Anonymous(identity)) => match identity.kind {
            AK::Struct => Type::new_struct(facts.anon_struct(identity).ok_or_else(missing)?),
            AK::Enum => Type::new_enum(facts.anon_enum(identity).ok_or_else(missing)?),
        },
        T::Array { element, len } => facts
            .intern_array(resolve_instance_type(facts, element)?, *len)
            .ok_or_else(missing)?,
        T::Slice { name, .. } => {
            let symbol = facts.name_symbol(name.as_ref()).ok_or_else(missing)?;
            Type::new_struct(facts.generated_struct(symbol).ok_or_else(missing)?)
        }
        T::PtrConst(value) => facts
            .intern_ptr_const(resolve_instance_type(facts, value)?)
            .ok_or_else(missing)?,
        T::PtrMut(value) => facts
            .intern_ptr_mut(resolve_instance_type(facts, value)?)
            .ok_or_else(missing)?,
        T::Module(token) => {
            let endpoint = facts.module_endpoint(*token).ok_or_else(missing)?;
            let id = facts
                .module_id_for_file(endpoint.file)
                .ok_or_else(missing)?;
            Type::new_module(id)
        }
        T::GenericParameter(_) => return Err(missing()),
    })
}

// ---------------------------------------------------------------------------
// `ProviderEndpointFacts` — the endpoint / definition-selection ProviderFacts
// (RUE-1091 slice r4b-2).
//
// The first provider-driven realization of the family-1A `BodyEndpointProvider`
// seam: where [`EpochFacts`] answers each endpoint op from the semantic epoch's
// `Sema` tables, this driver answers them from the body-scoped identity pool
// (slices 2a/2b/2c — minting nominal `StructId`/`Type` identities from the
// durable metadata a [`DurableNominalSource`] supplies) plus the shared
// whole-program `Rir` (the RIR-index handles), with the live body-fact provider
// ([`BodyFactProvider`]) available for the candidate-set presence check. It is
// the endpoint twin of r4b-1's [`super::call_resolution::ProviderCallFacts`].
//
// The core answer REUSES the provider-generic [`resolve_instance_type`] this
// module already exposes, driven over the pool instead of the epoch: the driver
// is a [`BodyEndpointProvider`] whose ops read the pool + a small overlay token
// space, so `resolve_instance_type(self, key)` walks the exact same
// `TypeInstanceKey` algebra production walks — only the fact SOURCE differs.
// The pool mints internally-consistent ids (the pool keystone); a differential
// compares the resolved type's index-independent render + metadata against the
// LIVE epoch, never a pool-relative index.
//
// RUE-1091 flip-era surface: `pub` because rFinal's whole-body differential and
// the step-4 flip drive the provider path from rue-compiler, where the pool's
// durable source is built from concrete nucleus signatures (an opaque
// `BodyFactProvider` associated type rue-air cannot destructure). The sole
// pre-flip caller is the rue-compiler differential; the flip promotes it to the
// production analyzer.
//
// Feasibility (r4a design-checkpoint table): P = answered-by-pool, C =
// composed-from-provider, R = RIR-index answer.
//   - `resolve_instance_type` (every pool-supported `TypeInstanceKey` arm)  → P
//   - `first_free_function` / `named_method_declaration` / `destructor`     → R
//   - `nominal_contains_in_module`                                          → C
// Deferred here, each with its unblocking slice named (reported, never silently
// answered wrong):
//   - the `(StructId, name)`-keyed `BodyEndpointProvider::named_method_
//     declaration` trait op → r4b-3 (the endpoint seam owns receiver→pool
//     identity). This driver answers the op by its provider-natural preimage
//     `(owner_file, owner_type_name, method)` on an inherent method, exactly as
//     `ProviderCallFacts` does; the `StructId`-keyed trait signature stays a
//     r4b-3 seam translation and returns `None` here.
//   - `function_info` / `function_by_file_name` → r4b-1's `ProviderCallFacts`
//     (the call family); `method_info` → r4b-3 (receiver→pool identity).
//   - `module_endpoint` / `module_id_for_file` (the `Module` arm) → module
//     identity is a pool-refused arm; the endpoint-seam module registry is
//     r4b-3 / the flip.
//   - `anon_struct` / `anon_enum` (the anonymous arm) → r6 (anonymous
//     mint-from-digest and the well-known `Option` facts); the pool resolves an
//     issued anonymous by lookup only.
//   - `generated_struct` (the `Slice` arm) and builtin names beyond the
//     pre-registered `BUILTIN_ENUMS` + `str` set → r6 (builtin / slice name
//     facts).
//   - `source_function_name` under specialization → r5; identity otherwise.
// ---------------------------------------------------------------------------

/// The issuer stamped on every [`SemanticDefinitionToken`] this driver mints
/// into its overlay token space. The value is arbitrary but must be stable and
/// distinct so a token minted here never aliases an epoch-issued one; the token
/// is only ever reversed through this driver's own overlay, never an epoch
/// table.
const OVERLAY_ISSUER: u64 = 0x0000_04b2_0000_0001;

/// One overlay endpoint entry: the `(file, name, kind)` the `Named`-nominal arm
/// of [`resolve_instance_type`] reads back through
/// [`BodyEndpointProvider::definition_endpoint`]. The durable key the entry
/// stands for lives in [`EndpointOverlay::by_file_name`], keyed by the same
/// `(file, name)` preimage the endpoint yields.
struct EndpointEntry {
    file: u32,
    name: Arc<str>,
    kind: StableDefinitionKind,
}

/// The driver's overlay token space: the provider-side analog of the epoch's
/// `stable_definition_endpoints` + `structs_by_file_name`/`enums_by_file_name`
/// tables, populated on demand as a differential mints a token per durable
/// nominal key. A token reverses to its `(file, name, kind)` endpoint; the
/// `(file, name)` preimage reverses to the durable key the pool mints from —
/// keyed on the pool's own interner [`Spur`], the symbol
/// [`BodyEndpointProvider::name_symbol`] hands back.
struct EndpointOverlay<K> {
    next_slot: u32,
    tokens: HashMap<SemanticDefinitionToken, EndpointEntry>,
    by_file_name: HashMap<(u32, Spur), K>,
}

impl<K> Default for EndpointOverlay<K> {
    fn default() -> Self {
        Self {
            next_slot: 0,
            tokens: HashMap::new(),
            by_file_name: HashMap::new(),
        }
    }
}

/// The endpoint-resolution ProviderFacts driver: answers the family-1A endpoint
/// ops from a body identity pool + RIR index + [`BodyFactProvider`], instead of
/// the epoch `Sema` tables [`EpochFacts`] reads.
///
/// Generic over the provider `P`, the pool durable source `S`, and the pool's
/// durable nominal key `K` and module `M` (rue-compiler binds
/// `K = StableDefinitionKey`, `M = ModuleId`). The pool lives behind a
/// [`RefCell`] because a nominal is minted on first consult (`&mut` on the
/// pool) while [`BodyEndpointProvider`]'s ops — and therefore the shared
/// [`resolve_instance_type`] logic driving them — are `&self`; the borrow is
/// never held across a re-entrant consult, so it never conflicts.
pub struct ProviderEndpointFacts<'a, P, S, K, M> {
    provider: &'a P,
    pool: RefCell<BodyIdentityPool<K, M, S>>,
    rir_index: BodyRirIndex,
    /// The whole-program RIR interner. The RIR-index ops resolve their `&str`
    /// keys through this interner (the shared `Rir`'s symbol space), distinct
    /// from the pool's own interner the nominal ops key on.
    rir_interner: &'a ThreadedRodeo,
    overlay: RefCell<EndpointOverlay<K>>,
}

impl<'a, P, S, K, M> ProviderEndpointFacts<'a, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>,
    K: Clone + Eq + Hash,
    M: Eq + Hash,
{
    /// Construct the driver over a provider, a durable nominal source, and the
    /// shared whole-program `Rir` + interner. The pool and RIR index are built
    /// here; nominals are minted lazily on first consult and the overlay token
    /// space starts empty (a caller mints a token per nominal with
    /// [`Self::register_named_nominal`]).
    pub fn new(provider: &'a P, source: S, rir: &'a Rir, rir_interner: &'a ThreadedRodeo) -> Self {
        Self {
            provider,
            pool: RefCell::new(BodyIdentityPool::new(source)),
            rir_index: BodyRirIndex::new(rir),
            rir_interner,
            overlay: RefCell::new(EndpointOverlay::default()),
        }
    }

    /// Mint an overlay [`SemanticDefinitionToken`] standing for a durable nominal
    /// key, recording the `(file, name, kind)` endpoint and the `(file, name)`
    /// preimage the `Named`-nominal arm of [`resolve_instance_type`] reverses. A
    /// caller supplies the same `(file, name, kind)` the epoch's stable endpoint
    /// carries, so the driver's token space stays a faithful stand-in for the
    /// epoch's `stable_definition_endpoints` without holding any epoch handle.
    pub fn register_named_nominal(
        &self,
        key: K,
        file: u32,
        name: &str,
        kind: StableDefinitionKind,
    ) -> SemanticDefinitionToken {
        let symbol = self.pool.borrow().intern_name(name);
        let mut overlay = self.overlay.borrow_mut();
        let slot = overlay.next_slot;
        overlay.next_slot += 1;
        let token = SemanticDefinitionToken::new(OVERLAY_ISSUER, slot);
        overlay.tokens.insert(
            token,
            EndpointEntry {
                file,
                name: Arc::from(name),
                kind,
            },
        );
        overlay.by_file_name.insert((file, symbol), key);
        token
    }

    /// (P) Materialize a canonical type-instance key into a concrete pool
    /// [`Type`], reusing the provider-generic [`resolve_instance_type`] driven
    /// over this pool-backed [`BodyEndpointProvider`]. Every arm the pool
    /// supports resolves; a deferred arm (module identity, generic parameter,
    /// anonymous mint, builtin/slice name beyond the pre-registered set) fails
    /// closed to `MissingStableIdentity`, exactly as the pool refuses it.
    pub fn resolve_instance_type(
        &self,
        value: &TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    ) -> Result<Type, SemanticBodyExportFailure> {
        resolve_instance_type(self, value)
    }

    /// (R) The first free-function RIR declaration for `(source_name, file)`,
    /// answered by [`BodyRirIndex`] over the shared `Rir` — the exact `InstRef`
    /// [`EpochFacts::first_free_function`] returns, keyed provider-naturally by
    /// the source name string.
    pub fn first_free_function(&self, source_name: &str, file: FileId) -> Option<InstRef> {
        let symbol = self.rir_interner.get(source_name)?;
        self.rir_index.first_free_function(symbol, file)
    }

    /// (R) The named-method RIR declaration for `(owner_file, owner_type_name,
    /// method)` — the durable-available preimage of the epoch's `(StructId,
    /// method)` key, answered by [`BodyRirIndex`]. Keyed by the preimage DIRECTLY
    /// (provider-natural), the r4a-2c "prefer rethreading" resolution: the
    /// `StructId`-keyed `BodyEndpointProvider::named_method_declaration` trait
    /// signature stays a r4b-3 seam translation (it returns `None` here).
    pub fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<InstRef> {
        let owner = self.rir_interner.get(owner_type_name)?;
        let method_sym = self.rir_interner.get(method)?;
        self.rir_index
            .named_method_declaration(owner_file, owner, method_sym)
    }

    /// (R) The destructor RIR declaration handle for `(file, type_name)`, the
    /// exact scan [`EpochFacts::destructor`] performs, answered by
    /// [`BodyRirIndex`]. Returns the located declaration `InstRef` (the private
    /// destructor record's public handle); the epoch keys the same record by the
    /// same `(file, type_name)` scan.
    pub fn destructor(&self, file: FileId, type_name: &str) -> Option<InstRef> {
        let symbol = self.rir_interner.get(type_name)?;
        self.rir_index
            .destructor(file.index(), symbol)
            .map(|record| record.declaration)
    }

    /// (C) Whether a source name resolves to a declared nominal of `want`
    /// (struct or enum) in `module` — the provider `lookup_unqualified`
    /// (ModuleItem) analog of the epoch's `structs_by_file_name` /
    /// `enums_by_file_name` membership. Selection (the kind filter) stays here
    /// against the returned candidate set, honoring the boundary's
    /// candidate-sets-not-winners contract; a unique or ambiguous candidate of
    /// the wanted kind counts as present, exactly as a populated epoch table
    /// entry does.
    pub fn nominal_contains_in_module(
        &self,
        module: &P::ModuleRef,
        source_name: &str,
        want: crate::AnonymousNominalKind,
    ) -> bool {
        use super::provider::{NameResolution, ProviderDefinitionKind, ProviderNamespace};
        let resolution =
            self.provider
                .lookup_unqualified(module, ProviderNamespace::ModuleItem, source_name);
        let kind = match want {
            crate::AnonymousNominalKind::Struct => ProviderDefinitionKind::Struct,
            crate::AnonymousNominalKind::Enum => ProviderDefinitionKind::Enum,
        };
        matches!(
            resolution.of_kind(kind),
            NameResolution::Unique(_) | NameResolution::Ambiguous(_)
        )
    }

    /// Read the body-local minted [`TypeInternPool`] under a closure, so a
    /// differential renders a resolved pool [`Type`] index-independently (the
    /// pool mints its own ids; parity is asserted through displays / metadata,
    /// never a pool-relative index — the 2a/2b contract). Closure-scoped because
    /// the pool lives behind a [`RefCell`].
    pub fn with_type_pool<R>(&self, read: impl FnOnce(&TypeInternPool) -> R) -> R {
        read(self.pool.borrow().type_pool())
    }
}

impl<P, S, K, M> BodyEndpointProvider for ProviderEndpointFacts<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>,
    K: Clone + Eq + Hash,
    M: Eq + Hash,
{
    fn name_symbol(&self, name: &str) -> Option<Spur> {
        Some(self.pool.borrow().intern_name(name))
    }

    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint> {
        let overlay = self.overlay.borrow();
        let entry = overlay.tokens.get(&token)?;
        Some(SemanticDefinitionEndpoint {
            token,
            file: entry.file,
            name: entry.name.clone(),
            kind: entry.kind,
            owner: None,
        })
    }

    fn module_endpoint(&self, _token: SemanticModuleToken) -> Option<SemanticModuleEndpoint> {
        // Module identity is a pool-refused arm; the endpoint-seam module
        // registry is r4b-3 / the flip. The `Module` arm fails closed here.
        None
    }

    fn function_by_file_name(&self, _file: FileId, _name: Spur) -> Option<Spur> {
        // The call family: r4b-1's `ProviderCallFacts` answers the free-function
        // identity; the `(file, name)`-keyed seam is r4b-3. Not consulted by
        // `resolve_instance_type`.
        None
    }

    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        let key = self
            .overlay
            .borrow()
            .by_file_name
            .get(&(file.index(), name))
            .cloned()?;
        self.pool
            .borrow_mut()
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_struct()
    }

    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        let key = self
            .overlay
            .borrow()
            .by_file_name
            .get(&(file.index(), name))
            .cloned()?;
        self.pool
            .borrow_mut()
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_enum()
    }

    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        let name = self.pool.borrow().resolve_symbol(name).to_owned();
        self.pool
            .borrow_mut()
            .resolve(&SemanticImportType::BuiltinNominal {
                name: Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Struct,
            })
            .ok()?
            .as_struct()
    }

    fn generated_struct(&self, _name: Spur) -> Option<StructId> {
        // Generated / slice struct names beyond the pool's pre-registered set
        // are r6 (builtin / slice name facts). The `Slice` arm fails closed.
        None
    }

    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        let name = self.pool.borrow().resolve_symbol(name).to_owned();
        self.pool
            .borrow_mut()
            .resolve(&SemanticImportType::BuiltinNominal {
                name: Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Enum,
            })
            .ok()?
            .as_enum()
    }

    fn anon_struct(&self, _identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        // Anonymous mint-from-digest is r6; the pool resolves an issued
        // anonymous by lookup only, which this differential does not seed.
        None
    }

    fn anon_enum(&self, _identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        None
    }

    fn is_builtin_or_generated_struct(&self, name: Spur) -> bool {
        self.builtin_or_generated_struct(name).is_some()
    }

    fn is_builtin_enum(&self, name: Spur) -> bool {
        self.builtin_enum(name).is_some()
    }

    fn function_info(&self, _name: Spur) -> Option<FunctionInfo> {
        // The call family: r4b-1's `ProviderCallFacts::function_info`.
        None
    }

    fn method_info(&self, _struct_id: StructId, _name: Spur) -> Option<MethodInfo> {
        // r4b-3: the receiver→pool identity the endpoint seam owns.
        None
    }

    fn source_function_name(&self, name: Spur) -> Spur {
        // Identity: the specialization name map is r5.
        name
    }

    fn first_free_function(&self, _source: Spur, _file_id: FileId) -> Option<InstRef> {
        // Answered provider-naturally by the inherent
        // `first_free_function(&str, FileId)`; the `Spur`-keyed trait op keys on
        // an ambiguous interner and is not consulted by `resolve_instance_type`.
        None
    }

    fn named_method_declaration(&self, _struct_id: StructId, _name: Spur) -> Option<InstRef> {
        // The `StructId`-keyed seam translation is r4b-3; the preimage-keyed
        // answer is the inherent `named_method_declaration(FileId, &str, &str)`.
        None
    }

    fn destructor(&self, _file: u32, _type_name: Spur) -> Option<RirDestructorDeclaration> {
        // Answered provider-naturally by the inherent `destructor(FileId, &str)`.
        None
    }

    fn module_id_for_file(&self, _file: u32) -> Option<ModuleId> {
        // Module identity is a pool-refused arm (see `module_endpoint`).
        None
    }

    fn intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.pool
            .borrow()
            .type_pool()
            .try_intern_array(element, len)
            .ok()
    }

    fn intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.pool
            .borrow()
            .type_pool()
            .try_intern_ptr_const(pointee)
            .ok()
    }

    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.pool
            .borrow()
            .type_pool()
            .try_intern_ptr_mut(pointee)
            .ok()
    }
}
