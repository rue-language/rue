//! Provenance-retaining canonical syntax assembly.

use std::sync::Arc;

use ahash::AHashMap;
use rue_error::{CompileError, CompileErrors, ErrorKind};
use rue_span::Span;

#[cfg(test)]
use crate::parsed_modules::ParsedAstView;
use crate::parsed_modules::{ParsedModule, ParsedProgram};
use crate::revisioned_query_database::{ModuleIndexEntry, ProjectedModuleIndex};
use crate::{DefinitionKind, DefinitionSnapshot, ImportDirectives, ModuleId, SourceRevision};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CanonicalMergeWork {
    pub modules_visited: usize,
    pub items_visited: usize,
    pub candidates_visited: usize,
    pub parser_invocations: usize,
    /// Deep AST payload clones (Arc handle retention is not a payload clone).
    pub ast_payload_clones: usize,
    /// Deep source-buffer clones (Arc handle retention is not a payload clone).
    pub source_text_clones: usize,
    pub source_bytes_rehashed: usize,
    pub definition_shards_indexed: usize,
    pub definition_shards_reused: usize,
    pub definition_shards_rebuilt: usize,
}

#[derive(Debug, Clone)]
pub struct CanonicalMergedAst {
    source_revision: SourceRevision,
    modules: Arc<[Arc<ParsedModule>]>,
    imports: ImportDirectives,
}

impl CanonicalMergedAst {
    pub(crate) fn from_parsed_program(program: &ParsedProgram) -> Self {
        Self {
            source_revision: program.source_revision().clone(),
            modules: program.modules().to_vec().into(),
            imports: program.import_directives().clone(),
        }
    }

    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }
    pub fn root(&self) -> &ModuleId {
        self.source_revision.root()
    }
    pub fn modules(&self) -> &[Arc<ParsedModule>] {
        &self.modules
    }
    pub fn import_directives(&self) -> &ImportDirectives {
        &self.imports
    }
    #[cfg(test)]
    pub fn validate_view(&self, view: &ParsedAstView) -> Result<(), CompileError> {
        let index = self
            .modules
            .binary_search_by(|module| module.module_id().cmp(view.module_id()))
            .map_err(|_| invalid_input("module view is absent from canonical merged syntax"))?;
        if !Arc::ptr_eq(&self.modules[index], view.module()) {
            return Err(invalid_input(
                "module view belongs to a foreign parsed artifact",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CanonicalMergedProgram {
    ast: CanonicalMergedAst,
    definitions: DefinitionSnapshot,
    work: CanonicalMergeWork,
}

impl CanonicalMergedProgram {
    pub fn ast(&self) -> &CanonicalMergedAst {
        &self.ast
    }
    pub fn definitions(&self) -> &DefinitionSnapshot {
        &self.definitions
    }
    pub fn work(&self) -> CanonicalMergeWork {
        self.work
    }
}

#[derive(Clone)]
struct CandidateDef {
    span: Span,
    physical_path: Arc<str>,
}

pub(crate) fn merge_parsed_modules_reusing_indexes(
    program: &ParsedProgram,
    indexes: &[ProjectedModuleIndex],
    previous: Option<&DefinitionSnapshot>,
    diagnostic_order: Option<&[ModuleId]>,
) -> Result<CanonicalMergedProgram, CompileErrors> {
    if indexes.len() != program.modules().len()
        || indexes
            .iter()
            .zip(program.modules())
            .any(|(index, module)| index.revision != *module.revision())
    {
        return Err(CompileErrors::from(invalid_input(
            "module indexes do not match the parsed program",
        )));
    }
    let ordered = diagnostic_order.map_or_else(
        || (0..program.modules().len()).collect::<Vec<_>>(),
        |order| {
            order
                .iter()
                .map(|module_id| {
                    program
                        .modules()
                        .binary_search_by(|module| module.module_id().cmp(module_id))
                        .expect("diagnostic order contains every parsed module")
                })
                .collect()
        },
    );
    let errors = canonical_duplicate_errors_from_indexes(program, indexes, &ordered);
    if !errors.is_empty() {
        return Err(CompileErrors::from(errors));
    }
    let (definitions, shards) =
        DefinitionSnapshot::from_module_indexes_reusing(program, indexes, previous)
            .map_err(CompileErrors::from)?;
    let mut work = CanonicalMergeWork {
        modules_visited: program.modules().len(),
        items_visited: program
            .modules()
            .iter()
            .map(|module| module.ast().items.len())
            .sum(),
        candidates_visited: indexes.iter().map(|index| index.definitions.len()).sum(),
        ..CanonicalMergeWork::default()
    };
    work.definition_shards_indexed = shards.shards_indexed;
    work.definition_shards_reused = shards.shards_reused;
    work.definition_shards_rebuilt = shards.shards_rebuilt;
    Ok(assemble_merged_program(program, definitions, work))
}

fn assemble_merged_program(
    program: &ParsedProgram,
    definitions: DefinitionSnapshot,
    work: CanonicalMergeWork,
) -> CanonicalMergedProgram {
    CanonicalMergedProgram {
        ast: CanonicalMergedAst::from_parsed_program(program),
        definitions,
        work,
    }
}

fn canonical_duplicate_errors_from_indexes(
    program: &ParsedProgram,
    indexes: &[ProjectedModuleIndex],
    order: &[usize],
) -> Vec<CompileError> {
    duplicate_errors(order.iter().map(|&index| {
        (
            program.modules()[index].physical_path(),
            indexes[index].definitions.as_ref(),
        )
    }))
}

fn duplicate_errors<'a>(
    modules: impl IntoIterator<Item = (&'a str, &'a [ModuleIndexEntry])>,
) -> Vec<CompileError> {
    let mut errors = Vec::new();
    for (physical_path, candidates) in modules {
        let mut functions = AHashMap::<&str, CandidateDef>::new();
        let mut function_names = AHashMap::<&str, CandidateDef>::new();
        let mut type_names = AHashMap::<&str, CandidateDef>::new();
        let mut structs = AHashMap::<&str, CandidateDef>::new();
        let mut enums = AHashMap::<&str, CandidateDef>::new();
        for candidate in candidates {
            let name = candidate.name.as_ref();
            let definition = || CandidateDef {
                span: candidate.declaration_span,
                physical_path: Arc::from(physical_path),
            };
            match candidate.kind {
                DefinitionKind::Function => {
                    // Duplicate names are diagnosed per file (spec 10.5:1),
                    // including `main`: namespacing makes a `main` in a
                    // non-root module an ordinary function, so only same-file
                    // collisions are rejected here. The program entry point is
                    // the root module's `main` (spec 6.1:38); RUE-920/RUE-921
                    // retired the program-wide `main` uniqueness check that
                    // ADR-0047 tracked as a transitional state.
                    if let Some(first) = functions.get(name) {
                        errors.push(function_conflict(candidate.declaration_span, name, first));
                    } else {
                        functions.insert(name, definition());
                    }
                    if let Some(first) = type_names.get(name) {
                        errors.push(mixed_kind_conflict(candidate.declaration_span, name, first));
                    }
                    function_names.entry(name).or_insert_with(definition);
                }
                DefinitionKind::Struct => {
                    if let Some(first) = function_names.get(name) {
                        errors.push(mixed_kind_conflict(candidate.declaration_span, name, first));
                    } else if let Some(first) = structs.get(name) {
                        errors.push(type_conflict(
                            candidate.declaration_span,
                            format!("struct `{name}`"),
                            format!("first defined in {}", first.physical_path),
                            first.span,
                        ));
                    } else if let Some(first) = enums.get(name) {
                        errors.push(type_conflict(
                            candidate.declaration_span,
                            format!("struct `{name}` (conflicts with enum)"),
                            format!("enum first defined in {}", first.physical_path),
                            first.span,
                        ));
                    } else {
                        type_names.entry(name).or_insert_with(definition);
                        structs.insert(name, definition());
                    }
                }
                DefinitionKind::Enum => {
                    if let Some(first) = function_names.get(name) {
                        errors.push(mixed_kind_conflict(candidate.declaration_span, name, first));
                    } else if let Some(first) = enums.get(name) {
                        errors.push(type_conflict(
                            candidate.declaration_span,
                            format!("enum `{name}`"),
                            format!("first defined in {}", first.physical_path),
                            first.span,
                        ));
                    } else if let Some(first) = structs.get(name) {
                        errors.push(type_conflict(
                            candidate.declaration_span,
                            format!("enum `{name}` (conflicts with struct)"),
                            format!("struct first defined in {}", first.physical_path),
                            first.span,
                        ));
                    } else {
                        type_names.entry(name).or_insert_with(definition);
                        enums.insert(name, definition());
                    }
                }
                // A test's name is a string literal in its own namespace, so
                // it collides with nothing here; duplicate test names are
                // diagnosed per module by the semantic nucleus (ADR-0083 §1).
                DefinitionKind::Destructor | DefinitionKind::Const | DefinitionKind::Test => {}
            }
        }
    }
    errors
}

fn function_conflict(span: Span, name: &str, first: &CandidateDef) -> CompileError {
    CompileError::new(
        ErrorKind::DuplicateFunctionDefinition {
            function_name: name.to_owned(),
        },
        span,
    )
    .with_label(
        format!("first defined in {}", first.physical_path),
        first.span,
    )
}

fn mixed_kind_conflict(span: Span, name: &str, first: &CandidateDef) -> CompileError {
    CompileError::new(
        ErrorKind::DuplicateMixedKindDefinition {
            name: name.to_owned(),
        },
        span,
    )
    .with_label(
        format!("first defined in {}", first.physical_path),
        first.span,
    )
}

fn type_conflict(span: Span, type_name: String, label: String, first_span: Span) -> CompileError {
    CompileError::new(ErrorKind::DuplicateTypeDefinition { type_name }, span)
        .with_label(label, first_span)
}

fn invalid_input(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {

    use rue_span::FileId;

    use super::*;
    use crate::parsed_modules::parse_source_snapshot_modules;
    use crate::{SourceMetadata, SourceSnapshot, SourceView};

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<AHashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<AHashMap<_, _>>();
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

    fn definitions(snapshot: &DefinitionSnapshot) -> Vec<String> {
        snapshot
            .definitions()
            .map(|record| {
                format!(
                    "{}|{:?}|{}|{:?}|{:?}",
                    record.name_key().module(),
                    record.name_key().namespace(),
                    record.name_key().name(),
                    record.kind(),
                    record.declaration_span()
                )
            })
            .collect()
    }

    #[test]
    fn canonical_duplicate_and_cross_kind_diagnostics_are_complete_and_ordered() {
        let source = "fn dup() {} fn dup() {} struct clash {} fn clash() {} \
            enum kind {} struct kind {} struct record {} struct record {} \
            enum choice {} enum choice {}";
        let snapshot = snapshot(&[(1, "main.rue", "main.rue", source)], 1);
        let canonical_errors = crate::test_support::test_merged_program(&snapshot).unwrap_err();
        assert_eq!(canonical_errors.len(), 5);
        let errors = canonical_errors.as_slice();
        assert!(matches!(
            &errors[0].kind,
            ErrorKind::DuplicateFunctionDefinition { function_name } if function_name == "dup"
        ));
        assert!(matches!(
            &errors[1].kind,
            ErrorKind::DuplicateMixedKindDefinition { name } if name == "clash"
        ));
        assert!(matches!(
            &errors[2].kind,
            ErrorKind::DuplicateTypeDefinition { type_name }
                if type_name == "struct `kind` (conflicts with enum)"
        ));
        assert!(matches!(
            &errors[3].kind,
            ErrorKind::DuplicateTypeDefinition { type_name } if type_name == "struct `record`"
        ));
        assert!(matches!(
            &errors[4].kind,
            ErrorKind::DuplicateTypeDefinition { type_name } if type_name == "enum `choice`"
        ));
        assert!(
            errors
                .iter()
                .all(|error| error.diagnostic().labels.len() == 1)
        );
    }

    #[test]
    fn cross_module_main_is_accepted_regardless_of_program_storage_order() {
        // RUE-920: `main` is root-module-scoped, not program-wide unique.
        // A `main` in more than one loaded module is no longer a conflict,
        // and that holds whichever storage order the modules arrive in.
        let snapshot = snapshot(
            &[
                (1, "a.rue", "a.rue", "fn main() {}"),
                (2, "b.rue", "b.rue", "fn main() {}"),
            ],
            1,
        );
        assert!(crate::test_support::test_merged_program(&snapshot).is_ok());
        // Storage order cannot vary the outcome: a reversed module vector
        // reassembles into the same canonical sequence the merge consumes.
        let canonical = parse_source_snapshot_modules(&snapshot).unwrap();
        let mut reversed = canonical.modules().to_vec();
        reversed.reverse();
        let reordered = ParsedProgram::new(canonical.root().clone(), reversed).unwrap();
        assert!(
            canonical
                .modules()
                .iter()
                .zip(reordered.modules())
                .all(|(left, right)| Arc::ptr_eq(left, right))
        );
    }

    #[test]
    fn batch_merge_uses_snapshot_presentation_order_without_reordering_program() {
        // Same-file duplicates remain per-file conflicts (spec 10.5:1). With one
        // in each module, the snapshot's own file order [b, a] fixes the
        // diagnostic sequence (b's conflict before a's) without reordering the
        // stored program, which stays in canonical ModuleId order.
        let snapshot = snapshot(
            &[
                (2, "b.rue", "b.rue", "fn clash() {} fn clash() {}"),
                (1, "a.rue", "a.rue", "fn dup() {} fn dup() {}"),
            ],
            1,
        );
        let mut session = crate::CompilerSession::new();
        session
            .update_for_presentation(&snapshot)
            .into_owner_result()
            .unwrap();
        let errors = session.merge().unwrap_err().into_iter().collect::<Vec<_>>();

        // Presentation order [b, a]: b.rue's conflict (file 2) is reported
        // before a.rue's (file 1).
        assert_eq!(errors[0].span().unwrap().file_id, FileId::new(2));
        assert_eq!(errors[1].span().unwrap().file_id, FileId::new(1));
        let parsed = parse_source_snapshot_modules(&snapshot).unwrap();
        assert_eq!(
            parsed
                .modules()
                .iter()
                .map(|module| module.file_id())
                .collect::<Vec<_>>(),
            [FileId::new(1), FileId::new(2)]
        );
    }

    #[test]
    fn reordered_same_arcs_have_identical_values_work_and_definitions() {
        let snapshot = snapshot(
            &[
                (9, "z.rue", "z.rue", "fn zed() {}"),
                (2, "a.rue", "a.rue", "fn alpha() {} const value: i32 = 1;"),
            ],
            9,
        );
        let first = parse_source_snapshot_modules(&snapshot).unwrap();
        let mut reversed = first.modules().to_vec();
        reversed.reverse();
        let second = ParsedProgram::new(first.root().clone(), reversed).unwrap();
        // Both assemblies publish one canonical module sequence, so the merge
        // sees a single program however the vector arrived.
        let first_merged = crate::test_support::test_merged_program(&snapshot).unwrap();
        let second_merged = crate::test_support::test_merged_program(&snapshot).unwrap();
        assert!(
            first
                .modules()
                .iter()
                .zip(second.modules())
                .all(|(left, right)| Arc::ptr_eq(left, right))
        );

        assert_eq!(first_merged.work(), second_merged.work());
        assert_eq!(
            first_merged.ast().source_revision(),
            second_merged.ast().source_revision()
        );
        assert_eq!(
            definitions(first_merged.definitions()),
            definitions(second_merged.definitions())
        );
        assert_eq!(
            first_merged
                .ast()
                .modules()
                .iter()
                .map(|module| module.revision())
                .collect::<Vec<_>>(),
            second_merged
                .ast()
                .modules()
                .iter()
                .map(|module| module.revision())
                .collect::<Vec<_>>()
        );
        assert_eq!(first_merged.work().parser_invocations, 0);
        assert_eq!(first_merged.work().ast_payload_clones, 0);
        assert_eq!(first_merged.work().source_text_clones, 0);
        assert_eq!(first_merged.work().source_bytes_rehashed, 0);
    }

    #[test]
    fn equal_numeric_local_spurs_use_owned_distinct_names() {
        let snapshot = snapshot(
            &[
                (1, "a.rue", "a.rue", "fn alpha() {}"),
                (2, "b.rue", "b.rue", "fn beta() {}"),
            ],
            1,
        );
        let program = parse_source_snapshot_modules(&snapshot).unwrap();
        let left = &program.modules()[0].definitions().candidates()[0];
        let right = &program.modules()[1].definitions().candidates()[0];
        assert_eq!(
            left.symbol().test_local_ordinal(),
            right.symbol().test_local_ordinal()
        );
        let merged = crate::test_support::test_merged_program(&snapshot).unwrap();
        assert_eq!(
            definitions(merged.definitions())
                .iter()
                .map(|value| value.split('|').nth(2).unwrap())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
    }

    /// Duplicate occurrences are retained where production holds them — each
    /// module's own definition index, built by the parse query — and the
    /// program that contains them is refused by merge rather than reaching a
    /// definition snapshot.
    #[test]
    fn duplicate_occurrences_are_retained_per_module_and_refused_by_merge() {
        let snapshot = snapshot(
            &[(1, "main.rue", "main.rue", "fn same() {} fn same() {}")],
            1,
        );
        let program = parse_source_snapshot_modules(&snapshot).unwrap();
        let candidates = program.modules()[0]
            .definitions()
            .candidates_named(crate::DefinitionNamespace::ModuleItem, "same")
            .collect::<Vec<_>>();
        assert_eq!(candidates.len(), 2);
        assert_ne!(candidates[0].occurrence(), candidates[1].occurrence());
        assert_eq!(
            candidates[0].symbol().test_local_ordinal(),
            candidates[1].symbol().test_local_ordinal()
        );

        assert!(crate::test_support::test_merged_program(&snapshot).is_err());
    }

    #[test]
    fn trusted_snapshot_canonicalizes_equal_bytes_without_rehashing() {
        // Two files whose bytes are equal but whose buffers are distinct
        // allocations, so canonicalization cannot be an allocation accident.
        let text = "fn same() {}";
        let metadata = SourceMetadata::new(
            FileId::new(1),
            [
                (FileId::new(1), String::from("left.rue")),
                (FileId::new(2), String::from("right.rue")),
            ]
            .into_iter()
            .collect(),
            [
                (FileId::new(1), String::from("left.rue")),
                (FileId::new(2), String::from("right.rue")),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap();
        let left_text = Arc::new(text.to_owned());
        let right_text = Arc::new(text.to_owned());
        assert!(!Arc::ptr_eq(&left_text, &right_text));
        let source = SourceSnapshot::new(
            metadata,
            vec![(FileId::new(1), left_text), (FileId::new(2), right_text)],
        )
        .unwrap();
        let merged = crate::test_support::test_merged_program(&source).unwrap();
        let rebuilt = merged.definitions().source_snapshot();
        assert_eq!(rebuilt.source_store().len(), 1);
        assert!(Arc::ptr_eq(
            &rebuilt.shared_source_text(FileId::new(1)).unwrap(),
            &rebuilt.shared_source_text(FileId::new(2)).unwrap()
        ));
        assert_eq!(merged.work().source_bytes_rehashed, 0);
    }

    #[test]
    fn canonical_merged_ast_rejects_independently_reparsed_view() {
        let snapshot = snapshot(&[(1, "main.rue", "main.rue", "fn main() {}")], 1);
        let foreign = parse_source_snapshot_modules(&snapshot).unwrap();
        let merged = crate::test_support::test_merged_program(&snapshot).unwrap();
        let admitted_view =
            crate::parsed_modules::ParsedAstView::from_module(merged.ast().modules()[0].clone());
        let foreign_view = foreign.ast_views().next().unwrap();
        merged.ast().validate_view(&admitted_view).unwrap();
        assert_eq!(
            merged
                .ast()
                .validate_view(&foreign_view)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: module view belongs to a foreign parsed artifact"
        );
    }

    #[test]
    fn test_source_view_constructor_supports_internal_fixtures() {
        let source = SourceView::new("main.rue", "fn main() {}", FileId::new(1));
        assert_eq!(source.path, "main.rue");
    }
}
