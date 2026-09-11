//! Rooted semantic, CFG, codegen, and object projections over the query database.

use super::{
    BackendQueryWork, CanonicalImportGraph, CodegenInputDescriptor, CompileError, CompileErrors,
    CompileOptions, CompileWarning, CompilerSession, ErrorKind, FrontendDiagnosticIdentity,
    PipelineRequestControl, RootedBodyGraph, RootedCfgOutput, RootedCfgUnit, RootedCodegenInput,
    RootedCodegenOutput, RootedCodegenReadyOutput, RootedParkOutcome,
    RootedPreOptimizationCfgOutput, RootedPreOptimizationCfgUnit, SemanticRequestControl,
    StablePreviewFeatures, collect_rooted_exports, no_published_program, pipeline_abort_errors,
    pipeline_control_errors, sort_rooted_warnings, unresolved_toolchain_park_errors,
};
use crate::SemanticInputDescriptor;
use ahash::AHashMap;
use rue_air::Node;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Where a rooted request can be canceled deterministically by a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootedCancellationSite {
    /// Just before the body-closure request every semantic consumer issues:
    /// the toolchain probe, a listing, a presentation, and a compile.
    BodyClosure,
    /// Just before the CFG batch — raw for a pre-optimization request,
    /// optimized for every other CFG-side presentation or compile — once the
    /// body closure has been analyzed.
    CfgBatch,
}

/// Deterministic mid-request cancellation for tests (RUE-2174), mirroring
/// `codegen_query::set_codegen_cancellation_tripwire`: reaching `site` cancels
/// the armed token before the request at that site is issued, so the stale
/// attempt must exit through the cancellation contract there rather than
/// completing or settling anything.
#[cfg(test)]
static ROOTED_CANCELLATION_TRIPWIRE: std::sync::Mutex<
    Option<(rue_query::CancellationToken, RootedCancellationSite)>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_rooted_cancellation_tripwire(
    tripwire: Option<(rue_query::CancellationToken, RootedCancellationSite)>,
) {
    *ROOTED_CANCELLATION_TRIPWIRE.lock().unwrap() = tripwire;
}

/// One tripwire probe. Free of cost outside test builds.
fn rooted_cancellation_probe(site: RootedCancellationSite) {
    #[cfg(test)]
    {
        let mut slot = ROOTED_CANCELLATION_TRIPWIRE.lock().unwrap();
        if let Some((token, armed)) = slot.as_ref()
            && *armed == site
        {
            token.cancel();
            *slot = None;
        }
    }
    #[cfg(not(test))]
    let _ = site;
}

impl CompilerSession {
    /// The call site that demanded `failing`, for a diagnostic raised inside a
    /// body the programmer did not write.
    ///
    /// An element-type gate (`@require_trivially_droppable(T)`, E0711) is
    /// stated in a container's own method body, so the analysis that rejects it
    /// runs over standard-library source with the programmer's element type
    /// substituted. That body has no call site of its own — one body analysis
    /// serves every caller, and the analysis is deliberately shared — so the
    /// demanding site cannot be threaded into the gate. It is recovered here
    /// instead, at the presentation boundary, from terminals the reached
    /// closure has already published: the earliest call instruction, in source
    /// order, that names the failing instance. Source positions therefore never
    /// enter query identity (ADR-0063); they are read back out of it.
    ///
    /// A method that delegates (`ArrayBuf::first` reading through
    /// `ArrayBuf::get`) puts one or more standard-library frames between the
    /// gate and the programmer, so the walk climbs until it reaches calls
    /// written outside the trusted standard library. The climb is
    /// breadth-first over *every* standard-library caller at each hop, not one
    /// of them: several wrappers reach one accessor (`ArrayBuf::first` and
    /// `ArrayBuf::last` both read through `ArrayBuf::get`), and following only
    /// the caller that sorts first would report whichever demand that one
    /// wrapper leads to rather than the earliest the programmer wrote.
    ///
    /// Every hop up to the cap is walked and every user-side call found along
    /// the way is a candidate, so the answer does not depend on how many
    /// library frames a demand happens to sit behind: a direct `a.get(0)` does
    /// not outrank an earlier `a.first()` merely for being one hop nearer. The
    /// winner is the earliest candidate in source order — by file path, then by
    /// offset — which is the same path-then-offset rule the driver already
    /// sorts a diagnostic batch by. It reports nothing rather than a guess when
    /// the chain runs out, which leaves the gate's own span primary.
    fn element_gate_demand_site(
        &self,
        revision: rue_query::Revision,
        closure: &crate::body_query::BodyClosureOutput,
        failing: &crate::body_query::BodyQueryKey,
        trusted_files: &ahash::AHashSet<rue_span::FileId>,
        cancellation: &rue_query::CancellationToken,
    ) -> Result<Option<rue_span::Span>, SemanticRequestControl> {
        /// Hops out of the gated body the walk takes. The eighth is the last
        /// one examined, so at most seven standard-library frames stand between
        /// the gate and a call the walk can report; past that it answers
        /// nothing and the gate's own span stays primary. Standard-library
        /// containers wrap one another a few layers deep at most, and the bound
        /// keeps a pathological or cyclic graph from turning a diagnostic into
        /// a scan.
        const MAX_DEMAND_HOPS: usize = 8;

        // The frontier is a `BTreeSet` and every answer is chosen by a total
        // source-identity order, so nothing about the result depends on the
        // closure's reachability order or on any hash iteration.
        let mut visited = BTreeSet::from([failing.instance.clone()]);
        let mut frontier = visited.clone();
        let mut demands = Vec::new();
        for _ in 0..MAX_DEMAND_HOPS {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier = BTreeSet::new();
            for callee in frontier.iter() {
                for caller in closure.bodies.iter() {
                    if caller.key.instance == *callee {
                        continue;
                    }
                    let rue_query::QueryOutcome::Success(bundle) = caller.bundle.outcome() else {
                        continue;
                    };
                    let crate::body_query::BodyTransaction::Success { body, .. } =
                        &bundle.transaction
                    else {
                        continue;
                    };
                    // Instruction anchors are relative to the body an export was
                    // taken from. An ordinary or specialized body starts at its
                    // locator's body span; an anonymous member's body is a
                    // fragment inside the producer the locator describes, so its
                    // own anchor is the extra offset.
                    let (semantic, anchor_base) = match body.as_ref() {
                        crate::body_query::CanonicalBody::Ordinary { body, .. } => (body, 0),
                        crate::body_query::CanonicalBody::Anonymous {
                            body, body_anchor, ..
                        } => (body, body_anchor.start),
                        crate::body_query::CanonicalBody::Specialization { body, .. } => (body, 0),
                    };
                    let Some(anchor) = earliest_call_anchor(semantic, callee) else {
                        continue;
                    };
                    let locator = self
                        .queries
                        .revisioned
                        .body_source_basis_projection(
                            revision,
                            caller.key.clone(),
                            cancellation.clone(),
                        )
                        .map_err(SemanticRequestControl::Abort)?;
                    let rue_query::QueryOutcome::Success(locator) = locator.outcome() else {
                        unreachable!("BodySourceLocator publishes typed values")
                    };
                    let Some(locator) = locator.as_ref() else {
                        continue;
                    };
                    let start = locator.body_start + anchor_base;
                    let span = rue_span::Span::with_file(
                        locator.file_id,
                        start + anchor.start,
                        start + anchor.end,
                    );
                    if trusted_files.contains(&span.file_id) {
                        // Another library frame between the gate and the
                        // programmer. Keep it for the next hop; the visited
                        // guard makes a shared or cyclic wrapper cost one
                        // expansion rather than one per path into it.
                        if visited.insert(caller.key.instance.clone()) {
                            next_frontier.insert(caller.key.instance.clone());
                        }
                        continue;
                    }
                    demands.push((
                        locator.physical_path.clone(),
                        span,
                        caller.key.instance.clone(),
                    ));
                }
            }
            frontier = next_frontier;
        }
        // Every candidate the whole walk found competes, so a demand written
        // earlier is not beaten by a later one that happens to be fewer hops
        // away. Reachability order is a scheduling artifact, so the winner is
        // chosen by source identity — the same path-then-offset rule the driver
        // already sorts a diagnostic batch by — with the body instance breaking
        // a tie between two calls at one position.
        Ok(demands
            .iter()
            .min_by(|left, right| {
                (&left.0, left.1.start, &left.2).cmp(&(&right.0, right.1.start, &right.2))
            })
            .map(|(_, span, _)| *span))
    }

    fn rooted_body_graph_attempt(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedBodyGraphAttempt, SemanticRequestControl> {
        self.require_successful_import_diagnostics()
            .map_err(SemanticRequestControl::Compile)?;
        let _imports = self
            .accepted_semantic_import_graph()
            .map_err(SemanticRequestControl::Compile)?;
        let program = self
            .published
            .clone()
            .ok_or_else(|| SemanticRequestControl::Compile(no_published_program()))?;
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .ok_or_else(|| SemanticRequestControl::Compile(no_published_program()))?;
        let modules = program
            .modules_iter()
            .map(|module| module.module_id().clone())
            .collect::<Vec<_>>();
        let _declaration_graph_collection_span =
            tracing::info_span!("declaration_graph_collection", phase = "semantic_analysis")
                .entered();
        let projection = match self
            .queries
            .revisioned
            .projected_declaration_semantics_for_modules(
                revision,
                modules.iter().cloned(),
                options.target,
                &options.preview_features,
                cancellation.clone(),
            ) {
            Ok(projection) => projection,
            Err(crate::revisioned_query_database::SemanticNucleusBatchFailure::Query(abort)) => {
                return Err(SemanticRequestControl::Abort(abort));
            }
            Err(crate::revisioned_query_database::SemanticNucleusBatchFailure::Stable {
                declaration,
                failure,
            }) => {
                return Err(SemanticRequestControl::Compile(
                    semantic_nucleus_failure_diagnostics(
                        program.modules(),
                        declaration.as_ref(),
                        &failure,
                    ),
                ));
            }
        };
        // This is the single root-set authority. `RootSelection::Tests` roots
        // every test declaration in the module closure and neither needs nor
        // roots `main`; `RootSelection::Executable` roots `main` plus the
        // c-export roots and never roots a test (ADR-0083 §1). The two sets are
        // disjoint, which is what makes a test body invisible to an executable
        // request rather than merely unreferenced by one.
        //
        // A test request additionally answers with the inventory of EVERY test
        // declaration, whether or not this request analyzes it: ordinals are
        // the inventory's own indices, and a request that excluded a
        // `compile_error` test from its root set would otherwise renumber every
        // later test (ADR-0083 §3).
        let mut test_inventory = Vec::new();
        let (main, roots) = match options.root_selection {
            crate::RootSelection::Tests => {
                let declared = projection
                    .declarations
                    .iter()
                    .filter(|declaration| {
                        declaration.key.kind() == crate::StableDefinitionKind::Test
                    })
                    .map(|declaration| {
                        crate::FunctionInstanceKey::Definition(declaration.key.clone())
                    })
                    .collect::<Vec<_>>();
                test_inventory =
                    crate::test_inventory::collect_test_inventory(program.modules(), &declared);
                let roots = declared
                    .into_iter()
                    .filter(|root| !self.excluded_test_roots.contains(root))
                    .collect::<BTreeSet<_>>();
                (None, roots)
            }
            crate::RootSelection::Executable => {
                let main = executable_main_declaration(&program, &projection)?;
                let mut roots =
                    BTreeSet::from([crate::FunctionInstanceKey::Definition(main.clone())]);
                roots.extend(
                    projection
                        .c_export_roots
                        .iter()
                        .map(|export| crate::FunctionInstanceKey::Definition(export.key.clone())),
                );
                (Some(main), roots)
            }
        };
        let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: options.target,
            preview_features: StablePreviewFeatures::new(&options.preview_features),
        };
        let root_identities: Arc<[crate::FunctionInstanceKey]> =
            roots.iter().cloned().collect::<Vec<_>>().into();
        drop(_declaration_graph_collection_span);

        // This compiler-owned consumer boundary includes retained-terminal
        // validation, query dispatch, deterministic terminal collection, and
        // the immediate work reduction. The timing layer records it
        // worker-locally, so the broad boundary does not serialize the query
        // runtime (RUE-1223).
        let _body_closure_collection_span =
            tracing::info_span!("body_closure_collection", phase = "semantic_analysis").entered();
        rooted_cancellation_probe(RootedCancellationSite::BodyClosure);
        let request = self
            .queries
            .revisioned
            .body_closure(
                revision,
                crate::body_query::BodyClosureQueryKey {
                    modules: modules.into(),
                    roots: Arc::clone(&root_identities),
                    configuration: configuration.clone(),
                },
                cancellation.clone(),
            )
            .map_err(SemanticRequestControl::Abort)?;
        let closure_terminal = &request.terminal;
        let rue_query::QueryOutcome::Success(closure) = closure_terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        if let Some(park) = &closure.parked_toolchain {
            return Err(SemanticRequestControl::Parked(Box::new(park.clone())));
        }
        let mut work = crate::CanonicalSemanticWork::default();
        request.accrue_reachability_work(&mut work.body_analysis);
        request.accrue_candidate_body_plan_work(&mut work);
        work.body_analysis.closure_bodies_visited = closure.bodies.len();
        for closure_body in closure.bodies.iter() {
            match request.execution_for(&closure_body.key) {
                rue_query::RequestExecution::Computed => {
                    work.body_analysis.body_analyses_computed += 1;
                    if request.was_retained(&closure_body.key) {
                        work.body_analysis.body_analyses_invalidated += 1;
                    }
                }
                rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined => {
                    work.body_analysis.body_analyses_reused += 1;
                }
                rue_query::RequestExecution::Aborted => unreachable!(
                    "a successful rooted body closure cannot retain an aborted body attempt"
                ),
            }
        }
        drop(_body_closure_collection_span);
        let _body_graph_projection_span =
            tracing::info_span!("body_graph_projection", phase = "semantic_analysis").entered();
        // Diagnostics are collected with the body each one belongs to, in the
        // order they have always been reported. A whole-run failure drops the
        // attribution and reports the sequence; the per-test test-image request
        // keeps it, because "which tests reach this broken body" is the whole
        // question a `compile_error` verdict answers (ADR-0083 §3).
        let mut errors = RootedBodyGraphRejection::default();
        for (instance, scheduling_errors) in closure.scheduling_errors.iter() {
            for error in scheduling_errors.iter() {
                errors.push_body(instance.clone(), error.clone());
            }
        }
        if let Some(fatal) = &closure.fatal {
            let fatal_errors = match fatal {
                crate::body_query::BodyClosureFatal::DeclarationFailed {
                    declaration,
                    failure,
                } => semantic_nucleus_failure_diagnostics(
                    program.modules(),
                    declaration.as_ref(),
                    failure,
                ),
                crate::body_query::BodyClosureFatal::ProducerFailed { failure, .. } => {
                    semantic_nucleus_failure_diagnostics(program.modules(), None, failure)
                }
                crate::body_query::BodyClosureFatal::WellKnownOptionResolution {
                    failure, ..
                } => well_known_option_resolution_diagnostics(program.modules(), failure),
                other => CompileError::without_span(ErrorKind::InternalError(format!(
                    "rooted body closure failed: {other:?}"
                )))
                .into(),
            };
            // A closure fatal stops the reachability walk itself, so the
            // closure it left behind is incomplete: bodies a later root would
            // have reached were never scheduled, and attributing the stop to
            // the roots walked so far would name an arbitrary subset. It stays
            // a whole-run failure (ADR-0083 §3's "outside every test closure").
            errors.extend_global(fatal_errors.iter().cloned());
        }

        let mut anonymous = BTreeMap::new();
        for fact in projection.anonymous_nominals.iter() {
            if let Err(identity) =
                crate::durable_semantics::merge_anonymous_nominal(&mut anonymous, fact)
            {
                errors.push_global(CompileError::without_span(ErrorKind::OutputPublication(
                    format!("conflicting anonymous facts for {identity:?}"),
                )));
            }
        }
        // Files the programmer did not write. An element-type gate stated in a
        // standard-library container body is rejected against the element type
        // the programmer chose, so its diagnostic belongs at the call that
        // demanded that body rather than at the gate line inside std.
        let trusted_files = program
            .modules_iter()
            .filter(|module| module.module_id().is_trusted_standard_library())
            .map(|module| module.file_id())
            .collect::<ahash::AHashSet<_>>();
        for closure_body in closure.bodies.iter() {
            let rue_query::QueryOutcome::Success(bundle) = closure_body.bundle.outcome() else {
                unreachable!("BodyAnalysisBundle publishes typed values")
            };
            if matches!(
                bundle.transaction,
                crate::body_query::BodyTransaction::DeterministicFailure { .. }
            ) {
                let locator = self
                    .queries
                    .revisioned
                    .body_source_basis_projection(
                        revision,
                        closure_body.key.clone(),
                        cancellation.clone(),
                    )
                    .map_err(SemanticRequestControl::Abort)?;
                let rue_query::QueryOutcome::Success(locator) = locator.outcome() else {
                    unreachable!("BodySourceLocator publishes typed values")
                };
                let projected = crate::revisioned_query_database::project_transaction_diagnostics(
                    bundle.transaction.clone(),
                    locator.as_ref(),
                );
                if let crate::body_query::BodyTransaction::DeterministicFailure {
                    errors: body_errors,
                    ..
                } = projected
                {
                    let body_errors = if body_errors
                        .iter()
                        .any(|error| is_element_gate_diagnostic(&error.kind))
                    {
                        let demand = self.element_gate_demand_site(
                            revision,
                            closure,
                            &closure_body.key,
                            &trusted_files,
                            &cancellation,
                        )?;
                        anchor_element_gate_diagnostics(body_errors, demand)
                    } else {
                        body_errors
                    };
                    errors.extend_body(
                        closure_body.key.instance.clone(),
                        body_errors.iter().cloned(),
                    );
                }
            }
            if let crate::body_query::BodyTransaction::Success {
                produced_anonymous_nominals,
                consulted_anonymous_nominals,
                ..
            } = &bundle.transaction
            {
                for fact in produced_anonymous_nominals
                    .0
                    .iter()
                    .chain(consulted_anonymous_nominals.0.iter())
                {
                    if let Err(identity) =
                        crate::durable_semantics::merge_anonymous_nominal(&mut anonymous, fact)
                    {
                        errors.push_global(CompileError::without_span(
                            ErrorKind::OutputPublication(format!(
                                "conflicting anonymous facts for {identity:?}"
                            )),
                        ));
                    }
                }
            }
            if let Some(crate::body_query::ProducedAnonymous::Produced(produced)) =
                &bundle.produced_anonymous
            {
                for fact in produced.0.iter() {
                    if let Err(identity) =
                        crate::durable_semantics::merge_anonymous_nominal(&mut anonymous, fact)
                    {
                        errors.push_global(CompileError::without_span(
                            ErrorKind::OutputPublication(format!(
                                "conflicting anonymous facts for {identity:?}"
                            )),
                        ));
                    }
                }
            }
        }
        for nominal in anonymous.values() {
            let crate::durable_semantics::DurableAnonymousNominalShape::Struct { methods, .. } =
                &nominal.shape
            else {
                continue;
            };
            let mut names = BTreeSet::new();
            if let Some(duplicate) = methods
                .iter()
                .find(|method| !names.insert(method.name.clone()))
            {
                errors.push_global(CompileError::without_span(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: format!(
                            "duplicate method `{}` in an anonymous struct",
                            duplicate.name
                        ),
                    },
                ));
            }
        }
        if !errors.is_empty() {
            errors.bodies = Arc::clone(&closure.bodies);
            errors.drop_glue_plans = Arc::clone(&closure.demanded_drop_glue_plans);
            errors.test_inventory = test_inventory.into();
            return Ok(RootedBodyGraphAttempt::Rejected(Box::new(errors)));
        }

        Ok(RootedBodyGraphAttempt::Graph(Box::new(RootedBodyGraph {
            revision,
            configuration,
            declarations: projection.declarations,
            declaration_index: projection.declaration_index,
            anonymous_nominals: anonymous.into_values().collect::<Vec<_>>().into(),
            declaration_dependencies: projection.dependencies,
            c_export_roots: projection.c_export_roots,
            modules: program.modules().to_vec().into(),
            main,
            // Every declared test, whether or not this request's root set
            // analyzes it: ordinals are indices into this table (ADR-0083 §2),
            // so an excluded test keeps its own and renumbers nothing.
            test_inventory: test_inventory.into(),
            excluded_tests: Arc::clone(&self.excluded_test_roots),
            roots: root_identities,
            closure: closure.clone(),
            work,
        })))
    }

    /// The body graph for a request, or the diagnostics that rejected it.
    ///
    /// The one entry point every rooted projection reaches semantic analysis
    /// through. Rejections keep the body each diagnostic belongs to so the
    /// test-image request can attribute one to the tests that reach it; every
    /// other caller flattens the rejection back into `CompileErrors` and is
    /// unchanged by the distinction.
    fn rooted_body_graph_with_cancellation(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedBodyGraph, SemanticRequestControl> {
        match self.rooted_body_graph_attempt(options, cancellation)? {
            RootedBodyGraphAttempt::Graph(graph) => Ok(*graph),
            RootedBodyGraphAttempt::Rejected(rejection) => {
                Err(SemanticRequestControl::Compile(rejection.into_errors()))
            }
        }
    }

    fn rooted_warning_references(
        &mut self,
        graph: &RootedBodyGraph,
        cancellation: rue_query::CancellationToken,
    ) -> Result<BTreeSet<crate::StableDefinitionKey>, PipelineRequestControl> {
        let functions = graph
            .declarations
            .iter()
            .filter(|declaration| declaration.key.kind() == crate::StableDefinitionKind::Function)
            .map(|declaration| {
                (
                    (
                        declaration.key.module().clone(),
                        Arc::<str>::from(declaration.key.name()),
                    ),
                    declaration.key.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let module_bindings = graph
            .declarations
            .iter()
            .filter_map(|declaration| {
                let crate::durable_semantics::DurableDeclarationPayload::ModuleBinding { target } =
                    &declaration.payload
                else {
                    return None;
                };
                Some((
                    (
                        declaration.key.module().clone(),
                        Arc::<str>::from(declaration.key.name()),
                    ),
                    target.clone(),
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let callable_aliases = graph
            .declarations
            .iter()
            .filter_map(|declaration| {
                let crate::durable_semantics::DurableDeclarationPayload::Const {
                    value: crate::durable_semantics::DurableConstValue::Function(target),
                    ..
                } = &declaration.payload
                else {
                    return None;
                };
                Some((
                    (
                        declaration.key.module().clone(),
                        Arc::<str>::from(declaration.key.name()),
                    ),
                    target.clone(),
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let resolve_head =
            |caller: &crate::ModuleId,
             head: &crate::revisioned_query_database::WarningStaticCallHead| {
                let (name, qualifiers) = head.components.split_last()?;
                let mut module = head.module.clone().unwrap_or_else(|| caller.clone());
                for qualifier in qualifiers {
                    module = module_bindings.get(&(module, qualifier.clone()))?.clone();
                }
                callable_aliases
                    .get(&(module.clone(), name.clone()))
                    .cloned()
                    .or_else(|| functions.get(&(module, name.clone())).cloned())
            };

        #[cfg(test)]
        self.warning_reference_executions.clear();
        let mut referenced = BTreeSet::new();
        let declarations = graph
            .declarations
            .iter()
            .filter(|declaration| declaration.key.kind().owns_body())
            .collect::<Vec<_>>();
        if declarations.is_empty() {
            self.metrics
                .set_warning_references(crate::unstable::WarningReferenceMetrics::default());
            return Ok(referenced);
        }
        let keys = declarations
            .iter()
            .map(|declaration| {
                crate::body_query::BodyQueryKey::new(
                    crate::FunctionInstanceKey::Definition(declaration.key.clone()),
                    graph.configuration.clone(),
                )
            })
            .collect::<Vec<_>>()
            .into();
        let (attempt, child_executions) = self.queries.revisioned.warning_body_reference_frontier(
            graph.revision,
            keys,
            cancellation,
        );
        let batch_execution = attempt.execution();
        let mut warning_work = crate::unstable::WarningReferenceMetrics {
            frontier_items: declarations.len(),
            frontier_batches: 1,
            frontier_batch_overhead: attempt
                .work()
                .iter()
                .find_map(|(name, count)| {
                    (name.as_ref() == "warning-reference.frontier.overhead")
                        .then_some(*count as usize)
                })
                .unwrap_or(0),
            ..crate::unstable::WarningReferenceMetrics::default()
        };
        for child in child_executions.iter().flatten() {
            match child.execution {
                rue_query::RequestExecution::Computed => warning_work.children_computed += 1,
                rue_query::RequestExecution::Reused => warning_work.children_reused += 1,
                rue_query::RequestExecution::Joined => warning_work.children_joined += 1,
                rue_query::RequestExecution::Aborted if child.canceled => {
                    warning_work.children_canceled += 1;
                }
                rue_query::RequestExecution::Aborted => {}
            }
        }
        self.metrics.set_warning_references(warning_work);
        let executions = child_executions
            .into_iter()
            .map(|execution| {
                execution
                    .map(|execution| execution.execution)
                    .unwrap_or(batch_execution)
            })
            .collect::<Vec<_>>();
        let terminal = attempt
            .into_result()
            .map_err(PipelineRequestControl::Abort)?;
        let rue_query::QueryOutcome::Success(batch) = terminal.outcome() else {
            unreachable!("WarningBodyReferenceFrontier publishes typed values")
        };
        assert_eq!(batch.values.len(), declarations.len());
        for ((declaration, projected), execution) in declarations
            .into_iter()
            .zip(batch.values.iter())
            .zip(executions.into_iter())
        {
            #[cfg(not(test))]
            let _ = execution;
            #[cfg(test)]
            self.warning_reference_executions
                .push((declaration.key.clone(), execution));
            let heads = match projected {
                crate::revisioned_query_database::WarningBodyReferencesValue::Available(heads) => {
                    heads
                }
                crate::revisioned_query_database::WarningBodyReferencesValue::Failure(failure) => {
                    return Err(CompileError::without_span(ErrorKind::InternalError(format!(
                        "warning body-reference projection failed: {failure:?}"
                    )))
                    .into());
                }
            };
            referenced.extend(
                heads
                    .iter()
                    .filter_map(|head| resolve_head(declaration.key.module(), head)),
            );
        }
        Ok(referenced)
    }

    pub(crate) fn rooted_cfg(
        &mut self,
        options: &CompileOptions,
    ) -> Result<RootedCfgOutput, CompileErrors> {
        self.rooted_cfg_request(options, rue_query::CancellationToken::new())
            .map_err(|control| pipeline_control_errors("rooted CFG", control))
    }

    /// [`Self::rooted_cfg`] under a caller's cancellation token, keeping the
    /// differential oracle's injected semantic fault on the same path so a
    /// cancellable presentation observes exactly what the uncancellable one
    /// does (RUE-2174).
    pub(crate) fn rooted_cfg_request(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCfgOutput, PipelineRequestControl> {
        if self.oracle_fault == Some(crate::unstable::DifferentialOracleFault::Semantic) {
            self.oracle_fault.take();
            return Err(PipelineRequestControl::Compile(CompileErrors::from(
                CompileError::without_span(ErrorKind::InternalError(
                    "differential semantic fault".into(),
                )),
            )));
        }
        self.rooted_cfg_with_cancellation(options, cancellation)
    }

    /// The request's ordered test inventory, analyzed but not lowered, with the
    /// diagnostics of every body in the closure that failed to analyze.
    ///
    /// This stops at the body graph deliberately (ADR-0083 §2: `--list` does
    /// semantic analysis of the test closure and no codegen), so it is the one
    /// entry point that answers a listing without building a CFG.
    ///
    /// The diagnostics come back beside the inventory rather than instead of
    /// it: a listing reports declarations, so a broken test is still listed,
    /// but a listing that swallowed the analysis failure would tell a reader a
    /// suite is fine when the run will report it broken.
    pub(crate) fn rooted_test_inventory(
        &mut self,
        options: &CompileOptions,
    ) -> Result<RootedTestInventory, CompileErrors> {
        match self.cancellable_test_inventory(options, rue_query::CancellationToken::new()) {
            TestInventoryOutcome::Listed(listing) => Ok(listing),
            TestInventoryOutcome::Errors(errors) => Err(errors),
            // The token this passed can never be canceled: nobody else holds it.
            TestInventoryOutcome::Canceled => {
                unreachable!("an uncancelled test inventory cannot report cancellation")
            }
        }
    }

    /// [`Self::rooted_test_inventory`] under a caller's cancellation token,
    /// for a retained host whose listing request can be abandoned (RUE-2174).
    ///
    /// Cancellation is an outcome rather than an error for the same reason it
    /// is one for the test-closure analysis: it says nothing about the program,
    /// so neither a partial inventory nor diagnostics may be reported for it.
    pub(crate) fn cancellable_test_inventory(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> TestInventoryOutcome {
        // A listing reports declarations, so a test whose body does not analyze
        // is still listed: its declaration parsed, and the inventory is built
        // from the declaration projection rather than from the bodies
        // (ADR-0083 §3). Only a rejection that reaches no inventory at all —
        // one the request itself refused, before roots existed — stops it.
        let (inventory, diagnostics) = match self.rooted_body_graph_attempt(options, cancellation) {
            Ok(RootedBodyGraphAttempt::Graph(graph)) => {
                (Arc::clone(&graph.test_inventory), CompileErrors::default())
            }
            Ok(RootedBodyGraphAttempt::Rejected(rejection)) => {
                if rejection.global {
                    return TestInventoryOutcome::Errors(rejection.into_errors());
                }
                (
                    Arc::clone(&rejection.test_inventory),
                    rejection.body_diagnostics(),
                )
            }
            Err(SemanticRequestControl::Abort(rue_query::QueryAbort::Canceled)) => {
                return TestInventoryOutcome::Canceled;
            }
            Err(control) => {
                return TestInventoryOutcome::Errors(semantic_control_errors(
                    "rooted test inventory",
                    control,
                ));
            }
        };
        TestInventoryOutcome::Listed(RootedTestInventory {
            entries: inventory.iter().map(|test| test.entry.clone()).collect(),
            diagnostics,
        })
    }

    pub(crate) fn rooted_pre_optimization_cfg(
        &mut self,
        options: &CompileOptions,
    ) -> Result<RootedPreOptimizationCfgOutput, CompileErrors> {
        match self.rooted_cfg_artifact_with_cancellation(
            options,
            rue_query::CancellationToken::new(),
            true,
            std::convert::identity,
            |_| unreachable!("a pre-optimization request cannot publish a post artifact"),
        ) {
            Ok(output) => Ok(output),
            Err(PipelineRequestControl::Compile(errors)) => Err(errors),
            Err(PipelineRequestControl::Abort(abort)) => {
                Err(pipeline_abort_errors("pre-optimization rooted CFG", abort))
            }
            Err(PipelineRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
        }
    }

    pub(crate) fn rooted_cfg_with_cancellation(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCfgOutput, PipelineRequestControl> {
        self.rooted_cfg_artifact_with_cancellation(
            options,
            cancellation,
            false,
            |_| unreachable!("a post-optimization request cannot publish a raw artifact"),
            std::convert::identity,
        )
    }

    fn rooted_cfg_artifact_with_cancellation<T>(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
        pre_optimization: bool,
        publish_pre: impl FnOnce(RootedPreOptimizationCfgOutput) -> T,
        publish_post: impl FnOnce(RootedCfgOutput) -> T,
    ) -> Result<T, PipelineRequestControl> {
        let graph = match self.rooted_body_graph_with_cancellation(options, cancellation.clone()) {
            Ok(graph) => graph,
            Err(SemanticRequestControl::Compile(errors)) => {
                return Err(PipelineRequestControl::Compile(errors));
            }
            Err(SemanticRequestControl::Parked(park)) => {
                return Err(PipelineRequestControl::Parked(park));
            }
            Err(SemanticRequestControl::Abort(abort)) => {
                return Err(PipelineRequestControl::Abort(abort));
            }
        };
        let mut work = graph.work;
        let mut identities = graph
            .closure
            .reached
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        identities.extend(
            graph
                .closure
                .demanded_drop_glue
                .iter()
                .cloned()
                .map(|owner| crate::FunctionInstanceKey::DropGlue(Node::new(owner))),
        );
        identities.extend(
            graph
                .closure
                .demanded_error_printers
                .iter()
                .map(crate::error_printer::error_printer_identity),
        );
        // A test image needs an entry point, and only a request that lowers one
        // synthesizes it: `rooted_test_inventory` stops at the body graph, so a
        // listing never builds a dispatcher it would not link (ADR-0083 §2,
        // §3).
        //
        // The table is the inventory verbatim, so ordinal `n` here is the same
        // `n` a listing published — including for a test excluded from the
        // image because its closure failed to analyze (ADR-0083 §3), which
        // holds its ordinal open as `None` rather than renumbering the rest.
        let dispatches_tests = options.root_selection == crate::RootSelection::Tests;
        let excluded_tests = graph.excluded_tests.iter().collect::<BTreeSet<_>>();
        let test_dispatcher_table: Arc<[Option<crate::FunctionInstanceKey>]> = graph
            .test_inventory
            .iter()
            .map(|test| (!excluded_tests.contains(&test.identity)).then(|| test.identity.clone()))
            .collect::<Vec<_>>()
            .into();
        // Whole-program CFG reachability at O2/O3 publishes only what the
        // request's roots reach, so the dispatcher has to be one of them.
        // `graph.roots` is the semantic root set, and it can only name
        // declarations the body closure was analyzed from; the dispatcher has
        // no declaration and is synthesized here. Nothing calls it, so a test
        // image built from the semantic roots alone classified the entry point
        // itself as unreachable and general inlining dropped its unit, leaving
        // the image with an undefined `main` (RUE-1995). It is a root of the
        // request exactly as `main` and the `extern "C"` exports are for an
        // executable one; this is where it becomes nameable, so this is where
        // it joins the root set.
        let mut cfg_roots = graph.roots.to_vec();
        if dispatches_tests {
            identities.insert(crate::FunctionInstanceKey::TestDispatcher);
            cfg_roots.push(crate::FunctionInstanceKey::TestDispatcher);
            cfg_roots.sort();
            cfg_roots.dedup();
        }
        let cfg_roots: Arc<[crate::FunctionInstanceKey]> = cfg_roots.into();
        // Only an executable request has a `main`, and only its symbol is
        // spelled unmangled (ADR-0083 §1: a test request has no entry point).
        let main_identity = graph
            .main
            .clone()
            .map(crate::FunctionInstanceKey::Definition);
        let callable_symbols = identities
            .iter()
            .cloned()
            .map(|identity| {
                let symbol = if Some(&identity) == main_identity.as_ref() {
                    Arc::from("main")
                } else {
                    crate::local_semantic_materialization::rooted_callable_symbol(&identity)
                };
                (identity, symbol)
            })
            // Probed by identity when a body's callable facts are selected,
            // never iterated. An ordered map here charged a recursive
            // `FunctionInstanceKey` comparison per level of every probe.
            .collect::<ahash::AHashMap<_, _>>();
        let mut cfg_inputs = Vec::with_capacity(identities.len());
        let warning_references = self.rooted_warning_references(&graph, cancellation.clone())?;
        let mut warnings = rooted_unused_function_warnings(&graph, &warning_references);
        let _cfg_collection_span =
            tracing::info_span!("optimized_cfg_collection", phase = "cfg_and_optimization")
                .entered();
        let (materialization_index, index_work) =
            crate::local_semantic_materialization::LocalFactSelectionIndex::new(
                &graph.declaration_index,
                &graph.declarations,
                &graph.anonymous_nominals,
            )
            .map_err(|error| {
                CompileError::without_span(ErrorKind::OutputPublication(format!(
                    "CFG materialization index rejected anonymous facts: {error:?}"
                )))
            })?;
        work.cfg.materialization_index_builds += 1;
        work.cfg.materialization_declarations_scanned += index_work.declarations_scanned;
        work.cfg.materialization_anonymous_nominals_scanned +=
            index_work.anonymous_nominals_scanned;
        work.cfg.materialization_type_nodes_scanned += index_work.type_nodes_scanned;
        // One table for this pass, covering both the body loop below and the
        // drop-glue loop after it: drop glue for a type reached from several
        // bodies selects the same closure each time.
        let mut fact_closures =
            crate::local_semantic_materialization::LocalMaterializationFactInterner::default();
        for closure_body in graph.closure.bodies.iter() {
            let rue_query::QueryOutcome::Success(bundle) = closure_body.bundle.outcome() else {
                unreachable!("BodyAnalysisBundle publishes typed values")
            };
            let crate::body_query::BodyTransaction::Success { body, .. } = &bundle.transaction
            else {
                continue;
            };
            let locator = self
                .queries
                .revisioned
                .body_source_basis_projection(
                    graph.revision,
                    closure_body.key.clone(),
                    cancellation.clone(),
                )
                .map_err(PipelineRequestControl::Abort)?;
            let rue_query::QueryOutcome::Success(locator) = locator.outcome() else {
                unreachable!("BodySourceLocator publishes typed values")
            };
            let Some(locator) = locator.as_ref() else {
                return Err(CompileError::without_span(ErrorKind::InternalError(format!(
                    "reached body {:?} has no source locator",
                    closure_body.key.instance
                )))
                .into());
            };
            let body_span = match body.as_ref() {
                crate::body_query::CanonicalBody::Ordinary { .. } => {
                    rue_span::Span::with_file(locator.file_id, locator.body_start, locator.body_end)
                }
                crate::body_query::CanonicalBody::Anonymous { body_anchor, .. } => {
                    rue_span::Span::with_file(
                        locator.file_id,
                        locator.body_start + body_anchor.start,
                        locator.body_start + body_anchor.end,
                    )
                }
                crate::body_query::CanonicalBody::Specialization { .. } => {
                    rue_span::Span::with_file(
                        locator.file_id,
                        locator.declaration_start,
                        locator.declaration_end,
                    )
                }
            };
            let semantic_body = match body.as_ref() {
                crate::body_query::CanonicalBody::Ordinary { body, .. }
                | crate::body_query::CanonicalBody::Anonymous { body, .. }
                | crate::body_query::CanonicalBody::Specialization { body, .. } => body,
            };
            warnings.extend(import_semantic_body_warnings(semantic_body, body_span));
            // Comptime providers participate in body reachability because their
            // results can produce runtime declarations and anonymous nominals,
            // but they have no runtime CFG/codegen terminal of their own.
            if semantic_body.return_type == rue_air::SemanticImportType::ComptimeType {
                work.cfg.comptime_functions_filtered += 1;
                continue;
            }
            work.cfg.functions_considered += 1;
            work.cfg.materialization_fact_selections += 1;
            let materialization =
                crate::local_semantic_materialization::select_materialization_facts(
                    &closure_body.key.instance,
                    semantic_body,
                    &materialization_index,
                    &callable_symbols,
                    &mut fact_closures,
                )
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "CFG materialization fact selection failed: {error:?}"
                        )),
                        body_span,
                    )
                })?;
            work.cfg.materialization_declarations_selected += materialization.declarations.len();
            work.cfg.materialization_anonymous_nominals_selected +=
                materialization.anonymous_nominals.len();
            work.cfg.materialization_callables_selected += materialization.callables.len();
            work.cfg.materialization_nominal_metadata_selected +=
                materialization.nominal_metadata.len();
            work.cfg.materialization_modules_selected += materialization.modules.len();
            work.cfg.materialization_builtin_nominals_selected +=
                materialization.builtin_nominals.len();
            work.cfg.materialization_required_types_selected +=
                materialization.required_types.len();
            cfg_inputs.push((
                closure_body.key.instance.clone(),
                crate::cfg_query::CfgSemanticInput::Body {
                    input: Arc::new(crate::cfg_query::CfgBodyInput {
                        function: closure_body.key.instance.clone(),
                        canonical: body.clone(),
                        body_span,
                        #[cfg(test)]
                        interner_limit: self.cfg_interner_limit,
                        #[cfg(test)]
                        force_failure: self.cfg_accessor_failure && semantic_body.is_accessor,
                    }),
                    materialization: Arc::new(materialization),
                },
                body_span,
            ));
        }
        // Synthesized drop glue has no source of its own, so it borrows a span
        // from a root of the request. An executable request keeps using `main`
        // exactly as before; a test request has no `main` at all (ADR-0083 §1),
        // so it falls back to its first root rather than to `Span::default()`,
        // which would leave every drop-glue diagnostic in a test closure
        // unlocated. `main` is preferred explicitly rather than taken as
        // `roots.first()`: the root set is ordered by definition key, so a
        // c-export can sort ahead of `main` in an executable graph.
        let root_span = |root: &crate::FunctionInstanceKey| {
            cfg_inputs
                .iter()
                .find(|(identity, _, _)| identity == root)
                .map(|(_, _, span)| *span)
        };
        let fallback_span = main_identity
            .as_ref()
            .and_then(root_span)
            .or_else(|| graph.roots.iter().find_map(root_span))
            .unwrap_or_default();
        for (owner, facts) in graph.closure.demanded_drop_glue_plans.iter() {
            work.cfg.drop_glue_functions_synthesized += 1;
            work.cfg.materialization_fact_selections += 1;
            let identity = crate::FunctionInstanceKey::DropGlue(Node::new(owner.clone()));
            let materialization =
                crate::local_semantic_materialization::select_drop_glue_materialization_facts(
                    owner,
                    facts,
                    &materialization_index,
                    &callable_symbols,
                    &mut fact_closures,
                )
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "drop-glue materialization fact selection failed: {error:?}"
                        )),
                        fallback_span,
                    )
                })?;
            work.cfg.materialization_declarations_selected += materialization.declarations.len();
            work.cfg.materialization_anonymous_nominals_selected +=
                materialization.anonymous_nominals.len();
            work.cfg.materialization_callables_selected += materialization.callables.len();
            work.cfg.materialization_nominal_metadata_selected +=
                materialization.nominal_metadata.len();
            work.cfg.materialization_modules_selected += materialization.modules.len();
            work.cfg.materialization_builtin_nominals_selected +=
                materialization.builtin_nominals.len();
            work.cfg.materialization_required_types_selected +=
                materialization.required_types.len();
            cfg_inputs.push((
                identity,
                crate::cfg_query::CfgSemanticInput::DropGlue {
                    owner: owner.clone(),
                    facts: Box::new(facts.clone()),
                    materialization: Arc::new(materialization),
                    body_span: fallback_span,
                },
                fallback_span,
            ));
        }
        for owner in graph.closure.demanded_error_printers.iter() {
            work.cfg.materialization_fact_selections += 1;
            // The plan is a pure function of the error type's declared shape,
            // which the selection index already holds; the printer needs no
            // query family of its own, because it never descends past one level
            // and so never has a fixpoint to reach.
            let facts = crate::error_printer::plan_error_printer(owner, &materialization_index);
            // Fact selection reads a body's types, strings, and callees — never
            // its parameter offsets — so it synthesizes against a uniform slot
            // width and lets CFG evaluation, which has the layout terminals,
            // build the same body with the real ones.
            let body = crate::error_printer::synthesize_error_printer(owner, &facts, &|_| Some(1))
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "error-printer synthesis failed: {error}"
                        )),
                        fallback_span,
                    )
                })?;
            let identity = crate::error_printer::error_printer_identity(owner);
            let materialization =
                crate::local_semantic_materialization::select_materialization_facts(
                    &identity,
                    &body,
                    &materialization_index,
                    &callable_symbols,
                    &mut fact_closures,
                )
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "error-printer materialization fact selection failed: {error:?}"
                        )),
                        fallback_span,
                    )
                })?;
            work.cfg.materialization_declarations_selected += materialization.declarations.len();
            work.cfg.materialization_anonymous_nominals_selected +=
                materialization.anonymous_nominals.len();
            work.cfg.materialization_callables_selected += materialization.callables.len();
            work.cfg.materialization_nominal_metadata_selected +=
                materialization.nominal_metadata.len();
            work.cfg.materialization_modules_selected += materialization.modules.len();
            work.cfg.materialization_builtin_nominals_selected +=
                materialization.builtin_nominals.len();
            work.cfg.materialization_required_types_selected +=
                materialization.required_types.len();
            cfg_inputs.push((
                identity,
                crate::cfg_query::CfgSemanticInput::ErrorPrinter {
                    owner: owner.clone(),
                    facts: Box::new(facts),
                    materialization: Arc::new(materialization),
                    body_span: fallback_span,
                },
                fallback_span,
            ));
        }
        if dispatches_tests {
            work.cfg.materialization_fact_selections += 1;
            // Fact selection walks the same body the CFG evaluator will
            // synthesize, so the callables the dispatcher names are exactly the
            // tests in the table.
            let body = crate::test_dispatcher::synthesize_test_dispatcher(&test_dispatcher_table);
            let materialization =
                crate::local_semantic_materialization::select_materialization_facts(
                    &crate::FunctionInstanceKey::TestDispatcher,
                    &body,
                    &materialization_index,
                    &callable_symbols,
                    &mut fact_closures,
                )
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "test-dispatcher materialization fact selection failed: {error:?}"
                        )),
                        fallback_span,
                    )
                })?;
            work.cfg.materialization_declarations_selected += materialization.declarations.len();
            work.cfg.materialization_anonymous_nominals_selected +=
                materialization.anonymous_nominals.len();
            work.cfg.materialization_callables_selected += materialization.callables.len();
            work.cfg.materialization_nominal_metadata_selected +=
                materialization.nominal_metadata.len();
            work.cfg.materialization_modules_selected += materialization.modules.len();
            work.cfg.materialization_builtin_nominals_selected +=
                materialization.builtin_nominals.len();
            work.cfg.materialization_required_types_selected +=
                materialization.required_types.len();
            cfg_inputs.push((
                crate::FunctionInstanceKey::TestDispatcher,
                crate::cfg_query::CfgSemanticInput::TestDispatcher {
                    table: Arc::clone(&test_dispatcher_table),
                    materialization: Arc::new(materialization),
                    body_span: fallback_span,
                },
                fallback_span,
            ));
        }
        work.cfg.materialization_fact_closures_allocated += fact_closures.allocated;
        work.cfg.materialization_fact_closures_reused += fact_closures.reused;
        // The selected facts now own everything carried by CFG memo keys. Do
        // not retain the request-wide lookup tables across CFG evaluation.
        drop(materialization_index);
        cfg_inputs.sort_by(|left, right| left.0.cmp(&right.0));
        let mut raw_accessor_keys = std::collections::BTreeMap::new();
        for (function, semantic_input, _) in &cfg_inputs {
            raw_accessor_keys.insert(
                function.clone(),
                crate::cfg_query::CfgQueryKey::new(
                    function.clone(),
                    graph.configuration.clone(),
                    semantic_input.clone(),
                ),
            );
        }
        // Both CFG batches — the raw one a pre-optimization request issues and
        // the optimized one every other consumer issues — follow this point.
        rooted_cancellation_probe(RootedCancellationSite::CfgBatch);
        if pre_optimization {
            let raw_requests = cfg_inputs
                .iter()
                .map(|(function, _, body_span)| {
                    (
                        function.clone(),
                        raw_accessor_keys
                            .get(function)
                            .expect("every CFG input has one raw key")
                            .clone(),
                        *body_span,
                    )
                })
                .collect::<Vec<_>>();
            let raw_keys = raw_requests
                .iter()
                .map(|(_, key, _)| key.clone())
                .collect::<Vec<_>>()
                .into();
            #[cfg(test)]
            self.rooted_cfg_executions.clear();
            let (raw_cfg_batch, attempt) =
                self.queries
                    .revisioned
                    .raw_cfg_batch(graph.revision, raw_keys, cancellation);
            let batch_execution = attempt.execution();
            let executions = if batch_execution == rue_query::RequestExecution::Computed {
                let executions = attempt
                    .nested_attempts()
                    .iter()
                    .filter(|attempt| attempt.node().family() == "compiler.cfg")
                    .map(rue_query::NestedQueryAttempt::execution)
                    .collect::<Vec<_>>();
                assert_eq!(executions.len(), raw_requests.len());
                executions
            } else {
                vec![batch_execution; raw_requests.len()]
            };
            let batch_work = |name: &str| {
                attempt
                    .work()
                    .iter()
                    .find_map(|(kind, count)| (kind.as_ref() == name).then_some(*count as usize))
                    .unwrap_or(0)
            };
            work.cfg.prerequisite_stable_types_scanned +=
                batch_work("cfg.prerequisite.stable-types-scanned");
            work.cfg.prerequisite_layout_requests += batch_work("cfg.prerequisite.layout-requests");
            work.cfg.prerequisite_drop_glue_requests +=
                batch_work("cfg.prerequisite.drop-glue-requests");
            work.cfg.retained_interner_charge_scans +=
                batch_work("cfg.retained-interner-charge-scans");
            work.cfg.retained_interner_entries_scanned +=
                batch_work("cfg.retained-interner-entries-scanned");
            work.cfg.retained_interner_utf8_bytes_scanned +=
                batch_work("cfg.retained-interner-utf8-bytes-scanned");
            work.cfg.cfg_builds_attempted += batch_work("cfg.build.attempts");
            work.cfg.cfg_builds_succeeded += batch_work("cfg.build.successes");
            work.cfg.cfg_builds_failed += batch_work("cfg.build.failures");
            work.cfg.air_instructions_consumed += batch_work("cfg.air.instructions");
            work.cfg.cfg_warnings_emitted += batch_work("cfg.warnings");
            let cfg_reuses = executions
                .iter()
                .filter(|execution| {
                    matches!(
                        execution,
                        rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined
                    )
                })
                .count();
            work.cfg.cfg_reuse_candidates += cfg_reuses;
            work.cfg.cfg_reuses += cfg_reuses;

            let batch_terminal = attempt
                .into_result()
                .map_err(PipelineRequestControl::Abort)?;
            let rue_query::QueryOutcome::Success(batch) = batch_terminal.outcome() else {
                unreachable!("RawCfgBatch publishes typed values")
            };
            assert_eq!(batch.values.len(), raw_requests.len());
            let mut cfgs = Vec::with_capacity(raw_requests.len());
            for (((function, cfg_key, body_span), value), _execution) in raw_requests
                .into_iter()
                .zip(batch.values.iter())
                .zip(executions)
            {
                #[cfg(test)]
                self.rooted_cfg_executions
                    .push((function.clone(), _execution));
                let record = match value {
                    crate::cfg_query::CfgValue::Available(record) => record.clone(),
                    crate::cfg_query::CfgValue::Failure {
                        errors,
                        body_span: old_span,
                    } => {
                        return Err(PipelineRequestControl::Compile(
                            crate::cfg_query::import_errors(errors, *old_span, body_span),
                        ));
                    }
                    crate::cfg_query::CfgValue::AccessorFailure { .. } => {
                        unreachable!("raw CFG queries do not publish accessor-splice failures")
                    }
                };
                let local_air_payload = record.air.payload_store_stats();
                work.cfg.local_epochs += 1;
                work.cfg.local_air_instructions += record.air.instructions().len();
                work.cfg.local_air_payload_bytes += local_air_payload
                    .word_store_logical_bytes
                    .saturating_add(local_air_payload.projection_store_logical_bytes)
                    .saturating_add(local_air_payload.place_store_logical_bytes);
                work.cfg.local_type_entries += record.type_pool.len();
                work.cfg.local_aggregate_type_aliases += record.local_aggregate_type_aliases;
                work.cfg.local_materialized_type_handles += record.local_materialized_type_handles;
                work.cfg.local_interner_entries += record.interner.len();
                work.cfg.local_interner_utf8_bytes += record.interner.utf8_bytes();
                work.cfg.local_strings += record.strings.len();
                work.cfg.local_atoms += record.local_atoms.len();
                warnings.extend(crate::cfg_query::import_warnings(
                    &record.materialization_warnings,
                    record.body_span,
                    body_span,
                ));
                warnings.extend(crate::cfg_query::import_warnings(
                    &record.warnings,
                    record.body_span,
                    body_span,
                ));
                cfgs.push(RootedPreOptimizationCfgUnit {
                    function,
                    cfg_key,
                    record,
                });
            }
            drop(_cfg_collection_span);
            cfgs.sort_by(|left, right| left.function.cmp(&right.function));
            sort_rooted_warnings(&graph, &mut warnings);
            let source = self
                .published_snapshot
                .clone()
                .ok_or_else(|| PipelineRequestControl::Compile(no_published_program()))?;
            let input = CodegenInputDescriptor {
                semantic: SemanticInputDescriptor::new(
                    &source,
                    options.target,
                    &options.preview_features,
                ),
                opt_level: options.opt_level.into(),
            };
            let imports = self
                .accepted_semantic_import_graph()
                .map_err(PipelineRequestControl::Compile)?;
            let diagnostics = self.publish_diagnostics(
                &source,
                FrontendDiagnosticIdentity::Semantic(semantic_diagnostic_input(&input, imports)),
                None,
                &warnings,
            );
            self.diagnostics.select_snapshot(&diagnostics);
            self.refresh_retention_metrics();
            return Ok(publish_pre(RootedPreOptimizationCfgOutput {
                cfgs,
                raw_cfg_batch,
                _raw_cfg_terminal: batch_terminal,
                warnings,
                work,
            }));
        }
        let accessor_subgraph = crate::cfg_query::accessor_cfg_subgraph(raw_accessor_keys)
            .map_err(|failure| {
                let (kind, span) = match failure {
                    crate::cfg_query::AccessorCfgSubgraphFailure::Missing(identity) => (
                        ErrorKind::InternalError(format!(
                            "accessor CFG dependency is missing: {identity:?}"
                        )),
                        fallback_span,
                    ),
                    crate::cfg_query::AccessorCfgSubgraphFailure::Cycle(identity) => {
                        let span = cfg_inputs
                            .iter()
                            .find(|(function, _, _)| function == &identity)
                            .map_or(fallback_span, |(_, _, body_span)| *body_span);
                        (
                            ErrorKind::AccessorRecursion {
                                method: crate::cfg_query::accessor_source_name(&identity),
                            },
                            span,
                        )
                    }
                };
                CompileError::new(kind, span)
            })?;
        let mut accessor_roots = accessor_subgraph.roots;
        let mut accessor_dependencies = accessor_subgraph.dependencies;
        let accessor_functions = accessor_subgraph.accessors;
        let cfg_requests = cfg_inputs
            .into_iter()
            .filter(|(function, _, _)| !accessor_functions.contains(function))
            .map(|(function, _, body_span)| {
                let cfg = accessor_roots
                    .remove(&function)
                    .expect("validated accessor subgraph has one root per executable function");
                let optimized_cfg_key = crate::cfg_query::OptimizedCfgQueryKey::new(
                    cfg,
                    options.opt_level,
                    accessor_dependencies
                        .remove(&function)
                        .expect("validated accessor subgraph has dependencies for every root"),
                );
                (function, optimized_cfg_key, body_span)
            })
            .collect::<Vec<_>>();
        let optimized_keys = cfg_requests
            .iter()
            .map(|(_, key, _)| key.clone())
            .collect::<Vec<_>>()
            .into();
        let mut cfgs = Vec::with_capacity(cfg_requests.len());
        let mut backend_root = self.queries.revisioned.begin_backend_root();
        #[cfg(test)]
        self.rooted_cfg_executions.clear();
        let (cfg_batch_key, attempt) = self.queries.revisioned.optimized_cfg_batch(
            graph.revision,
            optimized_keys,
            Arc::clone(&cfg_roots),
            cancellation,
        );
        let batch_execution = attempt.execution();
        let executions = if batch_execution == rue_query::RequestExecution::Computed {
            let executions = attempt
                .nested_attempts()
                .iter()
                .filter(|attempt| attempt.node().family() == "compiler.optimized-cfg")
                .map(rue_query::NestedQueryAttempt::execution)
                .collect::<Vec<_>>();
            assert_eq!(
                executions.len(),
                cfg_requests.len(),
                "an evaluated optimized-CFG batch records one direct child per key"
            );
            executions
        } else {
            vec![batch_execution; cfg_requests.len()]
        };
        let nested_cfg_attempts = attempt
            .nested_attempts()
            .iter()
            .filter(|attempt| attempt.node().family() == "compiler.cfg")
            .collect::<Vec<_>>();
        let nested_cfg_reuses = nested_cfg_attempts
            .iter()
            .filter(|attempt| {
                matches!(
                    attempt.execution(),
                    rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined
                )
            })
            .count();
        let mut backend_work = BackendQueryWork::default();
        for execution in &executions {
            backend_work.observe(*execution);
        }
        let batch_work = |name: &str| {
            attempt
                .work()
                .iter()
                .find_map(|(kind, count)| (kind.as_ref() == name).then_some(*count as usize))
                .unwrap_or(0)
        };
        work.cfg.prerequisite_stable_types_scanned +=
            batch_work("cfg.prerequisite.stable-types-scanned");
        work.cfg.prerequisite_layout_requests += batch_work("cfg.prerequisite.layout-requests");
        work.cfg.prerequisite_drop_glue_requests +=
            batch_work("cfg.prerequisite.drop-glue-requests");
        work.cfg.retained_interner_charge_scans += batch_work("cfg.retained-interner-charge-scans");
        work.cfg.retained_interner_entries_scanned +=
            batch_work("cfg.retained-interner-entries-scanned");
        work.cfg.retained_interner_utf8_bytes_scanned +=
            batch_work("cfg.retained-interner-utf8-bytes-scanned");
        work.cfg.cfg_builds_attempted += batch_work("cfg.build.attempts");
        work.cfg.cfg_builds_succeeded += batch_work("cfg.build.successes");
        work.cfg.cfg_builds_failed += batch_work("cfg.build.failures");
        work.cfg.air_instructions_consumed += batch_work("cfg.air.instructions");
        work.cfg.optimization_attempts += batch_work("cfg.optimize.attempts");
        work.cfg.optimization_completions += batch_work("cfg.optimize.successes");
        work.cfg.optimized_level_attempts += batch_work("cfg.optimize.nonzero-level");
        let optimization_work = |suffix: &str| {
            batch_work(&format!("cfg.optimize.{suffix}"))
                + batch_work(&format!("cfg.reoptimize.{suffix}"))
        };
        work.cfg.optimization_loops_analyzed += optimization_work("loops-analyzed");
        work.cfg.optimization_loops_unrolled += optimization_work("loops-unrolled");
        work.cfg.optimization_budget_refusals += optimization_work("budget-refusals");
        let passes = &mut work.cfg.optimization_passes;
        macro_rules! pass_work {
            ($field:ident, $name:literal) => {
                passes.$field += optimization_work($name);
            };
        }
        pass_work!(constopt_fold_attempts, "constopt.fold-attempts");
        pass_work!(constopt_folded, "constopt.folded");
        pass_work!(constopt_loads_rewritten, "constopt.loads-rewritten");
        pass_work!(peephole_divmods_reduced, "peephole.divmods-reduced");
        pass_work!(peephole_identities_rewired, "peephole.identities-rewired");
        pass_work!(simplify_blocks_scanned, "simplify.blocks-scanned");
        pass_work!(simplify_branches_folded, "simplify.branches-folded");
        pass_work!(simplify_switches_folded, "simplify.switches-folded");
        pass_work!(simplify_edges_threaded, "simplify.edges-threaded");
        pass_work!(simplify_forwarders_resolved, "simplify.forwarders-resolved");
        pass_work!(simplify_blocks_merged, "simplify.blocks-merged");
        pass_work!(dce_instructions_removed, "dce.instructions-removed");
        pass_work!(dce_blocks_removed, "dce.blocks-removed");
        pass_work!(forward_insts_scanned, "forward.insts-scanned");
        pass_work!(forward_loads_single_write, "forward.loads-single-write");
        pass_work!(forward_loads_block_local, "forward.loads-block-local");
        pass_work!(
            forward_rule1_dominance_pairs_checked,
            "forward.rule1-dominance-pairs-checked"
        );
        pass_work!(
            forward_dominator_computations,
            "forward.dominator-computations"
        );
        pass_work!(cse_insts_scanned, "cse.insts-scanned");
        pass_work!(cse_duplicates_replaced, "cse.duplicates-replaced");
        pass_work!(cse_max_table_entries_sum, "cse.max-table-entries");
        pass_work!(cse_dominator_computations, "cse.dominator-computations");
        pass_work!(
            preheader_normalization_forest_computations,
            "preheader-normalization.forest-computations"
        );
        pass_work!(
            preheader_normalization_loops_examined,
            "preheader-normalization.loops-examined"
        );
        pass_work!(
            preheader_normalization_preheaders_materialized,
            "preheader-normalization.preheaders-materialized"
        );
        pass_work!(
            preheader_normalization_verifier_dominator_computations,
            "preheader-normalization.verifier-dominator-computations"
        );
        pass_work!(licm_forest_computations, "licm.forest-computations");
        pass_work!(licm_def_block_scans, "licm.def-block-scans");
        pass_work!(licm_loops_analyzed, "licm.loops-analyzed");
        pass_work!(licm_instructions_examined, "licm.instructions-examined");
        pass_work!(
            licm_slot_fact_instructions_scanned,
            "licm.slot-fact-instructions-scanned"
        );
        pass_work!(
            licm_slot_fact_entries_initialized,
            "licm.slot-fact-entries-initialized"
        );
        pass_work!(
            licm_slot_fact_workspace_growths,
            "licm.slot-fact-workspace-growths"
        );
        pass_work!(licm_use_index_users_visited, "licm.use-index-users-visited");
        pass_work!(licm_use_index_edges_visited, "licm.use-index-edges-visited");
        pass_work!(
            licm_use_index_domain_entries_initialized,
            "licm.use-index-domain-entries-initialized"
        );
        pass_work!(licm_candidate_dependencies, "licm.candidate-dependencies");
        pass_work!(licm_worklist_pops, "licm.worklist-pops");
        pass_work!(licm_invariants_hoisted, "licm.invariants-hoisted");
        pass_work!(licm_hoist_workspace_growths, "licm.hoist-workspace-growths");
        pass_work!(unroll_forest_computations, "unroll.forest-computations");
        pass_work!(unroll_loops_analyzed, "loops-analyzed");
        pass_work!(unroll_loops_unrolled, "loops-unrolled");
        pass_work!(unroll_budget_refusals, "budget-refusals");
        pass_work!(unroll_shape_refusals, "unroll.shape-refusals");
        pass_work!(unroll_blocks_cloned, "unroll.blocks-cloned");
        pass_work!(unroll_values_cloned, "unroll.values-cloned");
        pass_work!(unroll_instructions_cloned, "unroll.instructions-cloned");
        pass_work!(
            publication_verifier_dominator_computations,
            "publication-verifier-dominator-computations"
        );
        passes.accessor_splice_imported_callee_verifier_dominator_computations +=
            batch_work("cfg.accessor-splice.imported-callee-verifier-dominator-computations");
        passes.accessor_splice_preoptimization_verifier_dominator_computations +=
            batch_work("cfg.accessor-splice.preoptimization-verifier-dominator-computations");
        passes.general_inline_splice_imported_callee_verifier_dominator_computations +=
            batch_work("cfg.general-inline.imported-callee-verifier-dominator-computations");
        passes.inline_splice_pre_reoptimization_verifier_dominator_computations +=
            batch_work("cfg.general-inline.pre-reoptimization-verifier-dominator-computations");
        work.cfg.optimization_inline_budget_refusals +=
            batch_work("cfg.general-inline-budget-refusals");
        work.cfg.optimization_inline_importability_refusals +=
            batch_work("cfg.general-inline-importability-refusals");
        work.cfg.optimization_inline_importability_checks +=
            batch_work("cfg.general-inline-importability-checks");
        work.cfg.optimization_inline_import_attempts +=
            batch_work("cfg.general-inline-import-attempts");
        work.cfg.optimization_inline_interner_stages +=
            batch_work("cfg.general-inline-interner-stages");
        work.cfg.optimization_inline_growth_preflights +=
            batch_work("cfg.general-inline-growth-preflights");
        let optimizer_code_growth = batch_work("cfg.optimize.code-growth-used");
        let optimizer_code_growth_blocks = batch_work("cfg.optimize.code-growth-blocks-used");
        let reoptimization_code_growth = batch_work("cfg.reoptimize.code-growth-used");
        let reoptimization_code_growth_blocks =
            batch_work("cfg.reoptimize.code-growth-blocks-used");
        let inline_code_growth = batch_work("cfg.general-inline-code-growth");
        let inline_code_growth_blocks = batch_work("cfg.general-inline-code-growth-blocks");
        work.cfg.optimization_code_growth_used +=
            optimizer_code_growth + inline_code_growth + reoptimization_code_growth;
        work.cfg.optimization_code_growth_blocks_used += optimizer_code_growth_blocks
            + inline_code_growth_blocks
            + reoptimization_code_growth_blocks;
        work.cfg.optimization_inline_code_growth_used += inline_code_growth;
        work.cfg.optimization_inline_code_growth_blocks_used += inline_code_growth_blocks;
        work.cfg.optimization_reoptimization_attempts += batch_work("cfg.reoptimize.attempts");
        work.cfg.optimization_reoptimization_completions +=
            batch_work("cfg.reoptimize.completions");
        work.cfg.optimization_reoptimization_code_growth_used += reoptimization_code_growth;
        work.cfg.optimization_reoptimization_code_growth_blocks_used +=
            reoptimization_code_growth_blocks;
        work.cfg.cfg_warnings_emitted += batch_work("cfg.warnings");
        let optimized_reuses = executions
            .iter()
            .filter(|execution| {
                matches!(
                    execution,
                    rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined
                )
            })
            .count();
        let cfg_reuses = if nested_cfg_attempts.is_empty() {
            optimized_reuses
        } else {
            nested_cfg_reuses
        };
        work.cfg.cfg_reuse_candidates += cfg_reuses;
        work.cfg.cfg_reuses += cfg_reuses;
        if let Some(terminal) = attempt.terminal() {
            self.queries.revisioned.retain_backend_optimized_cfg_batch(
                &mut backend_root,
                &cfg_batch_key,
                terminal,
            );
        }
        let batch = attempt
            .into_result()
            .map_err(PipelineRequestControl::Abort)?;
        let rue_query::QueryOutcome::Success(batch) = batch.outcome() else {
            unreachable!("OptimizedCfgBatch publishes typed values")
        };
        assert_eq!(batch.values.len(), cfg_requests.len());
        let unreachable_functions = batch.unreachable_functions.iter().collect::<BTreeSet<_>>();
        for (((function, optimized_cfg_key, body_span), value), _execution) in cfg_requests
            .into_iter()
            .zip(batch.values.iter())
            .zip(executions)
        {
            #[cfg(test)]
            self.rooted_cfg_executions
                .push((function.clone(), _execution));
            let record = match value {
                crate::cfg_query::CfgValue::Available(record) => record.clone(),
                crate::cfg_query::CfgValue::Failure {
                    errors,
                    body_span: old_span,
                } => {
                    return Err(PipelineRequestControl::Compile(
                        crate::cfg_query::import_errors(errors, *old_span, body_span),
                    ));
                }
                crate::cfg_query::CfgValue::AccessorFailure { errors, origin, .. } => {
                    return Err(PipelineRequestControl::Compile(
                        crate::cfg_query::import_accessor_failure(
                            errors,
                            origin,
                            &optimized_cfg_key,
                        ),
                    ));
                }
            };
            let local_air_payload = record.air.payload_store_stats();
            work.cfg.local_epochs += 1;
            work.cfg.local_air_instructions += record.air.instructions().len();
            work.cfg.local_air_payload_bytes += local_air_payload
                .word_store_logical_bytes
                .saturating_add(local_air_payload.projection_store_logical_bytes)
                .saturating_add(local_air_payload.place_store_logical_bytes);
            work.cfg.local_type_entries += record.type_pool.len();
            work.cfg.local_aggregate_type_aliases += record.local_aggregate_type_aliases;
            work.cfg.local_materialized_type_handles += record.local_materialized_type_handles;
            work.cfg.local_interner_entries += record.interner.len();
            work.cfg.local_interner_utf8_bytes += record.interner.utf8_bytes();
            work.cfg.local_strings += record.strings.len();
            work.cfg.local_atoms += record.local_atoms.len();
            warnings.extend(crate::cfg_query::import_warnings(
                &record.materialization_warnings,
                record.body_span,
                body_span,
            ));
            warnings.extend(crate::cfg_query::import_warnings(
                &record.warnings,
                record.body_span,
                body_span,
            ));
            // Reachability only controls backend publication. Diagnostics and
            // completed-work accounting belong to every successfully queried
            // body, including a callee removed after inlining.
            if unreachable_functions.contains(&function) {
                continue;
            }
            cfgs.push(RootedCfgUnit {
                function,
                optimized_cfg_key,
                record,
                body_span,
            });
        }
        if self.oracle_fault == Some(crate::unstable::DifferentialOracleFault::CfgTransformation) {
            self.oracle_fault.take();
            // The fault corrupts one root's CFG so the differential oracle
            // observes a disagreement. `main` is that root for an executable
            // request; a test request has no `main` (ADR-0083 §1), so the fault
            // lands on the first published unit instead of panicking. An empty
            // closure has nothing to corrupt and reports that as the same
            // "no comparison to corrupt" error the injection failure reports.
            let target = cfgs
                .iter()
                .position(|unit| Some(&unit.function) == main_identity.as_ref())
                .or(if cfgs.is_empty() { None } else { Some(0) })
                .map(|index| &mut cfgs[index]);
            let Some(target) = target else {
                return Err(CompileError::without_span(ErrorKind::InternalError(
                    "differential CFG transformation fault had no equality comparison to corrupt"
                        .into(),
                ))
                .into());
            };
            let record = Arc::make_mut(&mut target.record);
            if !record
                .cfg
                .inject_differential_comparison_fault(&record.type_pool)
            {
                return Err(CompileError::without_span(ErrorKind::InternalError(
                    "differential CFG transformation fault had no equality comparison to corrupt"
                        .into(),
                ))
                .into());
            }
        }
        drop(_cfg_collection_span);

        // Preserve the canonical backend/presentation order independently of
        // the query scheduling order. Function-instance identity is the right
        // key for query work, while machine symbols are the established public
        // ordering for MIR, assembly, and object-image consumers.
        cfgs.sort_by(|left, right| {
            left.record
                .codegen
                .defined_symbol
                .cmp(&right.record.codegen.defined_symbol)
        });

        // Presentation order for warnings is module path, then span, then the
        // rendered text. Building that key costs a module lookup and two
        // rendered strings, so it is decorated once per warning rather than
        // twice per comparison. The first module wins a duplicated file id,
        // matching the linear scan this replaces.
        let mut module_ids: AHashMap<rue_span::FileId, &str> =
            AHashMap::with_capacity(graph.modules.len());
        for module in graph.modules.iter() {
            module_ids
                .entry(module.file_id())
                .or_insert_with(|| module.module_id().as_str());
        }
        let mut keyed = warnings
            .drain(..)
            .map(|warning| {
                let span = warning.span();
                let key = (
                    span.and_then(|span| module_ids.get(&span.file_id).copied())
                        .unwrap_or(""),
                    span.map(|span| span.start).unwrap_or(0),
                    span.map(|span| span.end).unwrap_or(0),
                    warning.to_string(),
                    format!("{:?}", warning.diagnostic()),
                );
                (key, warning)
            })
            .collect::<Vec<_>>();
        keyed.sort_by(|left, right| left.0.cmp(&right.0));
        warnings.extend(keyed.into_iter().map(|(_, warning)| warning));
        warnings.dedup();

        let source = self
            .published_snapshot
            .clone()
            .ok_or_else(|| PipelineRequestControl::Compile(no_published_program()))?;
        let input = CodegenInputDescriptor {
            semantic: SemanticInputDescriptor::new(
                &source,
                options.target,
                &options.preview_features,
            ),
            opt_level: options.opt_level.into(),
        };
        let imports = self
            .accepted_semantic_import_graph()
            .map_err(PipelineRequestControl::Compile)?;
        let diagnostics = self.publish_diagnostics(
            &source,
            FrontendDiagnosticIdentity::Semantic(semantic_diagnostic_input(&input, imports)),
            None,
            &warnings,
        );
        self.diagnostics.select_snapshot(&diagnostics);
        self.refresh_retention_metrics();

        Ok(publish_post(RootedCfgOutput {
            graph,
            cfgs,
            optimized_cfg_batch: cfg_batch_key,
            warnings,
            work,
            backend_work,
            backend_root,
        }))
    }

    /// [`Self::rooted_codegen_with_cancellation`] under a token nobody can
    /// cancel, for tests that want the plain artifact. Production consumers —
    /// the presentation batch and the compile endpoints — carry their
    /// request's token instead (RUE-2174).
    #[cfg(test)]
    pub(crate) fn rooted_codegen(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
    ) -> Result<RootedCodegenOutput, CompileErrors> {
        match self.rooted_codegen_with_cancellation(
            options,
            request,
            rue_query::CancellationToken::new(),
        ) {
            Ok(output) => Ok(output),
            Err(PipelineRequestControl::Compile(errors)) => Err(errors),
            Err(PipelineRequestControl::Abort(abort)) => {
                Err(pipeline_abort_errors("rooted codegen", abort))
            }
            Err(PipelineRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
        }
    }

    pub(crate) fn rooted_codegen_with_cancellation(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCodegenOutput, PipelineRequestControl> {
        let ready =
            self.rooted_codegen_ready_with_cancellation(options, request, cancellation.clone())?;
        self.rooted_objects_ready_with_cancellation(ready, cancellation)
    }

    /// Complete the retained codegen boundary for an ordinary internal link.
    /// ObjectProjectionBatch is intentionally not requested: the linker
    /// consumes the retained CodegenUnits directly. Byte consumers continue
    /// through `rooted_objects_ready_with_cancellation` below.
    pub(crate) fn rooted_codegen_internal_with_cancellation(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCodegenOutput, PipelineRequestControl> {
        let ready =
            self.rooted_codegen_ready_with_cancellation(options, request, cancellation.clone())?;
        let RootedCodegenReadyOutput {
            graph,
            units,
            cfgs,
            warnings,
            work,
            cfg_work,
            codegen_work,
            backend_root,
            codegen_batch_key,
        } = ready;
        let exports = collect_rooted_exports(&graph, &cfgs);
        if cancellation.is_canceled() {
            return Err(PipelineRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        self.queries
            .revisioned
            .publish_backend_root(
                graph.revision,
                backend_root,
                crate::revisioned_query_database::BackendRootPublicationInput::Codegen(
                    codegen_batch_key,
                ),
            )
            .map_err(PipelineRequestControl::Abort)?;
        Ok(RootedCodegenOutput {
            input: RootedCodegenInput::Structured,
            units,
            objects: Vec::new(),
            cfgs,
            exports,
            warnings,
            work,
            cfg_work,
            codegen_work,
            object_projection_work: BackendQueryWork::default(),
        })
    }

    /// Collect the rooted reached set's canonical CodegenUnits while retaining
    /// the exact unpublished backend-root candidate for object projection.
    pub(crate) fn rooted_codegen_ready(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
    ) -> Result<RootedCodegenReadyOutput, CompileErrors> {
        match self.rooted_codegen_ready_with_cancellation(
            options,
            request,
            rue_query::CancellationToken::new(),
        ) {
            Ok(output) => Ok(output),
            Err(PipelineRequestControl::Compile(errors)) => Err(errors),
            Err(PipelineRequestControl::Abort(abort)) => {
                Err(pipeline_abort_errors("codegen-ready", abort))
            }
            Err(PipelineRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
        }
    }

    pub(crate) fn rooted_codegen_ready_with_cancellation(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCodegenReadyOutput, PipelineRequestControl> {
        let RootedCfgOutput {
            graph,
            cfgs,
            optimized_cfg_batch,
            warnings,
            work,
            backend_work: cfg_work,
            mut backend_root,
        } = self.rooted_cfg_with_cancellation(options, cancellation.clone())?;

        let codegen_keys = cfgs
            .iter()
            .map(|cfg| {
                crate::codegen_query::CodegenUnitQueryKey::new_with_batch(
                    cfg.optimized_cfg_key.clone(),
                    options.target,
                    request,
                    options.opt_level,
                    (!cfg.record.durable_reuse_allowed)
                        .then(|| std::sync::Arc::new(optimized_cfg_batch.clone())),
                )
            })
            .collect::<Vec<_>>()
            .into();
        let mut units = Vec::with_capacity(cfgs.len());
        #[cfg(test)]
        {
            self.codegen_executions.clear();
            self.codegen_attempt_work.clear();
            self.codegen_collections = 0;
            self.object_projection_executions.clear();
            self.object_projection_collections = 0;
        }
        let _codegen_collection_span =
            tracing::info_span!("codegen_collection", phase = "backend").entered();
        let (codegen_batch_key, attempt) =
            self.queries
                .revisioned
                .codegen_unit_batch(graph.revision, codegen_keys, cancellation);
        let batch_execution = attempt.execution();
        let child_attempts = if batch_execution == rue_query::RequestExecution::Computed {
            let attempts = attempt
                .nested_attempts()
                .iter()
                .filter(|attempt| attempt.node().family() == "compiler.codegen-unit")
                .map(rue_query::NestedQueryAttempt::execution)
                .collect::<Vec<_>>();
            assert_eq!(
                attempts.len(),
                cfgs.len(),
                "an evaluated CodegenUnit batch records one direct child per key"
            );
            Some(attempts)
        } else {
            None
        };
        #[cfg(test)]
        let child_attempt_work = if batch_execution == rue_query::RequestExecution::Computed {
            Some(
                attempt
                    .nested_attempts()
                    .iter()
                    .filter(|attempt| attempt.node().family() == "compiler.codegen-unit")
                    .map(|attempt| attempt.work().to_vec())
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        let mut codegen_work = BackendQueryWork::default();
        if let Some(terminal) = attempt.terminal() {
            self.queries.revisioned.retain_backend_codegen_batch(
                &mut backend_root,
                &codegen_batch_key,
                terminal,
            );
        }
        let batch = attempt
            .into_result()
            .map_err(PipelineRequestControl::Abort)?;
        let rue_query::QueryOutcome::Success(batch) = batch.outcome() else {
            unreachable!("CodegenUnitBatch publishes typed terminals")
        };
        assert_eq!(batch.values.len(), cfgs.len());
        for (index, (cfg, value)) in cfgs.iter().zip(batch.values.iter()).enumerate() {
            let execution = child_attempts
                .as_ref()
                .map_or(batch_execution, |attempts| attempts[index]);
            codegen_work.observe(execution);
            #[cfg(test)]
            {
                self.codegen_executions
                    .push((cfg.function.clone(), execution));
                self.codegen_attempt_work.push((
                    cfg.function.clone(),
                    child_attempt_work
                        .as_ref()
                        .map_or_else(Vec::new, |attempts| attempts[index].clone()),
                ));
            }
            match value {
                crate::codegen_query::CodegenUnitValue::Available(unit) => {
                    units.push(crate::codegen_query::CollectedCodegenUnit {
                        function: cfg.function.clone(),
                        unit: unit.clone(),
                    });
                    #[cfg(test)]
                    {
                        self.codegen_collections += 1;
                    }
                }
                crate::codegen_query::CodegenUnitValue::Failure(errors) => {
                    return Err(PipelineRequestControl::Compile(errors.clone()));
                }
            }
        }
        drop(_codegen_collection_span);
        Ok(RootedCodegenReadyOutput {
            graph,
            units,
            cfgs,
            warnings,
            work,
            cfg_work,
            codegen_work,
            backend_root,
            codegen_batch_key,
        })
    }

    /// Continue one compiler-issued codegen-ready capability through retained
    /// per-unit object projection and atomically publish the backend root.
    pub(crate) fn rooted_objects_ready(
        &mut self,
        ready: RootedCodegenReadyOutput,
    ) -> Result<RootedCodegenOutput, CompileErrors> {
        match self
            .rooted_objects_ready_with_cancellation(ready, rue_query::CancellationToken::new())
        {
            Ok(output) => Ok(output),
            Err(PipelineRequestControl::Compile(errors)) => Err(errors),
            Err(PipelineRequestControl::Abort(abort)) => {
                Err(pipeline_abort_errors("objects-ready", abort))
            }
            Err(PipelineRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
        }
    }

    pub(crate) fn rooted_objects_ready_with_cancellation(
        &mut self,
        ready: RootedCodegenReadyOutput,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCodegenOutput, PipelineRequestControl> {
        let RootedCodegenReadyOutput {
            graph,
            units,
            cfgs,
            warnings,
            work,
            cfg_work,
            codegen_work,
            mut backend_root,
            codegen_batch_key,
        } = ready;
        let object_keys = codegen_batch_key
            .keys
            .iter()
            .cloned()
            .map(crate::object_query::ObjectProjectionQueryKey::new)
            .collect::<Vec<_>>()
            .into();
        let (object_batch_key, object_attempt) = self.queries.revisioned.object_projection_batch(
            graph.revision,
            object_keys,
            cancellation.clone(),
        );
        let object_batch_execution = object_attempt.execution();
        let object_child_attempts =
            if object_batch_execution == rue_query::RequestExecution::Computed {
                let attempts = object_attempt
                    .nested_attempts()
                    .iter()
                    .filter(|attempt| attempt.node().family() == "compiler.object-projection")
                    .map(|attempt| attempt.execution())
                    .collect::<Vec<_>>();
                assert_eq!(
                    attempts.len(),
                    cfgs.len(),
                    "an evaluated ObjectProjection batch records one direct child per key"
                );
                Some(attempts)
            } else {
                None
            };
        let mut object_projection_work = BackendQueryWork::default();
        if let Some(terminal) = object_attempt.terminal() {
            self.queries
                .revisioned
                .retain_backend_object_projection_batch(
                    &mut backend_root,
                    &object_batch_key,
                    terminal,
                );
        }
        let object_batch = object_attempt
            .into_result()
            .map_err(PipelineRequestControl::Abort)?;
        let rue_query::QueryOutcome::Success(object_batch) = object_batch.outcome() else {
            unreachable!("ObjectProjectionBatch publishes typed terminals")
        };
        assert_eq!(object_batch.values.len(), units.len());
        let mut objects = Vec::with_capacity(units.len());
        for (index, (collected, value)) in units.iter().zip(object_batch.values.iter()).enumerate()
        {
            let execution = object_child_attempts
                .as_ref()
                .map_or(object_batch_execution, |attempts| attempts[index]);
            object_projection_work.observe(execution);
            #[cfg(test)]
            self.object_projection_executions
                .push((collected.function.clone(), execution));
            match value {
                crate::object_query::ObjectProjectionValue::Available(object) => {
                    objects.push(crate::object_query::CollectedObjectProjection {
                        function: collected.function.clone(),
                        unit: collected.unit.clone(),
                        object: object.clone(),
                    });
                    #[cfg(test)]
                    {
                        self.object_projection_collections += 1;
                    }
                }
                crate::object_query::ObjectProjectionValue::Failure(errors) => {
                    return Err(PipelineRequestControl::Compile(errors.clone()));
                }
            }
        }
        let exports = collect_rooted_exports(&graph, &cfgs);
        if cancellation.is_canceled() {
            return Err(PipelineRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        self.queries
            .revisioned
            .publish_backend_root(
                graph.revision,
                backend_root,
                crate::revisioned_query_database::BackendRootPublicationInput::Objects(
                    object_batch_key,
                ),
            )
            .map_err(PipelineRequestControl::Abort)?;
        Ok(RootedCodegenOutput {
            input: RootedCodegenInput::Projected,
            units,
            objects,
            cfgs,
            exports,
            warnings,
            work,
            cfg_work,
            codegen_work,
            object_projection_work,
        })
    }

    /// Collect reached canonical codegen terminals for tests which inspect the
    /// pre-object boundary. Production object and link consumers use
    /// `rooted_codegen`'s query-native image root; this adapter enumerates the
    /// semantic functions only so focused tests can inspect units without
    /// constructing a `ProgramImage`.
    #[cfg(test)]
    pub(crate) fn codegen_units(
        &mut self,
        semantic: &RootedCfgOutput,
        options: &crate::CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
    ) -> Result<Vec<crate::codegen_query::CollectedCodegenUnit>, crate::CompileErrors> {
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .ok_or_else(|| {
                crate::CompileErrors::from(crate::CompileError::without_span(
                    crate::ErrorKind::InvalidCompilerInput(
                        "code generation requires a published semantic revision".into(),
                    ),
                ))
            })?;
        let mut units = Vec::with_capacity(semantic.cfgs.len());
        #[cfg(test)]
        {
            self.codegen_executions.clear();
            self.codegen_attempt_work.clear();
            self.codegen_collections = 0;
        }
        for function in &semantic.cfgs {
            let attempt = self
                .queries
                .revisioned
                .codegen_unit(
                    revision,
                    function.optimized_cfg_key.clone(),
                    options.target,
                    request,
                    options.opt_level,
                    rue_query::CancellationToken::new(),
                )
                .map_err(|abort| {
                    crate::CompileErrors::from(crate::CompileError::without_span(
                        crate::ErrorKind::InternalError(crate::session::abort_internal_message(
                            "codegen", &abort,
                        )),
                    ))
                })?;
            #[cfg(test)]
            {
                self.codegen_executions
                    .push((function.function.clone(), attempt.execution()));
                self.codegen_attempt_work
                    .push((function.function.clone(), attempt.work().to_vec()));
            }
            let terminal = attempt.into_result().map_err(|abort| {
                crate::CompileErrors::from(crate::CompileError::without_span(
                    crate::ErrorKind::InternalError(crate::session::abort_internal_message(
                        "codegen", &abort,
                    )),
                ))
            })?;
            let rue_query::QueryOutcome::Success(unit) = terminal.outcome() else {
                unreachable!("CodegenUnit publishes typed terminals")
            };
            match unit {
                crate::codegen_query::CodegenUnitValue::Available(unit) => {
                    units.push(crate::codegen_query::CollectedCodegenUnit {
                        function: function.function.clone(),
                        unit: unit.clone(),
                    });
                    #[cfg(test)]
                    {
                        self.codegen_collections += 1;
                    }
                }
                crate::codegen_query::CodegenUnitValue::Failure(errors) => {
                    return Err(errors.clone());
                }
            }
        }
        Ok(units)
    }

    #[cfg(test)]
    pub(crate) fn codegen_executions(
        &self,
    ) -> &[(crate::FunctionInstanceKey, rue_query::RequestExecution)] {
        &self.codegen_executions
    }

    #[cfg(test)]
    pub(crate) fn rooted_cfg_executions(
        &self,
    ) -> &[(crate::FunctionInstanceKey, rue_query::RequestExecution)] {
        &self.rooted_cfg_executions
    }

    #[cfg(test)]
    pub(crate) fn warning_reference_executions(
        &self,
    ) -> &[(crate::StableDefinitionKey, rue_query::RequestExecution)] {
        &self.warning_reference_executions
    }

    #[cfg(test)]
    pub(crate) fn codegen_attempt_work(
        &self,
    ) -> &[(crate::FunctionInstanceKey, Vec<(std::sync::Arc<str>, u64)>)] {
        &self.codegen_attempt_work
    }

    #[cfg(test)]
    pub(crate) fn codegen_collections(&self) -> usize {
        self.codegen_collections
    }

    #[cfg(test)]
    pub(crate) fn object_projection_executions(
        &self,
    ) -> &[(crate::FunctionInstanceKey, rue_query::RequestExecution)] {
        &self.object_projection_executions
    }

    #[cfg(test)]
    pub(crate) fn object_projection_collections(&self) -> usize {
        self.object_projection_collections
    }

    #[cfg(test)]
    pub(crate) fn backend_root_metrics(
        &self,
    ) -> crate::revisioned_query_database::PublishedBackendRootMetrics {
        self.queries.revisioned.backend_root_metrics_for_test()
    }

    #[cfg(test)]
    pub(crate) fn raw_cfg_handoff_is_published(
        &self,
        output: &RootedPreOptimizationCfgOutput,
    ) -> bool {
        self.queries
            .revisioned
            .raw_cfg_handoff_matches_terminal_for_test(&output._raw_cfg_terminal)
    }

    #[cfg(test)]
    pub(crate) fn backend_cfg_key_is_retained(&self, key: &crate::cfg_query::CfgQueryKey) -> bool {
        self.queries
            .revisioned
            .backend_cfg_key_is_retained_for_test(key)
    }

    #[cfg(test)]
    pub(crate) fn raw_cfg_record_for_test(
        &self,
        key: crate::cfg_query::CfgQueryKey,
    ) -> Arc<crate::cfg_query::CfgRecord> {
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("raw CFG inspection requires a published semantic revision");
        let terminal = self
            .queries
            .revisioned
            .cfg(revision, key, rue_query::CancellationToken::new())
            .into_result()
            .expect("retained raw CFG request must not abort");
        let rue_query::QueryOutcome::Success(crate::cfg_query::CfgValue::Available(record)) =
            terminal.outcome()
        else {
            panic!("raw CFG inspection requires a successful record")
        };
        record.clone()
    }

    #[cfg(test)]
    pub(crate) fn object_projection_key_is_retained(
        &self,
        key: &crate::object_query::ObjectProjectionQueryKey,
    ) -> bool {
        self.queries
            .revisioned
            .object_projection_key_is_retained_for_test(key)
    }

    #[cfg(test)]
    pub(crate) fn query_evictions_for_test(&self) -> u64 {
        self.queries.revisioned.query_evictions_for_test()
    }

    /// [`Self::rooted_or_toolchain_park_with_cancellation`] under a token
    /// nobody can cancel, for tests of the park itself. The production
    /// acquisition loop always carries its request's token (RUE-2174).
    #[cfg(test)]
    pub(crate) fn rooted_or_toolchain_park(
        &mut self,
        options: &CompileOptions,
    ) -> RootedParkOutcome {
        match self.rooted_or_toolchain_park_with_cancellation(
            options,
            rue_query::CancellationToken::new(),
        ) {
            // The token this passed can never be canceled: nobody else holds it.
            RootedParkOutcome::Canceled => {
                unreachable!("an uncancelled toolchain probe cannot report cancellation")
            }
            outcome => outcome,
        }
    }

    /// [`Self::rooted_or_toolchain_park`] under a caller's cancellation token
    /// (RUE-2174): the host's acquisition loop hands the admitted request's
    /// token to the semantic probe itself, so a canceled request stops inside
    /// a long body-closure analysis rather than only between acquisition
    /// rounds. A canceled probe attaches no park and settles nothing.
    pub(crate) fn rooted_or_toolchain_park_with_cancellation(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> RootedParkOutcome {
        match self.rooted_body_graph_with_cancellation(options, cancellation) {
            Ok(_) => RootedParkOutcome::Ready,
            Err(SemanticRequestControl::Compile(errors)) => RootedParkOutcome::Errors(errors),
            Err(SemanticRequestControl::Parked(park)) => {
                self.attach_toolchain_park(&park);
                RootedParkOutcome::Parked(park)
            }
            Err(SemanticRequestControl::Abort(rue_query::QueryAbort::Canceled)) => {
                RootedParkOutcome::Canceled
            }
            Err(SemanticRequestControl::Abort(abort)) => {
                panic!("uncanceled rooted body-closure request aborted: {abort:?}")
            }
        }
    }
    /// Return the producer request that owns each currently retained ordinary
    /// body terminal named by `names`. A missing declaration or a declaration
    /// with no retained reached-body terminal is omitted.
    ///
    /// The scaling harness compares these stable provenance identities across
    /// revisions to prove the exact recomputed body set. Equal work counts alone
    /// cannot distinguish recomputing the intended consumers from recomputing
    /// the same number of unrelated bodies.
    #[cfg(test)]
    pub(crate) fn retained_body_transaction_origins_for_test(
        &self,
        names: &[String],
    ) -> BTreeMap<String, u64> {
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("the acceptance corpus has a semantic revision");
        self.queries
            .revisioned
            .retained_body_transaction_origins_for_test(revision, names)
    }

    /// Snapshot every retained body identity and its current observable
    /// transaction for the correctness oracle. The map includes stale cache
    /// identities with `None` when invalidation has made their terminal
    /// unobservable at the current revision.
    #[allow(dead_code)]
    pub(crate) fn retained_body_identity_states_for_test(
        &self,
        options: &CompileOptions,
    ) -> BTreeMap<String, Option<crate::BodyTransaction>> {
        let Some(revision) = self.queries.revisioned.current_semantic_revision() else {
            return BTreeMap::new();
        };
        self.queries
            .revisioned
            .retained_body_identity_states_for_test(
                revision,
                crate::semantic_query_nucleus::SemanticQueryConfiguration {
                    target: options.target,
                    preview_features: StablePreviewFeatures::new(&options.preview_features),
                },
            )
    }

    /// Analyze a test request, tolerating closures that fail (ADR-0083 §3).
    ///
    /// Each test item is its own root, so a body that fails to analyze rejects
    /// exactly the tests whose closures reach it. This walks the closure's own
    /// call relation backwards from each failed body to find them. Anything
    /// that belongs to no single body — a closure fatal, an anonymous-fact
    /// conflict, a failed import — is outside every test closure and still
    /// fails the whole run, and so is a failed body no test root reaches, which
    /// would mean the attribution missed an edge rather than that the failure
    /// belongs to nobody.
    pub(crate) fn rooted_test_closure_analysis(
        &mut self,
        options: &CompileOptions,
    ) -> Result<TestClosureAnalysis, CompileErrors> {
        match self.cancellable_test_closure_analysis(options, rue_query::CancellationToken::new()) {
            TestClosureAnalysisOutcome::Analyzed(analysis) => Ok(analysis),
            TestClosureAnalysisOutcome::Errors(errors) => Err(errors),
            // The token this passed can never be canceled: nobody else holds it.
            TestClosureAnalysisOutcome::Canceled => {
                unreachable!("an uncancelled test analysis cannot report cancellation")
            }
        }
    }

    /// [`Self::rooted_test_closure_analysis`] under a caller's cancellation
    /// token, for the retained watch host's test cycle (RUE-2023).
    ///
    /// Cancellation is an outcome rather than an error because it is not a
    /// fact about the program: the source revision this analyzed has been
    /// replaced, so its diagnostics describe bytes the user no longer has.
    pub(crate) fn cancellable_test_closure_analysis(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> TestClosureAnalysisOutcome {
        let attempt = match self.rooted_body_graph_attempt(options, cancellation) {
            Ok(attempt) => attempt,
            Err(SemanticRequestControl::Abort(rue_query::QueryAbort::Canceled)) => {
                return TestClosureAnalysisOutcome::Canceled;
            }
            Err(control) => {
                return TestClosureAnalysisOutcome::Errors(semantic_control_errors(
                    "rooted test closure analysis",
                    control,
                ));
            }
        };
        let rejection = match attempt {
            RootedBodyGraphAttempt::Graph(graph) => {
                return TestClosureAnalysisOutcome::Analyzed(TestClosureAnalysis {
                    inventory: Arc::clone(&graph.test_inventory),
                    failed: Vec::new(),
                    diagnostics: CompileErrors::default(),
                });
            }
            RootedBodyGraphAttempt::Rejected(rejection) => rejection,
        };
        if rejection.global {
            return TestClosureAnalysisOutcome::Errors(rejection.into_errors());
        }
        let per_body = rejection.per_body();
        let edges = rejection.call_edges();
        let mut callers: BTreeMap<&crate::FunctionInstanceKey, Vec<&crate::FunctionInstanceKey>> =
            BTreeMap::new();
        for (caller, callees) in &edges {
            for callee in callees {
                callers.entry(callee).or_default().push(caller);
            }
        }
        let roots: BTreeMap<&crate::FunctionInstanceKey, &crate::test_inventory::RootedTest> =
            rejection
                .test_inventory
                .iter()
                .map(|test| (&test.identity, test))
                .collect();
        // One backward walk per failed body rather than a forward walk per
        // test: the failures are few and the roots are not.
        let mut failed_tests: BTreeMap<u32, Vec<CompileError>> = BTreeMap::new();
        for (failed_body, errors) in &per_body {
            let mut seen = BTreeSet::new();
            let mut frontier = vec![failed_body];
            let mut reaching = BTreeSet::new();
            while let Some(instance) = frontier.pop() {
                if !seen.insert(instance) {
                    continue;
                }
                if let Some(test) = roots.get(instance) {
                    reaching.insert(test.entry.ordinal);
                }
                if let Some(callers) = callers.get(instance) {
                    frontier.extend(callers.iter().copied());
                }
            }
            if reaching.is_empty() {
                // A diagnostic inside no test's closure is one this walk failed
                // to attribute, not one nobody owns: the closure was built from
                // the test roots, so every body in it is reachable from one.
                // Reporting the whole run is the honest answer.
                return TestClosureAnalysisOutcome::Errors(rejection.into_errors());
            }
            for ordinal in reaching {
                failed_tests
                    .entry(ordinal)
                    .or_default()
                    .extend(errors.iter().cloned());
            }
        }
        let by_ordinal: BTreeMap<u32, &crate::test_inventory::RootedTest> = rejection
            .test_inventory
            .iter()
            .map(|test| (test.entry.ordinal, test))
            .collect();
        let failed = failed_tests
            .into_iter()
            .map(|(ordinal, errors)| FailedTestClosure {
                test: (*by_ordinal
                    .get(&ordinal)
                    .expect("an attributed ordinal is an inventory ordinal"))
                .clone(),
                errors: errors.into(),
            })
            .collect();
        TestClosureAnalysisOutcome::Analyzed(TestClosureAnalysis {
            inventory: Arc::clone(&rejection.test_inventory),
            // The union, once each: a helper several tests reach fails each of
            // them, and stderr must still show its diagnostic one time.
            diagnostics: rejection.body_diagnostics(),
            failed,
        })
    }

    /// Run `request` with these test roots excluded from the analyzed root set.
    ///
    /// The exclusion is request state rather than a caller-supplied option
    /// because no caller can know it: it is derived from a first analysis of
    /// this very request. It reaches every memo key that depends on it — the
    /// body closure is keyed by its roots, and the dispatcher's CFG by its
    /// table — so nothing downstream can select an artifact built from the
    /// fuller set.
    pub(crate) fn with_excluded_test_roots<T>(
        &mut self,
        excluded: Arc<[crate::FunctionInstanceKey]>,
        request: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let restored = std::mem::replace(&mut self.excluded_test_roots, excluded);
        let outcome = request(self);
        self.excluded_test_roots = restored;
        outcome
    }
}

#[cfg(test)]
fn stable_type_definition_root(
    value: &crate::TypeInstanceKey,
) -> Option<&crate::StableDefinitionKey> {
    use crate::{NominalInstanceKey as N, TypeInstanceKey as T};
    match value {
        T::Nominal(N::Named(value)) => Some(value),
        T::Nominal(N::Anonymous(value)) => stable_producer_definition_root(&value.producer),
        T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
            stable_type_definition_root(element)
        }
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn stable_function_definition_root(
    value: &crate::FunctionInstanceKey,
) -> Option<&crate::StableDefinitionKey> {
    use crate::FunctionInstanceKey as F;
    match crate::semantic_identity::function_specialization_base(value) {
        F::Definition(value) => Some(value),
        F::AnonymousMember { owner, .. } | F::DropGlue(owner) | F::ErrorPrinter(owner) => {
            stable_type_definition_root(owner)
        }
        F::Specialization { .. } | F::TestDispatcher => None,
    }
}

#[cfg(test)]
pub(super) fn stable_producer_definition_root(
    producer: &crate::StableProducerId,
) -> Option<&crate::StableDefinitionKey> {
    match producer {
        crate::StableProducerId::Definition(definition) => Some(definition),
        crate::StableProducerId::Function(function) => stable_function_definition_root(function),
    }
}

/// Whether this diagnostic is an element-type gate rejection, i.e. one a
/// container states about the element type its caller chose.
///
/// Only the trivially-droppable gate reaches a runtime body: `@require_droppable`
/// is consumed while reducing a `-> type` constructor and is already labelled
/// with its type-constructor application there.
fn is_element_gate_diagnostic(kind: &ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::ContainerElementNotTriviallyDroppable { .. }
    )
}

/// Re-anchor every element-gate diagnostic in `errors` at the call that
/// demanded the rejected body, keeping the gate's own line as a labelled
/// secondary span. Everything else in the batch is passed through untouched.
fn anchor_element_gate_diagnostics(
    errors: CompileErrors,
    demand: Option<rue_span::Span>,
) -> CompileErrors {
    let Some(demand) = demand else {
        return errors;
    };
    let mut anchored = CompileErrors::new();
    for error in errors.into_iter() {
        if !is_element_gate_diagnostic(&error.kind) {
            anchored.push(error);
            continue;
        }
        let Some(gate) = error.span() else {
            anchored.push(error);
            continue;
        };
        anchored.push(error.with_primary_span(demand).with_label(
            "the element type is required to be trivially droppable here",
            gate,
        ));
    }
    anchored
}

/// The earliest call in `body`, by instruction anchor, that names `callee`.
///
/// A specialized call records a canonical specialization identity rather than a
/// function instance key; it is compared through the one conversion the
/// materialization path already uses, so a body calling two specializations of
/// one generic cannot be credited with the wrong one.
fn earliest_call_anchor(
    body: &rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
    callee: &crate::FunctionInstanceKey,
) -> Option<rue_air::SemanticBodyAnchor> {
    body.instructions
        .iter()
        .filter(|inst| match &inst.data {
            rue_air::SemanticBodyInstData::Call { function, .. }
            | rue_air::SemanticBodyInstData::AccessorCall { function, .. } => function == callee,
            rue_air::SemanticBodyInstData::CallSpecialized { identity, .. } => {
                crate::semantic_identity::function_instance_from_specialization(identity)
                    .is_some_and(|instance| instance == *callee)
            }
            _ => false,
        })
        .map(|inst| inst.anchor)
        .min_by_key(|anchor| anchor.start)
}

fn import_semantic_body_warnings(
    body: &rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
    body_span: rue_span::Span,
) -> Vec<CompileWarning> {
    let locate = |anchor: &rue_air::SemanticBodyAnchor| {
        rue_span::Span::with_file(
            body_span.file_id,
            body_span.start + anchor.start,
            body_span.start + anchor.end,
        )
    };
    body.warnings
        .iter()
        .map(|warning| {
            let mut imported = CompileWarning::new(warning.kind.clone(), locate(&warning.anchor));
            for label in warning.labels.iter() {
                imported = imported.with_label(label.message.to_string(), locate(&label.anchor));
            }
            for note in warning.notes.iter() {
                imported = imported.with_note(note.to_string());
            }
            for help in warning.helps.iter() {
                imported = imported.with_help(help.to_string());
            }
            for suggestion in warning.suggestions.iter() {
                imported = imported.with_suggestion(
                    rue_error::Suggestion::new(
                        suggestion.message.to_string(),
                        locate(&suggestion.anchor),
                        suggestion.replacement.to_string(),
                    )
                    .with_applicability(suggestion.applicability),
                );
            }
            imported
        })
        .collect()
}

fn rooted_unused_function_warnings(
    graph: &RootedBodyGraph,
    warning_references: &BTreeSet<crate::StableDefinitionKey>,
) -> Vec<CompileWarning> {
    let mut referenced = graph
        .declaration_dependencies
        .iter()
        .filter_map(|dependency| {
            match &dependency.target {
            crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(key)
            | crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::TypeCallHead(
                key,
            )
            | crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                key,
            ) => Some(key.clone()),
            crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::BuiltinTypeCallHead(
                _,
            ) => None,
        }
        })
        .collect::<BTreeSet<_>>();
    referenced.extend(
        graph
            .closure
            .reached
            .iter()
            .filter_map(crate::semantic_identity::function_base_definition)
            .cloned(),
    );
    referenced.extend(warning_references.iter().cloned());

    /// One module's warning-collection lookups: the module itself, plus the
    /// span index of its function items, built on first use so a program whose
    /// candidates touch one module never indexes the rest.
    struct CandidateModule<'a> {
        module: &'a crate::parsed_modules::ParsedModule,
        functions: Option<AHashMap<rue_span::Span, &'a rue_parser::ast::Function>>,
    }

    // Candidate declarations are keyed by module and located by declaration
    // span, so both lookups are indexed rather than scanned per candidate. The
    // first module and the first item win a duplicated key, matching the linear
    // scans these replace.
    let mut modules: AHashMap<&crate::ModuleId, CandidateModule<'_>> =
        AHashMap::with_capacity(graph.modules.len());
    for module in graph.modules.iter() {
        modules
            .entry(module.module_id())
            .or_insert_with(|| CandidateModule {
                module,
                functions: None,
            });
    }

    let mut warnings = Vec::new();
    for declaration in graph.declarations.iter() {
        let name = declaration.key.name();
        if declaration.key.kind() != crate::StableDefinitionKind::Function
            || name == "main"
            || declaration.key.module().is_trusted_standard_library()
            || declaration.is_public
            || name.starts_with('_')
            || referenced.contains(&declaration.key)
        {
            continue;
        }
        let Some(entry) = modules.get_mut(declaration.key.module()) else {
            continue;
        };
        let module = entry.module;
        let candidate = crate::declaration_candidate::DeclarationCandidateKey {
            module: declaration.key.module().clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name: Arc::from(name),
            owner: None,
            duplicate_discriminator: 0,
        };
        let Some(locator) = module.definitions().declaration_locator(&candidate) else {
            continue;
        };
        let functions = entry.functions.get_or_insert_with(|| {
            let items = &module.ast().items;
            let mut spans = AHashMap::with_capacity(items.len());
            for item in items.iter() {
                if let rue_parser::ast::Item::Function(function) = item {
                    spans.entry(function.span).or_insert(function);
                }
            }
            spans
        });
        let Some(function) = functions.get(&locator.declaration_span).copied() else {
            continue;
        };
        let allows_unused = rue_parser::ast::directives_allow(
            &function.directives,
            rue_parser::WarningName::UnusedFunction,
        );
        if allows_unused {
            continue;
        }
        warnings.push(
            CompileWarning::new(
                rue_error::WarningKind::UnusedFunction(name.to_owned()),
                locator.declaration_span,
            )
            .with_help(format!(
                "if this is intentional, prefix it with an underscore: `_{name}`"
            )),
        );
    }
    warnings
}

/// The root module's `main` declaration for an executable request, validated
/// against the entry signature (spec 6.1:8).
///
/// A `RootSelection::Tests` request never calls this: a test request has no
/// entry point, and requiring one would make a test-only module unanalyzable
/// (ADR-0083 §1).
fn executable_main_declaration(
    program: &crate::parsed_modules::ParsedProgram,
    projection: &crate::revisioned_query_database::SemanticNucleusProjection,
) -> Result<crate::StableDefinitionKey, SemanticRequestControl> {
    let Some(main_declaration) = projection.declarations.iter().find(|declaration| {
        declaration.key.kind() == crate::StableDefinitionKind::Function
            && declaration.key.name() == "main"
            && declaration.key.module() == program.root()
    }) else {
        return Err(SemanticRequestControl::Compile(
            CompileError::without_span(ErrorKind::NoMainFunction).into(),
        ));
    };
    let crate::durable_semantics::DurableDeclarationPayload::Callable {
        parameters, result, ..
    } = &main_declaration.payload
    else {
        return Err(SemanticRequestControl::Compile(
            CompileError::without_span(ErrorKind::NoMainFunction).into(),
        ));
    };
    let invalid_main = if !parameters.is_empty() {
        Some("`main` must not declare parameters")
    } else if !matches!(
        result,
        crate::durable_semantics::DurableType::I32 | crate::durable_semantics::DurableType::Unit
    ) {
        Some("`main` must return `i32` or `()`")
    } else {
        None
    };
    if let Some(reason) = invalid_main {
        let span = program.module(program.root()).and_then(|module| {
            module.ast().items.iter().find_map(|item| match item {
                rue_parser::ast::Item::Function(function)
                    if module.resolve_raw_symbol(function.name.name) == "main" =>
                {
                    Some(function.span)
                }
                _ => None,
            })
        });
        let kind = ErrorKind::InvalidMainSignature { reason };
        return Err(SemanticRequestControl::Compile(
            match span {
                Some(span) => CompileError::new(kind, span),
                None => CompileError::without_span(kind),
            }
            .into(),
        ));
    }
    Ok(main_declaration.key.clone())
}

/// Where in a declaration's signature a query-owned failure belongs.
///
/// Two stable failure shapes reach this: a rule about the parameter itself
/// (a duplicate name) anchors at the whole parameter, and a diagnostic about
/// one written type anchors at that type (RUE-2161).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignatureAnchor {
    Parameter(u32),
    ParameterType(u32),
    ResultType,
}

/// The signature position a failure names, when it names one.
fn signature_anchor(
    failure: &crate::semantic_query_nucleus::SemanticNucleusFailure,
) -> Option<(&rue_error::ErrorKind, SignatureAnchor)> {
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as F, SignatureTypeAnchor as Written,
    };
    match failure {
        F::DiagnosticAtParameter { kind, ordinal } => {
            Some((kind, SignatureAnchor::Parameter(*ordinal)))
        }
        F::DiagnosticAtSignatureType { kind, anchor } => Some((
            kind,
            match anchor {
                Written::Parameter(ordinal) => SignatureAnchor::ParameterType(*ordinal),
                Written::Result => SignatureAnchor::ResultType,
            },
        )),
        _ => None,
    }
}

fn semantic_nucleus_failure_diagnostics(
    modules: &[Arc<crate::parsed_modules::ParsedModule>],
    declaration: Option<&crate::declaration_candidate::DeclarationCandidateKey>,
    failure: &crate::semantic_query_nucleus::SemanticNucleusFailure,
) -> CompileErrors {
    use crate::semantic_query_nucleus::SemanticNucleusFailure as F;
    if let F::DuplicateDeclarations(failures) = failure {
        let mut diagnostics = CompileErrors::new();
        for failure in failures.iter() {
            diagnostics.extend(semantic_nucleus_failure_diagnostics(
                modules,
                None,
                &F::DuplicateDeclaration {
                    kind: failure.kind.clone(),
                    first: failure.first.clone(),
                    duplicate: failure.duplicate.clone(),
                },
            ));
        }
        return diagnostics;
    }
    if let F::ForeignSignatureConflict(conflict) = failure {
        let locate = |declaration: &crate::declaration_candidate::DeclarationCandidateKey| {
            modules
                .iter()
                .find(|module| module.module_id() == &declaration.module)
                .and_then(|module| module.definitions().declaration_locator(declaration))
                .map(|locator| locator.declaration_span)
        };
        if let (Some(left_span), Some(right_span)) = (
            locate(&conflict.left.declaration),
            locate(&conflict.right.declaration),
        ) {
            let left = (left_span, &conflict.left);
            let right = (right_span, &conflict.right);
            let order = |(span, _): &(rue_span::Span, _)| (span.file_id.index(), span.start);
            let (first, second) = if order(&left) <= order(&right) {
                (left, right)
            } else {
                (right, left)
            };
            let spelled_alike = first.1.signature == second.1.signature;
            let mut error = CompileError::new(
                ErrorKind::ForeignSignatureConflict(Box::new(
                    rue_error::ForeignSignatureConflictError {
                        symbol: conflict.symbol.to_string(),
                        declared: second.1.signature.to_string(),
                        previously_declared: first.1.signature.to_string(),
                    },
                )),
                second.0,
            )
            .with_label("conflicting declaration of the same C symbol", second.0)
            .with_label("first declared here", first.0)
            .with_note(
                "an `extern \"C\"` declaration names an external C symbol, so every module that \
                 declares it describes the same function; only one definition is linked in",
            );
            if spelled_alike {
                error = error.with_note(
                    "the two signatures are spelled alike but resolve to different types: a struct \
                     or enum declared in each module is a distinct type, even under the same name",
                );
            }
            return CompileErrors::from(error.with_help(
                "make the declarations identical, or declare the symbol once and import that module",
            ));
        }
        return CompileErrors::from(CompileError::without_span(ErrorKind::InternalError(
            format!(
                "query-owned foreign-signature conflict could not be projected to source: {failure:?}"
            ),
        )));
    }
    if let Some((kind, anchor)) = signature_anchor(failure)
        && let Some(declaration) = declaration
        && let Some(module) = modules
            .iter()
            .find(|module| module.module_id() == &declaration.module)
        && let Some(locator) = module.definitions().declaration_locator(declaration)
    {
        // One AST lookup answers both anchors: the callable declared at this
        // locator, whatever item shape carries it.
        let signature = module.ast().items.iter().find_map(|item| match item {
            rue_parser::ast::Item::Function(function)
                if function.span == locator.declaration_span =>
            {
                Some((function.params.as_slice(), function.return_type.as_ref()))
            }
            rue_parser::ast::Item::Struct(structure) => structure
                .methods
                .iter()
                .find(|method| method.span == locator.declaration_span)
                .map(|method| (method.params.as_slice(), method.return_type.as_ref())),
            rue_parser::ast::Item::Extern(block) => block
                .fns
                .iter()
                .find(|function| function.span == locator.declaration_span)
                .map(|function| (function.params.as_slice(), function.return_type.as_ref())),
            _ => None,
        });
        if let Some((parameters, return_type)) = signature {
            let span = match anchor {
                SignatureAnchor::Parameter(ordinal) => parameters
                    .get(ordinal as usize)
                    .map(|parameter| parameter.span),
                SignatureAnchor::ParameterType(ordinal) => parameters
                    .get(ordinal as usize)
                    .map(|parameter| parameter.ty.span()),
                SignatureAnchor::ResultType => return_type.map(rue_parser::ast::TypeExpr::span),
            };
            if let Some(span) = span {
                return CompileErrors::from(CompileError::new(kind.clone(), span));
            }
        }
    }
    if let F::DiagnosticAtDeclaration { kind, declaration } = failure
        && let Some(span) = modules
            .iter()
            .find(|module| module.module_id() == &declaration.module)
            .and_then(|module| module.definitions().declaration_locator(declaration))
            .map(|locator| locator.declaration_span)
    {
        return CompileErrors::from(CompileError::new(kind.clone(), span));
    }
    if let F::DuplicateDeclaration {
        kind,
        first,
        duplicate,
    } = failure
        && let Some(module) = modules
            .iter()
            .find(|module| module.module_id() == &duplicate.module)
        && let Some(duplicate_span) = module
            .definitions()
            .declaration_locator(duplicate)
            .map(|locator| locator.declaration_span)
        && let Some(first_module) = modules
            .iter()
            .find(|module| module.module_id() == &first.module)
        && let Some(first_span) = first_module
            .definitions()
            .declaration_locator(first)
            .map(|locator| locator.declaration_span)
    {
        // Every duplicate points at the whole offending declaration, which for
        // a function or type is its signature plus body. A test declaration's
        // body says nothing about why the name collides, so the diagnostic is
        // narrowed to its `test "name"` header (ADR-0083 §1); the same
        // narrowing applies to the first declaration's label.
        let header = |module: &crate::parsed_modules::ParsedModule, declaration: rue_span::Span| {
            if !matches!(kind, ErrorKind::DuplicateTestDefinition { .. }) {
                return declaration;
            }
            module
                .ast()
                .items
                .iter()
                .find_map(|item| match item {
                    rue_parser::ast::Item::Test(test) if test.span == declaration => {
                        Some(test.header_span)
                    }
                    _ => None,
                })
                .unwrap_or(declaration)
        };
        return CompileErrors::from(
            CompileError::new(kind.clone(), header(module, duplicate_span)).with_label(
                format!("first defined in {}", first_module.physical_path()),
                header(first_module, first_span),
            ),
        );
    }
    if let F::DiagnosticAtProducerRange {
        kind,
        producer: producer_key,
        start,
        end,
    } = failure
        && let Some(producer) = modules
            .iter()
            .find(|module| module.module_id() == &producer_key.module)
            .and_then(|module| module.definitions().declaration_locator(producer_key))
            .map(|locator| locator.declaration_span)
        && let (Some(start), Some(end)) = (
            producer.start.checked_add(*start),
            producer.start.checked_add(*end),
        )
        && start <= end
        && end <= producer.end
    {
        return CompileErrors::from(CompileError::new(
            kind.clone(),
            rue_span::Span::with_file(producer.file_id, start, end),
        ));
    }
    if let F::OwnershipGate { kind, gate } = failure {
        let primary_span = declaration.and_then(|key| {
            modules
                .iter()
                .find(|module| module.module_id() == &key.module)
                .and_then(|module| module.definitions().declaration_locator(key))
                .map(|locator| locator.declaration_span)
        });
        let mut error = match primary_span {
            Some(span) => CompileError::new(kind.clone(), span),
            None => CompileError::without_span(kind.clone()),
        };
        if let Some(application) = &gate.application
            && let Some(span) = modules
                .iter()
                .find(|module| module.module_id() == &application.declaration.module)
                .and_then(|module| {
                    module
                        .definitions()
                        .declaration_locator(&application.declaration)
                })
                .map(|locator| locator.declaration_span)
        {
            error = error.with_label("required by the type-constructor application here", span);
        }
        return CompileErrors::from(error);
    }
    if let (Some(declaration), F::Diagnostic(ErrorKind::CopyStructWithDestructor { type_name })) =
        (declaration, failure)
        && let Some(module) = modules
            .iter()
            .find(|module| module.module_id() == &declaration.module)
    {
        let destructor_span = module.ast().items.iter().find_map(|item| match item {
            rue_parser::ast::Item::DropFn(drop)
                if module.resolve_raw_symbol(drop.type_name.name) == type_name =>
            {
                Some(drop.span)
            }
            _ => None,
        });
        let copy_span = module.ast().items.iter().find_map(|item| match item {
            rue_parser::ast::Item::Struct(structure)
                if module.resolve_raw_symbol(structure.name.name) == type_name =>
            {
                rue_parser::ast::find_directive(
                    &structure.directives,
                    rue_parser::DirectiveName::Copy,
                )
                .map(|directive| directive.span)
            }
            _ => None,
        });
        if let Some(destructor_span) = destructor_span {
            let mut error = CompileError::new(
                ErrorKind::CopyStructWithDestructor {
                    type_name: type_name.clone(),
                },
                destructor_span,
            )
            .with_label("destructor defined here", destructor_span)
            .with_note(
                "`@copy` values are duplicated implicitly, so the destructor would run \
                     once per copy — cleaning up the same resource multiple times",
            )
            .with_help("remove the `@copy` attribute or remove the `drop fn`");
            if let Some(copy_span) = copy_span {
                error = error.with_label("type declared `@copy` here", copy_span);
            }
            return CompileErrors::from(error);
        }
    }
    let span = declaration.and_then(|key| {
        modules
            .iter()
            .find(|module| module.module_id() == &key.module)
            .and_then(|module| module.definitions().declaration_locator(key))
            .map(|locator| locator.declaration_span)
    });
    let (kind, help, note) = match failure {
        F::Diagnostic(kind) => (kind.clone(), None, None),
        F::DiagnosticAtParameter { kind, .. } => (kind.clone(), None, None),
        F::DiagnosticAtSignatureType { kind, .. } => (kind.clone(), None, None),
        F::DiagnosticAtDeclaration { kind, .. } => (kind.clone(), None, None),
        F::DuplicateDeclaration { kind, .. } => (kind.clone(), None, None),
        F::DuplicateDeclarations(_) => unreachable!("duplicate batches return above"),
        F::ForeignSignatureConflict(_) => {
            unreachable!("foreign-signature conflicts return above")
        }
        F::DiagnosticAtProducerRange { kind, .. } => (kind.clone(), None, None),
        F::OwnershipGate { kind, .. } => (kind.clone(), None, None),
        F::DiagnosticWithHelp { kind, help } => (kind.clone(), Some(help.clone()), None),
        F::DiagnosticWithNote { kind, note } => (kind.clone(), None, Some(note.clone())),
        F::Cycle(nodes) => (
            ErrorKind::ConstInitializerCycle {
                cycle: nodes
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join(" -> "),
            },
            None,
            None,
        ),
        F::SignatureReentry { cycle, .. } => (
            ErrorKind::UnknownType(
                cycle
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join(" -> "),
            ),
            None,
            None,
        ),
        F::Resolution(message) if message.starts_with("unknown array length") => (
            ErrorKind::InvalidArrayLength {
                reason: message
                    .strip_prefix("unknown array length `")
                    .and_then(|name| name.strip_suffix('`'))
                    .map_or_else(
                        || message.to_string(),
                        |name| format!("'{name}' is not a compile-time constant"),
                    ),
            },
            None,
            None,
        ),
        F::Resolution(message) => (
            ErrorKind::ComptimeEvaluationFailed {
                reason: message.to_string(),
            },
            None,
            None,
        ),
        F::Shell(message) | F::Syntax(message) => (
            ErrorKind::InternalError(format!("semantic query invariant failed: {message}")),
            None,
            None,
        ),
    };
    let error = match span {
        Some(span) => CompileError::new(kind, span),
        None => CompileError::without_span(kind),
    };
    let error = match help {
        Some(help) => error.with_help(help.to_string()),
        None => error,
    };
    CompileErrors::from(match note {
        Some(note) => error.with_note(note.to_string()),
        None => error,
    })
}

fn well_known_option_resolution_diagnostics(
    modules: &[Arc<crate::parsed_modules::ParsedModule>],
    failure: &crate::revisioned_query_database::WellKnownOptionResolutionFailure,
) -> CompileErrors {
    use crate::revisioned_query_database::WellKnownOptionResolutionFailure as F;
    match failure {
        F::Incomplete {
            payload,
            prerequisite,
            detail,
        } => CompileErrors::from(CompileError::without_span(ErrorKind::InternalError(
            format!(
                "exact trusted Option({payload:?}) prerequisite resolution was incomplete{}: {detail}",
                prerequisite
                    .as_ref()
                    .map_or_else(String::new, |key| format!(" at {key:?}"))
            ),
        ))),
        F::Semantic { payload, failure } => {
            let mut errors = semantic_nucleus_failure_diagnostics(modules, None, failure);
            if errors.is_empty() {
                errors = CompileErrors::from(CompileError::without_span(ErrorKind::InternalError(
                    format!(
                        "trusted Option({payload:?}) resolution failed without diagnostics: {failure:?}"
                    ),
                )));
            }
            errors
        }
        F::WrongProjection { payload, detail } => CompileErrors::from(CompileError::without_span(
            ErrorKind::InternalError(format!(
                "trusted Option({payload:?}) resolution returned the wrong semantic projection: {detail}"
            )),
        )),
    }
}

fn semantic_diagnostic_input(
    input: &CodegenInputDescriptor,
    imports: CanonicalImportGraph,
) -> crate::ResolvedCodegenRevision {
    crate::ResolvedCodegenRevision::new(
        crate::ResolvedProgramRevision::new(input.semantic.clone(), imports),
        input.opt_level,
    )
}

/// A body graph, or the diagnostics that rejected the request that asked for it.
enum RootedBodyGraphAttempt {
    Graph(Box<RootedBodyGraph>),
    Rejected(Box<RootedBodyGraphRejection>),
}

/// Why a rooted semantic request failed, with each diagnostic's owning body.
///
/// Diagnostics are kept in publication order, exactly as the flattened
/// `CompileErrors` a whole-run failure reports has always carried them, and
/// each one remembers whether it belongs to one reached body or to the
/// compilation as a whole. Only the test-image request reads the attribution
/// (ADR-0083 §3): a body that failed to analyze excludes precisely the tests
/// whose closures reach it, and nothing else.
#[derive(Default)]
pub(super) struct RootedBodyGraphRejection {
    /// `(owning body, diagnostic)` in publication order; `None` for a
    /// diagnostic no single body owns.
    entries: Vec<(Option<crate::FunctionInstanceKey>, CompileError)>,
    /// Whether any diagnostic belongs to the compilation rather than a body.
    /// Such a request fails as a whole however its roots are arranged.
    global: bool,
    /// The analyzed closure's bodies, the call graph attribution walks.
    bodies: Arc<[crate::body_query::BodyClosureBody]>,
    /// The closure's own drop-glue plans, so the walk can follow the one edge
    /// that reaches a body through no call: a value's destruction. The
    /// reachability walk resolved these to schedule the destructors in the
    /// first place, so reading them back is the same relation rather than a
    /// second one.
    drop_glue_plans: Arc<[(crate::TypeInstanceKey, crate::type_queries::DropGlueFacts)]>,
    /// The request's full test inventory, so a `--list` that only wants the
    /// declarations still gets them (ADR-0083 §3: a test whose body does not
    /// analyze still parsed its declaration).
    test_inventory: Arc<[crate::test_inventory::RootedTest]>,
}

impl RootedBodyGraphRejection {
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn push_global(&mut self, error: CompileError) {
        self.global = true;
        self.entries.push((None, error));
    }

    fn extend_global(&mut self, errors: impl IntoIterator<Item = CompileError>) {
        for error in errors {
            self.push_global(error);
        }
    }

    fn push_body(&mut self, instance: crate::FunctionInstanceKey, error: CompileError) {
        self.entries.push((Some(instance), error));
    }

    fn extend_body(
        &mut self,
        instance: crate::FunctionInstanceKey,
        errors: impl IntoIterator<Item = CompileError>,
    ) {
        for error in errors {
            self.push_body(instance.clone(), error);
        }
    }

    /// The whole-run failure: every diagnostic, in publication order.
    fn into_errors(self) -> CompileErrors {
        self.entries
            .into_iter()
            .map(|(_, error)| error)
            .collect::<Vec<_>>()
            .into()
    }

    /// The diagnostics of each failed body, keyed by the body, in the same
    /// publication order within each body.
    fn per_body(&self) -> BTreeMap<crate::FunctionInstanceKey, Vec<CompileError>> {
        let mut per_body: BTreeMap<_, Vec<CompileError>> = BTreeMap::new();
        for (instance, error) in &self.entries {
            if let Some(instance) = instance {
                per_body
                    .entry(instance.clone())
                    .or_default()
                    .push(error.clone());
            }
        }
        per_body
    }

    /// Every body-owned diagnostic, grouped by its owning body.
    ///
    /// This is the sequence stderr publishes for a rejection whose failures are
    /// all inside bodies, and the one computation both the run path and `--list`
    /// read: a listing that could disagree with the run it previews about which
    /// bodies are broken would be worse than no listing. A global diagnostic is
    /// not here — such a rejection fails the request outright.
    fn body_diagnostics(&self) -> CompileErrors {
        self.per_body()
            .into_values()
            .flatten()
            .collect::<Vec<_>>()
            .into()
    }

    /// Which analyzed body reaches which other analyzed body.
    ///
    /// This is the closure's own scheduling relation, read back off what the
    /// closure published rather than recomputed: the reachability walk
    /// schedules a body for each `BodyReference::Callable` it finds, and it
    /// schedules a source destructor for each `BodyReference::DropGlue` — the
    /// one way a body is reached by no call. Destruction has to be an edge
    /// here because a `drop fn` is ordinary analyzed source that can fail to
    /// analyze, while the glue calling it is synthesized and cannot; without
    /// it, a broken destructor would be attributable to no test and fail the
    /// whole run. Error printers are synthesized in full, so they carry no
    /// source diagnostic to attribute.
    fn call_edges(&self) -> BTreeMap<crate::FunctionInstanceKey, Vec<crate::FunctionInstanceKey>> {
        let plans = self
            .drop_glue_plans
            .iter()
            .map(|(ty, facts)| (ty, facts))
            .collect::<BTreeMap<_, _>>();
        let mut edges: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for body in self.bodies.iter() {
            let rue_query::QueryOutcome::Success(bundle) = body.bundle.outcome() else {
                continue;
            };
            let references = match &bundle.transaction {
                crate::body_query::BodyTransaction::Success { references, .. }
                | crate::body_query::BodyTransaction::DeterministicFailure { references, .. } => {
                    references
                }
                crate::body_query::BodyTransaction::Control(_) => continue,
            };
            let callees = edges.entry(body.key.instance.clone()).or_default();
            // A destroyed aggregate's glue destroys its parts, so the walk
            // follows `nested` to reach a destructor several layers down. The
            // visited set is over the plan graph the closure already
            // terminated on, so it cannot cycle further than that did.
            let mut pending = Vec::new();
            let mut visited = BTreeSet::new();
            for reference in references.0.iter() {
                match reference {
                    crate::body_query::BodyReference::Callable(callable) => {
                        callees.push(callable.clone());
                    }
                    crate::body_query::BodyReference::DropGlue(ty) => pending.push(ty),
                    crate::body_query::BodyReference::Definition(_)
                    | crate::body_query::BodyReference::Type(_) => {}
                }
            }
            while let Some(ty) = pending.pop() {
                if !visited.insert(ty) {
                    continue;
                }
                let Some(facts) = plans.get(ty) else {
                    continue;
                };
                if let Some(destructor) = &facts.destructor {
                    callees.push(destructor.clone());
                }
                pending.extend(facts.nested.iter());
            }
        }
        edges
    }
}

/// One test excluded from a test image because its closure failed to analyze.
pub(crate) struct FailedTestClosure {
    pub(crate) test: crate::test_inventory::RootedTest,
    pub(crate) errors: CompileErrors,
}

/// A test request's ordered inventory and the failures its bodies reported.
///
/// A listing publishes both: every declaration that parsed, and the diagnostics
/// of the bodies that did not analyze (ADR-0083 §2).
pub(crate) struct RootedTestInventory {
    pub(crate) entries: Vec<crate::unstable::TestInventoryEntry>,
    /// The same sequence the run path writes to stderr, empty when every body
    /// in the closure analyzed.
    pub(crate) diagnostics: CompileErrors,
}

/// What a test request's semantic analysis produced when some closures failed.
pub(crate) struct TestClosureAnalysis {
    /// Every declared test, in stable-ID order.
    pub(crate) inventory: Arc<[crate::test_inventory::RootedTest]>,
    /// The tests whose closures failed, in ordinal order.
    pub(crate) failed: Vec<FailedTestClosure>,
    /// Every diagnostic behind those failures, once each however many tests
    /// reach it. This is what stderr publishes; the copies attached to each
    /// failed test are the attribution convenience (ADR-0083 §3).
    pub(crate) diagnostics: CompileErrors,
}

/// What a cancellable test inventory answered (RUE-2174).
pub(crate) enum TestInventoryOutcome {
    Listed(RootedTestInventory),
    /// The request was refused before any inventory existed; a per-body
    /// failure is listed beside the inventory instead.
    Errors(CompileErrors),
    /// The caller abandoned the request. Nothing is reported: a partial
    /// inventory or its diagnostics would describe a request nobody is
    /// waiting for.
    Canceled,
}

/// What a cancellable test-closure analysis answered (RUE-2023).
pub(crate) enum TestClosureAnalysisOutcome {
    Analyzed(TestClosureAnalysis),
    /// The request failed outside every test closure, so the whole request
    /// fails. Per-test failures are inside [`TestClosureAnalysis::failed`]
    /// instead; they are verdicts, not a failed request.
    Errors(CompileErrors),
    /// A newer source revision arrived. Nothing is reported: the diagnostics
    /// this would have carried describe source that no longer exists.
    Canceled,
}

/// Render a semantic control answer on the uncancellable error surface.
///
/// The counterpart of [`pipeline_control_errors`] for the requests that stop
/// at semantic analysis. `context` names the stage and reaches the reader only
/// through an internal-error diagnostic; a park and a compile failure carry
/// their own.
fn semantic_control_errors(context: &str, control: SemanticRequestControl) -> CompileErrors {
    match control {
        SemanticRequestControl::Compile(errors) => errors,
        SemanticRequestControl::Abort(abort) => pipeline_abort_errors(context, abort),
        SemanticRequestControl::Parked(park) => unresolved_toolchain_park_errors(&park),
    }
}
