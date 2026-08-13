//! Canonical, provenance-safe lowering from parsed modules to RIR.

use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use rue_error::{CompileError, ErrorKind};
use rue_rir::RirPrinter;
use rue_rir::{
    AstGen, InstRef, Rir, RirEditor, RirPayloadBuildError, RirValidationContext, ValidatedRir,
};
use rue_span::FileId;

use crate::retained_charge::RetainedCharge;
use crate::{CanonicalMergedProgram, SemanticSymbolUniverse, SourceRevision};

/// Classify a RIR construction failure for the user.
///
/// Spec C.1:2 makes exceeding a published implementation limit a diagnosable
/// compile-time failure, not an internal compiler error: a program that is too
/// large for the `u32` instruction array or the `u32`-indexed payload word
/// store (Appendix C.6:1) is rejected with `E1401` naming the limit it hit.
/// Only a genuine producer bug (a malformed builder request) stays an ICE, and
/// a failed reservation for a representable request is `E1402`.
pub(crate) fn rir_build_error_kind(context: &str, error: &RirPayloadBuildError) -> ErrorKind {
    if error.is_resource_limit() {
        ErrorKind::CompilerResourceLimit(error.to_string())
    } else if error.is_resource_exhaustion() {
        ErrorKind::CompilerResourceExhaustion(error.to_string())
    } else {
        ErrorKind::InternalError(format!("{context}: {error}"))
    }
}

/// Structural work performed by canonical RIR lowering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CanonicalRirWork {
    /// Module lowerers that executed for this request (cache hits contribute zero).
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
    /// Deterministic compatibility projection work, accounted separately from
    /// module lowering so terminal reuse remains visible.
    pub modules_projected: usize,
    pub instructions_appended: usize,
    pub payload_words_appended: usize,
}

/// RIR paired with the exact source revision and symbol universe that created it.
#[derive(Debug)]
pub struct CanonicalRirOutput {
    source_revision: SourceRevision,
    rir: ValidatedRir,
    symbols: SemanticSymbolUniverse,
    work: CanonicalRirWork,
    module_ranges: Vec<CanonicalRirModuleRange>,
    sources: Vec<CanonicalRirSource>,
}

/// Independently reusable lowering result for exactly one module source leaf.
#[derive(Debug)]
pub(crate) struct ModuleRirOutput {
    revision: crate::ModuleRevision,
    source_length: u32,
    rir: ValidatedRir,
    symbols: SemanticSymbolUniverse,
    work: CanonicalRirWork,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RemappedBodyRirAttribution {
    pub(crate) span_remap_validation_ns: u64,
    pub(crate) index: rue_air::BodyRirIndexAttribution,
    pub(crate) rir_instructions: u64,
    pub(crate) rir_payload_words: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BodyRirAttributionClock {
    pub(crate) started: Instant,
    pub(crate) rir_lower_finished_ns: u64,
}

impl ModuleRirOutput {
    pub(crate) fn revision(&self) -> &crate::ModuleRevision {
        &self.revision
    }

    pub(crate) fn work(&self) -> CanonicalRirWork {
        self.work
    }

    #[allow(dead_code)]
    pub(crate) fn into_remapped_body_rir_bundle(
        self,
        file_id: FileId,
        source_length: u32,
        remap_span: impl FnMut(rue_span::Span) -> rue_span::Span,
    ) -> Result<rue_air::BodyRirBundle, String> {
        self.into_remapped_body_rir_bundle_with_attribution(
            file_id,
            source_length,
            remap_span,
            None,
        )
        .map(|(bundle, _)| bundle)
    }

    pub(crate) fn into_remapped_body_rir_bundle_with_attribution(
        self,
        file_id: FileId,
        source_length: u32,
        remap_span: impl FnMut(rue_span::Span) -> rue_span::Span,
        attribution_clock: Option<BodyRirAttributionClock>,
    ) -> Result<(rue_air::BodyRirBundle, RemappedBodyRirAttribution), String> {
        let rir_instructions = self.rir.len() as u64;
        let rir_payload_words = self.rir.extra_len() as u64;
        let mut editor = RirEditor::new();
        editor
            .append_remapped_with_spans(&self.rir, std::convert::identity, remap_span)
            .map_err(|error| error.to_string())?;
        let source_lengths = [(file_id, source_length)];
        let validation = RirValidationContext {
            symbol_count: self.symbols.interner().len(),
            source_lengths: &source_lengths,
        };
        let rir = ValidatedRir::finish(editor, &validation).map_err(|error| error.to_string())?;
        let remap_finished_ns = attribution_clock
            .map(|clock| u64::try_from(clock.started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        let (bundle, mut index) = rue_air::BodyRirBundle::new_with_index_attribution(
            rir,
            Arc::try_unwrap(self.symbols.into_interner())
                .expect("module RIR owns its semantic symbol interner"),
            attribution_clock.is_some(),
        );
        let span_remap_validation_ns = attribution_clock
            .zip(remap_finished_ns)
            .map_or(0, |(clock, finished)| {
                finished.saturating_sub(clock.rir_lower_finished_ns)
            });
        if let (Some(clock), Some(remap_finished_ns)) = (attribution_clock, remap_finished_ns) {
            let index_finished_ns =
                u64::try_from(clock.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            index.duration_ns = index_finished_ns.saturating_sub(remap_finished_ns);
        }
        Ok((
            bundle,
            RemappedBodyRirAttribution {
                span_remap_validation_ns,
                index,
                rir_instructions,
                rir_payload_words,
            },
        ))
    }
}

impl RetainedCharge for ModuleRirOutput {
    fn retained_charge(&self) -> u64 {
        let instructions = self
            .rir
            .len()
            .saturating_mul(std::mem::size_of::<rue_rir::Inst>()) as u64;
        let payload = self
            .rir
            .extra_len()
            .saturating_mul(std::mem::size_of::<u32>()) as u64;
        self.revision
            .retained_charge()
            .saturating_add(instructions)
            .saturating_add(payload)
            .saturating_add(self.symbols.retained_charge())
    }
}

#[derive(Debug)]
struct CanonicalRirModuleRange {
    file_id: FileId,
    instructions: Range<u32>,
    extra: Range<u32>,
}

#[derive(Debug)]
struct CanonicalRirSource {
    file_id: FileId,
    revision: crate::ModuleRevision,
    length: u32,
}

/// Ephemeral caller-order indices consumed by the read-only RIR printer.
pub struct CanonicalRirPresentationOrder {
    pub instructions: Vec<InstRef>,
    pub extra: Vec<u32>,
}

impl CanonicalRirOutput {
    #[allow(dead_code)]
    pub(crate) fn structurally_eq(&self, other: &Self) -> bool {
        self.source_revision == other.source_revision
            && RirPrinter::new(&self.rir, self.symbols.interner()).to_string()
                == RirPrinter::new(&other.rir, other.symbols.interner()).to_string()
            && self.module_ranges.len() == other.module_ranges.len()
            && self
                .module_ranges
                .iter()
                .zip(other.module_ranges.iter())
                .all(|(left, right)| {
                    left.file_id == right.file_id
                        && left.instructions == right.instructions
                        && left.extra == right.extra
                })
    }

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

    pub(crate) fn source_identity_and_length(
        &self,
        file_id: FileId,
    ) -> (&crate::ModuleRevision, u32) {
        let source = self
            .sources
            .iter()
            .find(|source| source.file_id == file_id)
            .expect("validated RIR spans name a retained canonical source");
        (&source.revision, source.length)
    }

    /// Return a read-only instruction presentation order for caller-ordered files.
    ///
    /// Canonical RIR remains in stable module identity order; this permutation is
    /// consumed only by presentation printers and never changes semantic refs.
    pub fn presentation_order(
        &self,
        files: impl IntoIterator<Item = FileId>,
    ) -> CanonicalRirPresentationOrder {
        let mut instructions = Vec::with_capacity(self.rir.len());
        let mut extra = Vec::with_capacity(self.rir.extra_len());
        for file in files {
            let range = self
                .module_ranges
                .iter()
                .find(|candidate| candidate.file_id == file)
                .expect("RIR presentation file belongs to the canonical source revision");
            instructions.extend(range.instructions.clone().map(InstRef::from_raw));
            extra.extend(range.extra.clone());
        }
        assert_eq!(instructions.len(), self.rir.len());
        assert_eq!(extra.len(), self.rir.extra_len());
        CanonicalRirPresentationOrder {
            instructions,
            extra,
        }
    }
}

impl CanonicalRirWork {
    pub(crate) fn accumulate(&mut self, other: Self) {
        self.modules_visited += other.modules_visited;
        self.items_visited += other.items_visited;
        self.symbol_fields_translated += other.symbol_fields_translated;
        self.semantic_intern_attempts += other.semantic_intern_attempts;
        self.unique_semantic_strings += other.unique_semantic_strings;
        self.parser_invocations += other.parser_invocations;
        self.ast_payload_clones += other.ast_payload_clones;
        self.source_text_clones += other.source_text_clones;
    }
}

pub(crate) fn lower_module_rir_with_work(
    module: std::sync::Arc<crate::parsed_modules::ParsedModule>,
) -> Result<ModuleRirOutput, (CompileError, CanonicalRirWork)> {
    lower_module_rir_with_work_internal(module, None)
}

pub(crate) fn lower_module_rir_with_work_and_anonymous_anchors(
    module: std::sync::Arc<crate::parsed_modules::ParsedModule>,
    anchors: &[(
        rue_span::Span,
        rue_rir::AnonymousTypeSiteKind,
        rue_rir::RirStructuralAnchor,
    )],
) -> Result<ModuleRirOutput, (CompileError, CanonicalRirWork)> {
    lower_module_rir_with_work_internal(module, Some(anchors))
}

fn lower_module_rir_with_work_internal(
    module: std::sync::Arc<crate::parsed_modules::ParsedModule>,
    authoritative_anchors: Option<
        &[(
            rue_span::Span,
            rue_rir::AnonymousTypeSiteKind,
            rue_rir::RirStructuralAnchor,
        )],
    >,
) -> Result<ModuleRirOutput, (CompileError, CanonicalRirWork)> {
    let symbols = SemanticSymbolUniverse::from_modules(std::slice::from_ref(&module));
    let view = crate::parsed_modules::ParsedAstView::from_module(module.clone());
    let first_error = RefCell::<Option<CompileError>>::new(None);
    let mut work = CanonicalRirWork {
        modules_visited: 1,
        items_visited: module.ast().items.len(),
        ..CanonicalRirWork::default()
    };
    let editor = {
        let mut generator =
            AstGen::with_symbol_normalizer(symbols.interner(), |local| {
                match symbols.translate_ast_symbol(&view, local) {
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
        if let Some(anchors) = authoritative_anchors {
            generator
                .install_authoritative_anonymous_anchors(anchors.iter().cloned())
                .map_err(|error| {
                    (
                        CompileError::new(
                            ErrorKind::InternalError(format!(
                                "authoritative anonymous-anchor transport failed: {error}"
                            )),
                            rue_span::Span::new(0, 0),
                        ),
                        work,
                    )
                })?;
        }
        generator.append_items(&module.ast().items);
        if let Some(error) = first_error.borrow_mut().take() {
            return Err((error, work));
        }
        generator.try_finish_editor().map_err(|error| {
            (
                CompileError::new(
                    rir_build_error_kind("RIR module payload construction failed", &error),
                    rue_span::Span::new(0, 0),
                ),
                work,
            )
        })?
    };
    let source_length = u32::try_from(module.source_text().len()).map_err(|_| {
        (
            CompileError::new(
                ErrorKind::InternalError(
                    "canonical module source length exceeds RIR span capacity".to_string(),
                ),
                rue_span::Span::new(0, 0),
            ),
            work,
        )
    })?;
    let source_lengths = [(module.file_id(), source_length)];
    let validation = RirValidationContext {
        symbol_count: symbols.interner().len(),
        source_lengths: &source_lengths,
    };
    let rir = ValidatedRir::finish(editor, &validation).map_err(|error| {
        (
            CompileError::new(
                ErrorKind::InternalError(format!("RIR module payload validation failed: {error}")),
                rue_span::Span::new(0, 0),
            ),
            work,
        )
    })?;
    let translation = symbols.work();
    work.symbol_fields_translated = translation.local_symbol_resolutions;
    work.semantic_intern_attempts = translation.semantic_intern_attempts;
    work.unique_semantic_strings = translation.unique_semantic_strings;
    work.semantic_strings_retained = symbols.interner().len();
    Ok(ModuleRirOutput {
        revision: module.revision().clone(),
        source_length,
        rir,
        symbols,
        work,
    })
}

/// Assemble the deterministic whole-program compatibility view from canonical
/// module lowering terminals. This projection never traverses parser AST.
pub(crate) fn project_module_rirs_with_work(
    merged: &CanonicalMergedProgram,
    modules: &[std::sync::Arc<ModuleRirOutput>],
    query_work: CanonicalRirWork,
) -> Result<CanonicalRirOutput, (CompileError, CanonicalRirWork)> {
    let ast = merged.ast();
    if modules.len() != ast.modules().len()
        || modules
            .iter()
            .zip(ast.modules())
            .any(|(lowered, parsed)| lowered.revision() != parsed.revision())
    {
        return Err((
            CompileError::new(
                ErrorKind::InternalError(
                    "module RIR terminals do not match the canonical parsed projection".to_string(),
                ),
                rue_span::Span::new(0, 0),
            ),
            query_work,
        ));
    }
    let symbols = SemanticSymbolUniverse::from_modules(ast.modules());
    let mut editor = RirEditor::new();
    let mut module_ranges = Vec::with_capacity(modules.len());
    let mut work = query_work;
    for (lowered, parsed) in modules.iter().zip(ast.modules()) {
        let appended = editor
            .append_remapped_with_spans(
                &lowered.rir,
                |local| {
                    let text = lowered
                        .symbols
                        .interner()
                        .try_resolve(&local)
                        .expect("validated module RIR symbol belongs to its module universe");
                    symbols.interner().get_or_intern(text)
                },
                |span| rue_span::Span::with_file(parsed.file_id(), span.start, span.end),
            )
            .map_err(|error| {
                (
                    CompileError::new(
                        rir_build_error_kind("RIR module projection failed", &error),
                        rue_span::Span::new(0, 0),
                    ),
                    work,
                )
            })?;
        work.modules_projected += 1;
        work.instructions_appended += appended.instructions.len();
        work.payload_words_appended += appended.extra.len();
        module_ranges.push(CanonicalRirModuleRange {
            file_id: parsed.file_id(),
            instructions: appended.instructions,
            extra: appended.extra,
        });
    }
    let sources = modules
        .iter()
        .zip(ast.modules())
        .map(|(module, parsed)| CanonicalRirSource {
            file_id: parsed.file_id(),
            revision: module.revision.clone(),
            length: module.source_length,
        })
        .collect::<Vec<_>>();
    let source_lengths = sources
        .iter()
        .map(|source| (source.file_id, source.length))
        .collect::<Vec<_>>();
    let validation = RirValidationContext {
        symbol_count: symbols.interner().len(),
        source_lengths: &source_lengths,
    };
    let rir = ValidatedRir::finish(editor, &validation).map_err(|error| {
        (
            CompileError::new(
                ErrorKind::InternalError(format!("RIR payload validation failed: {error}")),
                rue_span::Span::new(0, 0),
            ),
            work,
        )
    })?;
    work.semantic_strings_retained = symbols.interner().len();
    Ok(CanonicalRirOutput {
        source_revision: ast.source_revision().clone(),
        rir,
        symbols,
        work,
        module_ranges,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use rue_rir::RirPrinter;
    use rue_span::FileId;

    use super::*;
    use crate::parsed_modules::{ParsedProgram, parse_source_snapshot_modules};
    use crate::{SourceMetadata, SourceSnapshot};

    #[test]
    fn rir_capacity_rejections_are_resource_limits_not_internal_errors() {
        // Spec C.1:2 / RUE-1221: a program too large for the u32 instruction
        // array or the u32-indexed payload word store is a diagnosable
        // compile-time failure (E1401) naming the limit, not an ICE.
        let limit = rir_build_error_kind(
            "ctx",
            &RirPayloadBuildError::ResourceLimitExceeded {
                family: "payload words",
            },
        );
        assert_eq!(limit.code(), rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT);
        assert!(limit.to_string().contains("payload words"));
        assert!(limit.to_string().contains("4294967295"));
        assert!(!limit.to_string().contains("internal compiler"));

        assert_eq!(
            rir_build_error_kind(
                "ctx",
                &RirPayloadBuildError::CapacityFailure {
                    family: "call args"
                },
            )
            .code(),
            rue_error::ErrorCode::COMPILER_RESOURCE_EXHAUSTION
        );
        assert_eq!(
            rir_build_error_kind(
                "ctx",
                &RirPayloadBuildError::InvalidBuilderInput {
                    family: "call args",
                    reason: "bad request",
                },
            )
            .code(),
            rue_error::ErrorCode::INTERNAL_ERROR
        );
    }

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

    fn print_in_snapshot_order(output: &CanonicalRirOutput, source: &SourceSnapshot) -> String {
        let order = output.presentation_order(source.files().map(|file| file.file_id));
        RirPrinter::with_presentation_order(
            output.rir(),
            output.symbols.interner(),
            order.instructions,
            order.extra,
        )
        .to_string()
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
        let stages = crate::test_support::test_frontend_stages(&source).unwrap();
        let merged = &stages.merged;
        let output = &stages.rir;
        let rendered = print(output);

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
    fn module_lowering_failure_retains_completed_work() {
        let source = snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }")], 1);
        let parsed = parse_source_snapshot_modules(&source).unwrap();
        let faulty = parsed.modules()[0].with_test_foreign_ast_symbol();
        let (error, work) = lower_module_rir_with_work(faulty).unwrap_err();
        assert!(error.to_string().contains("AST symbol is absent"));
        assert_eq!(work.modules_visited, 1);
        assert_eq!(work.items_visited, 1);
        assert_eq!(work.modules_projected, 0);
    }

    #[test]
    fn projection_failure_preserves_incoming_query_work() {
        let source = snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }")], 1);
        let merged = crate::test_support::test_merged_program(&source).unwrap();
        let query_work = CanonicalRirWork {
            modules_visited: 1,
            items_visited: 1,
            ..CanonicalRirWork::default()
        };
        let (_, failure_work) =
            project_module_rirs_with_work(&merged, &[], query_work).unwrap_err();
        assert_eq!(failure_work, query_work);
    }

    /// Lowering consumes whatever module order [`ParsedProgram`] publishes, and
    /// that order is canonical regardless of assembly order — so a reversed
    /// module vector cannot reach lowering as a different program.
    #[test]
    fn reordered_arc_assembly_publishes_one_canonical_module_order() {
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

        assert!(
            first
                .modules()
                .iter()
                .zip(second.modules())
                .all(|(left, right)| Arc::ptr_eq(left, right))
        );
        let rir = crate::test_support::test_canonical_rir(&source).unwrap();
        assert_eq!(rir.source_revision(), second.source_revision());
    }

    #[test]
    fn caller_order_presentation_differs_from_canonical_semantic_order() {
        let source = snapshot(
            &[
                (
                    8,
                    "/checkout/z.rue",
                    "z.rue",
                    "fn zed() -> i32 { let z = 40; z + 2 }",
                ),
                (
                    3,
                    "/checkout/a.rue",
                    "a.rue",
                    "fn alpha() -> i32 { let a = 1; a }",
                ),
            ],
            8,
        );
        let canonical = crate::test_support::test_canonical_rir(&source).unwrap();
        let semantic = print(&canonical);
        let presentation = print_in_snapshot_order(&canonical, &source);
        assert!(semantic.find("alpha").unwrap() < semantic.find("zed").unwrap());
        assert!(presentation.find("zed").unwrap() < presentation.find("alpha").unwrap());
        assert_eq!(canonical.work().parser_invocations, 0);
    }

    #[test]
    fn presentation_survives_file_id_and_physical_path_relocation() {
        let first = snapshot(
            &[
                (9, "/old/z.rue", "z.rue", "fn zed() -> i32 { 9 }"),
                (2, "/old/a.rue", "a.rue", "fn alpha() -> i32 { 2 }"),
            ],
            9,
        );
        let relocated = snapshot(
            &[
                (91, "/new/z.rue", "z.rue", "fn zed() -> i32 { 9 }"),
                (27, "/new/a.rue", "a.rue", "fn alpha() -> i32 { 2 }"),
            ],
            91,
        );
        let lower =
            |source: &SourceSnapshot| crate::test_support::test_canonical_rir(source).unwrap();
        let first_rir = lower(&first);
        let relocated_rir = lower(&relocated);

        assert_eq!(
            print_in_snapshot_order(&first_rir, &first),
            print_in_snapshot_order(&relocated_rir, &relocated)
        );
    }

    #[test]
    fn adversarial_symbol_surfaces_are_translated() {
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
                        fn exercise(values: [i32; LENGTH], text: StrBuf) -> i32 {
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
        let canonical = crate::test_support::test_canonical_rir(&source).unwrap();
        let rendered = print(&canonical);

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
