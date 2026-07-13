//! One-pass canonical declaration binding, body analysis, and CFG lowering.

use std::sync::Arc;

use rue_air::{
    AnalyzedBodyOwnerEvent, BodyAnalysisWork, BodyNamedDependencyEvent, DeclarationBindingWork,
    DeclarationBuiltinTypeCallHeadDependencyEvent, DeclarationTypeCallHeadDependencyEvent,
    DeclarationTypeDependencyEvent, NamedConstDependencyEvent, NamedDestructorDependencyEvent,
    NamedMethodDependencyEvent, OrdinaryFreeFunctionDependencyEvent, RirDeclarationIndexWork,
    SemanticBindingManifestWork, SpecializedFreeFunctionDependencyEvent,
    SpecializedFreeFunctionOrigin,
};
use tracing::info_span;

use crate::{
    BoundDefinitionSet, BoundDefinitionWork, CanonicalMergedProgram, CanonicalRirOutput,
    CodegenInputDescriptor, CompileOptions, CompileWarning, DurableDeclarationSemantic,
    FunctionWithCfg, MultiErrorResult, SemanticInputDescriptor, TypeInternPool,
    bound_definitions::{
        configure_canonical_sema, issue_bound_definitions, issue_shell_definitions,
    },
    build_functions_and_cfgs,
};

pub(crate) struct CanonicalOrdinaryAnalysis {
    pub output: CanonicalSemanticOutput,
    pub definitions: BoundDefinitionSet,
    pub durable_declarations: Option<std::sync::Arc<[DurableDeclarationSemantic]>>,
}

/// One current-revision declaration epoch prepared for either ordinary
/// resolution or durable installation. Stable identities are issued from the
/// same shells that the selected analysis path subsequently consumes.
pub(crate) struct CanonicalPreparedDeclarations<'a> {
    shells: rue_air::DeclarationShells<'a>,
    shell_records: Vec<rue_air::SemanticDeclarationShell>,
    definitions: BoundDefinitionSet,
    declaration_index: RirDeclarationIndexWork,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CanonicalDeclarationReuseWork {
    pub plan_executions: usize,
    pub durable_records_compared: usize,
    pub durable_records_reused: usize,
    pub ordinary_declaration_resolutions_skipped: usize,
    pub install_invocations: usize,
    pub fallbacks: usize,
    /// Actual request-local semantic epochs constructed, including a fresh
    /// epoch required after a consuming installation failure.
    pub semantic_epochs_started: usize,
    pub declaration_indexes_built: usize,
    pub shell_predeclaration_epochs: usize,
    pub durable_cache_population_exports: usize,
    pub fallback_epochs_started: usize,
}

pub(crate) fn prepare_canonical_declarations<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    options: &CompileOptions,
) -> MultiErrorResult<CanonicalPreparedDeclarations<'a>> {
    let sema = configure_timed_canonical_sema(merged, rir, options)?;
    let declaration_index = sema.rir_declaration_index_work();
    let shells = sema.predeclare_declaration_shells()?;
    let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
    let definitions = match issue_shell_definitions(merged, rir.source_revision(), &shell_records) {
        Ok(definitions) => definitions,
        Err(preparation_error) => {
            let preparation_error = crate::CompileErrors::from(preparation_error);
            let bound = shells.resolve_declarations()?;
            return recover_body_diagnostics(bound, preparation_error);
        }
    };
    Ok(CanonicalPreparedDeclarations {
        shells,
        shell_records,
        definitions,
        declaration_index,
    })
}

impl CanonicalPreparedDeclarations<'_> {
    pub(crate) fn definitions(&self) -> &BoundDefinitionSet {
        &self.definitions
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Structural work from one canonical semantic request.
pub struct CanonicalSemanticWork {
    /// One request-local RIR declaration-index construction.
    pub declaration_index: RirDeclarationIndexWork,
    /// Completed declaration binding, independent of optional manifest work.
    pub binding: DeclarationBindingWork,
    /// Authoritative binding-manifest traversal used to validate body tokens.
    pub manifest: SemanticBindingManifestWork,
    /// Public stable-ID issuance work, absent when IDs were not requested.
    pub bound_definitions: Option<BoundDefinitionWork>,
    /// Exact work performed to make AIR body ownership authoritative.
    pub body_owner_tokens: BodyOwnerTokenWork,
    /// Demand-driven function-body analysis work.
    pub body_analysis: BodyAnalysisWork,
    /// Observational durable ordinary-body boundary work. Reuse remains zero.
    pub durable_bodies: crate::DurableBodyWork,
    /// Drop-glue, CFG construction, and optimization work.
    pub cfg: CfgConstructionWork,
    /// Whether this request asked for stable source definition IDs.
    pub stable_ids_requested: bool,
    pub declaration_reuse: CanonicalDeclarationReuseWork,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BodyOwnerTokenWork {
    pub provisional_slots: usize,
    pub authoritative_slots: usize,
    pub slots_validated: usize,
    pub tokens_installed: usize,
    pub validation_failures: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CfgConstructionWork {
    pub drop_glue_functions_synthesized: usize,
    pub functions_considered: usize,
    pub comptime_functions_filtered: usize,
    pub cfg_builds_attempted: usize,
    pub cfg_builds_succeeded: usize,
    pub cfg_builds_failed: usize,
    pub air_instructions_consumed: usize,
    pub optimization_attempts: usize,
    pub optimization_completions: usize,
    pub optimized_level_attempts: usize,
    pub cfg_warnings_emitted: usize,
    pub implicit_destructor_targets_emitted: usize,
}

/// Owned semantic and optimized CFG artifacts from the canonical frontend.
#[derive(Debug)]
pub struct CanonicalSemanticOutput {
    input: CodegenInputDescriptor,
    functions: Vec<FunctionWithCfg>,
    type_pool: TypeInternPool,
    strings: Vec<String>,
    warnings: Vec<CompileWarning>,
    bound_definitions: Option<BoundDefinitionSet>,
    body_owner_issuer: BoundDefinitionSet,
    durable_ordinary_body_payloads: Arc<[crate::DurableOrdinaryBodyPayload]>,
    work: CanonicalSemanticWork,
    analyzed_body_owners: Vec<AnalyzedBodyOwnerEvent>,
    body_named_dependencies: Vec<BodyNamedDependencyEvent>,
    ordinary_free_function_dependencies: Vec<OrdinaryFreeFunctionDependencyEvent>,
    ordinary_free_function_dependencies_complete: bool,
    specialized_free_function_origins: Vec<SpecializedFreeFunctionOrigin>,
    specialized_free_function_dependencies: Vec<SpecializedFreeFunctionDependencyEvent>,
    specialized_free_function_dependencies_complete: bool,
    named_method_dependencies: Vec<NamedMethodDependencyEvent>,
    non_generic_named_method_dependencies_complete: bool,
    generic_named_method_dependencies_complete: bool,
    named_destructor_dependencies: Vec<NamedDestructorDependencyEvent>,
    named_destructor_dependencies_complete: bool,
    declaration_type_dependencies: Vec<DeclarationTypeDependencyEvent>,
    declaration_type_dependencies_complete: bool,
    declaration_type_call_head_dependencies: Vec<DeclarationTypeCallHeadDependencyEvent>,
    declaration_type_call_head_dependencies_complete: bool,
    declaration_builtin_type_call_head_dependencies:
        Vec<DeclarationBuiltinTypeCallHeadDependencyEvent>,
    supported_type_call_heads_complete: bool,
    named_const_dependencies: Vec<NamedConstDependencyEvent>,
    named_value_const_dependencies_complete: bool,
    implicit_named_destructor_dependencies: Vec<rue_air::ImplicitNamedDestructorDependencyEvent>,
    implicit_named_destructor_dependencies_complete: bool,
}

impl CanonicalSemanticOutput {
    /// Exact semantic and optimization identity of this output.
    pub fn input(&self) -> &CodegenInputDescriptor {
        &self.input
    }
    /// Analyzed functions paired with optimized CFGs in machine-symbol order.
    pub fn functions(&self) -> &[FunctionWithCfg] {
        &self.functions
    }
    /// Request-local type universe retained by the semantic output.
    pub fn type_pool(&self) -> &TypeInternPool {
        &self.type_pool
    }
    /// String literals indexed by AIR string-constant index.
    pub fn strings(&self) -> &[String] {
        &self.strings
    }
    /// Semantic and CFG warnings in canonical output order.
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }
    pub fn ordinary_free_function_dependencies(&self) -> &[OrdinaryFreeFunctionDependencyEvent] {
        &self.ordinary_free_function_dependencies
    }
    pub fn analyzed_body_owners(&self) -> &[AnalyzedBodyOwnerEvent] {
        &self.analyzed_body_owners
    }
    pub fn body_named_dependencies(&self) -> &[BodyNamedDependencyEvent] {
        &self.body_named_dependencies
    }
    pub fn ordinary_free_function_dependencies_complete(&self) -> bool {
        self.ordinary_free_function_dependencies_complete
    }
    pub fn specialized_free_function_origins(&self) -> &[SpecializedFreeFunctionOrigin] {
        &self.specialized_free_function_origins
    }
    pub fn specialized_free_function_dependencies(
        &self,
    ) -> &[SpecializedFreeFunctionDependencyEvent] {
        &self.specialized_free_function_dependencies
    }
    pub fn specialized_free_function_dependencies_complete(&self) -> bool {
        self.specialized_free_function_dependencies_complete
    }
    pub fn named_method_dependencies(&self) -> &[NamedMethodDependencyEvent] {
        &self.named_method_dependencies
    }
    pub fn non_generic_named_method_dependencies_complete(&self) -> bool {
        self.non_generic_named_method_dependencies_complete
    }
    pub fn generic_named_method_dependencies_complete(&self) -> bool {
        self.generic_named_method_dependencies_complete
    }
    pub fn named_destructor_dependencies(&self) -> &[NamedDestructorDependencyEvent] {
        &self.named_destructor_dependencies
    }
    pub fn named_destructor_dependencies_complete(&self) -> bool {
        self.named_destructor_dependencies_complete
    }
    pub fn declaration_type_dependencies(&self) -> &[DeclarationTypeDependencyEvent] {
        &self.declaration_type_dependencies
    }
    pub fn declaration_type_dependencies_complete(&self) -> bool {
        self.declaration_type_dependencies_complete
    }
    pub fn declaration_type_call_head_dependencies(
        &self,
    ) -> &[DeclarationTypeCallHeadDependencyEvent] {
        &self.declaration_type_call_head_dependencies
    }
    pub fn declaration_type_call_head_dependencies_complete(&self) -> bool {
        self.declaration_type_call_head_dependencies_complete
    }
    pub fn declaration_builtin_type_call_head_dependencies(
        &self,
    ) -> &[DeclarationBuiltinTypeCallHeadDependencyEvent] {
        &self.declaration_builtin_type_call_head_dependencies
    }
    pub fn supported_type_call_heads_complete(&self) -> bool {
        self.supported_type_call_heads_complete
    }
    pub fn named_const_dependencies(&self) -> &[NamedConstDependencyEvent] {
        &self.named_const_dependencies
    }
    pub fn named_value_const_dependencies_complete(&self) -> bool {
        self.named_value_const_dependencies_complete
    }
    pub fn implicit_named_destructor_dependencies(
        &self,
    ) -> &[rue_air::ImplicitNamedDestructorDependencyEvent] {
        &self.implicit_named_destructor_dependencies
    }
    pub fn implicit_named_destructor_dependencies_complete(&self) -> bool {
        self.implicit_named_destructor_dependencies_complete
    }
    /// Stable definition identities when requested for this run.
    pub fn bound_definitions(&self) -> Option<&BoundDefinitionSet> {
        self.bound_definitions.as_ref()
    }
    pub(crate) fn body_owner_issuer(&self) -> &BoundDefinitionSet {
        &self.body_owner_issuer
    }
    pub(crate) fn durable_ordinary_body_payloads(&self) -> &[crate::DurableOrdinaryBodyPayload] {
        &self.durable_ordinary_body_payloads
    }
    /// Structural work performed by this request.
    pub fn work(&self) -> CanonicalSemanticWork {
        self.work
    }
}

/// Bind declarations once, optionally issue stable IDs, then consume the same
/// transient bound Sema for body analysis and CFG construction.
pub fn analyze_canonical_program(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    options: &CompileOptions,
    request_stable_ids: bool,
) -> MultiErrorResult<CanonicalSemanticOutput> {
    let input = CodegenInputDescriptor {
        semantic: SemanticInputDescriptor::new(
            merged.definitions().source_snapshot(),
            options.target,
            &options.preview_features,
        ),
        opt_level: options.opt_level.into(),
    };
    let prepared = prepare_canonical_declarations(merged, rir, options)?;
    let CanonicalPreparedDeclarations {
        shells,
        shell_records: _,
        definitions,
        declaration_index,
    } = prepared;
    let bound = shells.resolve_declarations()?;
    finish_canonical_analysis(
        input,
        merged,
        rir,
        options,
        request_stable_ids,
        declaration_index,
        bound,
        definitions,
        CanonicalDeclarationReuseWork::default(),
        info_span!("sema").entered(),
    )
}

/// Run the ordinary canonical path once and opportunistically export its
/// resolved declaration payloads before body analysis consumes the binder.
/// Export is fail-closed: unsupported payloads disable reuse without changing
/// the successful batch semantic result.
pub(crate) fn analyze_prepared_canonical_program_with_durable_export(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    options: &CompileOptions,
    prepared: CanonicalPreparedDeclarations<'_>,
) -> MultiErrorResult<CanonicalOrdinaryAnalysis> {
    let input = CodegenInputDescriptor {
        semantic: SemanticInputDescriptor::new(
            merged.definitions().source_snapshot(),
            options.target,
            &options.preview_features,
        ),
        opt_level: options.opt_level.into(),
    };
    let sema_span = info_span!("sema").entered();
    let CanonicalPreparedDeclarations {
        shells,
        shell_records,
        definitions,
        declaration_index,
    } = prepared;
    let bound = shells.resolve_declarations()?;

    let durable_declarations = bound
        .with_declaration_semantics_from_shells(&shell_records, |records, _work| {
            crate::durable_semantics::convert_declaration_semantics(merged, &definitions, records)
        })
        .ok()
        .and_then(Result::ok);

    let output = finish_canonical_analysis(
        input,
        merged,
        rir,
        options,
        false,
        declaration_index,
        bound,
        definitions.clone(),
        CanonicalDeclarationReuseWork {
            semantic_epochs_started: 1,
            declaration_indexes_built: declaration_index.build_invocations,
            shell_predeclaration_epochs: 1,
            durable_cache_population_exports: usize::from(durable_declarations.is_some()),
            ..CanonicalDeclarationReuseWork::default()
        },
        sema_span,
    )?;
    Ok(CanonicalOrdinaryAnalysis {
        output,
        definitions,
        durable_declarations,
    })
}

/// Analyze bodies in a fresh semantic epoch whose declaration payloads are
/// installed from stable, request-independent records. Any projection or
/// installation failure is typed internally and falls back to a wholly fresh
/// ordinary binder; partially installed state is never observed.
pub(crate) fn analyze_prepared_canonical_program_reusing_declarations(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    options: &CompileOptions,
    prepared: CanonicalPreparedDeclarations<'_>,
    definitions: &BoundDefinitionSet,
    durable: &[DurableDeclarationSemantic],
) -> MultiErrorResult<CanonicalSemanticOutput> {
    let input = CodegenInputDescriptor {
        semantic: SemanticInputDescriptor::new(
            merged.definitions().source_snapshot(),
            options.target,
            &options.preview_features,
        ),
        opt_level: options.opt_level.into(),
    };
    let mut reuse = CanonicalDeclarationReuseWork {
        plan_executions: 1,
        durable_records_compared: durable.len(),
        semantic_epochs_started: 1,
        shell_predeclaration_epochs: 1,
        ..CanonicalDeclarationReuseWork::default()
    };
    let CanonicalPreparedDeclarations {
        shells,
        shell_records,
        definitions: prepared_definitions,
        declaration_index,
    } = prepared;
    let mut selected_definitions = prepared_definitions;
    reuse.declaration_indexes_built = declaration_index.build_invocations;
    let bound = match crate::project_durable_declaration_semantics(
        merged,
        definitions,
        &shell_records,
        durable,
    ) {
        Err(_) => {
            reuse.fallbacks = 1;
            // Projection is read-only, so ordinary resolution consumes the
            // exact same unmutated shells and does not create a hidden epoch.
            shells.resolve_declarations()?
        }
        Ok((projected, _)) => {
            reuse.install_invocations += 1;
            match shells.install_declaration_semantics(&projected) {
                Ok(bound) => {
                    reuse.durable_records_reused = durable.len();
                    reuse.ordinary_declaration_resolutions_skipped = 1;
                    bound
                }
                Err(_) => {
                    // Installation consumes potentially mutated shells. Only
                    // this failure requires a wholly fresh ordinary epoch.
                    reuse.fallbacks = 1;
                    reuse.fallback_epochs_started = 1;
                    reuse.semantic_epochs_started += 1;
                    let fallback = configure_timed_canonical_sema(merged, rir, options)?;
                    reuse.declaration_indexes_built +=
                        fallback.rir_declaration_index_work().build_invocations;
                    let fallback_shells = fallback.predeclare_declaration_shells()?;
                    let fallback_records = fallback_shells
                        .declaration_shells()
                        .cloned()
                        .collect::<Vec<_>>();
                    selected_definitions =
                        issue_shell_definitions(merged, rir.source_revision(), &fallback_records)
                            .map_err(crate::CompileErrors::from)?;
                    fallback_shells.resolve_declarations()?
                }
            }
        }
    };
    let sema_span = info_span!("sema").entered();
    finish_canonical_analysis(
        input,
        merged,
        rir,
        options,
        false,
        declaration_index,
        bound,
        selected_definitions,
        reuse,
        sema_span,
    )
}

/// Construct one request-local declaration index under the authoritative leaf
/// timing boundary. Keeping this wrapper at every canonical entry point gives
/// ordinary and durable-reuse requests the same non-nested span shape. A typed
/// durable-install fallback records a second leaf only because it genuinely
/// constructs a second semantic epoch.
fn configure_timed_canonical_sema<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    options: &CompileOptions,
) -> MultiErrorResult<rue_air::Sema<'a>> {
    let _span = info_span!("rir_declaration_index", instruction_count = rir.rir().len()).entered();
    configure_canonical_sema(
        merged,
        rir,
        options.preview_features.clone(),
        options.target,
    )
}

fn finish_canonical_analysis(
    input: CodegenInputDescriptor,
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    options: &CompileOptions,
    request_stable_ids: bool,
    declaration_index: RirDeclarationIndexWork,
    bound: rue_air::BoundSema<'_>,
    provisional_definitions: BoundDefinitionSet,
    declaration_reuse: CanonicalDeclarationReuseWork,
    sema_span: tracing::span::EnteredSpan,
) -> MultiErrorResult<CanonicalSemanticOutput> {
    let binding = bound.binding_work();
    let manifest = bound.binding_manifest();
    let authoritative_definitions = match issue_bound_definitions(
        merged,
        rir.source_revision(),
        manifest.bindings(),
        manifest.work(),
    ) {
        Ok(definitions) => definitions,
        Err(preparation_error) => {
            return recover_body_diagnostics(bound, crate::CompileErrors::from(preparation_error));
        }
    };
    let provisional_keys = provisional_definitions
        .definitions()
        .iter()
        .filter(|record| {
            matches!(
                record.stable_key().kind(),
                crate::StableDefinitionKind::Function
                    | crate::StableDefinitionKind::Method
                    | crate::StableDefinitionKind::AssociatedFunction
                    | crate::StableDefinitionKind::Destructor
            )
        })
        .map(|r| r.stable_key())
        .collect::<Vec<_>>();
    let authoritative_keys = authoritative_definitions
        .definitions()
        .iter()
        .filter(|record| {
            matches!(
                record.stable_key().kind(),
                crate::StableDefinitionKind::Function
                    | crate::StableDefinitionKind::Method
                    | crate::StableDefinitionKind::AssociatedFunction
                    | crate::StableDefinitionKind::Destructor
            )
        })
        .map(|r| r.stable_key())
        .collect::<Vec<_>>();
    if provisional_keys != authoritative_keys {
        let first_difference = provisional_keys
            .iter()
            .zip(&authoritative_keys)
            .position(|(a, b)| a != b);
        let preparation_error = crate::CompileErrors::from(crate::CompileError::without_span(
            rue_error::ErrorKind::InternalError(format!(
                "prepared declaration shells do not exactly match authoritative bound definitions: provisional={} authoritative={} first_difference={:?} provisional_key={:?} authoritative_key={:?}",
                provisional_keys.len(),
                authoritative_keys.len(),
                first_difference,
                first_difference.and_then(|index| provisional_keys.get(index)),
                first_difference.and_then(|index| authoritative_keys.get(index)),
            )),
        ));
        return recover_body_diagnostics(bound, preparation_error);
    }
    let manifest_work = manifest.work();
    let bound_definitions = request_stable_ids.then(|| authoritative_definitions.clone());
    let endpoints = authoritative_definitions.body_owner_endpoints();
    let body_owner_tokens = BodyOwnerTokenWork {
        provisional_slots: provisional_keys.len(),
        authoritative_slots: authoritative_keys.len(),
        slots_validated: authoritative_keys.len(),
        tokens_installed: endpoints.len(),
        validation_failures: 0,
    };
    let bound = bound.install_body_owner_tokens(&endpoints).map_err(|_| {
        crate::CompileErrors::from(crate::CompileError::without_span(
            rue_error::ErrorKind::InternalError(
                "failed to install authoritative body-owner tokens".into(),
            ),
        ))
    })?;

    let sema_output = bound.analyze_all_bodies()?;
    let body_analysis = sema_output.body_analysis_work;
    let mut durable_body_work = crate::DurableBodyWork {
        export_attempts: body_analysis.ordinary_body_exports_attempted,
        export_successes: body_analysis.ordinary_body_exports_succeeded,
        export_rejections: body_analysis.ordinary_body_exports_rejected,
        instructions_exported: body_analysis.ordinary_body_export_instructions_emitted,
        places_exported: body_analysis.ordinary_body_export_places_emitted,
        strings_exported: body_analysis.ordinary_body_export_strings_emitted,
        ..crate::DurableBodyWork::default()
    };
    let durable_ordinary_body_payloads = crate::convert_semantic_body_exports(
        &sema_output.ordinary_body_exports,
        merged,
        &authoritative_definitions,
        &mut durable_body_work,
    )
    .unwrap_or_else(|_| Arc::from([]));
    let analyzed_body_owners = sema_output.analyzed_body_owners.clone();
    let body_named_dependencies = sema_output.body_named_dependencies.clone();
    let ordinary_free_function_dependencies =
        sema_output.ordinary_free_function_dependencies.clone();
    let ordinary_free_function_dependencies_complete =
        sema_output.ordinary_free_function_dependencies_complete;
    let specialized_free_function_origins = sema_output.specialized_free_function_origins.clone();
    let specialized_free_function_dependencies =
        sema_output.specialized_free_function_dependencies.clone();
    let specialized_free_function_dependencies_complete =
        sema_output.specialized_free_function_dependencies_complete;
    let named_method_dependencies = sema_output.named_method_dependencies.clone();
    let non_generic_named_method_dependencies_complete =
        sema_output.non_generic_named_method_dependencies_complete;
    let generic_named_method_dependencies_complete =
        sema_output.generic_named_method_dependencies_complete;
    let named_destructor_dependencies = sema_output.named_destructor_dependencies.clone();
    let named_destructor_dependencies_complete = sema_output.named_destructor_dependencies_complete;
    let declaration_type_dependencies = sema_output.declaration_type_dependencies.clone();
    let declaration_type_dependencies_complete = sema_output.declaration_type_dependencies_complete;
    let declaration_type_call_head_dependencies =
        sema_output.declaration_type_call_head_dependencies.clone();
    let declaration_type_call_head_dependencies_complete =
        sema_output.declaration_type_call_head_dependencies_complete;
    let declaration_builtin_type_call_head_dependencies = sema_output
        .declaration_builtin_type_call_head_dependencies
        .clone();
    let supported_type_call_heads_complete = sema_output.supported_type_call_heads_complete;
    let named_const_dependencies = sema_output.named_const_dependencies.clone();
    let named_value_const_dependencies_complete =
        sema_output.named_value_const_dependencies_complete;
    drop(sema_span);
    let cfg = build_functions_and_cfgs(
        sema_output,
        options.opt_level,
        rir.semantic_symbols().interner(),
    )?;
    let mut warnings = cfg.warnings;
    warnings.sort_by(|left, right| {
        let key = |warning: &CompileWarning| {
            let span = warning.span();
            let module = span
                .and_then(|span| {
                    merged
                        .ast()
                        .modules()
                        .iter()
                        .find(|module| module.file_id() == span.file_id)
                })
                .map(|module| module.module_id().as_str())
                .unwrap_or("");
            (
                module,
                span.map(|span| span.start).unwrap_or(0),
                span.map(|span| span.end).unwrap_or(0),
                warning.to_string(),
                format!("{:?}", warning.diagnostic()),
            )
        };
        key(left).cmp(&key(right))
    });
    let work = CanonicalSemanticWork {
        declaration_index,
        binding,
        manifest: manifest_work,
        bound_definitions: bound_definitions.as_ref().map(BoundDefinitionSet::work),
        body_owner_tokens,
        body_analysis,
        durable_bodies: durable_body_work,
        cfg: cfg.work,
        stable_ids_requested: request_stable_ids,
        declaration_reuse,
    };
    Ok(CanonicalSemanticOutput {
        input,
        functions: cfg.functions,
        type_pool: cfg.type_pool,
        strings: cfg.strings,
        warnings,
        bound_definitions,
        body_owner_issuer: authoritative_definitions,
        durable_ordinary_body_payloads,
        work,
        analyzed_body_owners,
        body_named_dependencies,
        ordinary_free_function_dependencies,
        ordinary_free_function_dependencies_complete,
        specialized_free_function_origins,
        specialized_free_function_dependencies,
        specialized_free_function_dependencies_complete,
        named_method_dependencies,
        non_generic_named_method_dependencies_complete,
        generic_named_method_dependencies_complete,
        named_destructor_dependencies,
        named_destructor_dependencies_complete,
        declaration_type_dependencies,
        declaration_type_dependencies_complete,
        declaration_type_call_head_dependencies,
        declaration_type_call_head_dependencies_complete,
        declaration_builtin_type_call_head_dependencies,
        supported_type_call_heads_complete,
        named_const_dependencies,
        named_value_const_dependencies_complete,
        implicit_named_destructor_dependencies: cfg.implicit_named_destructor_dependencies,
        implicit_named_destructor_dependencies_complete: cfg
            .implicit_named_destructor_dependencies_complete,
    })
}

/// Preserve ordinary source diagnostics when strict token preparation rejects
/// an already-bound epoch. The semantic result is consumed and discarded: it
/// can recover diagnostics, but can never publish a partially prepared output.
fn recover_body_diagnostics<T>(
    bound: rue_air::BoundSema<'_>,
    preparation_error: crate::CompileErrors,
) -> MultiErrorResult<T> {
    match bound.analyze_all_bodies() {
        Err(source_errors) => Err(source_errors),
        Ok(_) => Err(preparation_error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use rue_span::FileId;

    use super::{
        BodyOwnerTokenWork, CanonicalSemanticOutput, CanonicalSemanticWork,
        analyze_canonical_program,
    };
    use crate::parsed_modules::parse_source_snapshot_modules;
    use crate::{
        CanonicalRirOutput, CompileOptions, FunctionWithCfg, SourceMetadata, SourceSnapshot,
        lower_canonical_rir, merge_parsed_modules,
    };

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<HashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<HashMap<_, _>>();
        let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
        SourceSnapshot::new(
            metadata,
            entries
                .iter()
                .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    fn assert_token_preparation_preserves_source_errors(source: &str) {
        let source = snapshot(&[(1, "/main.rue", "main.rue", source)], 1);
        let parsed = parse_source_snapshot_modules(&source).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        let options = CompileOptions::default();

        let ordinary = match crate::bound_definitions::configure_canonical_sema(
            &merged,
            &rir,
            options.preview_features.clone(),
            options.target,
        )
        .unwrap()
        .bind_declarations()
        {
            Err(errors) => errors,
            Ok(_) => panic!("test input must fail ordinary declaration binding"),
        };
        let canonical = analyze_canonical_program(&merged, &rir, &options, false).unwrap_err();
        let messages = |errors: crate::CompileErrors| {
            errors.iter().map(ToString::to_string).collect::<Vec<_>>()
        };
        assert_eq!(messages(canonical), messages(ordinary));
    }

    #[test]
    fn token_preparation_failures_recover_ordinary_binding_diagnostics() {
        for source in [
            "const value: i32 = 1; const value: i32 = 2; fn main() {}",
            "struct Value {} drop fn Value(self) {} drop fn Value(self) {} fn main() {}",
            "drop fn Missing(self) {} fn main() {}",
        ] {
            assert_token_preparation_preserves_source_errors(source);
        }
    }

    fn canonical(
        snapshot: &SourceSnapshot,
        options: &CompileOptions,
        ids: bool,
    ) -> (CanonicalSemanticOutput, CanonicalRirOutput) {
        let parsed = parse_source_snapshot_modules(snapshot).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        let output = analyze_canonical_program(&merged, &rir, options, ids).unwrap();
        (output, rir)
    }

    fn function_fingerprint(
        functions: &[FunctionWithCfg],
        interner: &crate::ThreadedRodeo,
    ) -> Vec<String> {
        functions
            .iter()
            .map(|function| {
                format!(
                    "{}|{}",
                    function.analyzed.name,
                    function.cfg.display_with_interner(interner)
                )
            })
            .collect()
    }

    #[test]
    fn specialization_origins_preserve_exact_generic_base_and_arguments() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                r#"fn id(comptime n: i32, value: i32) -> i32 { value + n }
                   fn wrap(comptime n: i32, value: i32) -> i32 { id(n, value) }
                   fn main() -> i32 {
                       wrap(1, 1) + wrap(1, 2) + id(2, 3)
                   }"#,
            )],
            1,
        );
        let (output, _) = canonical(&source, &CompileOptions::default(), false);
        let origins = output.specialized_free_function_origins();
        let wraps = origins
            .iter()
            .filter(|origin| origin.base_name == "wrap")
            .collect::<Vec<_>>();
        let ids = origins
            .iter()
            .filter(|origin| origin.base_name == "id")
            .collect::<Vec<_>>();
        assert_eq!(wraps.len(), 1, "identical specialization deduplicates");
        assert_eq!(ids.len(), 2, "direct and later-fixpoint specializations");
        assert!(ids.iter().all(|origin| origin.base_file == 1));
        assert_ne!(ids[0].value_arguments, ids[1].value_arguments);
        assert!(
            origins
                .iter()
                .all(|origin| origin.specialized_name != origin.base_name)
        );
        assert!(
            output
                .functions()
                .iter()
                .all(|function| function.analyzed.name != "id" && function.analyzed.name != "wrap")
        );
        assert_eq!(
            output.work().body_analysis.specialized_origin_records,
            origins.len()
        );
    }

    #[test]
    fn recursive_specialization_origins_are_deduplicated() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                r#"fn fib(comptime n: i32) -> i32 {
                       if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
                   }
                   fn main() -> i32 { fib(5) + fib(5) }"#,
            )],
            1,
        );
        let (output, _) = canonical(&source, &CompileOptions::default(), false);
        let origins = output
            .specialized_free_function_origins()
            .iter()
            .filter(|origin| origin.base_name == "fib")
            .collect::<Vec<_>>();
        assert_eq!(origins.len(), 6, "fib(0) through fib(5), each exactly once");
        assert!(origins.iter().all(|origin| origin.base_file == 1));
        assert_eq!(
            output.work().body_analysis.specialized_origin_records,
            origins.len()
        );
    }

    #[test]
    fn sibling_same_name_specializations_retain_distinct_base_files() {
        let source = snapshot(
            &[
                (
                    9,
                    "/p/main.rue",
                    "main.rue",
                    r#"const left = @import("left.rue");
                       const right = @import("right.rue");
                       fn main() -> i32 { left.id(1, 20) + right.id(2, 20) }"#,
                ),
                (
                    3,
                    "/p/left.rue",
                    "left.rue",
                    "pub fn id(comptime n: i32, value: i32) -> i32 { value + n }",
                ),
                (
                    7,
                    "/p/right.rue",
                    "right.rue",
                    "pub fn id(comptime n: i32, value: i32) -> i32 { value + n }",
                ),
            ],
            9,
        );
        let (output, _) = canonical(&source, &CompileOptions::default(), false);
        let origins = output
            .specialized_free_function_origins()
            .iter()
            .filter(|origin| origin.base_name == "id")
            .collect::<Vec<_>>();
        assert_eq!(origins.len(), 2);
        assert_eq!(
            origins
                .iter()
                .map(|origin| origin.base_file)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([3, 7])
        );
    }

    #[test]
    fn specialized_free_function_origin_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<rue_air::SpecializedFreeFunctionOrigin>();
    }

    #[test]
    fn canonical_semantic_query_provides_backend_artifacts() {
        let source = snapshot(
            &[
                (
                    9,
                    "/p/main.rue",
                    "main.rue",
                    "const helper = @import(\"helper.rue\"); fn main() -> i32 { helper.answer() }",
                ),
                (
                    3,
                    "/p/helper.rue",
                    "helper.rue",
                    "pub fn answer() -> i32 { 42 }",
                ),
            ],
            9,
        );
        let options = CompileOptions::default();
        let (canonical, canonical_rir) = canonical(&source, &options, false);
        assert_eq!(
            canonical.work().body_owner_tokens,
            BodyOwnerTokenWork {
                provisional_slots: 2,
                authoritative_slots: 2,
                slots_validated: 2,
                tokens_installed: 2,
                validation_failures: 0,
            }
        );
        let functions = function_fingerprint(
            canonical.functions(),
            canonical_rir.semantic_symbols().interner(),
        );
        assert_eq!(functions.len(), 2);
        assert!(functions.iter().any(|function| function.contains("main")));
        assert!(functions.iter().any(|function| function.contains("answer")));
        let _type_pool = canonical.type_pool();
        assert!(canonical.strings().is_empty());
        assert!(canonical.warnings().is_empty());
        assert_eq!(canonical.work().binding.bind_invocations, 1);
        assert_eq!(canonical.work().manifest.build_invocations, 1);
        assert!(canonical.bound_definitions().is_none());
    }

    #[test]
    fn requesting_ids_materializes_manifest_without_rebinding() {
        let source = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "struct Value { n: i32 } fn main() -> i32 { 42 }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let (ordinary, ordinary_rir) = canonical(&source, &options, false);
        let (with_ids, with_ids_rir) = canonical(&source, &options, true);
        assert_eq!(ordinary.work().binding.bind_invocations, 1);
        assert_eq!(with_ids.work().binding.bind_invocations, 1);
        assert_eq!(ordinary.work().manifest.build_invocations, 1);
        assert_eq!(with_ids.work().manifest.build_invocations, 1);
        assert!(ordinary.bound_definitions().is_none());
        assert!(with_ids.bound_definitions().is_some());
        assert_eq!(
            function_fingerprint(
                ordinary.functions(),
                ordinary_rir.semantic_symbols().interner()
            ),
            function_fingerprint(
                with_ids.functions(),
                with_ids_rir.semantic_symbols().interner()
            )
        );
    }

    fn irrelevant_declarations(count: usize) -> CanonicalSemanticWork {
        let mut source = String::from("fn main() -> i32 { 42 }");
        for index in 0..count {
            source.push_str(&format!(" fn irrelevant{index}() -> i32 {{ {index} }}"));
        }
        let snapshot = snapshot(&[(1, "/main.rue", "main.rue", &source)], 1);
        canonical(&snapshot, &CompileOptions::default(), false)
            .0
            .work()
    }

    #[test]
    fn binding_and_reachable_dispatch_are_constant_with_128_irrelevant_declarations() {
        let one = irrelevant_declarations(1);
        let many = irrelevant_declarations(128);
        assert_eq!(one.binding.bind_invocations, 1);
        assert_eq!(many.binding.bind_invocations, 1);
        assert_eq!(one.declaration_index.build_invocations, 1);
        assert_eq!(many.declaration_index.build_invocations, 1);
        assert_eq!(one.manifest.build_invocations, 1);
        assert_eq!(many.manifest.build_invocations, 1);
        assert_eq!(
            one.body_analysis.free_function_record_lookups,
            many.body_analysis.free_function_record_lookups
        );
        assert_eq!(one.body_analysis.reachable_declaration_rir_visits, 0);
        assert_eq!(many.body_analysis.reachable_declaration_rir_visits, 0);
        assert!(many.binding.input_rir_instructions > one.binding.input_rir_instructions);
        assert!(many.binding.indexed_free_functions > one.binding.indexed_free_functions);
    }

    fn named_method_capture_with_irrelevant_declarations(count: usize) -> CanonicalSemanticWork {
        let mut source = String::from(
            "fn helper() -> i32 { 1 } struct Value { fn run(borrow self) -> i32 { helper() } } fn main() -> i32 { let value = Value {}; value.run() }",
        );
        for index in 0..count {
            source.push_str(&format!(" fn irrelevant{index}() -> i32 {{ {index} }}"));
        }
        let snapshot = snapshot(&[(1, "/main.rue", "main.rue", &source)], 1);
        canonical(&snapshot, &CompileOptions::default(), false)
            .0
            .work()
    }

    #[test]
    fn named_method_capture_work_is_constant_with_128_irrelevant_declarations() {
        let one = named_method_capture_with_irrelevant_declarations(1);
        let many = named_method_capture_with_irrelevant_declarations(128);
        assert_eq!(one.body_analysis.named_method_dependency_events, 1);
        assert_eq!(many.body_analysis.named_method_dependency_events, 1);
        assert_eq!(one.body_analysis.named_method_record_lookups, 1);
        assert_eq!(many.body_analysis.named_method_record_lookups, 1);
        assert_eq!(one.body_analysis.reachable_declaration_rir_visits, 0);
        assert_eq!(many.body_analysis.reachable_declaration_rir_visits, 0);
    }

    #[test]
    fn comptime_named_methods_are_single_runtime_bodies_not_specializations() {
        let source = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn helper() -> i32 { 1 } struct Value { fn choose(borrow self, comptime n: i32) -> i32 { helper() + n } } fn main() -> i32 { let value = Value {}; value.choose(1) + value.choose(2) }",
            )],
            1,
        );
        let (output, _) = canonical(&source, &CompileOptions::default(), false);
        assert_eq!(
            output
                .functions()
                .iter()
                .filter(|function| function.analyzed.name.ends_with("Value.choose"))
                .count(),
            1
        );
        assert!(output.specialized_free_function_origins().is_empty());
        assert_eq!(output.named_method_dependencies().len(), 1);
        assert!(output.generic_named_method_dependencies_complete());
    }

    #[test]
    fn codegen_input_tracks_root_paths_and_options_but_not_linker() {
        let sources = [
            (
                1,
                "/old/main.rue",
                "main.rue",
                "const h = @import(\"helper.rue\"); fn main() -> i32 { h.helper() }",
            ),
            (
                2,
                "/old/helper.rue",
                "helper.rue",
                "pub fn helper() -> i32 { 42 }",
            ),
        ];
        let base_snapshot = snapshot(&sources, 1);
        let base_options = CompileOptions::default();
        let (base, _) = canonical(&base_snapshot, &base_options, false);

        let mut linker = base_options.clone();
        linker.linker = crate::LinkerMode::System("clang".to_owned());
        let (linker, _) = canonical(&base_snapshot, &linker, false);
        assert_eq!(base.input(), linker.input());

        let mut optimized = base_options.clone();
        optimized.opt_level = crate::OptLevel::O1;
        let (optimized, _) = canonical(&base_snapshot, &optimized, false);
        assert_ne!(base.input(), optimized.input());

        let relocated = snapshot(
            &[
                (1, "/new/main.rue", "main.rue", sources[0].3),
                (2, "/new/helper.rue", "helper.rue", sources[1].3),
            ],
            1,
        );
        let (relocated, _) = canonical(&relocated, &base_options, false);
        assert_ne!(base.input(), relocated.input());

        let different_root = snapshot(&sources, 2);
        let (different_root, _) = canonical(&different_root, &base_options, false);
        assert_ne!(base.input(), different_root.input());
    }

    #[test]
    fn cfg_work_is_exact_and_distinguishes_optimized_levels() {
        let source = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
            )],
            1,
        );
        let o0_options = CompileOptions::default();
        let (o0, _) = canonical(&source, &o0_options, false);
        let mut o1_options = o0_options.clone();
        o1_options.opt_level = crate::OptLevel::O1;
        let (o1, _) = canonical(&source, &o1_options, false);

        let o0 = o0.work().cfg;
        let o1 = o1.work().cfg;
        assert_eq!(o0.functions_considered, 2);
        assert_eq!(o0.comptime_functions_filtered, 0);
        assert_eq!(o0.drop_glue_functions_synthesized, 0);
        assert_eq!(o0.cfg_builds_attempted, 2);
        assert_eq!(o0.cfg_builds_succeeded, 2);
        assert_eq!(o0.cfg_builds_failed, 0);
        assert_eq!(o0.optimization_attempts, 2);
        assert_eq!(o0.optimization_completions, 2);
        assert_eq!(o0.optimized_level_attempts, 0);

        assert_eq!(o1.cfg_builds_attempted, o0.cfg_builds_attempted);
        assert_eq!(o1.cfg_builds_succeeded, o0.cfg_builds_succeeded);
        assert_eq!(o1.cfg_builds_failed, o0.cfg_builds_failed);
        assert_eq!(o1.optimization_attempts, 2);
        assert_eq!(o1.optimization_completions, 2);
        assert_eq!(o1.optimized_level_attempts, 2);
    }
}
