//! Canonical, provenance-safe lowering from parsed modules to RIR.

use std::cell::RefCell;

use rue_error::{CompileError, CompileResult};
use rue_rir::{AstGen, Rir};

use crate::{CanonicalMergedProgram, SemanticSymbolUniverse, SourceRevision};

/// Structural work performed by canonical RIR lowering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CanonicalRirWork {
    pub modules_visited: usize,
    pub items_visited: usize,
    pub symbol_fields_translated: usize,
    pub semantic_intern_attempts: usize,
    pub unique_semantic_strings: usize,
    /// All strings retained by the final RIR universe, including synthesized names.
    pub semantic_strings_retained: usize,
    pub parser_invocations: usize,
    pub ast_payload_clones: usize,
    pub source_text_clones: usize,
}

/// RIR paired with the exact source revision and symbol universe that created it.
#[derive(Debug)]
pub struct CanonicalRirOutput {
    source_revision: SourceRevision,
    rir: Rir,
    symbols: SemanticSymbolUniverse,
    work: CanonicalRirWork,
}

impl CanonicalRirOutput {
    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }

    pub fn rir(&self) -> &Rir {
        &self.rir
    }

    pub fn semantic_symbols(&self) -> &SemanticSymbolUniverse {
        &self.symbols
    }

    pub fn work(&self) -> CanonicalRirWork {
        self.work
    }
}

/// Lower canonical module views in stable `ModuleId` order into one RIR.
pub fn lower_canonical_rir(merged: &CanonicalMergedProgram) -> CompileResult<CanonicalRirOutput> {
    let ast = merged.ast();
    let symbols = SemanticSymbolUniverse::from_modules(ast.modules());
    let current_view = RefCell::<Option<crate::parsed_modules::ParsedAstView>>::new(None);
    let first_error = RefCell::<Option<CompileError>>::new(None);

    let rir = {
        let mut generator = AstGen::with_symbol_normalizer(symbols.interner(), |local| {
            let view = current_view.borrow();
            let translated = symbols.translate_ast_symbol(
                view.as_ref()
                    .expect("canonical lowering installs a module view before lowering items"),
                local,
            );
            match translated {
                Ok(symbol) => symbol.spur(),
                Err(error) => {
                    let mut slot = first_error.borrow_mut();
                    if slot.is_none() {
                        *slot = Some(error);
                    }
                    symbols
                        .interner()
                        .get_or_intern("__rue_invalid_local_symbol")
                }
            }
        });

        for view in ast.ast_views() {
            ast.validate_view(&view)?;
            *current_view.borrow_mut() = Some(view.clone());
            generator.append_items(&view.module().ast().items);
            if let Some(error) = first_error.borrow_mut().take() {
                return Err(error);
            }
        }
        generator.finish()
    };

    let translation = symbols.work();
    let work = CanonicalRirWork {
        modules_visited: ast.modules().len(),
        items_visited: ast
            .modules()
            .iter()
            .map(|module| module.ast().items.len())
            .sum(),
        symbol_fields_translated: translation.local_symbol_resolutions,
        semantic_intern_attempts: translation.semantic_intern_attempts,
        unique_semantic_strings: translation.unique_semantic_strings,
        semantic_strings_retained: symbols.interner().len(),
        ..CanonicalRirWork::default()
    };
    Ok(CanonicalRirOutput {
        source_revision: ast.source_revision().clone(),
        rir,
        symbols,
        work,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use rue_rir::{AstGen, RirPrinter};
    use rue_span::FileId;

    use super::*;
    use crate::parsed_modules::{ParsedProgram, parse_source_snapshot_modules};
    use crate::{
        SourceMetadata, SourceSnapshot, merge_parsed_modules, merge_symbols,
        parse_all_files_with_source_snapshot,
    };

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn canonical_output_is_send_and_sync() {
        assert_send_sync::<SemanticSymbolUniverse>();
        assert_send_sync::<CanonicalRirOutput>();
    }

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

    fn print(output: &CanonicalRirOutput) -> String {
        RirPrinter::new(output.rir(), output.symbols.interner()).to_string()
    }

    #[test]
    fn equal_local_spurs_lower_to_distinct_semantic_names() {
        let source = snapshot(
            &[
                (1, "/a.rue", "a.rue", "fn alpha() {}"),
                (2, "/b.rue", "b.rue", "fn beta() {}"),
            ],
            1,
        );
        let parsed = parse_source_snapshot_modules(&source).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let output = lower_canonical_rir(&merged).unwrap();
        let rendered = print(&output);

        assert!(rendered.contains("alpha"));
        assert!(rendered.contains("beta"));
        assert_eq!(output.source_revision(), merged.ast().source_revision());
        assert_eq!(output.work().modules_visited, 2);
        assert_eq!(output.work().items_visited, 2);
        assert_eq!(output.work().parser_invocations, 0);
        assert_eq!(output.work().ast_payload_clones, 0);
        assert_eq!(output.work().source_text_clones, 0);
    }

    #[test]
    fn reordered_arc_assembly_has_identical_rir_and_work() {
        let source = snapshot(
            &[
                (8, "/z.rue", "z.rue", "fn zed() {}"),
                (3, "/a.rue", "a.rue", "fn alpha() {}"),
            ],
            8,
        );
        let first = parse_source_snapshot_modules(&source).unwrap();
        let mut modules = first.modules().to_vec();
        modules.reverse();
        let second = ParsedProgram::new(first.root().clone(), modules).unwrap();
        let first = lower_canonical_rir(&merge_parsed_modules(&first).unwrap()).unwrap();
        let second = lower_canonical_rir(&merge_parsed_modules(&second).unwrap()).unwrap();

        assert_eq!(print(&first), print(&second));
        assert_eq!(first.work(), second.work());
    }

    #[test]
    fn canonical_rir_matches_the_legacy_shared_interner_lowering() {
        let source = snapshot(
            &[
                (
                    1,
                    "/a.rue",
                    "a.rue",
                    "struct Point { x: i32 } fn make(value: i32) -> Point { Point { x: value } }",
                ),
                (2, "/b.rue", "b.rue", "fn answer() -> i32 { 42 }"),
            ],
            1,
        );
        let parsed = parse_source_snapshot_modules(&source).unwrap();
        let canonical = lower_canonical_rir(&merge_parsed_modules(&parsed).unwrap()).unwrap();

        let legacy = merge_symbols(parse_all_files_with_source_snapshot(&source).unwrap()).unwrap();
        let legacy_rir = AstGen::generate_items(&legacy.interner, legacy.ast.items());
        let legacy = RirPrinter::new(&legacy_rir, &legacy.interner).to_string();

        assert_eq!(print(&canonical), legacy);
    }

    #[test]
    fn adversarial_symbol_surfaces_match_legacy_lowering() {
        let source = snapshot(
            &[
                (
                    1,
                    "/seed.rue",
                    "a-seed.rue",
                    "fn seed() { let displaced = 0; }",
                ),
                (
                    2,
                    "/symbols.rue",
                    "b-symbols.rue",
                    r#"
                        struct Resource {
                            value: i32,
                            fn set(self, next: i32) { self.value = next; }
                            fn make() -> Resource { Resource { value: 0 } }
                        }
                        enum Choice { None, Some(i32) }
                        const imported = @import("other.rue");
                        const LENGTH: u64 = 2;
                        drop fn Resource(self) { () }

                        @allow(unused_function)
                        fn exercise(values: [i32; LENGTH], text: String) -> i32 {
                            let mut resource = Resource.make();
                            resource.set(1);
                            resource.value = 2;
                            let field = resource.value;
                            let choice = Choice.Some(field);
                            let payload = match choice {
                                Choice.Some(inner) => inner,
                                Choice.None => 0,
                            };
                            let _ = "symbolic text";
                            for element in values { resource.value = element; }
                            for byte in text { resource.value = byte; }
                            @dbg(payload);
                            @sizeOf([i32; LENGTH]);
                            payload
                        }

                        fn TypeFactory(comptime T: type) -> type {
                            struct {
                                member: T,
                                fn get(self) -> T { self.member }
                            }
                        }
                    "#,
                ),
                (
                    3,
                    "/other.rue",
                    "c-other.rue",
                    "fn imported_name() -> i32 { 1 }",
                ),
            ],
            2,
        );
        let parsed = parse_source_snapshot_modules(&source).unwrap();
        let canonical = lower_canonical_rir(&merge_parsed_modules(&parsed).unwrap()).unwrap();
        let rendered = print(&canonical);

        let legacy = merge_symbols(parse_all_files_with_source_snapshot(&source).unwrap()).unwrap();
        let legacy_rir = AstGen::generate_items(&legacy.interner, legacy.ast.items());
        let legacy = RirPrinter::new(&legacy_rir, &legacy.interner).to_string();

        assert_eq!(rendered, legacy);
        assert!(canonical.work().symbol_fields_translated > 40);
        assert!(
            canonical
                .semantic_symbols()
                .interner()
                .get("unused_function")
                .is_some(),
            "directive arguments must resolve in the destination universe"
        );
        for expected in [
            "Resource",
            "Some",
            "inner",
            "LENGTH",
            "symbolic text",
            "element",
            "member",
        ] {
            assert!(
                rendered.contains(expected),
                "missing normalized `{expected}`"
            );
        }
    }
}
