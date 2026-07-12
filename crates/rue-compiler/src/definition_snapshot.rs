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

use crate::{ModuleId, ParsedProgram, SourceMetadata, SourceSnapshot};

/// A durable module identity derived from a canonical logical source path.
///
/// Unlike a [`FileId`], this value remains meaningful across compiler requests
/// within the same source graph. It is insensitive to checkout relocation when
/// callers provide relocation-stable logical paths; metadata that deliberately
/// falls back to physical paths retains those paths' location dependence.
/// Compatibility name for [`ModuleId`].
pub type ModuleKey = ModuleId;

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
    module: ModuleKey,
    namespace: DefinitionNamespace,
    name: Arc<str>,
}

impl DefinitionNameKey {
    /// Construct a name-binding key from durable, owned components.
    pub fn new(module: ModuleKey, namespace: DefinitionNamespace, name: impl AsRef<str>) -> Self {
        Self {
            module,
            namespace,
            name: Arc::from(name.as_ref()),
        }
    }

    /// The durable module containing this definition.
    #[inline]
    pub fn module(&self) -> &ModuleKey {
        &self.module
    }

    /// The name-resolution namespace containing this candidate.
    #[inline]
    pub fn namespace(&self) -> DefinitionNamespace {
        self.namespace
    }

    /// The resolved, owned source name.
    #[inline]
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
pub struct DefinitionId {
    module_index: usize,
    definition_index: usize,
}

impl DefinitionId {
    /// The issuing snapshot's logical-module index.
    #[inline]
    pub fn module_index(self) -> usize {
        self.module_index
    }

    /// The record's source-ordered index within its module.
    #[inline]
    pub fn definition_index(self) -> usize {
        self.definition_index
    }
}

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

    /// The file ID assigned to this module in the current request.
    #[inline]
    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    /// The current request's span for the definition name.
    #[inline]
    pub fn name_span(&self) -> Span {
        self.name_span
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
    key: ModuleKey,
    file_id: FileId,
    definitions: Vec<DefinitionRecord>,
}

impl ModuleDefinition {
    /// The durable logical identity of this module.
    #[inline]
    pub fn key(&self) -> &ModuleKey {
        &self.key
    }

    /// The file ID assigned to this module in the current request.
    #[inline]
    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    /// Top-level definitions in deterministic source-position order.
    #[inline]
    pub fn definitions(&self) -> &[DefinitionRecord] {
        &self.definitions
    }

    /// The number of top-level definitions in this module.
    #[inline]
    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    /// Whether this module contains no top-level definitions.
    #[inline]
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
    root_module: ModuleKey,
    modules: Vec<ModuleDefinition>,
    definition_count: usize,
    definitions_by_name: HashMap<DefinitionNameKey, Vec<DefinitionId>>,
}

impl DefinitionSnapshot {
    /// Build durable definition candidates from self-contained parsed modules.
    ///
    /// Names are already owned values in each module's definition index; this
    /// path performs no interner lookup, AST traversal, or source-byte hashing.
    pub fn from_parsed_modules(
        program: &crate::parsed_modules::ParsedProgram,
    ) -> CompileResult<Self> {
        let _span = info_span!(
            "definition_snapshot_modules",
            module_count = program.modules().len()
        )
        .entered();
        let source_snapshot = SourceSnapshot::from_parsed_modules(program)?;
        let mut modules = Vec::with_capacity(program.modules().len());
        let definition_count = program
            .modules()
            .iter()
            .map(|module| module.definitions().candidates().len())
            .sum();
        let mut definitions_by_name =
            HashMap::<DefinitionNameKey, Vec<DefinitionId>>::with_capacity(definition_count);

        for (module_index, module) in program.modules().iter().enumerate() {
            let mut definitions = Vec::with_capacity(module.definitions().candidates().len());
            for (definition_index, candidate) in
                module.definitions().candidates().iter().enumerate()
            {
                debug_assert_eq!(candidate.occurrence().index(), definition_index);
                let id = DefinitionId {
                    module_index,
                    definition_index,
                };
                let name_key = DefinitionNameKey::new(
                    module.module_id().clone(),
                    candidate.namespace(),
                    candidate.name(),
                );
                definitions_by_name
                    .entry(name_key.clone())
                    .or_default()
                    .push(id);
                definitions.push(DefinitionRecord {
                    id,
                    name_key,
                    kind: candidate.kind(),
                    visibility: candidate.visibility(),
                    file_id: module.file_id(),
                    name_span: candidate.name_span(),
                    declaration_span: candidate.declaration_span(),
                });
            }
            modules.push(ModuleDefinition {
                key: module.module_id().clone(),
                file_id: module.file_id(),
                definitions,
            });
        }

        Ok(Self {
            source_snapshot,
            root_module: program.root().clone(),
            modules,
            definition_count,
            definitions_by_name,
        })
    }

    /// Resolve a parsed program into durable name candidates and source records.
    ///
    /// The parsed-file ID multiset and physical paths must exactly match
    /// `source_snapshot`. Every recorded declaration and name span must belong
    /// to its containing parsed file, fit within that exact source text, and
    /// land on UTF-8 boundaries.
    ///
    /// Until [`ParsedProgram`] embeds source and symbol-universe provenance,
    /// callers must pass the exact [`SourceSnapshot`] from which `program` was
    /// parsed and keep the AST paired with that parse invocation's interner.
    /// Fallible interner lookup rejects numerically unresolvable handles rather
    /// than panicking. A foreign handle whose numeric index also exists in this
    /// interner remains indistinguishable until symbol-universe provenance is
    /// added.
    pub fn from_parsed_program(
        program: &ParsedProgram,
        source_snapshot: &SourceSnapshot,
    ) -> CompileResult<Self> {
        let _span = info_span!("definition_snapshot", file_count = program.files.len()).entered();
        let source_metadata = source_snapshot.metadata();
        validate_file_membership(program, source_metadata)?;

        let mut modules = Vec::with_capacity(program.files.len());
        let mut definition_count = 0;
        let definition_capacity = program.files.iter().map(|file| file.ast.items.len()).sum();
        let mut definitions_by_name =
            HashMap::<DefinitionNameKey, Vec<DefinitionId>>::with_capacity(definition_capacity);

        let mut parsed_files: Vec<_> = program.files.iter().collect();
        parsed_files.sort_by(|left, right| {
            source_metadata
                .logical_path(left.file_id)
                .expect("parsed-file membership validated above")
                .cmp(
                    source_metadata
                        .logical_path(right.file_id)
                        .expect("parsed-file membership validated above"),
                )
        });

        for (module_index, parsed_file) in parsed_files.into_iter().enumerate() {
            let module_key = ModuleKey::from_validated_canonical(
                source_metadata
                    .logical_path(parsed_file.file_id)
                    .expect("parsed-file membership validated above"),
            );
            let source_text = source_snapshot
                .source_text(parsed_file.file_id)
                .expect("parsed-file membership validated against source snapshot");
            let mut definitions =
                Vec::<PendingDefinition>::with_capacity(parsed_file.ast.items.len());

            for item in &parsed_file.ast.items {
                let parts = definition_parts(item).ok_or_else(|| {
                    let Item::Error(span) = item else {
                        unreachable!("all non-error items define top-level names");
                    };
                    invalid_input(format!(
                        "parsed program contains a recovered error item at {}..{} in file ID {}",
                        span.start,
                        span.end,
                        span.file_id.index(),
                    ))
                })?;

                validate_span(
                    "definition declaration",
                    parts.declaration_span,
                    parsed_file.file_id,
                    source_text,
                )?;
                validate_span(
                    "definition name",
                    parts.name.span,
                    parsed_file.file_id,
                    source_text,
                )?;
                if parts.name.span.start < parts.declaration_span.start
                    || parts.name.span.end > parts.declaration_span.end
                {
                    return Err(invalid_input(format!(
                        "definition name span {}..{} is outside declaration span {}..{} in file ID {}",
                        parts.name.span.start,
                        parts.name.span.end,
                        parts.declaration_span.start,
                        parts.declaration_span.end,
                        parsed_file.file_id.index(),
                    )));
                }

                let name = program
                    .interner
                    .try_resolve(&parts.name.name)
                    .ok_or_else(|| {
                        invalid_input(format!(
                            "definition name at {}..{} in file ID {} is not present in the parsed program interner",
                            parts.name.span.start,
                            parts.name.span.end,
                            parsed_file.file_id.index(),
                        ))
                    })?;

                definitions.push(PendingDefinition {
                    name_key: DefinitionNameKey::new(module_key.clone(), parts.namespace, name),
                    kind: parts.kind,
                    visibility: parts.visibility,
                    file_id: parsed_file.file_id,
                    name_span: parts.name.span,
                    declaration_span: parts.declaration_span,
                });
            }

            definitions.sort_by(|left, right| {
                (
                    left.declaration_span.start,
                    left.declaration_span.end,
                    left.kind,
                    left.name_key.name(),
                )
                    .cmp(&(
                        right.declaration_span.start,
                        right.declaration_span.end,
                        right.kind,
                        right.name_key.name(),
                    ))
            });

            let definitions: Vec<_> = definitions
                .into_iter()
                .enumerate()
                .map(|(definition_index, definition)| {
                    let id = DefinitionId {
                        module_index,
                        definition_index,
                    };
                    definitions_by_name
                        .entry(definition.name_key.clone())
                        .or_default()
                        .push(id);
                    DefinitionRecord {
                        id,
                        name_key: definition.name_key,
                        kind: definition.kind,
                        visibility: definition.visibility,
                        file_id: definition.file_id,
                        name_span: definition.name_span,
                        declaration_span: definition.declaration_span,
                    }
                })
                .collect();
            definition_count += definitions.len();
            modules.push(ModuleDefinition {
                key: module_key,
                file_id: parsed_file.file_id,
                definitions,
            });
        }

        let root_module = modules
            .iter()
            .find(|module| module.file_id == source_metadata.root_file_id())
            .expect("parsed-file membership includes the source root")
            .key
            .clone();

        Ok(Self {
            source_snapshot: source_snapshot.clone(),
            root_module,
            modules,
            definition_count,
            definitions_by_name,
        })
    }

    /// The exact immutable source revision to which all record locations refer.
    #[inline]
    pub fn source_snapshot(&self) -> &SourceSnapshot {
        &self.source_snapshot
    }

    /// The explicitly designated root module's durable key.
    #[inline]
    pub fn root_module(&self) -> &ModuleKey {
        &self.root_module
    }

    /// Alias for [`Self::root_module`] emphasizing that this is a key.
    #[inline]
    pub fn root_key(&self) -> &ModuleKey {
        self.root_module()
    }

    /// All modules in canonical logical-path order, including empty modules.
    #[inline]
    pub fn modules(&self) -> &[ModuleDefinition] {
        &self.modules
    }

    /// Find a module by durable key.
    pub fn module(&self, key: &ModuleKey) -> Option<&ModuleDefinition> {
        self.modules
            .binary_search_by(|module| module.key.cmp(key))
            .ok()
            .map(|index| &self.modules[index])
    }

    /// Iterate all definitions in module-path then source-position order.
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

    /// The total number of top-level definitions in all modules.
    #[inline]
    pub fn definition_count(&self) -> usize {
        self.definition_count
    }

    /// The number of modules, including empty modules.
    #[inline]
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// Whether the snapshot contains no modules.
    ///
    /// Valid source metadata is nonempty, so successfully built snapshots are
    /// never empty. This method is provided as the conventional companion to
    /// [`Self::module_count`].
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

struct PendingDefinition {
    name_key: DefinitionNameKey,
    kind: DefinitionKind,
    visibility: Option<Visibility>,
    file_id: FileId,
    name_span: Span,
    declaration_span: Span,
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
        Item::Error(_) => return None,
    };
    Some(parts)
}

fn validate_file_membership(
    program: &ParsedProgram,
    source_metadata: &SourceMetadata,
) -> CompileResult<()> {
    let mut counts = HashMap::<FileId, usize>::with_capacity(program.files.len());
    for parsed_file in &program.files {
        *counts.entry(parsed_file.file_id).or_default() += 1;
    }

    let mut duplicate_ids: Vec<_> = counts
        .iter()
        .filter_map(|(&file_id, &count)| (count > 1).then_some(file_id))
        .collect();
    duplicate_ids.sort_by_key(|file_id| file_id.index());
    if !duplicate_ids.is_empty() {
        return Err(invalid_input(format!(
            "parsed program contains duplicate file IDs: {}",
            display_file_ids(&duplicate_ids),
        )));
    }

    let mut unknown_ids: Vec<_> = counts
        .keys()
        .copied()
        .filter(|&file_id| !source_metadata.contains_file(file_id))
        .collect();
    unknown_ids.sort_by_key(|file_id| file_id.index());
    if !unknown_ids.is_empty() {
        return Err(invalid_input(format!(
            "parsed program contains unknown file IDs: {}",
            display_file_ids(&unknown_ids),
        )));
    }

    let missing_ids: Vec<_> = source_metadata
        .file_ids()
        .filter(|file_id| !counts.contains_key(file_id))
        .collect();
    if !missing_ids.is_empty() {
        return Err(invalid_input(format!(
            "parsed program is missing metadata file IDs: {}",
            display_file_ids(&missing_ids),
        )));
    }

    let mut mismatched_paths: Vec<_> = program
        .files
        .iter()
        .filter_map(|parsed_file| {
            let expected_path = source_metadata
                .physical_path(parsed_file.file_id)
                .expect("unknown parsed-file IDs rejected above");
            (parsed_file.path != expected_path).then_some((
                parsed_file.file_id,
                expected_path,
                parsed_file.path.as_str(),
            ))
        })
        .collect();
    mismatched_paths.sort_by_key(|(file_id, _, _)| file_id.index());
    if let Some((file_id, expected_path, actual_path)) = mismatched_paths.first() {
        return Err(invalid_input(format!(
            "physical path for parsed file ID {} is {:?}, but source metadata uses {:?}",
            file_id.index(),
            actual_path,
            expected_path,
        )));
    }

    Ok(())
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

fn display_file_ids(file_ids: &[FileId]) -> String {
    file_ids
        .iter()
        .map(|file_id| file_id.index().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ParsedFile, SourceFile, ThreadedRodeo, parse_all_files_with_source_snapshot};

    #[derive(Clone, Copy)]
    struct Input<'a> {
        physical_path: &'a str,
        logical_path: &'a str,
        source: &'a str,
        file_id: FileId,
    }

    fn input<'a>(
        physical_path: &'a str,
        logical_path: &'a str,
        source: &'a str,
        file_id: u32,
    ) -> Input<'a> {
        Input {
            physical_path,
            logical_path,
            source,
            file_id: FileId::new(file_id),
        }
    }

    fn parse(inputs: &[Input<'_>], root_file_id: FileId) -> (ParsedProgram, SourceSnapshot) {
        let physical_paths = inputs
            .iter()
            .map(|input| (input.file_id, input.physical_path.to_owned()))
            .collect();
        let logical_paths = inputs
            .iter()
            .map(|input| (input.file_id, input.logical_path.to_owned()))
            .collect();
        let metadata = SourceMetadata::new(root_file_id, physical_paths, logical_paths).unwrap();
        let sources: Vec<_> = inputs
            .iter()
            .map(|input| SourceFile::new(input.physical_path, input.source, input.file_id))
            .collect();
        let source_snapshot = SourceSnapshot::from_sources(&sources, metadata).unwrap();
        let program = parse_all_files_with_source_snapshot(&source_snapshot).unwrap();
        (program, source_snapshot)
    }

    fn build(inputs: &[Input<'_>], root_file_id: FileId) -> DefinitionSnapshot {
        let (program, source_snapshot) = parse(inputs, root_file_id);
        DefinitionSnapshot::from_parsed_program(&program, &source_snapshot).unwrap()
    }

    fn durable_projection(
        snapshot: &DefinitionSnapshot,
    ) -> (
        ModuleKey,
        Vec<ModuleKey>,
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

    fn invalid_message(error: CompileError) -> String {
        match error.kind {
            ErrorKind::InvalidCompilerInput(message) => message,
            other => panic!("expected InvalidCompilerInput, got {other:?}"),
        }
    }

    #[test]
    fn module_keys_are_canonical_and_reject_empty_paths() {
        let key = ModuleKey::from_logical_path("src/./nested/../main.rue").unwrap();
        assert_eq!(key.as_str(), "src/main.rue");
        assert!(ModuleKey::from_logical_path(".").is_err());
    }

    #[test]
    fn relocation_does_not_change_durable_names() {
        let first = build(
            &[
                input(
                    "/checkout-a/project/main.rue",
                    "src/main.rue",
                    "fn main() -> i32 { helper() }",
                    9,
                ),
                input(
                    "/checkout-a/project/helper.rue",
                    "src/helper.rue",
                    "pub fn helper() -> i32 { 1 }",
                    4,
                ),
            ],
            FileId::new(9),
        );
        let relocated = build(
            &[
                input(
                    "/different/place/main.rue",
                    "src/main.rue",
                    "fn main() -> i32 { helper() }",
                    9,
                ),
                input(
                    "/different/place/helper.rue",
                    "src/helper.rue",
                    "pub fn helper() -> i32 { 1 }",
                    4,
                ),
            ],
            FileId::new(9),
        );

        assert_eq!(durable_projection(&first), durable_projection(&relocated));
    }

    #[test]
    fn durable_keys_ignore_file_id_assignment_and_parser_input_order() {
        let first_inputs = [
            input("/one/root.rue", "app/root.rue", "fn main() {}", 91),
            input(
                "/one/z.rue",
                "lib/z.rue",
                "pub struct Shared { value: i32 }",
                7,
            ),
        ];
        let reassigned_and_reordered = [
            input(
                "/two/z.rue",
                "lib/z.rue",
                "pub struct Shared { value: i32 }",
                800,
            ),
            input("/two/root.rue", "app/root.rue", "fn main() {}", 3),
        ];
        let first = build(&first_inputs, FileId::new(91));
        let second = build(&reassigned_and_reordered, FileId::new(3));

        assert_eq!(durable_projection(&first), durable_projection(&second));
        assert_ne!(
            first.modules()[0].file_id(),
            second.modules()[0].file_id(),
            "current source locators remain request-local",
        );

        let same_ids_reordered = build(&[first_inputs[1], first_inputs[0]], FileId::new(91));
        assert_eq!(
            durable_projection(&first),
            durable_projection(&same_ids_reordered),
        );
    }

    #[test]
    fn records_explicit_root_while_modules_stay_in_logical_path_order() {
        let snapshot = build(
            &[
                input("/z.rue", "z/root.rue", "fn root() {}", 50),
                input("/a.rue", "a/first.rue", "fn first() {}", 2),
            ],
            FileId::new(50),
        );

        assert_eq!(snapshot.root_module().as_str(), "z/root.rue");
        assert_eq!(
            snapshot
                .modules()
                .iter()
                .map(|module| module.key().as_str())
                .collect::<Vec<_>>(),
            ["a/first.rue", "z/root.rue"],
        );
    }

    #[test]
    fn retains_the_exact_arc_backed_source_snapshot() {
        let inputs = [input("/root.rue", "root.rue", "fn main() {}", 12)];
        let (program, source_snapshot) = parse(&inputs, FileId::new(12));
        let original_text = source_snapshot.shared_source_text(FileId::new(12)).unwrap();
        let definitions =
            DefinitionSnapshot::from_parsed_program(&program, &source_snapshot).unwrap();
        let retained_text = definitions
            .source_snapshot()
            .shared_source_text(FileId::new(12))
            .unwrap();

        assert!(Arc::ptr_eq(&original_text, &retained_text));
        assert_eq!(
            definitions.source_snapshot().metadata().root_file_id(),
            FileId::new(12),
        );
    }

    #[test]
    fn records_all_definition_kinds_visibility_and_source_order() {
        let source = r#"
            pub fn public_fn() {}
            struct Record { value: i32 }
            pub enum Choice { A }
            const answer: i32 = 42;
            drop fn Record(self) {}
        "#;
        let snapshot = build(&[input("/defs.rue", "defs.rue", source, 6)], FileId::new(6));
        let definitions: Vec<_> = snapshot.definitions().collect();

        assert_eq!(snapshot.definition_count(), 5);
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
        assert_eq!(definitions[2].visibility(), Some(Visibility::Public));
        assert_eq!(definitions[4].visibility(), None);
        assert!(definitions[..4].iter().all(|definition| {
            definition.name_key().namespace() == DefinitionNamespace::ModuleItem
        }));
        assert_eq!(
            definitions[4].name_key().namespace(),
            DefinitionNamespace::Destructor,
        );
        assert_ne!(definitions[1].name_key(), definitions[4].name_key());
        assert!(
            definitions
                .windows(2)
                .all(|pair| pair[0].declaration_span().start < pair[1].declaration_span().start),
        );
        for definition in definitions {
            assert_eq!(definition.file_id(), FileId::new(6));
            assert_eq!(definition.name_span().file_id, FileId::new(6));
            assert_eq!(definition.declaration_span().file_id, FileId::new(6));
        }
    }

    #[test]
    fn source_position_order_and_ids_do_not_trust_ast_vector_order() {
        let inputs = [input(
            "/root.rue",
            "root.rue",
            "fn first() {}\nfn second() {}\nfn third() {}",
            1,
        )];
        let (mut program, source_snapshot) = parse(&inputs, FileId::new(1));
        Arc::make_mut(&mut program.files[0].ast).items.reverse();
        let snapshot = DefinitionSnapshot::from_parsed_program(&program, &source_snapshot).unwrap();
        let definitions: Vec<_> = snapshot.definitions().collect();

        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name_key().name())
                .collect::<Vec<_>>(),
            ["first", "second", "third"],
        );
        assert_eq!(
            definitions
                .iter()
                .map(|definition| {
                    (
                        definition.id().module_index(),
                        definition.id().definition_index(),
                    )
                })
                .collect::<Vec<_>>(),
            [(0, 0), (0, 1), (0, 2)],
        );
    }

    #[test]
    fn body_edits_and_source_moves_preserve_keys_but_renames_do_not() {
        let original = build(
            &[input(
                "/main.rue",
                "main.rue",
                "fn compute() -> i32 { 1 }",
                1,
            )],
            FileId::new(1),
        );
        let body_edit = build(
            &[input(
                "/main.rue",
                "main.rue",
                "fn compute() -> i32 { 123456 }",
                1,
            )],
            FileId::new(1),
        );
        let source_move = build(
            &[input(
                "/main.rue",
                "main.rue",
                "\n\nfn compute() -> i32 { 1 }",
                1,
            )],
            FileId::new(1),
        );
        let rename = build(
            &[input(
                "/main.rue",
                "main.rue",
                "fn calculate() -> i32 { 1 }",
                1,
            )],
            FileId::new(1),
        );

        let original_record = original.definitions().next().unwrap();
        let body_record = body_edit.definitions().next().unwrap();
        let moved_record = source_move.definitions().next().unwrap();
        let renamed_record = rename.definitions().next().unwrap();
        assert_eq!(original_record.name_key(), body_record.name_key());
        assert_eq!(original_record.name_key(), moved_record.name_key());
        assert_ne!(
            original_record.declaration_span(),
            body_record.declaration_span()
        );
        assert_ne!(original_record.name_span(), moved_record.name_span());
        assert_ne!(original_record.name_key(), renamed_record.name_key());
    }

    #[test]
    fn moving_a_definition_between_modules_changes_its_key() {
        let before = build(
            &[
                input("/root.rue", "root.rue", "fn main() {}", 1),
                input("/left.rue", "left.rue", "fn helper() {}", 2),
                input("/right.rue", "right.rue", "", 3),
            ],
            FileId::new(1),
        );
        let after = build(
            &[
                input("/root.rue", "root.rue", "fn main() {}", 1),
                input("/left.rue", "left.rue", "", 2),
                input("/right.rue", "right.rue", "fn helper() {}", 3),
            ],
            FileId::new(1),
        );
        let before_helper = before
            .definitions()
            .find(|definition| definition.name_key().name() == "helper")
            .unwrap();
        let after_helper = after
            .definitions()
            .find(|definition| definition.name_key().name() == "helper")
            .unwrap();

        assert_ne!(before_helper.name_key(), after_helper.name_key());
        assert_eq!(before_helper.name_key().module().as_str(), "left.rue");
        assert_eq!(after_helper.name_key().module().as_str(), "right.rue");
    }

    #[test]
    fn same_names_in_sibling_modules_have_distinct_keys() {
        let snapshot = build(
            &[
                input("/root.rue", "root.rue", "", 1),
                input("/a.rue", "siblings/a.rue", "fn shared() {}", 2),
                input("/b.rue", "siblings/b.rue", "fn shared() {}", 3),
            ],
            FileId::new(1),
        );
        let keys: Vec<_> = snapshot
            .definitions()
            .map(|definition| definition.name_key())
            .collect();

        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1]);
        assert_eq!(keys[0].name(), keys[1].name());
        assert_ne!(keys[0].module(), keys[1].module());
    }

    #[test]
    fn empty_modules_are_retained() {
        let snapshot = build(
            &[
                input("/root.rue", "root.rue", "fn main() {}", 1),
                input("/empty.rue", "empty.rue", "", 2),
            ],
            FileId::new(1),
        );
        let empty_key = ModuleKey::from_logical_path("empty.rue").unwrap();
        let empty = snapshot.module(&empty_key).unwrap();

        assert_eq!(snapshot.module_count(), 2);
        assert!(empty.is_empty());
        assert_eq!(empty.file_id(), FileId::new(2));
    }

    #[test]
    fn duplicate_definitions_share_name_keys_but_have_unique_snapshot_ids() {
        let snapshot = build(
            &[input(
                "/root.rue",
                "root.rue",
                "fn repeated() {} fn repeated() {}",
                1,
            )],
            FileId::new(1),
        );
        let definitions: Vec<_> = snapshot.definitions().collect();

        assert_eq!(definitions.len(), 2);
        assert_eq!(definitions[0].name_key(), definitions[1].name_key());
        assert_ne!(definitions[0].id(), definitions[1].id());
        assert_eq!(
            snapshot.definition(definitions[0].id()),
            Some(definitions[0]),
        );
        assert_eq!(
            snapshot
                .definitions_named(definitions[0].name_key())
                .map(DefinitionRecord::id)
                .collect::<Vec<_>>(),
            [definitions[0].id(), definitions[1].id()],
        );
        assert_ne!(
            definitions[0].declaration_span(),
            definitions[1].declaration_span()
        );
    }

    #[test]
    fn cross_kind_module_items_are_candidates_in_one_name_namespace() {
        let snapshot = build(
            &[input(
                "/root.rue",
                "root.rue",
                "fn shared() {} struct shared {} const shared: i32 = 1;",
                1,
            )],
            FileId::new(1),
        );
        let definitions: Vec<_> = snapshot.definitions().collect();

        assert_eq!(definitions.len(), 3);
        assert!(
            definitions
                .windows(2)
                .all(|pair| pair[0].name_key() == pair[1].name_key()),
        );
        assert_eq!(
            snapshot
                .definitions_named(definitions[0].name_key())
                .map(DefinitionRecord::kind)
                .collect::<Vec<_>>(),
            [
                DefinitionKind::Function,
                DefinitionKind::Struct,
                DefinitionKind::Const,
            ],
        );
    }

    #[test]
    fn membership_errors_list_multiple_ids_exactly_and_deterministically() {
        let inputs = [
            input("/two.rue", "two.rue", "fn two() {}", 2),
            input("/four.rue", "four.rue", "fn four() {}", 4),
            input("/eight.rue", "eight.rue", "fn eight() {}", 8),
        ];

        let (mut duplicate, source_snapshot) = parse(&inputs, FileId::new(2));
        duplicate.files.push(ParsedFile {
            path: duplicate.files[2].path.clone(),
            file_id: duplicate.files[2].file_id,
            ast: duplicate.files[2].ast.clone(),
        });
        duplicate.files.push(ParsedFile {
            path: duplicate.files[0].path.clone(),
            file_id: duplicate.files[0].file_id,
            ast: duplicate.files[0].ast.clone(),
        });
        assert_eq!(
            invalid_message(
                DefinitionSnapshot::from_parsed_program(&duplicate, &source_snapshot).unwrap_err(),
            ),
            "parsed program contains duplicate file IDs: 2, 8",
        );

        let (mut unknown, source_snapshot) = parse(&inputs, FileId::new(2));
        unknown.files[0].file_id = FileId::new(9);
        unknown.files[2].file_id = FileId::new(7);
        unknown.files.reverse();
        assert_eq!(
            invalid_message(
                DefinitionSnapshot::from_parsed_program(&unknown, &source_snapshot).unwrap_err(),
            ),
            "parsed program contains unknown file IDs: 7, 9",
        );

        let (mut missing, source_snapshot) = parse(&inputs, FileId::new(2));
        missing.files.retain(|file| file.file_id == FileId::new(8));
        assert_eq!(
            invalid_message(
                DefinitionSnapshot::from_parsed_program(&missing, &source_snapshot).unwrap_err(),
            ),
            "parsed program is missing metadata file IDs: 2, 4",
        );
    }

    #[test]
    fn rejects_stale_physical_paths_and_invalid_record_spans() {
        let inputs = [input(
            "/metadata/location.rue",
            "stable/main.rue",
            "fn main() {}",
            5,
        )];
        let (mut program, metadata) = parse(&inputs, FileId::new(5));
        program.files[0].path = "/new/diagnostic/location.rue".to_owned();
        let message = invalid_message(
            DefinitionSnapshot::from_parsed_program(&program, &metadata).unwrap_err(),
        );
        assert!(message.contains("physical path for parsed file ID 5"));
        program.files[0].path = "/metadata/location.rue".to_owned();

        let Item::Function(function) = &mut Arc::make_mut(&mut program.files[0].ast).items[0]
        else {
            panic!("expected function");
        };
        function.name.span.file_id = FileId::new(8);
        let message = invalid_message(
            DefinitionSnapshot::from_parsed_program(&program, &metadata).unwrap_err(),
        );
        assert!(message.contains("definition name uses file ID 8"));

        let Item::Function(function) = &mut Arc::make_mut(&mut program.files[0].ast).items[0]
        else {
            panic!("expected function");
        };
        function.name.span.file_id = FileId::new(5);
        function.name.span.start = function.name.span.end + 1;
        let message = invalid_message(
            DefinitionSnapshot::from_parsed_program(&program, &metadata).unwrap_err(),
        );
        assert!(message.contains("definition name has an inverted span"));
    }

    #[test]
    fn rejects_cross_file_spans_even_when_both_file_ids_are_known() {
        let inputs = [
            input("/one.rue", "one.rue", "fn one() {}", 1),
            input("/two.rue", "two.rue", "fn two() {}", 2),
        ];
        let (mut program, source_snapshot) = parse(&inputs, FileId::new(1));
        let Item::Function(function) = &mut Arc::make_mut(&mut program.files[0].ast).items[0]
        else {
            panic!("expected function");
        };
        function.span.file_id = FileId::new(2);
        assert_eq!(
            invalid_message(
                DefinitionSnapshot::from_parsed_program(&program, &source_snapshot).unwrap_err(),
            ),
            "definition declaration uses file ID 2, but its parsed file uses file ID 1",
        );

        let (mut program, source_snapshot) = parse(&inputs, FileId::new(1));
        let Item::Function(function) = &mut Arc::make_mut(&mut program.files[0].ast).items[0]
        else {
            panic!("expected function");
        };
        function.name.span.file_id = FileId::new(2);
        assert_eq!(
            invalid_message(
                DefinitionSnapshot::from_parsed_program(&program, &source_snapshot).unwrap_err(),
            ),
            "definition name uses file ID 2, but its parsed file uses file ID 1",
        );
    }

    #[test]
    fn rejects_spans_outside_or_between_boundaries_of_exact_source_text() {
        let inputs = [input(
            "/root.rue",
            "root.rue",
            "fn main() { let value = \"é\"; }",
            1,
        )];
        let (mut program, source_snapshot) = parse(&inputs, FileId::new(1));
        let source_len = source_snapshot.source_text(FileId::new(1)).unwrap().len();
        let Item::Function(function) = &mut Arc::make_mut(&mut program.files[0].ast).items[0]
        else {
            panic!("expected function");
        };
        function.span.end = u32::try_from(source_len + 1).unwrap();
        assert_eq!(
            invalid_message(
                DefinitionSnapshot::from_parsed_program(&program, &source_snapshot).unwrap_err(),
            ),
            format!(
                "definition declaration span 0..{} exceeds the {source_len}-byte source text for file ID 1",
                source_len + 1,
            ),
        );

        let (mut program, source_snapshot) = parse(&inputs, FileId::new(1));
        let interior = source_snapshot
            .source_text(FileId::new(1))
            .unwrap()
            .find('é')
            .unwrap()
            + 1;
        let Item::Function(function) = &mut Arc::make_mut(&mut program.files[0].ast).items[0]
        else {
            panic!("expected function");
        };
        function.name.span.start = u32::try_from(interior).unwrap();
        function.name.span.end = u32::try_from(interior).unwrap();
        assert_eq!(
            invalid_message(
                DefinitionSnapshot::from_parsed_program(&program, &source_snapshot).unwrap_err(),
            ),
            format!(
                "definition name span {interior}..{interior} is not on UTF-8 boundaries in file ID 1"
            ),
        );
    }

    #[test]
    fn rejects_unresolvable_foreign_symbol_handles_without_panicking() {
        let inputs = [input("/root.rue", "root.rue", "fn original() {}", 1)];
        let (mut program, metadata) = parse(&inputs, FileId::new(1));
        let foreign_interner = ThreadedRodeo::new();
        let mut foreign_name = foreign_interner.get_or_intern("foreign-0");
        for index in 1..256 {
            foreign_name = foreign_interner.get_or_intern(format!("foreign-{index}"));
        }
        let Item::Function(function) = &mut Arc::make_mut(&mut program.files[0].ast).items[0]
        else {
            panic!("expected function");
        };
        function.name.name = foreign_name;

        let message = invalid_message(
            DefinitionSnapshot::from_parsed_program(&program, &metadata).unwrap_err(),
        );
        assert!(message.contains("not present in the parsed program interner"));
    }

    #[test]
    fn rejects_recovered_error_items_at_the_success_only_boundary() {
        let inputs = [input("/root.rue", "root.rue", "fn original() {}", 1)];
        let (mut program, metadata) = parse(&inputs, FileId::new(1));
        Arc::make_mut(&mut program.files[0].ast).items[0] =
            Item::Error(Span::with_file(FileId::new(1), 0, 1));

        let message = invalid_message(
            DefinitionSnapshot::from_parsed_program(&program, &metadata).unwrap_err(),
        );
        assert!(message.contains("recovered error item"));
    }
}
