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
use crate::sema::ConstValue;
#[cfg(test)]
use crate::types::ArrayLen;
use crate::types::{ModuleId, StructId, TypeKind};
use lasso::{Spur, ThreadedRodeo};
use rue_rir::{InstData, InstRef, RepeatCount, Rir, RirTypeSyntaxNode, RirTypeSyntaxRef};
use rue_span::{FileId, Span};
use std::collections::HashMap;

use ahash::AHashMap;
use std::rc::Rc;

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
/// Constraint generation consults the thirteen declaration-universe families
/// purely by key. Rather than eagerly project the whole universe into owned
/// `HashMap`s before any body is analyzed (the O(universe)-per-body term
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
    /// Return type of the current function.
    pub return_type: Type,
    /// How many loops we're nested inside (for break/continue validation).
    pub loop_depth: u32,
    /// One entry per enclosing loop (innermost last); set to `true` when a
    /// `break` targeting that loop is seen. An infinite loop containing a
    /// break has type `()`; without one it has type `!` (see spec 4.8).
    pub loop_break_stack: Vec<bool>,
    checked_depth: u32,
    /// Scope stack for efficient scope management.
    scope_stack: Vec<Vec<(Spur, Option<LocalVarInfo>)>>,
}

impl<'a> ConstraintContext<'a> {
    /// Create a new context for a function.
    pub fn new(params: &'a AHashMap<Spur, ParamVarInfo>, return_type: Type) -> Self {
        Self {
            locals: AHashMap::new(),
            params,
            return_type,
            loop_depth: 0,
            loop_break_stack: Vec::new(),
            checked_depth: 0,
            scope_stack: Vec::new(),
        }
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
}

impl ExprInfo {
    /// Create a new expression info.
    pub fn new(ty: InferType, span: Span) -> Self {
        Self { ty, span }
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
    /// Deliberately the std hasher while the rest of this generator uses
    /// `AHashMap`: type inference walks this map to pre-create array types
    /// (`pre_create_array_types_from_infer_type`), so its iteration order
    /// decides the pool indices those array types receive, and those indices
    /// are later a sort key (`sort_unstable_by_key(Type::as_u32)`). The order
    /// is observable, so the hasher is not a free choice here.
    expr_types: HashMap<InstRef, InferType>,
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
    /// thirteen declaration-universe families are materialized on first consult
    /// through this seam instead of read from eager maps. `None` in unit tests,
    /// which construct the generator from literal maps.
    lazy: Option<&'a dyn LazyInferenceFacts>,
    /// Type variables allocated for integer literals.
    /// These start as unbound and need to be defaulted to i32 if unconstrained.
    int_literal_vars: Vec<TypeVarId>,
    /// Type variables allocated for string literals. Unlike integer literals,
    /// these default to the canonical core `str` type. Context may still bind
    /// a literal to the trusted standard-library `StrBuf` language item.
    string_literal_vars: Vec<TypeVarId>,
    /// Concrete default for an otherwise-unconstrained string literal.
    string_literal_default: Type,
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
    /// Type intern pool for creating pointer and array types during constraint generation.
    type_pool: &'a TypeInternPool,
}

impl<'a> ConstraintGenerator<'a> {
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
        Self {
            rir,
            interner,
            type_vars: TypeVarAllocator::new(),
            constraints: Vec::new(),
            expr_types: HashMap::new(),
            functions: Some(functions),
            builtin_structs: Some(builtin_structs),
            structs_by_file_name: None,
            builtin_enums: Some(builtin_enums),
            enums_by_file_name: None,
            methods: Some(methods),
            lazy: None,
            int_literal_vars: Vec::new(),
            string_literal_vars: Vec::new(),
            string_literal_default,
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
            type_pool,
        }
    }

    /// Create a generator driven by a demand-population provider (RUE-1091
    /// slice r5b).
    ///
    /// The eager family maps stay `None`; every keyed consult of the thirteen
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
        Self {
            rir,
            interner,
            type_vars: TypeVarAllocator::new(),
            constraints: Vec::new(),
            expr_types: HashMap::new(),
            functions: None,
            builtin_structs: None,
            structs_by_file_name: None,
            builtin_enums: None,
            enums_by_file_name: None,
            methods: None,
            lazy: Some(lazy),
            int_literal_vars: Vec::new(),
            string_literal_vars: Vec::new(),
            string_literal_default,
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
    /// constraint-generation time — an annotated binding, a parameter, a
    /// pointer-returning intrinsic with a fixed result type. Returns `None` for
    /// anything still standing on a type variable (`@raw`, `@ptr_offset`,
    /// `@int_to_ptr`), because this pass has no substitution to consult: the
    /// unifier has not run yet, so an unresolved operand carries no pointee to
    /// read and must be left free rather than guessed (RUE-1341).
    fn concrete_pointee_type(&self, ty: &InferType) -> Option<Type> {
        match ty.as_concrete()?.kind() {
            TypeKind::PtrConst(ptr_id) => Some(self.type_pool.ptr_const_def(ptr_id)),
            TypeKind::PtrMut(ptr_id) => Some(self.type_pool.ptr_mut_def(ptr_id)),
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

    /// Like [`Self::resolve_infer_array_length`], but a comptime *value*
    /// parameter captured at the call site (`values`, e.g. `N=3` for
    /// `make_array(3)`) resolves a named length before the file-level
    /// `const_values` table is consulted. Without this, a generic function
    /// whose return/param type is `[i32; N]` sized by a comptime value param
    /// couldn't resolve `N`, so the type fell back to the `COMPTIME_TYPE`
    /// placeholder and the call was misinferred as returning `type`
    /// (RUE-252).
    #[cfg(test)]
    fn resolve_infer_array_length_with_values(
        &self,
        len: &ArrayLen,
        values: &AHashMap<Spur, i128>,
        file_id: FileId,
    ) -> Option<u64> {
        match len {
            ArrayLen::Literal(n) => Some(*n),
            ArrayLen::Named(name) => {
                let sym = self.interner.get(name)?;
                // A comptime value parameter captured at the call site takes
                // precedence over a file-level `const` of the same name (RUE-252).
                let value = values
                    .get(&sym)
                    .copied()
                    .or_else(|| self.scoped_const_value(sym, file_id))?;
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
        self.constraints.push(constraint);
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
    pub fn expr_types(&self) -> &HashMap<InstRef, InferType> {
        &self.expr_types
    }

    /// Consume the constraint generator and return its generated constraints,
    /// literal variables, expression types, and allocated variable count.
    ///
    /// This is useful when you need ownership of the expression types map.
    /// The `type_var_count` can be used to pre-size the unifier's substitution for better performance.
    pub fn into_parts(
        self,
    ) -> (
        Vec<Constraint>,
        Vec<TypeVarId>,
        Vec<TypeVarId>,
        Type,
        HashMap<InstRef, InferType>,
        u32,
    ) {
        (
            self.constraints,
            self.int_literal_vars,
            self.string_literal_vars,
            self.string_literal_default,
            self.expr_types,
            self.type_vars.count(),
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

            // Float literals have no inference rule yet (ADR-0065 Phase 4,
            // RUE-1069): there is no `f32`/`f64` tag in the packed `Type` and
            // no `comptime_float` for the unifier to bind. Reporting `!` keeps
            // the solver quiet — `!` coerces to anything, so a float operand
            // neither constrains its neighbours nor leaves an unsolved
            // variable behind — and lets the AIR-emission pass be the single
            // place that reports the literal (as the preview gate, or as
            // E1109). Replace this with the real `comptime_float` rule in
            // Phase 4.
            InstData::FloatConst { .. } => InferType::Concrete(Type::NEVER),

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
            InstData::Add { lhs, rhs } => self.generate_add(*lhs, *rhs, ctx),

            // Binary arithmetic: both operands must have the same type, result is that type
            InstData::Sub { lhs, rhs }
            | InstData::Mul { lhs, rhs }
            | InstData::Div { lhs, rhs }
            | InstData::Mod { lhs, rhs } => self.generate_binary_arith(*lhs, *rhs, ctx),

            // Bitwise operations: same as arithmetic
            InstData::BitAnd { lhs, rhs }
            | InstData::BitOr { lhs, rhs }
            | InstData::BitXor { lhs, rhs }
            | InstData::Shl { lhs, rhs }
            | InstData::Shr { lhs, rhs } => self.generate_binary_arith(*lhs, *rhs, ctx),

            // Comparison operators: operands must match, result is bool
            InstData::Eq { lhs, rhs }
            | InstData::Ne { lhs, rhs }
            | InstData::Lt { lhs, rhs }
            | InstData::Gt { lhs, rhs }
            | InstData::Le { lhs, rhs }
            | InstData::Ge { lhs, rhs } => {
                let lhs_info = self.generate(*lhs, ctx);
                let rhs_info = self.generate(*rhs, ctx);
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
                let rhs_info = self.generate(*rhs, ctx);
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
                // Result type is the same as operand type
                let result_ty = operand_info.ty.clone();
                // Must be a signed integer
                self.add_constraint(Constraint::is_signed(result_ty.clone(), span));
                result_ty
            }

            // Logical NOT: operand must be bool
            InstData::Not { operand } => {
                let operand_info = self.generate(*operand, ctx);
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
                } else if let Some(param) = ctx.params.get(name) {
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
                            self.add_constraint(Constraint::equal(
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

                // Alloc produces unit type
                InferType::Concrete(Type::UNIT)
            }

            // Assignment
            InstData::Assign { name, value } => {
                let value_info = self.generate(*value, ctx);
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
                    if !self.is_slice_struct_type(target_ty.clone()) {
                        // Constrain value to match variable type
                        self.add_constraint(Constraint::equal(value_info.ty, target_ty, span));
                    }
                }
                // Assignment produces unit
                InferType::Concrete(Type::UNIT)
            }

            // Return statement
            InstData::Ret(value) => {
                if let Some(val_ref) = value {
                    let value_info = self.generate(*val_ref, ctx);
                    // Constrain return value to match function return type. A
                    // `str` return (ADR-0043 Phase 3, RUE-324) accepts a string
                    // literal (HM type `String`) by coercion; skip strict
                    // equality there and let sema materialize the `str`.
                    if !self.is_slice_struct_type(InferType::Concrete(ctx.return_type)) {
                        self.add_constraint(Constraint::equal(
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
            // type); the `yield` itself diverges like `return`.
            InstData::Yield(value) => {
                let value_info = self.generate(*value, ctx);
                self.add_constraint(Constraint::equal(
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
                // `print(s)` / `println(s)` builtin free functions (RUE-1):
                // generate the argument and yield unit. Semantic analysis
                // validates the shared text family (`StrBuf`, `str`, `Str(N)`),
                // while an unconstrained literal follows the normal edition /
                // preview default.
                // Only when the program hasn't shadowed the name with its own
                // `fn print`/`fn println` (a user definition wins).
                let is_print_builtin = function_key.is_none()
                    && matches!(self.interner.resolve(name), "print" | "println");
                if is_print_builtin {
                    for arg in args.iter() {
                        self.generate(arg.value, ctx);
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
                        let arg_infos: Vec<ExprInfo> = args
                            .iter()
                            .map(|arg| self.generate(arg.value, ctx))
                            .collect();

                        // Build the type substitution map from comptime type arguments
                        let mut type_subst: AHashMap<lasso::Spur, Type> = AHashMap::new();
                        // Comptime VALUE arguments (`comptime N: i32`) captured
                        // as their integer constant, so a return/param type
                        // sized by one — an array length `[i32; N]` — resolves
                        // at this call (RUE-252).
                        let mut value_subst: AHashMap<lasso::Spur, i128> = AHashMap::new();
                        for (i, arg) in args.iter().enumerate() {
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
                            } else if let Some(v) = self.extract_int_argument(arg.value) {
                                value_subst.insert(func.param_names[i], v);
                            }
                        }

                        // Constrain each runtime argument to its parameter type, with
                        // type parameters substituted. Comptime type parameters (the
                        // `T: type` arguments themselves) are validated in sema.
                        for (i, arg_info) in arg_infos.iter().enumerate() {
                            if i >= func.param_types.len() || i >= func.param_comptime.len() {
                                break;
                            }
                            let declared = &func.param_types[i];
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
                                    if is_type_call {
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
                            self.generate(arg.value, ctx);
                        }
                        // Return the declared return type (error will be caught in sema)
                        func.return_type.clone()
                    } else {
                        // Generate constraints for each argument
                        for (arg, param_ty) in args.iter().zip(func.param_types.iter()) {
                            let arg_info = self.generate(arg.value, ctx);
                            // Slice parameters coerce from an array argument
                            // (`borrow arr`); skip strict equality and let sema
                            // materialize the fat pointer (ADR-0043, RUE-322).
                            if self.is_slice_struct_type(param_ty.clone()) {
                                continue;
                            }
                            self.add_constraint(Constraint::equal(
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
                        self.generate(arg.value, ctx);
                    }
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Intrinsic call
            InstData::Intrinsic { name, args } => {
                let intrinsic_name = self.interner.resolve(name);
                let args = self.rir.intrinsic_args(args);

                if intrinsic_name == "intCast"
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
                        let _ = self.generate(*arg_ref, ctx);
                    }
                    // Return type is inferred from context - create a fresh type variable
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "panic" {
                    // `@panic` diverges: it aborts the process and never returns,
                    // so its expression type is `!` (never), a control-transfer
                    // form that participates in never coercion (spec 3.4:2,
                    // 4.13:5b; formal core §5.7; RUE-512). Keeping it explicit
                    // here — rather than leaning on the generic unit fallback —
                    // stops HM and semantic analysis from drifting apart.
                    for arg_ref in args.iter() {
                        // Text-taking intrinsics accept every stable text view.
                        // Leave literals unconstrained so they take the
                        // canonical `str` default when std is not imported.
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::NEVER)
                } else if intrinsic_name == "assert" {
                    // `@assert` is NOT never-typed: on the success path it returns
                    // and evaluates to `()`. It only aborts when the condition is
                    // false, so its static type is unit on both paths (spec
                    // 4.13:5b). Keep it explicit so HM and sema stay in lockstep.
                    for arg_ref in args.iter() {
                        // As with `@panic`, a literal message keeps the stable
                        // `str` default instead of requiring imported StrBuf.
                        self.generate(*arg_ref, ctx);
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
                        self.generate(*arg_ref, ctx);
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
                        let info = self.generate(*arg_ref, ctx);
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
                        let info = self.generate(*arg_ref, ctx);
                        self.add_constraint(Constraint::equal(
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
                        let info = self.generate(*arg_ref, ctx);
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
                        let info = self.generate(*arg_ref, ctx);
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
                        let info = self.generate(*arg_ref, ctx);
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
                        self.generate(*arg_ref, ctx);
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
                        let info = self.generate(*arg_ref, ctx);
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
                                    self.add_constraint(Constraint::equal(
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
                        let info = self.generate(*arg_ref, ctx);
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
                    // We create a fresh type variable for proper inference.
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "raw"
                    || intrinsic_name == "raw_mut"
                    || intrinsic_name == "field_ptr"
                {
                    // @raw / @raw_mut / @field_ptr: takes a place, returns a
                    // pointer to it (RUE-301). The return type is a pointer
                    // whose pointee is only known once the operand is analyzed,
                    // so we create a fresh type variable for proper inference
                    // (Sema fixes it to `ptr const T`/`ptr mut T`).
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "alloc" || intrinsic_name == "alloc_zeroed" {
                    // @alloc(size: u64, align: u64) -> ptr mut u8 and its
                    // zeroing twin (ADR-0059 Phase 3, RUE-961/RUE-968). Both
                    // operands are physical byte counts, so both are u64 and
                    // the result type is fixed rather than context-inferred.
                    for arg_ref in args.iter() {
                        let info = self.generate(*arg_ref, ctx);
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
                        let info = self.generate(*arg_ref, ctx);
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
                        let info = self.generate(*arg_ref, ctx);
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
                        let info = self.generate(*arg_ref, ctx);
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
                        let info = self.generate(*arg_ref, ctx);
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
                        self.generate(*arg_ref, ctx);
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
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::new_module(crate::types::ModuleId::UNRESOLVED))
                } else if intrinsic_name == "dbg"
                    || intrinsic_name == "drop"
                    || intrinsic_name == "test_preview_gate"
                {
                    // The remaining known intrinsics all return unit.
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::UNIT)
                } else {
                    // Unknown intrinsic: a fresh var, so sema can reject it with
                    // E0700 naming the bogus intrinsic instead of inference
                    // masking it with a type-mismatch against the context's
                    // expected type — the same treatment @cast gets (RUE-319,
                    // here RUE-1281).
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Var(self.fresh_var())
                }
            }

            InstData::InternalIntrinsic { intrinsic, args } => {
                let args = self.rir.internal_intrinsic_args(args);
                for arg_ref in args {
                    self.generate(arg_ref, ctx);
                }
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
                let block_insts = self.rir.block_insts(instructions);
                for block_inst_ref in block_insts.values() {
                    let info = self.generate(block_inst_ref, ctx);
                    last_ty = info.ty;
                }
                self.exit_scope(ctx);
                last_ty
            }

            // Branch (if/else)
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let cond_info = self.generate(*cond, ctx);
                self.add_constraint(Constraint::equal(
                    cond_info.ty,
                    InferType::Concrete(Type::BOOL),
                    cond_info.span,
                ));

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
                if self.comptime_values.is_some()
                    && let Some(ConstValue::Bool(taken)) = self.eval_comptime_value(*cond)
                {
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
                            info.ty
                        }
                        // `if false { .. }` with no else: nothing runs, unit.
                        None => InferType::Concrete(Type::UNIT),
                    };
                    self.record_type(inst_ref, result_ty.clone());
                    return ExprInfo::new(result_ty, span);
                }

                let then_info = self.generate(*then_block, ctx);

                if let Some(else_ref) = else_block {
                    let else_info = self.generate(*else_ref, ctx);

                    // Handle Never type coercion:
                    // - If one branch is Never, the if-else takes the other branch's type
                    // - If both are Never, the result is Never
                    // - Otherwise, both must unify to the same type
                    let then_is_never = matches!(&then_info.ty, InferType::Concrete(Type::NEVER));
                    let else_is_never = matches!(&else_info.ty, InferType::Concrete(Type::NEVER));

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
                    // (or the then branch type if it's unit-compatible)
                    InferType::Concrete(Type::UNIT)
                }
            }

            // While loop
            InstData::Loop { cond, body } => {
                let cond_info = self.generate(*cond, ctx);
                self.add_constraint(Constraint::equal(
                    cond_info.ty,
                    InferType::Concrete(Type::BOOL),
                    cond_info.span,
                ));

                ctx.loop_depth += 1;
                ctx.loop_break_stack.push(false);
                self.generate(*body, ctx);
                ctx.loop_break_stack.pop();
                ctx.loop_depth -= 1;

                // Loops produce unit
                InferType::Concrete(Type::UNIT)
            }

            // Infinite loop
            InstData::InfiniteLoop { body, .. } => {
                ctx.loop_depth += 1;
                ctx.loop_break_stack.push(false);
                self.generate(*body, ctx);
                let has_break = ctx.loop_break_stack.pop().unwrap_or(false);
                ctx.loop_depth -= 1;

                // An infinite loop with a break targeting it exits with unit;
                // without one it never returns (see spec 4.8:17 / 4.8:21).
                if has_break {
                    InferType::Concrete(Type::UNIT)
                } else {
                    InferType::Concrete(Type::NEVER)
                }
            }

            // Break/Continue
            InstData::Break { value } => {
                match value {
                    None => {
                        // Record the break against the innermost enclosing loop.
                        if let Some(broke) = ctx.loop_break_stack.last_mut() {
                            *broke = true;
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
            InstData::Continue => InferType::Concrete(Type::NEVER),

            // Match expression
            InstData::Match { scrutinee, arms } => {
                let scrutinee_info = self.generate(*scrutinee, ctx);
                let arms = self.rir.match_arms(arms);

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
                if let Some(selected) = self.comptime_selected_arm(*scrutinee, arms.iter()) {
                    self.enter_scope(ctx);
                    let body_info = self.generate(selected, ctx);
                    self.exit_scope(ctx);
                    self.record_type(inst_ref, body_info.ty.clone());
                    return ExprInfo::new(body_info.ty, span);
                }

                // Collect arm types, handling Never coercion
                let mut arm_types: Vec<ExprInfo> = Vec::new();
                for (pattern, body) in arms.iter() {
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
                    if let rue_rir::RirPatternView::Path {
                        module,
                        ctor_head,
                        type_name,
                        variant,
                        bindings,
                        span: pat_span,
                        ..
                    } = &pattern
                    {
                        if !bindings.is_empty() {
                            // An inline type-constructor pattern head
                            // (`Result(i32,i32).Ok(v)`, RUE-596) has no comptime
                            // interpreter in the inference engine, but sema
                            // pre-reduced it in `inline_ctor_head_types`
                            // (RUE-950/RUE-954) — consult that first so the
                            // payload bindings are pre-typed and a sibling
                            // arm's literal sees the join's expectation. Sema's
                            // `materialize_match_bindings` stays authoritative
                            // via `try_evaluate_const`.
                            let enum_ty = ctor_head
                                .and_then(|head| {
                                    self.inline_ctor_head_types
                                        .and_then(|heads| heads.get(&head).copied())
                                })
                                .or_else(|| {
                                    module.and_then(|module_ref| {
                                        self.enum_type_for_module(module_ref, type_name)
                                    })
                                })
                                .or_else(|| self.enum_type_for(type_name, pat_span.file_id));
                            if let Some(payload) = enum_ty
                                .and_then(|ty| ty.as_enum())
                                .map(|id| self.type_pool.enum_def(id))
                                .and_then(|def| {
                                    def.find_variant(self.interner.resolve(variant))
                                        .map(|v| def.variant_payload(v).to_vec())
                                })
                            {
                                for (i, bname) in bindings.iter().enumerate() {
                                    // A `_` payload (RUE-601) binds nothing —
                                    // skip registering a local for it.
                                    if self.interner.resolve(&bname) == "_" {
                                        continue;
                                    }
                                    if let Some(&pty) = payload.get(i) {
                                        ctx.insert_local(
                                            *bname,
                                            LocalVarInfo {
                                                ty: InferType::Concrete(pty),
                                                is_mut: false,
                                                span: *pat_span,
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }

                    // Generate body and collect its type
                    let body_info = self.generate(body, ctx);
                    self.exit_scope(ctx);
                    arm_types.push(body_info);
                }

                // Handle Never type coercion:
                // Filter out Never arms and use the remaining non-Never types
                let non_never_arms: Vec<_> = arm_types
                    .iter()
                    .filter(|info| !matches!(&info.ty, InferType::Concrete(Type::NEVER)))
                    .collect();

                if non_never_arms.is_empty() {
                    // All arms diverge - result is Never
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
                        let value_info = self.generate(value_ref, ctx);
                        if let Some(field_ty) = self.field_type_of(struct_ty, field_name) {
                            let expected = self.type_to_infer(field_ty);
                            // A `str` field (ADR-0043 Phase 3, RUE-324) accepts a
                            // string literal (HM type `String`) by coercion; skip
                            // strict equality and let sema materialize the `str`.
                            if !self.is_slice_struct_type(expected.clone()) {
                                self.add_constraint(Constraint::equal(
                                    value_info.ty,
                                    expected,
                                    span,
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
                        self.generate(value_ref, ctx);
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
                    && !ctx.params.contains_key(&name)
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
                        match member_const_ty.or(member_module_ty) {
                            Some(member_ty) => self.type_to_infer(member_ty),
                            None => InferType::Var(self.fresh_var()),
                        }
                    }
                }
            }

            // Field assignment
            InstData::FieldSet { base, field, value } => {
                let base_info = self.generate(*base, ctx);
                let value_info = self.generate(*value, ctx);
                // Constrain the assigned value against the field's declared
                // type, so a literal RHS is range-checked at the field's width
                // instead of wrapping (`s.a = 300` with `a: u8` must be
                // rejected rather than truncate to 44). (RUE-104)
                if let Some(field_ty) = self.known_field_type(&base_info.ty, *field) {
                    let expected = self.type_to_infer(field_ty);
                    self.add_constraint(Constraint::equal(value_info.ty, expected, span));
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
                    let first_info = self.generate(elements.get(0).unwrap(), ctx);
                    for elem_ref in elements.values().skip(1) {
                        let elem_info = self.generate(elem_ref, ctx);
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
            // count is a compile-time constant resolved here (literal or a
            // file-level `const`); an unresolved count (e.g. a `comptime` value
            // parameter, only known at specialization) yields a fresh variable
            // and is resolved/diagnosed by sema.
            InstData::ArrayRepeat { value, count } => {
                let value_info = self.generate(*value, ctx);
                let resolved = match count {
                    RepeatCount::Literal(n) => Some(*n),
                    RepeatCount::Named(sym) => self
                        .const_value((span.file_id, *sym))
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
                        _ => {
                            // Base might be a type variable that will resolve to an array.
                            // Use a fresh variable for the element type.
                            let result_var = self.fresh_var();
                            InferType::Var(result_var)
                        }
                    }
                }
            }

            // Array index assignment
            InstData::IndexSet { base, index, value } => {
                let base_info = self.generate(*base, ctx);
                let index_info = self.generate(*index, ctx);
                // Index must be an integer type (signed or unsigned) per spec
                // 7.1:7. Negative/out-of-range indices trap at runtime via the
                // bounds check, not at compile time (RUE-81/RUE-87).
                self.add_constraint(Constraint::is_integer(index_info.ty, index_info.span));

                let value_info = self.generate(*value, ctx);

                // Constrain value type to match array element type
                if let InferType::Array { element, .. } = &base_info.ty {
                    self.add_constraint(Constraint::equal(
                        value_info.ty,
                        (**element).clone(),
                        value_info.span,
                    ));
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
                    && !ctx.params.contains_key(&name)
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
                        ExprInfo::new(ty, span)
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
                            if ctx.locals.contains_key(&name) || ctx.params.contains_key(&name))
                    && let Some(member_ty) = self
                        .struct_type_for_module(module_ref, &type_name)
                        .or_else(|| self.enum_type_for_module(module_ref, &type_name))
                    && let Some(result) =
                        self.generate_call_on_reduced_type(member_ty, *method, args, span, ctx)
                {
                    self.record_type(inst_ref, result.clone());
                    return ExprInfo::new(result, span);
                }

                // Generate type for receiver
                let receiver_info = self.generate(*receiver, ctx);
                let call_args = self.rir.call_args(args);

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
                        let arg_info = self.generate(arg.value, ctx);
                        self.add_constraint(Constraint::equal(
                            arg_info.ty,
                            param_type.clone(),
                            arg_info.span,
                        ));
                    }
                    self.record_type(inst_ref, return_type.clone());
                    return ExprInfo::new(return_type, span);
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
                                    let arg_info = self.generate(arg.value, ctx);
                                    // Slice and `borrow str` parameters coerce
                                    // from a `borrow` argument; skip strict
                                    // equality and let sema materialize the
                                    // fat-pointer view (ADR-0043, RUE-322,
                                    // RUE-559) — same as the direct-Call path.
                                    if self.is_slice_struct_type(param_ty.clone()) {
                                        continue;
                                    }
                                    self.add_constraint(Constraint::equal(
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
                                let arg_infos: Vec<ExprInfo> = call_args
                                    .iter()
                                    .map(|arg| self.generate(arg.value, ctx))
                                    .collect();
                                let mut type_subst: AHashMap<lasso::Spur, Type> = AHashMap::new();
                                let mut value_subst: AHashMap<lasso::Spur, i128> = AHashMap::new();
                                for (i, arg) in call_args.iter().enumerate() {
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
                                    } else if let Some(v) = self.extract_int_argument(arg.value) {
                                        value_subst.insert(func.param_names[i], v);
                                    }
                                }
                                for (i, arg_info) in arg_infos.iter().enumerate() {
                                    if i >= func.param_types.len() || i >= func.param_comptime.len()
                                    {
                                        break;
                                    }
                                    if func.param_comptime_type.get(i) == Some(&true) {
                                        continue;
                                    }
                                    let declared = &func.param_types[i];
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
                                    self.generate(arg.value, ctx);
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
                                self.generate(arg.value, ctx);
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
                                self.generate(arg.value, ctx);
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
                            if let Some(method_sig) = self.method_sig(&method_key) {
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
                                    let arg_info = self.generate(arg.value, ctx);
                                    if !defer_equality {
                                        self.add_constraint(Constraint::equal(
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
                                    self.generate(arg.value, ctx);
                                }
                                InferType::Concrete(Type::ERROR)
                            }
                        } else {
                            // Non-struct receiver - sema will report the error
                            for arg in call_args.iter() {
                                self.generate(arg.value, ctx);
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
                                self.generate(arg.value, ctx);
                            }
                            InferType::Var(self.fresh_var())
                        }
                    }
                };

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

        // Record the type for this expression
        self.record_type(inst_ref, ty.clone());
        ExprInfo::new(ty, span)
    }

    /// Generate constraints for a binary arithmetic operation.
    fn generate_binary_arith(
        &mut self,
        lhs: InstRef,
        rhs: InstRef,
        ctx: &mut ConstraintContext,
    ) -> InferType {
        let lhs_info = self.generate(lhs, ctx);
        let rhs_info = self.generate(rhs, ctx);

        // A diverging operand (`!`, e.g. `n - match m {}`) makes the whole
        // expression diverge. Never coerces to any type (spec 3.4:3-4), so
        // don't constrain the operands to one another — doing so would drag an
        // integer literal to `!` and then bogusly range-check it against `!`
        // (RUE-270). The result is `!`; the surrounding context coerces it.
        if Self::is_never_concrete(&lhs_info.ty) || Self::is_never_concrete(&rhs_info.ty) {
            return InferType::Concrete(Type::NEVER);
        }

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
        self.add_constraint(Constraint::is_integer(result_ty.clone(), lhs_info.span));

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
        lhs: InstRef,
        rhs: InstRef,
        ctx: &mut ConstraintContext,
    ) -> InferType {
        let lhs_info = self.generate(lhs, ctx);
        let rhs_info = self.generate(rhs, ctx);

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
        self.add_constraint(Constraint::is_integer(result_ty.clone(), lhs_info.span));
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
                self.generate(arg.value, ctx);
            }
            return InferType::Concrete(Type::ERROR);
        }

        // Type not found - sema reports the error; still process args.
        let args = self.rir.call_args(args);
        for arg in args.iter() {
            self.generate(arg.value, ctx);
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
            for (i, arg) in args.iter().enumerate() {
                let arg_info = self.generate(arg.value, ctx);
                if let Some(&pty) = payload.get(i) {
                    // Convert the declared payload type structurally so an array
                    // payload (`[i32; 2]`) unifies with an array-literal argument
                    // and propagates the expected element type into its literal
                    // elements — exactly as struct-field init does (RUE-260).
                    let expected = self.type_to_infer(pty);
                    self.add_constraint(Constraint::equal(arg_info.ty, expected, arg_info.span));
                }
            }
            return Some(InferType::Concrete(ty));
        }
        let struct_id = ty.as_struct()?;
        let method_sig = self.method_sig(&(struct_id, function))?;
        let args = self.rir.call_args(args);
        for (arg, param_type) in args.iter().zip(method_sig.param_types.iter()) {
            let defer_equality = self.is_slice_struct_type(param_type.clone());
            let arg_info = self.generate(arg.value, ctx);
            if !defer_equality {
                self.add_constraint(Constraint::equal(
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

    /// Get the inferred type for a pattern.
    fn pattern_type(&mut self, pattern: &rue_rir::RirPatternView<'_>) -> InferType {
        match pattern {
            rue_rir::RirPatternView::Wildcard(_) => {
                // Wildcard matches anything - use a fresh type variable
                let var = self.fresh_var();
                InferType::Var(var)
            }
            rue_rir::RirPatternView::Int { .. } => InferType::IntLiteral,
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
    /// Handles type literals (`i32`, `bool`, ...), named struct/enum types, and
    /// forwarded type parameters (a reference to `T` inside a specialized generic
    /// body, resolved via `self.type_subst`). Returns `None` for type values that
    /// are only known to semantic analysis (e.g. a local variable bound to an
    /// anonymous struct type) - those are type-checked in sema instead.
    fn extract_type_argument(&self, arg: InstRef, ctx: &ConstraintContext) -> Option<Type> {
        let resolve_sym = |sym: &Spur| -> Option<Type> {
            if let Some(subst) = self.type_subst {
                if let Some(&ty) = subst.get(sym) {
                    return Some(ty);
                }
            }
            if let Some(ty) = self.builtin_struct_type(*sym) {
                return Some(ty);
            }
            if let Some(ty) = self.builtin_enum_type(*sym) {
                return Some(ty);
            }
            None
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
                // has no concrete type here, so don't resolve it — a runtime
                // value passed to a `comptime T: type` param is still rejected
                // in sema. Forwarded type parameters (`T` inside a specialized
                // generic body) are not in scope as runtime params/locals and
                // resolve via `self.type_subst` above.
                if ctx.locals.contains_key(name) || ctx.params.contains_key(name) {
                    return None;
                }
                resolve_sym(name)
            }
            _ => None,
        }
    }

    /// Extract a comptime *value* argument as an integer constant, for
    /// resolving an array length parameterized by a comptime value param
    /// (`fn f(comptime N: i32) -> [i32; N]`, RUE-252/RUE-553). Evaluates any
    /// compile-time-known integer expression — a literal, a `const`/comptime
    /// value reference, and arithmetic over them (`make(1 + 2)`) — via
    /// [`Self::eval_comptime_value`]; non-integer or non-comptime forms
    /// (checked fully in sema) yield `None`.
    fn extract_int_argument(&self, arg: InstRef) -> Option<i128> {
        match self.eval_comptime_value(arg)? {
            ConstValue::Integer(n) => Some(n),
            _ => None,
        }
    }

    /// Evaluate a compile-time-known integer/boolean expression against the
    /// comptime value parameters (`comptime_values`) and file-level integer
    /// constants (`const_values`) currently in scope, returning its
    /// [`ConstValue`]. Handles literals, `const`/comptime references, unary
    /// `-`/`!`, integer arithmetic (`+ - * / %`), the comparison operators,
    /// and boolean `&& ||`.
    ///
    /// Returns `None` for any form not statically decidable here — a runtime
    /// value, a call, an operation that could trap (division or modulo by
    /// zero) or overflow `i128`, or a kind mismatch (comparing an integer to a
    /// bool). This mirrors the *values* sema's comptime evaluator produces for
    /// the same expressions so inference and sema agree on which branch/arm is
    /// live (RUE-553/RUE-554); when in doubt it returns `None`, keeping it a
    /// strict subset of what sema evaluates, so the caller safely falls back
    /// to its runtime path.
    fn eval_comptime_value(&self, inst: InstRef) -> Option<ConstValue> {
        use ConstValue::{Bool, Integer};
        match &self.rir.get(inst).data {
            InstData::IntConst(v) => Some(Integer(*v as i128)),
            InstData::BoolConst(b) => Some(Bool(*b)),
            InstData::VarRef { name, .. } => self
                .comptime_values
                .and_then(|m| m.get(name).copied())
                .or_else(|| {
                    let file_id = self.rir.get(inst).span.file_id;
                    self.const_value((file_id, *name)).map(Integer)
                }),
            InstData::Neg { operand } => match self.eval_comptime_value(*operand)? {
                Integer(n) => Some(Integer(n.checked_neg()?)),
                _ => None,
            },
            InstData::Not { operand } => match self.eval_comptime_value(*operand)? {
                Bool(b) => Some(Bool(!b)),
                _ => None,
            },
            InstData::Add { lhs, rhs } => self.eval_int_binop(*lhs, *rhs, i128::checked_add),
            InstData::Sub { lhs, rhs } => self.eval_int_binop(*lhs, *rhs, i128::checked_sub),
            InstData::Mul { lhs, rhs } => self.eval_int_binop(*lhs, *rhs, i128::checked_mul),
            // `checked_div`/`checked_rem` return `None` on divide-by-zero, so a
            // trapping operation falls back to the runtime path rather than
            // diverging from sema (which reports the error).
            InstData::Div { lhs, rhs } => self.eval_int_binop(*lhs, *rhs, i128::checked_div),
            InstData::Mod { lhs, rhs } => self.eval_int_binop(*lhs, *rhs, i128::checked_rem),
            InstData::Eq { lhs, rhs } => self.eval_eq(*lhs, *rhs, true),
            InstData::Ne { lhs, rhs } => self.eval_eq(*lhs, *rhs, false),
            InstData::Lt { lhs, rhs } => self.eval_int_cmp(*lhs, *rhs, |a, b| a < b),
            InstData::Gt { lhs, rhs } => self.eval_int_cmp(*lhs, *rhs, |a, b| a > b),
            InstData::Le { lhs, rhs } => self.eval_int_cmp(*lhs, *rhs, |a, b| a <= b),
            InstData::Ge { lhs, rhs } => self.eval_int_cmp(*lhs, *rhs, |a, b| a >= b),
            InstData::And { lhs, rhs } => self.eval_bool_binop(*lhs, *rhs, |a, b| a && b),
            InstData::Or { lhs, rhs } => self.eval_bool_binop(*lhs, *rhs, |a, b| a || b),
            _ => None,
        }
    }

    /// Evaluate both operands as comptime integers and combine them with a
    /// checked arithmetic op; `None` if either operand isn't a comptime
    /// integer or the op traps/overflows. See [`Self::eval_comptime_value`].
    fn eval_int_binop(
        &self,
        lhs: InstRef,
        rhs: InstRef,
        f: fn(i128, i128) -> Option<i128>,
    ) -> Option<ConstValue> {
        let a = self.eval_comptime_int(lhs)?;
        let b = self.eval_comptime_int(rhs)?;
        f(a, b).map(ConstValue::Integer)
    }

    /// Evaluate both operands as comptime integers and compare them, yielding a
    /// boolean. See [`Self::eval_comptime_value`].
    fn eval_int_cmp(
        &self,
        lhs: InstRef,
        rhs: InstRef,
        f: fn(i128, i128) -> bool,
    ) -> Option<ConstValue> {
        let a = self.eval_comptime_int(lhs)?;
        let b = self.eval_comptime_int(rhs)?;
        Some(ConstValue::Bool(f(a, b)))
    }

    /// Evaluate both operands as comptime booleans and combine them. `None` if
    /// either operand isn't a comptime bool. See [`Self::eval_comptime_value`].
    fn eval_bool_binop(
        &self,
        lhs: InstRef,
        rhs: InstRef,
        f: fn(bool, bool) -> bool,
    ) -> Option<ConstValue> {
        let a = self.eval_comptime_bool(lhs)?;
        let b = self.eval_comptime_bool(rhs)?;
        Some(ConstValue::Bool(f(a, b)))
    }

    /// `==`/`!=` over two comptime values of the *same* kind (both integers or
    /// both booleans). A kind mismatch — comparing an integer to a bool — is
    /// ill-typed and left to sema, so this returns `None` rather than a
    /// defined-but-misleading answer. See [`Self::eval_comptime_value`].
    fn eval_eq(&self, lhs: InstRef, rhs: InstRef, want_equal: bool) -> Option<ConstValue> {
        use ConstValue::{Bool, Integer};
        let a = self.eval_comptime_value(lhs)?;
        let b = self.eval_comptime_value(rhs)?;
        let equal = match (a, b) {
            (Integer(x), Integer(y)) => x == y,
            (Bool(x), Bool(y)) => x == y,
            _ => return None,
        };
        Some(Bool(equal == want_equal))
    }

    /// Evaluate `inst` as a comptime integer, or `None` if it isn't one.
    fn eval_comptime_int(&self, inst: InstRef) -> Option<i128> {
        match self.eval_comptime_value(inst)? {
            ConstValue::Integer(n) => Some(n),
            _ => None,
        }
    }

    /// Evaluate `inst` as a comptime boolean, or `None` if it isn't one.
    fn eval_comptime_bool(&self, inst: InstRef) -> Option<bool> {
        match self.eval_comptime_value(inst)? {
            ConstValue::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// If `scrutinee` is a comptime value known for this specialization and the
    /// arm patterns form an exhaustive set this understands (integer/boolean
    /// literals plus a wildcard), return the body of the single arm the value
    /// selects. Mirrors sema's comptime arm selection (`analyze_match`, spec
    /// 4.14:19) so inference constrains only the selected arm; the constraint
    /// generator otherwise cross-constrains every arm and rejects a valid
    /// program whose statically-unselected arm has a different type (RUE-268).
    ///
    /// Returns `None` — meaning "constrain all arms as a runtime match" — for
    /// any shape not understood (a scrutinee that isn't comptime-evaluable, an
    /// enum pattern, a non-exhaustive set). A compound comptime scrutinee
    /// (`match n + 0`) is handled the same as a bare `match n`, since both go
    /// through [`Self::eval_comptime_value`] (RUE-554). The comptime cases
    /// handled here are a strict subset of those sema prunes with the same
    /// value and patterns, so whenever this prunes, sema also prunes to the
    /// *same* arm — the only arm whose body inference generated a type for.
    fn comptime_selected_arm<'r>(
        &self,
        scrutinee: InstRef,
        arms: impl Iterator<Item = (rue_rir::RirPatternView<'r>, InstRef)>,
    ) -> Option<InstRef> {
        use rue_rir::RirPatternView as RirPattern;

        // Only prune inside a specialization that has comptime value params in
        // scope (mirrors sema's `!ctx.comptime_value_vars.is_empty()` gate in
        // `analyze_match`); ordinary functions treat every match as runtime.
        self.comptime_values?;
        // Evaluate the scrutinee with the shared comptime evaluator, so a
        // compound scrutinee (`match n + 0 { .. }`) prunes exactly like the
        // bare `match n` form — matching sema's `try_evaluate_const_in_fn`
        // rather than a syntax-specific extraction (RUE-554).
        let value: ConstValue = self.eval_comptime_value(scrutinee)?;

        let mut selected: Option<InstRef> = None;
        let mut has_wildcard = false;
        let mut bool_true_covered = false;
        let mut bool_false_covered = false;
        for (pattern, body) in arms {
            let matched = match &pattern {
                RirPattern::Wildcard(_) => {
                    has_wildcard = true;
                    true
                }
                RirPattern::Int {
                    value: magnitude,
                    negative,
                    ..
                } => match value {
                    ConstValue::Integer(n) => {
                        let pat = if *negative {
                            -(*magnitude as i128)
                        } else {
                            *magnitude as i128
                        };
                        pat == n
                    }
                    // Value/pattern kind mismatch — fall back to runtime path.
                    _ => return None,
                },
                RirPattern::Bool(b, _) => match value {
                    ConstValue::Bool(v) => {
                        if *b {
                            bool_true_covered = true;
                        } else {
                            bool_false_covered = true;
                        }
                        *b == v
                    }
                    _ => return None,
                },
                // Enum patterns: not understood here — runtime path.
                _ => return None,
            };
            if matched && selected.is_none() {
                selected = Some(body);
            }
        }

        // Exhaustiveness is a property of the pattern set (spec 4.7:9), still
        // required when the value is comptime-known: a wildcard, or both bool
        // values for a bool scrutinee. A non-exhaustive set falls back so sema
        // reports the proper diagnostic on the runtime path.
        let exhaustive = has_wildcard
            || (matches!(value, ConstValue::Bool(_)) && bool_true_covered && bool_false_covered);
        if exhaustive { selected } else { None }
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
        // Should generate 3 constraints: lhs = result, rhs = result, IsInteger(result)
        assert_eq!(cgen.constraints().len(), 3);
        // Verify the third constraint is IsInteger
        match &cgen.constraints()[2] {
            Constraint::IsInteger(_, _) => {}
            _ => panic!("Expected IsInteger constraint for arithmetic result"),
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

    /// A comptime value parameter captured at the call site takes precedence
    /// over a file-level `const` of the same name (RUE-252). Preserving this
    /// ordering is required by the maintainer ruling.
    #[test]
    fn array_length_comptime_value_param_precedes_file_const() {
        let (rir, interner, type_pool) = cgen_fixture();
        let functions = AHashMap::new();
        let structs = AHashMap::new();
        let enums = AHashMap::new();
        let methods: AHashMap<(StructId, Spur), MethodSig> = AHashMap::new();
        let file_a = FileId::new(0);
        let n = interner.get_or_intern("N");
        let mut const_values: AHashMap<(FileId, Spur), i128> = AHashMap::new();
        const_values.insert((file_a, n), 9);

        let cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        )
        .with_const_values(&const_values);

        // With a comptime value binding N=5, the value parameter wins over the
        // file-level const N=9.
        let mut values: AHashMap<Spur, i128> = AHashMap::new();
        values.insert(n, 5);
        assert_eq!(
            cgen.resolve_infer_array_length_with_values(
                &ArrayLen::Named("N".to_string()),
                &values,
                file_a
            ),
            Some(5)
        );
        // With no value binding, the same-file const supplies the length.
        let empty: AHashMap<Spur, i128> = AHashMap::new();
        assert_eq!(
            cgen.resolve_infer_array_length_with_values(
                &ArrayLen::Named("N".to_string()),
                &empty,
                file_a
            ),
            Some(9)
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
