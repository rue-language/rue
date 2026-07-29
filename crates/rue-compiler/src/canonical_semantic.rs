//! One-pass canonical declaration binding, body analysis, and CFG lowering.

use std::{collections::BTreeMap, sync::Arc};

use rue_air::{
    AnalyzedBodyOwnerEvent, BodyAnalysisWork, BodyNamedDependencyEvent, DeclarationBindingWork,
    DeclarationBuiltinTypeCallHeadDependencyEvent, DeclarationTypeCallHeadDependencyEvent,
    DeclarationTypeDependencyEvent, NamedConstDependencyEvent, NamedDestructorDependencyEvent,
    NamedMethodDependencyEvent, OrdinaryFreeFunctionDependencyEvent, RirDeclarationIndexWork,
    SemanticBindingManifestWork, SpecializedFreeFunctionDependencyEvent,
    SpecializedFreeFunctionOrigin,
};
use tracing::info_span;

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
    queries::collect_function_cfg_queries,
};

#[cfg(test)]
thread_local! {
    static INJECT_CFG_FAILURE: Cell<bool> = const { Cell::new(false) };
    static INJECT_CFG_IMPORT_FAILURE: Cell<bool> = const { Cell::new(false) };
    static INJECT_DECLARATION_FAILURE: Cell<bool> = const { Cell::new(false) };
    static INJECT_AUTHORITATIVE_KEY_MISMATCH: Cell<bool> = const { Cell::new(false) };
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
pub(crate) fn with_test_cfg_import_failure_injection<T>(run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INJECT_CFG_IMPORT_FAILURE.with(|enabled| enabled.set(false));
        }
    }

    INJECT_CFG_IMPORT_FAILURE.with(|enabled| {
        assert!(
            !enabled.replace(true),
            "CFG import failure injection is not nestable"
        );
    });
    let _reset = Reset;
    run()
}

#[cfg(test)]
pub(crate) fn take_test_cfg_import_failure() -> bool {
    INJECT_CFG_IMPORT_FAILURE.with(|enabled| enabled.replace(false))
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
    /// Request-local declaration prefixes constructed.
    pub declaration_prefixes_built: usize,
    pub declaration_indexes_built: usize,
    pub declaration_prefix_population_runs: usize,
    pub durable_cache_population_exports: usize,
    pub declaration_prefix_fallbacks: usize,
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
            declaration_prefixes_built: 1,
            declaration_indexes_built: self.declaration_index.build_invocations,
            declaration_prefix_population_runs: 1,
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
    demanded_drop_glue: Arc<[crate::TypeInstanceKey]>,
    demanded_drop_glue_plans: Arc<[(crate::TypeInstanceKey, crate::type_queries::DropGlueFacts)]>,
    body_work: crate::DurableBodyWork,
    cfg_queries: &crate::revisioned_query_database::RevisionedQueryDatabase,
    revision: rue_query::Revision,
    cancellation: rue_query::CancellationToken,
) -> Result<CanonicalSemanticOutput, CanonicalSemanticFailure> {
    // Rebinding the durable declaration records and rebuilding the query
    // dependency edges runs before whole-program analysis opens `sema`. It is
    // real per-declaration work, so it gets its own leaf instead of widening
    // the pipeline's unattributed residual (RUE-786).
    let declaration_reuse_span =
        info_span!("declaration_reuse", phase = "semantic_analysis").entered();
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
        declaration_prefixes_built: 1,
        declaration_prefix_population_runs: 1,
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
        demanded_drop_glue,
        demanded_drop_glue_plans,
        body_work,
        sema_span,
        cfg_queries,
        revision,
        cancellation,
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
    demanded_drop_glue: Arc<[crate::TypeInstanceKey]>,
    demanded_drop_glue_plans: Arc<[(crate::TypeInstanceKey, crate::type_queries::DropGlueFacts)]>,
    durable_body_reuse_work: crate::DurableBodyWork,
    sema_span: tracing::span::EnteredSpan,
    cfg_queries: &crate::revisioned_query_database::RevisionedQueryDatabase,
    revision: rue_query::Revision,
    cancellation: rue_query::CancellationToken,
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
        demanded_drop_glue,
        demanded_drop_glue_plans,
        durable_body_reuse_work,
        sema_span,
        cfg_queries,
        revision,
        cancellation,
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
    demanded_drop_glue: Arc<[crate::TypeInstanceKey]>,
    demanded_drop_glue_plans: Arc<[(crate::TypeInstanceKey, crate::type_queries::DropGlueFacts)]>,
    mut durable_body_reuse_work: crate::DurableBodyWork,
    sema_span: tracing::span::EnteredSpan,
    cfg_queries: &crate::revisioned_query_database::RevisionedQueryDatabase,
    revision: rue_query::Revision,
    cancellation: rue_query::CancellationToken,
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
        .map(|candidate| {
            (
                candidate.identity.clone(),
                (candidate.body_span, candidate.body.clone()),
            )
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
    let issue_type = |stable: &crate::TypeInstanceKey| {
        stable.try_map_identities(
            &|definition| authoritative_definitions.semantic_token_for_key(definition),
            &|module| authoritative_definitions.module_token_for(merged, module),
        )
    };
    let demanded_issued_drop_glue = demanded_drop_glue
        .iter()
        .map(|stable| {
            issue_type(stable).map_err(|failure| {
                CanonicalSemanticFailure::new(
                    CanonicalSemanticFailurePhase::BodyAnalysis,
                    crate::CompileErrors::from(crate::CompileError::without_span(
                        rue_error::ErrorKind::InternalError(format!(
                            "failed to issue demanded drop-glue owner {stable:?}: {failure:?}"
                        )),
                    )),
                    CanonicalSemanticWork {
                        declaration_index,
                        binding,
                        manifest: manifest_work,
                        bound_definitions: Some(authoritative_definitions.work()),
                        body_analysis,
                        durable_bodies: durable_body_work,
                        declaration_reuse,
                        ..CanonicalSemanticWork::default()
                    },
                )
            })
        })
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    for issued in &demanded_issued_drop_glue {
        if !sema_output.aggregate_types_by_identity.contains_key(issued) {
            return Err(CanonicalSemanticFailure::new(
                CanonicalSemanticFailurePhase::BodyAnalysis,
                crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(
                        "demanded drop-glue owner was not materialized by reached AIR".into(),
                    ),
                )),
                CanonicalSemanticWork {
                    declaration_index,
                    binding,
                    manifest: manifest_work,
                    bound_definitions: Some(authoritative_definitions.work()),
                    body_analysis,
                    durable_bodies: durable_body_work,
                    declaration_reuse,
                    ..CanonicalSemanticWork::default()
                },
            ));
        }
    }
    let demanded_issued_drop_glue_plans = demanded_drop_glue_plans
        .iter()
        .map(|(stable, plan)| {
            issue_type(stable).and_then(|issued| {
                plan.try_map_identities(
                    &|definition| authoritative_definitions.semantic_token_for_key(definition),
                    &|module| authoritative_definitions.module_token_for(merged, module),
                )
                .map(|plan| (issued, plan))
            })
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
        .map_err(|failure| {
            CanonicalSemanticFailure::new(
                CanonicalSemanticFailurePhase::BodyAnalysis,
                crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(format!(
                        "failed to issue demanded drop-glue plan owner: {failure:?}"
                    )),
                )),
                CanonicalSemanticWork {
                    declaration_index,
                    binding,
                    manifest: manifest_work,
                    bound_definitions: Some(authoritative_definitions.work()),
                    body_analysis,
                    durable_bodies: durable_body_work,
                    declaration_reuse,
                    ..CanonicalSemanticWork::default()
                },
            )
        })?;
    let stable_drop_glue_plans = demanded_drop_glue_plans
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    let stable_aggregate_types = sema_output
        .aggregate_types_by_identity
        .iter()
        .map(|(issued, live)| {
            let callable = rue_air::FunctionInstanceKey::DropGlue(Box::new(issued.clone()));
            let stable =
                project_function_instance_key(&callable, merged, &authoritative_definitions)?;
            let crate::FunctionInstanceKey::DropGlue(stable) = stable else {
                unreachable!("drop-glue projection preserves its callable kind")
            };
            Ok::<_, rue_air::SemanticStableResolutionFailure>((*live, *stable))
        })
        .collect::<Result<std::collections::HashMap<_, _>, _>>()
        .map_err(|failure| {
            CanonicalSemanticFailure::new(
                CanonicalSemanticFailurePhase::BodyAnalysis,
                crate::CompileErrors::from(crate::CompileError::without_span(
                    rue_error::ErrorKind::InternalError(format!(
                        "failed to project reached aggregate type identity: {failure:?}"
                    )),
                )),
                CanonicalSemanticWork {
                    declaration_index,
                    binding,
                    manifest: manifest_work,
                    bound_definitions: Some(authoritative_definitions.work()),
                    body_analysis,
                    durable_bodies: durable_body_work,
                    declaration_reuse,
                    ..CanonicalSemanticWork::default()
                },
            )
        })?;
    let issued_callable_identities = sema_output
        .functions
        .iter()
        .map(|function| function.identity.clone())
        .chain(
            demanded_issued_drop_glue
                .iter()
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
            stable_cfg_inputs.push(crate::cfg_query::CfgBodyInput {
                function: function_key,
                body,
                body_span,
            });
        }
    }
    stable_cfg_inputs.sort_by(|left, right| left.function.cmp(&right.function));
    drop(sema_span);
    // CFG construction is a sibling of semantic-proper, not nested inside it:
    // `sema_span` closes above before this begins. Published phases must not
    // overlap, so this boundary is load-bearing rather than stylistic. The
    // `cfg_and_optimization` phase marker lives on `cfg_construction` inside
    // `collect_function_cfg_queries`.
    let cfg = collect_function_cfg_queries(
        sema_output,
        &demanded_issued_drop_glue,
        &demanded_issued_drop_glue_plans,
        &stable_drop_glue_plans,
        options.opt_level,
        rir.semantic_symbols().shared_interner(),
        &stable_cfg_inputs,
        stable_aggregate_types,
        &projected_callable_identities,
        cfg_queries,
        revision,
        crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: options.target,
            preview_features: crate::StablePreviewFeatures::new(&options.preview_features),
        },
        cancellation,
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

    use super::{BodyOwnerTokenWork, CanonicalSemanticOutput, CanonicalSemanticWork};
    use crate::{
        CanonicalRirOutput, CompileOptions, CompilerSession, FunctionWithCfg, PreviewFeatures,
        SourceMetadata, SourceSnapshot, Target,
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
        let stages = crate::test_support::test_frontend_stages(&source).unwrap();
        let rir = &stages.rir;
        let options = CompileOptions::default();
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
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let canonical = session.canonical_semantic(&options).unwrap_err();
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
        let stages = crate::test_support::test_frontend_stages(&source).unwrap();
        let merged = &stages.merged;
        let rir = &stages.rir;
        let imports = crate::import_graph::import_free_canonical_graph(merged.ast()).unwrap();
        crate::bound_definitions::configure_canonical_sema(
            merged,
            rir,
            PreviewFeatures::new(),
            Target::default(),
            &imports,
        )
        .unwrap()
        .predeclare_declaration_shells_for_test()
        .unwrap();
    }

    /// Analyze a fixture the way production does: one session, its own RIR and
    /// semantic queries.
    fn canonical(
        snapshot: &SourceSnapshot,
        options: &CompileOptions,
    ) -> (Arc<CanonicalSemanticOutput>, Arc<CanonicalRirOutput>) {
        let mut session = CompilerSession::new();
        if crate::test_support::fixture_has_imports(snapshot).unwrap() {
            crate::test_support::TestDiscoveryHost::new(snapshot)
                .unwrap()
                .drive(&mut session)
                .unwrap();
        } else {
            session.update(snapshot).into_result().unwrap();
        }
        let output = session.canonical_semantic(options).unwrap();
        let rir = session.selected_semantic_rir_owner().unwrap();
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

    fn function_base_definition(
        identity: &crate::FunctionInstanceKey,
    ) -> Option<&crate::StableDefinitionKey> {
        match identity {
            crate::FunctionInstanceKey::Definition(definition) => Some(definition),
            crate::FunctionInstanceKey::Specialization { base, .. } => {
                function_base_definition(base)
            }
            crate::FunctionInstanceKey::AnonymousMember { .. }
            | crate::FunctionInstanceKey::DropGlue(_) => None,
        }
    }

    fn specialized_functions<'a>(
        output: &'a CanonicalSemanticOutput,
        base_name: &'a str,
    ) -> impl Iterator<
        Item = (
            &'a crate::StableDefinitionKey,
            &'a crate::CanonicalArguments,
        ),
    > {
        output.functions().iter().filter_map(move |function| {
            let crate::FunctionInstanceKey::Specialization { base, arguments } =
                &function.semantic_identity
            else {
                return None;
            };
            let definition = function_base_definition(base)?;
            (definition.name() == base_name).then_some((definition, arguments))
        })
    }

    fn integer_arguments(arguments: &crate::CanonicalArguments) -> Vec<i128> {
        arguments
            .values
            .iter()
            .map(|value| match value {
                crate::CanonicalArgumentValue::Integer(value) => *value,
                value => panic!("expected integer specialization argument, got {value:?}"),
            })
            .collect()
    }

    #[test]
    fn specialization_identities_preserve_exact_generic_base_and_arguments() {
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
        let (output, _) = canonical(&source, &CompileOptions::default());
        let wraps = specialized_functions(&output, "wrap").collect::<Vec<_>>();
        let ids = specialized_functions(&output, "id").collect::<Vec<_>>();
        assert_eq!(wraps.len(), 1, "identical specialization deduplicates");
        assert_eq!(ids.len(), 2, "direct and later-fixpoint specializations");
        assert_eq!(integer_arguments(wraps[0].1), [1]);
        assert_eq!(
            ids.iter()
                .map(|(_, arguments)| integer_arguments(arguments))
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([vec![1], vec![2]])
        );
        assert!(wraps.iter().chain(ids.iter()).all(|(definition, _)| {
            definition.module().as_str() == "main.rue"
                && definition.kind() == crate::StableDefinitionKind::Function
        }));
        assert!(
            output
                .functions()
                .iter()
                .filter_map(|function| match &function.semantic_identity {
                    crate::FunctionInstanceKey::Definition(definition) => Some(definition),
                    _ => None,
                })
                .all(|definition| !matches!(definition.name(), "id" | "wrap")),
            "generic definitions are not runtime bodies"
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
        let (output, _) = canonical(&source, &CompileOptions::default());
        assert_eq!(
            specialized_functions(&output, "fib")
                .map(|(definition, arguments)| {
                    assert_eq!(definition.module().as_str(), "main.rue");
                    integer_arguments(arguments)
                })
                .collect::<std::collections::BTreeSet<_>>(),
            (0..=5).map(|value| vec![value]).collect(),
            "fib(0) through fib(5) must each have one production identity"
        );
    }

    #[test]
    fn sibling_same_name_specializations_retain_distinct_base_modules() {
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
        let (output, _) = canonical(&source, &CompileOptions::default());
        let instances = specialized_functions(&output, "id").collect::<Vec<_>>();
        assert_eq!(instances.len(), 2);
        assert_eq!(
            instances
                .iter()
                .map(|(definition, _)| definition.module().as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["left.rue", "right.rue"])
        );
        assert_eq!(
            instances
                .iter()
                .map(|(_, arguments)| integer_arguments(arguments))
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([vec![1], vec![2]])
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
        let (canonical, canonical_rir) = canonical(&source, &options);
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

    fn irrelevant_declarations(count: usize) -> CanonicalSemanticWork {
        let mut source = String::from("fn main() -> i32 { 42 }");
        for index in 0..count {
            source.push_str(&format!(" fn irrelevant{index}() -> i32 {{ {index} }}"));
        }
        let snapshot = snapshot(&[(1, "/main.rue", "main.rue", &source)], 1);
        canonical(&snapshot, &CompileOptions::default()).0.work()
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

    fn named_method_capture_with_irrelevant_declarations(
        count: usize,
    ) -> (Arc<CanonicalSemanticOutput>, Arc<CanonicalRirOutput>) {
        let mut source = String::from(
            "fn helper() -> i32 { 1 } struct Value { fn run(borrow self) -> i32 { helper() } } fn main() -> i32 { let value = Value {}; value.run() }",
        );
        for index in 0..count {
            source.push_str(&format!(" fn irrelevant{index}() -> i32 {{ {index} }}"));
        }
        let snapshot = snapshot(&[(1, "/main.rue", "main.rue", &source)], 1);
        canonical(&snapshot, &CompileOptions::default())
    }

    #[test]
    fn named_method_capture_is_unchanged_by_irrelevant_declarations() {
        let (one, one_rir) = named_method_capture_with_irrelevant_declarations(1);
        let (many, many_rir) = named_method_capture_with_irrelevant_declarations(128);
        assert_eq!(
            function_fingerprint(one.functions(), one_rir.semantic_symbols().interner()),
            function_fingerprint(many.functions(), many_rir.semantic_symbols().interner())
        );

        for output in [&one, &many] {
            let method = output
                .functions()
                .iter()
                .find(|function| {
                    matches!(
                        &function.semantic_identity,
                        crate::FunctionInstanceKey::Definition(definition)
                            if definition.kind() == crate::StableDefinitionKind::Method
                                && definition.name() == "run"
                                && definition.owner().is_some_and(|owner| owner.name() == "Value")
                    )
                })
                .expect("reachable named method must be a production body");
            let references = output
                .body_references(&method.semantic_identity)
                .expect("production body must retain its exact references");
            assert!(references.0.iter().any(|reference| matches!(
                reference,
                crate::body_query::BodyReference::Callable(
                    crate::FunctionInstanceKey::Definition(definition)
                ) if definition.name() == "helper"
            )));
        }
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
        let (output, _) = canonical(&source, &CompileOptions::default());
        assert_eq!(
            output
                .functions()
                .iter()
                .filter(|function| matches!(
                    &function.semantic_identity,
                    crate::FunctionInstanceKey::Definition(definition)
                        if definition.kind() == crate::StableDefinitionKind::Method
                            && definition.name() == "choose"
                            && definition.owner().is_some_and(|owner| owner.name() == "Value")
                ))
                .count(),
            1,
            "{:#?}",
            output
                .functions()
                .iter()
                .map(|function| &function.semantic_identity)
                .collect::<Vec<_>>()
        );
        assert!(
            output.functions().iter().all(|function| !matches!(
                &function.semantic_identity,
                crate::FunctionInstanceKey::Specialization { base, .. }
                    if function_base_definition(base).is_some_and(|definition| {
                        definition.kind() == crate::StableDefinitionKind::Method
                            && definition.name() == "choose"
                    })
            )),
            "comptime named-method arguments do not create runtime bodies"
        );
        let method = output
            .functions()
            .iter()
            .find(|function| {
                function_base_definition(&function.semantic_identity).is_some_and(|definition| {
                    definition.kind() == crate::StableDefinitionKind::Method
                        && definition.name() == "choose"
                })
            })
            .unwrap();
        let method_references = output.body_references(&method.semantic_identity).unwrap();
        assert!(
            method_references.0.iter().any(|reference| matches!(
                reference,
                crate::body_query::BodyReference::Callable(identity)
                    if function_base_definition(identity)
                        .is_some_and(|definition| definition.name() == "helper")
            )),
            "the retained named-method body must close over its transitive helper"
        );
        assert!(
            output.functions().iter().any(|function| {
                matches!(
                    &function.semantic_identity,
                    crate::FunctionInstanceKey::Definition(definition)
                        if definition.kind() == crate::StableDefinitionKind::Function
                            && definition.name() == "helper"
                )
            }),
            "the transitive helper must be composed into production output"
        );
        let main = output
            .functions()
            .iter()
            .find(|function| {
                function_base_definition(&function.semantic_identity)
                    .is_some_and(|definition| definition.name() == "main")
            })
            .unwrap();
        let references = output.body_references(&main.semantic_identity).unwrap();
        assert_eq!(
            references
                .0
                .iter()
                .filter(|reference| matches!(
                    reference,
                    crate::body_query::BodyReference::Callable(identity)
                        if function_base_definition(identity).is_some_and(|definition| {
                            definition.kind() == crate::StableDefinitionKind::Method
                                && definition.name() == "choose"
                        })
                ))
                .count(),
            1,
            "both calls share the named method's one exact body reference"
        );
    }

    #[test]
    fn codegen_input_tracks_root_paths_and_options_but_not_linker() {
        let sources = [
            // This fixture asserts on declared physical paths, so it must stay
            // import-free: an import-bearing fixture is republished by a
            // discovery epoch, which normalizes physical paths and would erase
            // the /old vs /new distinction the relocation case rests on.
            (1, "/old/main.rue", "main.rue", "fn main() -> i32 { 42 }"),
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
        let (base, _) = canonical(&base_snapshot, &base_options);

        let mut linker = base_options.clone();
        linker.linker = crate::LinkerMode::System("clang".to_owned());
        let (linker, _) = canonical(&base_snapshot, &linker);
        assert_eq!(base.input(), linker.input());

        let mut optimized = base_options.clone();
        optimized.opt_level = crate::OptLevel::O1;
        let (optimized, _) = canonical(&base_snapshot, &optimized);
        assert_ne!(base.input(), optimized.input());

        let relocated = snapshot(
            &[
                (1, "/new/main.rue", "main.rue", sources[0].3),
                (2, "/new/helper.rue", "helper.rue", sources[1].3),
            ],
            1,
        );
        let (relocated, _) = canonical(&relocated, &base_options);
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
        let (first_root, _) = canonical(&snapshot(&roots, 1), &base_options);
        let (second_root, _) = canonical(&snapshot(&roots, 2), &base_options);
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
        let (o0, _) = canonical(&source, &o0_options);
        let mut o1_options = o0_options.clone();
        o1_options.opt_level = crate::OptLevel::O1;
        let (o1, _) = canonical(&source, &o1_options);

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
