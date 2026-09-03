//! Stable declaration-query identities and value projections.
//!
//! This module owns the interchange values between declaration/comptime
//! queries and the request-local AIR materializer. Values here deliberately
//! contain no parser, RIR, AIR, type-pool, source-position, or interner handles.

use std::collections::BTreeSet;
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
    pub(crate) name: rue_rir::RirTypeSyntaxSymbol,
    pub(crate) mode: crate::declaration_candidate::DeclarationParameterMode,
    pub(crate) is_comptime: bool,
    pub(crate) ty: rue_rir::RirTypeSyntaxRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedSemanticField {
    pub(crate) name: rue_rir::RirTypeSyntaxSymbol,
    pub(crate) ty: rue_rir::RirTypeSyntaxRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedSemanticVariant {
    pub(crate) name: rue_rir::RirTypeSyntaxSymbol,
    pub(crate) payload_start: u32,
    pub(crate) payload_end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedSemanticSignature {
    Callable {
        syntax: rue_rir::RirTypeSyntaxArena<Arc<str>>,
        parameters: Arc<[ParsedSemanticParameter]>,
        result: rue_rir::RirTypeSyntaxRef,
        has_self: bool,
        self_mode: crate::declaration_candidate::DeclarationParameterMode,
        is_unchecked: bool,
        is_extern: bool,
        is_c_export: bool,
        is_accessor: bool,
        /// The declared accessor result qualifier. `Value` means no
        /// place-returning qualifier; it is retained even for invalid
        /// declarations so all signature producers validate the written
        /// receiver/result pairing rather than inferring one from the other.
        accessor_result_mode: crate::declaration_candidate::DeclarationParameterMode,
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
        syntax: rue_rir::RirTypeSyntaxArena<Arc<str>>,
        fields: Arc<[ParsedSemanticField]>,
        is_copy: bool,
        is_linear: bool,
        is_repr_c: bool,
    },
    Enum {
        syntax: rue_rir::RirTypeSyntaxArena<Arc<str>>,
        variants: Arc<[ParsedSemanticVariant]>,
        payloads: Arc<[rue_rir::RirTypeSyntaxRef]>,
        is_non_exhaustive: bool,
        is_public: bool,
        /// Directive range relative to the declaration span, retained so
        /// declaration diagnostics can point at `@non_exhaustive` itself.
        non_exhaustive_range: Option<(u32, u32)>,
    },
    Destructor,
}

impl ParsedSemanticSignature {
    pub(crate) fn syntax(&self) -> Option<&rue_rir::RirTypeSyntaxArena<Arc<str>>> {
        match self {
            Self::Callable { syntax, .. }
            | Self::Struct { syntax, .. }
            | Self::Enum { syntax, .. } => Some(syntax),
            Self::Destructor => None,
        }
    }

    pub(crate) fn symbol(&self, value: rue_rir::RirTypeSyntaxSymbol) -> &str {
        self.syntax()
            .and_then(|syntax| syntax.symbol(value))
            .map(AsRef::as_ref)
            .expect("signature symbols are validated when projected")
    }

    #[cfg(test)]
    pub(crate) fn render_type(&self, value: rue_rir::RirTypeSyntaxRef) -> String {
        self.syntax()
            .and_then(|syntax| syntax.render_type(value))
            .expect("signature type roots are validated when projected")
    }

    pub(crate) fn is_type_parameter_syntax(&self, value: rue_rir::RirTypeSyntaxRef) -> bool {
        let Some(syntax) = self.syntax() else {
            return false;
        };
        let Some(rue_rir::RirTypeSyntaxNode::Named(symbol)) = syntax.node(value) else {
            return false;
        };
        syntax
            .symbol(*symbol)
            .is_some_and(|name| name.as_ref() == "type")
    }

    pub(crate) fn callable_type_syntax(&self) -> Option<rue_air::DurableCallableTypeSyntax> {
        let Self::Callable {
            syntax,
            parameters,
            result,
            ..
        } = self
        else {
            return None;
        };
        Some(rue_air::DurableCallableTypeSyntax {
            syntax: syntax.clone(),
            parameters: parameters.iter().map(|parameter| parameter.ty).collect(),
            result: *result,
        })
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
    syntax: &mut rue_rir::RirTypeSyntaxBuilder<Arc<str>>,
    resolve: impl Copy + Fn(Spur) -> &'a str,
    parameters: &[rue_parser::ast::Param],
) -> Result<Arc<[ParsedSemanticParameter]>, Arc<str>> {
    parameters
        .iter()
        .map(|parameter| {
            let (mode, is_comptime) = parameter_mode(parameter.mode);
            Ok(ParsedSemanticParameter {
                name: syntax
                    .intern_symbol(Arc::from(resolve(parameter.name.name)))
                    .map_err(type_syntax_build_failure)?,
                mode,
                is_comptime,
                ty: syntax
                    .push_parser_type(&parameter.ty, |symbol| Arc::from(resolve(symbol)))
                    .map_err(type_syntax_build_failure)?,
            })
        })
        .collect::<Result<Vec<_>, Arc<str>>>()
        .map(Into::into)
}

fn type_syntax_build_failure(error: rue_rir::RirTypeSyntaxBuildError) -> Arc<str> {
    Arc::from(format!(
        "semantic signature type syntax exceeds the supported size: {error:?}"
    ))
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

/// Normalize one owner's parsed method facts for canonical accessor
/// declaration validation: sorted by name, with every ambiguously duplicated
/// name dropped.
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
/// enters the returned value. Every annotation is projected directly into the
/// declaration-local dense type-syntax arena; semantic resolution never
/// reconstructs or reparses a source fragment.
pub(crate) fn project_semantic_signature(
    module: &crate::parsed_modules::ParsedModule,
    key: &DeclarationCandidateKey,
) -> Result<ParsedSemanticSignature, Arc<str>> {
    use crate::parsed_modules::ParsedDeclarationAstRef;

    // RUE-1510 deleted `declaration_signature_parsing` and replaced it with
    // this projection, and RUE-1514 held the boundary to the deletion by
    // rejecting any producer that reports a signature parse. That proves no
    // parsing happens and says nothing about whether projection happens, so a
    // regression that removed or short-circuited projection entirely would
    // pass. This span is the positive half: RUE-1515 requires it non-zero.
    let _projection_span = tracing::info_span!(
        "declaration_signature_projection",
        phase = "semantic_analysis"
    )
    .entered();

    if key.category == Category::ConstCandidate {
        return Err(Arc::from(
            "constant candidates have no signature projection",
        ));
    }
    let declaration = module
        .declaration_ast(key)
        .ok_or_else(|| Arc::from("semantic signature has no exact parsed declaration"))?;
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
                    accessor_result_mode,
                    body: Option<&rue_parser::ast::Expr>|
     -> Result<ParsedSemanticSignature, Arc<str>> {
        let mut syntax = rue_rir::RirTypeSyntaxBuilder::default();
        let parameters = parsed_parameters(&mut syntax, resolve, parameters)?;
        let result = match result {
            Some(value) => syntax
                .push_parser_type(value, |symbol| Arc::from(resolve(symbol)))
                .map_err(type_syntax_build_failure)?,
            None => syntax.push_unit_type().map_err(type_syntax_build_failure)?,
        };
        Ok(ParsedSemanticSignature::Callable {
            syntax: syntax.finish(),
            parameters,
            result,
            has_self,
            self_mode,
            is_unchecked,
            is_extern,
            is_c_export,
            is_accessor,
            accessor_result_mode,
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
            function.place_return.is_some(),
            if function.place_return.is_some_and(|mode| mode.is_inout()) {
                crate::declaration_candidate::DeclarationParameterMode::Inout
            } else if function.place_return.is_some_and(|mode| mode.is_borrow()) {
                crate::declaration_candidate::DeclarationParameterMode::Borrow
            } else {
                crate::declaration_candidate::DeclarationParameterMode::Value
            },
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
            crate::declaration_candidate::DeclarationParameterMode::Value,
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
            method.place_return.is_some(),
            if method.place_return.is_some_and(|mode| mode.is_inout()) {
                crate::declaration_candidate::DeclarationParameterMode::Inout
            } else if method.place_return.is_some_and(|mode| mode.is_borrow()) {
                crate::declaration_candidate::DeclarationParameterMode::Borrow
            } else {
                crate::declaration_candidate::DeclarationParameterMode::Value
            },
            Some(&method.body),
        ),
        ParsedDeclarationAstRef::Struct(structure) => {
            let mut syntax = rue_rir::RirTypeSyntaxBuilder::default();
            let fields = structure
                .fields
                .iter()
                .map(|field| {
                    Ok(ParsedSemanticField {
                        name: syntax
                            .intern_symbol(Arc::from(resolve(field.name.name)))
                            .map_err(type_syntax_build_failure)?,
                        ty: syntax
                            .push_parser_type(&field.ty, |symbol| Arc::from(resolve(symbol)))
                            .map_err(type_syntax_build_failure)?,
                    })
                })
                .collect::<Result<Vec<_>, Arc<str>>>()?;
            Ok(ParsedSemanticSignature::Struct {
                syntax: syntax.finish(),
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
            let mut syntax = rue_rir::RirTypeSyntaxBuilder::default();
            let mut payloads = Vec::new();
            let variants = value
                .variants
                .iter()
                .map(|variant| {
                    let payload_start = u32::try_from(payloads.len()).map_err(|_| {
                        Arc::from("semantic signature payload exceeds the supported size")
                    })?;
                    for ty in &variant.payload {
                        payloads.push(
                            syntax
                                .push_parser_type(ty, |symbol| Arc::from(resolve(symbol)))
                                .map_err(type_syntax_build_failure)?,
                        );
                    }
                    let payload_end = u32::try_from(payloads.len()).map_err(|_| {
                        Arc::from("semantic signature payload exceeds the supported size")
                    })?;
                    Ok(ParsedSemanticVariant {
                        name: syntax
                            .intern_symbol(Arc::from(resolve(variant.name.name)))
                            .map_err(type_syntax_build_failure)?,
                        payload_start,
                        payload_end,
                    })
                })
                .collect::<Result<Vec<_>, Arc<str>>>()?;
            Ok(ParsedSemanticSignature::Enum {
                syntax: syntax.finish(),
                variants: variants.into(),
                payloads: payloads.into(),
                is_non_exhaustive: value
                    .directives
                    .iter()
                    .any(|directive| resolve(directive.name.name) == "non_exhaustive"),
                is_public: value.visibility == rue_parser::ast::Visibility::Public,
                non_exhaustive_range: value
                    .directives
                    .iter()
                    .find(|directive| resolve(directive.name.name) == "non_exhaustive")
                    .and_then(|directive| {
                        Some((
                            directive.span.start.checked_sub(value.span.start)?,
                            directive.span.end.checked_sub(value.span.start)?,
                        ))
                    }),
            })
        }
        // A test's signature is fixed by the grammar: no parameters, no
        // receiver, unit result (ADR-0083 §1). It is projected as an ordinary
        // callable so body analysis needs no second signature shape.
        ParsedDeclarationAstRef::Test(_) => callable(
            &[],
            None,
            false,
            crate::declaration_candidate::DeclarationParameterMode::Value,
            false,
            false,
            false,
            false,
            crate::declaration_candidate::DeclarationParameterMode::Value,
            None,
        ),
        ParsedDeclarationAstRef::Destructor(_) => Ok(ParsedSemanticSignature::Destructor),
        ParsedDeclarationAstRef::Const(_) => Err(Arc::from(
            "constant candidates have no signature projection",
        )),
    }
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
    /// A diagnostic anchored within the named producer declaration. Offsets
    /// are declaration-relative so the stable failure carries no revision-local span;
    /// retaining the producer key keeps nested comptime failures attached to
    /// their true declaration instead of the caller which observed them.
    DiagnosticAtProducerRange {
        kind: rue_error::ErrorKind,
        producer: DeclarationCandidateKey,
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
        accessor_result_mode: crate::durable_semantics::DurableParameterMode,
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
        is_non_exhaustive: bool,
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
            Self::Diagnostic(kind) | Self::DiagnosticAtParameter { kind, .. } => {
                kind.retained_charge()
            }
            Self::DiagnosticAtProducerRange { kind, producer, .. } => kind
                .retained_charge()
                .saturating_add(producer.retained_charge()),
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
            Self::Enum { variants, .. } => variants.retained_charge(),
            Self::Destructor => 0,
        }
    }
}

impl RetainedCharge for ResolvedDeclarationSignature {
    fn retained_charge(&self) -> u64 {
        let callable_type_syntax = self
            .callable_type_syntax
            .as_ref()
            .map_or(0, |syntax| syntax.retained_charge());
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
                accessor_result_mode: _,
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
            DeclarationSignatureProjection::Enum {
                variants,
                is_non_exhaustive,
            } => DurableDeclarationPayload::Enum {
                variants,
                is_non_exhaustive,
            },
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
        // A test declaration owns a body and no owning type (ADR-0083 §1). Its
        // dedicated namespace is what makes the non-collision with a same-named
        // function structural rather than conventional.
        Category::Test => (Namespace::Test, Kind::Test, None),
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
