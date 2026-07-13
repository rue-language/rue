//! Self-contained immutable parsed-module artifacts.
//!
//! This is the reuse-safe syntax boundary. Each module owns its parser symbol
//! universe, while [`ParsedProgram`] provides the sole parsed-program
//! representation used by semantic compilation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[cfg(test)]
use lasso::Key;
use lasso::{RodeoResolver, Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileErrors, CompileResult, ErrorKind};
use rue_parser::{
    AssignTarget, Ast, Expr, IntrinsicArg, Item, Pattern, Statement, TypeExpr, ast::Visibility,
};
use rue_span::{FileId, Span};
use tracing::info_span;

use crate::definition_snapshot::{definition_parts, validate_span};
use crate::{
    DefinitionKind, DefinitionNamespace, ImportDirective, ImportDirectives, ModuleId,
    ModuleRevision, MultiErrorResult, SourceId, SourceRevision, SourceSnapshot, SyntaxWork,
};

#[derive(Debug)]
struct SymbolProvenance;

/// A symbol handle bound to exactly one frozen module universe.
#[derive(Debug, Clone)]
pub struct ParsedSymbol {
    spur: Spur,
    provenance: Arc<SymbolProvenance>,
}

impl ParsedSymbol {
    #[cfg(test)]
    pub(crate) fn test_local_ordinal(&self) -> usize {
        self.spur.into_usize()
    }
}

/// Immutable symbol resolver for one parsed module.
#[derive(Debug)]
pub struct FrozenSymbolResolver {
    resolver: Arc<RodeoResolver<Spur>>,
    provenance: Arc<SymbolProvenance>,
}

impl FrozenSymbolResolver {
    /// Resolve only a handle issued by this exact symbol universe.
    pub fn resolve<'a>(&'a self, symbol: &ParsedSymbol) -> CompileResult<&'a str> {
        if !Arc::ptr_eq(&self.provenance, &symbol.provenance) {
            return Err(invalid_input(
                "parsed symbol belongs to a foreign symbol universe",
            ));
        }
        self.resolver
            .try_resolve(&symbol.spur)
            .ok_or_else(|| invalid_input("parsed symbol is absent from its frozen resolver"))
    }

    fn symbol(&self, spur: Spur) -> CompileResult<ParsedSymbol> {
        self.resolver
            .try_resolve(&spur)
            .ok_or_else(|| invalid_input("AST symbol is absent from its frozen resolver"))?;
        Ok(ParsedSymbol {
            spur,
            provenance: self.provenance.clone(),
        })
    }
}

#[derive(Debug)]
struct ProvenancedAst {
    ast: Arc<Ast>,
    provenance: Arc<SymbolProvenance>,
    source: SourceId,
}

/// Snapshot-local occurrence of one parsed definition candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParsedDefinitionOccurrence(u32);

impl ParsedDefinitionOccurrence {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One presemantic definition candidate; duplicates remain distinct values.
#[derive(Debug, Clone)]
pub struct ParsedDefinitionCandidate {
    occurrence: ParsedDefinitionOccurrence,
    namespace: DefinitionNamespace,
    kind: DefinitionKind,
    visibility: Option<Visibility>,
    name: Arc<str>,
    symbol: ParsedSymbol,
    name_span: Span,
    declaration_span: Span,
}

impl ParsedDefinitionCandidate {
    pub fn occurrence(&self) -> ParsedDefinitionOccurrence {
        self.occurrence
    }
    pub fn namespace(&self) -> DefinitionNamespace {
        self.namespace
    }
    pub fn kind(&self) -> DefinitionKind {
        self.kind
    }
    pub fn visibility(&self) -> Option<Visibility> {
        self.visibility
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn symbol(&self) -> &ParsedSymbol {
        &self.symbol
    }
    pub fn name_span(&self) -> Span {
        self.name_span
    }
    pub fn declaration_span(&self) -> Span {
        self.declaration_span
    }
}

/// Immutable per-module definition-candidate index.
#[derive(Debug, Clone)]
pub struct ParsedDefinitionIndex {
    candidates: Arc<[ParsedDefinitionCandidate]>,
    by_name: BTreeMap<(DefinitionNamespace, Arc<str>), Arc<[ParsedDefinitionOccurrence]>>,
}

impl ParsedDefinitionIndex {
    pub fn candidates(&self) -> &[ParsedDefinitionCandidate] {
        &self.candidates
    }

    pub fn candidates_named(
        &self,
        namespace: DefinitionNamespace,
        name: &str,
    ) -> impl Iterator<Item = &ParsedDefinitionCandidate> + '_ {
        self.by_name
            .get(&(namespace, Arc::from(name)))
            .into_iter()
            .flat_map(|occurrences| occurrences.iter())
            .map(|occurrence| &self.candidates[occurrence.index()])
    }
}

/// One import occurrence extracted directly into a reusable module artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParsedImportDirective {
    importer: ModuleId,
    source_offset: u32,
    specifier: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ParsedImportSite {
    source_offset: u32,
    specifier: Arc<str>,
}

/// Immutable parsed syntax whose spans and symbols belong to one FileId epoch.
#[derive(Debug)]
struct ParsedSyntaxPayload {
    source: SourceId,
    file_id: FileId,
    source_text: Arc<String>,
    ast: ProvenancedAst,
    resolver: FrozenSymbolResolver,
    definitions: ParsedDefinitionIndex,
    import_sites: Arc<[ParsedImportSite]>,
}

impl ParsedImportDirective {
    pub fn importer(&self) -> &ModuleId {
        &self.importer
    }
    pub fn source_offset(&self) -> u32 {
        self.source_offset
    }
    pub fn specifier(&self) -> &str {
        &self.specifier
    }
}

/// Immutable, Arc-shareable parsed syntax and exact local provenance.
#[derive(Debug)]
pub struct ParsedModule {
    revision: ModuleRevision,
    physical_path: Arc<str>,
    payload: Arc<ParsedSyntaxPayload>,
    imports: Arc<[ParsedImportDirective]>,
}

/// An AST paired with the exact parsed module that owns all of its symbols.
///
/// Views are issued only by [`ParsedProgram`]; cloning a view retains the
/// pointer-identical parsed module rather than copying its AST payload.
#[derive(Debug, Clone)]
pub struct ParsedAstView {
    module: Arc<ParsedModule>,
}

impl ParsedAstView {
    pub(crate) fn from_module(module: Arc<ParsedModule>) -> Self {
        Self { module }
    }

    pub fn module(&self) -> &Arc<ParsedModule> {
        &self.module
    }

    pub fn module_id(&self) -> &ModuleId {
        self.module.module_id()
    }

    pub fn ast(&self) -> &Ast {
        self.module.ast()
    }

    pub fn items(&self) -> impl ExactSizeIterator<Item = ParsedItemView> + '_ {
        (0..self.module.ast().items.len()).map(|index| ParsedItemView {
            module: self.module.clone(),
            index,
        })
    }
}

/// One parsed item paired with the module that owns its local symbols.
#[derive(Debug, Clone)]
pub struct ParsedItemView {
    module: Arc<ParsedModule>,
    index: usize,
}

impl ParsedItemView {
    pub(crate) fn from_module_index(module: Arc<ParsedModule>, index: usize) -> Self {
        debug_assert!(index < module.ast().items.len());
        Self { module, index }
    }

    pub fn module(&self) -> &Arc<ParsedModule> {
        &self.module
    }

    pub fn module_id(&self) -> &ModuleId {
        self.module.module_id()
    }

    pub fn item(&self) -> &Item {
        &self.module.ast().items[self.index]
    }
}

impl ParsedModule {
    pub(crate) fn shares_resolver_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(
            &self.payload.resolver.resolver,
            &other.payload.resolver.resolver,
        )
    }

    pub(crate) fn resolver_strings(&self) -> impl Iterator<Item = &str> {
        self.payload
            .resolver
            .resolver
            .iter()
            .map(|(_, value)| value)
    }

    pub fn revision(&self) -> &ModuleRevision {
        &self.revision
    }
    pub fn module_id(&self) -> &ModuleId {
        &self.revision.module
    }
    pub fn source_id(&self) -> &SourceId {
        &self.revision.source
    }
    pub fn file_id(&self) -> FileId {
        self.payload.file_id
    }
    pub fn physical_path(&self) -> &str {
        &self.physical_path
    }
    pub fn source_text(&self) -> &str {
        &self.payload.source_text
    }
    pub(crate) fn shared_source_text(&self) -> Arc<String> {
        self.payload.source_text.clone()
    }
    pub fn ast(&self) -> &Ast {
        &self.payload.ast.ast
    }
    /// Retain the immutable AST payload without projecting it into another
    /// syntax representation.
    pub fn shared_ast(&self) -> Arc<Ast> {
        self.payload.ast.ast.clone()
    }
    pub fn definitions(&self) -> &ParsedDefinitionIndex {
        &self.payload.definitions
    }
    pub fn imports(&self) -> &[ParsedImportDirective] {
        &self.imports
    }

    pub fn resolve(&self, symbol: &ParsedSymbol) -> CompileResult<&str> {
        self.payload.resolver.resolve(symbol)
    }

    pub(crate) fn parsed_symbol(&self, spur: Spur) -> CompileResult<ParsedSymbol> {
        self.payload.resolver.symbol(spur)
    }

    #[cfg(test)]
    fn payload_ptr(&self) -> *const ParsedSyntaxPayload {
        Arc::as_ptr(&self.payload)
    }
}

/// Exact work performed while assembling reusable parsed modules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParsedModulesWork {
    /// Lexing and parsing performed for modules that could not be reused.
    pub syntax: SyntaxWork,
    /// Previous program modules inserted into the point-lookup index.
    pub previous_modules_indexed: usize,
    /// Snapshot modules classified by this assembly.
    pub modules_considered: usize,
    /// Point lookups performed against the previous-module index.
    pub previous_module_lookups: usize,
    /// Entire ParsedModule Arcs retained unchanged.
    pub modules_reused: usize,
    /// Cheap envelopes rebuilt around retained syntax payloads.
    pub modules_rebound: usize,
    /// Modules lexed and parsed because source or FileId epoch changed.
    pub modules_reparsed: usize,
    /// Deep source-buffer clones performed while reusing modules.
    pub source_text_clones: usize,
    /// Source bytes rehashed while classifying reusable modules.
    pub source_bytes_rehashed: usize,
}

/// Deterministically ordered collection of independently parsed modules.
#[derive(Debug, Clone)]
pub struct ParsedProgram {
    source_revision: SourceRevision,
    modules: Arc<[Arc<ParsedModule>]>,
    imports: ImportDirectives,
}

impl ParsedProgram {
    pub fn new(root: ModuleId, mut modules: Vec<Arc<ParsedModule>>) -> CompileResult<Self> {
        modules.sort_by(|left, right| left.module_id().cmp(right.module_id()));
        let mut file_ids = modules
            .iter()
            .map(|module| (module.file_id(), module.module_id()))
            .collect::<Vec<_>>();
        file_ids.sort_by_key(|(file_id, _)| file_id.index());
        if let Some(duplicate) = file_ids.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(invalid_input(format!(
                "parsed program contains duplicate file ID {} for modules {} and {}",
                duplicate[0].0.index(),
                duplicate[0].1,
                duplicate[1].1
            )));
        }
        let source_revision = SourceRevision::new(
            root,
            modules
                .iter()
                .map(|module| module.revision().clone())
                .collect(),
        )?;
        let imports = ImportDirectives::from_records(
            modules
                .iter()
                .flat_map(|module| module.imports().iter())
                .map(|directive| {
                    ImportDirective::new(
                        directive.importer.clone(),
                        directive.source_offset,
                        directive.specifier.clone(),
                    )
                })
                .collect(),
        );
        Ok(Self {
            source_revision,
            modules: modules.into(),
            imports,
        })
    }

    pub fn root(&self) -> &ModuleId {
        self.source_revision.root()
    }
    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }
    pub fn modules(&self) -> &[Arc<ParsedModule>] {
        &self.modules
    }

    /// Look up a module by its stable logical identity.
    pub fn module(&self, id: &ModuleId) -> Option<&Arc<ParsedModule>> {
        self.modules
            .binary_search_by(|module| module.module_id().cmp(id))
            .ok()
            .map(|index| &self.modules[index])
    }

    /// Traverse module-qualified ASTs in canonical logical-module order.
    pub fn ast_views(&self) -> impl ExactSizeIterator<Item = ParsedAstView> + '_ {
        self.modules
            .iter()
            .cloned()
            .map(|module| ParsedAstView { module })
    }

    /// Canonical program-wide import occurrences, ready for graph resolution.
    pub fn import_directives(&self) -> &ImportDirectives {
        &self.imports
    }

    pub(crate) fn shared_symbol_strings(&self) -> Option<Vec<&str>> {
        let first = self.modules.first()?;
        if self.modules.iter().all(|module| {
            Arc::ptr_eq(
                &module.payload.resolver.resolver,
                &first.payload.resolver.resolver,
            )
        }) {
            Some(
                first
                    .payload
                    .resolver
                    .resolver
                    .iter()
                    .map(|(_, value)| value)
                    .collect(),
            )
        } else {
            None
        }
    }
}

/// Stable-module invalidations for one parse-session update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseInvalidationSummary {
    pub exact_reused: Vec<ModuleId>,
    pub payload_rebound: Vec<ModuleId>,
    pub reparsed: Vec<ModuleId>,
    pub added: Vec<ModuleId>,
    pub removed: Vec<ModuleId>,
}

/// Result and structural work from one canonical parse-session update.
#[derive(Debug)]
pub struct CanonicalParseUpdate {
    result: Result<Arc<ParsedProgram>, CompileErrors>,
    work: ParsedModulesWork,
    invalidation: ParseInvalidationSummary,
    #[cfg_attr(not(test), allow(dead_code))]
    baseline_advanced: bool,
}

impl CanonicalParseUpdate {
    #[cfg(test)]
    pub(crate) fn result(&self) -> Result<&Arc<ParsedProgram>, &CompileErrors> {
        self.result.as_ref()
    }

    pub fn into_result(self) -> Result<Arc<ParsedProgram>, CompileErrors> {
        self.result
    }

    pub fn work(&self) -> ParsedModulesWork {
        self.work
    }

    pub fn invalidation(&self) -> &ParseInvalidationSummary {
        &self.invalidation
    }

    #[cfg(test)]
    pub(crate) fn baseline_advanced(&self) -> bool {
        self.baseline_advanced
    }
}

/// In-process immutable canonical parse baseline for tooling queries.
#[derive(Debug, Default)]
pub struct CanonicalParseSession {
    baseline: Option<Arc<ParsedProgram>>,
}

impl CanonicalParseSession {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Seed a session only with a program belonging to this exact snapshot revision.
    #[cfg(test)]
    pub(crate) fn from_baseline(
        snapshot: &SourceSnapshot,
        baseline: Arc<ParsedProgram>,
    ) -> CompileResult<Self> {
        if baseline.source_revision() != snapshot.source_revision() {
            return Err(invalid_input(
                "parse-session baseline belongs to a foreign source revision",
            ));
        }
        Ok(Self {
            baseline: Some(baseline),
        })
    }

    #[cfg(test)]
    pub(crate) fn baseline(&self) -> Option<&Arc<ParsedProgram>> {
        self.baseline.as_ref()
    }

    /// Update from an immutable snapshot, publishing only successful results.
    /// Syntax diagnostics are complete and ordered by canonical `ModuleId`.
    /// On failure, invalidations are still relative to the last successful
    /// baseline and that baseline is not advanced.
    pub fn update(&mut self, snapshot: &SourceSnapshot) -> CanonicalParseUpdate {
        self.update_in_order(snapshot, DiagnosticOrder::Canonical)
    }

    /// One-shot batch adapter preserving the caller's established diagnostic
    /// order while publishing the same canonical parsed artifacts.
    pub(crate) fn update_for_batch(&mut self, snapshot: &SourceSnapshot) -> CanonicalParseUpdate {
        self.update_in_order(snapshot, DiagnosticOrder::Snapshot)
    }

    fn update_in_order(
        &mut self,
        snapshot: &SourceSnapshot,
        diagnostic_order: DiagnosticOrder,
    ) -> CanonicalParseUpdate {
        let invalidation = classify_invalidation(snapshot, self.baseline.as_deref());
        let outcome = parse_source_snapshot_modules_reusing_with_work(
            snapshot,
            self.baseline.as_deref(),
            diagnostic_order,
        );
        match outcome.result {
            Ok(program) => {
                let program = Arc::new(program);
                self.baseline = Some(program.clone());
                CanonicalParseUpdate {
                    result: Ok(program),
                    work: outcome.work,
                    invalidation,
                    baseline_advanced: true,
                }
            }
            Err(errors) => CanonicalParseUpdate {
                result: Err(errors),
                work: outcome.work,
                invalidation,
                baseline_advanced: false,
            },
        }
    }
}

fn classify_invalidation(
    snapshot: &SourceSnapshot,
    baseline: Option<&ParsedProgram>,
) -> ParseInvalidationSummary {
    let mut current = snapshot
        .metadata()
        .file_ids()
        .map(|file_id| {
            (
                snapshot.module_id(file_id).unwrap().clone(),
                file_id,
                snapshot.source_id(file_id).unwrap(),
                snapshot.metadata().physical_path(file_id).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    current.sort_by(|left, right| left.0.cmp(&right.0));
    let mut summary = ParseInvalidationSummary::default();
    let Some(baseline) = baseline else {
        summary.added = current.into_iter().map(|(module, ..)| module).collect();
        return summary;
    };

    for (module_id, file_id, source_id, physical_path) in &current {
        match baseline
            .modules()
            .binary_search_by(|module| module.module_id().cmp(module_id))
        {
            Ok(index) => {
                let previous = &baseline.modules()[index];
                if previous.file_id() == *file_id
                    && previous.source_id() == *source_id
                    && previous.physical_path() == *physical_path
                {
                    summary.exact_reused.push(module_id.clone());
                } else if previous.file_id() == *file_id && previous.source_id() == *source_id {
                    summary.payload_rebound.push(module_id.clone());
                } else {
                    summary.reparsed.push(module_id.clone());
                }
            }
            Err(_) => summary.added.push(module_id.clone()),
        }
    }
    for previous in baseline.modules() {
        if current
            .binary_search_by(|(module, ..)| module.cmp(previous.module_id()))
            .is_err()
        {
            summary.removed.push(previous.module_id().clone());
        }
    }
    summary
}

pub(crate) struct ParsedModulesOutcome {
    pub(crate) result: Result<ParsedProgram, CompileErrors>,
    pub(crate) work: ParsedModulesWork,
}

/// Parse every snapshot module independently and assemble canonical artifacts.
#[cfg(test)]
pub(crate) fn parse_source_snapshot_modules(
    snapshot: &SourceSnapshot,
) -> Result<ParsedProgram, CompileErrors> {
    parse_source_snapshot_modules_reusing(snapshot, None).map(|(program, _)| program)
}

/// Reuse exact syntax payloads while preserving canonical ModuleId diagnostic order.
///
/// Previous modules are indexed once by FileId. Hash-map iteration never
/// drives parsing, diagnostics, or artifact order; every snapshot module is
/// visited in canonical ModuleId order and performs at most one point lookup.
/// Caller-ordered syntax diagnostics are handled by the explicit AST
/// presentation adapter rather than another parsed-program representation.
#[cfg(test)]
pub(crate) fn parse_source_snapshot_modules_reusing(
    snapshot: &SourceSnapshot,
    previous: Option<&ParsedProgram>,
) -> Result<(ParsedProgram, ParsedModulesWork), CompileErrors> {
    let outcome = parse_source_snapshot_modules_reusing_with_work(
        snapshot,
        previous,
        DiagnosticOrder::Canonical,
    );
    outcome.result.map(|program| (program, outcome.work))
}

/// Caller-ordered syntax presentation for AST output.
#[derive(Debug)]
pub struct ParsedAstPresentation {
    files: Vec<(String, Arc<Ast>)>,
    work: ParsedAstPresentationWork,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParsedAstPresentationWork {
    pub parsed: ParsedModulesWork,
    pub merge_invocations: usize,
    pub astgen_invocations: usize,
    pub bind_invocations: usize,
    pub manifest_invocations: usize,
}

impl ParsedAstPresentation {
    pub fn files(&self) -> &[(String, Arc<Ast>)] {
        &self.files
    }

    pub fn work(&self) -> ParsedAstPresentationWork {
        self.work
    }
}

/// Parse once for exact shared-Spur AST presentation without merge or lowering.
pub fn parse_source_snapshot_for_ast_presentation(
    snapshot: &SourceSnapshot,
) -> MultiErrorResult<ParsedAstPresentation> {
    let _span = info_span!(
        "parse",
        file_count = snapshot.len(),
        purpose = "ast_presentation"
    )
    .entered();
    let outcome = crate::syntax::parse_snapshot_for_presentation(snapshot);
    let syntax = outcome.work;
    let asts = outcome.result?;
    let files = snapshot
        .files()
        .zip(asts)
        .map(|(source, ast)| (source.path.to_string(), ast))
        .collect();
    Ok(ParsedAstPresentation {
        files,
        work: ParsedAstPresentationWork {
            parsed: ParsedModulesWork {
                syntax,
                modules_considered: snapshot.len(),
                modules_reparsed: snapshot.len(),
                ..ParsedModulesWork::default()
            },
            ..ParsedAstPresentationWork::default()
        },
    })
}

/// Parse one stable module and return the exact syntax work performed.
#[cfg(test)]
pub(crate) fn parse_source_snapshot_module_with_stats(
    snapshot: &SourceSnapshot,
    module: &ModuleId,
) -> Result<(Arc<ParsedModule>, SyntaxWork), CompileErrors> {
    let file_id = snapshot
        .metadata()
        .file_ids()
        .find(|file_id| snapshot.module_id(*file_id) == Some(module))
        .ok_or_else(|| {
            CompileErrors::from(invalid_input(format!(
                "source snapshot contains no module {module}"
            )))
        })?;
    let (result, work) = parse_snapshot_file(snapshot, file_id);
    result.map(|module| (module, work))
}

fn parse_snapshot_file(
    snapshot: &SourceSnapshot,
    file_id: FileId,
) -> (Result<Arc<ParsedModule>, CompileErrors>, SyntaxWork) {
    let source = snapshot.source(file_id).expect("metadata membership");
    let outcome = crate::syntax::parse_file(source, ThreadedRodeo::new());
    let work = outcome.work;
    let result = outcome.result.and_then(|ast| {
        build_module(snapshot, file_id, ast, outcome.interner).map_err(CompileErrors::from)
    });
    (result, work)
}

#[derive(Clone, Copy)]
enum DiagnosticOrder {
    Canonical,
    Snapshot,
}

fn parse_source_snapshot_modules_reusing_with_work(
    snapshot: &SourceSnapshot,
    previous: Option<&ParsedProgram>,
    diagnostic_order: DiagnosticOrder,
) -> ParsedModulesOutcome {
    let mut file_ids = if matches!(diagnostic_order, DiagnosticOrder::Canonical) {
        snapshot.metadata().file_ids().collect::<Vec<_>>()
    } else {
        snapshot.files().map(|source| source.file_id).collect()
    };
    if matches!(diagnostic_order, DiagnosticOrder::Canonical) {
        file_ids.sort_by(|left, right| {
            snapshot
                .module_id(*left)
                .unwrap()
                .cmp(snapshot.module_id(*right).unwrap())
        });
    }
    let mut modules = Vec::with_capacity(file_ids.len());
    let mut errors = CompileErrors::new();
    let previous_by_file = previous
        .map(|program| {
            program
                .modules()
                .iter()
                .map(|module| (module.file_id(), module))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut work = ParsedModulesWork {
        previous_modules_indexed: previous_by_file.len(),
        ..ParsedModulesWork::default()
    };
    for file_id in file_ids {
        work.modules_considered += 1;
        work.previous_module_lookups += usize::from(previous.is_some());
        let module_id = snapshot.module_id(file_id).expect("snapshot membership");
        let source_id = snapshot.source_id(file_id).expect("snapshot membership");
        let physical_path = snapshot.metadata().physical_path(file_id).unwrap();
        let previous_module = previous_by_file.get(&file_id).copied();
        let exact = previous_module.filter(|module| {
            module.module_id() == module_id
                && module.source_id() == source_id
                && module.physical_path() == physical_path
        });
        let result = if let Some(module) = exact {
            work.modules_reused += 1;
            Ok(module.clone())
        } else if let Some(payload) = previous_module
            .filter(|module| module.source_id() == source_id)
            .map(|module| module.payload.clone())
        {
            work.modules_rebound += 1;
            Ok(bind_payload(snapshot, file_id, payload))
        } else {
            work.modules_reparsed += 1;
            let (result, file_work) = parse_snapshot_file(snapshot, file_id);
            work.syntax.lexer_invocations += file_work.lexer_invocations;
            work.syntax.parser_invocations += file_work.parser_invocations;
            work.syntax.lexed_bytes += file_work.lexed_bytes;
            work.syntax.tokens += file_work.tokens;
            result
        };
        match result {
            Ok(module) => modules.push(module),
            Err(file_errors) => errors.extend(file_errors),
        }
    }
    let result = if errors.is_empty() {
        ParsedProgram::new(snapshot.source_revision().root().clone(), modules)
            .map_err(CompileErrors::from)
    } else {
        Err(errors)
    };
    debug_assert_eq!(
        work.modules_considered,
        work.modules_reused + work.modules_rebound + work.modules_reparsed
    );
    ParsedModulesOutcome { result, work }
}

fn build_module(
    snapshot: &SourceSnapshot,
    file_id: FileId,
    ast: Arc<Ast>,
    interner: ThreadedRodeo,
) -> CompileResult<Arc<ParsedModule>> {
    let token = Arc::new(SymbolProvenance);
    let module = snapshot.module_id(file_id).expect("snapshot membership");
    let import_sites = collect_imports(&ast, module, &interner)?;
    let resolver = Arc::new(interner.into_resolver());
    build_module_with_resolver(snapshot, file_id, ast, resolver, token, import_sites)
}

fn build_module_with_resolver(
    snapshot: &SourceSnapshot,
    file_id: FileId,
    ast: Arc<Ast>,
    resolver: Arc<RodeoResolver<Spur>>,
    token: Arc<SymbolProvenance>,
    import_sites: Vec<ParsedImportSite>,
) -> CompileResult<Arc<ParsedModule>> {
    let module = snapshot
        .module_id(file_id)
        .expect("snapshot membership")
        .clone();
    let source = snapshot
        .source_id(file_id)
        .expect("snapshot membership")
        .clone();
    let source_text = snapshot
        .shared_source_text(file_id)
        .expect("snapshot membership");
    let resolver = FrozenSymbolResolver {
        resolver,
        provenance: token.clone(),
    };
    let provenanced_ast = ProvenancedAst {
        ast,
        provenance: token,
        source: source.clone(),
    };
    let revision = ModuleRevision {
        module,
        source: source.clone(),
    };
    validate_pair(&provenanced_ast, &resolver, &revision)?;
    let definitions =
        build_definition_index(file_id, &source_text, &provenanced_ast.ast, &resolver)?;
    let payload = Arc::new(ParsedSyntaxPayload {
        source,
        file_id,
        source_text,
        ast: provenanced_ast,
        resolver,
        definitions,
        import_sites: import_sites.into(),
    });
    Ok(bind_payload(snapshot, file_id, payload))
}

fn bind_payload(
    snapshot: &SourceSnapshot,
    file_id: FileId,
    payload: Arc<ParsedSyntaxPayload>,
) -> Arc<ParsedModule> {
    debug_assert_eq!(payload.file_id, file_id);
    debug_assert_eq!(snapshot.source_id(file_id), Some(&payload.source));
    let module = snapshot
        .module_id(file_id)
        .expect("snapshot membership")
        .clone();
    let revision = ModuleRevision {
        module: module.clone(),
        source: payload.source.clone(),
    };
    let imports = payload
        .import_sites
        .iter()
        .map(|site| ParsedImportDirective {
            importer: module.clone(),
            source_offset: site.source_offset,
            specifier: site.specifier.clone(),
        })
        .collect::<Vec<_>>();
    Arc::new(ParsedModule {
        revision,
        physical_path: Arc::from(snapshot.metadata().physical_path(file_id).unwrap()),
        payload,
        imports: imports.into(),
    })
}

fn collect_imports(
    ast: &Ast,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
) -> CompileResult<Vec<ParsedImportSite>> {
    let mut imports = Vec::new();
    for item in &ast.items {
        match item {
            Item::Function(value) => {
                walk_signature(
                    &value.params,
                    value.return_type.as_ref(),
                    module,
                    resolver,
                    &mut imports,
                )?;
                walk_expr(&value.body, module, resolver, &mut imports)?;
            }
            Item::Struct(value) => {
                for field in &value.fields {
                    walk_type_expr(&field.ty, module, resolver, &mut imports)?;
                }
                for method in &value.methods {
                    walk_signature(
                        &method.params,
                        method.return_type.as_ref(),
                        module,
                        resolver,
                        &mut imports,
                    )?;
                    walk_expr(&method.body, module, resolver, &mut imports)?;
                }
            }
            Item::DropFn(value) => walk_expr(&value.body, module, resolver, &mut imports)?,
            Item::Const(value) => {
                if let Some(ty) = &value.ty {
                    walk_type_expr(ty, module, resolver, &mut imports)?;
                }
                walk_expr(&value.init, module, resolver, &mut imports)?;
            }
            Item::Enum(value) => {
                for variant in &value.variants {
                    for ty in &variant.payload {
                        walk_type_expr(ty, module, resolver, &mut imports)?;
                    }
                }
            }
            Item::Error(_) => {}
        }
    }
    imports.sort();
    Ok(imports)
}

fn walk_signature(
    params: &[rue_parser::ast::Param],
    return_type: Option<&TypeExpr>,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportSite>,
) -> CompileResult<()> {
    for param in params {
        walk_type_expr(&param.ty, module, resolver, imports)?;
    }
    if let Some(return_type) = return_type {
        walk_type_expr(return_type, module, resolver, imports)?;
    }
    Ok(())
}

fn walk_type_expr(
    ty: &TypeExpr,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportSite>,
) -> CompileResult<()> {
    match ty {
        TypeExpr::Named(_)
        | TypeExpr::Qualified { .. }
        | TypeExpr::Unit(_)
        | TypeExpr::Never(_)
        | TypeExpr::StrFixed { .. }
        | TypeExpr::IntArg { .. } => {}
        TypeExpr::Array { element, .. } | TypeExpr::Slice { element, .. } => {
            walk_type_expr(element, module, resolver, imports)?;
        }
        TypeExpr::AnonymousStruct {
            fields, methods, ..
        } => {
            for field in fields {
                walk_type_expr(&field.ty, module, resolver, imports)?;
            }
            for method in methods {
                walk_signature(
                    &method.params,
                    method.return_type.as_ref(),
                    module,
                    resolver,
                    imports,
                )?;
                walk_expr(&method.body, module, resolver, imports)?;
            }
        }
        TypeExpr::AnonymousEnum { variants, .. } => {
            for variant in variants {
                for payload in &variant.payload {
                    walk_type_expr(payload, module, resolver, imports)?;
                }
            }
        }
        TypeExpr::PointerConst { pointee, .. } | TypeExpr::PointerMut { pointee, .. } => {
            walk_type_expr(pointee, module, resolver, imports)?;
        }
        TypeExpr::TypeCall { args, .. } | TypeExpr::QualifiedTypeCall { args, .. } => {
            for arg in args {
                walk_type_expr(arg, module, resolver, imports)?;
            }
        }
    }
    Ok(())
}

fn walk_args(
    args: &[rue_parser::ast::CallArg],
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportSite>,
) -> CompileResult<()> {
    for arg in args {
        walk_expr(&arg.expr, module, resolver, imports)?;
    }
    Ok(())
}

fn walk_block(
    block: &rue_parser::ast::BlockExpr,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportSite>,
) -> CompileResult<()> {
    for statement in &block.statements {
        match statement {
            Statement::Let(value) => walk_expr(&value.init, module, resolver, imports)?,
            Statement::Assign(value) => {
                match &value.target {
                    AssignTarget::Var(_) => {}
                    AssignTarget::Field(field) => {
                        walk_expr(&field.base, module, resolver, imports)?
                    }
                    AssignTarget::Index(index) => {
                        walk_expr(&index.base, module, resolver, imports)?;
                        walk_expr(&index.index, module, resolver, imports)?;
                    }
                }
                walk_expr(&value.value, module, resolver, imports)?;
            }
            Statement::Expr(value) => walk_expr(value, module, resolver, imports)?,
        }
    }
    walk_expr(&block.expr, module, resolver, imports)
}

fn walk_expr(
    expr: &Expr,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut Vec<ParsedImportSite>,
) -> CompileResult<()> {
    match expr {
        Expr::Int(_)
        | Expr::String(_)
        | Expr::Bool(_)
        | Expr::Unit(_)
        | Expr::Ident(_)
        | Expr::Continue(_)
        | Expr::SelfExpr(_)
        | Expr::Error(_) => {}
        Expr::TypeLit(value) => {
            walk_type_expr(&value.type_expr, module, resolver, imports)?;
        }
        Expr::Binary(value) => {
            walk_expr(&value.left, module, resolver, imports)?;
            walk_expr(&value.right, module, resolver, imports)?;
        }
        Expr::Unary(value) => walk_expr(&value.operand, module, resolver, imports)?,
        Expr::Paren(value) => walk_expr(&value.inner, module, resolver, imports)?,
        Expr::Block(value) => walk_block(value, module, resolver, imports)?,
        Expr::If(value) => {
            walk_expr(&value.cond, module, resolver, imports)?;
            walk_block(&value.then_block, module, resolver, imports)?;
            if let Some(block) = &value.else_block {
                walk_block(block, module, resolver, imports)?;
            }
        }
        Expr::Match(value) => {
            walk_expr(&value.scrutinee, module, resolver, imports)?;
            for arm in &value.arms {
                if let Pattern::Path(path) = &arm.pattern {
                    if let Some(base) = &path.base {
                        walk_expr(base, module, resolver, imports)?;
                    }
                    if let Some(args) = &path.ctor_args {
                        walk_args(args, module, resolver, imports)?;
                    }
                }
                walk_expr(&arm.body, module, resolver, imports)?;
            }
        }
        Expr::While(value) => {
            walk_expr(&value.cond, module, resolver, imports)?;
            walk_block(&value.body, module, resolver, imports)?;
        }
        Expr::Loop(value) => walk_block(&value.body, module, resolver, imports)?,
        Expr::For(value) => {
            walk_expr(&value.iterable, module, resolver, imports)?;
            walk_block(&value.body, module, resolver, imports)?;
        }
        Expr::Call(value) => walk_args(&value.args, module, resolver, imports)?,
        Expr::Break(value) => {
            if let Some(value) = &value.value {
                walk_expr(value, module, resolver, imports)?;
            }
        }
        Expr::Return(value) => {
            if let Some(value) = &value.value {
                walk_expr(value, module, resolver, imports)?;
            }
        }
        Expr::StructLit(value) => {
            if let Some(base) = &value.base {
                walk_expr(base, module, resolver, imports)?;
            }
            if let Some(args) = &value.ctor_args {
                walk_args(args, module, resolver, imports)?;
            }
            for field in &value.fields {
                walk_expr(&field.value, module, resolver, imports)?;
            }
        }
        Expr::Field(value) => walk_expr(&value.base, module, resolver, imports)?,
        Expr::MethodCall(value) => {
            walk_expr(&value.receiver, module, resolver, imports)?;
            walk_args(&value.args, module, resolver, imports)?;
        }
        Expr::Try(value) => walk_expr(&value.operand, module, resolver, imports)?,
        Expr::IntrinsicCall(value) => {
            let name = resolver.try_resolve(&value.name.name).ok_or_else(|| {
                invalid_input("intrinsic name is absent from the module symbol universe")
            })?;
            if name == "import"
                && let [IntrinsicArg::Expr(Expr::String(literal))] = value.args.as_slice()
            {
                let specifier = resolver.try_resolve(&literal.value).ok_or_else(|| {
                    invalid_input("import literal is absent from the module symbol universe")
                })?;
                imports.push(ParsedImportSite {
                    source_offset: value.span.start,
                    specifier: Arc::from(specifier),
                });
            }
            for arg in &value.args {
                if let IntrinsicArg::Expr(expr) = arg {
                    walk_expr(expr, module, resolver, imports)?;
                }
            }
        }
        Expr::ArrayLit(value) => {
            for element in &value.elements {
                walk_expr(element, module, resolver, imports)?;
            }
        }
        Expr::Index(value) => {
            walk_expr(&value.base, module, resolver, imports)?;
            walk_expr(&value.index, module, resolver, imports)?;
        }
        Expr::Path(value) => {
            if let Some(base) = &value.base {
                walk_expr(base, module, resolver, imports)?;
            }
        }
        Expr::Comptime(value) => walk_expr(&value.expr, module, resolver, imports)?,
        Expr::Checked(value) => walk_expr(&value.expr, module, resolver, imports)?,
    }
    Ok(())
}

fn validate_pair(
    ast: &ProvenancedAst,
    resolver: &FrozenSymbolResolver,
    revision: &ModuleRevision,
) -> CompileResult<()> {
    if !Arc::ptr_eq(&ast.provenance, &resolver.provenance) {
        return Err(invalid_input(
            "parsed AST and resolver have foreign provenance",
        ));
    }
    if ast.source != revision.source {
        return Err(invalid_input(
            "parsed AST and module revision have foreign source provenance",
        ));
    }
    Ok(())
}

fn build_definition_index(
    file_id: FileId,
    source_text: &str,
    ast: &Ast,
    resolver: &FrozenSymbolResolver,
) -> CompileResult<ParsedDefinitionIndex> {
    let mut pending = Vec::new();
    for item in &ast.items {
        let Some(parts) = definition_parts(item) else {
            let Item::Error(span) = item else {
                unreachable!()
            };
            return Err(invalid_input(format!(
                "parsed module contains recovered error item at {}..{}",
                span.start, span.end
            )));
        };
        validate_span(
            "definition declaration",
            parts.declaration_span,
            file_id,
            source_text,
        )?;
        validate_span("definition name", parts.name.span, file_id, source_text)?;
        if parts.name.span.start < parts.declaration_span.start
            || parts.name.span.end > parts.declaration_span.end
        {
            return Err(invalid_input(
                "definition name span is outside its declaration span",
            ));
        }
        let symbol = resolver.symbol(parts.name.name)?;
        let name: Arc<str> = Arc::from(resolver.resolve(&symbol)?);
        pending.push((parts, symbol, name));
    }
    pending.sort_by(|(left, _, left_name), (right, _, right_name)| {
        (
            left.declaration_span.start,
            left.declaration_span.end,
            left.kind,
            left_name,
        )
            .cmp(&(
                right.declaration_span.start,
                right.declaration_span.end,
                right.kind,
                right_name,
            ))
    });
    let mut by_name = BTreeMap::<_, Vec<_>>::new();
    let mut candidates = Vec::with_capacity(pending.len());
    for (index, (parts, symbol, name)) in pending.into_iter().enumerate() {
        let index = u32::try_from(index)
            .map_err(|_| invalid_input("parsed definition occurrence count exceeds u32"))?;
        let occurrence = ParsedDefinitionOccurrence(index);
        by_name
            .entry((parts.namespace, name.clone()))
            .or_default()
            .push(occurrence);
        candidates.push(ParsedDefinitionCandidate {
            occurrence,
            namespace: parts.namespace,
            kind: parts.kind,
            visibility: parts.visibility,
            name,
            symbol,
            name_span: parts.name.span,
            declaration_span: parts.declaration_span,
        });
    }
    let by_name = by_name
        .into_iter()
        .map(|(key, value)| (key, value.into()))
        .collect();
    Ok(ParsedDefinitionIndex {
        candidates: candidates.into(),
        by_name,
    })
}

fn invalid_input(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lasso::Key;
    use rue_error::{PreviewFeature, PreviewFeatures};

    use super::*;
    use crate::{
        ModuleResolutionInput, ModuleResolutionInputs, SemanticInputDescriptor, SourceMetadata,
        extract_import_directives, lower_canonical_rir, merge_parsed_modules,
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

    fn error_fingerprint(errors: &CompileErrors) -> Vec<String> {
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

    #[test]
    fn modules_are_canonical_arc_shareable_and_carry_import_values() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ParsedModule>();
        assert_send_sync::<ParsedProgram>();

        let snapshot = snapshot(
            &[
                (
                    20,
                    "/p/main.rue",
                    "app/main.rue",
                    "fn same() {} fn same() {} fn main() -> i32 { if true { let h = @import(\"helper.rue\"); } 0 }",
                ),
                (1, "/p/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            20,
        );
        let outcome = parse_source_snapshot_modules_reusing_with_work(
            &snapshot,
            None,
            DiagnosticOrder::Canonical,
        );
        assert_eq!(outcome.work.syntax.lexer_invocations, 2);
        assert_eq!(outcome.work.syntax.parser_invocations, 2);
        let program = outcome.result.unwrap();
        assert_eq!(
            program
                .modules()
                .iter()
                .map(|m| m.module_id().as_str())
                .collect::<Vec<_>>(),
            ["app/helper.rue", "app/main.rue"]
        );
        let main = &program.modules()[1];
        assert_eq!(main.imports().len(), 1);
        assert_eq!(main.imports()[0].specifier(), "helper.rue");
        assert_eq!(main.imports()[0].importer(), main.module_id());
        let duplicates = main
            .definitions()
            .candidates_named(DefinitionNamespace::ModuleItem, "same")
            .collect::<Vec<_>>();
        assert_eq!(duplicates.len(), 2);
        assert_ne!(duplicates[0].occurrence(), duplicates[1].occurrence());
        assert_eq!(main.resolve(duplicates[0].symbol()).unwrap(), "same");
        let graph = crate::resolve_canonical_import_graph(
            program.import_directives(),
            &ModuleResolutionInputs::from_metadata(snapshot.metadata()),
            None,
        )
        .unwrap();
        assert_eq!(graph.records().len(), 1);
    }

    #[test]
    fn foreign_same_numeric_spur_and_ast_resolver_pairs_fail_closed() {
        let make = || {
            let rodeo = ThreadedRodeo::new();
            let spur = rodeo.get_or_intern("same-index");
            let provenance = Arc::new(SymbolProvenance);
            (
                FrozenSymbolResolver {
                    resolver: Arc::new(rodeo.into_resolver()),
                    provenance: provenance.clone(),
                },
                ParsedSymbol { spur, provenance },
            )
        };
        let (first, symbol) = make();
        let (foreign, foreign_symbol) = make();
        assert_eq!(symbol.spur.into_usize(), foreign_symbol.spur.into_usize());
        assert_eq!(first.resolve(&symbol).unwrap(), "same-index");
        assert_eq!(
            foreign.resolve(&symbol).unwrap_err().to_string(),
            "invalid compiler input: parsed symbol belongs to a foreign symbol universe"
        );
        let source = SourceId::from_shared_text(Arc::new(String::from("one")));
        let ast = ProvenancedAst {
            ast: Arc::new(Ast { items: Vec::new() }),
            provenance: symbol.provenance,
            source: source.clone(),
        };
        let revision = ModuleRevision {
            module: ModuleId::from_logical_path("module.rue").unwrap(),
            source: source.clone(),
        };
        assert_eq!(
            validate_pair(&ast, &foreign, &revision)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: parsed AST and resolver have foreign provenance"
        );
        let own_resolver = FrozenSymbolResolver {
            resolver: Arc::new(ThreadedRodeo::new().into_resolver()),
            provenance: ast.provenance.clone(),
        };
        let foreign_revision = ModuleRevision {
            module: revision.module,
            source: SourceId::from_shared_text(Arc::new(String::from("two"))),
        };
        assert_eq!(
            validate_pair(&ast, &own_resolver, &foreign_revision)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: parsed AST and module revision have foreign source provenance"
        );
    }

    #[test]
    fn assembling_reused_modules_does_no_additional_syntax_work() {
        let snapshot = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/helper.rue", "helper.rue", "fn helper() {}"),
            ],
            7,
        );
        let outcome = parse_source_snapshot_modules_reusing_with_work(
            &snapshot,
            None,
            DiagnosticOrder::Canonical,
        );
        let work = outcome.work;
        let first = outcome.result.unwrap();
        let modules = first.modules().to_vec();
        let second = ParsedProgram::new(first.root().clone(), modules.clone()).unwrap();
        assert_eq!(first.source_revision(), second.source_revision());
        assert_eq!(first.source_revision(), snapshot.source_revision());
        assert!(
            first
                .modules()
                .iter()
                .zip(second.modules())
                .all(|(left, right)| Arc::ptr_eq(left, right))
        );

        let base = SemanticInputDescriptor::new(
            &snapshot,
            crate::Target::default(),
            &PreviewFeatures::default(),
        );
        let mut changed_target = base.clone();
        changed_target.target = if base.target == crate::Target::X86_64Linux {
            crate::Target::Aarch64Linux
        } else {
            crate::Target::X86_64Linux
        };
        let changed_resolution = ModuleResolutionInputs::new(
            base.resolution.root().clone(),
            base.resolution
                .modules()
                .iter()
                .map(|entry| ModuleResolutionInput {
                    module: entry.module.clone(),
                    physical_path: Arc::from(format!("/moved/{}", entry.module.as_str())),
                })
                .collect(),
        )
        .unwrap();
        let mut changed_features = base.clone();
        let mut features = PreviewFeatures::default();
        features.insert(PreviewFeature::TestInfra);
        changed_features.preview_features = crate::StablePreviewFeatures::new(&features);
        assert_ne!(base.target, changed_target.target);
        assert_ne!(base.resolution, changed_resolution);
        assert_ne!(base.preview_features, changed_features.preview_features);
        assert_eq!(work.syntax.lexer_invocations, 2);
        assert_eq!(work.syntax.parser_invocations, 2);
        assert_eq!(work.modules_reparsed, 2);
    }

    #[test]
    fn relocation_and_logical_rename_rebind_without_parsing() {
        let first = snapshot(
            &[(
                7,
                "/old/main.rue",
                "old-name.rue",
                "fn main() { @import(\"dep.rue\"); }",
            )],
            7,
        );
        let (first, initial) = parse_source_snapshot_modules_reusing(&first, None).unwrap();
        assert_eq!(initial.modules_reparsed, 1);
        let payload = first.modules()[0].payload_ptr();

        let moved = snapshot(
            &[(
                7,
                "/new/main.rue",
                "new-name.rue",
                "fn main() { @import(\"dep.rue\"); }",
            )],
            7,
        );
        let (moved, work) = parse_source_snapshot_modules_reusing(&moved, Some(&first)).unwrap();
        assert_eq!(work.syntax, SyntaxWork::default());
        assert_eq!(work.modules_reused, 0);
        assert_eq!(work.modules_rebound, 1);
        assert_eq!(work.modules_reparsed, 0);
        assert_eq!(work.previous_modules_indexed, 1);
        assert_eq!(work.modules_considered, 1);
        assert_eq!(work.previous_module_lookups, 1);
        assert_eq!(payload, moved.modules()[0].payload_ptr());
        assert_eq!(moved.modules()[0].physical_path(), "/new/main.rue");
        assert_eq!(moved.modules()[0].module_id().as_str(), "new-name.rue");
        assert_eq!(
            moved.modules()[0].imports()[0].importer(),
            moved.modules()[0].module_id()
        );
    }

    #[test]
    fn file_id_epoch_change_reparses_equal_source() {
        let first = snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
        let first = parse_source_snapshot_modules(&first).unwrap();
        let changed = snapshot(&[(9, "/main.rue", "main.rue", "fn main() {}")], 9);
        let (changed, work) =
            parse_source_snapshot_modules_reusing(&changed, Some(&first)).unwrap();
        assert_eq!(work.modules_reparsed, 1);
        assert_eq!(work.modules_considered, 1);
        assert_eq!(work.syntax.lexer_invocations, 1);
        assert_eq!(work.syntax.parser_invocations, 1);
        assert_ne!(
            first.modules()[0].payload_ptr(),
            changed.modules()[0].payload_ptr()
        );
        assert_eq!(changed.modules()[0].file_id(), FileId::new(9));
    }

    #[test]
    fn one_edit_among_128_modules_parses_exactly_once() {
        fn large(edited: bool) -> SourceSnapshot {
            let mut physical = HashMap::new();
            let mut logical = HashMap::new();
            let mut contents = Vec::new();
            for index in 0..129_u32 {
                let file_id = FileId::new(index + 1);
                physical.insert(file_id, format!("/p/m{index:03}.rue"));
                logical.insert(file_id, format!("m{index:03}.rue"));
                let source = if index == 64 && edited {
                    "fn changed() -> i32 { 2 }".to_owned()
                } else {
                    format!("fn item{index}() -> i32 {{ 1 }}")
                };
                contents.push((file_id, Arc::new(source)));
            }
            let metadata = SourceMetadata::new(FileId::new(1), physical, logical).unwrap();
            SourceSnapshot::new(metadata, contents).unwrap()
        }
        let first_snapshot = large(false);
        let first = parse_source_snapshot_modules(&first_snapshot).unwrap();
        let edited = large(true);
        let (second, work) = parse_source_snapshot_modules_reusing(&edited, Some(&first)).unwrap();
        assert_eq!(work.modules_reused, 128);
        assert_eq!(work.modules_rebound, 0);
        assert_eq!(work.modules_reparsed, 1);
        assert_eq!(work.previous_modules_indexed, 129);
        assert_eq!(work.modules_considered, 129);
        assert_eq!(work.previous_module_lookups, 129);
        assert_eq!(
            work.modules_considered,
            work.modules_reused + work.modules_rebound + work.modules_reparsed
        );
        assert_eq!(work.syntax.lexer_invocations, 1);
        assert_eq!(work.syntax.parser_invocations, 1);
        assert_eq!(second.modules().len(), 129);
        assert_eq!(
            first
                .modules()
                .iter()
                .zip(second.modules())
                .filter(|(left, right)| Arc::ptr_eq(left, right))
                .count(),
            128
        );
    }

    #[test]
    fn reuse_errors_keep_canonical_order_and_return_no_partial_program() {
        let good = snapshot(
            &[
                (1, "/z.rue", "z.rue", "fn zed() {}"),
                (2, "/a.rue", "a.rue", "fn alpha() {}"),
            ],
            2,
        );
        let previous = parse_source_snapshot_modules(&good).unwrap();
        let broken = snapshot(
            &[
                (1, "/z.rue", "z.rue", "fn zed( {"),
                (2, "/a.rue", "a.rue", "fn alpha() {}"),
            ],
            2,
        );
        let errors = parse_source_snapshot_modules_reusing(&broken, Some(&previous)).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors.iter().next().unwrap().span().unwrap().file_id,
            FileId::new(1)
        );

        let both_broken = snapshot(
            &[
                (1, "/z.rue", "z.rue", "fn zed( {"),
                (2, "/a.rue", "a.rue", "fn alpha( {"),
            ],
            2,
        );
        let errors =
            parse_source_snapshot_modules_reusing(&both_broken, Some(&previous)).unwrap_err();
        let order = errors
            .iter()
            .map(|error| error.span().unwrap().file_id)
            .collect::<Vec<_>>();
        assert_eq!(order, [FileId::new(2), FileId::new(1)]);
    }

    #[test]
    fn assembly_rejects_duplicate_request_local_file_ids_deterministically() {
        let first = snapshot(&[(7, "/a.rue", "a.rue", "fn a() {}")], 7);
        let second = snapshot(&[(7, "/b.rue", "b.rue", "fn b() {}")], 7);
        let a = parse_source_snapshot_modules(&first).unwrap().modules()[0].clone();
        let b = parse_source_snapshot_modules(&second).unwrap().modules()[0].clone();

        let error = ParsedProgram::new(a.module_id().clone(), vec![b, a])
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "invalid compiler input: parsed program contains duplicate file ID 7 for modules a.rue and b.rue"
        );
    }

    #[test]
    fn one_edited_module_reparses_once_and_reuses_unchanged_module_arc() {
        let initial = snapshot(
            &[
                (1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/helper.rue", "helper.rue", "fn helper() -> i32 { 1 }"),
            ],
            1,
        );
        let edited = snapshot(
            &[
                (1, "/main.rue", "main.rue", "fn main() -> i32 { 2 }"),
                (2, "/helper.rue", "helper.rue", "fn helper() -> i32 { 1 }"),
            ],
            1,
        );
        let old = parse_source_snapshot_modules(&initial).unwrap();
        let old_main = old
            .modules()
            .iter()
            .find(|module| module.module_id().as_str() == "main.rue")
            .unwrap();
        let unchanged = old
            .modules()
            .iter()
            .find(|module| module.module_id().as_str() == "helper.rue")
            .unwrap()
            .clone();
        let main_id = ModuleId::from_logical_path("main.rue").unwrap();
        let (new_main, work) = parse_source_snapshot_module_with_stats(&edited, &main_id).unwrap();
        assert_eq!(work.lexer_invocations, 1);
        assert_eq!(work.parser_invocations, 1);
        assert_ne!(old_main.revision(), new_main.revision());

        let assembled =
            ParsedProgram::new(main_id, vec![new_main.clone(), unchanged.clone()]).unwrap();
        let reused = assembled
            .modules()
            .iter()
            .find(|module| module.module_id().as_str() == "helper.rue")
            .unwrap();
        assert!(Arc::ptr_eq(reused, &unchanged));
        assert_eq!(assembled.source_revision(), edited.source_revision());
    }

    #[test]
    fn type_position_anonymous_method_imports_match_positional_rir_extraction() {
        let source = r#"
const top = @import("top");
fn consume(value: i32) {}
fn make_type() -> type {
    struct {
        field: i32,
        fn load() -> i32 {
            let body = @import("body");
            4
        }
    }
}
fn main() -> i32 {
    let array = [@import("array"), @import("array2")];
    if true { consume(@import("call_arg")); } else { let other = @import("else_block"); }
    let nested = @dbg(@import("intrinsic_arg"));
    let indexed = [@import("index_base")][0];
    comptime { @import("comptime") };
    0
}
"#;
        let snapshot = snapshot(&[(3, "/main.rue", "main.rue", source)], 3);
        let parsed = parse_source_snapshot_modules(&snapshot).unwrap();
        let parsed_values = parsed
            .import_directives()
            .iter()
            .map(|directive| (directive.source_offset(), directive.specifier()))
            .collect::<Vec<_>>();

        let merged = merge_parsed_modules(&parsed).unwrap();
        let lowered = lower_canonical_rir(&merged).unwrap();
        let rir = extract_import_directives(
            lowered.rir(),
            lowered.semantic_symbols().interner(),
            snapshot.metadata(),
        )
        .unwrap();
        let rir_values = rir
            .iter()
            .map(|directive| (directive.source_offset(), directive.specifier()))
            .collect::<Vec<_>>();

        assert_eq!(parsed_values, rir_values);
        assert_eq!(parsed_values.len(), 9);
        assert!(
            parsed_values
                .iter()
                .any(|(_, specifier)| *specifier == "body")
        );
    }

    #[test]
    fn input_order_and_file_ids_do_not_change_canonical_module_values() {
        let first = snapshot(
            &[
                (9, "/one/main.rue", "app/main.rue", "fn main() -> i32 { 0 }"),
                (2, "/one/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            9,
        );
        let moved = snapshot(
            &[
                (70, "/moved/helper.rue", "app/helper.rue", "fn helper() {}"),
                (
                    100,
                    "/moved/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { 0 }",
                ),
            ],
            100,
        );
        let first = parse_source_snapshot_modules(&first).unwrap();
        let moved = parse_source_snapshot_modules(&moved).unwrap();
        assert_eq!(first.root(), moved.root());
        assert_eq!(
            first
                .modules()
                .iter()
                .map(|module| module.revision())
                .collect::<Vec<_>>(),
            moved
                .modules()
                .iter()
                .map(|module| module.revision())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn reordered_broken_inputs_have_identical_canonical_diagnostics() {
        let entries = [
            (9, "/z.rue", "z.rue", "fn z( {"),
            (2, "/a.rue", "a.rue", "fn a() { let x = #; }"),
        ];
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect();
        let metadata = SourceMetadata::new(FileId::new(2), physical, logical).unwrap();
        let canonical_contents = vec![
            (FileId::new(2), Arc::new(entries[1].3.to_owned())),
            (FileId::new(9), Arc::new(entries[0].3.to_owned())),
        ];
        let mut reversed_contents = canonical_contents.clone();
        reversed_contents.reverse();
        let canonical = SourceSnapshot::new(metadata.clone(), canonical_contents).unwrap();
        let reversed = SourceSnapshot::new(metadata, reversed_contents).unwrap();

        let first = parse_source_snapshot_modules(&canonical).unwrap_err();
        let second = parse_source_snapshot_modules(&reversed).unwrap_err();
        assert_eq!(error_fingerprint(&first), error_fingerprint(&second));
    }

    #[test]
    fn parse_session_noop_reuses_exact_arcs_and_publishes_send_sync_result() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalParseSession>();
        assert_send_sync::<CanonicalParseUpdate>();
        assert_send_sync::<ParseInvalidationSummary>();
        assert_send_sync::<ParsedProgram>();

        let source = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let mut session = CanonicalParseSession::new();
        let first = session.update(&source).into_result().unwrap();
        let second_update = session.update(&source);
        let work = second_update.work();
        let second = second_update.into_result().unwrap();

        assert_eq!(work.previous_modules_indexed, 2);
        assert_eq!(work.previous_module_lookups, 2);
        assert_eq!(work.modules_reused, 2);
        assert_eq!(work.modules_reparsed, 0);
        assert_eq!(work.syntax.parser_invocations, 0);
        assert_eq!(work.source_text_clones, 0);
        assert_eq!(work.source_bytes_rehashed, 0);
        for (left, right) in first.modules().iter().zip(second.modules()) {
            assert!(Arc::ptr_eq(left, right));
        }

        let published = second.clone();
        let readers = (0..4)
            .map(|_| {
                let published = published.clone();
                std::thread::spawn(move || {
                    (
                        published.source_revision().clone(),
                        published.modules().len(),
                    )
                })
            })
            .collect::<Vec<_>>();
        for reader in readers {
            let (revision, len) = reader.join().unwrap();
            assert_eq!(&revision, published.source_revision());
            assert_eq!(len, 2);
        }
    }

    #[test]
    fn parse_session_one_edit_among_128_parses_once() {
        let make = |edited: bool| {
            let physical = (0..128)
                .map(|index| (FileId::new(index), format!("/p/m{index}.rue")))
                .collect();
            let logical = (0..128)
                .map(|index| (FileId::new(index), format!("m{index}.rue")))
                .collect();
            let metadata = SourceMetadata::new(FileId::new(0), physical, logical).unwrap();
            SourceSnapshot::new(
                metadata,
                (0..128)
                    .map(|index| {
                        let value = if edited && index == 73 { 2 } else { 1 };
                        (
                            FileId::new(index),
                            Arc::new(format!("fn f{index}() -> i32 {{ {value} }}")),
                        )
                    })
                    .collect(),
            )
            .unwrap()
        };
        let mut session = CanonicalParseSession::new();
        session.update(&make(false)).into_result().unwrap();
        let update = session.update(&make(true));
        let work = update.work();

        assert_eq!(work.previous_modules_indexed, 128);
        assert_eq!(work.previous_module_lookups, 128);
        assert_eq!(work.modules_reused, 127);
        assert_eq!(work.modules_reparsed, 1);
        assert_eq!(work.syntax.lexer_invocations, 1);
        assert_eq!(work.syntax.parser_invocations, 1);
        assert_eq!(update.invalidation().exact_reused.len(), 127);
        assert_eq!(update.invalidation().reparsed.len(), 1);
    }

    #[test]
    fn parse_session_distinguishes_relocation_file_ids_and_stable_renames() {
        let base = snapshot(
            &[
                (1, "/old/a.rue", "a.rue", "fn a() {}"),
                (2, "/old/b.rue", "b.rue", "fn b() {}"),
            ],
            1,
        );
        let relocated = snapshot(
            &[
                (1, "/new/a.rue", "a.rue", "fn a() {}"),
                (2, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            1,
        );
        let reassigned = snapshot(
            &[
                (11, "/new/a.rue", "a.rue", "fn a() {}"),
                (12, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            11,
        );
        let mut session = CanonicalParseSession::new();
        session.update(&base).into_result().unwrap();
        let moved = session.update(&relocated);
        assert_eq!(moved.work().modules_rebound, 2);
        assert_eq!(moved.invalidation().payload_rebound.len(), 2);
        moved.into_result().unwrap();
        let ids = session.update(&reassigned);
        assert_eq!(ids.work().modules_reparsed, 2);
        assert_eq!(ids.invalidation().reparsed.len(), 2);

        let renamed = snapshot(
            &[
                (11, "/new/a2.rue", "a2.rue", "fn a() {}"),
                (13, "/new/c.rue", "c.rue", "fn c() {}"),
            ],
            11,
        );
        ids.into_result().unwrap();
        let update = session.update(&renamed);
        assert_eq!(update.work().modules_rebound, 1);
        assert_eq!(update.work().modules_reparsed, 1);
        assert_eq!(update.invalidation().added.len(), 2);
        assert_eq!(update.invalidation().removed.len(), 2);
        assert!(update.invalidation().payload_rebound.is_empty());
    }

    #[test]
    fn failed_session_update_keeps_successful_baseline_for_recovery() {
        let good = snapshot(
            &[
                (1, "/p/a.rue", "a.rue", "fn a() {}"),
                (2, "/p/b.rue", "b.rue", "fn b() {}"),
                (3, "/p/c.rue", "c.rue", "fn c() {}"),
            ],
            1,
        );
        let broken = snapshot(
            &[
                (1, "/p/a.rue", "a.rue", "fn a() {}"),
                (2, "/p/b.rue", "b.rue", "fn b( {"),
                (3, "/p/c.rue", "c.rue", "fn c() {}"),
            ],
            1,
        );
        let recovered = snapshot(
            &[
                (1, "/p/a.rue", "a.rue", "fn a() {}"),
                (2, "/p/b.rue", "b.rue", "fn b() { let x = 1; }"),
                (3, "/p/c.rue", "c.rue", "fn c() {}"),
            ],
            1,
        );
        let mut session = CanonicalParseSession::new();
        let baseline = session.update(&good).into_result().unwrap();
        let failed = session.update(&broken);
        assert!(failed.result().is_err());
        assert!(!failed.baseline_advanced());
        assert_eq!(failed.work().modules_reused, 2);
        assert!(Arc::ptr_eq(session.baseline().unwrap(), &baseline));

        let recovered = session.update(&recovered);
        assert_eq!(recovered.work().modules_reused, 2);
        assert_eq!(recovered.work().modules_reparsed, 1);
        assert!(recovered.baseline_advanced());
        recovered.into_result().unwrap();
    }

    #[test]
    fn parse_session_rejects_foreign_baseline_and_keeps_canonical_error_order() {
        let first = snapshot(&[(1, "/a.rue", "a.rue", "fn a() {}")], 1);
        let foreign = snapshot(&[(9, "/z.rue", "z.rue", "fn z() {}")], 9);
        let parsed = Arc::new(parse_source_snapshot_modules(&first).unwrap());
        assert_eq!(
            CanonicalParseSession::from_baseline(&foreign, parsed)
                .unwrap_err()
                .to_string(),
            "invalid compiler input: parse-session baseline belongs to a foreign source revision"
        );

        let broken = snapshot(
            &[
                (9, "/z.rue", "z.rue", "fn z( {"),
                (2, "/a.rue", "a.rue", "fn a() { # }"),
            ],
            2,
        );
        let mut session = CanonicalParseSession::new();
        let update = session.update(&broken);
        let direct = parse_source_snapshot_modules(&broken).unwrap_err();
        assert_eq!(
            error_fingerprint(update.result().unwrap_err()),
            error_fingerprint(&direct)
        );
    }
}
