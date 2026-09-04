//! Compile-time expression evaluation (the comptime const evaluator).
//!
//! This module hosts the single evaluation engine behind:
//!
//! - `comptime { ... }` block expressions (spec 4.14)
//! - comptime argument validation and value capture for `comptime` parameters
//! - opportunistic constant evaluation (array bounds checks, comptime type
//!   construction for functions returning `type`)
//!
//! # Typed evaluation
//!
//! Values are carried as [`ConstValue::Integer`] backed by `i128`, which
//! covers the full range of every Rue integer type (including u64 values
//! above `i64::MAX`, RUE-70, and negative signed results, RUE-71).
//!
//! When the evaluator runs inside a function being analyzed, it has access to
//! the Hindley-Milner `resolved_types` map and performs arithmetic *at the
//! operand type*, exactly mirroring runtime semantics (spec 8.1):
//!
//! - Add/Sub/Mul/Neg/Div/Mod results that overflow the operand type are a
//!   hard error (`Err`), because the same operation would panic at runtime
//!   (RUE-68). Division/remainder by zero likewise.
//! - Shifts mask the shift amount modulo the bit width and truncate the
//!   result to the operand width (spec 4.3a:10, matching the RUE-29 runtime
//!   semantics), so any constant shift folds (RUE-69).
//! - `~` (BitNot) truncates to the operand width (`~0` as u8 is 255).
//!
//! When no resolved types are available (evaluating a comptime function body
//! before specialization), arithmetic falls back to checked `i64` semantics:
//! results outside the `i64` range make the expression non-evaluable
//! (`Ok(None)`) rather than an error. A file-level const initializer supplies
//! operand types up front (`infer_const_init_types` builds a `resolved_types`
//! map from the declared const type), so it takes the typed path and gets the
//! same operand-type overflow checks as a `comptime { }` block (RUE-230).
//!
//! # Outcome encoding
//!
//! The engine's `eval_const_expr` returns `CompileResult<Option<ConstValue>>`:
//!
//! - `Ok(Some(v))` — fully evaluated.
//! - `Ok(None)` — not compile-time evaluable (runtime variables, calls, ...).
//!   The `comptime` block handler reports this as E1200.
//! - `Err(e)` — the expression *is* constant but would panic at runtime
//!   (overflow at the operand type, division by zero). Inside a `comptime`
//!   block this is a compile error (spec 4.14:4); opportunistic callers
//!   (`try_evaluate_const` and friends) convert it to `None` and
//!   defer to the runtime check.

use ahash::{AHashMap, AHashSet};
use std::time::Instant;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_rir::{InstData, InstRef};
use rue_span::{FileId, Span};

use super::comptime::{
    ComptimeAnonymousKind, ComptimeArgMode, ComptimeArrayLengthBinding, ComptimeCallAdmission,
    ComptimeCallArgument, ComptimeCallKey, ComptimeCallPreparation, ComptimeCallProtocol,
    ComptimeDiagnosticSite, ComptimeDomain, ComptimeEngine, ComptimeEnv as GenericComptimeEnv,
    ComptimeFile, ComptimeFrame, ComptimeHost, ComptimeHostError, ComptimeHostResult,
    ComptimeIdentity, ComptimeInterrupts, ComptimeMatchPattern, ComptimeMethodDescriptor,
    ComptimeName, ComptimeNamedValueResolution, ComptimeOutcome, ComptimeProgramFacts,
    ComptimeRejections, ComptimeSelection, ComptimeSemanticRejection,
    ComptimeStructuredTypeResolution, ComptimeStructuredTypes, ComptimeTrap, ComptimeType,
    ComptimeTypeAlgebra, ComptimeValueAlgebra,
};
use super::context::{AnalysisContext, CheckedConstIndexCandidate, ConstValue};
use super::info::FunctionCallInfo;
use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};

impl ComptimeName for Spur {}
impl ComptimeFile for FileId {}
impl ComptimeIdentity for super::anon_structs::IssuedStableProducerId {}
impl ComptimeIdentity for super::anon_structs::IssuedAnonymousNominalKey {}

pub(crate) type ComptimeEnv<'a> = GenericComptimeEnv<
    'a,
    ConstValue,
    Type,
    Spur,
    FileId,
    super::anon_structs::IssuedStableProducerId,
>;

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CheckedConstIndexTestStats {
    pub evaluations: u64,
    pub hits: u64,
    pub canceled_hits: u64,
    pub candidate_hits: u64,
    pub candidate_rejections: u64,
    pub candidate_comparisons: u64,
    pub comparison_nodes: u64,
}

#[cfg(test)]
thread_local! {
    static CANCEL_ON_CHECKED_CONST_INDEX_HIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CHECKED_CONST_INDEX_HIT_REACHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn with_cancellation_on_checked_const_index_hit<R>(action: impl FnOnce() -> R) -> R {
    CANCEL_ON_CHECKED_CONST_INDEX_HIT.with(|armed| {
        let previous = armed.replace(true);
        CHECKED_CONST_INDEX_HIT_REACHED.with(|reached| reached.set(false));
        let result = action();
        armed.set(previous);
        CHECKED_CONST_INDEX_HIT_REACHED.with(|reached| reached.set(false));
        result
    })
}

#[cfg(test)]
pub(crate) fn checked_const_index_hit_requests_cancellation() -> bool {
    CANCEL_ON_CHECKED_CONST_INDEX_HIT.with(std::cell::Cell::get)
        && CHECKED_CONST_INDEX_HIT_REACHED.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    static CHECKED_CONST_INDEX_TEST_STATS: std::cell::Cell<CheckedConstIndexTestStats> =
        const { std::cell::Cell::new(CheckedConstIndexTestStats {
            evaluations: 0,
            hits: 0,
            canceled_hits: 0,
            candidate_hits: 0,
            candidate_rejections: 0,
            candidate_comparisons: 0,
            comparison_nodes: 0,
        }) };
}

#[cfg(test)]
pub(crate) fn reset_checked_const_index_test_stats() {
    CHECKED_CONST_INDEX_TEST_STATS.with(|stats| stats.set(CheckedConstIndexTestStats::default()));
}

#[cfg(test)]
pub(crate) fn checked_const_index_test_stats() -> CheckedConstIndexTestStats {
    CHECKED_CONST_INDEX_TEST_STATS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn update_checked_const_index_test_stats(update: impl FnOnce(&mut CheckedConstIndexTestStats)) {
    CHECKED_CONST_INDEX_TEST_STATS.with(|stats| {
        let mut current = stats.get();
        update(&mut current);
        stats.set(current);
    });
}

/// Incremental ordinary-body binding state. It is intentionally not Clone:
/// each admitted call owns one source-order binding transaction, and the
/// engine must not replay already validated arguments.
pub struct OrdinaryComptimeCallBinding {
    parameter_names: Vec<Spur>,
    parameter_is_type: Vec<bool>,
    arguments: Vec<ConstValue>,
}

/// Completed ordinary binding kept opaque until call preparation. The maps
/// remain an ordinary-body detail; the AIR engine never reconstructs them.
pub struct OrdinaryComptimeBoundCall {
    pub(crate) callee_types: AHashMap<Spur, Type>,
    pub(crate) callee_values: AHashMap<Spur, ConstValue>,
}

fn push_ordinary_comptime_call_argument(
    binding: &mut OrdinaryComptimeCallBinding,
    value: ConstValue,
) -> bool {
    binding.arguments.push(value);
    true
}

fn finish_ordinary_comptime_call_binding(
    binding: OrdinaryComptimeCallBinding,
) -> Option<OrdinaryComptimeBoundCall> {
    let mut callee_types = AHashMap::new();
    let mut callee_values = AHashMap::new();
    for (index, value) in binding.arguments.into_iter().enumerate() {
        let is_comptime_type = binding
            .parameter_is_type
            .get(index)
            .copied()
            .unwrap_or(matches!(value, ConstValue::Type(_)));
        let parameter_name = binding.parameter_names.get(index).copied()?;
        match (is_comptime_type, value) {
            (true, ConstValue::Type(ty)) => {
                callee_types.insert(parameter_name, ty);
            }
            // Ordinary body semantics deliberately ignore direct-unit
            // provenance: computed Unit remains a valid body type argument.
            (true, ConstValue::Unit) => {
                callee_types.insert(parameter_name, Type::UNIT);
            }
            (true, _) => return None,
            (false, value) => {
                callee_values.insert(parameter_name, value);
            }
        }
    }
    Some(OrdinaryComptimeBoundCall {
        callee_types,
        callee_values,
    })
}

impl super::comptime::ComptimeValue for ConstValue {
    type Type = Type;
    fn integer(value: i128) -> Self {
        Self::Integer(value)
    }
    fn boolean(value: bool) -> Self {
        Self::Bool(value)
    }
    fn unit() -> Self {
        Self::Unit
    }
    fn type_value(value: Type) -> Self {
        Self::Type(value)
    }
    fn as_integer(&self) -> Option<i128> {
        self.as_int_value()
    }
    fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
    fn as_type(&self) -> Option<Type> {
        match self {
            Self::Type(value) => Some(*value),
            _ => None,
        }
    }
}

impl ComptimeType for Type {}

pub(super) fn validate_comptime_value_for_type_impl(
    interner: &lasso::ThreadedRodeo,
    function_name: Spur,
    param_name: Spur,
    value: ConstValue,
    expected: Type,
    span: Span,
    friendly_type_display: impl Fn(Type) -> String,
) -> CompileResult<()> {
    if matches!(value, ConstValue::Function(_)) {
        return Err(CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "callable alias passed to compile-time parameter '{}' of '{}'; callable aliases cannot be passed as arguments",
                    interner.resolve(&param_name),
                    interner.resolve(&function_name)
                ),
            },
            span,
        ));
    }
    if let ConstValue::Integer(integer) = value
        && expected.is_integer()
    {
        if const_int_fits(integer, expected) {
            return Ok(());
        }
        return Err(CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "compile-time argument '{}' for '{}' has value {} outside the range of {}",
                    interner.resolve(&param_name),
                    interner.resolve(&function_name),
                    integer,
                    friendly_type_display(expected)
                ),
            },
            span,
        ));
    }
    let found = value.get_type();
    if found != expected {
        return Err(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: friendly_type_display(expected),
                found: friendly_type_display(found),
            },
            span,
        )
        .with_help(format!(
            "compile-time argument '{}' must match its declared type in '{}'",
            interner.resolve(&param_name),
            interner.resolve(&function_name)
        )));
    }
    Ok(())
}
use super::{DeferredOwnershipGate, DeferredOwnershipGateKind};
use crate::integer_semantics::CheckedIntegerResult;
use crate::types::{ArrayLen, StructField, Type, TypeKind};

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

impl<'a>
    GenericComptimeEnv<
        'a,
        ConstValue,
        Type,
        Spur,
        FileId,
        super::anon_structs::IssuedStableProducerId,
    >
{
    /// The environment for expressions inside the function currently being
    /// analyzed: comptime parameters in scope plus HM-resolved types.
    pub(crate) fn for_analysis(ctx: &'a AnalysisContext) -> Self {
        Self {
            canonical_identity: Some(ctx.canonical_producer.clone()),
            type_subst: ctx.comptime_type_vars.snapshot(),
            value_subst: ctx.comptime_value_vars.clone(),
            resolved_types: Some(ctx.resolved_types),
            // Borrow the caller's live locals instead of snapshotting their
            // names. `for_analysis` sits on per-expression probe paths (every
            // array index, every borrow operand, every `comptime` block), so a
            // fresh O(locals) set allocation and hash per probe made a body with
            // L locals and I probe sites do O(L x I) redundant set-building.
            // The borrowed-membership hook already exists and is already used by
            // the staged entry points; `is_runtime_local_name` consults both.
            runtime_local_names: AHashSet::new(),
            runtime_local_name_membership: Some(std::sync::Arc::new({
                // Capture only the locals map, not the whole context: `&ctx`
                // is neither `Send` nor `Sync`, and an `Arc` over a closure
                // holding it trips `clippy::arc_with_non_send_sync`.
                let locals = &ctx.locals;
                move |name: &Spur| locals.contains_key(name)
            })),
            runtime_binding_names: ctx.params.iter().map(|param| param.name).collect(),
            locals: AHashMap::new(),
            const_module_members: AHashMap::new(),
            defining_file: Some(ctx.current_file_id),
            expected_result: None,
        }
    }
}

/// Decide whether a compile-time-known scrutinee value matches a match arm's
/// pattern (RUE-262). Returns:
/// - `Some(true)` / `Some(false)` — the pattern definitely does / does not match;
/// - `None` — the match can't be decided at compile time here (an enum-variant
///   `Path` pattern, or a scrutinee whose kind the pattern can't compare
///   against), so the caller treats the whole `match` as non-evaluable.
pub(crate) fn const_pattern_matches(
    pattern: &ComptimeMatchPattern<impl ComptimeName>,
    scrut: ConstValue,
) -> Option<bool> {
    match pattern {
        ComptimeMatchPattern::Wildcard => Some(true),
        ComptimeMatchPattern::Bool(b) => match scrut {
            ConstValue::Bool(sb) => Some(sb == *b),
            _ => None,
        },
        ComptimeMatchPattern::Integer(pattern) => match scrut {
            ConstValue::Integer(n) => Some(n == *pattern),
            _ => None,
        },
        ComptimeMatchPattern::Path { .. } => None,
    }
}

/// Check whether `value` is representable in integer type `ty`.
pub(crate) fn const_int_fits(value: i128, ty: Type) -> bool {
    ty.integer_semantics()
        .is_some_and(|integer| integer.fits_i128(value))
}

/// Build the E1200 error for a constant operation that would panic at runtime.
pub(crate) fn comptime_panic_err(reason: String, span: Span) -> CompileError {
    CompileError::new(ErrorKind::ComptimeEvaluationFailed { reason }, span)
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Try to evaluate an RIR expression as a compile-time constant, with no
    /// substitutions or type context.
    ///
    /// Returns `Some(value)` if the expression can be fully evaluated at
    /// compile time, or `None` if evaluation requires runtime information
    /// (e.g. variable values, function calls) or would cause overflow/panic
    /// (callers using this entry point defer such cases to the runtime check).
    pub(crate) fn try_evaluate_const(&mut self, inst_ref: InstRef) -> Option<ConstValue> {
        let mut env = ComptimeEnv::new();
        self.eval_const_expr(inst_ref, &mut env).ok().flatten()
    }

    /// Like [`try_evaluate_const`], but evaluated inside the function being
    /// analyzed: comptime parameters in scope (`ctx.comptime_*_vars`) resolve
    /// to their values, and arithmetic is checked at HM-resolved types.
    ///
    /// [`try_evaluate_const`]: Self::try_evaluate_const
    pub(crate) fn try_evaluate_const_in_fn(
        &mut self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
    ) -> Option<ConstValue> {
        let mut env = ComptimeEnv::for_analysis(ctx);
        self.eval_const_expr(inst_ref, &mut env).ok().flatten()
    }

    pub(crate) fn try_evaluate_const_with_resolved_types_and_membership(
        &mut self,
        inst_ref: InstRef,
        resolved_types: &AHashMap<InstRef, Type>,
        type_subst: Option<&AHashMap<Spur, Type>>,
        value_subst: Option<&AHashMap<Spur, ConstValue>>,
        runtime_membership: std::sync::Arc<dyn Fn(&Spur) -> bool>,
        expected_result: Option<Type>,
    ) -> Option<ConstValue> {
        let empty_types = AHashMap::new();
        let empty_values = AHashMap::new();
        let mut env = ComptimeEnv::with_subst(
            type_subst.unwrap_or(&empty_types),
            value_subst.unwrap_or(&empty_values),
        );
        env.resolved_types = Some(resolved_types);
        env.runtime_local_name_membership = Some(runtime_membership);
        env.defining_file = Some(self.body_rir_ref().get(inst_ref).span.file_id);
        env.expected_result = expected_result;
        env.canonical_identity = self.active_anonymous_producer().cloned();
        self.eval_const_expr(inst_ref, &mut env).ok().flatten()
    }

    pub(crate) fn select_comptime_branch_with_resolved_types_and_membership(
        &mut self,
        condition: InstRef,
        resolved_types: &AHashMap<InstRef, Type>,
        type_subst: Option<&AHashMap<Spur, Type>>,
        value_subst: Option<&AHashMap<Spur, ConstValue>>,
        runtime_membership: std::sync::Arc<dyn Fn(&Spur) -> bool>,
    ) -> CompileResult<Option<bool>> {
        let empty_types = AHashMap::new();
        let empty_values = AHashMap::new();
        let mut env = ComptimeEnv::with_subst(
            type_subst.unwrap_or(&empty_types),
            value_subst.unwrap_or(&empty_values),
        );
        env.resolved_types = Some(resolved_types);
        env.runtime_local_name_membership = Some(runtime_membership);
        env.defining_file = Some(self.body_rir_ref().get(condition).span.file_id);
        env.canonical_identity = self.active_anonymous_producer().cloned();
        match ComptimeEngine::new(self).select_branch((), condition, &mut env) {
            ComptimeOutcome::Known(crate::sema::ComptimeSelection::Branch { taken }) => {
                Ok(Some(taken))
            }
            ComptimeOutcome::RuntimeDependent
            | ComptimeOutcome::NotReady
            | ComptimeOutcome::UnsupportedContext => Ok(None),
            ComptimeOutcome::Trap(trap) => Err(self.trap_failure(trap)),
            ComptimeOutcome::HostFailure(error) | ComptimeOutcome::Abort(error) => Err(error),
            ComptimeOutcome::Known(ComptimeSelection::Match { .. }) => Ok(None),
        }
    }

    pub(crate) fn select_comptime_match_with_resolved_types_and_membership(
        &mut self,
        scrutinee: InstRef,
        arms: &rue_rir::RirMatchArmsRange,
        resolved_types: &AHashMap<InstRef, Type>,
        type_subst: Option<&AHashMap<Spur, Type>>,
        value_subst: Option<&AHashMap<Spur, ConstValue>>,
        runtime_membership: std::sync::Arc<dyn Fn(&Spur) -> bool>,
    ) -> CompileResult<Option<usize>> {
        let empty_types = AHashMap::new();
        let empty_values = AHashMap::new();
        let mut env = ComptimeEnv::with_subst(
            type_subst.unwrap_or(&empty_types),
            value_subst.unwrap_or(&empty_values),
        );
        env.resolved_types = Some(resolved_types);
        env.runtime_local_name_membership = Some(runtime_membership);
        env.defining_file = Some(self.body_rir_ref().get(scrutinee).span.file_id);
        env.canonical_identity = self.active_anonymous_producer().cloned();
        match ComptimeEngine::new(self).select_match((), scrutinee, arms, &mut env) {
            ComptimeOutcome::Known(ComptimeSelection::Match { arm }) => Ok(Some(arm)),
            ComptimeOutcome::RuntimeDependent
            | ComptimeOutcome::NotReady
            | ComptimeOutcome::UnsupportedContext => Ok(None),
            ComptimeOutcome::Trap(trap) => Err(self.trap_failure(trap)),
            ComptimeOutcome::HostFailure(error) | ComptimeOutcome::Abort(error) => Err(error),
            ComptimeOutcome::Known(ComptimeSelection::Branch { .. }) => Ok(None),
        }
    }

    /// Evaluate an expression in the current function while preserving hard
    /// diagnostics. Required comptime arguments use this entry point: preview
    /// gates, privacy failures, and constant operations that would panic are
    /// source errors, not evidence that the argument is merely runtime.
    pub(crate) fn evaluate_const_in_fn(
        &mut self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
    ) -> CompileResult<Option<ConstValue>> {
        let mut env = ComptimeEnv::for_analysis(ctx);
        self.eval_const_expr(inst_ref, &mut env)
    }

    /// Try to evaluate an RIR instruction to a compile-time constant value
    /// with type/value substitution.
    ///
    /// This is used when evaluating generic functions that return `type`. For
    /// example, when calling `fn Pair(comptime T: type) -> type { struct {
    /// first: T, second: T } }` with `Pair(i32)`, we need to substitute
    /// `T -> i32` when evaluating the body.
    pub(crate) fn try_evaluate_const_with_subst(
        &mut self,
        inst_ref: InstRef,
        type_subst: &AHashMap<Spur, Type>,
        value_subst: &AHashMap<Spur, ConstValue>,
    ) -> Option<ConstValue> {
        let mut env = ComptimeEnv::with_subst(type_subst, value_subst);
        env.canonical_identity = self.active_anonymous_producer().cloned();
        // A module-qualified comptime call in the evaluated expression resolves
        // its receiver against the expression's own file's imports (RUE-511).
        env.defining_file = Some(self.body_rir_ref().get(inst_ref).span.file_id);
        self.eval_const_expr(inst_ref, &mut env).ok().flatten()
    }

    /// Try to extract a constant integer value from an RIR index expression.
    ///
    /// This is used for compile-time bounds checking. Returns `Some(value)` if
    /// the index can be evaluated to an integer constant at compile time.
    pub(crate) fn try_get_const_index(&mut self, inst_ref: InstRef) -> Option<i64> {
        self.try_evaluate_const(inst_ref)?.as_integer()
    }

    /// Like [`try_get_const_index`], but evaluated inside the function being
    /// analyzed, so the index is checked at its HM-resolved operand types.
    ///
    /// The type-unaware [`try_get_const_index`] folds `arr[X + 1]` as raw
    /// `i128` and only the array-length bound is checked — so `X + 1` where
    /// `X: i8 = 127` folds to `128`, fits a length-129 array, and the operand
    /// overflow is silently deferred to a runtime panic (RUE-234). Threading
    /// the resolved operand types (as [`try_evaluate_const_in_fn`] does)
    /// surfaces that overflow as a compile-time E1200/E0800 — the same check
    /// the const-initializer path gets (RUE-230) — *before* the length bound is
    /// consulted. Returns:
    /// - `Ok(Some(i))` — a compile-time-known, in-operand-type index `i`;
    /// - `Ok(None)`    — not a compile-time constant (runtime index);
    /// - `Err(..)`     — the index expression overflows at its operand type.
    ///
    /// [`try_get_const_index`]: Self::try_get_const_index
    /// [`try_evaluate_const_in_fn`]: Self::try_evaluate_const_in_fn
    pub(crate) fn try_get_const_index_checked(
        &mut self,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<i128>> {
        let scope = ctx.checked_const_index_scope_key();
        let cache_key = AnalysisContext::checked_const_index_cache_key(inst_ref, scope.clone());
        if let Some(value) = ctx
            .checked_const_index_cache
            .borrow()
            .get(&cache_key)
            .copied()
        {
            #[cfg(test)]
            CHECKED_CONST_INDEX_HIT_REACHED.with(|reached| reached.set(true));
            // A memo hit must not let retained work bypass the owning query's
            // cancellation boundary.
            let cancellation = self.check_canceled();
            #[cfg(test)]
            if cancellation.is_err() {
                update_checked_const_index_test_stats(|stats| stats.canceled_hits += 1);
            }
            cancellation?;
            ctx.checked_const_index_cache_hits
                .set(ctx.checked_const_index_cache_hits.get().saturating_add(1));
            #[cfg(test)]
            update_checked_const_index_test_stats(|stats| stats.hits += 1);
            return Ok(Some(value));
        }
        let span = self.body_rir_ref().get(inst_ref).span;
        let candidate_key = AnalysisContext::checked_const_index_candidate_key(span);
        let candidates = ctx
            .checked_const_index_candidates
            .borrow()
            .get(&candidate_key)
            .cloned()
            .unwrap_or_default();
        for candidate in candidates {
            ctx.checked_const_index_candidate_comparisons.set(
                ctx.checked_const_index_candidate_comparisons
                    .get()
                    .saturating_add(1),
            );
            let (equivalent, nodes) =
                self.checked_const_index_expressions_equivalent(&candidate, inst_ref, ctx);
            ctx.checked_const_index_comparison_nodes.set(
                ctx.checked_const_index_comparison_nodes
                    .get()
                    .saturating_add(nodes),
            );
            #[cfg(test)]
            update_checked_const_index_test_stats(|stats| {
                stats.candidate_comparisons += 1;
                stats.comparison_nodes = stats.comparison_nodes.saturating_add(nodes);
            });
            if equivalent {
                #[cfg(test)]
                CHECKED_CONST_INDEX_HIT_REACHED.with(|reached| reached.set(true));
                let cancellation = self.check_canceled();
                #[cfg(test)]
                if cancellation.is_err() {
                    update_checked_const_index_test_stats(|stats| stats.canceled_hits += 1);
                }
                cancellation?;
                ctx.checked_const_index_cache
                    .borrow_mut()
                    .insert(cache_key, candidate.value);
                ctx.checked_const_index_cache_hits
                    .set(ctx.checked_const_index_cache_hits.get().saturating_add(1));
                #[cfg(test)]
                update_checked_const_index_test_stats(|stats| {
                    stats.hits += 1;
                    stats.candidate_hits += 1;
                });
                return Ok(Some(candidate.value));
            }
            #[cfg(test)]
            update_checked_const_index_test_stats(|stats| stats.candidate_rejections += 1);
        }
        ctx.checked_const_index_evaluations
            .set(ctx.checked_const_index_evaluations.get().saturating_add(1));
        #[cfg(test)]
        update_checked_const_index_test_stats(|stats| stats.evaluations += 1);
        let mut env = ComptimeEnv::for_analysis(ctx);
        // Full i128 backing value, NOT the i64 narrowing: `as_integer()`
        // returns None for a u64 constant above i64::MAX, which made an
        // exactly-known out-of-bounds index (`a[18446744073709551615]`)
        // indistinguishable from a runtime index and skip the compile-time
        // bounds check (RUE-532).
        let value = self
            .eval_const_expr(inst_ref, &mut env)?
            .and_then(|v| v.as_int_value());
        drop(env);
        if let Some(value) = value {
            ctx.checked_const_index_cache
                .borrow_mut()
                .insert(cache_key, value);
            ctx.checked_const_index_candidates
                .borrow_mut()
                .entry(candidate_key)
                .or_default()
                .push(CheckedConstIndexCandidate {
                    root: inst_ref,
                    value,
                    runtime_local_names: ctx.locals.keys().copied().collect(),
                    comptime_type_vars: ctx.comptime_type_vars.snapshot(),
                });
        }
        Ok(value)
    }

    /// Typed equivalence for the expression forms admitted to cross-root reuse.
    /// Unsupported forms conservatively miss and evaluate normally. This work
    /// runs only after a successful same-span candidate exists. Failed probes
    /// are never admitted to either cache; an ordinary failure with no prior
    /// candidate avoids structural comparison entirely.
    fn checked_const_index_expressions_equivalent(
        &self,
        candidate: &CheckedConstIndexCandidate,
        right: InstRef,
        ctx: &AnalysisContext,
    ) -> (bool, u64) {
        let mut pending = vec![(candidate.root, right)];
        let mut visited = AHashSet::new();
        let mut nodes = 0_u64;
        while let Some((left, right)) = pending.pop() {
            if !visited.insert((left, right)) {
                continue;
            }
            nodes = nodes.saturating_add(1);
            let left_inst = self.body_rir_ref().get(left);
            let right_inst = self.body_rir_ref().get(right);
            if left_inst.span != right_inst.span
                || ctx.resolved_type_of(left) != ctx.resolved_type_of(right)
            {
                return (false, nodes);
            }
            let equivalent = match (&left_inst.data, &right_inst.data) {
                (InstData::IntConst(left), InstData::IntConst(right)) => left == right,
                (InstData::BoolConst(left), InstData::BoolConst(right)) => left == right,
                (InstData::UnitConst, InstData::UnitConst) => true,
                (
                    InstData::VarRef {
                        name: left_name, ..
                    },
                    InstData::VarRef {
                        name: right_name, ..
                    },
                ) => {
                    left_name == right_name
                        && candidate.runtime_local_names.contains(left_name)
                            == ctx.locals.contains_key(right_name)
                        && candidate.comptime_type_vars.get(left_name)
                            == ctx.comptime_type_vars.get(right_name)
                }
                (
                    InstData::FieldGet {
                        base: left_base,
                        field: left_field,
                    },
                    InstData::FieldGet {
                        base: right_base,
                        field: right_field,
                    },
                ) => {
                    pending.push((*left_base, *right_base));
                    left_field == right_field
                }
                (InstData::Neg { operand: left }, InstData::Neg { operand: right })
                | (InstData::Not { operand: left }, InstData::Not { operand: right })
                | (InstData::BitNot { operand: left }, InstData::BitNot { operand: right })
                | (InstData::Comptime { expr: left }, InstData::Comptime { expr: right }) => {
                    pending.push((*left, *right));
                    true
                }
                (InstData::Add { lhs: ll, rhs: lr }, InstData::Add { lhs: rl, rhs: rr })
                | (InstData::Sub { lhs: ll, rhs: lr }, InstData::Sub { lhs: rl, rhs: rr })
                | (InstData::Mul { lhs: ll, rhs: lr }, InstData::Mul { lhs: rl, rhs: rr })
                | (InstData::Div { lhs: ll, rhs: lr }, InstData::Div { lhs: rl, rhs: rr })
                | (InstData::Mod { lhs: ll, rhs: lr }, InstData::Mod { lhs: rl, rhs: rr })
                | (InstData::BitAnd { lhs: ll, rhs: lr }, InstData::BitAnd { lhs: rl, rhs: rr })
                | (InstData::BitOr { lhs: ll, rhs: lr }, InstData::BitOr { lhs: rl, rhs: rr })
                | (InstData::BitXor { lhs: ll, rhs: lr }, InstData::BitXor { lhs: rl, rhs: rr })
                | (InstData::Shl { lhs: ll, rhs: lr }, InstData::Shl { lhs: rl, rhs: rr })
                | (InstData::Shr { lhs: ll, rhs: lr }, InstData::Shr { lhs: rl, rhs: rr }) => {
                    pending.extend([(*lr, *rr), (*ll, *rl)]);
                    true
                }
                (
                    InstData::TypeIntrinsic {
                        name: left_name,
                        type_arg: left_type,
                    },
                    InstData::TypeIntrinsic {
                        name: right_name,
                        type_arg: right_type,
                    },
                ) => {
                    let syntax_equivalent = self
                        .checked_const_index_named_type_syntax_equivalent(*left_type, *right_type);
                    let binding_equivalent = self
                        .checked_const_index_named_type_syntax_symbol(*left_type)
                        .zip(self.checked_const_index_named_type_syntax_symbol(*right_type))
                        .is_none_or(|(left, right)| {
                            left == right
                                && candidate.comptime_type_vars.get(&left)
                                    == ctx.comptime_type_vars.get(&right)
                        });
                    left_name == right_name && syntax_equivalent && binding_equivalent
                }
                (
                    InstData::OffsetOf {
                        type_arg: left_type,
                        field: left_field,
                    },
                    InstData::OffsetOf {
                        type_arg: right_type,
                        field: right_field,
                    },
                ) => {
                    let syntax_equivalent = self
                        .checked_const_index_named_type_syntax_equivalent(*left_type, *right_type);
                    let binding_equivalent = self
                        .checked_const_index_named_type_syntax_symbol(*left_type)
                        .zip(self.checked_const_index_named_type_syntax_symbol(*right_type))
                        .is_none_or(|(left, right)| {
                            left == right
                                && candidate.comptime_type_vars.get(&left)
                                    == ctx.comptime_type_vars.get(&right)
                        });
                    left_field == right_field && syntax_equivalent && binding_equivalent
                }
                _ => false,
            };
            if !equivalent {
                return (false, nodes);
            }
        }
        (true, nodes)
    }

    /// The checked-index cases needing type-syntax reuse currently name a
    /// comptime alias or generic parameter. Compare that spelling through the
    /// typed arena; every richer syntax conservatively evaluates again.
    fn checked_const_index_named_type_syntax_equivalent(
        &self,
        left: rue_rir::RirTypeSyntaxRef,
        right: rue_rir::RirTypeSyntaxRef,
    ) -> bool {
        use rue_rir::RirTypeSyntaxNode;

        let arena = self.body_rir_ref().type_syntax();
        match (arena.node(left), arena.node(right)) {
            (Some(RirTypeSyntaxNode::Named(left)), Some(RirTypeSyntaxNode::Named(right))) => {
                arena.symbol(*left) == arena.symbol(*right)
            }
            (Some(RirTypeSyntaxNode::Unit), Some(RirTypeSyntaxNode::Unit))
            | (Some(RirTypeSyntaxNode::Never), Some(RirTypeSyntaxNode::Never)) => true,
            _ => false,
        }
    }

    fn checked_const_index_named_type_syntax_symbol(
        &self,
        reference: rue_rir::RirTypeSyntaxRef,
    ) -> Option<Spur> {
        use rue_rir::RirTypeSyntaxNode;

        let arena = self.body_rir_ref().type_syntax();
        let RirTypeSyntaxNode::Named(symbol) = arena.node(reference)? else {
            return None;
        };
        arena.symbol(*symbol).copied()
    }

    /// Check if an RIR instruction is a direct reference to a `comptime`
    /// parameter of the function currently being analyzed.
    ///
    /// Such a reference is compile-time known to every caller, so it may be
    /// forwarded to another function's comptime parameter (spec 4.14:5):
    ///
    /// ```rue
    /// fn g(comptime m: i32) -> i32 { m * 2 }
    /// fn f(comptime n: i32) -> i32 { g(n) }  // forwarding
    /// ```
    ///
    /// Since per-value specialization (RUE-166), bodies with comptime value
    /// parameters are only analyzed with the concrete values in
    /// `ctx.comptime_value_vars`, so forwards normally evaluate directly and
    /// this check is a fallback.
    pub(crate) fn is_comptime_param_forward(
        &self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
    ) -> bool {
        if let InstData::VarRef { name, .. } = &self.body_rir_ref().get(inst_ref).data {
            // A runtime local of the same name shadows the parameter.
            !ctx.locals.contains_key(name)
                && ctx.param(*name).is_some_and(|param| param.is_comptime)
        } else {
            false
        }
    }

    /// The resolved integer type of an expression, when known.
    fn const_expr_type(&self, env: &ComptimeEnv, inst_ref: InstRef) -> Option<Type> {
        env.resolved_types?
            .get(&inst_ref)
            .copied()
            .filter(Type::is_integer)
    }

    /// Finish an arithmetic operation using the kernel's typed result and its
    /// available raw mathematical result for diagnostics.
    ///
    /// - Typed: out-of-range results are a hard error (the operation would
    ///   panic at runtime, spec 8.1 / 4.14:4).
    /// - Untyped fallback: results outside the `i64` range make the
    ///   expression non-evaluable (legacy checked-i64 semantics).
    fn finish_arith(
        &self,
        result: CheckedIntegerResult,
        ty: Option<Type>,
        op: &str,
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
        match ty {
            Some(ty) => match result.checked() {
                Some(v) => Ok(Some(ConstValue::Integer(v))),
                _ => {
                    let ty_name = self.format_type_name(ty);
                    let detail = match result.raw() {
                        Some(v) => format!("the result {} does not fit in {}", v, ty_name),
                        None => format!("the result does not fit in {}", ty_name),
                    };
                    Err(comptime_panic_err(
                        format!(
                            "integer overflow evaluating `{}` at type {}: {} \
                             (this operation would panic at runtime)",
                            op, ty_name, detail
                        ),
                        span,
                    ))
                }
            },
            None => match result.raw() {
                Some(v) if v >= i128::from(i64::MIN) && v <= i128::from(i64::MAX) => {
                    Ok(Some(ConstValue::Integer(v)))
                }
                _ => Ok(None),
            },
        }
    }

    /// Resolve a bare name used as a type value through the canonical type
    /// resolver. Synthetic type values such as `str` are registered lazily, so looking
    /// only in the already-populated struct and enum maps made their first use
    /// as a generic type argument spuriously non-constant.
    fn resolve_named_type_value(&mut self, name: Spur, span: Span) -> CompileResult<Option<Type>> {
        match self.resolve_type(name, span) {
            Ok(ty) => Ok(Some(ty)),
            Err(error) if matches!(error.kind, ErrorKind::UnknownType(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Thin adapter into the canonical compile-time dispatcher.
    pub(crate) fn eval_const_expr(
        &mut self,
        inst_ref: InstRef,
        env: &mut ComptimeEnv,
    ) -> CompileResult<Option<ConstValue>> {
        ComptimeEngine::new(self)
            .evaluate(ComptimeFrame::expression((), inst_ref), env)
            .into_result(|trap| self.trap_failure(trap))
    }

    fn trap_failure(&self, trap: ComptimeTrap) -> CompileError {
        comptime_panic_err(
            format!("{} (this operation would panic at runtime)", trap.operation),
            trap.span,
        )
    }

    /// Build the exact body-local key for one admitted callable. Parameter
    /// order is part of the key's representation: the maps are convenient
    /// binding state, but their hash iteration order is intentionally not a
    /// semantic identity. The stable producer distinguishes callable and
    /// imported/generic identities; the target distinguishes target-sensitive
    /// intrinsic results; the concrete substitutions distinguish each state.
    fn comptime_reduction_key(
        &self,
        name: Spur,
        identity: super::anon_structs::IssuedStableProducerId,
        type_bindings: &AHashMap<Spur, Type>,
        value_bindings: &AHashMap<Spur, ConstValue>,
    ) -> Option<
        ComptimeCallKey<
            super::anon_structs::IssuedStableProducerId,
            rue_target::Target,
            Type,
            ConstValue,
        >,
    > {
        let info = self.function_info(name)?;
        let param_data = self.body_param_data(info.params);
        let type_flags = self.comptime_type_param_flags(&info);
        let mut type_arguments = Vec::new();
        let mut value_arguments = Vec::new();
        for (index, (parameter, _, _, is_comptime)) in param_data.iter().enumerate() {
            if !*is_comptime {
                continue;
            }
            if type_flags[index] {
                type_arguments.push(*type_bindings.get(parameter)?);
            } else {
                value_arguments.push(*value_bindings.get(parameter)?);
            }
        }
        let key = ComptimeCallKey {
            declaration: identity,
            configuration: self.target(),
            type_arguments: type_arguments.into(),
            value_arguments: value_arguments.into(),
        };
        #[cfg(test)]
        self.record_comptime_reduction_key(&key);
        Some(key)
    }

    /// Complete a child call after the engine has evaluated its body. This
    /// hook owns only semantic bookkeeping; it never walks RIR or starts a
    /// second evaluator.
    fn finish_comptime_call(
        &mut self,
        frame: &ComptimeFrame<
            ConstValue,
            Type,
            Spur,
            FileId,
            (),
            super::anon_structs::IssuedStableProducerId,
        >,
        _ticket: (),
        result: ComptimeOutcome<ConstValue, CompileError>,
    ) -> ComptimeOutcome<ConstValue, CompileError> {
        #[cfg(test)]
        if !matches!(result, ComptimeOutcome::Known(_)) {
            self.record_non_successful_comptime_completion();
        }
        if let (Some(name), ComptimeOutcome::Known(ConstValue::Type(ty))) = (frame.name, &result) {
            self.record_ctor_type_display(name, *ty, &frame.type_bindings, &frame.value_bindings);
        }
        if let (Some(identity), ComptimeOutcome::Known(value)) =
            (frame.call_identity.clone(), &result)
        {
            if let Some(name) = frame.name
                && let Some(key) = self.comptime_reduction_key(
                    name,
                    identity,
                    &frame.type_bindings,
                    &frame.value_bindings,
                )
            {
                self.memoize_comptime_reduction(key, *value);
            }
        }
        result
    }

    /// Resolve a decoded module path to its semantic callable key. The engine
    /// has already decoded the receiver's RIR shape and applied lexical
    /// shadowing; this hook performs only declaration/visibility lookup.
    fn resolve_module_comptime_callable(
        &mut self,
        file_id: FileId,
        segments: &[Spur],
        method: Spur,
        span: Span,
    ) -> CompileResult<Option<Spur>> {
        let recv_name = segments[0];
        let module_file_id = if segments.len() == 1 {
            // Resolve through the declaration namespace, not the raw binding
            // table: while declarations are being bound, the defining file's
            // import constant may not be collected yet. A struct field like
            // `index: std.strmap.StrMap(u64)` reduces `StrMap`'s body during
            // field resolution, and the nested `let U64s = arraybuf.ArrayBuf(..)`
            // inside that body names an import of *strmap's* file that nothing
            // has demanded before — a raw lookup miss here silently made the
            // whole reduction non-evaluable and surfaced as E1200 (RUE-993).
            // The re-export chain branch below already resolves on demand.
            let Some(binding) = self.resolve_module_binding_in_file(file_id, recv_name)? else {
                return Ok(None);
            };
            let Some(module_id) = binding.ty.as_module() else {
                return Ok(None);
            };
            let module_def = self.module_def(module_id);
            let module_file_id = module_def.file_id;
            module_file_id
        } else {
            let segment_strings: Vec<String> = segments
                .iter()
                .map(|s| self.body_interner().resolve(s).to_owned())
                .collect();
            let segments: Vec<&str> = segment_strings.iter().map(String::as_str).collect();
            // Walk failures (unknown member, non-module segment, privacy) make
            // the call non-evaluable here; the caller reports the comptime
            // failure and sema's other paths carry the precise diagnostics.
            let Some((_, Some(module_file_id), _)) = self
                .resolve_type_module_prefix_in_file(file_id, &segments, span)
                .ok()
            else {
                return Ok(None);
            };
            module_file_id
        };
        // Body analysis reads a closed declaration namespace: membership is
        // read-only and a missing signature is authoritative.
        let Some(function_key) = self.resolve_function_name_local(method, module_file_id) else {
            return Ok(None);
        };
        let Some(fn_info) = self
            .function_info(function_key)
            .filter(|info| info.file_id == module_file_id)
        else {
            return Ok(None);
        };
        // Visibility: a non-`pub` member accessed through a module object is not
        // usable from another directory (spec 10.3:7).
        let member_name = self.body_interner().resolve(&method).to_string();
        self.check_unqualified_visibility(
            "function",
            &member_name,
            fn_info.file_id,
            fn_info.is_pub,
            span,
        )?;
        Ok(Some(function_key))
    }

    /// Reduce a member-access chain (`std.strbuf.StrBuf`) that appears in
    /// *value position* — a comptime type-constructor argument — to its nominal
    /// type value (RUE-948). The `FieldGet` arm of [`eval_const_expr`] reaches
    /// here only after the chain was not found among the pre-resolved
    /// `const_module_members`, so this is the type-path case: `spec 4.14:26`
    /// treats a chain of const/import member accesses as comptime-evaluable, and
    /// the qualified type-annotation position already resolves the identical
    /// spelling. Route the value-position case through the same walker so
    /// `Result(std.strbuf.StrBuf, i32)` behaves like the accepted return-type
    /// form `-> Result(std.strbuf.StrBuf, i32)` and the alias workaround
    /// (`let S = std.strbuf.StrBuf; Result(S, i32)`).
    ///
    /// Returns `Ok(None)` (non-evaluable) when the chain is not a module type
    /// path — a shadowed root, no file context, a runtime value's field, or a
    /// member that is not a type — leaving the caller to report it. A privacy
    /// violation on an otherwise-valid path surfaces as its real diagnostic.
    fn resolve_comptime_type_path(
        &mut self,
        root_file: FileId,
        segments: &[Spur],
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
        // The engine has already decoded the RIR spine and applied lexical
        // shadowing. This hook performs only declaration/type resolution on
        // the copied semantic path facts.
        let segment_strings: Vec<String> = segments
            .iter()
            .map(|s| self.body_interner().resolve(s).to_owned())
            .collect();
        let segments: Vec<&str> = segment_strings.iter().map(String::as_str).collect();
        match self.resolve_qualified_type_name_in_file(root_file, &segments, span) {
            Ok(ty) => Ok(Some(ConstValue::Type(ty))),
            // An unknown member / non-module base is simply not a type path
            // here; defer to the caller (a genuine runtime field access, or the
            // comptime-arg check's E1201). Other errors — a privacy violation
            // on a real member — are hard diagnostics that must surface.
            Err(error)
                if matches!(
                    error.kind,
                    ErrorKind::UnknownType(_) | ErrorKind::UnknownModuleMember { .. }
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Tailored E1201 help for the member-access paths that remain
    /// non-evaluable in comptime-argument position after RUE-948's type-path
    /// fix: a module-qualified path (`lib.nums.K`) that names a compile-time
    /// *value* — a re-exported `const` — rather than a type. Such a value is
    /// genuinely compile-time known, so the generic "requires a compile-time
    /// known value" wording is wrong-headed; the real limitation is that this
    /// version does not yet fold a module-member value directly in argument
    /// position (a `let`-local of it is a runtime binding and does not help,
    /// but a file-level `const` alias does).
    ///
    /// Returns `None` for any other argument shape — a runtime `let`/parameter
    /// reference, an overflowed literal, an aggregate value — so those keep the
    /// generic help. Only a `FieldGet` chain rooted at an *unshadowed* module
    /// import of the current file qualifies.
    pub(crate) fn comptime_arg_member_access_help(
        &self,
        arg: InstRef,
        ctx: &AnalysisContext,
    ) -> Option<String> {
        // Walk the FieldGet chain to its root, collecting the dotted spine.
        let mut fields_rev: Vec<Spur> = Vec::new();
        let mut cursor = arg;
        let root_name = loop {
            match self.body_rir_ref().get(cursor).data {
                InstData::VarRef { name, .. } => break name,
                InstData::FieldGet { base, field } => {
                    fields_rev.push(field);
                    cursor = base;
                }
                _ => return None,
            }
        };
        // A bare `VarRef` (no field) is a plain runtime binding, not a path.
        if fields_rev.is_empty() {
            return None;
        }
        // A runtime local or parameter of the same name shadows the import, so
        // this is an ordinary field access on a value — the generic help is
        // correct there.
        if ctx.locals.contains_key(&root_name) || ctx.has_param(root_name) {
            return None;
        }
        // The root must name a module import of the current file for this to be
        // a module-qualified member-access path at all.
        let binding = self.module_binding(&(ctx.current_file_id, root_name))?;
        binding.ty.as_module()?;
        let mut segments: Vec<&str> = vec![self.body_interner().resolve(&root_name)];
        segments.extend(
            fields_rev
                .iter()
                .rev()
                .map(|s| self.body_interner().resolve(s)),
        );
        let path = segments.join(".");
        Some(format!(
            "the module-qualified path `{path}` names a compile-time value that \
             this version does not yet evaluate directly in a comptime-argument \
             position; bind it to a file-level `const` and pass that \
             (`const X = {path};` then use `X`)"
        ))
    }

    /// The `@require_droppable(T)` well-formedness gate for owning growable
    /// containers (RUE-388, Steve's 2026-07-09 ruling).
    ///
    /// A source-level owning container such as `std/arraybuf.rue`'s `ArrayBuf(T)`
    /// owns a heap buffer of `T`. It runs each live element's drop glue in
    /// ascending index order before freeing the buffer (Rust's `Vec<T>` drop
    /// discipline, RUE-646), so droppable-but-non-linear elements — nested
    /// `ArrayBuf`, `ArrayBuf(StrBuf)`, lists of `drop fn` structs — are legal.
    /// It does **not** yet track element linearity, so a `linear` element (which
    /// must be consumed exactly once, not merely dropped) would be leaked; this
    /// gate rejects only that class:
    ///
    /// - `linear` (transitively — infectious linearity, spec 3.8:57): E0499.
    ///
    /// Until container/element multiplicity propagation is designed (its own
    /// future ADR — deliberately out of scope here), linear elements stay
    /// rejected. A droppable element (primitives, pointers, `Copy` structs,
    /// destructor-bearing structs, nested containers) passes.
    pub(crate) fn check_require_droppable(&mut self, ty: Type, span: Span) -> CompileResult<()> {
        if self.declaration_binding_active() {
            match self.known_linear_during_binding(ty) {
                Some(true) => return Err(self.require_droppable_error(ty, span)),
                Some(false) => return Ok(()),
                None => {
                    self.defer_ownership_gate(
                        DeferredOwnershipGateKind::RequireDroppable,
                        ty,
                        span,
                    );
                    return Ok(());
                }
            }
        }
        self.check_require_droppable_finalized(ty, span)
    }

    fn check_require_droppable_finalized(&self, ty: Type, span: Span) -> CompileResult<()> {
        if self.type_carries_linear(ty) {
            return Err(self.require_droppable_error(ty, span));
        }
        // Droppable-but-non-linear element types are accepted (RUE-646): the
        // container runs each live element's drop glue before freeing its
        // buffer. Linear element types stay rejected because they require a
        // consuming discharge rather than ordinary drop glue.
        Ok(())
    }

    fn require_droppable_error(&self, ty: Type, span: Span) -> CompileError {
        CompileError::new(
            ErrorKind::ContainerElementIsLinear {
                ty: self.format_type_name(ty),
            },
            span,
        )
    }

    /// The `@require_trivially_droppable(T)` gate for by-copy element *reads*
    /// (RUE-651). `ArrayBuf(T)`'s `get`/`get_or` return the element by copying it
    /// out with `@ptr_read` while leaving the slot live. For a `T` with drop glue
    /// (a destructor, or a field/payload/element that has one) that copy aliases
    /// the element's owned resources: both the copy and the still-live slot run
    /// drop glue at scope exit — a double-free. This gate rejects those reads at
    /// their call site (E0711); use `get_ref` to read it in place or *move* it
    /// out with `pop`/`pop_or`
    /// instead. It is deliberately placed in the `get`/`get_or` method bodies (not
    /// the constructor), so demand-driven analysis (ADR-0045) fires it only when a
    /// program actually calls a by-copy read — storing, pushing, popping, and
    /// dropping a drop-glue element stay legal (RUE-646). Mirrors Swift's rule
    /// that a non-copyable element cannot use a by-value `get` subscript.
    ///
    /// A `linear` `T` never reaches here: `@require_droppable` already rejects it
    /// at instantiation, so `ArrayBuf(linear)` cannot be constructed to be read.
    pub(crate) fn check_trivially_droppable(&mut self, ty: Type, span: Span) -> CompileResult<()> {
        if self.declaration_binding_active() {
            match self.known_drop_glue_during_binding(ty) {
                Some(true) => return Err(self.trivially_droppable_error(ty, span)),
                Some(false) => return Ok(()),
                None => {
                    self.defer_ownership_gate(
                        DeferredOwnershipGateKind::RequireTriviallyDroppable,
                        ty,
                        span,
                    );
                    return Ok(());
                }
            }
        }
        self.check_trivially_droppable_finalized(ty, span)
    }

    fn check_trivially_droppable_finalized(&self, ty: Type, span: Span) -> CompileResult<()> {
        if self.type_has_drop_glue(ty) {
            return Err(self.trivially_droppable_error(ty, span));
        }
        Ok(())
    }

    fn trivially_droppable_error(&self, ty: Type, span: Span) -> CompileError {
        CompileError::new(
            ErrorKind::ContainerElementNotTriviallyDroppable {
                ty: self.format_type_name(ty),
            },
            span,
        )
    }

    fn defer_ownership_gate(&mut self, kind: DeferredOwnershipGateKind, ty: Type, span: Span) {
        debug_assert!(self.declaration_binding_active());
        debug_assert!(self.type_ownership_depends_on_nominal(ty));
        let gate = DeferredOwnershipGate { kind, ty, span };
        if !self.deferred_ownership_gates_mut().contains(&gate) {
            self.deferred_ownership_gates_mut().push(gate);
        }
    }

    /// Admit a comptime-evaluable call without traversing child RIR. This is
    /// deliberately the complete call-site admission order: resolve and
    /// record the dependency, check arity and explicit modes, then apply the
    /// implicit-comptime eligibility gate. Argument expressions are evaluated
    /// only after this returns.
    fn admit_comptime_call(
        &mut self,
        name: Spur,
        arg_count: usize,
        arg_modes: &[ComptimeArgMode],
        env: &mut ComptimeEnv,
        name_is_resolved_key: bool,
    ) -> CompileResult<Option<ComptimeCallAdmission<FunctionCallInfo, Spur>>> {
        // During declaration binding, the callee may simply not be collected yet:
        // constant initializers and struct-field / enum-payload types can
        // evaluate before the source-order sweep reaches the callee's `FnDecl`
        // (RUE-603). Resolve only in the evaluating expression's defining file.
        let Some(file_id) = env.defining_file else {
            return Ok(None);
        };
        // Qualified callers have already resolved the source member through
        // the receiver module's defining file and pass the exact internal
        // function key here. Unqualified callers pass a source name, which
        // must be resolved by the current environment's defining file. Keep
        // those representations explicit so neither path can fall back to a
        // graph-global source-name lookup.
        let resolved = if name_is_resolved_key {
            self.function_info(name).map(|info| (name, info))
        } else {
            self.resolve_function_name_local(name, file_id)
                .and_then(|key| self.function_info(key).map(|info| (key, info)))
        };
        let Some((name_key, fn_info)) = resolved else {
            return Ok(None);
        };
        let is_type_fn = self.function_returns_type(&fn_info);
        self.record_body_named_dependency(super::NamedConstDependencyTargetEvent::FreeFunction {
            file: fn_info.file_id.index(),
            name: self
                .body_interner()
                .resolve(&self.source_function_name(name_key))
                .to_string(),
        });
        let params = fn_info.params;
        let param_data = self.body_param_data(params);
        let param_names = param_data.names().to_vec();
        let param_modes = param_data.modes().to_vec();
        let param_comptime = param_data.comptime().to_vec();
        if arg_count != param_names.len() {
            return Ok(None);
        }
        self.validate_explicit_call_modes_owned(arg_modes, param_modes.iter().copied())?;

        // Same gate as `analyze_call`'s implicit-comptime path. A `-> type`
        // constructor reduces with no args (a nullary type alias) or when every
        // parameter is comptime. A *value*-returning function reduces only when
        // it has at least one parameter and every one is comptime — the
        // comptime-recursion / forwarding shape (`fn fact(comptime n: i32)`,
        // RUE-163 facet 1). A nullary or runtime-parametered value function is a
        // genuine runtime call, not a compile-time-known value: folding
        // `get_value()` (a plain `fn get_value() -> i32`) would wrongly make it
        // acceptable as a `comptime` argument (spec 4.14:6).
        let all_params_comptime = !param_names.is_empty() && param_comptime.iter().all(|&c| c);
        let eligible = if is_type_fn {
            param_names.is_empty() || all_params_comptime
        } else {
            all_params_comptime
        };
        if !eligible {
            return Ok(None);
        }
        Ok(Some(ComptimeCallAdmission {
            name: name_key,
            payload: fn_info,
        }))
    }

    fn validate_explicit_call_modes_owned(
        &self,
        args: &[ComptimeArgMode],
        expected_modes: impl ExactSizeIterator<Item = rue_rir::RirParamMode>,
    ) -> CompileResult<()> {
        assert_eq!(args.len(), expected_modes.len());
        for ((actual, span), expected) in args.iter().copied().zip(expected_modes) {
            use rue_rir::RirArgMode;
            match (expected, actual) {
                (rue_rir::RirParamMode::Inout, RirArgMode::Inout)
                | (rue_rir::RirParamMode::Borrow, RirArgMode::Borrow)
                | (rue_rir::RirParamMode::Normal, RirArgMode::Normal) => {}
                (rue_rir::RirParamMode::Inout, _) => {
                    return Err(CompileError::new(ErrorKind::InoutKeywordMissing, span));
                }
                (rue_rir::RirParamMode::Borrow, _) => {
                    return Err(CompileError::new(ErrorKind::BorrowKeywordMissing, span));
                }
                (rue_rir::RirParamMode::Normal, actual) => {
                    let mode = match actual {
                        RirArgMode::Inout => "inout",
                        RirArgMode::Borrow => "borrow",
                        RirArgMode::Normal => unreachable!(),
                    };
                    return Err(
                        CompileError::new(ErrorKind::UnexpectedCallArgumentMode { mode }, span)
                            .with_help(format!(
                                "remove the `{mode}` keyword; this argument is passed without an explicit mode"
                            )),
                    );
                }
            }
        }
        Ok(())
    }

    /// Start an incremental binding transaction immediately after admission.
    fn begin_comptime_call_binding(
        &self,
        admission: &ComptimeCallAdmission<FunctionCallInfo, Spur>,
        _argument_count: usize,
        _span: Span,
    ) -> CompileResult<OrdinaryComptimeCallBinding> {
        let param_data = self.body_param_data(admission.payload.params);
        Ok(OrdinaryComptimeCallBinding {
            parameter_names: param_data.names().to_vec(),
            parameter_is_type: self.comptime_type_param_flags(&admission.payload),
            arguments: Vec::with_capacity(_argument_count),
        })
    }

    fn bind_comptime_call_argument(
        &self,
        binding: &mut OrdinaryComptimeCallBinding,
        argument: ComptimeCallArgument<ConstValue>,
        _index: usize,
        _span: Span,
    ) -> CompileResult<bool> {
        // Ordinary semantics validate the complete batch only at finish. This
        // owned transaction therefore cannot publish an early mismatch or
        // let it mask a later child trap/abort.
        Ok(push_ordinary_comptime_call_argument(
            binding,
            *argument.value(),
        ))
    }

    fn finish_comptime_call_binding(
        &mut self,
        binding: OrdinaryComptimeCallBinding,
        _span: Span,
    ) -> CompileResult<Option<OrdinaryComptimeBoundCall>> {
        Ok(finish_ordinary_comptime_call_binding(binding))
    }

    /// Finish an admitted local call after its arguments have been evaluated.
    /// Provider calls must be queried before this hook so their cached result
    /// or diagnostic remains authoritative.
    fn prepare_comptime_call(
        &mut self,
        admission: ComptimeCallAdmission<FunctionCallInfo, Spur>,
        callee_types: AHashMap<Spur, Type>,
        callee_values: AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<
        Option<
            ComptimeCallPreparation<
                ConstValue,
                Type,
                Spur,
                FileId,
                (),
                super::anon_structs::IssuedStableProducerId,
                CompileError,
                (),
            >,
        >,
    > {
        let name_key = admission.name;
        let fn_body_info = self.function_body_info(name_key);
        let Some(fn_body_info) = fn_body_info else {
            return Ok(None);
        };
        let fn_info = crate::sema::info::FunctionCallInfo::from_body(fn_body_info);
        self.validate_comptime_call_substitutions(
            name_key,
            &fn_info,
            &callee_types,
            &callee_values,
            fn_body_info.span,
        )?;
        Ok(Some(ComptimeCallPreparation::Enter {
            frame: ComptimeFrame {
                program: (),
                body: fn_body_info.body,
                name: Some(name_key),
                context: Some(fn_body_info.file_id),
                span,
                function_span: fn_body_info.span,
                type_bindings: callee_types,
                value_bindings: callee_values,
                name_bindings: AHashMap::new(),
                call_identity: None,
                expected_result: Some(fn_body_info.return_type),
            },
            ticket: (),
        }))
    }

    pub(crate) fn validate_comptime_value_for_type(
        &self,
        function_name: Spur,
        param_name: Spur,
        value: ConstValue,
        expected: Type,
        span: Span,
    ) -> CompileResult<()> {
        validate_comptime_value_for_type_impl(
            self.body_interner(),
            function_name,
            param_name,
            value,
            expected,
            span,
            |ty| self.format_type_name(ty),
        )
    }

    /// Validate the complete comptime argument contract at the shared
    /// reduction boundary. Binding paths may differ, but none may reduce a
    /// body until type/value kinds, dependent declared types, and integer
    /// ranges agree with the source declaration.
    fn validate_comptime_call_substitutions(
        &mut self,
        function_name: Spur,
        function: &crate::sema::info::FunctionCallInfo,
        callee_types: &AHashMap<Spur, Type>,
        callee_values: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<()> {
        let params = function.params;
        let param_data = self.body_param_data(params);
        let param_names = param_data.names().to_vec();
        let param_types = param_data.types().to_vec();
        let param_comptime = param_data.comptime().to_vec();
        let param_comptime_type = self.comptime_type_param_flags(function);

        let expected_type_args = param_comptime
            .iter()
            .zip(param_comptime_type.iter())
            .filter(|(is_comptime, is_type)| **is_comptime && **is_type)
            .count();
        let expected_value_args = param_comptime
            .iter()
            .zip(param_comptime_type.iter())
            .filter(|(is_comptime, is_type)| **is_comptime && !**is_type)
            .count();
        if callee_types.len() != expected_type_args || callee_values.len() != expected_value_args {
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "comptime argument maps for '{}' do not match its declaration: received {}/{} type/value arguments, expected {expected_type_args}/{expected_value_args}",
                    self.body_interner().resolve(&function_name),
                    callee_types.len(),
                    callee_values.len(),
                )),
                span,
            ));
        }

        for (index, ((name, declared), is_comptime)) in param_names
            .iter()
            .zip(param_types.iter())
            .zip(param_comptime.iter())
            .enumerate()
        {
            if !is_comptime {
                continue;
            }
            if param_comptime_type[index] {
                if !callee_types.contains_key(name) {
                    return Err(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "comptime type argument for '{}' is missing while reducing '{}'",
                            self.body_interner().resolve(name),
                            self.body_interner().resolve(&function_name)
                        )),
                        span,
                    ));
                }
                continue;
            }
            let value = callee_values.get(name).copied().ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "comptime value argument for '{}' is missing while reducing '{}'",
                        self.body_interner().resolve(name),
                        self.body_interner().resolve(&function_name)
                    )),
                    span,
                )
            })?;
            let expected = self.resolve_substituted_param_type(
                function,
                index,
                *declared,
                callee_types,
                callee_values,
                span,
            )?;

            self.validate_comptime_value_for_type(function_name, *name, value, expected, span)?;
        }
        Ok(())
    }

    /// Reduce a comptime-evaluable function body under concrete substitutions.
    /// This is the shared path for type constructors and value-returning
    /// comptime functions, so it validates the argument contract once before
    /// evaluation and guards recursive reductions with the specialization
    /// depth limit (RUE-163, RUE-241, RUE-261).
    pub(crate) fn reduce_type_ctor_body(
        &mut self,
        name: Spur,
        callee_types: &AHashMap<Spur, Type>,
        callee_values: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
        let Some(info) = self.function_info(name) else {
            return Ok(None);
        };
        let admission = ComptimeCallAdmission {
            name,
            payload: info,
        };
        let bound = OrdinaryComptimeBoundCall {
            callee_types: callee_types.clone(),
            callee_values: callee_values.clone(),
        };
        let preparation =
            <Self as ComptimeCallProtocol>::prepare_comptime_call(self, admission, bound, span)
                .map_err(super::comptime::ComptimeHostError::into_failure)?;
        let Some(preparation) = preparation else {
            return Ok(None);
        };
        match preparation {
            ComptimeCallPreparation::Memoized(outcome) => {
                outcome.into_result(|trap| self.trap_failure(trap))
            }
            ComptimeCallPreparation::Enter { frame, ticket } => ComptimeEngine::new(self)
                .evaluate_entered_frame(frame, ticket)
                .into_result(|trap| self.trap_failure(trap)),
        }
    }

    /// Record `Ctor(args...)` as the display name for an anonymous type just
    /// produced by reducing `ctor`'s body (RUE-610; see
    /// the host's constructor-display registry). Named types keep their
    /// declared names;
    /// a partial substitution records nothing rather than a wrong spelling.
    pub(crate) fn record_ctor_type_display(
        &mut self,
        ctor: Spur,
        ty: Type,
        callee_types: &AHashMap<Spur, Type>,
        callee_values: &AHashMap<Spur, ConstValue>,
    ) {
        // Membership in the pool's anonymous registry is the classifier, never
        // the name spelling (RUE-1050): a name-prefix test here silently broke
        // for enums when the spelling changed to `__anon_enum_…`, and a source
        // declaration is allowed to spell itself `__anon_struct_…` (RUE-125
        // reserves only `__rue_*`), so the prefix can also false-positive.
        let is_anon = match ty.kind() {
            TypeKind::Struct(id) => self.body_type_pool().is_anonymous_struct(id),
            TypeKind::Enum(id) => self.body_type_pool().is_anonymous_enum(id),
            _ => false,
        };
        if !is_anon || self.has_ctor_type_display(ty) {
            return;
        }
        let Some(fn_info) = self.function_info(ctor) else {
            return;
        };
        let param_names = self.body_param_data(fn_info.params).names().to_vec();
        let mut args: Vec<String> = Vec::with_capacity(param_names.len());
        for param in &param_names {
            if let Some(arg_ty) = callee_types.get(param) {
                args.push(self.format_type_name(*arg_ty));
            } else if let Some(value) = callee_values.get(param) {
                match value {
                    ConstValue::Integer(i) => args.push(i.to_string()),
                    ConstValue::Bool(b) => args.push(b.to_string()),
                    ConstValue::Type(t) => args.push(self.format_type_name(*t)),
                    _ => return,
                }
            } else {
                return;
            }
        }
        let source_name = self.source_function_name(ctor);
        let display = format!(
            "{}({})",
            self.body_interner().resolve(&source_name),
            args.join(", ")
        );
        self.record_body_ctor_type_display(ty, display);
    }

    /// Pre-resolve `let`-bound compile-time type aliases in a function body,
    /// before HM inference runs (RUE-170, RUE-164).
    ///
    /// A binding like `let P = F();` (where `F` returns `type`) only gets a
    /// concrete type during sema's analysis pass, but inference runs first.
    /// This walk evaluates those initializers eagerly so uses of `P` as a type
    /// name (`P { ... }`, `let p: P = ...`, or a method receiver) follow the
    /// same constraint paths as named structs.
    ///
    /// The walk is opportunistic: initializers that can't be evaluated at
    /// compile time are simply skipped (sema diagnoses them later). The
    /// result is keyed by the binding's own `Alloc` instruction, and the
    /// evaluation environment is block-scoped (RUE-530): an alias is visible
    /// to later initializers in its block and its nested blocks, then
    /// unwound — so sibling-branch aliases sharing a name don't collide, and
    /// a shadowed alias is restored when the shadow's block ends. The
    /// constraint generator replays the same scoping live via
    /// `ConstraintGenerator::enter_scope`/`exit_scope`.
    ///
    /// `type_subst` / `value_subst` carry the enclosing comptime parameter
    /// substitutions (specialized generic bodies), so aliases like
    /// `let P = Pair(T)` resolve. Discovered aliases also feed back into the
    /// evaluation environment, so chains (`let Q = Wrap(P)`) resolve too.
    pub(crate) fn precompute_comptime_type_locals(
        &mut self,
        body: InstRef,
        type_subst: Option<&AHashMap<Spur, Type>>,
        value_subst: Option<&AHashMap<Spur, ConstValue>>,
        runtime_params: &[Spur],
        attribution_enabled: bool,
    ) -> CompileResult<(AHashMap<InstRef, Type>, ComptimePrecomputeAttribution)> {
        let mut attribution = ComptimePrecomputeAttribution {
            enabled: attribution_enabled,
            ..ComptimePrecomputeAttribution::default()
        };
        let mut discovered: AHashMap<InstRef, Type> = AHashMap::new();
        let mut eval_types: AHashMap<Spur, Type> = type_subst.cloned().unwrap_or_default();
        let eval_values: AHashMap<Spur, ConstValue> = value_subst.cloned().unwrap_or_default();
        let mut runtime_bindings: AHashSet<Spur> = runtime_params.iter().copied().collect();
        let mut root_frame = Vec::new();
        self.walk_comptime_type_locals(
            body,
            &mut discovered,
            &mut eval_types,
            &eval_values,
            &mut runtime_bindings,
            &mut root_frame,
            &mut attribution,
        )?;
        Ok((discovered, attribution))
    }

    /// In-order walk over statement positions for
    /// [`precompute_comptime_type_locals`]. Only containers that can hold
    /// `let` statements are entered; everything else is left alone.
    ///
    /// `frame` is the innermost enclosing block's undo list: each alias
    /// discovered in that block records the name's previous binding there,
    /// and the block arm unwinds its frame (in reverse, RUE-522-style) when
    /// its statements are done, restoring `eval_types` to the enclosing
    /// scope's view.
    fn walk_comptime_type_locals(
        &mut self,
        inst_ref: InstRef,
        discovered: &mut AHashMap<InstRef, Type>,
        eval_types: &mut AHashMap<Spur, Type>,
        eval_values: &AHashMap<Spur, ConstValue>,
        runtime_bindings: &mut AHashSet<Spur>,
        frame: &mut Vec<(Spur, Option<Type>, bool)>,
        attribution: &mut ComptimePrecomputeAttribution,
    ) -> CompileResult<()> {
        self.check_canceled()?;
        if attribution.enabled {
            attribution.alias_nodes_visited += 1;
        }
        match &self.body_rir_ref().get(inst_ref).data {
            InstData::Block { instructions } => {
                let instructions = instructions.clone();
                let statement_count = self.body_rir_ref().block_inst_count(&instructions);
                let mut inner_frame = Vec::new();
                if attribution.enabled {
                    attribution.alias_block_statements += statement_count as u64;
                }
                for index in 0..statement_count {
                    let stmt = self
                        .body_rir_ref()
                        .block_inst(&instructions, index)
                        .expect("the statement count came from this block payload");
                    self.walk_comptime_type_locals(
                        stmt,
                        discovered,
                        eval_types,
                        eval_values,
                        runtime_bindings,
                        &mut inner_frame,
                        attribution,
                    )?;
                }
                for (name, old_type, was_runtime) in inner_frame.into_iter().rev() {
                    match old_type {
                        Some(ty) => eval_types.insert(name, ty),
                        None => eval_types.remove(&name),
                    };
                    if was_runtime {
                        runtime_bindings.insert(name);
                    } else {
                        runtime_bindings.remove(&name);
                    }
                }
            }
            InstData::Alloc { name, init, .. } => {
                if attribution.enabled {
                    attribution.alias_allocations_examined += 1;
                }
                let (name, init) = (*name, *init);
                if let Some(name) = name {
                    let alias = if initializer_may_evaluate_to_type_with_bindings(
                        self.body_rir_ref(),
                        init,
                        runtime_bindings,
                    ) {
                        let started = attribution.enabled.then(Instant::now);
                        if attribution.enabled {
                            attribution.alias_filter_accepts += 1;
                            attribution.alias_eval_attempts += 1;
                        }
                        let result = self.try_eval_type_alias_init(
                            init,
                            eval_types,
                            eval_values,
                            runtime_bindings,
                        );
                        if let Some(started) = started {
                            attribution.eval_provider_ns = attribution
                                .eval_provider_ns
                                .saturating_add(elapsed_ns(started));
                            attribution.alias_type_successes += u64::from(result.is_some());
                        }
                        result
                    } else {
                        if attribution.enabled {
                            attribution.alias_filter_skips += 1;
                        }
                        None
                    };
                    let old_type = eval_types.remove(&name);
                    let was_runtime = runtime_bindings.remove(&name);
                    frame.push((name, old_type, was_runtime));
                    if let Some(ty) = alias {
                        discovered.insert(inst_ref, ty);
                        eval_types.insert(name, ty);
                    } else {
                        runtime_bindings.insert(name);
                    }
                }
            }
            InstData::Branch {
                then_block,
                else_block,
                ..
            } => {
                let (then_block, else_block) = (*then_block, *else_block);
                self.walk_comptime_type_locals(
                    then_block,
                    discovered,
                    eval_types,
                    eval_values,
                    runtime_bindings,
                    frame,
                    attribution,
                )?;
                if let Some(else_block) = else_block {
                    self.walk_comptime_type_locals(
                        else_block,
                        discovered,
                        eval_types,
                        eval_values,
                        runtime_bindings,
                        frame,
                        attribution,
                    )?;
                }
            }
            InstData::Loop { body, .. } | InstData::InfiniteLoop { body, .. } => {
                let body = *body;
                self.walk_comptime_type_locals(
                    body,
                    discovered,
                    eval_types,
                    eval_values,
                    runtime_bindings,
                    frame,
                    attribution,
                )?;
            }
            InstData::Match { arms, .. } => {
                let bodies: Vec<InstRef> = self
                    .body_rir_ref()
                    .match_arms(arms)
                    .iter()
                    .map(|(_, body)| body)
                    .collect();
                for body in bodies {
                    self.walk_comptime_type_locals(
                        body,
                        discovered,
                        eval_types,
                        eval_values,
                        runtime_bindings,
                        frame,
                        attribution,
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Evaluate a `let` initializer as a compile-time type value, if it is
    /// one. Handles calls to type-returning functions (`F()`, `Pair(i32)`,
    /// `FixedBuffer(8)` — mirroring `analyze_call`'s implicit-comptime gate)
    /// and direct type expressions (`let P = Q;`, `let P = struct { .. };`).
    /// Returns `None` for anything else.
    fn try_eval_type_alias_init(
        &mut self,
        init: InstRef,
        eval_types: &AHashMap<Spur, Type>,
        eval_values: &AHashMap<Spur, ConstValue>,
        runtime_bindings: &AHashSet<Spur>,
    ) -> Option<Type> {
        // `eval_const_expr`'s `Call` arm reduces type-function calls
        // compositionally (including nested/delegating ones), so a single
        // evaluation of the initializer handles `let P = Q;`,
        // `let P = struct { .. };`, and `let P = Pair(i32);` alike (RUE-251).
        let mut env = ComptimeEnv::with_subst(eval_types, eval_values);
        env.canonical_identity = self.active_anonymous_producer().cloned();
        env.runtime_local_names = runtime_bindings.clone();
        env.defining_file = Some(self.body_rir_ref().get(init).span.file_id);
        match self.eval_const_expr(init, &mut env).ok().flatten() {
            Some(ConstValue::Type(t)) => Some(t),
            _ => None,
        }
    }

    /// Pre-reduce inline type-constructor heads (`F(args).Variant(..)`,
    /// `F(args) { .. }`; RUE-596, spec 4.14:23) to their
    /// concrete struct/enum types before HM inference runs (RUE-599).
    ///
    /// The constraint generator has no comptime interpreter, so an inline
    /// head left the construction's arguments unconstrained — an integer
    /// payload literal then defaulted to `i32` and could no longer satisfy a
    /// wider declared payload type (`Result(i64, i32).Ok(41)` → E0206), even
    /// though the bound-alias form (`let R = Result(i64, i32); R.Ok(41)`)
    /// typed it correctly via `comptime_local_types`. This pass reduces each
    /// candidate head opportunistically (like
    /// [`precompute_comptime_type_locals`], non-evaluable heads are simply
    /// skipped and sema diagnoses them later) and returns the reductions
    /// keyed by the head's own `InstRef` for the generator to look up.
    ///
    /// The scan follows only instructions reachable from this body and stops
    /// at nested declaration owners. A module RIR contains every body in that
    /// source file; scanning that whole arena once per body would multiply
    /// unrelated work by the number of declarations in the module.
    /// `comptime_local_types` carries the body's `let`-bound type aliases so a
    /// head like `Result(T, i32)` with `let T = i64;` reduces.
    ///
    /// [`precompute_comptime_type_locals`]: Self::precompute_comptime_type_locals
    pub(crate) fn precompute_inline_ctor_head_types(
        &mut self,
        body: InstRef,
        type_subst: Option<&AHashMap<Spur, Type>>,
        value_subst: Option<&AHashMap<Spur, ConstValue>>,
        comptime_local_types: &AHashMap<Spur, Type>,
        attribution_enabled: bool,
    ) -> CompileResult<(AHashMap<InstRef, Type>, ComptimePrecomputeAttribution)> {
        // The body RIR index walk already censused the whole arena for these
        // shapes. Zero occurrences anywhere proves the reachability scan below
        // — whose candidates are a subset of the arena's — would collect
        // nothing, so the common candidate-free body skips the scan outright.
        if self.body_inline_ctor_head_candidates() == 0 {
            return Ok((
                AHashMap::new(),
                ComptimePrecomputeAttribution {
                    enabled: attribution_enabled,
                    ..ComptimePrecomputeAttribution::default()
                },
            ));
        }
        // A head is the receiver of a `.NAME(..)` path whose receiver is
        // itself a call (`F(args).Ok(x)`, or module-qualified
        // `m.F(args).Ok(x)`, which RIR spells as a nested MethodCall), or a
        // struct literal's explicit `ctor_head`. Runtime shapes like
        // `foo(x).bar()` are collected too but fail the reduction cheaply
        // (the comptime engine rejects callees with runtime parameters).
        let (candidates, scan) = inline_ctor_head_candidates_with_work_checked(
            self.body_rir_ref(),
            body,
            attribution_enabled,
            || self.check_canceled(),
        )?;
        let mut attribution = ComptimePrecomputeAttribution {
            enabled: attribution_enabled,
            inline_scan_bodies: u64::from(attribution_enabled),
            inline_scan_pops: scan.pops,
            inline_scan_child_edges: scan.child_edges,
            inline_raw_candidates: scan.raw_candidates,
            inline_final_candidates: if attribution_enabled {
                candidates.len() as u64
            } else {
                0
            },
            ..ComptimePrecomputeAttribution::default()
        };
        let mut eval_types: AHashMap<Spur, Type> = type_subst.cloned().unwrap_or_default();
        eval_types.extend(comptime_local_types);
        let eval_values: AHashMap<Spur, ConstValue> = value_subst.cloned().unwrap_or_default();
        let mut reduced = AHashMap::new();
        for head in candidates {
            self.check_canceled()?;
            let started = attribution.enabled.then(Instant::now);
            if attribution.enabled {
                attribution.inline_eval_attempts += 1;
            }
            let result = self.try_evaluate_const_with_subst(head, &eval_types, &eval_values);
            if let Some(started) = started {
                attribution.eval_provider_ns = attribution
                    .eval_provider_ns
                    .saturating_add(elapsed_ns(started));
            }
            if let Some(ConstValue::Type(ty)) = result
                && (ty.is_enum() || ty.as_struct().is_some())
            {
                if attribution.enabled {
                    attribution.inline_type_successes += 1;
                }
                reduced.insert(head, ty);
            }
        }
        Ok((reduced, attribution))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ComptimePrecomputeAttribution {
    enabled: bool,
    pub(crate) eval_provider_ns: u64,
    pub(crate) alias_nodes_visited: u64,
    pub(crate) alias_block_statements: u64,
    pub(crate) alias_allocations_examined: u64,
    pub(crate) alias_filter_accepts: u64,
    pub(crate) alias_filter_skips: u64,
    pub(crate) alias_eval_attempts: u64,
    pub(crate) alias_type_successes: u64,
    pub(crate) inline_scan_pops: u64,
    pub(crate) inline_scan_child_edges: u64,
    pub(crate) inline_scan_bodies: u64,
    pub(crate) inline_raw_candidates: u64,
    pub(crate) inline_final_candidates: u64,
    pub(crate) inline_eval_attempts: u64,
    pub(crate) inline_type_successes: u64,
}

impl ComptimePrecomputeAttribution {
    pub(crate) fn accrue(&mut self, other: Self) {
        self.enabled |= other.enabled;
        self.eval_provider_ns = self.eval_provider_ns.saturating_add(other.eval_provider_ns);
        self.alias_nodes_visited += other.alias_nodes_visited;
        self.alias_block_statements += other.alias_block_statements;
        self.alias_allocations_examined += other.alias_allocations_examined;
        self.alias_filter_accepts += other.alias_filter_accepts;
        self.alias_filter_skips += other.alias_filter_skips;
        self.alias_eval_attempts += other.alias_eval_attempts;
        self.alias_type_successes += other.alias_type_successes;
        self.inline_scan_pops += other.inline_scan_pops;
        self.inline_scan_child_edges += other.inline_scan_child_edges;
        self.inline_scan_bodies += other.inline_scan_bodies;
        self.inline_raw_candidates += other.inline_raw_candidates;
        self.inline_final_candidates += other.inline_final_candidates;
        self.inline_eval_attempts += other.inline_eval_attempts;
        self.inline_type_successes += other.inline_type_successes;
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct InlineCtorScanWork {
    pops: u64,
    child_edges: u64,
    raw_candidates: u64,
}

/// This one-sided shape filter excludes names already known to be runtime
/// bindings in the current lexical scope. Such a name
/// cannot evaluate to a type, while an unresolved/file-level name must remain
/// a candidate because the evaluator may resolve it through semantic facts.
pub(super) fn initializer_may_evaluate_to_type_with_bindings(
    rir: &rue_rir::Rir,
    inst_ref: InstRef,
    runtime_bindings: &AHashSet<Spur>,
) -> bool {
    match &rir.get(inst_ref).data {
        InstData::AnonStructType { .. }
        | InstData::AnonEnumType { .. }
        | InstData::TypeConst { .. }
        | InstData::Call { .. } => true,
        // A dotted head reduces to a type only through a module or type
        // binding, so its receiver spine must bottom out at a `VarRef` that a
        // runtime binding does not shadow (spec 4.14:6). A call result, an
        // index, or a runtime local's member is a genuine runtime value; the
        // evaluator would return non-evaluable for it, so skip the evaluation.
        InstData::MethodCall { receiver, .. } => {
            spine_root_names_unshadowed_binding(rir, *receiver, runtime_bindings)
        }
        InstData::FieldGet { .. } => {
            spine_root_names_unshadowed_binding(rir, inst_ref, runtime_bindings)
        }
        InstData::VarRef { name, .. } => !runtime_bindings.contains(name),
        InstData::Comptime { expr } => {
            initializer_may_evaluate_to_type_with_bindings(rir, *expr, runtime_bindings)
        }
        InstData::ArrayRepeat { value, .. } => {
            initializer_may_evaluate_to_type_with_bindings(rir, *value, runtime_bindings)
        }
        InstData::Block { instructions } => {
            let count = rir.block_inst_count(instructions);
            count != 0
                && initializer_may_evaluate_to_type_with_bindings(
                    rir,
                    rir.block_inst(instructions, count - 1)
                        .expect("the tail index came from this block payload"),
                    runtime_bindings,
                )
        }
        InstData::Branch {
            then_block,
            else_block,
            ..
        } => {
            initializer_may_evaluate_to_type_with_bindings(rir, *then_block, runtime_bindings)
                || else_block.is_some_and(|else_block| {
                    initializer_may_evaluate_to_type_with_bindings(
                        rir,
                        else_block,
                        runtime_bindings,
                    )
                })
        }
        InstData::Match { arms, .. } => rir.match_arms(arms).iter().any(|(_, body)| {
            initializer_may_evaluate_to_type_with_bindings(rir, body, runtime_bindings)
        }),
        _ => false,
    }
}

/// Whether a dotted receiver spine could still name a module, type, or
/// constant binding: it must be a chain of member accesses bottoming out at a
/// `VarRef` whose name no runtime binding shadows (spec 4.14:6). This mirrors
/// the reachability requirement of the evaluator's own qualified-call walk,
/// which rejects every other receiver shape as a runtime call.
fn spine_root_names_unshadowed_binding(
    rir: &rue_rir::Rir,
    mut cursor: InstRef,
    runtime_bindings: &AHashSet<Spur>,
) -> bool {
    loop {
        match &rir.get(cursor).data {
            InstData::VarRef { name, .. } => return !runtime_bindings.contains(name),
            InstData::FieldGet { base, .. } => cursor = *base,
            _ => return false,
        }
    }
}

#[cfg(test)]
pub(super) fn inline_ctor_head_candidates(rir: &rue_rir::Rir, body: InstRef) -> Vec<InstRef> {
    inline_ctor_head_candidates_with_work(rir, body, false).0
}

#[cfg(test)]
fn inline_ctor_head_candidates_with_work(
    rir: &rue_rir::Rir,
    body: InstRef,
    attribution_enabled: bool,
) -> (Vec<InstRef>, InlineCtorScanWork) {
    inline_ctor_head_candidates_with_work_checked(rir, body, attribution_enabled, || Ok(()))
        .expect("the no-op inline constructor scan cannot be canceled")
}

fn inline_ctor_head_candidates_with_work_checked<F>(
    rir: &rue_rir::Rir,
    body: InstRef,
    attribution_enabled: bool,
    mut check_canceled: F,
) -> CompileResult<(Vec<InstRef>, InlineCtorScanWork)>
where
    F: FnMut() -> CompileResult<()>,
{
    let mut pending = vec![body];
    let mut candidates = Vec::new();
    let mut work = InlineCtorScanWork::default();

    while let Some(current) = pending.pop() {
        check_canceled()?;
        if attribution_enabled {
            work.pops += 1;
        }
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

        match rir.get(current).data {
            InstData::MethodCall { receiver, .. } => {
                if matches!(
                    rir.get(receiver).data,
                    InstData::Call { .. } | InstData::MethodCall { .. }
                ) {
                    candidates.push(receiver);
                    if attribution_enabled {
                        work.raw_candidates += 1;
                    }
                }
            }
            InstData::StructInit {
                ctor_head: Some(head),
                ..
            } => {
                candidates.push(head);
                if attribution_enabled {
                    work.raw_candidates += 1;
                }
            }
            // An inline type-constructor pattern head (`Opt(u8).Some(b)`,
            // RUE-596) carries the head instruction on the pattern itself.
            // Collecting it lets inference pre-type the arm's payload
            // bindings and the pattern's scrutinee contract, so an arm
            // literal in a sibling arm sees the enclosing expectation
            // instead of defaulting to i32 (RUE-954).
            InstData::Match { ref arms, .. } => {
                for (pattern, _) in rir.match_arms(arms).iter() {
                    if let rue_rir::RirPatternView::Path {
                        ctor_head: Some(head),
                        ..
                    } = pattern
                    {
                        candidates.push(head);
                        if attribution_enabled {
                            work.raw_candidates += 1;
                        }
                    }
                }
            }
            _ => {}
        }

        let before = attribution_enabled.then_some(pending.len());
        rir.child_instructions(current, &mut pending);
        if let Some(before) = before {
            work.child_edges += (pending.len() - before) as u64;
        }
    }

    candidates.sort_unstable_by_key(|candidate| candidate.as_u32());
    candidates.dedup();
    Ok((candidates, work))
}

/// Local semantic adapter for the separated compile-time engine. The adapter only
/// exposes facts and named semantic hooks; recursive instruction traversal
/// remains in `comptime::ComptimeEngine`.
impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeDomain for OrdinaryBodyEngine<'h, H> {
    type Type = Type;
    type Value = ConstValue;
    type Name = Spur;
    type File = FileId;
    type CanonicalIdentity = super::anon_structs::IssuedStableProducerId;
    type AnonymousIdentity = super::anon_structs::IssuedAnonymousNominalKey;
    type ProgramKey = ();
    type Failure = CompileError;
    type CallAdmission = super::info::FunctionCallInfo;
    type CallBinding = OrdinaryComptimeCallBinding;
    type BoundCall = OrdinaryComptimeBoundCall;
    type CompletionTicket = ();
    type StructuredTypeSuspension = crate::semantic_type_resolution::ComptimeStructuredTypeJob<
        (),
        (),
        (),
        Spur,
        (),
        Type,
        ConstValue,
        Spur,
        std::sync::Arc<[std::sync::Arc<str>]>,
    >;
}

impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeInterrupts for OrdinaryBodyEngine<'h, H> {
    fn check_canceled(&self) -> ComptimeHostResult<(), Self::Failure> {
        OrdinaryBodyEngine::check_canceled(self).map_err(ComptimeHostError::HostFailure)
    }
}

impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeProgramFacts for OrdinaryBodyEngine<'h, H> {
    fn program_rir(&self, _program: &Self::ProgramKey) -> &rue_rir::Rir {
        OrdinaryBodyEngine::body_rir_ref(self)
    }
    fn name_from_symbol(
        &self,
        _program: &Self::ProgramKey,
        symbol: rue_rir::SymbolHandle,
    ) -> Self::Name {
        symbol.spur()
    }
    fn display_name(&self, name: &Self::Name) -> String {
        self.body_interner().resolve(name).to_owned()
    }
    fn file_for_program_span(&self, _program: &Self::ProgramKey, span: &Span) -> Self::File {
        span.file_id
    }
}

impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeTypeAlgebra for OrdinaryBodyEngine<'h, H> {
    fn unsupported_anon_method_type_param(
        &self,
        method_name: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "method '{}' declares its own `comptime` type parameter, \
                     which is not yet supported (a method cannot be \
                     monomorphized over its own type parameter); \
                     move the type parameter to the enclosing type \
                     constructor instead",
                    method_name
                ),
            },
            site.span(),
        )
    }
    fn non_function_anon_method(
        &self,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: "anonymous type carries a non-function method instruction".to_owned(),
            },
            site.span(),
        )
    }
    fn resolve_named_array_length(
        &mut self,
        name: &Spur,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        values: Option<&AHashMap<Spur, ConstValue>>,
        _binding: ComptimeArrayLengthBinding<ConstValue>,
    ) -> ComptimeOutcome<u64, Self::Failure> {
        let name = self.body_interner().resolve(name).to_owned();
        OrdinaryBodyEngine::resolve_array_length(self, &ArrayLen::Named(name), site.span(), values)
            .map_or_else(
                |error| ComptimeOutcome::HostFailure(error),
                ComptimeOutcome::Known,
            )
    }
    fn rir_type_named_symbol(
        &self,
        _program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Option<Spur> {
        OrdinaryBodyEngine::rir_type_named_symbol(self, syntax)
    }
    fn render_rir_type(
        &self,
        _program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> String {
        OrdinaryBodyEngine::render_rir_type(self, syntax)
    }
    fn get_or_create_array_type(&mut self, element: Type, length: u64) -> Type {
        Type::new_array(OrdinaryBodyEngine::get_or_create_array_type(
            self, element, length,
        ))
    }
    fn find_or_create_anon_struct(
        &mut self,
        identity: Self::AnonymousIdentity,
        fields: &[super::comptime::ComptimeField<Spur, Type>],
        sigs: &[ComptimeMethodDescriptor<Spur, Type>],
        _type_subst: &AHashMap<Spur, Type>,
        value_subst: &AHashMap<Spur, ConstValue>,
    ) -> ComptimeHostResult<(Type, bool), Self::Failure> {
        let fields: Vec<StructField> = fields
            .iter()
            .map(|field| StructField {
                name: self.body_interner().resolve(&field.name).to_owned(),
                ty: field.ty,
            })
            .collect();
        let sigs: Vec<super::AnonMethodSig> = sigs
            .iter()
            .map(|sig| super::AnonMethodSig {
                name: sig.name,
                has_self: sig.has_self,
                self_mode: sig.self_mode,
                returns_borrow: sig.returns_borrow,
                returns_inout: sig.returns_inout,
                param_types: sig
                    .parameters
                    .iter()
                    .map(|parameter| match &parameter.ty {
                        super::comptime::ComptimeMethodType::SelfType => {
                            super::AnonMethodType::SelfType
                        }
                        super::comptime::ComptimeMethodType::Concrete(ty) => {
                            super::AnonMethodType::Concrete(*ty)
                        }
                        super::comptime::ComptimeMethodType::Unsupported(shape) => {
                            super::AnonMethodType::Syntax(shape.clone().into())
                        }
                    })
                    .collect(),
                param_modes: sig
                    .parameters
                    .iter()
                    .map(|parameter| parameter.mode)
                    .collect(),
                param_comptime: sig
                    .parameters
                    .iter()
                    .map(|parameter| parameter.is_comptime)
                    .collect(),
                return_type: match &sig.result {
                    super::comptime::ComptimeMethodType::SelfType => {
                        super::AnonMethodType::SelfType
                    }
                    super::comptime::ComptimeMethodType::Concrete(ty) => {
                        super::AnonMethodType::Concrete(*ty)
                    }
                    super::comptime::ComptimeMethodType::Unsupported(shape) => {
                        super::AnonMethodType::Syntax(shape.clone().into())
                    }
                },
            })
            .collect();
        OrdinaryBodyEngine::find_or_create_anon_struct(self, identity, &fields, &sigs, value_subst)
            .map_err(Into::into)
    }
    fn find_or_create_anon_enum(
        &mut self,
        identity: Self::AnonymousIdentity,
        names: &[String],
        payloads: &[Vec<Type>],
        _type_subst: &AHashMap<Spur, Type>,
        _value_subst: &AHashMap<Spur, ConstValue>,
    ) -> ComptimeHostResult<Type, Self::Failure> {
        OrdinaryBodyEngine::find_or_create_anon_enum(self, identity, names, payloads)
            .map_err(Into::into)
    }
    fn check_require_droppable(
        &mut self,
        ty: Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure> {
        OrdinaryBodyEngine::check_require_droppable(self, ty, site.span()).map_err(Into::into)
    }
    fn check_trivially_droppable(
        &mut self,
        ty: Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure> {
        OrdinaryBodyEngine::check_trivially_droppable(self, ty, site.span()).map_err(Into::into)
    }
    fn type_name(&self, ty: &Type) -> String {
        self.format_type_name(*ty)
    }
    fn type_is_unsigned(&self, ty: &Type) -> bool {
        ty.is_unsigned()
    }
    fn type_integer_semantics(&self, ty: &Type) -> Option<crate::integer_semantics::IntegerType> {
        ty.integer_semantics()
    }
    fn type_float_width(&self, ty: &Type) -> Option<super::comptime::ComptimeFloatWidth> {
        match *ty {
            Type::F32 => Some(super::comptime::ComptimeFloatWidth::F32),
            Type::F64 => Some(super::comptime::ComptimeFloatWidth::F64),
            _ => None,
        }
    }
    fn float_type(&self, width: super::comptime::ComptimeFloatWidth) -> Option<Type> {
        Some(width.air_type())
    }
    fn const_expr_type(
        &self,
        _program: &Self::ProgramKey,
        env: &ComptimeEnv<'_>,
        inst_ref: InstRef,
    ) -> Option<Type> {
        OrdinaryBodyEngine::const_expr_type(self, env, inst_ref)
    }
    fn resolve_named_type_value(
        &mut self,
        _program: &Self::ProgramKey,
        name: Spur,
        span: Span,
    ) -> ComptimeHostResult<Option<Type>, Self::Failure> {
        OrdinaryBodyEngine::resolve_named_type_value(self, name, span).map_err(Into::into)
    }
    fn resolve_comptime_type_path(
        &mut self,
        file: FileId,
        segments: &[Spur],
        span: Span,
    ) -> ComptimeHostResult<Option<ConstValue>, Self::Failure> {
        OrdinaryBodyEngine::resolve_comptime_type_path(self, file, segments, span)
            .map_err(Into::into)
    }
    fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        _program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<Type> {
        OrdinaryBodyEngine::resolve_rir_type_for_comptime_with_subst_and_values_at_span(
            self, syntax, types, values, span,
        )
    }
}

impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeValueAlgebra for OrdinaryBodyEngine<'h, H> {
    fn resolve_comptime_named_value(
        &mut self,
        file: Self::File,
        name: Self::Name,
        span: Span,
    ) -> ComptimeHostResult<ComptimeNamedValueResolution<Self::Value>, Self::Failure> {
        let Some(info) = OrdinaryBodyEngine::value_const(self, &(file, name)) else {
            let resolved = OrdinaryBodyEngine::resolve_named_type_value(self, name, span)
                .map_err(ComptimeHostError::HostFailure)?;
            if let Some(ty) = resolved {
                let dependency = match ty.kind() {
                    TypeKind::Struct(id) => {
                        let def = self
                            .body_type_pool()
                            .struct_metadata(id)
                            .expect("struct type must have declaration metadata");
                        Some(super::NamedConstDependencyTargetEvent::NamedType {
                            file: def.file_id.index(),
                            name: def.name.to_string(),
                            kind: super::DeclarationTypeDependencyTargetKind::Struct,
                        })
                    }
                    TypeKind::Enum(id) => {
                        let def = self
                            .body_type_pool()
                            .enum_metadata(id)
                            .expect("enum type must have declaration metadata");
                        Some(super::NamedConstDependencyTargetEvent::NamedType {
                            file: def.file_id.index(),
                            name: def.name.to_string(),
                            kind: super::DeclarationTypeDependencyTargetKind::Enum,
                        })
                    }
                    _ => None,
                };
                if let Some(dependency) = dependency {
                    self.record_body_named_dependency(dependency);
                }
                return Ok(ComptimeNamedValueResolution::Known(ConstValue::Type(ty)));
            }
            return Ok(ComptimeNamedValueResolution::Missing);
        };
        let defining_file = info.span.file_id;
        let name_text = self.body_interner().resolve(&name).to_owned();
        self.record_body_named_dependency(super::NamedConstDependencyTargetEvent::ValueConst {
            file: defining_file.index(),
            name: name_text.clone(),
        });
        OrdinaryBodyEngine::check_unqualified_visibility(
            self,
            "constant",
            &name_text,
            defining_file,
            info.is_pub,
            span,
        )?;
        let value = match info.value {
            ConstValue::Integer(value) => Some(ConstValue::Integer(value)),
            ConstValue::Bool(value) => Some(ConstValue::Bool(value)),
            ConstValue::Unit => Some(ConstValue::Unit),
            ConstValue::Type(value) => Some(ConstValue::Type(value)),
            ConstValue::Float(value) => Some(ConstValue::Float(value)),
            _ => None,
        };
        Ok(match value {
            Some(value) => ComptimeNamedValueResolution::Known(value),
            None => ComptimeNamedValueResolution::RuntimeDependent,
        })
    }
    fn match_pattern(
        &self,
        pattern: &ComptimeMatchPattern<Spur>,
        value: &ConstValue,
    ) -> Option<bool> {
        const_pattern_matches(pattern, value.clone())
    }
    fn match_no_selected_arm(
        &self,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::RuntimeDependent
    }
    fn evaluate_binary_rhs_after_rejection(&self) -> bool {
        false
    }
    fn finish_arith(
        &self,
        result: CheckedIntegerResult,
        ty: Option<Type>,
        op: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<ConstValue>, Self::Failure> {
        // The engine uses a distinct semantic token for unary negation so
        // durable hosts can preserve its operation-specific wording. The
        // ordinary body diagnostic remains the historical `-` spelling.
        OrdinaryBodyEngine::finish_arith(
            self,
            result,
            ty,
            if op == "negation" { "-" } else { op },
            site.span(),
        )
        .map_err(Into::into)
    }

    fn resolve_float_const(
        &mut self,
        content: Self::Name,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        let text = self.body_interner().resolve(&content);
        let Some(canonical) = crate::canonical_decimal_literal(text) else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let symbol = self.body_interner().get_or_intern(canonical);
        ComptimeOutcome::Known(ConstValue::Float(rue_rir::SymbolHandle::new(symbol)))
    }
    fn float_value_text(&self, value: &ConstValue) -> Option<String> {
        match value {
            ConstValue::Float(content) => {
                Some(self.body_interner().resolve(&content.spur()).to_owned())
            }
            _ => None,
        }
    }
    fn float_value_from_text(
        &mut self,
        text: &str,
        _ty: Option<Type>,
    ) -> ComptimeHostResult<Option<ConstValue>, Self::Failure> {
        // The ordinary domain keeps floats as untyped text: the use site
        // materializes the value at its inferred type, which is the width the
        // text was rendered at.
        let symbol = self.body_interner().get_or_intern(text);
        Ok(Some(ConstValue::Float(rue_rir::SymbolHandle::new(symbol))))
    }
}

impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeCallProtocol for OrdinaryBodyEngine<'h, H> {
    fn resolve_module_comptime_callable(
        &mut self,
        file: FileId,
        segments: &[Spur],
        method: Spur,
        span: Span,
    ) -> ComptimeHostResult<Option<Spur>, Self::Failure> {
        OrdinaryBodyEngine::resolve_module_comptime_callable(self, file, segments, method, span)
            .map_err(Into::into)
    }
    fn admit_comptime_call(
        &mut self,
        name: Spur,
        count: usize,
        modes: &[ComptimeArgMode],
        env: &mut ComptimeEnv<'_>,
        resolved: bool,
    ) -> ComptimeHostResult<Option<ComptimeCallAdmission<FunctionCallInfo, Spur>>, Self::Failure>
    {
        OrdinaryBodyEngine::admit_comptime_call(self, name, count, modes, env, resolved)
            .map_err(Into::into)
    }
    fn begin_comptime_call_binding(
        &self,
        admission: &ComptimeCallAdmission<FunctionCallInfo, Spur>,
        argument_count: usize,
        span: Span,
    ) -> ComptimeHostResult<Self::CallBinding, Self::Failure> {
        OrdinaryBodyEngine::begin_comptime_call_binding(self, admission, argument_count, span)
            .map_err(Into::into)
    }
    fn bind_comptime_call_argument(
        &self,
        binding: &mut Self::CallBinding,
        argument: ComptimeCallArgument<ConstValue>,
        index: usize,
        span: Span,
    ) -> ComptimeHostResult<bool, Self::Failure> {
        OrdinaryBodyEngine::bind_comptime_call_argument(self, binding, argument, index, span)
            .map_err(Into::into)
    }
    fn finish_comptime_call_binding(
        &mut self,
        binding: Self::CallBinding,
        span: Span,
    ) -> ComptimeHostResult<Option<Self::BoundCall>, Self::Failure> {
        OrdinaryBodyEngine::finish_comptime_call_binding(self, binding, span).map_err(Into::into)
    }
    fn prepare_comptime_call(
        &mut self,
        admission: ComptimeCallAdmission<FunctionCallInfo, Spur>,
        bound: Self::BoundCall,
        span: Span,
    ) -> ComptimeHostResult<
        Option<
            ComptimeCallPreparation<
                ConstValue,
                Type,
                Spur,
                FileId,
                (),
                Self::CanonicalIdentity,
                CompileError,
                (),
            >,
        >,
        Self::Failure,
    > {
        if let Some(result) = OrdinaryBodyEngine::reduce_external_comptime_call(
            self,
            admission.name,
            &bound.callee_types,
            &bound.callee_values,
            span,
        ) {
            return result
                .map(|result| {
                    Some(ComptimeCallPreparation::Memoized(match result {
                        Some(value) => ComptimeOutcome::Known(value),
                        None => ComptimeOutcome::RuntimeDependent,
                    }))
                })
                .map_err(Into::into);
        }
        let name = admission.name;
        let preparation = OrdinaryBodyEngine::prepare_comptime_call(
            self,
            admission,
            bound.callee_types,
            bound.callee_values,
            span,
        )
        .map_err(ComptimeHostError::HostFailure)?;
        let Some(ComptimeCallPreparation::Enter { mut frame, ticket }) = preparation else {
            return Ok(preparation);
        };
        // Canonicalization is an optimization input to the local lookup, not
        // an admission gate. If it cannot issue an identity yet, leave the
        // frame identity-less so `run_frame` preserves its established
        // depth-first ordering: an over-limit call reports depth before the
        // original canonicalization failure. A successful issuance is carried
        // into the frame so a normal miss does not mint the same identity a
        // second time after the depth check.
        let identity = match self.canonical_function_producer(
            name,
            &frame.type_bindings,
            &frame.value_bindings,
        ) {
            Ok(identity) => identity,
            Err(_) => return Ok(Some(ComptimeCallPreparation::Enter { frame, ticket })),
        };
        // Admission already issued the exact identity used by the local
        // lookup. Carry it into the frame so `run_frame` does not mint the
        // same producer a second time after the depth check. The lookup itself
        // happens in the engine only after that depth check.
        frame.call_identity = Some(identity);
        Ok(Some(ComptimeCallPreparation::Enter { frame, ticket }))
    }
    fn lookup_completed_comptime_call(
        &mut self,
        frame: &ComptimeFrame<ConstValue, Type, Spur, FileId, (), Self::CanonicalIdentity>,
    ) -> ComptimeHostResult<Option<ConstValue>, Self::Failure> {
        let Some(name) = frame.name else {
            return Ok(None);
        };
        let Some(identity) = frame.call_identity.clone() else {
            return Ok(None);
        };
        let Some(key) = self.comptime_reduction_key(
            name,
            identity,
            &frame.type_bindings,
            &frame.value_bindings,
        ) else {
            return Ok(None);
        };
        let Some(value) = self.lookup_comptime_reduction(&key) else {
            #[cfg(test)]
            self.record_comptime_reduction_miss();
            return Ok(None);
        };
        self.check_canceled()
            .map_err(ComptimeHostError::HostFailure)?;
        Ok(Some(value))
    }
    fn finish_comptime_call(
        &mut self,
        frame: &ComptimeFrame<ConstValue, Type, Spur, FileId, (), Self::CanonicalIdentity>,
        _ticket: (),
        result: ComptimeOutcome<ConstValue, CompileError>,
    ) -> ComptimeOutcome<ConstValue, CompileError> {
        OrdinaryBodyEngine::finish_comptime_call(self, frame, (), result)
    }
    fn enter_comptime_call(
        &mut self,
        _frame: &ComptimeFrame<ConstValue, Type, Spur, FileId, (), Self::CanonicalIdentity>,
        _ticket: &(),
    ) -> ComptimeHostResult<(), CompileError> {
        Ok(())
    }
    fn canonical_function_producer(
        &self,
        _program: &Self::ProgramKey,
        _ticket: &Self::CompletionTicket,
        name: Spur,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> ComptimeHostResult<Self::CanonicalIdentity, Self::Failure> {
        OrdinaryBodyEngine::canonical_function_producer(self, name, types, values)
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue canonical comptime producer: {failure:?}"
                    )),
                    span,
                )
            })
            .map_err(Into::into)
    }
    fn issue_anonymous_identity(
        &self,
        _program: &Self::ProgramKey,
        kind: ComptimeAnonymousKind,
        producer: &Self::CanonicalIdentity,
        anchor: &rue_rir::RirStructuralAnchor,
    ) -> Self::AnonymousIdentity {
        crate::AnonymousNominalKey {
            kind: match kind {
                ComptimeAnonymousKind::Struct => crate::AnonymousNominalKind::Struct,
                ComptimeAnonymousKind::Enum => crate::AnonymousNominalKind::Enum,
            },
            producer: producer.clone(),
            anchor: anchor.clone(),
        }
    }
}

impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeStructuredTypes for OrdinaryBodyEngine<'h, H> {
    fn prepare_structured_type_call(
        &mut self,
        _suspension: &Self::StructuredTypeSuspension,
        _span: Span,
    ) -> ComptimeOutcome<
        Option<
            ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
                (),
            >,
        >,
        Self::Failure,
    > {
        unreachable!("ordinary comptime type resolution is synchronous")
    }
    fn resume_structured_type_call(
        &mut self,
        _suspension: Self::StructuredTypeSuspension,
        _result: ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> ComptimeOutcome<
        ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
        Self::Failure,
    > {
        unreachable!("ordinary comptime type resolution is synchronous")
    }
}

impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeRejections for OrdinaryBodyEngine<'h, H> {
    fn reject_comptime_expression(
        &self,
        rejection: ComptimeSemanticRejection<Self::Value>,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        match rejection {
            // Empty ordinary blocks historically reduce to unit; preserve
            // that body-domain behavior while durable hosts may reject them.
            ComptimeSemanticRejection::EmptyBlock => ComptimeOutcome::Known(ConstValue::Unit),
            _ => ComptimeOutcome::RuntimeDependent,
        }
    }
    fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure> {
        OrdinaryBodyEngine::require_preview(self, feature, what, site.span()).map_err(Into::into)
    }
    fn depth_exceeded(
        &self,
        name: &Spur,
        depth: usize,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "specialization of '{}' exceeded the maximum nesting depth ({}); \
                     is a comptime-recursive function missing a compile-time-known \
                     base case, or a generic function recursively instantiating \
                     itself with new types?",
                    self.body_interner().resolve(name),
                    depth
                ),
            },
            site.span(),
        )
    }
    fn literal_out_of_range(
        &self,
        value: u64,
        ty: &Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        CompileError::new(
            ErrorKind::LiteralOutOfRange {
                value,
                ty: self.type_name(ty),
            },
            site.span(),
        )
    }
    fn cannot_negate(
        &self,
        ty: &Type,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        CompileError::new(ErrorKind::CannotNegate(self.type_name(ty)), site.span())
    }
    fn label_ctor_instantiation_site(error: CompileError, span: Span) -> CompileError {
        OrdinaryBodyEngine::<H>::label_ctor_instantiation_site(error, span)
    }
}

impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeHost for OrdinaryBodyEngine<'h, H> {}

#[cfg(test)]
mod binding_tests {
    use super::*;

    #[test]
    fn ordinary_pattern_policy_keeps_unknown_paths_undecidable() {
        let interner = lasso::ThreadedRodeo::<lasso::Spur>::new();
        let type_name = interner.get_or_intern("Os");
        let variant = interner.get_or_intern("Macos");
        let target = ComptimeMatchPattern::Path {
            module_qualified: false,
            ctor_qualified: false,
            type_name,
            variant,
            binding_count: 0,
        };
        assert_eq!(
            const_pattern_matches(
                &ComptimeMatchPattern::<lasso::Spur>::Wildcard,
                ConstValue::Unit
            ),
            Some(true)
        );
        assert_eq!(
            const_pattern_matches(
                &ComptimeMatchPattern::<lasso::Spur>::Integer(-3),
                ConstValue::Integer(-3)
            ),
            Some(true)
        );
        assert_eq!(
            const_pattern_matches(
                &ComptimeMatchPattern::<lasso::Spur>::Bool(true),
                ConstValue::Integer(1)
            ),
            None
        );
        assert_eq!(const_pattern_matches(&target, ConstValue::Unit), None);
    }

    #[test]
    fn ordinary_binding_stores_invalid_shape_then_rejects_only_at_finish() {
        let interner = lasso::ThreadedRodeo::new();
        let value_name = interner.get_or_intern("value");
        let later_name = interner.get_or_intern("later");
        let mut binding = OrdinaryComptimeCallBinding {
            parameter_names: vec![value_name, later_name],
            parameter_is_type: vec![true, false],
            arguments: Vec::new(),
        };

        // Push is the ordinary host's transactional operation: it records
        // the shape without publishing an early mismatch. The whole batch
        // policy is applied exactly once by finish.
        assert!(push_ordinary_comptime_call_argument(
            &mut binding,
            ConstValue::Integer(7),
        ));
        assert!(push_ordinary_comptime_call_argument(
            &mut binding,
            ConstValue::Integer(7),
        ));
        assert_eq!(binding.arguments.len(), 2);
        assert!(finish_ordinary_comptime_call_binding(binding).is_none());
    }
}
