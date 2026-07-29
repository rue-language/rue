//! Versioned, request-independent ordinary-body candidates.
//!
//! The compiler owns the authorization envelope while `rue-air` owns the
//! request-independent body algebra stored inside it. Compact live AIR remains
//! separate and is relocated only at export and import boundaries.

use std::{cell::RefCell, collections::BTreeSet, sync::Arc};

use rue_air::{
    SemanticBodyInstData, SemanticBodyPattern, SemanticBodyProjection, SemanticDefinitionToken,
    SemanticImportConstValue, SemanticImportType, SemanticModuleToken,
};

use crate::{
    BoundDefinitionSet, CanonicalMergedProgram, StableBodyDependencyInputRecord,
    StableDefinitionKey, StableDefinitionKind,
};

// Version 10/9: the body algebra gained the recorded method-reference payload
// (RUE-1128). Retained artifacts from the payload-free shape fail closed to
// ordinary analysis instead of misprojecting an empty reference set.
pub const DURABLE_ORDINARY_BODY_SCHEMA_VERSION: u32 = 10;
pub const DURABLE_SPECIALIZED_BODY_SCHEMA_VERSION: u32 = 9;

/// Durable specialization of AIR's canonical body algebra. These aliases keep
/// compiler consumers explicit about the stable identity domain.
#[cfg(test)]
pub type DurableBodyAnchor = rue_air::SemanticBodyAnchor;
pub type DurableProjection = rue_air::SemanticBodyProjection<StableDefinitionKey, crate::ModuleId>;
#[cfg(test)]
pub type DurableAirInst = rue_air::SemanticBodyInst<StableDefinitionKey, crate::ModuleId>;
pub type DurableAirInstData = rue_air::SemanticBodyInstData<StableDefinitionKey, crate::ModuleId>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableOrdinaryBodyPayload {
    pub schema_version: u32,
    pub semantic_schema_version: crate::DurableSemanticSchemaVersion,
    pub owner: StableDefinitionKey,
    /// Exact current-revision body/signature input captured with the live
    /// stable issuer. Finalization rejects a same-owner stale payload.
    pub expected_inputs: crate::StableDefinitionInputFingerprint,
    /// The canonical request-independent semantic body algebra. Authorization
    /// remains a property of this complete versioned envelope, never of the
    /// bare body value.
    pub body: rue_air::SemanticBody<StableDefinitionKey, crate::ModuleId>,
}

impl std::ops::Deref for DurableOrdinaryBodyPayload {
    type Target = rue_air::SemanticBody<StableDefinitionKey, crate::ModuleId>;

    fn deref(&self) -> &Self::Target {
        &self.body
    }
}

impl std::ops::DerefMut for DurableOrdinaryBodyPayload {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.body
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableOrdinaryBody {
    pub payload: DurableOrdinaryBodyPayload,
    /// Exact semantic inputs which authorize this candidate. Finalization
    /// validates that its owner equals `payload.owner`.
    pub inputs: StableBodyDependencyInputRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSpecializedBodyPayload {
    pub schema_version: u32,
    pub semantic_schema_version: crate::DurableSemanticSchemaVersion,
    pub identity: rue_air::SemanticSpecializationIdentity<StableDefinitionKey, crate::ModuleId>,
    /// The completed body uses the same durable AIR algebra as ordinary bodies.
    pub body: DurableOrdinaryBodyPayload,
    pub dependencies: Arc<[StableDefinitionKey]>,
    pub dependency_boundary_complete: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DurableBodyWork {
    pub candidate_comparisons: usize,
    pub candidate_fallbacks: usize,
    pub specialized_mapping_attempts: usize,
    pub specialized_mapping_successes: usize,
    pub specialized_mapping_failures: usize,
    pub export_attempts: usize,
    pub export_successes: usize,
    pub export_rejections: usize,
    pub last_export_failure: Option<rue_air::SemanticBodyExportFailure>,
    pub instructions_exported: usize,
    pub places_exported: usize,
    pub strings_exported: usize,
    pub conversion_attempts: usize,
    pub conversion_completions: usize,
    pub conversion_failures: usize,
    pub last_conversion_failure: Option<DurableBodyConversionFailure>,
    pub stable_key_joins: usize,
    pub finalization_attempts: usize,
    pub finalization_completions: usize,
    pub finalization_failures: usize,
    pub projection_attempts: usize,
    pub projection_completions: usize,
    pub projection_failures: usize,
    pub last_projection_failure: Option<DurableBodyProjectionFailure>,
    pub instructions_projected: usize,
    pub places_projected: usize,
    pub strings_projected: usize,
    pub import_attempts: usize,
    pub import_successes: usize,
    pub import_failures: usize,
    pub unsupported_import_fallbacks: usize,
    pub structural_import_fallbacks: usize,
    pub last_import_failure: Option<rue_air::SemanticBodyImportFailureKind>,
    pub installed_instructions: usize,
    pub installed_places: usize,
    pub installed_strings: usize,
    pub atomic_discards: usize,
    pub reused_bodies: usize,
    pub skipped_body_analyses: usize,
}

impl DurableBodyWork {
    fn reject_projection<T>(
        &mut self,
        reason: DurableBodyProjectionFailure,
    ) -> Result<T, DurableBodyProjectionFailure> {
        self.projection_failures += 1;
        self.last_projection_failure = Some(reason);
        Err(reason)
    }

    pub(crate) fn record_import_failure(
        &mut self,
        reason: rue_air::SemanticBodyImportFailureKind,
        count: usize,
    ) {
        self.import_failures += count;
        match reason {
            rue_air::SemanticBodyImportFailureKind::UnsupportedForm => {
                self.unsupported_import_fallbacks += count;
            }
            rue_air::SemanticBodyImportFailureKind::Semantic(_)
            | rue_air::SemanticBodyImportFailureKind::StructuralValidation => {
                self.structural_import_fallbacks += count;
            }
        }
        self.last_import_failure = Some(reason);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableBodyProjectionFailure {
    SchemaVersionMismatch,
    OwnerInputMismatch,
    BlockedDependencyInputs,
    InputFingerprintMismatch,
    InvalidInstructionReference,
    ForwardInstructionReference,
    InvalidPlaceReference,
    InvalidStringReference,
    InvalidAnchor,
    InvalidSourceOrder,
    InvalidParameterModes,
    InvalidParameterDrop,
    InvalidBorrowSlot,
    WarningProducingBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableBodyConversionFailure {
    ForeignRevision,
    ForeignOwnerToken,
    MissingStableDefinition,
    AmbiguousStableDefinition,
    WrongDefinitionKind,
    MissingDependencyInputs,
    AmbiguousDependencyInputs,
    OwnerInputMismatch,
    BlockedDependencyInputs,
    WarningProducingBody,
    UnresolvedModule,
    UnsupportedGenericCall,
    FingerprintUnavailable,
}

struct DefinitionJoinIndex<'a> {
    definitions: &'a BoundDefinitionSet,
}

impl<'a> DefinitionJoinIndex<'a> {
    fn new(_merged: &'a CanonicalMergedProgram, definitions: &'a BoundDefinitionSet) -> Self {
        Self { definitions }
    }

    fn join(
        &self,
        identity: &SemanticDefinitionToken,
        work: &mut DurableBodyWork,
    ) -> Result<&'a StableDefinitionKey, DurableBodyConversionFailure> {
        work.stable_key_joins += 1;
        match self.definitions.key_for_semantic_token(*identity) {
            Ok(key) => Ok(key),
            Err(rue_air::SemanticStableResolutionFailure::Missing) => {
                Err(DurableBodyConversionFailure::MissingStableDefinition)
            }
            Err(rue_air::SemanticStableResolutionFailure::Ambiguous) => {
                Err(DurableBodyConversionFailure::AmbiguousStableDefinition)
            }
            Err(rue_air::SemanticStableResolutionFailure::WrongKind) => {
                Err(DurableBodyConversionFailure::WrongDefinitionKind)
            }
            Err(rue_air::SemanticStableResolutionFailure::ForeignIssuer) => {
                Err(DurableBodyConversionFailure::ForeignOwnerToken)
            }
        }
    }
}

fn join_definition<'a>(
    identity: &SemanticDefinitionToken,
    index: &DefinitionJoinIndex<'a>,
    work: &mut DurableBodyWork,
) -> Result<&'a StableDefinitionKey, DurableBodyConversionFailure> {
    index.join(identity, work)
}

fn canonical_type(
    ty: &SemanticImportType<SemanticDefinitionToken, SemanticModuleToken>,
    merged: &CanonicalMergedProgram,
    index: &DefinitionJoinIndex<'_>,
    work: &mut DurableBodyWork,
) -> Result<SemanticImportType<StableDefinitionKey, crate::ModuleId>, DurableBodyConversionFailure>
{
    Ok(match ty {
        SemanticImportType::I8 => SemanticImportType::I8,
        SemanticImportType::I16 => SemanticImportType::I16,
        SemanticImportType::I32 => SemanticImportType::I32,
        SemanticImportType::I64 => SemanticImportType::I64,
        SemanticImportType::U8 => SemanticImportType::U8,
        SemanticImportType::U16 => SemanticImportType::U16,
        SemanticImportType::U32 => SemanticImportType::U32,
        SemanticImportType::U64 => SemanticImportType::U64,
        SemanticImportType::Bool => SemanticImportType::Bool,
        SemanticImportType::Unit => SemanticImportType::Unit,
        SemanticImportType::Never => SemanticImportType::Never,
        SemanticImportType::ComptimeType => SemanticImportType::ComptimeType,
        SemanticImportType::BuiltinNominal { name, kind } => {
            canonical_builtin_nominal(name, *kind)?
        }
        SemanticImportType::Nominal(identity) => {
            let key = join_definition(identity, index, work)?;
            if !matches!(
                key.kind(),
                StableDefinitionKind::Struct | StableDefinitionKind::Enum
            ) {
                return Err(DurableBodyConversionFailure::WrongDefinitionKind);
            }
            SemanticImportType::Nominal(key.clone())
        }
        SemanticImportType::AnonymousNominal(identity) => {
            let work = RefCell::new(work);
            let identity = identity.try_map_identities(
                &|token| Ok(join_definition(token, index, &mut work.borrow_mut())?.clone()),
                &|token| {
                    index
                        .definitions
                        .module_for_semantic_token(merged, *token)
                        .cloned()
                        .map_err(|failure| match failure {
                            rue_air::SemanticStableResolutionFailure::ForeignIssuer => {
                                DurableBodyConversionFailure::ForeignOwnerToken
                            }
                            _ => DurableBodyConversionFailure::UnresolvedModule,
                        })
                },
            )?;
            validate_anonymous_identity(&identity)?;
            SemanticImportType::AnonymousNominal(identity)
        }
        SemanticImportType::Array { element, len } => SemanticImportType::Array {
            element: Box::new(canonical_type(element, merged, index, work)?),
            len: *len,
        },
        SemanticImportType::Slice { element, name } => SemanticImportType::Slice {
            element: Box::new(canonical_type(element, merged, index, work)?),
            name: name.clone(),
        },
        SemanticImportType::PtrConst(value) => {
            SemanticImportType::PtrConst(Box::new(canonical_type(value, merged, index, work)?))
        }
        SemanticImportType::PtrMut(value) => {
            SemanticImportType::PtrMut(Box::new(canonical_type(value, merged, index, work)?))
        }
        SemanticImportType::Module(token) => SemanticImportType::Module(
            index
                .definitions
                .module_for_semantic_token(merged, *token)
                .map_err(|failure| match failure {
                    rue_air::SemanticStableResolutionFailure::ForeignIssuer => {
                        DurableBodyConversionFailure::ForeignOwnerToken
                    }
                    _ => DurableBodyConversionFailure::UnresolvedModule,
                })?
                .clone(),
        ),
        SemanticImportType::GenericParameter(index) => SemanticImportType::GenericParameter(*index),
    })
}

fn validate_anonymous_identity(
    identity: &rue_air::AnonymousNominalKey<StableDefinitionKey, crate::ModuleId>,
) -> Result<(), DurableBodyConversionFailure> {
    use rue_air::{CanonicalArgumentValue as V, StableProducerId as P};

    fn type_key(
        value: &rue_air::TypeInstanceKey<StableDefinitionKey, crate::ModuleId>,
    ) -> Result<(), DurableBodyConversionFailure> {
        use rue_air::{NominalInstanceKey as N, TypeInstanceKey as T};
        match value {
            T::Nominal(N::Named(key))
                if !matches!(
                    key.kind(),
                    StableDefinitionKind::Struct | StableDefinitionKind::Enum
                ) =>
            {
                Err(DurableBodyConversionFailure::WrongDefinitionKind)
            }
            T::Nominal(N::Anonymous(key)) => validate_anonymous_identity(key),
            T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
                type_key(element)
            }
            _ => Ok(()),
        }
    }
    fn function_key(
        value: &rue_air::FunctionInstanceKey<StableDefinitionKey, crate::ModuleId>,
    ) -> Result<(), DurableBodyConversionFailure> {
        use rue_air::FunctionInstanceKey as F;
        match value {
            F::Definition(key) if !key.kind().owns_body() => {
                Err(DurableBodyConversionFailure::WrongDefinitionKind)
            }
            F::Specialization { base, arguments } => {
                function_key(base)?;
                arguments_key(arguments)
            }
            F::AnonymousMember { owner, .. } | F::DropGlue(owner) => type_key(owner),
            _ => Ok(()),
        }
    }
    fn arguments_key(
        arguments: &rue_air::CanonicalArguments<StableDefinitionKey, crate::ModuleId>,
    ) -> Result<(), DurableBodyConversionFailure> {
        for value in arguments.types.iter() {
            type_key(value)?;
        }
        for value in arguments.values.iter() {
            match value {
                V::Type(value) => type_key(value)?,
                V::Function(value) => function_key(value)?,
                _ => {}
            }
        }
        Ok(())
    }

    match &identity.producer {
        P::Definition(_) => {}
        P::Function(value) => function_key(value)?,
    }
    arguments_key(&identity.arguments)
}

fn canonical_builtin_nominal(
    name: &Arc<str>,
    kind: rue_air::SemanticImportNominalKind,
) -> Result<SemanticImportType<StableDefinitionKey, crate::ModuleId>, DurableBodyConversionFailure>
{
    if crate::durable_semantics::builtin_nominal_kind(name) != Some(kind) {
        return Err(DurableBodyConversionFailure::WrongDefinitionKind);
    }
    Ok(SemanticImportType::BuiltinNominal {
        name: name.clone(),
        kind,
    })
}

fn canonical_const_value(
    value: &SemanticImportConstValue<SemanticDefinitionToken, SemanticModuleToken>,
    merged: &CanonicalMergedProgram,
    index: &DefinitionJoinIndex<'_>,
    work: &mut DurableBodyWork,
) -> Result<
    SemanticImportConstValue<StableDefinitionKey, crate::ModuleId>,
    DurableBodyConversionFailure,
> {
    Ok(match value {
        SemanticImportConstValue::Integer(value) => SemanticImportConstValue::Integer(*value),
        SemanticImportConstValue::Bool(value) => SemanticImportConstValue::Bool(*value),
        SemanticImportConstValue::Type(value) => {
            SemanticImportConstValue::Type(canonical_type(value, merged, index, work)?)
        }
        SemanticImportConstValue::Function(value) => {
            let key = join_definition(value, index, work)?;
            if !matches!(
                key.kind(),
                StableDefinitionKind::Function
                    | StableDefinitionKind::Method
                    | StableDefinitionKind::AssociatedFunction
            ) {
                return Err(DurableBodyConversionFailure::WrongDefinitionKind);
            }
            SemanticImportConstValue::Function(key.clone())
        }
        SemanticImportConstValue::Unit => SemanticImportConstValue::Unit,
        SemanticImportConstValue::String(value) => SemanticImportConstValue::String(value.clone()),
    })
}

fn durable_specialization_identity(
    identity: &rue_air::SemanticSpecializationIdentity<
        SemanticDefinitionToken,
        SemanticModuleToken,
    >,
    merged: &CanonicalMergedProgram,
    index: &DefinitionJoinIndex<'_>,
    work: &mut DurableBodyWork,
) -> Result<
    rue_air::SemanticSpecializationIdentity<StableDefinitionKey, crate::ModuleId>,
    DurableBodyConversionFailure,
> {
    let base = join_definition(&identity.base, index, work)?.clone();
    if base.kind() != StableDefinitionKind::Function {
        return Err(DurableBodyConversionFailure::WrongDefinitionKind);
    }
    Ok(rue_air::SemanticSpecializationIdentity {
        base,
        type_arguments: identity
            .type_arguments
            .iter()
            .map(|value| canonical_type(value, merged, index, work))
            .collect::<Result<Vec<_>, _>>()?
            .into(),
        value_arguments: identity
            .value_arguments
            .iter()
            .map(|value| canonical_const_value(value, merged, index, work))
            .collect::<Result<Vec<_>, _>>()?
            .into(),
    })
}

fn canonical_identity_dependency_keys(
    identity: &rue_air::SemanticSpecializationIdentity<StableDefinitionKey, crate::ModuleId>,
) -> BTreeSet<StableDefinitionKey> {
    fn visit_type(
        value: &SemanticImportType<StableDefinitionKey, crate::ModuleId>,
        keys: &mut BTreeSet<StableDefinitionKey>,
    ) {
        match value {
            SemanticImportType::Nominal(key) => {
                keys.insert(key.clone());
            }
            SemanticImportType::AnonymousNominal(identity) => {
                collect_anonymous_definition_keys(identity, keys);
            }
            SemanticImportType::Array { element, .. }
            | SemanticImportType::PtrConst(element)
            | SemanticImportType::PtrMut(element) => visit_type(element, keys),
            _ => {}
        }
    }
    let mut keys = BTreeSet::new();
    for value in identity.type_arguments.iter() {
        visit_type(value, &mut keys);
    }
    for value in identity.value_arguments.iter() {
        match value {
            SemanticImportConstValue::Type(value) => visit_type(value, &mut keys),
            SemanticImportConstValue::Function(key) => {
                keys.insert(key.clone());
            }
            _ => {}
        }
    }
    keys
}

fn collect_anonymous_definition_keys(
    identity: &rue_air::AnonymousNominalKey<StableDefinitionKey, crate::ModuleId>,
    keys: &mut BTreeSet<StableDefinitionKey>,
) {
    let keys = RefCell::new(keys);
    identity
        .try_map_identities(
            &|key| {
                keys.borrow_mut().insert(key.clone());
                Ok::<_, std::convert::Infallible>(key.clone())
            },
            &|module| Ok::<_, std::convert::Infallible>(module.clone()),
        )
        .expect("identity-preserving anonymous dependency traversal is infallible");
}

fn collect_nominal_definition_keys(
    identity: &rue_air::NominalInstanceKey<StableDefinitionKey, crate::ModuleId>,
    keys: &mut BTreeSet<StableDefinitionKey>,
) {
    match identity {
        rue_air::NominalInstanceKey::Builtin { .. } => {}
        rue_air::NominalInstanceKey::Named(key) => {
            keys.insert(key.clone());
        }
        rue_air::NominalInstanceKey::Anonymous(key) => {
            collect_anonymous_definition_keys(key, keys);
        }
    }
}

fn collect_function_definition_keys(
    identity: &rue_air::FunctionInstanceKey<StableDefinitionKey, crate::ModuleId>,
    keys: &mut BTreeSet<StableDefinitionKey>,
) {
    let keys = RefCell::new(keys);
    identity
        .try_map_identities(
            &|key| {
                keys.borrow_mut().insert(key.clone());
                Ok::<_, std::convert::Infallible>(key.clone())
            },
            &|module| Ok::<_, std::convert::Infallible>(module.clone()),
        )
        .expect("identity-preserving function dependency traversal is infallible");
}

fn record_specialized_conversion_failure(
    work: &mut DurableBodyWork,
    failure: DurableBodyConversionFailure,
) -> DurableBodyConversionFailure {
    work.conversion_attempts += 1;
    work.conversion_failures += 1;
    work.last_conversion_failure = Some(failure);
    work.atomic_discards += 1;
    failure
}

/// Atomically joins completed specialization identities and bodies to stable
/// compiler-owned values. A failed identity, argument, or body conversion
/// discards the whole export set, never a partially stable artifact.
pub fn convert_semantic_specialized_body_exports(
    exports: &[rue_air::SemanticSpecializedBodyExport],
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
    work: &mut DurableBodyWork,
) -> Result<Arc<[DurableSpecializedBodyPayload]>, DurableBodyConversionFailure> {
    if definitions.source_revision() != merged.ast().source_revision() {
        work.conversion_attempts += exports.len();
        work.conversion_failures += exports.len();
        work.atomic_discards += usize::from(!exports.is_empty());
        return Err(DurableBodyConversionFailure::ForeignRevision);
    }
    let index = DefinitionJoinIndex::new(merged, definitions);
    let mut converted = Vec::with_capacity(exports.len());
    for export in exports {
        let preflight = (|| {
            let identity = durable_specialization_identity(&export.identity, merged, &index, work)?;
            let base = identity.base.clone();
            let token = definitions
                .body_owner_endpoints()
                .into_iter()
                .find(|endpoint| definitions.key_for_body_token(endpoint.token).ok() == Some(&base))
                .map(|endpoint| endpoint.token)
                .ok_or(DurableBodyConversionFailure::MissingStableDefinition)?;
            let mut dependencies = export
                .dependencies
                .iter()
                .map(|dependency| join_definition(dependency, &index, work).cloned())
                .collect::<Result<BTreeSet<_>, _>>()?;
            dependencies.extend(canonical_identity_dependency_keys(&identity));
            Ok((identity, token, dependencies))
        })();
        let (identity, token, dependencies) = match preflight {
            Ok(preflight) => preflight,
            Err(failure) => {
                return Err(record_specialized_conversion_failure(work, failure));
            }
        };
        let ordinary = rue_air::SemanticBodyExport {
            owner: token,
            body: export.body.clone(),
        };
        let bodies = convert_semantic_body_exports(&[ordinary], merged, definitions, work)?;
        let [body] = bodies.as_ref() else {
            unreachable!("one successful body conversion returns one body")
        };
        converted.push(DurableSpecializedBodyPayload {
            schema_version: DURABLE_SPECIALIZED_BODY_SCHEMA_VERSION,
            semantic_schema_version: crate::DURABLE_SEMANTIC_SCHEMA_VERSION,
            identity,
            body: body.clone(),
            dependencies: dependencies.into_iter().collect::<Vec<_>>().into(),
            dependency_boundary_complete: export.dependency_boundary_complete,
        });
    }
    Ok(converted.into())
}

/// Attach destructor inputs observed by CFG lowering to the exact completed
/// specialization that owned the drop. No dependency is inferred from types
/// merely mentioned or borrowed by the body.
pub fn attach_specialized_implicit_drop_dependencies(
    payloads: Arc<[DurableSpecializedBodyPayload]>,
    events: &[rue_air::ImplicitNamedDestructorDependencyEvent],
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
    work: &mut DurableBodyWork,
) -> Result<Arc<[DurableSpecializedBodyPayload]>, DurableBodyConversionFailure> {
    let index = DefinitionJoinIndex::new(merged, definitions);
    let mut payloads = payloads.to_vec();
    for event in events {
        let rue_air::ImplicitDropDependencySourceEvent::Specialization { identity } = &event.source
        else {
            continue;
        };
        let identity = durable_specialization_identity(identity, merged, &index, work)?;
        let Some(payload) = payloads
            .iter_mut()
            .find(|payload| payload.identity == identity)
        else {
            // Warning-producing and otherwise rejected exports have no durable
            // payload to enrich.
            continue;
        };
        work.stable_key_joins += 1;
        let destructor = definitions
            .definitions()
            .iter()
            .find(|record| {
                let key = record.stable_key();
                key.kind() == rue_air::StableDefinitionKind::Destructor
                    && key.owner().map(|owner| owner.name())
                        == Some(event.target_owner_name.as_str())
                    && record.declaration_span().file_id.index() == event.target_file
            })
            .map(|record| record.stable_key().clone())
            .ok_or(DurableBodyConversionFailure::MissingStableDefinition)?;
        let mut dependencies = payload
            .dependencies
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        dependencies.insert(destructor);
        payload.dependencies = dependencies.into_iter().collect::<Vec<_>>().into();
    }
    Ok(payloads.into())
}

/// Atomically join AIR's neutral ordinary-body exports to the authoritative
/// stable-definition universe for this exact revision. Complete dependency
/// inputs are attached later by [`finalize_durable_ordinary_bodies`].
pub fn convert_semantic_body_exports(
    exports: &[rue_air::SemanticBodyExport],
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
    work: &mut DurableBodyWork,
) -> Result<Arc<[DurableOrdinaryBodyPayload]>, DurableBodyConversionFailure> {
    if definitions.source_revision() != merged.ast().source_revision() {
        work.conversion_failures += exports.len();
        work.atomic_discards += usize::from(!exports.is_empty());
        return Err(DurableBodyConversionFailure::ForeignRevision);
    }

    let join_index = DefinitionJoinIndex::new(merged, definitions);
    let mut converted = Vec::with_capacity(exports.len());
    for export in exports {
        work.conversion_attempts += 1;
        let result =
            (|| {
                let owner_record = definitions
                    .definition_for_body_token(export.owner)
                    .map_err(|_| DurableBodyConversionFailure::ForeignOwnerToken)?;
                let owner = owner_record.stable_key().clone();
                if !matches!(
                    owner.kind(),
                    StableDefinitionKind::Function
                        | StableDefinitionKind::Method
                        | StableDefinitionKind::AssociatedFunction
                        | StableDefinitionKind::Destructor
                ) {
                    return Err(DurableBodyConversionFailure::WrongDefinitionKind);
                }
                if !export.body.warnings.is_empty() {
                    return Err(DurableBodyConversionFailure::WarningProducingBody);
                }
                if export.body.instructions.iter().any(|instruction| {
                    matches!(instruction.data, SemanticBodyInstData::CallGeneric)
                }) {
                    return Err(DurableBodyConversionFailure::UnsupportedGenericCall);
                }

                let expected_inputs = crate::session::stable_definition_input_fingerprint(
                    merged.definitions().source_snapshot(),
                    owner_record,
                )
                .map_err(|_| DurableBodyConversionFailure::FingerprintUnavailable)?;
                let stable_key_joins = std::cell::Cell::new(0usize);
                let body = export.body.try_map_keys(
                    &|identity| {
                        stable_key_joins.set(stable_key_joins.get() + 1);
                        join_index
                            .definitions
                            .key_for_semantic_token(*identity)
                            .cloned()
                            .map_err(|failure| match failure {
                                rue_air::SemanticStableResolutionFailure::Missing => {
                                    DurableBodyConversionFailure::MissingStableDefinition
                                }
                                rue_air::SemanticStableResolutionFailure::Ambiguous => {
                                    DurableBodyConversionFailure::AmbiguousStableDefinition
                                }
                                rue_air::SemanticStableResolutionFailure::WrongKind => {
                                    DurableBodyConversionFailure::WrongDefinitionKind
                                }
                                rue_air::SemanticStableResolutionFailure::ForeignIssuer => {
                                    DurableBodyConversionFailure::ForeignOwnerToken
                                }
                            })
                    },
                    &|token| {
                        join_index
                            .definitions
                            .module_for_semantic_token(merged, *token)
                            .cloned()
                            .map_err(|failure| match failure {
                                rue_air::SemanticStableResolutionFailure::ForeignIssuer => {
                                    DurableBodyConversionFailure::ForeignOwnerToken
                                }
                                _ => DurableBodyConversionFailure::UnresolvedModule,
                            })
                    },
                );
                work.stable_key_joins += stable_key_joins.get();
                let body = body?;
                Ok(DurableOrdinaryBodyPayload {
                    schema_version: DURABLE_ORDINARY_BODY_SCHEMA_VERSION,
                    semantic_schema_version: crate::DURABLE_SEMANTIC_SCHEMA_VERSION,
                    owner,
                    expected_inputs,
                    body,
                })
            })();

        match result {
            Ok(payload) => {
                work.conversion_completions += 1;
                converted.push(payload);
            }
            Err(error) => {
                work.conversion_failures += 1;
                work.atomic_discards += 1;
                work.last_conversion_failure = Some(error);
                return Err(error);
            }
        }
    }
    Ok(converted.into())
}

/// Attach the complete dependency records after the session has built them.
/// This stage is all-or-nothing and never mutates canonical semantic work.
pub fn finalize_durable_ordinary_bodies(
    payloads: &[DurableOrdinaryBodyPayload],
    inputs: &[StableBodyDependencyInputRecord],
    work: &mut DurableBodyWork,
) -> Result<Arc<[DurableOrdinaryBody]>, DurableBodyConversionFailure> {
    let mut result = Vec::with_capacity(payloads.len());
    for payload in payloads {
        work.finalization_attempts += 1;
        let matches = inputs
            .iter()
            .filter(|input| input.owner() == &payload.owner)
            .collect::<Vec<_>>();
        let input = match matches.as_slice() {
            [input] => (*input).clone(),
            [] => {
                work.finalization_failures += 1;
                work.atomic_discards += 1;
                return Err(DurableBodyConversionFailure::MissingDependencyInputs);
            }
            _ => {
                work.finalization_failures += 1;
                work.atomic_discards += 1;
                return Err(DurableBodyConversionFailure::AmbiguousDependencyInputs);
            }
        };
        if input.owner() != &payload.owner {
            work.finalization_failures += 1;
            work.atomic_discards += 1;
            return Err(DurableBodyConversionFailure::OwnerInputMismatch);
        }
        if input.fingerprint() != &payload.expected_inputs {
            work.finalization_failures += 1;
            work.atomic_discards += 1;
            return Err(DurableBodyConversionFailure::OwnerInputMismatch);
        }
        if !input.reusable_boundary_supported() {
            work.finalization_failures += 1;
            work.atomic_discards += 1;
            return Err(DurableBodyConversionFailure::BlockedDependencyInputs);
        }
        result.push(DurableOrdinaryBody {
            payload: payload.clone(),
            inputs: input,
        });
        work.finalization_completions += 1;
    }
    Ok(result.into())
}

impl DurableOrdinaryBody {
    pub fn owner(&self) -> &StableDefinitionKey {
        &self.payload.owner
    }

    pub fn project_semantic_body(
        &self,
        work: &mut DurableBodyWork,
    ) -> Result<rue_air::SemanticBody<StableDefinitionKey, Arc<str>>, DurableBodyProjectionFailure>
    {
        if self.inputs.owner() != &self.payload.owner
            || self.inputs.fingerprint() != &self.payload.expected_inputs
        {
            work.projection_attempts += 1;
            return work.reject_projection(DurableBodyProjectionFailure::InputFingerprintMismatch);
        }
        self.payload.project_semantic_body(work)
    }
}

impl DurableOrdinaryBodyPayload {
    pub(crate) fn referenced_definition_keys(&self) -> BTreeSet<StableDefinitionKey> {
        fn visit_type(
            value: &SemanticImportType<StableDefinitionKey, crate::ModuleId>,
            keys: &mut BTreeSet<StableDefinitionKey>,
        ) {
            match value {
                SemanticImportType::Nominal(key) => {
                    keys.insert(key.clone());
                }
                SemanticImportType::AnonymousNominal(identity) => {
                    collect_anonymous_definition_keys(identity, keys);
                }
                SemanticImportType::Array { element, .. }
                | SemanticImportType::PtrConst(element)
                | SemanticImportType::PtrMut(element) => visit_type(element, keys),
                _ => {}
            }
        }

        fn visit_specialization(
            identity: &rue_air::SemanticSpecializationIdentity<
                StableDefinitionKey,
                crate::ModuleId,
            >,
            keys: &mut BTreeSet<StableDefinitionKey>,
        ) {
            keys.insert(identity.base.clone());
            for value in identity.type_arguments.iter() {
                visit_type(value, keys);
            }
            for value in identity.value_arguments.iter() {
                match value {
                    SemanticImportConstValue::Type(value) => visit_type(value, keys),
                    SemanticImportConstValue::Function(key) => {
                        keys.insert(key.clone());
                    }
                    _ => {}
                }
            }
        }

        let mut keys = BTreeSet::new();
        visit_type(&self.body.return_type, &mut keys);
        for instruction in self.body.instructions.iter() {
            visit_type(&instruction.ty, &mut keys);
            match &instruction.data {
                SemanticBodyInstData::TypeConst(value)
                | SemanticBodyInstData::IntCast { from_ty: value, .. } => {
                    visit_type(value, &mut keys);
                }
                SemanticBodyInstData::Call { function, .. } => {
                    collect_function_definition_keys(function, &mut keys);
                }
                SemanticBodyInstData::CallSpecialized { identity, .. } => {
                    visit_specialization(identity, &mut keys);
                }
                SemanticBodyInstData::StructInit { struct_key, .. } => {
                    collect_nominal_definition_keys(struct_key, &mut keys);
                }
                SemanticBodyInstData::EnumVariant { enum_key, .. }
                | SemanticBodyInstData::EnumPayloadGet { enum_key, .. } => {
                    collect_nominal_definition_keys(enum_key, &mut keys);
                }
                SemanticBodyInstData::Match { arms, .. } => {
                    for arm in arms.iter() {
                        if let SemanticBodyPattern::EnumVariant { enum_key, .. } = &arm.pattern {
                            collect_nominal_definition_keys(enum_key, &mut keys);
                        }
                    }
                }
                _ => {}
            }
        }
        for place in self.body.places.iter() {
            visit_type(&place.base_type, &mut keys);
            for projection in place.projections.iter() {
                match projection {
                    SemanticBodyProjection::Field { struct_key, .. } => {
                        collect_nominal_definition_keys(struct_key, &mut keys);
                    }
                    SemanticBodyProjection::Index { array_type, .. } => {
                        visit_type(array_type, &mut keys);
                    }
                }
            }
        }
        for (_, value) in self.body.param_drops.iter() {
            visit_type(value, &mut keys);
        }
        keys
    }

    pub fn project_semantic_body(
        &self,
        work: &mut DurableBodyWork,
    ) -> Result<rue_air::SemanticBody<StableDefinitionKey, Arc<str>>, DurableBodyProjectionFailure>
    {
        work.projection_attempts += 1;
        if self.schema_version != DURABLE_ORDINARY_BODY_SCHEMA_VERSION
            || !crate::DURABLE_SEMANTIC_SCHEMA_VERSION.accepts(self.semantic_schema_version)
        {
            return work.reject_projection(DurableBodyProjectionFailure::SchemaVersionMismatch);
        }
        if self.body.param_by_ref.len() != self.body.num_param_slots as usize
            || self.body.param_writable.len() != self.body.num_param_slots as usize
            || self
                .body
                .param_writable
                .iter()
                .zip(self.body.param_by_ref.iter())
                .any(|(writable, by_ref)| *writable && !*by_ref)
        {
            return work.reject_projection(DurableBodyProjectionFailure::InvalidParameterModes);
        }
        if self
            .body
            .param_drops
            .iter()
            .any(|(slot, _)| *slot >= self.body.num_param_slots)
        {
            return work.reject_projection(DurableBodyProjectionFailure::InvalidParameterDrop);
        }
        if self
            .body
            .borrow_slots
            .iter()
            .any(|slot| *slot >= self.body.num_locals)
        {
            return work.reject_projection(DurableBodyProjectionFailure::InvalidBorrowSlot);
        }
        let instruction_len = self.body.instructions.len();
        let place_len = self.body.places.len();
        for place in self.body.places.iter() {
            for projection in place.projections.iter() {
                if let SemanticBodyProjection::Index { index, .. } = projection
                    && *index as usize >= instruction_len
                {
                    return work.reject_projection(
                        DurableBodyProjectionFailure::InvalidInstructionReference,
                    );
                }
            }
        }
        for (current, instruction) in self.body.instructions.iter().enumerate() {
            if instruction.anchor.start > instruction.anchor.end {
                return work.reject_projection(DurableBodyProjectionFailure::InvalidAnchor);
            }
            let check = |value: rue_air::SemanticBodyRef| {
                let index = value as usize;
                if index >= instruction_len {
                    Err(DurableBodyProjectionFailure::InvalidInstructionReference)
                } else if index >= current {
                    Err(DurableBodyProjectionFailure::ForwardInstructionReference)
                } else {
                    Ok(())
                }
            };
            let check_all = |values: &[rue_air::SemanticBodyRef]| -> Result<_, _> {
                values.iter().try_for_each(|value| check(*value))
            };
            use SemanticBodyInstData as D;
            let validated = match &instruction.data {
                D::Const(_)
                | D::BoolConst(_)
                | D::UnitConst
                | D::TypeConst(_)
                | D::Break
                | D::Continue
                | D::Load { .. }
                | D::Param { .. }
                | D::StorageLive { .. }
                | D::StorageDead { .. } => Ok(()),
                D::CallGeneric => Err(DurableBodyProjectionFailure::InvalidInstructionReference),
                D::StringConst(index) => {
                    if *index as usize >= self.body.strings.len() {
                        Err(DurableBodyProjectionFailure::InvalidStringReference)
                    } else {
                        Ok(())
                    }
                }
                D::Add(a, b)
                | D::Sub(a, b)
                | D::Mul(a, b)
                | D::WrappingAdd(a, b)
                | D::WrappingSub(a, b)
                | D::WrappingMul(a, b)
                | D::Div(a, b)
                | D::Mod(a, b)
                | D::Eq(a, b)
                | D::Ne(a, b)
                | D::Lt(a, b)
                | D::Gt(a, b)
                | D::Le(a, b)
                | D::Ge(a, b)
                | D::And(a, b)
                | D::Or(a, b)
                | D::BitAnd(a, b)
                | D::BitOr(a, b)
                | D::BitXor(a, b)
                | D::Shl(a, b)
                | D::Shr(a, b) => check(*a).and_then(|()| check(*b)),
                D::Neg(value)
                | D::Not(value)
                | D::BitNot(value)
                | D::Drop { value }
                | D::IntCast { value, .. } => check(*value),
                D::Branch {
                    cond,
                    then_value,
                    else_value,
                } => check(*cond)
                    .and_then(|()| check(*then_value))
                    .and_then(|()| else_value.map_or(Ok(()), &check)),
                D::Loop { cond, body } => check(*cond).and_then(|()| check(*body)),
                D::InfiniteLoop { body } => check(*body),
                D::Match { scrutinee, arms } => {
                    check(*scrutinee).and_then(|()| arms.iter().try_for_each(|arm| check(arm.body)))
                }
                D::Alloc { init, .. } => check(*init),
                D::Store { value, .. } | D::ParamStore { value, .. } => check(*value),
                D::Ret(value) => value.map_or(Ok(()), &check),
                D::Call { args, .. }
                | D::RuntimeCall { args, .. }
                | D::CallSpecialized { args, .. }
                | D::Intrinsic { args, .. } => args.iter().try_for_each(|arg| check(arg.value)),
                D::Block { statements, value } => {
                    check_all(statements).and_then(|()| check(*value))
                }
                D::StructInit {
                    fields,
                    source_order,
                    ..
                } => {
                    let mut seen = vec![false; fields.len()];
                    if fields.len() != source_order.len()
                        || source_order.iter().any(|index| {
                            let index = *index as usize;
                            index >= fields.len() || std::mem::replace(&mut seen[index], true)
                        })
                    {
                        Err(DurableBodyProjectionFailure::InvalidSourceOrder)
                    } else {
                        check_all(fields)
                    }
                }
                D::EnumPayloadGet { base, .. } => check(*base),
                D::ArrayInit { elements }
                | D::EnumVariant {
                    payload: elements, ..
                } => check_all(elements),
                D::PlaceRead { place } => {
                    if *place as usize >= place_len {
                        Err(DurableBodyProjectionFailure::InvalidPlaceReference)
                    } else {
                        Ok(())
                    }
                }
                D::PlaceWrite { place, value } => {
                    if *place as usize >= place_len {
                        Err(DurableBodyProjectionFailure::InvalidPlaceReference)
                    } else {
                        check(*value)
                    }
                }
                D::MarkMoved { value, place, .. } => {
                    if place.is_some_and(|place| place as usize >= place_len) {
                        Err(DurableBodyProjectionFailure::InvalidPlaceReference)
                    } else {
                        check(*value)
                    }
                }
            };
            if let Err(error) = validated {
                return work.reject_projection(error);
            }
        }
        if !self.body.warnings.is_empty() {
            return work.reject_projection(DurableBodyProjectionFailure::WarningProducingBody);
        }
        let body = self
            .body
            .try_map_keys(
                &|key| Ok::<_, std::convert::Infallible>(key.clone()),
                &|module| Ok::<_, std::convert::Infallible>(Arc::from(module.as_str())),
            )
            .expect("canonical body key projection is infallible");
        work.projection_completions += 1;
        work.instructions_projected += body.instructions.len();
        work.places_projected += body.places.len();
        work.strings_projected += body.strings.len();
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModuleId, StableDefinitionNamespace};

    fn owner() -> StableDefinitionKey {
        StableDefinitionKey::for_test(
            ModuleId::from_logical_path("main.rue").unwrap(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "main",
            None,
        )
    }

    fn payload(instructions: Vec<DurableAirInst>) -> DurableOrdinaryBodyPayload {
        let key = owner();
        DurableOrdinaryBodyPayload {
            schema_version: DURABLE_ORDINARY_BODY_SCHEMA_VERSION,
            semantic_schema_version: crate::DURABLE_SEMANTIC_SCHEMA_VERSION,
            owner: key.clone(),
            expected_inputs: crate::StableDefinitionInputFingerprint {
                schema_version: 1,
                key,
                declaration: crate::StableDefinitionFingerprint::for_test(1),
                signature: crate::StableDefinitionFingerprint::for_test(2),
                body_or_initializer: Some(crate::StableDefinitionFingerprint::for_test(3)),
                precision: crate::StableDefinitionFingerprintPrecision::SignatureAndBody,
            },
            body: rue_air::SemanticBody {
                return_type: SemanticImportType::Unit,
                instructions: instructions.into(),
                places: Arc::from([]),
                strings: Arc::from([]),
                local_atoms: Arc::from([]),
                param_drops: Arc::from([]),
                borrow_slots: Arc::from([]),
                num_locals: 0,
                num_param_slots: 0,
                param_by_ref: Arc::from([]),
                param_writable: Arc::from([]),
                allow_unreachable_code: false,
                warnings: Arc::from([]),
                method_references: Arc::from([]),
            },
        }
    }

    fn instruction(data: DurableAirInstData) -> DurableAirInst {
        DurableAirInst {
            data,
            ty: SemanticImportType::Unit,
            anchor: DurableBodyAnchor { start: 0, end: 0 },
        }
    }

    #[test]
    fn canonical_specialization_identity_preserves_all_argument_kinds() {
        let function = StableDefinitionKey::for_test(
            ModuleId::from_logical_path("helpers.rue").unwrap(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            "helper",
            None,
        );
        let identity: rue_air::SemanticSpecializationIdentity<StableDefinitionKey, ModuleId> =
            rue_air::SemanticSpecializationIdentity {
                base: owner(),
                type_arguments: Arc::from([SemanticImportType::I32]),
                value_arguments: Arc::from([
                    SemanticImportConstValue::Bool(true),
                    SemanticImportConstValue::Type(SemanticImportType::I64),
                    SemanticImportConstValue::Function(function.clone()),
                    SemanticImportConstValue::Unit,
                ]),
            };
        assert_eq!(identity.base, owner());
        assert_eq!(identity.type_arguments.as_ref(), &[SemanticImportType::I32]);
        assert_eq!(
            identity.value_arguments.as_ref(),
            &[
                SemanticImportConstValue::Bool(true),
                SemanticImportConstValue::Type(SemanticImportType::I64),
                SemanticImportConstValue::Function(function),
                SemanticImportConstValue::Unit,
            ]
        );
    }

    #[test]
    fn projection_rejects_forward_instruction_references() {
        let candidate = payload(vec![instruction(DurableAirInstData::Ret(Some(0)))]);
        let mut work = DurableBodyWork::default();
        assert_eq!(
            candidate.project_semantic_body(&mut work),
            Err(DurableBodyProjectionFailure::ForwardInstructionReference)
        );
        assert_eq!(work.projection_attempts, 1);
        assert_eq!(work.projection_completions, 0);
        assert_eq!(work.projection_failures, 1);
    }

    #[test]
    fn projection_reports_retained_warning_metadata_accurately() {
        let mut candidate = payload(vec![]);
        candidate.body.warnings = Arc::from([rue_air::SemanticBodyWarning {
            kind: rue_error::WarningKind::UnreachableCode,
            anchor: DurableBodyAnchor { start: 0, end: 0 },
            labels: Arc::new([]),
            notes: Arc::new([]),
            helps: Arc::new([]),
            suggestions: Arc::new([]),
        }]);
        let mut work = DurableBodyWork::default();
        assert_eq!(
            candidate.project_semantic_body(&mut work),
            Err(DurableBodyProjectionFailure::WarningProducingBody)
        );
        assert_eq!(work.projection_attempts, 1);
        assert_eq!(work.projection_completions, 0);
        assert_eq!(work.projection_failures, 1);
    }

    #[test]
    fn body_projection_rejects_a_foreign_canonical_algebra_epoch() {
        let mut candidate = payload(vec![instruction(DurableAirInstData::UnitConst)]);
        candidate.semantic_schema_version.implementation_epoch += 1;
        let mut work = DurableBodyWork::default();
        assert_eq!(
            candidate.project_semantic_body(&mut work),
            Err(DurableBodyProjectionFailure::SchemaVersionMismatch)
        );
        assert_eq!(work.projection_failures, 1);
        assert_eq!(
            work.last_projection_failure,
            Some(DurableBodyProjectionFailure::SchemaVersionMismatch)
        );
    }

    #[test]
    fn specialized_preflight_failures_are_counted_without_false_completion() {
        for failure in [
            DurableBodyConversionFailure::MissingStableDefinition,
            DurableBodyConversionFailure::ForeignOwnerToken,
            DurableBodyConversionFailure::AmbiguousStableDefinition,
        ] {
            let mut work = DurableBodyWork::default();
            assert_eq!(
                record_specialized_conversion_failure(&mut work, failure),
                failure
            );
            assert_eq!(work.conversion_attempts, 1);
            assert_eq!(work.conversion_failures, 1);
            assert_eq!(work.atomic_discards, 1);
            assert_eq!(work.conversion_completions, 0);
        }
    }

    #[test]
    fn runtime_calls_round_trip_without_source_definition_edges() {
        let candidate = payload(vec![instruction(DurableAirInstData::RuntimeCall {
            runtime: rue_air::RuntimeCallKind::ToString,
            args: Arc::from([]),
        })]);
        assert!(candidate.referenced_definition_keys().is_empty());

        let mut work = DurableBodyWork::default();
        let projected = candidate.project_semantic_body(&mut work).unwrap();
        assert!(matches!(
            projected.instructions[0].data,
            rue_air::SemanticBodyInstData::RuntimeCall {
                runtime: rue_air::RuntimeCallKind::ToString,
                ..
            }
        ));
    }

    #[test]
    fn projection_rejects_non_permutation_source_order() {
        let candidate = payload(vec![
            instruction(DurableAirInstData::UnitConst),
            instruction(DurableAirInstData::StructInit {
                struct_key: rue_air::NominalInstanceKey::Named(StableDefinitionKey::for_test(
                    ModuleId::from_logical_path("main.rue").unwrap(),
                    StableDefinitionNamespace::Type,
                    StableDefinitionKind::Struct,
                    "S",
                    None,
                )),
                fields: Arc::from([0]),
                source_order: Arc::from([1]),
            }),
        ]);
        let mut work = DurableBodyWork::default();
        assert_eq!(
            candidate.project_semantic_body(&mut work),
            Err(DurableBodyProjectionFailure::InvalidSourceOrder)
        );
        assert_eq!(work.projection_failures, 1);
    }

    #[test]
    fn builtin_nominals_reject_unknown_names_and_wrong_kinds() {
        use rue_air::SemanticImportNominalKind::{Enum, Struct};
        assert_eq!(
            canonical_builtin_nominal(&Arc::from("MissingBuiltin"), Struct),
            Err(DurableBodyConversionFailure::WrongDefinitionKind)
        );
        assert_eq!(
            canonical_builtin_nominal(&Arc::from("StrBuf"), Struct),
            Err(DurableBodyConversionFailure::WrongDefinitionKind)
        );
        assert_eq!(
            canonical_builtin_nominal(&Arc::from("str"), Struct),
            Ok(SemanticImportType::BuiltinNominal {
                name: Arc::from("str"),
                kind: Struct,
            })
        );
        assert_eq!(
            canonical_builtin_nominal(&Arc::from("Arch"), Struct),
            Err(DurableBodyConversionFailure::WrongDefinitionKind)
        );
        assert_eq!(
            canonical_builtin_nominal(&Arc::from("Arch"), Enum),
            Ok(SemanticImportType::BuiltinNominal {
                name: Arc::from("Arch"),
                kind: Enum,
            })
        );
        assert_eq!(
            canonical_builtin_nominal(&Arc::from("str"), Enum),
            Err(DurableBodyConversionFailure::WrongDefinitionKind)
        );
    }
}
