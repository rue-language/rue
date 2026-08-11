//! Self-contained immutable parsed-module artifacts.
//!
//! This is the reuse-safe syntax boundary. Each module owns its parser symbol
//! universe, while [`ParsedProgram`] provides the sole parsed-program
//! representation used by semantic compilation.

use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
use lasso::Key;
use lasso::{RodeoResolver, Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileErrors, CompileResult, ErrorKind};
use rue_parser::{
    AssignTarget, Ast, Expr, IntrinsicArg, Item, Pattern, Statement, TypeExpr, ast::Visibility,
};
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
        DeclarationShellFact, DeclarationShellFailure, RawAnonymousSite, RawConstSyntax,
        RawDeclarationBodySyntax, RawDeclarationSignatureSyntax,
    },
};

/// Slice the module-relative anonymous type sites that fall inside `fragment`
/// into locators relative to `fragment`'s start. The frontend anchor rides
/// along unchanged; the relative offsets let the durable evaluator reconnect
/// each reparsed literal to it without a module-space lookup (RUE-1089).
///
/// A site straddling or preceding the fragment is dropped rather than clamped:
/// the durable evaluator then fails closed on the corresponding reparsed literal
/// (a loud missing-locator diagnostic) instead of adopting a truncated locator.
fn fragment_anonymous_sites(
    sites: &[rue_rir::AnonymousTypeSite],
    fragment: Span,
) -> Arc<[RawAnonymousSite]> {
    sites
        .iter()
        .filter(|site| site.span.start >= fragment.start && site.span.end <= fragment.end)
        .map(|site| RawAnonymousSite {
            fragment_start: site.span.start - fragment.start,
            fragment_end: site.span.end - fragment.start,
            kind: site.kind,
            anchor: site.anchor.clone(),
        })
        .collect::<Vec<_>>()
        .into()
}

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
    declaration_by_key: BTreeMap<DeclarationCandidateKey, usize>,
    declaration_capabilities: Arc<[DeclarationOccurrenceCapability]>,
    #[cfg(test)]
    raw_const_syntax_materializations: Arc<AtomicUsize>,
    #[cfg(test)]
    raw_declaration_signature_terminal_materializations: Arc<AtomicUsize>,
    #[cfg(test)]
    raw_declaration_body_terminal_materializations: Arc<AtomicUsize>,
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
            .saturating_add(declaration_capabilities);
        #[cfg(test)]
        let charge = {
            let atomics = 4_u64.saturating_mul(std::mem::size_of::<AtomicUsize>() as u64);
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
        candidate.raw_body_span.or_else(|| {
            candidate
                .raw_const_syntax_spans
                .map(|spans| spans.initializer)
        })
    }

    /// Select the parser-owned raw syntax for exactly one constant key.
    ///
    /// The declaration table is constructed once with the module. This lookup
    /// must remain an exact `declaration_by_key` lookup so a demand for one
    /// constant never projects or scans unrelated declarations.
    fn materialize_raw_const_syntax(
        &self,
        key: &DeclarationCandidateKey,
        source_text: &str,
    ) -> Option<RawConstSyntax> {
        let index = self.declaration_by_key.get(key).copied()?;
        let candidate = self.declarations.get(index)?;
        if candidate.fact.key != *key {
            return None;
        }
        let spans = candidate.raw_const_syntax_spans?;
        let fragment = |span: Span| {
            source_text
                .get(span.start as usize..span.end as usize)
                .map(Arc::from)
        };
        let syntax = RawConstSyntax {
            declared_type: match spans.declared_type {
                Some(span) => Some(fragment(span)?),
                None => None,
            },
            initializer: fragment(spans.initializer)?,
            anonymous_sites: fragment_anonymous_sites(
                &candidate.anonymous_sites,
                spans.initializer,
            ),
        };
        #[cfg(test)]
        self.raw_const_syntax_materializations
            .fetch_add(1, Ordering::Relaxed);
        Some(syntax)
    }

    #[cfg(test)]
    fn raw_const_syntax_materialization_count(&self) -> usize {
        self.raw_const_syntax_materializations
            .load(Ordering::Relaxed)
    }

    /// Materialize the body-free syntax for exactly one declaration key.
    /// Locators are indexed with the module, but source fragments are copied
    /// only after this exact `declaration_by_key` lookup succeeds.
    fn materialize_raw_declaration_signature(
        &self,
        key: &DeclarationCandidateKey,
        source_text: &str,
    ) -> Option<RawDeclarationSignatureSyntax> {
        let index = self.declaration_by_key.get(key).copied()?;
        let candidate = self.declarations.get(index)?;
        if candidate.fact.key != *key {
            return None;
        }
        let locator = candidate.raw_signature_locator?;
        let fragment = |span: Span| {
            source_text
                .get(span.start as usize..span.end as usize)
                .map(Arc::from)
        };
        let (declaration_fragments, extern_abi) = match locator {
            RawDeclarationSignatureLocator::Contiguous { declaration } => {
                (vec![fragment(declaration)?].into(), None)
            }
            RawDeclarationSignatureLocator::SplitStruct {
                retained_prefix,
                closing_brace,
            } => (
                vec![fragment(retained_prefix)?, fragment(closing_brace)?].into(),
                None,
            ),
            RawDeclarationSignatureLocator::Extern { declaration, abi } => {
                (vec![fragment(declaration)?].into(), Some(fragment(abi)?))
            }
        };
        // 6.6:7 lets an accessor yield through a nested *accessor* call. For a
        // link whose receiver is this accessor's own `self`, the callee is one
        // of the owner's methods, so the deciding fact is the sibling
        // declaration's parsed `-> borrow` qualifier — retained here, where
        // the terminal is already materialized from this module's parse, so an
        // edit to a sibling invalidates this accessor's signature.
        let accessor = if candidate.is_accessor {
            Some(Arc::new(
                crate::declaration_candidate::RawAccessorSignatureSyntax {
                    body: fragment(candidate.raw_body_span?)?,
                    owner_methods: key.owner.as_ref().map_or_else(
                        || Arc::from(Vec::new()),
                        |owner| self.owner_method_accessor_facts(owner),
                    ),
                },
            ))
        } else {
            None
        };
        let syntax = RawDeclarationSignatureSyntax {
            declaration_fragments,
            extern_abi,
            accessor,
        };
        #[cfg(test)]
        self.raw_declaration_signature_terminal_materializations
            .fetch_add(1, Ordering::Relaxed);
        Some(syntax)
    }

    /// Every method one owner declares in this module — its name, whether it
    /// is itself a `-> borrow` accessor, and its `self`-call targets — in the
    /// normalized form the raw signature terminal retains.
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

    #[cfg(test)]
    fn raw_declaration_signature_terminal_materialization_count(&self) -> usize {
        self.raw_declaration_signature_terminal_materializations
            .load(Ordering::Relaxed)
    }

    /// Materialize the syntax for exactly one body-bearing declaration key.
    /// The current-epoch span stays parser-private; only owned source text may
    /// cross the revisioned query boundary.
    fn materialize_raw_declaration_body(
        &self,
        key: &DeclarationCandidateKey,
        source_text: &str,
    ) -> Option<RawDeclarationBodySyntax> {
        let index = self.declaration_by_key.get(key).copied()?;
        let candidate = self.declarations.get(index)?;
        if candidate.fact.key != *key {
            return None;
        }
        let body = candidate.raw_body_span?;
        let syntax = RawDeclarationBodySyntax {
            body: Arc::from(source_text.get(body.start as usize..body.end as usize)?),
            anonymous_sites: fragment_anonymous_sites(&candidate.anonymous_sites, body),
        };
        #[cfg(test)]
        self.raw_declaration_body_terminal_materializations
            .fetch_add(1, Ordering::Relaxed);
        Some(syntax)
    }

    fn body_source_spans(&self, key: &DeclarationCandidateKey) -> Option<(Span, Span)> {
        let index = self.declaration_by_key.get(key).copied()?;
        let candidate = self.declarations.get(index)?;
        (candidate.fact.key == *key)
            .then_some((candidate.declaration_span, candidate.raw_body_span?))
    }

    #[cfg(test)]
    fn raw_declaration_body_terminal_materialization_count(&self) -> usize {
        self.raw_declaration_body_terminal_materializations
            .load(Ordering::Relaxed)
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
    declaration_span: Span,
    raw_const_syntax_spans: Option<RawConstSyntaxSpans>,
    raw_signature_locator: Option<RawDeclarationSignatureLocator>,
    raw_body_span: Option<Span>,
    is_accessor: bool,
    /// The method names this declaration's body calls on its own `self`
    /// receiver, normalized. Non-empty only for methods; the accessor
    /// signature terminal retains it as a sibling's 6.6:14 cycle edge
    /// (RUE-1282).
    self_call_targets: Arc<[Arc<str>]>,
    raw_import_range: Option<RawDeclarationImportRange>,
    /// Value-position anonymous type literals inside this declaration's constant
    /// initializer or body, with module-relative spans and their frontend
    /// anchors. Sliced into fragment-relative sites when the raw const/body
    /// terminal is materialized (RUE-1089).
    anonymous_sites: Arc<[rue_rir::AnonymousTypeSite]>,
}

/// Parser-private locators for syntax that is materialized only after an exact
/// declaration-key lookup. Keeping these current-epoch spans private also
/// leaves diagnostic projection independent from the durable query terminal.
#[derive(Debug, Clone, Copy)]
struct RawConstSyntaxSpans {
    declared_type: Option<Span>,
    initializer: Span,
}

/// Parser-private signature locators. These current-epoch spans are also the
/// only source of future diagnostic projection; they never enter the durable
/// raw-signature terminal.
#[derive(Debug, Clone, Copy)]
enum RawDeclarationSignatureLocator {
    Contiguous {
        declaration: Span,
    },
    SplitStruct {
        retained_prefix: Span,
        closing_brace: Span,
    },
    Extern {
        declaration: Span,
        abi: Span,
    },
}

/// Parser-private range into the module's source-ordered import table for one
/// declaration. Runtime query values retain neither field.
#[derive(Debug, Clone, Copy)]
struct RawDeclarationImportRange {
    start: u32,
    len: u32,
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
    pub(crate) fn retained_allocation_charge(&self) -> u64 {
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

    pub(crate) fn evaluate_raw_const_syntax(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<RawConstSyntax> {
        self.definitions
            .materialize_raw_const_syntax(key, self.source_text())
    }

    #[cfg(test)]
    pub(crate) fn raw_const_syntax_materialization_count(&self) -> usize {
        self.definitions.raw_const_syntax_materialization_count()
    }

    pub(crate) fn evaluate_raw_declaration_signature(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<RawDeclarationSignatureSyntax> {
        self.definitions
            .materialize_raw_declaration_signature(key, self.source_text())
    }

    #[cfg(test)]
    pub(crate) fn raw_declaration_signature_terminal_materialization_count(&self) -> usize {
        self.definitions
            .raw_declaration_signature_terminal_materialization_count()
    }

    pub(crate) fn evaluate_raw_declaration_body(
        &self,
        key: &DeclarationCandidateKey,
    ) -> Option<RawDeclarationBodySyntax> {
        self.definitions
            .materialize_raw_declaration_body(key, self.source_text())
    }

    pub(crate) fn body_source_spans(&self, key: &DeclarationCandidateKey) -> Option<(Span, Span)> {
        self.definitions.body_source_spans(key)
    }

    #[cfg(test)]
    pub(crate) fn raw_declaration_body_terminal_materialization_count(&self) -> usize {
        self.definitions
            .raw_declaration_body_terminal_materialization_count()
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
    let import_sites = collect_imports(&ast, module, &interner)?;
    let resolver = Arc::new(interner.into_resolver());
    build_module_with_resolver(
        snapshot,
        file_id,
        ast,
        resolver,
        token,
        import_sites,
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
    import_sites: ImportSiteCollector,
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
        &import_sites.valid,
    )?;
    let payload = Arc::new(ParsedSyntaxPayload {
        source,
        source_text,
        token_count,
        tokens,
        ast: provenanced_ast,
        resolver,
        definitions,
        import_sites: import_sites.valid.into(),
        invalid_import_sites: import_sites.invalid.into(),
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
                declaration_span: remap_span(candidate.declaration_span),
                raw_const_syntax_spans: candidate.raw_const_syntax_spans.map(|spans| {
                    RawConstSyntaxSpans {
                        declared_type: spans.declared_type.map(remap_span),
                        initializer: remap_span(spans.initializer),
                    }
                }),
                raw_signature_locator: candidate.raw_signature_locator.map(
                    |locator| match locator {
                        RawDeclarationSignatureLocator::Contiguous { declaration } => {
                            RawDeclarationSignatureLocator::Contiguous {
                                declaration: remap_span(declaration),
                            }
                        }
                        RawDeclarationSignatureLocator::SplitStruct {
                            retained_prefix,
                            closing_brace,
                        } => RawDeclarationSignatureLocator::SplitStruct {
                            retained_prefix: remap_span(retained_prefix),
                            closing_brace: remap_span(closing_brace),
                        },
                        RawDeclarationSignatureLocator::Extern { declaration, abi } => {
                            RawDeclarationSignatureLocator::Extern {
                                declaration: remap_span(declaration),
                                abi: remap_span(abi),
                            }
                        }
                    },
                ),
                raw_body_span: candidate.raw_body_span.map(remap_span),
                is_accessor: candidate.is_accessor,
                self_call_targets: candidate.self_call_targets.clone(),
                raw_import_range: candidate.raw_import_range,
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
        declaration_capabilities: payload.definitions.declaration_capabilities.clone(),
        #[cfg(test)]
        raw_const_syntax_materializations: payload
            .definitions
            .raw_const_syntax_materializations
            .clone(),
        #[cfg(test)]
        raw_declaration_signature_terminal_materializations: payload
            .definitions
            .raw_declaration_signature_terminal_materializations
            .clone(),
        #[cfg(test)]
        raw_declaration_body_terminal_materializations: payload
            .definitions
            .raw_declaration_body_terminal_materializations
            .clone(),
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

fn collect_imports(
    ast: &Ast,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
) -> CompileResult<ImportSiteCollector> {
    let mut imports = ImportSiteCollector::default();
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
            Item::Extern(block) => {
                for foreign in &block.fns {
                    walk_signature(
                        &foreign.params,
                        foreign.return_type.as_ref(),
                        module,
                        resolver,
                        &mut imports,
                    )?;
                }
            }
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
    imports.valid.sort();
    imports
        .invalid
        .sort_by_key(|site| (site.span.file_id.index(), site.span.start));
    Ok(imports)
}

pub(crate) fn exact_syntax_import_sites(
    ast: &Ast,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
) -> CompileResult<Vec<ImportDirective>> {
    Ok(collect_imports(ast, module, resolver)?.valid)
}

fn walk_signature(
    params: &[rue_parser::ast::Param],
    return_type: Option<&TypeExpr>,
    module: &ModuleId,
    resolver: &ThreadedRodeo,
    imports: &mut ImportSiteCollector,
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
    imports: &mut ImportSiteCollector,
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
    imports: &mut ImportSiteCollector,
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
    imports: &mut ImportSiteCollector,
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
    imports: &mut ImportSiteCollector,
) -> CompileResult<()> {
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
        Expr::Yield(value) => walk_expr(&value.value, module, resolver, imports)?,
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
            if name == "import" {
                if let [IntrinsicArg::Expr(Expr::String(literal))] = value.args.as_slice() {
                    let specifier = resolver.try_resolve(&literal.value).ok_or_else(|| {
                        invalid_input("import literal is absent from the module symbol universe")
                    })?;
                    imports.valid.push(ImportDirective::new(
                        module.clone(),
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
                    imports
                        .invalid
                        .push(ParsedInvalidImportSite { span, shape });
                }
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

fn build_definition_index(
    module: ModuleId,
    file_id: FileId,
    source_text: &str,
    tokens: &[rue_lexer::Token],
    ast: &Ast,
    resolver: &FrozenSymbolResolver,
    import_sites: &[ImportDirective],
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
        Span,
        Option<RawConstSyntaxSpans>,
        Option<RawDeclarationSignatureLocator>,
        Option<Span>,
        bool,
        Arc<[Arc<str>]>,
        Option<RawDeclarationImportRange>,
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
                    Ok(DeclarationParameterHeader {
                        name: resolve_name(param.name)?,
                        mode,
                        is_comptime,
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
    for item in &ast.items {
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
                        raw_const_syntax_spans: Option<RawConstSyntaxSpans>,
                        raw_signature_locator: Option<RawDeclarationSignatureLocator>,
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
            if let Some(locator) = raw_signature_locator {
                let (first, second, abi) = match locator {
                    RawDeclarationSignatureLocator::Contiguous { declaration } => {
                        (declaration, None, None)
                    }
                    RawDeclarationSignatureLocator::SplitStruct {
                        retained_prefix,
                        closing_brace,
                    } => (retained_prefix, Some(closing_brace), None),
                    RawDeclarationSignatureLocator::Extern { declaration, abi } => {
                        (declaration, None, Some(abi))
                    }
                };
                validate_span("raw declaration signature", first, file_id, source_text)?;
                if let Some(span) = second {
                    validate_span("raw declaration signature", span, file_id, source_text)?;
                }
                if let Some(span) = abi {
                    validate_span("raw extern ABI", span, file_id, source_text)?;
                }
            }
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
                declaration_span,
                raw_const_syntax_spans,
                raw_signature_locator,
                raw_body_span,
                is_accessor,
                method_self_call_targets,
                raw_import_range,
                anonymous_sites,
            ));
            Ok(())
        };

        match item {
            Item::Function(function) => push(
                DeclarationCandidateCategory::Function,
                resolve_name(function.name)?,
                None,
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
                Some(RawDeclarationSignatureLocator::Contiguous {
                    declaration: token_bounded_signature_prefix(
                        function.span,
                        function.body.span(),
                        tokens,
                    )?,
                }),
                Some(function.body.span()),
                rue_rir::anonymous_type_sites(&function.body).into(),
            )?,
            Item::Struct(structure) => {
                let owner_name = resolve_name(structure.name)?;
                push(
                    DeclarationCandidateCategory::Struct,
                    owner_name.clone(),
                    None,
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
                    Some(struct_signature_locator(structure, tokens)?),
                    None,
                    Arc::from([]),
                )?;
                let owner = DeclarationCandidateOwner {
                    category: DeclarationCandidateCategory::Struct,
                    name: owner_name,
                };
                for method in &structure.methods {
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
                        Some(RawDeclarationSignatureLocator::Contiguous {
                            declaration: token_bounded_signature_prefix(
                                method.span,
                                method.body.span(),
                                tokens,
                            )?,
                        }),
                        Some(method.body.span()),
                        rue_rir::anonymous_type_sites(&method.body).into(),
                    )?;
                }
            }
            Item::Enum(value) => push(
                DeclarationCandidateCategory::Enum,
                resolve_name(value.name)?,
                None,
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
                Some(RawDeclarationSignatureLocator::Contiguous {
                    declaration: token_bounded_declaration(value.span, tokens)?,
                }),
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
                let raw_const_syntax_spans = RawConstSyntaxSpans {
                    declared_type,
                    initializer,
                };
                push(
                    DeclarationCandidateCategory::ConstCandidate,
                    resolve_name(value.name)?,
                    None,
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
                    Some(raw_const_syntax_spans),
                    None,
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
                Some(RawDeclarationSignatureLocator::Contiguous {
                    declaration: token_bounded_signature_prefix(
                        value.span,
                        value.body.span(),
                        tokens,
                    )?,
                }),
                Some(value.body.span()),
                rue_rir::anonymous_type_sites(&value.body).into(),
            )?,
            Item::Extern(block) => {
                for function in &block.fns {
                    push(
                        DeclarationCandidateCategory::ExternFunction,
                        resolve_name(function.name)?,
                        None,
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
                        Some(RawDeclarationSignatureLocator::Extern {
                            declaration: token_bounded_declaration(function.span, tokens)?,
                            abi: block.abi_span,
                        }),
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
        left.1
            .start
            .cmp(&right.1.start)
            .then(left.1.end.cmp(&right.1.end))
            .then(left.0.key.category.cmp(&right.0.key.category))
            .then(left.0.key.name.cmp(&right.0.key.name))
    });
    let mut duplicate_counts = BTreeMap::new();
    let declarations = pending_declarations
        .into_iter()
        .map(
            |(
                mut fact,
                declaration_span,
                raw_const_syntax_spans,
                raw_signature_locator,
                raw_body_span,
                is_accessor,
                self_call_targets,
                raw_import_range,
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
                    declaration_span,
                    raw_const_syntax_spans,
                    raw_signature_locator,
                    raw_body_span,
                    is_accessor,
                    self_call_targets,
                    raw_import_range,
                    anonymous_sites,
                })
            },
        )
        .collect::<CompileResult<Vec<_>>>()?;
    let mut declaration_by_key = BTreeMap::new();
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
    declaration_capabilities.sort_by(|left, right| left.key().cmp(right.key()));
    Ok(ParsedDefinitionIndex {
        candidates: candidates.into(),
        declarations: declarations.into(),
        declaration_by_key,
        declaration_capabilities: declaration_capabilities.into(),
        #[cfg(test)]
        raw_const_syntax_materializations: Arc::new(AtomicUsize::new(0)),
        #[cfg(test)]
        raw_declaration_signature_terminal_materializations: Arc::new(AtomicUsize::new(0)),
        #[cfg(test)]
        raw_declaration_body_terminal_materializations: Arc::new(AtomicUsize::new(0)),
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

/// Bound a body-bearing declaration at the last signature token. Lexer trivia
/// before the body belongs to neither the signature nor the body and therefore
/// must not perturb the durable signature terminal.
fn token_bounded_signature_prefix(
    declaration: Span,
    body: Span,
    tokens: &[rue_lexer::Token],
) -> CompileResult<Span> {
    signature_prefix(declaration, body)?;
    let signature_end = tokens
        .iter()
        .filter(|token| {
            token.span.file_id == declaration.file_id
                && token.span.start >= declaration.start
                && token.span.end <= body.start
                && !matches!(token.kind, rue_lexer::TokenKind::Eof)
        })
        .map(|token| token.span.end)
        .max()
        .ok_or_else(|| invalid_input("declaration signature contains no token before its body"))?;
    if signature_end <= declaration.start {
        return Err(invalid_input(
            "declaration signature token boundary is empty",
        ));
    }
    Ok(Span::with_file(
        declaration.file_id,
        declaration.start,
        signature_end,
    ))
}

/// Trim a body-free declaration to its first and last tokens.
fn token_bounded_declaration(
    declaration: Span,
    tokens: &[rue_lexer::Token],
) -> CompileResult<Span> {
    let mut declaration_tokens = tokens.iter().filter(|token| {
        token.span.file_id == declaration.file_id
            && token.span.start >= declaration.start
            && token.span.end <= declaration.end
            && !matches!(token.kind, rue_lexer::TokenKind::Eof)
    });
    let first = declaration_tokens
        .next()
        .ok_or_else(|| invalid_input("declaration contains no tokens"))?;
    let last = declaration_tokens.next_back().unwrap_or(first);
    Ok(Span::with_file(
        declaration.file_id,
        first.span.start,
        last.span.end,
    ))
}

/// Retain only a struct's header/directives/fields and its closing-brace token.
/// The two inline spans exclude the delimiter and all trivia before the first
/// method, all methods, and all trivia after the last method. Omitting a
/// trailing field comma is valid Rue syntax, so concatenating these fragments
/// remains a deterministic, reparsable struct declaration.
fn struct_signature_locator(
    structure: &rue_parser::ast::StructDecl,
    tokens: &[rue_lexer::Token],
) -> CompileResult<RawDeclarationSignatureLocator> {
    let declaration_tokens = || {
        tokens.iter().filter(|token| {
            token.span.file_id == structure.span.file_id
                && token.span.start >= structure.span.start
                && token.span.end <= structure.span.end
                && !matches!(token.kind, rue_lexer::TokenKind::Eof)
        })
    };
    let first = declaration_tokens()
        .next()
        .ok_or_else(|| invalid_input("struct declaration contains no tokens"))?;
    let opening_brace = declaration_tokens()
        .find(|token| matches!(token.kind, rue_lexer::TokenKind::LBrace))
        .ok_or_else(|| invalid_input("struct declaration has no opening-brace token"))?;
    let closing_brace = declaration_tokens()
        .rev()
        .find(|token| matches!(token.kind, rue_lexer::TokenKind::RBrace))
        .ok_or_else(|| invalid_input("struct declaration has no closing-brace token"))?;
    let retained_end = if let Some(field) = structure.fields.last() {
        let field_end_is_token_boundary =
            declaration_tokens().any(|token| token.span.end == field.span.end);
        if !field_end_is_token_boundary {
            return Err(invalid_input(
                "struct field does not end at a token boundary",
            ));
        }
        field.span.end
    } else {
        opening_brace.span.end
    };
    if first.span.start >= retained_end || retained_end > closing_brace.span.start {
        return Err(invalid_input(
            "struct retained signature tokens are not ordered before its closing brace",
        ));
    }
    Ok(RawDeclarationSignatureLocator::SplitStruct {
        retained_prefix: Span::with_file(structure.span.file_id, first.span.start, retained_end),
        closing_brace: closing_brace.span,
    })
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
