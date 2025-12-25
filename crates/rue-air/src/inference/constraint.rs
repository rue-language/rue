//! Constraint types and substitution for type inference.
//!
//! This module provides:
//! - [`Constraint`] - Type constraints generated during analysis
//! - [`Substitution`] - Mapping from type variables to resolved types

use super::types::{InferType, TypeVarId};
use rue_span::Span;
use std::collections::HashMap;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Type;

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
    fn test_constraint_creation() {
        let span = Span::new(10, 20);
        let c1 = Constraint::equal(InferType::Concrete(Type::I32), InferType::IntLiteral, span);
        let c2 = Constraint::is_signed(InferType::Var(TypeVarId::new(0)), span);

        assert_eq!(c1.span(), span);
        assert_eq!(c2.span(), span);
    }
}
