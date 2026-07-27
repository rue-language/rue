//! Endpoint and definition-selection facts for canonical body analysis.
//!
//! Every point lookup flows through [`BodyEndpointProvider`]. Query-backed
//! analysis uses [`ProviderEndpointFacts`]; the declaration analyzer implements
//! the same fact vocabulary for its non-body responsibilities.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use lasso::{Spur, ThreadedRodeo};
use rue_rir::InstRef;
use rue_span::FileId;

use super::anon_structs::IssuedAnonymousNominalKey;
use super::body_identity::{
    BodyRirView, ConstIdentityHandle, DurableAnonymousSource, DurableCallableSource,
    DurableConstSource, DurableNominalSource, FunctionIdentityHandle, ProviderIdentityContext,
};
use super::info::{ConstInfo, FunctionInfo, MethodInfo};
use super::provider::BodyFactProvider;
use super::{DeclarationPhase, Sema};
use crate::intern_pool::TypeInternPool;
use crate::types::{EnumId, ModuleId, StructId, Type};
use crate::{
    SemanticBodyExportFailure, SemanticDefinitionEndpoint, SemanticDefinitionToken,
    SemanticImportNominalKind, SemanticImportType, SemanticModuleEndpoint, SemanticModuleToken,
    StableDefinitionKind, TypeInstanceKey,
};

/// Exact endpoint and definition-selection facts consumed by body analysis.
/// Every operation returns owned or `Copy` data.
pub(crate) trait BodyEndpointProvider {
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

    /// The signature/binding info for an internal free-function symbol.
    fn function_info(&self, name: Spur) -> Option<FunctionInfo>;

    /// The signature/binding info for a `(struct, name)` member.
    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo>;

    /// The source name a specialized/internal function name derives from.
    /// Mirrors `Sema::source_function_name` (identity when unmapped).
    fn source_function_name(&self, name: Spur) -> Spur;

    /// The module id whose definition lives in `file`.
    fn module_id_for_file(&self, file: u32) -> Option<ModuleId>;

    /// Intern an array type, or `None` on a type-validation failure.
    fn intern_array(&self, element: Type, len: u64) -> Option<Type>;

    /// Intern a `ptr const` type, or `None` on a type-validation failure.
    fn intern_ptr_const(&self, pointee: Type) -> Option<Type>;

    /// Intern a `ptr mut` type, or `None` on a type-validation failure.
    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type>;
}

/// Endpoint reads supplied by a body-analysis host. Most operations are
/// immutable table reads. Type interning is host-specific and is not part of
/// the shared read view.
pub(super) trait BodyEndpointFactSource {
    fn endpoint_read_view(&self) -> Option<BodyEndpointReadView<'_>> {
        None
    }

    fn endpoint_view(&self) -> BodyEndpointReadView<'_> {
        self.endpoint_read_view()
            .expect("body endpoint fact source has no shared read view")
    }

    fn endpoint_name_symbol(&self, name: &str) -> Option<Spur> {
        self.endpoint_view().name_symbol(name)
    }
    fn endpoint_definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint> {
        self.endpoint_view().definition_endpoint(token)
    }
    fn endpoint_module_endpoint(
        &self,
        token: SemanticModuleToken,
    ) -> Option<SemanticModuleEndpoint> {
        self.endpoint_view().module_endpoint(token)
    }
    fn endpoint_function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur> {
        self.endpoint_view().function_by_file_name(file, name)
    }
    fn endpoint_struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.endpoint_view().struct_by_file_name(file, name)
    }
    fn endpoint_enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.endpoint_view().enum_by_file_name(file, name)
    }
    fn endpoint_builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.endpoint_view().builtin_or_generated_struct(name)
    }
    fn endpoint_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.endpoint_view().generated_struct(name)
    }
    fn endpoint_builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.endpoint_view().builtin_enum(name)
    }
    fn endpoint_anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        self.endpoint_view().anon_struct(identity)
    }
    fn endpoint_anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        self.endpoint_view().anon_enum(identity)
    }
    fn endpoint_function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.endpoint_view().function_info(name)
    }
    fn endpoint_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.endpoint_view().method_info(struct_id, name)
    }
    fn endpoint_source_function_name(&self, name: Spur) -> Spur {
        self.endpoint_view().source_function_name(name)
    }
    fn endpoint_module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        self.endpoint_view().module_id_for_file(file)
    }
    fn endpoint_intern_array(&self, _element: Type, _len: u64) -> Option<Type> {
        None
    }
    fn endpoint_intern_ptr_const(&self, _pointee: Type) -> Option<Type> {
        None
    }
    fn endpoint_intern_ptr_mut(&self, _pointee: Type) -> Option<Type> {
        None
    }
}

/// Shared read view for declaration-owned endpoint facts.
pub(super) struct BodyEndpointReadView<'a> {
    interner: &'a ThreadedRodeo,
    declarations: &'a super::DeclarationNamespace,
    stable_definition_endpoints: &'a HashMap<SemanticDefinitionToken, SemanticDefinitionEndpoint>,
    stable_module_endpoints: &'a HashMap<SemanticModuleToken, SemanticModuleEndpoint>,
    anonymous_methods: &'a HashMap<(StructId, Spur), MethodInfo>,
    generated_structs: &'a HashMap<Spur, StructId>,
    anon_struct_identities: &'a HashMap<IssuedAnonymousNominalKey, StructId>,
    anon_enum_identities: &'a HashMap<IssuedAnonymousNominalKey, EnumId>,
    module_registry: &'a crate::module_registry::ModuleRegistry,
}

impl BodyEndpointReadView<'_> {
    fn name_symbol(&self, name: &str) -> Option<Spur> {
        self.interner.get(name)
    }

    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint> {
        self.stable_definition_endpoints.get(&token).cloned()
    }

    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint> {
        self.stable_module_endpoints.get(&token).copied()
    }

    fn function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur> {
        self.declarations
            .functions_by_file_name
            .get(&(file, name))
            .copied()
    }

    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.declarations
            .structs_by_file_name
            .get(&(file, name))
            .copied()
    }

    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.declarations
            .enums_by_file_name
            .get(&(file, name))
            .copied()
    }

    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.declarations
            .builtin_structs
            .get(&name)
            .or_else(|| self.generated_structs.get(&name))
            .copied()
    }

    fn generated_struct(&self, name: Spur) -> Option<StructId> {
        self.generated_structs.get(&name).copied()
    }

    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.declarations.builtin_enums.get(&name).copied()
    }

    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        self.anon_struct_identities.get(identity).copied()
    }

    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        self.anon_enum_identities.get(identity).copied()
    }

    fn function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.declarations.functions.get(&name).copied()
    }

    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.anonymous_methods
            .get(&(struct_id, name))
            .copied()
            .or_else(|| self.declarations.methods.get(&(struct_id, name)).copied())
    }

    fn source_function_name(&self, name: Spur) -> Spur {
        self.declarations
            .function_source_names
            .get(&name)
            .copied()
            .unwrap_or(name)
    }

    fn module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        (0..self.module_registry.len())
            .map(|index| ModuleId::new(index as u32))
            .find(|id| self.module_registry.get_def(*id).file_id.index() == file)
    }
}

impl<D: DeclarationPhase> BodyEndpointFactSource for Sema<'_, D> {
    fn endpoint_read_view(&self) -> Option<BodyEndpointReadView<'_>> {
        Some(BodyEndpointReadView {
            interner: self.interner,
            declarations: &self.declarations,
            stable_definition_endpoints: &self.stable_definition_endpoints,
            stable_module_endpoints: &self.stable_module_endpoints,
            anonymous_methods: &self.anonymous_methods,
            generated_structs: &self.generated_structs,
            anon_struct_identities: &self.anon_struct_identities,
            anon_enum_identities: &self.anon_enum_identities,
            module_registry: &self.module_registry,
        })
    }

    fn endpoint_intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.type_pool.try_intern_array(element, len).ok()
    }

    fn endpoint_intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.type_pool.try_intern_ptr_const(pointee).ok()
    }

    fn endpoint_intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.type_pool.try_intern_ptr_mut(pointee).ok()
    }
}

/// Read-only endpoint adapter used by the canonical body engine.
///
/// It carries only a host capability; the current epoch-backed host is one
/// production source, while an owned body state can supply the same reads next.
pub(crate) struct EpochFacts<'host, H: BodyEndpointFactSource> {
    host: &'host H,
}

impl<'host, H: BodyEndpointFactSource> EpochFacts<'host, H> {
    pub(in crate::sema) fn new(host: &'host H) -> Self {
        Self { host }
    }
}

impl<H: BodyEndpointFactSource> BodyEndpointProvider for EpochFacts<'_, H> {
    fn name_symbol(&self, name: &str) -> Option<Spur> {
        self.host.endpoint_name_symbol(name)
    }
    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint> {
        self.host.endpoint_definition_endpoint(token)
    }
    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint> {
        self.host.endpoint_module_endpoint(token)
    }
    fn function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur> {
        self.host.endpoint_function_by_file_name(file, name)
    }
    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.host.endpoint_struct_by_file_name(file, name)
    }
    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.host.endpoint_enum_by_file_name(file, name)
    }
    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.host.endpoint_builtin_or_generated_struct(name)
    }
    fn generated_struct(&self, name: Spur) -> Option<StructId> {
        self.host.endpoint_generated_struct(name)
    }
    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.host.endpoint_builtin_enum(name)
    }
    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        self.host.endpoint_anon_struct(identity)
    }
    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        self.host.endpoint_anon_enum(identity)
    }
    fn function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.host.endpoint_function_info(name)
    }
    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.host.endpoint_method_info(struct_id, name)
    }
    fn source_function_name(&self, name: Spur) -> Spur {
        self.host.endpoint_source_function_name(name)
    }
    fn module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        self.host.endpoint_module_id_for_file(file)
    }
    fn intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.host.endpoint_intern_array(element, len)
    }
    fn intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.host.endpoint_intern_ptr_const(pointee)
    }
    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.host.endpoint_intern_ptr_mut(pointee)
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
/// provider-generic form of `body_analysis::materialize_instance_type`: an exact
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
// `ProviderEndpointFacts` — endpoint / definition-selection facts.
//
// This driver answers endpoint operations from the body-scoped identity pool,
// the request's RIR index, and point queries through [`BodyFactProvider`].
//
// The core answer REUSES the provider-generic [`resolve_instance_type`] this
// module already exposes, driven over the pool instead of the epoch: the driver
// is a [`BodyEndpointProvider`] whose ops read the pool + a small overlay token
// space, so `resolve_instance_type(self, key)` walks the exact same
// `TypeInstanceKey` algebra production walks — only the fact SOURCE differs.
// The pool mints internally-consistent ids (the pool keystone); a differential
// compares pool values by index-independent render and metadata, never by raw
// pool-relative index.
//
// This surface is public because rue-compiler supplies the concrete durable
// signature source behind the opaque provider boundary.
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
    owner: Option<Arc<str>>,
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
    definition_tokens: HashMap<K, SemanticDefinitionToken>,
    by_file_name: HashMap<(u32, Spur), K>,
    functions_by_file_name: HashMap<(u32, Spur), (Spur, K)>,
    function_keys: HashMap<Spur, K>,
    module_tokens: HashMap<SemanticModuleToken, SemanticModuleEndpoint>,
    /// Generated slice-struct identities minted on demand (RUE-1091 r6a): the
    /// provider-side analog of the epoch's `generated_structs` name→id map. A
    /// caller seeds one per `[T]` slice with [`ProviderEndpointFacts::
    /// register_generated_slice`] (exactly as it seeds a named nominal with
    /// [`ProviderEndpointFacts::register_named_nominal`]), so the `Slice` arm of
    /// [`resolve_instance_type`] resolves the generated-struct name the epoch
    /// mints during declaration gathering.
    generated_slices: HashMap<Spur, StructId>,
}

impl<K> Default for EndpointOverlay<K> {
    fn default() -> Self {
        Self {
            next_slot: 0,
            tokens: HashMap::new(),
            definition_tokens: HashMap::new(),
            by_file_name: HashMap::new(),
            functions_by_file_name: HashMap::new(),
            function_keys: HashMap::new(),
            module_tokens: HashMap::new(),
            generated_slices: HashMap::new(),
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
    identity: ProviderIdentityContext<K, M, S>,
    rir: BodyRirView<'a>,
    overlay: RefCell<EndpointOverlay<K>>,
    /// The issued→durable anonymous seam (RUE-1091 r6b): the anonymous producer
    /// key `resolve_instance_type` carries is in the issued-token domain, so a
    /// caller seeds the durable key it stands for with
    /// [`Self::register_anonymous_nominal`], exactly as it seeds a named nominal
    /// with [`Self::register_named_nominal`]. The `anon_struct`/`anon_enum` arms
    /// then mint through the pool's [`BodyIdentityPool::find_or_create_anon`].
    anon_by_issued: RefCell<HashMap<IssuedAnonymousNominalKey, crate::AnonymousNominalKey<K, M>>>,
}

impl<'a, P, S, K, M> ProviderEndpointFacts<'a, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M> + DurableAnonymousSource<K, M> + DurableCallableSource<K, M>,
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
{
    /// Construct the driver over one request-local RIR view and identity
    /// context. The view's declaration index is shared rather than rebuilt.
    pub fn new(provider: &'a P, source: S, rir: BodyRirView<'a>) -> Self {
        let identity = ProviderIdentityContext::new(source);
        Self::with_identity(provider, identity, rir)
    }

    /// Construct the endpoint facade from the one task-local provider
    /// authority shared with calls, aggregates, and body analysis.
    pub fn with_state(
        provider: &'a P,
        state: &super::ProviderBodyAnalysisState<K, M, S>,
        rir: BodyRirView<'a>,
    ) -> Self {
        assert!(
            state.require_rir_authority(&rir),
            "provider body state and RIR view must share one interner authority"
        );
        Self::with_overlay_identity(provider, state.identity_context(), rir)
    }

    /// Construct the driver inside an existing per-body identity universe.
    pub fn with_identity(
        provider: &'a P,
        identity: ProviderIdentityContext<K, M, S>,
        rir: BodyRirView<'a>,
    ) -> Self {
        Self::with_overlay_identity(provider, identity.fail_closed(), rir)
    }

    fn with_overlay_identity(
        provider: &'a P,
        identity: ProviderIdentityContext<K, M, S>,
        rir: BodyRirView<'a>,
    ) -> Self {
        Self {
            provider,
            identity,
            rir,
            overlay: RefCell::new(EndpointOverlay::default()),
            anon_by_issued: RefCell::new(HashMap::new()),
        }
    }

    /// Seed the durable anonymous producer key an issued-token anonymous key
    /// stands for, so the `anon_struct` / `anon_enum` arms of
    /// [`resolve_instance_type`] mint it through the pool (RUE-1091 r6b). The
    /// caller supplies the same durable identity the epoch's
    /// `import_anonymous_identity` reverses the issued key to; the pool then
    /// spells the byte-identical `__anon_*_{digest}` name from that durable key's
    /// stable relocation.
    pub fn register_anonymous_nominal(
        &self,
        issued: IssuedAnonymousNominalKey,
        durable: crate::AnonymousNominalKey<K, M>,
    ) {
        self.anon_by_issued.borrow_mut().insert(issued, durable);
    }

    /// Register one anonymous method atomically under both lookup preimages in
    /// the identity context shared with the call facade.
    pub fn register_anonymous_method(
        &self,
        file: FileId,
        owner: &str,
        method: &str,
        info: MethodInfo,
    ) -> bool {
        self.identity
            .register_anonymous_method(file, owner, method, info)
    }

    /// Return the registered method for a compact receiver/name pair,
    /// preserving the canonical anonymous-before-named order.
    pub fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.identity.method((struct_id, name))
    }

    /// Mint (or dedup) the anonymous nominal for a durable identity key directly,
    /// the inherent form the differential compares against the LIVE epoch's
    /// `find_or_create_anon_struct`/`_enum`. Returns the minted pool [`Type`], or
    /// `None` if the durable key names no anonymous shape / a digest collision
    /// refuses it (fail-closed).
    pub fn mint_anonymous(&self, durable: &crate::AnonymousNominalKey<K, M>) -> Option<Type> {
        self.identity.pool_mut()?.find_or_create_anon(durable).ok()
    }

    /// Install the per-body well-known `Option(payload)` registry (RUE-1112)
    /// through the pool — the provider-facing port of the epoch's
    /// `BoundSema::install_well_known_option_types` (RUE-1091 r6c). `nominals`
    /// are the durable identities of the trusted-std `Option` enums the
    /// per-body demand loop resolved; each mints through the pool's ordinary
    /// anonymous machinery so its full materialization is byte-identical to
    /// the epoch install's, and each is recorded under the export-as-produced
    /// ruling: the installing body EXPORTS these identities as produced
    /// anonymous nominals (the provider baseline subtraction consults
    /// [`Self::is_well_known_option_identity`]), never leaking them as
    /// imports. `option_by_payload` records the demand map fallible-intrinsic
    /// resolution consults. Fail-closed: `None` on any refusal (non-enum
    /// shape, absent shape, digest collision, a registry pair naming an
    /// identity the install never minted, unresolvable registry type) —
    /// a refusal is fatal for the requesting body, exactly as the epoch's
    /// failed install fails the body query deterministically. A refusal also
    /// POISONS the pool's well-known registry: repeat installs re-error (still
    /// `None` here) and the accessors below answer as if nothing was installed
    /// — no observable partial success, matching the atomicity of the epoch's
    /// by-value install (which drops the whole mutated `BoundSema` on failure).
    pub fn install_well_known_option_types(
        &self,
        nominals: &[crate::AnonymousNominalKey<K, M>],
        option_by_payload: &[(SemanticImportType<K, M>, SemanticImportType<K, M>)],
    ) -> Option<()>
    where
        K: Ord,
        M: Ord,
    {
        self.identity
            .pool_mut()?
            .install_well_known_option_types(nominals, option_by_payload)
            .ok()
    }

    /// Whether a durable identity (any producer spelling — entry
    /// canonicalization applies) is a well-known `Option` identity installed
    /// on this driver's pool: the export-as-produced ruling the provider
    /// baseline subtraction consults (RUE-1091 r6c).
    pub fn is_well_known_option_identity(&self, durable: &crate::AnonymousNominalKey<K, M>) -> bool
    where
        K: Ord,
        M: Ord,
    {
        self.identity.pool().is_well_known_option_identity(durable)
    }

    /// The number of well-known `Option` identities installed on this driver's
    /// pool.
    pub fn well_known_option_identity_count(&self) -> usize {
        self.identity.pool().well_known_option_identity_count()
    }

    /// The trusted std `Option` enum minted for a demanded payload type, or
    /// `None` when the payload was never demanded — the pool-backed answer to
    /// the epoch's `well_known_option_by_payload` consult
    /// (`resolve_option_result_type`).
    pub fn well_known_option_for_payload(&self, payload: Type) -> Option<Type> {
        self.identity.pool().well_known_option_for_payload(payload)
    }

    pub fn durable_anonymous_identity(&self, ty: Type) -> Option<crate::AnonymousNominalKey<K, M>> {
        self.identity.pool().durable_anonymous_identity(ty)
    }

    pub fn durable_named_identity(&self, ty: Type) -> Option<K> {
        self.identity.pool().durable_named_identity(ty)
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
        let symbol = self.identity.pool().intern_name(name);
        let mut overlay = self.overlay.borrow_mut();
        if let Some(token) = overlay.definition_tokens.get(&key) {
            let token = *token;
            overlay.by_file_name.insert((file, symbol), key);
            return token;
        }
        let slot = overlay.next_slot;
        overlay.next_slot += 1;
        let token = SemanticDefinitionToken::new(OVERLAY_ISSUER, slot);
        overlay.tokens.insert(
            token,
            EndpointEntry {
                file,
                name: Arc::from(name),
                kind,
                owner: None,
            },
        );
        overlay.by_file_name.insert((file, symbol), key.clone());
        overlay.definition_tokens.insert(key, token);
        token
    }

    /// Register one free-function endpoint in the body-local token and callable
    /// overlays. The exact durable key supplies signature truth while the local
    /// RIR supplies the declaration/body handles when the endpoint is
    /// consulted.
    pub fn register_function(&self, key: K, file: FileId, name: &str) -> SemanticDefinitionToken {
        let symbol = self.identity.pool().intern_name(name);
        let mut overlay = self.overlay.borrow_mut();
        if let Some(token) = overlay.definition_tokens.get(&key) {
            let token = *token;
            overlay
                .functions_by_file_name
                .insert((file.index(), symbol), (symbol, key.clone()));
            overlay.function_keys.insert(symbol, key);
            return token;
        }
        let slot = overlay.next_slot;
        overlay.next_slot += 1;
        let token = SemanticDefinitionToken::new(OVERLAY_ISSUER, slot);
        overlay.tokens.insert(
            token,
            EndpointEntry {
                file: file.index(),
                name: Arc::from(name),
                kind: StableDefinitionKind::Function,
                owner: None,
            },
        );
        overlay
            .functions_by_file_name
            .insert((file.index(), symbol), (symbol, key.clone()));
        overlay.function_keys.insert(symbol, key.clone());
        overlay.definition_tokens.insert(key, token);
        token
    }

    /// Mint an endpoint token for the exact body owner. Unlike
    /// [`Self::register_function`], member and destructor owners are not
    /// inserted into the free-function lookup maps.
    pub fn register_body_owner(
        &self,
        key: K,
        file: FileId,
        name: &str,
        kind: StableDefinitionKind,
        owner: Option<&str>,
    ) -> SemanticDefinitionToken {
        if kind == StableDefinitionKind::Function && owner.is_none() {
            return self.register_function(key, file, name);
        }
        let mut overlay = self.overlay.borrow_mut();
        if let Some(token) = overlay.definition_tokens.get(&key) {
            return *token;
        }
        let slot = overlay.next_slot;
        overlay.next_slot += 1;
        let token = SemanticDefinitionToken::new(OVERLAY_ISSUER, slot);
        overlay.tokens.insert(
            token,
            EndpointEntry {
                file: file.index(),
                name: Arc::from(name),
                kind,
                owner: owner.map(Arc::from),
            },
        );
        overlay.definition_tokens.insert(key, token);
        token
    }

    /// Mint (or return) the body-local module token and compact module id for a
    /// durable module identity at its current request file. This is the
    /// provider-side analog of the epoch's `stable_module_endpoints` +
    /// `module_registry`: the durable identity prevents two registrations of
    /// one module from minting distinct units, while the file is the exact
    /// declaration-level request fact consumed by the endpoint lookup.
    ///
    /// A conflicting durable→file or file→durable registration is rejected
    /// rather than guessed. The flip fills this from the admitted durable module
    /// set keyed by `BodyFactProvider::ModuleRef`, paired with the canonical
    /// module projection's current file handle.
    pub fn register_module(
        &self,
        module: M,
        file: FileId,
        file_path: &str,
        import_path: &str,
        durable_id: &str,
    ) -> Option<SemanticModuleToken> {
        let id = self.identity.modules_mut().register(
            module,
            file,
            file_path,
            import_path,
            durable_id,
        )?;
        let token = SemanticModuleToken::new(OVERLAY_ISSUER, id.index());
        let mut overlay = self.overlay.borrow_mut();
        overlay.module_tokens.insert(
            token,
            SemanticModuleEndpoint {
                token,
                file: file.index(),
            },
        );
        Some(token)
    }

    /// Recover the current request file for a module type minted by this
    /// driver's body-local registry. Used by the cross-path differential to
    /// compare index-independent module identity.
    pub fn module_file(&self, ty: Type) -> Option<FileId> {
        let id = ty.as_module()?;
        self.identity
            .modules()
            .get(id)
            .map(|definition| definition.file_id)
    }

    /// Mint (on first sight) the generated slice struct for a `[T]` slice and
    /// record its name→id under the pool's own interner, so the `Slice` arm of
    /// [`resolve_instance_type`] resolves the generated-struct name through
    /// [`BodyEndpointProvider::generated_struct`] (RUE-1091 r6a — the builtin /
    /// slice name facts). The pool mints the fat-pointer struct byte-identically
    /// to the epoch's `get_or_create_slice_struct_from_element`
    /// (`import_type_local`'s slice arm), and dedups on repeat, so a second
    /// consult of the same `(element, name)` returns the same id and mints
    /// nothing new. The `element` is the slice's durable element type; a caller
    /// supplies the same durable element the epoch's slice carries.
    pub fn register_generated_slice(
        &self,
        element: &SemanticImportType<K, M>,
        name: &str,
    ) -> Option<StructId>
    where
        M: Clone,
    {
        let symbol = self.identity.pool().intern_name(name);
        let id = self
            .identity
            .pool_mut()?
            .resolve(&SemanticImportType::Slice {
                element: Box::new(element.clone()),
                name: Arc::from(name),
            })
            .ok()?
            .as_struct()?;
        self.overlay
            .borrow_mut()
            .generated_slices
            .insert(symbol, id);
        Some(id)
    }

    /// Mint (or return) one generated fixed-capacity string and publish its
    /// name in the same generated-nominal overlay used by slice views.
    pub fn register_generated_fixed_string(&self, capacity: u64) -> Option<StructId> {
        let name = format!("Str({capacity})");
        let symbol = self.identity.pool().intern_name(&name);
        let id = self
            .identity
            .pool_mut()?
            .get_or_create_str_fixed(capacity)
            .as_struct()?;
        self.overlay
            .borrow_mut()
            .generated_slices
            .insert(symbol, id);
        Some(id)
    }

    /// (P) Materialize a canonical type-instance key into a concrete pool
    /// [`Type`], reusing the provider-generic [`resolve_instance_type`] driven
    /// over this pool-backed [`BodyEndpointProvider`]. Every arm the pool
    /// supports resolves; a deferred arm (generic parameter, an unregistered
    /// anonymous mint, `Str(N)` builtin name) fails closed to
    /// `MissingStableIdentity`, exactly as the pool refuses it. Generated slice
    /// names resolve once seeded with [`Self::register_generated_slice`] (r6a).
    pub fn resolve_instance_type(
        &self,
        value: &TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    ) -> Result<Type, SemanticBodyExportFailure> {
        resolve_instance_type(self, value)
    }

    /// (R) The first free-function RIR declaration for `(source_name, file)`,
    /// answered by [`BodyRirIndex`] over the request-local `Rir` — the exact `InstRef`
    /// [`EpochFacts::first_free_function`] returns, keyed provider-naturally by
    /// the source name string.
    pub fn first_free_function(&self, source_name: &str, file: FileId) -> Option<InstRef> {
        let symbol = self.rir.rir_interner().get(source_name)?;
        self.rir.rir_index().first_free_function(symbol, file)
    }

    /// (R) The named-method RIR declaration for `(owner_file, owner_type_name,
    /// method)` — the durable-available preimage of the epoch's `(StructId,
    /// method)` key, answered by [`BodyRirIndex`]. Keyed by the preimage directly
    /// (provider-natural), matching the production seam.
    pub fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<InstRef> {
        let owner = self.rir.rir_interner().get(owner_type_name)?;
        let method_sym = self.rir.rir_interner().get(method)?;
        self.rir
            .rir_index()
            .named_method_declaration(owner_file, owner, method_sym)
    }

    /// (R) The destructor RIR declaration handle for `(file, type_name)`, the
    /// exact scan [`EpochFacts::destructor`] performs, answered by
    /// [`BodyRirIndex`]. Returns the located declaration `InstRef` (the private
    /// destructor record's public handle); the epoch keys the same record by the
    /// same `(file, type_name)` scan.
    pub fn destructor(&self, file: FileId, type_name: &str) -> Option<InstRef> {
        let symbol = self.rir.rir_interner().get(type_name)?;
        self.rir
            .rir_index()
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
        let pool = self.identity.type_pool();
        read(&pool)
    }

    /// Freeze the pool's containment metadata after every nominal the body
    /// consumes has been minted and before any drop/ownership read.
    /// The shared bundle/state path rebases the finalized base and permits
    /// later body-local minting in an append-only overlay. A pool without the
    /// shared bundle/state refuses the operation.
    /// `None` on a containment cycle (fail-closed). Before the freeze,
    /// [`Self::type_needs_drop`] / [`Self::type_carries_linear`] answer `None`.
    pub fn finalize_containment_metadata(&self) -> Option<()> {
        self.identity.finalize_containment_metadata()
    }

    /// Whether a minted type transitively needs drop, or `None` before the
    /// [`Self::finalize_containment_metadata`] freeze.
    pub fn type_needs_drop(&self, ty: Type) -> Option<bool> {
        self.identity.pool().type_needs_drop(ty)
    }

    /// Whether a minted type transitively carries a linear component, or `None`
    /// before the freeze.
    pub fn type_carries_linear(&self, ty: Type) -> Option<bool> {
        self.identity.pool().type_carries_linear(ty)
    }
}

impl<P, S, K, M> ProviderEndpointFacts<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M> + DurableAnonymousSource<K, M> + DurableConstSource<K, M>,
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
{
    /// Assemble the value constant identified by durable `key`, joining it to
    /// the exact current-RIR declaration at `(declaring_file, source_name)`.
    ///
    /// The RIR side supplies only the request-local span. The pool side supplies
    /// only declaration-level durable type/value truth. If either side is absent,
    /// or a nested identity cannot be minted exactly, this returns `None`.
    pub fn const_info(
        &self,
        key: &K,
        declaring_file: FileId,
        source_name: &str,
    ) -> Option<ConstInfo> {
        let name = self.rir.rir_interner().get(source_name)?;
        let declaration = self
            .rir
            .rir_index()
            .const_declaration(declaring_file, name)?;
        self.identity
            .pool_mut()?
            .resolve_const(
                key,
                ConstIdentityHandle {
                    span: self.rir.rir().get(declaration).span,
                },
            )
            .ok()
    }

    /// Assemble a module-valued constant from the exact declaration span and a
    /// module identity already admitted to this body's shared registry.
    pub fn module_binding_info(
        &self,
        declaring_file: FileId,
        source_name: &str,
        target: &M,
        is_public: bool,
    ) -> Option<ConstInfo> {
        let name = self.rir.rir_interner().get(source_name)?;
        let declaration = self
            .rir
            .rir_index()
            .const_declaration(declaring_file, name)?;
        let module = self.identity.modules().id_for_durable(target)?;
        let ty = Type::new_module(module);
        Some(ConstInfo {
            is_pub: is_public,
            ty,
            value: super::ConstValue::Type(ty),
            span: self.rir.rir().get(declaration).span,
        })
    }

    /// Resolve a pool-owned const symbol for index-independent differential
    /// comparison. Function and string values use the pool's own interner.
    pub fn resolve_const_symbol(&self, symbol: Spur) -> String {
        self.identity.pool().resolve_symbol(symbol).to_owned()
    }
}

impl<P, S, K, M> BodyEndpointProvider for ProviderEndpointFacts<'_, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M> + DurableAnonymousSource<K, M> + DurableCallableSource<K, M>,
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
{
    fn name_symbol(&self, name: &str) -> Option<Spur> {
        Some(self.identity.pool().intern_name(name))
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
            owner: entry.owner.clone(),
        })
    }

    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint> {
        self.overlay.borrow().module_tokens.get(&token).copied()
    }

    fn function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur> {
        self.overlay
            .borrow()
            .functions_by_file_name
            .get(&(file.index(), name))
            .map(|(symbol, _)| *symbol)
    }

    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        let key = self
            .overlay
            .borrow()
            .by_file_name
            .get(&(file.index(), name))
            .cloned()?;
        self.identity
            .pool_mut()?
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
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_enum()
    }

    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        let owned = self.identity.pool().resolve_symbol(name).to_owned();
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::BuiltinNominal {
                name: Arc::from(owned.as_str()),
                kind: SemanticImportNominalKind::Struct,
            })
            .ok()
            .and_then(|ty| ty.as_struct())
            // Mirror the epoch's `builtin_structs.or_else(generated_structs)`:
            // a generated slice struct answers here too (RUE-1091 r6a).
            .or_else(|| self.generated_struct(name))
    }

    fn generated_struct(&self, name: Spur) -> Option<StructId> {
        // The generated slice-struct name, minted and recorded by
        // `register_generated_slice` (RUE-1091 r6a — builtin / slice name
        // facts). A name never seeded fails closed, exactly as the epoch's
        // `generated_structs.get` misses.
        self.overlay.borrow().generated_slices.get(&name).copied()
    }

    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        let name = self.identity.pool().resolve_symbol(name).to_owned();
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::BuiltinNominal {
                name: Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Enum,
            })
            .ok()?
            .as_enum()
    }

    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        // RUE-1091 r6b: mint the anonymous struct through the pool for a durable
        // key seeded by `register_anonymous_nominal`. An unseeded issued key fails
        // closed (the pool never invents an identity), mirroring the epoch's
        // `anon_struct_identities.get` miss.
        let durable = self.anon_by_issued.borrow().get(identity).cloned()?;
        self.identity
            .pool_mut()?
            .find_or_create_anon(&durable)
            .ok()?
            .as_struct()
    }

    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        let durable = self.anon_by_issued.borrow().get(identity).cloned()?;
        self.identity
            .pool_mut()?
            .find_or_create_anon(&durable)
            .ok()?
            .as_enum()
    }

    fn function_info(&self, name: Spur) -> Option<FunctionInfo> {
        let key = self.overlay.borrow().function_keys.get(&name).cloned()?;
        let file = self
            .overlay
            .borrow()
            .functions_by_file_name
            .keys()
            .find_map(|(file, symbol)| (*symbol == name).then_some(FileId::new(*file)))?;
        let declaration = self.rir.rir_index().first_free_function(name, file)?;
        let inst = self.rir.rir().get(declaration);
        let rue_rir::InstData::FnDecl {
            body,
            return_type,
            is_extern,
            is_c_export,
            directives,
            ..
        } = &inst.data
        else {
            return None;
        };
        let allow = |warning_name: &str| {
            let allow_sym = self.rir.rir_interner().get("allow");
            let warning_sym = self.rir.rir_interner().get(warning_name);
            self.rir
                .rir()
                .directives(directives)
                .iter()
                .any(|directive| {
                    Some(directive.name) == allow_sym
                        && directive.args.iter().any(|arg| Some(*arg) == warning_sym)
                })
        };
        self.identity
            .pool_mut()?
            .resolve_function(
                &key,
                FunctionIdentityHandle {
                    body: *body,
                    declaration,
                    span: inst.span,
                    return_type_sym: *return_type,
                    is_extern: *is_extern,
                    is_c_export: *is_c_export,
                    allow_unused_function: allow("unused_function"),
                    allow_unused_variable: allow("unused_variable"),
                    allow_unreachable_code: allow("unreachable_code"),
                    file_id: inst.span.file_id,
                },
            )
            .ok()
    }

    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        // The shared registry preserves Sema's anonymous-before-named order.
        // Named entries are installed from the exact call fact before analysis
        // consumes the endpoint view; both facades then observe one authority.
        self.method_info(struct_id, name)
    }

    fn source_function_name(&self, name: Spur) -> Spur {
        // Identity: the specialization name map is r5.
        name
    }

    fn module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        self.identity.modules().id_for_file(FileId::new(file))
    }

    fn intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.identity
            .pool()
            .type_pool()
            .try_intern_array(element, len)
            .ok()
    }

    fn intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.identity
            .pool()
            .type_pool()
            .try_intern_ptr_const(pointee)
            .ok()
    }

    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.identity
            .pool()
            .type_pool()
            .try_intern_ptr_mut(pointee)
            .ok()
    }
}
