//! Stable, resolution-independent import sites.
//!
//! Source discovery and diagnostic scheduling intentionally remain outside
//! this module. It extracts valid `@import("...")` sites from an already-lowered
//! program and resolves them only against an explicit, immutable source
//! snapshot.

use std::path::Path;
use std::sync::Arc;

use lasso::ThreadedRodeo;
use rue_air::{DirResolution, ModulePath};
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_rir::{InstData, Rir};

use crate::{ModuleId, SourceMetadata};

/// One valid import call, identified independently of request-local file IDs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportDirective {
    importer: ModuleId,
    source_offset: u32,
    specifier: Arc<str>,
}

impl ImportDirective {
    /// Canonical logical identity of the module containing this call.
    pub fn importer(&self) -> &ModuleId {
        &self.importer
    }

    /// Byte offset of the `@import` call in its source module.
    pub fn source_offset(&self) -> u32 {
        self.source_offset
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
pub struct ImportGraph {
    root: ModuleId,
    directives: ImportDirectives,
    resolutions: Arc<[ImportResolution]>,
    edges: Arc<[ImportEdge]>,
}

impl ImportGraph {
    pub fn root(&self) -> &ModuleId {
        &self.root
    }

    pub fn directives(&self) -> &ImportDirectives {
        &self.directives
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
pub fn extract_import_directives(
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
pub fn resolve_import_graph(
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rue_error::ErrorKind;
    use rue_span::FileId;

    use super::*;
    use crate::{CompilationUnit, CompileOptions, SourceFile};

    fn lower<'a>(
        sources: Vec<SourceFile<'a>>,
        root: FileId,
        logical_paths: HashMap<FileId, String>,
    ) -> CompilationUnit<'a> {
        let metadata = SourceMetadata::from_sources(&sources, root, logical_paths).unwrap();
        let mut unit =
            CompilationUnit::with_source_metadata(sources, metadata, CompileOptions::default())
                .unwrap();
        unit.parse().unwrap();
        assert!(unit.import_directives().is_none());
        unit.lower().unwrap();
        unit
    }

    fn specifiers<'a>(unit: &'a CompilationUnit<'_>) -> Vec<&'a str> {
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
            vec![SourceFile::new("main.rue", source, id)],
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
        let mut unit = lower(
            vec![SourceFile::new("main.rue", source, id)],
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
            let mut unit = lower(
                vec![SourceFile::new("main.rue", &source, id)],
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
            vec![SourceFile::new(
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
        let root = SourceFile::new(
            root_physical,
            "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
            root_id,
        );
        let helper = SourceFile::new(
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
                vec![SourceFile::new("main.rue", source, id)],
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

    #[test]
    fn a_new_parse_invalidates_and_relowering_restores_directives() {
        let id = FileId::new(1);
        let mut unit = lower(
            vec![SourceFile::new(
                "main.rue",
                "fn main() -> i32 { let m = @import(\"a\"); 0 }",
                id,
            )],
            id,
            HashMap::new(),
        );
        let first = unit.import_directives().unwrap().clone();
        unit.parse().unwrap();
        assert!(unit.import_directives().is_none());
        unit.lower().unwrap();
        assert_eq!(unit.import_directives(), Some(&first));
    }

    fn graph_unit<'a>(
        entries: &[(u32, &'a str, &'a str, &'a str)],
        root: u32,
    ) -> CompilationUnit<'a> {
        let sources = entries
            .iter()
            .map(|(id, physical, _, source)| SourceFile::new(physical, source, FileId::new(*id)))
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
        let root = SourceFile::new(
            &root_path,
            "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
            FileId::new(root_id),
        );
        let helper = SourceFile::new(&helper_path, "fn helper() {}", FileId::new(helper_id));
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
        let mut direct = graph_unit(&entries, 1);
        let mut queried = graph_unit(&entries, 1);
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
        let mut direct = graph_unit(&ambiguous_entries, 1);
        let mut queried = graph_unit(&ambiguous_entries, 1);
        assert!(matches!(
            queried.resolve_import_graph(None).unwrap().resolutions()[0],
            ImportResolution::Ambiguous { .. }
        ));
        assert_eq!(
            fingerprint(direct.analyze().unwrap_err()),
            fingerprint(queried.analyze().unwrap_err())
        );
    }
}
