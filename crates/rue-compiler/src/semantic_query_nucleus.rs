//! Stable declaration-query identities and value projections.
//!
//! This module owns the interchange values between declaration/comptime
//! queries and the request-local AIR materializer. Values here deliberately
//! contain no parser, RIR, AIR, type-pool, source-position, or interner handles.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lasso::Spur;
use rue_air::declaration_validation::{
    AccessorBodyVerdict, AccessorExitForm, AccessorYieldRootForm,
};

use crate::retained_charge::RetainedCharge;

use crate::declaration_candidate::{
    DeclarationCandidateCategory as Category, DeclarationCandidateKey, DeclarationShellFact,
};
use crate::durable_semantics::{
    DurableAnonymousNominal, DurableConstValue, DurableDeclarationPayload,
    DurableSemanticParameter, DurableType,
};
use crate::{
    ModuleId, StableDefinitionKey, StableDefinitionKind as Kind,
    StableDefinitionNamespace as Namespace,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedSemanticParameter {
    pub(crate) name: ParsedSemanticText,
    pub(crate) mode: crate::declaration_candidate::DeclarationParameterMode,
    pub(crate) is_comptime: bool,
    pub(crate) ty: ParsedSemanticText,
}

/// One string inside a declaration signature's compact text envelope.
///
/// The range is candidate-local and position-independent. It never indexes the
/// source file: the producer copies only the semantic spellings that consumers
/// still need until the structured-type tranche removes those spellings too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedSemanticText {
    pub(crate) start: u32,
    pub(crate) end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedSemanticField {
    pub(crate) name: ParsedSemanticText,
    pub(crate) ty: ParsedSemanticText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedSemanticVariant {
    pub(crate) name: ParsedSemanticText,
    pub(crate) payload_start: u32,
    pub(crate) payload_end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedSemanticSignature {
    Callable {
        text: Arc<str>,
        parameters: Arc<[ParsedSemanticParameter]>,
        result: ParsedSemanticText,
        has_self: bool,
        self_mode: crate::declaration_candidate::DeclarationParameterMode,
        is_unchecked: bool,
        is_extern: bool,
        is_c_export: bool,
        is_accessor: bool,
        /// What the accessor body's own syntax decides about spec 6.6:6 and
        /// 6.6:7, in the producer-neutral vocabulary every accessor producer
        /// shares (RUE-1232). Meaningful only when `is_accessor`; an ordinary
        /// signature retains no body and always reports
        /// [`AccessorBodyVerdict::MissingTrailingYield`] for the empty
        /// stand-in.
        accessor_body: AccessorBodyVerdict,
        /// `Some(name)` when this accessor participates in a 6.6:14 expansion
        /// cycle over its owner's `self`-receiver accessor calls (RUE-1282),
        /// decided from the retained owner-method facts. Always `None` for an
        /// ordinary callable.
        accessor_cycle: Option<Arc<str>>,
    },
    Struct {
        text: Arc<str>,
        fields: Arc<[ParsedSemanticField]>,
        is_copy: bool,
        is_linear: bool,
        is_repr_c: bool,
    },
    Enum {
        text: Arc<str>,
        variants: Arc<[ParsedSemanticVariant]>,
        payloads: Arc<[ParsedSemanticText]>,
    },
    Destructor,
}

impl ParsedSemanticSignature {
    pub(crate) fn text(&self, value: ParsedSemanticText) -> &str {
        let text = match self {
            Self::Callable { text, .. } | Self::Struct { text, .. } | Self::Enum { text, .. } => {
                text
            }
            Self::Destructor => return "",
        };
        text.get(value.start as usize..value.end as usize)
            .expect("signature text ranges are validated when projected")
    }

    pub(crate) fn callable_type_syntax(&self) -> Option<rue_air::DurableCallableTypeSyntax> {
        let Self::Callable {
            parameters, result, ..
        } = self
        else {
            return None;
        };
        Some(rue_air::DurableCallableTypeSyntax {
            parameters: parameters
                .iter()
                .map(|parameter| Arc::from(self.text(parameter.ty)))
                .collect(),
            result: Arc::from(self.text(*result)),
        })
    }
}

fn source_fragment(source: &str, span: rue_span::Span) -> Result<&str, Arc<str>> {
    source
        .get(span.start as usize..span.end as usize)
        .ok_or_else(|| Arc::from("semantic signature contains an invalid local span"))
}

#[derive(Default)]
struct ParsedSemanticTextBuilder {
    text: String,
}

impl ParsedSemanticTextBuilder {
    fn push(&mut self, value: &str) -> Result<ParsedSemanticText, Arc<str>> {
        let start = u32::try_from(self.text.len())
            .map_err(|_| Arc::from("semantic signature text exceeds the supported size"))?;
        let end = self
            .text
            .len()
            .checked_add(value.len())
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| Arc::from("semantic signature text exceeds the supported size"))?;
        self.text.push_str(value);
        Ok(ParsedSemanticText { start, end })
    }

    fn finish(self) -> Arc<str> {
        Arc::from(self.text)
    }
}

fn parameter_mode(
    mode: rue_parser::ast::ParamMode,
) -> (crate::declaration_candidate::DeclarationParameterMode, bool) {
    use crate::declaration_candidate::DeclarationParameterMode as M;
    match mode {
        rue_parser::ast::ParamMode::Normal => (M::Value, false),
        rue_parser::ast::ParamMode::Borrow => (M::Borrow, false),
        rue_parser::ast::ParamMode::Inout => (M::Inout, false),
        rue_parser::ast::ParamMode::Comptime => (M::Value, true),
    }
}

fn parsed_parameters<'a>(
    text: &mut ParsedSemanticTextBuilder,
    source: &str,
    resolve: impl Copy + Fn(Spur) -> &'a str,
    parameters: &[rue_parser::ast::Param],
) -> Result<Arc<[ParsedSemanticParameter]>, Arc<str>> {
    parameters
        .iter()
        .map(|parameter| {
            let (mode, is_comptime) = parameter_mode(parameter.mode);
            Ok(ParsedSemanticParameter {
                name: text.push(resolve(parameter.name.name))?,
                mode,
                is_comptime,
                ty: text.push(source_fragment(source, parameter.ty.span())?)?,
            })
        })
        .collect::<Result<Vec<_>, Arc<str>>>()
        .map(Into::into)
}

/// The `yield` an accessor body falls through to: the body itself, the
/// block's final expression, or its final statement (spec 6.6:6).
fn trailing_yield(body: &rue_parser::ast::Expr) -> Option<&rue_parser::ast::YieldExpr> {
    use rue_parser::ast::{Expr, Statement};
    match body {
        Expr::Yield(exit) => Some(exit),
        Expr::Block(block) => match (block.expr.as_ref(), block.statements.last()) {
            (Expr::Yield(exit), _) | (_, Some(Statement::Expr(Expr::Yield(exit)))) => Some(exit),
            _ => None,
        },
        _ => None,
    }
}

/// Normalize one owner's method facts into the form
/// [`crate::declaration_candidate::RawAccessorSignatureSyntax`] retains:
/// sorted by name, with every ambiguously duplicated name dropped.
///
/// A duplicate method is its own diagnostic, and 6.6:7 stays permissive
/// wherever it cannot *prove* a callee is a plain method, so a name that two
/// declarations claim is simply not decided here.
pub(crate) fn owner_method_accessor_facts(
    methods: impl IntoIterator<Item = rue_air::declaration_validation::AccessorOwnerMethod>,
) -> Arc<[rue_air::declaration_validation::AccessorOwnerMethod]> {
    let mut facts: Vec<rue_air::declaration_validation::AccessorOwnerMethod> =
        methods.into_iter().collect();
    facts.sort();
    let mut duplicated: BTreeSet<Arc<str>> = BTreeSet::new();
    for window in facts.windows(2) {
        if window[0].name == window[1].name {
            duplicated.insert(window[0].name.clone());
        }
    }
    facts.dedup_by(|left, right| left.name == right.name);
    facts.retain(|fact| !duplicated.contains(&fact.name));
    Arc::from(facts)
}

/// The method names one body calls on its own `self` receiver, normalized
/// (sorted, deduplicated) — the 6.6:14 cycle-edge fact (RUE-1282), for a
/// producer holding the parsed AST and its interner.
pub(crate) fn ast_self_call_targets(
    body: &rue_parser::ast::Expr,
    interner: &crate::ThreadedRodeo,
) -> Arc<[Arc<str>]> {
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
                targets.push(Arc::from(interner.resolve(&call.method.name)));
            }
        }
        expr.child_exprs(&mut walk);
    }
    targets.sort();
    targets.dedup();
    Arc::from(targets)
}

/// What one method-call link in a yielded projection chain is, as far as the
/// declaration's own parsed neighborhood can prove (spec 6.6:7).
///
/// Only a link applied directly to the receiver is decidable here: `self.m()`
/// calls a method of this accessor's owner, so `owner_methods` names it. A
/// receiver that is anything else — a chained call, a field, an index — has a
/// type this query does not resolve, and a callee this query must not guess.
fn accessor_method_link<'a>(
    call: &rue_parser::ast::MethodCallExpr,
    resolve: impl Copy + Fn(Spur) -> &'a str,
    owner_methods: &[rue_air::declaration_validation::AccessorOwnerMethod],
) -> rue_air::declaration_validation::AccessorMethodLink {
    use rue_air::declaration_validation::AccessorMethodLink as Link;
    use rue_parser::ast::Expr;
    let mut receiver = &*call.receiver;
    while let Expr::Paren(paren) = receiver {
        receiver = &paren.inner;
    }
    if !matches!(receiver, Expr::SelfExpr(_)) {
        return Link::Unresolved;
    }
    let name = resolve(call.method.name);
    match owner_methods
        .iter()
        .find(|method| method.name.as_ref() == name)
    {
        Some(method) if method.is_accessor => Link::Accessor,
        Some(_) => Link::PlainMethod,
        // A name the owner does not declare is not this rule's error to
        // report; call resolution names it.
        None => Link::Unresolved,
    }
}

/// Decide 6.6:6 and 6.6:7 from an accessor body's own syntax.
///
/// Both rules are containment and shape questions — "the final statement is a
/// `yield`, no other `yield` may appear, ... the body **MUST NOT** contain
/// `return` or `?`", and "the operand ... **MUST** be a place rooted at the
/// receiver parameter" — so an accessor the program merely declares is
/// judged exactly like one something calls (RUE-1212). Containment is read
/// off the syntax, which is why a `return` in a branch a specialization would
/// later prune still counts: 6.6:6 forbids the form appearing in the body.
///
/// This function owns the *AST walk* only. Which forms are illegal, and how
/// each one reads, are `rue_air::declaration_validation`'s, shared with the
/// RIR producers (RUE-1232) — the walks differ because the representations do,
/// but the rules cannot.
///
/// One link of 6.6:7 is not syntactic. A projection chain may pass through a
/// nested accessor call, and whether the callee is an accessor or a plain
/// method is a resolved-callee question. `owner_methods` closes the part of it
/// that is a *parsed* fact: a link whose receiver is the accessor's own `self`
/// calls a method of the owner, and whether that method is an accessor is on
/// the owner's other declarations (RUE-1232). Every other link — `self.a().b()`,
/// `self.field.m()` — targets a type this query cannot name, so the walk stays
/// permissive and leaves the rejection to the demanded path, which has the
/// receiver's type in hand.
fn accessor_body_shape<'a>(
    body: &rue_parser::ast::Expr,
    resolve: impl Copy + Fn(Spur) -> &'a str,
    owner_methods: &[rue_air::declaration_validation::AccessorOwnerMethod],
) -> AccessorBodyVerdict {
    use rue_parser::ast::Expr;
    let Some(trailing) = trailing_yield(body) else {
        return AccessorBodyVerdict::MissingTrailingYield;
    };
    // The walk order is a stack, so an offending exit is kept only when it
    // starts earlier in the body than any already found: both producers name
    // the *first* illegal form in the source.
    let mut pending = vec![body];
    let mut first_exit: Option<(AccessorExitForm, u32)> = None;
    while let Some(expr) = pending.pop() {
        let exit = match expr {
            // Occurrences are distinguished by span: the canonical parsed
            // body holds each `yield` exactly once.
            Expr::Yield(exit) if exit.span != trailing.span => AccessorExitForm::SecondYield,
            Expr::Return(_) => AccessorExitForm::Return,
            Expr::Try(_) => AccessorExitForm::Try,
            _ => {
                expr.child_exprs(&mut pending);
                continue;
            }
        };
        let start = expr.span().start;
        if first_exit.is_none_or(|(_, earliest)| start < earliest) {
            first_exit = Some((exit, start));
        }
    }
    if let Some((exit, _)) = first_exit {
        return AccessorBodyVerdict::OtherExit(exit);
    }
    let mut current = trailing.value.as_ref();
    loop {
        let root = match current {
            Expr::SelfExpr(_) => return AccessorBodyVerdict::WellFormed,
            Expr::Paren(paren) => {
                current = &paren.inner;
                continue;
            }
            Expr::Field(field) => {
                current = &field.base;
                continue;
            }
            Expr::Index(index) => {
                current = &index.base;
                continue;
            }
            Expr::MethodCall(call) => {
                let link = accessor_method_link(call, resolve, owner_methods);
                if rue_air::declaration_validation::accessor_method_link_error(link).is_some() {
                    return AccessorBodyVerdict::YieldNotReceiverRooted(
                        AccessorYieldRootForm::PlainMethod,
                    );
                }
                current = &call.receiver;
                continue;
            }
            Expr::Ident(name) => AccessorYieldRootForm::Named(Arc::from(resolve(name.name))),
            _ => AccessorYieldRootForm::Value,
        };
        return AccessorBodyVerdict::YieldNotReceiverRooted(root);
    }
}

/// Project one exact declaration signature from the canonical parsed module.
///
/// This borrows parser state only while building the position-independent
/// value projection. No parser node, parser interner, FileId, or absolute span
/// enters the returned value; type fragments remain the existing deferred
/// semantic-type syntax until the structured-type tranche replaces them.
pub(crate) fn project_semantic_signature(
    module: &crate::parsed_modules::ParsedModule,
    key: &DeclarationCandidateKey,
) -> Result<ParsedSemanticSignature, Arc<str>> {
    use crate::parsed_modules::ParsedDeclarationAstRef;

    if key.category == Category::ConstCandidate {
        return Err(Arc::from(
            "constant candidates have no signature projection",
        ));
    }
    let declaration = module
        .declaration_ast(key)
        .ok_or_else(|| Arc::from("semantic signature has no exact parsed declaration"))?;
    let source = module.source_text();
    let resolve = |symbol| module.resolve_raw_symbol(symbol);
    let owner_methods = module
        .declaration_accessor_owner_methods(key)
        .ok_or_else(|| Arc::from("semantic signature has no exact accessor facts"))?;
    let accessor_cycle =
        rue_air::declaration_validation::accessor_self_call_cycle(&key.name, &owner_methods)
            .then(|| key.name.clone());
    let callable = |parameters: &[rue_parser::ast::Param],
                    result: Option<&rue_parser::ast::TypeExpr>,
                    has_self,
                    self_mode,
                    is_unchecked,
                    is_extern,
                    is_c_export,
                    is_accessor,
                    body: Option<&rue_parser::ast::Expr>|
     -> Result<ParsedSemanticSignature, Arc<str>> {
        let mut text = ParsedSemanticTextBuilder::default();
        let parameters = parsed_parameters(&mut text, source, resolve, parameters)?;
        let result = text.push(match result {
            Some(value) => source_fragment(source, value.span())?,
            None => "()",
        })?;
        Ok(ParsedSemanticSignature::Callable {
            text: text.finish(),
            parameters,
            result,
            has_self,
            self_mode,
            is_unchecked,
            is_extern,
            is_c_export,
            is_accessor,
            accessor_body: if is_accessor {
                body.map_or(AccessorBodyVerdict::MissingTrailingYield, |body| {
                    accessor_body_shape(body, resolve, &owner_methods)
                })
            } else {
                AccessorBodyVerdict::MissingTrailingYield
            },
            accessor_cycle: is_accessor.then(|| accessor_cycle.clone()).flatten(),
        })
    };

    match declaration {
        ParsedDeclarationAstRef::Function(function) => callable(
            &function.params,
            function.return_type.as_ref(),
            false,
            crate::declaration_candidate::DeclarationParameterMode::Value,
            function.is_unchecked,
            false,
            function.export_abi.is_some(),
            function.borrow_return.is_some(),
            Some(&function.body),
        ),
        ParsedDeclarationAstRef::ExternFunction { function } => callable(
            &function.params,
            function.return_type.as_ref(),
            false,
            crate::declaration_candidate::DeclarationParameterMode::Value,
            false,
            true,
            false,
            false,
            None,
        ),
        ParsedDeclarationAstRef::Method { method, .. } => callable(
            &method.params,
            method.return_type.as_ref(),
            method.receiver.is_some(),
            method.receiver.as_ref().map_or(
                crate::declaration_candidate::DeclarationParameterMode::Value,
                |receiver| parameter_mode(receiver.mode).0,
            ),
            false,
            false,
            false,
            method.borrow_return.is_some(),
            Some(&method.body),
        ),
        ParsedDeclarationAstRef::Struct(structure) => {
            let mut text = ParsedSemanticTextBuilder::default();
            let fields = structure
                .fields
                .iter()
                .map(|field| {
                    Ok(ParsedSemanticField {
                        name: text.push(resolve(field.name.name))?,
                        ty: text.push(source_fragment(source, field.ty.span())?)?,
                    })
                })
                .collect::<Result<Vec<_>, Arc<str>>>()?;
            Ok(ParsedSemanticSignature::Struct {
                text: text.finish(),
                fields: fields.into(),
                is_copy: structure
                    .directives
                    .iter()
                    .any(|directive| resolve(directive.name.name) == "copy"),
                is_linear: structure.is_linear,
                is_repr_c: structure.directives.iter().any(|directive| {
                    resolve(directive.name.name) == "repr"
                        && directive.args.iter().any(|argument| match argument {
                            rue_parser::ast::DirectiveArg::Ident(argument) => {
                                resolve(argument.name) == "c"
                            }
                        })
                }),
            })
        }
        ParsedDeclarationAstRef::Enum(value) => {
            let mut text = ParsedSemanticTextBuilder::default();
            let mut payloads = Vec::new();
            let variants = value
                .variants
                .iter()
                .map(|variant| {
                    let payload_start = u32::try_from(payloads.len()).map_err(|_| {
                        Arc::from("semantic signature payload exceeds the supported size")
                    })?;
                    for ty in &variant.payload {
                        payloads.push(text.push(source_fragment(source, ty.span())?)?);
                    }
                    let payload_end = u32::try_from(payloads.len()).map_err(|_| {
                        Arc::from("semantic signature payload exceeds the supported size")
                    })?;
                    Ok(ParsedSemanticVariant {
                        name: text.push(resolve(variant.name.name))?,
                        payload_start,
                        payload_end,
                    })
                })
                .collect::<Result<Vec<_>, Arc<str>>>()?;
            Ok(ParsedSemanticSignature::Enum {
                text: text.finish(),
                variants: variants.into(),
                payloads: payloads.into(),
            })
        }
        ParsedDeclarationAstRef::Destructor(_) => Ok(ParsedSemanticSignature::Destructor),
        ParsedDeclarationAstRef::Const(_) => Err(Arc::from(
            "constant candidates have no signature projection",
        )),
    }
}

/// One anonymous type literal transported into a reparsed declaration fragment,
/// located in the fragment's own `FileId(0)` synthetic-source coordinate space
/// and carrying the frontend anchor `AstGen` minted for it. The span is a
/// transport locator only; it never enters a durable semantic fingerprint.
#[derive(Debug, Clone)]
pub(crate) struct TransportedAnonymousSite {
    pub(crate) span: rue_span::Span,
    pub(crate) kind: rue_rir::AnonymousTypeSiteKind,
    pub(crate) anchor: rue_rir::RirStructuralAnchor,
}

pub(crate) struct ParsedSemanticConst {
    pub(crate) source: String,
    pub(crate) fragment_start: u32,
    pub(crate) declaration: rue_parser::ast::ConstDecl,
    pub(crate) interner: crate::ThreadedRodeo,
    pub(crate) import_sites: Vec<crate::ImportDirective>,
    pub(crate) anonymous_sites: TransportedAnonymousSites,
}

pub(crate) struct ParsedSemanticBody {
    pub(crate) source: String,
    pub(crate) fragment_start: u32,
    pub(crate) expression: rue_parser::ast::Expr,
    pub(crate) interner: crate::ThreadedRodeo,
    pub(crate) import_sites: Vec<crate::ImportDirective>,
    pub(crate) anonymous_sites: TransportedAnonymousSites,
}

/// A validated, keyed index over the anonymous-type sites transported into one
/// reparsed producer fragment.
///
/// Well-formedness — no two sites sharing a fragment-local locator, no two
/// sharing an anchor — is checked exactly ONCE, when the fragment is parsed and
/// this index is built (`parse_semantic_const`/`parse_semantic_body`). Every
/// eval-time lookup is then an O(log S) keyed probe with no revalidation, and
/// the produced-nominal cross-check consults [`Self::authorizes`] in O(log S).
/// This replaces the former per-lookup O(S²) whole-table rescan that made a
/// producer with S sites cost O(S³) to reduce (RUE-1089, Theme 5).
#[derive(Debug, Clone, Default)]
pub(crate) struct TransportedAnonymousSites {
    by_locator: BTreeMap<(u32, u32), TransportedAnonymousSite>,
    authorized_anchors: BTreeSet<rue_rir::RirStructuralAnchor>,
    /// The first well-formedness violation found at construction, if any. A
    /// malformed table is anchor-transport corruption; the evaluator promotes
    /// this to a fail-closed E9000 before any lookup resolves or any terminal
    /// publishes, so a corrupt table can never mint an identity.
    malformed: Option<Arc<str>>,
}

impl TransportedAnonymousSites {
    fn from_sites(sites: Vec<TransportedAnonymousSite>) -> Self {
        let mut by_locator: BTreeMap<(u32, u32), TransportedAnonymousSite> = BTreeMap::new();
        let mut authorized_anchors = BTreeSet::new();
        let mut malformed: Option<Arc<str>> = None;
        for site in sites {
            let locator = (site.span.start, site.span.end);
            if let Some(existing) = by_locator.get(&locator) {
                malformed.get_or_insert_with(|| {
                    Arc::from(format!(
                        "carries a duplicate anonymous-type locator {}..{} (anchors {:?} and {:?})",
                        locator.0, locator.1, existing.anchor, site.anchor,
                    ))
                });
                continue;
            }
            if !authorized_anchors.insert(site.anchor.clone()) {
                malformed.get_or_insert_with(|| {
                    Arc::from(format!(
                        "carries two distinct anonymous sites sharing the anchor {:?}",
                        site.anchor,
                    ))
                });
            }
            by_locator.insert(locator, site);
        }
        Self {
            by_locator,
            authorized_anchors,
            malformed,
        }
    }

    /// The first well-formedness violation, if the transported table is corrupt.
    pub(crate) fn malformed(&self) -> Option<&str> {
        self.malformed.as_deref()
    }

    /// The site transported for the anonymous literal at `span`, by exact
    /// fragment-local locator. `None` means no site was transported for it.
    pub(crate) fn resolve(&self, span: rue_span::Span) -> Option<&TransportedAnonymousSite> {
        self.by_locator.get(&(span.start, span.end))
    }

    /// Whether the transported table authorizes `anchor` — some transported site
    /// carries exactly it. The produced-nominal cross-check fails the producer
    /// terminal for any minted nominal whose anchor is not authorized here.
    pub(crate) fn authorizes(&self, anchor: &rue_rir::RirStructuralAnchor) -> bool {
        self.authorized_anchors.contains(anchor)
    }
}

/// Shift each fragment-relative anonymous locator into the reparsed fragment's
/// synthetic-source coordinate space by the byte length of the synthetic prefix
/// preceding the reparsed initializer/body text, then build and validate the
/// keyed index once.
fn transport_anonymous_sites(
    sites: &[crate::declaration_candidate::RawAnonymousSite],
    prefix_len: usize,
    fragment_source: &str,
) -> TransportedAnonymousSites {
    let prefix = prefix_len as u32;
    #[cfg_attr(not(test), allow(unused_mut))]
    let mut transported: Vec<TransportedAnonymousSite> = sites
        .iter()
        .map(|site| TransportedAnonymousSite {
            span: rue_span::Span::new(prefix + site.fragment_start, prefix + site.fragment_end),
            kind: site.kind,
            anchor: site.anchor.clone(),
        })
        .collect();
    #[cfg(test)]
    inject_transport_faults(fragment_source, &mut transported);
    #[cfg(not(test))]
    let _ = fragment_source;
    TransportedAnonymousSites::from_sites(transported)
}

/// Test-only anchor-transport corruption (RUE-1089 acceptance criterion 7),
/// selected by a marker embedded in the reparsed fragment source. It corrupts
/// the transported table at construction exactly as a real transport bug would,
/// so the fail-closed validation is exercised without a real divergence. The
/// mode is fully determined by the fragment source, so it is race-free under
/// parallel test execution — no global state, no reset. The DIVERGENT mode is
/// injected at resolve time instead (it must diverge the RESOLVED anchor from
/// what this table authorizes), so it is absent here.
#[cfg(test)]
fn inject_transport_faults(source: &str, sites: &mut Vec<TransportedAnonymousSite>) {
    if source.contains("__RUE1089_FAULT_MISSING__") {
        sites.clear();
    } else if source.contains("__RUE1089_FAULT_DUPLICATE__") {
        let doubled = sites.clone();
        sites.extend(doubled);
    } else if source.contains("__RUE1089_FAULT_WRONG_KIND__") {
        for site in sites.iter_mut() {
            site.kind = match site.kind {
                rue_rir::AnonymousTypeSiteKind::Struct => rue_rir::AnonymousTypeSiteKind::Enum,
                rue_rir::AnonymousTypeSiteKind::Enum => rue_rir::AnonymousTypeSiteKind::Struct,
            };
        }
    }
}

/// Reparse one exact declaration body without consulting its module AST. The
/// synthetic function supplies only the parser context; the retained body is
/// byte-for-byte the producer terminal and all semantic lookup remains keyed
/// by the original declaration.
pub(crate) fn parse_semantic_body(
    key: &DeclarationCandidateKey,
    syntax: &crate::declaration_candidate::RawDeclarationBodySyntax,
) -> Result<ParsedSemanticBody, Arc<str>> {
    let source = format!("fn __semantic_body() {}", syntax.body);
    // The reparsed body text sits at this byte offset in the synthetic source;
    // shifting each fragment-relative anonymous locator by it lands the locator
    // in the reparsed AST's own `FileId(0)` coordinate space.
    let prefix_len = source.len() - syntax.body.len();
    let parsed = crate::syntax::parse_file(
        crate::SourceView::new("<semantic-body>", &source, rue_span::FileId::new(0)),
        crate::ThreadedRodeo::new(),
    );
    let ast = parsed
        .result
        .map_err(|errors| Arc::from(errors.to_string()))?;
    let Some(rue_parser::ast::Item::Function(function)) = ast.items.first() else {
        return Err(Arc::from("semantic body reparsed without a function"));
    };
    let import_sites =
        crate::parsed_modules::exact_syntax_import_sites(&ast, &key.module, &parsed.interner)
            .map_err(|error| Arc::from(error.to_string()))?;
    let anonymous_sites = transport_anonymous_sites(&syntax.anonymous_sites, prefix_len, &source);
    Ok(ParsedSemanticBody {
        source,
        fragment_start: u32::try_from(prefix_len)
            .map_err(|_| Arc::from("semantic body prefix exceeds span capacity"))?,
        expression: function.body.clone(),
        interner: parsed.interner,
        import_sites,
        anonymous_sites,
    })
}

pub(crate) fn parse_semantic_const(
    key: &DeclarationCandidateKey,
    syntax: &crate::declaration_candidate::RawConstSyntax,
) -> Result<ParsedSemanticConst, Arc<str>> {
    if key.category != Category::ConstCandidate {
        return Err(Arc::from("non-constant key requested constant syntax"));
    }
    let source = match &syntax.declared_type {
        Some(ty) => format!("const {}: {} = {};", key.name, ty, syntax.initializer),
        None => format!("const {} = {};", key.name, syntax.initializer),
    };
    // The initializer is placed verbatim, followed only by the trailing `;`, so
    // its start offset in the synthetic source is this prefix length. Shifting
    // each fragment-relative anonymous locator by it lands the locator in the
    // reparsed AST's own `FileId(0)` coordinate space.
    let prefix_len = source.len() - syntax.initializer.len() - 1;
    let parsed = crate::syntax::parse_file(
        crate::SourceView::new("<semantic-const>", &source, rue_span::FileId::new(0)),
        crate::ThreadedRodeo::new(),
    );
    let ast = parsed
        .result
        .map_err(|errors| Arc::from(errors.to_string()))?;
    let Some(rue_parser::ast::Item::Const(declaration)) = ast.items.first() else {
        return Err(Arc::from(
            "semantic const reparsed as a different declaration",
        ));
    };
    let import_sites =
        crate::parsed_modules::exact_syntax_import_sites(&ast, &key.module, &parsed.interner)
            .map_err(|error| Arc::from(error.to_string()))?;
    let anonymous_sites = transport_anonymous_sites(&syntax.anonymous_sites, prefix_len, &source);
    Ok(ParsedSemanticConst {
        source,
        fragment_start: u32::try_from(prefix_len)
            .map_err(|_| Arc::from("semantic const prefix exceeds span capacity"))?,
        declaration: declaration.clone(),
        interner: parsed.interner,
        import_sites,
        anonymous_sites,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SemanticQueryConfiguration {
    pub(crate) target: rue_target::Target,
    pub(crate) preview_features: crate::StablePreviewFeatures,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DeclarationSemanticQueryKey {
    pub(crate) declaration: DeclarationCandidateKey,
    pub(crate) configuration: SemanticQueryConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ComptimeCallQueryKey {
    pub(crate) declaration: DeclarationSemanticQueryKey,
    pub(crate) type_arguments: Arc<[(Arc<str>, DurableType)]>,
    pub(crate) value_arguments: Arc<[(Arc<str>, DurableConstValue)]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AnonymousNominalQueryKey {
    pub(crate) producer: DeclarationSemanticQueryKey,
    pub(crate) identity: crate::AnonymousNominalKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DeferredOwnershipQueryKey {
    pub(crate) producer: DeclarationSemanticQueryKey,
    pub(crate) gate: DeferredOwnershipGate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DeferredOwnershipGateKind {
    RequireDroppable,
    RequireTriviallyDroppable,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeferredOwnershipApplication {
    pub(crate) declaration: DeclarationCandidateKey,
    pub(crate) call_ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeferredOwnershipGateSource {
    pub(crate) declaration: DeclarationCandidateKey,
    pub(crate) start: u32,
    pub(crate) end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeferredOwnershipGate {
    pub(crate) kind: DeferredOwnershipGateKind,
    pub(crate) ty: DurableType,
    pub(crate) source: Arc<DeferredOwnershipGateSource>,
    pub(crate) application: Option<DeferredOwnershipApplication>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SemanticNucleusKey {
    Identity(DeclarationSemanticQueryKey),
    Signature(DeclarationSemanticQueryKey),
    NominalWellFormedness(DeclarationSemanticQueryKey),
    DeferredOwnership(DeferredOwnershipQueryKey),
    ConstResolution(DeclarationSemanticQueryKey),
    ComptimeCall(ComptimeCallQueryKey),
    AnonymousNominal(AnonymousNominalQueryKey),
    #[cfg(test)]
    EngineCycleProbe(DeclarationSemanticQueryKey),
}

impl SemanticNucleusKey {
    pub(crate) fn stable_identity(&self) -> String {
        match self {
            Self::Identity(key) => format!("identity:{}", key.stable_identity()),
            Self::Signature(key) => format!("signature:{}", key.stable_identity()),
            Self::NominalWellFormedness(key) => {
                format!("nominal-well-formed:{}", key.stable_identity())
            }
            Self::DeferredOwnership(key) => format!(
                "deferred-ownership:{}:{:?}",
                key.producer.stable_identity(),
                key.gate
            ),
            Self::ConstResolution(key) => format!("const:{}", key.stable_identity()),
            Self::ComptimeCall(key) => format!(
                "comptime:{}:{:?}:{:?}",
                key.declaration.stable_identity(),
                key.type_arguments,
                key.value_arguments
            ),
            Self::AnonymousNominal(key) => format!(
                "anonymous:{}:{:?}",
                key.producer.stable_identity(),
                key.identity,
            ),
            #[cfg(test)]
            Self::EngineCycleProbe(key) => {
                format!("engine-cycle-probe:{}", key.stable_identity())
            }
        }
    }

    pub(crate) fn declaration(&self) -> &DeclarationCandidateKey {
        match self {
            Self::Identity(key)
            | Self::Signature(key)
            | Self::NominalWellFormedness(key)
            | Self::ConstResolution(key) => &key.declaration,
            Self::DeferredOwnership(key) => &key.producer.declaration,
            Self::ComptimeCall(key) => &key.declaration.declaration,
            Self::AnonymousNominal(key) => &key.producer.declaration,
            #[cfg(test)]
            Self::EngineCycleProbe(key) => &key.declaration,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DuplicateDeclarationFailure {
    pub(crate) kind: rue_error::ErrorKind,
    pub(crate) first: DeclarationCandidateKey,
    pub(crate) duplicate: DeclarationCandidateKey,
}

/// One stable source site participating in a foreign-signature conflict.
///
/// The query result carries a durable declaration key rather than a span so it
/// remains reusable across revisions. Presentation resolves the key against
/// the current parsed module and orders the two sites by source position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignSignatureSite {
    pub(crate) declaration: DeclarationCandidateKey,
    pub(crate) signature: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignSignatureConflictFailure {
    pub(crate) symbol: Arc<str>,
    pub(crate) left: ForeignSignatureSite,
    pub(crate) right: ForeignSignatureSite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticNucleusFailure {
    Shell(Arc<str>),
    Syntax(Arc<str>),
    Resolution(Arc<str>),
    Diagnostic(rue_error::ErrorKind),
    DiagnosticAtParameter {
        kind: rue_error::ErrorKind,
        ordinal: u32,
    },
    DiagnosticAtDeclaration {
        kind: rue_error::ErrorKind,
        declaration: DeclarationCandidateKey,
    },
    DuplicateDeclaration {
        kind: rue_error::ErrorKind,
        first: DeclarationCandidateKey,
        duplicate: DeclarationCandidateKey,
    },
    DuplicateDeclarations(Arc<[DuplicateDeclarationFailure]>),
    ForeignSignatureConflict(ForeignSignatureConflictFailure),
    /// A diagnostic anchored within the producer fragment. Offsets are
    /// fragment-relative so the stable failure carries no revision-local span.
    DiagnosticAtProducerRange {
        kind: rue_error::ErrorKind,
        start: u32,
        end: u32,
    },
    OwnershipGate {
        kind: rue_error::ErrorKind,
        gate: DeferredOwnershipGate,
    },
    DiagnosticWithHelp {
        kind: rue_error::ErrorKind,
        help: Arc<str>,
    },
    DiagnosticWithNote {
        kind: rue_error::ErrorKind,
        note: Arc<str>,
    },
    SignatureReentry {
        signature: StableDefinitionKey,
        cycle: Arc<[Arc<str>]>,
    },
    Cycle(Arc<[Arc<str>]>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticNucleusValue {
    Identity(DeclarationIdentityProjection),
    Signature(ResolvedDeclarationSignature),
    NominalWellFormedness,
    DeferredOwnership,
    ConstResolution(ConstResolutionProjection),
    ComptimeCall(ComptimeCallProjection),
    AnonymousNominal(DurableAnonymousNominal),
    Failure(SemanticNucleusFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedDeclarationSignature {
    /// Stable source identity computed with the signature. Consumers which
    /// already observed this projection must not issue a peer identity query
    /// merely to recover the same key.
    pub(crate) definition: StableDefinitionKey,
    pub(crate) signature: DeclarationSignatureProjection,
    /// Exact callable type fragments captured by the same canonical parse as
    /// `signature`. Body specialization needs these for dependent types and
    /// must not reparse the raw declaration to recover them.
    pub(crate) callable_type_syntax: Option<rue_air::DurableCallableTypeSyntax>,
    pub(crate) anonymous_nominals: Arc<[DurableAnonymousNominal]>,
    pub(crate) dependencies: Arc<[SemanticDeclarationDependency]>,
    pub(crate) deferred_ownership: Arc<[DeferredOwnershipGate]>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ComptimeCallProjection {
    pub(crate) result: ComptimeCallResultProjection,
    pub(crate) anonymous_nominals: Arc<[DurableAnonymousNominal]>,
    pub(crate) dependencies: Arc<[SemanticDeclarationDependency]>,
    pub(crate) deferred_ownership: Arc<[DeferredOwnershipGate]>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SemanticDeclarationDependency {
    pub(crate) source: StableDefinitionKey,
    pub(crate) kind: rue_air::DeclarationTypeDependencyKind,
    pub(crate) target: SemanticDeclarationDependencyTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum SemanticDeclarationDependencyTarget {
    NamedType(StableDefinitionKey),
    TypeCallHead(StableDefinitionKey),
    BuiltinTypeCallHead(rue_air::BuiltinTypeCallHead),
    NamedValue(StableDefinitionKey),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ComptimeCallResultProjection {
    Type(DurableType),
    Value(DurableConstValue),
}

impl DeclarationSemanticQueryKey {
    pub(crate) fn stable_identity(&self) -> String {
        format!(
            "{}:{:?}:{:?}",
            self.declaration.stable_identity(),
            self.configuration.target,
            self.configuration.preview_features,
        )
    }
}

/// Identity facts are stamped separately from signatures and values. A
/// pointer edge can therefore name a nominal without forcing its aggregate
/// fields, and ordinary callers can observe a callable identity without its
/// declaration-time comptime result.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeclarationIdentityProjection {
    pub(crate) key: StableDefinitionKey,
    pub(crate) is_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DeclarationSignatureProjection {
    Callable {
        parameters: Arc<[DurableSemanticParameter]>,
        result: DurableType,
        has_self: bool,
        self_mode: crate::durable_semantics::DurableParameterMode,
        is_accessor: bool,
        is_unchecked: bool,
        is_extern: bool,
        is_c_export: bool,
    },
    Struct {
        fields: Arc<[(Arc<str>, DurableType)]>,
        is_copy: bool,
        is_linear: bool,
        is_repr_c: bool,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[DurableType]>)]>,
    },
    Destructor,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ConstResolutionProjection {
    Value {
        key: StableDefinitionKey,
        ty: DurableType,
        value: Box<DurableConstValue>,
        anonymous_nominals: Arc<[DurableAnonymousNominal]>,
        dependencies: Arc<[SemanticDeclarationDependency]>,
        deferred_ownership: Arc<[DeferredOwnershipGate]>,
    },
    ModuleBinding {
        key: StableDefinitionKey,
        target: ModuleId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DeclarationSemanticValue {
    pub(crate) identity: DeclarationIdentityProjection,
    pub(crate) payload: DurableDeclarationPayload,
}

impl RetainedCharge for DeferredOwnershipApplication {
    fn retained_charge(&self) -> u64 {
        self.declaration.retained_charge()
    }
}

impl RetainedCharge for DeferredOwnershipGateSource {
    fn retained_charge(&self) -> u64 {
        self.declaration.retained_charge()
    }
}

impl RetainedCharge for DeferredOwnershipGate {
    fn retained_charge(&self) -> u64 {
        self.ty
            .retained_charge()
            .saturating_add(self.source.retained_charge())
            .saturating_add(self.application.retained_charge())
    }
}

impl RetainedCharge for DuplicateDeclarationFailure {
    fn retained_charge(&self) -> u64 {
        self.kind
            .retained_charge()
            .saturating_add(self.first.retained_charge())
            .saturating_add(self.duplicate.retained_charge())
    }
}

impl RetainedCharge for ForeignSignatureSite {
    fn retained_charge(&self) -> u64 {
        self.declaration
            .retained_charge()
            .saturating_add(self.signature.retained_charge())
    }
}

impl RetainedCharge for ForeignSignatureConflictFailure {
    fn retained_charge(&self) -> u64 {
        self.symbol
            .retained_charge()
            .saturating_add(self.left.retained_charge())
            .saturating_add(self.right.retained_charge())
    }
}

impl RetainedCharge for SemanticNucleusFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Shell(detail) | Self::Syntax(detail) | Self::Resolution(detail) => {
                detail.retained_charge()
            }
            Self::Diagnostic(kind)
            | Self::DiagnosticAtParameter { kind, .. }
            | Self::DiagnosticAtProducerRange { kind, .. } => kind.retained_charge(),
            Self::DiagnosticAtDeclaration { kind, declaration } => kind
                .retained_charge()
                .saturating_add(declaration.retained_charge()),
            Self::DuplicateDeclaration {
                kind,
                first,
                duplicate,
            } => kind
                .retained_charge()
                .saturating_add(first.retained_charge())
                .saturating_add(duplicate.retained_charge()),
            Self::DuplicateDeclarations(failures) => failures.retained_charge(),
            Self::ForeignSignatureConflict(failure) => failure.retained_charge(),
            Self::OwnershipGate { kind, gate } => kind
                .retained_charge()
                .saturating_add(gate.retained_charge()),
            Self::DiagnosticWithHelp { kind, help } => kind
                .retained_charge()
                .saturating_add(help.retained_charge()),
            Self::DiagnosticWithNote { kind, note } => kind
                .retained_charge()
                .saturating_add(note.retained_charge()),
            Self::SignatureReentry { signature, cycle } => signature
                .retained_charge()
                .saturating_add(cycle.retained_charge()),
            Self::Cycle(cycle) => cycle.retained_charge(),
        }
    }
}

impl RetainedCharge for SemanticDeclarationDependencyTarget {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::NamedType(key) | Self::TypeCallHead(key) | Self::NamedValue(key) => {
                key.retained_charge()
            }
            Self::BuiltinTypeCallHead(_) => 0,
        }
    }
}

impl RetainedCharge for SemanticDeclarationDependency {
    fn retained_charge(&self) -> u64 {
        self.source
            .retained_charge()
            .saturating_add(self.target.retained_charge())
    }
}

impl RetainedCharge for DeclarationIdentityProjection {
    fn retained_charge(&self) -> u64 {
        self.key.retained_charge()
    }
}

impl RetainedCharge for DeclarationSignatureProjection {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Callable {
                parameters, result, ..
            } => parameters
                .retained_charge()
                .saturating_add(result.retained_charge()),
            Self::Struct { fields, .. } => fields.retained_charge(),
            Self::Enum { variants } => variants.retained_charge(),
            Self::Destructor => 0,
        }
    }
}

impl RetainedCharge for ResolvedDeclarationSignature {
    fn retained_charge(&self) -> u64 {
        let callable_type_syntax = self.callable_type_syntax.as_ref().map_or(0, |syntax| {
            syntax
                .parameters
                .retained_charge()
                .saturating_add(syntax.result.retained_charge())
        });
        self.definition
            .retained_charge()
            .saturating_add(self.signature.retained_charge())
            .saturating_add(callable_type_syntax)
            .saturating_add(self.anonymous_nominals.retained_charge())
            .saturating_add(self.dependencies.retained_charge())
            .saturating_add(self.deferred_ownership.retained_charge())
    }
}

impl RetainedCharge for ComptimeCallResultProjection {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Type(ty) => ty.retained_charge(),
            Self::Value(value) => value.retained_charge(),
        }
    }
}

impl RetainedCharge for ComptimeCallProjection {
    fn retained_charge(&self) -> u64 {
        self.result
            .retained_charge()
            .saturating_add(self.anonymous_nominals.retained_charge())
            .saturating_add(self.dependencies.retained_charge())
            .saturating_add(self.deferred_ownership.retained_charge())
    }
}

impl RetainedCharge for ConstResolutionProjection {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Value {
                key,
                ty,
                value,
                anonymous_nominals,
                dependencies,
                deferred_ownership,
            } => key
                .retained_charge()
                .saturating_add(ty.retained_charge())
                .saturating_add(value.retained_charge())
                .saturating_add(anonymous_nominals.retained_charge())
                .saturating_add(dependencies.retained_charge())
                .saturating_add(deferred_ownership.retained_charge()),
            Self::ModuleBinding { key, target } => key
                .retained_charge()
                .saturating_add(target.retained_charge()),
        }
    }
}

impl RetainedCharge for SemanticNucleusValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Identity(value) => value.retained_charge(),
            Self::Signature(value) => value.retained_charge(),
            Self::NominalWellFormedness | Self::DeferredOwnership => 0,
            Self::ConstResolution(value) => value.retained_charge(),
            Self::ComptimeCall(value) => value.retained_charge(),
            Self::AnonymousNominal(value) => value.retained_charge(),
            Self::Failure(failure) => failure.retained_charge(),
        }
    }
}

impl RetainedCharge for DeclarationSemanticValue {
    fn retained_charge(&self) -> u64 {
        self.identity
            .retained_charge()
            .saturating_add(self.payload.retained_charge())
    }
}

impl DeclarationSemanticValue {
    pub(crate) fn from_signature(
        identity: DeclarationIdentityProjection,
        signature: DeclarationSignatureProjection,
    ) -> Self {
        let payload = match signature {
            DeclarationSignatureProjection::Callable {
                parameters,
                result,
                has_self,
                self_mode,
                is_accessor: _,
                is_unchecked,
                is_extern: _,
                is_c_export: _,
            } => DurableDeclarationPayload::Callable {
                parameters,
                result,
                has_self,
                self_mode,
                is_unchecked,
            },
            DeclarationSignatureProjection::Struct {
                fields,
                is_copy,
                is_linear,
                is_repr_c: _,
            } => DurableDeclarationPayload::Struct {
                fields,
                is_copy,
                is_linear,
            },
            DeclarationSignatureProjection::Enum { variants } => {
                DurableDeclarationPayload::Enum { variants }
            }
            DeclarationSignatureProjection::Destructor => DurableDeclarationPayload::Destructor,
        };
        Self { identity, payload }
    }

    pub(crate) fn from_const(is_public: bool, resolution: ConstResolutionProjection) -> Self {
        match resolution {
            ConstResolutionProjection::Value { key, ty, value, .. } => Self {
                identity: DeclarationIdentityProjection { key, is_public },
                payload: DurableDeclarationPayload::Const { ty, value: *value },
            },
            ConstResolutionProjection::ModuleBinding { key, target } => Self {
                identity: DeclarationIdentityProjection { key, is_public },
                payload: DurableDeclarationPayload::ModuleBinding { target },
            },
        }
    }
}

pub(crate) fn direct_identity(
    shell: &DeclarationShellFact,
) -> Option<DeclarationIdentityProjection> {
    let (namespace, kind, owner) = match shell.key.category {
        Category::Function | Category::ExternFunction => (Namespace::Value, Kind::Function, None),
        Category::Struct => (Namespace::Type, Kind::Struct, None),
        Category::Enum => (Namespace::Type, Kind::Enum, None),
        Category::Destructor => (
            Namespace::Destructor,
            Kind::Destructor,
            shell
                .key
                .owner
                .as_ref()
                .map(|owner| (Kind::Struct, Arc::<str>::clone(&owner.name))),
        ),
        Category::Method => (
            Namespace::Method,
            Kind::Method,
            shell
                .key
                .owner
                .as_ref()
                .map(|owner| (Kind::Struct, Arc::<str>::clone(&owner.name))),
        ),
        Category::AssociatedFunction => (
            Namespace::Method,
            Kind::AssociatedFunction,
            shell
                .key
                .owner
                .as_ref()
                .map(|owner| (Kind::Struct, Arc::<str>::clone(&owner.name))),
        ),
        Category::ConstCandidate => return None,
    };
    Some(DeclarationIdentityProjection {
        key: StableDefinitionKey::from_stable_parts(
            shell.key.module.clone(),
            namespace,
            kind,
            shell.key.name.clone(),
            owner,
        ),
        is_public: shell.is_public,
    })
}

pub(crate) fn classified_const_identity(
    shell: &DeclarationShellFact,
    module_binding: bool,
) -> DeclarationIdentityProjection {
    debug_assert_eq!(shell.key.category, Category::ConstCandidate);
    DeclarationIdentityProjection {
        key: StableDefinitionKey::from_stable_parts(
            shell.key.module.clone(),
            Namespace::Value,
            if module_binding {
                Kind::ModuleBinding
            } else {
                Kind::ValueConst
            },
            shell.key.name.clone(),
            None,
        ),
        is_public: shell.is_public,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declaration_candidate::DeclarationParameterHeader;

    fn shell(category: Category) -> DeclarationShellFact {
        DeclarationShellFact {
            key: DeclarationCandidateKey {
                module: ModuleId::from_validated_canonical("app/main.rue"),
                category,
                name: Arc::from("item"),
                owner: None,
                duplicate_discriminator: 0,
            },
            is_public: true,
            parameters: Arc::<[DeclarationParameterHeader]>::from([]),
            receiver: None,
            receiver_is_mut: false,
            is_generic: false,
            is_unchecked: false,
            is_extern: false,
            signature_fingerprint: [0; 32],
        }
    }

    #[test]
    fn const_identity_is_issued_only_after_classification() {
        let shell = shell(Category::ConstCandidate);
        assert!(direct_identity(&shell).is_none());
        assert_eq!(
            classified_const_identity(&shell, false).key.kind(),
            Kind::ValueConst
        );
        assert_eq!(
            classified_const_identity(&shell, true).key.kind(),
            Kind::ModuleBinding
        );
    }
}
