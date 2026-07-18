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
    pub(crate) fn program(&self) -> &ResolvedProgramRevision {
        &self.program
    }

    #[cfg(test)]
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
