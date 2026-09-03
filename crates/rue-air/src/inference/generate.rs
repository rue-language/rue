//! Constraint generation for Hindley-Milner type inference.
//!
//! This module provides the constraint generation phase (Phase 1 of HM inference):
//! - [`ConstraintContext`] - Scoped variable tracking during generation
//! - [`ExprInfo`] - Result of constraint generation for an expression
//! - [`ConstraintGenerator`] - Walks RIR and generates type constraints
//! - Function/method signature types for type checking

use super::constraint::Constraint;
use super::types::{InferType, TypeVarAllocator, TypeVarId};
use crate::Type;
use crate::intern_pool::TypeInternPool;
use crate::scope::ScopedContext;
use crate::sema::{ComptimeSelection, ConstValue};
#[cfg(test)]
use crate::types::ArrayLen;
use crate::types::{ModuleId, StructId, TypeKind};
use lasso::{Key, Spur, ThreadedRodeo};
use rue_rir::{InstData, InstRef, RepeatCount, Rir, RirTypeSyntaxNode, RirTypeSyntaxRef};
use rue_span::{FileId, Span};

use ahash::AHashMap;
#[cfg(test)]
use ahash::RandomState;
#[cfg(test)]
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// Information about a local variable during constraint generation.
#[derive(Debug, Clone)]
pub struct LocalVarInfo {
    /// The inferred type of this variable.
    pub ty: InferType,
    /// Whether the variable is mutable.
    pub is_mut: bool,
    /// Span of the variable declaration.
    pub span: Span,
}

/// Information about a function parameter during constraint generation.
#[derive(Debug, Clone)]
pub struct ParamVarInfo {
    /// The type of this parameter, as InferType for uniform handling.
    pub ty: InferType,
    /// Whether whole assignment may target this parameter. Only `inout`
    /// parameters are mutable; constraining an assignment to a normal,
    /// borrow, or comptime parameter would mask the primary mutability error.
    pub is_inout: bool,
}

/// A typed lexical overlay captured at a staged selector checkpoint.
///
/// The root is persistent: inserting a shadowing binding copies only the
/// 32-bit key path, while cloning a checkpoint remains an `Arc` clone. This
/// avoids making every name lookup walk the entire lexical prefix.
#[derive(Debug, Clone)]
pub struct FrontierParamOverlay {
    root: Arc<FrontierParamTrieNode>,
}

#[derive(Debug, Clone)]
struct FrontierParamTrieNode {
    value: Option<ParamVarInfo>,
    children: [Option<Arc<FrontierParamTrieNode>>; 2],
}

impl FrontierParamTrieNode {
    fn empty() -> Arc<Self> {
        Arc::new(Self {
            value: None,
            children: [None, None],
        })
    }

    fn insert(node: &Arc<Self>, key: u32, bit: u32, value: ParamVarInfo) -> Arc<Self> {
        if bit == 32 {
            return Arc::new(Self {
                value: Some(value),
                children: node.children.clone(),
            });
        }
        let child_index = ((key >> (31 - bit)) & 1) as usize;
        let child = node.children[child_index]
            .clone()
            .unwrap_or_else(Self::empty);
        let updated = Self::insert(&child, key, bit + 1, value);
        let mut children = node.children.clone();
        children[child_index] = Some(updated);
        Arc::new(Self {
            value: node.value.clone(),
            children,
        })
    }

    fn lookup(node: &Arc<Self>, key: u32, bit: u32) -> Option<&ParamVarInfo> {
        if bit == 32 {
            return node.value.as_ref();
        }
        let child_index = ((key >> (31 - bit)) & 1) as usize;
        node.children[child_index]
            .as_ref()
            .and_then(|child| Self::lookup(child, key, bit + 1))
    }
}

impl FrontierParamOverlay {
    pub fn insert(parent: Option<&Arc<Self>>, name: Spur, info: ParamVarInfo) -> Arc<Self> {
        let root = parent
            .map(|overlay| overlay.root.clone())
            .unwrap_or_else(FrontierParamTrieNode::empty);
        Arc::new(Self {
            root: FrontierParamTrieNode::insert(&root, name.into_usize() as u32, 0, info),
        })
    }

    fn lookup(&self, name: Spur) -> Option<&ParamVarInfo> {
        FrontierParamTrieNode::lookup(&self.root, name.into_usize() as u32, 0)
    }
}

/// Information about a function during constraint generation.
///
/// Uses `InferType` rather than `Type` so that array types are represented
/// structurally (as `InferType::Array { element, length }`) rather than by
/// opaque IDs. This allows uniform handling during inference.
#[derive(Debug, Clone)]
pub struct FunctionSig {
    /// Parameter types (in order), as InferTypes for uniform handling.
    pub param_types: Vec<InferType>,
    /// Return type, as InferType for uniform handling.
    pub return_type: InferType,
    /// Whether this function requires specialization (has any comptime
    /// parameters — type parameters like `comptime T: type` or value
    /// parameters like `comptime n: i32`, RUE-166).
    /// Generic calls substitute the comptime type arguments into the parameter
    /// types before constraining arguments; type parameters that can't be
    /// resolved during constraint generation are checked in sema instead.
    /// Comptime value parameters have concrete declared types, so their
    /// arguments are constrained normally.
    pub is_generic: bool,
    /// Parameter modes (Normal, Inout, Borrow, Comptime).
    pub param_modes: Vec<rue_rir::RirParamMode>,
    /// Which parameters are comptime (declared with `comptime` keyword).
    /// This is separate from param_modes because `comptime T: type` sets
    /// is_comptime=true but mode=Normal.
    pub param_comptime: Vec<bool>,
    /// Which parameters are specifically declared `comptime ...: type`.
    /// A deferred comptime value parameter such as `comptime value: T` also
    /// has a `COMPTIME_TYPE` semantic placeholder, so that placeholder cannot
    /// distinguish the two source-level kinds.
    pub param_comptime_type: Vec<bool>,
    /// Parameter names, needed for type substitution in generic returns.
    pub param_names: Vec<lasso::Spur>,
    /// Exact structured parameter syntax used only when the reduced type is a
    /// specialization placeholder. The arena is declaration-owned and shared;
    /// inference never reconstructs or parses its spelling.
    pub(crate) param_type_syntax: Vec<Option<crate::sema::StructuredTypeSyntax>>,
    /// Exact structured return syntax for the same substitution path.
    pub(crate) return_type_syntax: Option<crate::sema::StructuredTypeSyntax>,
}

/// Information about a method during constraint generation.
///
/// Used for method calls (receiver.method()) and associated function calls (Type::function()).
#[derive(Debug, Clone)]
pub struct MethodSig {
    /// The struct type this method belongs to (as concrete Type::Struct)
    pub struct_type: Type,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// Parameter types (excluding self), as InferTypes for uniform handling.
    pub param_types: Vec<InferType>,
    /// Source parameter modes (excluding self), in the same order as
    /// `param_types`. Keeping modes in the inference signature prevents method
    /// calls from consulting a type-only shadow of the callee contract
    /// (RUE-634).
    pub param_modes: Vec<rue_rir::RirParamMode>,
    /// Return type, as InferType for uniform handling.
    pub return_type: InferType,
}

/// Demand-population seam for the inference context (RUE-1091 slice r5b).
///
/// Constraint generation consults the fourteen declaration-universe families
/// purely by key. Rather than eagerly project the whole universe into owned
/// `AHashMap`s before any body is analyzed (the O(universe)-per-body term
/// RUE-1083 removes), the production path implements this trait over the frozen
/// declaration state and materializes each consulted signature/type on first
/// lookup. Every method answers exactly the same value the eager projection
/// would have held for that key; the non-`Copy` signature families return owned
/// `Rc` handles because a demand cache cannot lend a `'a` borrow across the
/// generator's `&mut self` recursion.
///
/// Unit tests keep constructing the generator from literal maps (the eager
/// path, `lazy == None`); only the production body pipeline installs a lazy
/// provider.
pub(crate) trait LazyInferenceFacts {
    fn func_sig(&self, name: Spur) -> Option<Rc<FunctionSig>>;
    fn method_sig(&self, key: (StructId, Spur)) -> Option<Rc<MethodSig>>;
    fn builtin_struct_type(&self, name: Spur) -> Option<Type>;
    fn struct_type_by_file(&self, key: (FileId, Spur)) -> Option<Type>;
    fn builtin_enum_type(&self, name: Spur) -> Option<Type>;
    fn enum_type_by_file(&self, key: (FileId, Spur)) -> Option<Type>;
    fn nominal_type_accessible(&self, accessing_file: FileId, ty: Type) -> bool;
    fn const_type(&self, key: (FileId, Spur)) -> Option<Type>;
    fn const_type_alias(&self, key: (FileId, Spur)) -> Option<Type>;
    fn const_value(&self, key: (FileId, Spur)) -> Option<i128>;
    fn const_function_alias(&self, key: (FileId, Spur)) -> Option<Spur>;
    fn module_binding_type(&self, key: (FileId, Spur)) -> Option<Type>;
    fn module_file_id(&self, module: ModuleId) -> Option<FileId>;
    fn function_by_file(&self, key: (FileId, Spur)) -> Option<Spur>;
}

/// Context for constraint generation within a single function.
pub struct ConstraintContext<'a> {
    /// Local variables in scope.
    pub locals: AHashMap<Spur, LocalVarInfo>,
    /// Function parameters.
    pub params: &'a AHashMap<Spur, ParamVarInfo>,
    /// Bindings introduced after the function parameter checkpoint.
    pub frontier_overlay: Option<Arc<FrontierParamOverlay>>,
    /// Return type of the current function.
    pub return_type: Type,
    /// How many loops we're nested inside (for break/continue validation).
    pub loop_depth: u32,
    /// One entry per enclosing loop (innermost last). The syntactic bit gives
    /// an infinite loop its spec-mandated type even when its break is in dead
    /// code; the reachable bit determines whether that break can actually
    /// leave the loop and continue an enclosing expression (RUE-1615).
    pub loop_break_stack: Vec<LoopBreakFact>,
    checked_depth: u32,
    /// Scope stack for efficient scope management.
    scope_stack: Vec<Vec<(Spur, Option<LocalVarInfo>)>>,
}

/// Break facts collected during constraint generation. Type inference still
/// analyzes unreachable suffixes for diagnostics, so reachability must be
/// restored separately from the syntactic classification (spec 4.8:21).
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopBreakFact {
    pub syntactic: bool,
    pub reachable: bool,
}

fn restore_reachable_break_facts(
    loop_break_stack: &mut [LoopBreakFact],
    reachable_facts: &[LoopBreakFact],
) {
    assert_eq!(reachable_facts.len(), loop_break_stack.len());
    for (reachable, analyzed) in loop_break_stack.iter_mut().zip(reachable_facts) {
        reachable.syntactic |= analyzed.syntactic;
        reachable.reachable = analyzed.reachable;
    }
}

impl<'a> ConstraintContext<'a> {
    /// Create a new context for a function.
    pub fn new(params: &'a AHashMap<Spur, ParamVarInfo>, return_type: Type) -> Self {
        Self {
            locals: AHashMap::new(),
            params,
            frontier_overlay: None,
            return_type,
            loop_depth: 0,
            loop_break_stack: Vec::new(),
            checked_depth: 0,
            scope_stack: Vec::new(),
        }
    }

    pub fn with_frontier_overlay(mut self, overlay: Option<Arc<FrontierParamOverlay>>) -> Self {
        self.frontier_overlay = overlay;
        self
    }

    pub fn lookup_param(&self, name: Spur) -> Option<&ParamVarInfo> {
        if let Some(overlay) = self.frontier_overlay.as_deref()
            && let Some(info) = overlay.lookup(name)
        {
            return Some(info);
        }
        self.params.get(&name)
    }

    pub fn contains_param(&self, name: Spur) -> bool {
        self.lookup_param(name).is_some()
    }
}

impl ScopedContext for ConstraintContext<'_> {
    type VarInfo = LocalVarInfo;

    fn locals_mut(&mut self) -> &mut AHashMap<Spur, Self::VarInfo> {
        &mut self.locals
    }

    fn scope_stack_mut(&mut self) -> &mut Vec<Vec<(Spur, Option<Self::VarInfo>)>> {
        &mut self.scope_stack
    }
}

/// Result of constraint generation for an expression.
#[derive(Debug, Clone)]
pub struct ExprInfo {
    /// The inferred type of this expression.
    pub ty: InferType,
    /// The span of this expression (for error reporting).
    pub span: Span,
    /// Whether evaluation reaches its enclosing expression normally.
    pub continues: bool,
}

impl ExprInfo {
    /// Create a new expression info.
    pub fn new(ty: InferType, span: Span) -> Self {
        Self {
            ty,
            span,
            continues: true,
        }
    }

    pub fn with_continues(ty: InferType, span: Span, continues: bool) -> Self {
        Self {
            ty,
            span,
            continues,
        }
    }

    pub fn diverged(ty: InferType, span: Span) -> Self {
        Self::with_continues(ty, span, false)
    }
}

/// Constraint generator that walks RIR and generates type constraints.
///
/// This is Phase 1 of HM inference: constraint generation. The constraints
/// are later solved by the `Unifier` to determine concrete types.
pub struct ConstraintGenerator<'a> {
    /// The RIR being analyzed.
    rir: &'a Rir,
    /// String interner for resolving symbols.
    interner: &'a ThreadedRodeo,
    /// Type variable allocator.
    type_vars: TypeVarAllocator,
    /// Collected constraints.
    constraints: Vec<Constraint>,
    /// Mapping from RIR instruction to its inferred type.
    ///
    /// Expression types are keyed by instruction reference. Consumers impose
    /// instruction order before any operation whose result can observe type
    /// pool allocation, so this remains an unordered fast lookup map.
    expr_types: ahash::AHashMap<InstRef, InferType>,
    /// Whether each generated expression reaches its next evaluation point.
    expr_continues: AHashMap<InstRef, bool>,
    /// Function signatures (for call type checking). `None` when the generator
    /// is driven by a lazy provider (`lazy`), which materializes signatures on
    /// demand; unit tests still supply an eager map.
    functions: Option<&'a AHashMap<Spur, FunctionSig>>,
    /// Built-in struct types, which have no defining source file. `None` under
    /// a lazy provider (see `functions`).
    builtin_structs: Option<&'a AHashMap<Spur, Type>>,
    /// Module-local struct types ((defining file, source name) -> Type::new_struct(id)).
    structs_by_file_name: Option<&'a AHashMap<(FileId, Spur), Type>>,
    /// Built-in enum types, which have no defining source file. `None` under a
    /// lazy provider (see `functions`).
    builtin_enums: Option<&'a AHashMap<Spur, Type>>,
    /// Module-local enum types ((defining file, source name) -> Type::new_enum(id)).
    enums_by_file_name: Option<&'a AHashMap<(FileId, Spur), Type>>,
    /// Method signatures: (struct_id, method_name) -> MethodSig. `None` under a
    /// lazy provider (see `functions`).
    methods: Option<&'a AHashMap<(StructId, Spur), MethodSig>>,
    /// Demand-population provider (RUE-1091 slice r5b). When present, the
    /// fourteen declaration-universe families are materialized on first consult
    /// through this seam instead of read from eager maps. `None` in unit tests,
    /// which construct the generator from literal maps.
    lazy: Option<&'a dyn LazyInferenceFacts>,
    /// Type variables allocated for integer literals.
    /// These start as unbound and need to be defaulted to i32 if unconstrained.
    int_literal_vars: Vec<TypeVarId>,
    /// Variables rooted at a `comptime_float` literal. They accept only f32/f64
    /// context and default to f64 after whole-body unification.
    float_literal_vars: Vec<TypeVarId>,
    /// Type variables allocated for string literals. Unlike integer literals,
    /// these default to the canonical core `str` type. Context may still bind
    /// a literal to the trusted standard-library `StrBuf` language item.
    string_literal_vars: Vec<TypeVarId>,
    /// Concrete default for an otherwise-unconstrained string literal.
    string_literal_default: Type,
    /// Fixed-string nominal identities used while generating constraints.
    ///
    /// These are carried separately from expression types because a provider
    /// can materialize a fixed-string enum payload only while a construction
    /// is being constrained. The identity must reach unification even when
    /// the declaration is not present in the body's generated-name map.
    fixed_string_types: Vec<Type>,
    /// Trusted standard-library growable string identity, when imported.
    strbuf_type: Option<Type>,
    /// Type substitutions for Self and type parameters (used in method bodies).
    /// Maps type names (like "Self") to their concrete types.
    type_subst: Option<&'a AHashMap<Spur, Type>>,
    /// File-level constant types (name -> declared type), resolved during
    /// declaration gathering. Consulted by `VarRef` after locals and params so
    /// a const reference infers to its declared type instead of `<error>`
    /// (RUE-142). `None` only in unit tests; production passes the map via
    /// [`Self::with_const_types`].
    const_types: Option<&'a AHashMap<(FileId, Spur), Type>>,
    /// File-level type aliases (`const T = SomeType(...)`) resolved during
    /// declaration gathering. Consulted in type positions.
    const_type_aliases: Option<&'a AHashMap<(FileId, Spur), Type>>,
    /// Module-binding types (`const utils = @import(...)`): (declaring file,
    /// name) -> module type. Per-file scoped (RUE-113), so `VarRef` consults
    /// this with the reference's own `span.file_id` before `const_types`.
    /// `None` only in unit tests; production passes the map via
    /// [`Self::with_module_binding_types`].
    module_binding_types: Option<&'a AHashMap<(FileId, Spur), Type>>,
    /// Source-level function lookup: (defining file, source name) -> internal
    /// function key. Same-named functions across files get module-qualified
    /// internal keys in `functions`, so module-member calls resolve through
    /// this map first — the bare source name misses for them (RUE-576).
    /// `None` only in unit tests; production passes the map via
    /// [`Self::with_functions_by_file_name`].
    functions_by_file_name: Option<&'a AHashMap<(FileId, Spur), Spur>>,
    /// Module registry file identity for inference-time `module.Type` and
    /// `module.Enum` lookup.
    /// `None` only in unit tests; production passes the map via
    /// [`Self::with_module_file_ids`].
    module_file_ids: Option<&'a AHashMap<crate::types::ModuleId, FileId>>,
    /// Compile-time type aliases bound by `let` in the current function body
    /// (`let P = F();` where `F` returns `type`), pre-resolved by sema before
    /// constraint generation and keyed by each binding's own Alloc
    /// instruction. When the walk reaches a binding site in this map, the
    /// alias is brought into scope in [`Self::comptime_alias_types`] and
    /// unwound with its enclosing block (RUE-530). `None` only in unit tests;
    /// production passes the map via [`Self::with_comptime_local_bindings`].
    comptime_local_bindings: Option<&'a AHashMap<InstRef, Type>>,
    /// The comptime type aliases currently in scope (name → aliased type),
    /// maintained live during the walk from [`Self::comptime_local_bindings`]
    /// via [`Self::enter_scope`]/[`Self::exit_scope`]. Consulted after
    /// `type_subst` when resolving struct-literal type names and `let`
    /// annotations, so anonymous-struct aliases route through the same
    /// concrete paths as named structs (RUE-170), and lexically scoped like
    /// sema's `comptime_type_vars` so sibling-branch aliases don't collide
    /// and an alias doesn't outlive its block (RUE-530).
    comptime_alias_types: AHashMap<Spur, Type>,
    /// Per-scope undo frames for `comptime_alias_types`, parallel to the
    /// `ConstraintContext` scope stack (both are pushed/popped only by
    /// [`Self::enter_scope`]/[`Self::exit_scope`]): each frame records the
    /// shadowed binding (or absence) for every alias bound — or hidden by a
    /// same-named runtime `let` — in that scope, restored in reverse on exit
    /// (the RUE-522 pattern).
    alias_scope_stack: Vec<Vec<(Spur, Option<Type>)>>,
    /// Inline type-constructor heads (`F(args).Variant(..)`, `F(args) { .. }`;
    /// RUE-596, spec 4.14:23) pre-reduced by sema to their
    /// concrete struct/enum types, keyed by the head's own `InstRef` — the
    /// per-instruction analogue of `comptime_local_types` for heads that have
    /// no `let`-bound name. Consulted so construction arguments get their
    /// declared payload/parameter constraints; without it an integer payload
    /// literal defaulted to `i32` and could not satisfy a wider declared type
    /// (RUE-599). `None` only in unit tests; production passes the map via
    /// [`Self::with_inline_ctor_head_types`].
    inline_ctor_head_types: Option<&'a AHashMap<InstRef, Type>>,
    /// Method signatures registered after the shared `InferenceContext` was
    /// built: anonymous-struct methods are registered lazily during comptime
    /// evaluation, so they're absent from `methods`. Consulted when a method
    /// key misses `methods`, so a call on an anonymous-struct receiver yields
    /// its declared return type instead of `<error>` (RUE-164). `None` only
    /// in unit tests; production passes the map via
    /// [`Self::with_extra_method_sigs`].
    extra_method_sigs: Option<&'a AHashMap<(StructId, Spur), MethodSig>>,
    /// File-level integer constant values (name -> value), so an array length
    /// naming a `const` (`[i32; K]`) resolves to a concrete length during
    /// constraint generation (RUE-16). `None` only in unit tests; production
    /// passes the map via [`Self::with_const_values`].
    const_values: Option<&'a AHashMap<(FileId, Spur), i128>>,
    /// Function-valued constants: alias name -> callee function name. These
    /// let constraint generation type `alias(...)` as a direct call.
    const_function_aliases: Option<&'a AHashMap<(FileId, Spur), Spur>>,
    /// Comptime *value* parameters known for the specialization currently being
    /// analyzed (`comptime n: i32` → `n = 0` for the call `f(0)`). Lets a
    /// `match` on a comptime-known scrutinee prune to its selected arm during
    /// constraint generation, so only that arm participates in inference —
    /// mirroring sema's arm selection (spec 4.14:19). Without this, inference
    /// cross-constrains every arm and rejects a valid program whose statically
    /// unselected arm has a different type (RUE-268). `None` for ordinary
    /// (non-specialized) functions, where every match is treated as runtime.
    comptime_values: Option<&'a AHashMap<Spur, ConstValue>>,
    /// Canonical selector facts produced by the staged inference probe.
    comptime_selections: Option<&'a AHashMap<InstRef, ComptimeSelection>>,
    /// During the probe, selector bodies are visited only for diagnostics and
    /// their result joins are deferred until a canonical selection is known.
    staged_comptime_selectors: bool,
    /// A frontier pass advances exactly one source-graph segment.  Even when
    /// a selector fact is already known, stop at that selector so its selected
    /// body is enqueued as a separate checkpoint rather than regenerating the
    /// descendant suffix in every ancestor pass.
    comptime_frontier_mode: bool,
    /// Canonically evaluated computed comptime arguments keyed by source node.
    comptime_argument_values: Option<&'a AHashMap<InstRef, ConstValue>>,
    /// Optional query-owned cancellation probe. It is checked at every
    /// generated instruction so a canceled staged frontier cannot continue
    /// producing constraints or publish partial facts.
    cancel_check: Option<Box<dyn Fn() -> bool + 'a>>,
    /// Test/host observation invoked immediately before each sibling operand
    /// attempt. It is deliberately separate from cancellation visits so a
    /// canceled tail cannot hide continued loop iteration.
    sibling_attempt_hook: Option<Box<dyn Fn() + 'a>>,
    canceled: bool,
    /// Type intern pool for creating pointer and array types during constraint generation.
    type_pool: &'a TypeInternPool,
}

/// Return expression types in canonical RIR order. Lookup storage is
/// intentionally unordered, but array precreation must allocate composite
/// types in instruction order so pool indices do not depend on hash buckets.
pub(crate) fn expr_types_in_rir_order(
    expr_types: &AHashMap<InstRef, InferType>,
) -> Vec<(&InstRef, &InferType)> {
    let mut ordered = expr_types.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|(inst_ref, _)| inst_ref.as_u32());
    ordered
}

#[cfg(test)]
#[derive(Clone)]
struct ExprTypesTestLayout {
    state: RandomState,
    reverse_insertion: bool,
}

#[cfg(test)]
thread_local! {
    static EXPR_TYPES_TEST_LAYOUT: RefCell<Option<ExprTypesTestLayout>> = const { RefCell::new(None) };
}

/// Run a production body-analysis transaction with a controlled expression-map
/// layout. This is test-only instrumentation for proving that semantic output
/// does not depend on AHash's seed or insertion order.
#[cfg(test)]
pub(crate) fn with_expr_types_test_layout<R>(
    seeds: [u64; 4],
    reverse_insertion: bool,
    action: impl FnOnce() -> R,
) -> R {
    EXPR_TYPES_TEST_LAYOUT.with(|configured| {
        let previous = configured.replace(Some(ExprTypesTestLayout {
            state: RandomState::with_seeds(seeds[0], seeds[1], seeds[2], seeds[3]),
            reverse_insertion,
        }));
        let result = action();
        configured.replace(previous);
        result
    })
}

fn new_expr_types_map() -> AHashMap<InstRef, InferType> {
    #[cfg(test)]
    if let Some(layout) = EXPR_TYPES_TEST_LAYOUT.with(|configured| configured.borrow().clone()) {
        return AHashMap::with_hasher(layout.state);
    }
    AHashMap::new()
}

fn finalize_expr_types_map(
    expr_types: AHashMap<InstRef, InferType>,
) -> AHashMap<InstRef, InferType> {
    #[cfg(test)]
    if let Some(layout) = EXPR_TYPES_TEST_LAYOUT.with(|configured| configured.borrow().clone()) {
        let mut entries = expr_types.into_iter().collect::<Vec<_>>();
        entries.sort_unstable_by_key(|(inst_ref, _)| inst_ref.as_u32());
        if layout.reverse_insertion {
            entries.reverse();
        }
        let mut rebuilt = AHashMap::with_hasher(layout.state);
        rebuilt.extend(entries);
        return rebuilt;
    }
    expr_types
}

impl<'a> ConstraintGenerator<'a> {
    #[inline]
    fn note_sibling_attempt(&self) {
        if let Some(hook) = self.sibling_attempt_hook.as_ref() {
            hook();
        }
    }

    /// Generate one operand in a left-to-right sequence. Later operands are
    /// still visited for diagnostics after an earlier operand diverges, but
    /// their break facts cannot reach the enclosing loop (RUE-1615).
    fn generate_sequenced_operand(
        &mut self,
        inst_ref: InstRef,
        ctx: &mut ConstraintContext,
        reachable: bool,
    ) -> ExprInfo {
        self.note_sibling_attempt();
        if self.canceled {
            return ExprInfo::diverged(
                InferType::Concrete(Type::ERROR),
                self.rir.get(inst_ref).span,
            );
        }
        let reachable_facts = ctx.loop_break_stack.clone();
        let info = self.generate(inst_ref, ctx);
        if !reachable {
            restore_reachable_break_facts(&mut ctx.loop_break_stack, &reachable_facts);
        }
        info
    }

    /// Create a new constraint generator.
    pub fn new(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        functions: &'a AHashMap<Spur, FunctionSig>,
        builtin_structs: &'a AHashMap<Spur, Type>,
        builtin_enums: &'a AHashMap<Spur, Type>,
        methods: &'a AHashMap<(StructId, Spur), MethodSig>,
        type_pool: &'a TypeInternPool,
    ) -> Self {
        Self::with_type_subst(
            rir,
            interner,
            functions,
            builtin_structs,
            builtin_enums,
            methods,
            type_pool,
            None,
        )
    }

    /// Create a new constraint generator with type substitutions.
    ///
    /// The `type_subst` map provides type substitutions for names like "Self"
    /// that should be resolved to concrete types during constraint generation.
    /// This is used for method bodies where `Self { ... }` struct literals
    /// need to know the concrete struct type.
    pub fn with_type_subst(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        functions: &'a AHashMap<Spur, FunctionSig>,
        builtin_structs: &'a AHashMap<Spur, Type>,
        builtin_enums: &'a AHashMap<Spur, Type>,
        methods: &'a AHashMap<(StructId, Spur), MethodSig>,
        type_pool: &'a TypeInternPool,
        type_subst: Option<&'a AHashMap<Spur, Type>>,
    ) -> Self {
        let strbuf_type = type_pool.lang_item_type(crate::LangItem::StrBuf);
        let string_literal_default = Type::ERROR;
        let rir_capacity = rir.len();
        Self {
            rir,
            interner,
            type_vars: TypeVarAllocator::new(),
            constraints: Vec::with_capacity(rir_capacity),
            expr_types: new_expr_types_map(),
            expr_continues: AHashMap::new(),
            functions: Some(functions),
            builtin_structs: Some(builtin_structs),
            structs_by_file_name: None,
            builtin_enums: Some(builtin_enums),
            enums_by_file_name: None,
            methods: Some(methods),
            lazy: None,
            int_literal_vars: Vec::new(),
            float_literal_vars: Vec::new(),
            string_literal_vars: Vec::new(),
            string_literal_default,
            fixed_string_types: Vec::new(),
            strbuf_type,
            type_subst,
            const_types: None,
            const_type_aliases: None,
            module_binding_types: None,
            functions_by_file_name: None,
            module_file_ids: None,
            comptime_local_bindings: None,
            comptime_alias_types: AHashMap::new(),
            alias_scope_stack: Vec::new(),
            inline_ctor_head_types: None,
            extra_method_sigs: None,
            const_values: None,
            const_function_aliases: None,
            comptime_values: None,
            comptime_selections: None,
            staged_comptime_selectors: false,
            comptime_frontier_mode: false,
            comptime_argument_values: None,
            cancel_check: None,
            sibling_attempt_hook: None,
            canceled: false,
            type_pool,
        }
    }

    /// Create a generator driven by a demand-population provider (RUE-1091
    /// slice r5b).
    ///
    /// The eager family maps stay `None`; every keyed consult of the fourteen
    /// declaration-universe families routes through `lazy`, which materializes
    /// only the signatures and types the body actually names. This is the
    /// production body-analysis path — the eager `new`/`with_type_subst`
    /// constructors remain for unit tests that build literal maps.
    pub(crate) fn with_lazy_facts(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        type_pool: &'a TypeInternPool,
        type_subst: Option<&'a AHashMap<Spur, Type>>,
        lazy: &'a dyn LazyInferenceFacts,
    ) -> Self {
        let strbuf_type = type_pool.lang_item_type(crate::LangItem::StrBuf);
        let string_literal_default = Type::ERROR;
        let rir_capacity = rir.len();
        Self {
            rir,
            interner,
            type_vars: TypeVarAllocator::new(),
            constraints: Vec::with_capacity(rir_capacity),
            expr_types: new_expr_types_map(),
            expr_continues: AHashMap::new(),
            functions: None,
            builtin_structs: None,
            structs_by_file_name: None,
            builtin_enums: None,
            enums_by_file_name: None,
            methods: None,
            lazy: Some(lazy),
            int_literal_vars: Vec::new(),
            float_literal_vars: Vec::new(),
            string_literal_vars: Vec::new(),
            string_literal_default,
            fixed_string_types: Vec::new(),
            strbuf_type,
            type_subst,
            const_types: None,
            const_type_aliases: None,
            module_binding_types: None,
            functions_by_file_name: None,
            module_file_ids: None,
            comptime_local_bindings: None,
            comptime_alias_types: AHashMap::new(),
            alias_scope_stack: Vec::new(),
            inline_ctor_head_types: None,
            extra_method_sigs: None,
            const_values: None,
            const_function_aliases: None,
            comptime_values: None,
            comptime_selections: None,
            staged_comptime_selectors: false,
            comptime_frontier_mode: false,
            comptime_argument_values: None,
            cancel_check: None,
            sibling_attempt_hook: None,
            canceled: false,
            type_pool,
        }
    }

    /// Override the default used for unconstrained string literals.
    ///
    /// Semantic analysis supplies the canonical core `str` type.
    pub fn with_string_literal_default(mut self, ty: Type) -> Self {
        self.string_literal_default = ty;
        self
    }

    /// Supply the trusted StrBuf language item when its module is imported.
    pub fn with_strbuf_type(mut self, ty: Option<Type>) -> Self {
        self.strbuf_type = ty;
        self
    }

    /// Is `ty` the synthetic slice struct `[T]` (ADR-0043, RUE-322), the `str`
    /// string type (RUE-324), or a fixed-capacity string `Str(N)` (RUE-326)? A
    /// slice parameter accepts an array argument by coercion (`sum(borrow a)`),
    /// and a `str`/`Str(N)` position accepts a string literal (whose HM type is
    /// `String`) by coercion, so the constraint generator must NOT impose strict
    /// `arg == expected` equality when the expected type is one of these; the
    /// real compatibility check (including the `Str(N)` capacity-fits rule) and
    /// the fat-pointer/`str` materialization happen in semantic analysis.
    fn is_slice_struct_type(&self, ty: InferType) -> bool {
        if let InferType::Concrete(t) = ty
            && let Some(id) = t.as_struct()
        {
            let name: &str = &self.type_pool.struct_def(id).name;
            return crate::types::is_string_view_struct_name(name)
                || crate::types::is_slice_struct_name(name);
        }
        false
    }

    /// Is this method call a view `len` — `s.len()` on a receiver whose type is
    /// the synthetic slice struct `[T]` (7.2:17), the `str` string type
    /// (3.7:45), or a fixed-capacity `Str(N)` (3.7:52)? All three rules give the
    /// result type as `u64`.
    ///
    /// These three share one representation (a `{ptr, len}` view) and one
    /// method route: sema dispatches every method call on them through
    /// `analyze_slice_method`, which answers `len()` by reading the `len` word
    /// as `u64` and rejects everything else as an undefined method. None of
    /// them has a *declared* `len`, so unless its result type is published
    /// here the call types as `ERROR`, which unifies with any annotation —
    /// `let n: i32 = s.len();` then passed inference and reached CFG
    /// verification as a `u64` value in an `i32` slot (RUE-1611 for `[T]`,
    /// RUE-1679 for `str`/`Str(N)`).
    ///
    /// StrBuf is not in this family: it is a source-defined struct whose
    /// declared `len` ordinary method lookup already resolves.
    fn is_view_len_call(&self, receiver: StructId, method: Spur, arg_count: usize) -> bool {
        let name: &str = &self.type_pool.struct_def(receiver).name;
        arg_count == 0
            && self.interner.resolve(&method) == "len"
            && (crate::types::is_slice_struct_name(name)
                || crate::types::is_string_view_struct_name(name))
    }

    /// The string-literal analogue of [`Self::is_view_len_call`]: a zero-arg
    /// `len` on a receiver whose text type has not been fixed yet. Every text
    /// type the literal can settle on reports a `u64` byte length, so the
    /// result is known even though the receiver is not (RUE-1679).
    fn is_string_literal_len_call(&self, method: Spur, arg_count: usize) -> bool {
        arg_count == 0 && self.interner.resolve(&method) == "len"
    }

    /// Whether an already-known operand is one of Rue's packed string types.
    /// String indexing is lowered by semantic analysis to a byte read, so its
    /// result is always `u8`, unlike an array index whose element type comes
    /// from the array itself. String literals are admitted here too: their
    /// contextual type is finalized later, but indexing one has the same
    /// result type regardless of whether it becomes `str` or `StrBuf`.
    fn is_string_indexable_type(&self, ty: &InferType) -> bool {
        if self.is_string_concrete(ty) || self.is_string_literal_candidate(ty) {
            return true;
        }
        let InferType::Concrete(t) = ty else {
            return false;
        };
        let Some(id) = t.as_struct() else {
            return false;
        };
        crate::types::is_string_view_struct_name(&self.type_pool.struct_def(id).name)
    }

    /// The pointee of a pointer operand whose type is *already* concrete at
    /// constraint-generation time — an annotated binding, a parameter, or a
    /// pointer-returning intrinsic whose result was published by this pass.
    /// Returns `None` for anything still standing on a type variable (such as
    /// an unresolved `@raw`, `@ptr_offset`, or context-driven `@int_to_ptr`),
    /// because the unifier has not run yet and this pass must leave the
    /// genuinely unresolved pointee free rather than guess it (RUE-1341).
    fn concrete_pointee_type(&self, ty: &InferType) -> Option<Type> {
        match ty.as_concrete()?.kind() {
            TypeKind::PtrConst(ptr_id) => Some(self.type_pool.ptr_const_def(ptr_id)),
            TypeKind::PtrMut(ptr_id) => Some(self.type_pool.ptr_mut_def(ptr_id)),
            _ => None,
        }
    }

    /// Convert a fully concrete inference type to its interned semantic type.
    /// Array literals remain structural during constraint generation, so they
    /// need the same canonicalization as annotated array types before they can
    /// be used as a raw-pointer pointee. Unresolved elements stay unresolved.
    fn concrete_type(&self, ty: &InferType) -> Option<Type> {
        match ty {
            InferType::Concrete(ty)
                if !ty.is_error()
                    && !ty.is_never()
                    && !ty.is_comptime_type()
                    && !ty.is_module() =>
            {
                Some(*ty)
            }
            InferType::Concrete(_) => None,
            InferType::Array { element, length } => {
                let element = self.concrete_type(element)?;
                Some(Type::new_array(
                    self.type_pool.intern_array_from_type(element, *length),
                ))
            }
            InferType::Var(_) | InferType::IntLiteral => None,
        }
    }

    /// Whether a RIR operand can name local/parameter storage. Sema remains
    /// authoritative for place validity (including resolved field/index
    /// types), but avoiding computed/module/constant operands here prevents a
    /// speculative pointer result from masking its targeted diagnostic.
    fn is_inference_place(&self, inst_ref: InstRef, ctx: &ConstraintContext) -> bool {
        match self.rir.get(inst_ref).data {
            InstData::VarRef { name, .. } => {
                match ctx.locals.get(&name) {
                    Some(local) => {
                        // Comptime aliases and module values are represented
                        // in inference as locals, but sema materializes their
                        // uses as TypeConst rather than addressable runtime
                        // storage. Keep those uses on sema's targeted place
                        // diagnostic. A local shadows a parameter here, so
                        // do not fall through to the parameter predicate.
                        !matches!(
                            local.ty,
                            InferType::Concrete(ty)
                                if ty.is_comptime_type() || ty.is_module()
                        )
                    }
                    None => {
                        // Captured comptime values are materialized by sema
                        // as Const/TypeConst rather than Param AIR. Keep
                        // those names free as well; ordinary parameters (and
                        // their comptime modes when not captured) remain
                        // addressable Param values.
                        !self
                            .comptime_values
                            .is_some_and(|values| values.contains_key(&name))
                            && ctx.contains_param(name)
                    }
                }
            }
            InstData::FieldGet { base, .. } => self.is_inference_place(base, ctx),
            InstData::IndexGet { base, index } => {
                // Only fixed-array indexing names addressable storage. String
                // and slice indexing is a computed runtime read, even when
                // the base and index are concrete; publishing a pointer for
                // it would steal sema's E0485 place diagnostic.
                let base_is_fixed_array = self.expr_types.get(&base).is_some_and(|ty| match ty {
                    InferType::Array { .. } => true,
                    InferType::Concrete(ty) => ty.as_array().is_some(),
                    InferType::Var(_) | InferType::IntLiteral => false,
                });
                // Keep an invalid index owned by sema. An exact pointer
                // result for `@raw(a[true])` could otherwise make a separate
                // pointer annotation fail first with E0206.
                let index_is_integer = self.expr_types.get(&index).is_some_and(|ty| match ty {
                    InferType::Concrete(ty) => ty.is_integer(),
                    InferType::Var(id) => self.int_literal_vars.contains(id),
                    InferType::IntLiteral => true,
                    InferType::Array { .. } => false,
                });
                // Sema diagnoses a direct constant index outside a fixed
                // array as E0902 before it can form a PlaceRead. The
                // inference pass has no authoritative constant evaluator, so
                // only direct literals proven in bounds and runtime local/
                // parameter indices may publish a pointer; folded constants
                // and other computed expressions stay free for sema.
                let index_is_publishable = match self.rir.get(index).data {
                    InstData::IntConst(value) => {
                        index_is_integer
                            && self
                                .expr_types
                                .get(&base)
                                .and_then(|ty| match ty {
                                    InferType::Array { length, .. } => Some(*length),
                                    InferType::Concrete(ty) => ty
                                        .as_array()
                                        .map(|array_id| self.type_pool.array_def(array_id).1),
                                    InferType::Var(_) | InferType::IntLiteral => None,
                                })
                                .is_some_and(|length| value < length)
                    }
                    InstData::VarRef { name, .. } => {
                        index_is_integer
                            && match ctx.locals.get(&name) {
                                Some(local) => !matches!(
                                    local.ty,
                                    InferType::Concrete(ty)
                                        if ty.is_comptime_type() || ty.is_module()
                                ),
                                None => {
                                    !self
                                        .comptime_values
                                        .is_some_and(|values| values.contains_key(&name))
                                        && ctx.contains_param(name)
                                }
                            }
                    }
                    _ => false,
                };
                self.is_inference_place(base, ctx) && base_is_fixed_array && index_is_publishable
            }
            _ => false,
        }
    }

    /// The element type of an indexable operand whose type is *already*
    /// concrete at constraint-generation time: an interned fixed array
    /// `[T; N]`, or the synthetic slice struct `[T]` whose first field is the
    /// `*const T` half of the fat pointer (ADR-0043, RUE-322). An
    /// `InferType::Array` base is handled structurally by the caller; this
    /// covers the interned forms — a slice parameter, an annotated binding, a
    /// call result — that reach the generator as `Concrete`.
    ///
    /// Without it, indexing a concrete slice degraded to a fresh variable, so
    /// an integer literal sharing an operator with the element (`e.i.a * 10`)
    /// had nothing to take its width from and defaulted to i32, while sema
    /// typed the operator from the element's real width. That is exactly the
    /// mixed-width AIR the operator-agreement check in `inst.rs` rejects
    /// (RUE-1636 family; the check found this producer).
    fn concrete_element_type(&self, ty: &InferType) -> Option<Type> {
        let ty = ty.as_concrete()?;
        if let Some(array_id) = ty.as_array() {
            return Some(self.type_pool.array_def(array_id).0);
        }
        let id = ty.as_struct()?;
        // `str`/`Str(N)` share the view representation but index as bytes;
        // `is_string_indexable_type` answers those before this is consulted.
        if !crate::types::is_slice_struct_name(&self.type_pool.struct_def(id).name) {
            return None;
        }
        match self.type_pool.struct_def(id).fields.first()?.ty.kind() {
            TypeKind::PtrConst(ptr_id) => Some(self.type_pool.ptr_const_def(ptr_id)),
            _ => None,
        }
    }

    /// Provide file-level constant types (name -> declared type) for `VarRef`
    /// resolution. See the `const_types` field for details (RUE-142).
    pub fn with_const_types(mut self, const_types: &'a AHashMap<(FileId, Spur), Type>) -> Self {
        self.const_types = Some(const_types);
        self
    }

    /// Provide file-level type aliases for type-position resolution.
    pub fn with_const_type_aliases(
        mut self,
        const_type_aliases: &'a AHashMap<(FileId, Spur), Type>,
    ) -> Self {
        self.const_type_aliases = Some(const_type_aliases);
        self
    }

    /// Provide per-file module-binding types ((file, name) -> module type)
    /// for `VarRef` resolution. See the `module_binding_types` field (RUE-113).
    pub fn with_module_binding_types(
        mut self,
        module_binding_types: &'a AHashMap<(FileId, Spur), Type>,
    ) -> Self {
        self.module_binding_types = Some(module_binding_types);
        self
    }

    /// Provide the (defining file, source name) -> internal function key map
    /// for module-member call resolution (RUE-576).
    pub fn with_functions_by_file_name(
        mut self,
        functions_by_file_name: &'a AHashMap<(FileId, Spur), Spur>,
    ) -> Self {
        self.functions_by_file_name = Some(functions_by_file_name);
        self
    }

    /// Provide module registry file identities for `module.Type` lookup.
    pub fn with_module_file_ids(
        mut self,
        module_file_ids: &'a AHashMap<crate::types::ModuleId, FileId>,
    ) -> Self {
        self.module_file_ids = Some(module_file_ids);
        self
    }

    /// Provide pre-resolved comptime type aliases (binding-site Alloc
    /// `InstRef` -> concrete type) for struct-literal and `let`-annotation
    /// resolution. See the `comptime_local_bindings` field (RUE-170,
    /// RUE-530).
    pub fn with_comptime_local_bindings(
        mut self,
        comptime_local_bindings: &'a AHashMap<InstRef, Type>,
    ) -> Self {
        self.comptime_local_bindings = Some(comptime_local_bindings);
        self
    }

    /// Provide sema's pre-reduced inline type-constructor heads (head
    /// `InstRef` -> concrete type). See the `inline_ctor_head_types` field
    /// (RUE-599).
    pub fn with_inline_ctor_head_types(
        mut self,
        inline_ctor_head_types: &'a AHashMap<InstRef, Type>,
    ) -> Self {
        self.inline_ctor_head_types = Some(inline_ctor_head_types);
        self
    }

    /// Provide late-registered method signatures (anonymous-struct methods)
    /// for method-call resolution. See the `extra_method_sigs` field
    /// (RUE-164).
    pub fn with_extra_method_sigs(
        mut self,
        extra_method_sigs: &'a AHashMap<(StructId, Spur), MethodSig>,
    ) -> Self {
        self.extra_method_sigs = Some(extra_method_sigs);
        self
    }

    /// Provide file-level integer constant values (name -> value) so an array
    /// length naming a `const` resolves during constraint generation. See the
    /// `const_values` field (RUE-16).
    pub fn with_const_values(mut self, const_values: &'a AHashMap<(FileId, Spur), i128>) -> Self {
        self.const_values = Some(const_values);
        self
    }

    /// Provide the comptime value parameters of the specialization being
    /// analyzed so a `match` on a comptime-known scrutinee prunes to its
    /// selected arm during constraint generation. See the `comptime_values`
    /// field (RUE-268). `None` (ordinary functions) leaves every match
    /// runtime.
    pub fn with_comptime_values(
        mut self,
        comptime_values: Option<&'a AHashMap<Spur, ConstValue>>,
    ) -> Self {
        self.comptime_values = comptime_values;
        self
    }

    pub fn with_comptime_selections(
        mut self,
        selections: Option<&'a AHashMap<InstRef, ComptimeSelection>>,
        staged: bool,
    ) -> Self {
        self.comptime_selections = selections;
        self.staged_comptime_selectors = staged;
        self
    }

    /// Limit a staged body pass to the next unknown/known selector boundary.
    /// The canonical selection map remains shared; this flag only controls
    /// constraint generation's traversal frontier.
    pub fn with_comptime_frontier_mode(mut self, enabled: bool) -> Self {
        self.comptime_frontier_mode = enabled;
        self
    }

    pub fn with_comptime_argument_values(
        mut self,
        values: Option<&'a AHashMap<InstRef, ConstValue>>,
    ) -> Self {
        self.comptime_argument_values = values;
        self
    }

    /// Install a cheap query cancellation probe for a staged generation pass.
    /// The callback is deliberately owned so it cannot outlive the generator
    /// or borrow mutable semantic state while recursive generation proceeds.
    pub fn with_cancellation_check(mut self, check: Box<dyn Fn() -> bool + 'a>) -> Self {
        self.cancel_check = Some(check);
        self
    }

    pub fn with_sibling_attempt_hook(mut self, hook: Box<dyn Fn() + 'a>) -> Self {
        self.sibling_attempt_hook = Some(hook);
        self
    }

    pub fn was_canceled(&self) -> bool {
        self.canceled
    }

    /// Resolve an array-length component to a concrete length during constraint
    /// generation. Literal lengths are used directly; a named length resolves
    /// against file-level integer constants (`[i32; K]`). Names that don't
    /// resolve here (e.g. a `comptime` value parameter, only known at
    /// specialization) yield `None`; sema resolves and diagnoses them (RUE-16).
    #[cfg(test)]
    fn resolve_infer_array_length(&self, len: &ArrayLen, file_id: FileId) -> Option<u64> {
        match len {
            ArrayLen::Literal(n) => Some(*n),
            ArrayLen::Named(name) => {
                let sym = self.interner.get(name)?;
                let value = self.scoped_const_value(sym, file_id)?;
                u64::try_from(value).ok()
            }
        }
    }

    /// Resolve a bare integer-const name in array-length position against the
    /// referencing file's scope. A bare name denotes the same-module `const`
    /// (`const_values` is keyed by declaring file); a constant in another
    /// module is reached qualified and does not participate here merely because
    /// it is globally unique. Matches sema's authoritative by-file resolution
    /// (`resolve_const_info_in_file`) so inference and checking agree, and a
    /// same-named constant in an unrelated module cannot perturb a local length.
    fn scoped_const_value(&self, sym: Spur, file_id: FileId) -> Option<i128> {
        if let Some(lazy) = self.lazy {
            return lazy.const_value((file_id, sym));
        }
        self.const_values
            .and_then(|values| values.get(&(file_id, sym)).copied())
    }

    /// The integer bound to a comptime *value* parameter of the specialization
    /// currently being analyzed (`comptime n: u64` → `n = 3` for `make(3)`).
    /// `None` outside a specialization, for a name that is not a comptime value
    /// parameter, and for a non-integer binding. See the `comptime_values`
    /// field (RUE-268).
    fn comptime_value_int(&self, sym: Spur) -> Option<i128> {
        match self.comptime_values?.get(&sym)? {
            ConstValue::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// Resolve a bare type-alias name in alias-head position against the
    /// referencing file's scope, with the same by-file discipline as
    /// [`Self::scoped_const_value`]: a `const T = SomeType(...)` in the current
    /// module resolves; a same-named alias elsewhere is qualified, not bare.
    fn scoped_const_type_alias(&self, sym: Spur, file_id: FileId) -> Option<Type> {
        if let Some(lazy) = self.lazy {
            return lazy.const_type_alias((file_id, sym));
        }
        self.const_type_aliases
            .and_then(|aliases| aliases.get(&(file_id, sym)).copied())
    }

    /// Get the type variables allocated for integer literals.
    pub fn int_literal_vars(&self) -> &[TypeVarId] {
        &self.int_literal_vars
    }

    /// Allocate a fresh type variable.
    /// Resolve a struct field's declared type when the base expression's type
    /// is already a concrete struct. Returns None for unresolved bases,
    /// non-struct types, and unknown field names (sema diagnoses those).
    fn known_field_type(&self, base_ty: &InferType, field: Spur) -> Option<Type> {
        let InferType::Concrete(ty) = base_ty else {
            return None;
        };
        self.field_type_of(*ty, field)
    }

    /// Convert a resolved `Type` into an `InferType`, representing arrays
    /// structurally so they unify with array-literal expressions. Mirrors
    /// semantic analysis's type-to-infer-type lowering.
    fn type_to_infer(&self, ty: Type) -> InferType {
        match ty.kind() {
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.type_pool.array_def(array_id);
                InferType::Array {
                    element: Box::new(self.type_to_infer(element_type)),
                    length,
                }
            }
            _ => InferType::Concrete(ty),
        }
    }

    /// Look up a method signature.
    ///
    /// Under the lazy provider (production) this materializes the method
    /// signature on demand — including late-registered anonymous-struct methods,
    /// which the provider reads from the live method tables, subsuming the eager
    /// `extra_method_sigs` reconciliation (RUE-164). The eager (unit-test) path
    /// still consults the literal map plus `extra_method_sigs`.
    fn method_sig(&self, key: &(StructId, Spur)) -> Option<Rc<MethodSig>> {
        if let Some(lazy) = self.lazy {
            return lazy.method_sig(*key);
        }
        self.methods
            .and_then(|methods| methods.get(key))
            .or_else(|| self.extra_method_sigs.and_then(|sigs| sigs.get(key)))
            .map(|sig| Rc::new(sig.clone()))
    }

    /// Look up a function signature by internal key. Materialized on demand
    /// under the lazy provider; the eager path clones from the literal map.
    fn func_sig(&self, name: Spur) -> Option<Rc<FunctionSig>> {
        if let Some(lazy) = self.lazy {
            return lazy.func_sig(name);
        }
        self.functions
            .and_then(|functions| functions.get(&name))
            .map(|sig| Rc::new(sig.clone()))
    }

    /// Look up a built-in struct type by source name.
    fn builtin_struct_type(&self, name: Spur) -> Option<Type> {
        if let Some(lazy) = self.lazy {
            return lazy.builtin_struct_type(name);
        }
        self.builtin_structs
            .and_then(|builtins| builtins.get(&name).copied())
    }

    /// Look up a module-local struct type by (defining file, source name).
    fn struct_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        if let Some(lazy) = self.lazy {
            return lazy.struct_type_by_file(key);
        }
        self.structs_by_file_name
            .and_then(|map| map.get(&key).copied())
    }

    /// Look up a built-in enum type by source name.
    fn builtin_enum_type(&self, name: Spur) -> Option<Type> {
        if let Some(lazy) = self.lazy {
            return lazy.builtin_enum_type(name);
        }
        self.builtin_enums
            .and_then(|builtins| builtins.get(&name).copied())
    }

    /// Look up a module-local enum type by (defining file, source name).
    fn enum_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        if let Some(lazy) = self.lazy {
            return lazy.enum_type_by_file(key);
        }
        self.enums_by_file_name
            .and_then(|map| map.get(&key).copied())
    }

    fn nominal_type_accessible(&self, accessing_file: FileId, ty: Type) -> bool {
        if let Some(lazy) = self.lazy {
            return lazy.nominal_type_accessible(accessing_file, ty);
        }
        if let Some(id) = ty.as_struct() {
            let def = self.type_pool.struct_def(id);
            return def.is_pub || def.file_id == accessing_file;
        }
        if let Some(id) = ty.as_enum() {
            let def = self.type_pool.enum_def(id);
            return def.is_pub || def.file_id == accessing_file;
        }
        false
    }

    /// Look up a file-level constant's declared type by (declaring file, name).
    fn const_type(&self, key: (FileId, Spur)) -> Option<Type> {
        if let Some(lazy) = self.lazy {
            return lazy.const_type(key);
        }
        self.const_types.and_then(|map| map.get(&key).copied())
    }

    /// Look up a file-level type alias by (declaring file, name).
    fn const_type_alias(&self, key: (FileId, Spur)) -> Option<Type> {
        if let Some(lazy) = self.lazy {
            return lazy.const_type_alias(key);
        }
        self.const_type_aliases
            .and_then(|map| map.get(&key).copied())
    }

    /// Look up a file-level integer constant value by (declaring file, name).
    fn const_value(&self, key: (FileId, Spur)) -> Option<i128> {
        if let Some(lazy) = self.lazy {
            return lazy.const_value(key);
        }
        self.const_values.and_then(|map| map.get(&key).copied())
    }

    /// Look up a module-binding type by (declaring file, name).
    fn module_binding_type(&self, key: (FileId, Spur)) -> Option<Type> {
        if let Some(lazy) = self.lazy {
            return lazy.module_binding_type(key);
        }
        self.module_binding_types
            .and_then(|map| map.get(&key).copied())
    }

    /// Look up a function-valued-constant callee alias by (declaring file, name).
    fn const_function_alias(&self, key: (FileId, Spur)) -> Option<Spur> {
        if let Some(lazy) = self.lazy {
            return lazy.const_function_alias(key);
        }
        self.const_function_aliases
            .and_then(|map| map.get(&key).copied())
    }

    /// Resolve a module registry file identity.
    fn module_file_id(&self, module: ModuleId) -> Option<FileId> {
        if let Some(lazy) = self.lazy {
            return lazy.module_file_id(module);
        }
        self.module_file_ids
            .and_then(|map| map.get(&module).copied())
    }

    /// Look up a source-level function key by (defining file, source name).
    fn function_by_file(&self, key: (FileId, Spur)) -> Option<Spur> {
        if let Some(lazy) = self.lazy {
            return lazy.function_by_file(key);
        }
        self.functions_by_file_name
            .and_then(|map| map.get(&key).copied())
    }

    /// Resolve a field's declared type on a concrete struct type.
    fn field_type_of(&self, struct_ty: Type, field: Spur) -> Option<Type> {
        self.field_type_of_with_observer(struct_ty, field, || {})
    }

    #[inline(always)]
    fn field_type_of_with_observer(
        &self,
        struct_ty: Type,
        field: Spur,
        observe_candidate: impl FnMut(),
    ) -> Option<Type> {
        let TypeKind::Struct(struct_id) = struct_ty.kind() else {
            return None;
        };
        let field_name = self.interner.resolve(&field);
        self.type_pool
            .struct_def(struct_id)
            .find_field_with_observer(field_name, observe_candidate)
            .map(|(_, field)| field.ty)
    }

    pub fn fresh_var(&mut self) -> TypeVarId {
        self.type_vars.fresh()
    }

    /// Allocate a fresh type variable that behaves like an integer literal:
    /// the unifier rejects binding it to a non-integer type, and if it is
    /// still unbound after solving it defaults to i32. Used for a captured
    /// comptime integer value (an anonymous-struct method reading `comptime
    /// N: u8` from its enclosing function), whose declared width is not
    /// threaded through the capture and is instead recovered from use, exactly
    /// like the literal it stands in for (RUE-216).
    pub fn fresh_int_literal_var(&mut self) -> TypeVarId {
        let var = self.fresh_var();
        self.int_literal_vars.push(var);
        var
    }

    /// Add a constraint.
    pub fn add_constraint(&mut self, constraint: Constraint) {
        self.record_fixed_string_types(&constraint);
        self.constraints.push(constraint);
    }

    /// Record every fixed-string nominal occurring in a constraint's type
    /// shapes. The walk follows structural arrays so enum payloads such as
    /// `[Str(8); 2]` carry the exact pool identity into unification.
    fn record_fixed_string_types(&mut self, constraint: &Constraint) {
        fn record_type(types: &mut Vec<Type>, pool: &TypeInternPool, ty: &InferType) {
            match ty {
                InferType::Concrete(ty) => {
                    if let TypeKind::Struct(id) = ty.kind()
                        && crate::types::fixed_string_struct_capacity(&pool.struct_def(id))
                            .is_some()
                    {
                        types.push(*ty);
                    }
                }
                InferType::Array { element, .. } => record_type(types, pool, element),
                _ => {}
            }
        }
        match constraint {
            Constraint::Equal(lhs, rhs, _) | Constraint::ContextualEqual(lhs, rhs, _) => {
                record_type(&mut self.fixed_string_types, self.type_pool, lhs);
                record_type(&mut self.fixed_string_types, self.type_pool, rhs);
            }
            Constraint::IsInteger(ty, _)
            | Constraint::IsNumeric(ty, _)
            | Constraint::IsSigned(ty, _)
            | Constraint::IsUnsigned(ty, _) => {
                record_type(&mut self.fixed_string_types, self.type_pool, ty)
            }
        }
    }

    /// Record the type of an expression.
    pub fn record_type(&mut self, inst_ref: InstRef, ty: InferType) {
        self.expr_types.insert(inst_ref, ty);
    }

    /// Get the recorded type of an expression.
    pub fn get_type(&self, inst_ref: InstRef) -> Option<&InferType> {
        self.expr_types.get(&inst_ref)
    }

    /// Get all collected constraints.
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// Get the expression type mapping.
    pub fn expr_types(&self) -> &ahash::AHashMap<InstRef, InferType> {
        &self.expr_types
    }

    /// Consume the constraint generator and return its generated constraints,
    /// literal variables, expression types, fixed-string identities, and
    /// allocated variable count.
    ///
    /// This is useful when you need ownership of the expression types map.
    /// The `type_var_count` can be used to pre-size the unifier's substitution for better performance.
    pub fn into_parts(
        self,
    ) -> (
        Vec<Constraint>,
        Vec<TypeVarId>,
        Vec<TypeVarId>,
        Vec<TypeVarId>,
        Type,
        ahash::AHashMap<InstRef, InferType>,
        AHashMap<InstRef, bool>,
        u32,
        Vec<Type>,
    ) {
        let expr_types = finalize_expr_types_map(self.expr_types);
        (
            self.constraints,
            self.int_literal_vars,
            self.float_literal_vars,
            self.string_literal_vars,
            self.string_literal_default,
            expr_types,
            self.expr_continues,
            self.type_vars.count(),
            self.fixed_string_types,
        )
    }

    /// Enter a lexical scope: pushes the context's local-variable scope and
    /// this generator's comptime-alias scope together, so the two stacks stay
    /// in lockstep (RUE-530). Always pair with [`Self::exit_scope`]; scope
    /// sites must not call `ctx.push_scope()` directly.
    fn enter_scope(&mut self, ctx: &mut ConstraintContext) {
        ctx.push_scope();
        self.alias_scope_stack.push(Vec::new());
    }

    /// Exit a lexical scope: pops the context's local-variable scope and
    /// unwinds this scope's comptime-alias frame in reverse, restoring
    /// shadowed aliases and removing ones introduced here (the RUE-522
    /// unwind order).
    fn exit_scope(&mut self, ctx: &mut ConstraintContext) {
        ctx.pop_scope();
        if let Some(frame) = self.alias_scope_stack.pop() {
            for (name, old) in frame.into_iter().rev() {
                match old {
                    Some(ty) => self.comptime_alias_types.insert(name, ty),
                    None => self.comptime_alias_types.remove(&name),
                };
            }
        }
    }

    /// Bring a comptime type alias into scope, saving the name's previous
    /// binding (or absence) in the current scope frame for restore on
    /// [`Self::exit_scope`].
    fn bind_comptime_alias(&mut self, name: Spur, ty: Type) {
        let old = self.comptime_alias_types.insert(name, ty);
        if let Some(frame) = self.alias_scope_stack.last_mut() {
            frame.push((name, old));
        }
    }

    /// A runtime `let` binding hides any same-named comptime type alias for
    /// the rest of its scope (the inner binding wins, whatever its kind);
    /// save the alias in the current frame so it is restored when the
    /// shadowing binding's block ends.
    fn hide_comptime_alias(&mut self, name: Spur) {
        if let Some(old) = self.comptime_alias_types.remove(&name)
            && let Some(frame) = self.alias_scope_stack.last_mut()
        {
            frame.push((name, Some(old)));
        }
    }

    /// Generate constraints for an expression.
    ///
    /// Returns the inferred type of the expression. Records the type in
    /// `expr_types` and adds constraints to `constraints`.
    pub fn generate(&mut self, inst_ref: InstRef, ctx: &mut ConstraintContext) -> ExprInfo {
        let inst = self.rir.get(inst_ref);
        let span = inst.span;
        if self.canceled {
            return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
        }
        if self.cancel_check.as_ref().is_some_and(|check| check()) {
            self.canceled = true;
            return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
        }
        let mut continues = true;

        let ty = match &inst.data {
            InstData::IntConst(_) => {
                // Integer literals get a fresh type variable, recorded in
                // `int_literal_vars`. The unifier is told about these variables
                // (via `mark_int_literal_vars`) so that binding one to a
                // non-integer type is rejected at the offending constraint, and
                // any that remain unbound after solving default to i32 (via
                // `default_int_literal_vars`).
                //
                // Example: `let x: i64 = 42` generates:
                //   - type_var(?0) for the literal 42
                //   - constraint: Equal(Var(?0), Concrete(i64))
                // which binds ?0 -> Concrete(i64) during unification.
                let var = self.fresh_var();
                self.int_literal_vars.push(var);
                InferType::Var(var)
            }

            InstData::FloatConst { .. } => {
                let var = self.fresh_var();
                self.float_literal_vars.push(var);
                InferType::Var(var)
            }

            InstData::BoolConst(_) => InferType::Concrete(Type::BOOL),

            // String literals are context-sensitive. A fresh variable lets an
            // explicit `StrBuf` context retain the owning-buffer type;
            // otherwise semantic analysis defaults it after unification (to
            // core `str`, with contextual promotion to trusted `StrBuf`).
            InstData::StringConst { .. } => {
                let var = self.fresh_var();
                self.string_literal_vars.push(var);
                InferType::Var(var)
            }

            InstData::UnitConst => InferType::Concrete(Type::UNIT),

            // Addition is overloaded: integer arithmetic OR String + String
            // concatenation (RUE-17 Phase 1, ADR-0035). Split out from the pure
            // arithmetic operators so it doesn't force an `is_integer`
            // constraint on String operands.
            InstData::Add { lhs, rhs } => self.generate_add(inst_ref, *lhs, *rhs, ctx),

            // Binary arithmetic: both operands must have the same type, result is that type
            InstData::Sub { lhs, rhs }
            | InstData::Mul { lhs, rhs }
            | InstData::Div { lhs, rhs }
            | InstData::Mod { lhs, rhs } => self.generate_binary_arith(inst_ref, *lhs, *rhs, ctx),

            // Bitwise operations: same as arithmetic
            InstData::BitAnd { lhs, rhs }
            | InstData::BitOr { lhs, rhs }
            | InstData::BitXor { lhs, rhs }
            | InstData::Shl { lhs, rhs }
            | InstData::Shr { lhs, rhs } => {
                let ty = self.generate_binary_arith(inst_ref, *lhs, *rhs, ctx);
                self.add_constraint(Constraint::is_integer(ty.clone(), span));
                ty
            }

            // Comparison operators: operands must match, result is bool
            InstData::Eq { lhs, rhs }
            | InstData::Ne { lhs, rhs }
            | InstData::Lt { lhs, rhs }
            | InstData::Gt { lhs, rhs }
            | InstData::Le { lhs, rhs }
            | InstData::Ge { lhs, rhs } => {
                let lhs_info = self.generate(*lhs, ctx);
                let reachable_facts_after_lhs = ctx.loop_break_stack.clone();
                let rhs_info = self.generate(*rhs, ctx);
                if !lhs_info.continues {
                    restore_reachable_break_facts(
                        &mut ctx.loop_break_stack,
                        &reachable_facts_after_lhs,
                    );
                }
                continues &= lhs_info.continues && rhs_info.continues;
                // Operands must have the same type. (Chained comparisons are
                // rejected at parse time — validate.rs, RUE-528 — so a
                // comparison LHS reaching here is a legitimately
                // parenthesized boolean operand and gets normal typing.)
                self.add_constraint(Constraint::equal(lhs_info.ty, rhs_info.ty, span));
                InferType::Concrete(Type::BOOL)
            }

            // Logical operators: operands must be bool, result is bool
            InstData::And { lhs, rhs } | InstData::Or { lhs, rhs } => {
                let lhs_info = self.generate(*lhs, ctx);
                let reachable_facts_after_lhs = ctx.loop_break_stack.clone();
                let rhs_info = self.generate(*rhs, ctx);
                if !lhs_info.continues {
                    restore_reachable_break_facts(
                        &mut ctx.loop_break_stack,
                        &reachable_facts_after_lhs,
                    );
                }
                // Logical operators short-circuit. A diverging RHS does not
                // eliminate the LHS path that skips it, so only divergence of
                // the always-evaluated LHS removes normal continuation.
                continues &= lhs_info.continues;
                self.add_constraint(Constraint::equal(
                    lhs_info.ty,
                    InferType::Concrete(Type::BOOL),
                    lhs_info.span,
                ));
                self.add_constraint(Constraint::equal(
                    rhs_info.ty,
                    InferType::Concrete(Type::BOOL),
                    rhs_info.span,
                ));
                InferType::Concrete(Type::BOOL)
            }

            // Unary negation: operand must be signed integer
            InstData::Neg { operand } => {
                let operand_info = self.generate(*operand, ctx);
                continues &= operand_info.continues;
                // Result type is the same as operand type
                let result_ty = operand_info.ty.clone();
                // Must be a signed integer
                self.add_constraint(Constraint::is_signed(result_ty.clone(), span));
                result_ty
            }

            // Logical NOT: operand must be bool
            InstData::Not { operand } => {
                let operand_info = self.generate(*operand, ctx);
                continues &= operand_info.continues;
                self.add_constraint(Constraint::equal(
                    operand_info.ty,
                    InferType::Concrete(Type::BOOL),
                    operand_info.span,
                ));
                InferType::Concrete(Type::BOOL)
            }

            // Bitwise NOT: operand must be integer
            InstData::BitNot { operand } => {
                let operand_info = self.generate(*operand, ctx);
                continues &= operand_info.continues;
                let result_ty = operand_info.ty.clone();
                // Must be an integer type (signed or unsigned)
                self.add_constraint(Constraint::is_integer(result_ty.clone(), span));
                result_ty
            }

            // Try/`?`: unwraps `Option(T)` to `T` (RUE-6, ADR-0038). When the
            // operand's type is already a concrete `Option`-shaped enum, the
            // result is its `Some` payload; otherwise a fresh variable that
            // sema types authoritatively from the operand's enum. Sema does the
            // full checking (operand is Option, enclosing fn returns Option).
            InstData::Try { operand } => {
                let operand_info = self.generate(*operand, ctx);
                continues &= operand_info.continues;
                match &operand_info.ty {
                    InferType::Concrete(ty) => ty
                        .as_enum()
                        .map(|enum_id| self.type_pool.enum_def(enum_id))
                        .and_then(|def| {
                            def.find_variant("Some")
                                .and_then(|si| def.variant_payload(si).first().copied())
                        })
                        .map(InferType::Concrete)
                        .unwrap_or_else(|| InferType::Var(self.fresh_var())),
                    _ => InferType::Var(self.fresh_var()),
                }
            }

            // Variable reference
            InstData::VarRef { name, .. } => {
                if let Some(local) = ctx.locals.get(name) {
                    local.ty.clone()
                } else if let Some(param) = ctx.lookup_param(*name) {
                    param.ty.clone()
                } else if let Some(binding_ty) = self.module_binding_type((span.file_id, *name)) {
                    // Module binding declared in this file (`const m =
                    // @import(...)`): per-file scoped and distinct from the
                    // file's value-constant namespace (RUE-113).
                    self.type_to_infer(binding_ty)
                } else if let Some(const_ty) = self.const_type((span.file_id, *name)) {
                    // File-level constant: its type was resolved during
                    // declaration gathering (i32/bool/unit literals, module
                    // types for `const m = @import(...)`). Yielding it here
                    // anchors expressions like `N + 1` and `m.go() + 1` to the
                    // declaration's concrete type (RUE-142).
                    self.type_to_infer(const_ty)
                } else if self.struct_type_for(name, span.file_id).is_some()
                    || self.enum_type_for(name, span.file_id).is_some()
                {
                    // Named nominal types parse as `VarRef` when they appear
                    // in value position. Sema materializes them as compile-time
                    // `TypeConst` values, so inference must publish the same
                    // type instead of treating the name like an unresolved
                    // runtime variable. In particular, this keeps a type name
                    // used as an implicit function result from passing through
                    // the error-type compatibility escape hatch and reaching
                    // CFG construction without a runtime return value.
                    InferType::Concrete(Type::COMPTIME_TYPE)
                } else {
                    // Unknown variable - will be caught during semantic analysis
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Local variable allocation
            InstData::Alloc {
                directives: _,
                name,
                is_mut,
                ty: type_annotation,
                init,
                iter_elem: _,
            } => {
                let init_info = self.generate(*init, ctx);
                continues &= init_info.continues;

                let var_ty = if let Some(type_syntax) = type_annotation {
                    // Explicit type annotation - use it and constrain init to
                    // match. Comptime type aliases (`let P = F(); let p: P =
                    // ...`) resolve first, mirroring sema's annotation
                    // validation order (`comptime_type_vars` before the type
                    // tables); without this the annotation was unenforced and
                    // any value typechecked against it (RUE-170).
                    // Preserve the established best-effort inference boundary:
                    // a body-local annotation resolves file constants and
                    // already-bound type aliases, but specialization values are
                    // applied by the authoritative semantic pass. Eagerly
                    // substituting them here makes inference reject programs
                    // that the semantic annotation/coercion path accepts.
                    let annotated = self.infer_type_hint(
                        self.rir.type_syntax(),
                        *type_syntax,
                        None,
                        None,
                        span.file_id,
                    );
                    if let Some(annotated_ty) = annotated {
                        // A `str` annotation (ADR-0043 Phase 3, RUE-324) accepts
                        // a string literal (HM type `String`) by coercion, and a
                        // `[T]` slice annotation is second-class (rejected by
                        // sema anyway); in both cases skip strict equality and
                        // let sema materialize the value (mirrors the call-arg
                        // slice/`str` coercion below).
                        if !self.is_slice_struct_type(annotated_ty.clone()) {
                            self.add_constraint(Constraint::contextual(
                                init_info.ty,
                                annotated_ty.clone(),
                                span,
                            ));
                        }
                        annotated_ty
                    } else {
                        // Unknown type name (e.g., struct/enum) - use init type for now.
                        // Semantic analysis will catch undefined types and verify struct/enum
                        // field types match the definition.
                        init_info.ty
                    }
                } else if self
                    .comptime_local_bindings
                    .is_some_and(|bindings| bindings.contains_key(&inst_ref))
                {
                    // Sema pre-resolved this binding as a comptime type alias
                    // (`let O = std.option.Option(i64);`). A module-qualified
                    // constructor init infers as a fresh variable (the module
                    // member-call arm can't reduce a `-> type` body), which
                    // blocked the type-name-receiver forwarding for `O.Some(..)`
                    // and left payload literals unconstrained (RUE-609, the
                    // RUE-599 failure mode through a bound alias). The binding
                    // holds a type value, so type it as one; downstream paths
                    // read the concrete aliased type from `comptime_alias_types`.
                    InferType::Concrete(Type::COMPTIME_TYPE)
                } else {
                    // No annotation - use the init expression's type
                    init_info.ty
                };

                // Record the variable in scope (if it has a name), and keep
                // the comptime-alias view in sync: an alias binding comes
                // into scope here (its initializer above must not see it —
                // `let P = Wrap(P);` refers to the outer `P`), and a runtime
                // binding hides any same-named outer alias for the rest of
                // this scope (RUE-530).
                if let Some(var_name) = name {
                    ctx.insert_local(
                        *var_name,
                        LocalVarInfo {
                            ty: var_ty.clone(),
                            is_mut: *is_mut,
                            span,
                        },
                    );
                    match self
                        .comptime_local_bindings
                        .and_then(|bindings| bindings.get(&inst_ref).copied())
                    {
                        Some(alias_ty) => self.bind_comptime_alias(*var_name, alias_ty),
                        None => self.hide_comptime_alias(*var_name),
                    }
                }

                // Keep the binding's inferred type available to staged
                // frontier checkpoints.  Selected nested bodies may be
                // generated independently of their prefix, so their lexical
                // locals are supplied as synthetic parameters from this
                // recorded type.
                self.record_type(inst_ref, var_ty.clone());

                // Alloc normally produces unit, but an initializer that
                // cannot reach the binding never reaches that allocation.
                // Preserve the declared binding type above while exposing
                // bottom to the enclosing block.
                InferType::Concrete(Type::UNIT)
            }

            // Assignment
            InstData::Assign { name, value } => {
                let value_info = self.generate(*value, ctx);
                continues &= value_info.continues;
                // A local shadows a same-named parameter. Otherwise, constrain
                // assignment against the declared parameter type too. Every
                // `inout` whole assignment must be constrained before it can
                // reach `ParamStore` (RUE-641).
                let target_ty = ctx
                    .locals
                    .get(name)
                    .map(|local| local.ty.clone())
                    .or_else(|| {
                        ctx.params
                            .get(name)
                            .filter(|param| param.is_inout)
                            .map(|param| param.ty.clone())
                    });
                if let Some(target_ty) = target_ty {
                    // A `str` target (ADR-0043 Phase 3, RUE-324) accepts a string
                    // literal (HM type `String`) by coercion; skip strict
                    // equality and let sema materialize the `str` on the store.
                    if !self.is_slice_struct_type(target_ty.clone())
                        && value_info.continues
                        && !Self::is_never_concrete(&value_info.ty)
                    {
                        // Assignment stores the value with its semantic type.
                        self.add_constraint(Constraint::contextual(value_info.ty, target_ty, span));
                    }
                }
                // Assignment produces unit
                InferType::Concrete(Type::UNIT)
            }

            InstData::PlaceSet { place, value } => {
                let value_info = self.generate(*value, ctx);
                if self.was_canceled() {
                    return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                }
                let place_info = self.generate_sequenced_operand(*place, ctx, value_info.continues);
                if self.was_canceled() {
                    return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                }
                continues &= place_info.continues && value_info.continues;
                if value_info.continues {
                    self.add_constraint(Constraint::equal(place_info.ty, value_info.ty, span));
                }
                InferType::Concrete(Type::UNIT)
            }

            // Return statement
            InstData::Ret(value) => {
                continues = false;
                if let Some(val_ref) = value {
                    let value_info = self.generate(*val_ref, ctx);
                    // Constrain return value to match function return type. A
                    // `str` return (ADR-0043 Phase 3, RUE-324) accepts a string
                    // literal (HM type `String`) by coercion; skip strict
                    // equality there and let sema materialize the `str`.
                    if value_info.continues
                        && !self.is_slice_struct_type(InferType::Concrete(ctx.return_type))
                    {
                        self.add_constraint(Constraint::contextual(
                            value_info.ty,
                            InferType::Concrete(ctx.return_type),
                            span,
                        ));
                    }
                } else {
                    // Return without value - function must return unit
                    self.add_constraint(Constraint::equal(
                        InferType::Concrete(Type::UNIT),
                        InferType::Concrete(ctx.return_type),
                        span,
                    ));
                }
                // Return diverges
                InferType::Concrete(Type::NEVER)
            }

            // Accessor yield (ADR-0062): the yielded place must have the
            // accessor's declared element type `T` (the function's return
            // type). Accessor `yield` is represented with the never surface
            // type, but its standalone accessor CFG preserves the yielded
            // place as a distinguished reachable operand for call-site
            // splicing, so it does not carry ordinary return divergence.
            InstData::Yield(value) => {
                let value_info = self.generate(*value, ctx);
                continues &= value_info.continues;
                self.add_constraint(Constraint::contextual(
                    value_info.ty,
                    InferType::Concrete(ctx.return_type),
                    span,
                ));
                InferType::Concrete(Type::NEVER)
            }

            // Function call
            InstData::Call { name, args } => {
                let alias_target = self.const_function_alias((span.file_id, *name));
                let function_key =
                    alias_target.or_else(|| self.function_by_file((span.file_id, *name)));
                let args = self.rir.call_args(args);
                let mut arg_diverged = false;
                // `print(s)` / `println(s)` builtin free functions (RUE-1):
                // generate the argument and yield unit. Semantic analysis
                // validates the shared text family (`StrBuf`, `str`, `Str(N)`),
                // while an unconstrained literal follows the normal edition /
                // preview default.
                // Only when the program hasn't shadowed the name with its own
                // `fn print`/`fn println` (a user definition wins).
                let is_print_builtin = function_key.is_none()
                    && matches!(self.interner.resolve(name), "print" | "println");
                let result = if is_print_builtin {
                    for arg in args.iter() {
                        let info = self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                        arg_diverged |= !info.continues;
                        if self.was_canceled() {
                            break;
                        }
                    }
                    InferType::Concrete(Type::UNIT)
                } else if let Some(func) = function_key.and_then(|key| self.func_sig(key)) {
                    // For generic functions, build the type substitution map from the
                    // comptime type arguments, then constrain each runtime argument
                    // against its (substituted) parameter type. When a type parameter
                    // can't be resolved here (e.g. it's a local variable holding a
                    // type value), the constraint is skipped and the check happens in
                    // semantic analysis instead (RUE-73, RUE-99).
                    if func.is_generic {
                        // Process all arguments once, collecting their inferred types
                        let mut arg_infos = Vec::with_capacity(args.len());
                        for arg in args.iter() {
                            if self.was_canceled() {
                                break;
                            }
                            let info =
                                self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                            arg_diverged |= !info.continues;
                            arg_infos.push(info);
                            if self.was_canceled() {
                                break;
                            }
                        }

                        // Build the type substitution map from comptime type arguments
                        let mut type_subst: AHashMap<lasso::Spur, Type> = AHashMap::new();
                        // Comptime VALUE arguments (`comptime N: i32`) captured
                        // as their integer constant, so a return/param type
                        // sized by one — an array length `[i32; N]` — resolves
                        // at this call (RUE-252).
                        let mut value_subst: AHashMap<lasso::Spur, i128> = AHashMap::new();
                        for (i, arg) in args.iter().enumerate() {
                            if self.was_canceled() {
                                break;
                            }
                            if i >= func.param_comptime.len()
                                || !func.param_comptime[i]
                                || i >= func.param_names.len()
                            {
                                continue;
                            }
                            if func.param_comptime_type.get(i) == Some(&true) {
                                if let Some(ConstValue::Type(concrete_ty)) =
                                    self.comptime_argument_value(arg.value)
                                {
                                    type_subst.insert(func.param_names[i], concrete_ty);
                                } else if let Some(concrete_ty) =
                                    self.extract_type_argument(arg.value, ctx)
                                {
                                    type_subst.insert(func.param_names[i], concrete_ty);
                                }
                            } else if let Some(ConstValue::Integer(v)) =
                                self.comptime_argument_value(arg.value)
                            {
                                value_subst.insert(func.param_names[i], v);
                            }
                        }

                        // Constrain each runtime argument to its parameter type, with
                        // type parameters substituted. Comptime type parameters (the
                        // `T: type` arguments themselves) are validated in sema.
                        for (i, arg_info) in arg_infos.iter().enumerate() {
                            if self.was_canceled() {
                                break;
                            }
                            if i >= func.param_types.len() || i >= func.param_comptime.len() {
                                break;
                            }
                            let declared = &func.param_types[i];
                            if self.staged_comptime_selectors
                                && self
                                    .comptime_argument_values
                                    .is_none_or(|values| values.is_empty())
                                && *declared == InferType::Concrete(Type::COMPTIME_TYPE)
                            {
                                continue;
                            }
                            if func.param_comptime_type.get(i) == Some(&true) {
                                // Comptime TYPE parameter - the argument is a type value
                                continue;
                            }
                            let expected = if *declared == InferType::Concrete(Type::COMPTIME_TYPE)
                            {
                                // Generic parameter like `x: T`, or a composite
                                // mentioning a type parameter like `a: [T; 3]`
                                // (RUE-172) - substitute T
                                match func.param_type_syntax.get(i).and_then(|syntax| {
                                    syntax.as_ref().and_then(|syntax| {
                                        self.infer_structured_type_hint(
                                            syntax,
                                            &type_subst,
                                            &value_subst,
                                            span.file_id,
                                        )
                                    })
                                }) {
                                    Some(ty) => ty,
                                    // Unknown type parameter - checked in sema
                                    None => continue,
                                }
                            } else {
                                declared.clone()
                            };
                            // Slice parameters coerce from an array argument
                            // (ADR-0043, RUE-322); see the non-generic path.
                            if self.is_slice_struct_type(expected.clone()) {
                                continue;
                            }
                            self.add_constraint(Constraint::equal(
                                arg_info.ty.clone(),
                                expected,
                                arg_info.span,
                            ));
                        }

                        // Compute the actual return type by substituting type
                        // parameters - bare (`-> T`) or inside a composite
                        // (`-> [T; 3]`, RUE-172).
                        let return_type = if func.return_type
                            == InferType::Concrete(Type::COMPTIME_TYPE)
                        {
                            match func.return_type_syntax.as_ref().and_then(|syntax| {
                                self.infer_structured_type_hint(
                                    syntax,
                                    &type_subst,
                                    &value_subst,
                                    span.file_id,
                                )
                            }) {
                                Some(ty) => ty,
                                None => {
                                    // The declared return type is a
                                    // type-function application to a type
                                    // parameter (`-> Option(T)`; RUE-272).
                                    // The constraint generator can't reduce
                                    // a `-> type` constructor body, so it
                                    // can't name the monomorphized
                                    // struct/enum here — sema computes the
                                    // true type in the analyze pass. Use a
                                    // fresh inference variable (pinned by
                                    // the call's use site) rather than the
                                    // `COMPTIME_TYPE` placeholder, which
                                    // would spuriously unify against the real
                                    // result type and reject the program
                                    // (E0206). A literal `-> type`
                                    // constructor call is NOT a type-call in
                                    // this sense and still yields
                                    // `COMPTIME_TYPE`.
                                    let is_type_call =
                                        func.return_type_syntax.as_ref().is_some_and(|syntax| {
                                            matches!(
                                                syntax.arena.node(syntax.root),
                                                Some(RirTypeSyntaxNode::TypeCall { .. })
                                            )
                                        });
                                    if self.staged_comptime_selectors || is_type_call {
                                        InferType::Var(self.fresh_var())
                                    } else {
                                        func.return_type.clone()
                                    }
                                }
                            }
                        } else {
                            func.return_type.clone()
                        };

                        return_type
                    } else if args.len() != func.param_types.len() {
                        // Check argument count matches parameter count.
                        // Semantic analysis will emit a proper error; we just need to avoid
                        // panicking and process what we can.
                        // Still process all arguments to catch type errors within them
                        for arg in args.iter() {
                            let info =
                                self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                            arg_diverged |= !info.continues;
                            if self.was_canceled() {
                                break;
                            }
                        }
                        // Return the declared return type (error will be caught in sema)
                        func.return_type.clone()
                    } else {
                        // Generate constraints for each argument
                        for (arg, param_ty) in args.iter().zip(func.param_types.iter()) {
                            let arg_info =
                                self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                            arg_diverged |= !arg_info.continues;
                            if self.was_canceled() {
                                break;
                            }
                            // Slice parameters coerce from an array argument
                            // (`borrow arr`); skip strict equality and let sema
                            // materialize the fat pointer (ADR-0043, RUE-322).
                            if self.is_slice_struct_type(param_ty.clone()) {
                                continue;
                            }
                            self.add_constraint(Constraint::contextual(
                                arg_info.ty,
                                param_ty.clone(),
                                arg_info.span,
                            ));
                        }
                        func.return_type.clone()
                    }
                } else {
                    // Unknown function - still process arguments for constraint generation
                    for arg in args.iter() {
                        let info = self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                        arg_diverged |= !info.continues;
                        if self.was_canceled() {
                            break;
                        }
                    }
                    InferType::Concrete(Type::ERROR)
                };
                if arg_diverged || Self::is_never_concrete(&result) {
                    continues = false;
                    result
                } else {
                    result
                }
            }

            // Intrinsic call
            InstData::Intrinsic { name, args } => {
                let intrinsic_name = self.interner.resolve(name);
                let args = self.rir.intrinsic_args(args);
                let mut args_reachable = true;
                macro_rules! generate_intrinsic_arg {
                    ($arg:expr) => {{
                        if self.was_canceled() {
                            return ExprInfo::diverged(
                                InferType::Concrete(Type::ERROR),
                                self.rir.get($arg).span,
                            );
                        }
                        let info = self.generate_sequenced_operand($arg, ctx, args_reachable);
                        if self.was_canceled() {
                            return ExprInfo::diverged(
                                InferType::Concrete(Type::ERROR),
                                self.rir.get($arg).span,
                            );
                        }
                        args_reachable &= info.continues;
                        info
                    }};
                }

                let intrinsic_ty = if intrinsic_name == "intCast"
                    || intrinsic_name == "bitCast"
                    || intrinsic_name == "cast"
                {
                    // @intCast: target type is inferred from context.
                    // @bitCast: the same context-supplied target (RUE-952); sema
                    // additionally requires the two widths to agree (E0950).
                    // @cast: a fresh var here too, so sema can reject it with a
                    // clean "use @intCast" diagnostic instead of inference
                    // masking it with a type-mismatch error (RUE-319).
                    // The argument must be an integer type.
                    for arg_ref in args.iter() {
                        // Process arguments for constraint generation; the
                        // integer check happens in sema.
                        let _ = generate_intrinsic_arg!(*arg_ref);
                    }
                    // Return type is inferred from context - create a fresh type variable
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "int_to_float" || intrinsic_name == "float_cast" {
                    for arg_ref in args.iter() {
                        let _ = generate_intrinsic_arg!(*arg_ref);
                    }
                    let result = self.fresh_var();
                    self.float_literal_vars.push(result);
                    InferType::Var(result)
                } else if intrinsic_name == "float_to_int" {
                    for arg_ref in args.iter() {
                        let _ = generate_intrinsic_arg!(*arg_ref);
                    }
                    let result = self.fresh_var();
                    self.int_literal_vars.push(result);
                    InferType::Var(result)
                } else if intrinsic_name == "total_cmp" {
                    let common = self.fresh_var();
                    self.float_literal_vars.push(common);
                    let common = InferType::Var(common);
                    for arg_ref in args.iter() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        self.add_constraint(Constraint::equal(info.ty, common.clone(), info.span));
                    }
                    InferType::Concrete(Type::I32)
                } else if intrinsic_name == "panic" {
                    continues = false;
                    // `@panic` diverges: it aborts the process and never returns,
                    // so its expression type is `!` (never), a control-transfer
                    // form that participates in never coercion (spec 3.4:2,
                    // 4.13:5c; formal core §5.7; RUE-512). Keeping it explicit
                    // here — rather than leaning on the generic unit fallback —
                    // stops HM and semantic analysis from drifting apart.
                    for arg_ref in args.iter() {
                        // Text-taking intrinsics accept every stable text view.
                        // Leave literals unconstrained so they take the
                        // canonical `str` default when std is not imported.
                        generate_intrinsic_arg!(*arg_ref);
                    }
                    InferType::Concrete(Type::NEVER)
                } else if intrinsic_name == "assert" {
                    // `@assert` is NOT never-typed: on the success path it returns
                    // and evaluates to `()`. It only aborts when the condition is
                    // false, so its static type is unit on both paths (spec
                    // 4.13:5d). Keep it explicit so HM and sema stay in lockstep.
                    for arg_ref in args.iter() {
                        // As with `@panic`, a literal message keeps the stable
                        // `str` default instead of requiring imported StrBuf.
                        generate_intrinsic_arg!(*arg_ref);
                    }
                    InferType::Concrete(Type::UNIT)
                } else if intrinsic_name == "assert_eq" || intrinsic_name == "assert_ne" {
                    // `@assert_eq(l, r)` / `@assert_ne(l, r)`: the two operands
                    // share one type and the intrinsic evaluates to `()` on the
                    // path that continues, exactly like `@assert` (spec
                    // 4.13:5f). Unifying the operands here is what lets
                    // `@assert_eq(port, 8080)` give the literal the other
                    // side's type instead of the bare `i32` default; sema then
                    // checks that type supports `==`.
                    let operand_var = self.fresh_var();
                    let operand_ty = InferType::Var(operand_var);
                    for arg_ref in args.iter() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        self.add_constraint(Constraint::equal(
                            info.ty,
                            operand_ty.clone(),
                            info.span,
                        ));
                    }
                    InferType::Concrete(Type::UNIT)
                } else if intrinsic_name == "read_line" {
                    // @read_line: returns `Option(String)` (RUE-6, ADR-0038).
                    // The concrete Option type comes from context (a `let`
                    // annotation or the match arms the result feeds), so use a
                    // fresh variable and let unification resolve it — mirroring
                    // @intCast. Sema validates the resolved type is an
                    // Option-shaped enum over String.
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "to_string" {
                    // @to_string(n): takes any integer width, returns String
                    // (RUE-17 Phase 1, ADR-0035; RUE-314). No i64 constraint is
                    // added, so the argument keeps its own type; a bare integer
                    // literal defaults to i32 like everywhere else. Sema checks
                    // the resolved type is an integer and widens per signedness.
                    for arg_ref in args.iter() {
                        generate_intrinsic_arg!(*arg_ref);
                    }
                    self.string_infer_type()
                } else if intrinsic_name == "parse_i32"
                    || intrinsic_name == "parse_i64"
                    || intrinsic_name == "parse_u32"
                    || intrinsic_name == "parse_u64"
                {
                    // @parse_*: takes a String, returns `Option(int)` (RUE-6,
                    // ADR-0038). The concrete Option type (and thus the payload
                    // int type) is resolved from context, so use a fresh
                    // variable and let sema validate the resolved Option shape.
                    for arg_ref in args.iter() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        if self.is_string_literal_candidate(&info.ty) {
                            self.add_constraint(Constraint::equal(
                                info.ty,
                                self.string_infer_type(),
                                info.span,
                            ));
                        }
                    }
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "random_u32" {
                    // @random_u32: no arguments, returns u32
                    InferType::Concrete(Type::U32)
                } else if intrinsic_name == "random_u64" {
                    // @random_u64: no arguments, returns u64
                    InferType::Concrete(Type::U64)
                } else if intrinsic_name == "arg_count" || intrinsic_name == "env_count" {
                    // @arg_count / @env_count: no arguments, returns u64
                    // (RUE-935).
                    InferType::Concrete(Type::U64)
                } else if intrinsic_name == "arg_len" || intrinsic_name == "env_len" {
                    // @arg_len(i) / @env_len(i): a single u64 index, returns u64
                    // (RUE-935).
                    for arg_ref in args.iter() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        self.add_constraint(Constraint::contextual(
                            info.ty,
                            InferType::Concrete(Type::U64),
                            info.span,
                        ));
                    }
                    InferType::Concrete(Type::U64)
                } else if intrinsic_name == "arg_ptr" || intrinsic_name == "env_ptr" {
                    // @arg_ptr(i) / @env_ptr(i): a single u64 index, returns
                    // `ptr mut u8` (RUE-935).
                    for arg_ref in args.iter() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        self.add_constraint(Constraint::equal(
                            info.ty,
                            InferType::Concrete(Type::U64),
                            info.span,
                        ));
                    }
                    InferType::Concrete(Type::new_ptr_mut(
                        self.type_pool.intern_ptr_mut_from_type(Type::U8),
                    ))
                } else if intrinsic_name == "wrapping_add"
                    || intrinsic_name == "wrapping_sub"
                    || intrinsic_name == "wrapping_mul"
                {
                    // @wrapping_add/sub/mul(a, b): both operands and the result
                    // share one integer type — the same equality-and-integer
                    // constraints as checked `+`/`-`/`*` (see `generate_add`),
                    // minus the String-concat overload. Sema re-emits the
                    // resolved node without the overflow check (RUE-647).
                    let result_var = self.fresh_var();
                    let result_ty = InferType::Var(result_var);
                    for arg_ref in args.iter() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        self.add_constraint(Constraint::equal(
                            info.ty,
                            result_ty.clone(),
                            info.span,
                        ));
                    }
                    self.add_constraint(Constraint::is_integer(result_ty.clone(), span));
                    result_ty
                } else if intrinsic_name == "syscall" {
                    // @syscall: syscall_num and up to 6 args (all u64), returns i64.
                    //
                    // An integer-literal argument sees the declared u64
                    // parameter type here (RUE-954), so `@syscall(32, 1)`
                    // works without pre-binding `let fd: u64 = 1`. Only
                    // literals are constrained: a wrongly-typed non-literal
                    // argument keeps sema's targeted E0702 (`u64 for argument
                    // {i}`) instead of a generic unification E0206.
                    for arg_ref in args.iter() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        if matches!(self.rir.get(*arg_ref).data, InstData::IntConst(_)) {
                            self.add_constraint(Constraint::equal(
                                info.ty,
                                InferType::Concrete(Type::U64),
                                info.span,
                            ));
                        }
                    }
                    InferType::Concrete(Type::I64)
                } else if intrinsic_name == "ptr_to_int" {
                    // @ptr_to_int: takes a pointer, returns u64
                    for arg_ref in args.iter() {
                        generate_intrinsic_arg!(*arg_ref);
                    }
                    InferType::Concrete(Type::U64)
                } else if intrinsic_name == "ptr_write" || intrinsic_name == "ptr_write_unaligned" {
                    // @ptr_write / @ptr_write_unaligned: takes a pointer and
                    // value, returns unit (ADR-0059 Phase 4, RUE-978).
                    //
                    // When the pointer operand is already concrete in this pass
                    // (an annotated binding, a parameter, anything whose type
                    // does not depend on later unification), the pointee is the
                    // value operand's expectation — the same contextual channel
                    // `@intCast` reads out of HM. Without it `@ptr_write(p,
                    // @intCast(x))` left the cast's target variable free and
                    // sema reported E0709 (RUE-1341). The constraint mirrors
                    // sema's own `types_compatible` check exactly: `never` and
                    // `<error>` still coerce in `Unifier::unify`.
                    //
                    // If the pointer's type is not yet resolved here (e.g. it
                    // came from `@raw`/`@ptr_offset`, which are themselves
                    // fresh variables), nothing is added and the value operand
                    // stays exactly as free as before — sema keeps its own
                    // pointee reconciliation and its diagnostics are unchanged.
                    //
                    // Only a well-formed two-operand call is typed here; a
                    // wrong-arity call keeps generating its arguments
                    // unconstrained so sema still owns the arity diagnostic.
                    let typed_shape = args.len() == 2;
                    let mut pointee = None;
                    for (index, arg_ref) in args.iter().enumerate() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        if !typed_shape {
                            continue;
                        }
                        match index {
                            0 => pointee = self.concrete_pointee_type(&info.ty),
                            1 => {
                                // A `str`/`Str(N)`/slice pointee accepts its
                                // operand by coercion, so it takes the same
                                // strict-equality exemption as a call argument
                                // (see `is_slice_struct_type`); sema still
                                // materializes and checks it.
                                if let Some(pointee) = pointee
                                    && !self.is_slice_struct_type(InferType::Concrete(pointee))
                                {
                                    self.add_constraint(Constraint::contextual(
                                        info.ty,
                                        InferType::Concrete(pointee),
                                        info.span,
                                    ));
                                }
                            }
                            _ => {}
                        }
                    }
                    InferType::Concrete(Type::UNIT)
                } else if intrinsic_name == "ptr_read" || intrinsic_name == "ptr_read_unaligned" {
                    // @ptr_read / @ptr_read_unaligned: takes ptr const T or ptr
                    // mut T, returns T.
                    //
                    // The result is the pointee type. When the pointer operand
                    // is already concrete in this pass, publish that pointee so
                    // the read participates in inference like any other typed
                    // expression: `@ptr_read(p) == 30` then unifies the literal
                    // against the pointee instead of defaulting it to i32 and
                    // failing E0206 in sema (RUE-1341).
                    //
                    // Otherwise fall back to a fresh variable, exactly as
                    // before: the pointee is only known in sema, which fixes
                    // the result type there and reconciles it against whatever
                    // the annotation constrained the variable to (RUE-244).
                    // A wrong-arity call stays on that fallback so sema still
                    // owns the arity diagnostic.
                    let typed_shape = args.len() == 1;
                    let mut pointee = None;
                    for (index, arg_ref) in args.iter().enumerate() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        if typed_shape && index == 0 {
                            pointee = self.concrete_pointee_type(&info.ty);
                        }
                    }
                    match pointee {
                        Some(pointee) => InferType::Concrete(pointee),
                        None => {
                            let result_var = self.fresh_var();
                            InferType::Var(result_var)
                        }
                    }
                } else if intrinsic_name == "ptr_offset" {
                    // @ptr_offset: takes (ptr T, i64), returns ptr T
                    // The return type is the same as the input pointer type.
                    // Publish that identity when a well-formed call already
                    // has a concrete pointer operand; unresolved pointers and
                    // wrong-arity calls remain free so sema owns diagnostics.
                    let typed_shape = args.len() == 2;
                    let mut pointer_ty = None;
                    let mut offset_is_integer = false;
                    for (index, arg_ref) in args.iter().enumerate() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        if typed_shape {
                            match index {
                                0 => pointer_ty = self.concrete_type(&info.ty).filter(Type::is_ptr),
                                1 => {
                                    offset_is_integer = match info.ty {
                                        InferType::Concrete(ty) => ty.is_integer(),
                                        InferType::Var(id) => self.int_literal_vars.contains(&id),
                                        InferType::IntLiteral => true,
                                        InferType::Array { .. } => false,
                                    };
                                }
                                _ => {}
                            }
                        }
                    }
                    if !offset_is_integer {
                        pointer_ty = None;
                    }
                    pointer_ty.map_or_else(|| InferType::Var(self.fresh_var()), InferType::Concrete)
                } else if intrinsic_name == "place" {
                    // The trusted `@place(ptr)` bridge is represented as a
                    // pointer-shaped expression until accessor-yield analysis
                    // turns it into an indirect place.
                    for arg_ref in args.iter() {
                        generate_intrinsic_arg!(*arg_ref);
                    }
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "raw"
                    || intrinsic_name == "raw_mut"
                    || intrinsic_name == "field_ptr"
                {
                    // @raw / @raw_mut / @field_ptr: takes a place, returns a
                    // pointer to it (RUE-301). If the operand is a concrete
                    // local/parameter place, publish the exact interned
                    // pointee now. Computed and module/constant operands stay
                    // free so sema retains ownership of its place diagnostic.
                    let typed_shape = args.len() == 1;
                    let mut pointee = None;
                    for (index, arg_ref) in args.iter().enumerate() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        let is_field_place =
                            matches!(self.rir.get(*arg_ref).data, InstData::FieldGet { .. });
                        if typed_shape
                            && index == 0
                            && self.is_inference_place(*arg_ref, ctx)
                            && (intrinsic_name != "field_ptr" || is_field_place)
                        {
                            pointee = self.concrete_type(&info.ty);
                        }
                    }
                    match pointee {
                        Some(pointee) if intrinsic_name == "raw" => InferType::Concrete(
                            Type::new_ptr_const(self.type_pool.intern_ptr_const_from_type(pointee)),
                        ),
                        Some(pointee) => InferType::Concrete(Type::new_ptr_mut(
                            self.type_pool.intern_ptr_mut_from_type(pointee),
                        )),
                        None => InferType::Var(self.fresh_var()),
                    }
                } else if intrinsic_name == "alloc" || intrinsic_name == "alloc_zeroed" {
                    // @alloc(size: u64, align: u64) -> ptr mut u8 and its
                    // zeroing twin (ADR-0059 Phase 3, RUE-961/RUE-968). Both
                    // operands are physical byte counts, so both are u64 and
                    // the result type is fixed rather than context-inferred.
                    for arg_ref in args.iter() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        self.add_constraint(Constraint::equal(
                            info.ty,
                            InferType::Concrete(Type::U64),
                            info.span,
                        ));
                    }
                    InferType::Concrete(Type::new_ptr_mut(
                        self.type_pool.intern_ptr_mut_from_type(Type::U8),
                    ))
                } else if intrinsic_name == "realloc" || intrinsic_name == "resize" {
                    // @realloc(p, old_size, align, new_size) -> ptr mut u8 and
                    // @resize(p, old_size, align, new_size) -> bool share one
                    // operand shape: a `ptr mut u8` block plus three u64 byte
                    // counts (ADR-0059 Phase 3, RUE-961/RUE-968).
                    let ptr_ty =
                        Type::new_ptr_mut(self.type_pool.intern_ptr_mut_from_type(Type::U8));
                    for (i, arg_ref) in args.iter().enumerate() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        let expected = if i == 0 { ptr_ty } else { Type::U64 };
                        self.add_constraint(Constraint::equal(
                            info.ty,
                            InferType::Concrete(expected),
                            info.span,
                        ));
                    }
                    InferType::Concrete(if intrinsic_name == "resize" {
                        Type::BOOL
                    } else {
                        ptr_ty
                    })
                } else if intrinsic_name == "free" {
                    // @free(p: ptr mut u8, size: u64, align: u64) -> ().
                    let ptr_ty =
                        Type::new_ptr_mut(self.type_pool.intern_ptr_mut_from_type(Type::U8));
                    for (i, arg_ref) in args.iter().enumerate() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        let expected = if i == 0 { ptr_ty } else { Type::U64 };
                        self.add_constraint(Constraint::equal(
                            info.ty,
                            InferType::Concrete(expected),
                            info.span,
                        ));
                    }
                    InferType::Concrete(Type::UNIT)
                } else if intrinsic_name == "byte_copy" || intrinsic_name == "byte_move" {
                    // @byte_copy/@byte_move(dst: ptr mut u8,
                    // src: ptr const u8 | ptr mut u8, size: u64) -> (). Constrain
                    // dst to `ptr mut u8` and size to u64; the source pointer may
                    // be const or mut u8, so it is left to sema's
                    // `require_u8_pointer` rather than pinned here. The two
                    // differ only in their overlap contract (RUE-964), which
                    // inference does not see.
                    let ptr_ty =
                        Type::new_ptr_mut(self.type_pool.intern_ptr_mut_from_type(Type::U8));
                    for (i, arg_ref) in args.iter().enumerate() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        let expected = match i {
                            0 => Some(ptr_ty),
                            2 => Some(Type::U64),
                            _ => None,
                        };
                        if let Some(expected) = expected {
                            self.add_constraint(Constraint::equal(
                                info.ty,
                                InferType::Concrete(expected),
                                info.span,
                            ));
                        }
                    }
                    InferType::Concrete(Type::UNIT)
                } else if intrinsic_name == "byte_set" {
                    // @byte_set(dst: ptr mut u8, byte: u8, size: u64) -> ().
                    let ptr_ty =
                        Type::new_ptr_mut(self.type_pool.intern_ptr_mut_from_type(Type::U8));
                    for (i, arg_ref) in args.iter().enumerate() {
                        let info = generate_intrinsic_arg!(*arg_ref);
                        let expected = match i {
                            0 => Some(ptr_ty),
                            1 => Some(Type::U8),
                            2 => Some(Type::U64),
                            _ => None,
                        };
                        if let Some(expected) = expected {
                            self.add_constraint(Constraint::equal(
                                info.ty,
                                InferType::Concrete(expected),
                                info.span,
                            ));
                        }
                    }
                    InferType::Concrete(Type::UNIT)
                } else if intrinsic_name == "int_to_ptr" {
                    // @int_to_ptr: returns a pointer type inferred from context
                    for arg_ref in args.iter() {
                        generate_intrinsic_arg!(*arg_ref);
                    }
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "target_arch" {
                    // @target_arch: returns Arch enum
                    if let Some(arch_spur) = self.interner.get("Arch") {
                        if let Some(arch_ty) = self.builtin_enum_type(arch_spur) {
                            InferType::Concrete(arch_ty)
                        } else {
                            InferType::Concrete(Type::ERROR)
                        }
                    } else {
                        InferType::Concrete(Type::ERROR)
                    }
                } else if intrinsic_name == "target_os" {
                    // @target_os: returns Os enum
                    if let Some(os_spur) = self.interner.get("Os") {
                        if let Some(os_ty) = self.builtin_enum_type(os_spur) {
                            InferType::Concrete(os_ty)
                        } else {
                            InferType::Concrete(Type::ERROR)
                        }
                    } else {
                        InferType::Concrete(Type::ERROR)
                    }
                } else if intrinsic_name == "target_data_model" {
                    // @target_data_model: returns DataModel enum
                    if let Some(dm_spur) = self.interner.get("DataModel") {
                        if let Some(dm_ty) = self.builtin_enum_type(dm_spur) {
                            InferType::Concrete(dm_ty)
                        } else {
                            InferType::Concrete(Type::ERROR)
                        }
                    } else {
                        InferType::Concrete(Type::ERROR)
                    }
                } else if intrinsic_name == "import" {
                    // @import("path"): a module value. Resolving the path to a
                    // real ModuleId needs the registry, which inference doesn't
                    // have, so use the documented sentinel id — inference only
                    // needs module-ness; sema resolves the member with the
                    // receiver's real module/file identity during analysis.
                    // Returning Unit here (the old catch-all) made a member
                    // call on the binding unresolvable (RUE-142) and let a
                    // bare module expression coerce to `()` silently.
                    for arg_ref in args.iter() {
                        generate_intrinsic_arg!(*arg_ref);
                    }
                    InferType::Concrete(Type::new_module(crate::types::ModuleId::UNRESOLVED))
                } else if intrinsic_name == "dbg"
                    || intrinsic_name == "drop"
                    || intrinsic_name == "test_preview_gate"
                {
                    // The remaining known intrinsics all return unit.
                    for arg_ref in args.iter() {
                        generate_intrinsic_arg!(*arg_ref);
                    }
                    InferType::Concrete(Type::UNIT)
                } else {
                    // Unknown intrinsic: a fresh var, so sema can reject it with
                    // E0700 naming the bogus intrinsic instead of inference
                    // masking it with a type-mismatch against the context's
                    // expected type — the same treatment @cast gets (RUE-319,
                    // here RUE-1281).
                    for arg_ref in args.iter() {
                        generate_intrinsic_arg!(*arg_ref);
                    }
                    InferType::Var(self.fresh_var())
                };
                // Every intrinsic evaluates its operands strictly in source
                // order.  Keep that control fact separate from the intrinsic's
                // ordinary value type (notably `@assert`, which remains unit).
                continues &= args_reachable;
                intrinsic_ty
            }

            InstData::InternalIntrinsic { intrinsic, args } => {
                let args = self.rir.internal_intrinsic_args(args);
                let mut args_continue = true;
                for arg_ref in args {
                    self.note_sibling_attempt();
                    if self.was_canceled() {
                        break;
                    }
                    let info = self.generate(arg_ref, ctx);
                    args_continue &= info.continues;
                    if self.was_canceled() {
                        break;
                    }
                }
                continues &= args_continue;
                match intrinsic {
                    // The loop bound and next byte offset are usize (u64).
                    rue_rir::InternalIntrinsic::IterLen
                    | rue_rir::InternalIntrinsic::CharNext
                    | rue_rir::InternalIntrinsic::CharNextLossy => InferType::Concrete(Type::U64),
                    // Character iteration exposes Unicode scalar values.
                    rue_rir::InternalIntrinsic::CharScalar
                    | rue_rir::InternalIntrinsic::CharScalarLossy => InferType::Concrete(Type::U32),
                }
            }

            // Type intrinsic (@size_of, @align_of, @int_max, @int_min)
            InstData::TypeIntrinsic { name, type_arg } => {
                match self.interner.resolve(name) {
                    // The integer-bounds intrinsics evaluate to a value of the
                    // queried type itself (RUE-694): `@int_max(u8): u8`. When
                    // the type argument doesn't resolve here (a generic `T`
                    // before substitution) — or resolves to a non-integer,
                    // which sema rejects as E0702 — leave a fresh variable so
                    // sema stays authoritative for both the type and the
                    // diagnostic.
                    "int_max" | "int_min" => self
                        .infer_rir_type_hint(*type_arg, span.file_id)
                        .filter(|ty| matches!(ty, InferType::Concrete(ty) if ty.is_integer()))
                        .unwrap_or_else(|| InferType::Var(self.fresh_var())),
                    // The remaining type intrinsics return i32 (the size or
                    // alignment value).
                    _ => InferType::Concrete(Type::I32),
                }
            }

            // Field-offset intrinsic (@offset_of) — the compile-time byte
            // offset of a field within a struct (RUE-301). Returns u64, like
            // Rust's `core::mem::offset_of!`.
            InstData::OffsetOf {
                type_arg: _,
                field: _,
            } => InferType::Concrete(Type::U64),

            // Block
            InstData::Block { instructions } => {
                self.enter_scope(ctx);
                let mut last_ty = InferType::Concrete(Type::UNIT);
                let mut diverged = false;
                let mut diverged_break_stack: Option<Vec<LoopBreakFact>> = None;
                let block_insts = self.rir.block_insts(instructions);
                for block_inst_ref in block_insts.values() {
                    self.note_sibling_attempt();
                    if self.was_canceled() {
                        break;
                    }
                    let info = self.generate(block_inst_ref, ctx);
                    if self.was_canceled() {
                        break;
                    }
                    // Keep visiting unreachable instructions so inference can
                    // report errors in them, but the first genuinely
                    // diverging instruction makes every later tail
                    // unreachable. The block therefore has the bottom type,
                    // rather than the parser's synthetic unit tail. This is
                    // the HM counterpart of sema's outgoing-state `⊥` and
                    // lets a semicolon-terminated `@panic`/`return` inhabit
                    // any surrounding value context.
                    if !diverged {
                        diverged = !info.continues;
                        last_ty = info.ty;
                        if diverged {
                            diverged_break_stack = Some(ctx.loop_break_stack.clone());
                        }
                    }
                }
                if let Some(reachable_facts) = diverged_break_stack {
                    restore_reachable_break_facts(&mut ctx.loop_break_stack, &reachable_facts);
                }
                self.exit_scope(ctx);
                if diverged {
                    continues = false;
                    InferType::Concrete(Type::NEVER)
                } else {
                    last_ty
                }
            }

            // Branch (if/else)
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let cond_info = self.generate(*cond, ctx);
                let reachable_facts_after_condition = ctx.loop_break_stack.clone();
                continues &= cond_info.continues;
                self.add_constraint(Constraint::equal(
                    cond_info.ty,
                    InferType::Concrete(Type::BOOL),
                    cond_info.span,
                ));

                // The first inference pass is a selector probe. Its purpose
                // is only to solve the selector's operands; branch result
                // joins are intentionally deferred until the canonical engine
                // has supplied a selection fact.
                if self.staged_comptime_selectors
                    && self
                        .comptime_selections
                        .is_none_or(|facts| !facts.contains_key(&inst_ref))
                    && self
                        .comptime_values
                        .is_some_and(|values| !values.is_empty())
                {
                    let result_ty = InferType::Var(self.fresh_var());
                    self.record_type(inst_ref, result_ty.clone());
                    return ExprInfo::with_continues(result_ty, span, cond_info.continues);
                }

                // A frontier pass must not descend through a selector whose
                // fact is already known: doing so would replay every selected
                // descendant once for each ancestor.  The fact walk enqueues
                // that selected body as the next bounded checkpoint.
                if self.comptime_frontier_mode
                    && self
                        .comptime_selections
                        .is_some_and(|facts| facts.contains_key(&inst_ref))
                {
                    let result_ty = InferType::Var(self.fresh_var());
                    self.record_type(inst_ref, result_ty.clone());
                    return ExprInfo::with_continues(result_ty, span, cond_info.continues);
                }

                // Comptime-known condition (spec 4.14:17): inside a
                // specialization whose comptime value params make the condition
                // compile-time evaluable, only the taken branch is analyzed —
                // the untaken branch may be legal only for other
                // specializations, exactly as sema's `analyze_branch` prunes it
                // (RUE-554). This mirrors the `Match` arm's comptime selection;
                // the `comptime_values.is_some()` gate (the analog of sema's
                // non-empty `comptime_value_vars`) keeps ordinary `if`s fully
                // constrained even when the condition is a literal, and the
                // cond above is still constrained to `bool` on every path.
                if let Some(ComptimeSelection::Branch { taken }) = self
                    .comptime_selections
                    .and_then(|facts| facts.get(&inst_ref))
                {
                    let taken = *taken;
                    let selected = if taken {
                        Some(*then_block)
                    } else {
                        *else_block
                    };
                    let result_ty = match selected {
                        Some(block) => {
                            self.enter_scope(ctx);
                            let info = self.generate(block, ctx);
                            self.exit_scope(ctx);
                            if self.was_canceled() {
                                return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                            }
                            info.ty
                        }
                        // `if false { .. }` with no else: nothing runs, unit.
                        None => InferType::Concrete(Type::UNIT),
                    };
                    self.record_type(inst_ref, result_ty.clone());
                    return ExprInfo::with_continues(
                        result_ty,
                        span,
                        cond_info.continues
                            && selected.is_none_or(|block| {
                                self.expr_continues.get(&block).copied().unwrap_or(true)
                            }),
                    );
                }

                let then_info = self.generate(*then_block, ctx);
                if self.was_canceled() {
                    return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                }

                let branch_ty = if let Some(else_ref) = else_block {
                    let else_info = self.generate(*else_ref, ctx);
                    if self.was_canceled() {
                        return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                    }

                    // Handle Never type coercion:
                    // - If one branch is Never, the if-else takes the other branch's type
                    // - If both are Never, the result is Never
                    // - Otherwise, both must unify to the same type
                    let then_is_never = !then_info.continues;
                    let else_is_never = !else_info.continues;
                    continues &= then_info.continues || else_info.continues;

                    match (then_is_never, else_is_never) {
                        (true, true) => {
                            // Both diverge - result is Never
                            InferType::Concrete(Type::NEVER)
                        }
                        (true, false) => {
                            // Then diverges - result is else type
                            else_info.ty
                        }
                        (false, true) => {
                            // Else diverges - result is then type
                            then_info.ty
                        }
                        (false, false) => {
                            // Neither diverges - both must have the same type
                            let result_var = self.fresh_var();
                            let result_ty = InferType::Var(result_var);
                            self.add_constraint(Constraint::equal(
                                then_info.ty,
                                result_ty.clone(),
                                then_info.span,
                            ));
                            self.add_constraint(Constraint::equal(
                                else_info.ty,
                                result_ty.clone(),
                                else_info.span,
                            ));
                            result_ty
                        }
                    }
                } else {
                    // No else branch - the if expression has unit type
                    // and retains the condition-false continuation even when
                    // the then branch diverges.
                    InferType::Concrete(Type::UNIT)
                };
                if !cond_info.continues {
                    restore_reachable_break_facts(
                        &mut ctx.loop_break_stack,
                        &reachable_facts_after_condition,
                    );
                }
                branch_ty
            }

            // While loop
            InstData::Loop { cond, body } => {
                let cond_info = self.generate(*cond, ctx);
                let reachable_facts_after_condition = ctx.loop_break_stack.clone();
                continues &= cond_info.continues;
                self.add_constraint(Constraint::equal(
                    cond_info.ty,
                    InferType::Concrete(Type::BOOL),
                    cond_info.span,
                ));

                ctx.loop_depth += 1;
                ctx.loop_break_stack.push(LoopBreakFact::default());
                self.generate(*body, ctx);
                ctx.loop_break_stack.pop();
                ctx.loop_depth -= 1;
                if !cond_info.continues {
                    restore_reachable_break_facts(
                        &mut ctx.loop_break_stack,
                        &reachable_facts_after_condition,
                    );
                }

                // Loops produce unit
                InferType::Concrete(Type::UNIT)
            }

            // Infinite loop
            InstData::InfiniteLoop { body, .. } => {
                ctx.loop_depth += 1;
                ctx.loop_break_stack.push(LoopBreakFact::default());
                self.generate(*body, ctx);
                let break_fact = ctx.loop_break_stack.pop().unwrap_or_default();
                ctx.loop_depth -= 1;

                // An infinite loop with a break targeting it exits with unit;
                // without one it never returns (see spec 4.8:17 / 4.8:21).
                if break_fact.syntactic {
                    continues = break_fact.reachable;
                    InferType::Concrete(Type::UNIT)
                } else {
                    continues = false;
                    InferType::Concrete(Type::NEVER)
                }
            }

            // Break/Continue
            InstData::Break { value } => {
                continues = false;
                match value {
                    None => {
                        // Record the break against the innermost enclosing loop.
                        if let Some(break_fact) = ctx.loop_break_stack.last_mut() {
                            break_fact.syntactic = true;
                            break_fact.reachable = true;
                        }
                    }
                    Some(v) => {
                        // A value operand is always rejected by sema (spec
                        // 4.8:21). Don't count it as a loop exit here: keeping
                        // the loop `!`-typed lets sema report the dedicated
                        // break-with-value error instead of inference masking
                        // it with a type mismatch. Still generate constraints
                        // for the operand so inference stays total.
                        self.generate(*v, ctx);
                    }
                }
                InferType::Concrete(Type::NEVER)
            }
            InstData::Continue => {
                continues = false;
                InferType::Concrete(Type::NEVER)
            }

            // Match expression
            InstData::Match { scrutinee, arms } => {
                let scrutinee_info = self.generate(*scrutinee, ctx);
                let reachable_facts_after_scrutinee = ctx.loop_break_stack.clone();
                let arms = self.rir.match_arms(arms);

                if self.staged_comptime_selectors
                    && self
                        .comptime_selections
                        .is_none_or(|facts| !facts.contains_key(&inst_ref))
                    && self
                        .comptime_values
                        .is_some_and(|values| !values.is_empty())
                {
                    let result_ty = InferType::Var(self.fresh_var());
                    self.record_type(inst_ref, result_ty.clone());
                    return ExprInfo::with_continues(result_ty, span, scrutinee_info.continues);
                }

                // See the branch case above.  A selected match arm is a
                // separate source-graph frontier, never a recursive suffix of
                // the current one.
                if self.comptime_frontier_mode
                    && self
                        .comptime_selections
                        .is_some_and(|facts| facts.contains_key(&inst_ref))
                {
                    let result_ty = InferType::Var(self.fresh_var());
                    self.record_type(inst_ref, result_ty.clone());
                    return ExprInfo::with_continues(result_ty, span, scrutinee_info.continues);
                }

                // Comptime-known scrutinee (spec 4.14:19): when the scrutinee
                // is a comptime value known for this specialization, sema
                // selects and analyzes only the matching arm's body. Inference
                // must mirror that here — cross-constraining all arms would
                // reject a valid program whose statically-unselected arm has a
                // deliberately different type (RUE-268). Only the selected arm
                // participates; its body's type is the match's type. Any shape
                // this doesn't understand falls through to the runtime path,
                // which unifies every arm (so a genuine runtime match still
                // errors when arms disagree). This is a strict subset of the
                // scrutinees sema prunes, so the selected arm always has an
                // inferred type when sema later prunes to it.
                if let Some(ComptimeSelection::Match { arm }) = self
                    .comptime_selections
                    .and_then(|facts| facts.get(&inst_ref))
                    && let Some((selected_pattern, selected)) = arms.iter().nth(*arm)
                {
                    self.enter_scope(ctx);
                    self.register_match_bindings(&selected_pattern, ctx);
                    let body_info = self.generate(selected, ctx);
                    self.exit_scope(ctx);
                    if self.was_canceled() {
                        return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                    }
                    if !scrutinee_info.continues {
                        restore_reachable_break_facts(
                            &mut ctx.loop_break_stack,
                            &reachable_facts_after_scrutinee,
                        );
                    }
                    self.record_type(inst_ref, body_info.ty.clone());
                    return ExprInfo::with_continues(
                        body_info.ty,
                        span,
                        scrutinee_info.continues && body_info.continues,
                    );
                }

                continues &= scrutinee_info.continues;

                // Collect arm types, handling Never coercion
                let mut arm_types: Vec<ExprInfo> = Vec::new();
                for (pattern, body) in arms.iter() {
                    self.note_sibling_attempt();
                    if self.was_canceled() {
                        break;
                    }
                    // Patterns constrain the scrutinee type
                    let pattern_ty = self.pattern_type(&pattern);
                    self.add_constraint(Constraint::equal(
                        scrutinee_info.ty.clone(),
                        pattern_ty,
                        pattern.span(),
                    ));

                    // Register any tuple-variant payload bindings as locals
                    // scoped to the arm body, with their declared payload
                    // types, so the body's references resolve during inference
                    // (RUE-221). Without this, `Circle(r) => r` leaves `r`
                    // unbound and its type poisons the match result.
                    self.enter_scope(ctx);
                    self.register_match_bindings(&pattern, ctx);

                    // Generate body and collect its type
                    let body_info = self.generate(body, ctx);
                    self.exit_scope(ctx);
                    arm_types.push(body_info);
                    if self.was_canceled() {
                        break;
                    }
                }

                // Handle Never type coercion:
                // Filter out Never arms and use the remaining non-Never types
                let non_never_arms: Vec<_> =
                    arm_types.iter().filter(|info| info.continues).collect();
                continues &= arm_types.iter().any(|info| info.continues);

                if non_never_arms.is_empty() {
                    // All arms diverge - result is Never
                    if !scrutinee_info.continues {
                        restore_reachable_break_facts(
                            &mut ctx.loop_break_stack,
                            &reachable_facts_after_scrutinee,
                        );
                    }
                    InferType::Concrete(Type::NEVER)
                } else {
                    // Create constraints for non-Never arms to have the same type
                    let result_var = self.fresh_var();
                    let result_ty = InferType::Var(result_var);
                    for arm_info in non_never_arms {
                        self.add_constraint(Constraint::equal(
                            arm_info.ty.clone(),
                            result_ty.clone(),
                            arm_info.span,
                        ));
                    }
                    if !scrutinee_info.continues {
                        restore_reachable_break_facts(
                            &mut ctx.loop_break_stack,
                            &reachable_facts_after_scrutinee,
                        );
                    }
                    result_ty
                }
            }

            // Struct initialization
            InstData::StructInit {
                module,
                ctor_head,
                type_name,
                fields,
                shorthand_span: _,
            } => {
                // Inline type-constructor literal heads (`F(args) { ... }`,
                // RUE-596) resolve through sema's pre-reduced head map, the
                // same way the call-head path does (RUE-599); a head sema
                // could not reduce falls through to the error path below and
                // sema diagnoses it. Module-qualified literals
                // (`m.Point { ... }`) resolve in the module's defining file,
                // matching sema. Unqualified literals check type_subst first
                // (for Self/type parameters), then comptime type aliases
                // (`let P = F(); P { ... }`, RUE-170), then the current
                // file's module-local type table, then builtins.
                let struct_ty = if let Some(head) = ctor_head {
                    self.inline_ctor_head_types
                        .and_then(|heads| heads.get(head).copied())
                        .filter(|ty| ty.as_struct().is_some())
                } else if let Some(module_ref) = module {
                    let module_info = self.generate(*module_ref, ctx);
                    continues &= module_info.continues;
                    let module_id = match module_info.ty {
                        InferType::Concrete(ty) => ty.as_module(),
                        _ => None,
                    };
                    module_id
                        .and_then(|module_id| self.module_file_id(module_id))
                        .and_then(|file_id| self.struct_type_by_file((file_id, *type_name)))
                } else {
                    self.type_subst
                        .and_then(|subst| subst.get(type_name).copied())
                        .or_else(|| self.comptime_alias_types.get(type_name).copied())
                        .or_else(|| {
                            self.struct_type_by_file((span.file_id, *type_name))
                                .or_else(|| self.builtin_struct_type(*type_name))
                        })
                };

                let fields = self.rir.field_inits(fields);
                if let Some(struct_ty) = struct_ty {
                    // Constrain each initializer against its field's declared
                    // type, so literal initializers are range-checked at the
                    // field's width instead of silently wrapping
                    // (`S { a: 300 }` with a: u8 used to truncate to 44).
                    // (RUE-72)
                    for (field_name, value_ref) in fields.values() {
                        let value_info = self.generate_sequenced_operand(value_ref, ctx, continues);
                        continues &= value_info.continues;
                        if self.was_canceled() {
                            break;
                        }
                        if let Some(field_ty) = self.field_type_of(struct_ty, field_name) {
                            let expected = self.type_to_infer(field_ty);
                            // A `str` field (ADR-0043 Phase 3, RUE-324) accepts a
                            // string literal (HM type `String`) by coercion; skip
                            // strict equality and let sema materialize the `str`.
                            if !self.is_slice_struct_type(expected.clone()) {
                                self.add_constraint(Constraint::contextual(
                                    value_info.ty,
                                    expected,
                                    value_info.span,
                                ));
                            }
                        }
                    }
                    InferType::Concrete(struct_ty)
                } else {
                    // Unknown type name — sema reports the error. Still visit
                    // the initializers so every sub-expression gets a type;
                    // skipping them left compound initializers (`-1`, `1+2`)
                    // with unresolved variables, which sema then reported as
                    // an internal compiler error (RUE-170).
                    for (_, value_ref) in fields.values() {
                        let value_info = self.generate_sequenced_operand(value_ref, ctx, continues);
                        continues &= value_info.continues;
                        if self.was_canceled() {
                            break;
                        }
                    }
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Field access
            InstData::FieldGet { base, field } => {
                // `Enum.Variant` (RUE-488): a field access on a bare enum type
                // name is an enum-variant value. Yield the concrete enum type so
                // a mismatch — e.g. returning `Shape.Circle` from a function
                // declared `-> Color` — is caught here instead of a fresh
                // variable silently unifying with the expected type. A comptime
                // type-variable local (typed `COMPTIME_TYPE`) is a type
                // reference, not a runtime value shadow.
                if let InstData::VarRef { name, .. } = self.rir.get(*base).data
                    && !ctx.contains_param(name)
                    && ctx.locals.get(&name).is_none_or(
                        |l| matches!(l.ty, InferType::Concrete(t) if t == Type::COMPTIME_TYPE),
                    )
                    && let Some(enum_ty) = self.enum_type_for(&name, span.file_id)
                    && let Some(enum_id) = enum_ty.as_enum()
                    && self
                        .type_pool
                        .enum_def(enum_id)
                        .find_variant(self.interner.resolve(field))
                        .is_some()
                {
                    return {
                        let ty = InferType::Concrete(enum_ty);
                        self.record_type(inst_ref, ty.clone());
                        ExprInfo::new(ty, span)
                    };
                }

                // Module-qualified `module.Enum.Variant` (RUE-488): the base is
                // `module.Enum`, whose module member is an enum. Yield the
                // concrete enum type so same-named enums from sibling modules stay
                // distinct nominal types (a wrong one flowing into a call argument
                // or return is caught here, not silently unified). RUE-501.
                if let InstData::FieldGet {
                    base: module_ref,
                    field: type_name,
                } = self.rir.get(*base).data
                    && let Some(enum_ty) = self.enum_type_for_module(module_ref, &type_name)
                    && let Some(enum_id) = enum_ty.as_enum()
                    && self
                        .type_pool
                        .enum_def(enum_id)
                        .find_variant(self.interner.resolve(field))
                        .is_some()
                {
                    return {
                        let ty = InferType::Concrete(enum_ty);
                        self.record_type(inst_ref, ty.clone());
                        ExprInfo::new(ty, span)
                    };
                }

                let base_info = self.generate(*base, ctx);
                continues &= base_info.continues;
                if self.was_canceled() {
                    return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                }
                // When the base's struct type is already concrete, the field's
                // declared type is known here — yield it so downstream
                // constraints see the real type instead of a free variable.
                // This prevents literal defaulting from overriding the field
                // width and gives method calls the concrete receiver they
                // require. When the base is still a variable, fall back to a
                // fresh var; sema resolves and diagnoses later.
                // (RUE-89, RUE-126)
                match self.known_field_type(&base_info.ty, *field) {
                    Some(field_ty) => self.type_to_infer(field_ty),
                    None => {
                        // Sema resolves module members in nominal-before-const
                        // order. Mirror the nominal part here: `m.S` and `m.E`
                        // are compile-time type values, not unconstrained
                        // runtime fields. Visibility remains owned by sema;
                        // this lookup only supplies the value category needed
                        // by surrounding inference constraints.
                        let member_nominal_ty = match &base_info.ty {
                            InferType::Concrete(ty) if ty.is_module() => ty
                                .as_module()
                                .and_then(|module_id| self.module_file_id(module_id))
                                .and_then(|file_id| {
                                    self.struct_type_by_file((file_id, *field))
                                        .or_else(|| self.enum_type_by_file((file_id, *field)))
                                })
                                .filter(|member_ty| {
                                    self.nominal_type_accessible(span.file_id, *member_ty)
                                })
                                .map(|_| Type::COMPTIME_TYPE),
                            _ => None,
                        };
                        // Module receiver: `m.CONST` resolves to a module
                        // member value-constant; yield its declared type so
                        // uses like `m.CONST + 1` are anchored (RUE-160).
                        // Resolved authoritatively in the receiver module's
                        // defining file (RUE-140, RUE-638).
                        let member_const = match &base_info.ty {
                            InferType::Concrete(ty) if ty.is_module() => ty
                                .as_module()
                                .and_then(|module_id| self.module_file_id(module_id))
                                .and_then(|file_id| {
                                    self.const_type((file_id, *field)).map(|ty| (file_id, ty))
                                }),
                            _ => None,
                        };
                        let member_const_ty = member_const.map(|(_, ty)| ty);
                        // A module member that is itself a (re-exported) module:
                        // `std.cmp` where std's file has `pub const cmp =
                        // @import("cmp.rue")`. Resolve it to the member module's
                        // type so a nested call `std.cmp.min(..)` reaches the
                        // module-member call path and its arguments are
                        // constrained to the callee's parameters — without this
                        // the receiver was a fresh variable, so a generic std call
                        // with a literal (`std.cmp.min(i64, 3, 7)`) left the
                        // literal unconstrained and defaulted to i32 (RUE-693).
                        let member_module_ty = match &base_info.ty {
                            InferType::Concrete(ty) if ty.is_module() => ty
                                .as_module()
                                .and_then(|module_id| self.module_file_id(module_id))
                                .and_then(|file_id| self.module_binding_type((file_id, *field))),
                            _ => None,
                        };
                        match member_nominal_ty.or(member_const_ty).or(member_module_ty) {
                            Some(member_ty) => self.type_to_infer(member_ty),
                            None => InferType::Var(self.fresh_var()),
                        }
                    }
                }
            }

            // Field assignment
            InstData::FieldSet { base, field, value } => {
                let value_info = self.generate(*value, ctx);
                if self.was_canceled() {
                    return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                }
                let base_info = self.generate_sequenced_operand(*base, ctx, value_info.continues);
                if self.was_canceled() {
                    return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                }
                continues &= base_info.continues && value_info.continues;
                // Constrain the assigned value against the field's declared
                // type, so a literal RHS is range-checked at the field's width
                // instead of wrapping (`s.a = 300` with `a: u8` must be
                // rejected rather than truncate to 44). (RUE-104)
                if let Some(field_ty) = self.known_field_type(&base_info.ty, *field) {
                    let expected = self.type_to_infer(field_ty);
                    if value_info.continues {
                        self.add_constraint(Constraint::contextual(
                            value_info.ty,
                            expected,
                            value_info.span,
                        ));
                    }
                }
                InferType::Concrete(Type::UNIT)
            }

            // Enum variant
            InstData::EnumVariant {
                module, type_name, ..
            } => {
                let enum_ty = module
                    .and_then(|module_ref| self.enum_type_for_module(module_ref, type_name))
                    .or_else(|| self.enum_type_for(type_name, span.file_id));
                if let Some(enum_ty) = enum_ty {
                    InferType::Concrete(enum_ty)
                } else {
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Array initialization
            InstData::ArrayInit { elements } => {
                let elements = self.rir.array_elements(elements);
                if elements.is_empty() {
                    // Empty array - need type annotation to know element type
                    // Use a fresh type variable for the element type
                    let elem_var = self.fresh_var();
                    InferType::Array {
                        element: Box::new(InferType::Var(elem_var)),
                        length: 0,
                    }
                } else {
                    // Get element type from first element, constrain rest to match
                    let first_info =
                        self.generate_sequenced_operand(elements.get(0).unwrap(), ctx, true);
                    continues &= first_info.continues;
                    if self.was_canceled() {
                        return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                    }
                    for elem_ref in elements.values().skip(1) {
                        let elem_info = self.generate_sequenced_operand(elem_ref, ctx, continues);
                        continues &= elem_info.continues;
                        if self.was_canceled() {
                            break;
                        }
                        self.add_constraint(Constraint::equal(
                            elem_info.ty,
                            first_info.ty.clone(),
                            elem_info.span,
                        ));
                    }
                    // Build the array type with the inferred element type
                    InferType::Array {
                        element: Box::new(first_info.ty),
                        length: elements.len() as u64,
                    }
                }
            }

            // Array-repeat literal `[value; count]` (RUE-235). The array type
            // is `[typeof value; count]`; every slot has the value's type. The
            // count is a compile-time constant resolved here: a literal, a
            // comptime value parameter of the specialization being analyzed
            // (`fn make(comptime n: u64) -> i32 { [0; n][0] }`, spec 7.1:37),
            // or a file-level `const`. A count that still doesn't resolve
            // yields a fresh variable and is resolved/diagnosed by sema.
            InstData::ArrayRepeat { value, count } => {
                let value_info = self.generate(*value, ctx);
                continues &= value_info.continues;
                let resolved = match count {
                    RepeatCount::Literal(n) => Some(*n),
                    // A comptime value parameter captured at the call site
                    // takes precedence over a file-level `const` of the same
                    // name, the same precedence array-TYPE lengths use
                    // (RUE-252). Without the value-parameter half the repeat's
                    // type stayed an unconstrained variable that decayed to
                    // `<error>`, and sema reported the array-repeat literal as
                    // an un-annotatable empty array (E0903, RUE-1681).
                    RepeatCount::Named(sym) => self
                        .comptime_value_int(*sym)
                        .or_else(|| self.const_value((span.file_id, *sym)))
                        .and_then(|v| u64::try_from(v).ok()),
                };
                match resolved {
                    Some(length) => InferType::Array {
                        element: Box::new(value_info.ty),
                        length,
                    },
                    None => InferType::Var(self.fresh_var()),
                }
            }

            // Array index
            InstData::IndexGet { base, index } => {
                let base_info = self.generate(*base, ctx);
                let index_info = self.generate(*index, ctx);
                continues &= base_info.continues && index_info.continues;
                // Index must be an integer type (signed or unsigned) per spec
                // 7.1:7. A negative or out-of-range index is not a type error;
                // it is caught at runtime by the bounds check (unsigned 64-bit
                // compare, so negatives fail and trap — RUE-81/RUE-87).
                self.add_constraint(Constraint::is_integer(index_info.ty, index_info.span));

                // Extract element type from array type.
                // If base is InferType::Array, we can get the element type directly.
                // Otherwise, we need a fresh variable that will be resolved later.
                if self.is_string_indexable_type(&base_info.ty) {
                    InferType::Concrete(Type::U8)
                } else {
                    match &base_info.ty {
                        InferType::Array { element, .. } => (**element).clone(),
                        // An interned array or slice type carries its element
                        // type too; publish it rather than letting a literal
                        // beside the element default to i32 (see
                        // `concrete_element_type`).
                        _ => match self.concrete_element_type(&base_info.ty) {
                            Some(element_type) => self.type_to_infer(element_type),
                            None => {
                                // Base might be a type variable that will resolve to an array.
                                // Use a fresh variable for the element type.
                                let result_var = self.fresh_var();
                                InferType::Var(result_var)
                            }
                        },
                    }
                }
            }

            // Array index assignment
            InstData::IndexSet { base, index, value } => {
                let value_info = self.generate(*value, ctx);
                if self.was_canceled() {
                    return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                }
                let base_info = self.generate_sequenced_operand(*base, ctx, value_info.continues);
                if self.was_canceled() {
                    return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                }
                let index_info = self.generate_sequenced_operand(
                    *index,
                    ctx,
                    value_info.continues && base_info.continues,
                );
                if self.was_canceled() {
                    return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
                }
                // Index must be an integer type (signed or unsigned) per spec
                // 7.1:7. Negative/out-of-range indices trap at runtime via the
                // bounds check, not at compile time (RUE-81/RUE-87).
                self.add_constraint(Constraint::is_integer(index_info.ty, index_info.span));

                continues &= base_info.continues && index_info.continues && value_info.continues;

                // Constrain value type to match array element type
                if let InferType::Array { element, .. } = &base_info.ty {
                    if value_info.continues {
                        self.add_constraint(Constraint::equal(
                            value_info.ty,
                            (**element).clone(),
                            value_info.span,
                        ));
                    }
                }

                InferType::Concrete(Type::UNIT)
            }

            // Type declarations don't produce values
            InstData::FnDecl { .. }
            | InstData::StructDecl { .. }
            | InstData::EnumDecl { .. }
            | InstData::DropFnDecl { .. }
            | InstData::ConstDecl { .. } => InferType::Concrete(Type::UNIT),

            // Method call: receiver.method(args)
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                // `Type.method(args)` where the receiver names a type is an
                // associated-function call / enum tuple-variant construction
                // (RUE-488): forward to the type-qualified path so arguments are
                // constrained to the callee signature (a bare `MethodCall` arm
                // would leave a literal like the `8` in `StrBuf.with_capacity(8)`
                // unconstrained, defaulting it to `i32` and later clashing with a
                // `u64` parameter). Skip when a runtime value (a parameter, or a
                // local that is not itself a comptime type value) shadows the type
                // name — that is an ordinary value-method call. A comptime
                // type-variable local (`let O = Option(i32)`, typed
                // `COMPTIME_TYPE`) IS a type reference, so `O.Some(true)` resolves
                // to its concrete enum and a wrong payload type is caught here.
                if let InstData::VarRef { name, .. } = self.rir.get(*receiver).data
                    && !ctx.contains_param(name)
                    && ctx.locals.get(&name).is_none_or(
                        |l| matches!(l.ty, InferType::Concrete(t) if t == Type::COMPTIME_TYPE),
                    )
                    && (self
                        .struct_type_for(&name, span.file_id)
                        .and_then(|t| t.as_struct())
                        .is_some()
                        || self.enum_type_for(&name, span.file_id).is_some())
                {
                    return {
                        let ty = self.generate_type_qualified_call(name, *method, args, span, ctx);
                        self.record_type(inst_ref, ty.clone());
                        ExprInfo::with_continues(
                            ty.clone(),
                            span,
                            self.call_args_continue(args) && !Self::is_never_concrete(&ty),
                        )
                    };
                }

                // `module.Type.assoc_fn(...)`: the receiver is a module
                // member naming a struct/enum type declared in the module's
                // file. Route it through the type-qualified path so the
                // call's arguments and result are constrained — the FieldGet
                // arm yields a fresh variable for a type member (types are
                // declarations, not constants), which left integer-literal
                // operands used with the result to default to i32 and
                // zero-extend against the callee's real 64-bit value
                // (RUE-633 family). Skipped when the module name is shadowed
                // by a runtime binding (`local.field.method()` stays a
                // value-method chain).
                if let InstData::FieldGet {
                    base: module_ref,
                    field: type_name,
                } = self.rir.get(*receiver).data
                    && !matches!(self.rir.get(module_ref).data,
                        InstData::VarRef { name, .. }
                            if ctx.locals.contains_key(&name) || ctx.contains_param(name))
                    && let Some(member_ty) = self
                        .struct_type_for_module(module_ref, &type_name)
                        .or_else(|| self.enum_type_for_module(module_ref, &type_name))
                    && let Some(result) =
                        self.generate_call_on_reduced_type(member_ty, *method, args, span, ctx)
                {
                    self.record_type(inst_ref, result.clone());
                    return ExprInfo::with_continues(
                        result.clone(),
                        span,
                        self.call_args_continue(args) && !Self::is_never_concrete(&result),
                    );
                }

                // Generate type for receiver
                let receiver_info = self.generate(*receiver, ctx);
                let call_args = self.rir.call_args(args);
                let mut arg_diverged = !receiver_info.continues;

                // A string literal is otherwise defaulted only after solving,
                // but method lookup needs a receiver type while constraints
                // are still being generated. Use the canonical source StrBuf
                // method signature as contextual information for buffer-only
                // methods. `len` is shared by `str` and StrBuf and may use the
                // same result signature without forcing the receiver away from
                // its stable `str` default.
                if self.is_string_literal_candidate(&receiver_info.ty)
                    && let InferType::Concrete(string_ty) = self.string_infer_type()
                    && let Some(string_id) = string_ty.as_struct()
                    && let Some(method_sig) = self.method_sig(&(string_id, *method))
                {
                    let shared_str_method = self.string_literal_default_is_str()
                        && self.interner.resolve(method) == "len";
                    if !shared_str_method {
                        self.add_constraint(Constraint::equal(
                            receiver_info.ty.clone(),
                            InferType::Concrete(string_ty),
                            receiver_info.span,
                        ));
                    }
                    let param_types = method_sig.param_types.clone();
                    let return_type = method_sig.return_type.clone();
                    for (arg, param_type) in call_args.iter().zip(param_types.iter()) {
                        let arg_info =
                            self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                        arg_diverged |= !arg_info.continues;
                        if self.was_canceled() {
                            break;
                        }
                        self.add_constraint(Constraint::equal(
                            arg_info.ty,
                            param_type.clone(),
                            arg_info.span,
                        ));
                    }
                    self.record_type(inst_ref, return_type.clone());
                    return ExprInfo::with_continues(
                        return_type.clone(),
                        span,
                        !arg_diverged && !Self::is_never_concrete(&return_type),
                    );
                }

                // `len()` on a string literal whose text type is still
                // contextual. Its result is `u64` whichever type the literal
                // settles on — `str` (3.7:45) and `Str(N)` (3.7:52) by rule,
                // StrBuf by its source-defined signature — so publish that
                // result without pinning the receiver, which keeps its stable
                // `str` default. The StrBuf-signature
                // block above answers the same call only when the program
                // imports StrBuf; without that import the receiver stayed a
                // bare type variable, the call fell through to a fresh variable
                // that unified with any annotation, and `let n: i32 =
                // "hi".len();` reached CFG verification as a `u64` value in an
                // `i32` slot (RUE-1679, the RUE-1611 class on the string rungs).
                if self.is_string_literal_candidate(&receiver_info.ty)
                    && self.is_string_literal_len_call(*method, call_args.len())
                {
                    let return_type = InferType::Concrete(Type::U64);
                    self.record_type(inst_ref, return_type.clone());
                    return ExprInfo::with_continues(return_type, span, receiver_info.continues);
                }

                // Resolve the call's result type from the receiver's type.
                // When the receiver cannot yet be resolved but sema will
                // resolve it later, yield a fresh variable rather than ERROR:
                // a Concrete(ERROR) here flowed into sibling constraints and
                // produced bogus "literal out of range for '<error>'" failures
                // that masked (or preempted) sema's real analysis (RUE-126
                // treats FieldGet the same way).
                let result_type = match &receiver_info.ty {
                    // Module receiver: `m.go(...)` is a module member call.
                    // Resolve the member in the receiver module's file first
                    // (RUE-576): same-named functions across files carry
                    // module-qualified internal keys in `functions`. Yielding
                    // the exact member's return type anchors uses like
                    // `m.go() + 1` to the member declaration (RUE-142).
                    InferType::Concrete(ty) if ty.is_module() => {
                        let module_id = ty.as_module().expect("module type has a module id");
                        let module_file = self.module_file_id(module_id);
                        let alias_target = module_file
                            .and_then(|file_id| self.const_function_alias((file_id, *method)));
                        let function_key = alias_target.or_else(|| {
                            module_file
                                .and_then(|file_id| self.function_by_file((file_id, *method)))
                        });
                        if let Some(func) = function_key.and_then(|key| self.func_sig(key)) {
                            if !func.is_generic && call_args.len() == func.param_types.len() {
                                // Constrain each argument against its declared
                                // parameter type (same as a direct Call).
                                for (arg, param_ty) in call_args.iter().zip(func.param_types.iter())
                                {
                                    let arg_info = self.generate_sequenced_operand(
                                        arg.value,
                                        ctx,
                                        !arg_diverged,
                                    );
                                    arg_diverged |= !arg_info.continues;
                                    if self.was_canceled() {
                                        break;
                                    }
                                    // Slice and `borrow str` parameters coerce
                                    // from a `borrow` argument; skip strict
                                    // equality and let sema materialize the
                                    // fat-pointer view (ADR-0043, RUE-322,
                                    // RUE-559) — same as the direct-Call path.
                                    if self.is_slice_struct_type(param_ty.clone()) {
                                        continue;
                                    }
                                    self.add_constraint(Constraint::contextual(
                                        arg_info.ty,
                                        param_ty.clone(),
                                        arg_info.span,
                                    ));
                                }
                            } else if func.is_generic {
                                // Generic module-member callee (RUE-693): mirror
                                // the direct-Call generic path. Build the type
                                // substitution from the comptime type arguments,
                                // then constrain each runtime argument to its
                                // *substituted* parameter type. Without this, a
                                // literal argument to `h.min(i64, 3, 7)` stayed
                                // unconstrained across the module boundary and
                                // defaulted to i32, clashing with the instantiated
                                // i64 parameter (the non-generic path above already
                                // constrains, and same-file generic Calls do too).
                                let mut arg_infos = Vec::with_capacity(call_args.len());
                                for arg in call_args.iter() {
                                    let info = self.generate_sequenced_operand(
                                        arg.value,
                                        ctx,
                                        !arg_diverged,
                                    );
                                    arg_diverged |= !info.continues;
                                    arg_infos.push(info);
                                    if self.was_canceled() {
                                        break;
                                    }
                                }
                                let mut type_subst: AHashMap<lasso::Spur, Type> = AHashMap::new();
                                let mut value_subst: AHashMap<lasso::Spur, i128> = AHashMap::new();
                                for (i, arg) in call_args.iter().enumerate() {
                                    if self.was_canceled() {
                                        break;
                                    }
                                    if i >= func.param_comptime.len()
                                        || !func.param_comptime[i]
                                        || i >= func.param_names.len()
                                    {
                                        continue;
                                    }
                                    if func.param_comptime_type.get(i) == Some(&true) {
                                        if let Some(concrete_ty) =
                                            self.extract_type_argument(arg.value, ctx)
                                        {
                                            type_subst.insert(func.param_names[i], concrete_ty);
                                        }
                                    } else if let Some(ConstValue::Integer(v)) =
                                        self.comptime_argument_value(arg.value)
                                    {
                                        value_subst.insert(func.param_names[i], v);
                                    }
                                }
                                for (i, arg_info) in arg_infos.iter().enumerate() {
                                    if self.was_canceled() {
                                        break;
                                    }
                                    if i >= func.param_types.len() || i >= func.param_comptime.len()
                                    {
                                        break;
                                    }
                                    if func.param_comptime_type.get(i) == Some(&true) {
                                        continue;
                                    }
                                    let declared = &func.param_types[i];
                                    if self.staged_comptime_selectors
                                        && value_subst.is_empty()
                                        && *declared == InferType::Concrete(Type::COMPTIME_TYPE)
                                    {
                                        continue;
                                    }
                                    let expected =
                                        if *declared == InferType::Concrete(Type::COMPTIME_TYPE) {
                                            match func.param_type_syntax.get(i).and_then(|syntax| {
                                                syntax.as_ref().and_then(|syntax| {
                                                    self.infer_structured_type_hint(
                                                        syntax,
                                                        &type_subst,
                                                        &value_subst,
                                                        span.file_id,
                                                    )
                                                })
                                            }) {
                                                Some(ty) => ty,
                                                None => continue,
                                            }
                                        } else {
                                            declared.clone()
                                        };
                                    if self.is_slice_struct_type(expected.clone()) {
                                        continue;
                                    }
                                    self.add_constraint(Constraint::equal(
                                        arg_info.ty.clone(),
                                        expected,
                                        arg_info.span,
                                    ));
                                }
                            } else {
                                // Arity mismatch: just process the arguments;
                                // sema checks the rest.
                                for arg in call_args.iter() {
                                    let info = self.generate_sequenced_operand(
                                        arg.value,
                                        ctx,
                                        !arg_diverged,
                                    );
                                    arg_diverged |= !info.continues;
                                    if self.was_canceled() {
                                        break;
                                    }
                                }
                            }
                            if func.return_type == InferType::Concrete(Type::COMPTIME_TYPE) {
                                // Generic return type that can't be resolved
                                // here; sema specialization determines it.
                                InferType::Var(self.fresh_var())
                            } else {
                                func.return_type.clone()
                            }
                        } else {
                            // Unknown member - sema reports UndefinedFunction
                            for arg in call_args.iter() {
                                let info =
                                    self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                                arg_diverged |= !info.continues;
                                if self.was_canceled() {
                                    break;
                                }
                            }
                            InferType::Concrete(Type::ERROR)
                        }
                    }
                    // Receiver of an unresolved comptime-generic type: e.g.
                    // `pick(t, x).get()` where `t` is a local holding a type
                    // value, so the generic Call arm couldn't substitute the
                    // return type. Sema's comptime evaluation resolves the
                    // receiver and types the call during analysis; an ERROR
                    // here poisoned inference first (RUE-119 family).
                    //
                    // An inline type-constructor head (`Result(i64, i32)
                    // .Ok(41)`, RUE-596) is the exception: sema pre-reduced it
                    // in `inline_ctor_head_types`, so constrain the
                    // construction's arguments against the variant payload /
                    // assoc-fn signature exactly like the bound-alias form —
                    // an unconstrained integer payload literal otherwise
                    // defaulted to `i32` and could not satisfy a wider
                    // declared payload type (RUE-599).
                    InferType::Concrete(ty) if *ty == Type::COMPTIME_TYPE => {
                        // `module.Type.assoc_fn(...)`: the receiver is a
                        // module member naming a struct/enum type. Resolve it
                        // in the module's defining file and constrain the
                        // call like any type-qualified call — leaving it a
                        // fresh variable let integer-literal operands used
                        // with the result default to i32 (RUE-633 family).
                        let module_member_ty = match self.rir.get(*receiver).data {
                            InstData::FieldGet {
                                base: module_ref,
                                field: type_name,
                            } => self
                                .struct_type_for_module(module_ref, &type_name)
                                .or_else(|| self.enum_type_for_module(module_ref, &type_name)),
                            _ => None,
                        };
                        if let Some(reduced) = self
                            .inline_ctor_head_types
                            .and_then(|heads| heads.get(receiver).copied())
                            .or(module_member_ty)
                            && let Some(result) = self
                                .generate_call_on_reduced_type(reduced, *method, args, span, ctx)
                        {
                            result
                        } else {
                            for arg in call_args.iter() {
                                let info =
                                    self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                                arg_diverged |= !info.continues;
                                if self.was_canceled() {
                                    break;
                                }
                            }
                            InferType::Var(self.fresh_var())
                        }
                    }
                    InferType::Concrete(ty) => {
                        if let Some(struct_id) = ty.as_struct() {
                            // Use StructId directly for method lookup (falls
                            // back to late-registered anonymous-struct
                            // signatures, RUE-164)
                            let method_key = (struct_id, *method);
                            if self.is_view_len_call(struct_id, *method, call_args.len()) {
                                // `len` is the one method a `{ptr, len}` view —
                                // a slice `[T]`, a `str`, a `Str(N)` — has
                                // (7.2:17-18) and sema synthesizes it from the
                                // view's `len` word, so no signature is
                                // registered for the synthetic struct. Publish
                                // its `u64` result here anyway: without it the
                                // call typed as `ERROR`, which unifies with any
                                // annotation, so `let n: i32 = s.len();` passed
                                // inference and reached CFG verification as a
                                // `u64` value in an `i32` binding (RUE-1611,
                                // RUE-1679).
                                InferType::Concrete(Type::U64)
                            } else if let Some(method_sig) = self.method_sig(&method_key) {
                                // Generate constraints for arguments
                                for (arg, param_type) in
                                    call_args.iter().zip(method_sig.param_types.iter())
                                {
                                    // View/string compatibility is
                                    // representation-aware and authoritative
                                    // in sema. This includes contextual
                                    // `Str(N)` expressions; sema still
                                    // requires exact capacity for non-literals
                                    // (RUE-634/RUE-636).
                                    let defer_equality =
                                        self.is_slice_struct_type(param_type.clone());
                                    let arg_info = self.generate_sequenced_operand(
                                        arg.value,
                                        ctx,
                                        !arg_diverged,
                                    );
                                    arg_diverged |= !arg_info.continues;
                                    if self.was_canceled() {
                                        break;
                                    }
                                    if !defer_equality {
                                        self.add_constraint(Constraint::contextual(
                                            arg_info.ty,
                                            param_type.clone(),
                                            arg_info.span,
                                        ));
                                    }
                                }
                                method_sig.return_type.clone()
                            } else {
                                // Method not found - sema will report the error
                                // Still generate arg types to catch errors in arguments
                                for arg in call_args.iter() {
                                    let info = self.generate_sequenced_operand(
                                        arg.value,
                                        ctx,
                                        !arg_diverged,
                                    );
                                    arg_diverged |= !info.continues;
                                    if self.was_canceled() {
                                        break;
                                    }
                                }
                                InferType::Concrete(Type::ERROR)
                            }
                        } else {
                            // Non-struct receiver - sema will report the error
                            for arg in call_args.iter() {
                                let info =
                                    self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                                arg_diverged |= !info.continues;
                                if self.was_canceled() {
                                    break;
                                }
                            }
                            InferType::Concrete(Type::ERROR)
                        }
                    }
                    // Receiver still a type variable: sema resolves the
                    // receiver and diagnoses later (mirrors FieldGet's
                    // fallback, RUE-126/RUE-119).
                    //
                    // Exception: a module-qualified inline type-constructor
                    // head (`std.result.Result(i64, i32).Ok(41)`, RUE-950)
                    // reaches the outer construction with a `Var` receiver —
                    // its inner generic module-member call yields a fresh
                    // variable, not `COMPTIME_TYPE`, so the arm above never
                    // fires (the local `Result(i64, i32).Ok(41)` form does
                    // reach it, its receiver being a plain `Call`). Sema
                    // pre-reduced the head in `inline_ctor_head_types` keyed
                    // by the receiver `InstRef` regardless of resolution
                    // route, so constrain the construction's arguments against
                    // the reduced variant-payload / assoc-fn signature here —
                    // the same expectation-threading as the local form
                    // (RUE-599). Without it the payload literal stayed
                    // unconstrained, defaulted to i32, and failed a wider
                    // declared payload type with E0206.
                    _ => {
                        if let Some(reduced) = self
                            .inline_ctor_head_types
                            .and_then(|heads| heads.get(receiver).copied())
                            && let Some(result) = self
                                .generate_call_on_reduced_type(reduced, *method, args, span, ctx)
                        {
                            result
                        } else {
                            for arg in call_args.iter() {
                                let info =
                                    self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                                arg_diverged |= !info.continues;
                                if self.was_canceled() {
                                    break;
                                }
                            }
                            InferType::Var(self.fresh_var())
                        }
                    }
                };
                continues &= !arg_diverged && !Self::is_never_concrete(&result_type);

                result_type
            }

            // Comptime block: the type depends on whether evaluation succeeds at compile time.
            // For type inference, we use a fresh type variable that can unify with
            // whatever type is expected from the context (e.g., a let binding's type annotation).
            // Note: the variable is NOT registered in `int_literal_vars` — a comptime
            // block can have any type (bool, type, ...). If the inner expression is an
            // integer literal, the Equal constraint chains this variable to the
            // literal's variable, so i32-defaulting still applies transitively.
            InstData::Comptime { expr } => {
                // Generate constraints for the inner expression
                let inner_info = self.generate(*expr, ctx);
                continues &= inner_info.continues;

                // Use a fresh variable so comptime can unify with expected type from context.
                // The actual evaluation happens in sema where we know the final type.
                let var = self.fresh_var();
                // Add constraint that this var equals the inner expression's type
                self.add_constraint(Constraint::equal(InferType::Var(var), inner_info.ty, span));
                InferType::Var(var)
            }

            // Checked block: for type inference purposes, the type is the type of the inner expression
            // The actual checking of unchecked operations happens in sema
            InstData::Checked { expr } => {
                // Generate constraints for the inner expression
                {
                    ctx.checked_depth += 1;
                }
                let inner_info = self.generate(*expr, ctx);
                continues &= inner_info.continues;
                {
                    ctx.checked_depth -= 1;
                }
                inner_info.ty
            }

            // Type constant: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            // This has the special ComptimeType type which indicates it's a type value.
            InstData::TypeConst { .. } => InferType::Concrete(Type::COMPTIME_TYPE),

            // Anonymous struct type: a struct type used as a comptime value
            // This also has the ComptimeType type.
            InstData::AnonStructType { .. } => InferType::Concrete(Type::COMPTIME_TYPE),

            // Anonymous enum type: an enum (sum) type used as a comptime value.
            InstData::AnonEnumType { .. } => InferType::Concrete(Type::COMPTIME_TYPE),
        };

        // A cancellation observed while generating a child must unwind before
        // this instruction records a type or continues into enclosing sibling
        // work.  The enclosing loop checks the same flag before its next item.
        if self.canceled {
            return ExprInfo::diverged(InferType::Concrete(Type::ERROR), span);
        }

        // Alloc's expression result is unit, but its binding type is the
        // lexical checkpoint fact captured above.  Preserve that fact for
        // staged frontier reconstruction instead of overwriting it with the
        // statement result here.
        if !matches!(inst.data, InstData::Alloc { .. }) {
            self.record_type(inst_ref, ty.clone());
        }
        continues &= self.expr_continues.get(&inst_ref).copied().unwrap_or(true);
        self.expr_continues.insert(inst_ref, continues);
        ExprInfo::with_continues(ty, span, continues)
    }

    /// Generate constraints for a binary arithmetic operation.
    fn generate_binary_arith(
        &mut self,
        inst_ref: InstRef,
        lhs: InstRef,
        rhs: InstRef,
        ctx: &mut ConstraintContext,
    ) -> InferType {
        let lhs_info = self.generate(lhs, ctx);
        if self.was_canceled() {
            return InferType::Concrete(Type::ERROR);
        }
        let reachable_facts_after_lhs = ctx.loop_break_stack.clone();
        let rhs_info = self.generate(rhs, ctx);
        if self.was_canceled() {
            return InferType::Concrete(Type::ERROR);
        }
        if !lhs_info.continues {
            restore_reachable_break_facts(&mut ctx.loop_break_stack, &reachable_facts_after_lhs);
        }

        // A diverging operand (`!`, e.g. `n - match m {}`) makes the whole
        // expression diverge. Never coerces to any type (spec 3.4:3-4), so
        // don't constrain the operands to one another — doing so would drag an
        // integer literal to `!` and then bogusly range-check it against `!`
        // (RUE-270). The result is `!`; the surrounding context coerces it.
        if Self::is_never_concrete(&lhs_info.ty) || Self::is_never_concrete(&rhs_info.ty) {
            self.expr_continues
                .insert(inst_ref, lhs_info.continues && rhs_info.continues);
            return InferType::Concrete(Type::NEVER);
        }
        self.expr_continues
            .insert(inst_ref, lhs_info.continues && rhs_info.continues);

        // Both operands must have the same type
        // Use a fresh type variable for the result
        let result_var = self.fresh_var();
        let result_ty = InferType::Var(result_var);

        self.add_constraint(Constraint::equal(
            lhs_info.ty,
            result_ty.clone(),
            lhs_info.span,
        ));
        self.add_constraint(Constraint::equal(
            rhs_info.ty,
            result_ty.clone(),
            rhs_info.span,
        ));

        // Result must be an integer type (catches errors like `true + 1` early)
        self.add_constraint(Constraint::is_numeric(result_ty.clone(), lhs_info.span));

        result_ty
    }

    /// Whether `ty` is *concretely* the never type `!` (a diverging expression),
    /// as opposed to an unresolved type variable that might resolve to it.
    fn is_never_concrete(ty: &InferType) -> bool {
        matches!(ty, InferType::Concrete(t) if t.is_never())
    }

    /// Generate constraints for the `+` operator (RUE-17 Phase 1, ADR-0035).
    ///
    /// `+` is arithmetic addition on integers and concatenation on two
    /// `String`s. When either operand is *concretely* the builtin `String`
    /// type, both operands are constrained to `String` and the result is
    /// `String` (a `String + int` mix then fails unification with E0206).
    /// Otherwise this behaves exactly like [`generate_binary_arith`]: operands
    /// and result share a type that must be an integer.
    fn generate_add(
        &mut self,
        inst_ref: InstRef,
        lhs: InstRef,
        rhs: InstRef,
        ctx: &mut ConstraintContext,
    ) -> InferType {
        let lhs_info = self.generate(lhs, ctx);
        if self.was_canceled() {
            return InferType::Concrete(Type::ERROR);
        }
        let reachable_facts_after_lhs = ctx.loop_break_stack.clone();
        let rhs_info = self.generate(rhs, ctx);
        if self.was_canceled() {
            return InferType::Concrete(Type::ERROR);
        }
        if !lhs_info.continues {
            restore_reachable_break_facts(&mut ctx.loop_break_stack, &reachable_facts_after_lhs);
        }
        self.expr_continues
            .insert(inst_ref, lhs_info.continues && rhs_info.continues);

        // A diverging operand (`!`, e.g. `1 + match n {}`) makes the whole
        // expression diverge; never coerces to any type (spec 3.4:3-4). Don't
        // constrain the operands to one another (that would drag the integer
        // literal `1` to `!` and range-check it against `!`, RUE-270). Checked
        // before the String-concat overload so `s + match n {}` also diverges.
        if Self::is_never_concrete(&lhs_info.ty) || Self::is_never_concrete(&rhs_info.ty) {
            return InferType::Concrete(Type::NEVER);
        }

        if self.is_string_concrete(&lhs_info.ty)
            || self.is_string_concrete(&rhs_info.ty)
            || self.is_string_literal_candidate(&lhs_info.ty)
            || self.is_string_literal_candidate(&rhs_info.ty)
        {
            let string_ty = [&lhs_info.ty, &rhs_info.ty]
                .into_iter()
                .find(|ty| self.is_string_concrete(ty))
                .cloned()
                .unwrap_or_else(|| self.string_infer_type());
            self.add_constraint(Constraint::equal(
                lhs_info.ty,
                string_ty.clone(),
                lhs_info.span,
            ));
            self.add_constraint(Constraint::equal(
                rhs_info.ty,
                string_ty.clone(),
                rhs_info.span,
            ));
            return string_ty;
        }

        // Integer addition — identical to the other binary arithmetic operators.
        let result_var = self.fresh_var();
        let result_ty = InferType::Var(result_var);
        self.add_constraint(Constraint::equal(
            lhs_info.ty,
            result_ty.clone(),
            lhs_info.span,
        ));
        self.add_constraint(Constraint::equal(
            rhs_info.ty,
            result_ty.clone(),
            rhs_info.span,
        ));
        self.add_constraint(Constraint::is_numeric(result_ty.clone(), lhs_info.span));
        result_ty
    }

    /// The canonical standard-library `StrBuf` lang item as an
    /// [`InferType::Concrete`], or `Concrete(ERROR)` when std is absent.
    fn string_infer_type(&self) -> InferType {
        InferType::Concrete(self.strbuf_type.unwrap_or(Type::ERROR))
    }

    /// Whether `ty` is concretely the canonical StrBuf lang item.
    fn is_string_concrete(&self, ty: &InferType) -> bool {
        matches!(ty, InferType::Concrete(t) if t.as_struct().is_some_and(|id| self.type_pool.is_strbuf(id)))
    }

    /// Whether an inference type is the still-contextual type variable rooted
    /// at a string literal. Local bindings retain the same variable, so this
    /// also recognizes `let s = "a"; s + "b"` before defaulting runs.
    fn is_string_literal_candidate(&self, ty: &InferType) -> bool {
        matches!(ty, InferType::Var(var) if self.string_literal_vars.contains(var))
    }

    fn string_literal_default_is_str(&self) -> bool {
        self.string_literal_default
            .as_struct()
            .is_some_and(|id| &*self.type_pool.struct_def(id).name == "str")
    }

    fn call_args_continue(&self, args: &rue_rir::RirCallArgsRange) -> bool {
        self.rir
            .call_args(args)
            .iter()
            .all(|arg| self.expr_continues.get(&arg.value).copied().unwrap_or(true))
    }

    /// Generate constraints for a type-qualified call — an associated-function
    /// call or an enum tuple-variant construction — resolving the callee by type
    /// name and constraining each argument to the declared parameter/payload
    /// type. The `MethodCall` arm forwards type-name receivers here
    /// (`Type.function(args)`, RUE-488). Getting
    /// the argument constraints here — not just in sema — is what pins a literal
    /// like the `8` in `StrBuf.with_capacity(8)` to the declared `u64` parameter
    /// instead of letting it default to `i32`.
    fn generate_type_qualified_call(
        &mut self,
        type_name: Spur,
        function: Spur,
        args: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut ConstraintContext,
    ) -> InferType {
        // Enum tuple-variant construction: `Shape.Circle(5)` (RUE-221).
        // Checked first so it takes precedence over the struct-method path
        // below.
        if let Some(enum_ty) = self.enum_type_for(&type_name, span.file_id)
            && let Some(result) =
                self.generate_call_on_reduced_type(enum_ty, function, args, span, ctx)
        {
            return result;
        }

        // Struct associated function: constrain each argument to the declared
        // parameter type and yield the return type. Resolved through
        // `struct_type_for` so comptime and
        // file-level const aliases participate — `const Ints = ArrayBuf(i64);
        // Ints.new()` must yield the concrete instantiation or downstream
        // method arguments go unconstrained (RUE-633).
        if let Some(struct_ty) = self.struct_type_for(&type_name, span.file_id)
            && struct_ty.as_struct().is_some()
        {
            if let Some(result) =
                self.generate_call_on_reduced_type(struct_ty, function, args, span, ctx)
            {
                return result;
            }
            // Method not found - sema reports the error; still process args.
            let args = self.rir.call_args(args);
            for arg in args.iter() {
                self.note_sibling_attempt();
                if self.was_canceled() {
                    break;
                }
                self.generate(arg.value, ctx);
                if self.was_canceled() {
                    break;
                }
            }
            return InferType::Concrete(Type::ERROR);
        }

        // Type not found - sema reports the error; still process args.
        let args = self.rir.call_args(args);
        for arg in args.iter() {
            self.note_sibling_attempt();
            if self.was_canceled() {
                break;
            }
            self.generate(arg.value, ctx);
            if self.was_canceled() {
                break;
            }
        }
        InferType::Concrete(Type::ERROR)
    }

    /// Constrain a `Type.function(args)` call against an already-concrete
    /// type: enum tuple-variant construction (`Shape.Circle(5)`, RUE-221) or a
    /// struct associated-function call. Imposing the declared payload/parameter
    /// types on the arguments here — not just in sema — is what pins a literal
    /// like the `8` in `StrBuf.with_capacity(8)` to the declared `u64` instead
    /// of letting it default to `i32`. Shared by
    /// [`Self::generate_type_qualified_call`] (name-resolved heads) and the
    /// inline type-constructor head path (sema-reduced heads, RUE-599).
    /// Returns `None` — with the arguments NOT yet visited — when `function`
    /// is not a variant/associated function of `ty`; the caller processes the
    /// arguments and lets sema diagnose.
    fn generate_call_on_reduced_type(
        &mut self,
        ty: Type,
        function: Spur,
        args: &rue_rir::RirCallArgsRange,
        _span: Span,
        ctx: &mut ConstraintContext,
    ) -> Option<InferType> {
        if let Some(enum_id) = ty.as_enum() {
            let def = self.type_pool.enum_def(enum_id);
            let payload = def
                .find_variant(self.interner.resolve(&function))
                .map(|vidx| def.variant_payload(vidx).to_vec())?;
            let args = self.rir.call_args(args);
            let mut arg_diverged = false;
            for (i, arg) in args.iter().enumerate() {
                let arg_info = self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
                arg_diverged |= !arg_info.continues;
                if self.was_canceled() {
                    break;
                }
                if let Some(&pty) = payload.get(i) {
                    // Convert the declared payload type structurally so an array
                    // payload (`[i32; 2]`) unifies with an array-literal argument
                    // and propagates the expected element type into its literal
                    // elements — exactly as struct-field init does (RUE-260).
                    let expected = self.type_to_infer(pty);
                    self.add_constraint(Constraint::contextual(
                        arg_info.ty,
                        expected,
                        arg_info.span,
                    ));
                }
            }
            return Some(InferType::Concrete(ty));
        }
        let struct_id = ty.as_struct()?;
        let method_sig = self.method_sig(&(struct_id, function))?;
        let args = self.rir.call_args(args);
        let mut arg_diverged = false;
        for (arg, param_type) in args.iter().zip(method_sig.param_types.iter()) {
            let defer_equality = self.is_slice_struct_type(param_type.clone());
            let arg_info = self.generate_sequenced_operand(arg.value, ctx, !arg_diverged);
            arg_diverged |= !arg_info.continues;
            if self.was_canceled() {
                break;
            }
            if !defer_equality {
                self.add_constraint(Constraint::contextual(
                    arg_info.ty,
                    param_type.clone(),
                    arg_info.span,
                ));
            }
        }
        Some(method_sig.return_type.clone())
    }

    /// Resolve an enum type name that may be a comptime type-variable binding
    /// (`let O = Option(i32); O.Some(..)`), falling back to the named-enum
    /// table. Mirrors sema's `resolve_enum_type_name`; without the
    /// comptime-alias lookup, generic-enum construction/matching inferred
    /// `<error>` and poisoned the surrounding constraints (RUE-6 phase 2).
    /// Struct type for an unqualified name. Present precedence is substitutions,
    /// lexical aliases, file-level aliases, a declaration in the reference
    /// file, then builtins (RUE-525).
    fn struct_type_for(&self, type_name: &Spur, file_id: FileId) -> Option<Type> {
        // `Self` (and comptime type parameters) resolve through the enclosing
        // substitution first, so `Self.assoc_fn(args)` constrains its
        // arguments like `StructName.assoc_fn(args)` (RUE-639).
        self.type_subst
            .and_then(|subst| subst.get(type_name).copied())
            .filter(|ty| ty.as_struct().is_some())
            .or_else(|| {
                self.comptime_alias_types
                    .get(type_name)
                    .copied()
                    .filter(|ty| ty.as_struct().is_some())
            })
            .or_else(|| {
                // File-level `const Ints = ArrayBuf(i64);` aliases: without
                // this, a method chain rooted at the alias (`Ints.new()`)
                // missed the type-qualified path, the receiver degraded to a
                // fresh variable, and every later method argument was left
                // unconstrained — an integer-literal expression argument then
                // defaulted to i32 and was zero-extended into the declared
                // 64-bit slot (miscompile, RUE-633).
                self.const_type_alias((file_id, *type_name))
                    .filter(|ty| ty.as_struct().is_some())
            })
            .or_else(|| self.struct_type_by_file((file_id, *type_name)))
            .or_else(|| self.builtin_struct_type(*type_name))
    }

    fn enum_type_for(&self, type_name: &Spur, file_id: FileId) -> Option<Type> {
        self.type_subst
            .and_then(|subst| subst.get(type_name).copied())
            .filter(|ty| ty.is_enum())
            .or_else(|| {
                self.comptime_alias_types
                    .get(type_name)
                    .copied()
                    .filter(|ty| ty.is_enum())
            })
            .or_else(|| {
                // File-level const enum aliases, for the same reason as
                // `struct_type_for` (RUE-633): a construction rooted at the
                // alias must constrain its payload arguments.
                self.const_type_alias((file_id, *type_name))
                    .filter(|ty| ty.is_enum())
            })
            .or_else(|| self.enum_type_by_file((file_id, *type_name)))
            .or_else(|| self.builtin_enum_type(*type_name))
    }

    fn enum_type_for_module(&self, module: InstRef, type_name: &Spur) -> Option<Type> {
        let file_id = self.module_member_file(module)?;
        self.enum_type_by_file((file_id, *type_name)).or_else(|| {
            // A type-valued const member (`pub const Opt = Option(i64)`)
            // is as good as a declaration here (RUE-630/RUE-633): without
            // it the qualified alias head left the whole chain
            // unconstrained.
            self.const_type_alias((file_id, *type_name))
                .filter(|ty| ty.is_enum())
        })
    }

    /// Struct analogue of [`Self::enum_type_for_module`], for
    /// `module.Type.assoc_fn(...)` calls: without it the receiver degraded to
    /// a fresh variable, the call's arguments and result went unconstrained,
    /// and an integer-literal operand later compared against the result
    /// defaulted to i32 — zero-extended against the callee's real 64-bit
    /// value at run time (the RUE-633 miscompile family).
    fn struct_type_for_module(&self, module: InstRef, type_name: &Spur) -> Option<Type> {
        let file_id = self.module_member_file(module)?;
        self.struct_type_by_file((file_id, *type_name)).or_else(|| {
            // Type-valued const members participate like declarations —
            // see `enum_type_for_module` (RUE-630/RUE-633).
            self.const_type_alias((file_id, *type_name))
                .filter(|ty| ty.as_struct().is_some())
        })
    }

    /// The defining file of `module`'s target: a `VarRef` naming an imported
    /// module binding in the current file, or a `FieldGet` chain through
    /// re-exported module bindings (`db.query.Pred` resolves `db` in the
    /// current file, then `query` as a module binding in db's file, and so
    /// on) — without the recursion a nested-facade payload construction left
    /// its arguments unconstrained (RUE-633 family, reject-valid this time:
    /// sema's E0206 caught the defaulted literal).
    fn module_member_file(&self, module: InstRef) -> Option<FileId> {
        let inst = self.rir.get(module);
        match &inst.data {
            InstData::VarRef { name, .. } => {
                let module_ty = self.module_binding_type((inst.span.file_id, *name))?;
                self.module_file(module_ty)
            }
            InstData::FieldGet { base, field } => {
                let base_file = self.module_member_file(*base)?;
                let module_ty = self.module_binding_type((base_file, *field))?;
                self.module_file(module_ty)
            }
            _ => None,
        }
    }

    fn module_file(&self, module_ty: Type) -> Option<FileId> {
        let module_id = module_ty.as_module()?;
        self.module_file_id(module_id)
    }

    /// Install the exact payload locals for a selected or ordinary match arm.
    /// This is shared by both paths so pruning an unselected arm cannot change
    /// lexical shadowing or payload typing in the selected arm.
    fn register_match_bindings(
        &mut self,
        pattern: &rue_rir::RirPatternView<'_>,
        ctx: &mut ConstraintContext,
    ) {
        let rue_rir::RirPatternView::Path {
            module,
            ctor_head,
            type_name,
            variant,
            bindings,
            span: pat_span,
            ..
        } = pattern
        else {
            return;
        };
        if bindings.is_empty() {
            return;
        }
        let enum_ty = ctor_head
            .and_then(|head| {
                self.inline_ctor_head_types
                    .and_then(|heads| heads.get(&head).copied())
            })
            .or_else(|| {
                module.and_then(|module_ref| self.enum_type_for_module(module_ref, type_name))
            })
            .or_else(|| self.enum_type_for(type_name, pat_span.file_id));
        let Some(payload) = enum_ty
            .and_then(|ty| ty.as_enum())
            .map(|id| self.type_pool.enum_def(id))
            .and_then(|def| {
                def.find_variant(self.interner.resolve(variant))
                    .map(|v| def.variant_payload(v).to_vec())
            })
        else {
            return;
        };
        for (index, binding) in bindings.iter().enumerate() {
            if self.interner.resolve(&binding) == "_" {
                continue;
            }
            if let Some(&ty) = payload.get(index) {
                ctx.insert_local(
                    *binding,
                    LocalVarInfo {
                        ty: InferType::Concrete(ty),
                        is_mut: false,
                        span: *pat_span,
                    },
                );
            }
        }
    }

    /// Get the inferred type for a pattern.
    fn pattern_type(&mut self, pattern: &rue_rir::RirPatternView<'_>) -> InferType {
        match pattern {
            rue_rir::RirPatternView::Wildcard(_) => {
                // Wildcard matches anything - use a fresh type variable
                let var = self.fresh_var();
                InferType::Var(var)
            }
            // An integer pattern is an integer literal, and is typed exactly
            // like one: a fresh variable marked in `int_literal_vars`, never
            // the bare `InferType::IntLiteral` terminal (RUE-1636).
            //
            // The terminal is absorbing. `Unifier::bind` stores it into the
            // scrutinee's substitution slot, so every variable already chained
            // to the scrutinee path-compresses onto `IntLiteral` and the
            // union-find class stops having a single representative. A later
            // concrete type then upgrades only the variable that happens to be
            // queried (`rebind_int_literal_to_concrete`); its siblings stay
            // `IntLiteral` and default to i32. `let a = 1; let b = 2; let c = a
            // + b; match c { 3 => {}, _ => {} } a` with an `i64` return typed
            // `a` as i64 while `b` stayed i32 — one class, two widths, and
            // mixed-width AIR downstream.
            //
            // A marked variable keeps the class intact: binding the scrutinee
            // to it extends the chain instead of terminating it, so the
            // representative is upgraded once for the whole class and every
            // member resolves to the same width.
            rue_rir::RirPatternView::Int { .. } => InferType::Var(self.fresh_int_literal_var()),
            rue_rir::RirPatternView::Bool(_, _) => InferType::Concrete(Type::BOOL),
            rue_rir::RirPatternView::Path {
                module,
                ctor_head,
                type_name,
                ..
            } => {
                // An inline type-constructor head (`Opt(u8).Some(b)`, RUE-596)
                // was pre-reduced by sema in `inline_ctor_head_types`; consult
                // it first so the pattern contributes the real enum type
                // instead of poisoning the scrutinee with `<error>` (RUE-954).
                let enum_ty = ctor_head
                    .and_then(|head| {
                        self.inline_ctor_head_types
                            .and_then(|heads| heads.get(&head).copied())
                    })
                    .or_else(|| {
                        module
                            .and_then(|module_ref| self.enum_type_for_module(module_ref, type_name))
                    })
                    .or_else(|| self.enum_type_for(type_name, pattern.span().file_id));
                if let Some(enum_ty) = enum_ty {
                    InferType::Concrete(enum_ty)
                } else {
                    InferType::Concrete(Type::ERROR)
                }
            }
        }
    }

    /// Resolve a call argument used as a comptime type value (e.g. the `i32` in
    /// `identity(i32, 42)`) to a concrete type, if it can be determined during
    /// constraint generation.
    ///
    /// Handles type literals (`i32`, `bool`, ...), named struct/enum types
    /// (user-declared as well as built-in), and forwarded type parameters (a
    /// reference to `T` inside a specialized generic body, resolved via
    /// `self.type_subst`). Returns `None` for type values that are only known
    /// to semantic analysis (e.g. a local variable bound to an anonymous struct
    /// type) - those are type-checked in sema instead.
    fn extract_type_argument(&self, arg: InstRef, ctx: &ConstraintContext) -> Option<Type> {
        let file_id = self.rir.get(arg).span.file_id;
        let resolve_sym = |sym: &Spur| -> Option<Type> {
            // A forwarded type parameter substitutes to whatever the enclosing
            // specialization bound it to, of any kind (a primitive, an array,
            // a nominal type), so this lookup is unfiltered and comes first.
            if let Some(subst) = self.type_subst {
                if let Some(&ty) = subst.get(sym) {
                    return Some(ty);
                }
            }
            // A directly named struct/enum — `identity(Foo, ..)` — is a type
            // value exactly like `identity(i32, ..)`. `struct_type_for` /
            // `enum_type_for` are the same by-file lookups the type-qualified
            // call path uses (lexical aliases, file-level aliases, the
            // referencing file's declarations, then builtins), so a nominal
            // spelling resolves wherever its alias spelling already did.
            // Consulting only the builtin tables left the argument out of
            // `type_subst`, so a `-> T` return type could not be substituted
            // and stayed the literal `type` placeholder — reported as a bogus
            // "expected Foo, found type" mismatch at the binding (RUE-1680).
            self.struct_type_for(sym, file_id)
                .or_else(|| self.enum_type_for(sym, file_id))
        };

        match &self.rir.get(arg).data {
            InstData::TypeConst { type_name } => {
                match self.infer_rir_type_hint(*type_name, self.rir.get(arg).span.file_id) {
                    Some(InferType::Concrete(ty)) => Some(ty),
                    _ => None,
                }
            }
            // A struct/enum name or forwarded type parameter used as a value
            // parses as a variable reference, not a type literal.
            InstData::VarRef { name, .. } => {
                // A local bound to a type value (`let X = i32; identity(X, 42)`
                // or `let P = Pair(i32); f(P, ..)`) resolves to that bound type
                // via the in-scope comptime-alias view — the same map
                // generic-enum construction/matching consults (`enum_type_for`).
                // Without this the type argument was left unresolved, so the
                // call's return type defaulted to the literal `type` and
                // mismatched the substituted element type (spurious E0206,
                // RUE-281). The literal form (`identity(i32, 42)`) already
                // worked via the `TypeConst` arm; this makes an aliased type
                // behave identically.
                if let Some(ty) = self.comptime_alias_types.get(name).copied() {
                    return Some(ty);
                }
                // A local or parameter shadows any same-named struct/enum. A
                // local *not* bound to a comptime type value (a runtime value)
                // has no concrete type here. Preserve that error through
                // dependent substitutions so inference does not manufacture a
                // downstream mismatch; sema reports the canonical comptime
                // known-value diagnostic at the argument itself.
                // Forwarded type parameters (`T` inside a specialized generic
                // body) are not in scope as runtime params/locals and resolve
                // via `self.type_subst` above.
                if ctx.locals.contains_key(name) || ctx.contains_param(*name) {
                    return Some(Type::ERROR);
                }
                resolve_sym(name)
            }
            _ => None,
        }
    }

    /// Return a comptime argument already evaluated by the canonical engine.
    /// Bare value-parameter references remain available during the probe.
    fn comptime_argument_value(&self, arg: InstRef) -> Option<ConstValue> {
        self.comptime_argument_values
            .and_then(|values| values.get(&arg).copied())
            .or_else(|| match self.rir.get(arg).data {
                InstData::VarRef { name, .. } => self
                    .comptime_values
                    .and_then(|values| values.get(&name).copied()),
                _ => None,
            })
    }

    /// Derive a best-effort constraint hint from parser-structured type syntax.
    ///
    /// This is deliberately not semantic type resolution: it produces no
    /// durable type fact or diagnostic, and fact-bearing forms such as qualified
    /// names and comptime constructors return `None`. The one authoritative
    /// semantic policy runs later through `resolve_structured_semantic_type_syntax`.
    fn infer_rir_type_hint(&self, syntax: RirTypeSyntaxRef, file_id: FileId) -> Option<InferType> {
        self.infer_rir_type_hint_with_substitutions(syntax, self.type_subst, None, file_id)
    }

    fn infer_rir_type_hint_with_substitutions(
        &self,
        syntax: RirTypeSyntaxRef,
        subst: Option<&AHashMap<Spur, Type>>,
        values: Option<&AHashMap<Spur, i128>>,
        file_id: FileId,
    ) -> Option<InferType> {
        self.infer_type_hint(self.rir.type_syntax(), syntax, subst, values, file_id)
    }

    fn infer_structured_type_hint(
        &self,
        syntax: &crate::sema::StructuredTypeSyntax,
        subst: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, i128>,
        file_id: FileId,
    ) -> Option<InferType> {
        self.infer_type_hint(
            &syntax.arena,
            syntax.root,
            Some(subst),
            Some(values),
            file_id,
        )
    }

    fn infer_type_hint(
        &self,
        arena: &rue_rir::RirTypeSyntaxArena<Spur>,
        syntax: RirTypeSyntaxRef,
        subst: Option<&AHashMap<Spur, Type>>,
        values: Option<&AHashMap<Spur, i128>>,
        file_id: FileId,
    ) -> Option<InferType> {
        match arena.node(syntax)? {
            RirTypeSyntaxNode::Named(symbol) => {
                let name = *arena.symbol(*symbol)?;
                if let Some(ty) = subst.and_then(|subst| subst.get(&name)).copied() {
                    return Some(self.type_to_infer(ty));
                }
                if let Some(ty) = self.comptime_alias_types.get(&name).copied() {
                    return Some(self.type_to_infer(ty));
                }
                if let Some(ty) = self.const_type_alias((file_id, name)) {
                    return Some(self.type_to_infer(ty));
                }
                self.infer_named_type_hint(self.interner.resolve(&name), file_id)
            }
            RirTypeSyntaxNode::Unit => Some(InferType::Concrete(Type::UNIT)),
            RirTypeSyntaxNode::Never => Some(InferType::Concrete(Type::NEVER)),
            RirTypeSyntaxNode::Array { element, length } => {
                let element = self.infer_type_hint(arena, *element, subst, values, file_id)?;
                let length = match arena.node(*length)? {
                    RirTypeSyntaxNode::Integer(value) => u64::try_from(*value).ok()?,
                    RirTypeSyntaxNode::Named(symbol) => {
                        let name = *arena.symbol(*symbol)?;
                        values
                            .and_then(|values| values.get(&name).copied())
                            .or_else(|| self.scoped_const_value(name, file_id))
                            .and_then(|value| u64::try_from(value).ok())?
                    }
                    _ => return None,
                };
                Some(InferType::Array {
                    element: Box::new(element),
                    length,
                })
            }
            RirTypeSyntaxNode::PointerConst { pointee }
            | RirTypeSyntaxNode::PointerMut { pointee } => {
                let pointee = self
                    .infer_type_hint(arena, *pointee, subst, values, file_id)?
                    .as_concrete()?;
                let ty = match arena.node(syntax)? {
                    RirTypeSyntaxNode::PointerConst { .. } => {
                        Type::new_ptr_const(self.type_pool.intern_ptr_const_from_type(pointee))
                    }
                    RirTypeSyntaxNode::PointerMut { .. } => {
                        Type::new_ptr_mut(self.type_pool.intern_ptr_mut_from_type(pointee))
                    }
                    _ => unreachable!(),
                };
                Some(InferType::Concrete(ty))
            }
            RirTypeSyntaxNode::Qualified { .. }
            | RirTypeSyntaxNode::Slice { .. }
            | RirTypeSyntaxNode::AnonymousStruct { .. }
            | RirTypeSyntaxNode::AnonymousEnum { .. }
            | RirTypeSyntaxNode::TypeCall { .. }
            | RirTypeSyntaxNode::ValueCall { .. }
            | RirTypeSyntaxNode::Integer(_) => None,
        }
    }

    fn infer_named_type_hint(&self, name: &str, file_id: FileId) -> Option<InferType> {
        // Check primitives (single shared table, RUE-155)
        if let Some(ty) = Type::from_primitive_name(name) {
            return Some(InferType::Concrete(ty));
        }

        // Check for struct types (including builtin String)
        if let Some(name_spur) = self.interner.get(name) {
            if let Some(ty) = self.struct_type_by_file((file_id, name_spur)) {
                return Some(InferType::Concrete(ty));
            }
            if let Some(ty) = self.enum_type_by_file((file_id, name_spur)) {
                return Some(InferType::Concrete(ty));
            }
            if let Some(struct_ty) = self.builtin_struct_type(name_spur) {
                return Some(InferType::Concrete(struct_ty));
            }
            if let Some(enum_ty) = self.builtin_enum_type(name_spur) {
                return Some(InferType::Concrete(enum_ty));
            }
            if let Some(alias_ty) = self.scoped_const_type_alias(name_spur, file_id) {
                return Some(InferType::Concrete(alias_ty));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;

    /// Helper to create a minimal RIR, interner, and type pool for testing.
    fn make_test_rir_interner_and_type_pool() -> (rue_rir::RirEditor, ThreadedRodeo, TypeInternPool)
    {
        let rir = rue_rir::RirEditor::new();
        let interner = ThreadedRodeo::new();
        let type_pool = TypeInternPool::new();
        (rir, interner, type_pool)
    }

    #[test]
    fn indexed_struct_field_resolution_scales_with_field_accesses() {
        const FIELD_COUNT: usize = 8_192;

        let (rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let mut field_names = Vec::with_capacity(FIELD_COUNT);
        let fields = (0..FIELD_COUNT)
            .map(|index| {
                let name = format!("field_{index:04}");
                field_names.push(interner.get_or_intern(&name));
                crate::types::StructField {
                    name: name.into(),
                    ty: if index.is_multiple_of(2) {
                        Type::I32
                    } else {
                        Type::I64
                    },
                }
            })
            .collect();
        let (wide_id, inserted) = type_pool.register_struct(
            interner.get_or_intern("Wide"),
            crate::types::StructDef {
                name: "Wide".into(),
                fields,
                is_copy: true,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        assert!(inserted);

        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();
        let cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let wide_ty = Type::new_struct(wide_id);
        let mut candidate_comparisons = 0;

        for index in (0..FIELD_COUNT).rev() {
            let expected = if index.is_multiple_of(2) {
                Type::I32
            } else {
                Type::I64
            };
            assert_eq!(
                cgen.field_type_of_with_observer(wide_ty, field_names[index], || {
                    candidate_comparisons += 1;
                }),
                Some(expected)
            );
        }
        assert_eq!(
            candidate_comparisons, FIELD_COUNT,
            "indexed inference should compare one candidate name per field access"
        );

        let missing = interner.get_or_intern("missing");
        assert_eq!(cgen.field_type_of(wide_ty, missing), None);
        assert_eq!(cgen.field_type_of(Type::I32, field_names[0]), None);

        let (builtin_id, inserted) = type_pool.register_struct(
            interner.get_or_intern("BuiltinRecord"),
            crate::types::StructDef {
                name: "BuiltinRecord".into(),
                fields: vec![crate::types::StructField {
                    name: "value".into(),
                    ty: Type::I64,
                }],
                is_copy: true,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: true,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        assert!(inserted);
        assert_eq!(
            cgen.field_type_of(
                Type::new_struct(builtin_id),
                interner.get_or_intern("value")
            ),
            Some(Type::I64)
        );
    }

    #[test]
    fn test_constraint_generator_int_literal() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Add an integer constant to RIR
        let inst_ref = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(inst_ref, &mut ctx);

        // Integer literals now get a type variable (tracked as int literal var)
        assert!(matches!(info.ty, InferType::Var(_)));
        // The type variable should be tracked in int_literal_vars
        assert_eq!(cgen.int_literal_vars().len(), 1);
        // No constraints should be generated for a simple literal
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_bool_literal() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        let inst_ref = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::BOOL);

        let info = cgen.generate(inst_ref, &mut ctx);

        assert_eq!(info.ty, InferType::Concrete(Type::BOOL));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn pointer_intrinsics_publish_concrete_operand_types() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();
        let value_name = interner.get_or_intern("value");
        let value = rir.add_inst(rue_rir::Inst {
            data: InstData::VarRef {
                name: value_name,
                anchor: None,
            },
            span: Span::new(1, 6),
        });
        let raw = rir
            .add_intrinsic(interner.get_or_intern("raw"), &[value], Span::new(0, 10))
            .unwrap();
        let offset = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(11, 12),
        });
        let moved = rir
            .add_intrinsic(
                interner.get_or_intern("ptr_offset"),
                &[raw, offset],
                Span::new(0, 20),
            )
            .unwrap();

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);
        ctx.locals.insert(
            value_name,
            LocalVarInfo {
                ty: InferType::Concrete(Type::U16),
                is_mut: false,
                span: Span::new(1, 6),
            },
        );

        let raw_ty = cgen.generate(raw, &mut ctx).ty;
        let expected = Type::new_ptr_const(type_pool.intern_ptr_const_from_type(Type::U16));
        assert_eq!(raw_ty, InferType::Concrete(expected));
        assert_eq!(
            cgen.generate(moved, &mut ctx).ty,
            InferType::Concrete(expected)
        );
    }

    #[test]
    fn panic_is_never_and_assert_is_unit_in_hm() {
        // `@panic` diverges (type `!`); `@assert` returns on the success path
        // (type `()`). Pin both explicit HM contracts, and verify each still
        // visits its operand (RUE-512).
        for (name, has_arg, expected) in [
            ("panic", false, Type::NEVER),
            ("panic", true, Type::NEVER),
            ("assert", true, Type::UNIT),
        ] {
            let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
            let functions = AHashMap::new();
            let structs = AHashMap::new();
            let enums = AHashMap::new();
            let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

            // Argument legality belongs to sema; this probe only pins HM's
            // result contract and verifies that an operand is still visited.
            let arg = has_arg.then(|| {
                rir.add_inst(rue_rir::Inst {
                    data: InstData::BoolConst(true),
                    span: Span::new(1, 5),
                })
            });
            let arg_refs: Vec<_> = arg.into_iter().collect();
            let name = interner.get_or_intern(name);
            let intrinsic = rir.add_intrinsic(name, &arg_refs, Span::new(0, 6)).unwrap();

            let mut cgen = ConstraintGenerator::new(
                &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
            );
            let params = AHashMap::new();
            let mut ctx = ConstraintContext::new(&params, Type::UNIT);

            let info = cgen.generate(intrinsic, &mut ctx);
            assert_eq!(
                info.ty,
                InferType::Concrete(expected),
                "@{} HM result contract",
                interner.resolve(&name)
            );
            if let Some(arg) = arg_refs.first() {
                assert_eq!(
                    cgen.expr_types().get(arg),
                    Some(&InferType::Concrete(Type::BOOL)),
                    "@{} must still visit its operand",
                    interner.resolve(&name)
                );
            }
        }
    }

    /// `@assert_eq`/`@assert_ne` are unit-typed like `@assert`, and their two
    /// operands share one type — which is what lets a bare literal take the
    /// other side's type instead of the `i32` default (ADR-0083 Phase 2.5,
    /// spec 4.13:5f).
    #[test]
    fn comparison_assertions_are_unit_and_unify_their_operands_in_hm() {
        for name in ["assert_eq", "assert_ne"] {
            let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
            let functions = AHashMap::new();
            let structs = AHashMap::new();
            let enums = AHashMap::new();
            let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

            let left = rir.add_inst(rue_rir::Inst {
                data: InstData::BoolConst(true),
                span: Span::new(1, 5),
            });
            let right = rir.add_inst(rue_rir::Inst {
                data: InstData::IntConst(1),
                span: Span::new(7, 8),
            });
            let name = interner.get_or_intern(name);
            let intrinsic = rir
                .add_intrinsic(name, &[left, right], Span::new(0, 9))
                .unwrap();

            let mut cgen = ConstraintGenerator::new(
                &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
            );
            let params = AHashMap::new();
            let mut ctx = ConstraintContext::new(&params, Type::UNIT);
            let info = cgen.generate(intrinsic, &mut ctx);
            assert_eq!(
                info.ty,
                InferType::Concrete(Type::UNIT),
                "@{} HM result contract",
                interner.resolve(&name)
            );
            // Both operands are visited, and both are constrained to the same
            // variable — the mismatch these two literals really are is reported
            // when that variable is solved, not here.
            assert!(cgen.expr_types().contains_key(&left));
            assert!(cgen.expr_types().contains_key(&right));
        }
    }

    #[test]
    fn test_constraint_generator_binary_add() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: 1 + 2
        let lhs = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let rhs = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(4, 5),
        });
        let add = rir.add_inst(rue_rir::Inst {
            data: InstData::Add { lhs, rhs },
            span: Span::new(0, 5),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(add, &mut ctx);

        // Result should be a type variable
        assert!(info.ty.is_var());
        // Should generate 3 constraints: lhs = result, rhs = result, IsNumeric(result)
        assert_eq!(cgen.constraints().len(), 3);
        // Verify the third constraint admits integer or float arithmetic.
        match &cgen.constraints()[2] {
            Constraint::IsNumeric(_, _) => {}
            _ => panic!("Expected IsNumeric constraint for arithmetic result"),
        }
    }

    #[test]
    fn test_constraint_generator_comparison() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: 1 < 2
        let lhs = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let rhs = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(4, 5),
        });
        let lt = rir.add_inst(rue_rir::Inst {
            data: InstData::Lt { lhs, rhs },
            span: Span::new(0, 5),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::BOOL);

        let info = cgen.generate(lt, &mut ctx);

        // Comparisons always return Bool
        assert_eq!(info.ty, InferType::Concrete(Type::BOOL));
        // Should generate 1 constraint: lhs type = rhs type
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_generator_logical_and() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: true && false
        let lhs = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let rhs = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(false),
            span: Span::new(8, 13),
        });
        let and = rir.add_inst(rue_rir::Inst {
            data: InstData::And { lhs, rhs },
            span: Span::new(0, 13),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::BOOL);

        let info = cgen.generate(and, &mut ctx);

        // Logical operators return Bool
        assert_eq!(info.ty, InferType::Concrete(Type::BOOL));
        // Should generate 2 constraints: lhs = bool, rhs = bool
        assert_eq!(cgen.constraints().len(), 2);
    }

    #[test]
    fn test_constraint_generator_negation() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: -42
        let operand = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(1, 3),
        });
        let neg = rir.add_inst(rue_rir::Inst {
            data: InstData::Neg { operand },
            span: Span::new(0, 3),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(neg, &mut ctx);

        // Negation preserves the operand type (now a type variable for the int literal)
        assert!(matches!(info.ty, InferType::Var(_)));
        // Should generate 1 constraint: IsSigned for the result
        assert_eq!(cgen.constraints().len(), 1);
        // Verify it's an IsSigned constraint
        match &cgen.constraints()[0] {
            Constraint::IsSigned(_, _) => {}
            _ => panic!("Expected IsSigned constraint"),
        }
    }

    #[test]
    fn test_constraint_generator_return() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: return 42
        let value = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(7, 9),
        });
        let ret = rir.add_inst(rue_rir::Inst {
            data: InstData::Ret(Some(value)),
            span: Span::new(0, 9),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(ret, &mut ctx);

        // Return is divergent (Never type)
        assert_eq!(info.ty, InferType::Concrete(Type::NEVER));
        // Should generate 1 constraint: return value = return type
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_generator_if_else() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: if true { 1 } else { 2 }
        let cond = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(3, 7),
        });
        let then_val = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(10, 11),
        });
        let else_val = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(20, 21),
        });
        let branch = rir.add_inst(rue_rir::Inst {
            data: InstData::Branch {
                cond,
                then_block: then_val,
                else_block: Some(else_val),
            },
            span: Span::new(0, 25),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(branch, &mut ctx);

        // Result should be a type variable (unified from both branches)
        assert!(info.ty.is_var());
        // Should generate 3 constraints: cond = bool, then = result, else = result
        assert_eq!(cgen.constraints().len(), 3);
    }

    #[test]
    fn test_constraint_generator_while_loop() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: while true { 0 }
        let cond = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(6, 10),
        });
        let body = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(13, 14),
        });
        let loop_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::Loop { cond, body },
            span: Span::new(0, 15),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(loop_inst, &mut ctx);

        // While loops produce Unit
        assert_eq!(info.ty, InferType::Concrete(Type::UNIT));
        // Should generate 1 constraint: cond = bool
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_context_scope() {
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        // Use an interner to create a symbol
        let interner = ThreadedRodeo::new();
        let sym = interner.get_or_intern("x");
        ctx.insert_local(
            sym,
            LocalVarInfo {
                ty: InferType::Concrete(Type::I32),
                is_mut: false,
                span: Span::new(0, 1),
            },
        );

        assert!(ctx.locals.contains_key(&sym));

        // Push a scope and shadow the variable
        ctx.push_scope();
        ctx.insert_local(
            sym,
            LocalVarInfo {
                ty: InferType::Concrete(Type::I64),
                is_mut: true,
                span: Span::new(10, 15),
            },
        );

        // Should see the shadowed version
        let local = ctx.locals.get(&sym).unwrap();
        assert_eq!(local.ty, InferType::Concrete(Type::I64));
        assert!(local.is_mut);

        // Pop scope - should restore original
        ctx.pop_scope();
        let local = ctx.locals.get(&sym).unwrap();
        assert_eq!(local.ty, InferType::Concrete(Type::I32));
        assert!(!local.is_mut);
    }

    #[test]
    fn test_expr_info_creation() {
        let info = ExprInfo::new(InferType::IntLiteral, Span::new(5, 10));
        assert!(info.ty.is_int_literal());
        assert_eq!(info.span, Span::new(5, 10));
    }

    /// Helper to create a non-generic FunctionSig for tests
    fn make_test_func_sig(param_types: Vec<InferType>, return_type: InferType) -> FunctionSig {
        let num_params = param_types.len();
        FunctionSig {
            param_types,
            return_type,
            is_generic: false,
            param_modes: vec![rue_rir::RirParamMode::Normal; num_params],
            param_comptime: vec![false; num_params],
            param_comptime_type: vec![false; num_params],
            param_names: vec![],
            param_type_syntax: vec![],
            return_type_syntax: None,
        }
    }

    #[test]
    fn test_function_sig() {
        let sig = make_test_func_sig(
            vec![
                InferType::Concrete(Type::I32),
                InferType::Concrete(Type::BOOL),
            ],
            InferType::Concrete(Type::I64),
        );
        assert_eq!(sig.param_types.len(), 2);
        assert_eq!(sig.return_type, InferType::Concrete(Type::I64));
    }

    #[test]
    fn test_constraint_generator_infinite_loop() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: loop { 0 }
        let body = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(6, 7),
        });
        let loop_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::InfiniteLoop {
                body,
                iter_borrow: None,
            },
            span: Span::new(0, 10),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(loop_inst, &mut ctx);

        // Infinite loop produces Never (diverges)
        assert_eq!(info.ty, InferType::Concrete(Type::NEVER));
        // No constraints for infinite loop itself
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_break_continue() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        let break_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::Break { value: None },
            span: Span::new(0, 5),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(break_inst, &mut ctx);

        // Break diverges
        assert_eq!(info.ty, InferType::Concrete(Type::NEVER));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_index_get() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: arr[0]
        let base = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0), // Placeholder for array
            span: Span::new(0, 3),
        });
        let index = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(4, 5),
        });
        let index_get = rir.add_inst(rue_rir::Inst {
            data: InstData::IndexGet { base, index },
            span: Span::new(0, 6),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(index_get, &mut ctx);

        // Result is a type variable (element type unknown)
        assert!(info.ty.is_var());
        // Should generate 1 constraint: index must be an integer (spec 7.1:7)
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::IsInteger(_, _) => {}
            _ => panic!("Expected IsInteger constraint for index"),
        }
    }

    #[test]
    fn test_constraint_generator_index_set() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: arr[0] = 42
        let base = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0), // Placeholder for array
            span: Span::new(0, 3),
        });
        let index = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(4, 5),
        });
        let value = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(9, 11),
        });
        let index_set = rir.add_inst(rue_rir::Inst {
            data: InstData::IndexSet { base, index, value },
            span: Span::new(0, 11),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(index_set, &mut ctx);

        // Index assignment produces Unit
        assert_eq!(info.ty, InferType::Concrete(Type::UNIT));
        // Should generate 1 constraint: index must be an integer (spec 7.1:7)
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::IsInteger(_, _) => {}
            _ => panic!("Expected IsInteger constraint for index"),
        }
    }

    #[test]
    fn test_constraint_generator_empty_block() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: { } (empty block)
        let block = rir.add_block(&[], Span::new(0, 2)).unwrap();

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(block, &mut ctx);

        // Empty block produces Unit
        assert_eq!(info.ty, InferType::Concrete(Type::UNIT));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_bitwise_not() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: !42 (bitwise NOT)
        let operand = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(1, 3),
        });
        let bitnot = rir.add_inst(rue_rir::Inst {
            data: InstData::BitNot { operand },
            span: Span::new(0, 3),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(bitnot, &mut ctx);

        // Bitwise NOT preserves the operand type (now a type variable for int literal)
        assert!(matches!(info.ty, InferType::Var(_)));
        // Should generate 1 constraint: IsInteger for the result
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::IsInteger(_, _) => {}
            _ => panic!("Expected IsInteger constraint"),
        }
    }

    #[test]
    fn test_constraint_generator_function_call_arg_count_mismatch() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let mut functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Register a function that takes 2 parameters
        let func_name = interner.get_or_intern("foo");
        functions.insert(
            func_name,
            make_test_func_sig(
                vec![
                    InferType::Concrete(Type::I32),
                    InferType::Concrete(Type::I32),
                ],
                InferType::Concrete(Type::BOOL),
            ),
        );
        let functions_by_file_name = AHashMap::from([((FileId::DEFAULT, func_name), func_name)]);

        // Create a call with only 1 argument (mismatch)
        let arg = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(4, 6),
        });
        let call = rir
            .add_call(
                func_name,
                &[rue_rir::RirCallArg {
                    value: arg,
                    mode: rue_rir::RirArgMode::Normal,
                }],
                Span::new(0, 7),
            )
            .unwrap();

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        )
        .with_functions_by_file_name(&functions_by_file_name);
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::BOOL);

        let info = cgen.generate(call, &mut ctx);

        // Should still return the declared return type
        assert_eq!(info.ty, InferType::Concrete(Type::BOOL));
        // No constraints generated when arg count mismatches (error will be in sema)
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_unknown_function() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new(); // Empty - no functions registered
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create a call to an unknown function
        let unknown_func = interner.get_or_intern("unknown");
        let arg = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(8, 10),
        });
        let call = rir
            .add_call(
                unknown_func,
                &[rue_rir::RirCallArg {
                    value: arg,
                    mode: rue_rir::RirArgMode::Normal,
                }],
                Span::new(0, 11),
            )
            .unwrap();

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(call, &mut ctx);

        // Unknown function returns Error type
        assert_eq!(info.ty, InferType::Concrete(Type::ERROR));
        // Arguments should still be processed (but no constraints generated for them)
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_match_multiple_arms() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();

        // Create: match x { 1 => 10, 2 => 20, _ => 30 }
        let scrutinee = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(5),
            span: Span::new(6, 7),
        });

        // Arm 1: 1 => 10
        let body1 = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(10),
            span: Span::new(15, 17),
        });
        let pattern1 = rue_rir::RirPattern::Int {
            value: 1,
            negative: false,
            span: Span::new(10, 11),
        };

        // Arm 2: 2 => 20
        let body2 = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(20),
            span: Span::new(25, 27),
        });
        let pattern2 = rue_rir::RirPattern::Int {
            value: 2,
            negative: false,
            span: Span::new(20, 21),
        };

        // Arm 3: _ => 30
        let body3 = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(30),
            span: Span::new(35, 37),
        });
        let pattern3 = rue_rir::RirPattern::Wildcard(Span::new(30, 31));

        let match_inst = rir
            .add_match(
                scrutinee,
                &[(pattern1, body1), (pattern2, body2), (pattern3, body3)],
                Span::new(0, 40),
            )
            .unwrap();

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = AHashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(match_inst, &mut ctx);

        // Result should be a type variable (unified from all arm bodies)
        assert!(info.ty.is_var());

        // Should generate 6 constraints:
        // - 3 for pattern types matching scrutinee type (each arm)
        // - 3 for body types matching result type (each arm)
        assert_eq!(cgen.constraints().len(), 6);

        // Verify all constraints are Equal constraints
        for constraint in cgen.constraints() {
            match constraint {
                Constraint::Equal(_, _, _) => {}
                _ => panic!("Expected Equal constraint in match"),
            }
        }
    }

    // --- Scoped resolution of bare const names in array-length and
    // --- type-alias-head positions (RUE-1091 slice r0).
    //
    // A bare name resolves by ordinary scoped resolution — the same by-file
    // keying sema uses (`resolve_const_info_in_file`). A constant in another
    // module does not participate merely because it is globally unique; it is
    // reached qualified. These tests pin that rule and the invariants the
    // maintainer ruling requires: locality (no spooky action), the
    // comptime-value precedence, and the value-vs-type distinction.

    fn cgen_fixture() -> (rue_rir::RirEditor, ThreadedRodeo, TypeInternPool) {
        make_test_rir_interner_and_type_pool()
    }

    #[test]
    fn array_length_bare_const_resolves_in_declaring_file_scope() {
        let (rir, interner, type_pool) = cgen_fixture();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();
        let file_a = FileId::new(0);
        let k = interner.get_or_intern("K");
        let mut const_values: AHashMap<(FileId, Spur), i128> = AHashMap::new();
        const_values.insert((file_a, k), 4);

        let cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        )
        .with_const_values(&const_values);

        // In-scope, same module: resolves to the declared value.
        assert_eq!(
            cgen.resolve_infer_array_length(&ArrayLen::Named("K".to_string()), file_a),
            Some(4)
        );
    }

    #[test]
    fn array_length_bare_const_out_of_scope_does_not_resolve() {
        let (rir, interner, type_pool) = cgen_fixture();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);
        let k = interner.get_or_intern("K");
        // The only `K` lives in file_b; referencing it bare from file_a is out
        // of scope even though `K` is globally unique.
        let mut const_values: AHashMap<(FileId, Spur), i128> = AHashMap::new();
        const_values.insert((file_b, k), 4);

        let cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        )
        .with_const_values(&const_values);

        assert_eq!(
            cgen.resolve_infer_array_length(&ArrayLen::Named("K".to_string()), file_a),
            None
        );
    }

    /// Named regression for the retired whole-program uniqueness fallback: a
    /// same-named constant added in an unrelated module must NOT change a
    /// body's local resolution. Under the old scan two `N`s were "ambiguous"
    /// and a valid local `N` stopped resolving (spooky action at a distance);
    /// scoped resolution keeps file_a's `N` resolving to its own value.
    #[test]
    fn array_length_distant_same_named_const_cannot_perturb_local_resolution() {
        let (rir, interner, type_pool) = cgen_fixture();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);
        let n = interner.get_or_intern("N");
        let mut const_values: AHashMap<(FileId, Spur), i128> = AHashMap::new();
        const_values.insert((file_a, n), 3);
        // An unrelated distant const of the same name.
        const_values.insert((file_b, n), 99);

        let cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        )
        .with_const_values(&const_values);

        // file_a resolves its own N (no ambiguity, no bleed from file_b)...
        assert_eq!(
            cgen.resolve_infer_array_length(&ArrayLen::Named("N".to_string()), file_a),
            Some(3)
        );
        // ...and file_b resolves its own, independently.
        assert_eq!(
            cgen.resolve_infer_array_length(&ArrayLen::Named("N".to_string()), file_b),
            Some(99)
        );
    }

    #[test]
    fn alias_head_bare_name_resolves_in_declaring_file_scope_only() {
        let (rir, interner, type_pool) = cgen_fixture();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);
        let alias = interner.get_or_intern("MyAlias");
        let mut const_type_aliases: AHashMap<(FileId, Spur), Type> = AHashMap::new();
        const_type_aliases.insert((file_a, alias), Type::I32);

        let cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        )
        .with_const_type_aliases(&const_type_aliases);

        // Same module: the alias head resolves to its aliased type.
        assert_eq!(
            cgen.infer_named_type_hint("MyAlias", file_a),
            Some(InferType::Concrete(Type::I32))
        );
        // Another module: out of scope, does not resolve bare.
        assert_eq!(cgen.infer_named_type_hint("MyAlias", file_b), None);
    }

    /// Array lengths need an integer value; alias heads need a type. The two
    /// scoped lookups consult separate typed maps and must not collapse into a
    /// fuzzy "a const named X exists" match: a type-alias name never satisfies
    /// an array length, and an integer const never satisfies an alias head.
    #[test]
    fn scoped_lookups_keep_value_and_type_kinds_distinct() {
        let (rir, interner, type_pool) = cgen_fixture();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();
        let file_a = FileId::new(0);
        let value_name = interner.get_or_intern("K");
        let type_name = interner.get_or_intern("T");
        let mut const_values: AHashMap<(FileId, Spur), i128> = AHashMap::new();
        const_values.insert((file_a, value_name), 4);
        let mut const_type_aliases: AHashMap<(FileId, Spur), Type> = AHashMap::new();
        const_type_aliases.insert((file_a, type_name), Type::I32);

        let cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        )
        .with_const_values(&const_values)
        .with_const_type_aliases(&const_type_aliases);

        // A type-alias name in array-length position is not an integer value.
        assert_eq!(
            cgen.resolve_infer_array_length(&ArrayLen::Named("T".to_string()), file_a),
            None
        );
        // An integer const name in alias-head position is not a type alias.
        assert_eq!(cgen.infer_named_type_hint("K", file_a), None);
    }
}
