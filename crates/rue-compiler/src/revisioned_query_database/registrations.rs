use super::body::*;
use super::semantic::*;
use super::*;
mod backend;
mod body;
mod parse_import;
mod provider;
mod semantic;

use backend::*;
use body::*;
use parse_import::*;
#[cfg(test)]
use provider::register_provider_probe;
use semantic::*;

/// The single source-level manifest for registered compiler families.  Each
/// entry names the phase owner, stable family identity, constructor macro, and
/// the real Rust fragment that defines that macro.  The API inventory checks
/// this manifest against both the constructor invocation stream and fragment
/// contents, so a family cannot silently acquire a second authority.
#[cfg(test)]
pub(crate) const REGISTRATION_MANIFEST: &[(&str, &str, &str, &str)] = &[
    (
        "parse_import",
        concat!("compiler.", "parse-module"),
        "register_parse_import_parse_modules",
        include_str!("registrations/parse_import/parse_modules.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "parse-module-frontier"),
        "register_parse_import_parse_module_batches",
        include_str!("registrations/parse_import/parse_module_batches.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "module-source-basis"),
        "register_parse_import_module_source_bases",
        include_str!("registrations/parse_import/module_source_bases.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "module-index"),
        "register_parse_import_module_indexes",
        include_str!("registrations/parse_import/module_indexes.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "declaration-occurrence-index"),
        "register_parse_import_declaration_occurrence_indexes",
        include_str!("registrations/parse_import/declaration_occurrence_indexes.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "declaration-order"),
        "register_parse_import_declaration_orders",
        include_str!("registrations/parse_import/declaration_orders.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "declaration-shell"),
        "register_semantic_declaration_shells",
        include_str!("registrations/semantic/declaration_shells.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "stable-declaration-classification"),
        "register_semantic_stable_declaration_classifications",
        include_str!("registrations/semantic/stable_declaration_classifications.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "declaration-body-plan-artifacts"),
        "register_semantic_declaration_body_plan_artifacts",
        include_str!("registrations/semantic/declaration_body_plan_artifacts.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "lookup-name"),
        "register_parse_import_lookup_names",
        include_str!("registrations/parse_import/lookup_names.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "lookup-import"),
        "register_parse_import_lookup_imports",
        include_str!("registrations/parse_import/lookup_imports.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "resolve-import"),
        "register_parse_import_resolve_imports",
        include_str!("registrations/parse_import/resolve_imports.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "declaration-import"),
        "register_parse_import_declaration_imports",
        include_str!("registrations/parse_import/declaration_imports.rs"),
    ),
    (
        "body",
        concat!("compiler.", "body-source-basis"),
        "register_body_body_source_bases",
        include_str!("registrations/body/body_source_bases.rs"),
    ),
    (
        "body",
        concat!("compiler.", "test-body-input-probe"),
        "register_body_body_inputs",
        include_str!("registrations/body/body_inputs.rs"),
    ),
    (
        "body",
        concat!("compiler.", "warning-call-head-projection"),
        "register_body_warning_call_head_projections",
        include_str!("registrations/body/warning_call_head_projections.rs"),
    ),
    (
        "body",
        concat!("compiler.", "warning-body-references"),
        "register_body_warning_body_references",
        include_str!("registrations/body/warning_body_references.rs"),
    ),
    (
        "body",
        concat!("compiler.", "warning-body-reference-frontier"),
        "register_body_warning_body_reference_batches",
        include_str!("registrations/body/warning_body_reference_batches.rs"),
    ),
    (
        "body",
        concat!("compiler.", "body-transaction"),
        "register_body_body_transactions",
        include_str!("registrations/body/body_transactions.rs"),
    ),
    (
        "body",
        concat!("compiler.", "body-toolchain-demands"),
        "register_body_body_toolchain_demands",
        include_str!("registrations/body/body_toolchain_demands.rs"),
    ),
    (
        "body",
        concat!("compiler.", "body-produced-anonymous"),
        "register_body_body_produced_anonymous",
        include_str!("registrations/body/body_produced_anonymous.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "semantic-nucleus"),
        "register_semantic_semantic_nucleus",
        include_str!("registrations/semantic/semantic_nucleus.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "declaration-semantics-projection"),
        "register_semantic_declaration_semantics_projection",
        include_str!("registrations/semantic/declaration_semantics_projection.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "declaration-semantics-publication"),
        "register_semantic_declaration_semantics_publications",
        include_str!("registrations/semantic/declaration_semantics_publications.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "type-shape"),
        "register_semantic_type_shapes",
        include_str!("registrations/semantic/type_shapes.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "type-facts"),
        "register_semantic_type_facts",
        include_str!("registrations/semantic/type_facts.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "layout"),
        "register_semantic_layouts",
        include_str!("registrations/semantic/layouts.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "call-abi"),
        "register_semantic_call_abis",
        include_str!("registrations/semantic/call_abis.rs"),
    ),
    (
        "semantic",
        concat!("compiler.", "drop-glue"),
        "register_semantic_drop_glues",
        include_str!("registrations/semantic/drop_glues.rs"),
    ),
    (
        "backend",
        concat!("compiler.", "cfg"),
        "register_backend_cfgs",
        include_str!("registrations/backend/cfgs.rs"),
    ),
    (
        "backend",
        concat!("compiler.", "raw-cfg-batch"),
        "register_backend_raw_cfg_batches",
        include_str!("registrations/backend/raw_cfg_batches.rs"),
    ),
    (
        "backend",
        concat!("compiler.", "optimized-cfg"),
        "register_backend_optimized_cfgs",
        include_str!("registrations/backend/optimized_cfgs.rs"),
    ),
    (
        "backend",
        concat!("compiler.", "optimized-cfg-batch"),
        "register_backend_optimized_cfg_batches",
        include_str!("registrations/backend/optimized_cfg_batches.rs"),
    ),
    (
        "backend",
        concat!("compiler.", "codegen-unit"),
        "register_backend_codegen_units",
        include_str!("registrations/backend/codegen_units.rs"),
    ),
    (
        "backend",
        concat!("compiler.", "codegen-unit-batch"),
        "register_backend_codegen_unit_batches",
        include_str!("registrations/backend/codegen_unit_batches.rs"),
    ),
    (
        "backend",
        concat!("compiler.", "object-projection"),
        "register_backend_object_projections",
        include_str!("registrations/backend/object_projections.rs"),
    ),
    (
        "backend",
        concat!("compiler.", "object-projection-batch"),
        "register_backend_object_projection_batches",
        include_str!("registrations/backend/object_projection_batches.rs"),
    ),
    (
        "backend",
        concat!("compiler.", "backend-root-publication"),
        "register_backend_backend_root_publications",
        include_str!("registrations/backend/backend_root_publications.rs"),
    ),
    (
        "body",
        concat!("compiler.", "body-analysis-bundle"),
        "register_body_body_analysis_bundles",
        include_str!("registrations/body/body_analysis_bundles.rs"),
    ),
    (
        "body",
        concat!("compiler.", "body-reachability"),
        "register_body_body_reachability",
        include_str!("registrations/body/body_reachability.rs"),
    ),
    (
        "body",
        concat!("compiler.", "body-closure"),
        "register_body_body_closures",
        include_str!("registrations/body/body_closures.rs"),
    ),
    (
        "body",
        concat!("compiler.", "body-closure-publication"),
        "register_body_body_closure_publications",
        include_str!("registrations/body/body_closure_publications.rs"),
    ),
    (
        "parse_import",
        concat!("compiler.", "parse"),
        "register_parse_import_parse",
        include_str!("registrations/parse_import/parse.rs"),
    ),
    (
        "body",
        concat!("compiler.", "body-fact-provider-probe"),
        "register_provider_probe",
        include_str!("registrations/provider_probe.rs"),
    ),
];

#[cfg(test)]
impl Default for RevisionedQueryDatabase {
    fn default() -> Self {
        Self::new_canonical()
    }
}

impl RevisionedQueryDatabase {
    pub(crate) fn new(
        _authority: crate::session::RevisionedQueryDatabaseConstructionToken,
    ) -> Self {
        Self::new_canonical()
    }

    fn new_canonical() -> Self {
        Self::with_declaration_memo_retention_and_concurrency(
            DECLARATION_QUERY_MEMO_RETENTION,
            crate::query_concurrency(),
            u32::MAX as usize,
        )
    }

    /// Construct the database with an explicit declaration-keyed memo
    /// retention. Production uses [`DECLARATION_QUERY_MEMO_RETENTION`];
    /// eviction-lifecycle tests pass a small cap so exceeding it stays cheap.
    #[cfg(test)]
    pub(crate) fn with_declaration_memo_retention(declaration_memo_retention: usize) -> Self {
        Self::with_declaration_memo_retention_and_concurrency(
            declaration_memo_retention,
            1,
            rue_lexer::MAX_INTERNED_STRINGS,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_query_concurrency(query_concurrency: usize) -> Self {
        Self::with_declaration_memo_retention_and_concurrency(
            DECLARATION_QUERY_MEMO_RETENTION,
            query_concurrency,
            rue_lexer::MAX_INTERNED_STRINGS,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_interner_limit(max_entries: usize) -> Self {
        Self::with_declaration_memo_retention_and_concurrency(
            DECLARATION_QUERY_MEMO_RETENTION,
            1,
            max_entries,
        )
    }

    fn with_declaration_memo_retention_and_concurrency(
        declaration_memo_retention: usize,
        query_concurrency: usize,
        max_interner_entries: usize,
    ) -> Self {
        let runtime = CompilerQueryRuntime(QueryRuntime::new(query_concurrency));
        let body_reachability_meter = Arc::new(BodyReachabilityMeter::default());
        let identity_resolution =
            Arc::new(crate::source_snapshot::IdentityResolutionMeter::default());
        let module_store = Arc::new(Mutex::new(ModuleInputStore::default()));
        #[cfg(test)]
        let test_import_store = Arc::new(Mutex::new(TestImportInputStore {
            next_stamp: 1,
            ..TestImportInputStore::default()
        }));
        #[cfg(test)]
        let declaration_body_plan_failure_injection: DeclarationBodyPlanFailureInjection =
            Arc::new(Mutex::new(None));
        #[cfg(test)]
        let declaration_body_plan_astgen_evaluations =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let parse_store = module_store.clone();
        let parse_stage: ParseStage = Arc::new(Mutex::new(AHashMap::new()));
        let parse_stage_for_parse_modules = parse_stage.clone();
        let parse_identity_resolution = identity_resolution.clone();
        let parse_modules = register_parse_import_parse_modules!(
            parse_identity_resolution,
            parse_stage_for_parse_modules,
            parse_store,
            runtime
        );
        let parse_modules_for_batch = parse_modules.clone();
        let parse_module_batches =
            register_parse_import_parse_module_batches!(parse_modules_for_batch, runtime);
        let parse_for_module_source_bases = parse_modules.clone();
        let module_store_for_module_source_bases = module_store.clone();
        // Body-host module registration needs only stable file identity and
        // path. Keep that dependency independent of the exact parsed module so
        // an imported body's text edit does not invalidate every caller merely
        // to rediscover the same module identity. Exact body locators remain
        // responsible for current lengths and spans.
        let module_source_bases = register_parse_import_module_source_bases!(
            module_store_for_module_source_bases,
            parse_for_module_source_bases,
            runtime
        );
        #[cfg(test)]
        let module_index_build_log: Arc<Mutex<Vec<ModuleId>>> = Arc::new(Mutex::new(Vec::new()));
        #[cfg(test)]
        let lookup_name_eval_log: Arc<Mutex<Vec<LookupNameKey>>> = Arc::new(Mutex::new(Vec::new()));
        #[cfg(test)]
        let lookup_import_eval_log: Arc<Mutex<Vec<LookupImportKey>>> =
            Arc::new(Mutex::new(Vec::new()));
        #[cfg(test)]
        let body_closure_anonymous_digest_forcing =
            Arc::new(Mutex::new(TestBodyClosureAnonymousDigestForcing::default()));
        let parse_for_index = parse_modules.clone();
        #[cfg(test)]
        let module_index_build_probe = module_index_build_log.clone();
        let module_indexes = register_parse_import_module_indexes!(
            module_index_build_probe,
            parse_for_index,
            runtime
        );
        let parse_for_declaration_occurrences = parse_modules.clone();
        let parse_for_declaration_orders = parse_modules.clone();
        let parse_for_declaration_shells = parse_modules.clone();
        let declaration_occurrence_indexes = register_parse_import_declaration_occurrence_indexes!(
            parse_for_declaration_occurrences,
            runtime
        );
        let declaration_orders =
            register_parse_import_declaration_orders!(parse_for_declaration_orders, runtime);
        let occurrences_for_shells = declaration_occurrence_indexes.clone();
        let declaration_shells = register_semantic_declaration_shells!(
            declaration_memo_retention,
            occurrences_for_shells,
            parse_for_declaration_shells,
            runtime
        );
        let occurrences_for_stable_classification = declaration_occurrence_indexes.clone();
        let shells_for_stable_classification = declaration_shells.clone();
        let stable_declaration_classifications = register_semantic_stable_declaration_classifications!(
            declaration_memo_retention,
            occurrences_for_stable_classification,
            runtime,
            shells_for_stable_classification
        );
        let parse_for_declaration_body_plan_artifacts = parse_modules.clone();
        let index_for_declaration_body_plan_artifacts = module_indexes.clone();
        #[cfg(test)]
        let plan_failure_injection_for_artifacts = declaration_body_plan_failure_injection.clone();
        #[cfg(test)]
        let astgen_evaluations_for_artifacts = declaration_body_plan_astgen_evaluations.clone();
        let declaration_body_plan_artifacts = register_semantic_declaration_body_plan_artifacts!(
            astgen_evaluations_for_artifacts,
            index_for_declaration_body_plan_artifacts,
            parse_for_declaration_body_plan_artifacts,
            plan_failure_injection_for_artifacts,
            runtime
        );
        let index_for_lookup = module_indexes.clone();
        #[cfg(test)]
        let lookup_name_eval_probe = lookup_name_eval_log.clone();
        let lookup_names = register_parse_import_lookup_names!(
            declaration_memo_retention,
            index_for_lookup,
            lookup_name_eval_probe,
            runtime
        );
        let index_for_import_lookup = module_indexes.clone();
        let resolve_import_for_lookup = Arc::new(std::sync::OnceLock::<
            QueryFamily<ResolveImportKey, ResolveImportValue>,
        >::new());
        let resolve_import_for_lookup_evaluator = resolve_import_for_lookup.clone();
        #[cfg(test)]
        let lookup_import_eval_probe = lookup_import_eval_log.clone();
        let lookup_imports = register_parse_import_lookup_imports!(
            declaration_memo_retention,
            index_for_import_lookup,
            lookup_import_eval_probe,
            resolve_import_for_lookup_evaluator,
            runtime
        );
        let import_store = Arc::new(Mutex::new(ImportInputStore::default()));
        let evaluator_store = import_store.clone();
        let index_for_import_resolution = module_indexes.clone();
        let resolve_identity_resolution = identity_resolution.clone();
        let resolve_imports = register_parse_import_resolve_imports!(
            evaluator_store,
            index_for_import_resolution,
            resolve_identity_resolution,
            runtime
        );
        assert!(
            resolve_import_for_lookup
                .set(resolve_imports.clone())
                .is_ok(),
            "ResolveImport lookup dependency is installed once"
        );
        let occurrences_for_declaration_import = declaration_occurrence_indexes.clone();
        let shells_for_declaration_import = declaration_shells.clone();
        let parse_for_declaration_import = parse_modules.clone();
        let resolve_for_declaration_import = resolve_imports.clone();
        #[cfg(test)]
        let test_imports_for_declaration_import = test_import_store.clone();
        let declaration_imports = register_parse_import_declaration_imports!(
            declaration_memo_retention,
            occurrences_for_declaration_import,
            parse_for_declaration_import,
            resolve_for_declaration_import,
            runtime,
            shells_for_declaration_import,
            test_imports_for_declaration_import
        );
        let shells_for_semantic_nucleus = declaration_shells.clone();
        let shells_for_produced_anonymous = declaration_shells.clone();
        let parse_for_semantic_nucleus = parse_modules.clone();
        let artifacts_for_semantic_nucleus = declaration_body_plan_artifacts.clone();
        let names_for_semantic_nucleus = lookup_names.clone();
        let imports_for_semantic_nucleus = declaration_imports.clone();
        let classifications_for_body_source_bases = stable_declaration_classifications.clone();
        let parse_for_body_source_bases = parse_modules.clone();
        let module_store_for_body_source_bases = module_store.clone();
        let body_source_bases = register_body_body_source_bases!(
            classifications_for_body_source_bases,
            module_store_for_body_source_bases,
            parse_for_body_source_bases,
            runtime
        );
        let body_input_resolver = BodyInputResolver {
            stable_declaration_classifications: stable_declaration_classifications.clone(),
            declaration_shells: declaration_shells.clone(),
            declaration_body_plan_artifacts: declaration_body_plan_artifacts.clone(),
            body_source_bases: body_source_bases.clone(),
        };
        #[cfg(test)]
        let body_inputs = register_body_body_inputs!(body_input_resolver, runtime);
        let parse_for_warning_call_heads = parse_modules.clone();
        let warning_call_head_projections = register_body_warning_call_head_projections!(
            declaration_memo_retention,
            parse_for_warning_call_heads,
            runtime
        );
        let classifications_for_warning_references = stable_declaration_classifications.clone();
        let shells_for_warning_references = declaration_shells.clone();
        let call_heads_for_warning_references = warning_call_head_projections.clone();
        let imports_for_warning_references = declaration_imports.clone();
        // Allocate the semantic publication roots before constructing the
        // warning frontier so its evaluator can borrow the already-published
        // declaration/body cones without re-demanding their children.
        let publication_cone_retention_failures = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let declaration_semantics_root =
            Arc::new(Mutex::new(PublishedDeclarationSemanticsRoot::default()));
        let body_closure_root = Arc::new(Mutex::new(PublishedBodyClosureRoot::default()));
        let body_reachability_root = Arc::new(Mutex::new(PublishedBodyReachabilityRoot::default()));
        let warning_body_references = register_body_warning_body_references!(
            call_heads_for_warning_references,
            classifications_for_warning_references,
            imports_for_warning_references,
            runtime,
            shells_for_warning_references
        );
        let warning_body_references_for_batch = warning_body_references.clone();
        let declaration_root_for_warning_batch = declaration_semantics_root.clone();
        let closure_root_for_warning_batch = body_closure_root.clone();
        let reachability_root_for_warning_batch = body_reachability_root.clone();
        let classifications_for_warning_batch = stable_declaration_classifications.clone();
        let shells_for_warning_batch = declaration_shells.clone();
        let call_heads_for_warning_batch = warning_call_head_projections.clone();
        let warning_body_reference_batches = register_body_warning_body_reference_batches!(
            call_heads_for_warning_batch,
            classifications_for_warning_batch,
            closure_root_for_warning_batch,
            declaration_root_for_warning_batch,
            reachability_root_for_warning_batch,
            runtime,
            shells_for_warning_batch,
            warning_body_references_for_batch
        );
        let shared_durable_payloads = Arc::new(SharedDurablePayloadCache::default());
        let body_transaction_evaluator =
            Arc::new(std::sync::OnceLock::<BodyTransactionEvaluator>::new());
        let body_transaction_evaluator_for_family = body_transaction_evaluator.clone();
        let body_transactions =
            register_body_body_transactions!(body_transaction_evaluator_for_family, runtime);
        let artifacts_for_toolchain_demands = declaration_body_plan_artifacts.clone();
        let body_toolchain_demands =
            register_body_body_toolchain_demands!(artifacts_for_toolchain_demands, runtime);
        let transactions_for_produced_anonymous = body_transactions.clone();
        let semantic_nucleus_for_produced_anonymous =
            Arc::new(std::sync::OnceLock::<SemanticNucleusFamily>::new());
        let semantic_nucleus_for_produced_anonymous_evaluator =
            semantic_nucleus_for_produced_anonymous.clone();
        let body_produced_anonymous = register_body_body_produced_anonymous!(
            runtime,
            semantic_nucleus_for_produced_anonymous_evaluator,
            shells_for_produced_anonymous,
            transactions_for_produced_anonymous
        );
        let produced_anonymous_for_semantic_nucleus = body_produced_anonymous.clone();
        let semantic_nucleus = register_semantic_semantic_nucleus!(
            artifacts_for_semantic_nucleus,
            declaration_memo_retention,
            imports_for_semantic_nucleus,
            names_for_semantic_nucleus,
            parse_for_semantic_nucleus,
            produced_anonymous_for_semantic_nucleus,
            runtime,
            shells_for_semantic_nucleus
        );
        assert!(
            semantic_nucleus_for_produced_anonymous
                .set(semantic_nucleus.clone())
                .is_ok(),
            "SemanticNucleus producer projection is installed once"
        );
        let occurrences_for_declaration_projection = declaration_occurrence_indexes.clone();
        let orders_for_declaration_projection = declaration_orders.clone();
        let nucleus_for_declaration_projection = semantic_nucleus.clone();
        let shells_for_declaration_projection = declaration_shells.clone();
        let cone_retention_failures_for_declaration_publication =
            publication_cone_retention_failures.clone();
        let declaration_semantics_projection = register_semantic_declaration_semantics_projection!(
            nucleus_for_declaration_projection,
            occurrences_for_declaration_projection,
            orders_for_declaration_projection,
            runtime,
            shells_for_declaration_projection
        );
        let projection_for_declaration_publication = declaration_semantics_projection.clone();
        let artifacts_for_declaration_publication = declaration_body_plan_artifacts.clone();
        let root_for_declaration_publication = declaration_semantics_root.clone();
        let closure_root_for_declaration_publication = body_closure_root.clone();
        let reachability_root_for_declaration_publication = body_reachability_root.clone();
        let declaration_semantics_publications = register_semantic_declaration_semantics_publications!(
            artifacts_for_declaration_publication,
            closure_root_for_declaration_publication,
            cone_retention_failures_for_declaration_publication,
            projection_for_declaration_publication,
            reachability_root_for_declaration_publication,
            root_for_declaration_publication,
            runtime
        );
        let semantic_nucleus_for_type_shape = semantic_nucleus.clone();
        let produced_anonymous_for_type_shape = body_produced_anonymous.clone();
        let type_shapes = register_semantic_type_shapes!(
            produced_anonymous_for_type_shape,
            runtime,
            semantic_nucleus_for_type_shape
        );
        let type_facts_family = Arc::new(std::sync::OnceLock::<
            QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::TypeFactsValue>,
        >::new());
        let type_facts_family_for_evaluator = type_facts_family.clone();
        let semantic_nucleus_for_type_facts = semantic_nucleus.clone();
        let lookup_names_for_type_facts = lookup_names.clone();
        let produced_anonymous_for_type_facts = body_produced_anonymous.clone();
        let type_shapes_for_type_facts = type_shapes.clone();
        let type_facts = register_semantic_type_facts!(
            lookup_names_for_type_facts,
            produced_anonymous_for_type_facts,
            runtime,
            semantic_nucleus_for_type_facts,
            type_facts_family_for_evaluator,
            type_shapes_for_type_facts
        );
        assert!(
            type_facts_family.set(type_facts.clone()).is_ok(),
            "TypeFacts family is installed once"
        );
        let layout_family = Arc::new(std::sync::OnceLock::<
            QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::LayoutValue>,
        >::new());
        let layout_family_for_evaluator = layout_family.clone();
        let type_shapes_for_layout = type_shapes.clone();
        let layouts = register_semantic_layouts!(
            layout_family_for_evaluator,
            runtime,
            type_shapes_for_layout
        );
        assert!(
            layout_family.set(layouts.clone()).is_ok(),
            "Layout family is installed once"
        );
        let semantic_nucleus_for_call_abi = semantic_nucleus.clone();
        let declaration_shells_for_call_abi = declaration_shells.clone();
        let lookup_names_for_call_abi = lookup_names.clone();
        let produced_anonymous_for_call_abi = body_produced_anonymous.clone();
        let layouts_for_call_abi = layouts.clone();
        let call_abis = register_semantic_call_abis!(
            declaration_shells_for_call_abi,
            layouts_for_call_abi,
            lookup_names_for_call_abi,
            produced_anonymous_for_call_abi,
            runtime,
            semantic_nucleus_for_call_abi
        );
        let type_facts_for_drop_glue = type_facts.clone();
        let drop_glues = register_semantic_drop_glues!(runtime, type_facts_for_drop_glue);
        let layouts_for_cfg = layouts.clone();
        let type_facts_for_cfg = type_facts.clone();
        let drop_glues_for_cfg = drop_glues.clone();
        let call_abis_for_cfg = call_abis.clone();
        let cfgs = register_backend_cfgs!(
            call_abis_for_cfg,
            drop_glues_for_cfg,
            layouts_for_cfg,
            runtime,
            type_facts_for_cfg
        );
        let backend_root = Arc::new(Mutex::new(PublishedBackendRoot::default()));
        let cfg_collection_root = Arc::new(Mutex::new(PublishedCollectionRoot::default()));
        let codegen_collection_root = Arc::new(Mutex::new(PublishedCollectionRoot::default()));
        let cfgs_for_raw_batch = cfgs.clone();
        let backend_root_for_raw_cfg_batch = backend_root.clone();
        let body_closure_root_for_raw_cfg_batch = body_closure_root.clone();
        let body_reachability_root_for_raw_cfg_batch = body_reachability_root.clone();
        let cfg_collection_root_for_raw_cfg_batch = cfg_collection_root.clone();
        let codegen_collection_root_for_raw_cfg_batch = codegen_collection_root.clone();
        let raw_cfg_batches = register_backend_raw_cfg_batches!(
            backend_root_for_raw_cfg_batch,
            body_closure_root_for_raw_cfg_batch,
            body_reachability_root_for_raw_cfg_batch,
            cfg_collection_root_for_raw_cfg_batch,
            cfgs_for_raw_batch,
            codegen_collection_root_for_raw_cfg_batch,
            runtime
        );
        let cfgs_for_optimization = cfgs.clone();
        let optimized_cfgs = register_backend_optimized_cfgs!(cfgs_for_optimization, runtime);
        let optimized_cfgs_for_batch = optimized_cfgs.clone();
        let backend_root_for_optimized_cfg_batch = backend_root.clone();
        let body_closure_root_for_optimized_cfg_batch = body_closure_root.clone();
        let body_reachability_root_for_optimized_cfg_batch = body_reachability_root.clone();
        let cfg_collection_root_for_optimized_cfg_batch = cfg_collection_root.clone();
        let codegen_collection_root_for_optimized_cfg_batch = codegen_collection_root.clone();
        let optimized_cfg_batches = register_backend_optimized_cfg_batches!(
            backend_root_for_optimized_cfg_batch,
            body_closure_root_for_optimized_cfg_batch,
            body_reachability_root_for_optimized_cfg_batch,
            cfg_collection_root_for_optimized_cfg_batch,
            codegen_collection_root_for_optimized_cfg_batch,
            optimized_cfgs_for_batch,
            runtime
        );
        let optimized_cfgs_for_codegen = optimized_cfgs.clone();
        let optimized_cfg_batches_for_codegen = optimized_cfg_batches.clone();
        #[cfg(test)]
        let codegen_evaluator_gate = Arc::new(Mutex::new(None::<Arc<TestCodegenEvaluatorGate>>));
        #[cfg(test)]
        let codegen_gate_for_evaluator = codegen_evaluator_gate.clone();
        #[cfg(test)]
        let codegen_batch_evaluator_gate =
            Arc::new(Mutex::new(None::<Arc<TestBackendBatchEvaluatorGate>>));
        #[cfg(test)]
        let codegen_batch_gate_for_evaluator = codegen_batch_evaluator_gate.clone();
        let codegen_units = register_backend_codegen_units!(
            codegen_batch_gate_for_evaluator,
            codegen_gate_for_evaluator,
            optimized_cfg_batches_for_codegen,
            optimized_cfgs_for_codegen,
            runtime
        );
        let codegen_units_for_batch = codegen_units.clone();
        let backend_root_for_codegen_batch = backend_root.clone();
        let body_closure_root_for_codegen_batch = body_closure_root.clone();
        let body_reachability_root_for_codegen_batch = body_reachability_root.clone();
        let cfg_collection_root_for_codegen_batch = cfg_collection_root.clone();
        let codegen_collection_root_for_codegen_batch = codegen_collection_root.clone();
        let codegen_unit_batches = register_backend_codegen_unit_batches!(
            backend_root_for_codegen_batch,
            body_closure_root_for_codegen_batch,
            body_reachability_root_for_codegen_batch,
            cfg_collection_root_for_codegen_batch,
            codegen_collection_root_for_codegen_batch,
            codegen_units_for_batch,
            runtime
        );
        let codegen_units_for_object_projection = codegen_units.clone();
        let object_projections =
            register_backend_object_projections!(codegen_units_for_object_projection, runtime);
        let object_projections_for_batch = object_projections.clone();
        let backend_root_for_object_projection_batch = backend_root.clone();
        let cfg_collection_root_for_object_projection_batch = cfg_collection_root.clone();
        let codegen_collection_root_for_object_projection_batch = codegen_collection_root.clone();
        let body_closure_root_for_object_projection_batch = body_closure_root.clone();
        let body_reachability_root_for_object_projection_batch = body_reachability_root.clone();
        let object_projection_batches = register_backend_object_projection_batches!(
            backend_root_for_object_projection_batch,
            body_closure_root_for_object_projection_batch,
            body_reachability_root_for_object_projection_batch,
            cfg_collection_root_for_object_projection_batch,
            codegen_collection_root_for_object_projection_batch,
            object_projections_for_batch,
            runtime
        );
        let provider_observation_meter = Arc::new(ProviderObservationCounters::default());
        let lookup_root_lease = Arc::new(Mutex::new(PublishedRootLookupLease::default()));
        let object_projections_for_backend_publication = object_projections.clone();
        let codegen_units_for_backend_publication = codegen_units.clone();
        let backend_root_for_publication = backend_root.clone();
        let cfg_collection_root_for_backend_publication = cfg_collection_root.clone();
        let codegen_collection_root_for_backend_publication = codegen_collection_root.clone();
        let body_closure_root_for_backend_publication = body_closure_root.clone();
        let body_reachability_root_for_backend_publication = body_reachability_root.clone();
        let backend_root_publications = register_backend_backend_root_publications!(
            backend_root_for_publication,
            body_closure_root_for_backend_publication,
            body_reachability_root_for_backend_publication,
            cfg_collection_root_for_backend_publication,
            codegen_collection_root_for_backend_publication,
            codegen_units_for_backend_publication,
            object_projections_for_backend_publication,
            runtime
        );
        #[cfg(test)]
        let inject_body_transaction_failure = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let transactions_for_analysis_bundle = body_transactions.clone();
        let produced_for_analysis_bundle = body_produced_anonymous.clone();
        let body_analysis_bundles = register_body_body_analysis_bundles!(
            produced_for_analysis_bundle,
            runtime,
            transactions_for_analysis_bundle
        );
        let toolchain_for_body_closure = body_toolchain_demands.clone();
        let transactions_for_body_reachability = body_transactions.clone();
        let produced_for_body_reachability = body_produced_anonymous.clone();
        let input_for_body_closure = body_input_resolver.clone();
        let declarations_for_body_closure = declaration_semantics_projection.clone();
        let type_facts_for_body_reachability = type_facts.clone();
        let call_abis_for_body_reachability = call_abis.clone();
        let drop_glues_for_body_reachability = drop_glues.clone();
        let body_reachability = register_body_body_reachability!(
            call_abis_for_body_reachability,
            declarations_for_body_closure,
            drop_glues_for_body_reachability,
            input_for_body_closure,
            produced_for_body_reachability,
            runtime,
            toolchain_for_body_closure,
            transactions_for_body_reachability,
            type_facts_for_body_reachability
        );
        let reachability_for_closure = body_reachability.clone();
        let bundles_for_closure = body_analysis_bundles.clone();
        let declarations_for_closure_aggregation = declaration_semantics_projection.clone();
        #[cfg(test)]
        let anonymous_digest_forcing_for_closure_aggregation =
            body_closure_anonymous_digest_forcing.clone();
        let body_closures = register_body_body_closures!(
            anonymous_digest_forcing_for_closure_aggregation,
            bundles_for_closure,
            declarations_for_closure_aggregation,
            reachability_for_closure,
            runtime
        );
        let closures_for_publication = body_closures.clone();
        let reachability_for_publication = body_reachability.clone();
        let names_for_closure_publication = lookup_names.clone();
        let imports_for_closure_publication = lookup_imports.clone();
        let lease_for_closure_publication = lookup_root_lease.clone();
        let terminal_root_for_closure_publication = body_closure_root.clone();
        let terminal_root_for_reachability_publication = body_reachability_root.clone();
        let terminal_root_for_declaration_publication = declaration_semantics_root.clone();
        let runtime_for_closure_publication = runtime.clone();
        let body_closure_publications = register_body_body_closure_publications!(
            closures_for_publication,
            imports_for_closure_publication,
            lease_for_closure_publication,
            names_for_closure_publication,
            reachability_for_publication,
            runtime,
            runtime_for_closure_publication,
            terminal_root_for_closure_publication,
            terminal_root_for_declaration_publication,
            terminal_root_for_reachability_publication
        );
        assert!(
            body_transaction_evaluator
                .set(BodyTransactionEvaluator {
                    parse_modules: parse_modules.clone(),
                    module_source_bases: module_source_bases.clone(),
                    body_input: body_input_resolver,
                    body_toolchain_demands: body_toolchain_demands.clone(),
                    body_produced_anonymous: body_produced_anonymous.clone(),
                    semantic_nucleus: semantic_nucleus.clone(),
                    stable_declaration_classifications: stable_declaration_classifications.clone(),
                    declaration_shells: declaration_shells.clone(),
                    lookup_names: lookup_names.clone(),
                    lookup_imports: lookup_imports.clone(),
                    provider_observation_meter: provider_observation_meter.clone(),
                    lookup_root_lease: lookup_root_lease.clone(),
                    runtime: runtime.clone(),
                    shared_durable_payloads: shared_durable_payloads.clone(),
                    symbol_space: RevisionSymbolSpace::with_owner_bound(max_interner_entries),
                    #[cfg(test)]
                    inject_body_transaction_failure: inject_body_transaction_failure.clone(),
                })
                .is_ok(),
            "BodyTransaction evaluator is installed once"
        );
        let parse = register_parse_import_parse!(runtime);
        let parse_selection = parse.selection();
        Self {
            parse,
            parse_selection,
            runtime: runtime.clone(),
            next_revision: 1,
            next_source_stamp: 1,
            source_stamps: VecDeque::new(),
            import_store,
            module_store,
            cfg_collection_root,
            codegen_collection_root,
            publication_cone_retention_failures,
            parse_stage,
            #[cfg(test)]
            test_import_store,
            #[cfg(test)]
            declaration_body_plan_failure_injection,
            parse_modules,
            parse_module_batches,
            module_source_bases,
            module_indexes,
            declaration_occurrence_indexes,
            declaration_orders,
            declaration_shells,
            #[cfg(test)]
            stable_declaration_classifications: stable_declaration_classifications.clone(),
            warning_body_references,
            warning_body_reference_batches,
            #[cfg(test)]
            body_inputs,
            body_source_bases,
            body_toolchain_demands,
            body_transactions,
            shared_durable_payloads,
            body_analysis_bundles,
            body_reachability,
            body_closures,
            body_closure_publications,
            body_reachability_meter,
            body_produced_anonymous,
            declaration_body_plan_artifacts,
            #[cfg(test)]
            declaration_body_plan_astgen_evaluations,
            resolve_imports,
            #[cfg(test)]
            declaration_imports,
            semantic_nucleus,
            declaration_semantics_publications,
            type_shapes,
            type_facts,
            layouts,
            call_abis,
            drop_glues,
            cfgs,
            raw_cfg_batches,
            optimized_cfgs,
            optimized_cfg_batches,
            codegen_units,
            codegen_unit_batches,
            object_projections,
            object_projection_batches,
            backend_root_publications,
            #[cfg(test)]
            codegen_evaluator_gate,
            #[cfg(test)]
            codegen_batch_evaluator_gate,
            lookup_names,
            lookup_imports,
            #[cfg(test)]
            module_index_build_log,
            #[cfg(test)]
            lookup_name_eval_log,
            #[cfg(test)]
            lookup_import_eval_log,
            #[cfg(test)]
            body_closure_anonymous_digest_forcing,
            next_import_request: 0,
            current_import_revision: None,
            committed_import_revision: None,
            committed_import_revision_pin: None,
            active_compatibility_token: 1,
            ordinary_lineage_published: false,
            active_import_context: None,
            #[cfg(test)]
            current_test_import_revision: None,
            import_frontier_roots_requested: 0,
            exact_import_groups_dispatched: 0,
            import_view_full_leaves_published: 0,
            import_view_overlay_leaves_published: 0,
            import_view_ledger_entries_cloned: std::sync::atomic::AtomicU64::new(0),
            import_view_source_entries_compared: std::sync::atomic::AtomicU64::new(0),
            import_view_read_entries_compared: std::sync::atomic::AtomicU64::new(0),
            identity_resolution,
            lineage_additions: Vec::new(),
            provider_observation_meter,
            lookup_root_lease,
            body_closure_root,
            body_reachability_root,
            backend_root,
            backend_root_publication_gate: BackendRootPublicationGate::default(),
            next_backend_root_epoch: std::sync::atomic::AtomicU64::new(1),
            next_optimized_cfg_batch_generation: std::sync::atomic::AtomicU64::new(1),
            #[cfg(test)]
            inject_body_transaction_failure,
            #[cfg(test)]
            provider_probe: register_provider_probe!(runtime),
        }
    }
}
