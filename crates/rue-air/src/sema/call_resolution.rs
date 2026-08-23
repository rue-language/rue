//! Call, method, operator, and module-member resolution for body analysis.
//!
//! Selection policy lives in provider-generic functions over
//! [`CallResolutionFacts`]. The query-backed body host supplies the facts; the
//! declaration analyzer implements the same vocabulary for declaration work.
//!
//! Where the analyzer selects a winner across *several* candidate reads — the
//! static call-reference resolver's const-alias-then-local order — that
//! selection lives in the provider-generic free function below
//! ([`resolve_static_call_reference`]) rather than in the impl, so every fact
//! host replays the exact same candidate order, short-circuits, and
//! first-match-wins tie-breaks (RUE-1091 risk R1). Winner-picking that a
//! single epoch accessor already performs internally (`method_info`'s
//! anonymous-then-named fallback) stays inside that point query, matching the
//! `body_endpoint` convention.

use ahash::AHashMap;
use std::cell::RefCell;
use std::hash::Hash;

use lasso::Spur;
use rue_rir::{InstData, InstRef};
use rue_span::FileId;

use super::body_identity::{
    BodyRirView, DurableCallableSource, DurableNominalSource, FunctionIdentityHandle,
    MethodIdentityHandle, ProviderIdentityContext,
};
use super::info::{ConstInfo, FunctionCallInfo, FunctionInfo, MethodCallInfo, MethodInfo};
use super::provider::BodyFactProvider;
use crate::types::{ModuleDef, ModuleId, StructId};

/// The exact call/method/operator-resolution fact boundary consumed by the
/// family-1C analyzer sites. Every operation answers one point query against
/// the declaration universe and returns owned/`Copy` data — no borrowed
/// declaration table or live analyzer handle escapes.
pub(crate) trait CallResolutionFacts {
    /// The signature/binding info for an internal free-function symbol.
    fn call_function_info(&self, name: Spur) -> Option<FunctionCallInfo>;

    /// The source name a specialized/internal function name derives from
    /// (identity when unmapped).
    fn call_source_function_name(&self, name: Spur) -> Spur;

    /// The internal free-function symbol a source name resolves to inside its
    /// own file.
    fn call_resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur>;

    /// The value-constant info declared as `(file, name)` — a file-scoped
    /// `value_const` lookup.
    fn call_resolve_const_info_in_file(&self, name: Spur, file: FileId) -> Option<ConstInfo>;

    /// The value-constant info declared as `(file, name)`. Mirrors
    /// `value_const`; distinct from [`Self::call_resolve_const_info_in_file`] only in
    /// how the key is spelled at the call site.
    fn call_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo>;

    /// The module-binding const declared as `(file, name)`. Mirrors
    /// `module_binding`.
    fn call_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo>;

    /// The method/associated-function info for `(struct, name)`, preferring the
    /// anonymous table then the named table.
    fn call_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodCallInfo>;

    /// The module definition for a module id. Mirrors
    /// `module_registry.get_def`.
    fn call_module_def(&self, module_id: ModuleId) -> ModuleDef;
}

// ---------------------------------------------------------------------------
// `ProviderCallFacts` — call-resolution facts.
//
// This driver answers call, method, and operator facts from the exact body-fact provider
// boundary ([`BodyFactProvider`], realized in production by rue-compiler's
// `CompilerBodyFactProvider`) plus the body-scoped identity pool.
//
// This surface is public because rue-compiler supplies the concrete durable
// signature source behind the opaque provider boundary. Every method here is a
// thin pool consult, provider point query, or RIR-index handle fill, with no resolution
// LOGIC of its own (selection stays in [`resolve_static_call_reference`], the
// provider-generic free function above).
// ---------------------------------------------------------------------------

/// Query-backed call-resolution fact state. It owns the body-scoped identity
/// pool, request RIR view, and consulted overlays; the epoch host reads its own
/// tables directly.
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
    value_consts: RefCell<AHashMap<(u32, Spur), ConstInfo>>,
    module_bindings: RefCell<AHashMap<(u32, Spur), ConstInfo>>,
}

impl<'a, P, S, K, M> ProviderCallFacts<'a, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M>
        + super::DurableAnonymousSource<K, M>
        + DurableCallableSource<K, M>,
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
{
    /// Construct the driver over one request-local RIR bundle and identity
    /// context. The bundle's declaration index is shared rather than rebuilt.
    pub fn new(provider: &'a P, source: S, rir: BodyRirView<'a>) -> Self {
        let identity = ProviderIdentityContext::new(source);
        Self::with_identity(provider, identity, rir)
    }

    /// Construct the call fact state from the one task-local provider authority.
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
            value_consts: RefCell::new(AHashMap::new()),
            module_bindings: RefCell::new(AHashMap::new()),
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
    pub fn name_symbol(&self, name: &str) -> Result<Spur, lasso::LassoErrorKind> {
        self.identity.pool().intern_name(name)
    }

    /// Install an already materialized value constant into the call family's
    /// body-local overlay. The `ConstInfo` type belongs to this driver's shared
    /// identity universe.
    pub fn register_value_const(&self, file: FileId, name: &str, info: ConstInfo) {
        let Ok(name) = self.identity.pool().intern_name(name) else {
            return;
        };
        self.value_consts
            .borrow_mut()
            .insert((file.index(), name), info);
    }

    pub fn value_const(&self, file: FileId, name: &str) -> Option<ConstInfo> {
        let Ok(name) = self.identity.pool().intern_name(name) else {
            return None;
        };
        self.value_consts
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    pub fn register_module_binding(&self, file: FileId, name: &str, info: ConstInfo) {
        let Ok(name) = self.identity.pool().intern_name(name) else {
            return;
        };
        self.module_bindings
            .borrow_mut()
            .insert((file.index(), name), info);
    }

    pub fn module_binding(&self, file: FileId, name: &str) -> Option<ConstInfo> {
        let Ok(name) = self.identity.pool().intern_name(name) else {
            return None;
        };
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

    /// The registered module definition for a compact id.
    pub fn module_def(&self, module: ModuleId) -> Option<ModuleDef> {
        self.identity.modules().get(module)
    }

    /// Assemble a [`FunctionInfo`] for a durable free-function key by combining
    /// the pool's durable signature with the request-local RIR handle.
    /// `source_name`/`file` locate the declaration in the shared `Rir`, which is
    /// also the sole source of its span and file id.
    pub fn function_info(&self, key: &K, source_name: &str, file: FileId) -> Option<FunctionInfo> {
        let source_sym = self.rir.rir_interner().get(source_name)?;
        let declaration = self.rir.rir_index().first_free_function(source_sym, file)?;
        let handle = self.function_handle(declaration)?;
        self.identity.pool_mut()?.resolve_function(key, handle).ok()
    }

    /// Assemble a [`MethodInfo`] for a durable method key by combining the
    /// pool's durable signature with the RIR handle for
    /// `(owner_file, owner_type_name, method)`.
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

    /// Materialize only the durable callable portion of a method for a
    /// body-local call site. The returned record carries the request's local
    /// handles solely to satisfy the shared `MethodInfo` representation; call
    /// resolution consumes its receiver and signature fields and never treats
    /// those handles as the callee body.
    pub(in crate::sema) fn method_signature_info(&self, key: &K) -> Option<MethodCallInfo> {
        self.identity.pool_mut()?.resolve_method_call(key).ok()
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
            .ok()?
            .then_some(info)
    }

    /// Install one anonymous method atomically under its compact and
    /// durable-owner lookup preimages. Endpoint and call fact state cloned from
    /// this context therefore observe the same anonymous-first result.
    pub fn register_anonymous_method(
        &self,
        file: FileId,
        owner: &str,
        method: &str,
        info: MethodInfo,
    ) -> Result<bool, lasso::LassoErrorKind> {
        self.identity
            .register_anonymous_method(file, owner, method, info)
    }

    /// Return the named-method RIR declaration for
    /// `(owner_file, owner_type_name, method)`. The direct source preimage
    /// avoids minting or consulting a pool `StructId`.
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
            return_type_syntax: *return_type,
            returns_type: self
                .rir
                .rir()
                .type_syntax()
                .node(*return_type)
                .and_then(|node| match node {
                    rue_rir::RirTypeSyntaxNode::Named(symbol) => {
                        self.rir.rir().type_syntax().symbol(*symbol)
                    }
                    _ => None,
                })
                .is_some_and(|symbol| self.rir.rir_interner().resolve(symbol) == "type"),
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
            self_is_mut,
            returns_borrow,
            returns_inout,
            ..
        } = &inst.data
        else {
            return None;
        };
        Some(MethodIdentityHandle {
            body: *body,
            span: inst.span,
            self_is_mut: *self_is_mut,
            returns_borrow: *returns_borrow,
            returns_inout: *returns_inout,
        })
    }

    /// The `@allow(<warning>)` check over a directive view against the RIR
    /// interner (the driver holds no analyzer state).
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
