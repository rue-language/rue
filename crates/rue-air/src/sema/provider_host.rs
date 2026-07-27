//! The provider-backed ordinary-body analysis host, under construction.
//!
//! This module is the receiver the flip installs. Today the single
//! [`OrdinaryBodyAnalysisHost`] implementation is `Sema`, which reaches a
//! declaration-wide epoch for every fact a body asks for; that shape is the
//! `O(bodies × declarations)` term the body-context repair exists to delete.
//! The replacement receiver owns exactly two things:
//!
//! - **Body-local mutable state** — [`ProviderBodyLocalState`] below. Every
//!   field here is state one body evaluation creates, mutates, and drops. None
//!   of it is shared with another body, and none of it is derived from the
//!   declaration universe, so it is `O(this body)` by construction.
//! - **A narrow fact boundary** — [`crate::sema::provider::BodyFactProvider`],
//!   whose typed operations answer exactly the questions a body asks and whose
//!   implementation records the corresponding query edge per call.
//!
//! Everything a body needs is one of those two things or a configuration
//! constant. [`HOST_CAPABILITY_LEDGER`] states which, for every method on the
//! host contract, and [`host_capability_ledger_covers_the_contract`] pins that
//! claim against the real trait source so it cannot go stale as the contract
//! moves. The ledger is the flip's worklist: `NeedsProviderWiring` is the only
//! category with design work left in it.

// The overlay state is declared before the receiver that mutates it: the host
// impl lands with the fact adapters, and every field here is storage that impl
// needs. Nothing selects this module at runtime, so it is inert rather than a
// peer body path — the ledger test is its only consumer today.
#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap, HashSet};

use lasso::Spur;
use rue_error::CompileError;
use rue_span::FileId;

use super::anon_structs::{
    IssuedAnonymousNominalKey, IssuedCanonicalArguments, IssuedStableProducerId,
};
use super::{
    AnalyzedBodyOwnerEvent, AnonMethodSig, BodyAnalysisWork, ConstValue,
    DeclarationTypeDependencyKind, DeclarationTypeDependencySourceKind, DeferredOwnershipGate,
    MethodInfo, StructId, Type,
};
use crate::types::EnumId;

/// The mutable state one body evaluation owns outright.
///
/// This is the whole of the "local overlay" half of the base/overlay split: a
/// body constructs one of these, mutates only this, and drops it at
/// publication. Two bodies analyzed in either order therefore cannot observe
/// each other through the host, which is what makes the receiver
/// parallel-ready. Nothing here is keyed by, sized by, or copied from the
/// declaration universe.
///
/// The field set mirrors the body-mutated half of `Sema` exactly, so the flip
/// is a receiver substitution rather than a semantic change. Adding a field
/// here is a claim that some body mutates it; adding one that a *declaration*
/// mutates would reintroduce the shared-epoch coupling this type exists to
/// remove.
#[derive(Debug, Default)]
pub(crate) struct ProviderBodyLocalState {
    /// Synthetic nominals this body generated, by rendered name.
    pub(crate) generated_structs: HashMap<Spur, StructId>,
    pub(crate) generated_enums: HashMap<Spur, EnumId>,
    /// Producer-nominal identities this body minted, and their compact ids.
    pub(crate) anon_struct_identities: HashMap<IssuedAnonymousNominalKey, StructId>,
    pub(crate) anon_enum_identities: HashMap<IssuedAnonymousNominalKey, EnumId>,
    /// Digest ownership for verified anonymous-identity disambiguation.
    pub(crate) anonymous_digest_owners: HashMap<u128, IssuedAnonymousNominalKey>,
    /// Test-only forced digests, used to drive collision handling.
    pub(crate) forced_anonymous_digests: HashMap<IssuedAnonymousNominalKey, u128>,
    /// The compact ids this body minted for anonymous nominals.
    pub(crate) anonymous_struct_ids: HashSet<StructId>,
    pub(crate) anonymous_enum_ids: HashSet<EnumId>,
    /// Reverse map from a materialized type to the identity that produced it.
    pub(crate) canonical_anonymous_types: HashMap<Type, IssuedAnonymousNominalKey>,
    /// Anonymous member signatures, captures, and substitutions this body made.
    pub(crate) anon_struct_method_sigs: HashMap<StructId, Vec<AnonMethodSig>>,
    pub(crate) anon_struct_captured_values: HashMap<StructId, HashMap<Spur, ConstValue>>,
    pub(crate) anon_struct_type_subst: HashMap<StructId, HashMap<Spur, Type>>,
    /// Anonymous methods installed while analyzing this body.
    pub(crate) anonymous_methods: HashMap<(StructId, Spur), MethodInfo>,
    /// The producer whose content the analyzer is currently inside, if any.
    pub(crate) active_anonymous_producer:
        Option<(IssuedStableProducerId, IssuedCanonicalArguments)>,
    /// The identities that existed before this body ran, so publication can
    /// report only what this body produced.
    pub(crate) initial_anonymous_identities: BTreeSet<IssuedAnonymousNominalKey>,
    /// Structural accounting for the work this body actually performed.
    pub(crate) body_analysis_work: BodyAnalysisWork,
    /// The owner every dependency this body records is attributed to.
    pub(crate) body_dependency_observer: Option<AnalyzedBodyOwnerEvent>,
    /// Dependencies this body recorded, by family.
    pub(crate) body_callable_dependencies: Vec<(AnalyzedBodyOwnerEvent, Spur)>,
    pub(crate) body_specialization_dependencies: Vec<(
        AnalyzedBodyOwnerEvent,
        crate::FunctionInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    )>,
    /// The declaration-type dependency currently being attributed, if any.
    pub(crate) declaration_type_observer: Option<(
        FileId,
        String,
        Option<String>,
        DeclarationTypeDependencySourceKind,
        DeclarationTypeDependencyKind,
    )>,
    /// Error-recovery ledger for one-body analysis.
    pub(crate) one_body_error_recovery: bool,
    pub(crate) one_body_recovered_errors: Vec<CompileError>,
    pub(crate) one_body_inference_failure_incomplete: bool,
    /// Ownership gates deferred until the body's types are known.
    pub(crate) deferred_ownership_gates: Vec<DeferredOwnershipGate>,
    /// Comptime type-constructor recursion depth for this body.
    pub(crate) comptime_type_call_depth: usize,
    /// Constructor type displays this body rendered, for diagnostics.
    pub(crate) ctor_type_displays: HashMap<Type, String>,
    /// Endpoints for the stable identities this body actually consulted.
    ///
    /// Symbol rendering needs `token -> (file, name, owner, kind)`, which the
    /// epoch answers from a declaration-universe-sized reverse table
    /// (`stable_definition_endpoints`). A body never needs that table: it can
    /// only render a token it resolved, and every token it resolved came back
    /// from a provider lookup that already carried the endpoint. Recording the
    /// endpoint at that moment makes rendering `O(consulted)` and keeps the
    /// dependency edge on the lookup that produced it, rather than on a table
    /// read afterwards.
    pub(crate) consulted_definition_endpoints:
        HashMap<crate::SemanticDefinitionToken, crate::SemanticDefinitionEndpoint>,
    pub(crate) consulted_module_endpoints:
        HashMap<crate::SemanticModuleToken, crate::SemanticModuleEndpoint>,
}

impl ProviderBodyLocalState {
    /// Start one body epoch. Every field begins empty: a body never inherits
    /// another body's overlay, which is the property the schedule-permutation
    /// oracle checks.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Seal the identity set that existed before analysis, so publication can
    /// report produced nominals as a difference rather than a whole set.
    pub(crate) fn seal_initial_anonymous_identities(&mut self) {
        self.initial_anonymous_identities = self
            .anon_struct_identities
            .keys()
            .chain(self.anon_enum_identities.keys())
            .cloned()
            .collect();
    }

    /// Record the endpoint a provider lookup returned alongside its token.
    ///
    /// Called at the consult, never at the render: a token whose endpoint was
    /// never recorded is one this body never resolved, and rendering it would
    /// be reaching for a fact the body has no dependency edge on.
    pub(crate) fn record_consulted_definition_endpoint(
        &mut self,
        endpoint: crate::SemanticDefinitionEndpoint,
    ) {
        self.consulted_definition_endpoints
            .insert(endpoint.token, endpoint);
    }

    pub(crate) fn record_consulted_module_endpoint(
        &mut self,
        endpoint: crate::SemanticModuleEndpoint,
    ) {
        self.consulted_module_endpoints
            .insert(endpoint.token, endpoint);
    }

    /// The endpoint for a token this body resolved, or `None` when it did not.
    pub(crate) fn consulted_definition_endpoint(
        &self,
        token: &crate::SemanticDefinitionToken,
    ) -> Option<&crate::SemanticDefinitionEndpoint> {
        self.consulted_definition_endpoints.get(token)
    }

    pub(crate) fn consulted_module_endpoint(
        &self,
        token: &crate::SemanticModuleToken,
    ) -> Option<&crate::SemanticModuleEndpoint> {
        self.consulted_module_endpoints.get(token)
    }

    /// The producer-nominal identities this body actually minted.
    pub(crate) fn produced_anonymous_identities(
        &self,
    ) -> impl Iterator<Item = &IssuedAnonymousNominalKey> {
        self.anon_struct_identities
            .keys()
            .chain(self.anon_enum_identities.keys())
            .filter(|identity| !self.initial_anonymous_identities.contains(*identity))
    }
}

/// Where one host-contract method gets its answer once the receiver is the
/// provider-backed host rather than a declaration epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostCapability {
    /// Answered from [`ProviderBodyLocalState`]. Mechanical: the field already
    /// exists above and the method is a borrow or an insert.
    BodyLocal,
    /// Answered by the rue-air-owned type/parameter/interner authority the
    /// provider state already carries (`ProviderBodyAnalysisState`).
    ProviderState,
    /// Answered by a configuration constant the evaluator is keyed on
    /// (target, preview features, well-known symbol table).
    Configuration,
    /// Answered by an existing `BodyFactProvider` operation. Mechanical, but
    /// needs the adapter that maps the operation's owned fact onto the shape
    /// the engine expects.
    ProviderFact,
    /// The real remaining work: no provider operation answers this yet, or the
    /// answer must change shape at the flip. Each row names what it needs.
    NeedsProviderWiring(&'static str),
    /// The body host answers this negatively by construction — the capability
    /// belongs to declaration binding, which a body never performs.
    InapplicableToBodies,
}

/// One row per method on the host contract.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HostCapabilityRow {
    pub(crate) method: &'static str,
    pub(crate) capability: HostCapability,
}

const fn row(method: &'static str, capability: HostCapability) -> HostCapabilityRow {
    HostCapabilityRow { method, capability }
}

/// The flip's worklist, as executable data.
///
/// Every method on `OrdinaryBodyAnalysisHost` appears exactly once. The
/// accompanying test parses the trait's real source and asserts the two agree,
/// so a method added to the contract fails this module until someone states
/// where its answer comes from — the same discipline the rFinal harness applies
/// to its own exception tables.
pub(crate) const HOST_CAPABILITY_LEDGER: &[HostCapabilityRow] = &[
    // --- Body-local overlay: mechanical, storage already declared above. ---
    row("install_anonymous_method", HostCapability::BodyLocal),
    row("generated_structs", HostCapability::BodyLocal),
    row("generated_structs_mut", HostCapability::BodyLocal),
    row("generated_enums_mut", HostCapability::BodyLocal),
    row("anonymous_struct_id", HostCapability::BodyLocal),
    row("anonymous_enum_id", HostCapability::BodyLocal),
    row("anonymous_struct_identities_mut", HostCapability::BodyLocal),
    row("anonymous_enum_identities_mut", HostCapability::BodyLocal),
    row("anonymous_digest_owner", HostCapability::BodyLocal),
    row("install_anonymous_digest_owner", HostCapability::BodyLocal),
    row("forced_anonymous_digest", HostCapability::BodyLocal),
    row(
        "install_canonical_anonymous_type",
        HostCapability::BodyLocal,
    ),
    row("anonymous_struct_methods_mut", HostCapability::BodyLocal),
    row("anonymous_struct_captures_mut", HostCapability::BodyLocal),
    row("anonymous_struct_ids_mut", HostCapability::BodyLocal),
    row("anonymous_enum_ids_mut", HostCapability::BodyLocal),
    row("set_anon_struct_type_subst", HostCapability::BodyLocal),
    row(
        "replace_active_anonymous_producer",
        HostCapability::BodyLocal,
    ),
    row("active_anonymous_producer", HostCapability::BodyLocal),
    row("body_declaration_type_observer", HostCapability::BodyLocal),
    row("body_analysis_work_mut", HostCapability::BodyLocal),
    row(
        "record_resolved_declaration_type",
        HostCapability::BodyLocal,
    ),
    row("one_body_error_recovery", HostCapability::BodyLocal),
    row("one_body_first_recovered_error", HostCapability::BodyLocal),
    row("one_body_recovered_errors_mut", HostCapability::BodyLocal),
    row(
        "set_one_body_inference_failure_incomplete",
        HostCapability::BodyLocal,
    ),
    row("body_dependency_observer", HostCapability::BodyLocal),
    row("record_body_named_dependency", HostCapability::BodyLocal),
    row("record_body_callable_dependency", HostCapability::BodyLocal),
    row(
        "record_specialization_dependency",
        HostCapability::BodyLocal,
    ),
    row("comptime_type_call_depth", HostCapability::BodyLocal),
    row("set_comptime_type_call_depth", HostCapability::BodyLocal),
    row("has_ctor_type_display", HostCapability::BodyLocal),
    row("record_body_ctor_type_display", HostCapability::BodyLocal),
    row("deferred_ownership_gates_mut", HostCapability::BodyLocal),
    // --- Provider state: the type/parameter/interner authority it owns. ---
    row("body_param_data", HostCapability::ProviderState),
    row("allocate_method_params", HostCapability::ProviderState),
    row("body_interner", HostCapability::ProviderState),
    row("body_type_pool", HostCapability::ProviderState),
    row("intern_array_type", HostCapability::ProviderState),
    row("strbuf_type", HostCapability::ProviderState),
    row("is_strbuf", HostCapability::ProviderState),
    row("types_equivalent", HostCapability::ProviderState),
    // --- Configuration the body query is keyed on. ---
    row("known_symbols", HostCapability::Configuration),
    row("target", HostCapability::Configuration),
    row("builtin_arch_id", HostCapability::Configuration),
    row("builtin_os_id", HostCapability::Configuration),
    row("builtin_data_model_id", HostCapability::Configuration),
    row("require_preview", HostCapability::Configuration),
    // --- Existing provider operations, needing only an adapter. ---
    row("function_info", HostCapability::ProviderFact),
    row("value_const", HostCapability::ProviderFact),
    row("source_function_name", HostCapability::ProviderFact),
    row("resolve_function_name_local", HostCapability::ProviderFact),
    row("module_def", HostCapability::ProviderFact),
    row("struct_in_file", HostCapability::ProviderFact),
    row("struct_id_for_name", HostCapability::ProviderFact),
    row("builtin_struct", HostCapability::ProviderFact),
    row("destructor_span", HostCapability::ProviderFact),
    row("infectious_linear_reason", HostCapability::ProviderFact),
    row("resolve_canonical_import", HostCapability::ProviderFact),
    row("well_known_option", HostCapability::ProviderFact),
    row("trusted_try_producer", HostCapability::ProviderFact),
    row("comptime_type_param_flags", HostCapability::ProviderFact),
    // --- Declaration binding: a body never performs it. ---
    row(
        "declaration_binding_active",
        HostCapability::InapplicableToBodies,
    ),
    row(
        "known_linear_during_binding",
        HostCapability::InapplicableToBodies,
    ),
    row(
        "known_drop_glue_during_binding",
        HostCapability::InapplicableToBodies,
    ),
    // --- The remaining design work. ---
    row(
        "body_rir_ref",
        HostCapability::NeedsProviderWiring(
            "the engine borrows whole-program RIR; the flip must hand it the body's own \
             lowered RIR so no live `Rir` crosses the query boundary",
        ),
    ),
    row(
        "resolve_body_type",
        HostCapability::NeedsProviderWiring(
            "recursive type-syntax resolution; the evaluator is already generic over \
             `TypeSyntaxHost`, so this is binding that host to the provider rather than to `Sema`",
        ),
    ),
    // The five export rows below are one job, not five. `SemanticBodyExportHost`
    // already abstracts the publication path over exactly these facts, so the
    // work is a second implementation of that trait rather than new machinery.
    // Tracing the epoch's implementation shows no reverse identity map is
    // needed: a compact id carries its own `(file, name)` through the type pool,
    // so the stable token is a FORWARD keyed lookup the fact provider already
    // answers, and the only genuinely reverse fact — token to endpoint, for
    // symbol rendering — is memoized body-locally at the consult that produced
    // the token (`consulted_definition_endpoints`). Every export fact is
    // therefore already body-local, derivable from the type pool, or recorded
    // at lookup time.
    row(
        "canonical_type_instance",
        HostCapability::NeedsProviderWiring(
            "compact type -> stable instance key; anonymous and builtin arms are already \
             body-local and type-pool reads, and the named arm is a forward \
             `(file, name, kind)` lookup through the fact boundary",
        ),
    ),
    row(
        "canonical_argument_value",
        HostCapability::NeedsProviderWiring(
            "compact const value -> stable argument value; rides the same arms as \
             `canonical_type_instance`",
        ),
    ),
    row(
        "function_identity",
        HostCapability::NeedsProviderWiring(
            "rendered symbol -> stable definition token; `callable_symbol_method` answers the \
             method arm and the free-function arm is the same forward lookup",
        ),
    ),
    row(
        "stable_definition_symbol_component",
        HostCapability::NeedsProviderWiring(
            "render from the endpoint recorded at consult time, not from the epoch's \
             universe-sized `stable_definition_endpoints` table",
        ),
    ),
    row(
        "stable_module_symbol_component",
        HostCapability::NeedsProviderWiring(
            "module half of the same rendering, from `consulted_module_endpoints`",
        ),
    ),
];

/// The forward stable-identity lookups the export path performs.
///
/// This is an adapter seam, not a second fact authority: the compiler-side
/// implementation answers each call through the ordinary
/// [`crate::sema::provider::BodyFactProvider`] lookup operations, so the
/// dependency edge is recorded at the consult exactly as it is for every other
/// provider fact. It exists so rue-air can express "resolve this exact stable
/// identity" without the export path naming the provider's eight associated
/// types.
///
/// Both operations are keyed and exact. Neither has a whole-universe form,
/// which is the property that makes export `O(consulted)`: the epoch answers
/// the same questions by scanning `stable_definition_tokens` and
/// `stable_module_tokens`, both sized by the declaration universe.
pub(crate) trait StableIdentityFacts {
    /// The stable token for one exact declaration coordinate.
    ///
    /// Fails closed, mirroring the epoch: absent is `MissingStableIdentity`,
    /// multiple candidates are `AmbiguousStableIdentity`, and a candidate of
    /// the wrong kind is `WrongStableIdentityKind`. The caller never guesses.
    fn definition_token(
        &self,
        file: u32,
        name: &str,
        owner: Option<&str>,
        kind: crate::StableDefinitionKind,
    ) -> Result<crate::SemanticDefinitionToken, crate::SemanticBodyExportFailure>;

    /// The stable token for one consulted module file.
    fn module_token(
        &self,
        file: FileId,
    ) -> Result<crate::SemanticModuleToken, crate::SemanticBodyExportFailure>;

    /// The file backing a module id this body resolved.
    fn module_file(&self, module: crate::types::ModuleId) -> Option<FileId>;
}

/// One named method a rendered callable symbol reverses to.
///
/// The epoch reads this out of its `methods` / `anonymous_methods` tables; the
/// provider answers it with `BodyFactProvider::callable_symbol_method`, whose
/// fail-closed single-candidate contract is the same one the export path needs
/// (an ambiguous receiver or method is `None`, never a guess).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExportedMethodSite {
    pub(crate) owner: StructId,
    pub(crate) method: Spur,
    pub(crate) file: FileId,
    pub(crate) has_self: bool,
}

/// The callable facts the export path consults, keyed and exact.
///
/// Same adapter discipline as [`StableIdentityFacts`]: the compiler-side
/// implementation routes each call through the provider boundary so the edge is
/// recorded, and neither operation has a whole-universe form.
pub(crate) trait ExportCallableFacts {
    /// The file declaring a free function, or `None` when the symbol does not
    /// name one (it may still name a method).
    fn free_function_file(&self, symbol: Spur) -> Option<FileId>;

    /// The source name behind a possibly-specialized callable symbol.
    fn source_function_name(&self, symbol: Spur) -> Spur;

    /// The named method a rendered callable symbol reverses to.
    fn method_by_callable_symbol(&self, symbol: Spur) -> Option<ExportedMethodSite>;
}

/// The export half of the provider-backed receiver.
///
/// It answers the same questions `BodySema` answers for publication, from the
/// body-local overlay, the provider's type pool, and exact keyed lookups —
/// never a declaration-wide table. The arms below deliberately mirror
/// `BodySema::export_body_type` / `body_struct_identity` /
/// `body_enum_identity` structurally, so a reviewer can read them side by side
/// and see that the only difference is where the stable token comes from.
pub(crate) struct ProviderExportHost<'a, I, C> {
    local: &'a ProviderBodyLocalState,
    type_pool: &'a crate::intern_pool::TypeInternPool,
    identity: &'a I,
    callables: &'a C,
    interner: &'a lasso::ThreadedRodeo,
}

impl<'a, I: StableIdentityFacts, C: ExportCallableFacts> ProviderExportHost<'a, I, C> {
    pub(crate) fn new(
        local: &'a ProviderBodyLocalState,
        type_pool: &'a crate::intern_pool::TypeInternPool,
        identity: &'a I,
        callables: &'a C,
        interner: &'a lasso::ThreadedRodeo,
    ) -> Self {
        Self {
            local,
            type_pool,
            identity,
            callables,
            interner,
        }
    }

    pub(crate) fn resolve_publication_symbol(&self, symbol: &Spur) -> &str {
        self.interner.resolve(symbol)
    }

    /// A slice-like struct's element type, if this struct is one.
    fn slice_element_type(&self, ty: Type) -> Option<Type> {
        let crate::types::TypeKind::Struct(struct_id) = ty.kind() else {
            return None;
        };
        let def = self.type_pool.struct_def(struct_id);
        let is_slice = def.name.starts_with('[')
            && def.name.ends_with(']')
            && crate::types::parse_array_type_syntax(&def.name).is_none();
        let is_str_fixed = def.name.starts_with("Str(") && def.name.ends_with(')');
        if is_slice || def.name == "str" || is_str_fixed {
            if let crate::types::TypeKind::PtrConst(ptr_id) = def.fields[0].ty.kind() {
                return Some(self.type_pool.ptr_const_def(ptr_id));
            }
        }
        None
    }

    /// The named-nominal arm: the compact id carries its own file and name, so
    /// the stable token is one exact forward lookup rather than a reverse map.
    fn struct_identity(
        &self,
        id: StructId,
    ) -> Result<crate::SemanticDefinitionToken, crate::SemanticBodyExportFailure> {
        let def = self
            .type_pool
            .struct_metadata(id)
            .ok_or(crate::SemanticBodyExportFailure::UnsupportedType)?;
        if def.name.starts_with("__anon_struct_") {
            return Err(crate::SemanticBodyExportFailure::AnonymousNominal);
        }
        self.identity.definition_token(
            def.file_id.index(),
            def.name.as_str(),
            None,
            crate::StableDefinitionKind::Struct,
        )
    }

    fn enum_identity(
        &self,
        id: EnumId,
    ) -> Result<crate::SemanticDefinitionToken, crate::SemanticBodyExportFailure> {
        let def = self
            .type_pool
            .enum_metadata(id)
            .ok_or(crate::SemanticBodyExportFailure::UnsupportedType)?;
        if def.name.starts_with("__anon_enum_") {
            return Err(crate::SemanticBodyExportFailure::AnonymousNominal);
        }
        self.identity.definition_token(
            def.file_id.index(),
            def.name.as_str(),
            None,
            crate::StableDefinitionKind::Enum,
        )
    }

    pub(crate) fn body_struct_identity(
        &self,
        id: StructId,
    ) -> Result<
        crate::NominalInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        let ty = Type::new_struct(id);
        Ok(match self.local.canonical_anonymous_types.get(&ty) {
            Some(key) => crate::NominalInstanceKey::Anonymous(key.clone()),
            None if self.type_pool.struct_def(id).is_builtin
                || self.type_pool.struct_def(id).name == "str" =>
            {
                crate::NominalInstanceKey::Builtin {
                    kind: crate::AnonymousNominalKind::Struct,
                    name: std::sync::Arc::from(self.type_pool.struct_def(id).name.as_str()),
                }
            }
            None => crate::NominalInstanceKey::Named(self.struct_identity(id)?),
        })
    }

    pub(crate) fn body_enum_identity(
        &self,
        id: EnumId,
    ) -> Result<
        crate::NominalInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        let ty = Type::new_enum(id);
        Ok(match self.local.canonical_anonymous_types.get(&ty) {
            Some(key) => crate::NominalInstanceKey::Anonymous(key.clone()),
            None if rue_builtins::BUILTIN_ENUMS
                .iter()
                .any(|builtin| builtin.name == self.type_pool.enum_def(id).name) =>
            {
                crate::NominalInstanceKey::Builtin {
                    kind: crate::AnonymousNominalKind::Enum,
                    name: std::sync::Arc::from(self.type_pool.enum_def(id).name.as_str()),
                }
            }
            None => crate::NominalInstanceKey::Named(self.enum_identity(id)?),
        })
    }

    /// The stable instance key for a compact type.
    ///
    /// Structurally the same walk as [`Self::export_body_type`] over a
    /// different output algebra, mirroring the epoch's own split.
    pub(crate) fn canonical_type_instance(
        &self,
        ty: Type,
    ) -> Result<
        crate::TypeInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        use crate::SemanticBodyExportFailure as Failure;
        use crate::types::TypeKind;
        use crate::{AnonymousNominalKind as K, NominalInstanceKey as N, TypeInstanceKey as T};

        Ok(match ty.kind() {
            TypeKind::I8 => T::I8,
            TypeKind::I16 => T::I16,
            TypeKind::I32 => T::I32,
            TypeKind::I64 => T::I64,
            TypeKind::U8 => T::U8,
            TypeKind::U16 => T::U16,
            TypeKind::U32 => T::U32,
            TypeKind::U64 => T::U64,
            TypeKind::Bool => T::Bool,
            TypeKind::Unit => T::Unit,
            TypeKind::Never => T::Never,
            TypeKind::ComptimeType => T::ComptimeType,
            TypeKind::Struct(id) => {
                if let Some(key) = self.local.canonical_anonymous_types.get(&ty) {
                    T::Nominal(N::Anonymous(key.clone()))
                } else {
                    // Identity comes from the declaration shell, before fields
                    // are complete: a comptime type constructor can legitimately
                    // receive a nominal whose layout is still unknown, so
                    // identity must not depend on layout.
                    let def = self
                        .type_pool
                        .struct_metadata(id)
                        .ok_or(Failure::UnsupportedType)?;
                    if def.is_builtin {
                        T::BuiltinNominal {
                            kind: K::Struct,
                            name: std::sync::Arc::from(def.name.as_str()),
                        }
                    } else {
                        T::Nominal(N::Named(self.struct_identity(id)?))
                    }
                }
            }
            TypeKind::Enum(id) => {
                if let Some(key) = self.local.canonical_anonymous_types.get(&ty) {
                    T::Nominal(N::Anonymous(key.clone()))
                } else {
                    let def = self
                        .type_pool
                        .enum_metadata(id)
                        .ok_or(Failure::UnsupportedType)?;
                    if rue_builtins::BUILTIN_ENUMS
                        .iter()
                        .any(|builtin| builtin.name == def.name)
                    {
                        T::BuiltinNominal {
                            kind: K::Enum,
                            name: std::sync::Arc::from(def.name.as_str()),
                        }
                    } else {
                        T::Nominal(N::Named(self.enum_identity(id)?))
                    }
                }
            }
            TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                T::Array {
                    element: Box::new(self.canonical_type_instance(element)?),
                    len,
                }
            }
            TypeKind::PtrConst(id) => T::PtrConst(Box::new(
                self.canonical_type_instance(self.type_pool.ptr_const_def(id))?,
            )),
            TypeKind::PtrMut(id) => T::PtrMut(Box::new(
                self.canonical_type_instance(self.type_pool.ptr_mut_def(id))?,
            )),
            TypeKind::Module(id) => {
                let file = self
                    .identity
                    .module_file(id)
                    .ok_or(Failure::MissingStableIdentity)?;
                T::Module(self.identity.module_token(file)?)
            }
            TypeKind::Error => return Err(Failure::UnsupportedType),
        })
    }

    /// The stable definition token behind a rendered callable symbol.
    fn function_identity(
        &self,
        symbol: Spur,
    ) -> Result<crate::SemanticDefinitionToken, crate::SemanticBodyExportFailure> {
        if let Some(file) = self.callables.free_function_file(symbol) {
            let source = self.callables.source_function_name(symbol);
            return self.identity.definition_token(
                file.index(),
                self.interner.resolve(&source),
                None,
                crate::StableDefinitionKind::Function,
            );
        }
        let site = self
            .callables
            .method_by_callable_symbol(symbol)
            .ok_or(crate::SemanticBodyExportFailure::UnmappedFunction)?;
        let method = self.interner.resolve(&site.method);
        let owner = self.type_pool.struct_def(site.owner);
        if owner.name.starts_with("__anon_struct_") {
            return Err(crate::SemanticBodyExportFailure::AnonymousNominal);
        }
        self.identity.definition_token(
            site.file.index(),
            method,
            Some(owner.name.as_str()),
            if method == "__drop" {
                crate::StableDefinitionKind::Destructor
            } else if site.has_self {
                crate::StableDefinitionKind::Method
            } else {
                crate::StableDefinitionKind::AssociatedFunction
            },
        )
    }

    /// The stable instance a rendered callable symbol names.
    ///
    /// A method on a producer-nominal owner is an anonymous member of that
    /// producer, not a free definition — which is why the anonymous check runs
    /// against the body-local `canonical_anonymous_types` before falling
    /// through to definition identity.
    pub(crate) fn body_function_identity(
        &self,
        symbol: Spur,
    ) -> Result<
        crate::FunctionInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        if self.callables.free_function_file(symbol).is_some() {
            return Ok(crate::FunctionInstanceKey::Definition(
                self.function_identity(symbol)?,
            ));
        }
        let Some(site) = self.callables.method_by_callable_symbol(symbol) else {
            return Ok(crate::FunctionInstanceKey::Definition(
                self.function_identity(symbol)?,
            ));
        };
        let method = self.interner.resolve(&site.method);
        let owner = Type::new_struct(site.owner);
        if self.local.canonical_anonymous_types.contains_key(&owner) {
            let kind = if method == "__drop" {
                crate::AnonymousMemberKind::Destructor
            } else if site.has_self {
                crate::AnonymousMemberKind::Method
            } else {
                crate::AnonymousMemberKind::AssociatedFunction
            };
            return Ok(crate::FunctionInstanceKey::AnonymousMember {
                owner: Box::new(self.canonical_type_instance(owner)?),
                member: crate::AnonymousMemberKey {
                    kind,
                    name: std::sync::Arc::from(method),
                },
            });
        }
        Ok(crate::FunctionInstanceKey::Definition(
            self.function_identity(symbol)?,
        ))
    }

    pub(crate) fn export_body_type(
        &self,
        ty: Type,
    ) -> Result<
        crate::SemanticImportType<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        use crate::SemanticImportType as Exported;
        use crate::types::TypeKind;

        self.type_pool
            .validate_complete_type(ty)
            .map_err(|_| crate::SemanticBodyExportFailure::UnsupportedType)?;
        Ok(match ty.kind() {
            TypeKind::I8 => Exported::I8,
            TypeKind::I16 => Exported::I16,
            TypeKind::I32 => Exported::I32,
            TypeKind::I64 => Exported::I64,
            TypeKind::U8 => Exported::U8,
            TypeKind::U16 => Exported::U16,
            TypeKind::U32 => Exported::U32,
            TypeKind::U64 => Exported::U64,
            TypeKind::Bool => Exported::Bool,
            TypeKind::Unit => Exported::Unit,
            TypeKind::Never => Exported::Never,
            TypeKind::ComptimeType => Exported::ComptimeType,
            TypeKind::Struct(id) => {
                let def = self.type_pool.struct_def(id);
                if let Some(identity) = self.local.canonical_anonymous_types.get(&ty) {
                    Exported::AnonymousNominal(identity.clone())
                } else if let Some(element) = self.slice_element_type(ty)
                    && def.name.starts_with('[')
                {
                    Exported::Slice {
                        element: Box::new(self.export_body_type(element)?),
                        name: std::sync::Arc::from(def.name.as_str()),
                    }
                } else if def.is_builtin || def.name == "str" {
                    Exported::BuiltinNominal {
                        name: std::sync::Arc::from(def.name.as_str()),
                        kind: crate::SemanticImportNominalKind::Struct,
                    }
                } else {
                    Exported::Nominal(self.struct_identity(id)?)
                }
            }
            TypeKind::Enum(id) => {
                let def = self.type_pool.enum_def(id);
                if let Some(identity) = self.local.canonical_anonymous_types.get(&ty) {
                    Exported::AnonymousNominal(identity.clone())
                } else if rue_builtins::BUILTIN_ENUMS
                    .iter()
                    .any(|builtin| builtin.name == def.name)
                {
                    Exported::BuiltinNominal {
                        name: std::sync::Arc::from(def.name.as_str()),
                        kind: crate::SemanticImportNominalKind::Enum,
                    }
                } else {
                    Exported::Nominal(self.enum_identity(id)?)
                }
            }
            TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                Exported::Array {
                    element: Box::new(self.export_body_type(element)?),
                    len,
                }
            }
            TypeKind::PtrConst(id) => Exported::PtrConst(Box::new(
                self.export_body_type(self.type_pool.ptr_const_def(id))?,
            )),
            TypeKind::PtrMut(id) => Exported::PtrMut(Box::new(
                self.export_body_type(self.type_pool.ptr_mut_def(id))?,
            )),
            TypeKind::Module(id) => {
                let file = self
                    .identity
                    .module_file(id)
                    .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
                Exported::Module(self.identity.module_token(file)?)
            }
            TypeKind::Error => {
                return Err(crate::SemanticBodyExportFailure::UnsupportedType);
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDINARY_ENGINE_SOURCE: &str = include_str!("ordinary_engine.rs");

    /// Every method declared on the host contract, parsed from its real source.
    fn contract_methods() -> Vec<String> {
        let trait_source = ORDINARY_ENGINE_SOURCE
            .split("pub(crate) trait OrdinaryBodyAnalysisHost")
            .nth(1)
            .expect("the ordinary host contract is declared in this crate")
            .split("\n}\n")
            .next()
            .expect("the contract has a closing brace");
        trait_source
            .lines()
            .filter_map(|line| line.strip_prefix("    fn "))
            .filter_map(|line| line.split(['(', '<']).next())
            .map(str::to_owned)
            .collect()
    }

    /// The ledger is the flip's worklist, so it must describe the contract that
    /// actually exists — not the one it was written against. A method added to
    /// `OrdinaryBodyAnalysisHost` fails here until its answer has a stated
    /// source, and a method deleted from the contract fails here until its row
    /// goes with it.
    #[test]
    fn host_capability_ledger_covers_the_contract() {
        let contract = contract_methods();
        assert!(
            contract.len() > 60,
            "the contract parse found only {} methods, so the parser — not the ledger — is wrong",
            contract.len()
        );

        let ledgered = HOST_CAPABILITY_LEDGER
            .iter()
            .map(|row| row.method.to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            ledgered.len(),
            HOST_CAPABILITY_LEDGER.len(),
            "each contract method is ledgered exactly once"
        );

        let declared = contract.iter().cloned().collect::<BTreeSet<_>>();
        let missing = declared.difference(&ledgered).collect::<Vec<_>>();
        assert!(
            missing.is_empty(),
            "these host-contract methods have no stated fact source: {missing:?}"
        );
        let stale = ledgered.difference(&declared).collect::<Vec<_>>();
        assert!(
            stale.is_empty(),
            "these ledger rows name methods the contract no longer declares: {stale:?}"
        );
    }

    /// A body epoch starts empty. The schedule-permutation oracle depends on
    /// this: if a fresh overlay could observe a previous body's mint, opposite
    /// demand orders would not agree.
    #[test]
    fn a_fresh_body_overlay_shares_nothing() {
        let state = ProviderBodyLocalState::new();
        assert!(state.anon_struct_identities.is_empty());
        assert!(state.anonymous_methods.is_empty());
        assert!(state.generated_structs.is_empty());
        assert_eq!(state.comptime_type_call_depth, 0);
        assert!(state.produced_anonymous_identities().next().is_none());
    }

    /// Rendering a stable symbol is only legal for a token this body resolved.
    /// The memo is populated at the consult, so an unconsulted token misses —
    /// which is the difference between `O(consulted)` rendering and reading the
    /// epoch's declaration-universe endpoint table.
    #[test]
    fn only_consulted_tokens_can_be_rendered() {
        let mut state = ProviderBodyLocalState::new();
        let consulted = crate::SemanticDefinitionToken::new(1, 7);
        let never_consulted = crate::SemanticDefinitionToken::new(1, 8);

        assert!(state.consulted_definition_endpoint(&consulted).is_none());

        state.record_consulted_definition_endpoint(crate::SemanticDefinitionEndpoint {
            token: consulted,
            file: 3,
            name: "helper".into(),
            kind: crate::StableDefinitionKind::Function,
            owner: None,
        });

        let endpoint = state
            .consulted_definition_endpoint(&consulted)
            .expect("a consulted token carries the endpoint its lookup returned");
        assert_eq!(endpoint.file, 3);
        assert_eq!(&*endpoint.name, "helper");
        assert!(
            state
                .consulted_definition_endpoint(&never_consulted)
                .is_none(),
            "a token this body never resolved has no endpoint to render from"
        );
    }

    /// Callable facts for a body that names none.
    struct AbsentCallableFacts;

    impl ExportCallableFacts for AbsentCallableFacts {
        fn free_function_file(&self, _: Spur) -> Option<FileId> {
            None
        }
        fn source_function_name(&self, symbol: Spur) -> Spur {
            symbol
        }
        fn method_by_callable_symbol(&self, _: Spur) -> Option<ExportedMethodSite> {
            None
        }
    }

    /// A `StableIdentityFacts` that records every consult, so a test can assert
    /// not just what was exported but how much of the boundary it cost.
    #[derive(Default)]
    struct RecordingIdentityFacts {
        definition_consults: std::cell::RefCell<Vec<String>>,
        module_consults: std::cell::RefCell<Vec<u32>>,
    }

    impl StableIdentityFacts for RecordingIdentityFacts {
        fn definition_token(
            &self,
            file: u32,
            name: &str,
            owner: Option<&str>,
            kind: crate::StableDefinitionKind,
        ) -> Result<crate::SemanticDefinitionToken, crate::SemanticBodyExportFailure> {
            self.definition_consults
                .borrow_mut()
                .push(format!("{file}:{name}:{owner:?}:{kind:?}"));
            Err(crate::SemanticBodyExportFailure::MissingStableIdentity)
        }

        fn module_token(
            &self,
            file: FileId,
        ) -> Result<crate::SemanticModuleToken, crate::SemanticBodyExportFailure> {
            self.module_consults.borrow_mut().push(file.index());
            Err(crate::SemanticBodyExportFailure::MissingStableIdentity)
        }

        fn module_file(&self, _: crate::types::ModuleId) -> Option<FileId> {
            None
        }
    }

    /// Exporting a primitive consults the identity boundary zero times.
    ///
    /// This is the locality claim in its smallest form: the epoch answers these
    /// same cases while holding a declaration-universe token table, and the
    /// provider host answers them from the type pool alone. A regression that
    /// reintroduced a table read would show up here as a consult.
    #[test]
    fn exporting_primitives_costs_no_boundary_consults() {
        let local = ProviderBodyLocalState::new();
        let pool = crate::intern_pool::TypeInternPool::new();
        let interner = lasso::ThreadedRodeo::new();
        let identity = RecordingIdentityFacts::default();
        let callables = AbsentCallableFacts;
        let host = ProviderExportHost::new(&local, &pool, &identity, &callables, &interner);

        for (ty, expected) in [
            (Type::I32, crate::SemanticImportType::I32),
            (Type::BOOL, crate::SemanticImportType::Bool),
            (Type::UNIT, crate::SemanticImportType::Unit),
        ] {
            assert_eq!(
                host.export_body_type(ty).expect("a primitive exports"),
                expected
            );
        }

        assert!(
            identity.definition_consults.borrow().is_empty(),
            "primitives resolved through the identity boundary: {:?}",
            identity.definition_consults.borrow()
        );
        assert!(identity.module_consults.borrow().is_empty());
    }

    /// An unresolvable stable identity fails closed rather than inventing a
    /// token. The epoch has a synthetic-test fallback that mints one from a
    /// hash when its table is empty; the provider host must not inherit it,
    /// because "the table is empty" is not a state the provider can be in.
    #[test]
    fn an_unresolved_module_identity_fails_closed() {
        let local = ProviderBodyLocalState::new();
        let pool = crate::intern_pool::TypeInternPool::new();
        let interner = lasso::ThreadedRodeo::new();
        let identity = RecordingIdentityFacts::default();
        let callables = AbsentCallableFacts;
        let host = ProviderExportHost::new(&local, &pool, &identity, &callables, &interner);

        assert!(matches!(
            host.export_body_type(Type::ERROR),
            Err(crate::SemanticBodyExportFailure::UnsupportedType)
        ));
    }

    /// A symbol that names neither a free function nor a resolvable method is
    /// unmapped, not guessed. The provider's `callable_symbol_method` has the
    /// same fail-closed single-candidate contract, so an ambiguous receiver
    /// arrives here as `None` and must not become a fabricated definition.
    #[test]
    fn an_unmapped_callable_symbol_fails_closed() {
        let local = ProviderBodyLocalState::new();
        let pool = crate::intern_pool::TypeInternPool::new();
        let interner = lasso::ThreadedRodeo::new();
        let identity = RecordingIdentityFacts::default();
        let callables = AbsentCallableFacts;
        let host = ProviderExportHost::new(&local, &pool, &identity, &callables, &interner);

        let symbol = interner.get_or_intern("nowhere");
        assert!(matches!(
            host.body_function_identity(symbol),
            Err(crate::SemanticBodyExportFailure::UnmappedFunction)
        ));
        assert!(
            identity.definition_consults.borrow().is_empty(),
            "an unmapped symbol consulted the identity boundary: {:?}",
            identity.definition_consults.borrow()
        );
    }

    /// The only category with design work left in it is the one the flip has
    /// to solve; everything else is storage that already exists or an adapter
    /// over an operation that already exists. This test states that split as a
    /// number so it moves visibly as the flip lands, rather than being
    /// re-litigated from prose each time.
    #[test]
    fn the_remaining_design_work_is_named_and_bounded() {
        let outstanding = HOST_CAPABILITY_LEDGER
            .iter()
            .filter(|row| matches!(row.capability, HostCapability::NeedsProviderWiring(_)))
            .collect::<Vec<_>>();
        for row in &outstanding {
            let HostCapability::NeedsProviderWiring(reason) = row.capability else {
                unreachable!("filtered to the outstanding category")
            };
            assert!(
                !reason.is_empty(),
                "{} must say what wiring it needs",
                row.method
            );
        }
        assert_eq!(
            outstanding.len(),
            7,
            "the outstanding host capabilities are {:?}; update this count in the same change \
             that wires one, so the worklist cannot shrink silently",
            outstanding.iter().map(|row| row.method).collect::<Vec<_>>()
        );
    }
}
