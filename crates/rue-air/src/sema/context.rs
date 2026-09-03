//! Analysis context and helper types for semantic analysis.
//!
//! This module contains the supporting structures used during function body
//! analysis, including local variable tracking and scope management. Move,
//! borrow, and exclusivity state lives in [`super::ownership_state`] and is
//! embedded here as [`AnalysisContext::ownership`].

use ahash::{AHashMap, AHashSet};
use std::cell::{Cell, RefCell};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use lasso::Spur;
use rue_error::CompileWarning;
use rue_rir::{InstRef, RirParamMode, SymbolHandle};
use rue_span::{FileId, Span};

use super::ownership_state::OwnershipState;
use crate::inst::{AirPlaceBase, AirProjection};
use crate::scope::ScopedContext;
use crate::types::{StructId, Type};

/// Information about a local variable.
#[derive(Debug, Clone)]
pub(crate) struct LocalVar {
    /// Slot index for this variable
    pub slot: u32,
    /// Type of the variable
    pub ty: Type,
    /// Whether the variable is mutable
    pub is_mut: bool,
    /// Span of the variable declaration (for unused variable warnings)
    pub span: Span,
    /// Whether @allow(unused_variable) was applied to this binding
    pub allow_unused: bool,
}

/// Information about a function parameter.
#[derive(Debug, Clone)]
pub(crate) struct ParamInfo {
    /// Parameter name symbol
    pub name: Spur,
    /// Starting ABI slot for this parameter (0-based).
    /// For scalar types, this is the single slot.
    /// For struct types, this is the first field's slot.
    pub abi_slot: u32,
    /// Parameter type
    pub ty: Type,
    /// Parameter passing mode
    pub mode: RirParamMode,
    /// Whether the parameter is declared `comptime` (carried separately from
    /// `mode`, which stays `Normal` for comptime parameters — see
    /// `RirParam::is_comptime`). Used to allow forwarding a comptime
    /// parameter to another function's comptime parameter (spec 4.14:5).
    pub is_comptime: bool,
    /// Whether the binding is mutable in the body despite being by-value
    /// (`mode == Normal`). Today this is only true for a `mut self`
    /// receiver; mutations affect the callee's copy only, with no
    /// write-back to the caller (that is `Inout`).
    pub is_mut: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CheckedConstIndexScopeKey(Rc<()>);

impl CheckedConstIndexScopeKey {
    pub(crate) fn fresh() -> Self {
        Self(Rc::new(()))
    }
}

impl PartialEq for CheckedConstIndexScopeKey {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CheckedConstIndexScopeKey {}

impl Hash for CheckedConstIndexScopeKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Rc::as_ptr(&self.0).hash(state);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CheckedConstIndexScopeState {
    current: CheckedConstIndexScopeKey,
    stack: Vec<CheckedConstIndexScopeKey>,
}

impl CheckedConstIndexScopeState {
    pub(crate) fn new() -> Self {
        Self {
            current: CheckedConstIndexScopeKey::fresh(),
            stack: Vec::new(),
        }
    }

    fn key(&self) -> CheckedConstIndexScopeKey {
        self.current.clone()
    }

    fn changed(&mut self) {
        self.current = CheckedConstIndexScopeKey::fresh();
    }

    fn push(&mut self) {
        self.stack.push(self.current.clone());
    }

    fn pop(&mut self) {
        self.current = self
            .stack
            .pop()
            .expect("analysis scope identity stack stays parallel to lexical scopes");
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CheckedConstIndexCacheKey {
    root: InstRef,
    scope: CheckedConstIndexScopeKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CheckedConstIndexCandidateKey {
    span: Span,
}

#[derive(Debug, Clone)]
pub(crate) struct CheckedConstIndexCandidate {
    pub root: InstRef,
    pub value: i128,
    pub runtime_local_names: AHashSet<Spur>,
    pub comptime_type_vars: AHashMap<Spur, Type>,
}

/// Read-only outside this module so scope-state mutations cannot bypass the
/// checked-index identity refresh/restore paths.
#[derive(Debug, Clone)]
pub(crate) struct ScopeStateMap<V>(AHashMap<Spur, V>);

impl<V> ScopeStateMap<V> {
    pub(crate) fn new(values: AHashMap<Spur, V>) -> Self {
        Self(values)
    }

    fn insert(&mut self, name: Spur, value: V) -> Option<V> {
        self.0.insert(name, value)
    }

    fn remove(&mut self, name: &Spur) -> Option<V> {
        self.0.remove(name)
    }

    pub(crate) fn snapshot(&self) -> AHashMap<Spur, V>
    where
        V: Clone,
    {
        self.0.clone()
    }
}

impl<V> std::ops::Deref for ScopeStateMap<V> {
    type Target = AHashMap<Spur, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Resolves parameter names without making tiny signatures pay for an index.
///
/// Most Rue functions have only a handful of parameters, where a flat scan is
/// cheaper than allocating a map. Larger signatures are the case where the
/// repeated semantic lookups become quadratic in parameter uses, so they get
/// one body-scoped index shared by every analysis pass.
#[derive(Debug)]
pub(crate) struct ParamIndex {
    by_name: Option<AHashMap<Spur, usize>>,
}

impl ParamIndex {
    const LINEAR_LOOKUP_LIMIT: usize = 8;

    pub(crate) fn new(params: &[ParamInfo]) -> Self {
        let by_name = (params.len() > Self::LINEAR_LOOKUP_LIMIT).then(|| {
            let mut by_name = AHashMap::with_capacity(params.len());
            for (index, param) in params.iter().enumerate() {
                let previous = by_name.insert(param.name, index);
                assert!(
                    previous.is_none(),
                    "parameter names are unique before body analysis"
                );
            }
            by_name
        });
        Self { by_name }
    }

    pub(crate) fn get<'a>(&self, params: &'a [ParamInfo], name: Spur) -> Option<&'a ParamInfo> {
        match &self.by_name {
            Some(by_name) => by_name.get(&name).map(|index| &params[*index]),
            None => params.iter().find(|param| param.name == name),
        }
    }
}

/// One projection step of a [`PlaceAlias`], mirroring the metadata the place
/// tracer collects (see `analysis::ownership::ProjectionInfo`) in an owned,
/// clonable form.
#[derive(Debug, Clone)]
pub(crate) struct AliasProjection {
    pub proj: AirProjection,
    pub result_type: Type,
    pub field_name: Option<Spur>,
    pub const_index: Option<i64>,
    pub index_segment: Option<Spur>,
}

/// A name bound to a caller place during accessor inlining (ADR-0062).
///
/// While a place-returning accessor call (`-> borrow T` or `-> inout T`) is
/// expanded at its call site, the
/// accessor body's `self` resolves to the *caller's* receiver place: the
/// place tracer substitutes this alias where the body says `self`, so
/// `self.f` composes to `<receiver place>.f` rooted at the caller's own
/// root variable. The alias is a shared borrow: places reached through it
/// are read-only and never moved out of.
#[derive(Debug, Clone)]
pub(crate) struct PlaceAlias {
    pub base: AirPlaceBase,
    pub base_type: Type,
    pub projections: Vec<AliasProjection>,
    pub root_var: Spur,
}

/// Context for analyzing instructions within a function.
///
/// Bundles together the mutable state that needs to be threaded through
/// recursive `analyze_inst` calls.
#[derive(Clone)]
pub(crate) struct AnalysisContext<'a> {
    /// RIR body root of the definition producing values in this analysis.
    /// Anonymous structural anchors are relative to this producer.
    pub producer: InstRef,
    /// Issuer-scoped canonical identity of that producer. Anonymous nominal
    /// allocation consumes this value directly; the RIR handle above is
    /// traversal context only and never enters an entity key. The comptime
    /// arguments the producer was applied to are read back off it rather than
    /// carried alongside (RUE-1699).
    pub canonical_producer: super::anon_structs::IssuedStableProducerId,
    /// Exact callable identity which owns local data atoms in this body.
    pub canonical_function_identity: super::anon_structs::IssuedFunctionInstanceKey,
    /// File that owns the function body currently being analyzed.
    pub current_file_id: FileId,
    /// Local variables in scope
    pub locals: ScopeStateMap<LocalVar>,
    /// Function parameters (immutable reference, shared across the function)
    pub params: &'a [ParamInfo],
    /// Body-scoped parameter-name index shared by every semantic consumer.
    pub param_index: &'a ParamIndex,
    /// Next available slot for local variables
    pub next_slot: u32,
    /// How many loops we're nested inside (for break/continue validation)
    pub loop_depth: u32,
    /// How many `checked` blocks we're nested inside. Unchecked operations —
    /// raw-pointer intrinsics (`@raw`, `@ptr_read`, …) and calls to `unchecked
    /// fn`s — are only legal when this is greater than zero (spec 9.1:1,
    /// chapter 9). An `unchecked fn` body does NOT implicitly count as a
    /// checked context; the modifier only gates *callers* (see spec 9.1:1).
    pub checked_depth: u32,
    /// Local variables that have been read (for unused variable detection)
    pub used_locals: AHashSet<Spur>,
    /// Return type of the current function (for explicit return validation)
    pub return_type: Type,
    /// Scope stack for efficient scope management.
    /// Each entry is a list of (symbol, old_value) pairs for variables added/shadowed in that scope.
    /// When a scope is popped, we restore old values (for shadowed vars) or remove new vars.
    pub scope_stack: Vec<Vec<(Spur, Option<LocalVar>)>>,
    /// Per-scope saved comptime-type-alias bindings, parallel to
    /// `scope_stack`: each frame records the shadowed binding (or absence)
    /// for every name bound in `comptime_type_vars` — or hidden there by a
    /// same-named runtime `let` — in that scope, and `pop_scope` restores
    /// them in reverse. This gives `let`-bound type aliases lexical block
    /// scope (RUE-530): without it the flat map let an alias escape its
    /// block and made sibling-branch aliases collide. Bind aliases through
    /// [`AnalysisContext::bind_comptime_type_var`], never by inserting into
    /// `comptime_type_vars` directly.
    pub comptime_type_scope_stack: Vec<Vec<(Spur, Option<Type>)>>,
    /// Persistent identity of the exact runtime-name/type-alias environment.
    /// Scope mutations replace it; scope pop restores the saved identity.
    pub(crate) checked_const_index_scope_state: CheckedConstIndexScopeState,
    /// Resolved types from HM inference.
    /// Maps RIR instruction refs to their resolved concrete types.
    /// This is populated by running constraint generation and unification
    /// before AIR emission.
    pub resolved_types: &'a AHashMap<InstRef, Type>,
    /// Canonical normal-continuation facts produced by the same inference walk
    /// as `resolved_types`. Semantic consumers use these facts rather than
    /// rediscovering divergence from a construct's surface result type.
    pub resolved_continues: &'a AHashMap<InstRef, bool>,
    /// Canonical compile-time selector facts produced by the bounded inference
    /// probe. Semantic control-flow analysis consumes these facts directly so
    /// branch and match selection has one evaluator-owned decision path.
    pub comptime_selections: &'a AHashMap<InstRef, super::ComptimeSelection>,
    /// Statically reachable divergence observed while analyzing the current
    /// expression. Multiple provenance bits may be present when different
    /// paths terminate in different ways; retaining that set prevents a later unreachable
    /// operand or a branch-order choice from changing ownership semantics.
    pub divergence_kinds: DivergenceKinds,
    /// The move/borrow/exclusivity state machine for this body: variable
    /// move states, per-scope shadow frames, loop break/continue snapshots,
    /// active call loans, iteration borrows, and the full-expression
    /// exclusivity ledgers — see [`OwnershipState`] (RUE-1802).
    pub ownership: OwnershipState,
    /// Warnings collected during this function's analysis.
    /// Finalization merges these per-function warnings into the global output.
    pub warnings: Vec<CompileWarning>,
    /// Whether the current function carries `@allow(unused_variable)`.
    /// When set, every local unused-variable warning in this body is
    /// suppressed (spec 2.5:17).
    pub allow_unused_variables: bool,
    /// Local string table: maps string content to local index (for deduplication within function).
    /// Finalization merges these strings globally after body analysis.
    pub local_string_table: AHashMap<String, u32>,
    /// Local string data indexed by local string table index.
    /// After analysis, these are merged into the global string table with ID remapping.
    pub local_strings: Vec<String>,
    /// Every source occurrence, including aliases sharing one dense string ID.
    pub local_atoms:
        Vec<crate::LocalAtomRecord<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
    /// Comptime type variables: maps variable symbols to their compile-time type values.
    /// When a variable is bound to a comptime type (e.g., `let P = make_point()` where
    /// `make_point() -> type`), this map stores the resolved type so it can be used
    /// as a type annotation (e.g., `let p: P = ...`).
    pub comptime_type_vars: ScopeStateMap<Type>,
    /// Comptime value variables: maps variable symbols to their compile-time constant values.
    /// When an anonymous struct method captures comptime parameters from the enclosing function
    /// (e.g., `fn FixedBuffer(comptime N: i32)` creates a struct with methods that reference `N`),
    /// this map stores the captured values so method bodies can resolve them.
    pub comptime_value_vars: AHashMap<Spur, ConstValue>,
    /// Successful checked integer-index evaluations for this one canonical
    /// body analysis. AstGen deliberately duplicates a compound assignment's
    /// source index into its read and write RIR nodes. Exact-root hits are
    /// constant-time; distinct roots first share a cheap source-occurrence
    /// candidate and are reused only after typed structural equivalence. Both
    /// The exact-root lookup carries every mutable environment component
    /// consulted by `ComptimeEnv::for_analysis`.
    /// Producer, specialization/value substitutions, file/import identity, and
    /// resolved types are fixed by this context's body-local lifetime.
    pub checked_const_index_cache: Rc<RefCell<AHashMap<CheckedConstIndexCacheKey, i128>>>,
    /// Successful source-occurrence candidates. A distinct AstGen duplicate
    /// reaches structural and relevant-scope validation only after this cheap
    /// span lookup. Candidate environment snapshots are created only for a
    /// successful evaluator result; a failing probe can compare an existing
    /// candidate but is never itself admitted.
    pub checked_const_index_candidates:
        Rc<RefCell<AHashMap<CheckedConstIndexCandidateKey, Vec<CheckedConstIndexCandidate>>>>,
    /// Actual checked index evaluator invocations in this body. Cache hits do
    /// not increment this production work counter.
    pub checked_const_index_evaluations: Rc<Cell<u64>>,
    /// Successful checked index cache lookups in this body.
    pub checked_const_index_cache_hits: Rc<Cell<u64>>,
    /// Distinct-root candidates subjected to typed structural comparison.
    pub checked_const_index_candidate_comparisons: Rc<Cell<u64>>,
    /// RIR node pairs visited by those comparisons.
    pub checked_const_index_comparison_nodes: Rc<Cell<u64>>,
    /// Functions referenced during analysis of this function.
    /// Used for demand-driven semantic analysis (ADR-0045) to track
    /// which functions need to be analyzed. Each entry is a function name symbol.
    pub referenced_functions: AHashSet<Spur>,
    /// Methods referenced during analysis of this function.
    /// Each entry is (struct_id, method_name) matching the key format in methods map.
    pub(crate) referenced_methods: ahash::AHashSet<(StructId, Spur)>,
    /// The type this expression is expected to produce, when sema knows it from
    /// a surrounding annotation or pattern. Set narrowly around a
    /// let-initializer (to the resolved annotation type) and around a `match`
    /// scrutinee (to the enum named by the arm patterns). The fallible
    /// intrinsics (`@read_line`, `@parse_*`) read this to validate that an
    /// annotation or pattern expects their exact registry-installed
    /// `Option(T)` (RUE-6, ADR-0038). Context never selects that nominal. Left
    /// `None` everywhere else, so no other analysis is affected.
    pub expected_type: Option<Type>,
    /// Integer type supplied only while semantic recovery walks a call
    /// argument whose root is absent from the inference result. This is
    /// separate from `expected_type`: operator dispatch deliberately clears
    /// ordinary result expectations before visiting operands, while every
    /// integer node in a skipped malformed-constructor subtree still needs a
    /// deterministic type. `None` on the canonical inferred path keeps a
    /// genuinely missing inference fact classified as an internal error.
    pub missing_inference_integer_type: Option<Type>,
    /// Whether analysis is currently walking an inline constructor head that
    /// inference deliberately skipped after comptime reduction failed. Only
    /// call arguments reached inside this scope may synthesize the integer
    /// recovery context above.
    pub recover_missing_ctor_head_arguments: bool,
    /// The shared inference context for this body, threaded here so accessor
    /// call expansion (ADR-0062) can run type inference for the accessor's
    /// body on demand before splicing it into the caller.
    pub infer_ctx: &'a super::inference_ctx::InferenceContext,
    /// When analyzing a place-returning accessor body, the body block's single
    /// trailing `yield` instruction — the only `yield` the body may contain.
    /// `None` outside accessor bodies; a `yield` analyzed while this is
    /// `None` is E0256, and one that is not this exact instruction is E0254.
    pub accessor_trailing_yield: Option<InstRef>,
    /// RIR method-call instructions that expanded as accessor calls in this
    /// body (ADR-0062), mapped to the accessor's method name and receiver
    /// root. Escape-shape checks (`return`/`let`/store/aggregate capture)
    /// consult this after analyzing an operand to reject binding a borrowed
    /// place beyond its full expression, naming the offending accessor.
    pub accessor_call_insts: AHashMap<InstRef, (Spur, Spur)>,
    /// AIR place handles for accessor calls already materialized in this
    /// expression. Compound assignment reuses the same yielded place rather
    /// than expanding and loaning the accessor a second time.
    pub accessor_place_refs: AHashMap<InstRef, (crate::inst::AirPlaceRef, Spur, Type, bool, bool)>,
    /// Resolved-type overlays for accessor bodies currently being inlined,
    /// innermost last. `resolved_type_of` consults these before the body's
    /// own `resolved_types`, letting the caller's analysis walk accessor-body
    /// instructions that the caller's inference never visited.
    pub inline_resolved_types: Vec<Arc<AHashMap<InstRef, Type>>>,
    /// Names bound to caller places during accessor inlining (`self` inside
    /// an inlined accessor body). Scoped save/restore is handled by the
    /// expansion itself.
    pub place_aliases: AHashMap<Spur, PlaceAlias>,
    /// True only while analyzing the operand of a `?` expression (RUE-318). The
    /// `?` site cannot supply an `expected_type` for a *bare* fallible intrinsic
    /// (`@read_line()?` / `@parse_i64(s)?`): the enclosing function's `Option(U)`
    /// has the wrong payload (e.g. `@read_line` is `Option(StrBuf)`, not the
    /// function's `Option(i64)`). The fallible intrinsic instead uses its exact
    /// registry-installed `Option(payload)`. Left `false` everywhere else, where
    /// a contextual enum may validate — but never select — the result identity.
    pub try_operand: bool,
    /// True while analyzing a destructor body. A destructor's `self` is
    /// disposed of by the drop glue after the body runs (spec 3.8:62), so the
    /// early-exit edge walk (RUE-1614) must skip the by-value parameter
    /// obligation exactly as the end-of-body parameter check does.
    pub is_destructor: bool,
    /// True while analyzing the immediate block of a `test` item.
    ///
    /// One rule reads it: `?` has unwrap-and-report semantics in a test body
    /// (ADR-0083 §1, spec 6.7). The flag is the owner kind rather than a
    /// property of the enclosing block, which is exactly the scope the rule
    /// wants — a nested `if`/`while`/`match` arm inside the test body is still
    /// the test body, while a helper the test calls has its own owner kind and
    /// therefore keeps ordinary `?`.
    pub is_test_body: bool,
    /// Body-local call symbol bound to each error type's structural printer.
    ///
    /// Two `?` sites on the same error type share one printer (ADR-0083 §1), so
    /// they share the symbol that names it here too, and export publishes one
    /// callee identity for both.
    pub error_printer_symbols: AHashMap<Type, Spur>,
}

/// Classification of a non-continuing edge for linear scope checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DivergenceKind {
    /// The edge is the explicit, statically known process abort `@panic`.
    Panic,
    /// A scope exit whose linear obligation was checked at the edge.
    Exit,
    /// The edge unwinds or otherwise terminates without the panic exemption.
    Other,
}

/// Reachable divergence provenance accumulated for one expression.
///
/// This is intentionally a tiny set rather than a single "last edge" value:
/// a conditional or sequenced expression can have both an aborting panic edge
/// and an unwinding/other edge, and both remain relevant to its enclosing
/// ownership join.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DivergenceKinds(u8);

impl DivergenceKinds {
    pub const NONE: Self = Self(0);
    pub const PANIC: Self = Self(1);
    pub const OTHER: Self = Self(2);
    pub const EXIT: Self = Self(4);

    pub const fn from_kind(kind: DivergenceKind) -> Self {
        match kind {
            DivergenceKind::Panic => Self::PANIC,
            DivergenceKind::Other => Self::OTHER,
            DivergenceKind::Exit => Self::EXIT,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn has_other(self) -> bool {
        self.0 & Self::OTHER.0 != 0
    }

    pub const fn without_other(self) -> Self {
        Self(self.0 & !Self::OTHER.0)
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn insert(&mut self, kind: DivergenceKind) {
        *self = self.union(Self::from_kind(kind));
    }
}

// Import InstRef for use in resolved_types

impl ScopedContext for AnalysisContext<'_> {
    type VarInfo = LocalVar;

    fn locals_mut(&mut self) -> &mut AHashMap<Spur, Self::VarInfo> {
        &mut self.locals.0
    }

    fn scope_stack_mut(&mut self) -> &mut Vec<Vec<(Spur, Option<Self::VarInfo>)>> {
        &mut self.scope_stack
    }

    /// Insert a local variable, tracking it in the current scope for later cleanup.
    ///
    /// This override also handles the variable's MOVE state. A (re)declaration
    /// is a fresh binding, so it starts with no moves — but the shadowed
    /// binding's move state is SAVED in the current scope's move frame and
    /// restored by `pop_scope`, because `moved_vars` is keyed by name, not
    /// binding identity (RUE-522). Clearing it outright resurrected an
    /// already-moved outer binding once the shadow's block ended (double
    /// destruction); conversely, without the restore, a move of the inner
    /// shadow outlived its block and poisoned the live outer binding (false
    /// E0205). Applies to every scoped binding form that reaches
    /// `insert_local`: nested `let`, `match` payload bindings, loop binders.
    fn insert_local(&mut self, symbol: Spur, var: LocalVar) {
        // Const evaluation observes only runtime-name membership. Replaying a
        // declaration over an already-live runtime binding does not change
        // that state; adding the name or hiding a type alias does.
        if !self.locals.contains_key(&symbol) || self.comptime_type_vars.contains_key(&symbol) {
            self.refresh_checked_const_index_scope_state();
        }
        let old_value = self.locals.insert(symbol, var);
        // Track in the current scope (if any) for cleanup on pop
        if let Some(current_scope) = self.scope_stack.last_mut() {
            current_scope.push((symbol, old_value));
        }
        self.ownership.bind_fresh(symbol);
        // A runtime binding hides any same-named comptime type alias for the
        // rest of this scope — the inner binding wins whatever its kind — and
        // the alias is restored when this scope pops (RUE-530). Without the
        // removal, a type-annotation lookup (which consults
        // `comptime_type_vars` before locals) would resolve through the dead
        // alias.
        if let Some(old_alias) = self.comptime_type_vars.remove(&symbol)
            && let Some(alias_frame) = self.comptime_type_scope_stack.last_mut()
        {
            alias_frame.push((symbol, Some(old_alias)));
        }
    }

    fn push_scope(&mut self) {
        self.checked_const_index_scope_state.push();
        self.scope_stack.push(Vec::with_capacity(2));
        self.ownership.push_scope_frame();
        self.comptime_type_scope_stack.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        // Mirrors the trait default for locals (reverse order — see the trait
        // doc), plus the RUE-522 move-state and RUE-530 type-alias restores
        // from the parallel frames.
        if let Some(scope_entries) = self.scope_stack.pop() {
            for (symbol, old_value) in scope_entries.into_iter().rev() {
                match old_value {
                    Some(old_var) => {
                        self.locals.insert(symbol, old_var);
                    }
                    None => {
                        self.locals.remove(&symbol);
                    }
                }
            }
        }
        self.ownership.pop_scope_frame();
        if let Some(alias_frame) = self.comptime_type_scope_stack.pop() {
            for (symbol, old_alias) in alias_frame.into_iter().rev() {
                match old_alias {
                    Some(ty) => {
                        self.comptime_type_vars.insert(symbol, ty);
                    }
                    None => {
                        self.comptime_type_vars.remove(&symbol);
                    }
                }
            }
        }
        self.checked_const_index_scope_state.pop();
    }
}

impl<'a> AnalysisContext<'a> {
    pub(crate) fn checked_const_index_scope_key(&self) -> CheckedConstIndexScopeKey {
        // `ComptimeEnv` asks locals only for name membership; slots, mutability,
        // ownership state, and local types cannot affect const evaluation.
        // Parameter runtime names are immutable for the body and therefore do
        // not enter this per-probe state. Comptime values, producer identity,
        // defining file/imports, and resolved types are likewise body-fixed.
        self.checked_const_index_scope_state.key()
    }

    fn refresh_checked_const_index_scope_state(&mut self) {
        self.checked_const_index_scope_state.changed();
    }

    pub(crate) fn checked_const_index_cache_key(
        root: InstRef,
        scope: CheckedConstIndexScopeKey,
    ) -> CheckedConstIndexCacheKey {
        CheckedConstIndexCacheKey { root, scope }
    }

    pub(crate) fn checked_const_index_candidate_key(span: Span) -> CheckedConstIndexCandidateKey {
        CheckedConstIndexCandidateKey { span }
    }

    /// Resolve one function parameter through the body's canonical lookup.
    pub(crate) fn param(&self, name: Spur) -> Option<&'a ParamInfo> {
        self.param_index.get(self.params, name)
    }

    /// Whether this body has a function parameter with `name`.
    pub(crate) fn has_param(&self, name: Spur) -> bool {
        self.param(name).is_some()
    }

    /// Run one nested analysis with an explicit expected-type context, then
    /// restore the caller's context before returning its result.
    ///
    /// Expected types are expression-directed: structural result positions
    /// inherit them, while operands such as a call's arguments establish
    /// their own context from the callee contract. Keeping the save/restore in
    /// one helper prevents an early `Err` from leaking either context into a
    /// sibling expression.
    pub(crate) fn with_expected_type<T>(
        &mut self,
        expected_type: Option<Type>,
        analyze: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = std::mem::replace(&mut self.expected_type, expected_type);
        let result = analyze(self);
        self.expected_type = previous;
        result
    }

    /// Bind a `let`-bound comptime type alias (`let P = Point();`), saving
    /// the name's previous binding (or absence) in the current scope's alias
    /// frame so `pop_scope` restores it — the alias is lexically scoped like
    /// any other `let` (RUE-530). Also hides a same-named runtime local for
    /// this scope: the variable-reference and annotation paths consult
    /// `comptime_type_vars` and `locals` in different orders, so leaving
    /// both live would resolve the name inconsistently.
    pub fn bind_comptime_type_var(&mut self, symbol: Spur, ty: Type) {
        if self.comptime_type_vars.get(&symbol).copied() != Some(ty)
            || self.locals.contains_key(&symbol)
        {
            self.refresh_checked_const_index_scope_state();
        }
        let old_alias = self.comptime_type_vars.insert(symbol, ty);
        if let Some(alias_frame) = self.comptime_type_scope_stack.last_mut() {
            alias_frame.push((symbol, old_alias));
        }
        if let Some(old_local) = self.locals.remove(&symbol) {
            if let Some(current_scope) = self.scope_stack.last_mut() {
                current_scope.push((symbol, Some(old_local)));
            }
            self.ownership.bind_fresh(symbol);
        }
    }

    /// Create a scratch copy of this context for the loop back-edge move check.
    ///
    /// A value moved anywhere in a loop's condition or body is already moved
    /// when the back edge re-enters the loop. After analyzing a loop once, the
    /// loop is re-analyzed against a fork of the context (and a scratch `Air`)
    /// whose starting move state is the *post-body* state; any use of a moved
    /// value then surfaces as a `UseAfterMove` error pointing at the move from
    /// the "previous iteration". One re-run reaches a fixpoint: analysis is
    /// deterministic, so re-running from the post-body state marks exactly the
    /// same moves again.
    ///
    /// Output accumulators (warnings, referenced functions/methods) start
    /// empty so the discarded pass doesn't duplicate entries in the real
    /// context.
    pub fn fork_for_loop_recheck(&self) -> AnalysisContext<'a> {
        AnalysisContext {
            producer: self.producer,
            canonical_producer: self.canonical_producer.clone(),
            canonical_function_identity: self.canonical_function_identity.clone(),
            current_file_id: self.current_file_id,
            locals: self.locals.clone(),
            params: self.params,
            param_index: self.param_index,
            next_slot: self.next_slot,
            loop_depth: self.loop_depth,
            checked_depth: self.checked_depth,
            used_locals: self.used_locals.clone(),
            return_type: self.return_type,
            scope_stack: self.scope_stack.clone(),
            comptime_type_scope_stack: self.comptime_type_scope_stack.clone(),
            checked_const_index_scope_state: self.checked_const_index_scope_state.clone(),
            resolved_types: self.resolved_types,
            resolved_continues: self.resolved_continues,
            comptime_selections: self.comptime_selections,
            divergence_kinds: self.divergence_kinds,
            ownership: self.ownership.fork_for_recheck(),
            warnings: Vec::new(),
            allow_unused_variables: self.allow_unused_variables,
            local_string_table: self.local_string_table.clone(),
            local_strings: self.local_strings.clone(),
            local_atoms: self.local_atoms.clone(),
            comptime_type_vars: self.comptime_type_vars.clone(),
            comptime_value_vars: self.comptime_value_vars.clone(),
            checked_const_index_cache: self.checked_const_index_cache.clone(),
            checked_const_index_candidates: self.checked_const_index_candidates.clone(),
            checked_const_index_evaluations: self.checked_const_index_evaluations.clone(),
            checked_const_index_cache_hits: self.checked_const_index_cache_hits.clone(),
            checked_const_index_candidate_comparisons: self
                .checked_const_index_candidate_comparisons
                .clone(),
            checked_const_index_comparison_nodes: self.checked_const_index_comparison_nodes.clone(),
            referenced_functions: AHashSet::new(),
            referenced_methods: AHashSet::new(),
            expected_type: None,
            missing_inference_integer_type: self.missing_inference_integer_type,
            recover_missing_ctor_head_arguments: self.recover_missing_ctor_head_arguments,
            infer_ctx: self.infer_ctx,
            accessor_trailing_yield: self.accessor_trailing_yield,
            accessor_call_insts: self.accessor_call_insts.clone(),
            accessor_place_refs: self.accessor_place_refs.clone(),
            inline_resolved_types: self.inline_resolved_types.clone(),
            place_aliases: self.place_aliases.clone(),
            try_operand: false,
            is_destructor: self.is_destructor,
            is_test_body: self.is_test_body,
            error_printer_symbols: self.error_printer_symbols.clone(),
        }
    }

    /// Look up an instruction's inferred type, consulting the accessor-inline
    /// overlays (innermost first) before the body's own inference results.
    pub fn resolved_type_of(&self, inst_ref: InstRef) -> Option<Type> {
        for overlay in self.inline_resolved_types.iter().rev() {
            if let Some(ty) = overlay.get(&inst_ref) {
                return Some(*ty);
            }
        }
        self.resolved_types.get(&inst_ref).copied()
    }

    /// Return whether inference found a normal outgoing path for an
    /// instruction. Missing entries occur only for instructions analyzed from
    /// an inline overlay; their semantic result remains the fallback.
    pub fn resolved_continues_of(&self, inst_ref: InstRef) -> Option<bool> {
        self.resolved_continues.get(&inst_ref).copied()
    }

    /// Add a string to the local string table, returning its local index.
    ///
    /// This deduplicates strings within a single function. After function analysis
    /// completes, local strings are merged into the global string table with ID
    /// remapping in the AIR instructions.
    pub fn add_local_string(
        &mut self,
        content: String,
        anchor: rue_rir::RirStructuralAnchor,
    ) -> u32 {
        self.add_local_atom(content, crate::LocalAtomKind::String, anchor)
    }

    pub fn add_local_read_only_data(
        &mut self,
        content: String,
        anchor: rue_rir::RirStructuralAnchor,
    ) -> u32 {
        self.add_local_atom(content, crate::LocalAtomKind::ReadOnlyData, anchor)
    }

    /// Intern one compiler-authored string run in this body's local data.
    ///
    /// A source literal is anchored at the syntax that wrote it; text the
    /// compiler writes for itself — the file name, kind, and message a test
    /// body's `?` reports (ADR-0083 §1) — has no such syntax. It is anchored
    /// under a string-literal index no source literal can occupy, so a
    /// synthesized run can never collide with an anchored one, and identical
    /// content is interned once however many sites emit it.
    pub fn add_synthesized_string(&mut self, content: &str) -> u32 {
        if let Some(id) = self.local_string_table.get(content) {
            return *id;
        }
        let ordinal = u32::try_from(self.local_atoms.len()).unwrap_or(u32::MAX);
        let anchor = rue_rir::RirStructuralAnchor::new(vec![
            rue_rir::RirStructuralPathSegment::Body,
            rue_rir::RirStructuralPathSegment::StringLiteral(u32::MAX),
            rue_rir::RirStructuralPathSegment::ReadOnlyData(ordinal),
        ]);
        self.add_local_read_only_data(content.to_owned(), anchor)
    }

    fn add_local_atom(
        &mut self,
        content: String,
        kind: crate::LocalAtomKind,
        anchor: rue_rir::RirStructuralAnchor,
    ) -> u32 {
        let dense_id = self.add_local_string_content(content.clone());
        self.local_atoms.push(crate::LocalAtomRecord {
            identity: crate::LocalAtomId {
                producer: self.canonical_function_identity.clone(),
                kind,
                anchor,
            },
            content: content.into(),
            dense_id,
        });
        dense_id
    }

    fn add_local_string_content(&mut self, content: String) -> u32 {
        use std::collections::hash_map::Entry;
        match self.local_string_table.entry(content) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                let id = self.local_strings.len() as u32;
                self.local_strings.push(e.key().clone());
                e.insert(id);
                id
            }
        }
    }
}

/// Result of analyzing an instruction: the AIR reference and its synthesized type.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisResult {
    /// Reference to the generated AIR instruction
    pub air_ref: AirRef,
    /// The synthesized type of this expression
    pub ty: Type,
    /// Whether evaluation can reach the next instruction normally. This is
    /// deliberately independent of `ty`: a call/initializer keeps its
    /// declared value type even when evaluating an operand diverges.
    pub continues: bool,
}

use crate::inst::AirRef;

impl AnalysisResult {
    #[must_use]
    pub fn new(air_ref: AirRef, ty: Type) -> Self {
        Self {
            air_ref,
            ty,
            continues: true,
        }
    }

    #[must_use]
    pub fn with_continues(air_ref: AirRef, ty: Type, continues: bool) -> Self {
        Self {
            air_ref,
            ty,
            continues,
        }
    }

    #[must_use]
    pub fn diverged(air_ref: AirRef, ty: Type) -> Self {
        Self::with_continues(air_ref, ty, false)
    }
}

/// Represents a compile-time constant value.
///
/// This is used for compile-time evaluation of expressions and for
/// comptime parameters. For example, in `fn Buffer(comptime N: i32)`,
/// the value of `N` is stored as a `ConstValue::Integer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstValue {
    /// Integer value. Backed by `i128` so the full range of every Rue integer
    /// type is representable (u64 values above `i64::MAX` as well as negative
    /// signed values). Range checks against the expression's Rue type happen
    /// in the comptime evaluator (see `sema::comptime_eval`).
    Integer(i128),
    /// Boolean value
    Bool(bool),
    /// Type value - stores a concrete type for type parameters.
    /// This is used when a `comptime T: type` parameter is instantiated
    /// with a specific type like `i32` or `bool`.
    Type(Type),
    /// Function reference - stores the callee symbol.
    ///
    /// Function-valued constants are currently callable aliases only: they can
    /// be used as the callee of a call expression, but cannot be materialized
    /// as ordinary runtime values.
    ///
    /// The payload is an equality-only [`SymbolHandle`] rather than a bare
    /// `Spur` because this value reaches two places that must not depend on a
    /// handle's numeric value: specialization name mangling, which spells a
    /// link-time symbol, and the AIR comptime-argument encoder, which writes
    /// one word per argument (ADR-0076).
    Function(SymbolHandle),
    /// String value - stores the interned literal content (RUE-957).
    ///
    /// A string constant's use sites materialize it exactly like an inline
    /// string literal: the content joins the function's local string table
    /// and lowers to `.rodata`-backed `str` (`{ptr, len}`). String constants
    /// are not usable as comptime arguments (no `comptime s: str` parameters
    /// exist), so specialization serialization rejects them.
    String(SymbolHandle),
    /// Exact canonical decimal value, interned as `<significand>e<exponent>`.
    Float(SymbolHandle),
    /// Unit value - the value of `()`.
    Unit,
}

impl ConstValue {
    /// Try to extract an integer value that fits in an `i64`.
    ///
    /// Returns `None` for non-integer values and for integers outside the
    /// `i64` range (e.g. u64 values above `i64::MAX`). Use [`as_int_value`]
    /// when the full `i128` backing value is needed.
    ///
    /// [`as_int_value`]: ConstValue::as_int_value
    pub fn as_integer(self) -> Option<i64> {
        match self {
            ConstValue::Integer(n) => i64::try_from(n).ok(),
            _ => None,
        }
    }

    /// Try to extract the full backing integer value.
    pub fn as_int_value(self) -> Option<i128> {
        match self {
            ConstValue::Integer(n) => Some(n),
            _ => None,
        }
    }

    /// Try to extract a boolean value.
    pub fn as_bool(self) -> Option<bool> {
        match self {
            ConstValue::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// Try to extract a type value.
    pub fn as_type(self) -> Option<Type> {
        match self {
            ConstValue::Type(ty) => Some(ty),
            _ => None,
        }
    }

    /// Try to extract a function reference.
    pub fn as_function(self) -> Option<SymbolHandle> {
        match self {
            ConstValue::Function(name) => Some(name),
            _ => None,
        }
    }

    /// Check if this is a unit value.
    pub fn is_unit(self) -> bool {
        matches!(self, ConstValue::Unit)
    }

    /// Get the type of this constant value.
    pub fn get_type(&self) -> Type {
        match self {
            ConstValue::Integer(_) => Type::I64, // Default to i64 for comptime integers
            ConstValue::Bool(_) => Type::BOOL,
            ConstValue::Type(_) => Type::COMPTIME_TYPE,
            ConstValue::Function(_) => Type::COMPTIME_TYPE,
            // The `str` struct type lives in the type pool, which this
            // pool-free helper cannot reach. Both callers type-check comptime
            // arguments, which string constants never reach (the comptime
            // engine treats them as non-evaluable), so this arm only has to
            // avoid colliding with a real comptime parameter type.
            ConstValue::String(_) => Type::ERROR,
            ConstValue::Float(_) => Type::COMPTIME_FLOAT,
            ConstValue::Unit => Type::UNIT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;

    fn test_param(name: Spur, abi_slot: u32) -> ParamInfo {
        ParamInfo {
            name,
            abi_slot,
            ty: Type::I32,
            mode: RirParamMode::Normal,
            is_comptime: false,
            is_mut: false,
        }
    }

    #[test]
    fn small_parameter_signatures_use_flat_lookup() {
        let interner = ThreadedRodeo::new();
        let first = interner.get_or_intern("first");
        let second = interner.get_or_intern("second");
        let missing = interner.get_or_intern("missing");
        let params = [test_param(first, 0), test_param(second, 1)];
        let index = ParamIndex::new(&params);

        assert!(index.by_name.is_none());
        assert_eq!(
            index.get(&params, second).map(|param| param.abi_slot),
            Some(1)
        );
        assert!(index.get(&params, missing).is_none());
    }

    #[test]
    fn large_parameter_signatures_use_indexed_lookup() {
        let interner = ThreadedRodeo::new();
        let params = (0..=ParamIndex::LINEAR_LOOKUP_LIMIT)
            .map(|slot| {
                let name = interner.get_or_intern(format!("param_{slot}"));
                test_param(name, slot as u32)
            })
            .collect::<Vec<_>>();
        let index = ParamIndex::new(&params);

        assert!(index.by_name.is_some());
        for param in &params {
            assert_eq!(
                index.get(&params, param.name).map(|found| found.abi_slot),
                Some(param.abi_slot)
            );
        }
    }

    #[test]
    fn checked_index_scope_identity_clones_changes_and_restores_exactly() {
        let mut state = CheckedConstIndexScopeState::new();
        let baseline = state.key();
        state.push();
        state.changed();
        let bind = state.key();
        let fork = state.clone();
        assert_eq!(
            bind,
            fork.key(),
            "a context fork preserves exact scope state"
        );

        state.changed();
        let shadow = state.key();
        assert_ne!(baseline, bind, "a binding allocates a new state identity");
        assert_ne!(bind, shadow, "a shadow allocates another state identity");

        state.pop();
        let restored = state.key();
        assert_eq!(
            baseline, restored,
            "popping a scope restores the saved identity rather than hashing contents"
        );
    }

    // =========================================================================
    // ConstValue tests
    // =========================================================================

    #[test]
    fn const_value_as_integer() {
        let cv = ConstValue::Integer(42);
        assert_eq!(cv.as_integer(), Some(42));
        assert_eq!(cv.as_bool(), None);
    }

    #[test]
    fn const_value_as_bool() {
        let cv = ConstValue::Bool(true);
        assert_eq!(cv.as_bool(), Some(true));
        assert_eq!(cv.as_integer(), None);

        let cv2 = ConstValue::Bool(false);
        assert_eq!(cv2.as_bool(), Some(false));
    }

    #[test]
    fn const_value_negative_integer() {
        let cv = ConstValue::Integer(-100);
        assert_eq!(cv.as_integer(), Some(-100));
    }

    #[test]
    fn const_value_equality() {
        assert_eq!(ConstValue::Integer(42), ConstValue::Integer(42));
        assert_ne!(ConstValue::Integer(42), ConstValue::Integer(43));
        assert_eq!(ConstValue::Bool(true), ConstValue::Bool(true));
        assert_ne!(ConstValue::Bool(true), ConstValue::Bool(false));
        assert_ne!(ConstValue::Integer(1), ConstValue::Bool(true));
    }

    #[test]
    fn const_value_as_type() {
        let cv = ConstValue::Type(Type::I32);
        assert_eq!(cv.as_type(), Some(Type::I32));
        assert_eq!(cv.as_integer(), None);
        assert_eq!(cv.as_bool(), None);

        let cv2 = ConstValue::Type(Type::BOOL);
        assert_eq!(cv2.as_type(), Some(Type::BOOL));
    }

    #[test]
    fn const_value_unit() {
        let cv = ConstValue::Unit;
        assert!(cv.is_unit());
        assert_eq!(cv.as_integer(), None);
        assert_eq!(cv.as_bool(), None);
        assert_eq!(cv.as_type(), None);
    }

    #[test]
    fn const_value_get_type() {
        assert_eq!(ConstValue::Integer(42).get_type(), Type::I64);
        assert_eq!(ConstValue::Bool(true).get_type(), Type::BOOL);
        assert_eq!(ConstValue::Type(Type::I32).get_type(), Type::COMPTIME_TYPE);
        assert_eq!(ConstValue::Unit.get_type(), Type::UNIT);
    }

    #[test]
    fn const_value_type_equality() {
        assert_eq!(ConstValue::Type(Type::I32), ConstValue::Type(Type::I32));
        assert_ne!(ConstValue::Type(Type::I32), ConstValue::Type(Type::I64));
        assert_ne!(ConstValue::Type(Type::I32), ConstValue::Integer(32));
    }

    // =========================================================================
    // AnalysisResult tests
    // =========================================================================

    #[test]
    fn analysis_result_new() {
        let air_ref = AirRef::from_raw(5);
        let ty = Type::I32;

        let result = AnalysisResult::new(air_ref, ty);

        assert_eq!(result.air_ref.as_u32(), 5);
        assert_eq!(result.ty, Type::I32);
    }
}
