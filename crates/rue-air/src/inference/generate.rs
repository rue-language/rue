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
use crate::types::{
    ArrayLen, PtrMutability, StructId, TypeKind, parse_array_type_syntax,
    parse_pointer_type_syntax, parse_type_call_syntax,
};
use lasso::{Spur, ThreadedRodeo};
use rue_rir::{InstData, InstRef, RepeatCount, Rir};
use rue_span::{FileId, Span};
use std::collections::HashMap;

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
    /// Parameter names, needed for type substitution in generic returns.
    pub param_names: Vec<lasso::Spur>,
    /// Each parameter's declared type, as the source-level type name symbol
    /// (e.g. the symbol for "T" in `x: T`). Used to look up which comptime
    /// type parameter a generic parameter refers to.
    pub param_type_syms: Vec<lasso::Spur>,
    /// The return type as a symbol (used for substitution lookup).
    pub return_type_sym: lasso::Spur,
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
    /// Return type, as InferType for uniform handling.
    pub return_type: InferType,
}

/// Context for constraint generation within a single function.
pub struct ConstraintContext<'a> {
    /// Local variables in scope.
    pub locals: HashMap<Spur, LocalVarInfo>,
    /// Function parameters.
    pub params: &'a HashMap<Spur, ParamVarInfo>,
    /// Return type of the current function.
    pub return_type: Type,
    /// How many loops we're nested inside (for break/continue validation).
    pub loop_depth: u32,
    /// One entry per enclosing loop (innermost last); set to `true` when a
    /// `break` targeting that loop is seen. An infinite loop containing a
    /// break has type `()`; without one it has type `!` (see spec 4.8).
    pub loop_break_stack: Vec<bool>,
    /// Scope stack for efficient scope management.
    scope_stack: Vec<Vec<(Spur, Option<LocalVarInfo>)>>,
}

impl<'a> ConstraintContext<'a> {
    /// Create a new context for a function.
    pub fn new(params: &'a HashMap<Spur, ParamVarInfo>, return_type: Type) -> Self {
        Self {
            locals: HashMap::new(),
            params,
            return_type,
            loop_depth: 0,
            loop_break_stack: Vec::new(),
            scope_stack: Vec::new(),
        }
    }
}

impl ScopedContext for ConstraintContext<'_> {
    type VarInfo = LocalVarInfo;

    fn locals_mut(&mut self) -> &mut HashMap<Spur, Self::VarInfo> {
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
    expr_types: HashMap<InstRef, InferType>,
    /// Function signatures (for call type checking).
    functions: &'a HashMap<Spur, FunctionSig>,
    /// Struct types (name -> Type::new_struct(id)).
    structs: &'a HashMap<Spur, Type>,
    /// Module-local struct types ((defining file, source name) -> Type::new_struct(id)).
    structs_by_file_name: Option<&'a HashMap<(FileId, Spur), Type>>,
    /// Enum types (name -> Type::new_enum(id)).
    enums: &'a HashMap<Spur, Type>,
    /// Module-local enum types ((defining file, source name) -> Type::new_enum(id)).
    enums_by_file_name: Option<&'a HashMap<(FileId, Spur), Type>>,
    /// Method signatures: (struct_id, method_name) -> MethodSig
    methods: &'a HashMap<(StructId, Spur), MethodSig>,
    /// Type variables allocated for integer literals.
    /// These start as unbound and need to be defaulted to i32 if unconstrained.
    int_literal_vars: Vec<TypeVarId>,
    /// Type substitutions for Self and type parameters (used in method bodies).
    /// Maps type names (like "Self") to their concrete types.
    type_subst: Option<&'a HashMap<Spur, Type>>,
    /// File-level constant types (name -> declared type), resolved during
    /// declaration gathering. Consulted by `VarRef` after locals and params so
    /// a const reference infers to its declared type instead of `<error>`
    /// (RUE-142). `None` only in unit tests; production passes the map via
    /// [`Self::with_const_types`].
    const_types: Option<&'a HashMap<Spur, Type>>,
    /// File-level type aliases (`const T = SomeType(...)`) resolved during
    /// declaration gathering. Consulted in type positions.
    const_type_aliases: Option<&'a HashMap<Spur, Type>>,
    /// Module-binding types (`const utils = @import(...)`): (declaring file,
    /// name) -> module type. Per-file scoped (RUE-113), so `VarRef` consults
    /// this with the reference's own `span.file_id` before `const_types`.
    /// `None` only in unit tests; production passes the map via
    /// [`Self::with_module_binding_types`].
    module_binding_types: Option<&'a HashMap<(FileId, Spur), Type>>,
    /// Module registry file identity for inference-time `module.Type` and
    /// `module.Enum` lookup.
    /// `None` only in unit tests; production passes the map via
    /// [`Self::with_module_file_ids`].
    module_file_ids: Option<&'a HashMap<crate::types::ModuleId, FileId>>,
    /// Compile-time type aliases bound by `let` in the current function body
    /// (`let P = F();` where `F` returns `type`), pre-resolved by sema before
    /// constraint generation. Consulted after `type_subst` when resolving
    /// struct-literal type names and `let` annotations, so anonymous-struct
    /// aliases route through the same concrete paths as named structs
    /// (RUE-170). Like sema's `comptime_type_vars`, the map is flat (not
    /// scope-aware). `None` only in unit tests; production passes the map via
    /// [`Self::with_comptime_local_types`].
    comptime_local_types: Option<&'a HashMap<Spur, Type>>,
    /// Method signatures registered after the shared `InferenceContext` was
    /// built: anonymous-struct methods are registered lazily during comptime
    /// evaluation, so they're absent from `methods`. Consulted when a method
    /// key misses `methods`, so a call on an anonymous-struct receiver yields
    /// its declared return type instead of `<error>` (RUE-164). `None` only
    /// in unit tests; production passes the map via
    /// [`Self::with_extra_method_sigs`].
    extra_method_sigs: Option<&'a HashMap<(StructId, Spur), MethodSig>>,
    /// File-level integer constant values (name -> value), so an array length
    /// naming a `const` (`[i32; K]`) resolves to a concrete length during
    /// constraint generation (RUE-16). `None` only in unit tests; production
    /// passes the map via [`Self::with_const_values`].
    const_values: Option<&'a HashMap<Spur, i128>>,
    /// Function-valued constants: alias name -> callee function name. These
    /// let constraint generation type `alias(...)` as a direct call.
    const_function_aliases: Option<&'a HashMap<Spur, Spur>>,
    /// Comptime *value* parameters known for the specialization currently being
    /// analyzed (`comptime n: i32` → `n = 0` for the call `f(0)`). Lets a
    /// `match` on a comptime-known scrutinee prune to its selected arm during
    /// constraint generation, so only that arm participates in inference —
    /// mirroring sema's arm selection (spec 4.14:19). Without this, inference
    /// cross-constrains every arm and rejects a valid program whose statically
    /// unselected arm has a different type (RUE-268). `None` for ordinary
    /// (non-specialized) functions, where every match is treated as runtime.
    comptime_values: Option<&'a HashMap<Spur, ConstValue>>,
    /// Type intern pool for creating pointer and array types during constraint generation.
    type_pool: &'a TypeInternPool,
}

impl<'a> ConstraintGenerator<'a> {
    /// Create a new constraint generator.
    pub fn new(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        functions: &'a HashMap<Spur, FunctionSig>,
        structs: &'a HashMap<Spur, Type>,
        enums: &'a HashMap<Spur, Type>,
        methods: &'a HashMap<(StructId, Spur), MethodSig>,
        type_pool: &'a TypeInternPool,
    ) -> Self {
        Self::with_type_subst(
            rir, interner, functions, structs, enums, methods, type_pool, None,
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
        functions: &'a HashMap<Spur, FunctionSig>,
        structs: &'a HashMap<Spur, Type>,
        enums: &'a HashMap<Spur, Type>,
        methods: &'a HashMap<(StructId, Spur), MethodSig>,
        type_pool: &'a TypeInternPool,
        type_subst: Option<&'a HashMap<Spur, Type>>,
    ) -> Self {
        Self {
            rir,
            interner,
            type_vars: TypeVarAllocator::new(),
            constraints: Vec::new(),
            expr_types: HashMap::new(),
            functions,
            structs,
            structs_by_file_name: None,
            enums,
            enums_by_file_name: None,
            methods,
            int_literal_vars: Vec::new(),
            type_subst,
            const_types: None,
            const_type_aliases: None,
            module_binding_types: None,
            module_file_ids: None,
            comptime_local_types: None,
            extra_method_sigs: None,
            const_values: None,
            const_function_aliases: None,
            comptime_values: None,
            type_pool,
        }
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
            let name = &self.type_pool.struct_def(id).name;
            return name == "str"
                || (name.starts_with("Str(") && name.ends_with(')'))
                || (name.starts_with('[')
                    && name.ends_with(']')
                    && crate::types::parse_array_type_syntax(name).is_none());
        }
        false
    }

    /// Provide file-level constant types (name -> declared type) for `VarRef`
    /// resolution. See the `const_types` field for details (RUE-142).
    pub fn with_const_types(mut self, const_types: &'a HashMap<Spur, Type>) -> Self {
        self.const_types = Some(const_types);
        self
    }

    /// Provide file-level type aliases for type-position resolution.
    pub fn with_const_type_aliases(mut self, const_type_aliases: &'a HashMap<Spur, Type>) -> Self {
        self.const_type_aliases = Some(const_type_aliases);
        self
    }

    /// Provide per-file module-binding types ((file, name) -> module type)
    /// for `VarRef` resolution. See the `module_binding_types` field (RUE-113).
    pub fn with_module_binding_types(
        mut self,
        module_binding_types: &'a HashMap<(FileId, Spur), Type>,
    ) -> Self {
        self.module_binding_types = Some(module_binding_types);
        self
    }

    /// Provide module-local struct types for file-scoped and module-qualified
    /// struct literal resolution.
    pub fn with_structs_by_file_name(
        mut self,
        structs_by_file_name: &'a HashMap<(FileId, Spur), Type>,
    ) -> Self {
        self.structs_by_file_name = Some(structs_by_file_name);
        self
    }

    /// Provide module registry file identities for `module.Type` lookup.
    pub fn with_module_file_ids(
        mut self,
        module_file_ids: &'a HashMap<crate::types::ModuleId, FileId>,
    ) -> Self {
        self.module_file_ids = Some(module_file_ids);
        self
    }

    /// Provide module-local enum types for file-scoped and module-qualified
    /// enum variant/pattern resolution.
    pub fn with_enums_by_file_name(
        mut self,
        enums_by_file_name: &'a HashMap<(FileId, Spur), Type>,
    ) -> Self {
        self.enums_by_file_name = Some(enums_by_file_name);
        self
    }

    /// Provide pre-resolved comptime type aliases (local name -> concrete
    /// type) for struct-literal and `let`-annotation resolution. See the
    /// `comptime_local_types` field (RUE-170).
    pub fn with_comptime_local_types(
        mut self,
        comptime_local_types: &'a HashMap<Spur, Type>,
    ) -> Self {
        self.comptime_local_types = Some(comptime_local_types);
        self
    }

    /// Provide late-registered method signatures (anonymous-struct methods)
    /// for method-call resolution. See the `extra_method_sigs` field
    /// (RUE-164).
    pub fn with_extra_method_sigs(
        mut self,
        extra_method_sigs: &'a HashMap<(StructId, Spur), MethodSig>,
    ) -> Self {
        self.extra_method_sigs = Some(extra_method_sigs);
        self
    }

    /// Provide file-level integer constant values (name -> value) so an array
    /// length naming a `const` resolves during constraint generation. See the
    /// `const_values` field (RUE-16).
    pub fn with_const_values(mut self, const_values: &'a HashMap<Spur, i128>) -> Self {
        self.const_values = Some(const_values);
        self
    }

    /// Provide function-valued constants so `alias(...)` gets the callee's
    /// signature during constraint generation.
    pub fn with_const_function_aliases(
        mut self,
        const_function_aliases: &'a HashMap<Spur, Spur>,
    ) -> Self {
        self.const_function_aliases = Some(const_function_aliases);
        self
    }

    /// Provide the comptime value parameters of the specialization being
    /// analyzed so a `match` on a comptime-known scrutinee prunes to its
    /// selected arm during constraint generation. See the `comptime_values`
    /// field (RUE-268). `None` (ordinary functions) leaves every match
    /// runtime.
    pub fn with_comptime_values(
        mut self,
        comptime_values: Option<&'a HashMap<Spur, ConstValue>>,
    ) -> Self {
        self.comptime_values = comptime_values;
        self
    }

    /// Resolve an array-length component to a concrete length during constraint
    /// generation. Literal lengths are used directly; a named length resolves
    /// against file-level integer constants (`[i32; K]`). Names that don't
    /// resolve here (e.g. a `comptime` value parameter, only known at
    /// specialization) yield `None`; sema resolves and diagnoses them (RUE-16).
    fn resolve_infer_array_length(&self, len: &ArrayLen) -> Option<u64> {
        match len {
            ArrayLen::Literal(n) => Some(*n),
            ArrayLen::Named(name) => {
                let sym = self.interner.get(name)?;
                let value = *self.const_values?.get(&sym)?;
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
    fn resolve_infer_array_length_with_values(
        &self,
        len: &ArrayLen,
        values: &HashMap<Spur, i128>,
    ) -> Option<u64> {
        match len {
            ArrayLen::Literal(n) => Some(*n),
            ArrayLen::Named(name) => {
                let sym = self.interner.get(name)?;
                let value = values
                    .get(&sym)
                    .copied()
                    .or_else(|| self.const_values.and_then(|m| m.get(&sym).copied()))?;
                u64::try_from(value).ok()
            }
        }
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
    /// `Sema::type_to_infer_type`.
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

    /// Look up a method signature, falling back to the late-registered
    /// (anonymous-struct) signatures when the shared map misses (RUE-164).
    fn method_sig(&self, key: &(StructId, Spur)) -> Option<&'a MethodSig> {
        self.methods
            .get(key)
            .or_else(|| self.extra_method_sigs.and_then(|sigs| sigs.get(key)))
    }

    /// Resolve a field's declared type on a concrete struct type.
    fn field_type_of(&self, struct_ty: Type, field: Spur) -> Option<Type> {
        let TypeKind::Struct(struct_id) = struct_ty.kind() else {
            return None;
        };
        let field_name = self.interner.resolve(&field);
        self.type_pool
            .struct_def(struct_id)
            .fields
            .iter()
            .find(|f| f.name == field_name)
            .map(|f| f.ty)
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

    /// Check whether an instruction is a comparison operator.
    ///
    /// Mirrors `Sema::is_comparison`; used to recognize chained comparisons
    /// (`a < b < c`) during constraint generation.
    fn is_comparison(&self, inst_ref: InstRef) -> bool {
        matches!(
            self.rir.get(inst_ref).data,
            InstData::Lt { .. }
                | InstData::Gt { .. }
                | InstData::Le { .. }
                | InstData::Ge { .. }
                | InstData::Eq { .. }
                | InstData::Ne { .. }
        )
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

    /// Take ownership of the collected constraints.
    pub fn take_constraints(self) -> Vec<Constraint> {
        self.constraints
    }

    /// Get the expression type mapping.
    pub fn expr_types(&self) -> &HashMap<InstRef, InferType> {
        &self.expr_types
    }

    /// Consume the constraint generator and return (constraints, int_literal_vars, expr_types, type_var_count).
    ///
    /// This is useful when you need ownership of the expression types map.
    /// The `type_var_count` can be used to pre-size the unifier's substitution for better performance.
    pub fn into_parts(
        self,
    ) -> (
        Vec<Constraint>,
        Vec<TypeVarId>,
        HashMap<InstRef, InferType>,
        u32,
    ) {
        (
            self.constraints,
            self.int_literal_vars,
            self.expr_types,
            self.type_vars.count(),
        )
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

            InstData::BoolConst(_) => InferType::Concrete(Type::BOOL),

            // String literals have the builtin growable-string type `StrBuf`
            // (ADR-0043; formerly `String`).
            InstData::StringConst(_) => {
                // Look up the StrBuf type from the structs map
                if let Some(string_spur) = self.interner.get("StrBuf") {
                    if let Some(&string_ty) = self.structs.get(&string_spur) {
                        InferType::Concrete(string_ty)
                    } else {
                        // Fallback if StrBuf struct not found (shouldn't happen after builtin injection)
                        InferType::Concrete(Type::ERROR)
                    }
                } else {
                    InferType::Concrete(Type::ERROR)
                }
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
                // A chained comparison (`a < b < c`, left-associative, so the
                // LHS is itself a comparison) is always rejected by sema with a
                // dedicated error. Skip the operand-equality constraint in that
                // case so inference doesn't mask it with a generic type
                // mismatch (e.g. bool vs integer literal).
                if !self.is_comparison(*lhs) {
                    // Operands must have the same type
                    self.add_constraint(Constraint::equal(lhs_info.ty, rhs_info.ty, span));
                }
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
            InstData::VarRef { name } => {
                if let Some(local) = ctx.locals.get(name) {
                    local.ty.clone()
                } else if let Some(param) = ctx.params.get(name) {
                    param.ty.clone()
                } else if let Some(binding_ty) = self
                    .module_binding_types
                    .and_then(|bindings| bindings.get(&(span.file_id, *name)))
                {
                    // Module binding declared in this file (`const m =
                    // @import(...)`): per-file scoped, checked before the
                    // global value-const table (RUE-113).
                    self.type_to_infer(*binding_ty)
                } else if let Some(const_ty) = self.const_types.and_then(|consts| consts.get(name))
                {
                    // File-level constant: its type was resolved during
                    // declaration gathering (i32/bool/unit literals, module
                    // types for `const m = @import(...)`). Yielding it here
                    // anchors expressions like `N + 1` and `m.go() + 1` that
                    // previously decayed to `<error>` (RUE-142).
                    self.type_to_infer(*const_ty)
                } else {
                    // Unknown variable - will be caught during semantic analysis
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Local variable allocation
            InstData::Alloc {
                directives_start: _,
                directives_len: _,
                name,
                is_mut,
                ty: type_annotation,
                init,
                iter_elem: _,
            } => {
                let init_info = self.generate(*init, ctx);

                let var_ty = if let Some(ty_sym) = type_annotation {
                    // Explicit type annotation - use it and constrain init to
                    // match. Comptime type aliases (`let P = F(); let p: P =
                    // ...`) resolve first, mirroring sema's annotation
                    // validation order (`comptime_type_vars` before the type
                    // tables); without this the annotation was unenforced and
                    // any value typechecked against it (RUE-170).
                    let annotated = self
                        .comptime_local_types
                        .and_then(|aliases| aliases.get(ty_sym).copied())
                        .or_else(|| {
                            self.const_type_aliases
                                .and_then(|aliases| aliases.get(ty_sym).copied())
                        })
                        .map(|ty| self.type_to_infer(ty))
                        .or_else(|| self.resolve_type_name(self.interner.resolve(ty_sym)));
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
                } else {
                    // No annotation - use the init expression's type
                    init_info.ty
                };

                // Record the variable in scope (if it has a name)
                if let Some(var_name) = name {
                    ctx.insert_local(
                        *var_name,
                        LocalVarInfo {
                            ty: var_ty.clone(),
                            is_mut: *is_mut,
                            span,
                        },
                    );
                }

                // Alloc produces unit type
                InferType::Concrete(Type::UNIT)
            }

            // Assignment
            InstData::Assign { name, value } => {
                let value_info = self.generate(*value, ctx);
                if let Some(local) = ctx.locals.get(name) {
                    let local_ty = local.ty.clone();
                    // A `str` target (ADR-0043 Phase 3, RUE-324) accepts a string
                    // literal (HM type `String`) by coercion; skip strict
                    // equality and let sema materialize the `str` on the store.
                    if !self.is_slice_struct_type(local_ty.clone()) {
                        // Constrain value to match variable type
                        self.add_constraint(Constraint::equal(value_info.ty, local_ty, span));
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

            // Function call
            InstData::Call {
                name,
                args_start,
                args_len,
            } => {
                let name = self
                    .const_function_aliases
                    .and_then(|aliases| aliases.get(name))
                    .unwrap_or(name);
                let args = self.rir.get_call_args(*args_start, *args_len);
                // `print(s)` / `println(s)` builtin free functions (RUE-1):
                // constrain the single argument to String and yield unit, so a
                // literal argument resolves to String and the call type-checks.
                // Only when the program hasn't shadowed the name with its own
                // `fn print`/`fn println` (a user definition wins).
                let is_print_builtin = !self.functions.contains_key(name)
                    && matches!(self.interner.resolve(name), "print" | "println");
                if is_print_builtin {
                    for arg in args.iter() {
                        let arg_info = self.generate(arg.value, ctx);
                        self.add_constraint(Constraint::equal(
                            arg_info.ty,
                            self.string_infer_type(),
                            arg_info.span,
                        ));
                    }
                    InferType::Concrete(Type::UNIT)
                } else if let Some(func) = self.functions.get(name) {
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
                        let mut type_subst: std::collections::HashMap<lasso::Spur, Type> =
                            std::collections::HashMap::new();
                        // Comptime VALUE arguments (`comptime N: i32`) captured
                        // as their integer constant, so a return/param type
                        // sized by one — an array length `[i32; N]` — resolves
                        // at this call (RUE-252).
                        let mut value_subst: std::collections::HashMap<lasso::Spur, i128> =
                            std::collections::HashMap::new();
                        for (i, arg) in args.iter().enumerate() {
                            if i >= func.param_comptime.len()
                                || !func.param_comptime[i]
                                || i >= func.param_names.len()
                            {
                                continue;
                            }
                            if func.param_types.get(i)
                                == Some(&InferType::Concrete(Type::COMPTIME_TYPE))
                            {
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
                            if func.param_comptime[i]
                                && *declared == InferType::Concrete(Type::COMPTIME_TYPE)
                            {
                                // Comptime TYPE parameter - the argument is a type value
                                continue;
                            }
                            let expected = if *declared == InferType::Concrete(Type::COMPTIME_TYPE)
                            {
                                // Generic parameter like `x: T`, or a composite
                                // mentioning a type parameter like `a: [T; 3]`
                                // (RUE-172) - substitute T
                                match func.param_type_syms.get(i).and_then(|sym| {
                                    self.resolve_type_sym_with_subst(
                                        *sym,
                                        &type_subst,
                                        &value_subst,
                                    )
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
                        let return_type =
                            if func.return_type == InferType::Concrete(Type::COMPTIME_TYPE) {
                                match self.resolve_type_sym_with_subst(
                                    func.return_type_sym,
                                    &type_subst,
                                    &value_subst,
                                ) {
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
                                        let is_type_call = parse_type_call_syntax(
                                            self.interner.resolve(&func.return_type_sym),
                                        )
                                        .is_some();
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
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                let intrinsic_name = self.interner.resolve(name);
                let args = self.rir.get_inst_refs(*args_start, *args_len);

                if intrinsic_name == "intCast" || intrinsic_name == "cast" {
                    // @intCast: target type is inferred from context.
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
                        self.generate(*arg_ref, ctx);
                    }
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "random_u32" {
                    // @random_u32: no arguments, returns u32
                    InferType::Concrete(Type::U32)
                } else if intrinsic_name == "random_u64" {
                    // @random_u64: no arguments, returns u64
                    InferType::Concrete(Type::U64)
                } else if intrinsic_name == "syscall" {
                    // @syscall: syscall_num and up to 6 args (all u64), returns i64
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::I64)
                } else if intrinsic_name == "ptr_to_int" {
                    // @ptr_to_int: takes a pointer, returns u64
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::U64)
                } else if intrinsic_name == "ptr_write" {
                    // @ptr_write: takes a pointer and value, returns unit
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::UNIT)
                } else if intrinsic_name == "is_null" {
                    // @is_null: takes a pointer, returns bool
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::BOOL)
                } else if intrinsic_name == "ptr_read" {
                    // @ptr_read: takes ptr const T or ptr mut T, returns T
                    // The return type depends on the pointee type of the argument.
                    // We create a fresh type variable that will be resolved during
                    // semantic analysis when the actual pointer type is known.
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
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
                } else if intrinsic_name == "alloc" {
                    // @alloc(count: u64) -> ptr mut T. The element type T (and
                    // thus the result pointer type) is inferred from context,
                    // exactly like @int_to_ptr; sema validates it is `ptr mut T`.
                    // Constrain `count` to u64 so a bare integer literal
                    // (`@alloc(4)`) defaults to u64 rather than i64.
                    for arg_ref in args.iter() {
                        let info = self.generate(*arg_ref, ctx);
                        self.add_constraint(Constraint::equal(
                            info.ty,
                            InferType::Concrete(Type::U64),
                            info.span,
                        ));
                    }
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "realloc" {
                    // @realloc(ptr: ptr mut T, old_count: u64, new_count: u64)
                    // -> ptr mut T. The result type is the same pointer type as
                    // the first argument; unify a fresh var with arg0's type so
                    // it flows in both directions. Counts are constrained to u64.
                    let mut first_ty: Option<InferType> = None;
                    for (i, arg_ref) in args.iter().enumerate() {
                        let info = self.generate(*arg_ref, ctx);
                        if i == 0 {
                            first_ty = Some(info.ty);
                        } else {
                            self.add_constraint(Constraint::equal(
                                info.ty,
                                InferType::Concrete(Type::U64),
                                info.span,
                            ));
                        }
                    }
                    let result_var = self.fresh_var();
                    let result_ty = InferType::Var(result_var);
                    if let Some(arg0_ty) = first_ty {
                        self.add_constraint(Constraint::equal(result_ty.clone(), arg0_ty, span));
                    }
                    result_ty
                } else if intrinsic_name == "free" {
                    // @free(ptr: ptr mut T, count: u64) -> (). Constrain the
                    // `count` (second) argument to u64.
                    for (i, arg_ref) in args.iter().enumerate() {
                        let info = self.generate(*arg_ref, ctx);
                        if i == 1 {
                            self.add_constraint(Constraint::equal(
                                info.ty,
                                InferType::Concrete(Type::U64),
                                info.span,
                            ));
                        }
                    }
                    InferType::Concrete(Type::UNIT)
                } else if intrinsic_name == "int_to_ptr" || intrinsic_name == "null_ptr" {
                    // @int_to_ptr / @null_ptr: returns a pointer type inferred from context
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "ptr_copy" {
                    // @ptr_copy: (dst: ptr mut T, src: ptr const T, count: u64) -> ()
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::UNIT)
                } else if intrinsic_name == "target_arch" {
                    // @target_arch: returns Arch enum
                    if let Some(arch_spur) = self.interner.get("Arch") {
                        if let Some(&arch_ty) = self.enums.get(&arch_spur) {
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
                        if let Some(&os_ty) = self.enums.get(&os_spur) {
                            InferType::Concrete(os_ty)
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
                    // needs module-ness (member-call lookup is global by name,
                    // RUE-140) and sema assigns the real id during analysis.
                    // Returning Unit here (the old catch-all) made a member
                    // call on the binding unresolvable (RUE-142) and let a
                    // bare module expression coerce to `()` silently.
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::new_module(crate::types::ModuleId::UNRESOLVED))
                } else if intrinsic_name == "__rue_iter_len"
                    || intrinsic_name == "__rue_char_next"
                    || intrinsic_name == "__rue_char_next_lossy"
                {
                    // Compiler-internal `for`-loop leaves (RUE-220):
                    // @__rue_iter_len(coll) is the loop bound (byte length /
                    // array length) and @__rue_char_next{,_lossy}(s, p) is the
                    // next byte offset — both `usize` (u64).
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::U64)
                } else if intrinsic_name == "__rue_char_scalar"
                    || intrinsic_name == "__rue_char_scalar_lossy"
                {
                    // @__rue_char_scalar{,_lossy}(s, p): the Unicode scalar at
                    // byte offset `p`, a u32 (RUE-220; lossy variant RUE-17
                    // Phase 3).
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::U32)
                } else {
                    // Generate constraints for arguments (they need to be processed)
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    // @dbg and other intrinsics return Unit
                    InferType::Concrete(Type::UNIT)
                }
            }

            // Type intrinsic (@size_of, @align_of)
            InstData::TypeIntrinsic {
                name: _,
                type_arg: _,
            } => {
                // Type intrinsics return i32 (the size or alignment value)
                InferType::Concrete(Type::I32)
            }

            // Field-offset intrinsic (@offset_of) — the compile-time byte
            // offset of a field within a struct (RUE-301). Returns u64, like
            // Rust's `core::mem::offset_of!`.
            InstData::OffsetOf {
                type_arg: _,
                field: _,
            } => InferType::Concrete(Type::U64),

            // Block
            InstData::Block { extra_start, len } => {
                ctx.push_scope();
                let mut last_ty = InferType::Concrete(Type::UNIT);
                let block_insts = self.rir.get_extra(*extra_start, *len);
                for &inst_raw in block_insts {
                    let block_inst_ref = InstRef::from_raw(inst_raw);
                    let info = self.generate(block_inst_ref, ctx);
                    last_ty = info.ty;
                }
                ctx.pop_scope();
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
            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => {
                let scrutinee_info = self.generate(*scrutinee, ctx);
                let arms = self.rir.get_match_arms(*arms_start, *arms_len);

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
                if let Some(selected) = self.comptime_selected_arm(*scrutinee, &arms) {
                    ctx.push_scope();
                    let body_info = self.generate(selected, ctx);
                    ctx.pop_scope();
                    self.record_type(inst_ref, body_info.ty.clone());
                    return ExprInfo::new(body_info.ty, span);
                }

                // Collect arm types, handling Never coercion
                let mut arm_types: Vec<ExprInfo> = Vec::new();
                for (pattern, body) in arms.iter() {
                    // Patterns constrain the scrutinee type
                    let pattern_ty = self.pattern_type(pattern);
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
                    use crate::scope::ScopedContext;
                    ctx.push_scope();
                    if let rue_rir::RirPattern::Path {
                        module,
                        type_name,
                        variant,
                        bindings,
                        span: pat_span,
                    } = pattern
                    {
                        if !bindings.is_empty() {
                            let enum_ty = module
                                .and_then(|module_ref| {
                                    self.enum_type_for_module(module_ref, type_name)
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
                    let body_info = self.generate(*body, ctx);
                    ctx.pop_scope();
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
                type_name,
                fields_start,
                fields_len,
                ..
            } => {
                // Module-qualified literals (`m.Point { ... }`) resolve in
                // the module's defining file, matching sema. Unqualified
                // literals check type_subst first (for Self/type parameters),
                // then comptime type aliases (`let P = F(); P { ... }`,
                // RUE-170), then the current file's module-local type table,
                // then the unique-name compatibility map.
                let struct_ty = if let Some(module_ref) = module {
                    let module_info = self.generate(*module_ref, ctx);
                    let module_id = match module_info.ty {
                        InferType::Concrete(ty) => ty.as_module(),
                        _ => None,
                    };
                    module_id
                        .and_then(|module_id| {
                            self.module_file_ids
                                .and_then(|module_file_ids| module_file_ids.get(&module_id))
                                .copied()
                        })
                        .and_then(|file_id| {
                            self.structs_by_file_name
                                .and_then(|structs| structs.get(&(file_id, *type_name)))
                                .copied()
                        })
                } else {
                    self.type_subst
                        .and_then(|subst| subst.get(type_name).copied())
                        .or_else(|| {
                            self.comptime_local_types
                                .and_then(|aliases| aliases.get(type_name).copied())
                        })
                        .or_else(|| {
                            self.structs_by_file_name
                                .and_then(|structs| structs.get(&(span.file_id, *type_name)))
                                .copied()
                                .or_else(|| self.structs.get(type_name).copied())
                        })
                };

                let fields = self.rir.get_field_inits(*fields_start, *fields_len);
                if let Some(struct_ty) = struct_ty {
                    // Constrain each initializer against its field's declared
                    // type, so literal initializers are range-checked at the
                    // field's width instead of silently wrapping
                    // (`S { a: 300 }` with a: u8 used to truncate to 44).
                    // (RUE-72)
                    for (field_name, value_ref) in fields.iter() {
                        let value_info = self.generate(*value_ref, ctx);
                        if let Some(field_ty) = self.field_type_of(struct_ty, *field_name) {
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
                    for (_, value_ref) in fields.iter() {
                        self.generate(*value_ref, ctx);
                    }
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Field access
            InstData::FieldGet { base, field } => {
                let base_info = self.generate(*base, ctx);
                // When the base's struct type is already concrete, the field's
                // declared type is known RIGHT NOW — yield it so downstream
                // constraints see the real type instead of a free variable.
                // Leaving it free mis-defaulted literals compared against i64
                // fields to i32 (spurious E0800) and made method calls on a
                // field receiver unresolvable (the MethodCall arm requires a
                // Concrete receiver). When the base is still a variable, fall
                // back to a fresh var; sema resolves and diagnoses later.
                // (RUE-89, RUE-126)
                match self.known_field_type(&base_info.ty, *field) {
                    Some(field_ty) => self.type_to_infer(field_ty),
                    None => {
                        // Module receiver: `m.CONST` resolves to a module
                        // member value-constant; yield its declared type so
                        // uses like `m.CONST + 1` are anchored (RUE-160).
                        // The lookup is global by name, never verifying the
                        // constant belongs to the receiver module's file —
                        // the same known looseness as member calls (RUE-140).
                        let member_const_ty = match &base_info.ty {
                            InferType::Concrete(ty) if ty.is_module() => self
                                .const_types
                                .and_then(|consts| consts.get(field))
                                .copied(),
                            _ => None,
                        };
                        match member_const_ty {
                            Some(const_ty) => self.type_to_infer(const_ty),
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
                // instead of silently wrapping (`s.a = 300` with a: u8 used to
                // truncate to 44). (RUE-104)
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
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => {
                let elements = self.rir.get_inst_refs(*elems_start, *elems_len);
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
                    let first_info = self.generate(elements[0], ctx);
                    for elem_ref in elements.iter().skip(1) {
                        let elem_info = self.generate(*elem_ref, ctx);
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
                        .const_values
                        .and_then(|cv| cv.get(sym))
                        .and_then(|v| u64::try_from(*v).ok()),
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
                args_start,
                args_len,
            } => {
                // Generate type for receiver
                let receiver_info = self.generate(*receiver, ctx);
                let args = self.rir.get_call_args(*args_start, *args_len);

                // Resolve the call's result type from the receiver's type.
                // When the receiver can't be resolved RIGHT NOW but sema will
                // resolve it later, yield a fresh variable rather than ERROR:
                // a Concrete(ERROR) here flowed into sibling constraints and
                // produced bogus "literal out of range for '<error>'" failures
                // that masked (or preempted) sema's real analysis (RUE-126
                // treats FieldGet the same way).
                let result_type = match &receiver_info.ty {
                    // Module receiver: `m.go(...)` is a module member call.
                    // Mirror sema's `check_module_member_call`: the lookup is
                    // global by name, never verifying the function belongs to
                    // the receiver module's file (known looseness, RUE-140 —
                    // when that is ratified, narrow this lookup too). Yielding
                    // the callee's return type anchors uses like `m.go() + 1`
                    // that previously decayed to `<error>` (RUE-142).
                    InferType::Concrete(ty) if ty.is_module() => {
                        if let Some(func) = self.functions.get(method) {
                            if !func.is_generic && args.len() == func.param_types.len() {
                                // Constrain each argument against its declared
                                // parameter type (same as a direct Call).
                                for (arg, param_ty) in args.iter().zip(func.param_types.iter()) {
                                    let arg_info = self.generate(arg.value, ctx);
                                    self.add_constraint(Constraint::equal(
                                        arg_info.ty,
                                        param_ty.clone(),
                                        arg_info.span,
                                    ));
                                }
                            } else {
                                // Generic callee or arity mismatch: just
                                // process the arguments; sema checks the rest.
                                for arg in args.iter() {
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
                            for arg in args.iter() {
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
                    InferType::Concrete(ty) if *ty == Type::COMPTIME_TYPE => {
                        for arg in args.iter() {
                            self.generate(arg.value, ctx);
                        }
                        InferType::Var(self.fresh_var())
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
                                    args.iter().zip(method_sig.param_types.iter())
                                {
                                    let arg_info = self.generate(arg.value, ctx);
                                    self.add_constraint(Constraint::equal(
                                        arg_info.ty,
                                        param_type.clone(),
                                        arg_info.span,
                                    ));
                                }
                                method_sig.return_type.clone()
                            } else {
                                // Method not found - sema will report the error
                                // Still generate arg types to catch errors in arguments
                                for arg in args.iter() {
                                    self.generate(arg.value, ctx);
                                }
                                InferType::Concrete(Type::ERROR)
                            }
                        } else {
                            // Non-struct receiver - sema will report the error
                            for arg in args.iter() {
                                self.generate(arg.value, ctx);
                            }
                            InferType::Concrete(Type::ERROR)
                        }
                    }
                    // Receiver still a type variable: sema resolves the
                    // receiver and diagnoses later (mirrors FieldGet's
                    // fallback, RUE-126/RUE-119).
                    _ => {
                        for arg in args.iter() {
                            self.generate(arg.value, ctx);
                        }
                        InferType::Var(self.fresh_var())
                    }
                };

                result_type
            }

            // Associated function call: Type::function(args)
            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => {
                let args = self.rir.get_call_args(*args_start, *args_len);

                // Enum tuple-variant construction: `Shape::Circle(5)` parses as
                // an associated-function call. If `type_name` is an enum and
                // `function` names one of its variants, constrain each payload
                // argument to the declared payload type and yield the enum type
                // (RUE-221). Checked here first so it takes precedence over the
                // struct-method path below.
                let enum_variant = self
                    .enum_type_for(type_name, span.file_id)
                    .and_then(|ty| ty.as_enum().map(|id| (ty, id)))
                    .and_then(|(ty, id)| {
                        let def = self.type_pool.enum_def(id);
                        def.find_variant(self.interner.resolve(function))
                            .map(|vidx| (ty, def.variant_payload(vidx).to_vec()))
                    });
                if let Some((enum_ty, payload)) = enum_variant {
                    for (i, arg) in args.iter().enumerate() {
                        let arg_info = self.generate(arg.value, ctx);
                        if let Some(&pty) = payload.get(i) {
                            // Convert the declared payload type structurally so
                            // an array payload (`[i32; 2]`) unifies with an
                            // array-literal argument (`[1, 2]`) and propagates
                            // the expected element type into its literal
                            // elements — exactly as struct-field init does.
                            // Using `InferType::Concrete` here left the literal
                            // element type unconstrained, so `[{integer}; 2]`
                            // failed to unify with `[i32; 2]` (RUE-260).
                            let expected = self.type_to_infer(pty);
                            self.add_constraint(Constraint::equal(
                                arg_info.ty,
                                expected,
                                arg_info.span,
                            ));
                        }
                    }
                    let ty = InferType::Concrete(enum_ty);
                    self.record_type(inst_ref, ty.clone());
                    return ExprInfo::new(ty, span);
                }

                // Get struct ID from type name for method lookup
                let struct_id = self.structs.get(type_name).and_then(|ty| ty.as_struct());
                let result_type = if let Some(struct_id) = struct_id {
                    let method_key = (struct_id, *function);
                    if let Some(method_sig) = self.methods.get(&method_key) {
                        // Generate constraints for arguments
                        for (arg, param_type) in args.iter().zip(method_sig.param_types.iter()) {
                            let arg_info = self.generate(arg.value, ctx);
                            self.add_constraint(Constraint::equal(
                                arg_info.ty,
                                param_type.clone(),
                                arg_info.span,
                            ));
                        }
                        method_sig.return_type.clone()
                    } else {
                        // Method not found - sema will report the error
                        // Still generate arg types to catch errors in arguments
                        for arg in args.iter() {
                            self.generate(arg.value, ctx);
                        }
                        InferType::Concrete(Type::ERROR)
                    }
                } else {
                    // Type not found - sema will report the error
                    for arg in args.iter() {
                        self.generate(arg.value, ctx);
                    }
                    InferType::Concrete(Type::ERROR)
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
                let inner_info = self.generate(*expr, ctx);
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

        if self.is_string_concrete(&lhs_info.ty) || self.is_string_concrete(&rhs_info.ty) {
            let string_ty = self.string_infer_type();
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

    /// The builtin growable-string type `StrBuf` (ADR-0043; formerly `String`)
    /// as an [`InferType::Concrete`], or `Concrete(ERROR)` if it hasn't been
    /// injected (should not happen once builtin types are registered).
    fn string_infer_type(&self) -> InferType {
        if let Some(string_spur) = self.interner.get("StrBuf") {
            if let Some(&string_ty) = self.structs.get(&string_spur) {
                return InferType::Concrete(string_ty);
            }
        }
        InferType::Concrete(Type::ERROR)
    }

    /// Whether `ty` is *concretely* the builtin `StrBuf` struct (not a type
    /// variable that might later resolve to it).
    fn is_string_concrete(&self, ty: &InferType) -> bool {
        if let InferType::Concrete(t) = ty {
            if let Some(string_spur) = self.interner.get("StrBuf") {
                if let Some(&string_ty) = self.structs.get(&string_spur) {
                    return *t == string_ty;
                }
            }
        }
        false
    }

    /// Resolve an enum type name that may be a comptime type-variable binding
    /// (`let O = Option(i32); O::Some(..)`), falling back to the named-enum
    /// table. Mirrors sema's `resolve_enum_type_name`; without the
    /// comptime-alias lookup, generic-enum construction/matching inferred
    /// `<error>` and poisoned the surrounding constraints (RUE-6 phase 2).
    fn enum_type_for(&self, type_name: &Spur, file_id: FileId) -> Option<Type> {
        self.comptime_local_types
            .and_then(|aliases| aliases.get(type_name).copied())
            .filter(|ty| ty.is_enum())
            .or_else(|| {
                self.enums_by_file_name
                    .and_then(|enums| enums.get(&(file_id, *type_name)))
                    .copied()
            })
            .or_else(|| self.enums.get(type_name).copied())
    }

    fn enum_type_for_module(&self, module: InstRef, type_name: &Spur) -> Option<Type> {
        let inst = self.rir.get(module);
        let InstData::VarRef { name } = &inst.data else {
            return None;
        };
        let module_ty = self
            .module_binding_types
            .and_then(|bindings| bindings.get(&(inst.span.file_id, *name)))
            .copied()?;
        let module_id = module_ty.as_module()?;
        let file_id = self
            .module_file_ids
            .and_then(|module_file_ids| module_file_ids.get(&module_id))
            .copied()?;
        self.enums_by_file_name
            .and_then(|enums| enums.get(&(file_id, *type_name)))
            .copied()
    }

    /// Get the inferred type for a pattern.
    fn pattern_type(&mut self, pattern: &rue_rir::RirPattern) -> InferType {
        match pattern {
            rue_rir::RirPattern::Wildcard(_) => {
                // Wildcard matches anything - use a fresh type variable
                let var = self.fresh_var();
                InferType::Var(var)
            }
            rue_rir::RirPattern::Int { .. } => InferType::IntLiteral,
            rue_rir::RirPattern::Bool(_, _) => InferType::Concrete(Type::BOOL),
            rue_rir::RirPattern::Path {
                module, type_name, ..
            } => {
                let enum_ty = module
                    .and_then(|module_ref| self.enum_type_for_module(module_ref, type_name))
                    .or_else(|| self.enum_type_for(type_name, pattern.span().file_id));
                if let Some(enum_ty) = enum_ty {
                    InferType::Concrete(enum_ty)
                } else {
                    InferType::Concrete(Type::ERROR)
                }
            }
        }
    }

    /// Resolve a type name to an InferType.
    ///
    /// Handles primitive types, array syntax `[T; N]`, pointer syntax `ptr mut T` / `ptr const T`,
    /// and struct/enum types.
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
            if let Some(&ty) = self.structs.get(sym) {
                return Some(ty);
            }
            if let Some(&ty) = self.enums.get(sym) {
                return Some(ty);
            }
            None
        };

        match &self.rir.get(arg).data {
            InstData::TypeConst { type_name } => {
                if let Some(ty) = resolve_sym(type_name) {
                    return Some(ty);
                }
                match self.resolve_type_name(self.interner.resolve(type_name)) {
                    Some(InferType::Concrete(ty)) => Some(ty),
                    _ => None,
                }
            }
            // A struct/enum name or forwarded type parameter used as a value
            // parses as a variable reference, not a type literal.
            InstData::VarRef { name } => {
                // A local bound to a type value (`let X = i32; identity(X, 42)`
                // or `let P = Pair(i32); f(P, ..)`) resolves to that bound type
                // via the precomputed comptime-local-types map — the same map
                // generic-enum construction/matching consults (`enum_type_for`).
                // Without this the type argument was left unresolved, so the
                // call's return type defaulted to the literal `type` and
                // mismatched the substituted element type (spurious E0206,
                // RUE-281). The literal form (`identity(i32, 42)`) already
                // worked via the `TypeConst` arm; this makes an aliased type
                // behave identically.
                if let Some(ty) = self
                    .comptime_local_types
                    .and_then(|aliases| aliases.get(name).copied())
                {
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
    /// (`fn f(comptime N: i32) -> [i32; N]`, RUE-252). Handles an integer
    /// literal, its negation, and a reference to a file-level `const`; other
    /// forms (checked fully in sema) yield `None`.
    fn extract_int_argument(&self, arg: InstRef) -> Option<i128> {
        match &self.rir.get(arg).data {
            InstData::IntConst(v) => Some(*v as i128),
            InstData::Neg { operand } => {
                if let InstData::IntConst(v) = &self.rir.get(*operand).data {
                    Some(-(*v as i128))
                } else {
                    None
                }
            }
            InstData::VarRef { name } => self.const_values.and_then(|m| m.get(name).copied()),
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
    /// any shape not understood (a non-comptime or compound scrutinee, an enum
    /// pattern, a non-exhaustive set). The comptime cases handled here are a
    /// strict subset of those sema prunes with the same value and patterns, so
    /// whenever this prunes, sema also prunes to the *same* arm — the only arm
    /// whose body inference generated a type for.
    fn comptime_selected_arm(
        &self,
        scrutinee: InstRef,
        arms: &[(rue_rir::RirPattern, InstRef)],
    ) -> Option<InstRef> {
        use rue_rir::RirPattern;

        // Only a bare reference to a comptime value parameter is evaluated
        // here; richer scrutinees are left to sema's own selection.
        // `ConstValue` is `Copy`, so take an owned copy for the match below.
        let value: ConstValue = match &self.rir.get(scrutinee).data {
            InstData::VarRef { name } => *self.comptime_values?.get(name)?,
            _ => return None,
        };

        let mut selected: Option<InstRef> = None;
        let mut has_wildcard = false;
        let mut bool_true_covered = false;
        let mut bool_false_covered = false;
        for (pattern, body) in arms {
            let matched = match pattern {
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
                selected = Some(*body);
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

    /// Resolve a signature type symbol with type-parameter substitution,
    /// looking through composite syntax (RUE-172).
    ///
    /// Like [`Self::resolve_type_name`], but substitutes comptime type
    /// parameters (e.g. `T` with `T=i32` resolves `[T; 3]` to `[i32; 3]`).
    /// Returns `None` when a mentioned type parameter isn't in `subst` (the
    /// check then happens in sema / after specialization).
    fn resolve_type_sym_with_subst(
        &self,
        sym: Spur,
        subst: &HashMap<Spur, Type>,
        values: &HashMap<Spur, i128>,
    ) -> Option<InferType> {
        if let Some(&ty) = subst.get(&sym) {
            return Some(InferType::Concrete(ty));
        }
        self.resolve_type_name_with_subst(self.interner.resolve(&sym), subst, values)
    }

    fn resolve_type_name_with_subst(
        &self,
        name: &str,
        subst: &HashMap<Spur, Type>,
        values: &HashMap<Spur, i128>,
    ) -> Option<InferType> {
        if let Some(name_spur) = self.interner.get(name) {
            if let Some(&ty) = subst.get(&name_spur) {
                return Some(InferType::Concrete(ty));
            }
        }

        // Array syntax: [T; N] - recurse so substituted element types work.
        if let Some((element_type_str, len)) = parse_array_type_syntax(name) {
            let element_ty = self.resolve_type_name_with_subst(&element_type_str, subst, values)?;
            let length = self.resolve_infer_array_length_with_values(&len, values)?;
            return Some(InferType::Array {
                element: Box::new(element_ty),
                length,
            });
        }

        // Pointer syntax: ptr mut T / ptr const T - recurse on the pointee.
        if let Some((pointee_type_str, mutability)) = parse_pointer_type_syntax(name) {
            let pointee_infer_ty =
                self.resolve_type_name_with_subst(&pointee_type_str, subst, values)?;
            // Only concrete pointees can be interned during constraint generation
            // (same limitation as resolve_type_name).
            let pointee_ty = match pointee_infer_ty {
                InferType::Concrete(ty) => ty,
                _ => return None,
            };
            let ptr_ty = match mutability {
                PtrMutability::Mut => {
                    Type::new_ptr_mut(self.type_pool.intern_ptr_mut_from_type(pointee_ty))
                }
                PtrMutability::Const => {
                    Type::new_ptr_const(self.type_pool.intern_ptr_const_from_type(pointee_ty))
                }
            };
            return Some(InferType::Concrete(ptr_ty));
        }

        self.resolve_type_name(name)
    }

    fn resolve_type_name(&self, name: &str) -> Option<InferType> {
        // Check for array syntax first: [T; N]
        if let Some((element_type_str, len)) = parse_array_type_syntax(name) {
            // Recursively resolve the element type
            let element_ty = self.resolve_type_name(&element_type_str)?;
            let length = self.resolve_infer_array_length(&len)?;
            return Some(InferType::Array {
                element: Box::new(element_ty),
                length,
            });
        }

        // Check for pointer syntax: ptr mut T / ptr const T
        if let Some((pointee_type_str, mutability)) = parse_pointer_type_syntax(name) {
            // Recursively resolve the pointee type
            let pointee_infer_ty = self.resolve_type_name(&pointee_type_str)?;

            // Convert InferType to Type so we can intern the pointer
            let pointee_ty = match pointee_infer_ty {
                InferType::Concrete(ty) => ty,
                // Can't handle non-concrete types in pointer positions during constraint generation
                _ => return None,
            };

            // Intern the pointer type
            let ptr_ty = match mutability {
                PtrMutability::Mut => {
                    let ptr_id = self.type_pool.intern_ptr_mut_from_type(pointee_ty);
                    Type::new_ptr_mut(ptr_id)
                }
                PtrMutability::Const => {
                    let ptr_id = self.type_pool.intern_ptr_const_from_type(pointee_ty);
                    Type::new_ptr_const(ptr_id)
                }
            };
            return Some(InferType::Concrete(ptr_ty));
        }

        // Check primitives (single shared table, RUE-155)
        if let Some(ty) = Type::from_primitive_name(name) {
            return Some(InferType::Concrete(ty));
        }

        // Check for struct types (including builtin String)
        if let Some(name_spur) = self.interner.get(name) {
            if let Some(&struct_ty) = self.structs.get(&name_spur) {
                return Some(InferType::Concrete(struct_ty));
            }
            if let Some(&enum_ty) = self.enums.get(&name_spur) {
                return Some(InferType::Concrete(enum_ty));
            }
            if let Some(&alias_ty) = self
                .const_type_aliases
                .and_then(|aliases| aliases.get(&name_spur))
            {
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
    fn make_test_rir_interner_and_type_pool() -> (Rir, ThreadedRodeo, TypeInternPool) {
        let rir = Rir::new();
        let interner = ThreadedRodeo::new();
        let type_pool = TypeInternPool::new();
        (rir, interner, type_pool)
    }

    #[test]
    fn test_constraint_generator_int_literal() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

        // Add an integer constant to RIR
        let inst_ref = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

        let inst_ref = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::BOOL);

        let info = cgen.generate(inst_ref, &mut ctx);

        assert_eq!(info.ty, InferType::Concrete(Type::BOOL));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_binary_add() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(loop_inst, &mut ctx);

        // While loops produce Unit
        assert_eq!(info.ty, InferType::Concrete(Type::UNIT));
        // Should generate 1 constraint: cond = bool
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_context_scope() {
        let params = HashMap::new();
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
            param_names: vec![],
            param_type_syms: vec![],
            return_type_sym: lasso::Spur::default(),
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

        let break_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::Break { value: None },
            span: Span::new(0, 5),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(break_inst, &mut ctx);

        // Break diverges
        assert_eq!(info.ty, InferType::Concrete(Type::NEVER));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_index_get() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

        // Create: { } (empty block)
        let block = rir.add_inst(rue_rir::Inst {
            data: InstData::Block {
                extra_start: 0,
                len: 0,
            },
            span: Span::new(0, 2),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(block, &mut ctx);

        // Empty block produces Unit
        assert_eq!(info.ty, InferType::Concrete(Type::UNIT));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_bitwise_not() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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
        let params = HashMap::new();
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
        let mut functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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

        // Create a call with only 1 argument (mismatch)
        let arg = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(4, 6),
        });
        let (args_start, args_len) = rir.add_call_args(&[rue_rir::RirCallArg {
            value: arg,
            mode: rue_rir::RirArgMode::Normal,
        }]);
        let call = rir.add_inst(rue_rir::Inst {
            data: InstData::Call {
                name: func_name,
                args_start,
                args_len,
            },
            span: Span::new(0, 7),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = HashMap::new();
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
        let functions = HashMap::new(); // Empty - no functions registered
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

        // Create a call to an unknown function
        let unknown_func = interner.get_or_intern("unknown");
        let arg = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(8, 10),
        });
        let (args_start, args_len) = rir.add_call_args(&[rue_rir::RirCallArg {
            value: arg,
            mode: rue_rir::RirArgMode::Normal,
        }]);
        let call = rir.add_inst(rue_rir::Inst {
            data: InstData::Call {
                name: unknown_func,
                args_start,
                args_len,
            },
            span: Span::new(0, 11),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = HashMap::new();
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
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::new();

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

        let arms = vec![(pattern1, body1), (pattern2, body2), (pattern3, body3)];
        let (arms_start, arms_len) = rir.add_match_arms(&arms);
        let match_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 40),
        });

        let mut cgen = ConstraintGenerator::new(
            &rir, &interner, &functions, &structs, &enums, &methods, &type_pool,
        );
        let params = HashMap::new();
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
}
