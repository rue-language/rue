//! Request-independent semantic values used at compiler query boundaries.
//!
//! These types deliberately have no conversion from `rue_air::Type`. Such a
//! conversion is only sound while the successful declaration binder, its type
//! pool, and the exact-revision stable-definition join are available together.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[cfg(test)]
use rue_air::{
    SemanticBinding, SemanticDeclarationExport, SemanticDeclarationPayload,
    SemanticDeclarationShell, SemanticExportConstValue, SemanticExportType, SemanticParameterMode,
    StableDefinitionNamespace,
};
#[cfg(test)]
use rue_air::{SemanticExportFailure, SemanticImportNominalKind};
use rue_air::{SemanticImportConstValue, SemanticImportType};
#[cfg(test)]
use rue_span::FileId;

use crate::retained_charge::RetainedCharge;
#[cfg(test)]
use crate::{BoundDefinitionSet, CanonicalMergedProgram, StableDefinitionKind};
use crate::{ModuleId, StableDefinitionKey};

/// The durable specialization of rue-air's canonical type algebra.
pub type DurableType = SemanticImportType<StableDefinitionKey, ModuleId>;

/// The durable specialization of rue-air's canonical constant algebra.
pub type DurableConstValue = SemanticImportConstValue<StableDefinitionKey, ModuleId>;

#[cfg(test)]
fn builtin_nominal_kind(name: &str) -> Option<SemanticImportNominalKind> {
    if name == "str" {
        Some(SemanticImportNominalKind::Struct)
    } else if rue_builtins::get_builtin_enum(name).is_some() {
        Some(SemanticImportNominalKind::Enum)
    } else {
        None
    }
}

/// Query-owned materialization payload for one anonymous nominal referenced by
/// declaration semantics. Identity remains separate from shape so recursive
/// uses and structurally equal aliases can be joined before fields are filled.
#[derive(Debug, Clone)]
pub struct DurableAnonymousNominal {
    pub identity: crate::AnonymousNominalKey,
    /// Exact body-local type-pool name derived once with the durable fact.
    ///
    /// This deliberately preserves the historical full-identity Debug
    /// spelling. It is not the canonical source symbol and therefore does not
    /// participate in the RUE-1295 spelling decision.
    materialization_name: Arc<str>,
    pub shape: DurableAnonymousNominalShape,
    pub type_captures: Arc<[(Arc<str>, DurableType)]>,
    pub value_captures: Arc<[(Arc<str>, DurableConstValue)]>,
}

impl DurableAnonymousNominal {
    fn semantic_parts(
        &self,
    ) -> (
        &crate::AnonymousNominalKey,
        &DurableAnonymousNominalShape,
        &Arc<[(Arc<str>, DurableType)]>,
        &Arc<[(Arc<str>, DurableConstValue)]>,
    ) {
        (
            &self.identity,
            &self.shape,
            &self.type_captures,
            &self.value_captures,
        )
    }

    pub(crate) fn new(
        identity: crate::AnonymousNominalKey,
        shape: DurableAnonymousNominalShape,
        type_captures: Arc<[(Arc<str>, DurableType)]>,
        value_captures: Arc<[(Arc<str>, DurableConstValue)]>,
    ) -> Self {
        let materialization_name = Arc::from(format!(
            "anonymous-{:?}",
            identity.with_canonical_producer()
        ));
        Self {
            identity,
            materialization_name,
            shape,
            type_captures,
            value_captures,
        }
    }

    pub(crate) fn with_shape(&self, shape: DurableAnonymousNominalShape) -> Self {
        Self {
            identity: self.identity.clone(),
            materialization_name: self.materialization_name.clone(),
            shape,
            type_captures: self.type_captures.clone(),
            value_captures: self.value_captures.clone(),
        }
    }

    pub(crate) fn materialization_name(&self) -> &Arc<str> {
        &self.materialization_name
    }
}

// The carried name is a cache derived entirely from `identity`, not a new part
// of the durable fact's semantic identity. Keep equality, ordering, and hashing
// identical to the pre-cache representation so query keys do not hash the
// formatted name or invalidate merely because the cache representation changes.
impl PartialEq for DurableAnonymousNominal {
    fn eq(&self, other: &Self) -> bool {
        self.semantic_parts() == other.semantic_parts()
    }
}

impl Eq for DurableAnonymousNominal {}

impl PartialOrd for DurableAnonymousNominal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DurableAnonymousNominal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.semantic_parts().cmp(&other.semantic_parts())
    }
}

impl Hash for DurableAnonymousNominal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.semantic_parts().hash(state);
    }
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
    /// Exact producer-owned syntax for this member body. Declaration-only
    /// projections may omit it, but a body transaction must refuse to analyze
    /// the member unless the producer's `body-produced-anonymous` projection
    /// supplied this fragment.
    pub body: Option<DurableAnonymousMemberBodySyntax>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurableAnonymousMemberBodySyntax {
    /// Exact byte offsets relative to the producer's retained body or constant
    /// initializer. They are presentation locators, not anonymous identity.
    pub declaration_start: u32,
    pub body_start: u32,
    pub body_end: u32,
    pub signature: crate::declaration_candidate::RawDeclarationSignatureSyntax,
    pub body: crate::declaration_candidate::RawDeclarationBodySyntax,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableAnonymousMethodType {
    SelfType,
    Concrete(DurableType),
}

/// The canonical durable parameter mode shared with the rue-air consumer.
pub type DurableParameterMode = rue_air::SemanticParameterMode;

/// The durable specialization of rue-air's canonical signature parameter.
///
/// Keeping this as the boundary type lets retained declaration-signature
/// payloads flow into body analysis by sharing their immutable slice instead
/// of allocating and cloning every parameter for every materialization.
pub type DurableSemanticParameter =
    rue_air::DurableSignatureParameter<StableDefinitionKey, ModuleId>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurableDeclarationPayload {
    Callable {
        parameters: Arc<[DurableSemanticParameter]>,
        result: DurableType,
        has_self: bool,
        self_mode: DurableParameterMode,
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

impl RetainedCharge for DurableAnonymousNominal {
    fn retained_charge(&self) -> u64 {
        self.identity
            .retained_charge()
            .saturating_add(self.materialization_name.retained_charge())
            .saturating_add(self.shape.retained_charge())
            .saturating_add(self.type_captures.retained_charge())
            .saturating_add(self.value_captures.retained_charge())
    }
}

impl RetainedCharge for DurableAnonymousNominalShape {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Struct { fields, methods } => fields
                .retained_charge()
                .saturating_add(methods.retained_charge()),
            Self::Enum { variants } => variants.retained_charge(),
        }
    }
}

impl RetainedCharge for DurableAnonymousMethodSignature {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.parameters.retained_charge())
            .saturating_add(self.result.retained_charge())
            .saturating_add(self.body.retained_charge())
    }
}

impl RetainedCharge for DurableAnonymousMemberBodySyntax {
    fn retained_charge(&self) -> u64 {
        self.signature
            .retained_charge()
            .saturating_add(self.body.retained_charge())
    }
}

impl RetainedCharge for DurableAnonymousMethodType {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::SelfType => 0,
            Self::Concrete(ty) => ty.retained_charge(),
        }
    }
}

impl RetainedCharge for DurableParameterMode {
    fn retained_charge(&self) -> u64 {
        0
    }
}

impl RetainedCharge for DurableSemanticParameter {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.ty.retained_charge())
    }
}

impl RetainedCharge for DurableDeclarationPayload {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Callable {
                parameters, result, ..
            } => parameters
                .retained_charge()
                .saturating_add(result.retained_charge()),
            Self::Struct { fields, .. } => fields.retained_charge(),
            Self::Enum { variants } => variants.retained_charge(),
            Self::Const { ty, value } => {
                ty.retained_charge().saturating_add(value.retained_charge())
            }
            Self::ModuleBinding { target } => target.retained_charge(),
            Self::Destructor => 0,
        }
    }
}

impl RetainedCharge for DurableDeclarationSemantic {
    fn retained_charge(&self) -> u64 {
        self.key
            .retained_charge()
            .saturating_add(self.payload.retained_charge())
    }
}

// The production query graph stores durable semantic facts directly. This
// snapshot-wide projection survives only as a differential test oracle for the
// durable value algebra, never as a compiler execution path.
#[cfg(test)]
pub(crate) use legacy_projection_oracle::{
    DurableSemanticExportFailure, DurableSemanticProjectionFailure, DurableSemanticProjectionWork,
    convert_declaration_semantics, durable_module_type, project_durable_declaration_semantics,
    project_durable_option_registry,
};

#[cfg(test)]
mod legacy_projection_oracle {
    use super::*;

    /// Work performed by the stable-key/current-revision projection adapter.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct DurableSemanticProjectionWork {
        pub projection_invocations: usize,
        pub shells_visited: usize,
        /// Durable-source declaration records examined by the projection join.
        pub durable_records_visited: usize,
        /// Durable-source records whose payloads were copied into current-revision
        /// semantic exports. This is intentionally separate from inspection: a
        /// future shared projection may inspect a record without copying it.
        pub durable_records_copied: usize,
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
            durable_records_copied: exports.len(),
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
    ) -> Result<Arc<[rue_air::SemanticAnonymousNominalExport]>, DurableSemanticProjectionFailure>
    {
        let module_files = merged
            .ast()
            .modules()
            .iter()
            .map(|module| (module.module_id().clone(), module.file_id()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut projected = Vec::with_capacity(anonymous_nominals.len());
        for nominal in anonymous_nominals {
            let identity =
                current_anonymous_identity(&nominal.identity, definitions, &module_files)?;
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
                                        DurableAnonymousMethodType::Concrete(
                                            DurableType::AnonymousNominal(candidate),
                                        ) if candidate.with_canonical_producer().as_ref()
                                            == nominal
                                                .identity
                                                .with_canonical_producer()
                                                .as_ref() =>
                                        {
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
    #[cfg(test)]
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
        let nominals = project_durable_anonymous_nominals(
            merged,
            definitions,
            &resolution.anonymous_nominals,
        )?;
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
                self_mode,
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
                self_mode: match self_mode {
                    DurableParameterMode::Value => SemanticParameterMode::Value,
                    DurableParameterMode::Borrow => SemanticParameterMode::Borrow,
                    DurableParameterMode::Inout => SemanticParameterMode::Inout,
                },
                is_unchecked: *is_unchecked,
            },
            DurableDeclarationPayload::Struct {
                fields,
                is_copy,
                is_linear,
            } => SemanticDeclarationPayload::Struct {
                fields: fields
                    .iter()
                    .map(|(name, ty)| {
                        Ok((name.clone(), project_type(ty, definitions, module_files)?))
                    })
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
                StableDefinitionKind::AssociatedFunction => {
                    StableDefinitionKind::AssociatedFunction
                }
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
                SemanticExportType::GenericParameter(index) => {
                    DurableType::GenericParameter(*index)
                }
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
                    self_mode,
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
                    self_mode: match self_mode {
                        SemanticParameterMode::Value => DurableParameterMode::Value,
                        SemanticParameterMode::Borrow => DurableParameterMode::Borrow,
                        SemanticParameterMode::Inout => DurableParameterMode::Inout,
                    },
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
                                key_for(*file_id, name, StableDefinitionKind::Function, None)
                                    .ok_or(
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
}
