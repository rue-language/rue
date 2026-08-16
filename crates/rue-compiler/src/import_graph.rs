//! Stable import sites and the canonical graph committed by source discovery.
//!
//! Parsing creates [`ImportDirective`] values. Source discovery owns candidate
//! resolution and commits [`CanonicalImportGraph`]; this module only defines and
//! validates those durable artifacts for downstream consumers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::{ModuleId, ModuleResolutionInputs, SemanticInputDescriptor, StableOptLevel};
use rue_air::normalize_module_path;

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

/// Canonically ordered import sites from one lowered source snapshot, held as
/// bounded `Arc`-shared sorted tiers. A strictly-additive successor retains its
/// exact new-module delta while compacting equal-magnitude tail tiers. Equality,
/// hashing, and debug rendering are logical over
/// the merged order; the contiguous slice materializes lazily, at most once
/// per value, off the successor staging path.
#[derive(Clone)]
pub struct ImportDirectives(crate::shared_segments::SharedSegments<ImportDirective>);

fn import_directive_order(a: &ImportDirective, b: &ImportDirective) -> std::cmp::Ordering {
    a.cmp(b)
}

impl ImportDirectives {
    pub(crate) fn from_records(mut records: Vec<ImportDirective>) -> Self {
        records.sort();
        Self(crate::shared_segments::SharedSegments::flat(
            records.into(),
            import_directive_order,
        ))
    }

    /// A strictly-additive successor sharing every segment of `base` by
    /// reference and appending only `delta` (sorted here; must be disjoint —
    /// sites of modules `base` does not carry).
    pub(crate) fn extend(base: &ImportDirectives, delta: Vec<ImportDirective>) -> Self {
        Self(crate::shared_segments::SharedSegments::extend(
            &base.0, delta,
        ))
    }

    /// Import sites ordered by logical module, source offset, then specifier.
    pub fn as_slice(&self) -> &[ImportDirective] {
        self.0.as_slice()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.len() == 0
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ImportDirective> {
        self.as_slice().iter()
    }
}

impl Default for ImportDirectives {
    fn default() -> Self {
        Self::from_records(Vec::new())
    }
}

impl PartialEq for ImportDirectives {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for ImportDirectives {}

impl std::hash::Hash for ImportDirectives {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl std::fmt::Debug for ImportDirectives {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ImportDirectives").field(&self.0).finish()
    }
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

/// Canonical order for the record sequence: importer, normalized spelling, then
/// outcome (the record's derived total order).
fn record_order(a: &CanonicalImportRecord, b: &CanonicalImportRecord) -> std::cmp::Ordering {
    a.cmp(b)
}

/// Canonical resolved topology, independent of import-site occurrences.
///
/// The record sequence is a [`SharedSegments`], so a strictly-additive
/// trusted-toolchain successor (RUE-1112) keeps an exact sorted delta and shares
/// untouched size tiers. Equal-magnitude tail tiers compact to bound lookup and
/// merge work.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalImportGraph {
    root: ModuleId,
    records: crate::shared_segments::SharedSegments<CanonicalImportRecord>,
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
            records: crate::shared_segments::SharedSegments::flat(records.into(), record_order),
        }
    }

    /// Build a strictly-additive successor graph that shares `base`'s record
    /// sequence by reference and appends `delta` (RUE-1112). `delta`'s importers
    /// must be disjoint from `base`'s.
    pub(crate) fn from_additive_successor(
        root: ModuleId,
        base: &CanonicalImportGraph,
        delta: Vec<CanonicalImportRecord>,
    ) -> Self {
        Self {
            root,
            records: crate::shared_segments::SharedSegments::extend(&base.records, delta),
        }
    }

    /// The shared record-segment representation (RUE-1112 successor sharing).
    pub(crate) fn record_segments(
        &self,
    ) -> &crate::shared_segments::SharedSegments<CanonicalImportRecord> {
        &self.records
    }

    pub fn root(&self) -> &ModuleId {
        &self.root
    }

    /// Records sorted by importer, normalized spelling, then outcome.
    pub fn records(&self) -> &[CanonicalImportRecord] {
        self.records.as_slice()
    }

    /// Find the unique record for one canonical import key without re-deriving
    /// resolution. The graph's validated order makes this logarithmic.
    #[cfg(test)]
    pub(crate) fn record_for(
        &self,
        importer: &ModuleId,
        normalized_specifier: &str,
    ) -> Option<&CanonicalImportRecord> {
        self.record_for_with(importer, normalized_specifier, || {})
    }

    #[cfg(test)]
    fn record_for_with(
        &self,
        importer: &ModuleId,
        normalized_specifier: &str,
        mut compared: impl FnMut(),
    ) -> Option<&CanonicalImportRecord> {
        self.records()
            .binary_search_by(|record| {
                compared();
                record
                    .importer()
                    .cmp(importer)
                    .then_with(|| record.normalized_specifier().cmp(normalized_specifier))
            })
            .ok()
            .map(|index| &self.records()[index])
    }

    #[cfg(test)]
    pub(crate) fn record_for_with_comparison_count(
        &self,
        importer: &ModuleId,
        normalized_specifier: &str,
    ) -> (Option<&CanonicalImportRecord>, usize) {
        let mut comparisons = 0;
        let record = self.record_for_with(importer, normalized_specifier, || comparisons += 1);
        (record, comparisons)
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
            records: crate::shared_segments::SharedSegments::flat(records.into(), record_order),
        })
    }
}

/// A parsed or merged revision, viewed as the inputs an import-free canonical
/// graph is derived from.
pub(crate) trait ImportFreeRevision {
    fn root(&self) -> &ModuleId;
    fn modules(&self) -> &[Arc<crate::parsed_modules::ParsedModule>];
    fn import_directives(&self) -> &ImportDirectives;
}

impl ImportFreeRevision for crate::ParsedProgram {
    fn root(&self) -> &ModuleId {
        crate::ParsedProgram::root(self)
    }
    fn modules(&self) -> &[Arc<crate::parsed_modules::ParsedModule>] {
        crate::ParsedProgram::modules(self)
    }
    fn import_directives(&self) -> &ImportDirectives {
        crate::ParsedProgram::import_directives(self)
    }
}

impl ImportFreeRevision for crate::canonical_merge::CanonicalMergedAst {
    fn root(&self) -> &ModuleId {
        crate::canonical_merge::CanonicalMergedAst::root(self)
    }
    fn modules(&self) -> &[Arc<crate::parsed_modules::ParsedModule>] {
        crate::canonical_merge::CanonicalMergedAst::modules(self)
    }
    fn import_directives(&self) -> &ImportDirectives {
        crate::canonical_merge::CanonicalMergedAst::import_directives(self)
    }
}

/// The uniquely valid canonical graph for a revision that declares no import
/// directives: no records over that revision's own module set.
///
/// This is not a second resolver. An import-free revision has exactly one legal
/// topology, so it needs no candidate precedence, no host observation, and no
/// discovery epoch. The session's semantic path and the lower-layer unit tests
/// that analyze an import-free fixture both construct it here, so neither one
/// re-derives module resolution inputs on its own.
pub(crate) fn import_free_canonical_graph(
    program: &impl ImportFreeRevision,
) -> Result<CanonicalImportGraph, crate::CompileErrors> {
    if !program.import_directives().is_empty() {
        return Err(crate::CompileErrors::from(
            crate::CompileError::without_span(crate::ErrorKind::InvalidCompilerInput(
                "an import-bearing revision has no import-free canonical graph; resolve it through discovery"
                    .into(),
            )),
        ));
    }
    let inputs = ModuleResolutionInputs::new(
        program.root().clone(),
        program
            .modules()
            .iter()
            .map(|module| crate::ModuleResolutionInput {
                module: module.module_id().clone(),
                physical_path: Arc::from(module.physical_path()),
            })
            .collect(),
    )
    .map_err(crate::CompileErrors::from)?;
    CanonicalImportGraph::from_supplied(program.root().clone(), Vec::new(), &inputs).map_err(
        |validation| {
            crate::CompileErrors::from(crate::CompileError::without_span(
                crate::ErrorKind::InvalidCompilerInput(format!(
                    "invalid import-free semantic graph: {validation:?}"
                )),
            ))
        },
    )
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
pub(crate) struct ResolvedProgramRevision {
    semantic: SemanticInputDescriptor,
    imports: CanonicalImportGraph,
}

impl ResolvedProgramRevision {
    /// Compose semantic inputs and durable imports.
    ///
    /// The canonical graph itself ignores physical relocation, while this
    /// complete revision intentionally retains the explicit physical
    /// resolution inputs embedded in `semantic`.
    pub(crate) fn new(semantic: SemanticInputDescriptor, imports: CanonicalImportGraph) -> Self {
        Self { semantic, imports }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn imports(&self) -> &CanonicalImportGraph {
        &self.imports
    }
}

/// Complete code-generation identity after canonical import resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ResolvedCodegenRevision {
    program: ResolvedProgramRevision,
    opt_level: StableOptLevel,
}

impl ResolvedCodegenRevision {
    pub(crate) fn new(program: ResolvedProgramRevision, opt_level: StableOptLevel) -> Self {
        Self { program, opt_level }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn program(&self) -> &ResolvedProgramRevision {
        &self.program
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn opt_level(&self) -> StableOptLevel {
        self.opt_level
    }
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

/// Validate a strictly-additive trusted-toolchain successor graph incrementally
/// (RUE-1112).
///
/// The predecessor graph closed valid, and the newly appended modules have no
/// incoming edges from predecessor modules (nothing already in the compilation
/// imports a freshly demanded trusted leaf). Therefore:
///
/// * predecessor records were already validated and carry no problems, so they
///   are not re-examined;
/// * a new record can only introduce a problem of its own — a foreign importer
///   or target, a missing/ambiguous resolution, or a duplicate/conflict among the
///   new records themselves (new importers are disjoint from predecessor
///   importers, so no new record can conflict with a predecessor record);
/// * any cycle involving a new module consists entirely of new modules, because a
///   cycle edge into a new module can only originate from another new module.
///
/// This validates only `new_records` and detects cycles only within the
/// new-module-induced subgraph, unioning the result with the carried predecessor
/// validation — never re-walking the predecessor topology. `inputs` supplies the
/// merged module set for membership checks only.
pub(crate) fn validate_additive_successor(
    predecessor: &CanonicalImportGraphValidation,
    new_records: &[CanonicalImportRecord],
    inputs: &ModuleResolutionInputs,
    new_modules: &BTreeSet<ModuleId>,
) -> CanonicalImportGraphValidation {
    // Membership is checked by binary search over the shared, structurally
    // extended resolution inputs, so the predecessor module table is never
    // materialized into a set on the acquisition path (RUE-1112).
    let mut problems = predecessor.problems().to_vec();
    let mut by_key: BTreeMap<(ModuleId, Arc<str>), BTreeSet<CanonicalImportResolution>> =
        BTreeMap::new();
    // Cycle adjacency confined to new-module → new-module edges; every key and
    // target is a new module, so `cycle_components` is well formed.
    let mut new_adjacency: BTreeMap<ModuleId, BTreeSet<ModuleId>> = new_modules
        .iter()
        .cloned()
        .map(|module| (module, BTreeSet::new()))
        .collect();
    let mut sorted = new_records.to_vec();
    sorted.sort();
    for pair in sorted.windows(2) {
        if pair[0] == pair[1] {
            problems.push(CanonicalImportGraphProblem::DuplicateRecord(
                pair[0].clone(),
            ));
        }
    }
    for record in &sorted {
        let key = (record.importer.clone(), record.normalized_specifier.clone());
        by_key
            .entry(key.clone())
            .or_default()
            .insert(record.resolution.clone());
        if !inputs.contains(&record.importer) {
            problems.push(CanonicalImportGraphProblem::ForeignImporter {
                importer: key.0,
                normalized_specifier: key.1,
            });
        }
        match &record.resolution {
            CanonicalImportResolution::Resolved(target) => {
                if !inputs.contains(target) {
                    problems.push(CanonicalImportGraphProblem::ForeignResolvedTarget {
                        importer: record.importer.clone(),
                        normalized_specifier: record.normalized_specifier.clone(),
                        target: target.clone(),
                    });
                } else if inputs.contains(&record.importer)
                    && new_modules.contains(target)
                    && let Some(edges) = new_adjacency.get_mut(&record.importer)
                {
                    edges.insert(target.clone());
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
                    if !inputs.contains(target) {
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
    let mut cycles = predecessor.cycles().to_vec();
    cycles.extend(cycle_components(&new_adjacency));
    cycles.sort();
    cycles.dedup();
    CanonicalImportGraphValidation {
        problems: problems.into(),
        cycles: cycles.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(names: &[&str], root: &str) -> ModuleResolutionInputs {
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

    fn missing(importer: &str, specifier: &str) -> CanonicalImportRecord {
        CanonicalImportRecord::new(
            ModuleId::from_logical_path(importer).unwrap(),
            specifier,
            CanonicalImportResolution::Missing,
        )
    }

    /// The canonical graph's validated record order makes per-directive lookup
    /// a binary search. A wide importer must stay within the logarithmic probe
    /// budget while covering every directive.
    #[test]
    fn record_lookup_is_logarithmic_and_covers_every_directive() {
        const DIRECTIVES: usize = 2_048;
        let records = (0..DIRECTIVES)
            .map(|index| {
                let module = format!("module_{index:04}.rue");
                resolved("main.rue", &module, &module)
            })
            .collect::<Vec<_>>();
        let graph = CanonicalImportGraph::from_discovery_records(
            ModuleId::from_logical_path("main.rue").unwrap(),
            records,
        );
        let importer = ModuleId::from_logical_path("main.rue").unwrap();
        let comparisons_per_lookup = (usize::BITS - (DIRECTIVES - 1).leading_zeros()) as usize + 1;
        let mut comparisons = 0;
        for index in 0..DIRECTIVES {
            let specifier = format!("module_{index:04}.rue");
            let (record, probes) = graph.record_for_with_comparison_count(&importer, &specifier);
            let record = record.unwrap_or_else(|| panic!("graph covers directive {specifier}"));
            assert_eq!(
                record.resolution(),
                &CanonicalImportResolution::Resolved(
                    ModuleId::from_logical_path(&specifier).unwrap()
                ),
            );
            comparisons += probes;
        }
        assert!(
            graph.record_for(&importer, "missing.rue").is_none(),
            "an uncovered specifier resolves to no record"
        );
        assert!(
            comparisons <= DIRECTIVES * comparisons_per_lookup,
            "{comparisons} comparisons exceeded {} logarithmic lookup probes",
            DIRECTIVES * comparisons_per_lookup
        );
    }

    /// The incremental additive-successor validation must be identical to a full
    /// validation of the merged graph, so production can carry the predecessor
    /// validation and validate only the delta. This exercises the same code path
    /// the trusted-toolchain successor close uses, across a clean additive delta
    /// and deltas that introduce a new-module self-cycle, a new-module two-cycle,
    /// a new-to-predecessor edge, and a missing resolution.
    #[test]
    fn additive_successor_validation_equals_full_validation() {
        // A valid predecessor graph: root a imports b, b imports c.
        let predecessor_records = vec![
            resolved("a.rue", "b.rue", "b.rue"),
            resolved("b.rue", "c.rue", "c.rue"),
        ];
        let predecessor_inputs = inputs(&["a.rue", "b.rue", "c.rue"], "a.rue");
        let predecessor_graph = CanonicalImportGraph::from_discovery_records(
            ModuleId::from_logical_path("a.rue").unwrap(),
            predecessor_records.clone(),
        );
        let predecessor_validation =
            validate_canonical_import_graph(&predecessor_graph, &predecessor_inputs);
        assert!(predecessor_validation.is_valid());

        let new_module = |name: &str| ModuleId::from_logical_path(name).unwrap();

        // Each case: (added module names, delta records).
        let cases: Vec<(Vec<&str>, Vec<CanonicalImportRecord>)> = vec![
            // Clean additive: new x imports the existing c and a fresh y.
            (
                vec!["x.rue", "y.rue"],
                vec![
                    resolved("x.rue", "c.rue", "c.rue"),
                    resolved("x.rue", "y.rue", "y.rue"),
                ],
            ),
            // New-module self-cycle.
            (vec!["x.rue"], vec![resolved("x.rue", "x.rue", "x.rue")]),
            // New-module two-cycle among the delta.
            (
                vec!["x.rue", "y.rue"],
                vec![
                    resolved("x.rue", "y.rue", "y.rue"),
                    resolved("y.rue", "x.rue", "x.rue"),
                ],
            ),
            // New-to-predecessor edge (cannot create a cycle back into the delta).
            (vec!["x.rue"], vec![resolved("x.rue", "b.rue", "b.rue")]),
            // Delta with a missing resolution.
            (vec!["x.rue"], vec![missing("x.rue", "nope.rue")]),
        ];

        for (added, delta_records) in cases {
            let mut all_names = vec!["a.rue", "b.rue", "c.rue"];
            all_names.extend(added.iter().copied());
            let merged_inputs = inputs(&all_names, "a.rue");
            let new_modules: BTreeSet<ModuleId> =
                added.iter().map(|name| new_module(name)).collect();

            let mut merged_records = predecessor_records.clone();
            merged_records.extend(delta_records.iter().cloned());
            let merged_graph = CanonicalImportGraph::from_discovery_records(
                ModuleId::from_logical_path("a.rue").unwrap(),
                merged_records,
            );

            let full = validate_canonical_import_graph(&merged_graph, &merged_inputs);
            let incremental = validate_additive_successor(
                &predecessor_validation,
                &delta_records,
                &merged_inputs,
                &new_modules,
            );
            assert_eq!(
                incremental, full,
                "incremental successor validation must equal full validation for added={added:?}"
            );
        }
    }

    #[test]
    fn cycles_are_legal_stable_components_while_diamonds_are_acyclic() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalImportGraphValidation>();

        let inputs = inputs(&["a.rue", "b.rue", "c.rue", "d.rue"], "a.rue");
        let cycle = CanonicalImportGraph::from_supplied(
            ModuleId::from_logical_path("a.rue").unwrap(),
            vec![
                resolved("b.rue", "a.rue", "a.rue"),
                resolved("a.rue", "b.rue", "b.rue"),
            ],
            &inputs,
        )
        .unwrap();
        assert_eq!(
            validate_canonical_import_graph(&cycle, &inputs).cycles()[0]
                .modules()
                .iter()
                .map(ModuleId::as_str)
                .collect::<Vec<_>>(),
            ["a.rue", "b.rue"]
        );

        let diamond = CanonicalImportGraph::from_supplied(
            ModuleId::from_logical_path("a.rue").unwrap(),
            vec![
                resolved("a.rue", "b.rue", "b.rue"),
                resolved("a.rue", "c.rue", "c.rue"),
                resolved("b.rue", "d.rue", "d.rue"),
                resolved("c.rue", "d.rue", "d.rue"),
            ],
            &inputs,
        )
        .unwrap();
        assert!(
            validate_canonical_import_graph(&diamond, &inputs)
                .cycles()
                .is_empty()
        );
    }

    #[test]
    fn malformed_supplied_graphs_fail_closed_with_typed_findings() {
        let inputs = inputs(&["a.rue", "b.rue", "c.rue"], "a.rue");
        let a = ModuleId::from_logical_path("a.rue").unwrap();
        let b = ModuleId::from_logical_path("b.rue").unwrap();
        let c = ModuleId::from_logical_path("c.rue").unwrap();
        let foreign = ModuleId::from_logical_path("foreign.rue").unwrap();
        let duplicate = resolved("a.rue", "duplicate", "b.rue");
        let records = vec![
            duplicate.clone(),
            duplicate,
            CanonicalImportRecord::new(a.clone(), "missing", CanonicalImportResolution::Missing),
            resolved("a.rue", "conflict", "b.rue"),
            resolved("a.rue", "conflict", "c.rue"),
            CanonicalImportRecord::new(
                a,
                "foreign",
                CanonicalImportResolution::Resolved(foreign.clone()),
            ),
        ];
        let report = CanonicalImportGraph::from_supplied(foreign, records, &inputs).unwrap_err();
        assert!(!report.is_valid());
        assert!(report.problems().windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            report
                .problems()
                .iter()
                .any(|problem| matches!(problem, CanonicalImportGraphProblem::DuplicateRecord(_)))
        );
        assert!(report.problems().iter().any(|problem| matches!(
            problem,
            CanonicalImportGraphProblem::ConflictingCanonicalKey { resolutions, .. }
                if resolutions.as_ref()
                    == [
                        CanonicalImportResolution::Resolved(b.clone()),
                        CanonicalImportResolution::Resolved(c.clone()),
                    ]
        )));
        assert!(report.problems().iter().any(|problem| matches!(
            problem,
            CanonicalImportGraphProblem::MissingResolution { .. }
        )));
        assert!(report.problems().iter().any(|problem| matches!(
            problem,
            CanonicalImportGraphProblem::ForeignResolvedTarget { .. }
        )));
    }

    #[test]
    fn deep_acyclic_validation_is_iterative() {
        const MODULE_COUNT: usize = 20_000;
        let modules = (0..MODULE_COUNT)
            .map(|index| ModuleId::from_logical_path(&format!("module-{index:05}.rue")).unwrap())
            .collect::<Vec<_>>();
        let mut adjacency = modules
            .iter()
            .cloned()
            .map(|module| (module, BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();
        for pair in modules.windows(2) {
            adjacency.get_mut(&pair[0]).unwrap().insert(pair[1].clone());
        }
        assert!(cycle_components(&adjacency).is_empty());
    }
}
