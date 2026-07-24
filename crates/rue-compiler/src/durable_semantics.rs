//! Request-independent semantic values used at compiler query boundaries.
//!
//! These types deliberately have no conversion from `rue_air::Type`. Such a
//! conversion is only sound while the successful declaration binder, its type
//! pool, and the exact-revision stable-definition join are available together.

use std::sync::Arc;

#[cfg(test)]
use rue_air::SemanticExportFailure;
use rue_air::{
    SemanticBinding, SemanticDeclarationExport, SemanticDeclarationPayload,
    SemanticDeclarationShell, SemanticExportConstValue, SemanticExportType,
    SemanticImportConstValue, SemanticImportEpoch, SemanticImportFailure, SemanticImportNominal,
    SemanticImportNominalKind, SemanticImportType, SemanticParameterMode,
    StableDefinitionNamespace,
};
use rue_span::FileId;

/// Reconstruct the representable declaration universe in a new AIR epoch.
///
/// Nominal shells are issued in stable-key order before any field or variant
/// type is imported, so mutually recursive pointer graphs are supported. The
/// returned epoch contains no handles from the exporting semantic request.
pub fn import_durable_declaration_semantics(
    declarations: &[DurableDeclarationSemantic],
) -> Result<SemanticImportEpoch<StableDefinitionKey, Arc<str>>, SemanticImportFailure> {
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
            ) | (
                StableDefinitionKind::ModuleBinding,
                DurableDeclarationPayload::ModuleBinding { .. }
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
            | DurableType::Slice { element, .. }
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
            DurableDeclarationPayload::ModuleBinding { target } => {
                modules.insert(Arc::from(target.as_str()));
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
                                .map(DurableTypeProjection::import_dto)
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
            DurableDeclarationPayload::ModuleBinding { target } => {
                epoch.import_type(&DurableType::Module(target.clone()).import_dto())?;
            }
        }
    }
    Ok(epoch)
}

use crate::{
    BoundDefinitionSet, CanonicalMergedProgram, ModuleId, StableDefinitionKey, StableDefinitionKind,
};

/// Version of the canonical durable type/value algebra and its in-process
/// implementation namespace. Compatibility is explicit and fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableSemanticSchemaVersion {
    pub major: u16,
    pub minor: u16,
    pub implementation_epoch: u32,
}

pub const DURABLE_SEMANTIC_SCHEMA_VERSION: DurableSemanticSchemaVersion =
    DurableSemanticSchemaVersion {
        major: 1,
        minor: 0,
        // Epoch 6: slice identity carries its recursively resolved element
        // type instead of collapsing to a synthetic builtin name.
        implementation_epoch: 6,
    };

const DURABLE_SEMANTIC_COMPATIBLE_MINORS: &[u16] = &[0];

impl DurableSemanticSchemaVersion {
    pub fn accepts(self, candidate: Self) -> bool {
        self.major == candidate.major
            && self.implementation_epoch == candidate.implementation_epoch
            && candidate.minor <= self.minor
            && DURABLE_SEMANTIC_COMPATIBLE_MINORS.contains(&candidate.minor)
    }
}

pub(crate) fn builtin_nominal_kind(name: &str) -> Option<SemanticImportNominalKind> {
    if name == "str" {
        Some(SemanticImportNominalKind::Struct)
    } else if rue_builtins::get_builtin_enum(name).is_some() {
        Some(SemanticImportNominalKind::Enum)
    } else {
        None
    }
}

/// The durable specialization of rue-air's canonical type algebra.
pub type DurableType = SemanticImportType<StableDefinitionKey, ModuleId>;

/// The durable specialization of rue-air's canonical constant algebra.
pub type DurableConstValue = SemanticImportConstValue<StableDefinitionKey, ModuleId>;

/// Query-owned materialization payload for one anonymous nominal referenced by
/// declaration semantics. Identity remains separate from shape so recursive
/// uses and structurally equal aliases can be joined before fields are filled.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableAnonymousNominal {
    pub identity: crate::AnonymousNominalKey,
    pub shape: DurableAnonymousNominalShape,
    pub type_captures: Arc<[(Arc<str>, DurableType)]>,
    pub value_captures: Arc<[(Arc<str>, DurableConstValue)]>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableAnonymousNominalShape {
    Struct {
        fields: Arc<[(Arc<str>, DurableType)]>,
        methods: Arc<[DurableAnonymousMethodSignature]>,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[DurableType]>)]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableAnonymousMethodSignature {
    pub name: Arc<str>,
    pub has_self: bool,
    pub self_mode: DurableParameterMode,
    pub parameters: Arc<[(DurableAnonymousMethodType, DurableParameterMode, bool)]>,
    pub result: DurableAnonymousMethodType,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableAnonymousMethodType {
    SelfType,
    Concrete(DurableType),
}

/// Relocation into the string-module body algebra is intentionally expressed
/// through the canonical rue-air traversal, not a compiler-owned enum match.
pub(crate) trait DurableTypeProjection {
    fn import_dto(&self) -> SemanticImportType<StableDefinitionKey, Arc<str>>;
}

impl DurableTypeProjection for DurableType {
    fn import_dto(&self) -> SemanticImportType<StableDefinitionKey, Arc<str>> {
        self.try_map_identities(
            &|key| Ok::<_, std::convert::Infallible>(key.clone()),
            &|module| Ok(Arc::from(module.as_str())),
        )
        .expect("durable identity relocation is infallible")
    }
}

pub(crate) trait DurableConstValueProjection {
    fn import_dto(&self) -> SemanticImportConstValue<StableDefinitionKey, Arc<str>>;
}

impl DurableConstValueProjection for DurableConstValue {
    fn import_dto(&self) -> SemanticImportConstValue<StableDefinitionKey, Arc<str>> {
        self.try_map_identities(
            &|key| Ok::<_, std::convert::Infallible>(key.clone()),
            &|module| Ok(Arc::from(module.as_str())),
        )
        .expect("durable identity relocation is infallible")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableParameterMode {
    Value,
    Borrow,
    Inout,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableSemanticParameter {
    /// The parameter's source name. Carried on the durable declaration so a
    /// consumer that reconstructs a comptime type-constructor head from the
    /// signature alone (the provider-driven analyzer's `SignatureFacts`) can
    /// bind arguments by name without re-consulting the declaration shell.
    pub name: Arc<str>,
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
    /// The resolved canonical target of a top-level module-valued constant.
    ModuleBinding {
        target: ModuleId,
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
    DuplicateShell,
    AmbiguousDefinition,
    KindMismatch,
    ModuleMismatch,
    VisibilityMismatch,
    UnsupportedDeclaration,
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

    let mut durable_by_key = BTreeMap::new();
    for record in durable {
        if durable_by_key.insert(record.key.clone(), record).is_some() {
            return Err(DurableSemanticProjectionFailure::DuplicateDefinition);
        }
    }
    let durable_by_join_key = durable_by_key
        .keys()
        .map(|key| (ProjectionJoinKey::from_definition(key), key.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut shell_by_key = BTreeMap::new();
    for shell in shells {
        let join_key = ProjectionJoinKey::from_shell(shell)
            .ok_or(DurableSemanticProjectionFailure::UnsupportedDeclaration)?;
        let exact_definition = definition_by_join_key.get(&join_key).cloned();
        let exact_durable = exact_definition
            .as_ref()
            .filter(|key| durable_by_key.contains_key(*key))
            .cloned();
        let key = if let Some(key) = exact_durable {
            key
        } else if shell.identity.kind == StableDefinitionKind::ValueConst {
            // Const shells are deliberately captured before initializer
            // evaluation, and therefore have no provisional stable definition.
            // A selected compatible durable baseline may reclassify only this
            // exact current candidate as a module binding. The durable query's
            // input fingerprints prove initializer compatibility; this adapter
            // supplies only the current locator and header.
            let value_key = durable_by_join_key.get(&join_key).cloned();
            let mut module_join = join_key.clone();
            module_join.kind = StableDefinitionKind::ModuleBinding;
            match (value_key, durable_by_join_key.get(&module_join).cloned()) {
                (Some(key), None)
                    if matches!(
                        durable_by_key[&key].payload,
                        DurableDeclarationPayload::Const { .. }
                    ) =>
                {
                    key
                }
                (None, Some(key))
                    if matches!(
                        durable_by_key[&key].payload,
                        DurableDeclarationPayload::ModuleBinding { .. }
                    ) =>
                {
                    key
                }
                (Some(_), Some(_)) => {
                    return Err(DurableSemanticProjectionFailure::DuplicateDefinition);
                }
                _ => return Err(DurableSemanticProjectionFailure::KindMismatch),
            }
        } else {
            return Err(DurableSemanticProjectionFailure::MissingDefinition);
        };
        if shell_by_key.insert(key, shell).is_some() {
            return Err(DurableSemanticProjectionFailure::DuplicateShell);
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
                kind: if key.kind() == StableDefinitionKind::ModuleBinding {
                    StableDefinitionKind::ModuleBinding
                } else {
                    shell.identity.kind
                },
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

fn stable_namespace(value: StableDefinitionNamespace) -> crate::StableDefinitionNamespace {
    match value {
        StableDefinitionNamespace::Value => crate::StableDefinitionNamespace::Value,
        StableDefinitionNamespace::Type => crate::StableDefinitionNamespace::Type,
        StableDefinitionNamespace::Destructor => crate::StableDefinitionNamespace::Destructor,
        StableDefinitionNamespace::Method => crate::StableDefinitionNamespace::Method,
    }
}

fn stable_kind(value: StableDefinitionKind) -> Option<StableDefinitionKind> {
    Some(match value {
        StableDefinitionKind::Function => StableDefinitionKind::Function,
        StableDefinitionKind::Struct => StableDefinitionKind::Struct,
        StableDefinitionKind::Enum => StableDefinitionKind::Enum,
        StableDefinitionKind::ValueConst => StableDefinitionKind::ValueConst,
        StableDefinitionKind::ModuleBinding => StableDefinitionKind::ModuleBinding,
        StableDefinitionKind::Destructor => StableDefinitionKind::Destructor,
        StableDefinitionKind::Method => StableDefinitionKind::Method,
        StableDefinitionKind::AssociatedFunction => StableDefinitionKind::AssociatedFunction,
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
        StableDefinitionKind::Struct => StableDefinitionKind::Struct,
        StableDefinitionKind::Enum => StableDefinitionKind::Enum,
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

fn current_definition_identity(
    value: &StableDefinitionKey,
    definitions: &BoundDefinitionSet,
    module_files: &std::collections::BTreeMap<ModuleId, FileId>,
) -> Result<rue_air::SemanticDefinitionIdentity, DurableSemanticProjectionFailure> {
    let present = definitions
        .definitions()
        .iter()
        .any(|record| record.stable_key() == value);
    if !present
        && !matches!(
            value.kind(),
            StableDefinitionKind::ValueConst | StableDefinitionKind::ModuleBinding
        )
    {
        return Err(DurableSemanticProjectionFailure::MissingDefinition);
    }
    Ok(rue_air::SemanticDefinitionIdentity {
        file_id: *module_files
            .get(value.module())
            .ok_or(DurableSemanticProjectionFailure::ModuleMismatch)?,
        name: Arc::from(value.name()),
        owner: value.owner().map(|owner| Arc::from(owner.name())),
        kind: stable_air_kind(value.kind()),
    })
}

fn stable_air_kind(value: StableDefinitionKind) -> rue_air::StableDefinitionKind {
    match value {
        StableDefinitionKind::Function => rue_air::StableDefinitionKind::Function,
        StableDefinitionKind::Struct => rue_air::StableDefinitionKind::Struct,
        StableDefinitionKind::Enum => rue_air::StableDefinitionKind::Enum,
        StableDefinitionKind::ValueConst => rue_air::StableDefinitionKind::ValueConst,
        StableDefinitionKind::Destructor => rue_air::StableDefinitionKind::Destructor,
        StableDefinitionKind::Method => rue_air::StableDefinitionKind::Method,
        StableDefinitionKind::AssociatedFunction => {
            rue_air::StableDefinitionKind::AssociatedFunction
        }
        StableDefinitionKind::ModuleBinding => rue_air::StableDefinitionKind::ModuleBinding,
    }
}

fn semantic_parameter_mode(value: DurableParameterMode) -> rue_air::SemanticParameterMode {
    match value {
        DurableParameterMode::Value => rue_air::SemanticParameterMode::Value,
        DurableParameterMode::Borrow => rue_air::SemanticParameterMode::Borrow,
        DurableParameterMode::Inout => rue_air::SemanticParameterMode::Inout,
    }
}

fn current_anonymous_identity(
    value: &crate::AnonymousNominalKey,
    definitions: &BoundDefinitionSet,
    module_files: &std::collections::BTreeMap<ModuleId, FileId>,
) -> Result<rue_air::SemanticAnonymousNominalIdentity, DurableSemanticProjectionFailure> {
    value.try_map_identities(
        &|definition| current_definition_identity(definition, definitions, module_files),
        &|module| {
            module_files
                .contains_key(module)
                .then(|| Arc::from(module.as_str()))
                .ok_or(DurableSemanticProjectionFailure::ModuleMismatch)
        },
    )
}

fn project_type(
    value: &DurableType,
    definitions: &BoundDefinitionSet,
    module_files: &std::collections::BTreeMap<ModuleId, FileId>,
) -> Result<SemanticExportType, DurableSemanticProjectionFailure> {
    use rue_air::SemanticImportTypeFold as F;
    value.try_fold(&mut |node| {
        Ok(match node {
            F::I8 => SemanticExportType::I8,
            F::I16 => SemanticExportType::I16,
            F::I32 => SemanticExportType::I32,
            F::I64 => SemanticExportType::I64,
            F::U8 => SemanticExportType::U8,
            F::U16 => SemanticExportType::U16,
            F::U32 => SemanticExportType::U32,
            F::U64 => SemanticExportType::U64,
            F::Bool => SemanticExportType::Bool,
            F::Unit => SemanticExportType::Unit,
            F::Never => SemanticExportType::Never,
            F::ComptimeType => SemanticExportType::ComptimeType,
            F::BuiltinNominal { name, kind } => SemanticExportType::BuiltinNominal {
                name: name.clone(),
                kind,
            },
            F::GenericParameter(index) => SemanticExportType::GenericParameter(index),
            F::Nominal(key) => {
                SemanticExportType::Nominal(current_nominal(key, definitions, module_files)?)
            }
            // Durable declaration shells never publish request-local
            // anonymous nominals. Their identity is supported by body and
            // specialization payloads, whose current-request importer owns
            // the exact materialization join.
            F::AnonymousNominal(identity) => SemanticExportType::AnonymousNominal(
                current_anonymous_identity(identity, definitions, module_files)?,
            ),
            F::Array { element, len } => SemanticExportType::Array {
                element: Box::new(element),
                len,
            },
            F::Slice { element, name } => SemanticExportType::Slice {
                element: Box::new(element),
                name: name.clone(),
            },
            F::PtrConst(value) => SemanticExportType::PtrConst(Box::new(value)),
            F::PtrMut(value) => SemanticExportType::PtrMut(Box::new(value)),
            F::Module(module) => module_files
                .contains_key(module)
                .then(|| SemanticExportType::Module(Arc::from(module.as_str())))
                .ok_or(DurableSemanticProjectionFailure::ModuleMismatch)?,
        })
    })
}

pub(crate) fn project_durable_anonymous_nominals(
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
    anonymous_nominals: &[DurableAnonymousNominal],
) -> Result<Arc<[rue_air::SemanticAnonymousNominalExport]>, DurableSemanticProjectionFailure> {
    let module_files = merged
        .ast()
        .modules()
        .iter()
        .map(|module| (module.module_id().clone(), module.file_id()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut projected = Vec::with_capacity(anonymous_nominals.len());
    for nominal in anonymous_nominals {
        let identity = current_anonymous_identity(&nominal.identity, definitions, &module_files)?;
        let shape = match &nominal.shape {
            DurableAnonymousNominalShape::Struct { fields, methods } => {
                rue_air::SemanticAnonymousNominalShape::Struct {
                    fields: fields
                        .iter()
                        .map(|(name, ty)| {
                            Ok((name.clone(), project_type(ty, definitions, &module_files)?))
                        })
                        .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?
                        .into(),
                    methods: methods
                        .iter()
                        .map(|method| {
                            let project_method_type = |ty: &DurableAnonymousMethodType| {
                                Ok(match ty {
                                    DurableAnonymousMethodType::SelfType => {
                                        rue_air::SemanticAnonymousMethodType::SelfType
                                    }
                                    DurableAnonymousMethodType::Concrete(ty) => {
                                        rue_air::SemanticAnonymousMethodType::Concrete(
                                            project_type(ty, definitions, &module_files)?,
                                        )
                                    }
                                })
                            };
                            Ok(rue_air::SemanticAnonymousMethodSignature {
                                name: method.name.clone(),
                                has_self: method.has_self,
                                self_mode: semantic_parameter_mode(method.self_mode),
                                parameters: method
                                    .parameters
                                    .iter()
                                    .map(|(ty, mode, is_comptime)| {
                                        Ok((
                                            project_method_type(ty)?,
                                            semantic_parameter_mode(*mode),
                                            *is_comptime,
                                        ))
                                    })
                                    .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?
                                    .into(),
                                result: project_method_type(&method.result)?,
                            })
                        })
                        .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?
                        .into(),
                }
            }
            DurableAnonymousNominalShape::Enum { variants } => {
                rue_air::SemanticAnonymousNominalShape::Enum {
                    variants: variants
                        .iter()
                        .map(|(name, payload)| {
                            Ok((
                                name.clone(),
                                payload
                                    .iter()
                                    .map(|ty| project_type(ty, definitions, &module_files))
                                    .collect::<Result<Vec<_>, _>>()?
                                    .into(),
                            ))
                        })
                        .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?
                        .into(),
                }
            }
        };
        let type_captures = nominal
            .type_captures
            .iter()
            .map(|(name, ty)| Ok((name.clone(), project_type(ty, definitions, &module_files)?)))
            .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?
            .into();
        let value_captures = nominal
            .value_captures
            .iter()
            .map(|(name, value)| {
                Ok((
                    name.clone(),
                    project_const_value(value, definitions, &module_files)?,
                ))
            })
            .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?
            .into();
        projected.push(rue_air::SemanticAnonymousNominalExport {
            identity,
            shape,
            type_captures,
            value_captures,
        });
    }
    Ok(projected.into())
}

/// Project a per-body well-known `Option` resolution (RUE-1112) into the AIR
/// export algebra: the anonymous enum nominals to materialize narrowly, plus
/// each `(payload, option_enum)` type pair for the demand registry. Projection
/// runs against the same bound definition set as the ordinary anonymous
/// nominals, so the trusted `Option` producer/module identities resolve exactly.
pub(crate) fn project_durable_option_registry(
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
    resolution: &crate::body_query::WellKnownOptionResolution,
) -> Result<
    (
        Arc<[rue_air::SemanticAnonymousNominalExport]>,
        Vec<(rue_air::SemanticExportType, rue_air::SemanticExportType)>,
    ),
    DurableSemanticProjectionFailure,
> {
    let module_files = merged
        .ast()
        .modules()
        .iter()
        .map(|module| (module.module_id().clone(), module.file_id()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let nominals =
        project_durable_anonymous_nominals(merged, definitions, &resolution.anonymous_nominals)?;
    let option_by_payload = resolution
        .option_by_payload
        .iter()
        .map(|(payload, option)| {
            Ok((
                project_type(payload, definitions, &module_files)?,
                project_type(option, definitions, &module_files)?,
            ))
        })
        .collect::<Result<Vec<_>, DurableSemanticProjectionFailure>>()?;
    Ok((nominals, option_by_payload))
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
                        name: parameter.name.clone(),
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
        DurableDeclarationPayload::ModuleBinding { target } => {
            SemanticDeclarationPayload::ModuleBinding {
                target: project_type(
                    &DurableType::Module(target.clone()),
                    definitions,
                    module_files,
                )?,
            }
        }
        DurableDeclarationPayload::Const { ty, value } => SemanticDeclarationPayload::Const {
            ty: project_type(ty, definitions, module_files)?,
            value: project_const_value(value, definitions, module_files)?,
        },
    })
}

fn project_const_value(
    value: &DurableConstValue,
    definitions: &BoundDefinitionSet,
    module_files: &std::collections::BTreeMap<ModuleId, FileId>,
) -> Result<SemanticExportConstValue, DurableSemanticProjectionFailure> {
    Ok(match value {
        DurableConstValue::Integer(value) => SemanticExportConstValue::Integer(*value),
        DurableConstValue::Bool(value) => SemanticExportConstValue::Bool(*value),
        DurableConstValue::Type(value) => {
            SemanticExportConstValue::Type(project_type(value, definitions, module_files)?)
        }
        DurableConstValue::Function(key) => {
            let definition = definitions
                .definition_by_key(key)
                .ok_or(DurableSemanticProjectionFailure::MissingDefinition)?;
            if key.kind() != StableDefinitionKind::Function {
                return Err(DurableSemanticProjectionFailure::KindMismatch);
            }
            SemanticExportConstValue::Function {
                file_id: definition.declaration_span().file_id,
                name: key.name().into(),
            }
        }
        DurableConstValue::Unit => SemanticExportConstValue::Unit,
        DurableConstValue::String(value) => SemanticExportConstValue::String(value.clone()),
    })
}

fn validate_payload_shape(
    shell: &SemanticDeclarationShell,
    payload: &SemanticDeclarationPayload,
) -> Result<(), DurableSemanticProjectionFailure> {
    match (shell.identity.kind, payload) {
        (
            StableDefinitionKind::Function
            | StableDefinitionKind::Method
            | StableDefinitionKind::AssociatedFunction,
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
        (StableDefinitionKind::Struct, SemanticDeclarationPayload::Struct { .. })
        | (StableDefinitionKind::Enum, SemanticDeclarationPayload::Enum { .. })
        | (StableDefinitionKind::ValueConst, SemanticDeclarationPayload::Const { .. })
        | (
            StableDefinitionKind::ValueConst | StableDefinitionKind::ModuleBinding,
            SemanticDeclarationPayload::ModuleBinding { .. },
        )
        | (StableDefinitionKind::Destructor, SemanticDeclarationPayload::Destructor) => Ok(()),
        _ => Err(DurableSemanticProjectionFailure::KindMismatch),
    }
}

/// Typed fail-closed reasons from successful-binding export.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableSemanticExportFailure {
    ErrorType,
    MissingStableNominalDefinition,
    MissingStableFunctionDefinition,
    UnresolvedModule,
    AnonymousNominalType,
    UnsupportedTypeForm,
    RecursiveStructuralType,
}

#[cfg(test)]
pub(crate) fn durable_module_type(
    path: &str,
    merged: &CanonicalMergedProgram,
) -> Result<DurableType, DurableSemanticExportFailure> {
    let module = merged
        .ast()
        .modules()
        .iter()
        .find(|module| module.module_id().as_str() == path)
        .map(|module| module.module_id().clone())
        .ok_or(DurableSemanticExportFailure::UnresolvedModule)?;
    Ok(DurableType::Module(module))
}

#[cfg(test)]
pub(crate) fn convert_declaration_semantics(
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
    exports: &[SemanticDeclarationExport],
) -> Result<Arc<[DurableDeclarationSemantic]>, DurableSemanticExportFailure> {
    fn stable_kind(kind: StableDefinitionKind) -> Option<StableDefinitionKind> {
        Some(match kind {
            StableDefinitionKind::Function => StableDefinitionKind::Function,
            StableDefinitionKind::Struct => StableDefinitionKind::Struct,
            StableDefinitionKind::Enum => StableDefinitionKind::Enum,
            StableDefinitionKind::ValueConst => StableDefinitionKind::ValueConst,
            StableDefinitionKind::Destructor => StableDefinitionKind::Destructor,
            StableDefinitionKind::Method => StableDefinitionKind::Method,
            StableDefinitionKind::AssociatedFunction => StableDefinitionKind::AssociatedFunction,
            StableDefinitionKind::ModuleBinding => StableDefinitionKind::ModuleBinding,
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
            StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction => {
                owner.is_some()
            }
            StableDefinitionKind::Destructor => owner == Some(name),
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
                StableDefinitionKind::Struct => StableDefinitionKind::Struct,
                StableDefinitionKind::Enum => StableDefinitionKind::Enum,
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
            SemanticExportType::BuiltinNominal { name, kind } => {
                if builtin_nominal_kind(name) != Some(*kind) {
                    return Err(DurableSemanticExportFailure::MissingStableNominalDefinition);
                }
                DurableType::BuiltinNominal {
                    name: name.clone(),
                    kind: *kind,
                }
            }
            SemanticExportType::Nominal(n) => nominal(n)?,
            SemanticExportType::AnonymousNominal(_) => {
                return Err(DurableSemanticExportFailure::AnonymousNominalType);
            }
            SemanticExportType::Array { element, len } => DurableType::Array {
                element: Box::new(ty(element, merged, definitions)?),
                len: *len,
            },
            SemanticExportType::Slice { element, name } => DurableType::Slice {
                element: Box::new(ty(element, merged, definitions)?),
                name: name.clone(),
            },
            SemanticExportType::PtrConst(v) => {
                DurableType::PtrConst(Box::new(ty(v, merged, definitions)?))
            }
            SemanticExportType::PtrMut(v) => {
                DurableType::PtrMut(Box::new(ty(v, merged, definitions)?))
            }
            SemanticExportType::Module(path) => durable_module_type(path, merged)?,
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
                            name: p.name.clone(),
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
                    SemanticExportConstValue::String(content) => {
                        DurableConstValue::String(content.clone())
                    }
                    SemanticExportConstValue::Function { file_id, name } => {
                        DurableConstValue::Function(
                            key_for(*file_id, name, StableDefinitionKind::Function, None).ok_or(
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
            SemanticDeclarationPayload::ModuleBinding { target } => {
                let DurableType::Module(target) = ty(target, merged, definitions)? else {
                    return Err(DurableSemanticExportFailure::UnresolvedModule);
                };
                DurableDeclarationPayload::ModuleBinding { target }
            }
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

#[cfg(test)]
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
    use std::collections::HashSet;
    use std::hash::{DefaultHasher, Hash, Hasher};

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
    fn canonical_schema_policy_rejects_unknown_major_minor_and_implementation_epochs() {
        let current = DURABLE_SEMANTIC_SCHEMA_VERSION;
        assert!(current.accepts(current));
        assert!(!current.accepts(DurableSemanticSchemaVersion {
            major: current.major + 1,
            ..current
        }));
        assert!(!current.accepts(DurableSemanticSchemaVersion {
            minor: current.minor + 1,
            ..current
        }));
        assert!(!current.accepts(DurableSemanticSchemaVersion {
            implementation_epoch: current.implementation_epoch + 1,
            ..current
        }));
    }

    fn stable_hash<T: Hash>(value: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn durable_types_are_canonical_aliases_without_peer_enum_definitions() {
        let source = include_str!("durable_semantics.rs");
        assert!(
            source.contains(
                "pub type DurableType = SemanticImportType<StableDefinitionKey, ModuleId>;"
            )
        );
        assert!(source.contains(
            "pub type DurableConstValue = SemanticImportConstValue<StableDefinitionKey, ModuleId>;"
        ));
        let peer_type = ["pub enum Durable", "Type"].concat();
        let peer_value = ["pub enum Durable", "ConstValue"].concat();
        assert!(!source.contains(&peer_type));
        assert!(!source.contains(&peer_value));
        let ty: SemanticImportType<StableDefinitionKey, ModuleId> = DurableType::I32;
        let value: SemanticImportConstValue<StableDefinitionKey, ModuleId> =
            DurableConstValue::Type(ty);
        assert_eq!(
            value,
            SemanticImportConstValue::Type(SemanticImportType::I32)
        );
    }

    #[test]
    fn bounded_durable_types_round_trip_across_fresh_epochs() {
        let module = ModuleId::from_logical_path("pkg/main.rue").unwrap();
        let dependency = ModuleId::from_logical_path("pkg/dep.rue").unwrap();
        let structure = StableDefinitionKey::for_test(
            module.clone(),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
            "Record",
            None,
        );
        let enumeration = StableDefinitionKey::for_test(
            module.clone(),
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Enum,
            "Choice",
            None,
        );
        let callable_key = StableDefinitionKey::for_test(
            module,
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "generic",
            None,
        );
        let declarations = vec![
            DurableDeclarationSemantic {
                key: structure.clone(),
                is_public: true,
                payload: DurableDeclarationPayload::Struct {
                    fields: Arc::from([(
                        Arc::from("choice"),
                        DurableType::PtrConst(Box::new(DurableType::Nominal(enumeration.clone()))),
                    )]),
                    is_copy: false,
                    is_linear: false,
                },
            },
            DurableDeclarationSemantic {
                key: enumeration.clone(),
                is_public: true,
                payload: DurableDeclarationPayload::Enum {
                    variants: Arc::from([(
                        Arc::from("Only"),
                        Arc::from([
                            DurableType::Array {
                                element: Box::new(DurableType::PtrMut(Box::new(
                                    DurableType::Nominal(structure.clone()),
                                ))),
                                len: 2,
                            },
                            DurableType::PtrConst(Box::new(DurableType::BuiltinNominal {
                                name: Arc::from("str"),
                                kind: SemanticImportNominalKind::Struct,
                            })),
                        ]),
                    )]),
                },
            },
            DurableDeclarationSemantic {
                key: callable_key,
                is_public: true,
                payload: DurableDeclarationPayload::Callable {
                    parameters: Arc::from([
                        DurableSemanticParameter {
                            name: Arc::from("T"),
                            ty: DurableType::ComptimeType,
                            mode: DurableParameterMode::Value,
                            is_comptime: true,
                        },
                        DurableSemanticParameter {
                            name: Arc::from("value"),
                            ty: DurableType::GenericParameter(0),
                            mode: DurableParameterMode::Value,
                            is_comptime: false,
                        },
                        DurableSemanticParameter {
                            name: Arc::from("dep"),
                            ty: DurableType::Module(dependency.clone()),
                            mode: DurableParameterMode::Value,
                            is_comptime: false,
                        },
                    ]),
                    result: DurableType::Unit,
                    has_self: false,
                    is_unchecked: false,
                },
            },
        ];
        let first = import_durable_declaration_semantics(&declarations).unwrap();
        let second = import_durable_declaration_semantics(&declarations).unwrap();

        let mut all = vec![
            DurableType::I8,
            DurableType::I16,
            DurableType::I32,
            DurableType::I64,
            DurableType::U8,
            DurableType::U16,
            DurableType::U32,
            DurableType::U64,
            DurableType::Bool,
            DurableType::Unit,
            DurableType::Never,
            DurableType::ComptimeType,
            DurableType::BuiltinNominal {
                name: Arc::from("str"),
                kind: SemanticImportNominalKind::Struct,
            },
            DurableType::Nominal(structure),
            DurableType::Nominal(enumeration),
            DurableType::Module(dependency),
            DurableType::Array {
                element: Box::new(DurableType::U8),
                len: u64::MAX,
            },
        ];
        let mut frontier = all
            .iter()
            .filter(|ty| {
                !matches!(
                    ty,
                    DurableType::ComptimeType
                        | DurableType::Module(_)
                        | DurableType::GenericParameter(_)
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        for depth in 1..=2 {
            let mut next = Vec::with_capacity(frontier.len() * 3);
            for ty in frontier {
                next.push(DurableType::Array {
                    element: Box::new(ty.clone()),
                    len: depth,
                });
                next.push(DurableType::PtrConst(Box::new(ty.clone())));
                next.push(DurableType::PtrMut(Box::new(ty)));
            }
            all.extend(next.iter().cloned());
            frontier = next;
        }

        let mut unique = HashSet::new();
        for durable in all {
            assert!(unique.insert(durable.clone()));
            assert_eq!(stable_hash(&durable), stable_hash(&durable.clone()));
            assert!(!format!("{durable:?}").is_empty());

            let dto = durable.import_dto();
            let local = first.import_type(&dto).unwrap();
            let exported = first.export_type(local.clone()).unwrap();
            assert_eq!(exported, dto);
            let remapped = second.import_type(&exported).unwrap();
            assert_eq!(second.export_type(remapped).unwrap(), dto);
            assert_eq!(
                second.export_type(local),
                Err(SemanticImportFailure::ForeignLocalType)
            );
        }
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
        let frozen_types = epoch.type_pool().clone().freeze();
        assert!(rue_codegen::cfg_lower::type_uses_sret_return(
            &frozen_types,
            rue_air::Type::new_struct(strbuf),
            8,
        ));
    }

    #[test]
    fn callable_parameter_order_is_semantic() {
        let parameter = |ty| DurableSemanticParameter {
            name: Arc::from("p"),
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
                        name: Arc::from("T"),
                        ty: DurableType::ComptimeType,
                        mode: DurableParameterMode::Value,
                        is_comptime: true,
                    },
                    DurableSemanticParameter {
                        name: Arc::from("value"),
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
