//! Unification engine for Hindley-Milner type inference.
//!
//! This module provides the core unification algorithm:
//! - [`UnifyResult`] - Result of a unification attempt
//! - [`UnificationError`] - Error with span for reporting
//! - [`Unifier`] - The unification engine

use super::constraint::{Constraint, Substitution};
use super::types::{InferType, TypeVarId};
use crate::Type;
use rue_span::Span;

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

    /// Create a new unifier with a pre-sized substitution.
    ///
    /// Use this when you know how many type variables will be created
    /// (e.g., from `TypeVarAllocator::count()`).
    pub fn with_capacity(type_var_count: u32) -> Self {
        Unifier {
            substitution: Substitution::with_capacity(type_var_count as usize),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(unifier.resolve(&InferType::Var(v1)), Some(Type::I64));
        assert_eq!(unifier.resolve(&InferType::Var(v2)), Some(Type::I64));
    }

    // =========================================================================
    // Additional edge case tests
    // =========================================================================

    #[test]
    fn test_check_unsigned_with_unsigned_types() {
        let unifier = Unifier::new();
        assert!(
            unifier
                .check_unsigned(&InferType::Concrete(Type::U8))
                .is_ok()
        );
        assert!(
            unifier
                .check_unsigned(&InferType::Concrete(Type::U16))
                .is_ok()
        );
        assert!(
            unifier
                .check_unsigned(&InferType::Concrete(Type::U32))
                .is_ok()
        );
        assert!(
            unifier
                .check_unsigned(&InferType::Concrete(Type::U64))
                .is_ok()
        );
    }

    #[test]
    fn test_check_unsigned_with_signed_type() {
        let unifier = Unifier::new();
        let result = unifier.check_unsigned(&InferType::Concrete(Type::I32));
        assert!(!result.is_ok());
        assert!(matches!(result, UnifyResult::NotUnsigned { ty: Type::I32 }));
    }

    #[test]
    fn test_check_unsigned_with_int_literal() {
        let unifier = Unifier::new();
        // IntLiteral is OK - it will become an unsigned type if constrained
        assert!(unifier.check_unsigned(&InferType::IntLiteral).is_ok());
    }

    #[test]
    fn test_check_unsigned_with_non_integer_type() {
        let unifier = Unifier::new();
        let result = unifier.check_unsigned(&InferType::Concrete(Type::Bool));
        assert!(!result.is_ok());
        assert!(matches!(
            result,
            UnifyResult::NotUnsigned { ty: Type::Bool }
        ));
    }

    #[test]
    fn test_solve_constraints_is_unsigned() {
        let mut unifier = Unifier::new();
        let constraints = vec![Constraint::is_unsigned(
            InferType::Concrete(Type::U64),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_solve_constraints_is_unsigned_fails_for_signed() {
        let mut unifier = Unifier::new();
        let constraints = vec![Constraint::is_unsigned(
            InferType::Concrete(Type::I32),
            Span::new(0, 5),
        )];
        let errors = unifier.solve_constraints(&constraints);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind,
            UnifyResult::NotUnsigned { ty: Type::I32 }
        ));
    }

    #[test]
    fn test_unify_array_same_element_same_length() {
        let mut unifier = Unifier::new();
        let arr1 = InferType::Array {
            element: Box::new(InferType::Concrete(Type::I32)),
            length: 5,
        };
        let arr2 = InferType::Array {
            element: Box::new(InferType::Concrete(Type::I32)),
            length: 5,
        };
        let result = unifier.unify(&arr1, &arr2);
        assert!(result.is_ok());
    }

    #[test]
    fn test_unify_array_different_length() {
        let mut unifier = Unifier::new();
        let arr1 = InferType::Array {
            element: Box::new(InferType::Concrete(Type::I32)),
            length: 3,
        };
        let arr2 = InferType::Array {
            element: Box::new(InferType::Concrete(Type::I32)),
            length: 5,
        };
        let result = unifier.unify(&arr1, &arr2);
        assert!(!result.is_ok());
        assert!(matches!(result, UnifyResult::ArrayLengthMismatch { .. }));
    }

    #[test]
    fn test_unify_array_different_element_type() {
        let mut unifier = Unifier::new();
        let arr1 = InferType::Array {
            element: Box::new(InferType::Concrete(Type::I32)),
            length: 3,
        };
        let arr2 = InferType::Array {
            element: Box::new(InferType::Concrete(Type::Bool)),
            length: 3,
        };
        let result = unifier.unify(&arr1, &arr2);
        assert!(!result.is_ok());
        assert!(matches!(result, UnifyResult::TypeMismatch { .. }));
    }

    #[test]
    fn test_unify_array_with_variable_element() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);
        let arr1 = InferType::Array {
            element: Box::new(InferType::Var(v0)),
            length: 3,
        };
        let arr2 = InferType::Array {
            element: Box::new(InferType::Concrete(Type::I64)),
            length: 3,
        };
        let result = unifier.unify(&arr1, &arr2);
        assert!(result.is_ok());

        // Variable should now be bound to I64
        assert_eq!(
            unifier.substitution.apply(&InferType::Var(v0)),
            InferType::Concrete(Type::I64)
        );
    }

    #[test]
    fn test_unify_array_with_int_literal_element() {
        let mut unifier = Unifier::new();
        let arr1 = InferType::Array {
            element: Box::new(InferType::IntLiteral),
            length: 3,
        };
        let arr2 = InferType::Array {
            element: Box::new(InferType::Concrete(Type::I32)),
            length: 3,
        };
        let result = unifier.unify(&arr1, &arr2);
        assert!(result.is_ok());
    }

    #[test]
    fn test_unify_array_with_concrete() {
        let mut unifier = Unifier::new();
        let arr = InferType::Array {
            element: Box::new(InferType::Concrete(Type::I32)),
            length: 3,
        };
        let concrete = InferType::Concrete(Type::I32);
        let result = unifier.unify(&arr, &concrete);
        assert!(!result.is_ok());
        assert!(matches!(result, UnifyResult::TypeMismatch { .. }));
    }

    #[test]
    fn test_resolve_array_type() {
        let mut unifier = Unifier::new();
        let v0 = TypeVarId::new(0);

        // Bind v0 to array of I32
        unifier.substitution.insert(
            v0,
            InferType::Array {
                element: Box::new(InferType::Concrete(Type::I32)),
                length: 10,
            },
        );

        // resolve() returns None for arrays (they need special handling)
        let resolved = unifier.resolve(&InferType::Var(v0));
        assert!(resolved.is_none());

        // resolve_infer_type() should properly resolve arrays
        let resolved_infer = unifier.resolve_infer_type(&InferType::Var(v0));
        match resolved_infer {
            InferType::Array { element, length } => {
                assert_eq!(*element, InferType::Concrete(Type::I32));
                assert_eq!(length, 10);
            }
            _ => panic!("Expected InferType::Array, got {:?}", resolved_infer),
        }
    }

    #[test]
    fn test_unification_error_not_unsigned_message() {
        let error =
            UnificationError::new(UnifyResult::NotUnsigned { ty: Type::I32 }, Span::new(0, 5));
        let msg = error.message();
        assert!(msg.contains("unsigned"));
        assert!(msg.contains("i32"));
    }

    #[test]
    fn test_unification_error_array_length_mismatch_message() {
        let error = UnificationError::new(
            UnifyResult::ArrayLengthMismatch {
                expected: 3,
                found: 5,
            },
            Span::new(0, 10),
        );
        let msg = error.message();
        assert!(msg.contains("3"));
        assert!(msg.contains("5"));
    }

    #[test]
    fn test_unification_error_occurs_check_message() {
        let error = UnificationError::new(
            UnifyResult::OccursCheck {
                var: TypeVarId::new(0),
                ty: InferType::Array {
                    element: Box::new(InferType::Var(TypeVarId::new(0))),
                    length: 3,
                },
            },
            Span::new(0, 5),
        );
        let msg = error.message();
        assert!(msg.contains("infinite") || msg.contains("occurs"));
    }
}
