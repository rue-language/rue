//! Exact-one-body semantic transaction used by the compiler body query.
//!
//! Module selections remain exact stable module-binding definition tokens:
//! that is the canonical compiler join key and retains both the local binding
//! and its module payload without guessing a target from a display path. Drop
//! glue is not selected by semantic body analysis. Its named-destructor edges
//! are emitted by CFG construction from the canonical type identities carried
//! by a successful body; a deterministic semantic failure has no CFG artifact,
//! while its type dependencies retain every input that can affect drop facts.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use lasso::Spur;
use rue_error::{CompileError, CompileErrors, ErrorKind};
use rue_rir::InstData;
use rue_span::{FileId, Span};

use super::body_endpoint::BodyEndpointProvider;
use super::{AnalyzedBodyOwnerEvent, AnalyzedFunction, BodyAnalysisState, BodyOwnerKind, BodySema};
use crate::{
    FunctionInstanceKey, NominalInstanceKey, SemanticBodyExport, SemanticDefinitionEndpoint,
    SemanticDefinitionToken, SemanticModuleToken, SemanticSpecializationIdentity,
    SemanticSpecializedBodyExport, StableDefinitionKind, Type, TypeInstanceKey,
};

#[derive(Debug, Clone)]
pub enum OneBodyRequest {
    Definition(SemanticDefinitionToken),
    Specialization {
        base: SemanticDefinitionToken,
        type_arguments: Vec<Type>,
        value_arguments: Vec<super::ConstValue>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneBodyInterruption {
    Canceled,
    EngineAbort,
}

#[derive(Debug)]
pub enum OneBodyCanonicalArtifact {
    Ordinary(Box<SemanticBodyExport>),
    Anonymous(Box<crate::SemanticAnonymousBodyExport>),
    Specialization(Box<SemanticSpecializedBodyExport>),
}

/// Stable, request-independent description of an anonymous nominal created by
/// one successful body transaction. The value deliberately contains no live
/// type-pool IDs, interner symbols, RIR handles, or source spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticProducedAnonymousNominal {
    pub identity: crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
    pub shape: SemanticProducedAnonymousNominalShape,
    pub type_captures: Arc<
        [(
            Arc<str>,
            TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        )],
    >,
    pub value_captures: Arc<
        [(
            Arc<str>,
            crate::CanonicalArgumentValue<SemanticDefinitionToken, SemanticModuleToken>,
        )],
    >,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticProducedAnonymousNominalShape {
    Struct {
        fields: Arc<
            [(
                Arc<str>,
                TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
            )],
        >,
        methods: Arc<[SemanticProducedAnonymousMethodSignature]>,
    },
    Enum {
        variants: Arc<
            [(
                Arc<str>,
                Arc<[TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>]>,
            )],
        >,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticProducedAnonymousMethodSignature {
    pub name: Arc<str>,
    pub has_self: bool,
    pub self_mode: crate::SemanticParameterMode,
    pub parameters: Arc<
        [(
            SemanticProducedAnonymousMethodType,
            crate::SemanticParameterMode,
            bool,
        )],
    >,
    pub result: SemanticProducedAnonymousMethodType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticProducedAnonymousMethodType {
    SelfType,
    Concrete(TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OneBodyDependency {
    Callable(FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>),
    Definition(SemanticDefinitionToken),
    Type(TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>),
}

#[derive(Debug)]
pub enum OneBodyTransactionOutcome {
    Success {
        artifact: OneBodyCanonicalArtifact,
        references: Arc<[OneBodyDependency]>,
        produced_anonymous_nominals: Arc<[SemanticProducedAnonymousNominal]>,
    },
    DeterministicFailure {
        errors: CompileErrors,
        references: Arc<[OneBodyDependency]>,
    },
    NonTerminal {
        reason: OneBodyNonTerminalReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OneBodyNonTerminalReason {
    Canceled,
    EngineAbort,
    TypedIncomplete(crate::SemanticBodyExportFailure),
}

impl OneBodyTransactionOutcome {
    pub fn references(&self) -> Option<&[OneBodyDependency]> {
        match self {
            Self::Success { references, .. } | Self::DeterministicFailure { references, .. } => {
                Some(references)
            }
            Self::NonTerminal { .. } => None,
        }
    }

    pub fn artifact_instruction_count(&self) -> Option<usize> {
        match self {
            Self::Success {
                artifact: OneBodyCanonicalArtifact::Ordinary(artifact),
                ..
            } => Some(artifact.body.instructions.len()),
            Self::Success {
                artifact: OneBodyCanonicalArtifact::Anonymous(artifact),
                ..
            } => Some(artifact.body.instructions.len()),
            Self::Success {
                artifact: OneBodyCanonicalArtifact::Specialization(artifact),
                ..
            } => Some(artifact.body.instructions.len()),
            Self::DeterministicFailure { .. } | Self::NonTerminal { .. } => None,
        }
    }

    pub fn non_terminal_reason(&self) -> Option<&OneBodyNonTerminalReason> {
        match self {
            Self::NonTerminal { reason } => Some(reason),
            Self::Success { .. } | Self::DeterministicFailure { .. } => None,
        }
    }
}

#[cfg(test)]
mod exact_demand_tests {
    use super::*;

    #[test]
    fn str_materialization_demand_is_exact_to_requested_identity() {
        let base = FunctionInstanceKey::<&str, &str>::Definition("choose");
        let ordinary = FunctionInstanceKey::Specialization {
            base: Box::new(base.clone()),
            arguments: crate::CanonicalArguments {
                types: Arc::from([TypeInstanceKey::I32]),
                values: Arc::from([]),
            },
        };
        assert!(!function_instance_contains_str(&base));
        assert!(!function_instance_contains_str(&ordinary));

        let with_str = FunctionInstanceKey::Specialization {
            base: Box::new(base),
            arguments: crate::CanonicalArguments {
                types: Arc::from([TypeInstanceKey::PtrConst(Box::new(
                    TypeInstanceKey::BuiltinNominal {
                        kind: crate::AnonymousNominalKind::Struct,
                        name: Arc::from("str"),
                    },
                ))]),
                values: Arc::from([]),
            },
        };
        assert!(function_instance_contains_str(&with_str));
    }
}

fn interruption_outcome(interruption: OneBodyInterruption) -> OneBodyTransactionOutcome {
    OneBodyTransactionOutcome::NonTerminal {
        reason: match interruption {
            OneBodyInterruption::Canceled => OneBodyNonTerminalReason::Canceled,
            OneBodyInterruption::EngineAbort => OneBodyNonTerminalReason::EngineAbort,
        },
    }
}

fn stable_token<H: OneBodyPublicationHost>(
    host: &H,
    file: u32,
    name: &str,
    owner: Option<&str>,
    kind: StableDefinitionKind,
) -> Result<SemanticDefinitionToken, crate::SemanticBodyExportFailure> {
    host.publication_stable_token(file, name, owner, kind)
}

fn target_dependency<H: OneBodyPublicationHost>(
    host: &H,
    target: &super::NamedConstDependencyTargetEvent,
) -> Result<OneBodyDependency, crate::SemanticBodyExportFailure> {
    use super::{DeclarationTypeDependencyTargetKind as TK, NamedConstDependencyTargetEvent as T};
    match target {
        T::ValueConst { file, name } => {
            stable_token(host, *file, name, None, StableDefinitionKind::ValueConst)
                .map(OneBodyDependency::Definition)
        }
        T::FreeFunction { file, name } => {
            stable_token(host, *file, name, None, StableDefinitionKind::Function)
                .map(|token| OneBodyDependency::Callable(FunctionInstanceKey::Definition(token)))
        }
        T::NamedType { file, name, kind } => {
            if let Some(symbol) = host.publication_name_symbol(name.as_str()) {
                let builtin_kind = if host.publication_is_builtin_enum(symbol) {
                    Some(crate::AnonymousNominalKind::Enum)
                } else if host.publication_is_builtin_or_generated_struct(symbol) {
                    Some(crate::AnonymousNominalKind::Struct)
                } else {
                    None
                };
                if let Some(kind) = builtin_kind {
                    return Ok(OneBodyDependency::Type(TypeInstanceKey::BuiltinNominal {
                        kind,
                        name: Arc::from(name.as_str()),
                    }));
                }
            }
            let kind = match kind {
                TK::Struct => StableDefinitionKind::Struct,
                TK::Enum => StableDefinitionKind::Enum,
                TK::ValueConst => StableDefinitionKind::ValueConst,
            };
            let token = stable_token(host, *file, name, None, kind)?;
            if matches!(
                kind,
                StableDefinitionKind::Struct | StableDefinitionKind::Enum
            ) {
                Ok(OneBodyDependency::Type(TypeInstanceKey::Nominal(
                    NominalInstanceKey::Named(token),
                )))
            } else {
                Ok(OneBodyDependency::Definition(token))
            }
        }
        T::ModuleBinding { file, name } => {
            stable_token(host, *file, name, None, StableDefinitionKind::ModuleBinding)
                .map(OneBodyDependency::Definition)
        }
    }
}

#[derive(Debug)]
pub(super) enum ReferenceProjectionFailure {
    TypedIncomplete(crate::SemanticBodyExportFailure),
    EngineAbort,
}

impl From<crate::SemanticBodyExportFailure> for ReferenceProjectionFailure {
    fn from(reason: crate::SemanticBodyExportFailure) -> Self {
        Self::TypedIncomplete(reason)
    }
}

/// Representation-neutral facts and ledgers consumed by the canonical
/// one-body publication funnel.
///
/// Semantic analysis and specialization finish before this boundary. The
/// funnel owns stable positive-reference projection, deterministic failure
/// classification, produced-anonymous export, and success serialization. A
/// host exposes point operations and immutable request-local ledgers; it does
/// not lend an analyzer or a declaration epoch to the algorithm.
pub(super) trait OneBodyPublicationHost:
    super::semantic_body_export::SemanticBodyExportHost
{
    fn publication_stable_token(
        &self,
        file: u32,
        name: &str,
        owner: Option<&str>,
        kind: StableDefinitionKind,
    ) -> Result<SemanticDefinitionToken, crate::SemanticBodyExportFailure>;
    fn publication_name_symbol(&self, name: &str) -> Option<Spur>;
    fn publication_resolve_symbol(&self, symbol: &Spur) -> &str;
    fn publication_is_builtin_or_generated_struct(&self, symbol: Spur) -> bool;
    fn publication_is_builtin_enum(&self, symbol: Spur) -> bool;
    fn publication_body_function_identity(
        &self,
        symbol: Spur,
    ) -> Result<
        FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    >;
    fn publication_method_identity(
        &self,
        structure: crate::StructId,
        method: Spur,
    ) -> Result<
        FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        ReferenceProjectionFailure,
    >;
    fn publication_canonical_type_instance(
        &self,
        ty: Type,
    ) -> Result<
        TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    >;
    fn publication_canonical_argument_value(
        &self,
        value: super::ConstValue,
    ) -> Result<
        crate::CanonicalArgumentValue<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    >;

    fn publication_specialization_dependencies(
        &self,
    ) -> &[(
        AnalyzedBodyOwnerEvent,
        FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    )];
    fn publication_callable_dependencies(&self) -> &[(AnalyzedBodyOwnerEvent, Spur)];
    fn publication_named_dependencies(&self) -> &[super::BodyNamedDependencyEvent];
    fn publication_type_dependencies(&self) -> &[super::DeclarationTypeDependencyEvent];
    fn publication_type_call_head_dependencies(
        &self,
    ) -> &[super::DeclarationTypeCallHeadDependencyEvent];

    fn publication_anonymous_identities(
        &self,
    ) -> Vec<(
        Type,
        crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
    )>;
    fn publication_initial_anonymous_contains(
        &self,
        identity: &crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
    ) -> bool;
    fn publication_type_pool(&self) -> &crate::TypeInternPool;
    fn publication_anonymous_method_signatures(
        &self,
        structure: crate::StructId,
    ) -> &[super::AnonMethodSig];
    fn publication_anonymous_type_captures(&self, structure: crate::StructId) -> Vec<(Spur, Type)>;
    fn publication_anonymous_value_captures(
        &self,
        structure: crate::StructId,
    ) -> Vec<(Spur, super::ConstValue)>;

    fn publication_inference_failure_incomplete(&self) -> bool;
    fn publication_recovered_errors(&self) -> &[CompileError];
}

impl OneBodyPublicationHost for BodySema<'_> {
    fn publication_stable_token(
        &self,
        file: u32,
        name: &str,
        owner: Option<&str>,
        kind: StableDefinitionKind,
    ) -> Result<SemanticDefinitionToken, crate::SemanticBodyExportFailure> {
        self.stable_definition_token(file, name, owner, kind)
    }

    fn publication_name_symbol(&self, name: &str) -> Option<Spur> {
        self.interner.get(name)
    }

    fn publication_resolve_symbol(&self, symbol: &Spur) -> &str {
        self.interner.resolve(symbol)
    }

    fn publication_is_builtin_or_generated_struct(&self, symbol: Spur) -> bool {
        self.endpoint_facts().is_builtin_or_generated_struct(symbol)
    }

    fn publication_is_builtin_enum(&self, symbol: Spur) -> bool {
        self.endpoint_facts().is_builtin_enum(symbol)
    }

    fn publication_body_function_identity(
        &self,
        symbol: Spur,
    ) -> Result<
        FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        self.body_function_identity(symbol)
    }

    fn publication_method_identity(
        &self,
        structure: crate::StructId,
        method: Spur,
    ) -> Result<
        FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        ReferenceProjectionFailure,
    > {
        let Some(info) = self.endpoint_facts().method_info(structure, method) else {
            return Err(ReferenceProjectionFailure::EngineAbort);
        };
        let symbol = self.method_symbol(structure, self.interner.resolve(&method), info.has_self);
        let Some(symbol) = self.interner.get(&symbol) else {
            return Err(ReferenceProjectionFailure::EngineAbort);
        };
        self.body_function_identity(symbol).map_err(Into::into)
    }

    fn publication_canonical_type_instance(
        &self,
        ty: Type,
    ) -> Result<
        TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        self.canonical_type_instance(ty)
    }

    fn publication_canonical_argument_value(
        &self,
        value: super::ConstValue,
    ) -> Result<
        crate::CanonicalArgumentValue<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        self.canonical_argument_value(value)
    }

    fn publication_specialization_dependencies(
        &self,
    ) -> &[(
        AnalyzedBodyOwnerEvent,
        FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    )] {
        &self.body_specialization_dependencies
    }

    fn publication_callable_dependencies(&self) -> &[(AnalyzedBodyOwnerEvent, Spur)] {
        &self.body_callable_dependencies
    }

    fn publication_named_dependencies(&self) -> &[super::BodyNamedDependencyEvent] {
        &self.body_named_dependencies
    }

    fn publication_type_dependencies(&self) -> &[super::DeclarationTypeDependencyEvent] {
        &self.declaration_type_dependencies
    }

    fn publication_type_call_head_dependencies(
        &self,
    ) -> &[super::DeclarationTypeCallHeadDependencyEvent] {
        &self.declaration_type_call_head_dependencies
    }

    fn publication_anonymous_identities(
        &self,
    ) -> Vec<(
        Type,
        crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
    )> {
        self.canonical_anonymous_types
            .iter()
            .map(|(ty, identity)| (*ty, identity.clone()))
            .collect()
    }

    fn publication_initial_anonymous_contains(
        &self,
        identity: &crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
    ) -> bool {
        self.one_body_initial_anonymous_identities
            .contains(identity)
    }

    fn publication_type_pool(&self) -> &crate::TypeInternPool {
        &self.type_pool
    }

    fn publication_anonymous_method_signatures(
        &self,
        structure: crate::StructId,
    ) -> &[super::AnonMethodSig] {
        self.anon_struct_method_sigs
            .get(&structure)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn publication_anonymous_type_captures(&self, structure: crate::StructId) -> Vec<(Spur, Type)> {
        self.anon_struct_type_subst
            .get(&structure)
            .into_iter()
            .flat_map(|captures| captures.iter())
            .map(|(name, ty)| (*name, *ty))
            .collect()
    }

    fn publication_anonymous_value_captures(
        &self,
        structure: crate::StructId,
    ) -> Vec<(Spur, super::ConstValue)> {
        self.anon_struct_captured_values
            .get(&structure)
            .into_iter()
            .flat_map(|captures| captures.iter())
            .map(|(name, value)| (*name, *value))
            .collect()
    }

    fn publication_inference_failure_incomplete(&self) -> bool {
        self.one_body_inference_failure_incomplete
    }

    fn publication_recovered_errors(&self) -> &[CompileError] {
        &self.one_body_recovered_errors
    }
}

fn projection_failure_outcome(failure: ReferenceProjectionFailure) -> OneBodyTransactionOutcome {
    OneBodyTransactionOutcome::NonTerminal {
        reason: match failure {
            ReferenceProjectionFailure::TypedIncomplete(reason) => {
                OneBodyNonTerminalReason::TypedIncomplete(reason)
            }
            ReferenceProjectionFailure::EngineAbort => OneBodyNonTerminalReason::EngineAbort,
        },
    }
}

fn semantic_parameter_mode(mode: rue_rir::RirParamMode) -> crate::SemanticParameterMode {
    match mode {
        rue_rir::RirParamMode::Normal => crate::SemanticParameterMode::Value,
        rue_rir::RirParamMode::Borrow => crate::SemanticParameterMode::Borrow,
        rue_rir::RirParamMode::Inout => crate::SemanticParameterMode::Inout,
    }
}

fn produced_method_type<H: OneBodyPublicationHost>(
    host: &H,
    ty: &super::AnonMethodType,
    owner: &crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
) -> Result<SemanticProducedAnonymousMethodType, crate::SemanticBodyExportFailure> {
    fn concrete<H: OneBodyPublicationHost>(
        host: &H,
        ty: &super::AnonMethodType,
        owner: &crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
    ) -> Result<
        TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        Ok(match ty {
            super::AnonMethodType::SelfType => {
                TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(owner.clone()))
            }
            super::AnonMethodType::Concrete(ty) => host.publication_canonical_type_instance(*ty)?,
            super::AnonMethodType::Array { element, len } => TypeInstanceKey::Array {
                element: Box::new(concrete(host, element, owner)?),
                len: *len,
            },
            super::AnonMethodType::PtrConst(pointee) => {
                TypeInstanceKey::PtrConst(Box::new(concrete(host, pointee, owner)?))
            }
            super::AnonMethodType::PtrMut(pointee) => {
                TypeInstanceKey::PtrMut(Box::new(concrete(host, pointee, owner)?))
            }
            super::AnonMethodType::Syntax(_) => {
                return Err(crate::SemanticBodyExportFailure::UnsupportedType);
            }
        })
    }

    Ok(match ty {
        super::AnonMethodType::SelfType => SemanticProducedAnonymousMethodType::SelfType,
        _ => SemanticProducedAnonymousMethodType::Concrete(concrete(host, ty, owner)?),
    })
}

fn produced_anonymous_nominals<H: OneBodyPublicationHost>(
    host: &H,
) -> Result<Arc<[SemanticProducedAnonymousNominal]>, crate::SemanticBodyExportFailure> {
    let mut identities = host
        .publication_anonymous_identities()
        .into_iter()
        .filter(|(_, identity)| !host.publication_initial_anonymous_contains(identity))
        .collect::<Vec<_>>();
    // Deterministic total order over the direct producer keys so the exported
    // nominal stream never depends on `HashMap` iteration order.
    identities.sort_by(|(_, left), (_, right)| super::anon_structs::anonymous_key_cmp(left, right));

    identities
        .into_iter()
        .map(|(ty, identity)| {
            let (shape, type_captures, value_captures) = match (ty.kind(), identity.kind) {
                (crate::TypeKind::Struct(struct_id), crate::AnonymousNominalKind::Struct) => {
                    let definition = host.publication_type_pool().struct_def(struct_id);
                    let fields = definition
                        .fields
                        .iter()
                        .map(|field| {
                            Ok((
                                Arc::from(field.name.as_str()),
                                host.publication_canonical_type_instance(field.ty)?,
                            ))
                        })
                        .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                    let methods = host
                        .publication_anonymous_method_signatures(struct_id)
                        .iter()
                        .map(|method| {
                            let parameters = method
                                .param_types
                                .iter()
                                .zip(&method.param_modes)
                                .zip(&method.param_comptime)
                                .map(|((ty, mode), is_comptime)| {
                                    Ok((
                                        produced_method_type(host, ty, &identity)?,
                                        semantic_parameter_mode(*mode),
                                        *is_comptime,
                                    ))
                                })
                                .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                            Ok(SemanticProducedAnonymousMethodSignature {
                                name: Arc::from(host.publication_resolve_symbol(&method.name)),
                                has_self: method.has_self,
                                self_mode: semantic_parameter_mode(method.self_mode),
                                parameters: parameters.into(),
                                result: produced_method_type(host, &method.return_type, &identity)?,
                            })
                        })
                        .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                    let mut type_captures: Vec<(
                        Arc<str>,
                        TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
                    )> = host
                        .publication_anonymous_type_captures(struct_id)
                        .into_iter()
                        .map(|(name, ty)| {
                            Ok((
                                Arc::<str>::from(host.publication_resolve_symbol(&name)),
                                host.publication_canonical_type_instance(ty)?,
                            ))
                        })
                        .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                    type_captures.sort_by(|left, right| left.0.cmp(&right.0));
                    let mut value_captures: Vec<(
                        Arc<str>,
                        crate::CanonicalArgumentValue<SemanticDefinitionToken, SemanticModuleToken>,
                    )> = host
                        .publication_anonymous_value_captures(struct_id)
                        .into_iter()
                        .map(|(name, value)| {
                            Ok((
                                Arc::<str>::from(host.publication_resolve_symbol(&name)),
                                host.publication_canonical_argument_value(value)?,
                            ))
                        })
                        .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                    value_captures.sort_by(|left, right| left.0.cmp(&right.0));
                    (
                        SemanticProducedAnonymousNominalShape::Struct {
                            fields: fields.into(),
                            methods: methods.into(),
                        },
                        type_captures,
                        value_captures,
                    )
                }
                (crate::TypeKind::Enum(enum_id), crate::AnonymousNominalKind::Enum) => {
                    let definition = host.publication_type_pool().enum_def(enum_id);
                    let variants = definition
                        .variants
                        .iter()
                        .enumerate()
                        .map(|(index, name)| {
                            let payload = definition
                                .variant_payload(index)
                                .iter()
                                .map(|ty| host.publication_canonical_type_instance(*ty))
                                .collect::<Result<Vec<_>, _>>()?;
                            Ok((Arc::from(name.as_str()), Arc::from(payload)))
                        })
                        .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
                    (
                        SemanticProducedAnonymousNominalShape::Enum {
                            variants: variants.into(),
                        },
                        Vec::new(),
                        Vec::new(),
                    )
                }
                _ => return Err(crate::SemanticBodyExportFailure::UnsupportedType),
            };
            Ok(SemanticProducedAnonymousNominal {
                identity,
                shape,
                type_captures: type_captures.into(),
                value_captures: value_captures.into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Arc::from)
}

fn arguments_have_issuer(
    arguments: &crate::CanonicalArguments<SemanticDefinitionToken, SemanticModuleToken>,
    issuer: u64,
) -> bool {
    arguments.types.iter().all(|ty| type_has_issuer(ty, issuer))
        && arguments.values.iter().all(|value| match value {
            crate::CanonicalArgumentValue::Type(ty) => type_has_issuer(ty, issuer),
            crate::CanonicalArgumentValue::Function(function) => {
                function_has_issuer(function, issuer)
            }
            crate::CanonicalArgumentValue::Integer(_)
            | crate::CanonicalArgumentValue::Bool(_)
            | crate::CanonicalArgumentValue::Unit
            | crate::CanonicalArgumentValue::String(_) => true,
        })
}

fn anonymous_nominal_has_issuer(
    nominal: &crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
    issuer: u64,
) -> bool {
    let producer = match &nominal.producer {
        crate::StableProducerId::Definition(definition) => definition.issuer() == issuer,
        crate::StableProducerId::Function(function) => function_has_issuer(function, issuer),
    };
    producer && arguments_have_issuer(&nominal.arguments, issuer)
}

fn type_has_issuer(
    ty: &TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    issuer: u64,
) -> bool {
    match ty {
        TypeInstanceKey::Nominal(NominalInstanceKey::Builtin { .. }) => true,
        TypeInstanceKey::Nominal(NominalInstanceKey::Named(definition)) => {
            definition.issuer() == issuer
        }
        TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(nominal)) => {
            anonymous_nominal_has_issuer(nominal, issuer)
        }
        TypeInstanceKey::Array { element, .. }
        | TypeInstanceKey::Slice { element, .. }
        | TypeInstanceKey::PtrConst(element)
        | TypeInstanceKey::PtrMut(element) => type_has_issuer(element, issuer),
        TypeInstanceKey::Module(module) => module.issuer() == issuer,
        TypeInstanceKey::I8
        | TypeInstanceKey::I16
        | TypeInstanceKey::I32
        | TypeInstanceKey::I64
        | TypeInstanceKey::U8
        | TypeInstanceKey::U16
        | TypeInstanceKey::U32
        | TypeInstanceKey::U64
        | TypeInstanceKey::Bool
        | TypeInstanceKey::Unit
        | TypeInstanceKey::Never
        | TypeInstanceKey::ComptimeType
        | TypeInstanceKey::BuiltinNominal { .. }
        | TypeInstanceKey::GenericParameter(_) => true,
    }
}

fn function_has_issuer(
    function: &FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    issuer: u64,
) -> bool {
    match function {
        FunctionInstanceKey::Definition(definition) => definition.issuer() == issuer,
        FunctionInstanceKey::Specialization { base, arguments } => {
            function_has_issuer(base, issuer) && arguments_have_issuer(arguments, issuer)
        }
        FunctionInstanceKey::AnonymousMember { owner, .. } => type_has_issuer(owner, issuer),
        FunctionInstanceKey::DropGlue(ty) => type_has_issuer(ty, issuer),
    }
}

fn dependency_has_issuer(dependency: &OneBodyDependency, issuer: u64) -> bool {
    match dependency {
        OneBodyDependency::Callable(function) => function_has_issuer(function, issuer),
        OneBodyDependency::Definition(definition) => definition.issuer() == issuer,
        OneBodyDependency::Type(ty) => type_has_issuer(ty, issuer),
    }
}

fn body_references<H: OneBodyPublicationHost>(
    host: &H,
    owner: Option<super::BodyOwnerToken>,
    referenced_functions: &HashSet<Spur>,
    referenced_methods: &HashSet<(crate::StructId, Spur)>,
    specializations: &[(
        Spur,
        SemanticSpecializationIdentity<SemanticDefinitionToken, SemanticModuleToken>,
    )],
    specialization_instances: &[FunctionInstanceKey<
        SemanticDefinitionToken,
        SemanticModuleToken,
    >],
) -> Result<Arc<[OneBodyDependency]>, ReferenceProjectionFailure> {
    let mut references = BTreeSet::new();
    for function in referenced_functions {
        let identity = host.publication_body_function_identity(*function)?;
        references.insert(OneBodyDependency::Callable(identity));
    }
    for (structure, method) in referenced_methods {
        let identity = host.publication_method_identity(*structure, *method)?;
        references.insert(OneBodyDependency::Callable(identity));
    }
    for (source, identity) in host.publication_specialization_dependencies() {
        if owner.is_some_and(|owner| source.token() != Some(owner)) {
            continue;
        }
        references.insert(OneBodyDependency::Callable(identity.clone()));
    }
    for (source, symbol) in host.publication_callable_dependencies() {
        if owner.is_some_and(|owner| source.token() != Some(owner)) {
            continue;
        }
        let identity = host.publication_body_function_identity(*symbol)?;
        references.insert(OneBodyDependency::Callable(identity));
    }
    for event in host.publication_named_dependencies() {
        if owner.is_some_and(|owner| event.source.token() != Some(owner)) {
            continue;
        }
        references.insert(target_dependency(host, &event.target)?);
    }
    for event in host.publication_type_dependencies() {
        if event.dependency_kind != super::DeclarationTypeDependencyKind::Body
            || owner.is_some_and(|owner| event.source_token != Some(owner))
        {
            continue;
        }
        // Anonymous types have producer-relative structural identities. The
        // declaration dependency recorder still carries their diagnostic
        // display name, but that name is not (and must never be treated as) a
        // stable named declaration endpoint. Their exact dependency is
        // exported through the body type and produced-anonymous projections.
        if event.target_name.starts_with("__anon_struct_")
            || event.target_name.starts_with("__anon_enum_")
        {
            continue;
        }
        if host
            .publication_name_symbol(&event.target_name)
            .is_some_and(|symbol| {
                host.publication_is_builtin_or_generated_struct(symbol)
                    || host.publication_is_builtin_enum(symbol)
            })
        {
            continue;
        }
        let kind = match event.target_kind {
            super::DeclarationTypeDependencyTargetKind::Struct => StableDefinitionKind::Struct,
            super::DeclarationTypeDependencyTargetKind::Enum => StableDefinitionKind::Enum,
            super::DeclarationTypeDependencyTargetKind::ValueConst => {
                StableDefinitionKind::ValueConst
            }
        };
        let token = stable_token(host, event.target_file, &event.target_name, None, kind)?;
        if matches!(
            kind,
            StableDefinitionKind::Struct | StableDefinitionKind::Enum
        ) {
            references.insert(OneBodyDependency::Type(TypeInstanceKey::Nominal(
                NominalInstanceKey::Named(token),
            )));
        } else {
            references.insert(OneBodyDependency::Definition(token));
        }
    }
    for event in host.publication_type_call_head_dependencies() {
        if event.dependency_kind != super::DeclarationTypeDependencyKind::Body
            || owner.is_some_and(|owner| event.source_token != Some(owner))
        {
            continue;
        }
        let token = stable_token(
            host,
            event.callable_file,
            &event.callable_name,
            None,
            StableDefinitionKind::Function,
        )?;
        references.insert(OneBodyDependency::Definition(token));
    }
    for (_, identity) in specializations {
        references.remove(&OneBodyDependency::Callable(
            FunctionInstanceKey::Definition(identity.base),
        ));
    }
    // Anonymous bodies do not have an ordinary owner token, so their
    // value-specialized generic calls can be absent from the observer stream.
    // Add only those missing exact instances here; type-only calls already use
    // the established observer path and retain its producer ordering.
    references.extend(
        specialization_instances
            .iter()
            .filter(|instance| {
                matches!(
                    instance,
                    FunctionInstanceKey::Specialization { arguments, .. }
                        if !arguments.values.is_empty()
                ) && !host
                    .publication_specialization_dependencies()
                    .iter()
                    .any(|(_, recorded)| recorded == *instance)
            })
            .cloned()
            .map(OneBodyDependency::Callable),
    );
    for (source, identity) in host.publication_specialization_dependencies() {
        if owner.is_some_and(|owner| source.token() != Some(owner)) {
            continue;
        }
        if let FunctionInstanceKey::Specialization { base, .. } = identity {
            references.remove(&OneBodyDependency::Callable((**base).clone()));
        }
    }
    let references: Arc<[OneBodyDependency]> = references.into_iter().collect::<Vec<_>>().into();
    if let Some(owner) = owner
        && references
            .iter()
            .any(|dependency| !dependency_has_issuer(dependency, owner.issuer()))
    {
        return Err(ReferenceProjectionFailure::TypedIncomplete(
            crate::SemanticBodyExportFailure::ForeignStableIdentityIssuer,
        ));
    }
    Ok(references)
}

fn fail<H: OneBodyPublicationHost>(
    host: &H,
    owner: Option<super::BodyOwnerToken>,
    error: CompileError,
) -> OneBodyTransactionOutcome {
    if host.publication_inference_failure_incomplete() {
        return OneBodyTransactionOutcome::NonTerminal {
            reason: OneBodyNonTerminalReason::TypedIncomplete(
                crate::SemanticBodyExportFailure::MissingStableIdentity,
            ),
        };
    }
    if matches!(
        error.kind,
        ErrorKind::InvalidCompilerInput(_)
            | ErrorKind::CompilerProducerInvariant(_)
            | ErrorKind::CompilerResourceLimit(_)
            | ErrorKind::CompilerResourceExhaustion(_)
            | ErrorKind::OutputPublication(_)
            | ErrorKind::InternalError(_)
            | ErrorKind::InternalCodegenError(_)
    ) {
        return OneBodyTransactionOutcome::NonTerminal {
            reason: OneBodyNonTerminalReason::EngineAbort,
        };
    }
    let errors = if host.publication_recovered_errors().is_empty() {
        CompileErrors::from(error)
    } else {
        let mut errors = CompileErrors::new();
        for error in host.publication_recovered_errors() {
            errors.push(error.clone());
        }
        errors
    };
    match body_references(host, owner, &HashSet::new(), &HashSet::new(), &[], &[]) {
        Ok(references) => OneBodyTransactionOutcome::DeterministicFailure { errors, references },
        Err(failure) => projection_failure_outcome(failure),
    }
}

fn finalize_ordinary(
    sema: &BodySema<'_>,
    owner: super::BodyOwnerToken,
    body_span: Span,
    function: AnalyzedFunction,
    warnings: Vec<rue_error::CompileWarning>,
    strings: Vec<String>,
    referenced_functions: HashSet<Spur>,
    referenced_methods: HashSet<(crate::StructId, Spur)>,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    let (function, specializations, specialization_instances) =
        match crate::specialize::select_one_body_specializations(sema, function) {
            Ok(selected) => selected,
            Err(error) => return fail(sema, Some(owner), error),
        };
    publish_ordinary(
        sema,
        owner,
        body_span,
        function,
        warnings,
        strings,
        referenced_functions,
        referenced_methods,
        specializations,
        specialization_instances,
        interruption,
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_ordinary<H: OneBodyPublicationHost>(
    host: &H,
    owner: super::BodyOwnerToken,
    body_span: Span,
    function: AnalyzedFunction,
    warnings: Vec<rue_error::CompileWarning>,
    strings: Vec<String>,
    referenced_functions: HashSet<Spur>,
    referenced_methods: HashSet<(crate::StructId, Spur)>,
    specializations: Vec<(
        Spur,
        SemanticSpecializationIdentity<SemanticDefinitionToken, SemanticModuleToken>,
    )>,
    specialization_instances: Vec<
        FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    >,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    let references = match body_references(
        host,
        Some(owner),
        &referenced_functions,
        &referenced_methods,
        &specializations,
        &specialization_instances,
    ) {
        Ok(references) => references,
        Err(failure) => return projection_failure_outcome(failure),
    };
    if let Some(interruption) = interruption {
        return interruption_outcome(interruption);
    }
    let specialized_calls = specializations.iter().cloned().collect::<HashMap<_, _>>();
    let produced_anonymous_nominals = match produced_anonymous_nominals(host) {
        Ok(produced) => produced,
        Err(reason) => {
            return OneBodyTransactionOutcome::NonTerminal {
                reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
            };
        }
    };
    match super::semantic_body_export::export_body(
        host,
        owner,
        body_span,
        &function,
        &strings,
        &warnings,
        Some(&specialized_calls),
    ) {
        Ok(artifact) => OneBodyTransactionOutcome::Success {
            artifact: OneBodyCanonicalArtifact::Ordinary(Box::new(artifact)),
            references,
            produced_anonymous_nominals,
        },
        Err(reason) => OneBodyTransactionOutcome::NonTerminal {
            reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
        },
    }
}

pub(super) fn analyze_one_body(
    mut sema: BodySema<'_>,
    state: BodyAnalysisState<'_>,
    request: OneBodyRequest,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    // The request control state is authoritative and dominates setup,
    // semantic diagnostics, canonicalization, and publication alike.
    if let Some(interruption) = interruption {
        return interruption_outcome(interruption);
    }
    sema.one_body_initial_anonymous_identities = sema
        .canonical_anonymous_types
        .values()
        .filter(|identity| {
            sema.one_body_requested_producer
                .as_ref()
                .is_none_or(|producer| &identity.producer != producer)
        })
        // The well-known `Option` registry install materializes its nominals
        // before this baseline is taken, but the body that binds them must OWN
        // them: excluding them here makes `produced_anonymous_nominals` export
        // them, so the registry-rooted identity is durable across composition
        // instead of appearing to be a pre-existing import with no producer.
        .filter(|identity| !sema.well_known_option_identities.contains(*identity))
        .cloned()
        .collect();
    sema.one_body_error_recovery = true;
    sema.one_body_recovered_errors.clear();
    sema.one_body_inference_failure_incomplete = false;
    if let Err(error) = sema.get_or_create_str_struct(Span::default()) {
        return fail(&sema, None, error);
    }
    let infer_ctx = sema.build_inference_context();
    match request {
        OneBodyRequest::Specialization {
            base,
            type_arguments,
            value_arguments,
        } => {
            let Some(endpoint) = sema.endpoint_facts().definition_endpoint(base) else {
                return fail(
                    &sema,
                    None,
                    CompileError::without_span(ErrorKind::InvalidCompilerInput(
                        "specialization base has no current stable endpoint".into(),
                    )),
                );
            };
            let Some(source_name) = sema.interner.get(endpoint.name.as_ref()) else {
                return fail(
                    &sema,
                    None,
                    CompileError::without_span(ErrorKind::UndefinedFunction(
                        endpoint.name.to_string(),
                    )),
                );
            };
            let Some(base_name) = sema
                .endpoint_facts()
                .function_by_file_name(FileId::new(endpoint.file), source_name)
            else {
                return fail(
                    &sema,
                    None,
                    CompileError::without_span(ErrorKind::UndefinedFunction(
                        endpoint.name.to_string(),
                    )),
                );
            };
            let key = crate::specialize::SpecializationKey {
                base_name,
                type_args: type_arguments,
                value_args: value_arguments,
            };
            let base_name = key.base_name;
            let base_info = sema.endpoint_facts().function_info(base_name);
            let owner = base_info.as_ref().map(|info| {
                sema.body_owner_token(
                    info.file_id,
                    sema.interner.resolve(&sema.source_function_name(base_name)),
                    None,
                    BodyOwnerKind::FreeFunction,
                )
            });
            let specialized =
                match crate::specialize::analyze_one_specialization(&mut sema, &infer_ctx, key) {
                    Ok(body) => body,
                    Err(error) => return fail(&sema, owner, error),
                };
            let (function, nested, nested_instances) =
                match crate::specialize::select_one_body_specializations(
                    &sema,
                    specialized.function,
                ) {
                    Ok(value) => value,
                    Err(error) => return fail(&sema, owner, error),
                };
            let references = match body_references(
                &sema,
                owner,
                &specialized.referenced_functions,
                &specialized.referenced_methods,
                &nested,
                &nested_instances,
            ) {
                Ok(references) => references,
                Err(failure) => return projection_failure_outcome(failure),
            };
            let Some(base_info) = base_info else {
                return fail(
                    &sema,
                    owner,
                    CompileError::without_span(ErrorKind::UndefinedFunction(
                        sema.interner.resolve(&base_name).to_owned(),
                    )),
                );
            };
            let specialized_calls = nested.iter().cloned().collect::<HashMap<_, _>>();
            let produced_anonymous_nominals = match produced_anonymous_nominals(&sema) {
                Ok(produced) => produced,
                Err(reason) => {
                    return OneBodyTransactionOutcome::NonTerminal {
                        reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
                    };
                }
            };
            match sema.export_specialized_body(
                base_name,
                &base_info,
                &specialized.key.type_args,
                &specialized.key.value_args,
                &function,
                &specialized.local_strings,
                &specialized.warnings,
                &specialized_calls,
                &specialized.dependencies,
                specialized.dependency_boundary_complete,
            ) {
                Ok(artifact) if artifact.identity == specialized.identity => {
                    OneBodyTransactionOutcome::Success {
                        artifact: OneBodyCanonicalArtifact::Specialization(Box::new(artifact)),
                        references,
                        produced_anonymous_nominals,
                    }
                }
                Ok(_) => OneBodyTransactionOutcome::NonTerminal {
                    reason: OneBodyNonTerminalReason::TypedIncomplete(
                        crate::SemanticBodyExportFailure::MissingStableIdentity,
                    ),
                },
                Err(reason) => OneBodyTransactionOutcome::NonTerminal {
                    reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
                },
            }
        }
        OneBodyRequest::Definition(token) => {
            analyze_definition(&mut sema, &state, &infer_ctx, token, interruption)
        }
    }
}

pub(super) fn analyze_one_body_instance<K, M>(
    mut sema: BodySema<'_>,
    state: BodyAnalysisState<'_>,
    instance: &FunctionInstanceKey<K, M>,
    definition: impl Fn(&K) -> Result<SemanticDefinitionToken, crate::SemanticStableResolutionFailure>,
    module: impl Fn(&M) -> Result<SemanticModuleToken, crate::SemanticStableResolutionFailure>,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome
where
    K: Clone,
    M: Clone,
{
    let mapped = match instance.try_map_identities(&definition, &module) {
        Ok(mapped) => mapped,
        Err(_) => {
            return OneBodyTransactionOutcome::NonTerminal {
                reason: OneBodyNonTerminalReason::TypedIncomplete(
                    crate::SemanticBodyExportFailure::MissingStableIdentity,
                ),
            };
        }
    };
    // Materializing specialization arguments happens before `analyze_one_body`
    // performs its ordinary body setup. Register the lazy `str` builtin only
    // when this exact request carries it, so first-use `str` arguments have a
    // current-epoch endpoint without widening body-query demand.
    if function_instance_contains_str(&mapped)
        && let Err(error) = sema.get_or_create_str_struct(Span::default())
    {
        return fail(&sema, None, error);
    }
    if !matches!(mapped, FunctionInstanceKey::DropGlue(_)) {
        sema.one_body_requested_producer =
            Some(crate::StableProducerId::Function(Box::new(mapped.clone())));
    }
    let request = match mapped {
        FunctionInstanceKey::Definition(token) => OneBodyRequest::Definition(token),
        FunctionInstanceKey::Specialization { base, arguments } => {
            let FunctionInstanceKey::Definition(base) = *base else {
                return OneBodyTransactionOutcome::NonTerminal {
                    reason: OneBodyNonTerminalReason::TypedIncomplete(
                        crate::SemanticBodyExportFailure::MissingStableIdentity,
                    ),
                };
            };
            let type_arguments = match arguments
                .types
                .iter()
                .map(|ty| materialize_instance_type(&sema, ty))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(arguments) => arguments,
                Err(reason) => {
                    return OneBodyTransactionOutcome::NonTerminal {
                        reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
                    };
                }
            };
            let value_arguments = match arguments
                .values
                .iter()
                .map(|value| materialize_argument_value(&sema, value))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(arguments) => arguments,
                Err(reason) => {
                    return OneBodyTransactionOutcome::NonTerminal {
                        reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
                    };
                }
            };
            OneBodyRequest::Specialization {
                base,
                type_arguments,
                value_arguments,
            }
        }
        FunctionInstanceKey::AnonymousMember { owner, member } => {
            return analyze_anonymous_member(sema, *owner, member, interruption);
        }
        FunctionInstanceKey::DropGlue(_) => {
            return OneBodyTransactionOutcome::NonTerminal {
                reason: OneBodyNonTerminalReason::TypedIncomplete(
                    crate::SemanticBodyExportFailure::MissingStableIdentity,
                ),
            };
        }
    };
    analyze_one_body(sema, state, request, interruption)
}

fn function_instance_contains_str<D, M>(instance: &FunctionInstanceKey<D, M>) -> bool {
    fn arguments_contain_str<D, M>(arguments: &crate::CanonicalArguments<D, M>) -> bool {
        arguments.types.iter().any(type_instance_contains_str)
            || arguments.values.iter().any(|value| match value {
                crate::CanonicalArgumentValue::Type(ty) => type_instance_contains_str(ty),
                crate::CanonicalArgumentValue::Function(function) => {
                    function_instance_contains_str(function)
                }
                crate::CanonicalArgumentValue::Integer(_)
                | crate::CanonicalArgumentValue::Bool(_)
                | crate::CanonicalArgumentValue::Unit
                | crate::CanonicalArgumentValue::String(_) => false,
            })
    }

    fn type_instance_contains_str<D, M>(ty: &TypeInstanceKey<D, M>) -> bool {
        match ty {
            TypeInstanceKey::BuiltinNominal { name, .. }
            | TypeInstanceKey::Nominal(crate::NominalInstanceKey::Builtin { name, .. }) => {
                name.as_ref() == "str"
            }
            TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(identity)) => {
                arguments_contain_str(&identity.arguments)
                    || matches!(
                        &identity.producer,
                        crate::StableProducerId::Function(function)
                            if function_instance_contains_str(function)
                    )
            }
            TypeInstanceKey::Array { element, .. }
            | TypeInstanceKey::Slice { element, .. }
            | TypeInstanceKey::PtrConst(element)
            | TypeInstanceKey::PtrMut(element) => type_instance_contains_str(element),
            TypeInstanceKey::I8
            | TypeInstanceKey::I16
            | TypeInstanceKey::I32
            | TypeInstanceKey::I64
            | TypeInstanceKey::U8
            | TypeInstanceKey::U16
            | TypeInstanceKey::U32
            | TypeInstanceKey::U64
            | TypeInstanceKey::Bool
            | TypeInstanceKey::Unit
            | TypeInstanceKey::Never
            | TypeInstanceKey::ComptimeType
            | TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(_))
            | TypeInstanceKey::Module(_)
            | TypeInstanceKey::GenericParameter(_) => false,
        }
    }

    match instance {
        FunctionInstanceKey::Definition(_) => false,
        FunctionInstanceKey::Specialization { base, arguments } => {
            function_instance_contains_str(base) || arguments_contain_str(arguments)
        }
        FunctionInstanceKey::AnonymousMember { owner, .. }
        | FunctionInstanceKey::DropGlue(owner) => type_instance_contains_str(owner),
    }
}

fn analyze_anonymous_member(
    mut sema: BodySema<'_>,
    owner: TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    member: crate::AnonymousMemberKey,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    if let Some(interruption) = interruption {
        return interruption_outcome(interruption);
    }
    sema.one_body_initial_anonymous_identities =
        sema.canonical_anonymous_types.values().cloned().collect();
    sema.one_body_error_recovery = true;
    sema.one_body_recovered_errors.clear();
    sema.one_body_inference_failure_incomplete = false;
    if let Err(error) = sema.get_or_create_str_struct(Span::default()) {
        return fail(&sema, None, error);
    }
    let infer_ctx = sema.build_inference_context();
    let owner_type = match materialize_instance_type(&sema, &owner) {
        Ok(owner) => owner,
        Err(reason) => {
            return OneBodyTransactionOutcome::NonTerminal {
                reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
            };
        }
    };
    let Some(struct_id) = owner_type.as_struct() else {
        return OneBodyTransactionOutcome::NonTerminal {
            reason: OneBodyNonTerminalReason::TypedIncomplete(
                crate::SemanticBodyExportFailure::MissingStableIdentity,
            ),
        };
    };
    let Some(method_name) = sema.interner.get(member.name.as_ref()) else {
        return fail(
            &sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(member.name.to_string())),
        );
    };
    let Some(method_info) = sema.endpoint_facts().method_info(struct_id, method_name) else {
        return fail(
            &sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(member.name.to_string())),
        );
    };
    let expected_kind = if member.name.as_ref() == "__drop" {
        crate::AnonymousMemberKind::Destructor
    } else if method_info.has_self {
        crate::AnonymousMemberKind::Method
    } else {
        crate::AnonymousMemberKind::AssociatedFunction
    };
    if member.kind != expected_kind || method_info.struct_type != owner_type {
        return OneBodyTransactionOutcome::NonTerminal {
            reason: OneBodyNonTerminalReason::TypedIncomplete(
                crate::SemanticBodyExportFailure::MissingStableIdentity,
            ),
        };
    }
    let identity = FunctionInstanceKey::AnonymousMember {
        owner: Box::new(owner),
        member,
    };
    let full_name = sema.method_symbol(
        struct_id,
        sema.interner.resolve(&method_name),
        method_info.has_self,
    );
    let param_names = sema.param_arena.names(method_info.params);
    let param_types = sema.param_arena.types(method_info.params);
    let param_modes = sema.param_arena.modes(method_info.params);
    let param_comptime = sema.param_arena.comptime(method_info.params);
    let mut params = Vec::with_capacity(param_names.len() + usize::from(method_info.has_self));
    if method_info.has_self {
        params.push((
            sema.interner.get_or_intern("self"),
            method_info.struct_type,
            method_info.self_mode,
            false,
        ));
    }
    for index in 0..param_names.len() {
        params.push((
            param_names[index],
            param_types[index],
            param_modes[index],
            param_comptime[index],
        ));
    }
    let captured_values = sema
        .anon_struct_captured_values
        .get(&struct_id)
        .cloned()
        .unwrap_or_default();
    let type_subst = sema
        .anon_struct_type_subst
        .get(&struct_id)
        .cloned()
        .unwrap_or_default();
    let analyzed = if expected_kind == crate::AnonymousMemberKind::Destructor {
        sema.analyze_anon_destructor_body(
            &infer_ctx,
            &params,
            method_info.body,
            method_info.struct_type,
            &captured_values,
            method_name,
            &full_name,
            &type_subst,
        )
    } else {
        sema.analyze_method_body(
            &infer_ctx,
            method_name,
            method_info.has_self,
            method_info.return_type,
            &params,
            method_info.body,
            method_info.struct_type,
            &captured_values,
            &type_subst,
            method_info.self_is_mut,
        )
    };
    let (
        air,
        num_locals,
        num_param_slots,
        param_modes,
        warnings,
        strings,
        local_atoms,
        referenced_functions,
        referenced_methods,
    ) = match analyzed {
        Ok(value) => value,
        Err(error) => return fail(&sema, None, error),
    };
    let air = match crate::ValidatedAir::from_semantic_air_with_symbols(
        air,
        &sema.type_pool,
        sema.interner,
    ) {
        Ok(air) => air,
        Err(error) => return fail(&sema, None, error.into()),
    };
    let function = AnalyzedFunction {
        identity: identity.clone(),
        callable_kind: if expected_kind == crate::AnonymousMemberKind::Destructor {
            crate::AnalyzedCallableKind::Destructor
        } else {
            crate::AnalyzedCallableKind::Ordinary
        },
        ordinary_owner: None,
        name: full_name,
        implicit_drop_source: Some(super::ImplicitDropDependencySourceEvent::Anonymous),
        air,
        local_atoms,
        num_locals,
        num_param_slots,
        param_modes,
        allow_unreachable_code: false,
    };
    let (function, specializations, specialization_instances) =
        match crate::specialize::select_one_body_specializations(&sema, function) {
            Ok(value) => value,
            Err(error) => return fail(&sema, None, error),
        };
    let references = match body_references(
        &sema,
        None,
        &referenced_functions,
        &referenced_methods,
        &specializations,
        &specialization_instances,
    ) {
        Ok(references) => references,
        Err(failure) => return projection_failure_outcome(failure),
    };
    let specialized_calls = specializations.iter().cloned().collect::<HashMap<_, _>>();
    let produced_anonymous_nominals = match produced_anonymous_nominals(&sema) {
        Ok(produced) => produced,
        Err(reason) => {
            return OneBodyTransactionOutcome::NonTerminal {
                reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
            };
        }
    };
    match sema.export_anonymous_body_with_specializations(
        identity,
        method_info.span,
        &function,
        &strings,
        &warnings,
        &specialized_calls,
    ) {
        Ok(artifact) => OneBodyTransactionOutcome::Success {
            artifact: OneBodyCanonicalArtifact::Anonymous(Box::new(artifact)),
            references,
            produced_anonymous_nominals,
        },
        Err(reason) => OneBodyTransactionOutcome::NonTerminal {
            reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
        },
    }
}

pub(in crate::sema) fn materialize_argument_value(
    sema: &BodySema<'_>,
    value: &crate::CanonicalArgumentValue<SemanticDefinitionToken, SemanticModuleToken>,
) -> Result<super::ConstValue, crate::SemanticBodyExportFailure> {
    use crate::CanonicalArgumentValue as V;
    let facts = sema.endpoint_facts();
    Ok(match value {
        V::Integer(value) => super::ConstValue::Integer(*value),
        V::Bool(value) => super::ConstValue::Bool(*value),
        V::Type(value) => {
            super::ConstValue::Type(super::body_endpoint::resolve_instance_type(&facts, value)?)
        }
        V::Function(value) => {
            let FunctionInstanceKey::Definition(token) = value.as_ref() else {
                return Err(crate::SemanticBodyExportFailure::MissingStableIdentity);
            };
            let symbol = super::body_endpoint::resolve_free_function_symbol(&facts, *token)
                .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
            super::ConstValue::Function(symbol)
        }
        V::Unit => super::ConstValue::Unit,
        V::String(value) => super::ConstValue::String(sema.interner.get_or_intern(value.as_ref())),
    })
}

pub(in crate::sema) fn materialize_instance_type(
    sema: &BodySema<'_>,
    value: &TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
) -> Result<Type, crate::SemanticBodyExportFailure> {
    super::body_endpoint::resolve_instance_type(&sema.endpoint_facts(), value)
}

fn analyze_definition(
    sema: &mut BodySema<'_>,
    state: &BodyAnalysisState<'_>,
    infer_ctx: &super::InferenceContext,
    token: SemanticDefinitionToken,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    let facts = state.endpoint_facts();
    let Some(state_endpoint) = facts.definition_endpoint(token) else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "one-body request has no current stable endpoint".into(),
            )),
        );
    };
    match state_endpoint.kind {
        StableDefinitionKind::Function => {
            let selection = match select_ordinary_definition(&facts, state_endpoint) {
                Ok(selection) => selection,
                Err(error) => return fail(sema, None, error),
            };
            let OrdinaryDefinitionSelection {
                endpoint,
                name,
                info,
                declaration,
            } = selection;
            if info.is_generic || info.is_extern {
                return OneBodyTransactionOutcome::NonTerminal {
                    reason: OneBodyNonTerminalReason::TypedIncomplete(
                        crate::SemanticBodyExportFailure::UnsupportedGenericCall,
                    ),
                };
            }
            let InstData::FnDecl {
                params,
                return_type,
                body,
                ..
            } = &sema.rir.get(declaration).data
            else {
                unreachable!()
            };
            let owner = state.body_owner_token(
                info.file_id,
                endpoint.name.as_ref(),
                None,
                BodyOwnerKind::FreeFunction,
            );
            let previous_body =
                sema.body_dependency_observer
                    .replace(AnalyzedBodyOwnerEvent::FreeFunction {
                        token: owner,
                        file: endpoint.file,
                        name: endpoint.name.to_string(),
                    });
            let previous_type = sema.declaration_type_observer.replace((
                info.file_id,
                endpoint.name.to_string(),
                None,
                super::DeclarationTypeDependencySourceKind::Function,
                super::DeclarationTypeDependencyKind::Body,
            ));
            let result = sema.analyze_single_function(
                infer_ctx,
                sema.interner.resolve(&name),
                *return_type,
                sema.rir.params(params).values(),
                *body,
                info.span,
                info.allow_unused_variable,
                info.allow_unreachable_code,
            );
            sema.body_dependency_observer = previous_body;
            sema.declaration_type_observer = previous_type;
            match result {
                Ok((mut function, warnings, strings, functions, methods)) => {
                    function.ordinary_owner = Some(owner);
                    finalize_ordinary(
                        sema,
                        owner,
                        sema.rir.get(*body).span,
                        function,
                        warnings,
                        strings,
                        functions,
                        methods,
                        interruption,
                    )
                }
                Err(error) => fail(sema, Some(owner), error),
            }
        }
        StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction => {
            let Some(endpoint) = sema.endpoint_facts().definition_endpoint(token) else {
                return fail(
                    sema,
                    None,
                    CompileError::without_span(ErrorKind::InvalidCompilerInput(
                        "one-body request has no current stable endpoint".into(),
                    )),
                );
            };
            analyze_named_method(sema, infer_ctx, &endpoint, interruption)
        }
        StableDefinitionKind::Destructor => {
            let Some(endpoint) = sema.endpoint_facts().definition_endpoint(token) else {
                return fail(
                    sema,
                    None,
                    CompileError::without_span(ErrorKind::InvalidCompilerInput(
                        "one-body request has no current stable endpoint".into(),
                    )),
                );
            };
            analyze_named_destructor(sema, infer_ctx, &endpoint, interruption)
        }
        _ => fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "one-body request does not identify a callable body".into(),
            )),
        ),
    }
}

struct OrdinaryDefinitionSelection {
    endpoint: SemanticDefinitionEndpoint,
    name: Spur,
    info: crate::FunctionInfo,
    declaration: rue_rir::InstRef,
}

fn select_ordinary_definition<P: BodyEndpointProvider>(
    facts: &P,
    endpoint: SemanticDefinitionEndpoint,
) -> Result<OrdinaryDefinitionSelection, CompileError> {
    let Some(source_name) = facts.name_symbol(endpoint.name.as_ref()) else {
        return Err(CompileError::without_span(ErrorKind::UndefinedFunction(
            endpoint.name.to_string(),
        )));
    };
    let Some(name) = facts.function_by_file_name(FileId::new(endpoint.file), source_name) else {
        return Err(CompileError::without_span(ErrorKind::UndefinedFunction(
            endpoint.name.to_string(),
        )));
    };
    let info = facts
        .function_info(name)
        .expect("functions_by_file_name resolved to a registered function");
    let source = facts.source_function_name(name);
    let Some(declaration) = facts.first_free_function(source, info.file_id) else {
        return Err(CompileError::without_span(ErrorKind::UndefinedFunction(
            endpoint.name.to_string(),
        )));
    };
    Ok(OrdinaryDefinitionSelection {
        endpoint,
        name,
        info,
        declaration,
    })
}

fn analyze_named_method(
    sema: &mut BodySema<'_>,
    infer_ctx: &super::InferenceContext,
    endpoint: &crate::SemanticDefinitionEndpoint,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    let Some(owner_name) = endpoint.owner.as_deref() else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "method endpoint has no owner".into(),
            )),
        );
    };
    let Some(owner_symbol) = sema.interner.get(owner_name) else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "method owner is absent".into(),
            )),
        );
    };
    let Some(struct_id) = sema
        .endpoint_facts()
        .struct_by_file_name(FileId::new(endpoint.file), owner_symbol)
    else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "method owner is absent".into(),
            )),
        );
    };
    let Some(method_name) = sema.interner.get(endpoint.name.as_ref()) else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(endpoint.name.to_string())),
        );
    };
    let Some(info) = sema.endpoint_facts().method_info(struct_id, method_name) else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(endpoint.name.to_string())),
        );
    };
    let Some(declaration) = sema.endpoint_facts().named_method_declaration(
        FileId::new(endpoint.file),
        owner_symbol,
        method_name,
    ) else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(endpoint.name.to_string())),
        );
    };
    let InstData::FnDecl {
        params,
        return_type,
        body,
        has_self,
        self_mode,
        self_is_mut,
        ..
    } = &sema.rir.get(declaration).data
    else {
        unreachable!()
    };
    let kind = if *has_self {
        BodyOwnerKind::Method
    } else {
        BodyOwnerKind::AssociatedFunction
    };
    let owner = sema.body_owner_token(
        info.span.file_id,
        endpoint.name.as_ref(),
        Some(owner_name),
        kind,
    );
    let generic = sema
        .param_arena
        .comptime(info.params)
        .iter()
        .any(|value| *value);
    let owner_event = AnalyzedBodyOwnerEvent::NamedMethod {
        token: owner,
        file: endpoint.file,
        owner_name: owner_name.to_owned(),
        method_name: endpoint.name.to_string(),
        generic,
    };
    let previous_body = sema.body_dependency_observer.replace(owner_event);
    let previous_type = sema.declaration_type_observer.replace((
        info.span.file_id,
        endpoint.name.to_string(),
        Some(owner_name.to_owned()),
        if *has_self {
            super::DeclarationTypeDependencySourceKind::Method
        } else {
            super::DeclarationTypeDependencySourceKind::AssociatedFunction
        },
        super::DeclarationTypeDependencyKind::Body,
    ));
    let full_name = sema.method_symbol(struct_id, endpoint.name.as_ref(), *has_self);
    let result = sema.analyze_method_function(
        infer_ctx,
        &full_name,
        *return_type,
        sema.rir.params(params).values(),
        *body,
        info.span,
        info.struct_type,
        *has_self,
        *self_mode,
        *self_is_mut,
    );
    sema.body_dependency_observer = previous_body;
    sema.declaration_type_observer = previous_type;
    match result {
        Ok((mut function, warnings, strings, functions, methods)) => {
            function.ordinary_owner = Some(owner);
            finalize_ordinary(
                sema,
                owner,
                sema.rir.get(*body).span,
                function,
                warnings,
                strings,
                functions,
                methods,
                interruption,
            )
        }
        Err(error) => fail(sema, Some(owner), error),
    }
}

fn analyze_named_destructor(
    sema: &mut BodySema<'_>,
    infer_ctx: &super::InferenceContext,
    endpoint: &crate::SemanticDefinitionEndpoint,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    let Some(owner_name) = endpoint.owner.as_deref() else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "destructor endpoint has no owner".into(),
            )),
        );
    };
    let Some(owner_symbol) = sema.interner.get(owner_name) else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "destructor owner is absent".into(),
            )),
        );
    };
    let Some(struct_id) = sema
        .endpoint_facts()
        .struct_by_file_name(FileId::new(endpoint.file), owner_symbol)
    else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "destructor owner is absent".into(),
            )),
        );
    };
    let Some(record) = sema
        .endpoint_facts()
        .destructor(endpoint.file, owner_symbol)
    else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(format!(
                "{owner_name}.__drop"
            ))),
        );
    };
    let owner = sema.body_owner_token(
        record.span.file_id,
        owner_name,
        Some(owner_name),
        BodyOwnerKind::Destructor,
    );
    let previous_body =
        sema.body_dependency_observer
            .replace(AnalyzedBodyOwnerEvent::NamedDestructor {
                token: owner,
                file: endpoint.file,
                owner_name: owner_name.to_owned(),
            });
    let previous_type = sema.declaration_type_observer.replace((
        record.span.file_id,
        owner_name.to_owned(),
        Some(owner_name.to_owned()),
        super::DeclarationTypeDependencySourceKind::Destructor,
        super::DeclarationTypeDependencyKind::Body,
    ));
    let result = sema.analyze_destructor_function(
        infer_ctx,
        &sema.destructor_symbol(struct_id),
        record.body,
        record.span,
        Type::new_struct(struct_id),
    );
    sema.body_dependency_observer = previous_body;
    sema.declaration_type_observer = previous_type;
    match result {
        Ok((mut function, warnings, strings, functions, methods)) => {
            function.ordinary_owner = Some(owner);
            finalize_ordinary(
                sema,
                owner,
                sema.rir.get(record.body).span,
                function,
                warnings,
                strings,
                functions,
                methods,
                interruption,
            )
        }
        Err(error) => fail(sema, Some(owner), error),
    }
}
