//! Constraint types and substitution for type inference.
//!
//! This module provides:
//! - [`Constraint`] - Type constraints generated during analysis
//! - [`Substitution`] - Mapping from type variables to resolved types

use super::types::{InferType, TypeVarId};
use rue_span::Span;
use std::cell::RefCell;

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
///
/// # Performance
///
/// Uses a `Vec<Option<InferType>>` instead of `HashMap<TypeVarId, InferType>` for O(1)
/// lookups without hashing overhead. This works because `TypeVarId` is a sequential
/// `u32` starting from 0.
///
/// Additionally implements path compression: when following a chain of variable
/// references, intermediate links are updated to point directly to the final result,
/// amortizing the cost of chain traversal.
#[derive(Debug, Default)]
pub struct Substitution {
    /// Mapping from type variable index to its resolved type.
    /// Uses `RefCell` to allow path compression during immutable lookups.
    mapping: RefCell<Vec<Option<InferType>>>,
}

impl Substitution {
    /// Create an empty substitution.
    pub fn new() -> Self {
        Substitution {
            mapping: RefCell::new(Vec::new()),
        }
    }

    /// Create a substitution with pre-allocated capacity.
    ///
    /// Use when you know approximately how many type variables will be created.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut mapping = Vec::with_capacity(capacity);
        // Pre-fill with None to allow direct indexing
        mapping.resize(capacity, None);
        Substitution {
            mapping: RefCell::new(mapping),
        }
    }

    /// Insert a mapping from a type variable to a type.
    ///
    /// If the variable is already mapped, the old mapping is replaced.
    pub fn insert(&mut self, var: TypeVarId, ty: InferType) {
        let idx = var.index() as usize;
        let mut mapping = self.mapping.borrow_mut();
        // Grow the vector if necessary
        if idx >= mapping.len() {
            mapping.resize(idx + 1, None);
        }
        mapping[idx] = Some(ty);
    }

    /// Look up a type variable's immediate mapping (without following chains).
    pub fn get(&self, var: TypeVarId) -> Option<InferType> {
        let idx = var.index() as usize;
        let mapping = self.mapping.borrow();
        if idx < mapping.len() {
            mapping[idx].clone()
        } else {
            None
        }
    }

    /// Apply the substitution to a type, following type variable chains
    /// to their ultimate resolution.
    ///
    /// - `Concrete(ty)` → `Concrete(ty)` (unchanged)
    /// - `Var(id)` → follows chain until concrete or unbound variable
    /// - `IntLiteral` → `IntLiteral` (unchanged, unless we add IntLiteral
    ///   to variable mappings)
    /// - `Array { element, length }` → recursively apply to element type
    ///
    /// # Path Compression
    ///
    /// When following a chain like `v0 -> v1 -> v2 -> i32`, this method
    /// updates all intermediate links to point directly to the final result.
    /// This amortizes the cost of repeated lookups.
    pub fn apply(&self, ty: &InferType) -> InferType {
        match ty {
            InferType::Concrete(_) => ty.clone(),
            InferType::Var(id) => self.apply_var(*id),
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

    /// Apply substitution to a type variable with path compression.
    fn apply_var(&self, id: TypeVarId) -> InferType {
        let idx = id.index() as usize;

        // First, get the resolved type without holding the borrow
        let resolved = {
            let mapping = self.mapping.borrow();
            if idx >= mapping.len() {
                return InferType::Var(id);
            }
            match &mapping[idx] {
                None => return InferType::Var(id),
                Some(ty) => ty.clone(),
            }
        };

        // Recursively resolve
        let final_type = self.apply(&resolved);

        // Path compression: if we followed a chain to reach a different result,
        // update this mapping to point directly to the final result.
        // This avoids traversing the same chain repeatedly.
        if final_type != resolved {
            let mut mapping = self.mapping.borrow_mut();
            if idx < mapping.len() {
                mapping[idx] = Some(final_type.clone());
            }
        }

        final_type
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
                match self.get(*id) {
                    Some(resolved) => self.occurs_in(var, &resolved),
                    None => false,
                }
            }
            InferType::IntLiteral => false,
            InferType::Array { element, .. } => self.occurs_in(var, element),
        }
    }

    /// Get the number of mappings in the substitution.
    ///
    /// Note: This counts all slots that have values, requiring a full scan.
    /// For performance-critical code, consider tracking this separately.
    pub fn len(&self) -> usize {
        self.mapping.borrow().iter().filter(|c| c.is_some()).count()
    }

    /// Check if the substitution is empty.
    pub fn is_empty(&self) -> bool {
        self.mapping.borrow().iter().all(|c| c.is_none())
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

    #[test]
    fn test_substitution_with_capacity() {
        let subst = Substitution::with_capacity(10);
        // Pre-allocated substitution should be empty (no mappings yet)
        assert!(subst.is_empty());
        assert_eq!(subst.len(), 0);
    }

    #[test]
    fn test_substitution_path_compression() {
        // Create a long chain: v0 -> v1 -> v2 -> v3 -> v4 -> i32
        let mut subst = Substitution::new();
        let v0 = TypeVarId::new(0);
        let v1 = TypeVarId::new(1);
        let v2 = TypeVarId::new(2);
        let v3 = TypeVarId::new(3);
        let v4 = TypeVarId::new(4);

        subst.insert(v0, InferType::Var(v1));
        subst.insert(v1, InferType::Var(v2));
        subst.insert(v2, InferType::Var(v3));
        subst.insert(v3, InferType::Var(v4));
        subst.insert(v4, InferType::Concrete(Type::I32));

        // First lookup should resolve the chain
        assert_eq!(
            subst.apply(&InferType::Var(v0)),
            InferType::Concrete(Type::I32)
        );

        // After path compression, all intermediate variables should point directly to i32
        // Verify by checking that v0 now directly points to i32
        let resolved = subst.get(v0);
        assert_eq!(resolved, Some(InferType::Concrete(Type::I32)));

        // v1, v2, v3 should also be compressed
        assert_eq!(subst.get(v1), Some(InferType::Concrete(Type::I32)));
        assert_eq!(subst.get(v2), Some(InferType::Concrete(Type::I32)));
        assert_eq!(subst.get(v3), Some(InferType::Concrete(Type::I32)));
    }

    #[test]
    fn test_substitution_len_and_is_empty() {
        let mut subst = Substitution::new();
        assert!(subst.is_empty());
        assert_eq!(subst.len(), 0);

        subst.insert(TypeVarId::new(0), InferType::Concrete(Type::I32));
        assert!(!subst.is_empty());
        assert_eq!(subst.len(), 1);

        // Insert at a higher index - only counts actual mappings
        subst.insert(TypeVarId::new(5), InferType::Concrete(Type::Bool));
        assert_eq!(subst.len(), 2);
    }
}
