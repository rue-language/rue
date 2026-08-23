//! Provenance-safe translation from parsed-module symbols into one request.

use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
use ahash::AHashMap;
#[cfg(test)]
use lasso::Spur;
use lasso::ThreadedRodeo;
#[cfg(test)]
use rue_error::{CompileError, CompileResult, ErrorKind};

#[cfg(test)]
use crate::parsed_modules::ParsedProgram;
#[cfg(test)]
use crate::parsed_modules::{ParsedAstView, ParsedSymbol};

#[cfg(test)]
#[derive(Debug)]
struct SemanticSymbolProvenance;

/// A symbol in exactly one request-local semantic universe.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct SemanticSymbol {
    spur: Spur,
    provenance: Arc<SemanticSymbolProvenance>,
}

/// Structural work performed by semantic-symbol translation.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticTranslationWork {
    /// Canonical program modules admitted when the universe was constructed.
    pub modules_visited: usize,
    pub local_symbol_resolutions: usize,
    pub semantic_intern_attempts: usize,
    pub unique_semantic_strings: usize,
    pub ast_payload_clones: usize,
    pub parser_invocations: usize,
}

/// One request-local destination for module-local parsed symbols.
///
/// Callers should translate while traversing [`ParsedProgram::ast_views`],
/// whose module order is canonical. The universe owns no parse cache or
/// local-to-semantic memo table.
#[derive(Debug)]
pub struct SemanticSymbolUniverse {
    admitted_modules: Arc<[Arc<crate::parsed_modules::ParsedModule>]>,
    interner: Arc<ThreadedRodeo>,
    #[cfg(test)]
    provenance: Arc<SemanticSymbolProvenance>,
    #[cfg(test)]
    local_symbol_resolutions: AtomicUsize,
    #[cfg(test)]
    semantic_intern_attempts: AtomicUsize,
    #[cfg(test)]
    unique_semantic_strings: AtomicUsize,
}

impl SemanticSymbolUniverse {
    /// Start a destination universe for one canonical program traversal.
    #[cfg(test)]
    pub(crate) fn new(program: &ParsedProgram) -> Self {
        let universe = Self::from_modules(program.modules());
        if let Some(symbols) = program.shared_symbol_strings() {
            for symbol in symbols {
                universe.interner.get_or_intern(symbol);
            }
        }
        universe
    }

    pub(crate) fn from_modules(modules: &[Arc<crate::parsed_modules::ParsedModule>]) -> Self {
        let universe = Self {
            admitted_modules: modules.to_vec().into(),
            interner: Arc::new(ThreadedRodeo::new()),
            #[cfg(test)]
            provenance: Arc::new(SemanticSymbolProvenance),
            #[cfg(test)]
            local_symbol_resolutions: AtomicUsize::new(0),
            #[cfg(test)]
            semantic_intern_attempts: AtomicUsize::new(0),
            #[cfg(test)]
            unique_semantic_strings: AtomicUsize::new(0),
        };
        if let Some(first) = modules.first()
            && modules
                .iter()
                .all(|module| module.shares_resolver_with(first))
        {
            for symbol in first.resolver_strings() {
                universe.interner.get_or_intern(symbol);
            }
        }
        universe
    }

    /// Translate a provenanced local symbol through its owning module view.
    ///
    /// This API deliberately accepts no naked local `Spur`.
    #[cfg(test)]
    pub(crate) fn translate(
        &mut self,
        owner: &ParsedAstView,
        symbol: &ParsedSymbol,
    ) -> CompileResult<SemanticSymbol> {
        self.translate_shared(owner, symbol)
    }

    /// Canonical lowering is a single-threaded builder even though its frozen
    /// output remains `Send + Sync`.
    #[cfg(test)]
    fn translate_shared(
        &self,
        owner: &ParsedAstView,
        symbol: &ParsedSymbol,
    ) -> CompileResult<SemanticSymbol> {
        let admitted = self
            .admitted_modules
            .binary_search_by(|module| module.module_id().cmp(owner.module_id()))
            .map_err(|_| invalid_input("module view is absent from this semantic program"))?;
        let foreign = !Arc::ptr_eq(&self.admitted_modules[admitted], owner.module());
        if foreign {
            return Err(invalid_input(
                "module view belongs to a foreign parsed artifact",
            ));
        }
        let text = owner.module().resolve(symbol)?;
        self.local_symbol_resolutions
            .fetch_add(1, Ordering::Relaxed);
        self.semantic_intern_attempts
            .fetch_add(1, Ordering::Relaxed);
        if self.interner.get(text).is_none() {
            self.unique_semantic_strings.fetch_add(1, Ordering::Relaxed);
        }
        Ok(SemanticSymbol {
            spur: self.interner.get_or_intern(text),
            provenance: self.provenance.clone(),
        })
    }

    /// Resolve only symbols issued by this exact semantic universe.
    #[cfg(test)]
    pub(crate) fn resolve<'a>(&'a self, symbol: &SemanticSymbol) -> CompileResult<&'a str> {
        if !Arc::ptr_eq(&self.provenance, &symbol.provenance) {
            return Err(invalid_input(
                "semantic symbol belongs to a foreign semantic universe",
            ));
        }
        self.interner
            .try_resolve(&symbol.spur)
            .ok_or_else(|| invalid_input("semantic symbol is absent from its universe"))
    }

    #[cfg(test)]
    pub fn work(&self) -> SemanticTranslationWork {
        SemanticTranslationWork {
            modules_visited: self.admitted_modules.len(),
            local_symbol_resolutions: self.local_symbol_resolutions.load(Ordering::Relaxed),
            semantic_intern_attempts: self.semantic_intern_attempts.load(Ordering::Relaxed),
            unique_semantic_strings: self.unique_semantic_strings.load(Ordering::Relaxed),
            ast_payload_clones: 0,
            parser_invocations: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn translate_ast_symbol(
        &self,
        owner: &ParsedAstView,
        symbol: Spur,
    ) -> CompileResult<SemanticSymbol> {
        let symbol = owner.module().parsed_symbol(symbol)?;
        self.translate_shared(owner, &symbol)
    }

    pub fn interner(&self) -> &ThreadedRodeo {
        &self.interner
    }

    pub(crate) fn admitted_modules(&self) -> &[Arc<crate::parsed_modules::ParsedModule>] {
        &self.admitted_modules
    }
}

#[cfg(test)]
impl SemanticSymbol {
    pub(crate) fn spur(&self) -> Spur {
        self.spur
    }
}

#[cfg(test)]
fn invalid_input(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {

    use rue_span::FileId;

    use super::*;
    use crate::parsed_modules::parse_source_snapshot_modules;
    use crate::{ModuleId, SourceMetadata, SourceSnapshot};

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

    fn translated_definitions(
        program: &ParsedProgram,
    ) -> (Vec<(String, String)>, SemanticTranslationWork) {
        let mut universe = SemanticSymbolUniverse::new(program);
        let mut values = Vec::new();
        for view in program.ast_views() {
            for candidate in view.module().definitions().candidates() {
                let symbol = universe.translate(&view, candidate.symbol()).unwrap();
                values.push((
                    view.module_id().as_str().to_owned(),
                    universe.resolve(&symbol).unwrap().to_owned(),
                ));
            }
        }
        (values, universe.work())
    }

    #[test]
    fn equal_numeric_local_spurs_translate_to_their_distinct_text() {
        let snapshot = snapshot(
            &[
                (1, "/a.rue", "a.rue", "fn alpha() {}"),
                (2, "/b.rue", "b.rue", "fn beta() {}"),
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

        let (values, work) = translated_definitions(&program);
        assert_eq!(
            values,
            [
                (String::from("a.rue"), String::from("alpha")),
                (String::from("b.rue"), String::from("beta")),
            ]
        );
        assert_eq!(
            work,
            SemanticTranslationWork {
                modules_visited: 2,
                local_symbol_resolutions: 2,
                semantic_intern_attempts: 2,
                unique_semantic_strings: 2,
                ast_payload_clones: 0,
                parser_invocations: 0,
            }
        );
    }

    #[test]
    fn reordered_arc_assemblies_have_identical_views_values_and_work() {
        let snapshot = snapshot(
            &[
                (8, "/z.rue", "z.rue", "fn zed() {}"),
                (3, "/a.rue", "a.rue", "fn alpha() {}"),
            ],
            8,
        );
        let first = parse_source_snapshot_modules(&snapshot).unwrap();
        let first_arcs = first.modules().to_vec();
        let mut reversed = first_arcs.clone();
        reversed.reverse();
        let second = ParsedProgram::new(first.root().clone(), reversed).unwrap();

        assert!(
            first
                .modules()
                .iter()
                .zip(second.modules())
                .all(|(left, right)| Arc::ptr_eq(left, right))
        );
        assert_eq!(
            translated_definitions(&first),
            translated_definitions(&second)
        );
    }

    #[test]
    fn foreign_local_and_semantic_provenance_are_rejected() {
        let snapshot = snapshot(
            &[
                (1, "/a.rue", "a.rue", "fn alpha() {}"),
                (2, "/b.rue", "b.rue", "fn beta() {}"),
            ],
            1,
        );
        let program = parse_source_snapshot_modules(&snapshot).unwrap();
        let views = program.ast_views().collect::<Vec<_>>();
        let foreign = program.modules()[1].definitions().candidates()[0].symbol();
        let mut first_universe = SemanticSymbolUniverse::new(&program);
        assert_eq!(
            first_universe
                .translate(&views[0], foreign)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: parsed symbol belongs to a foreign symbol universe"
        );

        let own = program.modules()[0].definitions().candidates()[0].symbol();
        let semantic = first_universe.translate(&views[0], own).unwrap();
        let second_universe = SemanticSymbolUniverse::new(&program);
        assert_eq!(
            second_universe.resolve(&semantic).unwrap_err().to_string(),
            "invalid compiler input: semantic symbol belongs to a foreign semantic universe"
        );
    }

    #[test]
    fn universe_admission_uses_uniform_exact_module_owner_identity() {
        let make_snapshot = || snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
        let admitted = parse_source_snapshot_modules(&make_snapshot()).unwrap();
        let independently_parsed = parse_source_snapshot_modules(&make_snapshot()).unwrap();
        assert_eq!(
            admitted.modules()[0].revision(),
            independently_parsed.modules()[0].revision()
        );
        assert!(!Arc::ptr_eq(
            &admitted.modules()[0],
            &independently_parsed.modules()[0]
        ));

        let foreign_view = independently_parsed.ast_views().next().unwrap();
        let foreign_symbol =
            independently_parsed.modules()[0].definitions().candidates()[0].symbol();
        let mut universe = SemanticSymbolUniverse::new(&admitted);
        assert_eq!(
            universe
                .translate(&foreign_view, foreign_symbol)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: module view belongs to a foreign parsed artifact"
        );

        let edited = parse_source_snapshot_modules(&snapshot(
            &[(1, "/main.rue", "main.rue", "fn changed() {}")],
            1,
        ))
        .unwrap();
        let edited_view = edited.ast_views().next().unwrap();
        let edited_symbol = edited.modules()[0].definitions().candidates()[0].symbol();
        assert_eq!(
            universe
                .translate(&edited_view, edited_symbol)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: module view belongs to a foreign parsed artifact"
        );

        let reused =
            ParsedProgram::new(admitted.root().clone(), vec![admitted.modules()[0].clone()])
                .unwrap();
        let reused_view = reused.ast_views().next().unwrap();
        let reused_symbol = reused.modules()[0].definitions().candidates()[0].symbol();
        let mut reused_universe = SemanticSymbolUniverse::new(&reused);
        let translated = reused_universe
            .translate(&reused_view, reused_symbol)
            .unwrap();
        assert_eq!(reused_universe.resolve(&translated).unwrap(), "main");
    }

    #[test]
    fn same_bytes_at_distinct_module_ids_remain_distinct_zero_clone_views() {
        let text = "fn same() {}";
        let snapshot = snapshot(
            &[
                (4, "/left.rue", "left.rue", text),
                (9, "/right.rue", "right.rue", text),
            ],
            4,
        );
        let program = parse_source_snapshot_modules(&snapshot).unwrap();
        assert_eq!(
            program.modules()[0].source_id(),
            program.modules()[1].source_id()
        );
        let views = program.ast_views().collect::<Vec<_>>();
        assert_eq!(views[0].module_id().as_str(), "left.rue");
        assert_eq!(views[1].module_id().as_str(), "right.rue");
        assert_ne!(views[0].module_id(), views[1].module_id());
        assert!(Arc::ptr_eq(views[0].module(), &program.modules()[0]));
        assert!(Arc::ptr_eq(views[1].module(), &program.modules()[1]));

        let (_, work) = translated_definitions(&program);
        assert_eq!(work.ast_payload_clones, 0);
        assert_eq!(work.parser_invocations, 0);
        assert_eq!(work.modules_visited, 2);
    }

    #[test]
    fn item_views_retain_their_module_without_copying_ast_payloads() {
        let snapshot = snapshot(
            &[(1, "/main.rue", "main.rue", "fn first() {} fn second() {}")],
            1,
        );
        let program = parse_source_snapshot_modules(&snapshot).unwrap();
        let view = program.ast_views().next().unwrap();
        let items = view.items().collect::<Vec<_>>();
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| Arc::ptr_eq(item.module(), view.module()))
        );
        assert_eq!(items[0].module_id(), view.module_id());
    }

    #[test]
    fn missing_stable_module_cannot_construct_a_public_view() {
        let snapshot = snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
        let program = parse_source_snapshot_modules(&snapshot).unwrap();
        assert_eq!(program.ast_views().len(), 1);
        assert_eq!(
            ModuleId::from_logical_path("main.rue").unwrap(),
            program.ast_views().next().unwrap().module_id().clone()
        );
    }
}
