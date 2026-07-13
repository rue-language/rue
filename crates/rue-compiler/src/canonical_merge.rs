//! Provenance-retaining canonical syntax assembly.

use std::collections::HashMap;
use std::sync::Arc;

use rue_error::{CompileError, CompileErrors, ErrorKind};
use rue_span::Span;

use crate::parsed_modules::{ParsedAstView, ParsedItemView, ParsedModule, ParsedProgram};
use crate::{DefinitionKind, DefinitionSnapshot, ModuleId, SourceRevision};

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
}

impl CanonicalMergedAst {
    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }
    pub fn root(&self) -> &ModuleId {
        self.source_revision.root()
    }
    pub fn modules(&self) -> &[Arc<ParsedModule>] {
        &self.modules
    }
    pub fn ast_views(&self) -> impl ExactSizeIterator<Item = ParsedAstView> + '_ {
        self.modules.iter().cloned().map(ParsedAstView::from_module)
    }
    pub fn item_views(&self) -> impl Iterator<Item = ParsedItemView> + '_ {
        self.modules.iter().flat_map(|module| {
            let module = module.clone();
            (0..module.ast().items.len())
                .map(move |index| ParsedItemView::from_module_index(module.clone(), index))
        })
    }
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

pub fn merge_parsed_modules(
    program: &ParsedProgram,
) -> Result<CanonicalMergedProgram, CompileErrors> {
    merge_parsed_modules_in_order(program, None)
}

pub(crate) fn merge_parsed_modules_reusing_definitions(
    program: &ParsedProgram,
    previous: Option<&DefinitionSnapshot>,
) -> Result<CanonicalMergedProgram, CompileErrors> {
    let (mut work, ordered_modules) = merge_inputs(program, None);
    let errors = canonical_duplicate_errors(&ordered_modules);
    if !errors.is_empty() {
        return Err(CompileErrors::from(errors));
    }
    let (definitions, shards) = DefinitionSnapshot::from_parsed_modules_reusing(program, previous)
        .map_err(CompileErrors::from)?;
    work.definition_shards_indexed = shards.shards_indexed;
    work.definition_shards_reused = shards.shards_reused;
    work.definition_shards_rebuilt = shards.shards_rebuilt;
    Ok(assemble_merged_program(program, definitions, work))
}

pub(crate) fn merge_parsed_modules_for_batch(
    program: &ParsedProgram,
    diagnostic_order: &[ModuleId],
) -> Result<CanonicalMergedProgram, CompileErrors> {
    merge_parsed_modules_in_order(program, Some(diagnostic_order))
}

fn merge_parsed_modules_in_order(
    program: &ParsedProgram,
    diagnostic_order: Option<&[ModuleId]>,
) -> Result<CanonicalMergedProgram, CompileErrors> {
    let (work, ordered_modules) = merge_inputs(program, diagnostic_order);
    let errors = canonical_duplicate_errors(&ordered_modules);
    if !errors.is_empty() {
        return Err(CompileErrors::from(errors));
    }
    let (definitions, shards) = DefinitionSnapshot::from_parsed_modules_reusing(program, None)
        .map_err(CompileErrors::from)?;
    let mut work = work;
    work.definition_shards_indexed = shards.shards_indexed;
    work.definition_shards_reused = shards.shards_reused;
    work.definition_shards_rebuilt = shards.shards_rebuilt;
    Ok(assemble_merged_program(program, definitions, work))
}

fn merge_inputs<'a>(
    program: &'a ParsedProgram,
    diagnostic_order: Option<&[ModuleId]>,
) -> (CanonicalMergeWork, Vec<&'a Arc<ParsedModule>>) {
    let work = CanonicalMergeWork {
        modules_visited: program.modules().len(),
        items_visited: program
            .modules()
            .iter()
            .map(|module| module.ast().items.len())
            .sum(),
        candidates_visited: program
            .modules()
            .iter()
            .map(|module| module.definitions().candidates().len())
            .sum(),
        ..CanonicalMergeWork::default()
    };
    let ordered_modules = if let Some(order) = diagnostic_order {
        order
            .iter()
            .map(|module_id| {
                program
                    .modules()
                    .binary_search_by(|module| module.module_id().cmp(module_id))
                    .ok()
                    .map(|index| &program.modules()[index])
                    .expect("batch diagnostic order contains every parsed module")
            })
            .collect::<Vec<_>>()
    } else {
        program.modules().iter().collect()
    };
    (work, ordered_modules)
}

fn assemble_merged_program(
    program: &ParsedProgram,
    definitions: DefinitionSnapshot,
    work: CanonicalMergeWork,
) -> CanonicalMergedProgram {
    CanonicalMergedProgram {
        ast: CanonicalMergedAst {
            source_revision: program.source_revision().clone(),
            modules: program.modules().to_vec().into(),
        },
        definitions,
        work,
    }
}

fn canonical_duplicate_errors(modules: &[&Arc<ParsedModule>]) -> Vec<CompileError> {
    let mut errors = Vec::new();
    let mut program_main: Option<CandidateDef> = None;
    for module in modules {
        let mut functions = HashMap::<&str, CandidateDef>::new();
        let mut function_names = HashMap::<&str, CandidateDef>::new();
        let mut type_names = HashMap::<&str, CandidateDef>::new();
        let mut structs = HashMap::<&str, CandidateDef>::new();
        let mut enums = HashMap::<&str, CandidateDef>::new();
        for candidate in module.definitions().candidates() {
            let name = candidate.name();
            let definition = || CandidateDef {
                span: candidate.declaration_span(),
                physical_path: Arc::from(module.physical_path()),
            };
            match candidate.kind() {
                DefinitionKind::Function => {
                    if let Some(first) = functions.get(name) {
                        errors.push(function_conflict(candidate.declaration_span(), name, first));
                    } else if name == "main"
                        && let Some(first) = &program_main
                    {
                        errors.push(
                            CompileError::new(
                                ErrorKind::DuplicateFunctionDefinition {
                                    function_name: name.to_owned(),
                                },
                                candidate.declaration_span(),
                            )
                            .with_label("first defined here", first.span),
                        );
                    } else {
                        let value = definition();
                        if name == "main" {
                            program_main = Some(value.clone());
                        }
                        functions.insert(name, value);
                    }
                    if let Some(first) = type_names.get(name) {
                        errors.push(function_conflict(candidate.declaration_span(), name, first));
                    }
                    function_names.entry(name).or_insert_with(definition);
                }
                DefinitionKind::Struct => {
                    if let Some(first) = function_names.get(name) {
                        errors.push(function_conflict(candidate.declaration_span(), name, first));
                    } else if let Some(first) = structs.get(name) {
                        errors.push(type_conflict(
                            candidate.declaration_span(),
                            format!("struct `{name}`"),
                            format!("first defined in {}", first.physical_path),
                            first.span,
                        ));
                    } else if let Some(first) = enums.get(name) {
                        errors.push(type_conflict(
                            candidate.declaration_span(),
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
                        errors.push(function_conflict(candidate.declaration_span(), name, first));
                    } else if let Some(first) = enums.get(name) {
                        errors.push(type_conflict(
                            candidate.declaration_span(),
                            format!("enum `{name}`"),
                            format!("first defined in {}", first.physical_path),
                            first.span,
                        ));
                    } else if let Some(first) = structs.get(name) {
                        errors.push(type_conflict(
                            candidate.declaration_span(),
                            format!("enum `{name}` (conflicts with struct)"),
                            format!("struct first defined in {}", first.physical_path),
                            first.span,
                        ));
                    } else {
                        type_names.entry(name).or_insert_with(definition);
                        enums.insert(name, definition());
                    }
                }
                DefinitionKind::Destructor | DefinitionKind::Const => {}
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

fn type_conflict(span: Span, type_name: String, label: String, first_span: Span) -> CompileError {
    CompileError::new(ErrorKind::DuplicateTypeDefinition { type_name }, span)
        .with_label(label, first_span)
}

fn invalid_input(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rue_span::FileId;

    use super::*;
    use crate::parsed_modules::parse_source_snapshot_modules;
    use crate::{
        SourceFile, SourceMetadata, SourceSnapshot, merge_symbols,
        parse_all_files_with_source_snapshot,
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

    fn errors(errors: &CompileErrors) -> Vec<String> {
        errors
            .iter()
            .map(|error| {
                format!(
                    "{}|{:?}|{}|{:?}",
                    error.kind.code(),
                    error.span(),
                    error,
                    error.diagnostic()
                )
            })
            .collect()
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
    fn canonical_duplicate_and_cross_kind_diagnostics_match_legacy() {
        let source =
            "fn dup() {} fn dup() {} struct clash {} fn clash() {} enum kind {} struct kind {}";
        let snapshot = snapshot(&[(1, "main.rue", "main.rue", source)], 1);
        let canonical = parse_source_snapshot_modules(&snapshot).unwrap();
        let canonical_errors = merge_parsed_modules(&canonical).unwrap_err();

        let legacy = parse_all_files_with_source_snapshot(&snapshot).unwrap();
        let legacy_errors = merge_symbols(legacy).unwrap_err();
        assert_eq!(errors(&canonical_errors), errors(&legacy_errors));
    }

    #[test]
    fn canonical_cross_module_main_diagnostics_match_legacy_and_ignore_input_order() {
        let snapshot = snapshot(
            &[
                (1, "a.rue", "a.rue", "fn main() {}"),
                (2, "b.rue", "b.rue", "fn main() {}"),
            ],
            1,
        );
        let canonical = parse_source_snapshot_modules(&snapshot).unwrap();
        let expected = merge_parsed_modules(&canonical).unwrap_err();
        let mut reversed = canonical.modules().to_vec();
        reversed.reverse();
        let reordered = ParsedProgram::new(canonical.root().clone(), reversed).unwrap();
        let reordered = merge_parsed_modules(&reordered).unwrap_err();
        assert_eq!(errors(&expected), errors(&reordered));

        let legacy = parse_all_files_with_source_snapshot(&snapshot).unwrap();
        let legacy = merge_symbols(legacy).unwrap_err();
        assert_eq!(errors(&expected), errors(&legacy));
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
        let first_merged = merge_parsed_modules(&first).unwrap();
        let second_merged = merge_parsed_modules(&second).unwrap();

        assert_eq!(first_merged.work(), second_merged.work());
        assert_eq!(
            first_merged.ast().source_revision(),
            second_merged.ast().source_revision()
        );
        assert_eq!(
            definitions(first_merged.definitions()),
            definitions(second_merged.definitions())
        );
        assert!(
            first_merged
                .ast()
                .modules()
                .iter()
                .zip(second_merged.ast().modules())
                .all(|(left, right)| Arc::ptr_eq(left, right))
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
        let merged = merge_parsed_modules(&program).unwrap();
        assert_eq!(
            definitions(merged.definitions())
                .iter()
                .map(|value| value.split('|').nth(2).unwrap())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
    }

    #[test]
    fn definition_snapshot_retains_duplicate_occurrences_without_resolving_spurs() {
        let snapshot = snapshot(
            &[(1, "main.rue", "main.rue", "fn same() {} fn same() {}")],
            1,
        );
        let program = parse_source_snapshot_modules(&snapshot).unwrap();
        let definitions = DefinitionSnapshot::from_parsed_modules(&program).unwrap();
        let key = crate::DefinitionNameKey::new(
            program.root().clone(),
            crate::DefinitionNamespace::ModuleItem,
            "same",
        );
        let matches = definitions.definitions_named(&key).collect::<Vec<_>>();
        assert_eq!(matches.len(), 2);
        assert_ne!(matches[0].id(), matches[1].id());
    }

    #[test]
    fn trusted_snapshot_canonicalizes_equal_bytes_without_rehashing() {
        let text = "fn same() {}";
        let left_snapshot = snapshot(&[(1, "left.rue", "left.rue", text)], 1);
        let right_snapshot = snapshot(&[(2, "right.rue", "right.rue", text)], 2);
        let left = parse_source_snapshot_modules(&left_snapshot).unwrap();
        let right = parse_source_snapshot_modules(&right_snapshot).unwrap();
        assert!(!Arc::ptr_eq(
            &left.modules()[0].shared_source_text(),
            &right.modules()[0].shared_source_text()
        ));
        let program = ParsedProgram::new(
            left.root().clone(),
            vec![left.modules()[0].clone(), right.modules()[0].clone()],
        )
        .unwrap();
        let merged = merge_parsed_modules(&program).unwrap();
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
        let admitted = parse_source_snapshot_modules(&snapshot).unwrap();
        let foreign = parse_source_snapshot_modules(&snapshot).unwrap();
        let merged = merge_parsed_modules(&admitted).unwrap();
        let admitted_view = admitted.ast_views().next().unwrap();
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
    fn legacy_source_file_constructor_remains_available() {
        let source = SourceFile::new("main.rue", "fn main() {}", FileId::new(1));
        assert_eq!(source.path, "main.rue");
    }
}
