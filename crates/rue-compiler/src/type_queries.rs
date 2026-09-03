//! Canonical demand-driven type, layout, call-ABI, and drop-glue query values.
//!
//! These records deliberately use stable semantic identities.  Live `Type`
//! handles and pool indexes are materialization details and never cross this
//! boundary.

use rue_air::Node;
use std::hash::Hash;
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

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.ty.hash(hasher);
        self.configuration.hash(hasher);
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

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.callable.hash(hasher);
        self.configuration.hash(hasher);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CallAbiFacts {
    pub(crate) convention: CallAbiConvention,
    pub(crate) return_class: CallAbiReturnClass,
    pub(crate) arguments: Arc<[CallAbiArgument]>,
    /// The stable native machine symbol is derived once by the ABI terminal.
    /// It is a retained projection, not part of the ABI facts' semantic value.
    pub(crate) native_symbol: Option<Arc<str>>,
}

impl PartialEq for CallAbiFacts {
    fn eq(&self, other: &Self) -> bool {
        self.convention == other.convention
            && self.return_class == other.return_class
            && self.arguments == other.arguments
    }
}

impl Eq for CallAbiFacts {}

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

#[derive(Debug, Clone)]
pub(crate) struct DropGlueFacts<D = crate::StableDefinitionKey, M = crate::ModuleId> {
    pub(crate) required: bool,
    pub(crate) synthesize: bool,
    pub(crate) destructor: Option<rue_air::FunctionInstanceKey<D, M>>,
    pub(crate) nested: Arc<[rue_air::TypeInstanceKey<D, M>]>,
    pub(crate) plan: DropGluePlan<D, M>,
    /// The canonical machine symbol for this owner's synthesized drop glue.
    /// This is a retained projection, not part of the semantic drop plan.
    pub(crate) machine_symbol: Option<Arc<str>>,
    /// The canonical machine symbol for this owner's source destructor, when
    /// present. This is also a retained projection, not semantic identity.
    pub(crate) destructor_symbol: Option<Arc<str>>,
}

impl<D: PartialEq, M: PartialEq> PartialEq for DropGlueFacts<D, M> {
    fn eq(&self, other: &Self) -> bool {
        self.required == other.required
            && self.synthesize == other.synthesize
            && self.destructor == other.destructor
            && self.nested == other.nested
            && self.plan == other.plan
    }
}

impl<D: Eq, M: Eq> Eq for DropGlueFacts<D, M> {}

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
    pub(crate) fn try_map_identities<D2: std::hash::Hash, M2: std::hash::Hash, E>(
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
            machine_symbol: self.machine_symbol.clone(),
            destructor_symbol: self.destructor_symbol.clone(),
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
        self.arguments.retained_charge().saturating_add(
            self.native_symbol
                .as_ref()
                .map_or(0, |symbol| symbol.len() as u64),
        )
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
            .saturating_add(
                self.machine_symbol
                    .as_ref()
                    .map_or(0, |symbol| symbol.len() as u64),
            )
            .saturating_add(
                self.destructor_symbol
                    .as_ref()
                    .map_or(0, |symbol| symbol.len() as u64),
            )
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
        T::F32 => crate::TypeInstanceKey::F32,
        T::F64 => crate::TypeInstanceKey::F64,
        T::ComptimeFloat => crate::TypeInstanceKey::ComptimeFloat,
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
        T::AnonymousNominal(identity) => crate::TypeInstanceKey::Nominal(
            crate::NominalInstanceKey::Anonymous(Node::new(identity.clone())),
        ),
        T::Array { element, len } => crate::TypeInstanceKey::Array {
            element: Node::new(type_instance(element)),
            len: *len,
        },
        T::Slice { element, name } => crate::TypeInstanceKey::Slice {
            element: Node::new(type_instance(element)),
            name: name.clone(),
        },
        T::PtrConst(element) => crate::TypeInstanceKey::PtrConst(Node::new(type_instance(element))),
        T::PtrMut(element) => crate::TypeInstanceKey::PtrMut(Node::new(type_instance(element))),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn native_facts(symbol: &str) -> CallAbiFacts {
        CallAbiFacts {
            convention: CallAbiConvention::Native,
            return_class: CallAbiReturnClass::ZeroSized,
            arguments: Arc::from([]),
            native_symbol: Some(Arc::from(symbol)),
        }
    }

    #[test]
    fn native_symbol_is_a_retained_projection_not_abi_semantic_identity() {
        let first = native_facts("__rue_sem_v1_first");
        let second = native_facts("__rue_sem_v1_second");
        assert_eq!(first, second);
        assert_ne!(first.retained_charge(), second.retained_charge());
    }

    #[test]
    fn drop_symbols_are_retained_projections_not_drop_plan_identity() {
        let first: DropGlueFacts = DropGlueFacts {
            required: true,
            synthesize: true,
            destructor: None,
            nested: Arc::from([]),
            plan: DropGluePlan::None,
            machine_symbol: Some(Arc::from("__rue_drop_first")),
            destructor_symbol: Some(Arc::from("__rue_destructor_first")),
        };
        let second: DropGlueFacts = DropGlueFacts {
            required: true,
            synthesize: true,
            destructor: None,
            nested: Arc::from([]),
            plan: DropGluePlan::None,
            machine_symbol: Some(Arc::from("__rue_drop_second")),
            destructor_symbol: Some(Arc::from("__rue_destructor_second")),
        };
        assert_eq!(first, second);
        assert_ne!(first.retained_charge(), second.retained_charge());
    }
}
