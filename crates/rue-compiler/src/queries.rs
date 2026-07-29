use rue_air::TypeKind;
use tracing::{info, info_span};

use crate::*;

/// A borrowed source record returned by [`SourceSnapshot::files`].
///
/// Snapshot construction uses owned source buffers; this view exists for
/// diagnostics and presentation consumers that need the associated path and
/// request-local file ID without copying source text.
pub struct SourceView<'a> {
    /// Path to the source file (used for error messages).
    pub path: &'a str,
    /// Source code content.
    pub source: &'a str,
    /// Unique identifier for this file.
    pub file_id: FileId,
}

impl<'a> SourceView<'a> {
    pub(crate) fn new(path: &'a str, source: &'a str, file_id: FileId) -> Self {
        Self {
            path,
            source,
            file_id,
        }
    }
}

/// Which linker to use for the final linking phase.
///
/// The Rue compiler can either use its built-in ELF linker or delegate to
/// an external system linker like `clang`, `gcc`, or `ld`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkerMode {
    /// Use the internal linker (default).
    Internal,
    /// Use an external system linker (e.g., "clang", "ld", "gcc").
    System(String),
}

impl Default for LinkerMode {
    fn default() -> Self {
        LinkerMode::Internal
    }
}

/// Configuration options for compilation.
///
/// Controls target architecture, linker selection, optimization level, and feature flags.
///
/// # Example
///
/// ```ignore
/// let options = CompileOptions {
///     target: Target::host().unwrap(),
///     linker: LinkerMode::Internal,
///     opt_level: OptLevel::O1,
///     preview_features: PreviewFeatures::new(),
/// };
/// let snapshot = SourceSnapshot::single("main.rue", source)?;
/// let output = compile_snapshot(&snapshot, &options)?;
/// ```
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// The target architecture and OS.
    pub target: Target,
    /// Which linker to use.
    pub linker: LinkerMode,
    /// Optimization level.
    pub opt_level: OptLevel,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
    /// Static archives (`.a`) supplied on the command line with
    /// `--link-archive` (ADR-0064 C FFI). The linker resolves undefined
    /// `extern "C"` symbols from these members. Linking-only: this does not
    /// participate in the semantic or codegen cache identity.
    pub link_archives: Vec<std::path::PathBuf>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            target: Target::host()
                .expect("Rue cannot choose a default compile target on this unsupported host"),
            linker: LinkerMode::Internal,
            opt_level: OptLevel::default(),
            preview_features: PreviewFeatures::new(),
            link_archives: Vec::new(),
        }
    }
}

/// A function with its typed IR (AIR) and control flow graph (CFG).
///
/// This combines the output of semantic analysis with CFG construction.
#[derive(Clone)]
pub struct FunctionWithCfg {
    /// The analyzed function from semantic analysis.
    pub analyzed: std::sync::Arc<AnalyzedFunction>,
    /// Durable semantic identity retained independently from machine naming.
    pub semantic_identity: crate::FunctionInstanceKey,
    /// Typed symbol projection selected by the compiler authority.
    pub symbol: crate::StableSymbolId,
    /// Stable occurrence identities for local data owned by this function.
    /// Dense IDs remain a current string-table projection consumed by codegen.
    pub local_atoms: Vec<rue_air::LocalAtomRecord<crate::StableDefinitionKey, crate::ModuleId>>,
    /// Final object/linker symbol. Root source `main` is the documented
    /// ProgramEntry ABI alias; all other source/glue bodies use the encoder.
    pub machine_name: String,
    /// Request-local live symbol used only to resolve pre-projection AIR and
    /// cleanup metadata through the authoritative mapping.
    pub(crate) legacy_name: String,
    /// Stable semantic/ABI/CFG content identity used by the per-function
    /// codegen terminal. It intentionally excludes current interner and type
    /// pool indexes while retaining every exact input observed by Cfg.
    pub(crate) optimized_cfg_key: crate::cfg_query::OptimizedCfgQueryKey,
    /// The control flow graph built from the AIR.
    pub cfg: Cfg,
}

impl std::fmt::Debug for FunctionWithCfg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The optimized query key contains request-local relocation domains;
        // it is an execution dependency, not semantic presentation state.
        formatter
            .debug_struct("FunctionWithCfg")
            .field("analyzed", &self.analyzed)
            .field("semantic_identity", &self.semantic_identity)
            .field("symbol", &self.symbol)
            .field("local_atoms", &self.local_atoms)
            .field("machine_name", &self.machine_name)
            .field("legacy_name", &self.legacy_name)
            .field("cfg", &self.cfg)
            .finish()
    }
}

/// Intermediate compilation state after frontend processing.
///
/// This allows inspection of the IR at each stage, useful for
/// debugging and the `--emit` CLI flags.
#[cfg(test)]
pub(crate) struct CompileState {
    /// String interner used during compilation.
    pub interner: ThreadedRodeo,
    /// Analyzed functions with typed IR and control flow graphs.
    pub functions: Vec<FunctionWithCfg>,
    /// Type intern pool containing all struct and enum definitions.
    pub type_pool: FrozenTypeInternPool,
    /// String literals referenced by test CFGs.
    pub strings: Vec<String>,
    /// Warnings collected during compilation.
    pub warnings: Vec<CompileWarning>,
}

/// Frontend artifacts after semantic analysis has been lowered to CFGs.
pub(crate) struct CfgFrontendOutput {
    /// Analyzed functions paired with their optimized CFGs.
    pub(crate) functions: Vec<FunctionWithCfg>,
    /// Type intern pool containing all struct and enum definitions.
    pub(crate) type_pool: FrozenTypeInternPool,
    /// String literals indexed by their AIR string_const index.
    pub(crate) strings: Vec<String>,
    /// Warnings collected during semantic analysis and CFG construction.
    pub(crate) warnings: Vec<CompileWarning>,
    pub(crate) implicit_named_destructor_dependencies:
        Vec<rue_air::ImplicitNamedDestructorDependencyEvent>,
    pub(crate) implicit_named_destructor_dependencies_complete: bool,
    pub(crate) work: canonical_semantic::CfgConstructionWork,
}

pub(crate) struct CfgConstructionFailure {
    pub(crate) errors: CompileErrors,
    pub(crate) work: canonical_semantic::CfgConstructionWork,
}

/// Prepare reached functions and collect their canonical per-function
/// `OptimizedCfg` terminals. This function owns deterministic batch assembly,
/// not CFG construction or optimization.
pub(crate) fn collect_function_cfg_queries(
    sema_output: SemaOutput,
    demanded_drop_glue: &std::collections::BTreeSet<
        rue_air::TypeInstanceKey<rue_air::SemanticDefinitionToken, rue_air::SemanticModuleToken>,
    >,
    demanded_drop_glue_plans: &std::collections::BTreeMap<
        rue_air::TypeInstanceKey<rue_air::SemanticDefinitionToken, rue_air::SemanticModuleToken>,
        crate::type_queries::DropGlueFacts<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
    >,
    stable_drop_glue_plans: &std::collections::BTreeMap<
        crate::TypeInstanceKey,
        crate::type_queries::DropGlueFacts,
    >,
    opt_level: OptLevel,
    interner: std::sync::Arc<ThreadedRodeo>,
    stable_inputs: &[crate::cfg_query::CfgBodyInput],
    stable_aggregate_types: std::collections::HashMap<Type, crate::TypeInstanceKey>,
    projected_identities: &std::collections::BTreeMap<
        rue_air::FunctionInstanceKey<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
        crate::FunctionInstanceKey,
    >,
    cfg_queries: &crate::revisioned_query_database::RevisionedQueryDatabase,
    revision: rue_query::Revision,
    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    cancellation: rue_query::CancellationToken,
) -> Result<CfgFrontendOutput, CfgConstructionFailure> {
    let SemaOutput {
        functions,
        strings,
        mut warnings,
        type_pool,
        aggregate_type_identities_by_type: _,
        aggregate_types_by_identity,
        body_analysis_work: _,
        ..
    } = sema_output;

    // Synthesize drop glue functions.
    let drop_glue_functions = drop_glue::synthesize_demanded_drop_glue(
        &type_pool,
        &aggregate_types_by_identity,
        demanded_drop_glue.iter().cloned(),
        demanded_drop_glue_plans,
    )
    .map_err(|error| CfgConstructionFailure {
        errors: error.into(),
        work: canonical_semantic::CfgConstructionWork::default(),
    })?;
    let mut work = canonical_semantic::CfgConstructionWork {
        drop_glue_functions_synthesized: drop_glue_functions.len(),
        functions_considered: functions.len() + drop_glue_functions.len(),
        comptime_functions_filtered: functions
            .iter()
            .filter(|f| f.air.return_type() == Type::COMPTIME_TYPE)
            .count(),
        ..Default::default()
    };

    // Combine user functions with drop glue, filtering out comptime-only functions.
    let mut all_functions: Vec<_> = functions
        .into_iter()
        .filter(|f| f.air.return_type() != Type::COMPTIME_TYPE)
        .chain(drop_glue_functions)
        .collect();
    let mut legacy_to_machine = std::collections::BTreeMap::<String, String>::new();
    let mut legacy_to_stable =
        std::collections::BTreeMap::<String, crate::FunctionInstanceKey>::new();
    let mut canonical_owners =
        std::collections::BTreeMap::<String, crate::FunctionInstanceKey>::new();
    let mut projected = Vec::with_capacity(all_functions.len());
    for function in all_functions.drain(..) {
        let semantic_identity = projected_identities
            .get(&function.identity)
            .cloned()
            .ok_or_else(|| CfgConstructionFailure {
                errors: CompileError::without_span(ErrorKind::InternalError(format!(
                    "callable '{}' has no canonical identity projection",
                    function.name
                )))
                .into(),
                work: work.clone(),
            })?;
        let mut local_atoms = function
            .local_atoms
            .iter()
            .map(|atom| {
                if atom.identity.producer != function.identity
                    || strings.get(atom.dense_id as usize).map(String::as_str)
                        != Some(atom.content.as_ref())
                {
                    return Err(CfgConstructionFailure {
                        errors: CompileError::without_span(ErrorKind::InternalError(format!(
                            "callable '{}' has an invalid local atom projection",
                            function.name
                        )))
                        .into(),
                        work: work.clone(),
                    });
                }
                Ok(rue_air::LocalAtomRecord {
                    identity: rue_air::LocalAtomId {
                        producer: semantic_identity.clone(),
                        kind: atom.identity.kind,
                        anchor: atom.identity.anchor.clone(),
                    },
                    content: atom.content.clone(),
                    dense_id: atom.dense_id,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        local_atoms.sort_by(|left, right| left.identity.cmp(&right.identity));
        if local_atoms
            .windows(2)
            .any(|pair| pair[0].identity == pair[1].identity)
        {
            return Err(CfgConstructionFailure {
                errors: CompileError::without_span(ErrorKind::InternalError(format!(
                    "callable '{}' has duplicate local atom identities",
                    function.name
                )))
                .into(),
                work,
            });
        }
        let is_program_entry = function.name == "main";
        let symbol = if is_program_entry {
            crate::StableSymbolId::Callable(crate::StableCallableId::Compiler(
                crate::CompilerCallableId::ProgramEntry,
            ))
        } else {
            crate::StableSymbolId::Callable(crate::StableCallableId::Function(
                semantic_identity.clone(),
            ))
        };
        // `main` is the platform entry ABI alias of the root source function;
        // the function record still retains `semantic_identity` above.
        let machine_name = if is_program_entry {
            "main".to_owned()
        } else {
            crate::StableSymbolEncoder::encode(&symbol)
        };
        if let Some(previous) =
            legacy_to_machine.insert(function.name.clone(), machine_name.clone())
            && previous != machine_name
        {
            return Err(CfgConstructionFailure {
                errors: CompileError::without_span(ErrorKind::InternalError(format!(
                    "live callable symbol '{}' fans out to '{}' and '{}'",
                    function.name, previous, machine_name
                )))
                .into(),
                work,
            });
        }
        legacy_to_stable.insert(function.name.clone(), semantic_identity.clone());
        if let Some(previous) =
            canonical_owners.insert(machine_name.clone(), semantic_identity.clone())
            && previous != semantic_identity
        {
            return Err(CfgConstructionFailure {
                errors: CompileError::without_span(ErrorKind::InternalError(format!(
                    "canonical machine symbol collision at '{machine_name}'"
                )))
                .into(),
                work,
            });
        }
        projected.push((
            function,
            semantic_identity,
            symbol,
            local_atoms,
            machine_name,
        ));
    }
    // AIR and CFG remain source-semantic artifacts. Their live call names are
    // resolved through `legacy_to_machine` only at the codegen boundary; this
    // keeps presentation and durable CFG reuse independent of interner slots.
    let mut all_functions = projected;
    // Function order controls CFG collection, object-file order, and final
    // linker layout. Machine symbols are the stable semantic
    // identity shared by user, specialized, destructor, and glue functions.
    all_functions.sort_by(|left, right| left.4.cmp(&right.4));

    let _span = info_span!("cfg_collection", phase = "cfg_query_collection").entered();
    let aggregate_types = std::sync::Arc::new(stable_aggregate_types);
    let results: Vec<_> = all_functions
        .into_iter()
        .map(
            |(func, semantic_identity, symbol, local_atoms, machine_name)| {
                let legacy_name = func.name.clone();
                let current_input = stable_inputs
                    .binary_search_by(|input| input.function.cmp(&semantic_identity))
                    .ok()
                    .map(|index| &stable_inputs[index]);
                let body_span = current_input.map(|input| input.body_span).or_else(|| {
                    let mut instructions = func.air.iter().map(|(_, instruction)| instruction.span);
                    let first = instructions.next()?;
                    Some(instructions.fold(first, |span, next| rue_span::Span {
                        file_id: span.file_id,
                        start: span.start.min(next.start),
                        end: span.end.max(next.end),
                    }))
                });
                let Some(body_span) = body_span else {
                    return Err((
                        CompileError::without_span(ErrorKind::InternalError(format!(
                            "callable '{}' has no CFG source span",
                            func.name
                        )))
                        .into(),
                        canonical_semantic::CfgConstructionWork::default(),
                    ));
                };
                let semantic_input = if let Some(input) = current_input {
                    crate::cfg_query::CfgSemanticInput::Body(std::sync::Arc::new(
                        input.body.clone(),
                    ))
                } else if let crate::FunctionInstanceKey::DropGlue(owner) = &semantic_identity {
                    let Some(facts) = stable_drop_glue_plans.get(owner.as_ref()) else {
                        return Err((
                            CompileError::without_span(ErrorKind::InternalError(format!(
                                "drop glue {:?} has no canonical query plan",
                                owner
                            )))
                            .into(),
                            canonical_semantic::CfgConstructionWork::default(),
                        ));
                    };
                    crate::cfg_query::CfgSemanticInput::DropGlue {
                        owner: owner.as_ref().clone(),
                        facts: Box::new(facts.clone()),
                    }
                } else {
                    return Err((
                        CompileError::without_span(ErrorKind::InternalError(format!(
                            "callable '{}' has no canonical CFG semantic input",
                            func.name
                        )))
                        .into(),
                        canonical_semantic::CfgConstructionWork::default(),
                    ));
                };
                let func = std::sync::Arc::new(func);
                let domains = match current_input
                    .map(|input| {
                        crate::durable_cfg::CfgDomainProjection::from_body(
                            &func,
                            &input.function,
                            &input.body,
                            body_span,
                            &strings,
                            &type_pool,
                            &interner,
                            |ty| {
                                crate::durable_cfg::canonical_type_from_live(
                                    ty,
                                    &type_pool,
                                    &aggregate_types,
                                )
                            },
                            |symbol| legacy_to_stable.get(interner.resolve(&symbol)).cloned(),
                        )
                    })
                    .unwrap_or_else(|| {
                        crate::durable_cfg::CfgDomainProjection::from_canonical_function(
                            &func,
                            &semantic_identity,
                            body_span,
                            &strings,
                            &interner,
                            |ty| {
                                crate::durable_cfg::canonical_type_from_live(
                                    ty,
                                    &type_pool,
                                    &aggregate_types,
                                )
                            },
                            |symbol| legacy_to_stable.get(interner.resolve(&symbol)).cloned(),
                        )
                    }) {
                    Ok(domains) => domains,
                    Err(failure) => {
                        return Err((
                            CompileError::new(
                                ErrorKind::InternalError(format!(
                                    "canonical CFG domain projection failed: {failure:?}"
                                )),
                                body_span,
                            )
                            .into(),
                            canonical_semantic::CfgConstructionWork::default(),
                        ));
                    }
                };
                #[cfg(test)]
                let mut domains = domains;
                #[cfg(test)]
                if crate::canonical_semantic::take_test_cfg_import_failure() {
                    domains.force_import_failure_for_test();
                }
                let live = std::sync::Arc::new(crate::cfg_query::CfgLiveInput {
                    function: func.clone(),
                    type_pool: type_pool.clone(),
                    interner: interner.clone(),
                    domains: domains.clone(),
                    body_span,
                    aggregate_types: aggregate_types.clone(),
                    implicit_destructor_dependencies_complete: !matches!(
                        func.implicit_drop_source,
                        Some(rue_air::ImplicitDropDependencySourceEvent::Anonymous)
                    ),
                });
                let (optimized_cfg_key, attempt) = cfg_queries
                    .optimized_cfg(
                        revision,
                        semantic_identity.clone(),
                        configuration.clone(),
                        semantic_input,
                        live,
                        opt_level,
                        cancellation.clone(),
                    )
                    .map_err(|abort| {
                        (
                            CompileError::without_span(ErrorKind::InternalError(format!(
                                "CFG query aborted: {abort:?}"
                            )))
                            .into(),
                            canonical_semantic::CfgConstructionWork::default(),
                        )
                    })?;
                let optimized_execution = attempt.execution();
                let direct_work = |name: &str| {
                    attempt
                        .work()
                        .iter()
                        .find_map(|(kind, count)| {
                            (kind.as_ref() == name).then_some(*count as usize)
                        })
                        .unwrap_or(0)
                };
                let fallback_builds = direct_work("cfg.build.attempts");
                let fallback_build_successes = direct_work("cfg.build.successes");
                let fallback_build_failures = direct_work("cfg.build.failures");
                let import_attempts = direct_work("cfg.import.attempts");
                let import_successes = direct_work("cfg.import.successes");
                let import_failures = direct_work("cfg.import.failures");
                let cfg_fallbacks = direct_work("cfg.fallbacks");
                let cfg_execution = attempt
                    .nested_attempts()
                    .iter()
                    .find(|attempt| attempt.node().family() == "compiler.cfg")
                    .map(rue_query::NestedQueryAttempt::execution);
                let terminal = attempt.into_result().map_err(|abort| {
                    (
                        CompileError::without_span(ErrorKind::InternalError(format!(
                            "optimized CFG query aborted: {abort:?}"
                        )))
                        .into(),
                        canonical_semantic::CfgConstructionWork::default(),
                    )
                })?;
                let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                    unreachable!("OptimizedCfg publishes typed values")
                };
                let mut function_work = canonical_semantic::CfgConstructionWork::default();
                if cfg_execution == Some(rue_query::RequestExecution::Computed) {
                    function_work.cfg_builds_attempted = 1;
                    function_work.cfg_builds_succeeded =
                        usize::from(matches!(value, crate::cfg_query::CfgValue::Available(_)));
                    function_work.cfg_builds_failed = 1 - function_work.cfg_builds_succeeded;
                    function_work.air_instructions_consumed = func.air.instructions().len();
                } else if fallback_builds != 0 {
                    function_work.cfg_reuse_candidates = 1;
                    function_work.cfg_builds_attempted = fallback_builds;
                    function_work.cfg_builds_succeeded = fallback_build_successes;
                    function_work.cfg_builds_failed = fallback_build_failures;
                    function_work.air_instructions_consumed =
                        func.air.instructions().len() * fallback_builds;
                    function_work.cfg_import_attempts = import_attempts;
                    function_work.cfg_import_successes = import_successes;
                    function_work.cfg_import_failures = import_failures;
                    function_work.cfg_fallbacks = cfg_fallbacks;
                } else {
                    function_work.cfg_reuses = 1;
                    // Preserve the existing reuse accounting when no relocation
                    // was required; exact relocation work, when present, comes
                    // from the optimized evaluator's direct work record.
                    function_work.cfg_import_attempts = import_attempts.max(1);
                    function_work.cfg_import_successes = import_successes.max(1);
                }
                if optimized_execution == rue_query::RequestExecution::Computed
                    && matches!(value, crate::cfg_query::CfgValue::Available(_))
                {
                    function_work.optimization_attempts = 1;
                    function_work.optimization_completions =
                        usize::from(matches!(value, crate::cfg_query::CfgValue::Available(_)));
                    function_work.optimized_level_attempts = usize::from(opt_level != OptLevel::O0);
                }
                match value {
                    crate::cfg_query::CfgValue::Failure {
                        errors,
                        body_span: old_span,
                        ..
                    } => Err((
                        crate::cfg_query::import_errors(errors, *old_span, body_span),
                        function_work,
                    )),
                    crate::cfg_query::CfgValue::Available(record) => {
                        let cfg = if record.domains.same_live_domain(&domains) {
                            record.cfg.clone()
                        } else {
                            crate::durable_cfg::CfgDomainProjection::import_cfg(
                                &record.domains,
                                &domains,
                                &record.cfg,
                                body_span,
                            )
                            .and_then(|editor| {
                                editor
                                    .finish_after_optimization(&type_pool)
                                    .map_err(|_| crate::durable_cfg::CfgDomainFailure::Shape)
                            })
                            .map_err(|failure| {
                                (
                                    CompileError::new(
                                        ErrorKind::InternalError(format!(
                                            "CFG terminal relocation failed: {failure:?}"
                                        )),
                                        body_span,
                                    )
                                    .into(),
                                    function_work.clone(),
                                )
                            })?
                        };
                        let func_warnings = crate::cfg_query::import_warnings(
                            &record.warnings,
                            record.body_span,
                            body_span,
                        );
                        function_work.cfg_warnings_emitted = func_warnings.len();
                        function_work.implicit_destructor_targets_emitted =
                            record.implicit_destructor_targets.len();
                        let mut implicit_edges = Vec::new();
                        if let Some(source) = &func.implicit_drop_source {
                            for target in record.implicit_destructor_targets.iter() {
                                let live_type = aggregate_types
                                    .iter()
                                    .find_map(|(live, stable)| (stable == target).then_some(*live));
                                if let Some(TypeKind::Struct(id)) = live_type.map(|ty| ty.kind()) {
                                    let target = type_pool.struct_def(id);
                                    implicit_edges.push(
                                        rue_air::ImplicitNamedDestructorDependencyEvent {
                                            source: source.clone(),
                                            target_file: target.file_id.index(),
                                            target_owner_name: target.name.clone(),
                                        },
                                    );
                                }
                            }
                        }
                        Ok((
                            (
                                FunctionWithCfg {
                                    analyzed: func,
                                    semantic_identity,
                                    symbol,
                                    local_atoms,
                                    machine_name,
                                    legacy_name,
                                    optimized_cfg_key,
                                    cfg,
                                },
                                func_warnings,
                                implicit_edges,
                                record.implicit_destructor_dependencies_complete,
                            ),
                            function_work,
                        ))
                    }
                }
            },
        )
        .collect();

    let mut functions = Vec::with_capacity(results.len());
    let mut implicit_named_destructor_dependencies = Vec::new();
    let mut implicit_named_destructor_dependencies_complete = true;
    let mut first_errors = None;
    for result in results {
        let (output, function_work) = match result {
            Ok((output, function_work)) => (Some(output), function_work),
            Err((errors, function_work)) => {
                if first_errors.is_none() {
                    first_errors = Some(errors);
                }
                (None, function_work)
            }
        };
        work.cfg_builds_attempted += function_work.cfg_builds_attempted;
        work.cfg_builds_succeeded += function_work.cfg_builds_succeeded;
        work.cfg_builds_failed += function_work.cfg_builds_failed;
        work.air_instructions_consumed += function_work.air_instructions_consumed;
        work.optimization_attempts += function_work.optimization_attempts;
        work.optimization_completions += function_work.optimization_completions;
        work.optimized_level_attempts += function_work.optimized_level_attempts;
        work.cfg_warnings_emitted += function_work.cfg_warnings_emitted;
        work.implicit_destructor_targets_emitted +=
            function_work.implicit_destructor_targets_emitted;
        work.cfg_reuse_candidates += function_work.cfg_reuse_candidates;
        work.cfg_import_attempts += function_work.cfg_import_attempts;
        work.cfg_import_successes += function_work.cfg_import_successes;
        work.cfg_import_failures += function_work.cfg_import_failures;
        work.cfg_schema_version_rejections += function_work.cfg_schema_version_rejections;
        work.cfg_reuses += function_work.cfg_reuses;
        work.cfg_fallbacks += function_work.cfg_fallbacks;
        work.cfg_warnings_reused += function_work.cfg_warnings_reused;
        work.implicit_destructor_targets_reused += function_work.implicit_destructor_targets_reused;
        work.cfg_export_attempts += function_work.cfg_export_attempts;
        work.cfg_export_successes += function_work.cfg_export_successes;
        work.cfg_export_rejections += function_work.cfg_export_rejections;
        if let Some((func, func_warnings, mut implicit_edges, complete)) = output {
            functions.push(func);
            warnings.extend(func_warnings);
            implicit_named_destructor_dependencies.append(&mut implicit_edges);
            implicit_named_destructor_dependencies_complete &= complete;
        }
    }
    if let Some(errors) = first_errors {
        return Err(CfgConstructionFailure { errors, work });
    }
    implicit_named_destructor_dependencies.sort();
    implicit_named_destructor_dependencies.dedup();

    info!(
        function_count = functions.len(),
        "CFG construction complete"
    );

    Ok(CfgFrontendOutput {
        functions,
        type_pool,
        strings,
        warnings,
        implicit_named_destructor_dependencies,
        implicit_named_destructor_dependencies_complete,
        work,
    })
}

/// Source-volume and phase-work metrics collected by one-shot compilation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStats {
    pub files: usize,
    pub bytes: usize,
    pub lines: usize,
    pub tokens: usize,
}

/// Composable structural work from the session query graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineWork {
    pub parsed: ParsedModulesWork,
    pub merged: CanonicalMergeWork,
    pub lowered: CanonicalRirWork,
    pub semantic: CanonicalSemanticWork,
}

/// Output from successful one-shot compilation.
///
/// Contains the compiled executable binary and any warnings generated
/// during compilation. The binary format depends on the target platform
/// (ELF for Linux, Mach-O for macOS).
#[derive(Debug)]
pub struct CompileOutput {
    /// The compiled ELF binary.
    pub elf: Vec<u8>,
    /// Warnings generated during compilation.
    pub warnings: Vec<CompileWarning>,
    /// Source volume measured while executing the session queries.
    pub(crate) source_stats: SourceStats,
    /// Structural work performed by the session query graph.
    pub(crate) work: PipelineWork,
}

impl CompileOutput {
    /// Return instrumentation from this one-shot compilation without exposing
    /// query-engine work records.
    pub fn unstable_metrics(&self) -> crate::unstable::OneShotMetrics {
        crate::unstable::OneShotMetrics::new(self.source_stats, self.work)
    }
}

/// Compile an immutable owned source snapshot through a one-shot canonical
/// frontend session, then through the existing backend and linker boundary.
pub fn compile_snapshot(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let total_source_bytes: usize = snapshot.files().map(|source| source.source.len()).sum();
    let _span = info_span!(
        "compile",
        target = %options.target,
        file_count = snapshot.len(),
        source_bytes = total_source_bytes
    )
    .entered();
    compile_snapshot_impl(snapshot, options)
}

/// Compile the closed-valid discovery revision already adopted by `session`.
///
/// This preserves the compiler-owned import graph and captured discovery
/// context instead of reconstructing a peer frontend from source bytes.
impl CompilerSession {
    /// Run the fresh backend tail for the exact published snapshot used by the
    /// cold-versus-reused differential oracle, including direct no-discovery
    /// sessions. Production filesystem callers use [`Self::executable`].
    pub(crate) fn oracle_executable(
        &mut self,
        snapshot: &SourceSnapshot,
        options: &CompileOptions,
    ) -> MultiErrorResult<CompileOutput> {
        compile_with_session(self, snapshot, options)
    }

    /// Produce an executable from this session's closed-valid discovery revision.
    pub fn executable(&mut self, options: &CompileOptions) -> MultiErrorResult<CompileOutput> {
        let snapshot = self.committed_snapshot_for_executable()?;
        let total_source_bytes: usize = snapshot.files().map(|source| source.source.len()).sum();
        let _span = info_span!(
            "compile",
            target = %options.target,
            file_count = snapshot.len(),
            source_bytes = total_source_bytes
        )
        .entered();
        compile_with_session(self, &snapshot, options)
    }

    /// Produce an executable while the caller's canonical `compile` span is
    /// entered.
    ///
    /// The filesystem driver uses this after import discovery so the exact
    /// discovery parse and the later query pipeline share one timing root.
    /// Other callers should use [`Self::executable`], which owns that root.
    pub(crate) fn executable_in_compile_scope(
        &mut self,
        options: &CompileOptions,
    ) -> MultiErrorResult<CompileOutput> {
        let snapshot = self.committed_snapshot_for_executable()?;
        compile_with_session(self, &snapshot, options)
    }

    fn committed_snapshot_for_executable(&self) -> MultiErrorResult<SourceSnapshot> {
        let snapshot = self
            .committed_import_discovery_artifact()
            .ok_or_else(|| {
                CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                    "compilation requires a closed-valid import discovery revision".into(),
                )))
            })?
            .snapshot()
            .clone();
        Ok(snapshot)
    }
}

/// Drive this session's pipeline through the pre-link boundary: RIR, semantic
/// analysis, CFG lowering, code generation, and object-file creation — but NOT
/// linking. Returns the total number of generated object bytes so a caller can
/// keep the result alive without depending on link availability.
///
/// This is the exact pre-link interval the RUE-1086 scaling-bench runner times
/// (the ~45 ms Caldera target is a pre-link number). It shares the RIR and
/// semantic query terminals with [`compile_with_session`], so calling it after a
/// `semantic()` reuses the cached semantic result and times only the backend
/// tail through object generation.
pub(crate) fn pre_link_object_bytes_with_session(
    session: &mut CompilerSession,
    options: &CompileOptions,
) -> MultiErrorResult<usize> {
    let _span = info_span!("compile_pipeline_pre_link").entered();
    let rir = {
        let _span = info_span!("semantic_astgen", phase = "program_construction").entered();
        session.canonical_rir()?
    };
    let semantic = session.canonical_semantic(options)?;
    let foreign_symbols =
        crate::backend::collect_foreign_symbols(rir.rir(), rir.semantic_symbols().interner());
    let export_symbols =
        crate::backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
    let products = session.codegen_products(
        &semantic,
        &foreign_symbols,
        options,
        rue_codegen::BackendArtifactRequest::default(),
    )?;
    let objects = crate::backend::generate_pre_link_objects_from_products(
        semantic.functions(),
        products,
        options,
        &export_symbols,
    )?;
    Ok(objects.iter().map(|object| object.len()).sum())
}

fn compile_snapshot_impl(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let mut session = CompilerSession::new();
    session.update_for_presentation(snapshot).into_result()?;
    compile_with_session(&mut session, snapshot, options)
}

pub(crate) fn compile_with_session(
    session: &mut CompilerSession,
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let source_tokens = session
        .published_owner()
        .filter(|program| program.belongs_to_exact_snapshot(snapshot))
        .map(|program| program.token_count())
        .ok_or_else(|| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "compilation snapshot differs from the published parsed program".into(),
            )))
        })?;
    let total_source_bytes: usize = snapshot.files().map(|source| source.source.len()).sum();
    let _span = info_span!("compile_pipeline").entered();

    let rir = {
        let _span = info_span!("semantic_astgen", phase = "program_construction").entered();
        session.canonical_rir()?
    };
    let semantic = session.canonical_semantic(options)?;
    let session_work = session.work().clone();
    let foreign_symbols =
        crate::backend::collect_foreign_symbols(rir.rir(), rir.semantic_symbols().interner());
    let export_symbols =
        crate::backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
    let products = session.codegen_products(
        &semantic,
        &foreign_symbols,
        options,
        rue_codegen::BackendArtifactRequest::default(),
    )?;
    let mut output = crate::backend::compile_backend_products(
        semantic.functions(),
        products,
        options,
        semantic.warnings(),
        &export_symbols,
    )?;
    output.source_stats = SourceStats {
        files: snapshot.len(),
        bytes: total_source_bytes,
        lines: snapshot
            .files()
            .map(|source| source.source.lines().count())
            .sum(),
        tokens: source_tokens,
    };
    output.work = PipelineWork {
        parsed: session_work.last_parse,
        merged: session_work.last_merge,
        lowered: session_work.last_rir,
        semantic: semantic.work(),
    };
    Ok(output)
}
