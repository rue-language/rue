//! Self-contained immutable parsed-module artifacts.
//!
//! This is the reuse-safe syntax boundary. Each module owns its parser symbol
//! universe, while [`ParsedProgram`] provides the sole parsed-program
//! representation used by semantic compilation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use lasso::Key;
use lasso::{RodeoResolver, Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileErrors, CompileResult, ErrorKind};
use rue_parser::{AssignTarget, Ast, Expr, IntrinsicArg, Item, TypeExpr, ast::Visibility};
use rue_span::{FileId, Span};
use sha2::{Digest, Sha256};

use crate::definition_snapshot::{definition_parts, validate_span};
use crate::retained_charge::RetainedCharge;
use crate::{
    DefinitionKind, DefinitionNamespace, ImportDirective, ImportDirectives, ModuleId,
    ModuleRevision, SourceId, SourceRevision, SourceSnapshot, SyntaxWork,
    declaration_candidate::{
        DeclarationCandidateCategory, DeclarationCandidateKey, DeclarationCandidateOwner,
        DeclarationOccurrenceCapability, DeclarationParameterHeader, DeclarationParameterMode,
        DeclarationShellFact, DeclarationShellFailure,
    },
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
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParsedDefinitionOccurrence(u32);

#[cfg(test)]
impl ParsedDefinitionOccurrence {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One presemantic definition candidate; duplicates remain distinct values.
#[derive(Debug, Clone)]
pub struct ParsedDefinitionCandidate {
    #[cfg(test)]
    occurrence: ParsedDefinitionOccurrence,
    namespace: DefinitionNamespace,
    kind: DefinitionKind,
    visibility: Option<Visibility>,
    name: Arc<str>,
    #[cfg(test)]
    symbol: ParsedSymbol,
    name_span: Span,
    declaration_span: Span,
}

impl ParsedDefinitionCandidate {
    #[cfg(test)]
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
    #[cfg(test)]
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
    declarations: Arc<[ParsedDeclarationCandidate]>,
    declaration_by_key: HashMap<DeclarationCandidateKey, usize>,
    rir_recipes: Arc<[ParsedRirRecipe]>,
    declaration_capabilities: Arc<[DeclarationOccurrenceCapability]>,
    #[cfg(test)]
    declaration_import_locator_materializations: Arc<AtomicUsize>,
    #[cfg(test)]
    by_name: BTreeMap<(DefinitionNamespace, Arc<str>), Arc<[ParsedDefinitionOccurrence]>>,
}

impl ParsedDefinitionIndex {
    pub fn candidates(&self) -> &[ParsedDefinitionCandidate] {
        &self.candidates
    }

    /// Deterministic charge for every owned allocation reachable from this
    /// cloned index. Shared allocations are deliberately charged along each
    /// retained path, matching the runtime retention policy.
    pub(crate) fn retained_allocation_charge(&self) -> u64 {
        let candidates =
            (self.candidates.len() * std::mem::size_of::<ParsedDefinitionCandidate>()) as u64;
        let candidates = self
            .candidates
            .iter()
            .fold(candidates, |charge, candidate| {
                let candidate_charge = candidate.name.retained_charge();
                #[cfg(test)]
                let candidate_charge =
                    candidate_charge.saturating_add(std::mem::size_of::<SymbolProvenance>() as u64);
                charge.saturating_add(candidate_charge)
            });
        let declarations =
            (self.declarations.len() * std::mem::size_of::<ParsedDeclarationCandidate>()) as u64;
        let declarations = self
            .declarations
            .iter()
            .fold(declarations, |charge, declaration| {
                let anonymous_sites = (declaration.anonymous_sites.len()
                    * std::mem::size_of::<rue_rir::AnonymousTypeSite>())
                    as u64;
                charge
                    .saturating_add(declaration.fact.retained_charge())
                    .saturating_add(declaration.warning_call_heads.iter().fold(
                        (declaration.warning_call_heads.len()
                            * std::mem::size_of::<ParsedWarningCallHead>())
                            as u64,
                        |charge, head| {
                            let import = head
                                .import
                                .as_ref()
                                .map_or(0, |import| import.specifier.retained_charge());
                            head.components.iter().fold(
                                charge.saturating_add(import).saturating_add(
                                    (head.components.len() * std::mem::size_of::<Arc<str>>())
                                        as u64,
                                ),
                                |charge, component| {
                                    charge.saturating_add(component.retained_charge())
                                },
                            )
                        },
                    ))
                    .saturating_add(
                        declaration
                            .anonymous_sites
                            .iter()
                            .fold(anonymous_sites, |charge, site| {
                                charge.saturating_add(site.anchor.retained_charge())
                            }),
                    )
            });
        let declaration_by_key = self.declaration_by_key.iter().fold(
            (self.declaration_by_key.len()
                * std::mem::size_of::<(DeclarationCandidateKey, usize)>()) as u64,
            |charge, (key, _)| charge.saturating_add(key.retained_charge()),
        );
        let rir_recipes = self.rir_recipes.iter().fold(
            (self.rir_recipes.len() * std::mem::size_of::<ParsedRirRecipe>()) as u64,
            |charge, recipe| match recipe {
                ParsedRirRecipe::Single(key) => charge.saturating_add(key.retained_charge()),
                ParsedRirRecipe::Struct { shell, methods } => methods.iter().fold(
                    charge
                        .saturating_add(shell.retained_charge())
                        .saturating_add(
                            (methods.len() * std::mem::size_of::<DeclarationCandidateKey>()) as u64,
                        ),
                    |charge, key| charge.saturating_add(key.retained_charge()),
                ),
                ParsedRirRecipe::Extern { functions } => functions.iter().fold(
                    charge.saturating_add(
                        (functions.len() * std::mem::size_of::<DeclarationCandidateKey>()) as u64,
                    ),
                    |charge, key| charge.saturating_add(key.retained_charge()),
                ),
            },
        );
        let declaration_capabilities = (self.declaration_capabilities.len()
            * std::mem::size_of::<DeclarationOccurrenceCapability>())
            as u64;
        let declaration_capabilities = self
            .declaration_capabilities
            .iter()
            .fold(declaration_capabilities, |charge, capability| {
                charge.saturating_add(capability.retained_charge())
            });
        let charge = candidates
            .saturating_add(declarations)
            .saturating_add(declaration_by_key)
            .saturating_add(rir_recipes)
            .saturating_add(declaration_capabilities);
        #[cfg(test)]
        let charge = {
            let atomics = 2_u64.saturating_mul(std::mem::size_of::<AtomicUsize>() as u64);
            let by_name = self.by_name.iter().fold(
                (self.by_name.len()
                    * std::mem::size_of::<(
                        (DefinitionNamespace, Arc<str>),
                        Arc<[ParsedDefinitionOccurrence]>,
                    )>()) as u64,
                |charge, ((_, name), occurrences)| {
                    charge
                        .saturating_add(name.retained_charge())
                        .saturating_add(
                            (occurrences.len() * std::mem::size_of::<ParsedDefinitionOccurrence>())
                                as u64,
                        )
                },
            );
            charge.saturating_add(atomics).saturating_add(by_name)
        };
        charge
    }

    pub(crate) fn declaration_capabilities(&self) -> &[DeclarationOccurrenceCapability] {
        &self.declaration_capabilities
    }

    pub(crate) fn declaration_keys_in_source_order(
        &self,
    ) -> impl ExactSizeIterator<Item = &DeclarationCandidateKey> {
        self.declarations
            .iter()
            .map(|candidate| &candidate.fact.key)
    }

    pub(crate) fn rir_recipes(&self) -> &[ParsedRirRecipe] {
        &self.rir_recipes
    }

    pub(crate) fn evaluate_declaration_shell(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Result<DeclarationShellFact, DeclarationShellFailure> {
        let Some(index) = self.declaration_by_key.get(key).copied() else {
            return Err(DeclarationShellFailure::Absent(key.clone()));
        };
        let candidate = self
            .declarations
            .get(index)
            .ok_or_else(|| DeclarationShellFailure::ParserCapabilityMismatch(key.clone()))?;
        if candidate.fact.key != *key {
            return Err(DeclarationShellFailure::ParserCapabilityMismatch(
                key.clone(),
            ));
        }
        Ok(candidate.fact.clone())
    }

    pub(crate) fn producer_fragment_span(&self, key: &DeclarationCandidateKey) -> Option<Span> {
        let index = self.declaration_by_key.get(key).copied()?;
        let candidate = self.declarations.get(index)?;
        if candidate.fact.key != *key {
            return None;
        }
        candidate.raw_body_span.or(candidate.const_initializer_span)
    }

    /// Every method one owner declares in this module — its name, whether it
    /// is itself a `-> borrow` accessor, and its `self`-call targets — in the
    /// normalized form the canonical parsed signature projection consumes.
    fn owner_method_accessor_facts(
        &self,
        owner: &crate::declaration_candidate::DeclarationCandidateOwner,
    ) -> Arc<[rue_air::declaration_validation::AccessorOwnerMethod]> {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        crate::semantic_query_nucleus::owner_method_accessor_facts(
            self.declarations
                .iter()
                .filter(|candidate| {
                    candidate.fact.key.category == Category::Method
                        && candidate.fact.key.owner.as_ref() == Some(owner)
                })
                .map(
                    |candidate| rue_air::declaration_validation::AccessorOwnerMethod {
                        name: candidate.fact.key.name.clone(),
                        is_accessor: candidate.is_accessor,
                        self_call_targets: candidate.self_call_targets.clone(),
                    },
                ),
        )
    }

    fn body_source_spans(&self, key: &DeclarationCandidateKey) -> Option<(Span, Span)> {
        let index = self.declaration_by_key.get(key).copied()?;
        let candidate = self.declarations.get(index)?;
        (candidate.fact.key == *key)
            .then_some((candidate.declaration_span, candidate.raw_body_span?))
    }

    fn declaration_import_range(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<RawDeclarationImportRange> {
        let index = self.declaration_by_key.get(key).copied()?;
        let candidate = self.declarations.get(index)?;
        (candidate.fact.key == *key)
            .then_some(candidate.raw_import_range)
            .flatten()
    }

    #[cfg(test)]
    fn declaration_import_locator_materialization_count(&self) -> usize {
        self.declaration_import_locator_materializations
            .load(Ordering::Relaxed)
    }

    pub(crate) fn declaration_locator(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<ParsedDeclarationLocator> {
        let index = self.declaration_by_key.get(key).copied()?;
        let candidate = self.declarations.get(index)?;
        let source_order = u32::try_from(index).ok()?;
        (candidate.fact.key == *key).then_some(ParsedDeclarationLocator {
            declaration_span: candidate.declaration_span,
            source_order,
        })
    }

    pub(crate) fn declaration_warning_call_heads(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<&Arc<[ParsedWarningCallHead]>> {
        let index = self.declaration_by_key.get(key).copied()?;
        let candidate = self.declarations.get(index)?;
        (candidate.fact.key == *key).then_some(&candidate.warning_call_heads)
    }

    #[cfg(test)]
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

/// One canonical source occurrence paired with its current locator.
#[derive(Debug, Clone)]
pub(crate) struct ParsedDeclarationCandidate {
    fact: DeclarationShellFact,
    ast_locator: ParsedDeclarationAstLocator,
    declaration_span: Span,
    const_initializer_span: Option<Span>,
    raw_body_span: Option<Span>,
    is_accessor: bool,
    /// The method names this declaration's body calls on its own `self`
    /// receiver, normalized. Non-empty only for methods; the accessor
    /// signature terminal retains it as a sibling's 6.6:14 cycle edge
    /// (RUE-1282).
    self_call_targets: Arc<[Arc<str>]>,
    raw_import_range: Option<RawDeclarationImportRange>,
    /// Static call/type-constructor heads projected during the canonical
    /// module syntax walk. Names are already stripped of lexical locals and
    /// aliases; import references use declaration-local occurrence ordinals.
    warning_call_heads: Arc<[ParsedWarningCallHead]>,
    /// Value-position anonymous type literals inside this declaration's constant
    /// initializer or body, with module-relative spans and their frontend
    /// anchors. Sliced into fragment-relative sites only by the remaining
    /// test-only raw-body oracle (RUE-1089).
    anonymous_sites: Arc<[rue_rir::AnonymousTypeSite]>,
}

/// Stable parser-private route from a declaration candidate to its borrowed
/// syntax node. Ordinals are independent of FileId rebinding and are checked
/// against both the AST shape and the candidate category before use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ParsedDeclarationAstLocator {
    TopLevel { item: u32 },
    StructMethod { item: u32, method: u32 },
    ExternFunction { item: u32, function: u32 },
}

/// Parser-owned composition order for candidate RIR fragments. Struct method
/// fragments precede their shell so the shell can retain their exact roots;
/// extern members retain lexical order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedRirRecipe {
    Single(DeclarationCandidateKey),
    Struct {
        shell: DeclarationCandidateKey,
        methods: Arc<[DeclarationCandidateKey]>,
    },
    Extern {
        functions: Arc<[DeclarationCandidateKey]>,
    },
}

/// Exact borrowed syntax producer selected by a declaration key.
#[doc(hidden)]
pub enum ParsedDeclarationAstRef<'a> {
    Function(&'a rue_parser::ast::Function),
    Struct(&'a rue_parser::ast::StructDecl),
    Enum(&'a rue_parser::ast::EnumDecl),
    Const(&'a rue_parser::ast::ConstDecl),
    Destructor(&'a rue_parser::ast::DropFn),
    Method {
        owner: &'a rue_parser::ast::StructDecl,
        method: &'a rue_parser::ast::Method,
        ordinal: u32,
    },
    ExternFunction {
        function: &'a rue_parser::ast::ExternFn,
    },
}

/// Parser-private range into the module's source-ordered import table for one
/// declaration. Runtime query values retain neither field.
#[derive(Debug, Clone, Copy)]
struct RawDeclarationImportRange {
    start: u32,
    len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ParsedWarningCallHead {
    pub(crate) import: Option<ParsedWarningImport>,
    pub(crate) components: Arc<[Arc<str>]>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ParsedWarningImport {
    pub(crate) occurrence: u32,
    pub(crate) specifier: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawWarningCallHead {
    import_span: Option<Span>,
    components: Vec<Spur>,
}

impl Ord for RawWarningCallHead {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let span_key =
            |span: Option<Span>| span.map(|span| (span.file_id.index(), span.start, span.end));
        span_key(self.import_span)
            .cmp(&span_key(other.import_span))
            .then_with(|| {
                self.components
                    .iter()
                    .map(|spur| (*spur).into_usize())
                    .cmp(other.components.iter().map(|spur| (*spur).into_usize()))
            })
    }
}

impl PartialOrd for RawWarningCallHead {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedDeclarationImportFailure {
    SiteOutOfRange { available: u32 },
    SpecifierMismatch { actual: Arc<str> },
    CapabilityMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedDeclarationLocator {
    pub(crate) declaration_span: Span,
    pub(crate) source_order: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedInvalidImport {
    importer: ModuleId,
    span: Span,
    shape: InvalidImportShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InvalidImportShape {
    WrongArity { actual: u32 },
    NonStringArgument,
}

impl ParsedInvalidImport {
    pub(crate) fn retained_allocation_charge(&self) -> u64 {
        self.importer.retained_charge()
    }

    pub fn span(&self) -> Span {
        self.span
    }
    pub fn shape(&self) -> &InvalidImportShape {
        &self.shape
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedInvalidImportSite {
    span: Span,
    shape: InvalidImportShape,
}

#[derive(Default)]
struct ImportSiteCollector {
    valid: Vec<ImportDirective>,
    invalid: Vec<ParsedInvalidImportSite>,
}

#[derive(Default)]
struct ParsedModuleProjectionCollector {
    imports: ImportSiteCollector,
    warning_call_heads: HashMap<ParsedDeclarationAstLocator, Vec<RawWarningCallHead>>,
}

/// Immutable parsed syntax whose spans and symbols belong to one FileId epoch.
#[derive(Debug)]
struct ParsedSyntaxPayload {
    source: SourceId,
    source_text: Arc<String>,
    token_count: usize,
    tokens: Arc<[rue_lexer::Token]>,
    ast: ProvenancedAst,
    resolver: FrozenSymbolResolver,
    definitions: ParsedDefinitionIndex,
    import_sites: Arc<[ImportDirective]>,
    invalid_import_sites: Arc<[ParsedInvalidImportSite]>,
}

/// Immutable, Arc-shareable parsed syntax and exact local provenance.
#[derive(Debug)]
pub struct ParsedModule {
    /// Memoized retained-artifact charge of this exact module value; see
    /// [`Self::retained_allocation_charge`].
    retained_charge: std::sync::OnceLock<u64>,
    revision: ModuleRevision,
    file_id: FileId,
    physical_path: Arc<str>,
    payload: Arc<ParsedSyntaxPayload>,
    tokens: Arc<[rue_lexer::Token]>,
    ast: Arc<Ast>,
    definitions: ParsedDefinitionIndex,
    imports: Arc<[ImportDirective]>,
    invalid_imports: Arc<[ParsedInvalidImport]>,
}

/// An AST paired with the exact parsed module that owns all of its symbols.
///
/// Views are issued only by [`ParsedProgram`]; cloning a view retains the
/// pointer-identical parsed module rather than copying its AST payload.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct ParsedAstView {
    module: Arc<ParsedModule>,
}

#[cfg(test)]
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

    #[cfg(test)]
    pub fn items(&self) -> impl ExactSizeIterator<Item = ParsedItemView> + '_ {
        (0..self.module.ast().items.len()).map(|_| ParsedItemView {
            module: self.module.clone(),
        })
    }
}

/// Test-only proof that item traversal retains the exact parsed owner.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct ParsedItemView {
    module: Arc<ParsedModule>,
}

#[cfg(test)]
impl ParsedItemView {
    pub fn module(&self) -> &Arc<ParsedModule> {
        &self.module
    }

    pub fn module_id(&self) -> &ModuleId {
        self.module.module_id()
    }
}

impl ParsedModule {
    /// Charge the complete retained graph, including the immutable parser
    /// payload and the snapshot-local aliases derived from it. Aliased Arcs are
    /// charged once per reachable field rather than deduplicated by address.
    ///
    /// A module is immutable, so its charge is computed once per value and
    /// memoized: the program a discovery round publishes is charged whole on
    /// every publication and every retention measurement, and re-walking each
    /// carried module's syntax on each of those is quadratic in the length of an
    /// import chain. The memo answers with the number the walk produces.
    pub(crate) fn retained_allocation_charge(&self) -> u64 {
        *self
            .retained_charge
            .get_or_init(|| self.walk_retained_allocation_charge())
    }

    fn walk_retained_allocation_charge(&self) -> u64 {
        let ast_charge = |ast: &Arc<Ast>| ast.retained_charge();
        let tokens_charge = |tokens: &[rue_lexer::Token]| std::mem::size_of_val(tokens) as u64;
        let imports_charge = |imports: &[ImportDirective]| {
            imports
                .iter()
                .fold(std::mem::size_of_val(imports) as u64, |charge, import| {
                    charge.saturating_add(import.retained_charge())
                })
        };

        let payload = (std::mem::size_of::<ParsedSyntaxPayload>() as u64)
            .saturating_add(self.payload.source.retained_charge())
            .saturating_add(self.payload.source_text.retained_charge())
            .saturating_add(tokens_charge(&self.payload.tokens))
            .saturating_add(ast_charge(&self.payload.ast.ast))
            .saturating_add(std::mem::size_of::<SymbolProvenance>() as u64)
            .saturating_add(self.payload.ast.source.retained_charge())
            .saturating_add(std::mem::size_of::<RodeoResolver<Spur>>() as u64)
            .saturating_add(
                (self.payload.resolver.resolver.len() * std::mem::size_of::<Spur>()) as u64,
            )
            .saturating_add(
                self.payload
                    .resolver
                    .resolver
                    .iter()
                    .fold(0_u64, |charge, (_, value)| {
                        charge.saturating_add(value.len() as u64)
                    }),
            )
            .saturating_add(std::mem::size_of::<SymbolProvenance>() as u64)
            .saturating_add(self.payload.definitions.retained_allocation_charge())
            .saturating_add(imports_charge(&self.payload.import_sites))
            .saturating_add(
                (self.payload.invalid_import_sites.len()
                    * std::mem::size_of::<ParsedInvalidImportSite>()) as u64,
            );
        let invalid_imports = self.invalid_imports.iter().fold(
            (self.invalid_imports.len() * std::mem::size_of::<ParsedInvalidImport>()) as u64,
            |charge, invalid| charge.saturating_add(invalid.retained_allocation_charge()),
        );

        self.revision
            .retained_charge()
            .saturating_add(self.physical_path.retained_charge())
            .saturating_add(payload)
            .saturating_add(tokens_charge(&self.tokens))
            .saturating_add(ast_charge(&self.ast))
            .saturating_add(self.definitions.retained_allocation_charge())
            .saturating_add(imports_charge(&self.imports))
            .saturating_add(invalid_imports)
    }

    pub(crate) fn body_source_spans(&self, key: &DeclarationCandidateKey) -> Option<(Span, Span)> {
        self.definitions.body_source_spans(key)
    }

    /// Exact parser-indexed anonymous type sites for one declaration producer.
    /// The shared site walker intentionally excludes methods nested inside an
    /// anonymous type expression: AstGen enters each such method as its own
    /// semantic producer and derives that nested producer's table separately.
    pub(crate) fn declaration_anonymous_sites(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<&[rue_rir::AnonymousTypeSite]> {
        let index = self.definitions.declaration_by_key.get(key).copied()?;
        let candidate = self.definitions.declarations.get(index)?;
        (candidate.fact.key == *key).then_some(candidate.anonymous_sites.as_ref())
    }

    pub(crate) fn declaration_warning_call_heads(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<&Arc<[ParsedWarningCallHead]>> {
        self.definitions.declaration_warning_call_heads(key)
    }

    /// Exact parsed sibling facts needed to validate one accessor signature.
    /// The keyed declaration lookup and owner join are both checked here so a
    /// signature projection cannot silently borrow a neighboring owner's
    /// method set after an incremental update.
    pub(crate) fn declaration_accessor_owner_methods(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<Arc<[rue_air::declaration_validation::AccessorOwnerMethod]>> {
        let index = self.definitions.declaration_by_key.get(key).copied()?;
        let candidate = self.definitions.declarations.get(index)?;
        if candidate.fact.key != *key {
            return None;
        }
        if !candidate.is_accessor {
            return Some(Arc::from([]));
        }
        Some(key.owner.as_ref().map_or_else(
            || Arc::from([]),
            |owner| self.definitions.owner_method_accessor_facts(owner),
        ))
    }

    /// Resolve an exact declaration key to the borrowed parser node that owns
    /// its syntax. Every ordinal and variant is checked so corrupted or stale
    /// locators fail closed instead of selecting a neighboring declaration.
    #[doc(hidden)]
    pub fn declaration_ast(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<ParsedDeclarationAstRef<'_>> {
        let index = self.definitions.declaration_by_key.get(key).copied()?;
        let candidate = self.definitions.declarations.get(index)?;
        if candidate.fact.key != *key {
            return None;
        }
        let item = |ordinal: u32| self.ast.items.get(usize::try_from(ordinal).ok()?);
        let name_matches =
            |symbol: Spur, expected: &str| self.try_resolve_raw_symbol(symbol) == Some(expected);
        let span_matches = |span: Span| span == candidate.declaration_span;
        match (candidate.ast_locator, key.category) {
            (
                ParsedDeclarationAstLocator::TopLevel { item: ordinal },
                DeclarationCandidateCategory::Function,
            ) => match item(ordinal)? {
                Item::Function(value)
                    if span_matches(value.span)
                        && name_matches(value.name.name, key.name.as_ref())
                        && key.owner.is_none() =>
                {
                    Some(ParsedDeclarationAstRef::Function(value))
                }
                _ => None,
            },
            (
                ParsedDeclarationAstLocator::TopLevel { item: ordinal },
                DeclarationCandidateCategory::Struct,
            ) => match item(ordinal)? {
                Item::Struct(value)
                    if span_matches(value.span)
                        && name_matches(value.name.name, key.name.as_ref())
                        && key.owner.is_none() =>
                {
                    Some(ParsedDeclarationAstRef::Struct(value))
                }
                _ => None,
            },
            (
                ParsedDeclarationAstLocator::TopLevel { item: ordinal },
                DeclarationCandidateCategory::Enum,
            ) => match item(ordinal)? {
                Item::Enum(value)
                    if span_matches(value.span)
                        && name_matches(value.name.name, key.name.as_ref())
                        && key.owner.is_none() =>
                {
                    Some(ParsedDeclarationAstRef::Enum(value))
                }
                _ => None,
            },
            (
                ParsedDeclarationAstLocator::TopLevel { item: ordinal },
                DeclarationCandidateCategory::ConstCandidate,
            ) => match item(ordinal)? {
                Item::Const(value)
                    if span_matches(value.span)
                        && name_matches(value.name.name, key.name.as_ref())
                        && key.owner.is_none() =>
                {
                    Some(ParsedDeclarationAstRef::Const(value))
                }
                _ => None,
            },
            (
                ParsedDeclarationAstLocator::TopLevel { item: ordinal },
                DeclarationCandidateCategory::Destructor,
            ) => match item(ordinal)? {
                Item::DropFn(value)
                    if span_matches(value.span)
                        && name_matches(value.type_name.name, key.name.as_ref())
                        && key.owner.as_ref().is_some_and(|owner| {
                            owner.category == DeclarationCandidateCategory::Struct
                                && owner.name == key.name
                        }) =>
                {
                    Some(ParsedDeclarationAstRef::Destructor(value))
                }
                _ => None,
            },
            (
                ParsedDeclarationAstLocator::StructMethod {
                    item: ordinal,
                    method: method_ordinal,
                },
                DeclarationCandidateCategory::Method
                | DeclarationCandidateCategory::AssociatedFunction,
            ) => {
                let Item::Struct(owner) = item(ordinal)? else {
                    return None;
                };
                let method = owner.methods.get(usize::try_from(method_ordinal).ok()?)?;
                let owner_key = key.owner.as_ref()?;
                if !span_matches(method.span)
                    || !name_matches(method.name.name, key.name.as_ref())
                    || owner_key.category != DeclarationCandidateCategory::Struct
                    || !name_matches(owner.name.name, owner_key.name.as_ref())
                    || (key.category == DeclarationCandidateCategory::Method)
                        != method.receiver.is_some()
                {
                    return None;
                }
                Some(ParsedDeclarationAstRef::Method {
                    owner,
                    method,
                    ordinal: method_ordinal,
                })
            }
            (
                ParsedDeclarationAstLocator::ExternFunction {
                    item: ordinal,
                    function,
                },
                DeclarationCandidateCategory::ExternFunction,
            ) => {
                let Item::Extern(block) = item(ordinal)? else {
                    return None;
                };
                let function = block.fns.get(usize::try_from(function).ok()?)?;
                (span_matches(function.span)
                    && name_matches(function.name.name, key.name.as_ref())
                    && key.owner.is_none())
                .then_some(ParsedDeclarationAstRef::ExternFunction { function })
            }
            _ => None,
        }
    }

    pub(crate) fn declaration_import(
        &self,
        key: &crate::declaration_candidate::DeclarationImportSiteKey,
    ) -> Result<ImportDirective, ParsedDeclarationImportFailure> {
        let range = self
            .definitions
            .declaration_import_range(&key.declaration)
            .ok_or(ParsedDeclarationImportFailure::CapabilityMismatch)?;
        if key.occurrence >= range.len {
            return Err(ParsedDeclarationImportFailure::SiteOutOfRange {
                available: range.len,
            });
        }
        let index = range
            .start
            .checked_add(key.occurrence)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(ParsedDeclarationImportFailure::CapabilityMismatch)?;
        let site = self
            .imports
            .get(index)
            .ok_or(ParsedDeclarationImportFailure::CapabilityMismatch)?;
        let Some(locator) = self.definitions.declaration_locator(&key.declaration) else {
            return Err(ParsedDeclarationImportFailure::CapabilityMismatch);
        };
        if site.importer() != self.module_id()
            || site.source_offset() < locator.declaration_span.start
            || site.source_end() > locator.declaration_span.end
        {
            return Err(ParsedDeclarationImportFailure::CapabilityMismatch);
        }
        if site.specifier() != key.specifier.as_ref() {
            return Err(ParsedDeclarationImportFailure::SpecifierMismatch {
                actual: Arc::from(site.specifier()),
            });
        }
        #[cfg(test)]
        self.definitions
            .declaration_import_locator_materializations
            .fetch_add(1, Ordering::Relaxed);
        Ok(site.clone())
    }

    #[cfg(test)]
    pub(crate) fn declaration_import_locator_materialization_count(&self) -> usize {
        self.definitions
            .declaration_import_locator_materialization_count()
    }

    #[cfg(test)]
    pub(crate) fn with_test_foreign_ast_symbol(&self) -> Arc<Self> {
        let mut ast = (*self.ast).clone();
        let foreign = Spur::try_from_usize(self.payload.resolver.resolver.len() + 17)
            .expect("test symbol ordinal remains representable");
        let Some(Item::Function(function)) = ast.items.first_mut() else {
            panic!("test fault requires a leading function")
        };
        function.name.name = foreign;
        Arc::new(Self {
            // A perturbed module is a distinct value; it charges itself.
            retained_charge: std::sync::OnceLock::new(),
            revision: self.revision.clone(),
            file_id: self.file_id,
            physical_path: self.physical_path.clone(),
            payload: self.payload.clone(),
            tokens: self.tokens.clone(),
            ast: Arc::new(ast),
            definitions: self.definitions.clone(),
            imports: self.imports.clone(),
            invalid_imports: self.invalid_imports.clone(),
        })
    }

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
        self.file_id
    }
    pub fn physical_path(&self) -> &str {
        &self.physical_path
    }
    pub fn source_text(&self) -> &str {
        &self.payload.source_text
    }
    pub(crate) fn token_count(&self) -> usize {
        self.payload.token_count
    }
    pub(crate) fn tokens(&self) -> &[rue_lexer::Token] {
        &self.tokens
    }
    pub(crate) fn resolve_raw_symbol(&self, symbol: Spur) -> &str {
        self.payload.resolver.resolver.resolve(&symbol)
    }
    pub(crate) fn try_resolve_raw_symbol(&self, symbol: Spur) -> Option<&str> {
        self.payload.resolver.resolver.try_resolve(&symbol)
    }
    #[cfg(test)]
    pub(crate) fn shared_source_text(&self) -> Arc<String> {
        self.payload.source_text.clone()
    }
    pub fn ast(&self) -> &Ast {
        &self.ast
    }
    pub fn definitions(&self) -> &ParsedDefinitionIndex {
        &self.definitions
    }
    pub fn imports(&self) -> &[ImportDirective] {
        &self.imports
    }
    pub fn invalid_imports(&self) -> &[ParsedInvalidImport] {
        &self.invalid_imports
    }

    #[cfg(test)]
    pub fn resolve(&self, symbol: &ParsedSymbol) -> CompileResult<&str> {
        self.payload.resolver.resolve(symbol)
    }

    #[cfg(test)]
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
    /// Snapshot modules classified by this assembly.
    pub modules_considered: usize,
    /// Point lookups performed against the retained per-module parse queries.
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

impl ParsedModulesWork {
    /// Add the work from another update in the same bounded parse lifecycle.
    pub(crate) fn accumulate(&mut self, other: Self) {
        self.syntax.lexer_invocations += other.syntax.lexer_invocations;
        self.syntax.parser_invocations += other.syntax.parser_invocations;
        self.syntax.lexed_bytes += other.syntax.lexed_bytes;
        self.syntax.tokens += other.syntax.tokens;
        self.modules_considered += other.modules_considered;
        self.previous_module_lookups += other.previous_module_lookups;
        self.modules_reused += other.modules_reused;
        self.modules_rebound += other.modules_rebound;
        self.modules_reparsed += other.modules_reparsed;
        self.source_text_clones += other.source_text_clones;
        self.source_bytes_rehashed += other.source_bytes_rehashed;
    }
}

/// Deterministically ordered collection of independently parsed modules. The
/// module table is held as bounded `Arc`-shared sorted tiers: a strictly-additive
/// successor retains an exact delta while compacting equal-magnitude tail tiers.
/// The whole-program merged views — the
/// contiguous module slice and the merged import directives — are projections
/// that materialize lazily, never on the successor staging path.
#[derive(Debug, Clone)]
pub struct ParsedProgram {
    source_revision: SourceRevision,
    modules: crate::shared_segments::SharedSegments<Arc<ParsedModule>>,
    imports: ImportDirectives,
    invalid_imports: Arc<[ParsedInvalidImport]>,
}

fn parsed_module_order(a: &Arc<ParsedModule>, b: &Arc<ParsedModule>) -> std::cmp::Ordering {
    a.module_id().cmp(b.module_id())
}

fn sort_invalid_imports(invalid_imports: &mut [ParsedInvalidImport]) {
    invalid_imports.sort_by(|left, right| {
        left.importer
            .cmp(&right.importer)
            .then(left.span.file_id.index().cmp(&right.span.file_id.index()))
            .then(left.span.start.cmp(&right.span.start))
    });
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
                .cloned()
                .collect(),
        );
        let mut invalid_imports = modules
            .iter()
            .flat_map(|module| module.invalid_imports().iter().cloned())
            .collect::<Vec<_>>();
        sort_invalid_imports(&mut invalid_imports);
        Ok(Self {
            source_revision,
            modules: crate::shared_segments::SharedSegments::flat(
                modules.into(),
                parsed_module_order,
            ),
            imports,
            invalid_imports: invalid_imports.into(),
        })
    }

    /// A strictly-additive successor program: every predecessor module, import
    /// directive, and source-revision segment is shared by reference; only the
    /// newly appended modules are added. `source_revision` is the successor
    /// snapshot's already-extended revision — the identity the appended leaves
    /// were published under — never re-derived by enumerating modules.
    pub(crate) fn extend_successor(
        predecessor: &ParsedProgram,
        source_revision: SourceRevision,
        mut delta: Vec<Arc<ParsedModule>>,
    ) -> CompileResult<Self> {
        delta.sort_by(parsed_module_order);
        let predecessor_len = predecessor.modules.len();
        for module in &delta {
            if predecessor.module(module.module_id()).is_some() {
                return Err(invalid_input(format!(
                    "successor parse delta re-declares retained module {}",
                    module.module_id()
                )));
            }
            // Appended sources extend the predecessor's dense file table, so a
            // delta file ID inside the retained range is a construction error.
            if (module.file_id().index() as usize) <= predecessor_len {
                return Err(invalid_input(format!(
                    "successor parse delta reuses retained file ID {} for module {}",
                    module.file_id().index(),
                    module.module_id()
                )));
            }
        }
        if source_revision.module_segments().len() != predecessor_len + delta.len() {
            return Err(invalid_input(
                "successor parse delta does not reconcile with its extended source revision",
            ));
        }
        let imports = ImportDirectives::extend(
            &predecessor.imports,
            delta
                .iter()
                .flat_map(|module| module.imports().iter())
                .cloned()
                .collect(),
        );
        // A committed predecessor parsed cleanly, so this concatenation copies
        // only diagnostics-bearing records (empty in the additive flow).
        let mut invalid_imports = predecessor
            .invalid_imports
            .iter()
            .cloned()
            .chain(
                delta
                    .iter()
                    .flat_map(|module| module.invalid_imports().iter().cloned()),
            )
            .collect::<Vec<_>>();
        sort_invalid_imports(&mut invalid_imports);
        Ok(Self {
            source_revision,
            modules: crate::shared_segments::SharedSegments::extend(&predecessor.modules, delta),
            imports,
            invalid_imports: invalid_imports.into(),
        })
    }

    pub fn root(&self) -> &ModuleId {
        self.source_revision.root()
    }
    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }

    /// The contiguous merged module table — a lazily materialized projection
    /// (at most once per value). Paths that only iterate or look up single
    /// modules use [`Self::modules_iter`] / [`Self::module`] instead.
    pub fn modules(&self) -> &[Arc<ParsedModule>] {
        self.modules.as_slice()
    }

    /// Stream the modules in canonical logical order without materializing the
    /// merged table.
    pub(crate) fn modules_iter(&self) -> impl ExactSizeIterator<Item = &Arc<ParsedModule>> {
        self.modules.iter()
    }

    pub(crate) fn modules_len(&self) -> usize {
        self.modules.len()
    }

    /// Look up a module by its stable logical identity (per-segment binary
    /// search; never materializes the merged table).
    pub fn module(&self, id: &ModuleId) -> Option<&Arc<ParsedModule>> {
        self.modules.find_by(|module| module.module_id().cmp(id))
    }

    /// Traverse module-qualified ASTs in canonical logical-module order.
    #[cfg(test)]
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
    pub fn invalid_imports(&self) -> &[ParsedInvalidImport] {
        &self.invalid_imports
    }

    /// Token volume of this immutable program, including one EOF token per module.
    pub(crate) fn token_count(&self) -> usize {
        self.modules.iter().map(|module| module.token_count()).sum()
    }

    pub(crate) fn belongs_to_exact_snapshot(&self, snapshot: &SourceSnapshot) -> bool {
        self.source_revision() == snapshot.source_revision()
            && self.modules.len() == snapshot.len()
            && self.modules.iter().all(|module| {
                let file_id = module.file_id();
                snapshot.module_id(file_id) == Some(module.module_id())
                    && snapshot.source_id(file_id) == Some(module.source_id())
                    && snapshot.metadata().physical_path(file_id) == Some(module.physical_path())
                    && snapshot.source_text(file_id) == Some(module.source_text())
            })
    }

    #[cfg(test)]
    pub(crate) fn shared_symbol_strings(&self) -> Option<Vec<&str>> {
        let first = self.modules.iter().next()?;
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

/// Invalidation classification for a strictly-additive trusted successor
/// (RUE-1112): relative to its committed predecessor exactly the appended
/// modules are added — nothing is removed, rebound, or reparsed — so only the
/// delta is examined; retained modules are never enumerated or compared.
pub(crate) fn classify_successor_invalidation(delta: &[ModuleId]) -> ParseInvalidationSummary {
    let mut added = delta.to_vec();
    added.sort();
    ParseInvalidationSummary {
        added,
        ..Default::default()
    }
}

pub(crate) fn classify_invalidation(
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

/// Parse one snapshot through the canonical session parse query.
///
/// Tests that only need a parsed program for a snapshot get it the way
/// production does: a fresh [`CompilerSession`](crate::CompilerSession) update.
/// There is no second whole-program assembly to keep in agreement.
#[cfg(test)]
pub(crate) fn parse_source_snapshot_modules(
    snapshot: &SourceSnapshot,
) -> Result<Arc<ParsedProgram>, CompileErrors> {
    crate::CompilerSession::new()
        .update(snapshot)
        .into_owner_result()
}

pub(crate) fn parse_source_snapshot_module(
    snapshot: &SourceSnapshot,
    module: &ModuleId,
) -> (Result<Arc<ParsedModule>, CompileErrors>, SyntaxWork) {
    let file_id = snapshot
        .metadata()
        .file_ids()
        .find(|file_id| snapshot.module_id(*file_id) == Some(module))
        .ok_or_else(|| {
            CompileErrors::from(invalid_input(format!(
                "source snapshot contains no module {module}"
            )))
        });
    match file_id {
        Ok(file_id) => parse_snapshot_file(snapshot, file_id),
        Err(errors) => (Err(errors), SyntaxWork::default()),
    }
}

pub(crate) fn rebind_parsed_module(
    snapshot: &SourceSnapshot,
    module: &Arc<ParsedModule>,
) -> Arc<ParsedModule> {
    let file_id = snapshot
        .metadata()
        .file_ids()
        .find(|file_id| snapshot.module_id(*file_id) == Some(module.module_id()))
        .expect("parsed module belongs to the projected source snapshot");
    if module.file_id() == file_id
        && module.physical_path()
            == snapshot
                .metadata()
                .physical_path(file_id)
                .expect("source metadata retains every physical path")
    {
        module.clone()
    } else {
        bind_payload(snapshot, file_id, module.payload.clone())
    }
}

fn parse_snapshot_file(
    snapshot: &SourceSnapshot,
    file_id: FileId,
) -> (Result<Arc<ParsedModule>, CompileErrors>, SyntaxWork) {
    let source = snapshot.source(file_id).expect("metadata membership");
    let outcome = crate::syntax::parse_file(source, ThreadedRodeo::new());
    let work = outcome.work;
    let tokens = outcome.tokens;
    let result = outcome.result.and_then(|ast| {
        build_module(
            snapshot,
            file_id,
            ast,
            outcome.interner,
            work.tokens,
            tokens,
        )
        .map_err(CompileErrors::from)
    });
    (result, work)
}

fn build_module(
    snapshot: &SourceSnapshot,
    file_id: FileId,
    ast: Arc<Ast>,
    interner: ThreadedRodeo,
    token_count: usize,
    tokens: Arc<[rue_lexer::Token]>,
) -> CompileResult<Arc<ParsedModule>> {
    let token = Arc::new(SymbolProvenance);
    let module = snapshot.module_id(file_id).expect("snapshot membership");
    let projections = collect_module_projections(&ast, module, &interner)?;
    let resolver = Arc::new(interner.into_resolver());
    build_module_with_resolver(
        snapshot,
        file_id,
        ast,
        resolver,
        token,
        projections,
        token_count,
        tokens,
    )
}

fn build_module_with_resolver(
    snapshot: &SourceSnapshot,
    file_id: FileId,
    ast: Arc<Ast>,
    resolver: Arc<RodeoResolver<Spur>>,
    token: Arc<SymbolProvenance>,
    projections: ParsedModuleProjectionCollector,
    token_count: usize,
    tokens: Arc<[rue_lexer::Token]>,
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
    let definitions = build_definition_index(
        revision.module.clone(),
        file_id,
        &source_text,
        &tokens,
        &provenanced_ast.ast,
        &resolver,
        &projections.imports.valid,
        &projections.warning_call_heads,
    )?;
    let payload = Arc::new(ParsedSyntaxPayload {
        source,
        source_text,
        token_count,
        tokens,
        ast: provenanced_ast,
        resolver,
        definitions,
        import_sites: projections.imports.valid.into(),
        invalid_import_sites: projections.imports.invalid.into(),
    });
    Ok(bind_payload(snapshot, file_id, payload))
}

fn bind_payload(
    snapshot: &SourceSnapshot,
    file_id: FileId,
    payload: Arc<ParsedSyntaxPayload>,
) -> Arc<ParsedModule> {
    debug_assert_eq!(snapshot.source_id(file_id), Some(&payload.source));
    let module = snapshot
        .module_id(file_id)
        .expect("snapshot membership")
        .clone();
    let revision = ModuleRevision {
        module: module.clone(),
        source: payload.source.clone(),
    };
    let remap_span = |span: Span| Span::with_file(file_id, span.start, span.end);
    let payload_file_id = payload
        .tokens
        .first()
        .expect("the lexer always emits EOF")
        .span
        .file_id;
    let (tokens, ast) = if payload_file_id == file_id {
        (payload.tokens.clone(), payload.ast.ast.clone())
    } else {
        let tokens = payload
            .tokens
            .iter()
            .cloned()
            .map(|mut token| {
                token.span = remap_span(token.span);
                token
            })
            .collect::<Vec<_>>()
            .into();
        let mut ast = (*payload.ast.ast).clone();
        ast.rebind_file_id(file_id);
        (tokens, Arc::new(ast))
    };
    let definitions = ParsedDefinitionIndex {
        candidates: payload
            .definitions
            .candidates
            .iter()
            .cloned()
            .map(|candidate| ParsedDefinitionCandidate {
                name_span: remap_span(candidate.name_span),
                declaration_span: remap_span(candidate.declaration_span),
                ..candidate
            })
            .collect::<Vec<_>>()
            .into(),
        declarations: payload
            .definitions
            .declarations
            .iter()
            .cloned()
            .map(|candidate| ParsedDeclarationCandidate {
                fact: candidate.fact,
                ast_locator: candidate.ast_locator,
                declaration_span: remap_span(candidate.declaration_span),
                const_initializer_span: candidate.const_initializer_span.map(remap_span),
                raw_body_span: candidate.raw_body_span.map(remap_span),
                is_accessor: candidate.is_accessor,
                self_call_targets: candidate.self_call_targets.clone(),
                raw_import_range: candidate.raw_import_range,
                warning_call_heads: candidate.warning_call_heads.clone(),
                anonymous_sites: candidate
                    .anonymous_sites
                    .iter()
                    .map(|site| rue_rir::AnonymousTypeSite {
                        span: remap_span(site.span),
                        kind: site.kind,
                        anchor: site.anchor.clone(),
                    })
                    .collect::<Vec<_>>()
                    .into(),
            })
            .collect::<Vec<_>>()
            .into(),
        declaration_by_key: payload.definitions.declaration_by_key.clone(),
        rir_recipes: payload.definitions.rir_recipes.clone(),
        declaration_capabilities: payload.definitions.declaration_capabilities.clone(),
        #[cfg(test)]
        declaration_import_locator_materializations: payload
            .definitions
            .declaration_import_locator_materializations
            .clone(),
        #[cfg(test)]
        by_name: payload.definitions.by_name.clone(),
    };
    let imports = payload
        .import_sites
        .iter()
        .map(|site| {
            ImportDirective::new(
                module.clone(),
                site.source_offset(),
                site.source_end(),
                Arc::from(site.specifier()),
            )
        })
        .collect::<Vec<_>>();
    let invalid_imports = payload
        .invalid_import_sites
        .iter()
        .map(|site| ParsedInvalidImport {
            importer: module.clone(),
            span: remap_span(site.span),
            shape: site.shape.clone(),
        })
        .collect::<Vec<_>>();
    Arc::new(ParsedModule {
        retained_charge: std::sync::OnceLock::new(),
        revision,
        file_id,
        physical_path: Arc::from(snapshot.metadata().physical_path(file_id).unwrap()),
        payload,
        tokens,
        ast,
        definitions,
        imports: imports.into(),
        invalid_imports: invalid_imports.into(),
    })
}

#[derive(Debug, Clone)]
struct ParsedWarningStaticPath {
    import_span: Option<Span>,
    components: Vec<Spur>,
}

impl ParsedWarningStaticPath {
    fn into_head(self) -> Option<RawWarningCallHead> {
        (!self.components.is_empty()).then_some(RawWarningCallHead {
            import_span: self.import_span,
            components: self.components,
        })
    }
}

#[derive(Debug, Clone)]
enum ParsedWarningLexicalBinding {
    Local,
    StaticAlias(ParsedWarningStaticPath),
}

/// One parser-owned syntax projection pass. It discovers module imports and
/// declaration-local warning call heads together, so unused-function warning
/// reachability never re-enters the AST with a peer scope/name walker.
struct ParsedBodyProjectionCollector<'a> {
    module: &'a ModuleId,
    resolver: &'a ThreadedRodeo,
    imports: &'a mut ImportSiteCollector,
    scopes: Vec<HashMap<Spur, ParsedWarningLexicalBinding>>,
    heads: Vec<RawWarningCallHead>,
}

impl<'a> ParsedBodyProjectionCollector<'a> {
    fn new(
        module: &'a ModuleId,
        resolver: &'a ThreadedRodeo,
        imports: &'a mut ImportSiteCollector,
    ) -> Self {
        Self {
            module,
            resolver,
            imports,
            scopes: Vec::new(),
            heads: Vec::new(),
        }
    }

    fn finish(mut self) -> Vec<RawWarningCallHead> {
        self.heads.sort();
        self.heads.dedup();
        self.heads
    }

    fn symbol(&self, ident: rue_parser::ast::Ident) -> CompileResult<Spur> {
        self.resolver
            .try_resolve(&ident.name)
            .map(|_| ident.name)
            .ok_or_else(|| invalid_input("AST symbol is absent from the module symbol universe"))
    }

    fn spelling(&self, ident: rue_parser::ast::Ident) -> CompileResult<&str> {
        self.resolver
            .try_resolve(&ident.name)
            .ok_or_else(|| invalid_input("AST symbol is absent from the module symbol universe"))
    }

    fn binding(&self, symbol: Spur) -> Option<&ParsedWarningLexicalBinding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&symbol))
    }

    fn bind(
        &mut self,
        ident: rue_parser::ast::Ident,
        binding: ParsedWarningLexicalBinding,
    ) -> CompileResult<()> {
        let symbol = self.symbol(ident)?;
        self.scopes
            .last_mut()
            .expect("warning projection always has a lexical scope")
            .insert(symbol, binding);
        Ok(())
    }

    fn bind_local(&mut self, ident: rue_parser::ast::Ident) -> CompileResult<()> {
        self.bind(ident, ParsedWarningLexicalBinding::Local)
    }

    fn add_path(&mut self, path: ParsedWarningStaticPath) {
        if let Some(head) = path.into_head() {
            self.heads.push(head);
        }
    }

    fn add_unqualified(&mut self, ident: rue_parser::ast::Ident) -> CompileResult<()> {
        let symbol = self.symbol(ident)?;
        match self.binding(symbol).cloned() {
            Some(ParsedWarningLexicalBinding::Local) => {}
            Some(ParsedWarningLexicalBinding::StaticAlias(path)) => self.add_path(path),
            None => self.add_path(ParsedWarningStaticPath {
                import_span: None,
                components: vec![symbol],
            }),
        }
        Ok(())
    }

    fn add_qualified(
        &mut self,
        path: impl IntoIterator<Item = rue_parser::ast::Ident>,
    ) -> CompileResult<()> {
        let mut symbols = path
            .into_iter()
            .map(|ident| self.symbol(ident))
            .collect::<CompileResult<Vec<_>>>()?;
        let Some(first) = symbols.first().copied() else {
            return Ok(());
        };
        match self.binding(first).cloned() {
            Some(ParsedWarningLexicalBinding::Local) => {}
            Some(ParsedWarningLexicalBinding::StaticAlias(mut alias)) => {
                alias.components.extend(symbols.drain(1..));
                self.add_path(alias);
            }
            None => self.add_path(ParsedWarningStaticPath {
                import_span: None,
                components: symbols,
            }),
        }
        Ok(())
    }

    fn visit_callable(
        &mut self,
        parameters: &[rue_parser::ast::Param],
        result: Option<&TypeExpr>,
        body: &Expr,
        _has_self: bool,
    ) -> CompileResult<()> {
        self.scopes.push(HashMap::new());
        let outcome = (|| {
            for parameter in parameters {
                self.visit_type(&parameter.ty)?;
                self.bind_local(parameter.name)?;
            }
            if let Some(result) = result {
                self.visit_type(result)?;
            }
            self.visit_expr(body)
        })();
        self.scopes.pop();
        outcome
    }

    fn visit_signature(
        &mut self,
        parameters: &[rue_parser::ast::Param],
        result: Option<&TypeExpr>,
    ) -> CompileResult<()> {
        for parameter in parameters {
            self.visit_type(&parameter.ty)?;
        }
        if let Some(result) = result {
            self.visit_type(result)?;
        }
        Ok(())
    }

    fn visit_args(&mut self, args: &[rue_parser::ast::CallArg]) -> CompileResult<()> {
        for argument in args {
            self.visit_expr(&argument.expr)?;
        }
        Ok(())
    }

    fn visit_array_length(&mut self, length: &rue_parser::ast::ArrayLength) -> CompileResult<()> {
        match length {
            rue_parser::ast::ArrayLength::Literal(_) | rue_parser::ast::ArrayLength::Named(_) => {}
            rue_parser::ast::ArrayLength::Call { name, args } => {
                self.add_unqualified(*name)?;
                for argument in args {
                    self.visit_array_length(argument)?;
                }
            }
        }
        Ok(())
    }

    fn visit_type(&mut self, ty: &TypeExpr) -> CompileResult<()> {
        match ty {
            TypeExpr::Named(_)
            | TypeExpr::Qualified { .. }
            | TypeExpr::Unit(_)
            | TypeExpr::Never(_)
            | TypeExpr::StrFixed { .. }
            | TypeExpr::IntArg { .. } => {}
            TypeExpr::Array {
                element, length, ..
            } => {
                self.visit_type(element)?;
                self.visit_array_length(length)?;
            }
            TypeExpr::Slice { element, .. } => self.visit_type(element)?,
            TypeExpr::AnonymousStruct {
                fields, methods, ..
            } => {
                for field in fields {
                    self.visit_type(&field.ty)?;
                }
                for method in methods {
                    self.visit_callable(
                        &method.params,
                        method.return_type.as_ref(),
                        &method.body,
                        method.receiver.is_some(),
                    )?;
                }
            }
            TypeExpr::AnonymousEnum { variants, .. } => {
                for variant in variants {
                    for payload in &variant.payload {
                        self.visit_type(payload)?;
                    }
                }
            }
            TypeExpr::PointerConst { pointee, .. } | TypeExpr::PointerMut { pointee, .. } => {
                self.visit_type(pointee)?;
            }
            TypeExpr::TypeCall { name, args, .. } => {
                self.add_unqualified(*name)?;
                for argument in args {
                    self.visit_type(argument)?;
                }
            }
            TypeExpr::QualifiedTypeCall { segments, args, .. } => {
                self.add_qualified(segments.iter().copied())?;
                for argument in args {
                    self.visit_type(argument)?;
                }
            }
        }
        Ok(())
    }

    fn static_path(&self, expr: &Expr) -> CompileResult<Option<ParsedWarningStaticPath>> {
        match expr {
            Expr::Ident(ident) => {
                let symbol = self.symbol(*ident)?;
                Ok(match self.binding(symbol) {
                    Some(ParsedWarningLexicalBinding::Local) => None,
                    Some(ParsedWarningLexicalBinding::StaticAlias(path)) => Some(path.clone()),
                    None => Some(ParsedWarningStaticPath {
                        import_span: None,
                        components: vec![symbol],
                    }),
                })
            }
            Expr::Field(field) => {
                let Some(mut path) = self.static_path(&field.base)? else {
                    return Ok(None);
                };
                path.components.push(self.symbol(field.field)?);
                Ok(Some(path))
            }
            _ => Ok(None),
        }
    }

    fn resolved_import_path(&self, expr: &Expr) -> CompileResult<Option<ParsedWarningStaticPath>> {
        let Expr::IntrinsicCall(import) = expr else {
            return Ok(None);
        };
        if self.spelling(import.name)? != "import" {
            return Ok(None);
        }
        let [IntrinsicArg::Expr(Expr::String(literal))] = import.args.as_slice() else {
            return Ok(None);
        };
        self.resolver.try_resolve(&literal.value).ok_or_else(|| {
            invalid_input("import literal is absent from the module symbol universe")
        })?;
        Ok(Some(ParsedWarningStaticPath {
            import_span: Some(import.span),
            components: Vec::new(),
        }))
    }

    fn collect_import(&mut self, value: &rue_parser::ast::IntrinsicCallExpr) -> CompileResult<()> {
        if self.spelling(value.name)? != "import" {
            return Ok(());
        }
        if let [IntrinsicArg::Expr(Expr::String(literal))] = value.args.as_slice() {
            let specifier = self.resolver.try_resolve(&literal.value).ok_or_else(|| {
                invalid_input("import literal is absent from the module symbol universe")
            })?;
            self.imports.valid.push(ImportDirective::new(
                self.module.clone(),
                value.span.start,
                value.span.end,
                Arc::from(specifier),
            ));
        } else {
            let (span, shape) = if value.args.len() != 1 {
                (
                    value.span,
                    InvalidImportShape::WrongArity {
                        actual: u32::try_from(value.args.len()).unwrap_or(u32::MAX),
                    },
                )
            } else {
                let span = match &value.args[0] {
                    IntrinsicArg::Expr(expr) => expr.span(),
                    IntrinsicArg::Type(ty) => ty.span(),
                };
                (span, InvalidImportShape::NonStringArgument)
            };
            self.imports
                .invalid
                .push(ParsedInvalidImportSite { span, shape });
        }
        Ok(())
    }

    fn visit_block(&mut self, block: &rue_parser::ast::BlockExpr) -> CompileResult<()> {
        use rue_parser::ast::{LetPattern, Statement};
        self.scopes.push(HashMap::new());
        let outcome = (|| {
            for statement in &block.statements {
                match statement {
                    Statement::Let(binding) => {
                        if let Some(ty) = &binding.ty {
                            self.visit_type(ty)?;
                        }
                        self.visit_expr(&binding.init)?;
                        if let LetPattern::Ident(ident) = binding.pattern {
                            let alias = (!binding.is_mut)
                                .then(|| {
                                    self.resolved_import_path(&binding.init).and_then(|path| {
                                        path.map_or_else(
                                            || self.static_path(&binding.init),
                                            |path| Ok(Some(path)),
                                        )
                                    })
                                })
                                .transpose()?
                                .flatten();
                            self.bind(
                                ident,
                                alias.map_or(
                                    ParsedWarningLexicalBinding::Local,
                                    ParsedWarningLexicalBinding::StaticAlias,
                                ),
                            )?;
                        }
                    }
                    Statement::Assign(assignment) => {
                        match &assignment.target {
                            AssignTarget::Var(_) => {}
                            AssignTarget::Field(field) => self.visit_expr(&field.base)?,
                            AssignTarget::Index(index) => {
                                self.visit_expr(&index.base)?;
                                self.visit_expr(&index.index)?;
                            }
                        }
                        self.visit_expr(&assignment.value)?;
                    }
                    Statement::Expr(expr) => self.visit_expr(expr)?,
                }
            }
            self.visit_expr(&block.expr)
        })();
        self.scopes.pop();
        outcome
    }

    fn visit_expr(&mut self, expr: &Expr) -> CompileResult<()> {
        use rue_parser::ast::{LetPattern, Pattern};
        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Bool(_)
            | Expr::Unit(_)
            | Expr::Ident(_)
            | Expr::Continue(_)
            | Expr::SelfExpr(_)
            | Expr::Error(_) => {}
            Expr::TypeLit(value) => self.visit_type(&value.type_expr)?,
            Expr::Binary(value) => {
                self.visit_expr(&value.left)?;
                self.visit_expr(&value.right)?;
            }
            Expr::Unary(value) => self.visit_expr(&value.operand)?,
            Expr::Paren(value) => self.visit_expr(&value.inner)?,
            Expr::Block(value) => self.visit_block(value)?,
            Expr::If(value) => {
                self.visit_expr(&value.cond)?;
                self.visit_block(&value.then_block)?;
                if let Some(block) = &value.else_block {
                    self.visit_block(block)?;
                }
            }
            Expr::Match(value) => {
                self.visit_expr(&value.scrutinee)?;
                for arm in &value.arms {
                    self.scopes.push(HashMap::new());
                    let outcome = (|| {
                        if let Pattern::Path(path) = &arm.pattern {
                            if let Some(base) = &path.base {
                                self.visit_expr(base)?;
                            }
                            if let Some(args) = &path.ctor_args {
                                if let Some(base) = &path.base {
                                    if let Some(mut head) = self.static_path(base)? {
                                        head.components.push(self.symbol(path.type_name)?);
                                        self.add_path(head);
                                    }
                                } else {
                                    self.add_unqualified(path.type_name)?;
                                }
                                self.visit_args(args)?;
                            }
                            for binding in &path.bindings {
                                self.bind_local(*binding)?;
                            }
                        }
                        self.visit_expr(&arm.body)
                    })();
                    self.scopes.pop();
                    outcome?;
                }
            }
            Expr::While(value) => {
                self.visit_expr(&value.cond)?;
                self.visit_block(&value.body)?;
            }
            Expr::Loop(value) => self.visit_block(&value.body)?,
            Expr::For(value) => {
                self.visit_expr(&value.iterable)?;
                self.scopes.push(HashMap::new());
                let outcome = (|| {
                    if let LetPattern::Ident(ident) = value.binder {
                        self.bind_local(ident)?;
                    }
                    self.visit_block(&value.body)
                })();
                self.scopes.pop();
                outcome?;
            }
            Expr::Call(value) => {
                self.add_unqualified(value.name)?;
                self.visit_args(&value.args)?;
            }
            Expr::Break(value) => {
                if let Some(value) = &value.value {
                    self.visit_expr(value)?;
                }
            }
            Expr::Return(value) => {
                if let Some(value) = &value.value {
                    self.visit_expr(value)?;
                }
            }
            Expr::Yield(value) => self.visit_expr(&value.value)?,
            Expr::StructLit(value) => {
                if let Some(base) = &value.base {
                    self.visit_expr(base)?;
                }
                if let Some(args) = &value.ctor_args {
                    if let Some(base) = &value.base {
                        if let Some(mut path) = self.static_path(base)? {
                            path.components.push(self.symbol(value.name)?);
                            self.add_path(path);
                        }
                    } else {
                        self.add_unqualified(value.name)?;
                    }
                    self.visit_args(args)?;
                }
                for field in &value.fields {
                    self.visit_expr(&field.value)?;
                }
            }
            Expr::Field(value) => self.visit_expr(&value.base)?,
            Expr::MethodCall(value) => {
                if let Some(mut path) = self.static_path(&value.receiver)? {
                    path.components.push(self.symbol(value.method)?);
                    self.add_path(path);
                }
                self.visit_expr(&value.receiver)?;
                self.visit_args(&value.args)?;
            }
            Expr::Try(value) => self.visit_expr(&value.operand)?,
            Expr::IntrinsicCall(value) => {
                self.collect_import(value)?;
                for argument in &value.args {
                    match argument {
                        IntrinsicArg::Expr(expr) => self.visit_expr(expr)?,
                        IntrinsicArg::Type(ty) => self.visit_type(ty)?,
                    }
                }
            }
            Expr::ArrayLit(value) => {
                for element in &value.elements {
                    self.visit_expr(element)?;
                }
                if let Some(repeat) = &value.repeat {
                    self.visit_array_length(repeat)?;
                }
            }
            Expr::Index(value) => {
                self.visit_expr(&value.base)?;
                self.visit_expr(&value.index)?;
            }
            Expr::Path(value) => {
                if let Some(base) = &value.base {
                    self.visit_expr(base)?;
                }
            }
            Expr::Comptime(value) => self.visit_expr(&value.expr)?,
            Expr::Checked(value) => self.visit_expr(&value.expr)?,
        }
        Ok(())
    }
}

fn collect_module_projections(
    ast: &Ast,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
) -> CompileResult<ParsedModuleProjectionCollector> {
    let mut projections = ParsedModuleProjectionCollector::default();
    for (item_index, item) in ast.items.iter().enumerate() {
        let item_index = u32::try_from(item_index)
            .map_err(|_| invalid_input("parsed item ordinal exceeds u32"))?;
        match item {
            Item::Function(value) => {
                let mut collector =
                    ParsedBodyProjectionCollector::new(module, resolver, &mut projections.imports);
                collector.visit_callable(
                    &value.params,
                    value.return_type.as_ref(),
                    &value.body,
                    false,
                )?;
                projections.warning_call_heads.insert(
                    ParsedDeclarationAstLocator::TopLevel { item: item_index },
                    collector.finish(),
                );
            }
            Item::Struct(value) => {
                for field in &value.fields {
                    let mut collector = ParsedBodyProjectionCollector::new(
                        module,
                        resolver,
                        &mut projections.imports,
                    );
                    collector.visit_type(&field.ty)?;
                }
                for (method_index, method) in value.methods.iter().enumerate() {
                    let method_index = u32::try_from(method_index)
                        .map_err(|_| invalid_input("parsed method ordinal exceeds u32"))?;
                    let mut collector = ParsedBodyProjectionCollector::new(
                        module,
                        resolver,
                        &mut projections.imports,
                    );
                    collector.visit_callable(
                        &method.params,
                        method.return_type.as_ref(),
                        &method.body,
                        method.receiver.is_some(),
                    )?;
                    projections.warning_call_heads.insert(
                        ParsedDeclarationAstLocator::StructMethod {
                            item: item_index,
                            method: method_index,
                        },
                        collector.finish(),
                    );
                }
            }
            Item::DropFn(value) => {
                let mut collector =
                    ParsedBodyProjectionCollector::new(module, resolver, &mut projections.imports);
                collector.visit_callable(&[], None, &value.body, true)?;
                projections.warning_call_heads.insert(
                    ParsedDeclarationAstLocator::TopLevel { item: item_index },
                    collector.finish(),
                );
            }
            Item::Extern(block) => {
                for foreign in &block.fns {
                    let mut collector = ParsedBodyProjectionCollector::new(
                        module,
                        resolver,
                        &mut projections.imports,
                    );
                    collector.visit_signature(&foreign.params, foreign.return_type.as_ref())?;
                }
            }
            Item::Const(value) => {
                let mut collector =
                    ParsedBodyProjectionCollector::new(module, resolver, &mut projections.imports);
                if let Some(ty) = &value.ty {
                    collector.visit_type(ty)?;
                }
                collector.visit_expr(&value.init)?;
            }
            Item::Enum(value) => {
                for variant in &value.variants {
                    for ty in &variant.payload {
                        let mut collector = ParsedBodyProjectionCollector::new(
                            module,
                            resolver,
                            &mut projections.imports,
                        );
                        collector.visit_type(ty)?;
                    }
                }
            }
            Item::Error(_) => {}
        }
    }
    projections.imports.valid.sort();
    projections
        .imports
        .invalid
        .sort_by_key(|site| (site.span.file_id.index(), site.span.start));
    Ok(projections)
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

fn declaration_import_range(
    declaration: Span,
    import_sites: &[ImportDirective],
) -> CompileResult<RawDeclarationImportRange> {
    let start_index = import_sites.partition_point(|site| site.source_offset() < declaration.start);
    let end = import_sites.partition_point(|site| site.source_offset() < declaration.end);
    if import_sites[start_index..end]
        .iter()
        .any(|site| site.source_end() > declaration.end)
    {
        return Err(invalid_input(
            "declaration import site crosses its declaration boundary",
        ));
    }
    let start =
        u32::try_from(start_index).map_err(|_| invalid_input("module import index exceeds u32"))?;
    let len = u32::try_from(end - start_index)
        .map_err(|_| invalid_input("declaration import count exceeds u32"))?;
    Ok(RawDeclarationImportRange { start, len })
}

fn project_warning_call_heads(
    category: DeclarationCandidateCategory,
    locator: ParsedDeclarationAstLocator,
    declaration: Span,
    import_range: Option<RawDeclarationImportRange>,
    import_sites: &[ImportDirective],
    resolver: &FrozenSymbolResolver,
    raw_heads: &HashMap<ParsedDeclarationAstLocator, Vec<RawWarningCallHead>>,
) -> CompileResult<Arc<[ParsedWarningCallHead]>> {
    let owns_warning_body = matches!(
        category,
        DeclarationCandidateCategory::Function
            | DeclarationCandidateCategory::Destructor
            | DeclarationCandidateCategory::Method
            | DeclarationCandidateCategory::AssociatedFunction
    );
    if !owns_warning_body {
        if raw_heads.contains_key(&locator) {
            return Err(invalid_input(
                "bodyless declaration unexpectedly owns warning call heads",
            ));
        }
        return Ok(Arc::from([]));
    }

    let raw_heads = raw_heads
        .get(&locator)
        .ok_or_else(|| invalid_input("body declaration has no warning call-head projection"))?;
    let import_range = import_range
        .ok_or_else(|| invalid_input("body declaration has no canonical import range"))?;
    let start = usize::try_from(import_range.start)
        .map_err(|_| invalid_input("declaration import start exceeds usize"))?;
    let end = import_range
        .start
        .checked_add(import_range.len)
        .and_then(|end| usize::try_from(end).ok())
        .ok_or_else(|| invalid_input("declaration import range exceeds usize"))?;
    let imports = import_sites
        .get(start..end)
        .ok_or_else(|| invalid_input("declaration import range is outside the module table"))?;

    let mut projected = Vec::with_capacity(raw_heads.len());
    for head in raw_heads.iter() {
        if head.components.is_empty() {
            return Err(invalid_input("warning call head has no path components"));
        }
        let import = head
            .import_span
            .map(|span| {
                if span.file_id != declaration.file_id
                    || span.start < declaration.start
                    || span.end > declaration.end
                {
                    return Err(invalid_input(
                        "warning import alias is outside its declaration",
                    ));
                }
                let occurrence = imports
                    .binary_search_by_key(&(span.start, span.end), |site| {
                        (site.source_offset(), site.source_end())
                    })
                    .map_err(|_| {
                        invalid_input("warning import alias has no canonical import site")
                    })?;
                let site = &imports[occurrence];
                Ok(ParsedWarningImport {
                    occurrence: u32::try_from(occurrence)
                        .map_err(|_| invalid_input("declaration import occurrence exceeds u32"))?,
                    specifier: Arc::from(site.specifier()),
                })
            })
            .transpose()?;
        let components = head
            .components
            .iter()
            .map(|spur| {
                let symbol = resolver.symbol(*spur)?;
                Ok(Arc::from(resolver.resolve(&symbol)?))
            })
            .collect::<CompileResult<Vec<_>>>()?;
        projected.push(ParsedWarningCallHead {
            import,
            components: components.into(),
        });
    }
    projected.sort();
    projected.dedup();
    Ok(projected.into())
}

fn build_definition_index(
    module: ModuleId,
    file_id: FileId,
    source_text: &str,
    tokens: &[rue_lexer::Token],
    ast: &Ast,
    resolver: &FrozenSymbolResolver,
    import_sites: &[ImportDirective],
    raw_warning_call_heads: &HashMap<ParsedDeclarationAstLocator, Vec<RawWarningCallHead>>,
) -> CompileResult<ParsedDefinitionIndex> {
    if import_sites.windows(2).any(|sites| {
        (sites[0].source_offset(), sites[0].source_end())
            > (sites[1].source_offset(), sites[1].source_end())
    }) {
        return Err(invalid_input(
            "module import sites are not in canonical source order",
        ));
    }
    let mut pending = Vec::new();
    let mut pending_declarations = Vec::<(
        DeclarationShellFact,
        ParsedDeclarationAstLocator,
        Span,
        Option<Span>,
        Option<Span>,
        bool,
        Arc<[Arc<str>]>,
        Option<RawDeclarationImportRange>,
        Arc<[ParsedWarningCallHead]>,
        Arc<[rue_rir::AnonymousTypeSite]>,
    )>::new();
    let resolve_name = |ident: rue_parser::Ident| -> CompileResult<Arc<str>> {
        let symbol = resolver.symbol(ident.name)?;
        Ok(Arc::from(resolver.resolve(&symbol)?))
    };
    // The method names one body calls on its own `self` receiver, normalized
    // (sorted, deduplicated). Retained per method candidate so an accessor's
    // signature terminal can carry its siblings' 6.6:14 cycle edges
    // (RUE-1282) exactly as it carries their `-> borrow` qualifiers.
    let self_call_targets = |body: &rue_parser::ast::Expr| -> CompileResult<Arc<[Arc<str>]>> {
        use rue_parser::ast::Expr;
        let mut walk = vec![body];
        let mut targets: Vec<Arc<str>> = Vec::new();
        while let Some(expr) = walk.pop() {
            if let Expr::MethodCall(call) = expr {
                let mut receiver = &*call.receiver;
                while let Expr::Paren(paren) = receiver {
                    receiver = &paren.inner;
                }
                if matches!(receiver, Expr::SelfExpr(_)) {
                    targets.push(resolve_name(call.method)?);
                }
            }
            expr.child_exprs(&mut walk);
        }
        targets.sort();
        targets.dedup();
        Ok(Arc::from(targets))
    };
    let parameters =
        |params: &[rue_parser::Param]| -> CompileResult<Arc<[DeclarationParameterHeader]>> {
            params
                .iter()
                .map(|param| {
                    let (mode, is_comptime) = candidate_parameter_mode(param.mode);
                    let is_type_parameter = is_comptime
                        && match &param.ty {
                            rue_parser::ast::TypeExpr::Named(name) => {
                                resolve_name(*name)?.as_ref() == "type"
                            }
                            _ => false,
                        };
                    Ok(DeclarationParameterHeader {
                        name: resolve_name(param.name)?,
                        mode,
                        is_comptime,
                        is_type_parameter,
                    })
                })
                .collect::<CompileResult<Vec<_>>>()
                .map(Into::into)
        };
    // The semantic declaration layer treats every comptime parameter as a
    // generic specialization input, not only `comptime T: type` parameters.
    let is_generic = |params: &[rue_parser::Param]| -> CompileResult<bool> {
        Ok(params
            .iter()
            .any(|parameter| parameter.mode == rue_parser::ast::ParamMode::Comptime))
    };
    for (item_index, item) in ast.items.iter().enumerate() {
        let item_index = u32::try_from(item_index)
            .map_err(|_| invalid_input("parsed item ordinal exceeds u32"))?;
        // A foreign `extern "C"` block expands into one module-item definition
        // per member `fn` (ADR-0064 C FFI); every other item is a single
        // definition.
        let item_parts: Vec<_> = if let Item::Extern(block) = item {
            crate::definition_snapshot::extern_definition_parts(block).collect()
        } else {
            let Some(parts) = definition_parts(item) else {
                let Item::Error(span) = item else {
                    unreachable!()
                };
                return Err(invalid_input(format!(
                    "parsed module contains recovered error item at {}..{}",
                    span.start, span.end
                )));
            };
            vec![parts]
        };
        for parts in item_parts {
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

        let mut push = |category,
                        name: Arc<str>,
                        owner: Option<DeclarationCandidateOwner>,
                        ast_locator: ParsedDeclarationAstLocator,
                        is_public,
                        parameters,
                        receiver,
                        receiver_is_mut,
                        is_generic,
                        is_unchecked,
                        is_extern,
                        is_accessor,
                        method_self_call_targets: Arc<[Arc<str>]>,
                        declaration_span,
                        signature_spans: Vec<Span>,
                        const_initializer_span: Option<Span>,
                        raw_body_span: Option<Span>,
                        anonymous_sites: Arc<[rue_rir::AnonymousTypeSite]>|
         -> CompileResult<()> {
            let signature_fingerprint = declaration_signature_fingerprint(
                file_id,
                source_text,
                tokens,
                resolver,
                &signature_spans,
            )?;
            if let Some(body) = raw_body_span {
                validate_span("raw declaration body", body, file_id, source_text)?;
            }
            let raw_import_range = if matches!(
                category,
                DeclarationCandidateCategory::Function
                    | DeclarationCandidateCategory::ConstCandidate
                    | DeclarationCandidateCategory::Destructor
                    | DeclarationCandidateCategory::Method
                    | DeclarationCandidateCategory::AssociatedFunction
            ) {
                Some(declaration_import_range(declaration_span, import_sites)?)
            } else {
                None
            };
            let warning_call_heads = project_warning_call_heads(
                category,
                ast_locator,
                declaration_span,
                raw_import_range,
                import_sites,
                resolver,
                raw_warning_call_heads,
            )?;
            pending_declarations.push((
                DeclarationShellFact {
                    key: DeclarationCandidateKey {
                        module: module.clone(),
                        category,
                        name,
                        owner,
                        duplicate_discriminator: 0,
                    },
                    is_public,
                    parameters,
                    receiver,
                    receiver_is_mut,
                    is_generic,
                    is_unchecked,
                    is_extern,
                    signature_fingerprint,
                },
                ast_locator,
                declaration_span,
                const_initializer_span,
                raw_body_span,
                is_accessor,
                method_self_call_targets,
                raw_import_range,
                warning_call_heads,
                anonymous_sites,
            ));
            Ok(())
        };

        match item {
            Item::Function(function) => push(
                DeclarationCandidateCategory::Function,
                resolve_name(function.name)?,
                None,
                ParsedDeclarationAstLocator::TopLevel { item: item_index },
                function.visibility == Visibility::Public,
                parameters(&function.params)?,
                None,
                false,
                is_generic(&function.params)?,
                function.is_unchecked,
                false,
                function.borrow_return.is_some(),
                Arc::from([]),
                function.span,
                vec![signature_prefix(function.span, function.body.span())?],
                None,
                Some(function.body.span()),
                rue_rir::anonymous_type_sites(&function.body).into(),
            )?,
            Item::Struct(structure) => {
                let owner_name = resolve_name(structure.name)?;
                push(
                    DeclarationCandidateCategory::Struct,
                    owner_name.clone(),
                    None,
                    ParsedDeclarationAstLocator::TopLevel { item: item_index },
                    structure.visibility == Visibility::Public,
                    Arc::from([]),
                    None,
                    false,
                    false,
                    false,
                    false,
                    false,
                    Arc::from([]),
                    structure.span,
                    signature_fragments_excluding_method_bodies(structure)?,
                    None,
                    None,
                    Arc::from([]),
                )?;
                let owner = DeclarationCandidateOwner {
                    category: DeclarationCandidateCategory::Struct,
                    name: owner_name,
                };
                for (method_index, method) in structure.methods.iter().enumerate() {
                    let method_index = u32::try_from(method_index)
                        .map_err(|_| invalid_input("parsed method ordinal exceeds u32"))?;
                    let receiver = method
                        .receiver
                        .as_ref()
                        .map(|receiver| candidate_parameter_mode(receiver.mode).0);
                    push(
                        if receiver.is_some() {
                            DeclarationCandidateCategory::Method
                        } else {
                            DeclarationCandidateCategory::AssociatedFunction
                        },
                        resolve_name(method.name)?,
                        Some(owner.clone()),
                        ParsedDeclarationAstLocator::StructMethod {
                            item: item_index,
                            method: method_index,
                        },
                        false,
                        parameters(&method.params)?,
                        receiver,
                        method
                            .receiver
                            .as_ref()
                            .is_some_and(|receiver| receiver.is_mut),
                        is_generic(&method.params)?,
                        false,
                        false,
                        method.borrow_return.is_some(),
                        self_call_targets(&method.body)?,
                        method.span,
                        vec![signature_prefix(method.span, method.body.span())?],
                        None,
                        Some(method.body.span()),
                        rue_rir::anonymous_type_sites(&method.body).into(),
                    )?;
                }
            }
            Item::Enum(value) => push(
                DeclarationCandidateCategory::Enum,
                resolve_name(value.name)?,
                None,
                ParsedDeclarationAstLocator::TopLevel { item: item_index },
                value.visibility == Visibility::Public,
                Arc::from([]),
                None,
                false,
                false,
                false,
                false,
                false,
                Arc::from([]),
                value.span,
                vec![value.span],
                None,
                None,
                Arc::from([]),
            )?,
            Item::Const(value) => {
                let declared_type = value.ty.as_ref().map(TypeExpr::span);
                if let Some(span) = declared_type {
                    validate_span("constant declared type", span, file_id, source_text)?;
                }
                let initializer = value.init.span();
                validate_span("constant initializer", initializer, file_id, source_text)?;
                push(
                    DeclarationCandidateCategory::ConstCandidate,
                    resolve_name(value.name)?,
                    None,
                    ParsedDeclarationAstLocator::TopLevel { item: item_index },
                    value.visibility == Visibility::Public,
                    Arc::from([]),
                    None,
                    false,
                    false,
                    false,
                    false,
                    false,
                    Arc::from([]),
                    value.span,
                    vec![signature_prefix(value.span, value.init.span())?],
                    Some(initializer),
                    None,
                    rue_rir::anonymous_type_sites(&value.init).into(),
                )?;
            }
            Item::DropFn(value) => push(
                DeclarationCandidateCategory::Destructor,
                resolve_name(value.type_name)?,
                Some(DeclarationCandidateOwner {
                    category: DeclarationCandidateCategory::Struct,
                    name: resolve_name(value.type_name)?,
                }),
                ParsedDeclarationAstLocator::TopLevel { item: item_index },
                false,
                Arc::from([]),
                Some(DeclarationParameterMode::Value),
                false,
                false,
                false,
                false,
                false,
                Arc::from([]),
                value.span,
                vec![signature_prefix(value.span, value.body.span())?],
                None,
                Some(value.body.span()),
                rue_rir::anonymous_type_sites(&value.body).into(),
            )?,
            Item::Extern(block) => {
                for (function_index, function) in block.fns.iter().enumerate() {
                    let function_index = u32::try_from(function_index)
                        .map_err(|_| invalid_input("parsed extern member ordinal exceeds u32"))?;
                    push(
                        DeclarationCandidateCategory::ExternFunction,
                        resolve_name(function.name)?,
                        None,
                        ParsedDeclarationAstLocator::ExternFunction {
                            item: item_index,
                            function: function_index,
                        },
                        false,
                        parameters(&function.params)?,
                        None,
                        false,
                        is_generic(&function.params)?,
                        false,
                        true,
                        false,
                        Arc::from([]),
                        function.span,
                        vec![function.span],
                        None,
                        None,
                        Arc::from([]),
                    )?;
                }
            }
            Item::Error(_) => {}
        }
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
    #[cfg(test)]
    let mut by_name = BTreeMap::<_, Vec<_>>::new();
    let mut candidates = Vec::with_capacity(pending.len());
    for (index, (parts, symbol, name)) in pending.into_iter().enumerate() {
        #[cfg(not(test))]
        let _ = index;
        #[cfg(not(test))]
        let _ = symbol;
        #[cfg(test)]
        let index = u32::try_from(index)
            .map_err(|_| invalid_input("parsed definition occurrence count exceeds u32"))?;
        #[cfg(test)]
        let occurrence = ParsedDefinitionOccurrence(index);
        #[cfg(test)]
        by_name
            .entry((parts.namespace, name.clone()))
            .or_default()
            .push(occurrence);
        candidates.push(ParsedDefinitionCandidate {
            #[cfg(test)]
            occurrence,
            namespace: parts.namespace,
            kind: parts.kind,
            visibility: parts.visibility,
            name,
            #[cfg(test)]
            symbol,
            name_span: parts.name.span,
            declaration_span: parts.declaration_span,
        });
    }
    #[cfg(test)]
    let by_name = by_name
        .into_iter()
        .map(|(key, value)| (key, value.into()))
        .collect();
    pending_declarations.sort_by(|left, right| {
        left.2
            .start
            .cmp(&right.2.start)
            .then(left.2.end.cmp(&right.2.end))
            .then(left.0.key.category.cmp(&right.0.key.category))
            .then(left.0.key.name.cmp(&right.0.key.name))
    });
    let mut duplicate_counts = BTreeMap::new();
    let declarations = pending_declarations
        .into_iter()
        .map(
            |(
                mut fact,
                ast_locator,
                declaration_span,
                const_initializer_span,
                raw_body_span,
                is_accessor,
                self_call_targets,
                raw_import_range,
                warning_call_heads,
                anonymous_sites,
            )| {
                let duplicate = duplicate_counts
                    .entry((
                        fact.key.category,
                        fact.key.name.clone(),
                        fact.key.owner.clone(),
                    ))
                    .or_insert(0_u32);
                fact.key.duplicate_discriminator = *duplicate;
                *duplicate = duplicate.checked_add(1).ok_or_else(|| {
                    invalid_input("declaration duplicate discriminator exceeds u32")
                })?;
                Ok(ParsedDeclarationCandidate {
                    fact,
                    ast_locator,
                    declaration_span,
                    const_initializer_span,
                    raw_body_span,
                    is_accessor,
                    self_call_targets,
                    raw_import_range,
                    warning_call_heads,
                    anonymous_sites,
                })
            },
        )
        .collect::<CompileResult<Vec<_>>>()?;
    let mut declaration_by_key = HashMap::with_capacity(declarations.len());
    let mut declaration_capabilities = Vec::with_capacity(declarations.len());
    for (index, candidate) in declarations.iter().enumerate() {
        let key = candidate.fact.key.clone();
        let duplicate_multiplicity = *duplicate_counts
            .get(&(key.category, key.name.clone(), key.owner.clone()))
            .expect("every declaration contributes to its duplicate count");
        if declaration_by_key.insert(key.clone(), index).is_some() {
            declaration_capabilities.push(DeclarationOccurrenceCapability::Ambiguous {
                key,
                multiplicity: 2,
            });
        } else {
            declaration_capabilities.push(DeclarationOccurrenceCapability::Exact {
                key,
                duplicate_multiplicity,
            });
        }
    }
    let mut key_by_locator = HashMap::with_capacity(declarations.len());
    for candidate in &declarations {
        if key_by_locator
            .insert(candidate.ast_locator, candidate.fact.key.clone())
            .is_some()
        {
            return Err(invalid_input(
                "multiple declaration candidates share one AST locator",
            ));
        }
    }
    let exact_key = |locator| {
        key_by_locator
            .get(&locator)
            .cloned()
            .ok_or_else(|| invalid_input("AST item has no exact declaration candidate"))
    };
    let mut rir_recipes = Vec::with_capacity(ast.items.len());
    for (item, syntax) in ast.items.iter().enumerate() {
        let item =
            u32::try_from(item).map_err(|_| invalid_input("parsed item ordinal exceeds u32"))?;
        match syntax {
            Item::Function(_) | Item::Enum(_) | Item::Const(_) | Item::DropFn(_) => {
                rir_recipes.push(ParsedRirRecipe::Single(exact_key(
                    ParsedDeclarationAstLocator::TopLevel { item },
                )?));
            }
            Item::Struct(structure) => {
                let shell = exact_key(ParsedDeclarationAstLocator::TopLevel { item })?;
                let methods = structure
                    .methods
                    .iter()
                    .enumerate()
                    .map(|(method, _)| {
                        exact_key(ParsedDeclarationAstLocator::StructMethod {
                            item,
                            method: u32::try_from(method)
                                .map_err(|_| invalid_input("parsed method ordinal exceeds u32"))?,
                        })
                    })
                    .collect::<CompileResult<Vec<_>>>()?;
                rir_recipes.push(ParsedRirRecipe::Struct {
                    shell,
                    methods: methods.into(),
                });
            }
            Item::Extern(block) => {
                let functions = block
                    .fns
                    .iter()
                    .enumerate()
                    .map(|(function, _)| {
                        exact_key(ParsedDeclarationAstLocator::ExternFunction {
                            item,
                            function: u32::try_from(function).map_err(|_| {
                                invalid_input("parsed extern member ordinal exceeds u32")
                            })?,
                        })
                    })
                    .collect::<CompileResult<Vec<_>>>()?;
                rir_recipes.push(ParsedRirRecipe::Extern {
                    functions: functions.into(),
                });
            }
            Item::Error(_) => {
                return Err(invalid_input(
                    "parsed module contains recovered error item in RIR recipes",
                ));
            }
        }
    }
    declaration_capabilities.sort_by(|left, right| left.key().cmp(right.key()));
    Ok(ParsedDefinitionIndex {
        candidates: candidates.into(),
        declarations: declarations.into(),
        declaration_by_key,
        rir_recipes: rir_recipes.into(),
        declaration_capabilities: declaration_capabilities.into(),
        #[cfg(test)]
        declaration_import_locator_materializations: Arc::new(AtomicUsize::new(0)),
        #[cfg(test)]
        by_name,
    })
}

fn candidate_parameter_mode(mode: rue_parser::ast::ParamMode) -> (DeclarationParameterMode, bool) {
    match mode {
        rue_parser::ast::ParamMode::Normal => (DeclarationParameterMode::Value, false),
        rue_parser::ast::ParamMode::Borrow => (DeclarationParameterMode::Borrow, false),
        rue_parser::ast::ParamMode::Inout => (DeclarationParameterMode::Inout, false),
        rue_parser::ast::ParamMode::Comptime => (DeclarationParameterMode::Value, true),
    }
}

fn signature_prefix(declaration: Span, payload: Span) -> CompileResult<Span> {
    if declaration.file_id != payload.file_id
        || payload.start < declaration.start
        || payload.end > declaration.end
        || payload.start >= payload.end
    {
        return Err(invalid_input(
            "declaration payload is not contained by its declaration",
        ));
    }
    Ok(Span::with_file(
        declaration.file_id,
        declaration.start,
        payload.start,
    ))
}

fn signature_fragments_excluding_method_bodies(
    structure: &rue_parser::ast::StructDecl,
) -> CompileResult<Vec<Span>> {
    let mut fragments = Vec::with_capacity(structure.methods.len() + 1);
    let mut cursor = structure.span.start;
    for method in &structure.methods {
        let body = method.body.span();
        if body.file_id != structure.span.file_id
            || body.start < cursor
            || body.end > structure.span.end
            || body.start >= body.end
        {
            return Err(invalid_input(
                "struct method body is not ordered within its declaration",
            ));
        }
        fragments.push(Span::with_file(structure.span.file_id, cursor, body.start));
        cursor = body.end;
    }
    fragments.push(Span::with_file(
        structure.span.file_id,
        cursor,
        structure.span.end,
    ));
    Ok(fragments)
}

fn declaration_signature_fingerprint(
    file_id: FileId,
    source_text: &str,
    tokens: &[rue_lexer::Token],
    resolver: &FrozenSymbolResolver,
    spans: &[Span],
) -> CompileResult<[u8; 32]> {
    let mut hasher = Sha256::new();
    for span in spans {
        validate_span("declaration signature", *span, file_id, source_text)?;
        for token in tokens.iter().filter(|token| {
            token.span.file_id == file_id
                && token.span.start >= span.start
                && token.span.end <= span.end
                && !matches!(token.kind, rue_lexer::TokenKind::Eof)
        }) {
            let value = match token.kind {
                rue_lexer::TokenKind::Ident(spur) => {
                    format!("ident:{}", resolver.resolver.resolve(&spur))
                }
                rue_lexer::TokenKind::String(spur) => {
                    format!("string:{}", resolver.resolver.resolve(&spur))
                }
                rue_lexer::TokenKind::Int(value) => format!("int:{value}"),
                kind => kind.name().to_owned(),
            };
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        hasher.update(0_u64.to_le_bytes());
    }
    Ok(hasher.finalize().into())
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
        CompilerSession, ModuleResolutionInput, ModuleResolutionInputs, SemanticInputDescriptor,
        SourceMetadata,
    };

    fn declaration_facts(source: &str) -> Vec<DeclarationShellFact> {
        let snapshot = snapshot(&[(1, "/main.rue", "main.rue", source)], 1);
        let parsed = parse_source_snapshot_modules(&snapshot).unwrap();
        parsed.modules()[0]
            .definitions()
            .declaration_capabilities()
            .iter()
            .map(|capability| {
                parsed.modules()[0]
                    .definitions()
                    .evaluate_declaration_shell(capability.key())
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn declaration_candidates_cover_every_named_syntax_family_and_duplicates() {
        use DeclarationCandidateCategory as C;
        let facts = declaration_facts(
            r#"
pub struct Box {
    fn get(borrow self, comptime T: type) -> i32 { 0 }
    fn make() -> Box { Box {} }
}
enum Choice { A, B(i32) }
const selected = 1;
drop fn Box(self) {}
unchecked fn run(value: i32) -> i32 { value }
extern "C" { fn getpid() -> i32; }
fn duplicate() {}
fn duplicate() {}
fn type_factory() -> type { struct { fn hidden(self) {} } }
"#,
        );
        let categories = facts
            .iter()
            .map(|fact| fact.key.category)
            .collect::<Vec<_>>();
        for expected in [
            C::Struct,
            C::Method,
            C::AssociatedFunction,
            C::Enum,
            C::ConstCandidate,
            C::Destructor,
            C::Function,
            C::ExternFunction,
        ] {
            assert!(categories.contains(&expected), "missing {expected:?}");
        }
        let duplicates = facts
            .iter()
            .filter(|fact| fact.key.name.as_ref() == "duplicate")
            .map(|fact| fact.key.duplicate_discriminator)
            .collect::<Vec<_>>();
        assert_eq!(duplicates, vec![0, 1]);
        assert!(facts.iter().all(|fact| fact.key.name.as_ref() != "hidden"));
        let method = facts
            .iter()
            .find(|fact| fact.key.category == C::Method)
            .unwrap();
        assert_eq!(method.key.owner.as_ref().unwrap().name.as_ref(), "Box");
        assert_eq!(method.parameters.len(), 1);
        assert!(method.parameters[0].is_comptime);
        assert_eq!(method.receiver, Some(DeclarationParameterMode::Borrow));
    }

    #[test]
    fn declaration_ast_locators_select_exact_typed_members_and_rebind() {
        use DeclarationCandidateCategory as C;

        let source = r#"
struct Box {
    fn get(self) -> i32 { 0 }
    fn make() -> Box { Box {} }
}
enum Choice { A }
const selected: i32 = 1;
drop fn Box(self) {}
fn duplicate() {}
fn duplicate() {}
extern "C" { fn getpid() -> i32; }
"#;
        let original = snapshot(&[(1, "/main.rue", "main.rue", source)], 1);
        let parsed = parse_source_snapshot_modules(&original).unwrap();
        let module = &parsed.modules()[0];
        let keys = module
            .definitions()
            .declaration_keys_in_source_order()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            keys.iter()
                .filter(|key| key.name.as_ref() == "duplicate")
                .count(),
            2
        );
        for key in &keys {
            let locator = module.definitions().declaration_locator(key).unwrap();
            let (category, span) = match module.declaration_ast(key).unwrap() {
                ParsedDeclarationAstRef::Function(value) => (C::Function, value.span),
                ParsedDeclarationAstRef::Struct(value) => (C::Struct, value.span),
                ParsedDeclarationAstRef::Enum(value) => (C::Enum, value.span),
                ParsedDeclarationAstRef::Const(value) => (C::ConstCandidate, value.span),
                ParsedDeclarationAstRef::Destructor(value) => (C::Destructor, value.span),
                ParsedDeclarationAstRef::Method { owner, method, .. } => {
                    assert!(
                        method.span.start >= owner.span.start && method.span.end <= owner.span.end
                    );
                    (key.category, method.span)
                }
                ParsedDeclarationAstRef::ExternFunction { function } => {
                    (C::ExternFunction, function.span)
                }
            };
            assert_eq!(category, key.category);
            assert_eq!(span, locator.declaration_span);
        }

        let moved = snapshot(&[(9, "/main.rue", "main.rue", source)], 9);
        let moved = rebind_parsed_module(&moved, module);
        for key in keys {
            let span = match moved.declaration_ast(&key).unwrap() {
                ParsedDeclarationAstRef::Function(value) => value.span,
                ParsedDeclarationAstRef::Struct(value) => value.span,
                ParsedDeclarationAstRef::Enum(value) => value.span,
                ParsedDeclarationAstRef::Const(value) => value.span,
                ParsedDeclarationAstRef::Destructor(value) => value.span,
                ParsedDeclarationAstRef::Method { method, .. } => method.span,
                ParsedDeclarationAstRef::ExternFunction { function, .. } => function.span,
            };
            assert_eq!(span.file_id, FileId::new(9));
        }
    }

    #[test]
    fn declaration_shell_facts_ignore_payloads_whitespace_and_const_classification() {
        let value = declaration_facts(
            "struct Box { fn get(self) -> i32 { 1 } } const item = 1; fn main() { }",
        );
        let module = declaration_facts(
            "// leading relocation\n  struct Box { fn // between signature tokens\n get(self) -> i32 { 999 } }\n\n const item = @import(\"x.rue\"); // payload boundary\n fn main() { let x = 2; }",
        );
        assert_eq!(value, module);

        let edited = declaration_facts(
            "struct Box { fn get(borrow self) -> i32 { 1 } } const item = 1; fn main() { }",
        );
        assert_ne!(value, edited);

        let mutable = declaration_facts(
            "struct Box { fn get(mut self) -> i32 { 1 } } const item = 1; fn main() { }",
        );
        let method = mutable
            .iter()
            .find(|fact| fact.key.category == DeclarationCandidateCategory::Method)
            .unwrap();
        assert_eq!(method.receiver, Some(DeclarationParameterMode::Value));
        assert!(method.receiver_is_mut);
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
        let update = CompilerSession::new().update(&snapshot);
        assert_eq!(update.work().syntax.lexer_invocations, 2);
        assert_eq!(update.work().syntax.parser_invocations, 2);
        let program = update.into_owner_result().unwrap();
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
        assert_eq!(program.import_directives().len(), 1);
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
        let update = CompilerSession::new().update(&snapshot);
        let work = update.work();
        let first = update.into_owner_result().unwrap();
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

    /// Module identity keys the canonical parse query, so a module whose
    /// content is unchanged keeps its syntax payload across a relocation and
    /// only its cheap envelope is rebuilt.
    #[test]
    fn relocation_rebinds_without_parsing() {
        let mut session = CompilerSession::new();
        let first = snapshot(
            &[(
                7,
                "/old/main.rue",
                "main.rue",
                "fn main() { @import(\"dep.rue\"); }",
            )],
            7,
        );
        let update = session.update(&first);
        assert_eq!(update.work().modules_reparsed, 1);
        let first = update.into_owner_result().unwrap();
        let payload = first.modules()[0].payload_ptr();

        let moved = snapshot(
            &[(
                7,
                "/new/main.rue",
                "main.rue",
                "fn main() { @import(\"dep.rue\"); }",
            )],
            7,
        );
        let update = session.update(&moved);
        let work = update.work();
        assert_eq!(work.syntax, SyntaxWork::default());
        assert_eq!(work.modules_reused, 0);
        assert_eq!(work.modules_rebound, 1);
        assert_eq!(work.modules_reparsed, 0);
        assert_eq!(work.modules_considered, 1);
        assert_eq!(work.previous_module_lookups, 1);
        let moved = update.into_owner_result().unwrap();
        assert_eq!(payload, moved.modules()[0].payload_ptr());
        assert_eq!(moved.modules()[0].physical_path(), "/new/main.rue");
        assert_eq!(moved.modules()[0].module_id().as_str(), "main.rue");
        assert_eq!(
            moved.modules()[0].imports()[0].importer(),
            moved.modules()[0].module_id()
        );
    }

    /// A logical rename is a different module, so it is parsed rather than
    /// rebound even when the bytes are identical.
    #[test]
    fn logical_rename_parses_the_renamed_module() {
        let mut session = CompilerSession::new();
        let first = snapshot(&[(7, "/main.rue", "old-name.rue", "fn main() {}")], 7);
        session.update(&first).into_owner_result().unwrap();

        let renamed = snapshot(&[(7, "/main.rue", "new-name.rue", "fn main() {}")], 7);
        let update = session.update(&renamed);
        let work = update.work();
        assert_eq!(work.modules_considered, 1);
        assert_eq!(work.modules_reparsed, 1);
        assert_eq!(work.syntax.parser_invocations, 1);
        assert_eq!(
            update.invalidation().added,
            [ModuleId::from_logical_path("new-name.rue").unwrap()]
        );
        assert_eq!(
            update.invalidation().removed,
            [ModuleId::from_logical_path("old-name.rue").unwrap()]
        );
    }

    /// A FileId epoch change leaves module identity and content alone, so the
    /// canonical query is reused and only the envelope is rebound.
    #[test]
    fn file_id_epoch_change_rebinds_equal_source() {
        let mut session = CompilerSession::new();
        let first = snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
        let first = session.update(&first).into_owner_result().unwrap();
        let changed = snapshot(&[(9, "/main.rue", "main.rue", "fn main() {}")], 9);
        let update = session.update(&changed);
        let work = update.work();
        assert_eq!(work.modules_considered, 1);
        assert_eq!(work.modules_rebound, 1);
        assert_eq!(work.modules_reparsed, 0);
        assert_eq!(work.syntax, SyntaxWork::default());
        let changed = update.into_owner_result().unwrap();
        assert_eq!(
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
        let mut session = CompilerSession::new();
        let first_snapshot = large(false);
        let first = session.update(&first_snapshot).into_owner_result().unwrap();
        let edited = large(true);
        let update = session.update(&edited);
        let work = update.work();
        let second = update.into_owner_result().unwrap();
        assert_eq!(work.modules_reused, 128);
        assert_eq!(work.modules_rebound, 0);
        assert_eq!(work.modules_reparsed, 1);
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
        let mut session = CompilerSession::new();
        session.update(&good).into_owner_result().unwrap();
        let broken = snapshot(
            &[
                (1, "/z.rue", "z.rue", "fn zed( {"),
                (2, "/a.rue", "a.rue", "fn alpha() {}"),
            ],
            2,
        );
        let errors = session.update(&broken).into_owner_result().unwrap_err();
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
        let errors = session
            .update(&both_broken)
            .into_owner_result()
            .unwrap_err();
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
        let (new_main, work) = parse_source_snapshot_module(&edited, &main_id);
        let new_main = new_main.unwrap();
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
    fn parser_owned_import_sites_cover_nested_and_type_positions() {
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
    fn repeated_session_update_reuses_exact_arcs_and_publishes_send_sync_result() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ParseInvalidationSummary>();
        assert_send_sync::<ParsedProgram>();

        let source = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let mut session = CompilerSession::new();
        let first = session.update(&source).into_owner_result().unwrap();
        let second_update = session.update(&source);
        let work = second_update.work();
        let second = second_update.into_owner_result().unwrap();

        // Re-updating the same snapshot reselects the published parse terminal
        // outright, so the whole program — not just each module — is retained
        // and no structural work is performed at all.
        assert_eq!(work, ParsedModulesWork::default());
        assert!(Arc::ptr_eq(&first, &second));
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
    fn session_update_one_edit_among_128_parses_once() {
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
        let mut session = CompilerSession::new();
        session.update(&make(false)).into_owner_result().unwrap();
        let update = session.update(&make(true));
        let work = update.work();

        assert_eq!(work.previous_module_lookups, 128);
        assert_eq!(work.modules_reused, 127);
        assert_eq!(work.modules_reparsed, 1);
        assert_eq!(work.syntax.lexer_invocations, 1);
        assert_eq!(work.syntax.parser_invocations, 1);
        assert_eq!(update.invalidation().exact_reused.len(), 127);
        assert_eq!(update.invalidation().reparsed.len(), 1);
    }

    #[test]
    fn session_update_distinguishes_relocation_file_ids_and_stable_renames() {
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
        let mut session = CompilerSession::new();
        session.update(&base).into_owner_result().unwrap();
        let moved = session.update(&relocated);
        assert_eq!(moved.work().modules_rebound, 2);
        assert_eq!(moved.invalidation().payload_rebound.len(), 2);
        moved.into_owner_result().unwrap();
        // Only the FileId epoch moves, so module identity and content keep the
        // canonical parse query cached and only the envelopes are rebound.
        let ids = session.update(&reassigned);
        assert_eq!(ids.work().modules_rebound, 2);
        assert_eq!(ids.invalidation().reparsed.len(), 2);

        let renamed = snapshot(
            &[
                (11, "/new/a2.rue", "a2.rue", "fn a() {}"),
                (13, "/new/c.rue", "c.rue", "fn c() {}"),
            ],
            11,
        );
        ids.into_owner_result().unwrap();
        let update = session.update(&renamed);
        assert_eq!(update.work().modules_reparsed, 2);
        assert_eq!(update.invalidation().added.len(), 2);
        assert_eq!(update.invalidation().removed.len(), 2);
        assert!(update.invalidation().payload_rebound.is_empty());
    }

    #[test]
    fn caller_keeps_successful_parse_baseline_for_recovery() {
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
        let mut session = CompilerSession::new();
        session.update(&good).into_owner_result().unwrap();
        let failed = session.update(&broken);
        assert!(failed.result_owner().is_err());
        assert_eq!(failed.work().modules_reused, 2);

        let recovered = session.update(&recovered);
        assert_eq!(recovered.work().modules_reused, 2);
        assert_eq!(recovered.work().modules_reparsed, 1);
        recovered.into_owner_result().unwrap();
    }

    /// Two independent sessions parsing the same broken snapshot publish the
    /// same diagnostics in the same canonical order.
    #[test]
    fn independent_sessions_keep_canonical_error_order() {
        let broken = snapshot(
            &[
                (9, "/z.rue", "z.rue", "fn z( {"),
                (2, "/a.rue", "a.rue", "fn a() { # }"),
            ],
            2,
        );
        let first = parse_source_snapshot_modules(&broken).unwrap_err();
        let second = parse_source_snapshot_modules(&broken).unwrap_err();
        assert_eq!(error_fingerprint(&first), error_fingerprint(&second));
    }

    /// Exact-snapshot membership is physical, not just revision-deep: a
    /// relocated snapshot shares the source revision but does not own the
    /// program parsed from the original locations.
    #[test]
    fn exact_snapshot_membership_rejects_relocated_snapshot() {
        let original = snapshot(&[(1, "/a.rue", "a.rue", "fn a() {}")], 1);
        let relocated = snapshot(&[(9, "/moved/a.rue", "a.rue", "fn a() {}")], 9);
        assert_eq!(original.source_revision(), relocated.source_revision());
        let parsed = parse_source_snapshot_modules(&original).unwrap();
        assert!(parsed.belongs_to_exact_snapshot(&original));
        assert!(!parsed.belongs_to_exact_snapshot(&relocated));
    }
}
