//! In-process canonical parse, merge, and RIR query orchestration.

use std::sync::Arc;

use rue_air::{DeclarationBindingWork, SemanticBindingManifestWork};

use crate::{
    BoundDefinitionSet, BoundDefinitionWork, CanonicalImportGraph, CanonicalImportGraphValidation,
    CanonicalImportResolution, CanonicalMergeWork, CanonicalMergedProgram, CanonicalParseSession,
    CanonicalRirOutput, CanonicalRirWork, CanonicalSemanticOutput, CanonicalSemanticWork,
    CodegenInputDescriptor, CompileError, CompileErrors, CompileOptions, CompileWarning, ErrorKind,
    ModuleResolutionInputs, ParseInvalidationSummary, ParsedModulesWork, SemanticInputDescriptor,
    SourceRevision, SourceSnapshot, StableDefinitionKey, StableDefinitionKind,
    StableDefinitionNamespace, analyze_canonical_program,
    bound_definitions::bind_canonical_definitions_with_work,
    canonical_merge::merge_parsed_modules_reusing_definitions, lower_canonical_rir,
    parsed_modules::ParsedProgram, resolve_canonical_import_graph, validate_canonical_import_graph,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrontendQueryWork {
    pub calls: usize,
    pub executions: usize,
    pub reuses: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CanonicalFrontendSessionWork {
    pub updates: usize,
    pub last_parse: ParsedModulesWork,
    pub last_invalidation: ParseInvalidationSummary,
    pub imports: FrontendQueryWork,
    pub import_entries: usize,
    pub import_entries_invalidated: usize,
    pub merge: FrontendQueryWork,
    pub rir: FrontendQueryWork,
    pub downstream_invalidations: usize,
    pub last_merge: CanonicalMergeWork,
    pub last_rir: CanonicalRirWork,
    pub semantic: FrontendQueryWork,
    pub semantic_entries: usize,
    pub semantic_entries_invalidated: usize,
    pub semantic_records: Vec<SemanticQueryRecord>,
    pub definitions: FrontendQueryWork,
    pub definition_entries: usize,
    pub definition_entries_invalidated: usize,
    pub definition_records: Vec<DefinitionQueryRecord>,
    pub diagnostic_publications: usize,
    pub diagnostic_reuses: usize,
    pub diagnostic_invalidations: usize,
    pub dependency_manifests: FrontendQueryWork,
    pub dependency_manifest_records_visited: usize,
    pub dependency_manifest_import_records_visited: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticDependencyManifestWork {
    pub definition_records_visited: usize,
    pub import_records_visited: usize,
    pub free_function_events_translated: usize,
    pub specialization_origins_validated: usize,
    pub named_method_events_translated: usize,
    pub named_destructor_events_translated: usize,
    pub declaration_type_events_translated: usize,
    pub declaration_type_call_head_events_translated: usize,
    pub builtin_type_call_head_inputs_translated: usize,
    pub named_const_events_translated: usize,
    pub extra_rir_instructions_visited: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableFreeFunctionDependency {
    pub caller: StableDefinitionKey,
    pub callee: StableDefinitionKey,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableNamedMethodDependencyTarget {
    FreeFunction(StableDefinitionKey),
    NamedMethod(StableDefinitionKey),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedMethodDependency {
    pub caller: StableDefinitionKey,
    pub target: StableNamedMethodDependencyTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedDestructorDependency {
    pub caller: StableDefinitionKey,
    pub target: StableNamedMethodDependencyTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDeclarationTypeDependency {
    pub source: StableDefinitionKey,
    pub target: StableDefinitionKey,
    pub kind: rue_air::DeclarationTypeDependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDeclarationTypeCallHeadDependency {
    pub source: StableDefinitionKey,
    pub callable: StableDefinitionKey,
    pub kind: rue_air::DeclarationTypeDependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableBuiltinTypeCallHeadInput {
    pub source: StableDefinitionKey,
    pub builtin: rue_air::BuiltinTypeCallHead,
    pub kind: rue_air::DeclarationTypeDependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableNamedConstDependencyTarget {
    ValueConst(StableDefinitionKey),
    FreeFunction(StableDefinitionKey),
    NamedType(StableDefinitionKey),
    ModuleBinding(StableDefinitionKey),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedConstDependency {
    pub source: StableDefinitionKey,
    pub target: StableNamedConstDependencyTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableModuleImportDependency {
    Resolved {
        importer: crate::ModuleId,
        normalized_specifier: Arc<str>,
        target: crate::ModuleId,
    },
    Missing {
        importer: crate::ModuleId,
        normalized_specifier: Arc<str>,
    },
    Ambiguous {
        importer: crate::ModuleId,
        normalized_specifier: Arc<str>,
        file_module: crate::ModuleId,
        directory_module: crate::ModuleId,
    },
}

#[derive(Debug, Clone)]
pub struct SemanticDependencyInputManifest {
    input: SemanticInputDescriptor,
    imports: CanonicalImportGraph,
    definitions: Arc<[StableDefinitionKey]>,
    module_imports: Arc<[StableModuleImportDependency]>,
    free_function_dependencies: Arc<[StableFreeFunctionDependency]>,
    free_function_caller_dependencies_complete: bool,
    named_method_dependencies: Arc<[StableNamedMethodDependency]>,
    non_generic_named_method_dependencies_complete: bool,
    generic_named_method_dependencies_complete: bool,
    named_destructor_dependencies: Arc<[StableNamedDestructorDependency]>,
    named_destructor_dependencies_complete: bool,
    declaration_type_dependencies: Arc<[StableDeclarationTypeDependency]>,
    declaration_type_dependencies_complete: bool,
    declaration_type_call_head_dependencies: Arc<[StableDeclarationTypeCallHeadDependency]>,
    declaration_type_call_head_dependencies_complete: bool,
    builtin_type_call_head_inputs: Arc<[StableBuiltinTypeCallHeadInput]>,
    supported_type_call_heads_complete: bool,
    named_const_dependencies: Arc<[StableNamedConstDependency]>,
    named_value_const_dependencies_complete: bool,
    semantic_dependency_graph_complete: bool,
    definition_universe_complete: bool,
    work: SemanticDependencyManifestWork,
}

impl SemanticDependencyInputManifest {
    pub fn input(&self) -> &SemanticInputDescriptor {
        &self.input
    }
    pub fn imports(&self) -> &CanonicalImportGraph {
        &self.imports
    }
    pub fn definitions(&self) -> &[StableDefinitionKey] {
        &self.definitions
    }
    pub fn module_imports(&self) -> &[StableModuleImportDependency] {
        &self.module_imports
    }
    pub fn free_function_dependencies(&self) -> &[StableFreeFunctionDependency] {
        &self.free_function_dependencies
    }
    pub fn free_function_caller_dependencies_complete(&self) -> bool {
        self.free_function_caller_dependencies_complete
    }
    pub fn named_method_dependencies(&self) -> &[StableNamedMethodDependency] {
        &self.named_method_dependencies
    }
    pub fn non_generic_named_method_dependencies_complete(&self) -> bool {
        self.non_generic_named_method_dependencies_complete
    }
    pub fn generic_named_method_dependencies_complete(&self) -> bool {
        self.generic_named_method_dependencies_complete
    }
    pub fn named_destructor_dependencies(&self) -> &[StableNamedDestructorDependency] {
        &self.named_destructor_dependencies
    }
    pub fn named_destructor_dependencies_complete(&self) -> bool {
        self.named_destructor_dependencies_complete
    }
    pub fn declaration_type_dependencies(&self) -> &[StableDeclarationTypeDependency] {
        &self.declaration_type_dependencies
    }
    pub fn declaration_type_dependencies_complete(&self) -> bool {
        self.declaration_type_dependencies_complete
    }
    pub fn declaration_type_call_head_dependencies(
        &self,
    ) -> &[StableDeclarationTypeCallHeadDependency] {
        &self.declaration_type_call_head_dependencies
    }
    pub fn declaration_type_call_head_dependencies_complete(&self) -> bool {
        self.declaration_type_call_head_dependencies_complete
    }
    pub fn builtin_type_call_head_inputs(&self) -> &[StableBuiltinTypeCallHeadInput] {
        &self.builtin_type_call_head_inputs
    }
    pub fn supported_type_call_heads_complete(&self) -> bool {
        self.supported_type_call_heads_complete
    }
    pub fn named_const_dependencies(&self) -> &[StableNamedConstDependency] {
        &self.named_const_dependencies
    }
    pub fn named_value_const_dependencies_complete(&self) -> bool {
        self.named_value_const_dependencies_complete
    }
    pub fn semantic_dependency_graph_complete(&self) -> bool {
        self.semantic_dependency_graph_complete
    }
    pub fn definition_universe_complete(&self) -> bool {
        self.definition_universe_complete
    }
    pub fn work(&self) -> SemanticDependencyManifestWork {
        self.work
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FrontendDiagnosticStage {
    Syntax,
    Merge,
    Semantic(CodegenInputDescriptor),
}

#[derive(Debug, Clone)]
pub struct FrontendDiagnosticSnapshot {
    source: SourceSnapshot,
    stage: FrontendDiagnosticStage,
    errors: Arc<[CompileError]>,
    warnings: Arc<[CompileWarning]>,
}

impl FrontendDiagnosticSnapshot {
    pub fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    pub fn source_revision(&self) -> &SourceRevision {
        self.source.source_revision()
    }
    pub fn stage(&self) -> &FrontendDiagnosticStage {
        &self.stage
    }
    pub fn errors(&self) -> &[CompileError] {
        &self.errors
    }
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportGraphInputDescriptor {
    pub sources: SourceRevision,
    pub resolution: ModuleResolutionInputs,
    pub std_dir: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct CanonicalImportGraphOutput {
    input: ImportGraphInputDescriptor,
    graph: CanonicalImportGraph,
    validation: CanonicalImportGraphValidation,
}

impl CanonicalImportGraphOutput {
    pub fn input(&self) -> &ImportGraphInputDescriptor {
        &self.input
    }
    pub fn graph(&self) -> &CanonicalImportGraph {
        &self.graph
    }
    pub fn validation(&self) -> &CanonicalImportGraphValidation {
        &self.validation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticQueryRecord {
    pub input: CodegenInputDescriptor,
    pub work: CanonicalSemanticWork,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionQueryRecord {
    pub input: SemanticInputDescriptor,
    pub binding: DeclarationBindingWork,
    pub manifest: SemanticBindingManifestWork,
    pub issuance: BoundDefinitionWork,
    pub failed: bool,
}

#[derive(Debug)]
pub struct CanonicalFrontendUpdate {
    result: Result<Arc<ParsedProgram>, CompileErrors>,
    work: ParsedModulesWork,
    invalidation: ParseInvalidationSummary,
    downstream_invalidated: bool,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
}

impl CanonicalFrontendUpdate {
    pub fn result(&self) -> Result<&Arc<ParsedProgram>, &CompileErrors> {
        self.result.as_ref()
    }
    pub fn into_result(self) -> Result<Arc<ParsedProgram>, CompileErrors> {
        self.result
    }
    pub fn work(&self) -> ParsedModulesWork {
        self.work
    }
    pub fn invalidation(&self) -> &ParseInvalidationSummary {
        &self.invalidation
    }
    pub fn downstream_invalidated(&self) -> bool {
        self.downstream_invalidated
    }
    pub fn diagnostics(&self) -> &Arc<FrontendDiagnosticSnapshot> {
        &self.diagnostics
    }
}

#[derive(Debug, Default)]
pub struct CanonicalFrontendSession {
    parse: CanonicalParseSession,
    published: Option<Arc<ParsedProgram>>,
    published_snapshot: Option<SourceSnapshot>,
    merge_cache: Option<Result<Arc<CanonicalMergedProgram>, CompileErrors>>,
    definition_shard_baseline: Option<crate::DefinitionSnapshot>,
    rir_cache: Option<Arc<CanonicalRirOutput>>,
    import_cache: Vec<ImportCacheEntry>,
    semantic_cache: Vec<SemanticCacheEntry>,
    definition_cache: Vec<DefinitionCacheEntry>,
    work: CanonicalFrontendSessionWork,
    diagnostic_cache: Vec<Arc<FrontendDiagnosticSnapshot>>,
    latest_diagnostics: Option<Arc<FrontendDiagnosticSnapshot>>,
    dependency_manifest_cache: Vec<Arc<SemanticDependencyInputManifest>>,
}

#[derive(Debug)]
struct ImportCacheEntry {
    input: ImportGraphInputDescriptor,
    result: Result<Arc<CanonicalImportGraphOutput>, CompileErrors>,
}

#[derive(Debug)]
struct SemanticCacheEntry {
    input: CodegenInputDescriptor,
    result: Result<Arc<CanonicalSemanticOutput>, CompileErrors>,
}

#[derive(Debug)]
struct DefinitionCacheEntry {
    input: SemanticInputDescriptor,
    result: Result<Arc<BoundDefinitionSet>, CompileErrors>,
}

impl CanonicalFrontendSession {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn published(&self) -> Option<&Arc<ParsedProgram>> {
        self.published.as_ref()
    }
    pub fn work(&self) -> &CanonicalFrontendSessionWork {
        &self.work
    }
    pub fn latest_diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.latest_diagnostics.as_ref()
    }
    pub fn diagnostics_for(
        &self,
        source: &SourceSnapshot,
        stage: &FrontendDiagnosticStage,
    ) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.diagnostic_cache
            .iter()
            .rev()
            .find(|entry| same_attempt(entry.source(), source) && entry.stage() == stage)
    }

    fn publish_diagnostics(
        &mut self,
        source: &SourceSnapshot,
        stage: FrontendDiagnosticStage,
        errors: Option<&CompileErrors>,
        warnings: &[CompileWarning],
    ) -> Arc<FrontendDiagnosticSnapshot> {
        if let Some(existing) = self
            .diagnostic_cache
            .iter()
            .find(|entry| same_attempt(entry.source(), source) && entry.stage() == &stage)
        {
            self.work.diagnostic_reuses += 1;
            let existing = existing.clone();
            self.latest_diagnostics = Some(existing.clone());
            return existing;
        }
        if self.latest_diagnostics.is_some() {
            self.work.diagnostic_invalidations += 1;
        }
        let snapshot = Arc::new(FrontendDiagnosticSnapshot {
            source: source.clone(),
            stage,
            errors: errors
                .map(|errors| errors.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
                .into(),
            warnings: warnings.to_vec().into(),
        });
        self.work.diagnostic_publications += 1;
        self.diagnostic_cache.push(snapshot.clone());
        self.latest_diagnostics = Some(snapshot.clone());
        snapshot
    }

    pub fn update(&mut self, snapshot: &SourceSnapshot) -> CanonicalFrontendUpdate {
        self.work.updates += 1;
        let update = self.parse.update(snapshot);
        let parse_work = update.work();
        let invalidation = update.invalidation().clone();
        self.work.last_parse = parse_work;
        self.work.last_invalidation = invalidation.clone();
        let result = update.into_result();
        let diagnostics = self.publish_diagnostics(
            snapshot,
            FrontendDiagnosticStage::Syntax,
            result.as_ref().err(),
            &[],
        );
        match result {
            Ok(candidate) => {
                let exact = self.published.as_deref().is_some_and(|published| {
                    programs_are_pointer_equivalent(published, &candidate)
                });
                let downstream_invalidated = self.published.is_some() && !exact;
                if exact {
                    CanonicalFrontendUpdate {
                        result: Ok(self.published.as_ref().unwrap().clone()),
                        work: parse_work,
                        invalidation,
                        downstream_invalidated: false,
                        diagnostics,
                    }
                } else {
                    if downstream_invalidated {
                        self.work.downstream_invalidations += 1;
                    }
                    self.merge_cache = None;
                    self.rir_cache = None;
                    self.work.import_entries_invalidated += self.import_cache.len();
                    self.import_cache.clear();
                    self.work.import_entries = 0;
                    self.work.semantic_entries_invalidated += self.semantic_cache.len();
                    self.semantic_cache.clear();
                    self.work.definition_entries_invalidated += self.definition_cache.len();
                    self.definition_cache.clear();
                    self.work.last_merge = CanonicalMergeWork::default();
                    self.work.last_rir = CanonicalRirWork::default();
                    self.work.semantic_entries = 0;
                    self.work.semantic_records.clear();
                    self.work.definition_entries = 0;
                    self.work.definition_records.clear();
                    self.dependency_manifest_cache.clear();
                    self.published = Some(candidate.clone());
                    self.published_snapshot = Some(snapshot.clone());
                    CanonicalFrontendUpdate {
                        result: Ok(candidate),
                        work: parse_work,
                        invalidation,
                        downstream_invalidated,
                        diagnostics,
                    }
                }
            }
            Err(errors) => CanonicalFrontendUpdate {
                result: Err(errors),
                work: parse_work,
                invalidation,
                downstream_invalidated: false,
                diagnostics,
            },
        }
    }

    /// Resolve canonical parsed import sites without lowering or semantic work.
    pub fn import_graph(
        &mut self,
        std_dir: Option<&str>,
    ) -> Result<Arc<CanonicalImportGraphOutput>, CompileErrors> {
        self.work.imports.calls += 1;
        let parsed = self.published.as_deref().ok_or_else(no_published_program)?;
        let resolution = ModuleResolutionInputs::new(
            parsed.root().clone(),
            parsed
                .modules()
                .iter()
                .map(|module| crate::ModuleResolutionInput {
                    module: module.module_id().clone(),
                    physical_path: Arc::from(module.physical_path()),
                })
                .collect(),
        )
        .expect("published parsed modules have validated resolution inputs");
        let input = ImportGraphInputDescriptor {
            sources: parsed.source_revision().clone(),
            resolution,
            std_dir: std_dir.map(Arc::from),
        };
        if let Some(entry) = self.import_cache.iter().find(|entry| entry.input == input) {
            self.work.imports.reuses += 1;
            return entry.result.clone();
        }
        self.work.imports.executions += 1;
        let result =
            resolve_canonical_import_graph(parsed.import_directives(), &input.resolution, std_dir)
                .map(|graph| {
                    let validation = validate_canonical_import_graph(&graph, &input.resolution);
                    Arc::new(CanonicalImportGraphOutput {
                        input: input.clone(),
                        graph,
                        validation,
                    })
                })
                .map_err(CompileErrors::from);
        self.import_cache.push(ImportCacheEntry {
            input,
            result: result.clone(),
        });
        self.work.import_entries = self.import_cache.len();
        result
    }

    pub fn merge(&mut self) -> Result<Arc<CanonicalMergedProgram>, CompileErrors> {
        self.work.merge.calls += 1;
        if let Some(cached) = &self.merge_cache {
            self.work.merge.reuses += 1;
            let cached = cached.clone();
            let source = self
                .published_snapshot
                .clone()
                .expect("published program retains source snapshot");
            self.publish_diagnostics(
                &source,
                FrontendDiagnosticStage::Merge,
                cached.as_ref().err(),
                &[],
            );
            return cached;
        }
        let parsed = self.published.as_deref().ok_or_else(no_published_program)?;
        self.work.merge.executions += 1;
        let merged = merge_parsed_modules_reusing_definitions(
            parsed,
            self.definition_shard_baseline.as_ref(),
        )
        .map(Arc::new);
        if let Ok(merged) = &merged {
            debug_assert_eq!(merged.ast().source_revision(), parsed.source_revision());
            self.work.last_merge = merged.work();
            self.definition_shard_baseline = Some(merged.definitions().clone());
        }
        self.merge_cache = Some(merged.clone());
        let source = self
            .published_snapshot
            .clone()
            .expect("published program retains source snapshot");
        self.publish_diagnostics(
            &source,
            FrontendDiagnosticStage::Merge,
            merged.as_ref().err(),
            &[],
        );
        merged
    }

    pub fn rir(&mut self) -> Result<Arc<CanonicalRirOutput>, CompileErrors> {
        self.work.rir.calls += 1;
        if let Some(cached) = &self.rir_cache {
            self.work.rir.reuses += 1;
            return Ok(cached.clone());
        }
        let merged = self.merge()?;
        self.work.rir.executions += 1;
        let rir = Arc::new(lower_canonical_rir(&merged).map_err(CompileErrors::from)?);
        debug_assert_eq!(rir.source_revision(), merged.ast().source_revision());
        self.work.last_rir = rir.work();
        self.rir_cache = Some(rir.clone());
        Ok(rir)
    }

    /// Analyze the current published revision without issuing stable definition IDs.
    pub fn semantic(
        &mut self,
        options: &CompileOptions,
    ) -> Result<Arc<CanonicalSemanticOutput>, CompileErrors> {
        self.work.semantic.calls += 1;
        let rir = self.rir()?;
        let merged = match self.merge_cache.as_ref() {
            Some(Ok(merged)) => merged.clone(),
            Some(Err(errors)) => return Err(errors.clone()),
            None => unreachable!("successful RIR query retains its merge input"),
        };
        let input = CodegenInputDescriptor {
            semantic: SemanticInputDescriptor::new(
                merged.definitions().source_snapshot(),
                options.target,
                &options.preview_features,
            ),
            opt_level: options.opt_level.into(),
        };
        if let Some(entry) = self
            .semantic_cache
            .iter()
            .find(|entry| entry.input == input)
        {
            self.work.semantic.reuses += 1;
            let result = entry.result.clone();
            let source = self
                .published_snapshot
                .clone()
                .expect("semantic query retains source snapshot");
            self.publish_diagnostics(
                &source,
                FrontendDiagnosticStage::Semantic(input),
                result.as_ref().err(),
                result
                    .as_ref()
                    .map(|output| output.warnings())
                    .unwrap_or(&[]),
            );
            return result;
        }

        self.work.semantic.executions += 1;
        let result = analyze_canonical_program(&merged, &rir, options, false).map(Arc::new);
        let semantic_work = result
            .as_ref()
            .map(|output| output.work())
            .unwrap_or_default();
        if let Ok(output) = &result {
            debug_assert_eq!(output.input(), &input);
            debug_assert_eq!(semantic_work.binding.bind_invocations, 1);
            debug_assert_eq!(semantic_work.manifest.build_invocations, 0);
            debug_assert!(!semantic_work.stable_ids_requested);
        }
        self.semantic_cache.push(SemanticCacheEntry {
            input: input.clone(),
            result: result.clone(),
        });
        self.work.semantic_entries = self.semantic_cache.len();
        self.work.semantic_records.push(SemanticQueryRecord {
            input: input.clone(),
            work: semantic_work,
            failed: result.is_err(),
        });
        let source = self
            .published_snapshot
            .clone()
            .expect("semantic query retains source snapshot");
        self.publish_diagnostics(
            &source,
            FrontendDiagnosticStage::Semantic(input),
            result.as_ref().err(),
            result
                .as_ref()
                .map(|output| output.warnings())
                .unwrap_or(&[]),
        );
        result
    }

    /// Issue stable definition IDs on demand for the current semantic input.
    ///
    /// Ordinary analysis consumes `BoundSema` without building its optional
    /// manifest. Retaining that mutable, RIR-borrowing value would duplicate
    /// substantial semantic state, so this query performs one explicit second
    /// declaration bind after reusing a successful ordinary body analysis.
    pub fn stable_definitions(
        &mut self,
        options: &CompileOptions,
    ) -> Result<Arc<BoundDefinitionSet>, CompileErrors> {
        self.work.definitions.calls += 1;
        let rir = self.rir()?;
        let merged = match self.merge_cache.as_ref() {
            Some(Ok(merged)) => merged.clone(),
            Some(Err(errors)) => return Err(errors.clone()),
            None => unreachable!("successful RIR query retains merge input"),
        };
        let input = SemanticInputDescriptor::new(
            merged.definitions().source_snapshot(),
            options.target,
            &options.preview_features,
        );

        // Body validity is independent of opt/linker. Reuse any ordinary
        // semantic result with the same binding inputs before doing ID work.
        if let Some(validation) = self
            .semantic_cache
            .iter()
            .find(|entry| entry.input.semantic == input && entry.result.is_ok())
            .map(|entry| entry.result.clone())
        {
            validation?;
        } else {
            self.semantic(options)?;
        }

        if let Some(entry) = self
            .definition_cache
            .iter()
            .find(|entry| entry.input == input)
        {
            self.work.definitions.reuses += 1;
            return entry.result.clone();
        }
        self.work.definitions.executions += 1;
        let query = bind_canonical_definitions_with_work(
            &merged,
            &rir,
            options.preview_features.clone(),
            options.target,
        );
        let (result, binding, manifest, issuance) = match query {
            Ok((definitions, binding)) => {
                let manifest = definitions.manifest_work();
                let issuance = definitions.work();
                (Ok(Arc::new(definitions)), binding, manifest, issuance)
            }
            Err(errors) => (
                Err(errors),
                DeclarationBindingWork::default(),
                SemanticBindingManifestWork::default(),
                BoundDefinitionWork::default(),
            ),
        };
        self.definition_cache.push(DefinitionCacheEntry {
            input: input.clone(),
            result: result.clone(),
        });
        self.work.definition_entries = self.definition_cache.len();
        self.work.definition_records.push(DefinitionQueryRecord {
            input,
            binding,
            manifest,
            issuance,
            failed: result.is_err(),
        });
        result
    }

    /// Materialize stable semantic inputs for future dependency-edge capture.
    ///
    /// This tooling-only query shares the existing import and stable-definition
    /// queries. It performs no additional RIR traversal; reference edges are not
    /// yet claimed until every semantic reference surface can be captured.
    pub fn semantic_dependency_inputs(
        &mut self,
        options: &CompileOptions,
        std_dir: Option<&str>,
    ) -> Result<Arc<SemanticDependencyInputManifest>, CompileErrors> {
        self.work.dependency_manifests.calls += 1;
        let imports = self.import_graph(std_dir)?;
        let snapshot = self
            .published_snapshot
            .as_ref()
            .expect("stable definitions retain a published source snapshot");
        let input =
            SemanticInputDescriptor::new(snapshot, options.target, &options.preview_features);
        if let Some(cached) = self
            .dependency_manifest_cache
            .iter()
            .find(|manifest| manifest.input == input && manifest.imports == *imports.graph())
        {
            self.work.dependency_manifests.reuses += 1;
            return Ok(cached.clone());
        }
        self.work.dependency_manifests.executions += 1;
        let semantic = self.semantic(options);
        let definitions = self.stable_definitions(options);
        let definition_universe_complete = definitions.is_ok();
        let definition_records = definitions
            .as_ref()
            .map(|definitions| definitions.definitions())
            .unwrap_or(&[]);
        let mut keys = definition_records
            .iter()
            .map(|record| record.stable_key().clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        let (
            mut free_function_dependencies,
            mut named_method_dependencies,
            mut named_destructor_dependencies,
            mut declaration_type_dependencies,
            mut declaration_type_call_head_dependencies,
            mut builtin_type_call_head_inputs,
            mut named_const_dependencies,
            free_function_events_translated,
            specialization_origins_validated,
            named_method_events_translated,
            named_destructor_events_translated,
            declaration_type_events_translated,
            declaration_type_call_head_events_translated,
            builtin_type_call_head_inputs_translated,
            named_const_events_translated,
            free_function_caller_dependencies_complete,
            named_method_dependencies_complete,
            named_destructor_dependencies_complete,
            declaration_type_dependencies_complete,
            declaration_type_call_head_dependencies_complete,
            supported_type_call_heads_complete,
            named_value_const_dependencies_complete,
        ) = match (&semantic, &definitions) {
            (Ok(semantic), Ok(definitions)) => {
                if definitions.source_revision() != &input.sources {
                    return Err(invalid_dependency_manifest(
                        "semantic dependency translation used a foreign definition revision",
                    ));
                }
                let mut edges = Vec::new();
                for origin in semantic.specialized_free_function_origins() {
                    stable_free_function_endpoint(
                        definitions,
                        origin.base_file,
                        &origin.base_name,
                    )?;
                }
                for event in semantic.ordinary_free_function_dependencies() {
                    edges.push(StableFreeFunctionDependency {
                        caller: stable_free_function_endpoint(
                            definitions,
                            event.caller_file,
                            &event.caller_name,
                        )?,
                        callee: stable_free_function_endpoint(
                            definitions,
                            event.callee_file,
                            &event.callee_name,
                        )?,
                    });
                }
                for event in semantic.specialized_free_function_dependencies() {
                    edges.push(StableFreeFunctionDependency {
                        caller: stable_free_function_endpoint(
                            definitions,
                            event.base_file,
                            &event.base_name,
                        )?,
                        callee: stable_free_function_endpoint(
                            definitions,
                            event.callee_file,
                            &event.callee_name,
                        )?,
                    });
                }
                let mut method_edges = Vec::new();
                for event in semantic.named_method_dependencies() {
                    let caller = stable_named_method_endpoint(
                        definitions,
                        event.caller_file,
                        &event.caller_owner_name,
                        &event.caller_method_name,
                    )?;
                    let target = match &event.target {
                        rue_air::NamedMethodDependencyTargetEvent::FreeFunction { file, name } => {
                            StableNamedMethodDependencyTarget::FreeFunction(
                                stable_free_function_endpoint(definitions, *file, name)?,
                            )
                        }
                        rue_air::NamedMethodDependencyTargetEvent::NamedMethod {
                            file,
                            owner_name,
                            method_name,
                        } => StableNamedMethodDependencyTarget::NamedMethod(
                            stable_named_method_endpoint(
                                definitions,
                                *file,
                                owner_name,
                                method_name,
                            )?,
                        ),
                    };
                    method_edges.push(StableNamedMethodDependency { caller, target });
                }
                let mut destructor_edges = Vec::new();
                for event in semantic.named_destructor_dependencies() {
                    let caller = stable_named_destructor_endpoint(
                        definitions,
                        event.caller_file,
                        &event.caller_owner_name,
                    )?;
                    let target = match &event.target {
                        rue_air::NamedMethodDependencyTargetEvent::FreeFunction { file, name } => {
                            StableNamedMethodDependencyTarget::FreeFunction(
                                stable_free_function_endpoint(definitions, *file, name)?,
                            )
                        }
                        rue_air::NamedMethodDependencyTargetEvent::NamedMethod {
                            file,
                            owner_name,
                            method_name,
                        } => StableNamedMethodDependencyTarget::NamedMethod(
                            stable_named_method_endpoint(
                                definitions,
                                *file,
                                owner_name,
                                method_name,
                            )?,
                        ),
                    };
                    destructor_edges.push(StableNamedDestructorDependency { caller, target });
                }
                let mut type_edges = Vec::new();
                for event in semantic.declaration_type_dependencies() {
                    type_edges.push(StableDeclarationTypeDependency {
                        source: stable_declaration_source_endpoint(definitions, event)?,
                        target: stable_named_type_endpoint(definitions, event)?,
                        kind: event.dependency_kind,
                    });
                }
                let mut type_call_head_edges = Vec::new();
                for event in semantic.declaration_type_call_head_dependencies() {
                    type_call_head_edges.push(StableDeclarationTypeCallHeadDependency {
                        source: stable_declaration_type_source_endpoint(
                            definitions,
                            event.source_file,
                            &event.source_name,
                            event.source_owner_name.as_deref(),
                            event.source_kind,
                        )?,
                        callable: stable_free_function_endpoint(
                            definitions,
                            event.callable_file,
                            &event.callable_name,
                        )?,
                        kind: event.dependency_kind,
                    });
                }
                let mut builtin_head_inputs = Vec::new();
                for event in semantic.declaration_builtin_type_call_head_dependencies() {
                    builtin_head_inputs.push(StableBuiltinTypeCallHeadInput {
                        source: stable_declaration_type_source_endpoint(
                            definitions,
                            event.source_file,
                            &event.source_name,
                            event.source_owner_name.as_deref(),
                            event.source_kind,
                        )?,
                        builtin: event.builtin,
                        kind: event.dependency_kind,
                    });
                }
                let mut const_edges = Vec::new();
                for event in semantic.named_const_dependencies() {
                    let source = stable_top_level_endpoint(
                        definitions,
                        event.source_file,
                        &event.source_name,
                        StableDefinitionNamespace::Value,
                        StableDefinitionKind::ValueConst,
                    )?;
                    let target = match &event.target {
                        rue_air::NamedConstDependencyTargetEvent::ValueConst { file, name } => {
                            StableNamedConstDependencyTarget::ValueConst(stable_top_level_endpoint(
                                definitions,
                                *file,
                                name,
                                StableDefinitionNamespace::Value,
                                StableDefinitionKind::ValueConst,
                            )?)
                        }
                        rue_air::NamedConstDependencyTargetEvent::FreeFunction { file, name } => {
                            StableNamedConstDependencyTarget::FreeFunction(
                                stable_free_function_endpoint(definitions, *file, name)?,
                            )
                        }
                        rue_air::NamedConstDependencyTargetEvent::NamedType {
                            file,
                            name,
                            kind,
                        } => {
                            let kind = match kind {
                                rue_air::DeclarationTypeDependencyTargetKind::Struct => {
                                    StableDefinitionKind::Struct
                                }
                                rue_air::DeclarationTypeDependencyTargetKind::Enum => {
                                    StableDefinitionKind::Enum
                                }
                            };
                            StableNamedConstDependencyTarget::NamedType(stable_top_level_endpoint(
                                definitions,
                                *file,
                                name,
                                StableDefinitionNamespace::Type,
                                kind,
                            )?)
                        }
                        rue_air::NamedConstDependencyTargetEvent::ModuleBinding { file, name } => {
                            StableNamedConstDependencyTarget::ModuleBinding(
                                stable_top_level_endpoint(
                                    definitions,
                                    *file,
                                    name,
                                    StableDefinitionNamespace::Value,
                                    StableDefinitionKind::ModuleBinding,
                                )?,
                            )
                        }
                    };
                    const_edges.push(StableNamedConstDependency { source, target });
                }
                (
                    edges,
                    method_edges,
                    destructor_edges,
                    type_edges,
                    type_call_head_edges,
                    builtin_head_inputs,
                    const_edges,
                    semantic.ordinary_free_function_dependencies().len()
                        + semantic.specialized_free_function_dependencies().len(),
                    semantic.specialized_free_function_origins().len(),
                    semantic.named_method_dependencies().len(),
                    semantic.named_destructor_dependencies().len(),
                    semantic.declaration_type_dependencies().len(),
                    semantic.declaration_type_call_head_dependencies().len(),
                    semantic
                        .declaration_builtin_type_call_head_dependencies()
                        .len(),
                    semantic.named_const_dependencies().len(),
                    semantic.ordinary_free_function_dependencies_complete()
                        && semantic.specialized_free_function_dependencies_complete(),
                    semantic.non_generic_named_method_dependencies_complete(),
                    semantic.named_destructor_dependencies_complete(),
                    semantic.declaration_type_dependencies_complete(),
                    semantic.declaration_type_call_head_dependencies_complete(),
                    semantic.supported_type_call_heads_complete(),
                    semantic.named_value_const_dependencies_complete(),
                )
            }
            _ => (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
            ),
        };
        free_function_dependencies.sort();
        free_function_dependencies.dedup();
        named_method_dependencies.sort();
        named_method_dependencies.dedup();
        named_destructor_dependencies.sort();
        named_destructor_dependencies.dedup();
        declaration_type_dependencies.sort();
        declaration_type_dependencies.dedup();
        declaration_type_call_head_dependencies.sort();
        declaration_type_call_head_dependencies.dedup();
        builtin_type_call_head_inputs.sort();
        builtin_type_call_head_inputs.dedup();
        named_const_dependencies.sort();
        named_const_dependencies.dedup();
        let work = SemanticDependencyManifestWork {
            definition_records_visited: definition_records.len(),
            import_records_visited: imports.graph().records().len(),
            free_function_events_translated,
            specialization_origins_validated,
            named_method_events_translated,
            named_destructor_events_translated,
            declaration_type_events_translated,
            declaration_type_call_head_events_translated,
            builtin_type_call_head_inputs_translated,
            named_const_events_translated,
            extra_rir_instructions_visited: 0,
        };
        self.work.dependency_manifest_records_visited += work.definition_records_visited;
        self.work.dependency_manifest_import_records_visited += work.import_records_visited;
        let module_imports = imports
            .graph()
            .records()
            .iter()
            .map(|record| match record.resolution() {
                CanonicalImportResolution::Resolved(target) => {
                    StableModuleImportDependency::Resolved {
                        importer: record.importer().clone(),
                        normalized_specifier: Arc::from(record.normalized_specifier()),
                        target: target.clone(),
                    }
                }
                CanonicalImportResolution::Missing => StableModuleImportDependency::Missing {
                    importer: record.importer().clone(),
                    normalized_specifier: Arc::from(record.normalized_specifier()),
                },
                CanonicalImportResolution::Ambiguous {
                    file_module,
                    directory_module,
                } => StableModuleImportDependency::Ambiguous {
                    importer: record.importer().clone(),
                    normalized_specifier: Arc::from(record.normalized_specifier()),
                    file_module: file_module.clone(),
                    directory_module: directory_module.clone(),
                },
            })
            .collect::<Vec<_>>();
        let manifest = Arc::new(SemanticDependencyInputManifest {
            input,
            imports: imports.graph().clone(),
            definitions: keys.into(),
            module_imports: module_imports.into(),
            free_function_dependencies: free_function_dependencies.into(),
            free_function_caller_dependencies_complete,
            named_method_dependencies: named_method_dependencies.into(),
            non_generic_named_method_dependencies_complete: named_method_dependencies_complete,
            generic_named_method_dependencies_complete: false,
            named_destructor_dependencies: named_destructor_dependencies.into(),
            named_destructor_dependencies_complete,
            declaration_type_dependencies: declaration_type_dependencies.into(),
            declaration_type_dependencies_complete,
            declaration_type_call_head_dependencies: declaration_type_call_head_dependencies.into(),
            declaration_type_call_head_dependencies_complete,
            builtin_type_call_head_inputs: builtin_type_call_head_inputs.into(),
            supported_type_call_heads_complete,
            named_const_dependencies: named_const_dependencies.into(),
            named_value_const_dependencies_complete,
            semantic_dependency_graph_complete: false,
            definition_universe_complete,
            work,
        });
        self.dependency_manifest_cache.push(manifest.clone());
        Ok(manifest)
    }
}

fn stable_free_function_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    name: &str,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            record.declaration_span().file_id.index() == file
                && record.stable_key().name() == name
                && record.stable_key().namespace() == StableDefinitionNamespace::Value
                && record.stable_key().kind() == StableDefinitionKind::Function
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(&format!(
            "free-function dependency endpoint ({file}, '{name}') did not join exactly one bound function",
        )));
    };
    Ok(record.stable_key().clone())
}

fn stable_named_method_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    owner_name: &str,
    method_name: &str,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            let key = record.stable_key();
            record.declaration_span().file_id.index() == file
                && key.name() == method_name
                && key.namespace() == StableDefinitionNamespace::Method
                && matches!(
                    key.kind(),
                    StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction
                )
                && key.owner().is_some_and(|owner| owner.name() == owner_name)
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(&format!(
            "named-method dependency endpoint ({file}, '{owner_name}', '{method_name}') did not join exactly one bound method",
        )));
    };
    Ok(record.stable_key().clone())
}

fn stable_named_destructor_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    owner_name: &str,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            let key = record.stable_key();
            record.declaration_span().file_id.index() == file
                && key.name() == owner_name
                && key.namespace() == StableDefinitionNamespace::Destructor
                && key.kind() == StableDefinitionKind::Destructor
                && key.owner().is_some_and(|owner| owner.name() == owner_name)
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(&format!(
            "named-destructor dependency endpoint ({file}, '{owner_name}') did not join exactly one bound destructor",
        )));
    };
    Ok(record.stable_key().clone())
}

fn stable_declaration_source_endpoint(
    definitions: &BoundDefinitionSet,
    event: &rue_air::DeclarationTypeDependencyEvent,
) -> Result<StableDefinitionKey, CompileErrors> {
    stable_declaration_type_source_endpoint(
        definitions,
        event.source_file,
        &event.source_name,
        event.source_owner_name.as_deref(),
        event.source_kind,
    )
}

fn stable_declaration_type_source_endpoint(
    definitions: &BoundDefinitionSet,
    source_file: u32,
    source_name: &str,
    source_owner_name: Option<&str>,
    source_kind: rue_air::DeclarationTypeDependencySourceKind,
) -> Result<StableDefinitionKey, CompileErrors> {
    use rue_air::DeclarationTypeDependencySourceKind as K;
    match source_kind {
        K::Function => stable_free_function_endpoint(definitions, source_file, source_name),
        K::Method | K::AssociatedFunction => stable_named_method_endpoint(
            definitions,
            source_file,
            source_owner_name.unwrap_or(""),
            source_name,
        ),
        K::Destructor => stable_named_destructor_endpoint(
            definitions,
            source_file,
            source_owner_name.unwrap_or(source_name),
        ),
        K::Struct => stable_top_level_endpoint(
            definitions,
            source_file,
            source_name,
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
        ),
        K::Enum => stable_top_level_endpoint(
            definitions,
            source_file,
            source_name,
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Enum,
        ),
        K::ValueConst => stable_top_level_endpoint(
            definitions,
            source_file,
            source_name,
            StableDefinitionNamespace::Value,
            StableDefinitionKind::ValueConst,
        ),
    }
}

fn stable_named_type_endpoint(
    definitions: &BoundDefinitionSet,
    event: &rue_air::DeclarationTypeDependencyEvent,
) -> Result<StableDefinitionKey, CompileErrors> {
    let kind = match event.target_kind {
        rue_air::DeclarationTypeDependencyTargetKind::Struct => StableDefinitionKind::Struct,
        rue_air::DeclarationTypeDependencyTargetKind::Enum => StableDefinitionKind::Enum,
    };
    stable_top_level_endpoint(
        definitions,
        event.target_file,
        &event.target_name,
        StableDefinitionNamespace::Type,
        kind,
    )
}

fn stable_top_level_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    name: &str,
    namespace: StableDefinitionNamespace,
    kind: StableDefinitionKind,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            record.declaration_span().file_id.index() == file
                && record.stable_key().name() == name
                && record.stable_key().namespace() == namespace
                && record.stable_key().kind() == kind
                && record.stable_key().owner().is_none()
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(
            "declaration-type dependency endpoint did not join exactly one stable definition",
        ));
    };
    Ok(record.stable_key().clone())
}

fn invalid_dependency_manifest(reason: &str) -> CompileErrors {
    CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
        reason.to_owned(),
    )))
}

fn same_attempt(left: &SourceSnapshot, right: &SourceSnapshot) -> bool {
    left.source_revision() == right.source_revision() && left.metadata() == right.metadata()
}

fn programs_are_pointer_equivalent(left: &ParsedProgram, right: &ParsedProgram) -> bool {
    left.source_revision() == right.source_revision()
        && left.modules().len() == right.modules().len()
        && left
            .modules()
            .iter()
            .zip(right.modules())
            .all(|(left, right)| Arc::ptr_eq(left, right))
}

fn no_published_program() -> CompileErrors {
    CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
        "frontend query session has no successful parsed program".to_string(),
    )))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use rue_span::FileId;

    use super::*;
    use crate::{
        CanonicalImportResolution, LinkerMode, ModuleId, OptLevel, PreviewFeature, PreviewFeatures,
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

    fn base() -> SourceSnapshot {
        snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        )
    }

    #[test]
    fn repeated_queries_and_noop_update_retain_pointer_identity() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalFrontendSession>();
        assert_send_sync::<CanonicalMergedProgram>();
        assert_send_sync::<CanonicalRirOutput>();

        let source = base();
        let mut session = CanonicalFrontendSession::new();
        let first_program = session.update(&source).into_result().unwrap();
        let first_merge = session.merge().unwrap();
        let second_merge = session.merge().unwrap();
        let first_rir = session.rir().unwrap();
        let second_rir = session.rir().unwrap();
        assert!(Arc::ptr_eq(&first_merge, &second_merge));
        assert!(Arc::ptr_eq(&first_rir, &second_rir));

        let noop = session.update(&source);
        assert!(!noop.downstream_invalidated());
        let second_program = noop.into_result().unwrap();
        assert!(Arc::ptr_eq(&first_program, &second_program));
        assert!(Arc::ptr_eq(&first_merge, &session.merge().unwrap()));
        assert!(Arc::ptr_eq(&first_rir, &session.rir().unwrap()));
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(session.work().downstream_invalidations, 0);

        let published = session.published().unwrap().clone();
        let merged = first_merge.clone();
        let rir = first_rir.clone();
        std::thread::spawn(move || {
            assert_eq!(published.modules().len(), 2);
            assert_eq!(merged.ast().modules().len(), 2);
            assert!(!rir.rir().is_empty());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn one_edit_among_128_recomputes_downstream_once() {
        let make = |edited: bool| {
            let physical = (0..128)
                .map(|index| (FileId::new(index), format!("/p/m{index}.rue")))
                .collect();
            let logical = (0..128)
                .map(|index| (FileId::new(index), format!("m{index}.rue")))
                .collect();
            let metadata = SourceMetadata::new(FileId::new(0), physical, logical).unwrap();
            SourceSnapshot::new(
                metadata,
                (0..128)
                    .map(|index| {
                        let value = if edited && index == 81 { 2 } else { 1 };
                        (
                            FileId::new(index),
                            Arc::new(format!("fn f{index}() -> i32 {{ {value} }}")),
                        )
                    })
                    .collect(),
            )
            .unwrap()
        };
        let mut session = CanonicalFrontendSession::new();
        session.update(&make(false)).into_result().unwrap();
        session.rir().unwrap();
        let first_shards = session
            .definition_shard_baseline
            .as_ref()
            .unwrap()
            .shards()
            .to_vec();
        let update = session.update(&make(true));
        assert!(update.downstream_invalidated());
        assert_eq!(update.work().modules_reused, 127);
        assert_eq!(update.work().modules_reparsed, 1);
        session.rir().unwrap();
        let second_shards = session.definition_shard_baseline.as_ref().unwrap().shards();
        assert!(
            first_shards
                .iter()
                .zip(second_shards)
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
        assert_eq!(session.work().last_merge.definition_shards_indexed, 128);
        assert_eq!(session.work().last_merge.definition_shards_reused, 128);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 0);
        session.rir().unwrap();
        assert_eq!(session.work().merge.executions, 2);
        assert_eq!(session.work().rir.executions, 2);
        assert_eq!(session.work().downstream_invalidations, 1);
    }

    #[test]
    fn definition_shards_fail_closed_on_surface_identity_changes() {
        let initial = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            ],
            1,
        );
        let body = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() -> i32 { 2 }"),
            ],
            1,
        );
        let renamed_definition = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn b() -> i32 { 2 }"),
            ],
            1,
        );
        let relocated = snapshot(
            &[
                (1, "/m/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/m/a.rue", "a.rue", "fn b() -> i32 { 2 }"),
            ],
            1,
        );
        let reassigned = snapshot(
            &[
                (11, "/m/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (12, "/m/a.rue", "a.rue", "fn b() -> i32 { 2 }"),
            ],
            11,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&initial).into_result().unwrap();
        session.merge().unwrap();

        session.update(&body).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 2);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 0);

        session.update(&renamed_definition).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 1);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 1);

        session.update(&relocated).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 2);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 0);

        session.update(&reassigned).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 0);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 2);
    }

    #[test]
    fn syntax_failure_preserves_published_revision_and_cached_queries() {
        let source = base();
        let broken = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let mut session = CanonicalFrontendSession::new();
        let program = session.update(&source).into_result().unwrap();
        let merged = session.merge().unwrap();
        let rir = session.rir().unwrap();
        let failed = session.update(&broken);
        assert!(failed.result().is_err());
        assert!(!failed.downstream_invalidated());
        assert!(Arc::ptr_eq(session.published().unwrap(), &program));
        assert!(Arc::ptr_eq(&session.merge().unwrap(), &merged));
        assert!(Arc::ptr_eq(&session.rir().unwrap(), &rir));
    }

    #[test]
    fn duplicate_merge_error_is_memoized_and_recovery_invalidates_it() {
        let duplicate = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn same() {} fn same() {} fn main() {}",
            )],
            1,
        );
        let fixed = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&duplicate).into_result().unwrap();
        let first = session.merge().unwrap_err();
        let second = session.merge().unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert!(session.rir().is_err());
        assert!(session.semantic(&CompileOptions::default()).is_err());
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 0);
        assert_eq!(session.work().semantic.executions, 0);

        let update = session.update(&fixed);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        assert!(session.rir().is_ok());
        assert_eq!(session.work().merge.executions, 2);
        assert_eq!(session.work().rir.executions, 1);
    }

    #[test]
    fn root_relocation_file_id_and_logical_changes_invalidate_correctly() {
        let base = snapshot(
            &[
                (1, "/old/a.rue", "a.rue", "fn a() {}"),
                (2, "/old/b.rue", "b.rue", "fn b() {}"),
            ],
            1,
        );
        let root_only = snapshot(
            &[
                (1, "/old/a.rue", "a.rue", "fn a() {}"),
                (2, "/old/b.rue", "b.rue", "fn b() {}"),
            ],
            2,
        );
        let relocated = snapshot(
            &[
                (1, "/new/a.rue", "a.rue", "fn a() {}"),
                (2, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            2,
        );
        let reassigned = snapshot(
            &[
                (11, "/new/a.rue", "a.rue", "fn a() {}"),
                (12, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            12,
        );
        let renamed = snapshot(
            &[
                (11, "/new/a2.rue", "a2.rue", "fn a() {}"),
                (12, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            12,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&base).into_result().unwrap();
        session.rir().unwrap();

        let root = session.update(&root_only);
        assert!(root.downstream_invalidated());
        assert_eq!(root.work().modules_reused, 2);
        root.into_result().unwrap();
        session.rir().unwrap();
        let moved = session.update(&relocated);
        assert!(moved.downstream_invalidated());
        assert_eq!(moved.work().modules_rebound, 2);
        moved.into_result().unwrap();
        session.rir().unwrap();
        let ids = session.update(&reassigned);
        assert!(ids.downstream_invalidated());
        assert_eq!(ids.work().modules_reparsed, 2);
        ids.into_result().unwrap();
        session.rir().unwrap();
        let rename = session.update(&renamed);
        assert!(rename.downstream_invalidated());
        assert_eq!(rename.invalidation().added.len(), 1);
        assert_eq!(rename.invalidation().removed.len(), 1);
        assert_eq!(rename.work().modules_rebound, 1);
    }

    #[test]
    fn semantic_queries_reuse_by_codegen_identity_and_ignore_linker() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalSemanticOutput>();

        let source = base();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let options = CompileOptions::default();
        let first = session.semantic(&options).unwrap();
        let second = session.semantic(&options).unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        let linker_only = CompileOptions {
            linker: LinkerMode::System("unused-linker".to_string()),
            ..options.clone()
        };
        assert!(Arc::ptr_eq(
            &first,
            &session.semantic(&linker_only).unwrap()
        ));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 2);
        assert_eq!(session.work().semantic_entries, 1);
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(first.work().binding.bind_invocations, 1);
        assert_eq!(first.work().manifest.build_invocations, 0);

        let published = first.clone();
        std::thread::spawn(move || assert!(!published.functions().is_empty()))
            .join()
            .unwrap();
    }

    #[test]
    fn semantic_option_variants_create_deterministic_distinct_entries() {
        let source = base();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let default = CompileOptions::default();
        session.semantic(&default).unwrap();
        session
            .semantic(&CompileOptions {
                opt_level: OptLevel::O1,
                ..default.clone()
            })
            .unwrap();
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != default.target)
            .expect("multiple compiler targets");
        session
            .semantic(&CompileOptions {
                target: other_target,
                ..default.clone()
            })
            .unwrap();
        session
            .semantic(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..default
            })
            .unwrap();

        let work = session.work();
        assert_eq!(work.semantic.executions, 4);
        assert_eq!(work.semantic_entries, 4);
        assert_eq!(work.semantic_records.len(), 4);
        assert!(work.semantic_records.iter().all(|record| {
            !record.failed
                && record.work.binding.bind_invocations == 1
                && record.work.manifest.build_invocations == 0
        }));
        for (index, left) in work.semantic_records.iter().enumerate() {
            assert!(
                work.semantic_records[index + 1..]
                    .iter()
                    .all(|right| left.input != right.input)
            );
        }
    }

    #[test]
    fn semantic_cache_invalidates_on_edit_but_survives_failed_parse() {
        let source = base();
        let edited = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let broken = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let first = session.semantic(&options).unwrap();
        assert!(session.update(&broken).result().is_err());
        assert!(Arc::ptr_eq(&first, &session.semantic(&options).unwrap()));
        let update = session.update(&edited);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        let second = session.semantic(&options).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic_entries_invalidated, 1);
    }

    #[test]
    fn semantic_errors_are_memoized_and_recovery_reexecutes() {
        let invalid = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { missing_name }",
            )],
            1,
        );
        let valid = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CanonicalFrontendSession::new();
        session.update(&invalid).into_result().unwrap();
        let first = session.semantic(&options).unwrap_err();
        let second = session.semantic(&options).unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 1);
        assert_eq!(session.work().semantic_records.len(), 1);
        assert!(session.work().semantic_records[0].failed);

        session.update(&valid).into_result().unwrap();
        assert!(session.semantic(&options).is_ok());
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic_entries, 1);
        assert_eq!(session.work().semantic_entries_invalidated, 1);
    }

    #[test]
    fn stable_definitions_are_lazy_reused_and_make_two_bind_boundary_explicit() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BoundDefinitionSet>();

        let source = base();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let ordinary_options = CompileOptions::default();
        let ordinary = session.semantic(&ordinary_options).unwrap();
        assert_eq!(session.work().definitions.executions, 0);
        assert_eq!(session.work().definition_entries, 0);

        let id_options = CompileOptions {
            linker: LinkerMode::System("ignored".to_string()),
            opt_level: OptLevel::O1,
            ..ordinary_options.clone()
        };
        let first = session.stable_definitions(&id_options).unwrap();
        let second = session
            .stable_definitions(&CompileOptions {
                linker: LinkerMode::Internal,
                opt_level: OptLevel::O3,
                ..ordinary_options
            })
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().definitions.executions, 1);
        assert_eq!(session.work().definitions.reuses, 1);
        assert_eq!(session.work().definition_entries, 1);
        let record = &session.work().definition_records[0];
        assert_eq!(ordinary.work().binding.bind_invocations, 1);
        assert_eq!(record.binding.bind_invocations, 1);
        assert_eq!(record.manifest.build_invocations, 1);
        assert_eq!(first.manifest_work().build_invocations, 1);
        assert!(record.issuance.ids_issued > 0);
        assert!(!record.failed);

        let published = first.clone();
        std::thread::spawn(move || assert!(!published.definitions().is_empty()))
            .join()
            .unwrap();
    }

    #[test]
    fn stable_then_ordinary_reuses_the_validation_semantic_entry() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        session.stable_definitions(&options).unwrap();
        let semantic_executions = session.work().semantic.executions;
        let ordinary = session.semantic(&options).unwrap();

        assert!(!ordinary.functions().is_empty());
        assert_eq!(semantic_executions, 1);
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 1);
        assert_eq!(session.work().definitions.executions, 1);
        assert_eq!(
            session.work().definition_records[0]
                .binding
                .bind_invocations,
            1
        );
    }

    #[test]
    fn published_queries_support_stable_tooling_lookups() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CanonicalFrontendSession::new();
        let published = session.update(&source).into_result().unwrap();

        let module_id = ModuleId::from_logical_path("a.rue").unwrap();
        let module = published.module(&module_id).expect("module by stable ID");
        assert_eq!(module.module_id(), &module_id);
        assert!(
            published
                .module(&ModuleId::from_logical_path("missing.rue").unwrap())
                .is_none()
        );

        let definitions = session.stable_definitions(&options).unwrap();
        let record = &definitions.definitions()[0];
        assert!(std::ptr::eq(
            definitions
                .definition_by_key(record.stable_key())
                .expect("definition by stable key"),
            record
        ));
    }

    #[test]
    fn import_graph_query_reuses_and_recomputes_only_after_resolution_changes() {
        let original = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/p/app/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let relocated = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/else/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&original).into_result().unwrap();
        let first = session.import_graph(None).unwrap();
        let reused = session.import_graph(None).unwrap();
        assert!(Arc::ptr_eq(&first, &reused));
        assert!(matches!(
            first.graph().records()[0].resolution(),
            CanonicalImportResolution::Resolved(module) if module.as_str() == "app/helper.rue"
        ));

        let update = session.update(&relocated);
        assert_eq!(update.work().syntax.lexer_invocations, 0);
        assert_eq!(update.work().syntax.parser_invocations, 0);
        assert_eq!(update.work().modules_rebound, 1);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        let moved = session.import_graph(None).unwrap();
        assert!(matches!(
            moved.graph().records()[0].resolution(),
            CanonicalImportResolution::Missing
        ));
        assert_eq!(session.work().imports.calls, 3);
        assert_eq!(session.work().imports.executions, 2);
        assert_eq!(session.work().imports.reuses, 1);
        assert_eq!(session.work().import_entries_invalidated, 1);
        assert_eq!(session.work().merge.executions, 0);
        assert_eq!(session.work().rir.executions, 0);
        assert_eq!(session.work().semantic.executions, 0);
        assert_eq!(session.work().definitions.executions, 0);
    }

    #[test]
    fn import_graph_keys_root_std_context_and_preserves_last_good_graph() {
        let source = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "fn main() -> i32 { let s = @import(\"std\"); 0 }",
                ),
                (2, "/sdk/_std.rue", "std/_std.rue", "fn helper() {}"),
            ],
            1,
        );
        let other_root = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "fn main() -> i32 { let s = @import(\"std\"); 0 }",
                ),
                (2, "/sdk/_std.rue", "std/_std.rue", "fn helper() {}"),
            ],
            2,
        );
        let broken = snapshot(&[(1, "/p/main.rue", "main.rue", "fn main( {")], 1);
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let missing = session.import_graph(None).unwrap();
        let resolved = session.import_graph(Some("/sdk")).unwrap();
        assert!(matches!(
            missing.graph().records()[0].resolution(),
            CanonicalImportResolution::Missing
        ));
        assert!(matches!(
            resolved.graph().records()[0].resolution(),
            CanonicalImportResolution::Resolved(_)
        ));
        assert_eq!(session.work().import_entries, 2);
        assert!(session.update(&broken).result().is_err());
        assert!(Arc::ptr_eq(
            &resolved,
            &session.import_graph(Some("/sdk")).unwrap()
        ));

        session.update(&other_root).into_result().unwrap();
        let rerooted = session.import_graph(Some("/sdk")).unwrap();
        assert_eq!(rerooted.graph().root().as_str(), "std/_std.rue");
        assert_ne!(
            resolved.input().sources.root(),
            rerooted.input().sources.root()
        );
    }

    #[test]
    fn empty_import_graph_is_send_sync_and_concurrently_readable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalImportGraphOutput>();
        let mut session = CanonicalFrontendSession::new();
        session.update(&base()).into_result().unwrap();
        let graph = session.import_graph(None).unwrap();
        assert!(graph.graph().records().is_empty());
        std::thread::spawn(move || assert!(graph.validation().is_valid()))
            .join()
            .unwrap();
    }

    #[test]
    fn stable_definitions_prefers_a_successful_semantic_variant() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        session.semantic(&options).unwrap();

        let mut failed_input = session.semantic_cache[0].input.clone();
        failed_input.opt_level = crate::StableOptLevel::O1;
        session.semantic_cache.insert(
            0,
            SemanticCacheEntry {
                input: failed_input,
                result: Err(CompileErrors::from(CompileError::without_span(
                    ErrorKind::InvalidCompilerInput(
                        "synthetic prior failed opt variant".to_string(),
                    ),
                ))),
            },
        );

        let definitions = session
            .stable_definitions(&CompileOptions {
                opt_level: OptLevel::O2,
                ..options
            })
            .unwrap();

        assert!(!definitions.definitions().is_empty());
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().definitions.executions, 1);
        let record = &session.work().definition_records[0];
        assert_eq!(record.binding.bind_invocations, 1);
        assert_eq!(record.manifest.build_invocations, 1);
    }

    #[test]
    fn dependency_input_manifest_is_stable_ordered_and_adds_no_rir_scan() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SemanticDependencyInputManifest>();
        let source = base();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let first = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let second = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.definitions().windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            first.work().definition_records_visited,
            first.definitions().len()
        );
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
        assert_eq!(session.work().dependency_manifests.executions, 1);
        assert_eq!(session.work().dependency_manifests.reuses, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(session.work().semantic.executions, 1);
    }

    #[test]
    fn dependency_manifest_carries_resolved_and_fail_closed_module_edges() {
        let original = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/p/app/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let moved = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/else/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&original).into_result().unwrap();
        let resolved = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(resolved.definition_universe_complete());
        assert!(matches!(
            &resolved.module_imports()[0],
            StableModuleImportDependency::Resolved { importer, target, .. }
                if importer.as_str() == "app/main.rue" && target.as_str() == "app/helper.rue"
        ));

        let update = session.update(&moved);
        assert_eq!(update.work().syntax.lexer_invocations, 0);
        update.into_result().unwrap();
        let missing = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(!missing.definition_universe_complete());
        assert!(matches!(
            &missing.module_imports()[0],
            StableModuleImportDependency::Missing { importer, .. }
                if importer.as_str() == "app/main.rue"
        ));
        assert_eq!(missing.work().import_records_visited, 1);
        assert_eq!(missing.work().extra_rir_instructions_visited, 0);
        assert_eq!(session.work().dependency_manifests.executions, 2);
    }

    fn stable_edge_names(
        manifest: &SemanticDependencyInputManifest,
    ) -> Vec<(String, String, String, String)> {
        manifest
            .free_function_dependencies()
            .iter()
            .map(|edge| {
                (
                    edge.caller.module().as_str().to_owned(),
                    edge.caller.name().to_owned(),
                    edge.callee.module().as_str().to_owned(),
                    edge.callee.name().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn specialized_free_function_edges_are_stable_and_deduplicate_instances() {
        let first = snapshot(
            &[
                (
                    9,
                    "/one/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       fn main() -> i32 { lib.wrap(1, 10) + lib.wrap(2, 20) }"#,
                ),
                (
                    3,
                    "/one/lib.rue",
                    "lib.rue",
                    r#"fn leaf(value: i32) -> i32 { value }
                       fn inner(comptime n: i32, value: i32) -> i32 { leaf(value) + n }
                       pub fn wrap(comptime n: i32, value: i32) -> i32 { inner(n, value) }"#,
                ),
            ],
            9,
        );
        let moved = snapshot(
            &[
                (
                    41,
                    "/else/lib.rue",
                    "lib.rue",
                    r#"fn leaf(value: i32) -> i32 { value }
                       fn inner(comptime n: i32, value: i32) -> i32 { leaf(value) + n }
                       pub fn wrap(comptime n: i32, value: i32) -> i32 { inner(n, value) }"#,
                ),
                (
                    7,
                    "/else/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       fn main() -> i32 { lib.wrap(1, 10) + lib.wrap(2, 20) }"#,
                ),
            ],
            7,
        );
        let build = |source: &SourceSnapshot| {
            let mut session = CanonicalFrontendSession::new();
            session.update(source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(&first);
        let moved = build(&moved);
        assert_eq!(stable_edge_names(&first), stable_edge_names(&moved));
        assert_eq!(
            stable_edge_names(&first),
            vec![
                (
                    "lib.rue".into(),
                    "inner".into(),
                    "lib.rue".into(),
                    "leaf".into()
                ),
                (
                    "lib.rue".into(),
                    "wrap".into(),
                    "lib.rue".into(),
                    "inner".into()
                ),
                (
                    "main.rue".into(),
                    "main".into(),
                    "lib.rue".into(),
                    "wrap".into()
                ),
            ]
        );
        assert!(first.free_function_caller_dependencies_complete());
        assert!(!first.semantic_dependency_graph_complete());
        assert_eq!(first.work().specialization_origins_validated, 4);
        assert_eq!(first.work().free_function_events_translated, 5);
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn recursive_specialization_edges_and_renames_are_exact() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                r#"fn leaf(value: i32) -> i32 { value }
                   fn fib(comptime n: i32) -> i32 {
                       if n < 2 { leaf(n) } else { fib(n - 1) + fib(n - 2) }
                   }
                   fn main() -> i32 { fib(5) + fib(5) }"#,
            )],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert_eq!(
            stable_edge_names(&manifest),
            vec![
                (
                    "main.rue".into(),
                    "fib".into(),
                    "main.rue".into(),
                    "fib".into()
                ),
                (
                    "main.rue".into(),
                    "fib".into(),
                    "main.rue".into(),
                    "leaf".into()
                ),
                (
                    "main.rue".into(),
                    "main".into(),
                    "main.rue".into(),
                    "fib".into()
                ),
            ]
        );
        assert_eq!(manifest.work().specialization_origins_validated, 6);
        assert!(manifest.work().free_function_events_translated > 3);

        let renamed = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                r#"fn terminal(value: i32) -> i32 { value }
                   fn fib(comptime n: i32) -> i32 {
                       if n < 2 { terminal(n) } else { fib(n - 1) + fib(n - 2) }
                   }
                   fn main() -> i32 { fib(5) + fib(5) }"#,
            )],
            1,
        );
        session.update(&renamed).into_result().unwrap();
        let renamed = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(
            stable_edge_names(&renamed)
                .iter()
                .any(|edge| edge.3 == "terminal")
        );
        assert!(
            !stable_edge_names(&renamed)
                .iter()
                .any(|edge| edge.3 == "leaf")
        );
    }

    #[test]
    fn dependency_endpoint_translation_fails_closed_for_missing_and_non_functions() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const answer: i32 = 42; fn main() -> i32 { answer }",
            )],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let definitions = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        assert!(stable_free_function_endpoint(&definitions, 1, "missing").is_err());
        assert!(stable_free_function_endpoint(&definitions, 1, "answer").is_err());
        assert!(stable_named_method_endpoint(&definitions, 1, "answer", "answer").is_err());

        let rejected = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { true }")],
            1,
        );
        session.update(&rejected).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(!manifest.definition_universe_complete());
        assert!(!manifest.free_function_caller_dependencies_complete());
        assert!(manifest.free_function_dependencies().is_empty());
    }

    #[test]
    fn stable_named_method_dependency_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StableNamedMethodDependency>();
        assert_send_sync::<StableNamedMethodDependencyTarget>();
        assert_send_sync::<StableNamedConstDependency>();
        assert_send_sync::<StableNamedConstDependencyTarget>();
    }

    #[test]
    fn sibling_generic_owners_stay_distinct_and_non_function_calls_are_excluded() {
        let source = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    r#"const left = @import("left.rue");
                       const right = @import("right.rue");
                       fn main() -> i32 { left.id(1) + right.id(2) }"#,
                ),
                (
                    2,
                    "/p/left.rue",
                    "left.rue",
                    r#"struct Box { value: i32, fn get(borrow self) -> i32 { self.value } }
                       fn leaf(value: i32) -> i32 { value }
                       pub fn id(comptime n: i32) -> i32 {
                           let value = Box { value: n };
                           @dbg(n);
                           leaf(value.get())
                       }"#,
                ),
                (
                    3,
                    "/p/right.rue",
                    "right.rue",
                    r#"fn leaf(value: i32) -> i32 { value }
                       pub fn id(comptime n: i32) -> i32 { leaf(n) }"#,
                ),
            ],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert_eq!(
            stable_edge_names(&manifest),
            vec![
                (
                    "left.rue".into(),
                    "id".into(),
                    "left.rue".into(),
                    "leaf".into()
                ),
                (
                    "main.rue".into(),
                    "main".into(),
                    "left.rue".into(),
                    "id".into()
                ),
                (
                    "main.rue".into(),
                    "main".into(),
                    "right.rue".into(),
                    "id".into()
                ),
                (
                    "right.rue".into(),
                    "id".into(),
                    "right.rue".into(),
                    "leaf".into()
                ),
            ]
        );
        assert!(
            manifest
                .free_function_dependencies()
                .iter()
                .all(|edge| { edge.callee.name() != "get" && edge.callee.name() != "Box" })
        );
    }

    fn named_method_edge_names(
        manifest: &SemanticDependencyInputManifest,
    ) -> Vec<(String, String, String, String, String, String)> {
        manifest
            .named_method_dependencies()
            .iter()
            .map(|edge| {
                let caller_owner = edge.caller.owner().unwrap().name().to_owned();
                match &edge.target {
                    StableNamedMethodDependencyTarget::FreeFunction(target) => (
                        edge.caller.module().as_str().to_owned(),
                        caller_owner,
                        edge.caller.name().to_owned(),
                        "free".to_owned(),
                        target.module().as_str().to_owned(),
                        target.name().to_owned(),
                    ),
                    StableNamedMethodDependencyTarget::NamedMethod(target) => (
                        edge.caller.module().as_str().to_owned(),
                        caller_owner,
                        edge.caller.name().to_owned(),
                        target.owner().unwrap().name().to_owned(),
                        target.module().as_str().to_owned(),
                        target.name().to_owned(),
                    ),
                }
            })
            .collect()
    }

    #[test]
    fn named_method_edges_are_stable_exact_and_normalize_generic_free_callees() {
        let program = r#"fn helper() -> i32 { 1 }
            fn generic(comptime n: i32) -> i32 { helper() + n }
            struct B { value: i32, fn ping(borrow self) -> i32 { helper() + self.value } }
            struct A {
                value: i32,
                fn run(borrow self) -> i32 {
                    let b = B { value: self.value };
                    b.ping() + self.next()
                }
                fn next(borrow self) -> i32 { generic(2) + self.run() }
            }
            fn main() -> i32 { let a = A { value: 1 }; a.run() }"#;
        let first_source = snapshot(&[(9, "/one/main.rue", "main.rue", program)], 9);
        let moved_source = snapshot(&[(41, "/else/main.rue", "main.rue", program)], 41);
        let build = |source: &SourceSnapshot| {
            let mut session = CanonicalFrontendSession::new();
            session.update(source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(&first_source);
        let moved = build(&moved_source);
        assert_eq!(
            named_method_edge_names(&first),
            named_method_edge_names(&moved)
        );
        let edges = named_method_edge_names(&first);
        for expected in [
            ("main.rue", "A", "run", "B", "main.rue", "ping"),
            ("main.rue", "A", "run", "A", "main.rue", "next"),
            ("main.rue", "A", "next", "A", "main.rue", "run"),
            ("main.rue", "A", "next", "free", "main.rue", "generic"),
            ("main.rue", "B", "ping", "free", "main.rue", "helper"),
        ] {
            assert!(edges.contains(&(
                expected.0.into(),
                expected.1.into(),
                expected.2.into(),
                expected.3.into(),
                expected.4.into(),
                expected.5.into(),
            )));
        }
        assert!(first.non_generic_named_method_dependencies_complete());
        assert!(!first.generic_named_method_dependencies_complete());
        assert_eq!(first.work().named_method_events_translated, edges.len());
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn named_destructor_edges_translate_to_stable_owner_and_target() {
        let program = "fn cleanup() {} struct Value { n: i32 } drop fn Value(self) { cleanup(); } fn main() -> i32 { let value = Value { n: 1 }; 0 }";
        let first_source = snapshot(&[(3, "/one/main.rue", "main.rue", program)], 3);
        let moved_source = snapshot(&[(71, "/else/main.rue", "main.rue", program)], 71);
        let build = |source: &SourceSnapshot| {
            let mut session = CanonicalFrontendSession::new();
            session.update(source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(&first_source);
        let moved = build(&moved_source);
        assert_eq!(
            first.named_destructor_dependencies(),
            moved.named_destructor_dependencies()
        );
        let [edge] = first.named_destructor_dependencies() else {
            panic!("expected one destructor dependency");
        };
        assert_eq!(edge.caller.owner().unwrap().name(), "Value");
        assert_eq!(edge.caller.kind(), StableDefinitionKind::Destructor);
        let StableNamedMethodDependencyTarget::FreeFunction(target) = &edge.target else {
            panic!("cleanup is a free function");
        };
        assert_eq!(target.name(), "cleanup");
        assert!(first.named_destructor_dependencies_complete());
        assert_eq!(first.work().named_destructor_events_translated, 1);
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn resolved_declaration_type_edges_translate_without_rir_rescan() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                r#"
                struct Leaf { n: i32 }
                struct Holder { leaf: Leaf, fn get(borrow self, value: Leaf) -> Leaf { value } }
                enum Choice { One(Leaf) }
                fn convert(value: Leaf) -> Holder { Holder { leaf: value } }
                drop fn Holder(self) {}
                fn main() -> i32 { 0 }
            "#,
            )],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let names = manifest
            .declaration_type_dependencies()
            .iter()
            .map(|edge| {
                (
                    edge.source.name().to_owned(),
                    edge.target.name().to_owned(),
                    edge.kind,
                )
            })
            .collect::<Vec<_>>();
        assert!(names.contains(&(
            "Holder".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Field
        )));
        assert!(names.contains(&(
            "Choice".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Payload
        )));
        assert!(names.contains(&(
            "convert".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Signature
        )));
        assert!(names.contains(&(
            "get".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Signature
        )));
        assert!(names.contains(&(
            "Holder".into(),
            "Holder".into(),
            rue_air::DeclarationTypeDependencyKind::Owner
        )));
        assert!(!manifest.declaration_type_dependencies_complete());
        assert_eq!(manifest.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn deferred_nested_type_call_heads_survive_placeholder_erasure() {
        let program = r#"
            fn Result(comptime T: type) -> type { enum { Ok(T), Err } }
            fn Option(comptime T: type) -> type { enum { Some(T), None } }
            fn consume(comptime T: type, value: Option(Result(T))) -> i32 { 0 }
            fn main() -> i32 { 0 }
        "#;
        let build = |file_id, path| {
            let source = snapshot(&[(file_id, path, "main.rue", program)], file_id);
            let mut session = CanonicalFrontendSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(7, "/one/main.rue");
        let moved = build(91, "/moved/main.rue");
        assert_eq!(
            first.declaration_type_call_head_dependencies(),
            moved.declaration_type_call_head_dependencies()
        );
        let names = first
            .declaration_type_call_head_dependencies()
            .iter()
            .map(|edge| (edge.source.name(), edge.callable.name(), edge.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                (
                    "consume",
                    "Option",
                    rue_air::DeclarationTypeDependencyKind::Signature
                ),
                (
                    "consume",
                    "Result",
                    rue_air::DeclarationTypeDependencyKind::Signature
                ),
            ]
        );
        assert!(!first.declaration_type_call_head_dependencies_complete());
        assert_eq!(first.work().declaration_type_call_head_events_translated, 2);
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn module_qualified_type_call_head_uses_exact_callable_endpoint() {
        let source = snapshot(
            &[
                (
                    3,
                    "/p/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       fn consume(comptime T: type, value: lib.Box(T)) -> i32 { 0 }
                       fn main() -> i32 { 0 }"#,
                ),
                (
                    8,
                    "/p/lib.rue",
                    "lib.rue",
                    "pub fn Box(comptime T: type) -> type { struct { value: T } }",
                ),
            ],
            3,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let [edge] = manifest.declaration_type_call_head_dependencies() else {
            panic!("expected one module-qualified type-call head");
        };
        assert_eq!(edge.source.module().as_str(), "main.rue");
        assert_eq!(edge.source.name(), "consume");
        assert_eq!(edge.callable.module().as_str(), "lib.rue");
        assert_eq!(edge.callable.name(), "Box");
        assert_eq!(edge.callable.kind(), StableDefinitionKind::Function);
    }

    #[test]
    fn fixed_string_type_head_is_a_builtin_input_not_a_definition() {
        let source = snapshot(
            &[(
                4,
                "/p/main.rue",
                "main.rue",
                "fn consume(value: Str(8)) -> i32 { 0 } fn main() -> i32 { 0 }",
            )],
            4,
        );
        let mut options = CompileOptions::default();
        options
            .preview_features
            .insert("string_trio".parse().unwrap());
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session.semantic_dependency_inputs(&options, None).unwrap();
        let [input] = manifest.builtin_type_call_head_inputs() else {
            panic!("expected one fixed-string builtin input");
        };
        assert_eq!(input.source.name(), "consume");
        assert_eq!(
            input.builtin,
            rue_air::BuiltinTypeCallHead::FixedCapacityString
        );
        assert!(
            manifest
                .declaration_type_call_head_dependencies()
                .is_empty()
        );
        assert!(manifest.supported_type_call_heads_complete());
        assert!(!manifest.declaration_type_call_head_dependencies_complete());
        assert_eq!(manifest.work().builtin_type_call_head_inputs_translated, 1);
        assert_eq!(manifest.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn named_owner_associated_type_head_is_not_supported_type_syntax() {
        let source = snapshot(
            &[(
                6,
                "/p/main.rue",
                "main.rue",
                r#"struct Factory {
                       fn Make() -> type { struct { value: i32 } }
                   }
                   fn consume(value: Factory.Make()) -> i32 { 0 }
                   fn main() -> i32 { 0 }"#,
            )],
            6,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        assert!(
            session.semantic(&CompileOptions::default()).is_err(),
            "dotted type-call heads are module-qualified free functions, not associated functions"
        );
    }

    #[test]
    fn named_const_initializer_edges_are_stable_direct_and_zero_scan() {
        let program = r#"
            struct Point { x: i32 }
            fn inc(comptime n: i32) -> i32 { n + 1 }
            const A: i32 = 1;
            const B: i32 = inc(A);
            const C: i32 = A + B;
            const D: i32 = B + C;
            const P = Point;
            fn main() -> i32 { D }
        "#;
        let build = |file, path| {
            let source = snapshot(&[(file, path, "main.rue", program)], file);
            let mut session = CanonicalFrontendSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(2, "/one/main.rue");
        let moved = build(88, "/moved/main.rue");
        assert_eq!(
            first.named_const_dependencies(),
            moved.named_const_dependencies()
        );
        let names = first
            .named_const_dependencies()
            .iter()
            .map(|edge| {
                let target = match &edge.target {
                    StableNamedConstDependencyTarget::ValueConst(key)
                    | StableNamedConstDependencyTarget::FreeFunction(key)
                    | StableNamedConstDependencyTarget::NamedType(key)
                    | StableNamedConstDependencyTarget::ModuleBinding(key) => key.name(),
                };
                (edge.source.name(), target)
            })
            .collect::<Vec<_>>();
        for edge in [
            ("B", "A"),
            ("B", "inc"),
            ("C", "A"),
            ("C", "B"),
            ("D", "B"),
            ("D", "C"),
            ("P", "Point"),
        ] {
            assert!(
                names.contains(&edge),
                "missing direct edge {edge:?}: {names:?}"
            );
        }
        assert!(first.named_value_const_dependencies_complete());
        assert_eq!(first.work().named_const_events_translated, names.len());
        assert_eq!(first.work().extra_rir_instructions_visited, 0);

        let renamed_program = program
            .replace("const A: i32 = 1", "const Z: i32 = 1")
            .replace("inc(A)", "inc(Z)")
            .replace("A + B", "Z + B");
        let renamed_source = snapshot(&[(2, "/one/main.rue", "main.rue", &renamed_program)], 2);
        let mut renamed_session = CanonicalFrontendSession::new();
        renamed_session
            .update(&renamed_source)
            .into_result()
            .unwrap();
        let renamed = renamed_session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert_ne!(
            first.named_const_dependencies(),
            renamed.named_const_dependencies()
        );
        assert!(renamed.named_const_dependencies().iter().any(|edge| {
            edge.source.name() == "B"
                && matches!(&edge.target, StableNamedConstDependencyTarget::ValueConst(key) if key.name() == "Z")
        }));
    }

    #[test]
    fn cyclic_const_initializers_publish_no_partial_dependency_graph() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const A: i32 = B; const B: i32 = A; fn main() -> i32 { 0 }",
            )],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        assert!(session.semantic(&CompileOptions::default()).is_err());
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(manifest.named_const_dependencies().is_empty());
        assert!(!manifest.named_value_const_dependencies_complete());
        assert!(!manifest.definition_universe_complete());
    }

    #[test]
    fn qualified_const_edges_keep_module_binding_and_exact_member_identity() {
        let source = snapshot(
            &[
                (
                    3,
                    "/p/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       const other = @import("other.rue");
                       const X: i32 = lib.BASE;
                       const Y: i32 = other.BASE;
                       const T = lib.Row;
                       fn main() -> i32 { X + Y }"#,
                ),
                (11, "/p/other.rue", "other.rue", "pub const BASE: i32 = 5;"),
                (
                    9,
                    "/p/lib.rue",
                    "lib.rue",
                    "pub const BASE: i32 = 4; pub struct Row { n: i32 }",
                ),
            ],
            3,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let tags = manifest
            .named_const_dependencies()
            .iter()
            .map(|edge| {
                let (tag, target) = match &edge.target {
                    StableNamedConstDependencyTarget::ValueConst(key) => ("const", key),
                    StableNamedConstDependencyTarget::NamedType(key) => ("type", key),
                    StableNamedConstDependencyTarget::ModuleBinding(key) => ("module", key),
                    StableNamedConstDependencyTarget::FreeFunction(key) => ("fn", key),
                };
                (
                    edge.source.name(),
                    tag,
                    target.module().as_str(),
                    target.name(),
                )
            })
            .collect::<Vec<_>>();
        for expected in [
            ("X", "module", "main.rue", "lib"),
            ("X", "const", "lib.rue", "BASE"),
            ("T", "module", "main.rue", "lib"),
            ("T", "type", "lib.rue", "Row"),
            ("Y", "module", "main.rue", "other"),
            ("Y", "const", "other.rue", "BASE"),
        ] {
            assert!(tags.contains(&expected), "missing {expected:?}: {tags:?}");
        }
        assert!(
            tags.iter()
                .all(|(source, _, _, _)| *source != "lib" && *source != "other")
        );
    }

    #[test]
    fn const_dependency_capture_work_is_edge_proportional() {
        let build = |extra: usize| {
            let mut program = "const A: i32 = 1; const B: i32 = A;".to_string();
            for i in 0..extra {
                program.push_str(&format!(" const UNUSED_{i}: i32 = {i};"));
            }
            program.push_str(" fn main() -> i32 { B }");
            let source = snapshot(&[(1, "/p/main.rue", "main.rue", &program)], 1);
            let mut session = CanonicalFrontendSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let one = build(1);
        let many = build(128);
        assert_eq!(
            one.named_const_dependencies(),
            many.named_const_dependencies()
        );
        assert_eq!(one.work().named_const_events_translated, 1);
        assert_eq!(many.work().named_const_events_translated, 1);
        assert_eq!(many.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn stable_definition_target_and_feature_inputs_are_separate() {
        let source = base();
        let default = CompileOptions::default();
        let mut session = CanonicalFrontendSession::new();
        session.update(&source).into_result().unwrap();
        session.stable_definitions(&default).unwrap();
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != default.target)
            .expect("multiple compiler targets");
        session
            .stable_definitions(&CompileOptions {
                target: other_target,
                ..default.clone()
            })
            .unwrap();
        session
            .stable_definitions(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..default
            })
            .unwrap();

        assert_eq!(session.work().definitions.executions, 3);
        assert_eq!(session.work().definition_entries, 3);
        assert_eq!(session.work().definition_records.len(), 3);
        assert!(session.work().definition_records.iter().all(|record| {
            record.binding.bind_invocations == 1
                && record.manifest.build_invocations == 1
                && !record.failed
        }));
    }

    #[test]
    fn definition_keys_ignore_opt_linker_relocation_file_ids_and_order() {
        let original = snapshot(
            &[
                (7, "/old/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/old/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let moved = snapshot(
            &[
                (90, "/new/a.rue", "a.rue", "fn a() {}"),
                (40, "/new/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            40,
        );
        let renamed = snapshot(
            &[
                (90, "/new/lib/a.rue", "lib/a.rue", "fn a() {}"),
                (40, "/new/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            40,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&original).into_result().unwrap();
        let first = session
            .stable_definitions(&CompileOptions {
                linker: LinkerMode::System("x".to_string()),
                opt_level: OptLevel::O2,
                ..CompileOptions::default()
            })
            .unwrap();
        let keys = |set: &BoundDefinitionSet| {
            set.definitions()
                .iter()
                .map(|record| record.stable_key().clone())
                .collect::<Vec<_>>()
        };
        let first_keys = keys(&first);

        session.update(&moved).into_result().unwrap();
        let second = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        assert_eq!(keys(&second), first_keys);
        assert_eq!(session.work().definition_entries_invalidated, 1);

        session.update(&renamed).into_result().unwrap();
        let third = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        assert_ne!(keys(&third), first_keys);
    }

    #[test]
    fn failed_parse_preserves_ids_while_semantic_rejection_issues_none() {
        let valid = base();
        let syntax_bad = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let semantic_bad = snapshot(
            &[(
                7,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { missing_name }",
            )],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CanonicalFrontendSession::new();
        session.update(&valid).into_result().unwrap();
        let ids = session.stable_definitions(&options).unwrap();
        assert!(session.update(&syntax_bad).result().is_err());
        assert!(Arc::ptr_eq(
            &ids,
            &session.stable_definitions(&options).unwrap()
        ));

        session.update(&semantic_bad).into_result().unwrap();
        let first = session.stable_definitions(&options).unwrap_err();
        let second = session.stable_definitions(&options).unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert_eq!(session.work().definitions.executions, 1);
        assert_eq!(session.work().definition_entries, 0);
        assert_eq!(session.work().semantic_records.len(), 1);
        assert!(session.work().semantic_records[0].failed);

        session.update(&valid).into_result().unwrap();
        assert!(session.stable_definitions(&options).is_ok());
        assert_eq!(session.work().definitions.executions, 2);
    }

    #[test]
    fn diagnostic_artifacts_retain_attempt_provenance_and_query_identity() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FrontendDiagnosticSnapshot>();

        let valid = base();
        let syntax_bad = snapshot(&[(7, "/attempt/bad.rue", "bad.rue", "fn main( {")], 7);
        let semantic_bad = snapshot(
            &[(
                7,
                "/attempt/semantic.rue",
                "semantic.rue",
                "fn main() -> i32 { missing_name }",
            )],
            7,
        );
        let warning_source = snapshot(
            &[(
                7,
                "/attempt/warning.rue",
                "warning.rue",
                "fn main() -> i32 { let unused = 1; 0 }",
            )],
            7,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&valid).into_result().unwrap();
        let published = session.published().unwrap().clone();

        let failed = session.update(&syntax_bad);
        let syntax_diagnostics = failed.diagnostics().clone();
        assert_eq!(
            syntax_diagnostics.source().metadata(),
            syntax_bad.metadata()
        );
        assert_eq!(
            syntax_diagnostics.source_revision(),
            syntax_bad.source_revision()
        );
        assert!(!syntax_diagnostics.errors().is_empty());
        assert!(Arc::ptr_eq(session.published().unwrap(), &published));
        assert!(Arc::ptr_eq(
            session
                .diagnostics_for(&syntax_bad, &FrontendDiagnosticStage::Syntax)
                .unwrap(),
            &syntax_diagnostics
        ));

        session.update(&semantic_bad).into_result().unwrap();
        let options = CompileOptions::default();
        session.semantic(&options).unwrap_err();
        let first = session.latest_diagnostics().unwrap().clone();
        let first_fingerprint = first
            .errors()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        session.semantic(&options).unwrap_err();
        let reused = session.latest_diagnostics().unwrap().clone();
        assert!(Arc::ptr_eq(&first, &reused));
        assert_eq!(
            first_fingerprint,
            reused
                .errors()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        let FrontendDiagnosticStage::Semantic(input) = first.stage() else {
            panic!("semantic diagnostic stage");
        };
        assert_eq!(input.opt_level, crate::StableOptLevel::O0);

        session.update(&warning_source).into_result().unwrap();
        session.semantic(&options).unwrap();
        let warning = session.latest_diagnostics().unwrap().clone();
        assert!(warning.is_success());
        assert!(!warning.warnings().is_empty());
        session
            .semantic(&CompileOptions {
                linker: LinkerMode::Internal,
                ..options.clone()
            })
            .unwrap();
        assert!(Arc::ptr_eq(&warning, session.latest_diagnostics().unwrap()));
        session
            .semantic(&CompileOptions {
                opt_level: OptLevel::O1,
                ..options.clone()
            })
            .unwrap();
        let optimized = session.latest_diagnostics().unwrap().clone();
        assert!(!Arc::ptr_eq(&warning, &optimized));
        session
            .semantic(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..options.clone()
            })
            .unwrap();
        let featured = session.latest_diagnostics().unwrap().clone();
        assert!(!Arc::ptr_eq(&warning, &featured));
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != options.target)
            .unwrap();
        session
            .semantic(&CompileOptions {
                target: other_target,
                ..options
            })
            .unwrap();
        assert!(!Arc::ptr_eq(
            &warning,
            session.latest_diagnostics().unwrap()
        ));
        let old = syntax_diagnostics.clone();
        std::thread::spawn(move || {
            assert_eq!(old.source().metadata().root_file_id(), FileId::new(7));
            assert!(!old.errors().is_empty());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn merge_diagnostics_are_memoized_pointer_identically() {
        let duplicate = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() {} fn main() {}")],
            1,
        );
        let mut session = CanonicalFrontendSession::new();
        session.update(&duplicate).into_result().unwrap();
        session.merge().unwrap_err();
        let first = session.latest_diagnostics().unwrap().clone();
        session.merge().unwrap_err();
        let second = session.latest_diagnostics().unwrap().clone();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(first.stage(), FrontendDiagnosticStage::Merge));
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().diagnostic_reuses, 1);
    }
}
