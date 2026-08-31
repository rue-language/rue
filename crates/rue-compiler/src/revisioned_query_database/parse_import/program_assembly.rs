//! Parse/import-owned input publication and program assembly.
//!
//! This module owns import-view revisions, parse frontiers, successor/full
//! program assembly, parse selection, and their exact retention projections.
//! It adds no query family and uses the database runtime registered by the hub.

use super::super::*;

impl RevisionedQueryDatabase {
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

    /// Reselect the exact closed-valid import view after a filesystem host
    /// abandons a partially published request. Immutable candidate revisions
    /// remain retained normally, but none may stay selected after cancellation.
    pub(crate) fn restore_import_revision_after_abort(
        &mut self,
        revision: Option<ImportInputRevision>,
    ) -> CompileResult<()> {
        if self.committed_import_revision != revision {
            return Err(import_input_error(
                "cannot restore an aborted import request: committed revision selection disagrees with the closed artifact",
            ));
        }
        let reselected = self
            .parse_selection
            .reselect_last_good()
            .map_err(|error| {
                import_input_error(format!(
                    "cannot restore an aborted import request: the committed parse terminal is unavailable: {error:?}"
                ))
            })?;
        if revision.is_some() && !reselected {
            return Err(import_input_error(
                "cannot restore an aborted import request: the committed import view has no last-good parse terminal",
            ));
        }
        let Some(revision) = revision else {
            self.current_import_revision = None;
            self.active_import_context = None;
            self.lineage_additions.clear();
            self.refresh_import_input_protected_revisions();
            return Ok(());
        };
        let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
        let context = {
            let store = lock_import_store(&self.import_store);
            let view = store
                .revisions
                .iter()
                .find(|view| view.revision == runtime_revision)
                .ok_or_else(|| {
                    import_input_error(
                        "cannot restore an aborted import request: the committed input view is no longer retained",
                    )
                })?;
            if view.generation != revision.request_generation {
                return Err(import_input_error(
                    "cannot restore an aborted import request: the committed input generation does not match",
                ));
            }
            view.context.clone()
        };
        let module_view_retained = {
            let store = self
                .module_store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store.by_revision.contains_key(&runtime_revision)
        };
        if !module_view_retained {
            return Err(import_input_error(
                "cannot restore an aborted import request: the committed module input view is no longer retained",
            ));
        }
        self.current_import_revision = Some(revision);
        self.active_compatibility_token = revision.compatibility_token;
        self.active_import_context = Some(context);
        self.lineage_additions.clear();
        self.refresh_import_input_protected_revisions();
        Ok(())
    }

    /// Promote the request's selected input revision only after import close
    /// and public parse adoption have both succeeded.
    pub(crate) fn commit_import_request(&mut self) {
        let committed = self.current_import_revision;
        let pin = committed.map(|revision| {
            self.parse.retain_revision(Revision::new(
                revision.revision_id,
                revision.compatibility_token,
            ))
        });
        self.committed_import_revision = committed;
        // Install the new runtime root before the old pin drops so a successful
        // close never creates an eviction gap between committed revisions.
        self.committed_import_revision_pin = pin;
        self.refresh_import_input_protected_revisions();
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

    fn publish_import_view(
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
    fn publish_import_view_overlay(
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
            parent_runtime,
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

    fn parse_module_frontier(
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
    pub(in crate::revisioned_query_database) fn module_terminals(
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
        self.select_parse_inner(attempt, true);
    }

    /// Select a staging parse as request-current without promoting it across
    /// the import-discovery commit boundary.
    pub(crate) fn select_parse_candidate(
        &mut self,
        attempt: &QueryRequestAttempt<crate::session::ParseQueryRecord>,
    ) {
        self.select_parse_inner(attempt, false);
    }

    fn select_parse_inner(
        &mut self,
        attempt: &QueryRequestAttempt<crate::session::ParseQueryRecord>,
        promote: bool,
    ) {
        if attempt.execution() == RequestExecution::Aborted {
            self.parse_selection.clear_current();
        }
        if let Some(terminal) = attempt.terminal() {
            if promote {
                self.parse_selection
                    .publish(terminal)
                    .expect("selected terminal belongs to the Parse family");
            } else {
                self.parse_selection
                    .publish_candidate(terminal)
                    .expect("candidate terminal belongs to the Parse family");
            }
            // Publication establishes the runtime selection root before the
            // request bridge lease ends, so the terminal stays protected while
            // the diagnostic attempt index retains this request.
            attempt.release_result_lease();
        }
        self.refresh_import_input_protected_revisions();
        // Exact source stamps live exactly as long as a parse memo key (or the
        // current request before selection). They are never independently FIFO
        // evicted while a terminal can still observe the stamp.
        self.source_stamps.retain(|(source, _)| {
            self.parse
                .any_retained_key(|key| key.key.pinned_source() == Some(source))
        });
        debug_assert!(self.source_stamps.len() <= self.parse.retention().memo_nodes);
    }

    fn refresh_import_input_protected_revisions(&mut self) {
        let mut protected_revisions = [
            self.parse_selection.current(),
            self.parse_selection.last_good(),
        ]
        .into_iter()
        .flatten()
        .map(|terminal| terminal.revision())
        .collect::<BTreeSet<_>>();
        for revision in [self.current_import_revision, self.committed_import_revision]
            .into_iter()
            .flatten()
        {
            protected_revisions.insert(Revision::new(
                revision.revision_id,
                revision.compatibility_token,
            ));
        }
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
