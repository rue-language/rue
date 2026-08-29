use super::*;
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum BodyTransactionRequestFailure {
    Query(QueryAbort),
    DeferredAnonymousProducers(Arc<[crate::FunctionInstanceKey]>),
    /// An anonymous producer this body depends on committed a deterministic
    /// semantic failure. The dependent body cannot be built and must fail
    /// closed; the failure is never retried or reclassified as cancellation.
    ProducerFailed(Box<crate::semantic_query_nucleus::SemanticNucleusFailure>),
    /// One exact trusted `Option(payload)` specialization failed before body
    /// analysis. No partial registry or body terminal is published.
    WellKnownOptionResolution(WellKnownOptionResolutionFailure),
}

pub(crate) struct BodyClosureRequest {
    pub(crate) terminal: Arc<rue_query::QueryTerminal<crate::body_query::BodyClosureOutput>>,
    /// Keyed by the ADR-0074 structural key digest rather than the rendered
    /// body identity, so building and reading this projection never formats a
    /// body transaction's name.
    pub(super) body_executions: BTreeMap<rue_query::NodeIdentity, rue_query::RequestExecution>,
    pub(super) retained_before: BTreeSet<rue_query::NodeIdentity>,
    pub(super) work: Vec<(Arc<str>, u64)>,
    pub(crate) candidate_body_plan_work: crate::CandidateBodyPlanWork,
    pub(crate) candidate_body_materialization_work: crate::CandidateBodyPlanWork,
}

impl BodyClosureRequest {
    pub(crate) fn execution_for(
        &self,
        key: &crate::body_query::BodyQueryKey,
    ) -> rue_query::RequestExecution {
        self.body_executions
            .get(&rue_query::NodeIdentity::from_typed_key(
                "compiler.body-transaction",
                key,
            ))
            .copied()
            .unwrap_or(rue_query::RequestExecution::Reused)
    }

    pub(crate) fn was_retained(&self, key: &crate::body_query::BodyQueryKey) -> bool {
        self.retained_before
            .contains(&rue_query::NodeIdentity::from_typed_key(
                "compiler.body-transaction",
                key,
            ))
    }

    /// Accrue request-local database-owned reachability scheduling work into
    /// the canonical semantic request. Query work is already reduced by stable
    /// identity, so this projection is deterministic across worker schedules.
    pub(crate) fn accrue_reachability_work(&self, target: &mut rue_air::BodyAnalysisWork) {
        for (identity, amount) in &self.work {
            let amount = usize::try_from(*amount).unwrap_or(usize::MAX);
            let counter = match identity.as_ref() {
                "reachability.frontier.scans" => &mut target.reachability_frontier_scans,
                "reachability.frontier.scan-keys" => &mut target.reachability_frontier_scan_keys,
                "reachability.frontier.batches" => &mut target.reachability_frontier_batches,
                "reachability.frontier.keys" => &mut target.reachability_frontier_keys,
                "reachability.frontier.width-1" => &mut target.reachability_frontier_width_one,
                "reachability.frontier.width-2-3" => {
                    &mut target.reachability_frontier_width_two_to_three
                }
                "reachability.frontier.width-4-7" => {
                    &mut target.reachability_frontier_width_four_to_seven
                }
                "reachability.frontier.width-8-plus" => {
                    &mut target.reachability_frontier_width_eight_or_more
                }
                "reachability.transactions.prefetched" => {
                    &mut target.reachability_transactions_prefetched
                }
                "reachability.transactions.serial" => &mut target.reachability_transactions_serial,
                _ => continue,
            };
            *counter = counter.saturating_add(amount);
        }
    }

    pub(crate) fn accrue_candidate_body_plan_work(
        &self,
        target: &mut crate::CanonicalSemanticWork,
    ) {
        target.candidate_body_plan_construction = self.candidate_body_plan_work;
        target.candidate_body_plan_materialization = self.candidate_body_materialization_work;
    }
}

// NodeIdentity's cached presentation text is the only interior-mutability
// member; equality and ordering use the immutable family/hash/witness identity.
#[allow(clippy::mutable_key_type)]
pub(super) fn candidate_body_plan_work_from_nested(
    attempts: &[rue_query::NestedQueryAttempt],
) -> (crate::CandidateBodyPlanWork, crate::CandidateBodyPlanWork) {
    pub(super) fn priority(execution: RequestExecution) -> u8 {
        match execution {
            RequestExecution::Computed => 4,
            RequestExecution::Joined => 3,
            RequestExecution::Reused => 2,
            RequestExecution::Aborted => 1,
        }
    }
    pub(super) fn reduce_family<'a>(
        attempts: &'a [rue_query::NestedQueryAttempt],
        family: &str,
    ) -> Vec<&'a rue_query::NestedQueryAttempt> {
        let mut selected = BTreeMap::new();
        for attempt in attempts.iter().filter(|attempt| {
            attempt.node().family() == family
                && attempt.terminal_kind() == Some(QueryTerminalKind::Success)
        }) {
            selected
                .entry(attempt.node().clone())
                .and_modify(|current: &mut &rue_query::NestedQueryAttempt| {
                    if priority(attempt.execution()) > priority(current.execution()) {
                        *current = attempt;
                    }
                })
                .or_insert(attempt);
        }
        selected.into_values().collect()
    }
    pub(super) fn count(
        attempts: Vec<&rue_query::NestedQueryAttempt>,
        prefix: &str,
    ) -> crate::CandidateBodyPlanWork {
        let mut result = crate::CandidateBodyPlanWork::default();
        for attempt in attempts {
            match attempt.execution() {
                RequestExecution::Computed => {
                    for (identity, amount) in attempt.work() {
                        if identity.as_ref() == format!("{prefix}.plans") {
                            result.computed = result
                                .computed
                                .saturating_add(usize::try_from(*amount).unwrap_or(usize::MAX));
                        } else if identity.as_ref() == format!("{prefix}.instructions") {
                            result.instructions_produced = result
                                .instructions_produced
                                .saturating_add(usize::try_from(*amount).unwrap_or(usize::MAX));
                        } else if identity.as_ref() == format!("{prefix}.payload_words") {
                            result.payload_words_produced = result
                                .payload_words_produced
                                .saturating_add(usize::try_from(*amount).unwrap_or(usize::MAX));
                        }
                    }
                }
                RequestExecution::Reused | RequestExecution::Joined => result.reused += 1,
                RequestExecution::Aborted => {}
            }
        }
        result
    }
    (
        count(
            reduce_family(attempts, "compiler.declaration-body-plan-artifacts"),
            "candidate_body_plan.construction",
        ),
        count(
            reduce_family(attempts, "compiler.body-transaction"),
            "candidate_body_plan.materialization",
        ),
    )
}

#[allow(clippy::mutable_key_type)]
pub(super) fn successful_nested_nodes(
    attempts: &[rue_query::NestedQueryAttempt],
    family: &str,
) -> BTreeSet<rue_query::NodeIdentity> {
    attempts
        .iter()
        .filter(|attempt| {
            attempt.node().family() == family
                && attempt.terminal_kind() == Some(QueryTerminalKind::Success)
        })
        .map(|attempt| attempt.node().clone())
        .collect()
}

/// Recover exact reuse counts from a retained closure publication. Publication
/// deliberately retains only the compact closure result on a warm request, so
/// its nested-attempt ledger is empty. Candidate construction is deduplicated
/// by full identity because generic specializations share one candidate plan;
/// materialization remains per body instance.
#[allow(clippy::mutable_key_type)]
pub(super) fn candidate_body_plan_work_from_retained_closure(
    closure_terminal: &Arc<rue_query::QueryTerminal<crate::body_query::BodyClosureOutput>>,
    represented_construction: &BTreeSet<rue_query::NodeIdentity>,
    represented_materialization: &BTreeSet<rue_query::NodeIdentity>,
) -> (crate::CandidateBodyPlanWork, crate::CandidateBodyPlanWork) {
    let mut construction = crate::CandidateBodyPlanWork::default();
    let mut materialization = crate::CandidateBodyPlanWork::default();
    let rue_query::QueryOutcome::Success(closure) = closure_terminal.outcome() else {
        return (construction, materialization);
    };
    let mut candidates = BTreeSet::<rue_query::NodeIdentity>::new();
    let mut bodies = BTreeSet::<rue_query::NodeIdentity>::new();
    for body in closure.bodies.iter() {
        let rue_query::QueryOutcome::Success(bundle) = body.bundle.outcome() else {
            continue;
        };
        if matches!(
            bundle.transaction,
            crate::body_query::BodyTransaction::Success { .. }
        ) {
            if let Some(definition) = body_source_definition_key(&body.key.instance) {
                if let Some(candidate) = declaration_candidate_for_stable_key(definition) {
                    candidates.insert(rue_query::NodeIdentity::from_typed_key(
                        Arc::from("compiler.declaration-body-plan-artifacts"),
                        &DeclarationBodyPlanQueryKey(candidate),
                    ));
                }
            }
            // BodyAnalysisBundle forwards the successful body-transaction
            // terminal's registered work. Presence of this plan item is the
            // retained publication authority; failed and zero-work bundles
            // cannot manufacture a reuse count from their semantic value.
            if body.bundle.work().iter().any(|(identity, amount)| {
                identity.as_ref() == "candidate_body_plan.materialization.plans" && *amount > 0
            }) {
                bodies.insert(rue_query::NodeIdentity::from_typed_key(
                    Arc::from("compiler.body-transaction"),
                    &body.key,
                ));
            }
        }
    }
    construction.reused = candidates.difference(represented_construction).count();
    materialization.reused = bodies.difference(represented_materialization).count();
    (construction, materialization)
}

pub(crate) use crate::body_query::WellKnownOptionResolutionFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WellKnownDependencyAbortClass {
    Incomplete,
    Propagate,
}

pub(super) fn classify_well_known_dependency_abort(
    abort: &QueryAbort,
) -> WellKnownDependencyAbortClass {
    match abort {
        QueryAbort::Canceled | QueryAbort::MissingInput(_) => {
            WellKnownDependencyAbortClass::Incomplete
        }
        QueryAbort::Cycle(_) | QueryAbort::ForeignRuntime | QueryAbort::UnpublishedRevision(_) => {
            WellKnownDependencyAbortClass::Propagate
        }
    }
}

/// Whether a committed semantic-nucleus failure is an internal-error
/// (E9000-class) diagnostic. The anonymous-anchor transport invariant violation
/// (RUE-1089) surfaces exactly as `Diagnostic(InternalError(_))`. Such a
/// committed failure is a corrupt-input fact and must fail closed, never be
/// downgraded to a retryable abort or rescued by structural recomputation.
pub(crate) fn semantic_nucleus_failure_is_internal_error(
    failure: &crate::semantic_query_nucleus::SemanticNucleusFailure,
) -> bool {
    use crate::semantic_query_nucleus::SemanticNucleusFailure as F;
    if let F::DuplicateDeclarations(failures) = failure {
        return failures
            .iter()
            .any(|failure| matches!(failure.kind, rue_error::ErrorKind::InternalError(_)));
    }
    let kind = match failure {
        F::Diagnostic(kind)
        | F::DiagnosticAtParameter { kind, .. }
        | F::DiagnosticAtDeclaration { kind, .. }
        | F::DuplicateDeclaration { kind, .. }
        | F::DiagnosticAtProducerRange { kind, .. }
        | F::OwnershipGate { kind, .. }
        | F::DiagnosticWithHelp { kind, .. }
        | F::DiagnosticWithNote { kind, .. } => kind,
        F::Shell(_)
        | F::DuplicateDeclarations(_)
        | F::ForeignSignatureConflict(_)
        | F::Syntax(_)
        | F::Resolution(_)
        | F::SignatureReentry { .. }
        | F::Cycle(_) => {
            return false;
        }
    };
    matches!(kind, rue_error::ErrorKind::InternalError(_))
}

/// The producer functions named by an instance key's statically encoded
/// anonymous-nominal graph, in traversal order.
///
/// Body reachability needs this list twice for the same key: once to schedule
/// the producers the key names, and again on every frontier scan to ask whether
/// those producers have all been visited. Walking the key each time made a scan
/// cost the whole anonymous graph of every pending body, re-derived per round,
/// even though the graph is statically encoded and cannot move within a
/// request. The closure is a pure function of the key, so one
/// evaluation-scoped memo serves both phases (RUE-1557).
///
/// Duplicates are preserved rather than collapsed into a set: two identities
/// can name the same producer, and the scheduling phase is written against the
/// exact sequence the traversal emits.
pub(super) fn instance_producer_closure(
    context: &rue_query::QueryContext,
    memo: &mut AHashMap<Arc<crate::FunctionInstanceKey>, Arc<[Arc<crate::FunctionInstanceKey>]>>,
    seen: &mut AHashSet<*const crate::AnonymousNominalKey>,
    instance: &Arc<crate::FunctionInstanceKey>,
) -> Arc<[Arc<crate::FunctionInstanceKey>]> {
    if let Some(producers) = memo.get(instance) {
        return producers.clone();
    }
    let mut producers = Vec::new();
    visit_instance_anonymous_nominals(instance.as_ref(), seen, |identity| {
        if let crate::StableProducerId::Function(producer) = &identity.producer {
            producers.push(Arc::new(producer.as_ref().clone()));
        }
    });
    context.record_work(rue_query::WorkItem::new(
        "reachability.anonymous.closure-walks",
        1,
    ));
    memo.entry(instance.clone())
        .or_insert_with(|| Arc::from(producers))
        .clone()
}

/// The trusted-toolchain demand of one body, answered from this reachability
/// evaluation's cache once it has been observed.
///
/// Reachability asks for the same body's demand more than once. The prefetch
/// batch reads the frontier window, and if any demanded module is absent the
/// parking sweep then walks the whole frontier — which contains that window —
/// to union what every remaining body still needs. The demand is a pure
/// function of the key at this revision, so the repeat is served from the cache
/// and each body is queried at most once per request (RUE-1562).
///
/// Only repeats are skipped, so the set of dependency edges the evaluation
/// records is unchanged: a body whose demand has not been observed yet is still
/// queried here, and the first observation is what registers the edge.
pub(super) fn observe_body_toolchain_demand(
    context: &rue_query::QueryContext,
    demands: &QueryFamily<crate::body_query::BodyQueryKey, crate::BodyToolchainDemand>,
    cache: &mut AHashMap<Arc<crate::FunctionInstanceKey>, crate::BodyToolchainDemand>,
    configuration: &crate::semantic_query_nucleus::SemanticQueryConfiguration,
    instance: &Arc<crate::FunctionInstanceKey>,
) -> Result<crate::BodyToolchainDemand, QueryAbort> {
    if let Some(demand) = cache.get(instance) {
        return Ok(demand.clone());
    }
    let terminal = context.query_registered(
        demands,
        crate::body_query::BodyQueryKey::new(instance.as_ref().clone(), configuration.clone()),
    )?;
    let rue_query::QueryOutcome::Success(demand) = terminal.outcome() else {
        unreachable!("BodyToolchainDemands publishes typed values")
    };
    context.record_work(rue_query::WorkItem::new(
        "reachability.toolchain-demand.queries",
        1,
    ));
    Ok(cache
        .entry(instance.clone())
        .or_insert_with(|| demand.clone())
        .clone())
}

pub(super) fn visit_instance_anonymous_nominals<'a>(
    function: &'a crate::FunctionInstanceKey,
    seen: &mut AHashSet<*const crate::AnonymousNominalKey>,
    mut visit: impl FnMut(&'a crate::AnonymousNominalKey),
) {
    pub(super) fn arguments<'a, F: FnMut(&'a crate::AnonymousNominalKey)>(
        arguments: &'a crate::CanonicalArguments,
        seen: &mut AHashSet<*const crate::AnonymousNominalKey>,
        visit: &mut F,
    ) {
        for ty in arguments.types.iter() {
            instance_type(ty, seen, visit);
        }
        for value in arguments.values.iter() {
            match value {
                crate::CanonicalArgumentValue::Type(ty) => instance_type(ty, seen, visit),
                crate::CanonicalArgumentValue::Function(function) => {
                    instance_function(function, seen, visit);
                }
                _ => {}
            }
        }
    }

    pub(super) fn anonymous<'a, F: FnMut(&'a crate::AnonymousNominalKey)>(
        identity: &'a crate::AnonymousNominalKey,
        seen: &mut AHashSet<*const crate::AnonymousNominalKey>,
        visit: &mut F,
    ) {
        // Canonical argument slices are shared through `Arc`, so one instance
        // key can reach the same nested identity through several paths. Track
        // those shared objects by address: structural equality is unnecessary
        // for traversal, and a reusable hashed scratch set avoids one tree
        // allocation per visited anonymous identity while answering the repeat
        // check in constant time, so a wide or deeply nested key does not turn
        // the traversal quadratic in the identities it reaches.
        let identity_pointer = std::ptr::from_ref(identity);
        if !seen.insert(identity_pointer) {
            return;
        }
        visit(identity);
        // The producer is the key's whole reach: the comptime arguments it was
        // minted under live inside that producer's specialization (RUE-1699).
        if let crate::StableProducerId::Function(function) = &identity.producer {
            instance_function(function, seen, visit);
        }
    }

    pub(super) fn instance_type<'a, F: FnMut(&'a crate::AnonymousNominalKey)>(
        ty: &'a crate::TypeInstanceKey,
        seen: &mut AHashSet<*const crate::AnonymousNominalKey>,
        visit: &mut F,
    ) {
        match ty {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(identity)) => {
                anonymous(identity, seen, visit);
            }
            crate::TypeInstanceKey::Array { element, .. }
            | crate::TypeInstanceKey::Slice { element, .. }
            | crate::TypeInstanceKey::PtrConst(element)
            | crate::TypeInstanceKey::PtrMut(element) => instance_type(element, seen, visit),
            _ => {}
        }
    }

    pub(super) fn instance_function<'a, F: FnMut(&'a crate::AnonymousNominalKey)>(
        function: &'a crate::FunctionInstanceKey,
        seen: &mut AHashSet<*const crate::AnonymousNominalKey>,
        visit: &mut F,
    ) {
        match function {
            crate::FunctionInstanceKey::Definition(_) => {}
            crate::FunctionInstanceKey::Specialization {
                base,
                arguments: values,
            } => {
                instance_function(base, seen, visit);
                arguments(values, seen, visit);
            }
            crate::FunctionInstanceKey::AnonymousMember { owner, .. }
            | crate::FunctionInstanceKey::DropGlue(owner) => instance_type(owner, seen, visit),
        }
    }

    seen.clear();
    instance_function(function, seen, &mut visit);
}

pub(crate) fn collect_instance_anonymous_nominals(
    function: &crate::FunctionInstanceKey,
) -> BTreeSet<crate::AnonymousNominalKey> {
    let mut output = BTreeSet::new();
    let mut seen = AHashSet::new();
    visit_instance_anonymous_nominals(function, &mut seen, |identity| {
        output.insert(identity.clone());
    });
    output
}

pub(super) fn schedule_body_instance<V>(
    pending: &mut BTreeMap<Arc<crate::FunctionInstanceKey>, usize>,
    ready: &mut BTreeMap<Arc<crate::FunctionInstanceKey>, usize>,
    prefetched: &mut VecDeque<(Arc<crate::FunctionInstanceKey>, usize, V)>,
    visited: &BTreeSet<Arc<crate::FunctionInstanceKey>>,
    instance: &crate::FunctionInstanceKey,
    depth: usize,
) -> Option<(Arc<crate::FunctionInstanceKey>, bool)> {
    if visited.contains(instance) {
        return None;
    }

    if let Some(existing_depth) = ready.get_mut(instance) {
        let changed = depth < *existing_depth;
        *existing_depth = (*existing_depth).min(depth);
        let instance = ready
            .get_key_value(instance)
            .expect("the ready instance remains present after its depth update")
            .0
            .clone();
        return Some((instance, changed));
    }
    if let Some((prefetched_instance, existing_depth, _)) = prefetched
        .iter_mut()
        .find(|(prefetched_instance, _, _)| prefetched_instance.as_ref() == instance)
    {
        let changed = depth < *existing_depth;
        *existing_depth = (*existing_depth).min(depth);
        return Some((prefetched_instance.clone(), changed));
    }
    if let Some(existing_depth) = pending.get_mut(instance) {
        let changed = depth < *existing_depth;
        *existing_depth = (*existing_depth).min(depth);
        let instance = pending
            .get_key_value(instance)
            .expect("the pending instance remains present after its depth update")
            .0
            .clone();
        return Some((instance, changed));
    }

    let instance = Arc::new(instance.clone());
    pending.insert(instance.clone(), depth);
    Some((instance, true))
}

/// Reachability starts at the root body, while a comptime call depth starts at
/// its first application. The scheduler records the root at depth zero and
/// increments for every specialization edge, so the first specialization is
/// scheduler-depth one but comptime-depth zero.
#[inline]
pub(super) fn comptime_specialization_depth(scheduler_depth: usize) -> usize {
    scheduler_depth.saturating_sub(1)
}

pub(crate) fn durable_type_from_instance_key(
    value: &crate::TypeInstanceKey,
) -> Option<crate::durable_semantics::DurableType> {
    crate::durable_comptime::durable_type_from_instance_key(value)
}

pub(crate) fn durable_value_from_argument(
    value: &crate::CanonicalArgumentValue,
) -> Option<crate::durable_semantics::DurableConstValue> {
    use crate::CanonicalArgumentValue as V;
    use crate::durable_semantics::DurableConstValue as D;
    Some(match value {
        V::Integer(value) => D::Integer(*value),
        V::Bool(value) => D::Bool(*value),
        V::Type(value) => D::Type(durable_type_from_instance_key(value)?),
        V::Function(value) => {
            let crate::FunctionInstanceKey::Definition(key) = value.as_ref() else {
                return None;
            };
            D::Function(key.clone())
        }
        V::Unit => D::Unit,
        V::String(value) => D::String(value.clone()),
    })
}

pub(super) fn comptime_call_for_anonymous_function(
    producer: &crate::semantic_query_nucleus::DeclarationSemanticQueryKey,
    function: &crate::FunctionInstanceKey,
    shell: &crate::declaration_candidate::DeclarationShellFact,
    signature: &crate::semantic_query_nucleus::ResolvedDeclarationSignature,
    exact_type_syntax: &rue_air::DurableCallableTypeSyntax,
) -> Option<crate::semantic_query_nucleus::ComptimeCallQueryKey> {
    // A dependent runtime result also projects to `ComptimeType` until its
    // arguments are known (for example `[i32; N]`). Only a function whose
    // declared result is literally `type` is an anonymous type constructor.
    let Some(rue_rir::RirTypeSyntaxNode::Named(symbol)) =
        exact_type_syntax.syntax.node(exact_type_syntax.result)
    else {
        return None;
    };
    if exact_type_syntax
        .syntax
        .symbol(*symbol)
        .is_none_or(|name| name.as_ref() != "type")
    {
        return None;
    }
    let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
        parameters,
        result: crate::durable_semantics::DurableType::ComptimeType,
        is_extern: false,
        ..
    } = &signature.signature
    else {
        return None;
    };
    let expected = crate::semantic_query_nucleus::direct_identity(shell)?.key;
    let arguments = match function {
        crate::FunctionInstanceKey::Definition(definition) if *definition == expected => {
            crate::CanonicalArguments::default()
        }
        crate::FunctionInstanceKey::Specialization { base, arguments }
            if matches!(
                base.as_ref(),
                crate::FunctionInstanceKey::Definition(definition) if *definition == expected
            ) =>
        {
            arguments.clone()
        }
        _ => return None,
    };
    if shell.parameters.len() != parameters.len()
        || shell
            .parameters
            .iter()
            .any(|parameter| !parameter.is_comptime)
    {
        return None;
    }
    let mut type_arguments = arguments.types.iter();
    let mut value_arguments = arguments.values.iter();
    let mut types = Vec::new();
    let mut values = Vec::new();
    for (header, parameter) in shell.parameters.iter().zip(parameters.iter()) {
        if parameter.ty == crate::durable_semantics::DurableType::ComptimeType
            && let Some(value) = type_arguments.next()
        {
            types.push((header.name.clone(), durable_type_from_instance_key(value)?));
        } else {
            values.push((
                header.name.clone(),
                durable_value_from_argument(value_arguments.next()?)?,
            ));
        }
    }
    if type_arguments.next().is_some() || value_arguments.next().is_some() {
        return None;
    }
    Some(crate::semantic_query_nucleus::ComptimeCallQueryKey {
        declaration: producer.clone(),
        type_arguments: types.into(),
        value_arguments: values.into(),
    })
}

pub(super) fn collect_anonymous_nominal_type_dependencies(
    ty: &crate::durable_semantics::DurableType,
    output: &mut BTreeSet<crate::AnonymousNominalKey>,
) {
    use crate::durable_semantics::DurableType as T;
    match ty {
        T::AnonymousNominal(identity) => {
            output.insert(identity.clone());
        }
        T::Array { element, .. }
        | T::Slice { element, .. }
        | T::PtrConst(element)
        | T::PtrMut(element) => collect_anonymous_nominal_type_dependencies(element, output),
        _ => {}
    }
}

pub(super) fn body_type_instance(
    ty: &rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>,
) -> crate::TypeInstanceKey {
    use rue_air::SemanticImportType as T;
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
        T::BuiltinNominal { name, kind } => crate::TypeInstanceKey::BuiltinNominal {
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
            element: Node::new(body_type_instance(element)),
            len: *len,
        },
        T::Slice { element, name } => crate::TypeInstanceKey::Slice {
            element: Node::new(body_type_instance(element)),
            name: name.clone(),
        },
        T::PtrConst(element) => {
            crate::TypeInstanceKey::PtrConst(Node::new(body_type_instance(element)))
        }
        T::PtrMut(element) => {
            crate::TypeInstanceKey::PtrMut(Node::new(body_type_instance(element)))
        }
        T::Module(module) => crate::TypeInstanceKey::Module(module.clone()),
        T::GenericParameter(index) => crate::TypeInstanceKey::GenericParameter(*index),
    }
}

pub(super) fn collect_body_type_reference(
    ty: &rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>,
    references: &mut BTreeSet<crate::body_query::BodyReference>,
) {
    references.insert(crate::body_query::BodyReference::Type(body_type_instance(
        ty,
    )));
    use rue_air::SemanticImportType as T;
    match ty {
        T::Array { element, .. }
        | T::Slice { element, .. }
        | T::PtrConst(element)
        | T::PtrMut(element) => collect_body_type_reference(element, references),
        _ => {}
    }
}

/// Publish the exact identity-bearing dependencies already observed in a
/// provider-produced semantic body. This traverses the canonical output only;
/// it never repeats lookup or semantic queries after analysis.
pub(super) fn collect_published_body_references(
    body: &rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
    references: &mut BTreeSet<crate::body_query::BodyReference>,
) {
    use rue_air::SemanticBodyInstDependency as D;
    let collect_drop_obligation =
        |value: rue_air::SemanticBodyRef,
         references: &mut BTreeSet<crate::body_query::BodyReference>| {
            if let Some(value) = body.instructions.get(value as usize) {
                references.insert(crate::body_query::BodyReference::DropGlue(
                    body_type_instance(&value.ty),
                ));
            }
        };
    collect_body_type_reference(&body.return_type, references);
    for instruction in body.instructions.iter() {
        collect_body_type_reference(&instruction.ty, references);
        use rue_air::SemanticBodyInstData as I;
        match &instruction.data {
            // These are precisely the ownership sites from which CFG cleanup
            // elaboration can emit an implicit destroy: a live local, an
            // overwritten local/parameter/place, or a discarded statement
            // result. Publishing their value types here keeps DropGlue rooted
            // in the reached body that owns the obligation without duplicating
            // CFG's path-sensitive drop elaboration.
            I::Alloc { init: value, .. }
            | I::Store { value, .. }
            | I::ParamStore { value, .. }
            | I::PlaceWrite { value, .. }
            | I::Drop { value } => collect_drop_obligation(*value, references),
            I::Block { statements, .. } => {
                for &statement in statements.iter() {
                    collect_drop_obligation(statement, references);
                }
            }
            _ => {}
        }
        instruction
            .data
            .visit_dependencies(&mut |dependency| match dependency {
                D::Definition(definition) => {
                    references.insert(crate::body_query::BodyReference::Definition(
                        definition.clone(),
                    ));
                }
                D::Nominal(nominal) => {
                    references.insert(crate::body_query::BodyReference::Type(
                        crate::TypeInstanceKey::Nominal(nominal.clone()),
                    ));
                }
                D::Function(function) => {
                    references.insert(crate::body_query::BodyReference::Callable(function.clone()));
                }
                D::Type(ty) => collect_body_type_reference(ty, references),
                D::Instruction(_) | D::Place(_) | D::String(_) => {}
            });
    }
    for place in body.places.iter() {
        collect_body_type_reference(&place.base_type, references);
        for projection in place.projections.iter() {
            match projection {
                rue_air::SemanticBodyProjection::Field { struct_key, .. } => {
                    references.insert(crate::body_query::BodyReference::Type(
                        crate::TypeInstanceKey::Nominal(struct_key.clone()),
                    ));
                }
                rue_air::SemanticBodyProjection::Index { array_type, .. } => {
                    collect_body_type_reference(array_type, references);
                }
            }
        }
    }
    for (_, ty) in body.param_drops.iter() {
        collect_body_type_reference(ty, references);
        references.insert(crate::body_query::BodyReference::DropGlue(
            body_type_instance(ty),
        ));
    }
}

pub(super) fn collect_anonymous_nominal_value_dependencies(
    value: &crate::durable_semantics::DurableConstValue,
    output: &mut BTreeSet<crate::AnonymousNominalKey>,
) {
    if let crate::durable_semantics::DurableConstValue::Type(ty) = value {
        collect_anonymous_nominal_type_dependencies(ty, output);
    }
}

pub(super) fn collect_durable_anonymous_nominal_dependencies(
    nominal: &crate::durable_semantics::DurableAnonymousNominal,
    output: &mut BTreeSet<crate::AnonymousNominalKey>,
) {
    use crate::durable_semantics::{
        DurableAnonymousMethodType as M, DurableAnonymousNominalShape as S,
    };
    for (_, ty) in nominal.type_captures.iter() {
        collect_anonymous_nominal_type_dependencies(ty, output);
    }
    for (_, value) in nominal.value_captures.iter() {
        collect_anonymous_nominal_value_dependencies(value, output);
    }
    match &nominal.shape {
        S::Struct { fields, methods } => {
            for (_, ty) in fields.iter() {
                collect_anonymous_nominal_type_dependencies(ty, output);
            }
            for method in methods.iter() {
                for (ty, _, _) in method.parameters.iter() {
                    if let M::Concrete(ty) = ty {
                        collect_anonymous_nominal_type_dependencies(ty, output);
                    }
                }
                if let M::Concrete(ty) = &method.result {
                    collect_anonymous_nominal_type_dependencies(ty, output);
                }
            }
        }
        S::Enum { variants, .. } => {
            for (_, fields) in variants.iter() {
                for ty in fields.iter() {
                    collect_anonymous_nominal_type_dependencies(ty, output);
                }
            }
        }
    }
}

pub(crate) fn semantic_candidate_import_occurrences(
    rir: &rue_rir::ValidatedRir,
    symbols: &[&str],
    mut checkpoint: impl FnMut() -> Result<(), QueryAbort>,
) -> Result<BTreeMap<rue_rir::InstRef, (u32, Arc<str>)>, QueryAbort> {
    let mut sites = Vec::new();
    for (instruction_ref, instruction) in rir.iter() {
        if instruction_ref.as_u32() % 64 == 0 {
            checkpoint()?;
        }
        let rue_rir::InstData::Intrinsic { name, args } = &instruction.data else {
            continue;
        };
        if symbols[name.into_usize()] != "import" {
            continue;
        }
        let arguments = rir.intrinsic_args(args);
        if arguments.len() != 1 {
            continue;
        }
        let argument = arguments
            .get(0)
            .expect("validated intrinsic argument index");
        let rue_rir::InstData::StringConst { content, .. } = &rir.get(argument).data else {
            continue;
        };
        sites.push((
            instruction.span.start,
            instruction.span.end,
            instruction_ref,
            Arc::<str>::from(symbols[content.into_usize()]),
        ));
    }
    sites.sort_by_key(|(start, end, instruction, _)| (*start, *end, *instruction));
    Ok(sites
        .into_iter()
        .enumerate()
        .map(|(occurrence, (_, _, instruction, specifier))| {
            (
                instruction,
                (
                    u32::try_from(occurrence)
                        .expect("validated RIR instruction count is bounded by u32"),
                    specifier,
                ),
            )
        })
        .collect())
}

pub(super) fn with_restored_state<S, O, R, Install, Operation, Restore>(
    state: &mut S,
    install: Install,
    operation: Operation,
    restore: Restore,
) -> R
where
    Install: FnOnce(&mut S) -> O,
    Operation: FnOnce(&mut S) -> R,
    Restore: FnOnce(&mut S, O),
{
    let old = install(state);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(state)));
    restore(state, old);
    match result {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Exact query authorities used by durable comptime services. Keeping this
/// adapter separate from the evaluator makes cancellation and import-site
/// resolution reusable by the AIR host without allowing it to inspect RIR.
pub(super) struct DurableComptimeRootAuthority<'db> {
    pub(super) provider: SemanticNucleusTypeProvider<'db>,
    pub(super) imports: QueryFamily<DeclarationImportQueryKey, DeclarationImportQueryValue>,
    pub(super) session: crate::durable_comptime::DurableComptimeSession,
    pub(super) foreign: DurableComptimeForeignQueryAuthority<'db>,
}

impl<'db> DurableComptimeRootAuthority<'db> {
    pub(super) fn finish_root(mut self) -> SemanticNucleusTypeProvider<'db> {
        let session_effects = self
            .session
            .drain_root_effects()
            .expect("durable AIR root must unwind lifecycle edges");
        self.provider.merge_comptime_effects(
            session_effects,
            &crate::durable_comptime::DurableComptimeApplicationPolicy::preserve(),
        );
        self.provider
    }
}

pub(super) fn evaluate_durable_comptime_root(
    authority: &mut DurableComptimeRootAuthority<'_>,
    frame: crate::durable_comptime::DurableComptimeConstFrame,
    mut env: rue_air::ComptimeEnv<
        crate::durable_comptime::EvaluatedSemanticConst,
        crate::durable_comptime::DurableComptimeType,
        crate::durable_comptime::DurableComptimeName,
        crate::durable_comptime::DurableComptimeFile,
        crate::durable_comptime::DurableComptimeIdentity,
    >,
) -> rue_air::ComptimeOutcome<
    crate::durable_comptime::EvaluatedSemanticConst,
    crate::durable_comptime::DurableComptimeHostFailure,
> {
    env.defining_file = frame.context.clone();
    env.expected_result = frame.expected_result.clone();
    let mut host = crate::durable_comptime::DurableComptimeHost::new(authority);
    rue_air::ComptimeEngine::new(&mut host).evaluate(frame, &mut env)
}

/// Classify one canonical AIR root terminal into the query family's two
/// result channels. Semantic failures remain values, retained query failures
/// remain query failures, and aborts retain the AIR abort channel.
pub(super) fn durable_comptime_root_result(
    outcome: rue_air::ComptimeOutcome<
        crate::durable_comptime::EvaluatedSemanticConst,
        crate::durable_comptime::DurableComptimeHostFailure,
    >,
) -> Result<
    Result<crate::durable_comptime::EvaluatedSemanticConst, EvaluateSemanticConstError>,
    rue_query::QueryFailure,
> {
    match outcome {
        rue_air::ComptimeOutcome::Known(value) => Ok(Ok(value)),
        rue_air::ComptimeOutcome::HostFailure(error) => match error.into_root_host_failure() {
            Ok(failure) => Ok(Err(EvaluateSemanticConstError::Failure(failure))),
            Err(failure) => Err(failure),
        },
        rue_air::ComptimeOutcome::Abort(error) => Ok(Err(EvaluateSemanticConstError::Abort(
            error.into_root_abort(),
        ))),
        rue_air::ComptimeOutcome::Trap(trap) => Ok(Err(
            crate::durable_comptime::DurableComptimeFailure::comptime_failure(format!(
                "{} (this operation would panic at runtime)",
                trap.operation
            )),
        )),
        rue_air::ComptimeOutcome::RuntimeDependent
        | rue_air::ComptimeOutcome::NotReady
        | rue_air::ComptimeOutcome::UnsupportedContext => Ok(Err(
            crate::durable_comptime::DurableComptimeFailure::resolution(
                "declaration-time comptime did not reduce to a value",
            ),
        )),
    }
}

impl crate::durable_comptime::DurableComptimeForeignCallAuthority
    for DurableComptimeRootAuthority<'_>
{
    fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> Result<crate::body_query::ForeignComptimeCallLookup, QueryAbort> {
        self.foreign
            .probe_comptime_call(producer, type_arguments, value_arguments)
    }
}

impl crate::durable_comptime::DurableComptimeHostAuthority for DurableComptimeRootAuthority<'_> {
    fn durable_session(&self) -> &crate::durable_comptime::DurableComptimeSession {
        &self.session
    }

    fn durable_session_mut(&mut self) -> &mut crate::durable_comptime::DurableComptimeSession {
        &mut self.session
    }

    #[cfg(test)]
    fn test_array_length_override(&self) -> Option<i128> {
        TEST_ARRAY_LENGTH_OVERRIDE.with(std::cell::Cell::get)
    }
}

pub(super) fn project_named_value_candidate(
    provider: &SemanticNucleusTypeProvider<'_>,
    accessing_source: &crate::StableDefinitionKey,
    module: &ModuleId,
    name: &str,
    kind: crate::durable_comptime::DurableComptimeNamedValueKind,
) -> Result<
    Option<crate::durable_comptime::DurableComptimeNamedValueProjection>,
    rue_air::SemanticProviderError<
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    >,
> {
    let dependency = |key: crate::StableDefinitionKey| {
        crate::semantic_query_nucleus::SemanticDeclarationDependency {
            source: accessing_source.clone(),
            kind: rue_air::DeclarationTypeDependencyKind::Body,
            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                key,
            ),
        }
    };
    match kind {
        crate::durable_comptime::DurableComptimeNamedValueKind::Const => {
            let Some(candidate) =
                provider.candidate_from(accessing_source, module, name, DefinitionKind::Const)?
            else {
                return Ok(None);
            };
            let resolution = provider.const_resolution(candidate)?;
            let (value, key, anonymous_nominals) = match resolution {
                crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                    key,
                    ty,
                    value,
                    anonymous_nominals,
                    ..
                } => (
                    crate::durable_comptime::EvaluatedSemanticConst::Value(
                        crate::durable_comptime::TypedSemanticConst::typed(*value, ty),
                    ),
                    key,
                    anonymous_nominals,
                ),
                crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                    key,
                    target,
                } => (
                    crate::durable_comptime::EvaluatedSemanticConst::Module(target),
                    key,
                    Arc::from([]),
                ),
            };
            Ok(Some(
                crate::durable_comptime::DurableComptimeNamedValueProjection::new(
                    value,
                    dependency(key),
                )
                .with_anonymous_nominals(anonymous_nominals),
            ))
        }
        crate::durable_comptime::DurableComptimeNamedValueKind::Function => {
            let Some(candidate) = provider.candidate_from(
                accessing_source,
                module,
                name,
                DefinitionKind::Function,
            )?
            else {
                return Ok(None);
            };
            let identity = provider.identity(candidate)?;
            let key = identity.key.clone();
            Ok(Some(
                crate::durable_comptime::DurableComptimeNamedValueProjection::new(
                    crate::durable_comptime::EvaluatedSemanticConst::Value(
                        crate::durable_comptime::TypedSemanticConst::typed(
                            crate::durable_semantics::DurableConstValue::Function(key),
                            crate::durable_semantics::DurableType::ComptimeType,
                        ),
                    ),
                    dependency(identity.key),
                ),
            ))
        }
        crate::durable_comptime::DurableComptimeNamedValueKind::Struct
        | crate::durable_comptime::DurableComptimeNamedValueKind::Enum => {
            let definition_kind = match kind {
                crate::durable_comptime::DurableComptimeNamedValueKind::Struct => {
                    DefinitionKind::Struct
                }
                crate::durable_comptime::DurableComptimeNamedValueKind::Enum => {
                    DefinitionKind::Enum
                }
                crate::durable_comptime::DurableComptimeNamedValueKind::Const
                | crate::durable_comptime::DurableComptimeNamedValueKind::Function => {
                    unreachable!("scalar named-value kinds handled above")
                }
            };
            let Some(candidate) =
                provider.candidate_from(accessing_source, module, name, definition_kind)?
            else {
                return Ok(None);
            };
            let identity = provider.identity(candidate)?;
            let key = identity.key.clone();
            Ok(Some(
                crate::durable_comptime::DurableComptimeNamedValueProjection::new(
                    crate::durable_comptime::EvaluatedSemanticConst::Value(
                        crate::durable_comptime::TypedSemanticConst::typed(
                            crate::durable_semantics::DurableConstValue::Type(
                                crate::durable_semantics::DurableType::Nominal(key),
                            ),
                            crate::durable_semantics::DurableType::ComptimeType,
                        ),
                    ),
                    dependency(identity.key),
                ),
            ))
        }
    }
}

impl crate::durable_comptime::DurableComptimeSemanticAuthority
    for DurableComptimeRootAuthority<'_>
{
    fn check_canceled(&self) -> Result<(), QueryAbort> {
        self.provider.context.check_canceled()
    }

    fn resolve_type_syntax(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Result<
        crate::durable_semantics::DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
            StableDefinitionKey,
            Arc<str>,
        >,
    > {
        let Some(registered) = self.session.registered_program(program) else {
            return Err(rue_air::SemanticResolutionError::ProviderFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                    "durable comptime type syntax references an unregistered program",
                )),
            ));
        };
        let module = program.declaration.module();
        let source = program.declaration.clone();
        self.provider.with_dependency_source(&source, |provider| {
            rue_air::resolve_structured_semantic_type_syntax_with(
                provider,
                module,
                registered.rir.type_syntax(),
                syntax,
                |symbol| registered.symbols[symbol.into_usize()].as_ref(),
            )
        })
    }

    fn resolve_type_syntax_with_substitutions(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_substitutions: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> Result<
        crate::durable_semantics::DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
            StableDefinitionKey,
            Arc<str>,
        >,
    > {
        let (provider, session) = (&mut self.provider, &self.session);
        let Some(registered) = session.registered_program(program) else {
            return Err(rue_air::SemanticResolutionError::ProviderFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                    "durable comptime type syntax references an unregistered program",
                )),
            ));
        };
        let module = program.declaration.module();
        let source = program.declaration.clone();
        provider.with_dependency_source(&source, |provider| {
            provider.with_comptime_substitutions(
                type_substitutions,
                value_substitutions,
                |provider| {
                    rue_air::resolve_structured_semantic_type_syntax_with(
                        provider,
                        module,
                        registered.rir.type_syntax(),
                        syntax,
                        |symbol| registered.symbols[symbol.into_usize()].as_ref(),
                    )
                },
            )
        })
    }

    fn begin_structured_type(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: Vec<(Arc<str>, crate::durable_semantics::DurableType)>,
        value_substitutions: Vec<(Arc<str>, crate::durable_semantics::DurableConstValue)>,
    ) -> Result<
        crate::durable_comptime::DurableStructuredTypePoll,
        crate::durable_comptime::DurableStructuredTypeBeginError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let source = program.declaration.clone();
        self.provider.with_dependency_source(&source, |provider| {
            crate::durable_comptime::begin_durable_structured_type(
                &self.session,
                program,
                syntax,
                type_substitutions,
                value_substitutions,
                provider,
            )
        })
    }

    fn resume_structured_type(
        &mut self,
        job: crate::durable_comptime::DurableStructuredTypeJob,
        reduced: rue_air::SemanticProviderResult<
            Option<
                rue_air::SemanticComptimeCallResult<
                    crate::durable_semantics::DurableType,
                    crate::durable_semantics::DurableConstValue,
                >,
            >,
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    ) -> Result<
        crate::durable_comptime::DurableStructuredTypePoll,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
            StableDefinitionKey,
            Arc<str>,
        >,
    > {
        let source = job.program().key().declaration.clone();
        self.provider.with_dependency_source(&source, |provider| {
            crate::durable_comptime::resume_durable_structured_type(job, provider, reduced)
        })
    }

    fn begin_comptime_call_admission(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        crate::durable_comptime::DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        type Failure = crate::semantic_query_nucleus::SemanticNucleusFailure;

        let candidate = self.provider.candidate_from(
            accessing_source,
            module,
            name,
            DefinitionKind::Function,
        )?;
        let Some(candidate) = candidate else {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(format!("undefined comptime function `{name}`"))),
            ));
        };
        let identity = self.provider.identity(candidate.clone())?;
        Ok(crate::durable_comptime::DurableComptimeCallableAdmissionStart {
            candidate,
            identity: identity.clone(),
            configuration: self.provider.configuration.clone(),
            name: Arc::from(name),
            dependency: crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: accessing_source.clone(),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target:
                    crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        identity.key,
                    ),
            },
        })
    }

    fn begin_comptime_call_admission_for_key(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        head: &crate::StableDefinitionKey,
    ) -> Result<
        crate::durable_comptime::DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        type Failure = crate::semantic_query_nucleus::SemanticNucleusFailure;

        let Some(candidate) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(head)
        else {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(format!(
                    "undefined comptime function `{}`",
                    head.name()
                ))),
            ));
        };
        let identity = self.provider.identity(candidate.clone())?;
        if identity.key != *head {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(
                    "comptime function identity does not match requested key",
                )),
            ));
        }
        Ok(crate::durable_comptime::DurableComptimeCallableAdmissionStart {
            candidate,
            identity: identity.clone(),
            configuration: self.provider.configuration.clone(),
            name: Arc::from(head.name()),
            dependency: crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: accessing_source.clone(),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target:
                    crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        identity.key,
                    ),
            },
        })
    }

    fn finish_comptime_call_admission(
        &self,
        start: crate::durable_comptime::DurableComptimeCallableAdmissionStart,
        argument_modes: &[crate::durable_semantics::DurableParameterMode],
    ) -> Result<
        crate::durable_comptime::DurableComptimeCallableAdmission,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        type Failure = crate::semantic_query_nucleus::SemanticNucleusFailure;
        let crate::durable_comptime::DurableComptimeCallableAdmissionStart {
            candidate,
            identity,
            configuration,
            name,
            dependency: _,
        } = start;
        let signature = self.provider.signature(candidate.clone())?;
        let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
            parameters,
            result,
            ..
        } = signature
        else {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(format!("`{name}` is not callable"))),
            ));
        };
        let shell = self
            .provider
            .context
            .query_registered(
                self.provider.shells,
                DeclarationShellQueryKey(candidate.clone()),
            )
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(shell)) =
            shell.outcome()
        else {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from("comptime call shell became unavailable")),
            ));
        };
        if shell.parameters.len() != argument_modes.len()
            || parameters.len() != argument_modes.len()
        {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(format!(
                    "comptime call `{name}` has the wrong arity"
                ))),
            ));
        }
        for (parameter, argument_mode) in parameters.iter().zip(argument_modes.iter().copied()) {
            use crate::durable_semantics::DurableParameterMode as ParameterMode;
            let failure = match (parameter.mode, argument_mode) {
                (ParameterMode::Value, ParameterMode::Value)
                | (ParameterMode::Borrow, ParameterMode::Borrow)
                | (ParameterMode::Inout, ParameterMode::Inout) => None,
                (ParameterMode::Inout, _) => Some(rue_error::ErrorKind::InoutKeywordMissing),
                (ParameterMode::Borrow, _) => Some(rue_error::ErrorKind::BorrowKeywordMissing),
                (ParameterMode::Value, ParameterMode::Borrow) => {
                    Some(rue_error::ErrorKind::UnexpectedCallArgumentMode { mode: "borrow" })
                }
                (ParameterMode::Value, ParameterMode::Inout) => {
                    Some(rue_error::ErrorKind::UnexpectedCallArgumentMode { mode: "inout" })
                }
            };
            if let Some(kind) = failure {
                return Err(rue_air::SemanticProviderError::Failure(
                    Failure::Diagnostic(kind),
                ));
            }
        }
        let all_parameters_comptime =
            !parameters.is_empty() && parameters.iter().all(|parameter| parameter.is_comptime);
        let is_type_function = result == crate::durable_semantics::DurableType::ComptimeType;
        let eligible = if is_type_function {
            parameters.is_empty() || all_parameters_comptime
        } else {
            all_parameters_comptime
        };
        if !eligible {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Diagnostic(rue_error::ErrorKind::ConstExprNotSupported {
                    expr_kind: format!("call to `{name}`"),
                }),
            ));
        }
        Ok(crate::durable_comptime::DurableComptimeCallableAdmission {
            candidate,
            identity,
            configuration,
            parameters,
            result,
            shell_parameters: shell.parameters.clone(),
        })
    }

    fn resolve_named_value(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<crate::durable_comptime::DurableComptimeNamedValueProjection>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        #[cfg(test)]
        {
            TEST_NAMED_VALUE_CHECKS.with(|checks| {
                checks.set(checks.get() + 1);
            });
            if TEST_NAMED_VALUE_CANCEL.with(std::cell::Cell::get) {
                return Err(rue_air::SemanticProviderError::Abort(QueryAbort::Canceled));
            }
        }
        crate::durable_comptime::resolve_named_value_in_order(|kind| {
            project_named_value_candidate(&self.provider, accessing_source, module, name, kind)
        })
    }

    fn resolve_module_member(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        member: &str,
    ) -> Result<
        crate::durable_comptime::DurableComptimeNamedValueProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(projection) = crate::durable_comptime::resolve_module_member_in_order(|kind| {
            project_named_value_candidate(&self.provider, accessing_source, module, member, kind)
        })?
        else {
            return Err(rue_air::SemanticProviderError::Failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::UnknownModuleMember {
                        module_name: module.to_string(),
                        member_name: member.to_owned(),
                    },
                ),
            ));
        };
        Ok(projection)
    }

    fn resolve_import(
        &self,
        site: &crate::durable_comptime::DurableImportSite,
    ) -> Result<crate::durable_comptime::DurableImportResolution, QueryAbort> {
        let key = crate::declaration_candidate::DeclarationImportSiteKey {
            declaration: site.declaration.clone(),
            occurrence: site.occurrence,
            specifier: site.specifier.clone(),
        };
        let terminal = self
            .provider
            .context
            .query_registered(&self.imports, DeclarationImportQueryKey(key))?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("DeclarationImport publishes typed values")
        };
        Ok(match value {
            DeclarationImportQueryValue::Available(crate::CanonicalImportResolution::Resolved(
                module,
            )) => crate::durable_comptime::DurableImportResolution::Resolved(module.clone()),
            DeclarationImportQueryValue::Available(crate::CanonicalImportResolution::Missing) => {
                crate::durable_comptime::DurableImportResolution::Missing
            }
            DeclarationImportQueryValue::Failure(failure) => {
                crate::durable_comptime::DurableImportResolution::Failure(failure.clone())
            }
        })
    }

    fn resolve_keyed_import(
        &self,
        site: &rue_air::ComptimeSite<crate::body_query::DurableComptimeProgramKey>,
        specifier: &str,
    ) -> Result<
        crate::durable_comptime::DurableImportResolution,
        crate::durable_comptime::DurableComptimeKeyedImportError,
    > {
        if site.kind() != rue_air::ComptimeSiteKind::Import {
            return Err(crate::durable_comptime::DurableComptimeKeyedImportError::WrongSiteKind);
        }
        let program = site.program();
        let Some(registered) = self.session.registered_program(program) else {
            return Err(crate::durable_comptime::DurableComptimeKeyedImportError::UnknownProgram);
        };
        let Some(occurrence) = registered
            .imports
            .imports
            .iter()
            .find(|occurrence| occurrence.occurrence == site.occurrence())
        else {
            return Err(
                crate::durable_comptime::DurableComptimeKeyedImportError::UnknownInstruction,
            );
        };
        if occurrence.specifier.as_ref() != specifier {
            return Err(
                crate::durable_comptime::DurableComptimeKeyedImportError::SpecifierMismatch,
            );
        }
        let Some(declaration) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &program.declaration,
            )
        else {
            return Err(
                crate::durable_comptime::DurableComptimeKeyedImportError::UnknownDeclaration,
            );
        };
        let durable_site = crate::durable_comptime::DurableImportSite {
            declaration,
            occurrence: occurrence.occurrence,
            specifier: occurrence.specifier.clone(),
        };
        self.resolve_import(&durable_site)
            .map_err(crate::durable_comptime::DurableComptimeKeyedImportError::ProviderAbort)
    }

    fn resolve_target_intrinsic(
        &self,
        intrinsic: rue_air::ComptimeTargetIntrinsic,
        argument_count: usize,
    ) -> Result<
        crate::durable_comptime::TargetEnumValue,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        crate::durable_comptime::resolve_target_intrinsic_facts(
            intrinsic,
            argument_count,
            self.provider.configuration.target.arch(),
            self.provider.configuration.target.os(),
            self.provider.configuration.target.data_model(),
        )
        .map_err(rue_air::SemanticProviderError::Failure)
    }

    fn resolve_target_enum_variant(
        &self,
        type_name: &str,
        variant: &str,
    ) -> Result<
        crate::durable_comptime::TargetEnumValue,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        crate::durable_comptime::resolve_target_enum_variant_facts(type_name, variant)
            .map_err(rue_air::SemanticProviderError::Failure)
    }
}

thread_local! {
    pub(super) static SEMANTIC_COMPTIME_CALL_DEPTH: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    #[cfg(test)]
    pub(super) static TEST_NAMED_VALUE_CANCEL: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    #[cfg(test)]
    pub(super) static TEST_NAMED_VALUE_CHECKS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    #[cfg(test)]
    pub(super) static TEST_ARRAY_LENGTH_OVERRIDE: std::cell::Cell<Option<i128>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) struct TestSemanticComptimeNamedValueCancelGuard {
    pub(super) previous: bool,
}
#[cfg(test)]
impl TestSemanticComptimeNamedValueCancelGuard {
    pub(super) fn set(value: bool) -> Self {
        let previous = TEST_NAMED_VALUE_CANCEL.with(|slot| {
            let previous = slot.get();
            slot.set(value);
            previous
        });
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for TestSemanticComptimeNamedValueCancelGuard {
    fn drop(&mut self) {
        TEST_NAMED_VALUE_CANCEL.with(|slot| slot.set(self.previous));
    }
}

#[cfg(test)]
pub(super) struct TestSemanticComptimeArrayLengthOverrideGuard {
    pub(super) previous: Option<i128>,
}
#[cfg(test)]
impl TestSemanticComptimeArrayLengthOverrideGuard {
    pub(super) fn set(value: Option<i128>) -> Self {
        let previous = TEST_ARRAY_LENGTH_OVERRIDE.with(|slot| {
            let previous = slot.get();
            slot.set(value);
            previous
        });
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for TestSemanticComptimeArrayLengthOverrideGuard {
    fn drop(&mut self) {
        TEST_ARRAY_LENGTH_OVERRIDE.with(|slot| slot.set(self.previous));
    }
}

/// Query-stack ticket for a durable comptime call.
///
/// The query boundary owns this ticket so cancellation and unwinding restore
/// the caller's depth; the limit and diagnostic authority remain in AIR.
pub(super) struct SemanticComptimeCallDepthGuard(usize);

impl SemanticComptimeCallDepthGuard {
    pub(super) fn enter(name: &str) -> Result<Self, EvaluateSemanticConstError> {
        SEMANTIC_COMPTIME_CALL_DEPTH.with(|depth| {
            let current = depth.get();
            // This guard wraps child query entries rather than the root AIR
            // frame, so the first active query is propagated call depth one.
            let propagated_depth = rue_air::next_comptime_depth(current);
            if rue_air::comptime_depth_over_limit(propagated_depth) {
                return Err(
                    crate::durable_comptime::DurableComptimeFailure::maximum_depth(
                        name,
                        rue_air::MAX_COMPTIME_CALL_DEPTH,
                    ),
                );
            }
            depth.set(current + 1);
            Ok(Self(current))
        })
    }
}

impl Drop for SemanticComptimeCallDepthGuard {
    fn drop(&mut self) {
        SEMANTIC_COMPTIME_CALL_DEPTH.with(|depth| depth.set(self.0));
    }
}

impl SemanticNucleusTypeProvider<'_> {
    pub(super) fn with_dependency_source<R>(
        &mut self,
        source: &crate::StableDefinitionKey,
        operation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        with_restored_state(
            self,
            |provider| std::mem::replace(&mut provider.dependency_source, source.clone()),
            operation,
            |provider, previous| provider.dependency_source = previous,
        )
    }

    pub(super) fn merge_comptime_effects(
        &mut self,
        effects: crate::durable_comptime::DurableComptimeEffects,
        policy: &crate::durable_comptime::DurableComptimeApplicationPolicy,
    ) {
        effects.apply_to(
            &mut self.anonymous_nominals,
            &mut self.dependencies,
            &mut self.deferred_ownership,
            policy,
        );
    }

    pub(super) fn ffi_shape_failure(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        path: &mut Vec<String>,
    ) -> Result<
        Option<(
            rue_air::FfiRejectReason,
            Vec<String>,
            crate::durable_semantics::DurableType,
        )>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::DurableType as T;
        use rue_air::FfiRejectReason as R;
        match ty {
            T::I8
            | T::I16
            | T::I32
            | T::I64
            | T::U8
            | T::U16
            | T::U32
            | T::U64
            | T::Bool
            | T::PtrConst(_)
            | T::PtrMut(_) => Ok(None),
            T::Array { element, .. } => self.ffi_shape_failure(element, path),
            T::Nominal(key) if key.kind() == crate::StableDefinitionKind::Enum => {
                Ok(Some((R::Enum, path.clone(), ty.clone())))
            }
            T::Nominal(key) if key.kind() == crate::StableDefinitionKind::Struct => {
                let Some(candidate) =
                    self.candidate(key.module(), key.name(), DefinitionKind::Struct)?
                else {
                    return Self::provider_failure(format!(
                        "FFI struct `{}` is unavailable",
                        key.name()
                    ));
                };
                let signature = self.signature(candidate)?;
                let crate::semantic_query_nucleus::DeclarationSignatureProjection::Struct {
                    fields,
                    is_linear,
                    is_repr_c,
                    ..
                } = signature
                else {
                    return Self::provider_failure("FFI nominal has the wrong signature kind");
                };
                if !is_repr_c {
                    return Ok(Some((R::NonReprCAggregate, path.clone(), ty.clone())));
                }
                if fields.is_empty() {
                    return Ok(Some((R::EmptyStruct, path.clone(), ty.clone())));
                }
                if is_linear {
                    return Ok(Some((R::Linear, path.clone(), ty.clone())));
                }
                if self
                    .candidate(key.module(), key.name(), DefinitionKind::Destructor)?
                    .is_some()
                {
                    return Ok(Some((R::HasDestructor, path.clone(), ty.clone())));
                }
                for (name, field) in fields.iter() {
                    path.push(name.to_string());
                    if let Some(failure) = self.ffi_shape_failure(field, path)? {
                        return Ok(Some(failure));
                    }
                    path.pop();
                }
                Ok(None)
            }
            T::AnonymousNominal(_)
            | T::Slice { .. }
            | T::Unit
            | T::Never
            | T::ComptimeType
            | T::BuiltinNominal { .. }
            | T::Module(_)
            | T::GenericParameter(_) => Ok(Some((R::UnsupportedType, path.clone(), ty.clone()))),
            T::Nominal(_) => Ok(Some((R::UnsupportedType, path.clone(), ty.clone()))),
        }
    }

    pub(super) fn repr_c_failure_for_fields(
        &mut self,
        fields: &[(Arc<str>, crate::durable_semantics::DurableType)],
        is_linear: bool,
        has_destructor: bool,
    ) -> Result<
        Option<(
            rue_air::FfiRejectReason,
            Vec<String>,
            crate::durable_semantics::DurableType,
        )>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use rue_air::FfiRejectReason as R;
        if fields.is_empty() {
            return Ok(Some((
                R::EmptyStruct,
                Vec::new(),
                crate::durable_semantics::DurableType::Unit,
            )));
        }
        if is_linear {
            return Ok(Some((
                R::Linear,
                Vec::new(),
                crate::durable_semantics::DurableType::Unit,
            )));
        }
        if has_destructor {
            return Ok(Some((
                R::HasDestructor,
                Vec::new(),
                crate::durable_semantics::DurableType::Unit,
            )));
        }
        let mut path = Vec::new();
        for (name, ty) in fields {
            path.push(name.to_string());
            if let Some(failure) = self.ffi_shape_failure(ty, &mut path)? {
                return Ok(Some(failure));
            }
            path.pop();
        }
        Ok(None)
    }

    pub(super) fn provider_failure_value(
        message: impl Into<Arc<str>>,
    ) -> rue_air::SemanticProviderError<
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        rue_air::SemanticProviderError::Failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(message.into()),
        )
    }

    pub(super) fn provider_failure<T>(
        message: impl Into<Arc<str>>,
    ) -> Result<
        T,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        Err(Self::provider_failure_value(message))
    }

    pub(super) fn provider_domain_failure<T>(
        failure: crate::semantic_query_nucleus::SemanticNucleusFailure,
    ) -> Result<
        T,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        Err(rue_air::SemanticProviderError::Failure(failure))
    }

    pub(super) fn type_carries_linear(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        match self.type_carries_linear_inner(ty, &mut OwnershipWalk::new())? {
            LinearOwnershipFact::DoesNotCarry => Ok(false),
            LinearOwnershipFact::Carries => Ok(true),
            LinearOwnershipFact::Deferred => Ok(false),
        }
    }

    /// The memoizing entry point every recursive call goes through.
    ///
    /// A nominal key is the only thing worth storing — reaching one costs a
    /// signature resolution — so anything else goes straight to the walk. The
    /// subtree's taint is measured on its own rather than inherited, then
    /// folded back into the caller's, so one recursive branch cannot suppress
    /// memoization of an unrelated sibling.
    pub(super) fn type_carries_linear_inner(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        LinearOwnershipFact,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let crate::durable_semantics::DurableType::Nominal(key) = ty else {
            return self.type_carries_linear_walk(ty, walk);
        };
        if let Some(fact) = self
            .ownership_properties
            .get(key)
            .and_then(|properties| properties.carries_linear)
        {
            return Ok(fact);
        }
        let outer = std::mem::replace(&mut walk.tainted, false);
        let result = self.type_carries_linear_walk(ty, walk);
        let tainted = walk.tainted;
        walk.tainted = outer || tainted;
        if let Ok(fact) = &result
            && !tainted
        {
            self.ownership_properties
                .entry(key.clone())
                .or_default()
                .carries_linear = Some(*fact);
        }
        result
    }

    pub(super) fn type_carries_linear_walk(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        LinearOwnershipFact,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;

        match ty {
            T::Array { len: 0, .. } => Ok(LinearOwnershipFact::DoesNotCarry),
            T::Array { element, .. } => self.type_carries_linear_inner(element, walk),
            T::Nominal(key) => {
                if !walk.visiting.insert(key.clone()) {
                    // Provisional: this key is already on the stack, so the
                    // answer belongs to that stack rather than to the type.
                    walk.taint();
                    return Ok(LinearOwnershipFact::DoesNotCarry);
                }
                let kind = match key.kind() {
                    crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                    crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                    _ => {
                        walk.visiting.remove(key);
                        return Self::provider_failure(format!(
                            "non-nominal definition `{}` used as a nominal type",
                            key.name()
                        ));
                    }
                };
                let candidate =
                    self.candidate(key.module(), key.name(), kind)?
                        .ok_or_else(|| {
                            Self::provider_failure_value(format!(
                                "nominal definition `{}` is unavailable",
                                key.name()
                            ))
                        })?;
                let signature_query = crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                    self.declaration_query(candidate.clone()),
                );
                let resolved = match self.resolved_signature(candidate) {
                    Ok(signature) => signature,
                    Err(rue_air::SemanticProviderError::Failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::SignatureReentry {
                            signature,
                            ..
                        },
                    )) if signature == *key => {
                        walk.visiting.remove(key);
                        // Not resolvable yet; a later request may answer
                        // differently, so nothing here is a stable property.
                        walk.taint();
                        return Ok(LinearOwnershipFact::Deferred);
                    }
                    Err(rue_air::SemanticProviderError::Abort(QueryAbort::Cycle(nodes)))
                        if nodes.iter().any(|node| {
                            node.family() == "compiler.semantic-nucleus"
                                && node.key() == signature_query.stable_identity()
                        }) =>
                    {
                        walk.visiting.remove(key);
                        // Not resolvable yet; a later request may answer
                        // differently, so nothing here is a stable property.
                        walk.taint();
                        return Ok(LinearOwnershipFact::Deferred);
                    }
                    Err(error) => {
                        walk.visiting.remove(key);
                        return Err(error);
                    }
                };
                self.anonymous_nominals.extend(
                    resolved
                        .anonymous_nominals
                        .iter()
                        .cloned()
                        .map(|nominal| (nominal.identity.clone(), nominal)),
                );
                let signature = resolved.signature;
                let carries = match signature {
                    P::Struct {
                        fields, is_linear, ..
                    } => {
                        let mut carries = if is_linear {
                            LinearOwnershipFact::Carries
                        } else {
                            LinearOwnershipFact::DoesNotCarry
                        };
                        for (_, field) in fields.iter() {
                            carries = carries.combine(self.type_carries_linear_inner(field, walk)?);
                        }
                        carries
                    }
                    P::Enum { variants, .. } => {
                        let mut carries = LinearOwnershipFact::DoesNotCarry;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                carries =
                                    carries.combine(self.type_carries_linear_inner(field, walk)?);
                            }
                        }
                        carries
                    }
                    _ => {
                        walk.visiting.remove(key);
                        return Self::provider_failure(format!(
                            "nominal definition `{}` has a non-nominal signature",
                            key.name()
                        ));
                    }
                };
                walk.visiting.remove(key);
                Ok(carries)
            }
            T::AnonymousNominal(key) => {
                let Some(nominal) = self.anonymous_nominals.get(key).cloned() else {
                    return Self::provider_failure(
                        "anonymous nominal is unavailable while checking linearity",
                    );
                };
                match nominal.shape {
                    S::Struct { fields, .. } => {
                        let mut carries = LinearOwnershipFact::DoesNotCarry;
                        for (_, field) in fields.iter() {
                            carries = carries.combine(self.type_carries_linear_inner(field, walk)?);
                        }
                        Ok(carries)
                    }
                    S::Enum { variants, .. } => {
                        let mut carries = LinearOwnershipFact::DoesNotCarry;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                carries =
                                    carries.combine(self.type_carries_linear_inner(field, walk)?);
                            }
                        }
                        Ok(carries)
                    }
                }
            }
            T::Slice { .. } | T::PtrConst(_) | T::PtrMut(_) => {
                Ok(LinearOwnershipFact::DoesNotCarry)
            }
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
            | T::ComptimeType
            | T::BuiltinNominal { .. }
            | T::Module(_)
            | T::GenericParameter(_) => Ok(LinearOwnershipFact::DoesNotCarry),
        }
    }

    pub(super) fn type_has_drop_glue(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        self.type_has_drop_glue_inner(ty, &mut OwnershipWalk::new())
    }

    /// See [`Self::type_carries_linear_inner`] for why the memo is keyed on
    /// nominal types and why a tainted answer is not stored.
    pub(super) fn type_has_drop_glue_inner(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let crate::durable_semantics::DurableType::Nominal(key) = ty else {
            return self.type_has_drop_glue_walk(ty, walk);
        };
        if let Some(has_glue) = self
            .ownership_properties
            .get(key)
            .and_then(|properties| properties.has_drop_glue)
        {
            return Ok(has_glue);
        }
        let outer = std::mem::replace(&mut walk.tainted, false);
        let result = self.type_has_drop_glue_walk(ty, walk);
        let tainted = walk.tainted;
        walk.tainted = outer || tainted;
        if let Ok(has_glue) = &result
            && !tainted
        {
            self.ownership_properties
                .entry(key.clone())
                .or_default()
                .has_drop_glue = Some(*has_glue);
        }
        result
    }

    pub(super) fn type_has_drop_glue_walk(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;
        match ty {
            T::Array { len: 0, .. } => Ok(false),
            T::Array { element, .. } => self.type_has_drop_glue_inner(element, walk),
            T::Nominal(key) => {
                if !walk.visiting.insert(key.clone()) {
                    // Provisional; see `type_carries_linear_walk`.
                    walk.taint();
                    return Ok(false);
                }
                if key.kind() == crate::StableDefinitionKind::Struct {
                    let destructors = self
                        .context
                        .query_registered(
                            self.names,
                            LookupNameKey {
                                module: key.module().clone(),
                                namespace: DefinitionNamespace::Destructor,
                                name: Arc::from(key.name()),
                            },
                        )
                        .map_err(rue_air::SemanticProviderError::Abort)?;
                    let rue_query::QueryOutcome::Success(LookupNameValue(destructors)) =
                        destructors.outcome()
                    else {
                        unreachable!("LookupName publishes typed values")
                    };
                    if destructors.as_ref().is_ok_and(|facts| !facts.is_empty()) {
                        walk.visiting.remove(key);
                        return Ok(true);
                    }
                }
                let kind = match key.kind() {
                    crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                    crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                    _ => {
                        walk.visiting.remove(key);
                        return Ok(false);
                    }
                };
                let candidate = self
                    .candidate(key.module(), key.name(), kind)?
                    .ok_or_else(|| Self::provider_failure_value("nominal type is unavailable"))?;
                let signature = self.resolved_signature(candidate)?.signature;
                let has_glue = match signature {
                    P::Struct { fields, .. } => {
                        let mut has_glue = false;
                        for (_, field) in fields.iter() {
                            has_glue |= self.type_has_drop_glue_inner(field, walk)?;
                        }
                        has_glue
                    }
                    P::Enum { variants, .. } => {
                        let mut has_glue = false;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                has_glue |= self.type_has_drop_glue_inner(field, walk)?;
                            }
                        }
                        has_glue
                    }
                    _ => false,
                };
                walk.visiting.remove(key);
                Ok(has_glue)
            }
            T::AnonymousNominal(key) => {
                let nominal = self.anonymous_nominals.get(key).cloned().ok_or_else(|| {
                    Self::provider_failure_value(
                        "anonymous nominal is unavailable while checking drop glue",
                    )
                })?;
                match nominal.shape {
                    S::Struct { fields, .. } => {
                        for (_, field) in fields.iter() {
                            if self.type_has_drop_glue_inner(field, walk)? {
                                return Ok(true);
                            }
                        }
                    }
                    S::Enum { variants, .. } => {
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                if self.type_has_drop_glue_inner(field, walk)? {
                                    return Ok(true);
                                }
                            }
                        }
                    }
                }
                Ok(false)
            }
            T::GenericParameter { .. } => Self::provider_failure(
                "generic parameter remained unresolved while checking drop glue",
            ),
            _ => Ok(false),
        }
    }

    pub(super) fn type_is_copy(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        self.type_is_copy_inner(ty, &mut OwnershipWalk::new())
    }

    /// See [`Self::type_carries_linear_inner`] for why the memo is keyed on
    /// nominal types and why a tainted answer is not stored.
    pub(super) fn type_is_copy_inner(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let crate::durable_semantics::DurableType::Nominal(key) = ty else {
            return self.type_is_copy_walk(ty, walk);
        };
        if let Some(is_copy) = self
            .ownership_properties
            .get(key)
            .and_then(|properties| properties.is_copy)
        {
            return Ok(is_copy);
        }
        let outer = std::mem::replace(&mut walk.tainted, false);
        let result = self.type_is_copy_walk(ty, walk);
        let tainted = walk.tainted;
        walk.tainted = outer || tainted;
        if let Ok(is_copy) = &result
            && !tainted
        {
            self.ownership_properties
                .entry(key.clone())
                .or_default()
                .is_copy = Some(*is_copy);
        }
        result
    }

    pub(super) fn type_is_copy_walk(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        walk: &mut OwnershipWalk,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;

        match ty {
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
            | T::ComptimeType
            | T::PtrConst(_)
            | T::PtrMut(_)
            | T::Module(_)
            | T::Slice { .. }
            | T::BuiltinNominal { .. } => Ok(true),
            T::GenericParameter(_) => {
                Self::provider_failure("unsubstituted generic parameter reached Copy validation")
            }
            T::Array { element, .. } => self.type_is_copy_inner(element, walk),
            T::Nominal(key) => {
                if !walk.visiting.insert(key.clone()) {
                    // Provisional; see `type_carries_linear_walk`.
                    walk.taint();
                    return Ok(true);
                }
                let kind = match key.kind() {
                    crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                    crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                    _ => {
                        walk.visiting.remove(key);
                        return Self::provider_failure(format!(
                            "non-nominal definition `{}` used as a nominal type",
                            key.name()
                        ));
                    }
                };
                let candidate =
                    self.candidate(key.module(), key.name(), kind)?
                        .ok_or_else(|| {
                            Self::provider_failure_value(format!(
                                "nominal definition `{}` is unavailable",
                                key.name()
                            ))
                        })?;
                let resolved = self.resolved_signature(candidate)?;
                self.anonymous_nominals.extend(
                    resolved
                        .anonymous_nominals
                        .iter()
                        .cloned()
                        .map(|nominal| (nominal.identity.clone(), nominal)),
                );
                let is_copy = match resolved.signature {
                    P::Struct { is_copy, .. } => is_copy,
                    P::Enum { variants, .. } => {
                        let mut is_copy = true;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                is_copy &= self.type_is_copy_inner(field, walk)?;
                            }
                        }
                        is_copy
                    }
                    _ => {
                        walk.visiting.remove(key);
                        return Self::provider_failure(format!(
                            "nominal definition `{}` has a non-nominal signature",
                            key.name()
                        ));
                    }
                };
                walk.visiting.remove(key);
                Ok(is_copy)
            }
            T::AnonymousNominal(key) => {
                let nominal = self.anonymous_nominals.get(key).cloned().ok_or_else(|| {
                    Self::provider_failure_value(
                        "anonymous nominal is unavailable while checking Copy",
                    )
                })?;
                match nominal.shape {
                    S::Struct { fields, .. } => {
                        for (_, field) in fields.iter() {
                            if !self.type_is_copy_inner(field, walk)? {
                                return Ok(false);
                            }
                        }
                    }
                    S::Enum { variants, .. } => {
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                if !self.type_is_copy_inner(field, walk)? {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
                Ok(true)
            }
        }
    }

    pub(super) fn candidate(
        &self,
        module: &ModuleId,
        name: &str,
        kind: DefinitionKind,
    ) -> Result<
        Option<crate::declaration_candidate::DeclarationCandidateKey>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        self.candidate_from(&self.dependency_source, module, name, kind)
    }

    pub(super) fn candidate_from(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
        kind: DefinitionKind,
    ) -> Result<
        Option<crate::declaration_candidate::DeclarationCandidateKey>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let terminal = self
            .context
            .query_registered(
                self.names,
                LookupNameKey {
                    module: module.clone(),
                    namespace: if kind == DefinitionKind::Destructor {
                        DefinitionNamespace::Destructor
                    } else {
                        DefinitionNamespace::ModuleItem
                    },
                    name: Arc::from(name),
                },
            )
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(LookupNameValue(result)) = terminal.outcome() else {
            unreachable!("LookupName publishes typed values")
        };
        let entries = result
            .as_ref()
            .map_err(|failure| Self::provider_failure_value(format!("{failure:?}")))?;
        let mut matching = entries.iter().filter(|entry| entry.kind == kind);
        let Some(entry) = matching.next() else {
            return Ok(None);
        };
        if matching.next().is_some() {
            return Self::provider_failure(format!(
                "ambiguous declaration `{name}` in module {module}"
            ));
        }
        let defining = rue_air::SemanticVisibilityDomain::from_file_path(Some(module.as_str()));
        let accessing = rue_air::SemanticVisibilityDomain::from_file_path(Some(
            accessing_source.module().as_str(),
        ));
        let is_public = entry.visibility == Some(rue_parser::ast::Visibility::Public);
        if !defining.is_visible_from(&accessing, is_public) {
            return Self::provider_domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::PrivateMemberAccess {
                        item_kind: format!("{kind:?}").to_lowercase(),
                        name: name.to_owned(),
                    },
                ),
            );
        }
        let categories: &[crate::declaration_candidate::DeclarationCandidateCategory] = match kind {
            DefinitionKind::Function => &[
                crate::declaration_candidate::DeclarationCandidateCategory::Function,
                crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction,
            ],
            DefinitionKind::Struct => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::Struct]
            }
            DefinitionKind::Enum => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::Enum]
            }
            DefinitionKind::Const => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate]
            }
            DefinitionKind::Destructor => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::Destructor]
            }
        };
        for category in categories {
            let key = crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category: *category,
                name: entry.name.clone(),
                owner: (*category
                    == crate::declaration_candidate::DeclarationCandidateCategory::Destructor)
                    .then(|| crate::declaration_candidate::DeclarationCandidateOwner {
                        category:
                            crate::declaration_candidate::DeclarationCandidateCategory::Struct,
                        name: entry.name.clone(),
                    }),
                duplicate_discriminator: 0,
            };
            let shell = self
                .context
                .query_registered(self.shells, DeclarationShellQueryKey(key.clone()))
                .map_err(rue_air::SemanticProviderError::Abort)?;
            let rue_query::QueryOutcome::Success(shell) = shell.outcome() else {
                unreachable!("DeclarationShell publishes typed values")
            };
            if matches!(shell, DeclarationShellQueryValue::Available(_)) {
                return Ok(Some(key));
            }
        }
        Self::provider_failure(format!(
            "name index and declaration-shell index disagree for `{name}`"
        ))
    }

    pub(super) fn query(
        &self,
        key: crate::semantic_query_nucleus::SemanticNucleusKey,
    ) -> Result<
        crate::semantic_query_nucleus::SemanticNucleusValue,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let terminal = self
            .context
            .query_registered(self.family, key)
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("SemanticNucleus publishes typed values")
        };
        Ok(value.clone())
    }

    pub(super) fn declaration_query(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: self.configuration.clone(),
        }
    }

    pub(super) fn identity(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::DeclarationIdentityProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::{SemanticNucleusKey as K, SemanticNucleusValue as V};
        match self.query(K::Identity(self.declaration_query(declaration)))? {
            V::Identity(identity) => Ok(identity),
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("identity query returned the wrong projection"),
        }
    }

    pub(super) fn const_resolution(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::ConstResolutionProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::{SemanticNucleusKey as K, SemanticNucleusValue as V};
        match self.query(K::ConstResolution(self.declaration_query(declaration)))? {
            V::ConstResolution(value) => Ok(value),
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("const query returned the wrong projection"),
        }
    }

    pub(super) fn signature(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::DeclarationSignatureProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        Ok(self.resolved_signature(declaration)?.signature)
    }

    pub(super) fn resolved_signature(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::ResolvedDeclarationSignature,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::{SemanticNucleusKey as K, SemanticNucleusValue as V};
        match self.query(K::Signature(self.declaration_query(declaration)))? {
            V::Signature(value) => Ok(value),
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("signature query returned the wrong projection"),
        }
    }

    pub(super) fn validate_nominal_well_formedness(
        &mut self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        (),
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;

        pub(super) fn collect_type(
            ty: &T,
            anonymous: &BTreeMap<
                crate::AnonymousNominalKey,
                crate::durable_semantics::DurableAnonymousNominal,
            >,
            neighbors: &mut BTreeSet<StableDefinitionKey>,
        ) {
            let mut pending = vec![ty];
            let mut seen_anonymous = BTreeSet::new();
            while let Some(ty) = pending.pop() {
                match ty {
                    T::Nominal(key) => {
                        neighbors.insert(key.clone());
                    }
                    // Arrays are inline containment edges even at length zero.
                    T::Array { element, .. } => pending.push(element),
                    T::AnonymousNominal(key) if seen_anonymous.insert(key.clone()) => {
                        if let Some(nominal) = anonymous.get(key) {
                            match &nominal.shape {
                                S::Struct { fields, .. } => {
                                    pending.extend(fields.iter().map(|(_, ty)| ty));
                                }
                                S::Enum { variants, .. } => {
                                    pending.extend(
                                        variants.iter().flat_map(|(_, payload)| payload.iter()),
                                    );
                                }
                            }
                        }
                    }
                    // Pointers and slices are indirection and therefore break
                    // the by-value containment graph.
                    T::PtrConst(_) | T::PtrMut(_) | T::Slice { .. } => {}
                    _ => {}
                }
            }
        }

        let root = self.identity(declaration.clone())?.key;
        if declaration.category
            == crate::declaration_candidate::DeclarationCandidateCategory::Struct
            && matches!(
                self.signature(declaration.clone())?,
                P::Struct { is_copy: true, .. }
            )
            && self
                .candidate(
                    &declaration.module,
                    &declaration.name,
                    DefinitionKind::Destructor,
                )?
                .is_some()
        {
            return Self::provider_domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::CopyStructWithDestructor {
                        type_name: declaration.name.to_string(),
                    },
                ),
            );
        }
        let mut colors = BTreeMap::<StableDefinitionKey, u8>::new();
        let mut path = vec![root.clone()];
        let mut frames = Vec::<(StableDefinitionKey, Vec<StableDefinitionKey>, usize)>::new();

        let load = |provider: &mut Self,
                    key: &StableDefinitionKey|
         -> Result<
            Vec<StableDefinitionKey>,
            rue_air::SemanticProviderError<
                QueryAbort,
                crate::semantic_query_nucleus::SemanticNucleusFailure,
            >,
        > {
            let kind = match key.kind() {
                crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                _ => return Ok(Vec::new()),
            };
            let Some(candidate) = provider.candidate(key.module(), key.name(), kind)? else {
                return Self::provider_failure(format!(
                    "nominal definition `{}` is unavailable",
                    key.name()
                ));
            };
            let resolved = provider.resolved_signature(candidate)?;
            let anonymous = resolved
                .anonymous_nominals
                .iter()
                .cloned()
                .map(|nominal| (nominal.identity.clone(), nominal))
                .collect::<BTreeMap<_, _>>();
            let mut neighbors = BTreeSet::new();
            match &resolved.signature {
                P::Struct { fields, .. } => {
                    for (_, ty) in fields.iter() {
                        collect_type(ty, &anonymous, &mut neighbors);
                    }
                }
                P::Enum { variants, .. } => {
                    for (_, payload) in variants.iter() {
                        for ty in payload.iter() {
                            collect_type(ty, &anonymous, &mut neighbors);
                        }
                    }
                }
                _ => {
                    return Self::provider_failure(format!(
                        "nominal definition `{}` has a non-nominal signature",
                        key.name()
                    ));
                }
            }
            Ok(neighbors.into_iter().collect())
        };

        colors.insert(root.clone(), 1);
        frames.push((root.clone(), load(self, &root)?, 0));
        while let Some((key, neighbors, next)) = frames.last_mut() {
            if *next == neighbors.len() {
                colors.insert(key.clone(), 2);
                frames.pop();
                path.pop();
                continue;
            }
            let child = neighbors[*next].clone();
            *next += 1;
            match colors.get(&child).copied() {
                Some(1) => {
                    let start = path.iter().position(|key| key == &child).unwrap_or(0);
                    let cycle = path[start..]
                        .iter()
                        .chain(std::iter::once(&child))
                        .map(|key| key.name())
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    return Self::provider_domain_failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            rue_error::ErrorKind::RecursiveTypeInfiniteSize {
                                name: child.name().to_owned(),
                                cycle,
                            },
                        ),
                    );
                }
                Some(2) => {}
                _ => {
                    colors.insert(child.clone(), 1);
                    path.push(child.clone());
                    frames.push((child.clone(), load(self, &child)?, 0));
                }
            }
        }
        Ok(())
    }

    pub(super) fn constructor_fact(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<
            rue_air::SemanticTypeConstructorHead<
                StableDefinitionKey,
                Arc<str>,
                StableDefinitionKey,
            >,
        >,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::DeclarationSignatureProjection;
        let Some(candidate) = self.candidate(module, name, DefinitionKind::Function)? else {
            return Ok(None);
        };
        let identity = self.identity(candidate.clone())?;
        let signature = self.signature(candidate.clone())?;
        let DeclarationSignatureProjection::Callable {
            parameters, result, ..
        } = signature
        else {
            return Ok(None);
        };
        let shell = self
            .context
            .query_registered(self.shells, DeclarationShellQueryKey(candidate))
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(shell)) =
            shell.outcome()
        else {
            return Self::provider_failure("constructor shell became unavailable");
        };
        if shell.parameters.len() != parameters.len() {
            return Self::provider_failure("constructor parameter projections disagree");
        }
        let parameters = shell
            .parameters
            .iter()
            .zip(parameters.iter())
            .map(
                |(header, parameter)| rue_air::SemanticTypeConstructorParameter {
                    name: header.name.clone(),
                    is_comptime: parameter.is_comptime,
                    is_type: parameter.is_comptime
                        && parameter.ty == crate::durable_semantics::DurableType::ComptimeType,
                },
            )
            .collect::<Vec<_>>();
        self.dependencies.insert(
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.dependency_source.clone(),
                kind: self.dependency_kind,
                target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::TypeCallHead(
                    identity.key.clone(),
                ),
            },
        );
        Ok(Some(rue_air::SemanticTypeConstructorHead {
            key: identity.key.clone(),
            site: identity.key,
            parameters: parameters.into(),
            returns_type: result == crate::durable_semantics::DurableType::ComptimeType,
            is_public: identity.is_public,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }

    pub(super) fn module_binding_fact(
        &self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<rue_air::SemanticModuleBinding<ModuleId, StableDefinitionKey>>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(candidate) = self.candidate(module, name, DefinitionKind::Const)? else {
            return Ok(None);
        };
        let resolution = self.const_resolution(candidate)?;
        let crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding { key, target } =
            resolution
        else {
            return Ok(None);
        };
        let shell = self.identity_key_visibility(&key)?;
        Ok(Some(rue_air::SemanticModuleBinding {
            target,
            site: key,
            is_public: shell,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }

    pub(super) fn identity_key_visibility(
        &self,
        key: &StableDefinitionKey,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let category = match key.kind() {
            crate::StableDefinitionKind::Function => {
                crate::declaration_candidate::DeclarationCandidateCategory::Function
            }
            crate::StableDefinitionKind::Struct => {
                crate::declaration_candidate::DeclarationCandidateCategory::Struct
            }
            crate::StableDefinitionKind::Enum => {
                crate::declaration_candidate::DeclarationCandidateCategory::Enum
            }
            crate::StableDefinitionKind::ValueConst
            | crate::StableDefinitionKind::ModuleBinding => {
                crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate
            }
            _ => return Ok(false),
        };
        let candidate = crate::declaration_candidate::DeclarationCandidateKey {
            module: key.module().clone(),
            category,
            name: Arc::from(key.name()),
            owner: None,
            duplicate_discriminator: 0,
        };
        let terminal = self
            .context
            .query_registered(self.shells, DeclarationShellQueryKey(candidate))
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("DeclarationShell publishes typed values")
        };
        match value {
            DeclarationShellQueryValue::Available(shell) => Ok(shell.is_public),
            DeclarationShellQueryValue::Failure(failure) => {
                Self::provider_failure(format!("{failure:?}"))
            }
        }
    }

    pub(super) fn named_fact(
        &self,
        module: &ModuleId,
        name: &str,
        kind: DefinitionKind,
    ) -> Result<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(candidate) = self.candidate(module, name, kind)? else {
            return Ok(None);
        };
        let identity = self.identity(candidate)?;
        Ok(Some(rue_air::SemanticTypeFact {
            value: crate::durable_semantics::DurableType::Nominal(identity.key.clone()),
            site: identity.key,
            is_public: identity.is_public,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }

    pub(super) fn alias_fact(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(candidate) = self.candidate(module, name, DefinitionKind::Const)? else {
            return Ok(None);
        };
        let resolution = self.const_resolution(candidate)?;
        let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
            key,
            value,
            anonymous_nominals,
            dependencies,
            ..
        } = resolution
        else {
            return Ok(None);
        };
        let crate::durable_semantics::DurableConstValue::Type(value) = *value else {
            return Ok(None);
        };
        self.anonymous_nominals.extend(
            anonymous_nominals
                .iter()
                .cloned()
                .map(|value| (value.identity.clone(), value)),
        );
        self.dependencies.extend(dependencies.iter().cloned());
        let is_public = self.identity_key_visibility(&key)?;
        Ok(Some(rue_air::SemanticTypeFact {
            value,
            site: key,
            is_public,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }
}

impl rue_air::SemanticModulePathProvider<ModuleId, ModuleId, StableDefinitionKey>
    for SemanticNucleusTypeProvider<'_>
{
    type Abort = QueryAbort;
    type Failure = crate::semantic_query_nucleus::SemanticNucleusFailure;

    fn root_module_binding(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> Result<
        Option<rue_air::SemanticModuleBinding<ModuleId, StableDefinitionKey>>,
        rue_air::SemanticProviderError<Self::Abort, Self::Failure>,
    > {
        self.module_binding_fact(scope, name)
    }

    fn module_binding(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<rue_air::SemanticModuleBinding<ModuleId, StableDefinitionKey>>,
        rue_air::SemanticProviderError<Self::Abort, Self::Failure>,
    > {
        self.module_binding_fact(module, name)
    }

    fn module_display_name(&self, module: &ModuleId) -> Arc<str> {
        Arc::from(module.as_str())
    }

    fn accessing_domain(&self, scope: &ModuleId) -> rue_air::SemanticVisibilityDomain {
        rue_air::SemanticVisibilityDomain::from_file_path(Some(scope.as_str()))
    }
}

#[rustfmt::skip]
impl rue_air::SemanticTypeSyntaxProvider<ModuleId, ModuleId, StableDefinitionKey, StableDefinitionKey, Arc<str>, crate::durable_semantics::DurableType, crate::durable_semantics::DurableConstValue> for SemanticNucleusTypeProvider<'_> {
    fn with_comptime_substitutions<R>(
        &mut self,
        type_substitutions: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_substitutions: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
        operation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let new_types = type_substitutions.iter().cloned().collect();
        let new_values = value_substitutions.iter().cloned().collect();
        with_restored_state(
            self,
            |provider| {
                (
                    std::mem::replace(&mut provider.substitutions, new_types),
                    std::mem::replace(&mut provider.value_substitutions, new_values),
                )
            },
            operation,
            |provider, (previous_types, previous_values)| {
                provider.substitutions = previous_types;
                provider.value_substitutions = previous_values;
            },
        )
    }

    fn observe_selected_named_type(
        &mut self,
        _name: &str,
        kind: rue_air::SemanticTypeFactKind,
        fact: &rue_air::SemanticTypeFact<
            crate::durable_semantics::DurableType,
            StableDefinitionKey,
        >,
    ) -> rue_air::SemanticProviderResult<
        (),
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        if matches!(
            kind,
            rue_air::SemanticTypeFactKind::Struct
                | rue_air::SemanticTypeFactKind::Enum
                | rue_air::SemanticTypeFactKind::Constant
        ) {
            self.dependencies.insert(
                crate::semantic_query_nucleus::SemanticDeclarationDependency {
                    source: self.dependency_source.clone(),
                    kind: self.dependency_kind,
                    target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                        fact.site.clone(),
                    ),
                },
            );
        }
        Ok(())
    }

    fn observe_materialized_type(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        (),
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        fn collect(
            ty: &crate::durable_semantics::DurableType,
            output: &mut Vec<StableDefinitionKey>,
        ) {
            match ty {
                crate::durable_semantics::DurableType::Nominal(key) => output.push(key.clone()),
                crate::durable_semantics::DurableType::Array { element, .. }
                | crate::durable_semantics::DurableType::Slice { element, .. }
                | crate::durable_semantics::DurableType::PtrConst(element)
                | crate::durable_semantics::DurableType::PtrMut(element) => {
                    collect(element, output)
                }
                _ => {}
            }
        }
        let mut targets = Vec::new();
        collect(ty, &mut targets);
        self.dependencies.extend(targets.into_iter().map(|target| {
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.dependency_source.clone(),
                kind: self.dependency_kind,
                target:
                    crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                        target,
                    ),
            }
        }));
        Ok(())
    }

    fn substituted_type(
        &mut self,
        _scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(self.substitutions.get(name).cloned())
    }

    fn primitive_type(
        &mut self,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        use crate::durable_semantics::DurableType as T;
        Ok(Some(match name {
            "i8" => T::I8,
            "i16" => T::I16,
            "i32" => T::I32,
            "i64" => T::I64,
            "isize" => T::I64,
            "u8" => T::U8,
            "u16" => T::U16,
            "u32" => T::U32,
            "u64" => T::U64,
            "usize" => T::U64,
            "bool" => T::Bool,
            "()" => T::Unit,
            "!" => T::Never,
            "type" => T::ComptimeType,
            _ => return Ok(None),
        }))
    }

    fn builtin_type(
        &mut self,
        _scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(
            (name == "str").then(|| crate::durable_semantics::DurableType::BuiltinNominal {
                name: Arc::from("str"),
                kind: rue_air::SemanticImportNominalKind::Struct,
            }),
        )
    }

    fn root_struct_type(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(scope, name, DefinitionKind::Struct)
    }
    fn root_enum_type(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(scope, name, DefinitionKind::Enum)
    }
    fn root_type_alias(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.alias_fact(scope, name)
    }
    fn module_struct_type(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(module, name, DefinitionKind::Struct)
    }
    fn module_enum_type(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(module, name, DefinitionKind::Enum)
    }
    fn module_type_alias(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.alias_fact(module, name)
    }

    fn resolve_array_length(
        &mut self,
        scope: &ModuleId,
        length: rue_air::SemanticValueSyntax<'_>,
    ) -> rue_air::SemanticProviderResult<
        Option<u64>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        match length {
            rue_air::SemanticValueSyntax::Integer(value) => u64::try_from(value)
                .map(Some)
                .map_err(|_| {
                    rue_air::SemanticProviderError::Failure(durable_literal_array_length_failure(
                        value,
                    ))
                }),
            rue_air::SemanticValueSyntax::Name(name) => {
                if let Some(value) = self.value_substitutions.get(name) {
                    return crate::durable_comptime::durable_named_array_length_const(value)
                        .map(Some)
                        .map_err(|error| {
                            rue_air::SemanticProviderError::Failure(
                                durable_provider_named_array_length_failure(name, error),
                            )
                        });
                }
                if let Some(ty) = self.deferred_value_parameters.get(name) {
                    if matches!(
                        ty,
                        crate::durable_semantics::DurableType::I8
                            | crate::durable_semantics::DurableType::I16
                            | crate::durable_semantics::DurableType::I32
                            | crate::durable_semantics::DurableType::I64
                            | crate::durable_semantics::DurableType::U8
                            | crate::durable_semantics::DurableType::U16
                            | crate::durable_semantics::DurableType::U32
                            | crate::durable_semantics::DurableType::U64
                    ) {
                        return Ok(None);
                    }
                    return Self::provider_domain_failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            rue_error::ErrorKind::InvalidArrayLength {
                                reason: format!(
                                    "array length expression '{name}' has non-integer type {}",
                                    durable_type_diagnostic_name(ty),
                                ),
                            },
                        ),
                    );
                }
                let Some(candidate) = self.candidate(scope, name, DefinitionKind::Const)? else {
                    return Self::provider_failure(format!("unknown array length `{name}`"));
                };
                let resolution = self.const_resolution(candidate)?;
                let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                    value,
                    ..
                } = resolution
                else {
                    return Self::provider_failure(format!(
                        "array length `{name}` is not an integer"
                    ));
                };
                crate::durable_comptime::durable_named_array_length_const(&value)
                    .map(Some)
                    .map_err(|error| {
                        rue_air::SemanticProviderError::Failure(
                            durable_provider_named_array_length_failure(name, error),
                        )
                    })
            }
        }
    }

    fn array_length_from_value(
        &mut self,
        _scope: &ModuleId,
        value: &crate::durable_semantics::DurableConstValue,
    ) -> rue_air::SemanticProviderResult<
        Option<u64>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        let crate::durable_semantics::DurableConstValue::Integer(value) = value else {
            return Self::provider_failure("array length is not an integer");
        };
        let value = *value;
        u64::try_from(value).map(Some).map_err(|_| {
            rue_air::SemanticProviderError::Failure(durable_literal_array_length_failure(value))
        })
    }

    fn array_type(
        &mut self,
        element: crate::durable_semantics::DurableType,
        length: Option<u64>,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(match length {
            Some(len) => crate::durable_semantics::DurableType::Array {
                element: Arc::new(element),
                len,
            },
            None => crate::durable_semantics::DurableType::ComptimeType,
        })
    }
    fn ptr_const_type(
        &mut self,
        pointee: crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(crate::durable_semantics::DurableType::PtrConst(Arc::new(
            pointee,
        )))
    }
    fn ptr_mut_type(
        &mut self,
        pointee: crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(crate::durable_semantics::DurableType::PtrMut(Arc::new(
            pointee,
        )))
    }
    fn slice_type(
        &mut self,
        _scope: &ModuleId,
        syntax: &str,
        element: crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(crate::durable_semantics::DurableType::Slice {
            element: Arc::new(element),
            name: Arc::from(syntax),
        })
    }
    fn builtin_type_call(
        &mut self,
        _scope: &ModuleId,
        name: &str,
        arguments: &[rue_air::SemanticValueSyntax<'_>],
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        if name != "Str" {
            return Ok(None);
        }
        let [capacity] = arguments else {
            return Self::provider_failure("Str expects one capacity argument");
        };
        let capacity = match capacity {
            rue_air::SemanticValueSyntax::Integer(capacity) => u64::try_from(*capacity)
                .map_err(|_| Self::provider_failure_value("Str capacity must be an integer"))?,
            rue_air::SemanticValueSyntax::Name(capacity) => capacity
                .parse::<u64>()
                .map_err(|_| Self::provider_failure_value("Str capacity must be an integer"))?,
        };
        self.dependencies.insert(
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.dependency_source.clone(),
                kind: self.dependency_kind,
                target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::BuiltinTypeCallHead(
                    rue_air::BuiltinTypeCallHead::FixedCapacityString,
                ),
            },
        );
        Ok(Some(
            crate::durable_semantics::DurableType::BuiltinNominal {
                name: Arc::from(format!("Str({capacity})")),
                kind: rue_air::SemanticImportNominalKind::Struct,
            },
        ))
    }
    fn root_constructor(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeConstructorHead<
                StableDefinitionKey,
                Arc<str>,
                StableDefinitionKey,
            >,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.constructor_fact(scope, name)
    }
    fn module_constructor(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeConstructorHead<
                StableDefinitionKey,
                Arc<str>,
                StableDefinitionKey,
            >,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.constructor_fact(module, name)
    }
    fn resolve_value_argument(
        &mut self,
        scope: &ModuleId,
        _constructor: &str,
        head: &rue_air::SemanticTypeConstructorHead<
            StableDefinitionKey,
            Arc<str>,
            StableDefinitionKey,
        >,
        parameter_index: usize,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
        syntax: rue_air::SemanticValueSyntax<'_>,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableConstValue,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        use crate::durable_semantics::DurableConstValue as V;
        let syntax = match syntax {
            rue_air::SemanticValueSyntax::Integer(value) => return Ok(V::Integer(value)),
            rue_air::SemanticValueSyntax::Name(syntax) => syntax,
        };
        if syntax == "true" || syntax == "false" {
            return Ok(V::Bool(syntax == "true"));
        }
        if let Some((_, value)) = value_arguments
            .iter()
            .find(|(name, _)| name.as_ref() == syntax)
        {
            return Ok(value.clone());
        }
        if let Some((_, ty)) = type_arguments
            .iter()
            .find(|(name, _)| name.as_ref() == syntax)
        {
            return Ok(V::Type(ty.clone()));
        }
        if let Some(ty) = self.deferred_value_parameters.get(syntax) {
            return match ty {
                crate::durable_semantics::DurableType::I8
                | crate::durable_semantics::DurableType::I16
                | crate::durable_semantics::DurableType::I32
                | crate::durable_semantics::DurableType::I64
                | crate::durable_semantics::DurableType::U8
                | crate::durable_semantics::DurableType::U16
                | crate::durable_semantics::DurableType::U32
                | crate::durable_semantics::DurableType::U64 => Ok(V::Integer(0)),
                crate::durable_semantics::DurableType::Bool => Ok(V::Bool(false)),
                crate::durable_semantics::DurableType::Unit => Ok(V::Unit),
                _ => Self::provider_failure(format!(
                    "comptime parameter `{syntax}` has unsupported declared type {}",
                    durable_type_diagnostic_name(ty),
                )),
            };
        }
        if let Some(value) = self.value_substitutions.get(syntax) {
            return Ok(value.clone());
        }
        if let Some(ty) = self.substitutions.get(syntax) {
            return Ok(V::Type(ty.clone()));
        }
        if let Some(candidate) = self.candidate(scope, syntax, DefinitionKind::Const)? {
            if let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                value, ..
            } = self.const_resolution(candidate)?
            {
                return Ok(*value);
            }
        }
        let parameter = head
            .parameters
            .get(parameter_index)
            .map(|parameter| parameter.name.as_ref())
            .unwrap_or("?");
        Self::provider_failure(format!(
            "argument for comptime parameter `{parameter}` must be a compile-time known value"
        ))
    }
    fn reduce_comptime_call(
        &mut self,
        head: &rue_air::SemanticTypeConstructorHead<
            StableDefinitionKey,
            Arc<str>,
            StableDefinitionKey,
        >,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticComptimeCallResult<
                crate::durable_semantics::DurableType,
                crate::durable_semantics::DurableConstValue,
            >,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        use crate::semantic_query_nucleus::{
            ComptimeCallQueryKey, ComptimeCallResultProjection as P, DeclarationSemanticQueryKey,
            SemanticNucleusKey as K, SemanticNucleusValue as V,
        };
        let declaration = crate::declaration_candidate::DeclarationCandidateKey {
            module: head.key.module().clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name: Arc::from(head.key.name()),
            owner: None,
            duplicate_discriminator: 0,
        };
        let signature = self.signature(declaration.clone())?;
        let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
            parameters,
            ..
        } = signature
        else {
            return Self::provider_failure("type constructor has a non-callable signature");
        };
        let concrete_type_arguments = type_arguments
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>();
        for (name, value) in value_arguments {
            let Some((_, parameter)) = head
                .parameters
                .iter()
                .zip(parameters.iter())
                .find(|(header, _)| &header.name == name)
            else {
                return Self::provider_failure("comptime value argument has no parameter");
            };
            let expected = substitute_durable_generics(&parameter.ty, &concrete_type_arguments);
            if let Some(failure) =
                crate::durable_comptime::durable_structured_value_fit_failure(value, &expected)
            {
                return Self::provider_domain_failure(failure);
            }
        }
        let query = K::ComptimeCall(ComptimeCallQueryKey {
            declaration: DeclarationSemanticQueryKey {
                declaration,
                configuration: self.configuration.clone(),
            },
            type_arguments: type_arguments.to_vec().into(),
            value_arguments: value_arguments.to_vec().into(),
        });
        let _depth = SemanticComptimeCallDepthGuard::enter(head.key.name()).map_err(
            |error| match error {
                EvaluateSemanticConstError::Failure(failure) => {
                    rue_air::SemanticProviderError::Failure(*failure)
                }
                EvaluateSemanticConstError::Abort(abort) => {
                    rue_air::SemanticProviderError::Abort(abort)
                }
            },
        )?;
        let queried = self.query(query)?;
        match queried {
            V::ComptimeCall(value) => {
                let mut effects = crate::durable_comptime::DurableComptimeEffects::default();
                effects.merge_projection(
                    &value.anonymous_nominals,
                    &value.dependencies,
                    &value.deferred_ownership,
                    &crate::durable_comptime::DurableComptimeApplicationPolicy::preserve(),
                );
                self.merge_comptime_effects(
                    effects,
                    &crate::durable_comptime::DurableComptimeApplicationPolicy::preserve(),
                );
                match value.result {
                    P::Type(value) => Ok(Some(rue_air::SemanticComptimeCallResult::Type(value))),
                    P::Value(value) => Ok(Some(rue_air::SemanticComptimeCallResult::Value(value))),
                }
            }
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("comptime query returned the wrong projection"),
        }
    }
}

pub(super) enum ResolveSemanticSignatureError {
    Abort(QueryAbort),
    Failure(Box<crate::semantic_query_nucleus::SemanticNucleusFailure>),
}

impl ResolveSemanticSignatureError {
    pub(super) fn failure(failure: crate::semantic_query_nucleus::SemanticNucleusFailure) -> Self {
        Self::Failure(Box::new(failure))
    }
}

pub(super) fn semantic_type_query_failure(
    failure: rue_air::SemanticTypeSyntaxError<
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
        StableDefinitionKey,
        Arc<str>,
    >,
) -> ResolveSemanticSignatureError {
    use rue_air::SemanticTypeSyntaxFailure as F;
    use rue_error::ErrorKind;

    match crate::durable_comptime::classify_durable_type_syntax_failure(failure) {
        crate::durable_comptime::DurableTypeSyntaxClassification::Abort(abort) => {
            ResolveSemanticSignatureError::Abort(abort)
        }
        crate::durable_comptime::DurableTypeSyntaxClassification::Failure(failure) => {
            ResolveSemanticSignatureError::failure(failure)
        }
        crate::durable_comptime::DurableTypeSyntaxClassification::Semantic(failure) => {
            match failure {
                F::UnknownType { syntax } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::UnknownType(syntax.to_string()),
                    ),
                ),
                F::UnknownModuleMember { module, member, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            ErrorKind::UnknownModuleMember {
                                module_name: module.to_string(),
                                member_name: member.to_string(),
                            },
                        ),
                    )
                }
                F::ValueWhereTypeExpected { parameter, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                            Arc::from(format!(
                                "argument for comptime parameter `{parameter}` must be a type"
                            )),
                        ),
                    )
                }
                F::UnknownConstructor {
                    constructor,
                    expectation: rue_air::SemanticComptimeCallExpectation::Type,
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::UnknownType(format!("{constructor}(...)")),
                    ),
                ),
                F::UnknownConstructor {
                    constructor,
                    expectation: rue_air::SemanticComptimeCallExpectation::Value,
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "`{constructor}` is not a function; a compile-time value call requires a value-returning comptime function"
                            ),
                        },
                    ),
                ),
                F::InvalidConstructorArity {
                    constructor,
                    expected,
                    found,
                    expectation: rue_air::SemanticComptimeCallExpectation::Type,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                        format!(
                            "type constructor `{constructor}` expects {expected} comptime type argument(s), but {found} provided"
                        ),
                    )),
                ),
                F::InvalidConstructorArity {
                    constructor,
                    expected,
                    found,
                    expectation: rue_air::SemanticComptimeCallExpectation::Value,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "value-returning comptime function `{constructor}` expects {expected} comptime {}, but {found} {} provided",
                                if expected == 1 {
                                    "argument"
                                } else {
                                    "arguments"
                                },
                                if found == 1 { "was" } else { "were" },
                            ),
                        },
                    ),
                ),
                F::NotTypeConstructor { constructor, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                            Arc::from(format!("function `{constructor}` is not a type")),
                        ),
                    )
                }
                F::TypeWhereValueExpected { constructor, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "`{constructor}` returns `type` and cannot be used where a compile-time value is required"
                                ),
                            },
                        ),
                    )
                }
                F::RuntimeConstructorParameter {
                    constructor,
                    expectation: rue_air::SemanticComptimeCallExpectation::Type,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                        format!(
                            "type constructor `{constructor}` cannot have runtime parameters; all parameters must be `comptime`"
                        ),
                    )),
                ),
                F::RuntimeConstructorParameter {
                    constructor,
                    expectation: rue_air::SemanticComptimeCallExpectation::Value,
                    expected,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: if expected == 0 {
                                format!(
                                    "call `{constructor}(...)` is not a compile-time value because its callee must declare at least one `comptime` parameter"
                                )
                            } else {
                                format!(
                                    "call `{constructor}(...)` is not a compile-time value because all parameters must be `comptime`"
                                )
                            },
                        },
                    ),
                ),
                F::ConstructorDidNotReduce { constructor, .. } => {
                    ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "the type constructor `{constructor}` did not reduce to a concrete type at compile time"
                                ),
                            },
                        ),
                    )
                }
                F::PrivateItem {
                    kind,
                    name,
                    defining_file,
                    ..
                } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::PrivateUnqualifiedAccess(Box::new(
                            rue_error::PrivateUnqualifiedAccessData {
                                item_kind: kind.diagnostic_name().to_owned(),
                                name: name.to_string(),
                                defining_file: defining_file.to_string(),
                            },
                        )),
                    ),
                ),
                F::AmbiguousItem { name, .. } => ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!("type resolution is ambiguous for `{name}`"),
                        },
                    ),
                ),
                F::Path(path) => match path {
                    rue_air::SemanticModulePathFailure::Empty => {
                        ResolveSemanticSignatureError::failure(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "type path is empty".to_owned(),
                                },
                            ),
                        )
                    }
                    rue_air::SemanticModulePathFailure::UnknownRoot { name } => {
                        ResolveSemanticSignatureError::failure(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                                ErrorKind::UnknownType(name.to_string()),
                            ),
                        )
                    }
                    rue_air::SemanticModulePathFailure::UnknownMember {
                        module, member, ..
                    } => ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            ErrorKind::UnknownModuleMember {
                                module_name: module.to_string(),
                                member_name: member.to_string(),
                            },
                        ),
                    ),
                    rue_air::SemanticModulePathFailure::PrivateMember { member, .. } => {
                        ResolveSemanticSignatureError::failure(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: format!(
                                        "private module member `{member}` cannot be used in a type path"
                                    ),
                                },
                            ),
                        )
                    }
                },
            }
        }
    }
}

pub(super) fn resolve_parsed_semantic_signature(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    module: &ModuleId,
    parsed: &crate::semantic_query_nucleus::ParsedSemanticSignature,
) -> Result<
    crate::semantic_query_nucleus::DeclarationSignatureProjection,
    ResolveSemanticSignatureError,
> {
    use crate::durable_semantics::{DurableParameterMode as M, DurableSemanticParameter};
    use crate::semantic_query_nucleus::{
        DeclarationSignatureProjection as Output, ParsedSemanticSignature as Input,
    };

    pub(super) fn contains_slice(ty: &crate::durable_semantics::DurableType) -> bool {
        use crate::durable_semantics::DurableType as T;
        match ty {
            T::Slice { .. } => true,
            T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
                contains_slice(element)
            }
            _ => false,
        }
    }

    let diagnostic = |kind| {
        ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(kind),
        )
    };

    let resolve = |provider: &mut SemanticNucleusTypeProvider<'_>,
                   syntax: &rue_rir::RirTypeSyntaxArena<Arc<str>>,
                   root: rue_rir::RirTypeSyntaxRef,
                   kind: rue_air::DeclarationTypeDependencyKind| {
        provider.dependency_kind = kind;
        rue_air::resolve_structured_semantic_type_syntax(provider, module, syntax, root)
            .map_err(semantic_type_query_failure)
    };
    match parsed {
        Input::Callable {
            syntax,
            parameters,
            result,
            has_self,
            self_mode,
            is_unchecked,
            is_extern,
            is_c_export,
            is_accessor,
            accessor_result_mode,
            accessor_body,
            accessor_cycle,
            ..
        } => {
            if *is_accessor {
                // 6.6:3-6.6:7 over the exact canonical declaration. Which forms are
                // illegal, in which order, and how each diagnostic reads are
                // `rue_air::declaration_validation`'s, shared with the RIR
                // producers (RUE-1232); this seam owns only the lowering of
                // the parsed signature projection into that vocabulary.
                use rue_air::declaration_validation as rules;
                use rue_air::declaration_validation::{
                    AccessorParameterForm, AccessorReceiverForm,
                };
                let receiver = if provider.dependency_source.owner().is_none() {
                    AccessorReceiverForm::FreeFunction
                } else if !*has_self {
                    AccessorReceiverForm::AssociatedFunction
                } else {
                    match self_mode {
                        crate::declaration_candidate::DeclarationParameterMode::Borrow => {
                            AccessorReceiverForm::BorrowSelf
                        }
                        crate::declaration_candidate::DeclarationParameterMode::Inout => {
                            AccessorReceiverForm::InoutSelf
                        }
                        crate::declaration_candidate::DeclarationParameterMode::Value => {
                            AccessorReceiverForm::ValueSelf
                        }
                    }
                };
                if let Some(violation) = rules::accessor_signature_for_mode(
                    receiver,
                    *accessor_result_mode
                        == crate::declaration_candidate::DeclarationParameterMode::Inout,
                    parameters.iter().map(|parameter| {
                        if parameter.is_comptime {
                            return AccessorParameterForm::Comptime;
                        }
                        match parameter.mode {
                            crate::declaration_candidate::DeclarationParameterMode::Value => {
                                AccessorParameterForm::ByValue
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Borrow => {
                                AccessorParameterForm::Borrow
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Inout => {
                                AccessorParameterForm::Inout
                            }
                        }
                    }),
                ) {
                    use rue_air::declaration_validation::AccessorSignatureViolation as Violation;
                    return Err(match violation {
                        Violation::Receiver {
                            kind,
                            note: Some(note),
                        } => ResolveSemanticSignatureError::failure(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticWithNote {
                                kind,
                                note: Arc::from(note),
                            },
                        ),
                        Violation::Receiver { kind, note: None }
                        | Violation::Parameter { kind, .. } => diagnostic(kind),
                    });
                }
                // 6.6:6 and 6.6:7 over the accessor's own retained body. These
                // are legality rules on the declaration, so they hold with no
                // call site anywhere in the program (RUE-1212); see
                // `AccessorBodyVerdict` for the single link they leave to the
                // demanded path.
                if let Some(kind) = rules::accessor_body_error(accessor_body) {
                    return Err(diagnostic(kind));
                }
                // 6.6:14 over the owner's retained `self`-call edges: an
                // accessor cycle has no finite expansion, so it too is a
                // legality rule on the declaration (RUE-1282). Exotic edges
                // through a non-`self` receiver stay with the demanded-path
                // checks.
                if let Some(method) = accessor_cycle {
                    return Err(ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticWithNote {
                            kind: rules::accessor_recursion_error(method),
                            note: Arc::from(rules::ACCESSOR_RECURSION_NOTE),
                        },
                    ));
                }
            }
            let mut generic_index = 0_u32;
            for parameter in parameters.iter() {
                if parameter.is_comptime && parsed.is_type_parameter_syntax(parameter.ty) {
                    provider.substitutions.insert(
                        Arc::from(parsed.symbol(parameter.name)),
                        crate::durable_semantics::DurableType::GenericParameter(generic_index),
                    );
                    generic_index += 1;
                }
            }
            let parameters = parameters
                .iter()
                .map(|parameter| {
                    let ty = resolve(
                        provider,
                        syntax,
                        parameter.ty,
                        rue_air::DeclarationTypeDependencyKind::Signature,
                    )?;
                    if parameter.is_comptime && !parsed.is_type_parameter_syntax(parameter.ty) {
                        provider
                            .deferred_value_parameters
                            .insert(Arc::from(parsed.symbol(parameter.name)), ty.clone());
                    }
                    Ok(DurableSemanticParameter {
                        name: Arc::from(parsed.symbol(parameter.name)),
                        ty,
                        mode: match parameter.mode {
                            crate::declaration_candidate::DeclarationParameterMode::Value => {
                                M::Value
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Borrow => {
                                M::Borrow
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Inout => {
                                M::Inout
                            }
                        },
                        is_comptime: parameter.is_comptime,
                    })
                })
                .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?;
            let result = resolve(
                provider,
                syntax,
                *result,
                rue_air::DeclarationTypeDependencyKind::Signature,
            )?;
            if contains_slice(&result) {
                return Err(diagnostic(rue_error::ErrorKind::SliceReturnNotAllowed));
            }
            if (*is_extern || *is_c_export)
                && !provider
                    .configuration
                    .preview_features
                    .contains(rue_error::PreviewFeature::CFfi)
            {
                return Err(diagnostic(rue_error::ErrorKind::PreviewFeatureRequired {
                    feature: rue_error::PreviewFeature::CFfi,
                    what: if *is_extern {
                        "an `extern \"C\"` foreign declaration".to_owned()
                    } else {
                        "a `pub extern \"C\" fn` export".to_owned()
                    },
                }));
            }
            if *is_extern || *is_c_export {
                let check =
                    |provider: &mut SemanticNucleusTypeProvider<'_>,
                     ty: &crate::durable_semantics::DurableType| {
                        use crate::durable_semantics::DurableType as T;
                        if matches!(ty, T::Array { .. }) {
                            return Err(diagnostic(rue_error::ErrorKind::ExternArrayByValue {
                                ty: durable_type_diagnostic_name(ty),
                            }));
                        }
                        if let T::Nominal(key) = ty
                            && key.kind() == crate::StableDefinitionKind::Struct
                        {
                            let failure = provider.ffi_shape_failure(ty, &mut Vec::new()).map_err(
                                |error| match error {
                                    rue_air::SemanticProviderError::Abort(abort) => {
                                        ResolveSemanticSignatureError::Abort(abort)
                                    }
                                    rue_air::SemanticProviderError::Failure(failure) => {
                                        ResolveSemanticSignatureError::failure(failure)
                                    }
                                },
                            )?;
                            if failure.as_ref().is_some_and(|(reason, _, _)| {
                                *reason == rue_air::FfiRejectReason::NonReprCAggregate
                            }) {
                                return Err(diagnostic(
                                    rue_error::ErrorKind::ExternAggregateNotReprC {
                                        ty: durable_type_diagnostic_name(ty),
                                    },
                                ));
                            }
                            if failure.is_some() {
                                return Err(diagnostic(
                                    rue_error::ErrorKind::ExternSignatureTypeUnsupported {
                                        ty: durable_type_diagnostic_name(ty),
                                    },
                                ));
                            }
                            return Ok(());
                        }
                        if !matches!(
                            ty,
                            T::I8
                                | T::I16
                                | T::I32
                                | T::I64
                                | T::U8
                                | T::U16
                                | T::U32
                                | T::U64
                                | T::Bool
                                | T::PtrConst(_)
                                | T::PtrMut(_)
                        ) {
                            return Err(diagnostic(
                                rue_error::ErrorKind::ExternSignatureTypeUnsupported {
                                    ty: durable_type_diagnostic_name(ty),
                                },
                            ));
                        }
                        Ok(())
                    };
                for parameter in &parameters {
                    check(provider, &parameter.ty)?;
                }
                if result != crate::durable_semantics::DurableType::Unit {
                    check(provider, &result)?;
                }
            }
            if *is_c_export {
                let name = provider.dependency_source.name().to_owned();
                let reject = |reason| {
                    diagnostic(rue_error::ErrorKind::ExportSignatureUnsupported {
                        name: name.clone(),
                        reason,
                    })
                };
                if name == "main" {
                    return Err(reject("an export named `main` collides with the program entry point; give it a different C name".to_owned()));
                }
                if parameters.iter().any(|parameter| parameter.is_comptime) {
                    return Err(reject("a generic function has no single C symbol; export a concrete (non-`comptime`) function".to_owned()));
                }
                if let Some((index, _)) = parameters
                    .iter()
                    .enumerate()
                    .find(|(_, parameter)| parameter.mode != M::Value)
                {
                    return Err(reject(format!(
                        "parameter {} uses a by-reference (`borrow`/`inout`) mode, which does not cross a C boundary; pass a raw pointer instead",
                        index + 1
                    )));
                }
                if let Some(parameter) = parameters.iter().find(|parameter| {
                    matches!(
                        parameter.ty,
                        crate::durable_semantics::DurableType::Nominal(_)
                            | crate::durable_semantics::DurableType::Array { .. }
                    )
                }) {
                    return Err(reject(format!(
                        "aggregate parameter `{}` is not supported by the P4 export thunk (register repacking across the export boundary is future work); pass a pointer instead",
                        durable_type_diagnostic_name(&parameter.ty)
                    )));
                }
                if matches!(
                    result,
                    crate::durable_semantics::DurableType::Nominal(_)
                        | crate::durable_semantics::DurableType::Array { .. }
                ) {
                    return Err(reject(format!(
                        "aggregate return `{}` is not supported by the P4 export thunk",
                        durable_type_diagnostic_name(&result)
                    )));
                }
                if parameters.len() > 6 {
                    return Err(reject(format!(
                        "{} scalar parameters exceed the 6-register argument budget the P4 export thunk supports; reduce the parameter count",
                        parameters.len()
                    )));
                }
            }
            Ok(Output::Callable {
                parameters: parameters.into(),
                result,
                has_self: *has_self,
                self_mode: match self_mode {
                    crate::declaration_candidate::DeclarationParameterMode::Value => M::Value,
                    crate::declaration_candidate::DeclarationParameterMode::Borrow => M::Borrow,
                    crate::declaration_candidate::DeclarationParameterMode::Inout => M::Inout,
                },
                is_accessor: *is_accessor,
                accessor_result_mode: match accessor_result_mode {
                    crate::declaration_candidate::DeclarationParameterMode::Value => M::Value,
                    crate::declaration_candidate::DeclarationParameterMode::Borrow => M::Borrow,
                    crate::declaration_candidate::DeclarationParameterMode::Inout => M::Inout,
                },
                is_unchecked: *is_unchecked,
                is_extern: *is_extern,
                is_c_export: *is_c_export,
            })
        }
        Input::Struct {
            syntax,
            fields,
            is_copy,
            is_linear,
            is_repr_c,
            ..
        } => {
            if let Some(kind) = rue_air::declaration_validation::linear_copy_struct(
                provider.dependency_source.name(),
                *is_linear,
                *is_copy,
            ) {
                return Err(diagnostic(kind));
            }
            if let Some(kind) = rue_air::declaration_validation::duplicate_field(
                provider.dependency_source.name(),
                fields.iter().map(|field| parsed.symbol(field.name)),
            ) {
                return Err(diagnostic(kind));
            }
            let fields = fields
                .iter()
                .map(|field| {
                    let name: Arc<str> = Arc::from(parsed.symbol(field.name));
                    Ok((
                        name,
                        resolve(
                            provider,
                            syntax,
                            field.ty,
                            rue_air::DeclarationTypeDependencyKind::Field,
                        )?,
                    ))
                })
                .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?;
            if fields.iter().any(|(_, ty)| contains_slice(ty)) {
                return Err(diagnostic(rue_error::ErrorKind::SliceInAggregateField));
            }
            if fields
                .iter()
                .any(|(_, ty)| *ty == crate::durable_semantics::DurableType::ComptimeType)
            {
                return Err(ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                        "type values cannot exist at runtime",
                    )),
                ));
            }
            if *is_copy {
                for (field_name, field_ty) in &fields {
                    if !provider
                        .type_is_copy(field_ty)
                        .map_err(|error| match error {
                            rue_air::SemanticProviderError::Abort(abort) => {
                                ResolveSemanticSignatureError::Abort(abort)
                            }
                            rue_air::SemanticProviderError::Failure(failure) => {
                                ResolveSemanticSignatureError::failure(failure)
                            }
                        })?
                    {
                        return Err(diagnostic(rue_error::ErrorKind::CopyStructNonCopyField(
                            Box::new(rue_error::CopyStructNonCopyFieldError {
                                struct_name: provider.dependency_source.name().to_owned(),
                                field_name: field_name.to_string(),
                                field_type: durable_type_diagnostic_name(field_ty),
                            }),
                        )));
                    }
                }
            }
            if *is_repr_c {
                if !provider
                    .configuration
                    .preview_features
                    .contains(rue_error::PreviewFeature::CFfi)
                {
                    return Err(diagnostic(rue_error::ErrorKind::PreviewFeatureRequired {
                        feature: rue_error::PreviewFeature::CFfi,
                        what: "the `@repr(c)` representation marker".to_owned(),
                    }));
                }
                let has_destructor = provider
                    .candidate(
                        module,
                        provider.dependency_source.name(),
                        DefinitionKind::Destructor,
                    )
                    .map_err(|error| match error {
                        rue_air::SemanticProviderError::Abort(abort) => {
                            ResolveSemanticSignatureError::Abort(abort)
                        }
                        rue_air::SemanticProviderError::Failure(failure) => {
                            ResolveSemanticSignatureError::failure(failure)
                        }
                    })?
                    .is_some();
                if let Some((reason, path, failing)) = provider
                    .repr_c_failure_for_fields(&fields, *is_linear, has_destructor)
                    .map_err(|error| match error {
                        rue_air::SemanticProviderError::Abort(abort) => {
                            ResolveSemanticSignatureError::Abort(abort)
                        }
                        rue_air::SemanticProviderError::Failure(failure) => {
                            ResolveSemanticSignatureError::failure(failure)
                        }
                    })?
                {
                    let field_path = path.join(".");
                    let reason = if field_path.is_empty() {
                        reason.describe().to_owned()
                    } else {
                        format!(
                            "field `{field_path}` of type `{}` — {}",
                            durable_type_diagnostic_name(&failing),
                            reason.describe()
                        )
                    };
                    return Err(diagnostic(rue_error::ErrorKind::ReprCStructIneligible(
                        Box::new(rue_error::ReprCIneligibleError {
                            struct_name: provider.dependency_source.name().to_owned(),
                            field_path,
                            failing_type: durable_type_diagnostic_name(&failing),
                            reason,
                        }),
                    )));
                }
            }
            Ok(Output::Struct {
                fields: fields.into(),
                is_copy: *is_copy,
                is_linear: *is_linear,
                is_repr_c: *is_repr_c,
            })
        }
        Input::Enum {
            syntax,
            variants,
            payloads,
            is_non_exhaustive,
            is_public,
            non_exhaustive_range,
            ..
        } => {
            if *is_non_exhaustive && !*is_public {
                let kind = rue_error::ErrorKind::ParseError(
                    "@non_exhaustive can only be applied to public enums".to_string(),
                );
                return Err(match non_exhaustive_range {
                    Some((start, end)) => ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticAtProducerRange {
                            kind,
                            producer: declaration_candidate_for_stable_key(
                                &provider.dependency_source,
                            )
                            .expect("enum signature has a declaration candidate"),
                            start: *start,
                            end: *end,
                        },
                    ),
                    None => diagnostic(kind),
                });
            }
            if *is_non_exhaustive
                && !provider
                    .configuration
                    .preview_features
                    .contains(rue_error::PreviewFeature::NonExhaustiveEnums)
            {
                let kind = rue_error::ErrorKind::PreviewFeatureRequired {
                    feature: rue_error::PreviewFeature::NonExhaustiveEnums,
                    what: "@non_exhaustive enums".to_owned(),
                };
                return Err(match non_exhaustive_range {
                    Some((start, end)) => ResolveSemanticSignatureError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticAtProducerRange {
                            kind,
                            producer: declaration_candidate_for_stable_key(
                                &provider.dependency_source,
                            )
                            .expect("enum signature has a declaration candidate"),
                            start: *start,
                            end: *end,
                        },
                    ),
                    None => diagnostic(kind),
                });
            }
            if let Some(kind) = rue_air::declaration_validation::duplicate_variant(
                provider.dependency_source.name(),
                variants.iter().map(|variant| parsed.symbol(variant.name)),
            ) {
                return Err(diagnostic(kind));
            }
            let variants: Vec<(Arc<str>, Arc<[crate::durable_semantics::DurableType]>)> = variants
                .iter()
                .map(|variant| {
                    let payload = payloads
                        .get(variant.payload_start as usize..variant.payload_end as usize)
                        .expect("signature payload ranges are validated when projected");
                    Ok((
                        Arc::from(parsed.symbol(variant.name)),
                        payload
                            .iter()
                            .map(|root| {
                                resolve(
                                    provider,
                                    syntax,
                                    *root,
                                    rue_air::DeclarationTypeDependencyKind::Payload,
                                )
                            })
                            .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?
                            .into(),
                    ))
                })
                .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?;
            if variants
                .iter()
                .flat_map(|(_, payload)| payload.iter())
                .any(contains_slice)
            {
                return Err(diagnostic(rue_error::ErrorKind::SliceInAggregateField));
            }
            Ok(Output::Enum {
                variants: variants.into(),
                is_non_exhaustive: *is_non_exhaustive,
            })
        }
        Input::Destructor => Ok(Output::Destructor),
    }
}
#[derive(Clone)]
pub(super) struct BodyInputResolver {
    pub(super) stable_declaration_classifications: QueryFamily<
        StableDeclarationClassificationQueryKey,
        StableDeclarationClassificationQueryValue,
    >,
    pub(crate) declaration_shells:
        QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    pub(super) declaration_body_plan_artifacts:
        QueryFamily<DeclarationBodyPlanQueryKey, DeclarationBodyPlanArtifactsValue>,
    pub(super) body_source_bases:
        QueryFamily<crate::body_query::BodyQueryKey, Option<crate::body_query::BodySourceLocator>>,
}

impl BodyInputResolver {
    pub(super) fn select(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
    ) -> Result<
        Result<
            (
                StableDefinitionKey,
                crate::declaration_candidate::DeclarationCandidateKey,
            ),
            crate::body_query::BodyInputIncomplete,
        >,
        QueryAbort,
    > {
        use crate::body_query::BodyInputIncomplete as Incomplete;

        let Some(definition) = body_source_definition_key(&key.instance).cloned() else {
            return Ok(Err(Incomplete::UnsupportedInstance));
        };
        let classification = match context.query_registered(
            &self.stable_declaration_classifications,
            StableDeclarationClassificationQueryKey(definition.clone()),
        ) {
            Ok(value) => value,
            Err(QueryAbort::MissingInput(_)) => {
                return Ok(Err(Incomplete::MissingPrerequisite(Arc::from(
                    "stable declaration classification",
                ))));
            }
            Err(abort) => return Err(abort),
        };
        let candidate = match classification.outcome() {
            rue_query::QueryOutcome::Success(
                StableDeclarationClassificationQueryValue::Selected(candidate),
            ) => candidate.clone(),
            _ => {
                return Ok(Err(Incomplete::MissingPrerequisite(Arc::from(
                    "stable declaration candidate",
                ))));
            }
        };
        Ok(Ok((definition, candidate)))
    }

    pub(super) fn resolve_selected_artifact(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
        definition: StableDefinitionKey,
        candidate: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<crate::body_query::BodyInputValue, QueryAbort> {
        use crate::body_query::{BodyInputIncomplete as Incomplete, BodyInputValue};

        let artifacts = match context.query_registered(
            &self.declaration_body_plan_artifacts,
            DeclarationBodyPlanQueryKey(candidate),
        ) {
            Ok(value) => value,
            Err(QueryAbort::MissingInput(_)) => {
                return Ok(BodyInputValue::Incomplete(Incomplete::MissingPrerequisite(
                    Arc::from("declaration body plan"),
                )));
            }
            Err(abort) => return Err(abort),
        };
        let rue_query::QueryOutcome::Success(artifacts) = artifacts.outcome() else {
            unreachable!("DeclarationBodyPlanArtifacts publishes typed values")
        };
        let artifacts = match artifacts {
            DeclarationBodyPlanArtifactsValue::Available(artifacts) => artifacts,
            DeclarationBodyPlanArtifactsValue::Failure(failure) => {
                return Ok(BodyInputValue::Incomplete(Incomplete::BodyPlanFailure(
                    failure.clone(),
                )));
            }
        };
        let locator = context.query_registered(&self.body_source_bases, key.clone())?;
        let rue_query::QueryOutcome::Success(Some(locator)) = locator.outcome() else {
            return Ok(BodyInputValue::Incomplete(Incomplete::MissingPrerequisite(
                Arc::from("body source basis"),
            )));
        };
        Ok(BodyInputValue::Available(
            crate::body_query::OwnedBodyInput {
                owner: definition,
                source: locator.clone(),
                artifacts: artifacts.clone(),
            },
        ))
    }

    pub(super) fn resolve_producer_artifact(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
    ) -> Result<crate::body_query::BodyInputValue, QueryAbort> {
        let (definition, candidate) = match self.select(context, key)? {
            Ok(selected) => selected,
            Err(incomplete) => {
                return Ok(crate::body_query::BodyInputValue::Incomplete(incomplete));
            }
        };
        self.resolve_selected_artifact(context, key, definition, candidate)
    }

    pub(super) fn resolve(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
    ) -> Result<crate::body_query::BodyInputValue, QueryAbort> {
        use crate::body_query::{BodyInputIncomplete as Incomplete, BodyInputValue};

        let (definition, candidate) = match self.select(context, key)? {
            Ok(selected) => selected,
            Err(incomplete) => return Ok(BodyInputValue::Incomplete(incomplete)),
        };
        if !definition.kind().owns_body() {
            return Ok(BodyInputValue::Incomplete(Incomplete::UnsupportedKind(
                definition.kind(),
            )));
        }
        let shell = match context.query_registered(
            &self.declaration_shells,
            DeclarationShellQueryKey(candidate.clone()),
        ) {
            Ok(value) => value,
            Err(QueryAbort::MissingInput(_)) => {
                return Ok(BodyInputValue::Incomplete(Incomplete::MissingPrerequisite(
                    Arc::from("declaration shell"),
                )));
            }
            Err(abort) => return Err(abort),
        };
        let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(shell)) =
            shell.outcome()
        else {
            return Ok(BodyInputValue::Incomplete(Incomplete::MissingPrerequisite(
                Arc::from("declaration shell"),
            )));
        };
        if shell.is_extern
            || candidate.category
                == crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction
        {
            return Ok(BodyInputValue::Incomplete(Incomplete::Extern));
        }
        if shell.is_generic && matches!(key.instance, crate::FunctionInstanceKey::Definition(_)) {
            use crate::declaration_candidate::DeclarationCandidateCategory as Category;

            let named_runtime_value_body = matches!(
                candidate.category,
                Category::Method | Category::AssociatedFunction
            ) && shell
                .parameters
                .iter()
                .all(|parameter| !parameter.is_comptime || !parameter.is_type_parameter);
            if !named_runtime_value_body {
                return Ok(BodyInputValue::Incomplete(Incomplete::Generic));
            }
        }
        self.resolve_selected_artifact(context, key, definition, candidate)
    }
}

/// The revision-scoped owner of the shared symbol equality space (ADR-0076).
///
/// One append-only interner serves every body of one semantic revision, so a
/// name in the program's nominal closure is interned once rather than once per
/// body. A generation is retired when its revision falls out of the window
/// below; a body still carrying the retired generation fails
/// `require_rir_authority` and re-runs, so a superseded equality space is never
/// silently reused.
///
/// The window holds more than one revision on purpose. Retiring the previous
/// generation on every mint would make two concurrently pinned revisions retire
/// each other's space and abandon each other's bodies without progressing;
/// keeping the recent few makes that need more simultaneously live revisions
/// than the engine pins, while still bounding how many interners are resident.
#[derive(Debug)]
pub(super) struct RevisionSymbolSpace {
    live: Mutex<VecDeque<(Revision, rue_rir::SharedSymbolSpace)>>,
    generations: rue_rir::SymbolSpaceGenerations,
    max_entries: usize,
}

impl Default for RevisionSymbolSpace {
    fn default() -> Self {
        Self::with_owner_bound(rue_lexer::MAX_INTERNED_STRINGS)
    }
}

impl RevisionSymbolSpace {
    /// How many revisions' equality spaces stay live at once.
    pub(super) const WINDOW: usize = 4;

    pub(super) fn with_owner_bound(max_entries: usize) -> Self {
        Self {
            live: Mutex::new(VecDeque::new()),
            generations: rue_rir::SymbolSpaceGenerations::default(),
            max_entries,
        }
    }

    /// The live generation for `revision`, minting it if this revision has no
    /// live generation yet.
    pub(super) fn generation(&self, revision: Revision) -> rue_rir::SharedSymbolSpace {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, space)) = live.iter().find(|(pinned, _)| *pinned == revision) {
            return space.clone();
        }
        let space = self
            .generations
            .next_generation_with_owner_bound(self.max_entries);
        live.push_back((revision, space.clone()));
        while live.len() > Self::WINDOW {
            if let Some((_, evicted)) = live.pop_front() {
                evicted.supersede();
            }
        }
        space
    }
}

pub(super) struct BodyTransactionEvaluator {
    pub(super) parse_modules: QueryFamily<ModuleQueryKey, ParseModuleValue>,
    pub(crate) module_source_bases:
        QueryFamily<ModuleQueryKey, Option<rue_air::DurableBodySourceLocator>>,
    pub(super) body_input: BodyInputResolver,
    pub(super) body_toolchain_demands:
        QueryFamily<crate::body_query::BodyQueryKey, crate::BodyToolchainDemand>,
    pub(super) body_produced_anonymous:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::ProducedAnonymous>,
    pub(super) semantic_nucleus: SemanticNucleusFamily,
    pub(super) stable_declaration_classifications: QueryFamily<
        StableDeclarationClassificationQueryKey,
        StableDeclarationClassificationQueryValue,
    >,
    pub(crate) declaration_shells:
        QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    pub(super) lookup_names: QueryFamily<LookupNameKey, LookupNameValue>,
    pub(super) lookup_imports: QueryFamily<LookupImportKey, LookupImportValue>,
    pub(super) provider_observation_meter: Arc<ProviderObservationCounters>,
    pub(super) lookup_root_lease: Arc<Mutex<PublishedRootLookupLease>>,
    pub(super) runtime: QueryRuntime,
    pub(super) shared_durable_payloads: Arc<SharedDurablePayloadCache>,
    /// The ADR-0076 revision-shared symbol space. Every body of one semantic
    /// revision decodes its RIR into, and analyzes against, one append-only
    /// interner, so the program's nominal closure is interned once per
    /// revision instead of once per body.
    pub(super) symbol_space: RevisionSymbolSpace,
    #[cfg(test)]
    pub(super) inject_body_transaction_failure: Arc<std::sync::atomic::AtomicBool>,
}

impl BodyTransactionEvaluator {
    pub(super) fn body_plan_failure(
        definition: &crate::StableDefinitionKey,
        failure: &DeclarationBodyPlanFailure,
    ) -> crate::body_query::BodyTransaction {
        let errors = match failure {
            DeclarationBodyPlanFailure::CandidateRirRejected(errors) => errors.clone(),
            DeclarationBodyPlanFailure::Build(kind) => {
                crate::CompileErrors::from(crate::CompileError::without_span(kind.clone()))
            }
            failure => crate::CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InternalError(format!(
                    "canonical body plan failed for {definition:?}: {failure:?}"
                )),
            )),
        };
        crate::body_query::BodyTransaction::DeterministicFailure {
            diagnostic_basis: None,
            errors,
            references: crate::body_query::BodyReferences(Arc::from([])),
            lookup_observations: crate::body_query::BodyLookupObservations::default(),
        }
    }

    pub(super) fn lowering_failure(
        definition: &crate::StableDefinitionKey,
        detail: impl std::fmt::Display,
    ) -> crate::body_query::BodyTransaction {
        crate::body_query::BodyTransaction::DeterministicFailure {
            diagnostic_basis: None,
            errors: crate::CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InternalError(format!(
                    "owned body input lowering failed for {definition:?}: {detail}"
                )),
            )),
            references: crate::body_query::BodyReferences(Arc::from([])),
            lookup_observations: crate::body_query::BodyLookupObservations::default(),
        }
    }

    pub(super) fn lowering_build_failure(
        error: &rue_rir::RirPayloadBuildError,
    ) -> crate::body_query::BodyTransaction {
        crate::body_query::BodyTransaction::DeterministicFailure {
            diagnostic_basis: None,
            errors: crate::CompileErrors::from(crate::CompileError::without_span(
                crate::canonical_lower::rir_build_error_kind("packed body materialization", error),
            )),
            references: crate::body_query::BodyReferences(Arc::from([])),
            lookup_observations: crate::body_query::BodyLookupObservations::default(),
        }
    }

    pub(super) fn compiler_body_provider_queries<'a>(
        &self,
        context: &'a rue_query::QueryContext,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
        observed: std::rc::Rc<std::cell::RefCell<ObservedLookupRoot>>,
        positive_references: std::rc::Rc<
            std::cell::RefCell<BTreeSet<crate::body_query::BodyReference>>,
        >,
    ) -> CompilerBodyProviderQueries<'a> {
        CompilerBodyProviderQueries {
            context,
            parse_modules: self.parse_modules.clone(),
            module_source_bases: self.module_source_bases.clone(),
            lookup_names: self.lookup_names.clone(),
            lookup_imports: self.lookup_imports.clone(),
            declaration_body_plan_artifacts: self
                .body_input
                .declaration_body_plan_artifacts
                .clone(),
            semantic_nucleus: self.semantic_nucleus.clone(),
            body_produced_anonymous: self.body_produced_anonymous.clone(),
            body_toolchain_demands: self.body_toolchain_demands.clone(),
            configuration,
            status: std::rc::Rc::new(std::cell::RefCell::new(CompilerBodyProviderStatus::Ready)),
            deferred_anonymous_producers: std::rc::Rc::new(
                std::cell::RefCell::new(BTreeSet::new()),
            ),
            producer_transport_failure: std::rc::Rc::new(std::cell::RefCell::new(None)),
            observed,
            positive_references,
            meter: self.provider_observation_meter.clone(),
            shared_durable_payloads: self.shared_durable_payloads.clone(),
        }
    }

    pub(super) fn evaluate(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
    ) -> Result<QueryOutput<crate::body_query::BodyTransaction>, QueryAbort> {
        #[cfg(test)]
        if self
            .inject_body_transaction_failure
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(QueryOutput::success(
                crate::body_query::BodyTransaction::DeterministicFailure {
                    diagnostic_basis: None,
                    errors: crate::CompileErrors::from(crate::CompileError::without_span(
                        rue_error::ErrorKind::InternalError(format!(
                            "injected_body_transaction_failure for body instance {:?}",
                            key.instance
                        )),
                    )),
                    references: crate::body_query::BodyReferences(Arc::from([])),
                    lookup_observations: crate::body_query::BodyLookupObservations::default(),
                },
            )
            .with_terminal_kind(QueryTerminalKind::Failure));
        }
        let definition = body_source_definition_key(&key.instance)
            .cloned()
            .ok_or(QueryAbort::Canceled)?;
        let candidate =
            declaration_candidate_for_stable_key(&definition).ok_or(QueryAbort::Canceled)?;
        let deferred_anonymous_producers =
            std::rc::Rc::new(std::cell::RefCell::new(BTreeSet::new()));
        // Set when a depended-on anonymous producer committed a deterministic
        // semantic failure. It is carried out of the query closure — which can
        // only signal a bare `QueryAbort` — and mapped to typed `ProducerFailed`
        // control at the request boundary.
        let producer_transport_failure = std::rc::Rc::new(std::cell::RefCell::new(None));
        let well_known_resolution_failure: std::cell::RefCell<
            Option<WellKnownOptionResolutionFailure>,
        > = std::cell::RefCell::new(None);
        let observed = std::rc::Rc::new(std::cell::RefCell::new(ObservedLookupRoot::new()));
        let positive_references = std::rc::Rc::new(std::cell::RefCell::new(BTreeSet::new()));
        // Materialization work is accumulated only until the body transaction
        // reaches a successful terminal. Failed and canceled body attempts
        // therefore never publish structural lowering work.
        let materialization_work =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::<WorkItem>::new()));
        let result = (|| {
            // The transaction's prerequisite fan-out (toolchain demand/artifact
            // authority, transitive anonymous nominals, well-known `Option`
            // resolution) and its trailing lookup-edge recording both run
            // per reached body around the analysis itself. They are timed
            // separately so prerequisite work remains visible beside the
            // body-analysis computation (RUE-786).
            let prerequisites_span =
                tracing::info_span!("body_query_prerequisites", phase = "semantic_analysis")
                    .entered();
            // Observe THIS body's exact fallible-intrinsic payload set from the
            // registered `body-toolchain-demands` node — the ONE canonical
            // typed intrinsic-set and source-candidate authority (RUE-1112 C1)
            // — instead of rescanning or adding a duplicate source edge.
            // A body roots only the payloads it uses, so an unrelated body gains
            // no query edge to payloads it does not use.
            let toolchain_demand =
                context.query_registered(&self.body_toolchain_demands, key.clone())?;
            let rue_query::QueryOutcome::Success(toolchain_demand) = toolchain_demand.outcome()
            else {
                unreachable!("BodyToolchainDemands publishes typed values")
            };
            if !matches!(
                key.instance,
                crate::FunctionInstanceKey::AnonymousMember { .. }
            ) && !toolchain_demand.source_candidate_available()
            {
                return Err(QueryAbort::Canceled);
            }
            let body_payload_kinds = toolchain_demand.payload_kinds();
            let mut selected_anonymous = BTreeMap::new();
            let mut pending_anonymous = collect_instance_anonymous_nominals(&key.instance);
            while let Some(identity) = pending_anonymous.pop_first() {
                if let crate::StableProducerId::Function(function) = &identity.producer
                    && function.as_ref() != &key.instance
                {
                    let produced = match context.query_registered(
                        &self.body_produced_anonymous,
                        crate::body_query::BodyQueryKey::new(
                            (**function).clone(),
                            key.configuration.clone(),
                        ),
                    ) {
                        Ok(produced) => produced,
                        Err(QueryAbort::Canceled) => {
                            deferred_anonymous_producers
                                .borrow_mut()
                                .insert((**function).clone());
                            return Err(QueryAbort::Canceled);
                        }
                        Err(abort) => return Err(abort),
                    };
                    let rue_query::QueryOutcome::Success(produced) = produced.outcome() else {
                        unreachable!("BodyProducedAnonymous publishes typed values")
                    };
                    let produced = match produced {
                        crate::body_query::ProducedAnonymous::Produced(produced) => produced,
                        crate::body_query::ProducedAnonymous::ProducerFailed(failure) => {
                            *producer_transport_failure.borrow_mut() = Some(failure.clone());
                            return Err(QueryAbort::Canceled);
                        }
                    };
                    selected_anonymous.extend(
                        produced
                            .0
                            .iter()
                            .cloned()
                            .map(|nominal| (nominal.identity.clone(), nominal)),
                    );
                }
                if !selected_anonymous.contains_key(&identity) {
                    let query = anonymous_nominal_query_key(&identity, &key.configuration)
                        .ok_or(QueryAbort::Canceled)?;
                    let nominal = context.query_registered(
                        &self.semantic_nucleus,
                        crate::semantic_query_nucleus::SemanticNucleusKey::AnonymousNominal(query),
                    )?;
                    let rue_query::QueryOutcome::Success(nominal) = nominal.outcome() else {
                        unreachable!("SemanticNucleus publishes typed values")
                    };
                    let crate::semantic_query_nucleus::SemanticNucleusValue::AnonymousNominal(
                        nominal,
                    ) = nominal
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    selected_anonymous.insert(nominal.identity.clone(), nominal.clone());
                }
                if let Some(nominal) = selected_anonymous.get(&identity) {
                    let mut dependencies = BTreeSet::new();
                    collect_durable_anonymous_nominal_dependencies(nominal, &mut dependencies);
                    pending_anonymous.extend(
                        dependencies
                            .into_iter()
                            .filter(|dependency| !selected_anonymous.contains_key(dependency)),
                    );
                }
            }
            // Resolve THIS body's exact trusted-std `Option(payload)` set
            // atomically (RUE-1112). Every key is derived directly from the
            // registered body's canonical payload kinds. The session already
            // parked any missing trusted module before entering this task, so
            // a committed semantic failure or wrong projection is fatal:
            // publish neither a partial registry nor a body terminal.
            let well_known = {
                let mut option_by_payload = Vec::new();
                let mut nominals = BTreeMap::new();
                for &payload_kind in body_payload_kinds {
                    for prerequisite in
                        crate::well_known_option::exact_option_prerequisites(payload_kind)
                    {
                        let classified = match context.query_registered(
                            &self.stable_declaration_classifications,
                            StableDeclarationClassificationQueryKey(prerequisite.stable.clone()),
                        ) {
                            Ok(classified) => classified,
                            Err(abort) => {
                                if matches!(
                                    classify_well_known_dependency_abort(&abort),
                                    WellKnownDependencyAbortClass::Propagate
                                ) {
                                    return Err(abort);
                                }
                                *well_known_resolution_failure.borrow_mut() =
                                    Some(WellKnownOptionResolutionFailure::Incomplete {
                                        payload: payload_kind,
                                        prerequisite: Some(prerequisite.stable),
                                        detail: Arc::from(format!(
                                            "exact trusted declaration prerequisite is \
                                                 unavailable: {abort:?}"
                                        )),
                                    });
                                return Err(QueryAbort::Canceled);
                            }
                        };
                        match classified.outcome() {
                            rue_query::QueryOutcome::Success(
                                StableDeclarationClassificationQueryValue::Selected(candidate),
                            ) if candidate == &prerequisite.candidate => {}
                            rue_query::QueryOutcome::Success(value) => {
                                *well_known_resolution_failure.borrow_mut() =
                                    Some(WellKnownOptionResolutionFailure::Incomplete {
                                        payload: payload_kind,
                                        prerequisite: Some(prerequisite.stable),
                                        detail: Arc::from(format!(
                                            "exact trusted declaration prerequisite did not \
                                                 select its required candidate: {value:?}"
                                        )),
                                    });
                                return Err(QueryAbort::Canceled);
                            }
                            rue_query::QueryOutcome::Failure(failure) => {
                                *well_known_resolution_failure.borrow_mut() =
                                    Some(WellKnownOptionResolutionFailure::Incomplete {
                                        payload: payload_kind,
                                        prerequisite: Some(prerequisite.stable),
                                        detail: Arc::from(format!(
                                            "exact trusted declaration prerequisite query \
                                                 failed: {failure:?}"
                                        )),
                                    });
                                return Err(QueryAbort::Canceled);
                            }
                        }
                    }
                    let (payload, call) = crate::well_known_option::exact_option_query(
                        payload_kind,
                        &key.configuration,
                    );
                    let projected = match context.query_registered(
                        &self.semantic_nucleus,
                        crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(call),
                    ) {
                        Ok(projected) => projected,
                        Err(abort) => {
                            if matches!(
                                classify_well_known_dependency_abort(&abort),
                                WellKnownDependencyAbortClass::Propagate
                            ) {
                                return Err(abort);
                            }
                            *well_known_resolution_failure.borrow_mut() =
                                Some(WellKnownOptionResolutionFailure::Incomplete {
                                    payload: payload_kind,
                                    prerequisite: None,
                                    detail: Arc::from(format!(
                                        "exact trusted Option specialization is unavailable: \
                                             {abort:?}"
                                    )),
                                });
                            return Err(QueryAbort::Canceled);
                        }
                    };
                    let projection = match projected.outcome() {
                        rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(
                                projection,
                            ),
                        ) => projection,
                        rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(failure),
                        ) => {
                            *well_known_resolution_failure.borrow_mut() =
                                Some(WellKnownOptionResolutionFailure::Semantic {
                                    payload: payload_kind,
                                    failure: Box::new(failure.clone()),
                                });
                            return Err(QueryAbort::Canceled);
                        }
                        rue_query::QueryOutcome::Success(other) => {
                            *well_known_resolution_failure.borrow_mut() =
                                Some(WellKnownOptionResolutionFailure::WrongProjection {
                                    payload: payload_kind,
                                    detail: Arc::from(format!(
                                        "expected ComptimeCall(Type), found {other:?}"
                                    )),
                                });
                            return Err(QueryAbort::Canceled);
                        }
                        rue_query::QueryOutcome::Failure(failure) => {
                            *well_known_resolution_failure.borrow_mut() = Some(
                                WellKnownOptionResolutionFailure::WrongProjection {
                                    payload: payload_kind,
                                    detail: Arc::from(format!(
                                        "expected a semantic nucleus value, found query failure {failure:?}"
                                    )),
                                },
                            );
                            return Err(QueryAbort::Canceled);
                        }
                    };
                    let crate::semantic_query_nucleus::ComptimeCallResultProjection::Type(
                        option_type,
                    ) = &projection.result
                    else {
                        *well_known_resolution_failure.borrow_mut() =
                            Some(WellKnownOptionResolutionFailure::WrongProjection {
                                payload: payload_kind,
                                detail: Arc::from(format!(
                                    "expected ComptimeCall(Type), found ComptimeCall({:?})",
                                    projection.result
                                )),
                            });
                        return Err(QueryAbort::Canceled);
                    };
                    option_by_payload.push((payload, option_type.clone()));
                    for nominal in projection.anonymous_nominals.iter() {
                        nominals.insert(nominal.identity.clone(), nominal.clone());
                    }
                }
                crate::body_query::WellKnownOptionResolution {
                    option_by_payload: Arc::from(option_by_payload),
                    anonymous_nominals: Arc::from(nominals.into_values().collect::<Vec<_>>()),
                }
            };
            let well_known_facts = rue_air::ProviderWellKnownOptionFacts {
                nominals: well_known
                    .anonymous_nominals
                    .iter()
                    .map(|nominal| nominal.identity.clone())
                    .collect(),
                option_by_payload: well_known.option_by_payload.to_vec(),
            };
            let analysis_anonymous = selected_anonymous
                .values()
                .chain(well_known.anonymous_nominals.iter())
                .cloned()
                .collect::<Vec<_>>();
            let consulted_anonymous_nominals = crate::body_query::BodyConsultedAnonymousNominals(
                analysis_anonymous.clone().into(),
            );
            drop(prerequisites_span);
            let _analysis_span =
                tracing::info_span!("body_analysis", phase = "semantic_analysis").entered();
            let transaction = if matches!(key.instance, crate::FunctionInstanceKey::Definition(_))
                && matches!(
                    definition.kind(),
                    crate::StableDefinitionKind::Function
                        | crate::StableDefinitionKind::Method
                        | crate::StableDefinitionKind::AssociatedFunction
                        | crate::StableDefinitionKind::Destructor
                ) {
                let input = self.body_input.resolve(context, key)?;
                let input = match input {
                    crate::body_query::BodyInputValue::Available(input) => input,
                    crate::body_query::BodyInputValue::Incomplete(
                        crate::body_query::BodyInputIncomplete::BodyPlanFailure(failure),
                    ) => {
                        return Ok(QueryOutput::success(Self::body_plan_failure(
                            &definition,
                            &failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    crate::body_query::BodyInputValue::Incomplete(_) => {
                        return Err(QueryAbort::Canceled);
                    }
                };
                let attribution_enabled =
                    tracing::enabled!(target: "rue::timing", tracing::Level::INFO);
                let _body_input_lowering_span =
                    tracing::info_span!("body_input_lowering", phase = "semantic_analysis")
                        .entered();
                let symbol_space = self.symbol_space.generation(context.revision());
                let materialized = if attribution_enabled {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle_with_attribution(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|(bundle, attribution)| (bundle, Some(attribution)))
                } else {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|bundle| (bundle, None))
                };
                let bundle = match materialized {
                    Ok((bundle, attribution)) if input.artifacts.plan.instruction_count() > 0 => {
                        if let Some(attribution) = attribution {
                            publish_body_plan_materialization_attribution(attribution);
                        }
                        materialization_work.borrow_mut().extend([
                            WorkItem::new("candidate_body_plan.materialization.plans", 1),
                            WorkItem::new(
                                "candidate_body_plan.materialization.instructions",
                                bundle.instruction_count() as u64,
                            ),
                            WorkItem::new(
                                "candidate_body_plan.materialization.payload_words",
                                bundle.payload_word_count() as u64,
                            ),
                        ]);
                        bundle
                    }
                    Ok(_) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            "owned body input lowered to no local instructions",
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Query(abort)) => {
                        return Err(abort);
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Build(error)) => {
                        return Ok(QueryOutput::success(Self::lowering_build_failure(&error))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Invalid(
                        failure,
                    )) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                };
                drop(_body_input_lowering_span);
                // ADR-0076 §4: a body materialized against a superseded
                // equality space is never reused. Abandoning the attempt here
                // re-runs the body against the live generation, which is the
                // fail-closed half of `require_rir_authority`.
                if !bundle.symbol_space().is_live() {
                    return Err(QueryAbort::Canceled);
                }
                let provider = CompilerBodyFactProvider::new(
                    self.compiler_body_provider_queries(
                        context,
                        key.configuration.clone(),
                        observed.clone(),
                        positive_references.clone(),
                    )
                    .with_deferred_anonymous_producers(deferred_anonymous_producers.clone())
                    .with_producer_transport_failure(producer_transport_failure.clone()),
                );
                let source = CompilerBodyDurableSource::with_anonymous(
                    &provider,
                    &analysis_anonymous,
                    Some((
                        definition.module().clone(),
                        rue_air::DurableBodySourceLocator {
                            file_id: input.source.file_id,
                            physical_path: input.source.physical_path.clone(),
                            source_length: input.source.source_length,
                        },
                    )),
                );
                let preview = key
                    .configuration
                    .preview_features
                    .names()
                    .iter()
                    .filter_map(|name| name.parse().ok())
                    .collect();
                let analyzed = {
                    let _provider_analysis_span = tracing::info_span!(
                        "semantic_provider_analysis",
                        phase = "semantic_analysis"
                    )
                    .entered();
                    rue_air::analyze_provider_ordinary_body(
                        &provider,
                        source,
                        &bundle,
                        definition.clone(),
                        definition.name(),
                        definition.kind(),
                        definition.owner().map(|owner| owner.name()),
                        key.configuration.target,
                        preview,
                        &well_known_facts,
                    )
                };
                match provider.finish_status() {
                    Ok(()) => {}
                    Err(CompilerBodyProviderStatus::Fatal(abort)) => return Err(abort),
                    Err(CompilerBodyProviderStatus::Incomplete(_)) => {
                        return Err(QueryAbort::Canceled);
                    }
                    Err(CompilerBodyProviderStatus::Ready) => unreachable!(),
                }
                match analyzed {
                    Ok(analyzed) => {
                        self.provider_observation_meter
                            .accrue_provider_body_work(analyzed.work);
                        let mut references = analyzed
                            .referenced_definitions
                            .iter()
                            .cloned()
                            .map(|definition| {
                                crate::body_query::BodyReference::Callable(
                                    crate::FunctionInstanceKey::Definition(definition),
                                )
                            })
                            .collect::<BTreeSet<_>>();
                        references.extend(
                            analyzed
                                .referenced_values
                                .iter()
                                .cloned()
                                .map(crate::body_query::BodyReference::Definition),
                        );
                        let definition_tokens = analyzed
                            .definition_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let module_tokens = analyzed
                            .module_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let nested = analyzed
                            .referenced_specializations
                            .iter()
                            .map(|instance| {
                                instance.try_map_identities(
                                    &|token| {
                                        definition_tokens.get(token).cloned().ok_or(
                                            rue_air::SemanticStableResolutionFailure::Missing,
                                        )
                                    },
                                    &|token| {
                                        module_tokens.get(token).cloned().ok_or(
                                            rue_air::SemanticStableResolutionFailure::Missing,
                                        )
                                    },
                                )
                            })
                            .collect::<Result<Vec<_>, _>>();
                        if let Ok(nested) = &nested {
                            references.extend(
                                nested
                                    .iter()
                                    .cloned()
                                    .map(crate::body_query::BodyReference::Callable),
                            );
                        }
                        let body = analyzed.export.body.try_map_keys(
                            &|token| {
                                definition_tokens
                                    .get(token)
                                    .cloned()
                                    .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                            },
                            &|token| {
                                module_tokens
                                    .get(token)
                                    .cloned()
                                    .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                            },
                        );
                        let produced_anonymous_nominals =
                            project_provider_produced_anonymous_nominals(
                                &analyzed.produced_anonymous_nominals,
                                &definition_tokens,
                                &module_tokens,
                            );
                        match (body, nested, produced_anonymous_nominals) {
                            (Ok(body), Ok(_), Ok(produced_anonymous_nominals)) => {
                                collect_published_body_references(&body, &mut references);
                                crate::body_query::BodyTransaction::Success {
                                    body: Arc::new(crate::body_query::CanonicalBody::Ordinary {
                                        owner: definition.clone(),
                                        body,
                                    }),
                                    references: crate::body_query::BodyReferences(
                                        references.into_iter().collect::<Vec<_>>().into(),
                                    ),
                                    produced_anonymous_nominals,
                                    consulted_anonymous_nominals: consulted_anonymous_nominals
                                        .clone(),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                            (Err(failure), _, _) => {
                                crate::body_query::BodyTransaction::DeterministicFailure {
                                    diagnostic_basis: None,
                                    errors: crate::CompileErrors::from(
                                        crate::CompileError::without_span(
                                            rue_error::ErrorKind::OutputPublication(format!(
                                                "provider body key relocation failed: \
                                                         {failure:?}"
                                            )),
                                        ),
                                    ),
                                    references: crate::body_query::BodyReferences(Arc::from([])),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                            (_, Err(failure), _) => {
                                crate::body_query::BodyTransaction::DeterministicFailure {
                                    diagnostic_basis: None,
                                    errors: crate::CompileErrors::from(
                                        crate::CompileError::without_span(
                                            rue_error::ErrorKind::OutputPublication(format!(
                                                "provider specialization reference relocation \
                                                     failed: {failure:?}"
                                            )),
                                        ),
                                    ),
                                    references: crate::body_query::BodyReferences(Arc::from([])),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                            (_, _, Err(failure)) => {
                                crate::body_query::BodyTransaction::DeterministicFailure {
                                    diagnostic_basis: None,
                                    errors: crate::CompileErrors::from(
                                        crate::CompileError::without_span(
                                            rue_error::ErrorKind::OutputPublication(format!(
                                                "provider produced anonymous relocation failed: \
                                                 {failure:?}"
                                            )),
                                        ),
                                    ),
                                    references: crate::body_query::BodyReferences(Arc::from([])),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                        }
                    }
                    Err(error) => body_failure_with_source(error, &input.source),
                }
            } else if let crate::FunctionInstanceKey::Specialization { base: _, arguments } =
                &key.instance
                && matches!(definition.kind(), crate::StableDefinitionKind::Function)
            {
                let input = self.body_input.resolve(context, key)?;
                let input = match input {
                    crate::body_query::BodyInputValue::Available(input) => input,
                    crate::body_query::BodyInputValue::Incomplete(
                        crate::body_query::BodyInputIncomplete::BodyPlanFailure(failure),
                    ) => {
                        return Ok(QueryOutput::success(Self::body_plan_failure(
                            &definition,
                            &failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    crate::body_query::BodyInputValue::Incomplete(_) => {
                        return Err(QueryAbort::Canceled);
                    }
                };
                let attribution_enabled =
                    tracing::enabled!(target: "rue::timing", tracing::Level::INFO);
                let _body_input_lowering_span =
                    tracing::info_span!("body_input_lowering", phase = "semantic_analysis")
                        .entered();
                let symbol_space = self.symbol_space.generation(context.revision());
                let materialized = if attribution_enabled {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle_with_attribution(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|(bundle, attribution)| (bundle, Some(attribution)))
                } else {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|bundle| (bundle, None))
                };
                let bundle = match materialized {
                    Ok((bundle, attribution)) if input.artifacts.plan.instruction_count() > 0 => {
                        if let Some(attribution) = attribution {
                            publish_body_plan_materialization_attribution(attribution);
                        }
                        materialization_work.borrow_mut().extend([
                            WorkItem::new("candidate_body_plan.materialization.plans", 1),
                            WorkItem::new(
                                "candidate_body_plan.materialization.instructions",
                                bundle.instruction_count() as u64,
                            ),
                            WorkItem::new(
                                "candidate_body_plan.materialization.payload_words",
                                bundle.payload_word_count() as u64,
                            ),
                        ]);
                        bundle
                    }
                    Ok(_) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            "owned body input lowered to no local instructions",
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Query(abort)) => {
                        return Err(abort);
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Build(error)) => {
                        return Ok(QueryOutput::success(Self::lowering_build_failure(&error))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Invalid(
                        failure,
                    )) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                };
                drop(_body_input_lowering_span);
                // ADR-0076 §4: a body materialized against a superseded
                // equality space is never reused. Abandoning the attempt here
                // re-runs the body against the live generation, which is the
                // fail-closed half of `require_rir_authority`.
                if !bundle.symbol_space().is_live() {
                    return Err(QueryAbort::Canceled);
                }
                let provider = CompilerBodyFactProvider::new(
                    self.compiler_body_provider_queries(
                        context,
                        key.configuration.clone(),
                        observed.clone(),
                        positive_references.clone(),
                    )
                    .with_deferred_anonymous_producers(deferred_anonymous_producers.clone())
                    .with_producer_transport_failure(producer_transport_failure.clone()),
                );
                let source = CompilerBodyDurableSource::with_anonymous(
                    &provider,
                    &analysis_anonymous,
                    Some((
                        definition.module().clone(),
                        rue_air::DurableBodySourceLocator {
                            file_id: input.source.file_id,
                            physical_path: input.source.physical_path.clone(),
                            source_length: input.source.source_length,
                        },
                    )),
                );
                let preview = key
                    .configuration
                    .preview_features
                    .names()
                    .iter()
                    .filter_map(|name| name.parse().ok())
                    .collect();
                let analyzed = {
                    let _provider_analysis_span = tracing::info_span!(
                        "semantic_provider_analysis",
                        phase = "semantic_analysis"
                    )
                    .entered();
                    rue_air::analyze_provider_specialized_body(
                        &provider,
                        source,
                        &bundle,
                        definition.clone(),
                        definition.name(),
                        arguments,
                        key.configuration.target,
                        preview,
                        &well_known_facts,
                    )
                };
                match provider.finish_status() {
                    Ok(()) => {}
                    Err(CompilerBodyProviderStatus::Fatal(abort)) => return Err(abort),
                    Err(CompilerBodyProviderStatus::Incomplete(_)) => {
                        return Err(QueryAbort::Canceled);
                    }
                    Err(CompilerBodyProviderStatus::Ready) => unreachable!(),
                }
                // A comptime type constructor owns the anonymous nominals in
                // its result. Observe that exact semantic projection here so
                // the body-produced projection remains a thin view of this
                // transaction instead of recomputing producer facts.
                let produced_anonymous_nominals = {
                    let shell = context.query_registered(
                        &self.declaration_shells,
                        DeclarationShellQueryKey(candidate.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(
                        shell,
                    )) = shell.outcome()
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                        declaration: candidate.clone(),
                        configuration: key.configuration.clone(),
                    };
                    let signature = context.query_registered(
                        &self.semantic_nucleus,
                        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                            producer.clone(),
                        ),
                    )?;
                    let rue_query::QueryOutcome::Success(
                        crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature),
                    ) = signature.outcome()
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    let Some(exact_type_syntax) = signature.callable_type_syntax.as_ref() else {
                        return Err(QueryAbort::Canceled);
                    };
                    if let Some(call) = comptime_call_for_anonymous_function(
                        &producer,
                        &key.instance,
                        shell,
                        signature,
                        exact_type_syntax,
                    ) {
                        let projected = context.query_registered(
                            &self.semantic_nucleus,
                            crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(call),
                        )?;
                        let rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(
                                projected,
                            ),
                        ) = projected.outcome()
                        else {
                            return Err(QueryAbort::Canceled);
                        };
                        crate::body_query::BodyProducedAnonymousNominals(
                            projected.anonymous_nominals.clone(),
                        )
                    } else {
                        crate::body_query::BodyProducedAnonymousNominals(Arc::from([]))
                    }
                };
                match analyzed {
                    Ok(analyzed) => {
                        self.provider_observation_meter
                            .accrue_provider_body_work(analyzed.work);
                        let definition_tokens = analyzed
                            .definition_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let module_tokens = analyzed
                            .module_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let definition = |token: &rue_air::SemanticDefinitionToken| {
                            definition_tokens
                                .get(token)
                                .cloned()
                                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                        };
                        let module = |token: &rue_air::SemanticModuleToken| {
                            module_tokens
                                .get(token)
                                .cloned()
                                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                        };
                        let identity = analyzed.export.identity.try_map_keys(&definition, &module);
                        let body = analyzed.export.body.try_map_keys(&definition, &module);
                        let dependencies = analyzed
                            .export
                            .dependencies
                            .iter()
                            .map(&definition)
                            .collect::<Result<Vec<_>, _>>();
                        let nested = analyzed
                            .referenced_specializations
                            .iter()
                            .map(|instance| instance.try_map_identities(&definition, &module))
                            .collect::<Result<Vec<_>, _>>();
                        let locally_produced = project_provider_produced_anonymous_nominals(
                            &analyzed.produced_anonymous_nominals,
                            &definition_tokens,
                            &module_tokens,
                        );
                        match (identity, body, dependencies, nested, locally_produced) {
                            (
                                Ok(identity),
                                Ok(body),
                                Ok(dependencies),
                                Ok(nested),
                                Ok(locally_produced),
                            ) => {
                                let mut references = analyzed
                                    .referenced_definitions
                                    .into_iter()
                                    .map(|definition| {
                                        crate::body_query::BodyReference::Callable(
                                            crate::FunctionInstanceKey::Definition(definition),
                                        )
                                    })
                                    .collect::<BTreeSet<_>>();
                                references.extend(
                                    analyzed
                                        .referenced_values
                                        .into_iter()
                                        .map(crate::body_query::BodyReference::Definition),
                                );
                                references.extend(
                                    nested
                                        .into_iter()
                                        .map(crate::body_query::BodyReference::Callable),
                                );
                                collect_published_body_references(&body, &mut references);
                                let produced = produced_anonymous_nominals
                                    .0
                                    .iter()
                                    .chain(locally_produced.0.iter())
                                    .cloned()
                                    .map(|nominal| (nominal.identity.clone(), nominal))
                                    .collect::<BTreeMap<_, _>>();
                                crate::body_query::BodyTransaction::Success {
                                    body: Arc::new(
                                        crate::body_query::CanonicalBody::Specialization {
                                            identity,
                                            body,
                                            dependencies: dependencies.into(),
                                            dependency_boundary_complete: analyzed
                                                .export
                                                .dependency_boundary_complete,
                                        },
                                    ),
                                    references: crate::body_query::BodyReferences(
                                        references.into_iter().collect::<Vec<_>>().into(),
                                    ),
                                    produced_anonymous_nominals:
                                        crate::body_query::BodyProducedAnonymousNominals(
                                            produced.into_values().collect::<Vec<_>>().into(),
                                        ),
                                    consulted_anonymous_nominals: consulted_anonymous_nominals
                                        .clone(),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                            _ => crate::body_query::BodyTransaction::DeterministicFailure {
                                diagnostic_basis: None,
                                errors: crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::OutputPublication(
                                            "provider specialization key relocation failed"
                                                .to_owned(),
                                        ),
                                    ),
                                ),
                                references: crate::body_query::BodyReferences(Arc::from([])),
                                lookup_observations:
                                    crate::body_query::BodyLookupObservations::default(),
                            },
                        }
                    }
                    Err(error) => body_failure_with_source(error, &input.source),
                }
            } else if let crate::FunctionInstanceKey::AnonymousMember { owner, member } =
                &key.instance
            {
                let crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(
                    owner_identity,
                )) = owner.as_ref()
                else {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous member has a non-anonymous owner",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                };
                let Some(owner_fact) = selected_anonymous.get(owner_identity) else {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous member owner fact is unavailable",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                };
                let crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                    methods, ..
                } = &owner_fact.shape
                else {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous enum cannot own a callable member",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                };
                let Some(method) = methods.iter().find(|candidate| {
                    let kind = if candidate.name.as_ref() == "__drop" {
                        crate::AnonymousMemberKind::Destructor
                    } else if candidate.has_self {
                        crate::AnonymousMemberKind::Method
                    } else {
                        crate::AnonymousMemberKind::AssociatedFunction
                    };
                    candidate.name == member.name && kind == member.kind
                }) else {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous member identity is absent from its producer facts",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                };
                if !method.has_body {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous member producer facts do not admit a body",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                }
                let input = self.body_input.resolve_producer_artifact(context, key)?;
                let input = match input {
                    crate::body_query::BodyInputValue::Available(input) => input,
                    crate::body_query::BodyInputValue::Incomplete(
                        crate::body_query::BodyInputIncomplete::BodyPlanFailure(failure),
                    ) => {
                        return Ok(QueryOutput::success(Self::body_plan_failure(
                            &definition,
                            &failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    crate::body_query::BodyInputValue::Incomplete(incomplete) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            format!("anonymous producer artifact is unavailable: {incomplete:?}"),
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                };
                let attribution_enabled =
                    tracing::enabled!(target: "rue::timing", tracing::Level::INFO);
                let _body_input_lowering_span =
                    tracing::info_span!("body_input_lowering", phase = "semantic_analysis")
                        .entered();
                let symbol_space = self.symbol_space.generation(context.revision());
                let materialized = if attribution_enabled {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle_with_declaration_and_attribution(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|(bundle, declaration, attribution)| {
                            (bundle, declaration, Some(attribution))
                        })
                } else {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle_with_declaration(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|(bundle, declaration)| (bundle, declaration, None))
                };
                let (bundle, candidate_root) = match materialized {
                    Ok((bundle, declaration, attribution))
                        if input.artifacts.plan.instruction_count() > 0 =>
                    {
                        if let Some(attribution) = attribution {
                            publish_body_plan_materialization_attribution(attribution);
                        }
                        materialization_work.borrow_mut().extend([
                            WorkItem::new("candidate_body_plan.materialization.plans", 1),
                            WorkItem::new(
                                "candidate_body_plan.materialization.instructions",
                                bundle.instruction_count() as u64,
                            ),
                            WorkItem::new(
                                "candidate_body_plan.materialization.payload_words",
                                bundle.payload_word_count() as u64,
                            ),
                        ]);
                        (bundle, declaration)
                    }
                    Ok(_) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            "anonymous producer artifact contains no local instructions",
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Query(abort)) => {
                        return Err(abort);
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Build(error)) => {
                        return Ok(QueryOutput::success(Self::lowering_build_failure(&error))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Invalid(
                        failure,
                    )) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                };
                drop(_body_input_lowering_span);
                // ADR-0076 §4: a body materialized against a superseded
                // equality space is never reused. Abandoning the attempt here
                // re-runs the body against the live generation, which is the
                // fail-closed half of `require_rir_authority`.
                if !bundle.symbol_space().is_live() {
                    return Err(QueryAbort::Canceled);
                }
                let provider = CompilerBodyFactProvider::new(
                    self.compiler_body_provider_queries(
                        context,
                        key.configuration.clone(),
                        observed.clone(),
                        positive_references.clone(),
                    )
                    .with_deferred_anonymous_producers(deferred_anonymous_producers.clone())
                    .with_producer_transport_failure(producer_transport_failure.clone()),
                );
                let source_locator = rue_air::DurableBodySourceLocator {
                    file_id: input.source.file_id,
                    physical_path: input.source.physical_path.clone(),
                    source_length: input.source.source_length,
                };
                let source = CompilerBodyDurableSource::with_anonymous(
                    &provider,
                    &analysis_anonymous,
                    Some((definition.module().clone(), source_locator.clone())),
                );
                let preview = key
                    .configuration
                    .preview_features
                    .names()
                    .iter()
                    .filter_map(|name| name.parse().ok())
                    .collect();
                let analyzed = {
                    let _provider_analysis_span = tracing::info_span!(
                        "semantic_provider_analysis",
                        phase = "semantic_analysis"
                    )
                    .entered();
                    rue_air::analyze_provider_anonymous_body(
                        &provider,
                        source,
                        &bundle,
                        candidate_root,
                        definition.clone(),
                        owner.as_ref(),
                        member,
                        key.configuration.target,
                        preview,
                        &well_known_facts,
                    )
                };
                match provider.finish_status() {
                    Ok(()) => {}
                    Err(CompilerBodyProviderStatus::Fatal(abort)) => return Err(abort),
                    Err(CompilerBodyProviderStatus::Incomplete(_)) => {
                        return Err(QueryAbort::Canceled);
                    }
                    Err(CompilerBodyProviderStatus::Ready) => unreachable!(),
                }
                match analyzed {
                    Ok(analyzed) => {
                        self.provider_observation_meter
                            .accrue_provider_body_work(analyzed.work);
                        let body_anchor = analyzed
                            .body_span
                            .start
                            .checked_sub(input.source.body_start)
                            .zip(analyzed.body_span.end.checked_sub(input.source.body_start))
                            .filter(|(start, end)| {
                                analyzed.body_span.file_id == input.source.file_id
                                    && start <= end
                                    && analyzed.body_span.end <= input.source.body_end
                            })
                            .map(|(start, end)| crate::body_query::BodyRelativeRange {
                                start,
                                end,
                            });
                        let definition_tokens = analyzed
                            .definition_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let module_tokens = analyzed
                            .module_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let definition = |token: &rue_air::SemanticDefinitionToken| {
                            definition_tokens
                                .get(token)
                                .cloned()
                                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                        };
                        let module = |token: &rue_air::SemanticModuleToken| {
                            module_tokens
                                .get(token)
                                .cloned()
                                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                        };
                        let identity = analyzed
                            .export
                            .identity
                            .try_map_identities(&definition, &module);
                        let body = analyzed.export.body.try_map_keys(&definition, &module);
                        let nested = analyzed
                            .referenced_specializations
                            .iter()
                            .map(|instance| instance.try_map_identities(&definition, &module))
                            .collect::<Result<Vec<_>, _>>();
                        let produced_anonymous_nominals =
                            project_provider_produced_anonymous_nominals(
                                &analyzed.produced_anonymous_nominals,
                                &definition_tokens,
                                &module_tokens,
                            );
                        match (identity, body, nested, produced_anonymous_nominals) {
                            (
                                Ok(identity),
                                Ok(body),
                                Ok(nested),
                                Ok(produced_anonymous_nominals),
                            ) if identity == key.instance && body_anchor.is_some() => {
                                let mut references = analyzed
                                    .referenced_definitions
                                    .into_iter()
                                    .map(|definition| {
                                        crate::body_query::BodyReference::Callable(
                                            crate::FunctionInstanceKey::Definition(definition),
                                        )
                                    })
                                    .collect::<BTreeSet<_>>();
                                references.extend(
                                    analyzed
                                        .referenced_values
                                        .into_iter()
                                        .map(crate::body_query::BodyReference::Definition),
                                );
                                references.extend(
                                    nested
                                        .into_iter()
                                        .map(crate::body_query::BodyReference::Callable),
                                );
                                collect_published_body_references(&body, &mut references);
                                crate::body_query::BodyTransaction::Success {
                                    body: Arc::new(crate::body_query::CanonicalBody::Anonymous {
                                        identity,
                                        body_anchor: body_anchor.expect(
                                            "successful anonymous body projection has an anchor",
                                        ),
                                        body,
                                    }),
                                    references: crate::body_query::BodyReferences(
                                        references.into_iter().collect::<Vec<_>>().into(),
                                    ),
                                    produced_anonymous_nominals,
                                    consulted_anonymous_nominals: consulted_anonymous_nominals
                                        .clone(),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                            _ => crate::body_query::BodyTransaction::DeterministicFailure {
                                diagnostic_basis: None,
                                errors: crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::OutputPublication(
                                            "provider anonymous body key relocation failed"
                                                .to_owned(),
                                        ),
                                    ),
                                ),
                                references: crate::body_query::BodyReferences(Arc::from([])),
                                lookup_observations:
                                    crate::body_query::BodyLookupObservations::default(),
                            },
                        }
                    }
                    Err(error) => body_failure_with_source(error, &input.source),
                }
            } else {
                crate::body_query::BodyTransaction::DeterministicFailure {
                    diagnostic_basis: None,
                    errors: crate::CompileErrors::from(crate::CompileError::without_span(
                        rue_error::ErrorKind::InvalidCompilerInput(
                            "body query instance has no provider-native analyzer".into(),
                        ),
                    )),
                    references: crate::body_query::BodyReferences(Arc::from([])),
                    lookup_observations: crate::body_query::BodyLookupObservations::default(),
                }
            };
            // Provider operations above record every semantic, lookup, and
            // producer edge while the analysis consumes it. Replaying the
            // exported reference summary through semantic queries here would
            // be both redundant and cyclic (notably through
            // body-produced-anonymous -> body-transaction).
            let observed = observed.replace(ObservedLookupRoot::new());
            let descriptors = observed.descriptors();
            context.register_attempt_handoff(PublishedLookupRootHandoff {
                lease: self.lookup_root_lease.clone(),
                runtime: self.runtime.clone(),
                root: body_lookup_root_identity(key),
                observed: Some(observed),
                rollback: None,
            });
            let transaction = transaction.attach_provider_observations(
                descriptors,
                positive_references.replace(BTreeSet::new()),
            );
            let kind = if matches!(
                transaction,
                crate::body_query::BodyTransaction::Success { .. }
            ) {
                QueryTerminalKind::Success
            } else {
                QueryTerminalKind::Failure
            };
            let output = QueryOutput::success(transaction).with_terminal_kind(kind);
            if kind == QueryTerminalKind::Success {
                Ok(output.with_work(materialization_work.borrow_mut().drain(..).collect()))
            } else {
                Ok(output)
            }
        })();
        match result {
            Ok(output) => Ok(output),
            Err(abort) => {
                // Cancellation is query control and always wins. Domain-specific
                // deferrals are typed query values, atomically published with the
                // attempt that classified them; no revision/key side channel can
                // race a joiner or a successor request.
                if context.check_canceled().is_err() || !matches!(abort, QueryAbort::Canceled) {
                    return Err(abort);
                }
                let control = if let Some(failure) = producer_transport_failure.borrow_mut().take()
                {
                    crate::body_query::BodyTransactionControl::ProducerFailed(failure)
                } else if !deferred_anonymous_producers.borrow().is_empty() {
                    crate::body_query::BodyTransactionControl::DeferredAnonymousProducers(
                        deferred_anonymous_producers
                            .borrow()
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .into(),
                    )
                } else if let Some(failure) = well_known_resolution_failure.into_inner() {
                    crate::body_query::BodyTransactionControl::WellKnownOptionResolution(failure)
                } else {
                    return Err(abort);
                };
                Ok(
                    QueryOutput::success(crate::body_query::BodyTransaction::Control(control))
                        .with_terminal_kind(QueryTerminalKind::Failure),
                )
            }
        }
    }
}
impl RevisionedQueryDatabase {
    /// Request the canonical registered body evaluator. Domain-specific
    /// deferrals are typed terminal values published atomically with the
    /// attempt that produced them; cancellation remains a runtime abort.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn body_transaction(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<
        Arc<rue_query::QueryTerminal<crate::body_query::BodyTransaction>>,
        BodyTransactionRequestFailure,
    > {
        let attempt = self.runtime.request_registered(
            &self.body_transactions,
            revision,
            key.clone(),
            cancellation.clone(),
        );
        match attempt.into_result() {
            Ok(terminal) => match terminal.outcome() {
                rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Control(
                    crate::body_query::BodyTransactionControl::DeferredAnonymousProducers(
                        producers,
                    ),
                )) => Err(BodyTransactionRequestFailure::DeferredAnonymousProducers(
                    producers.clone(),
                )),
                rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Control(
                    crate::body_query::BodyTransactionControl::ProducerFailed(failure),
                )) => Err(BodyTransactionRequestFailure::ProducerFailed(
                    failure.clone(),
                )),
                rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Control(
                    crate::body_query::BodyTransactionControl::WellKnownOptionResolution(failure),
                )) => Err(BodyTransactionRequestFailure::WellKnownOptionResolution(
                    failure.clone(),
                )),
                rue_query::QueryOutcome::Success(transaction) => {
                    if let Some(observations) = transaction.lookup_observations() {
                        self.refresh_published_body_lookup_root(
                            revision,
                            &key,
                            observations,
                            cancellation.clone(),
                        )
                        .map_err(BodyTransactionRequestFailure::Query)?;
                    }
                    Ok(terminal)
                }
                _ => Ok(terminal),
            },
            Err(abort) if cancellation.is_canceled() => {
                Err(BodyTransactionRequestFailure::Query(QueryAbort::Canceled))
            }
            Err(abort) => Err(BodyTransactionRequestFailure::Query(abort)),
        }
    }

    #[allow(clippy::mutable_key_type)]
    pub(crate) fn body_closure(
        &self,
        revision: Revision,
        mut key: crate::body_query::BodyClosureQueryKey,
        cancellation: CancellationToken,
    ) -> Result<BodyClosureRequest, QueryAbort> {
        // The query runtime can intentionally keep one semantic revision id
        // across a successor source publication. The shared payload cache is
        // request-scoped, so never let a prior closure's durable signatures
        // cross that publication boundary.
        self.shared_durable_payloads.reset(false);
        let mut modules = key.modules.iter().cloned().collect::<Vec<_>>();
        modules.sort();
        modules.dedup();
        key.modules = modules.into();
        let mut roots = key.roots.iter().cloned().collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        key.roots = roots.into();
        let mut retained_before = BTreeSet::new();
        self.body_transactions.any_retained_key(|candidate| {
            if candidate.configuration == key.configuration {
                retained_before.insert(rue_query::NodeIdentity::from_typed_key(
                    "compiler.body-transaction",
                    candidate,
                ));
            }
            false
        });
        self.shared_durable_payloads
            .reset(retained_before.is_empty());
        let publication_attempt = self.runtime.request_registered(
            &self.body_closure_publications,
            revision,
            crate::body_query::BodyClosurePublicationKey {
                closure: key,
                epoch: revision.id(),
            },
            cancellation,
        );
        let body_executions = publication_attempt
            .nested_attempts()
            .iter()
            .filter(|attempt| attempt.node().family() == "compiler.body-transaction")
            .fold(BTreeMap::new(), |mut executions, attempt| {
                executions
                    .entry(attempt.node().clone())
                    .and_modify(|execution| {
                        // The publication request can observe the same body
                        // transaction first through closure validation and
                        // later through the closure evaluator. Charge it as
                        // computed if any nested attempt owned the evaluator;
                        // a later retained read must not erase that fact.
                        if attempt.execution() == rue_query::RequestExecution::Computed {
                            *execution = rue_query::RequestExecution::Computed;
                        }
                    })
                    .or_insert_with(|| attempt.execution());
                executions
            });
        let work: Vec<(Arc<str>, u64)> = publication_attempt
            .nested_attempts()
            .iter()
            .filter(|attempt| attempt.node().family() == "compiler.body-reachability")
            .flat_map(|attempt| attempt.work().iter().cloned())
            .fold(
                BTreeMap::<Arc<str>, u64>::new(),
                |mut reduced, (identity, amount)| {
                    reduced
                        .entry(identity)
                        .and_modify(|total| *total = total.saturating_add(amount))
                        .or_insert(amount);
                    reduced
                },
            )
            .into_iter()
            .collect();
        let (mut candidate_body_plan_work, mut candidate_body_materialization_work) =
            candidate_body_plan_work_from_nested(publication_attempt.nested_attempts());
        let represented_construction = successful_nested_nodes(
            publication_attempt.nested_attempts(),
            "compiler.declaration-body-plan-artifacts",
        );
        let represented_materialization = successful_nested_nodes(
            publication_attempt.nested_attempts(),
            "compiler.body-transaction",
        );
        self.body_reachability_meter.accrue(&work);
        let publication = publication_attempt.into_result()?;
        let rue_query::QueryOutcome::Success(closure) = publication.outcome() else {
            unreachable!("BodyClosurePublication publishes typed values")
        };
        // Retained publications have an intentionally empty nested-attempt
        // ledger. The closure is also retained on mixed edits, where nested
        // attempts cover only changed children. Recover missing successful
        // child counts from the compact publication and subtract attempts
        // already represented above. Output quantities remain zero on reuse.
        let (retained_construction, retained_materialization) =
            candidate_body_plan_work_from_retained_closure(
                closure,
                &represented_construction,
                &represented_materialization,
            );
        candidate_body_plan_work.reused = candidate_body_plan_work
            .reused
            .saturating_add(retained_construction.reused);
        candidate_body_materialization_work.reused = candidate_body_materialization_work
            .reused
            .saturating_add(retained_materialization.reused);
        Ok(BodyClosureRequest {
            terminal: closure.clone(),
            body_executions,
            retained_before,
            work,
            candidate_body_plan_work,
            candidate_body_materialization_work,
        })
    }

    #[cfg(test)]
    pub(super) fn body_closure_root_metrics(&self) -> (usize, u64, u64) {
        let root = self
            .body_closure_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (root.lease.len(), root.additions, root.deletions)
    }

    #[cfg(test)]
    pub(super) fn body_reachability_root_len(&self) -> usize {
        self.body_reachability_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lease
            .len()
    }

    /// Request the current presentation locator for one body independently of
    /// its retained semantic transaction and aggregate body closure.
    pub(crate) fn body_source_basis_projection(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<
        Arc<rue_query::QueryTerminal<Option<crate::body_query::BodySourceLocator>>>,
        QueryAbort,
    > {
        self.runtime
            .request_registered(&self.body_source_bases, revision, key, cancellation)
            .into_result()
    }

    /// Request one stable-ordered frontier of independent warning-only body
    /// projections. The registered root joins atomically, so cancellation or a
    /// child abort cannot publish a partial aggregate.
    pub(crate) fn warning_body_reference_frontier(
        &self,
        revision: Revision,
        keys: Arc<[crate::body_query::BodyQueryKey]>,
        cancellation: CancellationToken,
    ) -> (
        QueryRequestAttempt<WarningBodyReferencesBatchValue>,
        Vec<Option<FrontierChildExecution>>,
    ) {
        let attempt = self.runtime.request_registered(
            &self.warning_body_reference_batches,
            revision,
            WarningBodyReferencesBatchKey {
                bodies: keys.clone(),
            },
            cancellation,
        );
        let executions =
            frontier_child_executions(&attempt, "compiler.warning-body-references", keys.as_ref());
        (attempt, executions)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn body_produced_anonymous_projection(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<Arc<rue_query::QueryTerminal<crate::body_query::ProducedAnonymous>>, QueryAbort>
    {
        self.runtime
            .request_registered(&self.body_produced_anonymous, revision, key, cancellation)
            .into_result()
    }

    #[cfg(test)]
    pub(crate) fn body_input(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<Arc<rue_query::QueryTerminal<crate::body_query::BodyInputValue>>, QueryAbort> {
        self.runtime
            .request_registered(&self.body_inputs, revision, key, cancellation)
            .into_result()
    }

    /// Project one reached body's trusted-toolchain-module demand set (RUE-1112)
    /// from the registered `body-toolchain-demands` node. This is the rooted
    /// semantic attempt's park prerequisite: it observes the body's canonical
    /// declaration artifact, is pure and I/O-free, and never itself parks. The rooted attempt
    /// checks the projected modules against the satisfied catalogue and decides
    /// whether to park BEFORE the body transaction runs.
    #[cfg(test)]
    pub(crate) fn body_toolchain_demands(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<Arc<rue_query::QueryTerminal<crate::BodyToolchainDemand>>, QueryAbort> {
        self.runtime
            .request_registered(&self.body_toolchain_demands, revision, key, cancellation)
            .into_result()
    }

    /// Whether the body-transaction family has published any terminal. Used by
    /// the RUE-1112 park test to prove a park precedes any body transaction.
    #[cfg(test)]
    pub(crate) fn any_body_transaction_terminal(&self) -> bool {
        self.body_transactions.any_retained_key(|_| true)
    }

    /// Whether a retained body transaction has a successful value. Cached
    /// deterministic failures are intentionally excluded: retention of a
    /// failure is not evidence of published candidate work or reusable work.
    #[cfg(test)]
    pub(crate) fn any_successful_body_transaction_for_test(&self) -> bool {
        let Some(revision) = self.current_semantic_revision() else {
            return false;
        };
        let mut keys = Vec::new();
        self.body_transactions.any_retained_key(|key| {
            keys.push(key.clone());
            false
        });
        keys.into_iter().any(|key| {
            self.body_transaction(revision, key, CancellationToken::new())
                .is_ok_and(|terminal| {
                    matches!(
                        terminal.outcome(),
                        rue_query::QueryOutcome::Success(
                            crate::body_query::BodyTransaction::Success { .. }
                        )
                    )
                })
        })
    }

    /// Whether the exact body key already has a retained memo node.
    ///
    /// This is deliberately narrower than provenance: a newly reached body has
    /// no prior key to invalidate, even though its first terminal changes the
    /// provenance-derived body set. The query runtime remains authoritative for
    /// whether the current request actually computed or reused that key. The
    /// exact retained-key index probe is O(1) average-case; it does not
    /// enumerate the retained body family once per reached body.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn has_retained_body_key(&self, key: &crate::body_query::BodyQueryKey) -> bool {
        self.body_transactions.contains_retained_key(key)
    }

    /// Test-only snapshot of every retained body-query identity and whether its
    /// terminal is observable at `revision`.
    ///
    /// The key scan is intentionally rooted in the production body-transaction
    /// family. It includes stale keys left behind by invalidation, while the
    /// read-only terminal request records whether that key still has a current
    /// success or deterministic failure. The supplied computation is never
    /// entered, so this hook cannot become a second semantic computation path.
    #[allow(dead_code)]
    pub(crate) fn retained_body_identity_states_for_test(
        &self,
        revision: Revision,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    ) -> BTreeMap<String, Option<crate::BodyTransaction>> {
        let mut keys = Vec::new();
        self.body_transactions.any_retained_key(|candidate| {
            keys.push(candidate.clone());
            false
        });
        keys.retain(|key| key.configuration == configuration);
        keys.sort_by_key(|key| key.stable_identity());
        keys.into_iter()
            .map(|key| {
                let identity = key.stable_identity();
                let transaction = self.retained_body_transaction_for_test(revision, key);
                (identity, transaction)
            })
            .collect()
    }

    /// Test-only exact provenance for retained ordinary body terminals.
    ///
    /// Looking up retained keys directly keeps failed-revision acceptance tests
    /// independent of the aggregate stable-definition request, which may itself
    /// fail after a negative name lookup even though unrelated body terminals
    /// remain reusable.
    #[cfg(test)]
    pub(crate) fn retained_body_transaction_origins_for_test(
        &self,
        revision: Revision,
        names: &[String],
    ) -> BTreeMap<String, u64> {
        let mut origins = BTreeMap::new();
        for name in names {
            let mut retained_key = None;
            self.body_transactions.any_retained_key(|candidate| {
                let crate::FunctionInstanceKey::Definition(definition) = &candidate.instance else {
                    return false;
                };
                if definition.name() != name.as_str() {
                    return false;
                }
                retained_key = Some(candidate.clone());
                true
            });
            let Some(key) = retained_key else {
                continue;
            };
            let terminal = self.body_transaction(revision, key, CancellationToken::new());
            if let Ok(terminal) = terminal {
                origins.insert(name.clone(), terminal.origin_request_id());
            }
        }
        origins
    }

    /// Test-only lookup of one retained body transaction by its complete
    /// function-instance identity, including specializations and anonymous
    /// members.
    pub(crate) fn retained_body_transaction_for_test(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
    ) -> Option<crate::BodyTransaction> {
        let terminal = self
            .body_transaction(revision, key, CancellationToken::new())
            .ok()?;
        let rue_query::QueryOutcome::Success(transaction) = terminal.outcome() else {
            unreachable!("BodyTransaction publishes typed values")
        };
        Some(transaction.clone())
    }

    #[cfg(test)]
    pub(crate) fn projected_declaration_semantics(
        &self,
        revision: Revision,
        program: &crate::canonical_merge::CanonicalMergedAst,
        target: rue_target::Target,
        preview_features: &crate::PreviewFeatures,
        cancellation: CancellationToken,
    ) -> Result<SemanticNucleusProjection, SemanticNucleusBatchFailure> {
        self.projected_declaration_semantics_for_modules(
            revision,
            program
                .modules()
                .iter()
                .map(|module| module.module_id().clone()),
            target,
            preview_features,
            cancellation,
        )
    }

    /// Query the declaration nucleus from stable module membership directly.
    /// Production roots use this form so canonical merged syntax remains an
    /// explicit presentation projection rather than a codegen prerequisite.
    pub(crate) fn projected_declaration_semantics_for_modules(
        &self,
        revision: Revision,
        modules: impl IntoIterator<Item = ModuleId>,
        target: rue_target::Target,
        preview_features: &crate::PreviewFeatures,
        cancellation: CancellationToken,
    ) -> Result<SemanticNucleusProjection, SemanticNucleusBatchFailure> {
        let mut modules = modules.into_iter().collect::<Vec<_>>();
        modules.sort();
        modules.dedup();
        let key = SemanticNucleusProjectionKey {
            modules: modules.into(),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target,
                preview_features: crate::StablePreviewFeatures::new(preview_features),
            },
        };
        let publication = self
            .runtime
            .request_registered(
                &self.declaration_semantics_publications,
                revision,
                key,
                cancellation,
            )
            .into_result()
            .map_err(SemanticNucleusBatchFailure::Query)?;
        let rue_query::QueryOutcome::Success(terminal) = publication.outcome() else {
            unreachable!("DeclarationSemanticsPublication publishes typed values")
        };
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("DeclarationSemanticsProjection publishes typed values")
        };
        match value {
            SemanticNucleusProjectionValue::Available(projection) => Ok(projection.clone()),
            SemanticNucleusProjectionValue::Failure {
                declaration,
                failure,
            } => Err(SemanticNucleusBatchFailure::Stable {
                declaration: declaration.clone(),
                failure: failure.clone(),
            }),
        }
    }

    pub(super) fn evaluate_declaration_semantics_projection(
        context: &rue_query::QueryContext,
        declaration_occurrence_indexes: &QueryFamily<
            ModuleQueryKey,
            DeclarationOccurrenceIndexValue,
        >,
        declaration_orders: &QueryFamily<ModuleQueryKey, DeclarationOrderValue>,
        semantic_nucleus: &QueryFamily<
            crate::semantic_query_nucleus::SemanticNucleusKey,
            crate::semantic_query_nucleus::SemanticNucleusValue,
        >,
        declaration_shells: &QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
        key: &SemanticNucleusProjectionKey,
    ) -> Result<SemanticNucleusProjection, SemanticNucleusBatchFailure> {
        use crate::declaration_candidate::{
            DeclarationCandidateCategory as Category, DeclarationOccurrenceCapability,
        };
        use crate::semantic_query_nucleus::{
            DeclarationSemanticQueryKey as DeclarationQuery, DeclarationSemanticValue,
            SemanticNucleusKey as Key, SemanticNucleusValue as Value,
        };

        let configuration = key.configuration.clone();
        let mut values = Vec::new();
        let mut anonymous_nominals = BTreeMap::new();
        let mut dependencies = BTreeSet::new();
        let mut c_export_roots = BTreeSet::new();
        let mut duplicate_declarations = Vec::new();
        let mut foreign_declarations = BTreeMap::<
            Arc<str>,
            (
                crate::declaration_candidate::DeclarationCandidateKey,
                Arc<[crate::durable_semantics::DurableSemanticParameter]>,
                crate::durable_semantics::DurableType,
            ),
        >::new();
        for module in key.modules.iter() {
            if context.check_canceled().is_err() {
                return Err(SemanticNucleusBatchFailure::Query(QueryAbort::Canceled));
            }
            // Declaration-semantics projection splits into a per-module
            // occurrence index and a per-declaration nucleus request; both were
            // previously inside the pipeline's unattributed residual (RUE-786).
            let index_span =
                tracing::info_span!("declaration_occurrence_index", phase = "semantic_analysis")
                    .entered();
            let terminal = context
                .query_registered(
                    declaration_occurrence_indexes,
                    ModuleQueryKey(module.clone()),
                )
                .map_err(SemanticNucleusBatchFailure::Query)?;
            drop(index_span);
            let rue_query::QueryOutcome::Success(indexed) = terminal.outcome() else {
                unreachable!("DeclarationOccurrenceIndex publishes typed values")
            };
            let DeclarationOccurrenceIndexValue::Available(index) = indexed else {
                let DeclarationOccurrenceIndexValue::Failure(failure) = indexed else {
                    unreachable!()
                };
                return Err(SemanticNucleusBatchFailure::Stable {
                    declaration: None,
                    failure: Box::new(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(Arc::from(
                            format!("{failure:?}"),
                        )),
                    ),
                });
            };
            let terminal = context
                .query_registered(declaration_orders, ModuleQueryKey(module.clone()))
                .map_err(SemanticNucleusBatchFailure::Query)?;
            let rue_query::QueryOutcome::Success(order) = terminal.outcome() else {
                unreachable!("DeclarationOrder publishes typed values")
            };
            let DeclarationOrderValue::Available(order) = order else {
                let DeclarationOrderValue::Failure(failure) = order else {
                    unreachable!()
                };
                return Err(SemanticNucleusBatchFailure::Stable {
                    declaration: None,
                    failure: Box::new(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(Arc::from(
                            format!("{failure:?}"),
                        )),
                    ),
                });
            };
            let mut functions = BTreeMap::new();
            let mut function_names = BTreeMap::new();
            let mut type_names = BTreeMap::new();
            let mut structs = BTreeMap::new();
            let mut enums = BTreeMap::new();
            for candidate in order.iter().filter(|candidate| candidate.owner.is_none()) {
                use crate::declaration_candidate::DeclarationCandidateCategory as C;
                let name = candidate.name.clone();
                let duplicate = match candidate.category {
                    C::Function | C::ExternFunction => {
                        if let Some(first) = functions.get(&name) {
                            Some((
                                first,
                                rue_error::ErrorKind::DuplicateFunctionDefinition {
                                    function_name: name.to_string(),
                                },
                            ))
                        } else {
                            functions.insert(name.clone(), candidate.clone());
                            type_names.get(&name).map(|first| {
                                (
                                    first,
                                    rue_error::ErrorKind::DuplicateMixedKindDefinition {
                                        name: name.to_string(),
                                    },
                                )
                            })
                        }
                    }
                    C::Struct => {
                        if let Some(first) = function_names.get(&name) {
                            Some((
                                first,
                                rue_error::ErrorKind::DuplicateMixedKindDefinition {
                                    name: name.to_string(),
                                },
                            ))
                        } else if let Some(first) = structs.get(&name) {
                            Some((
                                first,
                                rue_error::ErrorKind::DuplicateTypeDefinition {
                                    type_name: format!("struct `{name}`"),
                                },
                            ))
                        } else {
                            enums.get(&name).map(|first| {
                                (
                                    first,
                                    rue_error::ErrorKind::DuplicateTypeDefinition {
                                        type_name: format!("struct `{name}` (conflicts with enum)"),
                                    },
                                )
                            })
                        }
                    }
                    C::Enum => {
                        if let Some(first) = function_names.get(&name) {
                            Some((
                                first,
                                rue_error::ErrorKind::DuplicateMixedKindDefinition {
                                    name: name.to_string(),
                                },
                            ))
                        } else if let Some(first) = enums.get(&name) {
                            Some((
                                first,
                                rue_error::ErrorKind::DuplicateTypeDefinition {
                                    type_name: format!("enum `{name}`"),
                                },
                            ))
                        } else {
                            structs.get(&name).map(|first| {
                                (
                                    first,
                                    rue_error::ErrorKind::DuplicateTypeDefinition {
                                        type_name: format!("enum `{name}` (conflicts with struct)"),
                                    },
                                )
                            })
                        }
                    }
                    C::ConstCandidate | C::Destructor | C::Method | C::AssociatedFunction => None,
                };
                if let Some((first, kind)) = duplicate {
                    duplicate_declarations.push(
                        crate::semantic_query_nucleus::DuplicateDeclarationFailure {
                            kind,
                            first: first.clone(),
                            duplicate: candidate.clone(),
                        },
                    );
                }
                match candidate.category {
                    C::Function | C::ExternFunction => {
                        function_names
                            .entry(name)
                            .or_insert_with(|| candidate.clone());
                    }
                    C::Struct => {
                        type_names
                            .entry(name.clone())
                            .or_insert_with(|| candidate.clone());
                        structs.entry(name).or_insert_with(|| candidate.clone());
                    }
                    C::Enum => {
                        type_names
                            .entry(name.clone())
                            .or_insert_with(|| candidate.clone());
                        enums.entry(name).or_insert_with(|| candidate.clone());
                    }
                    C::ConstCandidate | C::Destructor | C::Method | C::AssociatedFunction => {}
                }
            }
            let mut members =
                BTreeMap::<_, crate::declaration_candidate::DeclarationCandidateKey>::new();
            for candidate in order.iter().filter(|candidate| {
                matches!(
                    candidate.category,
                    Category::Method | Category::AssociatedFunction
                )
            }) {
                let Some(owner) = &candidate.owner else {
                    continue;
                };
                let member_key = (owner.clone(), candidate.name.clone());
                if let Some(first) = members.get(&member_key) {
                    duplicate_declarations.push(
                        crate::semantic_query_nucleus::DuplicateDeclarationFailure {
                            kind: rue_error::ErrorKind::DuplicateMethod {
                                type_name: owner.name.to_string(),
                                method_name: candidate.name.to_string(),
                            },
                            first: first.clone(),
                            duplicate: candidate.clone(),
                        },
                    );
                } else {
                    members.insert(member_key, candidate.clone());
                }
            }
            if !duplicate_declarations.is_empty() {
                continue;
            }
            for capability in index.capabilities.values() {
                let DeclarationOccurrenceCapability::Exact { .. } = capability else {
                    return Err(SemanticNucleusBatchFailure::Stable {
                        declaration: Some(capability.key().clone()),
                        failure: Box::new(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(
                                Arc::from(format!(
                                    "ambiguous declaration `{}`",
                                    capability.key().name
                                )),
                            ),
                        ),
                    });
                };
                let declaration = capability.key().clone();
                let query = DeclarationQuery {
                    declaration: declaration.clone(),
                    configuration: configuration.clone(),
                };
                let request = |key: Key| {
                    let _span =
                        tracing::info_span!("declaration_nucleus", phase = "semantic_analysis")
                            .entered();
                    let terminal = context
                        .query_registered(semantic_nucleus, key.clone())
                        .map_err(SemanticNucleusBatchFailure::Query)?;
                    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                        unreachable!("SemanticNucleus publishes typed values")
                    };
                    match value {
                        Value::Failure(failure) => Err(SemanticNucleusBatchFailure::Stable {
                            declaration: Some(declaration.clone()),
                            failure: Box::new(failure.clone()),
                        }),
                        value => Ok(value.clone()),
                    }
                };
                let semantic = if declaration.category == Category::ConstCandidate {
                    let Value::ConstResolution(resolution) =
                        request(Key::ConstResolution(query.clone()))?
                    else {
                        unreachable!("const query returned the wrong projection")
                    };
                    if let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                        anonymous_nominals: projected,
                        dependencies: projected_dependencies,
                        deferred_ownership,
                        ..
                    } = &resolution
                    {
                        anonymous_nominals.extend(
                            projected
                                .iter()
                                .cloned()
                                .map(|value| (value.identity.clone(), value)),
                        );
                        dependencies.extend(projected_dependencies.iter().cloned());
                        for gate in deferred_ownership.iter() {
                            let Value::DeferredOwnership = request(Key::DeferredOwnership(
                                crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                                    producer: query.clone(),
                                    gate: gate.clone(),
                                },
                            ))?
                            else {
                                unreachable!(
                                    "deferred ownership query returned the wrong projection"
                                )
                            };
                        }
                    }
                    let terminal = context
                        .query_registered(
                            declaration_shells,
                            DeclarationShellQueryKey(declaration.clone()),
                        )
                        .map_err(SemanticNucleusBatchFailure::Query)?;
                    let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(
                        shell,
                    )) = terminal.outcome()
                    else {
                        return Err(SemanticNucleusBatchFailure::Stable {
                            declaration: Some(declaration.clone()),
                            failure: Box::new(
                                crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(
                                    Arc::from("const declaration shell became unavailable"),
                                ),
                            ),
                        });
                    };
                    DeclarationSemanticValue::from_const(shell.is_public, resolution)
                } else {
                    let Value::Identity(identity) = request(Key::Identity(query.clone()))? else {
                        unreachable!("identity query returned the wrong projection")
                    };
                    if matches!(declaration.category, Category::Struct | Category::Enum) {
                        let Value::NominalWellFormedness =
                            request(Key::NominalWellFormedness(query.clone()))?
                        else {
                            unreachable!("nominal well-formedness returned the wrong projection")
                        };
                    }
                    let Value::Signature(signature) = request(Key::Signature(query.clone()))?
                    else {
                        unreachable!("signature query returned the wrong projection")
                    };
                    if let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                        parameters,
                        result,
                        is_extern: true,
                        ..
                    } = &signature.signature
                    {
                        // A foreign declaration names the C symbol it declares,
                        // so `extern "C" fn main` binds the program's own entry
                        // point rather than anything external (spec 9.3:6).
                        // Checked before the redeclaration comparison: signature
                        // agreement with the entry point is not a defence.
                        if declaration.name.as_ref() == "main" {
                            return Err(SemanticNucleusBatchFailure::Stable {
                                declaration: None,
                                failure: Box::new(
                                    crate::semantic_query_nucleus::SemanticNucleusFailure::DiagnosticAtDeclaration {
                                        kind: rue_error::ErrorKind::ForeignEntryPointDeclaration,
                                        declaration: declaration.clone(),
                                    },
                                ),
                            });
                        }
                        if let Some((previous, previous_parameters, previous_result)) =
                            foreign_declarations.get(&declaration.name)
                            && !foreign_signatures_agree(
                                previous_parameters,
                                previous_result,
                                parameters,
                                result,
                            )
                        {
                            use crate::semantic_query_nucleus::{
                                ForeignSignatureConflictFailure, ForeignSignatureSite,
                                SemanticNucleusFailure,
                            };
                            return Err(SemanticNucleusBatchFailure::Stable {
                                declaration: None,
                                failure: Box::new(SemanticNucleusFailure::ForeignSignatureConflict(
                                    ForeignSignatureConflictFailure {
                                        symbol: declaration.name.clone(),
                                        left: ForeignSignatureSite {
                                            declaration: previous.clone(),
                                            signature: Arc::from(foreign_signature_display(
                                                previous_parameters,
                                                previous_result,
                                            )),
                                        },
                                        right: ForeignSignatureSite {
                                            declaration: declaration.clone(),
                                            signature: Arc::from(foreign_signature_display(
                                                parameters, result,
                                            )),
                                        },
                                    },
                                )),
                            });
                        }
                        foreign_declarations.entry(declaration.name.clone()).or_insert_with(|| {
                            (declaration.clone(), parameters.clone(), result.clone())
                        });
                    }
                    for gate in signature.deferred_ownership.iter() {
                        let Value::DeferredOwnership = request(Key::DeferredOwnership(
                            crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                                producer: query.clone(),
                                gate: gate.clone(),
                            },
                        ))?
                        else {
                            unreachable!("deferred ownership query returned the wrong projection")
                        };
                    }
                    anonymous_nominals.extend(
                        signature
                            .anonymous_nominals
                            .iter()
                            .cloned()
                            .map(|value| (value.identity.clone(), value)),
                    );
                    dependencies.extend(signature.dependencies.iter().cloned());
                    let is_c_export = matches!(
                        &signature.signature,
                        crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                            is_c_export: true,
                            ..
                        }
                    );
                    let semantic =
                        DeclarationSemanticValue::from_signature(identity, signature.signature);
                    if is_c_export {
                        c_export_roots.insert(semantic.identity.key.clone());
                    }
                    semantic
                };
                values.push(crate::DurableDeclarationSemantic {
                    key: semantic.identity.key,
                    is_public: semantic.identity.is_public,
                    payload: semantic.payload,
                });
            }
        }
        if !duplicate_declarations.is_empty() {
            return Err(SemanticNucleusBatchFailure::Stable {
                declaration: None,
                failure: Box::new(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::DuplicateDeclarations(
                        duplicate_declarations.into(),
                    ),
                ),
            });
        }
        values.sort_by(|left, right| left.key.cmp(&right.key));
        let declarations: Arc<[crate::DurableDeclarationSemantic]> = values.into();
        let declaration_index = Arc::new(
            crate::local_semantic_materialization::SharedDeclarationFactIndex::new(&declarations),
        );
        Ok(SemanticNucleusProjection {
            declarations,
            declaration_index,
            anonymous_nominals: anonymous_nominals.into_values().collect::<Vec<_>>().into(),
            dependencies: dependencies.into_iter().collect::<Vec<_>>().into(),
            c_export_roots: c_export_roots.into_iter().collect::<Vec<_>>().into(),
        })
    }

    pub(crate) fn begin_import_inputs(
        &mut self,
        snapshot: &SourceSnapshot,
        context: ImportDiscoveryContext,
        accepted_reads: AcceptedReadManifest,
    ) -> CompileResult<ImportInputRevision> {
        self.next_import_request += 1;
        let generation = self.next_import_request;
        self.current_import_revision = None;
        self.lineage_additions.clear();
        // A new request is a fresh filesystem observation epoch only when the
        // observation *regime* changed. Under an unchanged regime the published
        // revision carries the same compatibility token as its predecessor, so
        // retained terminals stay eligible for red/green validation (RUE-1137,
        // ADR-0063 §2.1). The API still has no carried-ledger input that could
        // be mistaken for freshness authority.
        //
        // Carrying the token forward asserts that inputs this request did not
        // re-observe are unchanged. The compiler cannot verify that assertion
        // because ADR-0051 forbids it from touching the filesystem. Filesystem
        // hosts must establish Tier B authority before this call by sweeping the
        // previous rooted closure's accepted-read set. The CLI host implements
        // that request-start contract in `source_loader::reload_from_filesystem`
        // (RUE-1148): metadata matches reuse cached bytes, mismatches and
        // too-recent mtimes hash content, and only a digest change replaces the
        // source leaf. In-memory hosts already publish their explicit snapshots.
        self.publish_import_view(
            snapshot,
            context,
            accepted_reads,
            ImportObservationLedger::default(),
            generation,
            0,
        )
    }

    pub(crate) fn import_frontier(
        &mut self,
        revision: ImportInputRevision,
        plan: &ImportDiscoveryPlan,
        mode: ImportDemandMode,
        roots: &ImportDemandRoots,
    ) -> CompileResult<ImportDemandFrontier> {
        if self.current_import_revision != Some(revision) {
            return Err(import_input_error(
                "import demand requested from a non-current immutable revision",
            ));
        }
        let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
        let view = {
            let store = lock_import_store(&self.import_store);
            store
                .revisions
                .iter()
                .find(|view| view.revision == runtime_revision)
                .cloned()
        }
        .ok_or_else(|| import_input_error("import input revision is no longer retained"))?;
        if plan.context() != &view.context || plan.source_revision() != &view.sources {
            return Err(import_input_error(
                "import plan does not match its pinned granular input revision",
            ));
        }
        // Membership is proven per root by binary search over the plan's shared,
        // canonically ordered group segments, so the guard costs O(roots · log
        // plan) and never materializes the merged plan. Groups are ordered by
        // their first request, whose leading fields are the plan-wide discovery
        // context and then the occurrence, so searching on that pair is exact.
        // The search direction is also the safe one: a comparator disagreeing
        // with the stored order could only fail to find a group and reject a
        // legitimate root, never admit one the plan does not contain.
        {
            let segments = plan.group_segments();
            if roots.occurrences().iter().any(|occurrence| {
                !segments.contains_by(|group| {
                    group[0]
                        .context()
                        .cmp(plan.context())
                        .then_with(|| group[0].occurrence().cmp(occurrence))
                })
            }) {
                return Err(import_input_error(
                    "import demand roots contain an occurrence outside the pinned plan",
                ));
            }
        }
        let mut requests = Vec::new();
        let mut fanout = Vec::<Vec<ImportDiscoveryRequest>>::new();
        let mut operation_indices =
            BTreeMap::<crate::import_discovery::ImportHostOperationKey, usize>::new();
        let mut speculative_blocked = false;
        self.import_frontier_roots_requested = self
            .import_frontier_roots_requested
            .saturating_add(roots.occurrences().len() as u64);
        for occurrence in roots.occurrences() {
            let key = ResolveImportKey {
                occurrence: occurrence.clone(),
                mode,
            };
            let attempt = self.runtime.request_registered(
                &self.resolve_imports,
                runtime_revision,
                key,
                CancellationToken::new(),
            );
            let terminal = attempt.terminal().ok_or_else(|| {
                import_input_error(format!(
                    "ResolveImport query aborted: {:?}",
                    attempt.abort()
                ))
            })?;
            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                unreachable!("ResolveImport publishes typed success values")
            };
            if !value.site_found {
                return Err(import_input_error(
                    "import demand occurrence is absent from the current parsed module",
                ));
            }
            speculative_blocked |= value.speculative_blocked;
            for request in value.requests.iter() {
                let operation = crate::import_discovery::ImportHostOperationKey::new(request);
                if let Some(index) = operation_indices.get(&operation).copied() {
                    fanout[index].push(request.clone());
                } else {
                    let index = requests.len();
                    operation_indices.insert(operation, index);
                    requests.push(request.clone());
                    fanout.push(vec![request.clone()]);
                }
            }
        }
        Ok(ImportDemandFrontier {
            revision,
            mode,
            requests: requests.into(),
            fanout: fanout
                .into_iter()
                .map(|requests| Arc::<[ImportDiscoveryRequest]>::from(requests))
                .collect::<Vec<_>>()
                .into(),
            speculative_blocked,
        })
    }

    pub(crate) fn current_import_revision(&self) -> Option<ImportInputRevision> {
        self.current_import_revision
    }

    /// Cumulative import-occurrence roots dispatched by [`Self::import_frontier`].
    /// See the field docs on `import_frontier_roots_requested`.
    pub(crate) fn import_frontier_roots_requested(&self) -> u64 {
        self.import_frontier_roots_requested
    }

    /// Cumulative close-time `ResolveImport` projections dispatched by
    /// [`Self::exact_import_groups`]. See the field docs on
    /// `exact_import_groups_dispatched`.
    pub(crate) fn exact_import_groups_dispatched(&self) -> u64 {
        self.exact_import_groups_dispatched
    }

    /// Cumulative leaves published through the complete
    /// [`Self::publish_import_view`] path (fresh generations). Scales with the
    /// program; never used on the successor overlay path.
    pub(crate) fn import_view_full_leaves_published(&self) -> u64 {
        self.import_view_full_leaves_published
    }

    /// Cumulative leaves published through the sparse successor overlay path
    /// ([`Self::publish_import_view_overlay`]): delta leaves plus the one
    /// re-stamped aggregate topology leaf. Predecessor leaves are structurally
    /// inherited and never counted here, so the acquisition delta is O(new
    /// leaves), independent of the predecessor topology.
    pub(crate) fn import_view_overlay_leaves_published(&self) -> u64 {
        self.import_view_overlay_leaves_published
    }

    /// Cumulative ledger observations deep-copied while cloning view ledgers
    /// (each clone copies only the cloned value's recorded delta; frozen
    /// predecessor segments are shared by `Arc`).
    pub(crate) fn import_view_ledger_entries_cloned(&self) -> u64 {
        self.import_view_ledger_entries_cloned
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Predecessor source entries compared by the overlay publication's fallback
    /// diff; zero whenever the structural-authority path ran.
    pub(crate) fn import_view_source_entries_compared(&self) -> u64 {
        self.import_view_source_entries_compared
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Predecessor accepted-read entries compared by the overlay publication's
    /// fallback provenance diff; zero whenever the structural-authority path
    /// ran.
    pub(crate) fn import_view_read_entries_compared(&self) -> u64 {
        self.import_view_read_entries_compared
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The module-identity and physical-identity resolution counters.
    /// See [`crate::source_snapshot::IdentityResolutionMeter`].
    pub(crate) fn identity_resolution(&self) -> &crate::source_snapshot::IdentityResolutionMeter {
        &self.identity_resolution
    }

    /// The module revisions appended by overlay publications since the last
    /// committed close — the recorded-additions lineage (RUE-1112).
    pub(crate) fn lineage_additions(&self) -> &[ModuleRevision] {
        &self.lineage_additions
    }

    /// Reset the recorded-additions lineage at a committed close boundary.
    pub(crate) fn clear_lineage_additions(&mut self) {
        self.lineage_additions.clear();
    }

    pub(crate) fn exact_import_groups(
        &mut self,
        revision: ImportInputRevision,
        roots: &ImportDemandRoots,
    ) -> CompileResult<Vec<Arc<[ImportDiscoveryRequest]>>> {
        if self.current_import_revision != Some(revision) {
            return Err(import_input_error(
                "exact import projection requested from a non-current revision",
            ));
        }
        self.exact_import_groups_dispatched = self
            .exact_import_groups_dispatched
            .saturating_add(roots.occurrences().len() as u64);
        let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
        let mut groups = Vec::new();
        for occurrence in roots.occurrences() {
            let attempt = self.runtime.request_registered(
                &self.resolve_imports,
                runtime_revision,
                ResolveImportKey {
                    occurrence: occurrence.clone(),
                    mode: ImportDemandMode::Rooted,
                },
                CancellationToken::new(),
            );
            let terminal = attempt.terminal().ok_or_else(|| {
                import_input_error(format!(
                    "ResolveImport projection aborted: {:?}",
                    attempt.abort()
                ))
            })?;
            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                unreachable!("ResolveImport publishes typed values")
            };
            if !value.site_found {
                return Err(import_input_error(
                    "exact import projection occurrence is absent from the current parsed module",
                ));
            }
            groups.extend(value.groups.iter().cloned());
        }
        groups.sort_by(|left, right| left[0].cmp(&right[0]));
        Ok(groups)
    }

    /// RUE-1576: how many declaration publications could not retain their
    /// projection cone this session. Expected zero.
    pub(crate) fn publication_cone_retention_failures(&self) -> u64 {
        self.publication_cone_retention_failures
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Stage a wave's eager parses for the canonical parse query.
    ///
    /// A newer stage for the same module replaces an older unconsumed one, and
    /// consumption still verifies exact `SourceId` identity, so a stale entry
    /// can only ever be discarded, never used.
    pub(crate) fn stage_module_parses(
        &self,
        staged: Vec<crate::parsed_modules::StagedModuleParse>,
    ) {
        if staged.is_empty() {
            return;
        }
        let mut stage = self
            .parse_stage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for parse in staged {
            stage.insert(parse.module().clone(), parse);
        }
    }

    pub(crate) fn publish_import_batch(
        &mut self,
        frontier: &ImportDemandFrontier,
        snapshot: &SourceSnapshot,
        accepted_reads: AcceptedReadManifest,
        observations: Vec<ImportObservation>,
    ) -> CompileResult<ImportInputRevision> {
        if frontier.mode != ImportDemandMode::Rooted {
            return Err(import_input_error(
                "speculative import work cannot publish host observations",
            ));
        }
        if self.current_import_revision != Some(frontier.revision) {
            return Err(import_input_error(
                "import batch belongs to a stale immutable revision",
            ));
        }
        if observations.len() != frontier.requests.len()
            || observations
                .iter()
                .zip(frontier.requests.iter())
                .any(|(observation, request)| observation.request() != request)
        {
            return Err(import_input_error(
                "host import results must exactly preserve the compiler-produced batch order",
            ));
        }
        let mut ledger = {
            let store = lock_import_store(&self.import_store);
            let view = store
                .revisions
                .iter()
                .find(|view| view.revision.id() == frontier.revision.revision_id)
                .ok_or_else(|| import_input_error("import input revision is no longer retained"))?;
            // The persistent ledger clone deep-copies only the parent value's
            // recorded delta; frozen predecessor segments are shared by `Arc`.
            self.import_view_ledger_entries_cloned.fetch_add(
                view.ledger.recorded_delta().len() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            view.ledger.clone()
        };
        for (observation, fanout) in observations.into_iter().zip(frontier.fanout.iter()) {
            for request in fanout.iter().cloned() {
                ledger.record(observation.fanout_to(request)?)?;
            }
        }
        // Publish the successor as a sparse overlay over the current view: only
        // the batch's own additions become leaves, the aggregate topology is
        // re-stamped, and every predecessor leaf is structurally inherited. The
        // additions are re-derived and justified against the batch's accepted
        // observations inside the overlay publication, so an unrelated module in
        // the supplied snapshot or manifest is rejected there.
        self.publish_import_view_overlay(
            frontier.revision,
            snapshot,
            accepted_reads,
            ledger,
            OverlayJustification::BatchAccepted,
            frontier.revision.frontier_round + 1,
        )
    }

    /// Publish a strictly-additive trusted-toolchain successor input revision
    /// (RUE-1112) as a sparse overlay over the current published view. Unlike
    /// [`Self::publish_import_batch`] this carries no new import observation: the
    /// appended leaves' own `@import` edges are not yet observed here (the
    /// driver's subsequent re-close discovers them), so the carried ledger and the
    /// aggregate topology are inherited unchanged and only the appended leaves'
    /// source/provenance leaves are published. The additions are re-derived from
    /// the parent view and must equal the capability-verified `added` set exactly.
    pub(crate) fn publish_trusted_successor_view(
        &mut self,
        parent: ImportInputRevision,
        snapshot: &SourceSnapshot,
        accepted_reads: AcceptedReadManifest,
        ledger: ImportObservationLedger,
        added: &std::collections::BTreeSet<ModuleId>,
        frontier_round: u64,
    ) -> CompileResult<ImportInputRevision> {
        self.publish_import_view_overlay(
            parent,
            snapshot,
            accepted_reads,
            ledger,
            OverlayJustification::TrustedLeaves(added),
            frontier_round,
        )
    }

    pub(crate) fn import_ledger(
        &self,
        revision: ImportInputRevision,
    ) -> CompileResult<ImportObservationLedger> {
        let store = lock_import_store(&self.import_store);
        store
            .revisions
            .iter()
            .find(|view| {
                view.revision.id() == revision.revision_id
                    && view.generation == revision.request_generation
            })
            .map(|view| view.ledger.clone())
            .ok_or_else(|| import_input_error("import input revision is no longer retained"))
    }

    /// The complete published state of the current import-input revision: its
    /// snapshot, context, accepted-read provenance, and carried ledger
    /// (RUE-1112). The trusted-toolchain successor stage/close consume THIS
    /// state rather than any host-supplied replacement, so a caller cannot
    /// substitute a snapshot, context, provenance manifest, or ledger that
    /// diverges from what the compiler published.
    pub(crate) fn current_import_view_state(
        &self,
    ) -> Option<(
        ImportInputRevision,
        SourceSnapshot,
        ImportDiscoveryContext,
        AcceptedReadManifest,
        ImportObservationLedger,
        ImportInputTransition,
    )> {
        let current = self.current_import_revision?;
        let runtime = Revision::new(current.revision_id, current.compatibility_token);
        let view = {
            let store = lock_import_store(&self.import_store);
            store
                .revisions
                .iter()
                .find(|view| view.revision == runtime)
                .cloned()
        }?;
        let snapshot = {
            let store = self
                .module_store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store
                .revisions
                .iter()
                .find(|module_view| module_view.revision == runtime)
                .map(|module_view| module_view.snapshot.clone())
        }?;
        self.import_view_ledger_entries_cloned.fetch_add(
            view.ledger.recorded_delta().len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        Some((
            current,
            snapshot,
            view.context.clone(),
            view.accepted_reads.clone(),
            view.ledger.clone(),
            view.transition.clone(),
        ))
    }

    pub(super) fn publish_import_view(
        &mut self,
        snapshot: &SourceSnapshot,
        context: ImportDiscoveryContext,
        accepted_reads: AcceptedReadManifest,
        ledger: ImportObservationLedger,
        generation: u64,
        frontier_round: u64,
    ) -> CompileResult<ImportInputRevision> {
        let source_revision = snapshot.source_revision().clone();
        let sources = source_revision.modules();
        let provenance = accepted_reads
            .iter()
            .map(|entry| (entry.module(), entry))
            .collect::<BTreeMap<_, _>>();
        crate::import_discovery::validate_accepted_import_manifest(&accepted_reads)?;
        if provenance.len() != accepted_reads.len() {
            return Err(import_input_error(
                "accepted read manifest contains duplicate logical modules",
            ));
        }
        if sources
            .iter()
            .any(|source| !provenance.contains_key(&source.module))
        {
            return Err(import_input_error(
                "every module source leaf requires accepted-read provenance",
            ));
        }
        if ledger
            .iter()
            .any(|observation| observation.request().context() != &context)
        {
            return Err(import_input_error(
                "import observation belongs to a different discovery epoch",
            ));
        }
        let accepted_topology = AcceptedImportTopologyValue::Full(accepted_import_topology(
            ledger.iter(),
            &accepted_reads,
            &self.identity_resolution,
        )?);
        // RUE-1137/RUE-1202: the runtime revision's compatibility slot carries
        // one shared observation namespace for both ordinary updates and
        // rooted publication. The first rooted request may bind an existing
        // ordinary lineage to its context; a later context change starts the
        // context-derived regime. File changes remain per-leaf stamp changes.
        let compatibility_token = self.compatibility_token_for_import_context(&context);
        let revision = Revision::new(self.next_revision, compatibility_token);
        self.next_revision += 1;
        let mut leaves = Vec::new();
        let (accepted_topology_stamp, stamp_lease) = {
            let mut store = lock_import_store(&self.import_store);
            let ImportInputStore {
                next_stamp,
                context_stamps,
                provenance_stamps,
                observation_stamps,
                topology_stamps,
                ..
            } = &mut *store;
            let accepted_topology_stamp =
                exact_value_stamp(next_stamp, topology_stamps, &accepted_topology);
            leaves.push((
                accepted_import_topology_input(frontier_round),
                accepted_topology_stamp,
            ));
            retain_stamp_value(topology_stamps, &accepted_topology);
            leaves.push((
                import_context_input(),
                exact_value_stamp(next_stamp, context_stamps, &context),
            ));
            retain_stamp_value(context_stamps, &context);
            for source in sources.iter() {
                let accepted = provenance[&source.module];
                leaves.push((
                    accepted_read_input(&source.module),
                    exact_value_stamp(next_stamp, provenance_stamps, accepted),
                ));
            }
            for accepted in accepted_reads.iter() {
                leaves.push((
                    accepted_import_provenance_input(accepted.metadata_identity()),
                    exact_value_stamp(next_stamp, provenance_stamps, accepted),
                ));
                retain_stamp_value(provenance_stamps, accepted);
            }
            for observation in ledger.iter() {
                leaves.push((
                    import_observation_input(observation.request()),
                    exact_value_stamp(next_stamp, observation_stamps, observation),
                ));
                retain_stamp_value(observation_stamps, observation);
            }
            let stamp_lease = Arc::new(ImportInputStampLease {
                parent: None,
                context: Some(context.clone()),
                provenance: accepted_reads.iter().cloned().collect::<Vec<_>>().into(),
                observations: ledger.iter().cloned().collect::<Vec<_>>().into(),
                topology: Some(accepted_topology.clone()),
            });
            (accepted_topology_stamp, stamp_lease)
        };
        leaves.extend(publish_module_inputs(
            &self.module_store,
            revision,
            snapshot,
        ));
        let published_leaf_count = leaves.len() as u64;
        if let Err(error) = self.runtime.publish_revision(revision, leaves) {
            release_orphaned_import_stamp_leases(
                &mut lock_import_store(&self.import_store),
                stamp_lease,
            );
            discard_module_input_view(&self.module_store, revision);
            return Err(import_input_error(format!(
                "cannot publish import revision: {error:?}"
            )));
        }
        commit_module_input_view(&self.module_store, revision);
        self.import_view_full_leaves_published = self
            .import_view_full_leaves_published
            .saturating_add(published_leaf_count);
        self.active_compatibility_token = compatibility_token;
        self.active_import_context = Some(context.clone());
        let view = Arc::new(ImportInputView {
            revision,
            generation,
            transition: ImportInputTransition::Fresh,
            context,
            sources: source_revision,
            accepted_reads,
            ledger,
            accepted_topology_stamp,
            accepted_topology,
            stamp_lease,
        });
        let mut store = lock_import_store(&self.import_store);
        retain_import_input_view(&mut store, view);
        let published = ImportInputRevision {
            revision_id: revision.id(),
            request_generation: generation,
            compatibility_token,
            frontier_round,
        };
        self.current_import_revision = Some(published);
        Ok(published)
    }

    /// Publish a same-generation successor input view as a sparse immutable
    /// overlay over the CURRENT published view (RUE-1112).
    ///
    /// The successor's leaves are derived here, never supplied: sorted
    /// two-pointer diffs against the parent view yield exactly the added module
    /// sources, accepted reads, and observations, and every parent entry must
    /// reappear byte-identical (a mutated or dropped predecessor source, read, or
    /// observation rejects the publication — the lineage is strictly additive at
    /// this boundary, closing the batch-injection route). Only those delta leaves
    /// plus, when observations grew, the one re-stamped aggregate topology leaf
    /// are published through the runtime's sparse overlay; predecessor leaves are
    /// structurally inherited and never rehashed, revalidated, or republished.
    pub(super) fn publish_import_view_overlay(
        &mut self,
        parent: ImportInputRevision,
        snapshot: &SourceSnapshot,
        accepted_reads: AcceptedReadManifest,
        ledger: ImportObservationLedger,
        justification: OverlayJustification<'_>,
        frontier_round: u64,
    ) -> CompileResult<ImportInputRevision> {
        if self.current_import_revision != Some(parent) {
            return Err(import_input_error(
                "a successor overlay must extend the current published revision",
            ));
        }
        let parent_runtime = Revision::new(parent.revision_id, parent.compatibility_token);
        let parent_view = {
            let store = lock_import_store(&self.import_store);
            store
                .revisions
                .iter()
                .find(|view| view.revision == parent_runtime)
                .cloned()
        }
        .ok_or_else(|| import_input_error("import input revision is no longer retained"))?;

        // Source additions come from STRUCTURAL AUTHORITY: direct lineage
        // pointer identity proves the parent and retains the exact newest delta
        // even when the storage tiers compact. A rebuilt snapshot falls back to
        // the explicit byte-identical two-pointer diff.
        let successor_segments = snapshot.source_revision().module_segments();
        let parent_segments = parent_view.sources.module_segments();
        let structural_sources = successor_segments
            .direct_delta_from(parent_segments)
            .map(<[crate::ModuleRevision]>::to_vec);
        let new_sources = match structural_sources {
            Some(appended) => appended,
            None => {
                self.import_view_source_entries_compared.fetch_add(
                    parent_view.sources.modules().len() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                additive_diff(
                    parent_view.sources.modules().iter(),
                    snapshot.source_revision().modules().iter(),
                    |a, b| a.module.cmp(&b.module),
                    "module source",
                )?
            }
        };
        // Accepted-read provenance uses the same direct-lineage proof.
        let structural_reads = accepted_reads
            .segments()
            .direct_delta_from(parent_view.accepted_reads.segments())
            .map(<[crate::AcceptedReadManifestEntry]>::to_vec);
        let new_reads = match structural_reads {
            Some(appended) => appended,
            None => {
                self.import_view_read_entries_compared.fetch_add(
                    parent_view.accepted_reads.len() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                additive_diff(
                    parent_view.accepted_reads.iter(),
                    accepted_reads.iter(),
                    |a, b| a.module().cmp(b.module()),
                    "accepted-read provenance",
                )?
            }
        };
        let new_observations: Vec<ImportObservation> = ledger.recorded_delta().cloned().collect();

        // The additions must be EXACTLY what this step's justification derives —
        // set equality in both directions, not membership. A frontier batch's
        // accepted observations authorize exactly the newly resolved modules: an
        // unrelated module riding along in the snapshot/manifest is an
        // injection, and an authorized module missing from the snapshot or
        // manifest is an omission (topology would claim "resolved" with no
        // source leaf behind it); both reject. A trusted successor admits only
        // the capability-verified leaf set with no observations.
        let transition_is_host_batch =
            matches!(&justification, OverlayJustification::BatchAccepted);
        let authorized: std::collections::BTreeSet<ModuleId> = match justification {
            OverlayJustification::BatchAccepted => new_observations
                .iter()
                .filter_map(|observation| observation.accepted_source())
                .map(|source| {
                    crate::import_discovery::accepted_import_module(
                        source,
                        &accepted_reads,
                        &self.identity_resolution,
                    )
                })
                .collect::<Result<_, _>>()?,
            OverlayJustification::TrustedLeaves(added) => {
                if !new_observations.is_empty() {
                    return Err(import_input_error(
                        "a trusted successor carries no new import observations",
                    ));
                }
                added.clone()
            }
        };
        // Modules the authorization introduces that the parent does not already
        // carry (an accepted observation may re-resolve an existing module).
        let parent_has = |module: &ModuleId| {
            parent_view
                .sources
                .module_segments()
                .contains_by(|source| source.module.cmp(module))
        };
        let required_new: std::collections::BTreeSet<&ModuleId> = authorized
            .iter()
            .filter(|module| !parent_has(module))
            .collect();
        let new_source_ids: std::collections::BTreeSet<&ModuleId> =
            new_sources.iter().map(|source| &source.module).collect();
        let new_read_ids: std::collections::BTreeSet<&ModuleId> =
            new_reads.iter().map(|read| read.module()).collect();
        if new_source_ids != required_new {
            return Err(import_input_error(
                "successor overlay module sources must equal this step's authorized additions exactly",
            ));
        }
        if new_read_ids != required_new {
            return Err(import_input_error(
                "successor overlay accepted-read provenance must equal this step's authorized additions exactly",
            ));
        }
        for observation in &new_observations {
            if observation.request().context() != &parent_view.context {
                return Err(import_input_error(
                    "import observation belongs to a different discovery epoch",
                ));
            }
        }
        let added_topology = (!new_observations.is_empty())
            .then(|| {
                accepted_import_topology(
                    &new_observations,
                    &accepted_reads,
                    &self.identity_resolution,
                )
            })
            .transpose()?;
        let accepted_topology = added_topology.as_ref().map_or_else(
            || parent_view.accepted_topology.clone(),
            |added| AcceptedImportTopologyValue::Overlay {
                parent_stamp: parent_view.accepted_topology_stamp,
                added: added.clone(),
            },
        );

        // An overlay successor stays inside its parent's observation regime, so
        // it inherits the parent's compatibility token verbatim (RUE-1137).
        let revision = Revision::new(self.next_revision, parent.compatibility_token);
        self.next_revision += 1;
        let mut leaves = Vec::new();
        let (accepted_topology_stamp, stamp_lease) = {
            let mut store = lock_import_store(&self.import_store);
            let ImportInputStore {
                next_stamp,
                provenance_stamps,
                observation_stamps,
                topology_stamps,
                ..
            } = &mut *store;
            let accepted_topology_stamp = if added_topology.is_some() {
                // The observation set strictly grew, so the aggregate topology is
                // a genuinely new structural value. Its exact representation is
                // the parent stamp plus this overlay's sorted fact delta, so
                // lookup and retention stay O(delta) without a whole-ledger scan.
                let stamp = exact_value_stamp(next_stamp, topology_stamps, &accepted_topology);
                leaves.push((accepted_import_topology_input(frontier_round), stamp));
                retain_stamp_value(topology_stamps, &accepted_topology);
                stamp
            } else {
                parent_view.accepted_topology_stamp
            };
            for source in &new_sources {
                let accepted = accepted_reads
                    .find_module(&source.module)
                    .expect("delta provenance validated above");
                leaves.push((
                    accepted_read_input(&source.module),
                    exact_value_stamp(next_stamp, provenance_stamps, accepted),
                ));
            }
            for read in &new_reads {
                leaves.push((
                    accepted_import_provenance_input(read.metadata_identity()),
                    exact_value_stamp(next_stamp, provenance_stamps, read),
                ));
                retain_stamp_value(provenance_stamps, read);
            }
            for observation in &new_observations {
                leaves.push((
                    import_observation_input(observation.request()),
                    exact_value_stamp(next_stamp, observation_stamps, observation),
                ));
                retain_stamp_value(observation_stamps, observation);
            }
            let stamp_lease = Arc::new(ImportInputStampLease {
                parent: Some(parent_view.stamp_lease.clone()),
                context: None,
                provenance: new_reads.clone().into(),
                observations: new_observations.clone().into(),
                topology: added_topology.as_ref().map(|_| accepted_topology.clone()),
            });
            (accepted_topology_stamp, stamp_lease)
        };
        leaves.extend(publish_module_inputs_delta(
            &self.module_store,
            revision,
            snapshot,
            &new_sources,
        ));
        let published_leaf_count = leaves.len() as u64;
        if let Err(error) = self
            .runtime
            .publish_revision_overlay(revision, parent_runtime, leaves)
        {
            release_orphaned_import_stamp_leases(
                &mut lock_import_store(&self.import_store),
                stamp_lease,
            );
            discard_module_input_view(&self.module_store, revision);
            return Err(import_input_error(format!(
                "cannot publish successor overlay: {error:?}"
            )));
        }
        commit_module_input_view(&self.module_store, revision);
        self.import_view_overlay_leaves_published = self
            .import_view_overlay_leaves_published
            .saturating_add(published_leaf_count);
        // Record this step's exact additions on the session-owned lineage; the
        // successor stage/close derive their module delta from this record.
        self.lineage_additions.extend(new_sources.iter().cloned());
        let mut transition_additions = new_sources.clone();
        transition_additions.sort_by(|left, right| left.module.cmp(&right.module));
        let transition = if transition_is_host_batch {
            ImportInputTransition::HostBatch {
                parent,
                added: transition_additions.into(),
            }
        } else {
            ImportInputTransition::TrustedSuccessor {
                parent,
                added: transition_additions.into(),
            }
        };
        let view = Arc::new(ImportInputView {
            revision,
            generation: parent.request_generation,
            transition,
            context: parent_view.context.clone(),
            sources: snapshot.source_revision().clone(),
            accepted_reads,
            ledger,
            accepted_topology_stamp,
            accepted_topology,
            stamp_lease,
        });
        let mut store = lock_import_store(&self.import_store);
        retain_import_input_view(&mut store, view);
        let published = ImportInputRevision {
            revision_id: revision.id(),
            request_generation: parent.request_generation,
            compatibility_token: parent.compatibility_token,
            frontier_round,
        };
        self.current_import_revision = Some(published);
        Ok(published)
    }

    pub(crate) fn source_revision(
        &mut self,
        source: &crate::session::ExactSourceInput,
        snapshot: &SourceSnapshot,
    ) -> Revision {
        #[cfg(test)]
        {
            self.current_test_import_revision = None;
        }
        // Ordinary source publication stays in the active compatibility
        // namespace so retained terminals can validate across ordinary/rooted
        // protocol transitions (RUE-1202).
        // The parse family is allocated with the shared runtime now so callers
        // can migrate without creating a peer executor.
        let _parse_migration_family = &self.parse;
        let stamp = self
            .source_stamps
            .iter()
            .find_map(|(candidate, stamp)| (candidate == source).then_some(*stamp))
            .unwrap_or_else(|| {
                let stamp = self.next_source_stamp;
                self.next_source_stamp += 1;
                self.source_stamps.push_back((source.clone(), stamp));
                stamp
            });
        let revision = Revision::new(self.next_revision, self.active_compatibility_token);
        self.next_revision += 1;
        let mut leaves = vec![(InputIdentity::new(Self::SOURCE_INPUT, "current"), stamp)];
        leaves.extend(publish_module_inputs(
            &self.module_store,
            revision,
            snapshot,
        ));
        self.runtime
            .publish_revision(revision, leaves)
            .expect("compiler input revisions are immutable and uniquely numbered");
        commit_module_input_view(&self.module_store, revision);
        self.ordinary_lineage_published = true;
        revision
    }

    /// The input-leaf identity of one module's source content, for records
    /// that depend on exactly an appended segment's leaves (RUE-1112).
    pub(crate) fn module_source_input(module: &ModuleId) -> InputIdentity {
        module_source_input(module)
    }

    pub(super) fn parse_module_frontier(
        &self,
        revision: Revision,
        modules: Arc<[ModuleQueryKey]>,
    ) -> Result<(Arc<[ParseModuleValue]>, Vec<RequestExecution>, usize, usize), String> {
        if modules.is_empty() {
            return Ok((Arc::from([]), Vec::new(), 0, 0));
        }
        let attempt = self.runtime.request_registered(
            &self.parse_module_batches,
            revision,
            ParseModuleBatchKey {
                modules: modules.clone(),
            },
            CancellationToken::new(),
        );
        let batch_execution = attempt.execution();
        let child_lookups = attempt
            .nested_attempts()
            .iter()
            .filter(|nested| nested.node().family() == "compiler.parse-module")
            .count();
        let child_executions =
            frontier_child_executions(&attempt, "compiler.parse-module", modules.as_ref());
        let executions = if child_executions.iter().all(Option::is_none) {
            vec![batch_execution; modules.len()]
        } else {
            assert!(child_executions.iter().all(Option::is_some));
            child_executions
                .into_iter()
                .map(|execution| execution.unwrap().execution)
                .collect()
        };
        let overhead = attempt
            .work()
            .iter()
            .find_map(|(name, count)| {
                (name.as_ref() == "parse.frontier.overhead").then_some(*count as usize)
            })
            .unwrap_or(0);
        if attempt.terminal().is_none() {
            let detail = attempt
                .nested_attempts()
                .iter()
                .find_map(|child| {
                    child.abort().map(|abort| {
                        format!("ParseModule({}) aborted: {abort:?}", child.node().key())
                    })
                })
                .unwrap_or_else(|| format!("ParseModule frontier aborted: {:?}", attempt.abort()));
            return Err(detail);
        }
        let terminal = attempt
            .into_result()
            .expect("checked ParseModuleFrontier terminal remains available");
        let rue_query::QueryOutcome::Success(ParseModuleBatchValue(values)) = terminal.outcome()
        else {
            unreachable!("ParseModuleFrontier publishes typed values")
        };
        Ok((values.clone(), executions, overhead, child_lookups))
    }

    /// Parse ONLY a trusted successor's appended modules at the published
    /// overlay revision and structurally extend the retained predecessor
    /// program (RUE-1112). Predecessor modules are never re-dispatched,
    /// re-parsed, or re-enumerated; their leaves and parse terminals are
    /// inherited through the overlay lineage.
    pub(crate) fn parse_program_extension(
        &self,
        revision: Revision,
        predecessor: &Arc<ParsedProgram>,
        appended: &[(ModuleId, crate::FileId)],
    ) -> (
        Result<Arc<ParsedProgram>, crate::CompileErrors>,
        ParsedModulesWork,
    ) {
        let snapshot = self
            .module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revisions
            .iter()
            .find(|view| view.revision == revision)
            .expect("parse projection retains its module input revision")
            .snapshot
            .clone();
        let mut parsed = Vec::with_capacity(appended.len());
        let mut errors = crate::CompileErrors::new();
        let mut work = ParsedModulesWork {
            modules_considered: appended.len(),
            frontier_items: appended.len(),
            frontier_batches: usize::from(!appended.is_empty()),
            ..ParsedModulesWork::default()
        };
        let keys = appended
            .iter()
            .map(|(module, _)| ModuleQueryKey(module.clone()))
            .collect::<Vec<_>>()
            .into();
        let (values, executions, overhead, child_lookups) =
            match self.parse_module_frontier(revision, keys) {
                Ok(frontier) => frontier,
                Err(detail) => {
                    errors.push(import_input_error(detail));
                    return (Err(errors), work);
                }
            };
        work.frontier_batch_overhead = overhead;
        work.previous_module_lookups = child_lookups;
        for (((_module, file_id), value), execution) in appended
            .iter()
            .zip(values.iter())
            .zip(executions.into_iter())
        {
            let computed = matches!(execution, RequestExecution::Computed);
            if computed {
                work.modules_reparsed += 1;
                work.syntax.lexer_invocations += value.work.lexer_invocations;
                work.syntax.parser_invocations += value.work.parser_invocations;
                work.syntax.lexed_bytes += value.work.lexed_bytes;
                work.syntax.tokens += value.work.tokens;
            }
            match &value.result {
                Ok(module) => {
                    let projected = crate::parsed_modules::rebind_parsed_module(
                        &snapshot,
                        module,
                        &self.identity_resolution,
                    );
                    if !computed {
                        if Arc::ptr_eq(&projected, module) {
                            work.modules_reused += 1;
                        } else {
                            work.modules_rebound += 1;
                        }
                    }
                    parsed.push(projected);
                }
                Err(module_errors) => {
                    if !computed {
                        work.modules_reused += 1;
                    }
                    errors.extend(
                        module_errors
                            .clone()
                            .map_spans(|span| Span::with_file(*file_id, span.start, span.end)),
                    )
                }
            }
        }
        let result = if errors.is_empty() {
            ParsedProgram::extend_successor(predecessor, snapshot.source_revision().clone(), parsed)
                .map(Arc::new)
                .map_err(crate::CompileErrors::from)
        } else {
            Err(errors)
        };
        (result, work)
    }

    pub(crate) fn parse_program(
        &self,
        revision: Revision,
        root: &ModuleId,
        modules: impl IntoIterator<Item = ModuleId>,
    ) -> (
        Result<Arc<ParsedProgram>, crate::CompileErrors>,
        ParsedModulesWork,
    ) {
        let snapshot = self
            .module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revisions
            .iter()
            .find(|view| view.revision == revision)
            .expect("parse projection retains its module input revision")
            .snapshot
            .clone();
        let modules = modules.into_iter().collect::<Vec<_>>();
        let mut parsed = Vec::with_capacity(modules.len());
        let mut errors = crate::CompileErrors::new();
        let mut work = ParsedModulesWork {
            modules_considered: modules.len(),
            frontier_items: modules.len(),
            frontier_batches: usize::from(!modules.is_empty()),
            ..ParsedModulesWork::default()
        };
        let keys = modules
            .iter()
            .cloned()
            .map(ModuleQueryKey)
            .collect::<Vec<_>>()
            .into();
        let (values, executions, overhead, child_lookups) =
            match self.parse_module_frontier(revision, keys) {
                Ok(frontier) => frontier,
                Err(detail) => {
                    errors.push(import_input_error(detail));
                    return (Err(errors), work);
                }
            };
        work.frontier_batch_overhead = overhead;
        work.previous_module_lookups = child_lookups;
        for ((module, value), execution) in modules
            .into_iter()
            .zip(values.iter())
            .zip(executions.into_iter())
        {
            let current_file_id = snapshot
                .file_id_for_module(&module, &self.identity_resolution)
                .expect("parse demand belongs to the published source revision");
            let computed = matches!(execution, RequestExecution::Computed);
            if computed {
                work.modules_reparsed += 1;
                work.syntax.lexer_invocations += value.work.lexer_invocations;
                work.syntax.parser_invocations += value.work.parser_invocations;
                work.syntax.lexed_bytes += value.work.lexed_bytes;
                work.syntax.tokens += value.work.tokens;
            }
            match &value.result {
                Ok(module) => {
                    let projected = crate::parsed_modules::rebind_parsed_module(
                        &snapshot,
                        module,
                        &self.identity_resolution,
                    );
                    if !computed {
                        if Arc::ptr_eq(&projected, module) {
                            work.modules_reused += 1;
                        } else {
                            work.modules_rebound += 1;
                        }
                    }
                    parsed.push(projected);
                }
                Err(module_errors) => {
                    if !computed {
                        work.modules_reused += 1;
                    }
                    errors.extend(
                        module_errors.clone().map_spans(|span| {
                            Span::with_file(current_file_id, span.start, span.end)
                        }),
                    )
                }
            }
        }
        let result = if errors.is_empty() {
            ParsedProgram::new(root.clone(), parsed)
                .map(Arc::new)
                .map_err(crate::CompileErrors::from)
        } else {
            Err(errors)
        };
        (result, work)
    }

    pub(crate) fn compose_candidate_module_rirs(
        &self,
        revision: Revision,
        modules: impl IntoIterator<Item = ModuleId>,
    ) -> (
        Result<Vec<Arc<CandidateModuleRirOutput>>, crate::CompileErrors>,
        crate::CanonicalRirWork,
    ) {
        let snapshot = self
            .module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revisions
            .iter()
            .find(|view| view.revision == revision)
            .expect("candidate RIR composition retains its module input revision")
            .snapshot
            .clone();
        let mut outputs = Vec::new();
        let mut errors = crate::CompileErrors::new();
        let mut work = crate::CanonicalRirWork::default();
        for module in modules {
            let parsed_attempt = self.runtime.request_registered(
                &self.parse_modules,
                revision,
                ModuleQueryKey(module.clone()),
                CancellationToken::new(),
            );
            let Some(parsed_terminal) = parsed_attempt.terminal() else {
                errors.push(import_input_error(format!(
                    "candidate module composition parse({module}) aborted: {:?}",
                    parsed_attempt.abort()
                )));
                continue;
            };
            let rue_query::QueryOutcome::Success(parsed_value) = parsed_terminal.outcome() else {
                unreachable!("ParseModule publishes typed values")
            };
            let parsed = match &parsed_value.result {
                Ok(parsed) => crate::parsed_modules::rebind_parsed_module(
                    &snapshot,
                    parsed,
                    &self.identity_resolution,
                ),
                Err(module_errors) => {
                    errors.extend(module_errors.clone());
                    continue;
                }
            };
            let mut artifacts = AHashMap::new();
            let mut failed = false;
            for candidate in parsed.definitions().declaration_keys_in_source_order() {
                let attempt = self.runtime.request_registered(
                    &self.declaration_body_plan_artifacts,
                    revision,
                    DeclarationBodyPlanQueryKey(candidate.clone()),
                    CancellationToken::new(),
                );
                let Some(terminal) = attempt.terminal() else {
                    errors.push(import_input_error(format!(
                        "candidate RIR artifact {} aborted: {:?}",
                        candidate.stable_identity(),
                        attempt.abort()
                    )));
                    failed = true;
                    break;
                };
                let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                    unreachable!("DeclarationBodyPlanArtifacts publishes typed values")
                };
                match value {
                    DeclarationBodyPlanArtifactsValue::Available(artifact) => {
                        artifacts.insert(candidate.clone(), artifact.clone());
                    }
                    DeclarationBodyPlanArtifactsValue::Failure(failure) => {
                        errors.extend(candidate_rir_artifact_failure_errors(failure));
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                continue;
            }
            match crate::canonical_lower::compose_module_rir_from_candidate_artifacts(
                parsed,
                &artifacts,
                || Ok(()),
            ) {
                Ok(output) => {
                    work.accumulate(output.work());
                    outputs.push(Arc::new(output));
                }
                Err(failure) => errors.push(candidate_rir_composition_failure_error(&failure)),
            }
        }
        if errors.is_empty() {
            (Ok(outputs), work)
        } else {
            (Err(errors), work)
        }
    }

    pub(crate) fn projected_module_indexes(
        &self,
        revision: Revision,
        program: &ParsedProgram,
    ) -> Result<Vec<ProjectedModuleIndex>, crate::CompileErrors> {
        let mut projections = Vec::with_capacity(program.modules().len());
        let mut errors = crate::CompileErrors::new();
        for module in program.modules() {
            let index_attempt = self.runtime.request_registered(
                &self.module_indexes,
                revision,
                ModuleQueryKey(module.module_id().clone()),
                CancellationToken::new(),
            );
            let Some(index_terminal) = index_attempt.terminal() else {
                errors.push(import_input_error(format!(
                    "ModuleIndex({}) aborted: {:?}",
                    module.module_id(),
                    index_attempt.abort()
                )));
                continue;
            };
            let rue_query::QueryOutcome::Success(indexed) = index_terminal.outcome() else {
                unreachable!("ModuleIndex publishes typed values")
            };
            let index = match &indexed.0 {
                Ok(index) => index,
                Err(module_errors) => {
                    errors.extend(module_errors.clone());
                    continue;
                }
            };
            if index.revision != *module.revision() {
                errors.push(import_input_error(format!(
                    "ModuleIndex({}) belongs to a foreign source revision",
                    module.module_id()
                )));
                continue;
            }
            let mut definitions = Vec::with_capacity(index.definitions.len());
            for (namespace, name) in index.definition_keys() {
                let lookup_attempt = self.runtime.request_registered(
                    &self.lookup_names,
                    revision,
                    LookupNameKey {
                        module: module.module_id().clone(),
                        namespace,
                        name: name.clone(),
                    },
                    CancellationToken::new(),
                );
                let Some(lookup_terminal) = lookup_attempt.terminal() else {
                    errors.push(import_input_error(format!(
                        "LookupName({}) aborted: {:?}",
                        module.module_id(),
                        lookup_attempt.abort()
                    )));
                    continue;
                };
                let rue_query::QueryOutcome::Success(found) = lookup_terminal.outcome() else {
                    unreachable!("LookupName publishes typed values")
                };
                match &found.0 {
                    Ok(found) => {
                        let current = index
                            .definitions_for(namespace, name.as_ref())
                            .cloned()
                            .collect::<Vec<_>>();
                        let current_facts = current
                            .iter()
                            .map(ModuleIndexEntry::lookup_fact)
                            .collect::<Vec<_>>();
                        if current_facts.as_slice() == found.as_ref() {
                            definitions.extend(current);
                        } else {
                            errors.push(import_input_error(format!(
                                "LookupName({}::{name}) disagrees with current locators",
                                module.module_id()
                            )));
                        }
                    }
                    Err(failure) => errors.push(import_input_error(format!(
                        "LookupName({}::{name}) failed: {failure:?}",
                        module.module_id()
                    ))),
                }
            }
            definitions.sort_by(|left, right| {
                left.declaration_span
                    .start
                    .cmp(&right.declaration_span.start)
                    .then(left.declaration_span.end.cmp(&right.declaration_span.end))
                    .then(left.namespace.cmp(&right.namespace))
                    .then(left.name.cmp(&right.name))
            });
            if definitions.len() != index.definitions.len() {
                errors.push(import_input_error(format!(
                    "LookupName projection for {} is incomplete",
                    module.module_id()
                )));
                continue;
            }
            let file_id = module.file_id();
            for entry in &mut definitions {
                entry.name_span =
                    rue_span::Span::with_file(file_id, entry.name_span.start, entry.name_span.end);
                entry.declaration_span = rue_span::Span::with_file(
                    file_id,
                    entry.declaration_span.start,
                    entry.declaration_span.end,
                );
            }
            projections.push(ProjectedModuleIndex {
                revision: index.revision.clone(),
                definitions: definitions.into(),
            });
        }
        if errors.is_empty() {
            Ok(projections)
        } else {
            Err(errors)
        }
    }

    #[cfg(test)]
    pub(super) fn module_terminals(
        &self,
        revision: Revision,
        module: ModuleId,
    ) -> (Arc<ParsedModule>, Arc<ModuleIndex>) {
        let parse = self.runtime.request_registered(
            &self.parse_modules,
            revision,
            ModuleQueryKey(module.clone()),
            CancellationToken::new(),
        );
        let index = self.runtime.request_registered(
            &self.module_indexes,
            revision,
            ModuleQueryKey(module.clone()),
            CancellationToken::new(),
        );
        let parse = match parse.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(value) => value.result.clone().unwrap(),
            _ => unreachable!(),
        };
        let index = match index.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(value) => value.0.clone().unwrap(),
            _ => unreachable!(),
        };
        (parse, index)
    }

    pub(crate) fn select_parse(
        &mut self,
        attempt: &QueryRequestAttempt<crate::session::ParseQueryRecord>,
    ) {
        if attempt.execution() == RequestExecution::Aborted {
            self.parse_selection.clear_current();
        }
        if let Some(terminal) = attempt.terminal() {
            self.parse_selection
                .publish(terminal)
                .expect("selected terminal belongs to the Parse family");
            // Publication establishes the runtime selection root before the
            // request bridge lease ends, so the terminal stays protected while
            // the diagnostic attempt index retains this request.
            attempt.release_result_lease();
        }
        let protected_revisions = [
            self.parse_selection.current(),
            self.parse_selection.last_good(),
        ]
        .into_iter()
        .flatten()
        .map(|terminal| terminal.revision())
        .collect::<BTreeSet<_>>();
        {
            let mut store = self
                .module_store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store.protected_revisions = protected_revisions.clone();
            trim_module_input_views(&mut store);
        }
        {
            let mut store = lock_import_store(&self.import_store);
            store.protected_revisions = protected_revisions;
            trim_import_input_views(&mut store);
        }
        // Exact source stamps live exactly as long as a parse memo key (or the
        // current request before selection). They are never independently FIFO
        // evicted while a terminal can still observe the stamp.
        self.source_stamps.retain(|(source, _)| {
            self.parse
                .any_retained_key(|key| key.key.pinned_source() == Some(source))
        });
        debug_assert!(self.source_stamps.len() <= self.parse.retention().memo_nodes);
    }

    pub(crate) fn parse_attempt_view(
        &self,
        id: AttemptId,
        attempt: Arc<QueryRequestAttempt<crate::session::ParseQueryRecord>>,
        work: QueryStructuralWork,
    ) -> Arc<dyn AttemptView> {
        let origin = AttemptId(attempt.origin_request_id());
        let runtime_observations = attempt
            .dependencies()
            .iter()
            .cloned()
            .map(RuntimeObservation::Dependency)
            .chain(
                attempt
                    .inputs()
                    .iter()
                    .cloned()
                    .map(RuntimeObservation::Input),
            )
            .collect::<Vec<_>>()
            .into();
        let runtime_work = attempt.work().to_vec().into();
        Arc::new(RuntimeAttemptView::<crate::session::ParseQuery> {
            id,
            origin,
            attempt,
            work,
            runtime_observations,
            runtime_work,
        })
    }

    pub(crate) fn parse_origin_attempt_ids(&self) -> impl Iterator<Item = AttemptId> + '_ {
        let mut origins = self
            .parse
            .retained_origin_request_ids()
            .into_iter()
            .map(AttemptId)
            .collect::<BTreeSet<_>>();
        origins.extend(
            [
                self.parse_selection.current(),
                self.parse_selection.last_good(),
            ]
            .into_iter()
            .flatten()
            .map(|terminal| AttemptId(terminal.origin_request_id())),
        );
        origins.into_iter()
    }

    pub(crate) fn runtime_retention_metrics(&self) -> rue_query::RuntimeMetrics {
        self.runtime.metrics()
    }

    pub(crate) fn body_reachability_metrics(&self) -> crate::unstable::SemanticReachabilityMetrics {
        self.body_reachability_meter.snapshot()
    }

    pub(crate) fn input_stamp_retention_metrics(&self) -> InputStampRetentionMetrics {
        let module_store = self
            .module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let import_store = lock_import_store(&self.import_store);
        InputStampRetentionMetrics {
            module_views: module_store.revisions.len(),
            module_source_stamps: module_store.stamps.len(),
            import_views: import_store.revisions.len(),
            import_context_stamps: import_store.context_stamps.len(),
            accepted_topology_stamps: import_store.topology_stamps.len(),
            accepted_read_provenance_stamps: import_store.provenance_stamps.len(),
            import_observation_stamps: import_store.observation_stamps.len(),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_module_input_retention_for_test(&self, retention_limit: usize) {
        assert!(retention_limit > 0);
        let mut store = self
            .module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store.retention_limit = retention_limit;
        trim_module_input_views(&mut store);
    }

    #[cfg(test)]
    pub(crate) fn module_source_stamp_for_test(&self, source: &ModuleRevision) -> Option<u64> {
        self.module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stamps
            .get(&ModuleInputLeaf {
                revision: source.clone(),
            })
            .map(|retained| retained.stamp)
    }
}
