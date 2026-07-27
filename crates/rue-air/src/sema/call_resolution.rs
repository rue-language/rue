//! Call / method / operator / module-member resolution for exact-one-body
//! analysis.
//!
//! Family 1C of the RUE-1091 analyzer rewire (slice r1b). The free-function,
//! method, operator-overload, and module-member reads that `analysis.rs`,
//! `calls.rs`, `ownership.rs`, and `builtin_ops.rs` perform while resolving a
//! call, method, or operator to its callee no longer touch the semantic epoch
//! tables directly. They flow through [`CallResolutionFacts`] — the
//! value/call-world analog of [`crate::SemanticTypeSyntaxProvider`] and a
//! sibling of `body_endpoint`'s [`super::body_endpoint::BodyEndpointProvider`]
//! (family 1A) — so the selection *logic* is provider-generic and a later slice
//! can supply the same facts from a body-fact provider + overlay instead of the
//! epoch `Sema`.
//!
//! [`EpochFacts`] is the generic production adapter: it delegates each point
//! query to a [`CallResolutionFactSource`] supplied by its host. `Sema` supplies
//! the current declaration-epoch source, preserving the existing reads while
//! keeping the adapter independent of an analyzer representation. Every
//! operation is `&self` and returns owned or `Copy` data, so a caller inside an
//! `&mut Sema` stack constructs a short-lived [`EpochFacts`] per resolution
//! without retaining a borrow across the surrounding mutations.
//!
//! Where the analyzer selects a winner across *several* candidate reads — the
//! reachability classifier's free-function-then-named-method order, and the
//! static call-reference resolver's const-alias-then-local order — that
//! selection lives in the provider-generic free functions below
//! ([`classify_static_call`], [`resolve_static_call_reference`]) rather than in
//! the impl, so both `EpochFacts` and a future body-fact impl replay the exact
//! same candidate order, short-circuits, and first-match-wins tie-breaks
//! (RUE-1091 risk R1). Winner-picking that a single epoch accessor already
//! performs internally (`method_info`'s anonymous-then-named fallback, the
//! callable-symbol single-candidate check) stays inside that point query,
//! matching the `body_endpoint` convention.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;

use lasso::Spur;
use rue_rir::{InstData, InstRef};
use rue_span::FileId;

use super::body_identity::{
    BodyRirView, DurableCallableSource, DurableNominalSource, FunctionIdentityHandle,
    MethodIdentityHandle, ProviderIdentityContext,
};
use super::info::{ConstInfo, FunctionInfo, MethodInfo};
use super::provider::BodyFactProvider;
use super::{DeclarationPhase, Sema};
use crate::types::{ModuleDef, ModuleId, StructId};

/// The exact call/method/operator-resolution fact boundary consumed by the
/// family-1C analyzer sites. Every operation answers one point query against
/// the declaration universe and returns owned/`Copy` data — no borrowed epoch
/// table or live `Sema` handle escapes.
pub(crate) trait CallResolutionFacts {
    /// The signature/binding info for an internal free-function symbol.
    /// Mirrors `Sema::function_info` (`functions.get`).
    fn function_info(&self, name: Spur) -> Option<FunctionInfo>;

    /// Whether an internal free-function symbol is declared. Mirrors
    /// `functions.contains_key`.
    fn function_contains(&self, name: Spur) -> bool;

    /// The source name a specialized/internal function name derives from.
    /// Mirrors `Sema::source_function_name` (identity when unmapped).
    fn source_function_name(&self, name: Spur) -> Spur;

    /// The internal free-function symbol a source name resolves to inside its
    /// own file. Mirrors `Sema::resolve_function_name_local`.
    fn resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur>;

    /// The value-constant info declared as `(file, name)`. Mirrors
    /// `Sema::resolve_const_info_in_file` (a file-scoped `value_const`).
    fn resolve_const_info_in_file(&self, name: Spur, file: FileId) -> Option<ConstInfo>;

    /// The value-constant info declared as `(file, name)`. Mirrors
    /// `value_const`; distinct from [`Self::resolve_const_info_in_file`] only in
    /// how the key is spelled at the call site.
    fn value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo>;

    /// The module-binding const declared as `(file, name)`. Mirrors
    /// `module_binding`.
    fn module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo>;

    /// The method/associated-function info for `(struct, name)`, preferring the
    /// anonymous table then the named table. Mirrors `Sema::method_info`.
    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo>;

    /// The *named* method/associated-function info for `(struct, name)`,
    /// consulting only the named table. Mirrors a direct `methods.get`, so it
    /// deliberately does not fall back to the anonymous table the way
    /// [`Self::method_info`] does.
    fn named_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo>;

    /// The unique named `(struct, method, info)` a callable symbol resolves to,
    /// or `None` when the symbol is absent or ambiguous. Mirrors
    /// `Sema::named_method_by_callable_symbol`.
    fn named_method_by_callable_symbol(&self, name: Spur) -> Option<(StructId, Spur, MethodInfo)>;

    /// The named-method RIR declaration for the durable-available
    /// `(owner_file, owner_type_name, method_name)` preimage. Mirrors
    /// `structs_by_file_name.get` followed by `named_method_declarations.get`.
    fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: Spur,
        method_name: Spur,
    ) -> Option<InstRef>;

    /// The module definition for a module id. Mirrors
    /// `module_registry.get_def`.
    fn module_def(&self, module_id: ModuleId) -> ModuleDef;
}

/// Raw immutable call-resolution reads supplied by a body-analysis host.
pub(super) trait CallResolutionFactSource {
    fn call_function_info(&self, name: Spur) -> Option<FunctionInfo>;
    fn call_function_contains(&self, name: Spur) -> bool;
    fn call_source_function_name(&self, name: Spur) -> Spur;
    fn call_resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur>;
    fn call_resolve_const_info_in_file(&self, name: Spur, file: FileId) -> Option<ConstInfo>;
    fn call_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo>;
    fn call_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo>;
    fn call_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo>;
    fn call_named_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo>;
    fn call_named_method_by_callable_symbol(
        &self,
        name: Spur,
    ) -> Option<(StructId, Spur, MethodInfo)>;
    fn call_named_method_declaration(
        &self,
        file: FileId,
        ty: Spur,
        method: Spur,
    ) -> Option<InstRef>;
    fn call_module_def(&self, module: ModuleId) -> ModuleDef;
}

/// Direct epoch reads for the current host. The generic adapter below owns the
/// read-only abstraction; this impl is only the production source of facts.
impl<D: DeclarationPhase> CallResolutionFactSource for Sema<'_, D> {
    fn call_function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.function_info(name).copied()
    }

    fn call_function_contains(&self, name: Spur) -> bool {
        self.functions.contains_key(&name)
    }

    fn call_source_function_name(&self, name: Spur) -> Spur {
        self.source_function_name(name)
    }

    fn call_resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur> {
        self.record_body_module_item_lookup(file, name);
        self.resolve_function_name_local(name, file)
    }

    fn call_resolve_const_info_in_file(&self, name: Spur, file: FileId) -> Option<ConstInfo> {
        self.record_body_module_item_lookup(file, name);
        self.resolve_const_info_in_file(name, file).cloned()
    }

    fn call_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.record_body_module_item_lookup(file, name);
        self.value_const(&(file, name)).cloned()
    }

    fn call_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.record_body_module_item_lookup(file, name);
        self.module_binding(&(file, name)).cloned()
    }

    fn call_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.record_body_member_lookup(struct_id, name);
        self.method_info((struct_id, name)).copied()
    }

    fn call_named_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.record_body_member_lookup(struct_id, name);
        self.methods.get(&(struct_id, name)).copied()
    }

    fn call_named_method_by_callable_symbol(
        &self,
        name: Spur,
    ) -> Option<(StructId, Spur, MethodInfo)> {
        self.named_method_by_callable_symbol(name)
            .map(|(struct_id, method, info)| (struct_id, method, *info))
    }

    fn call_named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: Spur,
        method_name: Spur,
    ) -> Option<InstRef> {
        self.record_body_module_item_lookup(owner_file, owner_type_name);
        let struct_id = self
            .structs_by_file_name
            .get(&(owner_file, owner_type_name))?;
        self.record_body_member_lookup(*struct_id, method_name);
        self.named_method_declarations
            .get(&(*struct_id, method_name))
            .copied()
    }

    fn call_module_def(&self, module_id: ModuleId) -> ModuleDef {
        self.module_registry.get_def(module_id)
    }
}

/// Read-only call-resolution adapter used by the canonical body engine.
pub(crate) struct EpochFacts<'host, H: super::fact_mode::BodyAnalysisReadHost> {
    host: &'host H,
}

impl<'host, H: super::fact_mode::BodyAnalysisReadHost> EpochFacts<'host, H> {
    pub(in crate::sema) fn new(host: &'host H) -> Self {
        Self { host }
    }
}

impl<H: super::fact_mode::BodyAnalysisReadHost> CallResolutionFacts for EpochFacts<'_, H> {
    fn function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.host.call_function_info(name)
    }
    fn function_contains(&self, name: Spur) -> bool {
        self.host.call_function_contains(name)
    }
    fn source_function_name(&self, name: Spur) -> Spur {
        self.host.call_source_function_name(name)
    }
    fn resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur> {
        self.host.call_resolve_function_name_local(name, file)
    }
    fn resolve_const_info_in_file(&self, name: Spur, file: FileId) -> Option<ConstInfo> {
        self.host.call_resolve_const_info_in_file(name, file)
    }
    fn value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.host.call_value_const(file, name)
    }
    fn module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.host.call_module_binding(file, name)
    }
    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.host.call_method_info(struct_id, name)
    }
    fn named_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.host.call_named_method_info(struct_id, name)
    }
    fn named_method_by_callable_symbol(&self, name: Spur) -> Option<(StructId, Spur, MethodInfo)> {
        self.host.call_named_method_by_callable_symbol(name)
    }
    fn named_method_declaration(&self, file: FileId, ty: Spur, method: Spur) -> Option<InstRef> {
        self.host.call_named_method_declaration(file, ty, method)
    }
    fn module_def(&self, module_id: ModuleId) -> ModuleDef {
        self.host.call_module_def(module_id)
    }
}

/// The reachability classification of a statically discovered call name.
pub(in crate::sema) enum StaticCallReference {
    /// The name is a free function.
    Free(Spur),
    /// The name is the callable symbol of the unique `(struct, method)`.
    Method(StructId, Spur),
}

/// Classify a discovered call name as a free function or a unique named method,
/// preferring the free function. The provider-generic form of the inline
/// classifier in `imported_body_references`: a free-function candidate
/// wins over a named-method candidate (first-match-wins), and the named-method
/// read is skipped entirely when the free-function membership check succeeds.
pub(in crate::sema) fn classify_static_call<P: CallResolutionFacts>(
    facts: &P,
    name: Spur,
) -> Option<StaticCallReference> {
    if facts.function_contains(name) {
        return Some(StaticCallReference::Free(name));
    }
    if let Some((struct_id, method, _)) = facts.named_method_by_callable_symbol(name) {
        return Some(StaticCallReference::Method(struct_id, method));
    }
    None
}

/// Resolve a statically discovered call name to the free-function symbol it
/// references for reachability. The provider-generic form of the inline
/// resolution in `collect_static_function_references`, replaying `analyze_call`'s
/// alias-then-local selection: a function-valued constant alias is consulted
/// first (and, when it names a declared function, wins directly); otherwise the
/// file-local function name is resolved. The alias read is skipped when the name
/// binds no function-valued const, and the local read is skipped when the alias
/// already resolved to a declared function.
pub(in crate::sema) fn resolve_static_call_reference<P: CallResolutionFacts>(
    facts: &P,
    name: Spur,
    file: FileId,
) -> Option<Spur> {
    let mut target = name;
    let mut resolved_alias = false;
    if let Some(const_info) = facts.resolve_const_info_in_file(name, file)
        && let Some(callee) = const_info.value.as_function()
    {
        target = callee;
        resolved_alias = true;
    }

    if resolved_alias && facts.function_contains(target) {
        Some(target)
    } else {
        facts.resolve_function_name_local(target, file)
    }
}

// ---------------------------------------------------------------------------
// `ProviderCallFacts` — the call-resolution ProviderFacts (RUE-1091 slice r4b-1).
//
// The first provider-driven realization of the family-1C call/method/operator
// facts: where [`EpochFacts`] answers each op from the semantic epoch's
// `Sema` tables, this driver answers them from the exact body-fact provider
// boundary ([`BodyFactProvider`], realized in production by rue-compiler's
// `CompilerBodyFactProvider`) plus the body-scoped identity pool (slices
// 2a/2b/2c). It is the value/call analog of r2's `ProviderTypeFacts`: the fact
// SOURCE differs, the assembled answers (`FunctionInfo`/`MethodInfo`/…) are the
// already-published identity types a differential compares against the epoch.
//
// RUE-1091 flip-era surface: `pub` because rFinal's whole-body differential and
// the step-4 flip both drive the provider path from rue-compiler, where the
// pool's durable source is built from concrete nucleus signatures (an opaque
// `BodyFactProvider` associated type rue-air cannot destructure). The sole
// pre-flip caller is the rue-compiler differential; the flip promotes it to the
// production analyzer. Every method here is a thin composition — pool consult
// (P), provider point query (C/B), or RIR-index handle fill — with no resolution
// LOGIC of its own (selection stays in [`classify_static_call`] /
// [`resolve_static_call_reference`], the provider-generic free functions above).
//
// Feasibility (r4a design-checkpoint table): P = answered-by-pool, C =
// composed-from-existing-ops, B = boundary-op.
//   - function_info / method_info / named_method_info                 → P
//   - named_method_declaration                                        → P (BodyRirIndex)
//   - named_method_by_callable_symbol                                 → B (callable_symbol_method)
//   - function_contains / resolve_function_name_local                 → C (lookup)
// Disposition (updated by slice r4b-3, which owned this backlog):
//   - method_info / named_method_info → LANDED (r4b-3). The receiver preimage
//     `(owner_file, owner_type_name)` threads through the durable method key: the
//     inherent `method_info` composes the pool's durable method subset (receiver
//     through 2a) with the RIR handle the RIR index locates for the preimage. The
//     rue-compiler differential recovers the receiver by joining the method key's
//     `owner()` back to the owner nominal's durable key.
//   - value_const / module_binding / resolve_const_info_in_file → LANDED
//     (flip-prep): exact `ConstInfo` values materialized through the shared
//     endpoint/pool registry install into this driver's body-local overlay.
//   - module_def → LANDED (flip-prep): `ProviderModuleRegistry` mints the
//     body-local compact id from the durable module handle and stores its
//     current request file/path/import-path facts; this driver exposes the
//     owned definition through `module_def`.
//   - named_method_declaration → LANDED (flip-prep). Both seams now take the
//     provider-natural `(owner_file, owner_type_name, method_name)` preimage;
//     the epoch adapter alone translates it through `structs_by_file_name` to
//     the epoch map's `(StructId, method_name)` key.
//   - source_function_name under specialization → r5 (the specialization name
//     map); identity otherwise.
// ---------------------------------------------------------------------------

/// The call-resolution ProviderFacts driver: answers the family-1C facts from a
/// [`BodyFactProvider`] + the body-scoped identity pool, instead of the epoch
/// `Sema` tables [`EpochFacts`] reads.
///
/// Generic over the provider `P`, the pool durable source `S`, and the pool's
/// durable nominal/callable key `K` and module `M` (rue-compiler binds
/// `K = StableDefinitionKey`, `M = ModuleId`). The RIR index and interner are
/// body-query inputs (the shared whole-program `Rir`), never durable state — the
/// request/RIR-carried remainder of each identity (spans, `@allow` flags,
/// `is_extern`) is filled from them exactly as production's `binding_manifest`
/// fills it, so the pool's durable-signature subset and the RIR handle compose
/// to a byte-equivalent `FunctionInfo`/`MethodInfo` (the 2c capstone contract).
pub struct ProviderCallFacts<'a, P, S, K, M> {
    provider: &'a P,
    identity: ProviderIdentityContext<K, M, S>,
    rir: BodyRirView<'a>,
    value_consts: RefCell<HashMap<(u32, Spur), ConstInfo>>,
    module_bindings: RefCell<HashMap<(u32, Spur), ConstInfo>>,
}

impl<'a, P, S, K, M> ProviderCallFacts<'a, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M> + DurableCallableSource<K, M>,
    K: Clone + Eq + Hash,
    M: Eq + Hash,
{
    /// Construct the driver over one request-local RIR bundle and identity
    /// context. The bundle's declaration index is shared rather than rebuilt.
    pub fn new(provider: &'a P, source: S, rir: BodyRirView<'a>) -> Self {
        let identity = ProviderIdentityContext::new(source);
        Self::with_identity(provider, identity, rir)
    }

    /// Construct the call facade from the one task-local provider authority.
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
            value_consts: RefCell::new(HashMap::new()),
            module_bindings: RefCell::new(HashMap::new()),
        }
    }

    /// The body-local minted type pool, so a differential reads its metadata for
    /// the index-independent render/copyability comparison (the pool mints its
    /// own ids; parity is asserted through displays, never a pool-relative index
    /// — the 2a/2b contract).
    pub fn with_type_pool<R>(&self, read: impl FnOnce(&crate::TypeInternPool) -> R) -> R {
        let pool = self.identity.type_pool();
        read(&pool)
    }

    /// The body-local parameter arena backing every minted [`FunctionInfo`] /
    /// [`MethodInfo`] `params` range.
    pub fn with_param_arena<R>(&self, read: impl FnOnce(&crate::ParamArena) -> R) -> R {
        read(self.identity.pool().param_arena())
    }

    /// Resolve a pool-interner [`Spur`] (e.g. a minted `params` name symbol) to
    /// its source string; the pool's symbols are interner-relative, so name
    /// parity is asserted through resolved strings.
    pub fn resolve_symbol(&self, symbol: Spur) -> String {
        self.identity.pool().resolve_symbol(symbol).to_owned()
    }

    /// Intern a body-local name in the shared provider identity universe.
    pub fn name_symbol(&self, name: &str) -> Spur {
        self.identity.pool().intern_name(name)
    }

    /// Install an already materialized value constant into the call family's
    /// body-local overlay. The `ConstInfo` type belongs to this driver's shared
    /// identity universe.
    pub fn register_value_const(&self, file: FileId, name: &str, info: ConstInfo) {
        let name = self.identity.pool().intern_name(name);
        self.value_consts
            .borrow_mut()
            .insert((file.index(), name), info);
    }

    pub fn value_const(&self, file: FileId, name: &str) -> Option<ConstInfo> {
        let name = self.identity.pool().intern_name(name);
        self.value_consts
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    pub fn register_module_binding(&self, file: FileId, name: &str, info: ConstInfo) {
        let name = self.identity.pool().intern_name(name);
        self.module_bindings
            .borrow_mut()
            .insert((file.index(), name), info);
    }

    pub fn module_binding(&self, file: FileId, name: &str) -> Option<ConstInfo> {
        let name = self.identity.pool().intern_name(name);
        self.module_bindings
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    /// Register one durable module and its current request/presentation facts.
    /// Module-member call resolution uses the returned body-local compact id;
    /// [`Self::module_def`] recovers the same owned definition the epoch
    /// `CallResolutionFacts::module_def` consult returns.
    pub fn register_module(
        &self,
        module: M,
        file: FileId,
        file_path: &str,
        import_path: &str,
        durable_id: &str,
    ) -> Option<ModuleId> {
        self.identity
            .modules_mut()
            .register(module, file, file_path, import_path, durable_id)
    }

    /// The registered provider-era module definition for a compact id.
    pub fn module_def(&self, module: ModuleId) -> Option<ModuleDef> {
        self.identity.modules().get(module)
    }

    /// (P) Assemble a [`FunctionInfo`] for a durable free-function key, composing
    /// the pool's durable-signature subset (2b, whose nominal parameter types
    /// resolve through 2a) with the request/RIR handle located by the RIR index
    /// (2c). `source_name`/`file` locate the declaration in the shared `Rir`.
    /// The r4a-2c span contract holds by construction: the handle sources
    /// `span`/`file_id` from the located `FnDecl` inst, the same inst production
    /// sources them from — a differential asserts, never assumes, the equality.
    pub fn function_info(&self, key: &K, source_name: &str, file: FileId) -> Option<FunctionInfo> {
        let source_sym = self.rir.rir_interner().get(source_name)?;
        let declaration = self.rir.rir_index().first_free_function(source_sym, file)?;
        let handle = self.function_handle(declaration)?;
        self.identity.pool_mut()?.resolve_function(key, handle).ok()
    }

    /// (P) Assemble a [`MethodInfo`] for a durable method key, composing the
    /// pool's durable method subset (2b, receiver through 2a) with the RIR
    /// handle the RIR index locates for `(owner_file, owner_type_name, method)`.
    pub fn method_info(
        &self,
        key: &K,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<MethodInfo> {
        if let Some(info) = self
            .identity
            .method_for_owner(owner_file, owner_type_name, method)
        {
            return Some(info);
        }
        self.resolve_and_register_named_method(key, owner_file, owner_type_name, method)
    }

    /// (P) The *named* method info for `(owner, method)`. Anonymous-owner methods
    /// are a deferred pool arm (r6 anonymous minting), so over the named-method
    /// differential scope this coincides with [`Self::method_info`], mirroring
    /// the epoch's `methods.get` (named-only, no anonymous fallback).
    pub fn named_method_info(
        &self,
        key: &K,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<MethodInfo> {
        if let Some(info) =
            self.identity
                .named_method_for_owner(owner_file, owner_type_name, method)
        {
            return Some(info);
        }
        self.resolve_and_register_named_method(key, owner_file, owner_type_name, method)
    }

    fn resolve_and_register_named_method(
        &self,
        key: &K,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<MethodInfo> {
        let declaration = self.named_method_declaration(owner_file, owner_type_name, method)?;
        let handle = self.method_handle(declaration)?;
        let info = self.identity.pool_mut()?.resolve_method(key, handle).ok()?;
        self.identity
            .register_named_method(owner_file, owner_type_name, method, info)
            .then_some(info)
    }

    /// Install one anonymous method atomically under its compact and
    /// durable-owner lookup preimages. Endpoint and call facades cloned from
    /// this context therefore observe the same anonymous-first result.
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

    /// (P) The named-method RIR declaration for `(owner_file, owner_type_name,
    /// method)` — the durable-available preimage of the epoch's `(StructId,
    /// method)` key, answered by [`BodyRirIndex`]. Equal to the epoch's
    /// `named_method_declarations.get` under the `struct_by_file_name`
    /// bijection.
    ///
    /// This driver keys the op by the preimage directly (provider-natural), the
    /// r4a-2c "prefer rethreading" resolution. It never mints or consults a pool
    /// `StructId`; the production seam now carries this same preimage.
    pub fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<InstRef> {
        let owner_sym = self.rir.rir_interner().get(owner_type_name)?;
        let method_sym = self.rir.rir_interner().get(method)?;
        self.rir
            .rir_index()
            .named_method_declaration(owner_file, owner_sym, method_sym)
    }

    /// (B) Reverse a rendered callable symbol to its `(receiver, method)` via the
    /// r4a-1 boundary op. Answers `None` for a bare (unqualified, `$`-less)
    /// symbol — a builtin / language-item / anonymous owner whose defining module
    /// the boundary cannot recover — exactly as the epoch's callable index
    /// answers `Some` for such a symbol. That intentional epoch=`Some` /
    /// provider=`None` divergence is the r6-tied bare-owner known-divergence the
    /// differential pins.
    pub fn callable_symbol_receiver(
        &self,
        symbol: &str,
    ) -> Option<(P::ReceiverType, std::sync::Arc<str>)> {
        self.provider.callable_symbol_method(symbol)
    }

    /// (C) Whether a source name resolves to a declared free function in `module`
    /// — the provider `lookup_unqualified` (ModuleItem, Function kind) analog of
    /// the epoch's `functions.contains_key`. Selection (kind filter) stays here
    /// against the returned candidate set, honoring the boundary's
    /// candidate-sets-not-winners contract.
    pub fn function_contains_in_module(&self, module: &P::ModuleRef, source_name: &str) -> bool {
        use super::provider::{NameResolution, ProviderDefinitionKind, ProviderNamespace};
        let resolution =
            self.provider
                .lookup_unqualified(module, ProviderNamespace::ModuleItem, source_name);
        matches!(
            resolution.of_kind(ProviderDefinitionKind::Function),
            NameResolution::Unique(_) | NameResolution::Ambiguous(_)
        )
    }

    /// Fill a [`FunctionIdentityHandle`] from the located `FnDecl` inst — the
    /// verbatim request/RIR reads production performs (`binding_manifest.rs`):
    /// `body`, the pre-resolution return symbol, the RIR-only
    /// `is_extern`/`is_c_export`, and the `@allow` directive flags.
    fn function_handle(&self, declaration: InstRef) -> Option<FunctionIdentityHandle> {
        let inst = self.rir.rir().get(declaration);
        let InstData::FnDecl {
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
        let dirs = self.rir.rir().directives(directives);
        Some(FunctionIdentityHandle {
            body: *body,
            declaration,
            span: inst.span,
            return_type_sym: *return_type,
            is_extern: *is_extern,
            is_c_export: *is_c_export,
            allow_unused_function: self.has_allow(dirs.iter(), "unused_function"),
            allow_unused_variable: self.has_allow(dirs.iter(), "unused_variable"),
            allow_unreachable_code: self.has_allow(dirs.iter(), "unreachable_code"),
            file_id: inst.span.file_id,
        })
    }

    /// Fill a [`MethodIdentityHandle`] from the located method `FnDecl` inst.
    fn method_handle(&self, declaration: InstRef) -> Option<MethodIdentityHandle> {
        let inst = self.rir.rir().get(declaration);
        let InstData::FnDecl {
            body,
            self_mode,
            self_is_mut,
            ..
        } = &inst.data
        else {
            return None;
        };
        Some(MethodIdentityHandle {
            body: *body,
            span: inst.span,
            self_mode: *self_mode,
            self_is_mut: *self_is_mut,
        })
    }

    /// The `@allow(<warning>)` check over a directive view, replicated from
    /// `Sema::has_allow_directive` against the RIR interner (the driver holds no
    /// `Sema`).
    fn has_allow<'r>(
        &self,
        mut directives: impl Iterator<Item = rue_rir::RirDirectiveView<'r>>,
        warning_name: &str,
    ) -> bool {
        let allow_sym = self.rir.rir_interner().get("allow");
        let warning_sym = self.rir.rir_interner().get(warning_name);
        directives.any(|directive| {
            Some(directive.name) == allow_sym
                && directive.args.iter().any(|arg| Some(*arg) == warning_sym)
        })
    }
}
