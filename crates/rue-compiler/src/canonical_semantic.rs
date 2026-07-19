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

pub(crate) struct PreparedDurableBodyCandidate {
    pub owner: crate::StableDefinitionKey,
    pub body_span: rue_span::Span,
    pub body: rue_air::SemanticBody<crate::StableDefinitionKey, Arc<str>>,
}

pub(crate) struct PreparedDurableSpecializedBodyCandidate {
    pub identity: rue_air::SemanticSpecializationIdentity<crate::StableDefinitionKey, Arc<str>>,
    pub body_span: rue_span::Span,
    pub body: rue_air::SemanticBody<crate::StableDefinitionKey, Arc<str>>,
    pub dependencies: Arc<[crate::StableDefinitionKey]>,
    pub dependency_boundary_complete: bool,
}

#[derive(Debug, Clone)]
struct CanonicalAnonymousNominalAssociation {
    representative: crate::AnonymousNominalKey,
    aliases: Arc<[crate::AnonymousNominalKey]>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalDeclarationFallbackReason {
    SchemaVersionMismatch,
    UnsupportedExport(crate::DurableSemanticExportFailure),
    Projection(crate::DurableSemanticProjectionFailure),
    Import(rue_air::DeclarationInstallFailure),
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
    pub last_fallback_reason: Option<CanonicalDeclarationFallbackReason>,
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
            let preparation_error = crate::CompileErrors::from(preparation_error);
            let bound = shells.resolve_declarations_with_work().map_err(|failure| {
                declaration_resolution_failure(failure, declaration_index, false, declaration_reuse)
            })?;
            return Err(recover_declaration_failure(
                bound,
                preparation_error,
                declaration_index,
                false,
                declaration_reuse,
                BodyOwnerTokenWork::default(),
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
}

#[cfg(test)]
pub(crate) fn query_owned_declaration_shells_for_test<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    preview_features: rue_error::PreviewFeatures,
    target: crate::Target,
    imports: &CanonicalImportGraph,
) -> MultiErrorResult<rue_air::DeclarationShells<'a>> {
    let options = CompileOptions {
        preview_features,
        target,
        ..CompileOptions::default()
    };
    let query_shells =
        crate::revisioned_query_database::projected_declaration_shells_for_test(merged)?;
    let prepared = prepare_canonical_declarations(merged, rir, &options, imports, &query_shells)
        .map_err(|failure| failure.errors)?;
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
    query_owned_declaration_shells_for_test(merged, rir, preview_features, target, imports)?
        .resolve_declarations()
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

fn declaration_resolution_failure(
    failure: rue_air::DeclarationResolutionFailure,
    declaration_index: RirDeclarationIndexWork,
    stable_ids_requested: bool,
    declaration_reuse: CanonicalDeclarationReuseWork,
) -> CanonicalSemanticFailure {
    let binding = failure.work();
    CanonicalSemanticFailure::declaration(
        failure.into_errors(),
        declaration_stage_work(
            declaration_index,
            binding,
            SemanticBindingManifestWork::default(),
            BodyOwnerTokenWork::default(),
            BodyAnalysisWork::default(),
            stable_ids_requested,
            declaration_reuse,
        ),
    )
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
    anonymous_nominal_associations: Arc<[CanonicalAnonymousNominalAssociation]>,
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
        let anonymous_nominal_associations = self
            .anonymous_nominal_associations
            .iter()
            .map(|association| (&association.representative, &association.aliases))
            .collect::<Vec<_>>();
        record!(
            "anonymous_nominal_associations",
            anonymous_nominal_associations
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
    /// Structural work performed by this request.
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
    let prepared = prepare_canonical_declarations(merged, rir, options, imports, &query_shells)
        .map_err(|failure| failure.errors)?;
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
        Vec::new(),
        Vec::new(),
        Arc::from([]),
        crate::DurableBodyWork::default(),
        info_span!("sema").entered(),
    )
    .map_err(|failure| failure.errors)
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
    reuse_plan: CanonicalDeclarationReuseWork,
    body_candidates: Vec<PreparedDurableBodyCandidate>,
    specialized_body_candidates: Vec<PreparedDurableSpecializedBodyCandidate>,
    durable_cfg_candidates: Arc<[crate::queries::DurableCfgArtifact]>,
    body_work: crate::DurableBodyWork,
) -> Result<CanonicalOrdinaryAnalysis, CanonicalSemanticFailure> {
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
    let declaration_reuse = CanonicalDeclarationReuseWork {
        semantic_epochs_started: 1,
        declaration_indexes_built: declaration_index.build_invocations,
        shell_predeclaration_epochs: 1,
        ..reuse_plan
    };
    let bound = shells.resolve_declarations_with_work().map_err(|failure| {
        declaration_resolution_failure(failure, declaration_index, false, declaration_reuse)
    })?;

    let declaration_exports = bound
        .with_declaration_semantics_from_shells(&shell_records, |records, _work| records.to_vec())
        .map_err(crate::DurableSemanticExportFailure::from);

    let mut output = finish_canonical_analysis(
        input,
        merged,
        rir,
        options,
        false,
        declaration_index,
        bound,
        definitions.clone(),
        declaration_reuse,
        body_candidates,
        specialized_body_candidates,
        durable_cfg_candidates,
        body_work,
        sema_span,
    )?;
    // Final analysis has now issued the authoritative post-classification
    // definition universe. Join the already-owned AIR exports to that exact
    // universe without a second definition-issuance pass or RIR traversal.
    let durable_declarations = declaration_exports.and_then(|records| {
        crate::durable_semantics::convert_declaration_semantics(
            merged,
            &output.body_owner_issuer,
            &records,
        )
    });
    let durable_declarations = match durable_declarations {
        Ok(values) => Some(values),
        Err(reason) => {
            output.work.declaration_reuse.unsupported_export_fallbacks += 1;
            output.work.declaration_reuse.last_fallback_reason = Some(
                CanonicalDeclarationFallbackReason::UnsupportedExport(reason),
            );
            None
        }
    };
    output
        .work
        .declaration_reuse
        .durable_cache_population_exports = usize::from(durable_declarations.is_some());
    let durable_definitions = output.body_owner_issuer.clone();
    Ok(CanonicalOrdinaryAnalysis {
        output,
        definitions: durable_definitions,
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
    imports: &CanonicalImportGraph,
    prepared: CanonicalPreparedDeclarations<'_>,
    definitions: &BoundDefinitionSet,
    durable: &[DurableDeclarationSemantic],
    body_candidates: Vec<PreparedDurableBodyCandidate>,
    specialized_body_candidates: Vec<PreparedDurableSpecializedBodyCandidate>,
    durable_cfg_candidates: Arc<[crate::queries::DurableCfgArtifact]>,
    body_work: crate::DurableBodyWork,
) -> Result<CanonicalSemanticOutput, CanonicalSemanticFailure> {
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
        Err(reason) => {
            reuse.fallbacks = 1;
            match reason {
                crate::DurableSemanticProjectionFailure::UnsupportedDeclaration => {
                    reuse.structural_validation_fallbacks = 1;
                }
                _ => reuse.stable_join_fallbacks = 1,
            }
            reuse.last_fallback_reason =
                Some(CanonicalDeclarationFallbackReason::Projection(reason));
            // Projection is read-only, so ordinary resolution consumes the
            // exact same unmutated shells and does not create a hidden epoch.
            shells.resolve_declarations_with_work().map_err(|failure| {
                declaration_resolution_failure(failure, declaration_index, false, reuse)
            })?
        }
        Ok((projected, _)) => {
            reuse.install_invocations += 1;
            match shells.install_declaration_semantics(&projected) {
                Ok(bound) => {
                    reuse.durable_records_reused = durable.len();
                    reuse.ordinary_declaration_resolutions_skipped = 1;
                    bound
                }
                Err(reason) => {
                    // Installation consumes potentially mutated shells. Only
                    // this failure requires a wholly fresh ordinary epoch.
                    reuse.fallbacks = 1;
                    match reason {
                        rue_air::DeclarationInstallFailure::UnsupportedType
                        | rue_air::DeclarationInstallFailure::UnsupportedDeclaration => {
                            reuse.unsupported_import_fallbacks = 1;
                        }
                        _ => reuse.structural_validation_fallbacks = 1,
                    }
                    reuse.last_fallback_reason =
                        Some(CanonicalDeclarationFallbackReason::Import(reason));
                    let fallback = configure_timed_canonical_sema(merged, rir, options, imports)
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
                                    reuse,
                                ),
                            )
                        })?;
                    reuse.fallback_epochs_started = 1;
                    reuse.semantic_epochs_started += 1;
                    reuse.declaration_indexes_built +=
                        fallback.rir_declaration_index_work().build_invocations;
                    reuse.shell_predeclaration_epochs += 1;
                    let fallback_shells = fallback
                        .predeclare_imported_declaration_shells(&shell_records)
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
                                    reuse,
                                ),
                            )
                        })?;
                    let fallback_records = fallback_shells
                        .declaration_shells()
                        .cloned()
                        .collect::<Vec<_>>();
                    selected_definitions =
                        issue_shell_definitions(merged, rir.source_revision(), &fallback_records)
                            .map_err(crate::CompileErrors::from)
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
                                        reuse,
                                    ),
                                )
                            })?;
                    let fallback_shells = fallback_shells
                        .install_stable_identity_endpoints(
                            &selected_definitions.semantic_definition_endpoints(),
                            &selected_definitions.semantic_module_endpoints(merged),
                        )
                        .map_err(|failure| {
                            CanonicalSemanticFailure::declaration(
                                crate::CompileErrors::from(crate::CompileError::without_span(
                                    rue_error::ErrorKind::InternalError(format!(
                                        "failed to install fallback stable identity endpoints: {failure:?}"
                                    )),
                                )),
                                CanonicalSemanticWork::default(),
                            )
                        })?;
                    fallback_shells
                        .resolve_declarations_with_work()
                        .map_err(|failure| {
                            declaration_resolution_failure(failure, declaration_index, false, reuse)
                        })?
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
        body_candidates,
        specialized_body_candidates,
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
    durable_cfg_candidates: Arc<[crate::queries::DurableCfgArtifact]>,
    mut durable_body_reuse_work: crate::DurableBodyWork,
    sema_span: tracing::span::EnteredSpan,
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
    let air_candidates = durable_body_candidates
        .into_iter()
        .filter_map(|candidate| {
            let owner = token_by_key.get(&candidate.owner).copied()?;
            Some(rue_air::SemanticBodyCandidate {
                owner,
                body_span: candidate.body_span,
                body: candidate.body,
            })
        })
        .collect();
    let bound = bound.install_ordinary_body_candidates(
        air_candidates,
        |key: &crate::StableDefinitionKey| authoritative_definitions.semantic_token_for_key(key),
        |path: &Arc<str>| {
            let module = merged
                .ast()
                .modules()
                .iter()
                .find(|module| module.module_id().as_str() == path.as_ref())
                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)?;
            authoritative_definitions.module_token_for(merged, module.module_id())
        },
    );
    let specialized_air_candidates = durable_specialized_body_candidates
        .into_iter()
        .map(|candidate| rue_air::SemanticSpecializedBodyCandidate {
            identity: candidate.identity,
            body_span: candidate.body_span,
            body: candidate.body,
            dependencies: candidate.dependencies,
            dependency_boundary_complete: candidate.dependency_boundary_complete,
        })
        .collect();
    let (bound, specialized_install_work) = bound.install_specialized_body_candidates(
        specialized_air_candidates,
        |key: &crate::StableDefinitionKey| authoritative_definitions.semantic_token_for_key(key),
        |path: &Arc<str>| {
            let module = merged
                .ast()
                .modules()
                .iter()
                .find(|module| module.module_id().as_str() == path.as_ref())
                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)?;
            authoritative_definitions.module_token_for(merged, module.module_id())
        },
    );
    durable_body_reuse_work.specialized_mapping_attempts += specialized_install_work.attempts;
    durable_body_reuse_work.specialized_mapping_successes += specialized_install_work.successes;
    durable_body_reuse_work.specialized_mapping_failures +=
        specialized_install_work.mapping_failures;
    durable_body_reuse_work.candidate_fallbacks += specialized_install_work.mapping_failures;
    let sema_output = match bound.analyze_all_bodies_with_work() {
        Ok(output) => output,
        Err(failure) => {
            let mut failed_durable_body_work = durable_body_reuse_work;
            failed_durable_body_work.reused_bodies += failure.work().ordinary_bodies_reused;
            failed_durable_body_work.skipped_body_analyses +=
                failure.work().ordinary_body_analyses_skipped;
            fold_body_import_work(&mut failed_durable_body_work, failure.work());
            let work = CanonicalSemanticWork {
                declaration_index,
                binding,
                manifest: manifest_work,
                bound_definitions: bound_definitions.as_ref().map(BoundDefinitionSet::work),
                body_owner_tokens,
                body_analysis: failure.work(),
                durable_bodies: failed_durable_body_work,
                cfg: CfgConstructionWork::default(),
                stable_ids_requested: request_stable_ids,
                declaration_reuse,
            };
            return Err(CanonicalSemanticFailure::new(
                CanonicalSemanticFailurePhase::BodyAnalysis,
                failure.into_errors(),
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
        .map(|identities| {
            let representative = project_anonymous_nominal_key(
                &identities.representative,
                merged,
                &authoritative_definitions,
            )?;
            let aliases = identities
                .aliases
                .iter()
                .map(|key| project_anonymous_nominal_key(key, merged, &authoritative_definitions))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CanonicalAnonymousNominalAssociation {
                representative,
                aliases: aliases.into(),
            })
        })
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
    anonymous_nominal_associations.sort_by(|left, right| {
        left.representative
            .cmp(&right.representative)
            .then_with(|| left.aliases.cmp(&right.aliases))
    });
    for association in &anonymous_nominal_associations {
        debug_assert_eq!(
            association.aliases.first(),
            Some(&association.representative),
            "AIR must expose the stable-min representative first"
        );
    }
    let anonymous_nominal_associations: Arc<[CanonicalAnonymousNominalAssociation]> =
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
        let selected = if let Some(token) = function.ordinary_owner {
            authoritative_definitions
                .key_for_body_token(token)
                .ok()
                .and_then(|key| {
                    durable_ordinary_body_payloads
                        .iter()
                        .find(|payload| &payload.owner == key)
                        .and_then(|payload| {
                            authoritative_definitions
                                .definition_by_key(key)
                                .and_then(|record| record.body_span())
                                .map(|span| {
                                    (
                                        span,
                                        payload.clone(),
                                        crate::FunctionInstanceKey::Definition(key.clone()),
                                    )
                                })
                        })
                })
        } else if let Some(rue_air::ImplicitDropDependencySourceEvent::Specialization {
            identity,
        }) = &function.implicit_drop_source
        {
            crate::durable_body::convert_specialization_identity(
                identity,
                merged,
                &authoritative_definitions,
                &mut durable_body_work,
            )
            .ok()
            .and_then(|identity| {
                durable_specialized_body_payloads
                    .iter()
                    .find(|payload| payload.identity == identity)
                    .and_then(|payload| {
                        authoritative_definitions
                            .definition_by_key(&identity.base)
                            .and_then(|record| {
                                crate::semantic_identity::function_instance_from_specialization(
                                    &identity,
                                )
                                .map(|function| {
                                    (record.declaration_span(), payload.body.clone(), function)
                                })
                            })
                    })
            })
        } else {
            None
        };
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
        durable_cfgs: cfg.durable_cfgs,
    })
}

/// Preserve ordinary source diagnostics when strict token preparation rejects
/// an already-bound epoch. The semantic result is consumed and discarded: it
/// can recover diagnostics, but can never publish a partially prepared output.
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
    let (errors, body_analysis) = match bound.analyze_all_bodies_with_work() {
        Err(failure) => {
            let work = failure.work();
            (failure.into_errors(), work)
        }
        Ok(output) => (preparation_error, output.body_analysis_work),
    };
    CanonicalSemanticFailure::declaration(
        errors,
        declaration_stage_work(
            declaration_index,
            binding,
            manifest,
            body_owner_tokens,
            body_analysis,
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
        let imports = crate::bound_definitions::test_fixture_import_graph(&merged).unwrap();

        let ordinary = match rue_air::Sema::new_synthetic(
            rir.rir(),
            rir.semantic_symbols().interner(),
            options.preview_features.clone(),
        )
        .bind_declarations()
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
            "struct Value {} drop fn Value(self) {} drop fn Value(self) {} fn main() {}",
            "drop fn Missing(self) {} fn main() {}",
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
        let imports = crate::bound_definitions::test_fixture_import_graph(&merged).unwrap();
        crate::bound_definitions::configure_canonical_sema(
            &merged,
            &rir,
            PreviewFeatures::new(),
            Target::default(),
            &imports,
        )
        .unwrap()
        .predeclare_declaration_shells()
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
        let imports = crate::bound_definitions::test_fixture_import_graph(&merged).unwrap();
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
