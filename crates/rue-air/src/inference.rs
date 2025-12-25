//! Hindley-Milner type inference infrastructure.
//!
//! This module provides the core types and algorithms for constraint-based
//! type inference. The design follows Algorithm W with special handling for
//! integer literals.
//!
//! # Architecture
//!
//! Type inference works in three phases:
//! 1. **Constraint Generation**: Walk RIR, assign type variables to unknowns,
//!    generate constraints
//! 2. **Unification**: Solve constraints using Algorithm W, resolve type
//!    variables to concrete types
//! 3. **AIR Emission**: Walk RIR again with resolved types to emit typed AIR
//!
//! # Type Variables
//!
//! Type variables ([`TypeVarId`]) represent unknown types to be solved.
//! The [`Substitution`] maps type variables to their resolved types.
//!
//! # Integer Literals
//!
//! Integer literals get the special [`InferType::IntLiteral`] type rather than
//! a type variable. This models the fact that a literal like `42` can become
//! any integer type. When an `IntLiteral` unifies with a concrete integer type,
//! it becomes that type. Unconstrained `IntLiteral`s default to `i32` at the end.

use crate::Type;
use rue_span::Span;
use std::collections::HashMap;

/// Unique identifier for a type variable.
///
/// Type variables represent unknown types during constraint generation.
/// They are resolved to concrete types during unification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVarId(u32);

impl TypeVarId {
    /// Create a new type variable ID with the given index.
    #[inline]
    pub fn new(index: u32) -> Self {
        TypeVarId(index)
    }

    /// Get the underlying index.
    #[inline]
    pub fn index(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for TypeVarId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "?{}", self.0)
    }
}

/// Internal type representation during inference.
///
/// This is separate from the final [`Type`] enum to support type variables
/// and the special `IntLiteral` type. After unification, all `InferType`s
/// are resolved to concrete `Type`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferType {
    /// A concrete type (maps directly to [`Type`] enum).
    Concrete(Type),

    /// A type variable (unknown, to be solved).
    Var(TypeVarId),

    /// An integer literal type.
    ///
    /// Integer literals can unify with any integer type (i8-i64, u8-u64).
    /// When unified with a concrete integer type, the literal takes that type.
    /// If unconstrained at the end of inference, defaults to `i32`.
    IntLiteral,

    /// An array type during inference.
    ///
    /// Unlike `Concrete(Type::Array(id))`, this stores the element type as an
    /// `InferType` so we can handle cases where the element type is still a
    /// type variable. After unification, these are converted to `Type::Array`.
    Array {
        element: Box<InferType>,
        length: u64,
    },
}

impl InferType {
    /// Create an `InferType` from a concrete `Type`.
    #[inline]
    pub fn concrete(ty: Type) -> Self {
        InferType::Concrete(ty)
    }

    /// Create a type variable.
    #[inline]
    pub fn var(id: TypeVarId) -> Self {
        InferType::Var(id)
    }

    /// Create an integer literal type.
    #[inline]
    pub fn int_literal() -> Self {
        InferType::IntLiteral
    }

    /// Check if this is a concrete type.
    pub fn is_concrete(&self) -> bool {
        matches!(self, InferType::Concrete(_))
    }

    /// Check if this is a type variable.
    pub fn is_var(&self) -> bool {
        matches!(self, InferType::Var(_))
    }

    /// Check if this is an integer literal type.
    pub fn is_int_literal(&self) -> bool {
        matches!(self, InferType::IntLiteral)
    }

    /// Get the concrete type if this is `Concrete`.
    pub fn as_concrete(&self) -> Option<Type> {
        match self {
            InferType::Concrete(ty) => Some(*ty),
            _ => None,
        }
    }

    /// Get the type variable ID if this is `Var`.
    pub fn as_var(&self) -> Option<TypeVarId> {
        match self {
            InferType::Var(id) => Some(*id),
            _ => None,
        }
    }
}

impl std::fmt::Display for InferType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferType::Concrete(ty) => write!(f, "{ty}"),
            InferType::Var(id) => write!(f, "{id}"),
            InferType::IntLiteral => write!(f, "{{integer}}"),
            InferType::Array { element, length } => write!(f, "[{element}; {length}]"),
        }
    }
}

impl From<Type> for InferType {
    fn from(ty: Type) -> Self {
        InferType::Concrete(ty)
    }
}

/// A type constraint generated during analysis.
///
/// Constraints express relationships between types that must hold.
/// They are collected during constraint generation and then solved
/// during unification.
#[derive(Debug, Clone)]
pub enum Constraint {
    /// Two types must be equal: τ₁ = τ₂.
    ///
    /// This is the primary constraint type. Generated for:
    /// - Binary operations (both operands must have same type)
    /// - Assignments (value type must match variable type)
    /// - Function calls (argument types must match parameter types)
    /// - Return statements (returned type must match declared return type)
    Equal(InferType, InferType, Span),

    /// Type must be a signed integer: τ ∈ {i8, i16, i32, i64}.
    ///
    /// Generated for unary negation which requires signed types.
    /// Unsigned integers cannot be negated.
    IsSigned(InferType, Span),

    /// Type must be an integer (signed or unsigned): τ ∈ {i8, i16, i32, i64, u8, u16, u32, u64}.
    ///
    /// Generated for bitwise NOT which works on any integer type.
    IsInteger(InferType, Span),

    /// Type must be an unsigned integer: τ ∈ {u8, u16, u32, u64}.
    ///
    /// Generated for array indexing which requires non-negative indices.
    IsUnsigned(InferType, Span),
}

impl Constraint {
    /// Create an equality constraint.
    pub fn equal(lhs: InferType, rhs: InferType, span: Span) -> Self {
        Constraint::Equal(lhs, rhs, span)
    }

    /// Create a "must be signed" constraint.
    pub fn is_signed(ty: InferType, span: Span) -> Self {
        Constraint::IsSigned(ty, span)
    }

    /// Create a "must be integer" constraint.
    pub fn is_integer(ty: InferType, span: Span) -> Self {
        Constraint::IsInteger(ty, span)
    }

    /// Create a "must be unsigned" constraint.
    pub fn is_unsigned(ty: InferType, span: Span) -> Self {
        Constraint::IsUnsigned(ty, span)
    }

    /// Get the span for this constraint (for error reporting).
    pub fn span(&self) -> Span {
        match self {
            Constraint::Equal(_, _, span)
            | Constraint::IsSigned(_, span)
            | Constraint::IsInteger(_, span)
            | Constraint::IsUnsigned(_, span) => *span,
        }
    }
}

/// Allocator for fresh type variables.
///
/// Each function gets its own allocator to generate unique type variable IDs.
#[derive(Debug, Default)]
pub struct TypeVarAllocator {
    next_id: u32,
}

impl TypeVarAllocator {
    /// Create a new allocator starting from ID 0.
    pub fn new() -> Self {
        TypeVarAllocator { next_id: 0 }
    }

    /// Allocate a fresh type variable.
    pub fn fresh(&mut self) -> TypeVarId {
        let id = TypeVarId::new(self.next_id);
        self.next_id += 1;
        id
    }

    /// Get the number of type variables allocated.
    pub fn count(&self) -> u32 {
        self.next_id
    }
}

/// A substitution mapping type variables to their resolved types.
///
/// The substitution is built incrementally during unification.
/// It maps type variable IDs to `InferType`s, which may themselves
/// be type variables (requiring transitive lookup via `apply`).
#[derive(Debug, Default)]
pub struct Substitution {
    /// Mapping from type variable ID to its resolved type.
    mapping: HashMap<TypeVarId, InferType>,
}

impl Substitution {
    /// Create an empty substitution.
    pub fn new() -> Self {
        Substitution {
            mapping: HashMap::new(),
        }
    }

    /// Insert a mapping from a type variable to a type.
    ///
    /// If the variable is already mapped, the old mapping is replaced.
    pub fn insert(&mut self, var: TypeVarId, ty: InferType) {
        self.mapping.insert(var, ty);
    }

    /// Look up a type variable's immediate mapping (without following chains).
    pub fn get(&self, var: TypeVarId) -> Option<&InferType> {
        self.mapping.get(&var)
    }

    /// Apply the substitution to a type, following type variable chains
    /// to their ultimate resolution.
    ///
    /// - `Concrete(ty)` → `Concrete(ty)` (unchanged)
    /// - `Var(id)` → follows chain until concrete or unbound variable
    /// - `IntLiteral` → `IntLiteral` (unchanged, unless we add IntLiteral
    ///   to variable mappings)
    /// - `Array { element, length }` → recursively apply to element type
    pub fn apply(&self, ty: &InferType) -> InferType {
        match ty {
            InferType::Concrete(_) => ty.clone(),
            InferType::Var(id) => {
                // Follow the chain of substitutions
                match self.mapping.get(id) {
                    Some(resolved) => self.apply(resolved),
                    None => ty.clone(), // Unbound variable
                }
            }
            InferType::IntLiteral => ty.clone(),
            InferType::Array { element, length } => {
                let resolved_element = self.apply(element);
                InferType::Array {
                    element: Box::new(resolved_element),
                    length: *length,
                }
            }
        }
    }

    /// Check if a type variable occurs in a type (for occurs check).
    ///
    /// This prevents creating infinite types like `α = List<α>`.
    /// Returns `true` if the variable occurs in the type.
    pub fn occurs_in(&self, var: TypeVarId, ty: &InferType) -> bool {
        match ty {
            InferType::Concrete(_) => false,
            InferType::Var(id) => {
                if *id == var {
                    return true;
                }
                // Check if the variable chain leads to our target
                match self.mapping.get(id) {
                    Some(resolved) => self.occurs_in(var, resolved),
                    None => false,
                }
            }
            InferType::IntLiteral => false,
            InferType::Array { element, .. } => self.occurs_in(var, element),
        }
    }

    /// Get the number of mappings in the substitution.
    pub fn len(&self) -> usize {
        self.mapping.len()
    }

    /// Check if the substitution is empty.
    pub fn is_empty(&self) -> bool {
        self.mapping.is_empty()
    }
}

/// Result of unifying two types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifyResult {
    /// Unification succeeded.
    Ok,

    /// Types are incompatible.
    TypeMismatch {
        expected: InferType,
        found: InferType,
    },

    /// Integer literal cannot unify with non-integer type.
    IntLiteralNonInteger { found: Type },

    /// Occurs check failed (would create infinite type).
    OccursCheck { var: TypeVarId, ty: InferType },

    /// Type must be signed but is unsigned.
    NotSigned { ty: Type },

    /// Type must be an integer but is not.
    NotInteger { ty: Type },

    /// Type must be unsigned but is signed.
    NotUnsigned { ty: Type },

    /// Array lengths don't match.
    ArrayLengthMismatch { expected: u64, found: u64 },
}

impl UnifyResult {
    /// Check if unification succeeded.
    pub fn is_ok(&self) -> bool {
        matches!(self, UnifyResult::Ok)
    }
}

/// An error that occurred during unification.
///
/// Captures the error details along with the span for error reporting.
#[derive(Debug, Clone)]
pub struct UnificationError {
    /// The type of error that occurred.
    pub kind: UnifyResult,
    /// The source location where the error occurred.
    pub span: Span,
}

impl UnificationError {
    /// Create a new unification error.
    pub fn new(kind: UnifyResult, span: Span) -> Self {
        Self { kind, span }
    }

    /// Get a human-readable error message.
    pub fn message(&self) -> String {
        match &self.kind {
            UnifyResult::Ok => unreachable!("UnificationError should never contain Ok"),
            UnifyResult::TypeMismatch { expected, found } => {
                format!("type mismatch: expected {expected}, found {found}")
            }
            UnifyResult::IntLiteralNonInteger { found } => {
                format!("integer literal cannot be used as {found}")
            }
            UnifyResult::OccursCheck { var, ty } => {
                format!("infinite type: {var} cannot unify with {ty}")
            }
            UnifyResult::NotSigned { ty } => {
                format!("cannot negate unsigned type {ty}")
            }
            UnifyResult::NotInteger { ty } => {
                format!("expected integer type, found {ty}")
            }
            UnifyResult::NotUnsigned { ty } => {
                format!("array index must be unsigned integer type, found {ty}")
            }
            UnifyResult::ArrayLengthMismatch { expected, found } => {
                format!("array length mismatch: expected {expected}, found {found}")
            }
        }
    }
}

/// Unification engine for type inference.
///
/// The unifier processes constraints and builds a substitution mapping
/// type variables to their resolved types.
#[derive(Debug)]
pub struct Unifier {
    /// The current substitution being built.
    pub substitution: Substitution,
}

impl Default for Unifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Unifier {
    /// Create a new unifier with an empty substitution.
    pub fn new() -> Self {
        Unifier {
            substitution: Substitution::new(),
        }
    }

    /// Unify two types.
    ///
    /// This is the core of Algorithm W. It updates the substitution
    /// to make the two types equal, or returns an error if they cannot
    /// be unified.
    ///
    /// Unification rules:
    /// 1. `Concrete(T) = Concrete(T)` → succeeds if types are equal
    /// 2. `Var(α) = τ` → binds α to τ (after occurs check)
    /// 3. `τ = Var(α)` → binds α to τ (symmetric)
    /// 4. `IntLiteral = Concrete(integer)` → succeeds (literal takes integer type)
    /// 5. `IntLiteral = IntLiteral` → succeeds (both stay as IntLiteral)
    /// 6. `IntLiteral = Concrete(non-integer)` → fails
    /// 7. `Array(T1, N) = Array(T2, N)` → succeeds if T1 unifies with T2
    /// 8. `Array(T1, N1) = Array(T2, N2)` where N1 ≠ N2 → fails
    pub fn unify(&mut self, lhs: &InferType, rhs: &InferType) -> UnifyResult {
        // Apply current substitution to get most specific types
        let lhs_resolved = self.substitution.apply(lhs);
        let rhs_resolved = self.substitution.apply(rhs);

        match (&lhs_resolved, &rhs_resolved) {
            // Same concrete types unify
            (InferType::Concrete(t1), InferType::Concrete(t2)) => {
                if t1 == t2 || t1.can_coerce_to(t2) || t2.can_coerce_to(t1) {
                    UnifyResult::Ok
                } else {
                    UnifyResult::TypeMismatch {
                        expected: lhs_resolved,
                        found: rhs_resolved,
                    }
                }
            }

            // Variable on left: bind it to right (if occurs check passes)
            (InferType::Var(var), _) => self.bind(*var, &rhs_resolved),

            // Variable on right: bind it to left
            (_, InferType::Var(var)) => self.bind(*var, &lhs_resolved),

            // IntLiteral with concrete type
            (InferType::IntLiteral, InferType::Concrete(ty))
            | (InferType::Concrete(ty), InferType::IntLiteral) => {
                if ty.is_integer() {
                    // IntLiteral can become any integer type.
                    // If the original type was a variable bound to IntLiteral,
                    // rebind it to the concrete type.
                    self.rebind_int_literal_to_concrete(lhs, ty);
                    self.rebind_int_literal_to_concrete(rhs, ty);
                    UnifyResult::Ok
                } else if ty.is_error() {
                    // Error type propagates
                    UnifyResult::Ok
                } else {
                    UnifyResult::IntLiteralNonInteger { found: *ty }
                }
            }

            // Two IntLiterals unify (both remain as IntLiteral)
            (InferType::IntLiteral, InferType::IntLiteral) => UnifyResult::Ok,

            // Two arrays: lengths must match, element types must unify
            (
                InferType::Array {
                    element: elem1,
                    length: len1,
                },
                InferType::Array {
                    element: elem2,
                    length: len2,
                },
            ) => {
                if len1 != len2 {
                    UnifyResult::ArrayLengthMismatch {
                        expected: *len1,
                        found: *len2,
                    }
                } else {
                    // Recursively unify element types
                    self.unify(elem1, elem2)
                }
            }

            // Array with non-array: type mismatch
            // Note: This also handles Array with IntLiteral since the IntLiteral
            // cases with Concrete and IntLiteral are already handled above.
            (InferType::Array { .. }, _) | (_, InferType::Array { .. }) => {
                UnifyResult::TypeMismatch {
                    expected: lhs_resolved,
                    found: rhs_resolved,
                }
            }
        }
    }

    /// If the original type was a variable bound to IntLiteral,
    /// rebind it to the concrete integer type.
    fn rebind_int_literal_to_concrete(&mut self, original: &InferType, concrete_ty: &Type) {
        if let InferType::Var(var) = original {
            // Check if this variable is directly bound to IntLiteral
            if let Some(bound) = self.substitution.get(*var) {
                if bound.is_int_literal() {
                    self.substitution
                        .insert(*var, InferType::Concrete(*concrete_ty));
                }
            }
        }
    }

    /// Bind a type variable to a type.
    ///
    /// Performs the occurs check to prevent infinite types.
    fn bind(&mut self, var: TypeVarId, ty: &InferType) -> UnifyResult {
        // If binding to itself, it's a no-op
        if let InferType::Var(id) = ty {
            if *id == var {
                return UnifyResult::Ok;
            }
        }

        // Occurs check: prevent infinite types
        if self.substitution.occurs_in(var, ty) {
            return UnifyResult::OccursCheck {
                var,
                ty: ty.clone(),
            };
        }

        self.substitution.insert(var, ty.clone());
        UnifyResult::Ok
    }

    /// Check that a type is signed.
    ///
    /// Returns an error if the type is a concrete unsigned integer.
    /// For type variables and IntLiterals, this check is deferred
    /// (the constraint would need to be stored and checked later).
    pub fn check_signed(&self, ty: &InferType) -> UnifyResult {
        let ty = self.substitution.apply(ty);
        match &ty {
            InferType::Concrete(concrete) => {
                if concrete.is_signed() || concrete.is_error() || concrete.is_never() {
                    UnifyResult::Ok
                } else if concrete.is_unsigned() {
                    UnifyResult::NotSigned { ty: *concrete }
                } else {
                    // Non-integer type - this will be caught elsewhere
                    UnifyResult::Ok
                }
            }
            // For variables and IntLiteral, assume OK for now
            // (IntLiteral defaults to i32 which is signed)
            InferType::Var(_) | InferType::IntLiteral => UnifyResult::Ok,
            // Arrays are not signed - error will be caught elsewhere
            InferType::Array { .. } => UnifyResult::Ok,
        }
    }

    /// Check that a type is an integer (signed or unsigned).
    ///
    /// Returns an error if the type is a concrete non-integer type.
    /// For type variables and IntLiterals, the check passes.
    pub fn check_integer(&self, ty: &InferType) -> UnifyResult {
        let ty = self.substitution.apply(ty);
        match &ty {
            InferType::Concrete(concrete) => {
                if concrete.is_integer() || concrete.is_error() || concrete.is_never() {
                    UnifyResult::Ok
                } else {
                    UnifyResult::NotInteger { ty: *concrete }
                }
            }
            // Type variables and IntLiteral are OK - they will be resolved to integers
            InferType::Var(_) | InferType::IntLiteral => UnifyResult::Ok,
            // Arrays are not integers - return error
            InferType::Array { .. } => UnifyResult::NotInteger { ty: Type::Error },
        }
    }

    /// Check that a type is an unsigned integer.
    ///
    /// Returns an error if the type is a concrete signed integer or non-integer type.
    /// For type variables, the check is deferred.
    /// For IntLiteral, we allow it and it will be bound to u64 (the default unsigned type).
    pub fn check_unsigned(&self, ty: &InferType) -> UnifyResult {
        let ty = self.substitution.apply(ty);
        match &ty {
            InferType::Concrete(concrete) => {
                if concrete.is_unsigned() || concrete.is_error() || concrete.is_never() {
                    UnifyResult::Ok
                } else if concrete.is_signed() {
                    UnifyResult::NotUnsigned { ty: *concrete }
                } else {
                    // Non-integer type - report as not unsigned
                    UnifyResult::NotUnsigned { ty: *concrete }
                }
            }
            // Type variables - defer check (will be validated in sema)
            InferType::Var(_) => UnifyResult::Ok,
            // IntLiteral can be used as unsigned - it will be inferred to u64
            InferType::IntLiteral => UnifyResult::Ok,
            // Arrays are not unsigned integers
            InferType::Array { .. } => UnifyResult::NotUnsigned { ty: Type::Error },
        }
    }

    /// Solve a list of constraints, collecting any errors.
    ///
    /// This is the main entry point for Algorithm W. It processes each constraint
    /// in order, updates the substitution, and collects errors for reporting.
    ///
    /// On error, the unifier continues processing remaining constraints to catch
    /// as many errors as possible in one pass. Type variables involved in errors
    /// are bound to `Type::Error` for recovery.
    ///
    /// Returns a list of errors (empty if all constraints were satisfied).
    pub fn solve_constraints(&mut self, constraints: &[Constraint]) -> Vec<UnificationError> {
        let mut errors = Vec::new();

        for constraint in constraints {
            let result = match constraint {
                Constraint::Equal(lhs, rhs, span) => {
                    let result = self.unify(lhs, rhs);
                    if !result.is_ok() {
                        // On error, try to bind any unbound type variables to Error
                        // for recovery
                        self.recover_from_error(lhs, rhs);
                        errors.push(UnificationError::new(result, *span));
                    }
                    continue;
                }
                Constraint::IsSigned(ty, span) => {
                    let result = self.check_signed(ty);
                    (result, *span)
                }
                Constraint::IsInteger(ty, span) => {
                    let result = self.check_integer(ty);
                    (result, *span)
                }
                Constraint::IsUnsigned(ty, span) => {
                    // Special handling: if the type is an unbound variable or IntLiteral,
                    // bind it to u64. This handles integer literals used as array indices.
                    let applied = self.substitution.apply(ty);
                    match &applied {
                        InferType::IntLiteral => {
                            // IntLiteral bound through a chain - bind the variable to u64
                            if let InferType::Var(var) = ty {
                                self.substitution
                                    .insert(*var, InferType::Concrete(Type::U64));
                            }
                        }
                        InferType::Var(var) => {
                            // Unbound variable - bind it to u64
                            // This happens for integer literal variables that haven't
                            // been constrained yet.
                            self.substitution
                                .insert(*var, InferType::Concrete(Type::U64));
                        }
                        _ => {}
                    }
                    let result = self.check_unsigned(ty);
                    (result, *span)
                }
            };

            if !result.0.is_ok() {
                errors.push(UnificationError::new(result.0, result.1));
            }
        }

        errors
    }

    /// Recover from a unification error by binding unbound type variables to Error.
    ///
    /// This allows type checking to continue after an error, catching more
    /// errors in a single pass.
    fn recover_from_error(&mut self, lhs: &InferType, rhs: &InferType) {
        // Apply substitution first to find any unbound variables
        let lhs_applied = self.substitution.apply(lhs);
        let rhs_applied = self.substitution.apply(rhs);

        // Bind any unbound type variables to Error for recovery
        if let InferType::Var(var) = lhs_applied {
            self.substitution
                .insert(var, InferType::Concrete(Type::Error));
        }
        if let InferType::Var(var) = rhs_applied {
            self.substitution
                .insert(var, InferType::Concrete(Type::Error));
        }
    }

    /// Default all IntLiteral types bound to type variables to i32.
    ///
    /// Called at the end of unification for a function to resolve
    /// any unconstrained integer literals. This processes all type variables
    /// in the substitution that are bound to IntLiteral.
    /// Default unconstrained integer literal type variables to i32.
    ///
    /// Takes the list of type variables that were allocated for integer literals
    /// during constraint generation. Any that remain unbound (not constrained to
    /// a specific integer type) are defaulted to i32.
    pub fn default_int_literal_vars(&mut self, int_literal_vars: &[TypeVarId]) {
        for &var in int_literal_vars {
            // Check if this variable is already bound to a concrete type
            let resolved = self.substitution.apply(&InferType::Var(var));
            if let InferType::Var(_) = resolved {
                // Still unbound - default to i32
                self.substitution
                    .insert(var, InferType::Concrete(Type::I32));
            }
            // If it resolved to Concrete, it was already constrained - no action needed
        }
    }

    /// Resolve a type to its final form after applying all substitutions.
    ///
    /// Follows the substitution chain and defaults IntLiteral to i32.
    /// For arrays, recursively resolves the element type.
    /// Returns the fully-resolved `InferType`.
    pub fn resolve_infer_type(&self, ty: &InferType) -> InferType {
        let resolved = self.substitution.apply(ty);
        match resolved {
            InferType::Concrete(_) => resolved,
            InferType::Var(_) => resolved, // Unbound variable stays as-is
            InferType::IntLiteral => InferType::Concrete(Type::I32), // Default to i32
            InferType::Array { element, length } => {
                let resolved_element = self.resolve_infer_type(&element);
                InferType::Array {
                    element: Box::new(resolved_element),
                    length,
                }
            }
        }
    }

    /// Resolve a type to its final concrete type.
    ///
    /// Follows the substitution chain and defaults IntLiteral to i32.
    /// Returns `None` if the type resolves to an unbound type variable or an array
    /// (arrays need to be handled separately to create ArrayTypeIds).
    pub fn resolve(&self, ty: &InferType) -> Option<Type> {
        let resolved = self.resolve_infer_type(ty);
        match resolved {
            InferType::Concrete(t) => Some(t),
            InferType::Var(_) => None,                // Unbound variable
            InferType::IntLiteral => Some(Type::I32), // Default to i32 (shouldn't happen after resolve_infer_type)
            InferType::Array { .. } => None,          // Arrays need special handling
        }
    }

    /// Resolve a type to its final concrete type, with error recovery.
    ///
    /// Like `resolve`, but returns `Type::Error` instead of `None` for
    /// unbound type variables. This allows compilation to continue
    /// with error recovery.
    ///
    /// Note: For arrays, this returns `Type::Error`. Use `resolve_infer_type`
    /// and handle `InferType::Array` explicitly to create proper ArrayTypeIds.
    pub fn resolve_or_error(&self, ty: &InferType) -> Type {
        self.resolve(ty).unwrap_or(Type::Error)
    }
}

// ============================================================================
// Constraint Generation (Phase 2)
// ============================================================================

use rue_intern::{Interner, Symbol};
use rue_rir::{InstData, InstRef, Rir};

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
    pub locals: HashMap<Symbol, LocalVarInfo>,
    /// Function parameters.
    pub params: &'a HashMap<Symbol, ParamVarInfo>,
    /// Return type of the current function.
    pub return_type: Type,
    /// How many loops we're nested inside (for break/continue validation).
    pub loop_depth: u32,
    /// Scope stack for efficient scope management.
    scope_stack: Vec<Vec<(Symbol, Option<LocalVarInfo>)>>,
}

impl<'a> ConstraintContext<'a> {
    /// Create a new context for a function.
    pub fn new(params: &'a HashMap<Symbol, ParamVarInfo>, return_type: Type) -> Self {
        Self {
            locals: HashMap::new(),
            params,
            return_type,
            loop_depth: 0,
            scope_stack: Vec::new(),
        }
    }

    /// Push a new scope onto the stack.
    pub fn push_scope(&mut self) {
        // Pre-allocate for 2 variables since most scopes introduce few bindings
        self.scope_stack.push(Vec::with_capacity(2));
    }

    /// Pop the current scope, restoring any shadowed variables.
    pub fn pop_scope(&mut self) {
        if let Some(scope_entries) = self.scope_stack.pop() {
            for (symbol, old_value) in scope_entries {
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
    }

    /// Insert a local variable, tracking it in the current scope.
    pub fn insert_local(&mut self, symbol: Symbol, var: LocalVarInfo) {
        let old_value = self.locals.insert(symbol, var);
        if let Some(current_scope) = self.scope_stack.last_mut() {
            current_scope.push((symbol, old_value));
        }
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
    interner: &'a Interner,
    /// Type variable allocator.
    type_vars: TypeVarAllocator,
    /// Collected constraints.
    constraints: Vec<Constraint>,
    /// Mapping from RIR instruction to its inferred type.
    expr_types: HashMap<InstRef, InferType>,
    /// Function signatures (for call type checking).
    functions: &'a HashMap<Symbol, FunctionSig>,
    /// Struct types (name -> Type::Struct(id)).
    structs: &'a HashMap<Symbol, Type>,
    /// Enum types (name -> Type::Enum(id)).
    enums: &'a HashMap<Symbol, Type>,
    /// Method signatures: (struct_name, method_name) -> MethodSig
    methods: &'a HashMap<(Symbol, Symbol), MethodSig>,
    /// Type variables allocated for integer literals.
    /// These start as unbound and need to be defaulted to i32 if unconstrained.
    int_literal_vars: Vec<TypeVarId>,
}

impl<'a> ConstraintGenerator<'a> {
    /// Create a new constraint generator.
    pub fn new(
        rir: &'a Rir,
        interner: &'a Interner,
        functions: &'a HashMap<Symbol, FunctionSig>,
        structs: &'a HashMap<Symbol, Type>,
        enums: &'a HashMap<Symbol, Type>,
        methods: &'a HashMap<(Symbol, Symbol), MethodSig>,
    ) -> Self {
        Self {
            rir,
            interner,
            type_vars: TypeVarAllocator::new(),
            constraints: Vec::new(),
            expr_types: HashMap::new(),
            functions,
            structs,
            enums,
            methods,
            int_literal_vars: Vec::new(),
        }
    }

    /// Get the type variables allocated for integer literals.
    pub fn int_literal_vars(&self) -> &[TypeVarId] {
        &self.int_literal_vars
    }

    /// Allocate a fresh type variable.
    pub fn fresh_var(&mut self) -> TypeVarId {
        self.type_vars.fresh()
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

    /// Consume the constraint generator and return (constraints, int_literal_vars, expr_types).
    ///
    /// This is useful when you need ownership of the expression types map.
    pub fn into_parts(self) -> (Vec<Constraint>, Vec<TypeVarId>, HashMap<InstRef, InferType>) {
        (self.constraints, self.int_literal_vars, self.expr_types)
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
                // Integer literals get a fresh type variable that we immediately
                // bind to IntLiteral. This allows unification to track when the
                // literal is constrained to a specific integer type.
                //
                // Example: `let x: i64 = 42` generates:
                //   - type_var(?0) for the literal 42
                //   - substitution: ?0 -> IntLiteral
                //   - constraint: Equal(Var(?0), Concrete(i64))
                //
                // During unification, Equal(IntLiteral, Concrete(i64)) succeeds
                // and rebinds ?0 -> Concrete(i64) via rebind_int_literal_to_concrete.
                let var = self.fresh_var();
                self.int_literal_vars.push(var);
                InferType::Var(var)
            }

            InstData::BoolConst(_) => InferType::Concrete(Type::Bool),

            InstData::StringConst(_) => InferType::Concrete(Type::String),

            InstData::UnitConst => InferType::Concrete(Type::Unit),

            // Binary arithmetic: both operands must have the same type, result is that type
            InstData::Add { lhs, rhs }
            | InstData::Sub { lhs, rhs }
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
                // Operands must have the same type
                self.add_constraint(Constraint::equal(lhs_info.ty, rhs_info.ty, span));
                InferType::Concrete(Type::Bool)
            }

            // Logical operators: operands must be bool, result is bool
            InstData::And { lhs, rhs } | InstData::Or { lhs, rhs } => {
                let lhs_info = self.generate(*lhs, ctx);
                let rhs_info = self.generate(*rhs, ctx);
                self.add_constraint(Constraint::equal(
                    lhs_info.ty,
                    InferType::Concrete(Type::Bool),
                    lhs_info.span,
                ));
                self.add_constraint(Constraint::equal(
                    rhs_info.ty,
                    InferType::Concrete(Type::Bool),
                    rhs_info.span,
                ));
                InferType::Concrete(Type::Bool)
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
                    InferType::Concrete(Type::Bool),
                    operand_info.span,
                ));
                InferType::Concrete(Type::Bool)
            }

            // Bitwise NOT: operand must be integer
            InstData::BitNot { operand } => {
                let operand_info = self.generate(*operand, ctx);
                let result_ty = operand_info.ty.clone();
                // Must be an integer type (signed or unsigned)
                self.add_constraint(Constraint::is_integer(result_ty.clone(), span));
                result_ty
            }

            // Variable reference
            InstData::VarRef { name } => {
                if let Some(local) = ctx.locals.get(name) {
                    local.ty.clone()
                } else if let Some(param) = ctx.params.get(name) {
                    param.ty.clone()
                } else {
                    // Unknown variable - will be caught during semantic analysis
                    InferType::Concrete(Type::Error)
                }
            }

            // Parameter reference
            InstData::ParamRef { name, .. } => {
                if let Some(param) = ctx.params.get(name) {
                    param.ty.clone()
                } else {
                    InferType::Concrete(Type::Error)
                }
            }

            // Local variable allocation
            InstData::Alloc {
                directives: _,
                name,
                is_mut,
                ty: type_annotation,
                init,
            } => {
                let init_info = self.generate(*init, ctx);

                let var_ty = if let Some(ty_sym) = type_annotation {
                    // Explicit type annotation - use it and constrain init to match
                    let ty_name = self.interner.get(*ty_sym);
                    if let Some(annotated_ty) = self.resolve_type_name(ty_name) {
                        self.add_constraint(Constraint::equal(
                            init_info.ty,
                            annotated_ty.clone(),
                            span,
                        ));
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
                InferType::Concrete(Type::Unit)
            }

            // Assignment
            InstData::Assign { name, value } => {
                let value_info = self.generate(*value, ctx);
                if let Some(local) = ctx.locals.get(name) {
                    // Constrain value to match variable type
                    self.add_constraint(Constraint::equal(value_info.ty, local.ty.clone(), span));
                }
                // Assignment produces unit
                InferType::Concrete(Type::Unit)
            }

            // Return statement
            InstData::Ret(value) => {
                if let Some(val_ref) = value {
                    let value_info = self.generate(*val_ref, ctx);
                    // Constrain return value to match function return type
                    self.add_constraint(Constraint::equal(
                        value_info.ty,
                        InferType::Concrete(ctx.return_type),
                        span,
                    ));
                } else {
                    // Return without value - function must return unit
                    self.add_constraint(Constraint::equal(
                        InferType::Concrete(Type::Unit),
                        InferType::Concrete(ctx.return_type),
                        span,
                    ));
                }
                // Return diverges
                InferType::Concrete(Type::Never)
            }

            // Function call
            InstData::Call { name, args } => {
                if let Some(func) = self.functions.get(name) {
                    // Check argument count matches parameter count.
                    // Semantic analysis will emit a proper error; we just need to avoid
                    // panicking and process what we can.
                    if args.len() != func.param_types.len() {
                        // Still process all arguments to catch type errors within them
                        for arg_ref in args.iter() {
                            self.generate(*arg_ref, ctx);
                        }
                        // Return the declared return type (error will be caught in sema)
                        func.return_type.clone()
                    } else {
                        // Generate constraints for each argument
                        for (arg_ref, param_ty) in args.iter().zip(func.param_types.iter()) {
                            let arg_info = self.generate(*arg_ref, ctx);
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
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::Error)
                }
            }

            // Intrinsic call
            InstData::Intrinsic { name: _, args } => {
                // Generate constraints for arguments (they need to be processed)
                for arg_ref in args.iter() {
                    self.generate(*arg_ref, ctx);
                }
                // Currently only @dbg is supported, which returns Unit
                InferType::Concrete(Type::Unit)
            }

            // Type intrinsic (@size_of, @align_of)
            InstData::TypeIntrinsic {
                name: _,
                type_arg: _,
            } => {
                // Type intrinsics return i32 (the size or alignment value)
                InferType::Concrete(Type::I32)
            }

            // Block
            InstData::Block { extra_start, len } => {
                ctx.push_scope();
                let mut last_ty = InferType::Concrete(Type::Unit);
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
                    InferType::Concrete(Type::Bool),
                    cond_info.span,
                ));

                let then_info = self.generate(*then_block, ctx);

                if let Some(else_ref) = else_block {
                    let else_info = self.generate(*else_ref, ctx);

                    // Handle Never type coercion:
                    // - If one branch is Never, the if-else takes the other branch's type
                    // - If both are Never, the result is Never
                    // - Otherwise, both must unify to the same type
                    let then_is_never = matches!(&then_info.ty, InferType::Concrete(Type::Never));
                    let else_is_never = matches!(&else_info.ty, InferType::Concrete(Type::Never));

                    match (then_is_never, else_is_never) {
                        (true, true) => {
                            // Both diverge - result is Never
                            InferType::Concrete(Type::Never)
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
                    InferType::Concrete(Type::Unit)
                }
            }

            // While loop
            InstData::Loop { cond, body } => {
                let cond_info = self.generate(*cond, ctx);
                self.add_constraint(Constraint::equal(
                    cond_info.ty,
                    InferType::Concrete(Type::Bool),
                    cond_info.span,
                ));

                ctx.loop_depth += 1;
                self.generate(*body, ctx);
                ctx.loop_depth -= 1;

                // Loops produce unit
                InferType::Concrete(Type::Unit)
            }

            // Infinite loop
            InstData::InfiniteLoop { body } => {
                ctx.loop_depth += 1;
                self.generate(*body, ctx);
                ctx.loop_depth -= 1;

                // Infinite loop without break never returns
                InferType::Concrete(Type::Never)
            }

            // Break/Continue
            InstData::Break | InstData::Continue => InferType::Concrete(Type::Never),

            // Match expression
            InstData::Match { scrutinee, arms } => {
                let scrutinee_info = self.generate(*scrutinee, ctx);

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

                    // Generate body and collect its type
                    let body_info = self.generate(*body, ctx);
                    arm_types.push(body_info);
                }

                // Handle Never type coercion:
                // Filter out Never arms and use the remaining non-Never types
                let non_never_arms: Vec<_> = arm_types
                    .iter()
                    .filter(|info| !matches!(&info.ty, InferType::Concrete(Type::Never)))
                    .collect();

                if non_never_arms.is_empty() {
                    // All arms diverge - result is Never
                    InferType::Concrete(Type::Never)
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
            InstData::StructInit { type_name, fields } => {
                if let Some(&struct_ty) = self.structs.get(type_name) {
                    // Generate constraints for each field
                    for (_, value_ref) in fields.iter() {
                        self.generate(*value_ref, ctx);
                    }
                    InferType::Concrete(struct_ty)
                } else {
                    InferType::Concrete(Type::Error)
                }
            }

            // Field access
            InstData::FieldGet { base, field: _ } => {
                // Generate constraints for the base expression (needed for nested field access)
                let _base_info = self.generate(*base, ctx);
                // We need to look up the field type from the struct definition.
                // For now, use a fresh type variable - full resolution happens during
                // semantic analysis which has access to struct definitions.
                let result_var = self.fresh_var();
                InferType::Var(result_var)
            }

            // Field assignment
            InstData::FieldSet {
                base,
                field: _,
                value,
            } => {
                self.generate(*base, ctx);
                self.generate(*value, ctx);
                InferType::Concrete(Type::Unit)
            }

            // Enum variant
            InstData::EnumVariant {
                type_name,
                variant: _,
            } => {
                if let Some(&enum_ty) = self.enums.get(type_name) {
                    InferType::Concrete(enum_ty)
                } else {
                    InferType::Concrete(Type::Error)
                }
            }

            // Array initialization
            InstData::ArrayInit { elements } => {
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

            // Array index
            InstData::IndexGet { base, index } => {
                let base_info = self.generate(*base, ctx);
                let index_info = self.generate(*index, ctx);
                // Index must be an unsigned integer type
                self.add_constraint(Constraint::is_unsigned(index_info.ty, index_info.span));

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
                // Index must be an unsigned integer type
                self.add_constraint(Constraint::is_unsigned(index_info.ty, index_info.span));

                let value_info = self.generate(*value, ctx);

                // Constrain value type to match array element type
                if let InferType::Array { element, .. } = &base_info.ty {
                    self.add_constraint(Constraint::equal(
                        value_info.ty,
                        (**element).clone(),
                        value_info.span,
                    ));
                }

                InferType::Concrete(Type::Unit)
            }

            // Type declarations don't produce values
            InstData::FnDecl { .. }
            | InstData::StructDecl { .. }
            | InstData::EnumDecl { .. }
            | InstData::ImplDecl { .. }
            | InstData::DropFnDecl { .. } => InferType::Concrete(Type::Unit),

            // Method call: receiver.method(args)
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                // Generate type for receiver
                let receiver_info = self.generate(*receiver, ctx);

                // Get struct name from receiver type if it's a struct
                // If we can't determine the struct type, we still generate constraints
                // for the arguments and return a type variable (actual error is in sema)
                let result_type = if let InferType::Concrete(Type::Struct(struct_id)) =
                    &receiver_info.ty
                {
                    // Find the struct name symbol
                    let struct_name = self
                        .structs
                        .iter()
                        .find(|(_, ty)| **ty == Type::Struct(*struct_id))
                        .map(|(name, _)| *name);

                    if let Some(struct_name) = struct_name {
                        let method_key = (struct_name, *method);
                        if let Some(method_sig) = self.methods.get(&method_key) {
                            // Generate constraints for arguments
                            for (arg, param_type) in args.iter().zip(method_sig.param_types.iter())
                            {
                                let arg_info = self.generate(*arg, ctx);
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
                                self.generate(*arg, ctx);
                            }
                            InferType::Concrete(Type::Error)
                        }
                    } else {
                        // Couldn't find struct name - shouldn't happen but handle gracefully
                        for arg in args.iter() {
                            self.generate(*arg, ctx);
                        }
                        InferType::Concrete(Type::Error)
                    }
                } else {
                    // Non-struct receiver - sema will report the error
                    for arg in args.iter() {
                        self.generate(*arg, ctx);
                    }
                    InferType::Concrete(Type::Error)
                };

                result_type
            }

            // Associated function call: Type::function(args)
            InstData::AssocFnCall {
                type_name,
                function,
                args,
            } => {
                let method_key = (*type_name, *function);
                if let Some(method_sig) = self.methods.get(&method_key) {
                    // Generate constraints for arguments
                    for (arg, param_type) in args.iter().zip(method_sig.param_types.iter()) {
                        let arg_info = self.generate(*arg, ctx);
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
                        self.generate(*arg, ctx);
                    }
                    InferType::Concrete(Type::Error)
                }
            }
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

        result_ty
    }

    /// Get the inferred type for a pattern.
    fn pattern_type(&mut self, pattern: &rue_rir::RirPattern) -> InferType {
        match pattern {
            rue_rir::RirPattern::Wildcard(_) => {
                // Wildcard matches anything - use a fresh type variable
                let var = self.fresh_var();
                InferType::Var(var)
            }
            rue_rir::RirPattern::Int(_, _) => InferType::IntLiteral,
            rue_rir::RirPattern::Bool(_, _) => InferType::Concrete(Type::Bool),
            rue_rir::RirPattern::Path { type_name, .. } => {
                if let Some(&enum_ty) = self.enums.get(type_name) {
                    InferType::Concrete(enum_ty)
                } else {
                    InferType::Concrete(Type::Error)
                }
            }
        }
    }

    /// Resolve a type name to an InferType.
    ///
    /// Handles primitive types, array syntax `[T; N]`, and struct/enum types.
    fn resolve_type_name(&self, name: &str) -> Option<InferType> {
        // Check for array syntax first: [T; N]
        if let Some((element_type_str, length)) = Self::parse_array_type_syntax(name) {
            // Recursively resolve the element type
            let element_ty = self.resolve_type_name(&element_type_str)?;
            return Some(InferType::Array {
                element: Box::new(element_ty),
                length,
            });
        }

        // Check primitives
        let ty = match name {
            "i8" => Type::I8,
            "i16" => Type::I16,
            "i32" => Type::I32,
            "i64" => Type::I64,
            "u8" => Type::U8,
            "u16" => Type::U16,
            "u32" => Type::U32,
            "u64" => Type::U64,
            "bool" => Type::Bool,
            "()" => Type::Unit,
            "String" => Type::String,
            _ => return None, // Struct/enum types need to be looked up separately
        };
        Some(InferType::Concrete(ty))
    }

    /// Parse array type syntax "[T; N]" and return (element_type_str, length).
    fn parse_array_type_syntax(type_name: &str) -> Option<(String, u64)> {
        let type_name = type_name.trim();
        if !type_name.starts_with('[') || !type_name.ends_with(']') {
            return None;
        }

        // Remove the outer brackets
        let inner = &type_name[1..type_name.len() - 1];

        // Find the semicolon separator - need to handle nested arrays
        // We look for the last `;` that's at nesting level 0
        let mut bracket_depth = 0;
        let mut semi_pos = None;
        for (i, ch) in inner.char_indices() {
            match ch {
                '[' => bracket_depth += 1,
                ']' => bracket_depth -= 1,
                ';' if bracket_depth == 0 => semi_pos = Some(i),
                _ => {}
            }
        }

        let semi_pos = semi_pos?;
        let element_type = inner[..semi_pos].trim().to_string();
        let length_str = inner[semi_pos + 1..].trim();
        let length: u64 = length_str.parse().ok()?;

        Some((element_type, length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_var_allocator() {
        let mut alloc = TypeVarAllocator::new();
        let v0 = alloc.fresh();
        let v1 = alloc.fresh();
        let v2 = alloc.fresh();

        assert_eq!(v0.index(), 0);
        assert_eq!(v1.index(), 1);
        assert_eq!(v2.index(), 2);
        assert_eq!(alloc.count(), 3);
    }

    #[test]
    fn test_infer_type_display() {
        assert_eq!(format!("{}", InferType::Concrete(Type::I32)), "i32");
        assert_eq!(format!("{}", InferType::Var(TypeVarId::new(5))), "?5");
        assert_eq!(format!("{}", InferType::IntLiteral), "{integer}");
    }

    #[test]
    fn test_substitution_apply_concrete() {
        let subst = Substitution::new();
        let ty = InferType::Concrete(Type::I64);
        assert_eq!(subst.apply(&ty), InferType::Concrete(Type::I64));
    }

    #[test]
    fn test_substitution_apply_unbound_var() {
        let subst = Substitution::new();
        let v0 = TypeVarId::new(0);
        let ty = InferType::Var(v0);
        // Unbound variable returns itself
        assert_eq!(subst.apply(&ty), InferType::Var(v0));
    }

    #[test]
    fn test_substitution_apply_bound_var() {
        let mut subst = Substitution::new();
        let v0 = TypeVarId::new(0);
        subst.insert(v0, InferType::Concrete(Type::Bool));

        let ty = InferType::Var(v0);
        assert_eq!(subst.apply(&ty), InferType::Concrete(Type::Bool));
    }

    #[test]
    fn test_substitution_apply_chain() {
        let mut subst = Substitution::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);
        let v2 = TypeVarId::new(2);

        // Create chain: v0 -> v1 -> v2 -> i32
        subst.insert(v0, InferType::Var(v1));
        subst.insert(v1, InferType::Var(v2));
        subst.insert(v2, InferType::Concrete(Type::I32));

        assert_eq!(
            subst.apply(&InferType::Var(v0)),
            InferType::Concrete(Type::I32)
        );
    }

    #[test]
    fn test_occurs_check_simple() {
        let subst = Substitution::new();
        let v0 = TypeVarId::new(0);

        // Variable occurs in itself
        assert!(subst.occurs_in(v0, &InferType::Var(v0)));

        // Variable doesn't occur in different variable
        assert!(!subst.occurs_in(v0, &InferType::Var(TypeVarId::new(1))));

        // Variable doesn't occur in concrete type
        assert!(!subst.occurs_in(v0, &InferType::Concrete(Type::I32)));
    }

    #[test]
    fn test_occurs_check_through_chain() {
        let mut subst = Substitution::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);

        // Create chain: v1 -> v0
        subst.insert(v1, InferType::Var(v0));

        // v0 occurs in v1 (through substitution)
        assert!(subst.occurs_in(v0, &InferType::Var(v1)));
    }

    #[test]
    fn test_unify_same_concrete() {
        let mut unifier = Unifier::new();
        let result = unifier.unify(
            &InferType::Concrete(Type::I32),
            &InferType::Concrete(Type::I32),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_unify_different_concrete() {
        let mut unifier = Unifier::new();
        let result = unifier.unify(
            &InferType::Concrete(Type::I32),
            &InferType::Concrete(Type::I64),
        );
        assert!(!result.is_ok());
        assert!(matches!(result, UnifyResult::TypeMismatch { .. }));
    }

    #[test]
    fn test_unify_var_with_concrete() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);

        let result = unifier.unify(&InferType::Var(v0), &InferType::Concrete(Type::Bool));
        assert!(result.is_ok());

        // Variable should now be bound to Bool
        assert_eq!(
            unifier.substitution.apply(&InferType::Var(v0)),
            InferType::Concrete(Type::Bool)
        );
    }

    #[test]
    fn test_unify_int_literal_with_integer() {
        let mut unifier = Unifier::new();
        let result = unifier.unify(&InferType::IntLiteral, &InferType::Concrete(Type::I64));
        assert!(result.is_ok());
    }

    #[test]
    fn test_unify_int_literal_with_non_integer() {
        let mut unifier = Unifier::new();
        let result = unifier.unify(&InferType::IntLiteral, &InferType::Concrete(Type::Bool));
        assert!(!result.is_ok());
        assert!(matches!(result, UnifyResult::IntLiteralNonInteger { .. }));
    }

    #[test]
    fn test_unify_two_int_literals() {
        let mut unifier = Unifier::new();
        let result = unifier.unify(&InferType::IntLiteral, &InferType::IntLiteral);
        assert!(result.is_ok());
    }

    #[test]
    fn test_unify_never_coerces() {
        let mut unifier = Unifier::new();
        let result = unifier.unify(
            &InferType::Concrete(Type::Never),
            &InferType::Concrete(Type::I32),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_unify_error_coerces() {
        let mut unifier = Unifier::new();
        let result = unifier.unify(
            &InferType::Concrete(Type::Error),
            &InferType::Concrete(Type::Bool),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_unify_two_vars() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);

        let result = unifier.unify(&InferType::Var(v0), &InferType::Var(v1));
        assert!(result.is_ok());

        // One should be bound to the other
        // (implementation detail: v0 gets bound to v1)
    }

    #[test]
    fn test_unify_chain_resolution() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);

        // v0 = v1
        let result = unifier.unify(&InferType::Var(v0), &InferType::Var(v1));
        assert!(result.is_ok());

        // v1 = i64
        let result = unifier.unify(&InferType::Var(v1), &InferType::Concrete(Type::I64));
        assert!(result.is_ok());

        // v0 should now resolve to i64
        assert_eq!(
            unifier.substitution.apply(&InferType::Var(v0)),
            InferType::Concrete(Type::I64)
        );
    }

    #[test]
    fn test_resolve_concrete() {
        let unifier = Unifier::new();
        let ty = InferType::Concrete(Type::Bool);
        assert_eq!(unifier.resolve(&ty), Some(Type::Bool));
    }

    #[test]
    fn test_resolve_int_literal_defaults_to_i32() {
        let unifier = Unifier::new();
        let ty = InferType::IntLiteral;
        assert_eq!(unifier.resolve(&ty), Some(Type::I32));
    }

    #[test]
    fn test_resolve_unbound_var_returns_none() {
        let unifier = Unifier::new();
        let ty = InferType::Var(TypeVarId::new(0));
        assert_eq!(unifier.resolve(&ty), None);
    }

    #[test]
    fn test_resolve_bound_var() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        unifier
            .substitution
            .insert(v0, InferType::Concrete(Type::U8));

        assert_eq!(unifier.resolve(&InferType::Var(v0)), Some(Type::U8));
    }

    #[test]
    fn test_check_signed_with_signed_type() {
        let unifier = Unifier::new();
        assert!(
            unifier
                .check_signed(&InferType::Concrete(Type::I32))
                .is_ok()
        );
        assert!(
            unifier
                .check_signed(&InferType::Concrete(Type::I64))
                .is_ok()
        );
    }

    #[test]
    fn test_check_signed_with_unsigned_type() {
        let unifier = Unifier::new();
        let result = unifier.check_signed(&InferType::Concrete(Type::U32));
        assert!(!result.is_ok());
        assert!(matches!(result, UnifyResult::NotSigned { ty: Type::U32 }));
    }

    #[test]
    fn test_check_signed_with_int_literal() {
        let unifier = Unifier::new();
        // IntLiteral defaults to i32 which is signed, so this is OK
        assert!(unifier.check_signed(&InferType::IntLiteral).is_ok());
    }

    #[test]
    fn test_constraint_creation() {
        let span = Span::new(10, 20);
        let c1 = Constraint::equal(InferType::Concrete(Type::I32), InferType::IntLiteral, span);
        let c2 = Constraint::is_signed(InferType::Var(TypeVarId::new(0)), span);

        assert_eq!(c1.span(), span);
        assert_eq!(c2.span(), span);
    }

    #[test]
    fn test_unify_var_with_itself() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);

        // Unifying a variable with itself should succeed (no-op)
        let result = unifier.unify(&InferType::Var(v0), &InferType::Var(v0));
        assert!(result.is_ok());
    }

    #[test]
    fn test_occurs_check_prevents_infinite_type() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);

        // Bind v1 to v0
        unifier.substitution.insert(v1, InferType::Var(v0));

        // Now try to bind v0 to v1 - this would create a cycle
        let result = unifier.bind(v0, &InferType::Var(v1));
        assert!(matches!(result, UnifyResult::OccursCheck { .. }));
    }

    // ========================================================================
    // Constraint Generation Tests (Phase 2)
    // ========================================================================

    use rue_intern::Interner;

    /// Helper to create a minimal RIR for testing.
    fn make_test_rir_and_interner() -> (Rir, Interner) {
        let rir = Rir::new();
        let interner = Interner::new();
        (rir, interner)
    }

    #[test]
    fn test_constraint_generator_int_literal() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        // Add an integer constant to RIR
        let inst_ref = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
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
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        let inst_ref = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Bool);

        let info = cgen.generate(inst_ref, &mut ctx);

        assert_eq!(info.ty, InferType::Concrete(Type::Bool));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_binary_add() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

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

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(add, &mut ctx);

        // Result should be a type variable
        assert!(info.ty.is_var());
        // Should generate 2 constraints: lhs = result, rhs = result
        assert_eq!(cgen.constraints().len(), 2);
    }

    #[test]
    fn test_constraint_generator_comparison() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

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

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Bool);

        let info = cgen.generate(lt, &mut ctx);

        // Comparisons always return Bool
        assert_eq!(info.ty, InferType::Concrete(Type::Bool));
        // Should generate 1 constraint: lhs type = rhs type
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_generator_logical_and() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

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

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Bool);

        let info = cgen.generate(and, &mut ctx);

        // Logical operators return Bool
        assert_eq!(info.ty, InferType::Concrete(Type::Bool));
        // Should generate 2 constraints: lhs = bool, rhs = bool
        assert_eq!(cgen.constraints().len(), 2);
    }

    #[test]
    fn test_constraint_generator_negation() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        // Create: -42
        let operand = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(1, 3),
        });
        let neg = rir.add_inst(rue_rir::Inst {
            data: InstData::Neg { operand },
            span: Span::new(0, 3),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
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
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        // Create: return 42
        let value = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(7, 9),
        });
        let ret = rir.add_inst(rue_rir::Inst {
            data: InstData::Ret(Some(value)),
            span: Span::new(0, 9),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(ret, &mut ctx);

        // Return is divergent (Never type)
        assert_eq!(info.ty, InferType::Concrete(Type::Never));
        // Should generate 1 constraint: return value = return type
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_generator_if_else() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

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

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
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
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

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

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(loop_inst, &mut ctx);

        // While loops produce Unit
        assert_eq!(info.ty, InferType::Concrete(Type::Unit));
        // Should generate 1 constraint: cond = bool
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_context_scope() {
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        // Use an interner to create a symbol
        let mut interner = Interner::new();
        let sym = interner.intern("x");
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

    #[test]
    fn test_function_sig() {
        let sig = FunctionSig {
            param_types: vec![
                InferType::Concrete(Type::I32),
                InferType::Concrete(Type::Bool),
            ],
            return_type: InferType::Concrete(Type::I64),
        };
        assert_eq!(sig.param_types.len(), 2);
        assert_eq!(sig.return_type, InferType::Concrete(Type::I64));
    }

    #[test]
    fn test_constraint_generator_infinite_loop() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        // Create: loop { 0 }
        let body = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(6, 7),
        });
        let loop_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::InfiniteLoop { body },
            span: Span::new(0, 10),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(loop_inst, &mut ctx);

        // Infinite loop produces Never (diverges)
        assert_eq!(info.ty, InferType::Concrete(Type::Never));
        // No constraints for infinite loop itself
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_break_continue() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        let break_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::Break,
            span: Span::new(0, 5),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(break_inst, &mut ctx);

        // Break diverges
        assert_eq!(info.ty, InferType::Concrete(Type::Never));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_index_get() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

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

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(index_get, &mut ctx);

        // Result is a type variable (element type unknown)
        assert!(info.ty.is_var());
        // Should generate 1 constraint: index must be unsigned
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::IsUnsigned(_, _) => {}
            _ => panic!("Expected IsUnsigned constraint for index"),
        }
    }

    #[test]
    fn test_constraint_generator_index_set() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

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

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(index_set, &mut ctx);

        // Index assignment produces Unit
        assert_eq!(info.ty, InferType::Concrete(Type::Unit));
        // Should generate 1 constraint: index must be unsigned
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::IsUnsigned(_, _) => {}
            _ => panic!("Expected IsUnsigned constraint for index"),
        }
    }

    #[test]
    fn test_constraint_generator_empty_block() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        // Create: { } (empty block)
        let block = rir.add_inst(rue_rir::Inst {
            data: InstData::Block {
                extra_start: 0,
                len: 0,
            },
            span: Span::new(0, 2),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(block, &mut ctx);

        // Empty block produces Unit
        assert_eq!(info.ty, InferType::Concrete(Type::Unit));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_bitwise_not() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        // Create: !42 (bitwise NOT)
        let operand = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(1, 3),
        });
        let bitnot = rir.add_inst(rue_rir::Inst {
            data: InstData::BitNot { operand },
            span: Span::new(0, 3),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
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
        let (mut rir, mut interner) = make_test_rir_and_interner();
        let mut functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        // Register a function that takes 2 parameters
        let func_name = interner.intern("foo");
        functions.insert(
            func_name,
            FunctionSig {
                param_types: vec![
                    InferType::Concrete(Type::I32),
                    InferType::Concrete(Type::I32),
                ],
                return_type: InferType::Concrete(Type::Bool),
            },
        );

        // Create a call with only 1 argument (mismatch)
        let arg = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(4, 6),
        });
        let call = rir.add_inst(rue_rir::Inst {
            data: InstData::Call {
                name: func_name,
                args: vec![arg],
            },
            span: Span::new(0, 7),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Bool);

        let info = cgen.generate(call, &mut ctx);

        // Should still return the declared return type
        assert_eq!(info.ty, InferType::Concrete(Type::Bool));
        // No constraints generated when arg count mismatches (error will be in sema)
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_unknown_function() {
        let (mut rir, mut interner) = make_test_rir_and_interner();
        let functions = HashMap::new(); // Empty - no functions registered
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

        // Create a call to an unknown function
        let unknown_func = interner.intern("unknown");
        let arg = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(8, 10),
        });
        let call = rir.add_inst(rue_rir::Inst {
            data: InstData::Call {
                name: unknown_func,
                args: vec![arg],
            },
            span: Span::new(0, 11),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(call, &mut ctx);

        // Unknown function returns Error type
        assert_eq!(info.ty, InferType::Concrete(Type::Error));
        // Arguments should still be processed (but no constraints generated for them)
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_match_multiple_arms() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Symbol, Symbol), MethodSig> = HashMap::new();

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
        let pattern1 = rue_rir::RirPattern::Int(1, Span::new(10, 11));

        // Arm 2: 2 => 20
        let body2 = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(20),
            span: Span::new(25, 27),
        });
        let pattern2 = rue_rir::RirPattern::Int(2, Span::new(20, 21));

        // Arm 3: _ => 30
        let body3 = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(30),
            span: Span::new(35, 37),
        });
        let pattern3 = rue_rir::RirPattern::Wildcard(Span::new(30, 31));

        let match_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::Match {
                scrutinee,
                arms: vec![(pattern1, body1), (pattern2, body2), (pattern3, body3)],
            },
            span: Span::new(0, 40),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
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

    // ========================================================================
    // Phase 3: Unification Tests
    // ========================================================================

    #[test]
    fn test_check_integer_with_integer_types() {
        let unifier = Unifier::new();
        assert!(
            unifier
                .check_integer(&InferType::Concrete(Type::I32))
                .is_ok()
        );
        assert!(
            unifier
                .check_integer(&InferType::Concrete(Type::I64))
                .is_ok()
        );
        assert!(
            unifier
                .check_integer(&InferType::Concrete(Type::U8))
                .is_ok()
        );
        assert!(
            unifier
                .check_integer(&InferType::Concrete(Type::U64))
                .is_ok()
        );
    }

    #[test]
    fn test_check_integer_with_non_integer_type() {
        let unifier = Unifier::new();
        let result = unifier.check_integer(&InferType::Concrete(Type::Bool));
        assert!(!result.is_ok());
        assert!(matches!(result, UnifyResult::NotInteger { ty: Type::Bool }));
    }

    #[test]
    fn test_check_integer_with_int_literal() {
        let unifier = Unifier::new();
        // IntLiteral is OK - it will become an integer type
        assert!(unifier.check_integer(&InferType::IntLiteral).is_ok());
    }

    #[test]
    fn test_check_integer_with_type_variable() {
        let unifier = Unifier::new();
        // Type variable is OK - it might become an integer type
        assert!(
            unifier
                .check_integer(&InferType::Var(TypeVarId::new(0)))
                .is_ok()
        );
    }

    #[test]
    fn test_check_integer_with_error_type() {
        let unifier = Unifier::new();
        // Error type is OK - for error recovery
        assert!(
            unifier
                .check_integer(&InferType::Concrete(Type::Error))
                .is_ok()
        );
    }

    #[test]
    fn test_solve_constraints_empty() {
        let mut unifier = Unifier::new();
        let errors = unifier.solve_constraints(&[]);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_solve_constraints_simple_equal() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let constraints = vec![Constraint::equal(
            InferType::Var(v0),
            InferType::Concrete(Type::I64),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
        assert_eq!(unifier.resolve(&InferType::Var(v0)), Some(Type::I64));
    }

    #[test]
    fn test_solve_constraints_multiple_equal() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);
        let constraints = vec![
            Constraint::equal(InferType::Var(v0), InferType::Var(v1), Span::new(0, 5)),
            Constraint::equal(
                InferType::Var(v1),
                InferType::Concrete(Type::Bool),
                Span::new(6, 10),
            ),
        ];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
        // Both variables should resolve to Bool
        assert_eq!(unifier.resolve(&InferType::Var(v0)), Some(Type::Bool));
        assert_eq!(unifier.resolve(&InferType::Var(v1)), Some(Type::Bool));
    }

    #[test]
    fn test_solve_constraints_type_mismatch() {
        let mut unifier = Unifier::new();
        let constraints = vec![Constraint::equal(
            InferType::Concrete(Type::I32),
            InferType::Concrete(Type::Bool),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0].kind, UnifyResult::TypeMismatch { .. }));
        assert_eq!(errors[0].span, Span::new(0, 5));
    }

    #[test]
    fn test_solve_constraints_int_literal_unifies() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let constraints = vec![
            Constraint::equal(InferType::Var(v0), InferType::IntLiteral, Span::new(0, 5)),
            Constraint::equal(
                InferType::Var(v0),
                InferType::Concrete(Type::I64),
                Span::new(6, 10),
            ),
        ];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
        // Should resolve to i64 (IntLiteral takes the concrete integer type)
        assert_eq!(unifier.resolve(&InferType::Var(v0)), Some(Type::I64));
    }

    #[test]
    fn test_solve_constraints_is_signed() {
        let mut unifier = Unifier::new();
        let constraints = vec![Constraint::is_signed(
            InferType::Concrete(Type::I32),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_solve_constraints_is_signed_fails_for_unsigned() {
        let mut unifier = Unifier::new();
        let constraints = vec![Constraint::is_signed(
            InferType::Concrete(Type::U32),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind,
            UnifyResult::NotSigned { ty: Type::U32 }
        ));
    }

    #[test]
    fn test_solve_constraints_is_integer() {
        let mut unifier = Unifier::new();
        let constraints = vec![Constraint::is_integer(
            InferType::Concrete(Type::U8),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_solve_constraints_is_integer_fails_for_bool() {
        let mut unifier = Unifier::new();
        let constraints = vec![Constraint::is_integer(
            InferType::Concrete(Type::Bool),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind,
            UnifyResult::NotInteger { ty: Type::Bool }
        ));
    }

    #[test]
    fn test_solve_constraints_multiple_errors() {
        let mut unifier = Unifier::new();
        let constraints = vec![
            Constraint::equal(
                InferType::Concrete(Type::I32),
                InferType::Concrete(Type::Bool),
                Span::new(0, 5),
            ),
            Constraint::is_signed(InferType::Concrete(Type::U64), Span::new(10, 15)),
        ];
        let errors = unifier.solve_constraints(&constraints);
        // Should catch both errors in one pass
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn test_solve_constraints_error_recovery() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let constraints = vec![
            // This will fail: can't unify type variable with mismatched concretes
            Constraint::equal(
                InferType::Var(v0),
                InferType::Concrete(Type::I32),
                Span::new(0, 5),
            ),
            Constraint::equal(
                InferType::Var(v0),
                InferType::Concrete(Type::Bool),
                Span::new(6, 10),
            ),
        ];
        let errors = unifier.solve_constraints(&constraints);
        // Should report the second constraint as an error (first succeeds)
        assert_eq!(errors.len(), 1);
        // v0 should still be usable (bound to i32 from first constraint)
        assert_eq!(unifier.resolve(&InferType::Var(v0)), Some(Type::I32));
    }

    #[test]
    fn test_default_int_literal_vars() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);
        let v2 = TypeVarId::new(2);

        // v0 and v1 are int literal vars (unbound)
        // v2 is bound to Bool
        unifier
            .substitution
            .insert(v2, InferType::Concrete(Type::Bool));

        // Track v0 and v1 as int literal vars
        let int_literal_vars = vec![v0, v1];

        unifier.default_int_literal_vars(&int_literal_vars);

        // v0 and v1 should now be i32
        assert_eq!(unifier.resolve(&InferType::Var(v0)), Some(Type::I32));
        assert_eq!(unifier.resolve(&InferType::Var(v1)), Some(Type::I32));
        // v2 should still be Bool
        assert_eq!(unifier.resolve(&InferType::Var(v2)), Some(Type::Bool));
    }

    #[test]
    fn test_resolve_or_error_with_bound_var() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        unifier
            .substitution
            .insert(v0, InferType::Concrete(Type::U8));
        assert_eq!(unifier.resolve_or_error(&InferType::Var(v0)), Type::U8);
    }

    #[test]
    fn test_resolve_or_error_with_unbound_var() {
        let unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        // Unbound variable should return Error
        assert_eq!(unifier.resolve_or_error(&InferType::Var(v0)), Type::Error);
    }

    #[test]
    fn test_resolve_or_error_with_int_literal() {
        let unifier = Unifier::new();
        // IntLiteral defaults to i32
        assert_eq!(unifier.resolve_or_error(&InferType::IntLiteral), Type::I32);
    }

    #[test]
    fn test_unification_error_message() {
        let error = UnificationError::new(
            UnifyResult::TypeMismatch {
                expected: InferType::Concrete(Type::I32),
                found: InferType::Concrete(Type::Bool),
            },
            Span::new(10, 20),
        );
        let msg = error.message();
        assert!(msg.contains("type mismatch"));
        assert!(msg.contains("i32"));
        assert!(msg.contains("bool"));
    }

    #[test]
    fn test_unification_error_int_literal_non_integer_message() {
        let error = UnificationError::new(
            UnifyResult::IntLiteralNonInteger { found: Type::Bool },
            Span::new(0, 5),
        );
        let msg = error.message();
        assert!(msg.contains("integer literal"));
        assert!(msg.contains("bool"));
    }

    #[test]
    fn test_unification_error_not_signed_message() {
        let error =
            UnificationError::new(UnifyResult::NotSigned { ty: Type::U32 }, Span::new(0, 5));
        let msg = error.message();
        assert!(msg.contains("negate"));
        assert!(msg.contains("u32"));
    }

    #[test]
    fn test_unification_error_not_integer_message() {
        let error =
            UnificationError::new(UnifyResult::NotInteger { ty: Type::Bool }, Span::new(0, 5));
        let msg = error.message();
        assert!(msg.contains("expected integer"));
        assert!(msg.contains("bool"));
    }

    #[test]
    fn test_solve_constraints_int_literal_with_non_integer() {
        let mut unifier = Unifier::new();
        let constraints = vec![Constraint::equal(
            InferType::IntLiteral,
            InferType::Concrete(Type::Bool),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind,
            UnifyResult::IntLiteralNonInteger { found: Type::Bool }
        ));
    }

    #[test]
    fn test_solve_constraints_chain_resolution() {
        // Test: v0 = v1, v1 = v2, v2 = i64
        // All should resolve to i64
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);
        let v2 = TypeVarId::new(2);
        let constraints = vec![
            Constraint::equal(InferType::Var(v0), InferType::Var(v1), Span::new(0, 5)),
            Constraint::equal(InferType::Var(v1), InferType::Var(v2), Span::new(6, 10)),
            Constraint::equal(
                InferType::Var(v2),
                InferType::Concrete(Type::I64),
                Span::new(11, 15),
            ),
        ];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
        assert_eq!(unifier.resolve(&InferType::Var(v0)), Some(Type::I64));
        assert_eq!(unifier.resolve(&InferType::Var(v1)), Some(Type::I64));
        assert_eq!(unifier.resolve(&InferType::Var(v2)), Some(Type::I64));
    }

    #[test]
    fn test_solve_constraints_reverse_chain() {
        // Test: v2 = i64, v1 = v2, v0 = v1
        // Should work in any order
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);
        let v2 = TypeVarId::new(2);
        let constraints = vec![
            Constraint::equal(
                InferType::Var(v2),
                InferType::Concrete(Type::I64),
                Span::new(0, 5),
            ),
            Constraint::equal(InferType::Var(v1), InferType::Var(v2), Span::new(6, 10)),
            Constraint::equal(InferType::Var(v0), InferType::Var(v1), Span::new(11, 15)),
        ];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
        assert_eq!(unifier.resolve(&InferType::Var(v0)), Some(Type::I64));
    }

    #[test]
    fn test_solve_constraints_two_int_literal_vars() {
        // Two int literal vars that are constrained equal.
        // After defaulting, both should become i32.
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);

        // Both are int literal vars, constrained to be equal
        let constraints = vec![Constraint::equal(
            InferType::Var(v0),
            InferType::Var(v1),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());

        // Both are int literal vars that should be defaulted to i32
        let int_literal_vars = vec![v0, v1];
        unifier.default_int_literal_vars(&int_literal_vars);
        assert_eq!(unifier.resolve(&InferType::Var(v0)), Some(Type::I32));
        assert_eq!(unifier.resolve(&InferType::Var(v1)), Some(Type::I32));
    }

    #[test]
    fn test_solve_constraints_is_signed_with_resolved_var() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let constraints = vec![
            // First bind v0 to u32
            Constraint::equal(
                InferType::Var(v0),
                InferType::Concrete(Type::U32),
                Span::new(0, 5),
            ),
            // Then check if v0 is signed (should fail)
            Constraint::is_signed(InferType::Var(v0), Span::new(6, 10)),
        ];
        let errors = unifier.solve_constraints(&constraints);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind,
            UnifyResult::NotSigned { ty: Type::U32 }
        ));
    }

    #[test]
    fn test_solve_constraints_is_integer_with_resolved_var() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let constraints = vec![
            // First bind v0 to bool
            Constraint::equal(
                InferType::Var(v0),
                InferType::Concrete(Type::Bool),
                Span::new(0, 5),
            ),
            // Then check if v0 is integer (should fail)
            Constraint::is_integer(InferType::Var(v0), Span::new(6, 10)),
        ];
        let errors = unifier.solve_constraints(&constraints);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind,
            UnifyResult::NotInteger { ty: Type::Bool }
        ));
    }

    #[test]
    fn test_solve_constraints_is_unsigned_binds_literal_to_u64() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        // Simulate what happens for an integer literal used as an array index
        // when the variable is bound to IntLiteral
        unifier.substitution.insert(v0, InferType::IntLiteral);
        let constraints = vec![Constraint::is_unsigned(InferType::Var(v0), Span::new(0, 5))];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty(), "IsUnsigned on IntLiteral should succeed");
        // The literal should now be bound to U64
        assert_eq!(
            unifier.resolve(&InferType::Var(v0)),
            Some(Type::U64),
            "IntLiteral should be bound to U64 after IsUnsigned constraint"
        );
    }

    #[test]
    fn test_solve_constraints_is_unsigned_binds_unbound_var_to_u64() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        // Simulate what happens for an integer literal used as an array index
        // when the variable is still unbound (this is the real scenario)
        // v0 is NOT bound to anything initially
        let constraints = vec![Constraint::is_unsigned(InferType::Var(v0), Span::new(0, 5))];
        let errors = unifier.solve_constraints(&constraints);
        assert!(
            errors.is_empty(),
            "IsUnsigned on unbound var should succeed"
        );
        // The unbound variable should now be bound to U64
        assert_eq!(
            unifier.resolve(&InferType::Var(v0)),
            Some(Type::U64),
            "Unbound var should be bound to U64 after IsUnsigned constraint"
        );
    }

    #[test]
    fn test_solve_constraints_never_type_coerces() {
        let mut unifier = Unifier::new();
        let constraints = vec![
            Constraint::equal(
                InferType::Concrete(Type::Never),
                InferType::Concrete(Type::I32),
                Span::new(0, 5),
            ),
            Constraint::equal(
                InferType::Concrete(Type::Bool),
                InferType::Concrete(Type::Never),
                Span::new(6, 10),
            ),
        ];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_solve_constraints_error_type_coerces() {
        let mut unifier = Unifier::new();
        let constraints = vec![
            Constraint::equal(
                InferType::Concrete(Type::Error),
                InferType::Concrete(Type::I32),
                Span::new(0, 5),
            ),
            Constraint::equal(
                InferType::Concrete(Type::Bool),
                InferType::Concrete(Type::Error),
                Span::new(6, 10),
            ),
        ];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_int_literal_with_error_type() {
        let mut unifier = Unifier::new();
        // IntLiteral should unify with Error (for error recovery)
        let constraints = vec![Constraint::equal(
            InferType::IntLiteral,
            InferType::Concrete(Type::Error),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
    }
}
