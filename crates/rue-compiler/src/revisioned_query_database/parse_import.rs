use super::*;

// Program assembly is part of the parse/import authority: it publishes the
// exact input views consumed by registered parse families and assembles their
// retained terminals. Keeping it below this module does not add a query family
// or a second runtime.
mod program_assembly;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ModuleQueryKey(pub(super) ModuleId);

impl QueryKey for ModuleQueryKey {
    fn stable_identity(&self) -> String {
        self.0.as_str().to_owned()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.0.hash(hasher);
    }
}

#[derive(Debug, Clone)]
pub(super) struct ParseModuleValue {
    pub(super) result: Result<Arc<ParsedModule>, crate::CompileErrors>,
    pub(super) work: SyntaxWork,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ParseModuleBatchKey {
    pub(super) modules: Arc<[ModuleQueryKey]>,
}

impl QueryKey for ParseModuleBatchKey {
    fn stable_identity(&self) -> String {
        let mut identity = format!("parse-module-frontier;items={}", self.modules.len());
        for key in self.modules.iter() {
            identity.push('\u{1e}');
            identity.push_str(&key.stable_identity());
        }
        identity
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.modules.hash(hasher);
    }
}

#[derive(Debug, Clone)]
pub(super) struct ParseModuleBatchValue(pub(super) Arc<[ParseModuleValue]>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleIndexEntry {
    pub(crate) namespace: DefinitionNamespace,
    pub(crate) kind: DefinitionKind,
    pub(crate) visibility: Option<rue_parser::ast::Visibility>,
    pub(crate) name: Arc<str>,
    /// Language-item identity of this candidate, classified purely from its
    /// module's trusted-standard-library provenance and unqualified spelling
    /// (RUE-1091 / ADR-0066 §4 "visibility/kind metadata needed by
    /// resolution"). It is derived in-module, without enumerating other modules
    /// or bodies, so the index stays `O(module declarations)`.
    pub(crate) language_item: Option<rue_air::LangItem>,
    pub(crate) name_span: rue_span::Span,
    pub(crate) declaration_span: rue_span::Span,
}

impl ModuleIndexEntry {
    /// The position-free lookup fact projected from this index entry. Spans and
    /// the module revision stay in `ModuleIndex`; only the durable resolution
    /// columns cross into `LookupName`.
    pub(super) fn lookup_fact(&self) -> LookupNameFact {
        LookupNameFact {
            namespace: self.namespace,
            kind: self.kind,
            visibility: self.visibility,
            name: self.name.clone(),
            language_item: self.language_item,
        }
    }
}

/// Classify a candidate's language-item identity from parse-level facts alone:
/// its module's trusted-standard-library provenance plus its unqualified name.
/// This never enumerates other modules, keeping `ModuleIndex` construction
/// `O(module declarations)`.
pub(super) fn module_index_entry_language_item(
    module: &ModuleId,
    kind: DefinitionKind,
    name: &str,
) -> Option<rue_air::LangItem> {
    if kind == DefinitionKind::Struct && module.is_trusted_standard_library() {
        rue_air::LangItem::from_standard_library_nominal(module.as_str(), name)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ModuleDefinitionIndices {
    One(usize),
    Many(Vec<usize>),
}

impl ModuleDefinitionIndices {
    pub(super) fn push(&mut self, index: usize) {
        match self {
            Self::One(first) => *self = Self::Many(vec![*first, index]),
            Self::Many(indices) => indices.push(index),
        }
    }

    pub(super) fn as_slice(&self) -> &[usize] {
        match self {
            Self::One(index) => std::slice::from_ref(index),
            Self::Many(indices) => indices,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleIndex {
    pub(crate) revision: ModuleRevision,
    pub(crate) definitions: Arc<[ModuleIndexEntry]>,
    pub(super) definition_partitions:
        BTreeMap<DefinitionNamespace, BTreeMap<Arc<str>, ModuleDefinitionIndices>>,
    pub(crate) imports: Arc<[crate::ImportDirective]>,
    pub(super) import_partitions: BTreeMap<Arc<str>, usize>,
}

impl ModuleIndex {
    fn new(
        revision: ModuleRevision,
        definitions: Arc<[ModuleIndexEntry]>,
        imports: Arc<[crate::ImportDirective]>,
    ) -> Self {
        let mut definition_partitions: BTreeMap<
            DefinitionNamespace,
            BTreeMap<Arc<str>, ModuleDefinitionIndices>,
        > = BTreeMap::new();
        for (index, definition) in definitions.iter().enumerate() {
            definition_partitions
                .entry(definition.namespace)
                .or_default()
                .entry(definition.name.clone())
                .and_modify(|indices| indices.push(index))
                .or_insert(ModuleDefinitionIndices::One(index));
        }
        let mut import_partitions = BTreeMap::new();
        for (index, directive) in imports.iter().enumerate() {
            let normalized: Arc<str> =
                Arc::from(rue_air::normalize_module_path(directive.specifier()));
            import_partitions.entry(normalized).or_insert(index);
        }
        Self {
            revision,
            definitions,
            definition_partitions,
            imports,
            import_partitions,
        }
    }

    /// Source-ordered definitions for one exact semantic lookup key.
    pub(super) fn definitions_for(
        &self,
        namespace: DefinitionNamespace,
        name: &str,
    ) -> impl ExactSizeIterator<Item = &ModuleIndexEntry> {
        self.definition_indices(namespace, name)
            .iter()
            .map(|index| &self.definitions[*index])
    }

    pub(super) fn definition_indices(
        &self,
        namespace: DefinitionNamespace,
        name: &str,
    ) -> &[usize] {
        self.definition_partitions
            .get(&namespace)
            .and_then(|names| names.get(name))
            .map_or(&[], ModuleDefinitionIndices::as_slice)
    }

    /// Every exact definition key in deterministic namespace/name order.
    pub(super) fn definition_keys(&self) -> impl Iterator<Item = (DefinitionNamespace, &Arc<str>)> {
        self.definition_partitions
            .iter()
            .flat_map(|(namespace, names)| names.keys().map(move |name| (*namespace, name)))
    }

    /// Normalize one consulted import path and recover its first source-order
    /// locator without revisiting unrelated directives.
    pub(super) fn normalized_import(
        &self,
        specifier: &str,
    ) -> (String, Option<&crate::ImportDirective>) {
        let normalized = rue_air::normalize_module_path(specifier);
        let directive = self
            .import_partitions
            .get(normalized.as_str())
            .map(|index| &self.imports[*index]);
        (normalized, directive)
    }

    /// Recover one exact parser-owned import occurrence without revisiting
    /// unrelated directives. Module parsing keeps this slice in the same
    /// canonical field order used by `ImportDirective`'s derived ordering.
    pub(super) fn import_occurrence(
        &self,
        occurrence: &crate::ImportOccurrenceKey,
    ) -> Option<&crate::ImportDirective> {
        self.imports
            .binary_search_by(|directive| {
                directive
                    .importer()
                    .cmp(occurrence.importer())
                    .then_with(|| directive.source_offset().cmp(&occurrence.source_offset()))
                    .then_with(|| directive.source_end().cmp(&occurrence.source_end()))
                    .then_with(|| directive.specifier().cmp(occurrence.specifier()))
            })
            .ok()
            .map(|index| &self.imports[index])
    }
}

pub(super) fn new_module_index(
    revision: ModuleRevision,
    definitions: Arc<[ModuleIndexEntry]>,
    imports: Arc<[crate::ImportDirective]>,
) -> ModuleIndex {
    ModuleIndex::new(revision, definitions, imports)
}

/// Current-file-table projection assembled exclusively from ModuleIndex and
/// LookupName terminals. The terminal values remain module-relative and
/// reusable across snapshot renumbering.
#[derive(Debug, Clone)]
pub(crate) struct ProjectedModuleIndex {
    pub(crate) revision: ModuleRevision,
    pub(crate) definitions: Arc<[ModuleIndexEntry]>,
}

#[derive(Debug, Clone)]
pub(super) struct ModuleIndexValue(pub(super) Result<Arc<ModuleIndex>, crate::CompileErrors>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclarationOccurrenceIndex {
    pub(super) capabilities: BTreeMap<
        crate::declaration_candidate::DeclarationCandidateKey,
        crate::declaration_candidate::DeclarationOccurrenceCapability,
    >,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeclarationOccurrenceIndexValue {
    Available(Arc<DeclarationOccurrenceIndex>),
    Failure(crate::declaration_candidate::DeclarationOccurrenceFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeclarationOrderValue {
    Available(Arc<[crate::declaration_candidate::DeclarationCandidateKey]>),
    Failure(crate::declaration_candidate::DeclarationOccurrenceFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct DeclarationShellQueryKey(
    pub(crate) crate::declaration_candidate::DeclarationCandidateKey,
);

impl QueryKey for DeclarationShellQueryKey {
    fn stable_identity(&self) -> String {
        self.0.stable_identity()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.0.hash(hasher);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeclarationShellQueryValue {
    Available(crate::declaration_candidate::DeclarationShellFact),
    Failure(crate::declaration_candidate::DeclarationShellFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct StableDeclarationClassificationQueryKey(pub(super) crate::StableDefinitionKey);

impl QueryKey for StableDeclarationClassificationQueryKey {
    fn stable_identity(&self) -> String {
        let owner = self.0.owner().map_or_else(
            || "-".to_owned(),
            |owner| format!("{:?}:{}:{}", owner.kind(), owner.name().len(), owner.name()),
        );
        format!(
            "{}:{}:{:?}:{:?}:{}:{}:{}",
            self.0.module().as_str().len(),
            self.0.module().as_str(),
            self.0.namespace(),
            self.0.kind(),
            self.0.name().len(),
            self.0.name(),
            owner,
        )
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        // `StableDefinitionKey`'s `Hash` writes its SHA-256 content
        // accelerator over module, namespace, kind, name, and owner — the
        // exact fields the identity above renders.
        self.0.hash(hasher);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StableDeclarationClassificationQueryValue {
    Selected(crate::declaration_candidate::DeclarationCandidateKey),
    Absent,
    Invalid(StableDeclarationClassificationFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StableDeclarationClassificationFailure {
    MalformedStableKey(crate::StableDefinitionKey),
    OccurrencesUnavailable(crate::declaration_candidate::DeclarationOccurrenceFailure),
    Ambiguous(crate::declaration_candidate::DeclarationCandidateKey),
    DuplicateMultiplicity {
        key: crate::declaration_candidate::DeclarationCandidateKey,
        multiplicity: u32,
    },
    ParserCapabilityMismatch(crate::declaration_candidate::DeclarationCandidateKey),
    MultipleAvailable {
        first: crate::declaration_candidate::DeclarationCandidateKey,
        second: crate::declaration_candidate::DeclarationCandidateKey,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct DeclarationBodyPlanQueryKey(
    pub(crate) crate::declaration_candidate::DeclarationCandidateKey,
);

impl QueryKey for DeclarationBodyPlanQueryKey {
    fn stable_identity(&self) -> String {
        self.0.stable_identity()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.0.hash(hasher);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclarationBodyPlanFailure {
    CandidateRirRejected(crate::CompileErrors),
    CandidateUnavailable(crate::declaration_candidate::DeclarationCandidateKey),
    ForeignSymbol(Arc<str>),
    Build(rue_error::ErrorKind),
    Payload(Arc<str>),
    Validation(Arc<str>),
    SpanProjection(Arc<str>),
}

#[derive(Debug, Clone)]
pub(super) enum DeclarationBodyPlanArtifactsValue {
    Available(Arc<crate::canonical_lower::DeclarationBodyPlanArtifacts>),
    Failure(DeclarationBodyPlanFailure),
}

pub(super) fn declaration_body_plan_artifacts_equal(
    left: &DeclarationBodyPlanArtifactsValue,
    right: &DeclarationBodyPlanArtifactsValue,
) -> bool {
    match (left, right) {
        (
            DeclarationBodyPlanArtifactsValue::Available(left),
            DeclarationBodyPlanArtifactsValue::Available(right),
        ) => left.candidate == right.candidate && left.plan.structurally_eq(&right.plan),
        (
            DeclarationBodyPlanArtifactsValue::Failure(left),
            DeclarationBodyPlanArtifactsValue::Failure(right),
        ) => left == right,
        _ => false,
    }
}

pub(super) fn candidate_rir_artifact_failure_errors(
    failure: &DeclarationBodyPlanFailure,
) -> crate::CompileErrors {
    match failure {
        DeclarationBodyPlanFailure::CandidateRirRejected(errors) => errors.clone(),
        DeclarationBodyPlanFailure::Build(kind) => {
            crate::CompileErrors::from(crate::CompileError::without_span(kind.clone()))
        }
        failure => crate::CompileErrors::from(import_input_error(format!(
            "candidate RIR artifact failed: {failure:?}"
        ))),
    }
}

pub(super) fn candidate_rir_semantic_failure(
    failure: &DeclarationBodyPlanFailure,
) -> crate::semantic_query_nucleus::SemanticNucleusFailure {
    crate::durable_comptime::durable_candidate_rir_semantic_failure(failure)
}

pub(super) fn semantic_materialization_failure(
    failure: crate::canonical_lower::BodyPlanMaterializationFailure,
) -> Result<crate::semantic_query_nucleus::SemanticNucleusFailure, QueryAbort> {
    crate::durable_comptime::durable_materialization_semantic_failure(failure)
}

pub(super) fn candidate_rir_composition_failure_error(
    failure: &crate::canonical_lower::DeclarationBodyPlanBuildFailure,
) -> crate::CompileError {
    match failure {
        crate::canonical_lower::DeclarationBodyPlanBuildFailure::Build(error) => {
            crate::CompileError::without_span(crate::canonical_lower::rir_build_error_kind(
                "packed candidate module composition",
                error,
            ))
        }
        failure => import_input_error(format!("candidate module composition failed: {failure:?}")),
    }
}

/// A syntax-resolved static call head retained solely for unused-function
/// warning reachability. `module` is present only when a lexical `@import`
/// alias was resolved by the canonical declaration-import query; otherwise
/// resolution starts in the caller's module. Local value bindings are removed
/// before this value is published.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct WarningStaticCallHead {
    pub(crate) module: Option<ModuleId>,
    pub(crate) components: Arc<[Arc<str>]>,
}

/// Candidate-local warning syntax projected once by the parser-owned module
/// walk. This thin terminal exists only to preserve body-local stamps across
/// sibling edits; it performs no AST traversal of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WarningCallHeadProjectionValue {
    Available(Arc<[crate::parsed_modules::ParsedWarningCallHead]>),
    Failure(WarningBodyReferencesFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct WarningCallHeadProjectionQueryKey(
    pub(crate) crate::declaration_candidate::DeclarationCandidateKey,
);

impl QueryKey for WarningCallHeadProjectionQueryKey {
    fn stable_identity(&self) -> String {
        self.0.stable_identity()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.0.hash(hasher);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WarningBodyReferencesValue {
    Available(Arc<[WarningStaticCallHead]>),
    Failure(WarningBodyReferencesFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct WarningBodyReferencesBatchKey {
    pub(super) bodies: Arc<[crate::body_query::BodyQueryKey]>,
}

impl QueryKey for WarningBodyReferencesBatchKey {
    fn stable_identity(&self) -> String {
        let mut identity = format!(
            "warning-body-reference-frontier;items={}",
            self.bodies.len()
        );
        for key in self.bodies.iter() {
            identity.push('\u{1e}');
            identity.push_str(&key.stable_identity());
        }
        identity
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.bodies.hash(hasher);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WarningBodyReferencesBatchValue {
    pub(crate) values: Arc<[WarningBodyReferencesValue]>,
    /// Complete observed child cones captured while the adaptive batch still
    /// owns every request lease. One retained aggregate therefore protects a
    /// warning frontier wider than the per-body memo cap without copying the
    /// child values or manufacturing a second projection authority.
    pub(super) _retained_children: Arc<rue_query::RetainedPinSet>,
}

/// One frontier item's strongest request lifecycle in an aggregate attempt.
/// Validation may observe a child before a red aggregate evaluator requests it
/// again, so callers reduce duplicate nested attempts by stable child identity
/// instead of counting completion-order-dependent ledger entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrontierChildExecution {
    pub(crate) execution: RequestExecution,
    pub(crate) canceled: bool,
}

pub(super) fn frontier_child_executions<K: QueryKey>(
    attempt: &QueryRequestAttempt<impl Clone + Send + Sync + 'static>,
    family: &str,
    keys: &[K],
) -> Vec<Option<FrontierChildExecution>> {
    pub(super) fn priority(execution: RequestExecution) -> u8 {
        match execution {
            RequestExecution::Computed => 4,
            RequestExecution::Joined => 3,
            RequestExecution::Reused => 2,
            RequestExecution::Aborted => 1,
        }
    }

    keys.iter()
        .map(|key| {
            let identity = key.stable_identity();
            attempt
                .nested_attempts()
                .iter()
                .filter(|nested| {
                    nested.node().family() == family && nested.node().key() == identity
                })
                .map(|nested| FrontierChildExecution {
                    execution: nested.execution(),
                    canceled: matches!(nested.abort(), Some(QueryAbort::Canceled)),
                })
                .max_by_key(|nested| priority(nested.execution))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WarningBodyReferencesFailure {
    ClassificationAbsent(crate::StableDefinitionKey),
    ClassificationInvalid(StableDeclarationClassificationFailure),
    Shell(crate::declaration_candidate::DeclarationShellFailure),
    ParseRejected(ModuleId),
    ParserCapabilityMismatch(crate::declaration_candidate::DeclarationCandidateKey),
    Import(crate::declaration_candidate::DeclarationImportFailure),
    ImportResolution {
        key: crate::declaration_candidate::DeclarationImportSiteKey,
        resolution: crate::CanonicalImportResolution,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum DeclarationShellBatchFailure {
    Query(QueryAbort),
    Stable(crate::declaration_candidate::DeclarationShellFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticNucleusBatchFailure {
    Query(QueryAbort),
    Stable {
        declaration: Option<crate::declaration_candidate::DeclarationCandidateKey>,
        failure: Box<crate::semantic_query_nucleus::SemanticNucleusFailure>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SemanticNucleusProjection {
    pub(crate) declarations: Arc<[crate::DurableDeclarationSemantic]>,
    pub(crate) declaration_index:
        Arc<crate::local_semantic_materialization::SharedDeclarationFactIndex>,
    pub(crate) anonymous_nominals: Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
    pub(crate) dependencies: Arc<[crate::semantic_query_nucleus::SemanticDeclarationDependency]>,
    pub(crate) c_export_roots: Arc<[crate::StableDefinitionKey]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct SemanticNucleusProjectionKey {
    pub(super) modules: Arc<[ModuleId]>,
    pub(super) configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
}

impl QueryKey for SemanticNucleusProjectionKey {
    fn stable_identity(&self) -> String {
        format!("{:?}:{:?}", self.modules, self.configuration)
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.modules.hash(hasher);
        self.configuration.hash(hasher);
    }
}

#[derive(Debug, Clone)]
pub(super) enum SemanticNucleusProjectionValue {
    Available {
        projection: SemanticNucleusProjection,
        _retained_dependencies: Arc<rue_query::RetainedPinSet>,
    },
    Failure {
        declaration: Option<crate::declaration_candidate::DeclarationCandidateKey>,
        failure: Box<crate::semantic_query_nucleus::SemanticNucleusFailure>,
        _retained_dependencies: Arc<rue_query::RetainedPinSet>,
    },
}

impl SemanticNucleusProjectionValue {
    pub(super) fn retained_dependencies(&self) -> &Arc<rue_query::RetainedPinSet> {
        match self {
            Self::Available {
                _retained_dependencies,
                ..
            }
            | Self::Failure {
                _retained_dependencies,
                ..
            } => _retained_dependencies,
        }
    }
}

impl PartialEq for SemanticNucleusProjectionValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Available {
                    projection: left, ..
                },
                Self::Available {
                    projection: right, ..
                },
            ) => left == right,
            (
                Self::Failure {
                    declaration: left_declaration,
                    failure: left_failure,
                    ..
                },
                Self::Failure {
                    declaration: right_declaration,
                    failure: right_failure,
                    ..
                },
            ) => left_declaration == right_declaration && left_failure == right_failure,
            _ => false,
        }
    }
}

impl Eq for SemanticNucleusProjectionValue {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ModuleInputLeaf {
    pub(super) revision: ModuleRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ModuleMetadataLeaf {
    pub(super) module: ModuleId,
    pub(super) file_id: rue_span::FileId,
    pub(super) physical_path: Arc<str>,
}

#[derive(Debug)]
pub(super) struct ModuleInputView {
    pub(super) revision: Revision,
    pub(super) snapshot: SourceSnapshot,
    pub(super) metadata: crate::shared_segments::SharedSegments<ModuleMetadataLeaf>,
    pub(super) stamp_lease: Arc<ModuleInputStampLease>,
}

#[derive(Debug)]
pub(super) struct ModuleInputStampLease {
    pub(super) parent: Option<Arc<ModuleInputStampLease>>,
    pub(super) sources: Arc<[ModuleRevision]>,
    pub(super) metadata: Arc<[ModuleMetadataLeaf]>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RetainedValueStamp {
    pub(super) stamp: u64,
    pub(super) retained_views: usize,
}

#[derive(Debug)]
pub(super) struct ModuleInputStore {
    pub(super) revisions: VecDeque<Arc<ModuleInputView>>,
    /// Exact revision lookup for every retained module view. Recency is not a
    /// lineage authority: an aborted successor remains retained after the
    /// session reselects its committed predecessor.
    pub(super) by_revision: AHashMap<Revision, Arc<ModuleInputView>>,
    /// Runtime selection roots may outlive the ordinary recency window. At
    /// most current and last-good are protected, so retained views are bounded
    /// by the window plus two.
    pub(super) protected_revisions: BTreeSet<Revision>,
    pub(super) retention_limit: usize,
    pub(super) next_stamp: u64,
    pub(super) stamps: AHashMap<ModuleInputLeaf, RetainedValueStamp>,
    pub(super) metadata_stamps: AHashMap<ModuleMetadataLeaf, RetainedValueStamp>,
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct TestImportInputView {
    pub(super) revision: Revision,
    pub(super) graph: crate::CanonicalImportGraph,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct TestImportInputStore {
    pub(super) revisions: VecDeque<Arc<TestImportInputView>>,
    pub(super) next_stamp: u64,
    pub(super) stamps: Vec<(crate::CanonicalImportGraph, u64)>,
}

impl Default for ModuleInputStore {
    fn default() -> Self {
        Self {
            revisions: VecDeque::new(),
            by_revision: AHashMap::new(),
            protected_revisions: BTreeSet::new(),
            retention_limit: MODULE_INPUT_REVISION_RETENTION,
            next_stamp: 1,
            stamps: AHashMap::new(),
            metadata_stamps: AHashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ResolveImportKey {
    pub(super) occurrence: crate::ImportOccurrenceKey,
    pub(super) mode: ImportDemandMode,
}

impl QueryKey for ResolveImportKey {
    fn stable_identity(&self) -> String {
        format!(
            "{}:{:?}:{}..{}:{}",
            self.occurrence.importer(),
            self.mode,
            self.occurrence.source_offset(),
            self.occurrence.source_end(),
            self.occurrence.specifier()
        )
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.occurrence.hash(hasher);
        self.mode.hash(hasher);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolveImportValue {
    pub(super) site_found: bool,
    pub(super) groups: Arc<[Arc<[ImportDiscoveryRequest]>]>,
    pub(super) requests: Arc<[ImportDiscoveryRequest]>,
    pub(super) speculative_blocked: bool,
    pub(super) resolution: Option<crate::CanonicalImportResolution>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct DeclarationImportQueryKey(
    pub(crate) crate::declaration_candidate::DeclarationImportSiteKey,
);

impl QueryKey for DeclarationImportQueryKey {
    fn stable_identity(&self) -> String {
        self.0.stable_identity()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.0.hash(hasher);
    }
}

impl QueryKey for crate::semantic_query_nucleus::SemanticNucleusKey {
    fn stable_identity(&self) -> String {
        self.stable_identity()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        // The derived `Hash` enumerates the variant plus its typed payload.
        self.hash(hasher);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeclarationImportQueryValue {
    Available(crate::CanonicalImportResolution),
    Failure(crate::declaration_candidate::DeclarationImportFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LookupNameKey {
    pub(super) module: ModuleId,
    pub(super) namespace: DefinitionNamespace,
    pub(super) name: Arc<str>,
}

impl QueryKey for LookupNameKey {
    fn stable_identity(&self) -> String {
        format!("{}::{:?}::{}", self.module, self.namespace, self.name)
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.module.hash(hasher);
        self.namespace.hash(hasher);
        self.name.hash(hasher);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LookupNameFact {
    pub(super) namespace: DefinitionNamespace,
    pub(super) kind: DefinitionKind,
    pub(super) visibility: Option<rue_parser::ast::Visibility>,
    pub(super) name: Arc<str>,
    /// Language-item identity carried through from the module name index so
    /// resolution can distinguish a trusted-standard-library nominal from a
    /// same-named user declaration without re-consulting other modules
    /// (RUE-1091 / ADR-0066 §4 kind/visibility metadata).
    pub(super) language_item: Option<rue_air::LangItem>,
}

/// Position-free semantic result retained by `LookupName`.
///
/// Current-epoch spans and the module revision stay in `ModuleIndex`; callers
/// that need source locations rejoin these facts with that locator projection.
/// This lets trivia-only edits preserve downstream semantic stamps without
/// ever serving stale positions to diagnostics or presentation consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LookupNameFailure {
    ModuleIndexUnavailable(ModuleId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LookupNameValue(pub(super) Result<Arc<[LookupNameFact]>, LookupNameFailure>);

/// The canonical §4 name-resolution outcome for one consulted
/// `(module, namespace, name)` key, classified from the retained `LookupName`
/// candidate set. It makes success, absence, ambiguity, and index
/// unavailability first-class typed variants so consumers distinguish them
/// without each re-deriving the classification, while every carried candidate
/// still exposes its visibility, kind, and language-item columns.
///
/// This is registered query machinery for the RUE-1091 exact provider boundary:
/// the production body path does not consume it yet. It is a pure projection of
/// the existing `LookupNameValue`, so it inherits that terminal's stamp — equal
/// candidate sets classify to equal canonical results.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CanonicalNameResolution {
    /// The consulted module's index was unavailable (its parse failed).
    IndexUnavailable(ModuleId),
    /// No candidate matches the consulted `(module, namespace, name)`.
    Absent,
    /// Exactly one candidate matches.
    Unique(LookupNameFact),
    /// More than one candidate shares the consulted key — an ambiguous result.
    Ambiguous(Arc<[LookupNameFact]>),
}

#[cfg(test)]
impl CanonicalNameResolution {
    /// Classify a retained `LookupName` value into its canonical outcome.
    pub(super) fn classify(value: &LookupNameValue) -> Self {
        match &value.0 {
            Err(LookupNameFailure::ModuleIndexUnavailable(module)) => {
                Self::IndexUnavailable(module.clone())
            }
            Ok(facts) => match facts.as_ref() {
                [] => Self::Absent,
                [single] => Self::Unique(single.clone()),
                _ => Self::Ambiguous(Arc::from(facts.as_ref())),
            },
        }
    }

    /// The candidates carried by this resolution, in candidate order. Empty for
    /// `Absent` and `IndexUnavailable`.
    pub(super) fn candidates(&self) -> &[LookupNameFact] {
        match self {
            Self::Unique(fact) => std::slice::from_ref(fact),
            Self::Ambiguous(facts) => facts,
            Self::Absent | Self::IndexUnavailable(_) => &[],
        }
    }

    /// Re-classify keeping only candidates of the requested syntactic kind. A
    /// kind-distinguished view of the same index answer: two lookups over one
    /// candidate set that request different kinds yield distinct canonical
    /// records.
    pub(super) fn of_kind(&self, kind: DefinitionKind) -> Self {
        if let Self::IndexUnavailable(module) = self {
            return Self::IndexUnavailable(module.clone());
        }
        let retained: Vec<LookupNameFact> = self
            .candidates()
            .iter()
            .filter(|fact| fact.kind == kind)
            .cloned()
            .collect();
        Self::from_candidates(retained)
    }

    /// Re-classify keeping only candidates visible under the given public
    /// predicate. `public_only` retains public candidates when accessed across a
    /// visibility boundary; passing `false` retains every candidate (same-domain
    /// access). A visibility-filtered view is a distinct canonical record from
    /// the unfiltered one whenever a private candidate is dropped.
    pub(super) fn visible(&self, public_only: bool) -> Self {
        if let Self::IndexUnavailable(module) = self {
            return Self::IndexUnavailable(module.clone());
        }
        let retained: Vec<LookupNameFact> = self
            .candidates()
            .iter()
            .filter(|fact| {
                !public_only || fact.visibility == Some(rue_parser::ast::Visibility::Public)
            })
            .cloned()
            .collect();
        Self::from_candidates(retained)
    }

    pub(super) fn from_candidates(mut candidates: Vec<LookupNameFact>) -> Self {
        match candidates.len() {
            0 => Self::Absent,
            1 => Self::Unique(candidates.pop().expect("length checked")),
            _ => Self::Ambiguous(Arc::from(candidates)),
        }
    }
}

/// Key for the per-`(module, import-path)` binding-resolution family. One
/// logical terminal per distinct consulted import path in a consulting module,
/// matching ADR-0066 §4 "one logical terminal per … import-path key".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LookupImportKey {
    pub(super) module: ModuleId,
    pub(super) specifier: Arc<str>,
}

impl QueryKey for LookupImportKey {
    fn stable_identity(&self) -> String {
        format!("{}::@import::{}", self.module, self.specifier)
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.module.hash(hasher);
        self.specifier.hash(hasher);
    }
}

/// A resolved import binding. Position-free by construction: it carries only the
/// normalized specifier, never the `@import` call's source offset, so a
/// trivia-only edit that shifts the call preserves the binding's stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedImportBinding {
    pub(super) normalized_specifier: Arc<str>,
    pub(super) target: Option<ModuleId>,
}

/// A deterministic import-binding failure. Absent and rejected are
/// first-class terminal results and dependency edges (ADR-0066 §4 "A failed or
/// absent module binding is a first-class terminal result and dependency
/// edge"): a later edit that makes the path resolve changes this stamp and
/// invalidates exactly the consumers of the failed lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ImportBindingFailure {
    /// No `@import` directive in the consulting module names this specifier.
    Absent,
    /// Exactly one directive names it, but the specifier is malformed (it
    /// normalizes to an empty module path).
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LookupImportValue(pub(super) Result<ResolvedImportBinding, ImportBindingFailure>);

impl LookupImportValue {
    /// Classify one normalized import partition from the consulting module's
    /// index. It never reads another module, matching the §4 requirement to
    /// resolve "only the paths consulted by the lookup".
    ///
    /// Both the requested specifier and every directive specifier are normalized
    /// through the one [`rue_air::normalize_module_path`] authority *before*
    /// matching (RUE-1091 slice 3b, carried from the 3a review). A raw string
    /// match would treat `@import("./dep.rue")` and `@import("dep.rue")` — the
    /// same physical module (path_norm.rs / import_discovery.rs discipline) — as
    /// distinct specifiers, so repeated references to the same physical module
    /// would not share one lookup key and a normalized request against a
    /// `./`-spelled directive would falsely classify as `Absent`. Repeated
    /// source sites are not ambiguous: they consult the same normalized key,
    /// while genuine physical ambiguity remains a result of `ResolveImport`.
    pub(super) fn classify(
        normalized_specifier: String,
        directive: Option<&crate::ImportDirective>,
    ) -> Self {
        let Some(_) = directive else {
            return Self(Err(ImportBindingFailure::Absent));
        };
        if normalized_specifier.is_empty() {
            return Self(Err(ImportBindingFailure::Rejected));
        }
        Self(Ok(ResolvedImportBinding {
            normalized_specifier: Arc::from(normalized_specifier),
            target: None,
        }))
    }
}

impl RetainedCharge for ParseModuleValue {
    fn retained_charge(&self) -> u64 {
        self.result.retained_charge()
    }
}

impl RetainedCharge for ParseModuleBatchValue {
    fn retained_charge(&self) -> u64 {
        self.0.retained_charge()
    }
}

impl RetainedCharge for ModuleIndexEntry {
    fn retained_charge(&self) -> u64 {
        self.name.retained_charge()
    }
}

impl RetainedCharge for ModuleDefinitionIndices {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::One(_) => 0,
            Self::Many(indices) => indices.retained_charge(),
        }
    }
}

impl RetainedCharge for ModuleIndex {
    fn retained_charge(&self) -> u64 {
        self.revision
            .retained_charge()
            .saturating_add(self.definitions.retained_charge())
            .saturating_add(self.definition_partitions.retained_charge())
            .saturating_add(self.imports.retained_charge())
            .saturating_add(self.import_partitions.retained_charge())
    }
}

impl RetainedCharge for ModuleIndexValue {
    fn retained_charge(&self) -> u64 {
        self.0.retained_charge()
    }
}

impl RetainedCharge for DeclarationOccurrenceIndex {
    fn retained_charge(&self) -> u64 {
        self.capabilities.retained_charge()
    }
}

impl RetainedCharge for DeclarationOccurrenceIndexValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(value) => value.retained_charge(),
            Self::Failure(value) => value.retained_charge(),
        }
    }
}

impl RetainedCharge for DeclarationOrderValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(value) => value.retained_charge(),
            Self::Failure(value) => value.retained_charge(),
        }
    }
}

impl RetainedCharge for DeclarationShellQueryValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(value) => value.retained_charge(),
            Self::Failure(value) => value.retained_charge(),
        }
    }
}

impl RetainedCharge for StableDeclarationClassificationQueryValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Selected(value) => value.retained_charge(),
            Self::Invalid(value) => value.retained_charge(),
            Self::Absent => 0,
        }
    }
}

impl RetainedCharge for StableDeclarationClassificationFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::MalformedStableKey(value) => value.retained_charge(),
            Self::OccurrencesUnavailable(value) => value.retained_charge(),
            Self::Ambiguous(value)
            | Self::ParserCapabilityMismatch(value)
            | Self::DuplicateMultiplicity { key: value, .. } => value.retained_charge(),
            Self::MultipleAvailable { first, second } => first
                .retained_charge()
                .saturating_add(second.retained_charge()),
        }
    }
}

macro_rules! query_value_charge {
    ($ty:ty) => {
        impl RetainedCharge for $ty {
            fn retained_charge(&self) -> u64 {
                match self {
                    Self::Available(value) => value.retained_charge(),
                    Self::Failure(value) => value.retained_charge(),
                }
            }
        }
    };
}

query_value_charge!(WarningCallHeadProjectionValue);
query_value_charge!(WarningBodyReferencesValue);

impl RetainedCharge for WarningBodyReferencesBatchValue {
    fn retained_charge(&self) -> u64 {
        self.values.retained_charge()
    }
}

impl RetainedCharge for DeclarationBodyPlanFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::CandidateRirRejected(errors) => errors.retained_charge(),
            Self::CandidateUnavailable(candidate) => candidate.retained_charge(),
            Self::Build(kind) => kind.retained_charge(),
            Self::ForeignSymbol(detail)
            | Self::Payload(detail)
            | Self::Validation(detail)
            | Self::SpanProjection(detail) => detail.retained_charge(),
        }
    }
}

impl RetainedCharge for DeclarationBodyPlanArtifactsValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(artifacts) => artifacts.retained_charge(),
            Self::Failure(failure) => failure.retained_charge(),
        }
    }
}

impl RetainedCharge for WarningStaticCallHead {
    fn retained_charge(&self) -> u64 {
        self.module
            .retained_charge()
            .saturating_add(self.components.retained_charge())
    }
}

impl RetainedCharge for crate::parsed_modules::ParsedWarningCallHead {
    fn retained_charge(&self) -> u64 {
        self.import
            .as_ref()
            .map_or(0, |import| import.specifier.retained_charge())
            .saturating_add(self.components.retained_charge())
    }
}

impl RetainedCharge for WarningBodyReferencesFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::ClassificationAbsent(value) => value.retained_charge(),
            Self::ClassificationInvalid(value) => value.retained_charge(),
            Self::Shell(value) => value.retained_charge(),
            Self::ParseRejected(value) => value.retained_charge(),
            Self::ParserCapabilityMismatch(value) => value.retained_charge(),
            Self::Import(value) => value.retained_charge(),
            Self::ImportResolution { key, resolution } => key
                .retained_charge()
                .saturating_add(resolution.retained_charge()),
        }
    }
}

impl RetainedCharge for SemanticNucleusProjection {
    fn retained_charge(&self) -> u64 {
        self.declarations
            .retained_charge()
            .saturating_add(self.declaration_index.retained_charge())
            .saturating_add(self.anonymous_nominals.retained_charge())
            .saturating_add(self.dependencies.retained_charge())
            .saturating_add(self.c_export_roots.retained_charge())
    }
}

impl RetainedCharge for SemanticNucleusProjectionValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available {
                projection: value, ..
            } => value.retained_charge(),
            Self::Failure {
                declaration,
                failure,
                ..
            } => declaration
                .retained_charge()
                .saturating_add(failure.retained_charge()),
        }
    }
}

impl RetainedCharge for ResolveImportValue {
    fn retained_charge(&self) -> u64 {
        self.groups
            .retained_charge()
            .saturating_add(self.requests.retained_charge())
            .saturating_add(self.resolution.retained_charge())
    }
}

impl RetainedCharge for DeclarationImportQueryValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(value) => value.retained_charge(),
            Self::Failure(value) => value.retained_charge(),
        }
    }
}

impl RetainedCharge for LookupNameFact {
    fn retained_charge(&self) -> u64 {
        self.name.retained_charge()
    }
}

impl RetainedCharge for LookupNameFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::ModuleIndexUnavailable(value) => value.retained_charge(),
        }
    }
}

impl RetainedCharge for LookupNameValue {
    fn retained_charge(&self) -> u64 {
        self.0.retained_charge()
    }
}

impl RetainedCharge for ResolvedImportBinding {
    fn retained_charge(&self) -> u64 {
        self.normalized_specifier
            .retained_charge()
            .saturating_add(self.target.retained_charge())
    }
}

impl RetainedCharge for ImportBindingFailure {
    fn retained_charge(&self) -> u64 {
        0
    }
}

impl RetainedCharge for LookupImportValue {
    fn retained_charge(&self) -> u64 {
        self.0.retained_charge()
    }
}

#[derive(Debug)]
pub(super) struct ImportInputView {
    pub(super) revision: Revision,
    pub(crate) generation: u64,
    pub(super) transition: ImportInputTransition,
    pub(super) context: ImportDiscoveryContext,
    pub(super) sources: SourceRevision,
    pub(super) accepted_reads: AcceptedReadManifest,
    pub(super) ledger: ImportObservationLedger,
    pub(super) accepted_topology_stamp: u64,
    pub(super) accepted_topology: AcceptedImportTopologyValue,
    pub(super) stamp_lease: Arc<ImportInputStampLease>,
}

/// The compiler-owned lineage step that produced one immutable import-input
/// view. The filesystem host sees only [`ImportInputRevision`]; it can neither
/// inspect nor choose these additions. Ordinary discovery uses the exact
/// `HostBatch` parent/delta to extend the retained parse and import plan, while
/// trusted-toolchain continuation keeps its distinct capability protocol.
#[derive(Debug, Clone)]
pub(crate) enum ImportInputTransition {
    Fresh,
    HostBatch {
        parent: ImportInputRevision,
        added: Arc<[ModuleRevision]>,
    },
    TrustedSuccessor {
        parent: ImportInputRevision,
        added: Arc<[ModuleRevision]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct AcceptedImportTopologyFact {
    pub(super) importer: ModuleId,
    pub(super) exact_specifier: Arc<str>,
    pub(super) normalized_specifier: Arc<str>,
    pub(super) outcome: AcceptedImportTopologyOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum AcceptedImportTopologyOutcome {
    Resolved(ModuleId),
    Absent,
    PresentUnreadable,
    DeniedLexical,
    DeniedCanonical,
    InvalidPhysicalType,
    UnstableRead,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum AcceptedImportTopologyValue {
    Full(Arc<[AcceptedImportTopologyFact]>),
    Overlay {
        parent_stamp: u64,
        added: Arc<[AcceptedImportTopologyFact]>,
    },
}

#[derive(Debug)]
pub(super) struct ImportInputStampLease {
    pub(super) parent: Option<Arc<ImportInputStampLease>>,
    pub(super) context: Option<ImportDiscoveryContext>,
    pub(super) provenance: Arc<[AcceptedReadManifestEntry]>,
    pub(super) observations: Arc<[ImportObservation]>,
    pub(super) topology: Option<AcceptedImportTopologyValue>,
}

#[derive(Debug)]
pub(super) struct ImportInputStore {
    pub(super) revisions: VecDeque<Arc<ImportInputView>>,
    /// Matches module-input retention: current and last-good may add at most
    /// two protected revisions beyond the ordinary recency window.
    pub(super) protected_revisions: BTreeSet<Revision>,
    pub(super) next_stamp: u64,
    pub(super) context_stamps: AHashMap<ImportDiscoveryContext, RetainedValueStamp>,
    pub(super) provenance_stamps: AHashMap<AcceptedReadManifestEntry, RetainedValueStamp>,
    pub(super) observation_stamps: AHashMap<ImportObservation, RetainedValueStamp>,
    pub(super) topology_stamps: AHashMap<AcceptedImportTopologyValue, RetainedValueStamp>,
}

impl Default for ImportInputStore {
    fn default() -> Self {
        Self {
            revisions: VecDeque::new(),
            protected_revisions: BTreeSet::new(),
            next_stamp: 1,
            context_stamps: AHashMap::new(),
            provenance_stamps: AHashMap::new(),
            observation_stamps: AHashMap::new(),
            topology_stamps: AHashMap::new(),
        }
    }
}

pub(super) fn module_source_input(module: &ModuleId) -> InputIdentity {
    InputIdentity::new("module-source", Arc::<str>::from(module.as_str()))
}

pub(super) fn module_metadata_input(module: &ModuleId) -> InputIdentity {
    InputIdentity::new("module-metadata", Arc::<str>::from(module.as_str()))
}

pub(super) fn module_metadata_order(
    left: &ModuleMetadataLeaf,
    right: &ModuleMetadataLeaf,
) -> std::cmp::Ordering {
    left.module.cmp(&right.module)
}

pub(super) fn module_metadata_leaf_for_file(
    snapshot: &SourceSnapshot,
    file_id: rue_span::FileId,
) -> ModuleMetadataLeaf {
    let metadata = snapshot.metadata();
    ModuleMetadataLeaf {
        module: metadata
            .module_id(file_id)
            .expect("published file has logical module metadata"),
        file_id,
        physical_path: Arc::from(
            metadata
                .physical_path(file_id)
                .expect("published module has physical-path metadata"),
        ),
    }
}

pub(super) fn module_metadata_leaves(
    snapshot: &SourceSnapshot,
) -> AHashMap<ModuleId, ModuleMetadataLeaf> {
    let metadata = snapshot.metadata();
    metadata
        .file_ids()
        .map(|file_id| {
            let leaf = module_metadata_leaf_for_file(snapshot, file_id);
            (leaf.module.clone(), leaf)
        })
        .collect()
}

pub(super) fn import_context_input() -> InputIdentity {
    InputIdentity::new("import-discovery-context", "current")
}

/// The aggregate accepted-import-topology leaf, keyed by the frontier round
/// that produced it. Per-round keying makes a grown topology a NEW leaf
/// identity instead of a re-stamp of one "current" identity, so a discovery
/// overlay stays strictly additive at the runtime boundary and preserves its
/// validation epoch (ADR-0073). Consumers that want the newest aggregate
/// observe the exact round they were computed against; nothing in the
/// production pipeline observes this leaf today.
pub(super) fn accepted_import_topology_input(frontier_round: u64) -> InputIdentity {
    InputIdentity::new(
        "accepted-import-topology",
        format!("round-{frontier_round}"),
    )
}

pub(super) fn accepted_read_input(module: &ModuleId) -> InputIdentity {
    InputIdentity::new(
        "accepted-read-provenance",
        Arc::<str>::from(module.as_str()),
    )
}

pub(super) fn accepted_import_provenance_input(
    identity: crate::PhysicalFileIdentity,
) -> InputIdentity {
    InputIdentity::new(
        "accepted-import-provenance",
        format!("{}:{}", identity.volume(), identity.file()),
    )
}

pub(super) fn import_observation_input(request: &ImportDiscoveryRequest) -> InputIdentity {
    InputIdentity::new("import-observation", request.runtime_input_key())
}

#[cfg(test)]
pub(super) fn test_import_graph_input() -> InputIdentity {
    accepted_import_topology_input(0)
}

pub(super) fn import_input_error(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

pub(super) fn exact_value_stamp<T: Clone + Eq + Hash>(
    next_stamp: &mut u64,
    values: &mut AHashMap<T, RetainedValueStamp>,
    value: &T,
) -> u64 {
    if let Some(retained) = values.get(value) {
        return retained.stamp;
    }
    let stamp = *next_stamp;
    *next_stamp += 1;
    values.insert(
        value.clone(),
        RetainedValueStamp {
            stamp,
            retained_views: 0,
        },
    );
    stamp
}

#[cfg(test)]
pub(super) fn exact_test_value_stamp<T: Clone + Eq>(
    next_stamp: &mut u64,
    values: &mut Vec<(T, u64)>,
    value: &T,
) -> u64 {
    values
        .iter()
        .find_map(|(candidate, stamp)| (candidate == value).then_some(*stamp))
        .unwrap_or_else(|| {
            let stamp = *next_stamp;
            *next_stamp += 1;
            values.push((value.clone(), stamp));
            stamp
        })
}

pub(super) fn lock_import_store(
    store: &Mutex<ImportInputStore>,
) -> std::sync::MutexGuard<'_, ImportInputStore> {
    store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn retain_stamp_value<T: Eq + Hash>(
    stamps: &mut AHashMap<T, RetainedValueStamp>,
    value: &T,
) {
    stamps
        .get_mut(value)
        .expect("a published input view retains only values with assigned stamps")
        .retained_views += 1;
}

pub(super) fn release_stamp_value<T: Eq + Hash>(
    stamps: &mut AHashMap<T, RetainedValueStamp>,
    value: &T,
) {
    let remove = {
        let retained = stamps
            .get_mut(value)
            .expect("an evicted input view releases an assigned stamp");
        retained.retained_views = retained
            .retained_views
            .checked_sub(1)
            .expect("input stamp view references stay balanced");
        retained.retained_views == 0
    };
    if remove {
        stamps.remove(value);
    }
}

fn index_module_input_view(store: &mut ModuleInputStore, view: &Arc<ModuleInputView>) {
    assert!(
        store
            .by_revision
            .insert(view.revision, view.clone())
            .is_none(),
        "module input revisions are immutable and uniquely numbered"
    );
}

#[cfg(test)]
pub(super) fn retain_module_input_view(store: &mut ModuleInputStore, view: Arc<ModuleInputView>) {
    index_module_input_view(store, &view);
    store.revisions.push_back(view);
    trim_module_input_views(store);
}

pub(super) fn trim_module_input_views(store: &mut ModuleInputStore) {
    loop {
        let evictable_prefix = store.revisions.len().saturating_sub(store.retention_limit);
        let Some(index) = (0..evictable_prefix).find(|&index| {
            !store
                .protected_revisions
                .contains(&store.revisions[index].revision)
        }) else {
            break;
        };
        let evicted = store
            .revisions
            .remove(index)
            .expect("an evictable module input view has a valid index");
        let indexed = store
            .by_revision
            .remove(&evicted.revision)
            .expect("every retained module input view has an exact index entry");
        assert!(Arc::ptr_eq(&evicted, &indexed));
        let lease = evicted.stamp_lease.clone();
        drop(indexed);
        drop(evicted);
        release_orphaned_module_stamp_leases(store, lease);
    }
}

pub(super) fn release_orphaned_module_stamp_leases(
    store: &mut ModuleInputStore,
    lease: Arc<ModuleInputStampLease>,
) {
    let mut next = Some(lease);
    while let Some(lease) = next {
        let Ok(lease) = Arc::try_unwrap(lease) else {
            break;
        };
        for source in lease.sources.iter() {
            release_stamp_value(
                &mut store.stamps,
                &ModuleInputLeaf {
                    revision: source.clone(),
                },
            );
        }
        for metadata in lease.metadata.iter() {
            release_stamp_value(&mut store.metadata_stamps, metadata);
        }
        next = lease.parent;
    }
}

pub(super) fn discard_module_input_view(store: &Mutex<ModuleInputStore>, revision: Revision) {
    let mut store = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let view = store
        .revisions
        .pop_back()
        .expect("a failed runtime publication has a pending module view");
    assert_eq!(view.revision, revision);
    assert!(
        store
            .by_revision
            .get(&revision)
            .is_none_or(|indexed| !Arc::ptr_eq(&view, indexed)),
        "a failed runtime publication never indexes its pending module view"
    );
    let lease = view.stamp_lease.clone();
    drop(view);
    release_orphaned_module_stamp_leases(&mut store, lease);
}

pub(super) fn commit_module_input_view(store: &Mutex<ModuleInputStore>, revision: Revision) {
    let mut store = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        store.revisions.back().map(|view| view.revision),
        Some(revision),
        "runtime publication commits the pending module view"
    );
    let view = store
        .revisions
        .back()
        .expect("a committed module input revision retains its view")
        .clone();
    index_module_input_view(&mut store, &view);
    trim_module_input_views(&mut store);
}

pub(super) fn retain_import_input_view(store: &mut ImportInputStore, view: Arc<ImportInputView>) {
    store.revisions.push_back(view);
    trim_import_input_views(store);
}

pub(super) fn trim_import_input_views(store: &mut ImportInputStore) {
    loop {
        let evictable_prefix = store
            .revisions
            .len()
            .saturating_sub(IMPORT_INPUT_REVISION_RETENTION);
        let Some(index) = (0..evictable_prefix).find(|&index| {
            !store
                .protected_revisions
                .contains(&store.revisions[index].revision)
        }) else {
            break;
        };
        let evicted = store
            .revisions
            .remove(index)
            .expect("an evictable import input view has a valid index");
        let lease = evicted.stamp_lease.clone();
        drop(evicted);
        release_orphaned_import_stamp_leases(store, lease);
    }
}

pub(super) fn release_orphaned_import_stamp_leases(
    store: &mut ImportInputStore,
    lease: Arc<ImportInputStampLease>,
) {
    let mut next = Some(lease);
    while let Some(lease) = next {
        let Ok(lease) = Arc::try_unwrap(lease) else {
            break;
        };
        if let Some(context) = lease.context.as_ref() {
            release_stamp_value(&mut store.context_stamps, context);
        }
        for accepted in lease.provenance.iter() {
            release_stamp_value(&mut store.provenance_stamps, accepted);
        }
        for observation in lease.observations.iter() {
            release_stamp_value(&mut store.observation_stamps, observation);
        }
        if let Some(topology) = lease.topology.as_ref() {
            release_stamp_value(&mut store.topology_stamps, topology);
        }
        next = lease.parent;
    }
}

pub(super) fn accepted_import_topology<'a>(
    observations: impl IntoIterator<Item = &'a ImportObservation>,
    accepted_reads: &AcceptedReadManifest,
    meter: &crate::source_snapshot::IdentityResolutionMeter,
) -> CompileResult<Arc<[AcceptedImportTopologyFact]>> {
    let mut topology = observations
        .into_iter()
        .map(|observation| {
            let request = observation.request();
            let outcome = if let Some(source) = observation.accepted_source() {
                AcceptedImportTopologyOutcome::Resolved(
                    crate::import_discovery::accepted_import_module(source, accepted_reads, meter)?,
                )
            } else {
                use crate::ImportObservationStatus as S;
                match observation.status() {
                    S::Absent => AcceptedImportTopologyOutcome::Absent,
                    S::PresentReadable { .. } => {
                        unreachable!("a readable import observation retains its accepted source")
                    }
                    S::PresentUnreadable(_) => AcceptedImportTopologyOutcome::PresentUnreadable,
                    S::DeniedLexical => AcceptedImportTopologyOutcome::DeniedLexical,
                    S::DeniedCanonical { .. } => AcceptedImportTopologyOutcome::DeniedCanonical,
                    S::InvalidPhysicalType { .. } => {
                        AcceptedImportTopologyOutcome::InvalidPhysicalType
                    }
                    S::UnstableRead(_) => AcceptedImportTopologyOutcome::UnstableRead,
                    S::Cancelled => AcceptedImportTopologyOutcome::Cancelled,
                }
            };
            Ok(AcceptedImportTopologyFact {
                importer: request.occurrence().importer().clone(),
                exact_specifier: Arc::from(request.exact_specifier()),
                normalized_specifier: Arc::from(request.normalized_specifier()),
                outcome,
            })
        })
        .collect::<CompileResult<Vec<_>>>()?;
    topology.sort();
    Ok(topology.into())
}

pub(super) fn pending_occurrence_requests(
    groups: &[Arc<[ImportDiscoveryRequest]>],
    ledger: &ImportObservationLedger,
) -> Vec<ImportDiscoveryRequest> {
    let mut pending = Vec::new();
    for group in groups {
        match crate::import_discovery::exact_import_group_state(group, ledger) {
            crate::import_discovery::ExactImportGroupState::ConclusiveFailure(_)
            | crate::import_discovery::ExactImportGroupState::Cancelled(_)
            | crate::import_discovery::ExactImportGroupState::Resolved(_) => break,
            crate::import_discovery::ExactImportGroupState::Pending => {
                pending.extend(
                    group
                        .iter()
                        .filter(|request| ledger.get(request).is_none())
                        .cloned(),
                );
                break;
            }
            crate::import_discovery::ExactImportGroupState::Skippable => {}
        }
    }
    pending
}

pub(super) fn parse_module_value_equal(left: &ParseModuleValue, right: &ParseModuleValue) -> bool {
    match (&left.result, &right.result) {
        (Ok(left), Ok(right)) => left.revision() == right.revision(),
        (Err(left), Err(right)) => left == right,
        _ => false,
    }
}

pub(super) fn module_index_value_equal(left: &ModuleIndexValue, right: &ModuleIndexValue) -> bool {
    match (&left.0, &right.0) {
        (Ok(left), Ok(right)) => left == right,
        (Err(left), Err(right)) => left == right,
        _ => false,
    }
}

pub(super) fn declaration_occurrence_index_value_equal(
    left: &DeclarationOccurrenceIndexValue,
    right: &DeclarationOccurrenceIndexValue,
) -> bool {
    match (left, right) {
        (
            DeclarationOccurrenceIndexValue::Available(left),
            DeclarationOccurrenceIndexValue::Available(right),
        ) => left == right,
        (
            DeclarationOccurrenceIndexValue::Failure(left),
            DeclarationOccurrenceIndexValue::Failure(right),
        ) => left == right,
        _ => false,
    }
}

#[cfg(test)]
pub(super) fn project_semantic_shell(
    fact: &crate::declaration_candidate::DeclarationShellFact,
    declaration_span: rue_span::Span,
    source_order: u32,
) -> rue_air::SemanticDeclarationShell {
    use crate::declaration_candidate::{
        DeclarationCandidateCategory as C, DeclarationParameterMode as M,
    };
    use rue_air::{StableDefinitionKind as K, StableDefinitionNamespace as N};

    let (namespace, kind) = match fact.key.category {
        C::Function | C::ExternFunction => (N::Value, K::Function),
        C::Struct => (N::Type, K::Struct),
        C::Enum => (N::Type, K::Enum),
        // This is an epoch-local adapter only. The query fact and key remain
        // `ConstCandidate`; no stable definition ID is issued from this value.
        C::ConstCandidate => (N::Value, K::ValueConst),
        C::Destructor => (N::Destructor, K::Destructor),
        C::Method => (N::Method, K::Method),
        C::AssociatedFunction => (N::Method, K::AssociatedFunction),
        C::Test => (N::Test, K::Test),
    };
    let parameter_names = fact
        .parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>()
        .into();
    let parameter_modes = fact
        .parameters
        .iter()
        .map(|parameter| match parameter.mode {
            M::Value => rue_rir::RirParamMode::Normal,
            M::Borrow => rue_rir::RirParamMode::Borrow,
            M::Inout => rue_rir::RirParamMode::Inout,
        })
        .collect::<Vec<_>>()
        .into();
    let parameter_comptime = fact
        .parameters
        .iter()
        .map(|parameter| parameter.is_comptime)
        .collect::<Vec<_>>()
        .into();
    rue_air::SemanticDeclarationShell {
        identity: rue_air::SemanticDeclarationShellIdentity {
            module_path: Arc::from(fact.key.module.as_str()),
            is_trusted_standard_library: fact.key.module.is_trusted_standard_library(),
            namespace,
            kind,
            name: fact.key.name.clone(),
            owner: fact.key.owner.as_ref().map(|owner| owner.name.clone()),
        },
        declaration_span,
        parameter_names,
        parameter_modes,
        parameter_comptime,
        source_order,
        has_self: fact.receiver.is_some(),
        receiver_mode: fact.receiver.map(|mode| match mode {
            M::Value => rue_rir::RirParamMode::Normal,
            M::Borrow => rue_rir::RirParamMode::Borrow,
            M::Inout => rue_rir::RirParamMode::Inout,
        }),
        receiver_is_mut: fact.receiver_is_mut,
        is_generic: fact.is_generic,
        is_public: fact.is_public,
        is_unchecked: fact.is_unchecked,
        is_extern: fact.is_extern,
        signature_fingerprint: fact.signature_fingerprint,
    }
}

pub(super) fn module_input_view(
    store: &Mutex<ModuleInputStore>,
    revision: Revision,
) -> Result<Arc<ModuleInputView>, QueryAbort> {
    store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .revisions
        .iter()
        .find(|view| view.revision == revision)
        .cloned()
        .ok_or(QueryAbort::UnpublishedRevision(revision))
}

pub(crate) fn project_transaction_diagnostics(
    transaction: crate::body_query::BodyTransaction,
    current: Option<&crate::body_query::BodySourceLocator>,
) -> crate::body_query::BodyTransaction {
    use crate::body_query::BodyTransaction;
    match transaction {
        BodyTransaction::DeterministicFailure {
            errors,
            diagnostic_basis,
            references,
            lookup_observations,
        } => {
            let errors = if let Some(basis) = diagnostic_basis.as_ref() {
                let mut coordinates = basis.coordinates.iter();
                let errors = errors.map_spans(|mut span| {
                    let coordinate = coordinates
                        .next()
                        .expect("body diagnostic coordinates match diagnostic spans");
                    match coordinate {
                        crate::body_query::BodyDiagnosticCoordinate::Relative { start, end } => {
                            let Some(current) = current else {
                                return span;
                            };
                            let project =
                                |offset: &crate::body_query::BodyDiagnosticOffset| match offset {
                                    crate::body_query::BodyDiagnosticOffset::Declaration(
                                        offset,
                                    ) => current
                                        .declaration_start
                                        .saturating_add(*offset)
                                        .min(current.declaration_end),
                                    crate::body_query::BodyDiagnosticOffset::Body(offset) => {
                                        current
                                            .body_start
                                            .saturating_add(*offset)
                                            .min(current.body_end)
                                    }
                                };
                            span.file_id = current.file_id;
                            span.start = project(start);
                            span.end = project(end);
                            span
                        }
                        crate::body_query::BodyDiagnosticCoordinate::Preserved {
                            file_id,
                            start,
                            end,
                        } => {
                            span.file_id = *file_id;
                            span.start = *start;
                            span.end = *end;
                            span
                        }
                    }
                });
                errors
            } else {
                errors
            };
            BodyTransaction::DeterministicFailure {
                errors,
                diagnostic_basis,
                references,
                lookup_observations,
            }
        }
        other => other,
    }
}

pub(super) fn body_failure_with_source(
    error: crate::CompileError,
    source: &crate::body_query::BodySourceLocator,
) -> crate::body_query::BodyTransaction {
    let (errors, diagnostic_basis) =
        crate::body_query::relative_body_diagnostics(crate::CompileErrors::from(error), source);
    crate::body_query::BodyTransaction::DeterministicFailure {
        errors,
        diagnostic_basis: Some(diagnostic_basis),
        references: crate::body_query::BodyReferences(Arc::from([])),
        lookup_observations: crate::body_query::BodyLookupObservations::default(),
    }
}

/// What authorizes a successor overlay's added modules (RUE-1112): a frontier
/// batch admits exactly the modules its own accepted observations resolve; a
/// trusted successor admits exactly the capability-verified leaf set.
pub(crate) enum OverlayJustification<'a> {
    BatchAccepted,
    TrustedLeaves(&'a std::collections::BTreeSet<ModuleId>),
}

/// Two-pointer walk over two sequences sorted by `order`: returns the entries of
/// `successor` absent from `parent`, and rejects any parent entry that does not
/// reappear byte-identical (an additive lineage can only append).
pub(super) fn additive_diff<'a, T: Clone + PartialEq + 'a>(
    parent: impl Iterator<Item = &'a T>,
    successor: impl Iterator<Item = &'a T>,
    order: impl Fn(&T, &T) -> std::cmp::Ordering,
    what: &str,
) -> CompileResult<Vec<T>> {
    let mut added = Vec::new();
    let mut parent = parent.peekable();
    let mut successor = successor.peekable();
    loop {
        match (parent.peek(), successor.peek()) {
            (Some(old), Some(new)) => match order(old, new) {
                std::cmp::Ordering::Equal => {
                    if old != new {
                        return Err(import_input_error(format!(
                            "successor overlay mutates a predecessor {what} (the lineage is strictly additive)"
                        )));
                    }
                    parent.next();
                    successor.next();
                }
                std::cmp::Ordering::Greater => {
                    added.push((*new).clone());
                    successor.next();
                }
                std::cmp::Ordering::Less => {
                    return Err(import_input_error(format!(
                        "successor overlay drops a predecessor {what} (the lineage is strictly additive)"
                    )));
                }
            },
            (Some(_), None) => {
                return Err(import_input_error(format!(
                    "successor overlay drops a predecessor {what} (the lineage is strictly additive)"
                )));
            }
            (None, Some(new)) => {
                added.push((*new).clone());
                successor.next();
            }
            (None, None) => return Ok(added),
        }
    }
}

/// Publish module-source leaves for ONLY the newly added modules of a successor
/// overlay; inherited modules keep their leaves through the overlay's parent.
/// The retained module view still records the complete successor snapshot (an
/// `Arc`-backed clone).
pub(super) fn publish_module_inputs_delta(
    store: &Mutex<ModuleInputStore>,
    revision: Revision,
    parent_revision: Revision,
    snapshot: &SourceSnapshot,
    new_sources: &[ModuleRevision],
) -> Vec<(InputIdentity, u64)> {
    let mut store = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let parent_view = store
        .by_revision
        .get(&parent_revision)
        .expect("a module overlay extends its retained exact parent view")
        .clone();
    let parent_lease = parent_view.stamp_lease.clone();
    let mut leaves = Vec::new();
    let mut metadata_leases = Vec::new();
    let metadata_by_module = snapshot
        .direct_appended_file_ids_from(&parent_view.snapshot)
        .map(|file_ids| {
            file_ids
                .into_iter()
                .map(|file_id| module_metadata_leaf_for_file(snapshot, file_id))
                .map(|leaf| (leaf.module.clone(), leaf))
                .collect::<AHashMap<_, _>>()
        })
        .unwrap_or_else(|| module_metadata_leaves(snapshot));
    for source in new_sources {
        let leaf = ModuleInputLeaf {
            revision: source.clone(),
        };
        let metadata = metadata_by_module
            .get(&source.module)
            .expect("new source has metadata")
            .clone();
        let ModuleInputStore {
            next_stamp,
            stamps,
            metadata_stamps,
            ..
        } = &mut *store;
        leaves.push((
            module_source_input(&source.module),
            exact_value_stamp(next_stamp, stamps, &leaf),
        ));
        leaves.push((
            module_metadata_input(&source.module),
            exact_value_stamp(next_stamp, metadata_stamps, &metadata),
        ));
        retain_stamp_value(stamps, &leaf);
        retain_stamp_value(metadata_stamps, &metadata);
        metadata_leases.push(metadata);
    }
    let metadata = crate::shared_segments::SharedSegments::extend(
        &parent_view.metadata,
        metadata_leases.clone(),
    );
    let stamp_lease = Arc::new(ModuleInputStampLease {
        parent: Some(parent_lease),
        sources: new_sources.to_vec().into(),
        metadata: metadata_leases.into(),
    });
    store.revisions.push_back(Arc::new(ModuleInputView {
        revision,
        snapshot: snapshot.clone(),
        metadata,
        stamp_lease,
    }));
    leaves
}

pub(super) fn publish_module_inputs(
    store: &Mutex<ModuleInputStore>,
    revision: Revision,
    snapshot: &SourceSnapshot,
) -> Vec<(InputIdentity, u64)> {
    let mut store = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut leaves = Vec::new();
    let mut sources = Vec::new();
    let mut metadata_leases = Vec::new();
    let metadata_by_module = module_metadata_leaves(snapshot);
    for source in snapshot.source_revision().modules() {
        let leaf = ModuleInputLeaf {
            revision: source.clone(),
        };
        let metadata = metadata_by_module
            .get(&source.module)
            .expect("module source has metadata")
            .clone();
        let ModuleInputStore {
            next_stamp,
            stamps,
            metadata_stamps,
            ..
        } = &mut *store;
        leaves.push((
            module_source_input(&source.module),
            exact_value_stamp(next_stamp, stamps, &leaf),
        ));
        leaves.push((
            module_metadata_input(&source.module),
            exact_value_stamp(next_stamp, metadata_stamps, &metadata),
        ));
        retain_stamp_value(stamps, &leaf);
        retain_stamp_value(metadata_stamps, &metadata);
        sources.push(source.clone());
        metadata_leases.push(metadata);
    }
    let stamp_lease = Arc::new(ModuleInputStampLease {
        parent: None,
        sources: sources.into(),
        metadata: metadata_leases.into(),
    });
    let mut metadata = metadata_by_module.into_values().collect::<Vec<_>>();
    metadata.sort_by(module_metadata_order);
    store.revisions.push_back(Arc::new(ModuleInputView {
        revision,
        snapshot: snapshot.clone(),
        metadata: crate::shared_segments::SharedSegments::flat(
            metadata.into(),
            module_metadata_order,
        ),
        stamp_lease,
    }));
    leaves
}
impl RevisionedQueryDatabase {
    pub(crate) const SOURCE_INPUT: &'static str = "selected-source";

    pub(super) fn compatibility_token_for_import_context(
        &self,
        context: &ImportDiscoveryContext,
    ) -> u64 {
        match self.active_import_context.as_ref() {
            Some(active) if active == context => self.active_compatibility_token,
            Some(_) => context.regime_token(),
            // An ordinary update already published explicit source leaves in
            // this session. Bind that lineage to the first rooted context so
            // retained terminals can validate across the protocol boundary.
            None if self.ordinary_lineage_published => self.active_compatibility_token,
            // A purely rooted session has no lineage to bridge, so begin in
            // the context-derived namespace directly.
            None => context.regime_token(),
        }
    }

    pub(crate) fn current_parse_revision(&self) -> Option<Revision> {
        let terminal = self.parse_selection.current()?;
        let rue_query::QueryOutcome::Success(record) = terminal.outcome() else {
            unreachable!("Parse publishes typed records")
        };
        Some(record.runtime_revision())
    }

    /// The runtime family handle, for successor parsing to observe an exact
    /// adopted predecessor terminal.
    pub(crate) fn parse_family(
        &self,
    ) -> QueryFamily<
        CompatibilityKey<crate::session::ParseQueryKey>,
        crate::session::ParseQueryRecord,
    > {
        self.parse.clone()
    }

    /// The currently selected parse terminal, for identity assertions.
    #[cfg(test)]
    pub(crate) fn selected_parse_terminal(
        &self,
    ) -> Option<Arc<rue_query::QueryTerminal<crate::session::ParseQueryRecord>>> {
        self.parse_selection.current().cloned()
    }

    /// The exact last-good parse terminal retained by the runtime selection
    /// root. Successor parsing adopts this terminal as a true runtime edge.
    pub(crate) fn last_good_parse_terminal(
        &self,
    ) -> Option<&Arc<rue_query::QueryTerminal<crate::session::ParseQueryRecord>>> {
        self.parse_selection.last_good()
    }

    pub(crate) fn last_good_parse_record(&self) -> Option<&crate::session::ParseQueryRecord> {
        let terminal = self.parse_selection.last_good()?;
        match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => Some(record),
            rue_query::QueryOutcome::Failure(_) => None,
        }
    }

    pub(crate) fn request_parse(
        &self,
        revision: Revision,
        origin: AttemptId,
        key: crate::session::ParseQueryKey,
        compute: impl FnOnce(&QueryContext) -> Result<crate::session::ParseQueryRecord, QueryAbort>,
    ) -> Arc<QueryRequestAttempt<crate::session::ParseQueryRecord>> {
        Arc::new(self.runtime.request_with_origin(
            &self.parse,
            revision,
            CompatibilityKey { key },
            CancellationToken::new(),
            Some(origin.0),
            |context| {
                let record = compute(context)?;
                assert!(
                    crate::session::ParseQuery::record_is_consistent(&record),
                    "parse record key does not match its terminal artifact revision"
                );
                let kind = match crate::session::ParseQuery::terminal_kind(&record) {
                    TerminalKind::Success => QueryTerminalKind::Success,
                    TerminalKind::Failure => QueryTerminalKind::Failure,
                };
                Ok(QueryOutput::success(record).with_terminal_kind(kind))
            },
        ))
    }

    /// Revision pin for semantic work. Import discovery republishes the exact
    /// same module leaves together with its observation leaves, so semantic
    /// queries must run on that successor revision when one exists.
    pub(crate) fn current_semantic_revision(&self) -> Option<Revision> {
        self.current_import_revision
            .map(|revision| Revision::new(revision.revision_id, revision.compatibility_token))
            .or({
                #[cfg(test)]
                {
                    self.current_test_import_revision
                }
                #[cfg(not(test))]
                {
                    None
                }
            })
            .or_else(|| self.current_parse_revision())
    }

    #[cfg(test)]
    pub(crate) fn cfg(
        &self,
        revision: Revision,
        key: crate::cfg_query::CfgQueryKey,
        cancellation: CancellationToken,
    ) -> QueryRequestAttempt<crate::cfg_query::CfgValue> {
        self.runtime
            .request_registered(&self.cfgs, revision, key, cancellation)
    }

    /// Request one stable-ordered production root of raw CFG terminals. The
    /// registered evaluator captures the exact child cones before their
    /// request leases are released.
    pub(crate) fn raw_cfg_batch(
        &self,
        revision: Revision,
        keys: Arc<[crate::cfg_query::CfgQueryKey]>,
        cancellation: CancellationToken,
    ) -> (RawCfgBatchKey, QueryRequestAttempt<RawCfgBatchOutput>) {
        let key = RawCfgBatchKey { keys };
        let attempt = self.runtime.request_registered(
            &self.raw_cfg_batches,
            revision,
            key.clone(),
            cancellation,
        );
        // The pre-optimization artifact can be followed immediately by the
        // production optimized-CFG batch in the same session. Publish the raw
        // batch's exact retained child cones as that successor's fallback
        // authority while the request lease is still live, just as the
        // optimized batch publishes its cones for codegen below. Without this
        // handoff, green reuse of a raw compiler.cfg terminal can omit a deep
        // validation leaf (for example compiler.drop-glue) from the successor
        // task even though the raw artifact still owns the complete cone.
        if let Some(terminal) = attempt.terminal() {
            if let rue_query::QueryOutcome::Success(output) = terminal.outcome() {
                self.cfg_collection_root
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .lease = output._retained_children.clone();
            }
        }
        (key, attempt)
    }

    #[cfg(test)]
    pub(crate) fn optimized_cfg(
        &self,
        revision: Revision,
        cfg: crate::cfg_query::CfgQueryKey,
        opt_level: rue_cfg::OptLevel,
        accessor_dependencies: Arc<[crate::cfg_query::CfgQueryKey]>,
        cancellation: CancellationToken,
    ) -> Result<
        (
            crate::cfg_query::OptimizedCfgQueryKey,
            rue_query::QueryRequestAttempt<crate::cfg_query::CfgValue>,
        ),
        QueryAbort,
    > {
        let optimized =
            crate::cfg_query::OptimizedCfgQueryKey::new(cfg, opt_level, accessor_dependencies);
        let attempt = self.runtime.request_registered(
            &self.optimized_cfgs,
            revision,
            optimized.clone(),
            cancellation,
        );
        Ok((optimized, attempt))
    }

    /// Request one stable-ordered production root of optimized CFGs. The
    /// registered evaluator owns structured scheduling; the host only builds
    /// exact keys and projects returned typed values in the same order.
    pub(crate) fn optimized_cfg_batch(
        &self,
        revision: Revision,
        keys: Arc<[crate::cfg_query::OptimizedCfgQueryKey]>,
        roots: Arc<[crate::FunctionInstanceKey]>,
        cancellation: CancellationToken,
    ) -> (
        OptimizedCfgBatchKey,
        QueryRequestAttempt<OptimizedCfgBatchOutput>,
    ) {
        let generation = if keys
            .iter()
            .any(|key| matches!(key.opt_level, rue_cfg::OptLevel::O2 | rue_cfg::OptLevel::O3))
        {
            self.next_optimized_cfg_batch_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        } else {
            0
        };
        let key = OptimizedCfgBatchKey::new(keys, generation, roots);
        let attempt = self.runtime.request_registered(
            &self.optimized_cfg_batches,
            revision,
            key.clone(),
            cancellation,
        );
        // RUE-1576 seam 2: publish the batch's retained child cones for the
        // codegen batch scope that runs next. Best-effort: an unsuccessful
        // batch leaves the previous lease in place.
        if let Some(terminal) = attempt.terminal() {
            if let rue_query::QueryOutcome::Success(output) = terminal.outcome() {
                self.cfg_collection_root
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .lease = output._retained_children.clone();
            }
        }
        (key, attempt)
    }

    /// Request one canonical backend terminal from its registered optimized
    /// CFG. The nested terminal owns every current-domain lowering input, so
    /// this API requires no semantic output, type pool, or live function.
    #[cfg(test)]
    pub(crate) fn codegen_unit(
        &self,
        revision: Revision,
        optimized_cfg_key: crate::cfg_query::OptimizedCfgQueryKey,
        target: rue_target::Target,
        request: rue_codegen::BackendArtifactRequest,
        optimization: rue_cfg::OptLevel,
        cancellation: CancellationToken,
    ) -> Result<QueryRequestAttempt<crate::codegen_query::CodegenUnitValue>, QueryAbort> {
        Ok(self.runtime.request_registered(
            &self.codegen_units,
            revision,
            crate::codegen_query::CodegenUnitQueryKey::new(
                optimized_cfg_key,
                target,
                request,
                optimization,
            ),
            cancellation,
        ))
    }

    /// Request one stable-ordered production root of CodegenUnits through the
    /// runtime's registered structured scheduler.
    pub(crate) fn codegen_unit_batch(
        &self,
        revision: Revision,
        keys: Arc<[crate::codegen_query::CodegenUnitQueryKey]>,
        cancellation: CancellationToken,
    ) -> (
        CodegenUnitBatchKey,
        QueryRequestAttempt<CodegenUnitBatchOutput>,
    ) {
        let key = CodegenUnitBatchKey { keys };
        let attempt = self.runtime.request_registered(
            &self.codegen_unit_batches,
            revision,
            key.clone(),
            cancellation,
        );
        // RUE-1576: publish the batch's retained child cones for the
        // object-projection scope that runs next. Best-effort: an unsuccessful
        // batch leaves the previous lease in place.
        if let Some(terminal) = attempt.terminal() {
            if let rue_query::QueryOutcome::Success(output) = terminal.outcome() {
                self.codegen_collection_root
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .lease = output._retained_children.clone();
            }
        }
        (key, attempt)
    }

    /// Request one stable-ordered production root of retained per-unit object
    /// projections through the runtime's registered structured scheduler.
    pub(crate) fn object_projection_batch(
        &self,
        revision: Revision,
        keys: Arc<[crate::object_query::ObjectProjectionQueryKey]>,
        cancellation: CancellationToken,
    ) -> (
        ObjectProjectionBatchKey,
        QueryRequestAttempt<ObjectProjectionBatchOutput>,
    ) {
        let key = ObjectProjectionBatchKey { keys };
        let attempt = self.runtime.request_registered(
            &self.object_projection_batches,
            revision,
            key.clone(),
            cancellation,
        );
        (key, attempt)
    }

    /// Start an unpublished backend-root handoff. Every terminal retained into
    /// this candidate is acquired while its request attempt still owns the
    /// result lease, closing the birth-to-publication eviction window.
    pub(crate) fn begin_backend_root(&self) -> BackendRootCandidate {
        BackendRootCandidate::default()
    }

    /// Retain the registered optimized-CFG batch while its request lease is
    /// live. The batch value encapsulates pins acquired for every observed
    /// child while the evaluator's leases were live, so this one root pin
    /// protects the exact child cones until codegen acquires successor pins.
    pub(crate) fn retain_backend_optimized_cfg_batch(
        &self,
        candidate: &mut BackendRootCandidate,
        key: &OptimizedCfgBatchKey,
        terminal: &Arc<rue_query::QueryTerminal<OptimizedCfgBatchOutput>>,
    ) {
        let pin = self
            .optimized_cfg_batches
            .pin_terminal(terminal)
            .expect("optimized-CFG batch result belongs to the registered family");
        candidate.lease.lease(pin);
        let (non_reusable, unreachable) = match terminal.outcome() {
            rue_query::QueryOutcome::Success(output) => (
                output
                    .non_reusable_functions
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                output
                    .unreachable_functions
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>(),
            ),
            rue_query::QueryOutcome::Failure(_) => (
                std::collections::BTreeSet::new(),
                std::collections::BTreeSet::new(),
            ),
        };
        for optimized in key.keys.iter().filter(|optimized| {
            !non_reusable.contains(&optimized.cfg.function)
                && !unreachable.contains(&optimized.cfg.function)
        }) {
            for cfg_key in
                std::iter::once(&optimized.cfg).chain(optimized.accessor_dependencies.iter())
            {
                candidate.cfg_keys.insert(cfg_key.clone());
            }
            candidate.functions.insert(optimized.cfg.function.clone());
        }
        candidate.optimized_cfg_terminals = key
            .keys
            .iter()
            .filter(|optimized| !unreachable.contains(&optimized.cfg.function))
            .count();
    }

    /// Retain the registered CodegenUnit batch from result birth until the
    /// backend publication handoff installs its exact transitive cone. Its
    /// encapsulated child pins prevent bounded child memo retention from
    /// recomputing wide roots during that handoff.
    pub(crate) fn retain_backend_codegen_batch(
        &self,
        candidate: &mut BackendRootCandidate,
        key: &CodegenUnitBatchKey,
        terminal: &Arc<rue_query::QueryTerminal<CodegenUnitBatchOutput>>,
    ) {
        let pin = self
            .codegen_unit_batches
            .pin_terminal(terminal)
            .expect("codegen batch result belongs to the registered family");
        candidate.lease.lease(pin);
        candidate.codegen_unit_terminals = key.keys.len();
    }

    /// Retain the registered object batch from result birth until publication
    /// installs the exact object-to-codegen dependency cones.
    pub(crate) fn retain_backend_object_projection_batch(
        &self,
        candidate: &mut BackendRootCandidate,
        key: &ObjectProjectionBatchKey,
        terminal: &Arc<rue_query::QueryTerminal<ObjectProjectionBatchOutput>>,
    ) {
        let pin = self
            .object_projection_batches
            .pin_terminal(terminal)
            .expect("object projection batch result belongs to the registered family");
        candidate.lease.lease(pin);
        candidate.object_projection_terminals = key.keys.len();
    }

    /// Publish the full transitive query cone behind a successful backend
    /// collection. Direct candidate pins bridge the host collection into this
    /// registered request; its query context then observes and atomically hands
    /// off every exact validation dependency, not just the three backend-family
    /// terminals at the top of the graph.
    pub(crate) fn publish_backend_root(
        &self,
        revision: Revision,
        candidate: BackendRootCandidate,
        input: BackendRootPublicationInput,
    ) -> Result<(), QueryAbort> {
        // A root-publication request is transactional through its handoff
        // callbacks. Serialize only these short, whole-program swaps so a
        // canceled publication always rolls back the root it installed; all
        // per-function CFG and codegen queries remain independently parallel.
        let _publication = self.backend_root_publication_gate.enter();
        let epoch = self
            .next_backend_root_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = BackendRootPublicationKey {
            epoch,
            input,
            functions: candidate.functions.iter().cloned().collect(),
            cfg_terminals: candidate.cfg_keys.len(),
            optimized_cfg_terminals: candidate.optimized_cfg_terminals,
            codegen_unit_terminals: candidate.codegen_unit_terminals,
        };
        let attempt = self.runtime.request_registered(
            &self.backend_root_publications,
            revision,
            key,
            CancellationToken::new(),
        );
        let terminal = attempt.into_result()?;
        let rue_query::QueryOutcome::Success(published) = terminal.outcome() else {
            unreachable!("backend-root publication uses a typed terminal")
        };
        assert!(
            *published,
            "a host-validated successful CodegenUnit collection must publish successfully"
        );
        drop(candidate);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn backend_root_metrics_for_test(&self) -> PublishedBackendRootMetrics {
        let root = self
            .backend_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _retained_pins = root.lease.len();
        PublishedBackendRootMetrics {
            functions: root.functions.len(),
            cfg_terminals: root.cfg_terminals,
            optimized_cfg_terminals: root.optimized_cfg_terminals,
            codegen_unit_terminals: root.codegen_unit_terminals,
            object_projection_terminals: root.object_projection_terminals,
            publications: root.publications,
            additions: root.additions,
            deletions: root.deletions,
        }
    }

    #[cfg(test)]
    pub(crate) fn raw_cfg_handoff_matches_terminal_for_test(
        &self,
        terminal: &Arc<rue_query::QueryTerminal<RawCfgBatchOutput>>,
    ) -> bool {
        let rue_query::QueryOutcome::Success(output) = terminal.outcome() else {
            return false;
        };
        let root = self
            .cfg_collection_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::ptr_eq(&root.lease, &output._retained_children)
    }

    #[cfg(test)]
    pub(crate) fn backend_cfg_key_is_retained_for_test(
        &self,
        key: &crate::cfg_query::CfgQueryKey,
    ) -> bool {
        self.cfgs.contains_retained_key(key)
    }

    #[cfg(test)]
    pub(crate) fn object_projection_key_is_retained_for_test(
        &self,
        key: &crate::object_query::ObjectProjectionQueryKey,
    ) -> bool {
        self.object_projections.contains_retained_key(key)
    }

    #[cfg(test)]
    pub(crate) fn query_evictions_for_test(&self) -> u64 {
        self.runtime.metrics().evictions
    }

    /// Publish an immutable, revisioned import authority for lower-layer
    /// tests. The graph is a discovered one; it enters here as an input leaf
    /// rather than an out-of-band evaluator read, so changing it invalidates
    /// exactly the declaration import queries which observed it.
    #[cfg(test)]
    pub(super) fn adopt_test_import_graph_for_revision(
        &mut self,
        parse_revision: Revision,
        graph: crate::CanonicalImportGraph,
    ) {
        let snapshot = self
            .module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revisions
            .iter()
            .find(|view| view.revision == parse_revision)
            .expect("selected parse retains its module input view")
            .snapshot
            .clone();
        let revision = Revision::new(self.next_revision, self.active_compatibility_token);
        self.next_revision += 1;
        let stamp = {
            let mut store = self
                .test_import_store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let TestImportInputStore {
                next_stamp, stamps, ..
            } = &mut *store;
            exact_test_value_stamp(next_stamp, stamps, &graph)
        };
        let mut leaves = vec![(test_import_graph_input(), stamp)];
        leaves.extend(publish_module_inputs(
            &self.module_store,
            revision,
            &snapshot,
        ));
        self.runtime
            .publish_revision(revision, leaves)
            .expect("test import revisions are immutable and uniquely numbered");
        commit_module_input_view(&self.module_store, revision);
        let mut store = self
            .test_import_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store
            .revisions
            .push_back(Arc::new(TestImportInputView { revision, graph }));
        while store.revisions.len() > IMPORT_INPUT_REVISION_RETENTION {
            store.revisions.pop_front();
        }
        let retained = store.revisions.iter().cloned().collect::<Vec<_>>();
        store
            .stamps
            .retain(|(graph, _)| retained.iter().any(|view| &view.graph == graph));
        self.current_test_import_revision = Some(revision);
    }

    /// Request every query-owned declaration shell for the selected parsed
    /// program and attach only the current revision's diagnostic locators.
    #[cfg(test)]
    pub(crate) fn projected_declaration_shells(
        &self,
        revision: Revision,
        program: &crate::canonical_merge::CanonicalMergedAst,
        cancellation: CancellationToken,
    ) -> Result<Vec<rue_air::SemanticDeclarationShell>, DeclarationShellBatchFailure> {
        self.projected_declaration_shells_for_modules(revision, program.modules(), cancellation)
    }

    #[cfg(test)]
    pub(crate) fn projected_declaration_shells_for_modules(
        &self,
        revision: Revision,
        modules: &[Arc<crate::parsed_modules::ParsedModule>],
        cancellation: CancellationToken,
    ) -> Result<Vec<rue_air::SemanticDeclarationShell>, DeclarationShellBatchFailure> {
        let mut shells = Vec::new();
        let mut index_pins = Vec::new();
        let mut shell_pins = Vec::new();
        for module in modules {
            if cancellation.is_canceled() {
                return Err(DeclarationShellBatchFailure::Query(QueryAbort::Canceled));
            }
            let indexed_attempt = self.runtime.request_registered(
                &self.declaration_occurrence_indexes,
                revision,
                ModuleQueryKey(module.module_id().clone()),
                cancellation.clone(),
            );
            let indexed_terminal = indexed_attempt
                .into_result()
                .map_err(DeclarationShellBatchFailure::Query)?;
            index_pins.push(
                self.declaration_occurrence_indexes
                    .pin_terminal(&indexed_terminal)
                    .expect("occurrence terminal belongs to its family"),
            );
            let rue_query::QueryOutcome::Success(indexed) = indexed_terminal.outcome() else {
                unreachable!("DeclarationOccurrenceIndex publishes typed values")
            };
            let index = match indexed {
                DeclarationOccurrenceIndexValue::Available(index) => index,
                DeclarationOccurrenceIndexValue::Failure(failure) => {
                    return Err(DeclarationShellBatchFailure::Stable(
                        crate::declaration_candidate::DeclarationShellFailure::OccurrencesUnavailable(
                            failure.clone(),
                        ),
                    ));
                }
            };
            for capability in index.capabilities.values() {
                let key = capability.key();
                let crate::declaration_candidate::DeclarationOccurrenceCapability::Exact { .. } =
                    capability
                else {
                    return Err(DeclarationShellBatchFailure::Stable(
                        crate::declaration_candidate::DeclarationShellFailure::Ambiguous(
                            key.clone(),
                        ),
                    ));
                };
                if cancellation.is_canceled() {
                    return Err(DeclarationShellBatchFailure::Query(QueryAbort::Canceled));
                }
                let attempt = self.runtime.request_registered(
                    &self.declaration_shells,
                    revision,
                    DeclarationShellQueryKey(key.clone()),
                    cancellation.clone(),
                );
                let terminal = attempt
                    .into_result()
                    .map_err(DeclarationShellBatchFailure::Query)?;
                shell_pins.push(
                    self.declaration_shells
                        .pin_terminal(&terminal)
                        .expect("shell terminal belongs to its family"),
                );
                let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                    unreachable!("DeclarationShell publishes typed values")
                };
                let fact = match value {
                    DeclarationShellQueryValue::Available(fact) => fact,
                    DeclarationShellQueryValue::Failure(failure) => {
                        return Err(DeclarationShellBatchFailure::Stable(failure.clone()));
                    }
                };
                let Some(locator) = module.definitions().declaration_locator(&fact.key) else {
                    return Err(DeclarationShellBatchFailure::Stable(
                        crate::declaration_candidate::DeclarationShellFailure::ParserCapabilityMismatch(
                            fact.key.clone(),
                        ),
                    ));
                };
                shells.push(project_semantic_shell(
                    fact,
                    locator.declaration_span,
                    locator.source_order,
                ));
            }
        }
        if cancellation.is_canceled() {
            return Err(DeclarationShellBatchFailure::Query(QueryAbort::Canceled));
        }
        Ok(shells)
    }
}

// ---------------------------------------------------------------------------
// Declared test candidates (ADR-0083 §1)
//
// A candidate is a file the build system declared but the compiler may never
// have read: an orphaned `parser_tests.rue` is exactly the case the inventory
// exists to find. Its bytes therefore cannot arrive through the module input
// store — minting a module leaf for a candidate would put it in the closure
// this scan is trying to prove it is OUTSIDE of.
//
// So candidates get their own input leaves, in their own revision namespace,
// published by the same host that performs import reads and under the same
// read policy. Two properties follow, and both are the point:
//
//   * the scan is revision-keyed, so editing one candidate re-scans exactly
//     that candidate; and
//   * publishing candidates cannot perturb the import namespace's certificate
//     lineage, because candidate acquisition is an addition to the request
//     rather than a new observation of the program's sources.
// ---------------------------------------------------------------------------

/// The key of one parse-only candidate scan: the candidate's identity under the
/// read regime that acquired it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct TestCandidateScanKey(pub(super) crate::test_candidates::TestCandidateIdentity);

impl QueryKey for TestCandidateScanKey {
    fn stable_identity(&self) -> String {
        self.0.runtime_input_key().to_string()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.0.hash(hasher);
    }
}

/// The scan's published answer. Deliberately tiny: this query parses and counts,
/// and anything richer would be a second semantic path into candidate files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TestCandidateScanValue(pub(crate) crate::test_candidates::TestCandidateScan);

impl RetainedCharge for TestCandidateScanValue {
    fn retained_charge(&self) -> u64 {
        // Two scalars, published by value: the scan's whole answer is
        // `{ tests, parse_failed }`.
        (std::mem::size_of::<Self>() as u64).saturating_add(1)
    }
}

pub(super) fn test_candidate_scan_value_equal(
    left: &TestCandidateScanValue,
    right: &TestCandidateScanValue,
) -> bool {
    left == right
}

/// One candidate's acquired bytes or typed non-read outcome, as an input leaf.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct TestCandidateLeaf {
    pub(super) identity: crate::test_candidates::TestCandidateIdentity,
    pub(super) outcome: crate::TestCandidateOutcome,
}

#[derive(Debug)]
pub(super) struct TestCandidateInputView {
    pub(super) revision: Revision,
    pub(super) leaves: AHashMap<Arc<str>, TestCandidateLeaf>,
}

#[derive(Debug, Default)]
pub(super) struct TestCandidateInputStore {
    pub(super) revisions: VecDeque<Arc<TestCandidateInputView>>,
    pub(super) next_stamp: u64,
    pub(super) stamps: AHashMap<TestCandidateLeaf, u64>,
}

/// Retained candidate input views. Candidate acquisition happens once per
/// reported request, so a short window is enough to keep the previous inventory
/// available for red/green validation of an unchanged candidate.
pub(super) const TEST_CANDIDATE_INPUT_REVISION_RETENTION: usize = 4;

pub(super) fn test_candidate_input(
    identity: &crate::test_candidates::TestCandidateIdentity,
) -> InputIdentity {
    InputIdentity::new("test-candidate", identity.runtime_input_key())
}

pub(super) fn test_candidate_view(
    store: &Mutex<TestCandidateInputStore>,
    revision: Revision,
) -> Result<Arc<TestCandidateInputView>, QueryAbort> {
    store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .revisions
        .iter()
        .find(|view| view.revision == revision)
        .cloned()
        .ok_or(QueryAbort::UnpublishedRevision(revision))
}

impl RevisionedQueryDatabase {
    /// Publish one declared candidate inventory as an immutable input revision
    /// and return the revision its scan queries pin.
    ///
    /// The revision lives in the inventory's own regime namespace
    /// (`TestCandidateInventory::regime_token`), so it neither extends nor
    /// resets the import namespace's epoch head.
    pub(crate) fn publish_test_candidate_inputs(
        &mut self,
        inventory: &crate::TestCandidateInventory,
    ) -> CompileResult<Revision> {
        let revision = Revision::new(self.next_revision, inventory.regime_token());
        self.next_revision += 1;

        let mut leaves = Vec::with_capacity(inventory.len());
        let mut published = AHashMap::with_capacity(inventory.len());
        {
            let mut store = self
                .test_candidate_store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for candidate in inventory.candidates() {
                let leaf = TestCandidateLeaf {
                    identity: candidate.identity().clone(),
                    outcome: candidate.outcome().clone(),
                };
                let TestCandidateInputStore {
                    next_stamp, stamps, ..
                } = &mut *store;
                let stamp = *stamps.entry(leaf.clone()).or_insert_with(|| {
                    let stamp = *next_stamp;
                    *next_stamp += 1;
                    stamp
                });
                leaves.push((test_candidate_input(&leaf.identity), stamp));
                published.insert(leaf.identity.runtime_input_key(), leaf);
            }
        }

        // An empty inventory still publishes: a revision with no leaves is a
        // valid immutable view, and the caller's report is then trivially empty
        // rather than a special case.
        self.runtime
            .publish_revision(revision, leaves)
            .map_err(|error| {
                import_input_error(format!("cannot publish test-candidate revision: {error:?}"))
            })?;

        let mut store = self
            .test_candidate_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store.revisions.push_back(Arc::new(TestCandidateInputView {
            revision,
            leaves: published,
        }));
        while store.revisions.len() > TEST_CANDIDATE_INPUT_REVISION_RETENTION {
            store.revisions.pop_front();
        }
        Ok(revision)
    }

    /// Request one candidate's parse-only scan against a published candidate
    /// revision.
    pub(crate) fn test_candidate_scan(
        &self,
        revision: Revision,
        identity: crate::test_candidates::TestCandidateIdentity,
    ) -> QueryRequestAttempt<TestCandidateScanValue> {
        self.runtime.request_registered(
            &self.test_candidate_scans,
            revision,
            TestCandidateScanKey(identity),
            CancellationToken::new(),
        )
    }
}
