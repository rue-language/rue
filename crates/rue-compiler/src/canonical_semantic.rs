//! Semantic work accounting and declaration-only test fixtures.

use rue_air::{
    BodyAnalysisWork, DeclarationBindingWork, RirDeclarationIndexWork, SemanticBindingManifestWork,
};

#[cfg(test)]
use crate::{
    BoundDefinitionSet, CanonicalImportGraph, CanonicalMergedProgram, CanonicalRirOutput,
    CompileOptions, MultiErrorResult,
    bound_definitions::{configure_canonical_sema, issue_shell_definitions},
};

/// One current-revision declaration epoch prepared for either ordinary
/// resolution or durable installation. Stable identities are issued from the
/// same shells that the selected analysis path subsequently consumes.
#[cfg(test)]
pub(crate) struct CanonicalPreparedDeclarations<'a> {
    shells: rue_air::DeclarationShells<'a>,
    definitions: BoundDefinitionSet,
}

#[cfg(test)]
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
    /// Request-local declaration prefixes constructed.
    pub declaration_prefixes_built: usize,
    pub declaration_indexes_built: usize,
    pub declaration_prefix_population_runs: usize,
    pub durable_cache_population_exports: usize,
    pub declaration_prefix_fallbacks: usize,
}

#[cfg(test)]
pub(crate) fn prepare_query_declaration_shells<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    options: &CompileOptions,
    imports: &CanonicalImportGraph,
    query_shells: &[rue_air::SemanticDeclarationShell],
) -> Result<CanonicalPreparedDeclarations<'a>, CanonicalSemanticFailure> {
    let sema = configure_canonical_sema(
        merged,
        rir,
        options.preview_features.clone(),
        options.target,
        imports,
    )
    .map_err(|errors| {
        CanonicalSemanticFailure::declaration(errors, CanonicalSemanticWork::default())
    })?;
    let declaration_index = sema.rir_declaration_index_work();
    let declaration_reuse = CanonicalDeclarationReuseWork {
        declaration_prefixes_built: 1,
        declaration_indexes_built: declaration_index.build_invocations,
        declaration_prefix_population_runs: 1,
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
        definitions,
    })
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
    /// Exact work performed to make AIR body ownership authoritative.
    pub body_owner_tokens: BodyOwnerTokenWork,
    /// Demand-driven function-body analysis work.
    pub body_analysis: BodyAnalysisWork,
    /// Durable body comparison, import, export, reuse, and fallback work.
    pub durable_bodies: crate::DurableBodyWork,
    /// Drop-glue, CFG construction, and optimization work.
    pub cfg: CfgConstructionWork,
    #[cfg(test)]
    pub declaration_reuse: CanonicalDeclarationReuseWork,
}

impl CanonicalSemanticWork {
    #[allow(dead_code)]
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
        body.closure_bodies_visited = body
            .closure_bodies_visited
            .max(query.closure_bodies_visited);
        body.max_specialization_depth = body
            .max_specialization_depth
            .max(query.max_specialization_depth);
        body.reachability_frontier_scans += query.reachability_frontier_scans;
        body.reachability_frontier_scan_keys += query.reachability_frontier_scan_keys;
        body.reachability_frontier_batches += query.reachability_frontier_batches;
        body.reachability_frontier_keys += query.reachability_frontier_keys;
        body.reachability_frontier_width_one += query.reachability_frontier_width_one;
        body.reachability_frontier_width_two_to_three +=
            query.reachability_frontier_width_two_to_three;
        body.reachability_frontier_width_four_to_seven +=
            query.reachability_frontier_width_four_to_seven;
        body.reachability_frontier_width_eight_or_more +=
            query.reachability_frontier_width_eight_or_more;
        body.reachability_transactions_prefetched += query.reachability_transactions_prefetched;
        body.reachability_transactions_serial += query.reachability_transactions_serial;
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
    pub materialization_index_builds: usize,
    pub materialization_declarations_scanned: usize,
    pub materialization_anonymous_nominals_scanned: usize,
    pub materialization_type_nodes_scanned: usize,
    pub materialization_fact_selections: usize,
    pub retained_interner_charge_scans: usize,
    pub retained_interner_entries_scanned: usize,
    pub retained_interner_utf8_bytes_scanned: usize,
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

#[cfg(test)]
pub(crate) struct CanonicalSemanticFailure {
    pub(crate) errors: crate::CompileErrors,
}

#[cfg(test)]
impl CanonicalSemanticFailure {
    pub(crate) fn declaration(errors: crate::CompileErrors, _work: CanonicalSemanticWork) -> Self {
        Self { errors }
    }
}

#[cfg(test)]
fn declaration_stage_work(
    declaration_index: RirDeclarationIndexWork,
    binding: DeclarationBindingWork,
    manifest: SemanticBindingManifestWork,
    body_owner_tokens: BodyOwnerTokenWork,
    body_analysis: BodyAnalysisWork,
    declaration_reuse: CanonicalDeclarationReuseWork,
) -> CanonicalSemanticWork {
    CanonicalSemanticWork {
        declaration_index,
        binding,
        manifest,
        body_owner_tokens,
        body_analysis,
        declaration_reuse,
        ..CanonicalSemanticWork::default()
    }
}
