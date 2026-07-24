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

use lasso::Spur;
use rue_rir::InstRef;
use rue_span::FileId;

use super::BodySema;
use super::info::{ConstInfo, FunctionInfo, MethodInfo};
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
