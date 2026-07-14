//! Stable, resolution-independent import sites.
//!
//! Source discovery and diagnostic scheduling intentionally remain outside
//! this module. It extracts valid `@import("...")` sites from an already-lowered
//! program and resolves them only against an explicit, immutable source
//! snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

#[cfg(test)]
use lasso::ThreadedRodeo;
use rue_air::{DirResolution, ModulePath, normalize_module_path};
use rue_error::{CompileError, CompileResult, ErrorKind};
#[cfg(test)]
use rue_rir::{InstData, Rir};

#[cfg(test)]
use crate::SourceMetadata;
use crate::{
    CompileOptions, ModuleId, ModuleResolutionInputs, SemanticInputDescriptor, StableLinkerInput,
    StableOptLevel,
};

/// One valid import call, identified independently of request-local file IDs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportDirective {
    importer: ModuleId,
    source_offset: u32,
    source_end: u32,
    specifier: Arc<str>,
}

impl ImportDirective {
    pub(crate) fn new(
        importer: ModuleId,
        source_offset: u32,
        source_end: u32,
        specifier: Arc<str>,
    ) -> Self {
        Self {
            importer,
            source_offset,
            source_end,
            specifier,
        }
    }
    /// Canonical logical identity of the module containing this call.
    pub fn importer(&self) -> &ModuleId {
        &self.importer
    }

    /// Byte offset of the `@import` call in its source module.
    pub fn source_offset(&self) -> u32 {
        self.source_offset
    }
    pub fn source_end(&self) -> u32 {
        self.source_end
    }

    /// Exact decoded string-literal value passed to `@import`.
    pub fn specifier(&self) -> &str {
        &self.specifier
    }
}

/// Canonically ordered import sites from one lowered source snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ImportDirectives(Arc<[ImportDirective]>);

impl ImportDirectives {
    pub(crate) fn from_records(mut records: Vec<ImportDirective>) -> Self {
        records.sort();
        Self(records.into())
    }
    /// Import sites ordered by logical module, source offset, then specifier.
    pub fn as_slice(&self) -> &[ImportDirective] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ImportDirective> {
        self.0.iter()
    }
}

/// Resolution outcome for one canonical import directive.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImportResolution {
    /// Exactly one loaded module matched the directive.
    Resolved(ModuleId),
    /// No loaded module matched the directive.
    Missing,
    /// Both the file module and directory facade exist at the nearest base.
    Ambiguous {
        file_module: ModuleId,
        directory_module: ModuleId,
    },
}

/// One resolved import site. Repeated sites remain repeated edges.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportEdge {
    directive_index: u32,
    target: ModuleId,
}

impl ImportEdge {
    pub fn directive_index(&self) -> usize {
        self.directive_index as usize
    }

    pub fn target(&self) -> &ModuleId {
        &self.target
    }
}

/// Immutable canonical import topology for a loaded source snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg(test)]
pub(crate) struct ImportGraph {
    root: ModuleId,
    directives: ImportDirectives,
    resolutions: Arc<[ImportResolution]>,
    edges: Arc<[ImportEdge]>,
}

/// Occurrence-independent outcome in a durable import graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalImportResolution {
    Resolved(ModuleId),
    Missing,
    Ambiguous {
        file_module: ModuleId,
        directory_module: ModuleId,
    },
}

/// One durable resolved-import value, with no source position or physical path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalImportRecord {
    importer: ModuleId,
    normalized_specifier: Arc<str>,
    resolution: CanonicalImportResolution,
}

impl CanonicalImportRecord {
    /// Create a durable record, canonicalizing the import spelling lexically.
    pub fn new(
        importer: ModuleId,
        specifier: impl AsRef<str>,
        resolution: CanonicalImportResolution,
    ) -> Self {
        Self {
            importer,
            normalized_specifier: Arc::from(normalize_module_path(specifier.as_ref())),
            resolution,
        }
    }

    pub fn importer(&self) -> &ModuleId {
        &self.importer
    }

    pub fn normalized_specifier(&self) -> &str {
        &self.normalized_specifier
    }

    pub fn resolution(&self) -> &CanonicalImportResolution {
        &self.resolution
    }
}

/// Canonical resolved topology, independent of import-site occurrences.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalImportGraph {
    root: ModuleId,
    records: Arc<[CanonicalImportRecord]>,
}

impl CanonicalImportGraph {
    pub(crate) fn from_discovery_records(
        root: ModuleId,
        mut records: Vec<CanonicalImportRecord>,
    ) -> Self {
        records.sort();
        records.dedup();
        Self {
            root,
            records: records.into(),
        }
    }

    pub fn root(&self) -> &ModuleId {
        &self.root
    }

    /// Records sorted by importer, normalized spelling, then outcome.
    pub fn records(&self) -> &[CanonicalImportRecord] {
        &self.records
    }

    /// Validate and canonicalize an explicitly supplied graph, failing closed.
    ///
    /// Cycles are legal Rue topology and do not make construction fail. Query
    /// [`validate_canonical_import_graph`] to obtain their stable components.
    pub fn from_supplied(
        root: ModuleId,
        records: Vec<CanonicalImportRecord>,
        inputs: &ModuleResolutionInputs,
    ) -> Result<Self, CanonicalImportGraphValidation> {
        let validation = validate_records(&root, &records, inputs);
        if !validation.is_valid() {
            return Err(validation);
        }
        let mut records = records;
        records.sort();
        records.dedup();
        Ok(Self {
            root,
            records: records.into(),
        })
    }
}

/// Stable malformed-topology finding for a canonical import graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalImportGraphProblem {
    RootMismatch {
        expected: ModuleId,
        found: ModuleId,
    },
    RootNotInModuleSet(ModuleId),
    ForeignImporter {
        importer: ModuleId,
        normalized_specifier: Arc<str>,
    },
    ForeignResolvedTarget {
        importer: ModuleId,
        normalized_specifier: Arc<str>,
        target: ModuleId,
    },
    MissingResolution {
        importer: ModuleId,
        normalized_specifier: Arc<str>,
    },
    AmbiguousResolution {
        importer: ModuleId,
        normalized_specifier: Arc<str>,
        file_module: ModuleId,
        directory_module: ModuleId,
    },
    DuplicateRecord(CanonicalImportRecord),
    ConflictingCanonicalKey {
        importer: ModuleId,
        normalized_specifier: Arc<str>,
        resolutions: Arc<[CanonicalImportResolution]>,
    },
}

/// One deterministic strongly connected component in legal cyclic topology.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalImportCycle {
    modules: Arc<[ModuleId]>,
}

impl CanonicalImportCycle {
    pub fn modules(&self) -> &[ModuleId] {
        &self.modules
    }
}

/// Pure deterministic validation result.
///
/// `problems` are malformed/unresolved topology. `cycles` are legal topology,
/// reported separately. Acyclic reconvergence (including diamonds) is valid and
/// appears in neither collection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct CanonicalImportGraphValidation {
    problems: Arc<[CanonicalImportGraphProblem]>,
    cycles: Arc<[CanonicalImportCycle]>,
}

impl CanonicalImportGraphValidation {
    pub fn is_valid(&self) -> bool {
        self.problems.is_empty()
    }

    pub fn problems(&self) -> &[CanonicalImportGraphProblem] {
        &self.problems
    }

    pub fn cycles(&self) -> &[CanonicalImportCycle] {
        &self.cycles
    }
}

/// Complete downstream semantic identity after import resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedProgramRevision {
    semantic: SemanticInputDescriptor,
    imports: CanonicalImportGraph,
}

impl ResolvedProgramRevision {
    /// Compose semantic inputs and durable imports.
    ///
    /// The canonical graph itself ignores physical relocation, while this
    /// complete revision intentionally retains the explicit physical
    /// resolution inputs embedded in `semantic`.
    pub fn new(semantic: SemanticInputDescriptor, imports: CanonicalImportGraph) -> Self {
        Self { semantic, imports }
    }

    pub fn semantic(&self) -> &SemanticInputDescriptor {
        &self.semantic
    }

    pub fn imports(&self) -> &CanonicalImportGraph {
        &self.imports
    }
}

/// Complete code-generation identity after canonical import resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedCodegenRevision {
    program: ResolvedProgramRevision,
    opt_level: StableOptLevel,
}

impl ResolvedCodegenRevision {
    pub fn new(program: ResolvedProgramRevision, opt_level: StableOptLevel) -> Self {
        Self { program, opt_level }
    }

    /// Add code-generation options to an already-resolved program revision.
    ///
    /// Requiring `program` prevents this path from omitting canonical imports.
    pub fn from_compile_options(
        program: ResolvedProgramRevision,
        options: &CompileOptions,
    ) -> CompileResult<Self> {
        validate_resolved_compile_options(&program, options)?;
        Ok(Self::new(program, options.opt_level.into()))
    }

    pub fn program(&self) -> &ResolvedProgramRevision {
        &self.program
    }

    pub fn opt_level(&self) -> StableOptLevel {
        self.opt_level
    }
}

/// Complete link identity after canonical import resolution and code generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedLinkRevision {
    codegen: ResolvedCodegenRevision,
    linker: StableLinkerInput,
}

impl ResolvedLinkRevision {
    pub fn new(codegen: ResolvedCodegenRevision, linker: StableLinkerInput) -> Self {
        Self { codegen, linker }
    }

    /// Add code-generation and linker options to an already-resolved program.
    pub fn from_compile_options(
        program: ResolvedProgramRevision,
        options: &CompileOptions,
    ) -> CompileResult<Self> {
        Ok(Self::new(
            ResolvedCodegenRevision::from_compile_options(program, options)?,
            (&options.linker).into(),
        ))
    }

    pub fn codegen(&self) -> &ResolvedCodegenRevision {
        &self.codegen
    }

    pub fn linker(&self) -> &StableLinkerInput {
        &self.linker
    }
}

fn validate_resolved_compile_options(
    program: &ResolvedProgramRevision,
    options: &CompileOptions,
) -> CompileResult<()> {
    if program.semantic().target != options.target {
        return Err(provenance_error(format!(
            "resolved program target {} does not match compile options target {}",
            program.semantic().target,
            options.target
        )));
    }
    let features = crate::StablePreviewFeatures::new(&options.preview_features);
    if program.semantic().preview_features != features {
        return Err(provenance_error(
            "resolved program preview features do not match compile options preview features"
                .to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
impl ImportGraph {
    pub fn root(&self) -> &ModuleId {
        &self.root
    }

    /// Outcomes parallel to [`Self::directives`].
    pub fn resolutions(&self) -> &[ImportResolution] {
        &self.resolutions
    }

    pub fn edges(&self) -> &[ImportEdge] {
        &self.edges
    }
}

/// Extract valid, exactly-one-string-literal `@import` calls from positional RIR.
///
/// Malformed calls are deliberately absent: this function neither resolves
/// modules nor competes with semantic analysis for diagnostic precedence.
/// `interner` must be the matching interner used to build `rir`.
#[cfg(test)]
pub(crate) fn extract_import_directives(
    rir: &Rir,
    interner: &ThreadedRodeo,
    metadata: &SourceMetadata,
) -> CompileResult<ImportDirectives> {
    let mut directives = Vec::new();

    for (_, inst) in rir.iter() {
        let InstData::Intrinsic {
            name,
            args_start,
            args_len: 1,
        } = &inst.data
        else {
            continue;
        };
        if interner.resolve(name) != "import" {
            continue;
        }
        let argument = rir.get_inst_refs(*args_start, 1)[0];
        let InstData::StringConst(specifier) = &rir.get(argument).data else {
            continue;
        };
        let importer = metadata.module_id(inst.span.file_id).ok_or_else(|| {
            CompileError::without_span(ErrorKind::InvalidCompilerInput(format!(
                "import directive references file ID {} absent from source metadata",
                inst.span.file_id.index()
            )))
        })?;
        directives.push(ImportDirective {
            importer,
            source_offset: inst.span.start,
            source_end: inst.span.end,
            specifier: Arc::from(interner.resolve(specifier)),
        });
    }

    directives.sort();
    Ok(ImportDirectives(directives.into()))
}

/// Resolve canonical directives against the already-loaded source metadata.
///
/// `std_dir` is explicit resolution context corresponding to the driver's
/// optional standard-library directory. This function never reads the
/// environment, probes for new source inputs, or emits diagnostics. Missing
/// and ambiguous imports are retained as graph data.
#[cfg(test)]
pub(crate) fn resolve_import_graph(
    directives: &ImportDirectives,
    metadata: &SourceMetadata,
    std_dir: Option<&str>,
) -> CompileResult<ImportGraph> {
    let dir_of = |path: &str| {
        Path::new(path)
            .parent()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let root_path = metadata
        .physical_path(metadata.root_file_id())
        .expect("validated metadata contains its root");
    let root_dir = dir_of(root_path);
    let loaded_paths: Vec<String> = metadata
        .physical_paths()
        .map(|(_, path)| path.to_owned())
        .collect();

    let module_for_path = |path: &str| -> CompileResult<ModuleId> {
        metadata
            .physical_paths()
            .find_map(|(file_id, candidate)| (candidate == path).then_some(file_id))
            .and_then(|file_id| metadata.module_id(file_id))
            .ok_or_else(|| {
                provenance_error(format!(
                    "resolved import path {path:?} is absent from source metadata"
                ))
            })
    };
    let physical_for_module = |module: &ModuleId| -> CompileResult<&str> {
        metadata
            .logical_paths()
            .find_map(|(file_id, _)| {
                (metadata.module_id(file_id).as_ref() == Some(module))
                    .then(|| metadata.physical_path(file_id))
                    .flatten()
            })
            .ok_or_else(|| {
                provenance_error(format!(
                    "importer module {module:?} is absent from source metadata"
                ))
            })
    };

    let mut resolutions = Vec::with_capacity(directives.len());
    let mut edges = Vec::new();
    for (index, directive) in directives.iter().enumerate() {
        let importer_dir = dir_of(physical_for_module(directive.importer())?);
        let mut base_dirs = vec![importer_dir];
        if !base_dirs.contains(&root_dir) {
            base_dirs.push(root_dir.clone());
        }
        if base_dirs.is_empty() {
            base_dirs.push(String::new());
        }
        let base_refs: Vec<&str> = base_dirs.iter().map(String::as_str).collect();
        let resolution = ModulePath::parse(directive.specifier()).resolve_in_dirs_with_std_dir(
            &base_refs,
            loaded_paths.iter(),
            std_dir,
        );
        match resolution {
            DirResolution::Resolved(path) => {
                let target = module_for_path(&path)?;
                let directive_index = u32::try_from(index).map_err(|_| {
                    provenance_error("import directive count exceeds u32::MAX".to_string())
                })?;
                edges.push(ImportEdge {
                    directive_index,
                    target: target.clone(),
                });
                resolutions.push(ImportResolution::Resolved(target));
            }
            DirResolution::Ambiguous {
                file_module,
                dir_module,
            } => resolutions.push(ImportResolution::Ambiguous {
                file_module: module_for_path(&file_module)?,
                directory_module: module_for_path(&dir_module)?,
            }),
            DirResolution::NotFound => resolutions.push(ImportResolution::Missing),
        }
    }

    Ok(ImportGraph {
        root: metadata.root_module_id(),
        directives: directives.clone(),
        resolutions: resolutions.into(),
        edges: edges.into(),
    })
}

fn provenance_error(message: String) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message))
}

/// Resolve occurrence-independent durable import identity from explicit inputs.
///
/// Resolution uses only `inputs` and the explicitly supplied optional stdlib
/// directory. It never consults source metadata, file IDs, the environment, or
/// the filesystem for discovery.
pub fn resolve_canonical_import_graph(
    directives: &ImportDirectives,
    inputs: &ModuleResolutionInputs,
    std_dir: Option<&str>,
) -> CompileResult<CanonicalImportGraph> {
    let dir_of = |path: &str| {
        Path::new(path)
            .parent()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let root_path = inputs
        .physical_path(inputs.root())
        .expect("validated resolution inputs contain their root");
    let root_dir = dir_of(root_path);
    let loaded_paths: Vec<String> = inputs
        .modules()
        .iter()
        .map(|entry| entry.physical_path.to_string())
        .collect();
    let module_for_path = |path: &str| -> CompileResult<ModuleId> {
        inputs
            .modules()
            .iter()
            .find_map(|entry| (entry.physical_path.as_ref() == path).then(|| entry.module.clone()))
            .ok_or_else(|| {
                provenance_error(format!(
                    "resolved import path {path:?} is absent from module resolution inputs"
                ))
            })
    };

    let mut records = Vec::with_capacity(directives.len());
    for directive in directives.iter() {
        let importer_path = inputs.physical_path(directive.importer()).ok_or_else(|| {
            provenance_error(format!(
                "importer module {:?} is absent from module resolution inputs",
                directive.importer()
            ))
        })?;
        let importer_dir = dir_of(importer_path);
        let mut base_dirs = vec![importer_dir];
        if !base_dirs.contains(&root_dir) {
            base_dirs.push(root_dir.clone());
        }
        let base_refs: Vec<&str> = base_dirs.iter().map(String::as_str).collect();
        let outcome =
            resolve_explicit_candidates(directive.specifier(), &base_refs, &loaded_paths, std_dir);
        let resolution = match outcome {
            DirResolution::Resolved(path) => {
                CanonicalImportResolution::Resolved(module_for_path(&path)?)
            }
            DirResolution::Ambiguous {
                file_module,
                dir_module,
            } => CanonicalImportResolution::Ambiguous {
                file_module: module_for_path(&file_module)?,
                directory_module: module_for_path(&dir_module)?,
            },
            DirResolution::NotFound => CanonicalImportResolution::Missing,
        };
        records.push(CanonicalImportRecord::new(
            directive.importer().clone(),
            directive.specifier(),
            resolution,
        ));
    }
    records.sort();
    records.dedup();
    let graph = CanonicalImportGraph {
        root: inputs.root().clone(),
        records: records.into(),
    };
    let validation = validate_canonical_import_graph(&graph, inputs);
    debug_assert!(
        validation.problems().iter().all(|problem| matches!(
            problem,
            CanonicalImportGraphProblem::MissingResolution { .. }
                | CanonicalImportGraphProblem::AmbiguousResolution { .. }
        )),
        "derived canonical graph may be unresolved, but cannot be structurally malformed"
    );
    Ok(graph)
}

/// Validate a canonical graph against the explicit resolved module set.
pub fn validate_canonical_import_graph(
    graph: &CanonicalImportGraph,
    inputs: &ModuleResolutionInputs,
) -> CanonicalImportGraphValidation {
    validate_records(graph.root(), graph.records(), inputs)
}

fn validate_records(
    root: &ModuleId,
    records: &[CanonicalImportRecord],
    inputs: &ModuleResolutionInputs,
) -> CanonicalImportGraphValidation {
    let modules: BTreeSet<_> = inputs
        .modules()
        .iter()
        .map(|entry| entry.module.clone())
        .collect();
    let mut problems = Vec::new();
    if root != inputs.root() {
        problems.push(CanonicalImportGraphProblem::RootMismatch {
            expected: inputs.root().clone(),
            found: root.clone(),
        });
    }
    if !modules.contains(root) {
        problems.push(CanonicalImportGraphProblem::RootNotInModuleSet(
            root.clone(),
        ));
    }

    let mut sorted = records.to_vec();
    sorted.sort();
    for pair in sorted.windows(2) {
        if pair[0] == pair[1] {
            problems.push(CanonicalImportGraphProblem::DuplicateRecord(
                pair[0].clone(),
            ));
        }
    }
    let mut by_key: BTreeMap<(ModuleId, Arc<str>), BTreeSet<CanonicalImportResolution>> =
        BTreeMap::new();
    let mut adjacency: BTreeMap<ModuleId, BTreeSet<ModuleId>> = modules
        .iter()
        .cloned()
        .map(|module| (module, BTreeSet::new()))
        .collect();
    for record in &sorted {
        let key = (record.importer.clone(), record.normalized_specifier.clone());
        by_key
            .entry(key.clone())
            .or_default()
            .insert(record.resolution.clone());
        if !modules.contains(&record.importer) {
            problems.push(CanonicalImportGraphProblem::ForeignImporter {
                importer: key.0,
                normalized_specifier: key.1,
            });
        }
        match &record.resolution {
            CanonicalImportResolution::Resolved(target) => {
                if !modules.contains(target) {
                    problems.push(CanonicalImportGraphProblem::ForeignResolvedTarget {
                        importer: record.importer.clone(),
                        normalized_specifier: record.normalized_specifier.clone(),
                        target: target.clone(),
                    });
                } else if modules.contains(&record.importer) {
                    adjacency
                        .get_mut(&record.importer)
                        .expect("known importer has adjacency entry")
                        .insert(target.clone());
                }
            }
            CanonicalImportResolution::Missing => {
                problems.push(CanonicalImportGraphProblem::MissingResolution {
                    importer: record.importer.clone(),
                    normalized_specifier: record.normalized_specifier.clone(),
                });
            }
            CanonicalImportResolution::Ambiguous {
                file_module,
                directory_module,
            } => {
                for target in [file_module, directory_module] {
                    if !modules.contains(target) {
                        problems.push(CanonicalImportGraphProblem::ForeignResolvedTarget {
                            importer: record.importer.clone(),
                            normalized_specifier: record.normalized_specifier.clone(),
                            target: target.clone(),
                        });
                    }
                }
                problems.push(CanonicalImportGraphProblem::AmbiguousResolution {
                    importer: record.importer.clone(),
                    normalized_specifier: record.normalized_specifier.clone(),
                    file_module: file_module.clone(),
                    directory_module: directory_module.clone(),
                });
            }
        }
    }
    for ((importer, normalized_specifier), resolutions) in by_key {
        if resolutions.len() > 1 {
            problems.push(CanonicalImportGraphProblem::ConflictingCanonicalKey {
                importer,
                normalized_specifier,
                resolutions: resolutions.into_iter().collect::<Vec<_>>().into(),
            });
        }
    }
    problems.sort();
    problems.dedup();
    CanonicalImportGraphValidation {
        problems: problems.into(),
        cycles: cycle_components(&adjacency).into(),
    }
}

fn cycle_components(
    adjacency: &BTreeMap<ModuleId, BTreeSet<ModuleId>>,
) -> Vec<CanonicalImportCycle> {
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for node in adjacency.keys() {
        if visited.contains(node) {
            continue;
        }
        // Explicit enter/exit frames preserve recursive DFS postorder without
        // making supplied graph depth consume the process stack.
        let mut stack = vec![(node.clone(), false)];
        while let Some((current, exiting)) = stack.pop() {
            if exiting {
                order.push(current);
                continue;
            }
            if !visited.insert(current.clone()) {
                continue;
            }
            stack.push((current.clone(), true));
            stack.extend(
                adjacency[&current]
                    .iter()
                    .rev()
                    .map(|next| (next.clone(), false)),
            );
        }
    }
    let mut reverse: BTreeMap<ModuleId, BTreeSet<ModuleId>> = adjacency
        .keys()
        .cloned()
        .map(|module| (module, BTreeSet::new()))
        .collect();
    for (from, targets) in adjacency {
        for target in targets {
            reverse
                .get_mut(target)
                .expect("known target has reverse adjacency entry")
                .insert(from.clone());
        }
    }
    let mut assigned = BTreeSet::new();
    let mut cycles = Vec::new();
    while let Some(node) = order.pop() {
        if assigned.contains(&node) {
            continue;
        }
        let mut stack = vec![node];
        let mut component = Vec::new();
        while let Some(current) = stack.pop() {
            if !assigned.insert(current.clone()) {
                continue;
            }
            component.push(current.clone());
            stack.extend(reverse[&current].iter().rev().cloned());
        }
        component.sort();
        let self_cycle = component.len() == 1 && adjacency[&component[0]].contains(&component[0]);
        if component.len() > 1 || self_cycle {
            cycles.push(CanonicalImportCycle {
                modules: component.into(),
            });
        }
    }
    cycles.sort();
    cycles
}

/// Pure lexical counterpart of `ModulePath` loaded-file resolution.
///
/// Candidate grouping and precedence come from `ModulePath`; matching uses
/// normalized explicit input strings only and never canonicalizes or probes the
/// filesystem.
fn resolve_explicit_candidates(
    specifier: &str,
    base_dirs: &[&str],
    loaded_paths: &[String],
    std_dir: Option<&str>,
) -> DirResolution {
    let loaded: Vec<_> = loaded_paths
        .iter()
        .map(|path| (normalize_module_path(path), path))
        .collect();
    let find = |candidate: &str| {
        let normalized = normalize_module_path(candidate);
        loaded
            .iter()
            .find(|(path, _)| *path == normalized)
            .map(|(_, original)| (*original).clone())
    };
    let owned_bases: Vec<String> = base_dirs.iter().map(|base| (*base).to_string()).collect();
    for group in ModulePath::parse(specifier).candidate_groups(&owned_bases, std_dir) {
        match group.as_slice() {
            [candidate] => {
                if let Some(path) = find(candidate) {
                    return DirResolution::Resolved(path);
                }
            }
            [file_candidate, directory_candidate] => {
                let file_module = find(file_candidate);
                let dir_module = find(directory_candidate);
                match (file_module, dir_module) {
                    (Some(file_module), Some(dir_module)) => {
                        return DirResolution::Ambiguous {
                            file_module,
                            dir_module,
                        };
                    }
                    (Some(path), None) | (None, Some(path)) => {
                        return DirResolution::Resolved(path);
                    }
                    (None, None) => {}
                }
            }
            _ => unreachable!("ModulePath candidate groups contain one or two paths"),
        }
    }
    DirResolution::NotFound
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    use rue_error::ErrorKind;
    use rue_span::FileId;

    use super::*;
    use crate::{CanonicalRirOutput, CompileOptions, CompilerSession, SourceSnapshot, SourceView};

    fn value_hash(value: &impl Hash) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    struct LoweredFrontend {
        snapshot: SourceSnapshot,
        rir: Arc<CanonicalRirOutput>,
        directives: ImportDirectives,
    }

    impl LoweredFrontend {
        fn source_snapshot(&self) -> &SourceSnapshot {
            &self.snapshot
        }

        fn source_metadata(&self) -> &SourceMetadata {
            self.snapshot.metadata()
        }

        fn import_directives(&self) -> Option<&ImportDirectives> {
            Some(&self.directives)
        }

        fn rir(&self) -> &rue_rir::Rir {
            self.rir.rir()
        }

        fn interner(&self) -> &crate::ThreadedRodeo {
            self.rir.semantic_symbols().interner()
        }

        fn analyze(&self) -> Result<Arc<crate::CanonicalSemanticOutput>, crate::CompileErrors> {
            let mut session = CompilerSession::new();
            session
                .update_for_presentation(&self.snapshot)
                .into_result()?;
            session.semantic(&CompileOptions::default())
        }

        fn resolve_import_graph(&self, std_dir: Option<&str>) -> crate::CompileResult<ImportGraph> {
            resolve_import_graph(&self.directives, self.snapshot.metadata(), std_dir)
        }
    }

    fn lower<'a>(
        sources: Vec<SourceView<'a>>,
        root: FileId,
        logical_paths: HashMap<FileId, String>,
    ) -> LoweredFrontend {
        let metadata = SourceMetadata::from_sources(&sources, root, logical_paths).unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata).unwrap();
        let mut session = CompilerSession::new();
        let parsed = session
            .update_for_presentation(&snapshot)
            .into_result()
            .unwrap();
        let directives = parsed.import_directives().clone();
        let rir = session.rir().unwrap();
        LoweredFrontend {
            snapshot,
            rir,
            directives,
        }
    }

    fn specifiers(unit: &LoweredFrontend) -> Vec<&str> {
        unit.import_directives()
            .unwrap()
            .iter()
            .map(ImportDirective::specifier)
            .collect()
    }

    #[test]
    fn extracts_imports_from_nested_expression_and_body_forms() {
        let source = r#"
const top = @import("top");
fn consume(value: i32) {}
fn main() -> i32 {
    let array = [@import("array"), @import("array2")];
    if true {
        consume(@import("call_arg"));
    } else {
        let other = @import("else_block");
    }
    let nested = @dbg(@import("intrinsic_arg"));
    let indexed = [@import("index_base")][0];
    comptime { @import("comptime") };
    0
}
"#;
        let id = FileId::new(1);
        let unit = lower(
            vec![SourceView::new("main.rue", source, id)],
            id,
            HashMap::new(),
        );

        assert_eq!(
            specifiers(&unit),
            vec![
                "top",
                "array",
                "array2",
                "call_arg",
                "else_block",
                "intrinsic_arg",
                "index_base",
                "comptime",
            ]
        );
    }

    #[test]
    fn retains_duplicate_sites_and_excludes_malformed_imports() {
        let source = r#"
fn main() -> i32 {
    let zero = @import();
    let a = @import("same");
    let b = @import("same");
    let non_string = @import(1);
    let two = @import("first", "second");
    0
}
"#;
        let id = FileId::new(8);
        let unit = lower(
            vec![SourceView::new("main.rue", source, id)],
            id,
            HashMap::new(),
        );
        let directives = unit.import_directives().unwrap();
        assert_eq!(directives.len(), 2);
        assert_eq!(specifiers(&unit), vec!["same", "same"]);
        assert_ne!(
            directives.as_slice()[0].source_offset(),
            directives.as_slice()[1].source_offset()
        );

        let errors = unit.analyze().unwrap_err();
        assert!(matches!(
            &errors.first().unwrap().kind,
            ErrorKind::IntrinsicWrongArgCount {
                name,
                expected: 1,
                found: 0,
            } if name == "import"
        ));
    }

    #[test]
    fn malformed_only_imports_keep_existing_semantic_error_precedence() {
        let cases = [
            ("@import()", "zero"),
            ("@import(1)", "non_string"),
            ("@import(\"a\", \"b\")", "two"),
        ];
        for (call, expected) in cases {
            let source = format!("fn main() -> i32 {{ let value = {call}; 0 }}");
            let id = FileId::new(1);
            let unit = lower(
                vec![SourceView::new("main.rue", &source, id)],
                id,
                HashMap::new(),
            );
            assert!(unit.import_directives().unwrap().is_empty());
            let errors = unit.analyze().unwrap_err();
            assert_eq!(errors.len(), 1);
            let kind = &errors.first().unwrap().kind;
            match expected {
                "zero" => assert!(matches!(
                    kind,
                    ErrorKind::IntrinsicWrongArgCount {
                        name,
                        expected: 1,
                        found: 0,
                    } if name == "import"
                )),
                "non_string" => assert!(matches!(kind, ErrorKind::ImportRequiresStringLiteral)),
                "two" => assert!(matches!(
                    kind,
                    ErrorKind::IntrinsicWrongArgCount {
                        name,
                        expected: 1,
                        found: 2,
                    } if name == "import"
                )),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn extraction_rejects_import_spans_absent_from_metadata() {
        let id = FileId::new(7);
        let unit = lower(
            vec![SourceView::new(
                "main.rue",
                "fn main() -> i32 { let m = @import(\"a\"); 0 }",
                id,
            )],
            id,
            HashMap::new(),
        );
        let foreign = FileId::new(9);
        let metadata = SourceMetadata::new(
            foreign,
            HashMap::from([(foreign, "foreign.rue".to_string())]),
            HashMap::from([(foreign, "foreign.rue".to_string())]),
        )
        .unwrap();

        let error = extract_import_directives(unit.rir(), unit.interner(), &metadata).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid compiler input: import directive references file ID 7 absent from source metadata"
        );
    }

    fn invariant_snapshot(
        root_id: u32,
        helper_id: u32,
        root_physical: &str,
        helper_physical: &str,
        reversed: bool,
    ) -> ImportDirectives {
        let root_id = FileId::new(root_id);
        let helper_id = FileId::new(helper_id);
        let root = SourceView::new(
            root_physical,
            "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
            root_id,
        );
        let helper = SourceView::new(
            helper_physical,
            "fn helper() -> i32 { let h = @import(\"leaf.rue\"); 1 }",
            helper_id,
        );
        let sources = if reversed {
            vec![helper, root]
        } else {
            vec![root, helper]
        };
        lower(
            sources,
            root_id,
            HashMap::from([
                (root_id, "app/main.rue".to_string()),
                (helper_id, "app/helper.rue".to_string()),
            ]),
        )
        .import_directives()
        .unwrap()
        .clone()
    }

    #[test]
    fn directives_ignore_file_ids_load_order_and_physical_relocation() {
        let first = invariant_snapshot(1, 2, "/one/main.rue", "/one/helper.rue", false);
        let second = invariant_snapshot(90, 7, "/moved/main.rue", "/moved/helper.rue", true);
        assert_eq!(first, second);
    }

    #[test]
    fn source_offset_and_specifier_edits_change_directive_identity() {
        let id = FileId::new(1);
        let build = |source| {
            lower(
                vec![SourceView::new("main.rue", source, id)],
                id,
                HashMap::new(),
            )
            .import_directives()
            .unwrap()
            .clone()
        };
        let original = build("fn main() -> i32 { let m = @import(\"a\"); 0 }");
        let moved = build("fn main() -> i32 {  let m = @import(\"a\"); 0 }");
        let renamed = build("fn main() -> i32 { let m = @import(\"b\"); 0 }");
        assert_ne!(original, moved);
        assert_ne!(original, renamed);
    }

    fn graph_unit(entries: &[(u32, &str, &str, &str)], root: u32) -> LoweredFrontend {
        let sources = entries
            .iter()
            .map(|(id, physical, _, source)| SourceView::new(physical, source, FileId::new(*id)))
            .collect();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_string()))
            .collect();
        lower(sources, FileId::new(root), logical)
    }

    #[test]
    fn resolves_explicit_simple_and_std_with_explicit_context() {
        let unit = graph_unit(
            &[
                (
                    1,
                    "/project/main.rue",
                    "app/main.rue",
                    r#"fn main() -> i32 {
                        let a = @import("helper.rue");
                        let b = @import("helper");
                        let s = @import("std");
                        0
                    }"#,
                ),
                (2, "/project/helper.rue", "app/helper.rue", "fn helper() {}"),
                (3, "/sdk/_std.rue", "std/_std.rue", "fn std_item() {}"),
            ],
            1,
        );
        let graph = unit.resolve_import_graph(Some("/sdk")).unwrap();
        assert_eq!(graph.root().as_str(), "app/main.rue");
        assert_eq!(graph.edges().len(), 3);
        assert_eq!(graph.edges()[0].target().as_str(), "app/helper.rue");
        assert_eq!(graph.edges()[1].target().as_str(), "app/helper.rue");
        assert_eq!(graph.edges()[2].target().as_str(), "std/_std.rue");
        assert_eq!(graph.edges()[0].directive_index(), 0);
        assert_eq!(graph.edges()[1].directive_index(), 1);
    }

    #[test]
    fn retains_missing_and_nearest_base_ambiguity_as_data() {
        let unit = graph_unit(
            &[
                (
                    1,
                    "/p/main.rue",
                    "app/main.rue",
                    r#"fn main() -> i32 {
                        let missing = @import("missing");
                        let ambiguous = @import("foo");
                        0
                    }"#,
                ),
                (2, "/p/foo.rue", "app/foo.rue", "fn file_item() {}"),
                (
                    3,
                    "/p/foo/_foo.rue",
                    "app/foo/_foo.rue",
                    "fn facade_item() {}",
                ),
            ],
            1,
        );
        let graph = unit.resolve_import_graph(None).unwrap();
        assert_eq!(graph.edges().len(), 0);
        assert!(matches!(graph.resolutions()[0], ImportResolution::Missing));
        assert!(matches!(
            &graph.resolutions()[1],
            ImportResolution::Ambiguous {
                file_module,
                directory_module,
            } if file_module.as_str() == "app/foo.rue"
                && directory_module.as_str() == "app/foo/_foo.rue"
        ));
    }

    #[test]
    fn importer_directory_precedes_root_directory() {
        let unit = graph_unit(
            &[
                (1, "/p/main.rue", "app/main.rue", "fn main() -> i32 { 0 }"),
                (
                    2,
                    "/p/sub/importer.rue",
                    "app/sub/importer.rue",
                    "fn importer() { let f = @import(\"foo\"); }",
                ),
                (3, "/p/foo.rue", "app/root_foo.rue", "fn root_foo() {}"),
                (4, "/p/sub/foo.rue", "app/sub/foo.rue", "fn near_foo() {}"),
            ],
            1,
        );
        let graph = unit.resolve_import_graph(None).unwrap();
        assert_eq!(graph.edges()[0].target().as_str(), "app/sub/foo.rue");
    }

    #[test]
    fn cycles_and_diamonds_are_ordinary_repeated_topology() {
        let cycle = graph_unit(
            &[
                (
                    1,
                    "/p/a.rue",
                    "a.rue",
                    "fn main() -> i32 { let b = @import(\"b.rue\"); 0 }",
                ),
                (
                    2,
                    "/p/b.rue",
                    "b.rue",
                    "fn b() { let a = @import(\"a.rue\"); }",
                ),
            ],
            1,
        )
        .resolve_import_graph(None)
        .unwrap();
        assert_eq!(cycle.edges().len(), 2);
        assert_eq!(cycle.edges()[0].target().as_str(), "b.rue");
        assert_eq!(cycle.edges()[1].target().as_str(), "a.rue");

        let diamond = graph_unit(
            &[
                (1, "/p/a.rue", "a.rue", "fn main() -> i32 { let b = @import(\"b.rue\"); let c = @import(\"c.rue\"); 0 }"),
                (2, "/p/b.rue", "b.rue", "fn b() { let d = @import(\"d.rue\"); }"),
                (3, "/p/c.rue", "c.rue", "fn c() { let d = @import(\"d.rue\"); }"),
                (4, "/p/d.rue", "d.rue", "fn d() {}"),
            ],
            1,
        )
        .resolve_import_graph(None)
        .unwrap();
        assert_eq!(diamond.edges().len(), 4);
        assert_eq!(
            diamond
                .edges()
                .iter()
                .filter(|edge| edge.target().as_str() == "d.rue")
                .count(),
            2
        );
    }

    fn invariant_graph(prefix: &str, root_id: u32, helper_id: u32, reverse: bool) -> ImportGraph {
        let root_path = format!("{prefix}/main.rue");
        let helper_path = format!("{prefix}/helper.rue");
        let root = SourceView::new(
            &root_path,
            "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
            FileId::new(root_id),
        );
        let helper = SourceView::new(&helper_path, "fn helper() {}", FileId::new(helper_id));
        let sources = if reverse {
            vec![helper, root]
        } else {
            vec![root, helper]
        };
        lower(
            sources,
            FileId::new(root_id),
            HashMap::from([
                (FileId::new(root_id), "app/main.rue".to_string()),
                (FileId::new(helper_id), "app/helper.rue".to_string()),
            ]),
        )
        .resolve_import_graph(None)
        .unwrap()
    }

    #[test]
    fn graph_ignores_file_ids_load_order_and_physical_relocation() {
        assert_eq!(
            invariant_graph("/one", 1, 2, false),
            invariant_graph("/moved", 90, 7, true)
        );
    }

    #[test]
    fn resolver_fails_closed_on_foreign_directive_metadata() {
        let unit = graph_unit(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { let m = @import(\"m\"); 0 }",
            )],
            1,
        );
        let foreign = FileId::new(9);
        let metadata = SourceMetadata::new(
            foreign,
            HashMap::from([(foreign, "/other/main.rue".to_string())]),
            HashMap::from([(foreign, "other/main.rue".to_string())]),
        )
        .unwrap();
        let error =
            resolve_import_graph(unit.import_directives().unwrap(), &metadata, None).unwrap_err();
        assert!(error.to_string().contains("importer module"));
        assert!(error.to_string().contains("absent from source metadata"));
    }

    #[test]
    fn graph_query_does_not_change_existing_diagnostic_bytes_or_order() {
        let entries = [(
            1,
            "/p/main.rue",
            "main.rue",
            "fn main() -> i32 { let m = @import(\"missing\"); 0 }",
        )];
        let direct = graph_unit(&entries, 1);
        let queried = graph_unit(&entries, 1);
        resolve_canonical_import_graph(
            queried.import_directives().unwrap(),
            &ModuleResolutionInputs::from_metadata(queried.source_metadata()),
            None,
        )
        .unwrap();
        assert!(matches!(
            queried.resolve_import_graph(None).unwrap().resolutions()[0],
            ImportResolution::Missing
        ));
        let fingerprint = |errors: crate::CompileErrors| {
            errors
                .into_iter()
                .map(|error| (error.kind.code(), error.span(), error.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            fingerprint(direct.analyze().unwrap_err()),
            fingerprint(queried.analyze().unwrap_err())
        );

        let ambiguous_entries = [
            (
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { let f = @import(\"foo\"); 0 }",
            ),
            (2, "/p/foo.rue", "foo.rue", "fn file_item() {}"),
            (3, "/p/foo/_foo.rue", "foo/_foo.rue", "fn facade_item() {}"),
        ];
        let direct = graph_unit(&ambiguous_entries, 1);
        let queried = graph_unit(&ambiguous_entries, 1);
        assert!(matches!(
            queried.resolve_import_graph(None).unwrap().resolutions()[0],
            ImportResolution::Ambiguous { .. }
        ));
        assert_eq!(
            fingerprint(direct.analyze().unwrap_err()),
            fingerprint(queried.analyze().unwrap_err())
        );
    }

    #[test]
    fn durable_graph_normalizes_spelling_and_deduplicates_repeated_sites() {
        let unit = graph_unit(
            &[
                (
                    1,
                    "/p/main.rue",
                    "app/main.rue",
                    r#"fn main() -> i32 {
                        let first = @import("./helper.rue");
                        let second = @import("dir/../helper.rue");
                        let third = @import("helper.rue");
                        0
                    }"#,
                ),
                (2, "/p/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let inputs = ModuleResolutionInputs::from_metadata(unit.source_metadata());
        let durable =
            resolve_canonical_import_graph(unit.import_directives().unwrap(), &inputs, None)
                .unwrap();
        assert_eq!(unit.import_directives().unwrap().len(), 3);
        assert_eq!(unit.resolve_import_graph(None).unwrap().edges().len(), 3);
        assert_eq!(durable.records().len(), 1);
        assert_eq!(durable.records()[0].normalized_specifier(), "helper.rue");
        assert!(matches!(
            durable.records()[0].resolution(),
            CanonicalImportResolution::Resolved(module)
                if module.as_str() == "app/helper.rue"
        ));
    }

    fn reordered_durable_graph(source: &str) -> CanonicalImportGraph {
        let unit = graph_unit(
            &[
                (1, "/p/main.rue", "app/main.rue", source),
                (2, "/p/a.rue", "app/a.rue", "fn a() {}"),
                (3, "/p/b.rue", "app/b.rue", "fn b() {}"),
            ],
            1,
        );
        resolve_canonical_import_graph(
            unit.import_directives().unwrap(),
            &ModuleResolutionInputs::from_metadata(unit.source_metadata()),
            None,
        )
        .unwrap()
    }

    #[test]
    fn durable_graph_ignores_site_order_and_offsets() {
        let first = reordered_durable_graph(
            "fn main() -> i32 { let a = @import(\"a.rue\"); let b = @import(\"b.rue\"); 0 }",
        );
        let reordered = reordered_durable_graph(
            "fn main() -> i32 {   let b = @import(\"b.rue\"); let a = @import(\"a.rue\"); 0 }",
        );
        assert_eq!(first, reordered);
    }

    #[test]
    fn explicit_resolution_changes_graph_but_not_source_identity() {
        let unit = graph_unit(
            &[
                (
                    1,
                    "/p/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/p/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let original_inputs = ModuleResolutionInputs::from_metadata(unit.source_metadata());
        let source_revision = unit.source_snapshot().source_revision().clone();
        let moved_inputs = ModuleResolutionInputs::new(
            original_inputs.root().clone(),
            original_inputs
                .modules()
                .iter()
                .map(|entry| crate::ModuleResolutionInput {
                    module: entry.module.clone(),
                    physical_path: if entry.module.as_str() == "app/helper.rue" {
                        Arc::from("/elsewhere/helper.rue")
                    } else {
                        entry.physical_path.clone()
                    },
                })
                .collect(),
        )
        .unwrap();
        let original = resolve_canonical_import_graph(
            unit.import_directives().unwrap(),
            &original_inputs,
            None,
        )
        .unwrap();
        let changed =
            resolve_canonical_import_graph(unit.import_directives().unwrap(), &moved_inputs, None)
                .unwrap();
        assert_ne!(original, changed);
        assert_eq!(unit.source_snapshot().source_revision(), &source_revision);

        let mut original_semantic = SemanticInputDescriptor::new(
            unit.source_snapshot(),
            crate::Target::default(),
            &rue_error::PreviewFeatures::default(),
        );
        let mut moved_semantic = original_semantic.clone();
        original_semantic.resolution = original_inputs;
        moved_semantic.resolution = moved_inputs;
        assert_eq!(original_semantic.sources, moved_semantic.sources);
        assert_ne!(
            ResolvedProgramRevision::new(original_semantic, original),
            ResolvedProgramRevision::new(moved_semantic, changed)
        );
    }

    fn relocated_revision(
        prefix: &str,
        root_id: u32,
        helper_id: u32,
    ) -> (
        crate::SourceRevision,
        CanonicalImportGraph,
        ResolvedProgramRevision,
    ) {
        let root_path = format!("{prefix}/main.rue");
        let helper_path = format!("{prefix}/helper.rue");
        let unit = lower(
            vec![
                SourceView::new(&helper_path, "fn helper() {}", FileId::new(helper_id)),
                SourceView::new(
                    &root_path,
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                    FileId::new(root_id),
                ),
            ],
            FileId::new(root_id),
            HashMap::from([
                (FileId::new(root_id), "app/main.rue".to_string()),
                (FileId::new(helper_id), "app/helper.rue".to_string()),
            ]),
        );
        let semantic = SemanticInputDescriptor::new(
            unit.source_snapshot(),
            crate::Target::default(),
            &rue_error::PreviewFeatures::default(),
        );
        let graph = resolve_canonical_import_graph(
            unit.import_directives().unwrap(),
            &semantic.resolution,
            None,
        )
        .unwrap();
        (
            semantic.sources.clone(),
            graph.clone(),
            ResolvedProgramRevision::new(semantic, graph),
        )
    }

    #[test]
    fn relocation_keeps_graph_and_sources_but_changes_explicit_resolved_revision() {
        let (first_sources, first_graph, first_revision) = relocated_revision("/one", 1, 2);
        let (moved_sources, moved_graph, moved_revision) = relocated_revision("/moved", 90, 7);
        assert_eq!(first_sources, moved_sources);
        assert_eq!(first_graph, moved_graph);
        assert_eq!(value_hash(&first_graph), value_hash(&moved_graph));
        assert_ne!(first_revision, moved_revision);
    }

    #[test]
    fn resolved_codegen_and_link_revisions_layer_options_after_imports() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ResolvedCodegenRevision>();
        assert_send_sync::<ResolvedLinkRevision>();

        let (_, _, program) = relocated_revision("/one", 1, 2);
        let base_options = crate::CompileOptions::default();
        let mut optimized_options = base_options.clone();
        optimized_options.opt_level = crate::OptLevel::O2;
        let base_codegen =
            ResolvedCodegenRevision::from_compile_options(program.clone(), &base_options).unwrap();
        let optimized_codegen =
            ResolvedCodegenRevision::from_compile_options(program.clone(), &optimized_options)
                .unwrap();
        assert_eq!(base_codegen.program(), optimized_codegen.program());
        assert_ne!(base_codegen, optimized_codegen);
        assert_eq!(base_codegen.opt_level(), StableOptLevel::O0);
        assert_eq!(optimized_codegen.opt_level(), StableOptLevel::O2);

        let base_link =
            ResolvedLinkRevision::from_compile_options(program.clone(), &base_options).unwrap();
        let optimized_link =
            ResolvedLinkRevision::from_compile_options(program.clone(), &optimized_options)
                .unwrap();
        assert_ne!(base_link, optimized_link);
        assert_eq!(base_link.codegen().program(), &program);

        let mut system_options = base_options.clone();
        system_options.linker = crate::LinkerMode::System("cc".to_owned());
        let system_link =
            ResolvedLinkRevision::from_compile_options(program.clone(), &system_options).unwrap();
        assert_eq!(base_link.codegen(), system_link.codegen());
        assert_ne!(base_link, system_link);
        assert!(matches!(base_link.linker(), StableLinkerInput::Internal));
        assert!(
            matches!(system_link.linker(), StableLinkerInput::System(name) if name.as_ref() == "cc")
        );

        assert_eq!(base_codegen.program().semantic(), program.semantic());
        assert_eq!(base_codegen.program().imports(), program.imports());
        assert_eq!(base_codegen.clone(), base_codegen);
        assert_eq!(value_hash(&base_codegen.clone()), value_hash(&base_codegen));
        assert_eq!(value_hash(&base_link.clone()), value_hash(&base_link));
    }

    #[test]
    fn graph_changes_propagate_through_resolved_codegen_and_link_identity() {
        let (_, _, program) = relocated_revision("/one", 1, 2);
        let empty_graph = CanonicalImportGraph::from_supplied(
            program.imports().root().clone(),
            Vec::new(),
            &program.semantic().resolution,
        )
        .unwrap();
        let changed_program = ResolvedProgramRevision::new(program.semantic().clone(), empty_graph);
        let options = crate::CompileOptions::default();
        let original_codegen =
            ResolvedCodegenRevision::from_compile_options(program.clone(), &options).unwrap();
        let changed_codegen =
            ResolvedCodegenRevision::from_compile_options(changed_program.clone(), &options)
                .unwrap();
        assert_ne!(original_codegen, changed_codegen);
        assert_ne!(
            ResolvedLinkRevision::from_compile_options(program, &options).unwrap(),
            ResolvedLinkRevision::from_compile_options(changed_program, &options).unwrap()
        );
    }

    #[test]
    fn resolved_compile_options_fail_closed_on_semantic_mismatch() {
        let (_, _, program) = relocated_revision("/one", 1, 2);
        let target_options = crate::CompileOptions {
            target: if program.semantic().target == crate::Target::X86_64Linux {
                crate::Target::Aarch64Linux
            } else {
                crate::Target::X86_64Linux
            },
            ..crate::CompileOptions::default()
        };
        let target_error =
            ResolvedCodegenRevision::from_compile_options(program.clone(), &target_options)
                .unwrap_err();
        assert_eq!(
            target_error.to_string(),
            format!(
                "invalid compiler input: resolved program target {} does not match compile options target {}",
                program.semantic().target,
                target_options.target
            )
        );

        let mut feature_options = crate::CompileOptions::default();
        feature_options
            .preview_features
            .insert(rue_error::PreviewFeature::TestInfra);
        let feature_error =
            ResolvedLinkRevision::from_compile_options(program.clone(), &feature_options)
                .unwrap_err();
        assert_eq!(
            feature_error.to_string(),
            "invalid compiler input: resolved program preview features do not match compile options preview features"
        );
        assert_eq!(
            feature_error.to_string(),
            ResolvedLinkRevision::from_compile_options(program, &feature_options)
                .unwrap_err()
                .to_string()
        );
    }

    #[test]
    fn resolved_codegen_and_link_identity_retain_relocation_sensitivity() {
        let (_, first_graph, first_program) = relocated_revision("/one", 1, 2);
        let (_, moved_graph, moved_program) = relocated_revision("/moved", 90, 7);
        assert_eq!(first_graph, moved_graph);
        let options = crate::CompileOptions::default();
        assert_ne!(
            ResolvedCodegenRevision::from_compile_options(first_program.clone(), &options).unwrap(),
            ResolvedCodegenRevision::from_compile_options(moved_program.clone(), &options).unwrap()
        );
        assert_ne!(
            ResolvedLinkRevision::from_compile_options(first_program, &options).unwrap(),
            ResolvedLinkRevision::from_compile_options(moved_program, &options).unwrap()
        );
    }

    fn supplied_inputs(names: &[&str], root: &str) -> ModuleResolutionInputs {
        ModuleResolutionInputs::new(
            ModuleId::from_logical_path(root).unwrap(),
            names
                .iter()
                .map(|name| crate::ModuleResolutionInput {
                    module: ModuleId::from_logical_path(name).unwrap(),
                    physical_path: Arc::from(format!("/p/{name}")),
                })
                .collect(),
        )
        .unwrap()
    }

    fn resolved(importer: &str, specifier: &str, target: &str) -> CanonicalImportRecord {
        CanonicalImportRecord::new(
            ModuleId::from_logical_path(importer).unwrap(),
            specifier,
            CanonicalImportResolution::Resolved(ModuleId::from_logical_path(target).unwrap()),
        )
    }

    #[test]
    fn derived_and_supplied_equivalent_graphs_validate_identically() {
        let unit = graph_unit(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/p/helper.rue", "helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let inputs = ModuleResolutionInputs::from_metadata(unit.source_metadata());
        let derived =
            resolve_canonical_import_graph(unit.import_directives().unwrap(), &inputs, None)
                .unwrap();
        let supplied = CanonicalImportGraph::from_supplied(
            derived.root().clone(),
            derived.records().to_vec(),
            &inputs,
        )
        .unwrap();
        assert_eq!(derived, supplied);
        assert_eq!(
            validate_canonical_import_graph(&derived, &inputs),
            validate_canonical_import_graph(&supplied, &inputs)
        );
    }

    #[test]
    fn cycles_are_legal_stable_components_while_diamonds_are_acyclic() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalImportGraphValidation>();

        let inputs = supplied_inputs(&["a.rue", "b.rue", "c.rue", "d.rue"], "a.rue");
        let cycle = CanonicalImportGraph::from_supplied(
            ModuleId::from_logical_path("a.rue").unwrap(),
            vec![
                resolved("b.rue", "a.rue", "a.rue"),
                resolved("a.rue", "b.rue", "b.rue"),
            ],
            &inputs,
        )
        .expect("cycles are valid Rue topology");
        let report = validate_canonical_import_graph(&cycle, &inputs);
        assert!(report.is_valid());
        assert_eq!(report.cycles().len(), 1);
        assert_eq!(
            report.cycles()[0]
                .modules()
                .iter()
                .map(ModuleId::as_str)
                .collect::<Vec<_>>(),
            ["a.rue", "b.rue"]
        );

        let diamond = CanonicalImportGraph::from_supplied(
            ModuleId::from_logical_path("a.rue").unwrap(),
            vec![
                resolved("c.rue", "d.rue", "d.rue"),
                resolved("a.rue", "c.rue", "c.rue"),
                resolved("b.rue", "d.rue", "d.rue"),
                resolved("a.rue", "b.rue", "b.rue"),
            ],
            &inputs,
        )
        .expect("diamonds are valid acyclic topology");
        let report = validate_canonical_import_graph(&diamond, &inputs);
        assert!(report.is_valid());
        assert!(report.cycles().is_empty());
    }

    #[test]
    fn deep_acyclic_graph_validation_does_not_use_the_process_stack() {
        const MODULE_COUNT: usize = 20_000;
        let modules: Vec<_> = (0..MODULE_COUNT)
            .map(|index| ModuleId::from_logical_path(&format!("module-{index:05}.rue")).unwrap())
            .collect();
        let mut adjacency: BTreeMap<_, BTreeSet<_>> = modules
            .iter()
            .cloned()
            .map(|module| (module, BTreeSet::new()))
            .collect();
        for pair in modules.windows(2) {
            adjacency.get_mut(&pair[0]).unwrap().insert(pair[1].clone());
        }

        assert!(cycle_components(&adjacency).is_empty());
    }

    #[test]
    fn supplied_malformed_findings_are_typed_sorted_and_fail_closed() {
        let inputs = supplied_inputs(&["a.rue", "b.rue", "c.rue"], "a.rue");
        let a = ModuleId::from_logical_path("a.rue").unwrap();
        let b = ModuleId::from_logical_path("b.rue").unwrap();
        let c = ModuleId::from_logical_path("c.rue").unwrap();
        let foreign = ModuleId::from_logical_path("foreign.rue").unwrap();
        let duplicate = resolved("a.rue", "duplicate", "b.rue");
        let mut records = vec![
            duplicate.clone(),
            CanonicalImportRecord::new(a.clone(), "missing", CanonicalImportResolution::Missing),
            CanonicalImportRecord::new(
                a.clone(),
                "ambiguous",
                CanonicalImportResolution::Ambiguous {
                    file_module: b.clone(),
                    directory_module: foreign.clone(),
                },
            ),
            resolved("a.rue", "conflict", "b.rue"),
            resolved("a.rue", "conflict", "c.rue"),
            CanonicalImportRecord::new(
                foreign.clone(),
                "from-foreign",
                CanonicalImportResolution::Resolved(b.clone()),
            ),
            CanonicalImportRecord::new(
                a,
                "to-foreign",
                CanonicalImportResolution::Resolved(foreign.clone()),
            ),
            duplicate,
        ];
        let run = |records: Vec<CanonicalImportRecord>| {
            CanonicalImportGraph::from_supplied(foreign.clone(), records, &inputs).unwrap_err()
        };
        let first = run(records.clone());
        records.reverse();
        let reversed = run(records);
        assert_eq!(first, reversed);
        assert!(!first.is_valid());
        assert!(first.cycles().is_empty());
        assert!(first.problems().windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            first
                .problems()
                .iter()
                .any(|problem| matches!(problem, CanonicalImportGraphProblem::RootMismatch { .. }))
        );
        assert!(
            first.problems().iter().any(|problem| matches!(
                problem,
                CanonicalImportGraphProblem::RootNotInModuleSet(_)
            ))
        );
        assert!(
            first
                .problems()
                .iter()
                .any(|problem| matches!(problem, CanonicalImportGraphProblem::DuplicateRecord(_)))
        );
        assert!(first.problems().iter().any(|problem| matches!(
            problem,
            CanonicalImportGraphProblem::ConflictingCanonicalKey { resolutions, .. }
                if resolutions.as_ref()
                    == [
                        CanonicalImportResolution::Resolved(b.clone()),
                        CanonicalImportResolution::Resolved(c.clone()),
                    ]
        )));
        assert!(first.problems().iter().any(|problem| matches!(
            problem,
            CanonicalImportGraphProblem::MissingResolution { .. }
        )));
        assert!(first.problems().iter().any(|problem| matches!(
            problem,
            CanonicalImportGraphProblem::AmbiguousResolution { .. }
        )));
        assert!(
            first.problems().iter().any(|problem| matches!(
                problem,
                CanonicalImportGraphProblem::ForeignImporter { .. }
            ))
        );
        assert!(first.problems().iter().any(|problem| matches!(
            problem,
            CanonicalImportGraphProblem::ForeignResolvedTarget { .. }
        )));
    }
}
