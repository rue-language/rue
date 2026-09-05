//! Rooted CFG and codegen projections of the canonical query graph.

use super::*;

/// Park-aware result for the production body-closure root. Success carries no
/// recomposed whole-program value: consumers project the query-owned reached
/// terminals they actually need.
pub enum RootedParkOutcome {
    Ready,
    Errors(CompileErrors),
    Parked(Box<crate::ParkedToolchainModules>),
}

#[derive(Clone)]
pub struct RootedCfgUnit {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) optimized_cfg_key: crate::cfg_query::OptimizedCfgQueryKey,
    pub(crate) record: Arc<crate::cfg_query::CfgRecord>,
    #[allow(dead_code)]
    pub(crate) body_span: rue_span::Span,
}

impl std::fmt::Debug for RootedCfgUnit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RootedCfgUnit")
            .field("function", &self.function)
            .field("source_name", &self.record.source_name)
            .field(
                "semantic_shape",
                &self.record.domains.stable_debug_snapshot(&self.record.air),
            )
            .field("cfg_blocks", &self.record.cfg.blocks())
            .field("num_locals", &self.record.num_locals)
            .field("num_param_slots", &self.record.num_param_slots)
            .field("strings", &self.record.strings)
            .field("codegen", &self.record.codegen)
            .finish()
    }
}

#[derive(Debug)]
pub struct RootedCfgOutput {
    pub(super) graph: RootedBodyGraph,
    pub(crate) cfgs: Vec<RootedCfgUnit>,
    pub(crate) optimized_cfg_batch: crate::revisioned_query_database::OptimizedCfgBatchKey,
    pub(crate) warnings: Vec<CompileWarning>,
    pub(crate) work: crate::CanonicalSemanticWork,
    pub(super) backend_work: BackendQueryWork,
    pub(super) backend_root: crate::revisioned_query_database::BackendRootCandidate,
}

#[derive(Clone)]
pub struct RootedPreOptimizationCfgUnit {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) cfg_key: crate::cfg_query::CfgQueryKey,
    pub(crate) record: Arc<crate::cfg_query::CfgRecord>,
}

impl std::fmt::Debug for RootedPreOptimizationCfgUnit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RootedPreOptimizationCfgUnit")
            .field("function", &self.function)
            .field("source_name", &self.record.source_name)
            .field("cfg_blocks", &self.record.cfg.blocks())
            .finish()
    }
}

#[derive(Debug)]
pub struct RootedPreOptimizationCfgOutput {
    pub(super) cfgs: Vec<RootedPreOptimizationCfgUnit>,
    pub(crate) raw_cfg_batch: crate::revisioned_query_database::RawCfgBatchKey,
    // The batch terminal owns the exact retained cones of its compiler.cfg
    // children for as long as this public artifact remains live.
    pub(super) _raw_cfg_terminal:
        Arc<rue_query::QueryTerminal<crate::revisioned_query_database::RawCfgBatchOutput>>,
    pub(super) warnings: Vec<CompileWarning>,
    pub(super) work: crate::CanonicalSemanticWork,
}

impl RootedPreOptimizationCfgOutput {
    pub fn functions(&self) -> &[RootedPreOptimizationCfgUnit] {
        &self.cfgs
    }

    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }

    pub fn metrics(&self) -> crate::unstable::SemanticMetrics {
        crate::unstable::SemanticMetrics::from_work(self.work)
    }

    pub fn query_identity(&self) -> String {
        rue_query::QueryKey::stable_identity(&self.raw_cfg_batch)
    }
}

impl RootedPreOptimizationCfgUnit {
    pub fn definition_source_name(&self) -> Option<&str> {
        match &self.function {
            crate::FunctionInstanceKey::Definition(definition) => Some(definition.name()),
            _ => None,
        }
    }

    pub fn source_name(&self) -> &str {
        self.definition_source_name()
            .unwrap_or(&self.record.source_name)
    }

    pub fn cfg(&self) -> &rue_cfg::ValidatedCfg {
        &self.record.cfg
    }

    pub fn interner(&self) -> &Arc<lasso::ThreadedRodeo> {
        &self.record.interner
    }

    pub fn type_pool(&self) -> &rue_air::FrozenTypeInternPool {
        &self.record.type_pool
    }

    pub fn strings(&self) -> &Arc<[String]> {
        &self.record.strings
    }

    pub fn query_identity(&self) -> String {
        rue_query::QueryKey::stable_identity(&self.cfg_key)
    }
}

impl RootedCfgOutput {
    pub fn functions(&self) -> &[RootedCfgUnit] {
        &self.cfgs
    }

    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }

    pub fn metrics(&self) -> crate::unstable::SemanticMetrics {
        crate::unstable::SemanticMetrics::from_work(self.work)
    }

    #[cfg(test)]
    pub(crate) fn work(&self) -> &crate::CanonicalSemanticWork {
        &self.work
    }

    pub(crate) fn declarations(&self) -> &[crate::DurableDeclarationSemantic] {
        &self.graph.declarations
    }

    /// The `pub extern "C" fn` exports this rooted set carries, each with the
    /// signature the entry thunk is generated from.
    ///
    /// This is the same [`collect_rooted_exports`] intersection the codegen
    /// projection publishes, so an ABI presentation and a real link describe one
    /// export set.
    pub(crate) fn c_export_thunks(&self) -> Vec<crate::program_image_plan::RootedExportThunk> {
        collect_rooted_exports(&self.graph, &self.cfgs)
    }

    pub(crate) fn anonymous_nominals(
        &self,
    ) -> &[crate::durable_semantics::DurableAnonymousNominal] {
        &self.graph.anonymous_nominals
    }

    #[cfg(test)]
    pub(crate) fn type_pools(
        &self,
    ) -> impl ExactSizeIterator<Item = &rue_air::FrozenTypeInternPool> {
        self.cfgs.iter().map(|function| &function.record.type_pool)
    }

    #[cfg(test)]
    pub(crate) fn type_pool_stats(&self) -> Vec<rue_air::TypeInternPoolStats> {
        self.type_pools().map(|pool| pool.stats()).collect()
    }

    pub(crate) fn string_domains(&self) -> impl ExactSizeIterator<Item = &[String]> {
        self.cfgs
            .iter()
            .map(|function| function.record.strings.as_ref())
    }
}

impl RootedCfgUnit {
    pub fn definition_source_name(&self) -> Option<&str> {
        match &self.function {
            crate::FunctionInstanceKey::Definition(definition) => Some(definition.name()),
            _ => None,
        }
    }

    pub fn source_name(&self) -> &str {
        self.definition_source_name()
            .unwrap_or(&self.record.source_name)
    }

    pub fn air(&self) -> &rue_air::ValidatedAir {
        &self.record.air
    }

    pub fn cfg(&self) -> &rue_cfg::ValidatedCfg {
        &self.record.cfg
    }

    pub fn interner(&self) -> &Arc<lasso::ThreadedRodeo> {
        &self.record.interner
    }

    pub fn type_pool(&self) -> &rue_air::FrozenTypeInternPool {
        &self.record.type_pool
    }

    pub fn strings(&self) -> &Arc<[String]> {
        &self.record.strings
    }
}

impl RootedCfgUnit {
    #[cfg(test)]
    pub(crate) fn legacy_name(&self) -> &str {
        self.record
            .codegen
            .symbol_mappings
            .iter()
            .find_map(|(source, target)| {
                (target.as_str() == self.record.codegen.defined_symbol.as_ref())
                    .then_some(source.as_str())
            })
            .unwrap_or(self.record.source_name.as_ref())
    }
}

pub(crate) struct RootedCodegenOutput {
    pub(crate) input: RootedCodegenInput,
    pub(crate) units: Vec<crate::codegen_query::CollectedCodegenUnit>,
    pub(crate) objects: Vec<crate::object_query::CollectedObjectProjection>,
    #[allow(dead_code)]
    pub(crate) cfgs: Vec<RootedCfgUnit>,
    pub(crate) exports: Vec<crate::program_image_plan::RootedExportThunk>,
    pub(crate) warnings: Vec<CompileWarning>,
    pub(crate) work: crate::CanonicalSemanticWork,
    pub(crate) cfg_work: BackendQueryWork,
    pub(crate) codegen_work: BackendQueryWork,
    pub(crate) object_projection_work: BackendQueryWork,
}

pub(crate) enum RootedCodegenInput {
    Structured,
    Projected,
}

/// Opaque in-crate continuation between the canonical codegen-ready and
/// objects-ready endpoints. The unpublished backend-root candidate keeps the
/// exact CFG and CodegenUnit cones protected until object projection can
/// atomically publish their successor root.
pub(crate) struct RootedCodegenReadyOutput {
    pub(super) graph: RootedBodyGraph,
    pub(super) units: Vec<crate::codegen_query::CollectedCodegenUnit>,
    pub(super) cfgs: Vec<RootedCfgUnit>,
    pub(super) warnings: Vec<CompileWarning>,
    pub(crate) work: crate::CanonicalSemanticWork,
    pub(crate) cfg_work: BackendQueryWork,
    pub(crate) codegen_work: BackendQueryWork,
    pub(super) backend_root: crate::revisioned_query_database::BackendRootCandidate,
    pub(super) codegen_batch_key: crate::revisioned_query_database::CodegenUnitBatchKey,
}

#[derive(Debug, Clone)]
pub(super) struct RootedBodyGraph {
    pub(super) revision: rue_query::Revision,
    pub(super) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    pub(super) declarations: Arc<[crate::DurableDeclarationSemantic]>,
    pub(super) declaration_index:
        Arc<crate::local_semantic_materialization::SharedDeclarationFactIndex>,
    pub(super) anonymous_nominals: Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
    pub(super) declaration_dependencies:
        Arc<[crate::semantic_query_nucleus::SemanticDeclarationDependency]>,
    pub(super) c_export_roots: Arc<[crate::revisioned_query_database::DurableCExportRoot]>,
    pub(super) modules: Arc<[Arc<crate::parsed_modules::ParsedModule>]>,
    /// The program entry point, present only for a `RootSelection::Executable`
    /// request. A test request has no entry point (ADR-0083 §1).
    pub(super) main: Option<crate::StableDefinitionKey>,
    /// The exact root set this graph was analyzed under — the single authority
    /// consumers use instead of re-deriving roots from `main` and
    /// `c_export_roots`.
    pub(super) roots: Arc<[crate::FunctionInstanceKey]>,
    /// The request's tests in stable-ID order, empty unless this is a
    /// `RootSelection::Tests` graph (ADR-0083 §2).
    ///
    /// Computed once here so the `--list` surface and the test image's
    /// dispatcher ordinals are the same table rather than two sorts of the
    /// same declarations.
    pub(super) test_inventory: Arc<[crate::test_inventory::RootedTest]>,
    pub(super) closure: crate::body_query::BodyClosureOutput,
    pub(super) work: crate::CanonicalSemanticWork,
}

/// The C-ABI export thunks a request's image must carry.
///
/// `extern "C"` exports are executable-only. The nucleus records them for every
/// request, but only `RootSelection::Executable` roots them (ADR-0083 §1), so a
/// test request produces no CFG unit for one and this intersection is empty by
/// construction. A test image is entered through the synthesized dispatcher and
/// exposes nothing else; if a future selection should link exports alongside
/// tests, that belongs in the root-set authority, not here.
pub(super) fn collect_rooted_exports(
    graph: &RootedBodyGraph,
    cfgs: &[RootedCfgUnit],
) -> Vec<crate::program_image_plan::RootedExportThunk> {
    let export_roots = graph
        .c_export_roots
        .iter()
        .map(|export| {
            (
                crate::FunctionInstanceKey::Definition(export.key.clone()),
                export.convention,
            )
        })
        .collect::<BTreeMap<_, _>>();
    cfgs.iter()
        .filter_map(|cfg| {
            let convention = *export_roots.get(&cfg.function)?;
            Some(crate::program_image_plan::RootedExportThunk {
                function: cfg.function.clone(),
                exported_symbol: match &cfg.function {
                    crate::FunctionInstanceKey::Definition(key) => key.name().to_owned(),
                    _ => unreachable!("C export roots are source definitions"),
                },
                native_symbol: cfg.record.codegen.defined_symbol.to_string(),
                signature: export_signature(cfg, convention),
            })
        })
        .collect()
}

/// One export's complete ABI description, projected from its optimized CFG.
///
/// The parameter types come from the one AIR recovery the callee's own
/// parameter layout was derived from (`rue_air::body_parameter_types`), so the
/// thunk marshals exactly the values the native body expects — including a
/// parameter the body never reads, which has no `Param` instruction to recover a
/// type from. A C export's parameters are all by value (semantic analysis
/// rejects `borrow`/`inout` in an export signature), so every one carries a
/// type.
pub(crate) fn export_signature(
    cfg: &RootedCfgUnit,
    convention: rue_target::CallingConvention,
) -> rue_codegen::export_thunk::ExportSignature {
    let types = rue_air::body_parameter_types(&cfg.record.air);
    let parameter_types = cfg
        .record
        .cfg
        .source_param_abi()
        .iter()
        .map(|parameter| {
            parameter
                .ty
                .or_else(|| types.get(&parameter.start_slot).copied())
                .expect(
                    "a C export's by-value parameter carries a source type in its analyzed body",
                )
        })
        .collect::<Vec<_>>();
    rue_codegen::export_thunk::ExportSignature::for_types(
        &cfg.record.type_pool,
        convention,
        &parameter_types,
        cfg.record.cfg.return_type(),
    )
}

pub(super) fn sort_rooted_warnings(graph: &RootedBodyGraph, warnings: &mut Vec<CompileWarning>) {
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
}
