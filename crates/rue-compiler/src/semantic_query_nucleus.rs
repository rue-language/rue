//! Stable declaration-query identities and value projections.
//!
//! This module owns the interchange values between declaration/comptime
//! queries and the request-local AIR materializer. Values here deliberately
//! contain no parser, RIR, AIR, type-pool, source-position, or interner handles.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

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
    pub(crate) name: Arc<str>,
    pub(crate) mode: crate::declaration_candidate::DeclarationParameterMode,
    pub(crate) is_comptime: bool,
    pub(crate) ty: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedSemanticSignature {
    Callable {
        parameters: Arc<[ParsedSemanticParameter]>,
        result: Arc<str>,
        has_self: bool,
        self_mode: crate::declaration_candidate::DeclarationParameterMode,
        is_unchecked: bool,
        is_extern: bool,
        is_c_export: bool,
    },
    Struct {
        fields: Arc<[(Arc<str>, Arc<str>)]>,
        is_copy: bool,
        is_linear: bool,
        is_repr_c: bool,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[Arc<str>]>)]>,
    },
    Destructor,
}

fn signature_source(
    key: &DeclarationCandidateKey,
    syntax: &crate::declaration_candidate::RawDeclarationSignatureSyntax,
) -> String {
    let fragments = syntax
        .declaration_fragments
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<&str>>();
    match key.category {
        // Split struct terminals already retain the entire declaration except
        // method declarations. Concatenating the retained prefix and closing
        // brace reconstructs valid field-only syntax; inserting a bare block
        // here would itself be parsed as a malformed field.
        Category::Struct => fragments.concat(),
        Category::Function | Category::Destructor => format!("{} {{}}", fragments.concat()),
        Category::Method | Category::AssociatedFunction => format!(
            "struct {} {{ {} {{}} }}",
            key.owner
                .as_ref()
                .map(|owner| owner.name.as_ref())
                .unwrap_or("__missing_owner"),
            fragments.concat(),
        ),
        Category::ExternFunction => format!(
            "extern {} {{ {} }}",
            syntax.extern_abi.as_deref().unwrap_or("\"C\""),
            fragments.concat(),
        ),
        Category::Enum => fragments.concat(),
        Category::ConstCandidate => String::new(),
    }
}

fn source_fragment(source: &str, span: rue_span::Span) -> Result<Arc<str>, Arc<str>> {
    source
        .get(span.start as usize..span.end as usize)
        .map(Arc::from)
        .ok_or_else(|| Arc::from("semantic signature contains an invalid local span"))
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

fn parsed_parameters(
    source: &str,
    interner: &crate::ThreadedRodeo,
    parameters: &[rue_parser::ast::Param],
) -> Result<Arc<[ParsedSemanticParameter]>, Arc<str>> {
    parameters
        .iter()
        .map(|parameter| {
            let (mode, is_comptime) = parameter_mode(parameter.mode);
            Ok(ParsedSemanticParameter {
                name: Arc::from(interner.resolve(&parameter.name.name)),
                mode,
                is_comptime,
                ty: source_fragment(source, parameter.ty.span())?,
            })
        })
        .collect::<Result<Vec<_>, Arc<str>>>()
        .map(Into::into)
}

/// Reparse one exact body-free syntax terminal into a value-only shape. This
/// parser owns no name or type resolution policy; every type fragment is later
/// routed through `rue_air::resolve_semantic_type_syntax`.
pub(crate) fn parse_semantic_signature(
    key: &DeclarationCandidateKey,
    syntax: &crate::declaration_candidate::RawDeclarationSignatureSyntax,
) -> Result<ParsedSemanticSignature, Arc<str>> {
    if key.category == Category::ConstCandidate {
        return Err(Arc::from("constant candidates have no signature terminal"));
    }
    let source = signature_source(key, syntax);
    let parsed = crate::syntax::parse_file(
        crate::SourceView::new("<semantic-signature>", &source, rue_span::FileId::new(0)),
        crate::ThreadedRodeo::new(),
    );
    let ast = parsed
        .result
        .map_err(|errors| Arc::from(errors.to_string()))?;
    let interner = parsed.interner;
    let item = ast
        .items
        .first()
        .ok_or_else(|| Arc::from("semantic signature parsed no declaration"))?;
    let unit: Arc<str> = Arc::from("()");
    let callable = |parameters: &[rue_parser::ast::Param],
                    result: Option<&rue_parser::ast::TypeExpr>,
                    has_self,
                    self_mode,
                    is_unchecked,
                    is_extern,
                    is_c_export|
     -> Result<ParsedSemanticSignature, Arc<str>> {
        Ok(ParsedSemanticSignature::Callable {
            parameters: parsed_parameters(&source, &interner, parameters)?,
            result: result
                .map(|value| source_fragment(&source, value.span()))
                .transpose()?
                .unwrap_or_else(|| unit.clone()),
            has_self,
            self_mode,
            is_unchecked,
            is_extern,
            is_c_export,
        })
    };
    match (key.category, item) {
        (Category::Function, rue_parser::ast::Item::Function(function)) => callable(
            &function.params,
            function.return_type.as_ref(),
            false,
            crate::declaration_candidate::DeclarationParameterMode::Value,
            function.is_unchecked,
            false,
            function.export_abi.is_some(),
        ),
        (Category::ExternFunction, rue_parser::ast::Item::Extern(block)) => {
            let function = block
                .fns
                .first()
                .ok_or_else(|| Arc::from("semantic extern signature contains no function"))?;
            callable(
                &function.params,
                function.return_type.as_ref(),
                false,
                crate::declaration_candidate::DeclarationParameterMode::Value,
                false,
                true,
                false,
            )
        }
        (Category::Method | Category::AssociatedFunction, rue_parser::ast::Item::Struct(owner)) => {
            let method = owner
                .methods
                .first()
                .ok_or_else(|| Arc::from("semantic method signature contains no method"))?;
            callable(
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
            )
        }
        (Category::Struct, rue_parser::ast::Item::Struct(structure)) => {
            let fields = structure
                .fields
                .iter()
                .map(|field| {
                    Ok((
                        Arc::from(interner.resolve(&field.name.name)),
                        source_fragment(&source, field.ty.span())?,
                    ))
                })
                .collect::<Result<Vec<_>, Arc<str>>>()?;
            let copy = interner.get("copy");
            let is_copy = copy.is_some_and(|copy| {
                structure
                    .directives
                    .iter()
                    .any(|directive| directive.name.name == copy)
            });
            Ok(ParsedSemanticSignature::Struct {
                fields: fields.into(),
                is_copy,
                is_linear: structure.is_linear,
                is_repr_c: structure.directives.iter().any(|directive| {
                    interner.resolve(&directive.name.name) == "repr"
                        && directive.args.iter().any(|argument| match argument {
                            rue_parser::ast::DirectiveArg::Ident(argument) => {
                                interner.resolve(&argument.name) == "c"
                            }
                        })
                }),
            })
        }
        (Category::Enum, rue_parser::ast::Item::Enum(value)) => {
            let variants = value
                .variants
                .iter()
                .map(|variant| {
                    Ok((
                        Arc::from(interner.resolve(&variant.name.name)),
                        variant
                            .payload
                            .iter()
                            .map(|ty| source_fragment(&source, ty.span()))
                            .collect::<Result<Vec<_>, Arc<str>>>()?
                            .into(),
                    ))
                })
                .collect::<Result<Vec<_>, Arc<str>>>()?;
            Ok(ParsedSemanticSignature::Enum {
                variants: variants.into(),
            })
        }
        (Category::Destructor, rue_parser::ast::Item::DropFn(_)) => {
            Ok(ParsedSemanticSignature::Destructor)
        }
        _ => Err(Arc::from(
            "semantic signature category does not match reparsed declaration",
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
    pub(crate) signature: DeclarationSignatureProjection,
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
