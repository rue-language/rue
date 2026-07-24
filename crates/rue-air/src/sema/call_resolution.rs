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
//! [`EpochFacts`] is the one production implementation: each operation is the
//! verbatim epoch-table read the inline analyzer code performed, so the hoist is
//! byte-identical. Every operation is `&self` and returns owned or `Copy` data,
//! so a caller inside an `&mut Sema` stack constructs a short-lived
//! [`EpochFacts`] per resolution (mirroring `SemaTypeSyntaxProvider`) without
//! retaining a borrow across the surrounding mutations.
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

use std::hash::Hash;

use lasso::{Spur, ThreadedRodeo};
use rue_rir::{InstData, InstRef, Rir};
use rue_span::FileId;

use super::BodySema;
use super::body_identity::{
    BodyIdentityPool, BodyRirIndex, DurableCallableSource, DurableNominalSource,
    FunctionIdentityHandle, MethodIdentityHandle,
};
use super::info::{ConstInfo, FunctionInfo, MethodInfo};
use super::provider::BodyFactProvider;
use crate::types::{ModuleDef, ModuleId, StructId};

/// The exact call/method/operator-resolution fact boundary consumed by the
/// family-1C analyzer sites. Every operation answers one point query against
/// the declaration universe and returns owned/`Copy` data — no borrowed epoch
/// table or live `Sema` handle escapes.
pub(in crate::sema) trait CallResolutionFacts {
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

    /// The named-method RIR declaration for `(struct, name)`. Mirrors
    /// `named_method_declarations.get`.
    fn named_method_declaration(&self, struct_id: StructId, name: Spur) -> Option<InstRef>;

    /// The module definition for a module id. Mirrors
    /// `module_registry.get_def`.
    fn module_def(&self, module_id: ModuleId) -> ModuleDef;
}

/// The production [`CallResolutionFacts`]: every operation is the verbatim
/// epoch-table read the inline analyzer code performed.
pub(in crate::sema) struct EpochFacts<'s, 'a> {
    sema: &'s BodySema<'a>,
}

/// Construct a short-lived [`EpochFacts`] borrowing `sema` for the duration of
/// one call/method/operator resolution.
pub(in crate::sema) fn call_facts<'s, 'a>(sema: &'s BodySema<'a>) -> EpochFacts<'s, 'a> {
    EpochFacts { sema }
}

impl CallResolutionFacts for EpochFacts<'_, '_> {
    fn function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.sema.function_info(name).copied()
    }

    fn function_contains(&self, name: Spur) -> bool {
        self.sema.functions.contains_key(&name)
    }

    fn source_function_name(&self, name: Spur) -> Spur {
        self.sema.source_function_name(name)
    }

    fn resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur> {
        self.sema.resolve_function_name_local(name, file)
    }

    fn resolve_const_info_in_file(&self, name: Spur, file: FileId) -> Option<ConstInfo> {
        self.sema.resolve_const_info_in_file(name, file).cloned()
    }

    fn value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.sema.value_const(&(file, name)).cloned()
    }

    fn module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.sema.module_binding(&(file, name)).cloned()
    }

    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.sema.method_info((struct_id, name)).copied()
    }

    fn named_method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.sema.methods.get(&(struct_id, name)).copied()
    }

    fn named_method_by_callable_symbol(&self, name: Spur) -> Option<(StructId, Spur, MethodInfo)> {
        self.sema
            .named_method_by_callable_symbol(name)
            .map(|(struct_id, method, info)| (struct_id, method, *info))
    }

    fn named_method_declaration(&self, struct_id: StructId, name: Spur) -> Option<InstRef> {
        self.sema
            .named_method_declarations
            .get(&(struct_id, name))
            .copied()
    }

    fn module_def(&self, module_id: ModuleId) -> ModuleDef {
        self.sema.module_registry.get_def(module_id)
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
//   - value_const / module_binding / resolve_const_info_in_file → RE-DEFERRED to
//     the flip. Sharpened reason: a `ConstInfo` carries both a declaration `span`
//     the position-free provider boundary omits (needs a const-declaration RIR
//     handle — the const twin of the function/method handles) AND a
//     comptime-evaluated `value` the durable const payload carries but the body
//     identity pool has no const-minting arm for yet. Both are flip-slice work.
//   - module_def → RE-DEFERRED to the flip. Sharpened reason: it answers a
//     rue-air-internal `ModuleId` registry index the provider has no durable
//     preimage for; the module-facts + logical-path composition an equivalent
//     could be built from belongs with the flip's module registry.
//   - named_method_declaration (the `StructId`-keyed CallResolutionFacts /
//     BodyEndpointProvider trait op) → RE-DEFERRED to the side-B fill / flip. The
//     pool mints its own `StructId`s and exposes no `StructId → (owner_file,
//     owner_type_name)` reverse surface, so a clean StructId-keyed trait
//     translation is not available now; this driver answers the op by its
//     provider-natural preimage (the inherent `named_method_declaration`), the
//     r4a-2c "prefer rethreading" resolution — the analyzer computes the preimage
//     at the call site, so the trait rethreading is a flip concern.
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
    pool: BodyIdentityPool<K, M, S>,
    rir_index: BodyRirIndex,
    rir: &'a Rir,
    /// The whole-program RIR interner. Input names arrive already interned here
    /// (the provider path resolves them to `&str` and re-interns into the pool's
    /// own interner); the RIR index is keyed on this interner's [`Spur`]s.
    rir_interner: &'a ThreadedRodeo,
}

impl<'a, P, S, K, M> ProviderCallFacts<'a, P, S, K, M>
where
    P: BodyFactProvider,
    S: DurableNominalSource<K, M> + DurableCallableSource<K, M>,
    K: Clone + Eq + Hash,
    M: Eq + Hash,
{
    /// Construct the driver over a provider, a durable source, and the shared
    /// whole-program `Rir` + interner. The pool and RIR index are built here;
    /// nominals and callables are minted lazily on first consult.
    pub fn new(provider: &'a P, source: S, rir: &'a Rir, rir_interner: &'a ThreadedRodeo) -> Self {
        Self {
            provider,
            pool: BodyIdentityPool::new(source),
            rir_index: BodyRirIndex::new(rir),
            rir,
            rir_interner,
        }
    }

    /// The body-local minted type pool, so a differential reads its metadata for
    /// the index-independent render/copyability comparison (the pool mints its
    /// own ids; parity is asserted through displays, never a pool-relative index
    /// — the 2a/2b contract).
    pub fn type_pool(&self) -> &crate::TypeInternPool {
        self.pool.type_pool()
    }

    /// The body-local parameter arena backing every minted [`FunctionInfo`] /
    /// [`MethodInfo`] `params` range.
    pub fn param_arena(&self) -> &crate::ParamArena {
        self.pool.param_arena()
    }

    /// Resolve a pool-interner [`Spur`] (e.g. a minted `params` name symbol) to
    /// its source string; the pool's symbols are interner-relative, so name
    /// parity is asserted through resolved strings.
    pub fn resolve_symbol(&self, symbol: Spur) -> &str {
        self.pool.resolve_symbol(symbol)
    }

    /// (P) Assemble a [`FunctionInfo`] for a durable free-function key, composing
    /// the pool's durable-signature subset (2b, whose nominal parameter types
    /// resolve through 2a) with the request/RIR handle located by the RIR index
    /// (2c). `source_name`/`file` locate the declaration in the shared `Rir`.
    /// The r4a-2c span contract holds by construction: the handle sources
    /// `span`/`file_id` from the located `FnDecl` inst, the same inst production
    /// sources them from — a differential asserts, never assumes, the equality.
    pub fn function_info(
        &mut self,
        key: &K,
        source_name: &str,
        file: FileId,
    ) -> Option<FunctionInfo> {
        let source_sym = self.rir_interner.get(source_name)?;
        let declaration = self.rir_index.first_free_function(source_sym, file)?;
        let handle = self.function_handle(declaration)?;
        self.pool.resolve_function(key, handle).ok()
    }

    /// (P) Assemble a [`MethodInfo`] for a durable method key, composing the
    /// pool's durable method subset (2b, receiver through 2a) with the RIR
    /// handle the RIR index locates for `(owner_file, owner_type_name, method)`.
    pub fn method_info(
        &mut self,
        key: &K,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<MethodInfo> {
        let declaration = self.named_method_declaration(owner_file, owner_type_name, method)?;
        let handle = self.method_handle(declaration)?;
        self.pool.resolve_method(key, handle).ok()
    }

    /// (P) The *named* method info for `(owner, method)`. Anonymous-owner methods
    /// are a deferred pool arm (r6 anonymous minting), so over the named-method
    /// differential scope this coincides with [`Self::method_info`], mirroring
    /// the epoch's `methods.get` (named-only, no anonymous fallback).
    pub fn named_method_info(
        &mut self,
        key: &K,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<MethodInfo> {
        self.method_info(key, owner_file, owner_type_name, method)
    }

    /// (P) The named-method RIR declaration for `(owner_file, owner_type_name,
    /// method)` — the durable-available preimage of the epoch's `(StructId,
    /// method)` key, answered by [`BodyRirIndex`]. Equal to the epoch's
    /// `named_method_declarations.get` under the `struct_by_file_name`
    /// bijection.
    ///
    /// This driver keys the op by the preimage DIRECTLY (provider-natural), which
    /// IS the r4a-2c "prefer rethreading" resolution: it never mints or consults a
    /// pool `StructId`, so the endpoint seam's production trait signature stays
    /// untouched. The `StructId`-keyed `CallResolutionFacts::named_method_
    /// declaration(StructId, name)` trait impl — which must map its incoming
    /// `StructId` back to this preimage — is the endpoint-seam slice r4b-3's
    /// concern (it owns receiver→pool identity); r4b-1 exposes the preimage-keyed
    /// answer the flip will drive.
    pub fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: &str,
        method: &str,
    ) -> Option<InstRef> {
        let owner_sym = self.rir_interner.get(owner_type_name)?;
        let method_sym = self.rir_interner.get(method)?;
        self.rir_index
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
        let inst = self.rir.get(declaration);
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
        let dirs = self.rir.directives(directives);
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
        let inst = self.rir.get(declaration);
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
        let allow_sym = self.rir_interner.get("allow");
        let warning_sym = self.rir_interner.get(warning_name);
        directives.any(|directive| {
            Some(directive.name) == allow_sym
                && directive.args.iter().any(|arg| Some(*arg) == warning_sym)
        })
    }
}
