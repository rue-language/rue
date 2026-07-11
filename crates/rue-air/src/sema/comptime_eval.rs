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
//! [`Sema::eval_const_expr`] returns `CompileResult<Option<ConstValue>>`:
//!
//! - `Ok(Some(v))` — fully evaluated.
//! - `Ok(None)` — not compile-time evaluable (runtime variables, calls, ...).
//!   The `comptime` block handler reports this as E1200.
//! - `Err(e)` — the expression *is* constant but would panic at runtime
//!   (overflow at the operand type, division by zero). Inside a `comptime`
//!   block this is a compile error (spec 4.14:4); opportunistic callers
//!   ([`Sema::try_evaluate_const`] and friends) convert it to `None` and
//!   defer to the runtime check.

use std::collections::HashMap;
use std::sync::LazyLock;

use lasso::{Key, Spur};
use rue_error::{CompileError, CompileResult, ErrorKind, PreviewFeature};
use rue_rir::{InstData, InstRef, RepeatCount, RirPattern};
use rue_span::{FileId, Span};

use super::Sema;
use super::context::{AnalysisContext, ConstValue, LocalVar, ParamInfo};
use crate::specialize::MAX_SPECIALIZATION_ROUNDS;
use crate::types::{ArrayLen, StructField, Type, TypeKind};

/// Empty type substitution map for evaluation contexts without one.
static EMPTY_TYPE_SUBST: LazyLock<HashMap<Spur, Type>> = LazyLock::new(HashMap::new);
/// Empty value substitution map for evaluation contexts without one.
static EMPTY_VALUE_SUBST: LazyLock<HashMap<Spur, ConstValue>> = LazyLock::new(HashMap::new);
/// Empty module-member map for evaluation contexts without one.
static EMPTY_MODULE_MEMBERS: LazyLock<HashMap<InstRef, ConstValue>> = LazyLock::new(HashMap::new);

/// The environment a compile-time expression is evaluated in.
pub(crate) struct ComptimeEnv<'a> {
    /// Comptime type parameters in scope (e.g. `T` -> `i32`).
    type_subst: &'a HashMap<Spur, Type>,
    /// Comptime value parameters in scope (e.g. `N` -> `42`).
    value_subst: &'a HashMap<Spur, ConstValue>,
    /// Resolved types from HM inference for the function being analyzed.
    /// `None` when evaluating expressions outside a typed function context
    /// (comptime function bodies before specialization, const initializers).
    resolved_types: Option<&'a HashMap<InstRef, Type>>,
    /// Runtime locals in scope at the point being evaluated. A runtime local
    /// shadows same-named comptime parameters and file-level constants, so a
    /// reference to it makes the expression non-evaluable — without this,
    /// `let n = x; g(n)` inside a body with `comptime n` in scope would
    /// wrongly evaluate `n` to the parameter's value (spec 4.14:6).
    runtime_locals: Option<&'a HashMap<Spur, LocalVar>>,
    /// Runtime parameters in scope. They shadow same-named type values and
    /// constants just like locals; comptime parameters resolve through the
    /// substitution maps before this guard is consulted.
    runtime_params: Option<&'a [ParamInfo]>,
    /// `let` bindings introduced by blocks inside the comptime expression.
    locals: HashMap<Spur, ConstValue>,
    /// Values of module-member accesses (`m.CONST`) appearing in this
    /// expression, pre-resolved from the module's file (with privacy checks)
    /// before evaluation. The engine has no file/collector context of its own,
    /// so a `FieldGet` on a module is only evaluable as a sub-expression
    /// operand (`1 + m.CONST`) by looking its value up here (RUE-267). Keyed by
    /// the `FieldGet` instruction. Empty outside const-initializer evaluation.
    const_module_members: &'a HashMap<InstRef, ConstValue>,
    /// The file whose code is currently being reduced (RUE-511). A
    /// module-qualified comptime call written in a `-> type` constructor body
    /// (`let O = b.Mk(T)`) names an import (`b`) of *this* file's import graph,
    /// not of the file that triggered the instantiation — so resolving the
    /// receiver as a module binding must key `module_bindings` by this file, not
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
    fn substs_with_locals(&self) -> (HashMap<Spur, Type>, HashMap<Spur, ConstValue>) {
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
            type_subst: &EMPTY_TYPE_SUBST,
            value_subst: &EMPTY_VALUE_SUBST,
            resolved_types: None,
            runtime_locals: None,
            runtime_params: None,
            locals: HashMap::new(),
            const_module_members: &EMPTY_MODULE_MEMBERS,
            defining_file: None,
        }
    }

    /// An environment with comptime parameter substitutions but no resolved
    /// types (used when evaluating a comptime function body at a call site).
    pub(crate) fn with_subst(
        type_subst: &'a HashMap<Spur, Type>,
        value_subst: &'a HashMap<Spur, ConstValue>,
    ) -> Self {
        Self {
            type_subst,
            value_subst,
            resolved_types: None,
            runtime_locals: None,
            runtime_params: None,
            locals: HashMap::new(),
            const_module_members: &EMPTY_MODULE_MEMBERS,
            defining_file: None,
        }
    }

    /// The environment for expressions inside the function currently being
    /// analyzed: comptime parameters in scope plus HM-resolved types.
    pub(crate) fn for_analysis(ctx: &'a AnalysisContext) -> Self {
        Self {
            type_subst: &ctx.comptime_type_vars,
            value_subst: &ctx.comptime_value_vars,
            resolved_types: Some(ctx.resolved_types),
            runtime_locals: Some(&ctx.locals),
            runtime_params: Some(ctx.params),
            locals: HashMap::new(),
            const_module_members: &EMPTY_MODULE_MEMBERS,
            defining_file: Some(ctx.current_file_id),
        }
    }

    /// The environment for a file-level const initializer: no comptime
    /// parameters and no runtime locals, but a `resolved_types` map inferred
    /// from the declared const type (see `infer_const_init_types`). Threading
    /// these operand types lets `finish_arith` check arithmetic at the operand
    /// type — the same operand-type overflow (E1200, including intermediate
    /// results) the `comptime { }` block path gets from HM inference, instead
    /// of the raw-`i64` fallback that only range-checked the final value
    /// against the declared type (RUE-230).
    ///
    /// `defining_file` is the const's declaring file, so a type-constructor
    /// call in the initializer can collect the same-file callee's signature on
    /// demand (`const V = Vec(i32);` evaluated during struct-field resolution,
    /// before the main declaration sweep collected `Vec`; RUE-603), and a
    /// module-qualified comptime call nested in the initializer resolves its
    /// receiver against that file's imports (RUE-511).
    pub(crate) fn for_const_init(
        resolved_types: &'a HashMap<InstRef, Type>,
        const_module_members: &'a HashMap<InstRef, ConstValue>,
        defining_file: FileId,
    ) -> Self {
        Self {
            type_subst: &EMPTY_TYPE_SUBST,
            value_subst: &EMPTY_VALUE_SUBST,
            resolved_types: Some(resolved_types),
            runtime_locals: None,
            runtime_params: None,
            locals: HashMap::new(),
            const_module_members,
            defining_file: Some(defining_file),
        }
    }
}

/// Decide whether a compile-time-known scrutinee value matches a match arm's
/// pattern (RUE-262). Returns:
/// - `Some(true)` / `Some(false)` — the pattern definitely does / does not match;
/// - `None` — the match can't be decided at compile time here (an enum-variant
///   `Path` pattern, or a scrutinee whose kind the pattern can't compare
///   against), so the caller treats the whole `match` as non-evaluable.
fn const_pattern_matches(pattern: &RirPattern, scrut: ConstValue) -> Option<bool> {
    match pattern {
        RirPattern::Wildcard(_) => Some(true),
        RirPattern::Bool(b, _) => match scrut {
            ConstValue::Bool(sb) => Some(sb == *b),
            _ => None,
        },
        RirPattern::Int {
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
        RirPattern::Path { .. } => None,
    }
}

/// Check whether `value` is representable in integer type `ty`.
pub(crate) fn const_int_fits(value: i128, ty: Type) -> bool {
    match (ty.int_min(), ty.int_max()) {
        (Some(min), Some(max)) => value >= min && value <= max,
        _ => false,
    }
}

/// Truncate `value` to the width of integer type `ty` (two's complement
/// wrapping, sign-extended for signed types). Mirrors what the hardware does
/// for operations defined to truncate (shifts, bitwise NOT on unsigned).
fn truncate_to_type(value: i128, ty: Type) -> i128 {
    let width = ty
        .int_bit_width()
        .expect("truncate_to_type called with non-integer type");
    let mask = (1i128 << width) - 1;
    let mut r = value & mask;
    if ty.is_signed() && (r & (1i128 << (width - 1))) != 0 {
        r -= 1i128 << width;
    }
    r
}

/// Build the E1200 error for a constant operation that would panic at runtime.
fn comptime_panic_err(reason: String, span: Span) -> CompileError {
    CompileError::new(ErrorKind::ComptimeEvaluationFailed { reason }, span)
}

impl Sema<'_> {
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
    /// [`try_evaluate_const`]: Sema::try_evaluate_const
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
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> Option<ConstValue> {
        let mut env = ComptimeEnv::with_subst(type_subst, value_subst);
        // A module-qualified comptime call in the evaluated expression resolves
        // its receiver against the expression's own file's imports (RUE-511).
        env.defining_file = Some(self.rir.get(inst_ref).span.file_id);
        self.eval_const_expr(inst_ref, &mut env).ok().flatten()
    }

    /// Like [`try_evaluate_const_with_subst`], but *propagates* a hard
    /// diagnostic raised while reducing the body instead of swallowing it.
    ///
    /// Reducing a `-> type` constructor body may legitimately be non-evaluable
    /// (`Ok(None)` — defer to a runtime call) or raise a genuine compile error
    /// (`Err` — e.g. an arithmetic overflow, a privacy violation, or exceeding
    /// the comptime-recursion depth limit for a non-terminating type
    /// constructor, RUE-261). The type-constructor reduction site uses this so
    /// the latter surfaces as its real diagnostic (E1200) rather than being
    /// swallowed and mis-reported as a downstream link error.
    ///
    /// [`try_evaluate_const_with_subst`]: Sema::try_evaluate_const_with_subst
    pub(crate) fn eval_type_constructor_body(
        &mut self,
        inst_ref: InstRef,
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> CompileResult<Option<ConstValue>> {
        let mut env = ComptimeEnv::with_subst(type_subst, value_subst);
        // The body is code from the constructor's file, so a module-qualified
        // comptime call inside it (`let O = b.Mk(T)`) resolves its receiver
        // against that file's imports (RUE-511).
        env.defining_file = Some(self.rir.get(inst_ref).span.file_id);
        self.eval_const_expr(inst_ref, &mut env)
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
    /// [`try_get_const_index`]: Sema::try_get_const_index
    /// [`try_evaluate_const_in_fn`]: Sema::try_evaluate_const_in_fn
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
        if let InstData::VarRef { name } = &self.rir.get(inst_ref).data {
            // A runtime local of the same name shadows the parameter.
            !ctx.locals.contains_key(name)
                && ctx.params.iter().any(|p| p.name == *name && p.is_comptime)
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

    /// Finish an arithmetic operation: range-check `value` against the
    /// expression's type.
    ///
    /// - Typed: out-of-range results are a hard error (the operation would
    ///   panic at runtime, spec 8.1 / 4.14:4).
    /// - Untyped fallback: results outside the `i64` range make the
    ///   expression non-evaluable (legacy checked-i64 semantics).
    fn finish_arith(
        &self,
        value: Option<i128>,
        ty: Option<Type>,
        op: &str,
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
        match ty {
            Some(ty) => match value {
                Some(v) if const_int_fits(v, ty) => Ok(Some(ConstValue::Integer(v))),
                _ => {
                    let ty_name = ty.safe_name_with_pool(Some(&self.type_pool));
                    let detail = match value {
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
            None => match value {
                Some(v) if v >= i128::from(i64::MIN) && v <= i128::from(i64::MAX) => {
                    Ok(Some(ConstValue::Integer(v)))
                }
                _ => Ok(None),
            },
        }
    }

    /// Resolve a bare name used as a type value through the canonical type
    /// resolver. Preview types such as `str` are registered lazily, so looking
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
        let inst = self.rir.get(inst_ref);
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
                                ty: ty.safe_name_with_pool(Some(&self.type_pool)),
                            },
                            span,
                        ));
                    }
                }
                Ok(Some(ConstValue::Integer(v)))
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
                            ErrorKind::CannotNegate(ty.safe_name_with_pool(Some(&self.type_pool))),
                            span,
                        ));
                    }
                }
                // Negated literals bypass the literal range check so that
                // `-128` works for i8 (the magnitude alone exceeds i8::MAX).
                let operand_value = if let InstData::IntConst(mag) = &self.rir.get(*operand).data {
                    Some(ConstValue::Integer(*mag as i128))
                } else {
                    self.eval_const_expr(*operand, env)?
                };
                match operand_value {
                    Some(ConstValue::Integer(n)) => self.finish_arith(Some(-n), ty, "-", span),
                    // Can't negate a boolean, type, or unit
                    _ => Ok(None),
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
                self.finish_arith(l.checked_add(r), ty, "+", span)
            }
            InstData::Sub { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.const_expr_type(env, inst_ref);
                self.finish_arith(l.checked_sub(r), ty, "-", span)
            }
            InstData::Mul { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.const_expr_type(env, inst_ref);
                self.finish_arith(l.checked_mul(r), ty, "*", span)
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
                // Signed MIN / -1 (and MIN % -1) overflow at runtime, spec 8.1:3.
                if r == -1 {
                    match ty {
                        Some(t) if t.is_signed() && Some(l) == t.int_min() => {
                            return self.finish_arith(None, ty, op, span);
                        }
                        None if l == i128::from(i64::MIN) => return Ok(None),
                        _ => {}
                    }
                }
                self.finish_arith(Some(if is_div { l / r } else { l % r }), ty, op, span)
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
                        let width = ty
                            .int_bit_width()
                            .expect("const_expr_type returned non-integer");
                        // Two's-complement AND masks negative amounts the same
                        // way the hardware masks the count register.
                        let amt = (r & i128::from(width - 1)) as u32;
                        let v = if is_shl {
                            // Wrapping shift + truncation: the low `width`
                            // bits are exact, which is all that survives.
                            truncate_to_type(l.wrapping_shl(amt), ty)
                        } else {
                            // Value semantics make this arithmetic for signed
                            // (negative l) and logical for unsigned (l >= 0).
                            l >> amt
                        };
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
                    Some(ty) => truncate_to_type(!n, ty),
                    None => !n,
                };
                Ok(Some(ConstValue::Integer(v)))
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => self.eval_const_expr(*expr, env),

            // Block: evaluate `let` statements into the environment, then the
            // tail expression. Loops, assignments and calls are not supported
            // and make the block non-evaluable.
            InstData::Block { extra_start, len } => {
                if *len == 0 {
                    return Ok(Some(ConstValue::Unit));
                }
                let stmt_refs: Vec<InstRef> = self
                    .rir
                    .get_extra(*extra_start, *len)
                    .iter()
                    .map(|&raw| InstRef::from_raw(raw))
                    .collect();
                // Bindings are scoped to the block.
                let saved_locals = env.locals.clone();
                let mut result = Some(ConstValue::Unit);
                for (i, &stmt_ref) in stmt_refs.iter().enumerate() {
                    let is_tail = i + 1 == stmt_refs.len();
                    let value =
                        if let InstData::Alloc { name, init, .. } = &self.rir.get(stmt_ref).data {
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
            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => {
                let (scrutinee, arms_start, arms_len) = (*scrutinee, *arms_start, *arms_len);
                let Some(scrut) = self.eval_const_expr(scrutinee, env)? else {
                    return Ok(None);
                };
                let arms = self.rir.get_match_arms(arms_start, arms_len);
                for (pattern, body) in arms {
                    match const_pattern_matches(&pattern, scrut) {
                        Some(true) => return self.eval_const_expr(body, env),
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
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            } => {
                let field_decls = self.rir.get_field_decls(*fields_start, *fields_len);

                // Comptime `let` locals in scope participate in field-type
                // resolution (`let Inner = Mk(T); struct { x: Inner }`,
                // RUE-575), alongside the enclosing parameters.
                let (local_type_subst, local_value_subst) = env.substs_with_locals();

                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.interner.resolve(&name_sym).to_string();
                    // Field types resolve through both the type substitution
                    // (`comptime T: type`) and the value substitution
                    // (`comptime N: i32`, so an `[i32; N]` field gets a concrete
                    // length at each specialization; RUE-16).
                    let Some(field_ty) = self
                        .resolve_type_for_comptime_with_subst_and_values_at_span(
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
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                let (struct_ty, _is_new) = self.find_or_create_anon_struct(
                    &struct_fields,
                    &method_sigs,
                    &local_value_subst,
                );

                // Register methods if present and not yet registered for this
                // struct (it may have been created earlier without methods).
                if *methods_len > 0 {
                    // A method that declares its own `comptime T: type`
                    // parameter would need to be monomorphized per call over
                    // that parameter — a generics feature not yet supported
                    // (RUE-284). Merely *defining* such a method used to make
                    // the enclosing `-> type` constructor non-evaluable
                    // (registration returned None → the reduction bailed with
                    // Ok(None)), surfacing as a misleading E1200 mis-located at
                    // the `let` that instantiates the constructor. Detect it
                    // here and raise a clear diagnostic pointing *at the
                    // method* instead, without poisoning the rest of the
                    // reduction.
                    if let Some((method_span, method_name)) =
                        self.find_method_own_comptime_type_param(*methods_start, *methods_len)
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

                    let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let first_method_ref = method_refs[0];
                    let first_method_inst = self.rir.get(first_method_ref);
                    if let InstData::FnDecl {
                        name: method_name, ..
                    } = &first_method_inst.data
                    {
                        let needs_registration =
                            !self.methods.contains_key(&(struct_id, *method_name));

                        if needs_registration
                            && self
                                .register_anon_struct_methods_for_comptime_with_subst(
                                    struct_id,
                                    struct_ty,
                                    *methods_start,
                                    *methods_len,
                                    span,
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
                            self.anon_struct_type_subst
                                .insert(struct_id, local_type_subst.clone());
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
                variants_start,
                variants_len,
                payloads_start,
                payloads_len,
            } => {
                let variant_syms: Vec<lasso::Spur> = self
                    .rir
                    .get_symbols(*variants_start, *variants_len)
                    .to_vec();
                let payload_words: Vec<u32> =
                    self.rir.get_extra(*payloads_start, *payloads_len).to_vec();

                // Decode the self-describing payload region into per-variant
                // type-symbol lists (parallel to `variant_syms`), then resolve
                // each payload type through the substitutions.
                // Comptime `let` locals participate in payload-type
                // resolution, matching the struct arm (RUE-575).
                let (enum_type_subst, enum_value_subst) = env.substs_with_locals();

                let mut variant_names: Vec<String> = Vec::with_capacity(variant_syms.len());
                let mut variant_payloads: Vec<Vec<Type>> = Vec::with_capacity(variant_syms.len());
                let mut pi = 0usize;
                for &vsym in &variant_syms {
                    variant_names.push(self.interner.resolve(&vsym).to_string());
                    // A variant carries a payload only when the payload region
                    // is present (`payloads_len > 0`) and describes arity `k`.
                    let k = if payload_words.is_empty() {
                        0
                    } else {
                        let k = payload_words[pi] as usize;
                        pi += 1;
                        k
                    };
                    let mut tys: Vec<Type> = Vec::with_capacity(k);
                    for _ in 0..k {
                        let ty_sym = lasso::Spur::try_from_usize(payload_words[pi] as usize)
                            .expect("valid payload type symbol");
                        pi += 1;
                        let Some(ty) = self
                            .resolve_type_for_comptime_with_subst_and_values_at_span(
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

                let enum_ty = self.find_or_create_anon_enum(&variant_names, &variant_payloads);
                Ok(Some(ConstValue::Type(enum_ty)))
            }

            // TypeConst: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                let type_name = *type_name;
                // Type parameters in scope substitute first.
                if let Some(&ty) = env.type_subst.get(&type_name) {
                    return Ok(Some(ConstValue::Type(ty)));
                }
                // A named type (primitive / struct / enum) resolves directly.
                if let Some(ty) = self.resolve_named_type_value(type_name, span)? {
                    return Ok(Some(ConstValue::Type(ty)));
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
                    .resolve_type_for_comptime_with_subst_and_values_at_span(
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
                        let name = self.interner.resolve(&sym).to_string();
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
            InstData::VarRef { name } => {
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
                if let Some(params) = env.runtime_params {
                    if params.iter().any(|param| param.name == *name) {
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
                //    (module bindings live in `Sema::module_bindings`).
                //    Privacy applies here too (E0460, RUE-183): the table is
                //    global, so a const initializer in one directory could
                //    otherwise read a private constant from another. The
                //    VarRef's own span locates the referencing file;
                //    speculative callers (`try_evaluate_const*`) swallow the
                //    error and defer to runtime analysis, which re-checks.
                if let Some(info) = self.constants_by_file_name.get(&(span.file_id, *name)) {
                    self.check_unqualified_visibility(
                        "constant",
                        self.interner.resolve(name),
                        info.span.file_id,
                        info.is_pub,
                        span,
                    )?;
                    return Ok(Some(info.value));
                }
                // 7. Type names used as values (e.g. `Point` in
                //    `fn make_type() -> type { Point }`)
                Ok(self
                    .resolve_named_type_value(*name, span)?
                    .map(ConstValue::Type))
            }

            // Call to a `-> type` function: reduce it to the resulting type
            // value when the callee is a type constructor and every argument
            // is compile-time known. This makes comptime type-function calls
            // compose in ANY position — a delegating return body
            // (`fn Alias() -> type { Point() }`), a nested argument
            // (`WrapA(WrapA(i32))`), and chains thereof (RUE-251).
            InstData::Call {
                name,
                args_start,
                args_len,
            } => {
                let (name, args_start, args_len) = (*name, *args_start, *args_len);
                self.eval_comptime_type_call(name, args_start, args_len, env)
                    .map_err(|e| Self::label_ctor_instantiation_site(e, span))
            }

            // Module-member access (`m.CONST`) as an operand of a larger const
            // initializer. The value was pre-resolved from the module's file
            // (with privacy checks) before evaluation — see the
            // `const_module_members` field — since the engine has no file or
            // constant-collector context to resolve it here. A member absent
            // from the map (a non-module base, or a re-export used as a value)
            // is not evaluable, so the caller reports it (RUE-267).
            InstData::FieldGet { .. } => Ok(env.const_module_members.get(&inst_ref).copied()),

            // Type intrinsic in comptime position. `@require_droppable(T)` is the
            // owning-container well-formedness gate (RUE-388/RUE-646): std's
            // `ArrayBuf(T)` calls it in its `-> type` constructor body so that
            // instantiating the container with an element type it cannot yet
            // correctly own — one that is `linear` — is rejected at instantiation
            // time (E0499). Droppable-but-non-linear elements are accepted: the
            // container runs each live element's drop glue before freeing its
            // buffer (RUE-646). It reduces to unit so the surrounding block
            // body still yields the `struct { .. }` tail. `@size_of`/`@align_of`
            // are not comptime-foldable here and stay non-evaluable.
            InstData::TypeIntrinsic { name, type_arg } => {
                let (name, type_arg) = (*name, *type_arg);
                let gate = self.interner.resolve(&name);
                // Both well-formedness gates reduce to unit at comptime:
                // `@require_droppable` (instantiation-time, rejects `linear`) and
                // `@require_trivially_droppable` (read-time, rejects drop glue —
                // RUE-651). Any other type intrinsic (`@size_of`/`@align_of`) is
                // not comptime-foldable here.
                let is_droppable_gate = gate == "require_droppable";
                let is_trivial_gate = gate == "require_trivially_droppable";
                if !is_droppable_gate && !is_trivial_gate {
                    return Ok(None);
                }
                // Resolve the element type through the enclosing comptime
                // substitutions (`T -> Inner` for `ArrayBuf(Inner)`); a
                // still-unresolved type parameter makes the gate non-evaluable
                // (it will be re-checked at a concrete instantiation).
                let Some(elem_ty) = self.resolve_type_for_comptime_with_subst_and_values_at_span(
                    type_arg,
                    env.type_subst,
                    env.value_subst,
                    span,
                ) else {
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
                args_start,
                args_len,
            } => {
                let (receiver, method, args_start, args_len) =
                    (*receiver, *method, *args_start, *args_len);
                self.eval_module_qualified_comptime_call(
                    receiver, method, args_start, args_len, span, env,
                )
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
    /// The membership check (`fn_info.file_id == module_file_id`) closes the
    /// RUE-564 cross-module hole: functions live in a flat global table keyed by
    /// name, so a same-named constructor in a different file must not satisfy
    /// `b.Mk`. Visibility is enforced the same way the qualified type-annotation
    /// path enforces it (E0460/E0706 surface as the reduction's E1200 here since
    /// the comptime engine cannot itself emit a diagnostic mid-reduction).
    fn eval_module_qualified_comptime_call(
        &mut self,
        receiver: InstRef,
        method: Spur,
        args_start: u32,
        args_len: u32,
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
            match self.rir.get(cursor).data {
                InstData::VarRef { name } => break name,
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
            let Some(binding) = self.module_bindings.get(&(file_id, recv_name)).cloned() else {
                return Ok(None);
            };
            let Some(module_id) = binding.ty.as_module() else {
                return Ok(None);
            };
            let module_def = self.module_registry.get_def(module_id);
            let Some(module_file_id) = self.canonical_file_id(&module_def.file_path) else {
                return Ok(None);
            };
            module_file_id
        } else {
            let mut segments: Vec<&str> = vec![self.interner.resolve(&recv_name)];
            segments.extend(chain_rev.iter().rev().map(|s| self.interner.resolve(s)));
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
        // Ensure the member's signature is collected, then require membership:
        // the resolved function must actually be declared in the module's file.
        self.ensure_free_function_signature(method, Some(module_file_id))?;
        let function_key = self
            .resolve_function_name_local(method, module_file_id)
            .unwrap_or(method);
        let Some(fn_info) = self
            .functions
            .get(&function_key)
            .copied()
            .filter(|info| info.file_id == module_file_id)
        else {
            return Ok(None);
        };
        // Visibility: a non-`pub` member accessed through a module object is not
        // usable from another directory (spec 10.3:7).
        let member_name = self.interner.resolve(&method).to_string();
        self.check_unqualified_visibility(
            "function",
            &member_name,
            fn_info.file_id,
            fn_info.is_pub,
            span,
        )?;
        // Reduce through the shared path; arguments are evaluated in the current
        // environment so `T` (an enclosing comptime parameter) still resolves.
        self.eval_comptime_type_call(function_key, args_start, args_len, env)
            .map_err(|e| Self::label_ctor_instantiation_site(e, span))
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
    pub(crate) fn check_require_droppable(&self, ty: Type, span: Span) -> CompileResult<()> {
        // Linear element types stay rejected (E0499): a linear value must be
        // consumed exactly once and a container cannot run the consuming
        // discharge on drop — that is the deferred RUE-649 work.
        if self.type_carries_linear(ty) {
            return Err(CompileError::new(
                ErrorKind::ContainerElementIsLinear {
                    ty: self.format_type_name(ty),
                },
                span,
            ));
        }
        // Droppable-but-non-linear element types are now ACCEPTED (RUE-646,
        // Steve's 2026-07-11 ruling): the container runs each live element's
        // drop glue before freeing its buffer, exactly as Rust's `Vec<T>`
        // does. The old E0498 rejection is gone; `ArrayBuf(ArrayBuf(i64))`,
        // `ArrayBuf(StrBuf)`, and lists of `drop fn` structs are legal.
        Ok(())
    }

    /// The `@require_trivially_droppable(T)` gate for by-copy element *reads*
    /// (RUE-651). `ArrayBuf(T)`'s `get`/`get_or` return the element by copying it
    /// out with `@ptr_read` while leaving the slot live. For a `T` with drop glue
    /// (a destructor, or a field/payload/element that has one) that copy aliases
    /// the element's owned resources: both the copy and the still-live slot run
    /// drop glue at scope exit — a double-free. This gate rejects those reads at
    /// their call site (E0711); the element must be *moved* out with `pop`/`pop_or`
    /// instead. It is deliberately placed in the `get`/`get_or` method bodies (not
    /// the constructor), so demand-driven analysis (ADR-0045) fires it only when a
    /// program actually calls a by-copy read — storing, pushing, popping, and
    /// dropping a drop-glue element stay legal (RUE-646). Mirrors Swift's rule
    /// that a non-copyable element cannot use a by-value `get` subscript.
    ///
    /// A `linear` `T` never reaches here: `@require_droppable` already rejects it
    /// at instantiation, so `ArrayBuf(linear)` cannot be constructed to be read.
    pub(crate) fn check_trivially_droppable(&self, ty: Type, span: Span) -> CompileResult<()> {
        if self.type_has_drop_glue(ty) {
            return Err(CompileError::new(
                ErrorKind::ContainerElementNotTriviallyDroppable {
                    ty: self.format_type_name(ty),
                },
                span,
            ));
        }
        Ok(())
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
    /// [`eval_const_expr`]: Sema::eval_const_expr
    fn eval_comptime_type_call(
        &mut self,
        name: Spur,
        args_start: u32,
        args_len: u32,
        env: &mut ComptimeEnv,
    ) -> CompileResult<Option<ConstValue>> {
        let (name_key, fn_info) = if let Some(info) = self.functions.get(&name).copied() {
            (name, info)
        } else {
            // The callee may simply not be collected yet: const initializers
            // (`const V = Vec(i32);`) and struct-field / enum-payload types
            // evaluate before the main declaration sweep reaches the callee's
            // `FnDecl` (RUE-603). Collect the evaluating expression's own
            // file's declaration on demand; a genuinely unknown name stays
            // non-evaluable.
            let Some(file_id) = env.defining_file else {
                return Ok(None);
            };
            self.ensure_free_function_signature(name, Some(file_id))?;
            let Some((key, info)) = self
                .resolve_function_name_local(name, file_id)
                .and_then(|key| self.functions.get(&key).copied().map(|info| (key, info)))
            else {
                return Ok(None);
            };
            (key, info)
        };
        let is_type_fn = fn_info.return_type == Type::COMPTIME_TYPE;
        let params = fn_info.params;
        let param_names = self.param_arena.names(params).to_vec();
        let param_modes = self.param_arena.modes(params).to_vec();
        let param_comptime = self.param_arena.comptime(params).to_vec();
        let param_comptime_type = self.comptime_type_param_flags(&fn_info);
        let args = self.rir.get_call_args(args_start, args_len).to_vec();
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
        let mut callee_types: HashMap<Spur, Type> = HashMap::new();
        let mut callee_values: HashMap<Spur, ConstValue> = HashMap::new();
        for (i, arg) in args.iter().enumerate() {
            let Some(v) = self.eval_const_expr(arg.value, env)? else {
                return Ok(None);
            };
            match (param_comptime_type[i], v) {
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
        self.reduce_type_ctor_body(name_key, &callee_types, &callee_values)
    }

    /// Reduce a comptime-evaluable function's body to a [`ConstValue`] under
    /// the given comptime parameter substitutions — a type value for a
    /// `-> type` constructor, or an integer/bool for a value-returning
    /// comptime function (RUE-163 facet 1). Shared by
    /// [`eval_comptime_type_call`] (value/const-expr positions, args evaluated
    /// via [`eval_const_expr`]) and [`resolve_type_function_call`] (a
    /// type-function call written directly in a signature/annotation position,
    /// args resolved as types; RUE-241) so both produce the identical
    /// monomorphized result.
    ///
    /// Guards the reduction against unbounded self-recursion. A `-> type`
    /// function reduces eagerly on the host stack, so a constructor with no
    /// compile-time-known base case (`fn Bad() -> type { Bad() }`,
    /// `fn Wrap(comptime n: i32) -> type { Wrap(n + 1) }`) would overflow that
    /// stack and abort the compiler (SIGABRT) with no diagnostic. Cap the
    /// depth at the same bound the specialization pass uses for value
    /// recursion, emitting the identical E1200 (RUE-261, spec 4.14:18).
    ///
    /// [`eval_comptime_type_call`]: Sema::eval_comptime_type_call
    /// [`eval_const_expr`]: Sema::eval_const_expr
    /// [`resolve_type_function_call`]: Sema::resolve_type_function_call
    pub(crate) fn reduce_type_ctor_body(
        &mut self,
        name: Spur,
        callee_types: &HashMap<Spur, Type>,
        callee_values: &HashMap<Spur, ConstValue>,
    ) -> CompileResult<Option<ConstValue>> {
        let Some(fn_info) = self.functions.get(&name) else {
            return Ok(None);
        };
        let fn_body = fn_info.body;
        let fn_span = fn_info.span;
        let fn_file = fn_info.file_id;
        let mut callee_env = ComptimeEnv::with_subst(callee_types, callee_values);
        // The callee body is code from the callee's file: a module-qualified
        // comptime call inside it (`let O = b.Mk(T)`) names an import of *that*
        // file, so the receiver must resolve against the callee's module
        // bindings, not the instantiation site's (RUE-511).
        callee_env.defining_file = Some(fn_file);
        self.comptime_type_call_depth += 1;
        if self.comptime_type_call_depth > MAX_SPECIALIZATION_ROUNDS {
            self.comptime_type_call_depth -= 1;
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "specialization of '{}' exceeded the maximum nesting depth ({}); \
                         is a comptime-recursive function missing a compile-time-known \
                         base case, or a generic function recursively instantiating \
                         itself with new types?",
                        self.interner.resolve(&name),
                        MAX_SPECIALIZATION_ROUNDS
                    ),
                },
                fn_span,
            ));
        }
        let result = self.eval_const_expr(fn_body, &mut callee_env);
        self.comptime_type_call_depth -= 1;
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
    /// `Sema::ctor_type_displays`). Named types keep their declared names;
    /// a partial substitution records nothing rather than a wrong spelling.
    fn record_ctor_type_display(
        &mut self,
        ctor: Spur,
        ty: Type,
        callee_types: &HashMap<Spur, Type>,
        callee_values: &HashMap<Spur, ConstValue>,
    ) {
        let is_anon = match ty.kind() {
            TypeKind::Struct(id) => self
                .type_pool
                .struct_def(id)
                .name
                .starts_with("__anon_struct_"),
            TypeKind::Enum(id) => self.type_pool.enum_def(id).name.starts_with("enum {"),
            _ => false,
        };
        if !is_anon || self.ctor_type_displays.contains_key(&ty) {
            return;
        }
        let Some(fn_info) = self.functions.get(&ctor) else {
            return;
        };
        let param_names = self.param_arena.names(fn_info.params).to_vec();
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
            self.interner.resolve(&source_name),
            args.join(", ")
        );
        self.ctor_type_displays.insert(ty, display);
    }

    /// Pre-resolve `let`-bound compile-time type aliases in a function body,
    /// before HM inference runs (RUE-170, RUE-164).
    ///
    /// A binding like `let P = F();` (where `F` returns `type`) only gets a
    /// concrete type during sema's analysis pass — but inference runs first,
    /// so every use of `P` as a type name (`P { ... }`, `let p: P = ...`,
    /// methods on a `P`-typed receiver) used to fall through to `<error>` or
    /// an unconstrained variable. This walk finds such bindings and evaluates
    /// their initializers eagerly so the constraint generator can route them
    /// through the same paths as named structs.
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
        type_subst: Option<&HashMap<Spur, Type>>,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
    ) -> HashMap<InstRef, Type> {
        let mut discovered: HashMap<InstRef, Type> = HashMap::new();
        let mut eval_types: HashMap<Spur, Type> = type_subst.cloned().unwrap_or_default();
        let eval_values: HashMap<Spur, ConstValue> = value_subst.cloned().unwrap_or_default();
        let mut root_frame = Vec::new();
        self.walk_comptime_type_locals(
            body,
            &mut discovered,
            &mut eval_types,
            &eval_values,
            &mut root_frame,
        );
        discovered
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
        discovered: &mut HashMap<InstRef, Type>,
        eval_types: &mut HashMap<Spur, Type>,
        eval_values: &HashMap<Spur, ConstValue>,
        frame: &mut Vec<(Spur, Option<Type>)>,
    ) {
        match &self.rir.get(inst_ref).data {
            InstData::Block { extra_start, len } => {
                let stmts: Vec<InstRef> = self
                    .rir
                    .get_extra(*extra_start, *len)
                    .iter()
                    .map(|&raw| InstRef::from_raw(raw))
                    .collect();
                let mut inner_frame = Vec::new();
                for stmt in stmts {
                    self.walk_comptime_type_locals(
                        stmt,
                        discovered,
                        eval_types,
                        eval_values,
                        &mut inner_frame,
                    );
                }
                for (name, old) in inner_frame.into_iter().rev() {
                    match old {
                        Some(ty) => eval_types.insert(name, ty),
                        None => eval_types.remove(&name),
                    };
                }
            }
            InstData::Alloc { name, init, .. } => {
                let (name, init) = (*name, *init);
                if let Some(name) = name {
                    if let Some(ty) = self.try_eval_type_alias_init(init, eval_types, eval_values) {
                        discovered.insert(inst_ref, ty);
                        frame.push((name, eval_types.insert(name, ty)));
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
                    frame,
                );
                if let Some(else_block) = else_block {
                    self.walk_comptime_type_locals(
                        else_block,
                        discovered,
                        eval_types,
                        eval_values,
                        frame,
                    );
                }
            }
            InstData::Loop { body, .. } | InstData::InfiniteLoop { body, .. } => {
                let body = *body;
                self.walk_comptime_type_locals(body, discovered, eval_types, eval_values, frame);
            }
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let bodies: Vec<InstRef> = self
                    .rir
                    .get_match_arms(*arms_start, *arms_len)
                    .iter()
                    .map(|(_, body)| *body)
                    .collect();
                for body in bodies {
                    self.walk_comptime_type_locals(
                        body,
                        discovered,
                        eval_types,
                        eval_values,
                        frame,
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
        eval_types: &HashMap<Spur, Type>,
        eval_values: &HashMap<Spur, ConstValue>,
    ) -> Option<Type> {
        // `eval_const_expr`'s `Call` arm reduces type-function calls
        // compositionally (including nested/delegating ones), so a single
        // evaluation of the initializer handles `let P = Q;`,
        // `let P = struct { .. };`, and `let P = Pair(i32);` alike (RUE-251).
        match self.try_evaluate_const_with_subst(init, eval_types, eval_values) {
            Some(ConstValue::Type(t)) => Some(t),
            _ => None,
        }
    }

    /// Pre-reduce inline type-constructor heads (`F(args).Variant(..)`,
    /// `F(args) { .. }`; RUE-596, preview `inline_type_ctor_paths`) to their
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
    /// The scan covers the whole RIR, not just the current body: heads
    /// belonging to other functions evaluate under this function's
    /// substitutions, but their keys are `InstRef`s this body's constraint
    /// generation never visits, and reduction is idempotent (specializations
    /// are cached, anonymous types dedup structurally), so stray entries are
    /// inert. `comptime_local_types` carries the body's `let`-bound type
    /// aliases so a head like `Result(T, i32)` with `let T = i64;` reduces.
    ///
    /// Gated on the preview feature so non-preview compiles pay nothing;
    /// when `inline_type_ctor_paths` stabilizes (RUE-598), remove the gate
    /// and cache the candidate scan if it shows up in `--time-passes`.
    ///
    /// [`precompute_comptime_type_locals`]: Sema::precompute_comptime_type_locals
    pub(crate) fn precompute_inline_ctor_head_types(
        &mut self,
        type_subst: Option<&HashMap<Spur, Type>>,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
        comptime_local_types: &HashMap<Spur, Type>,
    ) -> HashMap<InstRef, Type> {
        if !self
            .preview_features
            .contains(&PreviewFeature::InlineTypeCtorPath)
        {
            return HashMap::new();
        }
        // A head is the receiver of a `.NAME(..)` path whose receiver is
        // itself a call (`F(args).Ok(x)`, or module-qualified
        // `m.F(args).Ok(x)`, which RIR spells as a nested MethodCall), or a
        // struct literal's explicit `ctor_head`. Runtime shapes like
        // `foo(x).bar()` are collected too but fail the reduction cheaply
        // (the comptime engine rejects callees with runtime parameters).
        let candidates: Vec<InstRef> = self
            .rir
            .iter()
            .filter_map(|(_, inst)| match inst.data {
                InstData::MethodCall { receiver, .. } => matches!(
                    self.rir.get(receiver).data,
                    InstData::Call { .. } | InstData::MethodCall { .. }
                )
                .then_some(receiver),
                InstData::StructInit {
                    ctor_head: Some(head),
                    ..
                } => Some(head),
                _ => None,
            })
            .collect();
        let mut eval_types: HashMap<Spur, Type> = type_subst.cloned().unwrap_or_default();
        eval_types.extend(comptime_local_types);
        let eval_values: HashMap<Spur, ConstValue> = value_subst.cloned().unwrap_or_default();
        let mut reduced = HashMap::new();
        for head in candidates {
            if let Some(ConstValue::Type(ty)) =
                self.try_evaluate_const_with_subst(head, &eval_types, &eval_values)
                && (ty.is_enum() || ty.as_struct().is_some())
            {
                reduced.insert(head, ty);
            }
        }
        reduced
    }
}
