// `Sema`, `BoundSema`, and `DeclarationShells` are public legacy wrappers;
// one-body fact dispatch is hosted by the analyzer itself.
#![allow(private_bounds)]

//! Semantic analysis - RIR to AIR conversion.
//!
//! Sema performs type checking and converts untyped RIR to typed AIR.
//! This is analogous to Zig's Sema phase.
//!
//! # Module Organization
//!
//! This module is split into several submodules for maintainability:
//!
//! - [`context`] - Analysis context and helper types (LocalVar, AnalysisContext, etc.)
//! - [`declarations`] - Declaration gathering (register_type_names, resolve_declarations)
//! - [`builtins`] - Built-in type injection (String, etc.)
//! - [`typeck`] - Type resolution and checking helpers
//! - [`analysis`] - Function analysis, type inference coordination, and RIR-to-AIR lowering
//! - [`aggregates`] - Aggregate construction, fields, indexing, and enum operations
//! - [`control_flow`] - Branch, loop, match, try, return, and block analysis
//! - [`info`] - Function, method, and constant info types
//! - [`gather`] - Declaration gathering output
//! - [`output`] - Semantic analysis output types
//! - [`inference_ctx`] - Pre-computed type information for inference
//! - [`visibility`] - Module visibility checking
//! - [`anon_structs`] - Anonymous struct structural equality
//! - [`file_paths`] - File path management for multi-file compilation
//!
//! The main entry points are:
//! - [`Sema::new`] - Create a new semantic analyzer
//! - [`BoundSema::analyze_one_body_instance`] - Analyze one exact query-owned body
//! - [`BoundSema::compose_queried_bodies`] - Import queried bodies into final output

mod aggregate_resolution;
mod aggregates;
pub(crate) mod analysis;
mod analyze_ops;
mod anon_structs;
mod binding_manifest;
mod body_endpoint;
mod body_identity;
mod builtins;
mod call_resolution;
mod comptime_eval;
mod context;
mod control_flow;
mod declaration_base;
mod declaration_index;
mod declarations;
mod fact_mode;
mod file_paths;
mod inference_ctx;
mod info;
mod known_symbols;
mod metadata;
mod one_body;
mod ordinary_engine;
mod output;
pub mod provider;
mod provider_host;
mod provider_module_registry;
mod semantic_body_export;
mod typeck;
mod visibility;

// Public re-exports
pub use binding_manifest::{
    BoundSema, DeclarationBindingWork, DeclarationInstallFailure, DeclarationResolutionFailure,
    DeclarationShells, SemanticAnonymousMethodSignature, SemanticAnonymousMethodType,
    SemanticAnonymousNominalExport, SemanticAnonymousNominalIdentity,
    SemanticAnonymousNominalShape, SemanticBinding, SemanticBindingManifest,
    SemanticBindingManifestWork, SemanticDeclarationExport, SemanticDeclarationExportWork,
    SemanticDeclarationPayload, SemanticDeclarationShell, SemanticDeclarationShellIdentity,
    SemanticDefinitionIdentity, SemanticExportConstValue, SemanticExportFailure,
    SemanticExportParameter, SemanticExportType, SemanticNominalIdentity, SemanticParameterMode,
};
pub use context::ConstValue;
pub use declaration_base::EpochDerivationUnits;
pub use declaration_index::RirDeclarationIndexWork;
pub(crate) use fact_mode::BodyAnalysisHost;
pub(crate) use inference_ctx::HostInferenceFacts;
pub use inference_ctx::InferenceContext;
use info::ConstResolution;
pub use info::{AnonMethodSig, AnonMethodType, ConstInfo, FunctionInfo, MethodInfo};
pub use known_symbols::KnownSymbols;
pub use metadata::SemaMetadata;
pub use one_body::{
    OneBodyCanonicalArtifact, OneBodyDependency, OneBodyInterruption, OneBodyNonTerminalReason,
    OneBodyRequest, OneBodyTransactionOutcome, SemanticProducedAnonymousMethodSignature,
    SemanticProducedAnonymousMethodType, SemanticProducedAnonymousNominal,
    SemanticProducedAnonymousNominalShape,
};
pub use output::{
    AnalyzedBodyOwnerEvent, AnalyzedCallableKind, AnalyzedFunction, BodyAnalysisFailure,
    BodyAnalysisWork, BodyLookupCollector, BodyLookupObservation, BodyNamedDependencyEvent,
    BodyOwnerEndpoint, BodyOwnerKind, BodyOwnerToken, BuiltinTypeCallHead,
    DeclarationBuiltinTypeCallHeadDependencyEvent, DeclarationTypeCallHeadDependencyEvent,
    DeclarationTypeDependencyEvent, DeclarationTypeDependencyKind,
    DeclarationTypeDependencySourceKind, DeclarationTypeDependencyTargetKind,
    ImplicitDropDependencySourceEvent, ImplicitNamedDestructorDependencyEvent,
    NamedConstDependencyEvent, NamedConstDependencyTargetEvent, NamedDestructorDependencyEvent,
    NamedMethodDependencyEvent, NamedMethodDependencyTargetEvent,
    OrdinaryFreeFunctionDependencyEvent, ParamSlotModes, PerBodyDeclarationContextWork, SemaOutput,
    SourceParamAbi, SpecializedFreeFunctionDependencyEvent, SpecializedFreeFunctionOrigin,
};
pub use provider::{
    BodyFactProvider, DropCopyMetadata, ImportResolution, MemberCandidate, MemberKind,
    NameCandidate, NameResolution, NominalWellFormedness, OperatorMemberCandidate, OperatorName,
    ProviderDefinitionKind, ProviderNamespace,
};
// RUE-1091 slice r4b-1 flip-era surface: the durable source vocabulary the
// rue-compiler provider adapter supplies to the body identity pool, and the
// call-resolution ProviderFacts driver that composes pool + provider answers.
pub use aggregate_resolution::{
    ProviderAggregateFacts, ProviderModuleMember, ProviderQualifiedType, ProviderStructHead,
};
pub use body_endpoint::ProviderEndpointFacts;
pub use body_identity::{
    BodyRirBundle, BodyRirView, DurableAnonymousShape, DurableAnonymousSource,
    DurableCallableSource, DurableConst, DurableConstSource, DurableFunction, DurableMethod,
    DurableNominal, DurableNominalBody, DurableNominalSource, DurableSignatureParameter,
    ProviderBodyAnalysisState, ProviderIdentityContext,
};
pub use call_resolution::ProviderCallFacts;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileErrors, MultiErrorResult, PreviewFeatures};
use rue_rir::Rir;
use rue_span::{FileId, Span};
use rue_target::Target;

use crate::intern_pool::TypeInternPool;
use crate::param_arena::ParamArena;
use crate::types::{EnumId, StructId, Type};

/// Epoch-local authorization for evaluating one parser-owned const candidate.
/// The wrapper deliberately has no stable-definition export conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EpochLocalConstCandidateToken(crate::SemanticDefinitionToken);

#[derive(Debug, Clone)]
pub(crate) struct EpochLocalConstCandidateEndpoint {
    file: u32,
    name: String,
}

/// The source declaration namespace while declaration binding is in progress.
///
/// This wrapper is the only namespace state that implements `DerefMut`; after
/// binding, it is consumed into [`SourceDeclarations`], whose maps are
/// structurally read-only.
#[doc(hidden)]
pub struct MutableDeclarations(DeclarationNamespace);

/// The closed source declaration namespace consumed by body analysis.
///
/// The namespace is closed at the phase transition, so every body of a request
/// reads the same value. It is held behind an `Arc` and shared rather than
/// copied: deriving a body-local epoch from a request-scoped declaration base
/// (RUE-1135) bumps a refcount instead of copying O(declarations) map entries,
/// and the absence of `DerefMut` here is what makes that sharing sound — no
/// body can reach the shared namespace mutably.
#[doc(hidden)]
#[derive(Clone)]
pub struct SourceDeclarations(Arc<DeclarationNamespace>);

#[doc(hidden)]
pub struct DeclarationNamespace {
    functions: HashMap<Spur, FunctionInfo>,
    functions_by_file_name: HashMap<(FileId, Spur), Spur>,
    function_source_names: HashMap<Spur, Spur>,
    builtin_structs: HashMap<Spur, StructId>,
    structs_by_file_name: HashMap<(FileId, Spur), StructId>,
    builtin_enums: HashMap<Spur, EnumId>,
    enums_by_file_name: HashMap<(FileId, Spur), EnumId>,
    methods: HashMap<(StructId, Spur), MethodInfo>,
    named_method_declarations: HashMap<(StructId, Spur), rue_rir::InstRef>,
    const_resolutions: HashMap<(FileId, Spur), ConstResolution>,
}

impl DeclarationNamespace {
    fn new() -> Self {
        Self {
            functions: HashMap::new(),
            functions_by_file_name: HashMap::new(),
            function_source_names: HashMap::new(),
            builtin_structs: HashMap::new(),
            structs_by_file_name: HashMap::new(),
            builtin_enums: HashMap::new(),
            enums_by_file_name: HashMap::new(),
            methods: HashMap::new(),
            named_method_declarations: HashMap::new(),
            const_resolutions: HashMap::new(),
        }
    }

    fn value_const(&self, key: &(FileId, Spur)) -> Option<&ConstInfo> {
        self.const_resolutions
            .get(key)
            .and_then(ConstResolution::value)
    }

    fn module_binding(&self, key: &(FileId, Spur)) -> Option<&ConstInfo> {
        self.const_resolutions
            .get(key)
            .and_then(ConstResolution::module_binding)
    }

    fn value_consts(&self) -> impl Iterator<Item = (&(FileId, Spur), &ConstInfo)> {
        self.const_resolutions
            .iter()
            .filter_map(|(key, value)| value.value().map(|info| (key, info)))
    }

    // Only the `#[cfg(test)]` namespace-boundary snapshot enumerates module
    // bindings wholesale now; per-body inference resolves them by key through
    // the demand-population provider (RUE-1091 slice r5b).
    #[cfg_attr(not(test), allow(dead_code))]
    fn module_binding_consts(&self) -> impl Iterator<Item = (&(FileId, Spur), &ConstInfo)> {
        self.const_resolutions
            .iter()
            .filter_map(|(key, value)| value.module_binding().map(|info| (key, info)))
    }
}

impl std::ops::Deref for MutableDeclarations {
    type Target = DeclarationNamespace;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for MutableDeclarations {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl std::ops::Deref for SourceDeclarations {
    type Target = DeclarationNamespace;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[doc(hidden)]
pub trait DeclarationPhase: std::ops::Deref<Target = DeclarationNamespace> + Sized {
    fn resolve_indexed_const(
        sema: &mut Sema<'_, Self>,
        name: Spur,
        file_id: FileId,
    ) -> rue_error::CompileResult<Option<ConstValue>>;

    fn collect_free_function_signature(
        sema: &mut Sema<'_, Self>,
        target: Spur,
        file_id: Option<FileId>,
    ) -> rue_error::CompileResult<Option<(FileId, bool)>>;
}

impl DeclarationPhase for MutableDeclarations {
    fn resolve_indexed_const(
        sema: &mut Sema<'_, Self>,
        name: Spur,
        file_id: FileId,
    ) -> rue_error::CompileResult<Option<ConstValue>> {
        if !sema.declaration_binding_active {
            return Ok(None);
        }
        sema.resolve_indexed_const_binding_impl(name, file_id)
    }

    fn collect_free_function_signature(
        sema: &mut Sema<'_, Self>,
        target: Spur,
        file_id: Option<FileId>,
    ) -> rue_error::CompileResult<Option<(FileId, bool)>> {
        if !sema.declaration_binding_active {
            return Ok(None);
        }
        sema.collect_free_function_signature_binding_impl(target, file_id)
    }
}

impl DeclarationPhase for SourceDeclarations {
    fn resolve_indexed_const(
        _sema: &mut Sema<'_, Self>,
        _name: Spur,
        _file_id: FileId,
    ) -> rue_error::CompileResult<Option<ConstValue>> {
        Ok(None)
    }

    fn collect_free_function_signature(
        _sema: &mut Sema<'_, Self>,
        _target: Spur,
        _file_id: Option<FileId>,
    ) -> rue_error::CompileResult<Option<(FileId, bool)>> {
        Ok(None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredOwnershipGateKind {
    RequireDroppable,
    RequireTriviallyDroppable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeferredOwnershipGate {
    kind: DeferredOwnershipGateKind,
    ty: Type,
    span: Span,
}

/// Frozen declaration/read state installed once for all body epochs of a
/// request. It deliberately contains data, never an analyzer facade: body
/// epochs are constructed from this base plus their own mutable overlay.
pub(super) struct BodySemanticBase<'a> {
    declarations: SourceDeclarations,
    rir: &'a Rir,
    interner: &'a ThreadedRodeo,
    declaration_index: Arc<declaration_index::RirDeclarationIndex>,
    bound_const_candidates: Option<Arc<declaration_index::BoundConstCandidateIndex>>,
    synthetic_declaration_discovery: bool,
    named_callable_methods_by_symbol: Arc<HashMap<String, Vec<(StructId, Spur)>>>,
    stable_definition_tokens: Arc<
        HashMap<
            (u32, String, Option<String>, crate::StableDefinitionKind),
            crate::SemanticDefinitionToken,
        >,
    >,
    stable_definition_endpoints:
        Arc<HashMap<crate::SemanticDefinitionToken, crate::SemanticDefinitionEndpoint>>,
    const_candidate_tokens: Arc<HashMap<(u32, String), EpochLocalConstCandidateToken>>,
    const_candidate_endpoints:
        Arc<HashMap<EpochLocalConstCandidateToken, EpochLocalConstCandidateEndpoint>>,
    stable_module_tokens: Arc<HashMap<FileId, crate::SemanticModuleToken>>,
    stable_module_endpoints:
        Arc<HashMap<crate::SemanticModuleToken, crate::SemanticModuleEndpoint>>,
    body_owner_tokens: Arc<HashMap<(u32, String, Option<String>, BodyOwnerKind), BodyOwnerToken>>,
    preview_features: PreviewFeatures,
    target: Target,
    builtin_arch_id: Option<EnumId>,
    builtin_os_id: Option<EnumId>,
    builtin_data_model_id: Option<EnumId>,
    known: KnownSymbols,
    type_pool: TypeInternPool,
    module_registry: Arc<crate::module_registry::ModuleRegistry>,
    canonical_imports: Option<Arc<crate::canonical_imports::CanonicalImportContext>>,
    file_paths: Arc<HashMap<FileId, String>>,
    symbol_paths: Arc<HashMap<FileId, String>>,
    trusted_standard_library_files: Arc<HashSet<FileId>>,
    root_file_id: Option<FileId>,
    param_arena: ParamArena,
    local_seed: BodyLocalSeed,
}

/// Request-local identity state for receiver families that have crossed the
/// frozen body-epoch boundary. It owns an immutable epoch handle, never an
/// analyzer, plus a request-local lookup collector overlay.
pub(super) struct BodyAnalysisState<'a> {
    epoch: Arc<BodySemanticBase<'a>>,
    body_lookup_collector: Option<BodyLookupCollector>,
}

impl<'a> BodyAnalysisState<'a> {
    pub(super) fn from_body_semantic_base(epoch: Arc<BodySemanticBase<'a>>) -> Self {
        Self {
            epoch,
            body_lookup_collector: None,
        }
    }

    pub(super) fn install_body_lookup_collector(&mut self, collector: BodyLookupCollector) {
        self.body_lookup_collector = Some(collector);
    }

    pub(super) fn endpoint_facts(&self) -> body_endpoint::EpochFacts<'_, Self> {
        body_endpoint::EpochFacts::new(self)
    }

    /// Resolve the exact stable owner identity used by body publication.
    pub(super) fn body_owner_token(
        &self,
        file: FileId,
        name: &str,
        owner: Option<&str>,
        kind: BodyOwnerKind,
    ) -> BodyOwnerToken {
        resolve_body_owner_token(
            &self.epoch.body_owner_tokens,
            file,
            name,
            owner,
            kind,
            self.epoch.synthetic_declaration_discovery,
        )
    }
}

fn resolve_body_owner_token(
    tokens: &HashMap<(u32, String, Option<String>, BodyOwnerKind), BodyOwnerToken>,
    file: FileId,
    name: &str,
    owner: Option<&str>,
    kind: BodyOwnerKind,
    allow_fixture_identity: bool,
) -> BodyOwnerToken {
    let key = (
        file.index(),
        name.to_owned(),
        owner.map(str::to_owned),
        kind,
    );
    if tokens.is_empty() {
        if allow_fixture_identity {
            return BodyOwnerToken::new(0, file.index());
        }
        panic!("production body analysis requires installed body-owner tokens");
    }
    *tokens.get(&key).unwrap_or_else(|| {
        panic!(
            "supported ordinary body must have a validated owner token: {key:?}; installed={:?}",
            tokens.keys().collect::<Vec<_>>()
        )
    })
}

/// The declaration-time portion of a body-local overlay. Derivation copies
/// only this anonymous/generated state; all ordinary declaration maps live in
/// [`BodySemanticBase`].
struct BodyLocalSeed {
    anonymous_methods: HashMap<(StructId, Spur), MethodInfo>,
    anonymous_callable_methods_by_symbol: HashMap<String, Vec<(StructId, Spur)>>,
    generated_structs: HashMap<Spur, StructId>,
    generated_enums: HashMap<Spur, EnumId>,
    anon_struct_identities: HashMap<anon_structs::IssuedAnonymousNominalKey, StructId>,
    anon_enum_identities: HashMap<anon_structs::IssuedAnonymousNominalKey, EnumId>,
    anonymous_digest_owners: HashMap<u128, anon_structs::IssuedAnonymousNominalKey>,
    #[cfg(test)]
    forced_anonymous_digests: HashMap<anon_structs::IssuedAnonymousNominalKey, u128>,
    anonymous_struct_ids: HashSet<StructId>,
    anonymous_enum_ids: HashSet<EnumId>,
    canonical_anonymous_types: HashMap<Type, anon_structs::IssuedAnonymousNominalKey>,
    anon_struct_method_sigs: HashMap<StructId, Vec<AnonMethodSig>>,
    anon_struct_captured_values: HashMap<StructId, HashMap<Spur, ConstValue>>,
    anon_struct_type_subst: HashMap<StructId, HashMap<Spur, Type>>,
    destructor_spans: HashMap<StructId, Span>,
    infectious_linear: HashMap<StructId, (String, String)>,
    deferred_ownership_gates: Vec<DeferredOwnershipGate>,
    ctor_type_displays: HashMap<Type, String>,
    well_known_option_by_payload: HashMap<Type, Type>,
    well_known_option_identities:
        std::collections::BTreeSet<anon_structs::IssuedAnonymousNominalKey>,
}

/// Semantic analyzer that converts RIR to AIR.
pub struct Sema<'a, D: DeclarationPhase = MutableDeclarations> {
    declarations: D,
    pub(crate) rir: &'a Rir,
    pub(crate) interner: &'a ThreadedRodeo,
    /// Request-local declaration candidates for this exact RIR arena.
    /// Shared with every body derived from one declaration base (RUE-1135):
    /// the index is a pure function of the RIR arena and is never written after
    /// the base is prepared.
    declaration_index: Arc<declaration_index::RirDeclarationIndex>,
    /// Const locators admitted by the declaration-shell boundary. Canonical
    /// epochs populate this only from query-owned shells; it is installed
    /// immediately before declaration resolution begins.
    bound_const_candidates: Option<Arc<declaration_index::BoundConstCandidateIndex>>,
    /// Raw RIR declaration discovery is confined to synthetic component-test
    /// epochs. Canonical compiler epochs must import query-owned shells.
    synthetic_declaration_discovery: bool,
    /// Function table: maps internal function name symbols to their info.
    ///
    /// The internal key is normally the source name, but functions with the
    /// same source name in distinct files get deterministic module-qualified
    /// keys so they can coexist without colliding in AIR/codegen.
    /// Source declarations live in `declarations`; source-name lookup there is
    /// keyed by defining file/module. Internal function symbols remain the
    /// stable keys used by AIR and codegen.
    pub(crate) anonymous_methods: HashMap<(StructId, Spur), MethodInfo>,
    /// Reverse lookups from the AIR/codegen callable symbol to method keys.
    /// Keeping source and anonymous owners separate makes the language's
    /// named-first resolution precedence explicit even when their rendered
    /// symbols collide. Buckets retain collisions so ambiguity fails closed
    /// instead of depending on hash-map iteration order.
    pub(crate) named_callable_methods_by_symbol: Arc<HashMap<String, Vec<(StructId, Spur)>>>,
    pub(crate) anonymous_callable_methods_by_symbol: HashMap<String, Vec<(StructId, Spur)>>,
    /// Body-created synthetic and anonymous type-name overlays.
    pub(crate) generated_structs: HashMap<Spur, StructId>,
    pub(crate) generated_enums: HashMap<Spur, EnumId>,
    /// Producer-relative identities issued before anonymous pool allocation.
    pub(crate) anon_struct_identities: HashMap<anon_structs::IssuedAnonymousNominalKey, StructId>,
    pub(crate) anon_enum_identities: HashMap<anon_structs::IssuedAnonymousNominalKey, EnumId>,
    /// Digest-collision registry (RUE-1089, Theme 4b). Maps each anonymous-symbol
    /// digest to the exact `AnonymousNominalKey` that first minted it. The 128-bit
    /// digest spells presentation names only and must never decide type identity:
    /// a second, DISTINCT key hashing to a digest already owned here is a
    /// fail-closed internal error, never a silent pool/symbol (`StructId`/`EnumId`)
    /// reuse. Shared by both the struct and enum minting paths.
    pub(crate) anonymous_digest_owners: HashMap<u128, anon_structs::IssuedAnonymousNominalKey>,
    /// Test-only forced-digest hook (RUE-1089, Theme 4b). Maps specific
    /// `AnonymousNominalKey`s to a chosen digest so two DISTINCT producer keys can
    /// be pointed at one digest, exercising the fail-closed collision registry
    /// without depending on a real 128-bit hash collision. Keys absent here hash
    /// normally.
    #[cfg(test)]
    pub(crate) forced_anonymous_digests: HashMap<anon_structs::IssuedAnonymousNominalKey, u128>,
    /// O(1) reverse membership for source anonymous nominal compatibility.
    pub(crate) anonymous_struct_ids: HashSet<StructId>,
    pub(crate) anonymous_enum_ids: HashSet<EnumId>,
    /// Reverse canonical identity for live anonymous types. This is the only
    /// bridge used while recursively canonicalizing type arguments; retained
    /// keys never contain the live `Type` values used as lookup keys here.
    pub(crate) canonical_anonymous_types: HashMap<Type, anon_structs::IssuedAnonymousNominalKey>,
    /// Canonical producer currently issuing identities during body analysis.
    /// Entry-point wrappers scope this value around the shared body engine.
    pub(crate) active_anonymous_producer: Option<(
        anon_structs::IssuedStableProducerId,
        anon_structs::IssuedCanonicalArguments,
    )>,
    pub(crate) body_analysis_work: BodyAnalysisWork,
    pub(crate) analyzed_body_owners: Vec<AnalyzedBodyOwnerEvent>,
    pub(crate) ordinary_body_exports: Vec<crate::SemanticBodyExport>,
    pub(crate) specialized_body_exports: Vec<crate::SemanticSpecializedBodyExport>,
    pub(crate) reusable_ordinary_bodies: HashMap<
        BodyOwnerToken,
        crate::SemanticBodyCandidate<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    >,
    pub(crate) reusable_specialized_bodies: Vec<
        crate::SemanticSpecializedBodyCandidate<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    >,
    // The stable endpoint tables (RUE-1135). Installed once when a declaration
    // base is built and read-only for the whole of body analysis — body analysis
    // resolves tokens through them and never issues one — so every body derived
    // from one base shares them behind an `Arc` instead of copying
    // O(declarations) entries. A write outside the install stages fails to
    // compile here rather than silently copying.
    pub(crate) stable_definition_tokens: Arc<
        HashMap<
            (u32, String, Option<String>, crate::StableDefinitionKind),
            crate::SemanticDefinitionToken,
        >,
    >,
    pub(crate) stable_definition_endpoints:
        Arc<HashMap<crate::SemanticDefinitionToken, crate::SemanticDefinitionEndpoint>>,
    pub(crate) const_candidate_tokens: Arc<HashMap<(u32, String), EpochLocalConstCandidateToken>>,
    pub(crate) const_candidate_endpoints:
        Arc<HashMap<EpochLocalConstCandidateToken, EpochLocalConstCandidateEndpoint>>,
    pub(crate) stable_module_tokens: Arc<HashMap<FileId, crate::SemanticModuleToken>>,
    pub(crate) stable_module_endpoints:
        Arc<HashMap<crate::SemanticModuleToken, crate::SemanticModuleEndpoint>>,
    pub(crate) body_dependency_observer: Option<AnalyzedBodyOwnerEvent>,
    pub(crate) body_owner_tokens:
        Arc<HashMap<(u32, String, Option<String>, BodyOwnerKind), BodyOwnerToken>>,
    pub(crate) body_named_dependencies: Vec<BodyNamedDependencyEvent>,
    pub(crate) body_lookup_collector: Option<BodyLookupCollector>,
    pub(crate) one_body_error_recovery: bool,
    pub(crate) one_body_recovered_errors: Vec<rue_error::CompileError>,
    pub(crate) one_body_inference_failure_incomplete: bool,
    /// Anonymous identities already installed when an exact-one-body attempt
    /// starts. Successful export subtracts this baseline so the transaction
    /// publishes every nominal materialized by the body, including nested
    /// comptime producers, but never republishes imported owners.
    pub(crate) one_body_initial_anonymous_identities:
        std::collections::BTreeSet<anon_structs::IssuedAnonymousNominalKey>,
    pub(crate) one_body_requested_producer:
        Option<crate::StableProducerId<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
    pub(crate) body_callable_dependencies: Vec<(AnalyzedBodyOwnerEvent, Spur)>,
    pub(crate) body_specialization_dependencies: Vec<(
        AnalyzedBodyOwnerEvent,
        crate::FunctionInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    )>,
    pub(crate) ordinary_free_function_dependencies: Vec<OrdinaryFreeFunctionDependencyEvent>,
    pub(crate) specialized_free_function_origins: Vec<SpecializedFreeFunctionOrigin>,
    pub(crate) specialized_free_function_dependencies: Vec<SpecializedFreeFunctionDependencyEvent>,
    pub(crate) named_method_dependencies: Vec<NamedMethodDependencyEvent>,
    pub(crate) non_generic_named_method_dependencies_complete: bool,
    pub(crate) named_destructor_dependencies: Vec<NamedDestructorDependencyEvent>,
    pub(crate) declaration_type_dependencies: Vec<DeclarationTypeDependencyEvent>,
    pub(crate) declaration_type_call_head_dependencies: Vec<DeclarationTypeCallHeadDependencyEvent>,
    pub(crate) declaration_builtin_type_call_head_dependencies:
        Vec<DeclarationBuiltinTypeCallHeadDependencyEvent>,
    pub(crate) named_const_dependencies: Vec<NamedConstDependencyEvent>,
    pub(crate) named_const_dependency_source: Option<(FileId, String)>,
    pub(crate) declaration_type_observer: Option<(
        FileId,
        String,
        Option<String>,
        DeclarationTypeDependencySourceKind,
        DeclarationTypeDependencyKind,
    )>,
    /// Active dependency stack while declaration binding resolves constants.
    /// Empty outside the declaration-resolution phase; retained on `Sema` so
    /// recursive type resolution shares one cycle detector without rebuilding
    /// a declaration worklist.
    pub(crate) const_resolution_in_progress: Vec<(FileId, Spur)>,
    /// True only while the current-revision declaration payloads are being
    /// resolved. Once `BoundSema` exists, missing constants are authoritative
    /// lookup misses and may never restart declaration discovery.
    pub(crate) declaration_binding_active: bool,
    /// Enabled preview features
    pub(crate) preview_features: PreviewFeatures,
    /// Requested compilation target.
    pub(crate) target: Target,
    /// EnumId of the synthetic Arch enum (for @target_arch intrinsic).
    pub(crate) builtin_arch_id: Option<EnumId>,
    /// EnumId of the synthetic Os enum (for @target_os intrinsic).
    pub(crate) builtin_os_id: Option<EnumId>,
    /// EnumId of the synthetic DataModel enum (for @target_data_model
    /// intrinsic, ADR-0064 Amendment 1).
    pub(crate) builtin_data_model_id: Option<EnumId>,
    /// Pre-interned known symbols for fast comparison.
    pub(crate) known: KnownSymbols,
    /// Canonical composite-type storage for this semantic epoch (ADR-0024).
    pub(crate) type_pool: TypeInternPool,
    /// Canonically prepopulated module registry for the current semantic epoch,
    /// read-only once installed: body analysis resolves modules through the
    /// registry and never registers one, so it is shared across the bodies of a
    /// declaration base (RUE-1135).
    pub(crate) module_registry: Arc<crate::module_registry::ModuleRegistry>,
    /// Accepted canonical import-site identities for this semantic epoch.
    pub(crate) canonical_imports: Option<Arc<crate::canonical_imports::CanonicalImportContext>>,
    /// Maps FileId to request-local source paths used for presentation.
    pub(crate) file_paths: Arc<HashMap<FileId, String>>,
    /// Maps FileId to relocation-stable paths used in generated symbols.
    pub(crate) symbol_paths: Arc<HashMap<FileId, String>>,
    /// FileIds whose standard-library provenance was established by the
    /// frontend's import resolver, rather than inferred from path spelling.
    pub(crate) trusted_standard_library_files: Arc<HashSet<FileId>>,
    /// Explicit semantic root module for root-relative import fallback.
    pub(crate) root_file_id: Option<FileId>,
    /// Arena storage for function/method parameter data.
    pub(crate) param_arena: ParamArena,
    /// Method signatures for anonymous structs, used for structural equality comparison.
    pub(crate) anon_struct_method_sigs: HashMap<StructId, Vec<AnonMethodSig>>,
    /// Captured comptime values for anonymous structs.
    /// When an anonymous struct with methods is created inside a comptime function,
    /// the comptime parameter values (e.g., N=42 in FixedBuffer(comptime N: i32)) are
    /// stored here, keyed by StructId. These values become part of type identity:
    /// FixedBuffer(42) and FixedBuffer(100) are different types.
    pub(crate) anon_struct_captured_values: HashMap<StructId, HashMap<Spur, ConstValue>>,
    /// Captured comptime *type* substitution for anonymous structs produced by
    /// a `-> type` comptime constructor. When `Vec(comptime T: type)` is
    /// instantiated as `Vec(i32)`, this stores `T -> i32` keyed by the
    /// resulting StructId, so the enclosing type parameter resolves not just in
    /// field/method *signatures* (done at registration) but throughout every
    /// method *body* at analysis time (RUE-313). Empty for non-generic anon
    /// structs.
    pub(crate) anon_struct_type_subst: HashMap<StructId, HashMap<Spur, Type>>,
    /// Span of each user-defined `drop fn` declaration, keyed by the struct
    /// it destructs. Used by diagnostics that point at the destructor
    /// (E0456 field-move-out-of-destructor-type, E0457 @copy-with-destructor).
    /// Builtin destructors (e.g. String's) have no entry.
    pub(crate) destructor_spans: HashMap<StructId, Span>,
    /// Structs that became linear via infectious linearity (RUE-40): not
    /// declared `linear`, but containing a field that carries a linear value.
    /// Maps the struct to the (field name, field type name) that caused it,
    /// for diagnostics explaining why the container is linear.
    pub(crate) infectious_linear: HashMap<StructId, (String, String)>,
    /// Ownership gates reached while declaration payloads are still mutable.
    /// Their negative result is not authoritative until nominal payloads,
    /// infectious linearity, and destructors have all been finalized.
    pub(crate) deferred_ownership_gates: Vec<DeferredOwnershipGate>,
    /// Current recursion depth of `-> type` comptime-function reduction
    /// (`eval_comptime_type_call`). Unlike value functions — whose comptime
    /// recursion is bounded by the specialization-round limit — a `-> type`
    /// function reduces eagerly on the host stack, so an unbounded
    /// self-recursive type constructor (`fn Bad() -> type { Bad() }`) would
    /// overflow that stack (SIGABRT) without this guard. Bounded at
    /// `MAX_SPECIALIZATION_ROUNDS`, emitting the same E1200 (RUE-261).
    pub(crate) comptime_type_call_depth: usize,
    /// Internal names of free functions whose signatures are mid-collection in
    /// [`Self::collect_free_function_signature_during_binding`]. Collecting a
    /// signature resolves its parameter/return types, which may name other type
    /// constructors and collect *them* on demand (RUE-603); a signature cycle
    /// (`fn A(x: B(i32))` / `fn B(x: A(i32))`) must not re-enter a
    /// mid-collection signature, or the reduction would recurse forever.
    pub(crate) fn_signatures_in_flight: HashSet<Spur>,
    /// Human-readable instantiation spellings for type-constructor-produced
    /// anonymous types (`ArrayBuf(i64)` for the anon struct otherwise named
    /// `__anon_struct_4`), recorded when [`Self::reduce_type_ctor_body`]
    /// reduces a constructor application. Consulted by
    /// [`Self::format_type_name`] so diagnostics print the spelling the user
    /// wrote (RUE-610). First writer wins: structurally-deduped types keep
    /// the spelling of their first instantiation.
    pub(crate) ctor_type_displays: HashMap<Type, String>,
    /// Per-body well-known `Option(payload)` registry (RUE-1112). Maps an
    /// expected payload `Type` to the trusted standard-library `Option` enum
    /// specialized with that payload. Populated narrowly, before body analysis,
    /// from the per-body `WellKnownTypes` demand resolution — never from the
    /// body's own composition/import universe. Consulted by fallible-intrinsic
    /// resolution (`resolve_option_result_type`) so `@parse_*`/`@read_line`
    /// bind the exact trusted std `Option(payload)` in every context even when
    /// the body does not `@import` it. Production installs the complete per-body
    /// registry before analysis; a missing entry fails closed instead of adopting
    /// a structurally compatible body-local enum.
    pub(crate) well_known_option_by_payload: HashMap<Type, Type>,
    /// Anonymous enum identities materialized by the well-known `Option`
    /// registry install for this body. They are subtracted from the initial
    /// baseline so the installing body EXPORTS them as produced anonymous
    /// nominals — exactly as a body that materializes `Option(payload)` through
    /// the ordinary annotation/comptime path would. This is what makes the
    /// registry-rooted identity durable across composition: the body that binds
    /// a fallible intrinsic's `Option` is the body that produces it, so a
    /// consumer (including this same body under `?`) resolves one identity
    /// instead of failing import-only composition with `StableResolution(Missing)`.
    pub(crate) well_known_option_identities:
        std::collections::BTreeSet<anon_structs::IssuedAnonymousNominalKey>,
}

impl<'a> BodySema<'a> {
    /// Freeze the immutable declaration/read universe used by every body
    /// derived from this request. The base is data-only; construction of a
    /// body epoch remains the sole responsibility of this production analyzer.
    pub(super) fn body_semantic_base(&self) -> BodySemanticBase<'a> {
        BodySemanticBase {
            declarations: self.declarations.clone(),
            rir: self.rir,
            interner: self.interner,
            declaration_index: self.declaration_index.clone(),
            bound_const_candidates: self.bound_const_candidates.clone(),
            synthetic_declaration_discovery: self.synthetic_declaration_discovery,
            named_callable_methods_by_symbol: self.named_callable_methods_by_symbol.clone(),
            stable_definition_tokens: self.stable_definition_tokens.clone(),
            stable_definition_endpoints: self.stable_definition_endpoints.clone(),
            const_candidate_tokens: self.const_candidate_tokens.clone(),
            const_candidate_endpoints: self.const_candidate_endpoints.clone(),
            stable_module_tokens: self.stable_module_tokens.clone(),
            stable_module_endpoints: self.stable_module_endpoints.clone(),
            body_owner_tokens: self.body_owner_tokens.clone(),
            preview_features: self.preview_features.clone(),
            target: self.target.clone(),
            builtin_arch_id: self.builtin_arch_id,
            builtin_os_id: self.builtin_os_id,
            builtin_data_model_id: self.builtin_data_model_id,
            known: self.known.clone(),
            type_pool: self.type_pool.derive_overlay(),
            module_registry: self.module_registry.clone(),
            canonical_imports: self.canonical_imports.clone(),
            file_paths: self.file_paths.clone(),
            symbol_paths: self.symbol_paths.clone(),
            trusted_standard_library_files: self.trusted_standard_library_files.clone(),
            root_file_id: self.root_file_id,
            param_arena: self.param_arena.derive_overlay(),
            local_seed: BodyLocalSeed {
                anonymous_methods: self.anonymous_methods.clone(),
                anonymous_callable_methods_by_symbol: self
                    .anonymous_callable_methods_by_symbol
                    .clone(),
                generated_structs: self.generated_structs.clone(),
                generated_enums: self.generated_enums.clone(),
                anon_struct_identities: self.anon_struct_identities.clone(),
                anon_enum_identities: self.anon_enum_identities.clone(),
                anonymous_digest_owners: self.anonymous_digest_owners.clone(),
                #[cfg(test)]
                forced_anonymous_digests: self.forced_anonymous_digests.clone(),
                anonymous_struct_ids: self.anonymous_struct_ids.clone(),
                anonymous_enum_ids: self.anonymous_enum_ids.clone(),
                canonical_anonymous_types: self.canonical_anonymous_types.clone(),
                anon_struct_method_sigs: self.anon_struct_method_sigs.clone(),
                anon_struct_captured_values: self.anon_struct_captured_values.clone(),
                anon_struct_type_subst: self.anon_struct_type_subst.clone(),
                destructor_spans: self.destructor_spans.clone(),
                infectious_linear: self.infectious_linear.clone(),
                deferred_ownership_gates: self.deferred_ownership_gates.clone(),
                ctor_type_displays: self.ctor_type_displays.clone(),
                well_known_option_by_payload: self.well_known_option_by_payload.clone(),
                well_known_option_identities: self.well_known_option_identities.clone(),
            },
        }
    }

    /// Build one independent ordinary body epoch from immutable base data and
    /// freshly owned body-local state. This intentionally does not clone a
    /// `Sema` or delegate construction through one.
    pub(super) fn derive_from_body_semantic_base(base: &BodySemanticBase<'a>) -> Self {
        let local = &base.local_seed;
        Self {
            declarations: base.declarations.clone(),
            rir: base.rir,
            interner: base.interner,
            declaration_index: base.declaration_index.clone(),
            bound_const_candidates: base.bound_const_candidates.clone(),
            synthetic_declaration_discovery: base.synthetic_declaration_discovery,
            anonymous_methods: local.anonymous_methods.clone(),
            named_callable_methods_by_symbol: base.named_callable_methods_by_symbol.clone(),
            anonymous_callable_methods_by_symbol: local
                .anonymous_callable_methods_by_symbol
                .clone(),
            generated_structs: local.generated_structs.clone(),
            generated_enums: local.generated_enums.clone(),
            anon_struct_identities: local.anon_struct_identities.clone(),
            anon_enum_identities: local.anon_enum_identities.clone(),
            anonymous_digest_owners: local.anonymous_digest_owners.clone(),
            #[cfg(test)]
            forced_anonymous_digests: local.forced_anonymous_digests.clone(),
            anonymous_struct_ids: local.anonymous_struct_ids.clone(),
            anonymous_enum_ids: local.anonymous_enum_ids.clone(),
            canonical_anonymous_types: local.canonical_anonymous_types.clone(),
            active_anonymous_producer: None,
            body_analysis_work: BodyAnalysisWork::default(),
            analyzed_body_owners: Vec::new(),
            ordinary_body_exports: Vec::new(),
            specialized_body_exports: Vec::new(),
            reusable_ordinary_bodies: HashMap::new(),
            reusable_specialized_bodies: Vec::new(),
            stable_definition_tokens: base.stable_definition_tokens.clone(),
            stable_definition_endpoints: base.stable_definition_endpoints.clone(),
            const_candidate_tokens: base.const_candidate_tokens.clone(),
            const_candidate_endpoints: base.const_candidate_endpoints.clone(),
            stable_module_tokens: base.stable_module_tokens.clone(),
            stable_module_endpoints: base.stable_module_endpoints.clone(),
            body_dependency_observer: None,
            body_owner_tokens: base.body_owner_tokens.clone(),
            body_named_dependencies: Vec::new(),
            body_lookup_collector: None,
            one_body_error_recovery: false,
            one_body_recovered_errors: Vec::new(),
            one_body_inference_failure_incomplete: false,
            one_body_initial_anonymous_identities: std::collections::BTreeSet::new(),
            one_body_requested_producer: None,
            body_callable_dependencies: Vec::new(),
            body_specialization_dependencies: Vec::new(),
            ordinary_free_function_dependencies: Vec::new(),
            specialized_free_function_origins: Vec::new(),
            specialized_free_function_dependencies: Vec::new(),
            named_method_dependencies: Vec::new(),
            non_generic_named_method_dependencies_complete: false,
            named_destructor_dependencies: Vec::new(),
            declaration_type_dependencies: Vec::new(),
            declaration_type_call_head_dependencies: Vec::new(),
            declaration_builtin_type_call_head_dependencies: Vec::new(),
            named_const_dependencies: Vec::new(),
            named_const_dependency_source: None,
            declaration_type_observer: None,
            const_resolution_in_progress: Vec::new(),
            declaration_binding_active: false,
            preview_features: base.preview_features.clone(),
            target: base.target.clone(),
            builtin_arch_id: base.builtin_arch_id,
            builtin_os_id: base.builtin_os_id,
            builtin_data_model_id: base.builtin_data_model_id,
            known: base.known.clone(),
            type_pool: base.type_pool.derive_overlay(),
            module_registry: base.module_registry.clone(),
            canonical_imports: base.canonical_imports.clone(),
            file_paths: base.file_paths.clone(),
            symbol_paths: base.symbol_paths.clone(),
            trusted_standard_library_files: base.trusted_standard_library_files.clone(),
            root_file_id: base.root_file_id,
            param_arena: base.param_arena.derive_overlay(),
            anon_struct_method_sigs: local.anon_struct_method_sigs.clone(),
            anon_struct_captured_values: local.anon_struct_captured_values.clone(),
            anon_struct_type_subst: local.anon_struct_type_subst.clone(),
            destructor_spans: local.destructor_spans.clone(),
            infectious_linear: local.infectious_linear.clone(),
            deferred_ownership_gates: local.deferred_ownership_gates.clone(),
            comptime_type_call_depth: 0,
            fn_signatures_in_flight: HashSet::new(),
            ctor_type_displays: local.ctor_type_displays.clone(),
            well_known_option_by_payload: local.well_known_option_by_payload.clone(),
            well_known_option_identities: local.well_known_option_identities.clone(),
        }
    }
}

impl<D: DeclarationPhase> std::ops::Deref for Sema<'_, D> {
    type Target = DeclarationNamespace;

    fn deref(&self) -> &Self::Target {
        &self.declarations
    }
}

impl<'a, D: DeclarationPhase> Sema<'a, D> {
    /// Change only the declaration-phase capability while preserving every
    /// other analyzer field by value. Keeping this conversion exhaustive makes
    /// additions to `Sema` fail to compile here until their phase-boundary
    /// semantics are reviewed.
    fn map_declarations<E: DeclarationPhase>(self, map: impl FnOnce(D) -> E) -> Sema<'a, E> {
        let Sema {
            declarations,
            rir,
            interner,
            declaration_index,
            bound_const_candidates,
            synthetic_declaration_discovery,
            anonymous_methods,
            named_callable_methods_by_symbol,
            anonymous_callable_methods_by_symbol,
            generated_structs,
            generated_enums,
            anon_struct_identities,
            anon_enum_identities,
            anonymous_digest_owners,
            #[cfg(test)]
            forced_anonymous_digests,
            anonymous_struct_ids,
            anonymous_enum_ids,
            canonical_anonymous_types,
            active_anonymous_producer,
            body_analysis_work,
            analyzed_body_owners,
            ordinary_body_exports,
            specialized_body_exports,
            reusable_ordinary_bodies,
            reusable_specialized_bodies,
            stable_definition_tokens,
            stable_definition_endpoints,
            const_candidate_tokens,
            const_candidate_endpoints,
            stable_module_tokens,
            stable_module_endpoints,
            body_dependency_observer,
            body_owner_tokens,
            body_named_dependencies,
            body_lookup_collector,
            one_body_error_recovery,
            one_body_recovered_errors,
            one_body_inference_failure_incomplete,
            one_body_initial_anonymous_identities,
            one_body_requested_producer,
            body_callable_dependencies,
            body_specialization_dependencies,
            ordinary_free_function_dependencies,
            specialized_free_function_origins,
            specialized_free_function_dependencies,
            named_method_dependencies,
            non_generic_named_method_dependencies_complete,
            named_destructor_dependencies,
            declaration_type_dependencies,
            declaration_type_call_head_dependencies,
            declaration_builtin_type_call_head_dependencies,
            named_const_dependencies,
            named_const_dependency_source,
            declaration_type_observer,
            const_resolution_in_progress,
            declaration_binding_active,
            preview_features,
            target,
            builtin_arch_id,
            builtin_os_id,
            builtin_data_model_id,
            known,
            type_pool,
            module_registry,
            canonical_imports,
            file_paths,
            symbol_paths,
            trusted_standard_library_files,
            root_file_id,
            param_arena,
            anon_struct_method_sigs,
            anon_struct_captured_values,
            anon_struct_type_subst,
            destructor_spans,
            infectious_linear,
            deferred_ownership_gates,
            comptime_type_call_depth,
            fn_signatures_in_flight,
            ctor_type_displays,
            well_known_option_by_payload,
            well_known_option_identities,
        } = self;
        Sema {
            declarations: map(declarations),
            rir,
            interner,
            declaration_index,
            bound_const_candidates,
            synthetic_declaration_discovery,
            anonymous_methods,
            named_callable_methods_by_symbol,
            anonymous_callable_methods_by_symbol,
            generated_structs,
            generated_enums,
            anon_struct_identities,
            anon_enum_identities,
            anonymous_digest_owners,
            #[cfg(test)]
            forced_anonymous_digests,
            anonymous_struct_ids,
            anonymous_enum_ids,
            canonical_anonymous_types,
            active_anonymous_producer,
            body_analysis_work,
            analyzed_body_owners,
            ordinary_body_exports,
            specialized_body_exports,
            reusable_ordinary_bodies,
            reusable_specialized_bodies,
            stable_definition_tokens,
            stable_definition_endpoints,
            const_candidate_tokens,
            const_candidate_endpoints,
            stable_module_tokens,
            stable_module_endpoints,
            body_dependency_observer,
            body_owner_tokens,
            body_named_dependencies,
            body_lookup_collector,
            one_body_error_recovery,
            one_body_recovered_errors,
            one_body_inference_failure_incomplete,
            one_body_initial_anonymous_identities,
            one_body_requested_producer,
            body_callable_dependencies,
            body_specialization_dependencies,
            ordinary_free_function_dependencies,
            specialized_free_function_origins,
            specialized_free_function_dependencies,
            named_method_dependencies,
            non_generic_named_method_dependencies_complete,
            named_destructor_dependencies,
            declaration_type_dependencies,
            declaration_type_call_head_dependencies,
            declaration_builtin_type_call_head_dependencies,
            named_const_dependencies,
            named_const_dependency_source,
            declaration_type_observer,
            const_resolution_in_progress,
            declaration_binding_active,
            preview_features,
            target,
            builtin_arch_id,
            builtin_os_id,
            builtin_data_model_id,
            known,
            type_pool,
            module_registry,
            canonical_imports,
            file_paths,
            symbol_paths,
            trusted_standard_library_files,
            root_file_id,
            param_arena,
            anon_struct_method_sigs,
            anon_struct_captured_values,
            anon_struct_type_subst,
            destructor_spans,
            infectious_linear,
            deferred_ownership_gates,
            comptime_type_call_depth,
            fn_signatures_in_flight,
            ctor_type_displays,
            well_known_option_by_payload,
            well_known_option_identities,
        }
    }

    pub(crate) fn function_info(&self, name: Spur) -> Option<&FunctionInfo> {
        self.functions.get(&name)
    }

    pub(crate) fn method_info(&self, key: (StructId, Spur)) -> Option<&MethodInfo> {
        self.anonymous_methods
            .get(&key)
            .or_else(|| self.methods.get(&key))
    }

    pub(crate) fn method_by_callable_symbol(
        &self,
        symbol: Spur,
    ) -> Option<(StructId, Spur, &MethodInfo)> {
        let symbol = self.interner.resolve(&symbol);
        let (anonymous, (struct_id, method_name)) = self.callable_method_key_by_symbol(symbol)?;
        let info = if anonymous {
            self.anonymous_methods.get(&(struct_id, method_name))?
        } else {
            self.methods.get(&(struct_id, method_name))?
        };
        Some((struct_id, method_name, info))
    }

    fn callable_method_key_by_symbol(&self, symbol: &str) -> Option<(bool, (StructId, Spur))> {
        if let Some(candidates) = self.named_callable_methods_by_symbol.get(symbol) {
            let &[key] = candidates.as_slice() else {
                return None;
            };
            return Some((false, key));
        }
        let &[key] = self
            .anonymous_callable_methods_by_symbol
            .get(symbol)?
            .as_slice()
        else {
            return None;
        };
        Some((true, key))
    }

    pub(crate) fn named_method_by_callable_symbol(
        &self,
        symbol: Spur,
    ) -> Option<(StructId, Spur, &MethodInfo)> {
        let &[(struct_id, method_name)] = self
            .named_callable_methods_by_symbol
            .get(self.interner.resolve(&symbol))?
            .as_slice()
        else {
            return None;
        };
        Some((
            struct_id,
            method_name,
            self.methods.get(&(struct_id, method_name))?,
        ))
    }

    fn insert_callable_method_candidate(
        candidates: &mut HashMap<String, Vec<(StructId, Spur)>>,
        symbol: String,
        key: (StructId, Spur),
    ) {
        let bucket = candidates.entry(symbol).or_default();
        if !bucket.contains(&key) {
            bucket.push(key);
        }
    }

    pub(crate) fn index_anonymous_callable_method(
        &mut self,
        struct_id: StructId,
        method_name: Spur,
        has_self: bool,
    ) {
        let symbol = self.method_symbol(struct_id, self.interner.resolve(&method_name), has_self);
        // Queried-body import resolves calls by their final AIR/codegen symbol.
        // Projected anonymous definitions therefore intern that symbol at
        // owner installation time, before the import-only composer validates
        // any body that references the member.
        self.interner.get_or_intern(&symbol);
        Self::insert_callable_method_candidate(
            &mut self.anonymous_callable_methods_by_symbol,
            symbol,
            (struct_id, method_name),
        );
    }

    fn rebuild_callable_method_index(&mut self) {
        let named_methods = self
            .methods
            .iter()
            .map(|(&(struct_id, method_name), info)| (struct_id, method_name, info.has_self))
            .collect::<Vec<_>>();
        let anonymous_methods = self
            .anonymous_methods
            .iter()
            .map(|(&(struct_id, method_name), info)| (struct_id, method_name, info.has_self))
            .collect::<Vec<_>>();
        let named_symbols = named_methods
            .iter()
            .map(|&(struct_id, method_name, has_self)| {
                (
                    self.method_symbol(struct_id, self.interner.resolve(&method_name), has_self),
                    (struct_id, method_name),
                )
            })
            .collect::<Vec<_>>();
        let named_callable_methods_by_symbol =
            Arc::make_mut(&mut self.named_callable_methods_by_symbol);
        named_callable_methods_by_symbol.clear();
        named_callable_methods_by_symbol.reserve(named_methods.len());
        for (symbol, key) in named_symbols {
            Self::insert_callable_method_candidate(named_callable_methods_by_symbol, symbol, key);
        }
        self.anonymous_callable_methods_by_symbol.clear();
        self.anonymous_callable_methods_by_symbol
            .reserve(anonymous_methods.len());
        for (struct_id, method_name, has_self) in anonymous_methods {
            self.index_anonymous_callable_method(struct_id, method_name, has_self);
        }
    }

    pub(crate) fn has_method(&self, key: (StructId, Spur)) -> bool {
        self.method_info(key).is_some()
    }

    pub(crate) fn struct_id_for_name(&self, name: Spur) -> Option<StructId> {
        self.generated_structs
            .get(&name)
            .or_else(|| self.builtin_structs.get(&name))
            .copied()
    }

    pub(crate) fn enum_id_for_name(&self, name: Spur) -> Option<EnumId> {
        self.generated_enums
            .get(&name)
            .or_else(|| self.builtin_enums.get(&name))
            .copied()
    }

    pub(crate) fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        span: Span,
    ) -> rue_error::CompileResult<()> {
        if self.preview_features.contains(&feature) {
            Ok(())
        } else {
            Err(rue_error::CompileError::new(
                rue_error::ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_string(),
                },
                span,
            )
            .with_help(format!(
                "use `--preview {}` to enable this feature ({})",
                feature.name(),
                feature.adr()
            )))
        }
    }

    pub(crate) fn collect_free_function_signature_during_binding(
        &mut self,
        target: Spur,
        file_id: Option<FileId>,
    ) -> rue_error::CompileResult<Option<(FileId, bool)>> {
        D::collect_free_function_signature(self, target, file_id)
    }

    pub(crate) fn record_named_const_dependency(
        &mut self,
        target: NamedConstDependencyTargetEvent,
    ) {
        // Constant evaluation is shared by declaration binding and body
        // analysis. A body can evaluate a named constant completely away
        // while still exporting semantics derived from its value (for
        // example, the length in an anonymous array field). Preserve that
        // exact provenance on the active body as well as on an active
        // declaration so one-body query validation observes the producer.
        self.record_body_named_dependency(target.clone());
        let Some((file, name)) = self.named_const_dependency_source.clone() else {
            return;
        };
        self.named_const_dependencies
            .push(NamedConstDependencyEvent {
                source_file: file.index(),
                source_name: name,
                target,
            });
        self.body_analysis_work.named_const_dependency_events += 1;
    }

    pub(crate) fn record_body_named_dependency(&mut self, target: NamedConstDependencyTargetEvent) {
        let Some(source) = self.body_dependency_observer.clone() else {
            return;
        };
        self.body_named_dependencies
            .push(BodyNamedDependencyEvent { source, target });
        self.body_analysis_work.named_const_dependency_events += 1;
    }

    pub(crate) fn record_body_module_item_lookup(&self, file: FileId, name: Spur) {
        let Some(collector) = &self.body_lookup_collector else {
            return;
        };
        collector.record(BodyLookupObservation::ModuleItem {
            file: file.index(),
            name: std::sync::Arc::from(self.interner.resolve(&name)),
        });
    }

    pub(crate) fn record_body_member_lookup(&self, owner: StructId, member: Spur) {
        let Some(collector) = &self.body_lookup_collector else {
            return;
        };
        if self.anonymous_struct_ids.contains(&owner) {
            return;
        }
        let Some(definition) = self.type_pool.try_struct_def(owner) else {
            return;
        };
        if definition.is_builtin {
            return;
        }
        collector.record(BodyLookupObservation::Member {
            owner_file: definition.file_id.index(),
            owner_name: std::sync::Arc::from(definition.name),
            member_name: std::sync::Arc::from(self.interner.resolve(&member)),
        });
    }

    pub(crate) fn record_body_callable_dependency(&mut self, symbol: Spur) {
        let Some(source) = self.body_dependency_observer.clone() else {
            return;
        };
        self.body_callable_dependencies.push((source, symbol));
    }

    pub(crate) fn validate_explicit_call_modes<A>(
        &self,
        args: A,
        expected_modes: impl ExactSizeIterator<Item = rue_rir::RirParamMode>,
    ) -> rue_error::CompileResult<()>
    where
        A: IntoIterator,
        A::IntoIter: ExactSizeIterator,
        A::Item: std::ops::Deref<Target = rue_rir::RirCallArg>,
    {
        let args = args.into_iter();
        assert_eq!(args.len(), expected_modes.len());
        for (arg, expected_mode) in args.zip(expected_modes) {
            let arg = &*arg;
            use rue_rir::{RirArgMode, RirParamMode};
            match (expected_mode, arg.mode) {
                (RirParamMode::Inout, RirArgMode::Inout)
                | (RirParamMode::Borrow, RirArgMode::Borrow)
                | (RirParamMode::Normal, RirArgMode::Normal) => {}
                (RirParamMode::Inout, _) => {
                    return Err(rue_error::CompileError::new(
                        rue_error::ErrorKind::InoutKeywordMissing,
                        self.rir.get(arg.value).span,
                    ));
                }
                (RirParamMode::Borrow, _) => {
                    return Err(rue_error::CompileError::new(
                        rue_error::ErrorKind::BorrowKeywordMissing,
                        self.rir.get(arg.value).span,
                    ));
                }
                (RirParamMode::Normal, actual) => {
                    let mode = match actual {
                        RirArgMode::Inout => "inout",
                        RirArgMode::Borrow => "borrow",
                        RirArgMode::Normal => unreachable!(),
                    };
                    return Err(rue_error::CompileError::new(
                        rue_error::ErrorKind::UnexpectedCallArgumentMode { mode },
                        self.rir.get(arg.value).span,
                    )
                    .with_help(format!(
                        "remove the `{mode}` keyword; this argument is passed without an explicit mode"
                    )));
                }
            }
        }
        Ok(())
    }
}

impl std::ops::DerefMut for Sema<'_, MutableDeclarations> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.declarations
    }
}

pub(crate) type BodySema<'a> = Sema<'a, SourceDeclarations>;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NamespaceBoundarySnapshot {
    pub(crate) source_counts: [usize; 11],
    pub(crate) source_fingerprint: u64,
    pub(crate) generated_structs: usize,
    pub(crate) generated_enums: usize,
    pub(crate) anonymous_methods: usize,
}

#[cfg(test)]
impl BodySema<'_> {
    pub(crate) fn namespace_boundary_snapshot(&self) -> NamespaceBoundarySnapshot {
        use std::hash::{Hash, Hasher};

        fn fingerprint<T: Hash>(tag: u64, values: impl Iterator<Item = T>) -> u64 {
            values.fold(0, |fingerprint, value| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                tag.hash(&mut hasher);
                value.hash(&mut hasher);
                fingerprint ^ hasher.finish()
            })
        }

        let source_fingerprint = fingerprint(0, self.functions.keys())
            ^ fingerprint(1, self.functions_by_file_name.iter())
            ^ fingerprint(2, self.function_source_names.iter())
            ^ fingerprint(3, self.builtin_structs.iter())
            ^ fingerprint(4, self.structs_by_file_name.iter())
            ^ fingerprint(5, self.builtin_enums.iter())
            ^ fingerprint(6, self.enums_by_file_name.iter())
            ^ fingerprint(7, self.methods.keys())
            ^ fingerprint(8, self.named_method_declarations.iter())
            ^ fingerprint(9, self.value_consts().map(|(key, _)| key))
            ^ fingerprint(10, self.module_binding_consts().map(|(key, _)| key));

        NamespaceBoundarySnapshot {
            source_counts: [
                self.functions.len(),
                self.functions_by_file_name.len(),
                self.function_source_names.len(),
                self.builtin_structs.len(),
                self.structs_by_file_name.len(),
                self.builtin_enums.len(),
                self.enums_by_file_name.len(),
                self.methods.len(),
                self.named_method_declarations.len(),
                self.value_consts().count(),
                self.module_binding_consts().count(),
            ],
            source_fingerprint,
            generated_structs: self.generated_structs.len(),
            generated_enums: self.generated_enums.len(),
            anonymous_methods: self.anonymous_methods.len(),
        }
    }
}

impl<D: DeclarationPhase> Sema<'_, D> {
    pub(in crate::sema) fn aggregate_facts(
        &self,
    ) -> <Self as BodyAnalysisHost>::AggregateFacts<'_> {
        BodyAnalysisHost::aggregate_facts(self)
    }

    pub(crate) fn body_owner_token(
        &self,
        file: FileId,
        name: &str,
        owner: Option<&str>,
        kind: BodyOwnerKind,
    ) -> BodyOwnerToken {
        // Explicit synthetic fixtures and declaration-failure recovery do not
        // cross a durable identity boundary. Issuer zero is reserved for
        // those present API contracts; successful canonical analysis installs
        // compiler-issued tokens before bodies are analyzed.
        resolve_body_owner_token(
            &self.body_owner_tokens,
            file,
            name,
            owner,
            kind,
            self.synthetic_declaration_discovery,
        )
    }
}

impl BodySema<'_> {
    pub(in crate::sema) fn endpoint_facts(&self) -> <Self as BodyAnalysisHost>::EndpointFacts<'_> {
        BodyAnalysisHost::endpoint_facts(self)
    }

    pub(in crate::sema) fn call_facts(&self) -> <Self as BodyAnalysisHost>::CallFacts<'_> {
        BodyAnalysisHost::call_facts(self)
    }
}

impl<'a> Sema<'a, MutableDeclarations> {
    fn freeze_declarations(mut self) -> BodySema<'a> {
        self.rebuild_callable_method_index();
        self.map_declarations(|MutableDeclarations(namespace)| {
            SourceDeclarations(Arc::new(namespace))
        })
    }

    /// Create a phase-local semantic analyzer for synthetic/test use.
    ///
    /// This compatibility entry point derives explicit synthetic identities
    /// from the RIR and never participates in durable compiler joins. Supported
    /// compiler construction uses [`Self::new_for_target`] with canonical
    /// metadata.
    pub fn new_synthetic(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        preview_features: PreviewFeatures,
    ) -> Self {
        let mut sema = Self::new_for_target(
            rir,
            interner,
            preview_features,
            SemaMetadata::synthetic_for_rir(rir),
            Target::host()
                .expect("Rue cannot choose a default sema target on this unsupported host"),
        );
        sema.synthetic_declaration_discovery = true;
        let synthetic_const_candidates = sema
            .declaration_index
            .shell_declarations()
            .iter()
            .filter_map(|candidate| {
                matches!(
                    sema.rir.get(candidate.declaration).data,
                    rue_rir::InstData::ConstDecl { .. }
                )
                .then_some((candidate.source_order, candidate.declaration))
            })
            .collect::<Vec<_>>();
        sema.bound_const_candidates = Some(Arc::new(
            declaration_index::BoundConstCandidateIndex::new(sema.rir, synthetic_const_candidates),
        ));
        sema
    }

    /// Create a new semantic analyzer for an explicit compilation target.
    pub fn new_for_target(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        preview_features: PreviewFeatures,
        metadata: SemaMetadata,
        target: Target,
    ) -> Self {
        let SemaMetadata {
            root_file_id,
            physical_paths,
            logical_paths,
        } = metadata;
        let type_pool = TypeInternPool::new();
        type_pool.set_symbol_paths(logical_paths.clone());
        Self {
            declarations: MutableDeclarations(DeclarationNamespace::new()),
            rir,
            interner,
            declaration_index: Arc::new(declaration_index::RirDeclarationIndex::new(rir)),
            bound_const_candidates: None,
            synthetic_declaration_discovery: false,
            anonymous_methods: HashMap::new(),
            named_callable_methods_by_symbol: Arc::new(HashMap::new()),
            anonymous_callable_methods_by_symbol: HashMap::new(),
            generated_structs: HashMap::new(),
            generated_enums: HashMap::new(),
            anon_struct_identities: HashMap::new(),
            anon_enum_identities: HashMap::new(),
            anonymous_digest_owners: HashMap::new(),
            #[cfg(test)]
            forced_anonymous_digests: HashMap::new(),
            anonymous_struct_ids: HashSet::new(),
            anonymous_enum_ids: HashSet::new(),
            canonical_anonymous_types: HashMap::new(),
            active_anonymous_producer: None,
            body_analysis_work: BodyAnalysisWork::default(),
            analyzed_body_owners: Vec::new(),
            ordinary_body_exports: Vec::new(),
            specialized_body_exports: Vec::new(),
            reusable_ordinary_bodies: HashMap::new(),
            reusable_specialized_bodies: Vec::new(),
            stable_definition_tokens: Arc::default(),
            stable_definition_endpoints: Arc::default(),
            const_candidate_tokens: Arc::default(),
            const_candidate_endpoints: Arc::default(),
            stable_module_tokens: Arc::default(),
            stable_module_endpoints: Arc::default(),
            body_dependency_observer: None,
            body_owner_tokens: Arc::default(),
            body_named_dependencies: Vec::new(),
            body_lookup_collector: None,
            one_body_error_recovery: false,
            one_body_recovered_errors: Vec::new(),
            one_body_inference_failure_incomplete: false,
            one_body_initial_anonymous_identities: std::collections::BTreeSet::new(),
            one_body_requested_producer: None,
            body_callable_dependencies: Vec::new(),
            body_specialization_dependencies: Vec::new(),
            ordinary_free_function_dependencies: Vec::new(),
            specialized_free_function_origins: Vec::new(),
            specialized_free_function_dependencies: Vec::new(),
            named_method_dependencies: Vec::new(),
            non_generic_named_method_dependencies_complete: true,
            named_destructor_dependencies: Vec::new(),
            declaration_type_dependencies: Vec::new(),
            declaration_type_call_head_dependencies: Vec::new(),
            declaration_builtin_type_call_head_dependencies: Vec::new(),
            named_const_dependencies: Vec::new(),
            named_const_dependency_source: None,
            declaration_type_observer: None,
            const_resolution_in_progress: Vec::new(),
            declaration_binding_active: false,
            preview_features,
            target,
            builtin_arch_id: None,
            builtin_os_id: None,
            builtin_data_model_id: None,
            known: KnownSymbols::new(interner),
            type_pool,
            module_registry: Arc::new(crate::module_registry::ModuleRegistry::new()),
            canonical_imports: None,
            file_paths: Arc::new(physical_paths),
            symbol_paths: Arc::new(logical_paths),
            trusted_standard_library_files: Arc::default(),
            root_file_id: Some(root_file_id),
            param_arena: ParamArena::new(),
            anon_struct_method_sigs: HashMap::new(),
            anon_struct_captured_values: HashMap::new(),
            anon_struct_type_subst: HashMap::new(),
            destructor_spans: HashMap::new(),
            infectious_linear: HashMap::new(),
            deferred_ownership_gates: Vec::new(),
            comptime_type_call_depth: 0,
            fn_signatures_in_flight: HashSet::new(),
            ctor_type_displays: HashMap::new(),
            well_known_option_by_payload: HashMap::new(),
            well_known_option_identities: std::collections::BTreeSet::new(),
        }
    }

    /// Exact structural work used to index this semantic request's raw RIR
    /// declaration candidates.
    ///
    /// The counters contain no instruction handles and are safe to retain for
    /// profiling. The indexed declarations themselves remain private and
    /// meaningful only for this `Sema` and its exact RIR/interner epoch.
    #[inline]
    pub fn rir_declaration_index_work(&self) -> RirDeclarationIndexWork {
        self.declaration_index.work()
    }

    /// Test-support adapter for exercising AIR's retired source-owned
    /// declaration producer independently of the compiler query graph.
    #[doc(hidden)]
    pub fn analyze_all_for_test(self) -> MultiErrorResult<SemaOutput> {
        self.bind_declarations_for_test()?
            .analyze_all_bodies_for_test()
    }

    /// Bind and analyze every body after installing authoritative
    /// stable-identity endpoints keyed by each declaration's file.
    ///
    /// [`analyze_all_for_test`](Self::analyze_all_for_test) leaves
    /// `stable_definition_endpoints` empty, so no producer can be recognized as
    /// the trusted standard-library `Option`/`Result` and `?` on any operand is
    /// rejected. This entry point installs the endpoints, so `?` legality —
    /// which is exact producer-module identity (RUE-1112), not shape — can be
    /// exercised: a producer defined in a file whose symbol path is the trusted
    /// `\0rue-std/option.rue` / `\0rue-std/result.rue` identity is recognized as
    /// the std producer and binds under `?`. A synthetic harness grants trusted
    /// identity by placing the `Option`/`Result` producer in such a file (see
    /// `set_symbol_paths` / `set_trusted_standard_library_files`).
    ///
    /// Endpoints follow the production contract: definition tokens are issued in
    /// stable declaration-key order, and module resolution uses the empty module
    /// set (the flat synthetic namespace has no query-owned module registry).
    #[doc(hidden)]
    pub fn analyze_all_for_test_with_stable_endpoints(self) -> MultiErrorResult<SemaOutput> {
        let shells = self.predeclare_declaration_shells_for_test()?;
        let records = shells.declaration_shells().cloned().collect::<Vec<_>>();

        let mut definition_shells = records.clone();
        definition_shells.sort_by(|left, right| left.identity.cmp(&right.identity));
        let definitions = definition_shells
            .iter()
            .enumerate()
            .map(|(slot, shell)| crate::SemanticDefinitionEndpoint {
                token: crate::SemanticDefinitionToken::new(92, slot as u32),
                file: shell.declaration_span.file_id.index(),
                name: shell.identity.name.clone(),
                kind: shell.identity.kind,
                owner: shell.identity.owner.clone(),
            })
            .collect::<Vec<_>>();

        let owners = records
            .iter()
            .filter_map(|shell| {
                let (kind, name, owner_name) = match shell.identity.kind {
                    crate::StableDefinitionKind::Function => (
                        crate::BodyOwnerKind::FreeFunction,
                        shell.identity.name.to_string(),
                        None,
                    ),
                    crate::StableDefinitionKind::Method => (
                        crate::BodyOwnerKind::Method,
                        shell.identity.name.to_string(),
                        shell.identity.owner.as_deref().map(str::to_owned),
                    ),
                    crate::StableDefinitionKind::AssociatedFunction => (
                        crate::BodyOwnerKind::AssociatedFunction,
                        shell.identity.name.to_string(),
                        shell.identity.owner.as_deref().map(str::to_owned),
                    ),
                    crate::StableDefinitionKind::Destructor => {
                        let owner = shell.identity.owner.as_deref()?.to_owned();
                        (crate::BodyOwnerKind::Destructor, owner.clone(), Some(owner))
                    }
                    _ => return None,
                };
                Some(crate::BodyOwnerEndpoint {
                    token: crate::BodyOwnerToken::new(91, shell.source_order),
                    kind,
                    file: shell.declaration_span.file_id.index(),
                    name,
                    owner_name,
                })
            })
            .collect::<Vec<_>>();

        shells
            .install_stable_identity_endpoints(&definitions, &[])
            .expect("synthetic stable-identity endpoints match predeclared shells")
            .resolve_declarations_for_test()?
            .install_body_owner_tokens(&owners)
            .expect("synthetic body-owner tokens match resolved bodies")
            .analyze_all_bodies_for_test()
    }

    #[cfg(test)]
    pub fn analyze_all(self) -> MultiErrorResult<SemaOutput> {
        self.analyze_all_for_test()
    }

    /// Test-support adapter for the retired source-owned declaration producer.
    #[doc(hidden)]
    pub fn bind_declarations_for_test(self) -> MultiErrorResult<BoundSema<'a>> {
        self.predeclare_declaration_shells_for_test()?
            .resolve_declarations_for_test()
    }

    #[cfg(test)]
    pub fn bind_declarations(self) -> MultiErrorResult<BoundSema<'a>> {
        self.bind_declarations_for_test()
    }

    /// Test-support adapter that validates module-keyed declaration namespaces
    /// and predeclares nominal shells through the retired source-owned producer.
    #[doc(hidden)]
    pub fn predeclare_declaration_shells_for_test(
        mut self,
    ) -> MultiErrorResult<DeclarationShells<'a>> {
        assert!(
            self.synthetic_declaration_discovery,
            "canonical compiler epochs must import query-owned declaration shells"
        );
        let mut binding_work = DeclarationBindingWork::from_inputs(
            self.rir.len(),
            self.declaration_index.work(),
            self.module_registry.len(),
        );
        // Phase 0: Inject built-in types (String, etc.) before user code.
        // Canonical merge has already validated function/type/main conflicts;
        // value constants are checked later once module bindings are known.
        self.inject_builtin_types();
        binding_work.namespace_setup_invocations += 1;

        // Phase 1: Register nominal type shells.
        self.register_type_names().map_err(CompileErrors::from)?;
        binding_work.nominal_type_predeclaration_invocations += 1;

        // Phase 2: Freeze stable-joinable identities and authoritative
        // current-revision syntax metadata before resolving semantic payloads.
        // This does not evaluate constants or resolve any signature.
        let (pending_payloads, pending_nominals) = self.predeclare_callable_value_shells();
        binding_work.callable_value_predeclaration_invocations += 1;
        binding_work.callable_value_shells_predeclared = pending_payloads.len();
        binding_work.indexed_declaration_records_visited =
            pending_payloads.len() + pending_nominals.len();
        Ok(DeclarationShells {
            sema: self,
            binding_work,
            pending_payloads,
            pending_nominals,
        })
    }

    /// Import query-owned, position-free declaration shells and attach them to
    /// this epoch's private RIR locators. The importer never rediscovers a
    /// missing shell and fails closed on every mismatch.
    pub fn predeclare_imported_declaration_shells(
        mut self,
        imported: &[SemanticDeclarationShell],
    ) -> MultiErrorResult<DeclarationShells<'a>> {
        let mut binding_work = DeclarationBindingWork::from_inputs(
            self.rir.len(),
            self.declaration_index.work(),
            self.module_registry.len(),
        );
        self.inject_builtin_types();
        binding_work.namespace_setup_invocations += 1;
        self.register_type_names().map_err(CompileErrors::from)?;
        binding_work.nominal_type_predeclaration_invocations += 1;
        let (pending_payloads, pending_nominals) = self
            .attach_imported_declaration_shells(imported)
            .map_err(CompileErrors::from)?;
        binding_work.callable_value_predeclaration_invocations += 1;
        binding_work.callable_value_shells_predeclared = pending_payloads.len();
        binding_work.indexed_declaration_records_visited =
            pending_payloads.len() + pending_nominals.len();
        Ok(DeclarationShells {
            sema: self,
            binding_work,
            pending_payloads,
            pending_nominals,
        })
    }
}

impl BodySema<'_> {
    /// Test-only oracle for the retired whole-program body driver.
    fn analyze_all_bodies_for_test(self) -> MultiErrorResult<SemaOutput> {
        analysis::analyze_all_function_bodies_for_test(self)
    }
}

#[cfg(test)]
mod consistency_tests;
#[cfg(test)]
mod tests;
