//! Named semantic services and foreign-call authority boundaries.
//!
//! Services accept semantic identities and reduced values, never RIR
//! instructions or evaluator callbacks.

use super::lifecycle::*;
use super::projection::*;
use super::structured::*;
use super::*;

/// The exact durable projection of one named value lookup.  The dependency is
/// deliberately direct: resolving a const, callable, or nominal observes the
/// resolved declaration only, while any transitive const effects remain owned
/// by the const query that produced its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeNamedValueProjection {
    value: EvaluatedSemanticConst,
    dependency: SemanticDeclarationDependency,
    anonymous_nominals: Arc<[DurableAnonymousNominal]>,
}

/// The only declaration kinds considered by durable named-value lookup, in
/// the same order as the established semantic lookup policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeNamedValueKind {
    Const,
    Function,
    Struct,
    Enum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeNamedValueOrder {
    Unqualified,
    ModuleMember,
}

const DURABLE_COMPTIME_UNQUALIFIED_VALUE_KINDS: [DurableComptimeNamedValueKind; 4] = [
    DurableComptimeNamedValueKind::Const,
    DurableComptimeNamedValueKind::Function,
    DurableComptimeNamedValueKind::Struct,
    DurableComptimeNamedValueKind::Enum,
];
const DURABLE_COMPTIME_MODULE_MEMBER_KINDS: [DurableComptimeNamedValueKind; 4] = [
    DurableComptimeNamedValueKind::Const,
    DurableComptimeNamedValueKind::Struct,
    DurableComptimeNamedValueKind::Enum,
    DurableComptimeNamedValueKind::Function,
];

/// Run the canonical named-value candidate order.  The probe is semantic
/// candidate/identity work only; it cannot evaluate an instruction or demand
/// a child query.  Errors stop the order immediately, and the first value
/// stops it without probing later declaration kinds.
pub(crate) fn resolve_named_value_in_order<T, E>(
    probe: impl FnMut(DurableComptimeNamedValueKind) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
    resolve_named_value_with_order(DurableComptimeNamedValueOrder::Unqualified, probe)
}

pub(crate) fn resolve_module_member_in_order<T, E>(
    mut probe: impl FnMut(DurableComptimeNamedValueKind) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
    resolve_named_value_with_order(DurableComptimeNamedValueOrder::ModuleMember, &mut probe)
}

fn resolve_named_value_with_order<T, E>(
    order: DurableComptimeNamedValueOrder,
    mut probe: impl FnMut(DurableComptimeNamedValueKind) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
    let kinds = match order {
        DurableComptimeNamedValueOrder::Unqualified => &DURABLE_COMPTIME_UNQUALIFIED_VALUE_KINDS,
        DurableComptimeNamedValueOrder::ModuleMember => &DURABLE_COMPTIME_MODULE_MEMBER_KINDS,
    };
    for kind in kinds {
        if let Some(value) = probe(*kind)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

impl DurableComptimeNamedValueProjection {
    pub(crate) fn new(
        value: EvaluatedSemanticConst,
        dependency: SemanticDeclarationDependency,
    ) -> Self {
        Self {
            value,
            dependency,
            anonymous_nominals: Arc::from([]),
        }
    }

    pub(crate) fn with_anonymous_nominals(
        mut self,
        anonymous_nominals: Arc<[DurableAnonymousNominal]>,
    ) -> Self {
        self.anonymous_nominals = anonymous_nominals;
        self
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        EvaluatedSemanticConst,
        SemanticDeclarationDependency,
        Arc<[DurableAnonymousNominal]>,
    ) {
        (self.value, self.dependency, self.anonymous_nominals)
    }
}

/// Canonical semantic services needed by durable comptime entry points.
///
/// Implementations live beside the query authorities. This facade is an
/// operation boundary, not an evaluator: neither trait accepts an instruction
/// reference, instruction data, or callback capable of evaluating a child.
pub(crate) trait DurableComptimeSemanticAuthority {
    fn check_canceled(&self) -> Result<(), QueryAbort>;

    /// Resolve one declaration-owned type syntax through the canonical AIR
    /// structured resolver. The program key selects one registered owning
    /// arena, symbol table, and module; callers cannot mix dense syntax refs
    /// with another program. This operation never walks expression
    /// instructions or evaluates a child.
    fn resolve_type_syntax(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Result<
        DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    >;

    /// Resolve syntax against the exact active type/value substitution view.
    /// The program key remains the sole arena and symbol authority; callers
    /// cannot pair a syntax reference with an independently supplied arena.
    #[allow(dead_code)] // consumed by the canonical durable AIR host
    fn resolve_type_syntax_with_substitutions(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: &[(Arc<str>, DurableType)],
        value_substitutions: &[(Arc<str>, DurableConstValue)],
    ) -> Result<
        DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    >;

    fn begin_structured_type(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: Vec<(Arc<str>, DurableType)>,
        value_substitutions: Vec<(Arc<str>, DurableConstValue)>,
    ) -> Result<
        DurableStructuredTypePoll,
        DurableStructuredTypeBeginError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn resume_structured_type(
        &mut self,
        job: DurableStructuredTypeJob,
        reduced: rue_air::SemanticProviderResult<
            Option<rue_air::SemanticComptimeCallResult<DurableType, DurableConstValue>>,
            QueryAbort,
            SemanticNucleusFailure,
        >,
    ) -> Result<
        DurableStructuredTypePoll,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    >;

    fn begin_comptime_call_admission(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    /// Begin admission from the exact stable callable head supplied by a
    /// structured-type continuation.  Unlike the spelling-based operation,
    /// this operation must not rediscover a declaration through module/name
    /// lookup: the implementation reconstructs the canonical syntax candidate
    /// for the stable key with discriminator zero, then verifies the projected
    /// identity key. A stable key does not carry duplicate-discriminator
    /// information, so this operation must not claim to preserve duplicate
    /// candidates; it never infers identity from a spelling.
    #[allow(dead_code)] // consumed by the canonical structured-frame adapter
    fn begin_comptime_call_admission_for_key(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        head: &crate::StableDefinitionKey,
    ) -> Result<
        DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn finish_comptime_call_admission(
        &self,
        start: DurableComptimeCallableAdmissionStart,
        argument_modes: &[crate::durable_semantics::DurableParameterMode],
    ) -> Result<
        DurableComptimeCallableAdmission,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    /// Resolve a named const, module binding, callable, or nominal in the
    /// canonical order used by durable identifier evaluation.
    fn resolve_named_value(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<DurableComptimeNamedValueProjection>,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn resolve_module_member(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        member: &str,
    ) -> Result<
        DurableComptimeNamedValueProjection,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    >;

    fn resolve_import(
        &self,
        site: &DurableImportSite,
    ) -> Result<DurableImportResolution, QueryAbort>;

    /// Resolve an import occurrence through the exact registered program
    /// authority. The semantic site already carries the owning program and
    /// source-order occurrence; implementations must not consult an ambient
    /// occurrence map.
    fn resolve_keyed_import(
        &self,
        site: &rue_air::ComptimeSite<crate::body_query::DurableComptimeProgramKey>,
        specifier: &str,
    ) -> Result<DurableImportResolution, DurableComptimeKeyedImportError>;

    /// Resolve a target intrinsic from semantic name/arity facts.  The
    /// authority owns the configured target and the diagnostic policy; no RIR
    /// instruction or argument callback crosses this boundary.
    fn resolve_target_intrinsic(
        &self,
        intrinsic: ComptimeTargetIntrinsic,
        argument_count: usize,
    ) -> Result<TargetEnumValue, rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>>;

    /// Resolve a target descriptor member through the canonical target
    /// authority, preserving the durable evaluator's exact value shape.
    fn resolve_target_enum_variant(
        &self,
        type_name: &str,
        variant: &str,
    ) -> Result<TargetEnumValue, rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>>;
}

#[allow(dead_code)] // activated by the canonical durable AIR host
pub(crate) trait DurableComptimeForeignCallAuthority {
    fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort>;
}

/// Complete compiler-side service set composed by the AIR host. Query roots
/// implement this beside their semantic and foreign-call authorities; the
/// host borrows the service facade and owns no provider or registry itself.
#[allow(dead_code)] // consumed by the canonical durable AIR host
pub(crate) trait DurableComptimeHostAuthority:
    DurableComptimeSemanticAuthority + DurableComptimeForeignCallAuthority
{
    fn durable_session(&self) -> &DurableComptimeSession;
    fn durable_session_mut(&mut self) -> &mut DurableComptimeSession;

    /// Test-only injection point for exercising the named array-length
    /// conversion boundary with a value the source language cannot spell.
    /// Production authorities leave this disabled.
    fn test_array_length_override(&self) -> Option<i128> {
        None
    }
}

pub(crate) struct DurableComptimeServices<'a, A: ?Sized> {
    authority: &'a mut A,
}

impl<'a, A: ?Sized> DurableComptimeServices<'a, A> {
    pub(crate) fn new(authority: &'a mut A) -> Self {
        Self { authority }
    }
}

impl<A: DurableComptimeSemanticAuthority + ?Sized> DurableComptimeServices<'_, A> {
    pub(super) fn check_canceled(&self) -> Result<(), QueryAbort> {
        self.authority.check_canceled()
    }

    pub(crate) fn resolve_type_syntax(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Result<
        DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    > {
        self.authority.resolve_type_syntax(program, syntax)
    }

    #[allow(dead_code)] // consumed by the canonical durable AIR host
    pub(crate) fn resolve_type_syntax_with_substitutions(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: &[(Arc<str>, DurableType)],
        value_substitutions: &[(Arc<str>, DurableConstValue)],
    ) -> Result<
        DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    > {
        self.authority.resolve_type_syntax_with_substitutions(
            program,
            syntax,
            type_substitutions,
            value_substitutions,
        )
    }

    pub(super) fn begin_structured_type(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: Vec<(Arc<str>, DurableType)>,
        value_substitutions: Vec<(Arc<str>, DurableConstValue)>,
    ) -> Result<
        DurableStructuredTypePoll,
        DurableStructuredTypeBeginError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority.begin_structured_type(
            program,
            syntax,
            type_substitutions,
            value_substitutions,
        )
    }

    pub(super) fn resume_structured_type(
        &mut self,
        job: DurableStructuredTypeJob,
        reduced: rue_air::SemanticProviderResult<
            Option<rue_air::SemanticComptimeCallResult<DurableType, DurableConstValue>>,
            QueryAbort,
            SemanticNucleusFailure,
        >,
    ) -> Result<
        DurableStructuredTypePoll,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            SemanticNucleusFailure,
            crate::StableDefinitionKey,
            Arc<str>,
        >,
    > {
        self.authority.resume_structured_type(job, reduced)
    }

    pub(super) fn begin_comptime_call_admission(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .begin_comptime_call_admission(accessing_source, module, name)
    }

    #[allow(dead_code)] // consumed by the canonical structured-frame adapter
    pub(crate) fn begin_comptime_call_admission_for_key(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        head: &crate::StableDefinitionKey,
    ) -> Result<
        DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .begin_comptime_call_admission_for_key(accessing_source, head)
    }

    pub(crate) fn finish_comptime_call_admission(
        &self,
        start: DurableComptimeCallableAdmissionStart,
        argument_modes: &[crate::durable_semantics::DurableParameterMode],
    ) -> Result<
        DurableComptimeCallableAdmission,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .finish_comptime_call_admission(start, argument_modes)
    }

    pub(super) fn resolve_named_value(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<DurableComptimeNamedValueProjection>,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .resolve_named_value(accessing_source, module, name)
    }

    pub(super) fn resolve_module_member(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        member: &str,
    ) -> Result<
        DurableComptimeNamedValueProjection,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        self.authority
            .resolve_module_member(accessing_source, module, member)
    }

    pub(super) fn resolve_target_intrinsic(
        &self,
        intrinsic: ComptimeTargetIntrinsic,
        argument_count: usize,
    ) -> Result<TargetEnumValue, rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>>
    {
        self.authority
            .resolve_target_intrinsic(intrinsic, argument_count)
    }

    pub(super) fn resolve_target_enum_variant(
        &self,
        type_name: &str,
        variant: &str,
    ) -> Result<TargetEnumValue, rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>>
    {
        self.authority
            .resolve_target_enum_variant(type_name, variant)
    }

    /// Finish admission for a structured-type call.  Structured syntax has
    /// already supplied the ordered arguments, so every argument is a value;
    /// the begin phase remains separate so its dependency can be observed
    /// before signature/arity policy is allowed to fail.
    #[allow(dead_code)] // consumed by the canonical structured-frame adapter
    pub(crate) fn finish_structured_comptime_call_admission(
        &self,
        start: DurableComptimeCallableAdmissionStart,
        argument_count: usize,
    ) -> Result<
        DurableComptimeCallableAdmission,
        rue_air::SemanticProviderError<QueryAbort, SemanticNucleusFailure>,
    > {
        let argument_modes = (0..argument_count)
            .map(|_| crate::durable_semantics::DurableParameterMode::Value)
            .collect::<Vec<_>>();
        self.finish_comptime_call_admission(start, &argument_modes)
    }

    /// Resolve an import against the registered program selected by `program`.
    /// This is the only evaluator-facing import path; occurrence metadata and
    /// declaration identity are paired atomically before querying imports.
    pub(crate) fn resolve_keyed_import(
        &self,
        site: &rue_air::ComptimeSite<crate::body_query::DurableComptimeProgramKey>,
        specifier: &str,
    ) -> Result<DurableImportResolution, DurableComptimeKeyedImportError> {
        if site.kind() != rue_air::ComptimeSiteKind::Import {
            return Err(DurableComptimeKeyedImportError::WrongSiteKind);
        }
        self.authority.resolve_keyed_import(site, specifier)
    }
}

impl<A: DurableComptimeHostAuthority + ?Sized> DurableComptimeServices<'_, A> {
    pub(super) fn durable_session(&self) -> &DurableComptimeSession {
        self.authority.durable_session()
    }

    pub(super) fn durable_session_mut(&mut self) -> &mut DurableComptimeSession {
        self.authority.durable_session_mut()
    }

    pub(super) fn test_array_length_override(&self) -> Option<i128> {
        self.authority.test_array_length_override()
    }
}

#[allow(dead_code)] // activated by the canonical durable AIR host
impl<A: DurableComptimeForeignCallAuthority + ?Sized> DurableComptimeServices<'_, A> {
    /// Probe only an already-published foreign fact or admit its owned body
    /// frame. The authority owns dependency observation and cancellation; this
    /// method never demands a child comptime query.
    #[allow(dead_code)] // activated by the canonical durable AIR host
    pub(crate) fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, DurableType)],
        value_arguments: &[(Arc<str>, DurableConstValue)],
    ) -> Result<ForeignComptimeCallLookup, QueryAbort> {
        self.authority
            .probe_comptime_call(producer, type_arguments, value_arguments)
    }

    /// Consume the pending package and perform exactly one raw foreign probe.
    /// The query slices are borrowed from the opaque bound call; lookup and
    /// lifecycle state cannot be reconstructed or retried by the caller.
    #[allow(dead_code)]
    pub(crate) fn probe_prepared_call(
        &self,
        pending: DurableComptimePendingCall,
    ) -> Result<DurableComptimeProbedCall, QueryAbort> {
        let query = pending.query_view();
        let lookup = self.authority.probe_comptime_call(
            pending.producer(),
            query.type_arguments(),
            query.value_arguments(),
        )?;
        Ok(DurableComptimeProbedCall { pending, lookup })
    }

    /// Probe one structured-type continuation exactly once. The caller keeps
    /// the AIR job for resume; this package owns only its copied keyed request
    /// until the session consumes the lookup, so it cannot be cross-paired.
    #[allow(dead_code)]
    pub(crate) fn probe_structured_type_call(
        &self,
        pending: DurableStructuredTypeValidatedCall,
    ) -> Result<DurableStructuredTypeProbedCall, QueryAbort> {
        let request = &pending.pending.request;
        let lookup = self.authority.probe_comptime_call(
            &request.head_key,
            &request.type_arguments,
            &request.value_arguments,
        )?;
        Ok(DurableStructuredTypeProbedCall { pending, lookup })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_value_kernel_is_ordered_and_short_circuits() {
        let mut all_missing = Vec::new();
        assert_eq!(
            resolve_named_value_in_order(|kind| {
                all_missing.push(kind);
                Ok::<Option<()>, ()>(None)
            })
            .unwrap(),
            None
        );
        assert_eq!(
            all_missing,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Function,
                DurableComptimeNamedValueKind::Struct,
                DurableComptimeNamedValueKind::Enum,
            ]
        );

        let mut early_success = Vec::new();
        assert_eq!(
            resolve_named_value_in_order(|kind| {
                early_success.push(kind);
                Ok::<Option<&str>, ()>(
                    (kind == DurableComptimeNamedValueKind::Struct).then_some("struct"),
                )
            })
            .unwrap(),
            Some("struct")
        );
        assert_eq!(
            early_success,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Function,
                DurableComptimeNamedValueKind::Struct,
            ]
        );

        let mut const_failure = Vec::new();
        assert_eq!(
            resolve_named_value_in_order(|kind| {
                const_failure.push(kind);
                if kind == DurableComptimeNamedValueKind::Const {
                    Err::<Option<()>, _>("const failure")
                } else {
                    Ok(None)
                }
            }),
            Err("const failure")
        );
        assert_eq!(const_failure, vec![DurableComptimeNamedValueKind::Const]);

        let mut middle_failure = Vec::new();
        assert_eq!(
            resolve_named_value_in_order(|kind| {
                middle_failure.push(kind);
                if kind == DurableComptimeNamedValueKind::Struct {
                    Err::<Option<()>, _>("struct failure")
                } else {
                    Ok(None)
                }
            }),
            Err("struct failure")
        );
        assert_eq!(
            middle_failure,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Function,
                DurableComptimeNamedValueKind::Struct,
            ]
        );

        let mut module_missing = Vec::new();
        assert_eq!(
            resolve_module_member_in_order(|kind| {
                module_missing.push(kind);
                Ok::<Option<()>, ()>(None)
            })
            .unwrap(),
            None
        );
        assert_eq!(
            module_missing,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Struct,
                DurableComptimeNamedValueKind::Enum,
                DurableComptimeNamedValueKind::Function,
            ]
        );

        let mut module_success = Vec::new();
        assert_eq!(
            resolve_module_member_in_order(|kind| {
                module_success.push(kind);
                Ok::<Option<&str>, ()>(
                    (kind == DurableComptimeNamedValueKind::Enum).then_some("enum"),
                )
            })
            .unwrap(),
            Some("enum")
        );
        assert_eq!(
            module_success,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Struct,
                DurableComptimeNamedValueKind::Enum,
            ]
        );

        let mut module_failure = Vec::new();
        assert_eq!(
            resolve_module_member_in_order(|kind| {
                module_failure.push(kind);
                if kind == DurableComptimeNamedValueKind::Struct {
                    Err::<Option<()>, _>("module struct failure")
                } else {
                    Ok(None)
                }
            }),
            Err("module struct failure")
        );
        assert_eq!(
            module_failure,
            vec![
                DurableComptimeNamedValueKind::Const,
                DurableComptimeNamedValueKind::Struct,
            ]
        );
    }
}
