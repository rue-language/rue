use super::body::BodyInputResolver;
use super::body::*;
use super::*;
pub(super) struct SemanticNucleusTypeProvider<'a> {
    pub(super) context: &'a QueryContext,
    pub(super) family: &'a SemanticNucleusFamily,
    pub(super) shells: &'a QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    pub(super) names: &'a QueryFamily<LookupNameKey, LookupNameValue>,
    pub(super) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    pub(super) substitutions: BTreeMap<Arc<str>, crate::durable_semantics::DurableType>,
    pub(super) value_substitutions: BTreeMap<Arc<str>, crate::durable_semantics::DurableConstValue>,
    pub(super) deferred_value_parameters: BTreeMap<Arc<str>, crate::durable_semantics::DurableType>,
    pub(super) anonymous_nominals:
        BTreeMap<crate::AnonymousNominalKey, crate::durable_semantics::DurableAnonymousNominal>,
    pub(super) dependency_source: crate::StableDefinitionKey,
    pub(super) dependency_kind: rue_air::DeclarationTypeDependencyKind,
    pub(super) dependencies: BTreeSet<crate::semantic_query_nucleus::SemanticDeclarationDependency>,
    pub(super) deferred_ownership: BTreeSet<crate::semantic_query_nucleus::DeferredOwnershipGate>,
    /// Recursive ownership answers already proven for a nominal type, for the
    /// life of this provider. See [`OwnershipProperties`].
    pub(super) ownership_properties: BTreeMap<crate::StableDefinitionKey, OwnershipProperties>,
}

/// One nominal type's recursive ownership answers, memoized per provider.
///
/// `type_carries_linear` and `type_is_copy` walk the durable type graph, and a
/// body that mentions the same
/// aggregate repeatedly re-walked it and re-resolved its signature every
/// time. The two stay separate fields rather than one computed bundle because
/// they are asked for independently and each costs its own traversal;
/// filling one must not force the other.
///
/// Only answers that did not depend on cycle-breaking are stored. Each walker
/// breaks a recursive type by answering provisionally — `DoesNotCarry` for
/// linear containment and `true` for Copy — for a key already on its own stack, so a
/// result reached through such an answer is a property of that stack and not
/// of the type. [`OwnershipWalk::tainted`] carries that condition back up, and
/// a tainted result is returned without being stored. `Deferred` and every
/// error are likewise never stored: they say the signature was not resolvable
/// yet, which a later request may answer differently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct OwnershipProperties {
    pub(super) carries_linear: Option<LinearOwnershipFact>,
    pub(super) is_copy: Option<bool>,
}

/// The cycle-breaking stack of one ownership traversal, plus whether the
/// subtree currently being computed reached an answer through that stack.
pub(super) struct OwnershipWalk {
    pub(super) visiting: BTreeSet<StableDefinitionKey>,
    pub(super) tainted: bool,
}

impl OwnershipWalk {
    pub(super) fn new() -> Self {
        Self {
            visiting: BTreeSet::new(),
            tainted: false,
        }
    }

    /// Record that the answer being computed came from breaking a cycle or
    /// from a signature that is not resolvable yet.
    pub(super) fn taint(&mut self) {
        self.tainted = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinearOwnershipFact {
    DoesNotCarry,
    Carries,
    Deferred,
}

impl LinearOwnershipFact {
    pub(super) fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Carries, _) | (_, Self::Carries) => Self::Carries,
            (Self::Deferred, _) | (_, Self::Deferred) => Self::Deferred,
            _ => Self::DoesNotCarry,
        }
    }
}

pub(super) type EvaluateSemanticConstError = crate::durable_comptime::DurableComptimeFailure;

pub(super) fn durable_type_diagnostic_name(ty: &crate::durable_semantics::DurableType) -> String {
    crate::durable_comptime::durable_type_diagnostic_name(ty)
}

pub(super) fn deferred_gate_type_diagnostic_name(
    context: &QueryContext,
    family: &SemanticNucleusFamily,
    ty: &crate::durable_semantics::DurableType,
    configuration: &crate::semantic_query_nucleus::SemanticQueryConfiguration,
) -> Result<String, QueryAbort> {
    let crate::durable_semantics::DurableType::AnonymousNominal(identity) = ty else {
        return Ok(durable_type_diagnostic_name(ty));
    };
    let producer = match &identity.producer {
        crate::StableProducerId::Definition(definition) => definition,
        crate::StableProducerId::Function(function) => {
            let Some(definition) = function_definition_key(function) else {
                return Ok(durable_type_diagnostic_name(ty));
            };
            definition
        }
    };
    let Some(declaration) = declaration_candidate_for_stable_key(producer) else {
        return Ok(durable_type_diagnostic_name(ty));
    };
    let signature = context.query_registered(
        family,
        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: configuration.clone(),
            },
        ),
    )?;
    let rue_query::QueryOutcome::Success(
        crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature),
    ) = signature.outcome()
    else {
        return Ok(durable_type_diagnostic_name(ty));
    };
    let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
        parameters, ..
    } = &signature.signature
    else {
        return Ok(durable_type_diagnostic_name(ty));
    };
    Ok(crate::durable_comptime::durable_type_diagnostic_name_with_parameters(ty, parameters))
}

pub(super) fn foreign_signature_display(
    parameters: &[crate::durable_semantics::DurableSemanticParameter],
    result: &crate::durable_semantics::DurableType,
) -> String {
    use crate::durable_semantics::DurableParameterMode as Mode;

    let parameters = parameters
        .iter()
        .map(|parameter| {
            let ty = durable_type_diagnostic_name(&parameter.ty);
            let ty = match parameter.mode {
                Mode::Value => ty,
                Mode::Borrow => format!("borrow {ty}"),
                Mode::Inout => format!("inout {ty}"),
            };
            if parameter.is_comptime {
                format!("comptime {ty}")
            } else {
                ty
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if matches!(result, crate::durable_semantics::DurableType::Unit) {
        format!("fn({parameters})")
    } else {
        format!(
            "fn({parameters}) -> {}",
            durable_type_diagnostic_name(result)
        )
    }
}

pub(super) fn foreign_signatures_agree(
    left_parameters: &[crate::durable_semantics::DurableSemanticParameter],
    left_result: &crate::durable_semantics::DurableType,
    right_parameters: &[crate::durable_semantics::DurableSemanticParameter],
    right_result: &crate::durable_semantics::DurableType,
) -> bool {
    left_result == right_result
        && left_parameters.len() == right_parameters.len()
        && left_parameters
            .iter()
            .zip(right_parameters)
            .all(|(left, right)| {
                left.ty == right.ty
                    && left.mode == right.mode
                    && left.is_comptime == right.is_comptime
            })
}

pub(super) fn inferred_const_type_name(
    value: &crate::durable_semantics::DurableConstValue,
) -> &'static str {
    crate::durable_comptime::inferred_durable_const_type_name(value)
}

pub(super) fn suggested_const_type_name(
    value: &crate::durable_semantics::DurableConstValue,
) -> &'static str {
    crate::durable_comptime::suggested_durable_const_type_name(value)
}

/// The bit pattern a `const` initializer of float type denotes, or `None` when
/// the initializer is not a value of that type at all.
///
/// `literal` distinguishes the two forms a float const initializer takes.
/// A source float literal still carries its exact written decimal, and spec
/// 3.12:10 rejects one that rounds to an infinity in the const's type — the
/// same rule the body path applies to `let x: f32 = 1e39;`. A value the
/// compile-time engine computed already carries the width it was evaluated at
/// and may legitimately be `inf` or `NaN` (`1.0 / 0.0`), so it is read with
/// the permissive value parser.
pub(super) fn float_const_initializer_bits(
    text: &str,
    ty: rue_air::Type,
    literal: bool,
) -> Option<u64> {
    if literal {
        rue_air::finite_float_literal_bits(text, ty)
    } else {
        rue_air::float_value_bits(text, ty)
    }
}

pub(super) fn substitute_durable_generics(
    ty: &crate::durable_semantics::DurableType,
    type_arguments: &[crate::durable_semantics::DurableType],
) -> crate::durable_semantics::DurableType {
    crate::durable_comptime::substitute_durable_generics(ty, type_arguments)
}

/// Build the E0481 for an array length that is already *fully concrete* and
/// still cannot be a length.
///
/// This must be a domain diagnostic rather than an opaque `Resolution` failure.
/// A `Resolution` failure means "this provider could not resolve the syntax
/// yet", which the comptime-call reduction is entitled to treat as
/// non-evaluable; the caller then reports whatever it makes of an unreduced
/// call. So `fn Buf(comptime n: i64) -> type { struct { data: [i64; n] } }`
/// applied as `Buf(-1)` lost its real "array length is negative" error and
/// surfaced as an unrelated E1200 about storing a type value at runtime
/// (RUE-1734). Every caller below has a concrete integer in hand — a still
/// unsubstituted `comptime n` returns `Ok(None)` well before this point — so the
/// error is a genuine well-formedness failure and must be reported as one.
///
/// The wording mirrors `rue_air::sema::typeck`'s local provider so the same
/// program reports the same text whichever provider resolved the length.
/// The unnamed counterpart of the durable named-length failure, for a length
/// that arrived as a literal or an already-evaluated comptime value.
pub(super) fn durable_literal_array_length_failure(
    value: i128,
) -> crate::semantic_query_nucleus::SemanticNucleusFailure {
    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
        rue_error::ErrorKind::InvalidArrayLength {
            reason: format!("array length must be non-negative, got {value}"),
        },
    )
}

pub(super) fn durable_provider_named_array_length_failure(
    name: &str,
    error: crate::durable_comptime::DurableComptimeArrayLengthError,
) -> crate::semantic_query_nucleus::SemanticNucleusFailure {
    use crate::durable_comptime::DurableComptimeArrayLengthError as E;
    match error {
        E::Negative(value) => crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength {
                reason: format!("array length '{name}' is negative ({value})"),
            },
        ),
        E::TooLarge(value) => crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength {
                reason: format!("array length '{name}' ({value}) is too large"),
            },
        ),
        E::NonInteger | E::Module | E::TargetEnum => {
            crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                format!("array length `{name}` is not an integer").into(),
            )
        }
    }
}

pub(super) fn semantic_nucleus_declaration_name(identity: &str) -> Option<Arc<str>> {
    let candidate = [
        "identity:",
        "signature:",
        "nominal-well-formed:",
        "const:",
        "comptime:",
        "anonymous:",
    ]
    .iter()
    .find_map(|prefix| identity.strip_prefix(prefix))?;
    let (module_len, rest) = candidate.split_once(':')?;
    let module_len = module_len.parse::<usize>().ok()?;
    let rest = rest.get(module_len..)?.strip_prefix(':')?;
    let (_, rest) = rest.split_once(':')?;
    let (name_len, rest) = rest.split_once(':')?;
    let name_len = name_len.parse::<usize>().ok()?;
    Some(Arc::from(rest.get(..name_len)?))
}

pub(super) fn semantic_nucleus_cycle_names(nodes: &[rue_query::NodeIdentity]) -> Arc<[Arc<str>]> {
    let mut names = nodes
        .iter()
        .filter(|node| node.family() == "compiler.semantic-nucleus")
        .filter_map(|node| semantic_nucleus_declaration_name(node.key()))
        .collect::<Vec<_>>();
    if let Some(first) = names.first().cloned()
        && (names.len() == 1 || names.last() != Some(&first))
    {
        names.push(first);
    }
    names.into()
}

pub(crate) fn function_definition_key(
    function: &crate::FunctionInstanceKey,
) -> Option<&StableDefinitionKey> {
    crate::semantic_identity::function_base_definition(function)
}

pub(super) fn producer_body_source_definition_key(
    producer: &crate::StableProducerId,
) -> Option<&StableDefinitionKey> {
    match producer {
        crate::StableProducerId::Definition(key) => Some(key),
        crate::StableProducerId::Function(function) => {
            function_body_source_definition_key(function)
        }
    }
}

pub(super) fn function_body_source_definition_key(
    function: &crate::FunctionInstanceKey,
) -> Option<&StableDefinitionKey> {
    match crate::semantic_identity::function_specialization_base(function) {
        crate::FunctionInstanceKey::Definition(key) => Some(key),
        crate::FunctionInstanceKey::AnonymousMember { owner, .. } => {
            let crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(owner)) =
                owner.as_ref()
            else {
                return None;
            };
            producer_body_source_definition_key(&owner.producer)
        }
        crate::FunctionInstanceKey::Specialization { .. }
        | crate::FunctionInstanceKey::DropGlue(_)
        | crate::FunctionInstanceKey::ErrorPrinter(_)
        | crate::FunctionInstanceKey::TestDispatcher => None,
    }
}

pub(super) fn body_source_definition_key(
    function: &crate::FunctionInstanceKey,
) -> Option<&StableDefinitionKey> {
    if let crate::FunctionInstanceKey::AnonymousMember { owner, .. } = function {
        let crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(owner)) =
            owner.as_ref()
        else {
            return None;
        };
        return producer_body_source_definition_key(&owner.producer);
    }
    function_body_source_definition_key(function)
}

pub(super) fn closure_callable_has_body(
    context: &rue_query::QueryContext,
    body_input: &BodyInputResolver,
    declarations: &[crate::DurableDeclarationSemantic],
    declaration_index: &crate::local_semantic_materialization::SharedDeclarationFactIndex,
    callable: &crate::FunctionInstanceKey,
    configuration: &crate::semantic_query_nucleus::SemanticQueryConfiguration,
) -> Result<Result<bool, Arc<str>>, QueryAbort> {
    if matches!(
        callable,
        crate::FunctionInstanceKey::DropGlue(_) | crate::FunctionInstanceKey::ErrorPrinter(_)
    ) {
        return Ok(Ok(false));
    }
    if matches!(callable, crate::FunctionInstanceKey::AnonymousMember { .. }) {
        return Ok(Ok(true));
    }
    let key = crate::body_query::BodyQueryKey::new(callable.clone(), configuration.clone());
    let value = body_input.resolve(context, &key)?;
    let crate::body_query::BodyInputValue::Available(input) = value else {
        return Ok(match &value {
            crate::body_query::BodyInputValue::Incomplete(
                crate::body_query::BodyInputIncomplete::MissingPrerequisite(detail),
            ) => Err(detail.clone()),
            crate::body_query::BodyInputValue::Incomplete(
                crate::body_query::BodyInputIncomplete::BodyPlanFailure(_),
            ) => Ok(true),
            crate::body_query::BodyInputValue::Incomplete(_) => Ok(false),
            crate::body_query::BodyInputValue::Available(_) => unreachable!(),
        });
    };
    if !matches!(callable, crate::FunctionInstanceKey::Definition(_)) {
        return Ok(Ok(true));
    }
    let Some(declaration) = declaration_index.declaration(declarations, &input.owner) else {
        return Ok(Err(Arc::from(format!(
            "body input owner {:?} has no declaration-semantic projection",
            input.owner
        ))));
    };
    use crate::durable_semantics::DurableDeclarationPayload as Payload;
    Ok(Ok(match &declaration.payload {
        Payload::Callable {
            parameters, result, ..
        } => {
            !matches!(result, crate::durable_semantics::DurableType::ComptimeType)
                // Only comptime *type* parameters require a specialized
                // callable identity. Comptime value parameters are retained
                // runtime inputs, so the definition body is an ordinary
                // closure node and must remain reachable.
                && parameters.iter().all(durable_parameter_is_runtime)
        }
        Payload::Destructor => true,
        Payload::Struct { .. }
        | Payload::Enum { .. }
        | Payload::Const { .. }
        | Payload::ModuleBinding { .. } => false,
    }))
}

pub(super) fn publish_body_plan_materialization_attribution(
    attribution: crate::canonical_lower::BodyPlanMaterializationAttribution,
) {
    let attributed_total_ns = attribution
        .span_remap_validation_ns
        .saturating_add(attribution.index.duration_ns);
    let declaration = attribution.index.declaration_index;
    tracing::event!(
        name: "semantic_body_lowering_breakdown",
        target: "rue::timing",
        tracing::Level::INFO,
        attributed_total_ns,
        assembly_snapshot_ns = 0_u64,
        lex_parse_ns = 0_u64,
        rir_lower_ns = 0_u64,
        span_remap_validation_ns = attribution.span_remap_validation_ns,
        body_rir_index_ns = attribution.index.duration_ns,
        plan_materializations = 1_u64,
        base_symbol_rebuild_ns = attribution.base_symbol_rebuild_ns,
        base_symbols_rebuilt = attribution.base_symbols_rebuilt,
        rir_instructions = attribution.rir_instructions,
        rir_payload_words = attribution.rir_payload_words,
        index_builds = declaration.build_invocations as u64,
        index_rir_instructions_visited = declaration.rir_instructions_visited as u64,
        index_method_references_visited = declaration.method_references_visited as u64,
        index_shell_declarations_visited = attribution.index.shell_declarations_visited,
        index_named_methods_indexed = attribution.index.named_methods_indexed,
        index_const_declarations_indexed = attribution.index.const_declarations_indexed,
    );
}

/// Reconstruct the canonical syntax candidate represented by a stable key.
///
/// Stable definition keys identify the canonical declaration spelling and
/// owner, but intentionally do not encode a duplicate-discriminator.  This
/// helper therefore reconstructs discriminator zero; callers must use the
/// projected identity check rather than treating the result as proof that a
/// duplicate candidate was preserved.
pub(crate) fn declaration_candidate_for_stable_key(
    key: &StableDefinitionKey,
) -> Option<crate::declaration_candidate::DeclarationCandidateKey> {
    stable_syntax_candidate_set(key)?[0].clone()
}

pub(super) fn stable_syntax_candidate_set(
    key: &StableDefinitionKey,
) -> Option<[Option<crate::declaration_candidate::DeclarationCandidateKey>; 2]> {
    use crate::StableDefinitionKind as K;
    use crate::declaration_candidate::{
        DeclarationCandidateCategory as C, DeclarationCandidateOwner,
    };

    let categories = match key.kind() {
        K::Function => [Some(C::Function), Some(C::ExternFunction)],
        K::Struct => [Some(C::Struct), None],
        K::Enum => [Some(C::Enum), None],
        K::ValueConst | K::ModuleBinding => [Some(C::ConstCandidate), None],
        K::Method => [Some(C::Method), None],
        K::AssociatedFunction => [Some(C::AssociatedFunction), None],
        K::Destructor => [Some(C::Destructor), None],
        K::Test => [Some(C::Test), None],
    };
    let owner = match key.owner() {
        Some(owner) => Some(DeclarationCandidateOwner {
            category: match owner.kind() {
                K::Struct => C::Struct,
                K::Enum => C::Enum,
                _ => return None,
            },
            name: Arc::clone(owner.shared_name()),
        }),
        None => None,
    };
    if key.kind().requires_owner() && owner.is_none() {
        return None;
    }
    Some(categories.map(|category| {
        category.map(
            |category| crate::declaration_candidate::DeclarationCandidateKey {
                module: key.module().clone(),
                category,
                name: Arc::clone(key.shared_name()),
                owner: owner.clone(),
                duplicate_discriminator: 0,
            },
        )
    }))
}

pub(super) fn anonymous_nominal_query_key(
    identity: &crate::AnonymousNominalKey,
    configuration: &crate::semantic_query_nucleus::SemanticQueryConfiguration,
) -> Option<crate::semantic_query_nucleus::AnonymousNominalQueryKey> {
    let producer = match &identity.producer {
        crate::StableProducerId::Definition(key) => key,
        crate::StableProducerId::Function(function) => function_definition_key(function)?,
    };
    Some(crate::semantic_query_nucleus::AnonymousNominalQueryKey {
        producer: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate_for_stable_key(producer)?,
            configuration: configuration.clone(),
        },
        identity: identity.clone(),
    })
}

pub(super) fn query_anonymous_nominal(
    context: &rue_query::QueryContext,
    semantic_nucleus: &SemanticNucleusFamily,
    body_produced_anonymous: &QueryFamily<
        crate::body_query::BodyQueryKey,
        crate::body_query::ProducedAnonymous,
    >,
    identity: &crate::AnonymousNominalKey,
    configuration: &crate::semantic_query_nucleus::SemanticQueryConfiguration,
) -> Result<
    Result<
        crate::durable_semantics::DurableAnonymousNominal,
        crate::type_queries::TypeQueryFailure,
    >,
    QueryAbort,
> {
    let unavailable = |detail| {
        Err(crate::type_queries::TypeQueryFailure::Unavailable(
            Arc::from(detail),
        ))
    };
    if let Some(query) = anonymous_nominal_query_key(identity, configuration) {
        let terminal = context.query_registered(
            semantic_nucleus,
            crate::semantic_query_nucleus::SemanticNucleusKey::AnonymousNominal(query),
        )?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("SemanticNucleus publishes typed values")
        };
        return Ok(match value {
            crate::semantic_query_nucleus::SemanticNucleusValue::AnonymousNominal(nominal) => {
                Ok(nominal.clone())
            }
            _ => unavailable("anonymous type facts are unavailable"),
        });
    }
    let crate::StableProducerId::Function(producer) = &identity.producer else {
        return Ok(unavailable("anonymous type has no stable producer"));
    };
    let produced = context.query_registered(
        body_produced_anonymous,
        crate::body_query::BodyQueryKey::new(producer.as_ref().clone(), configuration.clone()),
    )?;
    let rue_query::QueryOutcome::Success(produced) = produced.outcome() else {
        unreachable!("BodyProducedAnonymous publishes typed values")
    };
    let crate::body_query::ProducedAnonymous::Produced(produced) = produced else {
        return Ok(unavailable("anonymous type producer failed"));
    };
    Ok(produced
        .0
        .iter()
        .find(|nominal| nominal.identity == *identity)
        .cloned()
        .ok_or_else(|| {
            crate::type_queries::TypeQueryFailure::Unavailable(Arc::from(
                "anonymous type is absent from its producer",
            ))
        }))
}

pub(super) fn type_shape_from_terminal(
    terminal: &rue_query::QueryTerminal<crate::type_queries::TypeShapeValue>,
) -> Result<&crate::type_queries::TypeShape, crate::type_queries::TypeQueryFailure> {
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!("TypeShape publishes typed values")
    };
    match value {
        crate::type_queries::TypeShapeValue::Available(shape) => Ok(shape),
        crate::type_queries::TypeShapeValue::Failure(failure) => Err(failure.clone()),
    }
}

pub(super) fn evaluate_type_shape(
    context: &rue_query::QueryContext,
    semantic_nucleus: &SemanticNucleusFamily,
    body_produced_anonymous: &QueryFamily<
        crate::body_query::BodyQueryKey,
        crate::body_query::ProducedAnonymous,
    >,
    key: &crate::type_queries::TypeQueryKey,
) -> Result<QueryOutput<crate::type_queries::TypeShapeValue>, QueryAbort> {
    use crate::type_queries::{TypeQueryFailure, TypeShape, TypeShapeValue};
    use crate::{NominalInstanceKey as N, TypeInstanceKey as T};
    let value = match &key.ty {
        T::I8
        | T::I16
        | T::I32
        | T::I64
        | T::U8
        | T::U16
        | T::U32
        | T::U64
        | T::Bool
        | T::Unit
        | T::Never => TypeShapeValue::Available(TypeShape::Scalar),
        T::F32 | T::F64 => TypeShapeValue::Available(TypeShape::Scalar),
        T::PtrConst(_) | T::PtrMut(_) => TypeShapeValue::Available(TypeShape::Pointer),
        T::Slice { .. } => TypeShapeValue::Available(TypeShape::Slice),
        T::Array { element, len } => TypeShapeValue::Available(TypeShape::Array {
            element: element.as_ref().clone(),
            len: *len,
        }),
        T::ComptimeType | T::ComptimeFloat | T::Module(_) | T::GenericParameter(_) => {
            TypeShapeValue::Available(TypeShape::Opaque)
        }
        T::BuiltinNominal { kind, name } | T::Nominal(N::Builtin { kind, name })
            if *kind == rue_air::AnonymousNominalKind::Struct
                && rue_air::is_string_view_struct_name(name) =>
        {
            TypeShapeValue::Available(TypeShape::Struct {
                fields: Arc::from([
                    (Arc::from("ptr"), T::PtrConst(Node::new(T::U8))),
                    (Arc::from("len"), T::U64),
                ]),
            })
        }
        T::BuiltinNominal { kind, name } | T::Nominal(N::Builtin { kind, name })
            if *kind == rue_air::AnonymousNominalKind::Enum =>
        {
            TypeShapeValue::Available(match rue_builtins::get_builtin_enum(name) {
                Some(definition) => TypeShape::Enum {
                    variants: definition
                        .variants
                        .iter()
                        .map(|variant| (Arc::from(*variant), Arc::from([])))
                        .collect::<Vec<_>>()
                        .into(),
                },
                None => TypeShape::Opaque,
            })
        }
        T::BuiltinNominal { .. } | T::Nominal(N::Builtin { .. }) => {
            TypeShapeValue::Available(TypeShape::Opaque)
        }
        T::Nominal(N::Named(definition)) => {
            let Some(candidate) = declaration_candidate_for_stable_key(definition) else {
                return Ok(QueryOutput::success(TypeShapeValue::Failure(
                    TypeQueryFailure::Unavailable(Arc::from(
                        "named type has no declaration candidate",
                    )),
                ))
                .with_terminal_kind(QueryTerminalKind::Failure));
            };
            let signature = context.query_registered(
                semantic_nucleus,
                crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                    crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                        declaration: candidate,
                        configuration: key.configuration.clone(),
                    },
                ),
            )?;
            let rue_query::QueryOutcome::Success(signature) = signature.outcome() else {
                unreachable!("SemanticNucleus publishes typed values")
            };
            use crate::semantic_query_nucleus::DeclarationSignatureProjection as S;
            match signature {
                crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature) => {
                    match &signature.signature {
                        S::Struct { fields, .. } => TypeShapeValue::Available(TypeShape::Struct {
                            fields: fields
                                .iter()
                                .map(|(name, ty)| {
                                    (name.clone(), crate::type_queries::type_instance(ty))
                                })
                                .collect::<Vec<_>>()
                                .into(),
                        }),
                        S::Enum { variants, .. } => TypeShapeValue::Available(TypeShape::Enum {
                            variants: variants
                                .iter()
                                .map(|(name, fields)| {
                                    (
                                        name.clone(),
                                        fields
                                            .iter()
                                            .map(crate::type_queries::type_instance)
                                            .collect::<Vec<_>>()
                                            .into(),
                                    )
                                })
                                .collect::<Vec<_>>()
                                .into(),
                        }),
                        _ => TypeShapeValue::Failure(TypeQueryFailure::Invalid(Arc::from(
                            "named type resolved to a non-type signature",
                        ))),
                    }
                }
                _ => TypeShapeValue::Failure(TypeQueryFailure::Unavailable(Arc::from(
                    "type signature is unavailable",
                ))),
            }
        }
        T::Nominal(N::Anonymous(identity)) => {
            let nominal = match query_anonymous_nominal(
                context,
                semantic_nucleus,
                body_produced_anonymous,
                identity,
                &key.configuration,
            )? {
                Ok(nominal) => nominal,
                Err(failure) => {
                    return Ok(QueryOutput::success(TypeShapeValue::Failure(failure))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                }
            };
            use crate::durable_semantics::DurableAnonymousNominalShape as S;
            TypeShapeValue::Available(match &nominal.shape {
                S::Struct { fields, .. } => TypeShape::Struct {
                    fields: fields
                        .iter()
                        .map(|(name, ty)| (name.clone(), crate::type_queries::type_instance(ty)))
                        .collect::<Vec<_>>()
                        .into(),
                },
                S::Enum { variants, .. } => TypeShape::Enum {
                    variants: variants
                        .iter()
                        .map(|(name, fields)| {
                            (
                                name.clone(),
                                fields
                                    .iter()
                                    .map(crate::type_queries::type_instance)
                                    .collect::<Vec<_>>()
                                    .into(),
                            )
                        })
                        .collect::<Vec<_>>()
                        .into(),
                },
            })
        }
    };
    let kind = if matches!(value, TypeShapeValue::Failure(_)) {
        QueryTerminalKind::Failure
    } else {
        QueryTerminalKind::Success
    };
    Ok(QueryOutput::success(value).with_terminal_kind(kind))
}

pub(super) fn type_query_failure(
    detail: impl Into<Arc<str>>,
) -> crate::type_queries::TypeFactsValue {
    crate::type_queries::TypeFactsValue::Failure(
        crate::type_queries::TypeQueryFailure::Unavailable(detail.into()),
    )
}

pub(super) fn type_facts_from_terminal(
    terminal: &rue_query::QueryTerminal<crate::type_queries::TypeFactsValue>,
) -> Result<&crate::type_queries::TypeFacts, crate::type_queries::TypeQueryFailure> {
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!("TypeFacts publishes typed values")
    };
    match value {
        crate::type_queries::TypeFactsValue::Available(facts) => Ok(facts.as_ref()),
        crate::type_queries::TypeFactsValue::Failure(failure) => Err(failure.clone()),
    }
}

pub(super) fn evaluate_type_facts(
    context: &rue_query::QueryContext,
    family: &QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::TypeFactsValue>,
    type_shapes: &QueryFamily<
        crate::type_queries::TypeQueryKey,
        crate::type_queries::TypeShapeValue,
    >,
    semantic_nucleus: &SemanticNucleusFamily,
    lookup_names: &QueryFamily<LookupNameKey, LookupNameValue>,
    body_produced_anonymous: &QueryFamily<
        crate::body_query::BodyQueryKey,
        crate::body_query::ProducedAnonymous,
    >,
    key: &crate::type_queries::TypeQueryKey,
) -> Result<QueryOutput<crate::type_queries::TypeFactsValue>, QueryAbort> {
    use crate::type_queries::{TypeFacts, TypeFactsValue, TypeShape};
    use crate::{NominalInstanceKey as N, TypeInstanceKey as T};

    // COPY_POLICY_DURABLE_PROJECTION_OWNER: type-facts
    let shape_terminal = context.query_registered(type_shapes, key.clone())?;
    let canonical_shape = match type_shape_from_terminal(&shape_terminal) {
        Ok(shape) => shape.clone(),
        Err(failure) => {
            return Ok(QueryOutput::success(TypeFactsValue::Failure(failure))
                .with_terminal_kind(QueryTerminalKind::Failure));
        }
    };
    let direct = |is_copy| {
        TypeFactsValue::Available(Box::new(TypeFacts {
            is_copy,
            carries_linear: false,
            needs_drop: false,
            destructor: None,
            shape: canonical_shape.clone(),
        }))
    };
    let mut value = match &key.ty {
        T::I8
        | T::I16
        | T::I32
        | T::I64
        | T::U8
        | T::U16
        | T::U32
        | T::U64
        | T::Bool
        | T::Unit
        | T::Never => direct(true),
        T::F32 | T::F64 | T::ComptimeFloat => direct(true),
        T::PtrConst(_) | T::PtrMut(_) => direct(true),
        T::Slice { .. } => direct(true),
        T::ComptimeType | T::Module(_) | T::GenericParameter(_) => direct(true),
        T::BuiltinNominal { kind, name } | T::Nominal(N::Builtin { kind, name })
            if *kind == rue_air::AnonymousNominalKind::Struct
                && rue_air::is_string_view_struct_name(name) =>
        {
            direct(true)
        }
        T::BuiltinNominal { kind, name } | T::Nominal(N::Builtin { kind, name })
            if *kind == rue_air::AnonymousNominalKind::Enum =>
        {
            direct(rue_builtins::get_builtin_enum(name).is_some())
        }
        T::BuiltinNominal { .. } | T::Nominal(N::Builtin { .. }) => direct(false),
        T::Array { element, len } => {
            let child = context.query_registered(
                family,
                crate::type_queries::TypeQueryKey {
                    ty: element.as_ref().clone(),
                    configuration: key.configuration.clone(),
                },
            )?;
            match type_facts_from_terminal(&child) {
                Ok(child) => TypeFactsValue::Available(Box::new(TypeFacts {
                    is_copy: child.is_copy,
                    carries_linear: child.carries_linear,
                    needs_drop: rue_air::drop_glue::requires_drop_glue(
                        rue_air::drop_glue::DropGlueShape::Array { len: *len },
                        [child.needs_drop],
                    ),
                    destructor: None,
                    shape: canonical_shape.clone(),
                })),
                Err(failure) => TypeFactsValue::Failure(failure),
            }
        }
        T::Nominal(N::Named(definition)) => {
            let Some(candidate) = declaration_candidate_for_stable_key(definition) else {
                return Ok(QueryOutput::success(type_query_failure(
                    "named type has no declaration candidate",
                ))
                .with_terminal_kind(QueryTerminalKind::Failure));
            };
            let signature = context.query_registered(
                semantic_nucleus,
                crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                    crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                        declaration: candidate,
                        configuration: key.configuration.clone(),
                    },
                ),
            )?;
            let rue_query::QueryOutcome::Success(signature) = signature.outcome() else {
                unreachable!("SemanticNucleus publishes typed values")
            };
            let (mut is_copy, copy_from_children, is_linear) = match signature {
                crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature) => {
                    use crate::semantic_query_nucleus::DeclarationSignatureProjection as S;
                    match &signature.signature {
                        S::Struct {
                            is_copy, is_linear, ..
                        } => (*is_copy, false, *is_linear),
                        S::Enum { .. } => (true, true, false),
                        _ => {
                            return Ok(QueryOutput::success(type_query_failure(
                                "named type resolved to a non-type signature",
                            ))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                    }
                }
                crate::semantic_query_nucleus::SemanticNucleusValue::Failure(failure) => {
                    return Ok(QueryOutput::success(type_query_failure(format!(
                        "type signature failed: {failure:?}"
                    )))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                }
                _ => {
                    return Ok(QueryOutput::success(type_query_failure(
                        "type signature returned the wrong projection",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                }
            };
            let shape = canonical_shape.clone();
            let children = match &shape {
                TypeShape::Struct { fields } => {
                    fields.iter().map(|(_, ty)| ty.clone()).collect::<Vec<_>>()
                }
                TypeShape::Enum { variants } => variants
                    .iter()
                    .flat_map(|(_, fields)| fields.iter().cloned())
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            let child_keys = children
                .iter()
                .cloned()
                .map(|ty| crate::type_queries::TypeQueryKey {
                    ty,
                    configuration: key.configuration.clone(),
                })
                .collect::<Vec<_>>();
            let child_terminals = context.query_registered_adaptive_batch(family, child_keys)?;
            let mut carries_linear = is_linear;
            let mut child_needs_drop = Vec::with_capacity(child_terminals.len());
            for child in &child_terminals {
                match type_facts_from_terminal(child) {
                    Ok(child) => {
                        if copy_from_children {
                            is_copy &= child.is_copy;
                        }
                        carries_linear |= child.carries_linear;
                        child_needs_drop.push(child.needs_drop);
                    }
                    Err(failure) => {
                        return Ok(QueryOutput::success(TypeFactsValue::Failure(failure))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                }
            }
            let destructor_lookup = context.query_registered(
                lookup_names,
                LookupNameKey {
                    module: definition.module().clone(),
                    namespace: DefinitionNamespace::Destructor,
                    name: Arc::from(definition.name()),
                },
            )?;
            let destructor = match destructor_lookup.outcome() {
                rue_query::QueryOutcome::Success(LookupNameValue(Ok(facts))) => {
                    facts.first().map(|_| {
                        crate::FunctionInstanceKey::Definition(
                            crate::StableDefinitionKey::from_stable_parts(
                                definition.module().clone(),
                                crate::StableDefinitionNamespace::Destructor,
                                crate::StableDefinitionKind::Destructor,
                                definition.name(),
                                Some((definition.kind(), Arc::<str>::from(definition.name()))),
                            ),
                        )
                    })
                }
                _ => None,
            };
            let needs_drop = rue_air::drop_glue::requires_drop_glue(
                rue_air::drop_glue::DropGlueShape::Aggregate {
                    has_destructor: destructor.is_some(),
                },
                child_needs_drop,
            );
            TypeFactsValue::Available(Box::new(TypeFacts {
                is_copy,
                carries_linear,
                needs_drop,
                destructor,
                shape,
            }))
        }
        T::Nominal(N::Anonymous(identity)) => {
            let nominal = match query_anonymous_nominal(
                context,
                semantic_nucleus,
                body_produced_anonymous,
                identity,
                &key.configuration,
            )? {
                Ok(nominal) => nominal,
                Err(failure) => {
                    return Ok(QueryOutput::success(TypeFactsValue::Failure(failure))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                }
            };
            use crate::durable_semantics::DurableAnonymousNominalShape as S;
            let destructor = match &nominal.shape {
                S::Struct { methods, .. } => methods
                    .iter()
                    .find(|method| {
                        rue_air::drop_glue::is_anonymous_destructor(
                            method.name.as_ref(),
                            method.has_self,
                        )
                    })
                    .map(|_| crate::FunctionInstanceKey::AnonymousMember {
                        owner: Node::new(key.ty.clone()),
                        member: crate::AnonymousMemberKey {
                            kind: crate::AnonymousMemberKind::Destructor,
                            name: Arc::from("__drop"),
                        },
                    }),
                S::Enum { .. } => None,
            };
            let shape = canonical_shape.clone();
            let children = match &shape {
                TypeShape::Struct { fields } => {
                    fields.iter().map(|(_, ty)| ty.clone()).collect::<Vec<_>>()
                }
                TypeShape::Enum { variants } => variants
                    .iter()
                    .flat_map(|(_, fields)| fields.iter().cloned())
                    .collect(),
                _ => Vec::new(),
            };
            let child_terminals = context.query_registered_adaptive_batch(
                family,
                children
                    .iter()
                    .cloned()
                    .map(|ty| crate::type_queries::TypeQueryKey {
                        ty,
                        configuration: key.configuration.clone(),
                    }),
            )?;
            // Durable types cannot delegate directly to the AIR pool because
            // they are evaluated before pool materialization. This is the
            // representation-boundary projection of TypeInternPool's policy:
            // anonymous composites are Copy iff they have no destructor and
            // every by-value child is Copy.
            let mut is_copy = destructor.is_none();
            let mut carries_linear = false;
            let mut child_needs_drop = Vec::with_capacity(child_terminals.len());
            for child in &child_terminals {
                match type_facts_from_terminal(child) {
                    Ok(child) => {
                        is_copy &= child.is_copy;
                        carries_linear |= child.carries_linear;
                        child_needs_drop.push(child.needs_drop);
                    }
                    Err(failure) => {
                        return Ok(QueryOutput::success(TypeFactsValue::Failure(failure))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                }
            }
            let needs_drop = rue_air::drop_glue::requires_drop_glue(
                rue_air::drop_glue::DropGlueShape::Aggregate {
                    has_destructor: destructor.is_some(),
                },
                child_needs_drop,
            );
            TypeFactsValue::Available(Box::new(TypeFacts {
                is_copy,
                carries_linear,
                needs_drop,
                destructor,
                shape,
            }))
        }
    };
    if let TypeFactsValue::Available(facts) = &mut value {
        facts.shape = canonical_shape;
    }
    let kind = if matches!(value, TypeFactsValue::Failure(_)) {
        QueryTerminalKind::Failure
    } else {
        QueryTerminalKind::Success
    };
    Ok(QueryOutput::success(value).with_terminal_kind(kind))
}

pub(super) fn scalar_layout(ty: &crate::TypeInstanceKey) -> crate::type_queries::CanonicalLayout {
    use crate::TypeInstanceKey as T;
    let size = match ty {
        T::I8 | T::U8 | T::Bool => 1,
        T::I16 | T::U16 => 2,
        T::I32 | T::U32 => 4,
        T::I64 | T::U64 => 8,
        T::Unit | T::Never => 0,
        _ => 8,
    };
    crate::type_queries::CanonicalLayout {
        size,
        alignment: size.max(1),
        stride: size,
        abi_slots: u32::from(size != 0),
        slot_identical: matches!(ty, T::I64 | T::U64 | T::Unit | T::Never),
        kind: crate::type_queries::CanonicalLayoutKind::Scalar,
    }
}

pub(super) fn layout_from_terminal(
    terminal: &rue_query::QueryTerminal<crate::type_queries::LayoutValue>,
) -> Result<&crate::type_queries::CanonicalLayout, crate::type_queries::TypeQueryFailure> {
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!("Layout publishes typed values")
    };
    match value {
        crate::type_queries::LayoutValue::Available(layout) => Ok(layout),
        crate::type_queries::LayoutValue::Failure(failure) => Err(failure.clone()),
    }
}

pub(super) fn evaluate_layout(
    context: &rue_query::QueryContext,
    family: &QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::LayoutValue>,
    type_shapes: &QueryFamily<
        crate::type_queries::TypeQueryKey,
        crate::type_queries::TypeShapeValue,
    >,
    key: &crate::type_queries::TypeQueryKey,
) -> Result<QueryOutput<crate::type_queries::LayoutValue>, QueryAbort> {
    use crate::TypeInstanceKey as T;
    use crate::type_queries::{CanonicalLayout, CanonicalLayoutKind, LayoutValue, TypeShape};
    if matches!(key.ty, T::PtrConst(_) | T::PtrMut(_)) {
        return Ok(QueryOutput::success(LayoutValue::Available(
            CanonicalLayout {
                size: 8,
                alignment: 8,
                stride: 8,
                abi_slots: 1,
                slot_identical: true,
                kind: CanonicalLayoutKind::Pointer,
            },
        )));
    }
    if matches!(key.ty, T::Slice { .. }) {
        // A generated slice is the canonical `{ data: ptr const T, len: u64 }`
        // fat pointer. Its pointee remains deliberately unobserved.
        return Ok(QueryOutput::success(LayoutValue::Available(
            CanonicalLayout {
                size: 16,
                alignment: 8,
                stride: 16,
                abi_slots: 2,
                slot_identical: true,
                kind: CanonicalLayoutKind::Slice,
            },
        )));
    }
    if matches!(
        key.ty,
        T::I8
            | T::I16
            | T::I32
            | T::I64
            | T::U8
            | T::U16
            | T::U32
            | T::U64
            | T::Bool
            | T::Unit
            | T::Never
    ) {
        return Ok(QueryOutput::success(LayoutValue::Available(scalar_layout(
            &key.ty,
        ))));
    }
    let shape_terminal = context.query_registered(type_shapes, key.clone())?;
    let shape = match type_shape_from_terminal(&shape_terminal) {
        Ok(shape) => shape,
        Err(failure) => {
            return Ok(QueryOutput::success(LayoutValue::Failure(failure))
                .with_terminal_kind(QueryTerminalKind::Failure));
        }
    };
    let value = match shape {
        TypeShape::Array { element, len } => {
            if *len == 0 {
                return Ok(QueryOutput::success(LayoutValue::Available(
                    CanonicalLayout {
                        size: 0,
                        alignment: 1,
                        stride: 0,
                        abi_slots: 0,
                        slot_identical: true,
                        kind: CanonicalLayoutKind::Array {
                            element: None,
                            count: 0,
                        },
                    },
                )));
            }
            let element = context.query_registered(
                family,
                crate::type_queries::TypeQueryKey {
                    ty: element.clone(),
                    configuration: key.configuration.clone(),
                },
            )?;
            match layout_from_terminal(&element) {
                Ok(element) => {
                    let size = element.stride.saturating_mul(*len);
                    LayoutValue::Available(CanonicalLayout {
                        size,
                        alignment: element.alignment,
                        stride: size,
                        abi_slots: u32::try_from(u64::from(element.abi_slots).saturating_mul(*len))
                            .unwrap_or(u32::MAX),
                        slot_identical: element.slot_identical,
                        kind: CanonicalLayoutKind::Array {
                            element: Some(Box::new(element.clone())),
                            count: *len,
                        },
                    })
                }
                Err(failure) => LayoutValue::Failure(failure),
            }
        }
        TypeShape::Struct { fields } => {
            let terminals = context.query_registered_adaptive_batch(
                family,
                fields
                    .iter()
                    .map(|(_, ty)| crate::type_queries::TypeQueryKey {
                        ty: ty.clone(),
                        configuration: key.configuration.clone(),
                    }),
            )?;
            let mut offset = 0u64;
            let mut alignment = 1u64;
            let mut slots = 0u32;
            let mut slot_identical = true;
            let mut offsets = Vec::with_capacity(terminals.len());
            let mut padding_ranges = Vec::new();
            for terminal in &terminals {
                let layout = match layout_from_terminal(terminal) {
                    Ok(layout) => layout,
                    Err(failure) => {
                        return Ok(QueryOutput::success(LayoutValue::Failure(failure))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                };
                let placed = crate::type_queries::align_to(offset, layout.alignment);
                if placed > offset {
                    padding_ranges.push(rue_air::PaddingRange {
                        start: offset,
                        end: placed,
                    });
                }
                offsets.push(placed);
                offset = placed.saturating_add(layout.size);
                alignment = alignment.max(layout.alignment);
                slots = slots.saturating_add(layout.abi_slots);
                slot_identical &= layout.slot_identical;
            }
            let size = crate::type_queries::align_to(offset, alignment);
            if size > offset {
                padding_ranges.push(rue_air::PaddingRange {
                    start: offset,
                    end: size,
                });
            }
            LayoutValue::Available(CanonicalLayout {
                size,
                alignment,
                stride: size,
                abi_slots: slots,
                slot_identical,
                kind: CanonicalLayoutKind::Struct {
                    field_offsets: offsets.into(),
                    padding_ranges: padding_ranges.into(),
                },
            })
        }
        TypeShape::Enum { variants } => {
            let keys = variants
                .iter()
                .flat_map(|(_, fields)| fields.iter())
                .cloned()
                .map(|ty| crate::type_queries::TypeQueryKey {
                    ty,
                    configuration: key.configuration.clone(),
                })
                .collect::<Vec<_>>();
            let terminals = context.query_registered_adaptive_batch(family, keys)?;
            let tag_size = match variants.len() {
                0..=256 => 1,
                257..=65536 => 2,
                _ => 4,
            };
            let mut cursor = 0usize;
            let mut payload_size = 0u64;
            let mut payload_alignment = 1u64;
            let mut max_slots = 0u32;
            let mut projected = Vec::with_capacity(variants.len());
            for (_, fields) in variants.iter() {
                let mut offset = 0u64;
                let mut variant_slots = 0u32;
                let mut offsets = Vec::with_capacity(fields.len());
                for _ in fields.iter() {
                    let layout = match layout_from_terminal(&terminals[cursor]) {
                        Ok(layout) => layout,
                        Err(failure) => {
                            return Ok(QueryOutput::success(LayoutValue::Failure(failure))
                                .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                    };
                    cursor += 1;
                    offset = crate::type_queries::align_to(offset, layout.alignment);
                    offsets.push(offset);
                    offset = offset.saturating_add(layout.size);
                    payload_alignment = payload_alignment.max(layout.alignment);
                    variant_slots = variant_slots.saturating_add(layout.abi_slots);
                }
                payload_size = payload_size.max(offset);
                max_slots = max_slots.max(variant_slots);
                projected.push(offsets.into());
            }
            let alignment = payload_alignment.max(tag_size);
            let payload_offset = crate::type_queries::align_to(tag_size, payload_alignment);
            let size = crate::type_queries::align_to(
                payload_offset.saturating_add(payload_size),
                alignment,
            );
            LayoutValue::Available(CanonicalLayout {
                size,
                alignment,
                stride: size,
                abi_slots: 1u32.saturating_add(max_slots),
                // Compact enums always carry a narrow tag rather than the
                // eight-byte discriminant slot used by value decomposition.
                slot_identical: false,
                kind: CanonicalLayoutKind::Enum {
                    tag_size,
                    payload_offset,
                    variants: projected
                        .into_iter()
                        .map(|offsets: Arc<[u64]>| {
                            offsets
                                .iter()
                                .map(|offset| payload_offset.saturating_add(*offset))
                                .collect::<Vec<_>>()
                                .into()
                        })
                        .collect::<Vec<_>>()
                        .into(),
                },
            })
        }
        TypeShape::Scalar => LayoutValue::Available(scalar_layout(&key.ty)),
        TypeShape::Pointer => unreachable!("pointer layouts return before TypeShape"),
        TypeShape::Slice => unreachable!("slice layouts return before TypeShape"),
        TypeShape::Opaque => LayoutValue::Failure(crate::type_queries::TypeQueryFailure::Invalid(
            Arc::from("type is not materializable"),
        )),
    };
    let kind = if matches!(value, LayoutValue::Failure(_)) {
        QueryTerminalKind::Failure
    } else {
        QueryTerminalKind::Success
    };
    Ok(QueryOutput::success(value).with_terminal_kind(kind))
}

#[derive(Debug)]
pub(super) struct StableCallableSignature {
    pub(super) parameters: Vec<(
        crate::durable_semantics::DurableParameterMode,
        crate::TypeInstanceKey,
    )>,
    pub(super) result: crate::TypeInstanceKey,
    /// The C convention this callable is *entered* under, already resolved from
    /// the declaration's ABI string against the compilation target, or `None`
    /// for a callable entered under Rue's native convention. Only a foreign
    /// declaration is a C entry: a `pub extern` export's own body is native and
    /// its separate thunk is the crossing.
    pub(super) foreign_convention: Option<rue_target::CallingConvention>,
}

pub(super) fn named_callable_owner_type(
    definition: &crate::StableDefinitionKey,
) -> Option<crate::TypeInstanceKey> {
    let owner = definition.owner()?;
    Some(crate::TypeInstanceKey::Nominal(
        crate::NominalInstanceKey::Named(crate::StableDefinitionKey::from_stable_parts(
            owner.module().clone(),
            crate::StableDefinitionNamespace::Type,
            owner.kind(),
            owner.name(),
            None,
        )),
    ))
}

pub(super) fn anonymous_method_type(
    ty: &crate::durable_semantics::DurableAnonymousMethodType,
    owner: &crate::TypeInstanceKey,
) -> crate::TypeInstanceKey {
    match ty {
        crate::durable_semantics::DurableAnonymousMethodType::SelfType => owner.clone(),
        crate::durable_semantics::DurableAnonymousMethodType::Concrete(ty) => {
            crate::type_queries::type_instance(ty)
        }
    }
}

pub(super) fn durable_parameter_is_runtime(
    parameter: &crate::durable_semantics::DurableSemanticParameter,
) -> bool {
    !parameter.is_comptime || parameter.ty != crate::durable_semantics::DurableType::ComptimeType
}

pub(super) fn anonymous_parameter_is_runtime(
    ty: &crate::durable_semantics::DurableAnonymousMethodType,
    is_comptime: bool,
) -> bool {
    !is_comptime
        || !matches!(
            ty,
            crate::durable_semantics::DurableAnonymousMethodType::Concrete(
                crate::durable_semantics::DurableType::ComptimeType
            )
        )
}

pub(super) fn exact_specialized_callable_types(
    context: &rue_query::QueryContext,
    semantic_nucleus: &SemanticNucleusFamily,
    declaration_shells: &QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    lookup_names: &QueryFamily<LookupNameKey, LookupNameValue>,
    declaration: &crate::declaration_candidate::DeclarationCandidateKey,
    definition: &crate::StableDefinitionKey,
    signature_parameters: &[crate::durable_semantics::DurableSemanticParameter],
    callable_type_syntax: &rue_air::DurableCallableTypeSyntax,
    arguments: &crate::CanonicalArguments,
    configuration: &crate::semantic_query_nucleus::SemanticQueryConfiguration,
) -> Result<
    Result<
        (
            Vec<crate::durable_semantics::DurableType>,
            crate::durable_semantics::DurableType,
        ),
        crate::type_queries::TypeQueryFailure,
    >,
    QueryAbort,
> {
    use crate::type_queries::TypeQueryFailure;

    let shell = context.query_registered(
        declaration_shells,
        DeclarationShellQueryKey(declaration.clone()),
    )?;
    let rue_query::QueryOutcome::Success(shell) = shell.outcome() else {
        unreachable!("DeclarationShell publishes typed values")
    };
    let DeclarationShellQueryValue::Available(shell) = shell else {
        return Ok(Err(TypeQueryFailure::Unavailable(Arc::from(format!(
            "specialized callable shell is unavailable: {shell:?}"
        )))));
    };
    if shell.parameters.len() != signature_parameters.len() {
        return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
            "specialized callable shell disagrees with its semantic signature",
        ))));
    }

    let mut type_arguments = arguments.types.iter();
    let mut value_arguments = arguments.values.iter();
    let mut substitutions = BTreeMap::new();
    let mut value_substitutions = BTreeMap::new();
    for (header, parameter) in shell.parameters.iter().zip(signature_parameters) {
        if !header.is_comptime {
            continue;
        }
        if parameter.ty == crate::durable_semantics::DurableType::ComptimeType {
            let Some(argument) = type_arguments
                .next()
                .and_then(durable_type_from_instance_key)
            else {
                return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
                    "specialized callable has an invalid comptime type argument stream",
                ))));
            };
            substitutions.insert(header.name.clone(), argument);
        } else {
            let Some(argument) = value_arguments.next().and_then(durable_value_from_argument)
            else {
                return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
                    "specialized callable has an invalid comptime value argument stream",
                ))));
            };
            value_substitutions.insert(header.name.clone(), argument);
        }
    }
    if type_arguments.next().is_some() || value_arguments.next().is_some() {
        return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
            "specialized callable has excess comptime arguments",
        ))));
    }

    if callable_type_syntax.parameters.len() != signature_parameters.len() {
        return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
            "specialized callable syntax disagrees with its semantic signature",
        ))));
    }

    let mut provider = SemanticNucleusTypeProvider {
        context,
        family: semantic_nucleus,
        shells: declaration_shells,
        names: lookup_names,
        configuration: configuration.clone(),
        substitutions,
        value_substitutions,
        deferred_value_parameters: BTreeMap::new(),
        anonymous_nominals: BTreeMap::new(),
        dependency_source: definition.clone(),
        dependency_kind: rue_air::DeclarationTypeDependencyKind::Signature,
        dependencies: BTreeSet::new(),
        deferred_ownership: BTreeSet::new(),
        ownership_properties: BTreeMap::new(),
    };
    let mut resolve = |root: rue_rir::RirTypeSyntaxRef| {
        rue_air::resolve_structured_semantic_type_syntax(
            &mut provider,
            &declaration.module,
            &callable_type_syntax.syntax,
            root,
        )
        .map_err(semantic_type_query_failure)
    };
    let mut runtime_parameters = Vec::new();
    for (parameter, _semantic) in callable_type_syntax
        .parameters
        .iter()
        .zip(signature_parameters)
        .filter(|(_, semantic)| durable_parameter_is_runtime(semantic))
    {
        match resolve(*parameter) {
            Ok(ty) => runtime_parameters.push(ty),
            Err(ResolveSemanticSignatureError::Abort(abort)) => return Err(abort),
            Err(ResolveSemanticSignatureError::Failure(failure)) => {
                return Ok(Err(TypeQueryFailure::Invalid(Arc::from(format!(
                    "specialized callable parameter type failed to resolve: {failure:?}"
                )))));
            }
        }
    }
    let result = match resolve(callable_type_syntax.result) {
        Ok(result) => result,
        Err(ResolveSemanticSignatureError::Abort(abort)) => return Err(abort),
        Err(ResolveSemanticSignatureError::Failure(failure)) => {
            return Ok(Err(TypeQueryFailure::Invalid(Arc::from(format!(
                "specialized callable result type failed to resolve: {failure:?}"
            )))));
        }
    };
    Ok(Ok((runtime_parameters, result)))
}

pub(super) fn query_callable_signature(
    context: &rue_query::QueryContext,
    semantic_nucleus: &SemanticNucleusFamily,
    declaration_shells: &QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    lookup_names: &QueryFamily<LookupNameKey, LookupNameValue>,
    body_produced_anonymous: &QueryFamily<
        crate::body_query::BodyQueryKey,
        crate::body_query::ProducedAnonymous,
    >,
    key: &crate::type_queries::CallAbiQueryKey,
) -> Result<Result<StableCallableSignature, crate::type_queries::TypeQueryFailure>, QueryAbort> {
    use crate::durable_semantics::DurableParameterMode;
    use crate::type_queries::TypeQueryFailure;
    match &key.callable {
        crate::FunctionInstanceKey::DropGlue(owner) => Ok(Ok(StableCallableSignature {
            parameters: vec![(DurableParameterMode::Value, owner.as_ref().clone())],
            result: crate::TypeInstanceKey::Unit,
            foreign_convention: None,
        })),
        // A structural printer takes the error value and hands back a borrowed
        // `{ptr, len}` view of the rendering it wrote (ADR-0083 §1).
        crate::FunctionInstanceKey::ErrorPrinter(owner) => Ok(Ok(StableCallableSignature {
            parameters: vec![(DurableParameterMode::Value, owner.as_ref().clone())],
            result: crate::TypeInstanceKey::BuiltinNominal {
                kind: crate::AnonymousNominalKind::Struct,
                name: Arc::from("str"),
            },
            foreign_convention: None,
        })),
        crate::FunctionInstanceKey::Definition(definition)
            if definition.kind() == crate::StableDefinitionKind::Destructor =>
        {
            let Some(owner) = named_callable_owner_type(definition) else {
                return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
                    "named destructor has no nominal owner",
                ))));
            };
            Ok(Ok(StableCallableSignature {
                parameters: vec![(DurableParameterMode::Value, owner)],
                result: crate::TypeInstanceKey::Unit,
                foreign_convention: None,
            }))
        }
        crate::FunctionInstanceKey::AnonymousMember { owner, member } => {
            let crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(identity)) =
                owner.as_ref()
            else {
                return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
                    "anonymous member owner is not an anonymous nominal",
                ))));
            };
            let nominal = match query_anonymous_nominal(
                context,
                semantic_nucleus,
                body_produced_anonymous,
                identity,
                &key.configuration,
            )? {
                Ok(nominal) => nominal,
                Err(failure) => return Ok(Err(failure)),
            };
            let crate::durable_semantics::DurableAnonymousNominalShape::Struct { methods, .. } =
                &nominal.shape
            else {
                return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
                    "anonymous enum has no callable members",
                ))));
            };
            let signature = methods.iter().find(|signature| {
                signature.name == member.name
                    && match member.kind {
                        crate::AnonymousMemberKind::Destructor => {
                            rue_air::drop_glue::is_anonymous_destructor(
                                signature.name.as_ref(),
                                signature.has_self,
                            )
                        }
                        crate::AnonymousMemberKind::Method => signature.has_self,
                        crate::AnonymousMemberKind::AssociatedFunction => !signature.has_self,
                    }
            });
            let Some(signature) = signature else {
                return Ok(Err(TypeQueryFailure::Unavailable(Arc::from(
                    "anonymous member signature is unavailable",
                ))));
            };
            let mut parameters = Vec::new();
            if signature.has_self {
                parameters.push((signature.self_mode, owner.as_ref().clone()));
            }
            parameters.extend(
                signature
                    .parameters
                    .iter()
                    .filter(|(ty, _, comptime)| anonymous_parameter_is_runtime(ty, *comptime))
                    .map(|(ty, mode, _)| (*mode, anonymous_method_type(ty, owner))),
            );
            Ok(Ok(StableCallableSignature {
                parameters,
                result: anonymous_method_type(&signature.result, owner),
                foreign_convention: None,
            }))
        }
        callable => {
            let Some(definition) = function_definition_key(callable) else {
                return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
                    "callable identity has no signature contract",
                ))));
            };
            let Some(candidates) = stable_syntax_candidate_set(definition) else {
                return Ok(Err(TypeQueryFailure::Unavailable(Arc::from(
                    "callable has no declaration candidate",
                ))));
            };
            let candidates = candidates.into_iter().flatten().collect::<Vec<_>>();
            let terminals = context.query_registered_adaptive_batch(
                semantic_nucleus,
                candidates.iter().cloned().map(|declaration| {
                    crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                        crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                            declaration,
                            configuration: key.configuration.clone(),
                        },
                    )
                }),
            )?;
            let mut observed_failures = Vec::new();
            let mut signatures =
                terminals
                    .iter()
                    .zip(&candidates)
                    .filter_map(|(terminal, declaration)| {
                        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                            unreachable!("SemanticNucleus publishes typed values")
                        };
                        match value {
                            crate::semantic_query_nucleus::SemanticNucleusValue::Signature(
                                signature,
                            ) => Some((signature, declaration)),
                            other => {
                                observed_failures.push(format!("{other:?}"));
                                None
                            }
                        }
                    });
            let Some((signature, declaration)) = signatures.next() else {
                return Ok(Err(TypeQueryFailure::Unavailable(Arc::from(format!(
                    "callable signature is unavailable: {}",
                    observed_failures.join("; ")
                )))));
            };
            if signatures.next().is_some() {
                return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
                    "callable signature is ambiguous",
                ))));
            }
            let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                parameters,
                result,
                has_self,
                self_mode,
                is_extern,
                is_c_export: _,
                foreign_convention,
                ..
            } = &signature.signature
            else {
                return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
                    "definition is not callable",
                ))));
            };
            let mut runtime_parameters = Vec::new();
            if *has_self {
                let Some(owner) = named_callable_owner_type(definition) else {
                    return Ok(Err(TypeQueryFailure::Invalid(Arc::from(
                        "receiver-taking callable has no nominal owner",
                    ))));
                };
                runtime_parameters.push((*self_mode, owner));
            }
            let (runtime_types, result) = match callable {
                crate::FunctionInstanceKey::Specialization { arguments, .. } => {
                    let Some(callable_type_syntax) = signature.callable_type_syntax.as_ref() else {
                        return Ok(Err(TypeQueryFailure::Unavailable(Arc::from(
                            "specialized callable type syntax is unavailable",
                        ))));
                    };
                    match exact_specialized_callable_types(
                        context,
                        semantic_nucleus,
                        declaration_shells,
                        lookup_names,
                        declaration,
                        definition,
                        parameters,
                        callable_type_syntax,
                        arguments,
                        &key.configuration,
                    )? {
                        Ok(signature) => signature,
                        Err(failure) => return Ok(Err(failure)),
                    }
                }
                _ => (
                    parameters
                        .iter()
                        .filter(|parameter| durable_parameter_is_runtime(parameter))
                        .map(|parameter| parameter.ty.clone())
                        .collect(),
                    result.clone(),
                ),
            };
            runtime_parameters.extend(
                parameters
                    .iter()
                    .filter(|parameter| durable_parameter_is_runtime(parameter))
                    .zip(runtime_types)
                    .map(|(parameter, ty)| {
                        (parameter.mode, crate::type_queries::type_instance(&ty))
                    }),
            );
            Ok(Ok(StableCallableSignature {
                parameters: runtime_parameters,
                result: crate::type_queries::type_instance(&result),
                // A C export's source body uses Rue's native ABI. The separate
                // entry thunk is the C boundary, and carries the declaration's
                // convention on its own.
                foreign_convention: is_extern.then_some(*foreign_convention).flatten(),
            }))
        }
    }
}

pub(super) fn stable_type_is_aggregate(ty: &crate::TypeInstanceKey) -> bool {
    use crate::{NominalInstanceKey as N, TypeInstanceKey as T};
    match ty {
        T::Array { .. } | T::Slice { .. } => true,
        T::BuiltinNominal { .. } | T::Nominal(N::Builtin { .. }) => true,
        T::Nominal(N::Named(definition)) => matches!(
            definition.kind(),
            crate::StableDefinitionKind::Struct | crate::StableDefinitionKind::Enum
        ),
        T::Nominal(N::Anonymous(_)) => true,
        _ => false,
    }
}

pub(super) fn stable_type_is_strbuf(ty: &crate::TypeInstanceKey) -> bool {
    matches!(
        ty,
        crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(definition))
            if definition.module().is_trusted_standard_library()
                && definition.kind() == crate::StableDefinitionKind::Struct
                && definition.name() == "StrBuf"
    )
}

/// The stable plane's projection of a scalar type key onto its target-C
/// width-and-signedness class; the extension operation itself lives on
/// [`rue_air::CAbiScalarKind::extension`], shared with the live classifier.
pub(super) fn c_scalar_kind(ty: &crate::TypeInstanceKey) -> rue_air::CAbiScalarKind {
    use crate::TypeInstanceKey as T;
    use rue_air::CAbiScalarKind as K;
    match ty {
        T::I8 => K::I8,
        T::I16 => K::I16,
        T::I32 => K::I32,
        T::U8 => K::U8,
        T::Bool => K::Bool,
        T::U16 => K::U16,
        T::U32 => K::U32,
        // Register-width scalars (i64/u64, pointers) and every remaining key
        // this projection can see need no extension.
        _ => K::RegisterWidth,
    }
}

/// The stable plane's projection of one type onto the target-C classification
/// facts.
///
/// The live classifier makes the same projection from the request-scoped type
/// pool ([`rue_air::c_abi_type_facts`]); both then classify through the one
/// kernel, [`rue_air::lower_c_signature`]. That is why an `extern "C"` import,
/// a `pub extern "C" fn` export, and this query cannot disagree about where a
/// value crosses.
pub(super) fn stable_c_abi_type_facts(
    layout: &crate::type_queries::CanonicalLayout,
    ty: &crate::TypeInstanceKey,
) -> rue_air::CAbiTypeFacts {
    if stable_type_is_aggregate(ty) {
        return rue_air::CAbiTypeFacts::Aggregate {
            size: layout.size,
            align: layout.alignment,
        };
    }
    if layout.abi_slots == 0 {
        return rue_air::CAbiTypeFacts::ZeroSized;
    }
    let kind = c_scalar_kind(ty);
    rue_air::CAbiTypeFacts::Scalar {
        kind,
        class: kind.register_class(),
    }
}

/// The stable plane's projection of one type onto the shared native
/// classification kernel: the canonical layout supplies the slot count and
/// slot-identity, the stable type keys supply the aggregate and `StrBuf`
/// predicates. The decision tree itself lives on
/// [`rue_air::NativeAbiTypeFacts`].
pub(super) fn stable_native_abi_facts(
    layout: &crate::type_queries::CanonicalLayout,
    ty: &crate::TypeInstanceKey,
) -> rue_air::NativeAbiTypeFacts {
    rue_air::NativeAbiTypeFacts {
        abi_slots: layout.abi_slots,
        aggregate: stable_type_is_aggregate(ty),
        strbuf: stable_type_is_strbuf(ty),
        slot_identical: layout.slot_identical,
    }
}

pub(super) fn evaluate_call_abi(
    context: &rue_query::QueryContext,
    semantic_nucleus: &SemanticNucleusFamily,
    declaration_shells: &QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    lookup_names: &QueryFamily<LookupNameKey, LookupNameValue>,
    body_produced_anonymous: &QueryFamily<
        crate::body_query::BodyQueryKey,
        crate::body_query::ProducedAnonymous,
    >,
    layouts: &QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::LayoutValue>,
    key: &crate::type_queries::CallAbiQueryKey,
) -> Result<QueryOutput<crate::type_queries::CallAbiValue>, QueryAbort> {
    use crate::durable_semantics::DurableParameterMode;
    use crate::type_queries::{
        CallAbiArgument, CallAbiArgumentClass as A, CallAbiFacts, CallAbiReturnClass as R,
        CallAbiValue,
    };
    use rue_target::CallingConvention;
    let signature = match query_callable_signature(
        context,
        semantic_nucleus,
        declaration_shells,
        lookup_names,
        body_produced_anonymous,
        key,
    )? {
        Ok(signature) => signature,
        Err(failure) => {
            return Ok(QueryOutput::success(CallAbiValue::Failure(failure))
                .with_terminal_kind(QueryTerminalKind::Failure));
        }
    };
    // A foreign declaration's convention was resolved once, from its written ABI
    // string against this compilation target, when its signature was checked
    // (9.3:1b); the stable plane carries that row rather than re-deriving the
    // target's own C row here. Everything else is entered natively.
    let convention = signature
        .foreign_convention
        .unwrap_or(CallingConvention::Rue);
    // Every layout this ABI needs, in one batch: each by-value parameter in
    // signature order, then the result. A reference parameter is passed as a
    // pointer whatever it points at, so it contributes no key and no layout
    // dependency.
    //
    // These requests are independent — one parameter's layout never feeds
    // another's — but issuing them one at a time made them a serial chain
    // through the query runtime. The adaptive batch keeps the same stable
    // order and the same per-key observations while letting independent
    // evaluators run concurrently, and degrades to in-task requests when the
    // enclosing batch already owns every worker slot.
    //
    // Duplicate parameter types stay duplicated rather than deduplicated, so
    // a signature observes the layout family exactly as often as it did
    // before, and the repeats resolve against the family memo.
    let layout_terminals = context.query_registered_adaptive_batch(
        layouts,
        signature
            .parameters
            .iter()
            .filter(|(mode, _)| matches!(mode, DurableParameterMode::Value))
            .map(|(_, ty)| ty)
            .chain(std::iter::once(&signature.result))
            .map(|ty| crate::type_queries::TypeQueryKey {
                ty: ty.clone(),
                configuration: key.configuration.clone(),
            }),
    )?;
    let Some((return_terminal, parameter_terminals)) = layout_terminals.split_last() else {
        unreachable!("the batch always carries the result layout")
    };

    // The batch resolves every layout before any is inspected, so a failing
    // parameter no longer short-circuits the requests after it. Reporting
    // still walks parameters in signature order and stops at the first
    // failure, so which failure a caller sees is unchanged.
    let mut parameter_terminals = parameter_terminals.iter();
    let mut layouts = Vec::with_capacity(signature.parameters.len());
    for (mode, _) in &signature.parameters {
        if !matches!(mode, DurableParameterMode::Value) {
            layouts.push(None);
            continue;
        }
        let Some(terminal) = parameter_terminals.next() else {
            unreachable!("the batch carries one layout per by-value parameter")
        };
        match layout_from_terminal(terminal) {
            Ok(layout) => layouts.push(Some(layout)),
            Err(failure) => {
                return Ok(QueryOutput::success(CallAbiValue::Failure(failure))
                    .with_terminal_kind(QueryTerminalKind::Failure));
            }
        }
    }
    let return_layout = match layout_from_terminal(return_terminal) {
        Ok(layout) => layout,
        Err(failure) => {
            return Ok(QueryOutput::success(CallAbiValue::Failure(failure))
                .with_terminal_kind(QueryTerminalKind::Failure));
        }
    };

    // A C boundary is placed by the one classification function every crossing
    // site consumes; the native convention keeps its own decision tree. The
    // stable plane projects the facts from canonical layout values and stable
    // type keys, and the live classifier projects the same facts from the
    // request-scoped type pool, so the two planes classify identically.
    let lowered = (!convention.is_rue()).then(|| {
        let parameters = signature
            .parameters
            .iter()
            .zip(&layouts)
            .map(|((mode, ty), layout)| match (mode, layout) {
                (DurableParameterMode::Value, Some(layout)) => (
                    stable_c_abi_type_facts(layout, ty),
                    rue_air::ArgConvention::ByValue,
                ),
                // A reference parameter is one pointer whatever it points at,
                // so it contributes no layout and needs none.
                _ => (
                    rue_air::CAbiTypeFacts::by_reference_pointer(),
                    rue_air::ArgConvention::ByReference,
                ),
            })
            .collect::<Vec<_>>();
        rue_air::lower_c_signature(
            convention,
            &parameters,
            stable_c_abi_type_facts(return_layout, &signature.result),
        )
    });

    let mut arguments = Vec::with_capacity(signature.parameters.len());
    for (index, ((mode, ty), layout)) in signature.parameters.iter().zip(&layouts).enumerate() {
        let Some(layout) = layout else {
            arguments.push(CallAbiArgument {
                mode: *mode,
                value_slots: 1,
                class: A::Reference,
            });
            continue;
        };
        let class = match &lowered {
            None => match stable_native_abi_facts(layout, ty)
                .classify_arg(rue_air::ArgConvention::ByValue)
            {
                rue_air::ArgClass::Omitted => A::Omitted,
                rue_air::ArgClass::Direct { slot_count } => A::NativeDirect { slots: slot_count },
                rue_air::ArgClass::Indirect => A::NativeIndirect,
            },
            Some(lowered) => {
                let argument = lowered.arguments()[index];
                match argument.location {
                    rue_air::ArgLocation::Omitted => A::Omitted,
                    _ if !stable_type_is_aggregate(ty) => A::CScalar {
                        extension: argument.extension,
                    },
                    rue_air::ArgLocation::Registers { count, .. } => {
                        A::CIntegerRegisters { eightbytes: count }
                    }
                    rue_air::ArgLocation::Stack { size, align, .. } => A::CByValueStack {
                        size,
                        alignment: align,
                    },
                    rue_air::ArgLocation::Indirect { size, align, .. } => A::CByReferenceCopy {
                        size,
                        alignment: align,
                    },
                }
            }
        };
        arguments.push(CallAbiArgument {
            mode: *mode,
            value_slots: layout.abi_slots,
            class,
        });
    }
    let return_class = match &lowered {
        None => {
            let budget = rue_air::native_return_register_budget(key.configuration.target.arch());
            match stable_native_abi_facts(return_layout, &signature.result).classify_return(budget)
            {
                rue_air::ReturnClass::ZeroSized => R::ZeroSized,
                rue_air::ReturnClass::Scalar => R::Scalar {
                    extension: rue_air::ScalarAbiExtension::None,
                },
                rue_air::ReturnClass::Registers { slot_count } => {
                    R::NativeRegisters { slots: slot_count }
                }
                rue_air::ReturnClass::Indirect { slot_count } => {
                    R::NativeIndirect { slots: slot_count }
                }
            }
        }
        Some(lowered) => match lowered.ret() {
            rue_air::LoweredReturn::Void => R::ZeroSized,
            rue_air::LoweredReturn::Registers {
                count, extension, ..
            } => {
                if stable_type_is_aggregate(&signature.result) {
                    R::CIntegerRegisters { eightbytes: count }
                } else {
                    R::Scalar { extension }
                }
            }
            rue_air::LoweredReturn::Sret { size, align, .. } => R::CIndirect {
                size,
                alignment: align,
            },
        },
    };
    Ok(QueryOutput::success(CallAbiValue::Available(
        CallAbiFacts {
            convention,
            return_class,
            arguments: arguments.into(),
            native_symbol: convention.is_rue().then(|| {
                Arc::from(crate::StableSymbolEncoder::encode(
                    &crate::StableSymbolId::Callable(crate::StableCallableId::Function(
                        key.callable.clone(),
                    )),
                ))
            }),
        },
    )))
}

pub(super) fn evaluate_drop_glue(
    context: &rue_query::QueryContext,
    type_facts: &QueryFamily<
        crate::type_queries::TypeQueryKey,
        crate::type_queries::TypeFactsValue,
    >,
    key: &crate::type_queries::TypeQueryKey,
) -> Result<QueryOutput<crate::type_queries::DropGlueValue>, QueryAbort> {
    use crate::type_queries::{
        DropGlueFacts, DropGlueField, DropGluePlan, DropGlueValue, DropGlueVariant,
        DropGlueVariantField, TypeShape,
    };
    let terminal = context.query_registered(type_facts, key.clone())?;
    let facts = match type_facts_from_terminal(&terminal) {
        Ok(facts) => facts,
        Err(failure) => {
            return Ok(QueryOutput::success(DropGlueValue::Failure(failure))
                .with_terminal_kind(QueryTerminalKind::Failure));
        }
    };
    // `evaluate_type_facts` queries `compiler.type-shape` for this same key and
    // stamps the answer onto every `Available` value it publishes, so the facts
    // already carry the canonical shape. Asking the shape family again would
    // repeat a lookup per drop-glue request for a value in hand, and it cannot
    // disagree: facts that resolved at all resolved through that shape, and the
    // dependency this drops is still observed transitively through the facts
    // edge, so invalidation reaches here unchanged.
    let shape = &facts.shape;
    let children = match shape {
        TypeShape::Array { element, len } if *len != 0 => vec![element.clone()],
        TypeShape::Array { .. } => Vec::new(),
        TypeShape::Struct { fields } => fields.iter().map(|(_, ty)| ty.clone()).collect(),
        TypeShape::Enum { variants } => variants
            .iter()
            .flat_map(|(_, fields)| fields.iter().cloned())
            .collect(),
        TypeShape::Scalar | TypeShape::Pointer | TypeShape::Slice | TypeShape::Opaque => Vec::new(),
    };
    let terminals = context.query_registered_adaptive_batch(
        type_facts,
        children
            .iter()
            .cloned()
            .map(|ty| crate::type_queries::TypeQueryKey {
                ty,
                configuration: key.configuration.clone(),
            }),
    )?;
    let mut nested = Vec::new();
    let mut decisions = Vec::new();
    for (ty, terminal) in children.into_iter().zip(terminals) {
        match type_facts_from_terminal(&terminal) {
            Ok(child) => {
                if child.needs_drop {
                    nested.push(ty.clone());
                }
                decisions.push((ty, child.needs_drop));
            }
            Err(failure) => {
                return Ok(QueryOutput::success(DropGlueValue::Failure(failure))
                    .with_terminal_kind(QueryTerminalKind::Failure));
            }
        }
    }
    nested.sort();
    nested.dedup();
    let mut decisions = decisions.into_iter();
    let plan = match shape {
        TypeShape::Struct { fields } => DropGluePlan::Struct {
            fields: fields
                .iter()
                .map(|(name, ty)| {
                    let (_observed, drop) = decisions
                        .next()
                        .expect("one ownership decision per struct field");
                    DropGlueField {
                        name: name.clone(),
                        ty: ty.clone(),
                        drop,
                    }
                })
                .collect::<Vec<_>>()
                .into(),
        },
        TypeShape::Array { element, len } => DropGluePlan::Array {
            element: element.clone(),
            len: *len,
            drop_element: if *len == 0 {
                false
            } else {
                decisions
                    .next()
                    .expect("one ownership decision for a non-empty array")
                    .1
            },
        },
        TypeShape::Enum { variants } => DropGluePlan::Enum {
            variants: variants
                .iter()
                .map(|(name, fields)| DropGlueVariant {
                    name: name.clone(),
                    fields: fields
                        .iter()
                        .map(|ty| {
                            let (_observed, drop) = decisions
                                .next()
                                .expect("one ownership decision per enum field");
                            DropGlueVariantField {
                                ty: ty.clone(),
                                drop,
                            }
                        })
                        .collect::<Vec<_>>()
                        .into(),
                })
                .collect::<Vec<_>>()
                .into(),
        },
        TypeShape::Scalar | TypeShape::Pointer | TypeShape::Slice | TypeShape::Opaque => {
            DropGluePlan::None
        }
    };
    Ok(QueryOutput::success(DropGlueValue::Available(Box::new(
        DropGlueFacts {
            required: facts.needs_drop,
            synthesize: facts.needs_drop,
            destructor: facts.destructor.clone(),
            nested: nested.into(),
            plan,
            machine_symbol: facts.needs_drop.then(|| {
                Arc::from(crate::StableSymbolEncoder::encode(
                    &crate::StableSymbolId::Callable(crate::StableCallableId::Function(
                        crate::FunctionInstanceKey::DropGlue(Node::new(key.ty.clone())),
                    )),
                ))
            }),
            destructor_symbol: facts.destructor.as_ref().map(|destructor| {
                Arc::from(crate::StableSymbolEncoder::encode(
                    &crate::StableSymbolId::Callable(crate::StableCallableId::Function(
                        destructor.clone(),
                    )),
                ))
            }),
        },
    ))))
}
