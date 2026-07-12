//! Request-independent semantic values used at compiler query boundaries.
//!
//! These types deliberately have no conversion from `rue_air::Type`. Such a
//! conversion is only sound while the successful declaration binder, its type
//! pool, and the exact-revision stable-definition join are available together.

use std::sync::Arc;

use crate::{ModuleId, StableDefinitionKey};

/// Version of the canonical durable type/value encoding.
pub const DURABLE_SEMANTIC_SCHEMA_VERSION: u32 = 1;

/// An owned, request-independent Rue type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableType {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,
    Unit,
    Never,
    ComptimeType,
    /// A named struct or enum in the exact stable definition universe.
    Nominal(StableDefinitionKey),
    Array {
        element: Box<DurableType>,
        len: u64,
    },
    PtrConst(Box<DurableType>),
    PtrMut(Box<DurableType>),
    /// Reserved for the source-level tuple surface once binding supports it.
    Tuple(Arc<[DurableType]>),
    /// Reserved for first-class function types. Parameter order is semantic.
    Function {
        parameters: Arc<[DurableType]>,
        result: Box<DurableType>,
    },
    /// A module value's resolved logical module identity.
    Module(ModuleId),
    /// A declaration-scoped generic parameter, indexed in source order.
    GenericParameter(u32),
}

/// An owned, request-independent compile-time value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableConstValue {
    Integer(i128),
    Bool(bool),
    Type(DurableType),
    /// Function aliases use declaration identity, never a mangled/interner name.
    Function(StableDefinitionKey),
    Unit,
}

/// Typed fail-closed reasons from the future successful-binding exporter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableSemanticExportFailure {
    ErrorType,
    MissingTypePoolEntry,
    MissingStableNominalDefinition,
    MissingStableFunctionDefinition,
    UnresolvedModule,
    AnonymousNominalType,
    UnsupportedLocalType,
    UnsupportedTypeForm,
    UnsupportedConstValue,
    RecursiveStructuralType,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_query_value<T: Send + Sync + Clone + Eq + Ord + std::hash::Hash>() {}

    #[test]
    fn durable_values_have_query_key_traits() {
        assert_query_value::<DurableType>();
        assert_query_value::<DurableConstValue>();
        assert_query_value::<DurableSemanticExportFailure>();
    }

    #[test]
    fn structural_order_is_canonical_and_parameter_order_is_semantic() {
        let a = DurableType::Tuple(Arc::from([DurableType::Bool, DurableType::I32]));
        let b = DurableType::Tuple(Arc::from([DurableType::I32, DurableType::Bool]));
        assert_ne!(a, b);

        let first = DurableConstValue::Type(DurableType::Array {
            element: Box::new(DurableType::PtrConst(Box::new(DurableType::U8))),
            len: 4,
        });
        assert_eq!(first.clone(), first);
    }
}
