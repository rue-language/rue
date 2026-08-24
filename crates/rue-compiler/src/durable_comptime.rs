//! Durable compile-time values shared by the legacy evaluator and the AIR
//! adapter boundary.
//!
//! The value cases in this module are deliberately the existing durable
//! evaluator representation.  The AIR implementations below are only an
//! adapter over that representation; they do not define a second value
//! algebra or perform evaluation.

use std::sync::Arc;

use rue_air::{ComptimeType, ComptimeValue};
use rue_query::QueryAbort;

use crate::ModuleId;
use crate::body_query::ForeignComptimeCallLookup;
use crate::declaration_candidate::{DeclarationCandidateKey, DeclarationImportFailure};
use crate::durable_semantics::{DurableConstValue, DurableType};

/// The exact semantic site consumed by the durable import authority.
///
/// This intentionally carries the declaration identity, semantic occurrence,
/// and decoded specifier only. It cannot name or evaluate an RIR instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableImportSite {
    pub(crate) declaration: DeclarationCandidateKey,
    pub(crate) occurrence: u32,
    pub(crate) specifier: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableImportResolution {
    Resolved(ModuleId),
    Missing,
    Failure(DeclarationImportFailure),
}

/// Canonical semantic services needed by durable comptime entry points.
///
/// Implementations live beside the query authorities. This facade is an
/// operation boundary, not an evaluator: neither trait accepts an instruction
/// reference, instruction data, or callback capable of evaluating a child.
pub(crate) trait DurableComptimeSemanticAuthority {
    fn check_canceled(&self) -> Result<(), QueryAbort>;

    fn resolve_import(
        &self,
        site: &DurableImportSite,
    ) -> Result<DurableImportResolution, QueryAbort>;
}

pub(crate) trait DurableComptimeForeignCallAuthority {
    fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort>;
}

pub(crate) struct DurableComptimeServices<'a, A: ?Sized> {
    authority: &'a A,
}

impl<'a, A: ?Sized> DurableComptimeServices<'a, A> {
    pub(crate) fn new(authority: &'a A) -> Self {
        Self { authority }
    }
}

impl<A: DurableComptimeSemanticAuthority + ?Sized> DurableComptimeServices<'_, A> {
    pub(crate) fn check_canceled(&self) -> Result<(), QueryAbort> {
        self.authority.check_canceled()
    }

    pub(crate) fn resolve_import(
        &self,
        site: &DurableImportSite,
    ) -> Result<DurableImportResolution, QueryAbort> {
        self.authority.resolve_import(site)
    }
}

impl<A: DurableComptimeForeignCallAuthority + ?Sized> DurableComptimeServices<'_, A> {
    /// Probe only an already-published foreign fact or admit its owned body
    /// frame. The authority owns dependency observation and cancellation; this
    /// method never demands a child comptime query.
    pub(crate) fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort> {
        self.authority
            .probe_comptime_call(producer, type_arguments, value_arguments)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluatedSemanticConst {
    Value(Arc<TypedSemanticConst>),
    Module(ModuleId),
    TargetEnum(TargetEnumValue),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetEnumValue {
    pub(crate) type_name: &'static str,
    pub(crate) variant: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedSemanticConst {
    pub(crate) value: DurableConstValue,
    /// `None` is reserved for an unconstrained integer literal. Every named
    /// value, local derived from one, and completed operation carries its
    /// canonical semantic type; consumers must never reconstruct it from the
    /// value's magnitude.
    pub(crate) ty: Option<DurableType>,
}

impl TypedSemanticConst {
    pub(crate) fn typed(value: DurableConstValue, ty: DurableType) -> Arc<Self> {
        Arc::new(Self {
            value,
            ty: Some(ty),
        })
    }

    pub(crate) fn integer_literal(value: i128) -> Arc<Self> {
        Arc::new(Self {
            value: DurableConstValue::Integer(value),
            ty: None,
        })
    }
}

/// AIR's type marker for a durable value.
///
/// Keeping the wrapper local avoids implementing an AIR trait for the generic
/// semantic-import type alias, which would violate Rust's orphan rules. The
/// conversion is lossless and intentionally carries no behavior of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeType(pub(crate) DurableType);

impl From<DurableType> for DurableComptimeType {
    fn from(value: DurableType) -> Self {
        Self(value)
    }
}

impl From<DurableComptimeType> for DurableType {
    fn from(value: DurableComptimeType) -> Self {
        value.0
    }
}

impl AsRef<DurableType> for DurableComptimeType {
    fn as_ref(&self) -> &DurableType {
        &self.0
    }
}

impl ComptimeType for DurableComptimeType {}

impl ComptimeValue for EvaluatedSemanticConst {
    type Type = DurableComptimeType;

    fn integer(value: i128) -> Self {
        Self::Value(TypedSemanticConst::integer_literal(value))
    }

    fn boolean(value: bool) -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Bool(value),
            DurableType::Bool,
        ))
    }

    fn unit() -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Unit,
            DurableType::Unit,
        ))
    }

    fn type_value(value: Self::Type) -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Type(value.0),
            DurableType::ComptimeType,
        ))
    }

    fn as_integer(&self) -> Option<i128> {
        let Self::Value(value) = self else {
            return None;
        };
        match value.value {
            DurableConstValue::Integer(value) => Some(value),
            DurableConstValue::Bool(_)
            | DurableConstValue::Type(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_boolean(&self) -> Option<bool> {
        let Self::Value(value) = self else {
            return None;
        };
        match value.value {
            DurableConstValue::Bool(value) => Some(value),
            DurableConstValue::Integer(_)
            | DurableConstValue::Type(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_type(&self) -> Option<Self::Type> {
        let Self::Value(value) = self else {
            return None;
        };
        match &value.value {
            DurableConstValue::Type(value) => Some(DurableComptimeType(value.clone())),
            DurableConstValue::Integer(_)
            | DurableConstValue::Bool(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_integer_type(&self) -> Option<Self::Type> {
        let Self::Value(value) = self else {
            return None;
        };
        if !matches!(value.value, DurableConstValue::Integer(_)) {
            return None;
        }
        value.ty.clone().map(DurableComptimeType)
    }

    fn integer_typed(value: i128, ty: Option<Self::Type>) -> Self {
        match ty {
            Some(ty) => Self::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(value),
                ty.0,
            )),
            None => Self::integer(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(value: DurableConstValue, ty: Option<DurableType>) -> EvaluatedSemanticConst {
        EvaluatedSemanticConst::Value(Arc::new(TypedSemanticConst { value, ty }))
    }

    #[test]
    fn scalar_constructors_preserve_the_existing_durable_forms() {
        assert_eq!(
            EvaluatedSemanticConst::integer(7),
            value(DurableConstValue::Integer(7), None)
        );
        assert_eq!(
            EvaluatedSemanticConst::boolean(true),
            value(DurableConstValue::Bool(true), Some(DurableType::Bool))
        );
        assert_eq!(
            EvaluatedSemanticConst::unit(),
            value(DurableConstValue::Unit, Some(DurableType::Unit))
        );
    }

    #[test]
    fn integer_metadata_is_optional_and_lossless() {
        let plain = EvaluatedSemanticConst::integer(9);
        assert_eq!(plain.as_integer(), Some(9));
        assert_eq!(plain.as_integer_type(), None);

        let typed =
            EvaluatedSemanticConst::integer_typed(9, Some(DurableComptimeType(DurableType::I16)));
        assert_eq!(typed.as_integer(), Some(9));
        assert_eq!(
            typed.as_integer_type(),
            Some(DurableComptimeType(DurableType::I16))
        );
        assert_eq!(
            typed,
            value(DurableConstValue::Integer(9), Some(DurableType::I16))
        );
    }

    #[test]
    fn type_values_round_trip_without_reinterpreting_other_variants() {
        let ty = DurableComptimeType(DurableType::Array {
            element: Arc::new(DurableType::U32),
            len: 3,
        });
        let type_value = EvaluatedSemanticConst::type_value(ty.clone());
        assert_eq!(type_value.as_type(), Some(ty.clone()));
        assert_eq!(
            type_value,
            value(
                DurableConstValue::Type(ty.0),
                Some(DurableType::ComptimeType)
            )
        );
        assert_eq!(type_value.as_integer(), None);
        assert_eq!(type_value.as_boolean(), None);
        assert_eq!(type_value.as_integer_type(), None);
    }

    #[test]
    fn module_and_target_enum_values_are_not_scalar_values() {
        let module = EvaluatedSemanticConst::Module(ModuleId::from_logical_path("m").unwrap());
        assert_eq!(module.as_integer(), None);
        assert_eq!(module.as_boolean(), None);
        assert_eq!(module.as_type(), None);
        assert_eq!(module.as_integer_type(), None);

        let target = EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name: "Os",
            variant: "Macos",
        });
        assert_eq!(target.as_integer(), None);
        assert_eq!(target.as_boolean(), None);
        assert_eq!(target.as_type(), None);
        assert_eq!(target.as_integer_type(), None);
    }

    #[test]
    fn clone_and_conversions_preserve_representation() {
        let ty = DurableType::PtrMut(Arc::new(DurableType::I64));
        let wrapped = DurableComptimeType(ty.clone());
        let unwrapped: DurableType = wrapped.clone().into();
        assert_eq!(unwrapped, ty.clone());
        assert_eq!(DurableComptimeType::from(ty.clone()).as_ref(), &ty);

        let original =
            EvaluatedSemanticConst::integer_typed(-12, Some(DurableComptimeType(ty.clone())));
        assert_eq!(original.clone(), original);
        assert_eq!(original.as_integer_type(), Some(DurableComptimeType(ty)));
    }
}
