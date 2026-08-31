//! Session construction, cancellation recovery, and revision-facing lifecycle.

use super::{
    CanonicalImportGraph, CompileError, CompileErrors, CompilerSession,
    DiagnosticAttemptProvenance, ErrorKind, FrontendDiagnosticIdentity, FrontendDiagnosticSnapshot,
    QueryComputationGuard, no_published_program,
};
#[cfg(test)]
use super::{CompileOptions, StablePreviewFeatures};
use std::sync::Arc;

impl CompilerSession {
    #[cfg(test)]
    pub(crate) fn cancel_constraint_generation_after_nodes_for_test(
        &self,
        nodes: usize,
    ) -> crate::revisioned_query_database::TestConstraintGenerationCancellationGuard {
        self.queries
            .revisioned
            .cancel_constraint_generation_after_nodes_for_test(nodes)
    }

    #[cfg(test)]
    pub(crate) fn constraint_generation_visits_for_test(&self) -> usize {
        self.queries
            .revisioned
            .constraint_generation_visits_for_test()
    }

    #[cfg(test)]
    pub(crate) fn any_successful_body_transaction_for_test(&self) -> bool {
        self.queries
            .revisioned
            .any_successful_body_transaction_for_test()
    }

    #[cfg(test)]
    pub(crate) fn empty_body_closure_work_for_test(
        &self,
        options: &CompileOptions,
    ) -> (crate::CandidateBodyPlanWork, crate::CandidateBodyPlanWork) {
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("empty body-closure test requires a semantic revision");
        let request = self
            .queries
            .revisioned
            .body_closure(
                revision,
                crate::body_query::BodyClosureQueryKey {
                    modules: Arc::from([]),
                    roots: Arc::from([]),
                    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                        target: options.target.clone(),
                        preview_features: StablePreviewFeatures::new(&options.preview_features),
                    },
                },
                rue_query::CancellationToken::new(),
            )
            .expect("empty body-closure query must publish");
        (
            request.candidate_body_plan_work,
            request.candidate_body_materialization_work,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_query_concurrency(workers: usize) -> Self {
        let mut session = Self::default();
        session.queries.revisioned =
            crate::revisioned_query_database::RevisionedQueryDatabase::with_query_concurrency(
                workers,
            );
        session
    }

    /// Construct a canonical session with a bounded shared symbol space for
    /// deterministic resource-limit regression tests. The bound is owned by
    /// the query database and therefore reaches the worker threads that run
    /// canonical materialization.
    #[cfg(test)]
    pub(crate) fn with_interner_limit(max_entries: usize) -> Self {
        let mut session = Self::default();
        session.interner_limit = Some(max_entries);
        session.queries.revisioned =
            crate::revisioned_query_database::RevisionedQueryDatabase::with_interner_limit(
                max_entries,
            );
        session
    }

    #[cfg(test)]
    pub(crate) fn with_cfg_interner_limit(max_entries: usize) -> Self {
        let mut session = Self::default();
        session.cfg_interner_limit = Some(max_entries);
        session
    }

    #[cfg(test)]
    pub(crate) fn with_cfg_accessor_failure() -> Self {
        let mut session = Self::default();
        session.cfg_accessor_failure = true;
        session
    }

    /// Force one exact production CodegenUnit request through an owner/joiner
    /// schedule. The normal rooted CFG query supplies the key; the registered
    /// CodegenUnit evaluator supplies the value. This controls only when the
    /// owner may finish and never constructs a peer artifact path.
    #[cfg(test)]
    pub(crate) fn exercise_codegen_schedule_for_test(
        &mut self,
        options: &CompileOptions,
        cancel_joiner: bool,
    ) -> (rue_query::RequestExecution, rue_query::RequestExecution) {
        let rooted = self
            .rooted_cfg(options)
            .expect("the schedule fixture reaches a valid CFG");
        let [cfg] = rooted.cfgs.as_slice() else {
            panic!("the schedule fixture must reach exactly one CodegenUnit");
        };
        let revision = rooted.graph.revision;
        let key = cfg.optimized_cfg_key.clone();
        let database = &self.queries.revisioned;
        let gate = database.arm_codegen_evaluator_gate_for_test();
        let baseline = database.runtime_metrics_for_test();
        let joiner_cancellation = rue_query::CancellationToken::new();

        let (owner_execution, joiner_execution) = std::thread::scope(|scope| {
            let owner_key = key.clone();
            let owner = scope.spawn(|| {
                database
                    .codegen_unit(
                        revision,
                        owner_key,
                        options.target,
                        rue_codegen::BackendArtifactRequest::default(),
                        options.opt_level,
                        rue_query::CancellationToken::new(),
                    )
                    .expect("the owner CodegenUnit request is registered")
            });
            gate.wait_until_entered();

            let joiner_key = key.clone();
            let joiner_token = joiner_cancellation.clone();
            let joiner = scope.spawn(|| {
                database
                    .codegen_unit(
                        revision,
                        joiner_key,
                        options.target,
                        rue_codegen::BackendArtifactRequest::default(),
                        options.opt_level,
                        joiner_token,
                    )
                    .expect("the joining CodegenUnit request is registered")
            });

            let wait_for = |predicate: &dyn Fn(rue_query::RuntimeMetrics) -> bool| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !predicate(database.runtime_metrics_for_test())
                    && std::time::Instant::now() < deadline
                {
                    std::thread::yield_now();
                }
                predicate(database.runtime_metrics_for_test())
            };
            let joined = wait_for(&|metrics| metrics.joins > baseline.joins);
            let canceled = if cancel_joiner && joined {
                joiner_cancellation.cancel();
                wait_for(&|metrics| metrics.cancellations > baseline.cancellations)
            } else {
                true
            };

            // Always release the owner before asserting the schedule so a
            // failed observation cannot strand a scoped worker.
            gate.release();
            let owner = owner.join().expect("CodegenUnit owner did not panic");
            let joiner = joiner.join().expect("CodegenUnit joiner did not panic");
            assert!(
                joined,
                "the exact-key request did not join within 5 seconds"
            );
            assert!(
                canceled,
                "the joined waiter did not cancel within 5 seconds"
            );
            assert!(
                owner.terminal().is_some(),
                "the live owner must publish the canonical CodegenUnit"
            );
            if cancel_joiner {
                assert!(matches!(
                    joiner.abort(),
                    Some(rue_query::QueryAbort::Canceled)
                ));
                assert!(joiner.terminal().is_none());
            } else {
                assert!(joiner.terminal().is_some());
            }
            (owner.execution(), joiner.execution())
        });
        (owner_execution, joiner_execution)
    }

    /// Gate the first `gated_children` production CodegenUnit evaluators in one
    /// rooted batch and return their peak simultaneous occupancy.
    #[cfg(test)]
    pub(crate) fn exercise_codegen_batch_overlap_for_test(
        &mut self,
        options: &CompileOptions,
        gated_children: usize,
        rendezvous: bool,
    ) -> (usize, usize) {
        let gate = self
            .queries
            .revisioned
            .arm_codegen_batch_evaluator_gate_for_test(gated_children, rendezvous);
        if rendezvous {
            std::thread::scope(|scope| {
                let compilation = scope.spawn(|| {
                    self.rooted_codegen(options, rue_codegen::BackendArtifactRequest::default())
                });
                let all_entered = gate.wait_until_all_entered_and_release();
                compilation
                    .join()
                    .expect("CodegenUnit batch compilation did not panic")
                    .expect("CodegenUnit batch fixture compiles successfully");
                assert!(
                    all_entered,
                    "CodegenUnit evaluators did not reach the requested concurrent occupancy"
                );
            });
        } else {
            self.rooted_codegen(options, rue_codegen::BackendArtifactRequest::default())
                .expect("CodegenUnit batch fixture compiles successfully");
        }
        (gate.peak(), gate.entered())
    }
    /// Perturb one canonical observation for the in-tree differential oracle.
    #[doc(hidden)]
    pub(crate) fn inject_stale_query_for_oracle(
        &mut self,
        fault: crate::unstable::DifferentialOracleFault,
    ) -> bool {
        match fault {
            crate::unstable::DifferentialOracleFault::Semantic
            | crate::unstable::DifferentialOracleFault::CfgTransformation => {
                self.oracle_fault = Some(fault);
                true
            }
            crate::unstable::DifferentialOracleFault::Diagnostic => {
                let Some(source) = self.published_snapshot.clone() else {
                    return false;
                };
                let errors = CompileErrors::from(CompileError::without_span(
                    ErrorKind::InternalError("differential diagnostic fault".into()),
                ));
                // This intentional oracle-only corruption must be selected as a
                // distinct canonical attempt. `publish_diagnostics` correctly
                // reuses an equal RIR key, which would hide this oracle fault.
                let snapshot = Arc::new(FrontendDiagnosticSnapshot {
                    source: source.clone(),
                    stage: FrontendDiagnosticIdentity::Rir(source.source_revision().clone()),
                    provenance: DiagnosticAttemptProvenance::Canonical,
                    errors: errors.iter().cloned().collect::<Vec<_>>().into(),
                    warnings: Arc::from([]),
                });
                self.diagnostics.select_snapshot(&snapshot);
                self.refresh_retention_metrics();
                true
            }
            crate::unstable::DifferentialOracleFault::Import => {
                self.inject_stale_import_query_for_oracle()
            }
        }
    }

    pub fn new() -> Self {
        <Self as ::core::default::Default>::default()
    }

    pub(super) fn resume_canceled_query(
        &mut self,
        guard: &mut QueryComputationGuard,
        payload: Box<dyn std::any::Any + Send>,
    ) -> ! {
        match guard.family {
            "import-diagnostics" | "merge" | "rir" | "semantic" | "definitions" | "parse" => {}
            family => unreachable!("unknown query guard family {family}"),
        }
        self.metrics.synchronize();
        std::panic::resume_unwind(payload)
    }

    /// Select the accepted import topology for semantic construction.
    ///
    /// Import-bearing revisions must come from the atomically adopted
    /// discovery artifact. A direct session remains usable for an import-free
    /// snapshot by supplying the uniquely valid empty graph; it may never
    /// reconstruct resolved imports from paths or environment state.
    pub(super) fn accepted_semantic_import_graph(
        &self,
    ) -> Result<CanonicalImportGraph, CompileErrors> {
        let program = self.published.as_ref().ok_or_else(no_published_program)?;
        let graph = if !program.import_directives().is_empty() {
            let committed = self.committed_import_graph()?;
            if &committed.input().sources != program.source_revision() {
                return Err(CompileErrors::from(CompileError::without_span(
                    ErrorKind::InvalidCompilerInput(
                        "committed import graph belongs to a foreign source revision".into(),
                    ),
                )));
            }
            committed.graph().clone()
        } else {
            crate::import_graph::import_free_canonical_graph(program.as_ref())?
        };
        Ok(graph)
    }
    pub fn published(&self) -> Option<crate::SyntaxView> {
        self.published.as_ref().cloned().map(crate::SyntaxView::new)
    }
}
