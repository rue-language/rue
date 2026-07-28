//! One-pass canonical declaration binding, body analysis, and CFG lowering.

use std::{collections::BTreeMap, sync::Arc};

use rue_air::{
    AnalyzedBodyOwnerEvent, BodyAnalysisWork, BodyNamedDependencyEvent, DeclarationBindingWork,
    DeclarationBuiltinTypeCallHeadDependencyEvent, DeclarationTypeCallHeadDependencyEvent,
    DeclarationTypeDependencyEvent, NamedConstDependencyEvent, NamedDestructorDependencyEvent,
    NamedMethodDependencyEvent, OrdinaryFreeFunctionDependencyEvent, PerBodyDeclarationContextWork,
    RirDeclarationIndexWork, SemanticBindingManifestWork, SpecializedFreeFunctionDependencyEvent,
    SpecializedFreeFunctionOrigin,
};
use tracing::info_span;

/// Request-scoped memo for the body-invariant durable declaration projection
/// (RUE-1132).
///
/// `project_durable_declaration_semantics` is a pure function of inputs that are
/// all revision-scoped — the merged program, the prepared shell records and
/// their provisional definition universe, and the query-owned durable
/// declarations. None of them carries a body key, so every reached body used to
/// recompute a bit-for-bit identical value. This memo computes it once per
/// semantic request and hands the shared `Arc` to each body.
///
/// It is **not** a dependency authority and holds no lease: it is created inside
/// one rooted attempt, dropped with it, and pins the revision and configuration
/// it was filled for. A request presenting a different revision/configuration is
/// a programming error, not a cache miss — the body fails closed rather than
/// recomputing into a slot that belongs to another revision. Because the value
/// is owned data (`Arc<[SemanticDeclarationExport]>`) rather than a semantic
/// epoch, sharing it retains no mutable declaration state across bodies.
///
/// The projection lock is never held across the projection itself: a miss
/// releases the lock, projects, then re-locks to publish. Two concurrent misses
/// may therefore both project — deterministically producing equal values — and
/// the first writer wins, mirroring `recipe_cache`'s claim discipline.
pub(crate) struct SharedDeclarationProjection {
    filled: std::sync::Mutex<Option<FilledDeclarationProjection>>,
}

struct FilledDeclarationProjection {
    revision: crate::SourceRevision,
    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    exports: Arc<[rue_air::SemanticDeclarationExport]>,
}

/// Why a body's projection request did not reuse the shared value. The stage
/// charges its projection work only on a `Computed` outcome, so a reused
/// projection drops the per-body counter at the stage's own source rather than
/// being subtracted afterwards.
enum SharedProjectionOutcome {
    Computed(
        Arc<[rue_air::SemanticDeclarationExport]>,
        crate::durable_semantics::DurableSemanticProjectionWork,
    ),
    Reused(Arc<[rue_air::SemanticDeclarationExport]>),
}

impl SharedDeclarationProjection {
    pub(crate) fn new() -> Self {
        Self {
            filled: std::sync::Mutex::new(None),
        }
    }

    /// The declaration projection for this request, computing it on first
    /// demand. `project` runs outside the lock.
    fn get_or_project(
        &self,
        revision: &crate::SourceRevision,
        configuration: &crate::semantic_query_nucleus::SemanticQueryConfiguration,
        project: impl FnOnce() -> Result<
            (
                Arc<[rue_air::SemanticDeclarationExport]>,
                crate::durable_semantics::DurableSemanticProjectionWork,
            ),
            crate::durable_semantics::DurableSemanticProjectionFailure,
        >,
    ) -> Result<SharedProjectionOutcome, SharedProjectionFailure> {
        {
            let filled = self
                .filled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(filled) = filled.as_ref() {
                if filled.revision != *revision || filled.configuration != *configuration {
                    return Err(SharedProjectionFailure::ForeignRequest);
                }
                return Ok(SharedProjectionOutcome::Reused(filled.exports.clone()));
            }
        }
        let (exports, work) = project().map_err(SharedProjectionFailure::Projection)?;
        let mut filled = self
            .filled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match filled.as_ref() {
            // A concurrent miss published first. Its value is equal by
            // construction, so adopt it and keep one shared identity.
            Some(existing)
                if existing.revision == *revision && existing.configuration == *configuration =>
            {
                let exports = existing.exports.clone();
                drop(filled);
                Ok(SharedProjectionOutcome::Computed(exports, work))
            }
            Some(_) => Err(SharedProjectionFailure::ForeignRequest),
            None => {
                *filled = Some(FilledDeclarationProjection {
                    revision: revision.clone(),
                    configuration: configuration.clone(),
                    exports: exports.clone(),
                });
                drop(filled);
                Ok(SharedProjectionOutcome::Computed(exports, work))
            }
        }
    }
}

#[derive(Debug)]
enum SharedProjectionFailure {
    Projection(crate::durable_semantics::DurableSemanticProjectionFailure),
    ForeignRequest,
}

pub(crate) struct PreparedDurableBodyCandidate {
    pub owner: crate::StableDefinitionKey,
    pub body_span: rue_span::Span,
    pub body: rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
}

pub(crate) struct PreparedDurableSpecializedBodyCandidate {
    pub instance: crate::FunctionInstanceKey,
    pub identity:
        rue_air::SemanticSpecializationIdentity<crate::StableDefinitionKey, crate::ModuleId>,
    pub body_span: rue_span::Span,
    pub body: rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
}

pub(crate) struct PreparedDurableAnonymousBodyCandidate {
    pub identity: crate::FunctionInstanceKey,
    pub body_span: rue_span::Span,
    pub body: rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
}

fn fold_body_import_work(durable: &mut crate::DurableBodyWork, body: BodyAnalysisWork) {
    durable.import_attempts += body.ordinary_body_import_attempts;
    durable.import_successes += body.ordinary_body_import_successes;
    if let Some(reason) = body.last_ordinary_body_import_failure {
        durable.record_import_failure(reason, body.ordinary_body_import_failures);
    }
    durable.atomic_discards += body.ordinary_body_import_atomic_discards;
    durable.candidate_fallbacks += body.ordinary_body_import_failures;
    durable.installed_instructions += body.ordinary_body_import_instructions_installed;
    durable.installed_places += body.ordinary_body_import_places_installed;
    durable.installed_strings += body.ordinary_body_import_strings_installed;
}

#[cfg(test)]
use std::cell::Cell;

use crate::{
    BoundDefinitionSet, BoundDefinitionWork, CanonicalImportGraph, CanonicalMergedProgram,
    CanonicalRirOutput, CodegenInputDescriptor, CompileOptions, CompileWarning,
    DurableDeclarationSemantic, FrozenTypeInternPool, FunctionWithCfg, MultiErrorResult,
    SemanticInputDescriptor,
    bound_definitions::{
        configure_canonical_sema, issue_bound_definitions, issue_shell_definitions,
    },
    build_functions_and_cfgs,
};

#[cfg(test)]
thread_local! {
    static INJECT_CFG_FAILURE: Cell<bool> = const { Cell::new(false) };
    static INJECT_DECLARATION_FAILURE: Cell<bool> = const { Cell::new(false) };
    static INJECT_AUTHORITATIVE_KEY_MISMATCH: Cell<bool> = const { Cell::new(false) };
    static INJECT_BODY_QUERY_STAGE_FAILURE: Cell<bool> = const { Cell::new(false) };
    static INJECT_OVERLAY_CANCEL_AFTER_SEED: Cell<bool> = const { Cell::new(false) };
}

/// Force the test-only overlay driver to observe a cancellation after it has
/// seeded the overlay's local token space but before it analyzes the body, so
/// the mid-analysis cancellation path runs against a populated overlay.
#[cfg(test)]
pub(crate) fn with_test_overlay_cancel_after_seed_injection<T>(run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INJECT_OVERLAY_CANCEL_AFTER_SEED.with(|enabled| enabled.set(false));
        }
    }

    INJECT_OVERLAY_CANCEL_AFTER_SEED.with(|enabled| {
        assert!(
            !enabled.replace(true),
            "overlay cancel-after-seed injection is not nestable"
        );
    });
    let _reset = Reset;
    run()
}

#[cfg(test)]
pub(crate) fn with_test_cfg_failure_injection<T>(run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INJECT_CFG_FAILURE.with(|enabled| enabled.set(false));
        }
    }

    INJECT_CFG_FAILURE.with(|enabled| {
        assert!(
            !enabled.replace(true),
            "CFG failure injection is not nestable"
        );
    });
    let _reset = Reset;
    run()
}

#[cfg(test)]
pub(crate) fn with_test_declaration_failure_injection<T>(run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INJECT_DECLARATION_FAILURE.with(|enabled| enabled.set(false));
        }
    }

    INJECT_DECLARATION_FAILURE.with(|enabled| {
        assert!(
            !enabled.replace(true),
            "declaration failure injection is not nestable"
        );
    });
    let _reset = Reset;
    run()
}

#[cfg(test)]
pub(crate) fn with_test_body_query_stage_failure_injection<T>(run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INJECT_BODY_QUERY_STAGE_FAILURE.with(|enabled| enabled.set(false));
        }
    }

    INJECT_BODY_QUERY_STAGE_FAILURE.with(|enabled| {
        assert!(
            !enabled.replace(true),
            "body query stage failure injection is not nestable"
        );
    });
    let _reset = Reset;
    run()
}

#[cfg(test)]
pub(crate) fn with_test_authoritative_key_mismatch<T>(run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INJECT_AUTHORITATIVE_KEY_MISMATCH.with(|enabled| enabled.set(false));
        }
    }

    INJECT_AUTHORITATIVE_KEY_MISMATCH.with(|enabled| {
        assert!(
            !enabled.replace(true),
            "authoritative-key mismatch injection is not nestable"
        );
    });
    let _reset = Reset;
    run()
}

#[cfg(test)]
fn inject_cfg_failure(sema_output: &mut rue_air::SemaOutput, interner: &crate::ThreadedRodeo) {
    use rue_air::{AirEditor, AirValidationContext, Type};
    use rue_span::Span;

    if !INJECT_CFG_FAILURE.with(Cell::get) {
        return;
    }
    let function = sema_output
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("test CFG injection requires main");
    let mut air = AirEditor::new(Type::I32);
    let call = air
        .add_call_generic(
            interner.get_or_intern("test_unrewritten_generic"),
            &[],
            &[],
            &[],
            Type::I32,
            Span::new(0, 1),
        )
        .unwrap();
    air.add_ret(Some(call), Type::I32, Span::new(0, 1));
    function.air = air
        .finish(AirValidationContext::Canonical(&sema_output.type_pool))
        .expect("injected AIR must validate");
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
    /// Candidates rejected before projection because their canonical algebra
    /// version or compiler implementation epoch is incompatible.
    pub schema_version_rejections: usize,
    pub unsupported_export_fallbacks: usize,
    pub stable_join_fallbacks: usize,
    pub structural_validation_fallbacks: usize,
    pub unsupported_import_fallbacks: usize,
    /// Actual request-local semantic epochs constructed, including a fresh
    /// epoch required after a consuming installation failure.
    pub semantic_epochs_started: usize,
    pub declaration_indexes_built: usize,
    pub shell_predeclaration_epochs: usize,
    pub durable_cache_population_exports: usize,
    pub fallback_epochs_started: usize,
}

pub(crate) fn prepare_query_declaration_shells<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    options: &CompileOptions,
    imports: &CanonicalImportGraph,
    query_shells: &[rue_air::SemanticDeclarationShell],
) -> Result<CanonicalPreparedDeclarations<'a>, CanonicalSemanticFailure> {
    let sema = configure_timed_canonical_sema(merged, rir, options, imports).map_err(|errors| {
        CanonicalSemanticFailure::declaration(errors, CanonicalSemanticWork::default())
    })?;
    let declaration_index = sema.rir_declaration_index_work();
    let declaration_reuse = CanonicalDeclarationReuseWork {
        semantic_epochs_started: 1,
        declaration_indexes_built: declaration_index.build_invocations,
        shell_predeclaration_epochs: 1,
        ..CanonicalDeclarationReuseWork::default()
    };
    let shells = sema
        .predeclare_imported_declaration_shells(query_shells)
        .map_err(|errors| {
            CanonicalSemanticFailure::declaration(
                errors,
                declaration_stage_work(
                    declaration_index,
                    DeclarationBindingWork::default(),
                    SemanticBindingManifestWork::default(),
                    BodyOwnerTokenWork::default(),
                    BodyAnalysisWork::default(),
                    false,
                    declaration_reuse,
                ),
            )
        })?;
    let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
    let definitions = match issue_shell_definitions(merged, rir.source_revision(), &shell_records) {
        Ok(definitions) => definitions,
        Err(preparation_error) => {
            return Err(CanonicalSemanticFailure::declaration(
                crate::CompileErrors::from(preparation_error),
                declaration_stage_work(
                    declaration_index,
                    DeclarationBindingWork::default(),
                    SemanticBindingManifestWork::default(),
                    BodyOwnerTokenWork::default(),
                    BodyAnalysisWork::default(),
                    false,
                    declaration_reuse,
                ),
            ));
        }
    };
    let shells = shells
        .install_stable_identity_endpoints(
            &definitions.semantic_definition_endpoints(),
            &definitions.semantic_module_endpoints(merged),
        )
        .map_err(|failure| {
            CanonicalSemanticFailure::declaration(
                crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(format!(
                        "failed to install provisional stable identity endpoints: {failure:?}"
                    )),
                )),
                CanonicalSemanticWork::default(),
            )
        })?;
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

    pub(crate) fn declaration_index_work(&self) -> RirDeclarationIndexWork {
        self.declaration_index
    }

    pub(crate) fn declaration_reuse_work(&self) -> CanonicalDeclarationReuseWork {
        CanonicalDeclarationReuseWork {
            semantic_epochs_started: 1,
            declaration_indexes_built: self.declaration_index.build_invocations,
            shell_predeclaration_epochs: 1,
            ..CanonicalDeclarationReuseWork::default()
        }
    }
}

/// The shared test recipe under [`query_owned_declaration_shells_for_test`] and
/// [`bind_query_owned_declarations_with_definitions_for_test`]: project the
/// query-owned declaration shells and prepare them through the production
/// `prepare_query_declaration_shells` path.
#[cfg(test)]
fn prepared_query_declarations_for_test<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    preview_features: rue_error::PreviewFeatures,
    target: crate::Target,
    imports: &CanonicalImportGraph,
) -> MultiErrorResult<CanonicalPreparedDeclarations<'a>> {
    let options = CompileOptions {
        preview_features,
        target,
        ..CompileOptions::default()
    };
    let query_shells =
        crate::revisioned_query_database::projected_declaration_shells_for_test(merged)?;
    prepare_query_declaration_shells(merged, rir, &options, imports, &query_shells)
        .map_err(|failure| failure.errors)
}

#[cfg(test)]
pub(crate) fn query_owned_declaration_shells_for_test<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    preview_features: rue_error::PreviewFeatures,
    target: crate::Target,
    imports: &CanonicalImportGraph,
) -> MultiErrorResult<rue_air::DeclarationShells<'a>> {
    let prepared =
        prepared_query_declarations_for_test(merged, rir, preview_features, target, imports)?;
    Ok(prepared.shells)
}

#[cfg(test)]
pub(crate) fn bind_query_owned_declarations_for_test<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    preview_features: rue_error::PreviewFeatures,
    target: crate::Target,
    imports: &CanonicalImportGraph,
) -> MultiErrorResult<rue_air::BoundSema<'a>> {
    let prepared =
        prepared_query_declarations_for_test(merged, rir, preview_features, target, imports)?;
    let CanonicalPreparedDeclarations {
        shells,
        definitions,
        ..
    } = prepared;
    let body_owner_endpoints = definitions.body_owner_endpoints();
    Ok(shells
        .resolve_declarations_for_test()?
        .install_body_owner_tokens(&body_owner_endpoints)
        .expect("query-owned test declarations install their owner endpoints"))
}

/// The [`bind_query_owned_declarations_for_test`] recipe, additionally handing
/// back the [`BoundDefinitionSet`] whose stable identity endpoints were
/// installed into the returned epoch. The RUE-1091 r6c differential needs both:
/// the production well-known `Option` projection
/// (`project_durable_option_registry`) resolves durable keys through the SAME
/// definition set that issued the epoch's endpoints, exactly as the production
/// `body_transaction` install does.
#[cfg(test)]
pub(crate) fn bind_query_owned_declarations_with_definitions_for_test<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    preview_features: rue_error::PreviewFeatures,
    target: crate::Target,
    imports: &CanonicalImportGraph,
) -> MultiErrorResult<(rue_air::BoundSema<'a>, BoundDefinitionSet)> {
    let prepared =
        prepared_query_declarations_for_test(merged, rir, preview_features, target, imports)?;
    let CanonicalPreparedDeclarations {
        shells,
        definitions,
        ..
    } = prepared;
    let body_owner_endpoints = definitions.body_owner_endpoints();
    let bound = shells
        .resolve_declarations_for_test()?
        .install_body_owner_tokens(&body_owner_endpoints)
        .expect("query-owned test declarations install their owner endpoints");
    Ok((bound, definitions))
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
    /// Durable body comparison, import, export, reuse, and fallback work.
    pub durable_bodies: crate::DurableBodyWork,
    /// Drop-glue, CFG construction, and optimization work.
    pub cfg: CfgConstructionWork,
    /// Whether this request asked for stable source definition IDs.
    pub stable_ids_requested: bool,
    pub declaration_reuse: CanonicalDeclarationReuseWork,
}

impl CanonicalSemanticWork {
    pub(crate) fn accrue_body_query_work(&mut self, query: BodyAnalysisWork) {
        let body = &mut self.body_analysis;
        body.body_analyses_computed += query.body_analyses_computed;
        body.body_analyses_reused += query.body_analyses_reused;
        body.body_analyses_invalidated += query.body_analyses_invalidated;
        body.bodies_attempted += query.bodies_attempted;
        body.bodies_succeeded += query.bodies_succeeded;
        body.bodies_failed += query.bodies_failed;
        // Coordinator traversal metrics accrue once per semantic request; the
        // restart/deferral counters are running totals while the closure-size
        // and specialization-depth fields are completion snapshots merged by
        // maximum.
        body.closure_restarts += query.closure_restarts;
        body.deferred_producer_retries += query.deferred_producer_retries;
        body.per_body_declaration_context.cold_body_preparations +=
            query.per_body_declaration_context.cold_body_preparations;
        body.per_body_declaration_context.declarations_inspected +=
            query.per_body_declaration_context.declarations_inspected;
        body.per_body_declaration_context.modules_registered +=
            query.per_body_declaration_context.modules_registered;
        body.per_body_declaration_context.rir_indexes_constructed +=
            query.per_body_declaration_context.rir_indexes_constructed;
        body.per_body_declaration_context.rir_instructions_visited +=
            query.per_body_declaration_context.rir_instructions_visited;
        body.per_body_declaration_context.shells_prepared +=
            query.per_body_declaration_context.shells_prepared;
        body.per_body_declaration_context.semantics_installed +=
            query.per_body_declaration_context.semantics_installed;
        body.per_body_declaration_context.projections_performed +=
            query.per_body_declaration_context.projections_performed;
        body.per_body_declaration_context
            .durable_source_records_inspected += query
            .per_body_declaration_context
            .durable_source_records_inspected;
        body.per_body_declaration_context
            .durable_source_records_copied += query
            .per_body_declaration_context
            .durable_source_records_copied;
        body.per_body_declaration_context.endpoints_installed +=
            query.per_body_declaration_context.endpoints_installed;
        body.per_body_declaration_context.declaration_bases_built +=
            query.per_body_declaration_context.declaration_bases_built;
        body.per_body_declaration_context.body_epochs_derived +=
            query.per_body_declaration_context.body_epochs_derived;
        body.per_body_declaration_context.declaration_units_shared +=
            query.per_body_declaration_context.declaration_units_shared;
        body.per_body_declaration_context.body_local_units_copied +=
            query.per_body_declaration_context.body_local_units_copied;
        body.closure_bodies_visited = body
            .closure_bodies_visited
            .max(query.closure_bodies_visited);
        body.max_specialization_depth = body
            .max_specialization_depth
            .max(query.max_specialization_depth);
        body.air_instructions_produced += query.air_instructions_produced;
        body.local_strings_produced += query.local_strings_produced;
        body.ordinary_body_exports_attempted += query.ordinary_body_exports_attempted;
        body.ordinary_body_exports_succeeded += query.ordinary_body_exports_succeeded;
        body.ordinary_body_exports_rejected += query.ordinary_body_exports_rejected;
        body.ordinary_body_export_instructions_emitted +=
            query.ordinary_body_export_instructions_emitted;
        body.ordinary_body_export_places_emitted += query.ordinary_body_export_places_emitted;
        body.ordinary_body_export_strings_emitted += query.ordinary_body_export_strings_emitted;
        body.specialized_bodies_attempted += query.specialized_bodies_attempted;
        body.specialized_bodies_succeeded += query.specialized_bodies_succeeded;
        body.specialized_bodies_failed += query.specialized_bodies_failed;
        body.specialized_body_exports_attempted += query.specialized_body_exports_attempted;
        body.specialized_body_exports_succeeded += query.specialized_body_exports_succeeded;
        body.specialized_body_exports_rejected += query.specialized_body_exports_rejected;
        body.specialized_body_export_instructions_emitted +=
            query.specialized_body_export_instructions_emitted;
        body.specialized_body_export_places_emitted += query.specialized_body_export_places_emitted;
        body.specialized_body_export_strings_emitted +=
            query.specialized_body_export_strings_emitted;

        let durable = &mut self.durable_bodies;
        durable.export_attempts +=
            query.ordinary_body_exports_attempted + query.specialized_body_exports_attempted;
        durable.export_successes +=
            query.ordinary_body_exports_succeeded + query.specialized_body_exports_succeeded;
        durable.export_rejections +=
            query.ordinary_body_exports_rejected + query.specialized_body_exports_rejected;
        durable.conversion_attempts += query.bodies_succeeded;
        durable.conversion_completions += query.bodies_succeeded;
        durable.instructions_exported += query.ordinary_body_export_instructions_emitted
            + query.specialized_body_export_instructions_emitted;
        durable.places_exported += query.ordinary_body_export_places_emitted
            + query.specialized_body_export_places_emitted;
        durable.strings_exported += query.ordinary_body_export_strings_emitted
            + query.specialized_body_export_strings_emitted;
    }
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
    pub cfg_reuse_candidates: usize,
    pub cfg_import_attempts: usize,
    pub cfg_import_successes: usize,
    pub cfg_import_failures: usize,
    pub cfg_schema_version_rejections: usize,
    pub cfg_reuses: usize,
    pub cfg_fallbacks: usize,
    pub cfg_warnings_reused: usize,
    pub implicit_destructor_targets_reused: usize,
    pub cfg_export_attempts: usize,
    pub cfg_export_successes: usize,
    pub cfg_export_rejections: usize,
}

/// The semantic phase that prevented publication of request artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalSemanticFailurePhase {
    Declaration,
    BodyAnalysis,
    CfgConstruction,
}

/// Value-only structural work retained for a failed semantic request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalSemanticFailureWork {
    pub phase: CanonicalSemanticFailurePhase,
    pub work: CanonicalSemanticWork,
}

pub(crate) struct CanonicalSemanticFailure {
    pub(crate) errors: crate::CompileErrors,
    pub(crate) failure: CanonicalSemanticFailureWork,
}

impl CanonicalSemanticFailure {
    fn new(
        phase: CanonicalSemanticFailurePhase,
        errors: crate::CompileErrors,
        work: CanonicalSemanticWork,
    ) -> Self {
        Self {
            errors,
            failure: CanonicalSemanticFailureWork { phase, work },
        }
    }

    pub(crate) fn declaration(errors: crate::CompileErrors, work: CanonicalSemanticWork) -> Self {
        Self::new(CanonicalSemanticFailurePhase::Declaration, errors, work)
    }

    pub(crate) fn body(errors: crate::CompileErrors, work: CanonicalSemanticWork) -> Self {
        Self::new(CanonicalSemanticFailurePhase::BodyAnalysis, errors, work)
    }
}

fn declaration_stage_work(
    declaration_index: RirDeclarationIndexWork,
    binding: DeclarationBindingWork,
    manifest: SemanticBindingManifestWork,
    body_owner_tokens: BodyOwnerTokenWork,
    body_analysis: BodyAnalysisWork,
    stable_ids_requested: bool,
    declaration_reuse: CanonicalDeclarationReuseWork,
) -> CanonicalSemanticWork {
    CanonicalSemanticWork {
        declaration_index,
        binding,
        manifest,
        body_owner_tokens,
        body_analysis,
        stable_ids_requested,
        declaration_reuse,
        ..CanonicalSemanticWork::default()
    }
}

/// Owned semantic and optimized CFG artifacts returned by `CompilerSession`.
#[derive(Debug)]
pub struct CanonicalSemanticOutput {
    input: CodegenInputDescriptor,
    functions: Vec<FunctionWithCfg>,
    type_pool: FrozenTypeInternPool,
    strings: Vec<String>,
    warnings: Vec<CompileWarning>,
    #[cfg_attr(not(test), allow(dead_code))]
    bound_definitions: Option<BoundDefinitionSet>,
    /// Request-independent anonymous nominal identities. The AIR issuer tokens
    /// have already been projected through `body_owner_issuer`; no live pool or
    /// issuer identity crosses this retention boundary.
    anonymous_nominal_associations: Arc<[crate::AnonymousNominalKey]>,
    body_owner_issuer: BoundDefinitionSet,
    durable_ordinary_body_payloads: Arc<[crate::DurableOrdinaryBodyPayload]>,
    durable_specialized_body_payloads: Arc<[crate::DurableSpecializedBodyPayload]>,
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
    body_references: BTreeMap<crate::FunctionInstanceKey, crate::body_query::BodyReferences>,
    durable_cfgs: Arc<[crate::queries::DurableCfgArtifact]>,
}

impl CanonicalSemanticOutput {
    pub(crate) fn unstable_parity_snapshot(&self) -> crate::unstable::SemanticParitySnapshot {
        use std::fmt::Write as _;

        let type_pool = self
            .type_pool
            .all_types()
            .map(|ty| match ty.kind() {
                rue_air::TypeKind::Struct(id) => {
                    format!("struct:{:?}", self.type_pool.struct_def(id))
                }
                rue_air::TypeKind::Enum(id) => {
                    format!("enum:{:?}", self.type_pool.enum_def(id))
                }
                rue_air::TypeKind::Array(id) => {
                    let (element, len) = self.type_pool.array_def(id);
                    format!("array:{element:?}:{len}")
                }
                rue_air::TypeKind::PtrConst(id) => {
                    format!("ptr_const:{:?}", self.type_pool.ptr_const_def(id))
                }
                rue_air::TypeKind::PtrMut(id) => {
                    format!("ptr_mut:{:?}", self.type_pool.ptr_mut_def(id))
                }
                _ => unreachable!("type pool stores only composite types"),
            })
            .collect::<Vec<_>>();
        let mut details = String::new();
        macro_rules! record {
            ($name:literal, $value:expr) => {
                writeln!(&mut details, concat!($name, "={:?}"), $value)
                    .expect("write parity snapshot to String")
            };
        }
        record!("input", &self.input);
        record!("functions", &self.functions);
        record!("type_pool", type_pool);
        record!("bound_definitions", &self.bound_definitions);
        record!(
            "anonymous_nominal_associations",
            &self.anonymous_nominal_associations
        );
        record!("strings", &self.strings);
        record!("warnings", &self.warnings);
        record!("analyzed_body_owners", &self.analyzed_body_owners);
        record!("body_named_dependencies", &self.body_named_dependencies);
        record!(
            "ordinary_free_function_dependencies",
            &self.ordinary_free_function_dependencies
        );
        record!(
            "specialized_free_function_origins",
            &self.specialized_free_function_origins
        );
        record!(
            "specialized_free_function_dependencies",
            &self.specialized_free_function_dependencies
        );
        record!(
            "ordinary_free_function_dependencies_complete",
            self.ordinary_free_function_dependencies_complete
        );
        record!(
            "specialized_free_function_dependencies_complete",
            self.specialized_free_function_dependencies_complete
        );
        record!("named_method_dependencies", &self.named_method_dependencies);
        record!(
            "non_generic_named_method_dependencies_complete",
            self.non_generic_named_method_dependencies_complete
        );
        record!(
            "generic_named_method_dependencies_complete",
            self.generic_named_method_dependencies_complete
        );
        record!(
            "named_destructor_dependencies",
            &self.named_destructor_dependencies
        );
        record!(
            "named_destructor_dependencies_complete",
            self.named_destructor_dependencies_complete
        );
        record!(
            "declaration_type_dependencies",
            &self.declaration_type_dependencies
        );
        record!(
            "declaration_type_dependencies_complete",
            self.declaration_type_dependencies_complete
        );
        record!(
            "declaration_type_call_head_dependencies",
            &self.declaration_type_call_head_dependencies
        );
        record!(
            "declaration_type_call_head_dependencies_complete",
            self.declaration_type_call_head_dependencies_complete
        );
        record!(
            "declaration_builtin_type_call_head_dependencies",
            &self.declaration_builtin_type_call_head_dependencies
        );
        record!(
            "supported_type_call_heads_complete",
            self.supported_type_call_heads_complete
        );
        record!("named_const_dependencies", &self.named_const_dependencies);
        record!(
            "named_value_const_dependencies_complete",
            self.named_value_const_dependencies_complete
        );
        record!(
            "implicit_named_destructor_dependencies",
            &self.implicit_named_destructor_dependencies
        );
        record!(
            "implicit_named_destructor_dependencies_complete",
            self.implicit_named_destructor_dependencies_complete
        );
        record!(
            "durable_artifact_status",
            self.unstable_durable_artifact_status()
        );
        crate::unstable::SemanticParitySnapshot::new(details)
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<FunctionWithCfg>,
        FrozenTypeInternPool,
        Vec<String>,
        Vec<CompileWarning>,
    ) {
        (self.functions, self.type_pool, self.strings, self.warnings)
    }

    /// Exact semantic and optimization identity of this output.
    pub(crate) fn input(&self) -> &CodegenInputDescriptor {
        &self.input
    }
    /// Debug-only identity projection for differential and benchmark tooling.
    pub(crate) fn unstable_input_debug(&self) -> String {
        format!("{:?}", self.input)
    }
    /// Analyzed functions paired with optimized CFGs in machine-symbol order.
    pub fn functions(&self) -> &[FunctionWithCfg] {
        &self.functions
    }
    /// Request-local type universe retained by the semantic output.
    pub fn type_pool(&self) -> &FrozenTypeInternPool {
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

    pub(crate) fn accrue_body_query_work(&mut self, query: BodyAnalysisWork) {
        self.work.accrue_body_query_work(query);
    }
    pub(crate) fn install_body_references(
        &mut self,
        references: BTreeMap<crate::FunctionInstanceKey, crate::body_query::BodyReferences>,
    ) {
        self.body_references = references;
    }
    pub(crate) fn body_references(
        &self,
        function: &crate::FunctionInstanceKey,
    ) -> Option<&crate::body_query::BodyReferences> {
        self.body_references.get(function)
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
    pub(crate) fn durable_cfgs(&self) -> &Arc<[crate::queries::DurableCfgArtifact]> {
        &self.durable_cfgs
    }
    /// Stable definition identities when requested for this run.
    #[cfg(test)]
    pub(crate) fn bound_definitions(&self) -> Option<&BoundDefinitionSet> {
        self.bound_definitions.as_ref()
    }
    pub(crate) fn body_owner_issuer(&self) -> &BoundDefinitionSet {
        &self.body_owner_issuer
    }
    pub(crate) fn durable_ordinary_body_payloads(&self) -> &[crate::DurableOrdinaryBodyPayload] {
        &self.durable_ordinary_body_payloads
    }

    #[cfg(test)]
    pub(crate) fn durable_specialized_body_payloads(
        &self,
    ) -> &[crate::DurableSpecializedBodyPayload] {
        &self.durable_specialized_body_payloads
    }

    /// Explicitly unstable equality status for durable-cache instrumentation.
    pub(crate) fn unstable_durable_artifact_status(
        &self,
    ) -> crate::unstable::DurableArtifactStatus {
        crate::unstable::DurableArtifactStatus::from_debug(&self.durable_specialized_body_payloads)
    }
    /// Structural work performed by the request that computed this output.
    /// An exact-cycle caller may receive a memoized output without executing a
    /// new semantic request, so these fields can describe historical work from
    /// the request that originally produced the retained result.
    pub(crate) fn work(&self) -> CanonicalSemanticWork {
        self.work
    }
    /// Return an owned snapshot of explicitly unstable semantic work metrics.
    pub fn unstable_metrics(&self) -> crate::unstable::SemanticMetrics {
        crate::unstable::SemanticMetrics::from_work(self.work)
    }
}

/// Bind declarations once, optionally issue stable IDs, then consume the same
/// transient bound Sema for body analysis and CFG construction.
#[cfg(test)]
pub(crate) fn analyze_canonical_program_for_test_support(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    options: &CompileOptions,
    imports: &CanonicalImportGraph,
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
    let query_shells =
        crate::revisioned_query_database::projected_declaration_shells_for_test(merged)?;
    let prepared = prepare_query_declaration_shells(merged, rir, options, imports, &query_shells)
        .map_err(|failure| failure.errors)?;
    let CanonicalPreparedDeclarations {
        shells,
        shell_records: _,
        definitions,
        declaration_index,
    } = prepared;
    let bound = shells.resolve_declarations_for_test()?;
    finish_canonical_analysis_with(
        input,
        merged,
        rir,
        options,
        request_stable_ids,
        declaration_index,
        bound,
        definitions,
        CanonicalDeclarationReuseWork::default(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Arc::from([]),
        crate::DurableBodyWork::default(),
        info_span!("sema", phase = "semantic_analysis").entered(),
        |bound, _candidates, _definitions, _merged| {
            bound
                .analyze_all_bodies_for_test()
                .map_err(|errors| CanonicalBodyCompositionFailure {
                    errors,
                    work: BodyAnalysisWork::default(),
                })
        },
    )
    .map_err(|failure| failure.errors)
}

/// Analyze bodies in a fresh semantic epoch whose declaration payloads come
/// from the canonical keyed semantic nucleus. Projection and installation are
/// invariant checks, not a compatibility choice: a failure is terminal and
/// must never revive the removed ordinary declaration resolver.
pub(crate) fn analyze_prepared_canonical_program_reusing_declarations(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    options: &CompileOptions,
    _imports: &CanonicalImportGraph,
    prepared: CanonicalPreparedDeclarations<'_>,
    definitions: &BoundDefinitionSet,
    durable: &[DurableDeclarationSemantic],
    anonymous_nominals: &[crate::durable_semantics::DurableAnonymousNominal],
    declaration_dependencies: &[crate::semantic_query_nucleus::SemanticDeclarationDependency],
    body_candidates: Vec<PreparedDurableBodyCandidate>,
    specialized_body_candidates: Vec<PreparedDurableSpecializedBodyCandidate>,
    anonymous_body_candidates: Vec<PreparedDurableAnonymousBodyCandidate>,
    durable_cfg_candidates: Arc<[crate::queries::DurableCfgArtifact]>,
    body_work: crate::DurableBodyWork,
) -> Result<CanonicalSemanticOutput, CanonicalSemanticFailure> {
    // Rebinding the durable declaration records and rebuilding the query
    // dependency edges runs before whole-program analysis opens `sema`. It is
    // real per-declaration work, so it gets its own leaf instead of widening
    // the pipeline's unattributed residual (RUE-786).
    let declaration_reuse_span = info_span!("declaration_reuse").entered();
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
    let selected_definitions = prepared_definitions;
    reuse.declaration_indexes_built = declaration_index.build_invocations;
    let (projected, _) = crate::project_durable_declaration_semantics(
        merged,
        definitions,
        &shell_records,
        durable,
    )
    .map_err(|reason| {
        CanonicalSemanticFailure::declaration(
            crate::CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InternalError(format!(
                    "query declaration projection invariant failed: {reason:?}; definitions={:?}; durable={:?}; shells={:?}",
                    definitions
                        .definitions()
                        .iter()
                        .map(|record| record.stable_key())
                        .collect::<Vec<_>>(),
                    durable.iter().map(|record| &record.key).collect::<Vec<_>>(),
                    shell_records
                        .iter()
                        .map(|shell| &shell.identity)
                        .collect::<Vec<_>>(),
                )),
            )),
            declaration_stage_work(
                declaration_index,
                DeclarationBindingWork::default(),
                SemanticBindingManifestWork::default(),
                BodyOwnerTokenWork::default(),
                BodyAnalysisWork::default(),
                false,
                reuse,
            ),
        )
    })?;
    let projected_anonymous = crate::durable_semantics::project_durable_anonymous_nominals(
        merged,
        definitions,
        anonymous_nominals,
    )
    .map_err(|reason| {
        CanonicalSemanticFailure::declaration(
            crate::CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InternalError(format!(
                    "query anonymous nominal projection invariant failed: {reason:?}"
                )),
            )),
            CanonicalSemanticWork::default(),
        )
    })?;
    reuse.install_invocations = 1;
    let bound = shells
        .install_declaration_semantics_with_anonymous(&projected, &projected_anonymous)
        .map_err(|reason| {
            CanonicalSemanticFailure::declaration(
                crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(format!(
                        "query declaration installation invariant failed: {reason:?}"
                    )),
                )),
                declaration_stage_work(
                    declaration_index,
                    DeclarationBindingWork::default(),
                    SemanticBindingManifestWork::default(),
                    BodyOwnerTokenWork::default(),
                    BodyAnalysisWork::default(),
                    false,
                    reuse,
                ),
            )
        })?;
    let dependency_file = |key: &crate::StableDefinitionKey| {
        definitions
            .definition_by_key(key)
            .map(|record| record.declaration_span().file_id.index())
            .or_else(|| {
                merged
                    .ast()
                    .modules()
                    .iter()
                    .find(|module| module.module_id() == key.module())
                    .map(|module| module.file_id().index())
            })
    };
    let mut type_dependencies = Vec::new();
    let mut type_call_heads = Vec::new();
    let mut builtin_type_call_heads = Vec::new();
    let mut named_const_dependencies = Vec::new();
    for dependency in declaration_dependencies {
        let Some(source_file) = dependency_file(&dependency.source) else {
            return Err(CanonicalSemanticFailure::declaration(
                crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(format!(
                        "query dependency source is absent from the current definition epoch: {:?}",
                        dependency.source
                    )),
                )),
                CanonicalSemanticWork::default(),
            ));
        };
        let source_kind = match dependency.source.kind() {
            crate::StableDefinitionKind::Function => {
                rue_air::DeclarationTypeDependencySourceKind::Function
            }
            crate::StableDefinitionKind::Struct => {
                rue_air::DeclarationTypeDependencySourceKind::Struct
            }
            crate::StableDefinitionKind::Enum => rue_air::DeclarationTypeDependencySourceKind::Enum,
            crate::StableDefinitionKind::ValueConst
            | crate::StableDefinitionKind::ModuleBinding => {
                rue_air::DeclarationTypeDependencySourceKind::ValueConst
            }
            crate::StableDefinitionKind::Method => {
                rue_air::DeclarationTypeDependencySourceKind::Method
            }
            crate::StableDefinitionKind::AssociatedFunction => {
                rue_air::DeclarationTypeDependencySourceKind::AssociatedFunction
            }
            crate::StableDefinitionKind::Destructor => {
                rue_air::DeclarationTypeDependencySourceKind::Destructor
            }
        };
        let source_name = dependency.source.name().to_owned();
        let source_owner_name = dependency
            .source
            .owner()
            .map(|owner| owner.name().to_owned());
        match &dependency.target {
            crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                target,
            ) => {
                let Some(target_file) = dependency_file(target) else {
                    return Err(CanonicalSemanticFailure::declaration(
                        crate::CompileErrors::from(crate::CompileError::without_span(
                            rue_error::ErrorKind::InternalError(format!(
                                "query dependency target is absent from the current definition epoch: {target:?}"
                            )),
                        )),
                        CanonicalSemanticWork::default(),
                    ));
                };
                let target_kind = match target.kind() {
                    crate::StableDefinitionKind::Struct => {
                        rue_air::DeclarationTypeDependencyTargetKind::Struct
                    }
                    crate::StableDefinitionKind::Enum => {
                        rue_air::DeclarationTypeDependencyTargetKind::Enum
                    }
                    crate::StableDefinitionKind::ValueConst
                    | crate::StableDefinitionKind::ModuleBinding => {
                        rue_air::DeclarationTypeDependencyTargetKind::ValueConst
                    }
                    _ => continue,
                };
                type_dependencies.push(rue_air::DeclarationTypeDependencyEvent {
                    source_token: None,
                    source_file,
                    source_name,
                    source_owner_name,
                    source_kind,
                    dependency_kind: dependency.kind,
                    target_file,
                    target_name: target.name().to_owned(),
                    target_kind,
                });
            }
            crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::TypeCallHead(
                callable,
            ) => {
                let Some(callable_file) = dependency_file(callable) else {
                    return Err(CanonicalSemanticFailure::declaration(
                        crate::CompileErrors::from(crate::CompileError::without_span(
                            rue_error::ErrorKind::InternalError(format!(
                                "query type-call head is absent from the current definition epoch: {callable:?}"
                            )),
                        )),
                        CanonicalSemanticWork::default(),
                    ));
                };
                type_call_heads.push(rue_air::DeclarationTypeCallHeadDependencyEvent {
                    source_token: None,
                    source_file,
                    source_name,
                    source_owner_name,
                    source_kind,
                    dependency_kind: dependency.kind,
                    callable_file,
                    callable_name: callable.name().to_owned(),
                });
            }
            crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::BuiltinTypeCallHead(
                builtin,
            ) => builtin_type_call_heads.push(
                rue_air::DeclarationBuiltinTypeCallHeadDependencyEvent {
                    source_token: None,
                    source_file,
                    source_name,
                    source_owner_name,
                    source_kind,
                    dependency_kind: dependency.kind,
                    builtin: *builtin,
                },
            ),
            crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                target,
            ) => {
                let Some(target_file) = dependency_file(target) else {
                    return Err(CanonicalSemanticFailure::declaration(
                        crate::CompileErrors::from(crate::CompileError::without_span(
                            rue_error::ErrorKind::InternalError(format!(
                                "query named-value dependency target is absent from the current definition epoch: {target:?}"
                            )),
                        )),
                        CanonicalSemanticWork::default(),
                    ));
                };
                let target = match target.kind() {
                    crate::StableDefinitionKind::ValueConst => {
                        rue_air::NamedConstDependencyTargetEvent::ValueConst {
                            file: target_file,
                            name: target.name().to_owned(),
                        }
                    }
                    crate::StableDefinitionKind::ModuleBinding => {
                        rue_air::NamedConstDependencyTargetEvent::ModuleBinding {
                            file: target_file,
                            name: target.name().to_owned(),
                        }
                    }
                    crate::StableDefinitionKind::Function => {
                        rue_air::NamedConstDependencyTargetEvent::FreeFunction {
                            file: target_file,
                            name: target.name().to_owned(),
                        }
                    }
                    crate::StableDefinitionKind::Struct => {
                        rue_air::NamedConstDependencyTargetEvent::NamedType {
                            file: target_file,
                            name: target.name().to_owned(),
                            kind: rue_air::DeclarationTypeDependencyTargetKind::Struct,
                        }
                    }
                    crate::StableDefinitionKind::Enum => {
                        rue_air::NamedConstDependencyTargetEvent::NamedType {
                            file: target_file,
                            name: target.name().to_owned(),
                            kind: rue_air::DeclarationTypeDependencyTargetKind::Enum,
                        }
                    }
                    _ => continue,
                };
                named_const_dependencies.push(rue_air::NamedConstDependencyEvent {
                    source_file,
                    source_name,
                    target,
                });
            }
        }
    }
    let bound = bound.install_query_declaration_dependencies(
        &type_dependencies,
        &type_call_heads,
        &builtin_type_call_heads,
        &named_const_dependencies,
    );
    reuse.durable_records_reused = durable.len();
    reuse.ordinary_declaration_resolutions_skipped = 1;
    drop(declaration_reuse_span);
    let sema_span = info_span!("sema", phase = "semantic_analysis").entered();
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
        body_candidates,
        specialized_body_candidates,
        anonymous_body_candidates,
        durable_cfg_candidates,
        body_work,
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
    imports: &CanonicalImportGraph,
) -> MultiErrorResult<rue_air::Sema<'a>> {
    let _span = info_span!("rir_declaration_index", instruction_count = rir.rir().len()).entered();
    configure_canonical_sema(
        merged,
        rir,
        options.preview_features.clone(),
        options.target,
        imports,
    )
}

fn project_anonymous_nominal_key(
    key: &rue_air::AnonymousNominalKey<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >,
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
) -> Result<crate::AnonymousNominalKey, rue_air::SemanticStableResolutionFailure> {
    use rue_air::SemanticStableResolutionFailure as Failure;

    fn definition(
        token: rue_air::SemanticDefinitionToken,
        definitions: &BoundDefinitionSet,
    ) -> Result<crate::StableDefinitionKey, Failure> {
        definitions.key_for_semantic_token(token).cloned()
    }

    fn nominal_definition(
        token: rue_air::SemanticDefinitionToken,
        definitions: &BoundDefinitionSet,
    ) -> Result<crate::StableDefinitionKey, Failure> {
        let key = definition(token, definitions)?;
        if !matches!(
            key.kind(),
            crate::StableDefinitionKind::Struct | crate::StableDefinitionKind::Enum
        ) {
            return Err(Failure::WrongKind);
        }
        Ok(key)
    }

    fn function_definition(
        token: rue_air::SemanticDefinitionToken,
        definitions: &BoundDefinitionSet,
    ) -> Result<crate::StableDefinitionKey, Failure> {
        let key = definition(token, definitions)?;
        if !key.kind().owns_body() {
            return Err(Failure::WrongKind);
        }
        Ok(key)
    }

    fn arguments(
        value: &rue_air::CanonicalArguments<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
        merged: &CanonicalMergedProgram,
        definitions: &BoundDefinitionSet,
    ) -> Result<crate::CanonicalArguments, Failure> {
        Ok(crate::CanonicalArguments {
            types: value
                .types
                .iter()
                .map(|value| ty(value, merged, definitions))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
            values: value
                .values
                .iter()
                .map(|value| argument(value, merged, definitions))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        })
    }

    fn argument(
        value: &rue_air::CanonicalArgumentValue<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
        merged: &CanonicalMergedProgram,
        definitions: &BoundDefinitionSet,
    ) -> Result<crate::CanonicalArgumentValue, Failure> {
        use rue_air::CanonicalArgumentValue as V;
        Ok(match value {
            V::Integer(value) => crate::CanonicalArgumentValue::Integer(*value),
            V::Bool(value) => crate::CanonicalArgumentValue::Bool(*value),
            V::Type(value) => {
                crate::CanonicalArgumentValue::Type(Box::new(ty(value, merged, definitions)?))
            }
            V::Function(value) => crate::CanonicalArgumentValue::Function(Box::new(function(
                value,
                merged,
                definitions,
            )?)),
            V::Unit => crate::CanonicalArgumentValue::Unit,
            V::String(value) => crate::CanonicalArgumentValue::String(value.clone()),
        })
    }

    fn anonymous(
        value: &rue_air::AnonymousNominalKey<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
        merged: &CanonicalMergedProgram,
        definitions: &BoundDefinitionSet,
    ) -> Result<crate::AnonymousNominalKey, Failure> {
        Ok(crate::AnonymousNominalKey {
            kind: value.kind,
            producer: producer(&value.producer, merged, definitions)?,
            anchor: value.anchor.clone(),
            arguments: arguments(&value.arguments, merged, definitions)?,
        })
    }

    fn ty(
        value: &rue_air::TypeInstanceKey<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
        merged: &CanonicalMergedProgram,
        definitions: &BoundDefinitionSet,
    ) -> Result<crate::TypeInstanceKey, Failure> {
        use rue_air::{NominalInstanceKey as N, TypeInstanceKey as T};
        Ok(match value {
            T::I8 => crate::TypeInstanceKey::I8,
            T::I16 => crate::TypeInstanceKey::I16,
            T::I32 => crate::TypeInstanceKey::I32,
            T::I64 => crate::TypeInstanceKey::I64,
            T::U8 => crate::TypeInstanceKey::U8,
            T::U16 => crate::TypeInstanceKey::U16,
            T::U32 => crate::TypeInstanceKey::U32,
            T::U64 => crate::TypeInstanceKey::U64,
            T::Bool => crate::TypeInstanceKey::Bool,
            T::Unit => crate::TypeInstanceKey::Unit,
            T::Never => crate::TypeInstanceKey::Never,
            T::ComptimeType => crate::TypeInstanceKey::ComptimeType,
            T::BuiltinNominal { kind, name } => crate::TypeInstanceKey::BuiltinNominal {
                kind: *kind,
                name: name.clone(),
            },
            T::Nominal(N::Builtin { kind, name }) => {
                crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Builtin {
                    kind: *kind,
                    name: name.clone(),
                })
            }
            T::Nominal(N::Named(value)) => crate::TypeInstanceKey::Nominal(
                crate::NominalInstanceKey::Named(nominal_definition(*value, definitions)?),
            ),
            T::Nominal(N::Anonymous(value)) => crate::TypeInstanceKey::Nominal(
                crate::NominalInstanceKey::Anonymous(anonymous(value, merged, definitions)?),
            ),
            T::Array { element, len } => crate::TypeInstanceKey::Array {
                element: Box::new(ty(element, merged, definitions)?),
                len: *len,
            },
            T::Slice { element, name } => crate::TypeInstanceKey::Slice {
                element: Box::new(ty(element, merged, definitions)?),
                name: name.clone(),
            },
            T::PtrConst(value) => {
                crate::TypeInstanceKey::PtrConst(Box::new(ty(value, merged, definitions)?))
            }
            T::PtrMut(value) => {
                crate::TypeInstanceKey::PtrMut(Box::new(ty(value, merged, definitions)?))
            }
            T::Module(value) => crate::TypeInstanceKey::Module(
                definitions
                    .module_for_semantic_token(merged, *value)?
                    .clone(),
            ),
            T::GenericParameter(value) => crate::TypeInstanceKey::GenericParameter(*value),
        })
    }

    fn function(
        value: &rue_air::FunctionInstanceKey<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
        merged: &CanonicalMergedProgram,
        definitions: &BoundDefinitionSet,
    ) -> Result<crate::FunctionInstanceKey, Failure> {
        use rue_air::FunctionInstanceKey as F;
        Ok(match value {
            F::Definition(value) => {
                crate::FunctionInstanceKey::Definition(function_definition(*value, definitions)?)
            }
            F::Specialization {
                base,
                arguments: args,
            } => crate::FunctionInstanceKey::Specialization {
                base: Box::new(function(base, merged, definitions)?),
                arguments: arguments(args, merged, definitions)?,
            },
            F::AnonymousMember { owner, member } => crate::FunctionInstanceKey::AnonymousMember {
                owner: Box::new(ty(owner, merged, definitions)?),
                member: member.clone(),
            },
            F::DropGlue(value) => {
                crate::FunctionInstanceKey::DropGlue(Box::new(ty(value, merged, definitions)?))
            }
        })
    }

    fn producer(
        value: &rue_air::StableProducerId<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
        merged: &CanonicalMergedProgram,
        definitions: &BoundDefinitionSet,
    ) -> Result<crate::StableProducerId, Failure> {
        Ok(match value {
            rue_air::StableProducerId::Definition(value) => {
                crate::StableProducerId::Definition(definition(*value, definitions)?)
            }
            rue_air::StableProducerId::Function(value) => {
                crate::StableProducerId::Function(Box::new(function(value, merged, definitions)?))
            }
        })
    }

    anonymous(key, merged, definitions)
}

pub(crate) fn project_function_instance_key(
    key: &rue_air::FunctionInstanceKey<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >,
    merged: &CanonicalMergedProgram,
    definitions: &BoundDefinitionSet,
) -> Result<crate::FunctionInstanceKey, rue_air::SemanticStableResolutionFailure> {
    use rue_air::SemanticStableResolutionFailure as Failure;
    let projected = key.try_map_identities(
        &|token| definitions.key_for_semantic_token(*token).cloned(),
        &|token| {
            definitions
                .module_for_semantic_token(merged, *token)
                .cloned()
        },
    )?;
    fn validate(key: &crate::FunctionInstanceKey) -> Result<(), Failure> {
        match key {
            crate::FunctionInstanceKey::Definition(definition) => definition
                .kind()
                .owns_body()
                .then_some(())
                .ok_or(Failure::WrongKind),
            crate::FunctionInstanceKey::Specialization { base, .. } => validate(base),
            crate::FunctionInstanceKey::AnonymousMember { .. }
            | crate::FunctionInstanceKey::DropGlue(_) => Ok(()),
        }
    }
    validate(&projected)?;
    Ok(projected)
}

fn project_produced_anonymous_nominals(
    values: &[rue_air::SemanticProducedAnonymousNominal],
    resolver: &dyn BodyOutcomeResolver,
) -> Result<
    crate::body_query::BodyProducedAnonymousNominals,
    rue_air::SemanticStableResolutionFailure,
> {
    use crate::durable_semantics::{
        DurableAnonymousMethodSignature as Method, DurableAnonymousMethodType as MethodType,
        DurableAnonymousNominal as Nominal, DurableAnonymousNominalShape as Shape,
        DurableParameterMode as Mode,
    };

    let map_type = |ty: &rue_air::TypeInstanceKey<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >| {
        let ty = ty
            .try_map_identities(&|token| resolver.key_for_semantic_token(*token), &|token| {
                resolver.module_for_semantic_token(*token)
            })?;
        crate::revisioned_query_database::durable_type_from_instance_key(&ty)
            .ok_or(rue_air::SemanticStableResolutionFailure::WrongKind)
    };
    let map_value = |value: &rue_air::CanonicalArgumentValue<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >| {
        let value = value
            .try_map_identities(&|token| resolver.key_for_semantic_token(*token), &|token| {
                resolver.module_for_semantic_token(*token)
            })?;
        crate::revisioned_query_database::durable_value_from_argument(&value)
            .ok_or(rue_air::SemanticStableResolutionFailure::WrongKind)
    };
    let mode = |mode| match mode {
        rue_air::SemanticParameterMode::Value => Mode::Value,
        rue_air::SemanticParameterMode::Borrow => Mode::Borrow,
        rue_air::SemanticParameterMode::Inout => Mode::Inout,
    };

    values
        .iter()
        .map(|value| {
            let identity = value
                .identity
                .try_map_identities(&|token| resolver.key_for_semantic_token(*token), &|token| {
                    resolver.module_for_semantic_token(*token)
                })?;
            let method_type = |ty: &rue_air::SemanticProducedAnonymousMethodType| {
                Ok(match ty {
                    rue_air::SemanticProducedAnonymousMethodType::SelfType => MethodType::SelfType,
                    rue_air::SemanticProducedAnonymousMethodType::Concrete(ty) => {
                        MethodType::Concrete(map_type(ty)?)
                    }
                })
            };
            let shape = match &value.shape {
                rue_air::SemanticProducedAnonymousNominalShape::Struct { fields, methods } => {
                    Shape::Struct {
                        fields: fields
                            .iter()
                            .map(|(name, ty)| Ok((name.clone(), map_type(ty)?)))
                            .collect::<Result<Vec<_>, rue_air::SemanticStableResolutionFailure>>()?
                            .into(),
                        methods: methods
                            .iter()
                            .map(|method| {
                                Ok(Method {
                                    name: method.name.clone(),
                                    has_self: method.has_self,
                                    self_mode: mode(method.self_mode),
                                    parameters:
                                        method
                                            .parameters
                                            .iter()
                                            .map(|(ty, parameter_mode, is_comptime)| {
                                                Ok((
                                                    method_type(ty)?,
                                                    mode(*parameter_mode),
                                                    *is_comptime,
                                                ))
                                            })
                                            .collect::<Result<
                                                Vec<_>,
                                                rue_air::SemanticStableResolutionFailure,
                                            >>()?
                                            .into(),
                                    result: method_type(&method.result)?,
                                })
                            })
                            .collect::<Result<Vec<_>, rue_air::SemanticStableResolutionFailure>>()?
                            .into(),
                    }
                }
                rue_air::SemanticProducedAnonymousNominalShape::Enum { variants } => Shape::Enum {
                    variants: variants
                        .iter()
                        .map(|(name, payload)| {
                            Ok((
                                name.clone(),
                                payload
                                    .iter()
                                    .map(map_type)
                                    .collect::<Result<Vec<_>, _>>()?
                                    .into(),
                            ))
                        })
                        .collect::<Result<Vec<_>, rue_air::SemanticStableResolutionFailure>>()?
                        .into(),
                },
            };
            Ok(Nominal {
                identity,
                shape,
                type_captures: value
                    .type_captures
                    .iter()
                    .map(|(name, ty)| Ok((name.clone(), map_type(ty)?)))
                    .collect::<Result<Vec<_>, rue_air::SemanticStableResolutionFailure>>()?
                    .into(),
                value_captures: value
                    .value_captures
                    .iter()
                    .map(|(name, value)| Ok((name.clone(), map_value(value)?)))
                    .collect::<Result<Vec<_>, rue_air::SemanticStableResolutionFailure>>()?
                    .into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|values| crate::body_query::BodyProducedAnonymousNominals(values.into()))
}

/// One fully bound declaration epoch and the definition universe issued with it.
///
/// A request builds exactly one of these — the immutable declaration base
/// (RUE-1135) — and every reached body analyzes inside an epoch *derived* from
/// it through [`BoundBodyEpoch::derive_body_epoch`]. Both production and the
/// differential oracle consume epochs through [`analyze_body_in_epoch`].
pub(crate) struct BoundBodyEpoch<'a> {
    bound: rue_air::BoundSema<'a>,
    definitions: BoundDefinitionSet,
}

impl<'a> BoundBodyEpoch<'a> {
    /// Seal this epoch as a request's declaration base (RUE-1135), moving its
    /// type pool and parameter arena into shared immutable layers. Every later
    /// [`Self::derive_body_epoch`] then shares those two families by refcount
    /// instead of materializing a flat base per body.
    pub(crate) fn seal_as_declaration_base(&mut self) {
        self.bound.seal_as_declaration_base();
    }

    /// Derive one body-local epoch from this declaration base (RUE-1135),
    /// charging what the derivation shared and what it copied.
    ///
    /// The declaration namespace, the stable endpoint tables, the module
    /// registry, and the immutable prefixes of the type pool and parameter arena
    /// are shared with the base; only the small per-body owned state is copied.
    /// The issued definition universe is shared by reference: it is immutable
    /// durable data that body analysis only reads.
    pub(crate) fn derive_body_epoch(&self, units: &mut rue_air::EpochDerivationUnits) -> Self {
        Self {
            bound: self.bound.derive_body_epoch(units),
            definitions: self.definitions.clone(),
        }
    }

    /// Type-pool entries and parameters this epoch owns locally, excluding
    /// everything it reads from a shared base. Zero immediately after a
    /// derivation: the base's universe is read, not copied.
    pub(crate) fn local_universe_entries(&self) -> usize {
        self.bound.local_type_pool_entries() + self.bound.local_parameter_entries()
    }

    /// Type-pool entries and parameters this epoch reads from a shared base.
    #[cfg(test)]
    pub(crate) fn shared_universe_entries(&self) -> usize {
        self.bound.shared_type_pool_entries() + self.bound.shared_parameter_entries()
    }
}

/// The per-body epoch inputs a declaration base was built against.
///
/// A retained base can serve a body only when these compare equal: both are
/// installed *into* the epoch, so an epoch built against different ones is a
/// different epoch. On the measured corpora one base serves every body of a
/// request; a mismatch rebuilds rather than silently serving a stale base.
#[derive(Debug, PartialEq, Eq)]
struct DeclarationBaseInputs {
    revision: crate::SourceRevision,
    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    anonymous_nominals: Vec<crate::durable_semantics::DurableAnonymousNominal>,
    well_known: crate::body_query::WellKnownOptionResolution,
}

/// The request-scoped immutable declaration base (RUE-1135, RUE-1091 repair
/// option 2).
///
/// `analyze_body_query` used to rebuild the whole declaration epoch — prepare,
/// project, install, endpoints — for every reached body. Those stages take only
/// revision-scoped inputs and produce the same epoch every time; the only reason
/// they repeated is that the resulting epoch is mutated during body analysis.
/// This holds one built epoch for the length of one rooted attempt and hands
/// each body an epoch derived from it, so the mutation lands in a body-local
/// overlay instead of forcing a rebuild.
///
/// Three properties are load-bearing:
///
/// - **Private to the body bridge.** It is created inside one rooted attempt and
///   dropped with it. It is not a query authority, holds no lease, publishes no
///   artifact, and nothing outside this module can obtain the epoch it holds. A
///   failed or canceled request drops the base with the attempt, so a failure
///   cannot poison it.
/// - **It replaces per-body construction.** There is exactly one production
///   body-analysis path; `build_bound_body_epoch` now runs once per request
///   rather than once per body, and its per-stage counters record that.
/// - **Schedule-order independence.** Two bodies derived from one base share no
///   mutable state: the shared containers have no mutable path out of the base,
///   and each body's type-pool and parameter appends land above the base's
///   entries in its own overlay. One body's materializations therefore cannot be
///   observed by another regardless of the order they were scheduled in.
///
/// It pins the revision, configuration, and per-body epoch inputs it was filled
/// for. A request presenting different ones rebuilds the base rather than
/// serving an epoch that was never built for it.
pub(crate) struct SharedDeclarationBase<'a> {
    mode: SharedDeclarationBaseMode,
    meter: std::sync::Arc<crate::declaration_base_meter::DeclarationBaseMeter>,
    filled: std::sync::Mutex<Option<FilledDeclarationBase<'a>>>,
}

/// How a request treats its declaration base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedDeclarationBaseMode {
    /// Production: build the base once per request and derive every body from
    /// it.
    Shared,
    /// Validation: analyze every body twice — once inside a freshly built,
    /// independently derived epoch, once inside an epoch derived from the shared
    /// base — and compare the published transactions. The derived arm is the one
    /// that publishes, so a divergence is recorded rather than papered over.
    /// Deliberately useless for timing; enabled only by
    /// [`crate::unstable::enable_shared_declaration_base_differential`].
    Differential,
}

struct FilledDeclarationBase<'a> {
    inputs: DeclarationBaseInputs,
    epoch: BoundBodyEpoch<'a>,
}

impl<'a> SharedDeclarationBase<'a> {
    pub(crate) fn new(
        mode: SharedDeclarationBaseMode,
        meter: std::sync::Arc<crate::declaration_base_meter::DeclarationBaseMeter>,
    ) -> Self {
        Self {
            mode,
            meter,
            filled: std::sync::Mutex::new(None),
        }
    }

    /// A base private to one caller, with its own meter. Used by the harnesses
    /// that measure or compare a single body in isolation and must not inherit a
    /// base another body already paid for.
    #[cfg(test)]
    pub(crate) fn isolated() -> Self {
        Self::new(SharedDeclarationBaseMode::Shared, std::sync::Arc::default())
    }

    fn is_differential(&self) -> bool {
        self.mode == SharedDeclarationBaseMode::Differential
    }

    /// Record one differential comparison between an independently built epoch
    /// and the base-derived epoch that actually published.
    fn record_parity(
        &self,
        independent: &crate::body_query::BodyTransaction,
        derived: &crate::body_query::BodyTransaction,
    ) {
        self.meter
            .record_parity(crate::body_query::transaction_equal(independent, derived));
    }

    /// A body-local epoch for this request, building the base on first demand.
    ///
    /// The base build charges the ordinary prepare/project/install/endpoint
    /// counters at their own stage sources, exactly as a per-body build did —
    /// once per request instead of once per body. The derivation charges its own
    /// shared/copied unit counters, so the two are read together and the drop in
    /// the stage counters is accounted for rather than merely observed.
    #[allow(clippy::too_many_arguments)]
    fn get_or_derive(
        &self,
        merged: &CanonicalMergedProgram,
        rir: &'a CanonicalRirOutput,
        options: &CompileOptions,
        imports: &CanonicalImportGraph,
        query_shells: &[rue_air::SemanticDeclarationShell],
        query_declarations: &[DurableDeclarationSemantic],
        query_anonymous_nominals: &[crate::durable_semantics::DurableAnonymousNominal],
        well_known: &crate::body_query::WellKnownOptionResolution,
        key: &crate::body_query::BodyQueryKey,
        shared_projection: &SharedDeclarationProjection,
        work: &mut PerBodyDeclarationContextWork,
    ) -> Result<BoundBodyEpoch<'a>, crate::body_query::BodyTransaction> {
        let inputs = DeclarationBaseInputs {
            revision: rir.source_revision().clone(),
            configuration: key.configuration.clone(),
            anonymous_nominals: query_anonymous_nominals.to_vec(),
            well_known: well_known.clone(),
        };
        let mut filled = self
            .filled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if filled.as_ref().is_none_or(|held| held.inputs != inputs) {
            let epoch = build_bound_body_epoch(
                merged,
                rir,
                options,
                imports,
                query_shells,
                query_declarations,
                query_anonymous_nominals,
                well_known,
                key,
                shared_projection,
                work,
            )?;
            let mut epoch = epoch;
            // Seal once, here, so the type pool and parameter arena are shared
            // by refcount from the first derivation onwards. An unsealed base
            // has to materialize a flat immutable layer before it can be
            // shared, and paying that per body would put the copy the base
            // exists to remove straight back on the per-body path.
            epoch.seal_as_declaration_base();
            // Charged by the builder, at the point a base was really derived
            // from scratch: a request that reuses its base never reaches here.
            work.declaration_bases_built += 1;
            self.meter.record_base_built(filled.is_some());
            *filled = Some(FilledDeclarationBase { inputs, epoch });
        }
        let held = filled.as_ref().expect("a base was just retained");
        let mut units = rue_air::EpochDerivationUnits::default();
        let derived = held.epoch.derive_body_epoch(&mut units);
        // The layering invariant, checked where it is established: a freshly
        // derived epoch reads the base's whole type-pool and parameter universe
        // and owns none of it. A derivation that copied instead of layering
        // would show up here rather than only as a slower compile.
        assert_eq!(
            derived.local_universe_entries(),
            0,
            "a freshly derived epoch owns no type-pool or parameter entry of its own"
        );
        work.body_epochs_derived += 1;
        work.declaration_units_shared += units.total_units_shared();
        work.body_local_units_copied += units.body_local_entries_copied;
        self.meter.record_epoch_derived(&units);
        Ok(derived)
    }
}

/// Render a deterministic body-query stage failure for `instance`.
///
/// Preparation, projection, and installation failures are invariant violations,
/// not cancellations. Each is surfaced as a failed body transaction so the
/// coordinator renders a real compiler diagnostic instead of a disguised abort
/// that panics the uncanceled session.
pub(crate) fn body_stage_failure(
    instance: &crate::FunctionInstanceKey,
    stage: &str,
    detail: String,
) -> crate::body_query::BodyTransaction {
    use crate::body_query::{BodyReference, BodyReferences, BodyTransaction};

    BodyTransaction::DeterministicFailure {
        errors: crate::CompileErrors::from(crate::CompileError::without_span(
            rue_error::ErrorKind::InternalError(format!(
                "canonical body query stage `{stage}` failed for body instance {instance:?}: {detail}"
            )),
        )),
        references: BodyReferences(Arc::from(Vec::<BodyReference>::new())),
        lookup_observations: Arc::from([]),
    }
}

/// Materialize one fresh declaration epoch: prepare shells, project durable
/// declaration semantics, install them, and install the stable endpoints and the
/// per-body well-known `Option` registry.
///
/// This is the O(declarations) per-body prefix RUE-1091 exists to remove. It is
/// factored out of [`analyze_body_query`] so one request can build the base once
/// while the differential oracle can still construct an independent epoch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_bound_body_epoch<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    options: &CompileOptions,
    imports: &CanonicalImportGraph,
    query_shells: &[rue_air::SemanticDeclarationShell],
    query_declarations: &[DurableDeclarationSemantic],
    query_anonymous_nominals: &[crate::durable_semantics::DurableAnonymousNominal],
    well_known: &crate::body_query::WellKnownOptionResolution,
    key: &crate::body_query::BodyQueryKey,
    // The request-scoped memo for the body-invariant declaration projection
    // (RUE-1132). The first reached body of a request projects and publishes;
    // every later body reuses the shared exports and charges no projection work.
    shared_projection: &SharedDeclarationProjection,
    // Per-body declaration-context work is accrued here, at the stage sources,
    // as each stage actually performs it — never from the coordinator's input
    // slice lengths. The out-parameter carries partial work when a stage fails
    // (e.g. a prepare/project failure records the prepare shells but no install
    // or endpoint work), so the structural gate observes a shortcut added inside
    // any stage and never charges installation for a body that never installed.
    work: &mut PerBodyDeclarationContextWork,
) -> Result<BoundBodyEpoch<'a>, crate::body_query::BodyTransaction> {
    // Deterministic preparation, projection, and installation failures are
    // invariant violations, not cancellations. Each is surfaced as a failed body
    // transaction so the coordinator renders a real compiler diagnostic instead
    // of a disguised abort that panics the uncanceled session.
    let deterministic_failure =
        |stage: &str, detail: String| body_stage_failure(&key.instance, stage, detail);
    // Per-reached-body pipeline stages are timed as sibling leaves so
    // `--time-passes` can split the whole-program-rebuild cost RUE-1083 traced
    // to `analyze_body_query`. Each guard is dropped before the next stage so
    // the stages do not nest into one another.
    let prepare_span = info_span!("body_prepare_declarations").entered();
    // The prepare stage is now committed to real work: count this cold body and
    // (below) charge shells from the prepare stage's own output.
    work.cold_body_preparations += 1;
    let prepared =
        match prepare_query_declaration_shells(merged, rir, options, imports, query_shells) {
            Ok(prepared) => prepared,
            Err(failure) => {
                return Err(deterministic_failure(
                    "prepare_query_declaration_shells",
                    format!("{} ({:?})", failure.errors, failure.failure),
                ));
            }
        };
    let CanonicalPreparedDeclarations {
        shells,
        shell_records,
        definitions: provisional,
        declaration_index,
    } = prepared;
    // Prepare-stage source: the shells this stage actually materialized. A
    // shared-base shortcut that predeclares fewer shells drops this counter.
    work.shells_prepared += shell_records.len();
    work.declarations_inspected += shell_records.len();
    work.rir_indexes_constructed += declaration_index.build_invocations;
    work.rir_instructions_visited += declaration_index.rir_instructions_visited;
    drop(prepare_span);
    let project_span = info_span!("body_project_declarations").entered();
    // RUE-1132: the declaration projection is body-invariant, so it is computed
    // once per request and shared. The stage still runs in full for the first
    // reached body — including its failure classification — so a projection
    // failure is reported identically regardless of which body demanded it.
    let projection =
        shared_projection.get_or_project(rir.source_revision(), &key.configuration, || {
            crate::project_durable_declaration_semantics(
                merged,
                &provisional,
                &shell_records,
                query_declarations,
            )
        });
    let (projected, projection_work) = match projection {
        Ok(SharedProjectionOutcome::Computed(projected, work)) => (projected, Some(work)),
        Ok(SharedProjectionOutcome::Reused(projected)) => (projected, None),
        Err(SharedProjectionFailure::Projection(failure)) => {
            return Err(deterministic_failure(
                "project_durable_declaration_semantics",
                format!("{failure:?}"),
            ));
        }
        Err(SharedProjectionFailure::ForeignRequest) => {
            return Err(deterministic_failure(
                "shared_declaration_projection_revision",
                "the shared declaration projection belongs to a different revision or \
                 configuration than this body's request"
                    .into(),
            ));
        }
    };
    let projected_anonymous = match crate::durable_semantics::project_durable_anonymous_nominals(
        merged,
        &provisional,
        query_anonymous_nominals,
    ) {
        Ok(projected_anonymous) => projected_anonymous,
        Err(failure) => {
            return Err(deterministic_failure(
                "project_durable_anonymous_nominals",
                format!("{failure:?}"),
            ));
        }
    };
    // Project-stage source: the durable declaration records the projection join
    // actually traversed (`DurableSemanticProjectionWork::durable_records_visited`,
    // the value the projector returns and previously discarded), plus the
    // anonymous-nominal exports the anonymous projector produced (it exposes no
    // work struct, so its result cardinality is the only available measure).
    // Sourcing the declaration term from the projector's recorded work rather
    // than `projected.len()` means a projection shortcut that visits fewer
    // records — e.g. a shared base that skips already-projected declarations —
    // drops this counter even if the exported slice length is unchanged.
    //
    // RUE-1132 is exactly that shortcut: a body that reused the request's shared
    // projection visited and copied no durable record, so it charges only the
    // anonymous-nominal term it did project. The declaration term is charged
    // once per request, by whichever body computed it.
    let declaration_projection_work = projection_work.unwrap_or_default();
    work.projections_performed +=
        declaration_projection_work.durable_records_visited + projected_anonymous.len();
    work.durable_source_records_inspected += declaration_projection_work.durable_records_visited;
    work.durable_source_records_copied += declaration_projection_work.durable_records_copied;
    drop(project_span);
    let install_span = info_span!("body_install_declarations").entered();
    let bound = match shells
        .install_declaration_semantics_with_anonymous(&projected, &projected_anonymous)
    {
        Ok(bound) => bound,
        Err(failure) => {
            return Err(deterministic_failure(
                "install_declaration_semantics_with_anonymous",
                format!("{failure:?}"),
            ));
        }
    };
    // Install-stage source: the durable payloads the install stage recorded that
    // it installed (`DeclarationBindingWork::durable_payloads_installed`), read
    // back from the bound manifest's own work rather than the projected slice
    // length. Charged only after the install completes, never on a
    // prepare/project failure path, so the counter records performed
    // installation work only. An install shortcut that reuses a shared bound
    // base and installs fewer payloads therefore drops this counter.
    work.semantics_installed += bound.binding_work().durable_payloads_installed;
    work.modules_registered += bound.binding_work().modules_registered;
    let manifest = bound.binding_manifest();
    let definitions = match issue_bound_definitions(
        merged,
        rir.source_revision(),
        manifest.bindings(),
        manifest.work(),
    ) {
        Ok(definitions) => definitions,
        Err(failure) => {
            return Err(deterministic_failure(
                "issue_bound_definitions",
                format!("{failure:?}"),
            ));
        }
    };
    if provisional
        .definitions()
        .iter()
        .filter(|record| record.stable_key().kind().owns_body())
        .map(|record| record.stable_key())
        .ne(definitions
            .definitions()
            .iter()
            .filter(|record| record.stable_key().kind().owns_body())
            .map(|record| record.stable_key()))
    {
        return Err(deterministic_failure(
            "provisional_body_owner_key_equality",
            "provisional body-owner keys do not match the issued definition universe".into(),
        ));
    }
    let body_owner_endpoints = definitions.body_owner_endpoints();
    let body_owner_endpoints_installed = body_owner_endpoints.len();
    let bound = match bound.install_body_owner_tokens(&body_owner_endpoints) {
        Ok(bound) => bound,
        Err(failure) => {
            return Err(deterministic_failure(
                "install_body_owner_tokens",
                format!("{failure:?}"),
            ));
        }
    };
    let semantic_definition_endpoints = definitions.semantic_definition_endpoints();
    let semantic_module_endpoints = definitions.semantic_module_endpoints(merged);
    let semantic_module_endpoints_installed = semantic_module_endpoints.len();
    let bound = match bound.install_stable_identity_endpoints(
        &semantic_definition_endpoints,
        &semantic_module_endpoints,
    ) {
        Ok(bound) => bound,
        Err(failure) => {
            return Err(deterministic_failure(
                "install_stable_identity_endpoints",
                format!("{failure:?}"),
            ));
        }
    };
    // Endpoint-stage source: every endpoint this stage actually installed — the
    // body-owner endpoints, the stable definition endpoints, AND the module
    // endpoints passed to `install_stable_identity_endpoints` (previously
    // omitted from the counter even though the stage traverses and validates
    // them). Only reached after a successful install, isolating endpoint
    // traversal work from the projection/install terms.
    work.endpoints_installed += body_owner_endpoints_installed
        + semantic_definition_endpoints.len()
        + semantic_module_endpoints_installed;
    // Install the per-body well-known `Option(payload)` registry (RUE-1112)
    // narrowly, now that whole-program definition/module endpoints exist. The
    // resolved trusted-std `Option` enums are materialized into this epoch and
    // recorded so fallible-intrinsic resolution binds them under `?`; they are
    // never appended to composition, `BodyReferences`, or reachability, so the
    // fail-closed validation for ordinary nominals stays intact. A body that
    // demanded nothing installs nothing.
    let bound = if well_known.is_empty() {
        bound
    } else {
        let (well_known_nominals, well_known_option_by_payload) =
            match crate::durable_semantics::project_durable_option_registry(
                merged,
                &definitions,
                well_known,
            ) {
                Ok(projected) => projected,
                Err(failure) => {
                    return Err(deterministic_failure(
                        "project_durable_option_registry",
                        format!("{failure:?}"),
                    ));
                }
            };
        match bound
            .install_well_known_option_types(&well_known_nominals, &well_known_option_by_payload)
        {
            Ok(bound) => bound,
            Err(failure) => {
                return Err(deterministic_failure(
                    "install_well_known_option_types",
                    format!("{failure:?}"),
                ));
            }
        }
    };
    drop(install_span);
    Ok(BoundBodyEpoch { bound, definitions })
}

/// Analyze exactly one stable function instance inside an already bound epoch,
/// then publish its durable transaction. The epoch is consumed, so body
/// analysis owns all of its local mutable state.
pub(crate) fn analyze_body_in_epoch(
    epoch: BoundBodyEpoch<'_>,
    merged: &CanonicalMergedProgram,
    key: &crate::body_query::BodyQueryKey,
    cancellation: &rue_query::CancellationToken,
) -> Result<crate::body_query::BodyTransaction, rue_query::QueryAbort> {
    let BoundBodyEpoch { bound, definitions } = epoch;
    let deterministic_failure =
        |stage: &str, detail: String| body_stage_failure(&key.instance, stage, detail);
    let analyze_span = info_span!("body_analyze").entered();
    let lookup_collector = rue_air::BodyLookupCollector::new();
    let outcome = bound.analyze_one_body_instance_recording_lookups(
        &key.instance,
        |definition| definitions.semantic_token_for_key(definition),
        |module| definitions.module_token_for(merged, module),
        cancellation
            .is_canceled()
            .then_some(rue_air::OneBodyInterruption::Canceled),
        lookup_collector.clone(),
    );
    drop(analyze_span);
    // Keep cancellation responsive across the synchronous AIR callback. The
    // query runtime also checks after its evaluator returns, but crossing the
    // stable projection boundary after cancellation would do needless work.
    if cancellation.is_canceled() {
        return Err(rue_query::QueryAbort::Canceled);
    }
    let _export_span = info_span!("body_export").entered();
    // Publication converts the request-scoped overlay/epoch outcome back to the
    // durable body artifact through a resolver over the current issuer's stable
    // token space. The conversion is factored so the production `BoundDefinitionSet`
    // path and the body-local overlay path (RUE-1091 ADR-0066 §4) publish through
    // the identical code, guaranteeing byte-identical references, produced
    // nominals, and stable diagnostic ordering regardless of which token space
    // issued the compact IDs.
    let resolver = BoundOutcomeResolver {
        definitions: &definitions,
        merged,
    };
    let mut transaction = publish_one_body_outcome(outcome, &resolver, &deterministic_failure)?;
    let module_for_file = |file| {
        merged
            .ast()
            .modules()
            .iter()
            .find(|module| module.file_id().index() == file)
            .map(|module| module.module_id().clone())
    };
    let lookup_observations = lookup_collector
        .observations()
        .iter()
        .filter_map(|observation| {
            use crate::body_query::{BodyLookupKey as Key, BodyLookupNamespace as Namespace};
            Some(match observation {
                rue_air::BodyLookupObservation::ModuleItem { file, name } => Key::Name {
                    module: module_for_file(*file)?,
                    namespace: Namespace::ModuleItem,
                    name: name.clone(),
                },
                rue_air::BodyLookupObservation::Destructor { file, type_name } => Key::Name {
                    module: module_for_file(*file)?,
                    namespace: Namespace::Destructor,
                    name: type_name.clone(),
                },
                rue_air::BodyLookupObservation::Member {
                    owner_file,
                    owner_name,
                    member_name,
                } => Key::Member {
                    module: module_for_file(*owner_file)?,
                    owner_name: owner_name.clone(),
                    member_name: member_name.clone(),
                },
            })
        })
        .collect::<Vec<_>>()
        .into();
    transaction.install_lookup_observations(lookup_observations);
    Ok(transaction)
}

/// Derive one body-local epoch from the request's declaration base and analyze
/// exactly one stable function instance. The derived epoch is discarded after
/// its canonical projections cross the stable bridge; the base outlives it and
/// serves the request's remaining bodies (RUE-1135).
#[allow(clippy::too_many_arguments)]
pub(crate) fn analyze_body_query<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    options: &CompileOptions,
    imports: &CanonicalImportGraph,
    query_shells: &[rue_air::SemanticDeclarationShell],
    query_declarations: &[DurableDeclarationSemantic],
    query_anonymous_nominals: &[crate::durable_semantics::DurableAnonymousNominal],
    well_known: &crate::body_query::WellKnownOptionResolution,
    key: &crate::body_query::BodyQueryKey,
    cancellation: &rue_query::CancellationToken,
    shared_projection: &SharedDeclarationProjection,
    // The request-scoped immutable declaration base (RUE-1135). The first
    // reached body of a request builds it; every body — including that one —
    // analyzes inside an epoch derived from it.
    shared_base: &SharedDeclarationBase<'a>,
    work: &mut PerBodyDeclarationContextWork,
) -> Result<crate::body_query::BodyTransaction, rue_query::QueryAbort> {
    if cancellation.is_canceled() {
        return Err(rue_query::QueryAbort::Canceled);
    }
    #[cfg(test)]
    if INJECT_BODY_QUERY_STAGE_FAILURE.with(Cell::get) {
        return Ok(body_stage_failure(
            &key.instance,
            "injected_body_query_stage",
            "test body query stage failure injection".into(),
        ));
    }
    // Validation arm (RUE-1135): a genuinely independent derivation of the same
    // epoch, built and consumed exactly as the pre-base per-body path did. It
    // runs first so the comparison is against a fresh epoch rather than a second
    // read of the shared one, and its stage work is discarded — the production
    // arm below owns the accounting.
    let independent = if shared_base.is_differential() {
        let mut independent_work = PerBodyDeclarationContextWork::default();
        let independent_projection = SharedDeclarationProjection::new();
        let epoch = build_bound_body_epoch(
            merged,
            rir,
            options,
            imports,
            query_shells,
            query_declarations,
            query_anonymous_nominals,
            well_known,
            key,
            &independent_projection,
            &mut independent_work,
        );
        Some(match epoch {
            Ok(epoch) => analyze_body_in_epoch(epoch, merged, key, cancellation)?,
            Err(failure) => failure,
        })
    } else {
        None
    };
    // Deriving this body's epoch from the shared declaration base runs for
    // every reached body, unlike the one-shot prepare/project/install stages
    // above, so it is timed separately (RUE-786).
    let derive_span = info_span!("body_derive_epoch").entered();
    let derived = shared_base.get_or_derive(
        merged,
        rir,
        options,
        imports,
        query_shells,
        query_declarations,
        query_anonymous_nominals,
        well_known,
        key,
        shared_projection,
        work,
    );
    drop(derive_span);
    let epoch = match derived {
        Ok(epoch) => epoch,
        Err(failure) => return Ok(failure),
    };
    let transaction = analyze_body_in_epoch(epoch, merged, key, cancellation)?;
    if let Some(independent) = independent {
        shared_base.record_parity(&independent, &transaction);
    }
    Ok(transaction)
}
/// The stable-identity back-projection publication needs to convert one
/// `OneBodyTransactionOutcome` (carrying issuer-scoped compact tokens) into the
/// durable `BodyTransaction`. It abstracts over which token space issued the
/// tokens: the production path resolves against the whole-epoch
/// `BoundDefinitionSet`; the body-local overlay resolves against its own
/// task-owned minted token space. Compact overlay IDs never cross this boundary.
pub(crate) trait BodyOutcomeResolver {
    fn key_for_semantic_token(
        &self,
        token: rue_air::SemanticDefinitionToken,
    ) -> Result<crate::StableDefinitionKey, rue_air::SemanticStableResolutionFailure>;
    fn module_for_semantic_token(
        &self,
        token: rue_air::SemanticModuleToken,
    ) -> Result<crate::ModuleId, rue_air::SemanticStableResolutionFailure>;
    /// Resolve the ordinary body owner. The error carries the pre-formatted
    /// deterministic-failure detail so publication renders an identical
    /// diagnostic regardless of resolver.
    fn definition_key_for_body_token(
        &self,
        token: rue_air::BodyOwnerToken,
    ) -> Result<crate::StableDefinitionKey, String>;
}

/// Production resolver: the whole-epoch bound definition universe plus its
/// canonical module ordering.
struct BoundOutcomeResolver<'a> {
    definitions: &'a BoundDefinitionSet,
    merged: &'a CanonicalMergedProgram,
}

impl BodyOutcomeResolver for BoundOutcomeResolver<'_> {
    fn key_for_semantic_token(
        &self,
        token: rue_air::SemanticDefinitionToken,
    ) -> Result<crate::StableDefinitionKey, rue_air::SemanticStableResolutionFailure> {
        self.definitions.key_for_semantic_token(token).cloned()
    }

    fn module_for_semantic_token(
        &self,
        token: rue_air::SemanticModuleToken,
    ) -> Result<crate::ModuleId, rue_air::SemanticStableResolutionFailure> {
        self.definitions
            .module_for_semantic_token(self.merged, token)
            .cloned()
    }

    fn definition_key_for_body_token(
        &self,
        token: rue_air::BodyOwnerToken,
    ) -> Result<crate::StableDefinitionKey, String> {
        self.definitions
            .definition_for_body_token(token)
            .map(|record| record.stable_key().clone())
            .map_err(|failure| format!("{failure:?}"))
    }
}

/// Convert one analyzed body outcome to its durable `BodyTransaction`.
///
/// A `Success` maps every reference, body key, produced-anonymous identity, and
/// ordinary body owner back across the stable bridge, then folds field-place
/// nominal references into the reference set. A `DeterministicFailure` carries
/// its authoritative, already stably-ordered diagnostics through unchanged —
/// publication never permutes them, so task schedule, provider observation
/// order, compact-ID allocation order, cache hits, and warm reuse cannot change
/// observable diagnostic order. A `NonTerminal` outcome publishes nothing.
pub(crate) fn publish_one_body_outcome(
    outcome: rue_air::OneBodyTransactionOutcome,
    resolver: &dyn BodyOutcomeResolver,
    deterministic_failure: &dyn Fn(&str, String) -> crate::body_query::BodyTransaction,
) -> Result<crate::body_query::BodyTransaction, rue_query::QueryAbort> {
    use crate::body_query::{BodyReference, BodyReferences, BodyTransaction, CanonicalBody};
    use rue_air::{OneBodyCanonicalArtifact as Artifact, OneBodyDependency as Dependency};

    let map_references = |references: Arc<[Dependency]>| {
        references
            .iter()
            .map(|reference| {
                Ok(match reference {
                    Dependency::Callable(value) => {
                        BodyReference::Callable(value.try_map_identities(
                            &|token| resolver.key_for_semantic_token(*token),
                            &|token| resolver.module_for_semantic_token(*token),
                        )?)
                    }
                    Dependency::Definition(token) => {
                        BodyReference::Definition(resolver.key_for_semantic_token(*token)?)
                    }
                    Dependency::Type(value) => BodyReference::Type(value.try_map_identities(
                        &|token| resolver.key_for_semantic_token(*token),
                        &|token| resolver.module_for_semantic_token(*token),
                    )?),
                })
            })
            .collect::<Result<Vec<_>, rue_air::SemanticStableResolutionFailure>>()
            .map(|values| BodyReferences(values.into()))
    };
    match outcome {
        rue_air::OneBodyTransactionOutcome::Success {
            artifact,
            references,
            produced_anonymous_nominals,
        } => {
            let references = match map_references(references) {
                Ok(references) => references,
                Err(failure) => {
                    return Ok(deterministic_failure(
                        "map_body_references",
                        format!("{failure:?}"),
                    ));
                }
            };
            let produced_anonymous_nominals =
                match project_produced_anonymous_nominals(&produced_anonymous_nominals, resolver) {
                    Ok(produced_anonymous_nominals) => produced_anonymous_nominals,
                    Err(failure) => {
                        return Ok(deterministic_failure(
                            "project_produced_anonymous_nominals",
                            format!("{failure:?}"),
                        ));
                    }
                };
            let body =
                match artifact {
                    Artifact::Ordinary(export) => {
                        let owner = match resolver.definition_key_for_body_token(export.owner) {
                            Ok(owner) => owner,
                            Err(detail) => {
                                return Ok(deterministic_failure(
                                    "resolve_ordinary_body_owner",
                                    detail,
                                ));
                            }
                        };
                        let body = match export.body.try_map_keys(
                            &|token| resolver.key_for_semantic_token(*token),
                            &|token| resolver.module_for_semantic_token(*token),
                        ) {
                            Ok(body) => body,
                            Err(failure) => {
                                return Ok(deterministic_failure(
                                    "map_ordinary_body_keys",
                                    format!("{failure:?}"),
                                ));
                            }
                        };
                        CanonicalBody::Ordinary { owner, body }
                    }
                    Artifact::Anonymous(export) => {
                        let identity = match export.identity.try_map_identities(
                            &|token| resolver.key_for_semantic_token(*token),
                            &|token| resolver.module_for_semantic_token(*token),
                        ) {
                            Ok(identity) => identity,
                            Err(failure) => {
                                return Ok(deterministic_failure(
                                    "map_anonymous_body_identity",
                                    format!("{failure:?}"),
                                ));
                            }
                        };
                        let body = match export.body.try_map_keys(
                            &|token| resolver.key_for_semantic_token(*token),
                            &|token| resolver.module_for_semantic_token(*token),
                        ) {
                            Ok(body) => body,
                            Err(failure) => {
                                return Ok(deterministic_failure(
                                    "map_anonymous_body_keys",
                                    format!("{failure:?}"),
                                ));
                            }
                        };
                        CanonicalBody::Anonymous { identity, body }
                    }
                    Artifact::Specialization(export) => {
                        let identity = match export.identity.try_map_keys(
                            &|token| resolver.key_for_semantic_token(*token),
                            &|token| resolver.module_for_semantic_token(*token),
                        ) {
                            Ok(identity) => identity,
                            Err(failure) => {
                                return Ok(deterministic_failure(
                                    "map_specialization_body_identity",
                                    format!("{failure:?}"),
                                ));
                            }
                        };
                        let body = match export.body.try_map_keys(
                            &|token| resolver.key_for_semantic_token(*token),
                            &|token| resolver.module_for_semantic_token(*token),
                        ) {
                            Ok(body) => body,
                            Err(failure) => {
                                return Ok(deterministic_failure(
                                    "map_specialization_body_keys",
                                    format!("{failure:?}"),
                                ));
                            }
                        };
                        let dependencies = match export
                            .dependencies
                            .iter()
                            .map(|token| resolver.key_for_semantic_token(*token))
                            .collect::<Result<Vec<_>, _>>()
                        {
                            Ok(dependencies) => dependencies.into(),
                            Err(failure) => {
                                return Ok(deterministic_failure(
                                    "map_specialization_dependencies",
                                    format!("{failure:?}"),
                                ));
                            }
                        };
                        CanonicalBody::Specialization {
                            identity,
                            body,
                            dependencies,
                            dependency_boundary_complete: export.dependency_boundary_complete,
                        }
                    }
                };
            let semantic_body = match &body {
                CanonicalBody::Ordinary { body, .. }
                | CanonicalBody::Anonymous { body, .. }
                | CanonicalBody::Specialization { body, .. } => body,
            };
            let mut references = references
                .0
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            for place in semantic_body.places.iter() {
                for projection in place.projections.iter() {
                    if let rue_air::SemanticBodyProjection::Field { struct_key, .. } = projection {
                        references.insert(BodyReference::Type(crate::TypeInstanceKey::Nominal(
                            struct_key.clone(),
                        )));
                    }
                }
            }
            let references = BodyReferences(references.into_iter().collect::<Vec<_>>().into());
            Ok(BodyTransaction::Success {
                body: Box::new(body),
                references,
                produced_anonymous_nominals,
                lookup_observations: Arc::from([]),
            })
        }
        rue_air::OneBodyTransactionOutcome::DeterministicFailure { errors, references } => {
            // The failing body already carries authoritative diagnostics. If its
            // reference set cannot cross the stable bridge, keep those diagnostics
            // and drop the unresolved references rather than disguising the
            // invariant violation as a cancellation.
            let references = map_references(references)
                .unwrap_or_else(|_| BodyReferences(Arc::from(Vec::<BodyReference>::new())));
            Ok(BodyTransaction::DeterministicFailure {
                errors,
                references,
                lookup_observations: Arc::from([]),
            })
        }
        // A non-terminal outcome is a request to observe a deferred producer, not
        // a deterministic failure; the coordinator reschedules it.
        rue_air::OneBodyTransactionOutcome::NonTerminal { .. } => {
            Err(rue_query::QueryAbort::Canceled)
        }
    }
}

/// Test-only overlay driver (RUE-1091 slice 2b). It analyzes exactly one body
/// through a task-owned `BodySemanticOverlay` instead of the whole-epoch
/// `BoundDefinitionSet` token space, then publishes through the shared
/// `publish_one_body_outcome`. The declaration prepare/project/install prefix is
/// unchanged; only the stable-identity endpoints are re-issued under the
/// overlay's own issuer, so the analyzed body mints and resolves overlay-local
/// compact IDs end to end. This is not a production path — `analyze_body_query`
/// is the one production path and remains inert to overlays.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn analyze_body_via_overlay(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    options: &CompileOptions,
    imports: &CanonicalImportGraph,
    query_shells: &[rue_air::SemanticDeclarationShell],
    query_declarations: &[DurableDeclarationSemantic],
    query_anonymous_nominals: &[crate::durable_semantics::DurableAnonymousNominal],
    well_known: &crate::body_query::WellKnownOptionResolution,
    key: &crate::body_query::BodyQueryKey,
    cancellation: &rue_query::CancellationToken,
    overlay: &mut crate::body_overlay::BodySemanticOverlay,
) -> Result<crate::body_query::BodyTransaction, rue_query::QueryAbort> {
    use crate::body_query::{BodyReference, BodyReferences, BodyTransaction};
    use rue_air::{BodyOwnerToken, SemanticDefinitionToken, SemanticModuleToken};

    if cancellation.is_canceled() {
        return Err(rue_query::QueryAbort::Canceled);
    }
    let deterministic_failure = |stage: &str, detail: String| -> BodyTransaction {
        BodyTransaction::DeterministicFailure {
            errors: crate::CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InternalError(format!(
                    "overlay body query stage `{stage}` failed for body instance {:?}: {detail}",
                    key.instance,
                )),
            )),
            references: BodyReferences(Arc::from(Vec::<BodyReference>::new())),
            lookup_observations: Arc::from([]),
        }
    };

    let prepared =
        match prepare_query_declaration_shells(merged, rir, options, imports, query_shells) {
            Ok(prepared) => prepared,
            Err(failure) => {
                return Ok(deterministic_failure(
                    "prepare_query_declaration_shells",
                    format!("{} ({:?})", failure.errors, failure.failure),
                ));
            }
        };
    let CanonicalPreparedDeclarations {
        shells,
        definitions: provisional,
        ..
    } = prepared;
    let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
    let (projected, _) = match crate::project_durable_declaration_semantics(
        merged,
        &provisional,
        &shell_records,
        query_declarations,
    ) {
        Ok(projected) => projected,
        Err(failure) => {
            return Ok(deterministic_failure(
                "project_durable_declaration_semantics",
                format!("{failure:?}"),
            ));
        }
    };
    let projected_anonymous = match crate::durable_semantics::project_durable_anonymous_nominals(
        merged,
        &provisional,
        query_anonymous_nominals,
    ) {
        Ok(projected_anonymous) => projected_anonymous,
        Err(failure) => {
            return Ok(deterministic_failure(
                "project_durable_anonymous_nominals",
                format!("{failure:?}"),
            ));
        }
    };
    let bound = match shells
        .install_declaration_semantics_with_anonymous(&projected, &projected_anonymous)
    {
        Ok(bound) => bound,
        Err(failure) => {
            return Ok(deterministic_failure(
                "install_declaration_semantics_with_anonymous",
                format!("{failure:?}"),
            ));
        }
    };
    let manifest = bound.binding_manifest();
    let definitions = match issue_bound_definitions(
        merged,
        rir.source_revision(),
        manifest.bindings(),
        manifest.work(),
    ) {
        Ok(definitions) => definitions,
        Err(failure) => {
            return Ok(deterministic_failure(
                "issue_bound_definitions",
                format!("{failure:?}"),
            ));
        }
    };

    // Seed the overlay's local token space in the bound universe's sorted order
    // so an overlay definition slot mirrors its whole-epoch counterpart. This is
    // the recipe-materialization mint: every definition/module fact the body can
    // reach gets a fresh overlay-local token recorded against its stable identity.
    for record in definitions.definitions() {
        overlay.intern_definition(record.stable_key());
    }
    for module in merged.ast().modules() {
        overlay.intern_module(module.module_id());
    }

    // Re-issue the endpoints under the overlay's own issuer. Text, file, kind,
    // and owner provenance are unchanged (installation still validates them);
    // only the issuer-scoped token differs, so the analyzed body observes and
    // publishes overlay-local compact IDs rather than the whole-epoch ones.
    //
    // Adopting the epoch endpoints' slot indices and re-minting only the issuer
    // is a test-driver convenience: it couples the overlay's token space to the
    // bound universe's seed order, which the overlay-equals-production equality
    // test guards. Slice 3 replaces this adoption with provider-supplied name,
    // import, and declaration facts consulted on demand.
    let issuer = overlay.issuer();
    let body_owner_endpoints = definitions
        .body_owner_endpoints()
        .into_iter()
        .map(|mut endpoint| {
            endpoint.token = BodyOwnerToken::new(issuer, endpoint.token.slot());
            endpoint
        })
        .collect::<Vec<_>>();
    let bound = match bound.install_body_owner_tokens(&body_owner_endpoints) {
        Ok(bound) => bound,
        Err(failure) => {
            return Ok(deterministic_failure(
                "install_body_owner_tokens",
                format!("{failure:?}"),
            ));
        }
    };
    let semantic_definition_endpoints = definitions
        .semantic_definition_endpoints()
        .into_iter()
        .map(|mut endpoint| {
            endpoint.token = SemanticDefinitionToken::new(issuer, endpoint.token.slot());
            endpoint
        })
        .collect::<Vec<_>>();
    let semantic_module_endpoints = definitions
        .semantic_module_endpoints(merged)
        .into_iter()
        .map(|mut endpoint| {
            endpoint.token = SemanticModuleToken::new(issuer, endpoint.token.slot());
            endpoint
        })
        .collect::<Vec<_>>();
    let bound = match bound.install_stable_identity_endpoints(
        &semantic_definition_endpoints,
        &semantic_module_endpoints,
    ) {
        Ok(bound) => bound,
        Err(failure) => {
            return Ok(deterministic_failure(
                "install_stable_identity_endpoints",
                format!("{failure:?}"),
            ));
        }
    };
    // Well-known `Option(payload)` materialization is internal to the epoch and
    // independent of the endpoint issuer, so its projection reuses the definition
    // universe. A body that demands nothing installs nothing.
    let bound = if well_known.is_empty() {
        bound
    } else {
        let (well_known_nominals, well_known_option_by_payload) =
            match crate::durable_semantics::project_durable_option_registry(
                merged,
                &definitions,
                well_known,
            ) {
                Ok(projected) => projected,
                Err(failure) => {
                    return Ok(deterministic_failure(
                        "project_durable_option_registry",
                        format!("{failure:?}"),
                    ));
                }
            };
        match bound
            .install_well_known_option_types(&well_known_nominals, &well_known_option_by_payload)
        {
            Ok(bound) => bound,
            Err(failure) => {
                return Ok(deterministic_failure(
                    "install_well_known_option_types",
                    format!("{failure:?}"),
                ));
            }
        }
    };

    // A cancellation observed after the overlay has already minted its local
    // token space must still publish nothing: the request control state
    // dominates, and the seeded overlay is abandoned whole. This test-only
    // injection flips cancellation exactly here — between seeding/install and
    // analysis — so the mid-analysis cancellation path is exercised with a
    // populated overlay (ADR-0066 §4 "Cancellation publishes no partial overlay
    // or dependency set").
    #[cfg(test)]
    if INJECT_OVERLAY_CANCEL_AFTER_SEED.with(Cell::get) {
        return Err(rue_query::QueryAbort::Canceled);
    }

    let outcome = bound.analyze_one_body_instance(
        &key.instance,
        |definition| overlay.semantic_token_for_key(definition),
        |module| overlay.module_token_for(module),
        cancellation
            .is_canceled()
            .then_some(rue_air::OneBodyInterruption::Canceled),
    );
    if cancellation.is_canceled() {
        return Err(rue_query::QueryAbort::Canceled);
    }
    publish_one_body_outcome(outcome, &*overlay, &deterministic_failure)
}

struct CanonicalBodyCompositionFailure {
    errors: crate::CompileErrors,
    work: BodyAnalysisWork,
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
    durable_body_candidates: Vec<PreparedDurableBodyCandidate>,
    durable_specialized_body_candidates: Vec<PreparedDurableSpecializedBodyCandidate>,
    durable_anonymous_body_candidates: Vec<PreparedDurableAnonymousBodyCandidate>,
    durable_cfg_candidates: Arc<[crate::queries::DurableCfgArtifact]>,
    durable_body_reuse_work: crate::DurableBodyWork,
    sema_span: tracing::span::EnteredSpan,
) -> Result<CanonicalSemanticOutput, CanonicalSemanticFailure> {
    finish_canonical_analysis_with(
        input,
        merged,
        rir,
        options,
        request_stable_ids,
        declaration_index,
        bound,
        provisional_definitions,
        declaration_reuse,
        durable_body_candidates,
        durable_specialized_body_candidates,
        durable_anonymous_body_candidates,
        durable_cfg_candidates,
        durable_body_reuse_work,
        sema_span,
        |bound, candidates, definitions, merged| {
            bound
                .compose_queried_bodies(
                    candidates,
                    |key: &crate::StableDefinitionKey| definitions.semantic_token_for_key(key),
                    |module: &crate::ModuleId| definitions.module_token_for(merged, module),
                )
                .map_err(|failure| CanonicalBodyCompositionFailure {
                    work: failure.work(),
                    errors: failure.into_errors(),
                })
        },
    )
}

fn finish_canonical_analysis_with(
    input: CodegenInputDescriptor,
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    options: &CompileOptions,
    request_stable_ids: bool,
    declaration_index: RirDeclarationIndexWork,
    bound: rue_air::BoundSema<'_>,
    provisional_definitions: BoundDefinitionSet,
    declaration_reuse: CanonicalDeclarationReuseWork,
    durable_body_candidates: Vec<PreparedDurableBodyCandidate>,
    durable_specialized_body_candidates: Vec<PreparedDurableSpecializedBodyCandidate>,
    durable_anonymous_body_candidates: Vec<PreparedDurableAnonymousBodyCandidate>,
    durable_cfg_candidates: Arc<[crate::queries::DurableCfgArtifact]>,
    mut durable_body_reuse_work: crate::DurableBodyWork,
    sema_span: tracing::span::EnteredSpan,
    analyze_bodies: impl FnOnce(
        rue_air::BoundSema<'_>,
        Vec<rue_air::SemanticQueriedBodyCandidate<crate::StableDefinitionKey, crate::ModuleId>>,
        &BoundDefinitionSet,
        &CanonicalMergedProgram,
    ) -> Result<rue_air::SemaOutput, CanonicalBodyCompositionFailure>,
) -> Result<CanonicalSemanticOutput, CanonicalSemanticFailure> {
    let binding = bound.binding_work();
    #[cfg(test)]
    if INJECT_DECLARATION_FAILURE.with(Cell::get) {
        return Err(recover_declaration_failure(
            bound,
            crate::CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InternalError("test declaration failure injection".into()),
            )),
            declaration_index,
            request_stable_ids,
            declaration_reuse,
            BodyOwnerTokenWork::default(),
        ));
    }
    let manifest = bound.binding_manifest();
    let authoritative_definitions = match issue_bound_definitions(
        merged,
        rir.source_revision(),
        manifest.bindings(),
        manifest.work(),
    ) {
        Ok(definitions) => definitions,
        Err(preparation_error) => {
            return Err(recover_declaration_failure(
                bound,
                crate::CompileErrors::from(preparation_error),
                declaration_index,
                request_stable_ids,
                declaration_reuse,
                BodyOwnerTokenWork::default(),
            ));
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
    #[cfg(test)]
    let mut authoritative_keys = authoritative_keys;
    #[cfg(test)]
    if INJECT_AUTHORITATIVE_KEY_MISMATCH.with(Cell::get) {
        authoritative_keys.pop();
    }
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
        return Err(recover_declaration_failure(
            bound,
            preparation_error,
            declaration_index,
            request_stable_ids,
            declaration_reuse,
            BodyOwnerTokenWork {
                provisional_slots: provisional_keys.len(),
                authoritative_slots: authoritative_keys.len(),
                slots_validated: first_difference
                    .unwrap_or_else(|| provisional_keys.len().min(authoritative_keys.len())),
                tokens_installed: 0,
                validation_failures: 1,
            },
        ));
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
        let mut failed_tokens = body_owner_tokens;
        failed_tokens.tokens_installed = 0;
        failed_tokens.validation_failures = 1;
        CanonicalSemanticFailure::declaration(
            crate::CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InternalError(
                    "failed to install authoritative body-owner tokens".into(),
                ),
            )),
            declaration_stage_work(
                declaration_index,
                binding,
                manifest_work,
                failed_tokens,
                BodyAnalysisWork::default(),
                request_stable_ids,
                declaration_reuse,
            ),
        )
    })?;
    let bound = bound
        .install_stable_identity_endpoints(
            &authoritative_definitions.semantic_definition_endpoints(),
            &authoritative_definitions.semantic_module_endpoints(merged),
        )
        .map_err(|failure| {
            CanonicalSemanticFailure::declaration(
                crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(format!(
                        "failed to install authoritative stable identity endpoints: {failure:?}"
                    )),
                )),
                declaration_stage_work(
                    declaration_index,
                    binding,
                    manifest_work,
                    body_owner_tokens,
                    BodyAnalysisWork::default(),
                    request_stable_ids,
                    declaration_reuse,
                ),
            )
        })?;
    let token_by_key = authoritative_definitions
        .body_owner_endpoints()
        .into_iter()
        .zip(authoritative_keys.iter())
        .map(|(endpoint, key)| ((*key).clone(), endpoint.token))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut queried_candidates = durable_body_candidates
        .into_iter()
        .filter_map(|candidate| {
            let owner = token_by_key.get(&candidate.owner).copied()?;
            Some(rue_air::SemanticQueriedBodyCandidate {
                identity: crate::FunctionInstanceKey::Definition(candidate.owner),
                ordinary_owner: Some(owner),
                specialization_identity: None,
                body_span: candidate.body_span,
                body: candidate.body,
            })
        })
        .collect::<Vec<_>>();
    queried_candidates.extend(
        durable_specialized_body_candidates
            .into_iter()
            .map(|candidate| rue_air::SemanticQueriedBodyCandidate {
                identity: candidate.instance,
                ordinary_owner: None,
                specialization_identity: Some(candidate.identity),
                body_span: candidate.body_span,
                body: candidate.body,
            }),
    );
    queried_candidates.extend(
        durable_anonymous_body_candidates
            .into_iter()
            .map(|candidate| rue_air::SemanticQueriedBodyCandidate {
                identity: candidate.identity,
                ordinary_owner: None,
                specialization_identity: None,
                body_span: candidate.body_span,
                body: candidate.body,
            }),
    );
    let queried_cfg_bodies = queried_candidates
        .iter()
        .filter_map(|candidate| {
            let owner =
                crate::revisioned_query_database::function_definition_key(&candidate.identity)?
                    .clone();
            let record = authoritative_definitions.definition_by_key(&owner)?;
            let expected_inputs = crate::session::stable_definition_input_fingerprint(
                merged.definitions().source_snapshot(),
                record,
            )
            .ok()?;
            Some((
                candidate.identity.clone(),
                (
                    candidate.body_span,
                    crate::DurableOrdinaryBodyPayload {
                        schema_version: crate::durable_body::DURABLE_ORDINARY_BODY_SCHEMA_VERSION,
                        semantic_schema_version: crate::DURABLE_SEMANTIC_SCHEMA_VERSION,
                        owner,
                        expected_inputs,
                        body: candidate.body.clone(),
                    },
                ),
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let composition = analyze_bodies(
        bound,
        queried_candidates,
        &authoritative_definitions,
        merged,
    );
    let sema_output = match composition {
        Ok(output) => output,
        Err(failure) => {
            let mut failed_durable_body_work = durable_body_reuse_work;
            failed_durable_body_work.reused_bodies += failure.work.ordinary_bodies_reused;
            failed_durable_body_work.skipped_body_analyses +=
                failure.work.ordinary_body_analyses_skipped;
            fold_body_import_work(&mut failed_durable_body_work, failure.work);
            let work = CanonicalSemanticWork {
                declaration_index,
                binding,
                manifest: manifest_work,
                bound_definitions: bound_definitions.as_ref().map(BoundDefinitionSet::work),
                body_owner_tokens,
                body_analysis: failure.work,
                durable_bodies: failed_durable_body_work,
                cfg: CfgConstructionWork::default(),
                stable_ids_requested: request_stable_ids,
                declaration_reuse,
            };
            return Err(CanonicalSemanticFailure::new(
                CanonicalSemanticFailurePhase::BodyAnalysis,
                failure.errors,
                work,
            ));
        }
    };
    #[cfg(test)]
    let mut sema_output = sema_output;
    #[cfg(test)]
    inject_cfg_failure(&mut sema_output, rir.semantic_symbols().interner());
    let body_analysis = sema_output.body_analysis_work;
    durable_body_reuse_work.reused_bodies += body_analysis.ordinary_bodies_reused;
    durable_body_reuse_work.skipped_body_analyses += body_analysis.ordinary_body_analyses_skipped;
    fold_body_import_work(&mut durable_body_reuse_work, body_analysis);
    let mut durable_body_work = crate::DurableBodyWork {
        export_attempts: body_analysis.ordinary_body_exports_attempted,
        export_successes: body_analysis.ordinary_body_exports_succeeded,
        export_rejections: body_analysis.ordinary_body_exports_rejected,
        last_export_failure: body_analysis
            .last_ordinary_body_export_failure
            .or(body_analysis.last_specialized_body_export_failure),
        instructions_exported: body_analysis.ordinary_body_export_instructions_emitted,
        places_exported: body_analysis.ordinary_body_export_places_emitted,
        strings_exported: body_analysis.ordinary_body_export_strings_emitted,
        ..durable_body_reuse_work
    };
    let durable_ordinary_body_payloads = crate::convert_semantic_body_exports(
        &sema_output.ordinary_body_exports,
        merged,
        &authoritative_definitions,
        &mut durable_body_work,
    )
    .unwrap_or_else(|_| Arc::from([]));
    let durable_specialized_body_payloads = crate::convert_semantic_specialized_body_exports(
        &sema_output.specialized_body_exports,
        merged,
        &authoritative_definitions,
        &mut durable_body_work,
    )
    .unwrap_or_else(|_| Arc::from([]));
    let anonymous_nominal_associations = sema_output
        .anonymous_nominal_identities_by_type
        .values()
        .map(|identity| project_anonymous_nominal_key(identity, merged, &authoritative_definitions))
        .collect::<Result<Vec<_>, rue_air::SemanticStableResolutionFailure>>();
    let mut anonymous_nominal_associations = match anonymous_nominal_associations {
        Ok(associations) => associations,
        Err(failure) => {
            let work = CanonicalSemanticWork {
                declaration_index,
                binding,
                manifest: manifest_work,
                bound_definitions: bound_definitions.as_ref().map(BoundDefinitionSet::work),
                body_owner_tokens,
                body_analysis,
                durable_bodies: durable_body_work,
                cfg: CfgConstructionWork::default(),
                stable_ids_requested: request_stable_ids,
                declaration_reuse,
            };
            return Err(CanonicalSemanticFailure::new(
                CanonicalSemanticFailurePhase::BodyAnalysis,
                crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(format!(
                        "failed to project an anonymous nominal identity through the authoritative definition boundary: {failure:?}"
                    )),
                )),
                work,
            ));
        }
    };
    // Deterministic order over the direct producer keys so the retained
    // artifact never depends on `HashMap` iteration order (ADR-0066).
    anonymous_nominal_associations.sort();
    let anonymous_nominal_associations: Arc<[crate::AnonymousNominalKey]> =
        anonymous_nominal_associations.into();
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
    let issued_callable_identities = sema_output
        .functions
        .iter()
        .map(|function| function.identity.clone())
        .chain(
            sema_output
                .aggregate_type_identities_by_type
                .values()
                .cloned()
                .map(|ty| rue_air::FunctionInstanceKey::DropGlue(Box::new(ty))),
        )
        .collect::<std::collections::BTreeSet<_>>();
    let projected_callable_identities = issued_callable_identities
        .into_iter()
        .map(|issued| {
            project_function_instance_key(&issued, merged, &authoritative_definitions)
                .map(|stable| (issued, stable))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
        .map_err(|failure| {
            CanonicalSemanticFailure::new(
                CanonicalSemanticFailurePhase::BodyAnalysis,
                crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(format!(
                        "failed to project callable identity through the authoritative definition boundary: {failure:?}"
                    )),
                )),
                CanonicalSemanticWork {
                    declaration_index,
                    binding,
                    manifest: manifest_work,
                    bound_definitions: bound_definitions.as_ref().map(BoundDefinitionSet::work),
                    body_owner_tokens,
                    body_analysis,
                    durable_bodies: durable_body_work,
                    cfg: CfgConstructionWork::default(),
                    stable_ids_requested: request_stable_ids,
                    declaration_reuse,
                },
            )
        })?;
    let mut stable_cfg_inputs = Vec::new();
    for function in &sema_output.functions {
        let selected = projected_callable_identities
            .get(&function.identity)
            .and_then(|function_key| {
                queried_cfg_bodies
                    .get(function_key)
                    .map(|(body_span, body)| (*body_span, body.clone(), function_key.clone()))
            });
        if let Some((body_span, body, function_key)) = selected {
            let Ok(type_dependencies) = crate::durable_cfg::transitive_body_type_dependencies(
                &body,
                &sema_output.type_pool,
                &authoritative_definitions,
            ) else {
                continue;
            };
            let type_inputs = type_dependencies
                .into_iter()
                .map(|key| {
                    authoritative_definitions
                        .definition_by_key(&key)
                        .ok_or(())
                        .and_then(|record| {
                            crate::session::stable_definition_input_fingerprint(
                                merged.definitions().source_snapshot(),
                                record,
                            )
                            .map_err(|_| ())
                        })
                })
                .collect::<Result<Vec<_>, _>>()
                .ok();
            let Some(type_inputs) = type_inputs else {
                continue;
            };
            stable_cfg_inputs.push(crate::durable_cfg::CurrentCfgInput {
                stable: crate::durable_cfg::StableCfgInput {
                    function: function_key,
                    body,
                    type_inputs: type_inputs.into(),
                },
                body_span,
            });
        }
    }
    stable_cfg_inputs.sort_by(|left, right| left.stable.function.cmp(&right.stable.function));
    drop(sema_span);
    // CFG construction is a sibling of semantic-proper, not nested inside it:
    // `sema_span` closes above before this begins. Published phases must not
    // overlap, so this boundary is load-bearing rather than stylistic. The
    // `cfg_and_optimization` phase marker lives on `cfg_construction` inside
    // `build_functions_and_cfgs`.
    let cfg = build_functions_and_cfgs(
        sema_output,
        options.opt_level,
        options.target,
        rir.semantic_symbols().interner(),
        &durable_cfg_candidates,
        &stable_cfg_inputs,
        &projected_callable_identities,
    )
    .map_err(|failure| {
        let work = CanonicalSemanticWork {
            declaration_index,
            binding,
            manifest: manifest_work,
            bound_definitions: bound_definitions.as_ref().map(BoundDefinitionSet::work),
            body_owner_tokens,
            body_analysis,
            durable_bodies: durable_body_work,
            cfg: failure.work,
            stable_ids_requested: request_stable_ids,
            declaration_reuse,
        };
        CanonicalSemanticFailure::new(
            CanonicalSemanticFailurePhase::CfgConstruction,
            failure.errors,
            work,
        )
    })?;
    // Everything after CFG construction — drop-dependency attachment, warning
    // ordering, and semantic-output assembly — is the finalization tail. It
    // runs outside both `sema` and `cfg_construction`, so it needs its own leaf
    // for the pipeline residual to stay honest (RUE-786).
    let _finalization_span = info_span!("semantic_finalization").entered();
    let durable_specialized_body_payloads =
        crate::durable_body::attach_specialized_implicit_drop_dependencies(
            durable_specialized_body_payloads,
            &cfg.implicit_named_destructor_dependencies,
            merged,
            &authoritative_definitions,
            &mut durable_body_work,
        )
        .unwrap_or_else(|_| Arc::from([]));
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
        anonymous_nominal_associations,
        body_owner_issuer: authoritative_definitions,
        durable_ordinary_body_payloads,
        durable_specialized_body_payloads,
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
        body_references: BTreeMap::new(),
        durable_cfgs: cfg.durable_cfgs,
    })
}

/// Publish the strict preparation failure without invoking a second body
/// authority. Body diagnostics are already owned by BodyTransaction; a
/// declaration-bound epoch that cannot accept exact stable identities is not
/// permitted to run the retired reachable-body driver for recovery.
fn recover_declaration_failure(
    bound: rue_air::BoundSema<'_>,
    preparation_error: crate::CompileErrors,
    declaration_index: RirDeclarationIndexWork,
    stable_ids_requested: bool,
    declaration_reuse: CanonicalDeclarationReuseWork,
    body_owner_tokens: BodyOwnerTokenWork,
) -> CanonicalSemanticFailure {
    let binding = bound.binding_work();
    let manifest = bound.binding_manifest().work();
    CanonicalSemanticFailure::declaration(
        preparation_error,
        declaration_stage_work(
            declaration_index,
            binding,
            manifest,
            body_owner_tokens,
            BodyAnalysisWork::default(),
            stable_ids_requested,
            declaration_reuse,
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use rue_span::FileId;

    use super::{
        BodyOwnerTokenWork, CanonicalSemanticOutput, CanonicalSemanticWork,
        analyze_canonical_program_for_test_support,
    };
    use crate::parsed_modules::parse_source_snapshot_modules;
    use crate::{
        CanonicalRirOutput, CompileOptions, FunctionWithCfg, PreviewFeatures, SourceMetadata,
        SourceSnapshot, Target, lower_canonical_rir, merge_parsed_modules,
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
        let imports = crate::import_graph::import_free_canonical_graph(merged.ast()).unwrap();

        let ordinary = match rue_air::Sema::new_synthetic(
            rir.rir(),
            rir.semantic_symbols().interner(),
            options.preview_features.clone(),
        )
        .bind_declarations_for_test()
        {
            Err(errors) => errors,
            Ok(_) => panic!("test input must fail ordinary declaration binding"),
        };
        let canonical =
            analyze_canonical_program_for_test_support(&merged, &rir, &options, &imports, false)
                .unwrap_err();
        let messages = |errors: crate::CompileErrors| {
            errors.iter().map(ToString::to_string).collect::<Vec<_>>()
        };
        assert_eq!(messages(canonical), messages(ordinary));
    }

    #[test]
    fn token_preparation_failures_recover_ordinary_binding_diagnostics() {
        for source in [
            "const value: i32 = 1; const value: i32 = 2; fn main() {}",
            "const X: i32 = 1; fn X() -> i32 { 2 } fn main() -> i32 { 0 }",
            "enum Color { Red, Green } const Color: i32 = 5; fn main() -> i32 { Color }",
        ] {
            assert_token_preparation_preserves_source_errors(source);
        }
    }

    #[test]
    #[should_panic(
        expected = "canonical compiler epochs must import query-owned declaration shells"
    )]
    fn canonical_sema_cannot_invoke_raw_declaration_discovery() {
        let source = snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
        let parsed = parse_source_snapshot_modules(&source).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        let imports = crate::import_graph::import_free_canonical_graph(merged.ast()).unwrap();
        crate::bound_definitions::configure_canonical_sema(
            &merged,
            &rir,
            PreviewFeatures::new(),
            Target::default(),
            &imports,
        )
        .unwrap()
        .predeclare_declaration_shells_for_test()
        .unwrap();
    }

    fn canonical(
        snapshot: &SourceSnapshot,
        options: &CompileOptions,
        ids: bool,
    ) -> (CanonicalSemanticOutput, CanonicalRirOutput) {
        let parsed = parse_source_snapshot_modules(snapshot).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        let imports = crate::test_support::test_import_graph(snapshot).unwrap();
        let output =
            analyze_canonical_program_for_test_support(&merged, &rir, options, &imports, ids)
                .unwrap();
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
                // The named struct's method symbol is file-qualified
                // (`Value$<file>.choose`) under ADR-0066.
                .filter(|function| {
                    function.analyzed.name.starts_with("Value$")
                        && function.analyzed.name.ends_with(".choose")
                })
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
                // helper.rue carries its own `main` so the `different_root`
                // case below (root = file 2) is a valid program. Under RUE-920
                // a non-root `main` is an ordinary namespaced function, so this
                // `main` is inert when file 1 is the root.
                "pub fn helper() -> i32 { 42 } fn main() -> i32 { 0 }",
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

        // The designated root is its own input axis. It is exercised on an
        // import-free pair: designating `helper.rue` as the root of the
        // import-bearing fixture above would leave `main.rue` unreachable, and
        // a program's import graph covers only what discovery reaches from its
        // root.
        let roots = [
            (1, "/roots/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            (
                2,
                "/roots/helper.rue",
                "helper.rue",
                "fn main() -> i32 { 1 }",
            ),
        ];
        let (first_root, _) = canonical(&snapshot(&roots, 1), &base_options, false);
        let (second_root, _) = canonical(&snapshot(&roots, 2), &base_options, false);
        assert_ne!(first_root.input(), second_root.input());
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

    mod shared_declaration_projection {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::super::{
            SharedDeclarationProjection, SharedProjectionFailure, SharedProjectionOutcome,
        };
        use crate::durable_semantics::{
            DurableSemanticProjectionFailure, DurableSemanticProjectionWork,
        };
        use crate::source_identity::{ModuleRevision, SourceId, SourceRevision};

        fn revision(text: &str) -> SourceRevision {
            let module = crate::ModuleId::from_logical_path("main")
                .expect("`main` is a valid logical module path");
            SourceRevision::new(
                module.clone(),
                vec![ModuleRevision {
                    module,
                    source: SourceId::from_shared_text(Arc::new(text.to_owned())),
                }],
            )
            .expect("a single-module revision is well formed")
        }

        fn configuration() -> crate::semantic_query_nucleus::SemanticQueryConfiguration {
            crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: crate::Target::X86_64Linux,
                preview_features: crate::StablePreviewFeatures::new(
                    &crate::PreviewFeatures::default(),
                ),
            }
        }

        /// The memo is agnostic to the projection payload — it shares whatever
        /// the projector returned — so an empty export set is sufficient to
        /// exercise compute/reuse/fail-closed. Payload equivalence is covered by
        /// the projector's own tests and by the corpus differentials.
        fn exports() -> Arc<[rue_air::SemanticDeclarationExport]> {
            Arc::from(Vec::new())
        }

        /// The first body of a request projects; every later body reuses the
        /// exact same allocation rather than reprojecting. This is the whole
        /// point of RUE-1132, so it is asserted on `Arc` identity, not equality.
        #[test]
        fn later_bodies_reuse_the_first_body_s_projection() {
            let shared = SharedDeclarationProjection::new();
            let revision = revision("fn main() {}");
            let configuration = configuration();
            let projections = AtomicUsize::new(0);
            let project = || {
                projections.fetch_add(1, Ordering::Relaxed);
                Ok((exports(), DurableSemanticProjectionWork::default()))
            };

            let first = shared
                .get_or_project(&revision, &configuration, project)
                .expect("the first request projects");
            let SharedProjectionOutcome::Computed(first, _) = first else {
                panic!("the first request must compute");
            };
            let second = shared
                .get_or_project(&revision, &configuration, project)
                .expect("the second request reuses");
            let SharedProjectionOutcome::Reused(second) = second else {
                panic!("the second request must reuse, not recompute");
            };

            assert_eq!(
                projections.load(Ordering::Relaxed),
                1,
                "the projection must run exactly once per request"
            );
            assert!(
                Arc::ptr_eq(&first, &second),
                "every body must share one projection allocation"
            );
        }

        /// A memo that outlived its revision is a programming error, not a miss.
        /// It must never silently reproject into a slot owned by another
        /// revision, because the body would then observe a projection that does
        /// not belong to its own source.
        #[test]
        fn a_foreign_revision_fails_closed_instead_of_reprojecting() {
            let shared = SharedDeclarationProjection::new();
            let configuration = configuration();
            shared
                .get_or_project(&revision("fn main() {}"), &configuration, || {
                    Ok((exports(), DurableSemanticProjectionWork::default()))
                })
                .expect("the first request projects");

            let foreign = shared.get_or_project(
                &revision("fn main() { let x = 1; }"),
                &configuration,
                || panic!("a foreign revision must not be given the chance to reproject"),
            );

            assert!(matches!(
                foreign,
                Err(SharedProjectionFailure::ForeignRequest)
            ));
        }

        /// The same fail-closed rule applies to configuration: a body compiled
        /// for another target or preview-feature set must not adopt this
        /// request's exports.
        #[test]
        fn a_foreign_configuration_fails_closed() {
            let shared = SharedDeclarationProjection::new();
            let revision = revision("fn main() {}");
            shared
                .get_or_project(&revision, &configuration(), || {
                    Ok((exports(), DurableSemanticProjectionWork::default()))
                })
                .expect("the first request projects");

            let mut foreign_configuration = configuration();
            foreign_configuration.target = crate::Target::Aarch64Linux;
            let foreign = shared.get_or_project(&revision, &foreign_configuration, || {
                panic!("a foreign configuration must not be given the chance to reproject")
            });

            assert!(matches!(
                foreign,
                Err(SharedProjectionFailure::ForeignRequest)
            ));
        }

        /// A failed projection must not fill the memo: the failure is
        /// deterministic and every body must observe it, rather than the first
        /// body failing and later bodies reusing a half-filled slot.
        #[test]
        fn a_failed_projection_leaves_the_memo_empty() {
            let shared = SharedDeclarationProjection::new();
            let revision = revision("fn main() {}");
            let configuration = configuration();

            let failed = shared.get_or_project(&revision, &configuration, || {
                Err(DurableSemanticProjectionFailure::AmbiguousDefinition)
            });
            assert!(matches!(
                failed,
                Err(SharedProjectionFailure::Projection(_))
            ));

            let retried = shared
                .get_or_project(&revision, &configuration, || {
                    Ok((exports(), DurableSemanticProjectionWork::default()))
                })
                .expect("a later body reprojects after a failure");
            assert!(
                matches!(retried, SharedProjectionOutcome::Computed(..)),
                "a failure must not be cached as a reusable projection"
            );
        }
    }

    /// RUE-1135: the request-scoped immutable declaration base.
    ///
    /// The base's whole claim is that sharing one bound declaration epoch across
    /// a request's bodies is indistinguishable from building one per body. These
    /// tests attack that claim from both ends: body-for-body publication parity
    /// against an independently built epoch, and schedule-order independence
    /// between bodies that share a base.
    mod shared_declaration_base {
        use std::sync::Arc;

        use super::super::{
            SharedDeclarationBase, SharedDeclarationProjection, analyze_body_in_epoch,
            build_bound_body_epoch,
        };
        use crate::{
            CompileOptions, CompilerSession, SourceSnapshot, StableDefinitionKind,
            StablePreviewFeatures,
        };

        /// Inputs for driving bodies through an explicit declaration base,
        /// without the session's traversal in the way.
        struct BaseInputs {
            merged: crate::CanonicalMergedProgram,
            rir: crate::CanonicalRirOutput,
            options: CompileOptions,
            imports: crate::CanonicalImportGraph,
            query_shells: Vec<rue_air::SemanticDeclarationShell>,
            query_declarations: Arc<[crate::durable_semantics::DurableDeclarationSemantic]>,
            definitions: crate::bound_definitions::BoundDefinitionSet,
        }

        impl BaseInputs {
            fn build(source: &str) -> Self {
                let snapshot = SourceSnapshot::single("main.rue", source).expect("corpus parses");
                let parsed =
                    crate::parsed_modules::parse_source_snapshot_modules(&snapshot).unwrap();
                let merged = crate::merge_parsed_modules(&parsed).unwrap();
                let rir = crate::lower_canonical_rir(&merged).unwrap();
                let options = CompileOptions::default();
                let imports =
                    crate::import_graph::import_free_canonical_graph(merged.ast()).unwrap();
                let (definitions, query_declarations, _) =
                    crate::bound_definitions::bind_canonical_declaration_semantics(
                        &merged,
                        &rir,
                        options.preview_features.clone(),
                        options.target,
                        &imports,
                    )
                    .unwrap();
                let query_shells = super::super::query_owned_declaration_shells_for_test(
                    &merged,
                    &rir,
                    options.preview_features.clone(),
                    options.target,
                    &imports,
                )
                .unwrap()
                .declaration_shells()
                .cloned()
                .collect::<Vec<_>>();
                Self {
                    merged,
                    rir,
                    options,
                    imports,
                    query_shells,
                    query_declarations,
                    definitions,
                }
            }

            fn body_key(
                &self,
                kind: StableDefinitionKind,
                name: &str,
            ) -> crate::body_query::BodyQueryKey {
                let definition = self
                    .definitions
                    .definitions()
                    .iter()
                    .find(|record| {
                        record.stable_key().kind() == kind && record.stable_key().name() == name
                    })
                    .unwrap_or_else(|| panic!("no {kind:?} named {name}"))
                    .stable_key()
                    .clone();
                crate::body_query::BodyQueryKey {
                    instance: crate::FunctionInstanceKey::Definition(definition),
                    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                        target: self.options.target,
                        preview_features: StablePreviewFeatures::new(
                            &self.options.preview_features,
                        ),
                    },
                }
            }

            /// One freshly built, independently derived epoch for `key` — the
            /// per-body construction the base replaced.
            fn independent(
                &self,
                key: &crate::body_query::BodyQueryKey,
            ) -> crate::body_query::BodyTransaction {
                let mut work = rue_air::PerBodyDeclarationContextWork::default();
                let projection = SharedDeclarationProjection::new();
                let epoch = build_bound_body_epoch(
                    &self.merged,
                    &self.rir,
                    &self.options,
                    &self.imports,
                    &self.query_shells,
                    &self.query_declarations,
                    &[],
                    &crate::body_query::WellKnownOptionResolution::default(),
                    key,
                    &projection,
                    &mut work,
                )
                .expect("the corpus builds an epoch");
                analyze_body_in_epoch(
                    epoch,
                    &self.merged,
                    key,
                    &rue_query::CancellationToken::new(),
                )
                .expect("an uncanceled body publishes")
            }
        }

        /// A base retained across a whole sequence of bodies, driven directly.
        fn analyze_through_one_base(
            inputs: &BaseInputs,
            keys: &[crate::body_query::BodyQueryKey],
        ) -> Vec<crate::body_query::BodyTransaction> {
            let projection = SharedDeclarationProjection::new();
            let base = SharedDeclarationBase::isolated();
            let mut work = rue_air::PerBodyDeclarationContextWork::default();
            keys.iter()
                .map(|key| {
                    let epoch = base
                        .get_or_derive(
                            &inputs.merged,
                            &inputs.rir,
                            &inputs.options,
                            &inputs.imports,
                            &inputs.query_shells,
                            &inputs.query_declarations,
                            &[],
                            &crate::body_query::WellKnownOptionResolution::default(),
                            key,
                            &projection,
                            &mut work,
                        )
                        .expect("the corpus derives an epoch");
                    analyze_body_in_epoch(
                        epoch,
                        &inputs.merged,
                        key,
                        &rue_query::CancellationToken::new(),
                    )
                    .expect("an uncanceled body publishes")
                })
                .collect()
        }

        /// Corpora chosen for what a shared base is most likely to get wrong.
        /// The plain-function case is the RUE-1133 probe's shape; the rest add
        /// the families that corpus made invisible — named structs and methods,
        /// a `-> type` producer that materializes anonymous nominals into the
        /// epoch, and a body whose analysis fails.
        const CASES: &[(&str, &[(StableDefinitionKind, &str)])] = &[
            (
                "fn helper() -> i32 { 7 } fn other() -> i32 { 9 } \
                 fn main() -> i32 { helper() + other() }",
                &[
                    (StableDefinitionKind::Function, "helper"),
                    (StableDefinitionKind::Function, "other"),
                    (StableDefinitionKind::Function, "main"),
                ],
            ),
            (
                "struct Counter { value: i32, fn get(self) -> i32 { self.value } \
                 fn twice(self) -> i32 { self.value + self.value } } \
                 fn main() -> i32 { Counter { value: 3 }.get() }",
                &[
                    (StableDefinitionKind::Method, "get"),
                    (StableDefinitionKind::Method, "twice"),
                    (StableDefinitionKind::Function, "main"),
                ],
            ),
            (
                "fn Pair() -> type { struct { a: i32, b: i32 } } \
                 fn Trip() -> type { struct { a: i32, b: i32, c: i32 } } \
                 fn use_pair() -> i32 { let P = Pair(); let p: P = P { a: 1, b: 2 }; p.a + p.b } \
                 fn main() -> i32 { use_pair() }",
                &[
                    (StableDefinitionKind::Function, "Pair"),
                    (StableDefinitionKind::Function, "Trip"),
                    (StableDefinitionKind::Function, "use_pair"),
                    (StableDefinitionKind::Function, "main"),
                ],
            ),
            (
                "fn ok() -> i32 { 1 } fn bad() -> i32 { true } fn main() -> i32 { ok() }",
                &[
                    (StableDefinitionKind::Function, "ok"),
                    (StableDefinitionKind::Function, "bad"),
                    (StableDefinitionKind::Function, "main"),
                ],
            ),
        ];

        // The central claim: a body analyzed inside an epoch derived from a
        // shared base publishes exactly what the same body publishes inside a
        // freshly built epoch. `transaction_equal` compares the canonical body,
        // its references, its produced anonymous nominals, and the rendered
        // diagnostic stream including order, so a divergence in artifacts,
        // identities, or diagnostic ORDER fails here.
        #[test]
        fn base_derived_bodies_publish_what_independently_built_bodies_publish() {
            for (source, bodies) in CASES {
                let inputs = BaseInputs::build(source);
                let keys = bodies
                    .iter()
                    .map(|(kind, name)| inputs.body_key(*kind, name))
                    .collect::<Vec<_>>();
                let derived = analyze_through_one_base(&inputs, &keys);
                for (key, derived) in keys.iter().zip(&derived) {
                    let independent = inputs.independent(key);
                    assert!(
                        crate::body_query::transaction_equal(&independent, derived),
                        "a base-derived body diverged from an independently built \
                         one for {:?}:\n  independent={independent:?}\n  derived={derived:?}",
                        key.instance
                    );
                }
            }
        }

        // Schedule-order independence — the risk a shared base introduces and
        // the one the RUE-1133 probe's deep copy could not test. The
        // pre-cutover batch compiler shared one epoch *sequentially*, so a body
        // could observe what an earlier body materialized. Here the same bodies
        // are analyzed through one base in a forward order and in the reverse
        // order; each body must publish the same transaction either way.
        #[test]
        fn bodies_sharing_one_base_publish_the_same_transaction_in_any_order() {
            for (source, bodies) in CASES {
                let inputs = BaseInputs::build(source);
                let keys = bodies
                    .iter()
                    .map(|(kind, name)| inputs.body_key(*kind, name))
                    .collect::<Vec<_>>();
                let forward = analyze_through_one_base(&inputs, &keys);
                let mut reversed_keys = keys.clone();
                reversed_keys.reverse();
                let mut reversed = analyze_through_one_base(&inputs, &reversed_keys);
                reversed.reverse();
                for ((key, forward), reversed) in keys.iter().zip(&forward).zip(&reversed) {
                    assert!(
                        crate::body_query::transaction_equal(forward, reversed),
                        "body {:?} published differently depending on the order its \
                         siblings ran in:\n  forward={forward:?}\n  reversed={reversed:?}",
                        key.instance
                    );
                }
            }
        }

        // The base is a base: deriving an epoch from it, analyzing a body, and
        // deriving again must leave the base's own universe untouched. If a
        // body's interning reached the base's type pool or parameter arena
        // instead of its own overlay, these sizes would grow.
        #[test]
        fn deriving_and_analyzing_never_grows_the_base_universe() {
            let inputs = BaseInputs::build(CASES[2].0);
            let keys = CASES[2]
                .1
                .iter()
                .map(|(kind, name)| inputs.body_key(*kind, name))
                .collect::<Vec<_>>();
            let projection = SharedDeclarationProjection::new();
            let mut work = rue_air::PerBodyDeclarationContextWork::default();
            let base = build_bound_body_epoch(
                &inputs.merged,
                &inputs.rir,
                &inputs.options,
                &inputs.imports,
                &inputs.query_shells,
                &inputs.query_declarations,
                &[],
                &crate::body_query::WellKnownOptionResolution::default(),
                &keys[0],
                &projection,
                &mut work,
            )
            .expect("the corpus builds a base");
            let before = base.shared_universe_entries() + base.local_universe_entries();
            // Sealing moves the base's own type-pool and parameter universe into
            // the shared immutable layer without changing what it contains. Every
            // later derivation shares that layer by refcount.
            let mut base = base;
            base.seal_as_declaration_base();
            assert_eq!(
                base.local_universe_entries(),
                0,
                "sealing moves the base's whole universe into its shared layer"
            );
            assert_eq!(
                base.shared_universe_entries(),
                before,
                "sealing must not change what the base contains"
            );
            for key in &keys {
                let mut units = rue_air::EpochDerivationUnits::default();
                let derived = base.derive_body_epoch(&mut units);
                assert_eq!(
                    derived.local_universe_entries(),
                    0,
                    "a derivation must copy no type-pool or parameter entry"
                );
                assert_eq!(
                    derived.shared_universe_entries(),
                    before,
                    "a derivation must read the whole base universe"
                );
                assert!(
                    units.total_units_shared() > 0,
                    "a derivation shares the base's declaration universe: {units:?}"
                );
                assert!(
                    units.canonical_import_entries_shared > 0,
                    "canonical import state must be accounted as shared: {units:?}"
                );
                assert!(
                    units.type_side_table_entries_shared > 0,
                    "type-pool side tables must be accounted as shared: {units:?}"
                );
                analyze_body_in_epoch(
                    derived,
                    &inputs.merged,
                    key,
                    &rue_query::CancellationToken::new(),
                )
                .expect("an uncanceled body publishes");
            }
            assert_eq!(
                base.shared_universe_entries() + base.local_universe_entries(),
                before,
                "analyzing bodies must not extend the shared base's universe"
            );
        }

        /// A corpus with several unrelated declarations and several reached
        /// bodies, so a per-body epoch would be genuinely O(declarations).
        fn corpus(bodies: usize, decls: usize) -> SourceSnapshot {
            let mut source = String::new();
            for index in 0..bodies {
                source.push_str(&format!("fn b{index}() -> i32 {{ {index} }}\n"));
            }
            for index in 0..decls {
                source.push_str(&format!("struct D{index} {{ n: i32 }}\n"));
            }
            source.push_str("fn main() -> i32 {\n    let mut acc = 0;\n");
            for index in 0..bodies {
                source.push_str(&format!("    acc = acc + b{index}();\n"));
            }
            source.push_str("    acc\n}\n");
            SourceSnapshot::single("main.rue", source).expect("synthetic corpus parses")
        }

        fn compile(
            snapshot: &SourceSnapshot,
            differential: bool,
        ) -> (
            crate::CanonicalSemanticWork,
            crate::unstable::DeclarationBaseMetrics,
        ) {
            let mut session = CompilerSession::new();
            session.enable_shared_declaration_base_differential(differential);
            session.update(snapshot).into_result().unwrap();
            let output = session
                .canonical_semantic(&CompileOptions::default())
                .expect("synthetic corpus compiles");
            (output.work(), session.declaration_base_metrics())
        }

        // One base serves an entire request. This is the premise: if the
        // retained base could not be reused, "build once per request" would not
        // describe what ran, and the O(declarations) factor would still be
        // charged per body.
        #[test]
        fn one_declaration_base_serves_every_body_of_a_request() {
            let (work, metrics) = compile(&corpus(8, 16), false);
            assert_eq!(metrics.bases_built, 1, "{metrics:?}");
            assert_eq!(metrics.base_input_misses, 0, "{metrics:?}");
            assert!(metrics.epochs_derived >= 9, "{metrics:?}");
            let context = &work.body_analysis.per_body_declaration_context;
            assert_eq!(
                context.declaration_bases_built, 1,
                "the stage counters agree with the meter: {context:?}"
            );
            assert_eq!(
                context.body_epochs_derived, metrics.epochs_derived as usize,
                "{context:?}"
            );
        }

        // The ordinary per-body declaration-context counters stay honest. The
        // prepare/project/install/endpoint stages now genuinely run once per
        // request instead of once per body, so those counters record ONE prefix
        // — they are not reclassified or left reading a total the compiler no
        // longer performs. The shared-unit counters account for what replaced
        // them, and the two must be read together.
        #[test]
        fn the_shared_base_charges_one_prefix_and_reports_what_it_shared() {
            let (work, metrics) = compile(&corpus(8, 16), false);
            let context = &work.body_analysis.per_body_declaration_context;
            assert_eq!(
                context.cold_body_preparations, 1,
                "the request prepares exactly one declaration epoch: {context:?}"
            );
            assert!(
                context.body_epochs_derived > 4 * context.cold_body_preparations,
                "many bodies were served by that one prefix: {context:?}"
            );
            // The work did not vanish, it changed shape: every derivation reports
            // the units it read from the base rather than copied.
            assert!(context.declaration_units_shared > 0, "{context:?}");
            assert!(
                context.declaration_units_shared > context.body_local_units_copied,
                "sharing must dominate copying: {context:?}"
            );
            assert_eq!(
                context.declaration_units_shared % context.body_epochs_derived as u64,
                0,
                "every body reads the same base, so the shared total is a clean \
                 multiple of the derivation count: {context:?}"
            );
            assert_eq!(metrics.parity_comparisons, 0, "{metrics:?}");
        }

        // The whole-request parity oracle. Every reached body of a real compile
        // is analyzed both ways and compared, including the multi-body traversal,
        // the well-known `Option` registry, and producer nominals that are
        // installed INTO the epoch.
        #[test]
        fn every_reached_body_of_a_request_survives_the_parity_oracle() {
            let source = "\
fn Pair() -> type { struct { a: i32, b: i32 } }
struct Counter { value: i32, fn get(self) -> i32 { self.value } }
fn use_pair() -> i32 { let P = Pair(); let p: P = P { a: 1, b: 2 }; p.a + p.b }
fn use_counter() -> i32 { Counter { value: 4 }.get() }
fn main() -> i32 { use_pair() + use_counter() }
";
            let snapshot = SourceSnapshot::single("main.rue", source).unwrap();
            let (work, metrics) = compile(&snapshot, true);
            assert!(
                work.body_analysis.bodies_succeeded >= 5,
                "the corpus must reach the anonymous members: {metrics:?}"
            );
            assert_eq!(
                metrics.parity_comparisons, metrics.epochs_derived,
                "every body served from the base was compared: {metrics:?}"
            );
            assert_eq!(
                metrics.parity_mismatches, 0,
                "a base-derived body diverged from an independently built one: {metrics:?}"
            );
        }

        // A compile that never asks for the oracle never runs it, and the base
        // meter still records the production path. This is what keeps the
        // validation arm out of ordinary compiles.
        #[test]
        fn an_ordinary_compile_runs_the_base_but_not_the_oracle() {
            let (work, metrics) = compile(&corpus(4, 8), false);
            assert_eq!(metrics.parity_comparisons, 0, "{metrics:?}");
            assert_eq!(metrics.parity_mismatches, 0, "{metrics:?}");
            assert!(metrics.epochs_derived > 0, "{metrics:?}");
            assert!(work.body_analysis.bodies_succeeded > 0);
        }

        // Sharing a base does not change what the compiler emits. The same
        // corpus compiled with the oracle on and off produces the same body
        // analysis totals and the same diagnostics.
        #[test]
        fn the_oracle_does_not_change_what_the_request_produces() {
            let snapshot = corpus(6, 12);
            let (plain, _) = compile(&snapshot, false);
            let (checked, metrics) = compile(&snapshot, true);
            assert_eq!(metrics.parity_mismatches, 0, "{metrics:?}");
            assert_eq!(
                plain.body_analysis.bodies_succeeded,
                checked.body_analysis.bodies_succeeded
            );
            assert_eq!(
                plain.body_analysis.air_instructions_produced,
                checked.body_analysis.air_instructions_produced
            );
        }
    }
}
