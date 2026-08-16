//! Declaration gathering for semantic analysis.
//!
//! This module handles the first phase of semantic analysis: gathering all
//! type and function declarations from the RIR. This includes:
//!
//! - Registering struct and enum type names
//! - Resolving struct field types
//! - Collecting function signatures
//! - Collecting method signatures from impl blocks
//! - Validating @copy structs

use std::collections::HashSet;
use std::sync::Arc;

use rue_error::{CompileError, CompileResult};
use rue_rir::InstData;
use rue_rir::{InstRef, RirParamMode};
use rue_span::Span;

use crate::declaration_validation::{
    AccessorBodyVerdict, AccessorExitForm, AccessorParameterForm, AccessorReceiverForm,
    AccessorYieldRootForm,
};

/// The declaration legality of a `-> borrow T` accessor (ADR-0062): the result
/// position requires the `borrow_accessors` preview (6.6:3), the receiver is a
/// shared `borrow self` (6.6:4), every other parameter is a plain by-value
/// guard input (6.6:5), the body's only exit is its trailing `yield` (6.6:6),
/// and that `yield` hands out a receiver-rooted place (6.6:7).
///
/// These are legality rules on the declaration, so a declared-but-uncalled
/// accessor is exactly as ill-formed as a called one. Every producer runs them
/// over every accessor declaration it admits, which is what keeps the rules
/// independent of the driver's on-demand body analysis. What this function
/// owns is the *RIR walk*; which forms are illegal and how each one reads is
/// [`crate::declaration_validation`]'s, shared with the driver's reparsed-AST
/// producer. The body rules are syntactic over the RIR, so they belong here
/// with the signature rules; the single part that is not — whether a
/// method-call link in the yielded chain names an accessor — is documented on
/// [`accessor_yield_root`] and stays with the demanded path.
///
/// The declaring `FnDecl` is the only carrier of `returns_borrow`: the durable
/// signature records the result type's source spelling, which never contains
/// the result-position `borrow` qualifier.
///
/// The preview gate runs first, so an ungated program reports E1100 rather
/// than a shape error about a form it cannot name yet.
pub(super) fn check_accessor_declaration_shape(
    rir: &rue_rir::Rir,
    interner: &lasso::ThreadedRodeo,
    declaration: InstRef,
    body: Option<InstRef>,
    has_named_owner: bool,
    preview_features: &rue_error::PreviewFeatures,
) -> CompileResult<()> {
    let inst = rir.get(declaration);
    let InstData::FnDecl {
        params,
        has_self,
        self_mode,
        returns_borrow: true,
        ..
    } = &inst.data
    else {
        return Ok(());
    };
    let span = inst.span;
    super::require_preview_feature(
        preview_features,
        crate::declaration_validation::ACCESSOR_PREVIEW_FEATURE,
        crate::declaration_validation::ACCESSOR_PREVIEW_SUBJECT,
        span,
    )?;
    // An accessor hands out a projection of its receiver, so the receiver is
    // the first thing that has to exist and be a shared borrow. `self` is
    // carried by `has_self`, so the parameter list is exactly the guard
    // inputs.
    let receiver = if !has_named_owner {
        AccessorReceiverForm::FreeFunction
    } else if !*has_self {
        AccessorReceiverForm::AssociatedFunction
    } else {
        match self_mode {
            RirParamMode::Borrow => AccessorReceiverForm::BorrowSelf,
            RirParamMode::Inout => AccessorReceiverForm::InoutSelf,
            RirParamMode::Normal => AccessorReceiverForm::ValueSelf,
        }
    };
    let params = rir.params(params);
    if let Some(violation) = crate::declaration_validation::accessor_signature(
        receiver,
        params
            .iter()
            .map(|param| accessor_parameter_form(param.mode, param.is_comptime)),
    ) {
        use crate::declaration_validation::AccessorSignatureViolation as Violation;
        return Err(match violation {
            Violation::Receiver { kind, note } => {
                let error = CompileError::new(kind, span);
                match note {
                    Some(note) => error.with_note(note),
                    None => error,
                }
            }
            Violation::Parameter { kind, ordinal } => {
                let span = params
                    .iter()
                    .nth(ordinal)
                    .map_or(span, |parameter| parameter.span);
                CompileError::new(kind, span)
            }
        });
    }
    if let Some(body) = body {
        let (verdict, span) = accessor_body_verdict(rir, interner, body);
        if let Some(kind) = crate::declaration_validation::accessor_body_error(&verdict) {
            return Err(CompileError::new(kind, span));
        }
    }
    Ok(())
}

/// The 6.6:5 form of one RIR parameter.
pub(super) fn accessor_parameter_form(
    mode: RirParamMode,
    is_comptime: bool,
) -> AccessorParameterForm {
    if is_comptime {
        return AccessorParameterForm::Comptime;
    }
    match mode {
        RirParamMode::Normal => AccessorParameterForm::ByValue,
        RirParamMode::Borrow => AccessorParameterForm::Borrow,
        RirParamMode::Inout => AccessorParameterForm::Inout,
    }
}

/// Decide 6.6:6 and 6.6:7 for one accessor body over the RIR, with the span of
/// the offending form.
///
/// The rules apply in the spec's own order — a body with no trailing `yield`
/// is reported as that, not as whatever its last expression yields — and each
/// verdict is turned into a diagnostic by
/// [`crate::declaration_validation::accessor_body_error`], so this producer
/// and the driver's reparsed-AST producer cannot disagree on wording.
fn accessor_body_verdict(
    rir: &rue_rir::Rir,
    interner: &lasso::ThreadedRodeo,
    body: InstRef,
) -> (AccessorBodyVerdict, Span) {
    let Some(trailing) = trailing_yield(rir, body) else {
        return (
            AccessorBodyVerdict::MissingTrailingYield,
            rir.get(body).span,
        );
    };
    if let Some((exit, span)) = accessor_body_exit(rir, body, trailing) {
        return (AccessorBodyVerdict::OtherExit(exit), span);
    }
    let InstData::Yield(operand) = rir.get(trailing).data else {
        unreachable!("the trailing exit is a `yield` by construction")
    };
    match accessor_yield_root(rir, interner, operand) {
        Some((root, span)) => (AccessorBodyVerdict::YieldNotReceiverRooted(root), span),
        None => (AccessorBodyVerdict::WellFormed, rir.get(body).span),
    }
}

/// Every instruction lexically contained in `body`, `body` itself included,
/// in lowering (source) order.
///
/// The walk stops at a nested declaration — a nested `fn`, drop function,
/// struct, or anonymous type owns its own body, and predeclaration reaches
/// that declaration on its own entry — so a containment question answered
/// here is answered about this body alone.
fn body_instructions(rir: &rue_rir::Rir, body: InstRef) -> Vec<InstRef> {
    let mut pending = vec![body];
    let mut seen = HashSet::new();
    let mut contained = Vec::new();
    let mut children = Vec::new();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        contained.push(current);
        if current != body
            && matches!(
                rir.get(current).data,
                InstData::FnDecl { .. }
                    | InstData::DropFnDecl { .. }
                    | InstData::StructDecl { .. }
                    | InstData::EnumDecl { .. }
                    | InstData::AnonStructType { .. }
                    | InstData::AnonEnumType { .. }
            )
        {
            continue;
        }
        children.clear();
        rir.child_instructions(current, &mut children);
        pending.extend(children.iter().copied());
    }
    // Instruction indices are assigned as astgen lowers, so ordering by index
    // reports the first offending form in the source rather than an arbitrary
    // one from the traversal.
    contained.sort_unstable_by_key(|inst_ref| inst_ref.as_u32());
    contained
}

/// The rest of 6.6:6: an accessor body's trailing `yield` is its *only* exit —
/// no second `yield`, no `return`, no `?` (E0254).
///
/// "Contains" is the spec's own wording, and containment is decidable from the
/// RIR, so this runs at the declaration for every accessor the program
/// declares rather than only for a body some call site demands. The
/// comptime-pruned branch of an `if` counts: 6.6:6 forbids the form appearing
/// in the body, not merely on a path the specialization keeps.
fn accessor_body_exit(
    rir: &rue_rir::Rir,
    body: InstRef,
    trailing: InstRef,
) -> Option<(AccessorExitForm, Span)> {
    for inst_ref in body_instructions(rir, body) {
        let inst = rir.get(inst_ref);
        let exit = match &inst.data {
            InstData::Yield(_) if inst_ref != trailing => AccessorExitForm::SecondYield,
            InstData::Ret(_) => AccessorExitForm::Return,
            InstData::Try { .. } => AccessorExitForm::Try,
            _ => continue,
        };
        return Some((exit, inst.span));
    }
    None
}

/// 6.6:7 over the RIR: what the trailing `yield`'s operand chain is rooted at,
/// and the span of the form that decided it (E0255).
///
/// Everything this rule needs is syntactic except one link. A nested
/// method-call link is legal only when the callee is *itself* an accessor,
/// and which method a call names is a resolved-type question, so this walk
/// accepts any method call and keeps descending to the chain's root — it never
/// reports [`AccessorYieldRootForm::PlainMethod`], which only a producer with
/// the callee in hand may claim. That leaves exactly one residual for a
/// declaration nothing calls: a chain through a plain (non-accessor) method of
/// the receiver, such as `yield self.plain();`, whose rejection stays with
/// [`super::control_flow`]'s demanded-path check. Every other shape — a local,
/// a non-receiver parameter, a computed value, a chain rooted at anything but
/// `self` — is decided here with no call site.
fn accessor_yield_root(
    rir: &rue_rir::Rir,
    interner: &lasso::ThreadedRodeo,
    operand: InstRef,
) -> Option<(AccessorYieldRootForm, Span)> {
    let self_sym = interner.get_or_intern("self");
    let mut current = operand;
    loop {
        let inst = rir.get(current);
        let span = inst.span;
        match &inst.data {
            InstData::VarRef { name, .. } => {
                if *name == self_sym {
                    return None;
                }
                let root = AccessorYieldRootForm::Named(Arc::from(interner.resolve(name)));
                return Some((root, span));
            }
            InstData::FieldGet { base, .. } | InstData::IndexGet { base, .. } => current = *base,
            InstData::MethodCall { receiver, .. } => current = *receiver,
            _ => return Some((AccessorYieldRootForm::Value, span)),
        }
    }
}

/// The single trailing `yield` that an accessor body falls through to
/// (6.6:6, ADR-0062 phase 1), if it has one.
///
/// Which instruction is the trailing exit is decidable from the RIR alone, so
/// the declaration seam decides a body with no trailing `yield` for every
/// accessor in the program. [`accessor_body_exit`] then finds the *other*
/// exits from the same seam.
fn trailing_yield(rir: &rue_rir::Rir, body: InstRef) -> Option<InstRef> {
    // A single-statement body lowers to the instruction itself; a
    // multi-statement body lowers to a block whose last instruction is the
    // trailing exit.
    let trailing = match &rir.get(body).data {
        InstData::Block { instructions } => rir.block_insts(instructions).values().last(),
        _ => Some(body),
    };
    trailing.filter(|inst_ref| matches!(rir.get(*inst_ref).data, InstData::Yield(_)))
}

/// [`trailing_yield`] as the body engine demands it: the trailing exit it
/// records for the body it is about to analyze, or E0254.
pub(super) fn accessor_trailing_yield(rir: &rue_rir::Rir, body: InstRef) -> CompileResult<InstRef> {
    trailing_yield(rir, body).ok_or_else(|| {
        CompileError::new(
            crate::declaration_validation::accessor_missing_yield_error(),
            rir.get(body).span,
        )
    })
}
