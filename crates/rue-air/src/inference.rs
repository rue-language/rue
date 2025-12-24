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

    /// Get the span for this constraint (for error reporting).
    pub fn span(&self) -> Span {
        match self {
            Constraint::Equal(_, _, span) | Constraint::IsSigned(_, span) => *span,
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
}

impl UnifyResult {
    /// Check if unification succeeded.
    pub fn is_ok(&self) -> bool {
        matches!(self, UnifyResult::Ok)
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
    pub fn unify(&mut self, lhs: &InferType, rhs: &InferType) -> UnifyResult {
        // Apply current substitution to get most specific types
        let lhs = self.substitution.apply(lhs);
        let rhs = self.substitution.apply(rhs);

        match (&lhs, &rhs) {
            // Same concrete types unify
            (InferType::Concrete(t1), InferType::Concrete(t2)) => {
                if t1 == t2 || t1.can_coerce_to(t2) || t2.can_coerce_to(t1) {
                    UnifyResult::Ok
                } else {
                    UnifyResult::TypeMismatch {
                        expected: lhs,
                        found: rhs,
                    }
                }
            }

            // Variable on left: bind it to right (if occurs check passes)
            (InferType::Var(var), _) => self.bind(*var, &rhs),

            // Variable on right: bind it to left
            (_, InferType::Var(var)) => self.bind(*var, &lhs),

            // IntLiteral with concrete type
            (InferType::IntLiteral, InferType::Concrete(ty))
            | (InferType::Concrete(ty), InferType::IntLiteral) => {
                if ty.is_integer() {
                    // IntLiteral can become any integer type
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
        }
    }

    /// Default any remaining IntLiteral types to i32.
    ///
    /// Called at the end of unification for a function to resolve
    /// any unconstrained integer literals.
    pub fn default_int_literals(&mut self, var: TypeVarId) {
        if let Some(ty) = self.substitution.get(var) {
            if ty.is_int_literal() {
                self.substitution
                    .insert(var, InferType::Concrete(Type::I32));
            }
        }
    }

    /// Resolve a type to its final concrete type.
    ///
    /// Follows the substitution chain and defaults IntLiteral to i32.
    /// Returns `None` if the type resolves to an unbound type variable.
    pub fn resolve(&self, ty: &InferType) -> Option<Type> {
        let resolved = self.substitution.apply(ty);
        match resolved {
            InferType::Concrete(t) => Some(t),
            InferType::Var(_) => None,                // Unbound variable
            InferType::IntLiteral => Some(Type::I32), // Default to i32
        }
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
}
