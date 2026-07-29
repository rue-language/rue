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
#[derive(Debug)]
pub struct FunctionWithCfg {
    /// The analyzed function from semantic analysis.
    pub analyzed: AnalyzedFunction,
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
    /// The control flow graph built from the AIR.
    pub cfg: Cfg,
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
    pub(crate) durable_cfgs: std::sync::Arc<[DurableCfgArtifact]>,
}

pub(crate) const DURABLE_CFG_SCHEMA_VERSION: u32 = 4;

/// Last-good, fail-closed CFG candidate retained between semantic requests.
///
#[derive(Debug, Clone)]
pub(crate) struct DurableCfgArtifact {
    pub(crate) schema_version: u32,
    pub(crate) semantic_schema_version: crate::DurableSemanticSchemaVersion,
    pub(crate) input: crate::durable_cfg::StableCfgInput,
    opt_level: OptLevel,
    target: Target,
    cfg: Cfg,
    domains: crate::durable_cfg::CfgDomainProjection,
}

pub(crate) struct CfgConstructionFailure {
    pub(crate) errors: CompileErrors,
    pub(crate) work: canonical_semantic::CfgConstructionWork,
}

/// Lower semantic-analysis output through drop-glue synthesis, comptime filtering,
/// CFG construction, and CFG optimization.
///
/// This is shared by `CompilerSession` queries and `--emit` presentation
/// helpers so the live semantic tail stays in one place.
pub(crate) fn build_functions_and_cfgs(
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
    opt_level: OptLevel,
    target: Target,
    interner: &ThreadedRodeo,
    durable_candidates: &[DurableCfgArtifact],
    stable_inputs: &[crate::durable_cfg::CurrentCfgInput],
    projected_identities: &std::collections::BTreeMap<
        rue_air::FunctionInstanceKey<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
        crate::FunctionInstanceKey,
    >,
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

    // Entered once on the calling thread and held across the stable collection.
    let _span = info_span!("cfg_construction", phase = "cfg_and_optimization").entered();

    let results: Vec<_> = all_functions
        .into_iter()
        .map(
            |(func, semantic_identity, symbol, local_atoms, machine_name)| {
                let legacy_name = func.name.clone();
                let dependency_source = func.implicit_drop_source.clone();
                let current_input = stable_inputs
                    .binary_search_by(|input| input.stable.function.cmp(&semantic_identity))
                    .ok()
                    .map(|index| &stable_inputs[index]);
                let projection = current_input.and_then(|input| {
                    crate::durable_cfg::CfgDomainProjection::from_body(
                        &func,
                        &input.stable,
                        input.body_span,
                        &strings,
                    )
                    .ok()
                });
                let candidate = current_input.and_then(|input| {
                    durable_candidates
                        .binary_search_by(|candidate| {
                            candidate.input.function.cmp(&input.stable.function)
                        })
                        .ok()
                        .map(|index| &durable_candidates[index])
                });
                let mut function_work = canonical_semantic::CfgConstructionWork::default();
                if let Some(candidate) = candidate {
                    function_work.cfg_reuse_candidates = 1;
                    function_work.cfg_import_attempts = 1;
                    let schema_compatible = candidate.schema_version == DURABLE_CFG_SCHEMA_VERSION
                        && crate::DURABLE_SEMANTIC_SCHEMA_VERSION
                            .accepts(candidate.semantic_schema_version);
                    if !schema_compatible {
                        function_work.cfg_schema_version_rejections = 1;
                    }
                    if schema_compatible
                        && candidate.opt_level == opt_level
                        && candidate.target == target
                        && current_input.is_some_and(|input| input.stable == candidate.input)
                        && let Some(projection) = projection.as_ref()
                    {
                        match crate::durable_cfg::CfgDomainProjection::import_cfg(
                            &candidate.domains,
                            projection,
                            &candidate.cfg,
                            current_input.unwrap().body_span,
                        ) {
                            Ok(imported) => {
                                let imported = imported.finish_after_optimization(&type_pool);
                                if let Ok(imported) = imported {
                                    function_work.cfg_import_successes = 1;
                                    function_work.cfg_reuses = 1;
                                    // Publish the current request's domain projection.
                                    // Retaining the candidate's old spans would make a
                                    // second position-only edit reuse stale locations.
                                    let artifact = DurableCfgArtifact {
                                        schema_version: candidate.schema_version,
                                        semantic_schema_version: candidate.semantic_schema_version,
                                        input: current_input.unwrap().stable.clone(),
                                        opt_level: candidate.opt_level,
                                        target: candidate.target,
                                        cfg: imported.clone(),
                                        domains: projection.clone(),
                                    };
                                    return Ok((
                                        (
                                            FunctionWithCfg {
                                                analyzed: func,
                                                semantic_identity,
                                                symbol,
                                                local_atoms,
                                                machine_name,
                                                legacy_name,
                                                cfg: imported,
                                            },
                                            Vec::new(),
                                            Vec::new(),
                                            true,
                                            Some(artifact),
                                        ),
                                        function_work,
                                    ));
                                }
                            }
                            Err(crate::durable_cfg::CfgDomainFailure::Edit(error)) => {
                                function_work.cfg_import_failures = 1;
                                function_work.cfg_builds_failed = 1;
                                let mut errors = CompileErrors::new();
                                errors.push(CompileError::new(
                                    ErrorKind::InternalError(format!(
                                        "CFG import payload construction failed: {error:?}"
                                    )),
                                    current_input.unwrap().body_span,
                                ));
                                return Err((errors, function_work));
                            }
                            Err(_) => {}
                        }
                    }
                    function_work.cfg_import_failures = 1;
                    function_work.cfg_fallbacks = 1;
                }
                function_work.cfg_builds_attempted = 1;
                function_work.air_instructions_consumed = func.air.instructions().len();
                let cfg_output = CfgBuilder::build(
                    &func.air,
                    func.num_locals,
                    func.num_param_slots,
                    &legacy_name,
                    &type_pool,
                    func.param_modes.clone(),
                    interner,
                    func.allow_unreachable_code,
                    func.callable_kind,
                );

                // A non-empty `errors` means the CFG builder hit malformed AIR
                // (an internal compiler error, RUE-7). Abort before optimizing
                // the discarded CFG rather than working on it.
                if !cfg_output.errors.is_empty() {
                    function_work.cfg_builds_failed = 1;
                    let mut errs = CompileErrors::new();
                    for e in cfg_output.errors {
                        errs.push(e);
                    }
                    return Err((errs, function_work));
                }

                function_work.cfg_builds_succeeded = 1;
                function_work.optimization_attempts = 1;
                function_work.optimized_level_attempts = usize::from(opt_level != OptLevel::O0);
                let cfg = cfg_output
                    .cfg
                    .expect("successful CFG construction publishes a validated CFG");
                let cfg = match rue_cfg::opt::optimize(cfg, opt_level, &type_pool) {
                    Ok(cfg) => cfg,
                    Err(error) => {
                        function_work.cfg_builds_failed = 1;
                        let mut errors = CompileErrors::new();
                        errors.push(CompileError::without_span(ErrorKind::InternalError(
                            format!("CFG optimization failed: {error}"),
                        )));
                        return Err((errors, function_work));
                    }
                };
                function_work.optimization_completions = 1;
                function_work.cfg_warnings_emitted = cfg_output.warnings.len();
                function_work.implicit_destructor_targets_emitted =
                    cfg_output.implicit_named_destructors.len();

                let mut implicit_edges = Vec::new();
                let mut complete = !cfg_output.anonymous_destructor_dependency_incomplete;
                // A named struct definition globally owns its synthesized glue.
                // Its own destructor is emitted as a direct AIR call rather than a
                // CFG Drop, so retain that definition -> destructor edge here.
                if let Some(rue_air::ImplicitDropDependencySourceEvent::NamedStruct {
                    file,
                    name,
                }) = &dependency_source
                {
                    for struct_id in type_pool.all_struct_ids() {
                        let target = type_pool.struct_def(struct_id);
                        if target.file_id.index() == *file
                            && target.name == *name
                            && target.destructor.is_some()
                            && !target.is_builtin
                        {
                            implicit_edges.push(rue_air::ImplicitNamedDestructorDependencyEvent {
                                source: dependency_source.clone().unwrap(),
                                target_file: *file,
                                target_owner_name: name.clone(),
                            });
                        }
                    }
                }
                if !cfg_output.implicit_named_destructors.is_empty() {
                    if matches!(
                        dependency_source,
                        Some(rue_air::ImplicitDropDependencySourceEvent::Anonymous)
                    ) {
                        complete = false;
                    } else if let Some(source) = dependency_source {
                        for struct_id in cfg_output.implicit_named_destructors {
                            let target = type_pool.struct_def(struct_id);
                            implicit_edges.push(rue_air::ImplicitNamedDestructorDependencyEvent {
                                source: source.clone(),
                                target_file: target.file_id.index(),
                                target_owner_name: target.name.clone(),
                            });
                        }
                    }
                }

                function_work.cfg_export_attempts = usize::from(current_input.is_some());
                let artifact = current_input.zip(projection).and_then(|(input, domains)| {
                    (cfg_output.warnings.is_empty()
                        && implicit_edges.is_empty()
                        && complete
                        && domains.validate_cfg(&cfg, input.body_span).is_ok())
                    .then(|| DurableCfgArtifact {
                        schema_version: DURABLE_CFG_SCHEMA_VERSION,
                        semantic_schema_version: crate::DURABLE_SEMANTIC_SCHEMA_VERSION,
                        input: input.stable.clone(),
                        opt_level,
                        target,
                        cfg: cfg.clone(),
                        domains,
                    })
                });
                function_work.cfg_export_successes = usize::from(artifact.is_some());
                function_work.cfg_export_rejections =
                    function_work.cfg_export_attempts - function_work.cfg_export_successes;
                Ok((
                    (
                        FunctionWithCfg {
                            analyzed: func,
                            semantic_identity,
                            symbol,
                            local_atoms,
                            machine_name,
                            legacy_name,
                            cfg,
                        },
                        cfg_output.warnings,
                        implicit_edges,
                        complete,
                        artifact,
                    ),
                    function_work,
                ))
            },
        )
        .collect();

    let mut functions = Vec::with_capacity(results.len());
    let mut implicit_named_destructor_dependencies = Vec::new();
    let mut implicit_named_destructor_dependencies_complete = true;
    let mut durable_cfgs = Vec::new();
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
        if let Some((func, func_warnings, mut implicit_edges, complete, artifact)) = output {
            functions.push(func);
            warnings.extend(func_warnings);
            implicit_named_destructor_dependencies.append(&mut implicit_edges);
            implicit_named_destructor_dependencies_complete &= complete;
            durable_cfgs.extend(artifact);
        }
    }
    if let Some(errors) = first_errors {
        return Err(CfgConstructionFailure { errors, work });
    }
    implicit_named_destructor_dependencies.sort();
    implicit_named_destructor_dependencies.dedup();
    durable_cfgs.sort_by(|left, right| left.input.function.cmp(&right.input.function));

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
        durable_cfgs: durable_cfgs.into(),
    })
}

#[cfg(test)]
pub(crate) fn synthetic_projected_function_identities(
    sema_output: &SemaOutput,
) -> std::collections::BTreeMap<
    rue_air::FunctionInstanceKey<rue_air::SemanticDefinitionToken, rue_air::SemanticModuleToken>,
    crate::FunctionInstanceKey,
> {
    use std::convert::Infallible;
    use std::sync::Arc;

    let definition_module = crate::ModuleId::from_logical_path("test/definitions.rue")
        .expect("synthetic test module path is valid");
    let project = |identity: &rue_air::FunctionInstanceKey<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >| {
        identity
            .try_map_identities(
                &|token| {
                    Ok::<_, Infallible>(crate::StableDefinitionKey::for_test(
                        definition_module.clone(),
                        crate::StableDefinitionNamespace::Value,
                        crate::StableDefinitionKind::Function,
                        Arc::<str>::from(format!("definition_{}", token.slot())),
                        None,
                    ))
                },
                &|token| {
                    Ok::<_, Infallible>(
                        crate::ModuleId::from_logical_path(&format!(
                            "test/module_{}.rue",
                            token.slot()
                        ))
                        .expect("synthetic test module path is valid"),
                    )
                },
            )
            .expect("synthetic identity projection is infallible")
    };

    sema_output
        .functions
        .iter()
        .map(|function| (function.identity.clone(), project(&function.identity)))
        .chain(
            sema_output
                .aggregate_type_identities_by_type
                .values()
                .cloned()
                .map(|identity| {
                    let identity = rue_air::FunctionInstanceKey::DropGlue(Box::new(identity));
                    let projected = project(&identity);
                    (identity, projected)
                }),
        )
        .collect()
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
    let objects = crate::backend::generate_pre_link_objects(
        semantic.functions(),
        semantic.type_pool(),
        semantic.strings(),
        rir.semantic_symbols().interner(),
        options,
        &foreign_symbols,
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
    let session_work = session.work();
    let foreign_symbols =
        crate::backend::collect_foreign_symbols(rir.rir(), rir.semantic_symbols().interner());
    let export_symbols =
        crate::backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
    let mut output = crate::backend::compile_backend(
        semantic.functions(),
        semantic.type_pool(),
        semantic.strings(),
        rir.semantic_symbols().interner(),
        options,
        semantic.warnings(),
        &foreign_symbols,
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

#[cfg(test)]
mod failure_work_tests {
    use lasso::ThreadedRodeo;
    use rue_air::{Sema, Type};
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;
    use rue_span::Span;

    use super::*;

    fn malformed_cfg_input() -> (SemaOutput, ThreadedRodeo) {
        let source = "fn alpha() -> i32 { 1 }\nfn broken() -> i32 { 2 }\nfn main() -> i32 { alpha() + broken() + zeta() }\nfn zeta() -> i32 { 3 }";
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let mut output = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .analyze_all_for_test()
            .unwrap();

        for (name, start) in [("broken", 10), ("zeta", 20)] {
            let generic = interner.get_or_intern(format!("unrewritten_{name}"));
            let function = output
                .functions
                .iter_mut()
                .find(|function| function.name == name)
                .unwrap();
            let mut air = rue_air::AirEditor::new(Type::I32);
            let call = air
                .add_call_generic(
                    generic,
                    &[],
                    &[],
                    &[],
                    Type::I32,
                    Span::new(start, start + 1),
                )
                .unwrap();
            air.add_ret(Some(call), Type::I32, Span::new(start, start + 1));
            function.air = air
                .finish(rue_air::AirValidationContext::Canonical(&output.type_pool))
                .expect("malformed test AIR structure must validate");
        }
        (output, interner)
    }

    #[test]
    fn malformed_air_retains_deterministic_work_from_every_cfg_builder() {
        let run = || {
            let (output, interner) = malformed_cfg_input();
            let projected_identities = synthetic_projected_function_identities(&output);
            let demanded_drop_glue = std::collections::BTreeSet::new();
            let drop_glue_plans = std::collections::BTreeMap::new();
            match build_functions_and_cfgs(
                output,
                &demanded_drop_glue,
                &drop_glue_plans,
                OptLevel::O1,
                Target::host().unwrap(),
                &interner,
                &[],
                &[],
                &projected_identities,
            ) {
                Ok(_) => panic!("malformed AIR unexpectedly built a CFG"),
                Err(failure) => failure,
            }
        };
        let first = run();
        let second = run();
        assert_eq!(first.work, second.work);
        assert_eq!(first.work.functions_considered, 4);
        assert_eq!(first.work.cfg_builds_attempted, 4);
        assert_eq!(first.work.cfg_builds_succeeded, 2);
        assert_eq!(first.work.cfg_builds_failed, 2);
        assert_eq!(first.work.optimization_attempts, 2);
        assert_eq!(first.work.optimization_completions, 2);
        assert_eq!(first.work.optimized_level_attempts, 2);
        assert_eq!(
            format!("{:?}", first.errors),
            format!("{:?}", second.errors)
        );
        assert_eq!(
            first.errors.iter().next().unwrap().span().unwrap().start,
            20,
            "the first canonically sorted failing function supplies the published diagnostic"
        );
    }
}
