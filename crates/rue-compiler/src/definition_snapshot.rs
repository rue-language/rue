//! Durable parsed names and deterministic snapshot-local occurrences.
//!
//! Parser ASTs deliberately use request-local [`FileId`] and [`Spur`] values.
//! Those are excellent compact handles within one compilation, but neither is
//! suitable as a key retained by compiler services between requests.  This
//! module resolves those handles at the syntax boundary and records a stable
//! module path and owned name alongside concrete kinds and the current
//! request's diagnostic locations.

use std::collections::HashMap;
use std::sync::Arc;

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_parser::{Ident, Item, ast::Visibility};
use rue_span::{FileId, Span};
use tracing::info_span;

use crate::{ModuleId, SourceSnapshot};

/// The concrete syntax kind of a parsed top-level definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DefinitionKind {
    /// A free function.
    Function,
    /// A named structure type.
    Struct,
    /// A named enumeration type.
    Enum,
    /// A destructor, keyed by the type name following `drop fn`.
    Destructor,
    /// A file-level constant.
    Const,
}

/// The name-resolution namespace containing a parsed definition candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DefinitionNamespace {
    /// Presemantic module-item candidates: functions, structures,
    /// enumerations, and constants share this key space until semantic
    /// evaluation can distinguish value constants from module bindings.
    ModuleItem,
    /// Destructor declarations, keyed by the type name following `drop fn`.
    Destructor,
}

/// A durable name-binding key for one or more definition candidates.
///
/// This is deliberately not a unique semantic `DefId`: cross-kind module-item
/// collisions and repeated declarations share a name key. Name resolution can
/// use it to retrieve every candidate, then assign any eventual semantic
/// identity only after duplicate and ambiguity checks. All components are
/// owned semantic values; the key contains no parser interner handle,
/// request-local file ID, or diagnostic path. Clones share their strings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinitionNameKey {
    module: ModuleId,
    namespace: DefinitionNamespace,
    name: Arc<str>,
}

impl DefinitionNameKey {
    /// Construct a name-binding key from durable, owned components.
    pub fn new(module: ModuleId, namespace: DefinitionNamespace, name: impl AsRef<str>) -> Self {
        Self {
            module,
            namespace,
            name: Arc::from(name.as_ref()),
        }
    }

    /// The durable module containing this definition.
    #[inline]
    #[cfg(test)]
    pub fn module(&self) -> &ModuleId {
        &self.module
    }

    /// The name-resolution namespace containing this candidate.
    #[inline]
    #[cfg(test)]
    pub fn namespace(&self) -> DefinitionNamespace {
        self.namespace
    }

    /// The resolved, owned source name.
    #[inline]
    #[cfg(test)]
    pub fn name(&self) -> &str {
        self.name.as_ref()
    }
}

/// The unique location of one definition record within a single snapshot.
///
/// IDs distinguish duplicate and cross-kind candidates, but are meaningful
/// only with the [`DefinitionSnapshot`] that issued them. They are not durable
/// across source revisions or independently built snapshots, even when their
/// numeric components happen to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinitionOccurrenceId {
    module_index: usize,
    definition_index: usize,
}

/// Compatibility name; occurrence IDs are always snapshot-local.
pub type DefinitionId = DefinitionOccurrenceId;

/// One definition candidate paired with its current request's source locator.
///
/// The name key is suitable for comparison across requests in the same source
/// graph identity. The ID and locator fields are intentionally snapshot-local
/// and support diagnostics and navigation after candidates have been matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionRecord {
    id: DefinitionId,
    name_key: DefinitionNameKey,
    kind: DefinitionKind,
    visibility: Option<Visibility>,
    file_id: FileId,
    name_span: Span,
    declaration_span: Span,
}

impl DefinitionRecord {
    /// This record's unique, snapshot-local ID.
    #[inline]
    pub fn id(&self) -> DefinitionId {
        self.id
    }

    /// The durable name-binding key shared by colliding candidates.
    #[inline]
    #[cfg(test)]
    pub fn name_key(&self) -> &DefinitionNameKey {
        &self.name_key
    }

    /// The concrete syntax kind of this candidate.
    #[inline]
    pub fn kind(&self) -> DefinitionKind {
        self.kind
    }

    /// The parsed visibility of this definition.
    ///
    /// Drop functions have no visibility modifier and return `None` rather
    /// than inventing a source property that does not exist.
    #[inline]
    pub fn visibility(&self) -> Option<Visibility> {
        self.visibility
    }

    /// The current request's span for the complete declaration.
    #[inline]
    pub fn declaration_span(&self) -> Span {
        self.declaration_span
    }
}

/// The definitions belonging to one logical module in a syntax snapshot.
///
/// Empty source modules are represented by values whose [`Self::definitions`]
/// slice is empty. Definitions are ordered by source position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDefinition {
    key: ModuleId,
    file_id: FileId,
    definitions: Vec<DefinitionRecord>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DefinitionShardWork {
    pub shards_indexed: usize,
    pub shards_reused: usize,
    pub shards_rebuilt: usize,
}

#[derive(Debug, Clone)]
pub struct DefinitionShard {
    key: ModuleId,
    file_id: FileId,
    records: Arc<[DefinitionShardRecord]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefinitionShardRecord {
    namespace: DefinitionNamespace,
    kind: DefinitionKind,
    visibility: Option<Visibility>,
    name: Arc<str>,
    name_span: Span,
    declaration_span: Span,
}

impl DefinitionShard {
    fn matches_index(
        &self,
        module: &crate::parsed_modules::ParsedModule,
        index: &crate::revisioned_query_database::ProjectedModuleIndex,
    ) -> bool {
        self.file_id == module.file_id()
            && self.records.len() == index.definitions.len()
            && self
                .records
                .iter()
                .zip(index.definitions.iter())
                .all(|(record, candidate)| {
                    record.namespace == candidate.namespace
                        && record.kind == candidate.kind
                        && record.visibility == candidate.visibility
                        && record.name == candidate.name
                        && record.name_span == candidate.name_span
                        && record.declaration_span == candidate.declaration_span
                })
    }
}

impl ModuleDefinition {
    /// The durable logical identity of this module.
    #[inline]
    #[cfg(test)]
    pub fn key(&self) -> &ModuleId {
        &self.key
    }

    /// The file ID assigned to this module in the current request.
    #[inline]
    #[cfg(test)]
    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    /// Whether this module contains no top-level definitions.
    #[inline]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }
}

/// An immutable, deterministic view of every parsed top-level definition.
///
/// Modules are ordered by canonical logical path, independently of parser
/// input order and numeric file IDs. Definitions within each module are
/// ordered by source position. Duplicate and cross-kind declarations are
/// retained, so multiple records may intentionally carry equal
/// [`DefinitionNameKey`] values.
///
/// The exact source snapshot is retained so current-request file IDs and spans
/// always have a source revision with which they can be interpreted. Whole
/// snapshots deliberately do not implement equality: durable names alone are
/// insufficient to decide revision equality or cache validity.
#[derive(Debug, Clone)]
pub struct DefinitionSnapshot {
    source_snapshot: SourceSnapshot,
    #[cfg(test)]
    root_module: ModuleId,
    modules: Vec<ModuleDefinition>,
    definitions_by_name: HashMap<DefinitionNameKey, Vec<DefinitionId>>,
    shards: Arc<[Arc<DefinitionShard>]>,
}

impl DefinitionSnapshot {
    pub(crate) fn from_module_indexes_reusing(
        program: &crate::parsed_modules::ParsedProgram,
        indexes: &[crate::revisioned_query_database::ProjectedModuleIndex],
        previous: Option<&Self>,
    ) -> CompileResult<(Self, DefinitionShardWork)> {
        let _span = info_span!(
            "definition_snapshot_modules",
            module_count = program.modules().len()
        )
        .entered();
        if indexes.len() != program.modules().len()
            || indexes
                .iter()
                .zip(program.modules())
                .any(|(index, module)| index.revision != *module.revision())
        {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "module indexes do not match the parsed program".into(),
            )));
        }
        let source_snapshot = SourceSnapshot::from_parsed_modules(program)?;
        let mut modules = Vec::with_capacity(program.modules().len());
        let mut work = DefinitionShardWork {
            shards_indexed: previous.map_or(0, |snapshot| snapshot.shards.len()),
            ..DefinitionShardWork::default()
        };
        let mut shards = Vec::with_capacity(program.modules().len());
        let definition_count = indexes.iter().map(|index| index.definitions.len()).sum();
        let mut definitions_by_name =
            HashMap::<DefinitionNameKey, Vec<DefinitionId>>::with_capacity(definition_count);

        for (module_index, (module, index)) in program.modules().iter().zip(indexes).enumerate() {
            let candidate_records = || {
                index
                    .definitions
                    .iter()
                    .map(|candidate| DefinitionShardRecord {
                        namespace: candidate.namespace,
                        kind: candidate.kind,
                        visibility: candidate.visibility,
                        name: candidate.name.clone(),
                        name_span: candidate.name_span,
                        declaration_span: candidate.declaration_span,
                    })
                    .collect::<Vec<_>>()
            };
            let shard = previous
                .and_then(|snapshot| {
                    snapshot
                        .shards
                        .binary_search_by(|shard| shard.key.cmp(module.module_id()))
                        .ok()
                        .map(|position| snapshot.shards[position].clone())
                })
                .filter(|shard| shard.matches_index(module, index));
            let shard = if let Some(shard) = shard {
                work.shards_reused += 1;
                shard
            } else {
                work.shards_rebuilt += 1;
                Arc::new(DefinitionShard {
                    key: module.module_id().clone(),
                    file_id: module.file_id(),
                    records: candidate_records().into(),
                })
            };
            let mut definitions = Vec::with_capacity(shard.records.len());
            for (definition_index, candidate) in shard.records.iter().enumerate() {
                let id = DefinitionId {
                    module_index,
                    definition_index,
                };
                let name_key = DefinitionNameKey::new(
                    module.module_id().clone(),
                    candidate.namespace,
                    &candidate.name,
                );
                definitions_by_name
                    .entry(name_key.clone())
                    .or_default()
                    .push(id);
                definitions.push(DefinitionRecord {
                    id,
                    name_key,
                    kind: candidate.kind,
                    visibility: candidate.visibility,
                    file_id: module.file_id(),
                    name_span: candidate.name_span,
                    declaration_span: candidate.declaration_span,
                });
            }
            modules.push(ModuleDefinition {
                key: module.module_id().clone(),
                file_id: module.file_id(),
                definitions,
            });
            shards.push(shard);
        }

        Ok((
            Self {
                source_snapshot,
                #[cfg(test)]
                root_module: program.root().clone(),
                modules,
                definitions_by_name,
                shards: shards.into(),
            },
            work,
        ))
    }

    /// The exact immutable source revision to which all record locations refer.
    #[inline]
    pub fn source_snapshot(&self) -> &SourceSnapshot {
        &self.source_snapshot
    }

    /// The explicitly designated root module identity.
    #[inline]
    #[cfg(test)]
    pub fn root_module(&self) -> &ModuleId {
        &self.root_module
    }

    /// All modules in canonical logical-path order, including empty modules.
    #[inline]
    #[cfg(test)]
    pub fn modules(&self) -> &[ModuleDefinition] {
        &self.modules
    }

    #[cfg(test)]
    pub fn shards(&self) -> &[Arc<DefinitionShard>] {
        &self.shards
    }

    /// Find a module by its durable identity.
    #[cfg(test)]
    pub fn module(&self, id: &ModuleId) -> Option<&ModuleDefinition> {
        self.modules
            .binary_search_by(|module| module.key.cmp(id))
            .ok()
            .map(|index| &self.modules[index])
    }

    /// Iterate all definitions in module-path then source-position order.
    #[cfg(test)]
    pub fn definitions(&self) -> impl DoubleEndedIterator<Item = &DefinitionRecord> + '_ {
        self.modules
            .iter()
            .flat_map(|module| module.definitions.iter())
    }

    /// Resolve a snapshot-local definition ID.
    ///
    /// IDs issued by another snapshot may coincidentally address a record here;
    /// callers must use an ID only with its issuing snapshot.
    pub fn definition(&self, id: DefinitionId) -> Option<&DefinitionRecord> {
        let definition = self
            .modules
            .get(id.module_index)?
            .definitions
            .get(id.definition_index)?;
        debug_assert_eq!(definition.id, id);
        Some(definition)
    }

    /// Look up every candidate with a durable name-binding key.
    ///
    /// Occurrences are returned in the snapshot's deterministic module-path
    /// then source-position order. Duplicate and cross-kind candidates are not
    /// collapsed.
    pub fn definitions_named<'a>(
        &'a self,
        name_key: &DefinitionNameKey,
    ) -> impl DoubleEndedIterator<Item = &'a DefinitionRecord> + ExactSizeIterator + 'a {
        self.definitions_by_name
            .get(name_key)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|&id| {
                self.definition(id)
                    .expect("name index contains only snapshot-issued definition IDs")
            })
    }

    /// The number of modules, including empty modules.
    #[inline]
    #[cfg(test)]
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DefinitionParts {
    pub(crate) namespace: DefinitionNamespace,
    pub(crate) kind: DefinitionKind,
    pub(crate) visibility: Option<Visibility>,
    pub(crate) name: Ident,
    pub(crate) declaration_span: Span,
}

pub(crate) fn definition_parts(item: &Item) -> Option<DefinitionParts> {
    let parts = match item {
        Item::Function(function) => DefinitionParts {
            namespace: DefinitionNamespace::ModuleItem,
            kind: DefinitionKind::Function,
            visibility: Some(function.visibility),
            name: function.name,
            declaration_span: function.span,
        },
        Item::Struct(structure) => DefinitionParts {
            namespace: DefinitionNamespace::ModuleItem,
            kind: DefinitionKind::Struct,
            visibility: Some(structure.visibility),
            name: structure.name,
            declaration_span: structure.span,
        },
        Item::Enum(enumeration) => DefinitionParts {
            namespace: DefinitionNamespace::ModuleItem,
            kind: DefinitionKind::Enum,
            visibility: Some(enumeration.visibility),
            name: enumeration.name,
            declaration_span: enumeration.span,
        },
        Item::DropFn(drop_function) => DefinitionParts {
            namespace: DefinitionNamespace::Destructor,
            kind: DefinitionKind::Destructor,
            visibility: None,
            name: drop_function.type_name,
            declaration_span: drop_function.span,
        },
        Item::Const(constant) => DefinitionParts {
            namespace: DefinitionNamespace::ModuleItem,
            kind: DefinitionKind::Const,
            visibility: Some(constant.visibility),
            name: constant.name,
            declaration_span: constant.span,
        },
        // A foreign `extern "C"` block declares several definitions at once, so
        // it does not fit the one-item/one-definition shape here. The definition
        // index expands it via `extern_definition_parts` (ADR-0064 C FFI).
        Item::Extern(_) => return None,
        Item::Error(_) => return None,
    };
    Some(parts)
}

/// Per-`fn` definition parts for a foreign `extern "C"` block. Each member is an
/// independent module-item definition keyed by its own declaration span.
pub(crate) fn extern_definition_parts(
    block: &rue_parser::ast::ExternBlock,
) -> impl Iterator<Item = DefinitionParts> + '_ {
    block.fns.iter().map(|foreign| DefinitionParts {
        namespace: DefinitionNamespace::ModuleItem,
        kind: DefinitionKind::Function,
        visibility: Some(Visibility::Private),
        name: foreign.name,
        declaration_span: foreign.span,
    })
}

pub(crate) fn validate_span(
    kind: &str,
    span: Span,
    containing_file_id: FileId,
    source_text: &str,
) -> CompileResult<()> {
    if span.file_id != containing_file_id {
        return Err(invalid_input(format!(
            "{kind} uses file ID {}, but its parsed file uses file ID {}",
            span.file_id.index(),
            containing_file_id.index(),
        )));
    }
    if span.start > span.end {
        return Err(invalid_input(format!(
            "{kind} has an inverted span {}..{} in file ID {}",
            span.start,
            span.end,
            containing_file_id.index(),
        )));
    }
    if span.end as usize > source_text.len() {
        return Err(invalid_input(format!(
            "{kind} span {}..{} exceeds the {}-byte source text for file ID {}",
            span.start,
            span.end,
            source_text.len(),
            containing_file_id.index(),
        )));
    }
    if !source_text.is_char_boundary(span.start as usize)
        || !source_text.is_char_boundary(span.end as usize)
    {
        return Err(invalid_input(format!(
            "{kind} span {}..{} is not on UTF-8 boundaries in file ID {}",
            span.start,
            span.end,
            containing_file_id.index(),
        )));
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceMetadata;

    fn source_snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, physical, _, _)| (FileId::new(*id), (*physical).to_owned()))
            .collect();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect();
        let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
        SourceSnapshot::new(
            metadata,
            entries
                .iter()
                .map(|(id, _, _, source)| (FileId::new(*id), Arc::new((*source).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    /// The canonical definition snapshot for a fixture, taken from the merge
    /// query production uses rather than a second assembly.
    fn build(entries: &[(u32, &str, &str, &str)], root: u32) -> DefinitionSnapshot {
        let source = source_snapshot(entries, root);
        crate::test_support::test_merged_program(&source)
            .unwrap()
            .definitions()
            .clone()
    }

    fn durable_projection(
        snapshot: &DefinitionSnapshot,
    ) -> (
        ModuleId,
        Vec<ModuleId>,
        Vec<(DefinitionNameKey, DefinitionKind)>,
    ) {
        (
            snapshot.root_module().clone(),
            snapshot
                .modules()
                .iter()
                .map(|module| module.key().clone())
                .collect(),
            snapshot
                .definitions()
                .map(|definition| (definition.name_key().clone(), definition.kind()))
                .collect(),
        )
    }

    #[test]
    fn module_ids_are_canonical_and_reject_empty_paths() {
        let id = ModuleId::from_logical_path("src/./nested/../main.rue").unwrap();
        assert_eq!(id.as_str(), "src/main.rue");
        assert!(ModuleId::from_logical_path(".").is_err());
    }

    #[test]
    fn canonical_definitions_are_relocation_file_id_and_input_order_independent() {
        let first = build(
            &[
                (91, "/one/root.rue", "app/root.rue", "fn main() {}"),
                (
                    7,
                    "/one/z.rue",
                    "lib/z.rue",
                    "pub struct Shared { value: i32 }",
                ),
            ],
            91,
        );
        let relocated = build(
            &[
                (
                    800,
                    "/two/z.rue",
                    "lib/z.rue",
                    "pub struct Shared { value: i32 }",
                ),
                (3, "/two/root.rue", "app/root.rue", "fn main() {}"),
            ],
            3,
        );

        assert_eq!(durable_projection(&first), durable_projection(&relocated));
        assert_ne!(
            first.modules()[0].file_id(),
            relocated.modules()[0].file_id()
        );
    }

    #[test]
    fn records_explicit_root_and_all_definition_kinds_in_source_order() {
        let source = r#"
            pub fn public_fn() {}
            struct Record { value: i32 }
            pub enum Choice { A }
            const answer: i32 = 42;
            drop fn Record(self) {}
        "#;
        let snapshot = build(
            &[
                (50, "/z.rue", "z/root.rue", source),
                (2, "/a.rue", "a/empty.rue", ""),
            ],
            50,
        );
        let definitions = snapshot.definitions().collect::<Vec<_>>();

        assert_eq!(snapshot.root_module().as_str(), "z/root.rue");
        assert_eq!(
            snapshot
                .modules()
                .iter()
                .map(|module| module.key().as_str())
                .collect::<Vec<_>>(),
            ["a/empty.rue", "z/root.rue"],
        );
        assert_eq!(
            definitions
                .iter()
                .map(|definition| (definition.kind(), definition.name_key().name()))
                .collect::<Vec<_>>(),
            [
                (DefinitionKind::Function, "public_fn"),
                (DefinitionKind::Struct, "Record"),
                (DefinitionKind::Enum, "Choice"),
                (DefinitionKind::Const, "answer"),
                (DefinitionKind::Destructor, "Record"),
            ],
        );
        assert_eq!(definitions[0].visibility(), Some(Visibility::Public));
        assert_eq!(definitions[1].visibility(), Some(Visibility::Private));
        assert_eq!(definitions[4].visibility(), None);
        assert!(
            definitions.windows(2).all(|pair| {
                pair[0].declaration_span().start < pair[1].declaration_span().start
            })
        );
    }

    #[test]
    fn exact_arc_backed_sources_and_empty_modules_are_retained() {
        let source = source_snapshot(
            &[
                (1, "/root.rue", "root.rue", "fn main() {}"),
                (2, "/empty.rue", "empty.rue", ""),
            ],
            1,
        );
        let original = source.shared_source_text(FileId::new(1)).unwrap();
        let merged = crate::test_support::test_merged_program(&source).unwrap();
        let definitions = merged.definitions();
        let retained = definitions
            .source_snapshot()
            .shared_source_text(FileId::new(1))
            .unwrap();

        assert!(Arc::ptr_eq(&original, &retained));
        assert_eq!(definitions.module_count(), 2);
        assert!(
            definitions
                .module(&ModuleId::from_logical_path("empty.rue").unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn validate_span_rejects_foreign_inverted_out_of_bounds_and_utf8_boundaries() {
        let file = FileId::new(7);
        assert_eq!(
            validate_span(
                "definition",
                Span::with_file(FileId::new(8), 0, 1),
                file,
                "é",
            )
            .unwrap_err()
            .to_string(),
            "invalid compiler input: definition uses file ID 8, but its parsed file uses file ID 7",
        );
        assert_eq!(
            validate_span("definition", Span::with_file(file, 2, 1), file, "é")
                .unwrap_err()
                .to_string(),
            "invalid compiler input: definition has an inverted span 2..1 in file ID 7",
        );
        assert_eq!(
            validate_span("definition", Span::with_file(file, 0, 3), file, "é")
                .unwrap_err()
                .to_string(),
            "invalid compiler input: definition span 0..3 exceeds the 2-byte source text for file ID 7",
        );
        assert_eq!(
            validate_span("definition", Span::with_file(file, 0, 1), file, "é")
                .unwrap_err()
                .to_string(),
            "invalid compiler input: definition span 0..1 is not on UTF-8 boundaries in file ID 7",
        );
    }
}
