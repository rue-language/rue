//! Canonical parse, merge, and RIR artifact projections.

use super::{
    AttemptId, CanonicalMergeWork, CanonicalMergedProgram, CanonicalRirOutput, CanonicalRirWork,
    CompileError, CompileErrors, CompilerSession, CompilerSessionUpdate,
    DiagnosticAttemptProvenance, ErrorKind, ExactSourceInput, FrontendDiagnosticIdentity,
    FrontendDiagnosticSnapshot, MergeQuery, OrdinaryParseKey, ParseInvalidationSummary,
    ParseQueryKey, ParseQueryRecord, ParsedModulesWork, ParsedProgram, PreparedSuccessorParse,
    QueryAttemptExecution, QueryComputationGuard, QueryStructuralWork, RirQuery, SourceSnapshot,
};
use crate::canonical_lower::project_candidate_module_rirs_with_work;
use crate::canonical_merge::merge_parsed_modules_reusing_indexes;
use crate::parsed_modules::classify_invalidation;
use crate::typed_query_store::AttemptView;
use std::sync::Arc;

impl CompilerSession {
    pub fn update(&mut self, snapshot: &SourceSnapshot) -> CompilerSessionUpdate {
        // A source update supersedes the predecessor any outstanding
        // trusted-toolchain continuation or successor-delta authority was
        // issued against (RUE-1112): a stale capability can neither stage nor
        // close over an artifact the update replaced.
        self.invalidate_import_successor_authority();
        self.select_diagnostic_presentation(None);
        let provenance = self.syntax_diagnostic_provenance();
        self.run_parse_update(snapshot, provenance)
    }

    /// Publish a snapshot while retaining its caller-selected presentation order.
    ///
    /// Query artifacts still use stable module identity. Only syntax and merge
    /// diagnostic ordering follows [`SourceSnapshot::files`], which is useful
    /// for command-line and other presentation-oriented consumers.
    pub(crate) fn update_for_presentation(
        &mut self,
        snapshot: &SourceSnapshot,
    ) -> CompilerSessionUpdate {
        // A presentation update replaces the retained parse artifact exactly
        // like a source update, so it likewise supersedes any outstanding
        // trusted-toolchain continuation or successor-delta authority
        // (RUE-1112).
        self.invalidate_import_successor_authority();
        self.select_diagnostic_presentation(Some(crate::shared_segments::SharedList::flat(
            snapshot
                .files()
                .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
                .collect(),
        )));
        let provenance = self.syntax_diagnostic_provenance();
        self.run_parse_update(snapshot, provenance)
    }

    /// Select the parse this presentation shows after an import-discovery
    /// close. A trusted successor re-selects its retained successor parse
    /// terminal; an ordinary close selects the presentation order and runs the
    /// parse update, whose per-module requests reuse the retained module
    /// terminals. The discovery parse itself is deliberately NOT handed over:
    /// adopting it wholesale would bypass parse-terminal publication
    /// (RUE-1144).
    pub(super) fn select_parse_for_presentation(
        &mut self,
        snapshot: &SourceSnapshot,
        successor: bool,
        retained_successor_parse: Option<ParseQueryRecord>,
    ) -> CompilerSessionUpdate {
        // A trusted-successor close adopts by RE-SELECTING the exact successor
        // parse terminal its stage computed and retained on the open artifact
        // — same key, same revision — never by re-deriving an extension
        // against the now-selected successor state (which would mint a second
        // empty-extension terminal). A missing retained terminal rejects the
        // close.
        if successor {
            return match retained_successor_parse {
                Some(record) => self.run_parse_update_successor(snapshot, record),
                None => {
                    let errors = CompileErrors::from(CompileError::without_span(
                        ErrorKind::InvalidCompilerInput(
                            "trusted-toolchain successor close rejected: the staged successor parse terminal is not retained".into(),
                        ),
                    ));
                    let diagnostics = Arc::new(FrontendDiagnosticSnapshot {
                        source: snapshot.clone(),
                        stage: FrontendDiagnosticIdentity::Syntax,
                        provenance: DiagnosticAttemptProvenance::Canonical,
                        errors: errors.as_slice().to_vec().into(),
                        warnings: Arc::from([]),
                    });
                    CompilerSessionUpdate {
                        result: Err(errors),
                        work: ParsedModulesWork::default(),
                        #[cfg(test)]
                        invalidation: ParseInvalidationSummary::default(),
                        downstream_invalidated: false,
                        diagnostics,
                    }
                }
            };
        }
        self.select_diagnostic_presentation(Some(crate::shared_segments::SharedList::flat(
            snapshot
                .files()
                .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
                .collect(),
        )));
        let provenance = self.syntax_diagnostic_provenance();
        self.run_parse_update(snapshot, provenance)
    }

    fn parse_baseline(&self) -> Option<Arc<ParsedProgram>> {
        self.queries
            .revisioned
            .last_good_parse_record()
            .and_then(|record| record.result.as_ref().ok())
            .cloned()
    }

    pub(super) fn parse_invalidation(&self, snapshot: &SourceSnapshot) -> ParseInvalidationSummary {
        let baseline = self.parse_baseline();
        classify_invalidation(snapshot, baseline.as_deref())
    }

    fn syntax_diagnostic_provenance(&self) -> DiagnosticAttemptProvenance {
        self.batch_diagnostic_order
            .as_ref()
            .map_or(DiagnosticAttemptProvenance::Canonical, |order| {
                DiagnosticAttemptProvenance::Presentation(order.clone())
            })
    }

    fn select_diagnostic_presentation(
        &mut self,
        order: Option<crate::shared_segments::SharedList<crate::ModuleId>>,
    ) {
        self.batch_diagnostic_order = order;
    }

    fn execute_parse_query(
        &mut self,
        snapshot: &SourceSnapshot,
        presentation: DiagnosticAttemptProvenance,
        attempt_id: AttemptId,
        promote_selection: bool,
    ) -> (
        ParseQueryRecord,
        Arc<dyn AttemptView>,
        QueryAttemptExecution,
        ParsedModulesWork,
        ParseInvalidationSummary,
        Arc<rue_query::QueryTerminal<ParseQueryRecord>>,
    ) {
        // Keying, module parsing, and terminal publication are separate costs
        // inside the parse query. Timing them apart keeps the staging residual
        // from hiding whole-snapshot content hashing behind `parse_file`
        // (RUE-786).
        let key_span = tracing::info_span!("parse_query_key").entered();
        let source = ExactSourceInput::new(snapshot);
        // An ordinary key carries every file's exact content identity, so the
        // typed store hashes and compares each of them.
        self.parse_key_entries_compared = self
            .parse_key_entries_compared
            .saturating_add(snapshot.len() as u64);
        let key = ParseQueryKey::Ordinary(Box::new(OrdinaryParseKey {
            source: source.clone(),
            file_order: snapshot
                .files()
                .map(|source| source.file_id)
                .collect::<Vec<_>>()
                .into(),
            presentation: presentation.clone(),
        }));
        let revision = self.queries.revisioned.source_revision(&source, snapshot);
        let demanded_modules = match &presentation {
            DiagnosticAttemptProvenance::Canonical => snapshot
                .source_revision()
                .modules()
                .iter()
                .map(|source| source.module.clone())
                .collect::<Vec<_>>(),
            DiagnosticAttemptProvenance::Presentation(order) => order.iter().cloned().collect(),
        };
        self.parse_sources_materialized = self
            .parse_sources_materialized
            .saturating_add(demanded_modules.len() as u64);
        self.parse_modules_dispatched = self
            .parse_modules_dispatched
            .saturating_add(demanded_modules.len() as u64);
        drop(key_span);
        // The per-module parses run OUTSIDE the outer query body below, as
        // top-level requests, so the outer `compiler.parse` node never records
        // them as dependencies (RUE-1145). This is deliberate, and safe only
        // because the node is key-identified: `ExactSourceInput` embeds every
        // file's exact content identity, so any source change selects a
        // different node and its recorded edge set is never what invalidates
        // it. The single synthetic whole-source leaf recorded in the closure is
        // NOT a real dependency graph — no consumer may read this node's edges
        // to learn which modules it consumed. (The successor path is different:
        // it adopts the predecessor terminal and records the appended modules'
        // input leaves, so its edges are real.) This shim node is deleted by
        // ADR-0063 Phase 12 (RUE-1033); recording real module edges here would
        // mean growing an out-of-band observation API in rue-query for a node
        // scheduled for removal.
        let (modular_result, modular_work) = {
            let _span = tracing::info_span!("parse_program").entered();
            self.queries.revisioned.parse_program(
                revision,
                snapshot.source_revision().root(),
                demanded_modules,
            )
        };
        let _commit_span = tracing::info_span!("parse_query_commit").entered();
        self.parse_invalidation_entries_compared = self
            .parse_invalidation_entries_compared
            .saturating_add(snapshot.len() as u64);
        let baseline = self.parse_baseline();
        let attempt =
            self.queries
                .revisioned
                .request_parse(revision, attempt_id, key.clone(), |context| {
                    // Key-identified-only node: this synthetic leaf exists so
                    // the terminal has a non-empty input set, not to model the
                    // per-module parses consumed above (RUE-1145; see the
                    // deletion-gate comment at `parse_program`).
                    context.input(rue_query::InputIdentity::new(
                        crate::revisioned_query_database::RevisionedQueryDatabase::SOURCE_INPUT,
                        "current",
                    ))?;
                    let work = modular_work;
                    let invalidation = classify_invalidation(snapshot, baseline.as_deref());
                    let result = modular_result;
                    // Freeze diagnostics privately with the query output. Session
                    // selection and metrics happen only after atomic publication.
                    let diagnostics = Arc::new(FrontendDiagnosticSnapshot {
                        source: snapshot.clone(),
                        stage: FrontendDiagnosticIdentity::Syntax,
                        provenance: presentation.clone(),
                        errors: result.as_ref().err().map_or_else(
                            || Arc::from([]),
                            |errors| errors.as_slice().to_vec().into(),
                        ),
                        warnings: Arc::from([]),
                    });
                    Ok(ParseQueryRecord {
                        key,
                        runtime_revision: revision,
                        snapshot: snapshot.clone(),
                        result,
                        diagnostics,
                        work,
                        invalidation,
                    })
                });
        if promote_selection {
            self.queries.revisioned.select_parse(&attempt);
        } else {
            self.queries.revisioned.select_parse_candidate(&attempt);
        }
        let terminal = attempt
            .terminal()
            .unwrap_or_else(|| panic!("parse query aborted: {:?}", attempt.abort()))
            .clone();
        let record = match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => record.clone(),
            rue_query::QueryOutcome::Failure(_) => unreachable!("parse retains typed records"),
        };
        let execution = match attempt.execution() {
            rue_query::RequestExecution::Computed => {
                self.metrics
                    .diagnostic_publication(self.diagnostics.latest().is_some());
                QueryAttemptExecution::Computed
            }
            rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined => {
                self.reuse_diagnostics(record.diagnostics.clone());
                QueryAttemptExecution::Reused
            }
            rue_query::RequestExecution::Aborted => unreachable!(),
        };
        let work = if execution == QueryAttemptExecution::Computed {
            record.work
        } else {
            ParsedModulesWork::default()
        };
        let invalidation = if execution == QueryAttemptExecution::Computed {
            record.invalidation.clone()
        } else {
            self.parse_invalidation(snapshot)
        };
        let view = self.queries.revisioned.parse_attempt_view(
            attempt_id,
            attempt,
            QueryStructuralWork::Parse(work),
        );
        self.diagnostics.select(view.clone());
        (record, view, execution, work, invalidation, terminal)
    }

    /// Reconcile one successor parse extension without side effects: the
    /// retained parse artifact this stage extends, its presentation order, and
    /// the appended (module, file) pairs. The compiler-owned input transition
    /// proves the exact parent revision and appended revisions; the retained
    /// terminal must still be adoptable, rooted identically, and have the exact
    /// predecessor presentation order. A record from an intervening update is
    /// rejected. Everything here is O(appended); predecessor contents are
    /// carried by the immutable revision rather than rescanned or re-hashed.
    fn prepare_successor_parse(
        &self,
        snapshot: &SourceSnapshot,
        delta: &Arc<[crate::ModuleRevision]>,
        terminal: Arc<rue_query::QueryTerminal<ParseQueryRecord>>,
    ) -> Result<PreparedSuccessorParse, CompileErrors> {
        let reject = |message: &str| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("incremental import parse rejected: {message}"),
            )))
        };
        let Ok(predecessor_terminal) = self
            .queries
            .revisioned
            .parse_family()
            .adoptable_terminal(&terminal)
        else {
            return Err(reject(
                "the retained predecessor parse terminal is not adoptable",
            ));
        };
        let rue_query::QueryOutcome::Success(record) = predecessor_terminal.terminal().outcome()
        else {
            return Err(reject("the retained predecessor parse artifact failed"));
        };
        let Ok(predecessor_program) = record.result.as_ref().cloned() else {
            return Err(reject("the retained predecessor parse artifact failed"));
        };
        let DiagnosticAttemptProvenance::Presentation(predecessor_order) =
            &record.diagnostics.provenance
        else {
            return Err(reject(
                "the retained parse artifact carries no staging presentation order",
            ));
        };
        let predecessor_order = predecessor_order.clone();
        let predecessor_revision = record.runtime_revision;
        // The private input transition already proves predecessor identity and
        // unchanged carried sources. Snapshot assemblers may compact their
        // persistent segments, so pointer ancestry is not a semantic
        // requirement here; requiring it would reject an otherwise exact host
        // batch after compaction. Root identity plus the exact appended segment
        // is rechecked below without walking the predecessor prefix.
        let predecessor_snapshot = record.snapshot.clone();
        if snapshot.source_revision().root() != predecessor_snapshot.source_revision().root() {
            return Err(reject(
                "the retained parse artifact belongs to a different root module",
            ));
        }
        let predecessor_len = predecessor_program.modules_len();
        if predecessor_len != predecessor_snapshot.len()
            || predecessor_order.len() != predecessor_len
        {
            return Err(reject(
                "the retained parse artifact does not cover its own snapshot",
            ));
        }
        // A re-stage whose snapshot appended nothing since the retained parse
        // (a frontier round that only grew observations) extends with an empty
        // delta and reuses every retained module.
        if predecessor_len > snapshot.len() || snapshot.len() - predecessor_len > delta.len() {
            return Err(reject(
                "the successor snapshot does not extend the retained parse artifact by the authorized delta",
            ));
        }
        // The appended sources extend the predecessor's dense file table, so
        // the appended (module, file) pairs are exactly the tail file IDs.
        let mut appended = Vec::with_capacity(snapshot.len() - predecessor_len);
        for index in predecessor_len as u32 + 1..=snapshot.len() as u32 {
            let file_id = crate::FileId::new(index);
            let Some(module) = snapshot.module_id(file_id) else {
                return Err(reject("an appended source has no logical module"));
            };
            appended.push((module.clone(), file_id));
        }
        // Every appended module revision must be one of the
        // capability-verified additions. The capability delta is cumulative
        // since the committed close; the parse key below keeps only this
        // stage's exact suffix.
        let mut segment = Vec::with_capacity(appended.len());
        for (module, file_id) in &appended {
            let source = snapshot
                .source_id(*file_id)
                .expect("the appended source has a stable content identity")
                .clone();
            let Ok(index) = delta.binary_search_by(|revision| revision.module.cmp(module)) else {
                return Err(reject(
                    "an appended module is outside the capability-verified delta",
                ));
            };
            if delta[index].source != source {
                return Err(reject(
                    "an appended module's source differs from the capability-verified delta",
                ));
            }
            segment.push(crate::ModuleRevision {
                module: module.clone(),
                source,
            });
        }
        segment.sort_by(|left, right| left.module.cmp(&right.module));
        Ok(PreparedSuccessorParse {
            predecessor_program,
            predecessor_order,
            predecessor_revision,
            predecessor_terminal,
            appended,
            segment: segment.into(),
        })
    }

    /// The successor parse projection (RUE-1112): keyed on the published
    /// lineage identity plus the exact appended segment, parsing ONLY the
    /// appended modules and structurally extending the retained predecessor
    /// parsed program and presentation order.
    #[allow(clippy::type_complexity)]
    fn execute_parse_query_successor(
        &mut self,
        snapshot: &SourceSnapshot,
        revision: crate::ImportInputRevision,
        prepared: PreparedSuccessorParse,
        attempt_id: AttemptId,
    ) -> (
        ParseQueryRecord,
        Arc<dyn AttemptView>,
        QueryAttemptExecution,
        ParsedModulesWork,
        ParseInvalidationSummary,
        Arc<rue_query::QueryTerminal<ParseQueryRecord>>,
    ) {
        let PreparedSuccessorParse {
            predecessor_program,
            predecessor_order,
            predecessor_revision,
            predecessor_terminal,
            appended,
            segment,
        } = prepared;
        let successor_order = crate::shared_segments::SharedList::extend(
            &predecessor_order,
            appended.iter().map(|(module, _)| module.clone()).collect(),
        );
        self.select_diagnostic_presentation(Some(successor_order.clone()));
        let presentation = DiagnosticAttemptProvenance::Presentation(successor_order);

        // A successor key embeds only the published lineage identity and its
        // appended segment.
        self.parse_key_entries_compared = self
            .parse_key_entries_compared
            .saturating_add(segment.len() as u64);
        let key = ParseQueryKey::Successor {
            revision,
            segment,
            predecessor: predecessor_revision,
        };
        let runtime_revision =
            // The runtime revision's compatibility slot is the observation
            // regime, not the per-request counter (RUE-1137). This must match
            // how import publication built the revision, or the module-input
            // and parse projections cannot find their published views.
            rue_query::Revision::new(revision.revision_id, revision.compatibility_token);
        self.parse_modules_dispatched = self
            .parse_modules_dispatched
            .saturating_add(appended.len() as u64);
        let (modular_result, modular_work) = self.queries.revisioned.parse_program_extension(
            runtime_revision,
            &predecessor_program,
            &appended,
        );
        self.parse_invalidation_entries_compared = self
            .parse_invalidation_entries_compared
            .saturating_add(appended.len() as u64);
        let appended_modules: Vec<crate::ModuleId> =
            appended.iter().map(|(module, _)| module.clone()).collect();
        let parse_family = self.queries.revisioned.parse_family();
        let attempt = self.queries.revisioned.request_parse(
            runtime_revision,
            attempt_id,
            key.clone(),
            |context| {
                // The record adopts the CAPTURED predecessor parse terminal as a
                // runtime dependency — the exact terminal held by preparation,
                // observed by node, incarnation, and stamp with no key hash or
                // content comparison — so successor-after-predecessor is a real
                // query edge: red/green validation and leases flow through it,
                // and the node's endorsement at this revision carries the exact
                // stamp to every compatible descendant. Adoption is sound here
                // because parse keys are content-addressed: the key alone pins
                // the terminal's value. A stale or evicted terminal aborts the
                // attempt rather than being silently re-derived.
                if parse_family
                    .observe_adopted_terminal(context, &predecessor_terminal)
                    .is_err()
                {
                    return Err(rue_query::QueryAbort::Canceled);
                }
                // Plus exactly the appended modules' input leaves; the remaining
                // predecessor content is pinned by the dependency above and the
                // published lineage identity in the key.
                for (module, _) in &appended {
                    context.input(
                    crate::revisioned_query_database::RevisionedQueryDatabase::module_source_input(
                        module,
                    ),
                )?;
                }
                let work = modular_work;
                let invalidation =
                    crate::parsed_modules::classify_successor_invalidation(&appended_modules);
                let result = modular_result;
                // Freeze diagnostics privately with the query output. Session
                // selection and metrics happen only after atomic publication.
                let diagnostics = Arc::new(FrontendDiagnosticSnapshot {
                    source: snapshot.clone(),
                    stage: FrontendDiagnosticIdentity::Syntax,
                    provenance: presentation.clone(),
                    errors: result
                        .as_ref()
                        .err()
                        .map_or_else(|| Arc::from([]), |errors| errors.as_slice().to_vec().into()),
                    warnings: Arc::from([]),
                });
                Ok(ParseQueryRecord {
                    key,
                    runtime_revision,
                    snapshot: snapshot.clone(),
                    result,
                    diagnostics,
                    work,
                    invalidation,
                })
            },
        );
        self.queries.revisioned.select_parse_candidate(&attempt);
        let terminal = attempt
            .terminal()
            .unwrap_or_else(|| panic!("parse query aborted: {:?}", attempt.abort()))
            .clone();
        let record = match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => record.clone(),
            rue_query::QueryOutcome::Failure(_) => unreachable!("parse retains typed records"),
        };
        let execution = match attempt.execution() {
            rue_query::RequestExecution::Computed => {
                self.metrics
                    .diagnostic_publication(self.diagnostics.latest().is_some());
                QueryAttemptExecution::Computed
            }
            rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined => {
                self.reuse_diagnostics(record.diagnostics.clone());
                QueryAttemptExecution::Reused
            }
            rue_query::RequestExecution::Aborted => unreachable!(),
        };
        let work = if execution == QueryAttemptExecution::Computed {
            record.work
        } else {
            ParsedModulesWork::default()
        };
        // A successor record's classification is relative to the retained
        // predecessor its key pins, so the reused branch reuses it verbatim.
        let invalidation = record.invalidation.clone();
        let view = self.queries.revisioned.parse_attempt_view(
            attempt_id,
            attempt,
            QueryStructuralWork::Parse(work),
        );
        self.diagnostics.select(view.clone());
        (record, view, execution, work, invalidation, terminal)
    }

    pub(super) fn parse_staging_snapshot(
        &mut self,
        snapshot: &SourceSnapshot,
        successor: Option<(
            crate::ImportInputRevision,
            &Arc<[crate::ModuleRevision]>,
            Arc<rue_query::QueryTerminal<ParseQueryRecord>>,
        )>,
    ) -> (
        Result<Arc<ParsedProgram>, CompileErrors>,
        ParsedModulesWork,
        Option<ParseQueryRecord>,
        Option<Arc<rue_query::QueryTerminal<ParseQueryRecord>>>,
    ) {
        // A successor stage MUST extend its verified predecessor: a failed
        // predecessor binding rejects the stage rather than silently falling
        // back to a full content-keyed build under successor authority.
        let prepared_successor = match successor {
            Some((revision, delta, terminal)) => {
                match self.prepare_successor_parse(snapshot, delta, terminal) {
                    Ok(prepared) => Some((revision, prepared)),
                    Err(errors) => {
                        return (Err(errors), ParsedModulesWork::default(), None, None);
                    }
                }
            }
            None => None,
        };
        let staged_successor = prepared_successor.is_some();
        let mut guard = self.metrics.begin_unprojected("parse");
        let attempt_id = guard.id;
        let (record, view, execution, work, _invalidation, terminal) = match prepared_successor {
            Some((revision, prepared)) => {
                self.execute_parse_query_successor(snapshot, revision, prepared, attempt_id)
            }
            None => {
                let order = snapshot
                    .files()
                    .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
                    .collect::<Vec<_>>();
                self.parse_sources_materialized = self
                    .parse_sources_materialized
                    .saturating_add(order.len() as u64);
                let order = crate::shared_segments::SharedList::flat(order.into());
                self.select_diagnostic_presentation(Some(order.clone()));
                let presentation = DiagnosticAttemptProvenance::Presentation(order);
                self.execute_parse_query(snapshot, presentation, attempt_id, false)
            }
        };
        guard.started();
        let result = record.result.clone();
        guard.attach_diagnostics(record.diagnostics.clone());
        guard.bind(view);
        guard.finish(execution, None, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        let retained = staged_successor.then(|| record.clone());
        (result, work, retained, Some(terminal))
    }

    fn run_parse_update(
        &mut self,
        snapshot: &SourceSnapshot,
        presentation: DiagnosticAttemptProvenance,
    ) -> CompilerSessionUpdate {
        let mut guard = self.metrics.begin_unprojected("parse");
        let attempt_id = guard.id;
        let (record, view, execution, parse_work, invalidation, _) =
            self.execute_parse_query(snapshot, presentation, attempt_id, true);
        guard.started();
        self.metrics.update(parse_work, invalidation.clone());
        let result = record.result.clone();
        let diagnostics = record.diagnostics.clone();
        guard.attach_diagnostics(diagnostics.clone());
        guard.bind(view);
        guard.finish(execution, None, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        self.refresh_retention_metrics();
        match result {
            Ok(candidate) => {
                // An open discovery artifact survives only an EXACT
                // republication of its own snapshot. Source revisions exclude
                // physical paths and presentation order, so a same-revision
                // replacement update (relocated or reordered files) must still
                // invalidate the artifact — otherwise its later close would
                // republish the superseded snapshot, rolling physical and
                // presentation state backward (RUE-1823).
                self.retain_open_discovery_for_exact_snapshot(snapshot);
                let exact = self.published.as_deref().is_some_and(|published| {
                    programs_are_pointer_equivalent(published, &candidate)
                });
                let downstream_invalidated = self.published.is_some() && !exact;
                if exact {
                    self.published_snapshot = Some(snapshot.clone());
                    CompilerSessionUpdate {
                        result: Ok(self.published.as_ref().unwrap().clone()),
                        work: parse_work,
                        #[cfg(test)]
                        invalidation,
                        downstream_invalidated: false,
                        diagnostics,
                    }
                } else {
                    self.metrics
                        .project_dependency_invalidations(downstream_invalidated);
                    self.published = Some(candidate.clone());
                    self.published_snapshot = Some(snapshot.clone());
                    CompilerSessionUpdate {
                        result: Ok(candidate),
                        work: parse_work,
                        #[cfg(test)]
                        invalidation,
                        downstream_invalidated,
                        diagnostics,
                    }
                }
            }
            Err(errors) => CompilerSessionUpdate {
                result: Err(errors),
                work: parse_work,
                #[cfg(test)]
                invalidation,
                downstream_invalidated: false,
                diagnostics,
            },
        }
    }

    /// The successor-close counterpart of [`Self::run_parse_update`]: adopts
    /// the successor parse terminal for semantic queries with the same
    /// publication bookkeeping, without re-running the whole-program
    /// content-keyed projection (RUE-1112). The candidate extends the retained
    /// predecessor by construction, so downstream invalidation follows from an
    /// existing publication rather than a module-table comparison.
    fn run_parse_update_successor(
        &mut self,
        snapshot: &SourceSnapshot,
        retained: ParseQueryRecord,
    ) -> CompilerSessionUpdate {
        let mut guard = self.metrics.begin_unprojected("parse");
        let attempt_id = guard.id;
        // Re-request the exact staged terminal: same key, same revision. The
        // stage's selection protects that terminal, so this reuses it without
        // publishing anything new; the recompute body republishes the retained
        // record verbatim only if the terminal were ever evicted.
        if let ParseQueryKey::Successor { segment, .. } = &retained.key {
            self.parse_key_entries_compared = self
                .parse_key_entries_compared
                .saturating_add(segment.len() as u64);
        }
        let key = retained.key.clone();
        let runtime_revision = retained.runtime_revision;
        let recompute = retained.clone();
        let attempt =
            self.queries
                .revisioned
                .request_parse(runtime_revision, attempt_id, key, |context| {
                    for module in &recompute.invalidation.added {
                        context.input(
                    crate::revisioned_query_database::RevisionedQueryDatabase::module_source_input(
                        module,
                    ),
                )?;
                    }
                    Ok(recompute.clone())
                });
        self.queries.revisioned.select_parse(&attempt);
        let terminal = attempt
            .terminal()
            .unwrap_or_else(|| panic!("parse query aborted: {:?}", attempt.abort()));
        let record = match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => record.clone(),
            rue_query::QueryOutcome::Failure(_) => unreachable!("parse retains typed records"),
        };
        let execution = match attempt.execution() {
            rue_query::RequestExecution::Computed => QueryAttemptExecution::Computed,
            rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined => {
                self.reuse_diagnostics(record.diagnostics.clone());
                QueryAttemptExecution::Reused
            }
            rue_query::RequestExecution::Aborted => unreachable!(),
        };
        // The stage already accounted this terminal's parse work; re-selecting
        // it at close performs none.
        let parse_work = ParsedModulesWork::default();
        let invalidation = record.invalidation.clone();
        let view = self.queries.revisioned.parse_attempt_view(
            attempt_id,
            attempt,
            QueryStructuralWork::Parse(parse_work),
        );
        self.diagnostics.select(view.clone());
        guard.started();
        self.metrics.update(parse_work, invalidation.clone());
        let result = record.result.clone();
        let diagnostics = record.diagnostics.clone();
        guard.attach_diagnostics(diagnostics.clone());
        guard.bind(view);
        guard.finish(execution, None, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        match result {
            Ok(candidate) => {
                // An open discovery artifact survives only an EXACT
                // republication of its own snapshot. Source revisions exclude
                // physical paths and presentation order, so a same-revision
                // replacement update (relocated or reordered files) must still
                // invalidate the artifact — otherwise its later close would
                // republish the superseded snapshot, rolling physical and
                // presentation state backward (RUE-1823).
                self.retain_open_discovery_for_exact_snapshot(snapshot);
                let downstream_invalidated = self.published.is_some();
                // The predecessor source leaf stays live: additive adoption
                // must not disappear it and transitively invalidate every
                // retained terminal that still correctly depends on it.
                self.metrics.project_dependency_invalidations(false);
                self.published = Some(candidate.clone());
                self.published_snapshot = Some(snapshot.clone());
                self.refresh_retention_metrics();
                CompilerSessionUpdate {
                    result: Ok(candidate),
                    work: parse_work,
                    #[cfg(test)]
                    invalidation,
                    downstream_invalidated,
                    diagnostics,
                }
            }
            Err(errors) => CompilerSessionUpdate {
                result: Err(errors),
                work: parse_work,
                #[cfg(test)]
                invalidation,
                downstream_invalidated: false,
                diagnostics,
            },
        }
    }

    pub(crate) fn merge(&mut self) -> Result<Arc<CanonicalMergedProgram>, CompileErrors> {
        let mut guard = self.metrics.begin::<MergeQuery>();
        let attempt_id = guard.id;
        let mut execution = QueryAttemptExecution::Rejected;
        let mut origin = None;
        let mut attempt_work = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.merge_attempt(
                attempt_id,
                &mut guard,
                &mut execution,
                &mut origin,
                &mut attempt_work,
            )
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        let structural = attempt_work
            .map(QueryStructuralWork::Merge)
            .unwrap_or(QueryStructuralWork::None);
        guard.finish(execution, origin, &result, structural);
        self.metrics.synchronize();
        result
    }

    fn merge_attempt(
        &mut self,
        _attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        _origin: &mut Option<AttemptId>,
        attempt_work: &mut Option<CanonicalMergeWork>,
    ) -> Result<Arc<CanonicalMergedProgram>, CompileErrors> {
        self.require_closed_discovery()?;
        let parsed = self.published.clone().ok_or_else(no_published_program)?;
        *execution = QueryAttemptExecution::Computed;
        guard.started();
        let runtime_revision = self
            .queries
            .revisioned
            .last_good_parse_record()
            .expect("merge has a successful parse terminal")
            .runtime_revision;
        let projected_indexes = {
            let _span = tracing::info_span!("module_index_projection").entered();
            self.queries
                .revisioned
                .projected_module_indexes(runtime_revision, &parsed)
        };
        // Freeze the traversal work before the fallible duplicate/definition
        // checks so deterministic merge failures retain the work already done.
        *attempt_work = Some(CanonicalMergeWork {
            modules_visited: parsed.modules().len(),
            items_visited: parsed
                .modules()
                .iter()
                .map(|module| module.ast().items.len())
                .sum(),
            candidates_visited: projected_indexes.as_ref().map_or(0, |indexes| {
                indexes.iter().map(|index| index.definitions.len()).sum()
            }),
            ..CanonicalMergeWork::default()
        });
        guard.accrue(QueryStructuralWork::Merge(
            attempt_work.expect("merge prefix just installed"),
        ));
        let batch_order = self
            .batch_diagnostic_order
            .as_ref()
            .map(crate::shared_segments::SharedList::as_arc);
        let merged = {
            let _span = tracing::info_span!("canonical_merge").entered();
            projected_indexes
                .and_then(|indexes| {
                    merge_parsed_modules_reusing_indexes(
                        &parsed,
                        &indexes,
                        self.definition_shard_baseline.as_ref(),
                        batch_order.as_deref(),
                    )
                })
                .map(Arc::new)
        };
        if let Ok(merged) = &merged {
            debug_assert_eq!(merged.ast().source_revision(), parsed.source_revision());
            *attempt_work = Some(merged.work());
            guard.accrue(QueryStructuralWork::Merge(merged.work()));
        }
        if let Ok(merged) = &merged {
            self.definition_shard_baseline = Some(merged.definitions().clone());
        }
        let source = self
            .published_snapshot
            .clone()
            .expect("published program retains source snapshot");
        let diagnostics = self.publish_diagnostics(
            &source,
            FrontendDiagnosticIdentity::Merge,
            merged.as_ref().err(),
            &[],
        );
        guard.attach_diagnostics(diagnostics.clone());
        self.diagnostics.select_snapshot(&diagnostics);
        self.refresh_retention_metrics();
        merged
    }

    /// Query the canonical RIR through an immutable, owner-retaining view.
    pub fn rir(&mut self) -> Result<Arc<crate::RirView>, CompileErrors> {
        self.canonical_rir().map(crate::RirView::new).map(Arc::new)
    }

    pub(crate) fn canonical_rir(&mut self) -> Result<Arc<CanonicalRirOutput>, CompileErrors> {
        let mut guard = self.metrics.begin::<RirQuery>();
        let attempt_id = guard.id;
        let mut execution = QueryAttemptExecution::Rejected;
        let mut origin = None;
        let mut attempt_work = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.rir_attempt(
                attempt_id,
                &mut guard,
                &mut execution,
                &mut origin,
                &mut attempt_work,
            )
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        let structural = attempt_work
            .map(QueryStructuralWork::Rir)
            .unwrap_or(QueryStructuralWork::None);
        guard.finish(execution, origin, &result, structural);
        self.metrics.synchronize();
        result
    }

    fn rir_attempt(
        &mut self,
        _attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        _origin: &mut Option<AttemptId>,
        attempt_work: &mut Option<CanonicalRirWork>,
    ) -> Result<Arc<CanonicalRirOutput>, CompileErrors> {
        self.require_successful_import_diagnostics()?;
        let source = self
            .published
            .as_ref()
            .ok_or_else(no_published_program)?
            .source_revision()
            .clone();
        let merged = self.merge();
        let result = match &merged {
            Ok(merged) => {
                *execution = QueryAttemptExecution::Computed;
                guard.started();
                let revision = self
                    .queries
                    .revisioned
                    .last_good_parse_record()
                    .expect("published syntax has a successful parse terminal")
                    .runtime_revision;
                let module_ids = merged
                    .ast()
                    .modules()
                    .iter()
                    .map(|module| module.module_id().clone())
                    .collect::<Vec<_>>();
                let (module_rirs, query_work) = {
                    let _span = tracing::info_span!("module_rir_lowering").entered();
                    self.queries
                        .revisioned
                        .compose_candidate_module_rirs(revision, module_ids)
                };
                match module_rirs {
                    Ok(modules) => {
                        let projected = {
                            let _span = tracing::info_span!("rir_projection").entered();
                            project_candidate_module_rirs_with_work(merged, &modules, query_work, {
                                #[cfg(test)]
                                {
                                    self.interner_limit
                                        .unwrap_or(rue_lexer::MAX_INTERNED_STRINGS)
                                }
                                #[cfg(not(test))]
                                {
                                    rue_lexer::MAX_INTERNED_STRINGS
                                }
                            })
                        };
                        match projected {
                            Ok(rir) => {
                                let rir = Arc::new(rir);
                                *attempt_work = Some(rir.work());
                                guard.accrue(QueryStructuralWork::Rir(rir.work()));
                                Ok(rir)
                            }
                            Err((error, work)) => {
                                *attempt_work = Some(work);
                                guard.accrue(QueryStructuralWork::Rir(work));
                                Err(CompileErrors::from(error))
                            }
                        }
                    }
                    Err(errors) => {
                        *attempt_work = Some(query_work);
                        guard.accrue(QueryStructuralWork::Rir(query_work));
                        Err(errors)
                    }
                }
            }
            Err(errors) => Err(errors.clone()),
        };
        if let Ok(rir) = &result {
            debug_assert_eq!(rir.source_revision(), &source);
        }
        let source_snapshot = self
            .published_snapshot
            .clone()
            .expect("RIR query retains its exact source snapshot");
        let diagnostics = Arc::new(FrontendDiagnosticSnapshot {
            source: source_snapshot,
            stage: FrontendDiagnosticIdentity::Rir(source.clone()),
            provenance: DiagnosticAttemptProvenance::Canonical,
            errors: result
                .as_ref()
                .err()
                .map_or_else(|| Arc::from([]), |errors| errors.as_slice().to_vec().into()),
            warnings: Arc::from([]),
        });
        guard.attach_diagnostics(diagnostics.clone());
        self.diagnostics.select_snapshot(&diagnostics);
        self.refresh_retention_metrics();
        result
    }

    /// Report the declared test candidates the request's module closure does
    /// not contain (ADR-0083 §1).
    ///
    /// Discovery is the import closure, so a `parser_tests.rue` nothing imports
    /// does not exist to any request — and forgetting the wiring produces
    /// silence rather than a diagnostic. The declared candidate inventory is
    /// what breaks that silence: every candidate outside the closure that
    /// parses as containing test items, or that could not be parsed at all, is
    /// reported here.
    ///
    /// Three properties are deliberate. Absent candidates are silent, because a
    /// build target's `srcs` glob legitimately names files another root owns.
    /// Candidates are never modules: scanning one publishes no module leaf,
    /// joins no closure, and roots nothing. And the result is a report, not a
    /// diagnostic — the caller decides whether this request wants the warning
    /// (`rue test` does; an ordinary build does not).
    ///
    /// Closure membership is decided by comparing the candidate's normalized
    /// logical path with each module's published identity as a string. A
    /// candidate reached by the program under a different spelling than the
    /// one the build declared — a symlink, or a path the resolver rewrote — is
    /// therefore reported as unimported even though its file is in the closure.
    /// Matching on canonical physical identity instead would need candidate
    /// acquisition to return the canonical path; that is a small protocol
    /// addition left for a real report of the false positive.
    ///
    /// The returned rows are ordered by path.
    pub(crate) fn unimported_test_files(
        &mut self,
        candidates: &crate::TestCandidateInventory,
    ) -> Result<Vec<crate::UnimportedTestFile>, CompileErrors> {
        let committed = self.committed_import_discovery_artifact().ok_or_else(|| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "reporting unimported test files requires a closed-valid import discovery revision"
                    .into(),
            )))
        })?;
        // An inventory acquired under one read regime cannot describe a closure
        // discovered under another: the same spelling names a different file.
        let context = committed.context();
        if context.project_root() != candidates.project_root()
            || context.std_root().unwrap_or("") != candidates.std_root().unwrap_or("")
            || context.read_policy_revision() != candidates.read_policy_revision()
        {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "test candidates were declared under a different read policy than the \
                     compiled closure"
                        .into(),
                ),
            )));
        }

        let program = self
            .published_owner()
            .cloned()
            .ok_or_else(no_published_program)?;
        let closure = program
            .modules()
            .iter()
            .map(|module| module.module_id().as_str().to_owned())
            .collect::<std::collections::BTreeSet<_>>();

        let revision = self
            .queries
            .revisioned
            .publish_test_candidate_inputs(candidates)
            .map_err(CompileErrors::from)?;

        let mut reported = Vec::new();
        for candidate in candidates.candidates() {
            if closure.contains(candidate.path()) {
                continue;
            }
            let attempt = self
                .queries
                .revisioned
                .test_candidate_scan(revision, candidate.identity().clone());
            let terminal = attempt.terminal().ok_or_else(|| {
                CompileErrors::from(CompileError::without_span(ErrorKind::InternalError(
                    format!(
                        "test-candidate scan for '{}' published no terminal",
                        candidate.path()
                    ),
                )))
            })?;
            let rue_query::QueryOutcome::Success(scan) = terminal.outcome() else {
                return Err(CompileErrors::from(CompileError::without_span(
                    ErrorKind::InternalError(format!(
                        "test-candidate scan for '{}' failed",
                        candidate.path()
                    )),
                )));
            };
            let scan = scan.0;
            if scan.tests == 0 && !scan.parse_failed {
                continue;
            }
            reported.push(crate::UnimportedTestFile {
                path: candidate.path().to_owned(),
                tests: scan.tests,
                parse_failed: scan.parse_failed,
            });
        }
        reported.sort();
        Ok(reported)
    }
}

fn programs_are_pointer_equivalent(left: &ParsedProgram, right: &ParsedProgram) -> bool {
    left.source_revision() == right.source_revision()
        && left.modules().len() == right.modules().len()
        && left
            .modules()
            .iter()
            .zip(right.modules())
            .all(|(left, right)| Arc::ptr_eq(left, right))
}

pub(super) fn no_published_program() -> CompileErrors {
    CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
        "frontend query session has no successful parsed program".to_string(),
    )))
}

#[cfg(test)]
mod tests;
