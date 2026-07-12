//! Request-independent semantic values used at compiler query boundaries.
//!
//! These types deliberately have no conversion from `rue_air::Type`. Such a
//! conversion is only sound while the successful declaration binder, its type
//! pool, and the exact-revision stable-definition join are available together.

use std::sync::Arc;

use rue_air::{
    SemanticBindingKind, SemanticDeclarationExport, SemanticDeclarationPayload,
    SemanticExportConstValue, SemanticExportFailure, SemanticExportType, SemanticParameterMode,
};

use crate::{
    BoundDefinitionSet, CanonicalMergedProgram, ModuleId, StableDefinitionKey, StableDefinitionKind,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableParameterMode {
    Value,
    Borrow,
    Inout,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableSemanticParameter {
    pub ty: DurableType,
    pub mode: DurableParameterMode,
    pub is_comptime: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableDeclarationPayload {
    Callable {
        parameters: Arc<[DurableSemanticParameter]>,
        result: DurableType,
        has_self: bool,
        is_unchecked: bool,
    },
    Struct {
        fields: Arc<[(Arc<str>, DurableType)]>,
        is_copy: bool,
        is_linear: bool,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[DurableType]>)]>,
    },
    Const {
        ty: DurableType,
        value: DurableConstValue,
    },
    Destructor,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableDeclarationSemantic {
    pub key: StableDefinitionKey,
    pub is_public: bool,
    pub payload: DurableDeclarationPayload,
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

pub(crate) fn convert_declaration_semantics(
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
    exports: &[SemanticDeclarationExport],
) -> Result<Arc<[DurableDeclarationSemantic]>, DurableSemanticExportFailure> {
    fn stable_kind(kind: SemanticBindingKind) -> Option<StableDefinitionKind> {
        Some(match kind {
            SemanticBindingKind::Function => StableDefinitionKind::Function,
            SemanticBindingKind::Struct => StableDefinitionKind::Struct,
            SemanticBindingKind::Enum => StableDefinitionKind::Enum,
            SemanticBindingKind::ValueConst => StableDefinitionKind::ValueConst,
            SemanticBindingKind::Destructor => StableDefinitionKind::Destructor,
            SemanticBindingKind::Method => StableDefinitionKind::Method,
            SemanticBindingKind::AssociatedFunction => StableDefinitionKind::AssociatedFunction,
            SemanticBindingKind::ModuleBinding => return None,
        })
    }
    let module_for_file = |file_id| {
        merged
            .ast()
            .modules()
            .iter()
            .find(|m| m.file_id() == file_id)
            .map(|m| m.module_id())
    };
    let key_for = |file_id, name: &str, kind, owner: Option<&str>| {
        let module = module_for_file(file_id)?;
        let stable_kind = stable_kind(kind)?;
        let owner_is_valid = match kind {
            SemanticBindingKind::Method | SemanticBindingKind::AssociatedFunction => {
                owner.is_some()
            }
            SemanticBindingKind::Destructor => owner == Some(name),
            _ => owner.is_none(),
        };
        if !owner_is_valid {
            return None;
        }
        let matches = definitions
            .definitions()
            .iter()
            .map(|r| r.stable_key())
            .filter(|key| {
                key.module() == module
                    && key.kind() == stable_kind
                    && key.name() == name
                    && key.owner().map(|owner| owner.name()) == owner
            })
            .collect::<Vec<_>>();
        let [key] = matches.as_slice() else {
            return None;
        };
        Some((*key).clone())
    };
    fn ty(
        value: &SemanticExportType,
        merged: &CanonicalMergedProgram,
        definitions: &BoundDefinitionSet,
    ) -> Result<DurableType, DurableSemanticExportFailure> {
        let nominal = |identity: &rue_air::SemanticNominalIdentity| {
            let module = merged
                .ast()
                .modules()
                .iter()
                .find(|m| m.file_id() == identity.file_id)
                .map(|m| m.module_id())
                .ok_or(DurableSemanticExportFailure::MissingStableNominalDefinition)?;
            let kind = match identity.kind {
                SemanticBindingKind::Struct => StableDefinitionKind::Struct,
                SemanticBindingKind::Enum => StableDefinitionKind::Enum,
                _ => return Err(DurableSemanticExportFailure::MissingStableNominalDefinition),
            };
            definitions
                .definitions()
                .iter()
                .map(|r| r.stable_key())
                .find(|key| {
                    key.module() == module
                        && key.kind() == kind
                        && key.name() == identity.name.as_ref()
                })
                .cloned()
                .map(DurableType::Nominal)
                .ok_or(DurableSemanticExportFailure::MissingStableNominalDefinition)
        };
        Ok(match value {
            SemanticExportType::I8 => DurableType::I8,
            SemanticExportType::I16 => DurableType::I16,
            SemanticExportType::I32 => DurableType::I32,
            SemanticExportType::I64 => DurableType::I64,
            SemanticExportType::U8 => DurableType::U8,
            SemanticExportType::U16 => DurableType::U16,
            SemanticExportType::U32 => DurableType::U32,
            SemanticExportType::U64 => DurableType::U64,
            SemanticExportType::Bool => DurableType::Bool,
            SemanticExportType::Unit => DurableType::Unit,
            SemanticExportType::Never => DurableType::Never,
            SemanticExportType::ComptimeType => DurableType::ComptimeType,
            SemanticExportType::GenericParameter(index) => DurableType::GenericParameter(*index),
            SemanticExportType::Nominal(n) => nominal(n)?,
            SemanticExportType::Array { element, len } => DurableType::Array {
                element: Box::new(ty(element, merged, definitions)?),
                len: *len,
            },
            SemanticExportType::PtrConst(v) => {
                DurableType::PtrConst(Box::new(ty(v, merged, definitions)?))
            }
            SemanticExportType::PtrMut(v) => {
                DurableType::PtrMut(Box::new(ty(v, merged, definitions)?))
            }
            SemanticExportType::Module(path) => {
                let module = merged
                    .ast()
                    .modules()
                    .iter()
                    .find(|m| m.physical_path() == path.as_ref())
                    .map(|m| m.module_id().clone())
                    .ok_or(DurableSemanticExportFailure::UnresolvedModule)?;
                DurableType::Module(module)
            }
        })
    }
    let mut result = Vec::with_capacity(exports.len());
    for export in exports {
        let key = key_for(
            export.identity.file_id,
            &export.identity.name,
            export.identity.kind,
            export.identity.owner.as_deref(),
        )
        .ok_or(DurableSemanticExportFailure::MissingStableNominalDefinition)?;
        let payload = match &export.payload {
            SemanticDeclarationPayload::Callable {
                parameters,
                result,
                has_self,
                is_unchecked,
            } => DurableDeclarationPayload::Callable {
                parameters: parameters
                    .iter()
                    .map(|p| {
                        Ok(DurableSemanticParameter {
                            ty: ty(&p.ty, merged, definitions)?,
                            mode: match p.mode {
                                SemanticParameterMode::Value => DurableParameterMode::Value,
                                SemanticParameterMode::Borrow => DurableParameterMode::Borrow,
                                SemanticParameterMode::Inout => DurableParameterMode::Inout,
                            },
                            is_comptime: p.is_comptime,
                        })
                    })
                    .collect::<Result<Vec<_>, DurableSemanticExportFailure>>()?
                    .into(),
                result: ty(result, merged, definitions)?,
                has_self: *has_self,
                is_unchecked: *is_unchecked,
            },
            SemanticDeclarationPayload::Struct {
                fields,
                is_copy,
                is_linear,
            } => DurableDeclarationPayload::Struct {
                fields: fields
                    .iter()
                    .map(|(n, t)| Ok((n.clone(), ty(t, merged, definitions)?)))
                    .collect::<Result<Vec<_>, DurableSemanticExportFailure>>()?
                    .into(),
                is_copy: *is_copy,
                is_linear: *is_linear,
            },
            SemanticDeclarationPayload::Enum { variants } => DurableDeclarationPayload::Enum {
                variants: variants
                    .iter()
                    .map(|(n, p)| {
                        Ok((
                            n.clone(),
                            p.iter()
                                .map(|t| ty(t, merged, definitions))
                                .collect::<Result<Vec<_>, _>>()?
                                .into(),
                        ))
                    })
                    .collect::<Result<Vec<_>, DurableSemanticExportFailure>>()?
                    .into(),
            },
            SemanticDeclarationPayload::Const {
                ty: const_ty,
                value,
            } => {
                let value = match value {
                    SemanticExportConstValue::Integer(v) => DurableConstValue::Integer(*v),
                    SemanticExportConstValue::Bool(v) => DurableConstValue::Bool(*v),
                    SemanticExportConstValue::Type(t) => {
                        DurableConstValue::Type(ty(t, merged, definitions)?)
                    }
                    SemanticExportConstValue::Unit => DurableConstValue::Unit,
                    SemanticExportConstValue::Function { file_id, name } => {
                        DurableConstValue::Function(
                            key_for(*file_id, name, SemanticBindingKind::Function, None).ok_or(
                                DurableSemanticExportFailure::MissingStableFunctionDefinition,
                            )?,
                        )
                    }
                };
                DurableDeclarationPayload::Const {
                    ty: ty(const_ty, merged, definitions)?,
                    value,
                }
            }
            SemanticDeclarationPayload::Destructor => DurableDeclarationPayload::Destructor,
        };
        result.push(DurableDeclarationSemantic {
            key,
            is_public: export.identity.is_public,
            payload,
        });
    }
    result.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(result.into())
}

impl From<SemanticExportFailure> for DurableSemanticExportFailure {
    fn from(value: SemanticExportFailure) -> Self {
        match value {
            SemanticExportFailure::ErrorType => Self::ErrorType,
            SemanticExportFailure::AnonymousNominalType => Self::AnonymousNominalType,
            SemanticExportFailure::UnmappedNominalType => Self::MissingStableNominalDefinition,
            SemanticExportFailure::UnmappedFunction => Self::MissingStableFunctionDefinition,
            SemanticExportFailure::UnsupportedParameterMode => Self::UnsupportedTypeForm,
            SemanticExportFailure::RecursiveStructuralType => Self::RecursiveStructuralType,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_query_value<T: Send + Sync + Clone + Eq + Ord + std::hash::Hash>() {}

    #[test]
    fn durable_values_have_query_key_traits() {
        assert_query_value::<DurableType>();
        assert_query_value::<DurableConstValue>();
        assert_query_value::<DurableSemanticParameter>();
        assert_query_value::<DurableDeclarationPayload>();
        assert_query_value::<DurableDeclarationSemantic>();
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
