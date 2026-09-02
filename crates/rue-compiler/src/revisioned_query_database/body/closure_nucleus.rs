//! Body-closure reachability and declaration semantic-nucleus projection.
//!
//! This module owns closure scheduling, retained-work accounting, and the
//! stable declaration projection. It consumes registered query families; it
//! does not create a peer runtime or an alternate semantic evaluation path.

use super::super::*;

pub(crate) struct BodyClosureRequest {
    pub(crate) terminal: Arc<rue_query::QueryTerminal<crate::body_query::BodyClosureOutput>>,
    /// Keyed by the ADR-0074 structural key digest rather than the rendered
    /// body identity, so building and reading this projection never formats a
    /// body transaction's name.
    pub(in crate::revisioned_query_database) body_executions:
        BTreeMap<rue_query::NodeIdentity, rue_query::RequestExecution>,
    retained_before: BTreeSet<rue_query::NodeIdentity>,
    pub(in crate::revisioned_query_database) work: Vec<(Arc<str>, u64)>,
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
fn candidate_body_plan_work_from_nested(
    attempts: &[rue_query::NestedQueryAttempt],
) -> (crate::CandidateBodyPlanWork, crate::CandidateBodyPlanWork) {
    fn priority(execution: RequestExecution) -> u8 {
        match execution {
            RequestExecution::Computed => 4,
            RequestExecution::Joined => 3,
            RequestExecution::Reused => 2,
            RequestExecution::Aborted => 1,
        }
    }
    fn reduce_family<'a>(
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
    fn count(
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
fn successful_nested_nodes(
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
fn candidate_body_plan_work_from_retained_closure(
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
        | F::DiagnosticAtModuleRange { kind, .. }
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
pub(in crate::revisioned_query_database) fn instance_producer_closure(
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
pub(in crate::revisioned_query_database) fn observe_body_toolchain_demand(
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

pub(in crate::revisioned_query_database) fn visit_instance_anonymous_nominals<'a>(
    function: &'a crate::FunctionInstanceKey,
    seen: &mut AHashSet<*const crate::AnonymousNominalKey>,
    mut visit: impl FnMut(&'a crate::AnonymousNominalKey),
) {
    fn arguments<'a, F: FnMut(&'a crate::AnonymousNominalKey)>(
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

    fn anonymous<'a, F: FnMut(&'a crate::AnonymousNominalKey)>(
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

    fn instance_type<'a, F: FnMut(&'a crate::AnonymousNominalKey)>(
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

    fn instance_function<'a, F: FnMut(&'a crate::AnonymousNominalKey)>(
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

pub(in crate::revisioned_query_database) fn schedule_body_instance<V>(
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
pub(in crate::revisioned_query_database) fn comptime_specialization_depth(
    scheduler_depth: usize,
) -> usize {
    scheduler_depth.saturating_sub(1)
}

impl RevisionedQueryDatabase {
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
    pub(in crate::revisioned_query_database) fn body_closure_root_metrics(
        &self,
    ) -> (usize, u64, u64) {
        let root = self
            .body_closure_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (root.lease.len(), root.additions, root.deletions)
    }

    #[cfg(test)]
    pub(in crate::revisioned_query_database) fn body_reachability_root_len(&self) -> usize {
        self.body_reachability_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lease
            .len()
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

    pub(in crate::revisioned_query_database) fn evaluate_declaration_semantics_projection(
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
            // Freestanding conformance assertions (spec 6.7:9) belong to no
            // declaration; resolving them here is what makes their preview
            // gate and name errors surface for a program that never relies
            // on them.
            let terminal = context
                .query_registered(
                    semantic_nucleus,
                    Key::ModuleConformances(
                        crate::semantic_query_nucleus::ModuleSemanticQueryKey {
                            module: module.clone(),
                            configuration: configuration.clone(),
                        },
                    ),
                )
                .map_err(SemanticNucleusBatchFailure::Query)?;
            let rue_query::QueryOutcome::Success(conformances) = terminal.outcome() else {
                unreachable!("SemanticNucleus publishes typed values")
            };
            if let Value::Failure(failure) = conformances {
                return Err(SemanticNucleusBatchFailure::Stable {
                    declaration: None,
                    failure: Box::new(failure.clone()),
                });
            }
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
                        for value in projected.iter() {
                            if let Err(identity) = crate::durable_semantics::merge_anonymous_nominal(
                                &mut anonymous_nominals,
                                value,
                            ) {
                                return Err(SemanticNucleusBatchFailure::Stable {
                                    declaration: Some(declaration.clone()),
                                    failure: Box::new(
                                        crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                                            Arc::from(format!(
                                                "conflicting durable anonymous facts for {identity:?}"
                                            )),
                                        ),
                                    ),
                                });
                            }
                        }
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
                    for value in signature.anonymous_nominals.iter() {
                        if let Err(identity) = crate::durable_semantics::merge_anonymous_nominal(
                            &mut anonymous_nominals,
                            value,
                        ) {
                            return Err(SemanticNucleusBatchFailure::Stable {
                                declaration: Some(declaration.clone()),
                                failure: Box::new(
                                    crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                                        Arc::from(format!(
                                            "conflicting durable anonymous facts for {identity:?}"
                                        )),
                                    ),
                                ),
                            });
                        }
                    }
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

    pub(crate) fn body_reachability_metrics(&self) -> crate::unstable::SemanticReachabilityMetrics {
        self.body_reachability_meter.snapshot()
    }
}
