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
use std::sync::LazyLock;
use std::time::Instant;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_rir::{InstData, InstRef, RepeatCount};
use rue_span::{FileId, Span};

use super::context::{AnalysisContext, ConstValue, LocalVar, ParamIndex, ParamInfo};
use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};

pub(super) fn validate_comptime_value_for_type_impl(
    interner: &lasso::ThreadedRodeo,
    type_pool: &crate::intern_pool::TypeInternPool,
    function_name: Spur,
    param_name: Spur,
    value: ConstValue,
    expected: Type,
    span: Span,
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
                    expected.safe_name_with_pool(Some(type_pool))
                ),
            },
            span,
        ));
    }
    let found = value.get_type();
    if found != expected {
        return Err(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: expected.safe_name_with_pool(Some(type_pool)),
                found: found.safe_name_with_pool(Some(type_pool)),
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
use crate::specialize::MAX_SPECIALIZATION_ROUNDS;
use crate::types::{ArrayLen, StructField, Type, TypeKind};

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Empty type substitution map for evaluation contexts without one.
static EMPTY_TYPE_SUBST: LazyLock<AHashMap<Spur, Type>> = LazyLock::new(AHashMap::new);
/// Empty value substitution map for evaluation contexts without one.
static EMPTY_VALUE_SUBST: LazyLock<AHashMap<Spur, ConstValue>> = LazyLock::new(AHashMap::new);
/// Empty module-member map for evaluation contexts without one.
static EMPTY_MODULE_MEMBERS: LazyLock<AHashMap<InstRef, ConstValue>> = LazyLock::new(AHashMap::new);

/// The environment a compile-time expression is evaluated in.
pub(crate) struct ComptimeEnv<'a> {
    /// Definition-relative producer root for anonymous identity issuance.
    producer: Option<InstRef>,
    canonical_identity: Option<(
        super::anon_structs::IssuedStableProducerId,
        super::anon_structs::IssuedCanonicalArguments,
    )>,
    /// Comptime type parameters in scope (e.g. `T` -> `i32`).
    type_subst: &'a AHashMap<Spur, Type>,
    /// Comptime value parameters in scope (e.g. `N` -> `42`).
    value_subst: &'a AHashMap<Spur, ConstValue>,
    /// Resolved types from HM inference for the function being analyzed.
    /// `None` when evaluating expressions outside a typed function context
    /// (comptime function bodies before specialization, const initializers).
    resolved_types: Option<&'a AHashMap<InstRef, Type>>,
    /// Runtime locals in scope at the point being evaluated. A runtime local
    /// shadows same-named comptime parameters and file-level constants, so a
    /// reference to it makes the expression non-evaluable — without this,
    /// `let n = x; g(n)` inside a body with `comptime n` in scope would
    /// wrongly evaluate `n` to the parameter's value (spec 4.14:6).
    runtime_locals: Option<&'a AHashMap<Spur, LocalVar>>,
    /// Runtime parameters in scope. They shadow same-named type values and
    /// constants just like locals; comptime parameters resolve through the
    /// substitution maps before this guard is consulted.
    runtime_params: Option<(&'a [ParamInfo], &'a ParamIndex)>,
    /// Runtime bindings known only by name during the pre-inference local
    /// type-alias walk. This lightweight lexical view prevents ordinary
    /// parameters and earlier `let` bindings from falling through to global
    /// constant/type lookup before `AnalysisContext` exists.
    runtime_binding_names: Option<&'a AHashSet<Spur>>,
    /// `let` bindings introduced by blocks inside the comptime expression.
    locals: AHashMap<Spur, ConstValue>,
    /// Values of module-member accesses (`m.CONST`) appearing in this
    /// expression, pre-resolved from the module's file (with privacy checks)
    /// before evaluation. The engine has no file/collector context of its own,
    /// so a `FieldGet` on a module is only evaluable as a sub-expression
    /// operand (`1 + m.CONST`) by looking its value up here (RUE-267). Keyed by
    /// the `FieldGet` instruction. Empty outside const-initializer evaluation.
    const_module_members: &'a AHashMap<InstRef, ConstValue>,
    /// The file whose code is currently being reduced (RUE-511). A
    /// module-qualified comptime call written in a `-> type` constructor body
    /// (`let O = b.Mk(T)`) names an import (`b`) of *this* file's import graph,
    /// not of the file that triggered the instantiation — so resolving the
    /// receiver as a module binding must key the tagged resolution by this file, not
    /// the instantiation site. Set from `ctx.current_file_id` when analyzing a
    /// body, and to the callee's `FunctionInfo.file_id` when reducing a
    /// type-constructor body. `None` where no file context is available (the
    /// receiver is then non-evaluable and the call is a runtime call).
    defining_file: Option<FileId>,
}

impl<'a> ComptimeEnv<'a> {
    /// The substitution maps augmented with this environment's comptime
    /// `let` locals (RUE-575): a type-valued local (`let Inner = Mk(T);`)
    /// participates in type resolution exactly like a `comptime T: type`
    /// parameter, and an integer/bool-valued local like a comptime value
    /// parameter, wherever the anonymous-type arms resolve field, payload,
    /// and method-signature types. Locals are inserted last, so an alias
    /// shadows a same-named enclosing parameter (lexical scoping).
    fn substs_with_locals(&self) -> (AHashMap<Spur, Type>, AHashMap<Spur, ConstValue>) {
        let mut type_subst = self.type_subst.clone();
        let mut value_subst = self.value_subst.clone();
        for (name, val) in &self.locals {
            match val {
                ConstValue::Type(t) => {
                    type_subst.insert(*name, *t);
                }
                other => {
                    value_subst.insert(*name, *other);
                }
            }
        }
        (type_subst, value_subst)
    }

    /// An environment with no substitutions and no type information.
    pub(crate) fn new() -> Self {
        Self {
            producer: None,
            canonical_identity: None,
            type_subst: &EMPTY_TYPE_SUBST,
            value_subst: &EMPTY_VALUE_SUBST,
            resolved_types: None,
            runtime_locals: None,
            runtime_params: None,
            runtime_binding_names: None,
            locals: AHashMap::new(),
            const_module_members: &EMPTY_MODULE_MEMBERS,
            defining_file: None,
        }
    }

    /// An environment with comptime parameter substitutions but no resolved
    /// types (used when evaluating a comptime function body at a call site).
    pub(crate) fn with_subst(
        type_subst: &'a AHashMap<Spur, Type>,
        value_subst: &'a AHashMap<Spur, ConstValue>,
    ) -> Self {
        Self {
            producer: None,
            canonical_identity: None,
            type_subst,
            value_subst,
            resolved_types: None,
            runtime_locals: None,
            runtime_params: None,
            runtime_binding_names: None,
            locals: AHashMap::new(),
            const_module_members: &EMPTY_MODULE_MEMBERS,
            defining_file: None,
        }
    }

    /// The environment for expressions inside the function currently being
    /// analyzed: comptime parameters in scope plus HM-resolved types.
    pub(crate) fn for_analysis(ctx: &'a AnalysisContext) -> Self {
        Self {
            producer: Some(ctx.producer),
            canonical_identity: Some((
                ctx.canonical_producer.clone(),
                ctx.canonical_producer_arguments.clone(),
            )),
            type_subst: &ctx.comptime_type_vars,
            value_subst: &ctx.comptime_value_vars,
            resolved_types: Some(ctx.resolved_types),
            runtime_locals: Some(&ctx.locals),
            runtime_params: Some((ctx.params, ctx.param_index)),
            runtime_binding_names: None,
            locals: AHashMap::new(),
            const_module_members: &EMPTY_MODULE_MEMBERS,
            defining_file: Some(ctx.current_file_id),
        }
    }
}

/// Decide whether a compile-time-known scrutinee value matches a match arm's
/// pattern (RUE-262). Returns:
/// - `Some(true)` / `Some(false)` — the pattern definitely does / does not match;
/// - `None` — the match can't be decided at compile time here (an enum-variant
///   `Path` pattern, or a scrutinee whose kind the pattern can't compare
///   against), so the caller treats the whole `match` as non-evaluable.
fn const_pattern_matches(pattern: &rue_rir::RirPatternView<'_>, scrut: ConstValue) -> Option<bool> {
    match pattern {
        rue_rir::RirPatternView::Wildcard(_) => Some(true),
        rue_rir::RirPatternView::Bool(b, _) => match scrut {
            ConstValue::Bool(sb) => Some(sb == *b),
            _ => None,
        },
        rue_rir::RirPatternView::Int {
            value, negative, ..
        } => match scrut {
            ConstValue::Integer(n) => {
                let pv = *value as i128;
                let pv = if *negative { -pv } else { pv };
                Some(n == pv)
            }
            _ => None,
        },
        // Enum-variant patterns aren't representable as a `ConstValue` (there
        // is no comptime enum-value form), so they can't be decided here.
        rue_rir::RirPatternView::Path { .. } => None,
    }
}

/// Check whether `value` is representable in integer type `ty`.
pub(crate) fn const_int_fits(value: i128, ty: Type) -> bool {
    ty.integer_semantics()
        .is_some_and(|integer| integer.fits_i128(value))
}

/// Build the E1200 error for a constant operation that would panic at runtime.
fn comptime_panic_err(reason: String, span: Span) -> CompileError {
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
        env.producer = Some(inst_ref);
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
        ctx: &AnalysisContext,
    ) -> CompileResult<Option<i128>> {
        let mut env = ComptimeEnv::for_analysis(ctx);
        // Full i128 backing value, NOT the i64 narrowing: `as_integer()`
        // returns None for a u64 constant above i64::MAX, which made an
        // exactly-known out-of-bounds index (`a[18446744073709551615]`)
        // indistinguishable from a runtime index and skip the compile-time
        // bounds check (RUE-532).
        Ok(self
            .eval_const_expr(inst_ref, &mut env)?
            .and_then(|v| v.as_int_value()))
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
                    let ty_name = ty.safe_name_with_pool(Some(self.body_type_pool()));
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

    /// Evaluate both operands of a binary operation as integers.
    ///
    /// Returns `Ok(None)` if either operand is not a compile-time integer.
    fn eval_int_operands(
        &mut self,
        lhs: InstRef,
        rhs: InstRef,
        env: &mut ComptimeEnv,
    ) -> CompileResult<Option<(i128, i128)>> {
        let Some(l) = self
            .eval_const_expr(lhs, env)?
            .and_then(ConstValue::as_int_value)
        else {
            return Ok(None);
        };
        let Some(r) = self
            .eval_const_expr(rhs, env)?
            .and_then(ConstValue::as_int_value)
        else {
            return Ok(None);
        };
        Ok(Some((l, r)))
    }

    /// The single compile-time evaluation engine. See the module docs for the
    /// outcome encoding (`Ok(Some)` / `Ok(None)` / `Err`).
    pub(crate) fn eval_const_expr(
        &mut self,
        inst_ref: InstRef,
        env: &mut ComptimeEnv,
    ) -> CompileResult<Option<ConstValue>> {
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };
        let span = inst.span;
        match &inst.data {
            // Integer literals. The literal itself must fit its resolved type
            // (the inner expression of a comptime block never goes through
            // `analyze_literal`, so this is where `300` at type u8 is caught).
            InstData::IntConst(value) => {
                let v = *value as i128;
                if let Some(ty) = self.const_expr_type(env, inst_ref) {
                    if !const_int_fits(v, ty) {
                        return Err(CompileError::new(
                            ErrorKind::LiteralOutOfRange {
                                value: *value,
                                ty: ty.safe_name_with_pool(Some(self.body_type_pool())),
                            },
                            span,
                        ));
                    }
                }
                Ok(Some(ConstValue::Integer(v)))
            }

            // Float literals stop here for the same reason they stop in
            // `analyze_inst_dispatch` (ADR-0065, RUE-1069): there is no
            // `comptime_float` value in `ConstValue` yet. Naming the real
            // reason matters more here than elsewhere — falling through to
            // the generic "not knowable at compile time" would be actively
            // wrong about a literal, which is the most compile-time-knowable
            // thing there is. Delete this arm when Phase 4 lands.
            InstData::FloatConst { .. } => {
                self.require_preview(
                    rue_error::PreviewFeature::Floats,
                    "a floating-point literal",
                    span,
                )?;
                Err(CompileError::new(ErrorKind::FloatNotYetImplemented, span))
            }

            // Boolean literals
            InstData::BoolConst(value) => Ok(Some(ConstValue::Bool(*value))),

            // Unit literal
            InstData::UnitConst => Ok(Some(ConstValue::Unit)),

            // Unary negation: -expr
            InstData::Neg { operand } => {
                let ty = self.const_expr_type(env, inst_ref);
                if let Some(ty) = ty {
                    if ty.is_unsigned() {
                        return Err(CompileError::new(
                            ErrorKind::CannotNegate(
                                ty.safe_name_with_pool(Some(self.body_type_pool())),
                            ),
                            span,
                        ));
                    }
                }
                if let InstData::IntConst(magnitude) = &self.body_rir_ref().get(*operand).data {
                    // The literal path uses mathematical magnitude semantics:
                    // unlike an ordinary runtime value, `128` must not first
                    // canonicalize to -128 before becoming `-128`.
                    let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                        || CheckedIntegerResult::from_raw((*magnitude as i128).checked_neg()),
                        |integer| integer.checked_neg_literal_report_i128(*magnitude as i128),
                    );
                    self.finish_arith(result, ty, "-", span)
                } else {
                    match self.eval_const_expr(*operand, env)? {
                        Some(ConstValue::Integer(n)) => {
                            let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                                || CheckedIntegerResult::from_raw(n.checked_neg()),
                                |integer| integer.checked_neg_report_i128(n),
                            );
                            self.finish_arith(result, ty, "-", span)
                        }
                        // Can't negate a boolean, type, or unit
                        _ => Ok(None),
                    }
                }
            }

            // Logical NOT: !expr
            InstData::Not { operand } => {
                match self.eval_const_expr(*operand, env)? {
                    Some(ConstValue::Bool(b)) => Ok(Some(ConstValue::Bool(!b))),
                    // Can't logical-NOT an integer, type, or unit
                    _ => Ok(None),
                }
            }

            // Binary arithmetic operations, checked at the operand type
            InstData::Add { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.const_expr_type(env, inst_ref);
                let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                    || CheckedIntegerResult::from_raw(l.checked_add(r)),
                    |integer| integer.checked_add_report_i128(l, r),
                );
                self.finish_arith(result, ty, "+", span)
            }
            InstData::Sub { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.const_expr_type(env, inst_ref);
                let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                    || CheckedIntegerResult::from_raw(l.checked_sub(r)),
                    |integer| integer.checked_sub_report_i128(l, r),
                );
                self.finish_arith(result, ty, "-", span)
            }
            InstData::Mul { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.const_expr_type(env, inst_ref);
                let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                    || CheckedIntegerResult::from_raw(l.checked_mul(r)),
                    |integer| integer.checked_mul_report_i128(l, r),
                );
                self.finish_arith(result, ty, "*", span)
            }
            InstData::Div { lhs, rhs } | InstData::Mod { lhs, rhs } => {
                let is_div = matches!(&inst.data, InstData::Div { .. });
                let op = if is_div { "/" } else { "%" };
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.const_expr_type(env, inst_ref);
                if r == 0 {
                    let what = if is_div { "division" } else { "remainder" };
                    return match ty {
                        Some(_) => Err(comptime_panic_err(
                            format!("{} by zero (this operation would panic at runtime)", what),
                            span,
                        )),
                        // Untyped fallback: defer to the runtime check.
                        None => Ok(None),
                    };
                }
                // Untyped evaluation retains its historical i64 fallback;
                // typed MIN / -1 trapping is owned by the kernel report.
                if r == -1 && ty.is_none() && l == i128::from(i64::MIN) {
                    return Ok(None);
                }
                let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                    || {
                        CheckedIntegerResult::from_raw(if is_div {
                            l.checked_div(r)
                        } else {
                            l.checked_rem(r)
                        })
                    },
                    |integer| {
                        if is_div {
                            integer.checked_div_report_i128(l, r)
                        } else {
                            integer.checked_rem_report_i128(l, r)
                        }
                    },
                );
                self.finish_arith(result, ty, op, span)
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                let l = self.eval_const_expr(*lhs, env)?;
                let r = self.eval_const_expr(*rhs, env)?;
                match (l, r) {
                    (Some(ConstValue::Integer(a)), Some(ConstValue::Integer(b))) => {
                        Ok(Some(ConstValue::Bool(a == b)))
                    }
                    (Some(ConstValue::Bool(a)), Some(ConstValue::Bool(b))) => {
                        Ok(Some(ConstValue::Bool(a == b)))
                    }
                    _ => Ok(None), // Mixed or non-constant operands
                }
            }
            InstData::Ne { lhs, rhs } => {
                let l = self.eval_const_expr(*lhs, env)?;
                let r = self.eval_const_expr(*rhs, env)?;
                match (l, r) {
                    (Some(ConstValue::Integer(a)), Some(ConstValue::Integer(b))) => {
                        Ok(Some(ConstValue::Bool(a != b)))
                    }
                    (Some(ConstValue::Bool(a)), Some(ConstValue::Bool(b))) => {
                        Ok(Some(ConstValue::Bool(a != b)))
                    }
                    _ => Ok(None),
                }
            }
            InstData::Lt { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(ConstValue::Bool(l < r)))
            }
            InstData::Gt { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(ConstValue::Bool(l > r)))
            }
            InstData::Le { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(ConstValue::Bool(l <= r)))
            }
            InstData::Ge { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(ConstValue::Bool(l >= r)))
            }

            // Logical operations: short-circuit like the runtime, so a
            // non-constant (or would-panic) RHS is irrelevant when the LHS
            // already decides the result.
            InstData::And { lhs, rhs } => match self.eval_const_expr(*lhs, env)? {
                Some(ConstValue::Bool(false)) => Ok(Some(ConstValue::Bool(false))),
                Some(ConstValue::Bool(true)) => match self.eval_const_expr(*rhs, env)? {
                    Some(ConstValue::Bool(b)) => Ok(Some(ConstValue::Bool(b))),
                    _ => Ok(None),
                },
                _ => Ok(None),
            },
            InstData::Or { lhs, rhs } => match self.eval_const_expr(*lhs, env)? {
                Some(ConstValue::Bool(true)) => Ok(Some(ConstValue::Bool(true))),
                Some(ConstValue::Bool(false)) => match self.eval_const_expr(*rhs, env)? {
                    Some(ConstValue::Bool(b)) => Ok(Some(ConstValue::Bool(b))),
                    _ => Ok(None),
                },
                _ => Ok(None),
            },

            // Bitwise operations. For values in range of their type these are
            // closed (no overflow possible), so no range check is needed.
            InstData::BitAnd { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(ConstValue::Integer(l & r)))
            }
            InstData::BitOr { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(ConstValue::Integer(l | r)))
            }
            InstData::BitXor { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(ConstValue::Integer(l ^ r)))
            }

            // Shifts: the amount is masked modulo the bit width and the
            // result truncated to the operand width (spec 4.3a:10), exactly
            // matching the runtime semantics (RUE-29).
            InstData::Shl { lhs, rhs } | InstData::Shr { lhs, rhs } => {
                let is_shl = matches!(&inst.data, InstData::Shl { .. });
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                match self.const_expr_type(env, inst_ref) {
                    Some(ty) => {
                        let integer = ty
                            .integer_semantics()
                            .expect("const_expr_type returned non-integer");
                        // Two's-complement AND masks negative amounts the same
                        // way the hardware masks the count register.
                        let v = integer.shift_i128(l, r, is_shl);
                        Ok(Some(ConstValue::Integer(v)))
                    }
                    None => {
                        // Without the operand type the width is unknown, so
                        // only fold amounts < 8 (safe for every width) and
                        // defer the rest to runtime.
                        if !(0..8).contains(&r) {
                            return Ok(None);
                        }
                        Ok(Some(ConstValue::Integer(if is_shl {
                            l << r
                        } else {
                            l >> r
                        })))
                    }
                }
            }

            // Bitwise NOT: truncated to the operand width (`~0` as u8 = 255).
            InstData::BitNot { operand } => {
                let Some(n) = self
                    .eval_const_expr(*operand, env)?
                    .and_then(ConstValue::as_int_value)
                else {
                    return Ok(None);
                };
                let v = match self.const_expr_type(env, inst_ref) {
                    Some(ty) => ty
                        .integer_semantics()
                        .expect("bitnot requires an integer type")
                        .bitnot_i128(n),
                    None => !n,
                };
                Ok(Some(ConstValue::Integer(v)))
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => self.eval_const_expr(*expr, env),

            // Block: evaluate `let` statements into the environment, then the
            // tail expression. Loops, assignments and calls are not supported
            // and make the block non-evaluable.
            InstData::Block { instructions } => {
                let stmt_refs = self.body_rir_ref().block_insts(instructions).to_vec();
                if stmt_refs.is_empty() {
                    return Ok(Some(ConstValue::Unit));
                }
                // Bindings are scoped to the block.
                let saved_locals = env.locals.clone();
                let mut result = Some(ConstValue::Unit);
                for (i, stmt_ref) in stmt_refs.iter().copied().enumerate() {
                    let is_tail = i + 1 == stmt_refs.len();
                    let value = if let InstData::Alloc { name, init, .. } =
                        &self.body_rir_ref().get(stmt_ref).data
                    {
                        let (name, init) = (*name, *init);
                        let Some(v) = self.eval_const_expr(init, env)? else {
                            env.locals = saved_locals;
                            return Ok(None);
                        };
                        if let Some(name) = name {
                            env.locals.insert(name, v);
                        }
                        // A `let` statement itself evaluates to unit.
                        ConstValue::Unit
                    } else {
                        let Some(v) = self.eval_const_expr(stmt_ref, env)? else {
                            env.locals = saved_locals;
                            return Ok(None);
                        };
                        v
                    };
                    if is_tail {
                        result = Some(value);
                    }
                }
                env.locals = saved_locals;
                Ok(result)
            }

            // Comptime-known `if`: select the taken branch and reduce to its
            // value. This is what lets an `if` in a `-> type` body pick a
            // struct/enum branch at compile time (spec 4.14:17, RUE-262) — the
            // same branch selection ordinary comptime values already relied on
            // through the block/let path, now available as an expression. A
            // non-constant condition makes the whole `if` non-evaluable.
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let (cond, then_block, else_block) = (*cond, *then_block, *else_block);
                match self.eval_const_expr(cond, env)? {
                    Some(ConstValue::Bool(true)) => self.eval_const_expr(then_block, env),
                    Some(ConstValue::Bool(false)) => match else_block {
                        Some(else_block) => self.eval_const_expr(else_block, env),
                        // `if c { .. }` with no else yields unit when false.
                        None => Ok(Some(ConstValue::Unit)),
                    },
                    // Non-constant (or non-bool) condition: not evaluable.
                    _ => Ok(None),
                }
            }

            // Comptime-known `match`: evaluate the scrutinee, select the first
            // arm whose pattern matches, and reduce to that arm's body value
            // (spec 4.14:19, RUE-262). An enum-variant (`Path`) pattern isn't
            // representable as a `ConstValue`, and a non-constant scrutinee is
            // not decidable here — both make the `match` non-evaluable.
            InstData::Match { scrutinee, arms } => {
                let scrutinee = *scrutinee;
                let Some(scrut) = self.eval_const_expr(scrutinee, env)? else {
                    return Ok(None);
                };
                let arms = self.body_rir_ref().match_arms(arms).to_vec();
                for (pattern, body) in arms.iter() {
                    match const_pattern_matches(pattern, scrut) {
                        Some(true) => return self.eval_const_expr(*body, env),
                        Some(false) => continue,
                        // Undecidable pattern (e.g. an enum-variant `Path`
                        // against a non-representable scrutinee): bail out.
                        None => return Ok(None),
                    }
                }
                // No arm matched. Exhaustiveness checking should make this
                // unreachable for a well-typed match; treat as non-evaluable.
                Ok(None)
            }

            // Anonymous struct type: evaluate to a comptime type value,
            // resolving field types through the type substitution.
            InstData::AnonStructType {
                fields,
                methods,
                anchor,
            } => {
                let field_decls = self.body_rir_ref().anon_struct_fields(fields).to_vec();

                // Comptime `let` locals in scope participate in field-type
                // resolution (`let Inner = Mk(T); struct { x: Inner }`,
                // RUE-575), alongside the enclosing parameters.
                let (local_type_subst, local_value_subst) = env.substs_with_locals();

                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.body_interner().resolve(&name_sym).to_string();
                    // Field types resolve through both the type substitution
                    // (`comptime T: type`) and the value substitution
                    // (`comptime N: i32`, so an `[i32; N]` field gets a concrete
                    // length at each specialization; RUE-16).
                    let Some(field_ty) = self
                        .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                            type_sym,
                            &local_type_subst,
                            &local_value_subst,
                            span,
                        )
                    else {
                        return Ok(None);
                    };
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                let method_sigs =
                    self.extract_anon_method_sigs(methods, &local_type_subst, &local_value_subst);

                let Some((producer, arguments)) = env.canonical_identity.clone() else {
                    return Ok(None);
                };
                let (struct_ty, _is_new) = self.find_or_create_anon_struct(
                    crate::AnonymousNominalKey {
                        kind: crate::AnonymousNominalKind::Struct,
                        producer,
                        anchor: anchor.clone(),
                        arguments,
                    },
                    &struct_fields,
                    &method_sigs,
                    &local_value_subst,
                )?;

                // Register methods if present and not yet registered for this
                // struct (it may have been created earlier without methods).
                if !self.body_rir_ref().anon_struct_methods(methods).is_empty() {
                    // A method that declares its own `comptime T: type`
                    // parameter would need per-call monomorphization over that
                    // parameter, which is unsupported (RUE-284). Reject it at
                    // the method declaration so the enclosing `-> type`
                    // reduction cannot degrade into an unrelated E1200 at the
                    // instantiation site.
                    if let Some((method_span, method_name)) =
                        self.find_method_own_comptime_type_param(methods)
                    {
                        return Err(CompileError::new(
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
                            method_span,
                        ));
                    }
                    let Some(struct_id) = struct_ty.as_struct() else {
                        return Ok(None);
                    };

                    let method_refs = self.body_rir_ref().anon_struct_methods(methods);
                    let first_method_ref = method_refs.get(0).unwrap();
                    let first_method_inst = self.body_rir_ref().get(first_method_ref);
                    if let InstData::FnDecl {
                        name: method_name, ..
                    } = &first_method_inst.data
                    {
                        let needs_registration = !self.has_method((struct_id, *method_name));

                        if needs_registration
                            && self
                                .register_anon_struct_methods_for_comptime_with_subst(
                                    struct_id,
                                    struct_ty,
                                    methods,
                                    &local_type_subst,
                                    &local_value_subst,
                                )
                                .is_none()
                        {
                            // Registration failure (e.g. duplicate method
                            // names) makes the type non-evaluable; the
                            // caller reports the comptime failure.
                            return Ok(None);
                        }

                        // Remember the enclosing type substitution (e.g.
                        // `T -> i32` for `Vec(i32)`) so it resolves inside every
                        // method *body*, not just the signatures registered
                        // above (RUE-313). Method bodies are analyzed later, in
                        // a separate pass that has no other way to recover the
                        // constructor's type parameters.
                        if needs_registration && !local_type_subst.is_empty() {
                            self.set_anon_struct_type_subst(struct_id, local_type_subst.clone());
                        }
                    }
                }
                Ok(Some(ConstValue::Type(struct_ty)))
            }

            // Anonymous enum type: evaluate to a comptime type value, resolving
            // each variant's payload types through the type/value substitution.
            // The enum analog of the AnonStructType arm above — this is what
            // makes `fn Option(comptime T: type) -> type { enum { Some(T), None } }`
            // monomorphize per instantiation (ADR-0038, RUE-6 phase 2).
            InstData::AnonEnumType {
                variants,
                payloads,
                anchor,
            } => {
                let variant_syms: Vec<lasso::Spur> =
                    self.body_rir_ref().anon_enum_variants(variants).to_vec();
                let payload_symbols: Vec<Vec<rue_rir::RirTypeSyntaxRef>> = self
                    .body_rir_ref()
                    .anon_enum_payloads(payloads, variants)
                    .map(|payload| payload.to_vec())
                    .collect();

                // Decode the self-describing payload region into per-variant
                // type-symbol lists (parallel to `variant_syms`), then resolve
                // each payload type through the substitutions.
                // Comptime `let` locals participate in payload-type
                // resolution, matching the struct arm (RUE-575).
                let (enum_type_subst, enum_value_subst) = env.substs_with_locals();

                let mut variant_names: Vec<String> = Vec::with_capacity(variant_syms.len());
                let mut variant_payloads: Vec<Vec<Type>> = Vec::with_capacity(variant_syms.len());
                for (&vsym, symbols) in variant_syms.iter().zip(payload_symbols) {
                    variant_names.push(self.body_interner().resolve(&vsym).to_string());
                    let mut tys: Vec<Type> = Vec::with_capacity(symbols.len());
                    for ty_sym in symbols {
                        let Some(ty) = self
                            .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                                ty_sym,
                                &enum_type_subst,
                                &enum_value_subst,
                                span,
                            )
                        else {
                            return Ok(None);
                        };
                        tys.push(ty);
                    }
                    variant_payloads.push(tys);
                }

                let Some((producer, arguments)) = env.canonical_identity.clone() else {
                    return Ok(None);
                };
                let enum_ty = self.find_or_create_anon_enum(
                    crate::AnonymousNominalKey {
                        kind: crate::AnonymousNominalKind::Enum,
                        producer,
                        anchor: anchor.clone(),
                        arguments,
                    },
                    &variant_names,
                    &variant_payloads,
                )?;
                Ok(Some(ConstValue::Type(enum_ty)))
            }

            // TypeConst: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                let type_name = *type_name;
                // Type parameters in scope substitute first.
                if let Some(type_symbol) = self.rir_type_named_symbol(type_name) {
                    if let Some(&ty) = env.type_subst.get(&type_symbol) {
                        return Ok(Some(ConstValue::Type(ty)));
                    }
                    // A named type (primitive / struct / enum) resolves directly.
                    if let Some(ty) = self.resolve_named_type_value(type_symbol, span)? {
                        return Ok(Some(ConstValue::Type(ty)));
                    }
                }
                // A *composite* or *unit* type value — `[i32; 2]`, `()`,
                // `ptr const T` — is an equally-valid type argument (Appendix A
                // treats them as unambiguous type spellings; RUE-565). Its
                // TypeConst carries the composite spelling as the interned
                // `type_name`, so decode it through the full comptime type
                // resolver under the current substitutions (an inner element /
                // pointee naming an enclosing `comptime T` still resolves). An
                // unresolvable spelling stays non-evaluable (`None`).
                Ok(self
                    .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                        type_name,
                        env.type_subst,
                        env.value_subst,
                        span,
                    )
                    .map(ConstValue::Type))
            }

            // An array-repeat expression `[T; N]` used as a comptime *type* value
            // (RUE-565). The surface form `[i32; 2]` in expression position parses
            // as an array-repeat literal whose element is a type value; when that
            // element reduces to a `ConstValue::Type`, the whole expression is the
            // array TYPE `[T; N]` — a legal type-constructor argument
            // (`Option([i32; 2])`). A repeat over a *runtime* element is a genuine
            // array value literal and is not comptime-foldable here (`None`).
            InstData::ArrayRepeat { value, count } => {
                let (value, count) = (*value, count.clone());
                let Some(ConstValue::Type(elem_ty)) = self.eval_const_expr(value, env)? else {
                    return Ok(None);
                };
                let len = match count {
                    RepeatCount::Literal(n) => n,
                    RepeatCount::Named(sym) => {
                        let name = self.body_interner().resolve(&sym).to_string();
                        match self.resolve_array_length(
                            &ArrayLen::Named(name),
                            span,
                            Some(env.value_subst),
                        ) {
                            Ok(n) => n,
                            Err(_) => return Ok(None),
                        }
                    }
                };
                let array_type_id = self.get_or_create_array_type(elem_ty, len);
                Ok(Some(ConstValue::Type(Type::new_array(array_type_id))))
            }

            // VarRef: comptime let-bindings, comptime parameters, file-level
            // constants, then type names.
            InstData::VarRef { name, .. } => {
                // 1. `let` bindings inside the comptime expression
                if let Some(&v) = env.locals.get(name) {
                    return Ok(Some(v));
                }
                // 2. Runtime locals shadow comptime parameters and file-level
                //    constants: a reference that resolves to one is not
                //    compile-time evaluable (spec 4.14:6).
                if let Some(locals) = env.runtime_locals {
                    if locals.contains_key(name) {
                        return Ok(None);
                    }
                }
                if let Some(names) = env.runtime_binding_names
                    && names.contains(name)
                {
                    return Ok(None);
                }
                // 3. Comptime type parameters in scope
                if let Some(&ty) = env.type_subst.get(name) {
                    return Ok(Some(ConstValue::Type(ty)));
                }
                // 4. Comptime value parameters in scope
                if let Some(&v) = env.value_subst.get(name) {
                    return Ok(Some(v));
                }
                // 5. Runtime parameters shadow file-level constants and type
                //    names. A comptime parameter with a concrete value was
                //    already handled by the substitution maps above.
                if let Some((params, param_index)) = env.runtime_params {
                    if param_index.get(params, *name).is_some() {
                        return Ok(None);
                    }
                }
                // 6. File-level constants: the value was evaluated once
                //    (and range-checked against the declared type) during
                //    declaration gathering — use it directly. Re-evaluating
                //    the initializer here would fail for forms only the
                //    declaration collector can resolve (module member
                //    access, RUE-160) and was exponential for const chains.
                //    Module-typed constants never appear in this table
                //    (module bindings are a distinct tagged resolution).
                //    Privacy applies here too (E0460, RUE-183): the table is
                //    global, so a const initializer in one directory could
                //    otherwise read a private constant from another. The
                //    VarRef's own span locates the referencing file;
                //    speculative callers (`try_evaluate_const*`) swallow the
                //    error and defer to runtime analysis, which re-checks.
                if let Some(info) = self.value_const(&(span.file_id, *name)) {
                    self.record_body_named_dependency(
                        super::NamedConstDependencyTargetEvent::ValueConst {
                            file: info.span.file_id.index(),
                            name: self.body_interner().resolve(name).to_string(),
                        },
                    );
                    self.check_unqualified_visibility(
                        "constant",
                        self.body_interner().resolve(name),
                        info.span.file_id,
                        info.is_pub,
                        span,
                    )?;
                    // String constants stay out of the comptime engine: no
                    // engine operation consumes them (no comptime string
                    // params or string arithmetic), so treat a reference as
                    // non-evaluable instead of leaking a value the arms
                    // below would mis-type (RUE-957). Use sites materialize
                    // string constants through the runtime path instead.
                    if matches!(info.value, super::ConstValue::String(_)) {
                        return Ok(None);
                    }
                    return Ok(Some(info.value));
                }
                // 7. Type names used as values (e.g. `Point` in
                //    `fn make_type() -> type { Point }`)
                let resolved = self.resolve_named_type_value(*name, span)?;
                if let Some(ty) = resolved {
                    match ty.kind() {
                        TypeKind::Struct(id) => {
                            let def = self
                                .body_type_pool()
                                .struct_metadata(id)
                                .expect("struct type must have declaration metadata");
                            self.record_body_named_dependency(
                                super::NamedConstDependencyTargetEvent::NamedType {
                                    file: def.file_id.index(),
                                    name: def.name.to_string(),
                                    kind: super::DeclarationTypeDependencyTargetKind::Struct,
                                },
                            );
                        }
                        TypeKind::Enum(id) => {
                            let def = self
                                .body_type_pool()
                                .enum_metadata(id)
                                .expect("enum type must have declaration metadata");
                            self.record_body_named_dependency(
                                super::NamedConstDependencyTargetEvent::NamedType {
                                    file: def.file_id.index(),
                                    name: def.name.to_string(),
                                    kind: super::DeclarationTypeDependencyTargetKind::Enum,
                                },
                            );
                        }
                        _ => {}
                    }
                }
                Ok(resolved.map(ConstValue::Type))
            }

            // Call to a `-> type` function: reduce it to the resulting type
            // value when the callee is a type constructor and every argument
            // is compile-time known. This makes comptime type-function calls
            // compose in ANY position — a delegating return body
            // (`fn Alias() -> type { Point() }`), a nested argument
            // (`WrapA(WrapA(i32))`), and chains thereof (RUE-251).
            InstData::Call { name, args } => {
                let name = *name;
                self.eval_comptime_type_call(name, args, env, false, span)
                    .map_err(|e| Self::label_ctor_instantiation_site(e, span))
            }

            // Module-member access (`m.CONST`) as an operand of a larger const
            // initializer. The value was pre-resolved from the module's file
            // (with privacy checks) before evaluation — see the
            // `const_module_members` field — since the engine has no file or
            // constant-collector context to resolve it here. A member absent
            // from the map may still be a member-access *type* path used as a
            // comptime type-constructor argument (`std.strbuf.StrBuf` in
            // `Result(std.strbuf.StrBuf, i32)`, RUE-948): resolve that chain to
            // its nominal type through the same walker the qualified
            // type-annotation position uses. A base that is neither a
            // pre-resolved member value nor a module type path (a runtime
            // value's field) stays non-evaluable, so the caller reports it
            // (RUE-267).
            InstData::FieldGet { .. } => {
                if let Some(&value) = env.const_module_members.get(&inst_ref) {
                    return Ok(Some(value));
                }
                self.eval_field_get_type_path(inst_ref, span, env)
            }

            // Type intrinsic in comptime position. `@require_droppable(T)` is the
            // owning-container well-formedness gate (RUE-388/RUE-646): std's
            // `ArrayBuf(T)` calls it in its `-> type` constructor body so that
            // instantiating the container with an element type it cannot yet
            // correctly own — one that is `linear` — is rejected at instantiation
            // time (E0499). Droppable-but-non-linear elements are accepted: the
            // container runs each live element's drop glue before freeing its
            // buffer (RUE-646). It reduces to unit so the surrounding block
            // body still yields the `struct { .. }` tail. `@size_of`/`@align_of`
            // are not comptime-foldable here and stay non-evaluable (spec
            // 4.14:29); `@int_max`/`@int_min` depend only on the type identity,
            // not layout, so they evaluate to their integer bound (RUE-694).
            InstData::TypeIntrinsic { name, type_arg } => {
                let (name, type_arg) = (*name, *type_arg);
                let gate = self.body_interner().resolve(&name);
                // Both well-formedness gates reduce to unit at comptime:
                // `@require_droppable` (instantiation-time, rejects `linear`) and
                // `@require_trivially_droppable` (read-time, rejects drop glue —
                // RUE-651). Any other type intrinsic (`@size_of`/`@align_of`) is
                // not comptime-foldable here.
                let is_droppable_gate = gate == "require_droppable";
                let is_trivial_gate = gate == "require_trivially_droppable";
                let is_int_bound = gate == "int_max" || gate == "int_min";
                if is_int_bound {
                    let is_max = gate == "int_max";
                    // A still-unresolved type parameter makes the intrinsic
                    // non-evaluable here; it folds at a concrete instantiation.
                    let Some(int_ty) = self
                        .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                            type_arg,
                            env.type_subst,
                            env.value_subst,
                            span,
                        )
                    else {
                        return Ok(None);
                    };
                    let bound = if is_max {
                        int_ty.int_max()
                    } else {
                        int_ty.int_min()
                    };
                    // A non-integer argument is diagnosed by runtime analysis
                    // (`analyze_type_intrinsic`, E0702); stay non-evaluable
                    // rather than duplicating the diagnostic.
                    return Ok(bound.map(ConstValue::Integer));
                }
                if !is_droppable_gate && !is_trivial_gate {
                    return Ok(None);
                }
                // Resolve the element type through the enclosing comptime
                // substitutions (`T -> Inner` for `ArrayBuf(Inner)`); a
                // still-unresolved type parameter makes the gate non-evaluable
                // (it will be re-checked at a concrete instantiation).
                let Some(elem_ty) = self
                    .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                        type_arg,
                        env.type_subst,
                        env.value_subst,
                        span,
                    )
                else {
                    return Ok(None);
                };
                if is_trivial_gate {
                    self.check_trivially_droppable(elem_ty, span)?;
                } else {
                    self.check_require_droppable(elem_ty, span)?;
                }
                Ok(Some(ConstValue::Unit))
            }

            // Module-qualified comptime type-constructor call in value position,
            // e.g. `let O = b.Mk(T)` inside a `-> type` constructor body that is
            // being reduced (RUE-511). The receiver must be an unshadowed
            // `VarRef` naming a module binding of the *defining* file; membership
            // and visibility are validated before the call is reduced through the
            // same path unqualified calls take. Any other receiver (a runtime
            // value's method, a shadowed name) is a genuine runtime call and
            // stays non-evaluable.
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                let (receiver, method) = (*receiver, *method);
                self.eval_module_qualified_comptime_call(receiver, method, args, span, env)
            }

            // Everything else requires runtime evaluation
            _ => Ok(None),
        }
    }

    /// Reduce a module-qualified comptime type-constructor call written in
    /// *value position* inside a reducing `-> type` constructor body — the
    /// cross-module analogue of the `Call` arm's `eval_comptime_type_call`
    /// (RUE-511). Returns `Ok(None)` (a runtime call, non-evaluable) unless the
    /// receiver is an unshadowed `VarRef` that names a module binding of the
    /// environment's `defining_file`, and the named member is a `-> type`
    /// constructor that actually belongs to that module's file.
    ///
    /// The receiver module's defining file is authoritative, so a same-named
    /// constructor in a different file cannot satisfy `b.Mk`. Visibility is
    /// enforced the same way the qualified type-annotation
    /// path enforces it (E0460/E0706 surface as the reduction's E1200 here since
    /// the comptime engine cannot itself emit a diagnostic mid-reduction).
    fn eval_module_qualified_comptime_call(
        &mut self,
        receiver: InstRef,
        method: Spur,
        args: &rue_rir::RirCallArgsRange,
        span: Span,
        env: &mut ComptimeEnv,
    ) -> CompileResult<Option<ConstValue>> {
        // The receiver may be a bare import binding (`ab.Mk(..)`) or a
        // re-export chain through module facades (`std.arraybuf.ArrayBuf(..)`,
        // RUE-609); collect the dotted spine down to its root name. Any other
        // receiver shape (a runtime value's method) is a genuine runtime call
        // and stays non-evaluable.
        let mut chain_rev: Vec<Spur> = Vec::new();
        let mut cursor = receiver;
        let recv_name = loop {
            match self.body_rir_ref().get(cursor).data {
                InstData::VarRef { name, .. } => break name,
                InstData::FieldGet { base, field } => {
                    chain_rev.push(field);
                    cursor = base;
                }
                _ => return Ok(None),
            }
        };
        // A `let`-binding, runtime local, or comptime parameter of the same name
        // shadows the module import (spec 4.14:6) — then this is not a module
        // call and is non-evaluable.
        if env.locals.contains_key(&recv_name) {
            return Ok(None);
        }
        if let Some(locals) = env.runtime_locals {
            if locals.contains_key(&recv_name) {
                return Ok(None);
            }
        }
        // The precompute's walk conveys runtime locals through this set rather
        // than `runtime_locals`; the qualified-constant walk already honors it,
        // and the same spec 4.14:6 shadowing applies to a qualified call.
        if let Some(names) = env.runtime_binding_names
            && names.contains(&recv_name)
        {
            return Ok(None);
        }
        if env.type_subst.contains_key(&recv_name) || env.value_subst.contains_key(&recv_name) {
            return Ok(None);
        }
        // The receiver's root names an import of the file whose body is being
        // reduced; any further segments walk re-export bindings in the
        // imported files (the same walk qualified type annotations use).
        let Some(file_id) = env.defining_file else {
            return Ok(None);
        };
        let module_file_id = if chain_rev.is_empty() {
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
            let mut segment_strings: Vec<String> =
                vec![self.body_interner().resolve(&recv_name).to_owned()];
            segment_strings.extend(
                chain_rev
                    .iter()
                    .rev()
                    .map(|s| self.body_interner().resolve(s).to_owned()),
            );
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
        // Reduce through the shared path; arguments are evaluated in the current
        // environment so `T` (an enclosing comptime parameter) still resolves.
        self.eval_comptime_type_call(function_key, args, env, true, span)
            .map_err(|e| Self::label_ctor_instantiation_site(e, span))
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
    fn eval_field_get_type_path(
        &mut self,
        inst_ref: InstRef,
        span: Span,
        env: &ComptimeEnv,
    ) -> CompileResult<Option<ConstValue>> {
        // Collect the dotted spine down to its root name, exactly as the
        // module-qualified call walk does. Any non-`FieldGet`/`VarRef` link
        // (a runtime value's field) is not a type path.
        let mut chain_rev: Vec<Spur> = Vec::new();
        let mut cursor = inst_ref;
        let root_name = loop {
            match self.body_rir_ref().get(cursor).data {
                InstData::VarRef { name, .. } => break name,
                InstData::FieldGet { base, field } => {
                    chain_rev.push(field);
                    cursor = base;
                }
                _ => return Ok(None),
            }
        };
        // A `let`-binding, runtime local, runtime parameter, or comptime
        // parameter of the same name shadows the module import (spec 4.14:6);
        // the chain is then a field access on that binding, not a type path.
        if env.locals.contains_key(&root_name) {
            return Ok(None);
        }
        if let Some(locals) = env.runtime_locals
            && locals.contains_key(&root_name)
        {
            return Ok(None);
        }
        if let Some(names) = env.runtime_binding_names
            && names.contains(&root_name)
        {
            return Ok(None);
        }
        if let Some((params, param_index)) = env.runtime_params
            && param_index.get(params, root_name).is_some()
        {
            return Ok(None);
        }
        if env.type_subst.contains_key(&root_name) || env.value_subst.contains_key(&root_name) {
            return Ok(None);
        }
        // The root names an import of the file whose body is being reduced; the
        // remaining segments walk re-export bindings and the final member the
        // same way qualified type annotations do.
        let Some(root_file) = env.defining_file else {
            return Ok(None);
        };
        let mut segment_strings: Vec<String> =
            vec![self.body_interner().resolve(&root_name).to_owned()];
        segment_strings.extend(
            chain_rev
                .iter()
                .rev()
                .map(|s| self.body_interner().resolve(s).to_owned()),
        );
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

    /// Reduce a call to a comptime-evaluable function to its resulting value,
    /// when every argument is compile-time known. Shared by
    /// [`eval_const_expr`]'s `Call` arm (so nested/delegating calls compose)
    /// and the analysis pass (RUE-251).
    ///
    /// Two callee shapes reduce here:
    ///
    /// - **`-> type` constructors** — reduce to a [`ConstValue::Type`], so
    ///   `Pair(i32)` composes in any position (RUE-251).
    /// - **Value-returning functions with all-comptime (or no) parameters** —
    ///   reduce to their [`ConstValue::Integer`]/[`ConstValue::Bool`] result,
    ///   which is what lets a comptime-recursive `fn fact(comptime n: i32)`
    ///   produce a compile-time constant usable as an array length or inside a
    ///   `comptime { }` block (RUE-163 facet 1, spec 4.14:5). A function with
    ///   any *runtime* parameter is a genuine runtime call and is left
    ///   non-evaluable, so ordinary calls like `add(3, 5)` (runtime `a`, `b`)
    ///   are not folded here.
    ///
    /// Returns `Ok(None)` — not an error — for an unknown callee, a
    /// non-const argument, an arity mismatch, or a call that does not meet the
    /// implicit-comptime gate: the call is then just a runtime call and simply
    /// non-evaluable here. An explicit argument mode that disagrees with the
    /// callee is a source error and returns `Err`, as does a failure while
    /// reducing the body (arithmetic overflow, recursion-depth overrun);
    /// opportunistic callers swallow those errors.
    ///
    /// [`eval_const_expr`]: Self::eval_const_expr
    fn eval_comptime_type_call(
        &mut self,
        name: Spur,
        args: &rue_rir::RirCallArgsRange,
        env: &mut ComptimeEnv,
        name_is_resolved_key: bool,
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
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
        let param_comptime_type = self.comptime_type_param_flags(&fn_info);
        let args = self.body_rir_ref().call_args(args).to_vec();
        if args.len() != param_names.len() {
            return Ok(None);
        }
        // A comptime reduction is still a source-level call. Validate its
        // explicit passing modes before the evaluator erases them while
        // binding constant arguments (RUE-634).
        self.validate_explicit_call_modes(&args, param_modes.iter().copied())?;

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
        // Evaluate each argument compositionally in the current environment,
        // so a nested type-function call (`WrapA(i32)` inside
        // `WrapA(WrapA(i32))`) and references to enclosing comptime
        // params/aliases both resolve. A non-const argument makes the whole
        // call non-evaluable.
        let mut callee_types: AHashMap<Spur, Type> = AHashMap::new();
        let mut callee_values: AHashMap<Spur, ConstValue> = AHashMap::new();
        for (i, arg) in args.iter().enumerate() {
            let Some(v) = self.eval_const_expr(arg.value, env)? else {
                return Ok(None);
            };
            // Provider-backed callable facts can materialize a callee without
            // retaining its source RIR parameter symbols. The evaluated value
            // is still authoritative for a missing flag: only a `type`-typed
            // comptime parameter can accept `ConstValue::Type`.
            let is_comptime_type = param_comptime_type
                .get(i)
                .copied()
                .unwrap_or(matches!(v, ConstValue::Type(_)));
            match (is_comptime_type, v) {
                (true, ConstValue::Type(t)) => {
                    callee_types.insert(param_names[i], t);
                }
                (true, ConstValue::Unit) => {
                    callee_types.insert(param_names[i], Type::UNIT);
                }
                (true, _) => return Ok(None),
                (false, value) => {
                    callee_values.insert(param_names[i], value);
                }
            }
        }
        // The callee body sees only its own parameters. Reduce it with the
        // freshly-built substitution maps.
        self.reduce_type_ctor_body(name_key, &callee_types, &callee_values, span)
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
            self.body_type_pool(),
            function_name,
            param_name,
            value,
            expected,
            span,
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
        if let Some(result) =
            self.reduce_external_comptime_call(name, callee_types, callee_values, span)
        {
            return result;
        }
        let Some(fn_body_info) = self.function_body_info(name) else {
            return Ok(None);
        };
        let fn_info = crate::sema::info::FunctionCallInfo::from_body(fn_body_info);
        self.validate_comptime_call_substitutions(
            name,
            &fn_info,
            callee_types,
            callee_values,
            fn_body_info.span,
        )?;
        let fn_body = fn_body_info.body;
        let fn_span = fn_body_info.span;
        let fn_file = fn_body_info.file_id;
        let canonical_identity = self
            .canonical_function_producer(name, callee_types, callee_values)
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue canonical comptime producer: {failure:?}"
                    )),
                    fn_span,
                )
            })?;
        let mut callee_env = ComptimeEnv::with_subst(callee_types, callee_values);
        callee_env.producer = Some(fn_body);
        callee_env.canonical_identity = Some(canonical_identity);
        // The callee body is code from the callee's file: a module-qualified
        // comptime call inside it (`let O = b.Mk(T)`) names an import of *that*
        // file, so the receiver must resolve against the callee's module
        // bindings, not the instantiation site's (RUE-511).
        callee_env.defining_file = Some(fn_file);
        let depth = self.comptime_type_call_depth() + 1;
        self.set_comptime_type_call_depth(depth);
        if self.comptime_type_call_depth() > MAX_SPECIALIZATION_ROUNDS {
            self.set_comptime_type_call_depth(self.comptime_type_call_depth() - 1);
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "specialization of '{}' exceeded the maximum nesting depth ({}); \
                         is a comptime-recursive function missing a compile-time-known \
                         base case, or a generic function recursively instantiating \
                         itself with new types?",
                        self.body_interner().resolve(&name),
                        MAX_SPECIALIZATION_ROUNDS
                    ),
                },
                fn_span,
            ));
        }
        let result = self.eval_const_expr(fn_body, &mut callee_env);
        self.set_comptime_type_call_depth(self.comptime_type_call_depth() - 1);
        // Record the human-readable instantiation spelling for a
        // constructor-produced anonymous type, so diagnostics print
        // `ArrayBuf(i64)` instead of `__anon_struct_4` (RUE-610).
        if let Ok(Some(ConstValue::Type(t))) = &result {
            self.record_ctor_type_display(name, *t, callee_types, callee_values);
        }
        result
    }

    /// Record `Ctor(args...)` as the display name for an anonymous type just
    /// produced by reducing `ctor`'s body (RUE-610; see
    /// the host's constructor-display registry). Named types keep their
    /// declared names;
    /// a partial substitution records nothing rather than a wrong spelling.
    fn record_ctor_type_display(
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
    ) -> (AHashMap<InstRef, Type>, ComptimePrecomputeAttribution) {
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
        );
        (discovered, attribution)
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
    ) {
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
                    );
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
                );
                if let Some(else_block) = else_block {
                    self.walk_comptime_type_locals(
                        else_block,
                        discovered,
                        eval_types,
                        eval_values,
                        runtime_bindings,
                        frame,
                        attribution,
                    );
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
                );
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
                    );
                }
            }
            _ => {}
        }
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
        env.producer = Some(init);
        env.canonical_identity = self.active_anonymous_producer().cloned();
        env.runtime_binding_names = Some(runtime_bindings);
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
    ) -> (AHashMap<InstRef, Type>, ComptimePrecomputeAttribution) {
        // The body RIR index walk already censused the whole arena for these
        // shapes. Zero occurrences anywhere proves the reachability scan below
        // — whose candidates are a subset of the arena's — would collect
        // nothing, so the common candidate-free body skips the scan outright.
        if self.body_inline_ctor_head_candidates() == 0 {
            return (
                AHashMap::new(),
                ComptimePrecomputeAttribution {
                    enabled: attribution_enabled,
                    ..ComptimePrecomputeAttribution::default()
                },
            );
        }
        // A head is the receiver of a `.NAME(..)` path whose receiver is
        // itself a call (`F(args).Ok(x)`, or module-qualified
        // `m.F(args).Ok(x)`, which RIR spells as a nested MethodCall), or a
        // struct literal's explicit `ctor_head`. Runtime shapes like
        // `foo(x).bar()` are collected too but fail the reduction cheaply
        // (the comptime engine rejects callees with runtime parameters).
        let (candidates, scan) =
            inline_ctor_head_candidates_with_work(self.body_rir_ref(), body, attribution_enabled);
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
        (reduced, attribution)
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

fn inline_ctor_head_candidates_with_work(
    rir: &rue_rir::Rir,
    body: InstRef,
    attribution_enabled: bool,
) -> (Vec<InstRef>, InlineCtorScanWork) {
    let mut pending = vec![body];
    let mut candidates = Vec::new();
    let mut work = InlineCtorScanWork::default();

    while let Some(current) = pending.pop() {
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
    (candidates, work)
}
