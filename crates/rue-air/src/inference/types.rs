//! Type variable infrastructure for Hindley-Milner type inference.
//!
//! This module provides the core type representations used during inference:
//! - [`TypeVarId`] - Unique identifier for type variables
//! - [`InferType`] - Type representation during inference (supports variables)
//! - [`TypeVarAllocator`] - Allocates fresh type variables

use crate::Type;

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
    /// Unlike `Concrete(Type::new_array(id))`, this stores the element type as an
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
}
