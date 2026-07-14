//! Request-independent semantic values used at compiler query boundaries.
//!
//! These types deliberately have no conversion from `rue_air::Type`. Such a
//! conversion is only sound while the successful declaration binder, its type
//! pool, and the exact-revision stable-definition join are available together.

use std::sync::Arc;

use rue_air::{
    SemanticBinding, SemanticBindingKind, SemanticBindingNamespace, SemanticDeclarationExport,
    SemanticDeclarationPayload, SemanticDeclarationShell, SemanticExportConstValue,
    SemanticExportFailure, SemanticExportType, SemanticImportConstValue, SemanticImportEpoch,
    SemanticImportFailure, SemanticImportNominal, SemanticImportNominalKind, SemanticImportType,
    SemanticParameterMode,
};
use rue_span::FileId;

/// A fresh AIR epoch populated only from request-independent semantic values.
pub type DurableSemanticImportEpoch = SemanticImportEpoch<StableDefinitionKey, Arc<str>>;

impl DurableType {
    pub(crate) fn import_dto(&self) -> SemanticImportType<StableDefinitionKey, Arc<str>> {
        match self {
            Self::I8 => SemanticImportType::I8,
            Self::I16 => SemanticImportType::I16,
            Self::I32 => SemanticImportType::I32,
            Self::I64 => SemanticImportType::I64,
            Self::U8 => SemanticImportType::U8,
            Self::U16 => SemanticImportType::U16,
            Self::U32 => SemanticImportType::U32,
            Self::U64 => SemanticImportType::U64,
            Self::Bool => SemanticImportType::Bool,
            Self::Unit => SemanticImportType::Unit,
            Self::Never => SemanticImportType::Never,
            Self::ComptimeType => SemanticImportType::ComptimeType,
            Self::BuiltinNominal { name, kind } => SemanticImportType::BuiltinNominal {
                name: name.clone(),
                kind: *kind,
            },
            Self::Nominal(key) => SemanticImportType::Nominal(key.clone()),
            Self::Array { element, len } => SemanticImportType::Array {
                element: Box::new(element.import_dto()),
                len: *len,
            },
            Self::PtrConst(value) => SemanticImportType::PtrConst(Box::new(value.import_dto())),
            Self::PtrMut(value) => SemanticImportType::PtrMut(Box::new(value.import_dto())),
            Self::Module(module) => SemanticImportType::Module(Arc::from(module.as_str())),
            Self::GenericParameter(index) => SemanticImportType::GenericParameter(*index),
        }
    }
}

impl DurableConstValue {
    pub(crate) fn import_dto(&self) -> SemanticImportConstValue<StableDefinitionKey, Arc<str>> {
        match self {
            Self::Integer(value) => SemanticImportConstValue::Integer(*value),
            Self::Bool(value) => SemanticImportConstValue::Bool(*value),
            Self::Type(value) => SemanticImportConstValue::Type(value.import_dto()),
            Self::Function(key) => SemanticImportConstValue::Function(key.clone()),
            Self::Unit => SemanticImportConstValue::Unit,
        }
    }
}

/// Reconstruct the representable declaration universe in a new AIR epoch.
///
/// Nominal shells are issued in stable-key order before any field or variant
/// type is imported, so mutually recursive pointer graphs are supported. The
/// returned epoch contains no handles from the exporting semantic request.
pub fn import_durable_declaration_semantics(
    declarations: &[DurableDeclarationSemantic],
) -> Result<DurableSemanticImportEpoch, SemanticImportFailure> {
    fn payload_matches_key(declaration: &DurableDeclarationSemantic) -> bool {
        matches!(
            (declaration.key.kind(), &declaration.payload),
            (
                StableDefinitionKind::Function
                    | StableDefinitionKind::Method
                    | StableDefinitionKind::AssociatedFunction,
                DurableDeclarationPayload::Callable { .. }
            ) | (
                StableDefinitionKind::Struct,
                DurableDeclarationPayload::Struct { .. }
            ) | (
                StableDefinitionKind::Enum,
                DurableDeclarationPayload::Enum { .. }
            ) | (
                StableDefinitionKind::ValueConst,
                DurableDeclarationPayload::Const { .. }
            ) | (
                StableDefinitionKind::Destructor,
                DurableDeclarationPayload::Destructor
            )
        )
    }

    if declarations
        .iter()
        .any(|declaration| !payload_matches_key(declaration))
    {
        return Err(SemanticImportFailure::DeclarationKindMismatch);
    }
    fn collect_modules(ty: &DurableType, modules: &mut std::collections::BTreeSet<Arc<str>>) {
        match ty {
            DurableType::BuiltinNominal { .. } => {}
            DurableType::Module(module) => {
                modules.insert(Arc::from(module.as_str()));
            }
            DurableType::Array { element, .. }
            | DurableType::PtrConst(element)
            | DurableType::PtrMut(element) => collect_modules(element, modules),
            _ => {}
        }
    }
    let mut modules = std::collections::BTreeSet::<Arc<str>>::new();
    let mut nominals = Vec::new();
    let mut functions = Vec::new();
    for declaration in declarations {
        modules.insert(Arc::from(declaration.key.module().as_str()));
        match &declaration.payload {
            DurableDeclarationPayload::Callable {
                parameters, result, ..
            } => {
                for parameter in parameters.iter() {
                    collect_modules(&parameter.ty, &mut modules);
                }
                collect_modules(result, &mut modules);
            }
            DurableDeclarationPayload::Struct { fields, .. } => {
                for (_, ty) in fields.iter() {
                    collect_modules(ty, &mut modules);
                }
            }
            DurableDeclarationPayload::Enum { variants } => {
                for (_, payload) in variants.iter() {
                    for ty in payload.iter() {
                        collect_modules(ty, &mut modules);
                    }
                }
            }
            DurableDeclarationPayload::Const { ty, value } => {
                collect_modules(ty, &mut modules);
                if let DurableConstValue::Type(ty) = value {
                    collect_modules(ty, &mut modules);
                }
            }
            DurableDeclarationPayload::Destructor => {}
        }
        match &declaration.payload {
            DurableDeclarationPayload::Struct { .. } => nominals.push(SemanticImportNominal {
                key: declaration.key.clone(),
                module_path: Arc::from(declaration.key.module().as_str()),
                name: Arc::from(declaration.key.name()),
                kind: SemanticImportNominalKind::Struct,
                is_public: declaration.is_public,
                lang_item: if declaration.key.module().is_trusted_standard_library() {
                    rue_air::LangItem::from_standard_library_nominal(
                        declaration.key.module().as_str(),
                        declaration.key.name(),
                    )
                } else {
                    None
                },
            }),
            DurableDeclarationPayload::Enum { .. } => nominals.push(SemanticImportNominal {
                key: declaration.key.clone(),
                module_path: Arc::from(declaration.key.module().as_str()),
                name: Arc::from(declaration.key.name()),
                kind: SemanticImportNominalKind::Enum,
                is_public: declaration.is_public,
                lang_item: None,
            }),
            DurableDeclarationPayload::Callable { .. } => {
                // Length-prefix every component. Unlike a source-like name this
                // is injective for the full stable key, including its owner.
                let owner = declaration.key.owner();
                let owner_module = owner.map(|owner| owner.module().as_str()).unwrap_or("");
                let owner_name = owner.map(|owner| owner.name()).unwrap_or("");
                let kind = format!("{:?}", declaration.key.kind());
                let identity = format!(
                    "{}:{}|{:?}|{}:{}|{}:{}|{}:{}|{:?}|{}:{}",
                    declaration.key.module().as_str().len(),
                    declaration.key.module().as_str(),
                    declaration.key.namespace(),
                    kind.len(),
                    kind,
                    declaration.key.name().len(),
                    declaration.key.name(),
                    owner_module.len(),
                    owner_module,
                    owner.map(|owner| owner.kind()),
                    owner_name.len(),
                    owner_name,
                );
                functions.push((declaration.key.clone(), Arc::from(identity)));
            }
            _ => {}
        }
    }
    let epoch = SemanticImportEpoch::new(nominals, functions, modules.into_iter().collect())?;
    for declaration in declarations {
        match &declaration.payload {
            DurableDeclarationPayload::Struct {
                fields,
                is_copy,
                is_linear,
            } => {
                let fields = fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), ty.import_dto()))
                    .collect::<Vec<_>>();
                epoch.complete_struct(&declaration.key, &fields, *is_copy, *is_linear)?;
            }
            DurableDeclarationPayload::Enum { variants } => {
                let variants = variants
                    .iter()
                    .map(|(name, payload)| {
                        (
                            name.clone(),
                            payload
                                .iter()
                                .map(DurableType::import_dto)
                                .collect::<Vec<_>>()
                                .into(),
                        )
                    })
                    .collect::<Vec<_>>();
                epoch.complete_enum(&declaration.key, &variants)?;
            }
            DurableDeclarationPayload::Const { ty, value } => {
                epoch.import_type(&ty.import_dto())?;
                epoch.import_const_value(&value.import_dto())?;
            }
            DurableDeclarationPayload::Callable {
                parameters, result, ..
            } => {
                let parameters = parameters
                    .iter()
                    .map(|parameter| (parameter.ty.import_dto(), parameter.is_comptime))
                    .collect::<Vec<_>>();
                epoch.validate_callable_signature(&parameters, &result.import_dto())?;
            }
            DurableDeclarationPayload::Destructor => {}
        }
    }
    Ok(epoch)
}

use crate::{
    BoundDefinitionSet, CanonicalMergedProgram, ModuleId, StableDefinitionKey, StableDefinitionKind,
};

/// Version of the canonical durable type/value encoding.
pub const DURABLE_SEMANTIC_SCHEMA_VERSION: u32 = 3;

pub(crate) fn builtin_nominal_kind(name: &str) -> Option<SemanticImportNominalKind> {
    if name == "str" {
        Some(SemanticImportNominalKind::Struct)
    } else if rue_builtins::get_builtin_enum(name).is_some() {
        Some(SemanticImportNominalKind::Enum)
    } else {
        None
    }
}

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
    /// A compiler-injected nominal, identified by the canonical builtin
    /// registry rather than by a source definition key.
    BuiltinNominal {
        name: Arc<str>,
        kind: SemanticImportNominalKind,
    },
    /// A named struct or enum in the exact stable definition universe.
    Nominal(StableDefinitionKey),
    Array {
        element: Box<DurableType>,
        len: u64,
    },
    PtrConst(Box<DurableType>),
    PtrMut(Box<DurableType>),
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

/// Work performed by the stable-key/current-revision projection adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DurableSemanticProjectionWork {
    pub projection_invocations: usize,
    pub shells_visited: usize,
    pub durable_records_visited: usize,
    /// Stable definition records inserted into the exact projection join index.
    pub definition_records_indexed: usize,
    /// Exact-key definition index probes performed while joining shells.
    pub definition_lookup_probes: usize,
    /// Projection is a metadata join and must never inspect RIR.
    pub rir_instructions_visited: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectionJoinKey {
    module: ModuleId,
    namespace: crate::StableDefinitionNamespace,
    kind: StableDefinitionKind,
    name: Arc<str>,
    owner: Option<Arc<str>>,
}

impl ProjectionJoinKey {
    fn from_definition(key: &StableDefinitionKey) -> Self {
        Self {
            module: key.module().clone(),
            namespace: key.namespace(),
            kind: key.kind(),
            name: Arc::from(key.name()),
            owner: key.owner().map(|owner| Arc::from(owner.name())),
        }
    }

    fn from_shell(shell: &SemanticDeclarationShell) -> Option<Self> {
        Some(Self {
            module: if shell.identity.is_trusted_standard_library {
                ModuleId::from_trusted_validated_canonical(&shell.identity.module_path)
            } else {
                ModuleId::from_validated_canonical(&shell.identity.module_path)
            },
            namespace: stable_namespace(shell.identity.namespace),
            kind: stable_kind(shell.identity.kind)?,
            name: shell.identity.name.clone(),
            owner: shell.identity.owner.clone(),
        })
    }
}

/// Typed reasons that make durable installation ineligible.  Projection is
/// atomic: no AIR state is mutated before this validation succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableSemanticProjectionFailure {
    DefinitionUniverseRevisionMismatch,
    MissingDefinition,
    DuplicateDefinition,
    ExtraDefinition,
    MissingShell,
    DuplicateShell,
    AmbiguousDefinition,
    NamespaceMismatch,
    KindMismatch,
    OwnerMismatch,
    ModuleMismatch,
    VisibilityMismatch,
    UnsupportedDeclaration,
    UnsupportedType,
}

/// Project stable-keyed semantics into exact-current-revision AIR DTOs.
///
/// The definition set supplies provenance and `FileId`/span ownership, while
/// shells supply authoritative body and parameter metadata. Records are
/// returned in stable-key order. The join is deliberately total and
/// bijective for the supported subset.
pub fn project_durable_declaration_semantics(
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
    shells: &[SemanticDeclarationShell],
    durable: &[DurableDeclarationSemantic],
) -> Result<
    (
        Arc<[SemanticDeclarationExport]>,
        DurableSemanticProjectionWork,
    ),
    DurableSemanticProjectionFailure,
> {
    use std::collections::{BTreeMap, BTreeSet};

    if definitions.source_revision() != merged.ast().source_revision() {
        return Err(DurableSemanticProjectionFailure::DefinitionUniverseRevisionMismatch);
    }
    let modules = merged.ast().modules();
    let module_files = modules
        .iter()
        .map(|module| (module.module_id().clone(), module.file_id()))
        .collect::<BTreeMap<_, _>>();
    if module_files.len() != modules.len() {
        return Err(DurableSemanticProjectionFailure::AmbiguousDefinition);
    }

    let mut definition_by_join_key = BTreeMap::new();
    for record in definitions.definitions() {
        let join_key = ProjectionJoinKey::from_definition(record.stable_key());
        if definition_by_join_key
            .insert(join_key, record.stable_key().clone())
            .is_some()
        {
            return Err(DurableSemanticProjectionFailure::DuplicateDefinition);
        }
    }

    let mut shell_by_key = BTreeMap::new();
    for shell in shells {
        let join_key = ProjectionJoinKey::from_shell(shell)
            .ok_or(DurableSemanticProjectionFailure::UnsupportedDeclaration)?;
        let key = definition_by_join_key
            .get(&join_key)
            .cloned()
            .ok_or(DurableSemanticProjectionFailure::MissingDefinition)?;
        if shell_by_key.insert(key, shell).is_some() {
            return Err(DurableSemanticProjectionFailure::DuplicateShell);
        }
    }
    let mut durable_by_key = BTreeMap::new();
    for record in durable {
        if durable_by_key.insert(record.key.clone(), record).is_some() {
            return Err(DurableSemanticProjectionFailure::DuplicateDefinition);
        }
    }
    let expected = shell_by_key.keys().cloned().collect::<BTreeSet<_>>();
    let supplied = durable_by_key.keys().cloned().collect::<BTreeSet<_>>();
    if let Some(key) = expected.difference(&supplied).next() {
        let _ = key;
        return Err(DurableSemanticProjectionFailure::MissingDefinition);
    }
    if supplied.difference(&expected).next().is_some() {
        return Err(DurableSemanticProjectionFailure::ExtraDefinition);
    }

    let mut exports = Vec::with_capacity(expected.len());
    for key in expected {
        let shell = shell_by_key[&key];
        let record = durable_by_key[&key];
        let file_id = *module_files
            .get(key.module())
            .ok_or(DurableSemanticProjectionFailure::ModuleMismatch)?;
        if shell.declaration_span.file_id != file_id
            || shell.identity.module_path.as_ref() != key.module().as_str()
        {
            return Err(DurableSemanticProjectionFailure::ModuleMismatch);
        }
        if record.is_public != shell.is_public {
            return Err(DurableSemanticProjectionFailure::VisibilityMismatch);
        }
        let payload = project_payload(&record.payload, definitions, &module_files)?;
        validate_payload_shape(shell, &payload)?;
        exports.push(SemanticDeclarationExport {
            identity: SemanticBinding {
                file_id,
                declaration_span: shell.declaration_span,
                namespace: shell.identity.namespace,
                kind: shell.identity.kind,
                name: shell.identity.name.clone(),
                owner: shell.identity.owner.clone(),
                is_public: shell.is_public,
            },
            payload,
        });
    }
    let work = DurableSemanticProjectionWork {
        projection_invocations: 1,
        shells_visited: shells.len(),
        durable_records_visited: durable.len(),
        definition_records_indexed: definitions.definitions().len(),
        definition_lookup_probes: shells.len(),
        rir_instructions_visited: 0,
    };
    Ok((exports.into(), work))
}

fn stable_namespace(value: SemanticBindingNamespace) -> crate::StableDefinitionNamespace {
    match value {
        SemanticBindingNamespace::Value => crate::StableDefinitionNamespace::Value,
        SemanticBindingNamespace::Type => crate::StableDefinitionNamespace::Type,
        SemanticBindingNamespace::Destructor => crate::StableDefinitionNamespace::Destructor,
        SemanticBindingNamespace::Method => crate::StableDefinitionNamespace::Method,
    }
}

fn stable_kind(value: SemanticBindingKind) -> Option<StableDefinitionKind> {
    Some(match value {
        SemanticBindingKind::Function => StableDefinitionKind::Function,
        SemanticBindingKind::Struct => StableDefinitionKind::Struct,
        SemanticBindingKind::Enum => StableDefinitionKind::Enum,
        SemanticBindingKind::ValueConst => StableDefinitionKind::ValueConst,
        SemanticBindingKind::ModuleBinding => StableDefinitionKind::ModuleBinding,
        SemanticBindingKind::Destructor => StableDefinitionKind::Destructor,
        SemanticBindingKind::Method => StableDefinitionKind::Method,
        SemanticBindingKind::AssociatedFunction => StableDefinitionKind::AssociatedFunction,
    })
}

fn current_nominal(
    key: &StableDefinitionKey,
    definitions: &BoundDefinitionSet,
    module_files: &std::collections::BTreeMap<ModuleId, FileId>,
) -> Result<rue_air::SemanticNominalIdentity, DurableSemanticProjectionFailure> {
    definitions
        .definition_by_key(key)
        .ok_or(DurableSemanticProjectionFailure::MissingDefinition)?;
    let kind = match key.kind() {
        StableDefinitionKind::Struct => SemanticBindingKind::Struct,
        StableDefinitionKind::Enum => SemanticBindingKind::Enum,
        _ => return Err(DurableSemanticProjectionFailure::KindMismatch),
    };
    Ok(rue_air::SemanticNominalIdentity {
        file_id: *module_files
            .get(key.module())
            .ok_or(DurableSemanticProjectionFailure::ModuleMismatch)?,
        name: Arc::from(key.name()),
        kind,
    })
}

fn project_type(
    value: &DurableType,
    definitions: &BoundDefinitionSet,
    module_files: &std::collections::BTreeMap<ModuleId, FileId>,
) -> Result<SemanticExportType, DurableSemanticProjectionFailure> {
    Ok(match value {
        DurableType::I8 => SemanticExportType::I8,
        DurableType::I16 => SemanticExportType::I16,
        DurableType::I32 => SemanticExportType::I32,
        DurableType::I64 => SemanticExportType::I64,
        DurableType::U8 => SemanticExportType::U8,
        DurableType::U16 => SemanticExportType::U16,
        DurableType::U32 => SemanticExportType::U32,
        DurableType::U64 => SemanticExportType::U64,
        DurableType::Bool => SemanticExportType::Bool,
        DurableType::Unit => SemanticExportType::Unit,
        DurableType::Never => SemanticExportType::Never,
        DurableType::ComptimeType => SemanticExportType::ComptimeType,
        DurableType::BuiltinNominal { name, kind } => {
            let kind = match kind {
                SemanticImportNominalKind::Struct => SemanticBindingKind::Struct,
                SemanticImportNominalKind::Enum => SemanticBindingKind::Enum,
            };
            SemanticExportType::Nominal(rue_air::SemanticNominalIdentity {
                file_id: FileId::new(0),
                name: name.clone(),
                kind,
            })
        }
        DurableType::GenericParameter(index) => SemanticExportType::GenericParameter(*index),
        DurableType::Nominal(key) => {
            SemanticExportType::Nominal(current_nominal(key, definitions, module_files)?)
        }
        DurableType::Array { element, len } => SemanticExportType::Array {
            element: Box::new(project_type(element, definitions, module_files)?),
            len: *len,
        },
        DurableType::PtrConst(value) => {
            SemanticExportType::PtrConst(Box::new(project_type(value, definitions, module_files)?))
        }
        DurableType::PtrMut(value) => {
            SemanticExportType::PtrMut(Box::new(project_type(value, definitions, module_files)?))
        }
        DurableType::Module(_) => {
            return Err(DurableSemanticProjectionFailure::UnsupportedType);
        }
    })
}

fn project_payload(
    value: &DurableDeclarationPayload,
    definitions: &BoundDefinitionSet,
    module_files: &std::collections::BTreeMap<ModuleId, FileId>,
) -> Result<SemanticDeclarationPayload, DurableSemanticProjectionFailure> {
    Ok(match value {
        DurableDeclarationPayload::Callable {
            parameters,
            result,
            has_self,
            is_unchecked,
        } => SemanticDeclarationPayload::Callable {
            parameters: parameters
                .iter()
                .map(|parameter| {
                    Ok(rue_air::SemanticExportParameter {
                        ty: project_type(&parameter.ty, definitions, module_files)?,
                        mode: match parameter.mode {
                            DurableParameterMode::Value => SemanticParameterMode::Value,
                            DurableParameterMode::Borrow => SemanticParameterMode::Borrow,
                            DurableParameterMode::Inout => SemanticParameterMode::Inout,
                        },
                        is_comptime: parameter.is_comptime,
                    })
                })
                .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?
                .into(),
            result: project_type(result, definitions, module_files)?,
            has_self: *has_self,
            is_unchecked: *is_unchecked,
        },
        DurableDeclarationPayload::Struct {
            fields,
            is_copy,
            is_linear,
        } => SemanticDeclarationPayload::Struct {
            fields: fields
                .iter()
                .map(|(name, ty)| Ok((name.clone(), project_type(ty, definitions, module_files)?)))
                .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?
                .into(),
            is_copy: *is_copy,
            is_linear: *is_linear,
        },
        DurableDeclarationPayload::Enum { variants } => SemanticDeclarationPayload::Enum {
            variants: variants
                .iter()
                .map(|(name, payload)| {
                    Ok((
                        name.clone(),
                        payload
                            .iter()
                            .map(|ty| project_type(ty, definitions, module_files))
                            .collect::<Result<Vec<_>, _>>()?
                            .into(),
                    ))
                })
                .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?
                .into(),
        },
        DurableDeclarationPayload::Destructor => SemanticDeclarationPayload::Destructor,
        DurableDeclarationPayload::Const { .. } => {
            return Err(DurableSemanticProjectionFailure::UnsupportedDeclaration);
        }
    })
}

fn validate_payload_shape(
    shell: &SemanticDeclarationShell,
    payload: &SemanticDeclarationPayload,
) -> Result<(), DurableSemanticProjectionFailure> {
    match (shell.identity.kind, payload) {
        (
            SemanticBindingKind::Function
            | SemanticBindingKind::Method
            | SemanticBindingKind::AssociatedFunction,
            SemanticDeclarationPayload::Callable {
                parameters,
                has_self,
                is_unchecked,
                ..
            },
        ) if parameters.len() == shell.parameter_names.len()
            && *has_self == shell.has_self
            && *is_unchecked == shell.is_unchecked =>
        {
            Ok(())
        }
        (SemanticBindingKind::Struct, SemanticDeclarationPayload::Struct { .. })
        | (SemanticBindingKind::Enum, SemanticDeclarationPayload::Enum { .. })
        | (SemanticBindingKind::Destructor, SemanticDeclarationPayload::Destructor) => Ok(()),
        _ => Err(DurableSemanticProjectionFailure::KindMismatch),
    }
}

/// Typed fail-closed reasons from successful-binding export.
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
            if identity.file_id == FileId::new(0) {
                let kind = match identity.kind {
                    SemanticBindingKind::Struct => SemanticImportNominalKind::Struct,
                    SemanticBindingKind::Enum => SemanticImportNominalKind::Enum,
                    _ => {
                        return Err(DurableSemanticExportFailure::MissingStableNominalDefinition);
                    }
                };
                if builtin_nominal_kind(&identity.name) != Some(kind) {
                    return Err(DurableSemanticExportFailure::MissingStableNominalDefinition);
                }
                return Ok(DurableType::BuiltinNominal {
                    name: identity.name.clone(),
                    kind,
                });
            }
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
            SemanticExportFailure::UnsupportedParameterMode
            | SemanticExportFailure::UnsupportedGenericSignature => Self::UnsupportedTypeForm,
            SemanticExportFailure::RecursiveStructuralType => Self::RecursiveStructuralType,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StableDefinitionNamespace;

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
    fn trusted_std_strbuf_survives_durable_import_with_aarch64_sret_identity() {
        let key = StableDefinitionKey::for_test(
            ModuleId::from_trusted_standard_library_path("\0rue-std/strbuf.rue").unwrap(),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
            "StrBuf",
            None,
        );
        let epoch = import_durable_declaration_semantics(&[DurableDeclarationSemantic {
            key,
            is_public: true,
            payload: DurableDeclarationPayload::Struct {
                fields: Arc::from([
                    (
                        Arc::from("buf"),
                        DurableType::PtrMut(Box::new(DurableType::U8)),
                    ),
                    (Arc::from("len"), DurableType::U64),
                    (Arc::from("cap"), DurableType::U64),
                ]),
                is_copy: false,
                is_linear: false,
            },
        }])
        .unwrap();
        let strbuf = epoch
            .type_pool()
            .all_struct_ids()
            .into_iter()
            .find(|id| epoch.type_pool().struct_lang_item(*id) == Some(rue_air::LangItem::StrBuf))
            .expect("durable import should restore StrBuf language-item identity");
        assert_eq!(epoch.type_pool().struct_symbol_name(strbuf), "StrBuf");
        assert!(rue_codegen::cfg_lower::type_uses_sret_return(
            epoch.type_pool(),
            rue_air::Type::new_struct(strbuf),
            8,
        ));
    }

    #[test]
    fn callable_parameter_order_is_semantic() {
        let parameter = |ty| DurableSemanticParameter {
            ty,
            mode: DurableParameterMode::Value,
            is_comptime: false,
        };
        let a = DurableDeclarationPayload::Callable {
            parameters: Arc::from([parameter(DurableType::Bool), parameter(DurableType::I32)]),
            result: DurableType::Unit,
            has_self: false,
            is_unchecked: false,
        };
        let b = DurableDeclarationPayload::Callable {
            parameters: Arc::from([parameter(DurableType::I32), parameter(DurableType::Bool)]),
            result: DurableType::Unit,
            has_self: false,
            is_unchecked: false,
        };
        assert_ne!(a, b);

        let first = DurableConstValue::Type(DurableType::Array {
            element: Box::new(DurableType::PtrConst(Box::new(DurableType::U8))),
            len: 4,
        });
        assert_eq!(first.clone(), first);
    }

    fn test_key(
        kind: StableDefinitionKind,
        name: &str,
        owner: Option<&str>,
    ) -> StableDefinitionKey {
        StableDefinitionKey::for_test(
            ModuleId::from_logical_path("pkg/main.rue").unwrap(),
            match kind {
                StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction => {
                    StableDefinitionNamespace::Method
                }
                _ => StableDefinitionNamespace::Value,
            },
            kind,
            Arc::from(name),
            owner.map(|name| (StableDefinitionKind::Struct, Arc::from(name))),
        )
    }

    fn callable(key: StableDefinitionKey) -> DurableDeclarationSemantic {
        DurableDeclarationSemantic {
            key,
            is_public: true,
            payload: DurableDeclarationPayload::Callable {
                parameters: Arc::from([]),
                result: DurableType::Unit,
                has_self: false,
                is_unchecked: false,
            },
        }
    }

    #[test]
    fn durable_callable_rejects_generic_indices_outside_its_type_parameters() {
        let declaration = DurableDeclarationSemantic {
            key: test_key(StableDefinitionKind::Function, "invalid", None),
            is_public: true,
            payload: DurableDeclarationPayload::Callable {
                parameters: Arc::from([
                    DurableSemanticParameter {
                        ty: DurableType::ComptimeType,
                        mode: DurableParameterMode::Value,
                        is_comptime: true,
                    },
                    DurableSemanticParameter {
                        ty: DurableType::GenericParameter(1),
                        mode: DurableParameterMode::Value,
                        is_comptime: false,
                    },
                ]),
                result: DurableType::GenericParameter(0),
                has_self: false,
                is_unchecked: false,
            },
        };
        assert!(matches!(
            import_durable_declaration_semantics(&[declaration]),
            Err(SemanticImportFailure::GenericParameterOutOfRange)
        ));
    }

    #[test]
    fn sibling_owned_callables_have_distinct_symbols_and_exact_round_trip() {
        let left = test_key(
            StableDefinitionKind::AssociatedFunction,
            "make",
            Some("Left"),
        );
        let right = test_key(
            StableDefinitionKind::AssociatedFunction,
            "make",
            Some("Right"),
        );
        let epoch = import_durable_declaration_semantics(&[
            callable(left.clone()),
            callable(right.clone()),
        ])
        .unwrap();

        let left_value = epoch
            .import_const_value(&SemanticImportConstValue::Function(left.clone()))
            .unwrap();
        let right_value = epoch
            .import_const_value(&SemanticImportConstValue::Function(right.clone()))
            .unwrap();
        assert_ne!(left_value, right_value);
        assert_eq!(
            epoch.export_const_value(left_value).unwrap(),
            SemanticImportConstValue::Function(left)
        );
        assert_eq!(
            epoch.export_const_value(right_value).unwrap(),
            SemanticImportConstValue::Function(right)
        );
    }

    #[test]
    fn callable_kind_is_part_of_the_local_identity() {
        let method = StableDefinitionKey::for_test(
            ModuleId::from_logical_path("pkg/main.rue").unwrap(),
            StableDefinitionNamespace::Method,
            StableDefinitionKind::Method,
            "same",
            Some((StableDefinitionKind::Struct, Arc::from("Owner"))),
        );
        let associated = StableDefinitionKey::for_test(
            ModuleId::from_logical_path("pkg/main.rue").unwrap(),
            StableDefinitionNamespace::Method,
            StableDefinitionKind::AssociatedFunction,
            "same",
            Some((StableDefinitionKind::Struct, Arc::from("Owner"))),
        );
        let epoch = import_durable_declaration_semantics(&[
            callable(method.clone()),
            callable(associated.clone()),
        ])
        .unwrap();
        let method_value = epoch
            .import_const_value(&SemanticImportConstValue::Function(method.clone()))
            .unwrap();
        let associated_value = epoch
            .import_const_value(&SemanticImportConstValue::Function(associated.clone()))
            .unwrap();
        assert_ne!(method_value, associated_value);
        assert_eq!(
            epoch.export_const_value(method_value).unwrap(),
            SemanticImportConstValue::Function(method)
        );
        assert_eq!(
            epoch.export_const_value(associated_value).unwrap(),
            SemanticImportConstValue::Function(associated)
        );
    }

    #[test]
    fn import_rejects_payload_kind_mismatch_before_building_an_epoch() {
        let declaration = DurableDeclarationSemantic {
            key: test_key(StableDefinitionKind::Function, "wrong", None),
            is_public: true,
            payload: DurableDeclarationPayload::Struct {
                fields: Arc::from([]),
                is_copy: false,
                is_linear: false,
            },
        };
        assert!(matches!(
            import_durable_declaration_semantics(&[declaration]),
            Err(SemanticImportFailure::DeclarationKindMismatch)
        ));

        let module_binding = DurableDeclarationSemantic {
            key: test_key(StableDefinitionKind::ModuleBinding, "module", None),
            is_public: true,
            payload: DurableDeclarationPayload::Const {
                ty: DurableType::Module(ModuleId::from_logical_path("pkg/dep.rue").unwrap()),
                value: DurableConstValue::Unit,
            },
        };
        assert!(matches!(
            import_durable_declaration_semantics(&[module_binding]),
            Err(SemanticImportFailure::DeclarationKindMismatch)
        ));
    }
}
