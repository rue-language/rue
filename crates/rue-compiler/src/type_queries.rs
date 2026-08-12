//! Canonical demand-driven type, layout, call-ABI, and drop-glue query values.
//!
//! These records deliberately use stable semantic identities.  Live `Type`
//! handles and pool indexes are materialization details and never cross this
//! boundary.

use std::sync::Arc;

use rue_query::QueryKey;

use crate::retained_charge::RetainedCharge;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TypeQueryKey {
    pub(crate) ty: crate::TypeInstanceKey,
    pub(crate) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
}

impl QueryKey for TypeQueryKey {
    fn stable_identity(&self) -> String {
        format!(
            "{:?};target={:?};preview={:?}",
            self.ty, self.configuration.target, self.configuration.preview_features
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeShape {
    Scalar,
    Pointer,
    Slice,
    Array {
        element: crate::TypeInstanceKey,
        len: u64,
    },
    Struct {
        fields: Arc<[(Arc<str>, crate::TypeInstanceKey)]>,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[crate::TypeInstanceKey]>)]>,
    },
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeShapeValue {
    Available(TypeShape),
    Failure(TypeQueryFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeFacts {
    pub(crate) is_copy: bool,
    pub(crate) carries_linear: bool,
    pub(crate) needs_drop: bool,
    pub(crate) destructor: Option<crate::FunctionInstanceKey>,
    /// Repeated for ownership consumers that need to enumerate children. The
    /// canonical structural stamp is [`TypeShapeValue`]; layout never observes
    /// this aggregate ownership value.
    pub(crate) shape: TypeShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeQueryFailure {
    Unavailable(Arc<str>),
    Invalid(Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeFactsValue {
    Available(Box<TypeFacts>),
    Failure(TypeQueryFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalLayout {
    pub(crate) size: u64,
    pub(crate) alignment: u64,
    pub(crate) stride: u64,
    pub(crate) abi_slots: u32,
    /// Whether the compact memory image is byte-for-byte identical to the
    /// flattened eight-byte slot representation used by the native call ABI.
    pub(crate) slot_identical: bool,
    pub(crate) kind: CanonicalLayoutKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalLayoutKind {
    Scalar,
    Pointer,
    Slice,
    Array {
        element: Option<Box<CanonicalLayout>>,
        count: u64,
    },
    Struct {
        field_offsets: Arc<[u64]>,
        padding_ranges: Arc<[rue_air::PaddingRange]>,
    },
    Enum {
        tag_size: u64,
        payload_offset: u64,
        variants: Arc<[Arc<[u64]>]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LayoutValue {
    Available(CanonicalLayout),
    Failure(TypeQueryFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CallAbiQueryKey {
    pub(crate) callable: crate::FunctionInstanceKey,
    pub(crate) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
}

impl QueryKey for CallAbiQueryKey {
    fn stable_identity(&self) -> String {
        format!(
            "{:?};target={:?};preview={:?}",
            self.callable, self.configuration.target, self.configuration.preview_features
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallAbiFacts {
    pub(crate) convention: CallAbiConvention,
    pub(crate) return_class: CallAbiReturnClass,
    pub(crate) arguments: Arc<[CallAbiArgument]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallAbiConvention {
    Native,
    TargetC(rue_air::TargetCAbiFlavor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallAbiReturnClass {
    ZeroSized,
    Scalar {
        extension: rue_air::ScalarAbiExtension,
    },
    NativeRegisters {
        slots: u32,
    },
    NativeIndirect {
        slots: u32,
    },
    CIntegerRegisters {
        eightbytes: u32,
    },
    CIndirect {
        size: u32,
        alignment: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallAbiArgument {
    pub(crate) mode: crate::durable_semantics::DurableParameterMode,
    pub(crate) value_slots: u32,
    pub(crate) class: CallAbiArgumentClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallAbiArgumentClass {
    Omitted,
    NativeDirect {
        slots: u32,
    },
    NativeIndirect,
    CScalar {
        extension: rue_air::ScalarAbiExtension,
    },
    CIntegerRegisters {
        eightbytes: u32,
    },
    CByValueStack {
        size: u32,
        alignment: u32,
    },
    CByReferenceCopy {
        size: u32,
        alignment: u32,
    },
    Reference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallAbiValue {
    Available(CallAbiFacts),
    Failure(TypeQueryFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DropGlueFacts<D = crate::StableDefinitionKey, M = crate::ModuleId> {
    pub(crate) required: bool,
    pub(crate) synthesize: bool,
    pub(crate) destructor: Option<rue_air::FunctionInstanceKey<D, M>>,
    pub(crate) nested: Arc<[rue_air::TypeInstanceKey<D, M>]>,
    pub(crate) plan: DropGluePlan<D, M>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DropGluePlan<D = crate::StableDefinitionKey, M = crate::ModuleId> {
    None,
    Struct {
        fields: Arc<[DropGlueField<D, M>]>,
    },
    Array {
        element: rue_air::TypeInstanceKey<D, M>,
        len: u64,
        drop_element: bool,
    },
    Enum {
        variants: Arc<[DropGlueVariant<D, M>]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DropGlueField<D = crate::StableDefinitionKey, M = crate::ModuleId> {
    pub(crate) name: Arc<str>,
    pub(crate) ty: rue_air::TypeInstanceKey<D, M>,
    pub(crate) drop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DropGlueVariant<D = crate::StableDefinitionKey, M = crate::ModuleId> {
    pub(crate) name: Arc<str>,
    pub(crate) fields: Arc<[DropGlueVariantField<D, M>]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DropGlueVariantField<D = crate::StableDefinitionKey, M = crate::ModuleId> {
    pub(crate) ty: rue_air::TypeInstanceKey<D, M>,
    pub(crate) drop: bool,
}

impl<D, M> DropGlueFacts<D, M> {
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn try_map_identities<D2, M2, E>(
        &self,
        definition: &impl Fn(&D) -> Result<D2, E>,
        module: &impl Fn(&M) -> Result<M2, E>,
    ) -> Result<DropGlueFacts<D2, M2>, E> {
        let map_type =
            |ty: &rue_air::TypeInstanceKey<D, M>| ty.try_map_identities(definition, module);
        Ok(DropGlueFacts {
            required: self.required,
            synthesize: self.synthesize,
            destructor: self
                .destructor
                .as_ref()
                .map(|value| value.try_map_identities(definition, module))
                .transpose()?,
            nested: self
                .nested
                .iter()
                .map(&map_type)
                .collect::<Result<Vec<_>, _>>()?
                .into(),
            plan: match &self.plan {
                DropGluePlan::None => DropGluePlan::None,
                DropGluePlan::Struct { fields } => DropGluePlan::Struct {
                    fields: fields
                        .iter()
                        .map(|field| {
                            Ok(DropGlueField {
                                name: field.name.clone(),
                                ty: map_type(&field.ty)?,
                                drop: field.drop,
                            })
                        })
                        .collect::<Result<Vec<_>, E>>()?
                        .into(),
                },
                DropGluePlan::Array {
                    element,
                    len,
                    drop_element,
                } => DropGluePlan::Array {
                    element: map_type(element)?,
                    len: *len,
                    drop_element: *drop_element,
                },
                DropGluePlan::Enum { variants } => DropGluePlan::Enum {
                    variants: variants
                        .iter()
                        .map(|variant| {
                            Ok(DropGlueVariant {
                                name: variant.name.clone(),
                                fields: variant
                                    .fields
                                    .iter()
                                    .map(|field| {
                                        Ok(DropGlueVariantField {
                                            ty: map_type(&field.ty)?,
                                            drop: field.drop,
                                        })
                                    })
                                    .collect::<Result<Vec<_>, E>>()?
                                    .into(),
                            })
                        })
                        .collect::<Result<Vec<_>, E>>()?
                        .into(),
                },
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DropGlueValue {
    Available(Box<DropGlueFacts>),
    Failure(TypeQueryFailure),
}

impl RetainedCharge for TypeShape {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Array { element, .. } => element.retained_charge(),
            Self::Struct { fields } => fields.retained_charge(),
            Self::Enum { variants } => variants.retained_charge(),
            Self::Scalar | Self::Pointer | Self::Slice | Self::Opaque => 0,
        }
    }
}

impl RetainedCharge for TypeShapeValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(shape) => shape.retained_charge(),
            Self::Failure(failure) => failure.retained_charge(),
        }
    }
}

impl RetainedCharge for TypeFacts {
    fn retained_charge(&self) -> u64 {
        self.destructor
            .retained_charge()
            .saturating_add(self.shape.retained_charge())
    }
}

impl RetainedCharge for TypeQueryFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Unavailable(detail) | Self::Invalid(detail) => detail.retained_charge(),
        }
    }
}

impl RetainedCharge for TypeFactsValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(facts) => facts.retained_charge(),
            Self::Failure(failure) => failure.retained_charge(),
        }
    }
}

impl RetainedCharge for CanonicalLayout {
    fn retained_charge(&self) -> u64 {
        self.kind.retained_charge()
    }
}

impl RetainedCharge for CanonicalLayoutKind {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Array { element, .. } => element.retained_charge(),
            Self::Struct {
                field_offsets,
                padding_ranges,
            } => field_offsets
                .retained_charge()
                .saturating_add(padding_ranges.retained_charge()),
            Self::Enum { variants, .. } => variants.retained_charge(),
            Self::Scalar | Self::Pointer | Self::Slice => 0,
        }
    }
}

impl RetainedCharge for LayoutValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(layout) => layout.retained_charge(),
            Self::Failure(failure) => failure.retained_charge(),
        }
    }
}

impl RetainedCharge for CallAbiArgument {
    fn retained_charge(&self) -> u64 {
        0
    }
}

impl RetainedCharge for CallAbiFacts {
    fn retained_charge(&self) -> u64 {
        self.arguments.retained_charge()
    }
}

impl RetainedCharge for CallAbiValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(facts) => facts.retained_charge(),
            Self::Failure(failure) => failure.retained_charge(),
        }
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for DropGlueFacts<D, M> {
    fn retained_charge(&self) -> u64 {
        self.destructor
            .retained_charge()
            .saturating_add(self.nested.retained_charge())
            .saturating_add(self.plan.retained_charge())
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for DropGluePlan<D, M> {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::None => 0,
            Self::Struct { fields } => fields.retained_charge(),
            Self::Array { element, .. } => element.retained_charge(),
            Self::Enum { variants } => variants.retained_charge(),
        }
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for DropGlueField<D, M> {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.ty.retained_charge())
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for DropGlueVariant<D, M> {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.fields.retained_charge())
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for DropGlueVariantField<D, M> {
    fn retained_charge(&self) -> u64 {
        self.ty.retained_charge()
    }
}

impl RetainedCharge for DropGlueValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(facts) => facts.retained_charge(),
            Self::Failure(failure) => failure.retained_charge(),
        }
    }
}

pub(crate) fn type_instance(ty: &crate::durable_semantics::DurableType) -> crate::TypeInstanceKey {
    use crate::durable_semantics::DurableType as T;
    match ty {
        T::I8 => crate::TypeInstanceKey::I8,
        T::I16 => crate::TypeInstanceKey::I16,
        T::I32 => crate::TypeInstanceKey::I32,
        T::I64 => crate::TypeInstanceKey::I64,
        T::U8 => crate::TypeInstanceKey::U8,
        T::U16 => crate::TypeInstanceKey::U16,
        T::U32 => crate::TypeInstanceKey::U32,
        T::U64 => crate::TypeInstanceKey::U64,
        T::Bool => crate::TypeInstanceKey::Bool,
        T::Unit => crate::TypeInstanceKey::Unit,
        T::Never => crate::TypeInstanceKey::Never,
        T::ComptimeType => crate::TypeInstanceKey::ComptimeType,
        T::BuiltinNominal { kind, name } => crate::TypeInstanceKey::BuiltinNominal {
            kind: match kind {
                rue_air::SemanticImportNominalKind::Struct => rue_air::AnonymousNominalKind::Struct,
                rue_air::SemanticImportNominalKind::Enum => rue_air::AnonymousNominalKind::Enum,
            },
            name: name.clone(),
        },
        T::Nominal(definition) => {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(definition.clone()))
        }
        T::AnonymousNominal(identity) => {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(identity.clone()))
        }
        T::Array { element, len } => crate::TypeInstanceKey::Array {
            element: Box::new(type_instance(element)),
            len: *len,
        },
        T::Slice { element, name } => crate::TypeInstanceKey::Slice {
            element: Box::new(type_instance(element)),
            name: name.clone(),
        },
        T::PtrConst(element) => crate::TypeInstanceKey::PtrConst(Box::new(type_instance(element))),
        T::PtrMut(element) => crate::TypeInstanceKey::PtrMut(Box::new(type_instance(element))),
        T::Module(module) => crate::TypeInstanceKey::Module(module.clone()),
        T::GenericParameter(index) => crate::TypeInstanceKey::GenericParameter(*index),
    }
}

pub(crate) fn align_to(value: u64, alignment: u64) -> u64 {
    if alignment <= 1 {
        value
    } else {
        value.saturating_add(alignment - 1) / alignment * alignment
    }
}
