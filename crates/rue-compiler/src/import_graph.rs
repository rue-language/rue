//! Stable, resolution-independent import sites.
//!
//! Import discovery and semantic resolution intentionally remain outside this
//! module. These values describe only syntactically valid `@import("...")`
//! intrinsics present in an already-lowered program.

use std::sync::Arc;

use lasso::ThreadedRodeo;
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
}
